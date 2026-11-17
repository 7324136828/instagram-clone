//! Headless tests of the viewer and the Explore grid: opening, moving between pictures with the
//! keys, the carousel inside the viewer, zoom, and closing; and what is left undrawn because the
//! card of the viewer or the black of auto view covers it (rendering.rs).

mod common;

use common::{app_with, config, copy_of_loose, count, count_containing, fixture, harness, harness_sized, profile, settle, settle_idle, toasts, TestFolder};
use egui::accesskit::Role;
use egui::{pos2, vec2, Color32, Event, Key, Modifiers, MouseWheelSource, MouseWheelUnit, PointerButton, Shape, TouchPhase};
use egui_kittest::kittest::Queryable as _;
use egui_kittest::Harness;
use instagram_core::catalog::{self, OpenOptions, OpenRequest};
use instagram_core::model::PostKey;
use instagram_windows_app::app::App;
use instagram_windows_app::state::{Action, View};
use instagram_windows_app::viewer;
use instagram_windows_app::zoom::Zoom;

/// The key of the post the viewer shows.
fn shown(harness: &Harness<'_, App>) -> Option<PostKey> {
    harness.state().state.viewer.as_ref().map(|viewer| viewer.key.clone())
}

/// The index of the post the viewer shows.
fn shown_index(harness: &Harness<'_, App>) -> Option<usize> {
    let key = shown(harness)?;
    harness.state().state.posts().iter().position(|post| post.key == key)
}

/// Presses `key` and runs frames until the app is idle again.
fn press(harness: &mut Harness<'_, App>, key: Key) {
    harness.key_press(key);
    settle_idle(harness);
}

/// An export-basic app with the viewer open on post `index`, after its profile folder: bound
/// first, the folder is dropped (and removed) after the harness.
fn viewer_on(test: &str, index: usize) -> (TestFolder, Harness<'static, App>) {
    let profile = profile(test);
    let mut app = app_with(&profile, &fixture("export-basic"));
    let key = app.state.posts()[index].key.clone();
    app.dispatch(Action::OpenViewer(key));
    let mut harness = harness(app);
    settle_idle(&mut harness);
    (profile, harness)
}

