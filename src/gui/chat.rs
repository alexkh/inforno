use egui::{Margin, RichText, Stroke};
use egui_commonmark::CommonMarkViewer;
use rust_i18n::t;
use crate::gui::math_render::compile_math_to_svg_embedded;
use regex::Regex;
use std::sync::OnceLock;

use crate::bulat::editor::{CodeEditor, Syntax, ColorTheme};

use crate::{
    common::{
        ChatMsg, ChatMsgUi, MsgRole,
    },
    gui::{State},
};

// --- Markdown Chunker ---

enum ContentChunk<'a> {
    Markdown(&'a str),
    RustCode {
        code: &'a str,
        filepath: Option<String>,
    },
}

fn parse_chunks(text: &str) -> Vec<ContentChunk> {
    static RE_RUST_BLOCK: OnceLock<Regex> = OnceLock::new();
    static RE_FILEPATH: OnceLock<Regex> = OnceLock::new();

    // (?m) multi-line mode: ^ and $ match line boundaries.
    // (?s) dot-all mode: . matches newlines.
    // This strictly enforces that both the opening and closing triple backticks
    // are on their own separate lines, ignoring inline backticks.
    let re = RE_RUST_BLOCK.get_or_init(|| {
        Regex::new(r"(?ms)^[ \t]{0,3}\x60{3}(?:rust|rs)[ \t]*\r?\n(.*?)\r?\n[ \t]{0,3}\x60{3}[ \t]*$").unwrap()
    });

    // Matches strings ending in ".rs", ignoring trailing markdown bold/italics/backticks or colons.
    let re_filepath = RE_FILEPATH.get_or_init(|| {
        Regex::new(r"(?im)([a-z0-9_/\.\-]+\.rs)\s*[\*`:]*\s*$").unwrap()
    });

    let mut chunks = Vec::new();
    let mut last_end = 0;

    for caps in re.captures_iter(text) {
        let full_match = caps.get(0).unwrap();
        let code_match = caps.get(1).unwrap();

        let mut filepath = None;

        if full_match.start() > last_end {
            let md_text = &text[last_end..full_match.start()];
            chunks.push(ContentChunk::Markdown(md_text));

            // NEW: Try to extract a file path from the very end of this markdown chunk
            if let Some(path_caps) = re_filepath.captures(md_text) {
                filepath = Some(path_caps.get(1).unwrap().as_str().to_string());
            }
        }

        chunks.push(ContentChunk::RustCode {
            code: code_match.as_str(),
            filepath,
        });

        last_end = full_match.end();
    }

    if last_end < text.len() {
        chunks.push(ContentChunk::Markdown(&text[last_end..]));
    }

    chunks
}


// --- Main Entry Point ---

pub fn ui_chat(ctx: &egui::Context, state: &mut State) {
    egui::CentralPanel::default()
    //.stick_to_the_bottom(true)
    .show(ctx, |ui| {
        if state.is_modal_open {
            ui.disable();
        }
        egui::ScrollArea::vertical()
        .stick_to_bottom(true)
        .id_salt("chat_scroll_main")
        // Fix for scrolling behavior: preventing auto-shrink ensures the
        // scroll area tries to fill the parent, helping capture input.
        .auto_shrink([false, false])
        .show(ui, |ui| {
            render_chat_messages(ui, state, ui.available_width());
        });
    });
}

// --- Message Rendering ---

