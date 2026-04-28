use egui::Ui;
use std::fs;
use rust_i18n::t;
use crate::gui::State;

pub fn ui_right_panel(ui: &mut egui::Ui, state: &mut State) {
    let ctx = ui.ctx().clone();
    let mut close_merge = false;
    let mut save_merge = false;

    if let Some(active_merge) = &mut state.active_merge {
        egui::Panel::right("merge_tool_panel")
            .resizable(true)
            .default_width(ctx.viewport_rect().width() * 0.5)
            .width_range(200.0..=ctx.viewport_rect().width() * 0.9)
            .show_inside(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.heading(format!("Merging {}", active_merge.path.display()));

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("❌ Close").clicked() {
                            close_merge = true;
                        }

                        // --- NEW: Save Button ---
                        if ui.button("💾 Save").on_hover_text("Save merged changes to disk").clicked() {
                            save_merge = true;
                        }
                    });
                });
                ui.separator();

                // Render the diff tool
                active_merge.app.show(ui);
            });

        // --- Execute Save ---
        if save_merge {
            match fs::write(&active_merge.path, &active_merge.app.left_code_real) {
                Ok(_) => {
                    println!("Successfully saved to {}", active_merge.path.display());
                    // Optional: Show a temporary success message in the UI or leave as console log
                }
                Err(e) => {
                    state.error_msg = Some(format!("Failed to save merged file: {}", e));
                    state.is_modal_open = true;
                }
            }
        }
    }

    if close_merge {
        state.active_merge = None;
    }
}