#[test]
fn the_arrow_keys_move_between_pictures_and_page_a_carousel_first() {
    let (_profile, mut harness) = viewer_on("viewer-keys", 3);
    assert_eq!(shown_index(&harness), Some(3));
    assert_eq!(harness.query_all_by_role_and_label(Role::Dialog, "Post by pixel.orchard.demo").count(), 1);
    assert_eq!(harness.state().state.window_title(), "post4.heic - Instagram Windows App");
    // The picture after this one is decoded already (the feed behind the viewer is at its top
    // and has not asked for it); the one after that is not.
    let next_picture = harness.state().state.posts()[4].media[0].path.clone();
    let later_picture = harness.state().state.posts()[5].media[0].path.clone();
    assert!(harness.state_mut().services.loader.any_rendition(&next_picture).is_some(), "the next picture is decoded ahead");
    assert!(harness.state_mut().services.loader.any_rendition(&later_picture).is_none());

    // Right: the next post, a carousel of two; Right again pages it; a third Right leaves it.
    press(&mut harness, Key::ArrowRight);
    assert_eq!(shown_index(&harness), Some(4));
    let carousel = shown(&harness).expect("the viewer is open");
    assert_eq!(harness.state().state.post_ui(&carousel).slide, 0);
    assert_eq!(count(&harness, "1/2"), 1);
    assert_eq!(harness.state().state.window_title(), "post5_1.jpg - Instagram Windows App");
    press(&mut harness, Key::ArrowRight);
    assert_eq!(shown_index(&harness), Some(4), "the carousel is paged first");
    assert_eq!(harness.state().state.post_ui(&carousel).slide, 1);
    assert_eq!(count(&harness, "2/2"), 1);
    assert_eq!(harness.state().state.window_title(), "post5_2.jpg - Instagram Windows App");
    press(&mut harness, Key::ArrowRight);
    assert_eq!(shown_index(&harness), Some(5));
    // The last post: Right does nothing more, and there is no arrow to a next picture.
    press(&mut harness, Key::ArrowRight);
    assert_eq!(shown_index(&harness), Some(5));
    assert_eq!(harness.query_all_by_role_and_label(Role::Button, "Next picture").count(), 0);
    assert_eq!(harness.query_all_by_role_and_label(Role::Button, "Previous picture").count(), 1);

    // Left: back into the carousel, which is still on its second slide, then through it.
    press(&mut harness, Key::ArrowLeft);
    assert_eq!(shown_index(&harness), Some(4));
    assert_eq!(harness.state().state.post_ui(&carousel).slide, 1);
    press(&mut harness, Key::ArrowLeft);
    assert_eq!((shown_index(&harness), harness.state().state.post_ui(&carousel).slide), (Some(4), 0));
    press(&mut harness, Key::ArrowLeft);
    assert_eq!(shown_index(&harness), Some(3));

    // Home and End.
    press(&mut harness, Key::Home);
    assert_eq!(shown_index(&harness), Some(0));
    assert_eq!(harness.query_all_by_role_and_label(Role::Button, "Previous picture").count(), 0);
    press(&mut harness, Key::ArrowLeft);
    assert_eq!(shown_index(&harness), Some(0), "nothing before the first picture");
    press(&mut harness, Key::End);
    assert_eq!(shown_index(&harness), Some(5));

    // The round arrows do the same as the keys.
    harness.get_by_role_and_label(Role::Button, "Previous picture").click();
    settle_idle(&mut harness);
    assert_eq!(shown_index(&harness), Some(4));
    // The back and forward buttons of a mouse too.
    harness.event(Event::PointerButton { pos: pos2(400.0, 400.0), button: PointerButton::Extra1, pressed: true, modifiers: Modifiers::NONE });
    harness.event(Event::PointerButton { pos: pos2(400.0, 400.0), button: PointerButton::Extra1, pressed: false, modifiers: Modifiers::NONE });
    settle_idle(&mut harness);
    assert_eq!(shown_index(&harness), Some(3));
    harness.event(Event::PointerButton { pos: pos2(400.0, 400.0), button: PointerButton::Extra2, pressed: true, modifiers: Modifiers::NONE });
    harness.event(Event::PointerButton { pos: pos2(400.0, 400.0), button: PointerButton::Extra2, pressed: false, modifiers: Modifiers::NONE });
    settle_idle(&mut harness);
    assert_eq!(shown_index(&harness), Some(4));
}

