//! Cells inside a chat (notebook-style): the welcome note shown in an empty
//! chat, Developer note cells (Markdown or Rhai, view/edit), and the
//! appender at the bottom, which commits into a new note cell and promotes
//! a temporary chat to a saved one if needed.

use std::collections::HashMap;

use bulat::editor::{CodeEditor, CodeEditorOutput, ColorTheme, Syntax};
use egui::{Margin, RichText, Stroke};
use rust_i18n::t;

use inforno_core::common::{Chat, ChatMsg, MsgRole};
use inforno_core::scripting::is_likely_rhai;

use crate::state::{ChatMsgUi, State};

use super::content::render_msg_content;
use super::message::render_delete_menu;
use super::{ChatActions, RenderCtx};

// --- Layout helpers ---

/// Width of every notebook cell: the pane width minus the chat view's
/// padding, capped by the user's max message width.
fn cell_width(ctx: &RenderCtx) -> f32 {
    (ctx.total_width - 40.0).max(400.0).min(ctx.max_msg_width)
}

/// A left-aligned row at most `max_w` wide that claims the rest of the line.
fn cell_row(ui: &mut egui::Ui, max_w: f32, add_contents: impl FnOnce(&mut egui::Ui)) {
    ui.horizontal(|ui| {
        ui.set_max_width(max_w);
        ui.vertical(|ui| {
            ui.set_max_width(max_w);
            add_contents(ui);
        });
        ui.allocate_space(egui::vec2(ui.available_width(), 0.0));
    });
}

/// Frame shared by the welcome note and note cells.
fn note_frame(ui: &egui::Ui) -> egui::Frame {
    egui::Frame::default()
        .outer_margin(Margin { top: 5, right: 10, bottom: 5, left: 10 })
        .inner_margin(10.0)
        .fill(ui.visuals().extreme_bg_color)
        .corner_radius(5.0)
        .stroke(Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color))
}

/// Shows `editor` with one extra trailing line (so the cursor can always
/// reach the end of the text) and strips that padding again afterwards.
fn show_padded_editor(ui: &mut egui::Ui, mut editor: CodeEditor, text: &mut String) -> CodeEditorOutput {
    text.push('\n');
    let out = editor.show(ui, text);
    if text.ends_with('\n') {
        text.pop();
        if text.ends_with('\r') {
            text.pop();
        }
    }
    out
}

/// Once text looks like Rhai it stays Rhai while being edited, until the
/// cell is emptied.
fn sticky_rhai(was_rhai: bool, text: &str) -> bool {
    if !was_rhai && is_likely_rhai(text) {
        true
    } else if was_rhai && text.trim().is_empty() {
        false
    } else {
        was_rhai
    }
}

fn syntax_for(is_rhai: bool) -> Syntax {
    if is_rhai { Syntax::rhai() } else { Syntax::from_mime("text/markdown") }
}

// --- Cells ---

/// Fake note shown in an empty chat. Uses a fixed temporary id (-1) so its
/// UI state (e.g. Raw toggle) survives across frames.
pub(super) fn render_welcome(
    ui: &mut egui::Ui,
    ctx: &mut RenderCtx,
    actions: &mut ChatActions,
    msg_ui_map: &mut HashMap<i64, ChatMsgUi>,
) {
    let _welcome_span = tracing::info_span!("render_welcome_message").entered();
    let max_w = cell_width(ctx);

    let fake_msg = ChatMsg {
        id: -1,
        // Normalize so strict Markdown rules (e.g. table spacing) are met
        content: inforno_core::db::normalize_code_blocks(&t!("welcome_tour")),
        ..Default::default()
    };
    let fake_msg_ui = msg_ui_map.entry(fake_msg.id).or_default();

    cell_row(ui, max_w, |ui| {
        note_frame(ui).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("📝 Note Cell").weak().small());
                ui.label(RichText::new("✨ Welcome").color(egui::Color32::DARK_GREEN).small());
            });
            ui.add_space(5.0);
            render_msg_content(ui, ctx, actions, &fake_msg, fake_msg_ui, max_w as usize);
        });
    });
}

