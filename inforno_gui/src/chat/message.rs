//! User and assistant message frames: the assistant reply grid, message
//! headers (options menu, copy, raw toggle), reasoning blocks and
//! attachments.

use std::collections::HashMap;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use egui::{Margin, RichText, Stroke};
use rust_i18n::t;

use inforno_core::common::{Attachment, ChatMsg, MsgRole, format_deep_copy};

use crate::state::ChatMsgUi;

use super::content::render_msg_content;
use super::{ChatActions, MathCache, RenderCtx};

/// Lays out a run of consecutive assistant replies side by side, wrapping
/// into rows. Column count follows the pane width, item width the user's
/// max message width.
pub(super) fn render_assistant_grid(
    ui: &mut egui::Ui,
    ctx: &mut RenderCtx,
    actions: &mut ChatActions,
    msg_pool: &HashMap<i64, ChatMsg>,
    msg_ui_map: &mut HashMap<i64, ChatMsgUi>,
    batch_ids: &[i64],
) {
    let effective_width = ctx.total_width - 38.0;
    let item_min_width = 400.0;
    let item_max_width = ctx.max_msg_width;
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
                let Some(msg) = msg_pool.get(&msg_id) else { continue };
                let msg_ui = msg_ui_map.entry(msg_id).or_default();

                ui.allocate_ui_with_layout(
                    egui::vec2(item_width, 0.0),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        ui.set_width(item_width);
                        render_assistant_msg(ui, ctx, actions, msg, msg_ui, item_width);
                    }
                );
            }
            ui.allocate_space(egui::vec2(ui.available_width(), 0.0));
        });

        if row_idx < (batch_ids.len().div_ceil(cols) - 1) {
            ui.add_space(spacing);
        }
    }
    ui.add_space(15.0);
}

/// A user or system message, indented from the left, with its attachments.
pub(super) fn render_user_msg(
    ui: &mut egui::Ui,
    ctx: &mut RenderCtx,
    actions: &mut ChatActions,
    msg: &ChatMsg,
    msg_ui: &mut ChatMsgUi,
) {
    let left_offset = 127.0; // Matches the left outer margin of the user frame
    let right_padding = 30.0;

    // Subtract the offset from both bounds so frame + margin together
    // respect the global max_msg_width limit
    let effective_width = (ctx.total_width - right_padding - left_offset).max(400.0);
    let adjusted_max = (ctx.max_msg_width - left_offset).max(400.0);
    let max_w = effective_width.min(adjusted_max);

    egui::ScrollArea::horizontal().show(ui, |ui| {
        // The parent wrappers need room for both the content and the margin
        ui.set_max_width(max_w + left_offset);
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.set_max_width(max_w + left_offset);

                egui::Frame::default()
                    .stroke(Stroke { width: 1.0, color: ui.visuals().strong_text_color() })
                    .outer_margin(Margin { top: 0, right: 0, bottom: 15, left: 127 })
                    .inner_margin(10.0)
                    .corner_radius(5.0)
                    .fill(ui.visuals().extreme_bg_color)
                    .show(ui, |ui| {
                        render_msg_header(ui, msg_ui, &msg.msg_role.to_string(), msg, &mut actions.delete_requests);
                        render_msg_content(ui, ctx, actions, msg, msg_ui, (max_w - 20.0) as usize);
                        render_attachments(ui, msg, &ctx.math_cache);
                    });
            });
            ui.allocate_space(egui::vec2(ui.available_width(), 0.0));
        });
    });
}

