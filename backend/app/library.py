"""The merged import library kept in the instagram_cache folder.

The folder is itself a valid "instagram-export" v1 export (see instagram.md section 8):

    post.json                      manifest + "library" metadata (revision, import history)
    posts/<stem>_<k>.<ext>         media, <stem> derived from the post id
    posts/<stem>_<k>_poster.<ext>  video posters
    avatars/<stem>.<ext>           profile pictures, <stem> derived from the username
    .staging/<upload_id>/...       files of uploads that are not committed yet
    .instagram-library             ownership marker; also locked while a process uses the folder

The library only adopts a folder that is empty or that it created itself (the marker, or a
post.json it wrote), and only ever deletes files it could have written there: a folder that
already holds an export or other files is refused (503), never cleaned up. One process at a
time uses a folder (the marker is locked), so two servers never overwrite each other's work.

The browser parses an export, uploads its media into a staging session and commits the
normalised posts. A commit merges them into post.json the same way the frontend's
mergeImportResults does: a post with a known id replaces the old one, the rest is added, and
posts are ordered by taken_at (newest first, undated posts last). A post's caption (its
description) can be edited afterwards (update_post); its files stay as they are.
"""

from __future__ import annotations

import json
import logging
import math
import os
import re
import shutil
import stat
import tempfile
import threading
import time
import uuid
from collections.abc import AsyncIterator, Callable, Iterable
from dataclasses import dataclass, field
from datetime import datetime, timedelta, timezone
from pathlib import Path
from typing import Any, BinaryIO, Literal
from urllib.parse import urlsplit

import anyio.to_thread

LOGGER = logging.getLogger(__name__)

JsonObject = dict[str, Any]
MediaKind = Literal["image", "video"]
SourceKind = Literal["archive", "folder", "multiple"]

KiB = 1024
MiB = 1024 * KiB
GiB = 1024 * MiB

MANIFEST_NAME = "post.json"
POSTS_FOLDER = "posts"
AVATARS_FOLDER = "avatars"
STAGING_FOLDER = ".staging"
_TEMPORARY_PREFIX = ".post.json-"
_TEMPORARY_SUFFIX = ".tmp"
_PROBE_PREFIX = ".write-test-"
MARKER_NAME = ".instagram-library"
_MARKER_TEXT = (
    b"This folder holds the posts imported into the Instagram clone (post.json, posts/,\r\n"
    b"avatars/). The app manages these files: do not put other files into posts/ or avatars/.\r\n"
)
#: Windows locks one byte of the marker far past its text, so the text stays readable.
_LOCK_OFFSET = 1 << 30

# Text caps, the same as frontend/src/import/limits.ts.
MAX_CAPTION_CHARS = 10_000
MAX_COMMENT_CHARS = 2000
MAX_USERNAME_CHARS = 100
MAX_NAME_CHARS = 200
MAX_ALT_CHARS = 2000
MAX_LOCATION_CHARS = 200
MAX_ID_CHARS = 100
MAX_URL_CHARS = 2048
MAX_SOURCE_NAME_CHARS = 255
MAX_SAFE_INTEGER = 2**53 - 1
#: File name stems (post ids, usernames) are cut to this many characters.
MAX_STEM_CHARS = 64
#: Body of a post edit (PATCH /api/library/posts/{id}): a caption, JSON-escaped at worst.
MAX_POST_EDIT_BYTES = 256 * KiB

#: Bytes of an upload looked at to decide what it is (enough for a long ftyp brand list).
SNIFF_BYTES = 4096
#: Upload bytes collected before each write to disk.
WRITE_BUFFER_BYTES = 1 * MiB
#: Free disk space is checked again after this many bytes of one upload.
FREE_SPACE_CHECK_BYTES = 64 * MiB

UPLOAD_ID_RE = re.compile(r"[0-9a-f]{32}")
STAGED_NAME_RE = re.compile(r"m[0-9]{6}\.[a-z0-9]{2,5}")
_IMAGE_EXTENSIONS = "jpg|png|gif|webp|avif|bmp"
_MEDIA_EXTENSIONS = f"{_IMAGE_EXTENSIONS}|mp4|mov|webm"
POST_FILE_RE = re.compile(
    rf"(?P<stem>[A-Za-z0-9_-]{{1,{MAX_STEM_CHARS}}})_[0-9]{{1,3}}(?:_poster)?\.(?:{_MEDIA_EXTENSIONS})"
)
AVATAR_FILE_RE = re.compile(
    rf"(?P<stem>[A-Za-z0-9_-]{{1,{MAX_STEM_CHARS}}})\.(?:{_IMAGE_EXTENSIONS})"
)
_USERNAME_RE = re.compile(r"[A-Za-z0-9._]{1,30}")
_URL_FORBIDDEN_RE = re.compile(r"[\x00-\x20\x7f-\x9f\\]")
_LONE_SURROGATE_RE = re.compile(r"[\ud800-\udfff]")
_INSTAGRAM_HOSTS = frozenset({"instagram.com", "www.instagram.com", "m.instagram.com", "instagr.am"})
_WINDOWS_RESERVED_NAMES = frozenset(
    {"con", "prn", "aux", "nul"}
    | {f"com{digit}" for digit in "123456789"}
    | {f"lpt{digit}" for digit in "123456789"}
)


@dataclass(frozen=True)
class LibraryLimits:
    """Size limits of the library; the defaults are the contract values, tests shrink them."""

    max_image_bytes: int = 64 * MiB
    max_video_bytes: int = 1 * GiB
    max_session_bytes: int = 8 * GiB
    max_cache_bytes: int = 64 * GiB
    max_commit_bytes: int = 32 * MiB
    max_files_per_session: int = 100_000
    max_posts_per_commit: int = 5000
    max_media_per_post: int = 30
    max_comments_per_post: int = 200
    max_imports_history: int = 1000
    #: An upload session nobody sent a file to for this long is dropped with its staged files.
    stale_staging_seconds: float = 30 * 60
    #: Upload sessions open at once; beyond it the longest idle one is dropped (or 429).
    max_open_sessions: int = 8
    #: Uploads stop while less than this would stay free on the library's drive.
    min_free_bytes: int = 1 * GiB


class LibraryError(Exception):
    """A request the library refuses; `detail` is safe to show to the user."""

    def __init__(self, status_code: int, detail: str) -> None:
        super().__init__(detail)
        self.status_code = status_code
        self.detail = detail


# --------------------------------------------------------------------------------------------
# Media sniffing


@dataclass(frozen=True)
class MediaFormat:
    kind: MediaKind
    mime: str
    extension: str


JPEG = MediaFormat("image", "image/jpeg", "jpg")
PNG = MediaFormat("image", "image/png", "png")
GIF = MediaFormat("image", "image/gif", "gif")
WEBP = MediaFormat("image", "image/webp", "webp")
AVIF = MediaFormat("image", "image/avif", "avif")
BMP = MediaFormat("image", "image/bmp", "bmp")
MP4 = MediaFormat("video", "video/mp4", "mp4")
QUICKTIME = MediaFormat("video", "video/quicktime", "mov")
WEBM = MediaFormat("video", "video/webm", "webm")
FORMAT_BY_EXTENSION = {
    media.extension: media for media in (JPEG, PNG, GIF, WEBP, AVIF, BMP, MP4, QUICKTIME, WEBM)
}

_AVIF_BRANDS = frozenset({b"avif", b"avis"})
_HEVC_BRANDS = frozenset({b"heic", b"heix", b"hevc", b"hevx", b"heim", b"heis", b"hevm", b"hevs"})
_HEIF_GENERIC_BRANDS = frozenset({b"mif1", b"msf1"})
_BMP_HEADER_SIZES = frozenset({12, 40, 52, 56, 64, 108, 124})
_EBML_MAGIC = 0x1A45DFA3
_EBML_DOCTYPE = 0x4282


