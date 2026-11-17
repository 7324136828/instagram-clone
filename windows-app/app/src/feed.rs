//! The home feed: the banner and the column of post cards, virtualised.
//!
//! A card is the reference web project's `PostCard` (`Feed.tsx:184-335`, `styles.css:166-204`):
//! header, picture, action row, body. Its height depends on the aspect ratio of the cover and on
//! the wrapped caption, and a collection may hold thousands of posts, so the feed never lays out
//! all of them. `RowTable` keeps one height per post (an estimate until the card has been near
//! the view once), only the cards in and around the view are measured, painted and decoded, and
//! a `ScrollAnchor` keeps what the user is looking at in place when cards above it learn their
//! real height. Sizes of pictures are read from the file headers in the background; a frame is
//! square until its size is known and stable afterwards.
//!
//! Videos (video brief 5.3): every video card claims its video with the share of its frame on
//! screen; on a graphics card the most visible one plays muted in a loop with a sound button,
//! on a software renderer a play badge opens the viewer instead.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use egui::epaint::text::{LayoutJob, TextWrapping};
use egui::epaint::StrokeKind;
use egui::{pos2, vec2, Color32, Galley, Id, Key, Modifiers, Pos2, Rect, Sense, Stroke, Ui, UiBuilder, Vec2};
use instagram_core::model::{Post, PostKey};
use instagram_core::text;
use instagram_core::urls;
use instagram_core::video_rules;

use crate::banner;
use crate::icons::{self, Icon, IconStyle};
use crate::layout::{self, RowTable, ScrollAnchor};
use crate::loader::{ImageState, Loader, Priority, SizeState, Variant};
use crate::media::{self, CarouselChrome, Display, PlaceholderLook};
use crate::state::{Action, Env, PostUi, ScrollRequest, BURST_SECONDS};
use crate::strings;
use crate::theme::{self, Palette, FEED_WIDTH, MAIN_PADDING_BOTTOM, MAIN_PADDING_TOP};
use crate::video::{controls, Surface, VideoKey};
use crate::widgets::{self, SpanLink};

// --- Geometry of a card (styles.css:166-204) ----------------------------------------------------

/// Space below every card and below the banner (`margin-bottom: 16px`).
pub const CARD_GAP: f32 = 16.0;
/// Width of the card's border.
const CARD_BORDER: f32 = 1.0;
/// Corner radius of the card.
const CARD_RADIUS: f32 = 8.0;
/// Width of everything inside the border: the picture is this wide.
const INNER_WIDTH: f32 = FEED_WIDTH - 2.0 * CARD_BORDER;
/// Height of the header: 10 px padding around the 40 px ringed avatar.
const HEADER_HEIGHT: f32 = 60.0;
/// Horizontal padding of header, action row and body.
const PADDING_X: f32 = 12.0;
/// Size of the header avatar (40 px with its ring).
const HEADER_AVATAR_SIZE: f32 = 34.0;
/// Space between the parts of the header (`gap: 10px`).
const HEADER_GAP: f32 = 10.0;
/// Space between the parts of the author line (`gap: 5px`).
const AUTHOR_GAP: f32 = 5.0;
/// Font size of the author line.
const AUTHOR_SIZE: f32 = 14.0;
/// Font size of the line under the author line.
const SUBLINE_SIZE: f32 = 12.0;
/// Side of the verified badge in the header.
const HEADER_BADGE_SIZE: f32 = 13.0;
/// Side of the verified badge in the caption.
const CAPTION_BADGE_SIZE: f32 = 12.0;
/// Height of the action row: `padding: 8px 12px 4px` around 26 px icons.
const ACTIONS_HEIGHT: f32 = 38.0;
/// Side of the action icons.
const ACTION_ICON_SIZE: f32 = 26.0;
/// Space between the action icons on the left.
const ACTION_ICON_GAP: f32 = 12.0;
/// Width of the text of the body.
const BODY_WIDTH: f32 = INNER_WIDTH - 2.0 * PADDING_X;
/// Space under the last block of the body.
const BODY_PADDING_BOTTOM: f32 = 12.0;
/// Font size of the body.
const BODY_SIZE: f32 = 14.0;
/// Line height of the body (`line-height: 1.5`).
const BODY_LINE_HEIGHT: f32 = 21.0;
/// Font size of the date line.
const DATE_SIZE: f32 = 10.0;
/// Line height of the date line.
const DATE_LINE_HEIGHT: f32 = 15.0;
/// At most this many exported comments are shown under a caption (`PREVIEW_COMMENTS`).
const PREVIEW_COMMENTS: usize = 2;
/// Height of the block after the last card: 40 px padding around the compass, the title and
/// the hint with 8 px gaps (`.feed__end`).
const FEED_END_HEIGHT: f32 = 40.0 + 40.0 + 8.0 + 21.0 + 8.0 + 17.0 + 40.0;
/// Cards this far outside the view are measured and their pictures decoded ahead.
const OVERSCAN: f32 = 700.0;
/// One press of an arrow key scrolls this far.
const ARROW_SCROLL: f32 = 60.0;
/// Page Up / Page Down keep this much of the previous view on screen.
const PAGE_OVERLAP: f32 = 60.0;

// --- Body: the text blocks under the action row -------------------------------------------------

/// What a block of the body is, for interaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BlockKind {
    Likes,
    Caption,
    ViewComments,
    Comment,
    Date,
}

/// One block of the body, laid out.
struct Block {
    kind: BlockKind,
    galley: Arc<Galley>,
    /// The CSS margins; vertical margins of neighbours collapse to the larger one.
    margin_top: f32,
    margin_bottom: f32,
    /// The text of the block, for screen readers and tests.
    text: String,
    links: Vec<SpanLink>,
    /// The `char` range of the "more" button of a collapsed caption.
    more: Option<std::ops::Range<usize>>,
    /// The `char` range of the space the verified badge is painted after.
    badge_after: Option<std::ops::Range<usize>>,
}

/// The laid-out body of a card.
struct Body {
    blocks: Vec<Block>,
    /// Height including the margins and the padding below.
    height: f32,
}