fn render_chat_messages(ui: &mut egui::Ui, state: &mut State, total_width: f32) {
    let msg_ui_map = &mut state.chat_msg_ui;
    let cache = &mut state.common_mark_cache;
    let msg_pool = &state.chat.msg_pool;

    let project_root = &state.project_root;
    let active_merge = &mut state.active_merge;

    // We clone the Rc pointer here (very cheap)
    let math_cache = state.math_cache.clone();

    if msg_pool.is_empty() {
           egui::Frame::default()
        .stroke(Stroke { width: 1.0, color: ui.visuals().hyperlink_color })
        .outer_margin(Margin { top: 0, right: 5, bottom: 0, left: 5 })
        .inner_margin(10.0)
        .corner_radius(5.0)
        .fill(ui.visuals().faint_bg_color)
        .show(ui, |ui| {
            ui.heading(t!("welcome_tour"));
        });
        return;
    }

    let active_agent_ind = 0;
    let mut assistant_batch: Vec<i64> = Vec::new();

    if let Some(agent) = state.chat.agents.get(active_agent_ind) {
        for &msg_id in &agent.msg_ids {
            if let Some(msg) = msg_pool.get(&msg_id) {
                match msg.msg_role {
                    MsgRole::User | MsgRole::System => {
                        if !assistant_batch.is_empty() {
                            // Pass a clone of the cache pointer
                            render_assistant_grid(ui, cache, msg_pool,
                                msg_ui_map, &assistant_batch, total_width, math_cache.clone(),
                            project_root, &mut *active_merge);
                            assistant_batch.clear();
                        }

                        let msg_ui = msg_ui_map.entry(msg_id)
                                .or_insert(ChatMsgUi::default());
                        // Pass a clone of the cache pointer
                        render_user_msg(ui, cache, msg, msg_ui, total_width, math_cache.clone(),
                            project_root, &mut *active_merge);
                    }
                    _ => {
                        assistant_batch.push(msg_id);
                    }
                }
            }
        }

        if !assistant_batch.is_empty() {
            // Pass a clone of the cache pointer
            render_assistant_grid(ui, cache, msg_pool, msg_ui_map,
                    &assistant_batch, total_width, math_cache.clone(),
                    project_root, &mut *active_merge);
        }
    }
}

fn render_assistant_grid(
    ui: &mut egui::Ui,
    cache: &mut egui_commonmark::CommonMarkCache,
    msg_pool: &std::collections::HashMap<i64, ChatMsg>,
    msg_ui_map: &mut std::collections::HashMap<i64, ChatMsgUi>,
    batch_ids: &[i64],
    total_width: f32,
    math_cache: std::rc::Rc<std::cell::RefCell<std::collections::HashMap<String, std::sync::Arc<[u8]>>>>,
    project_root: &Option<std::path::PathBuf>,
    active_merge: &mut Option<crate::gui::ActiveMerge>,
) {
    let effective_width = total_width - 38.0;
    let item_min_width = 400.0;
    let item_max_width = 900.0;
    let spacing = 10.0;

    let max_cols = (((effective_width + spacing) / (item_min_width + spacing)).floor() as usize).max(1);
    let divisor = if batch_ids.len() < max_cols { batch_ids.len() as f32 } else { max_cols as f32 };

    let total_spacing = spacing * (divisor - 1.0);
    let rounding_buffer = divisor * 2.0;

    let raw_item_width = (effective_width - total_spacing - rounding_buffer) / divisor;
    let item_width = raw_item_width.clamp(item_min_width, item_max_width);
    let cols = max_cols;

    for (row_idx, row_ids) in batch_ids.chunks(cols).enumerate() {
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = spacing;
            for &msg_id in row_ids {
                if let Some(msg) = msg_pool.get(&msg_id) {
                    let msg_ui = msg_ui_map.entry(msg_id).or_insert(ChatMsgUi::default());

                    ui.allocate_ui_with_layout(
                        egui::vec2(item_width, 0.0),
                        egui::Layout::top_down(egui::Align::Min),
                        |ui| {
                            ui.set_width(item_width);
                            render_assistant_msg(
                                    ui, cache, msg, msg_ui, item_width, math_cache.clone(),
                                    project_root, &mut *active_merge);
                        }
                    );
                }
            }
            ui.allocate_space(egui::vec2(ui.available_width(), 0.0));
        });

        if row_idx < (batch_ids.len().div_ceil(cols) - 1) {
             ui.add_space(spacing);
        }
    }
    ui.add_space(15.0);
}

