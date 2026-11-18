//! ABC scores drawn by Nulla's engraver (`nulla_render`): in chat messages
//! (`show_score`) and in the score pane (`score_editor`).
//!
//! Pictures are drawn on worker threads, so a chat full of scores stays
//! responsive, and an engraver panic on odd input costs one picture, not the
//! app. A block is drawn only once it scrolls into view; until then it holds
//! a placeholder of about the right height. When the text, the column width
//! or the display scale changes, the old picture stays (scaled to fit) until
//! the new one is ready. Pictures are white and tinted with the text colour,
//! like math, so a theme change needs no new picture.
//!
//! Hovering a mark of the score outlines it and shows its ABC text. Clicking
//! it selects that text: in the score pane directly, from a chat by opening
//! the score pane. The right-click menu copies or saves the score.

use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use egui::RichText;
use egui::text::{CCursor, CCursorRange};
use nulla_render::abc::datatypes::Span;
use nulla_render::engrave::{self, Engraving, Hit};
use nulla_render::{FontSource, Fonts, Score};

/// Screen points per staff space. Lyrics (1.6 spaces) come out a little
/// smaller than body text.
const POINTS_PER_SPACE: f32 = 8.0;

/// The narrowest page that is engraved, in staff spaces. A narrower column
/// shows it scaled down.
const MIN_WIDTH: f32 = 40.0;

/// How far the column may change width, in staff spaces, before the page is
/// engraved again. In between, the picture is scaled to fit.
const WIDTH_SLACK: f32 = 4.0;

/// How many pictures are drawn at the same time, across all chats and panes.
const MAX_JOBS: usize = 2;

/// Resolution of copied and saved pictures, in pixels per staff space.
const EXPORT_PIXELS_PER_SPACE: f32 = 40.0;

/// Pictures being drawn right now.
static RUNNING: AtomicUsize = AtomicUsize::new(0);

/// The fonts scores are drawn with, built into the binary and loaded on
/// first use: Bravura for the music, Liberation Serif (Times metrics, with
/// Cyrillic) for titles and lyrics. Both are under the SIL OFL.
fn fonts() -> &'static Fonts {
    static FONTS: OnceLock<Fonts> = OnceLock::new();
    FONTS.get_or_init(|| {
        let files: [&[u8]; 4] = [
            include_bytes!("../assets/fonts/Bravura.otf"),
            include_bytes!("../assets/fonts/LiberationSerif-Regular.ttf"),
            include_bytes!("../assets/fonts/LiberationSerif-Italic.ttf"),
            include_bytes!("../assets/fonts/LiberationSerif-Bold.ttf"),
        ];
        Fonts::new(files.map(|bytes| FontSource::Data(bytes.to_vec())))
    })
}

//------------------------------------------------------------------------------
// what callers get

/// What the user asked for on a score.
#[derive(Default)]
pub struct ScoreResponse {
    /// A mark of the score was clicked: its text in the ABC.
    pub clicked: Option<Span>,
    /// "Open in score pane" was chosen: true for the pane on the right.
    pub open_pane: Option<bool>,
    /// "Save as …" was chosen: the file's bytes, for a save dialog.
    pub save: Option<Vec<u8>>,
}

/// Draws the ABC text of a chat code block as a score, with a folded list of
/// what could not be read or engraved below it. `id` names the block.
pub fn show_score(ui: &mut egui::Ui, id: egui::Id, code: &str) -> ScoreResponse {
    score_view(ui, id, code, None, true)
}

/// The text editor of the score pane in `tile_id`.
pub fn editor_id(tile_id: egui_tiles::TileId) -> egui::Id {
    egui::Id::unique(("score_pane_editor", tile_id))
}

/// The score pane: the ABC text on the left, its score on the right.
/// Selecting text highlights its marks; clicking a mark selects its text.
pub fn score_editor(ui: &mut egui::Ui, tile_id: egui_tiles::TileId, source: &mut String) -> ScoreResponse {
    let editor_id = editor_id(tile_id);
    let selection = editor_selection(ui.ctx(), editor_id, source);
    let mut out = ScoreResponse::default();
    ui.columns(2, |columns| {
        egui::ScrollArea::vertical()
            .id_salt("score_source")
            .show(&mut columns[0], |ui| {
                ui.add(
                    egui::TextEdit::multiline(source)
                        .id(editor_id)
                        .code_editor()
                        .desired_width(f32::INFINITY),
                );
            });
        egui::ScrollArea::vertical()
            .id_salt("score_picture")
            .show(&mut columns[1], |ui| {
                out = score_view(ui, egui::Id::unique(("score_pane", tile_id)), source, selection, false);
            });
    });
    if let Some(span) = out.clicked.take() {
        select_in_editor(ui.ctx(), editor_id, source, span);
    }
    out
}