#[test]
fn the_viewer_shows_the_post_and_closes_layer_by_layer() {
    let (_profile, mut harness) = viewer_on("viewer-content", 0);
    let first = shown(&harness).expect("the viewer is open");

    // The column: author, location, the whole description (never collapsed), the comments of
    // the export, the link to the rest, likes and date. The feed card behind it shows some of
    // these as well, hence "at least".
    assert!(count(&harness, "fixture.sunrise.bakery") >= 2);
    assert!(count(&harness, "Fixtureville, Testland") >= 2);
    assert!(count_containing(&harness, "Come early, the cinnamon knots never last past nine.") >= 1, "the viewer shows the whole description");
    assert_eq!(count_containing(&harness, "Saving me two of those"), 2);
    assert_eq!(count(&harness, "3 likes"), 1, "the likes of a comment");
    assert_eq!(harness.query_all_by_role_and_label(Role::Button, "View all 1,234 comments on Instagram").count(), 1);
    assert!(count(&harness, "56.8K likes") >= 2);

    // Like in the viewer: the count follows, the card behind it too.
    let like = harness.get_all_by_role_and_label(Role::Button, "Like").max_by(|a, b| a.rect().left().total_cmp(&b.rect().left())).expect("the viewer's heart");
    like.click();
    harness.run_steps(3);
    assert!(harness.state().state.post_ui(&first).liked);
    assert_eq!(harness.state().state.post_ui(&first).burst_at, None, "the viewer has no heart animation");

    // Share copies the link.
    let share = harness.get_all_by_role_and_label(Role::Button, "Share").max_by(|a, b| a.rect().left().total_cmp(&b.rect().left())).expect("the viewer's share button");
    share.click();
    harness.run_steps(3);
    assert_eq!(toasts(&harness), ["Link copied."]);

    // Escape closes the sheet first, then the viewer.
    harness.state_mut().dispatch(Action::OpenPostOptions(first.clone()));
    harness.run_steps(3);
    harness.key_press(Key::Escape);
    harness.run_steps(4);
    assert!(harness.state().state.sheet.is_none());
    assert!(harness.state().state.viewer.is_some());
    // While the sheet was open the arrow keys did not reach the viewer.
    harness.state_mut().dispatch(Action::OpenPostOptions(first.clone()));
    harness.run_steps(3);
    harness.key_press(Key::ArrowRight);
    harness.run_steps(3);
    assert_eq!(shown(&harness), Some(first));
    harness.key_press(Key::Escape);
    harness.run_steps(4);
    harness.key_press(Key::Escape);
    harness.run_steps(4);
    assert!(harness.state().state.viewer.is_none());
    assert_eq!(harness.state().state.window_title(), "Instagram Windows App");

    // The X closes it too, and so does a click on the scrim beside the card.
    let key = harness.state().state.posts()[1].key.clone();
    harness.state_mut().dispatch(Action::OpenViewer(key.clone()));
    settle_idle(&mut harness);
    let close = harness.get_all_by_role_and_label(Role::Button, "Close").min_by(|a, b| a.rect().top().total_cmp(&b.rect().top())).expect("the X of the viewer");
    close.click();
    harness.run_steps(3);
    assert!(harness.state().state.viewer.is_none());

    harness.state_mut().dispatch(Action::OpenViewer(key));
    settle_idle(&mut harness);
    let layout = viewer::layout(egui::Rect::from_min_size(pos2(0.0, 0.0), vec2(common::WINDOW.0, common::WINDOW.1)));
    // A click on the card does not close it; one in the strip above the card does.
    let on_card = layout.side.center();
    harness.event(Event::PointerMoved(on_card));
    harness.event(Event::PointerButton { pos: on_card, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE });
    harness.event(Event::PointerButton { pos: on_card, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE });
    harness.run_steps(3);
    assert!(harness.state().state.viewer.is_some());
    let on_scrim = pos2(layout.card.center().x, layout.card.top() / 2.0);
    harness.event(Event::PointerMoved(on_scrim));
    harness.event(Event::PointerButton { pos: on_scrim, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE });
    harness.event(Event::PointerButton { pos: on_scrim, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE });
    harness.run_steps(3);
    assert!(harness.state().state.viewer.is_none());
}

