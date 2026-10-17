use eframe::egui::{self, RichText, TextEdit, Vec2b};
use rust_i18n::t;
use secrecy::SecretString;
use secrecy::zeroize::Zeroize;

use inforno_core::api_keys::{self, OPENROUTER, format_date};
use inforno_core::common::ApiKey;
use crate::state::State;

pub fn ui_key_manager(ctx: &egui::Context, state: &mut State) {
    egui::Window::new(t!("api_key_manager"))
        .collapsible(false)
        .scroll(Vec2b { x: true, y: true })
        .open(&mut state.show_key_manager)
        .default_width(500.)
        .show(ctx, |ui| {
        if state.is_modal_open {
            ui.disable();
        }

        ui.label(RichText::new(t!("openrouter_api_key")).strong());

        if let Some(error) = &state.vault_error {
            ui.add_space(10.0);
            ui.colored_label(ui.visuals().error_fg_color, format!("Saved keys aren't available: {error}"));
            ui.label("You can still set a key for this session only; it stays in memory until Inforno closes.");
        }

        if state.key_saved {
            ui.add_space(20.0);
            ui.label(t!("openrouter_key_is_set"));
            if let Some(expires) = state.key_storage.expires {
                ui.label(format!(
                    "Saved keys expire on {}; you'll be asked for them again then.",
                    format_date(expires)
                ));
            }
            ui.weak(state.key_storage.description.as_str());
            ui.add_space(10.0);

            if ui.button(t!("delete_openrouter_key_btn")).clicked() {
                match store_openrouter_key(None) {
                    Ok(storage) => {
                        state.key_saved = false;
                        state.key_storage = storage;
                        state.vault_error = None;
                    },
                    Err(error) => state.vault_error = Some(error),
                }
            }
            ui.add_space(20.0);
        } else if state.openrouter_api_key.is_set {
            ui.add_space(20.0);
            ui.label(RichText::new(t!("key_active")).strong());
            ui.add_space(20.0);
        }

        ui.label(RichText::new(t!("openrouter_key_instruction")));

        ui.vertical_centered( |ui| {
            let mut is_button_enabled = false;
            let response = ui.add(
                TextEdit::singleline(&mut state.api_key_entered)
                    .desired_width(15.0)
                    .horizontal_align(egui::Align::Center)
            );
            response.request_focus();

            ui.add_space(10.0);

            if state.api_key_entered.len() > 3 {
                is_button_enabled = true;
            }

            if ui.add_enabled(is_button_enabled,
                    egui::Button::new(t!("set_key_temporarily_btn"))).clicked() {
                let new_key = ApiKey {
                    key: state.api_key_entered.clone().into(),
                    is_set: true,
                };
                state.api_key_entered.zeroize();
                state.openrouter_api_key = new_key;
            }

            ui.add_space(10.0);

            let save_tooltip = if state.key_storage.description.is_empty() {
                "Save in vaultorno".to_string()
            } else {
                format!("Save in {}", state.key_storage.description)
            };
            if ui.add_enabled(is_button_enabled, egui::Button::new("💾 Save in vault"))
                    .on_hover_text(save_tooltip)
                    .clicked() {
                let new_key = ApiKey {
                    key: state.api_key_entered.clone().into(),
                    is_set: true,
                };
                state.api_key_entered.zeroize();

                // If saving fails, the key still works for this session.
                match store_openrouter_key(Some(&new_key.key)) {
                    Ok(storage) => {
                        state.key_saved = true;
                        state.key_storage = storage;
                        state.vault_error = None;
                    },
                    Err(error) => {
                        state.key_saved = false;
                        state.vault_error = Some(error);
                    },
                }
                state.openrouter_api_key = new_key;
            }
        });

    });
}

// --- Saved keys (vaultorno, through inforno_core::api_keys) ---

/// What the key manager shows about saved keys.
#[derive(Default, Clone)]
pub struct KeyStorage {
    /// Which backend holds them and how, from `Vault::describe()`.
    pub description: String,
    /// When the saved keys expire (Unix seconds); none if nothing is saved.
    pub expires: Option<u64>,
}

/// The saved OpenRouter key, if any, and what holds it.
pub fn load_openrouter_key() -> Result<(Option<SecretString>, KeyStorage), String> {
    let vault = api_keys::open_vault().map_err(|e| e.to_string())?;
    let (mut keys, expires) = api_keys::read_keys(&vault).map_err(|e| e.to_string())?;
    let storage = KeyStorage { description: vault.describe(), expires };
    Ok((keys.remove(OPENROUTER), storage))
}

/// Saves the OpenRouter key in vaultorno, or removes it (`None`).
pub fn store_openrouter_key(key: Option<&SecretString>) -> Result<KeyStorage, String> {
    let vault = api_keys::open_vault().map_err(|e| e.to_string())?;
    let expires = api_keys::set_key(&vault, OPENROUTER, key).map_err(|e| e.to_string())?;
    Ok(KeyStorage { description: vault.describe(), expires })
}