/// Selects `span` of `source` in the text editor `editor_id` and focuses it.
/// Works before the editor is first shown: it picks the state up then.
pub fn select_in_editor(ctx: &egui::Context, editor_id: egui::Id, source: &str, span: Span) {
    let mut state = egui::TextEdit::load_state(ctx, editor_id).unwrap_or_default();
    let a = char_index(source, span.start);
    let b = char_index(source, span.end);
    state
        .cursor
        .set_char_range(Some(CCursorRange::two(CCursor::new(a), CCursor::new(b))));
    state.store(ctx, editor_id);
    ctx.memory_mut(|m| m.request_focus(editor_id));
}

/// The byte range selected in the text editor `editor_id`, if it has a cursor.
fn editor_selection(ctx: &egui::Context, editor_id: egui::Id, source: &str) -> Option<Span> {
    let state = egui::TextEdit::load_state(ctx, editor_id)?;
    let range = state.cursor.char_range()?;
    let a: usize = range.primary.index.into();
    let b: usize = range.secondary.index.into();
    Some(Span::new(byte_index(source, a.min(b)), byte_index(source, a.max(b))))
}

/// Character index of byte offset `byte` in `s`.
fn char_index(s: &str, byte: usize) -> usize {
    s.get(..byte).map_or_else(|| s.chars().count(), |before| before.chars().count())
}

/// Byte offset of character index `index` in `s`.
fn byte_index(s: &str, index: usize) -> usize {
    s.char_indices().nth(index).map_or(s.len(), |(b, _)| b)
}

//------------------------------------------------------------------------------
// pictures

/// A finished picture and what it was drawn from.
struct Drawn {
    code_hash: u64,
    /// Page width in staff spaces.
    width: f32,
    pixels_per_point: f32,
    texture: Option<egui::TextureHandle>,
    /// Spans point into the ABC text with `prefix_len` bytes in front.
    engraving: Engraving,
    /// Bytes put in front of the ABC text before parsing (an `X:` line).
    prefix_len: usize,
    /// (line in the ABC text, message)
    diagnostics: Vec<(usize, String)>,
}

impl Drawn {
    /// The picture still shows this text at this width and scale.
    fn fits(&self, code_hash: u64, width: f32, ppp: f32) -> bool {
        self.code_hash == code_hash
            && self.pixels_per_point == ppp
            && (self.width - width).abs() <= WIDTH_SLACK
    }

    /// Size in staff spaces.
    fn size(&self) -> egui::Vec2 {
        egui::vec2(self.engraving.width, self.engraving.height)
    }

    /// The text a mark came from, as a range of the ABC text.
    fn span(&self, hit: &Hit) -> Span {
        Span::new(
            hit.span.start.saturating_sub(self.prefix_len),
            hit.span.end.saturating_sub(self.prefix_len),
        )
    }
}

/// A picture being drawn on a worker thread.
enum Job {
    Running,
    Done(Drawn),
    /// The worker panicked on the text with this hash.
    Failed(u64),
}

type JobSlot = Arc<Mutex<Job>>;

/// What a score keeps between frames, in egui's temp memory.
#[derive(Clone, Default)]
struct BlockState {
    /// The latest finished picture.
    shown: Option<Arc<Drawn>>,
    /// The picture being drawn, if any. One at a time per score.
    job: Option<JobSlot>,
    /// The text the engraver failed on; not tried again until it changes.
    failed: Option<u64>,
}

