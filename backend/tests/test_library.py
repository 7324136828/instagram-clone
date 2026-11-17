from __future__ import annotations

import json
import os
import tempfile
import threading
import time
import unittest
from collections.abc import AsyncIterator, Iterator
from pathlib import Path
from typing import Any
from unittest.mock import patch

import anyio
from fastapi.testclient import TestClient

from backend import settings
from backend.app import library as library_module
from backend.app import main
from backend.app.library import (
    Library,
    LibraryError,
    LibraryLimits,
    parse_timestamp,
    is_instagram_url,
    safe_instagram_url,
    safe_web_url,
    sniff_media,
    sort_posts,
)

JPEG = b"\xff\xd8\xff\xe0\x00\x10JFIF\x00" + bytes(range(256)) * 2
PNG = b"\x89PNG\r\n\x1a\n" + b"\x00" * 200
GIF = b"GIF89a" + b"\x00" * 50
WEBP = b"RIFF\x00\x00\x00\x00WEBPVP8 " + b"\x00" * 50
BMP = b"BM" + b"\x00" * 4 + b"\x00\x00\x00\x00" + b"\x36\x00\x00\x00" + b"\x28\x00\x00\x00" + b"\x00" * 40


def ftyp(major: bytes, *compatible: bytes, tail: int = 300) -> bytes:
    box = major + b"\x00\x00\x02\x00" + b"".join(compatible)
    return (8 + len(box)).to_bytes(4, "big") + b"ftyp" + box + b"\x00" * tail


def ebml(doctype: bytes, tail: int = 300) -> bytes:
    children = (
        b"\x42\x86\x81\x01"  # EBMLVersion 1
        + b"\x42\xf7\x81\x01"  # EBMLReadVersion 1
        + b"\x42\x82" + bytes([0x80 | len(doctype)]) + doctype
        + b"\x42\x87\x81\x04"  # DocTypeVersion 4
    )
    return b"\x1a\x45\xdf\xa3" + bytes([0x80 | len(children)]) + children + b"\x00" * tail


MP4 = ftyp(b"isom", b"isom", b"iso2", b"mp41")
MOV = ftyp(b"qt  ", b"qt  ")
WEBM = ebml(b"webm")
AVIF = ftyp(b"mif1", b"mif1", b"avif")
HEIC = ftyp(b"heic", b"mif1", b"heic")
HEIF = ftyp(b"mif1", b"mif1")
MATROSKA = ebml(b"matroska")
SVG = b'<?xml version="1.0"?><svg xmlns="http://www.w3.org/2000/svg"></svg>'
HTML = b"<!doctype html><html><script>alert(1)</script></html>"


def image_post(post_id: str, path: str, **fields: Any) -> dict[str, Any]:
    return {"id": post_id, "media": [{"type": "image", "path": path}], "user": "someone", **fields}


class LibraryApiTestCase(unittest.TestCase):
    def setUp(self) -> None:
        temporary = tempfile.TemporaryDirectory(prefix="library-test-")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name).resolve()
        self.cache = self.root / "cache"
        self.client = self.start()

    def start(
        self, limits: LibraryLimits | None = None, cache: Path | None = None
    ) -> TestClient:
        application = main.create_app(
            self.root / "counter.db",
            cache if cache is not None else self.cache,
            library_limits=limits,
        )
        client = TestClient(application)
        client.__enter__()
        self.addCleanup(self.stop, client)
        return client

    def stop(self, client: TestClient) -> None:
        """Shuts an app down (its lifespan unlocks the library folder); idempotent."""
        if not getattr(client, "stopped", False):
            client.stopped = True  # type: ignore[attr-defined]
            client.__exit__(None, None, None)

    def restart(self, limits: LibraryLimits | None = None) -> TestClient:
        self.stop(self.client)
        self.client = self.start(limits)
        return self.client

    def begin(self, client: TestClient | None = None) -> str:
        response = (client or self.client).post("/api/library/uploads")
        self.assertEqual(response.status_code, 201, response.text)
        upload_id: str = response.json()["upload_id"]
        return upload_id

    def put(
        self, upload_id: str, name: str, content: bytes | Iterator[bytes], client: TestClient | None = None
    ) -> Any:
        return (client or self.client).put(
            f"/api/library/uploads/{upload_id}/files/{name}", content=content
        )

    def upload(self, files: dict[str, bytes], client: TestClient | None = None) -> str:
        upload_id = self.begin(client)
        for name, content in files.items():
            response = self.put(upload_id, name, content, client)
            self.assertEqual(response.status_code, 200, response.text)
        return upload_id

    def commit(
        self, upload_id: str, posts: list[Any], client: TestClient | None = None, **extra: Any
    ) -> Any:
        body = {"source_name": "export.zip", "source_kind": "archive", "posts": posts, **extra}
        # json.dumps escapes lone surrogates, which httpx's json= cannot encode.
        return (client or self.client).post(
            f"/api/library/uploads/{upload_id}/commit",
            content=json.dumps(body),
            headers={"Content-Type": "application/json"},
        )

    def import_posts(self, files: dict[str, bytes], posts: list[Any], **extra: Any) -> dict[str, Any]:
        response = self.commit(self.upload(files), posts, **extra)
        self.assertEqual(response.status_code, 200, response.text)
        body: dict[str, Any] = response.json()
        return body

    def library(self) -> dict[str, Any]:
        response = self.client.get("/api/library")
        self.assertEqual(response.status_code, 200, response.text)
        body: dict[str, Any] = response.json()
        return body

    def staged_entries(self) -> list[Path]:
        staging = self.cache / ".staging"
        return sorted(staging.rglob("*")) if staging.exists() else []


class LibraryReadTests(LibraryApiTestCase):
    def test_empty_library_creates_the_folder_lazily(self) -> None:
        self.assertFalse(self.cache.exists())
        self.assertEqual(
            self.library(),
            {"directory": str(self.cache), "revision": 0, "updated_at": None, "imports": [], "posts": []},
        )
        self.assertTrue(self.cache.is_dir())
        self.assertFalse((self.cache / "post.json").exists())

    def test_cors_preflight_allows_put(self) -> None:
        response = self.client.options(
            "/api/library/uploads/x/files/m000001.jpg",
            headers={"Origin": "http://localhost:5173", "Access-Control-Request-Method": "PUT"},
        )
        self.assertEqual(response.status_code, 200)
        self.assertIn("PUT", response.headers["access-control-allow-methods"])