/// The heights and margins of a body's blocks, stacked as CSS stacks them: margins between two
/// blocks collapse to the larger, the first top margin and the last bottom margin stay.
fn stacked_height(blocks: impl Iterator<Item = (f32, f32, f32)>) -> f32 {
    let mut height = 0.0;
    let mut previous_bottom: Option<f32> = None;
    for (block_height, margin_top, margin_bottom) in blocks {
        height += match previous_bottom {
            Some(bottom) => bottom.max(margin_top),
            None => margin_top,
        };
        height += block_height;
        previous_bottom = Some(margin_bottom);
    }
    height + previous_bottom.unwrap_or(0.0) + BODY_PADDING_BOTTOM
}

/// Appends `text` to `job` in the type of the body, colouring hashtags and mentions as links,
/// and records them.
fn append_rich(job: &mut LayoutJob, text: &str, palette: &Palette, links: &mut Vec<SpanLink>) {
    widgets::append_rich(job, text, BODY_SIZE, BODY_LINE_HEIGHT, palette, links);
}

/// A job that wraps at the width of the body.
fn body_job() -> LayoutJob {
    LayoutJob { wrap: TextWrapping { max_width: BODY_WIDTH, ..TextWrapping::default() }, ..LayoutJob::default() }
}

/// The number shown in the likes line: the exported count, plus one while the user likes it.
pub(crate) fn shown_likes(post: &Post, post_ui: &PostUi) -> Option<u64> {
    post.like_count.map(|count| count.saturating_add(u64::from(post_ui.liked)))
}

/// The number in "View all N comments", shown only when the export carries comments to look at.
fn shown_comment_count(post: &Post) -> Option<u64> {
    if post.comments.is_empty() {
        None
    } else {
        Some(post.comment_count.max(post.comments.len() as u64))
    }
}

/// Lays out the body of a card.
fn build_body(ctx: &egui::Context, post: &Post, post_ui: &PostUi, palette: &Palette, now_ms: i64) -> Body {
    let mut blocks = Vec::new();
    let bold = |color: Color32| widgets::format(theme::semibold(BODY_SIZE), color, BODY_LINE_HEIGHT);
    let regular = |color: Color32| widgets::format(theme::regular(BODY_SIZE), color, BODY_LINE_HEIGHT);

    // Likes: a known count, or "Liked by you" once the user liked a post without one.
    let likes = match shown_likes(post, post_ui) {
        Some(count) => {
            let (number, rest) = strings::likes_line(count);
            Some((number, rest.to_owned(), true))
        }
        None if post_ui.liked => Some((strings::LIKED_BY.to_owned(), strings::LIKED_BY_YOU.to_owned(), false)),
        None => None,
    };
    if let Some((first, second, first_is_bold)) = likes {
        let mut job = body_job();
        let (first_format, second_format) = if first_is_bold { (bold(palette.text), regular(palette.text)) } else { (regular(palette.text), bold(palette.text)) };
        job.append(&first, 0.0, first_format);
        job.append(&second, 0.0, second_format);
        let text = job.text.clone();
        blocks.push(Block { kind: BlockKind::Likes, galley: widgets::layout_job(ctx, job), margin_top: 0.0, margin_bottom: 6.0, text, links: Vec::new(), more: None, badge_after: None });
    }

    // Caption: bold username, the verified badge, then the text, collapsed to its preview.
    if !text::js_trim(&post.caption).is_empty() {
        let preview = text::caption_preview(&post.caption);
        let collapsed = preview.truncated && !post_ui.expanded;
        let shown = if collapsed { preview.text } else { post.caption.as_str() };
        let mut job = body_job();
        let mut links = Vec::new();
        job.append(&post.user.username, 0.0, bold(palette.text));
        let username_chars = job.text.chars().count();
        job.append(" ", 0.0, regular(palette.text));
        let mut badge_after = None;
        if post.user.verified == Some(true) {
            // The badge is painted into the gap `leading_space` leaves after the first space.
            badge_after = Some(username_chars..username_chars + 1);
            job.append(" ", CAPTION_BADGE_SIZE, regular(palette.text));
        }
        append_rich(&mut job, shown, palette, &mut links);
        let mut more = None;
        if collapsed {
            job.append(strings::POST_CAPTION_ELLIPSIS, 0.0, regular(palette.text));
            let start = job.text.chars().count();
            job.append(strings::POST_CAPTION_MORE, 0.0, regular(palette.muted));
            more = Some(start..start + strings::POST_CAPTION_MORE.chars().count());
        }
        let text = job.text.clone();
        blocks.push(Block { kind: BlockKind::Caption, galley: widgets::layout_job(ctx, job), margin_top: 0.0, margin_bottom: 6.0, text, links, more, badge_after });
    }

    // "View all N comments" and the first exported comments.
    if let Some(count) = shown_comment_count(post) {
        let text = strings::view_comments(count);
        let galley = widgets::wrapped(ctx, &text, theme::regular(BODY_SIZE), palette.muted, BODY_LINE_HEIGHT, BODY_WIDTH);
        blocks.push(Block { kind: BlockKind::ViewComments, galley, margin_top: 0.0, margin_bottom: 6.0, text, links: Vec::new(), more: None, badge_after: None });
    }
    for comment in post.comments.iter().take(PREVIEW_COMMENTS) {
        let mut job = body_job();
        let mut links = Vec::new();
        job.append(&comment.username, 0.0, bold(palette.text));
        job.append(" ", 0.0, regular(palette.text));
        append_rich(&mut job, &comment.text, palette, &mut links);
        let text = job.text.clone();
        blocks.push(Block { kind: BlockKind::Comment, galley: widgets::layout_job(ctx, job), margin_top: 0.0, margin_bottom: 3.0, text, links, more: None, badge_after: None });
    }

    // The date line.
    if let Some(taken_at) = post.taken_at {
        let text = text::date_label(now_ms, taken_at, &text::local_offset_minutes);
        let mut format = widgets::format(theme::regular(DATE_SIZE), palette.muted, DATE_LINE_HEIGHT);
        format.extra_letter_spacing = 0.3;
        let mut job = LayoutJob::single_section(text.clone(), format);
        job.wrap = TextWrapping { max_width: BODY_WIDTH, ..TextWrapping::default() };
        blocks.push(Block { kind: BlockKind::Date, galley: widgets::layout_job(ctx, job), margin_top: 8.0, margin_bottom: 0.0, text, links: Vec::new(), more: None, badge_after: None });
    }

    let height = stacked_height(blocks.iter().map(|block| (block.galley.size().y, block.margin_top, block.margin_bottom)));
    Body { blocks, height }
}

