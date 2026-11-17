# Instagram Clone

A full-stack Instagram application for Windows: a React/Vite frontend, a
Python (FastAPI) backend, and a native Windows viewer in `windows-app/` (Rust,
eframe/egui) that presents pictures and Instagram exports in the same look and
serves the same web pages from inside a single portable exe. One repository,
one application: the Web UI and the backend come from `frontend/` and
`backend/`, and the Windows app is their native counterpart.

The frontend is an Instagram clone
whose styling is migrated from a captured `instagram.com` session: the original
Facebook Design System (FDS) light/dark design tokens (colors, typography,
surfaces, dividers, shadows) are vendored verbatim in
`frontend/src/styles/instagram-tokens.css` and consumed by
`frontend/src/styles.css`.

Highlights:

- **Home feed** with stories tray + story viewer, posts (like, save, comment,
  double-tap to like), suggested accounts, and the standard footer.
- **Post detail** modal, **Explore** grid, **Reels**, **Profile**, plus
  **Search**, **Notifications**, **Messages** panels and a **Create post** flow.
- **Light / dark appearance** switch (More → Switch appearance), persisted in
  `localStorage`, driven by the migrated dark-mode tokens.
- **Responsive** desktop, collapsed icon-rail, and mobile bottom-nav layouts.
- **Real pictures and videos through import**: pick or drop one or several
  Instagram exports (folders or `.zip` / `.tar` / `.tar.gz` archives with
  `post.json` and `posts/`) and the feed, Explore, profiles and post detail
  show their photos, videos, captions, authors and counts, carousels and
  HEIC/HEIF included. Every import is merged into a **library** that the
  backend keeps in `%TEMP%\instagram_cache`. See
  [Importing an Instagram export](#importing-an-instagram-export).
- **Auto view**: a full-screen slideshow that moves to the next post after a
  time you choose (15 seconds by default), and lets videos finish first.
- **View on Instagram**: every imported post links to the real post, and every
  imported author to the real profile, in a new tab. Posts from other sites
  work too: the link then reads **View on** followed by the site's name.
- Seed content (usernames, captions, like counts) ships with the frontend;
  until something is imported, media uses offline gradient placeholders.
- **Portable Windows build**: `build.bat` produces `server.exe` folders that
  run without Python or Node.js, with the frontend and the backend together
  or on different machines. See [Portable build](#portable-build).
- **Native Windows app**: `run-app.bat` starts `windows-app/`, the Rust viewer
  that opens pictures, folders and export archives in the Instagram look, with
  zoom, auto view, description editing and export downloads; its **Browser
  view** serves the same web pages as this stack. See
  [windows-app/README.md](windows-app/README.md).

The backend (FastAPI + SQLite) still exposes the template's `/api/counter`
endpoints from the original scaffold, and keeps the import library under
`/api/library`. The rest of the Instagram UI is self-contained with local seed
data. Exports are read in the browser; only the parsed posts and their media
files are sent to the clone's own backend, never anywhere else.

## Quick start

On 64-bit Windows 10 or 11, with Python 3.10+ and Node.js on PATH (Rust and the
Visual Studio C++ build tools are needed only for the native app):

```powershell
.\setup.bat
.\run.bat      # the web stack: FastAPI backend + React frontend
.\run-app.bat  # the native Windows app (needs Rust; run setup.bat first)
```

If a Python virtual environment or Conda environment is already active, setup
installs into that environment and run uses the same interpreter. Otherwise,
the scripts create and reuse `.venv` in this project. `setup.bat` also runs
the native app's setup (toolchain check and `cargo fetch`) when Rust is
installed, and says so when it is not.

The default backend and frontend ports are `8000` and `5173`. The runner checks
both ports before launch and advances to the next available port when needed.
It prints the actual URLs selected for the session.

## LAN access

The development runner and portable Windows Release build listen on `0.0.0.0`
by default, making the frontend and API available on the host's network
interfaces. Start either launcher and open a printed `LAN` frontend URL on
another device connected to the same network. The Debug build defaults to
`127.0.0.1` for local-only access:

```powershell
.\run.bat
# Or, after building:
.\build\dist\Release\run.bat --frontend 4000 --backend 9000
```

Startup lists each detected IPv4 address with the selected ports. `.\run.bat`
prints, for example:

```text
==============================================================
Starting Instagram Clone Full-Stack Services
Python:   C:\...\instagram-clone\.venv\Scripts\python.exe
Backend:  http://127.0.0.1:8000 (API docs: http://127.0.0.1:8000/docs)
Frontend: http://localhost:5173
LAN:      http://192.168.1.156:5173
LAN API:  http://192.168.1.156:8000/docs
LAN:      http://192.168.56.1:5173
LAN API:  http://192.168.56.1:8000/docs
Open a LAN URL on another device connected to the same network.
Press Ctrl+C to stop both services.
==============================================================
```

The portable build also names its role, the settings file, the database and
the library folder. `.\build\dist\Release\run.bat --frontend 4000 --backend 9000`
prints, for example (without `--frontend` and `--backend` the ports come from
`settings.json`, by default `3127` and `8317`, see [Configuration](#configuration)):

```text
Starting Instagram Clone Full-Stack Services
Role:     all
Python:   C:\...\build\dist\Release\server.exe
Backend:  http://127.0.0.1:9000 (API docs: http://127.0.0.1:9000/docs)
Frontend: http://127.0.0.1:4000
LAN:      http://192.168.1.156:4000
LAN API:  http://192.168.1.156:9000/docs
LAN:      http://192.168.56.1:4000
LAN API:  http://192.168.56.1:9000/docs
Open a LAN URL on another device connected to the same network.
Settings: C:\Users\<you>\AppData\Roaming\instagram-clone\settings.json
Database: C:\Users\<you>\AppData\Local\Temp\instagram-clone\counter.db
Cache:    C:\Users\<you>\AppData\Local\Temp\instagram_cache
[run] Press Ctrl+C to stop.
```

Use the address of the adapter connected to your other device; virtual-machine
adapters may also appear. The frontend forwards `/api` requests to the backend
on the host, so remote browsers work without IP-specific frontend configuration.
If Windows Firewall blocks access, allow the running Python interpreter or
`server.exe` on your private network. Wi-Fi guest networks or client isolation
can also prevent devices from reaching each other.

For local-only access, use `--host 127.0.0.1` with either launcher or set
`BACKEND_HOST=127.0.0.1` in the relevant `.env`. To bind a specific adapter,
use `--host 192.168.1.156` (replace it with your host's address). Command-line
`--host` takes precedence over `BACKEND_HOST`. Existing `.env` files retain
their settings; change an existing loopback value to `0.0.0.0` to enable LAN.

## Importing an Instagram export

The clone can replace its seed feed with posts exported from Instagram. An
export is a folder, or an archive of that folder, in the `instagram-export`
format described below. [instagram.md](instagram.md) is the runbook that lets
an AI agent with a browser MCP client produce such an export from posts the
logged-in user can already see.

### Opening the importer

- Desktop: **Import** in the sidebar, or **More → Import Instagram export**.
- Phone-sized screens: the import button in the top header.
- Anywhere: drag exports onto the window. No dialog has to be open. (Only
  while **Create** is open, dropped files go to that dialog instead.)

The dialog then offers three ways in:

| Way | What to pick |
| --- | --- |
| **Select archive files** | One or more `.zip`, `.tar`, `.tar.gz` or `.tgz` files, for example all parts of a split export. `post.json` may sit at the root of each archive or inside one top-level folder. |
| **Select folder** | The export folder itself, the one that contains `post.json`. The browser asks for permission to read the folder. One folder per pick (a browser limit); pick again, or drag several folders at once. |
| **Drag and drop** | Archives, export folders, or the loose contents of one export, dropped on the dialog or anywhere on the page. Several archives and folders dropped together are imported as one. |

Archives and folders picked or dropped together (up to 50) are merged into one
import: when the same post (same `id`) is in more than one of them, the one
read last is kept and a warning says which copy was replaced. A source that is
not an export, or is damaged, becomes a warning naming it; the others are still
imported.

To try it without an export of your own, import
`e2e/fixtures/export-basic.zip` (or the folder `e2e/fixtures/export-basic`):
six synthetic test-card posts, one of them a real HEIC file.
`e2e/fixtures/export-videos.zip` adds video posts, and
`e2e/fixtures/export-part1.zip` with `export-part2.zip` is one export split
over two archives.

When the import finishes the dialog reports `Imported N of M posts` and has a
warning for every entry it had to skip or repair: the first five are listed,
**Show all N warnings** opens the rest. **View feed** shows the result.

A banner above the feed describes what is shown and offers **Auto view**. With
the library (see below) it reads
`Showing N posts from your library (M imports) · saved in <folder>`, with
**Import more** and **Clear library**, which asks for confirmation. Without
it, the banner names the import (`Showing N of M posts` when entries were
skipped) and offers **Import another** and **Clear import**. When there were
warnings it also has an **N warnings** button that brings the list back, also
after a reload, and when the export names the page it was taken from, a
**View source on Instagram** link.

The dialog can be closed while an import is still running. A small bar then
shows `Importing…` with **Show** and **Cancel**. If a post, a panel or another
dialog is open when the import finishes, the feed is not replaced underneath
it: the bar says `N posts ready` and waits for **View feed** or **Discard**.

### The library in %TEMP%\instagram_cache

When the clone runs with its backend (`run.bat` or a portable
build), every import is saved in a library on the machine that runs the
backend, and the feed shows the library. The browser still reads the export
itself; it then uploads the posts and their media files to the backend, which
merges them into what is already there (same post `id`: the new one replaces
the old one and its files; posts are ordered newest first). Imports therefore
add up across import actions, browsers, reloads and restarts.

The folder is chosen in this order:

1. The environment variable `INSTAGRAM_CACHE_DIR` (also from a `.env` file).
   A relative path resolves from the project root in development, or beside
   `server.exe` in a portable build.
2. `instagram_cache_directory` in `settings.json` (see
   [Configuration](#configuration)). `%VARS%` and `~` are expanded; a relative
   path resolves from the folder that holds `settings.json`.
3. `instagram_cache` in the temporary folder: `%TEMP%\instagram_cache` on
   Windows (`$TEMP`, else the system temporary folder, elsewhere).

The portable server prints the folder as `Cache:` when it starts, and the
import dialog and the banner show it too (a browser on another computer is
told only the folder's name). The folder is itself a valid export
(the validator in [instagram.md](instagram.md), section 9, passes it), so it
can be archived and imported elsewhere:

```text
instagram_cache/
├─ post.json            the merged posts, plus the list of imports ("library")
├─ posts/               <post>_<k>.<ext> pictures and videos, <post>_<k>_poster.<ext> posters
├─ avatars/             <username>.<ext>
├─ .instagram-library   marks the folder as the library; locked while a backend uses it
└─ .staging/            uploads in progress; leftovers are removed when the backend starts
```

The library only takes a folder that is empty (or does not exist yet) or that
it created itself. A folder that already holds other files, for example an
export unpacked there, is refused with a message naming one of them, and
nothing in it is changed: choose another folder with `INSTAGRAM_CACHE_DIR` or
`instagram_cache_directory`. Only one running backend (a `run.bat` backend or a
`server.exe`) can use a library folder at a time; a second one reports that the
folder is in use by another running copy, and its pages keep imports in the
browser.

**Clear library** (with a confirmation) deletes only the library's own files in
that folder: `post.json`, the pictures, videos and profile pictures it wrote
under `posts/` and `avatars/`, and unfinished uploads. Other files, and the
archives you imported from, are not touched. With the clone stopped, deleting
the folder has the same effect. Temporary folders can be cleaned by the
operating system, so set `instagram_cache_directory` to a permanent folder if
the library matters. The backend accepts at most 64 MB per picture, 1 GB per
video, 8 GB per upload and 64 GB in the whole library, and stops an upload when
less than 1 GB would stay free on its drive. An upload nobody continues for 30
minutes is dropped, and at most 8 uploads can be open at once.

An import of more than 5,000 posts (several archives together can have that
many), or one larger than a single upload allows, is saved in several parts,
one after the other. Each part is listed as `<name> (part 1 of 2)` and so on
in the library's list of imports, and the dialog shows `part 1 of 2` while it
saves.

While the files are uploaded the dialog shows the progress and **Cancel**.
Once it says `Adding the posts to your library…` the backend is merging the
import: it can no longer be cancelled and is shown as saved. Cancelling an
import that is saved in parts keeps the parts that were already saved. If a
later part cannot be saved, the dialog says
`Saved N of M posts; the rest could not be saved` and why, and the feed shows
the library with the saved parts; import the archives again to save the rest
(posts already saved are updated, not duplicated). When some of the archives
picked together cannot be read at all, the headline counts only the others,
for example `Imported N of M posts from 3 of 5 archives`, with a warning sign
and the list of sources.

Without a backend (for example `npm run dev` on its own, or a backend that
cannot use the folder), the clone falls back to keeping imports in this
browser only, as described under [Privacy and storage](#privacy-and-storage).
If saving to the library fails, the dialog says why (`Not saved in the
library`) and the posts are shown from the browser instead. Until the page is
reloaded, further imports stay in this browser too, and the dialog says so
("Reload the page to save to the library again."). After a reload with a
working library, a notice `N posts from <name> are kept in this browser only`
offers **Save to library** and **Discard**, so those posts are not lost.

### Auto view

Auto view shows the posts one after another in a full-screen view with the
author, a caption excerpt, the counts and a progress bar, and moves on by
itself. Start it from **Auto view** in the import banner,
**Auto view from here** behind a post's **More options** button (starts at
that post), or **More → Auto view**. It shows the imported or library posts, or the seed feed
when nothing is imported.

The setup dialog asks for the time per post (2 to 3600 seconds, 15 by
default) and three options: let videos play to the end (a video post stays
until the later of its time and the end of the video), start again after the
last post (otherwise an "End of posts" screen offers **Restart**), and shuffle
the order. A carousel shares its time between its slides, at least 2 seconds
each. The settings are remembered in the browser.

While it runs: Space pauses and resumes, ← and → go to the previous and next
post, + and - change the time per post, and Escape exits. It pauses while the
browser tab is hidden. Adding `#autoview=15` to the clone's address starts it
directly, with 15 seconds per post, once the feed has loaded;
`#autoview` alone uses the saved settings, and `&loop=0`, `&shuffle=1` and
`&wait=0` set the options.

### The format in brief

```text
instagram-export/            a folder, or instagram-export.zip / .tar / .tar.gz
├─ post.json
├─ posts/
│  ├─ post1.jpg
│  ├─ post2.png
│  ├─ post3.webp
│  ├─ post4.heic             HEIC / HEIF is converted in the browser
│  ├─ post5_1.jpg            carousel slides
│  ├─ post5_2.jpg
│  ├─ post6.mp4              a video (MP4, MOV or WebM)
│  └─ post6_poster.jpg       its optional poster image
└─ avatars/                  optional profile pictures
   └─ <username>.jpg
```

```json
{
  "format": "instagram-export",
  "version": 1,
  "source": { "type": "profile", "url": "https://www.instagram.com/lumen.atlas/" },
  "posts": [
    {
      "id": "C9aKq2LtXy1",
      "url": "https://www.instagram.com/p/C9aKq2LtXy1/",
      "image": "posts/post1.jpg",
      "user": { "username": "lumen.atlas", "full_name": "Lumen Atlas" },
      "caption": "The post owner's first comment, i.e. the post description.",
      "comment_count": 1234,
      "like_count": 56789,
      "taken_at": "2026-11-01T15:04:05Z"
    },
    {
      "id": "C7TtW0YhBs3",
      "url": "https://www.instagram.com/reel/C7TtW0YhBs3/",
      "media_type": "video",
      "media": [
        { "type": "video", "path": "posts/post6.mp4", "poster": "posts/post6_poster.jpg" }
      ],
      "user": { "username": "lumen.atlas" },
      "caption": "A reel.",
      "comment_count": 12,
      "like_count": 340,
      "taken_at": "2026-10-01T12:00:00Z"
    }
  ]
}
```

Per post, `user` and at least one picture or video are required. Pictures
and videos are listed in `media` (in slide order, each a path or an object
with `type`, `path`, an optional `poster` for a video and an optional `alt`);
the shorter `image` (one picture) and `images` (a carousel) still work, and so
do `video` / `videos`. `url`, `caption`, `comment_count`, `like_count` (`null`
when the owner hides it), `taken_at`, `media_type` (`image`, `carousel` or
`video`; a `video` post that ships only a picture shows it with a note that
only the cover was exported), `alt`, `location` and a sample of `comments` are
optional. The full field reference, a validator and the export procedure are
in [instagram.md](instagram.md).

The importer is lenient with hand-made exports: the manifest may be a bare
JSON array, may be called `posts.json`, and may use common alias names
(`image_path`, `from`, `description`, `comments_count`, `likes`, `permalink`,
`timestamp`, ...), also with a `post` in front and written out in words
(`post_image_path`, `post_from`, `comments_total_count`, `first_comment`).
Counts may be written as `"1,234"` or `"12.3K"`. File paths
may use backslashes and the wrong letter case, and may be written from outside
the export (`my-export/posts/post1.jpg`, `C:/Users/me/my-export/posts/post1.jpg`):
the part that names a file of the export is used. `__MACOSX` folders and other
operating-system droppings are ignored.

### Pictures and videos

- Pictures shown as they are: JPEG, PNG, GIF, WebP, AVIF and BMP.
- HEIC / HEIF (the iPhone format) is converted to JPEG in the browser. The
  decoder (about 3 MB) is downloaded from the app itself only when an export
  contains such an image. Browsers that display HEIC natively keep the
  original.
- Videos: MP4 (also `.m4v`), MOV and WebM. In the feed the most visible video
  plays muted and loops; the speaker button turns the sound on. The post view
  has the usual video controls, and grids show the poster with a video badge.
  Whether a video plays depends on its codec: H.264 MP4 (what Instagram
  delivers) plays everywhere, HEVC (H.265, common in iPhone `.mov` files)
  does not in every browser. A video the browser cannot play shows its poster
  with "This video can't be played in this browser" and a **Download video**
  link.
- The type is read from the file's content, not from its extension, so a
  mislabelled file still works. SVG, Matroska (MKV), AVI and anything else are
  skipped with a warning.
- A file path that is a web address is never fetched. Only files inside the
  export are used.

### Links back to the original

Posts do not have to come from Instagram. A post, profile or source link may
point to any site (`https://example.com/post/1`, a Mastodon status, a blog);
the label then names that site, for example **View on example.com**. The
examples below use Instagram, which is what the exporter produces.

Each imported post with a `url` gets a **View on Instagram** link in the feed,
in the menu behind **More options** and in the post view; the timestamp and
the "View all N comments on Instagram" line lead there too. Usernames open a
profile view with a **View on Instagram** button for the real profile, and the
banner links to the page the export was taken from. All of them open in a new
tab. A link must be an absolute `http` or `https` address; credentials in it
(`user:password@`) are dropped, and anything else (`javascript:`, `data:`,
relative or malformed addresses) is dropped and reported as a warning. On
`instagram.com` itself (`www.`, `m.` and `instagr.am`) a link must also lead to
what its label says (a post, a profile or a feed page) and is opened over
`https`. A post from another site is never given an `instagram.com` profile
link made up from its username.

### Privacy and storage

Exports are read in the browser. Nothing is sent to Instagram or any other
site. With the backend, the parsed posts and their media files go to the
clone's own backend, which stores them in the library folder described above,
and every browser that opens the clone shows that library. There is no
password: anyone who can reach the clone on the network can see, import into
and clear it, so use the Debug build or `--host 127.0.0.1` for a private
library, and do not run the Release build on untrusted networks. Other
websites open in the same browser cannot use it: the backend accepts changes
only from the clone's own pages (others get `403`), and it answers only when it
is addressed by an IP address, `localhost` or the computer's own name (others
get `400`, which stops DNS-rebinding pages). To open the clone under another
name, such as a DNS name you gave the computer, add that name to
`allowed_hosts` in `settings.json` or to `ALLOWED_HOSTS`.

Without a backend, the last import is kept in the browser's IndexedDB
(database `ig-clone-import`) so that it is still there after a reload, per
browser and per address. A new import then asks whether to
**Add to current import** (the default) or **Replace current import**. **Clear import** removes
it and brings the seed feed back. Videos are kept there too, which can take
much of the browser's storage quota; see [Limits](#limits).

Likes, saves and comments made on imported posts are local to the page and
are not stored.

### Limits

| Limit | Value |
| --- | --- |
| Posts per export | 5,000 (further entries are ignored with a warning) |
| Pictures and videos per post | 30 |
| Sample comments per post | 200 |
| `post.json` | 32 MB |
| One image | 64 MB |
| One video | 1 GB |
| All pictures and videos together | 4 GB per import (all archives and folders of one pick or drop) |
| Files in the folder or archive | 100,000 |
| Archives or folders per import | 50 |

Large exports also need the memory and, without a backend, the browser storage
quota to hold them. If the browser refuses to store an import, it still works
until the page is closed and a notice says that it could not be kept.

### Troubleshooting

| What you see | What to do |
| --- | --- |
| `No post.json was found` | Pick the folder that contains `post.json` (not `posts/`), or an archive of it. A `post.json` picked on its own is not enough. |
| `The zip file is damaged or incomplete` | The download or copy was cut short, or the file is not a zip. Create the archive again. With `(files overlap)` the archive lists the same stretch of data under several names, which no zip tool does; it is refused as a whole, so pack the folder again. |
| A RAR, 7z, xz, bzip2 or Zstandard file is refused | Repack as `.zip`, `.tar` or `.tar.gz`, or extract it and select the folder. Password-protected zips and split-volume zips (`.z01`, `.z02`, ...) are not supported either; separate complete archives such as `instagram-export-part1.zip` and `-part2.zip` are. |
| Warning `the image "..." is not in the export` (or `the video`) | The path in `post.json` does not match a file. Paths are relative to `post.json`. |
| Warning `is not a supported image` | The file is not one of the formats above (for example an SVG). |
| Warning `which is not supported. Convert it to MP4 (H.264) or WebM.` | The file is a video container the clone does not play (MKV, AVI, WMV, FLV, MPEG). Convert it and import again. |
| Warning `HEIC/HEIF image that could not be converted` | The file is damaged or uses a variant the decoder cannot read. Convert it to JPEG before exporting. |
| `This video can't be played in this browser` | The video's codec is not supported by this browser (usually HEVC). Use **Download video**, try another browser, or convert it to H.264 MP4. |
| Warning that a link `is not a valid http(s) link`, or is an instagram.com link but not to a post or profile, `and was ignored` | The post is imported, without that link. Use a full `https://` address. For instagram.com the `https://www.instagram.com/p/<shortcode>/` form is expected for posts and `https://www.instagram.com/<username>/` for profiles. |
| Warning `replaces the one with the same id` | The same post was in two of the archives or imports; the later copy is kept. Nothing to do unless the older copy was the better one. |
| `None of the N posts in post.json could be imported` | No entry had a usable picture or video. The message names the first problem. |
| `Not saved in the library` | The backend could not store the import (stopped, out of disk space, folder not writable, a limit reached); the message says why. The posts are shown from the browser for now. Fix the cause and reload the page: the posts are offered for **Save to library**. |
| `already holds files this app did not create` | The library folder is not empty and was not created by the clone (for example an unpacked export is in it). Nothing there is changed. Choose another folder with `INSTAGRAM_CACHE_DIR` or `instagram_cache_directory`, or move those files away. |
| `is in use by another running copy of this app` | Another backend (a second `run.bat`, or a `server.exe`) uses the same library folder. Stop it, or give this one its own folder. |
| `This host name is not allowed` | The clone was opened under a DNS name it does not know. Use the IP address, `localhost` or the computer's name, or add the name to `allowed_hosts`. |
| `Changes are only accepted from the app's own pages` | A change came from another website, or through a proxy that rewrites the address. Open the clone's own address. |
| The import is gone after a reload (no backend) | The browser could not store it (private window, storage full or blocked). Import again, or free up storage. |
| **Select folder** shows no folder picker | A few mobile browsers cannot pick folders. Use an archive instead. |

## Configuration

On Windows, the backend creates
`%APPDATA%/instagram-clone/settings.json` on its first startup.
Its `data_directory` setting defaults to the resolved
`%TEMP%/instagram-clone` folder, and the SQLite database is stored
there as `counter.db`. Development, Debug, and Release use the same settings
and data for the current Windows user.

Portable Debug and Release launchers also read `frontend_port` and
`backend_port` from this file. Before starting, `run.bat` creates an empty
JSON object (`{}`) if the file is absent and fills only missing port fields
from the `port_mapping.json` beside `server.exe`. The build copies that file
from the project root into both distributions; its values are the project
defaults (currently frontend `3127` and backend `8317`). Existing ports,
storage settings, and other fields are preserved, so `port_mapping.json` only
decides the ports on a machine (a Windows user) that has none saved yet: once
`settings.json` holds `frontend_port` and `backend_port`, editing
`port_mapping.json` changes nothing there (the portable launcher then prints a
`Ports:` line that says so). Edit the settings file (or delete its two port
fields to take them from `port_mapping.json` again) and restart the launcher to
change ports:

```json
{
  "frontend_port": 4100,
  "backend_port": 4200,
  "data_directory": "D:/CounterData",
  "instagram_cache_directory": "D:/InstagramLibrary",
  "backend_url": "http://192.168.1.20:4200"
}
```

| Setting | Purpose |
| --- | --- |
| `frontend_port`, `backend_port` | Ports of the portable build (filled from `port_mapping.json` when missing) |
| `data_directory` | Folder of the counter database `counter.db` |
| `instagram_cache_directory` | Folder of the import library (default `%TEMP%\instagram_cache`) |
| `backend_url` | Where a portable frontend started with `--role frontend` forwards `/api` |
| `allowed_hosts` | Extra host names the clone answers to, for example `["photos.home.arpa"]` (IP addresses, `localhost` and the computer's own names always work; `"*"` allows any) |

Folders are created when needed. Environment variables and `~` are expanded
using the platform's path conventions; relative paths resolve from the folder
containing `settings.json`. Unknown settings are preserved. Invalid JSON or an
invalid `data_directory`, `instagram_cache_directory` or `backend_url` stops
startup with an error.

Temporary folders can be cleared by the operating system, so choose a permanent
folder for data you want to retain. Existing databases and libraries are not
moved automatically. With the application stopped, copy or move an existing
`counter.db` into the chosen folder, or point `COUNTER_DB_PATH` at the old file;
move the contents of the old library folder into the new one.

On Linux and macOS, settings are stored under
`$XDG_CONFIG_HOME/instagram-clone/settings.json`, or
`~/.config/instagram-clone/settings.json` when `XDG_CONFIG_HOME` is
unset. The data folder defaults to `instagram-clone` under `TEMP`,
or the system temporary directory when `TEMP` is unset; the library likewise
defaults to `instagram_cache` there.

Copy `.env.example` to `.env` to customize the preferred ports, bind address,
or explicitly override the database file or the library folder. Environment
variables already defined in the terminal take precedence over values in
`.env`.

| Variable | Default | Purpose |
| --- | --- | --- |
| `BACKEND_PORT` | `8000` | First backend port to try |
| `FRONTEND_PORT` | `5173` | First frontend port to try |
| `BACKEND_HOST` | `0.0.0.0` | Bind address for both frontend and backend |
| `COUNTER_DB_PATH` | Unset | Override the settings-based SQLite database path |
| `INSTAGRAM_CACHE_DIR` | Unset | Override the settings-based library folder |
| `ALLOWED_HOSTS` | Unset | Comma-separated extra host names, added to `allowed_hosts` |
| `CORS_ORIGINS` | The Vite dev server (development); none (portable build) | Comma-separated origins of other web apps that may call the API |

Relative `COUNTER_DB_PATH` and `INSTAGRAM_CACHE_DIR` overrides resolve from the
project root in development, or beside `server.exe` in a packaged build.

## API

| Method | Path | Behavior |
| --- | --- | --- |
| `GET` | `/api/health` | Backend health check |
| `GET` | `/api/counter` | Read the saved count |
| `POST` | `/api/counter/click` | Increment and return the count |
| `DELETE` | `/api/counter` | Reset and return the count |
| `GET` | `/api/library` | The library: folder, revision, imports and posts (`503` when the folder cannot be used) |
| `DELETE` | `/api/library` | Clear the library |
| `GET` | `/api/library/files/{path}` | A picture or video of the library (`posts/` and `avatars/` only; supports `Range`) |
| `POST` | `/api/library/uploads` | Start an upload session |
| `PUT` | `/api/library/uploads/{id}/files/{name}` | Upload one media file (raw body, streamed to disk) |
| `POST` | `/api/library/uploads/{id}/commit` | Merge the uploaded posts into the library |
| `DELETE` | `/api/library/uploads/{id}` | Abort an upload session |
| `DELETE` | `/api/library/posts/{id}` | Remove one post and its files |

Interactive API documentation is available at the backend `/docs` URL.

## Portable build

Build a portable Windows distribution (requires Python 3.10+ and Node.js/npm
on the build machine):

```powershell
.\build.bat
# Only this web package, not the native app: .\build.bat --web-only
# Equivalent for the web package alone: python build.py
```

`build.bat` builds the whole application: these web packages and then the
native Windows app (`--web-only` skips it, `--app-only` skips the web
packages, `--skip-install` and `--skip-tests` forward to the matching build).

The build installs dependencies in the active Python environment, or creates
and uses the project `.venv`, then builds React and packages FastAPI with
PyInstaller. Use `build.bat --skip-install` to reuse installed dependencies.
Its work files (`build\pip-cache`, `build\npm-cache`,
`build\pyinstaller-cache`, staging folders) stay in `build\`; the result is
in `build\dist`:

```text
build/dist/
  Debug/                  Local version (default host: 127.0.0.1)
    public/               Built frontend HTML, JavaScript, and CSS
    server.exe            Standalone backend and frontend server
    run.bat               Starts frontend and backend together
    run-frontend.bat      Starts only the frontend (run.bat --role frontend)
    run-backend.bat       Starts only the backend (run.bat --role backend)
    runtime.json          Generated default bind address
    port_mapping.json     Project defaults for missing settings ports
    README.txt            How to run it, roles, ports, the library folder
  Release/                LAN version (default host: 0.0.0.0)
    (the same files)
```

Both folders contain the same packaged application, with different default
bind addresses selected by their generated `runtime.json` files. To run it on
another Windows machine, copy the **whole** `Debug` or `Release` folder there
(or unzip the matching archive from the CI build); Python and Node.js are not
required. `server.exe` needs `public`, `runtime.json` and `port_mapping.json`
beside it. Before you pass a build on to other people, read
[Third-party software](#third-party-software): the folder contains an LGPL
library, whose licence texts are in `public/licenses`. Start it with:

```powershell
.\build\dist\Debug\run.bat
.\build\dist\Release\run.bat
.\build\dist\Release\run.bat --frontend 4000 --backend 9000
```

Debug binds to `127.0.0.1` for local access; Release binds to `0.0.0.0` for
LAN access. Both use the ports saved in the per-user `settings.json` of the
machine they run on, initializing missing fields from their included
`port_mapping.json` (see [Configuration](#configuration)), so copying a folder
to another machine brings the project's default ports with it. To give a
build other ports, edit `port_mapping.json` in `build\dist\<profile>` before
the first start on each machine (a rebuild writes it again from the project
root's `port_mapping.json`, so change that one for every future build); on a
machine that has already run the app, change `settings.json` instead.
`--frontend-port` and `--backend-port` are also accepted. Ports must be
distinct, available integers between 1 and 65535. Open the printed frontend
URL, and press Ctrl+C in the launcher terminal to stop the services. The path
of the folder must not contain a `%` character; the launchers refuse to start
from such a folder.

### Roles: frontend and backend on different machines

`server.exe` runs in one of three roles, chosen with `--role` (the
`run-*.bat` files pass it for you; other arguments are passed through):

| Role | Launcher | What it serves |
| --- | --- | --- |
| `all` (default) | `run.bat` | Frontend and backend in one process; the frontend forwards `/api` to the backend |
| `backend` | `run-backend.bat` | Only the API on the backend port, including the library. It prints its LAN addresses and the `Cache:` folder. |
| `frontend` | `run-frontend.bat` | Only `public` on the frontend port, forwarding every `/api` request to `--backend-url` |

To split them, use the Release build on both machines (the Debug backend only
listens on `127.0.0.1`):

```powershell
# Machine A (keeps the database and the library):
.\run-backend.bat
# Machine B (what the browsers open), with machine A's printed LAN address and backend port:
.\run-frontend.bat --backend-url http://192.168.1.20:8317
```

Instead of `--backend-url`, machine B can set `backend_url` in its
`settings.json`. Without either, the frontend role forwards to the backend
port on its own machine. Browsers only talk to machine B; uploads and videos
are streamed through it. Machine A's firewall must allow `server.exe` on the
backend port, and machine B's on the frontend port. `--backend-url` accepts
only an `http://` or `https://` address with a host and a port, and is only
used with `--role frontend`. Give machine A's IP address or computer name
there; for any other DNS name, add it to `allowed_hosts` in machine A's
`settings.json`.

### Overrides and updates

The executable serves `public` and forwards frontend `/api` requests to the
selected backend, so changing ports does not require rebuilding React.
An optional `.env` beside each `server.exe` supports `FRONTEND_PORT`,
`BACKEND_PORT`, `BACKEND_HOST`, `COUNTER_DB_PATH` and `INSTAGRAM_CACHE_DIR`.
Command-line ports take precedence over environment variables, which take
precedence over the saved settings ports. Overrides apply to that launch and
do not change saved ports. Invalid JSON, missing or invalid mapping defaults,
or invalid saved port values stop startup with an error. For the bind
address, `--host` takes precedence over `BACKEND_HOST`, which takes precedence
over the generated `runtime.json` default. Both distributions use the per-user
`settings.json`, data directory and library folder described above. Relative
`COUNTER_DB_PATH` and `INSTAGRAM_CACHE_DIR` overrides resolve beside
`server.exe`, regardless of the launcher's working directory.

Rebuilding replaces only `public`, `server.exe`, the three launchers,
`runtime.json`, `port_mapping.json` and `README.txt` in `build\dist\Debug` and
`build\dist\Release`, and keeps everything else there, such as a `.env` or
data files you added. Builds made before this layout went to `dist\` in the
project root; the build no longer updates that folder, so move any `.env` you
keep there into `build\dist\<profile>`.

The `Build and release Windows binaries` GitHub workflow runs the same build
and publishes `build\dist\Debug`, `build\dist\Release` and the native app's
`windows-app\build\dist\Debug` and `windows-app\build\dist\Release` as zip
archives.

## Tests

```powershell
python -m unittest discover -s backend\tests -v
cd frontend
npm run typecheck
npm test
npm run build
cd ..\e2e
npm run typecheck
npm test
```

`npm test` in `frontend` runs the Vitest unit tests of the importer
(`frontend/src/import/*.test.ts`: archive readers, `post.json` parsing, image
and video detection, merging several sources, the library client, link
sanitising, IndexedDB persistence) and of auto view. They also keep the
documentation honest: `docs.test.ts` imports every `post.json` example of
this README, of `instagram.md` and of the import dialog and expects no
warning, and compares the quoted messages, button labels, limits and the link
checks of the validator in `instagram.md` with the code.

The Playwright suite starts both FastAPI and Vite automatically, then exercises
the live API through the frontend proxy using an isolated test database and an
isolated library folder (`INSTAGRAM_CACHE_DIR` points into the test results,
never at the real `%TEMP%\instagram_cache`). It uses Microsoft Edge.

`e2e/tests/import.spec.ts`, `video.spec.ts`, `library.spec.ts` and
`autoview.spec.ts` drive the importer, video playback, the library and auto
view in the real browser with the exports in `e2e/fixtures/`: a folder, zip and
tar.gz archives, a real HEIC picture, real MP4 and WebM clips, an export split
over two archives, a hand-made manifest and one full of problems. The tests
answer requests to instagram.com with a stub, so nothing leaves the machine.
The fixtures are synthetic and committed; `e2e/fixtures/generate-fixtures.py`
recreates them (it needs Python with `pillow` and `pillow-heif`, the tests do
not), and `e2e/fixtures/record-videos.ts` records the clips in Edge.

## Third-party software

The project's own code is licensed under the Apache License 2.0 (see
`LICENSE`). A built frontend (`frontend/dist`, or the `public` folder of a
portable build) also contains code from these packages:

| Package | Used for | Licence |
| --- | --- | --- |
| [heic-to](https://github.com/hoppergee/heic-to) 1.6 | Converting HEIC/HEIF pictures in the browser. It contains [libheif](https://github.com/strukturag/libheif) and [libde265](https://github.com/strukturag/libde265), compiled to JavaScript. | LGPL-3.0 |
| [fflate](https://github.com/101arrowz/fflate) 0.8 | Reading compressed data in `.zip` and `.tar.gz` exports | MIT |
| [React](https://github.com/facebook/react) 18 (`react`, `react-dom`, `scheduler`) | The user interface | MIT |

`heic-to` is the one package here that is not under a permissive licence. It
is not mixed into the application's own bundle: Vite emits it as a separate
file (`assets/heic-to-<hash>.js`, about 3 MB) that is loaded only when an
import contains a HEIC/HEIF picture. Because the whole application is open
source, the frontend can be rebuilt with another or a modified version of the
library: change the dependency in `frontend/package.json`, then run
`npm install` and `npm run build` in `frontend`.

The build minifies these packages and removes their licence headers, so the
licence texts travel next to the code. `frontend/public/licenses/` holds a
copy of each package's `LICENSE` file (the one of `heic-to` is its copyright
notice followed by the LGPL 3.0), the text of the GNU GPL 3.0, which the LGPL
builds on, and a `README.txt` that says which file belongs to which package.
Vite copies that folder into every build: it ends up in
`frontend/dist/licenses` and, in a portable build, in `public/licenses` of
both `build/dist/Debug` and `build/dist/Release`. `npm test` in `frontend`
fails when a shipped text differs from the installed package or a new
dependency has none.

The build does not contain the project's own `LICENSE`. If you give a build
to someone else, add that file.

`server.exe` additionally contains the Python runtime and the Python packages
from `backend/requirements.txt` with their dependencies, each under its own
licence.

## Project layout

```text
backend/                  FastAPI application, SQLite store, import library, and unit tests
backend/packaged.py       Entry point of server.exe (roles, ports, the /api proxy)
frontend/                 TypeScript React/Vite application
frontend/src/import/      In-browser importer for Instagram exports, with unit tests
frontend/public/licenses/ Licence texts of the bundled packages, copied into every build
e2e/                      Standalone TypeScript Playwright end-to-end tests
e2e/fixtures/             Synthetic exports used by the import tests, and their generator
windows-app/              Native Windows viewer (Rust eframe/egui workspace: core + app)
windows-app/web/dist/     The browser pages it embeds: a build of frontend/
windows-app/README.md     The native app's own documentation
instagram.md              Runbook: export Instagram posts with a browser MCP client
port_mapping.json         Default ports copied into every portable build
secrets.md                Secret-scan audit trail (no secret values)
setup.py / setup.bat      Dependency setup (Python, Node, and the native app's Rust)
run.py / run.bat          Combined backend/frontend runner (the web stack)
run-app.bat               Native Windows app runner (cargo run in windows-app/)
build.py / build.bat      Builds everything: build/dist (web) and windows-app/build/dist
update-web.bat            Rebuilds windows-app/web/dist from frontend/ (needs Node.js)
```