class LibraryImportTests(LibraryApiTestCase):
    def test_commit_writes_a_v1_export_with_images_videos_posters_and_avatars(self) -> None:
        upload_id = self.upload(
            {
                "m000001.jpg": JPEG,
                "m000002.mp4": MP4,
                "m000003.jpg": JPEG,
                "m000004.png": PNG,
                "m000005.webm": WEBM,
                "m000006.webp": WEBP,
            }
        )
        response = self.commit(
            upload_id,
            [
                image_post(
                    "C9aKq2LtXy1",
                    "m000001.jpg",
                    url="https://www.instagram.com/p/C9aKq2LtXy1/",
                    user={"username": "Lumen.Atlas", "full_name": "Lumen", "verified": True},
                    caption="Two mornings",
                    comment_count=1234,
                    like_count=56789,
                    taken_at="2026-11-01T15:04:05Z",
                    alt="Photo",
                    location="Chamonix",
                    comments=[{"username": "a", "text": "Nice", "like_count": 3}],
                ),
                {
                    "id": "C5xYz",
                    "url": "https://www.instagram.com/reel/C5xYz/",
                    "media_type": "video",
                    "media": [{"type": "video", "path": "m000002.mp4", "poster": "m000003.jpg"}],
                    "user": "someone",
                    "taken_at": "2026-10-01T12:00:00Z",
                },
                {
                    "id": "mixed",
                    "media": ["m000005.webm", {"path": "m000006.webp", "alt": "slide"}],
                    "user": "someone",
                },
            ],
            avatars={"lumen.atlas": "m000004.png"},
            exported_at="2026-11-10T09:30:00Z",
            source_url="https://www.instagram.com/lumen.atlas/",
        )
        self.assertEqual(response.status_code, 200, response.text)
        body = response.json()

        self.assertEqual(body["revision"], 1)
        self.assertEqual(body["directory"], str(self.cache))
        self.assertEqual([post["id"] for post in body["posts"]], ["C9aKq2LtXy1", "C5xYz", "mixed"])
        image, video, mixed = body["posts"]
        self.assertEqual(
            image["media"],
            [{"type": "image", "path": "posts/C9aKq2LtXy1_1.jpg", "mime": "image/jpeg", "bytes": len(JPEG)}],
        )
        self.assertEqual(image["image"], "posts/C9aKq2LtXy1_1.jpg")
        self.assertEqual(image["images"], ["posts/C9aKq2LtXy1_1.jpg"])
        self.assertEqual(
            image["user"],
            {
                "username": "Lumen.Atlas",
                "full_name": "Lumen",
                "verified": True,
                "profile_url": "https://www.instagram.com/Lumen.Atlas/",
                "avatar": "avatars/lumen_atlas.png",
            },
        )
        self.assertEqual(image["media_type"], "image")
        self.assertEqual(image["caption"], "Two mornings")
        self.assertEqual((image["comment_count"], image["like_count"]), (1234, 56789))
        self.assertEqual(image["taken_at"], "2026-11-01T15:04:05.000Z")
        self.assertEqual((image["alt"], image["location"]), ("Photo", "Chamonix"))
        self.assertEqual(image["comments"], [{"username": "a", "text": "Nice", "like_count": 3}])

        self.assertEqual(video["media_type"], "video")
        self.assertEqual(
            video["media"],
            [
                {
                    "type": "video",
                    "path": "posts/C5xYz_1.mp4",
                    "poster": "posts/C5xYz_1_poster.jpg",
                    "mime": "video/mp4",
                    "bytes": len(MP4),
                }
            ],
        )
        self.assertEqual(video["images"], ["posts/C5xYz_1_poster.jpg"])
        self.assertEqual(video["url"], "https://www.instagram.com/reel/C5xYz/")

        self.assertEqual(mixed["media_type"], "carousel")
        self.assertEqual(
            [(item["type"], item["path"], item.get("alt")) for item in mixed["media"]],
            [("video", "posts/mixed_1.webm", None), ("image", "posts/mixed_2.webp", "slide")],
        )
        self.assertEqual(mixed["image"], "posts/mixed_2.webp")

        self.assertEqual(len(body["imports"]), 1)
        record = body["imports"][0]
        self.assertEqual(record["id"], upload_id)
        self.assertEqual(
            {key: record[key] for key in ("source_name", "source_kind", "posts", "new", "updated")},
            {"source_name": "export.zip", "source_kind": "archive", "posts": 3, "new": 3, "updated": 0},
        )
        self.assertEqual(record["exported_at"], "2026-11-10T09:30:00.000Z")
        self.assertEqual(record["source_url"], "https://www.instagram.com/lumen.atlas/")

        manifest = json.loads((self.cache / "post.json").read_text(encoding="utf-8"))
        self.assertEqual(manifest["format"], "instagram-export")
        self.assertEqual(manifest["version"], 1)
        self.assertEqual(manifest["source"], {"type": "library"})
        self.assertEqual(manifest["library"]["revision"], 1)
        self.assertEqual(manifest["posts"], body["posts"])
        for post in manifest["posts"]:
            for path in library_module._library_paths(post):
                self.assertTrue((self.cache / path).is_file(), path)
        self.assertEqual((self.cache / "posts" / "C5xYz_1.mp4").read_bytes(), MP4)
        self.assertEqual(self.staged_entries(), [])
        self.assertEqual(self.library(), body)

    def test_legacy_image_and_video_fields_are_accepted(self) -> None:
        body = self.import_posts(
            {"m000001.jpg": JPEG, "m000002.png": PNG, "m000003.mov": MOV, "m000004.gif": GIF},
            [
                {"id": "a", "image": "m000002.png", "images": ["m000001.jpg"], "user": "x"},
                {"id": "b", "video": "m000003.mov", "poster": "m000004.gif", "user": "x"},
            ],
        )
        first, second = body["posts"]
        self.assertEqual([item["path"] for item in first["media"]], ["posts/a_1.png", "posts/a_2.jpg"])
        self.assertEqual(
            second["media"][0],
            {
                "type": "video",
                "path": "posts/b_1.mov",
                "poster": "posts/b_1_poster.gif",
                "mime": "video/quicktime",
                "bytes": len(MOV),
            },
        )
        self.assertEqual(second["media_type"], "video")

    def test_sniffed_type_wins_over_the_declared_type_and_extension(self) -> None:
        upload_id = self.begin()
        response = self.put(upload_id, "m000001.jpg", MP4)
        self.assertEqual(response.json(), {"name": "m000001.jpg", "bytes": len(MP4), "mime": "video/mp4", "kind": "video"})
        body = self.commit(upload_id, [image_post("p", "m000001.jpg")]).json()
        self.assertEqual(body["posts"][0]["media"][0]["path"], "posts/p_1.mp4")
        self.assertEqual(body["posts"][0]["media_type"], "video")
        self.assertNotIn("image", body["posts"][0])

    def test_one_staged_file_used_twice_is_copied(self) -> None:
        body = self.import_posts(
            {"m000001.jpg": JPEG},
            [image_post("a", "m000001.jpg"), image_post("b", "m000001.jpg")],
            avatars={"someone": "m000001.jpg"},
        )
        for path in ("posts/a_1.jpg", "posts/b_1.jpg", "avatars/someone.jpg"):
            self.assertEqual((self.cache / path).read_bytes(), JPEG)
        self.assertEqual(body["posts"][1]["user"]["avatar"], "avatars/someone.jpg")

    def test_streamed_body_in_small_chunks_is_sniffed_across_chunks(self) -> None:
        def chunks() -> Iterator[bytes]:
            for start in range(0, len(MP4), 3):
                yield MP4[start : start + 3]

        upload_id = self.begin()
        response = self.put(upload_id, "m000001.mp4", chunks())
        self.assertEqual(response.status_code, 200, response.text)
        self.assertEqual((self.cache / ".staging" / upload_id / "m000001.mp4").read_bytes(), MP4)

    def test_uploading_the_same_name_again_replaces_the_file(self) -> None:
        upload_id = self.upload({"m000001.jpg": JPEG})
        self.assertEqual(self.put(upload_id, "m000001.jpg", PNG).json()["mime"], "image/png")
        body = self.commit(upload_id, [image_post("a", "m000001.jpg")]).json()
        self.assertEqual(body["posts"][0]["media"][0]["path"], "posts/a_1.png")


