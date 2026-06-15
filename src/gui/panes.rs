use egui_tiles::{Behavior, TileId, UiResponse};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// This enum represents the data we save to the .ron file.
/// It is lightweight and only contains the instructions on WHAT to render.
#[derive(Clone, Serialize, Deserialize, PartialEq)]
pub enum Pane {
    Chat { chat_id: i64 },
    Editor { path: PathBuf, content: String },
    Merge { path: PathBuf },
}

#[derive(Clone, Copy, PartialEq)]
pub enum SplitAction {
    Right,
    Down,
}

/// This is the bridge between egui_tiles and your application state.
pub struct PaneBehavior<'a> {
    pub state: &'a mut crate::gui::State,
    // A queue to hold our split requests until the UI is done drawing
    pub split_requests: Vec<(Pane, SplitAction)>,
    // A queue to hold tabs that need to be closed
    pub close_requests: Vec<egui_tiles::TileId>,
}

impl<'a> Behavior<Pane> for PaneBehavior<'a> {
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
            // Grab the active pane, clone it, and attach the split direction.
            if let Some(egui_tiles::Tile::Pane(active_pane)) = tiles.get(active_id) {
                if ui.button("◫").on_hover_text("Split Right").clicked() {
                    self.split_requests.push((active_pane.clone(), SplitAction::Right));
                }
                if ui.button("⊟").on_hover_text("Split Down").clicked() {
                    self.split_requests.push((active_pane.clone(), SplitAction::Down));
                }
            }
        }
    }

    // Override the tile title to inject the "A1", "B2" label
    fn tab_title_for_tile(&mut self, tiles: &egui_tiles::Tiles<Pane>, tile_id: TileId) -> egui::widget_text::WidgetText {
        let label = self.state.tile_labels.get(&tile_id).cloned().unwrap_or_default();
        let title = if let Some(egui_tiles::Tile::Pane(pane)) = tiles.get(tile_id) {
            self.tab_title_for_pane(pane).text().to_string()
        } else {
            "Tab".to_string()
        };
        format!("{}  {}", label, title).into()
    }

    // FIX: Updated return type to egui::widget_text::WidgetText
    fn tab_title_for_pane(&mut self, pane: &Pane) -> egui::widget_text::WidgetText {
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
            Pane::Editor { path, content: _ } => {
                let filename = path.file_name().unwrap_or_default().to_string_lossy();
                format!("📝 {} ✖", filename).into()
            }
            Pane::Merge { path } => {
                let filename = path.file_name().unwrap_or_default().to_string_lossy();
                format!("🛠 {} ✖", filename).into()
            }
        }
    }

    // Intercept clicks on the tab header
    fn on_tab_button(
        &mut self,
        tiles: &egui_tiles::Tiles<Pane>,
        tile_id: TileId,
        button_response: egui::response::Response,
    ) -> egui::response::Response {
        if button_response.clicked() {
            // Check if the click was on the rightmost edge of the tab (the ✖ icon)
            if let Some(pointer_pos) = button_response.interact_pointer_pos() {
                // If the click is within the last 24 pixels of the button, treat it as a close!
                if pointer_pos.x > button_response.rect.max.x - 24.0 {
                    self.close_requests.push(tile_id);
                    return button_response;
                }
            }

            // --- FIX: Strictly synchronize state when tab is clicked ---
            self.state.active_tile_id = Some(tile_id);
            match tiles.get(tile_id) {
                Some(egui_tiles::Tile::Pane(Pane::Chat { chat_id })) => {
                    self.state.active_chat_id = Some(*chat_id);
                }
                Some(egui_tiles::Tile::Pane(Pane::Editor { .. })) => {
                    self.state.active_chat_id = None; // Disconnect bottom panel!
                }
                _ => {}
            }


        }

        button_response
    }

    fn pane_ui(&mut self, ui: &mut egui::Ui, tile_id: TileId, pane: &mut Pane) -> UiResponse {
        let is_active = self.state.active_tile_id == Some(tile_id);

        // Define our highlight border. If active, use the theme's hyperlink/accent color.
        let frame = if is_active {
            egui::Frame::default()
                .stroke(egui::Stroke::new(1.0, ui.visuals().strong_text_color()))
                .inner_margin(1.0)
        } else {
            egui::Frame::default()
                .stroke(egui::Stroke::NONE)
                .inner_margin(2.0)
        };

        frame.show(ui, |ui| {
            // --- FIX: PASSIVE FOCUS DETECTION ---
            // Detect clicks ANYWHERE in the tile's maximum rectangle and steal focus.
            if ui.rect_contains_pointer(ui.max_rect()) && ui.input(|i| i.pointer.any_pressed()) {
                self.state.active_tile_id = Some(tile_id);
                match pane {
                    Pane::Chat { chat_id } => {
                        self.state.active_chat_id = Some(*chat_id);
                    }
                    Pane::Editor { .. } | Pane::Merge { .. } => {
                        self.state.active_chat_id = None; // Disconnect bottom panel!
                    }
                }
            }

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

                    Pane::Editor { path, content } => {
                        ui.horizontal(|ui| {
                            ui.visuals_mut().override_text_color = Some(ui.visuals().text_color().linear_multiply(0.8));
                            ui.label(format!("Editing: {}", path.file_name().unwrap_or_default().to_string_lossy()));

                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                // 2. Removed the Split buttons from here!
                                // Now we only keep pane-specific actions like Save.
                                if ui.button("💾 Save").clicked() {
                                    if let Err(e) = std::fs::write(&path, &*content) {
                                        self.state.error_msg = Some(format!("Failed to save file: {}", e));
                                        self.state.is_modal_open = true;
                                    }
                                }
                            });
                        });
                        ui.separator();

                        let available_width = ui.available_width();

                        // No more outer scroll area! Let the editor fill the space.
                        crate::bulat::editor::CodeEditor::default()
                            .id_source(format!("editor_code_{:?}", tile_id))
                            .with_theme(crate::bulat::editor::ColorTheme::SV)
                            .with_syntax(crate::bulat::editor::Syntax::rust())
                            .vscroll(true)
                            .v_auto_shrink(false)
                            .desired_width(available_width)
                            .show(ui, content);
                    }

                    Pane::Merge { path } => {
                        ui.horizontal(|ui| {
                            ui.visuals_mut().override_text_color = Some(ui.visuals().text_color().linear_multiply(0.8));
                            ui.label(format!("Merging: {}", path.file_name().unwrap_or_default().to_string_lossy()));

                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui.button("💾 Save").clicked() {
                                    if let Some(app) = self.state.merge_apps.get(&tile_id) {
                                        if let Err(e) = std::fs::write(&path, &app.left_code_real) {
                                            self.state.error_msg = Some(format!("Failed to save file: {}", e));
                                            self.state.is_modal_open = true;
                                        }
                                    }
                                }
                            });
                        });
                        ui.separator();

                        if let Some(app) = self.state.merge_apps.get_mut(&tile_id) {
                            app.show(ui);
                        } else {
                            ui.vertical_centered(|ui| {
                                ui.add_space(20.0);
                                ui.colored_label(ui.visuals().warn_fg_color, "Merge session data lost.");
                                ui.label("Please close this tab and reopen it from the chat.");
                            });
                        }
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

