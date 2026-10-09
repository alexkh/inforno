//! Renders the body of a single chat message: markdown (with math and
//! emoji), executable Rhai blocks, and code blocks with their file /
//! merge-tool / inline-diff tooling.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::mpsc::Sender;

use bulat::editor::{CodeEditor, ColorTheme, Syntax};
use egui::RichText;
use egui_commonmark::CommonMarkViewer;
use regex::Regex;
use rust_i18n::t;

use inforno_core::common::{ChatMsg, FileOp, FileOpMsg};
use inforno_core::parsing::{ContentChunk, parse_chunks};
use inforno_core::realm::ActiveRealm;

use super::latex::{compile_math_to_svg_embedded, MathLayout};
use super::table::{render_table, split_tables, MdBlock};
use crate::split_button::SplitButton;
use crate::state::ChatMsgUi;

use super::{ChatActions, MathCache, RenderCtx, GUI_ROLE};

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

pub(super) fn render_msg_content(
    ui: &mut egui::Ui,
    ctx: &mut RenderCtx,
    actions: &mut ChatActions,
    msg: &ChatMsg,
    msg_ui: &mut ChatMsgUi,
    max_image_width: usize,
) {
    if msg_ui.show_raw {
        ui.label(RichText::new(msg.content.as_str()).strong());
        return;
    }

    for (i, chunk) in parse_chunks(&msg.content).into_iter().enumerate() {
        match chunk {
            ContentChunk::Markdown(md_text) => {
                for (j, block) in split_tables(md_text).into_iter().enumerate() {
                    let id_salt = (msg.id, i, j);
                    match block {
                        MdBlock::Text(text) => {
                            render_markdown(ui, &mut *ctx.cache, id_salt, text, max_image_width, &ctx.math_cache);
                        }
                        MdBlock::Table(table) => {
                            render_table(ui, &table, id_salt, |ui, cell, cell_id| {
                                let cell_image_width = max_image_width.min(ui.available_width() as usize);
                                render_markdown(ui, &mut *ctx.cache, cell_id, cell, cell_image_width, &ctx.math_cache);
                            });
                        }
                    }
                }
            }
            ContentChunk::Code { lang, code, filepath } => {
                let mime_type = resolve_mime(lang, filepath.as_deref());
                if mime_type == "application/x-rhai" {
                    render_rhai_block(
                        ui, msg, i, code,
                        &mut actions.rhai_updates, &mut actions.llm_prompt_request,
                    );
                } else {
                    render_code_block(
                        ui, msg, msg_ui, i, code, filepath.as_deref(), mime_type,
                        ctx.project_root, ctx.active_realm, ctx.op_tx,
                        &mut actions.comfy_workflow_request,
                    );
                }
            }
        }
    }
}

/// MIME type from the fence's language tag, falling back to the file
/// extension from the header when the tag is missing or unrecognized.
fn resolve_mime(lang: &str, filepath: Option<&str>) -> &'static str {
    let mime = Syntax::guess_mime_from_markdown_lang(lang);
    if mime == "text/plain" {
        if let Some(path) = filepath {
            return Syntax::guess_mime_from_path(Path::new(path));
        }
    }
    mime
}

// ---------------------------------------------------------------------------
// Markdown + math
// ---------------------------------------------------------------------------

fn render_markdown(
    ui: &mut egui::Ui,
    cache: &mut egui_commonmark::CommonMarkCache,
    id_salt: impl std::hash::Hash + std::fmt::Debug,
    md_text: &str,
    max_image_width: usize,
    math_cache: &MathCache,
) {
    if md_text.trim().is_empty() {
        return;
    }

    let local_math_cache = math_cache.clone();
    let emojified = crate::emoji_render::emojify_markdown(md_text);

    // Unique egui ID context per markdown block (or table cell)
    ui.push_id(id_salt, |ui| {
        // Keep every label's baseline one ascent below its row top, however
        // tall the row gets. Inline math raises a row's height to make room
        // below a formula, and relies on this to leave the text in place.
        ui.style_mut().override_text_valign = Some(egui::Align::TOP);

        CommonMarkViewer::new()
            .max_image_width(Some(max_image_width))
            .render_math_fn(Some(&mut move |ui, math, is_inline| {
                render_math(ui, &local_math_cache, math, is_inline);
            }))
            .show(ui, cache, &emojified);
    });
}

