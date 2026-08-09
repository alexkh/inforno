use egui::{Margin, RichText, Stroke};
use egui_commonmark::CommonMarkViewer;
use rust_i18n::t;
use inforno_core::common::Attachment;
use crate::math_render::compile_math_to_svg_embedded;
use std::sync::OnceLock;
use regex::Regex;
use base64::{Engine as _, engine::general_purpose::STANDARD};

use bulat::editor::{CodeEditor, Syntax, ColorTheme};
use crate::split_button::SplitButton;

use inforno_core::{
    common::{
        ChatMsg, MsgRole,
    },
};

use crate::state::{State, ChatMsgUi};

pub fn ui_chat(ui: &mut egui::Ui, state: &mut State) {
    egui::CentralPanel::default()
    //.stick_to_the_bottom(true)
    .show(ui, |ui| {
        if state.is_modal_open {
            ui.disable();
        }

        // 1. Temporarily extract the tree from the state.
        // We replace it with a cheap, empty placeholder so the state remains valid.
        let mut tree = std::mem::replace(
            &mut state.pane_tree,
            egui_tiles::Tree::empty("temp_tree")
        );

        // 2. Create the behavior bridge WITH the new action queue
        let mut behavior = crate::panes::PaneBehavior {
            state,
            split_requests: Vec::new(),
            close_requests: Vec::new(),
            open_chat_requests: Vec::new(),
        };

        // 3. Render the layout
        tree.ui(&mut behavior, ui);

        // --- NEW: Layout Auto-Fix Pass ---
        // When a user drags the active tab out of a container to split the screen,
        // the old container still remembers that tab as "active", even though it's gone!
        // This sweep ensures every Tabs container points to a valid, existing child.
        let mut needs_repaint = false;
        for (_, tile) in tree.tiles.iter_mut() {
            if let egui_tiles::Tile::Container(egui_tiles::Container::Tabs(tabs)) = tile {
                if !tabs.children.is_empty() {
                    // Check if the currently active tab actually exists inside this container
                    let has_valid_active = tabs.active.is_some_and(|id| tabs.children.contains(&id));

                    if !has_valid_active {
                        // The active tab was dragged away. Reset focus to the first remaining tab.
                        tabs.active = Some(tabs.children[0]);
                        needs_repaint = true;
                    }
                }
            }
        }

        if needs_repaint {
            ui.ctx().request_repaint();
        }
        // ---------------------------------

        // Extract the queues to drop `behavior` and release the borrow on `state`
        let split_requests = behavior.split_requests;
        let close_requests = behavior.close_requests;
        let open_chat_requests = behavior.open_chat_requests;

        // 4. Process any requested splits safely
        for (new_pane, direction) in split_requests {
            let new_tile = tree.tiles.insert_pane(new_pane);
            let new_tabs = tree.tiles.insert_tab_tile(vec![new_tile]);

            if let Some(root_id) = tree.root {
                // Determine whether to split horizontally or vertically
                let new_root = match direction {
                    crate::panes::SplitAction::Right => {
                        tree.tiles.insert_horizontal_tile(vec![root_id, new_tabs])
                    }
                    crate::panes::SplitAction::Down => {
                        tree.tiles.insert_vertical_tile(vec![root_id, new_tabs])
                    }
                };
                tree.root = Some(new_root);
            } else {
                tree.root = Some(new_tabs);
            }
        }

        // 5. Process any requested closes safely
        for dead_id in close_requests {
            // Notice we are using `state` directly now, not `behavior.state`
            if state.active_tile_id == Some(dead_id) {
                state.active_tile_id = None;
                state.active_chat_id = None;
            }

            state.merge_apps.remove(&dead_id);

            // This safely removes the tab and cleans up its parent containers
            tree.remove_recursively(dead_id);
        }

        // 6. Put the updated tree back! (Safe because `behavior` is dead)
        state.pane_tree = tree;

        // 6.5 Process Tab Upgrades (e.g., Temporary Notebooks converting to saved Chats)
        let upgrades = ui.data_mut(|d| {
            let u = d.get_temp::<Vec<(i64, i64)>>(egui::Id::new("tab_upgrades")).unwrap_or_default();
            d.insert_temp(egui::Id::new("tab_upgrades"), Vec::<(i64, i64)>::new());
            u
        });

        for (old_id, new_id) in upgrades {
            for (_, tile) in state.pane_tree.tiles.iter_mut() {
                if let egui_tiles::Tile::Pane(crate::panes::Pane::Chat { chat_id: id }) = tile {
                    if *id == old_id {
                        *id = new_id;
                    }
                }
            }
            // Safely drop the old ghost chat from memory now that tabs are updated
            state.open_chats.remove(&old_id);
        }

        // 7. Process any requested chat opens
        for (chat_id, open_right) in open_chat_requests {
            // Guarantee the target chat is loaded into application memory
            if !state.open_chats.contains_key(&chat_id) {
                let loaded_chat = inforno_core::db::fetch_chat(&state.db_conn, chat_id, &state.presets).unwrap_or_default();
                state.open_chats.insert(chat_id, loaded_chat);
            }

            // Route the UI appropriately
            if open_right {
                crate::panes::open_chat_in_right_pane(state, chat_id);
            } else {
                crate::panes::open_chat_in_tab(state, chat_id);
            }
        }
    });
}

// --- Message Rendering ---

