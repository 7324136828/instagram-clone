//! The browser pages with the whole app, headless (web brief 4.15): the server started at the
//! first frame on ports of its own (`AppConfig::web` with port 0), the browser's changes carried
//! out by the frames of the test while a client thread waits, the UI thread that does not answer,
//! the stop at the exit, the More menu and About entries, and the Browser view sheet that moves
//! the server to typed ports and writes settings.json.

mod common;

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use common::{app_with, config, count, count_containing, fixture, harness, harness_sized, profile, settle, settle_idle, toasts};
use egui::accesskit::Role;
use egui::{Key, Modifiers};
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use egui_kittest::Harness;
use instagram_core::catalog::{self, OpenOptions, OpenRequest};
use instagram_core::settings::WebSettings;
use instagram_core::store::DescriptionStore;
use instagram_windows_app::app::App;
use instagram_windows_app::state::Action;
use instagram_windows_app::web::{WebConfig, WebOverrides, WebStatus};
use instagram_windows_app::web_assets;
use serde_json::Value;

/// Width of the sheets.
const SHEET_WIDTH: f32 = 400.0;

/// The browser pages of a test: any free ports (port 0), the reply timeout given.
fn web_config(reply_timeout: Duration) -> WebConfig {
    let settings = WebSettings { frontend_port: Some(0), backend_port: Some(0), ..WebSettings::default() };
    let mut config = WebConfig::new(settings, WebOverrides::default(), web_assets::WEB_FILES);
    config.reply_timeout = reply_timeout;
    config
}

/// An app with the browser pages and the collection at `paths` on screen.
fn app_with_web(profile: &Path, paths: Vec<PathBuf>, reply_timeout: Duration) -> App {
    let mut configuration = config(profile);
    configuration.web = Some(web_config(reply_timeout));
    let mut app = App::new(configuration);
    if !paths.is_empty() {
        let request = OpenRequest { paths, options: OpenOptions { default_username: app.services.default_username.clone() } };
        let opened = catalog::open_paths(&request, &app.services.store, &app.services.context).expect("the fixture opens");
        app.show_opened(opened);
    }
    app
}

/// Runs frames until the server runs; its page and API addresses.
fn running(harness: &mut Harness<'_, App>) -> (SocketAddr, SocketAddr) {
    settle(harness, "the browser pages to start", |app| app.services.web.running().is_some());
    let running = harness.state().services.web.running().cloned().expect("running");
    (running.frontend, running.backend)
}

/// One request; the status and the body.
fn request(address: SocketAddr, head: &str, body: &[u8]) -> (u16, Vec<u8>) {
    let mut stream = TcpStream::connect(address).expect("connect");
    stream.set_read_timeout(Some(Duration::from_secs(30))).expect("timeout");
    stream.write_all(format!("{head}\r\nConnection: close\r\n\r\n").as_bytes()).expect("send");
    stream.write_all(body).expect("send the body");
    let mut answer = Vec::new();
    stream.read_to_end(&mut answer).expect("the answer");
    let split = answer.windows(4).position(|window| window == b"\r\n\r\n").expect("a head") + 4;
    let status = String::from_utf8_lossy(&answer[..split]).split(' ').nth(1).and_then(|code| code.parse().ok()).expect("a status");
    (status, answer[split..].to_vec())
}

/// A PATCH of a caption from a client thread, while the test runs frames; its status and body.
fn patch_while_running(harness: &mut Harness<'_, App>, address: SocketAddr, id: &str, caption: &str) -> (u16, Vec<u8>) {
    let body = serde_json::json!({ "caption": caption }).to_string();
    let head = format!("PATCH /api/library/posts/{id} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nSec-Fetch-Site: same-origin\r\nContent-Type: application/json\r\nContent-Length: {}", address.port(), body.len());
    let client = std::thread::spawn(move || request(address, &head, body.as_bytes()));
    let deadline = Instant::now() + Duration::from_secs(30);
    while !client.is_finished() {
        harness.step();
        assert!(Instant::now() < deadline, "the PATCH was not answered");
        std::thread::sleep(Duration::from_millis(5));
    }
    client.join().expect("the client")
}

