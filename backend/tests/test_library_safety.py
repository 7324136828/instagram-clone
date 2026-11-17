"""The library never takes over or deletes files it did not write, one process at a time uses a
folder, uploads respect the disk, and the API refuses DNS-rebound hosts and cross-site writes."""

from __future__ import annotations

import json
import os
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path
from typing import Any
from unittest.mock import patch

from fastapi.testclient import TestClient

from backend import settings
from backend.app import guard
from backend.app import library as library_module
from backend.app import main
from backend.app.library import Library, LibraryError, LibraryLimits

try:
    from .test_library import JPEG, MP4, PNG, LibraryApiTestCase, image_post
except ImportError:  # discovered with backend/tests as the top-level folder
    from test_library import JPEG, MP4, PNG, LibraryApiTestCase, image_post  # type: ignore[no-redef]

PROJECT_ROOT = Path(__file__).resolve().parents[2]


def tree(folder: Path) -> dict[str, bytes | None]:
    """Every file (with its bytes) and folder below `folder`."""
    return {
        path.relative_to(folder).as_posix(): (path.read_bytes() if path.is_file() else None)
        for path in sorted(folder.rglob("*"))
    }


def write_export(folder: Path) -> None:
    """A valid v1 export using spellings the library itself never writes (instagram.md 8)."""
    (folder / "posts").mkdir(parents=True)
    (folder / "avatars").mkdir()
    for name in ("post1_1.jpg", "post1_2.jpg", "post2_1.mp4", "post2_1_poster.jpg", "post3_1.jpg"):
        (folder / "posts" / name).write_bytes(JPEG if name.endswith(".jpg") else MP4)
    (folder / "avatars" / "someone.png").write_bytes(PNG)
    (folder / "post.json").write_text(
        json.dumps({
            "format": "instagram-export",
            "version": 1,
            "posts": [
                {"id": "1", "media": ["posts/post1_1.jpg", "posts/post1_2.jpg"]},
                {"id": "2", "video": "posts/post2_1.mp4", "thumbnail": "posts/post2_1_poster.jpg"},
                {"id": "3", "media": [{"path": "posts/post3_1.jpg"}], "user": {"username": "someone", "avatar": "avatars/someone.png"}},
            ],
        }),
        encoding="utf-8",
    )