/// Math hook for CommonMarkViewer. Also renders `$emoji:...$` placeholders
/// produced by `emojify_markdown`.
fn render_math(ui: &mut egui::Ui, math_cache: &MathCache, math: &str, is_inline: bool) {
    if let Some(codes) = math.strip_prefix("emoji:") {
        crate::emoji_render::emoji_inline(ui, codes);
        return;
    }

    // `$x$` and `$$x$$` render differently, so the mode is part of the key
    // (both for our cache and for egui's texture cache, which keys by URI).
    let key = format!("{}:{}", if is_inline { "inline" } else { "display" }, math);

    let (svg_bytes, layout) = compiled_math(ui, math_cache, &key, math, is_inline);

    // Failed compilation: show the raw LaTeX in a warning color so it isn't lost.
    if svg_bytes.is_empty() {
        let raw_math = if is_inline { format!("${}$", math) } else { format!("$${}$$", math) };
        ui.label(RichText::new(raw_math).monospace().color(ui.visuals().warn_fg_color));
        return;
    }

    let uri = format!("bytes://math_{}.svg", egui::Id::new(&key).value());
    let image = egui::Image::new(egui::ImageSource::Bytes {
        uri: uri.into(),
        bytes: egui::load::Bytes::Shared(svg_bytes),
    })
    .tint(ui.visuals().text_color());

    // egui points per Typst pt. The body *row* height is the reference, the
    // same one EGUI_BASELINE_IN_ROW is measured against.
    let row_height = ui.text_style_height(&egui::TextStyle::Body);
    let pt_to_ui = row_height / 11.0 * OPTICAL_SCALE * SVG_PX_PER_PT;

    let actually_inline = is_inline && !math.contains("\\displaystyle");
    if actually_inline {
        paint_inline_math(ui, &image, &layout, pt_to_ui, row_height);
    } else {
        paint_display_math(ui, &image, layout.size_pt * pt_to_ui, row_height);
    }
}

/// Display math (`$$…$$`, or inline math using `\displaystyle`) gets a line
/// of its own, centered, with a little space above and below, like LaTeX.
/// The slot spans the whole line, so egui starts a new line for it when
/// needed and whatever follows continues below it. A formula wider than the
/// line shrinks to fit.
fn paint_display_math(ui: &mut egui::Ui, image: &egui::Image<'_>, size: egui::Vec2, row_height: f32) {
    // While a table measures its columns (sizing pass, unbounded width),
    // report the formula's own width rather than the whole line.
    let line_width = if ui.is_sizing_pass() { size.x } else { ui.max_rect().width() };
    let size = size * (line_width / size.x).min(1.0);
    let padding = 0.3 * row_height;
    let (slot, _) = ui.allocate_exact_size(egui::vec2(line_width, size.y + 2.0 * padding), egui::Sense::hover());
    image.paint_at(ui, egui::Rect::from_center_size(slot.center(), size));
}

/// Typst's 11pt text is drawn at this fraction of the egui body row height.
/// Tuned by eye: it matches the x-height of Typst's math font to egui's
/// Ubuntu-Light.
const OPTICAL_SCALE: f32 = 0.8;

/// egui's SVG loader (usvg) converts the `pt` sizes Typst writes into CSS
/// pixels: 96 px per 72 pt.
const SVG_PX_PER_PT: f32 = 96.0 / 72.0;

/// Where egui puts the text baseline inside a body text row, as a fraction
/// of the row height from its top: `ascent / (ascent - descent + line_gap)`.
/// Values for Ubuntu-Light, egui's default proportional font (932, -189, 28
/// per 1000 units). With a different body font, recompute from its metrics.
const EGUI_BASELINE_IN_ROW: f32 = 932.0 / 1149.0;

/// Returns the formula's SVG bytes and layout numbers, compiling on first
/// use. The bytes live in `math_cache`; the layout lives in egui's temp
/// memory under the same key, so the cache type (shared with image
/// attachments) is unchanged.
fn compiled_math(
    ui: &egui::Ui,
    math_cache: &MathCache,
    key: &str,
    math: &str,
    is_inline: bool,
) -> (std::sync::Arc<[u8]>, MathLayout) {
    let layout_id = egui::Id::new(("math_layout", key));
    let cached_bytes = math_cache.borrow().get(key).cloned();
    let cached_layout = ui.data(|d| d.get_temp::<MathLayout>(layout_id));

    if let (Some(bytes), Some(layout)) = (cached_bytes, cached_layout) {
        return (bytes, layout);
    }

    let (bytes, layout): (std::sync::Arc<[u8]>, MathLayout) =
        match compile_math_to_svg_embedded(math, is_inline) {
            Some(m) => (m.svg.into(), m.layout),
            None => (std::sync::Arc::from([]), MathLayout::default()),
        };
    math_cache.borrow_mut().insert(key.to_string(), bytes.clone());
    ui.data_mut(|d| d.insert_temp(layout_id, layout.clone()));
    (bytes, layout)
}