/// A Developer message: rendered like any message in View mode, editable
/// as Markdown/Rhai in Edit mode. Saved on switching back to View, on focus
/// loss, or on Ctrl+Enter.
pub(super) fn render_note_cell(
    ui: &mut egui::Ui,
    ctx: &mut RenderCtx,
    actions: &mut ChatActions,
    msg: &ChatMsg,
    msg_ui: &mut ChatMsgUi,
) {
    let mut note_content = msg.content.clone();
    let num_lines = note_content.lines().count().max(1);

    // Syntax detection runs once per message, then stays cached in UI state
    let is_rhai = *msg_ui.is_rhai.get_or_insert_with(|| is_likely_rhai(&note_content));

    let edit_mode_id = ui.id().with(format!("note_edit_mode_{}", msg.id));
    let mut is_edit_mode = ui.data(|d| d.get_temp::<bool>(edit_mode_id).unwrap_or(false));

    let max_w = cell_width(ctx);

    cell_row(ui, max_w, |ui| {
        note_frame(ui).show(ui, |ui| {
            ui.horizontal(|ui| {
                render_delete_menu(ui, msg.id, &mut actions.delete_requests);

                ui.label(RichText::new("📝 Note Cell").weak().small());
                if msg.is_unsaved {
                    ui.label(RichText::new("✏ Unsaved").color(ui.visuals().warn_fg_color).small());
                } else {
                    ui.label(RichText::new("✔ Saved").color(egui::Color32::DARK_GREEN).small());
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("🗐").on_hover_text("Copy note to clipboard").clicked() {
                        ui.ctx().copy_text(note_content.clone());
                    }

                    let toggle_label = if is_edit_mode { "👁 View" } else { "📝 Edit" };
                    if ui.toggle_value(&mut is_edit_mode, toggle_label).clicked() {
                        ui.data_mut(|d| d.insert_temp(edit_mode_id, is_edit_mode));

                        // Explicitly save when switching back to View mode
                        if !is_edit_mode {
                            actions.db_updates.push((msg.id, note_content.clone()));
                            actions.content_updates.push((msg.id, note_content.clone(), false));
                        }
                    }
                });
            });

            if is_edit_mode {
                render_note_editor(
                    ui, actions, msg.id, msg_ui, &mut note_content,
                    num_lines, is_rhai, max_w, edit_mode_id,
                );
            } else {
                ui.add_space(5.0);
                render_msg_content(ui, ctx, actions, msg, msg_ui, max_w as usize);
            }
        });
    });
}

fn render_note_editor(
    ui: &mut egui::Ui,
    actions: &mut ChatActions,
    msg_id: i64,
    msg_ui: &mut ChatMsgUi,
    note_content: &mut String,
    num_lines: usize,
    is_rhai: bool,
    max_w: f32,
    edit_mode_id: egui::Id,
) {
    let editor = CodeEditor::default()
        .id_source(format!("note_{}", msg_id))
        .with_theme(ColorTheme::SV)
        .with_syntax(syntax_for(is_rhai))
        .with_numlines(false)
        .with_rows(num_lines + 1)
        .vscroll(false)
        .v_auto_shrink(true) // Uncap height to display full text
        .desired_width(max_w);
    let out = show_padded_editor(ui, editor, note_content);
    let response = &out.output.response;

    // Mark as unsaved while typing
    if response.changed() {
        msg_ui.is_rhai = Some(sticky_rhai(is_rhai, note_content));
        actions.content_updates.push((msg_id, note_content.clone(), true));
    }

    // Ctrl+Enter: save and switch back to View mode
    let shortcut = egui::KeyboardShortcut::new(egui::Modifiers::CTRL, egui::Key::Enter);
    let mut force_save = false;
    if response.has_focus() && ui.input_mut(|i| i.consume_shortcut(&shortcut)) {
        force_save = true;
        response.surrender_focus();
        ui.data_mut(|d| d.insert_temp(edit_mode_id, false));
    }

    // Commit to the database and mark as saved on focus loss
    if response.lost_focus() || force_save {
        actions.db_updates.push((msg_id, note_content.clone()));
        actions.content_updates.push((msg_id, note_content.clone(), false));
    }
}

