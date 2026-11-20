use egui_tiles::{Behavior, TileId, UiResponse};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::split_button::SplitButton;

/// This enum represents the data we save to the .ron file.
/// It is lightweight and only contains the instructions on WHAT to render.
#[derive(Clone, Serialize, Deserialize, PartialEq)]
pub enum Pane {
    Chat { chat_id: i64 },
    Editor { path: PathBuf, content: String },
    Merge { path: PathBuf },
    SearchResults { query: String, results: Vec<inforno_core::common::SearchResult> },
    /// ABC text and its score, side by side.
    Score { source: String },
}

#[derive(Clone, Copy, PartialEq)]
pub enum SplitAction {
    Right,
    Down,
}

/// A pane shown in a window of its own, inside the main one, instead of as
/// a tab. It keeps the id it had in the tree: egui_tiles never hands out an
/// id twice (its counter only grows), so `editor_apps`, `merge_apps` and the
/// pane's widget state stay keyed by it, and docking puts it back under the
/// same id.
/// Kept for this run only: Inforno saves no layout.
#[derive(Clone)]
pub struct FloatingPane {
    pub id: TileId,
    pub pane: Pane,
    /// The window's outer rect, in points.
    pub rect: egui::Rect,
    /// The tab group it came from: "Dock as tab" goes back there if it still exists.
    pub home: Option<TileId>,
}

/// What a floating window's buttons asked for.
#[derive(Clone, Copy)]
pub enum FloatingAction {
    /// "Dock as tab"
    Dock(TileId),
    /// The window's ✖
    Close(TileId),
}

/// The egui id of a floating pane's window (also its layer's id).
pub fn floating_window_id(tile_id: TileId) -> egui::Id {
    egui::Id::unique(("floating_pane", tile_id))
}

/// The scope of a pane's widgets: the same wherever the pane is drawn.
pub fn pane_scope_id(tile_id: TileId) -> egui::Id {
    egui::Id::unique(("pane_scope", tile_id))
}

/// Where `apply_actions` leaves the view rect of the chat a score popup was
/// asked from, keyed by the score's text, for `open_score` to center it on.
pub fn score_popup_over_id(source: &str) -> egui::Id {
    egui::Id::new(("score_popup_over", source))
}

/// Where a score asked for from a chat opens.
#[derive(Clone, Copy, PartialEq)]
pub enum ScorePlace {
    /// A floating window centered on the chat: a click on a note.
    Window,
    /// A tab in the active tab group.
    Tab,
    /// A tab in the group on the right.
    Right,
}

/// This is the bridge between egui_tiles and your application state.
pub struct PaneBehavior<'a> {
    pub state: &'a mut crate::state::State,
    // A queue to hold our split requests until the UI is done drawing
    pub split_requests: Vec<(Pane, SplitAction)>,
    // A queue to hold tabs that need to be closed
    pub close_requests: Vec<egui_tiles::TileId>,
    // Tuple of (chat_id, open_in_right_pane)
    pub open_chat_requests: Vec<(i64, bool)>,
    /// Tabs to turn into floating windows
    pub float_requests: Vec<TileId>,
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
                if ui.button("🗗").on_hover_text("Float in a window").clicked() {
                    self.float_requests.push(active_id);
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
        // Helper to format tab names (e.g., turns ".../gui/mod.rs" into "gui/")
        let format_path_name = |p: &std::path::Path| {
            let name = p.file_name().unwrap_or_default().to_string_lossy();
            if name == "mod.rs" {
                if let Some(parent) = p.parent().and_then(|parent| parent.file_name()) {
                    return format!("{}/", parent.to_string_lossy());
                }
            }
            name.into_owned()
        };

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
                format!("📝 {} ✖", format_path_name(path)).into()
            }
            Pane::Merge { path } => {
                format!("🛠 {} ✖", format_path_name(path)).into()
            }
            Pane::SearchResults { query, .. } => {
                format!("🔍 Search: {} ✖", query).into()
            }
            Pane::Score { source } => {
                // the tune's title, if it has one
                let title = source
                    .lines()
                    .find_map(|l| l.strip_prefix("T:"))
                    .map(str::trim)
                    .filter(|t| !t.is_empty())
                    .unwrap_or("Score");
                format!("♫ {} ✖", title).into()
            }
        }
    }

    // Intercept clicks on the tab header
    fn on_tab_button(
        &mut self,
        tiles: &mut egui_tiles::Tiles<Pane>,
        tile_id: TileId,
        mut button_response: egui::response::Response,
    ) -> egui::response::Response {

        // --- ADD RELATIVE PATH TOOLTIP ---
        if let Some(egui_tiles::Tile::Pane(pane)) = tiles.get(tile_id) {
            if let Pane::Editor { path, .. } | Pane::Merge { path } = pane {
                let tooltip_text = inforno_core::realm::get_relative_path(&self.state.active_realm, &self.state.project_root, path);

                button_response = button_response.on_hover_text(
                    egui::RichText::new(tooltip_text)
                    .strong()
                    .heading()
                );
            }
        }

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
        self.pane_contents(ui, tile_id, pane);
        UiResponse::None
    }

    fn simplification_options(&self) -> egui_tiles::SimplificationOptions {
        let mut opts = egui_tiles::SimplificationOptions::default();
        opts.all_panes_must_have_tabs = true;
        opts
    }
}

