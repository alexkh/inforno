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
    pub mount_edit_has_intro: bool,
    pub mount_edit_intro: String,

    // --- Visual Builder: Place Edit State ---
    pub is_editing_place: bool,
    pub place_edit_original_key: Option<String>, // None means "Adding new"
    pub place_edit_key: String,
    pub place_edit_path: String,
    pub place_edit_comment: String,

    // Indicates the app booted with a broken YAML file and is offering a rescue
    pub is_fixing_broken_realm: bool,
    // Tracks if a successful save occurred, requiring a reload when the window closes
    pub needs_reload: bool,
    // Explicitly tracked realm name to guarantee saving works reliably
    pub realm_name: Option<String>,
}

pub fn ui_realm_config(ctx: &egui::Context, state: &mut State) {
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
                            if ui.add_enabled(can_save, egui::Button::new("💾 Save")).clicked() {
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
                                        
                                        if state.realm_config_state.is_fixing_broken_realm {
                                            // Instant rescue reload!
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
                                            }
                                        } else {
                                            state.realm_config_state.needs_reload = true;
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
                            Ok(_new_config) => {
                                substate.parse_error = None;
                                // Optionally: sync `new_config` back to the live Form variables here
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
                    ScrollArea::vertical().id_salt("vfs_tree_scroll").show(ui, |ui| {
                        if let Some(realm) = &state.active_realm {
                            render_vfs_tree(ui, realm);
                        } else {
                            ui.label("No active realm to display.");
                        }
                    });
                });
            });
        });

    // Intercept window close event to apply and reload
    if state.show_realm_config && !is_open {
        let mut did_save = false;

        // If the user closes the window with valid, unsaved changes, automatically save them
        if state.realm_config_state.yaml_buffer.trim() != state.realm_config_state.original_yaml.trim() 
            && state.realm_config_state.parse_error.is_none() 
        {
            let realm_name_opt = state.active_realm.as_ref().map(|r| r.name.clone())
                .or_else(|| state.perma.active_realm_name.lock().unwrap().clone());

            if let Some(realm_name) = realm_name_opt {
                if let Some(proj_dirs) = directories::ProjectDirs::from("", "", "inforno") {
                    let realm_dir = proj_dirs.config_dir().join("realms").join(&realm_name);
                    let yaml_path = realm_dir.join("realm2.yml");
                    if std::fs::write(&yaml_path, &state.realm_config_state.yaml_buffer).is_ok() {
                        did_save = true;
                    }
                }
            }
        }

        // If we saved now, or if they clicked the Save button previously, trigger a full reload
        if did_save || state.realm_config_state.needs_reload {
            state.realm_config_state.needs_reload = false;
            
            let mut target_sandbox = state.sandbox.clone();
            
            // If we are recovering from a broken realm on boot, `main.rs` aborted before populating `perma`.
            // We must explicitly inject it now so the hot-reload knows which Realm to compile!
            if state.realm_config_state.is_fixing_broken_realm {
                if let Some(r_name) = &state.realm_config_state.realm_name {
                    if let Ok(mut lock) = state.perma.active_realm_name.lock() {
                        *lock = Some(r_name.clone());
                    }
                    
                    // Attempt to resolve the sandbox so we don't fall back to the home sandbox
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
            // Cancelled out without fixing
            state.realm_config_state.is_fixing_broken_realm = false;
        }
    }

    state.show_realm_config = is_open;
}

// --- Helper Functions ---

fn render_form_column(ui: &mut egui::Ui, state: &mut State) {
    let substate = &mut state.realm_config_state;

    // Drive the form directly from the live YAML editor, allowing 2-way data binding
    let parsed_config = match serde_saphyr::from_str::<inforno_core::realm::RealmConfig>(&substate.yaml_buffer) {
        Ok(c) => c,
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
    };

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

                    ui.checkbox(&mut substate.mount_edit_has_intro, "Include Intro text");
                    if substate.mount_edit_has_intro {
                        ui.text_edit_multiline(&mut substate.mount_edit_intro);
                    }

                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        if ui.button("✔ Apply").clicked() {
                            let host_path = std::path::PathBuf::from(substate.mount_edit_host.trim());
                            let canonical_host = std::fs::canonicalize(&host_path).unwrap_or(host_path);

                            let new_mount = inforno_core::realm::RealmMountConfig {
                                host: canonical_host,
                                read_only: substate.mount_edit_ro,
                                intro: if substate.mount_edit_has_intro && !substate.mount_edit_intro.trim().is_empty() {
                                    Some(substate.mount_edit_intro.trim().to_string())
                                } else {
                                    None
                                },
                            };

                            let new_key = substate.mount_edit_key.trim().to_string();

                            if let Some(ref orig_key) = substate.mount_edit_original_key {
                                // Rebuild map to preserve insertion order where possible.
                                // Carry over whatever comment was already attached to this
                                // mount (edited via the raw YAML, not exposed as a form
                                // field yet) instead of silently dropping it here.
                                let existing_comment = parsed_config.mounts.get(orig_key)
                                    .map(|c| c.1.clone())
                                    .unwrap_or_default();
                                let mut new_mounts = indexmap::IndexMap::new();
                                for (k, v) in parsed_config.mounts.iter() {
                                    if k == orig_key {
                                        new_mounts.insert(new_key.clone(), serde_saphyr::Commented(new_mount.clone(), existing_comment.clone()));
                                    } else {
                                        new_mounts.insert(k.clone(), v.clone());
                                    }
                                }
                                new_config.mounts = new_mounts;
                            } else {
                                new_config.mounts.insert(new_key, serde_saphyr::Commented(new_mount, String::new()));
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
                        ui.horizontal(|ui| {
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
                                    substate.mount_edit_has_intro = mount.intro.is_some();
                                    substate.mount_edit_intro = mount.intro.clone().unwrap_or_default();
                                }
                            });
                        });
                        ui.horizontal(|ui| {
                            ui.label("Host:");
                            ui.label(mount.host.display().to_string());
                        });
                        ui.horizontal(|ui| {
                            ui.label("Read Only:");
                            ui.label(mount.read_only.to_string());
                        });
                        if let Some(intro) = &mount.intro {
                            ui.label(format!("Intro: {}", intro));
                        }
                    });
                }
                if ui.button("+ Add Mount").clicked() {
                    substate.is_editing_mount = true;
                    substate.mount_edit_original_key = None;
                    substate.mount_edit_key = "/new_mount".to_string();
                    substate.mount_edit_host = "".to_string();
                    substate.mount_edit_ro = false;
                    substate.mount_edit_has_intro = false;
                    substate.mount_edit_intro = "".to_string();
                }
            }
        });

    ui.add_space(10.0);

    egui::CollapsingHeader::new(format!("🔖 Places ({})", parsed_config.places.len()))
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
                                format!(" {}", substate.place_edit_comment.trim())
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
                        ui.horizontal(|ui| {
                            ui.label("->");
                            ui.label(&path.0);
                            if !path.1.is_empty() {
                                ui.label(egui::RichText::new(format!("// {}", path.1.trim())).weak());
                            }
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
            for (name, _expr) in &parsed_config.expressions {
                ui.label(egui::RichText::new(name).strong());
            }
            ui.button("+ Add Span");
        });

    ui.add_space(10.0);

    egui::CollapsingHeader::new(format!("📶 Tiers ({})", parsed_config.tiers.len()))
        .show(ui, |ui| {
            for (tier_num, tier_cfg) in &parsed_config.tiers {
                let tier_cfg = &tier_cfg.0;
                ui.group(|ui| {
                    ui.label(egui::RichText::new(format!("Tier {}", tier_num)).strong());
                    ui.label(format!("Powers: {} defined", tier_cfg.powers.len()));
                });
            }
            ui.button("+ Add Tier");
        });

    ui.add_space(10.0);

    egui::CollapsingHeader::new(format!("🎭 Roles ({})", parsed_config.roles.len()))
        .show(ui, |ui| {
            for (name, role) in &parsed_config.roles {
                let role = &role.0;
                ui.group(|ui| {
                    ui.label(egui::RichText::new(name).strong());
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
                    ui.label(format!("Intro: {}", role.intro));
                    if !role.powers.is_empty() {
                        ui.label(format!("Powers: {} defined", role.powers.len()));
                    }
                });
            }
            ui.button("+ Add Role");
        });

    ui.add_space(10.0);

    egui::CollapsingHeader::new(format!("📦 Sandboxes ({})", parsed_config.sandboxes.len()))
        .show(ui, |ui| {
            for (name, sandbox) in &parsed_config.sandboxes {
                let sandbox = &sandbox.0;
                ui.group(|ui| {
                    ui.label(egui::RichText::new(name).strong());
                    ui.label(format!("Path: {}", sandbox.path.display()));
                    if !sandbox.roles.is_empty() {
                        ui.label(format!("Roles: {}", sandbox.roles.join(", ")));
                    }
                    if let Some(desc) = &sandbox.description {
                        ui.label(format!("Description: {}", desc));
                    }
                });
            }
            ui.button("+ Add Sandbox");
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
                substate.yaml_buffer = yaml;
                substate.parse_error = None; // Clear any existing typing errors
            }
            Err(e) => {
                substate.parse_error = Some(format!("Visual Builder Serialization Error: {}", e));
            }
        }
    }
}

fn render_vfs_tree(ui: &mut egui::Ui, realm: &inforno_core::realm::ActiveRealm) {
    // You would dynamically build this based on `realm.mounts`
    for mount in &realm.mounts {
        egui::CollapsingHeader::new(format!("🗄 {}", mount.virtual_path))
            .default_open(true)
            .show(ui, |ui| {
                // In a real scenario, you could use `walkdir` up to a depth of 1 or 2
                // mapped through your `glob_selections` to show what is accessible.

                // For now, mockup visual representation:
                ui.label(RichText::new("Host Path:").weak());
                ui.label(mount.host_path.display().to_string());
                ui.add_space(5.0);

                egui::CollapsingHeader::new("📁 src")
                    .show(ui, |ui| {
                        ui.label("📄 main.rs");
                        ui.label("📄 lib.rs");
                    });
                ui.label("📄 Cargo.toml");
            });
    }
}