/// A score with its placeholder, problems and interaction. `selection` is
/// the text to highlight; `in_chat` adds the "open in score pane" items.
fn score_view(
    ui: &mut egui::Ui,
    id: egui::Id,
    code: &str,
    selection: Option<Span>,
    in_chat: bool,
) -> ScoreResponse {
    let mut out = ScoreResponse::default();
    let ppp = ui.ctx().pixels_per_point();
    let available = ui.available_width();
    let available = if available.is_finite() { available } else { 800.0 };
    let width = (available / POINTS_PER_SPACE).max(MIN_WIDTH);
    let code_hash = hash(code);

    let mut state = ui.data(|d| d.get_temp::<BlockState>(id)).unwrap_or_default();
    collect(&mut state);
    if state.failed.is_some_and(|h| h != code_hash) {
        state.failed = None;
    }
    let warn = ui.visuals().warn_fg_color;

    if state.shown.is_some() || state.failed.is_none() {
        // the picture, or a placeholder of about the right size
        let size = match &state.shown {
            Some(drawn) => drawn.size() * POINTS_PER_SPACE,
            None => egui::vec2(available, estimated_height(code) * POINTS_PER_SPACE),
        };
        let size = if size.x > available { size * (available / size.x) } else { size };
        let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());

        let current = state.shown.as_ref().is_some_and(|d| d.fits(code_hash, width, ppp));
        if !current && state.job.is_none() && state.failed.is_none() && ui.is_rect_visible(rect) {
            state.job = start(ui.ctx(), code, code_hash, width, ppp);
        }

        match state.shown.as_ref().and_then(|d| d.texture.as_ref()) {
            Some(texture) => {
                let ink = ui.visuals().text_color();
                egui::Image::new(texture).tint(ink).paint_at(ui, rect);
            }
            None => {
                ui.painter().rect_filled(rect, 4.0, ui.visuals().faint_bg_color);
            }
        }

        if let Some(drawn) = &state.shown {
            interact(ui, &response, rect, drawn, code, selection, in_chat, &mut out);
        }
    }

    if state.failed.is_some() {
        let note = "The engraver could not draw this ABC. Its text is still there.";
        ui.label(RichText::new(note).small().color(warn));
    }

    if let Some(drawn) = &state.shown
        && !drawn.diagnostics.is_empty()
    {
        let title = format!("{} problem(s) in this ABC", drawn.diagnostics.len());
        egui::CollapsingHeader::new(RichText::new(title).color(warn))
            .id_salt(id.with("problems"))
            .show(ui, |ui| {
                for (line, message) in &drawn.diagnostics {
                    ui.label(format!("line {line}: {message}"));
                }
            });
    }

    ui.data_mut(|d| d.insert_temp(id, state));
    out
}

/// Hover, click and the right-click menu of a drawn score at `rect`.
#[allow(clippy::too_many_arguments)]
fn interact(
    ui: &egui::Ui,
    response: &egui::Response,
    rect: egui::Rect,
    drawn: &Drawn,
    code: &str,
    selection: Option<Span>,
    in_chat: bool,
    out: &mut ScoreResponse,
) {
    // screen points per staff space, as the picture is drawn now
    let scale = rect.width() / drawn.engraving.width.max(1.0);
    let to_screen = |r: &engrave::Rect| {
        egui::Rect::from_min_max(
            rect.min + egui::vec2(r.x0, r.y0) * scale,
            rect.min + egui::vec2(r.x1, r.y1) * scale,
        )
    };
    let hit_at = |pos: egui::Pos2| {
        let p = (pos - rect.min) / scale;
        drawn.engraving.hit_at(p.x, p.y).copied()
    };
    let painter = ui.painter_at(rect);
    let accent = ui.visuals().selection.bg_fill;

    // the marks of the selected text
    if let Some(sel) = selection {
        for hit in &drawn.engraving.hits {
            let span = drawn.span(hit);
            let overlaps = if sel.is_empty() {
                span.contains(sel.start)
            } else {
                span.start < sel.end && sel.start < span.end
            };
            if overlaps {
                painter.rect_filled(to_screen(&hit.rect), 2.0, accent.gamma_multiply(0.5));
            }
        }
    }

    // the mark under the pointer, with its text
    if let Some(pos) = response.hover_pos()
        && let Some(hit) = hit_at(pos)
    {
        painter.rect_stroke(
            to_screen(&hit.rect),
            2.0,
            egui::Stroke::new(1.0, accent),
            egui::StrokeKind::Outside,
        );
        let span = drawn.span(&hit);
        // a picture of older text may not match the text any more
        if let Some(text) = code.get(span.start..span.end) {
            let line = code[..span.start].matches('\n').count() + 1;
            let tip = format!("line {line}: {}", text.trim());
            response.clone().on_hover_text_at_pointer(RichText::new(tip).monospace());
        }
    }

    if response.clicked()
        && let Some(pos) = response.interact_pointer_pos()
        && let Some(hit) = hit_at(pos)
    {
        out.clicked = Some(drawn.span(&hit));
    }

    response.context_menu(|ui| {
        if ui.button("Copy ABC").clicked() {
            ui.ctx().copy_text(code.to_string());
            ui.close();
        }
        if ui.button("Copy image").clicked() {
            if let Some(r) = nulla_render::rasterize(&drawn.engraving, fonts(), EXPORT_PIXELS_PER_SPACE) {
                ui.ctx().copy_image(egui::ColorImage::from_rgba_unmultiplied(
                    [r.width as usize, r.height as usize],
                    &r.on_paper(),
                ));
            }
            ui.close();
        }
        ui.separator();
        if ui.button("Save as SVG…").clicked() {
            out.save = nulla_render::standalone_svg(&drawn.engraving, fonts()).map(String::into_bytes);
            ui.close();
        }
        if ui.button("Save as PNG…").clicked() {
            out.save = nulla_render::rasterize(&drawn.engraving, fonts(), EXPORT_PIXELS_PER_SPACE)
                .and_then(|r| r.png_on_paper());
            ui.close();
        }
        if in_chat {
            ui.separator();
            if ui.button("Open in score pane").clicked() {
                out.open_pane = Some(false);
                ui.close();
            }
            if ui.button("Open in right pane").clicked() {
                out.open_pane = Some(true);
                ui.close();
            }
        }
    });
}

