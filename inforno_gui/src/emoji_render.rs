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

/// Build (but don't add) a `Button` whose only content is `emoji`, drawn as
/// a color image if this font has one, or as plain text otherwise. Lets the
/// caller chain builder methods (`.small()`, `.selected(..)`, etc.) before
/// calling `ui.add(...)`, same as `egui::Button::new(...)` would.
pub fn emoji_button_widget(ui: &egui::Ui, emoji: char) -> egui::Button<'static> {
    match emoji_image(ui, emoji) {
        Some(img) => egui::Button::image(img),
        None => egui::Button::new(emoji.to_string()),
    }
}

/// Build (but don't yet show) a `MenuButton` whose opener is `emoji` drawn
/// as a color image, with an optional trailing text/RichText atom (e.g. a
/// live attachment count). Falls back to a plain-text opener if this font
/// has no color glyph for `emoji`. Call `.ui(ui, |ui| { ... })` on the
/// result the same way you would on `MenuButton::new(...)`.
pub fn emoji_menu_button(
    ui: &egui::Ui,
    emoji: char,
    trailing: Option<egui::WidgetText>,
) -> egui::containers::menu::MenuButton<'static> {
    use egui::containers::menu::MenuButton;

    match (emoji_image(ui, emoji), trailing) {
        (Some(img), Some(text)) => MenuButton::new((img, text)),
        (Some(img), None) => MenuButton::new(img),
        (None, Some(text)) => MenuButton::new(format!("{emoji} {}", text.text())),
        (None, None) => MenuButton::new(emoji.to_string()),
    }
}

/// Drop-in replacement for `ui.button(text)` that renders a single leading
/// emoji as an inline color image, e.g. "💾 Save" draws a real floppy-disk
/// image followed by a "Save" text atom. This only inspects the *first*
/// character - it's built around the "<emoji> Label" convention already
/// used for every button in this file, not arbitrary multi-emoji strings.
/// (`IntoAtoms` has a documented impl for 2-tuples of `Into<Atom>`, which is
/// what `ui.button((image, "text"))` in the egui docs relies on - there's
/// no equivalent impl for an arbitrary-length `Vec<Atom>`.)
pub fn emoji_button(ui: &mut egui::Ui, text: &str) -> egui::Response {
    let mut chars = text.chars();
    if let Some(first) = chars.next() {
        if let Some(img) = emoji_image(ui, first) {
            let rest = chars.as_str().trim_start();
            if rest.is_empty() {
                // Icon-only button (e.g. "🗐" with no trailing label) - skip
                // the text atom entirely so we don't reserve space/gap for
                // an empty string, which was stretching the button out into
                // a rectangle instead of keeping it square.
                return ui.add(egui::Button::image(img));
            }
            return ui.add(egui::Button::new((img, rest)));
        }
    }
    ui.button(text)
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

/// Codepoints we are willing to swap for a color bitmap inside LLM
/// Markdown. Conservative on purpose: arrows, check marks, dingbats and
/// regional indicators stay on the normal text path.
fn is_emoji_candidate(c: char) -> bool {
    matches!(c as u32,
        0x1F300..=0x1F64F
        | 0x1F680..=0x1F6FF
        | 0x1F900..=0x1FAFF
        | 0x2705 | 0x274C | 0x274E | 0x2753..=0x2755 | 0x2757
        | 0x2728 | 0x2B50 | 0x26A0 | 0x26A1 | 0x2764)
}

/// Variation selector, ZWJ, keycap and skin-tone modifiers. Swallowed when
/// they follow an emoji, since we cannot shape sequences.
fn is_emoji_joiner(c: char) -> bool {
    matches!(c as u32, 0xFE0F | 0x200D | 0x20E3 | 0x1F3FB..=0x1F3FF)
}

/// Rewrites emoji in Markdown text as `$emoji:<hex>[,<hex>..]$` placeholders,
/// which the math hook in `render_msg_content` turns into inline images.
/// Inline code spans are copied verbatim. Returns the input untouched
/// (no allocation) when it contains no emoji.
pub fn emojify_markdown(text: &str) -> std::borrow::Cow<'_, str> {
    use std::borrow::Cow;

    if !text.chars().any(|c| is_emoji_candidate(c) || is_regional_indicator(c)) {
        return Cow::Borrowed(text);
    }

    fn flush(out: &mut String, run: &mut Vec<String>) {
        if !run.is_empty() {
            out.push_str("$emoji:");
            out.push_str(&run.join(","));
            out.push('$');
            run.clear();
        }
    }

    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len() + 32);
    let mut run: Vec<String> = Vec::new();
    let mut i = 0;
    let mut join_next = false;

    while i < text.len() {
        // Inline code span: copy verbatim, including the closing fence.
        if bytes[i] == b'`' {
            flush(&mut out, &mut run);
            let start = i;
            while i < bytes.len() && bytes[i] == b'`' {
                i += 1;
            }
            let ticks = i - start;
            let fence = "`".repeat(ticks);
            if let Some(rel) = text[i..].find(&fence) {
                i += rel + ticks;
            }
            out.push_str(&text[start..i]);
            continue;
        }

        let c = text[i..].chars().next().unwrap();
        i += c.len_utf8();

        if is_emoji_joiner(c) && !run.is_empty() {
            if let Some(last) = run.last_mut() {
                last.push_str(&format!("-{:x}", c as u32));
            }
            join_next = c as u32 == 0x200D;
            continue;
        }
        if is_regional_indicator(c) {
            let next = text[i..].chars().next().filter(|n| is_regional_indicator(*n));
            if let Some(n) = next {
                i += n.len_utf8();
                run.push(format!("{:x}-{:x}", c as u32, n as u32));
                join_next = false;
                continue;
            }
        }
        if is_emoji_candidate(c) {
            if join_next && !run.is_empty() {
                if let Some(last) = run.last_mut() {
                    last.push_str(&format!("-{:x}", c as u32));
                }
            } else {
                run.push(format!("{:x}", c as u32));
            }
            join_next = false;
        } else {
            flush(&mut out, &mut run);
            out.push(c);
        }
    }
    flush(&mut out, &mut run);

    Cow::Owned(out)
}

