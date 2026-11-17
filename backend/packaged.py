"""Run the portable backend and built frontend from one executable.

--role all (default) serves both from this process. --role backend serves only the API and
--role frontend only the built frontend, proxying /api to --backend-url, so the two halves can
run on different machines.
"""

from __future__ import annotations

import argparse
import asyncio
import json
import os
import signal
import socket
import sys
from collections.abc import AsyncIterator, Sequence
from contextlib import asynccontextmanager, contextmanager
from pathlib import Path
from typing import Iterator
from urllib.parse import unquote, urlsplit

import h11
import httpx
import uvicorn
from fastapi import FastAPI, Request
from starlette.datastructures import Headers
from starlette.requests import ClientDisconnect
from starlette.responses import JSONResponse, Response, StreamingResponse
from starlette.staticfiles import StaticFiles
from starlette.types import Receive, Scope, Send

HOP_BY_HOP_HEADERS = {
    "connection", "keep-alive", "proxy-authenticate", "proxy-authorization",
    "te", "trailer", "transfer-encoding", "upgrade",
}
ROLES = ("all", "frontend", "backend")
BACKEND_URL_SETTING = "backend_url"
# Uploads and video streams may stay open for as long as they need; only an unreachable
# backend fails fast.
PROXY_TIMEOUT = httpx.Timeout(None, connect=10.0)


class RunError(RuntimeError):
    """Raised when the portable application cannot start or keep running."""


def runtime_directory() -> Path:
    # A one-file executable extracts its Python modules to a temporary directory.
    # Public files belong beside server.exe, outside that directory.
    if getattr(sys, "frozen", False):
        return Path(sys.executable).resolve().parent
    return Path(__file__).resolve().parents[1] / "build" / "dist" / "Debug"


def load_environment(root: Path) -> None:
    env_file = root / ".env"
    if env_file.is_file():
        for raw_line in env_file.read_text(encoding="utf-8-sig").splitlines():
            line = raw_line.strip()
            if not line or line.startswith("#") or "=" not in line:
                continue
            key, value = line.split("=", 1)
            if key.strip():
                os.environ.setdefault(key.strip(), value.strip().strip('"').strip("'"))

    configured = os.environ.get("COUNTER_DB_PATH")
    if configured and configured.strip():
        database = Path(os.path.expandvars(configured)).expanduser()
        if not database.is_absolute():
            database = root / database
        os.environ["COUNTER_DB_PATH"] = str(database.resolve())
    elif configured is not None:
        os.environ.pop("COUNTER_DB_PATH")


def runtime_host(root: Path) -> str:
    profile_path = root / "runtime.json"
    if not profile_path.exists():
        return "0.0.0.0"
    try:
        profile = json.loads(profile_path.read_text(encoding="utf-8-sig"))
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        raise RunError(f"Cannot read runtime profile at {profile_path}: {error}") from error
    if not isinstance(profile, dict) or profile.get("host") not in ("127.0.0.1", "0.0.0.0"):
        raise RunError(
            f"Invalid runtime profile at {profile_path}: host must be 127.0.0.1 or 0.0.0.0"
        )
    return profile["host"]


def port_number(value: str | int) -> int:
    try:
        port = int(value)
    except (TypeError, ValueError) as error:
        raise argparse.ArgumentTypeError("port must be an integer") from error
    if not 1 <= port <= 65535:
        raise argparse.ArgumentTypeError("port must be between 1 and 65535")
    return port


def backend_url(value: str) -> str:
    """Validate an http(s) origin such as http://192.168.1.20:8317 and drop a trailing slash."""
    try:
        parts = urlsplit(value.strip())
        port = parts.port
    except ValueError as error:
        raise argparse.ArgumentTypeError(f"invalid backend URL {value!r}: {error}") from error
    if (
        parts.scheme not in ("http", "https") or not parts.hostname
        or parts.username is not None or parts.password is not None
        or parts.path not in ("", "/") or parts.query or parts.fragment
    ):
        raise argparse.ArgumentTypeError(
            f"invalid backend URL {value!r}: expected http://HOST:PORT (no path or query)"
        )
    if port == 0:
        raise argparse.ArgumentTypeError(f"invalid backend URL {value!r}: port must not be 0")
    return f"{parts.scheme}://{parts.netloc}"