fn render_assistant_msg(
    ui: &mut egui::Ui,
    ctx: &mut RenderCtx,
    actions: &mut ChatActions,
    msg: &ChatMsg,
    msg_ui: &mut ChatMsgUi,
    item_width: f32,
) {
    egui::Frame::default()
        .stroke(Stroke { width: 1.0, color: ui.visuals().hyperlink_color })
        .outer_margin(Margin::ZERO)
        .inner_margin(10.0)
        .corner_radius(5.0)
        .fill(ui.visuals().faint_bg_color)
        .show(ui, |ui| {
            egui::ScrollArea::horizontal()
                .id_salt(format!("assistant_message_scroll_{}", msg.id))
                .show(ui, |ui| {
                    ui.set_max_width(item_width - 25.0);

                    let label = format!("{}:", msg.name.as_deref().unwrap_or("assistant"));
                    render_msg_header(ui, msg_ui, &label, msg, &mut actions.delete_requests);

                    if let Some(reasoning) = msg.reasoning.as_deref().filter(|r| !r.is_empty()) {
                        if msg_ui.show_raw {
                            ui.label(format!("{}: \n{}", t!("thought_process"), reasoning));
                            ui.separator();
                        } else {
                            render_reasoning_block(ui, reasoning, msg.id);
                        }
                    }

                    let content_width = (item_width - 25.0).max(100.0);
                    render_msg_content(ui, ctx, actions, msg, msg_ui, content_width as usize);
                });
        });
}

fn render_msg_header(
    ui: &mut egui::Ui,
    msg_ui: &mut ChatMsgUi,
    label: &str,
    msg: &ChatMsg,
    delete_requests: &mut Vec<i64>,
) {
    ui.horizontal(|ui| {
        render_delete_menu(ui, msg.id, delete_requests);

        ui.label(RichText::new(label).strong());

        #[cfg(debug_assertions)]
        ui.label(RichText::new(format!("msg_id: {}", msg.id)).strong());

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            // Added first, so it ends up on the far right
            if ui.button("🗐").on_hover_text("Copy raw message to clipboard").clicked() {
                ui.ctx().copy_text(msg.content.clone());
            }
            if msg.msg_role == MsgRole::User {
                if ui.button("📚").on_hover_text("Deep copy (message + attached files) to clipboard").clicked() {
                    let attachments: Vec<Attachment> = msg.details
                        .as_deref()
                        .and_then(|d| serde_json::from_str(d).ok())
                        .unwrap_or_default();
                    let deep_text = format_deep_copy(&msg.content, &attachments);
                    ui.ctx().copy_text(deep_text);
                }
            }
            if ui.toggle_value(&mut msg_ui.show_raw, "Raw").clicked() {
                println!("Raw button clicked");
            }
        });
    });
}

/// The 🔧 button with its options popup (currently just Delete). Shared by
/// message headers and note cells.
pub(super) fn render_delete_menu(ui: &mut egui::Ui, msg_id: i64, delete_requests: &mut Vec<i64>) {
    let popup_id = ui.make_persistent_id(format!("msg_wrench_{}", msg_id));
    let wrench_resp = ui.button("🔧").on_hover_text("Message Options");
    egui::Popup::from_toggle_button_response(&wrench_resp)
        .id(popup_id)
        .close_behavior(egui::PopupCloseBehavior::CloseOnClick)
        .show(|ui| {
            ui.set_min_width(140.0); // Prevent text wrapping
            ui.with_layout(egui::Layout::top_down_justified(egui::Align::LEFT), |ui| {
                let delete_btn = egui::Button::new(
                    RichText::new(t!("delete_msg_btn")).color(ui.visuals().error_fg_color)
                ).frame(false);
                if ui.add(delete_btn)
                    .on_hover_text(RichText::new(t!("delete_msg_tooltip")).strong().heading())
                    .clicked()
                {
                    delete_requests.push(msg_id);
                    ui.close();
                }
            });
        });
}

fn render_reasoning_block(ui: &mut egui::Ui, text: &str,
        id_salt: impl std::hash::Hash + std::fmt::Debug) {
    egui::CollapsingHeader::new(
        RichText::new(t!("thought_process")).italics().weak()
    )
    .id_salt(id_salt)
    .default_open(true)
    .show(ui, |ui| {
        egui::Frame::new()
            .inner_margin(8.0)
            .show(ui, |ui| {
                ui.label(
                    RichText::new(text)
                        .italics()
                        .color(ui.visuals().weak_text_color())
                );
            });
    });
    ui.add_space(10.0);
}

