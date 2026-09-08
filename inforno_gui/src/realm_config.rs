use egui::{Color32, RichText, ScrollArea};
use crate::state::State;
use bulat::editor::{CodeEditor, Syntax, ColorTheme};

#[derive(Default)]
pub struct RealmConfigState {
    // The raw text being edited on the right side
    pub yaml_buffer: String,
    // If the user made a typo in the YAML, we store the error here
    pub parse_error: Option<String>,
    // Tracks if changes in the form need to be serialized back to the text buffer
    pub sync_to_yaml_needed: bool,
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
                    ui.heading("realm2.yml");
                    ui.separator();

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
                        match serde_yaml::from_str::<inforno_core::realm::RealmConfig>(&substate.yaml_buffer) {
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

    state.show_realm_config = is_open;
}

// --- Helper Functions ---

fn render_form_column(ui: &mut egui::Ui, state: &mut State) {
    let Some(realm) = &state.active_realm else {
        ui.label(egui::RichText::new("No active realm loaded to configure.").weak().italics());
        return;
    };

    let config = &realm.raw_config;

    egui::CollapsingHeader::new(format!("🗄 Mounts ({})", config.mounts.len()))
        .default_open(true)
        .show(ui, |ui| {
            for (name, mount) in &config.mounts {
                ui.group(|ui| {
                    ui.label(egui::RichText::new(name).strong());
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
            ui.button("+ Add Mount");
        });

    ui.add_space(10.0);

    egui::CollapsingHeader::new(format!("🔖 Places ({})", config.places.len()))
        .show(ui, |ui| {
            for (name, path) in &config.places {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(name).strong());
                    ui.label("->");
                    ui.label(path);
                });
            }
            ui.button("+ Add Place");
        });

    ui.add_space(10.0);

    egui::CollapsingHeader::new(format!("🎯 Spans ({})", config.expressions.len()))
        .show(ui, |ui| {
            for (name, _expr) in &config.expressions {
                ui.label(egui::RichText::new(name).strong());
            }
            ui.button("+ Add Span");
        });

    ui.add_space(10.0);

    egui::CollapsingHeader::new(format!("📶 Tiers ({})", config.tiers.len()))
        .show(ui, |ui| {
            for (tier_num, tier_cfg) in &config.tiers {
                ui.group(|ui| {
                    ui.label(egui::RichText::new(format!("Tier {}", tier_num)).strong());
                    ui.label(format!("Powers: {} defined", tier_cfg.powers.len()));
                });
            }
            ui.button("+ Add Tier");
        });

    ui.add_space(10.0);

    egui::CollapsingHeader::new(format!("🎭 Roles ({})", config.roles.len()))
        .show(ui, |ui| {
            for (name, role) in &config.roles {
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

    egui::CollapsingHeader::new(format!("📦 Sandboxes ({})", config.sandboxes.len()))
        .show(ui, |ui| {
            for (name, sandbox) in &config.sandboxes {
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
