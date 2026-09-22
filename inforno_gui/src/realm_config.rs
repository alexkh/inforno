use egui::{Color32, RichText, ScrollArea};
use crate::state::State;
use bulat::editor::{CodeEditor, Syntax, ColorTheme};

#[derive(Default)]
pub struct RealmConfigState {
    // The raw text being edited on the right side
    pub yaml_buffer: String,
    // The original loaded text to track unsaved changes
    pub original_yaml: String,
    // If the user made a typo in the YAML, we store the error here
    pub parse_error: Option<String>,
    // Tracks if changes in the form need to be serialized back to the text buffer
    pub sync_to_yaml_needed: bool,

    // --- Visual Builder: Mount Edit State ---
    pub is_editing_mount: bool,
    pub mount_edit_original_key: Option<String>, // None means "Adding new"
    pub mount_edit_key: String,
    pub mount_edit_host: String,
    pub mount_edit_ro: bool,
    // Mounts no longer have their own `intro` field -- the mount's
    // (optional) description now lives in the YAML comment on its key,
    // the same slot `RealmMountConfig` used to keep separately.
    pub mount_edit_comment: String,

    // --- Visual Builder: Place Edit State ---
    pub is_editing_place: bool,
    pub place_edit_original_key: Option<String>, // None means "Adding new"
    pub place_edit_key: String,
    pub place_edit_path: String,
    pub place_edit_comment: String,

    // --- Visual Builder: Span Edit State ---
    pub is_editing_span: bool,
    pub span_edit_original_key: Option<String>, // None means "Adding new"
    pub span_edit_key: String,
    pub span_edit_expr: Option<inforno_core::realm::GlobExpr>,

    // --- Visual Builder: Tier Edit State ---
    pub is_editing_tier: bool,
    pub tier_edit_original_key: Option<u32>, // None means "Adding new"
    pub tier_edit_key: String,
    pub tier_edit_comment: String,
    pub tier_edit_bin: String,
    pub tier_edit_env: String,
    pub tier_edit_powers: Vec<serde_saphyr::Commented<inforno_core::realm::Power>>,

    // --- Visual Builder: Role Edit State ---
    pub is_editing_role: bool,
    pub role_edit_original_key: Option<String>, // None means "Adding new"
    pub role_edit_key: String,
    pub role_edit_comment: String,
    pub role_edit_tier: String,
    pub role_edit_boss: String,
    pub role_edit_bin: String,
    pub role_edit_env: String,
    pub role_edit_powers: Vec<serde_saphyr::Commented<inforno_core::realm::Power>>,

    // --- Visual Builder: Sandbox Edit State ---
    pub is_editing_sandbox: bool,
    pub sandbox_edit_original_key: Option<String>, // None means "Adding new"
    pub sandbox_edit_key: String,
    pub sandbox_edit_comment: String,
    pub sandbox_edit_path: String,
    pub sandbox_edit_roles: Vec<String>,

    pub cached_config: Option<inforno_core::realm::RealmConfig>,
    pub show_save_confirmation: bool,

    // The role selected in the VFS tree preview dropdown
    pub vfs_preview_role: String,

    // Indicates the app booted with a broken YAML file and is offering a rescue
    pub is_fixing_broken_realm: bool,
    // Tracks if a successful save occurred, requiring a reload when the window closes
    pub needs_reload: bool,
    // Explicitly tracked realm name to guarantee saving works reliably
    pub realm_name: Option<String>,

    // --- New Realm Wizard State ---
    pub show_new_realm_wizard: bool,
    pub wizard_step: usize,
    pub wizard_project_type: usize,
    pub wizard_realm_name: String,
    pub wizard_sandbox_option: usize,
}

