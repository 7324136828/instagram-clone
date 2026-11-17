from __future__ import annotations

import asyncio
import hashlib
import io
import json
import os
import socket
import subprocess
import sys
import tempfile
import threading
import time
import tracemalloc
import unittest
from collections.abc import AsyncIterator, Iterator
from contextlib import contextmanager, redirect_stderr, redirect_stdout
from pathlib import Path
from typing import Any
from unittest.mock import Mock, patch

import httpx
import uvicorn
from fastapi import FastAPI, Request
from starlette.requests import ClientDisconnect
from starlette.responses import FileResponse, StreamingResponse

from backend import packaged
from backend.network import discover_lan_ipv4

PROJECT_ROOT = Path(__file__).resolve().parents[2]
ENTRYPOINT = """
import sys
from pathlib import Path
sys.path.insert(0, sys.argv[1])
from backend import packaged
packaged.runtime_directory = lambda: Path(sys.argv[2])
try:
    raise SystemExit(packaged.main(sys.argv[3:]))
except (packaged.RunError, OSError) as error:
    print(f'[run] ERROR: {error}', file=sys.stderr)
    raise SystemExit(1)
"""

# On Windows a virtual environment's python.exe is a launcher that runs the base interpreter in a
# child process. Terminating the launcher left that child (whose working directory is the test's
# temporary folder) to be killed asynchronously, so folder cleanup failed with PermissionError.
# Start the base interpreter itself, configured for the virtual environment as the launcher does.
if os.name == "nt" and sys.prefix != sys.base_prefix:
    PYTHON = getattr(sys, "_base_executable", sys.executable)
    PYTHON_ENVIRONMENT = {"__PYVENV_LAUNCHER__": sys.executable}
else:
    PYTHON = sys.executable
    PYTHON_ENVIRONMENT = {}


def free_ports() -> tuple[int, int]:
    with socket.socket() as first, socket.socket() as second:
        first.bind(("127.0.0.1", 0))
        second.bind(("127.0.0.1", 0))
        return first.getsockname()[1], second.getsockname()[1]