class LibraryMergeTests(LibraryApiTestCase):
    def test_same_id_replaces_the_old_post_and_its_files(self) -> None:
        self.import_posts(
            {"m000001.jpg": JPEG, "m000002.png": PNG},
            [
                image_post("X", "m000001.jpg", taken_at="2026-01-01T00:00:00Z", caption="old"),
                image_post("keep", "m000002.png", taken_at="2025-01-01T00:00:00Z"),
            ],
        )
        self.assertTrue((self.cache / "posts" / "X_1.jpg").is_file())

        body = self.import_posts(
            {"m000001.webp": WEBP, "m000002.gif": GIF},
            [
                image_post("X", "m000001.webp", taken_at="2026-01-01T00:00:00Z", caption="new"),
                image_post("Y", "m000002.gif", taken_at="2027-01-01T00:00:00Z"),
            ],
        )
        self.assertEqual(body["revision"], 2)
        self.assertEqual([post["id"] for post in body["posts"]], ["Y", "X", "keep"])
        replaced = body["posts"][1]
        self.assertEqual(replaced["caption"], "new")
        self.assertEqual(replaced["media"][0]["path"], "posts/X-2_1.webp")
        self.assertFalse((self.cache / "posts" / "X_1.jpg").exists())
        self.assertTrue((self.cache / "posts" / "keep_1.png").is_file())
        self.assertEqual(
            [(item["new"], item["updated"], item["posts"]) for item in body["imports"]],
            [(2, 0, 2), (1, 1, 2)],
        )

        # The first name is free again, so a third version takes it back.
        third = self.import_posts({"m000001.jpg": JPEG}, [image_post("X", "m000001.jpg")])
        x = next(post for post in third["posts"] if post["id"] == "X")
        self.assertEqual(x["media"][0]["path"], "posts/X_1.jpg")
        self.assertFalse((self.cache / "posts" / "X-2_1.webp").exists())

    def test_posts_sort_newest_first_and_undated_last_in_original_order(self) -> None:
        files = {f"m00000{number}.jpg": JPEG for number in range(1, 6)}
        body = self.import_posts(
            files,
            [
                image_post("u1", "m000001.jpg"),
                image_post("old", "m000002.jpg", taken_at=1_600_000_000),
                image_post("u2", "m000003.jpg"),
                image_post("new", "m000004.jpg", taken_at="2026-05-01T10:00:00+02:00"),
                image_post("mid", "m000005.jpg", taken_at="2024-02-02T00:00:00Z"),
            ],
        )
        self.assertEqual([post["id"] for post in body["posts"]], ["new", "mid", "old", "u1", "u2"])
        self.assertEqual(body["posts"][0]["taken_at"], "2026-05-01T08:00:00.000Z")
        self.assertEqual(body["posts"][2]["taken_at"], "2020-09-13T12:26:40.000Z")

    def test_duplicate_ids_in_one_commit_keep_the_last_entry(self) -> None:
        body = self.import_posts(
            {"m000001.jpg": JPEG, "m000002.png": PNG},
            [image_post("d", "m000001.jpg", caption="first"), image_post("d", "m000002.png", caption="second")],
        )
        self.assertEqual(len(body["posts"]), 1)
        self.assertEqual(body["posts"][0]["caption"], "second")
        self.assertEqual(body["imports"][0]["posts"], 1)
        self.assertFalse((self.cache / "posts" / "d_1.jpg").exists())

    def test_avatar_is_kept_for_later_imports_and_replaced_by_a_newer_one(self) -> None:
        self.import_posts({"m000001.jpg": JPEG, "m000002.png": PNG}, [image_post("a", "m000001.jpg")], avatars={"someone": "m000002.png"})
        second = self.import_posts({"m000001.jpg": JPEG}, [image_post("b", "m000001.jpg")])
        self.assertEqual({post["user"]["avatar"] for post in second["posts"]}, {"avatars/someone.png"})

        third = self.import_posts(
            {"m000001.jpg": JPEG, "m000002.gif": GIF}, [image_post("c", "m000001.jpg")], avatars={"Someone": "m000002.gif"}
        )
        self.assertEqual({post["user"]["avatar"] for post in third["posts"]}, {"avatars/someone-2.gif"})
        self.assertFalse((self.cache / "avatars" / "someone.png").exists())

    def test_ids_become_collision_safe_file_names(self) -> None:
        long_id = "L" * 100
        files = {f"m00000{number}.jpg": JPEG for number in range(1, 7)}
        body = self.import_posts(
            files,
            [
                image_post("a.b", "m000001.jpg"),
                image_post("a_b", "m000002.jpg"),
                image_post("A_B", "m000003.jpg"),
                image_post("../../evil", "m000004.jpg"),
                image_post(long_id, "m000005.jpg"),
                image_post("CON", "m000006.jpg", user="nul"),
            ],
            avatars={"nul": "m000001.jpg"},
        )
        paths = [post["media"][0]["path"] for post in body["posts"]]
        self.assertEqual(
            paths[:4],
            ["posts/a_b_1.jpg", "posts/a_b-2_1.jpg", "posts/A_B-3_1.jpg", "posts/evil_1.jpg"],
        )
        self.assertEqual(paths[4], f"posts/{'L' * 63}_1.jpg")
        self.assertEqual(paths[5], "posts/CON__1.jpg")
        self.assertEqual(body["posts"][3]["id"], ".._.._evil")
        self.assertEqual(body["posts"][4]["id"], long_id)
        self.assertEqual(body["posts"][5]["user"]["avatar"], "avatars/nul_.jpg")
        self.assertNotIn("avatar", body["posts"][0]["user"])
        for path in paths:
            self.assertTrue((self.cache / path).is_file())
        self.assertEqual(
            sorted(os.listdir(self.cache)), [".instagram-library", ".staging", "avatars", "post.json", "posts"]
        )

    def test_comment_authors_are_read_from_username_or_user(self) -> None:
        body = self.import_posts(
            {"m000001.jpg": JPEG},
            [
                image_post(
                    "c",
                    "m000001.jpg",
                    comments=[
                        {"username": "first", "text": "a"},
                        {"user": "second", "text": "b"},
                        {"user": {"username": "third"}, "text": "c"},
                        {"text": "d"},
                    ],
                )
            ],
        )
        authors = [comment["username"] for comment in body["posts"][0]["comments"]]
        self.assertEqual(authors, ["first", "second", "third", "unknown"])

    def test_posts_from_other_sites_keep_their_links(self) -> None:
        body = self.import_posts(
            {"m000001.jpg": JPEG, "m000002.jpg": JPEG, "m000003.jpg": JPEG},
            [
                image_post(
                    "a", "m000001.jpg", url="https://mastodon.social/@alice/1",
                    user={"username": "alice", "profile_url": "https://mastodon.social/@alice"},
                ),
                # No profile link: not an Instagram post, so none is made up from the username.
                image_post("b", "m000002.jpg", url="https://example.com/p/b", user="bob"),
                # An Instagram post still gets the profile of the same name.
                image_post("c", "m000003.jpg", url="https://www.instagram.com/p/c/", user="carol"),
            ],
            source_url="https://mastodon.social/@alice",
        )
        first, second, third = body["posts"]
        self.assertEqual(first["url"], "https://mastodon.social/@alice/1")
        self.assertEqual(first["user"]["profile_url"], "https://mastodon.social/@alice")
        self.assertEqual(second["url"], "https://example.com/p/b")
        self.assertNotIn("profile_url", second["user"])
        self.assertEqual(third["user"]["profile_url"], "https://www.instagram.com/carol/")
        self.assertEqual(body["imports"][0]["source_url"], "https://mastodon.social/@alice")

    def test_text_is_capped_and_links_are_sanitised(self) -> None:
        body = self.import_posts(
            {"m000001.jpg": JPEG},
            [
                image_post(
                    "t",
                    "m000001.jpg",
                    caption="x" * 20_000 + "\ud800",
                    url="javascript:alert(1)",
                    user={"username": "@" + "u" * 300, "full_name": "n" * 500, "profile_url": "javascript:alert(1)"},
                    comments=[{"username": "c", "text": "y" * 5000}] * 300,
                    alt="a" * 3000,
                    location="l" * 300,
                    like_count=-5,
                    comment_count="12",
                    taken_at="not a date",
                )
            ],
            source_name="s" * 1000,
            source_url="data:text/html,x",
        )
        post = body["posts"][0]
        self.assertEqual(len(post["caption"]), 10_000)
        self.assertNotIn("url", post)
        self.assertEqual(post["user"], {"username": "u" * 100, "full_name": "n" * 200})
        self.assertEqual(len(post["comments"]), 200)
        self.assertEqual(len(post["comments"][0]["text"]), 2000)
        self.assertEqual((len(post["alt"]), len(post["location"])), (2000, 200))
        self.assertIsNone(post["like_count"])
        self.assertEqual(post["comment_count"], 0)
        self.assertNotIn("taken_at", post)
        self.assertEqual(len(body["imports"][0]["source_name"]), 255)
        self.assertNotIn("source_url", body["imports"][0])

        body = self.import_posts(
            {"m000001.jpg": JPEG},
            [image_post("t2", "m000001.jpg", caption="ok \ud83d", url="http://user:pw@WWW.instagram.com:8080/p/t2/")],
        )
        post = next(post for post in body["posts"] if post["id"] == "t2")
        self.assertEqual(post["caption"], "ok �")
        self.assertEqual(post["url"], "https://www.instagram.com/p/t2/")