fn render_user_msg(
    ui: &mut egui::Ui,
    cache: &mut egui_commonmark::CommonMarkCache,
    msg: &ChatMsg,
    msg_ui: &mut ChatMsgUi,
    total_width: f32,
    math_cache: std::rc::Rc<std::cell::RefCell<std::collections::HashMap<String, std::sync::Arc<[u8]>>>>,
    project_root: &Option<std::path::PathBuf>,
    active_merge: &mut Option<crate::gui::ActiveMerge>,
) {
    let effective_width = total_width - 30.0;

    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            let max_w = effective_width.clamp(400.0, 800.0);
            ui.set_max_width(max_w);

            egui::Frame::default()
            .stroke(Stroke { width: 1.0, color: ui.visuals().strong_text_color() })
            .outer_margin(Margin { top: 0, right: 0, bottom: 15, left: 127 })
            .inner_margin(10.0)
            .corner_radius(5.0)
            .fill(ui.visuals().extreme_bg_color)
            .show(ui, |ui| {
                render_msg_header(ui, msg_ui, &msg.msg_role.to_string(), msg.id);
                render_msg_content(ui, cache, msg, msg_ui, (max_w - 20.0) as usize, math_cache,
                    project_root, active_merge);
            });
        });
        ui.allocate_space(egui::vec2(ui.available_width(), 0.0));
    });
}

fn render_assistant_msg(
    ui: &mut egui::Ui,
    cache: &mut egui_commonmark::CommonMarkCache,
    msg: &ChatMsg,
    msg_ui: &mut ChatMsgUi,
    item_width: f32,
    math_cache: std::rc::Rc<std::cell::RefCell<std::collections::HashMap<String, std::sync::Arc<[u8]>>>>,
    project_root: &Option<std::path::PathBuf>,
    active_merge: &mut Option<crate::gui::ActiveMerge>,
) {
    egui::Frame::default()
    .stroke(Stroke { width: 1.0, color: ui.visuals().hyperlink_color })
    .outer_margin(Margin::ZERO)
    .inner_margin(10.0)
    .corner_radius(5.0)
    .fill(ui.visuals().faint_bg_color)
    .show(ui, |ui| {
        let label = format!("{}:", msg.name.as_deref().unwrap_or("assistant"));
        render_msg_header(ui, msg_ui, &label, msg.id);

        if let Some(reasoning) = &msg.reasoning {
            if !reasoning.is_empty() {
                if msg_ui.show_raw {
                    ui.label(format!("{}: \n{}", t!("thought_process"), reasoning));
                    ui.separator();
                } else {
                    render_reasoning_block(ui, reasoning, msg.id);
                }
            }
        }

        let content_width = (item_width - 20.0).max(100.0);
        render_msg_content(ui, cache, msg, msg_ui, content_width as usize, math_cache,
            project_root, active_merge);
    });
}

fn render_msg_header(
    ui: &mut egui::Ui,
    msg_ui: &mut ChatMsgUi,
    label: &str,
    msg_id: i64,
) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(label).strong());

        #[cfg(debug_assertions)]
        ui.label(RichText::new(format!("msg_id: {}", msg_id)).strong());

        ui.with_layout(
            egui::Layout::right_to_left(egui::Align::Center),
            |ui| {
                if ui.toggle_value(&mut msg_ui.show_raw, "Raw").clicked() {
                    println!("Raw button clicked");
                }
            },
        );
    });
}