/// The height of a body that has not been laid out yet: every block as one line, the caption
/// by its length. Close enough for the scrollbar; the real height replaces it when the card
/// comes near the view.
fn estimate_body_height(post: &Post, post_ui: &PostUi) -> f32 {
    /// Characters of 14 px Segoe UI that fit a line of the body, roughly.
    const CHARS_PER_LINE: usize = 62;
    let mut blocks: Vec<(f32, f32, f32)> = Vec::new();
    if post.like_count.is_some() || post_ui.liked {
        blocks.push((BODY_LINE_HEIGHT, 0.0, 6.0));
    }
    if !text::js_trim(&post.caption).is_empty() {
        let preview = text::caption_preview(&post.caption);
        let shown = if preview.truncated && !post_ui.expanded { preview.text } else { post.caption.as_str() };
        let lines: usize = format!("{} {shown}", post.user.username).split('\n').map(|line| line.chars().count() / CHARS_PER_LINE + 1).sum();
        blocks.push((lines as f32 * BODY_LINE_HEIGHT, 0.0, 6.0));
    }
    if shown_comment_count(post).is_some() {
        blocks.push((BODY_LINE_HEIGHT, 0.0, 6.0));
    }
    for _ in post.comments.iter().take(PREVIEW_COMMENTS) {
        blocks.push((BODY_LINE_HEIGHT, 0.0, 3.0));
    }
    if post.taken_at.is_some() {
        blocks.push((DATE_LINE_HEIGHT, 8.0, 0.0));
    }
    stacked_height(blocks.into_iter())
}

/// The height of a whole card from the height of its picture frame and of its body.
fn card_height(media_height: f32, body_height: f32) -> f32 {
    2.0 * CARD_BORDER + HEADER_HEIGHT + media_height + ACTIONS_HEIGHT + body_height
}

/// The file whose size decides the frame of a post: the cover, or its poster for a video (made
/// from its first frame when it has none). Never starts work.
fn cover_path(post: &Post, loader: &Loader) -> Option<PathBuf> {
    match post.cover().map(|media| media::peek_display_file(loader, media)) {
        Some(Display::File(path)) => Some(path),
        _ => None,
    }
}

// --- The view -----------------------------------------------------------------------------------

/// What the feed remembers about one post between frames.
#[derive(Debug, Clone, Copy)]
struct RowMemo {
    media_height: f32,
    body_height: f32,
}

/// The feed view: the virtualised list and its scroll position.
#[derive(Default)]
pub struct FeedView {
    /// The `collection_serial` the rows belong to.
    serial: Option<u64>,
    table: RowTable,
    rows: Vec<RowMemo>,
    /// Rows whose cover size is not known yet, by the file they wait for.
    waiting_by_cover: HashMap<PathBuf, Vec<usize>>,
    /// The loader's `size_revision` this view has caught up with.
    seen_size_revision: u64,
    /// What the top edge of the view is anchored to; None while the banner is in view.
    anchor: Option<ScrollAnchor>,
    /// The End key was pressed: stay at the end while the cards there learn their heights.
    pin_end: bool,
    last_offset: f32,
    /// Heights measured while painting; applied after the frame so that the positions used for
    /// painting and for the anchor are the same.
    measured: Vec<(usize, f32, f32)>,
    /// False while the layout is still changing (the screenshot harness waits for it).
    settled: bool,
    /// How many cards were painted in the last frame (for the log and the tests).
    painted_cards: usize,
    /// A scroll position asked for from outside the view (the harness), for the next frame.
    jump: Option<Jump>,
}

/// A scroll asked for from outside the view.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Jump {
    /// To this offset from the top.
    To(f32),
    /// By this many points, down when positive.
    By(f32),
}

impl FeedView {
    /// True when the last frame changed nothing in the layout.
    pub fn is_settled(&self) -> bool {
        self.settled
    }

    /// How many cards the last frame painted: a handful, however long the feed is.
    pub fn painted_cards(&self) -> usize {
        self.painted_cards
    }

    /// The scroll position of the last frame.
    pub fn scroll_offset(&self) -> f32 {
        self.last_offset
    }

    /// Scrolls to `offset` points from the top at the next frame.
    pub fn jump_to(&mut self, offset: f32) {
        self.jump = Some(Jump::To(offset));
    }

    /// Scrolls by `delta` points (down when positive) at the next frame.
    pub fn jump_by(&mut self, delta: f32) {
        self.jump = Some(Jump::By(delta));
    }

    /// Starts over for a new list of posts: one estimated height per post, and the size of
    /// every cover asked for in the background, in feed order.
    fn rebuild(&mut self, posts: &[Post], env: &mut Env<'_>) {
        self.rows.clear();
        self.waiting_by_cover.clear();
        self.seen_size_revision = env.loader.size_revision();
        self.measured.clear();
        self.table = RowTable::uniform(posts.len(), 0.0, CARD_GAP);
        for (index, post) in posts.iter().enumerate() {
            let post_ui = env.state.post_ui(&post.key);
            let display = post.cover().map(|cover| (cover.path.clone(), media::display_file(env.loader, cover, Priority::background(index))));
            let size = match display {
                Some((_, Display::File(path))) => match env.loader.size_of(&path, Priority::background(index)) {
                    SizeState::Known(size) => Some(size),
                    SizeState::Pending => {
                        self.waiting_by_cover.entry(path).or_default().push(index);
                        None
                    }
                    SizeState::Failed(_) => None,
                },
                // The poster of a video is being made: the row waits for the video.
                Some((video, Display::Pending)) => {
                    self.waiting_by_cover.entry(video).or_default().push(index);
                    None
                }
                Some((_, Display::Nothing)) | None => None,
            };
            let memo = RowMemo { media_height: layout::feed_frame_height(INNER_WIDTH, size), body_height: estimate_body_height(post, &post_ui) };
            self.table.set_height(index, card_height(memo.media_height, memo.body_height));
            self.rows.push(memo);
        }
        self.anchor = None;
        self.pin_end = false;
        self.serial = Some(env.state.collection_serial);
    }

    /// Gives row `index` the frame of its cover if the size is known now. Returns true when
    /// the row changed its height.
    fn adopt_size(&mut self, index: usize, posts: &[Post], loader: &Loader) -> bool {
        let Some(path) = posts.get(index).and_then(|post| cover_path(post, loader)) else {
            return false;
        };
        match loader.peek_size(&path) {
            SizeState::Known(size) => match self.rows.get_mut(index) {
                Some(memo) => {
                    memo.media_height = layout::feed_frame_height(INNER_WIDTH, Some(size));
                    self.table.set_height(index, card_height(memo.media_height, memo.body_height))
                }
                None => false,
            },
            SizeState::Pending | SizeState::Failed(_) => false,
        }
    }

