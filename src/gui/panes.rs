use egui_tiles::{Behavior, TileId, UiResponse};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// This enum represents the data we save to the .ron file.
/// It is lightweight and only contains the instructions on WHAT to render.
#[derive(Clone, Serialize, Deserialize, PartialEq)]
pub enum Pane {
    Chat,
    Editor { path: PathBuf },
    // We can easily add MergeTool here later!
}

/// This is the bridge between egui_tiles and your application state.
pub struct PaneBehavior<'a> {
    pub state: &'a mut crate::gui::State,
    // NEW: A queue to hold our split requests until the UI is done drawing
    pub split_requests: Vec<Pane>,
}

impl<'a> Behavior<Pane> for PaneBehavior<'a> {
    // 1. Define the title of the tab
    fn tab_title_for_pane(&mut self, pane: &Pane) -> egui::WidgetText {
        match pane {
            Pane::Chat => "💬 Chat".into(),
            Pane::Editor { path } => {
                let filename = path.file_name().unwrap_or_default().to_string_lossy();
                format!("📝 {}", filename).into()
            }
        }
    }

    // 2. Render the actual content of the pane
    fn pane_ui(
        &mut self,
        ui: &mut egui::Ui,
        _tile_id: TileId,
        pane: &mut Pane,
    ) -> UiResponse {
        match pane {
            Pane::Chat => {
                /* A top header with the Split button
                ui.horizontal(|ui| {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("◫ Split View").on_hover_text("Open another chat side-by-side").clicked() {
                            // Push a request to duplicate this chat pane
                            self.split_requests.push(Pane::Chat);
                        }
                    });
                });
                ui.separator();
                */

                // Your existing chat rendering
                egui::ScrollArea::vertical()
                    .stick_to_bottom(true)
                    .id_salt("chat_scroll_main")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        crate::gui::chat::render_chat_messages(
                            ui,
                            self.state,
                            ui.available_width(),
                        );
                    });
            }
            Pane::Editor { path } => {
                // We'll give the placeholder editor the same split functionality
                ui.horizontal(|ui| {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("◫ Split View").clicked() {
                            self.split_requests.push(Pane::Editor { path: path.clone() });
                        }
                    });
                });
                ui.separator();

                ui.centered_and_justified(|ui| {
                    ui.heading(format!("Text Editor for: {}", path.display()));
                });
            }
        }

// Tells egui_tiles we are holding onto the pane normally
        UiResponse::None
    }

    fn simplification_options(&self) -> egui_tiles::SimplificationOptions {
        let mut opts = egui_tiles::SimplificationOptions::default();
        opts.all_panes_must_have_tabs = true;
        opts
    }
}