/// Draws an inline formula. A formula that fits on a line is drawn whole,
/// and egui moves it to the next line when it doesn't fit where it starts.
/// Only a formula wider than a whole line is split, at the TeX-style break
/// points `latex.rs` found, into pieces that egui wraps like words. If a
/// piece is still wider than the line, the whole formula shrinks to fit, so
/// it never needs a horizontal scrollbar.
fn paint_inline_math(ui: &mut egui::Ui, image: &egui::Image<'_>, layout: &MathLayout, pt_to_ui: f32, row_height: f32) {
    let size = layout.size_pt * pt_to_ui;
    let wraps = ui.layout().main_wrap();
    let line_width = ui.max_rect().width();

    let mut cuts = vec![0.0];
    if wraps && size.x > line_width {
        cuts.extend(layout.breaks_pt.iter().map(|b| b * pt_to_ui));
    }
    cuts.push(size.x);

    let widest = cuts.windows(2).map(|w| w[1] - w[0]).fold(0.0, f32::max);
    let shrink = if wraps && widest > line_width { line_width / widest } else { 1.0 };

    // Rasterize once at the full size; each piece paints its slice of it.
    let full_size = size * shrink;
    let texture = image.clone().fit_to_exact_size(full_size).load_for_size(ui.ctx(), full_size);
    let tint = ui.visuals().text_color();

    for w in cuts.windows(2) {
        let piece_size = egui::vec2(w[1] - w[0], size.y) * shrink;
        let rect = place_inline_piece(ui, piece_size, layout.baseline_pt * pt_to_ui * shrink, row_height);
        if let Ok(egui::load::TexturePoll::Ready { texture }) = &texture {
            let uv = egui::Rect::from_min_max(egui::pos2(w[0] / size.x, 0.0), egui::pos2(w[1] / size.x, 1.0));
            ui.painter().image(texture.id, rect, uv, tint);
        }
    }
}

/// Places one inline piece like a word of text and returns where to paint
/// it: the slot fills the current row, so it starts at the row top in every
/// layout, and the image is offset so its baseline lands on the text
/// baseline. If the piece hangs below the row (denominators, lower limits),
/// the row grows to cover it, so labels after it on this line start their
/// second line below the formula (their baseline stays put, see the TOP
/// alignment in `render_markdown`), and the ui grows so the next row does
/// too. Anything rising above the row is painted into the line gap; the row
/// above is already laid out.
fn place_inline_piece(ui: &mut egui::Ui, size: egui::Vec2, baseline: f32, row_height: f32) -> egui::Rect {
    let wraps = ui.layout().main_wrap();

    if wraps {
        let at_line_start = |ui: &egui::Ui| ui.cursor().left() <= ui.max_rect().left() + 0.5;

        // About to wrap: start the new line ourselves, one text row tall
        // (not as tall as this line may have grown).
        if !at_line_start(ui) && ui.available_size_before_wrap().x < size.x {
            ui.end_row();
            ui.set_row_height(row_height);
        }

        // Nothing is on this line yet, so if the piece rises above it (tall
        // superscripts, \overbrace labels, \big delimiters) the whole line
        // can still move down to clear the line above. Mid-line that isn't
        // possible: the text before it is already placed.
        let rise = baseline - EGUI_BASELINE_IN_ROW * row_height - ui.spacing().item_spacing.y;
        if rise > 0.5 && at_line_start(ui) {
            ui.set_row_height(rise);
            ui.allocate_exact_size(egui::vec2(0.0, rise), egui::Sense::hover());
            ui.end_row();
            ui.set_row_height(row_height);
        }
    }

    let slot_height = if wraps { ui.cursor().height().max(row_height) } else { row_height };
    let (slot, _) = ui.allocate_exact_size(egui::vec2(size.x, slot_height), egui::Sense::hover());

    let text_baseline_y = slot.top() + EGUI_BASELINE_IN_ROW * row_height;
    let rect = egui::Rect::from_min_size(egui::pos2(slot.left(), text_baseline_y - baseline), size);

    if rect.bottom() > slot.bottom() {
        ui.expand_to_include_rect(egui::Rect::from_min_max(
            slot.left_bottom(),
            egui::pos2(slot.left(), rect.bottom()),
        ));
        if wraps {
            ui.set_row_height(rect.bottom() - ui.cursor().top());
        }
    }

    rect
}