def _iso_bmff(head: bytes) -> MediaFormat | None:
    """ftyp brands: AVIF is an image, HEIC/HEIF is refused (the browser converts it), else video."""
    declared = int.from_bytes(head[0:4], "big")
    box_end = min(len(head), declared if declared >= 16 else len(head))
    major = head[8:12]
    compatible = {head[offset : offset + 4] for offset in range(16, box_end - 3, 4)}
    if major in _AVIF_BRANDS:
        return AVIF
    if major in _HEVC_BRANDS:
        return None
    if compatible & _AVIF_BRANDS:
        return AVIF
    if compatible & _HEVC_BRANDS or major in _HEIF_GENERIC_BRANDS:
        return None
    return QUICKTIME if major == b"qt  " else MP4


def _read_vint(data: bytes, position: int, *, keep_marker: bool) -> tuple[int, int] | None:
    """EBML variable-size integer at `position`: (value, length in bytes)."""
    if position >= len(data) or data[position] == 0:
        return None
    first = data[position]
    length = 9 - first.bit_length()
    if position + length > len(data):
        return None
    value = first if keep_marker else first & ((1 << (8 - length)) - 1)
    for byte in data[position + 1 : position + length]:
        value = (value << 8) | byte
    return value, length


def _webm(head: bytes) -> MediaFormat | None:
    """A WebM file is an EBML document whose DocType is "webm" (Matroska is not supported)."""
    magic = _read_vint(head, 0, keep_marker=True)
    if magic is None or magic[0] != _EBML_MAGIC:
        return None
    size = _read_vint(head, magic[1], keep_marker=False)
    if size is None:
        return None
    position = magic[1] + size[1]
    end = min(len(head), position + size[0])
    while position < end:
        element = _read_vint(head, position, keep_marker=True)
        if element is None:
            return None
        length = _read_vint(head, position + element[1], keep_marker=False)
        if length is None:
            return None
        position += element[1] + length[1]
        if element[0] == _EBML_DOCTYPE:
            doctype = head[position : position + length[0]].rstrip(b"\0")
            return WEBM if doctype == b"webm" else None
        position += length[0]
    return None


def sniff_media(head: bytes) -> MediaFormat | None:
    """Identifies an accepted upload from its first bytes; None for anything else (SVG, HTML ...)."""
    if head.startswith(b"\xff\xd8\xff"):
        return JPEG
    if head.startswith(b"\x89PNG\r\n\x1a\n"):
        return PNG
    if head[:6] in (b"GIF87a", b"GIF89a"):
        return GIF
    if len(head) >= 12 and head[:4] == b"RIFF" and head[8:12] == b"WEBP":
        return WEBP
    if len(head) >= 12 and head[4:8] == b"ftyp":
        return _iso_bmff(head)
    if head.startswith(b"\x1a\x45\xdf\xa3"):
        return _webm(head)
    if (
        len(head) >= 18
        and head[:2] == b"BM"
        and head[6:10] == b"\0\0\0\0"
        and int.from_bytes(head[14:18], "little") in _BMP_HEADER_SIZES
    ):
        return BMP
    return None


# --------------------------------------------------------------------------------------------
# Cleaning of the commit body (the browser already did this; the server does not trust it)


def _format_bytes(count: int) -> str:
    if count >= GiB:
        return f"{count / GiB:.1f}".removesuffix(".0") + " GB"
    if count >= MiB:
        return f"{count / MiB:.1f}".removesuffix(".0") + " MB"
    if count >= KiB:
        return f"{round(count / KiB)} KB"
    return f"{count} bytes"


def _shorten(text: str) -> str:
    return text if len(text) <= 80 else f"{text[:77]}..."


def clean_text(value: object, max_chars: int) -> str:
    """Trimmed text without NUL or unpaired surrogates, cut to max_chars."""
    if isinstance(value, bool) or not isinstance(value, (str, int, float)):
        return ""
    text = _LONE_SURROGATE_RE.sub("�", str(value).replace("\0", "")).strip()
    return text[:max_chars]


def clean_username(value: object) -> str | None:
    if isinstance(value, bool) or not isinstance(value, (str, int)):
        return None
    name = re.sub(r"[\x00-\x1f\x7f]", "", str(value)).strip().lstrip("@").strip()
    name = _LONE_SURROGATE_RE.sub("�", name)
    return name[:MAX_USERNAME_CHARS] or None


def clean_id(value: object) -> str | None:
    """Post ids end up in URLs and React keys: [A-Za-z0-9._-] only, like the frontend parser."""
    if isinstance(value, bool) or not isinstance(value, (str, int)):
        return None
    text = re.sub(r"[^A-Za-z0-9._-]+", "_", str(value).strip())[:MAX_ID_CHARS]
    return text if re.search(r"[A-Za-z0-9]", text) else None


def clean_count(value: object) -> int | None:
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        return None
    if not math.isfinite(value) or value < 0:
        return None
    return min(round(value), MAX_SAFE_INTEGER)


def safe_instagram_url(value: object) -> str | None:
    """An https link to Instagram's own pages, rebuilt without credentials or port; else None."""
    if not isinstance(value, str):
        return None
    raw = value.strip()
    if not raw or len(raw) > MAX_URL_CHARS or not re.match(r"https?://", raw, re.IGNORECASE):
        return None
    if _URL_FORBIDDEN_RE.search(raw):
        return None
    try:
        parts = urlsplit(raw)
        hostname = parts.hostname
    except ValueError:
        return None
    if parts.scheme.lower() not in ("http", "https") or hostname not in _INSTAGRAM_HOSTS:
        return None
    query = f"?{parts.query}" if parts.query else ""
    fragment = f"#{parts.fragment}" if parts.fragment else ""
    return f"https://{hostname}{parts.path}{query}{fragment}"


def is_instagram_url(value: object) -> bool:
    """True for a link on one of Instagram's own hosts."""
    return safe_instagram_url(value) is not None


def safe_web_url(value: object) -> str | None:
    """An http(s) link to any site, rebuilt without credentials; else None.

    Posts may come from anywhere, not only from Instagram. Links on Instagram's own hosts are
    returned as safe_instagram_url does (https, no port); other hosts keep their scheme and port.
    Mirrors sanitizeWebUrl in frontend/src/import/urls.ts.
    """
    instagram = safe_instagram_url(value)
    if instagram:
        return instagram
    if not isinstance(value, str):
        return None
    raw = value.strip()
    if not raw or len(raw) > MAX_URL_CHARS or not re.match(r"https?://", raw, re.IGNORECASE):
        return None
    if _URL_FORBIDDEN_RE.search(raw):
        return None
    try:
        parts = urlsplit(raw)
        hostname = parts.hostname
        port = parts.port
    except ValueError:
        return None
    scheme = parts.scheme.lower()
    if scheme not in ("http", "https") or not hostname:
        return None
    host = f"[{hostname}]" if ":" in hostname else hostname
    if port is not None:
        host = f"{host}:{port}"
    query = f"?{parts.query}" if parts.query else ""
    fragment = f"#{parts.fragment}" if parts.fragment else ""
    return f"{scheme}://{host}{parts.path}{query}{fragment}"


def _iso(moment: datetime) -> str:
    """The same shape as JavaScript's Date.toISOString()."""
    utc = moment.astimezone(timezone.utc)
    return f"{utc:%Y-%m-%dT%H:%M:%S}.{utc.microsecond // 1000:03d}Z"


def _now() -> str:
    return _iso(datetime.now(timezone.utc))


def _epoch_ms(value: float) -> float | None:
    if not math.isfinite(value) or value <= 0:
        return None
    if value < 1e11:
        return value * 1000  # seconds
    if value < 1e14:
        return value  # milliseconds
    if value < 1e17:
        return value / 1e3  # microseconds
    return value / 1e6  # nanoseconds


_EPOCH = datetime(1970, 1, 1, tzinfo=timezone.utc)