    /// Takes over the sizes that arrived since the last frame. Returns true when a row changed.
    ///
    /// Normally only the rows of the files the loader reports for this frame are touched, so
    /// the cost does not grow with the length of the feed. When frames were missed (the feed
    /// was not shown for a while) every row that still waits is looked at once.
    fn adopt_sizes(&mut self, posts: &[Post], loader: &Loader) -> bool {
        let revision = loader.size_revision();
        if revision == self.seen_size_revision {
            return false;
        }
        let news = loader.size_news();
        let missed_nothing = self.seen_size_revision + news.len() as u64 == revision;
        self.seen_size_revision = revision;
        let mut changed = false;
        if missed_nothing {
            for path in news {
                for index in self.waiting_by_cover.remove(path).unwrap_or_default() {
                    changed |= self.adopt_size(index, posts, loader);
                }
            }
        } else {
            let waiting = std::mem::take(&mut self.waiting_by_cover);
            for (path, indices) in waiting {
                // A row may wait for a picture's size or for a video's poster.
                if loader.peek_size(&path) == SizeState::Pending && loader.peek_poster(&path) == crate::loader::PosterState::Pending {
                    self.waiting_by_cover.insert(path, indices);
                } else {
                    for index in indices {
                        changed |= self.adopt_size(index, posts, loader);
                    }
                }
            }
        }
        changed
    }

    /// Shows the feed in `main_rect` (the window right of the sidebar). `scroll_request` is
    /// where the state wants the feed to go in this frame.
    pub fn show(&mut self, ui: &mut Ui, main_rect: Rect, env: &mut Env<'_>, scroll_request: Option<ScrollRequest>) {
        let ctx = ui.ctx().clone();
        let state = env.state;
        let Some(collection) = state.collection.as_ref() else {
            return;
        };
        let posts = collection.posts.as_slice();
        if self.serial != Some(state.collection_serial) {
            self.rebuild(posts, env);
        }
        let mut changed = self.adopt_sizes(posts, env.loader);

        let now_ms = text::now_ms();
        let column_left = widgets::snap(&ctx, pos2(main_rect.center().x - FEED_WIDTH / 2.0, 0.0)).x;
        let banner = banner::plan(&ctx, env, collection, FEED_WIDTH);
        // Where the first card starts, measured from the top of the scrolled content.
        let list_origin = MAIN_PADDING_TOP + banner.height + CARD_GAP;
        let after_cards = if posts.is_empty() { 0.0 } else { CARD_GAP };
        let total_height = list_origin + self.table.total_height() + after_cards + FEED_END_HEIGHT + MAIN_PADDING_BOTTOM;
        let view_height = main_rect.height();
        let max_offset = (total_height - view_height).max(0.0);

        // Where to scroll: an explicit request, the keyboard, the End pin, or the anchor when
        // rows above the view changed their height.
        let mut force: Option<f32> = None;
        let mut key_delta = 0.0;
        match scroll_request {
            Some(ScrollRequest::Top) => {
                force = Some(0.0);
                self.pin_end = false;
            }
            Some(ScrollRequest::Post(key)) => {
                if let Some(index) = collection.index_of(&key) {
                    force = Some((list_origin + self.table.top(index) - CARD_GAP).max(0.0));
                    self.pin_end = false;
                }
            }
            None => {}
        }
        if let Some(jump) = self.jump.take() {
            let target = match jump {
                Jump::To(offset) => offset,
                Jump::By(delta) => self.last_offset + delta,
            };
            force = Some(target.clamp(0.0, max_offset));
            self.pin_end = false;
        }
        let keyboard_is_free = !state.overlay_open() && ctx.memory(|memory| memory.focused().is_none());
        if keyboard_is_free && force.is_none() {
            let pressed = |key: Key| ctx.input_mut(|input| input.consume_key(Modifiers::NONE, key));
            if pressed(Key::Home) {
                force = Some(0.0);
                self.pin_end = false;
            } else if pressed(Key::End) {
                self.pin_end = true;
            } else if pressed(Key::PageDown) {
                key_delta = -(view_height - PAGE_OVERLAP).max(ARROW_SCROLL);
            } else if pressed(Key::PageUp) {
                key_delta = (view_height - PAGE_OVERLAP).max(ARROW_SCROLL);
            } else if pressed(Key::ArrowDown) {
                key_delta = -ARROW_SCROLL;
            } else if pressed(Key::ArrowUp) {
                key_delta = ARROW_SCROLL;
            }
            if key_delta != 0.0 {
                self.pin_end = false;
            }
        }
        if force.is_none() {
            if self.pin_end {
                force = Some(max_offset);
            } else if let Some(anchor) = self.anchor {
                let wanted = (list_origin + anchor.resolve(&self.table)).clamp(0.0, max_offset);
                if (wanted - self.last_offset).abs() > 0.5 {
                    force = Some(wanted);
                }
            }
        }

        self.painted_cards = 0;
        let output = ui
            .scope_builder(UiBuilder::new().max_rect(main_rect), |ui| {
                let mut area = egui::ScrollArea::vertical().id_salt("feed").auto_shrink([false, false]);
                if let Some(offset) = force {
                    area = area.vertical_scroll_offset(offset);
                }
                area.show_viewport(ui, |ui, viewport| {
                    let content_top = ui.max_rect().top();
                    ui.allocate_rect(Rect::from_min_size(pos2(main_rect.left(), content_top), vec2(ui.available_width(), total_height)), Sense::hover());
                    if key_delta != 0.0 {
                        ui.scroll_with_delta(vec2(0.0, key_delta));
                    }

                    if viewport.top() < MAIN_PADDING_TOP + banner.height {
                        banner::show(ui, pos2(column_left, content_top + MAIN_PADDING_TOP), FEED_WIDTH, &banner, env);
                    }

                    let (list_top, list_bottom) = (viewport.top() - list_origin, viewport.bottom() - list_origin);
                    let range = self.table.visible_rows(list_top, list_bottom, OVERSCAN);
                    let first_on_screen = self.table.row_at(list_top.max(0.0)).unwrap_or(0);
                    for index in range {
                        let Some(post) = posts.get(index) else {
                            break;
                        };
                        let top = self.table.top(index);
                        let on_screen = top < list_bottom && top + self.table.height(index) > list_top;
                        let card_min = pos2(column_left, content_top + list_origin + top);
                        let rank = index.abs_diff(first_on_screen);
                        let (media_height, body_height) = show_card(ui, card_min, post, on_screen, rank, env, now_ms);
                        if on_screen {
                            self.painted_cards += 1;
                        }
                        if (card_height(media_height, body_height) - self.table.height(index)).abs() > 0.01 {
                            self.measured.push((index, media_height, body_height));
                        }
                    }

                    let end_top = list_origin + self.table.total_height() + after_cards;
                    if end_top < viewport.bottom() && end_top + FEED_END_HEIGHT > viewport.top() {
                        show_feed_end(ui, pos2(column_left, content_top + end_top), collection.is_export(), state.palette());
                    }
                })
            })
            .inner;

        let offset = output.state.clamped_offset().y;
        // A wheel turn or a drag of the scrollbar while pinned to the end releases the pin.
        if self.pin_end && force.is_some_and(|forced| (offset - forced.min(max_offset)).abs() > 1.0) {
            self.pin_end = false;
        }
        // While the banner is in view the feed is anchored to its top, so that the banner
        // growing (the warnings list) pushes the cards down instead of scrolling the view.
        self.anchor = if offset > list_origin - CARD_GAP { ScrollAnchor::capture(&self.table, offset - list_origin) } else { None };
        let moved = (offset - self.last_offset).abs() > 0.5;
        self.last_offset = offset;

        for (index, media_height, body_height) in std::mem::take(&mut self.measured) {
            if let Some(memo) = self.rows.get_mut(index) {
                *memo = RowMemo { media_height, body_height };
                changed |= self.table.set_height(index, card_height(media_height, body_height));
            }
        }
        self.settled = !changed && !moved && key_delta == 0.0;
        if changed {
            ctx.request_repaint();
        }
    }
}