pub fn ui_realm_config(ctx: &egui::Context, state: &mut State) {
    if state.realm_config_state.show_new_realm_wizard {
        ui_new_realm_wizard(ctx, state);
    }

    if !state.show_realm_config {
        return;
    }

    let mut is_open = state.show_realm_config;

    egui::Window::new("🏰 Realm Configuration")
        .default_width(900.0) // Wide enough for both columns
        .default_height(600.0)
        .open(&mut is_open)
        .show(ctx, |ui| {
            if state.is_modal_open {
                ui.disable();
            }

            // A 2-column layout for Form | YAML
            ui.columns(2, |columns| {
                // --- LEFT COLUMN: Interactive Form ---
                columns[0].vertical(|ui| {
                    ui.heading("Visual Builder");
                    ui.separator();

                    ScrollArea::vertical().id_salt("realm_form_scroll").show(ui, |ui| {
                        render_form_column(ui, state);
                    });
                });

                // --- RIGHT COLUMN: YAML & VFS Tree ---
                columns[1].vertical(|ui| {
                    // Ignore invisible trailing newline shifts created by the CodeEditor
                    let is_dirty = state.realm_config_state.yaml_buffer.trim() != state.realm_config_state.original_yaml.trim();
                    let can_save = is_dirty && state.realm_config_state.parse_error.is_none();
                    let mut trigger_save = false;

                    ui.horizontal(|ui| {
                        ui.heading("realm2.yml");
                        if is_dirty {
                            ui.label(egui::RichText::new("●").color(ui.visuals().warn_fg_color))
                                .on_hover_text("Unsaved changes");
                        }

                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.add_enabled(can_save, egui::Button::new("💾 Save & Apply")).clicked() {
                                trigger_save = true;
                            }
                        });
                    });
                    ui.separator();

                    if trigger_save {
                        // 1. Try explicit state cache, then active_realm, then perma lock
                        let realm_name_opt = state.realm_config_state.realm_name.clone()
                            .or_else(|| state.active_realm.as_ref().map(|r| r.name.clone()))
                            .or_else(|| state.perma.active_realm_name.lock().unwrap().clone());

                        if let Some(realm_name) = realm_name_opt {
                            if let Some(proj_dirs) = directories::ProjectDirs::from("", "", "inforno") {
                                let realm_dir = proj_dirs.config_dir().join("realms").join(&realm_name);
                                let yaml_path = realm_dir.join("realm2.yml");

                                match std::fs::write(&yaml_path, &state.realm_config_state.yaml_buffer) {
                                    Ok(_) => {
                                        state.realm_config_state.original_yaml = state.realm_config_state.yaml_buffer.clone();

                                        // Instant apply and reload!
                                        if let Ok(new_config) = serde_saphyr::from_str::<inforno_core::realm::RealmConfig>(&state.realm_config_state.yaml_buffer) {
                                            if let Ok(mut lock) = state.perma.active_realm_name.lock() {
                                                *lock = Some(realm_name.clone());
                                            }

                                            let target_sandbox = match inforno_core::realm::resolve_default_sandbox_path(&new_config) {
                                                Ok(resolved) => resolved,
                                                Err(_) => {
                                                    if let Ok(mut lock) = state.perma.realm_awaiting_sandbox.lock() {
                                                        *lock = Some(realm_name.clone());
                                                    }
                                                    state.sandbox.clone()
                                                }
                                            };

                                            // Cache the text so we can reinject it into the fresh state
                                            let cached_yaml = state.realm_config_state.yaml_buffer.clone();

                                            state.reload(Some(target_sandbox));

                                            // Repopulate the fresh state so the window doesn't go blank!
                                            state.realm_config_state.yaml_buffer = cached_yaml.clone();
                                            state.realm_config_state.original_yaml = cached_yaml;
                                            state.realm_config_state.realm_name = Some(realm_name.clone());
                                            state.realm_config_state.is_fixing_broken_realm = false;
                                            state.realm_config_state.needs_reload = false;
                                            
                                            // Ensure window stays open
                                            state.show_realm_config = true;
                                        }
                                    }
                                    Err(e) => {
                                        state.error_msg = Some(format!("Failed to save realm2.yml: {}", e));
                                        state.is_modal_open = true;
                                    }
                                }
                            } else {
                                state.error_msg = Some("Save Failed: Could not resolve the project configuration directory.".to_string());
                                state.is_modal_open = true;
                            }
                        } else {
                            state.error_msg = Some("Save Failed: Could not determine the active Realm name. Try relaunching with `--realm <name>`.".to_string());
                            state.is_modal_open = true;
                        }
                    }

                    let substate = &mut state.realm_config_state;

                    // 1. Live YAML Editor
                    let mut yaml_changed = false;

                    // Let the outer egui layout strict-bound the height and handle scrolling natively
                    ScrollArea::both().id_salt("realm_yaml_scroll").max_height(350.0).show(ui, |ui| {
                        let num_lines = substate.yaml_buffer.lines().count().max(1);

                        let out = CodeEditor::default()
                            .id_source("realm_yaml_editor")
                            .with_theme(ColorTheme::SV)
                            .with_syntax(Syntax::yaml())
                            .with_numlines(true)
                            .with_rows(num_lines + 1)
                            .vscroll(false) // Disable internal scrolling
                            .v_auto_shrink(true) // Uncap internal height so the parent handles the bounds
                            .show(ui, &mut substate.yaml_buffer);

                        yaml_changed = out.output.response.changed();
                    });

                    // If user types in the right pane, we try to parse it
                    if yaml_changed {
                        match serde_saphyr::from_str::<inforno_core::realm::RealmConfig>(&substate.yaml_buffer) {
                            Ok(new_config) => {
                                substate.parse_error = None;
                                substate.cached_config = Some(new_config);
                            },
                            Err(e) => {
                                substate.parse_error = Some(e.to_string());
                            }
                        }
                    }

                    if let Some(err) = &substate.parse_error {
                        ui.colored_label(ui.visuals().error_fg_color, format!("YAML Error: {}", err));
                    } else if !substate.yaml_buffer.is_empty() {
                       ui.colored_label(Color32::GREEN, "✔ YAML is valid");
                    }

                    ui.add_space(20.0);

                    // 2. Live VFS Tree Visualization
                    ui.heading("Active Virtual File System");
                    ui.separator();
                    if let Some(realm) = &state.active_realm {
                        ui.horizontal(|ui| {
                            ui.label("Preview as Role:");
                            if substate.vfs_preview_role.is_empty() && !realm.roles.is_empty() {
                                substate.vfs_preview_role = realm.roles.keys().next().unwrap().clone();
                            }

                            egui::ComboBox::from_id_salt("vfs_preview_role")
                                .selected_text(&substate.vfs_preview_role)
                                .show_ui(ui, |ui| {
                                    for role in realm.roles.keys() {
                                        ui.selectable_value(&mut substate.vfs_preview_role, role.clone(), role);
                                    }
                                });
                        });
                        ui.add_space(5.0);

                        // Use both() to allow horizontal scrolling for deep folder trees
                        ScrollArea::both().id_salt("vfs_tree_scroll").max_height(350.0).show(ui, |ui| {
                            render_vfs_tree(ui, realm, &substate.vfs_preview_role);
                        });
                    } else {
                        ui.label("No active realm loaded to display (Apply changes first).");
                    }
                });
            });
        });

    // Intercept window close event to apply and reload
    let mut close_requested = state.show_realm_config && !is_open;

    if close_requested {
        let is_dirty = state.realm_config_state.yaml_buffer.trim() != state.realm_config_state.original_yaml.trim();
        if is_dirty {
            is_open = true; // Prevent closing
            state.realm_config_state.show_save_confirmation = true;
        } else {
            if state.realm_config_state.needs_reload {
                state.realm_config_state.needs_reload = false;

                let mut target_sandbox = state.sandbox.clone();
                if state.realm_config_state.is_fixing_broken_realm {
                    if let Some(r_name) = &state.realm_config_state.realm_name {
                        if let Ok(mut lock) = state.perma.active_realm_name.lock() {
                            *lock = Some(r_name.clone());
                        }
                        if let Ok(new_config) = serde_saphyr::from_str::<inforno_core::realm::RealmConfig>(&state.realm_config_state.yaml_buffer) {
                            match inforno_core::realm::resolve_default_sandbox_path(&new_config) {
                                Ok(resolved) => target_sandbox = resolved,
                                Err(_) => {
                                    if let Ok(mut lock) = state.perma.realm_awaiting_sandbox.lock() {
                                        *lock = Some(r_name.clone());
                                    }
                                }
                            }
                        }
                    }
                }
                state.realm_config_state.is_fixing_broken_realm = false;
                state.reload(Some(target_sandbox));
            } else if state.realm_config_state.is_fixing_broken_realm {
                state.realm_config_state.is_fixing_broken_realm = false;
            }
        }
    }

    if state.realm_config_state.show_save_confirmation {
        let mut modal_open = true;
        egui::Window::new("Unsaved Changes")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .open(&mut modal_open)
            .show(ctx, |ui| {
                ui.label("You have unsaved changes in realm2.yml.");
                ui.label("Do you want to save them before closing?");
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if ui.button("💾 Save & Close").clicked() {
                        let realm_name_opt = state.realm_config_state.realm_name.clone()
                            .or_else(|| state.active_realm.as_ref().map(|r| r.name.clone()))
                            .or_else(|| state.perma.active_realm_name.lock().unwrap().clone());

                        if let Some(realm_name) = realm_name_opt {
                            if let Some(proj_dirs) = directories::ProjectDirs::from("", "", "inforno") {
                                let realm_dir = proj_dirs.config_dir().join("realms").join(&realm_name);
                                let yaml_path = realm_dir.join("realm2.yml");
                                if std::fs::write(&yaml_path, &state.realm_config_state.yaml_buffer).is_ok() {
                                    state.realm_config_state.original_yaml = state.realm_config_state.yaml_buffer.clone();
                                }
                            }
                        }

                        state.realm_config_state.show_save_confirmation = false;
                        is_open = false;

                        let mut target_sandbox = state.sandbox.clone();
                        if state.realm_config_state.is_fixing_broken_realm {
                            if let Some(r_name) = &state.realm_config_state.realm_name {
                                if let Ok(mut lock) = state.perma.active_realm_name.lock() {
                                    *lock = Some(r_name.clone());
                                }
                                if let Ok(new_config) = serde_saphyr::from_str::<inforno_core::realm::RealmConfig>(&state.realm_config_state.yaml_buffer) {
                                    match inforno_core::realm::resolve_default_sandbox_path(&new_config) {
                                        Ok(resolved) => target_sandbox = resolved,
                                        Err(_) => {
                                            if let Ok(mut lock) = state.perma.realm_awaiting_sandbox.lock() {
                                                *lock = Some(r_name.clone());
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        state.realm_config_state.is_fixing_broken_realm = false;
                        state.realm_config_state.needs_reload = false;
                        state.reload(Some(target_sandbox));
                    }
                    if ui.button("🗑 Discard & Close").clicked() {
                        state.realm_config_state.yaml_buffer = state.realm_config_state.original_yaml.clone();
                        state.realm_config_state.cached_config = None;
                        state.realm_config_state.show_save_confirmation = false;
                        is_open = false;

                        if state.realm_config_state.needs_reload {
                            state.realm_config_state.needs_reload = false;
                            state.reload(Some(state.sandbox.clone()));
                        }
                        state.realm_config_state.is_fixing_broken_realm = false;
                    }
                    if ui.button("✖ Cancel").clicked() {
                        state.realm_config_state.show_save_confirmation = false;
                    }
                });
            });

        if !modal_open {
            state.realm_config_state.show_save_confirmation = false;
        }
    }

    state.show_realm_config = is_open;
}

// --- Helper Functions ---

fn render_form_column(ui: &mut egui::Ui, state: &mut State) {
    let substate = &mut state.realm_config_state;

    if substate.cached_config.is_none() {
        match serde_saphyr::from_str::<inforno_core::realm::RealmConfig>(&substate.yaml_buffer) {
            Ok(c) => {
                substate.cached_config = Some(c);
            }
            Err(e) => {
                ui.label(egui::RichText::new("Fix YAML errors on the right to use the Visual Builder.")
                    .color(ui.visuals().error_fg_color)
                    .strong()
                );
                ui.add_space(8.0);
                ui.label(egui::RichText::new(format!("Error Details:\n{}", e))
                    .color(ui.visuals().error_fg_color)
                    .monospace()
                );
                return;
            }
        }
    }

    let parsed_config = substate.cached_config.as_ref().unwrap().clone();
    let mut new_config = parsed_config.clone();
    let mut config_changed = false;

    egui::CollapsingHeader::new(format!("🗄 Mounts ({})", parsed_config.mounts.len()))
        .default_open(true)
        .show(ui, |ui| {
            if substate.is_editing_mount {
                ui.group(|ui| {
                    ui.heading(if substate.mount_edit_original_key.is_some() { "Edit Mount" } else { "Add Mount" });

                    ui.horizontal(|ui| {
                        ui.label("Mount Point (Virtual):");
                        ui.text_edit_singleline(&mut substate.mount_edit_key);
                    });

                    ui.horizontal(|ui| {
                        ui.label("Host Directory:");
                        ui.text_edit_singleline(&mut substate.mount_edit_host);
                    });
                    ui.label(egui::RichText::new("Host path resolves to canonical absolute path on Apply.").weak().small());

                    ui.checkbox(&mut substate.mount_edit_ro, "Read Only");

                    ui.horizontal(|ui| {
                        ui.label("Comment (optional):");
                        ui.text_edit_multiline(&mut substate.mount_edit_comment);
                    });

                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        if ui.button("✔ Apply").clicked() {
                            let host_path = std::path::PathBuf::from(substate.mount_edit_host.trim());
                            let canonical_host = std::fs::canonicalize(&host_path).unwrap_or(host_path);

                            let new_mount = inforno_core::realm::RealmMountConfig {
                                host: canonical_host,
                                read_only: substate.mount_edit_ro,
                            };
                            let comment = substate.mount_edit_comment.trim();
                            let new_comment = comment.to_string();

                            let new_key = substate.mount_edit_key.trim().to_string();

                            if let Some(ref orig_key) = substate.mount_edit_original_key {
                                // Rebuild map to preserve insertion order where possible.
                                let mut new_mounts = indexmap::IndexMap::new();
                                for (k, v) in parsed_config.mounts.iter() {
                                    if k == orig_key {
                                        new_mounts.insert(new_key.clone(), serde_saphyr::Commented(new_mount.clone(), new_comment.clone()));
                                    } else {
                                        new_mounts.insert(k.clone(), v.clone());
                                    }
                                }
                                new_config.mounts = new_mounts;
                            } else {
                                new_config.mounts.insert(new_key, serde_saphyr::Commented(new_mount, new_comment));
                            }

                            config_changed = true;
                            substate.is_editing_mount = false;
                        }

                        if ui.button("✖ Cancel").clicked() {
                            substate.is_editing_mount = false;
                        }
                    });
                });
            } else {
                for (name, mount) in &parsed_config.mounts {
                    let mount = &mount.0;
                    ui.group(|ui| {
                        if let Some(c) = parsed_config.mounts.get(name) {
                            if !c.1.trim().is_empty() {
                                ui.label(egui::RichText::new(format!("# {}", c.1.trim())).weak());
                            }
                        }
                        ui.horizontal(|ui| {
                            if mount.read_only {
                                ui.label("(RO)").on_hover_text("Read Only Mount");
                            } else {
                                ui.label("(RW)").on_hover_text("Read-Write Mount");
                            }
                            ui.label(egui::RichText::new(name).strong());
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui.button("🗑").on_hover_text("Delete Mount").clicked() {
                                    new_config.mounts.shift_remove(name);
                                    config_changed = true;
                                }
                                if ui.button("✏").on_hover_text("Edit Mount").clicked() {
                                    substate.is_editing_mount = true;
                                    substate.mount_edit_original_key = Some(name.clone());
                                    substate.mount_edit_key = name.clone();
                                    substate.mount_edit_host = mount.host.display().to_string();
                                    substate.mount_edit_ro = mount.read_only;
                                    substate.mount_edit_comment = parsed_config.mounts.get(name)
                                        .map(|c| c.1.trim().to_string())
                                        .unwrap_or_default();
                                }
                            });
                        });
                        ui.horizontal(|ui| {
                            ui.label("Host:");
                            ui.label(mount.host.display().to_string());
                        });
                    });
                }
                if ui.button("+ Add Mount").clicked() {
                    substate.is_editing_mount = true;
                    substate.mount_edit_original_key = None;
                    substate.mount_edit_key = "/new_mount".to_string();
                    substate.mount_edit_host = "".to_string();
                    substate.mount_edit_ro = true;
                    substate.mount_edit_comment = "".to_string();
                }
            }
        });

    ui.add_space(10.0);

    egui::CollapsingHeader::new(format!("🗼 Places ({})", parsed_config.places.len()))
        .show(ui, |ui| {
            if substate.is_editing_place {
                ui.group(|ui| {
                    ui.heading(if substate.place_edit_original_key.is_some() { "Edit Place" } else { "Add Place" });

                    ui.horizontal(|ui| {
                        ui.label("Name:");
                        ui.text_edit_singleline(&mut substate.place_edit_key);
                    });

                    ui.horizontal(|ui| {
                        ui.label("Path:");
                        ui.text_edit_singleline(&mut substate.place_edit_path);
                    });

                    ui.horizontal(|ui| {
                        ui.label("Intro (Comment):");
                        ui.text_edit_singleline(&mut substate.place_edit_comment);
                    });

                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        let can_apply = !substate.place_edit_key.trim().is_empty() &&
                                        !substate.place_edit_path.trim().is_empty() &&
                                        !substate.place_edit_comment.trim().is_empty();

                        if ui.add_enabled(can_apply, egui::Button::new("✔ Apply"))
                            .on_disabled_hover_text("Name, Path, and Intro are all strictly required.")
                            .clicked() {

                            let new_key = substate.place_edit_key.trim().to_string();
                            let new_place = serde_saphyr::Commented(
                                substate.place_edit_path.trim().to_string(),
                                substate.place_edit_comment.trim().to_string()
                            );

                            if let Some(ref orig_key) = substate.place_edit_original_key {
                                let mut new_places = indexmap::IndexMap::new();
                                for (k, v) in parsed_config.places.iter() {
                                    if k == orig_key {
                                        new_places.insert(new_key.clone(), new_place.clone());
                                    } else {
                                        new_places.insert(k.clone(), v.clone());
                                    }
                                }
                                new_config.places = new_places;
                            } else {
                                new_config.places.insert(new_key, new_place);
                            }

                            config_changed = true;
                            substate.is_editing_place = false;
                        }

                        if ui.button("✖ Cancel").clicked() {
                            substate.is_editing_place = false;
                        }
                    });
                });
            } else {
                for (name, path) in &parsed_config.places {
                    ui.group(|ui| {
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new(name).strong());
                            ui.label("->");
                            ui.label(&path.0);
                            if !path.1.is_empty() {
                                ui.label(egui::RichText::new(format!("# {}", path.1.trim())).weak());
                            }
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui.button("🗑").on_hover_text("Delete Place").clicked() {
                                    new_config.places.shift_remove(name);
                                    config_changed = true;
                                }
                                if ui.button("✏").on_hover_text("Edit Place").clicked() {
                                    substate.is_editing_place = true;
                                    substate.place_edit_original_key = Some(name.clone());
                                    substate.place_edit_key = name.clone();
                                    substate.place_edit_path = path.0.clone();
                                    substate.place_edit_comment = path.1.trim().to_string();
                                }
                            });
                        });
                    });
                }
                if ui.button("+ Add Place").clicked() {
                    substate.is_editing_place = true;
                    substate.place_edit_original_key = None;
                    substate.place_edit_key = "new_place".to_string();
                    substate.place_edit_path = "/path/to/place".to_string();
                    substate.place_edit_comment = "Required introduction description".to_string();
                }
            }
        });

    ui.add_space(10.0);

    egui::CollapsingHeader::new(format!("🎯 Spans ({})", parsed_config.expressions.len()))
        .show(ui, |ui| {
            if substate.is_editing_span {
                ui.group(|ui| {
                    ui.heading(if substate.span_edit_original_key.is_some() { "Edit Span" } else { "Add Span" });

                    ui.horizontal(|ui| {
                        ui.label("Name:");
                        ui.text_edit_singleline(&mut substate.span_edit_key);
                    });

                    ui.add_space(5.0);
                    ui.label("Expression:");

                    let available_refs: Vec<String> = parsed_config.expressions.keys().cloned().collect();

                    if let Some(mut expr) = substate.span_edit_expr.take() {
                        ui_edit_glob_expr(ui, &mut expr, &available_refs, 0);
                        substate.span_edit_expr = Some(expr);
                    }

                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        let can_apply = !substate.span_edit_key.trim().is_empty() && substate.span_edit_expr.is_some();

                        if ui.add_enabled(can_apply, egui::Button::new("✔ Apply")).clicked() {
                            let new_key = substate.span_edit_key.trim().to_string();
                            let new_expr = substate.span_edit_expr.take().unwrap();

                            if let Some(ref orig_key) = substate.span_edit_original_key {
                                let mut new_exprs = indexmap::IndexMap::new();
                                for (k, v) in parsed_config.expressions.iter() {
                                    if k == orig_key {
                                        new_exprs.insert(new_key.clone(), new_expr.clone());
                                    } else {
                                        new_exprs.insert(k.clone(), v.clone());
                                    }
                                }
                                new_config.expressions = new_exprs;
                            } else {
                                new_config.expressions.insert(new_key, new_expr);
                            }

                            config_changed = true;
                            substate.is_editing_span = false;
                        }

                        if ui.button("✖ Cancel").clicked() {
                            substate.is_editing_span = false;
                            substate.span_edit_expr = None;
                        }
                    });
                });
            } else {
                for (name, expr) in &parsed_config.expressions {
                    ui.group(|ui| {
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new(name).strong());
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui.button("🗑").on_hover_text("Delete Span").clicked() {
                                    new_config.expressions.shift_remove(name);
                                    config_changed = true;
                                }
                                if ui.button("✏").on_hover_text("Edit Span").clicked() {
                                    substate.is_editing_span = true;
                                    substate.span_edit_original_key = Some(name.clone());
                                    substate.span_edit_key = name.clone();
                                    substate.span_edit_expr = Some(expr.clone());
                                }
                            });
                        });
                        ui.label(egui::RichText::new(inforno_core::realm::describe_expr(expr, &parsed_config.expressions)).weak().small());
                    });
                }
                if ui.button("+ Add Span").clicked() {
                    substate.is_editing_span = true;
                    substate.span_edit_original_key = None;
                    substate.span_edit_key = "new_span".to_string();
                    substate.span_edit_expr = Some(inforno_core::realm::GlobExpr::Pattern("**/*".to_string()));
                }
            }
        });
    ui.add_space(10.0);

    egui::CollapsingHeader::new(format!("🎓 Tiers ({})", parsed_config.tiers.len()))
        .show(ui, |ui| {
            let available_refs: Vec<String> = parsed_config.expressions.keys().cloned().collect();

            if substate.is_editing_tier {
                ui.group(|ui| {
                    ui.heading(if substate.tier_edit_original_key.is_some() { "Edit Tier" } else { "Add Tier" });

                    ui.horizontal(|ui| {
                        ui.label("Tier Level (2-9):");
                        ui.text_edit_singleline(&mut substate.tier_edit_key);
                    });

                    ui.horizontal(|ui| {
                        ui.label("Comment:");
                        ui.text_edit_singleline(&mut substate.tier_edit_comment);
                    });

                    ui.separator();

                    ui.add_space(5.0);
                    ui.columns(2, |cols| {
                        cols[0].vertical(|ui| {
                            ui.label("Extra Binaries (one per line):");
                            ui.text_edit_multiline(&mut substate.tier_edit_bin);
                        });
                        cols[1].vertical(|ui| {
                            ui.label("Environment Variables (one per line):");
                            ui.text_edit_multiline(&mut substate.tier_edit_env);
                        });
                    });

                    ui.separator();

                    ui.label(egui::RichText::new("Powers:").strong());
                    let mut to_remove_power = None;
                    for (i, p) in substate.tier_edit_powers.iter_mut().enumerate() {
                        ui.group(|ui| {
                            ui.horizontal(|ui| {
                                ui.label(egui::RichText::new(format!("Power {}", i + 1)).strong());
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    if ui.button("🗑").clicked() {
                                        to_remove_power = Some(i);
                                    }
                                });
                            });
                            ui.horizontal(|ui| {
                                ui.label("Comment:");
                                ui.text_edit_singleline(&mut p.1);
                            });

                            ui.label("Span:");
                            ui_edit_glob_expr(ui, &mut p.0.span, &available_refs, 2000 + i);

                            ui.horizontal(|ui| {
                                ui.label("Caps:");
                                let mut has_read = p.0.caps.contains(&inforno_core::realm::Cap::Read);
                                let mut has_write = p.0.caps.contains(&inforno_core::realm::Cap::Write);
                                let mut has_append = p.0.caps.contains(&inforno_core::realm::Cap::Append);
                                let mut has_create = p.0.caps.contains(&inforno_core::realm::Cap::Create);

                                if ui.checkbox(&mut has_read, "read").changed() {
                                    if has_read { p.0.caps.push(inforno_core::realm::Cap::Read); } else { p.0.caps.retain(|c| *c != inforno_core::realm::Cap::Read); }
                                }
                                if ui.checkbox(&mut has_write, "write").changed() {
                                    if has_write { p.0.caps.push(inforno_core::realm::Cap::Write); } else { p.0.caps.retain(|c| *c != inforno_core::realm::Cap::Write); }
                                }
                                if ui.checkbox(&mut has_append, "append").changed() {
                                    if has_append { p.0.caps.push(inforno_core::realm::Cap::Append); } else { p.0.caps.retain(|c| *c != inforno_core::realm::Cap::Append); }
                                }
                                if ui.checkbox(&mut has_create, "create").changed() {
                                    if has_create { p.0.caps.push(inforno_core::realm::Cap::Create); } else { p.0.caps.retain(|c| *c != inforno_core::realm::Cap::Create); }
                                }
                            });
                            ui.horizontal(|ui| {
                                let mut has_overrides = p.0.overrides.is_some();
                                if ui.checkbox(&mut has_overrides, "Overrides dotfiles").changed() {
                                    if has_overrides {
                                        p.0.overrides = Some("dotfiles".to_string());
                                    } else {
                                        p.0.overrides = None;
                                    }
                                }
                            });
                        });
                    }
                    if let Some(i) = to_remove_power {
                        substate.tier_edit_powers.remove(i);
                    }
                    if ui.button("+ Add Power").clicked() {
                        substate.tier_edit_powers.push(serde_saphyr::Commented(
                            inforno_core::realm::Power {
                                span: inforno_core::realm::GlobExpr::Pattern("**/*".to_string()),
                                caps: vec![inforno_core::realm::Cap::Read],
                                overrides: None,
                            },
                            "".to_string()
                        ));
                    }

                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        let parsed_key = substate.tier_edit_key.parse::<u32>();
                        let is_valid_key = parsed_key.is_ok() && parsed_key.as_ref().unwrap() >= &2 && parsed_key.as_ref().unwrap() <= &9;

                        if ui.add_enabled(is_valid_key, egui::Button::new("✔ Apply")).on_disabled_hover_text("Tier Level must be a number between 2 and 9.").clicked() {
                            let new_key = parsed_key.unwrap();
                            let bin_lines: Vec<String> = substate.tier_edit_bin.lines().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();

                            let new_tier = serde_saphyr::Commented(
                                inforno_core::realm::TierConfig {
                                    powers: substate.tier_edit_powers.clone(),
                                    bin: bin_lines,
                                    env: substate.tier_edit_env.lines().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect(),
                                },
                                substate.tier_edit_comment.clone()
                            );

                            if let Some(orig_key) = substate.tier_edit_original_key {
                                if orig_key != new_key {
                                    new_config.tiers.remove(&orig_key);
                                }
                            }
                            new_config.tiers.insert(new_key, new_tier);
                            config_changed = true;
                            substate.is_editing_tier = false;
                        }
                        if ui.button("✖ Cancel").clicked() {
                            substate.is_editing_tier = false;
                        }
                    });
                });
            } else {
                for (tier_num, tier_cfg) in &parsed_config.tiers {
                    let tier_inner = &tier_cfg.0;
                    ui.group(|ui| {
                        if !tier_cfg.1.trim().is_empty() {
                            ui.label(egui::RichText::new(format!("# {}", tier_cfg.1.trim())).weak());
                        }
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new(format!("Tier {}", tier_num)).strong());
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui.button("🗑").on_hover_text("Delete Tier").clicked() {
                                    new_config.tiers.remove(tier_num);
                                    config_changed = true;
                                }
                                if ui.button("✏").on_hover_text("Edit Tier").clicked() {
                                    substate.is_editing_tier = true;
                                    substate.tier_edit_original_key = Some(*tier_num);
                                    substate.tier_edit_key = tier_num.to_string();
                                    substate.tier_edit_comment = tier_cfg.1.trim().to_string();
                                    substate.tier_edit_bin = tier_inner.bin.join("\n");
                                    substate.tier_edit_env = tier_inner.env.join("\n");
                                    substate.tier_edit_powers = tier_inner.powers.clone();
                                }
                            });
                        });
                        ui.label(format!("Powers: {} defined", tier_inner.powers.len()));
                        if !tier_inner.bin.is_empty() {
                            ui.label(egui::RichText::new(format!("Includes {} extra binaries.", tier_inner.bin.len())).weak().small());
                        }
                        if !tier_inner.env.is_empty() {
                            ui.label(egui::RichText::new(format!("Includes {} env variables.", tier_inner.env.len())).weak().small());
                        }
                    });
                }
                if ui.button("+ Add Tier").clicked() {
                    substate.is_editing_tier = true;
                    substate.tier_edit_original_key = None;
                    substate.tier_edit_key = "2".to_string();
                    substate.tier_edit_comment = "".to_string();
                    substate.tier_edit_bin = "".to_string();
                    substate.tier_edit_env = "".to_string();
                    substate.tier_edit_powers = vec![];
                }
            }
        });

    ui.add_space(10.0);

    egui::CollapsingHeader::new(format!("🎭 Roles ({})", parsed_config.roles.len()))
        .show(ui, |ui| {
            let available_refs: Vec<String> = parsed_config.expressions.keys().cloned().collect();

            if substate.is_editing_role {
                ui.group(|ui| {
                    ui.heading(if substate.role_edit_original_key.is_some() { "Edit Role" } else { "Add Role" });

                    ui.horizontal(|ui| {
                        ui.label("Name:");
                        ui.text_edit_singleline(&mut substate.role_edit_key);
                    });

                    ui.horizontal(|ui| {
                        ui.label("Comment:");
                        ui.text_edit_singleline(&mut substate.role_edit_comment);
                    });

                    ui.horizontal(|ui| {
                        ui.label("Tier (0-9):");
                        ui.text_edit_singleline(&mut substate.role_edit_tier);
                    });

                    ui.horizontal(|ui| {
                        ui.label("Boss:");
                        ui.text_edit_singleline(&mut substate.role_edit_boss);
                    });

                    ui.add_space(5.0);
                    ui.columns(2, |cols| {
                        cols[0].vertical(|ui| {
                            ui.label("Extra Binaries (one per line):");
                            ui.text_edit_multiline(&mut substate.role_edit_bin);
                        });
                        cols[1].vertical(|ui| {
                            ui.label("Environment Variables (one per line):");
                            ui.text_edit_multiline(&mut substate.role_edit_env);
                        });
                    });

                    ui.separator();

                    ui.label(egui::RichText::new("Powers:").strong());
                    let mut to_remove_power = None;
                    for (i, p) in substate.role_edit_powers.iter_mut().enumerate() {
                        ui.group(|ui| {
                            ui.horizontal(|ui| {
                                ui.label(egui::RichText::new(format!("Power {}", i + 1)).strong());
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    if ui.button("🗑").clicked() {
                                        to_remove_power = Some(i);
                                    }
                                });
                            });
                            ui.horizontal(|ui| {
                                ui.label("Comment:");
                                ui.text_edit_singleline(&mut p.1);
                            });
                            
                            ui.label("Span:");
                            ui_edit_glob_expr(ui, &mut p.0.span, &available_refs, 3000 + i);

                            ui.horizontal(|ui| {
                                ui.label("Caps:");
                                let mut has_read = p.0.caps.contains(&inforno_core::realm::Cap::Read);
                                let mut has_write = p.0.caps.contains(&inforno_core::realm::Cap::Write);
                                let mut has_append = p.0.caps.contains(&inforno_core::realm::Cap::Append);
                                let mut has_create = p.0.caps.contains(&inforno_core::realm::Cap::Create);

                                if ui.checkbox(&mut has_read, "read").changed() {
                                    if has_read { p.0.caps.push(inforno_core::realm::Cap::Read); } else { p.0.caps.retain(|c| *c != inforno_core::realm::Cap::Read); }
                                }
                                if ui.checkbox(&mut has_write, "write").changed() {
                                    if has_write { p.0.caps.push(inforno_core::realm::Cap::Write); } else { p.0.caps.retain(|c| *c != inforno_core::realm::Cap::Write); }
                                }
                                if ui.checkbox(&mut has_append, "append").changed() {
                                    if has_append { p.0.caps.push(inforno_core::realm::Cap::Append); } else { p.0.caps.retain(|c| *c != inforno_core::realm::Cap::Append); }
                                }
                                if ui.checkbox(&mut has_create, "create").changed() {
                                    if has_create { p.0.caps.push(inforno_core::realm::Cap::Create); } else { p.0.caps.retain(|c| *c != inforno_core::realm::Cap::Create); }
                                }
                            });
                            ui.horizontal(|ui| {
                                let mut has_overrides = p.0.overrides.is_some();
                                if ui.checkbox(&mut has_overrides, "Overrides dotfiles").changed() {
                                    if has_overrides {
                                        p.0.overrides = Some("dotfiles".to_string());
                                    } else {
                                        p.0.overrides = None;
                                    }
                                }
                            });
                        });
                    }
                    if let Some(i) = to_remove_power {
                        substate.role_edit_powers.remove(i);
                    }
                    if ui.button("+ Add Power").clicked() {
                        substate.role_edit_powers.push(serde_saphyr::Commented(
                            inforno_core::realm::Power {
                                span: inforno_core::realm::GlobExpr::Pattern("**/*".to_string()),
                                caps: vec![inforno_core::realm::Cap::Read],
                                overrides: None,
                            },
                            "".to_string()
                        ));
                    }

                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        let parsed_tier = substate.role_edit_tier.parse::<u32>();
                        let is_valid_tier = parsed_tier.is_ok() && parsed_tier.as_ref().unwrap() <= &9;
                        
                        let mut tier_zero_violation = false;
                        if is_valid_tier && *parsed_tier.as_ref().unwrap() == 0 {
                            if !substate.role_edit_powers.is_empty() || !substate.role_edit_bin.trim().is_empty() {
                                tier_zero_violation = true;
                            }
                        }
                        
                        let can_apply = !substate.role_edit_key.trim().is_empty() && is_valid_tier && !tier_zero_violation;
                        
                        let tooltip = if tier_zero_violation {
                            "Tier 0 cannot have powers or extra binaries."
                        } else {
                            "Name is required and Tier must be a number between 0 and 9."
                        };

                        if ui.add_enabled(can_apply, egui::Button::new("✔ Apply")).on_disabled_hover_text(tooltip).clicked() {
                            let new_key = substate.role_edit_key.trim().to_string();
                            let new_tier = parsed_tier.unwrap();
                            let boss_val = substate.role_edit_boss.trim().to_string();
                            
                            let bin_lines: Vec<String> = substate.role_edit_bin.lines().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
                            let env_lines: Vec<String> = substate.role_edit_env.lines().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();

                            let new_role = serde_saphyr::Commented(
                                inforno_core::realm::RoleConfig {
                                    tier: inforno_core::realm::Tier(new_tier),
                                    boss: if boss_val.is_empty() { None } else { Some(boss_val) },
                                    powers: substate.role_edit_powers.clone(),
                                    bin: bin_lines,
                                    env: env_lines,
                                },
                                substate.role_edit_comment.clone()
                            );

                            if let Some(ref orig_key) = substate.role_edit_original_key {
                                let mut new_roles = indexmap::IndexMap::new();
                                for (k, v) in parsed_config.roles.iter() {
                                    if k == orig_key {
                                        new_roles.insert(new_key.clone(), new_role.clone());
                                    } else {
                                        new_roles.insert(k.clone(), v.clone());
                                    }
                                }
                                new_config.roles = new_roles;
                            } else {
                                new_config.roles.insert(new_key, new_role);
                            }

                            config_changed = true;
                            substate.is_editing_role = false;
                        }
                        if ui.button("✖ Cancel").clicked() {
                            substate.is_editing_role = false;
                        }
                    });
                });
            } else {
                for (name, role_cfg) in &parsed_config.roles {
                    let role = &role_cfg.0;
                    ui.group(|ui| {
                        if !role_cfg.1.trim().is_empty() {
                            ui.label(egui::RichText::new(format!("# {}", role_cfg.1.trim())).weak());
                        }
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new(name).strong());
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui.button("🗑").on_hover_text("Delete Role").clicked() {
                                    new_config.roles.shift_remove(name);
                                    config_changed = true;
                                }
                                if ui.button("✏").on_hover_text("Edit Role").clicked() {
                                    substate.is_editing_role = true;
                                    substate.role_edit_original_key = Some(name.clone());
                                    substate.role_edit_key = name.clone();
                                    substate.role_edit_comment = role_cfg.1.trim().to_string();
                                    substate.role_edit_tier = role.tier.0.to_string();
                                    substate.role_edit_boss = role.boss.clone().unwrap_or_default();
                                    substate.role_edit_bin = role.bin.join("\n");
                                    substate.role_edit_env = role.env.join("\n");
                                    substate.role_edit_powers = role.powers.clone();
                                }
                            });
                        });
                        ui.horizontal(|ui| {
                            ui.label("Tier:");
                            ui.label(role.tier.0.to_string());
                        });
                        if let Some(boss) = &role.boss {
                            ui.horizontal(|ui| {
                                ui.label("Boss:");
                                ui.label(boss);
                            });
                        }
                        if !role.powers.is_empty() {
                            ui.label(format!("Powers: {} defined", role.powers.len()));
                        }
                        if !role.bin.is_empty() {
                            ui.label(egui::RichText::new(format!("Includes {} extra binaries.", role.bin.len())).weak().small());
                        }
                    });
                }
                if ui.button("+ Add Role").clicked() {
                    substate.is_editing_role = true;
                    substate.role_edit_original_key = None;
                    substate.role_edit_key = "new_role".to_string();
                    substate.role_edit_comment = "".to_string();
                    substate.role_edit_tier = "2".to_string();
                    substate.role_edit_boss = "".to_string();
                    substate.role_edit_bin = "".to_string();
                    substate.role_edit_env = "".to_string();
                    substate.role_edit_powers = vec![];
                }
            }
        });
    ui.add_space(10.0);

    egui::CollapsingHeader::new(format!("📦 Sandboxes ({})", parsed_config.sandboxes.len()))
        .show(ui, |ui| {
            if substate.is_editing_sandbox {
                ui.group(|ui| {
                    ui.heading(if substate.sandbox_edit_original_key.is_some() { "Edit Sandbox" } else { "Add Sandbox" });

                    ui.horizontal(|ui| {
                        ui.label("Name:");
                        ui.text_edit_singleline(&mut substate.sandbox_edit_key);
                    });

                    ui.horizontal(|ui| {
                        ui.label("Comment:");
                        ui.text_edit_singleline(&mut substate.sandbox_edit_comment);
                    });

                    ui.horizontal(|ui| {
                        ui.label("Path:");
                        ui.text_edit_singleline(&mut substate.sandbox_edit_path);
                    });
                    ui.label(egui::RichText::new("Must be an absolute path ending in .rno").weak().small());

                    ui.add_space(5.0);
                    ui.label(egui::RichText::new("Authorized Roles:").strong());
                    ui.indent("sandbox_roles_indent", |ui| {
                        for role_name in parsed_config.roles.keys() {
                            let mut is_authorized = substate.sandbox_edit_roles.contains(role_name);
                            if ui.checkbox(&mut is_authorized, role_name).changed() {
                                if is_authorized {
                                    substate.sandbox_edit_roles.push(role_name.clone());
                                } else {
                                    substate.sandbox_edit_roles.retain(|r| r != role_name);
                                }
                            }
                        }

                        // Display a warning if a role is authorized but missing from the Realm configuration
                        let mut to_remove_role = None;
                        for (i, role_name) in substate.sandbox_edit_roles.iter().enumerate() {
                            if !parsed_config.roles.contains_key(role_name) {
                                ui.horizontal(|ui| {
                                    ui.label(egui::RichText::new(format!("{} (Unknown Role)", role_name)).color(ui.visuals().error_fg_color));
                                    if ui.button("✖").clicked() {
                                        to_remove_role = Some(i);
                                    }
                                });
                            }
                        }
                        if let Some(i) = to_remove_role {
                            substate.sandbox_edit_roles.remove(i);
                        }
                    });

                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        let path_valid = std::path::Path::new(&substate.sandbox_edit_path).is_absolute() 
                                      && substate.sandbox_edit_path.ends_with(".rno");
                        let roles_valid = !substate.sandbox_edit_roles.is_empty();
                        let key_valid = !substate.sandbox_edit_key.trim().is_empty();
                        
                        let can_apply = key_valid && path_valid && roles_valid;

                        if ui.add_enabled(can_apply, egui::Button::new("✔ Apply"))
                            .on_disabled_hover_text("Name required. Path must be absolute .rno. At least 1 role must be authorized.")
                            .clicked() {
                                
                            let new_key = substate.sandbox_edit_key.trim().to_string();
                            let new_sandbox = serde_saphyr::Commented(
                                inforno_core::realm::SandboxRef {
                                    path: std::path::PathBuf::from(substate.sandbox_edit_path.trim()),
                                    roles: substate.sandbox_edit_roles.clone(),
                                },
                                substate.sandbox_edit_comment.clone()
                            );

                            if let Some(ref orig_key) = substate.sandbox_edit_original_key {
                                let mut new_sandboxes = indexmap::IndexMap::new();
                                for (k, v) in parsed_config.sandboxes.iter() {
                                    if k == orig_key {
                                        new_sandboxes.insert(new_key.clone(), new_sandbox.clone());
                                    } else {
                                        new_sandboxes.insert(k.clone(), v.clone());
                                    }
                                }
                                new_config.sandboxes = new_sandboxes;
                            } else {
                                new_config.sandboxes.insert(new_key, new_sandbox);
                            }

                            config_changed = true;
                            substate.is_editing_sandbox = false;
                        }

                        if ui.button("✖ Cancel").clicked() {
                            substate.is_editing_sandbox = false;
                        }
                    });
                });
            } else {
                for (name, sandbox_cfg) in &parsed_config.sandboxes {
                    let sandbox = &sandbox_cfg.0;
                    ui.group(|ui| {
                        if !sandbox_cfg.1.trim().is_empty() {
                            ui.label(egui::RichText::new(format!("# {}", sandbox_cfg.1.trim())).weak());
                        }
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new(name).strong());
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui.button("🗑").on_hover_text("Delete Sandbox").clicked() {
                                    new_config.sandboxes.shift_remove(name);
                                    config_changed = true;
                                }
                                if ui.button("✏").on_hover_text("Edit Sandbox").clicked() {
                                    substate.is_editing_sandbox = true;
                                    substate.sandbox_edit_original_key = Some(name.clone());
                                    substate.sandbox_edit_key = name.clone();
                                    substate.sandbox_edit_comment = sandbox_cfg.1.trim().to_string();
                                    substate.sandbox_edit_path = sandbox.path.display().to_string();
                                    substate.sandbox_edit_roles = sandbox.roles.clone();
                                }
                            });
                        });
                        ui.label(format!("Path: {}", sandbox.path.display()));
                        if !sandbox.roles.is_empty() {
                            ui.label(format!("Roles: {}", sandbox.roles.join(", ")));
                        }
                    });
                }
                if ui.button("+ Add Sandbox").clicked() {
                    substate.is_editing_sandbox = true;
                    substate.sandbox_edit_original_key = None;
                    substate.sandbox_edit_key = "new_sandbox".to_string();
                    substate.sandbox_edit_comment = "".to_string();
                    substate.sandbox_edit_path = "/path/to/sandbox.rno".to_string();
                    substate.sandbox_edit_roles = vec![];
                }
            }
        });

    // If the visual builder produced changes, serialize them automatically back to the right-side text editor.
    // CommentPosition::Above (rather than the default Inline) is required here: Inline silently
    // drops comments attached to non-scalar values (mounts/roles/tiers/sandboxes are all
    // structs), so without this every comment on those would vanish the moment the Visual
    // Builder touches anything. This also changes how `places` comments render -- as a line
    // above the key instead of trailing on the same line -- rather than reflowing per field.
    if config_changed {
        let opts = serde_saphyr::ser_options! { comment_position: serde_saphyr::CommentPosition::Above };
        match serde_saphyr::to_string_with_options(&new_config, opts) {
            Ok(yaml) => {
                let mut lines: Vec<String> = yaml.lines().map(String::from).collect();

                // Pass 1: Handle Inner Struct Comments (Mounts, Roles, Tiers)
                let mut i = 0;
                while i < lines.len() {
                    let current_line = &lines[i];
                    if current_line.trim_end().ends_with(':') {
                        let key_indent = current_line.len() - current_line.trim_start().len();
                        let mut comment_block = Vec::new();
                        let mut j = i + 1;

                        while j < lines.len() {
                            let next_line = &lines[j];
                            let trimmed = next_line.trim_start();
                            if trimmed.starts_with('#') {
                                let indent = next_line.len() - trimmed.len();
                                if indent > key_indent {
                                    comment_block.push(trimmed.to_string());
                                    j += 1;
                                    continue;
                                }
                            }
                            break;
                        }

                        let num_comments = comment_block.len();
                        if num_comments > 0 {
                            // Remove the comments from inside the struct block
                            for _ in 0..num_comments {
                                lines.remove(i + 1);
                            }

                            if num_comments == 1 {
                                // SINGLE-LINE: Inline it! Append directly to the parent key.
                                let comment = &comment_block[0];
                                lines[i] = format!("{} {}", lines[i], comment);
                            } else {
                                // MULTI-LINE: Hoist it safely ABOVE the parent key.
                                let indent_str = " ".repeat(key_indent);
                                for (idx, comment) in comment_block.into_iter().enumerate() {
                                    lines.insert(i + idx, format!("{}{}", indent_str, comment));
                                }
                                i += num_comments;
                            }
                        }
                    }
                    i += 1;
                }

                // Pass 2: Handle Above-Scalar Comments (Places)
                let mut i = 0;
                while i < lines.len() {
                    let line = &lines[i];
                    let trimmed = line.trim_start();

                    if trimmed.starts_with('#') {
                        let indent = line.len() - trimmed.len();

                        if i + 1 < lines.len() {
                            let next_line = &lines[i + 1];
                            let next_trimmed = next_line.trim_start();
                            let next_indent = next_line.len() - next_trimmed.len();

                            // Check if this is an isolated, single-line comment
                            let is_single_comment = if i > 0 {
                                let prev_line = &lines[i - 1];
                                let prev_trimmed = prev_line.trim_start();
                                let prev_indent = prev_line.len() - prev_trimmed.len();
                                !(prev_trimmed.starts_with('#') && prev_indent == indent)
                            } else {
                                true
                            };

                            // If it's a single comment, and the next line is a scalar (key: value), inline it!
                            if is_single_comment
                                && next_indent == indent
                                && !next_trimmed.starts_with('#')
                                && next_trimmed.contains(':')
                                && !next_trimmed.trim_end().ends_with(':')
                            {
                                let comment_text = trimmed.to_string();
                                let base_line = next_line.to_string();

                                lines[i] = format!("{} {}", base_line, comment_text);
                                lines.remove(i + 1);
                                // Don't advance `i` so we can re-evaluate the merged line (won't match '#' anyway)
                            }
                        }
                    }
                    i += 1;
                }

                // Pass 3: Collapse dangling list dashes (artifact from commented structs in lists)
                let mut i = 0;
                while i < lines.len() {
                    let trimmed = lines[i].trim();
                    if trimmed == "-" {
                        if i + 1 < lines.len() {
                            let next_trimmed = lines[i + 1].trim_start();
                            // Only collapse if the next line is a key/value, not a multi-line comment block
                            if !next_trimmed.starts_with('#') && !next_trimmed.is_empty() {
                                let next_line_owned = lines.remove(i + 1);
                                let indent = lines[i].len() - lines[i].trim_start().len();
                                let indent_str = " ".repeat(indent);
                                lines[i] = format!("{}- {}", indent_str, next_line_owned.trim_start());
                            }
                        }
                    }
                    i += 1;
                }

                // Pass 4: Collapse short arrays (like caps: and sandbox roles:) into inline flow style
                let mut i = 0;
                while i < lines.len() {
                    let trimmed = lines[i].trim();
                    // Target `caps:` and any indented `roles:` (to avoid collapsing the top-level roles: block)
                    if trimmed == "caps:" || (trimmed == "roles:" && lines[i].starts_with(' ')) {
                        let base_indent = lines[i].len() - lines[i].trim_start().len();
                        let mut list_items = Vec::new();
                        let mut j = i + 1;
                        while j < lines.len() {
                            let next_line = &lines[j];
                            let next_trimmed = next_line.trim_start();
                            let next_indent = next_line.len() - next_trimmed.len();

                            // Only consume items if their indent is >= the parent key, preventing us from eating sibling blocks
                            if next_indent >= base_indent && next_trimmed.starts_with("- ") {
                                list_items.push(next_trimmed[2..].trim().to_string());
                                j += 1;
                            } else {
                                break;
                            }
                        }
                        if !list_items.is_empty() {
                            let inline_array = format!("[{}]", list_items.join(", "));
                            lines[i] = format!("{} {}", lines[i], inline_array);
                            for _ in 0..list_items.len() {
                                lines.remove(i + 1);
                            }
                        }
                    }
                    i += 1;
                }

                // Pass 5: Insert empty lines between major top-level sections
                let top_level_keys = ["mounts:", "places:", "spans:", "tiers:", "roles:", "sandboxes:", "bin:", "env:"];
                let mut i = 0;
                while i < lines.len() {
                    if top_level_keys.contains(&lines[i].as_str()) {
                        if i > 0 && !lines[i - 1].trim().is_empty() {
                            lines.insert(i, String::new());
                            i += 1; // Skip the newly inserted line
                        }
                    }
                    i += 1;
                }

                let mut final_yaml = lines.join("\n");
                if yaml.ends_with('\n') {
                    final_yaml.push('\n');
                }

                substate.yaml_buffer = final_yaml;
                substate.parse_error = None; // Clear any existing typing errors
                substate.cached_config = Some(new_config);
            }
            Err(e) => {
                substate.parse_error = Some(format!("Visual Builder Serialization Error: {}", e));
            }
        }
    }
}

