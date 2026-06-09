use egui_tiles::{Behavior, TileId, UiResponse};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// This enum represents the data we save to the .ron file.
/// It is lightweight and only contains the instructions on WHAT to render.
#[derive(Clone, Serialize, Deserialize, PartialEq)]
pub enum Pane {
    Chat { chat_id: i64 },
    Editor { path: PathBuf },
    // We can easily add MergeTool here later!
}

/// This is the bridge between egui_tiles and your application state.
pub struct PaneBehavior<'a> {
    pub state: &'a mut crate::gui::State,
    // A queue to hold our split requests until the UI is done drawing
    pub split_requests: Vec<Pane>,
    // A queue to hold tabs that need to be closed
    pub close_requests: Vec<egui_tiles::TileId>,
}

impl<'a> Behavior<Pane> for PaneBehavior<'a> {
    // Places buttons on the far right of the tab bar row
    fn top_bar_right_ui(
        &mut self,
        tiles: &egui_tiles::Tiles<Pane>,
        ui: &mut egui::Ui,
        _tile_id: TileId,
        tabs: &egui_tiles::Tabs,
        _is_expanded: &mut f32,
    ) {
        // Get the currently active tab in this container
        if let Some(active_id) = tabs.active {
            if let Some(egui_tiles::Tile::Pane(Pane::Chat { chat_id })) = tiles.get(active_id) {
                // Draw the split button
                if ui.button("◫").on_hover_text("Split this chat into a new pane").clicked() {
                    self.split_requests.push(Pane::Chat { chat_id: *chat_id });
                }
            }

            // Note: If you later add a split option for the Editor pane,
            // you can easily add an `else if let Some(Tile::Pane(Pane::Editor { path }))` block here!
        }
    }

    // Override the tile title to inject the "A1", "B2" label
    fn tab_title_for_tile(&mut self, tiles: &egui_tiles::Tiles<Pane>, tile_id: TileId) -> egui::WidgetText {
        let label = self.state.tile_labels.get(&tile_id).cloned().unwrap_or_default();
        let title = if let Some(egui_tiles::Tile::Pane(pane)) = tiles.get(tile_id) {
            self.tab_title_for_pane(pane).text().to_string()
        } else {
            "Tab".to_string()
        };
        format!("{}  {}", label, title).into()
    }

    fn tab_title_for_pane(&mut self, pane: &Pane) -> egui::WidgetText {
        match pane {
            Pane::Chat { chat_id } => {
                if let Some(chat) = self.state.open_chats.get(chat_id) {
                    let safe_title = chat.title.replace('\n', " ").replace('\r', "");
                    // ADDED: Spacing and the close icon
                    format!("💬 {} ✖", safe_title).into()
                } else {
                    "💬 New Chat ✖".into()
                }
            }
            Pane::Editor { path } => {
                let filename = path.file_name().unwrap_or_default().to_string_lossy();
                format!("📝 {} ✖", filename).into()
            }
        }
    }

    // Intercept clicks on the tab header
    fn on_tab_button(
        &mut self,
        tiles: &egui_tiles::Tiles<Pane>,
        tile_id: TileId,
        button_response: egui::Response,
    ) -> egui::Response {
        if button_response.clicked() {
            // Check if the click was on the rightmost edge of the tab (the ✖ icon)
            if let Some(pointer_pos) = button_response.interact_pointer_pos() {
                // If the click is within the last 24 pixels of the button, treat it as a close!
                if pointer_pos.x > button_response.rect.max.x - 24.0 {
                    self.close_requests.push(tile_id);
                    return button_response;
                }
            }

            // Normal click logic -> Focus the tab
            self.state.active_tile_id = Some(tile_id);
            if let Some(egui_tiles::Tile::Pane(Pane::Chat { chat_id })) = tiles.get(tile_id) {
                self.state.active_chat_id = Some(*chat_id);
            }
        } else if button_response.middle_clicked() {
            // BONUS: Middle-clicking anywhere on the tab will also close it!
            self.close_requests.push(tile_id);
        }

        button_response
    }