class PackagedRuntimeTests(unittest.TestCase):
    def setUp(self) -> None:
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        # Match settings path normalization, including Windows short-name aliases.
        self.settings_root = Path(directory.name).resolve()
        (self.settings_root / "temp").mkdir()

    def environment(self) -> dict[str, str]:
        environment = os.environ.copy()
        for name in (
            "BACKEND_PORT", "FRONTEND_PORT", "BACKEND_HOST", "COUNTER_DB_PATH",
            "INSTAGRAM_CACHE_DIR",
        ):
            environment.pop(name, None)
        environment.update(PYTHON_ENVIRONMENT)
        environment.update({
            "APPDATA": str(self.settings_root / "appdata"),
            "XDG_CONFIG_HOME": str(self.settings_root / "appdata"),
            "TEMP": str(self.settings_root / "temp"),
            "TMP": str(self.settings_root / "temp"),
            "TMPDIR": str(self.settings_root / "temp"),
        })
        return environment

    def command(self, root: Path, *arguments: str) -> list[str]:
        return [PYTHON, "-c", ENTRYPOINT, str(PROJECT_ROOT), str(root), *arguments]

    @contextmanager
    def serving(
        self, root: Path, arguments: list[str], ready_url: str, *, cwd: Path | None = None,
        environment: dict[str, str] | None = None,
    ) -> Iterator[tuple[httpx.Client, Any]]:
        """Run the packaged entry point until ready_url answers 200; yield a client and the
        function returning the process output so far."""
        with tempfile.TemporaryFile(mode="w+b") as log:
            def output() -> str:
                log.seek(0)
                return log.read().decode("utf-8", errors="replace")

            process = subprocess.Popen(
                self.command(root, *arguments), cwd=cwd or root,
                env={**self.environment(), **(environment or {})},
                stdout=log, stderr=subprocess.STDOUT,
                creationflags=subprocess.CREATE_NO_WINDOW if os.name == "nt" else 0,
            )
            try:
                with httpx.Client(trust_env=False, timeout=5) as client:
                    deadline = time.monotonic() + 15
                    while time.monotonic() < deadline:
                        if process.poll() is not None:
                            self.fail(output())
                        try:
                            if client.get(ready_url, timeout=1).status_code == 200:
                                break
                        except httpx.RequestError:
                            pass
                        time.sleep(0.05)
                    else:
                        self.fail(f"Portable service did not become ready at {ready_url}")
                    yield client, output
            finally:
                if process.poll() is None:
                    process.terminate()
                    try:
                        process.wait(timeout=5)
                    except subprocess.TimeoutExpired:
                        process.kill()
                        process.wait(timeout=5)

    @contextmanager
    def running(
        self, root: Path, frontend: int, backend: int, *, cwd: Path | None = None,
        use_cli_ports: bool = True,
    ) -> Iterator[httpx.Client]:
        arguments = ["--frontend", str(frontend), "--backend-port", str(backend)] if use_cli_ports else []
        with self.serving(
            root, arguments, f"http://127.0.0.1:{frontend}/api/health", cwd=cwd,
        ) as (client, _output):
            yield client

    def make_mapping(self, root: Path, frontend: int = 3000, backend: int = 8000) -> Path:
        path = root / "port_mapping.json"
        path.write_text(json.dumps({"frontend_port": frontend, "backend_port": backend}), encoding="utf-8")
        return path

    def settings_file(self) -> Path:
        return self.settings_root / "appdata" / "instagram-clone" / "settings.json"

    def make_public(self, root: Path) -> None:
        self.make_mapping(root)
        (root / "public").mkdir()
        (root / "public" / "index.html").write_text("<h1>Portable frontend</h1>")
        (root / ".env").write_text("COUNTER_DB_PATH=data/portable.db\n")

    def test_database_override_expands_environment_and_blank_uses_settings(self) -> None:
        root = self.settings_root
        storage = root / "selected data"
        variable = "%COUNTER_STORAGE_TEST%" if os.name == "nt" else "$COUNTER_STORAGE_TEST"
        environment = self.environment()
        environment.update({
            "COUNTER_STORAGE_TEST": str(storage),
            "COUNTER_DB_PATH": variable + "/counter.db",
        })
        with patch.dict(os.environ, environment, clear=True):
            packaged.load_environment(root)
            self.assertEqual(os.environ["COUNTER_DB_PATH"], str((storage / "counter.db").resolve()))
        for value in ("", " \t "):
            with self.subTest(value=value), patch.dict(
                os.environ, {**self.environment(), "COUNTER_DB_PATH": value}, clear=True,
            ):
                packaged.load_environment(root)
                self.assertNotIn("COUNTER_DB_PATH", os.environ)

    def test_default_lan_and_explicit_bind_configuration(self) -> None:
        with patch.dict(os.environ, self.environment(), clear=True):
            self.assertEqual(packaged.parse_args([]).host, "0.0.0.0")
            self.assertEqual(packaged.parse_args([], default_host="127.0.0.1").host, "127.0.0.1")
            self.assertEqual(packaged.parse_args(["--host", "127.0.0.1"]).host, "127.0.0.1")
            with patch.dict(os.environ, {"BACKEND_HOST": "192.168.1.156"}):
                self.assertEqual(packaged.parse_args([]).host, "192.168.1.156")
                self.assertEqual(
                    packaged.parse_args([], default_host="127.0.0.1").host, "192.168.1.156",
                )
                self.assertEqual(packaged.parse_args(["--host", "127.0.0.1"]).host, "127.0.0.1")

    def test_port_precedence_is_cli_then_environment_then_settings(self) -> None:
        root = self.settings_root
        mapping = self.make_mapping(root)
        settings = self.settings_file()
        settings.parent.mkdir(parents=True)
        original = json.dumps({"frontend_port": 4100, "backend_port": 4200, "custom_setting": "preserved"})
        settings.write_text(original, encoding="utf-8")
        for environment_ports, arguments, expected in (
            ({}, [], (4100, 4200)),
            ({"FRONTEND_PORT": "5100"}, [], (5100, 4200)),
            ({"BACKEND_PORT": "5200"}, [], (4100, 5200)),
            ({"FRONTEND_PORT": "5100", "BACKEND_PORT": "5200"}, [], (5100, 5200)),
            ({"FRONTEND_PORT": "5100", "BACKEND_PORT": "5200"},
             ["--frontend", "6100", "--backend-port", "6200"], (6100, 6200)),
        ):
            with self.subTest(environment=environment_ports, arguments=arguments), patch.dict(
                os.environ, {**self.environment(), **environment_ports}, clear=True,
            ):
                options = packaged.parse_args(arguments, defaults_path=mapping)
                self.assertEqual((options.frontend_port, options.backend_port), expected)
                self.assertEqual(settings.read_text(encoding="utf-8"), original)

    def test_saved_ports_that_differ_from_the_port_mapping_are_pointed_out(self) -> None:
        mapping = self.make_mapping(self.settings_root, 13999, 18999)
        settings = self.settings_file()
        settings.parent.mkdir(parents=True)
        settings.write_text(json.dumps({"frontend_port": 3127, "backend_port": 8317}), encoding="utf-8")
        with patch.dict(os.environ, self.environment(), clear=True):
            note = packaged.parse_args([], defaults_path=mapping).port_note
            self.assertEqual(
                note,
                "3127/8317 (frontend/backend) from settings.json; port_mapping.json says "
                "13999/18999. Edit settings.json, or delete its port fields to use port_mapping.json.",
            )
            # Ports given for this launch are what the user asked for: nothing to point out.
            self.assertIsNone(packaged.parse_args(
                ["--frontend", "4000", "--backend", "9000"], defaults_path=mapping,
            ).port_note)
        with patch.dict(os.environ, {**self.environment(), "FRONTEND_PORT": "4000", "BACKEND_PORT": "9000"}, clear=True):
            self.assertIsNone(packaged.parse_args([], defaults_path=mapping).port_note)
        # Only the backend port comes from settings.json, and it differs.
        with patch.dict(os.environ, self.environment(), clear=True):
            self.assertIsNotNone(packaged.parse_args(["--frontend", "4000"], defaults_path=mapping).port_note)
        # Matching ports (the usual case) print nothing.
        same = self.make_mapping(self.settings_root, 3127, 8317)
        with patch.dict(os.environ, self.environment(), clear=True):
            self.assertIsNone(packaged.parse_args([], defaults_path=same).port_note)

    def test_effective_environment_and_cli_ports_are_validated(self) -> None:
        mapping = self.make_mapping(self.settings_root)
        for environment_ports, arguments in (
            ({"FRONTEND_PORT": "0"}, []),
            ({"BACKEND_PORT": "65536"}, []),
            ({"FRONTEND_PORT": "abc"}, []),
            ({"FRONTEND_PORT": "8000"}, []),
            ({"BACKEND_PORT": "4100"}, ["--frontend", "4100"]),
        ):
            with self.subTest(environment=environment_ports, arguments=arguments), patch.dict(
                os.environ, {**self.environment(), **environment_ports}, clear=True,
            ), redirect_stderr(io.StringIO()), self.assertRaises(SystemExit) as raised:
                packaged.parse_args(arguments, defaults_path=mapping)
            self.assertEqual(raised.exception.code, 2)

    def test_runtime_profile_defaults_and_validation(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            profile = root / "runtime.json"
            self.assertEqual(packaged.runtime_host(root), "0.0.0.0")
            for host in ("127.0.0.1", "0.0.0.0"):
                profile.write_text(json.dumps({"host": host}), encoding="utf-8")
                self.assertEqual(packaged.runtime_host(root), host)
            for content in (
                "{", "[]", "{}", '{"host": "localhost"}',
                '{"host": null}', '{"host": ["127.0.0.1"]}',
            ):
                with self.subTest(content=content):
                    profile.write_text(content, encoding="utf-8")
                    with self.assertRaises(packaged.RunError) as raised:
                        packaged.runtime_host(root)
                    self.assertIn(str(profile), str(raised.exception))
                    self.assertIn("runtime profile", str(raised.exception))
            profile.unlink()
            profile.mkdir()
            with self.assertRaisesRegex(packaged.RunError, "Cannot read runtime profile"):
                packaged.runtime_host(root)

    def test_frozen_runtime_profile_follows_executable_after_folder_rename(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / "Debug"
            root.mkdir()
            (root / "runtime.json").write_text('{"host": "127.0.0.1"}', encoding="utf-8")
            for name in ("Debug", "RenamedPortable"):
                with self.subTest(folder=name):
                    if root.name != name:
                        root = root.rename(root.with_name(name))
                    with (
                        patch.object(packaged.sys, "frozen", True, create=True),
                        patch.object(packaged.sys, "executable", str(root / "server.exe")),
                    ):
                        self.assertEqual(packaged.runtime_directory(), root.resolve())
                        self.assertEqual(packaged.runtime_host(packaged.runtime_directory()), "127.0.0.1")

    def test_profile_environment_file_process_environment_and_cli_precedence(self) -> None:
        for profile_host in ("127.0.0.1", "0.0.0.0"):
            for file_host, process_host, arguments, expected in (
                (None, None, [], profile_host),
                ("192.168.1.156", None, [], "192.168.1.156"),
                ("192.168.1.156", "192.168.1.157", [], "192.168.1.157"),
                ("192.168.1.156", "192.168.1.157", ["--host", "127.0.0.1"], "127.0.0.1"),
            ):
                with self.subTest(profile=profile_host, file=file_host, process=process_host, cli=arguments):
                    with tempfile.TemporaryDirectory() as directory:
                        root = Path(directory)
                        self.make_public(root)
                        (root / "runtime.json").write_text(json.dumps({"host": profile_host}), encoding="utf-8")
                        if file_host is not None:
                            (root / ".env").write_text(f"BACKEND_HOST={file_host}\n", encoding="utf-8")
                        environment = self.environment()
                        if process_host is not None:
                            environment["BACKEND_HOST"] = process_host
                        with (
                            patch.dict(os.environ, environment, clear=True),
                            patch.object(packaged, "runtime_directory", return_value=root),
                            patch.object(packaged, "bind_listener", side_effect=packaged.RunError("binding checked")) as bind,
                            self.assertRaisesRegex(packaged.RunError, "binding checked"),
                        ):
                            packaged.main(arguments)
                        bind.assert_called_once_with(expected, 8000, "backend")

    def test_startup_links_follow_binding_and_proxy_uses_local_backend(self) -> None:
        addresses = ["192.168.1.156", "192.168.56.1"]
        for host, arguments, expected_addresses in (
            ("0.0.0.0", [], addresses),
            ("127.0.0.1", ["--host", "127.0.0.1"], []),
            ("192.168.1.156", ["--host", "192.168.1.156"], addresses[:1]),
        ):
            with self.subTest(host=host), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                self.make_public(root)
                backend_socket = Mock(spec=socket.socket, family=socket.AF_INET)
                frontend_socket = Mock(spec=socket.socket, family=socket.AF_INET)
                backend_socket.getsockname.return_value = (host, 8000)
                frontend_socket.getsockname.return_value = (host, 3000)
                output = io.StringIO()
                with (
                    patch.dict(os.environ, self.environment(), clear=True),
                    patch.object(packaged, "runtime_directory", return_value=root),
                    patch.object(packaged, "bind_listener", side_effect=[backend_socket, frontend_socket]) as bind,
                    patch("backend.network.discover_lan_ipv4", return_value=addresses),
                    patch.object(packaged, "create_frontend_app") as frontend_app,
                    patch.object(packaged, "serve_applications", new=Mock(return_value=object())),
                    patch.object(packaged.asyncio, "run", return_value=0),
                    redirect_stdout(output),
                ):
                    self.assertEqual(packaged.main(arguments), 0)
                self.assertEqual(bind.call_args_list[0].args, (host, 8000, "backend"))
                self.assertEqual(bind.call_args_list[1].args, (host, 3000, "frontend"))
                local_host = "127.0.0.1" if host == "0.0.0.0" else host
                frontend_app.assert_called_once_with(root / "public", f"http://{local_host}:8000")
                lines = output.getvalue().splitlines()
                self.assertIn("Starting Instagram Clone Full-Stack Services", lines)
                self.assertIn(f"Frontend: http://{local_host}:3000", lines)
                self.assertIn(
                    f"Backend:  http://{local_host}:8000 (API docs: http://{local_host}:8000/docs)",
                    lines,
                )
                self.assertEqual(
                    [line for line in lines if line.startswith("LAN: ")],
                    [f"LAN:      http://{address}:3000" for address in expected_addresses],
                )
                self.assertEqual(
                    [line for line in lines if line.startswith("LAN API: ")],
                    [f"LAN API:  http://{address}:8000/docs" for address in expected_addresses],
                )
                backend_socket.close.assert_called_once()
                frontend_socket.close.assert_called_once()

    def test_release_profile_serves_frontend_and_proxy_over_lan(self) -> None:
        addresses = discover_lan_ipv4()
        if not addresses:
            self.skipTest("No local LAN IPv4 address is available")
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / "Release"
            root.mkdir()
            self.make_public(root)
            (root / "runtime.json").write_text('{"host": "0.0.0.0"}', encoding="utf-8")
            frontend, backend = free_ports()
            with self.running(root, frontend, backend) as client:
                for address in addresses:
                    with self.subTest(address=address):
                        frontend_url = f"http://{address}:{frontend}"
                        backend_url = f"http://{address}:{backend}"
                        self.assertIn("Portable frontend", client.get(frontend_url).text)
                        self.assertEqual(client.get(frontend_url + "/api/health").json(), {"status": "ok"})
                        self.assertEqual(client.get(backend_url + "/docs").status_code, 200)
                        count = client.post(frontend_url + "/api/counter/click").json()["count"]
                        self.assertEqual(client.get(backend_url + "/api/counter").json()["count"], count)

    def test_debug_profile_serves_local_proxy_and_refuses_lan_after_folder_rename(self) -> None:
        addresses = discover_lan_ipv4()
        if not addresses:
            self.skipTest("No local LAN IPv4 address is available")
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / "Debug"
            root.mkdir()
            self.make_public(root)
            (root / "runtime.json").write_text('{"host": "127.0.0.1"}', encoding="utf-8")
            root = root.rename(root.with_name("RenamedPortable"))
            frontend, backend = free_ports()
            frontend_url = f"http://127.0.0.1:{frontend}"
            backend_url = f"http://127.0.0.1:{backend}"
            with self.running(root, frontend, backend) as client:
                self.assertIn("Portable frontend", client.get(frontend_url).text)
                self.assertEqual(client.get(frontend_url + "/api/health").json(), {"status": "ok"})
                count = client.post(frontend_url + "/api/counter/click").json()["count"]
                self.assertEqual(client.get(backend_url + "/api/counter").json()["count"], count)
                for address in addresses:
                    for port in (frontend, backend):
                        with self.subTest(address=address, port=port):
                            with self.assertRaises((httpx.ConnectError, httpx.ConnectTimeout)):
                                client.get(f"http://{address}:{port}/api/health")

    def test_separate_ports_proxy_static_files_and_persistent_state(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self.make_public(root)
            frontend, backend = free_ports()
            frontend_url = f"http://127.0.0.1:{frontend}"
            backend_url = f"http://127.0.0.1:{backend}"
            with self.running(root, frontend, backend) as client:
                self.assertIn("Portable frontend", client.get(frontend_url).text)
                self.assertEqual(client.get(backend_url + "/api/health").json(), {"status": "ok"})
                self.assertEqual(client.get(frontend_url + "/api/counter").json()["count"], 0)
                self.assertEqual(client.post(frontend_url + "/api/counter/click").json()["count"], 1)
                self.assertEqual(client.get(backend_url + "/api/counter").json()["count"], 1)
                self.assertEqual(client.get(frontend_url + "/%2e%2e/.env").status_code, 404)
            self.assertTrue((root / "data" / "portable.db").is_file())
            frontend, backend = free_ports()
            with self.running(root, frontend, backend) as client:
                frontend_url = f"http://127.0.0.1:{frontend}"
                self.assertEqual(client.get(frontend_url + "/api/counter").json()["count"], 1)
                self.assertEqual(client.delete(frontend_url + "/api/counter").json()["count"], 0)

    def test_default_storage_survives_restart_and_distribution_folder_change(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            workspace = Path(directory)
            root = workspace / "Debug"
            root.mkdir()
            self.make_public(root)
            (root / ".env").unlink()
            (root / "runtime.json").write_text('{"host": "127.0.0.1"}', encoding="utf-8")
            settings = self.settings_root / "appdata" / "instagram-clone" / "settings.json"
            database = self.settings_root / "temp" / "instagram-clone" / "counter.db"
            frontend, backend = free_ports()
            with self.running(root, frontend, backend, cwd=workspace) as client:
                url = f"http://127.0.0.1:{frontend}/api/counter"
                self.assertEqual(client.get(url).json()["count"], 0)
                self.assertEqual(client.post(url + "/click").json()["count"], 1)
            original_settings = settings.read_text(encoding="utf-8")
            self.assertEqual(json.loads(original_settings), {"frontend_port": 3000, "backend_port": 8000})
            self.assertTrue(database.is_file())
            self.assertFalse((root / "data").exists())

            root = root.rename(workspace / "Release")
            (root / "runtime.json").write_text('{"host": "0.0.0.0"}', encoding="utf-8")
            frontend, backend = free_ports()
            with self.running(root, frontend, backend, cwd=workspace) as client:
                self.assertEqual(
                    client.get(f"http://127.0.0.1:{frontend}/api/counter").json()["count"], 1,
                )
            self.assertEqual(settings.read_text(encoding="utf-8"), original_settings)

    def test_settings_select_custom_storage_across_restarts(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            workspace = Path(directory)
            root = workspace / "distribution"
            root.mkdir()
            self.make_public(root)
            (root / ".env").unlink()
            settings = self.settings_root / "appdata" / "instagram-clone" / "settings.json"
            settings.parent.mkdir(parents=True)
            storage = workspace / "selected data" / "nested"
            content = json.dumps({"data_directory": str(storage), "custom_setting": "preserved"})
            settings.write_text(content, encoding="utf-8")
            for expected_count in (0, 1):
                frontend, backend = free_ports()
                with self.running(root, frontend, backend, cwd=workspace) as client:
                    url = f"http://127.0.0.1:{frontend}/api/counter"
                    self.assertEqual(client.get(url).json()["count"], expected_count)
                    if expected_count == 0:
                        self.assertEqual(client.post(url + "/click").json()["count"], 1)
            self.assertTrue((storage / "counter.db").is_file())
            self.assertEqual(json.loads(settings.read_text(encoding="utf-8")), {
                "data_directory": str(storage), "custom_setting": "preserved",
                "frontend_port": 3000, "backend_port": 8000,
            })

    def test_settings_ports_start_frontend_backend_and_proxy_without_cli(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self.make_public(root)
            frontend, backend = free_ports()
            settings = self.settings_file()
            settings.parent.mkdir(parents=True)
            original = json.dumps({
                "frontend_port": frontend, "backend_port": backend,
                "custom_setting": "preserved",
            })
            settings.write_text(original, encoding="utf-8")
            with self.running(root, frontend, backend, use_cli_ports=False) as client:
                frontend_url = f"http://127.0.0.1:{frontend}"
                backend_url = f"http://127.0.0.1:{backend}"
                self.assertIn("Portable frontend", client.get(frontend_url).text)
                self.assertEqual(client.get(backend_url + "/api/health").json(), {"status": "ok"})
                self.assertEqual(client.post(frontend_url + "/api/counter/click").json()["count"], 1)
                self.assertEqual(client.get(backend_url + "/api/counter").json()["count"], 1)
            self.assertEqual(settings.read_text(encoding="utf-8"), original)

    def test_partial_settings_fill_only_missing_ports_and_start_services(self) -> None:
        for missing in ("frontend_port", "backend_port", "both"):
            with self.subTest(missing=missing), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                self.make_public(root)
                frontend, backend = free_ports()
                # The populated port deliberately differs from the corresponding
                # distribution default so startup proves it was preserved.
                if missing == "frontend_port":
                    self.make_mapping(root, frontend, 8000)
                    configured = {"backend_port": backend}
                elif missing == "backend_port":
                    self.make_mapping(root, 3000, backend)
                    configured = {"frontend_port": frontend}
                else:
                    self.make_mapping(root, frontend, backend)
                    configured = {}
                configured["custom_setting"] = "preserved"
                settings = self.settings_file()
                settings.parent.mkdir(parents=True, exist_ok=True)
                settings.write_text(json.dumps(configured), encoding="utf-8")
                with self.running(root, frontend, backend, use_cli_ports=False) as client:
                    self.assertEqual(
                        client.get(f"http://127.0.0.1:{backend}/api/health").json(),
                        {"status": "ok"},
                    )
                    self.assertIn("Portable frontend", client.get(f"http://127.0.0.1:{frontend}").text)
                self.assertEqual(json.loads(settings.read_text(encoding="utf-8")), {
                    "frontend_port": frontend, "backend_port": backend,
                    "custom_setting": "preserved",
                })

    def test_initialize_settings_needs_no_frontend_and_opens_no_ports(self) -> None:
        with tempfile.TemporaryDirectory() as directory, socket.socket() as frontend, socket.socket() as backend:
            root = Path(directory)
            frontend.bind(("127.0.0.1", 0))
            backend.bind(("127.0.0.1", 0))
            frontend.listen()
            backend.listen()
            expected = {
                "frontend_port": frontend.getsockname()[1],
                "backend_port": backend.getsockname()[1],
            }
            self.make_mapping(root, expected["frontend_port"], expected["backend_port"])
            completed = subprocess.run(
                self.command(root, "--initialize-settings"), cwd=root, env=self.environment(),
                capture_output=True, text=True, timeout=15,
                creationflags=subprocess.CREATE_NO_WINDOW if os.name == "nt" else 0,
            )
            self.assertEqual(completed.returncode, 0, completed.stderr)
            self.assertNotIn("Starting Instagram Clone", completed.stdout)
            self.assertEqual(json.loads(self.settings_file().read_text(encoding="utf-8")), expected)
            self.assertFalse((self.settings_root / "temp" / "instagram-clone").exists())
            self.assertFalse((root / "public").exists())

            # Initialization is idempotent and does not rewrite unrelated settings.
            preserved = json.dumps({**expected, "custom_setting": "preserved"})
            self.settings_file().write_text(preserved, encoding="utf-8")
            completed = subprocess.run(
                self.command(root, "--initialize-settings"), cwd=root, env=self.environment(),
                capture_output=True, text=True, timeout=15,
                creationflags=subprocess.CREATE_NO_WINDOW if os.name == "nt" else 0,
            )
            self.assertEqual(completed.returncode, 0, completed.stderr)
            self.assertEqual(self.settings_file().read_text(encoding="utf-8"), preserved)

    def test_invalid_or_missing_port_mapping_fails_before_service_startup(self) -> None:
        for content in (None, "{", "[]", '{"frontend_port": 3000}'):
            with self.subTest(content=content), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                mapping = root / "port_mapping.json"
                if content is not None:
                    mapping.write_text(content, encoding="utf-8")
                completed = subprocess.run(
                    self.command(root), cwd=root, env=self.environment(),
                    capture_output=True, text=True, timeout=15,
                    creationflags=subprocess.CREATE_NO_WINDOW if os.name == "nt" else 0,
                )
                self.assertEqual(completed.returncode, 1)
                self.assertIn(str(mapping), completed.stderr)
                self.assertNotIn("Frontend is missing", completed.stderr)
                self.assertNotIn("Traceback", completed.stderr)
                self.assertNotIn("Starting Instagram Clone", completed.stdout)

    def test_invalid_settings_report_clear_error_and_release_ports(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self.make_public(root)
            settings = self.settings_root / "appdata" / "instagram-clone" / "settings.json"
            settings.parent.mkdir(parents=True)
            settings.write_text("{", encoding="utf-8")
            frontend, backend = free_ports()
            completed = subprocess.run(
                self.command(root, "--frontend", str(frontend), "--backend", str(backend)),
                cwd=root, env=self.environment(), capture_output=True, text=True, timeout=15,
                creationflags=subprocess.CREATE_NO_WINDOW if os.name == "nt" else 0,
            )
            self.assertEqual(completed.returncode, 1)
            self.assertIn(str(settings), completed.stderr)
            self.assertNotIn("Traceback", completed.stderr)
            self.assertNotIn("Starting Instagram Clone", completed.stdout)
            self.assertEqual(settings.read_text(encoding="utf-8"), "{")
            for port in (frontend, backend):
                with socket.socket() as released:
                    released.bind(("127.0.0.1", port))

    def test_help_does_not_create_user_settings(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            completed = subprocess.run(
                self.command(Path(directory), "--help"), env=self.environment(),
                capture_output=True, text=True, timeout=15,
                creationflags=subprocess.CREATE_NO_WINDOW if os.name == "nt" else 0,
            )
            self.assertEqual(completed.returncode, 0)
            self.assertIn("usage:", completed.stdout)
            self.assertFalse((self.settings_root / "appdata").exists())

    def test_busy_frontend_port_fails_and_releases_backend_port(self) -> None:
        with tempfile.TemporaryDirectory() as directory, socket.socket() as busy:
            root = Path(directory)
            self.make_public(root)
            busy.bind(("127.0.0.1", 0))
            busy.listen()
            frontend = busy.getsockname()[1]
            _, backend = free_ports()
            completed = subprocess.run(
                self.command(root, "--frontend-port", str(frontend), "--backend", str(backend)),
                cwd=root, env=self.environment(), capture_output=True, text=True, timeout=15,
                creationflags=subprocess.CREATE_NO_WINDOW if os.name == "nt" else 0,
            )
            self.assertEqual(completed.returncode, 1)
            self.assertIn("Cannot bind frontend", completed.stderr)
            with socket.socket() as released:
                released.bind(("127.0.0.1", backend))

    def test_invalid_and_duplicate_ports_are_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            self.make_mapping(Path(directory))
            for arguments in (("--frontend", "0"), ("--backend", "65536"),
                              ("--backend", "abc"), ("--frontend", "8000")):
                with self.subTest(arguments=arguments):
                    completed = subprocess.run(
                        self.command(Path(directory), *arguments), env=self.environment(),
                        capture_output=True, text=True, timeout=15,
                        creationflags=subprocess.CREATE_NO_WINDOW if os.name == "nt" else 0,
                    )
                    self.assertEqual(completed.returncode, 2)
                    self.assertIn("error:", completed.stderr)

    def test_roles_and_backend_url_options(self) -> None:
        with patch.dict(os.environ, self.environment(), clear=True):
            options = packaged.parse_args([])
            self.assertEqual((options.role, options.backend_url), ("all", None))
            options = packaged.parse_args(["--role", "backend"])
            self.assertEqual((options.role, options.backend_url), ("backend", None))
            for arguments, expected in (
                (["--role", "frontend"], "http://127.0.0.1:8000"),
                (["--role", "frontend", "--host", "192.168.1.156", "--backend", "9100"],
                 "http://192.168.1.156:9100"),
                (["--role", "frontend", "--backend-url", "http://192.168.1.20:8317/"],
                 "http://192.168.1.20:8317"),
                (["--role", "frontend", "--backend-url", "https://api.example:443"],
                 "https://api.example:443"),
                # Another machine's backend may use the same port number as this frontend.
                (["--role", "frontend", "--frontend", "8317", "--backend", "8317",
                  "--backend-url", "http://192.168.1.20:8317"], "http://192.168.1.20:8317"),
            ):
                with self.subTest(arguments=arguments):
                    self.assertEqual(packaged.parse_args(arguments).backend_url, expected)
            backend_only = packaged.parse_args(["--role", "backend", "--frontend", "8000"])
            self.assertEqual((backend_only.frontend_port, backend_only.backend_port), (8000, 8000))
            for arguments in (
                ["--role", "web"],
                ["--backend-url", "http://192.168.1.20:8317"],
                ["--role", "backend", "--backend-url", "http://192.168.1.20:8317"],
                ["--role", "frontend", "--frontend", "8000"],
                *(["--role", "frontend", "--backend-url", url] for url in (
                    "192.168.1.20:8317", "ftp://host:21", "http://", "http://host:99999",
                    "http://host:0", "http://host:8317/api", "http://host:8317?x=1",
                    "http://user:secret@host:8317",
                )),
            ):
                with self.subTest(arguments=arguments), redirect_stderr(io.StringIO()), \
                        self.assertRaises(SystemExit) as raised:
                    packaged.parse_args(arguments)
                self.assertEqual(raised.exception.code, 2)

    def test_frontend_backend_url_from_settings_and_cli_precedence(self) -> None:
        mapping = self.make_mapping(self.settings_root, 4100, 4200)
        settings = self.settings_file()
        settings.parent.mkdir(parents=True)
        with patch.dict(os.environ, self.environment(), clear=True):
            settings.write_text(json.dumps({"backend_url": "http://192.168.1.20:8317/"}))
            self.assertEqual(
                packaged.parse_args(["--role", "frontend"], defaults_path=mapping).backend_url,
                "http://192.168.1.20:8317",
            )
            self.assertEqual(packaged.parse_args(
                ["--role", "frontend", "--backend-url", "http://10.0.0.5:9000"],
                defaults_path=mapping,
            ).backend_url, "http://10.0.0.5:9000")
            # Other roles never read it.
            self.assertIsNone(packaged.parse_args([], defaults_path=mapping).backend_url)
            for invalid in ("not a url", 8317, ""):
                with self.subTest(value=invalid):
                    settings.write_text(json.dumps({"backend_url": invalid}))
                    with self.assertRaises(packaged.RunError) as raised:
                        packaged.parse_args(["--role", "frontend"], defaults_path=mapping)
                    self.assertIn(str(settings), str(raised.exception))
                    self.assertIn("backend_url", str(raised.exception))

    def run_role(
        self, root: Path, arguments: list[str], *, host: str = "0.0.0.0",
        environment: dict[str, str] | None = None,
    ) -> tuple[str, Mock, Mock, Mock, Mock]:
        """main() with sockets, LAN discovery, the apps and serving mocked out."""
        addresses = ["192.168.1.156"]
        sockets = {}
        for label, port in (("backend", 8000), ("frontend", 3000)):
            listener = Mock(spec=socket.socket, family=socket.AF_INET)
            listener.getsockname.return_value = (host, port)
            sockets[label] = listener
        output = io.StringIO()
        with (
            patch.dict(os.environ, {**self.environment(), **(environment or {})}, clear=True),
            patch.object(packaged, "runtime_directory", return_value=root),
            patch.object(packaged, "bind_listener", side_effect=lambda h, p, label: sockets[label]) as bind,
            patch("backend.network.discover_lan_ipv4", return_value=addresses),
            patch("backend.app.main.create_app") as backend_app,
            patch.object(packaged, "create_frontend_app") as frontend_app,
            patch.object(packaged, "serve_applications", new=Mock(return_value=object())) as serve,
            patch.object(packaged.asyncio, "run", return_value=0),
            redirect_stdout(output),
        ):
            self.assertEqual(packaged.main(arguments), 0)
        for listener in sockets.values():
            self.assertLessEqual(listener.close.call_count, 1)
        return output.getvalue(), bind, backend_app, frontend_app, serve

    def test_backend_role_binds_only_the_backend_and_needs_no_frontend_files(self) -> None:
        root = self.settings_root / "Release"
        root.mkdir()
        self.make_mapping(root)
        output, bind, backend_app, frontend_app, serve = self.run_role(root, ["--role", "backend"])
        bind.assert_called_once_with("0.0.0.0", 8000, "backend")
        frontend_app.assert_not_called()
        cache = self.settings_root / "temp" / "instagram_cache"
        backend_app.assert_called_once_with(
            self.settings_root / "temp" / "instagram-clone" / "counter.db",
            cache_directory=cache, cors_origins=[],
        )
        (services,) = serve.call_args.args
        self.assertEqual([(label, application) for label, application, _ in services],
                         [("Backend", backend_app.return_value)])
        lines = output.splitlines()
        for line in (
            "Role:     backend",
            "Backend:  http://127.0.0.1:8000 (API docs: http://127.0.0.1:8000/docs)",
            "LAN API:  http://192.168.1.156:8000 (API docs: http://192.168.1.156:8000/docs)",
            "Start a frontend on another machine with: "
            "run-frontend.bat --backend-url http://192.168.1.156:8000",
            f"Cache:    {cache}",
        ):
            self.assertIn(line, lines)
        self.assertFalse(any(line.startswith(("Frontend:", "LAN: ")) for line in lines))

    def test_frontend_role_binds_only_the_frontend_and_opens_no_database(self) -> None:
        root = self.settings_root / "Release"
        root.mkdir()
        self.make_public(root)
        (root / ".env").unlink()
        output, bind, backend_app, frontend_app, serve = self.run_role(
            root, ["--role", "frontend", "--backend-url", "http://192.168.1.20:8317"],
        )
        bind.assert_called_once_with("0.0.0.0", 3000, "frontend")
        backend_app.assert_not_called()
        frontend_app.assert_called_once_with(root / "public", "http://192.168.1.20:8317")
        (services,) = serve.call_args.args
        self.assertEqual([label for label, _, _ in services], ["Frontend"])
        lines = output.splitlines()
        self.assertIn("Backend:  http://192.168.1.20:8317 (proxied at /api)", lines)
        self.assertIn("Frontend: http://127.0.0.1:3000", lines)
        self.assertIn("LAN:      http://192.168.1.156:3000", lines)
        self.assertFalse(any(line.startswith(("Database:", "Cache:", "LAN API:")) for line in lines))
        self.assertFalse((self.settings_root / "temp" / "instagram-clone").exists())

    def test_frontend_role_requires_the_built_frontend(self) -> None:
        root = self.settings_root / "Release"
        root.mkdir()
        self.make_mapping(root)
        with (
            patch.dict(os.environ, self.environment(), clear=True),
            patch.object(packaged, "runtime_directory", return_value=root),
            patch.object(packaged, "bind_listener") as bind,
            self.assertRaisesRegex(packaged.RunError, "Frontend is missing"),
        ):
            packaged.main(["--role", "frontend"])
        bind.assert_not_called()

    def test_all_role_passes_the_configured_cache_folder_and_prints_it(self) -> None:
        root = self.settings_root / "Debug"
        root.mkdir()
        self.make_public(root)
        cache = self.settings_root / "chosen cache"
        output, bind, backend_app, frontend_app, _serve = self.run_role(
            root, [], host="127.0.0.1", environment={"INSTAGRAM_CACHE_DIR": str(cache)},
        )
        self.assertEqual([call.args[2] for call in bind.call_args_list], ["backend", "frontend"])
        # The packaged UI is same-origin: the Vite dev origins are not trusted (CORS_ORIGINS is).
        self.assertEqual(backend_app.call_args.kwargs, {"cache_directory": cache, "cors_origins": []})
        frontend_app.assert_called_once_with(root / "public", "http://127.0.0.1:8000")
        self.assertIn(f"Cache:    {cache}", output.splitlines())
        self.assertIn("Role:     all", output.splitlines())

        _output, _bind, backend_app, _frontend_app, _serve = self.run_role(
            root, [], host="127.0.0.1",
            environment={"INSTAGRAM_CACHE_DIR": str(cache), "CORS_ORIGINS": "http://tool.local:9000"},
        )
        self.assertEqual(backend_app.call_args.kwargs["cors_origins"], ["http://tool.local:9000"])

    def test_backend_and_frontend_roles_split_across_processes_with_port_mapping(self) -> None:
        root = self.settings_root / "Release"
        root.mkdir()
        self.make_public(root)
        (root / ".env").unlink()
        (root / "runtime.json").write_text('{"host": "127.0.0.1"}', encoding="utf-8")
        frontend, backend = free_ports()
        self.make_mapping(root, frontend, backend)
        frontend_url = f"http://127.0.0.1:{frontend}"
        backend_url = f"http://127.0.0.1:{backend}"
        with self.serving(
            root, ["--role", "backend"], backend_url + "/api/health",
        ) as (client, backend_output):
            # Ports came from port_mapping.json through the new settings file.
            self.assertEqual(json.loads(self.settings_file().read_text(encoding="utf-8")),
                             {"frontend_port": frontend, "backend_port": backend})
            with self.assertRaises(httpx.ConnectError):
                client.get(frontend_url)
            with self.serving(
                root, ["--role", "frontend"], frontend_url + "/api/health",
            ) as (_, frontend_output):
                self.assertIn("Portable frontend", client.get(frontend_url).text)
                count = client.post(frontend_url + "/api/counter/click").json()["count"]
                self.assertEqual(client.get(backend_url + "/api/counter").json()["count"], count)
                self.assertIn(f"Backend:  {backend_url} (proxied at /api)", frontend_output())
            self.assertIn("Role:     backend", backend_output())
            self.assertIn(
                f"Cache:    {self.settings_root / 'temp' / 'instagram_cache'}", backend_output(),
            )

    def test_frontend_role_proxies_to_the_backend_url_option(self) -> None:
        root = self.settings_root / "Release"
        root.mkdir()
        self.make_public(root)
        (root / ".env").unlink()
        (root / "runtime.json").write_text('{"host": "127.0.0.1"}', encoding="utf-8")
        frontend, backend = free_ports()
        _, unused = free_ports()
        cache = self.settings_root / "library cache"
        backend_url = f"http://127.0.0.1:{backend}"
        with self.serving(
            root, ["--role", "backend", "--backend", str(backend), "--frontend", str(frontend)],
            backend_url + "/api/health", environment={"INSTAGRAM_CACHE_DIR": str(cache)},
        ) as (client, backend_output):
            # Nothing listens on the frontend's own backend port: only --backend-url works.
            with self.serving(
                root, ["--role", "frontend", "--frontend", str(frontend),
                       "--backend", str(unused), "--backend-url", backend_url + "/"],
                f"http://127.0.0.1:{frontend}/api/health",
            ):
                self.assertEqual(client.get(f"http://127.0.0.1:{frontend}/api/counter").json()["count"], 0)
                library = client.get(f"http://127.0.0.1:{frontend}/api/library")
                self.assertEqual(library.status_code, client.get(backend_url + "/api/library").status_code)
            self.assertIn(f"Cache:    {cache}", backend_output())

    def test_backend_startup_failure_stops_both_services(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self.make_public(root)
            (root / "blocked").write_text("A file cannot hold a database directory.")
            (root / ".env").write_text("COUNTER_DB_PATH=blocked/counter.db\n")
            frontend, backend = free_ports()
            completed = subprocess.run(
                self.command(root, "--frontend", str(frontend), "--backend", str(backend)),
                cwd=root, env=self.environment(), capture_output=True, text=True, timeout=15,
                creationflags=subprocess.CREATE_NO_WINDOW if os.name == "nt" else 0,
            )
            self.assertEqual(completed.returncode, 1)
            self.assertIn("Backend failed", completed.stderr)
            for port in (frontend, backend):
                with socket.socket() as released:
                    released.bind(("127.0.0.1", port))


class ConsoleTests(unittest.TestCase):
    def test_redirected_output_escapes_characters_the_code_page_cannot_encode(self) -> None:
        # A path with characters outside the ANSI code page (a CJK user or folder name) is
        # printed at startup; with output redirected to a file that must not stop the server.
        streams = [io.TextIOWrapper(io.BytesIO(), encoding="cp1252", errors="strict") for _ in range(2)]
        with patch.object(sys, "stdout", streams[0]), patch.object(sys, "stderr", streams[1]):
            packaged.configure_console()
            print("Settings: \u6e2c\u8a66 folder")
            print("[run] ERROR: \u0416", file=sys.stderr)
        for stream in streams:
            stream.flush()
        self.assertIn(b"Settings: \\u6e2c\\u8a66 folder", streams[0].buffer.getvalue())
        self.assertIn(b"\\u0416", streams[1].buffer.getvalue())

    def test_main_configures_the_console_before_printing_anything(self) -> None:
        with (
            patch.object(packaged, "configure_console") as configure,
            patch.object(packaged, "runtime_directory", side_effect=RuntimeError("stop here")),
            self.assertRaisesRegex(RuntimeError, "stop here"),
        ):
            packaged.main([])
        configure.assert_called_once_with()


class ServedApp:
    """Serve an ASGI application on a free loopback port from a background thread."""

    def __init__(self, application: Any) -> None:
        self.listener = packaged.bind_listener("127.0.0.1", 0, "test")
        self.url = packaged.listener_url(self.listener)
        self.server = packaged.ManagedServer(uvicorn.Config(
            application, loop="asyncio", http="h11", ws="none", lifespan="on",
            access_log=False, log_level="warning", timeout_graceful_shutdown=5,
        ))
        self.thread = threading.Thread(
            target=asyncio.run, args=(self.server.serve(sockets=[self.listener]),), daemon=True,
        )

    def __enter__(self) -> ServedApp:
        self.thread.start()
        deadline = time.monotonic() + 10
        while not self.server.started:
            if not self.thread.is_alive() or time.monotonic() > deadline:
                raise RuntimeError(f"Test server at {self.url} did not start")
            time.sleep(0.01)
        return self

    def __exit__(self, *exc_info: object) -> None:
        self.stop()

    def stop(self) -> None:
        """Stop after in-flight requests finish (idempotent)."""
        self.server.should_exit = True
        self.thread.join(timeout=10)
        self.listener.close()


class FakeBackend:
    """Backend endpoints that record what the proxy forwards and when."""

    def __init__(self, video: Path) -> None:
        self.upload_started = threading.Event()
        self.upload_aborted = threading.Event()
        self.client_has_first_chunk = threading.Event()
        self.stream_closed = threading.Event()
        self.stalled: list[str] = []
        self.chunks_sent = 0
        application = FastAPI()

        @application.put("/api/upload")
        async def upload(request: Request) -> dict[str, Any]:
            digest = hashlib.sha256()
            size = 0
            try:
                async for chunk in request.stream():
                    if chunk:
                        self.upload_started.set()
                    digest.update(chunk)
                    size += len(chunk)
            except ClientDisconnect:
                self.upload_aborted.set()
                return {}
            return {
                "bytes": size, "sha256": digest.hexdigest(),
                "content_length": request.headers.get("content-length"),
                "transfer_encoding": request.headers.get("transfer-encoding"),
            }

        @application.get("/api/stream")
        async def stream(chunks: int) -> StreamingResponse:
            async def body() -> AsyncIterator[bytes]:
                try:
                    yield b"first"
                    if not await asyncio.to_thread(self.client_has_first_chunk.wait, 5):
                        self.stalled.append("response")
                    for _ in range(chunks):
                        self.chunks_sent += 1
                        yield b"x" * 1024
                        await asyncio.sleep(0.025)
                finally:
                    self.stream_closed.set()

            return StreamingResponse(body(), media_type="application/octet-stream")

        @application.api_route("/api/video", methods=["GET", "HEAD"])
        def read_video() -> FileResponse:
            return FileResponse(video, media_type="video/mp4", filename="clip.mp4")

        @application.api_route("/api/echo/{rest:path}", methods=["GET", "POST"])
        async def echo(request: Request) -> dict[str, Any]:
            return {
                "raw_path": request.scope["raw_path"].decode("ascii"),
                "query": request.scope["query_string"].decode("ascii"),
                "headers": [[key, value] for key, value in request.headers.items()],
                "body": (await request.body()).decode("utf-8"),
            }

        self.application = application


class StreamingProxyTests(unittest.TestCase):
    def setUp(self) -> None:
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name)
        (self.root / "public").mkdir()
        (self.root / "public" / "index.html").write_text("<h1>Portable frontend</h1>")
        self.video = self.root / "clip.mp4"
        self.video.write_bytes(bytes(range(256)) * 64)
        self.backend = FakeBackend(self.video)
        self.backend_server = self.serve(self.backend.application)
        self.frontend = self.serve(
            packaged.create_frontend_app(self.root / "public", self.backend_server.url),
        )
        self.client = httpx.Client(base_url=self.frontend.url, trust_env=False, timeout=30)
        self.addCleanup(self.client.close)

    def serve(self, application: Any) -> ServedApp:
        served = ServedApp(application).__enter__()
        self.addCleanup(served.stop)
        return served

    def frontend_port(self) -> int:
        return int(self.frontend.url.rsplit(":", 1)[1])

    def test_large_upload_is_streamed_with_its_length_and_never_buffered(self) -> None:
        chunk = os.urandom(1024 * 1024)
        count = 48
        expected = hashlib.sha256(chunk * count).hexdigest()

        def body() -> Iterator[bytes]:
            yield chunk
            # A buffering proxy would wait for the whole body before contacting the backend.
            if not self.backend.upload_started.wait(5):
                self.backend.stalled.append("upload")
            for _ in range(count - 1):
                yield chunk

        tracemalloc.start()
        try:
            response = self.client.put(
                "/api/upload", content=body(), headers={"content-length": str(len(chunk) * count)},
            )
            _, peak = tracemalloc.get_traced_memory()
        finally:
            tracemalloc.stop()
        self.assertEqual(response.status_code, 200, response.text)
        self.assertEqual(response.json(), {
            "bytes": len(chunk) * count, "sha256": expected,
            "content_length": str(len(chunk) * count), "transfer_encoding": None,
        })
        self.assertEqual(self.backend.stalled, [])
        # Client, proxy and backend share this process: 48 MiB went through in far less memory.
        self.assertLess(peak, 16 * 1024 * 1024)

    def test_response_is_streamed_before_the_backend_finishes(self) -> None:
        received = b""
        with self.client.stream("GET", "/api/stream", params={"chunks": 3}) as response:
            for part in response.iter_raw():
                received += part
                if received.startswith(b"first"):
                    self.backend.client_has_first_chunk.set()
        self.assertEqual(received, b"first" + b"x" * 3 * 1024)
        self.assertEqual(self.backend.stalled, [])

    def test_closing_the_browser_connection_releases_the_backend_stream(self) -> None:
        with self.client.stream("GET", "/api/stream", params={"chunks": 400}) as response:
            self.backend.client_has_first_chunk.set()
            first = next(response.iter_raw())
            self.assertTrue(first.startswith(b"first"))
        self.assertTrue(self.backend.stream_closed.wait(5))
        self.assertLess(self.backend.chunks_sent, 400)

    def test_cancelled_upload_aborts_the_backend_request_without_an_error(self) -> None:
        with self.assertNoLogs("uvicorn.error", level="ERROR"):
            with socket.create_connection(("127.0.0.1", self.frontend_port())) as raw:
                raw.sendall(
                    b"PUT /api/upload HTTP/1.1\r\nHost: test\r\n"
                    b"Content-Length: 10000000\r\n\r\n" + b"x" * 65536
                )
                self.assertTrue(self.backend.upload_started.wait(5))
            self.assertTrue(self.backend.upload_aborted.wait(5))
            # Stopping waits for the proxy's request to finish, so any error is logged by now.
            self.frontend.stop()

    def test_range_requests_and_media_headers_pass_through(self) -> None:
        content = self.video.read_bytes()
        full = self.client.get("/api/video")
        self.assertEqual(full.content, content)
        etag = full.headers["etag"]
        partial = self.client.get("/api/video", headers={"range": "bytes=2-5", "if-range": etag})
        self.assertEqual(partial.status_code, 206)
        self.assertEqual(partial.content, content[2:6])
        self.assertEqual(partial.headers["content-range"], f"bytes 2-5/{len(content)}")
        self.assertEqual(partial.headers["content-length"], "4")
        for name in ("accept-ranges", "etag", "last-modified", "content-type", "content-disposition"):
            with self.subTest(header=name):
                self.assertEqual(partial.headers[name], full.headers[name])
        self.assertEqual(partial.headers["content-type"], "video/mp4")
        self.assertIn('filename="clip.mp4"', partial.headers["content-disposition"])
        self.assertEqual(len(partial.headers.get_list("date")), 1)
        self.assertEqual(len(partial.headers.get_list("server")), 1)
        # A stale If-Range validator reaches the backend, which then sends the whole file.
        stale = self.client.get("/api/video", headers={"range": "bytes=2-5", "if-range": '"old"'})
        self.assertEqual((stale.status_code, stale.content), (200, content))
        head = self.client.head("/api/video")
        self.assertEqual((head.status_code, head.headers["content-length"], head.content),
                         (200, str(len(content)), b""))

    def test_paths_queries_and_end_to_end_headers_are_forwarded_exactly(self) -> None:
        response = self.client.post(
            "/api/echo/posts/a%2Fb%20c.mp4?r=3&q=%26x", content=b"payload",
            headers={
                "connection": "keep-alive, x-hop", "x-hop": "secret",
                "keep-alive": "timeout=5", "x-custom": "kept", "accept-encoding": "identity",
            },
        )
        self.assertEqual(response.status_code, 200)
        echoed = response.json()
        self.assertEqual(echoed["raw_path"], "/api/echo/posts/a%2Fb%20c.mp4")
        self.assertEqual(echoed["query"], "r=3&q=%26x")
        self.assertEqual(echoed["body"], "payload")
        headers = dict(echoed["headers"])
        self.assertEqual(headers["x-custom"], "kept")
        self.assertEqual(headers["accept-encoding"], "identity")
        self.assertEqual(headers["content-length"], "7")
        self.assertNotIn("x-hop", headers)
        self.assertNotIn("keep-alive", headers)
        self.assertEqual(headers["host"], self.backend_server.url.removeprefix("http://"))

    def test_requests_without_a_browser_header_get_no_proxy_defaults(self) -> None:
        with socket.create_connection(("127.0.0.1", self.frontend_port())) as raw:
            raw.sendall(b"GET /api/echo/x HTTP/1.1\r\nHost: test\r\nConnection: close\r\n\r\n")
            reply = b""
            while chunk := raw.recv(65536):
                reply += chunk
        headers = dict(json.loads(reply.split(b"\r\n\r\n", 1)[1])["headers"])
        for name in ("accept", "accept-encoding", "user-agent", "transfer-encoding"):
            with self.subTest(header=name):
                self.assertNotIn(name, headers)

    def test_unreachable_backend_returns_json_502(self) -> None:
        _, closed = free_ports()
        frontend = self.serve(packaged.create_frontend_app(
            self.root / "public", f"http://127.0.0.1:{closed}",
        ))
        response = self.client.get(frontend.url + "/api/health")
        self.assertEqual(response.status_code, 502)
        self.assertEqual(response.json(), {"detail": "Backend is unavailable"})
        self.assertIn("Portable frontend", self.client.get(frontend.url + "/").text)

    def raw_request(self, request: bytes) -> tuple[int, bytes]:
        with socket.create_connection(("127.0.0.1", self.frontend_port())) as raw:
            raw.sendall(request)
            reply = b""
            while chunk := raw.recv(65536):
                reply += chunk
        head, _, body = reply.partition(b"\r\n\r\n")
        return int(head.split(b" ", 2)[1]), body

    def test_dot_segments_never_reach_another_backend_route(self) -> None:
        for path in (b"/api/../docs", b"/api/%2e%2e/docs", b"/api/echo/./x", b"/api/echo/%2E/x"):
            with self.subTest(path=path):
                status, body = self.raw_request(
                    b"GET " + path + b" HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n"
                )
                self.assertEqual(status, 400)
                self.assertEqual(json.loads(body), {"detail": "Invalid path"})

    def test_chunked_body_with_a_content_length_is_forwarded_chunked(self) -> None:
        # RFC 9112: Transfer-Encoding wins and the Content-Length must not be forwarded.
        data = b"x" * 38
        status, body = self.raw_request(
            b"POST /api/echo/x HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: 0\r\n"
            b"Transfer-Encoding: chunked\r\nConnection: close\r\n\r\n26\r\n" + data + b"\r\n0\r\n\r\n"
        )
        self.assertEqual(status, 200, body)
        echoed = json.loads(body)
        self.assertEqual(echoed["body"], data.decode("ascii"))
        headers = dict(echoed["headers"])
        self.assertNotIn("content-length", headers)
        self.assertEqual(headers["transfer-encoding"], "chunked")

    def test_proxy_replaces_origin_and_tells_the_backend_the_browser_address(self) -> None:
        response = self.client.post(
            "/api/echo/x", content=b"payload",
            headers={
                "origin": self.frontend.url, "sec-fetch-site": "same-origin",
            },
        )
        self.assertEqual(response.status_code, 200, response.text)
        headers = dict(response.json()["headers"])
        self.assertNotIn("origin", headers)
        self.assertNotIn("sec-fetch-site", headers)
        self.assertEqual(headers["x-forwarded-for"], "127.0.0.1")
        # A list the browser sends is never passed on: the proxy names one address itself.
        response = self.client.get("/api/echo/x", headers={"x-forwarded-for": "10.9.8.7, 10.0.0.1"})
        forwarded = [value for key, value in response.json()["headers"] if key == "x-forwarded-for"]
        self.assertEqual(len(forwarded), 1)
        self.assertNotIn(",", forwarded[0])

    def test_frontend_refuses_rebound_host_names_and_cross_site_writes(self) -> None:
        for path in (b"/", b"/api/echo/x"):
            with self.subTest(path=path):
                status, _ = self.raw_request(
                    b"GET " + path + b" HTTP/1.1\r\nHost: attacker.example:3000\r\nConnection: close\r\n\r\n"
                )
                self.assertEqual(status, 400)
        response = self.client.put(
            "/api/upload", content=b"x",
            headers={"origin": "http://evil.example", "sec-fetch-site": "cross-site"},
        )
        self.assertEqual(response.status_code, 403)
        self.assertFalse(self.backend.upload_started.is_set())
        self.assertEqual(self.client.get("/").status_code, 200)

    def test_forwarded_headers_drop_hop_by_hop_and_connection_headers(self) -> None:
        headers = httpx.Headers([
            ("Connection", "close, X-Hop"), ("X-Hop", "1"), ("Keep-Alive", "timeout=5"),
            ("Transfer-Encoding", "chunked"), ("Upgrade", "h2c"), ("Host", "frontend"),
            ("Date", "Mon, 01 Jan 2024 00:00:00 GMT"), ("Server", "uvicorn"),
            ("Range", "bytes=0-"), ("If-Range", '"abc"'), ("Content-Length", "10"),
            ("Content-Encoding", "gzip"), ("ETag", '"abc"'), ("Set-Cookie", "a=1"),
            ("Set-Cookie", "b=2"),
        ])
        self.assertEqual(packaged.forwarded_headers(headers), [
            (b"Range", b"bytes=0-"), (b"If-Range", b'"abc"'), (b"Content-Length", b"10"),
            (b"Content-Encoding", b"gzip"), (b"ETag", b'"abc"'), (b"Set-Cookie", b"a=1"),
            (b"Set-Cookie", b"b=2"),
        ])


if __name__ == "__main__":
    unittest.main()