class LibraryEditTests(LibraryApiTestCase):
    def edit(self, post_id: str, body: Any) -> Any:
        return self.client.patch(
            f"/api/library/posts/{post_id}",
            content=body if isinstance(body, bytes) else json.dumps(body),
            headers={"Content-Type": "application/json"},
        )

    def test_caption_is_replaced_and_saved_in_post_json(self) -> None:
        self.import_posts(
            {"m000001.jpg": JPEG, "m000002.png": PNG},
            [image_post("a", "m000001.jpg", caption="old"), image_post("b", "m000002.png", caption="keep")],
        )
        response = self.edit("a", {"caption": "  New text 🍋\nsecond line  "})
        self.assertEqual(response.status_code, 200, response.text)
        body = response.json()
        self.assertEqual(body["revision"], 2)
        captions = {post["id"]: post["caption"] for post in body["posts"]}
        self.assertEqual(captions, {"a": "New text 🍋\nsecond line", "b": "keep"})
        # The post keeps its place, files and the other fields; nothing counts as an import.
        self.assertEqual([post["id"] for post in body["posts"]], ["a", "b"])
        self.assertEqual(body["posts"][0]["media"][0]["path"], "posts/a_1.jpg")
        self.assertEqual(len(body["imports"]), 1)
        self.assertTrue((self.cache / "posts" / "a_1.jpg").exists())
        on_disk = json.loads((self.cache / "post.json").read_text(encoding="utf-8"))
        self.assertEqual(on_disk["posts"][0]["caption"], "New text 🍋\nsecond line")
        # Survives a restart.
        self.restart()
        self.assertEqual(self.library()["posts"][0]["caption"], "New text 🍋\nsecond line")

    def test_an_empty_caption_is_allowed(self) -> None:
        self.import_posts({"m000001.jpg": JPEG}, [image_post("a", "m000001.jpg", caption="old")])
        response = self.edit("a", {"caption": ""})
        self.assertEqual(response.status_code, 200, response.text)
        self.assertEqual(response.json()["posts"][0]["caption"], "")

    def test_an_unchanged_caption_writes_nothing(self) -> None:
        self.import_posts({"m000001.jpg": JPEG}, [image_post("a", "m000001.jpg", caption="same")])
        before = (self.cache / "post.json").stat().st_mtime_ns
        response = self.edit("a", {"caption": "same"})
        self.assertEqual(response.status_code, 200, response.text)
        self.assertEqual(response.json()["revision"], 1)
        self.assertEqual((self.cache / "post.json").stat().st_mtime_ns, before)

    def test_bad_edits_are_refused_and_change_nothing(self) -> None:
        self.import_posts({"m000001.jpg": JPEG}, [image_post("a", "m000001.jpg", caption="old")])
        for body, status in (
            ({"caption": "x"}, 404),  # (sent to an unknown id below)
            (b"{broken", 422),
            ([], 422),
            ({}, 422),
            ({"caption": 5}, 422),
            ({"caption": None}, 422),
            ({"caption": "x", "like_count": 1}, 422),
            ({"caption": "x" * 10_001}, 422),
            (b'{"caption": "' + b"x" * (300 * 1024) + b'"}', 413),
        ):
            with self.subTest(body=body if not isinstance(body, bytes) else body[:20]):
                post_id = "missing" if status == 404 else "a"
                response = self.edit(post_id, body)
                self.assertEqual(response.status_code, status, response.text)
        self.assertEqual(self.library()["posts"][0]["caption"], "old")
        self.assertEqual(self.library()["revision"], 1)

    def test_longest_allowed_caption_is_kept_whole(self) -> None:
        self.import_posts({"m000001.jpg": JPEG}, [image_post("a", "m000001.jpg")])
        caption = "🍋" * 10_000
        response = self.edit("a", {"caption": caption})
        self.assertEqual(response.status_code, 200, response.text)
        self.assertEqual(response.json()["posts"][0]["caption"], caption)


class LibraryDeleteTests(LibraryApiTestCase):
    def test_delete_post_removes_it_and_its_files(self) -> None:
        self.import_posts(
            {"m000001.jpg": JPEG, "m000002.mp4": MP4, "m000003.png": PNG},
            [
                image_post("a", "m000001.jpg"),
                {"id": "b", "media": [{"path": "m000002.mp4", "poster": "m000003.png"}], "user": "x"},
            ],
        )
        response = self.client.delete("/api/library/posts/b")
        self.assertEqual(response.status_code, 200, response.text)
        body = response.json()
        self.assertEqual([post["id"] for post in body["posts"]], ["a"])
        self.assertEqual(body["revision"], 2)
        self.assertEqual(len(body["imports"]), 1)
        self.assertEqual(sorted(os.listdir(self.cache / "posts")), ["a_1.jpg"])
        self.assertEqual(self.client.delete("/api/library/posts/b").status_code, 404)

    def test_file_held_open_during_delete_is_removed_by_a_later_sweep(self) -> None:
        self.import_posts({"m000001.jpg": JPEG, "m000002.png": PNG}, [image_post("a", "m000001.jpg"), image_post("b", "m000002.png")])
        locked = str(self.cache / "posts" / "a_1.jpg")
        real_unlink = os.unlink

        def unlink(path: Any, *args: Any, **kwargs: Any) -> None:
            if str(path) == locked:
                raise PermissionError(32, "in use", locked)
            real_unlink(path, *args, **kwargs)

        with patch("os.unlink", side_effect=unlink):
            self.assertEqual(self.client.delete("/api/library/posts/a").status_code, 200)
        self.assertTrue(Path(locked).exists())
        self.assertEqual(self.client.delete("/api/library/posts/b").status_code, 200)
        self.assertEqual(os.listdir(self.cache / "posts"), [])

    def test_clear_removes_only_what_the_library_owns(self) -> None:
        self.import_posts({"m000001.jpg": JPEG, "m000002.png": PNG}, [image_post("a", "m000001.jpg")], avatars={"someone": "m000002.png"})
        open_upload = self.upload({"m000001.jpg": JPEG})
        (self.cache / "notes.txt").write_text("mine", encoding="utf-8")
        (self.cache / "folder").mkdir()
        (self.root / "sibling.txt").write_text("keep", encoding="utf-8")

        self.assertEqual(self.client.delete("/api/library").status_code, 204)

        self.assertEqual(sorted(os.listdir(self.cache)), [".instagram-library", "folder", "notes.txt"])
        self.assertTrue((self.root / "sibling.txt").is_file())
        body = self.library()
        self.assertEqual((body["posts"], body["imports"], body["updated_at"]), ([], [], None))
        self.assertEqual(body["revision"], 2, "revisions keep growing so file URLs never repeat")
        self.assertEqual(self.put(open_upload, "m000002.jpg", JPEG).status_code, 404)
        self.assertEqual(self.client.delete("/api/library").status_code, 204)

        again = self.import_posts({"m000001.jpg": JPEG}, [image_post("a", "m000001.jpg")])
        self.assertGreater(again["revision"], 2)