impl PaneBehavior<'_> {
    /// What a pane shows, wherever it is: in a tab (`pane_ui`) or in a
    /// floating window (`floating_ui`). Keyed by `tile_id` either way.
    pub fn pane_contents(&mut self, ui: &mut egui::Ui, tile_id: TileId, pane: &mut Pane) {
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
                    Pane::Editor { .. } | Pane::Merge { .. } | Pane::SearchResults { .. } | Pane::Score { .. } => {
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

            // Every pane's widgets get ids of their own, the same wherever the
            // pane is drawn. Splitting the screen or floating the pane moves
            // it in the ui tree, and ids derived from there would change and
            // lose its state: a chat's ScrollArea would start over, stuck to
            // the bottom.
            ui.scope_builder(egui::UiBuilder::new().scope_id(pane_scope_id(tile_id)), |ui| {
                // Clone the root and realm to avoid holding a borrow over self.state during UI interactions
                let proj_root = self.state.project_root.clone();
                let active_realm = self.state.active_realm.clone();

                let relative_path = |p: &std::path::PathBuf| {
                    inforno_core::realm::get_relative_path(&active_realm, &proj_root, p)
                };

                match pane {
                    Pane::Chat { chat_id } => {
                        // Per pane (we're inside push_id(tile_id)): a press waits here for the next frame.
                        let nav_id = ui.scope_id().with("chat_nav");
                        let anchor_id = ui.scope_id().with("chat_anchor");
                        let scroll = egui::ScrollArea::vertical()
                            .id_salt(format!("pane_scroll_{:?}", tile_id))
                            .stick_to_bottom(true)
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                let mut highlight = self.state.chat_highlights.remove(chat_id);
                                let msg_tops = bulat::editor::selectable_scroll_contents(ui, |ui| {
                                    crate::search_highlight::highlight_contents(ui, highlight.as_mut(), |ui| {
                                        crate::chat::render_chat_messages(
                                            ui,
                                            self.state,
                                            *chat_id,
                                            ui.available_width(),
                                        )
                                    })
                                });
                                if let Some(highlight) = highlight {
                                    self.state.chat_highlights.insert(*chat_id, highlight);
                                }
                                // Last things in the contents, after every nested ScrollArea.
                                crate::chat::apply_chat_nav(ui, nav_id, &msg_tops);
                                crate::chat::keep_chat_anchor(ui, anchor_id, &msg_tops);
                            });
                        crate::chat::chat_nav_buttons(ui, &scroll, nav_id);
                    }

                    Pane::Editor { path, content: _ } => {
                        // Note: You must store `BulatEditorApp` in your Inforno State
                        // exactly like you currently store `merge_apps`.
                        let is_active_pane = self.state.active_tile_id == Some(tile_id);
                        if let Some(app) = self.state.editor_apps.get_mut(&tile_id) {
                            app.active = is_active_pane;
                            if let Some(action) = app.show(ui) {
                                match action {
                                    bulat::EditorAction::SaveRequested => {
                                        // Inforno handles the actual disk write safely using its realm knowledge
                                        if let Err(e) = std::fs::write(&path, &app.content) {
                                            self.state.error_msg = Some(format!("Failed to save file: {}", e));
                                            self.state.is_modal_open = true;
                                        }
                                    },
                                    bulat::EditorAction::ThemeChanged(_new_theme) => {
                                        // Inforno updates the global egui visual context here if needed
                                    }
                                }
                            }
                        } else {
                            ui.vertical_centered(|ui| {
                                ui.add_space(20.0);
                                ui.colored_label(ui.visuals().warn_fg_color, "Editor session data lost.");
                                ui.label("Please close this tab and reopen it.");
                            });
                        }
                    }

                    Pane::Merge { path } => {
                        if let Some(app) = self.state.merge_apps.get_mut(&tile_id) {
                            // Let the embedded tool know the virtual filename and the ID string
                            app.left_filepath = Some(relative_path(path));
                            app.right_filepath = None;
                            app.search_state_id = Some(egui::Id::unique("tab_search").with(tile_id));
                            app.active = self.state.active_tile_id == Some(tile_id);

                            let (save_left, _) = app.show(ui);

                            if save_left {
                                if let Err(e) = std::fs::write(&path, &app.left_code_real) {
                                    self.state.error_msg = Some(format!("Failed to save file: {}", e));
                                    self.state.is_modal_open = true;
                                }
                            }
                        } else {
                            ui.vertical_centered(|ui| {
                                ui.add_space(20.0);
                                ui.colored_label(ui.visuals().warn_fg_color, "Merge session data lost.");
                                ui.label("Please close this tab and reopen it from the chat.");
                            });
                        }
                    }

                    Pane::SearchResults { query, results } => {
                        ui.heading(format!("Search Results for: \"{}\"", query));
                        ui.separator();

                        if results.is_empty() {
                            ui.label(egui::RichText::new("No results found.").weak().italics());
                        } else {
                            egui::ScrollArea::vertical()
                                .id_salt(format!("search_scroll_{:?}", tile_id))
                                .show(ui, |ui| {
                                    for res in results {
                                        ui.group(|ui| {

                                            // --- USE THE NEW HELPER HERE ---
                                            // We pass the chat title to act as the button text.
                                            // It returns true/false for which part was clicked.
                                            let btn_resp = SplitButton::new(&res.chat_title)
                                                .id_salt(res.chat_id)
                                                .arrow_tooltip("Open in right pane")
                                                .show(ui);

                                            if btn_resp.main_clicked || btn_resp.arrow_clicked {
                                                self.state.chat_highlights.insert(
                                                    res.chat_id,
                                                    crate::search_highlight::ChatHighlight::new(query),
                                                );
                                                self.open_chat_requests.push((res.chat_id, btn_resp.arrow_clicked));
                                            }
                                            // -------------------------------

                                            // grep-style snippet
                                            ui.label(crate::search_highlight::snippet_job(ui, &res.snippet, query));
                                        });
                                    }
                                });
                        }
                    }

                    Pane::Score { source } => {
                        let response = crate::score::score_editor(ui, tile_id, source);
                        if let Some(bytes) = response.save {
                            self.state.pending_file_dialog_op = None;
                            self.state.pending_score_export = Some(bytes);
                            self.state.file_dialog.save_file();
                        }
                    }
                }
            });
        });
    }

    /// Draws each floating pane in a window of its own, inside the main one,
    /// with the same contents its tab would have. Returns what the windows'
    /// buttons asked for; `ui_chat` carries that out once the tree is back.
    pub fn floating_ui(&mut self, ctx: &egui::Context, floating: &mut [FloatingPane]) -> Vec<FloatingAction> {
        if let Some(id) = self.state.raise_window.take() {
            ctx.move_to_top(egui::LayerId::new(egui::Order::Middle, floating_window_id(id)));
        }
        let enabled = !self.state.is_modal_open;
        let mut actions = Vec::new();
        for fp in floating.iter_mut() {
            let title = self.tab_title_for_pane(&fp.pane);
            let title = title.text().trim_end_matches('✖').trim_end().to_owned();
            let mut open = true;
            let shown = egui::Window::new(title)
                .id(floating_window_id(fp.id))
                .default_rect(fp.rect)
                .collapsible(false)
                .resizable(true)
                .open(&mut open)
                .show(ctx, |ui| {
                    if !enabled {
                        ui.disable();
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                        if ui.small_button("🗖 Dock as tab").on_hover_text("Put this window back as a tab").clicked() {
                            actions.push(FloatingAction::Dock(fp.id));
                        }
                    });
                    self.pane_contents(ui, fp.id, &mut fp.pane);
                });
            if let Some(shown) = shown {
                fp.rect = shown.response.rect;
            }
            if !open {
                actions.push(FloatingAction::Close(fp.id));
            }
        }
        actions
    }
}

