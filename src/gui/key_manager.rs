use eframe::egui::{self, RichText, TextEdit, Vec2b};
use keyring::Entry;
use rust_i18n::t;
use secrecy::ExposeSecret;
use secrecy::zeroize::Zeroize;

use crate::common::{ApiKey, KEYRING_INFO};
use super::State;

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

        if state.keyring_used {
            ui.add_space(20.0);
            ui.label(t!("openrouter_key_is_set"));
            ui.add_space(10.0);

            if ui.button(t!("delete_openrouter_key_btn")).clicked() {
                // delete the key from the system keyring:
                if let Ok(entry) = Entry::new(KEYRING_INFO[0], KEYRING_INFO[1]) {
                    match entry.delete_credential() {
                        Ok(_result) => {
                            state.keyring_used = false;
                            println!("{}!", t!("key_deleted_from_keyring"));
                        },
                        Err(error) => {
                            println!("{}: {}",
                                t!("error_deleting_key_from_keyring"), error);
                        },
                    }
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

            if ui.add_enabled(is_button_enabled,
                    egui::Button::new(t!("save_to_keyring_btn"))).clicked() {
                let new_key = ApiKey {
                    key: state.api_key_entered.clone().into(),
                    is_set: true,
                };
                state.api_key_entered.zeroize();

                // store the key into the system keyring:
                if let Ok(entry) = Entry::new(KEYRING_INFO[0], KEYRING_INFO[1]) {
                    match entry.set_password(new_key.key.expose_secret()) {
                        Ok(_result) => {
                            println!("{}", t!("key_saved_to_keyring"));
                        },
                        Err(error) => {
                            println!("{}: {}",
                                t!("error_saving_key_to_keyring"), error);
                        },
                    }
                }
                state.openrouter_api_key = new_key;
            }
        });

    });
}
