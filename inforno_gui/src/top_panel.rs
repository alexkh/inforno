use egui::{Color32, RichText};
use rust_i18n::t;

use inforno_core::{common::{FileOp, FileOpMsg}, db::reset_sandbox_db};
use crate::state::{State, err_color};
use crate::mybtn;

pub fn ui_top_panel(ui: &mut egui::Ui, state: &mut State) {
    let ctx = ui.ctx().clone();
    egui::Panel::top("top_panel").show(ui, |ui| {
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
                    ui.close();
                    state.pending_file_dialog_op = Some(FileOp::SaveAs);
                    state.file_dialog = egui_file_dialog::FileDialog::new()
                        .default_file_name("")
                        .add_file_filter(
                            "Inforno Sandbox",
                            egui_file_dialog::Filter::new(|p: &std::path::Path| {
                                p.extension().is_some_and(|ext| ext == "rno")
                            })
                        );
                    state.file_dialog.save_file();
                }

                // Save Copy Button
                if mybtn!(ui, "menu_sandbox_save_copy_btn") {
                    ui.close(); // Fixed deprecation
                    state.pending_file_dialog_op = Some(FileOp::SaveCopy);
                    state.file_dialog = egui_file_dialog::FileDialog::new()
                        .default_file_name("")
                        .add_file_filter(
                            "Inforno Sandbox",
                            egui_file_dialog::Filter::new(|p: &std::path::Path| {
                                p.extension().is_some_and(|ext| ext == "rno")
                            })
                        );
                    state.file_dialog.save_file();
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
                        attachments: None,
                        left_content: None,
                        right_content: None,
                    });
                }
            }).response.on_hover_text(
                egui::RichText::new(t!("menu_sandbox_tooltip"))
                .strong()
                .heading());

            // Open Button
            if ui.button(t!("menu_sandbox_open_btn"))
                .on_hover_text(egui::RichText::new(
                    t!("menu_sandbox_open_btn_tooltip"))
                    .strong()
                    .heading()
                )
                .clicked() {

                state.pending_file_dialog_op = Some(FileOp::Open);
                state.file_dialog = egui_file_dialog::FileDialog::new()
                    .add_file_filter(
                        "Inforno Sandbox",
                        egui_file_dialog::Filter::new(|p: &std::path::Path| {
                            p.extension().is_some_and(|ext| ext == "rno")
                        })
                    );
                state.file_dialog.pick_file();
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

            ui.separator(); // Visual spacer

            let edit_resp = crate::split_button::SplitButton::new("📝 Edit")
                .id_salt("top_panel_edit_btn")
                .main_tooltip("Open file in editor")
                .arrow_tooltip(t!("right_button_tooltip"))
                .transparent(true)
                .show(ui);

            if edit_resp.main_clicked || edit_resp.arrow_clicked {
                ui.close();
                state.pending_file_dialog_op = Some(if edit_resp.arrow_clicked {
                    FileOp::OpenEditorRight
                } else {
                    FileOp::OpenEditor
                });

                if let Some(root) = &state.project_root {
                    state.file_dialog = egui_file_dialog::FileDialog::new()
                        .initial_directory(root.clone());
                } else {
                    state.file_dialog = egui_file_dialog::FileDialog::new();
                }

                state.file_dialog.pick_file();
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if let Some(realm) = &state.active_realm {
                        // Forcing a horizontal layout guarantees Left-to-Right ordering,
                        // even if the parent toolbar is drawing Right-to-Left!
                        ui.horizontal(|ui| {
                            let orange = ui.visuals().warn_fg_color;
                            let slash_color = ui.visuals().weak_text_color();

                            // Find the currently active mount
                            let active_mount = realm.mounts.iter().find(|m| {
                                state.active_workspace_name.as_ref() == Some(&m.virtual_path)
                            });

                            // --- PART 3: 📁 The Sub-Project (Only if kind == workspace) ---
                            if let Some(mount) = active_mount {
                                if mount.kind.to_lowercase() == "workspace" {

                                    // Safely cache the TOML parsed members so the GUI doesn't stutter
                                    let cache_id = egui::Id::new("ws_members").with(&mount.host_path);
                                    let members: Vec<String> = ctx.data_mut(|d| {
                                        d.get_temp_mut_or_insert_with(cache_id, || {
                                            let mut parsed = vec![".".to_string()];
                                            let cargo_path = mount.host_path.join("Cargo.toml");

                                            if let Ok(content) = std::fs::read_to_string(&cargo_path) {
                                                if let Ok(toml_val) = toml::from_str::<toml::Value>(&content) {
                                                    if let Some(arr) = toml_val.get("workspace").and_then(|w| w.get("members")).and_then(|m| m.as_array()) {
                                                        for item in arr {
                                                            if let Some(s) = item.as_str() {
                                                                if s.ends_with("/*") {
                                                                    // Expand globs like "crates/*"
                                                                    let base = s.trim_end_matches("/*");
                                                                    if let Ok(entries) = std::fs::read_dir(mount.host_path.join(base)) {
                                                                        for e in entries.flatten() {
                                                                            if e.path().is_dir() && e.path().join("Cargo.toml").exists() {
                                                                                if let Some(name) = e.file_name().to_str() {
                                                                                    parsed.push(format!("{}/{}", base, name));
                                                                                }
                                                                            }
                                                                        }
                                                                    }
                                                                } else {
                                                                    parsed.push(s.to_string());
                                                                }
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                            parsed
                                        }).clone()
                                    });

                                    // Retrieve selected sub-project from cache
                                    let active_sub = ctx.data_mut(|d| {
                                        d.get_temp::<String>(egui::Id::new("sub_project")).unwrap_or_else(|| ".".to_string())
                                    });

                                    let mut sub_job = egui::text::LayoutJob::default();
                                    sub_job.append(&format!("📁 {}", active_sub), 0.0, egui::text::TextFormat {
                                        color: orange,
                                        ..Default::default()
                                    });

                                    egui::ComboBox::from_id_salt("sub_project_selector")
                                        .width(0.0)
                                        .selected_text(sub_job)
                                        .show_ui(ui, |ui| {
                                            for member in members {
                                                let is_selected = member == active_sub;
                                                if ui.selectable_label(is_selected, format!("📁 {}", member)).clicked() {
                                                    // Save selection state
                                                    ctx.data_mut(|d| d.insert_temp(egui::Id::new("sub_project"), member.clone()));

                                                    // Update actual project root so IDE/chat targets the new path!
                                                    if member == "." {
                                                        state.project_root = Some(mount.host_path.clone());
                                                    } else {
                                                        state.project_root = Some(mount.host_path.join(&member));
                                                    }
                                                }
                                            }
                                        });
                                    ui.label(egui::RichText::new("/").color(orange).strong());
                                }
                            }

                            // --- PART 2: 🗄 The Mount Point (Workspace) ---
                            let mut mount_job = egui::text::LayoutJob::default();
                            if let Some(mount) = active_mount {
                                let icon = match mount.kind.to_lowercase().as_str() {
                                    "workspace" => "🗄",
                                    "docs" => "📚",
                                    "static" => "🌐",
                                    _ => "📁",
                                };
                                mount_job.append(&format!("{} {}", icon, mount.virtual_path), 0.0, egui::text::TextFormat {
                                    color: orange,
                                    ..Default::default()
                                });
                            } else {
                                mount_job.append("Select Mount...", 0.0, egui::text::TextFormat {
                                    color: orange,
                                    ..Default::default()
                                });
                            }

                            egui::ComboBox::from_id_salt("mount_selector")
                                .width(0.0)
                                .selected_text(mount_job)
                                .show_ui(ui, |ui| {
                                    for mount in &realm.mounts {
                                        let is_selected = active_mount.map_or(false, |m| m.virtual_path == mount.virtual_path);

                                        let icon = match mount.kind.to_lowercase().as_str() {
                                            "workspace" => "🗄",
                                            "docs" => "📚",
                                            "static" => "🌐",
                                            _ => "📁",
                                        };

                                        // Rich text row with descriptions for the dropdown
                                        let mut item_job = egui::text::LayoutJob::default();
                                        item_job.append(&format!("{} {}  ", icon, mount.virtual_path), 0.0, egui::text::TextFormat {
                                            color: ui.visuals().text_color(),
                                            ..Default::default()
                                        });

                                        if let Some(desc) = &mount.description {
                                            item_job.append(desc, 0.0, egui::text::TextFormat {
                                                color: ui.visuals().weak_text_color(),
                                                font_id: egui::FontId::proportional(12.0),
                                                ..Default::default()
                                            });
                                        }

                                        if ui.selectable_label(is_selected, item_job).clicked() {
                                            state.active_workspace_name = Some(mount.virtual_path.clone());
                                            state.project_root = Some(mount.host_path.clone());

                                            // Reset the sub-project cache state when mount changes
                                            ctx.data_mut(|d| d.insert_temp(egui::Id::new("sub_project"), String::from(".")));
                                        }
                                    }
                                });

                            // --- PART 1: 🏰 The Realm ---
                            ui.label(egui::RichText::new("/").color(orange).strong());

                            if ui.button(
                                egui::RichText::new(format!("🏰 {} ⚙", realm.name))
                                        .color(orange)
                                        .strong()
                                )
                                .on_hover_text("Open Realm Configuration")
                                .clicked() {
                                    state.show_realm_config = !state.show_realm_config;

                                    // Initialize the YAML buffer if opening
                                    if state.show_realm_config {
                                        if let Some(active_realm) = &state.active_realm {
                                            // Assuming ActiveRealm can be serialized back to RealmConfig
                                            if let Ok(yaml) = serde_yaml::to_string(&active_realm.raw_config) {
                                                state.realm_config_state.yaml_buffer = yaml;
                                            }
                                        }
                                    }
                                }
                        });
                    }
            });

        });
    });
}
