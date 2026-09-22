use egui::{Color32, RichText};
use rust_i18n::t;

use inforno_core::{common::{FileOp, FileOpMsg}, db::reset_sandbox_db};
use crate::{emoji_render::{emoji_button, emoji_image, emoji_label}, state::{State, err_color}};
use crate::mybtn;

fn switch_to_realm_sandbox(state: &mut State, realm_name: &str, sandbox_key: &str) {
    if let Some(proj_dirs) = directories::ProjectDirs::from("", "", "inforno") {
        let yaml_path = proj_dirs.config_dir().join("realms").join(realm_name).join("realm2.yml");
        if let Ok(yaml_str) = std::fs::read_to_string(&yaml_path) {
            if let Ok(config) = serde_saphyr::from_str::<inforno_core::realm::RealmConfig>(&yaml_str) {
                if let Ok(path) = inforno_core::realm::resolve_sandbox_path(sandbox_key, &config) {
                    *state.perma.active_realm_name.lock().unwrap() = Some(realm_name.to_string());
                    state.reload(Some(path));
                    return;
                }
            }
        }
    }
    state.error_msg = Some(format!("Failed to resolve sandbox '{}' for realm '{}'", sandbox_key, realm_name));
    state.is_modal_open = true;
}

pub fn ui_top_panel(ui: &mut egui::Ui, state: &mut State) {
    let ctx = ui.ctx().clone();

    // Intercept any startup errors and show them as a modal popup
    if let Some(err) = ctx.data_mut(|d| d.remove_temp::<String>(egui::Id::new("startup_error"))) {
        state.error_msg = Some(err);
        state.is_modal_open = true;

        if let Some(broken_yaml) = ctx.data_mut(|d| d.remove_temp::<String>(egui::Id::new("broken_realm_yaml"))) {
            state.realm_config_state.yaml_buffer = broken_yaml.clone();
            state.realm_config_state.original_yaml = broken_yaml;
            state.realm_config_state.cached_config = None;
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

            ui.colored_label(ui.visuals().code_bg_color,"|");

            // --- 📦 SANDBOX MENU ---
            // Scan the realms directory every 2 seconds to keep the list fresh
            let now = std::time::Instant::now();
            let (realms_cache, last_scan) = ctx.data_mut(|d| {
                d.get_temp::<(Vec<(String, Vec<String>)>, std::time::Instant)>(egui::Id::new("realms_scan_cache"))
                    .unwrap_or((vec![], now - std::time::Duration::from_secs(10)))
            });

            let mut new_realms = realms_cache.clone();
            if now.duration_since(last_scan).as_secs_f32() > 2.0 {
                new_realms.clear();
                if let Some(proj_dirs) = directories::ProjectDirs::from("", "", "inforno") {
                    let realms_dir = proj_dirs.config_dir().join("realms");
                    if let Ok(entries) = std::fs::read_dir(realms_dir) {
                        for entry in entries.flatten() {
                            if entry.path().is_dir() {
                                let realm_name = entry.file_name().to_string_lossy().to_string();
                                let yaml_path = entry.path().join("realm2.yml");
                                if yaml_path.exists() {
                                    let mut sandbox_keys = vec![];
                                    if let Ok(yaml_str) = std::fs::read_to_string(&yaml_path) {
                                        if let Ok(config) = serde_saphyr::from_str::<inforno_core::realm::RealmConfig>(&yaml_str) {
                                            sandbox_keys = config.sandboxes.keys().cloned().collect();
                                        }
                                    }
                                    new_realms.push((realm_name, sandbox_keys));
                                }
                            }
                        }
                    }
                }
                new_realms.sort_by(|a, b| a.0.cmp(&b.0));
                ctx.data_mut(|d| d.insert_temp(egui::Id::new("realms_scan_cache"), (new_realms.clone(), now)));
            }

            ui.menu_button(t!("menu_sandbox"), |ui| {
                if ui.button("✨ New Realm").clicked() {
                    state.realm_config_state.show_new_realm_wizard = true;
                    state.realm_config_state.wizard_step = 0;
                    state.realm_config_state.wizard_project_type = 0;
                    state.realm_config_state.wizard_realm_name.clear();
                    state.realm_config_state.wizard_sandbox_option = 0;
                    ui.close();
                }
                ui.separator();

                // 1. Dynamic Realms List
                if !new_realms.is_empty() {
                    ui.label(egui::RichText::new("🏰 Realms").strong().color(ui.visuals().warn_fg_color));
                    for (realm_name, sandboxes) in &new_realms {
                        if sandboxes.is_empty() {
                            if ui.button(format!("{} (No Sandboxes)", realm_name)).clicked() {
                                *state.perma.realm_awaiting_sandbox.lock().unwrap() = Some(realm_name.clone());
                                *state.perma.active_realm_name.lock().unwrap() = Some(realm_name.clone());
                                state.reload(None);
                                ui.close();
                            }
                        } else if sandboxes.len() == 1 {
                            let sb_key = &sandboxes[0];
                            if ui.button(realm_name).clicked() {
                                switch_to_realm_sandbox(state, realm_name, sb_key);
                                ui.close();
                            }
                        } else {
                            ui.menu_button(realm_name, |ui| {
                                for sb_key in sandboxes {
                                    if ui.button(sb_key).clicked() {
                                        switch_to_realm_sandbox(state, realm_name, sb_key);
                                        ui.close();
                                    }
                                }
                            });
                        }
                    }
                    ui.separator();
                }

                // 2. Base Sandbox Actions
                let is_home = state.is_in_home_sandbox && state.active_realm.is_none();
                if ui.add_enabled(!is_home, egui::Button::new(t!("menu_sandbox_home_btn"))).clicked() {
                    *state.perma.active_realm_name.lock().unwrap() = None; // Break out of the realm
                    state.reload(None);
                    ui.close();
                }

                if ui.button(t!("menu_sandbox_open_btn")).clicked() {
                    state.pending_file_dialog_op = Some(FileOp::Open);
                    state.file_dialog = egui_file_dialog::FileDialog::new()
                        .add_file_filter(
                            "Inforno Sandbox",
                            egui_file_dialog::Filter::new(|p: &std::path::Path| {
                                p.extension().is_some_and(|ext| ext == "rno")
                            })
                        );
                    state.file_dialog.pick_file();
                    ui.close();
                }

                ui.separator();

                if mybtn!(ui, "menu_sandbox_save_as_btn") {
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
                    ui.close();
                }

                if mybtn!(ui, "menu_sandbox_save_copy_btn") {
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
                    ui.close();
                }

                ui.separator();

                if ui.button(egui::RichText::new(t!("menu_sandbox_clear")).color(ui.visuals().error_fg_color)).clicked() {
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
                    ui.close();
                }
            }).response.on_hover_text(
                egui::RichText::new(t!("menu_sandbox_tooltip"))
                .strong()
                .heading()
            );

            #[cfg(target_os = "linux")]
            {
                ui.colored_label(ui.visuals().code_bg_color,"|");
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
                    (egui::Color32::GREEN, "😈") // "🟢")
                } else {
                    (ui.visuals().error_fg_color, "👿") // "🔴")
                };

                if ui.button(egui::RichText::new(status_text).color(status_color))
                    .on_hover_text(if new_running { "Autorno daemon is running" } else { "Start Autorno daemon" })
                    .clicked()
                {
                    if !new_running {
                        if let Ok(exe_path) = std::env::current_exe() {
                            if let Some(parent) = exe_path.parent() {
                                inforno_core::realm_mount::cleanup_orphaned_mounts();

                                let autorno_path = parent.join("autorno");
                                let _ = std::process::Command::new(autorno_path).spawn();
                                // Force an immediate re-check next frame
                                ctx.data_mut(|d| d.insert_temp(egui::Id::new("autorno_status"), (false, now - std::time::Duration::from_secs(10))));
                            }
                        }
                    }
                }
            }

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


            if ui.add(crate::emoji_render::emoji_button_widget(ui, '🌙')).clicked() {
                ctx.set_theme(egui::Theme::Dark);
            }

            if ui.add(crate::emoji_render::emoji_button_widget(ui, '🔆')).clicked() {
                ctx.set_theme(egui::Theme::Light);
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

            // ui.colored_label(ui.visuals().code_bg_color,"|");

            ui.colored_label(ui.visuals().code_bg_color,"|");

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
                                egui::RichText::new(format!("{}", realm.name))
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
                                                    state.realm_config_state.cached_config = None;
                                                }
                                            }
                                        }
                                    }
                                }
                            emoji_label(ui, "🏰");

                            #[cfg(target_os = "linux")]
                            {
                                if ui.button("👥").on_hover_text("Open interactive terminal as role...").clicked() {
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
                                                    .arg("--workdir")
                                                    .arg(&start_dir)
                                                    .arg(&realm.name)
                                                    .arg("gui")
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