// --- One card -----------------------------------------------------------------------------------

/// Lays out one card and, when it is on screen, paints it with its top-left corner at `min`.
/// A card outside the view is only measured and its pictures are asked for ahead of time.
/// Returns the height of its picture frame and of its body.
fn show_card(ui: &Ui, min: Pos2, post: &Post, on_screen: bool, rank: usize, env: &mut Env<'_>, now_ms: i64) -> (f32, f32) {
    let ctx = ui.ctx().clone();
    let state = env.state;
    let palette = state.palette();
    let post_ui = state.post_ui(&post.key);
    let cover_size = cover_path(post, env.loader).and_then(|path| match env.loader.peek_size(&path) {
        SizeState::Known(size) => Some(size),
        _ => None,
    });
    let media_height = layout::feed_frame_height(INNER_WIDTH, cover_size);
    let body = build_body(&ctx, post, &post_ui, palette, now_ms);
    let frame = Rect::from_min_size(min + vec2(CARD_BORDER, CARD_BORDER + HEADER_HEIGHT), vec2(INNER_WIDTH, media_height));

    if !on_screen {
        // Decode ahead: the cover (and the avatar) of a card that is about to scroll in.
        let slide = post_ui.slide.min(post.media.len().saturating_sub(1));
        let _ = media::cover_picture(env.loader, &ctx, post, slide, frame.size(), Priority::near(rank * 4));
        if let Some(avatar) = &post.user.avatar {
            let _ = env.loader.image(avatar, avatar_variant(&ctx, HEADER_AVATAR_SIZE), Priority::near(rank * 4 + 3));
        }
        return (media_height, body.height);
    }

    let card_id = Id::unique(("feed-card", post.key.as_str()));
    let rect = Rect::from_min_size(min, vec2(FEED_WIDTH, card_height(media_height, body.height)));
    let painter = ui.painter();
    painter.rect_filled(rect, CARD_RADIUS, palette.surface);
    painter.rect_stroke(rect, CARD_RADIUS, Stroke::new(CARD_BORDER, palette.border), StrokeKind::Inside);

    let inner_left = min.x + CARD_BORDER;
    show_header(ui, pos2(inner_left, min.y + CARD_BORDER), post, card_id, rank, env, now_ms);
    show_media(ui, frame, post, &post_ui, card_id, rank, env);
    show_actions(ui, pos2(inner_left, frame.bottom()), post, &post_ui, card_id, env);
    show_body(ui, pos2(inner_left + PADDING_X, frame.bottom() + ACTIONS_HEIGHT), &body, post, card_id, env);
    (media_height, body.height)
}

/// The rendition an avatar of `size` points is decoded to: its picture (the size without the
/// 2 px border) at the screen's pixel density.
pub(crate) fn avatar_variant(ctx: &egui::Context, size: f32) -> Variant {
    let side = (size - 2.0 * widgets::AVATAR_BORDER_WIDTH) * ctx.pixels_per_point();
    Variant::Cover { min_side_px: side.ceil().max(1.0) as u32 }
}

/// The picture of the author of `post` for an avatar of `size` points, once it is decoded.
pub(crate) fn avatar_picture(loader: &mut Loader, ctx: &egui::Context, post: &Post, size: f32, priority: Priority) -> Option<crate::loader::Picture> {
    let path = post.user.avatar.as_ref()?;
    match loader.image(path, avatar_variant(ctx, size), priority) {
        ImageState::Ready(picture) => Some(picture),
        _ => None,
    }
}