// ---------------------------------------------------------------------------
// Rhai script blocks
// ---------------------------------------------------------------------------

fn render_rhai_block(
    ui: &mut egui::Ui,
    msg: &ChatMsg,
    chunk_idx: usize,
    code: &str,
    rhai_updates: &mut Vec<(i64, String)>,
    llm_prompt_request: &mut Option<String>,
) {
    ui.add_space(6.0);
    egui::Frame::default()
        .inner_margin(8.0)
        .corner_radius(5.0)
        .stroke(egui::Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color))
        .fill(ui.visuals().extreme_bg_color)
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                crate::emoji_render::emoji_label(ui, "📜 Rhai Script");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("🗐").on_hover_text("Copy to clipboard").clicked() {
                        ui.ctx().copy_text(code.to_string());
                    }
                    if ui.button("▶ Run").on_hover_text("Execute this Rhai script").clicked() {
                        let (output, prompt_req) = inforno_core::scripting::run_rhai(code);
                        rhai_updates.push((msg.id, output));
                        if prompt_req.is_some() {
                            *llm_prompt_request = prompt_req;
                        }
                    }
                });
            });

            let mut display_buffer = code.to_string();
            display_buffer.push('\n');

            CodeEditor::default()
                .id_source(format!("rhai_code_{}_{}", msg.id, chunk_idx))
                .with_theme(ColorTheme::SV)
                .with_syntax(Syntax::rhai())
                .with_numlines(false)
                .with_rows(display_buffer.lines().count().max(1))
                .vscroll(false)
                .v_auto_shrink(true)
                .show(ui, &mut display_buffer);

            if let Some(vol_out) = &msg.volatile_output {
                ui.separator();
                ui.label(RichText::new("Output:").weak().small());
                // `&str` makes it read-only while still selectable/copyable
                let mut out_str = vol_out.as_str();
                ui.add(
                    egui::TextEdit::multiline(&mut out_str)
                        .font(egui::FontId::monospace(10.0))
                        .frame(egui::Frame::new())
                        .desired_width(f32::INFINITY),
                );
            }
        });
    ui.add_space(6.0);
}

// ---------------------------------------------------------------------------
// Code blocks
// ---------------------------------------------------------------------------

fn render_code_block(
    ui: &mut egui::Ui,
    msg: &ChatMsg,
    msg_ui: &mut ChatMsgUi,
    chunk_idx: usize,
    code: &str,
    filepath: Option<&str>,
    mime_type: &'static str,
    project_root: &Option<PathBuf>,
    active_realm: &Option<ActiveRealm>,
    op_tx: &Sender<FileOpMsg>,
    comfy_workflow_request: &mut Option<String>,
) {
    ui.add_space(6.0);

    render_comfy_workflow_button(ui, code, comfy_workflow_request);

    let resolved = filepath.and_then(|p| {
        inforno_core::realm::resolve_filepath(active_realm, GUI_ROLE, project_root, p)
    });

    // --- HEADER: path / merge buttons on the left, copy + inline diff on the right ---
    ui.horizontal(|ui| {
        match (&resolved, filepath) {
            (Some((path, autocorrected)), Some(display_path)) => {
                render_file_buttons(
                    ui, msg.id, chunk_idx, path, display_path, *autocorrected,
                    code, project_root, active_realm, op_tx,
                );
            }
            (_, Some(path)) => {
                // Has a filepath but no project root/realm to resolve it against
                if ui.button(format!("📝 {}", path)).on_hover_text("Open file in editor").clicked() {
                    send_open_editor(op_tx, PathBuf::from(path), FileOp::OpenEditor);
                }
            }
            (_, None) => {
                ui.label(RichText::new(language_label(mime_type)).weak());
            }
        }

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.button("🗐").on_hover_text("Copy to clipboard").clicked() {
                ui.ctx().copy_text(code.to_string());
            }
            if let Some((path, _)) = &resolved {
                render_inline_diff_controls(ui, msg_ui, chunk_idx, code, path, project_root, active_realm);
            }
        });
    });

    // --- BODY: inline diff tool if one was mounted, otherwise plain code ---
    if let Some(diff_app) = msg_ui.inline_diffs.get_mut(&chunk_idx) {
        ui.push_id(format!("diff_app_wrapper_{}_{}", msg.id, chunk_idx), |ui| {
            egui::Frame::default()
                .inner_margin(4.0)
                .stroke(egui::Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color))
                .show(ui, |ui| {
                    diff_app.show(ui);
                });
        });
    } else {
        render_plain_code(ui, msg.id, chunk_idx, code, mime_type);
    }

    ui.add_space(6.0);
}

