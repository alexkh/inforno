//! Renders emoji as color images pulled directly from the bundled
//! NotoColorEmoji.ttf. egui 0.36's text pipeline only rasterizes glyphs
//! through a single-channel grayscale atlas, so color emoji have to be
//! drawn as images rather than laid out as font glyphs.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use ttf_parser::{Face, RasterImageFormat};

// Reuse the exact bytes already embedded for the font fallback stack in
// `configure_fonts` (main.rs) - no extra asset to ship.
static EMOJI_FONT_BYTES: &[u8] = include_bytes!("../assets/fonts/NotoColorEmoji.ttf");

fn emoji_face() -> &'static Face<'static> {
    static FACE: OnceLock<Face<'static>> = OnceLock::new();
    FACE.get_or_init(|| {
        Face::parse(EMOJI_FONT_BYTES, 0).expect("bundled NotoColorEmoji.ttf failed to parse")
    })
}

thread_local! {
    // egui UI code all runs on one thread, so a thread-local cache avoids
    // needing to plumb a cache handle through every render function.
    static EMOJI_PNG_CACHE: RefCell<HashMap<(char, u16), Option<Arc<[u8]>>>> =
        RefCell::new(HashMap::new());
}

/// Look up (and cache) the raw PNG bytes embedded in NotoColorEmoji.ttf for
/// `ch`, at the strike closest to `px` pixels-per-em. `None` means this font
/// has no color bitmap for the character (caller should fall back to text).
pub fn emoji_png_bytes(ch: char, px: u16) -> Option<Arc<[u8]>> {
    EMOJI_PNG_CACHE.with(|cache| {
        if let Some(hit) = cache.borrow().get(&(ch, px)) {
            return hit.clone();
        }

        let face = emoji_face();
        let bytes = face.glyph_index(ch).and_then(|gid| {
            face.glyph_raster_image(gid, px)
                .filter(|img| img.format == RasterImageFormat::PNG)
                .map(|img| Arc::<[u8]>::from(img.data))
        });

        cache.borrow_mut().insert((ch, px), bytes.clone());
        bytes
    })
}

/// Build an `egui::Image` for a single emoji character, sized to roughly
/// match the current text row height. Returns `None` if there's no color
/// glyph for this character.
pub fn emoji_image(ui: &egui::Ui, ch: char) -> Option<egui::Image<'static>> {
    let row_h = ui.text_style_height(&egui::TextStyle::Body);
    // Request a strike a bit larger than the display size for crisper downscaling.
    let px = ((row_h * ui.ctx().pixels_per_point()) as u16).max(16) * 2;

    let bytes = emoji_png_bytes(ch, px)?;
    let uri = format!("bytes://emoji_{:x}_{}.png", ch as u32, px);

    Some(
        egui::Image::new(egui::ImageSource::Bytes {
            uri: uri.into(),
            bytes: egui::load::Bytes::Shared(bytes),
        })
        .fit_to_exact_size(egui::vec2(row_h, row_h)),
    )
}

/// Draw a short, app-authored string that may contain emoji, replacing any
/// codepoint we have a color bitmap for with an inline image and leaving
/// everything else as normal text. Meant for buttons/headers/status lines -
/// NOT for arbitrary LLM markdown, which flows through `CommonMarkViewer`
/// and would need a separate hook to get inline color emoji there.
pub fn emoji_label(ui: &mut egui::Ui, text: &str) {
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        let mut run = String::new();

        for ch in text.chars() {
            match emoji_image(ui, ch) {
                Some(img) => {
                    if !run.is_empty() {
                        ui.label(std::mem::take(&mut run));
                    }
                    ui.add(img);
                }
                None => run.push(ch),
            }
        }
        if !run.is_empty() {
            ui.label(run);
        }
    });
}