#[test]
fn a_browser_edit_changes_the_app_and_descriptions_json() {
    let profile = profile("web-edit");
    let paths = vec![fixture("export-basic"), fixture("archives").join("export-basic.zip")];
    let mut harness = harness(app_with_web(&profile, paths, Duration::from_secs(10)));
    let (frontend, _) = running(&mut harness);
    let before: Value = serde_json::from_slice(&request(frontend, "GET /api/library HTTP/1.1\r\nHost: 127.0.0.1", b"").1).expect("JSON");
    let revision = before["revision"].as_u64().expect("a revision");
    assert_eq!(before["posts"].as_array().map(Vec::len), Some(6), "both sources: the archive's posts win on equal ids");

    let (status, body) = patch_while_running(&mut harness, frontend, "FXTbasic002", "Changed in the browser");
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
    let after: Value = serde_json::from_slice(&body).expect("JSON");
    assert!(after["revision"].as_u64().expect("a revision") > revision, "the revision grew");
    // The app shows it, and descriptions.json has it (with the archive's key: read last wins).
    let post = harness.state().state.posts().iter().find(|post| post.id == "FXTbasic002").cloned().expect("the post");
    assert_eq!(post.caption, "Changed in the browser");
    assert!(post.key.as_str().starts_with("archive|"), "{}", post.key);
    let stored = DescriptionStore::load(&harness.state().services.paths.descriptions_file, 0).expect("descriptions.json").store;
    assert_eq!(stored.get(&post.key).map(|entry| entry.caption.as_str()), Some("Changed in the browser"));

    // The editor's own Save of another post later keeps the browser's edit.
    let other = harness.state().state.posts().iter().find(|post| post.id == "FXTbasic003").map(|post| post.key.clone()).expect("another post");
    harness.state_mut().dispatch(Action::OpenEditor(other.clone()));
    harness.state_mut().dispatch(Action::EditorInput("Changed in the app".to_owned()));
    harness.state_mut().dispatch(Action::SaveDescription);
    settle_idle(&mut harness);
    let stored = DescriptionStore::load(&harness.state().services.paths.descriptions_file, 0).expect("descriptions.json").store;
    assert_eq!(stored.get(&post.key).map(|entry| entry.caption.as_str()), Some("Changed in the browser"));
    assert_eq!(stored.get(&other).map(|entry| entry.caption.as_str()), Some("Changed in the app"));
    // ... and the browser sees the app's edit at its next load.
    harness.step();
    let library: Value = serde_json::from_slice(&request(frontend, "GET /api/library HTTP/1.1\r\nHost: 127.0.0.1", b"").1).expect("JSON");
    let seen = library["posts"].as_array().expect("posts").iter().find(|post| post["id"] == "FXTbasic003").map(|post| post["caption"].clone());
    assert_eq!(seen, Some(Value::from("Changed in the app")));
}

#[test]
fn a_ui_thread_that_does_not_answer_gives_503_and_the_exit_stops_the_server() {
    let profile = profile("web-timeout");
    let mut harness = harness(app_with_web(&profile, vec![fixture("export-basic")], Duration::from_millis(300)));
    let (frontend, backend) = running(&mut harness);
    // No frames run while the client waits.
    let body = br#"{"caption": "Nobody answers"}"#;
    let (status, answer) = request(frontend, &format!("PATCH /api/library/posts/FXTbasic001 HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}", body.len()), body);
    assert_eq!(status, 503);
    assert_eq!(serde_json::from_slice::<Value>(&answer).expect("JSON")["detail"], Value::from("Instagram Windows App did not answer in time. Try again."));
    // The command still waits in the channel; the next frame carries it out for nobody.
    harness.step();
    let started = Instant::now();
    harness.state_mut().shut_down();
    assert!(started.elapsed() < Duration::from_secs(2), "the stop is quick");
    assert!(TcpStream::connect_timeout(&frontend, Duration::from_millis(500)).is_err(), "the page address refuses");
    assert!(TcpStream::connect_timeout(&backend, Duration::from_millis(500)).is_err(), "the API address refuses");
}

