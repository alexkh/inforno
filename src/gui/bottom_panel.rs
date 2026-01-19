use std::sync::{Arc, atomic::{AtomicBool, Ordering}};

use egui::{Key, Modifiers, Ui};
use rusqlite::Connection;
use rust_i18n::t;

use crate::{common::{Agent, ChatMsg, ChatQue, ChatRouter, ChatStreamEvent, MsgRole, PresetSelection, Presets, cloud_color, local_color, router_color, run_chat_stream_router, text_color}, db::{fetch_chat, mk_chat, mk_msg, mod_agent_msgs, mod_agent_preset, update_agent_preset_snapshot}, gui::{State, agent_config::AgentConfigState, reload_db_chats}};

pub struct BottomPanelState {
    pub col1_width: f32,
    pub col2_width: f32,
    pub height: f32,
    pub row_height: f32,
    pub height_modified: bool,
    pub desired_rows: usize,
    pub system_prompt_edited: String,
    pub prompt_edited: String,
    pub show_system_prompt: bool,
}

impl Default for BottomPanelState {
    fn default() -> Self {
        Self {
            col1_width: 80.0,
            col2_width: 250.0,
            height: 80.0,
            row_height: 0.0,
            height_modified: false,
            desired_rows: 5,
            system_prompt_edited: String::new(),
            prompt_edited: String::new(),
            show_system_prompt: false,
        }
    }
}

pub fn ui_bottom_panel(ctx: &egui::Context, state: &mut State) {
    // 1. Extract state values we might modify locally
    let mut col1_w = state.bottom_panel_state.col1_width;
    let mut col2_w = state.bottom_panel_state.col2_width;
    let mut panel_h = state.bottom_panel_state.height;

    egui::TopBottomPanel::bottom("chat_input_panel")
        .resizable(false) // We implement custom resizing below
        .exact_height(panel_h)
        .show(ctx, |ui| {

            if state.is_modal_open {
                ui.disable();
            }

            // 2. Handle the manual resize logic (returns updated height)
            panel_h = handle_panel_resize(ui, state, panel_h);

            // 3. Main Horizontal Layout
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Min), |ui| {
                let panel_height = ui.available_height();

                // Toggle Button
                if ui.add(egui::Button::new("💻").small().selected(
                        state.bottom_panel_state.show_system_prompt))
                        .on_hover_text(t!("toggle_system_prompt"))
                        .clicked()
                {
                    state.bottom_panel_state.show_system_prompt =
                            !state.bottom_panel_state.show_system_prompt;
                }

                // --- Column 1: System Prompt ---
                if state.bottom_panel_state.show_system_prompt {
                    ui.allocate_ui(egui::vec2(col1_w, panel_height), |ui| {
                        ui.set_width(col1_w);
                        render_system_prompt_col(ui, state);
                    });
                    // Splitter 1
                    vertical_splitter(ui, &mut col1_w);
                }

                // --- Column 2: User Prompt ---
                ui.allocate_ui(egui::vec2(col2_w, panel_height), |ui| {
                    ui.set_width(col2_w);
                    render_user_prompt_col(ui, state, panel_height);
                });

                // Splitter 2
                vertical_splitter(ui, &mut col2_w);

                // --- Column 3: Actions (Send & Presets) ---
                render_actions_col(ui, state, ctx);
            });
        });

    // 4. Write modified sizes back to state
    state.bottom_panel_state.col1_width = col1_w;
    state.bottom_panel_state.col2_width = col2_w;
    state.bottom_panel_state.height = panel_h;
}

// --- Helper Functions ---

/// Handles the thin strip at the top used to resize the panel height
fn handle_panel_resize(ui: &mut Ui, state: &mut State, mut current_height: f32)
            -> f32 {
    let (_rect, response) = ui.allocate_at_least(
        egui::vec2(ui.available_width(), 2.0),
        egui::Sense::drag()
    );

    let response = response.on_hover_cursor(egui::CursorIcon::ResizeVertical);

    if response.dragged() {
        // Delta Y is negative when moving UP, so we subtract it to increase height
        current_height -= ui.input(|i| i.pointer.delta().y);
        current_height = current_height.clamp(80.0, f32::MAX);
        state.bottom_panel_state.height_modified = true;
    }

    current_height
}