fn render_plain_code(ui: &mut egui::Ui, msg_id: i64, chunk_idx: usize, code: &str, mime_type: &str) {
    let mut display_buffer = code.to_string();
    display_buffer.push('\n');
    let display_lines = display_buffer.lines().count().max(1);

    CodeEditor::default()
        .id_source(format!("code_block_{}_{}", msg_id, chunk_idx))
        .with_theme(ColorTheme::SV)
        .with_syntax(Syntax::from_mime(mime_type))
        .with_numlines(false)
        .with_rows(display_lines)
        // The parent chat window handles scrolling natively
        .vscroll(false)
        .v_auto_shrink(true)
        .show(ui, &mut display_buffer);
}

fn language_label(mime_type: &str) -> &'static str {
    match mime_type {
        "text/rust" => "🦀 Rust",
        "text/x-c" | "text/x-c++" => "⚙️ C/C++",
        "application/x-rhai" => "📜 Rhai",
        "text/x-python" => "🐍 Python",
        "text/javascript" | "text/typescript" => "🌐 JS/TS",
        "text/html" => "🌐 HTML",
        "text/markdown" => "📝 Markdown",
        "application/json" => "📦 JSON",
        "application/toml" | "application/yaml" => "⚙️ Config",
        "application/x-sh" => "🐚 Shell",
        _ => "📄 Text",
    }
}

/// Shows "Open in Workflow Editor" above any code block holding ComfyUI
/// workflow JSON (API or UI format), whatever language tag the fence has.
/// Real-world exports may contain raw control chars inside strings, so the
/// same repair pass as the importer runs first.
fn render_comfy_workflow_button(ui: &mut egui::Ui, code: &str, comfy_workflow_request: &mut Option<String>) {
    // Cheap check: avoids a repair pass + JSON parse on every non-JSON block, every frame.
    let trimmed = code.trim();
    if !trimmed.starts_with('{') {
        return;
    }
    let sanitized = crate::workflow_editor::sanitize_json_control_chars(trimmed);
    let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&sanitized) else { return };
    if !crate::workflow_editor::is_comfy_workflow_json(&parsed) {
        return;
    }

    ui.horizontal(|ui| {
        crate::emoji_render::emoji_label(ui, "🧩 ComfyUI Workflow");
        if ui.button("Open in Workflow Editor").clicked() {
            *comfy_workflow_request = Some(sanitized.clone());
        }
    });
    ui.add_space(4.0);
}

// ---------------------------------------------------------------------------
// File buttons: open in editor / open in merge tool
// ---------------------------------------------------------------------------

fn render_file_buttons(
    ui: &mut egui::Ui,
    msg_id: i64,
    chunk_idx: usize,
    path: &Path,
    display_path: &str,
    autocorrected: bool,
    code: &str,
    project_root: &Option<PathBuf>,
    active_realm: &Option<ActiveRealm>,
    op_tx: &Sender<FileOpMsg>,
) {
    ui.spacing_mut().item_spacing.x = 6.0;

    let mut btn_text = format!("📄 {}", display_path);
    let mut tooltip = "Open file in editor".to_string();
    if autocorrected {
        btn_text.push_str(" ⚠️");
        let rel_path_str = inforno_core::realm::get_relative_path(active_realm, project_root, path);
        tooltip = format!("File path autocorrected to:\n{}", rel_path_str);
    }

    let open_resp = SplitButton::new(btn_text)
        .id_salt(format!("open_btn_{}_{}", msg_id, chunk_idx))
        .main_tooltip(tooltip)
        .arrow_tooltip(t!("right_button_tooltip"))
        .show(ui);
    if open_resp.main_clicked {
        send_open_editor(op_tx, path.to_path_buf(), FileOp::OpenEditor);
    }
    if open_resp.arrow_clicked {
        send_open_editor(op_tx, path.to_path_buf(), FileOp::OpenEditorRight);
    }

    let merge_resp = SplitButton::new(t!("open_in_merge_tool_btn"))
        .id_salt(format!("merge_btn_{}_{}", msg_id, chunk_idx))
        .main_tooltip(t!("open_in_merge_tool_tooltip"))
        .arrow_tooltip(t!("right_button_tooltip"))
        .show(ui);
    if merge_resp.main_clicked {
        send_merge_request(op_tx, path, code, false);
    }
    if merge_resp.arrow_clicked {
        send_merge_request(op_tx, path, code, true);
    }
}