def parse_timestamp(value: object) -> str | None:
    """ISO 8601 (UTC, milliseconds, Z) from an ISO string or an epoch number; None when invalid."""
    milliseconds: float | None
    if isinstance(value, bool):
        return None
    if isinstance(value, (int, float)):
        milliseconds = _epoch_ms(float(value))
    elif isinstance(value, str):
        text = value.strip()
        if not text or len(text) > 64:
            return None
        if re.fullmatch(r"\d+(?:\.\d+)?", text):
            milliseconds = _epoch_ms(float(text))
        else:
            try:
                parsed = datetime.fromisoformat(text)
            except ValueError:
                return None
            if parsed.tzinfo is None:
                parsed = parsed.replace(tzinfo=timezone.utc)
            milliseconds = (parsed - _EPOCH) / timedelta(milliseconds=1)
    else:
        return None
    if milliseconds is None:
        return None
    try:
        moment = _EPOCH + timedelta(milliseconds=round(milliseconds))
    except OverflowError:
        return None
    return _iso(moment) if 1971 <= moment.year <= 9999 else None


def _sort_key(post: JsonObject) -> tuple[int, float]:
    taken_at = post.get("taken_at")
    if isinstance(taken_at, str):
        try:
            moment = datetime.fromisoformat(taken_at)
        except ValueError:
            pass
        else:
            if moment.tzinfo is None:
                moment = moment.replace(tzinfo=timezone.utc)
            return (0, -moment.timestamp())
    return (1, 0.0)


def sort_posts(posts: Iterable[JsonObject]) -> list[JsonObject]:
    """Newest first; undated posts after the dated ones, in their original order (stable)."""
    return sorted(posts, key=_sort_key)


def safe_stem(value: str, fallback: str) -> str:
    """A file name stem of [A-Za-z0-9_-], at most MAX_STEM_CHARS, never a Windows device name."""
    stem = re.sub(r"[^A-Za-z0-9_-]+", "_", value).strip("_")[: MAX_STEM_CHARS - 1] or fallback
    return f"{stem}_" if stem.casefold() in _WINDOWS_RESERVED_NAMES else stem


def _unique_stem(base: str, taken: set[str]) -> str:
    """base, base-2, base-3 ... compared without letter case (Windows file names ignore it)."""
    candidate = base
    number = 2
    while candidate.casefold() in taken:
        suffix = f"-{number}"
        candidate = f"{base[: MAX_STEM_CHARS - len(suffix)]}{suffix}"
        number += 1
    taken.add(candidate.casefold())
    return candidate


# --------------------------------------------------------------------------------------------
# The commit body


@dataclass(frozen=True)
class StagedFile:
    name: str
    size: int
    media: MediaFormat


@dataclass(frozen=True)
class _NewMedia:
    staged: StagedFile
    poster: StagedFile | None
    alt: str | None


@dataclass(frozen=True)
class _NewPost:
    id: str
    media: list[_NewMedia]
    media_type: str
    user: JsonObject
    fields: JsonObject


@dataclass(frozen=True)
class _Commit:
    source_name: str
    source_kind: SourceKind
    exported_at: str | None
    source_url: str | None
    posts: list[_NewPost]
    avatars: dict[str, StagedFile]


def _first_string(source: JsonObject, keys: tuple[str, ...]) -> str | None:
    for key in keys:
        value = source.get(key)
        if isinstance(value, str) and value.strip():
            return value.strip()
    return None


def _string_list(value: object) -> list[str]:
    if isinstance(value, str):
        return [value.strip()] if value.strip() else []
    if isinstance(value, list):
        return [item.strip() for item in value if isinstance(item, str) and item.strip()]
    return []


class _CommitParser:
    def __init__(self, files: dict[str, StagedFile], limits: LibraryLimits) -> None:
        self.files = files
        self.limits = limits

    def staged(self, value: str, where: str) -> StagedFile:
        staged = self.files.get(value) if STAGED_NAME_RE.fullmatch(value) else None
        if staged is None:
            raise LibraryError(422, f'{where}: "{_shorten(value)}" is not a file of this upload.')
        return staged

    def media_items(self, raw: JsonObject) -> list[tuple[str, str | None, str | None]]:
        """(path, poster, alt) triples: `media` wins over image/images/video/videos."""
        media = raw.get("media")
        if isinstance(media, list) and media:
            items: list[tuple[str, str | None, str | None]] = []
            for item in media:
                if isinstance(item, str):
                    items.append((item.strip(), None, None))
                elif isinstance(item, dict):
                    path = _first_string(item, ("path", "file", "src", "url")) or ""
                    poster = _first_string(item, ("poster", "thumbnail", "cover"))
                    alt = clean_text(item.get("alt"), MAX_ALT_CHARS) or None
                    items.append((path, poster, alt))
                else:
                    items.append(("", None, None))
            return items
        images = _string_list(raw.get("images"))
        cover = raw.get("image")
        if isinstance(cover, str) and cover.strip() and cover.strip() not in images:
            images.insert(0, cover.strip())
        videos = _string_list(raw.get("videos")) + _string_list(raw.get("video"))
        poster = _first_string(raw, ("poster", "thumbnail", "cover"))
        return [(path, None, None) for path in images] + [
            (path, poster if position == 0 else None, None) for position, path in enumerate(videos)
        ]

    def media(self, raw: JsonObject, where: str) -> list[_NewMedia]:
        result: list[_NewMedia] = []
        seen: set[str] = set()
        for path, poster_name, alt in self.media_items(raw):
            if path in seen:
                continue
            seen.add(path)
            staged = self.staged(path, where)
            poster: StagedFile | None = None
            if poster_name is not None and staged.media.kind == "video":
                poster = self.staged(poster_name, where)
                if poster.media.kind != "image":
                    raise LibraryError(422, f'{where}: the poster "{poster_name}" is not an image.')
            result.append(_NewMedia(staged, poster, alt))
        if not result:
            raise LibraryError(422, f"{where} has no media.")
        if len(result) > self.limits.max_media_per_post:
            raise LibraryError(
                422, f"{where} has {len(result)} media; at most {self.limits.max_media_per_post} are allowed."
            )
        return result

    @staticmethod
    def user(raw: JsonObject, from_instagram: bool = True) -> JsonObject:
        value = raw.get("user")
        details: JsonObject = value if isinstance(value, dict) else {}
        username = clean_username(details.get("username") if details else value) or "unknown"
        user: JsonObject = {"username": username}
        full_name = clean_text(details.get("full_name"), MAX_NAME_CHARS)
        if full_name:
            user["full_name"] = full_name
        verified = details.get("verified")
        if isinstance(verified, bool):
            user["verified"] = verified
        profile_url = safe_web_url(details.get("profile_url"))
        if profile_url is None and from_instagram and _USERNAME_RE.fullmatch(username):
            # No usable link: a post from Instagram (or without a link) gets the profile of the same name.
            profile_url = f"https://www.instagram.com/{username}/"
        if profile_url:
            user["profile_url"] = profile_url
        return user

    def comments(self, value: object) -> list[JsonObject]:
        comments: list[JsonObject] = []
        for item in value if isinstance(value, list) else []:
            if len(comments) >= self.limits.max_comments_per_post:
                break
            if not isinstance(item, dict):
                continue
            text = clean_text(item.get("text"), MAX_COMMENT_CHARS)
            if not text:
                continue
            # v1 names the author "username"; "user" (a name or {username}) is read like the frontend parser does.
            author = item.get("username", item.get("user"))
            if isinstance(author, dict):
                author = author.get("username")
            comment: JsonObject = {
                "username": clean_username(author) or "unknown",
                "text": text,
                "like_count": clean_count(item.get("like_count")) or 0,
            }
            taken_at = parse_timestamp(item.get("taken_at"))
            if taken_at:
                comment["taken_at"] = taken_at
            comments.append(comment)
        return comments

    def post(self, raw: object, index: int) -> _NewPost:
        where = f"Post {index + 1}"
        if not isinstance(raw, dict):
            raise LibraryError(422, f"{where} is not a JSON object.")
        post_id = clean_id(raw.get("id"))
        if post_id is None:
            raise LibraryError(422, f"{where} has no usable id.")
        media = self.media(raw, f"{where} ({post_id})")

        declared = raw.get("media_type")
        if declared in ("video", "carousel"):
            media_type = str(declared)
        elif len(media) > 1:
            media_type = "carousel"
        else:
            media_type = media[0].staged.media.kind

        fields: JsonObject = {}
        url = safe_web_url(raw.get("url"))
        if url:
            fields["url"] = url
        fields["caption"] = clean_text(raw.get("caption"), MAX_CAPTION_CHARS)
        fields["comment_count"] = clean_count(raw.get("comment_count")) or 0
        fields["like_count"] = clean_count(raw.get("like_count"))
        taken_at = parse_timestamp(raw.get("taken_at"))
        if taken_at:
            fields["taken_at"] = taken_at
        alt = clean_text(raw.get("alt"), MAX_ALT_CHARS)
        if alt:
            fields["alt"] = alt
        location = clean_text(raw.get("location"), MAX_LOCATION_CHARS)
        if location:
            fields["location"] = location
        comments = self.comments(raw.get("comments"))
        if comments:
            fields["comments"] = comments
        return _NewPost(post_id, media, media_type, self.user(raw, url is None or is_instagram_url(url)), fields)

    def avatars(self, value: object) -> dict[str, StagedFile]:
        if value is None:
            return {}
        if not isinstance(value, dict):
            raise LibraryError(422, '"avatars" must map usernames to uploaded file names.')
        avatars: dict[str, StagedFile] = {}
        for key, name in value.items():
            username = clean_username(key)
            if username is None or not isinstance(name, str):
                raise LibraryError(422, f'The avatar entry "{_shorten(str(key))}" is not valid.')
            staged = self.staged(name, f'The avatar of "{username}"')
            if staged.media.kind != "image":
                raise LibraryError(422, f'The avatar of "{username}" is not an image.')
            avatars[username.casefold()] = staged
        return avatars

    def parse(self, body: bytes) -> _Commit:
        try:
            raw = json.loads(body)
        except ValueError as error:
            raise LibraryError(422, f"The commit body is not valid JSON: {error}") from error
        if not isinstance(raw, dict):
            raise LibraryError(422, "The commit body must be a JSON object.")
        source_kind = raw.get("source_kind")
        if source_kind not in ("archive", "folder", "multiple"):
            raise LibraryError(422, 'source_kind must be "archive", "folder" or "multiple".')
        posts = raw.get("posts")
        if not isinstance(posts, list) or not posts:
            raise LibraryError(422, "The commit lists no posts.")
        if len(posts) > self.limits.max_posts_per_commit:
            raise LibraryError(
                422,
                f"The commit lists {len(posts)} posts; at most "
                f"{self.limits.max_posts_per_commit} can be saved at once.",
            )
        # Later entries win on the same id; the first one keeps its position (like a JS Map).
        unique: dict[str, _NewPost] = {}
        for index, entry in enumerate(posts):
            post = self.post(entry, index)
            unique[post.id] = post
        return _Commit(
            source_name=clean_text(raw.get("source_name"), MAX_SOURCE_NAME_CHARS) or "Import",
            source_kind=source_kind,
            exported_at=parse_timestamp(raw.get("exported_at")),
            source_url=safe_web_url(raw.get("source_url")),
            posts=list(unique.values()),
            avatars=self.avatars(raw.get("avatars")),
        )