class LibraryUploadTests(LibraryApiTestCase):
    def test_upload_ids_are_uuid_hex_and_unknown_ones_are_404(self) -> None:
        upload_id = self.begin()
        self.assertRegex(upload_id, r"^[0-9a-f]{32}$")
        self.assertTrue((self.cache / ".staging" / upload_id).is_dir())
        for unknown in ("0" * 32, "not-an-id", upload_id.upper()):
            with self.subTest(upload_id=unknown):
                self.assertEqual(self.put(unknown, "m000001.jpg", JPEG).status_code, 404)
                self.assertEqual(
                    self.client.post(f"/api/library/uploads/{unknown}/commit", json={}).status_code, 404
                )

    def test_bad_staged_names_are_refused(self) -> None:
        upload_id = self.begin()
        for name in ("m1.jpg", "M000001.jpg", "m000001.JPG", "m000001.j", "m0000001.jpg", "x000001.jpg", "m000001.jpegxx", "m000001.jpg.part", "post.json"):
            with self.subTest(name=name):
                self.assertEqual(self.put(upload_id, name, JPEG).status_code, 422)
        for name in ("..%2fm000001.jpg", "%2e%2e%2f%2e%2e%2fpost.json", "m000001.jpg%2f.."):
            with self.subTest(name=name):
                self.assertIn(self.put(upload_id, name, JPEG).status_code, (404, 422))
        self.assertEqual(self.staged_entries(), [self.cache / ".staging" / upload_id])
        self.assertEqual(sorted(os.listdir(self.cache)), [".instagram-library", ".staging"])

    def test_only_supported_images_and_videos_are_accepted(self) -> None:
        upload_id = self.begin()
        accepted = {
            "m000001.jpg": "image/jpeg",
            "m000002.png": "image/png",
            "m000003.gif": "image/gif",
            "m000004.webp": "image/webp",
            "m000005.avif": "image/avif",
            "m000006.bmp": "image/bmp",
            "m000007.mp4": "video/mp4",
            "m000008.mov": "video/quicktime",
            "m000009.webm": "video/webm",
        }
        contents = dict(zip(accepted, (JPEG, PNG, GIF, WEBP, AVIF, BMP, MP4, MOV, WEBM)))
        for name, mime in accepted.items():
            with self.subTest(name=name):
                response = self.put(upload_id, name, contents[name])
                self.assertEqual(response.status_code, 200, response.text)
                self.assertEqual(response.json()["mime"], mime)
        refused = {"svg": SVG, "html": HTML, "heic": HEIC, "heif": HEIF, "mkv": MATROSKA, "empty": b"", "text": b"hello"}
        for label, content in refused.items():
            with self.subTest(refused=label):
                response = self.put(upload_id, "m000010.jpg", content)
                self.assertEqual(response.status_code, 415)
                self.assertIn("detail", response.json())
        self.assertNotIn("m000010.jpg", os.listdir(self.cache / ".staging" / upload_id))
        self.assertFalse(list((self.cache / ".staging").rglob("*.part")))

    def test_abort_is_idempotent_and_removes_staged_files(self) -> None:
        upload_id = self.upload({"m000001.jpg": JPEG})
        for _ in range(2):
            self.assertEqual(self.client.delete(f"/api/library/uploads/{upload_id}").status_code, 204)
        self.assertFalse((self.cache / ".staging" / upload_id).exists())
        self.assertEqual(self.put(upload_id, "m000002.jpg", JPEG).status_code, 404)
        self.assertEqual(self.client.delete("/api/library/uploads/not-an-id").status_code, 204)

        leftover = self.cache / ".staging" / ("ab" * 16)
        leftover.mkdir()
        (leftover / "m000001.jpg").write_bytes(JPEG)
        self.assertEqual(self.client.delete(f"/api/library/uploads/{'ab' * 16}").status_code, 204)
        self.assertFalse(leftover.exists())

    def test_commit_validation_errors_keep_the_upload_usable(self) -> None:
        upload_id = self.upload({"m000001.jpg": JPEG, "m000002.mp4": MP4})
        url = f"/api/library/uploads/{upload_id}/commit"
        invalid_bodies: list[Any] = [
            {"source_name": "x", "source_kind": "archive", "posts": [image_post("a", "m000009.jpg")]},
            {"source_name": "x", "source_kind": "archive", "posts": [image_post("a", "../post.json")]},
            {"source_name": "x", "source_kind": "archive", "posts": [image_post("a", "posts/a_1.jpg")]},
            {"source_name": "x", "source_kind": "archive", "posts": [{"id": "a", "media": [{"path": "m000002.mp4", "poster": "m000002.mp4"}]}]},
            {"source_name": "x", "source_kind": "archive", "posts": [{"id": "a", "media": [], "user": "x"}]},
            {"source_name": "x", "source_kind": "archive", "posts": [{"media": ["m000001.jpg"]}]},
            {"source_name": "x", "source_kind": "archive", "posts": ["m000001.jpg"]},
            {"source_name": "x", "source_kind": "library", "posts": [image_post("a", "m000001.jpg")]},
            {"source_name": "x", "source_kind": "archive", "posts": []},
            {"source_name": "x", "source_kind": "archive", "posts": [image_post("a", "m000001.jpg")], "avatars": {"x": "m000002.mp4"}},
            {"source_name": "x", "source_kind": "archive", "posts": [image_post("a", "m000001.jpg")], "avatars": ["m000001.jpg"]},
            [],
        ]
        for body in invalid_bodies:
            with self.subTest(body=body):
                response = self.client.post(url, json=body)
                self.assertEqual(response.status_code, 422, response.text)
                self.assertIsInstance(response.json()["detail"], str)
        response = self.client.post(url, content=b"{not json", headers={"Content-Type": "application/json"})
        self.assertEqual(response.status_code, 422)
        self.assertEqual(self.library()["revision"], 0)
        self.assertFalse((self.cache / "post.json").exists())

        self.assertEqual(self.commit(upload_id, [image_post("a", "m000001.jpg")]).status_code, 200)
        self.assertEqual(self.commit(upload_id, [image_post("a", "m000001.jpg")]).status_code, 404)

    def test_failed_save_rolls_back_moved_files(self) -> None:
        self.import_posts({"m000001.jpg": JPEG}, [image_post("a", "m000001.jpg")])
        before = (self.cache / "post.json").read_bytes()
        upload_id = self.upload({"m000001.png": PNG})
        with patch.object(library_module, "_write_manifest", side_effect=OSError("disk full")):
            response = self.commit(upload_id, [image_post("b", "m000001.png")])
        self.assertEqual(response.status_code, 503)
        self.assertIn("disk full", response.json()["detail"])
        self.assertEqual((self.cache / "post.json").read_bytes(), before)
        self.assertEqual(os.listdir(self.cache / "posts"), ["a_1.jpg"])
        self.assertEqual(self.staged_entries(), [])
        self.assertEqual(self.library()["revision"], 1)