#[test]
fn an_import_from_the_browser_is_added_with_a_toast() {
    let profile = profile("web-import");
    let mut harness = harness(app_with_web(&profile, vec![fixture("export-basic")], Duration::from_secs(10)));
    let (frontend, _) = running(&mut harness);
    let same_origin = |method: &str, path: &str, length: usize| format!("{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nSec-Fetch-Site: same-origin\r\nContent-Length: {length}", frontend.port());
    let (status, body) = request(frontend, &same_origin("POST", "/api/library/uploads", 0), b"");
    assert_eq!(status, 201);
    let id = serde_json::from_slice::<Value>(&body).expect("JSON")["upload_id"].as_str().expect("an id").to_owned();
    let jpeg = std::fs::read(fixture("export-basic").join("posts").join("post1.jpg")).expect("a JPEG");
    assert_eq!(request(frontend, &same_origin("PUT", &format!("/api/library/uploads/{id}/files/m000001.jpg"), jpeg.len()), &jpeg).0, 200);
    let commit = serde_json::json!({"source_name": "phone", "source_kind": "folder", "posts": [
        {"id": "WEB1", "media": [{"type": "image", "path": "m000001.jpg"}], "user": {"username": "lumen.atlas"}, "caption": "From the browser", "taken_at": "2030-01-01T00:00:00.000Z"},
        {"id": "WEB2", "media": [{"type": "image", "path": "m000001.jpg"}], "user": {"username": "lumen.atlas"}, "caption": "Twice"}
    ]}).to_string();
    let head = same_origin("POST", &format!("/api/library/uploads/{id}/commit"), commit.len());
    let client = std::thread::spawn(move || request(frontend, &head, commit.as_bytes()));
    while !client.is_finished() {
        harness.step();
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(client.join().expect("the client").0, 200);
    settle_idle(&mut harness);
    assert_eq!(toasts(&harness), ["2 posts added from the browser."]);
    let ids: Vec<String> = harness.state().state.posts().iter().map(|post| post.id.clone()).collect();
    assert_eq!(ids.len(), 8);
    assert_eq!(ids[0], "WEB1", "merged newest first");
    assert_eq!(count_containing(&harness, "Showing 8 posts from 2 folders"), 1, "the banner counts the browser imports as a folder");
    // The More menu now offers to open them again (they are open: not listed).
    harness.get_by_role_and_label(Role::Button, "More").click();
    harness.run_steps(3);
    assert_eq!(count(&harness, "Open browser imports"), 0);
    assert!(!harness.get_by_role_and_label(Role::Button, "Open in browser").accesskit_node().is_disabled());
}

#[test]
fn about_shows_where_the_browser_pages_are() {
    let profile = profile("web-about");
    let mut harness = harness(app_with_web(&profile, vec![fixture("export-basic")], Duration::from_secs(10)));
    let (frontend, backend) = running(&mut harness);
    harness.state_mut().dispatch(Action::ShowAbout);
    harness.run_steps(4);
    assert_eq!(count(&harness, "Browser"), 1);
    assert_eq!(count(&harness, "The posts that are open here can also be seen in a web browser."), 1);
    assert_eq!(count(&harness, &format!("http://127.0.0.1:{}/", frontend.port())), 1, "the page address, as a link");
    assert_eq!(count(&harness, &format!("http://127.0.0.1:{}", backend.port())), 1, "the API address, as a link");
    assert_eq!(count(&harness, "The browser shows what is open when the page is loaded: reload it to see changes made here."), 1);
    // "Browser view" leads to the sheet; About gives way.
    harness.query_all_by_role(Role::Button).find(|node| (node.rect().width() - SHEET_WIDTH).abs() < 0.5 && node.accesskit_node().label().as_deref() == Some("Browser view")).expect("the row").click();
    harness.run_steps(4);
    assert!(harness.state().state.about_opened_at.is_none());
    assert!(harness.state().state.browser_sheet.is_some());
}

#[test]
fn without_a_server_the_menu_and_about_say_so() {
    let profile = profile("web-off");
    let mut harness = harness(app_with(&profile, &fixture("export-basic")));
    settle_idle(&mut harness);
    assert_eq!(harness.state().services.web.status(), &WebStatus::Off(instagram_windows_app::web::OffReason::NotStarted));
    harness.get_by_role_and_label(Role::Button, "More").click();
    harness.run_steps(3);
    assert!(harness.get_by_role_and_label(Role::Button, "Open in browser").accesskit_node().is_disabled());
    assert!(!harness.get_by_role_and_label(Role::Button, "Browser view").accesskit_node().is_disabled());
    harness.get_by_role_and_label(Role::Button, "About").click();
    harness.run_steps(4);
    assert_eq!(count(&harness, "The browser pages are not started in this run. Turn them on in Browser view."), 1);
}

/// A port nobody listens on (bound once and released).
fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").and_then(|listener| listener.local_addr()).map(|address| address.port()).expect("a free port")
}