fn render_system_prompt_col(ui: &mut Ui, state: &mut State) {
    egui::ScrollArea::vertical()
    .id_salt("system_prompt_scroll")
    .show(ui, |ui| {
        egui::Frame::new()
            .stroke(egui::Stroke::new(1.0, ui.visuals().hyperlink_color))
            .corner_radius(ui.visuals().widgets.active.corner_radius)
            .show(ui, |ui| {
                ui.add(
                    egui::TextEdit::multiline(
                        &mut state.bottom_panel_state.system_prompt_edited)
                        .desired_width(f32::INFINITY)
                        .desired_rows(state.bottom_panel_state.desired_rows)
                        .hint_text(t!("system_prompt_optional"))
                )
            });
    });
}

fn render_user_prompt_col(ui: &mut Ui, state: &mut State, panel_height: f32) {
    egui::ScrollArea::vertical()
        .id_salt("prompt_scroll")
        .show(ui, |ui| {
            let response = ui.add(
                egui::TextEdit::multiline(
                            &mut state.bottom_panel_state.prompt_edited)
                    .desired_width(f32::INFINITY)
                    .desired_rows(state.bottom_panel_state.desired_rows)
                    .hint_text(t!("enter_prompt_here")),
            );

            // Calculate row height once if unknown
            if state.bottom_panel_state.row_height == 0.0 {
                state.bottom_panel_state.row_height =
                        response.rect.height() /
                        (state.bottom_panel_state.desired_rows as f32);
                state.bottom_panel_state.height_modified = true;
            }

            // Recalculate desired rows if panel height changed
            if state.bottom_panel_state.height_modified {
                if state.bottom_panel_state.row_height > 0.0 {
                    state.bottom_panel_state.desired_rows =
                            (panel_height / state.bottom_panel_state.row_height)
                            as usize;
                }
                state.bottom_panel_state.height_modified = false;
            }
        });
}

fn render_actions_col(ui: &mut Ui, state: &mut State,  ctx: &egui::Context) {
    let mut do_send_prompt_now = false;

    // Check for Ctrl+Enter
    if ui.input_mut(|i| i.consume_key(Modifiers::CTRL, Key::Enter)) {
        do_send_prompt_now = true;
    }

    let btn_height = state.bottom_panel_state.desired_rows as f32
            * state.bottom_panel_state.row_height;

    // Safety check to ensure button has some height even if rows calc fails
    let actual_btn_height = if btn_height > 0.0 { btn_height } else { 30.0 };

    let button_text = if state.chat_streaming_state.streaming {
        "⏹ Stop".to_string()
    } else {
        t!("send_prompt_btn").to_string()
    };

    let send_btn = egui::Button::new(button_text).wrap()
            .selected(state.chat_streaming_state.streaming);

    let send_clicked = ui.add_sized(
        [80.0, actual_btn_height],
        send_btn,
    ).clicked();

    // only submit prompt if there is text entered
    if do_send_prompt_now || send_clicked {
        if state.chat_streaming_state.streaming {
            if let Some(flag) = &state.chat_streaming_state.abort_flag {
                flag.store(true, Ordering::Relaxed);
            }
            return;
        }
        if !state.bottom_panel_state.prompt_edited.is_empty() {
            submit_prompt(state, ctx);
        } else {
            state.error_msg = Some(t!("error_empty_prompt").to_string());
            state.is_modal_open = true;
        }
    }

    egui::ScrollArea::vertical().id_salt("agent_scroll").show(ui, |ui| {
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(2.0, 2.0);
            ui.vertical(|ui| {
                // Prepare disjoint borrows
                // We need to mutate agents, but only read presets.
                // Rust can usually handle this if we access fields directly,
                // but destructuring makes it explicit and safe.
                let presets = &state.presets;
                let agents = &mut state.chat.agents;

                for (i, agent) in agents.iter_mut().enumerate().skip(1) {
                    if agent.deleted { continue; }
                    ui.horizontal(|ui| {
                        ui.set_width(ui.available_width());
                        ui.spacing_mut().item_spacing.x = 4.0;

                        // We create a unique ID for the combo box using the index
                        let id_source = format!("chat_agent_{}", i);
                        ui.label(format!("{}", agent.id));
                        render_agent(ui, agent, &mut state.agent_config_state,
                                presets, &id_source, &state.db_conn);
                    });
                }

                // In your UI update loop
                let is_full = state.chat.agents.len() >= 127;

                if ui.add_enabled(!is_full, egui::Button::new("+"))
                .on_hover_text(if is_full { "Max agents reached" } else
                        { "Add another agent" })
                .clicked() {
                    // create a new agent right inside chat, sync it to db
                    if let Err(e) = state.chat.add_agent_try_sync(
                        &state.db_conn) {
                        eprintln!("Failed to add agent: {}", e);
                        state.error_msg = Some(
                                format!("Failed to add agent: {}", e));
                        state.is_modal_open = true;
                    }
                }
            });
        });
    });
}