#[test]
fn the_keys_the_wheel_and_a_double_click_zoom_the_picture() {
    // Post 3 is 960 x 540 on a stage of 851 x 740 points: fitted at 89 %.
    let (_profile, mut harness) = viewer_on("viewer-zoom", 2);
    // A test frame is a quarter of a second: give the two clicks of a double click the time.
    harness.ctx.options_mut(|options| options.input_options.max_double_click_delay = 1.0);
    assert_eq!(harness.state().viewer.zoom(), Zoom::FIT);
    assert_eq!(harness.state().viewer.zoom_percent(), Some(89));
    assert_eq!(count_containing(&harness, "%"), 0, "no indicator while the picture is fitted");

    // "+" steps up and stops at 100 %; again goes on to 125 %.
    press(&mut harness, Key::Plus);
    assert_eq!(harness.state().viewer.zoom().scale, Some(1.0));
    assert_eq!(count(&harness, "100%"), 1, "the indicator shows while the picture is not fitted");
    press(&mut harness, Key::Plus);
    assert_eq!(harness.state().viewer.zoom_percent(), Some(125));
    press(&mut harness, Key::Minus);
    assert_eq!(harness.state().viewer.zoom_percent(), Some(100));
    // "0" fits the picture again.
    press(&mut harness, Key::Num0);
    assert_eq!(harness.state().viewer.zoom(), Zoom::FIT);
    // "-" at the fitted size does nothing.
    press(&mut harness, Key::Minus);
    assert_eq!(harness.state().viewer.zoom(), Zoom::FIT);

    // A double click goes to 100 % around the click, another one back.
    let layout = viewer::layout(egui::Rect::from_min_size(pos2(0.0, 0.0), vec2(common::WINDOW.0, common::WINDOW.1)));
    let point = layout.stage.center() + vec2(120.0, -60.0);
    let click = |harness: &Harness<'_, App>| {
        harness.event(Event::PointerMoved(point));
        harness.event(Event::PointerButton { pos: point, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE });
        harness.event(Event::PointerButton { pos: point, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE });
    };
    click(&harness);
    click(&harness);
    settle_idle(&mut harness);
    let zoomed = harness.state().viewer.zoom();
    assert_eq!(zoomed.scale, Some(1.0));
    // The click was right of the centre: the picture moved left to keep that point in place.
    // (It is not as high as the stage, so it stays centred vertically.)
    assert!(zoomed.pan.x < 0.0 && zoomed.pan.y == 0.0, "the picture grew around the click point: {:?}", zoomed.pan);
    // A while later (a third click right away would be a triple click) the same again.
    harness.run_steps(8);
    click(&harness);
    click(&harness);
    settle_idle(&mut harness);
    assert_eq!(harness.state().viewer.zoom(), Zoom::FIT);

    // The wheel zooms in around the pointer, and back out to the fitted picture.
    let wheel = |notches: f32| Event::MouseWheel { unit: MouseWheelUnit::Line, delta: vec2(0.0, notches), phase: TouchPhase::Move, source: MouseWheelSource::Unknown, modifiers: Modifiers::NONE };
    harness.event(Event::PointerMoved(point));
    harness.event(wheel(3.0));
    harness.run_steps(12);
    settle_idle(&mut harness);
    let percent = harness.state().viewer.zoom_percent().expect("the size of the picture is known");
    assert!((140..=160).contains(&percent), "three notches of 20 % from 89 % give about 154 %, not {percent} %");
    assert_eq!(harness.state().viewer.zoom().scale.map(|scale| scale > 1.0), Some(true));
    harness.event(wheel(-10.0));
    harness.run_steps(12);
    settle_idle(&mut harness);
    assert_eq!(harness.state().viewer.zoom(), Zoom::FIT);

    // Zoomed in, dragging moves the picture; another picture starts fitted again.
    harness.state_mut().viewer.request_zoom(Some(3.0));
    settle_idle(&mut harness);
    assert_eq!(harness.state().viewer.zoom().pan, vec2(0.0, 0.0));
    harness.event(Event::PointerMoved(point));
    harness.event(Event::PointerButton { pos: point, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE });
    harness.event(Event::PointerMoved(point + vec2(-80.0, 30.0)));
    harness.event(Event::PointerButton { pos: point + vec2(-80.0, 30.0), button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE });
    settle_idle(&mut harness);
    let pan = harness.state().viewer.zoom().pan;
    assert!((pan.x + 80.0).abs() < 1.0 && (pan.y - 30.0).abs() < 1.0, "the picture followed the drag: {pan:?}");
    assert!(harness.state().state.viewer.is_some(), "a drag is not a click on the scrim");
    press(&mut harness, Key::ArrowRight);
    assert_eq!(harness.state().viewer.zoom(), Zoom::FIT);
}