pub fn compute_tile_locations(tree: &egui_tiles::Tree<Pane>, floating: &[FloatingPane]) -> (
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

    // 4. Floating windows: W1, W2 ...
    for (i, fp) in floating.iter().enumerate() {
        let label = format!("W{}", i + 1);
        tile_labels.insert(fp.id, label.clone());
        if let Pane::Chat { chat_id } = &fp.pane {
            chat_locations.entry(*chat_id).or_insert_with(Vec::new).push(label);
        }
    }

    (tile_labels, chat_locations)
}

// --- UNIFIED SPAWNER HELPERS ---

/// Core engine logic to spawn a pane into the currently active tab container.
fn spawn_in_tab(state: &mut crate::state::State, new_pane: Pane) -> egui_tiles::TileId {
    // A floating pane isn't in the tree, so it can't anchor a new tab.
    let prev_active_id = state.active_tile_id.filter(|id| state.pane_tree.tiles.get(*id).is_some());
    let new_tile_id = state.pane_tree.tiles.insert_pane(new_pane);

    if let Some(ptid) = prev_active_id {
        // 1. Check if the active pane is already inside a Tabs container
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
            // It is inside a Tabs container -> Inject it natively!
            if let Some(egui_tiles::Tile::Container(egui_tiles::Container::Tabs(tabs))) = state.pane_tree.tiles.get_mut(tabs_id) {
                tabs.add_child(new_tile_id);
                tabs.set_active(new_tile_id);
            }
        } else {
            // The active pane is floating (e.g., freshly split or root).
            // We mutate the tile IN-PLACE so parent pointers remain perfectly intact!
            let old_pane = if let Some(egui_tiles::Tile::Pane(p)) = state.pane_tree.tiles.get(ptid) {
                p.clone()
            } else {
                return new_tile_id;
            };

            let old_pane_new_id = state.pane_tree.tiles.insert_pane(old_pane);
            let mut new_tabs = egui_tiles::Tabs::new(vec![old_pane_new_id, new_tile_id]);
            new_tabs.active = Some(new_tile_id);

            // Overwrite the existing floating pane with the new Tabs container!
            if let Some(tile_ref) = state.pane_tree.tiles.get_mut(ptid) {
                *tile_ref = egui_tiles::Tile::Container(egui_tiles::Container::Tabs(new_tabs));
            }
        }
    } else {
        // 2. Tree is empty or nothing is selected. Inject at root.
        if let Some(root_id) = state.pane_tree.root {
            let mut is_tabs = false;
            if let Some(egui_tiles::Tile::Container(egui_tiles::Container::Tabs(tabs))) = state.pane_tree.tiles.get_mut(root_id) {
                tabs.add_child(new_tile_id);
                tabs.set_active(new_tile_id);
                is_tabs = true;
            }

            // Enforce SimplificationOptions::all_panes_must_have_tabs
            if !is_tabs {
                let new_tabs_id = state.pane_tree.tiles.insert_tab_tile(vec![root_id, new_tile_id]);
                state.pane_tree.root = Some(new_tabs_id);
                if let Some(egui_tiles::Tile::Container(egui_tiles::Container::Tabs(tabs))) = state.pane_tree.tiles.get_mut(new_tabs_id) {
                    tabs.set_active(new_tile_id);
                }
            }
        } else {
            // Tree is entirely empty. Create a root Tabs container.
            let new_tabs_id = state.pane_tree.tiles.insert_tab_tile(vec![new_tile_id]);
            state.pane_tree.root = Some(new_tabs_id);
        }
    }

    state.pane_tree.make_active(|tid, _| tid == new_tile_id);
    state.active_tile_id = Some(new_tile_id);
    new_tile_id
}