/// Replaces the text of a port field of the Browser view sheet through the keyboard.
fn type_port(harness: &mut Harness<'_, App>, label: &str, text: &str) {
    harness.get_by_role_and_label(Role::TextInput, label).click();
    harness.run_steps(2);
    harness.key_press_modifiers(Modifiers::COMMAND, Key::A);
    harness.run_steps(1);
    harness.get_by_role_and_label(Role::TextInput, label).type_text(text);
    harness.run_steps(2);
}

/// The Save row of the sheet.
fn press_save(harness: &mut Harness<'_, App>) {
    harness.query_all_by_role(Role::Button).find(|node| (node.rect().width() - SHEET_WIDTH).abs() < 0.5 && node.accesskit_node().label().as_deref() == Some("Save")).expect("Save").click();
    harness.run_steps(2);
}

#[test]
fn the_browser_view_sheet_moves_the_server_and_saves_the_ports() {
    let profile = profile("web-sheet");
    let mut harness = harness(app_with_web(&profile, vec![fixture("export-basic")], Duration::from_secs(10)));
    let (old_frontend, old_backend) = running(&mut harness);
    harness.get_by_role_and_label(Role::Button, "More").click();
    harness.run_steps(3);
    harness.get_by_role_and_label(Role::Button, "Browser view").click();
    harness.run_steps(4);
    assert_eq!(count(&harness, "Page address"), 1);
    assert_eq!(count(&harness, &format!("http://127.0.0.1:{}/", old_frontend.port())), 1);
    assert_eq!(count(&harness, "Allow other devices on this network"), 1);

    // The checks of the fields: nothing is changed.
    type_port(&mut harness, "Page address port", "70000");
    press_save(&mut harness);
    assert_eq!(count(&harness, "Page address port must be a whole number from 1 to 65535."), 1);
    type_port(&mut harness, "Page address port", "5555");
    type_port(&mut harness, "API address port", "5555");
    press_save(&mut harness);
    assert_eq!(count(&harness, "The page address and the API address need two different ports."), 1);

    // A port another program holds is refused with its number; the server stays where it is.
    let taken = TcpListener::bind("127.0.0.1:0").expect("another program");
    let taken_port = taken.local_addr().expect("address").port();
    type_port(&mut harness, "Page address port", &taken_port.to_string());
    type_port(&mut harness, "API address port", &free_port().to_string());
    press_save(&mut harness);
    settle(&mut harness, "the refusal", |app| app.state.browser_sheet.as_ref().is_some_and(|sheet| !sheet.saving && sheet.error.is_some()));
    assert_eq!(count(&harness, &format!("Port {taken_port} is used by another program. Choose another port.")), 1);
    assert_eq!(harness.state().services.web.running().map(|running| running.frontend), Some(old_frontend));
    let settings_text = std::fs::read_to_string(&harness.state().services.paths.settings_file).expect("settings.json");
    assert!(!settings_text.contains("frontend_port"), "nothing was saved: {settings_text}");
    drop(taken);

    // Two free ports: the server moves, settings.json gets them, the sheet closes with a toast.
    let (page, api) = (free_port(), free_port());
    type_port(&mut harness, "Page address port", &page.to_string());
    type_port(&mut harness, "API address port", &api.to_string());
    press_save(&mut harness);
    settle(&mut harness, "the move", |app| app.state.browser_sheet.is_none());
    assert_eq!(toasts(&harness), ["Saved in settings.json."]);
    let running = harness.state().services.web.running().cloned().expect("running");
    assert_eq!((running.frontend.port(), running.backend.port()), (page, api));
    assert!(TcpStream::connect_timeout(&old_frontend, Duration::from_millis(500)).is_err(), "the old page address refuses");
    assert!(TcpStream::connect_timeout(&old_backend, Duration::from_millis(500)).is_err(), "the old API address refuses");
    assert_eq!(request(running.frontend, "GET /api/health HTTP/1.1\r\nHost: 127.0.0.1", b"").0, 200);
    let saved: Value = serde_json::from_str(&std::fs::read_to_string(&harness.state().services.paths.settings_file).expect("settings.json")).expect("JSON");
    assert_eq!((saved["frontend_port"].as_u64(), saved["backend_port"].as_u64(), saved["web_host"].as_str(), saved["web_server"].as_bool()), (Some(u64::from(page)), Some(u64::from(api)), Some("127.0.0.1"), Some(true)));
    assert!(saved.get("data_directory").is_some(), "the other keys stay");

    // Off: the server stops and settings.json says so.
    harness.state_mut().dispatch(Action::ShowBrowserView);
    harness.state_mut().dispatch(Action::BrowserViewServer(false));
    harness.state_mut().dispatch(Action::BrowserViewSave);
    settle(&mut harness, "the stop", |app| app.state.browser_sheet.is_none());
    assert_eq!(harness.state().services.web.status(), &WebStatus::Off(instagram_windows_app::web::OffReason::Sheet));
    let deadline = Instant::now() + Duration::from_secs(5);
    while TcpStream::connect_timeout(&running.frontend, Duration::from_millis(200)).is_ok() {
        assert!(Instant::now() < deadline, "the page address still answers");
        std::thread::sleep(Duration::from_millis(50));
    }
    let saved: Value = serde_json::from_str(&std::fs::read_to_string(&harness.state().services.paths.settings_file).expect("settings.json")).expect("JSON");
    assert_eq!(saved["web_server"].as_bool(), Some(false));
}

