//! ABC music in chat messages, drawn as scores by Nulla's engraver
//! (`nulla_render`).
//!
//! Pictures are drawn on worker threads, so a chat full of scores stays
//! responsive, and an engraver panic on odd input costs one picture, not the
//! app. A block is drawn only once it scrolls into view; until then it holds
//! a placeholder of about the right height. When the text, the column width
//! or the display scale changes, the old picture stays (scaled to fit) until
//! the new one is ready. Pictures are white and tinted with the text colour,
//! like math, so a theme change needs no new picture.

use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use egui::RichText;
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

/// How many pictures are drawn at the same time, across all chats.
const MAX_JOBS: usize = 2;

/// Pictures being drawn right now.
static RUNNING: AtomicUsize = AtomicUsize::new(0);

/// The fonts scores are drawn with, built into the binary and loaded on
/// first use: Bravura for the music, Liberation Serif (Times metrics, with
/// Cyrillic) for titles and lyrics. Both are under the SIL OFL.
fn fonts() -> &'static Fonts {
    static FONTS: OnceLock<Fonts> = OnceLock::new();
    FONTS.get_or_init(|| {
        let files: [&[u8]; 4] = [
            include_bytes!("../../assets/fonts/Bravura.otf"),
            include_bytes!("../../assets/fonts/LiberationSerif-Regular.ttf"),
            include_bytes!("../../assets/fonts/LiberationSerif-Italic.ttf"),
            include_bytes!("../../assets/fonts/LiberationSerif-Bold.ttf"),
        ];
        Fonts::new(files.map(|bytes| FontSource::Data(bytes.to_vec())))
    })
}

/// A finished picture and what it was drawn from.
struct Drawn {
    code_hash: u64,
    /// Page width in staff spaces.
    width: f32,
    pixels_per_point: f32,
    texture: Option<egui::TextureHandle>,
    /// Picture size in staff spaces.
    size: egui::Vec2,
    /// (line in the block, message)
    diagnostics: Vec<(usize, String)>,
}

impl Drawn {
    /// The picture still shows this text at this width and scale.
    fn fits(&self, code_hash: u64, width: f32, ppp: f32) -> bool {
        self.code_hash == code_hash
            && self.pixels_per_point == ppp
            && (self.width - width).abs() <= WIDTH_SLACK
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

/// What a block keeps between frames, in egui's temp memory.
#[derive(Clone, Default)]
struct BlockState {
    /// The latest finished picture.
    shown: Option<Arc<Drawn>>,
    /// The picture being drawn, if any. One at a time per block.
    job: Option<JobSlot>,
    /// The text the engraver failed on; not tried again until it changes.
    failed: Option<u64>,
}

/// Draws the ABC text of a code block as a score, with a folded list of what
/// could not be read or engraved below it. `id` names the block.
pub(super) fn show_score(ui: &mut egui::Ui, id: egui::Id, code: &str) {
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
            Some(drawn) => drawn.size * POINTS_PER_SPACE,
            None => egui::vec2(available, estimated_height(code) * POINTS_PER_SPACE),
        };
        let size = if size.x > available { size * (available / size.x) } else { size };
        let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());

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
    }

    if state.failed.is_some() {
        let note = "The engraver could not draw this ABC. Its source is below.";
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

/// Takes the result of the block's job once it has one.
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

/// Parses, engraves and rasterizes one block. Runs on a worker thread.
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
        size: egui::vec2(score.engraving.width, score.engraving.height),
        diagnostics,
    }
}