/// Core engine logic to spawn a pane into the column to the right (or create one).
fn spawn_in_right_pane(state: &mut crate::state::State, new_pane: Pane) -> egui_tiles::TileId {
    // A floating pane isn't in the tree, so it has no right neighbour.
    let active_id = match state.active_tile_id.filter(|id| state.pane_tree.tiles.get(*id).is_some()) {
        Some(id) => id,
        None => {
            // Fallback to normal open if nothing is active
            return spawn_in_tab(state, new_pane);
        }
    };

    // Helper: Find parent and position
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

    // Helper: Find leftmost leaf in a container
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

    let new_tile_id = state.pane_tree.tiles.insert_pane(new_pane);

    // 3. If a neighbor exists on the right, inject the tab!
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
            let old_pane = {
                let tile_ref = state.pane_tree.tiles.get_mut(leaf_id).unwrap();
                match std::mem::replace(tile_ref, egui_tiles::Tile::Container(egui_tiles::Container::Tabs(egui_tiles::Tabs::new(vec![])))) {
                    egui_tiles::Tile::Pane(p) => p,
                    _ => return new_tile_id, // Safely abort
                }
            };

            let old_pane_new_id = state.pane_tree.tiles.insert_pane(old_pane);

            if let Some(egui_tiles::Tile::Container(egui_tiles::Container::Tabs(tabs))) = state.pane_tree.tiles.get_mut(leaf_id) {
                tabs.children = vec![old_pane_new_id, new_tile_id];
                tabs.active = Some(new_tile_id);
            }
        }
    } else {
        // 4. No right neighbor found? Split horizontally to create one.
        let new_tabs_id = state.pane_tree.tiles.insert_tab_tile(vec![new_tile_id]);

        if let Some(root_id) = state.pane_tree.root {
            let new_root = state.pane_tree.tiles.insert_horizontal_tile(vec![root_id, new_tabs_id]);
            state.pane_tree.root = Some(new_root);
        } else {
            state.pane_tree.root = Some(new_tabs_id);
        }
    }

    state.pane_tree.make_active(|tid, _| tid == new_tile_id);
    state.active_tile_id = Some(new_tile_id);
    new_tile_id
}