#[test]
fn a_save_in_a_command_line_run_keeps_the_saved_ports_it_was_not_asked_to_change() {
    let profile = profile("web-sheet-cli");
    // settings.json has ports of its own; this run takes others from the command line.
    let settings_folder = profile.join("instagram-windows-app");
    std::fs::create_dir_all(&settings_folder).expect("the settings folder");
    std::fs::write(settings_folder.join("settings.json"), "{\n  \"frontend_port\": 3301,\n  \"backend_port\": 3302\n}\n").expect("settings.json");
    let mut configuration = config(&profile);
    let settings = WebSettings { frontend_port: Some(3301), backend_port: Some(3302), ..WebSettings::default() };
    configuration.web = Some(WebConfig::new(settings, WebOverrides { frontend_port: Some(0), backend_port: Some(0), ..WebOverrides::default() }, web_assets::WEB_FILES));
    let mut harness = harness(App::new(configuration));
    let (frontend, backend) = running(&mut harness);

    // The fields show what settings.json keeps; the running addresses are above them.
    harness.state_mut().dispatch(Action::ShowBrowserView);
    harness.run_steps(4);
    let fields = |harness: &Harness<'_, App>| harness.state().state.browser_sheet.as_ref().map(|sheet| (sheet.frontend_text.clone(), sheet.backend_text.clone()));
    assert_eq!(fields(&harness), Some(("3301".to_owned(), "3302".to_owned())));
    assert_eq!(count(&harness, &format!("http://127.0.0.1:{}/", frontend.port())), 1, "the page address of this run");
    // Said once, at the end, naming what the command line set.
    assert_eq!(count(&harness, "The command line sets the ports for this run. The controls show settings.json, and Save keeps them for later runs."), 1);
    assert_eq!(count_containing(&harness, "command line"), 1);

    // Only the page port is typed: the API port of settings.json stays, the run keeps its ports.
    harness.state_mut().dispatch(Action::BrowserViewPort(instagram_windows_app::state::PortField::Frontend, "3321".to_owned()));
    harness.state_mut().dispatch(Action::BrowserViewSave);
    settle(&mut harness, "the save", |app| app.state.browser_sheet.is_none());
    assert_eq!(toasts(&harness), ["Saved in settings.json for later runs; the command line decides for this run."]);
    let saved: Value = serde_json::from_str(&std::fs::read_to_string(settings_folder.join("settings.json")).expect("settings.json")).expect("JSON");
    assert_eq!((saved["frontend_port"].as_u64(), saved["backend_port"].as_u64()), (Some(3321), Some(3302)), "the run's API port was not saved");
    let running = harness.state().services.web.running().cloned().expect("running");
    assert_eq!((running.frontend, running.backend), (frontend, backend), "the command line decides for this run");

    // Opened again: what was saved, so a second Save cannot store the run's ports either.
    harness.state_mut().dispatch(Action::ShowBrowserView);
    harness.run_steps(2);
    assert_eq!(fields(&harness), Some(("3321".to_owned(), "3302".to_owned())));
}