def configured_backend_url() -> str | None:
    """The optional settings.json ``backend_url`` used by ``--role frontend``."""
    from backend.settings import settings_path

    path = settings_path()
    if not path.is_file():
        return None
    try:
        values = json.loads(path.read_text(encoding="utf-8-sig"))
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        raise RunError(f"Cannot read configuration at {path}: {error}") from error
    configured = values.get(BACKEND_URL_SETTING) if isinstance(values, dict) else None
    if configured is None:
        return None
    try:
        if not isinstance(configured, str):
            raise argparse.ArgumentTypeError("expected a URL string")
        return backend_url(configured)
    except argparse.ArgumentTypeError as error:
        raise RunError(f"Invalid {BACKEND_URL_SETTING} in settings at {path}: {error}") from error


def local_url(host: str, port: int) -> str:
    """URL for reaching a listener bound to host:port from this machine."""
    if host == "0.0.0.0":
        host = "127.0.0.1"
    elif host == "::":
        host = "::1"
    if ":" in host:
        host = f"[{host}]"
    return f"http://{host}:{port}"


def parse_args(
    argv: list[str] | None = None, *, default_host: str = "0.0.0.0",
    defaults_path: Path | None = None,
) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    parser.add_argument(
        "--role", choices=ROLES, default="all",
        help="services to run: all (default), frontend only, or backend (API) only",
    )
    parser.add_argument(
        "--frontend", "--frontend-port", dest="frontend_port", type=port_number,
        help="frontend port (default: FRONTEND_PORT or settings.json)",
    )
    parser.add_argument(
        "--backend", "--backend-port", dest="backend_port", type=port_number,
        help="backend port (default: BACKEND_PORT or settings.json)",
    )
    parser.add_argument(
        "--backend-url", type=backend_url, metavar="URL",
        help="--role frontend: backend that /api is proxied to, e.g. http://192.168.1.20:8317 "
             "(default: settings.json backend_url, else this machine's backend port)",
    )
    parser.add_argument(
        "--host", default=os.environ.get("BACKEND_HOST", default_host),
        help="bind address (default: BACKEND_HOST or runtime profile)",
    )
    parser.add_argument("--initialize-settings", action="store_true", help=argparse.SUPPRESS)
    options = parser.parse_args(argv)
    if options.backend_url is not None and options.role != "frontend":
        parser.error("--backend-url is only used with --role frontend")
    defaults = {"frontend_port": 3000, "backend_port": 8000}
    mapping: dict[str, int] | None = None
    if defaults_path is not None:
        from backend.settings import SettingsError, initialize_port_settings, load_port_mapping

        try:
            defaults = initialize_port_settings(defaults_path)
            mapping = load_port_mapping(defaults_path)
        except SettingsError as error:
            raise RunError(str(error)) from error
    options.port_note = None
    if options.initialize_settings:
        return options
    from_settings: list[str] = []
    for field, variable in (("frontend_port", "FRONTEND_PORT"), ("backend_port", "BACKEND_PORT")):
        value = getattr(options, field)
        if value is None and variable not in os.environ:
            from_settings.append(field)
        if value is None:
            value = os.environ.get(variable, defaults[field])
        try:
            setattr(options, field, port_number(value))
        except argparse.ArgumentTypeError as error:
            parser.error(f"{field}: {error}")
    if mapping is not None and any(defaults[field] != mapping[field] for field in from_settings):
        # settings.json wins over port_mapping.json once it holds ports: say so, since editing
        # the mapping beside server.exe would otherwise seem to have no effect.
        options.port_note = (
            f"{defaults['frontend_port']}/{defaults['backend_port']} (frontend/backend) from "
            f"settings.json; port_mapping.json says {mapping['frontend_port']}/"
            f"{mapping['backend_port']}. Edit settings.json, or delete its port fields to use "
            "port_mapping.json."
        )
    if options.role == "frontend" and options.backend_url is None and defaults_path is not None:
        options.backend_url = configured_backend_url()
    # The frontend proxies to this machine's backend port unless a backend URL says otherwise.
    proxies_locally = options.role == "all" or (
        options.role == "frontend" and options.backend_url is None
    )
    if proxies_locally and options.frontend_port == options.backend_port:
        parser.error("frontend and backend ports must be different")
    if options.role == "frontend" and options.backend_url is None:
        options.backend_url = local_url(options.host, options.backend_port)
    return options