// --- PUBLIC API WRAPPERS ---

pub fn open_chat_in_tab(state: &mut crate::state::State, chat_id: i64) {
    // If already open, just focus it
    let mut found_tile_id = None;
    for (tile_id, tile) in state.pane_tree.tiles.iter() {
        if let egui_tiles::Tile::Pane(Pane::Chat { chat_id: id }) = tile {
            if *id == chat_id {
                found_tile_id = Some(*tile_id);
                break;
            }
        }
    }

    if let Some(tile_id) = found_tile_id {
        state.pane_tree.make_active(|tid, _| tid == tile_id);
        state.active_tile_id = Some(tile_id);
        state.active_chat_id = Some(chat_id);
        return;
    }

    // Open in a floating window: bring that to the front instead.
    let floating = state.floating_panes.iter().find_map(|f| match &f.pane {
        Pane::Chat { chat_id: id } if *id == chat_id => Some(f.id),
        _ => None,
    });
    if let Some(tile_id) = floating {
        state.raise_window = Some(tile_id);
        state.active_tile_id = Some(tile_id);
        state.active_chat_id = Some(chat_id);
        return;
    }

    spawn_in_tab(state, Pane::Chat { chat_id });
    state.active_chat_id = Some(chat_id); // Connect the bottom panel
}

pub fn open_chat_in_right_pane(state: &mut crate::state::State, chat_id: i64) {
    spawn_in_right_pane(state, Pane::Chat { chat_id });
    state.active_chat_id = Some(chat_id);
}

