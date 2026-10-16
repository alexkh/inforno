use eframe::egui::{self, RichText, TextEdit, Vec2b};
use rust_i18n::t;
use secrecy::SecretString;
use secrecy::zeroize::Zeroize;

use inforno_core::common::{ApiKey, KEYRING_INFO};
use inforno_core::vault::{self, Vault};
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
            ui.colored_label(ui.visuals().error_fg_color, format!("The encrypted key file couldn't be read: {error}"));
            ui.label("Saving a key replaces it; the old file is kept as api-keys.unreadable.");
        }

        if state.key_saved {
            ui.add_space(20.0);
            ui.label(t!("openrouter_key_is_set"));
            ui.add_space(10.0);

            if ui.button(t!("delete_openrouter_key_btn")).clicked() {
                // remove the key from the encrypted key file:
                match store_openrouter_key(None) {
                    Ok(()) => {
                        state.key_saved = false;
                        println!("OpenRouter API key removed from the encrypted key file.");
                    },
                    Err(error) => {
                        println!("Could not remove the API key: {}", error);
                    },
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

            let can_save = is_button_enabled && vault::platform_sealer().is_some();
            if ui.add_enabled(can_save, egui::Button::new("💾 Save encrypted"))
                    .on_hover_text(storage_description())
                    .clicked() {
                let new_key = ApiKey {
                    key: state.api_key_entered.clone().into(),
                    is_set: true,
                };
                state.api_key_entered.zeroize();

                // store the key in the encrypted key file:
                match store_openrouter_key(Some(&new_key.key)) {
                    Ok(()) => {
                        state.key_saved = true;
                        state.vault_error = None;
                        println!("OpenRouter API key saved to the encrypted key file.");
                    },
                    Err(error) => {
                        println!("Could not save the API key: {}", error);
                    },
                }
                state.openrouter_api_key = new_key;
            }
        });

    });
}

// --- Encrypted key storage (inforno_core::vault) ---

/// Vault entry of the OpenRouter key.
const OPENROUTER: &str = "openrouter";

fn vault_location() -> Result<(std::path::PathBuf, Box<dyn vault::Sealer>), String> {
    let sealer = vault::platform_sealer()
        .ok_or_else(|| "encrypted key storage isn't available on this platform".to_string())?;
    let path = vault::default_path().ok_or_else(|| "could not find the user data directory".to_string())?;
    Ok((path, sealer))
}

/// What protects saved keys, for tooltips (and later the security report).
pub fn storage_description() -> &'static str {
    vault::platform_sealer().map_or("not available on this platform", |s| s.description())
}

/// The OpenRouter key from the encrypted key file, if one is stored there.
pub fn load_openrouter_key() -> Result<Option<SecretString>, String> {
    let (path, sealer) = vault_location()?;
    let vault = Vault::load(&path, sealer.as_ref()).map_err(|e| e.to_string())?;
    Ok(vault.get(OPENROUTER).cloned())
}

/// Stores the OpenRouter key in the encrypted key file, or removes it
/// (`None`). A key file that can't be decrypted any more (e.g. after a TPM
/// reset) is moved aside as `api-keys.unreadable` and replaced.
pub fn store_openrouter_key(key: Option<&SecretString>) -> Result<(), String> {
    let (path, sealer) = vault_location()?;
    let mut vault = match Vault::load(&path, sealer.as_ref()) {
        Ok(vault) => vault,
        Err(e) => {
            let aside = path.with_extension("unreadable");
            std::fs::rename(&path, &aside).map_err(|re| format!("{e} (and moving it aside failed: {re})"))?;
            eprintln!("Unreadable key file moved to {} ({e})", aside.display());
            Vault::default()
        }
    };
    match key {
        Some(key) => vault.set(OPENROUTER, key.clone()),
        None => {
            vault.remove(OPENROUTER);
        }
    }
    vault.save(&path, sealer.as_ref()).map_err(|e| e.to_string())
}

/// Moves the OpenRouter key from the system keyring, where older versions
/// kept it, into the encrypted key file. Runs once: a marker file next to
/// the key file records it, so the keyring (which may ask for its password)
/// isn't touched again. Returns the key found, if any, and an error if it
/// couldn't be stored (then the next start tries again).
pub fn migrate_from_keyring() -> (Option<SecretString>, Option<String>) {
    let Ok((path, _)) = vault_location() else { return (None, None) };
    let marker = path.with_extension("keyring-migrated");
    if marker.exists() {
        return (None, None);
    }

    let entry = keyring::Entry::new(KEYRING_INFO[0], KEYRING_INFO[1]).ok();
    let key = entry.as_ref().and_then(|e| e.get_password().ok()).map(SecretString::from);
    if let Some(key) = &key {
        if let Err(e) = store_openrouter_key(Some(key)) {
            return (Some(key.clone()), Some(e));
        }
        if let Some(entry) = &entry {
            let _ = entry.delete_credential();
        }
        println!("OpenRouter API key moved from the system keyring into the encrypted key file.");
    }
    if let Some(dir) = marker.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(&marker, b"");
    (key, None)
}