def bind_listener(host: str, port: int, label: str) -> socket.socket:
    """Reserve the requested port before starting any application."""
    try:
        addresses = socket.getaddrinfo(host, port, type=socket.SOCK_STREAM)
    except OSError as error:
        raise RunError(f"Cannot resolve bind address {host!r}: {error}") from error
    last_error: OSError | None = None
    for family, kind, protocol, _, address in addresses:
        listener = socket.socket(family, kind, protocol)
        try:
            if os.name == "nt":
                listener.setsockopt(socket.SOL_SOCKET, socket.SO_EXCLUSIVEADDRUSE, 1)
            else:
                listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
            listener.bind(address)
            listener.listen(socket.SOMAXCONN)
            listener.setblocking(False)
            return listener
        except OSError as error:
            listener.close()
            last_error = error
    raise RunError(
        f"Cannot bind {label} to {host}:{port}; choose another port or close "
        f"the service using it. {last_error}"
    ) from last_error


def listener_url(listener: socket.socket) -> str:
    host, port = listener.getsockname()[:2]
    return local_url(host, port)


def forwarded_headers(headers: httpx.Headers | Headers) -> list[tuple[bytes, bytes]]:
    """End-to-end headers of a proxied message.

    Bodies are relayed byte for byte (never decoded), so Content-Length and Content-Encoding stay
    valid; Range, If-Range, Content-Range, Accept-Ranges, ETag and Last-Modified pass unchanged.
    Host, Date and Server describe the connection that carries the message and are set anew.
    """
    excluded = HOP_BY_HOP_HEADERS | {"host", "date", "server"} | {
        item.strip().lower() for item in headers.get("connection", "").split(",")
    }
    return [(key, value) for key, value in headers.raw
            if key.decode("latin-1").lower() not in excluded]


class UpstreamResponse(StreamingResponse):
    """Relays a streamed backend response and always releases its connection.

    Starlette skips background tasks when the browser disconnects (a closed video, a cancelled
    download), so the upstream response is closed in ``finally`` instead.
    """

    def __init__(self, upstream: httpx.Response) -> None:
        super().__init__(upstream.aiter_raw(), status_code=upstream.status_code)
        self.raw_headers = forwarded_headers(upstream.headers)
        self.upstream = upstream

    async def __call__(self, scope: Scope, receive: Receive, send: Send) -> None:
        try:
            await super().__call__(scope, receive, send)
        finally:
            await self.upstream.aclose()


#: Request headers the proxy sets itself: the browser's Origin and Sec-Fetch-Site were checked
#: here (RequestGuard) and mean nothing to the backend, whose address differs; X-Forwarded-For
#: tells the backend which address the browser has.
PROXY_REPLACED_HEADERS = frozenset({"origin", "sec-fetch-site", "x-forwarded-for"})


def has_dot_segment(raw_path: bytes) -> bool:
    """/api/../docs and /api/%2e%2e/docs: HTTPX would normalise them to another route."""
    path = raw_path.split(b"?", 1)[0].decode("latin-1")
    return any(unquote(segment) in (".", "..") for segment in path.split("/"))


def proxied_request_headers(request: Request) -> list[tuple[bytes, bytes]]:
    headers = [
        (key, value) for key, value in forwarded_headers(request.headers)
        if key.decode("latin-1").lower() not in PROXY_REPLACED_HEADERS
    ]
    if "transfer-encoding" in request.headers:
        # RFC 9112 6.3: with Transfer-Encoding a Content-Length is ignored; the body is
        # forwarded chunked, so a stale length must not reach the backend.
        headers = [(key, value) for key, value in headers if key.lower() != b"content-length"]
    if request.client is not None:
        headers.append((b"x-forwarded-for", request.client.host.encode("latin-1")))
    return headers