pub fn open_editor_in_tab(state: &mut crate::state::State, path: PathBuf, content: String) {
    let new_tile_id = spawn_in_tab(state, Pane::Editor { path: path.clone(), content: String::new() });
    
    // Generate the clean, relative path for the editor's UI display
    let display_path = inforno_core::realm::get_relative_path(&state.active_realm, &state.project_root, &path);
    state.editor_apps.insert(new_tile_id, bulat::BulatEditorApp::new(Some(PathBuf::from(display_path)), content));
    
    state.active_chat_id = None; // Disconnect the bottom panel
}

pub fn open_editor_in_right_pane(state: &mut crate::state::State, path: PathBuf, content: String) {
    let new_tile_id = spawn_in_right_pane(state, Pane::Editor { path: path.clone(), content: String::new() });
    
    // Generate the clean, relative path for the editor's UI display
    let display_path = inforno_core::realm::get_relative_path(&state.active_realm, &state.project_root, &path);
    state.editor_apps.insert(new_tile_id, bulat::BulatEditorApp::new(Some(PathBuf::from(display_path)), content));
    
    state.active_chat_id = None;
}

/// The prompt stays with the chat the merge came from.
pub fn open_merge_in_tab(state: &mut crate::state::State, path: PathBuf, left: String, right: String) {
    let new_tile_id = spawn_in_tab(state, Pane::Merge { path });
    state.merge_apps.insert(new_tile_id, bulat::DiffApp::new(left, right));
}

/// The prompt stays with the chat the merge came from.
pub fn open_merge_in_right_pane(state: &mut crate::state::State, path: PathBuf, left: String, right: String) {
    let new_tile_id = spawn_in_right_pane(state, Pane::Merge { path });
    state.merge_apps.insert(new_tile_id, bulat::DiffApp::new(left, right));
}

pub fn open_search_results_in_tab(state: &mut crate::state::State, query: String, results: Vec<inforno_core::common::SearchResult>) {
    spawn_in_tab(state, Pane::SearchResults { query, results });
    state.active_chat_id = None;
}

/// Shows ABC text in a score pane, with `selection` selected in its text. A
/// tab or window that already shows the same text is reused; otherwise a
/// new one opens at `place`.
pub fn open_score(
    state: &mut crate::state::State,
    ctx: &egui::Context,
    source: String,
    selection: Option<nulla_render::abc::datatypes::Span>,
    place: ScorePlace,
) {
    // the view of the chat this was asked from, if a popup was asked for
    let over_id = score_popup_over_id(&source);
    let over = ctx.data_mut(|d| {
        let over = d.get_temp::<egui::Rect>(over_id);
        d.remove::<egui::Rect>(over_id);
        over
    });
    let existing = state.pane_tree.tiles.iter().find_map(|(id, tile)| match tile {
        egui_tiles::Tile::Pane(Pane::Score { source: shown }) if *shown == source => Some(*id),
        _ => None,
    });
    let floating = state.floating_panes.iter().find_map(|f| match &f.pane {
        Pane::Score { source: shown } if *shown == source => Some(f.id),
        _ => None,
    });
    let tile_id = match (existing, floating) {
        (Some(id), _) => {
            state.pane_tree.make_active(|tid, _| tid == id);
            state.active_tile_id = Some(id);
            id
        }
        (None, Some(id)) => {
            ctx.move_to_top(egui::LayerId::new(egui::Order::Middle, floating_window_id(id)));
            state.active_tile_id = Some(id);
            id
        }
        (None, None) => {
            let pane = Pane::Score { source: source.clone() };
            match place {
                ScorePlace::Window => spawn_floating(state, ctx, pane, over, score_window_size(ctx)),
                ScorePlace::Tab => spawn_in_tab(state, pane),
                ScorePlace::Right => spawn_in_right_pane(state, pane),
            }
        }
    };
    if let Some(span) = selection {
        crate::score::select_in_editor(ctx, crate::score::editor_id(tile_id), &source, span);
    }
    // A score in a window is a quick look: the prompt stays with the chat.
    if place != ScorePlace::Window {
        state.active_chat_id = None;
    }
}

