"""Request checks shared by the API and the packaged frontend server.

* Host: a DNS-rebinding page reaches the app under its own domain name, so only host names a
  rebinding attacker cannot control are accepted: IP addresses, single-label names (localhost,
  the computer's name), *.localhost, this computer's own names and the names configured in
  ALLOWED_HOSTS / settings.json "allowed_hosts".
* Writes: a request that changes something (POST, PUT, PATCH, DELETE) must come from the app's
  own pages: a browser marks it same-origin (Sec-Fetch-Site) or sends an Origin equal to the
  address it asked for or one of the trusted (CORS) origins. Clients that are not browsers send
  neither header and are not affected.
"""

from __future__ import annotations

import functools
import ipaddress
import logging
import socket
from collections.abc import Callable, Iterable
from urllib.parse import urlsplit

from starlette.datastructures import Headers
from starlette.responses import JSONResponse
from starlette.types import ASGIApp, Receive, Scope, Send

LOGGER = logging.getLogger(__name__)

SAFE_METHODS = frozenset({"GET", "HEAD", "OPTIONS", "TRACE"})
#: The address of the browser, set by the packaged frontend server when it forwards /api.
FORWARDED_FOR = "x-forwarded-for"


def host_name(host: str) -> str | None:
    """The lower-case name of a Host header ("[::1]:80" -> "::1", "a.b.:80" -> "a.b")."""
    value = host.strip()
    if value.startswith("["):
        end = value.find("]")
        if end < 0:
            return None
        name = value[1:end]
    elif value.count(":") == 1:
        name = value.split(":", 1)[0]
    else:
        name = value
    name = name.rstrip(".").lower()
    return name or None


def _is_ip(name: str) -> bool:
    try:
        ipaddress.ip_address(name.split("%", 1)[0])
    except ValueError:
        return False
    return True


@functools.lru_cache(maxsize=1)
def machine_names() -> frozenset[str]:
    """This computer's own names (host name, FQDN, mDNS .local name), lower case."""
    names: set[str] = set()
    try:
        hostname = socket.gethostname()
        names.update({hostname, f"{hostname}.local", socket.getfqdn()})
    except OSError:
        pass
    return frozenset(name.rstrip(".").lower() for name in names if name)


def host_allowed(name: str | None, extra: Iterable[str] = ()) -> bool:
    if not name:
        return False
    extra_names = {item.strip().rstrip(".").lower() for item in extra if item.strip()}
    if "*" in extra_names or name in extra_names:
        return True
    if _is_ip(name) or "." not in name or name.endswith(".localhost"):
        return True
    return name in machine_names()


def _is_loopback(address: str) -> bool:
    try:
        return ipaddress.ip_address(address.strip().split("%", 1)[0]).is_loopback
    except ValueError:
        return False


def is_local_client(scope: Scope) -> bool:
    """The request comes from this computer. Behind a proxy on this computer (the packaged
    frontend server) the browser's address in X-Forwarded-For counts instead; a peer that is
    no IP address (an in-process test client, a Unix socket) is local."""
    client = scope.get("client")
    peer = client[0] if client else None
    if peer is None:
        return True
    try:
        ipaddress.ip_address(peer.split("%", 1)[0])
    except ValueError:
        return True
    if not _is_loopback(peer):
        return False
    forwarded = Headers(scope=scope).get(FORWARDED_FOR)
    if forwarded is None:
        return True
    return _is_loopback(forwarded.split(",")[-1])


def _origin_key(origin: str) -> tuple[str, str, int | None] | None:
    try:
        parts = urlsplit(origin.strip())
        port = parts.port
    except ValueError:
        return None
    if not parts.scheme or not parts.hostname:
        return None
    scheme = parts.scheme.lower()
    default = {"http": 80, "https": 443}.get(scheme)
    return scheme, parts.hostname.rstrip(".").lower(), port if port is not None else default


class RequestGuard:
    """ASGI middleware applying the Host and write-origin checks described above."""

    def __init__(
        self,
        app: ASGIApp,
        *,
        trusted_origins: Iterable[str] = (),
        allowed_hosts: Callable[[], Iterable[str]] | Iterable[str] = (),
    ) -> None:
        self.app = app
        origins = [origin.strip() for origin in trusted_origins if origin.strip()]
        self.trust_any_origin = "*" in origins
        self.trusted = {key for origin in origins if (key := _origin_key(origin)) is not None}
        self._allowed_hosts = allowed_hosts
        self._extra_hosts: list[str] | None = None

    def extra_hosts(self) -> list[str]:
        if self._extra_hosts is None:
            source = self._allowed_hosts
            try:
                self._extra_hosts = list(source() if callable(source) else source)
            except Exception as error:  # a broken settings file must not take the app down
                LOGGER.warning("Ignoring the configured allowed host names: %s", error)
                self._extra_hosts = []
        return self._extra_hosts

    def _host_ok(self, host: str | None) -> bool:
        if host is None:
            return True  # HTTP/1.0 without Host: no browser sends that
        name = host_name(host)
        if name is not None and (_is_ip(name) or "." not in name or name.endswith(".localhost")):
            return True  # the common cases need no settings lookup
        return host_allowed(name, self.extra_hosts())

    def _write_ok(self, scope: Scope, headers: Headers) -> bool:
        site = headers.get("sec-fetch-site")
        if site in ("same-origin", "none"):
            return True
        origin = headers.get("origin")
        if origin is None:
            # Browsers always send Origin with a cross-site write; other clients send neither.
            return site is None
        if self.trust_any_origin:
            return True
        key = _origin_key(origin)
        if key is None:
            return False  # "null" (sandboxed frames, file: pages) and garbage
        if key in self.trusted:
            return True
        host = headers.get("host")
        own = _origin_key(f"{scope.get('scheme', 'http')}://{host}") if host else None
        return own is not None and key == own

    async def __call__(self, scope: Scope, receive: Receive, send: Send) -> None:
        if scope["type"] != "http":
            await self.app(scope, receive, send)
            return
        headers = Headers(scope=scope)
        host = headers.get("host")
        if not self._host_ok(host):
            response = JSONResponse(
                {
                    "detail": "This host name is not allowed. Open the app by IP address, "
                    "localhost or this computer's name, or add the name to \"allowed_hosts\" "
                    "in settings.json (or ALLOWED_HOSTS)."
                },
                status_code=400,
            )
            await response(scope, receive, send)
            return
        if scope["method"].upper() not in SAFE_METHODS and not self._write_ok(scope, headers):
            response = JSONResponse(
                {"detail": "Changes are only accepted from the app's own pages."}, status_code=403
            )
            await response(scope, receive, send)
            return
        await self.app(scope, receive, send)
