use egui::{Color32, RichText};
use rust_i18n::t;

use crate::{common::{FileOp, FileOpMsg, err_color}, db::reset_sandbox_db, gui::State, mybtn};

pub fn ui_top_panel(ctx: &egui::Context, state: &mut State) {
    egui::TopBottomPanel::top("top_panel").show(ctx, |ui| {
        if state.is_modal_open {
            ui.disable();
        }
        egui::MenuBar::new().ui(ui, |ui| {
            let app_language = state.perma.app_language.lock().unwrap().clone();
            let (lang_label, target_lang) = if app_language == "ru" {
                ("ru", "en")
            } else {
                ("en", "ru")
            };
            if ui.button(lang_label)
                .on_hover_text(egui::RichText::new(
                    "Switch Language / Переключить язык")
                    .strong()
                    .heading()
                )
                .clicked()
            {
                // 1. Update the state variable (for saving to disk later)
                *state.perma.app_language.lock().unwrap() =
                        target_lang.to_string();

                // 2. Update the live locale immediately
                rust_i18n::set_locale(target_lang);
            }
            ui.separator(); // Visual spacer

            // API Keys Button
            let api_btn = egui::Button::new(t!("menu_api_keys_btn"))
                    .selected(state.show_key_manager);
            if ui.add(api_btn)
                .on_hover_text(
                    egui::RichText::new(t!("menu_api_keys_btn_tooltip"))
                    .strong()
                    .heading()
                )
                .clicked() {
                state.show_key_manager = !state.show_key_manager;
            }
            if state.openrouter_api_key.is_set {
                ui.label(RichText::new("🔑")
                .color(Color32::from_rgb(0, 220, 0)).strong());
            } else {
                ui.colored_label(err_color(), "🔑");
            }

            ui.colored_label(ui.visuals().code_bg_color,"|");

            // Presets Button
            let api_btn = egui::Button::new(t!("menu_presets_btn"))
                    .selected(state.show_preset_editor);
            if ui.add(api_btn)
                .on_hover_text(
                    egui::RichText::new(t!("menu_presets_btn_tooltip"))
                    .strong()
                    .heading()
                )
                .clicked() {
                state.show_preset_editor = !state.show_preset_editor;
            }

            ui.colored_label(ui.visuals().code_bg_color,"|");

            if mybtn!(ui, "menu_dark_theme_btn") {
                ctx.set_theme(egui::Theme::Dark);
            }

            if mybtn!(ui, "menu_light_theme_btn") {
                ctx.set_theme(egui::Theme::Light);
            }

            ui.colored_label(ui.visuals().code_bg_color,"|");

            // Sandbox Menu
            ui.menu_button(t!("menu_sandbox"), |ui| {
                // Save As Button
                if mybtn!(ui, "menu_sandbox_save_as_btn") {
                    if state.open_sandbox_showing {
                        return;
                    }
                    state.open_sandbox_showing = true;
                    state.is_modal_open = true;
                    let tx_clone = state.op_tx.clone();
                    let ctx_clone = ctx.clone();
                    tokio::spawn(async move {
                        let task = rfd::AsyncFileDialog::new()
                            .add_filter("Inforno Sandbox: *.rno", &["rno"])
                            .set_file_name("Unnamed.rno")
                            .save_file()
                            .await;

                        // if user picked a file and did not cancel:
                        if let Some(handle) = task {
                            let _ = tx_clone.send(FileOpMsg {
                                op: FileOp::SaveAs,
                                cancelled: false,
                                path: Some(handle.path().to_path_buf()),
                            });
                        } else {
                            let _ = tx_clone.send(FileOpMsg {
                                op: FileOp::SaveAs,
                                cancelled: true,
                                path: None,
                            });
                        }
                        ctx_clone.request_repaint();
                    });
                }

                // Save Copy Button
                if mybtn!(ui, "menu_sandbox_save_copy_btn") {
                    if state.open_sandbox_showing {
                        return;
                    }
                    state.open_sandbox_showing = true;
                    state.is_modal_open = true;
                    let tx_clone = state.op_tx.clone();
                    let ctx_clone = ctx.clone();
                    tokio::spawn(async move {
                        let task = rfd::AsyncFileDialog::new()
                            .add_filter("Inforno Sandbox: *.rno", &["rno"])
                            .set_file_name("Unnamed.rno")
                            .save_file()
                            .await;

                        // if user picked a file and did not cancel:
                        if let Some(handle) = task {
                            let _ = tx_clone.send(FileOpMsg {
                                op: FileOp::SaveCopy,
                                cancelled: false,
                                path: Some(handle.path().to_path_buf()),
                            });
                        } else {
                            let _ = tx_clone.send(FileOpMsg {
                                op: FileOp::SaveCopy,
                                cancelled: true,
                                path: None,
                            });
                        }
                        ctx_clone.request_repaint();
                    });
                }

                ui.add_space(10.0);
                ui.separator();
                ui.add_space(10.0);

                // Clear Button
                if ui.button(
                    egui::RichText::new(t!("menu_sandbox_clear"))
                    .color(ui.visuals().error_fg_color)
                ).clicked() {
                    let _ = reset_sandbox_db(&state.db_conn);
                    let tx_clone = state.op_tx.clone();
                    let _ = tx_clone.send(FileOpMsg {
                        op: FileOp::Clear,
                        cancelled: false,
                        path: None,
                    });
                }
            }).response.on_hover_text(
                egui::RichText::new(t!("menu_sandbox_tooltip"))
                .strong()
                .heading());

            // Open Button
            let sandbox_open_btn = egui::Button::new(t!("menu_sandbox_open_btn"))
                .selected(state.open_sandbox_showing);
            if ui.add(sandbox_open_btn)
                .on_hover_text(egui::RichText::new(
                    t!("menu_sandbox_open_btn_tooltip"))
                    .strong()
                    .heading()
                )
                .clicked() {
                if state.open_sandbox_showing {
                    return;
                }
                state.open_sandbox_showing = true;
                state.is_modal_open = true;
                let tx_clone = state.op_tx.clone();
                let ctx_clone = ctx.clone();
                tokio::spawn(async move {
                    let task = rfd::AsyncFileDialog::new()
                        .add_filter("Inforno Sandbox: *.rno", &["rno"])
                        .pick_file()
                        .await;

                    // if user picked a file and did not cancel:
                    if let Some(handle) = task {
                        let _ = tx_clone.send(FileOpMsg {
                            op: FileOp::Open,
                            cancelled: false,
                            path: Some(handle.path().to_path_buf()),
                        });
                    } else {
                        let _ = tx_clone.send(FileOpMsg {
                            op: FileOp::Open,
                            cancelled: true,
                            path: None,
                        });
                    }
                    ctx_clone.request_repaint();
                });
            }

            if ui.add_enabled(!state.is_in_home_sandbox,
                egui::Button::new(t!("menu_sandbox_home_btn")))
                .on_hover_text(egui::RichText::new(
                    t!("menu_sandbox_home_btn_tooltip"))
                    .strong()
                    .heading())
                .on_disabled_hover_text(egui::RichText::new(
                    t!("menu_sandbox_home_btn_tooltip"))
                    .heading())
                .clicked() {
                    state.reload(None);
                };
        });
    });
}