/// The always-present empty cell at the bottom of a chat. Its text lives in
/// `chat.draft_note`; it is committed (see `commit_draft`) on focus loss or
/// Ctrl+Enter.
pub(super) fn render_appender(
    ui: &mut egui::Ui,
    ctx: &RenderCtx,
    actions: &mut ChatActions,
    chat_id: i64,
    chat: &Chat,
) {
    let _appender_span = tracing::info_span!("render_notebook_appender").entered();
    ui.add_space(1.0);

    let max_w = cell_width(ctx);

    cell_row(ui, max_w, |ui| {
        egui::Frame::default()
            .outer_margin(Margin { top: 1, right: 1, bottom: 1, left: 1 })
            .inner_margin(1.0)
            .fill(ui.visuals().faint_bg_color)
            .corner_radius(5.0)
            .show(ui, |ui| {
                let mut draft_note = chat.draft_note.clone();
                let num_lines = draft_note.lines().count().max(1);

                let editor = CodeEditor::default()
                    .id_source(format!("draft_{}", chat_id))
                    .with_theme(ColorTheme::SV)
                    .with_syntax(syntax_for(chat.draft_is_rhai))
                    .with_numlines(false)
                    .with_rows(num_lines + 1)
                    .vscroll(false)
                    .v_auto_shrink(true) // Uncap height to display full text
                    .desired_width(max_w - 50.0);
                let out = show_padded_editor(ui, editor, &mut draft_note);
                let response = &out.output.response;

                // Placeholder text while empty
                if draft_note.is_empty() && !response.has_focus() {
                    ui.painter().text(
                        response.rect.min + egui::vec2(2.0, 2.0),
                        egui::Align2::LEFT_TOP,
                        "Click to add a note or executable script...",
                        egui::FontId::monospace(14.0),
                        ui.visuals().text_color().linear_multiply(0.5)
                    );
                }

                let shortcut = egui::KeyboardShortcut::new(egui::Modifiers::CTRL, egui::Key::Enter);
                if response.has_focus() && ui.input_mut(|i| i.consume_shortcut(&shortcut)) {
                    response.surrender_focus();
                    actions.draft_lost_focus = true; // Force commit immediately
                }

                if response.changed() || draft_note != chat.draft_note {
                    let is_rhai = sticky_rhai(chat.draft_is_rhai, &draft_note);
                    actions.new_draft = Some((draft_note.clone(), is_rhai));
                }

                if response.lost_focus() {
                    actions.draft_lost_focus = true;
                }
            });
    });
}

/// Saves `draft_content` as a new note cell at the end of the chat.
///
/// A temporary chat (id <= 0) is first promoted to a saved one, titled
/// after the draft's first non-empty line. Its tab is retargeted through
/// the `tab_upgrades` queue in egui memory, because the pane tree is
/// borrowed by the layout engine right now (see `ui_chat`).
pub(super) fn commit_draft(ui: &mut egui::Ui, state: &mut State, chat_id: i64, draft_content: String) {
    let mut actual_chat_id = chat_id;

    if actual_chat_id <= 0 {
        let mut new_chat = Chat::default();

        let title = draft_content.lines()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("Notebook")
            .trim();
        new_chat.title = if title.chars().count() > 40 {
            format!("{}...", title.chars().take(37).collect::<String>())
        } else {
            title.to_string()
        };

        // Chat::default() + mk_chat() already create the Omnis agent
        if let Ok(()) = inforno_core::db::mk_chat(&state.db_conn, &mut new_chat) {
            actual_chat_id = new_chat.id;
            state.open_chats.insert(actual_chat_id, new_chat);
            state.active_chat_id = Some(actual_chat_id);

            ui.data_mut(|d| {
                let mut upgrades = d.get_temp::<Vec<(i64, i64)>>(egui::Id::new("tab_upgrades")).unwrap_or_default();
                upgrades.push((chat_id, actual_chat_id));
                d.insert_temp(egui::Id::new("tab_upgrades"), upgrades);
            });

            crate::state::reload_db_chats(&state.db_conn, &mut state.db_chats);
        }
    }

    let mut new_msg = ChatMsg {
        id: 0,
        msg_role: MsgRole::Developer,
        content: draft_content,
        ..Default::default()
    };

    if let Ok(()) = inforno_core::db::mk_msg(&state.db_conn, &mut new_msg) {
        let new_id = new_msg.id;
        if let Some(target_chat) = state.open_chats.get_mut(&actual_chat_id) {
            target_chat.msg_pool.insert(new_id, new_msg);
            for agent in target_chat.agents.iter_mut() {
                agent.msg_ids.push(new_id);
                let _ = inforno_core::db::mod_agent_msgs(&state.db_conn, agent.id, &agent.msg_ids);
            }
        }
    }
}