pub fn open_editor_in_tab(state: &mut crate::gui::State, path: PathBuf, content: String) {
    let prev_active_id = state.active_tile_id; // Capture current focus

    let new_pane = Pane::Editor { path, content };
    let new_tile_id = state.pane_tree.tiles.insert_pane(new_pane);

    state.active_tile_id = Some(new_tile_id); // Focus new tab globally
    state.active_chat_id = None; // Ensure bottom panel disconnects when editor opens

    if let Some(ptid) = prev_active_id {
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
            if let Some(egui_tiles::Tile::Container(egui_tiles::Container::Tabs(tabs))) = state.pane_tree.tiles.get_mut(tabs_id) {
                tabs.add_child(new_tile_id);
                tabs.set_active(new_tile_id);
            }
        } else {
            let old_pane = if let Some(egui_tiles::Tile::Pane(p)) = state.pane_tree.tiles.get(ptid) {
                p.clone()
            } else { return; };

            let old_pane_new_id = state.pane_tree.tiles.insert_pane(old_pane);
            let mut new_tabs = egui_tiles::Tabs::new(vec![old_pane_new_id, new_tile_id]);
            new_tabs.active = Some(new_tile_id);

            if let Some(tile_ref) = state.pane_tree.tiles.get_mut(ptid) {
                *tile_ref = egui_tiles::Tile::Container(egui_tiles::Container::Tabs(new_tabs));
            }
        }
    } else {
        if let Some(root_id) = state.pane_tree.root {
            // FIXED: Check if the root is already a tabs container
            let mut is_tabs = false;
            if let Some(egui_tiles::Tile::Container(egui_tiles::Container::Tabs(tabs))) = state.pane_tree.tiles.get_mut(root_id) {
                tabs.add_child(new_tile_id);
                tabs.set_active(new_tile_id);
                is_tabs = true;
            }

            if !is_tabs {
                let new_tabs_id = state.pane_tree.tiles.insert_tab_tile(vec![root_id, new_tile_id]);
                state.pane_tree.root = Some(new_tabs_id);

                // FIXED: Tell the brand new Tabs container to focus our new tile
                if let Some(egui_tiles::Tile::Container(egui_tiles::Container::Tabs(tabs))) = state.pane_tree.tiles.get_mut(new_tabs_id) {
                    tabs.set_active(new_tile_id);
                }
            }
        } else {
            state.pane_tree.root = Some(new_tile_id);
        }
    }

    // Force egui_tiles to traverse the tree and verify focus
    state.pane_tree.make_active(|tid, _| tid == new_tile_id);
}

