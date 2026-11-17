# Video fixtures

`vp9_640x360.webm` (62,553 bytes): a 3-second VP9 video of 640 x 360 pixels without sound, made
by this project: recorded from an animated canvas with `MediaRecorder` (`video/webm;codecs=vp9`,
1 Mbit/s) in headless Microsoft Edge, driven by Playwright. Like every WebM a browser records, it
has no length in its header. `core/tests/poster.rs` reads its codec and first frame; the test is
skipped with a note when Windows has no VP9 decoder (the VP9 Video Extensions of the Microsoft
Store).

The other test videos are made by the tests themselves with Media Foundation's sink writer
(`instagram_core::poster::fixture`), or come from the clone's `export-videos.zip` in
`../archives`.