// --- FLOATING WINDOWS ---

/// Opens `pane` in a new floating window of `size`, centered on `over` (else
/// by the pointer) and inside the main window: moved to fit, not shrunk. Its
/// id comes from the tree, which never hands that id out again, so it can be
/// docked later.
fn spawn_floating(
    state: &mut crate::state::State,
    ctx: &egui::Context,
    pane: Pane,
    over: Option<egui::Rect>,
    size: egui::Vec2,
) -> TileId {
    let id = state.pane_tree.tiles.next_free_id();
    let screen = ctx.content_rect();
    let size = size.min(screen.size());
    let min = match over {
        Some(chat) => chat.center() - size / 2.0,
        None => ctx.input(|i| i.pointer.latest_pos()).unwrap_or(screen.min) + egui::vec2(16.0, 16.0),
    };
    let rect = egui::Rect::from_min_size(min.min(screen.max - size).max(screen.min), size);
    state.floating_panes.push(FloatingPane { id, pane, rect, home: None });
    state.active_tile_id = Some(id);
    id
}

/// A score window big enough for its text and its score side by side: 80% of
/// the main window's width, up to 1200 points, and 75% of its height, up to
/// 800 points.
fn score_window_size(ctx: &egui::Context) -> egui::Vec2 {
    let screen = ctx.content_rect().size();
    egui::vec2((screen.x * 0.8).min(1200.0), (screen.y * 0.75).min(800.0))
}

/// Takes the tab `tile_id` out of the tree into a floating window, under the
/// same id. The window starts over where the tab was, a little offset.
pub fn float_pane(tree: &mut egui_tiles::Tree<Pane>, tile_id: TileId) -> Option<FloatingPane> {
    if !matches!(tree.tiles.get(tile_id), Some(egui_tiles::Tile::Pane(_))) {
        return None;
    }
    let rect = match tree.tiles.rect(tile_id) {
        Some(r) => egui::Rect::from_min_size(
            r.min + egui::vec2(24.0, 24.0),
            r.size().min(egui::vec2(900.0, 700.0)),
        ),
        None => egui::Rect::from_min_size(egui::pos2(120.0, 120.0), egui::vec2(720.0, 480.0)),
    };
    let home = tree.tiles.parent_of(tile_id);
    match tree.remove_recursively(tile_id).pop() {
        Some(egui_tiles::Tile::Pane(pane)) => Some(FloatingPane { id: tile_id, pane, rect, home }),
        _ => None,
    }
}

/// Puts a floating pane back into the tree as the active tab, under its own
/// id: in the tab group it came from if that still exists, else the root's
/// (or any) tab group, else a new one.
pub fn dock_pane(tree: &mut egui_tiles::Tree<Pane>, floating: FloatingPane) {
    use egui_tiles::{Container, Tile};

    fn is_tabs(tree: &egui_tiles::Tree<Pane>, tid: TileId) -> bool {
        matches!(tree.tiles.get(tid), Some(Tile::Container(Container::Tabs(_))))
    }

    let FloatingPane { id, pane, home, .. } = floating;
    let target = home
        .filter(|&tid| is_tabs(tree, tid))
        .or(tree.root.filter(|&tid| is_tabs(tree, tid)))
        .or_else(|| {
            tree.tiles.iter().find_map(|(tid, tile)| {
                matches!(tile, Tile::Container(Container::Tabs(_))).then_some(*tid)
            })
        });

    tree.tiles.insert(id, Tile::Pane(pane));
    match (target, tree.root) {
        (Some(tabs_id), _) => {
            if let Some(Tile::Container(Container::Tabs(tabs))) = tree.tiles.get_mut(tabs_id) {
                tabs.add_child(id);
                tabs.set_active(id);
            }
        }
        (None, None) => tree.root = Some(tree.tiles.insert_tab_tile(vec![id])),
        (None, Some(root)) => {
            let tabs = tree.tiles.insert_tab_tile(vec![id]);
            tree.root = Some(tree.tiles.insert_horizontal_tile(vec![root, tabs]));
        }
    }
    tree.make_active(|tid, _| tid == id);
}