pub fn open_editor_in_right_pane(state: &mut crate::gui::State, path: std::path::PathBuf, content: String) {
    let active_id = match state.active_tile_id {
        Some(id) => id,
        None => {
            // Fallback to normal open if nothing is active
            open_editor_in_tab(state, path, content);
            return;
        }
    };

    // Helper: Find parent and position
    fn find_parent(tree: &egui_tiles::Tree<Pane>, child_id: egui_tiles::TileId) -> Option<(egui_tiles::TileId, usize)> {
        for (id, tile) in tree.tiles.iter() {
            if let egui_tiles::Tile::Container(c) = tile {
                // FIXED: children() is an iterator, so we call .position() directly
                if let Some(pos) = c.children().position(|&child| child == child_id) {
                    return Some((*id, pos));
                }
            }
        }
        None
    }

    // Helper: Find leftmost leaf in a container
    fn find_leftmost_leaf(tree: &egui_tiles::Tree<Pane>, current: egui_tiles::TileId) -> egui_tiles::TileId {
        if let Some(tile) = tree.tiles.get(current) {
            match tile {
                egui_tiles::Tile::Container(c) => {
                    // FIXED: Since children() is an iterator, use .next() to get the first item
                    if let Some(&first) = c.children().next() {
                        return find_leftmost_leaf(tree, first);
                    }
                }
                egui_tiles::Tile::Pane(_) => return current,
            }
        }
        current
    }

    // 1. Identify if the active tile is wrapped in Tabs
    let mut target_node = active_id;
    if let Some((pid, _)) = find_parent(&state.pane_tree, active_id) {
        if matches!(state.pane_tree.tiles.get(pid), Some(egui_tiles::Tile::Container(egui_tiles::Container::Tabs(_)))) {
            target_node = pid;
        }
    }

    // 2. Search up the tree for a Horizontal layout with a neighbor to the right
    let mut right_neighbor_id = None;
    let mut current = target_node;

    while let Some((parent_id, pos)) = find_parent(&state.pane_tree, current) {
        // FIXED: Match on Linear container and check its direction
        if let Some(egui_tiles::Tile::Container(egui_tiles::Container::Linear(l))) = state.pane_tree.tiles.get(parent_id) {
            if l.dir == egui_tiles::LinearDir::Horizontal {
                if pos + 1 < l.children.len() {
                    right_neighbor_id = Some(l.children[pos + 1]);
                    break;
                }
            }
        }
        current = parent_id;
    }

    // 3. Create the new pane
    let new_pane = Pane::Editor { path, content };
    let new_tile_id = state.pane_tree.tiles.insert_pane(new_pane);

    // 4. If a neighbor exists on the right, inject the tab!
    if let Some(neighbor_id) = right_neighbor_id {
        let leaf_id = find_leftmost_leaf(&state.pane_tree, neighbor_id);

        let mut leaf_parent_tabs = None;
        if let Some((pid, _)) = find_parent(&state.pane_tree, leaf_id) {
            if let Some(egui_tiles::Tile::Container(egui_tiles::Container::Tabs(_))) = state.pane_tree.tiles.get(pid) {
                leaf_parent_tabs = Some(pid);
            }
        }

        if let Some(tabs_id) = leaf_parent_tabs {
            // Found a Tabs container on the right! Append to it.
            if let Some(egui_tiles::Tile::Container(egui_tiles::Container::Tabs(tabs))) = state.pane_tree.tiles.get_mut(tabs_id) {
                tabs.add_child(new_tile_id);
                tabs.set_active(new_tile_id);
            }
        } else {
            // Found a floating Pane on the right! Wrap it in a Tabs container in-place.

            // 1. Use a tightly scoped block to extract the old pane and drop the borrow immediately
            let old_pane = {
                let tile_ref = state.pane_tree.tiles.get_mut(leaf_id).unwrap();
                match std::mem::replace(tile_ref, egui_tiles::Tile::Container(egui_tiles::Container::Tabs(egui_tiles::Tabs::new(vec![])))) {
                    egui_tiles::Tile::Pane(p) => p,
                    _ => return, // Safely abort if something went wrong
                }
            }; // <-- tile_ref is dropped here, ending the first mutable borrow

            // 2. Now it is perfectly safe to borrow the tree again to insert the new pane
            let old_pane_new_id = state.pane_tree.tiles.insert_pane(old_pane);

            // 3. Re-borrow the original leaf one last time to populate its new Tabs container
            if let Some(egui_tiles::Tile::Container(egui_tiles::Container::Tabs(tabs))) = state.pane_tree.tiles.get_mut(leaf_id) {
                tabs.children = vec![old_pane_new_id, new_tile_id];
                tabs.active = Some(new_tile_id);
            }
        }

        state.pane_tree.make_active(|tid, _| tid == new_tile_id);
        state.active_tile_id = Some(new_tile_id);
        state.active_chat_id = None;
        return;
    }

    // 5. No right neighbor found? Split horizontally to create one.
    let new_tabs_id = state.pane_tree.tiles.insert_tab_tile(vec![new_tile_id]);

    if let Some(root_id) = state.pane_tree.root {
        let new_root = state.pane_tree.tiles.insert_horizontal_tile(vec![root_id, new_tabs_id]);
        state.pane_tree.root = Some(new_root);
    } else {
        state.pane_tree.root = Some(new_tabs_id);
    }

    state.pane_tree.make_active(|tid, _| tid == new_tile_id);
    state.active_tile_id = Some(new_tile_id);
    state.active_chat_id = None;
}