fn ui_new_realm_wizard(ctx: &egui::Context, state: &mut State) {
    let mut is_open = state.realm_config_state.show_new_realm_wizard;
    egui::Window::new("✨ New Realm")
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .open(&mut is_open)
        .show(ctx, |ui| {
            if state.realm_config_state.wizard_step == 0 {
                ui.heading("Step 1: Project Type");
                ui.label("What type of project is this?");
                ui.label(egui::RichText::new("Don't worry, the realm can be modified at any time later.").weak());
                ui.add_space(10.0);

                ui.radio_value(&mut state.realm_config_state.wizard_project_type, 1, "1. Brainstorming or Preliminary research");
                ui.radio_value(&mut state.realm_config_state.wizard_project_type, 2, "2. Software Development");
                ui.radio_value(&mut state.realm_config_state.wizard_project_type, 3, "3. Content Generation");
                ui.radio_value(&mut state.realm_config_state.wizard_project_type, 4, "4. Other");

                ui.add_space(15.0);
                ui.horizontal(|ui| {
                    if ui.add_enabled(state.realm_config_state.wizard_project_type != 0, egui::Button::new("Next ➡")).clicked() {
                        state.realm_config_state.wizard_step = 1;
                    }
                    if ui.button("Cancel").clicked() {
                        state.realm_config_state.show_new_realm_wizard = false;
                    }
                });
            } else if state.realm_config_state.wizard_step == 1 {
                ui.heading("Step 2: Realm Configuration");
                ui.add_space(10.0);

                ui.horizontal(|ui| {
                    ui.label("Realm Name (lower_case_no_spaces):");
                    ui.text_edit_singleline(&mut state.realm_config_state.wizard_realm_name);
                });

                ui.add_space(10.0);
                ui.label("How would you like to initialize the sandbox?");
                ui.radio_value(&mut state.realm_config_state.wizard_sandbox_option, 0, "Copy current sandbox into the new realm");
                ui.radio_value(&mut state.realm_config_state.wizard_sandbox_option, 1, "Create a new empty sandbox (keep presets)");
                ui.radio_value(&mut state.realm_config_state.wizard_sandbox_option, 2, "Create a totally empty sandbox");

                ui.add_space(15.0);
                ui.horizontal(|ui| {
                    if ui.button("⬅ Back").clicked() {
                        state.realm_config_state.wizard_step = 0;
                    }
                    let can_finish = !state.realm_config_state.wizard_realm_name.trim().is_empty();
                    if ui.add_enabled(can_finish, egui::Button::new("✔ Create Realm")).clicked() {
                        let realm_name = state.realm_config_state.wizard_realm_name.trim().to_string();
                        if let Some(proj_dirs) = directories::ProjectDirs::from("", "", "inforno") {
                            let realm_dir = proj_dirs.config_dir().join("realms").join(&realm_name);
                            let _ = std::fs::create_dir_all(&realm_dir);

                            let sandbox_dir = proj_dirs.data_dir().join("sandboxes");
                            let _ = std::fs::create_dir_all(&sandbox_dir);
                            let default_sandbox_path = sandbox_dir.join(format!("{}.rno", realm_name));
                            let path_str = default_sandbox_path.to_string_lossy().replace('\\', "/");
                            
                            // TODO: Handle actual SQLite sandbox file copying/creation logic here 
                            // based on state.realm_config_state.wizard_sandbox_option

                            let minimal_yaml = format!(r#"mounts: {{}}
places: {{}}
tiers: {{}}
roles:
  gui:
    tier: 1
sandboxes:
  default:
    path: {}
    roles:
      - gui
"#, path_str);

                            let yaml_path = realm_dir.join("realm2.yml");
                            let _ = std::fs::write(&yaml_path, minimal_yaml.clone());

                            // Set up the state BEFORE reload
                            if let Ok(mut lock) = state.perma.active_realm_name.lock() {
                                *lock = Some(realm_name.clone());
                            }

                            state.reload(Some(default_sandbox_path.clone()));

                            state.realm_config_state.yaml_buffer = minimal_yaml.clone();
                            state.realm_config_state.original_yaml = minimal_yaml.clone();
                            state.realm_config_state.cached_config = None;
                            state.realm_config_state.realm_name = Some(realm_name);
                            
                            state.show_realm_config = true;
                            state.realm_config_state.show_new_realm_wizard = false;
                        }
                    }
                });
            }
        });

    if !is_open {
        state.realm_config_state.show_new_realm_wizard = false;
    }
}

fn ui_edit_glob_expr(
    ui: &mut egui::Ui,
    expr: &mut inforno_core::realm::GlobExpr,
    available_refs: &[String],
    id_salt: usize
) {
    use inforno_core::realm::GlobExpr;

    ui.push_id(id_salt, |ui| {
        egui::Frame::default()
            .inner_margin(6.0)
            .stroke(egui::Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color))
            .corner_radius(4.0)
            .show(ui, |ui| {
                // Force a vertical layout so nested recursive calls don't inherit horizontal layouts
                ui.vertical(|ui| {
                    ui.horizontal(|ui| {
                        let current_variant = match expr {
                        GlobExpr::Pattern(s) if s.starts_with('@') => "Ref",
                        GlobExpr::Pattern(_) => "Match",
                        GlobExpr::List(_) | GlobExpr::Any { .. } => "Any",
                        GlobExpr::All { .. } => "All",
                        GlobExpr::Not { .. } => "Not",
                    };

                    let label = match current_variant {
                        "Match" => "📄 Glob Match",
                        "Any" => "🔀 Any (OR)",
                        "All" => "🔗 All (AND)",
                        "Not" => "🚫 Not",
                        "Ref" => "🔖 Reference",
                        _ => "",
                    };

                    egui::ComboBox::from_id_salt("variant_combo").selected_text(label).show_ui(ui, |ui| {
                        if ui.selectable_label(current_variant == "Match", "📄 Glob Match").clicked() && current_variant != "Match" {
                            *expr = GlobExpr::Pattern("**/*".to_string());
                        }
                        if ui.selectable_label(current_variant == "Any", "🔀 Any (OR)").clicked() && current_variant != "Any" {
                            *expr = GlobExpr::List(vec![]);
                        }
                        if ui.selectable_label(current_variant == "All", "🔗 All (AND)").clicked() && current_variant != "All" {
                            *expr = GlobExpr::All { all: vec![] };
                        }
                        if ui.selectable_label(current_variant == "Not", "🚫 Not").clicked() && current_variant != "Not" {
                            *expr = GlobExpr::Not { not: Box::new(GlobExpr::Pattern("**/*".to_string())) };
                        }
                        if ui.selectable_label(current_variant == "Ref", "🔖 Reference").clicked() && current_variant != "Ref" {
                            *expr = GlobExpr::Pattern("@".to_string());
                        }
                    });
                });

                ui.indent("expr_indent", |ui| {
                    match expr {
                        GlobExpr::Pattern(s) => {
                            if s.starts_with('@') {
                                let mut current_ref = s.strip_prefix('@').unwrap_or("").to_string();
                                egui::ComboBox::from_id_salt("ref_combo")
                                    .selected_text(if current_ref.is_empty() { "Select a span..." } else { current_ref.as_str() })
                                    .show_ui(ui, |ui| {
                                        for r in available_refs {
                                            if ui.selectable_value(&mut current_ref, r.clone(), r).clicked() {
                                                *s = format!("@{}", current_ref);
                                            }
                                        }
                                    });
                            } else {
                                ui.horizontal(|ui| {
                                    ui.text_edit_singleline(s);
                                });
                            }
                        }
                        GlobExpr::List(any) | GlobExpr::Any { any } | GlobExpr::All { all: any } => {
                            let mut to_remove = None;
                            for (i, e) in any.iter_mut().enumerate() {
                                ui.horizontal_top(|ui| {
                                    if ui.button("✖").clicked() {
                                        to_remove = Some(i);
                                    }
                                    ui_edit_glob_expr(ui, e, available_refs, i);
                                });
                            }
                            if let Some(i) = to_remove {
                                any.remove(i);
                            }
                            if ui.button("+ Add Condition").clicked() {
                                any.push(GlobExpr::Pattern("**/*".to_string()));
                            }
                        }
                        GlobExpr::Not { not } => {
                            ui_edit_glob_expr(ui, not, available_refs, 0);
                        }
                    }
                });
            });
        });
    });
}