    fn pane_ui(&mut self, ui: &mut egui::Ui, tile_id: TileId, pane: &mut Pane) -> UiResponse {
        // Determine if this specific tile is the globally active one
        let is_active = self.state.active_tile_id == Some(tile_id);

        // Define our highlight border. If active, use the theme's hyperlink/accent color.
        let frame = if is_active {
            egui::Frame::default()
                .stroke(egui::Stroke::new(1.0, ui.visuals().strong_text_color()))
                .inner_margin(1.0)
        } else {
            egui::Frame::default()
                .stroke(egui::Stroke::NONE)
                .inner_margin(2.0) // Keep layout stable when border disappears
        };

        frame.show(ui, |ui| {
            // --- THE WATERMARK ---
            if let Some(label) = self.state.tile_labels.get(&tile_id).and_then(
                    |s| s.find(|c: char| c.is_ascii_digit()).map(|i| &s[..i])) {
                let text_color = ui.visuals().text_color().linear_multiply(0.10);
                ui.painter().text(
                    ui.max_rect().right_top() + egui::vec2(-5.0, 5.0),
                    egui::Align2::RIGHT_TOP,
                    label,
                    egui::FontId::monospace(48.0),
                    text_color
                );
            }

            // SANDBOX ALL IDS TO PREVENT COLLISIONS BETWEEN DUPLICATE PANES
            ui.push_id(tile_id, |ui| {
                match pane {
                    Pane::Chat { chat_id } => {
                        // Detect clicks inside the body of the pane to steal focus
                        if ui.ui_contains_pointer() && ui.input(|i| i.pointer.any_pressed()) {
                            self.state.active_tile_id = Some(tile_id); // NEW
                            self.state.active_chat_id = Some(*chat_id);
                        }

                        egui::ScrollArea::vertical()
                            .id_salt(format!("pane_scroll_{:?}", tile_id))
                            .stick_to_bottom(true)
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                crate::gui::chat::render_chat_messages(
                                    ui,
                                    self.state,
                                    *chat_id,
                                    ui.available_width(),
                                );
                            });
                    }
                    Pane::Editor { path } => {
                        // Detect clicks in the editor pane too!
                        if ui.ui_contains_pointer() && ui.input(|i| i.pointer.any_pressed()) {
                            self.state.active_tile_id = Some(tile_id);
                        }
                        // ... editor code ...
                    }
                }
            });
        });

        UiResponse::None
    }

    fn simplification_options(&self) -> egui_tiles::SimplificationOptions {
        let mut opts = egui_tiles::SimplificationOptions::default();
        opts.all_panes_must_have_tabs = true;
        opts
    }
}

pub fn compute_tile_locations(tree: &egui_tiles::Tree<Pane>) -> (
    std::collections::HashMap<egui_tiles::TileId, String>,
    std::collections::HashMap<i64, Vec<String>>
) {
    let mut tile_labels = std::collections::HashMap::new();
    let mut chat_locations = std::collections::HashMap::new();

    // 1. Find all Tab containers to assign them A, B, C...
    let mut tab_containers = Vec::new();
    for (tile_id, tile) in tree.tiles.iter() {
        if let egui_tiles::Tile::Container(egui_tiles::Container::Tabs(_)) = tile {
            tab_containers.push(*tile_id);
        }
    }

    // Sort by the debug string since TileId doesn't implement Ord natively
    tab_containers.sort_by_cached_key(|id| format!("{:?}", id));

    // Helper to get A, B.. Z, AA, AB..
    fn index_to_letter(mut idx: usize) -> String {
        let mut res = String::new();
        loop {
            let rem = idx % 26;
            res.insert(0, (b'A' + rem as u8) as char);
            if idx < 26 { break; }
            idx = (idx / 26) - 1;
        }
        res
    }

    // 2. Assign Tab numbers (1, 2, 3) inside the containers
    for (i, container_id) in tab_containers.iter().enumerate() {
        let letter = index_to_letter(i);
        if let Some(egui_tiles::Tile::Container(egui_tiles::Container::Tabs(tabs))) = tree.tiles.get(*container_id) {
            for (tab_idx, &child_id) in tabs.children.iter().enumerate() {
                let label = format!("{}{}", letter, tab_idx + 1);
                tile_labels.insert(child_id, label.clone());

                if let Some(egui_tiles::Tile::Pane(Pane::Chat { chat_id })) = tree.tiles.get(child_id) {
                    chat_locations.entry(*chat_id).or_insert_with(Vec::new).push(label);
                }
            }
        }
    }

    // 3. Fallback for panes floating alone (Not inside a Tabs container yet)
    let mut floating_idx = 0;
    for (tile_id, tile) in tree.tiles.iter() {
        if let egui_tiles::Tile::Pane(pane) = tile {
            if !tile_labels.contains_key(tile_id) {
                let letter = index_to_letter(tab_containers.len() + floating_idx);
                let label = format!("{}1", letter);
                tile_labels.insert(*tile_id, label.clone());
                floating_idx += 1;

                if let Pane::Chat { chat_id } = pane {
                    chat_locations.entry(*chat_id).or_insert_with(Vec::new).push(label);
                }
            }
        }
    }

    (tile_labels, chat_locations)
}

