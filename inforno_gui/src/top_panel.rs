use egui::{Color32, RichText};
use rust_i18n::t;

use inforno_core::{common::{FileOp, FileOpMsg}, db::reset_sandbox_db};
use crate::state::{State, err_color};
use crate::mybtn;

pub fn ui_top_panel(ui: &mut egui::Ui, state: &mut State) {
    let ctx = ui.ctx().clone();
    
    // Intercept any startup errors and show them as a modal popup
    if let Some(err) = ctx.data_mut(|d| d.remove_temp::<String>(egui::Id::new("startup_error"))) {
        state.error_msg = Some(err);
        state.is_modal_open = true;
        
        if let Some(broken_yaml) = ctx.data_mut(|d| d.remove_temp::<String>(egui::Id::new("broken_realm_yaml"))) {
            state.realm_config_state.yaml_buffer = broken_yaml.clone();
            state.realm_config_state.original_yaml = broken_yaml;
            state.realm_config_state.is_fixing_broken_realm = true;
            
            if let Some(broken_name) = ctx.data_mut(|d| d.remove_temp::<String>(egui::Id::new("broken_realm_name"))) {
                state.realm_config_state.realm_name = Some(broken_name);
            }
        }
    }

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

                            // --- 🔖 The Place (Workspace) ---
                            if !realm.raw_config.places.is_empty() {
                                
                                let cache_id = egui::Id::new("active_place").with(&realm.name);
                                
                                // Retrieve selected place from cache, default to the FIRST place in the IndexMap
                                let first_place_name = realm.raw_config.places.keys().next().unwrap().clone();
                                let active_place_name = ctx.data_mut(|d| {
                                    d.get_temp::<String>(cache_id).unwrap_or_else(|| first_place_name.clone())
                                });

                                let mut place_job = egui::text::LayoutJob::default();
                                place_job.append(&format!("🔖 {}", active_place_name), 0.0, egui::text::TextFormat {
                                    color: orange,
                                    ..Default::default()
                                });

                                egui::ComboBox::from_id_salt("place_selector")
                                    .width(0.0)
                                    .selected_text(place_job)
                                    .show_ui(ui, |ui| {
                                        for (place_name, place_vpath) in &realm.raw_config.places {
                                            let is_selected = *place_name == active_place_name;
                                            if ui.selectable_label(is_selected, format!("🔖 {}", place_name)).clicked() {
                                                // Save selection state
                                                ctx.data_mut(|d| d.insert_temp(cache_id, place_name.clone()));

                                                state.active_workspace_name = Some(place_vpath.0.clone());

                                                // Resolve virtual path to host path for the IDE
                                                for mount in &realm.mounts {
                                                    let p_clean = place_vpath.0.trim_matches('/');
                                                    let m_clean = mount.virtual_path.trim_matches('/');

                                                    let is_match = if m_clean.is_empty() {
                                                        true
                                                    } else if p_clean == m_clean {
                                                        true
                                                    } else if p_clean.starts_with(&format!("{}/", m_clean)) {
                                                        true
                                                    } else {
                                                        false
                                                    };

                                                    if is_match {
                                                        let relative = if m_clean.is_empty() {
                                                            p_clean
                                                        } else {
                                                            p_clean.strip_prefix(m_clean).unwrap_or("").trim_start_matches('/')
                                                        };
                                                        state.project_root = Some(mount.host_path.join(relative));
                                                        break;
                                                    }
                                                }
                                            }
                                        }
                                    });
                            }

                            ui.label(egui::RichText::new("/").color(orange).strong());

                            // --- 🏰 The Realm ---
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
                                            if let Some(realm_dir) = directories::ProjectDirs::from("", "", "inforno")
                                                .map(|d| d.config_dir().join("realms").join(&active_realm.name)) {
                                                
                                                let yaml_path = realm_dir.join("realm2.yml");
                                                if let Ok(config_str) = std::fs::read_to_string(&yaml_path) {
                                                    state.realm_config_state.yaml_buffer = config_str.clone();
                                                    state.realm_config_state.original_yaml = config_str;
                                                }
                                            }
                                        }
                                    }
                                }

                            #[cfg(target_os = "linux")]
                            {
                                // --- ⚙ Autorno Daemon Status ---
                                let now = std::time::Instant::now();
                                let (is_running, last_check) = ctx.data_mut(|d| {
                                    d.get_temp::<(bool, std::time::Instant)>(egui::Id::new("autorno_status"))
                                     .unwrap_or((false, now - std::time::Duration::from_secs(10)))
                                });
                                
                                let mut new_running = is_running;
                                // Ping the Unix socket once every 2 seconds to avoid spanning too many syscalls
                                if now.duration_since(last_check).as_secs_f32() > 2.0 {
                                    if let Some(proj_dirs) = directories::ProjectDirs::from("", "", "inforno") {
                                        let socket_path = proj_dirs.cache_dir().join("autorno.sock");
                                        new_running = std::os::unix::net::UnixStream::connect(socket_path).is_ok();
                                    }
                                    ctx.data_mut(|d| d.insert_temp(egui::Id::new("autorno_status"), (new_running, now)));
                                }

                                let (status_color, status_text) = if new_running {
                                    (egui::Color32::GREEN, "🟢")
                                } else {
                                    (ui.visuals().error_fg_color, "🔴")
                                };

                                if ui.button(egui::RichText::new(status_text).color(status_color))
                                    .on_hover_text(if new_running { "Autorno daemon is running" } else { "Start Autorno daemon" })
                                    .clicked() 
                                {
                                    if !new_running {
                                        if let Ok(exe_path) = std::env::current_exe() {
                                            if let Some(parent) = exe_path.parent() {
                                                let autorno_path = parent.join("autorno");
                                                let _ = std::process::Command::new(autorno_path).spawn();
                                                // Force an immediate re-check next frame
                                                ctx.data_mut(|d| d.insert_temp(egui::Id::new("autorno_status"), (false, now - std::time::Duration::from_secs(10))));
                                            }
                                        }
                                    }
                                }

                                // --- 💻 Terminal ---
                                if ui.button("💻").on_hover_text("Open interactive terminal in VFS (as gui)").clicked() {
                                    if let Ok(exe_path) = std::env::current_exe() {
                                        if let Some(parent) = exe_path.parent() {
                                            let autorno_path = parent.join("autorno");
                                            
                                            // Extract the first bookmark's path. 
                                            // (Ensure `places` uses IndexMap in your Realm struct to guarantee order).
                                            // If your places are structs requiring the mandatory 'intro' field, change this to: .map(|p| p.path.clone())
                                            let start_dir = realm.raw_config.places.values().next()
                                                .map(|v| v.0.clone())
                                                .unwrap_or_else(|| "/".to_string());
                                                
                                            let shell_cmd = format!("cd {} && exec bash", start_dir);
                                            
                                            // Fallback chain for different Desktop Environments
                                            let terms = [
                                                ("x-terminal-emulator", vec!["-e"]),
                                                ("gnome-terminal", vec!["--"]),
                                                ("konsole", vec!["-e"]),
                                                ("alacritty", vec!["-e"]),
                                                ("kitty", vec!["--"]),
                                            ];
                                            
                                            for (term, t_args) in terms {
                                                if std::process::Command::new(term)
                                                    .args(&t_args)
                                                    .arg(&autorno_path)
                                                    .arg("exec")
                                                    .arg(&realm.name)
                                                    .arg("gui")
                                                    .arg(&shell_cmd)
                                                    .spawn()
                                                    .is_ok() 
                                                {
                                                    break;
                                                }
                                            }
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