/// The header: ringed avatar, username, verified badge, time, the line below, and the buttons.
fn show_header(ui: &Ui, min: Pos2, post: &Post, card_id: Id, rank: usize, env: &mut Env<'_>, now_ms: i64) {
    let ctx = ui.ctx().clone();
    let palette = env.palette();
    let painter = ui.painter();
    let center_y = min.y + HEADER_HEIGHT / 2.0;

    // Avatar: the exported picture when there is one, else the placeholder with the initial.
    let avatar_center = pos2(min.x + PADDING_X + widgets::avatar_outer_size(HEADER_AVATAR_SIZE, true) / 2.0, center_y);
    let avatar_picture = avatar_picture(env.loader, &ctx, post, HEADER_AVATAR_SIZE, Priority::visible(rank * 4 + 3));
    widgets::avatar(painter, avatar_center, HEADER_AVATAR_SIZE, true, &post.user.username, avatar_picture.as_ref(), palette.surface);

    // Buttons on the right: "More options", and the link icon of a post with a link. Their
    // places are taken first, but they are made after the text: Tab follows the order widgets
    // are made in, and the clone's header runs left to right (Feed.tsx:226-247).
    let mut right = min.x + INNER_WIDTH - PADDING_X;
    let more_center = pos2(right - 12.0, center_y);
    right -= 24.0 + HEADER_GAP;
    let site = post.permalink.as_deref().map(urls::site_label);
    let external_center = post.permalink.as_ref().and(site.as_ref()).map(|_| pos2(right - 10.0, center_y));
    if external_center.is_some() {
        right -= 20.0 + HEADER_GAP;
    }

    // The two lines of text between avatar and buttons, centred as a block.
    let meta_left = min.x + PADDING_X + widgets::avatar_outer_size(HEADER_AVATAR_SIZE, true) + HEADER_GAP;
    let meta_width = (right - meta_left).max(0.0);
    let author_height = theme::normal_line_height(AUTHOR_SIZE);
    let subline_height = theme::normal_line_height(SUBLINE_SIZE);
    let subline = post.header_subline().filter(|line| !line.is_empty());
    let block_height = author_height + if subline.is_some() { subline_height } else { 0.0 };
    let author_center_y = center_y - block_height / 2.0 + author_height / 2.0;

    let verified = post.user.verified == Some(true);
    let time_label = post.taken_at.map(|taken_at| text::relative_time_label(now_ms, taken_at));
    let separator = widgets::line(&ctx, strings::POST_TIME_SEPARATOR, theme::regular(AUTHOR_SIZE), palette.muted, author_height);
    let time = time_label.as_deref().map(|label| widgets::line(&ctx, label, theme::regular(AUTHOR_SIZE), palette.muted, author_height));
    let mut reserved = 0.0;
    if verified {
        reserved += AUTHOR_GAP + HEADER_BADGE_SIZE;
    }
    if let Some(time) = &time {
        reserved += AUTHOR_GAP + separator.size().x + AUTHOR_GAP + time.size().x;
    }
    let username = widgets::line_elided(&ctx, &post.user.username, theme::semibold(AUTHOR_SIZE), palette.text, author_height, (meta_width - reserved).max(24.0));
    let username_rect = widgets::paint_line(painter, meta_left, author_center_y, username);
    widgets::text_label(ui, username_rect, card_id.with("username"), &post.user.username);
    let mut x = username_rect.right();
    if verified {
        x += AUTHOR_GAP;
        let badge_center = pos2(x + HEADER_BADGE_SIZE / 2.0, author_center_y + 1.0);
        icons::paint(painter, Icon::Verified, badge_center, IconStyle::filled(HEADER_BADGE_SIZE), Color32::WHITE);
        widgets::text_label(ui, Rect::from_center_size(badge_center, Vec2::splat(HEADER_BADGE_SIZE)), card_id.with("verified"), strings::POST_VERIFIED);
        x += HEADER_BADGE_SIZE;
    }
    if let (Some(time), Some(label)) = (time, &time_label) {
        x += AUTHOR_GAP;
        let separator_width = separator.size().x;
        widgets::paint_line(painter, x, author_center_y, separator);
        x += separator_width + AUTHOR_GAP;
        let time_rect = Rect::from_min_size(pos2(x, author_center_y - author_height / 2.0), time.size());
        match (&post.permalink, &site) {
            (Some(link), Some(site)) => {
                // With a link the time is a link: underlined under the pointer.
                let response = widgets::hit(ui, time_rect, card_id.with("time"), label).on_hover_text(strings::open_post_on(site));
                widgets::paint_line(painter, x, author_center_y, time);
                if response.hovered() {
                    let underline_y = (time_rect.bottom() - 3.0).round() + 0.5;
                    painter.hline(time_rect.x_range(), underline_y, Stroke::new(1.0, palette.muted));
                }
                widgets::focus_ring(ui, &response, time_rect, 2.0, theme::FOCUS_RING_OFFSET);
                if response.clicked() {
                    env.push(Action::OpenLink(link.clone()));
                }
            }
            _ => {
                widgets::paint_line(painter, x, author_center_y, time);
                widgets::text_label(ui, time_rect, card_id.with("time"), label);
            }
        }
    }
    if let Some(subline) = subline {
        let galley = widgets::line_elided(&ctx, subline, theme::regular(SUBLINE_SIZE), palette.muted, subline_height, meta_width);
        let subline_center_y = author_center_y + author_height / 2.0 + subline_height / 2.0;
        let subline_rect = widgets::paint_line(painter, meta_left, subline_center_y, galley);
        widgets::text_label(ui, subline_rect, card_id.with("subline"), subline);
    }

    if let (Some(center), Some(link), Some(site)) = (external_center, &post.permalink, &site) {
        let label = strings::view_on(site);
        if widgets::icon_button(ui, center, card_id.with("external"), Icon::External, IconStyle::outline(20.0), palette.text, &label).clicked() {
            env.push(Action::OpenLink(link.clone()));
        }
    }
    if widgets::icon_button(ui, more_center, card_id.with("more-options"), Icon::Ellipsis, IconStyle::filled(24.0), palette.text, strings::POST_MORE_OPTIONS).clicked() {
        env.push(Action::OpenPostOptions(post.key.clone()));
    }
}