/// Renders a single agent's controls (Label + Preset Selector)
fn render_agent(
    ui: &mut egui::Ui,
    agent: &mut Agent,
    substate: &mut AgentConfigState,
    presets: &Presets, // Assuming Presets type is defined
    id_source: &str,
    conn: &Connection,
) {
    // button that mutes/unmutes the agent
    let btn_text = &agent.name;
    let btn = egui::Button::new(btn_text).selected(!agent.muted);

    if ui.add(btn).on_hover_text("Click to Mute or Unmute").clicked() {
        agent.muted = !agent.muted;
    }

    // Sync the selection first (ensure title matches ID)
    agent.preset_selection.sync_with_presets(presets);

    // Preset Combo Box
    // We pass the specific agent's preset_selection
    if preset_combo_box(
        ui,
        id_source,
        &mut agent.preset_selection,
        presets
    ) {
        // value changed. Save it to the database
        let _ = mod_agent_preset(conn, agent.id, agent.preset_selection.id,
                presets.get(agent.preset_selection.id));
        agent.preset = presets.get(agent.preset_selection.id).cloned();
    }

    // wrench menu here 🔧
    if ui.button("🔧").on_hover_text("Modify this preset").clicked() {
        // A. Get the current preset data
        if let Some(current_preset) = presets.get(agent.preset_selection.id) {
            if let Some(agent_preset) = agent.preset.as_ref() {

                // B. Initialize the Agent Config Window State
                substate.target_agent_id = Some(agent.id);
                substate.target_agent_ind = Some(agent.agent_ind);

                // C. Clone the preset into the editor state
                substate.editor_state.edited_preset = agent_preset.clone();
                substate.editor_state.router_changed = true; // Trigger validation refresh

                // D. Set up UI strings (seed/temp) for the text inputs
                substate.editor_state.seed_entered = agent_preset.options.seed
                    .map(|s| s.to_string()).unwrap_or_default();
                substate.editor_state.temperature_entered =
                    agent_preset.options.temperature
                    .map(|t| t.to_string()).unwrap_or_default();

                substate.is_open = true;
            }
        }
    }

    if let Some(current) = &agent.preset {
        if let Some(original) = presets.get(agent.preset_selection.id) {
            if current.options.include_reasoning !=
                    original.options.include_reasoning {
                egui::Frame::new()
                .stroke(egui::Stroke::new(1.0, text_color()))
                .inner_margin(egui::Margin::symmetric(3, 0))
                .corner_radius(3.0)
                .show(ui, |ui| {
                    ui.label(format!("{} {}", t!("reasoning_label"),
                    current.options.include_reasoning
                    .map_or(t!("unset").to_string(), |s| {
                    if s { t!("yes").to_string() } else { t!("no").to_string()
                    }})));
                });
            }

            if current.options.temperature !=
                    original.options.temperature {
                egui::Frame::new()
                .stroke(egui::Stroke::new(1.0, text_color()))
                .inner_margin(egui::Margin::symmetric(3, 0))
                .corner_radius(3.0)
                .show(ui, |ui| {
                    ui.label(format!("{} {}", t!("temperature_label"),
                    current.options.temperature.map(|t| t.to_string())
                    .unwrap_or_else(|| t!("unset").to_string())));
                });
            }

            if current.options.seed !=
                   original.options.seed {
                egui::Frame::new()
                .stroke(egui::Stroke::new(1.0, text_color()))
                .inner_margin(egui::Margin::symmetric(3, 0))
                .corner_radius(3.0)
                .show(ui, |ui| {
                    ui.label(format!("{} {}", t!("seed_label"),
                    current.options.seed.map(|t| t.to_string())
                    .unwrap_or_else(|| t!("unset").to_string())));
                });
            }
/*
            // If we found changes, display the indicator
            if !changes.is_empty() {
                let text = changes.join(", ");
                let tooltip = format!("Modified options:\n{}", text);

                ui.label(
                    egui::RichText::new("*")
                        .color(egui::Color32::from_rgb(255, 165, 0)) // Orange warning color
                        .strong()
                )
                .on_hover_text(tooltip);
            }
*/
        }
    }
}

