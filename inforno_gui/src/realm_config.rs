use egui::{Color32, RichText, ScrollArea};
use crate::state::State;

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
                    ui.heading("realm.yml");
                    ui.separator();

                    let substate = &mut state.realm_config_state;

                    // 1. Live YAML Editor
                    ScrollArea::vertical().id_salt("realm_yaml_scroll").max_height(350.0).show(ui, |ui| {
                        let response = ui.add_sized(
                            ui.available_size(),
                            egui::TextEdit::multiline(&mut substate.yaml_buffer)
                                .font(egui::TextStyle::Monospace) // Looks like code
                                .code_editor() // Turns off word wrapping, adds line numbers if configured
                        );

                        // If user types in the right pane, we try to parse it
                        if response.changed() {
                            match serde_yaml::from_str::<inforno_core::common::RealmConfig>(&substate.yaml_buffer) {
                                Ok(new_config) => {
                                    substate.parse_error = None;
                                    // Optionally: sync `new_config` back to the live Form variables here
                                },
                                Err(e) => {
                                    substate.parse_error = Some(e.to_string());
                                }
                            }
                        }
                    });

                    if let Some(err) = &substate.parse_error {
                        ui.colored_label(Color32::RED, format!("YAML Error: {}", err));
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
    // Here you build the UI for your mounts, globs, roles, and actors.
    // Example:
    egui::CollapsingHeader::new("🗄 Mounts")
        .default_open(true)
        .show(ui, |ui| {
            // Loop through state.active_realm.config.mounts to render editable text fields
            ui.label("Mount form elements go here...");
            ui.button("+ Add Mount");
        });

    ui.add_space(10.0);

    egui::CollapsingHeader::new("🔍 Glob Selections")
        .default_open(true)
        .show(ui, |ui| {
             ui.label("Glob form elements go here...");
        });

    ui.add_space(10.0);
    // ... Roles and Actors
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