#[tracing::instrument(skip_all)]
pub fn render_chat_messages(ui: &mut egui::Ui, state: &mut State, chat_id: i64, total_width: f32) {
    let _render_span = tracing::info_span!("render_chat_messages", chat_id).entered();

    let mut max_msg_width = *state.chat_widths.entry(chat_id).or_insert(800.0);

    ui.input_mut(|i| {
        if i.consume_shortcut(&egui::KeyboardShortcut::new(egui::Modifiers::CTRL, egui::Key::OpenBracket)) {
            max_msg_width -= 50.0;
        }
        if i.consume_shortcut(&egui::KeyboardShortcut::new(egui::Modifiers::CTRL, egui::Key::CloseBracket)) {
            max_msg_width += 50.0;
        }
    });

    // Only enforce the minimum bound so it doesn't break layout
    max_msg_width = max_msg_width.max(400.0);
    state.chat_widths.insert(chat_id, max_msg_width);

    let msg_ui_map = &mut state.chat_msg_ui;
    let cache = &mut state.common_mark_cache;

    let project_root = &state.project_root;
    let active_realm = &state.active_realm;
	let op_tx = state.op_tx.clone();

    // We clone the Rc pointer here (very cheap)
    let math_cache = state.math_cache.clone();

    // Fetch the specific chat being rendered
    let Some(chat) = state.open_chats.get(&chat_id) else {
        return; // Chat not loaded yet
    };

    let msg_pool = &chat.msg_pool;


    let mut content_updates = Vec::new();
    let mut db_updates = Vec::new();
    let mut rhai_updates = Vec::new(); // NEW: Captures output string to update msg pool
    let mut new_draft = None;
    let mut draft_lost_focus = false;
    let mut llm_prompt_request = None; // NEW: Captures requests sent via Rhai `send_prompt()`
    let mut delete_requests = Vec::new(); // NEW: Queue deletions

    {
        // Fetch the specific chat being rendered
        let Some(chat) = state.open_chats.get(&chat_id) else {
            return; // Chat not loaded yet
        };

        let msg_pool = &chat.msg_pool;

        if msg_pool.is_empty() {
            let _welcome_span = tracing::info_span!("render_welcome_message").entered();
            let mut welcome_text = t!("welcome_tour").to_string();
            let num_lines = welcome_text.lines().count().max(1);
            let note_margin_offset = 40.0;
            let effective_width = (total_width - note_margin_offset).max(400.0);
            let max_w = effective_width.min(max_msg_width);

            ui.horizontal(|ui| {
                ui.set_max_width(max_w);
                ui.vertical(|ui| {
                    ui.set_max_width(max_w);
                    egui::Frame::default()
                        .outer_margin(Margin { top: 5, right: 10, bottom: 5, left: 10 })
                        .inner_margin(10.0)
                        .fill(ui.visuals().extreme_bg_color)
                        .corner_radius(5.0)
                        .stroke(Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color))
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.label(egui::RichText::new("📝 Note Cell").weak().small());
                                ui.label(egui::RichText::new("✨ Welcome").color(egui::Color32::DARK_GREEN).small());
                            });
                            ui.add_space(5.0);
                            let mut fake_msg = inforno_core::common::ChatMsg::default();
                            // Guarantee strict Markdown compliance (like table spacing) is met!
                            fake_msg.content = inforno_core::db::normalize_code_blocks(&welcome_text);
                            fake_msg.id = -1; // Arbitrary temporary ID

                            // Fetch persistent UI state using the temporary ID so interactions don't reset
                            let fake_msg_ui = msg_ui_map.entry(fake_msg.id).or_insert_with(ChatMsgUi::default);

                            render_msg_content(
                                ui, cache, &fake_msg, fake_msg_ui, max_w as usize, math_cache.clone(),
                                project_root, active_realm, &op_tx, &mut rhai_updates, &mut llm_prompt_request
                            );
                        });
                });
                ui.allocate_space(egui::vec2(ui.available_width(), 0.0));
            });
            // NO `return;` HERE! We allow the execution to fall through
            // so the Notebook Appender cell renders beneath this fake view.
        }

        let active_agent_ind = 0;
        let mut assistant_batch: Vec<i64> = Vec::new();

        if let Some(agent) = chat.agents.get(active_agent_ind) {
            let _agent_span = tracing::info_span!("process_agent_messages", agent_id = agent.id).entered();
            for &msg_id in &agent.msg_ids {
                if let Some(msg) = msg_pool.get(&msg_id) {
                    let _msg_span = tracing::info_span!("process_message", msg_id = msg_id, role = %msg.msg_role).entered();
                    match msg.msg_role {
                        MsgRole::User | MsgRole::System => {
                            if !assistant_batch.is_empty() {
                                // Pass a clone of the cache pointer
                                render_assistant_grid(ui, cache, msg_pool,
                                    msg_ui_map, &assistant_batch, total_width, math_cache.clone(),
                                project_root, active_realm, &op_tx, max_msg_width, &mut rhai_updates, &mut llm_prompt_request, &mut delete_requests);
                                assistant_batch.clear();
                            }

                            let msg_ui = msg_ui_map.entry(msg_id)
                                    .or_insert(ChatMsgUi::default());
                            // Pass a clone of the cache pointer
                            render_user_msg(ui, cache, msg, msg_ui, total_width, math_cache.clone(),
                                project_root, active_realm, &op_tx, max_msg_width, &mut rhai_updates, &mut llm_prompt_request, &mut delete_requests);
                        }
                        MsgRole::Developer => {
                            if !assistant_batch.is_empty() {
                                render_assistant_grid(ui, cache, msg_pool,
                                    msg_ui_map, &assistant_batch, total_width, math_cache.clone(),
                                project_root, active_realm, &op_tx, max_msg_width, &mut rhai_updates, &mut llm_prompt_request, &mut delete_requests);
                                assistant_batch.clear();
                            }

                            let msg_ui = msg_ui_map.entry(msg_id).or_insert(ChatMsgUi::default());
                            let mut note_content = msg.content.clone();
                            let num_lines = note_content.lines().count().max(1);

                            // Dynamically check syntax on load, cache it in UI state
                            let is_rhai = *msg_ui.is_rhai.get_or_insert_with(|| inforno_core::scripting::is_likely_rhai(&note_content));

                            let edit_mode_id = ui.id().with(format!("note_edit_mode_{}", msg.id));
                            // Default to view mode (false)
                            let mut is_edit_mode = ui.data(|d| d.get_temp::<bool>(edit_mode_id).unwrap_or(false));

                            // Enforce the max width limit for the Developer Note Cell
                            let note_margin_offset = 40.0;
                            let effective_width = (total_width - note_margin_offset).max(400.0);
                            let max_w = effective_width.min(max_msg_width);

                            ui.horizontal(|ui| {
                                ui.set_max_width(max_w);
                                ui.vertical(|ui| {
                                    ui.set_max_width(max_w);

                                    egui::Frame::default()
                                        .outer_margin(Margin { top: 5, right: 10, bottom: 5, left: 10 })
                                        .inner_margin(10.0)
                                .fill(ui.visuals().extreme_bg_color)
                                .corner_radius(5.0)
                                .stroke(Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color))
                                .show(ui, |ui| {
                                    ui.horizontal(|ui| {
                                        let popup_id = ui.make_persistent_id(format!("msg_wrench_{}", msg.id));
                                        let wrench_resp = ui.button("🔧").on_hover_text("Message Options");
                                        egui::Popup::from_toggle_button_response(&wrench_resp)
                                        .id(popup_id)
                                        .close_behavior(egui::PopupCloseBehavior::CloseOnClick)
                                        .show(|ui| {
                                            ui.set_min_width(140.0); // Prevent text wrapping
                                            ui.with_layout(egui::Layout::top_down_justified(egui::Align::LEFT), |ui| {
                                            if ui.add(egui::Button::new(egui::RichText::new(t!("delete_msg_btn"))
                                            .color(ui.visuals().error_fg_color))
                                            .frame(false))
                                            .on_hover_text(
                                                egui::RichText::new(t!("delete_msg_tooltip"))
                                            .strong()
                                            .heading()
                                            )
                                            .clicked() {
                                                delete_requests.push(msg.id);
                                                ui.close();
                                            }
                                            });
                                        });

                                        ui.label(egui::RichText::new("📝 Note Cell").weak().small());

                                        // NEW: Saved/Unsaved Indicators
                                        if msg.is_unsaved {
                                            ui.label(egui::RichText::new("✏ Unsaved").color(ui.visuals().warn_fg_color).small());
                                        } else {
                                            ui.label(egui::RichText::new("✔ Saved").color(egui::Color32::DARK_GREEN).small());
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
                                                            db_updates.push((msg_id, note_content.clone()));
                                                            content_updates.push((msg_id, note_content.clone(), false));
                                                        }
                                                    }
                                                });
                                            });

                                            if is_edit_mode {
                                        let syntax = if is_rhai { Syntax::rhai() } else { Syntax::from_mime("text/markdown") };

                                        let mut padded_note = note_content.clone();
                                        padded_note.push('\n');

                                        let out = CodeEditor::default()
                                            .id_source(format!("note_{}", msg.id))
                                            .with_theme(ColorTheme::SV)
                                            .with_syntax(syntax)
                                            .with_numlines(false)
                                            .with_rows(num_lines + 1)
                                            .vscroll(false)
                                            .v_auto_shrink(true) // Uncap height to display full text
                                            .desired_width(max_w)
                                            .show(ui, &mut padded_note);

                                        if padded_note.ends_with('\n') {
                                            padded_note.pop();
                                            if padded_note.ends_with('\r') {
                                                padded_note.pop();
                                            }
                                        }
                                        note_content = padded_note;

                                        // Sticky Rhai Detection & Mark as unsaved while typing
                                        if out.output.response.changed() {
                                            let mut new_is_rhai = is_rhai;

                                            if !new_is_rhai && inforno_core::scripting::is_likely_rhai(&note_content) {
                                                new_is_rhai = true; // Upgrade to Rhai
                                            } else if new_is_rhai && note_content.trim().is_empty() {
                                                new_is_rhai = false; // Downgrade to Text if emptied
                                            }

                                            msg_ui.is_rhai = Some(new_is_rhai); // Cache immediately to UI

                                            content_updates.push((msg_id, note_content.clone(), true));
                                        }

                                        let shortcut = egui::KeyboardShortcut::new(egui::Modifiers::CTRL, egui::Key::Enter);
                                        let mut force_save = false;

                                        if out.output.response.has_focus() && ui.input_mut(|i| i.consume_shortcut(&shortcut)) {
                                            force_save = true;
                                            out.output.response.surrender_focus();
                                            // Switch back to View mode automatically
                                            is_edit_mode = false;
                                            ui.data_mut(|d| d.insert_temp(edit_mode_id, false));
                                        }

                                        // Commit to database and mark as saved on focus lost
                                        if out.output.response.lost_focus() || force_save {
                                            db_updates.push((msg_id, note_content.clone()));
                                            content_updates.push((msg_id, note_content.clone(), false));
                                        }
                                                                                } else {
                                                // VIEW MODE
                                                ui.add_space(5.0);
                                                render_msg_content(
                                                    ui, cache, msg, msg_ui, max_w as usize, math_cache.clone(),
                                                    project_root, active_realm, &op_tx,
                                                    &mut rhai_updates, &mut llm_prompt_request
                                                );
                                            }
                                        });
                                }); // End vertical wrapper
                                ui.allocate_space(egui::vec2(ui.available_width(), 0.0));
                            }); // End horizontal wrapper
                        }
                        _ => {
                            assistant_batch.push(msg_id);
                        }
                    }
                }
            }

            if !assistant_batch.is_empty() {
                let _assistant_grid_span = tracing::info_span!("render_assistant_grid", count = assistant_batch.len()).entered();
                // Pass a clone of the cache pointer
                render_assistant_grid(ui, cache, msg_pool, msg_ui_map,
                        &assistant_batch, total_width, math_cache.clone(),
                        project_root, active_realm, &op_tx, max_msg_width, &mut rhai_updates, &mut llm_prompt_request, &mut delete_requests);
            }

            // --- NOTEBOOK APPENDER CELL ---
            let _appender_span = tracing::info_span!("render_notebook_appender").entered();
            ui.add_space(1.0);

            let appender_offset = 40.0; // Adjust to account for the chat view's padding
            let effective_width = (total_width - appender_offset).max(400.0);
            let max_w = effective_width.min(max_msg_width);

            ui.horizontal(|ui| {
                ui.set_max_width(max_w);
                ui.vertical(|ui| {
                    ui.set_max_width(max_w);

                    egui::Frame::default()
                        .outer_margin(Margin { top: 1, right: 1, bottom: 1, left: 1 })
                        .inner_margin(1.0)
                        .fill(ui.visuals().faint_bg_color)
                        .corner_radius(5.0)
                        .show(ui, |ui| {
                            let mut draft_note = chat.draft_note.clone();
                            let num_lines = draft_note.lines().count().max(1);

                            let syntax = if chat.draft_is_rhai { Syntax::rhai() } else { Syntax::from_mime("text/markdown") };

                            let mut padded_draft = draft_note.clone();
                            padded_draft.push('\n');

                            let out = CodeEditor::default()
                                .id_source(format!("draft_{}", chat_id))
                                .with_theme(ColorTheme::SV)
                                .with_syntax(syntax)
                                .with_numlines(false)
                                .with_rows(num_lines + 1)
                                .vscroll(false)
                                .v_auto_shrink(true) // Uncap height to display full text
                                .desired_width(max_w - 50.0)
                                .show(ui, &mut padded_draft);

                            if padded_draft.ends_with('\n') {
                                padded_draft.pop();
                                if padded_draft.ends_with('\r') {
                                    padded_draft.pop();
                                }
                            }
                            draft_note = padded_draft;

                    // Optional placeholder text if empty
                    if draft_note.is_empty() && !out.output.response.has_focus() {
                        ui.painter().text(
                            out.output.response.rect.min + egui::vec2(2.0, 2.0),
                            egui::Align2::LEFT_TOP,
                            "Click to add a note or executable script...",
                            egui::FontId::monospace(14.0),
                            ui.visuals().text_color().linear_multiply(0.5)
                        );
                    }

                    let shortcut = egui::KeyboardShortcut::new(egui::Modifiers::CTRL, egui::Key::Enter);
                    if out.output.response.has_focus() && ui.input_mut(|i| i.consume_shortcut(&shortcut)) {
                        out.output.response.surrender_focus();
                        draft_lost_focus = true; // Force commit immediately
                    }

                    if out.output.response.changed() || draft_note != chat.draft_note {
                        let mut new_is_rhai = chat.draft_is_rhai;
                        if !new_is_rhai && inforno_core::scripting::is_likely_rhai(&draft_note) {
                            new_is_rhai = true;
                        } else if new_is_rhai && draft_note.trim().is_empty() {
                            new_is_rhai = false;
                        }
                        new_draft = Some((draft_note.clone(), new_is_rhai));
                    }

                    if out.output.response.lost_focus() {
                        draft_lost_focus = true;
                    }
                });
                }); // End vertical wrapper
                ui.allocate_space(egui::vec2(ui.available_width(), 0.0));
            }); // End horizontal wrapper
        }
    }

    // Apply mutable updates outside of the chat borrow
    if !content_updates.is_empty() || new_draft.is_some() || draft_lost_focus || !rhai_updates.is_empty() || llm_prompt_request.is_some() || !delete_requests.is_empty() {

        let mut extracted_draft_to_save = None;

        if let Some(chat) = state.open_chats.get_mut(&chat_id) {
            // Apply Deletions
            for id in &delete_requests {
                chat.msg_pool.remove(id);
                for agent in chat.agents.iter_mut() {
                    agent.msg_ids.retain(|x| x != id);
                    let _ = inforno_core::db::mod_agent_msgs(&state.db_conn, agent.id, &agent.msg_ids);
                }
                let _ = inforno_core::db::delete_msg(&state.db_conn, *id);
            }

            for (id, content, unsaved) in content_updates {
                if let Some(m) = chat.msg_pool.get_mut(&id) {
                    m.content = content;
                    m.is_unsaved = unsaved;
                }
            }
            for (id, out_text) in rhai_updates {
                if let Some(m) = chat.msg_pool.get_mut(&id) {
                    m.volatile_output = Some(out_text);
                }
            }
            if let Some((nd, is_rhai)) = new_draft {
                chat.draft_note = nd;
                chat.draft_is_rhai = is_rhai;
            }
            if draft_lost_focus {
                let draft = chat.draft_note.trim();
                if !draft.is_empty() {
                    // Extract the content safely, then clear it!
                    extracted_draft_to_save = Some(chat.draft_note.clone());
                    chat.draft_note.clear();
                }
            }
        } // <--- THE BORROW ON state.open_chats DROPS HERE!

        // Now we can safely mutate the HashMap without conflicts
        if let Some(draft_content) = extracted_draft_to_save {
            let mut actual_chat_id = chat_id;

                    // 1. Promote to a persistent chat if we are in the placeholder (temp IDs are <= 0)
                    if actual_chat_id <= 0 {
                        let mut new_chat = inforno_core::common::Chat::default();

                        // Derive the title from the first non-empty line of the draft
                        let title = draft_content.lines()
                            .find(|l| !l.trim().is_empty())
                            .unwrap_or("Notebook")
                            .trim();

                        // Truncate if it's too long
                        new_chat.title = if title.chars().count() > 40 {
                            format!("{}...", title.chars().take(37).collect::<String>())
                        } else {
                            title.to_string()
                        };

                        if let Ok(()) = inforno_core::db::mk_chat(&state.db_conn, &mut new_chat) {
                            actual_chat_id = new_chat.id;

                            // We removed the manual 'Omnis' creation here because
                            // Chat::default() and mk_chat() already handle it perfectly!

                            state.open_chats.insert(actual_chat_id, new_chat);
                            state.active_chat_id = Some(actual_chat_id);

                            // Queue the UI tab update in egui's memory since the pane_tree
                            // is currently borrowed by the layout engine!
                            ui.data_mut(|d| {
                                let mut upgrades = d.get_temp::<Vec<(i64, i64)>>(egui::Id::new("tab_upgrades")).unwrap_or_default();
                                upgrades.push((chat_id, actual_chat_id));
                                d.insert_temp(egui::Id::new("tab_upgrades"), upgrades);
                            });

                            // Refresh sidebar
                            crate::state::reload_db_chats(&state.db_conn, &mut state.db_chats);
                        }
                    }

            // 2. Append the Note Cell Message to the Chat
            let mut new_msg = inforno_core::common::ChatMsg {
                id: 0,
                msg_role: inforno_core::common::MsgRole::Developer,
                content: draft_content, // Use the extracted String
                ..Default::default()
            };

            if let Ok(()) = inforno_core::db::mk_msg(&state.db_conn, &mut new_msg) {
                let new_id = new_msg.id;
                // Safely grab a fresh mutable borrow to the specific target chat
                if let Some(target_chat) = state.open_chats.get_mut(&actual_chat_id) {
                    target_chat.msg_pool.insert(new_id, new_msg);
                    for agent in target_chat.agents.iter_mut() {
                        agent.msg_ids.push(new_id);
                        let _ = inforno_core::db::mod_agent_msgs(&state.db_conn, agent.id, &agent.msg_ids);
                    }
                }
            }
        }
    }

    for (id, content) in db_updates {
        let _ = inforno_core::db::mod_msg_content(&state.db_conn, id, &content);
    }

    // --- NEW: Render Floating Minimalist Slider at the top of the viewport ---
    let clip_rect = ui.clip_rect();
    let mut slider_rect = clip_rect;
    slider_rect.max.y = slider_rect.min.y + 12.0;

    let mut ui_top = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(slider_rect)
            .layout(egui::Layout::top_down(egui::Align::Center))
    );
    ui_top.scope(|ui| {
        // Determine if the mouse is inside the slider's bounding box
        let pointer_pos = ui.input(|i| i.pointer.hover_pos());
        let is_hovered = pointer_pos.map_or(false, |pos| slider_rect.contains(pos));

        // Fetch the drag state of THIS specific slider from the previous frame
        let state_id = ui.id().with("slider_drag_state");
        let was_dragged_last_frame = ui.data(|d| d.get_temp::<bool>(state_id).unwrap_or(false));

        let show_slider = is_hovered || was_dragged_last_frame;

        let visuals = ui.visuals_mut();


        // If the mouse is not nearby and we aren't dragging the handle, hide the grabber itself
        if !show_slider {
            // Hide the background rail completely
            visuals.widgets.inactive.bg_fill = egui::Color32::TRANSPARENT;
            visuals.widgets.inactive.bg_stroke = egui::Stroke::NONE;
            visuals.widgets.hovered.bg_fill = egui::Color32::TRANSPARENT;
            visuals.widgets.hovered.bg_stroke = egui::Stroke::NONE;
            visuals.widgets.active.bg_fill = egui::Color32::TRANSPARENT;
            visuals.widgets.active.bg_stroke = egui::Stroke::NONE;

            visuals.widgets.inactive.fg_stroke = egui::Stroke::NONE;
        }

        let prev_width = max_msg_width;
        let resp = ui.add_sized(
            [ui.available_width(), 12.0],
            // Allow the slider to extend to the current window size OR the currently saved max
            egui::Slider::new(&mut max_msg_width, 400.0..=(total_width.max(400.0).max(prev_width)))
                .show_value(false)
                .text("")
        );

        if show_slider {
            resp.clone().on_hover_text(
                    egui::RichText::new(t!("adjust_message_width_tooltip"))
                    .strong()
                    .heading()
                );
        }

        // Save the exact drag state of this slider for the NEXT frame
        ui.data_mut(|d| d.insert_temp(state_id, resp.dragged()));

        // Instantly save state if the slider was dragged this frame
        if prev_width != max_msg_width {
            state.chat_widths.insert(chat_id, max_msg_width);
            ui.ctx().request_repaint();
        }
    });
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
    active_realm: &Option<inforno_core::realm::ActiveRealm>,
    op_tx: &std::sync::mpsc::Sender<inforno_core::common::FileOpMsg>,
    max_msg_width: f32,
    rhai_updates: &mut Vec<(i64, String)>,
    llm_prompt_request: &mut Option<String>,
    delete_requests: &mut Vec<i64>,
) {
    let effective_width = total_width - 38.0;
    let item_min_width = 400.0;
    let item_max_width = max_msg_width;
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
                                    project_root, active_realm, op_tx, rhai_updates, llm_prompt_request, delete_requests);
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
    active_realm: &Option<inforno_core::realm::ActiveRealm>,
    op_tx: &std::sync::mpsc::Sender<inforno_core::common::FileOpMsg>,
    max_msg_width: f32,
    rhai_updates: &mut Vec<(i64, String)>,
    llm_prompt_request: &mut Option<String>,
    delete_requests: &mut Vec<i64>,
) {
    let left_offset = 127.0; // Matches the left outer margin of the user frame
    let right_padding = 30.0;

    // Subtract the 127px offset from both our total and maximum bounds
    // to ensure the frame + margin combination respects the global max_msg_width limit
    let effective_width = (total_width - right_padding - left_offset).max(400.0);
    let adjusted_max = (max_msg_width - left_offset).max(400.0);

    let max_w = effective_width.min(adjusted_max);

    let scroll_area = egui::ScrollArea::horizontal();

    scroll_area.show(ui, |ui| {
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
                    render_msg_header(ui, msg_ui, &msg.msg_role.to_string(), msg, delete_requests);
                    render_msg_content(ui, cache, msg, msg_ui, (max_w - 20.0) as usize, math_cache.clone(),
                        project_root, active_realm, op_tx, rhai_updates, llm_prompt_request);

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
                                                                    let max_img_w = 300.0_f32;
                                                                    let scale = if size.x > max_img_w { max_img_w / size.x } else { 1.0 };

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
    active_realm: &Option<inforno_core::realm::ActiveRealm>,
    op_tx: &std::sync::mpsc::Sender<inforno_core::common::FileOpMsg>,
    rhai_updates: &mut Vec<(i64, String)>,
    llm_prompt_request: &mut Option<String>,
    delete_requests: &mut Vec<i64>,
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
            render_msg_header(ui, msg_ui, &label, msg, delete_requests);

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
                project_root, active_realm, op_tx, rhai_updates, llm_prompt_request);
        });
    });
}