fn send_open_editor(op_tx: &Sender<FileOpMsg>, path: PathBuf, op: FileOp) {
    let _ = op_tx.send(FileOpMsg {
        op,
        cancelled: false,
        path: Some(path),
        ..Default::default()
    });
}

/// Pre-merges the snippet into the current file (LLM search/replace diffs,
/// then single-function splicing, then the raw snippet as a last resort)
/// and asks the GUI to open the merge tool on the result.
fn send_merge_request(op_tx: &Sender<FileOpMsg>, path: &Path, code: &str, right_pane: bool) {
    let original_content = std::fs::read_to_string(path).unwrap_or_default();
    let clean_snippet = strip_snippet_header(code);

    let merged = bulat::engine::apply_llm_diffs(&original_content, &clean_snippet)
        .or_else(|| bulat::engine::try_splice_snippet(&original_content, &clean_snippet));
    let right_content = merged.unwrap_or(clean_snippet);

    let _ = op_tx.send(FileOpMsg {
        op: if right_pane { FileOp::OpenMergeRight } else { FileOp::OpenMerge },
        path: Some(path.to_path_buf()),
        left_content: Some(original_content),
        right_content: Some(right_content),
        ..Default::default()
    });
}

/// Removes file-path metadata an LLM tends to put at the top of a snippet:
/// a `--- File: x.rs ---` style line, a bare `---` divider, or a stray
/// filename line sitting right above a `<<<<` diff marker.
fn strip_snippet_header(code: &str) -> String {
    static RE_STRIP_FILE: OnceLock<Regex> = OnceLock::new();
    let re_strip = RE_STRIP_FILE.get_or_init(|| {
        Regex::new(r"(?im)^[ \t]*(?://|/\*|#)?[ \t]*(?:---[ \t]*(?:File:)?[ \t]*|File:[ \t]*)[a-z0-9_/\.\-]+\.[a-z]+[ \t]*(?:---|\*/)?\r?\n?").unwrap()
    });

    let stripped_owned = re_strip.replace(code, "");
    let mut clean_snippet = stripped_owned.as_ref();

    // Malformed header: an empty "---" or similar line
    let trimmed = clean_snippet.trim_start();
    if trimmed.starts_with("---") || trimmed.starts_with("// ---") || trimmed.starts_with("/* ---") {
        clean_snippet = match clean_snippet.find('\n') {
            Some(nl) => &clean_snippet[nl + 1..],
            None => "", // single-line snippet removed entirely
        };
    }

    // Standalone filename line right before the <<<< marker
    let mut lines = clean_snippet.lines().filter(|l| !l.trim().is_empty());
    if let (Some(first_line), Some(second_line)) = (lines.next(), lines.next()) {
        let looks_like_filename = first_line.contains('.')
            && !first_line.contains('{')
            && !first_line.contains('(')
            && !first_line.contains(';');
        if second_line.trim_start().starts_with("<<<<") && looks_like_filename {
            // Slice by pointer offset so the diff marker's indentation is kept intact
            let offset = (second_line.as_ptr() as usize) - (clean_snippet.as_ptr() as usize);
            clean_snippet = &clean_snippet[offset..];
        }
    }

    clean_snippet.to_string()
}

// ---------------------------------------------------------------------------
// Inline diff: embedded DiffApp with Save / Quick Merge
// ---------------------------------------------------------------------------

/// Line endings normalized and trimmed, for comparing blocks loosely.
fn norm(s: &str) -> String {
    s.replace("\r\n", "\n").trim().to_string()
}

/// `text` with its only occurrence of `from` replaced by `to`; None when
/// `from` is missing or occurs more than once.
fn swap_unique(text: &str, from: &str, to: &str) -> Option<String> {
    let mut found = text.match_indices(from);
    let (idx, _) = found.next()?;
    if found.next().is_some() {
        return None;
    }
    Some(format!("{}{}{}", &text[..idx], to, &text[idx + from.len()..]))
}

