use egui::{Vec2b, Window};
use rust_i18n::t;
use crate::common::{ChatRouter, ModelOptions, load_presets};
use crate::db::{save_preset, update_agent_preset_snapshot};
use crate::gui::State;
// Import the reusable components
use crate::gui::preset_editor::{
    PresetEditorState, render_common_options,
};

/// The state specific to the Agent Config Window
#[derive(Default)]
pub struct AgentConfigState {
    pub is_open: bool,
    pub target_agent_id: Option<i64>, // the db id of the agent we are modifying
    pub target_agent_ind: Option<usize>, // index inside Chat
    pub editor_state: PresetEditorState, // reusing the editor state struct
}

pub fn ui_agent_config(ctx: &egui::Context, state: &mut State) {
    // 1. Check if window should be open
    if !state.agent_config_state.is_open {
        return;
    }

    let mut is_open = state.agent_config_state.is_open;
    let mut should_close = false;

    Window::new(t!("agent_config_window_title"))
    .id(egui::Id::new("agent_conf_win")) // Unique ID
    .collapsible(false)
    .resizable(true)
    .default_width(450.0)
    .scroll(Vec2b { x: false, y: true })
    .open(&mut is_open)
    .show(ctx, |ui| {
        // 2. Render the specific editor based on Router type
        /*
        let router = &state.agent_config_state
                .editor_state.edited_preset.chat_router;

        match router {
            ChatRouter::Ollama => render_ollama_editor(ui, ctx, state),
            ChatRouter::Openrouter => render_openrouter_editor(ui, state),
        }*/
        ui.colored_label(
            ui.visuals().hyperlink_color, t!("config_editor_invitation"),
        );

        if let Some(original_preset) = state.presets.get(
                    state.agent_config_state.editor_state.edited_preset.id) {
            render_common_options(ui, &mut state.agent_config_state.editor_state,
                    &original_preset.options);
        } else {
            // if preset does not exist yet, create a temporary ModelOptions
            let default_model_options = ModelOptions::default();
            render_common_options(ui, &mut state.agent_config_state.editor_state,
                    &default_model_options);
        }

        // 3. Save / Action Buttons
        ui.add_space(10.0);
        ui.separator();
        ui.horizontal(|ui| {
            if ui.button(t!("agent_config_save_changes_btn")).clicked() {
                save_agent_preset(state);
                should_close = true;
            }

            if ui.button(t!("cancel_btn")).clicked() {
                should_close = true;
            }
        });
    });

    if should_close {
        is_open = false;
    }

    // Handle close via 'X' button
    state.agent_config_state.is_open = is_open;
}

fn save_agent_preset(state: &mut State) {
    let edited = &state.agent_config_state.editor_state.edited_preset;
    if let Some(agent_ind) =
            state.agent_config_state.target_agent_ind {
        let agent = &mut state.chat.agents[agent_ind];
        agent.preset = Some(edited.clone()); // save modified preset to agent
        // save modified agent to Sandbox
        let result = update_agent_preset_snapshot(
                &state.db_conn, agent.id, agent.preset.as_ref());
        if result.is_err() {
            state.error_msg =
                Some(t!("error_saving_agent_config_to_sandbox").to_string());
            state.is_modal_open = true;
        }
    }
}