class FolderOwnershipTests(LibraryApiTestCase):
    def assert_refused(self, client: TestClient, folder: Path) -> None:
        for method, url in (
            ("GET", "/api/library"),
            ("POST", "/api/library/uploads"),
            ("DELETE", "/api/library"),
            ("DELETE", "/api/library/posts/1"),
            ("GET", "/api/library/files/posts/post1_1.jpg"),
        ):
            with self.subTest(method=method, url=url):
                response = client.request(method, url)
                self.assertEqual(response.status_code, 503, response.text)
                self.assertIn("did not create", response.json()["detail"])

    def test_an_existing_export_is_refused_and_never_touched(self) -> None:
        folder = self.root / "my-export"
        write_export(folder)
        before = tree(folder)
        client = self.start(cache=folder)
        self.assert_refused(client, folder)
        self.stop(client)
        self.assertEqual(tree(folder), before, "no file was deleted, moved or added")

    def test_a_folder_with_other_files_is_refused_and_never_touched(self) -> None:
        layouts: dict[str, dict[str, bytes]] = {
            "photos and notes": {"posts/vacation_12.jpg": JPEG, "posts/IMG1234.jpg": JPEG, "avatars/mom.png": PNG, "notes.txt": b"mine"},
            "only an avatar": {"avatars/mom.png": PNG},
            "home-like folder": {"Documents/letter.txt": b"x", "desktop.ini": b""},
            "foreign post.json": {"post.json": b'{"posts": []}'},
            "damaged post.json": {"post.json": b"{broken"},
        }
        for label, files in layouts.items():
            with self.subTest(layout=label):
                folder = self.root / label
                for name, content in files.items():
                    (folder / name).parent.mkdir(parents=True, exist_ok=True)
                    (folder / name).write_bytes(content)
                before = tree(folder)
                client = self.start(cache=folder)
                self.assertEqual(client.get("/api/library").status_code, 503)
                self.assertEqual(client.delete("/api/library").status_code, 503)
                self.stop(client)
                self.assertEqual(tree(folder), before)

    def test_empty_folders_and_library_leftovers_are_adopted(self) -> None:
        empty = self.root / "empty"
        empty.mkdir()
        leftovers = self.root / "leftovers"
        (leftovers / "posts").mkdir(parents=True)
        (leftovers / ".staging" / ("ab" * 16)).mkdir(parents=True)
        (leftovers / ".post.json-x.tmp").write_text("{", encoding="utf-8")
        for folder in (empty, leftovers):
            with self.subTest(folder=folder.name):
                client = self.start(cache=folder)
                self.assertEqual(client.get("/api/library").status_code, 200)
                self.assertTrue((folder / ".instagram-library").is_file())
                self.stop(client)

    def test_a_library_from_the_previous_version_without_a_marker_is_adopted(self) -> None:
        body = self.import_posts({"m000001.jpg": JPEG}, [image_post("a", "m000001.jpg")])
        self.stop(self.client)
        (self.cache / ".instagram-library").unlink()
        self.client = self.start()
        self.assertEqual(self.library(), body)
        self.assertTrue((self.cache / ".instagram-library").is_file())

    def test_sweep_keeps_files_referenced_in_any_v1_spelling(self) -> None:
        self.import_posts({"m000001.jpg": JPEG}, [image_post("a", "m000001.jpg")])
        self.stop(self.client)
        document = json.loads((self.cache / "post.json").read_text(encoding="utf-8"))
        document["posts"] += [
            {"id": "s", "media": ["posts/s_1.jpg", "./posts/s_2.jpg"]},
            {"id": "v", "video": "posts/v_1.mp4", "thumbnail": "posts/v_1_poster.jpg"},
            {"id": "w", "videos": ["posts/w_1.mp4"], "cover": "posts/w_1_poster.jpg"},
            {"id": "f", "media": [{"file": "posts/f_1.jpg"}, {"src": "posts/f_2.mp4", "thumbnail": "posts/f_2_poster.jpg"}]},
        ]
        (self.cache / "post.json").write_text(json.dumps(document), encoding="utf-8")
        referenced = [
            "s_1.jpg", "s_2.jpg", "v_1.mp4", "v_1_poster.jpg", "w_1.mp4", "w_1_poster.jpg",
            "f_1.jpg", "f_2.mp4", "f_2_poster.jpg",
        ]
        for name in [*referenced, "orphan_1.jpg"]:
            (self.cache / "posts" / name).write_bytes(JPEG)
        self.client = self.start()
        self.assertEqual(len(self.library()["posts"]), 5)
        self.assertEqual(sorted(os.listdir(self.cache / "posts")), sorted(["a_1.jpg", *referenced]))

    def test_clear_deletes_only_files_the_library_wrote(self) -> None:
        self.import_posts({"m000001.jpg": JPEG, "m000002.png": PNG}, [image_post("a", "m000001.jpg")], avatars={"someone": "m000002.png"})
        (self.cache / "posts" / "IMG1234.jpg").write_bytes(JPEG)
        (self.cache / "posts" / "sub").mkdir()
        (self.cache / "posts" / "sub" / "doc.txt").write_text("mine", encoding="utf-8")
        (self.cache / "avatars" / "me.heic").write_bytes(b"heic")
        (self.cache / ".staging" / "keep.txt").write_text("mine", encoding="utf-8")
        self.assertEqual(self.client.delete("/api/library").status_code, 204)
        self.assertEqual(
            sorted(tree(self.cache)),
            [".instagram-library", ".staging", ".staging/keep.txt", "avatars", "avatars/me.heic",
             "posts", "posts/IMG1234.jpg", "posts/sub", "posts/sub/doc.txt"],
        )
        self.assertEqual(self.library()["posts"], [])