#[test]
fn every_value_of_the_command_line_is_named_in_one_note() {
    let profile = profile("web-sheet-cli-all");
    let mut configuration = config(&profile);
    configuration.web = Some(WebConfig::new(WebSettings::default(), WebOverrides { no_web: true, host: Some("127.0.0.1".parse().expect("address")), frontend_port: Some(4741), backend_port: Some(4742) }, web_assets::WEB_FILES));
    let mut harness = harness(App::new(configuration));
    settle_idle(&mut harness);
    harness.state_mut().dispatch(Action::ShowBrowserView);
    harness.run_steps(4);
    assert_eq!(count(&harness, "The command line sets the server switch, the ports and the network switch for this run. The controls show settings.json, and Save keeps them for later runs."), 1);
    assert_eq!(count_containing(&harness, "command line"), 1, "one note, not one under each control");
    assert_eq!(count(&harness, "The browser pages are off for this run (--no-web)."), 1);
}

#[test]
fn a_moved_port_is_said_in_full_and_the_sheet_keeps_save_in_view() {
    let profile = profile("web-moved");
    // Another program holds the page port of settings.json: the server runs on the next one.
    let taken = TcpListener::bind("127.0.0.1:0").expect("another program");
    let taken_port = taken.local_addr().expect("address").port();
    let mut configuration = config(&profile);
    let settings = WebSettings { frontend_port: Some(taken_port), backend_port: Some(0), ..WebSettings::default() };
    configuration.web = Some(WebConfig::new(settings, WebOverrides::default(), web_assets::WEB_FILES));
    let mut harness = harness_sized(App::new(configuration), (800.0, 600.0));
    let (frontend, _) = running(&mut harness);
    assert_ne!(frontend.port(), taken_port);
    harness.state_mut().dispatch(Action::ShowBrowserView);
    harness.run_steps(4);
    // The whole sentence, wrapped inside the sheet (it was cut with an ellipsis on one line).
    let sentence = format!("Port {taken_port} was taken when the server started, so it runs on {}.", frontend.port());
    let rect = harness.get_by_label(&sentence).rect();
    assert!(rect.height() > 30.0 && rect.width() <= SHEET_WIDTH - 40.0 + 0.5, "two lines within the sheet: {rect:?}");
    // At 800 x 600 Save and Cancel are inside the window.
    for row in ["Save", "Cancel"] {
        let node = harness.query_all_by_role(Role::Button).find(|node| (node.rect().width() - SHEET_WIDTH).abs() < 0.5 && node.accesskit_node().label().as_deref() == Some(row)).expect(row);
        assert!(node.rect().bottom() <= 600.0 - 20.0 + 0.5, "{row} is in view: {:?}", node.rect());
    }
    drop(taken);
}

#[test]
fn inside_browser_view_the_off_line_points_below_and_about_keeps_its_own() {
    let profile = profile("web-off-sheet");
    let mut harness = harness(app_with(&profile, &fixture("export-basic")));
    settle_idle(&mut harness);
    harness.state_mut().dispatch(Action::ShowBrowserView);
    harness.run_steps(4);
    assert_eq!(count(&harness, "The browser pages are not started in this run. Turn them on below."), 1);
    assert_eq!(count_containing(&harness, "Turn them on in Browser view"), 0, "not inside Browser view itself");
}

