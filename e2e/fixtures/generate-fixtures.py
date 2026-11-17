#!/usr/bin/env python3
"""Generates the import fixtures used by e2e/tests/import.spec.ts.

The generated files are committed, so the Playwright suite needs no Python. Re-run this script
only when the fixtures have to change:

    python -m venv .fixtures-venv
    .fixtures-venv/Scripts/python -m pip install pillow pillow-heif     (bin/python on Linux/macOS)
    .fixtures-venv/Scripts/python e2e/fixtures/generate-fixtures.py

Everything is synthetic: flat-colour test cards with a label, and account names that say
"fixture" or "demo". No real Instagram account, photo or post is involved. The shortcodes and
permalinks are made up; the tests never let a request for them leave the machine.

Output (next to this script):

    export-basic/          canonical "instagram-export" v1 folder (JPEG, PNG, WebP, real HEIC)
    export-basic.zip       the same, post.json at the root of the archive
    export-wrapped.zip     the same inside one top-level folder, plus __MACOSX / .DS_Store noise
    export-basic.tar.gz    the same as a gzipped tar with "./" member names
    export-lenient.zip     bare-array manifest, alias field names, backslash paths, wrong-case
                           file names, counts as strings, extensions that lie about the type
    export-problems.zip    one valid post plus entries the importer must skip or defuse
    not-an-export.zip      a valid zip without post.json
    garbage.zip            random bytes with a .zip name
    export-videos.zip      video posts: WebM with a poster, MP4 without one (the "video" alias),
                           a carousel mixing a picture and a video, a .mov the browser cannot play,
                           and a plain picture
    export-part1.zip       one profile split over two archives that both contain FXTpart0002
    export-part2.zip       (part 2 holds the newer copy, so a merged import keeps that one)

The videos come from media/, which record-videos.ts records in Microsoft Edge (canvas +
MediaRecorder); no video tool is needed. Record them first when they are missing:

    cd e2e && node fixtures/record-videos.ts
"""
from __future__ import annotations

import gzip
import io
import json
import random
import shutil
import tarfile
import zipfile
from datetime import datetime, timezone
from pathlib import Path

from PIL import Image, ImageDraw, ImageFont
import pillow_heif

pillow_heif.register_heif_opener()

HERE = Path(__file__).resolve().parent
# Fixed timestamps keep the archives byte-for-byte reproducible.
ZIP_TIME = (2026, 11, 10, 9, 30, 0)
TAR_TIME = int(datetime(2026, 11, 10, 9, 30, 0, tzinfo=timezone.utc).timestamp())

BAKERY = "fixture.sunrise.bakery"
TIDEWATER = "fixture_tidewater"
ORCHARD = "pixel.orchard.demo"
LIGHTHOUSE = "lenient.lighthouse.demo"
GULL = "lenient_gull_demo"
PELICAN = "problem.pelican.demo"
VIDEO_STUDIO = "fixture.video.studio"
VIDEO_CLIPS = "clip_fixture_demo"
PARTS = "fixture.parts.demo"

MEDIA = HERE / "media"


# ----------------------------------------------------------------------------------------------
# Images
# ----------------------------------------------------------------------------------------------

def font(size: int) -> ImageFont.ImageFont:
    """Pillow's bundled scalable font: no dependency on fonts installed on the machine."""
    return ImageFont.load_default(size=size)


def shade(color: tuple[int, int, int], factor: float) -> tuple[int, int, int]:
    return tuple(max(0, min(255, round(channel * factor))) for channel in color)  # type: ignore[return-value]