// Tile spawners at the bottom of the file
pub fn open_merge_in_tab(state: &mut crate::gui::State, path: PathBuf, left: String, right: String) {
    let prev_active_id = state.active_tile_id;

    let new_pane = Pane::Merge { path };
    let new_tile_id = state.pane_tree.tiles.insert_pane(new_pane);

    // Inject the heavy DiffApp into State memory
    state.merge_apps.insert(new_tile_id, crate::bulat::DiffApp::new(left, right));

    state.active_tile_id = Some(new_tile_id);
    state.active_chat_id = None;

    if let Some(ptid) = prev_active_id {
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
            if let Some(egui_tiles::Tile::Container(egui_tiles::Container::Tabs(tabs))) = state.pane_tree.tiles.get_mut(tabs_id) {
                tabs.add_child(new_tile_id);
                tabs.set_active(new_tile_id);
            }
        } else {
            let old_pane = if let Some(egui_tiles::Tile::Pane(p)) = state.pane_tree.tiles.get(ptid) {
                p.clone()
            } else { return; };

            let old_pane_new_id = state.pane_tree.tiles.insert_pane(old_pane);
            let mut new_tabs = egui_tiles::Tabs::new(vec![old_pane_new_id, new_tile_id]);
            new_tabs.active = Some(new_tile_id);

            if let Some(tile_ref) = state.pane_tree.tiles.get_mut(ptid) {
                *tile_ref = egui_tiles::Tile::Container(egui_tiles::Container::Tabs(new_tabs));
            }
        }
    } else {
        if let Some(root_id) = state.pane_tree.root {
            let mut is_tabs = false;
            if let Some(egui_tiles::Tile::Container(egui_tiles::Container::Tabs(tabs))) = state.pane_tree.tiles.get_mut(root_id) {
                tabs.add_child(new_tile_id);
                tabs.set_active(new_tile_id);
                is_tabs = true;
            }

            if !is_tabs {
                let new_tabs_id = state.pane_tree.tiles.insert_tab_tile(vec![root_id, new_tile_id]);
                state.pane_tree.root = Some(new_tabs_id);

                if let Some(egui_tiles::Tile::Container(egui_tiles::Container::Tabs(tabs))) = state.pane_tree.tiles.get_mut(new_tabs_id) {
                    tabs.set_active(new_tile_id);
                }
            }
        } else {
            state.pane_tree.root = Some(new_tile_id);
        }
    }

    state.pane_tree.make_active(|tid, _| tid == new_tile_id);
}

