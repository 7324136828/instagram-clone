//! The vendored browser pages: a production build of the Instagram clone's frontend (`web/dist`,
//! see `web/VENDORED.json`), embedded into the executable by `build.rs`. The table is handed to
//! the core's HTTP server, which serves it on the page address.

include!(concat!(env!("OUT_DIR"), "/web_files.rs"));

#[cfg(test)]
mod tests {
    use super::WEB_FILES;

    #[test]
    fn the_pages_are_embedded_with_their_entry_and_licences() {
        let index = WEB_FILES.iter().find(|file| file.path == "index.html").expect("index.html is embedded");
        assert!(index.content_type.starts_with("text/html") && !index.immutable);
        let text = String::from_utf8_lossy(index.bytes);
        // Every script and style sheet index.html names is in the table, cached for good.
        for reference in text.split("\"/assets/").skip(1).filter_map(|rest| rest.split('"').next()) {
            let asset = WEB_FILES.iter().find(|file| file.path == format!("assets/{reference}")).unwrap_or_else(|| panic!("assets/{reference} is embedded"));
            assert!(asset.immutable);
        }
        assert!(WEB_FILES.iter().any(|file| file.path == "licenses/README.txt"));
        assert!(WEB_FILES.iter().all(|file| file.etag.starts_with('"') && file.etag.len() == 18));
    }
}