// The smart tab spawner/focuser we discussed
pub fn open_chat_in_tab(state: &mut crate::gui::State, new_chat_id: i64) {
    let prev_chat_id = state.active_chat_id;
    state.active_chat_id = Some(new_chat_id);

    // If already open, just focus it
    let mut found_tile_id = None;
    for (tile_id, tile) in state.pane_tree.tiles.iter() {
        if let egui_tiles::Tile::Pane(Pane::Chat { chat_id }) = tile {
            if *chat_id == new_chat_id {
                found_tile_id = Some(*tile_id);
                break;
            }
        }
    }

    if let Some(tile_id) = found_tile_id {
        state.pane_tree.make_active(|tid, _| tid == tile_id);
        return;
    }

    let new_pane = Pane::Chat { chat_id: new_chat_id };
    let new_tile_id = state.pane_tree.tiles.insert_pane(new_pane);

    // Find the TileId of the previously active chat
    let mut prev_tile_id = None;
    if let Some(prev_id) = prev_chat_id {
        for (tid, tile) in state.pane_tree.tiles.iter() {
            if let egui_tiles::Tile::Pane(Pane::Chat { chat_id }) = tile {
                if *chat_id == prev_id { prev_tile_id = Some(*tid); break; }
            }
        }
    }

    if let Some(ptid) = prev_tile_id {
        // Check if the previous pane is already inside a Tabs container
        let mut parent_is_tabs = None;
        for (tid, tile) in state.pane_tree.tiles.iter() {
            if let egui_tiles::Tile::Container(egui_tiles::Container::Tabs(t)) = tile {
                if t.children.contains(&ptid) {
                    parent_is_tabs = Some(*tid);
                    break;
                }
            }
        }

        if let Some(tabs_id) = parent_is_tabs {
            // It was inside a Tabs container -> Inject it natively!
            if let Some(egui_tiles::Tile::Container(egui_tiles::Container::Tabs(tabs))) = state.pane_tree.tiles.get_mut(tabs_id) {
                tabs.add_child(new_tile_id);
                tabs.set_active(new_tile_id);
            }
        } else {
            // The active pane is floating (e.g., freshly split or root).
            // We mutate the tile IN-PLACE so parent pointers (Linear, Grid, Root) remain perfectly intact!

            // 1. Extract the old Pane payload safely
            let old_pane = if let Some(egui_tiles::Tile::Pane(p)) = state.pane_tree.tiles.get(ptid) {
                p.clone()
            } else {
                return;
            };

            // 2. Relocate the old pane to a new ID
            let old_pane_new_id = state.pane_tree.tiles.insert_pane(old_pane);

            // 3. Generate a fresh Tabs container holding both the old pane and the new pane
            let mut new_tabs = egui_tiles::Tabs::new(vec![old_pane_new_id, new_tile_id]);
            new_tabs.active = Some(new_tile_id);

            // 4. Overwrite the existing floating pane with the new Tabs container!
            if let Some(tile_ref) = state.pane_tree.tiles.get_mut(ptid) {
                *tile_ref = egui_tiles::Tile::Container(egui_tiles::Container::Tabs(new_tabs));
            }
        }
    } else {
        // No previous chat found. Inject into root.
        if let Some(root_id) = state.pane_tree.root {
            let mut is_tabs = false;
            if let Some(egui_tiles::Tile::Container(egui_tiles::Container::Tabs(tabs))) = state.pane_tree.tiles.get_mut(root_id) {
                tabs.add_child(new_tile_id);
                tabs.set_active(new_tile_id);
                is_tabs = true;
            }

            // Only wrap if the root isn't already a Tabs container
            if !is_tabs {
                let new_tabs_id = state.pane_tree.tiles.insert_tab_tile(vec![root_id, new_tile_id]);
                state.pane_tree.root = Some(new_tabs_id);
                if let Some(egui_tiles::Tile::Container(egui_tiles::Container::Tabs(tabs))) = state.pane_tree.tiles.get_mut(new_tabs_id) {
                    tabs.set_active(new_tile_id);
                }
            }
        } else {
            // Tree is entirely empty
            state.pane_tree.root = Some(new_tile_id);
        }
    }
}