use egui::{Margin, RichText, Stroke};
use egui_commonmark::CommonMarkViewer;
use rust_i18n::t;
use crate::common::Attachment;
use crate::gui::math_render::compile_math_to_svg_embedded;
use regex::Regex;
use std::sync::OnceLock;
use base64::{Engine as _, engine::general_purpose::STANDARD};

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
    let re = RE_RUST_BLOCK.get_or_init(|| {
        Regex::new(r"(?ms)^[ \t]{0,3}\x60{3}(?:rust|rs)[ \t]*\r?\n(.*?)\r?\n[ \t]{0,3}\x60{3}[ \t]*$").unwrap()
    });

    // Broadened regex: Matches strings ending in ".rs" ANYWHERE in the text.
    let re_filepath = RE_FILEPATH.get_or_init(|| {
        Regex::new(r"(?i)([a-z0-9_/\.\-]+\.rs)").unwrap()
    });

    let mut chunks = Vec::new();
    let mut last_end = 0;

    // NEW: State variable to remember the filename across multiple code blocks
    let mut current_filepath: Option<String> = None;

    for caps in re.captures_iter(text) {
        let full_match = caps.get(0).unwrap();
        let code_match = caps.get(1).unwrap();

        if full_match.start() > last_end {
            let md_text = &text[last_end..full_match.start()];
            chunks.push(ContentChunk::Markdown(md_text));

            // Search the entire markdown chunk for filepaths
            let mut found_in_chunk = None;
            for path_caps in re_filepath.captures_iter(md_text) {
                // Keep overwriting so we end up with the LAST match (closest to the code block)
                found_in_chunk = Some(path_caps.get(1).unwrap().as_str().to_string());
            }

            // If we found a filename in this intermediate text, update our active tracker.
            // If we didn't, `current_filepath` retains whatever file it was already tracking!
            if found_in_chunk.is_some() {
                current_filepath = found_in_chunk;
            }
        }

        chunks.push(ContentChunk::RustCode {
            code: code_match.as_str(),
            // Clone the persistent state into this chunk
            filepath: current_filepath.clone(),
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
    let max_w = effective_width.clamp(400.0, 800.0);

    let scroll_area = egui::ScrollArea::horizontal();

    scroll_area.show(ui, |ui| {
        ui.set_max_width(max_w);
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
                    render_msg_header(ui, msg_ui, &msg.msg_role.to_string(), msg);
                    render_msg_content(ui, cache, msg, msg_ui, (max_w - 20.0) as usize, math_cache.clone(),
                        project_root, active_merge);

                    // --- Render JSON Attachments as Spoilers or Images ---
                    if let Some(details_json) = &msg.details {
                        if let Ok(attachments) = serde_json::from_str::<Vec<Attachment>>(details_json) {
                            if !attachments.is_empty() {
                                ui.add_space(8.0);
                                egui::CollapsingHeader::new(egui::RichText::new(format!("📎 {} Attached Files", attachments.len())).strong())
                                    .id_salt(format!("details_collapse_{}", msg.id))
                                    .show(ui, |ui| {
                                        // Iterate through the array of attachments
                                        for att in attachments {
                                            egui::CollapsingHeader::new(egui::RichText::new(&att.filename).weak())
                                                .id_salt(format!("att_collapse_{}_{}", msg.id, att.filename))
                                                .show(ui, |ui| {
                                                    // DIFFERENTIATE TEXT VS IMAGE
                                                    if att.mime_type.starts_with("image/") {
                                                        let ext = match att.mime_type.as_str() {
                                                            "image/jpeg" | "image/jpg" => ".jpg",
                                                            "image/webp" => ".webp",
                                                            "image/gif" => ".gif",
                                                            _ => ".png",
                                                        };

                                                        let uri = format!("bytes://{}_{}{}", msg.id, att.filename, ext);

                                                        let mut cache_map = math_cache.borrow_mut();
                                                        let image_bytes = cache_map.entry(uri.clone()).or_insert_with(|| {
                                                            STANDARD.decode(att.content.trim()).unwrap_or_default().into()
                                                        });

                                                        if !image_bytes.is_empty() {
                                                            // 1. Show the byte size so we mathematically KNOW the data is there
                                                            ui.label(egui::RichText::new(format!("📸 Loaded: {} bytes", image_bytes.len())).weak().small());

                                                            ui.ctx().include_bytes(uri.clone(), image_bytes.clone());

                                                            let source = egui::ImageSource::Bytes {
                                                                uri: uri.clone().into(),
                                                                bytes: egui::load::Bytes::Shared(image_bytes.clone()),
                                                            };

                                                            // 2. Explicitly poll the texture to see exactly what state the engine is in
                                                            match ui.ctx().try_load_texture(&uri, egui::TextureOptions::LINEAR, egui::SizeHint::default()) {
                                                                Ok(egui::load::TexturePoll::Pending { .. }) => {
                                                                    ui.horizontal(|ui| {
                                                                        ui.spinner();
                                                                        ui.label("Decoding image...");
                                                                    });
                                                                }
                                                                Ok(egui::load::TexturePoll::Ready { texture }) => {
                                                                    // 3. Force a strict size so the layout CANNOT collapse to 0x0
                                                                    let size = texture.size;
                                                                    let max_w = 300.0_f32;
                                                                    let scale = if size.x > max_w { max_w / size.x } else { 1.0 };

                                                                    ui.add(egui::Image::new(source).fit_to_exact_size(size * scale));
                                                                }
                                                                Err(err) => {
                                                                    ui.colored_label(ui.visuals().error_fg_color, format!("Texture Error: {}", err));
                                                                }
                                                            }
                                                        } else {
                                                            ui.colored_label(ui.visuals().error_fg_color, "Failed to decode image data.");
                                                        }
                                                    } else {
                                                        // Standard Text Rendering
                                                        egui::ScrollArea::vertical()
                                                            .id_salt(format!("att_scroll_{}_{}", msg.id, att.filename))
                                                            .max_height(300.0)
                                                            .show(ui, |ui| {
                                                                let mut code = att.content.as_str();
                                                                ui.add(
                                                                    egui::TextEdit::multiline(&mut code)
                                                                        .desired_width(f32::INFINITY)
                                                                        .font(egui::TextStyle::Monospace)
                                                                        .interactive(false)
                                                                );
                                                            });
                                                    }
                                                });
                                        }
                                    });
                            }
                        }
                    }
                });
            });
            ui.allocate_space(egui::vec2(ui.available_width(), 0.0));
        });
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
        let scroll_area = egui::ScrollArea::horizontal()
            .id_salt(format!("assistant_message_scroll_{}", msg.id));

        scroll_area.show(ui, |ui| {
            ui.set_max_width(item_width - 25.0);

            let label = format!("{}:", msg.name.as_deref().unwrap_or("assistant"));
            render_msg_header(ui, msg_ui, &label, msg);

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

            let content_width = (item_width - 25.0).max(100.0);
            render_msg_content(ui, cache, msg, msg_ui, content_width as usize, math_cache,
                project_root, active_merge);
        });
    });
}

