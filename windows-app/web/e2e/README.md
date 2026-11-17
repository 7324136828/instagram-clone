# End-to-end test of the browser pages

`web-e2e.mjs` runs the real exe and a real browser: Microsoft Edge, headless,
driven by Playwright. It is not part of `cargo test` and not part of the
build; run it by hand after a build:

```powershell
cargo build --release -p instagram-windows-app
node web\e2e\web-e2e.mjs --exe build\target\release\instagram-windows-app.exe --work C:\Temp\ig-e2e --shots C:\Temp\ig-e2e\shots
```

It needs Node.js and Playwright. Nothing is installed for this project:
Playwright is imported read-only from the repository's
`e2e\node_modules` (the repository root is the default, or
`--clone <folder>`). Without it the script prints `SKIPPED` and ends with exit
code 0.

What it checks, with the exe's own profile folder under the work folder
(`--profile-dir`, so nothing of your profile is read or written):

1. Exports (`core\tests\fixtures\export-basic` and the clone's
   `export-videos.zip`): the banner (its "saved in" names the browser
   imports folder), the order and captions of the posts, every picture
   loaded (the carousel, the HEIC picture), every picture, poster and video
   fetched from the page address, a video playing with Range requests; an edit in the browser reaching `descriptions.json`
   and the app; an edit in the app (the scenario step `wait-file:` waits for
   a file the script creates) showing after a reload; "Clear library" refused;
   an import of the clone's `export-basic.zip` landing in the browser imports
   folder and in the app; "Download all" passing the clone's validator; raw
   requests (a foreign Host, a cross-site write, a path outside the open
   posts, the API address and `/docs`); the app closing within 5 seconds
   after WM_CLOSE with both ports free.
2. Loose pictures (`core\tests\fixtures\loose`) and a loose MP4 (the clone's
   `clip2.mp4`, taken from `export-videos.zip` with Windows' `tar`): every
   picture loaded; the video listed with a poster made from its first frame,
   the poster served as a JPEG and shown in a page of the page address but
   refused from the API address (`Cross-Origin-Resource-Policy: same-site`:
   Edge takes another port for another site), the video playing (its `currentTime`
   advances) with Range requests, nothing written beside the files; and an
   import from the browser refused with the app's sentence.
3. Screenshots of the app's About and Browser view sheets (`--screenshot`).

Screenshots of the browser (light and dark: the feed, the post view, the edit
dialog) and of the app go to `--shots`; every check is printed as `PASS` or
`FAIL` and written to `e2e-results.txt` in the work folder. The exit code is 0
when every check passed.