#: Fields of a library post the user may change after the import.
EDITABLE_POST_FIELDS = ("caption",)


def parse_post_changes(body: bytes) -> JsonObject:
    """The changes of a post edit: {"caption": "..."}. Text that is too long is refused (422),
    never cut, so an edit is saved exactly as written (apart from the trimmed ends)."""
    try:
        raw = json.loads(body)
    except ValueError as error:
        raise LibraryError(422, f"The request body is not valid JSON: {error}") from error
    if not isinstance(raw, dict):
        raise LibraryError(422, "The request body must be a JSON object.")
    unknown = sorted(str(key) for key in raw if key not in EDITABLE_POST_FIELDS)
    if unknown:
        raise LibraryError(422, f'"{_shorten(unknown[0])}" cannot be changed; only the caption can.')
    if "caption" not in raw:
        raise LibraryError(422, "The request changes nothing: send a caption.")
    caption = raw["caption"]
    if not isinstance(caption, str):
        raise LibraryError(422, "The caption must be a string.")
    if len(caption) > MAX_CAPTION_CHARS:
        raise LibraryError(422, f"A caption can be at most {MAX_CAPTION_CHARS:,} characters long.")
    return {"caption": clean_text(caption, MAX_CAPTION_CHARS)}


# --------------------------------------------------------------------------------------------
# Files on disk


def _is_link(path: Path) -> bool:
    return path.is_symlink() or path.is_junction()


def _remove_tree(path: Path) -> None:
    """Best effort: removes a file, folder or link (never what a link points to). What a reader
    still holds open on Windows stays behind and is picked up by a later sweep or purge."""
    try:
        if _is_link(path):
            if path.is_junction():
                os.rmdir(path)
            else:
                path.unlink()
        elif path.is_dir():
            failures: list[str] = []
            shutil.rmtree(path, onexc=lambda _function, name, _error: failures.append(name))
            if failures:
                LOGGER.warning("Could not remove %d item(s) below %s", len(failures), path)
        else:
            path.unlink(missing_ok=True)
    except OSError as error:
        LOGGER.warning("Could not remove %s: %s", path, error)


def _scandir(directory: Path) -> list[os.DirEntry[str]]:
    """The entries of a real folder; nothing for a missing folder or a link (never followed)."""
    if _is_link(directory):
        return []
    try:
        with os.scandir(directory) as entries:
            return list(entries)
    except FileNotFoundError:
        return []


def _open_for_write(path: Path) -> BinaryIO:
    return open(path, "wb")


def _replace(source: Path, target: Path) -> None:
    """os.replace, retried briefly: on Windows a reader (an antivirus, an editor) can hold the target."""
    for attempt in range(5):
        try:
            os.replace(source, target)
            return
        except PermissionError:
            if attempt == 4:
                raise
            time.sleep(0.05 * (attempt + 1))


def _write_manifest(root: Path, document: JsonObject) -> None:
    """Writes post.json completely, then swaps it in, so a reader never sees half a file."""
    descriptor, temporary_name = tempfile.mkstemp(
        prefix=_TEMPORARY_PREFIX, suffix=_TEMPORARY_SUFFIX, dir=root
    )
    temporary = Path(temporary_name)
    try:
        with os.fdopen(descriptor, "w", encoding="utf-8", newline="\n") as handle:
            json.dump(document, handle, ensure_ascii=False, indent=2)
            handle.write("\n")
            handle.flush()
            os.fsync(handle.fileno())
        _replace(temporary, root / MANIFEST_NAME)
    finally:
        temporary.unlink(missing_ok=True)


_MEDIA_PATH_KEYS = ("path", "file", "src", "url", "poster", "thumbnail", "cover")
_POST_PATH_KEYS = ("image", "images", "video", "videos", "poster", "thumbnail", "cover")


def _normal_path(value: str) -> str:
    path = value.strip().replace("\\", "/")
    while path.startswith("./"):
        path = path[2:]
    return path


def _library_paths(post: JsonObject) -> Iterable[str]:
    """Every file path a post references, in every spelling a v1 export may use (media items as
    strings or objects with path/file/src/url and poster/thumbnail/cover, image/images,
    video/videos, a post-level poster, the user's avatar). The library itself writes only some
    of them; reading all of them keeps a sweep from ever treating a referenced file as unused."""
    media = post.get("media")
    for item in media if isinstance(media, list) else []:
        if isinstance(item, str):
            yield _normal_path(item)
        elif isinstance(item, dict):
            for key in _MEDIA_PATH_KEYS:
                value = item.get(key)
                if isinstance(value, str):
                    yield _normal_path(value)
    for key in _POST_PATH_KEYS:
        value = post.get(key)
        for single in value if isinstance(value, list) else [value]:
            if isinstance(single, str):
                yield _normal_path(single)
    user = post.get("user")
    if isinstance(user, dict) and isinstance(user.get("avatar"), str):
        yield _normal_path(user["avatar"])