// returns true if changed
pub fn preset_combo_box(
    ui: &mut egui::Ui,
    salt: impl std::hash::Hash, // Unique ID for egui memory
    selection: &mut PresetSelection,
    presets: &Presets,
) -> bool {
    // 1. Sync Logic: Ensure index points to the right ID before drawing
    selection.sync_with_presets(presets);

    // 2. Prepare the Header Text (Closed Box)
    let current_text_widget = if selection.ind == usize::MAX {
        // No selection
        egui::RichText::new(t!("select_a_preset"))
                .color(ui.visuals().text_color())
    } else {
        // Selection exists
        let title = selection.title.clone();
        // Try to find the router for the current selection to color it
        if let Some(preset) = presets.get(selection.id) {
            let color = router_color(&preset.chat_router);
            egui::RichText::new(title).color(color)
        } else {
            // Fallback if lookup fails
            egui::RichText::new(title)
        }
    };

    let tooltip_text = presets
        .get(selection.id)
        .map(|p| p.tooltip.as_str())
        .unwrap_or("");
    let tooltip_color = presets
        .get(selection.id)
        .map(|p| router_color(&p.chat_router))
        .unwrap_or(ui.visuals().text_color());

    let mut changed = false;

    let response = egui::ComboBox::from_id_salt(salt)
    .height(500.0)
    .selected_text(current_text_widget)
    .show_ui(ui, |ui| {
        for (index, (id, title)) in presets.cache.iter().enumerate() {
            let is_selected = selection.id == *id;

            // Determine color for this specific item in the list
            let mut label_text = egui::RichText::new(title);
            if let Some(preset) = presets.get(*id) {
                label_text = label_text.color(router_color(
                    &preset.chat_router));
            }

            if ui.selectable_label(is_selected, label_text).clicked() {
                selection.ind = index;
                selection.id = *id;
                selection.title = title.clone();
                changed = true;
            }
        }
    });

    if !tooltip_text.is_empty() {
        response.response.on_hover_text(
            egui::RichText::new(tooltip_text)
            .strong()
            .heading()
            .color(tooltip_color)
        );
    }

    changed
}