class LibraryLimitTests(LibraryApiTestCase):
    def setUp(self) -> None:
        super().setUp()
        self.limits = LibraryLimits(
            max_image_bytes=1000,
            max_video_bytes=3000,
            max_session_bytes=5000,
            max_cache_bytes=7000,
            max_commit_bytes=2000,
            max_files_per_session=3,
            max_posts_per_commit=2,
            max_media_per_post=2,
            max_comments_per_post=1,
        )
        self.client = self.start(self.limits, self.root / "limited")
        self.cache = self.root / "limited"

    @staticmethod
    def sized(content: bytes, size: int) -> bytes:
        return content + b"\x00" * (size - len(content))

    @staticmethod
    def chunked(content: bytes, size: int = 256) -> Iterator[bytes]:
        for start in range(0, len(content), size):
            yield content[start : start + size]

    def test_per_kind_limits_are_enforced_while_streaming(self) -> None:
        upload_id = self.begin()
        self.assertEqual(self.put(upload_id, "m000001.jpg", self.sized(JPEG, 1000)).status_code, 200)
        self.assertEqual(self.put(upload_id, "m000002.jpg", self.chunked(self.sized(JPEG, 1001))).status_code, 413)
        self.assertEqual(self.put(upload_id, "m000002.mp4", self.chunked(self.sized(MP4, 3000))).status_code, 200)
        response = self.put(upload_id, "m000003.mp4", self.chunked(self.sized(MP4, 3001)))
        self.assertEqual(response.status_code, 413)
        self.assertIn("video", response.json()["detail"])
        self.assertEqual(sorted(os.listdir(self.cache / ".staging" / upload_id)), ["m000001.jpg", "m000002.mp4"])

    def test_content_length_beyond_every_limit_is_refused_before_reading(self) -> None:
        upload_id = self.begin()
        response = self.put(upload_id, "m000001.mp4", self.sized(MP4, 3001))
        self.assertEqual(response.status_code, 413)
        self.assertEqual(os.listdir(self.cache / ".staging" / upload_id), [])

    def test_session_cache_and_file_count_limits(self) -> None:
        first = self.begin()
        self.assertEqual(self.put(first, "m000001.mp4", self.sized(MP4, 3000)).status_code, 200)
        response = self.put(first, "m000002.mp4", self.chunked(self.sized(MP4, 2001)))
        self.assertEqual(response.status_code, 413)
        self.assertIn("One import", response.json()["detail"])
        self.assertEqual(self.put(first, "m000002.mp4", self.sized(MP4, 2000)).status_code, 200)
        self.assertEqual(self.commit(first, [{"id": "a", "media": ["m000001.mp4", "m000002.mp4"], "user": "x"}]).status_code, 200)

        # 5000 bytes are in the library now: the cache limit (7000) counts them and staged bytes.
        second = self.begin()
        self.assertEqual(self.put(second, "m000001.jpg", self.sized(JPEG, 1000)).status_code, 200)
        self.assertEqual(self.put(second, "m000002.jpg", self.sized(JPEG, 995)).status_code, 200)
        response = self.put(second, "m000003.jpg", self.chunked(JPEG[:10], 4))
        self.assertEqual(response.status_code, 413)
        self.assertIn("full", response.json()["detail"])
        # Aborting gives the staged bytes back.
        self.assertEqual(self.client.delete(f"/api/library/uploads/{second}").status_code, 204)
        self.assertEqual(self.put(self.begin(), "m000001.jpg", self.sized(JPEG, 1000)).status_code, 200)

        third = self.begin()
        for number in range(1, 4):
            self.assertEqual(self.put(third, f"m00000{number}.jpg", JPEG[:10]).status_code, 200)
        self.assertEqual(self.put(third, "m000004.jpg", JPEG[:10]).status_code, 413)
        self.assertEqual(self.put(third, "m000003.jpg", JPEG[:20]).status_code, 200)

    def test_commit_limits(self) -> None:
        upload_id = self.upload({"m000001.jpg": JPEG[:10], "m000002.jpg": JPEG[:10], "m000003.jpg": JPEG[:10]})
        too_many_posts = [image_post(name, "m000001.jpg") for name in ("a", "b", "c")]
        self.assertEqual(self.commit(upload_id, too_many_posts).status_code, 422)
        too_many_media = [{"id": "a", "media": ["m000001.jpg", "m000002.jpg", "m000003.jpg"], "user": "x"}]
        self.assertEqual(self.commit(upload_id, too_many_media).status_code, 422)
        huge = [image_post("a", "m000001.jpg", caption="x" * 3000)]
        self.assertEqual(self.commit(upload_id, huge).status_code, 413)
        comments = [image_post("a", "m000001.jpg", comments=[{"username": "u", "text": "t"}] * 3)]
        body = self.commit(upload_id, comments).json()
        self.assertEqual(len(body["posts"][0]["comments"]), 1)