#[test]
fn one_picture_file_opens_in_the_viewer_on_that_picture() {
    let profile = profile("viewer-one-file");
    let pictures = copy_of_loose(&profile);
    let picked = pictures.join("IMG_10.jpg");
    let mut app = App::new(config(&profile));
    let request = OpenRequest { paths: vec![picked], options: OpenOptions { default_username: app.services.default_username.clone() } };
    let opened = catalog::open_paths(&request, &app.services.store, &app.services.context).expect("the picture opens");
    assert!(opened.focus.is_some(), "one picked file is the focus of its folder");
    app.show_opened(opened);
    let mut harness = harness(app);
    settle_idle(&mut harness);

    // The collection is the folder; the viewer is on the picked picture.
    let app = harness.state();
    assert_eq!(app.state.posts().len(), 5);
    let index = shown_index(&harness).expect("the viewer opened by itself");
    assert_eq!(app.state.posts()[index].context.as_deref(), Some("IMG_10.jpg"));
    assert_eq!(app.state.window_title(), "IMG_10.jpg - Instagram Windows App");
    // The file name is the line under the header, in the viewer and on the card behind it.
    assert!(count(&harness, "IMG_10.jpg") >= 2);
    // A loose picture has no links: no "View on" button, and its sheet has no link rows.
    assert_eq!(harness.query_all_by_label_contains("View on").count(), 0);

    // Its neighbours in file-name order are one key press away.
    press(&mut harness, Key::ArrowLeft);
    assert_eq!(harness.state().state.window_title(), "IMG_2.jpg - Instagram Windows App");
    press(&mut harness, Key::End);
    assert_eq!(harness.state().state.window_title(), "Sunset at the pier (final).webp - Instagram Windows App");

    // Closing the viewer after moving: the feed has followed to the last picture.
    press(&mut harness, Key::Escape);
    assert!(harness.state().state.viewer.is_none());
    settle(&mut harness, "the feed to follow", |app| app.feed.scroll_offset() > 500.0);
    assert_eq!(count(&harness, "Sunset at the pier (final).webp"), 1);
}

#[test]
fn the_explore_grid_opens_the_viewer_and_paints_only_what_is_in_view() {
    let profile = profile("explore");
    let mut harness = harness(app_with(&profile, &fixture("export-basic")));
    settle_idle(&mut harness);

    harness.get_by_role_and_label(Role::Button, "Explore").click();
    settle_idle(&mut harness);
    assert_eq!(harness.state().state.view, View::Explore);
    assert_eq!(count(&harness, "Explore"), 2, "the sidebar item and the heading");
    assert_eq!(harness.state().explore.painted_tiles(), 6);
    assert_eq!(harness.query_all_by_role_and_label(Role::Button, "Open post by fixture.sunrise.bakery").count(), 2);
    // The carousel and the video carry their badges.
    assert_eq!(count(&harness, "Carousel"), 1);
    assert_eq!(count(&harness, "Video"), 1);

    // Under the pointer a tile shows its counts.
    let tile = harness.get_all_by_role_and_label(Role::Button, "Open post by fixture.sunrise.bakery").min_by(|a, b| a.rect().left().total_cmp(&b.rect().left())).expect("the first tile");
    assert_eq!(tile.rect().size(), vec2(309.0, 309.0), "tiles of the reference size in a wide window");
    tile.hover();
    harness.run_steps(3);
    assert_eq!(count(&harness, "56.8K"), 1);
    assert_eq!(count(&harness, "1.2K"), 1);

    // A click opens the viewer on that post; the grid stays behind it.
    let tile = harness.get_all_by_role_and_label(Role::Button, "Open post by fixture.sunrise.bakery").min_by(|a, b| a.rect().left().total_cmp(&b.rect().left())).expect("the first tile");
    tile.click();
    settle_idle(&mut harness);
    assert_eq!(shown_index(&harness), Some(0));
    assert_eq!(harness.state().state.view, View::Explore);
    press(&mut harness, Key::Escape);
    assert!(harness.state().state.viewer.is_none());
    assert_eq!(harness.state().state.view, View::Explore);

    // Home leads back to the feed.
    harness.get_by_role_and_label(Role::Button, "Home").click();
    settle_idle(&mut harness);
    assert_eq!(harness.state().state.view, View::Home);
    assert_eq!(count(&harness, "Showing 6 posts from export-basic"), 1);
}

