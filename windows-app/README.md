# Instagram Windows App

A native Windows image viewer written in Rust (eframe/egui, drawn with
DirectX 12). It shows the pictures of a folder the way Instagram shows posts:
the layout, colours, icons and type sizes follow the Instagram clone, the
React web frontend in this repository it is the native counterpart of, and its light and
dark palettes are reproduced in `app/src/theme.rs`. It keeps two features of
the clone, editing the description of a post and downloading posts as an
export, and what it downloads is an archive the clone imports. What is open
can also be seen in a web browser, with the clone's own pages.

Highlights:

- **Opens what you have**: one picture or video (with the other pictures
  and videos of its folder), several of them, a folder, or an Instagram
  export (a folder with `post.json`, or a `.zip`, `.tar`, `.tar.gz` or
  `.tgz` archive of one, several of them merged into one feed), from the
  command line, Explorer's "Open with", a dialog or by drag and drop. See
  [Opening pictures](#opening-pictures).
- **Videos**: MP4, MOV and WebM videos play with sound, a bar of controls and
  keys in the viewer and auto view, and in the feed with a graphics card. See
  [Videos](#videos).
- **Home feed, Explore grid and a viewer** in the clone's look: post cards
  with like, save and double-click to like, a three-column grid, and a post
  view with the picture contained on black.
- **Image-viewer controls** the clone does not have: previous and next
  picture, zoom around the pointer up to 800 %, drag to pan, fullscreen.
- **Edit description**: every picture can get a description of up to 10,000
  characters. Descriptions are kept in one file in the app's data folder;
  picture folders and exports are never modified.
- **Download post** and **Download all**: the pictures with their
  descriptions as an `instagram-export` `.zip`, the format the Instagram
  clone imports.
- **Auto view**: a slideshow that moves to the next picture after a time you
  choose (15 seconds by default).
- **Light / dark appearance** (**More → Switch appearance**), remembered
  between runs.
- **Browser pages**: the posts that are open, shown by the clone's own web
  pages at `http://127.0.0.1:3127/`, with **Edit description**, downloads
  and imports there too (**More → Open in browser**). See
  [Browser](#browser).
- **One portable exe**: `build.bat` produces `build\dist\Debug` and
  `build\dist\Release`, each a folder with a single executable that needs
  nothing installed. See [Build](#build).

The viewer works offline. It reads the files you open and writes only into
its own data folder, its archive cache, the browser imports folder and the
archives you ask it to download; nothing is sent anywhere. For the browser
pages it listens on 127.0.0.1, this computer only, and on the network only
when you allow it in **Browser view**. Links of an export, such as
**View on Instagram**, are handed to your browser.

## Quick start

Requirements, on 64-bit Windows 10 or 11:

- Rust, the stable toolchain `stable-x86_64-pc-windows-msvc`, from
  <https://rustup.rs>.
- The Visual Studio C++ build tools with a Windows SDK: the workload "Desktop
  development with C++" of Visual Studio 2022 or of the Build Tools for Visual
  Studio 2022.

Nothing else: no Python, no Node.js, no CMake, NASM or Clang. Every
dependency is built by cargo with the MSVC tools. (Node.js is used only by
optional tests and by `update-web.bat`, which rebuilds the browser pages;
see [Tests](#tests) and [Build](#build).)

```powershell
.\setup.bat
.\run.bat
# Open a folder or a picture right away:
.\run.bat C:\Users\me\Pictures\Holiday
```

`setup.bat` checks the toolchain, says what to install when something is
missing, and downloads the crates named in `Cargo.lock`
(`cargo fetch --locked`). `run.bat` builds the debug profile and starts it
with the arguments you pass; it runs setup first when setup has not passed
yet. The first build compiles about 190 crates and takes a few minutes (six
on two cores); later builds take seconds.

## Opening pictures

### Ways in

| Way | What to do |
| --- | --- |
| Command line | Pass pictures and folders as arguments: `instagram-windows-app.exe C:\Pictures\IMG_2.jpg`. Explorer's "Open with" and dragging files onto the exe do the same (see [Default viewer](#default-viewer)). |
| **Open pictures** | In the sidebar, in the empty window, or with Ctrl+O. The dialog takes one or several files. Its filters: **Pictures, videos and exports** (the picture and video extensions and `.zip`, `.tar`, `.tgz`, `.tar.gz`), **Pictures and videos**, **Export archives** and **All files**, which shows everything, because a picture or a video is recognised by its content. |
| **Open folder** | In the sidebar, in the empty window, or with Ctrl+Shift+O. |
| Drag and drop | Drop pictures, videos, archives or a folder anywhere on the window (`Drop pictures, videos or a folder`). Nothing is taken while a dialog is open or auto view runs. |
| **Open more** | In the banner above the feed: the **Open pictures** dialog again. |
| **More → Open the web app's library** | Listed when the Instagram clone's backend keeps a library with a `post.json` on this computer (see [Pictures and exports](#pictures-and-exports)). |
| **More → Open browser imports** | Listed when the browser imports folder holds a `post.json` and is not open (see [Browser](#browser)). |

The dialogs start in the folder that was opened last.

### What is opened

| You open | The viewer shows |
| --- | --- |
| One picture or video | Every supported picture and video directly in its folder, with the viewer open on the one you picked |
| Several pictures or videos | Exactly those files |
| A folder | Every supported picture and video directly in it; subfolders are not searched |
| A folder that holds `post.json` (or `posts.json`), or that file itself | The export: its posts with authors, descriptions, counts, carousels and sample comments |
| A folder without pictures of its own that holds an export one folder down (an extracted download) | That export |
| An archive: `.zip`, `.tar`, `.tar.gz` or `.tgz` | The export inside it, as if it were extracted |
| Several export folders and archives | One feed of all their posts, newest first |
| A folder without `post.json` and without picture files that holds archives (a Downloads folder) | Those archives, merged |

A folder with pictures or videos directly in it always opens as those, even
when an export lies in one of its subfolders or an archive lies in it. If
the one file you picked cannot be shown (an empty file, not a supported
picture or video), a message says why at once.

Loose pictures and videos become one post per file, never a carousel, in
the order Explorer shows them: names are compared without regard to case
and runs of digits as numbers (`img_1.png`, `IMG_2.jpg`, `IMG_10.jpg`). The
author of every loose picture or video is the `default_username` setting
(your Windows account name unless you set it), the time is the file's
modified time, and the line under the author is the file name.

Opening more while something is shown: picture files are added to loose
pictures that are already open (a file that is already there is not added
twice); export folders and archives are added to exports that are already
open; anything else replaces what is shown (a folder of pictures, pictures
while exports are shown, exports while loose pictures are shown). Exports
are shown on their own: pictures named together with exports are not
opened and a warning says so. One picture you open or drop is then the one
on show: the viewer opens on it, or moves to it when it is open. Files of
which none can be shown are explained in a message. Pictures that arrive
while **Edit description** is open wait until the dialog is closed, so your
text is never lost to them.

A banner above the feed says what is open, for example
`Showing 5 pictures from Holiday`, `Showing 2 videos from Holiday`,
`Showing 7 pictures and videos from Holiday`,
`Showing 6 posts from export-basic`,
`Showing 6 posts from export-basic.zip`, `Showing 3 posts from 2 archives`,
or `Showing 5 of 7 posts from export-problems` when entries of `post.json`
were skipped. It offers **Auto view**, **Download all**, **Open more** and
**Close**. Files that could not be opened and entries that had to be skipped
or repaired become warnings: the banner then has a button such as
**3 warnings**, which lists the first five; **Show all 12 warnings** opens
the rest and **Show fewer** folds them again.

### Pictures and exports

- JPEG, PNG, GIF, WebP and BMP are decoded by the viewer itself. The EXIF
  orientation of a photo is applied. Of an animated GIF or WebP the first
  frame is shown.
- HEIC / HEIF (the iPhone format) and AVIF are decoded by Windows. That needs
  the "HEIF Image Extensions" from the Microsoft Store, together with the
  "HEVC Video Extensions" for HEIC or the "AV1 Video Extension" for AVIF.
  Without them the picture shows a placeholder that says so; it can still be
  downloaded.
- The type is read from the file's content, not from its extension, so a
  mislabelled picture still opens. Other files in a folder are passed over
  silently, unless their extension says they should be a picture or a video
  (`.mp4`, `.m4v`, `.mov`, `.webm`); a file you picked yourself that cannot
  be shown always becomes a warning.
- MP4 (also `.m4v`), MOV and WebM videos open like pictures, up to 1 GB
  each, and play (see [Videos](#videos)). A Matroska (`.mkv`), AVI or WMV
  file you pick is refused with a message that says so. A video of an
  export is kept, plays, and is downloaded byte for byte.
- An export is read with the lenient rules of the Instagram clone: the
  manifest may be a bare JSON list, may be called `posts.json`, and may use
  the alias names the clone accepts. A file path that is a web address is
  never fetched; only files inside the export are used.

A picture that cannot be decoded shows a placeholder with the reason. It
stays in the collection and is downloaded as the file it is.

**More → Open the web app's library** opens the library of the Instagram
clone's backend, the folder where it keeps what was imported in the
browser, when that folder holds a `post.json`: `INSTAGRAM_CACHE_DIR` when it
is set to a full path, else `instagram_cache_directory` of
`%APPDATA%\instagram-clone\settings.json`, else
`%TEMP%\instagram_cache`. It opens as an export folder and is only read:
descriptions you write for its posts go to `descriptions.json`, never into
the library.

### Archives

An export the Instagram clone wrote, or one it can import, often comes as an
archive. Open it like a folder: pass it on the command line, pick it in
**Open pictures** (filter **Export archives**), or drop it on the window.
Several archives, or archives and export folders, opened together become one
feed. Archives are read the way the Instagram clone reads them, with the
clone's own rules and messages:

- `.zip` (stored or deflated members, ZIP64, names in UTF-8 or the DOS code
  page, a self-extracting prefix), `.tar` (ustar, GNU long names, pax) and
  `.tar.gz` / `.tgz`. RAR, 7z, xz, bzip2 and Zstandard archives are
  recognised and refused with a message.
- `post.json` may lie at the top of the archive or inside one top-level
  folder (the shallowest one is taken); its folder is the export. Files
  macOS adds (`__MACOSX`, `._*`, `.DS_Store`) are ignored.
- A member that cannot be read (password protected, an unknown compression
  method, damaged) becomes a warning of its post, as a missing file would.
  An archive whose members share their bytes, or that unpacks to more than
  it declares, is refused: such archives are made to fill a disk.
- Several export folders and archives opened together are merged exactly as
  the clone merges them: a post whose id is already there replaces the
  earlier one and a warning says so (`replaces the one with the same id`),
  the posts are sorted newest first, and the warnings of each source start
  with its name. At most 50 sources are read at a time, and a source that
  cannot be read becomes a warning while the others are shown.
- The archive itself is only read, and nothing is ever written next to it.
  The pictures and videos post.json names are copied into the archive cache,
  `%LOCALAPPDATA%\instagram-windows-app\archives` (file names there are made
  by the viewer, never taken from the archive). The next time the same
  archive, unchanged, is opened, the cache is used and the archive is not
  read at all. Opened together, the archives of one opening may add up to 4
  GB of pictures and videos.
- The cache cleans itself up at every start and after every new archive:
  older versions of an archive go at once, archives not opened for 30 days
  go, and beyond 8 GB the least recently used go first. What is on screen
  in any window of the viewer is never removed. Unpacking stops when less
  than 1 GB would stay free on the drive.
- A description you write for a post of an archive is kept in
  `descriptions.json` under the path of the archive and the id of the post
  (see [Edit description](#edit-description)), so it is still there when the
  cache has been cleaned up or the archive is opened together with others.
  **Download post** and **Download all** write the files of the cache, byte
  for byte, as a new export; **Show in folder** shows the archive itself.

### Limits

| Limit | Value |
| --- | --- |
| One picture file | 64 MB (a larger file is skipped with a warning) |
| One loose video file | 1 GB (a larger file is skipped with a warning) |
| Files in an opened folder | 100,000 |
| Pixels of one picture | 256 megapixels (a larger picture is not shown, but can be downloaded) |
| Longest side of a picture on screen | 8,192 pixels (a larger picture is scaled down) |
| Zoom | 800 % |
| Description | 10,000 characters |
| Posts of an export | 5,000 (further entries are ignored with a warning) |
| Pictures and videos per post of an export | 30 |
| Sample comments per post of an export | 200 |
| `post.json` | 32 MB |
| One video of an export | 1 GB |
| Export folders and archives opened at a time | 50 (further ones are left out with a warning) |
| Pictures and videos of the archives of one opening | 4 GB |
| An unpacked `.tar.gz` | 4.1 GB |
| Archive cache | 8 GB; an archive not opened for 30 days is removed; 1 GB of the drive stays free |
| Posts per downloaded archive | 5,000 (more are split into parts) |
| Pictures and videos per downloaded archive | 2 GB (more is split into parts) |
| Time per post in auto view | 2 to 3600 seconds |
| Videos playing at a time | 1 (one player each for the feed, the viewer and auto view; 3 at most) |
| Frames of a video with a graphics card | 1,920 pixels on the longest side (a larger video is scaled down) |
| Frames of a video without a graphics card | 230,400 pixels (640 x 360), 10 repaints a second |
| A paused video of the feed is kept | 30 seconds |
| Poster of a loose video | 1,080 pixels on the longest side |
| Poster cache | 256 MB (the oldest posters go first) |
| Auto view waits for a video without a length | 600 seconds |

A description is counted the way the clone counts it, in UTF-16 code units:
most emoji count as two characters.

### Troubleshooting

| What you see | What to do |
| --- | --- |
| `No pictures were found in` | The folder has no supported picture directly in it. Subfolders are not searched: open the subfolder, or pick the pictures. |
| Warning `so only the picked picture was opened` | The folder of the picture you opened has more than 100,000 files; its other pictures are not read. Open them from a smaller folder. |
| Warning `belongs to an earlier file of that name and is not shown` | The picture was replaced by another file of the same name (its size and modified time both differ from when the description was saved). The description stays in `descriptions.json`; write a new one for the new picture. |
| Warning `is not a supported image` | The file you picked is not one of the formats above (for example an SVG or a text file). |
| Warning `is larger than 64 MB and was skipped` | The file is over the size limit for one picture. |
| Warning `is larger than 1 GB and was skipped` | The video is over the size limit for one video. |
| `can't be played: Windows has no decoder for its` | Windows lacks the codec of the video; the message names the Microsoft Store extension that brings it ("HEVC Video Extensions" for most iPhone videos, "VP9 Video Extensions", "AV1 Video Extension", "Web Media Extensions"). Install it and open the video again, or convert the video to MP4 (H.264). |
| `can't be played: the file is damaged or incomplete` | The video was not copied completely, or it is no video Windows can read. Get it again, or convert it to MP4 (H.264). |
| `can't be played: it has no picture.` | The file holds sound only. The viewer shows videos, not sound files. |
| A video starts about two seconds after it is shown | The computer has no sound output (a virtual machine, a remote session without sound), and Windows waits that long for one before it plays a video with a sound track. The poster is shown meanwhile. |
| `could not be shown: Windows has no decoder for it` | A HEIC or AVIF picture, and Windows lacks the codec. Install the "HEIF Image Extensions" and the "HEVC Video Extensions" (HEIC) or the "AV1 Video Extension" (AVIF) from the Microsoft Store and open the pictures again. |
| `is too large to show` | The picture has more than 256 megapixels. It can still be downloaded. |
| Warning `was not opened: an export is shown on its own.` | An export was opened together with other files. Open the pictures separately. |
| Warning `is not in the export` | A path in `post.json` does not match a file. Paths are relative to `post.json`. |
| `No post.json was found in` | The archive (or folder) is not an Instagram export. Open the pictures in it as pictures: extract the archive and open the folder. |
| `The zip file is damaged or incomplete` or `The archive is damaged or incomplete` | The archive was not downloaded or copied completely, or it is no zip file. Get it again. |
| `archives are not supported` or `is an archive type that is not supported` | RAR, 7z and the like are not read. Extract the archive with the program that made it and open the folder, or pack it again as `.zip`. |
| Warning `is password protected and cannot be read` or `uses a zip compression method that is not supported` | That file of the archive cannot be unpacked. Re-create the archive without a password and with standard (deflate) compression. |
| Warning `could not be imported:` | One of several archives or folders could not be read; the others are shown. The warning says why. |
| Warning `replaces the one with the same id` | Two of the exports you opened have a post with the same id; the one opened last is shown. |
| `Not enough free disk space` | The archive cache keeps 1 GB of its drive free. Free some space and open the archive again. |
| `add up to more than` | The archives opened at once hold more than 4 GB of pictures and videos. Open them in smaller groups. |
| `The description could not be saved` | `descriptions.json` could not be written (the data folder is read-only, the disk is full, or another program holds the file); the message says why. The dialog stays open with your text: fix the cause and choose **Save** again. |
| `could not be read. The file was kept as` | `descriptions.json` was damaged. The viewer set it aside under the name in the message and started a new one; the old file is still in the data folder. The message stays until you choose **OK**. |
| `Invalid settings at` or `Cannot read or create settings at` (a message box at the start) | `settings.json` is not valid JSON or a value in it is unusable. The message names the file; correct or delete it. The viewer never changes or replaces the file. |
| `Invalid data_directory in settings at` (a message box at the start) | The data folder cannot be created: a file has its name, the name is not allowed on Windows, or the drive cannot be written to. Correct `data_directory` in `settings.json`. |
| `A download is already being prepared.` | One download runs at a time. Wait for it, or cancel it in the banner. |
| `The archive could not be written` | The folder chosen in the Save dialog cannot be written to, or the disk is full, or another program holds the archive of that name (the message then names it); the message says why. Nothing is left behind there. |
| `Nothing could be downloaded.` | No file of the posts could be read any more, for example because the pictures were moved after they were opened. Open them again. |
| `Enter a whole number of seconds from 2 to 3600.` | The time per post in the auto view dialog must be a whole number in that range. |
| Scrolling and zooming are slow | The computer has no graphics adapter that supports DirectX 12 (a virtual machine, some remote sessions), so Windows draws in software, on the processor. The viewer adapts to that (see [Known limitations](#known-limitations)), but every pixel still costs processor time: a smaller window is smoother. |
| Videos do not play in the feed, only behind **Play video** | Windows draws in software (see above). Videos then play in the viewer and in auto view only, in smaller frames; see [Videos](#videos). |
| Nothing appears for `--help` or `--version` | The Release exe has no console of its own. Start it from a command prompt or a PowerShell window, or use the Debug exe. |
| `The browser pages could not be started` | No port was free (`No free port was found`) or the address of `web_host` / `--host` is not one of this computer (`cannot be used`). Choose other ports in **Browser view**, or correct the address. |
| `is used by another program` in **Browser view** | The typed port is taken. Choose another port; nothing was changed. |
| The page address does not open on another device | Turn on **Allow other devices on this network** in **Browser view**, use an address it lists, and allow the viewer in the Windows Defender Firewall prompt (or in its settings) for private networks. |
| `This host name is not allowed` | The browser reached the viewer by a name that is not an address, `localhost` or this computer's name. Use the address shown in **Browser view**, or add the name to `allowed_hosts` in `settings.json`. |
| `Changes are only accepted from the app's own pages.` | A change came from another web site. Make changes from the viewer's own pages. |
| `Instagram Windows App is showing loose pictures` | Imports from the browser need an export or nothing open in the viewer. Close the loose pictures there, or open an export or archive, and import again. |
| `in use by another running copy of this app` | Another copy of the viewer uses the browser imports folder. Close it, or give this copy its own folder with `library_directory`. |

## Views

The sidebar has **Home**, **Explore**, **Open pictures**, **Open folder**,
**Auto view** and, at the bottom, **More** with the menu entries
**Auto view**, **Switch appearance**, **Open in browser** (while the browser
pages run), **Browser view**, **Download all**, **Open browser imports**
(only while the browser imports folder can be opened),
**Open the web app's library** (only while the Instagram clone keeps a
library on this computer), **Close pictures** and **About**. In a window
narrower than 1264 pixels the sidebar becomes a rail of icons. The smallest
window is 640 x 480.

The window opens where it was closed, at its size, but never larger than
the screen without the taskbar; the first window is centred on the screen.
A window that was closed while minimised or in fullscreen comes back as a
normal window, and the window disappears as soon as you close it.

- **Home** is the feed: a 470-pixel column of post cards with the author,
  the time, the picture (cropped to between 4:5 and 1.91:1, as Instagram
  does), like, comment, share and save, the likes line when the export knows
  a count, the description with `more` for a long one, and the date.
  Double-click a picture to like it. The comment button opens the viewer.
  The share button copies the link of the post (`Link copied.`) or, for a
  loose picture, the path of the file (`Path copied.`).
- **Explore** is a grid of square tiles, three in a row. A tile shows its
  counts under the pointer and opens the viewer.
- The viewer shows one post: the whole picture on black, and beside it the
  author, the full description, the sample comments of an export, the
  actions and the date. Arrows at the left and right edge lead to the
  previous and the next picture. The window title names the file, for
  example `IMG_2.jpg - Instagram Windows App` (for a post of an archive, the
  name its `post.json` gives the file).

Likes, saves and comments are local to the window and are not stored, as in
the clone.

**More options**, the three dots of a post, opens these entries; each is
only there when it applies:

| Entry | What it does |
| --- | --- |
| **View on Instagram** | Opens the post's link in the browser. Posts of an export with a link have it; the label names the site, whatever it is. |
| **View profile on Instagram** | Opens the author's profile link in the browser. |
| **Copy link** | Copies the post's link (`Link copied to clipboard.`). |
| **Go to post** | Opens the viewer; not shown inside the viewer. |
| **Edit description** | See [Edit description](#edit-description). |
| **Download post** | See [Download post and Download all](#download-post-and-download-all). |
| **Show in folder** | Opens Explorer with the picture's file selected; for a post of an archive, the archive. |
| **Auto view from here** | Starts auto view at this post. |
| **Cancel** | Closes the list. |

**More → About** shows the version, the data folder, the settings file and
where the browser pages are (or why they are off); **Browser view** opens the
sheet of [the browser pages](#browser-view), and **Open data folder** opens
the data folder in Explorer.

### Keyboard

| Where | Keys | What happens |
| --- | --- | --- |
| Anywhere | Ctrl+O | Open pictures |
| Anywhere | Ctrl+Shift+O | Open folder |
| Anywhere | F11 | Fullscreen on and off |
| Anywhere | Escape | Close what is on top; with nothing open, leave fullscreen |
| Home and Explore | Home / End | Jump to the top or the bottom |
| Home and Explore | Page Up / Page Down | Scroll by one screen |
| Home and Explore | Up / Down | Scroll by a small step |
| Viewer | Left / Right | Previous or next slide of a carousel, then the previous or next picture |
| Viewer | Home / End | First or last picture |
| Viewer | + / - | Zoom in or out |
| Viewer | 0 | Fit the picture to the window |
| Viewer | Space / K | Play or pause the video |
| Viewer | M | Sound of videos on or off |
| Viewer | Shift+Left / Shift+Right | Back or forward 5 seconds in the video |
| Viewer | Escape | Leave fullscreen, then close the viewer |
| Edit description | Ctrl+Enter | Save |
| Edit description | Enter | New line |
| Edit description | Escape | Close without saving |
| Auto view setup | Enter | Start (while the seconds field has the focus) |
| Auto view | Space | Pause and resume; on the end screen, start again |
| Auto view | Left / Right | Previous or next post |
| Auto view | + / - | More or less time per post |
| Auto view | M | Sound of videos on or off |
| Auto view | Escape | Exit auto view |
| Dialogs and menus | Escape | Close the dialog, sheet or menu on top |
| Dialogs and menus | Tab / Shift+Tab | Move the focus to the next or previous control |
| Dialogs and menus | Enter or Space | Press the focused button |

While a button has the keyboard focus, the arrow keys, Enter and Space
belong to that button. Ctrl+O and Ctrl+Shift+O do nothing while a dialog is
open or auto view runs. `instagram-windows-app.exe --help` prints this table.

With the mouse in the viewer: the wheel zooms around the pointer (a factor
of 1.2 per notch, 1.25 per key press), a double click switches between the
fitted picture and 100 %, dragging moves a picture that is larger than its
frame, and the back and forward buttons of a mouse go to the previous and
next picture. The mouse settings of Windows apply: a double click is two
clicks within the double-click time, and one notch of the wheel scrolls the
feed as far as a browser scrolls a page (100 pixels at three lines per
notch).

### Appearance

The viewer starts in the light appearance, as the clone does.
**More → Switch appearance** changes to dark and back; the title bar of the
window follows, and the choice is remembered in `state.json`.

## Videos

MP4 (also `.m4v`), MOV and WebM videos play: loose video files and the
videos of export folders and archives. Windows plays them with Media
Foundation, the engine of its own video players, together with their sound;
nothing has to be installed for H.264 and VP8 videos.

- **Home**: with a graphics card, the video that is most in view (at least
  40 % of its frame) plays without sound and in a loop, as on Instagram;
  scrolling hands over to the next one, and only one plays at a time.
  **Turn sound on** / **Turn sound off** at the bottom right of every video,
  or a click on the video that plays, switches the sound of all videos.
  A double click still likes the post. The video pauses while something
  covers the feed (the viewer, auto view, a dialog, a menu) and goes on
  where it was when that closes.
- Without a graphics card (Windows draws in software, see below) the feed
  does not play videos by itself: a video shows its poster with
  **Play video**, which opens it in the viewer.
- **Explore** shows the poster of a video with the video badge.
- The viewer plays the video on show at once, in a loop. A bar over its
  bottom has **Play** / **Pause**, the time (`0:03 / 0:10`), the position
  (click to go there, drag to look for a place), **Mute** / **Unmute** and
  **Volume**; it shows while the pointer is over the video, while the
  video is paused and for 2.5 seconds after a key or a move of the pointer.
  A click on the video plays or pauses it, a double click is F11. Keys:
  Space or K, M for the sound, Shift+Left and Shift+Right for 5 seconds
  back or forward; Left and Right go to the previous or next post as for
  pictures. Videos are not zoomed.
- Sound and volume are one setting for the feed, the viewer and auto view,
  for as long as the viewer runs; it starts with the sound off.
- Minimising the window pauses the video that plays, in the feed, the
  viewer or auto view; when the window is back it plays on where it was,
  unless you had paused it. A video of the feed that was paused for more
  than 30 seconds (minimised or covered) starts again from its beginning.
- A video that cannot be played shows its poster and why, for example
  `"IMG_1.MOV" can't be played: Windows has no decoder for its HEVC video. HEVC videos need the HEVC Video Extensions from the Microsoft Store.`,
  with **Show in folder**.
- Codecs: H.264 and VP8 play on every Windows 10 and 11 (the N editions
  need the Media Feature Pack). HEVC (most iPhone videos) needs the "HEVC
  Video Extensions", VP9 the "VP9 Video Extensions" and AV1 the "AV1 Video
  Extension" from the Microsoft Store; a Windows without a WebM reader
  needs the "Web Media Extensions". A phone video recorded sideways is
  shown upright.
- The poster of a loose video is its first frame, made the first time the
  video is shown (a few hundredths of a second) and kept in
  `%LOCALAPPDATA%\instagram-windows-app\posters`, never next to the video.
  A video of an export shows the export's poster, or one made the same way
  when the export has none.
- On a computer without a sound output (a virtual machine, a remote session
  without sound) a video with a sound track starts about two seconds after
  it is shown; Windows waits that long for a sound device. The poster is
  shown meanwhile.
- Without a graphics card, every repaint of the window costs the processor
  up to a tenth of a second, so a playing video is shown in frames of at
  most 640 x 360 pixels and with at most 10 repaints a second, each with
  the newest frame. Windows still decodes every frame. Measured on a
  virtual machine with two logical processors and no graphics card: of a
  720p video's 30 frames a second the window showed about 10 new frames a
  second, in a window of 1920 x 1290 pixels at 150 % and in one of
  1440 x 900 at 100 %, with the video alone or opened from a folder of
  eight posts; a 640 x 360 video the same. The viewer then kept about 1.2
  to 1.4 of the two processors busy in the larger window and about 0.8 to
  0.9 in the smaller one. The video still looks less smooth than on a
  computer with a graphics card. The feed's posters and play badges cost
  nothing while nothing plays.
- When a video closes, app.log says how many frames Windows handed over
  and how many new frames reached the screen, per second of playing; with
  `--frame-log` (see [Command line](#command-line)) it also counts the new
  video frames of every five seconds next to the repaints.

## Edit description

**More options → Edit description** opens a dialog with the author, a field
with the placeholder `Write a description…`, a counter such as
`38 / 10,000`, and **Save** and **Cancel**.

- The limit is 10,000 characters and is enforced while you type and paste.
- Enter makes a new line; Ctrl+Enter saves. Escape, **Cancel** or a click
  beside the dialog closes it without saving.
- **Save** is enabled once the text differs from the current description.
  White space at both ends is removed when it is saved. A saved description
  is confirmed with `Description updated.`
- If it cannot be saved, the dialog stays open with your text and says why
  (`The description could not be saved`).

Descriptions are kept in `descriptions.json` in the data folder (see
[Configuration](#configuration)), and nowhere else:

- The folders of your pictures are never written to, and an opened export or
  archive is never changed. The note under the field says so:
  `Kept on this computer with the picture.` for a loose picture,
  `Kept on this computer. The export folder is not changed.` for a post of an
  export folder, `Kept on this computer. The archive is not changed.` for a
  post of an archive.
- A loose picture is recognised by its full path, without regard to case. If
  you move or rename the file, its description is no longer shown; it stays
  in `descriptions.json` under the old path. If another picture later takes
  that path (a camera counts from `IMG_0001` again) and both its size and its
  modified time differ from the picture that was described, the description
  is not shown for it either, and a warning says so.
- A post of an export is recognised by the path of its `post.json` and the
  id of the post, a post of an archive by the path of the archive and the id
  (so its description stays when the archive cache is cleaned up, and when
  the archive is opened together with others). Changing a description back
  to the text of the export removes the entry.
- The file is written atomically (a temporary file beside it, then a
  rename), and every save first reads the file again, so two windows of the
  viewer do not lose each other's descriptions. **Edit description** also
  reads it again, so the dialog starts from what another window saved.

To carry descriptions to another computer or into the Instagram clone,
download the pictures: the archive holds every description.

## Download post and Download all

**Download post** (in **More options**) writes one post, **Download all** (in
the banner and in the **More** menu) writes every post that is open. A Save
dialog asks where; it suggests `instagram-export.zip` for everything and a
name such as `instagram-export-IMG_2.zip` for one post, in the folder of the
last download or in your Downloads folder.

The archive is an `instagram-export`, version 1, the format the Instagram
clone imports with its "Select archive files" button:

```text
instagram-export.zip
├─ post.json                 the posts, each with its description
├─ posts/
│  ├─ post1.jpg              the picture of a post, as the file is on disk
│  ├─ post2_1.jpg            slides of a carousel
│  ├─ post2_2.jpg
│  ├─ post3.mp4              a video
│  └─ post3_poster.jpg       its poster
└─ avatars/                  profile pictures of an export
   └─ <username>.jpg
```

- Files are copied byte for byte: a HEIC stays a HEIC, nothing is re-encoded,
  and a picture the viewer could not show is still exported.
- `post.json` holds the edited descriptions. For a loose picture it names
  the author (`default_username`), the file's modified time as `taken_at`,
  no link and no like count.
- A loose video is written as it is (an `.m4v` as `.mp4`), with its first
  frame from the poster cache as its poster (`post3_poster.jpg`), so that
  the clone's grids show a picture for it. When no poster can be made the
  video goes without one. A video of an export keeps its own poster, or
  none.
- More than 5,000 posts or 2 GB of pictures and videos are split into parts
  beside the name you chose: `instagram-export-part1.zip`,
  `instagram-export-part2.zip` and so on, each a complete export. When a
  download may be split and part files of an earlier download are in the
  folder, the viewer asks **Replace the earlier download?** first.
- While it runs, the banner button reads `Preparing 3 of 12 · Cancel`; a
  click cancels (`Download cancelled.`). Each archive is written under a
  temporary name and renamed when it is complete, so a cancelled or failed
  download leaves nothing behind.
- At the end a message says what was written, for example
  `Downloaded 6 posts as instagram-export.zip.` A file that could not be
  read is named after `Left out:`.
- One download runs at a time. **Close pictures** asks before it cancels a
  running download (**Close pictures?**).

The clone's own validator accepts these archives. To check one yourself,
extract it and run (this needs Node.js):

```powershell
node core\tests\validator\validate-export.mjs C:\path\to\extracted-folder
```

It ends with exit code 0. For loose pictures it prints one kind of warning,
about the `url` that such a post does not have.

## Browser

What is open in the viewer can also be seen in a web browser, with the
Instagram clone's own pages: the feed, the Explore grid, the post view,
**Edit description**, **Download all**, auto view and importing more exports.
The viewer serves them itself. A production build of the clone's frontend is
part of the exe (`web\dist`, built from the clone's sources as
`web\VENDORED.json` records); nothing has to be installed and nothing is
fetched from anywhere.

### Opening the pages

- **More → Open in browser** opens the page address,
  `http://127.0.0.1:3127/` by default, in your default browser. It is listed
  while the pages run, which they do from the start unless `web_server` is
  `false` or the run was started with `--no-web`.
- **More → Browser view** (see [Browser view](#browser-view)) and
  **More → About** show the two addresses of the pages:
  - the page address, `http://127.0.0.1:3127/` by default: the pages
    themselves, which reach the API under the same address (`/api/...`), so
    a browser on another device needs only this one;
  - the API address, `http://127.0.0.1:8317` by default: the API alone, as
    the clone's backend has it, with a `/docs` page that lists the routes
    of the [API](#api) table.
- The ports are `frontend_port` and `backend_port` of `settings.json` (or
  `--frontend-port` and `--backend-port`), and **Page address port** and
  **API address port** in **Browser view**. When one of them is taken at the
  start, the next free port is used (at most 100 are tried) and
  **Browser view** says so (`was taken when the server started`).
- By default the pages are for this computer only: the viewer listens on
  127.0.0.1, which no other device can reach.
- **Allow other devices on this network** in **Browser view**, then **Save**,
  makes the pages reachable from a phone or another computer (`web_host`
  `0.0.0.0`); their addresses (`http://<address of this computer>:3127/`)
  then appear in **Browser view** and in **About** under **On other devices**.
  Windows Defender Firewall usually asks the first time whether to let the
  viewer through; allow it for private networks only, or the other devices
  cannot connect. Devices on the network can see the open posts and change
  their descriptions. Turn the switch off again, and **Save**, to stop that.
  The switch is saved: every later start listens on the network too, and
  says so at once
  (`The browser pages are open to other devices on this network.`).
- The browser shows what was open when the page was loaded: reload it to see
  what changed in the viewer since. The pages load their posts once, as the
  clone's do, and do not refresh by themselves.

### What the browser can and cannot do

The browser can:

- show the posts that are open in the viewer, with the clone's feed, Explore
  grid, post view and auto view;
- play videos: they are served in ranges, as a browser asks for them to
  seek. A loose video is listed with its first frame as its poster, made
  before the list is sent (for at most 5 seconds; a poster still being made
  then follows when the page asks for it). A video no frame can be read from
  (a damaged file) is listed without a poster, so a page never asks for one
  that cannot come. HEIC pictures are sent as JPEG copies, which every
  browser shows (see `web-cache` under [Configuration](#configuration));
- **Edit description**: the text is saved into the viewer's
  `descriptions.json`, exactly as the viewer's own dialog saves it, and the
  viewer shows the new text at once. The browser's own words for it ("Saved
  in post.json of your library.", "Description saved in your library.") are
  the clone's: the viewer never changes an export or `post.json`;
- **Download post** and **Download all**: the page puts the archive together
  in the browser, in the clone's format, and the browser saves it where it
  saves downloads, on the device that shows the page;
- "Import more": the import is saved in the browser imports folder,
  `%LOCALAPPDATA%\instagram-windows-app\library` (setting
  `library_directory`), which is itself an export folder. The viewer adds it
  to what is open, its posts winning on equal ids, and says so (for example
  `2 posts added from the browser.`); **More → Open browser imports** opens
  it again later. With nothing open the pages show the clone's sample feed,
  as the clone does with an empty library;
- "Clear library": it clears the browser imports folder, and only when
  nothing else is open. Confirming while your own folders or archives are
  open changes nothing and says why (see below).

The pages are the clone's, which knows one place for everything: its
library. Where they name that place, the viewer names the browser imports
folder, `%LOCALAPPDATA%\instagram-windows-app\library`, because that is
where imports go and what Clear library acts on: in the Import dialog
("Imports are added to your library, which this app's server keeps in
..."), after an import ("Saved in your library (2 new posts): ..."), in
the banner ("Showing 34 posts from your library (3 imports) · saved in
...") and in the Clear library question. Your own exports and archives are
never named as the place an import is saved; their posts stay where they
are, also while the banner says "saved in" the browser imports folder. With
loose pictures open those places name the pictures' folder, and an import
or Clear library is refused (see below).

The browser cannot:

- delete or change anything of yours: your pictures, folders, archives and
  exports are only read, and with your own pictures, folders or archives
  open "Clear library" is refused
  (`The posts come from your own pictures, folders or archives`);
- import while loose pictures are open
  (`Instagram Windows App is showing loose pictures`): the viewer never mixes
  loose pictures with posts of an export. Close them, or open an export, and
  import again;
- see anything but the open posts: nothing is listed, only the files of the
  open posts can be fetched, by the names the pages get, and another
  computer is never told a full path of this one;
- change the settings, the ports or **Allow other devices on this network**:
  no route of the API does;
- follow the viewer by itself: reload the page to see what changed.

Safety: by default only 127.0.0.1 listens. A page that reaches the viewer
under another web site's name (DNS rebinding) is refused
(`This host name is not allowed`); names you trust can be added to
`allowed_hosts`. Changes (POST, PUT, PATCH, DELETE) are accepted only from
the viewer's own pages or from programs that are not browsers, such as curl
(`Changes are only accepted from the app's own pages.`). The API address
answers cross-origin requests from the page address only, and another web
site can neither embed the pictures and videos nor learn the address of the
pages from a link they follow. Other devices are never told a full path,
not even in an error, and see `unknown` as the author of loose pictures
unless `default_username` names one (the Windows account name stays on this
computer).

Limits, so that no device takes the pages away from the others:

- Each address holds at most 64 connections and one client at most 16 of
  them. Every program on this computer is one client, whatever loopback
  address (`127.x.x.x`) it uses, and 8 places of each address are kept for
  this computer: devices on the network never take them, however many
  addresses they use.
- A connection takes a place only once its first request has fully
  arrived. Until then it waits without one, for its first byte at most 10
  seconds and for the whole request at most 30; one client has at most 32
  such connections on an address and all clients 128 (32 of them kept for
  this computer), and the one that has waited longest is closed for a new
  one (only one of the same client while that client has its 32). A request
  that had arrived with its whole body when its connection was closed is
  answered all the same; one whose body was still on its way is answered
  `Too many connections`. So connections that send nothing, or only part of
  a request, take no place, however often they are opened again; what they
  can do is close their own client's connections before the requests of
  those arrive (see [Known limitations](#known-limitations)).
- A connection that waits for its next request, or that only reads what its
  client still sends after an answer, gives its place to a new request at
  once when the address is full (a browser simply opens a new connection).
  A connection whose client has sent bytes that the viewer has not read yet
  is never closed for another one. Every connection is closed after 5
  minutes or 100 requests (a browser simply opens a new one).
- A request body and an answer must keep up with 16 KB a second: one that
  falls 30 seconds behind is cut off (an answer with a reset, so the rest of
  it is not sent), and what came faster before counts for at most 30
  seconds ahead. A client that keeps that pace keeps its connection for as
  long as the transfer lasts: a program on this computer that reads 16
  large videos at that pace at once holds this computer's places of that
  address, and devices on the network that do so on many addresses hold the
  places of the network.
- One device can have at most 4 imports open, and starting more cancels only
  its own; programs on this computer share the user's 4, and 2 of the 8 are
  kept for them. An import that has received no file within 2 minutes is
  dropped (one with files after 30 minutes without a new one). The commit
  that saves an import is written to the import's own folder as it arrives,
  so one that arrives slowly holds only its own connection and import; then
  at most 2 imports are merged into the folder at a time, and another one
  waits up to 30 seconds for its turn before it is answered
  `Other imports are being saved right now. Try again in a moment.`

### Browser view

**More → Browser view**, also reached from **About**, shows where the pages
are and changes it:

- **Show the open posts in a browser** turns the server on and off
  (`web_server`).
- **Page address port** and **API address port** take whole numbers from 1
  to 65535, two different ones
  (`The page address and the API address need two different ports.`).
- **Allow other devices on this network** listens on every network adapter
  instead of 127.0.0.1 (`web_host`).
- **Save** opens the typed ports at once, without restarting the viewer: a
  request being answered finishes, an import being uploaded from a browser is
  cancelled, and the old ports are free afterwards. A typed port is never
  replaced by another one: a port another program uses is refused
  (`is used by another program`) and nothing is changed. Then the four values
  go into `settings.json`, which keeps every other key, its value, the order
  of the keys and its layout: only the four values are replaced, and a key
  the file did not have is added after the last one, indented like it
  (`Saved in settings.json.`). If the file cannot be written, nothing on disk
  changes and the new values apply until the viewer is closed
  (`settings.json could not be saved`).
- A value given on the command line stays in effect for that run, and the
  addresses at the top show it. Its control shows the value of
  `settings.json` instead (the default when the file has none), and one note
  at the end of the sheet names what the command line set for this run
  (`The controls show settings.json, and Save keeps them for later runs.`):
  **Save** writes the controls as they stand, so a value you did not touch
  is saved unchanged and the run's value never becomes a saved one
  (`Saved in settings.json for later runs; the command line decides for this run.`).
- In a small window the sheet scrolls, and **Save** and **Cancel** stay in
  view below it.

The browser itself can never change these settings: no route of the API
does.

### API

Both addresses answer the API of the Instagram clone's backend, over what is
open in the viewer; the API address also has `/docs`. Errors are JSON, as
`{"detail": "..."}`.

| Method | Path | Behavior |
| --- | --- | --- |
| `GET` | `/api/health` | {"status": "ok"} |
| `GET` | `/api/library` | The posts that are open in Instagram Windows App, as the Instagram clone's library JSON. |
| `DELETE` | `/api/library` | Clears the browser imports folder when nothing else is open (204), else 403. |
| `GET`, `HEAD` | `/api/library/files/{path}` | A picture or video of an open post (Range requests, ETag); HEIC pictures as JPEG. |
| `POST` | `/api/library/uploads` | Starts saving an import from the browser: {"upload_id": "..."} (201). |
| `PUT` | `/api/library/uploads/{upload_id}/files/{name}` | One file of that import (Content-Length required). |
| `POST` | `/api/library/uploads/{upload_id}/commit` | Saves the import in the browser imports folder and shows it in the app. |
| `DELETE` | `/api/library/uploads/{upload_id}` | Cancels an import that is not committed (204). |
| `PATCH` | `/api/library/posts/{post_id}` | {"caption": "..."}: changes the description, saved in descriptions.json of the app. |
| `DELETE` | `/api/library/posts/{post_id}` | Removes a post of the browser imports folder; 403 for every other post. |

The `/docs` page of the API address lists this table (the clone's backend
has an interactive page there; this one is a plain list). Every route
answers on the page address as well, under the same path.

Not there: the clone's counter routes and its `/openapi.json`. A file path is
`posts/<id>-<hash>_<n>.<ext>` or `avatars/<name>-<hash>.<ext>`, made from
the post and the file, and stays the same while the post is open.

### Upload limits

The browser imports folder has the clone's limits:

| Upload limit | Value |
| --- | --- |
| One picture | 64 MB |
| One video | 1 GB |
| One import (an upload session) | 8 GB |
| The whole folder | 64 GB |
| Posts per import | 5,000 |
| Files per import | 100,000 |
| The commit of an import | 32 MB |
| Imports being saved at once | 8 |
| Imports being saved at once from one device | 4 |
| Of those, kept for programs on this computer | 2 |
| Commits merged into the folder at the same time | 2 |
| A commit waits for its turn at most | 30 seconds |
| An import that received no file is dropped after | 2 minutes |
| An idle import is dropped after | 30 minutes |
| Free space that stays on the drive | 1 GB |

## Auto view

Auto view shows the pictures one after another in a black view that fills
the window, with the author, a counter such as `1 / 6`, the start of the
description, the counts and a progress bar, and moves on by itself. Start it
from **Auto view** in the sidebar or the banner, from **More → Auto view**,
or from **More options → Auto view from here**, which starts at that post.

The setup dialog asks for the **Time per post** (2 to 3600 seconds, 15 by
default, with quick choices from 5 seconds to 1 minute) and has three
options: **Let videos play to the end** (on by default), **Start again after
the last post** (on by default; otherwise an `End of posts` screen offers
**Restart** and **Exit**) and **Shuffle the order**. A carousel shares its
time between its slides, at least 2 seconds each. **Start** remembers the
settings in `state.json`.

A video plays while it is shown. With **Let videos play to the end**, a
video longer than its time stays until it has ended (at the latest after its
length and a quarter of it, at least 5 seconds more), and a video that does
not know its length (a WebM recorded in a browser) stays until it ends, at
most 600 seconds; a shorter video plays in a loop until its time is up.
Without the option every video gets the time of a picture, and a video that
cannot be played always does.

While it runs: Space pauses and resumes (a **Paused** label is shown; a
video pauses with it), Left and Right go to the previous and next post, +
and - change the time per post, M switches the sound of videos, and
Escape exits. The same controls are at the bottom of the view, the bar of a
video shows under the pointer, and **Open post** leaves auto view and opens
the post in the viewer. Time does not pass while the window is minimised,
and a video on show pauses until the window is back (then the show and the
video go on, unless you paused them). F11 makes the view fill the screen.

From the command line, `--autoview=15` starts auto view as soon as the
pictures are open, with 15 seconds per post; `--autoview` alone uses the
remembered settings, and `--loop=0`, `--shuffle=1` and `--wait=0` set the
options. What the command line sets is used for that run and is not
remembered.

```powershell
.\build\dist\Release\instagram-windows-app.exe --autoview=10 --shuffle=1 C:\Users\me\Pictures\Holiday
```

## Configuration

The viewer creates `%APPDATA%\instagram-windows-app\settings.json` on its
first start, with the one setting `data_directory`. The file is yours: the
viewer reads it at every start and writes to it only when you choose
**Save** in **Browser view**, which changes the values of `web_server`,
`frontend_port`, `backend_port` and `web_host` and nothing else, the layout
of the file included. `run.bat` and the Debug and
Release builds use the same settings and data for the current Windows user.

```json
{
  "data_directory": "D:/InstagramViewerData",
  "default_username": "lumen.atlas",
  "frontend_port": 3127,
  "backend_port": 8317,
  "allowed_hosts": ["photos.example"]
}
```

| Setting | Purpose |
| --- | --- |
| `data_directory` | The folder for the files below. Default: `data` beside `settings.json`, that is `%APPDATA%\instagram-windows-app\data`. |
| `default_username` | The name shown as the author of loose pictures, and written as their author into downloads. Default: your Windows account name, reduced to letters, digits, `.` and `_`, or `unknown`; browser pages on other devices then see `unknown`. |
| `web_server` | `false` starts no browser pages. Default: `true`. |
| `frontend_port` | The port of the page address, from 1 to 65535. Default: 3127; when it is taken at the start, the next free port is used. |
| `backend_port` | The port of the API address, not the same as `frontend_port`. Default: 8317. |
| `web_host` | The IPv4 address the pages listen on: `127.0.0.1`, this computer only (the default), or `0.0.0.0`, every network adapter. |
| `allowed_hosts` | Further host names the pages answer to, as a list or one comma-separated text; `"*"` allows any. Default: none (IP addresses, `localhost` and this computer's names always work). |
| `library_directory` | The browser imports folder, expanded like `data_directory`. Default: `%LOCALAPPDATA%\instagram-windows-app\library`. |

For the browser pages the command line wins for its run, then
`settings.json`, then the defaults.

`%VARIABLES%` and a leading `~` in `data_directory` are expanded, and a
relative path resolves from the folder that contains `settings.json`; a
drive letter without a backslash after it (`C:data`) is refused. The folder
is created at the start. Unknown settings are kept as they are, and a byte
order mark at the start of the file is accepted. Invalid JSON, a value that
cannot be used, or a data folder that cannot be created stops the start with
a message box that names the file (and exit code 1); the file is left
untouched.

The default data folder is deliberately not under `%TEMP%`: the descriptions
you write exist nowhere else. Existing files are not moved when you change
`data_directory`; with the viewer closed, move them into the new folder
yourself.

The data folder holds:

| File | Purpose |
| --- | --- |
| `descriptions.json` | The descriptions you wrote, see [Edit description](#edit-description). Include it in your backups. |
| `state.json` | What the viewer remembers: the appearance, the auto view settings, the folder opened last and the folder of the last download. A missing or damaged file means the defaults. |
| `window.ron` | The size and position of the window (corrected at the start when it would not fit the screen). |
| `app.log` | The log. A file over 1 MB is set aside as `app.log.old` at the next start. Set the environment variable `INSTAGRAM_WINDOWS_APP_LOG` to `debug` for more detail. |

**More → About** shows both locations.

What the viewer makes for itself and can make again lives in the local
(not roaming) profile, `%LOCALAPPDATA%\instagram-windows-app`, and is not
configurable:

| Folder | Purpose |
| --- | --- |
| `archives` | The archive cache: the files the viewer unpacked from the archives you opened, one folder per archive. It cleans itself up (see [Archives](#archives)); it is safe to delete while the viewer is closed. |
| `library` | The browser imports folder (unless `library_directory` names another one): what was imported in the browser pages, as an export folder. Only what the browser imported is deleted there, through "Clear library" in the browser. Its `posts` and `avatars` folders belong to the viewer, as the file `.instagram-library` in it says: a file you put there that is named like the viewer's own (`<name>_1.jpg` in `posts`, `<name>.jpg` in `avatars`) is deleted too, as the clone does; other files stay. |
| `web-cache` | JPEG copies of HEIC pictures for the browser pages (browsers other than Safari show no HEIC). At most 1 GB, the oldest go first; safe to delete while the viewer is closed. |
| `posters` | The posters of loose videos: their first frames as JPEG pictures. At most 256 MB, the oldest go first; safe to delete while the viewer is closed. |

### Command line

```text
instagram-windows-app.exe [options] [pictures or folders...]
```

Everything that is not an option is a picture or a folder to open.

| Option | Purpose |
| --- | --- |
| `--autoview[=SECONDS]` | Start auto view as soon as the pictures are open, with SECONDS (2 to 3600) per post or the remembered time |
| `--loop=0\|1` | With --autoview: start again after the last post (1) or stop there (0) |
| `--shuffle=0\|1` | With --autoview: show the pictures in a random order (1) or in their own (0) |
| `--wait=0\|1` | With --autoview: let a video longer than the time per post play to its end (1) or not (0) |
| `--help` | Print the options and the keyboard shortcuts, then exit |
| `--version` | Print the name and the version, then exit |
| `--no-web` | Do not start the browser pages in this run |
| `--host ADDRESS` | Listen on this IPv4 address in this run (0.0.0.0 for every network adapter) instead of 127.0.0.1 |
| `--frontend-port PORT` | The first port tried for the page address in this run (3127 by default); --frontend is the same |
| `--backend-port PORT` | The first port tried for the API address in this run (8317 by default); --backend is the same |

A wrong port or address is refused at the start
(`expects a port number from 1 to 65535`,
`The browser pages and the API need two different ports.`).

`--help` and `--version` neither read the settings nor open a window. The
Release exe is a windowed program: it prints them to the console it was
started from, or to a file or pipe its output is redirected to.

These options exist for development, the tests and the build. A run with
`--profile-dir` never touches your real settings and data:

| Option | Purpose |
| --- | --- |
| `--profile-dir <folder>` | Use `<folder>` in place of `%APPDATA%` for `settings.json` and the data folder, and its subfolders `Local` and `Temp` in place of `%LOCALAPPDATA%` (the archive cache) and `%TEMP%` |
| `--screenshot <png>` | Render with a hidden window, save the picture of the window as a PNG and exit |
| `--size <W>x<H>` | The inner size of the window in points (1440x900 with --screenshot) |
| `--scale <factor>` | Pixels per point, from 0.25 to 8 (the display's scale otherwise) |
| `--theme light\|dark` | Start in this appearance, as if Switch appearance had chosen it |
| `--scenario <step>[,<step>...]` | Put the interface into a state after opening; an unknown step prints the list of steps |
| `--watchdog <seconds>` | End the run after this long (a --screenshot run gives up after 120 by default) |
| `--frame-log` | Write frame counts and timings to app.log |
| `--animate` | Keep animations running in a --screenshot run |

Exit codes: 0 after a normal run, 1 when the settings are invalid or the
window cannot be opened, 2 for an unknown option or scenario step, 3 when the
watchdog ends a run, 4 when the screenshot could not be saved.

### Default viewer

The viewer does not register itself with Windows and has no installer. To
open pictures with it from Explorer:

1. Build it (see [Build](#build)) and keep `build\dist\Release` where it is,
   or copy that folder to a place of your choice.
2. Right-click a picture, choose "Open with" and "Choose another app".
3. Browse to `instagram-windows-app.exe` ("Choose an app on your PC" on
   Windows 11, "Look for another app on this PC" on Windows 10).
4. Choose "Always" to make it the viewer for that file type; repeat for the
   other types you use (`.jpg`, `.png`, `.heic`, ...).

Explorer starts the exe with the picture as its argument, so the viewer
opens on that picture with the rest of its folder behind it. Windows
remembers the path of the exe: choose it again after you move the folder.

## Differences from the Instagram clone

Kept from the clone: the look of the sidebar, feed, Explore grid, post view,
action sheet, dialogs and toasts in both appearances; **Edit description**
with its limit, keys and messages; **Download post** and **Download all**
with the same archive format and messages; reading exports from folders and
from `.zip`, `.tar` and `.tar.gz` archives, with the clone's readers, limits
and messages, and merging several of them with the clone's rules (read last
wins, newest first, warnings named after their source, at most 50 sources,
4 GB per opening); auto view; the `settings.json` contract.

Not in the viewer:

- Stories, Reels, search, notifications, messages, profile pages, creating a
  post, suggestions, the comment field, and the clone's sample content. With
  nothing open the viewer shows `Open pictures to get started`.
- The clone's backend as a separate program, its counter API, and the proxy
  between its two servers: the viewer answers both addresses itself.

- The phone layout: below the width where the clone moves its navigation to
  the bottom, the viewer keeps the rail of icons.

Added to the clone's design:

- Opening loose pictures and videos at all: the clone can only show an
  export. A loose video gets its first frame as its poster, on screen and
  in downloads.
- Previous and next picture, zoom, pan and fullscreen in the post view, and
  the file name in the window title.
- **Show in folder**, **Open folder**, **Close pictures**, **About**, and the
  Save dialog of a download.
- **Browser view**, and the clone's pages over loose pictures and over
  archives the viewer opened.

Different on purpose:

- An edited description is saved in `descriptions.json`, not into the export
  or a library.
- The clone loads the files of an archive into the memory of the browser
  tab; the viewer unpacks the files `post.json` names into its archive cache
  instead, and uses them again the next time. The clone's paths in warnings
  include the folder the export lies in inside the archive; the viewer's
  start at that folder, as for an export folder.
- The **More** menu has seven entries (more while browser imports or the web
  app's library can be opened).
- The banner of several exports opened at once, such as "2 archives and 1
  folder", counts the archives and folders whose posts it shows; one that
  could not be imported is only named in the warnings. The clone counts
  every file that was selected.
- In the browser pages, descriptions go to `descriptions.json`, not into the
  library's `post.json`; a description written in the viewer stays on top
  of a post the browser imports again (the clone would show the newly
  imported text). The pages' import dialog says imports are kept in the
  folder of what is open; they go to the browser imports folder. For loose
  pictures, the pages show a "View profile" link guessed from the author's
  name, as the clone does for any post without a link.
- The browser pages: a HEIC picture is sent to the browser as a JPEG copy
  made when it is first asked for (the clone converts HEIC once, when it is
  imported), so a download made in the browser holds the JPEG; the `/docs`
  page of the API address is a plain list of the routes, not the clone's
  interactive page; the two addresses are served by the viewer itself, on
  the ports of **Browser view** (the clone's defaults, 3127 and 8317), and
  the next free port is taken when one of them is in use at the start.
- The browser imports folder takes uploads only under the picture and video
  names the clone's pages give them (the clone's backend takes any short
  extension), and too many imports at once cancel only the oldest idle one
  of the same device (the clone cancels anybody's), because devices on the
  network may share the folder.
- A comment count is shown for posts of an export only; a loose picture has
  none.
- Videos: one sound setting for the feed, the viewer and auto view, kept for
  the run (the clone starts every video of its post view and auto view
  muted, as browsers require); the viewer's own bar of controls instead of
  the browser's; **Show in folder** instead of the clone's "Download video"
  for a video that cannot be played (the file is on this computer already);
  without a graphics card the feed does not play videos by itself.

## Known limitations

- Only the first frame of an animated GIF or WebP is shown.
- Without a graphics card a playing video shows about 10 new frames a
  second, not its 30 (see [Videos](#videos)): the window is repainted at
  most 10 times a second then, and each repaint costs about a tenth of a
  second of processor time in a window of 1920 x 1290 pixels. Repainting
  for every frame was measured at about 12 new frames a second with both
  processors of a two-processor machine nearly fully busy, so it is not
  done.
- Video playback was measured on a machine without a graphics card and
  without a sound output only. With a graphics card the frames come from
  Direct3D 11 (hardware decoding, where there is any); that path was tried
  only on the software adapter of that machine. Set the environment
  variable `INSTAGRAM_WINDOWS_APP_VIDEO_PATH` to `wic` to play through the
  path the software renderer uses instead.
- A video damaged in the middle can stop on a frame without any message:
  Windows reports nothing then.
- Text that runs from right to left (Arabic, Hebrew) is shaped correctly but
  not reordered: a stretch of such text reads right to left, yet stretches
  stay in typing order and a wrapped paragraph shows its lines in the wrong
  order. This is a limit of the text layout of egui 0.37.
- Flag emoji are drawn as their two letters, as everywhere on Windows.
- A description belongs to the path of its picture: moving or renaming the
  file separates the two.
- A picture is decoded to at most 8,192 pixels per side. A larger one (a
  panorama, a scan) is scaled down to that, also at a zoom of 100 %, which
  then shows it stretched rather than pixel for pixel.
- Without a graphics adapter that supports DirectX 12, Windows draws the
  window in software, on the processor. The viewer then filters pictures in
  its own shader and leaves out what a layer above covers, which makes it
  several times faster, but every frame still costs processor time.
  Development and all automated checks ran on such a machine, a virtual
  machine with two logical processors used over a remote session. There, a
  frame of the viewer in a window of 1920 x 1290 pixels takes about 80 ms of
  processor time, zooming and panning run at about 16 to 23 frames per
  second (about 10 to 12 in a maximised window of 2560 x 1494 pixels), and
  a 12-megapixel camera photo opened on its own appears after about 0.8
  seconds. Larger pictures take longer, because they are decoded first.
- HEIC pictures come out of the Windows decoder with slightly shifted
  colours (a few levels too light, white with a tint), compared with the
  same picture in a browser.
- If the viewer is ended abruptly (a crash, Task Manager, a power cut) while
  it writes `descriptions.json` or an archive, the temporary file it was
  writing stays beside it (a name starting with a dot and ending in `.tmp`).
  It can be deleted.
- The mouse wheel over the sidebar and the Space key do not scroll the
  feed, and Escape on the **More** menu closes it without handing the
  keyboard focus back to **More**.
- At 125 % and 150 % scaling the one-pixel lines (borders, dividers) are
  drawn across two pixels and look lighter than in a browser.
- In small windows the About sheet, **Browser view** and the auto view
  setup do not fit and scroll (About and **Browser view** keep their rows
  in view below the part that scrolls).
- In auto view the slides of a carousel change without the slide animation.
- There is no installer and no update check, and the exe is not signed.
- The browser pages do not follow the viewer by themselves: what changed in
  the viewer shows after a reload of the page. An **Edit description** dialog
  open in the viewer keeps its text when a browser saves the same post (the
  last save wins).
- A second copy of the viewer takes the next free ports, and imports from its
  pages are refused while the first copy uses the browser imports folder
  (`in use by another running copy of this app`).
- Every program on this computer counts as the same client as your browser
  there, and a request that is being worked on keeps its place. So a
  program on this computer can hold the 16 places this computer has on an
  address with requests whose body or answer moves at the slowest pace the
  pages accept (16 KB a second), or moves nothing until it is cut off 30
  seconds later, sent again and again. The browser then gets
  `Too many connections` there until they end (see the limits under
  [What the browser can and cannot do](#what-the-browser-can-and-cannot-do)).
  Connections that only wait never keep it out, and connections that send
  nothing or only part of a request take no place. But a program on this
  computer that opens such connections very fast, several hundred a second,
  closes the browser's new connections there before their requests arrive,
  and some of the browser's requests then fail. The limits keep devices on
  the network from doing this to this computer; no server can keep a
  program on the same computer from disturbing it.

## Build

```powershell
.\build.bat
# Only one profile, or without running the tests:
.\build.bat --profile-only Release
.\build.bat --skip-tests
```

`build.bat` needs the same toolchain as [Quick start](#quick-start) and
nothing more. It works from a plain command prompt; a Visual Studio developer
prompt is not needed, because the linker and the Windows SDK are found
through the Visual Studio installation. It then:

1. checks the toolchain, as `setup.bat` does;
2. collects the licence texts of every crate and font in the exe;
3. runs `cargo build --locked` for the debug and the release profile;
4. runs `cargo test --locked --workspace` (unless `--skip-tests`);
5. puts each package together in a staging folder, checks it, and
   smoke-tests the exe: `--version` must print the version, and one run with
   a hidden window and a profile of its own must render the test export
   `core/tests/fixtures/export-basic` and save a picture of the window;
6. publishes the packages.

Everything the build writes stays in `build\`; the result is in
`build\dist`:

```text
build/
  target/                       cargo's target folder, kept between builds
  smoke-test/                   Debug.png and Release.png of the last smoke test
  dist/
    Debug/                      Console build: a console window shows the log
      instagram-windows-app.exe The whole program
      README.txt                How to start it, settings, data, options
      licenses/                 Third-party licence texts, with an index
    Release/                    Optimised build without a console window
      (the same files)
```

Each folder is complete on its own: copy it to any 64-bit Windows 10 or 11
machine and start the exe. Nothing has to be installed there. The C runtime
is linked into the exe (`.cargo/config.toml`), so it needs only DLLs that
are part of Windows, not the Visual C++ Redistributable. Before you pass a
build on, read [Third-party software](#third-party-software).

Rebuilding replaces only `instagram-windows-app.exe`, `README.txt` and
`licenses` in `build\dist\Debug` and `build\dist\Release`, and keeps
everything else you put there. A build that fails, at any step, leaves
`build\dist` as it was and keeps its staging folder
(`build\staging-<id>`) for a look until the next build. The build refuses to
publish while the exe in `build\dist` is running.

The browser pages in `web\dist` are part of the project and are embedded as
they are; the build needs no Node.js for them. Their licence folder,
`web\dist\licenses` (the LGPL and GPL texts of heic-to among them), is copied
into `licenses\web` of each package. The vendored crate in `vendor/` is
compiled by cargo like any other crate. To rebuild them from the
Instagram clone's current frontend sources (this needs Node.js and npm):

```powershell
.\update-web.bat
# Only check whether web\dist was built from the sources as they are now:
.\update-web.bat -Check
```

`update-web.bat` copies `..\frontend` (or `-Source <folder>`)
into a work folder, runs `npm ci` and the clone's `vite build` there, checks
the output, replaces `web\dist` and writes `web\VENDORED.json` (the hash of
the sources, the versions of Node.js, npm and Vite, and every file with its
size and SHA-256). Nothing is ever written into the frontend's folder. Then run
`build.bat` (or `cargo build`) to embed the new pages.

On the two-core machine this project was developed on, a build of both
profiles with the tests took 20 to 30 minutes from an empty `build\target`
(about 22 minutes in one run, 28 and a half in another while other programs
kept the machine busy), and one to one and a half minutes when nothing had
changed: the tests run again every time.

The `Build and release Windows binaries` GitHub workflow runs the same build
and publishes `build\dist\Debug` and `build\dist\Release` as zip archives
with a `SHA256SUMS.txt`.

## Tests

```powershell
cargo test --workspace
# One crate, or one suite:
cargo test -p instagram-core
cargo test -p instagram-windows-app --test docs
```

`instagram-core` holds the format, the files and the logic, without any user
interface. Its unit tests sit beside the code; `core/tests/` adds the reader
and the export with the synthetic exports in `core/tests/fixtures/`, picture
decoding (a real HEIC among them), the `settings.json` contract, the text
helpers with the clone's own test vectors, and an end-to-end run: open, edit
a description, download, read the download back. `core/tests/archives.rs`
opens the clone's fixture archives (copied into
`core/tests/fixtures/archives/`) and archives built by the test: stored and
deflated zips, tar and tar.gz, parts and duplicates, damaged and hostile
archives (names such as `../evil.jpg`, `C:/x`, `con.jpg`, links in a tar,
size bombs, ten thousand entries), the archive cache and its clean-up, the
merge, and the round trip from **Download all** back into the viewer.
Where an archive is written, the tests extract it and run the clone's
validator (`core/tests/validator/validate-export.mjs`) on it. That step
needs Node.js and is skipped with a note when `node` is not installed;
nothing else does. `cargo test` hides the note unless it is run with
`-- --nocapture`, so `build.bat` prints one line after the tests when
`node` is not on PATH.

`core/tests/web.rs` tests the browser pages over real sockets on ports of
their own: every route and its JSON, the route table of `/docs` and of this
README against the router (on both addresses, every documented method is
answered and every other one refused), byte ranges and revalidation, attempts
to reach files outside the open posts, foreign host names, cross-site writes
and CORS, the limits of the HTTP layer (silent, trickling and slowly reading
clients, connections per address and per listener, a commit checked before
its body is read), uploads into the browser imports folder (the folder must
read back as an export; names Windows would run are refused), clearing, the
stop, and moving the server to other ports.

Videos are tested with videos the tests make themselves with Media
Foundation's sink writer (no video file of the project is needed, except one
VP9 WebM recorded in a browser): `core/tests/poster.rs` reads their codec and
first frame, turned and padded videos among them, and `app/tests/video_mf.rs`
plays them without a window: every frame in order to the end, seeks while
paused that show the right frame, looping, a file that is no video, and a
close within 100 ms.

`instagram-windows-app` is tested without a window: `app/tests/ui_*.rs`
build the whole interface with egui's test harness and drive it through the
accessible names of its controls, the way a screen reader sees it.
`app/tests/ui_viewer.rs` also checks what the viewer leaves undrawn for a
computer without a graphics card: nothing below the card of the viewer is
drawn inside it, and auto view is drawn on a window cleared to black.
`app/tests/ui_video.rs` drives the interface with a stand-in for the video players: which
feed video plays, the sound button, the play badge without a graphics card,
the controls and keys of the viewer, and auto view waiting for a video.
`app/tests/web.rs` runs the whole app with its browser pages: an edit from a
browser reaches the app and `descriptions.json`, an import is added, About
and **Browser view** show the addresses, **Save** moves the server, and in
a run with ports on the command line it saves only what was typed.

`web/e2e/web-e2e.mjs` drives the real exe with a real browser (Microsoft Edge,
headless, through Playwright); it is not part of `cargo test` and needs
Node.js and the repository's `e2e\node_modules` at the repository root. See
`web/e2e/README.md`:

```powershell
node web\e2e\web-e2e.mjs --exe build\target\release\instagram-windows-app.exe
```

No test touches your settings or data. Every test uses a folder of its own
under the temporary folder in place of `%APPDATA%` and of the archive cache,
and the tests that open the fixtures check afterwards that not a byte of
them changed. The tests of the interface, the browser pages, the video
players and this README write in `%TEMP%\instagram-windows-app-tests`, one
folder `<test>-<process id>` per test, and each folder is removed when its
test ends, also when the test fails; the other tests remove the folders
they make in `%TEMP%` as well. Set the environment variable
`INSTAGRAM_WINDOWS_APP_KEEP_TEST_FILES` to `1` to keep the folders for a
look: each test then prints where its folder is, and nothing in
`%TEMP%\instagram-windows-app-tests` is removed. A test run that was
stopped before its tests ended, or that kept its folders, leaves them
there; the next test run without the variable removes the folders of test
processes that no longer run.

The tests also keep the documentation honest. `app/tests/docs.rs` compares
this README with the code: every label in bold and every quoted message
must exist in the interface, the keyboard table and the option tables must
equal the tables the app prints with `--help` (and every option must be
accepted by the real command-line parser), the limits must equal the
constants, and the settings and files named under
[Configuration](#configuration) must be the ones the code reads and writes.
The API table must be the server's route table, every link inside this
README must lead to a heading, and `vendor/egui-wgpu` must be the egui-wgpu
that `Cargo.lock` builds, with its shader change in place.
It also runs the build script for the generated files of a package and
checks them: `README.txt` must be plain ASCII with Windows line ends in 78
columns, and the licence index must cover every crate that `cargo tree`
lists for the exe.

For a look at the real window without a display, the development options
take a screenshot of any state, for example the viewer on the first post in
the dark appearance:

```powershell
.\build\target\debug\instagram-windows-app.exe --profile-dir build\shots\profile --screenshot build\shots\viewer.png --size 1280x800 --scale 1.0 --theme dark --scenario viewer:0 core\tests\fixtures\export-basic
```

## Third-party software

The project's own code is licensed under the Apache License 2.0 (see
`LICENSE`). The exe is built from about 190 Rust crates, all under permissive
licences. The ones that do most of the work:

| Package | Used for | Licence |
| --- | --- | --- |
| [egui](https://github.com/emilk/egui) 0.37 (`eframe`, `egui`, `epaint`, `egui-wgpu`, `egui-winit`, `egui_extras`) | The user interface and its window | MIT OR Apache-2.0 |
| [wgpu](https://github.com/gfx-rs/wgpu) 30 | Drawing with DirectX 12 | MIT OR Apache-2.0 |
| [winit](https://github.com/rust-windowing/winit) 0.30 | The window and its input | Apache-2.0 |
| [image](https://github.com/image-rs/image) 0.25 (with `zune-jpeg`, `png`, `image-webp`, `gif`) | Decoding JPEG, PNG, WebP, GIF and BMP | MIT OR Apache-2.0 |
| [resvg](https://github.com/linebender/resvg) 0.45 | Drawing the icons | Apache-2.0 OR MIT |
| [zip](https://github.com/zip-rs/zip2) 9 | Writing the download archives | MIT |
| [flate2](https://github.com/rust-lang/flate2-rs) 1 | Reading deflated zip members and `.tar.gz` archives | MIT OR Apache-2.0 |
| [zlib-rs](https://github.com/trifectatechfoundation/zlib-rs) 0.6 | Compressing `post.json` in an archive, and decompressing opened archives | Zlib |
| [rfd](https://github.com/PolyMeilex/rfd) 0.18 | The Open and Save dialogs | MIT |
| [httparse](https://github.com/seanmonstar/httparse) 1.10 | Reading the requests of the browser pages (the server itself is the standard library's networking) | MIT OR Apache-2.0 |
| [getrandom](https://github.com/rust-random/getrandom) 0.3 | The ids of uploads from the browser | MIT OR Apache-2.0 |
| [serde_json](https://github.com/serde-rs/json) 1 | Reading and writing JSON | MIT OR Apache-2.0 |
| [windows](https://github.com/microsoft/windows-rs) 0.62 | Windows Imaging Component, for HEIC and AVIF | MIT OR Apache-2.0 |
| [accesskit](https://github.com/AccessKit/accesskit) 0.25 | Accessible names for screen readers | MIT OR Apache-2.0 |

One of them is built from a copy in the project: `vendor/egui-wgpu` is
egui-wgpu 0.37.0 as published, with one change to its shader that makes
drawing in software about four times faster; `vendor/README.md` describes the
change and how to redo it when egui is upgraded, and `vendor/egui-wgpu.patch`
is the change alone. The licence index of a build lists it like the other
crates, with a note that it is built from that copy.

The compiled program no longer shows the licence notices of these crates, so
the licence texts travel next to it. The build collects them for every crate
that `cargo tree` lists for the Windows exe, from the crate sources in the
cargo registry, into `licenses\` of both `build\dist\Debug` and
`build\dist\Release`: the licence files each crate is published with, the
project's own `LICENSE`, and a `README.txt` that says which file belongs to
which package. A few crates are published without a licence file; for them
the standard text of their licence from `licenses/standard/` stands in, and
the index names their authors. The build stops when a crate has neither.
`cargo test` fails when the index misses a crate or lists a file that is not
there.

Three fonts are inside the exe; they come with egui and are used when a
Windows font is missing: Ubuntu Light (Ubuntu Font Licence 1.0), Hack (MIT
and the Bitstream Vera licence) and five icon glyphs of emoji-icon-font
(MIT). Their licence texts are in `licenses\fonts`. Segoe UI, Segoe UI Emoji
and the other fonts of Windows are read from the Windows fonts folder at run
time and are not part of a build.

The exe also contains the Rust standard library (MIT OR Apache-2.0) and the
static C runtime of the Visual Studio C++ build tools, which the licence
terms of Visual Studio allow to be distributed as part of a program.

The browser pages inside the exe are a build of the Instagram clone's
frontend and contain, minified, these packages (the clone's own list is
`web\dist\licenses\README.txt`, served at `/licenses/README.txt`; a build
copies the folder to `licenses\web`):

| Package of the pages | Used for | Licence |
| --- | --- | --- |
| heic-to | Converting HEIC pictures in the browser; it contains libheif and libde265, compiled to JavaScript | LGPL-3.0 |
| fflate | Reading `.zip` and `.tar.gz` imports in the browser | MIT |
| react, react-dom, scheduler | The pages | MIT |

heic-to is not mixed into the pages' own code: it is the separate file
`assets/heic-to-<hash>.js`, loaded only when an import in the browser holds a
HEIC picture. To use another or a changed version of it, change the
`heic-to` dependency of the clone's frontend, run `update-web.bat` and build
again; the texts of the LGPL and the GPL it incorporates are in
`licenses\web`.

## Project layout

```text
core/                     Package instagram-core: format, files and logic, no user interface
core/src/messages.rs      Every sentence the core produces: warnings, errors, toasts
core/src/archive/         The zip and tar readers (ports of the clone's) and the archive cache
core/src/sources.rs       The merge of several export folders and archives
core/src/web/             The browser pages: HTTP server, routes, guard, snapshot of what is open
core/src/library.rs       The browser imports folder (a port of the clone's library.py)
core/src/video_rules.rs   Which video plays, frame sizes, times, the sentence of a failed video
core/src/poster.rs        The posters of loose videos (Media Foundation's source reader) and their cache
core/src/test_folder.rs   The tests' folders in %TEMP%\instagram-windows-app-tests, removed when a test ends
core/tests/               Integration tests of the core
core/tests/fixtures/      Synthetic exports and loose pictures used by the tests and the build
core/tests/fixtures/archives/  The clone's fixture archives (export-basic.zip, parts, damaged ones)
core/tests/fixtures/video/     A VP9 WebM recorded in a browser (the other test videos are made by the tests)
core/tests/validator/     The export validator of the Instagram clone, for the tests
app/                      Package instagram-windows-app: the eframe/egui interface and the exe
app/src/strings.rs        Every label of the interface
app/src/shortcuts.rs      The keyboard shortcuts and command-line options that --help prints
app/src/harness.rs        The development options: --screenshot, --scenario and the others
app/src/web.rs            Starting, moving and stopping the browser pages; the Browser view sheet is app/src/browser.rs
app/src/video/            Video playback: the Media Engine on a thread per player, who plays, the controls
app/src/rendering.rs      Quick drawing on a computer without a graphics card
app/tests/                Headless tests of the interface, and docs.rs for this README
vendor/egui-wgpu/         egui-wgpu 0.37.0 with a faster shader for drawing in software
vendor/README.md          What was changed in vendor/egui-wgpu, and how to redo it on an upgrade of egui
vendor/egui-wgpu.patch    That change alone, as a unified diff against the published crate
app/assets/               The icon of the window and of the exe
web/dist/                 The browser pages: a build of the Instagram clone's frontend, embedded into the exe
web/VENDORED.json         What web/dist was built from: the hash of the sources, the tools, every file
web/e2e/                  The end-to-end test of the browser pages with Playwright (optional)
update-web.bat / update-web.ps1  Rebuilds web/dist from the clone's frontend (needs Node.js)
licenses/standard/        Standard licence texts for crates that are published without one
.cargo/config.toml        The target folder (build/target) and the static C runtime
Cargo.toml / Cargo.lock   The workspace, its dependency versions and build profiles
setup.bat / setup.ps1     Toolchain check and dependency download
run.bat                   Builds the debug profile and starts it
build.bat / build.ps1     Portable Debug and Release builder (output: build/dist)
../.github/workflows/     The GitHub workflow that builds and releases the zip archives
```