class LibraryFileTests(LibraryApiTestCase):
    def setUp(self) -> None:
        super().setUp()
        self.video = MP4 + bytes(range(256)) * 3
        self.import_posts(
            {"m000001.mp4": self.video, "m000002.jpg": JPEG, "m000003.png": PNG},
            [{"id": "v", "media": [{"path": "m000001.mp4", "poster": "m000002.jpg"}], "user": "someone"}],
            avatars={"someone": "m000003.png"},
        )

    def test_files_are_served_with_their_type_and_nosniff(self) -> None:
        for path, content, mime in (
            ("posts/v_1.mp4", self.video, "video/mp4"),
            ("posts/v_1_poster.jpg", JPEG, "image/jpeg"),
            ("avatars/someone.png", PNG, "image/png"),
        ):
            with self.subTest(path=path):
                response = self.client.get(f"/api/library/files/{path}", params={"r": 1})
                self.assertEqual(response.status_code, 200)
                self.assertEqual(response.content, content)
                self.assertEqual(response.headers["content-type"], mime)
                self.assertEqual(response.headers["x-content-type-options"], "nosniff")
                self.assertEqual(response.headers["cache-control"], "no-cache")
                self.assertEqual(response.headers["accept-ranges"], "bytes")

    def test_range_requests_let_videos_seek(self) -> None:
        url = "/api/library/files/posts/v_1.mp4"
        size = len(self.video)
        response = self.client.get(url, headers={"Range": "bytes=0-99"})
        self.assertEqual(response.status_code, 206)
        self.assertEqual(response.headers["content-range"], f"bytes 0-99/{size}")
        self.assertEqual(response.content, self.video[:100])

        response = self.client.get(url, headers={"Range": f"bytes={size - 10}-"})
        self.assertEqual(response.status_code, 206)
        self.assertEqual(response.content, self.video[-10:])

        response = self.client.get(url, headers={"Range": f"bytes={size + 10}-"})
        self.assertEqual(response.status_code, 416)
        self.assertEqual(response.headers["content-range"], f"bytes */{size}")

        etag = self.client.head(url).headers["etag"]
        response = self.client.get(url, headers={"Range": "bytes=5-9", "If-Range": etag})
        self.assertEqual((response.status_code, response.content), (206, self.video[5:10]))
        response = self.client.get(url, headers={"Range": "bytes=5-9", "If-Range": '"stale"'})
        self.assertEqual((response.status_code, response.content), (200, self.video))

        response = self.client.head(url)
        self.assertEqual(response.status_code, 200)
        self.assertEqual(response.headers["content-length"], str(size))
        self.assertEqual(response.content, b"")

    def test_only_library_media_names_are_served(self) -> None:
        (self.cache / "posts" / "notes.html").write_text("<script></script>", encoding="utf-8")
        (self.cache / "posts" / "x_1.svg").write_bytes(SVG)
        for path in (
            "post.json",
            "posts/notes.html",
            "posts/x_1.svg",
            "posts/missing_1.jpg",
            "posts/../post.json",
            "posts/..%2fpost.json",
            "%2e%2e/%2e%2e/secret.txt",
            "posts/v_1.mp4:stream",
            "posts",
            "avatars/someone_1.mp4",
            "POSTS/v_1.mp4",
            ".staging/" + "ab" * 16 + "/m000001.jpg",
            "posts/sub/v_1.mp4",
        ):
            with self.subTest(path=path):
                response = self.client.get(f"/api/library/files/{path}")
                self.assertEqual(response.status_code, 404)
                self.assertNotIn("text/html", response.headers.get("content-type", "").replace("application/json", ""))

    @unittest.skipUnless(hasattr(os, "symlink"), "symbolic links are not available")
    def test_symbolic_links_are_never_followed(self) -> None:
        outside = self.root / "outside.jpg"
        outside.write_bytes(JPEG)
        try:
            os.symlink(outside, self.cache / "posts" / "link_1.jpg")
        except OSError as error:  # Windows without developer mode
            self.skipTest(f"cannot create a symbolic link: {error}")
        self.assertEqual(self.client.get("/api/library/files/posts/link_1.jpg").status_code, 404)
        self.client.delete("/api/library")
        self.assertTrue(outside.is_file())

    @unittest.skipUnless(os.name == "nt", "junctions exist on Windows only")
    def test_junctioned_media_folder_is_refused_and_clear_keeps_its_target(self) -> None:
        import _winapi

        outside = self.root / "elsewhere"
        outside.mkdir()
        (outside / "a_1.jpg").write_bytes(JPEG)
        cache = self.root / "junctioned"
        owner = self.start(cache=cache)
        self.assertEqual(owner.get("/api/library").status_code, 200)
        self.stop(owner)
        _winapi.CreateJunction(str(outside), str(cache / "posts"))
        client = self.start(cache=cache)

        self.assertEqual(client.get("/api/library").status_code, 503)
        self.assertEqual(client.get("/api/library/files/posts/a_1.jpg").status_code, 503)
        self.assertEqual(client.delete("/api/library").status_code, 204)
        self.assertFalse((cache / "posts").exists())
        self.assertTrue((outside / "a_1.jpg").is_file())
        self.assertEqual(client.get("/api/library").status_code, 200)


class LibraryLifecycleTests(LibraryApiTestCase):
    def test_library_survives_a_restart(self) -> None:
        body = self.import_posts({"m000001.jpg": JPEG}, [image_post("a", "m000001.jpg")])
        self.restart()
        self.assertEqual(self.library(), body)
        second = self.import_posts({"m000001.png": PNG}, [image_post("b", "m000001.png")])
        self.assertEqual(second["revision"], 2)
        self.assertEqual(sorted(post["id"] for post in second["posts"]), ["a", "b"])

    def test_first_use_removes_staged_uploads_of_earlier_runs_and_unreferenced_files(self) -> None:
        self.import_posts({"m000001.jpg": JPEG}, [image_post("a", "m000001.jpg")])
        self.stop(self.client)
        # Upload sessions live in memory: whatever an earlier run staged, however recently, can
        # never be committed and must not keep counting against (or escaping) the size limits.
        stale = self.cache / ".staging" / ("ab" * 16)
        fresh = self.cache / ".staging" / ("cd" * 16)
        for folder in (stale, fresh):
            folder.mkdir()
            (folder / "m000001.jpg").write_bytes(JPEG)
        old = time.time() - 25 * 60 * 60
        os.utime(stale, (old, old))
        (self.cache / ".staging" / "keep.txt").write_text("not ours", encoding="utf-8")
        (self.cache / "posts" / "orphan_1.jpg").write_bytes(JPEG)
        (self.cache / "posts" / "readme.txt").write_text("not ours", encoding="utf-8")
        (self.cache / ".post.json-x.tmp").write_text("{", encoding="utf-8")

        restarted = self.start()
        self.assertEqual(len(restarted.get("/api/library").json()["posts"]), 1)
        self.assertFalse(stale.exists())
        self.assertFalse(fresh.exists())
        self.assertEqual(os.listdir(self.cache / ".staging"), ["keep.txt"])
        self.assertEqual(sorted(os.listdir(self.cache / "posts")), ["a_1.jpg", "readme.txt"])
        self.assertFalse((self.cache / ".post.json-x.tmp").exists())

    def test_unusable_folder_gives_503_without_breaking_startup(self) -> None:
        blocked = self.root / "a-file"
        blocked.write_text("x", encoding="utf-8")
        client = self.start(cache=blocked / "cache")
        self.assertEqual(client.get("/api/health").json(), {"status": "ok"})
        for method, url in (
            ("GET", "/api/library"),
            ("POST", "/api/library/uploads"),
            ("DELETE", "/api/library"),
            ("DELETE", "/api/library/posts/a"),
            ("PATCH", "/api/library/posts/a"),
            ("GET", "/api/library/files/posts/a_1.jpg"),
        ):
            with self.subTest(method=method, url=url):
                body = b'{"caption": "x"}' if method == "PATCH" else None
                response = client.request(method, url, content=body)
                self.assertEqual(response.status_code, 503)
                self.assertIn(str(blocked), response.json()["detail"])
        self.assertEqual(client.post("/api/counter/click").json()["count"], 1)

    def test_damaged_post_json_gives_503_until_the_library_is_cleared(self) -> None:
        self.import_posts({"m000001.jpg": JPEG}, [image_post("a", "m000001.jpg")])
        self.stop(self.client)
        (self.cache / "post.json").write_text("{broken", encoding="utf-8")
        self.client = self.start()
        response = self.client.get("/api/library")
        self.assertEqual(response.status_code, 503)
        self.assertIn("damaged", response.json()["detail"])
        self.assertTrue((self.cache / "posts" / "a_1.jpg").exists(), "a damaged library is not swept")
        self.assertEqual(self.client.delete("/api/library").status_code, 204)
        self.assertEqual(self.library()["posts"], [])

    def test_concurrent_commits_never_lose_posts(self) -> None:
        uploads = [self.upload({"m000001.jpg": JPEG}) for _ in range(3)]
        real_write = library_module._write_manifest

        def slow_write(root: Path, document: dict[str, Any]) -> None:
            time.sleep(0.15)
            real_write(root, document)

        start = threading.Barrier(len(uploads))
        statuses: list[int] = []

        def run(index: int) -> None:
            start.wait(timeout=10)
            statuses.append(self.commit(uploads[index], [image_post(f"p{index}", "m000001.jpg")]).status_code)

        with patch.object(library_module, "_write_manifest", side_effect=slow_write):
            threads = [threading.Thread(target=run, args=(index,)) for index in range(len(uploads))]
            for thread in threads:
                thread.start()
            for thread in threads:
                thread.join(timeout=30)
        self.assertEqual(statuses, [200] * len(uploads))
        body = self.library()
        self.assertEqual(sorted(post["id"] for post in body["posts"]), ["p0", "p1", "p2"])
        self.assertEqual(body["revision"], 3)
        self.assertEqual(len(body["imports"]), 3)

    def test_default_folder_comes_from_settings_and_settings_errors_give_503(self) -> None:
        environment = patch.dict(
            os.environ,
            {"APPDATA": str(self.root / "roaming"), "XDG_CONFIG_HOME": str(self.root / "config"), "TEMP": str(self.root / "temp")},
        )
        environment.start()
        self.addCleanup(environment.stop)
        os.environ.pop("INSTAGRAM_CACHE_DIR", None)
        client = TestClient(main.create_app(self.root / "counter.db"))
        client.__enter__()
        self.addCleanup(client.__exit__, None, None, None)
        self.assertEqual(client.get("/api/library").json()["directory"], str(self.root / "temp" / "instagram_cache"))

        path = settings.settings_path()
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text('{"instagram_cache_directory": 42}', encoding="utf-8")
        broken = TestClient(main.create_app(self.root / "counter.db"))
        broken.__enter__()
        self.addCleanup(broken.__exit__, None, None, None)
        response = broken.get("/api/library")
        self.assertEqual(response.status_code, 503)
        self.assertIn("instagram_cache_directory", response.json()["detail"])