def card(size: tuple[int, int], color: tuple[int, int, int], title: str, subtitle: str) -> Image.Image:
    """A test card: flat colour, frame, a centred circle (reveals stretching) and a label."""
    width, height = size
    image = Image.new("RGB", size, color)
    draw = ImageDraw.Draw(image)
    unit = min(width, height)

    # Darker band at the bottom and a frame, so cropping is visible in screenshots.
    draw.rectangle([0, height - unit // 6, width, height], fill=shade(color, 0.72))
    inset = unit // 28
    draw.rectangle([inset, inset, width - inset - 1, height - inset - 1], outline=(255, 255, 255), width=max(2, unit // 120))

    radius = unit // 4
    cx, cy = width // 2, height // 2
    draw.ellipse([cx - radius, cy - radius, cx + radius, cy + radius], outline=(255, 255, 255), width=max(3, unit // 90))

    # The title shrinks until it fits inside the circle.
    title_size = max(18, unit // 9)
    while title_size > 12 and draw.textlength(title, font=font(title_size)) > radius * 1.7:
        title_size -= 2
    title_font = font(title_size)
    sub_font = font(max(12, unit // 22))
    draw.text((cx, cy - unit // 28), title, font=title_font, fill=(255, 255, 255), anchor="mm")
    draw.text((cx, cy + unit // 12), subtitle, font=sub_font, fill=(255, 255, 255), anchor="mm")
    draw.text((cx, height - unit // 12), f"{width} x {height}", font=sub_font, fill=(255, 255, 255), anchor="mm")
    return image


def avatar(color: tuple[int, int, int], letter: str) -> Image.Image:
    image = Image.new("RGB", (160, 160), color)
    draw = ImageDraw.Draw(image)
    draw.text((80, 80), letter, font=font(96), fill=(255, 255, 255), anchor="mm")
    return image


def encode(image: Image.Image, fmt: str) -> bytes:
    out = io.BytesIO()
    if fmt == "jpeg":
        image.save(out, format="JPEG", quality=84, optimize=True)
    elif fmt == "png":
        image.save(out, format="PNG", optimize=True)
    elif fmt == "webp":
        image.save(out, format="WEBP", quality=84, method=6)
    elif fmt == "heic":
        # A genuine HEIF container with an HEVC-coded image (brand "heic"), like an iPhone photo.
        image.save(out, format="HEIF", quality=82)
    else:
        raise ValueError(fmt)
    return out.getvalue()


def check_magic(data: bytes, fmt: str) -> None:
    ok = {
        "jpeg": data[:3] == b"\xff\xd8\xff",
        "png": data[:8] == b"\x89PNG\r\n\x1a\n",
        "webp": data[:4] == b"RIFF" and data[8:12] == b"WEBP",
        "heic": data[4:8] == b"ftyp" and data[8:12] in (b"heic", b"heix", b"mif1", b"msf1", b"hevc"),
    }[fmt]
    if not ok:
        raise SystemExit(f"encoder did not produce {fmt}: {data[:16]!r}")


def picture(size: tuple[int, int], color: tuple[int, int, int], title: str, fmt: str, label: str | None = None) -> bytes:
    data = encode(card(size, color, title, label or fmt.upper()), fmt)
    check_magic(data, fmt)
    return data


# ----------------------------------------------------------------------------------------------
# Containers
# ----------------------------------------------------------------------------------------------

def dump_json(value: object) -> bytes:
    return (json.dumps(value, ensure_ascii=False, indent=2) + "\n").encode("utf-8")


def write_folder(target: Path, files: dict[str, bytes]) -> None:
    if target.exists():
        shutil.rmtree(target)
    for name, data in files.items():
        path = target / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)


def write_zip(target: Path, files: dict[str, bytes]) -> None:
    with zipfile.ZipFile(target, "w") as archive:
        for name, data in files.items():
            info = zipfile.ZipInfo(name, date_time=ZIP_TIME)
            info.compress_type = zipfile.ZIP_DEFLATED
            info.external_attr = 0o644 << 16
            archive.writestr(info, data)


def write_tar_gz(target: Path, files: dict[str, bytes]) -> None:
    raw = io.BytesIO()
    with tarfile.open(fileobj=raw, mode="w", format=tarfile.USTAR_FORMAT) as archive:
        directories: set[str] = set()
        for name, data in files.items():
            # "./"-prefixed members with explicit directory entries, as `tar -czf x.tar.gz .` writes them.
            parent = str(Path(name).parent).replace("\\", "/")
            if parent != "." and parent not in directories:
                directories.add(parent)
                folder = tarfile.TarInfo(f"./{parent}")
                folder.type = tarfile.DIRTYPE
                folder.mode = 0o755
                folder.mtime = TAR_TIME
                archive.addfile(folder)
            info = tarfile.TarInfo(f"./{name}")
            info.size = len(data)
            info.mode = 0o644
            info.mtime = TAR_TIME
            archive.addfile(info, io.BytesIO(data))
    with open(target, "wb") as handle:
        with gzip.GzipFile(filename="", fileobj=handle, mode="wb", mtime=0) as zipped:
            zipped.write(raw.getvalue())


# ----------------------------------------------------------------------------------------------
# export-basic: the canonical format
# ----------------------------------------------------------------------------------------------

def basic_export() -> dict[str, bytes]:
    manifest = {
        "format": "instagram-export",
        "version": 1,
        "exported_at": "2026-11-10T09:30:00Z",
        "source": {"type": "profile", "url": f"https://www.instagram.com/{BAKERY}/"},
        "posts": [
            {
                "id": "FXTbasic001",
                "url": "https://www.instagram.com/p/FXTbasic001/",
                "image": "posts/post1.jpg",
                "user": {
                    "username": BAKERY,
                    "full_name": "Sunrise Bakery (test fixture)",
                    "verified": True,
                    "avatar": f"avatars/{BAKERY}.jpg",
                    "profile_url": f"https://www.instagram.com/{BAKERY}/",
                },
                "caption": (
                    "Fresh out of the oven 🥐☀️\n"
                    f"First batch of the season, baked together with @{TIDEWATER}.\n"
                    "\n"
                    "Come early, the cinnamon knots never last past nine.\n"
                    "#sourdough #fixturedata #朝ごはん"
                ),
                "comment_count": 1234,
                "like_count": 56789,
                "taken_at": "2026-11-01T15:04:05Z",
                "media_type": "image",
                "alt": "Orange test card labelled POST 1",
                "location": "Fixtureville, Testland",
                "comments": [
                    {"username": TIDEWATER, "text": "Saving me two of those 🙌", "like_count": 3, "taken_at": "2026-11-01T16:00:00Z"},
                    {"username": ORCHARD, "text": "The crumb on that loaf! #goals", "like_count": 0, "taken_at": "2026-11-01T17:30:00Z"},
                ],
            },
            {
                "id": "FXTbasic002",
                "url": "https://www.instagram.com/p/FXTbasic002/",
                "image": "posts/post2.png",
                "user": {
                    "username": TIDEWATER,
                    "full_name": "Tidewater Test Account",
                    "verified": False,
                    "avatar": f"avatars/{TIDEWATER}.jpg",
                    "profile_url": f"https://www.instagram.com/{TIDEWATER}/",
                },
                "caption": (
                    "潮が引いたあとの静かな海 🌊\n"
                    "Вечерний отлив и тишина.\n"
                    "مساء هادئ على الشاطئ\n"
                    "#tidepool #lowtide"
                ),
                "comment_count": 42,
                "like_count": None,
                "taken_at": "2026-10-28T07:15:00Z",
                "media_type": "image",
                "alt": "Blue portrait test card labelled POST 2",
            },
            {
                "id": "FXTbasic003",
                "url": "https://www.instagram.com/p/FXTbasic003/",
                "image": "posts/post3.webp",
                "user": {
                    "username": BAKERY,
                    "full_name": "Sunrise Bakery (test fixture)",
                    "verified": True,
                    "avatar": f"avatars/{BAKERY}.jpg",
                    "profile_url": f"https://www.instagram.com/{BAKERY}/",
                },
                "caption": "Quiet morning in the green room 🌿",
                "comment_count": 0,
                "like_count": 987,
                "taken_at": "2026-10-20T06:00:00Z",
                "media_type": "image",
            },
            {
                "id": "FXTbasic004",
                "url": "https://www.instagram.com/p/FXTbasic004/",
                "image": "posts/post4.heic",
                "user": {
                    "username": ORCHARD,
                    "full_name": "Pixel Orchard Demo",
                    "profile_url": f"https://www.instagram.com/{ORCHARD}/",
                },
                "caption": "Straight off the phone as HEIC 📱🍎\n#heic #shotonfixture",
                "comment_count": 7,
                "like_count": 321,
                "taken_at": "2026-10-12T18:45:00Z",
                "media_type": "image",
                "alt": "Purple test card labelled POST 4 HEIC",
            },
            {
                "id": "FXTbasic005",
                "url": "https://www.instagram.com/p/FXTbasic005/",
                "image": "posts/post5_1.jpg",
                "images": ["posts/post5_1.jpg", "posts/post5_2.jpg"],
                "user": {
                    "username": TIDEWATER,
                    "full_name": "Tidewater Test Account",
                    "verified": False,
                    "avatar": f"avatars/{TIDEWATER}.jpg",
                    "profile_url": f"https://www.instagram.com/{TIDEWATER}/",
                },
                "caption": "Two tides, one afternoon ↔️ swipe for the second one\n#carousel #fixturedata",
                "comment_count": 18,
                "like_count": 2500,
                "taken_at": "2026-10-05T12:00:00Z",
                "media_type": "carousel",
            },
            {
                "id": "FXTbasic006",
                "url": "https://www.instagram.com/reel/FXTbasic006/",
                "image": "posts/post6.jpg",
                "user": {
                    "username": ORCHARD,
                    "full_name": "Pixel Orchard Demo",
                    "profile_url": f"https://www.instagram.com/{ORCHARD}/",
                },
                "caption": "Timelapse of the orchard at dusk 🎬 (the export keeps the cover image only)",
                "comment_count": 3,
                "like_count": 45678,
                "taken_at": "2026-09-30T20:10:00Z",
                "media_type": "video",
            },
        ],
    }
    return {
        "post.json": dump_json(manifest),
        "posts/post1.jpg": picture((720, 720), (230, 126, 34), "POST 1", "jpeg"),
        "posts/post2.png": picture((720, 900), (41, 128, 185), "POST 2", "png"),
        "posts/post3.webp": picture((960, 540), (39, 174, 96), "POST 3", "webp"),
        "posts/post4.heic": picture((720, 720), (142, 68, 173), "POST 4", "heic"),
        "posts/post5_1.jpg": picture((720, 720), (22, 160, 133), "POST 5 - 1/2", "jpeg"),
        "posts/post5_2.jpg": picture((720, 720), (192, 57, 43), "POST 5 - 2/2", "jpeg"),
        "posts/post6.jpg": picture((720, 720), (52, 73, 94), "POST 6", "jpeg", "VIDEO COVER"),
        f"avatars/{BAKERY}.jpg": encode(avatar((211, 84, 0), "S"), "jpeg"),
        f"avatars/{TIDEWATER}.jpg": encode(avatar((31, 97, 141), "T"), "jpeg"),
    }


def wrapped(files: dict[str, bytes], folder: str) -> dict[str, bytes]:
    """The export inside one top-level folder, with the droppings macOS adds to zips."""
    out: dict[str, bytes] = {}
    for name, data in files.items():
        out[f"{folder}/{name}"] = data
    out[f"{folder}/.DS_Store"] = b"\x00\x00\x00\x01Bud1" + b"\x00" * 24
    # AppleDouble companions: same names as real members, but not the real content.
    out[f"__MACOSX/{folder}/._post.json"] = b"\x00\x05\x16\x07\x00\x02\x00\x00Mac OS X        " + b"\x00" * 32
    out[f"__MACOSX/{folder}/posts/._post1.jpg"] = b"\x00\x05\x16\x07\x00\x02\x00\x00Mac OS X        " + b"\x00" * 32
    out[f"__MACOSX/{folder}/post.json"] = b'{"posts": "this is noise, not the manifest"}'
    return out


# ----------------------------------------------------------------------------------------------
# export-lenient: everything the importer has to tolerate
# ----------------------------------------------------------------------------------------------

def lenient_export() -> dict[str, bytes]:
    epoch_seconds = int(datetime(2026, 10, 30, 12, 0, 0, tzinfo=timezone.utc).timestamp())
    manifest = [
        {
            # Aliases, a Windows-style path in the wrong case, counts written for humans.
            "shortcode": "FXTlen001",
            "permalink": "https://www.instagram.com/p/FXTlen001/",
            "image_path": "Posts\\LENIENT1.JPG",
            "from": LIGHTHOUSE,
            "description": "Alias field names all the way down 🗼\n#lenient",
            "comments_count": "1,234",
            "likes": "12.3K",
            "timestamp": epoch_seconds,
            "type": "image",
        },
        {
            # http link (upgraded to https), a .png that is really a WebP, owner as an object.
            "code": "FXTlen002",
            "link": "http://instagram.com/p/FXTlen002/",
            "path": ".\\posts\\lenient2.png",
            "owner": {
                "name": LIGHTHOUSE,
                "fullName": "Lenient Lighthouse",
                "is_verified": True,
                "profile_pic": "Avatars\\Lighthouse.PNG",
                "url": f"https://www.instagram.com/{LIGHTHOUSE}/",
            },
            "first_comment": "The extension says PNG, the bytes say WebP.",
            "total_comments": "2.5K",
            "likes_count": "1.2M",
            "date": "2026-10-29T10:00:00+02:00",
        },
        {
            # Carousel through an alias, a leading slash, junk counts.
            "id": "FXTlen003",
            "post_url": "https://www.instagram.com/p/FXTlen003/",
            "media_paths": ["/posts/lenient3_1.jpeg", "posts/lenient3_2.jpeg"],
            "author": {"handle": GULL, "display_name": "Lenient Gull"},
            "text": "Two slides listed under media_paths.",
            "commentCount": "not a number",
            "likeCount": -5,
            "created_at": epoch_seconds * 1000 - 86_400_000,
        },
        {
            # A .jpg that is really a PNG, the user as a bare "username" string, no link at all.
            "file": "posts/lenient4.jpg",
            "username": GULL,
            "caption": "",
            "comments_total": 5,
            "like_count": "987",
        },
    ]
    return {
        "Post.JSON": dump_json(manifest),
        "posts/lenient1.jpg": picture((720, 720), (243, 156, 18), "LENIENT 1", "jpeg"),
        "posts/lenient2.png": picture((720, 720), (26, 188, 156), "LENIENT 2", "webp", "WEBP NAMED .PNG"),
        "posts/lenient3_1.jpeg": picture((720, 720), (155, 89, 182), "LENIENT 3 - 1/2", "jpeg"),
        "posts/lenient3_2.jpeg": picture((720, 720), (52, 152, 219), "LENIENT 3 - 2/2", "jpeg"),
        "posts/lenient4.jpg": picture((720, 720), (231, 76, 60), "LENIENT 4", "png", "PNG NAMED .JPG"),
        "avatars/lighthouse.png": encode(avatar((183, 149, 11), "L"), "png"),
    }


# ----------------------------------------------------------------------------------------------
# export-problems: entries that must be skipped or defused
# ----------------------------------------------------------------------------------------------

SVG_IMAGE = (
    '<?xml version="1.0" encoding="UTF-8"?>\n'
    '<svg xmlns="http://www.w3.org/2000/svg" width="720" height="720" viewBox="0 0 720 720">\n'
    '  <rect width="720" height="720" fill="#c0392b"/>\n'
    '  <text x="360" y="380" font-size="64" text-anchor="middle" fill="#fff">SVG MUST NOT RENDER</text>\n'
    '</svg>\n'
)


def problems_export() -> dict[str, bytes]:
    pelican = {"username": PELICAN, "full_name": "Problem Pelican Demo"}
    manifest = {
        "format": "instagram-export",
        "version": 1,
        "exported_at": "2026-11-10T09:30:00Z",
        "posts": [
            {
                "id": "FXTprob001",
                "url": "https://www.instagram.com/p/FXTprob001/",
                "image": "posts/problem1.jpg",
                "user": pelican,
                "caption": "The one valid post of this export ✅",
                "comment_count": 12,
                "like_count": 345,
                "taken_at": "2026-11-02T08:00:00Z",
            },
            {
                "id": "FXTprob002",
                "url": "https://www.instagram.com/p/FXTprob002/",
                "image": "posts/does-not-exist.jpg",
                "user": pelican,
                "caption": "My image is missing from the export.",
                "comment_count": 1,
                "like_count": 1,
            },
            {
                "id": "FXTprob003",
                "url": "javascript:alert(document.domain)",
                "image": "posts/problem3.jpg",
                "user": pelican,
                "caption": "My link is a javascript: URL.",
                "comment_count": 2,
                "like_count": 2,
            },
            {
                "id": "FXTprob004",
                "url": "https://www.instagram.com.evil.example/p/FXTprob004/",
                "image": "posts/problem4.png",
                "user": {**pelican, "profile_url": "https://instagram.com.evil.example/" + PELICAN + "/"},
                "caption": "My link and my profile link point at a look-alike host.",
                "comment_count": 3,
                "like_count": 3,
            },
            {
                "id": "FXTprob001",
                "url": "https://www.instagram.com/p/FXTprob001/",
                "image": "posts/problem5.jpg",
                "user": pelican,
                "caption": "I reuse the id of the first post.",
                "comment_count": 4,
                "like_count": 4,
            },
            {
                "id": "FXTprob006",
                "url": "https://www.instagram.com/p/FXTprob006/",
                "image": "posts/problem6.svg",
                "user": pelican,
                "caption": "My image is an SVG.",
                "comment_count": 5,
                "like_count": 5,
            },
            {
                "id": "FXTprob007",
                "url": "https://www.instagram.com/p/FXTprob007/",
                "image": "https://cdn.fixture.invalid/remote/problem7.jpg",
                "user": pelican,
                "caption": "My image path is a remote URL, which is never fetched.",
                "comment_count": 6,
                "like_count": 6,
            },
        ],
    }
    return {
        "post.json": dump_json(manifest),
        "posts/problem1.jpg": picture((720, 720), (39, 174, 96), "PROBLEM 1", "jpeg", "VALID"),
        "posts/problem3.jpg": picture((720, 720), (127, 140, 141), "PROBLEM 3", "jpeg", "JAVASCRIPT: LINK"),
        "posts/problem4.png": picture((720, 720), (44, 62, 80), "PROBLEM 4", "png", "LOOK-ALIKE HOST"),
        "posts/problem5.jpg": picture((720, 720), (211, 84, 0), "PROBLEM 5", "jpeg", "DUPLICATE ID"),
        "posts/problem6.svg": SVG_IMAGE.encode("utf-8"),
    }


def not_an_export() -> dict[str, bytes]:
    return {
        "readme.txt": b"Holiday pictures. There is no post.json in this archive.\n",
        "photos/holiday.jpg": picture((480, 480), (149, 165, 166), "NOT AN EXPORT", "jpeg"),
    }


# ----------------------------------------------------------------------------------------------
# Videos: export-videos.zip, and an export split over two archives (export-part1/2.zip)
# ----------------------------------------------------------------------------------------------

def clip(name: str) -> bytes:
    """A video recorded by record-videos.ts (committed under media/)."""
    data = (MEDIA / name).read_bytes()
    is_webm = data[:4] == b"\x1a\x45\xdf\xa3" and b"webm" in data[:64]
    is_mp4 = data[4:8] == b"ftyp"
    if not (is_webm or is_mp4):
        raise SystemExit(f"{name} is not a WebM or MP4 video")
    return data


def unplayable_mov() -> bytes:
    """Sniffs as a QuickTime video (ftyp brand "qt  ") but holds no playable movie, like a
    HEVC .mov in a browser without HEVC support: the browser reports a media error."""
    ftyp = (20).to_bytes(4, "big") + b"ftyp" + b"qt  " + (0).to_bytes(4, "big") + b"qt  "
    payload = random.Random(4).randbytes(2048)
    mdat = (8 + len(payload)).to_bytes(4, "big") + b"mdat" + payload
    return ftyp + mdat


def video_user(username: str, full_name: str) -> dict[str, object]:
    return {"username": username, "full_name": full_name, "profile_url": f"https://www.instagram.com/{username}/"}


def video_export() -> dict[str, bytes]:
    studio = video_user(VIDEO_STUDIO, "Video Studio (test fixture)")
    clips = video_user(VIDEO_CLIPS, "Clip Demo (test fixture)")
    manifest = {
        "format": "instagram-export",
        "version": 1,
        "exported_at": "2026-11-10T09:30:00Z",
        "posts": [
            {
                "id": "FXTvideo001",
                "url": "https://www.instagram.com/reel/FXTvideo001/",
                "media_type": "video",
                "media": [{"type": "video", "path": "posts/reel1.webm", "poster": "posts/reel1_poster.jpg"}],
                "user": studio,
                "caption": "An orange clip with a poster #fixturedata",
                "comment_count": 4,
                "like_count": 120,
                "taken_at": "2026-11-05T10:00:00Z",
                "alt": "Orange test clip with a moving white disc",
            },
            {
                "id": "FXTvideo002",
                "url": "https://www.instagram.com/reel/FXTvideo002/",
                "media_type": "video",
                "video": "posts/clip2.mp4",
                "user": clips,
                "caption": "A violet MP4 clip without a poster",
                "comment_count": 0,
                "like_count": 7,
                "taken_at": "2026-11-04T10:00:00Z",
            },
            {
                "id": "FXTvideo003",
                "url": "https://www.instagram.com/p/FXTvideo003/",
                "media_type": "carousel",
                "media": ["posts/slide3_1.jpg", {"type": "video", "path": "posts/slide3_2.webm"}],
                "user": studio,
                "caption": "A carousel: a picture, then a teal clip",
                "comment_count": 2,
                "like_count": 55,
                "taken_at": "2026-11-03T10:00:00Z",
            },
            {
                "id": "FXTvideo004",
                "url": "https://www.instagram.com/reel/FXTvideo004/",
                "media_type": "video",
                "media": [{"type": "video", "path": "posts/broken4.mov"}],
                "user": clips,
                "caption": "A .mov this browser cannot play",
                "comment_count": 0,
                "like_count": 1,
                "taken_at": "2026-11-02T10:00:00Z",
            },
            {
                "id": "FXTvideo005",
                "url": "https://www.instagram.com/p/FXTvideo005/",
                "image": "posts/post5.png",
                "user": studio,
                "caption": "A plain picture between the clips",
                "comment_count": 0,
                "like_count": 3,
                "taken_at": "2026-11-01T10:00:00Z",
            },
        ],
    }
    return {
        "post.json": dump_json(manifest),
        "posts/reel1.webm": clip("clip-orange.webm"),
        "posts/reel1_poster.jpg": picture((240, 300), (240, 140, 30), "POSTER 1", "jpeg"),
        "posts/clip2.mp4": clip("clip-violet.mp4"),
        "posts/slide3_1.jpg": picture((300, 240), (52, 101, 164), "SLIDE 3.1", "jpeg"),
        "posts/slide3_2.webm": clip("clip-teal.webm"),
        "posts/broken4.mov": unplayable_mov(),
        "posts/post5.png": picture((300, 300), (106, 153, 78), "POST 5", "png"),
    }


def part_post(post_id: str, path: str, caption: str, taken_at: str, media_type: str = "image") -> dict[str, object]:
    entry: dict[str, object] = {
        "id": post_id,
        "url": f"https://www.instagram.com/p/{post_id}/",
        "media_type": media_type,
        "media": [{"type": media_type, "path": path}],
        "user": video_user(PARTS, "Parts Demo (test fixture)"),
        "caption": caption,
        "comment_count": 0,
        "like_count": 1,
        "taken_at": taken_at,
    }
    return entry


def part_exports() -> tuple[dict[str, bytes], dict[str, bytes]]:
    """One profile exported in two archives that overlap in FXTpart0002 (part 2 has the newer copy)."""
    part1 = {
        "format": "instagram-export",
        "version": 1,
        "exported_at": "2026-10-04T08:00:00Z",
        "posts": [
            part_post("FXTpart0001", "posts/part1_a.jpg", "Part 1: the oldest post", "2026-10-01T08:00:00Z"),
            part_post("FXTpart0002", "posts/part1_b.webm", "Part 1: first version of the shared post", "2026-10-03T08:00:00Z", "video"),
        ],
    }
    part2 = {
        "format": "instagram-export",
        "version": 1,
        "exported_at": "2026-10-06T08:00:00Z",
        "posts": [
            part_post("FXTpart0002", "posts/part2_b.jpg", "Part 2: second version of the shared post", "2026-10-03T08:00:00Z"),
            part_post("FXTpart0003", "posts/part2_c.jpg", "Part 2: the newest post", "2026-10-05T08:00:00Z"),
        ],
    }
    return (
        {
            "post.json": dump_json(part1),
            "posts/part1_a.jpg": picture((300, 300), (192, 57, 43), "PART 1 A", "jpeg"),
            "posts/part1_b.webm": clip("clip-orange.webm"),
        },
        {
            "post.json": dump_json(part2),
            "posts/part2_b.jpg": picture((300, 300), (39, 174, 96), "PART 2 B", "jpeg"),
            "posts/part2_c.jpg": picture((300, 300), (41, 128, 185), "PART 2 C", "jpeg"),
        },
    )


def main() -> None:
    basic = basic_export()
    write_folder(HERE / "export-basic", basic)
    write_zip(HERE / "export-basic.zip", basic)
    write_zip(HERE / "export-wrapped.zip", wrapped(basic, "my-instagram-export"))
    write_tar_gz(HERE / "export-basic.tar.gz", basic)
    write_zip(HERE / "export-lenient.zip", lenient_export())
    write_zip(HERE / "export-problems.zip", problems_export())
    write_zip(HERE / "not-an-export.zip", not_an_export())
    (HERE / "garbage.zip").write_bytes(random.Random(20261110).randbytes(4096))
    write_zip(HERE / "export-videos.zip", video_export())
    part1, part2 = part_exports()
    write_zip(HERE / "export-part1.zip", part1)
    write_zip(HERE / "export-part2.zip", part2)

    for path in sorted(HERE.rglob("*")):
        if path.is_file() and path.suffix != ".py":
            print(f"{path.stat().st_size:>8}  {path.relative_to(HERE).as_posix()}")


if __name__ == "__main__":
    main()