def _is_library_document(raw: object) -> bool:
    """A post.json the library wrote: source.type "library" and a "library" object."""
    if not isinstance(raw, dict) or not isinstance(raw.get("library"), dict):
        return False
    source = raw.get("source")
    return isinstance(source, dict) and source.get("type") == "library"


def _remove_if_empty(directory: Path) -> None:
    try:
        os.rmdir(directory)
    except OSError:
        pass


def _free_bytes(path: Path) -> int | None:
    try:
        return shutil.disk_usage(path).free
    except OSError:
        return None


class _FolderInUse(Exception):
    """Another process holds the library folder's lock."""


def _lock_marker(path: Path) -> int:
    """Opens (creating) the marker and locks it without waiting: _FolderInUse when another
    process holds it, OSError when the file cannot be opened. The lock goes with the process,
    also when it crashes."""
    descriptor = os.open(path, os.O_RDWR | os.O_CREAT | getattr(os, "O_BINARY", 0), 0o644)
    try:
        try:
            if os.name == "nt":
                import msvcrt

                os.lseek(descriptor, _LOCK_OFFSET, os.SEEK_SET)
                msvcrt.locking(descriptor, msvcrt.LK_NBLCK, 1)
            else:
                import fcntl

                fcntl.flock(descriptor, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except OSError as error:
            raise _FolderInUse(str(error)) from error
        if os.fstat(descriptor).st_size == 0:
            os.lseek(descriptor, 0, os.SEEK_SET)
            os.write(descriptor, _MARKER_TEXT)
    except BaseException:
        os.close(descriptor)
        raise
    return descriptor


def _unlock_marker(descriptor: int) -> None:
    try:
        if os.name == "nt":
            import msvcrt

            os.lseek(descriptor, _LOCK_OFFSET, os.SEEK_SET)
            msvcrt.locking(descriptor, msvcrt.LK_UNLCK, 1)
    except OSError:
        pass
    finally:
        os.close(descriptor)


def _stems(posts: Iterable[JsonObject], folder: str, pattern: re.Pattern[str]) -> set[str]:
    stems: set[str] = set()
    for post in posts:
        for path in _library_paths(post):
            directory, _, name = path.partition("/")
            match = pattern.fullmatch(name) if directory == folder else None
            if match:
                stems.add(match["stem"].casefold())
    return stems


def _stems_on_disk(directory: Path, pattern: re.Pattern[str]) -> set[str]:
    stems: set[str] = set()
    for entry in _scandir(directory):
        match = pattern.fullmatch(entry.name)
        if match:
            stems.add(match["stem"].casefold())
    return stems


@dataclass(frozen=True)
class _Document:
    revision: int
    updated_at: str | None
    imports: list[JsonObject]
    posts: list[JsonObject]


def _empty_document(revision: int) -> _Document:
    return _Document(revision=revision, updated_at=None, imports=[], posts=[])


def _parse_document(raw: object, path: Path) -> _Document:
    damaged = LibraryError(
        503, f"The library file {path} is damaged. Clear the library to start again."
    )
    if not isinstance(raw, dict) or not isinstance(raw.get("posts"), list):
        raise damaged
    meta = raw.get("library")
    meta = meta if isinstance(meta, dict) else {}
    revision = meta.get("revision")
    updated_at = meta.get("updated_at")
    imports = meta.get("imports")
    return _Document(
        revision=revision if isinstance(revision, int) and not isinstance(revision, bool) and revision >= 0 else 0,
        updated_at=updated_at if isinstance(updated_at, str) else None,
        imports=[item for item in imports if isinstance(item, dict)] if isinstance(imports, list) else [],
        posts=[post for post in raw["posts"] if isinstance(post, dict)],
    )


# --------------------------------------------------------------------------------------------
# Upload sessions


@dataclass
class _Session:
    upload_id: str
    directory: Path
    files: dict[str, StagedFile] = field(default_factory=dict)
    writing: set[str] = field(default_factory=set)
    #: Bytes counted against the session and cache limits (kept files + bytes being written).
    reserved: int = 0
    state: Literal["open", "committing", "closed"] = "open"
    touched: float = field(default_factory=time.monotonic)


#: Commits, deletions and clears of every library in this process run one at a time.
_PROCESS_LOCK = threading.RLock()


class Library:
    """The library in one cache folder. The folder is resolved and prepared on first use."""

    def __init__(
        self, resolve_directory: Callable[[], Path], limits: LibraryLimits | None = None
    ) -> None:
        self.limits = limits or LibraryLimits()
        self._resolve_directory = resolve_directory
        self._root: Path | None = None
        self._cached: tuple[tuple[int, int, int], _Document] | None = None
        #: Revisions only grow, also across a clear, so file URLs (?r=<revision>) never repeat.
        self._revision_floor = 0
        self._library_bytes = 0
        self._staged_bytes = 0
        self._sessions: dict[str, _Session] = {}
        self._sessions_lock = threading.Lock()
        #: The locked marker file while this library uses its folder.
        self._lock_descriptor: int | None = None

    # -- folder --------------------------------------------------------------------------

    def _ready_root(self, *, refuse_links: bool = True) -> Path:
        """The usable cache folder; LibraryError(503) when it cannot be used."""
        root = self._root
        if root is None:
            try:
                root = Path(self._resolve_directory()).expanduser().resolve()
            except (OSError, RuntimeError, ValueError) as error:
                raise LibraryError(503, f"The library folder is not configured correctly: {error}") from error
        try:
            root.mkdir(parents=True, exist_ok=True)
            if refuse_links:
                for name in (POSTS_FOLDER, AVATARS_FOLDER, STAGING_FOLDER):
                    if _is_link(root / name):
                        raise LibraryError(
                            503, f"{root / name} is a link; the library only uses real folders."
                        )
            if self._root is None:
                self._claim(root)
                try:
                    self._prepare(root)
                except BaseException:
                    self._release_claim()
                    raise
                self._root = root
        except OSError as error:
            raise LibraryError(503, f"The library folder {root} cannot be used: {error}") from error
        return root

    @staticmethod
    def _foreign_entry(root: Path) -> str | None:
        """The first thing in a folder without the marker that the library did not create, or
        None when the library may adopt the folder (empty, or holding a library post.json)."""
        marker = root / MARKER_NAME
        if _is_link(marker):
            return MARKER_NAME
        if marker.is_file():
            return None
        manifest = root / MANIFEST_NAME
        if manifest.exists() or _is_link(manifest):
            try:
                raw = json.loads(manifest.read_bytes())
            except (OSError, ValueError):
                raw = None
            return None if _is_library_document(raw) else MANIFEST_NAME
        for entry in _scandir(root):
            name = entry.name
            if name.startswith(_PROBE_PREFIX) or (
                name.startswith(_TEMPORARY_PREFIX) and name.endswith(_TEMPORARY_SUFFIX)
            ):
                continue
            if _is_link(Path(entry.path)):
                return name
            if name in (POSTS_FOLDER, AVATARS_FOLDER, STAGING_FOLDER) and entry.is_dir(follow_symlinks=False):
                allowed = UPLOAD_ID_RE if name == STAGING_FOLDER else None
                inside = [
                    item.name
                    for item in _scandir(Path(entry.path))
                    if allowed is None or not allowed.fullmatch(item.name)
                ]
                if not inside:
                    continue
                return f"{name}/{inside[0]}"
            return name
        return None

    def _claim(self, root: Path) -> None:
        """Takes the folder for this library: refuses a folder holding files it did not create
        and a folder another running process uses (503), then locks the marker."""
        foreign = self._foreign_entry(root)
        if foreign is not None:
            raise LibraryError(
                503,
                f"The library folder {root} already holds files this app did not create "
                f'("{_shorten(foreign)}", for example an extracted export). To keep them safe the '
                "library only uses an empty folder or one it created itself: choose another folder "
                'with INSTAGRAM_CACHE_DIR or "instagram_cache_directory" in settings.json, or move '
                "those files elsewhere.",
            )
        try:
            self._lock_descriptor = _lock_marker(root / MARKER_NAME)
        except _FolderInUse as error:
            raise LibraryError(
                503,
                f"The library folder {root} is in use by another running copy of this app. Stop "
                "that copy, or give this one its own folder with INSTAGRAM_CACHE_DIR or "
                f'"instagram_cache_directory" in settings.json. ({error})',
            ) from error

    def _release_claim(self) -> None:
        descriptor, self._lock_descriptor = self._lock_descriptor, None
        if descriptor is not None:
            _unlock_marker(descriptor)

    def close(self) -> None:
        """Ends this library's use of its folder: drops open uploads and unlocks the folder."""
        with _PROCESS_LOCK:
            with self._sessions_lock:
                sessions = list(self._sessions.values())
            for session in sessions:
                self._close_session(session)
            self._release_claim()
            self._root = None
            self._cached = None

    def _prepare(self, root: Path) -> None:
        """First use in this process: prove the folder is writable, then tidy it up."""
        descriptor, probe = tempfile.mkstemp(prefix=_PROBE_PREFIX, dir=root)
        os.close(descriptor)
        os.unlink(probe)
        # Sessions live in memory only and this process holds the folder's lock: whatever is
        # staged belongs to an earlier run and can never be committed.
        for entry in _scandir(root / STAGING_FOLDER):
            if UPLOAD_ID_RE.fullmatch(entry.name):
                _remove_tree(Path(entry.path))
        try:
            document = self._load(root)
        except LibraryError:
            LOGGER.warning("The library in %s is damaged; it is left as it is.", root)
        else:
            self._sweep(root, document)
        self._library_bytes = self._measure(root)

    def _purge_stale_staging(self, root: Path) -> None:
        """Drops sessions nobody sent a file to for a while, and staging folders without a
        session (a session whose folder a reader held open when it closed)."""
        cutoff = time.monotonic() - self.limits.stale_staging_seconds
        with self._sessions_lock:
            stale = [
                session
                for session in self._sessions.values()
                if session.state == "open" and not session.writing and session.touched < cutoff
            ]
        for session in stale:
            self._close_session(session)
        with self._sessions_lock:
            live = set(self._sessions)
        for entry in _scandir(root / STAGING_FOLDER):
            if entry.name not in live and UPLOAD_ID_RE.fullmatch(entry.name):
                _remove_tree(Path(entry.path))

    @staticmethod
    def _measure(root: Path) -> int:
        total = 0
        for folder in (POSTS_FOLDER, AVATARS_FOLDER):
            for entry in _scandir(root / folder):
                try:
                    if entry.is_file(follow_symlinks=False):
                        total += entry.stat(follow_symlinks=False).st_size
                except OSError:
                    continue
        return total

    def _sweep(self, root: Path, document: _Document) -> None:
        """Deletes library files post.json no longer references (replaced or deleted posts,
        files a reader held open during an earlier delete, leftovers of an interrupted commit)."""
        referenced = {path.casefold() for post in document.posts for path in _library_paths(post)}
        for folder, pattern in ((POSTS_FOLDER, POST_FILE_RE), (AVATARS_FOLDER, AVATAR_FILE_RE)):
            for entry in _scandir(root / folder):
                if not pattern.fullmatch(entry.name) or entry.is_dir(follow_symlinks=False):
                    continue
                if f"{folder}/{entry.name}".casefold() not in referenced:
                    _remove_tree(Path(entry.path))
        for entry in _scandir(root):
            if entry.name.startswith(_TEMPORARY_PREFIX) and entry.name.endswith(_TEMPORARY_SUFFIX):
                _remove_tree(Path(entry.path))

    # -- post.json -----------------------------------------------------------------------

    def _load(self, root: Path) -> _Document:
        path = root / MANIFEST_NAME
        try:
            info = path.stat()
        except FileNotFoundError:
            return _empty_document(self._revision_floor)
        signature = (info.st_mtime_ns, info.st_size, info.st_ino)
        if self._cached is not None and self._cached[0] == signature:
            return self._cached[1]
        try:
            raw = json.loads(path.read_bytes())
        except ValueError as error:
            raise LibraryError(
                503, f"The library file {path} is damaged ({error}). Clear the library to start again."
            ) from error
        document = _parse_document(raw, path)
        self._cached = (signature, document)
        return document

    def _save(self, root: Path, document: _Document) -> None:
        _write_manifest(
            root,
            {
                "format": "instagram-export",
                "version": 1,
                "exported_at": document.updated_at,
                "source": {"type": "library"},
                "library": {
                    "revision": document.revision,
                    "updated_at": document.updated_at,
                    "imports": document.imports,
                },
                "posts": document.posts,
            },
        )
        info = (root / MANIFEST_NAME).stat()
        self._cached = ((info.st_mtime_ns, info.st_size, info.st_ino), document)

    @staticmethod
    def _response(root: Path, document: _Document) -> JsonObject:
        return {
            "directory": str(root),
            "revision": document.revision,
            "updated_at": document.updated_at,
            "imports": document.imports,
            "posts": document.posts,
        }

    def _next_revision(self, document: _Document) -> int:
        return max(document.revision, self._revision_floor) + 1

    # -- public API ----------------------------------------------------------------------

    def snapshot(self) -> JsonObject:
        with _PROCESS_LOCK:
            root = self._ready_root()
            return self._response(root, self._load(root))

    def media_file(self, relative_path: str) -> tuple[Path, MediaFormat, os.stat_result]:
        """A file below posts/ or avatars/ that the library could have written; else 404."""
        not_found = LibraryError(404, "No such library file.")
        folder, _, name = relative_path.partition("/")
        pattern = {POSTS_FOLDER: POST_FILE_RE, AVATARS_FOLDER: AVATAR_FILE_RE}.get(folder)
        if pattern is None or not pattern.fullmatch(name):
            raise not_found
        root = self._root
        if root is None:
            with _PROCESS_LOCK:
                root = self._ready_root()
        directory = root / folder
        path = directory / name
        try:
            if _is_link(directory):
                raise not_found
            # lstat: a link (even one to a file inside the library) is never served.
            info = os.stat(path, follow_symlinks=False)
        except OSError as error:
            raise not_found from error
        if not stat.S_ISREG(info.st_mode):
            raise not_found
        return path, FORMAT_BY_EXTENSION[name.rsplit(".", 1)[1]], info

    def create_upload(self) -> str:
        with _PROCESS_LOCK:
            root = self._ready_root()
            self._purge_stale_staging(root)
            self._make_room_for_a_session()
            upload_id = uuid.uuid4().hex
            directory = root / STAGING_FOLDER / upload_id
            try:
                directory.mkdir(parents=True)
            except OSError as error:
                raise LibraryError(503, f"Cannot start an upload in {root}: {error}") from error
            with self._sessions_lock:
                self._sessions[upload_id] = _Session(upload_id, directory)
            return upload_id

    async def receive_file(
        self,
        upload_id: str,
        name: str,
        body: AsyncIterator[bytes],
        content_length: int | None,
    ) -> JsonObject:
        """Streams one upload body to disk, checking its type and every size limit on the way."""
        if not STAGED_NAME_RE.fullmatch(name):
            raise LibraryError(422, f'"{_shorten(name)}" is not a valid upload file name.')
        largest = max(self.limits.max_image_bytes, self.limits.max_video_bytes)
        if content_length is not None and content_length > largest:
            raise LibraryError(413, f"A file can be at most {_format_bytes(largest)}.")
        session = self._start_write(upload_id, name)
        part = session.directory / f"{name}.part"
        handle: BinaryIO | None = None
        owned = 0  # bytes reserved for this file, released again unless the file is kept
        checked = 0  # `owned` when free disk space was last checked
        try:
            self._check_free_space(session.directory, content_length or 0)
            handle = await anyio.to_thread.run_sync(_open_for_write, part)
            head = bytearray()
            media: MediaFormat | None = None
            pending = bytearray()
            async for chunk in body:
                if media is None:
                    head += chunk
                    if len(head) < SNIFF_BYTES:
                        continue
                    media = self._accept(bytes(head))
                    chunk = bytes(head)
                self._reserve(session, media, owned, len(chunk))
                owned += len(chunk)
                pending += chunk
                if len(pending) >= WRITE_BUFFER_BYTES:
                    if owned - checked >= FREE_SPACE_CHECK_BYTES:
                        self._check_free_space(session.directory, len(pending))
                        checked = owned
                    await anyio.to_thread.run_sync(handle.write, bytes(pending))
                    pending.clear()
            if media is None:
                media = self._accept(bytes(head))
                self._reserve(session, media, owned, len(head))
                owned += len(head)
                pending += head
            if pending:
                await anyio.to_thread.run_sync(handle.write, bytes(pending))
            await anyio.to_thread.run_sync(handle.close)
            handle = None
            await anyio.to_thread.run_sync(_replace, part, session.directory / name)
            size = owned
            self._keep(session, StagedFile(name, size, media))
            owned = 0
            return {"name": name, "bytes": size, "mime": media.mime, "kind": media.kind}
        finally:
            if handle is not None:
                handle.close()
            part.unlink(missing_ok=True)
            self._end_write(session, name, owned)

    def abort_upload(self, upload_id: str) -> None:
        if not UPLOAD_ID_RE.fullmatch(upload_id):
            return
        with self._sessions_lock:
            session = self._sessions.get(upload_id)
            if session is not None and session.state == "committing":
                raise LibraryError(409, "The upload is being saved and can no longer be cancelled.")
        if session is not None:
            self._close_session(session)
            return
        # Not known to this process (a previous run): still remove what it left behind.
        root = self._root
        if root is not None:
            _remove_tree(root / STAGING_FOLDER / upload_id)

    def commit(self, upload_id: str, body: bytes) -> JsonObject:
        with _PROCESS_LOCK:
            root = self._ready_root()
            document = self._load(root)
            session = self._begin_commit(upload_id)
            try:
                commit = _CommitParser(session.files, self.limits).parse(body)
            except BaseException:
                with self._sessions_lock:
                    session.state = "open"
                raise
            try:
                updated = self._apply(root, session, document, commit)
            except OSError as error:
                raise LibraryError(503, f"Could not save the import to {root}: {error}") from error
            finally:
                self._close_session(session)
            self._sweep(root, updated)
            self._library_bytes = self._measure(root)
            return self._response(root, updated)

    def delete_post(self, post_id: str) -> JsonObject:
        with _PROCESS_LOCK:
            root = self._ready_root()
            document = self._load(root)
            posts = [post for post in document.posts if post.get("id") != post_id]
            if len(posts) == len(document.posts):
                raise LibraryError(404, "The library has no post with this id.")
            updated = _Document(
                revision=self._next_revision(document),
                updated_at=_now(),
                imports=document.imports,
                posts=posts,
            )
            try:
                self._save(root, updated)
            except OSError as error:
                raise LibraryError(503, f"Could not update {root / MANIFEST_NAME}: {error}") from error
            self._sweep(root, updated)
            self._library_bytes = self._measure(root)
            return self._response(root, updated)

    def update_post(self, post_id: str, changes: JsonObject) -> JsonObject:
        """Applies `changes` (see parse_post_changes) to one post and saves post.json. The post
        keeps its place and its files; an edit that changes nothing writes nothing."""
        with _PROCESS_LOCK:
            root = self._ready_root()
            document = self._load(root)
            index = next(
                (position for position, post in enumerate(document.posts) if post.get("id") == post_id),
                None,
            )
            if index is None:
                raise LibraryError(404, "The library has no post with this id.")
            current = document.posts[index]
            if all(current.get(key) == value for key, value in changes.items()):
                return self._response(root, document)
            posts = list(document.posts)
            posts[index] = {**current, **changes}
            updated = _Document(
                revision=self._next_revision(document),
                updated_at=_now(),
                imports=document.imports,
                posts=posts,
            )
            try:
                self._save(root, updated)
            except OSError as error:
                raise LibraryError(503, f"Could not update {root / MANIFEST_NAME}: {error}") from error
            return self._response(root, updated)

    def clear(self) -> None:
        """Removes what the library wrote (post.json, its media and avatar files, staged
        uploads) and nothing else; posts/, avatars/ and .staging/ go only once they are empty.
        A folder the library does not own is refused by _ready_root (503), never cleared."""
        with _PROCESS_LOCK:
            root = self._ready_root(refuse_links=False)
            try:
                revision = self._load(root).revision
            except LibraryError:
                revision = self._revision_floor
            with self._sessions_lock:
                sessions = list(self._sessions.values())
            for session in sessions:
                self._close_session(session)
            try:
                (root / MANIFEST_NAME).unlink(missing_ok=True)
            except OSError as error:
                raise LibraryError(503, f"Could not remove {root / MANIFEST_NAME}: {error}") from error
            self._cached = None
            self._revision_floor = max(revision, self._revision_floor) + 1
            # Files a reader still holds open stay behind; the next sweep removes them.
            for folder, pattern in (
                (POSTS_FOLDER, POST_FILE_RE),
                (AVATARS_FOLDER, AVATAR_FILE_RE),
                (STAGING_FOLDER, UPLOAD_ID_RE),
            ):
                directory = root / folder
                if _is_link(directory):
                    _remove_tree(directory)  # the link only, never its target
                    continue
                for entry in _scandir(directory):
                    if pattern.fullmatch(entry.name) and (
                        folder == STAGING_FOLDER or not entry.is_dir(follow_symlinks=False)
                    ):
                        _remove_tree(Path(entry.path))
                _remove_if_empty(directory)
            self._sweep(root, _empty_document(self._revision_floor))
            self._library_bytes = self._measure(root)

    # -- sessions ------------------------------------------------------------------------

    def _session(self, upload_id: str) -> _Session:
        session = self._sessions.get(upload_id) if UPLOAD_ID_RE.fullmatch(upload_id) else None
        if session is None:
            raise LibraryError(404, "No such upload; start a new one.")
        return session

    def _start_write(self, upload_id: str, name: str) -> _Session:
        with self._sessions_lock:
            session = self._session(upload_id)
            if session.state != "open":
                raise LibraryError(409, "This upload is being saved and accepts no more files.")
            if name in session.writing:
                raise LibraryError(409, f"{name} is already being uploaded.")
            if name not in session.files and (
                len(session.files) + len(session.writing) >= self.limits.max_files_per_session
            ):
                raise LibraryError(
                    413, f"An upload can hold at most {self.limits.max_files_per_session} files."
                )
            session.writing.add(name)
            session.touched = time.monotonic()
            return session

    def _make_room_for_a_session(self) -> None:
        """Keeps at most max_open_sessions: drops the longest idle one, or refuses (429)."""
        with self._sessions_lock:
            sessions = list(self._sessions.values())
        if len(sessions) < self.limits.max_open_sessions:
            return
        idle = [session for session in sessions if session.state == "open" and not session.writing]
        if not idle:
            raise LibraryError(
                429, "Too many imports are being saved at the same time. Try again when one has finished."
            )
        self._close_session(min(idle, key=lambda session: session.touched))

    def _check_free_space(self, directory: Path, upcoming: int) -> None:
        free = _free_bytes(directory)
        if free is not None and free - upcoming < self.limits.min_free_bytes:
            raise LibraryError(
                507,
                f"Not enough free disk space: {_format_bytes(max(free, 0))} is free on the "
                f"library's drive and at least {_format_bytes(self.limits.min_free_bytes)} must stay free.",
            )

    def _accept(self, head: bytes) -> MediaFormat:
        media = sniff_media(head)
        if media is None:
            raise LibraryError(
                415,
                "Only JPEG, PNG, GIF, WebP, AVIF and BMP images and MP4, MOV and WebM videos can be saved.",
            )
        return media

    def _reserve(self, session: _Session, media: MediaFormat, already: int, extra: int) -> None:
        limit = self.limits.max_image_bytes if media.kind == "image" else self.limits.max_video_bytes
        with self._sessions_lock:
            if session.state != "open":
                raise LibraryError(409, "This upload was cancelled.")
            if already + extra > limit:
                raise LibraryError(413, f"A single {media.kind} can be at most {_format_bytes(limit)}.")
            if session.reserved + extra > self.limits.max_session_bytes:
                raise LibraryError(
                    413, f"One import can save at most {_format_bytes(self.limits.max_session_bytes)}."
                )
            if self._library_bytes + self._staged_bytes + extra > self.limits.max_cache_bytes:
                raise LibraryError(
                    413, f"The library is full ({_format_bytes(self.limits.max_cache_bytes)})."
                )
            session.reserved += extra
            self._staged_bytes += extra
            session.touched = time.monotonic()

    def _release(self, session: _Session, count: int) -> None:
        """Caller holds _sessions_lock. A closed session has released everything already."""
        if session.state != "closed" and count:
            session.reserved -= count
            self._staged_bytes -= count

    def _keep(self, session: _Session, staged: StagedFile) -> None:
        with self._sessions_lock:
            if session.state != "open":
                raise LibraryError(409, "This upload was cancelled.")
            previous = session.files.get(staged.name)
            if previous is not None:
                self._release(session, previous.size)
            session.files[staged.name] = staged

    def _end_write(self, session: _Session, name: str, unused_bytes: int) -> None:
        with self._sessions_lock:
            session.writing.discard(name)
            self._release(session, unused_bytes)
            orphaned = session.state == "closed" and not session.writing
        if orphaned:
            _remove_tree(session.directory)

    def _begin_commit(self, upload_id: str) -> _Session:
        with self._sessions_lock:
            session = self._session(upload_id)
            if session.state != "open":
                raise LibraryError(409, "This upload is already being saved.")
            if session.writing:
                raise LibraryError(409, "Some files of this upload are still being uploaded.")
            session.state = "committing"
            return session

    def _close_session(self, session: _Session) -> None:
        """Forgets a session and its staged bytes; its folder goes once no write is running."""
        with self._sessions_lock:
            if session.state == "closed":
                return
            self._staged_bytes -= session.reserved
            session.reserved = 0
            session.state = "closed"
            self._sessions.pop(session.upload_id, None)
            idle = not session.writing
        if idle:
            _remove_tree(session.directory)

    # -- merging -------------------------------------------------------------------------

    def _apply(self, root: Path, session: _Session, document: _Document, commit: _Commit) -> _Document:
        """Moves the staged files to stable names and writes the merged post.json."""
        posts_directory = root / POSTS_FOLDER
        avatars_directory = root / AVATARS_FOLDER
        posts_directory.mkdir(exist_ok=True)
        avatars_directory.mkdir(exist_ok=True)
        # A name is free when neither post.json nor the disk uses it (the old files of a replaced
        # post stay until the new post.json is in place).
        post_stems = _stems(document.posts, POSTS_FOLDER, POST_FILE_RE) | _stems_on_disk(
            posts_directory, POST_FILE_RE
        )
        avatar_stems = _stems(document.posts, AVATARS_FOLDER, AVATAR_FILE_RE) | _stems_on_disk(
            avatars_directory, AVATAR_FILE_RE
        )
        moves: list[tuple[StagedFile, str]] = []

        avatar_by_user: dict[str, str] = {}
        for post in document.posts:
            user = post.get("user")
            if isinstance(user, dict) and isinstance(user.get("avatar"), str):
                avatar_by_user.setdefault(str(user.get("username", "")).casefold(), user["avatar"])
        new_avatars: dict[str, str] = {}
        for username, staged in commit.avatars.items():
            stem = _unique_stem(safe_stem(username, "user"), avatar_stems)
            path = f"{AVATARS_FOLDER}/{stem}.{staged.media.extension}"
            moves.append((staged, path))
            new_avatars[username] = path
        avatar_by_user.update(new_avatars)

        entries: list[JsonObject] = []
        for new_post in commit.posts:
            stem = _unique_stem(safe_stem(new_post.id, "post"), post_stems)
            media: list[JsonObject] = []
            for number, item in enumerate(new_post.media, start=1):
                path = f"{POSTS_FOLDER}/{stem}_{number}.{item.staged.media.extension}"
                moves.append((item.staged, path))
                described: JsonObject = {"type": item.staged.media.kind, "path": path}
                if item.poster is not None:
                    poster = f"{POSTS_FOLDER}/{stem}_{number}_poster.{item.poster.media.extension}"
                    moves.append((item.poster, poster))
                    described["poster"] = poster
                if item.alt:
                    described["alt"] = item.alt
                described["mime"] = item.staged.media.mime
                described["bytes"] = item.staged.size
                media.append(described)
            # image/images mirror the pictures (a video's poster) for readers of the older format.
            pictures = [
                item["path"] if item["type"] == "image" else item.get("poster") for item in media
            ]
            images = [picture for picture in pictures if picture]
            user = dict(new_post.user)
            avatar = avatar_by_user.get(user["username"].casefold())
            if avatar:
                user["avatar"] = avatar
            entry: JsonObject = {"id": new_post.id}
            if "url" in new_post.fields:
                entry["url"] = new_post.fields["url"]
            entry["media_type"] = new_post.media_type
            entry["media"] = media
            if images:
                entry["image"] = images[0]
                entry["images"] = images
            entry["user"] = user
            entry.update((key, value) for key, value in new_post.fields.items() if key != "url")
            entries.append(entry)

        merged = [self._with_avatar(post, new_avatars) for post in document.posts]
        positions = {post.get("id"): index for index, post in enumerate(merged)}
        added = replaced = 0
        for entry in entries:
            index = positions.get(entry["id"])
            if index is None:
                positions[entry["id"]] = len(merged)
                merged.append(entry)
                added += 1
            else:
                merged[index] = entry
                replaced += 1

        now = _now()
        record: JsonObject = {
            "id": session.upload_id,
            "source_name": commit.source_name,
            "source_kind": commit.source_kind,
            "imported_at": now,
            "posts": len(entries),
            "new": added,
            "updated": replaced,
        }
        if commit.exported_at:
            record["exported_at"] = commit.exported_at
        if commit.source_url:
            record["source_url"] = commit.source_url
        updated = _Document(
            revision=self._next_revision(document),
            updated_at=now,
            imports=[*document.imports, record][-self.limits.max_imports_history :],
            posts=sort_posts(merged),
        )

        placed: dict[str, Path] = {}
        created: list[Path] = []
        try:
            for staged, path in moves:
                target = root / path
                first = placed.get(staged.name)
                if first is None:
                    _replace(session.directory / staged.name, target)
                    placed[staged.name] = target
                else:
                    shutil.copyfile(first, target)
                created.append(target)
            self._save(root, updated)
        except OSError:
            for target in created:
                target.unlink(missing_ok=True)
            raise
        return updated

    @staticmethod
    def _with_avatar(post: JsonObject, avatars: dict[str, str]) -> JsonObject:
        """A library post whose user just got a new avatar points at it."""
        user = post.get("user")
        if not isinstance(user, dict):
            return post
        avatar = avatars.get(str(user.get("username", "")).casefold())
        return {**post, "user": {**user, "avatar": avatar}} if avatar else post


async def read_body(body: AsyncIterator[bytes], limit: int) -> bytes:
    """A request body of at most `limit` bytes; LibraryError(413) beyond that."""
    data = bytearray()
    async for chunk in body:
        data += chunk
        if len(data) > limit:
            raise LibraryError(413, f"The request is larger than {_format_bytes(limit)}.")
    return bytes(data)