def create_frontend_app(public: Path, backend_url: str) -> FastAPI:
    """Serve ``public`` and stream /api requests and responses to ``backend_url``."""
    from backend.app.guard import RequestGuard
    from backend.settings import configured_allowed_hosts

    @asynccontextmanager
    async def lifespan(application: FastAPI) -> AsyncIterator[None]:
        # Local traffic must not depend on the target machine's HTTP proxy setup.
        async with httpx.AsyncClient(
            base_url=backend_url, trust_env=False, timeout=PROXY_TIMEOUT,
            limits=httpx.Limits(max_connections=None, max_keepalive_connections=20),
        ) as client:
            # Bodies are relayed undecoded, so the backend must see the browser's own
            # Accept, Accept-Encoding and User-Agent headers, never HTTPX defaults.
            for name in ("accept", "accept-encoding", "user-agent"):
                del client.headers[name]
            application.state.backend_client = client
            yield

    application = FastAPI(
        docs_url=None, redoc_url=None, openapi_url=None, lifespan=lifespan,
    )

    async def proxy(request: Request) -> Response:
        client: httpx.AsyncClient = request.app.state.backend_client
        has_body = "content-length" in request.headers or "transfer-encoding" in request.headers
        # The undecoded path keeps escapes such as %2F inside a library file name intact.
        target: bytes = request.scope.get("raw_path") or request.url.path.encode("utf-8")
        if has_dot_segment(target):
            return JSONResponse({"detail": "Invalid path"}, status_code=400)
        if request.scope["query_string"]:
            target += b"?" + request.scope["query_string"]
        upstream_request = client.build_request(
            request.method, httpx.URL(raw_path=target),
            headers=proxied_request_headers(request),
            # Request bodies (multi-GB video uploads) are forwarded chunk by chunk.
            content=request.stream() if has_body else None,
        )
        try:
            upstream = await client.send(upstream_request, stream=True)
        except (h11.LocalProtocolError, httpx.LocalProtocolError):
            return JSONResponse({"detail": "Malformed request"}, status_code=400)
        except httpx.RequestError:
            return JSONResponse({"detail": "Backend is unavailable"}, status_code=502)
        except ClientDisconnect:
            # The browser cancelled its upload; nobody is left to read a response.
            return Response(status_code=400)
        return UpstreamResponse(upstream)

    methods = ["GET", "HEAD", "POST", "PUT", "PATCH", "DELETE", "OPTIONS"]
    application.add_api_route("/api", proxy, methods=methods)
    application.add_api_route("/api/{path:path}", proxy, methods=methods)
    application.mount("/", StaticFiles(directory=public, html=True), name="public")
    # Same checks as the API: no DNS-rebound host names, no cross-site writes through /api.
    application.add_middleware(RequestGuard, allowed_hosts=configured_allowed_hosts)
    return application


class ManagedServer(uvicorn.Server):
    """Several listeners share one process-level signal handler."""

    @contextmanager
    def capture_signals(self) -> Iterator[None]:
        yield

    def install_signal_handlers(self) -> None:
        # Compatibility with Uvicorn releases before capture_signals was added.
        pass


Service = tuple[str, FastAPI, socket.socket]


async def serve_applications(services: Sequence[Service]) -> int:
    """Serve each (label, application, listener) until a stop signal; one failure stops all."""
    servers = [
        ManagedServer(uvicorn.Config(
            application, loop="asyncio", http="h11", ws="none", lifespan="on",
            access_log=False, use_colors=False, timeout_graceful_shutdown=5,
        ))
        for _, application, _ in services
    ]
    labels = [label for label, _, _ in services]
    stopping = False

    def stop_services(signum: int, frame: object) -> None:
        nonlocal stopping
        for server in servers:
            server.should_exit = True
            if stopping:
                server.force_exit = True
        if not stopping:
            print("\n[run] Stopping services...", flush=True)
        stopping = True

    async def serve(server: ManagedServer, listener: socket.socket, label: str) -> None:
        try:
            await server.serve(sockets=[listener])
        except SystemExit as error:
            # Keep startup failure inside this task so its siblings can shut down.
            raise RunError(f"{label} failed to start (exit code {error.code})") from error

    signals = [signal.SIGINT, signal.SIGTERM]
    if hasattr(signal, "SIGBREAK"):
        signals.append(signal.SIGBREAK)
    previous = {item: signal.signal(item, stop_services) for item in signals}
    tasks = [
        asyncio.create_task(serve(server, listener, label))
        for server, (label, _, listener) in zip(servers, services)
    ]
    try:
        await asyncio.wait(tasks, return_when=asyncio.FIRST_COMPLETED)
    finally:
        for server in servers:
            server.should_exit = True
        results = await asyncio.gather(*tasks, return_exceptions=True)
        for item, handler in previous.items():
            signal.signal(item, handler)
    for label, result in zip(labels, results):
        if isinstance(result, BaseException):
            raise RunError(f"{label} failed: {result}") from result
    if not stopping:
        raise RunError("A service stopped unexpectedly; all services have been stopped")
    return 0