/// The picture frame of a card: the slides, the carousel chrome, the video badge, the like heart.
fn show_media(ui: &Ui, frame: Rect, post: &Post, post_ui: &PostUi, card_id: Id, rank: usize, env: &mut Env<'_>) {
    let ctx = ui.ctx().clone();
    let state = env.state;
    let palette = state.palette();
    let slide_count = post.media.len().max(1);
    let current = post_ui.slide.min(slide_count - 1);
    let has_dots = slide_count > 1;

    // The slides, side by side on a track that moves (`transform: translateX(-current * 100%)`).
    let clipped = ui.painter().with_clip_rect(frame.intersect(ui.clip_rect()));
    clipped.rect_filled(frame, 0.0, theme::MEDIA_BLACK);
    let position = post_ui.track_position(state.time, state.animations);
    let mut current_failed: Option<String> = None;
    for slide in 0..slide_count {
        let offset = (slide as f32 - position) * frame.width();
        if offset.abs() >= frame.width() {
            // Off the frame. The slide after the current one is decoded ahead (Media.tsx:74-81).
            if slide == current + 1 {
                let _ = media::cover_picture(env.loader, &ctx, post, slide, frame.size(), Priority::near(rank * 4 + 1));
            }
            continue;
        }
        let slide_state = media::cover_picture(env.loader, &ctx, post, slide, frame.size(), Priority::visible(rank * 4));
        if slide == current {
            current_failed = slide_state.failure().map(str::to_owned);
        }
        let rect = frame.translate(vec2(widgets::snap(&ctx, pos2(offset, 0.0)).x, 0.0));
        media::paint_cover_slide(&clipped, rect, post, &slide_state, PlaceholderLook::feed(has_dots), palette);
    }
    let sliding = post_ui.is_sliding(state.time, state.animations);
    if sliding {
        ctx.request_repaint();
    }

    // A video on the slide on show: claimed with the share of the frame on screen; its frames
    // over the poster once they come (video brief 5.3).
    let video = video_slide(ui, frame, post, current, sliding, env);
    if let Some(VideoCard { texture: Some((texture, size)), .. }) = &video {
        controls::paint_cover(&clipped, frame, *texture, *size);
    }

    // The frame itself: named after the picture; a double click likes the post.
    let label = media::slide_label(post, current, current_failed.as_deref());
    let response = widgets::picture_area(ui, frame, card_id.with("media"), &label);
    if response.double_clicked() {
        env.push(Action::LikeByDoubleClick(post.key.clone()));
    }

    if post.is_video() {
        media::video_badge(ui, frame, card_id.with("video-badge"));
    }
    if let Some(video) = &video {
        let caps = env.video.caps();
        if let Some(sentence) = &video.failure {
            if controls::failure_panel(ui, frame, card_id.with("video-failed"), sentence) {
                env.push(Action::ShowInFolder(post.key.clone()));
            }
        } else if caps.autoplays_feed() {
            // The clone: a click on the playing video, or the button, switches the sound of the
            // feed; the two clicks of a double click switch it twice.
            let sound_on = env.state.video_sound_on;
            let clicked_video = video.playing && response.clicked();
            if controls::sound_button(ui, frame, card_id.with("video-sound"), sound_on) || clicked_video {
                env.push(Action::ToggleVideoSound);
            }
        } else if controls::play_badge(ui, frame.center(), card_id.with("video-play")) || (response.clicked() && !response.double_clicked()) {
            // No autoplay on a software renderer: the viewer plays it.
            env.push(Action::PlayInViewer(post.key.clone(), current));
        }
    }
    let chrome = CarouselChrome { frame, current, count: slide_count, arrow_inset: media::CAROUSEL_ARROW_INSET, hide_dots: false };
    if let Some(slide) = media::carousel_chrome(ui, card_id, chrome) {
        env.push(Action::ShowSlide(post.key.clone(), slide));
    }
    let painter = ui.painter();

    // The like heart (`heartpop`): white, 90 px, over the middle of the picture.
    if let Some(started) = post_ui.burst_at {
        let fraction = match state.burst_freeze {
            Some(frozen) => Some(frozen),
            None if state.animations => Some(((state.time - started) / BURST_SECONDS) as f32).filter(|fraction| *fraction < 1.0),
            None => None,
        };
        if let Some(fraction) = fraction {
            let (scale, opacity) = layout::heart_burst(fraction);
            icons::paint(painter, Icon::Heart, frame.center(), IconStyle::filled(90.0 * scale).with_shadow(0.55), theme::with_opacity(Color32::WHITE, opacity));
            if state.burst_freeze.is_none() {
                ctx.request_repaint();
            }
        }
    }
}

/// What a video card shows this frame.
struct VideoCard {
    /// The newest frame, while this card's video plays (or paused where it was).
    texture: Option<(egui::TextureId, [usize; 2])>,
    /// It plays now.
    playing: bool,
    /// Why it cannot be played.
    failure: Option<String>,
}

/// Claims the video of slide `slide` of `post` for the feed and returns what to show; `None`
/// when the slide is no video file.
fn video_slide(ui: &Ui, frame: Rect, post: &Post, slide: usize, sliding: bool, env: &mut Env<'_>) -> Option<VideoCard> {
    let media = post.media.get(slide).filter(|media| media.kind == instagram_core::model::MediaKind::Video)?;
    let key = VideoKey::new(&post.key, slide);
    let name = post.media_file_name(slide).unwrap_or_default();
    // Only the slide on show competes, and not while the carousel moves.
    let view = ui.clip_rect();
    let ratio = if sliding { 0.0 } else { video_rules::visible_ratio(frame.top(), frame.bottom(), view.top(), view.bottom()) };
    let scale = ui.ctx().pixels_per_point();
    let rect_px = ((frame.width() * scale).round() as u32, (frame.height() * scale).round() as u32);
    env.video.claim(Surface::Feed, key.clone(), &media.path, &name, ratio, rect_px);
    let shown = env.video.shown(Surface::Feed, &key);
    let failure = match &shown {
        Some(shown) => shown.failure(&name),
        None => env.video.feed_failure(&key),
    };
    Some(VideoCard {
        texture: shown.as_ref().and_then(|shown| shown.texture),
        playing: shown.as_ref().is_some_and(|shown| shown.status.phase == crate::video::Phase::Playing),
        failure,
    })
}

/// The action row: like, comment and share on the left, save on the right.
fn show_actions(ui: &Ui, min: Pos2, post: &Post, post_ui: &PostUi, card_id: Id, env: &mut Env<'_>) {
    let palette = env.palette();
    let center_y = min.y + 8.0 + ACTION_ICON_SIZE / 2.0;
    let first_x = min.x + PADDING_X + ACTION_ICON_SIZE / 2.0;
    let step = ACTION_ICON_SIZE + ACTION_ICON_GAP;
    let key = &post.key;

    let (like_color, like_label) = if post_ui.liked { (theme::LIKE_RED, strings::POST_UNLIKE) } else { (palette.text, strings::POST_LIKE) };
    if widgets::icon_button(ui, pos2(first_x, center_y), card_id.with("like"), Icon::Heart, IconStyle::toggled(ACTION_ICON_SIZE, post_ui.liked), like_color, like_label).clicked() {
        env.push(Action::ToggleLike(key.clone()));
    }
    if widgets::icon_button(ui, pos2(first_x + step, center_y), card_id.with("comment"), Icon::Comment, IconStyle::outline(ACTION_ICON_SIZE), palette.text, strings::POST_COMMENT).clicked() {
        env.push(Action::OpenViewer(key.clone()));
    }
    if widgets::icon_button(ui, pos2(first_x + 2.0 * step, center_y), card_id.with("share"), Icon::Share, IconStyle::outline(ACTION_ICON_SIZE), palette.text, strings::POST_SHARE).clicked() {
        env.push(Action::Share(key.clone()));
    }
    let save_x = min.x + INNER_WIDTH - PADDING_X - ACTION_ICON_SIZE / 2.0;
    let save_label = if post_ui.saved { strings::POST_UNSAVE } else { strings::POST_SAVE };
    if widgets::icon_button(ui, pos2(save_x, center_y), card_id.with("save"), Icon::Bookmark, IconStyle::toggled(ACTION_ICON_SIZE, post_ui.saved), palette.text, save_label).clicked() {
        env.push(Action::ToggleSave(key.clone()));
    }
}