/// Draws the emoji named by an `emojify_markdown` placeholder payload
/// (comma-separated hex codepoints). Falls back to plain text for any
/// codepoint without a color bitmap.
pub fn emoji_inline(ui: &mut egui::Ui, codes: &str) {
    for item in codes.split(',') {
        let cps: Vec<char> = item
            .split('-')
            .filter_map(|h| u32::from_str_radix(h, 16).ok().and_then(char::from_u32))
            .collect();
        let Some(&first) = cps.first() else {
            continue;
        };
        if let Some(img) = emoji_seq_image(ui, &cps) {
            ui.add(img);
        } else if let Some(img) = emoji_image(ui, first).filter(|_| !is_regional_indicator(first)) {
            ui.add(img);
        } else {
            ui.label(cps.iter().collect::<String>());
        }
    }
}

thread_local! {
    // Separate rustybuzz view of the same embedded bytes (no extra copy of
    // the font data itself, just the parsed table offsets).
    static SHAPE_FACE: Option<rustybuzz::Face<'static>> =
        rustybuzz::Face::from_slice(EMOJI_FONT_BYTES, 0);
}

/// Shapes `cps` with the emoji font's GSUB rules. Returns the glyph only if
/// the whole sequence collapsed into exactly one real glyph (a flag, ZWJ
/// family, skin-tone sequence...). Otherwise `None`.
fn shape_sequence_gid(cps: &[char]) -> Option<ttf_parser::GlyphId> {
    SHAPE_FACE.with(|face| {
        let face = face.as_ref()?;
        let mut buf = rustybuzz::UnicodeBuffer::new();
        buf.push_str(&cps.iter().collect::<String>());
        buf.guess_segment_properties();
        let out = rustybuzz::shape(face, &[], buf);
        let infos = out.glyph_infos();
        if infos.len() == 1 && infos[0].glyph_id != 0 {
            Some(ttf_parser::GlyphId(infos[0].glyph_id as u16))
        } else {
            None
        }
    })
}

fn is_regional_indicator(c: char) -> bool {
    matches!(c as u32, 0x1F1E6..=0x1F1FF)
}

thread_local! {
    static EMOJI_SEQ_CACHE: RefCell<HashMap<(String, u16), Option<Arc<[u8]>>>> =
        RefCell::new(HashMap::new());
}

/// PNG bytes for a (possibly multi-codepoint) emoji sequence. Multi-codepoint
/// sequences are looked up by Noto's glyph name (`u1F1E8_1F1E6`), trying the
/// sequence as given and then without VS16 (U+FE0F).
fn emoji_seq_png_bytes(cps: &[char], px: u16) -> Option<Arc<[u8]>> {
    let key = cps.iter().map(|c| format!("{:x}", *c as u32)).collect::<Vec<_>>().join("-");

    EMOJI_SEQ_CACHE.with(|cache| {
        if let Some(hit) = cache.borrow().get(&(key.clone(), px)) {
            return hit.clone();
        }

        let face = emoji_face();
        let no_vs16: Vec<char> = cps.iter().copied().filter(|&c| c as u32 != 0xFE0F).collect();
        let mut bytes = None;

        for variant in [cps, no_vs16.as_slice()] {
            if variant.is_empty() {
                continue;
            }
            let gid = if variant.len() == 1 {
                face.glyph_index(variant[0])
            } else {
                shape_sequence_gid(variant)
            };
            bytes = gid.and_then(|gid| {
                face.glyph_raster_image(gid, px)
                    .filter(|img| img.format == RasterImageFormat::PNG)
                    .map(|img| Arc::<[u8]>::from(img.data))
            });
            if bytes.is_some() {
                break;
            }
        }

        cache.borrow_mut().insert((key, px), bytes.clone());
        bytes
    })
}

/// Same sizing as `emoji_image`, but for a whole emoji sequence.
fn emoji_seq_image(ui: &egui::Ui, cps: &[char]) -> Option<egui::Image<'static>> {
    let row_h = ui.text_style_height(&egui::TextStyle::Body);
    let px = ((row_h * ui.ctx().pixels_per_point()) as u16).max(16) * 2;

    let bytes = emoji_seq_png_bytes(cps, px)?;
    let key = cps.iter().map(|c| format!("{:x}", *c as u32)).collect::<Vec<_>>().join("-");
    let uri = format!("bytes://emoji_seq_{}_{}.png", key, px);

    Some(
        egui::Image::new(egui::ImageSource::Bytes {
            uri: uri.into(),
            bytes: egui::load::Bytes::Shared(bytes),
        })
        .fit_to_exact_size(egui::vec2(row_h, row_h)),
    )
}