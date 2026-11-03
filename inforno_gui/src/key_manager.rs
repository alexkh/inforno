use eframe::egui::{self, RichText, TextEdit, Vec2b};
use rust_i18n::t;
use secrecy::zeroize::Zeroize;

use inforno_core::api_keys::{self, KeyStore, OPENROUTER, RENEW_DAYS, SavedKey, format_date};
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

        for notice in &state.vault_notices {
            ui.colored_label(ui.visuals().warn_fg_color, notice);
            ui.add_space(6.0);
        }

        ui.label(RichText::new(t!("openrouter_api_key")).strong());

        if let Some(error) = &state.vault_error {
            ui.add_space(10.0);
            ui.colored_label(ui.visuals().error_fg_color, format!("Saved keys aren't available: {error}"));
            ui.label("You can still set a key for this session only; it stays in memory until Inforno closes.");
        }

        // Within RENEW_DAYS of expiry, saving starts a new 30 days.
        let renew_due = state.key_saved
            && api_keys::expires_within(state.key_storage.expires, RENEW_DAYS);

        if state.key_saved {
            ui.add_space(20.0);
            ui.label(t!("openrouter_key_is_set"));
            let expires = state.key_storage.expires;
            if expires != 0 {
                if renew_due {
                    ui.colored_label(ui.visuals().warn_fg_color, format!(
                        "Saved keys expire on {}. Enter your key again, ideally a freshly \
                         issued one, and save it to keep it for another {} days.",
                        format_date(expires),
                        api_keys::LIFETIME_DAYS
                    ));
                } else {
                    ui.label(format!(
                        "Saved keys expire on {}; you'll be asked for them again then.",
                        format_date(expires)
                    ));
                }
            }
            ui.weak(state.key_storage.description.as_str());
            ui.add_space(10.0);

            if ui.button(t!("delete_openrouter_key_btn")).clicked() {
                match store_openrouter_key(&state.perma.keys, None, false) {
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
                state.openrouter_api_key = ApiKey::new(
                    state.api_key_entered.clone().into(),
                    api_keys::default_hosts(OPENROUTER),
                );
                state.api_key_entered.zeroize();
            }

            ui.add_space(10.0);

            let save_tooltip = if state.key_storage.description.is_empty() {
                "Save in your vault".to_string()
            } else {
                format!("Save in your vault: {}", state.key_storage.description)
            };
            if ui.add_enabled(is_button_enabled, egui::Button::new("💾 Save in vault"))
                    .on_hover_text(save_tooltip)
                    .clicked() {
                let new_key = SavedKey::new(OPENROUTER, state.api_key_entered.clone().into());
                state.api_key_entered.zeroize();

                // If saving fails, the key still works for this session.
                match store_openrouter_key(&state.perma.keys, Some(&new_key), renew_due) {
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
                state.openrouter_api_key = ApiKey::new(new_key.key, new_key.hosts);
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
    /// When the saved keys expire (Unix seconds); 0 for none: nothing saved,
    /// or they never expire.
    pub expires: u64,
}

/// The saved OpenRouter key, if any, and what holds it.
pub fn load_openrouter_key(keys: &KeyStore) -> Result<(Option<SavedKey>, KeyStorage), String> {
    keys.with_vault(|vault| {
        let (mut saved, expires) = api_keys::read_keys(vault)?;
        let storage = KeyStorage { description: vault.describe(), expires };
        Ok((saved.remove(OPENROUTER), storage))
    })
    .map_err(|e| e.to_string())
}

/// Saves the OpenRouter key, or removes it (`None`). With `renew`, the saved
/// keys are replaced by this one, which starts a new 30 days.
pub fn store_openrouter_key(
    keys: &KeyStore,
    key: Option<&SavedKey>,
    renew: bool,
) -> Result<KeyStorage, String> {
    keys.with_vault(|vault| {
        let expires = match (key, renew) {
            (Some(key), true) => api_keys::renew_key(vault, OPENROUTER, key)?,
            (key, _) => api_keys::set_key(vault, OPENROUTER, key)?,
        };
        Ok(KeyStorage { description: vault.describe(), expires })
    })
    .map_err(|e| e.to_string())
}
