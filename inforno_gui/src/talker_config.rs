use egui::{Vec2b, Window};
use rust_i18n::t;
use inforno_core::common::ModelOptions;
use inforno_core::db::update_talker_model_preset_snapshot;
use crate::state::State;
// Import the reusable components
use crate::model_preset_editor::{
    ModelPresetEditorState, render_common_options,
};

/// The state specific to the Talker Config Window
#[derive(Default)]
pub struct TalkerConfigState {
    pub is_open: bool,
    pub target_talker_id: Option<i64>, // the db id of the talker we are modifying
    pub target_talker_ind: Option<usize>, // index inside Chat
    pub editor_state: ModelPresetEditorState, // reusing the editor state struct
}

pub fn ui_talker_config(ctx: &egui::Context, state: &mut State) {
    // 1. Check if window should be open
    if !state.talker_config_state.is_open {
        return;
    }

    let mut is_open = state.talker_config_state.is_open;
    let mut should_close = false;

    Window::new(t!("talker_config_window_title"))
    .id(egui::Id::new("talker_conf_win")) // Unique ID
    .collapsible(false)
    .resizable(true)
    .default_width(450.0)
    .scroll(Vec2b { x: false, y: true })
    .open(&mut is_open)
    .show(ctx, |ui| {
        // 2. Render the specific editor based on Router type
        /*
        let router = &state.talker_config_state
                .editor_state.edited_preset.chat_router;

        match router {
            ChatRouter::Ollama => render_ollama_editor(ui, ctx, state),
            ChatRouter::Openrouter => render_openrouter_editor(ui, state),
        }*/
        ui.colored_label(
            ui.visuals().hyperlink_color, t!("config_editor_invitation"),
        );

        if let Some(original_preset) = state.presets.get(
                    state.talker_config_state.editor_state.edited_preset.id) {
            render_common_options(ui, &mut state.talker_config_state.editor_state,
                    &original_preset.options);
        } else {
            // if preset does not exist yet, create a temporary ModelOptions
            let default_model_options = ModelOptions::default();
            render_common_options(ui, &mut state.talker_config_state.editor_state,
                    &default_model_options);
        }

        // 3. Save / Action Buttons
        ui.add_space(10.0);
        ui.separator();
        ui.horizontal(|ui| {
            if ui.button(t!("talker_config_save_changes_btn")).clicked() {
                save_talker_preset(state);
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
    state.talker_config_state.is_open = is_open;
}

fn save_talker_preset(state: &mut State) {
    let edited = &state.talker_config_state.editor_state.edited_preset;
    let active_chat_id = state.active_chat_id.unwrap_or(0);

    if let Some(chat) = state.open_chats.get_mut(&active_chat_id) {
        if let Some(talker_ind) = state.talker_config_state.target_talker_ind {
            let talker = &mut chat.talkers[talker_ind];
            talker.preset = Some(edited.clone());
            let result = update_talker_model_preset_snapshot(&state.db_conn, talker.id, talker.preset.as_ref());

            if result.is_err() {
                state.error_msg =
                    Some(t!("error_saving_talker_config_to_folio").to_string());
                state.is_modal_open = true;
            }
        }
    }
}