// --- Attachments ---

/// Attachments are stored as a JSON array in `msg.details`. Shown as a
/// collapsible list: images inline, everything else as selectable text.
fn render_attachments(ui: &mut egui::Ui, msg: &ChatMsg, math_cache: &MathCache) {
    let Some(details_json) = &msg.details else { return };
    let Ok(attachments) = serde_json::from_str::<Vec<Attachment>>(details_json) else { return };
    if attachments.is_empty() {
        return;
    }

    ui.add_space(8.0);
    egui::CollapsingHeader::new(RichText::new(format!("📎 {} Attached Files", attachments.len())).strong())
        .id_salt(format!("details_collapse_{}", msg.id))
        .show(ui, |ui| {
            for att in attachments {
                egui::CollapsingHeader::new(RichText::new(&att.filename).weak())
                    .id_salt(format!("att_collapse_{}_{}", msg.id, att.filename))
                    .show(ui, |ui| {
                        if att.mime_type.starts_with("image/") {
                            render_image_attachment(ui, msg.id, &att, math_cache);
                        } else {
                            render_text_attachment(ui, msg.id, &att);
                        }
                    });
            }
        });
}

fn render_text_attachment(ui: &mut egui::Ui, msg_id: i64, att: &Attachment) {
    egui::ScrollArea::vertical()
        .id_salt(format!("att_scroll_{}_{}", msg_id, att.filename))
        .max_height(300.0)
        .show(ui, |ui| {
            // `&str` makes it read-only while still selectable/copyable
            let mut code = att.content.as_str();
            ui.add(
                egui::TextEdit::multiline(&mut code)
                    .desired_width(f32::INFINITY)
                    .font(egui::TextStyle::Monospace)
            );
        });
}

fn render_image_attachment(ui: &mut egui::Ui, msg_id: i64, att: &Attachment, math_cache: &MathCache) {
    let ext = match att.mime_type.as_str() {
        "image/jpeg" | "image/jpg" => ".jpg",
        "image/webp" => ".webp",
        "image/gif" => ".gif",
        _ => ".png",
    };
    let uri = format!("bytes://{}_{}{}", msg_id, att.filename, ext);

    let image_bytes = math_cache
        .borrow_mut()
        .entry(uri.clone())
        .or_insert_with(|| STANDARD.decode(att.content.trim()).unwrap_or_default().into())
        .clone();

    if image_bytes.is_empty() {
        ui.colored_label(ui.visuals().error_fg_color, "Failed to decode image data.");
        return;
    }

    // Show the byte size so it's obvious the data is actually there
    ui.label(RichText::new(format!("📸 Loaded: {} bytes", image_bytes.len())).weak().small());

    ui.ctx().include_bytes(uri.clone(), image_bytes.clone());

    let source = egui::ImageSource::Bytes {
        uri: uri.clone().into(),
        bytes: egui::load::Bytes::Shared(image_bytes),
    };

    // Poll the texture explicitly to see exactly what state the loader is in
    match ui.ctx().try_load_texture(&uri, egui::TextureOptions::LINEAR, egui::SizeHint::default()) {
        Ok(egui::load::TexturePoll::Pending { .. }) => {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Decoding image...");
            });
        }
        Ok(egui::load::TexturePoll::Ready { texture }) => {
            // Force a strict size so the layout cannot collapse to 0x0
            let size = texture.size;
            let max_img_w = 300.0_f32;
            let scale = if size.x > max_img_w { max_img_w / size.x } else { 1.0 };
            ui.add(egui::Image::new(source).fit_to_exact_size(size * scale));
        }
        Err(err) => {
            ui.colored_label(ui.visuals().error_fg_color, format!("Texture Error: {}", err));
        }
    }
}