#[test]
fn a_long_grid_lays_out_only_the_rows_near_the_view() {
    let profile = profile("explore-long");
    let pictures = profile.join("pictures");
    std::fs::create_dir_all(&pictures).expect("pictures folder");
    let png = std::fs::read(fixture("loose").join("img_1.png")).expect("a small png");
    for index in 0..600 {
        std::fs::write(pictures.join(format!("picture_{index:04}.png")), &png).expect("write");
    }
    let mut app = app_with(&profile, &pictures);
    app.dispatch(Action::ShowExplore);
    let mut harness = harness(app);
    settle_idle(&mut harness);

    let app = harness.state();
    assert_eq!(app.state.posts().len(), 600);
    assert!(app.explore.painted_tiles() <= 9, "{} tiles were painted", app.explore.painted_tiles());
    let decoded = app.services.loader.stats().decoded;
    assert!((1..=24).contains(&decoded), "{decoded} pictures were decoded for one screen of a 600-picture grid");

    // The End key goes to the last row.
    press(&mut harness, Key::End);
    settle(&mut harness, "the end of the grid", |app| app.explore.scroll_offset() > 10_000.0);
    assert!(harness.state().explore.painted_tiles() <= 9);
    assert!(harness.state().services.loader.stats().decoded <= 60);
    // A loose picture has no counts to show: its tile opens the viewer all the same.
    let tile = harness.get_all_by_role_and_label(Role::Button, "Open post by tester").max_by(|a, b| (a.rect().top(), a.rect().left()).partial_cmp(&(b.rect().top(), b.rect().left())).expect("finite positions")).expect("the last tile");
    tile.click();
    settle_idle(&mut harness);
    assert_eq!(shown_index(&harness), Some(599));
}

#[test]
fn a_small_window_keeps_the_viewer_usable() {
    // The smallest window: the arrows lie on the picture, the column is narrower.
    let profile = profile("viewer-small");
    let mut app = app_with(&profile, &fixture("export-basic"));
    let key = app.state.posts()[4].key.clone();
    app.dispatch(Action::OpenViewer(key.clone()));
    let mut harness = harness_sized(app, (640.0, 480.0));
    settle_idle(&mut harness);

    let layout = viewer::layout(egui::Rect::from_min_size(pos2(0.0, 0.0), vec2(640.0, 480.0)));
    assert!(layout.arrows_on_stage);
    let previous = harness.get_by_role_and_label(Role::Button, "Previous picture").rect();
    let next_photo = harness.get_by_role_and_label(Role::Button, "Next photo").rect();
    let next = harness.get_by_role_and_label(Role::Button, "Next picture").rect();
    assert!(layout.stage.contains_rect(previous) && layout.stage.contains_rect(next));
    assert!(next_photo.right() < next.left(), "the carousel arrow keeps clear of the arrow to the next picture");
    // Both still work.
    harness.get_by_role_and_label(Role::Button, "Next photo").click();
    settle_idle(&mut harness);
    assert_eq!(harness.state().state.post_ui(&key).slide, 1);
    harness.get_by_role_and_label(Role::Button, "Next picture").click();
    settle_idle(&mut harness);
    assert_eq!(shown_index(&harness), Some(5));
}

// --- What is not drawn (rendering.rs) -------------------------------------------------------------

/// True when `shape` puts no pixel on screen (a fill without colour, nothing at all).
fn paints_nothing(shape: &Shape) -> bool {
    match shape {
        Shape::Noop => true,
        Shape::Rect(rect) => rect.fill == Color32::TRANSPARENT && rect.stroke.is_empty(),
        Shape::Vec(shapes) => shapes.iter().all(paints_nothing),
        _ => false,
    }
}

/// The shapes of the last frame that put pixels on screen, bottom to top.
fn painting_shapes(harness: &Harness<'_, App>) -> Vec<egui::epaint::ClippedShape> {
    harness.output().shapes.iter().filter(|clipped| !paints_nothing(&clipped.shape)).cloned().collect()
}

/// A rectangle over the whole of `screen` in black of some opacity.
fn black_over(shape: &Shape, screen: egui::Rect) -> Option<u8> {
    match shape {
        Shape::Rect(rect) if rect.rect == screen && rect.fill.r() == 0 && rect.fill.g() == 0 && rect.fill.b() == 0 => Some(rect.fill.a()),
        _ => None,
    }
}