class SingleProcessTests(LibraryApiTestCase):
    def test_a_second_library_on_the_same_folder_waits_for_the_first_to_close(self) -> None:
        self.import_posts({"m000001.jpg": JPEG}, [image_post("a", "m000001.jpg")])
        second = self.start()
        response = second.get("/api/library")
        self.assertEqual(response.status_code, 503)
        self.assertIn("in use by another running copy", response.json()["detail"])
        self.assertEqual(second.post("/api/library/uploads").status_code, 503)
        self.stop(self.client)
        self.assertEqual([post["id"] for post in second.get("/api/library").json()["posts"]], ["a"])

    def test_another_process_using_the_folder_keeps_this_one_from_changing_it(self) -> None:
        self.import_posts({"m000001.jpg": JPEG}, [image_post("a", "m000001.jpg")])
        self.stop(self.client)
        script = (
            "import sys\n"
            "from pathlib import Path\n"
            "from backend.app.library import Library\n"
            "library = Library(lambda: Path(sys.argv[1]))\n"
            "print(len(library.snapshot()['posts']), flush=True)\n"
            "sys.stdin.read()\n"
        )
        other = subprocess.Popen(
            [sys.executable, "-c", script, str(self.cache)], cwd=PROJECT_ROOT,
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True,
        )
        self.addCleanup(other.kill)
        self.addCleanup(lambda: other.stdout and other.stdout.close())
        assert other.stdout is not None and other.stdin is not None
        self.assertEqual(other.stdout.readline().strip(), "1")

        self.client = self.start()
        for method, url in (("GET", "/api/library"), ("POST", "/api/library/uploads"), ("DELETE", "/api/library")):
            with self.subTest(method=method, url=url):
                self.assertEqual(self.client.request(method, url).status_code, 503)
        self.assertTrue((self.cache / "posts" / "a_1.jpg").is_file())

        other.stdin.close()
        other.wait(timeout=30)
        # The lock goes with the process; this app takes the folder over on its next request.
        self.assertEqual([post["id"] for post in self.library()["posts"]], ["a"])


