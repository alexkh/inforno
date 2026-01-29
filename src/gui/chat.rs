use egui::{Margin, RichText, Stroke};
use egui_commonmark::CommonMarkViewer;
use rust_i18n::t;

use crate::{
    common::{
        ChatMsg, ChatMsgUi, MsgRole,
    },
    gui::{State},
};

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

    // welcome screen is only shown when chat is empty:
    if msg_pool.is_empty() {
           egui::Frame::default()
        .stroke(Stroke {
            width: 1.0,
            color: ui.visuals().hyperlink_color,
        })
        .outer_margin(Margin {
            top: 0,
            right: 5,
            bottom: 0,
            left: 5,
        })
        .inner_margin(10.0)
        .corner_radius(5.0)
        .fill(ui.visuals().faint_bg_color)
        .show(ui, |ui| {
            ui.heading(t!("welcome_tour"));
        });
        return;
    }

    let active_agent_ind = 0;

    // Buffer to hold contiguous assistant message IDs
    let mut assistant_batch: Vec<i64> = Vec::new();

    if let Some(agent) = state.chat.agents.get(active_agent_ind) {
        for &msg_id in &agent.msg_ids {
            if let Some(msg) = msg_pool.get(&msg_id) {

                match msg.msg_role {
                    MsgRole::User | MsgRole::System => {
                        // 1. If we have a pending batch of assistant messages,
                        // render them first
                        if !assistant_batch.is_empty() {
                            render_assistant_grid(ui, cache, msg_pool,
                                msg_ui_map, &assistant_batch, total_width);
                            assistant_batch.clear();
                        }

                        // 2. Render the user message (Full Width)
                        // We access the map entry here to get the mutable UI state
                        let msg_ui = msg_ui_map.entry(msg_id)
                                .or_insert(ChatMsgUi::default());
                        render_user_msg(ui, cache, msg, msg_ui, total_width);
                    }
                    _ => {
                        // Assume Assistant messages go into the grid batch
                        assistant_batch.push(msg_id);
                    }
                }
            }
        }

        // Render any remaining assistant messages at the end of the chat
        if !assistant_batch.is_empty() {
            render_assistant_grid(ui, cache, msg_pool, msg_ui_map,
                    &assistant_batch, total_width);
        }
    }
}

/// Renders a list of assistant messages side-by-side based on available width
fn render_assistant_grid(
    ui: &mut egui::Ui,
    cache: &mut egui_commonmark::CommonMarkCache,
    msg_pool: &std::collections::HashMap<i64, ChatMsg>,
    msg_ui_map: &mut std::collections::HashMap<i64, ChatMsgUi>,
    batch_ids: &[i64],
    total_width: f32,
) {
    let effective_width = total_width - 38.0;

    let item_min_width = 400.0;
    let item_max_width = 900.0;
    let spacing = 10.0;

    // Calculate how many columns fit. Minimum 1 column.
    let max_cols = (((effective_width + spacing) / (item_min_width + spacing))
        .floor() as usize)
        .max(1);

    // If we have fewer messages than max columns, use the message count
    // to calculate width (allowing them to grow), otherwise use max_cols.
    let divisor = if batch_ids.len() < max_cols {
        batch_ids.len() as f32
    } else {
        max_cols as f32
    };

    // 4. Calculate Item Width
    // Formula: (Available - TotalSpacing - RoundingBuffer) / Count
    // RoundingBuffer: Subtract 2px per item to handle floating point expansion
    let total_spacing = spacing * (divisor - 1.0);
    let rounding_buffer = divisor * 2.0;

    let raw_item_width = (effective_width - total_spacing - rounding_buffer) / divisor;
    let item_width = raw_item_width.clamp(item_min_width, item_max_width);

    // Use max_cols for the chunking logic so the grid wraps correctly
    let cols = max_cols;

    // Grid Loop: Iterate over chunks (rows)
    for (row_idx, row_ids) in batch_ids.chunks(cols).enumerate() {
        // Use horizontal layout for the row
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = spacing;
            for &msg_id in row_ids {
                if let Some(msg) = msg_pool.get(&msg_id) {
                    let msg_ui = msg_ui_map.entry(msg_id).or_insert(ChatMsgUi::default());

                    // We create a scope to enforce the calculated width
                    ui.allocate_ui_with_layout(
                        egui::vec2(item_width, 0.0), // 0.0 y allows it to grow vertically
                        egui::Layout::top_down(egui::Align::Min),
                        |ui| {
                            // We set the width on the ui so wrapping works correctly inside the message
                            ui.set_width(item_width);
                            render_assistant_msg(
                                    ui, cache, msg, msg_ui, item_width);
                        }
                    );
                }
            }

            // FIX 3: Fill the remaining horizontal space in the row.
            // This ensures the row's bounding box extends to the edge of the screen,
            // allowing the ScrollArea to capture scroll events even in the empty space.
            ui.allocate_space(egui::vec2(ui.available_width(), 0.0));
        });

        // Add vertical spacing between rows
        if row_idx < (batch_ids.len().div_ceil(cols) - 1) {
             ui.add_space(spacing);
        }
    }
    // Add space after the entire grid block before the next user message
    ui.add_space(15.0);
}