#[test]
fn the_lead_of_browser_view_is_read_out_as_it_is_painted() {
    // A screen reader reads the sheet's own shorter lead, not About's sentence.
    let profile = profile("web-sheet-lead");
    let mut harness = harness(app_with(&profile, &fixture("export-basic")));
    settle_idle(&mut harness);
    harness.state_mut().dispatch(Action::ShowBrowserView);
    harness.run_steps(4);
    assert_eq!(count(&harness, instagram_windows_app::strings::BROWSER_VIEW_LEAD), 1);
    assert_eq!(count(&harness, instagram_windows_app::strings::ABOUT_BROWSER_LEAD), 0);
    harness.get_by_label(instagram_windows_app::strings::BROWSER_VIEW_LEAD);
}

#[test]
fn an_invalid_settings_file_keeps_the_new_ports_for_this_run_only() {
    let profile = profile("web-sheet-invalid");
    let mut harness = harness(app_with_web(&profile, Vec::new(), Duration::from_secs(10)));
    running(&mut harness);
    let settings_file = harness.state().services.paths.settings_file.clone();
    std::fs::write(&settings_file, b"{ not json").expect("a broken settings.json");
    let (page, api) = (free_port(), free_port());
    harness.state_mut().dispatch(Action::ShowBrowserView);
    harness.state_mut().dispatch(Action::BrowserViewPort(instagram_windows_app::state::PortField::Frontend, page.to_string()));
    harness.state_mut().dispatch(Action::BrowserViewPort(instagram_windows_app::state::PortField::Backend, api.to_string()));
    harness.state_mut().dispatch(Action::BrowserViewSave);
    settle(&mut harness, "the save", |app| app.state.browser_sheet.as_ref().is_some_and(|sheet| !sheet.saving && sheet.error.is_some()));
    let error = harness.state().state.browser_sheet.as_ref().and_then(|sheet| sheet.error.clone()).expect("an error");
    assert!(error.starts_with("settings.json could not be saved: it is not valid JSON (") && error.ends_with(". The new values apply until the app is closed."), "{error}");
    assert_eq!(std::fs::read(&settings_file).expect("the file"), b"{ not json", "nothing on disk changed");
    assert_eq!(harness.state().services.web.running().map(|running| running.frontend.port()), Some(page), "the new port serves in this run");
}

/// Copies a folder (one level of subfolders is enough for the fixtures).
fn copy_folder(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).expect("a folder");
    for entry in std::fs::read_dir(from).expect("the fixture").flatten() {
        let target = to.join(entry.file_name());
        if entry.path().is_dir() {
            copy_folder(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).expect("a file");
        }
    }
}

#[test]
fn the_more_menu_offers_the_browser_imports_while_they_are_not_open() {
    let profile = profile("web-open-imports");
    let mut harness = harness(app_with(&profile, &fixture("loose")));
    settle_idle(&mut harness);
    let library_dir = harness.state().services.web.library_dir().to_path_buf();
    // The More menu looks once as it opens: nothing there yet.
    harness.get_by_role_and_label(Role::Button, "More").click();
    harness.run_steps(3);
    assert_eq!(count(&harness, "Open browser imports"), 0);
    harness.key_press(Key::Escape);
    harness.run_steps(2);
    copy_folder(&fixture("export-basic"), &library_dir);
    harness.get_by_role_and_label(Role::Button, "More").click();
    harness.run_steps(3);
    harness.get_by_role_and_label(Role::Button, "Open browser imports").click();
    settle(&mut harness, "the browser imports to open", |app| app.state.collection.as_ref().is_some_and(|collection| collection.is_export()));
    let sources = harness.state().state.collection.as_ref().map(|collection| collection.sources().to_vec()).unwrap_or_default();
    assert_eq!(sources.len(), 1);
    assert_eq!(sources[0].kind, instagram_core::model::ExportKind::WebLibrary);
    assert_eq!(count_containing(&harness, "Showing 6 posts from Browser imports"), 1);
}