fn render_msg_header(
    ui: &mut egui::Ui,
    msg_ui: &mut ChatMsgUi,
    label: &str,
    msg: &ChatMsg, // Changed from msg_id: i64 to msg: &ChatMsg
) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(label).strong());

        #[cfg(debug_assertions)]
        ui.label(RichText::new(format!("msg_id: {}", msg.id)).strong());

        ui.with_layout(
            egui::Layout::right_to_left(egui::Align::Center),
            |ui| {
                // Add the Copy button first (it will be on the far right)
                if ui.button("🗐").on_hover_text("Copy raw message to clipboard").clicked() {
                    ui.ctx().copy_text(msg.content.clone());
                }

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

                    // Wrap the viewer in a unique egui ID context
ui.push_id(format!("md_{}_{}", msg.id, i), |ui| {
                        CommonMarkViewer::new()
                            .max_image_width(Some(max_image_width))
                            .render_math_fn(Some(&mut move |ui, math, is_inline| {
                                let mut cache_map = local_math_cache.borrow_mut();
                                let svg_bytes = cache_map.entry(math.to_string()).or_insert_with(|| {
                                    let bytes = compile_math_to_svg_embedded(math, is_inline).unwrap_or_default();
                                    bytes.into()
                                });

                                // --- NEW: Graceful fallback for failed math compilation ---
                                if svg_bytes.is_empty() {
                                    // Render the raw LaTeX text so it isn't lost, using a warning color
                                    let raw_math = if is_inline {
                                        format!("${}$", math)
                                    } else {
                                        format!("$${}$$", math)
                                    };
                                    ui.label(egui::RichText::new(raw_math)
                                        .monospace()
                                        .color(ui.visuals().warn_fg_color));

                                    // Abort so we don't try to render an empty image!
                                    return;
                                }
                                // ----------------------------------------------------------

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
                    });
                }

                ContentChunk::RustCode { code, filepath } => {
                    let mut code_buffer = code.to_string();
                    let num_lines = code_buffer.lines().count().max(1);

                    ui.add_space(6.0);

                    // --- HEADER: Path, Merge Tool, and Copy Button ---
                    ui.horizontal(|ui| {
                        // Left side: Filepath and Merge button
                        if let Some(path) = filepath {
                            if let Some(root) = project_root {
                                let full_path = root.join(&path);
                                ui.label(egui::RichText::new(format!("📄 {}", path)).strong());
                                if ui.button("🛠 Open in Merge Tool").clicked() {
                                    let original_content = std::fs::read_to_string(&full_path)
                                        .unwrap_or_else(|_| String::new());

                                    // Attempt to seamlessly splice the function
                                    // If splicing fails (e.g. multiple functions, function not found),
                                    // it elegantly falls back to opening the raw snippet.
                                    let right_content = try_splice_snippet(&original_content, &code_buffer)
                                        .unwrap_or_else(|| code_buffer.clone());

                                    *active_merge = Some(crate::gui::ActiveMerge {
                                        app: crate::bulat::DiffApp::new(
                                            original_content,
                                            right_content
                                        ),
                                        path: full_path,
                                    });
                                }                            } else {
                                ui.label(egui::RichText::new(format!("📄 {}", path)).strong());
                            }
                        } else {
                            ui.label(egui::RichText::new("🦀 Rust").weak());
                        }

                        // Right side: Copy Button
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.button("🗐").on_hover_text("Copy to clipboard").clicked() {
                                ui.ctx().copy_text(code.to_string());
                            }
                        });
                    });

                    CodeEditor::default()
                        .id_source(format!("code_block_{}_{}", msg.id, i))
                        .with_theme(ColorTheme::SV)
                        .with_syntax(Syntax::rust())
                        .with_numlines(false)
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

// when LLM sends only one function, we want to pre-merge it with the target
// file before sending it to the GUI merge tool
fn find_function_spans(code: &str, fn_name: &str) -> Vec<(usize, usize)> {
    // Looks for: fn my_function_name( or fn my_function_name<
    let pattern = format!(r"(?m)^[ \t]*(?:pub\s+)?(?:async\s+)?fn\s+{}(?:\s|<|\()", regex::escape(fn_name));
    let re = match Regex::new(&pattern) {
        Ok(r) => r,
        Err(_) => return vec![],
    };

    let mut spans = Vec::new();
    for mat in re.find_iter(code) {
        let start_idx = mat.start();
        let mut brace_count = 0;
        let mut found_first_brace = false;

        let mut chars = code[start_idx..].char_indices().peekable();
        let mut in_string = false;
        let mut in_char = false;
        let mut in_comment = false;
        let mut in_multi_comment = false;

        // A lightweight lexer to safely count braces without tripping on strings/comments
        while let Some((i, c)) = chars.next() {
            if in_comment {
                if c == '\n' { in_comment = false; }
                continue;
            }
            if in_multi_comment {
                if c == '*' {
                    if let Some(&(_, '/')) = chars.peek() {
                        chars.next();
                        in_multi_comment = false;
                    }
                }
                continue;
            }
            if in_string {
                if c == '\\' { chars.next(); } // skip escaped char
                else if c == '"' { in_string = false; }
                continue;
            }
            if in_char {
                if c == '\\' { chars.next(); }
                else if c == '\'' { in_char = false; }
                continue;
            }

            match c {
                '"' => in_string = true,
                '\'' => in_char = true,
                '/' => {
                    if let Some(&(_, '/')) = chars.peek() {
                        in_comment = true;
                        chars.next();
                    } else if let Some(&(_, '*')) = chars.peek() {
                        in_multi_comment = true;
                        chars.next();
                    }
                },
                '{' => {
                    brace_count += 1;
                    found_first_brace = true;
                },
                '}' => {
                    brace_count -= 1;
                    if found_first_brace && brace_count == 0 {
                        spans.push((start_idx, start_idx + i + 1));
                        break;
                    }
                },
                _ => {}
            }
        }
    }
    spans
}

fn try_splice_snippet(original: &str, snippet: &str) -> Option<String> {
    static RE_FN: OnceLock<Regex> = OnceLock::new();
    let re_fn = RE_FN.get_or_init(|| Regex::new(r"(?m)^[ \t]*(?:pub\s+)?(?:async\s+)?fn\s+([a-zA-Z0-9_]+)(?:\s|<|\()").unwrap());

    let mut fn_names = Vec::new();
    for caps in re_fn.captures_iter(snippet) {
        if let Some(name) = caps.get(1) {
            fn_names.push(name.as_str());
        }
    }

    // Safety check: Only attempt splice if there is EXACTLY one function in the snippet
    if fn_names.len() == 1 {
        let fn_name = fn_names[0];

        let orig_spans = find_function_spans(original, fn_name);
        let snip_spans = find_function_spans(snippet, fn_name);

        println!("orig_spans: {:?}", orig_spans);
        println!("snip_spans: {:?}", snip_spans);

        // Safety check: Only splice if the function name is completely unique in BOTH strings
        // (This prevents accidentally overwriting the wrong `fn new()` in a file with multiple structs)
        if orig_spans.len() == 1 && snip_spans.len() == 1 {
            let (orig_start, orig_end) = orig_spans[0];
            let (snip_start, snip_end) = snip_spans[0];

            let spliced_function = &snippet[snip_start..snip_end];

            let mut new_code = String::with_capacity(original.len() + spliced_function.len());
            new_code.push_str(&original[..orig_start]);
            new_code.push_str(spliced_function);
            new_code.push_str(&original[orig_end..]);

            return Some(new_code);
        }
    }
    None
}