class ParallelUploadTests(unittest.TestCase):
    def test_uploads_to_different_sessions_run_in_parallel(self) -> None:
        with tempfile.TemporaryDirectory(prefix="library-test-") as directory:
            library = Library(lambda: Path(directory) / "cache")
            first = library.create_upload()
            second = library.create_upload()

            async def scenario() -> None:
                release = anyio.Event()

                async def slow_body() -> AsyncIterator[bytes]:
                    yield JPEG[:5]
                    await release.wait()
                    yield JPEG[5:]

                async def quick_body() -> AsyncIterator[bytes]:
                    yield PNG

                async with anyio.create_task_group() as group:
                    group.start_soon(library.receive_file, first, "m000001.jpg", slow_body(), None)
                    await anyio.sleep(0.05)
                    with anyio.fail_after(5):
                        stored = await library.receive_file(second, "m000001.png", quick_body(), None)
                    self.assertEqual(stored["mime"], "image/png")
                    with self.assertRaises(LibraryError) as conflict:
                        library.commit(first, b"{}")
                    self.assertEqual(conflict.exception.status_code, 409)
                    release.set()

            anyio.run(scenario)
            response = library.commit(
                first,
                json.dumps({"source_name": "a", "source_kind": "folder", "posts": [image_post("a", "m000001.jpg")]}).encode(),
            )
            self.assertEqual(response["posts"][0]["media"][0]["bytes"], len(JPEG))
            library.abort_upload(second)
            library.close()


class HelperTests(unittest.TestCase):
    def test_sniff_media(self) -> None:
        cases = {
            JPEG: "image/jpeg",
            PNG: "image/png",
            GIF: "image/gif",
            WEBP: "image/webp",
            BMP: "image/bmp",
            AVIF: "image/avif",
            ftyp(b"avis"): "image/avif",
            MP4: "video/mp4",
            ftyp(b"mp42", b"mp42", b"isom"): "video/mp4",
            MOV: "video/quicktime",
            WEBM: "video/webm",
        }
        for content, mime in cases.items():
            with self.subTest(mime=mime, head=content[:12]):
                media = sniff_media(content)
                self.assertIsNotNone(media)
                assert media is not None
                self.assertEqual(media.mime, mime)
        for content in (HEIC, HEIF, ftyp(b"mif1", b"heix"), MATROSKA, SVG, HTML, b"", b"BM" + b"\x01" * 30, b"\x1a\x45\xdf\xa3"):
            with self.subTest(refused=content[:16]):
                self.assertIsNone(sniff_media(content))

    def test_parse_timestamp(self) -> None:
        self.assertEqual(parse_timestamp(1_700_000_000), "2023-11-14T22:13:20.000Z")
        self.assertEqual(parse_timestamp(1_700_000_000_123), "2023-11-14T22:13:20.123Z")
        self.assertEqual(parse_timestamp("1700000000"), "2023-11-14T22:13:20.000Z")
        self.assertEqual(parse_timestamp("2026-11-01T15:04:05Z"), "2026-11-01T15:04:05.000Z")
        self.assertEqual(parse_timestamp("2026-11-01T17:04:05+02:00"), "2026-11-01T15:04:05.000Z")
        invalid: tuple[object, ...] = (None, True, -5, 0, "", "yesterday", "1960-01-01T00:00:00Z", float("nan"), [], "9" * 70)
        for value in invalid:
            with self.subTest(value=value):
                self.assertIsNone(parse_timestamp(value))

    def test_safe_instagram_url(self) -> None:
        self.assertEqual(safe_instagram_url("http://instagram.com/p/x/?a=1#b"), "https://instagram.com/p/x/?a=1#b")
        for value in ("javascript:alert(1)", "https://evil.example/", "https://l.instagram.com/?u=x", "//www.instagram.com/", "https://www.instagram.com/a b", 5):
            with self.subTest(value=value):
                self.assertIsNone(safe_instagram_url(value))

    def test_safe_web_url_accepts_any_http_site_and_drops_credentials(self) -> None:
        for value, expected in (
            ("https://example.com/a?b=1#c", "https://example.com/a?b=1#c"),
            ("HTTP://Example.COM:81/x", "http://example.com:81/x"),
            ("https://user:secret@example.com/x", "https://example.com/x"),
            ("https://[::1]:8080/x", "https://[::1]:8080/x"),
            # Instagram's own hosts are normalised as before.
            ("http://www.instagram.com:80/p/abc/", "https://www.instagram.com/p/abc/"),
            ("https://l.instagram.com/?u=x", "https://l.instagram.com/?u=x"),
        ):
            with self.subTest(value=value):
                self.assertEqual(safe_web_url(value), expected)
        for value in (
            "javascript:alert(1)", "data:text/html,x", "file:///etc/passwd", "ftp://example.com/x", "//example.com/x",
            "/relative", "example.com/x", "https://", "https:///x", "https://example.com/a b", "https://example.com\@evil/",
            "", "https://example.com/" + "a" * 3000, 5, None,
        ):
            with self.subTest(value=value):
                self.assertIsNone(safe_web_url(value))
        self.assertTrue(is_instagram_url("https://instagr.am/p/x/"))
        self.assertFalse(is_instagram_url("https://example.com/"))

    def test_sort_posts_is_stable_for_undated_posts(self) -> None:
        posts = [{"id": "u1"}, {"id": "d1", "taken_at": "2020-01-01T00:00:00.000Z"}, {"id": "u2", "taken_at": "bad"}, {"id": "d2", "taken_at": "2021-01-01T00:00:00.000Z"}]
        self.assertEqual([post["id"] for post in sort_posts(posts)], ["d2", "d1", "u1", "u2"])


if __name__ == "__main__":
    unittest.main()