fn submit_prompt(state: &mut State, ctx: &egui::Context) {
    // check if at least one agent has valid preset selected
    if !state.chat.agents.iter().skip(1).any(|a|
                state.presets.get(a.preset_selection.id).is_some()) {
        state.error_msg = Some(t!("error_no_agent_preset_selected").to_string());
        state.is_modal_open = true;
        return;
    }

    let prompt_text = state.bottom_panel_state.prompt_edited.clone();

    let rt_handle = state.perma.rt.clone();
    let tx_base = state.chat_streaming_state.tx.clone();

    // ---------------------------------------------------------
    // 1. Ensure Chat Exists
    // ---------------------------------------------------------
    if state.chat.id == 0 {
        // create a chat title from the first 40 characters of the prompt
        state.chat.title = state.bottom_panel_state.prompt_edited.chars()
                .take(40).collect::<String>();
        // create a chat in the database, so that we have the new chat id
        match mk_chat(&state.db_conn, &mut state.chat) {
            Ok(()) => {
                reload_db_chats(&state.db_conn, &mut state.db_chats);
            }
            Err(e) => {
                eprintln!("CRITICAL DB ERROR (create chat): {}", e);
                return;
            }
        }
    }

    // ---------------------------------------------------------
    // 2. Handle System Prompt (New Logic)
    // ---------------------------------------------------------
    // If the panel is visible and contains text, we send it as a System message
    // immediately before the User message, then hide the panel.
    if state.bottom_panel_state.show_system_prompt {
        let sys_content = state.bottom_panel_state.system_prompt_edited.trim();

        if !sys_content.is_empty() {
            let mut sys_msg = ChatMsg {
                id: 0,
                msg_role: MsgRole::System,
                content: state.bottom_panel_state.system_prompt_edited.clone(),
                ..Default::default()
            };

            // Save System Message to DB
            if let Ok(()) = mk_msg(&state.db_conn, &mut sys_msg) {
                // Add to memory pool
                state.chat.msg_pool.insert(sys_msg.id, sys_msg.clone());

                // Add this System Message ID to ALL agents (including Omnis/Index 0)
                for agent in state.chat.agents.iter_mut() {
                    agent.msg_ids.push(sys_msg.id);
                    // Sync this agent's history to DB
                    if let Err(e) = mod_agent_msgs(&state.db_conn, agent.id, &agent.msg_ids) {
                        eprintln!("DB ERROR (updating agent with sys msg): {}", e);
                    }
                }

                // Hide the panel so it doesn't send again automatically
                state.bottom_panel_state.show_system_prompt = false;
            } else {
                eprintln!("CRITICAL DB ERROR (save sys msg)");
            }
        }
    }

    // ---------------------------------------------------------
    // 3. Create & Save User Message
    // ---------------------------------------------------------
    let mut usr_msg = ChatMsg {
        id: 0, // Placeholder, will be updated from DB
        msg_role: MsgRole::User,
        content: prompt_text.clone(), // or .to_string()
        // Defaults (preset_id=0, preset=None, etc.) are handled by Default
        ..Default::default()
    };

    // ---------------------------------------------------------
    // 4. Persist User Message to DB (mk_msg)
    // ---------------------------------------------------------
    if let Err(e) = mk_msg(&state.db_conn, &mut usr_msg) {
        eprintln!("CRITICAL DB ERROR (save msg): {}", e);
        return;
    }

    // Now that we know it succeeded, capture the ID.
    // No 'mut' needed, and no wasted initialization.
    let usr_msg_id = usr_msg.id;

    state.chat.msg_pool.insert(usr_msg.id, usr_msg.clone());

    // ---------------------------------------------------------
    // 5. Prepare Multi-Agent Streaming
    // ---------------------------------------------------------
    println!("Starting streaming...");
    state.chat_streaming_state.streaming = true;
    state.chat_streaming_state.bitmask = 0;

    // create a new flag, while false it means don't abort
    let abort_flag = Arc::new(AtomicBool::new(false));
    state.chat_streaming_state.abort_flag = Some(abort_flag.clone());

    // ensure buffers are sized correctly for the number of agents
    let agent_count = std::cmp::min(state.chat.agents.len(), 128);
    state.chat_streaming_state.msg_ids.clear();
    state.chat_streaming_state.msg_ids.resize_with(
            agent_count, || 0);
    state.chat_streaming_state.content_buffers.clear();
    state.chat_streaming_state.content_buffers.resize_with(
            agent_count, || String::new());
    state.chat_streaming_state.reasoning_buffers.clear();
    state.chat_streaming_state.reasoning_buffers.resize_with(
            agent_count, || String::new());

    for (index, agent) in state.chat.agents.iter_mut().enumerate() {
        // add the user msg id to this agent's history
        // Note: System msg (if any) was already added in Step 2, so this comes after.
        agent.msg_ids.push(usr_msg_id);
        // save this message id to the database
        if let Err(e) = mod_agent_msgs(
                    &state.db_conn, agent.id, &agent.msg_ids) {
            eprintln!("DB ERROR (agent {}): {}", index, e);
            continue;
        }
    }

    // before streaming we create a snapshot of the current chat, because we
    // want to share it with all the streaming threads as read-only
    let shared_chat = std::sync::Arc::new(state.chat.clone());

    for index in 1..state.chat.agents.len() {
        if state.chat.agents[index].deleted || state.chat.agents[index].muted {
            continue;
        }

        if state.chat.agents[index].preset.is_none() {
            let preset_id = state.chat.agents[index].preset_selection.id;
            // Update the Agent's actual state
            state.chat.agents[index].preset = state.presets.get(preset_id).cloned();
            let _ = update_agent_preset_snapshot(&state.db_conn,
                state.chat.agents[index].id,
                state.chat.agents[index].preset.as_ref());
        }

        let Some(mut preset) = state.chat.agents[index].preset.clone() else {
            continue;
        };

        // for OpenRouter we must provide the API Key, but it must not be
        // saved to the database or even in memory
        if preset.chat_router == ChatRouter::Openrouter {
            preset.api_key = state.openrouter_api_key.clone();
        }

        // prepare thread data
        let tx = tx_base.clone();
        let que = ChatQue {
            preset,
            chat: shared_chat.clone(), // cheap arc clone
            agent_ind: index,
        };

        // ---------------------------------------------------------
        // PREPARE ASSISTANT MESSAGE (Placeholder)
        // ---------------------------------------------------------

        // 1. Determine the Effective Preset (Snapshot)
        // We do this now so the message history immediately records
        // what settings were used for this generation.
        let (effective_preset, preset_id) = {
            let agent = &state.chat.agents[index];
            if let Some(override_p) = &agent.preset {
                (Some(override_p.clone()), agent.preset_selection.id)
            } else {
                (
                    state.presets.get(agent.preset_selection.id).cloned(),
                    agent.preset_selection.id
                )
            }
        };

        // 2. Create the ChatMsg Object in Memory
        let mut assistant_msg = ChatMsg {
            id: 0, // Placeholder
            msg_role: MsgRole::Assistant,
            content: String::new(),
            reasoning: None,
            preset: effective_preset,
            preset_id,
            name: Some(state.chat.agents[index].name.clone()),
            ..Default::default()
        };

        // 3. Persist to Database (mk_msg)
        if let Ok(()) = mk_msg(&state.db_conn, &mut assistant_msg) {

            // A. Update the ID in the struct and Streaming State
            state.chat_streaming_state.msg_ids[index] = assistant_msg.id;

            // B. Update Agents (Omnis + Specific Agent)
            // Note: We update both Memory and Database for the message lists

            // Update Omnis (Index 0)
            if let Some(omnis) = state.chat.agents.get_mut(0) {
                omnis.msg_ids.push(assistant_msg.id);
                let _ = crate::db::mod_agent_msgs(
                        &state.db_conn, omnis.id, &omnis.msg_ids);
            }

            // Update The Specific Agent
            if let Some(agent) = state.chat.agents.get_mut(index) {
                agent.msg_ids.push(assistant_msg.id);
                let _ = crate::db::mod_agent_msgs(
                        &state.db_conn, agent.id, &agent.msg_ids);
            }

            // C. Insert into Pool (So UI renders it immediately)
            state.chat.msg_pool.insert(assistant_msg.id, assistant_msg);
        }

        // spawn
        state.chat_streaming_state.bitmask |= 1 << index as u128;
        let ctx_clone = ctx.clone();
        let thread_abort = abort_flag.clone();
        rt_handle.spawn(async move {
            if let Err(e) = run_chat_stream_router(que, tx.clone(),
                        &ctx_clone, thread_abort).await {
                let _ = tx.send(ChatStreamEvent::Error(index,
                        format!("Error: {}", e)));
            }
            // signal finished
            let _ = tx.send(ChatStreamEvent::Finished(index));
        });
    }
}

fn vertical_splitter(ui: &mut egui::Ui, width: &mut f32) {
    // 1. Allocate a thin strip of space for the handle
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(8.0, ui.available_height()),
        egui::Sense::drag()
    );

    // 2. Change cursor to indicate resizability
    let response = response.on_hover_cursor(egui::CursorIcon::ResizeHorizontal);

    // 3. Draw a visual line (optional, but looks nice)
    if ui.is_rect_visible(rect) {
        let color = if response.dragged() {
            ui.visuals().widgets.active.bg_fill
        } else if response.hovered() {
            ui.visuals().widgets.hovered.bg_fill
        } else {
            egui::Color32::from_gray(100) // Default line color
        };

        // Draw a vertical line in the center of the drag area
        let center_x = rect.center().x;
        ui.painter().vline(center_x, rect.y_range(),
                egui::Stroke::new(1.0, color));
    }

    // 4. Update the width based on drag delta
    if response.dragged() {
        *width += ui.input(|i| i.pointer.delta().x);
        // Clamp to prevent it from disappearing
        *width = width.max(50.0);
    }
}