fn render_user_msg(
    ui: &mut egui::Ui,
    cache: &mut egui_commonmark::CommonMarkCache,
    msg: &ChatMsg,
    msg_ui: &mut ChatMsgUi,
    total_width: f32,
) {
    // Consistent margin for user message too
    let effective_width = total_width - 30.0;

    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            let max_w = effective_width.clamp(400.0, 800.0);
            ui.set_max_width(max_w);

            egui::Frame::default()
            .stroke(Stroke {
                width: 1.0,
                color: ui.visuals().strong_text_color(),
            })
            .outer_margin(Margin {
                top: 0,
                right: 0,
                bottom: 15,
                left: 127,
            })
            .inner_margin(10.0)
            .corner_radius(5.0)
            .fill(ui.visuals().extreme_bg_color)
            .show(ui, |ui| {
                render_msg_header(ui, msg_ui, &msg.msg_role.to_string(), msg.id);
                render_msg_content(ui, cache, msg, msg_ui, (max_w - 20.0) as usize);
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
) {
    egui::Frame::default()
    .stroke(Stroke {
        width: 1.0,
        color: ui.visuals().hyperlink_color,
    })
    .outer_margin(Margin::ZERO) // No outer margin ensures strict width adherence
    .inner_margin(10.0)
    .corner_radius(5.0)
    .fill(ui.visuals().faint_bg_color)
    .show(ui, |ui| {
        let label = format!(
            "{}:",
            msg.name.as_deref().unwrap_or("assistant")
            //msg.model.as_deref().unwrap_or("assistant")
        );
        render_msg_header(ui, msg_ui, &label, msg.id);

        // --- REASONING BLOCK ---
        if let Some(reasoning) = &msg.reasoning {
            if !reasoning.is_empty() {
                if msg_ui.show_raw {
                    // In raw mode, just dump it plainly
                    ui.label(format!("{}: \n{}", t!("thought_process"),
                            reasoning));
                    ui.separator();
                } else {
                    render_reasoning_block(ui, reasoning, msg.id);
                }
            }
        }
        // -----------------------

        // Subtract 20.0 (inner margins 10 left + 10 right) for the content area
        let content_width = (item_width - 20.0).max(100.0);
        render_msg_content(ui, cache, msg, msg_ui, content_width as usize);
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
) {
    if msg_ui.show_raw {
        ui.label(RichText::new(format!("{}", msg.content)).strong());
    } else {
        // 1. Set the spacing to ensure no accidental indentation
        CommonMarkViewer::new()
        .max_image_width(Some(max_image_width))
        .show(ui, cache, &msg.content);
    }
}

fn render_reasoning_block(ui: &mut egui::Ui, text: &str,
        id_salt: impl std::hash::Hash) {
    egui::CollapsingHeader::new(
        egui::RichText::new(t!("thought_process")).italics().weak()
    )
    .id_salt(id_salt)
    .default_open(true) // Set to false if you want it closed by default
    .show(ui, |ui| {
        egui::Frame::new()
            //.fill(ui.visuals().code_bg_color)
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