/// The body: likes, caption, comments, date, with their links and the "more" button.
fn show_body(ui: &Ui, min: Pos2, body: &Body, post: &Post, card_id: Id, env: &mut Env<'_>) {
    let palette = env.palette();
    let painter = ui.painter();
    let mut y = min.y;
    let mut previous_bottom: Option<f32> = None;
    for (index, block) in body.blocks.iter().enumerate() {
        y += match previous_bottom {
            Some(bottom) => bottom.max(block.margin_top),
            None => block.margin_top,
        };
        let pos = widgets::snap(ui.ctx(), pos2(min.x, y));
        let block_rect = Rect::from_min_size(pos, block.galley.size());
        let block_id = card_id.with(("body", index));
        widgets::paint_galley(painter, pos, block.galley.clone());

        match block.kind {
            BlockKind::ViewComments => {
                let response = widgets::hit(ui, block_rect, block_id, &block.text);
                widgets::focus_ring(ui, &response, block_rect, 2.0, theme::FOCUS_RING_OFFSET);
                if response.clicked() {
                    env.push(Action::OpenViewer(post.key.clone()));
                }
            }
            BlockKind::Likes | BlockKind::Caption | BlockKind::Comment | BlockKind::Date => {
                widgets::text_label(ui, block_rect, block_id, &block.text);
            }
        }

        if let Some(range) = &block.badge_after {
            if let Some(space) = widgets::char_range_rects(&block.galley, range.clone()).first() {
                let center = pos + vec2(space.right() + CAPTION_BADGE_SIZE / 2.0, space.center().y + 1.0);
                icons::paint(painter, Icon::Verified, center, IconStyle::filled(CAPTION_BADGE_SIZE), Color32::WHITE);
            }
        }
        if let Some(url) = widgets::link_areas(ui, &block.galley, pos, &block.links, block_id, palette) {
            env.push(Action::OpenLink(url));
        }
        if let Some(range) = &block.more {
            for (part, rect) in widgets::char_range_rects(&block.galley, range.clone()).into_iter().enumerate() {
                let rect = rect.translate(pos.to_vec2());
                let response = widgets::hit(ui, rect, block_id.with(("more", part)), strings::POST_CAPTION_MORE);
                widgets::focus_ring(ui, &response, rect, 2.0, 0.0);
                if response.clicked() {
                    env.push(Action::ExpandCaption(post.key.clone()));
                }
            }
        }

        y += block.galley.size().y;
        previous_bottom = Some(block.margin_bottom);
    }
}

/// The block after the last card (`.feed__end`): a compass, "You're all caught up", a hint.
fn show_feed_end(ui: &Ui, min: Pos2, is_export: bool, palette: &Palette) {
    let ctx = ui.ctx().clone();
    let painter = ui.painter();
    let center_x = min.x + FEED_WIDTH / 2.0;
    let mut y = min.y + 40.0;
    icons::paint(painter, Icon::Compass, pos2(center_x, y + 20.0), IconStyle::outline(40.0), palette.muted);
    y += 40.0 + 8.0;
    let title = widgets::line(&ctx, strings::FEED_END_TITLE, theme::semibold(16.0), palette.text, 21.0);
    let title_rect = Rect::from_min_size(widgets::snap(&ctx, pos2(center_x - title.size().x / 2.0, y)), title.size());
    widgets::paint_galley(painter, title_rect.min, title);
    widgets::text_label(ui, title_rect, Id::unique("feed-end-title"), strings::FEED_END_TITLE);
    y += 21.0 + 8.0;
    let hint_text = if is_export { strings::FEED_END_POSTS } else { strings::FEED_END_PICTURES };
    let hint = widgets::line(&ctx, hint_text, theme::regular(13.0), palette.muted, 17.0);
    let hint_rect = Rect::from_min_size(widgets::snap(&ctx, pos2(center_x - hint.size().x / 2.0, y)), hint.size());
    widgets::paint_galley(painter, hint_rect.min, hint);
    widgets::text_label(ui, hint_rect, Id::unique("feed-end-hint"), hint_text);
}

/// The key of the post whose card is at the top of the view, for tests and the log.
pub fn first_visible_post(view: &FeedView, posts: &[Post]) -> Option<PostKey> {
    view.anchor.and_then(|anchor| posts.get(anchor.row)).map(|post| post.key.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn margins_between_blocks_collapse_like_css() {
        // The first card of the reference: likes, a three-line caption, "View all", two
        // comments, the date: 203 px (measured in the web project).
        let blocks = [(21.0, 0.0, 6.0), (63.0, 0.0, 6.0), (21.0, 0.0, 6.0), (21.0, 0.0, 3.0), (21.0, 0.0, 3.0), (15.0, 8.0, 0.0)];
        assert_eq!(stacked_height(blocks.into_iter()), 203.0);
        // Only a date: its top margin, the line, the padding.
        assert_eq!(stacked_height([(15.0, 8.0, 0.0)].into_iter()), 35.0);
        // Nothing at all: the padding.
        assert_eq!(stacked_height(std::iter::empty()), 12.0);
        // A caption alone keeps its bottom margin inside the padding.
        assert_eq!(stacked_height([(21.0, 0.0, 6.0)].into_iter()), 39.0);
    }

    #[test]
    fn the_reference_card_adds_up() {
        // Card 1 of the reference is 812 px with the comment row (41 px) this app leaves out.
        assert_eq!(card_height(468.0, 203.0), 812.0 - 41.0);
    }

    #[test]
    fn the_feed_end_block_has_the_height_of_its_parts() {
        assert_eq!(FEED_END_HEIGHT, 174.0);
    }
}