#[test]
fn nothing_below_the_card_of_the_viewer_is_drawn_inside_it() {
    let (_profile, harness) = viewer_on("covered-card", 0);
    let screen = harness.ctx.content_rect();
    let layout = viewer::layout(screen);
    let hole = viewer::card_interior(&layout);
    assert_eq!(harness.state().viewer.opaque_area(), Some(hole), "the app learns where the card is opaque");

    // Layers are painted bottom to top: the first shape that reaches into the card is the card
    // itself, the black of its picture area. The page, the sidebar, the feed and the scrim below
    // it are clipped to the bands around the card (rendering::keep_out).
    let mut shapes = painting_shapes(&harness);
    // The test harness wraps the app in a frame of its own (egui_kittest's central panel) and
    // paints its fill, the page colour, below everything when that frame closes, after the app
    // has run. The window has no such frame: eframe hands the app a Ui without a background.
    if matches!(shapes.first().map(|clipped| &clipped.shape), Some(Shape::Rect(rect)) if rect.fill == instagram_windows_app::theme::LIGHT.bg && rect.rect != screen) {
        shapes.remove(0);
    }
    let inside = hole.shrink(1.0);
    let first = shapes.iter().find(|clipped| clipped.clip_rect.intersects(inside) && clipped.shape.visual_bounding_rect().intersects(inside)).expect("the card is painted");
    match &first.shape {
        Shape::Rect(rect) => assert_eq!((rect.rect, rect.fill), (layout.stage, instagram_windows_app::theme::MEDIA_BLACK), "the first shape inside the card"),
        other => panic!("the first shape inside the card is {other:?}"),
    }
    // The scrim is still there around the card: once in each of the four bands, none inside.
    let scrims: Vec<&egui::epaint::ClippedShape> = shapes.iter().filter(|clipped| black_over(&clipped.shape, screen).is_some_and(|alpha| alpha < 255)).collect();
    assert_eq!(scrims.len(), 4, "the scrim, clipped to the four bands around the card");
    assert!(scrims.iter().all(|scrim| !scrim.clip_rect.intersects(inside)));
    let covered: f32 = scrims.iter().map(|scrim| scrim.clip_rect.intersect(screen).area()).sum();
    assert!((covered + hole.area() - screen.area()).abs() < 1.0, "the bands and the card together cover the window once");
}

#[test]
fn auto_view_alone_is_drawn_on_a_window_cleared_to_black() {
    let profile = profile("autoview-cleared");
    let mut app = app_with(&profile, &fixture("export-basic"));
    // As in the window: eframe clears to `clear_color`, the app does not paint the page itself.
    app.paints_background = false;
    let mut harness = harness(app);
    settle_idle(&mut harness);
    let page = eframe::App::clear_color(harness.state(), &egui::Visuals::light());
    assert!(!harness.state().cleared_to_black);

    let start = |harness: &mut Harness<'_, App>| {
        let app = harness.state_mut();
        app.state.auto_view_clock = Some(0.0);
        instagram_windows_app::state::start_auto_view(&mut app.state, &mut app.services, instagram_core::autoview::Settings::default(), 0);
        app.touch();
        settle_idle(harness);
    };
    start(&mut harness);
    let screen = harness.ctx.content_rect();
    assert!(harness.state().cleared_to_black);
    assert_eq!(eframe::App::clear_color(harness.state(), &egui::Visuals::light()), Color32::BLACK.to_normalized_gamma_f32());
    assert!(!painting_shapes(&harness).iter().any(|clipped| black_over(&clipped.shape, screen).is_some()), "no black backdrop over the window's own black");

    // Out of auto view the window is cleared to the page colour again.
    harness.state_mut().dispatch(Action::ExitAutoView);
    settle_idle(&mut harness);
    assert!(!harness.state().cleared_to_black);
    assert_eq!(eframe::App::clear_color(harness.state(), &egui::Visuals::light()), page);

    // Where the app paints the page itself (no window), the show brings its own black.
    harness.state_mut().paints_background = true;
    start(&mut harness);
    assert!(!harness.state().cleared_to_black);
    assert!(painting_shapes(&harness).iter().any(|clipped| black_over(&clipped.shape, screen) == Some(255)), "the black backdrop of the show");
}