fn render_msg_content(
    ui: &mut egui::Ui,
    cache: &mut egui_commonmark::CommonMarkCache,
    msg: &ChatMsg,
    msg_ui: &ChatMsgUi,
    max_image_width: usize,
    math_cache: std::rc::Rc<std::cell::RefCell<std::collections::HashMap<String, std::sync::Arc<[u8]>>>>,
    project_root: &Option<std::path::PathBuf>,
    active_merge: &mut Option<crate::gui::ActiveMerge>,
) {
    if msg_ui.show_raw {
        ui.label(RichText::new(format!("{}", msg.content)).strong());
    } else {
        // Break the content into pieces
        let chunks = parse_chunks(&msg.content);

        for (i, chunk) in chunks.into_iter().enumerate() {
            match chunk {
                ContentChunk::Markdown(md_text) => {
                    // Only render markdown if there's actually text to render
                    if md_text.trim().is_empty() {
                        continue;
                    }

                    let local_math_cache = math_cache.clone();

                    CommonMarkViewer::new()
                        .max_image_width(Some(max_image_width))
                        .render_math_fn(Some(&mut move |ui, math, is_inline| {
                            let mut cache_map = local_math_cache.borrow_mut();
                            let svg_bytes = cache_map.entry(math.to_string()).or_insert_with(|| {
                                let bytes = compile_math_to_svg_embedded(math, is_inline).unwrap_or_default();
                                bytes.into()
                            });

                            let uri = format!("bytes://math_{}.svg", egui::Id::new(math).value());

                            let mut image = egui::Image::new(egui::ImageSource::Bytes {
                                uri: uri.into(),
                                bytes: egui::load::Bytes::Shared(svg_bytes.clone()),
                            });

                            image = image.tint(ui.visuals().text_color());

                            let egui_font_size = ui.text_style_height(&egui::TextStyle::Body);
                            let optical_adjustment = 0.8;
                            let scale_factor = (egui_font_size / 11.0) * optical_adjustment;

                            image = image.fit_to_original_size(scale_factor);

                            let actually_inline = is_inline && !math.contains("\\displaystyle");

                            if !actually_inline {
                                image = image.max_width(ui.available_width());
                            }

                            ui.add(image);
                        }))
                        .show(ui, cache, md_text);
                }

                ContentChunk::RustCode { code, filepath } => { // Update destructuring
                    let mut code_buffer = code.to_string();
                    let num_lines = code_buffer.lines().count().max(1);

                    ui.add_space(6.0);

                    // --- NEW: Merge Tool Button ---
                    if let Some(path) = filepath {
                        if let Some(root) = project_root {
                            let full_path = root.join(&path);
                            ui.horizontal(|ui| {
                                ui.label(egui::RichText::new(format!("📄 {}", path)).strong());
                                if ui.button("🛠 Open in Merge Tool").clicked() {
                                    let original_content = std::fs::read_to_string(&full_path)
                                        .unwrap_or_else(|_| String::new());

                                    // --- NEW: Wrap it in ActiveMerge ---
                                    *active_merge = Some(crate::gui::ActiveMerge {
                                        app: crate::bulat::DiffApp::new(
                                            original_content,
                                            code_buffer.clone()
                                        ),
                                        path: full_path,
                                    });
                                }
                            });
                            ui.add_space(4.0);
                        }
                    }

                    CodeEditor::default()
                        .id_source(format!("code_block_{}_{}", msg.id, i))
                        .with_theme(ColorTheme::SV)
                        .with_syntax(Syntax::rust())
                        .with_numlines(false)
                        //.with_rows(num_lines)
                        // Disable internal scroll so the parent chat window handles scrolling natively
                        .vscroll(false)
                        .show(ui, &mut code_buffer);

                    ui.add_space(6.0);
                }
            }
        }
    }
}

fn render_reasoning_block(ui: &mut egui::Ui, text: &str,
        id_salt: impl std::hash::Hash) {
    egui::CollapsingHeader::new(
        egui::RichText::new(t!("thought_process")).italics().weak()
    )
    .id_salt(id_salt)
    .default_open(true)
    .show(ui, |ui| {
        egui::Frame::new()
            .inner_margin(8.0)
            .show(ui, |ui| {
                ui.label(
                    egui::RichText::new(text)
                        .italics()
                        .color(ui.visuals().weak_text_color())
                );
            });
    });
    ui.add_space(10.0);
}