/// The file with a merged edit taken back out (its REPLACE text swapped back
/// for SEARCH), if that can be done cleanly: the edit is applied and REPLACE
/// occurs exactly once, matched the way a merge writes it (verbatim, or with
/// normalized line endings). A pure deletion leaves nothing to anchor the
/// restored text to, so it can't be undone.
fn undo_merge_text(file: &str, search_block: &str, replace_block: &str) -> Option<String> {
    if norm(replace_block).is_empty() || !bulat::engine::is_diff_applied(file, search_block, replace_block) {
        return None;
    }
    match file.matches(replace_block).count() {
        1 => swap_unique(file, replace_block, search_block),
        0 => swap_unique(&file.replace("\r\n", "\n"), &norm(replace_block), &norm(search_block)),
        _ => None, // ambiguous
    }
}


struct DiffLocation {
    search_found: bool,
    replace_found: bool,
    /// 0-based line in the file where the found block starts.
    line_offset: usize,
    search_norm: String,
    replace_norm: String,
}

/// Finds `block` in the file, first verbatim, then with normalized line
/// endings. Returns the 0-based line where it starts.
fn find_line(original: &str, orig_norm: &str, block: &str, block_norm: &str) -> Option<usize> {
    let line_of = |text: &str, idx: usize| text[..idx].matches('\n').count();
    if let Some(idx) = original.find(block) {
        return Some(line_of(original, idx));
    }
    if !block_norm.is_empty() {
        if let Some(idx) = orig_norm.find(block_norm) {
            return Some(line_of(orig_norm, idx));
        }
    }
    None
}

/// Checks whether the SEARCH block (still to apply) or the REPLACE block
/// (already applied) is present in the live file.
fn locate_diff(original: &str, search_block: &str, replace_block: &str) -> DiffLocation {
    let orig_norm = original.replace("\r\n", "\n");
    let mut loc = DiffLocation {
        search_found: false,
        replace_found: false,
        line_offset: 0,
        search_norm: norm(search_block),
        replace_norm: norm(replace_block),
    };

    if !search_block.is_empty() {
        if let Some(line) = find_line(original, &orig_norm, search_block, &loc.search_norm) {
            loc.search_found = true;
            loc.line_offset = line;
        }
    }

    if !replace_block.is_empty() {
        if let Some(line) = find_line(original, &orig_norm, replace_block, &loc.replace_norm) {
            loc.replace_found = true;
            if !loc.search_found {
                loc.line_offset = line;
            }
        }

        // Only the replace block matched: too generic to assume it's our applied snippet
        if loc.replace_found && !loc.search_found
            && loc.replace_norm.lines().count() <= 1 && loc.replace_norm.len() <= 20
        {
            loc.replace_found = false;
        }
    }

    loc
}

/// What the live file says about one inline diff snippet.
struct DiffStatus {
    search_block: String,
    replace_block: String,
    loc: DiffLocation,
    applied: bool,
    can_undo: bool,
}

/// `status` is None when the snippet holds no `<<<< ==== >>>>` diff.
#[derive(Clone)]
struct CachedDiffStatus {
    file_stamp: Option<(Option<std::time::SystemTime>, u64)>,
    status: Option<std::sync::Arc<DiffStatus>>,
}

/// The diff status of a code block, recomputed only when the file's
/// modification time or size changes (or the snippet does, as it's part of
/// the key), so the frame loop doesn't read and search the file every frame.
fn inline_diff_status(ui: &egui::Ui, code: &str, path: &Path) -> Option<std::sync::Arc<DiffStatus>> {
    let id = egui::Id::new(("inline_diff_status", path, code));
    let file_stamp = std::fs::metadata(path).ok().map(|m| (m.modified().ok(), m.len()));
    if let Some(cached) = ui.data(|d| d.get_temp::<CachedDiffStatus>(id)) {
        if cached.file_stamp == file_stamp {
            return cached.status;
        }
    }

    let status = bulat::engine::extract_raw_diff_blocks(code).map(|(search_block, replace_block)| {
        let file = std::fs::read_to_string(path).unwrap_or_default();
        std::sync::Arc::new(DiffStatus {
            loc: locate_diff(&file, &search_block, &replace_block),
            applied: bulat::engine::is_diff_applied(&file, &search_block, &replace_block),
            can_undo: undo_merge_text(&file, &search_block, &replace_block).is_some(),
            search_block,
            replace_block,
        })
    });
    ui.data_mut(|d| d.insert_temp(id, CachedDiffStatus { file_stamp, status: status.clone() }));
    status
}