pub fn open_merge_in_right_pane(state: &mut crate::gui::State, path: PathBuf, left: String, right: String) {
    let active_id = match state.active_tile_id {
        Some(id) => id,
        None => {
            open_merge_in_tab(state, path, left, right);
            return;
        }
    };

    fn find_parent(tree: &egui_tiles::Tree<Pane>, child_id: egui_tiles::TileId) -> Option<(egui_tiles::TileId, usize)> {
        for (id, tile) in tree.tiles.iter() {
            if let egui_tiles::Tile::Container(c) = tile {
                if let Some(pos) = c.children().position(|&child| child == child_id) {
                    return Some((*id, pos));
                }
            }
        }
        None
    }

    fn find_leftmost_leaf(tree: &egui_tiles::Tree<Pane>, current: egui_tiles::TileId) -> egui_tiles::TileId {
        if let Some(tile) = tree.tiles.get(current) {
            match tile {
                egui_tiles::Tile::Container(c) => {
                    if let Some(&first) = c.children().next() {
                        return find_leftmost_leaf(tree, first);
                    }
                }
                egui_tiles::Tile::Pane(_) => return current,
            }
        }
        current
    }

    let mut target_node = active_id;
    if let Some((pid, _)) = find_parent(&state.pane_tree, active_id) {
        if matches!(state.pane_tree.tiles.get(pid), Some(egui_tiles::Tile::Container(egui_tiles::Container::Tabs(_)))) {
            target_node = pid;
        }
    }

    let mut right_neighbor_id = None;
    let mut current = target_node;

    while let Some((parent_id, pos)) = find_parent(&state.pane_tree, current) {
        if let Some(egui_tiles::Tile::Container(egui_tiles::Container::Linear(l))) = state.pane_tree.tiles.get(parent_id) {
            if l.dir == egui_tiles::LinearDir::Horizontal {
                if pos + 1 < l.children.len() {
                    right_neighbor_id = Some(l.children[pos + 1]);
                    break;
                }
            }
        }
        current = parent_id;
    }

    let new_pane = Pane::Merge { path };
    let new_tile_id = state.pane_tree.tiles.insert_pane(new_pane);

    // Inject the heavy DiffApp into State memory
    state.merge_apps.insert(new_tile_id, crate::bulat::DiffApp::new(left, right));

    if let Some(neighbor_id) = right_neighbor_id {
        let leaf_id = find_leftmost_leaf(&state.pane_tree, neighbor_id);

        let mut leaf_parent_tabs = None;
        if let Some((pid, _)) = find_parent(&state.pane_tree, leaf_id) {
            if let Some(egui_tiles::Tile::Container(egui_tiles::Container::Tabs(_))) = state.pane_tree.tiles.get(pid) {
                leaf_parent_tabs = Some(pid);
            }
        }

        if let Some(tabs_id) = leaf_parent_tabs {
            if let Some(egui_tiles::Tile::Container(egui_tiles::Container::Tabs(tabs))) = state.pane_tree.tiles.get_mut(tabs_id) {
                tabs.add_child(new_tile_id);
                tabs.set_active(new_tile_id);
            }
        } else {
            let old_pane = {
                let tile_ref = state.pane_tree.tiles.get_mut(leaf_id).unwrap();
                match std::mem::replace(tile_ref, egui_tiles::Tile::Container(egui_tiles::Container::Tabs(egui_tiles::Tabs::new(vec![])))) {
                    egui_tiles::Tile::Pane(p) => p,
                    _ => return,
                }
            };

            let old_pane_new_id = state.pane_tree.tiles.insert_pane(old_pane);

            if let Some(egui_tiles::Tile::Container(egui_tiles::Container::Tabs(tabs))) = state.pane_tree.tiles.get_mut(leaf_id) {
                tabs.children = vec![old_pane_new_id, new_tile_id];
                tabs.active = Some(new_tile_id);
            }
        }

        state.pane_tree.make_active(|tid, _| tid == new_tile_id);
        state.active_tile_id = Some(new_tile_id);
        state.active_chat_id = None;
        return;
    }

    let new_tabs_id = state.pane_tree.tiles.insert_tab_tile(vec![new_tile_id]);

    if let Some(root_id) = state.pane_tree.root {
        let new_root = state.pane_tree.tiles.insert_horizontal_tile(vec![root_id, new_tabs_id]);
        state.pane_tree.root = Some(new_root);
    } else {
        state.pane_tree.root = Some(new_tabs_id);
    }

    state.pane_tree.make_active(|tid, _| tid == new_tile_id);
    state.active_tile_id = Some(new_tile_id);
    state.active_chat_id = None;
}