fn render_vfs_tree(ui: &mut egui::Ui, realm: &inforno_core::realm::ActiveRealm, role: &str) {
    for mount in &realm.mounts {
        let v_path = std::path::PathBuf::from(&mount.virtual_path);
        render_vfs_node(ui, realm, &mount.host_path, &v_path, role, true);
    }
}

fn render_vfs_node(
    ui: &mut egui::Ui,
    realm: &inforno_core::realm::ActiveRealm,
    host_path: &std::path::Path,
    virtual_path: &std::path::Path,
    role: &str,
    is_root: bool
) {
    // 1. FUSE Visibility Gate: If the Realm hides it (e.g. dotfiles), it doesn't exist to `ls`.
    if realm.is_path_hidden(virtual_path, role) {
        return;
    }

    let is_dir = host_path.is_dir();

    // 2. FUSE Access Check: Can they modify it?
    // Directories themselves are governed by the mount's RO flag. FUSE evaluates
    // glob-based Create/Write/Unlink rules against the specific *child* file path.
    let mut can_read = true;
    let mut can_write = true;
    let mut can_append = true;

    if is_dir {
        if realm.is_path_read_only(virtual_path) {
            can_write = false;
            can_append = false;
        }
    } else {
        can_read = realm.can_access(virtual_path, inforno_core::realm::Cap::Read, role).is_ok();
        can_write = realm.can_access(virtual_path, inforno_core::realm::Cap::Write, role).is_ok();
        can_append = realm.can_access(virtual_path, inforno_core::realm::Cap::Append, role).is_ok();
    }

    let icon = if can_write {
        ""
    } else if can_append {
        " 📝 (Append Only)"
    } else if can_read {
        " 🔒 (Read Only)"
    } else {
        " 🚫 (No Access)"
    };

    let name = if is_root {
        format!("🗄 {} (→ {}){}", virtual_path.display(), host_path.display(), icon)
    } else {
        format!("{}{}", host_path.file_name().unwrap_or_default().to_string_lossy(), icon)
    };

    let text_color = if !can_read {
        ui.visuals().error_fg_color // Red for unreadable files
    } else if can_write {
        ui.visuals().text_color() // Normal for RW
    } else {
        ui.visuals().weak_text_color() // Dimmed for RO/Append
    };

    if host_path.is_dir() {
        let label = egui::RichText::new(format!("{} {}", if is_root {""} else {"📁"}, name)).color(text_color);

        egui::CollapsingHeader::new(label)
            .id_salt(virtual_path) // Guarantee unique ID
            .default_open(is_root) // Auto-open the mount roots
            .show(ui, |ui| {
                // Egui's lazyness shines here: this code only runs if the header is EXPANDED!
                if let Ok(entries) = std::fs::read_dir(host_path) {
                    let mut paths: Vec<_> = entries.filter_map(|e| e.ok()).collect();
                    // Sort directories first, then alphabetically
                    paths.sort_by_key(|e| {
                        let is_d = e.path().is_dir();
                        (!is_d, e.file_name())
                    });

                    if paths.is_empty() {
                        ui.label(egui::RichText::new("(empty)").weak().italics());
                    } else {
                        for entry in paths {
                            render_vfs_node(
                                ui,
                                realm,
                                &entry.path(),
                                &virtual_path.join(entry.file_name()),
                                role,
                                false
                            );
                        }
                    }
                } else {
                    ui.label(egui::RichText::new("Failed to read directory from disk")
                        .color(ui.visuals().error_fg_color));
                }
            });
    } else {
        ui.label(egui::RichText::new(format!("📄 {}", name)).color(text_color));
    }
}