fn hash(code: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    code.hash(&mut hasher);
    hasher.finish()
}

/// About how tall a score will be, in staff spaces, before it is drawn: room
/// for a title and for a staff for each line of music.
fn estimated_height(code: &str) -> f32 {
    let music_lines = code
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('%') && l.as_bytes().get(1) != Some(&b':'))
        .count();
    10.0 + 11.0 * music_lines as f32
}

/// Takes the result of the score's job once it has one.
fn collect(state: &mut BlockState) {
    let Some(slot) = state.job.clone() else { return };
    let mut job = slot.lock().unwrap_or_else(|e| e.into_inner());
    match std::mem::replace(&mut *job, Job::Running) {
        Job::Running => return,
        Job::Done(drawn) => state.shown = Some(Arc::new(drawn)),
        Job::Failed(code_hash) => state.failed = Some(code_hash),
    }
    state.job = None;
}

/// Starts drawing on a worker thread, unless enough are running already;
/// a finished job asks for a repaint, and the next frame tries again.
fn start(ctx: &egui::Context, code: &str, code_hash: u64, width: f32, ppp: f32) -> Option<JobSlot> {
    if RUNNING.fetch_add(1, Ordering::SeqCst) >= MAX_JOBS {
        RUNNING.fetch_sub(1, Ordering::SeqCst);
        return None;
    }
    let slot: JobSlot = Arc::new(Mutex::new(Job::Running));
    let worker = Worker {
        slot: slot.clone(),
        ctx: ctx.clone(),
        code_hash,
    };
    let code = code.to_string();
    std::thread::Builder::new()
        .name("abc-score".to_string())
        .spawn(move || {
            let worker = worker;
            let drawn = draw(&worker.ctx, &code, code_hash, width, ppp);
            *worker.slot.lock().unwrap_or_else(|e| e.into_inner()) = Job::Done(drawn);
        })
        .ok()
        .map(|_| slot)
}

/// Counts a job as running for as long as it lives. A job that ends without
/// a picture (the engraver panicked) is marked failed.
struct Worker {
    slot: JobSlot,
    ctx: egui::Context,
    code_hash: u64,
}

impl Drop for Worker {
    fn drop(&mut self) {
        let mut job = self.slot.lock().unwrap_or_else(|e| e.into_inner());
        if matches!(*job, Job::Running) {
            *job = Job::Failed(self.code_hash);
        }
        drop(job);
        RUNNING.fetch_sub(1, Ordering::SeqCst);
        self.ctx.request_repaint();
    }
}

/// Parses, engraves and rasterizes one score. Runs on a worker thread.
fn draw(ctx: &egui::Context, code: &str, code_hash: u64, width: f32, ppp: f32) -> Drawn {
    // Without an X: line the parser sees no tune, and models often leave it out.
    let prefix = if code.lines().any(|l| l.starts_with("X:")) { "" } else { "X:1\n" };
    let source = format!("{prefix}{code}");
    let mut score = Score::default();
    score.load_abc(&source);
    score.engrave(width);

    let prefix_lines = prefix.matches('\n').count();
    let diagnostics = score
        .diagnostics()
        .iter()
        .map(|d| {
            let line = source[..d.span.start.min(source.len())].matches('\n').count() + 1;
            (line.saturating_sub(prefix_lines).max(1), d.message.clone())
        })
        .collect();

    let texture = nulla_render::rasterize(&score.engraving, fonts(), POINTS_PER_SPACE * ppp).map(|r| {
        let image = egui::ColorImage::from_rgba_unmultiplied(
            [r.width as usize, r.height as usize],
            &r.white_mask(),
        );
        ctx.load_texture(format!("abc_score_{code_hash:x}"), image, egui::TextureOptions::default())
    });

    Drawn {
        code_hash,
        width,
        pixels_per_point: ppp,
        texture,
        engraving: std::mem::take(&mut score.engraving),
        prefix_len: prefix.len(),
        diagnostics,
    }
}