fn render_msg_header(
    ui: &mut egui::Ui,
    msg_ui: &mut ChatMsgUi,
    label: &str,
    msg: &ChatMsg, // Changed from msg_id: i64 to msg: &ChatMsg
    delete_requests: &mut Vec<i64>,
) {
    ui.horizontal(|ui| {
        let popup_id = ui.make_persistent_id(format!("msg_wrench_{}", msg.id));
        let wrench_resp = ui.button("🔧").on_hover_text("Message Options");
        egui::Popup::from_toggle_button_response(&wrench_resp)
        .id(popup_id)
        .close_behavior(egui::PopupCloseBehavior::CloseOnClick)
        .show(|ui| {
            ui.set_min_width(140.0); // Prevent text wrapping
            ui.with_layout(egui::Layout::top_down_justified(egui::Align::LEFT), |ui| {
                if ui.add(egui::Button::new(egui::RichText::new(t!("delete_msg_btn"))
                .color(ui.visuals().error_fg_color))
                .frame(false))
                .on_hover_text(
                    egui::RichText::new(t!("delete_msg_tooltip"))
                    .strong()
                    .heading()
                )
                .clicked() {
                    delete_requests.push(msg.id);
                    ui.close();
                }
            });
        });

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
    msg_ui: &mut ChatMsgUi,
    max_image_width: usize,
    math_cache: std::rc::Rc<std::cell::RefCell<std::collections::HashMap<String, std::sync::Arc<[u8]>>>>,
    project_root: &Option<std::path::PathBuf>,
    active_realm: &Option<inforno_core::realm::ActiveRealm>,
    op_tx: &std::sync::mpsc::Sender<inforno_core::common::FileOpMsg>,
    rhai_updates: &mut Vec<(i64, String)>,
    llm_prompt_request: &mut Option<String>,
) {
    if msg_ui.show_raw {
        ui.label(RichText::new(format!("{}", msg.content)).strong());
    } else {
        // Break the content into pieces
        let chunks = inforno_core::parsing::parse_chunks(&msg.content);

        for (i, chunk) in chunks.into_iter().enumerate() {
            match chunk {
                inforno_core::parsing::ContentChunk::Markdown(md_text) => {
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

                inforno_core::parsing::ContentChunk::Code { lang, code, filepath } => {
                    // 1. Evaluate the precise MIME type
                    let mut mime_type = bulat::editor::Syntax::guess_mime_from_markdown_lang(lang);

                    // If the markdown tag was missing or unrecognized (defaulting to text/plain),
                    // fall back to guessing based on the file extension from the header.
                    if mime_type == "text/plain" {
                        if let Some(path) = &filepath {
                            mime_type = bulat::editor::Syntax::guess_mime_from_path(std::path::Path::new(path));
                        }
                    }

                    if mime_type == "application/x-rhai" {
                        let mut code_buffer = code.to_string();
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

                                let mut display_buffer = code_buffer.clone();
                                display_buffer.push('\n');

                                CodeEditor::default()
                                    .id_source(format!("rhai_code_{}_{}", msg.id, i))
                                    .with_theme(ColorTheme::SV)
                                    .with_syntax(Syntax::rhai())
                                    .with_numlines(false)
                                    .with_rows(display_buffer.lines().count().max(1))
                                    .vscroll(false)
                                    .v_auto_shrink(true)
                                    .show(ui, &mut display_buffer);

                                if let Some(vol_out) = &msg.volatile_output {
                                    ui.separator();
                                    ui.label(egui::RichText::new("Output:").weak().small());
                                    let mut out_str = vol_out.clone();
                                    ui.add(
                                        egui::TextEdit::multiline(&mut out_str)
                                            .font(egui::TextStyle::Monospace)
                                            .interactive(false)
                                            .frame(egui::Frame::new())
                                            .desired_width(f32::INFINITY)
                                    );
                                }
                            });
                        ui.add_space(6.0);
                        continue;
                    }
                    let mut code_buffer = code.to_string();
                    let num_lines = code_buffer.lines().count().max(1);

                    ui.add_space(6.0);

                    let mut actual_path = None;
                    let mut autocorrected = false;
                    let mut display_path = String::new();

                    if let Some(path) = &filepath {
                        display_path = path.clone();
                        if let Some((resolved, corrected)) = inforno_core::realm::resolve_filepath(active_realm, project_root, path) {
                            actual_path = Some(resolved);
                            autocorrected = corrected;
                        }
                    }

                    // --- HEADER: Path, Merge Tool, and Copy Button ---
                    ui.horizontal(|ui| {
                        // Left side: Filepath and Merge button
                        if let Some(path) = &actual_path {
                            ui.spacing_mut().item_spacing.x = 6.0;

                            let mut btn_text = format!("📄 {}", display_path);
                            let mut tooltip = "Open file in editor".to_string();

                            if autocorrected {
                                btn_text.push_str(" ⚠️");
                                let rel_path_str = inforno_core::realm::get_relative_path(active_realm, project_root, path);
                                tooltip = format!("File path autocorrected to:\n{}", rel_path_str);
                            }


                            let open_resp = SplitButton::new(btn_text)
                                .id_salt(format!("open_btn_{}_{}", msg.id, i))
                                .main_tooltip(tooltip)
                                .arrow_tooltip(t!("right_button_tooltip"))
                                .show(ui);

                            if open_resp.main_clicked {
                                let _ = op_tx.send(inforno_core::common::FileOpMsg {
                                    op: inforno_core::common::FileOp::OpenEditor,
                                    cancelled: false,
                                    path: Some(path.clone()),
                                    ..Default::default()
                                });
                            }
                            if open_resp.arrow_clicked {
                                let _ = op_tx.send(inforno_core::common::FileOpMsg {
                                    op: inforno_core::common::FileOp::OpenEditorRight,
                                    cancelled: false,
                                    path: Some(path.clone()),
                                    ..Default::default()
                                });
                            }

                            let merge_resp = SplitButton::new(t!("open_in_merge_tool_btn"))
                                .id_salt(format!("merge_btn_{}_{}", msg.id, i))
                                .main_tooltip(t!("open_in_merge_tool_tooltip"))
                                .arrow_tooltip(t!("right_button_tooltip"))
                                .show(ui);

                            let trigger_merge = |right_pane: bool| {
                                let original_content = std::fs::read_to_string(path).unwrap_or_default();

                                // 1. Strip leading `--- File: ...` metadata line
                                // 1. Strip leading `--- File: ...` metadata line
                                static RE_STRIP_FILE: OnceLock<Regex> = OnceLock::new();
                                let re_strip = RE_STRIP_FILE.get_or_init(|| {
                                    Regex::new(r"(?im)^[ \t]*(?://|/\*|#)?[ \t]*(?:---[ \t]*(?:File:)?[ \t]*|File:[ \t]*)[a-z0-9_/\.\-]+\.[a-z]+[ \t]*(?:---|\*/)?\r?\n?").unwrap()
                                });

                                let stripped_owned = re_strip.replace(code_buffer.as_str(), "");
                                let mut clean_snippet = stripped_owned.as_ref();

                                // Fallback manual strip if the LLM just sent an empty "---" or similar malformed line
                                let trimmed = clean_snippet.trim_start();
                                if trimmed.starts_with("---") || trimmed.starts_with("// ---") || trimmed.starts_with("/* ---") {
                                    if let Some(nl) = clean_snippet.find('\n') {
                                        clean_snippet = &clean_snippet[nl + 1..];
                                    } else {
                                        clean_snippet = ""; // Single line snippet completely removed
                                    }
                                }

                                // NEW: If the LLM left a standalone filename right before the <<<< marker, strip it manually
                                let mut lines = clean_snippet.lines().filter(|l| !l.trim().is_empty());
                                if let Some(first_line) = lines.next() {
                                    if let Some(second_line) = lines.next() {
                                        if second_line.trim_start().starts_with("<<<<") {
                                            // Ensure the first line is just a stray filename/comment and not actual code
                                            if first_line.contains('.') && !first_line.contains('{') && !first_line.contains('(') && !first_line.contains(';') {
                                                // Mathematically slice off the first line to guarantee we don't corrupt the diff marker's indentation
                                                let offset = (second_line.as_ptr() as usize) - (clean_snippet.as_ptr() as usize);
                                                clean_snippet = &clean_snippet[offset..];
                                            }
                                        }
                                    }
                                }

                                // 2. Try applying LLM Search/Replace Diffs
                                let mut right_content = bulat::engine::apply_llm_diffs(&original_content, clean_snippet);

                                // 3. Try fallback to function splicing
                                if right_content.is_none() {
                                    right_content = bulat::engine::try_splice_snippet(&original_content, clean_snippet);
                                }

                                // 4. Final fallback to the whole snippet body
                                let final_right_content = right_content.unwrap_or_else(|| clean_snippet.to_string());

                                let _ = op_tx.send(inforno_core::common::FileOpMsg {
                                    op: if right_pane { inforno_core::common::FileOp::OpenMergeRight } else { inforno_core::common::FileOp::OpenMerge },
                                    path: Some(path.clone()),
                                    left_content: Some(original_content),
                                    right_content: Some(final_right_content),
                                    ..Default::default()
                                });
                            };

                            if merge_resp.main_clicked { trigger_merge(false); }
                            if merge_resp.arrow_clicked { trigger_merge(true); }

                        } else if let Some(path) = &filepath {
                            // Fallback button if there's no project root but we have a filepath
                            if ui.button(format!("📝 {}", path)).on_hover_text("Open file in editor").clicked() {
                                let _ = op_tx.send(inforno_core::common::FileOpMsg {
                                    op: inforno_core::common::FileOp::OpenEditor,
                                    cancelled: false,
                                    path: Some(std::path::PathBuf::from(path)),
                                    ..Default::default()
                                });
                            }
                        } else {
                            let display_name = match mime_type {
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
                            };

                            ui.label(egui::RichText::new(display_name).weak());
                        }

                        // Right side: Copy Button AND Inline Diff Tools
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.button("🗐").on_hover_text("Copy to clipboard").clicked() {
                                ui.ctx().copy_text(code.to_string());
                            }

                            if let Some(path) = &actual_path {
                                // --- NEW: INLINE DIFF DETECTOR ---
                                // 1. Check if the LLM provided a formal <<<< ==== >>>> block
                                if let Some((search_block, replace_block)) = bulat::engine::extract_raw_diff_blocks(&code_buffer) {
                                    // 2. Read the live file to verify the exact SEARCH block exists right now
                                    let original_content = std::fs::read_to_string(path).unwrap_or_default();

                                    // We check using exact matches or normalized whitespace matching.
                                    let mut found_search_match = false;
                                    let mut found_replace_match = false;
                                    let mut match_offset_lines = 0;

                                    let search_norm = search_block.replace("\r\n", "\n").trim().to_string();
                                    let replace_norm = replace_block.replace("\r\n", "\n").trim().to_string();
                                    let orig_norm = original_content.replace("\r\n", "\n");

                                    if !search_block.is_empty() {
                                        if let Some(idx) = original_content.find(&search_block) {
                                            found_search_match = true;
                                            // Count absolute newlines before the match index
                                            match_offset_lines = original_content[..idx].chars().filter(|&c| c == '\n').count();
                                        } else if !search_norm.is_empty() {
                                            if let Some(idx) = orig_norm.find(&search_norm) {
                                                found_search_match = true;
                                                match_offset_lines = orig_norm[..idx].chars().filter(|&c| c == '\n').count();
                                            }
                                        }
                                    }

                                    if !replace_block.is_empty() {
                                        if let Some(idx) = original_content.find(&replace_block) {
                                            found_replace_match = true;
                                            if !found_search_match {
                                                match_offset_lines = original_content[..idx].chars().filter(|&c| c == '\n').count();
                                            }
                                        } else if !replace_norm.is_empty() {
                                            if let Some(idx) = orig_norm.find(&replace_norm) {
                                                found_replace_match = true;
                                                if !found_search_match {
                                                    match_offset_lines = orig_norm[..idx].chars().filter(|&c| c == '\n').count();
                                                }
                                            }
                                        }

                                        // Safety check: if we only found the replace block, ensure it's not a generic one-liner
                                        if found_replace_match && !found_search_match {
                                            if replace_norm.lines().count() <= 1 && replace_norm.len() <= 20 {
                                                found_replace_match = false; // Too generic to confidently assume it's our applied snippet!
                                            }
                                        }
                                    }

                                    if found_search_match || found_replace_match {
                                        // 3. Mount or retrieve the DiffApp for this chunk!
                                        // We avoid holding a lock on msg_ui by performing isolated operations
                                        if !msg_ui.inline_diffs.contains_key(&i) {
                                            let mut app = bulat::DiffApp::new(search_block.clone(), replace_block.clone())
                                                .with_line_offset(match_offset_lines);
                                            app.embedded = true; // Request full height!

                                            // Securely map the path back to the Realm or Project Root
                                            let rel_path = inforno_core::realm::get_relative_path(active_realm, project_root, &path);
                                            app.left_filepath = Some(rel_path);

                                            msg_ui.inline_diffs.insert(i, app);
                                        }

                                        // --- NEW: Dynamic check to see if it's already applied on disk ---
                                        if found_replace_match {
                                            // If the search block is gone but replace block is there, it's definitely merged.
                                            // Otherwise, apply the same overlap/length safeguards.
                                            if !found_search_match || replace_norm.contains(&search_norm) || replace_norm.lines().count() > 1 || replace_norm.len() > 20 {
                                                msg_ui.inline_diffs_saved.insert(i, true);
                                            }
                                        }

                                        let is_saved = *msg_ui.inline_diffs_saved.entry(i).or_insert(false);

                                        ui.add_space(10.0);
                                        if is_saved {
                                            ui.label(egui::RichText::new("✔ Merged").color(egui::Color32::GREEN));
                                        } else {
                                            let mut save_left = false;
                                            let mut save_right = false;

                                            if ui.button("💾 Save").on_hover_text("Save the manually merged code (Left side).").clicked() {
                                                save_left = true;
                                            }
                                            if ui.button("⚡ Quick Merge").on_hover_text("Instantly apply the LLM's full replacement (Right side).").clicked() {
                                                save_right = true;
                                            }

                                            if save_left || save_right {
                                                // 4. PERFORM LIVE SAVE
                                                let latest_disk_content = std::fs::read_to_string(path).unwrap_or_default();

                                                let diff_app_ref = msg_ui.inline_diffs.get(&i).unwrap();
                                                let mut current_replacement = if save_right {
                                                    diff_app_ref.right_code_real.clone()
                                                } else {
                                                    diff_app_ref.left_code_real.clone()
                                                };

                                                // Remove the single trailing newline that DiffApp::new unconditionally appended.
                                                // This prevents the "empty line insertion" bug during inline merges.
                                                if current_replacement.ends_with('\n') {
                                                    current_replacement.pop();
                                                    if current_replacement.ends_with('\r') {
                                                        current_replacement.pop();
                                                    }
                                                }

                                                // --- PREVENT DOUBLE-APPLY ON FAST CLICKS ---
                                                let replace_norm = current_replacement.replace("\r\n", "\n").trim().to_string();
                                                let search_norm = search_block.replace("\r\n", "\n").trim().to_string();
                                                let disk_norm = latest_disk_content.replace("\r\n", "\n");

                                                if !replace_norm.is_empty() && disk_norm.contains(&replace_norm) {
                                                    if replace_norm.contains(&search_norm) || replace_norm.lines().count() > 1 || replace_norm.len() > 20 {
                                                        msg_ui.inline_diffs_saved.insert(i, true);
                                                        return; // Exit the UI closure early, skipping the write!
                                                    }
                                                }

                                                // We must find it again in case the file changed since we last rendered
                                                let mut replaced_successfully = false;
                                                let mut new_text = String::new();

                                                if let Some(idx) = latest_disk_content.find(&search_block) {
                                                    new_text.push_str(&latest_disk_content[..idx]);
                                                    new_text.push_str(&current_replacement);
                                                    new_text.push_str(&latest_disk_content[idx + search_block.len()..]);
                                                    replaced_successfully = true;
                                                } else {
                                                    // Try normalized fallback
                                                    let search_norm = search_block.replace("\r\n", "\n").trim().to_string();
                                                    let orig_norm = latest_disk_content.replace("\r\n", "\n");
                                                    if let Some(idx) = orig_norm.find(&search_norm) {
                                                        let replace_norm = current_replacement.replace("\r\n", "\n");
                                                        new_text.push_str(&orig_norm[..idx]);
                                                        new_text.push_str(&replace_norm);
                                                        new_text.push_str(&orig_norm[idx + search_norm.len()..]);
                                                        replaced_successfully = true;
                                                    }
                                                }

                                                if replaced_successfully {
                                                    if let Err(e) = std::fs::write(path, new_text) {
                                                        eprintln!("Failed to quick merge: {}", e);
                                                    } else {
                                                        msg_ui.inline_diffs_saved.insert(i, true);
                                                    }
                                                } else {
                                                    eprintln!("Quick merge failed: Target block modified or no longer matches.");
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        });
                    });

                    // If we successfully initialized an inline DiffApp for this block, show THAT instead of the raw text.
                    if let Some(diff_app) = msg_ui.inline_diffs.get_mut(&i) {
                        ui.push_id(format!("diff_app_wrapper_{}_{}", msg.id, i), |ui| {
                            egui::Frame::default()
                                .inner_margin(4.0)
                                .stroke(egui::Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color))
                                .show(ui, |ui| {
                                    diff_app.show(ui);
                                });
                        });
                    } else {
                        // Otherwise, just show the normal LLM raw text
                        let mut display_buffer = code_buffer.clone();
                        display_buffer.push('\n');
                        let display_lines = display_buffer.lines().count().max(1);

                        CodeEditor::default()
                            .id_source(format!("code_block_{}_{}", msg.id, i))
                            .with_theme(ColorTheme::SV)
                            .with_syntax(Syntax::from_mime(mime_type))
                            .with_numlines(false)
                            .with_rows(display_lines)
                            // Disable internal scroll so the parent chat window handles scrolling natively
                            .vscroll(false)
                            .v_auto_shrink(true) // Uncap height to display full snippet
                            .show(ui, &mut display_buffer);

                    }

                    ui.add_space(6.0);
                }
            }
        }
    }
}

fn render_reasoning_block(ui: &mut egui::Ui, text: &str,
        id_salt: impl std::hash::Hash + std::fmt::Debug) {
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