/// Right side of a code block header. If the snippet is a single
/// `<<<< ==== >>>>` diff that matches the live file, this mounts an
/// embedded DiffApp for the chunk and shows Save / Quick Merge, or
/// "✔ Merged" once the change is on disk.
fn render_inline_diff_controls(
    ui: &mut egui::Ui,
    msg_ui: &mut ChatMsgUi,
    chunk_idx: usize,
    code: &str,
    path: &Path,
    project_root: &Option<PathBuf>,
    active_realm: &Option<ActiveRealm>,
) {
    let Some(status) = inline_diff_status(ui, code, path) else { return };
    let loc = &status.loc;
    if !loc.search_found && !loc.replace_found {
        return;
    }

    msg_ui.inline_diffs.entry(chunk_idx).or_insert_with(|| {
        let mut app = bulat::DiffApp::new(status.search_block.clone(), status.replace_block.clone())
            .with_line_offset(loc.line_offset);
        app.embedded = true; // full height inside the chat
        app.left_filepath = Some(inforno_core::realm::get_relative_path(active_realm, project_root, path));
        app
    });

    if loc.replace_found && status.applied {
        msg_ui.inline_diffs_saved.insert(chunk_idx, true);
    }

    let is_saved = *msg_ui.inline_diffs_saved.entry(chunk_idx).or_insert(false);

    ui.add_space(10.0);
    if is_saved {
        if status.can_undo {
            let undo = ui.small_button("↶ Undo")
                .on_hover_text("Put the original text back. Offered only while the merged text is in the file exactly once.");
            if undo.clicked() {
                // Built from the file as it is now, not as it was when cached
                let file = std::fs::read_to_string(path).unwrap_or_default();
                match undo_merge_text(&file, &status.search_block, &status.replace_block) {
                    Some(undone) => match std::fs::write(path, undone) {
                        Ok(()) => {
                            msg_ui.inline_diffs_saved.insert(chunk_idx, false);
                        }
                        Err(e) => eprintln!("Failed to undo merge: {}", e),
                    },
                    None => eprintln!("Undo skipped: the file changed and the merge can no longer be undone cleanly."),
                }
            }
        }
        ui.label(RichText::new("✔ Merged").color(egui::Color32::GREEN));
        return;
    }

    let save_left = ui.button("💾 Save").on_hover_text("Save the manually merged code (Left side).").clicked();
    let save_right = ui.button("⚡ Quick Merge").on_hover_text("Instantly apply the LLM's full replacement (Right side).").clicked();

    if save_left || save_right {
        let diff_app = &msg_ui.inline_diffs[&chunk_idx];
        let replacement = if save_right {
            diff_app.right_code_real.clone()
        } else {
            diff_app.left_code_real.clone()
        };
        if quick_merge_to_disk(path, &status.search_block, replacement) {
            msg_ui.inline_diffs_saved.insert(chunk_idx, true);
        }
    }
}

/// Writes `replacement` over `search_block` in the file on disk, re-reading
/// it first in case it changed since the last frame. Returns true when the
/// file now contains the change, either written now or already present.
fn quick_merge_to_disk(path: &Path, search_block: &str, mut replacement: String) -> bool {
    let latest = std::fs::read_to_string(path).unwrap_or_default();

    // DiffApp::new appends one trailing newline; drop it so the inline
    // merge doesn't insert an empty line.
    if replacement.ends_with('\n') {
        replacement.pop();
        if replacement.ends_with('\r') {
            replacement.pop();
        }
    }

    // Prevent double-apply on fast clicks
    let search_norm = norm(search_block);
    if bulat::engine::is_diff_applied(&latest, search_block, &replacement) {
        return true;
    }

    let new_text = if let Some(idx) = latest.find(search_block) {
        format!("{}{}{}", &latest[..idx], replacement, &latest[idx + search_block.len()..])
    } else {
        let orig_norm = latest.replace("\r\n", "\n");
        match orig_norm.find(&search_norm) {
            Some(idx) => format!(
                "{}{}{}",
                &orig_norm[..idx],
                replacement.replace("\r\n", "\n"),
                &orig_norm[idx + search_norm.len()..]
            ),
            None => {
                eprintln!("Quick merge failed: Target block modified or no longer matches.");
                return false;
            }
        }
    };

    match std::fs::write(path, new_text) {
        Ok(()) => true,
        Err(e) => {
            eprintln!("Failed to quick merge: {}", e);
            false
        }
    }
}