class UploadResourceTests(LibraryApiTestCase):
    def test_uploads_stop_before_the_drive_runs_out_of_space(self) -> None:
        upload_id = self.begin()
        with patch.object(library_module, "_free_bytes", return_value=100 * 1024 * 1024):
            response = self.put(upload_id, "m000001.jpg", JPEG)
        self.assertEqual(response.status_code, 507, response.text)
        self.assertIn("free disk space", response.json()["detail"])
        self.assertEqual(os.listdir(self.cache / ".staging" / upload_id), [])

        # A long upload is checked again while it streams.
        readings = iter([10 * 1024**3, 100 * 1024 * 1024])
        with (
            patch.object(library_module, "_free_bytes", side_effect=lambda _path: next(readings)),
            patch.object(library_module, "WRITE_BUFFER_BYTES", 1024),
            patch.object(library_module, "FREE_SPACE_CHECK_BYTES", 2048),
        ):
            chunks = iter([JPEG] + [b"\x00" * 1024] * 8)
            response = self.put(upload_id, "m000001.jpg", chunks)
        self.assertEqual(response.status_code, 507, response.text)
        self.assertEqual(os.listdir(self.cache / ".staging" / upload_id), [])
        self.assertEqual(self.put(upload_id, "m000001.jpg", JPEG).status_code, 200)

    def test_open_sessions_are_capped_by_dropping_the_longest_idle_one(self) -> None:
        self.client = self.restart(LibraryLimits(max_open_sessions=2))
        first, second = self.begin(), self.begin()
        self.assertEqual(self.put(second, "m000001.jpg", JPEG).status_code, 200)
        third = self.begin()
        self.assertEqual(self.put(first, "m000001.jpg", JPEG).status_code, 404)
        self.assertFalse((self.cache / ".staging" / first).exists())
        for upload_id in (second, third):
            self.assertEqual(self.put(upload_id, "m000002.jpg", JPEG).status_code, 200)

    def test_busy_sessions_are_never_dropped(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            library = Library(lambda: Path(directory) / "cache", LibraryLimits(max_open_sessions=1))
            try:
                busy = library.create_upload()
                library._start_write(busy, "m000001.jpg")
                with self.assertRaises(LibraryError) as refused:
                    library.create_upload()
                self.assertEqual(refused.exception.status_code, 429)
                library._end_write(library._sessions[busy], "m000001.jpg", 0)
                library.create_upload()
                self.assertNotIn(busy, library._sessions)
            finally:
                library.close()

    def test_idle_sessions_expire(self) -> None:
        self.client = self.restart(LibraryLimits(stale_staging_seconds=0.05))
        idle = self.begin()
        time.sleep(0.1)
        self.begin()
        self.assertEqual(self.put(idle, "m000001.jpg", JPEG).status_code, 404)
        self.assertFalse((self.cache / ".staging" / idle).exists())


class RequestGuardTests(LibraryApiTestCase):
    def test_dns_rebound_host_names_are_refused(self) -> None:
        self.import_posts({"m000001.jpg": JPEG}, [image_post("secret", "m000001.jpg")])
        for host in ("attacker.example:8000", "attacker.test", "rebind.127.0.0.1.nip.io:3000"):
            with self.subTest(host=host):
                for method, url in (("GET", "/api/library"), ("DELETE", "/api/library"), ("GET", "/api/library/files/posts/secret_1.jpg")):
                    response = self.client.request(method, url, headers={"Host": host})
                    self.assertEqual(response.status_code, 400, (method, url))
        self.assertEqual([post["id"] for post in self.library()["posts"]], ["secret"])

    def test_addresses_local_names_and_configured_hosts_are_accepted(self) -> None:
        with patch.object(guard, "machine_names", return_value=frozenset({"mypc.corp.example"})):
            for host in (
                "127.0.0.1:8000", "localhost:8000", "LOCALHOST.", "[::1]:8000", "192.168.1.20:8317",
                "[fe80::1%25eth0]:80", "mypc:8317", "app.localhost:3000", "mypc.corp.example:8317",
            ):
                with self.subTest(host=host):
                    self.assertEqual(self.client.get("/api/health", headers={"Host": host}).status_code, 200)

        client = TestClient(main.create_app(self.root / "counter.db", self.cache, allowed_hosts=["photos.example.lan"]))
        self.assertEqual(client.get("/api/health", headers={"Host": "photos.example.lan:80"}).status_code, 200)
        self.assertEqual(client.get("/api/health", headers={"Host": "other.example.lan"}).status_code, 400)

    def test_allowed_hosts_come_from_the_environment_and_settings(self) -> None:
        environment = patch.dict(os.environ, {"APPDATA": str(self.root / "roaming"), "XDG_CONFIG_HOME": str(self.root / "config"), "ALLOWED_HOSTS": "a.example, b.example"})
        environment.start()
        self.addCleanup(environment.stop)
        path = settings.settings_path()
        path.parent.mkdir(parents=True)
        path.write_text('{"allowed_hosts": ["c.example"]}', encoding="utf-8")
        self.assertEqual(settings.configured_allowed_hosts(), ["a.example", "b.example", "c.example"])
        client = TestClient(main.create_app(self.root / "counter.db", self.cache))
        for host, status in (("a.example", 200), ("c.example", 200), ("d.example", 400)):
            with self.subTest(host=host):
                self.assertEqual(client.get("/api/health", headers={"Host": host}).status_code, status)
        path.write_text('{"allowed_hosts": 5}', encoding="utf-8")
        with self.assertRaises(settings.SettingsError):
            settings.configured_allowed_hosts()
        broken = TestClient(main.create_app(self.root / "counter.db", self.cache))
        self.assertEqual(broken.get("/api/health", headers={"Host": "d.example"}).status_code, 400)
        self.assertEqual(broken.get("/api/health", headers={"Host": "127.0.0.1"}).status_code, 200)

    def test_cross_site_writes_are_refused_before_anything_happens(self) -> None:
        self.import_posts({"m000001.jpg": JPEG}, [image_post("a", "m000001.jpg")])
        staged_before = sorted(os.listdir(self.cache / ".staging"))
        cross_site = [
            {"Origin": "http://evil.example", "Sec-Fetch-Site": "cross-site"},
            {"Origin": "http://evil.example"},  # a browser without Fetch Metadata
            {"Origin": "null", "Sec-Fetch-Site": "cross-site"},  # sandboxed frame
            {"Origin": "http://localhost:5199", "Sec-Fetch-Site": "same-site"},  # some other local dev server
            {"Sec-Fetch-Site": "cross-site"},
        ]
        for headers in cross_site:
            for method, url in (
                ("POST", "/api/library/uploads"),  # what a no-cors fetch or sendBeacon can send
                ("DELETE", "/api/library"),
                ("DELETE", "/api/library/posts/a"),
                ("PATCH", "/api/library/posts/a"),
                ("POST", "/api/counter/click"),
            ):
                with self.subTest(headers=headers, method=method, url=url):
                    response = self.client.request(method, url, headers=headers, content=b"x")
                    self.assertEqual(response.status_code, 403)
        self.assertEqual(sorted(os.listdir(self.cache / ".staging")), staged_before)
        self.assertEqual([post["id"] for post in self.library()["posts"]], ["a"])
        # Reading stays possible; CORS decides what another site may see of the answer.
        self.assertEqual(self.client.get("/api/library", headers=cross_site[0]).status_code, 200)

    def test_the_apps_own_pages_and_other_clients_can_write(self) -> None:
        allowed = [
            {},  # curl, scripts, Playwright's request API
            {"Sec-Fetch-Site": "same-origin", "Origin": "http://localhost:5173"},  # through Vite's proxy
            {"Sec-Fetch-Site": "none"},
            {"Origin": "http://testserver"},  # same origin, no Fetch Metadata
            {"Origin": "http://localhost:5173", "Sec-Fetch-Site": "same-site"},  # a trusted CORS origin
        ]
        client = TestClient(main.create_app(self.root / "counter.db", self.cache, cors_origins=["http://localhost:5173"]))
        with client:
            for headers in allowed:
                with self.subTest(headers=headers):
                    response = client.post("/api/library/uploads", headers=headers)
                    self.assertEqual(response.status_code, 201, response.text)

    def test_other_computers_are_not_told_the_folders_full_path(self) -> None:
        self.stop(self.client)
        application = main.create_app(self.root / "counter.db", self.cache)
        hidden = f"{self.cache.name} on the computer running the backend"
        for peer, headers, expected in (
            (("192.168.1.20", 50000), {}, hidden),
            (("127.0.0.1", 50000), {"X-Forwarded-For": "192.168.1.20"}, hidden),  # via the packaged proxy
            (("192.168.1.20", 50000), {"X-Forwarded-For": "127.0.0.1"}, hidden),  # spoofed
            (("127.0.0.1", 50000), {}, str(self.cache)),
            (("::1", 50000), {"X-Forwarded-For": "127.0.0.1"}, str(self.cache)),
        ):
            with self.subTest(peer=peer, headers=headers), TestClient(application, client=peer) as client:
                self.assertEqual(client.get("/api/library", headers=headers).json()["directory"], expected)
        with TestClient(application, client=("192.168.1.20", 50000)) as client:
            upload_id = client.post("/api/library/uploads").json()["upload_id"]
            client.put(f"/api/library/uploads/{upload_id}/files/m000001.jpg", content=JPEG)
            body = {"source_name": "x.zip", "source_kind": "archive", "posts": [image_post("a", "m000001.jpg")]}
            committed = client.post(f"/api/library/uploads/{upload_id}/commit", json=body).json()
            self.assertEqual(committed["directory"], hidden)
            self.assertEqual(client.delete("/api/library/posts/a").json()["directory"], hidden)


class GuardHelperTests(unittest.TestCase):
    def test_host_names(self) -> None:
        self.assertEqual(guard.host_name("[::1]:8000"), "::1")
        self.assertEqual(guard.host_name("Example.COM.:80"), "example.com")
        self.assertEqual(guard.host_name("localhost"), "localhost")
        self.assertIsNone(guard.host_name("[::1"))
        self.assertFalse(guard.host_allowed(None))
        self.assertTrue(guard.host_allowed("anything.example", ["*"]))


if __name__ == "__main__":
    unittest.main()