def configure_console() -> None:
    """Output redirected to a file or pipe is encoded with the ANSI code page, which cannot
    encode every path (a Cyrillic or CJK user or folder name): such characters are escaped
    instead of stopping the server."""
    for stream in (sys.stdout, sys.stderr):
        reconfigure = getattr(stream, "reconfigure", None)
        if reconfigure is not None:
            try:
                reconfigure(errors="backslashreplace")
            except (OSError, ValueError):
                pass


def main(argv: list[str] | None = None) -> int:
    configure_console()
    root = runtime_directory()
    load_environment(root)
    if not getattr(sys, "frozen", False):
        sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
    options = parse_args(
        argv, default_host=runtime_host(root), defaults_path=root / "port_mapping.json",
    )
    if options.initialize_settings:
        return 0
    public = root / "public"
    if options.role != "backend" and not (public / "index.html").is_file():
        raise RunError(f"Frontend is missing at {public}; rebuild with build.bat")

    listeners: list[socket.socket] = []
    backend_socket: socket.socket | None = None
    frontend_socket: socket.socket | None = None
    try:
        if options.role != "frontend":
            backend_socket = bind_listener(options.host, options.backend_port, "backend")
            listeners.append(backend_socket)
        if options.role != "backend":
            frontend_socket = bind_listener(options.host, options.frontend_port, "frontend")
            listeners.append(frontend_socket)
        from backend.network import lan_urls
        from backend.settings import settings_path

        services: list[Service] = []
        details: list[str] = []
        if backend_socket is not None:
            from backend.app.main import configured_cors_origins, create_app
            from backend.settings import (
                SettingsError, configured_cache_directory, configured_database_path,
            )

            try:
                database = configured_database_path(root)
                cache = configured_cache_directory(root)
            except SettingsError as error:
                raise RunError(str(error)) from error
            # The packaged UI is same-origin through the proxy: no other origin is trusted
            # unless CORS_ORIGINS names it (the Vite dev default does not apply here).
            services.append(("Backend", create_app(
                database, cache_directory=cache, cors_origins=configured_cors_origins(""),
            ), backend_socket))
            details += [f"Database: {database}", f"Cache:    {cache}"]
        if frontend_socket is not None:
            # Role all proxies to its own backend; role frontend to the configured URL.
            proxied = options.backend_url if backend_socket is None else listener_url(backend_socket)
            services.append(("Frontend", create_frontend_app(public, proxied), frontend_socket))

        print("Starting Instagram Clone Full-Stack Services")
        print(f"Role:     {options.role}")
        print(f"Python:   {sys.executable}")
        if backend_socket is not None:
            url = listener_url(backend_socket)
            print(f"Backend:  {url} (API docs: {url}/docs)")
        else:
            print(f"Backend:  {options.backend_url} (proxied at /api)")
        if frontend_socket is not None:
            print(f"Frontend: {listener_url(frontend_socket)}")
        backend_lan_urls = lan_urls(backend_socket) if backend_socket is not None else []
        if frontend_socket is not None:
            by_host = {urlsplit(url).hostname: url for url in backend_lan_urls}
            frontend_lan_urls = lan_urls(frontend_socket)
            for url in frontend_lan_urls:
                print(f"LAN:      {url}")
                api_url = by_host.get(urlsplit(url).hostname)
                if api_url is not None:
                    print(f"LAN API:  {api_url}/docs")
            if frontend_lan_urls:
                print("Open a LAN URL on another device connected to the same network.")
        else:
            for url in backend_lan_urls:
                print(f"LAN API:  {url} (API docs: {url}/docs)")
            if backend_lan_urls:
                print("Start a frontend on another machine with: "
                      f"run-frontend.bat --backend-url {backend_lan_urls[0]}")
        print(f"Settings: {settings_path()}")
        if getattr(options, "port_note", None):
            print(f"Ports:    {options.port_note}")
        for line in details:
            print(line)
        print("[run] Press Ctrl+C to stop.", flush=True)
        return asyncio.run(serve_applications(services))
    finally:
        for listener in listeners:
            listener.close()


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except KeyboardInterrupt:
        raise SystemExit(0)
    except (RunError, OSError) as error:
        print(f"[run] ERROR: {error}", file=sys.stderr)
        raise SystemExit(1)
