pub mod engine;
use std::path::PathBuf;

#[cfg(feature = "gui")]
pub mod editor;

#[cfg(feature = "gui")]
use eframe::egui;
#[cfg(feature = "gui")]
use egui::{Color32, Pos2, Rect, Vec2};
#[cfg(feature = "gui")]
use std::collections::BTreeMap;
#[cfg(feature = "gui")]
use similar::DiffOp;
#[cfg(feature = "gui")]
use editor::{CodeEditor, ColorTheme, Syntax};
#[cfg(feature = "gui")]
use engine::BulatEngine;

// Stores info about a diff block to render the button
#[cfg(feature = "gui")]
#[derive(Clone)]
struct DiffBlock {
    op: DiffOp,
    visual_line_idx: usize,
    height_in_lines: usize,
}

#[cfg(feature = "gui")]
#[derive(serde::Serialize, serde::Deserialize, Default, Clone)]
pub struct BulatConfig {
    pub theme: Option<String>,
}

#[cfg(feature = "gui")]
pub fn config_path() -> Option<std::path::PathBuf> {
    directories::ProjectDirs::from("", "", "bulat")
        .map(|proj_dirs| proj_dirs.config_dir().join("global.yml"))
}

#[cfg(feature = "gui")]
pub fn load_config() -> BulatConfig {
    if let Some(path) = config_path() {
        if let Ok(content) = std::fs::read_to_string(&path) {
            if let Ok(config) = serde_yaml::from_str::<BulatConfig>(&content) {
                return config;
            }
        }
    }
    BulatConfig::default()
}

#[cfg(feature = "gui")]
pub fn save_config(theme_name: &str) {
    if let Some(path) = config_path() {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let config = BulatConfig {
            theme: Some(theme_name.to_string()),
        };
        if let Ok(yaml) = serde_yaml::to_string(&config) {
            let _ = std::fs::write(path, yaml);
        }
    }
}

#[cfg(feature = "gui")]
pub fn render_settings_window(ctx: &egui::Context, show_settings: &mut bool, current_theme: &mut editor::ColorTheme) {
    let mut show = *show_settings;
    if show {
        egui::Window::new("🔧 Editor Configuration")
            .collapsible(false)
            .resizable(false)
            .open(&mut show)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label("Theme:");
                    egui::ComboBox::from_id_salt("global_theme_selector")
                        .selected_text(current_theme.name())
                        .show_ui(ui, |ui| {
                            for theme in editor::ColorTheme::available_themes() {
                                if ui.selectable_value(current_theme, *theme, theme.name()).changed() {
                                    if theme.is_dark() {
                                        ctx.set_visuals(egui::Visuals::dark());
                                    } else {
                                        ctx.set_visuals(egui::Visuals::light());
                                    }
                                    save_config(theme.name());
                                }
                            }
                        });
                });
            });
    }
    *show_settings = show;
}

#[cfg(feature = "gui")]
pub fn render_search_bar(ui: &mut egui::Ui, state_id: egui::Id, request_search_focus: bool) {
    let search_edit_id = state_id.with("edit");
    let mut search_state = ui.ctx().data_mut(|d| d.get_temp::<editor::CodeEditorState>(state_id).unwrap_or_default());

    let mut next = false;
    let mut prev = false;
    let mut changed = false;

    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
        ui.spacing_mut().item_spacing.x = 2.0;

        let response = ui.add(
            egui::TextEdit::singleline(&mut search_state.search_term)
                .id_source(search_edit_id)
                .desired_width(120.0)
                .hint_text("🔍 Search...")
        );

        if request_search_focus {
            response.request_focus();
        }

        changed = response.changed();

        if response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
            next = true;
            response.request_focus();
        }

        if !search_state.search_term.is_empty() {
            if search_state.match_count > 0 {
                ui.label(egui::RichText::new(format!(" {}/{} ", search_state.current_match + 1, search_state.match_count)).color(egui::Color32::GRAY));
            } else {
                ui.label(egui::RichText::new(" 0/0 ").color(egui::Color32::GRAY));
            }

            if ui.button("↑").clicked() { prev = true; }
            if ui.button("↓").clicked() { next = true; }

            if ui.button("✖").clicked() {
                search_state.search_term.clear();
                changed = true;
            }
        } else {
            ui.label(" "); // Empty space to keep layout stable
        }
        ui.add_space(8.0);
    });

    if changed {
        search_state.current_match = 0;
    } else if next {
        search_state.current_match = search_state.current_match.saturating_add(1);
    } else if prev {
        search_state.current_match = search_state.current_match.checked_sub(1).unwrap_or(search_state.match_count.saturating_sub(1));
    }

    if search_state.match_count > 0 {
        search_state.current_match %= search_state.match_count;
    }

    search_state.scroll_to_match = changed || next || prev;
    search_state.match_count = 0; // Reset so editors can re-accumulate this frame

    ui.ctx().data_mut(|d| d.insert_temp(state_id, search_state));
}

#[cfg(feature = "gui")]
#[derive(Clone)]
pub struct EditorViewState {
    pub language_override: Option<String>,
    pub is_dirty: bool,
    pub show_settings: bool,
    pub theme: editor::ColorTheme,
}

#[cfg(feature = "gui")]
impl Default for EditorViewState {
    fn default() -> Self {
        let config = load_config();
        let mut theme = editor::ColorTheme::default();
        if let Some(t_name) = config.theme {
            if let Some(t) = editor::ColorTheme::available_themes().iter().find(|t| t.name() == t_name) {
                theme = *t;
            }
        }
        Self { language_override: None, is_dirty: false, show_settings: false, theme }
    }
}

/// Natively embeds the full UI capability of the text editor without storing permanent structs.
#[cfg(feature = "gui")]
pub fn show_editor(
    ui: &mut egui::Ui,
    id: egui::Id,
    filepath: Option<&str>,
    title_override: Option<&str>,
    code: &mut String,
    hide_save: bool,
) -> bool {
    let mut view_state = ui.ctx().data_mut(|d| d.get_temp::<EditorViewState>(id).unwrap_or_default());
    let mut save_requested = false;

    if !hide_save {
        save_requested = ui.input_mut(|i| i.consume_shortcut(&egui::KeyboardShortcut::new(egui::Modifiers::COMMAND, egui::Key::S)));
    }
    let request_search_focus = ui.input_mut(|i| i.consume_shortcut(&egui::KeyboardShortcut::new(egui::Modifiers::COMMAND, egui::Key::F)));

    render_settings_window(ui.ctx(), &mut view_state.show_settings, &mut view_state.theme);

    ui.horizontal(|ui| {
        if ui.button("🔧").on_hover_text("Open Settings").clicked() {
            view_state.show_settings = true;
        }

        let current_mime = view_state.language_override.clone().unwrap_or_else(|| {
            filepath
                .map(|p| editor::Syntax::guess_mime_from_path(std::path::Path::new(p)))
                .unwrap_or("text/plain")
                .to_string()
        });

        let dirty_marker = if view_state.is_dirty && !hide_save { "*" } else { "" };

        if let Some(title) = title_override {
            ui.heading(format!("{}{}", title, dirty_marker));
        } else {
            let display_path = filepath.unwrap_or("New File");
            ui.heading(format!("Editing: {}{}", display_path, dirty_marker));
        }

        egui::ComboBox::from_id_salt(id.with("mime_type_selector"))
            .selected_text(&current_mime)
            .show_ui(ui, |ui| {
                let supported_mimes = [
                    "text/plain", "text/rust", "text/x-c", "text/x-c++",
                    "application/x-rhai", "text/markdown", "application/json",
                    "application/toml", "application/yaml", "text/javascript",
                    "text/typescript", "text/x-python", "text/html", "text/css",
                    "application/x-sh"
                ];
                for &mime_opt in &supported_mimes {
                    if ui.selectable_label(current_mime == mime_opt, mime_opt).clicked() {
                        view_state.language_override = Some(mime_opt.to_string());
                    }
                }
            });

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if !hide_save && filepath.is_some() {
                let save_btn = egui::Button::new("💾 Save");
                if view_state.is_dirty {
                    egui::Stroke::new(1.0_f32, ui.visuals().warn_fg_color);
                }
                if ui.add(save_btn).clicked() {
                    save_requested = true;
                }
            }

            render_search_bar(ui, id.with("search"), request_search_focus);
        });
    });
    ui.separator();

    let active_mime = view_state.language_override.clone().unwrap_or_else(|| {
        filepath
            .map(|p| editor::Syntax::guess_mime_from_path(std::path::Path::new(p)))
            .unwrap_or("text/plain")
            .to_string()
    });

    let syntax = editor::Syntax::get_or_load(ui.ctx(), &active_mime);

    let editor_output = editor::CodeEditor::default()
        .id_source(format!("{:?}", id.with("editor")))
        .with_theme(view_state.theme)
        .with_syntax(syntax)
        .with_numlines(true)
        .vscroll(true)
        .v_auto_shrink(false)
        .with_search_state_id(id.with("search"))
        .show(ui, code);

    if editor_output.output.response.changed() {
        view_state.is_dirty = true;
    }
    if save_requested {
        view_state.is_dirty = false;
    }

    ui.ctx().data_mut(|d| d.insert_temp(id, view_state));

    save_requested
}

#[cfg(feature = "gui")]
#[derive(Clone)]
pub struct DiffApp {
    pub left_filepath: Option<String>,
    pub right_filepath: Option<String>,
    pub language_override: Option<String>,
    pub show_settings: bool,

    // The "True" content of the files
    pub left_code_real: String,
    pub right_code_real: String,

    // The "View" content (padded with gaps for visual alignment)
    left_view: String,
    right_view: String,

    // Mapping from View Index -> Real Line Number (1-based)
    left_line_map: Vec<Option<usize>>,
    right_line_map: Vec<Option<usize>>,

    syntax: Syntax,
    theme: ColorTheme,

    // Background highlight maps
    left_diff_map: BTreeMap<usize, Color32>,
    right_diff_map: BTreeMap<usize, Color32>,

    scroll_offset: f32,
    hscroll_ratio: f32,
    left_max_hscroll: f32,
    right_max_hscroll: f32,

    // NEW: List of diff blocks to render buttons
    diff_blocks: Vec<DiffBlock>,
    calculated_row_height: f32,
    pub search_state_id: Option<egui::Id>,
    pub embedded: bool,
    pub line_offset: usize,
}

#[cfg(feature = "gui")]
impl DiffApp {
    pub fn new(mut left_code: String, mut right_code: String) -> Self {

        // --- NEW LINE SANITIZATION ---
        // This prevents egui's selection ranges from desyncing by 1 byte per line!
        left_code = left_code.replace('\r', "");
        right_code = right_code.replace('\r', "");

        // Ensure both files end with a newline to prevent un-mergeable
        // EOF (End of File) diffs caused by missing '\n' characters.
        if !left_code.is_empty() && !left_code.ends_with('\n') {
            left_code.push('\n');
        }
        if !right_code.is_empty() && !right_code.ends_with('\n') {
            right_code.push('\n');
        }
        // ------------------------------

        let config = load_config();
        let mut theme = editor::ColorTheme::default();
        if let Some(t_name) = config.theme {
            if let Some(t) = editor::ColorTheme::available_themes().iter().find(|t| t.name() == t_name) {
                theme = *t;
            }
        }

        let mut app = Self {
            left_filepath: None,
            right_filepath: None,
            language_override: None,
            show_settings: false,
            left_code_real: left_code,
            right_code_real: right_code,
            left_view: String::new(),
            right_view: String::new(),
            left_line_map: Vec::new(),
            right_line_map: Vec::new(),
            syntax: editor::Syntax::rust(),
            theme,
            left_diff_map: std::collections::BTreeMap::new(),
            right_diff_map: BTreeMap::new(),
            scroll_offset: 0.0,
            hscroll_ratio: 0.0,
            left_max_hscroll: 0.0,
            right_max_hscroll: 0.0,
            diff_blocks: Vec::new(),
            calculated_row_height: 14.0,
            search_state_id: None,
            embedded: false,
            line_offset: 0,
        };

        // compute initial diffs
        app.recalculate_diff();

        app
    }

    pub fn with_line_offset(mut self, offset: usize) -> Self {
        self.line_offset = offset;
        self.recalculate_diff();
        self
    }

    pub fn set_theme(&mut self, theme: ColorTheme) {
        self.theme = theme;
    }

    /// Uses the `similar` crate to compare text and populate the highlight maps
    fn recalculate_diff(&mut self) {
        self.left_diff_map.clear();
        self.right_diff_map.clear();
        self.left_view.clear();
        self.right_view.clear();
        self.left_line_map.clear();
        self.right_line_map.clear();
        self.diff_blocks.clear();

        // Colors
        let color_diff_add = Color32::from_rgba_premultiplied(0, 40, 0, 255);      // Greenish
        let color_diff_del = Color32::from_rgba_premultiplied(30, 0, 0, 255);      // Reddish
        let color_diff_change = Color32::from_rgba_premultiplied(0, 0, 40, 255);   // Bluish
        let color_gap = Color32::from_rgb(25, 25, 25); // Dark Grey for the "Void" gaps

        // DELEGATE TO ENGINE:
        let ops = BulatEngine::compute_diffs(&self.left_code_real, &self.right_code_real);

        // Track current visual line index
        let mut visual_line_idx = 0;

        // Helper to grab slices
        let left_lines: Vec<&str> = self.left_code_real.lines().collect();
        let right_lines: Vec<&str> = self.right_code_real.lines().collect();

        for op in ops.iter() {
            match op {
                DiffOp::Equal { old_index, new_index, len } => {
                    // Just append the content
                    for i in 0..*len {
                        self.left_view.push_str(left_lines[old_index + i]);
                        self.left_view.push('\n');
                        // Map visual line to real line (1-based)
                        self.left_line_map.push(Some(self.line_offset + old_index + i + 1));

                        self.right_view.push_str(right_lines[new_index + i]);
                        self.right_view.push('\n');
                        self.right_line_map.push(Some(self.line_offset + new_index + i + 1));
                    }
                    visual_line_idx += len;
                }
                DiffOp::Delete { old_index, old_len, .. } => {
                    // Store block info
                    self.diff_blocks.push(DiffBlock {
                        op: op.clone(),
                        visual_line_idx,
                        height_in_lines: *old_len,
                    });

                    for i in 0..*old_len {
                        // Left Side (Real content)
                        self.left_view.push_str(left_lines[old_index + i]);
                        self.left_view.push('\n');
                        self.left_line_map.push(Some(self.line_offset + old_index + i + 1));
                        self.left_diff_map.insert(visual_line_idx + i, color_diff_del);

                        // Right Side (Gap)
                        self.right_view.push_str("\u{200B}\n");
                        self.right_line_map.push(None); // No line number
                        self.right_diff_map.insert(visual_line_idx + i, color_gap);
                    }
                    visual_line_idx += old_len;
                }
                DiffOp::Insert { new_index, new_len, .. } => {
                    self.diff_blocks.push(DiffBlock {
                        op: op.clone(),
                        visual_line_idx,
                        height_in_lines: *new_len,
                    });

                    for i in 0..*new_len {
                        // Left Side (Gap)
                        self.left_view.push_str("\u{200B}\n");
                        self.left_line_map.push(None);
                        self.left_diff_map.insert(visual_line_idx + i, color_gap);

                        // Right Side (Real Content)
                        self.right_view.push_str(right_lines[new_index + i]);
                        self.right_view.push('\n');
                        self.right_line_map.push(Some(self.line_offset + new_index + i + 1));
                        self.right_diff_map.insert(visual_line_idx + i, color_diff_add);
                    }
                    visual_line_idx += new_len;
                }
                DiffOp::Replace { old_index, old_len, new_index, new_len } => {
                    // Content exists on both sides, but lengths might differ.
                    // We need to pad the shorter one to match the longer one.
                    let max_len = std::cmp::max(old_len, new_len);

                    self.diff_blocks.push(DiffBlock {
                        op: op.clone(),
                        visual_line_idx,
                        height_in_lines: *max_len,
                    });

                    for i in 0..*max_len {
                        // LEFT Processing
                        if i < *old_len {
                            self.left_view.push_str(left_lines[old_index + i]);
                            self.left_view.push('\n');
                            self.left_line_map.push(Some(self.line_offset + old_index + i + 1));
                            self.left_diff_map.insert(visual_line_idx + i, color_diff_change);
                        } else {
                            // Pad Left
                            self.left_view.push_str("\u{200B}\n");
                            self.left_line_map.push(None);
                            self.left_diff_map.insert(visual_line_idx + i, color_gap);
                        }

                        // RIGHT Processing
                        if i < *new_len {
                            self.right_view.push_str(right_lines[new_index + i]);
                            self.right_view.push('\n');
                            self.right_line_map.push(Some(self.line_offset + new_index + i + 1));
                            self.right_diff_map.insert(visual_line_idx + i, color_diff_change);
                        } else {
                            // Pad Right
                            self.right_view.push_str("\u{200B}\n");
                            self.right_line_map.push(None);
                            self.right_diff_map.insert(visual_line_idx + i, color_gap);
                        }
                    }
                    visual_line_idx += max_len;
                }
            }
        }
    }

    // --- MERGE LOGIC ---

    fn apply_merge(&mut self, op: DiffOp) {
        // DELEGATE TO PURE ENGINE:
        self.left_code_real = BulatEngine::apply_merge(&self.left_code_real, &self.right_code_real, &op);

        // Recalculate the diff!
        self.recalculate_diff();
    }

    // Extracts the real code from a padded view by removing gap lines
    fn extract_real_code(view: &str) -> String {
        BulatEngine::extract_real_code(view)
    }

    pub fn show(&mut self, ui: &mut egui::Ui) -> (bool, bool) {
        let mut save_left = false;
        let mut save_right = false;

        render_settings_window(ui.ctx(), &mut self.show_settings, &mut self.theme);
        let request_search_focus = ui.input_mut(|i| i.consume_shortcut(&egui::KeyboardShortcut::new(egui::Modifiers::COMMAND, egui::Key::F)));

        ui.horizontal(|ui| {
            if ui.button("🔧").on_hover_text("Open Settings").clicked() {
                self.show_settings = true;
            }

            let display_path = self.left_filepath.as_deref().or(self.right_filepath.as_deref()).unwrap_or("Diff Merge");
            ui.heading(display_path);

            let current_mime = self.language_override.clone().unwrap_or_else(|| {
                self.left_filepath.as_ref().or(self.right_filepath.as_ref())
                    .map(|p| editor::Syntax::guess_mime_from_path(std::path::Path::new(p)))
                    .unwrap_or("text/plain")
                    .to_string()
            });

            egui::ComboBox::from_id_salt(ui.id().with("diff_mime_type_selector"))
                .selected_text(&current_mime)
                .show_ui(ui, |ui| {
                    let supported_mimes = [
                        "text/plain", "text/rust", "text/x-c", "text/x-c++",
                        "application/x-rhai", "text/markdown", "application/json",
                        "application/toml", "application/yaml", "text/javascript",
                        "text/typescript", "text/x-python", "text/html", "text/css",
                        "application/x-sh"
                    ];
                    for &mime_opt in &supported_mimes {
                        if ui.selectable_label(current_mime == mime_opt, mime_opt).clicked() {
                            self.language_override = Some(mime_opt.to_string());
                        }
                    }
                });

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if self.right_filepath.is_some() {
                    if ui.button("💾 Save Right").clicked() { save_right = true; }
                }
                if self.left_filepath.is_some() {
                    if ui.button("💾 Save Left").clicked() { save_left = true; }
                }

                let search_id = self.search_state_id.unwrap_or_else(|| ui.id().with("search"));
                render_search_bar(ui, search_id, request_search_focus);
            });
        });
        ui.separator();

        let active_mime = self.language_override.clone().unwrap_or_else(|| {
            self.left_filepath.as_ref().or(self.right_filepath.as_ref())
                .map(|p| editor::Syntax::guess_mime_from_path(std::path::Path::new(p)))
                .unwrap_or("text/plain")
                .to_string()
        });
        self.syntax = editor::Syntax::get_or_load(ui.ctx(), &active_mime);

        let mut left_changed = false;
        let mut right_changed = false;
        let row_height = self.calculated_row_height;

        // prepare the horizontal offset we will apply next frame
        let mut next_hscroll_ratio = self.hscroll_ratio;

        // 1. Single Outer ScrollArea for everything
        // When embedded, we completely disable vertical scrolling. This naturally
        // removes the scrollbar and forces the container to expand to its full content height.
        egui::ScrollArea::new([false, !self.embedded])
            .id_salt("global_scroll")
            .min_scrolled_height(0.0)
            .show(ui, |ui| {

                let has_focus = ui.memory(|m| m.focused().is_some());
                let pointer_pos = ui.input(|i| i.pointer.hover_pos());
                let is_hovered = pointer_pos.map_or(false, |pos| ui.clip_rect().contains(pos));

                if is_hovered && !has_focus {
                    let mut page_up = false;
                    let mut page_down = false;
                    ui.input_mut(|i| {
                        if i.consume_key(egui::Modifiers::NONE, egui::Key::PageUp) {
                            page_up = true;
                        }
                        if i.consume_key(egui::Modifiers::NONE, egui::Key::PageDown) {
                            page_down = true;
                        }
                    });

                    if page_up || page_down {
                        let height = ui.ctx().content_rect().height() * 0.8;
                        let shift = if page_up { -height } else { height };
                        let target_rect = ui.clip_rect().translate(egui::vec2(0.0, shift));
                        ui.scroll_to_rect(target_rect, None);
                    }
                }

                // Set our spacing FIRST so we know exactly what we are dealing with
                ui.spacing_mut().item_spacing.x = 5.0;

                // Calculate the EXACT overhead.
                // We have 5 elements in the horizontal layout, which means 4 gaps of 5.0 (20.0 total).
                // We have two manual ui.add_space() calls of 5.0 and 1.0 (6.0 total).
                // We have the middle column (20.0).
                // Total fixed overhead: 20.0 + 6.0 + 20.0 = 46.0
                let middle_width = 20.0;
                let fixed_overhead = 46.0;

                // Subtract overhead, divide by 2, and floor() it to prevent rounding loops
                let side_width = ((ui.available_width() - fixed_overhead) / 2.0).max(50.0).floor();

                // 2. Setup the Horizontal Layout (Replacing the Grid)
                ui.horizontal_top(|ui| {

                    // --- COLUMN 1: LEFT EDITOR ---
                    ui.vertical(|ui| {
                        // Enforce the width of this column
                        ui.set_min_width(side_width);
                        ui.set_max_width(side_width);
                        ui.horizontal(|ui| {
                            ui.heading(self.left_filepath.as_deref().unwrap_or("File 1 (Left)"));
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui.button("🗐").on_hover_text("Copy original file").clicked() {
                                    ui.ctx().copy_text(self.left_code_real.clone());
                                }
                            });
                        });

                        let expected_left_offset = self.hscroll_ratio * self.left_max_hscroll;

                        let mut left_editor = CodeEditor::default()
                            .id_source("left_editor")
                            .with_rows(self.left_line_map.len());

                        if let Some(id) = self.search_state_id {
                            left_editor = left_editor.with_search_state_id(id);
                        }

                        let left_out = left_editor
                            //.with_fontsize(14.0)
                            .with_row_height(row_height)
                            .with_theme(self.theme)
                            .with_syntax(self.syntax.clone())
                            .vscroll(false) // IMPORTANT: No internal scroll
                            .v_auto_shrink(self.embedded) // Let the inner editor shrink dynamically
                            .with_diff(self.left_diff_map.clone())
                            .with_line_numbers(self.left_line_map.clone())
                            // Optional but good practice: tell the editor its desired width
                            .desired_width(side_width)
                            .with_hscroll_offset(expected_left_offset)
                            .show(ui, &mut self.left_view);

                        if left_out.output.response.changed() {
                            left_changed = true;
                        }

                        // Now that the text is laid out, let's measure its exact bounds.
                        let total_lines = self.left_line_map.len() as f32;
                        if total_lines > 0.0 {
                            // galley.rect.height() gives the exact pixel height of the text block
                            let measured_total_height = left_out.output.galley.rect.height();
                            // we add 1 to total_lines because of extra newline at the end
                            let measured_row_height = measured_total_height / (total_lines + 1.0);

                            // If our guess differs from reality by more than a tiny fraction of a pixel,
                            // update it and immediately ask egui to draw the next frame.
                            if (self.calculated_row_height - measured_row_height).abs() > 0.05 {
                                self.calculated_row_height = measured_row_height;
                                ui.ctx().request_repaint();
                                // println!("Calculated row height: {}", self.calculated_row_height);
                            }
                        }

                        // 2. Save the max width for the next frame's calculation
                        self.left_max_hscroll = left_out.max_hscroll_offset;

                        // 3. Did the user scroll? (Using > 1.0 to ignore float precision noise)
                        if (left_out.hscroll_offset - expected_left_offset).abs() > 1.0 {
                            if left_out.max_hscroll_offset > 0.0 {
                                // Calculate the new global ratio based on user's manual scroll
                                next_hscroll_ratio = left_out.hscroll_offset / left_out.max_hscroll_offset;
                            }
                        }
                    });

                    // Add spacing between Column 1 and Column 2
                    ui.add_space(5.0);

                    // --- COLUMN 2: MERGE ACTIONS ---
                    ui.vertical(|ui| {
                        // Enforce the smaller width for the buttons column
                        ui.set_min_width(middle_width);
                        ui.set_max_width(middle_width);

                        ui.add_sized([middle_width, 20.0], egui::Label::new(" "));

                        // Allocate a space that matches the editors' height
                        let total_height = self.left_line_map.len() as f32 * row_height;
                        let (rect, _) = ui.allocate_at_least(
                            Vec2::new(middle_width, total_height),
                            egui::Sense::hover()
                        );

                        let mut action_to_perform = None;

                        for block in &self.diff_blocks {
                            let y_pos = rect.min.y + (block.visual_line_idx as f32 * row_height);
                            let block_height = block.height_in_lines as f32 * row_height;

                            // Center button in the vertical block of the diff
                            let button_rect = Rect::from_center_size(
                                Pos2::new(rect.center().x, y_pos + (block_height / 2.0)),
                                Vec2::new(24.0, block_height)
                            );

                            ui.put(button_rect, |ui: &mut egui::Ui| {
                                let (label, color) = match block.op {
                                    DiffOp::Delete { .. } => ("❌", Color32::DARK_RED),
                                    DiffOp::Insert { .. } => ("⬅", Color32::DARK_GREEN),
                                    DiffOp::Replace { .. } => ("⬅", Color32::DARK_BLUE),
                                    _ => ("", Color32::TRANSPARENT),
                                };

                                if !label.is_empty() {
                                    if ui.add(egui::Button::new(label).fill(color)).clicked() {
                                        action_to_perform = Some(block.op.clone());
                                    }
                                }
                                ui.response()
                            });
                        }

                        if let Some(op) = action_to_perform {
                            self.apply_merge(op);
                        }
                    });

                    // Add spacing between Column 2 and Column 3
                    ui.add_space(1.0);

                    // --- COLUMN 3: RIGHT EDITOR ---
                    ui.vertical(|ui| {
                        // Enforce the width of this column
                        ui.set_min_width(side_width);
                        ui.set_max_width(side_width);
                        ui.horizontal(|ui| {
                            ui.heading(self.right_filepath.as_deref().unwrap_or("File 2 (Right)"));
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui.button("🗐").on_hover_text("Copy modified file").clicked() {
                                    ui.ctx().copy_text(self.right_code_real.clone());
                                }
                            });
                        });

                        let expected_right_offset = self.hscroll_ratio * self.right_max_hscroll;

                        let mut right_editor = CodeEditor::default()
                            .id_source("right_editor")
                            .with_rows(self.right_line_map.len());

                        if let Some(id) = self.search_state_id {
                            right_editor = right_editor.with_search_state_id(id);
                        }

                        let right_out = right_editor
                            //.with_fontsize(14.0)
                            .with_row_height(row_height)
                            .with_theme(self.theme)
                            .with_syntax(self.syntax.clone())
                            .vscroll(false) // IMPORTANT: No internal scroll
                            .v_auto_shrink(self.embedded) // Let the inner editor shrink dynamically
                            .with_diff(self.right_diff_map.clone())
                            .with_line_numbers(self.right_line_map.clone())
                            .desired_width(side_width)
                            .with_hscroll_offset(expected_right_offset)
                            .show(ui, &mut self.right_view);

                        if right_out.output.response.changed() {
                            right_changed = true;
                        }

                        // 2. Save the max width
                        self.right_max_hscroll = right_out.max_hscroll_offset;

                        // 3. Did the user scroll?
                        if (right_out.hscroll_offset - expected_right_offset).abs() > 1.0 {
                            if right_out.max_hscroll_offset > 0.0 {
                                next_hscroll_ratio = right_out.hscroll_offset / right_out.max_hscroll_offset;
                            }
                        }
                    });
                });
        });

        self.hscroll_ratio = next_hscroll_ratio.clamp(0.0, 1.0);

        // Apply manual text edits if they occurred
        if left_changed {
            self.left_code_real = Self::extract_real_code(&self.left_view);
        }
        if right_changed {
            self.right_code_real = Self::extract_real_code(&self.right_view);
        }
        if left_changed || right_changed {
            self.recalculate_diff();
        }

        (save_left, save_right)
    }
}

pub enum EditorAction {
    SaveRequested,
    ThemeChanged(ColorTheme),
    // Add other events you need the host to know about
}

pub struct BulatEditorApp {
    pub filepath: Option<PathBuf>,
    pub content: String,
    pub search_term: String,
    pub show_settings: bool,
    pub current_theme: crate::editor::ColorTheme,
    pub active_mime: Option<String>,
    pub is_dirty: bool,
}

impl BulatEditorApp {
    pub fn new(filepath: Option<PathBuf>, content: String) -> Self {
        let config = load_config();
        let mut active_theme = crate::editor::ColorTheme::default(); // Defaults to SV

        // Load the globally saved theme from global.yml
        if let Some(t_name) = config.theme {
            if let Some(t) = crate::editor::themes::DEFAULT_THEMES.iter().find(|t| t.name() == t_name) {
                active_theme = t.clone();
            }
        }

        Self {
            filepath,
            content,
            search_term: String::new(),
            show_settings: false,
            current_theme: active_theme,
            active_mime: None,
            is_dirty: false,
        }
    }

    pub fn show(&mut self, ui: &mut egui::Ui) -> Option<EditorAction> {
        let mut action = None;

        ui.horizontal(|ui| {
            // 1. Wrench Menu (Left)
            if ui.button("🔧").clicked() {
                self.show_settings = !self.show_settings;
            }

            // 2. File Path
            ui.label(self.filepath.as_ref().map(|p| p.display().to_string()).unwrap_or_else(|| "New File".into()));

            // 3. Language/MIME Selector
            let current_mime = self.active_mime.clone().unwrap_or_else(|| {
                self.filepath.as_ref()
                    .map(|p| crate::editor::Syntax::guess_mime_from_path(p))
                    .unwrap_or("text/plain")
                    .to_string()
            });

            egui::ComboBox::from_id_salt("mime_selector")
                .selected_text(&current_mime)
                .show_ui(ui, |ui| {
                    let supported_mimes = [
                        "text/plain", "text/rust", "text/x-c", "text/x-c++",
                        "application/x-rhai", "text/markdown", "application/json",
                        "application/toml", "application/yaml", "text/javascript",
                        "text/typescript", "text/x-python", "text/html", "text/css",
                        "application/x-sh"
                    ];
                    for &mime_opt in &supported_mimes {
                        ui.selectable_value(&mut self.active_mime, Some(mime_opt.to_string()), mime_opt);
                    }
                });

            // 4. Search and Save (Right-to-Left)
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let mut save_btn = egui::Button::new("💾 Save");
                if self.is_dirty {
                    save_btn = save_btn.stroke(egui::Stroke::new(1.0, ui.visuals().warn_fg_color));
                }

                if ui.add(save_btn).clicked() {
                    self.is_dirty = false;
                    action = Some(EditorAction::SaveRequested);
                }

                // Embed the exact same render_search_bar logic here, but using `self.search_term`
                // self.render_search_bar(ui);
            });
        });

        ui.separator();

        // 5. Render Settings Window if active
        let mut close_settings = false;
        if self.show_settings {
            egui::Window::new("Editor Settings")
                .open(&mut self.show_settings)
                .collapsible(false)
                .resizable(false)
                .show(ui.ctx(), |ui| {
                    ui.horizontal(|ui| {
                        ui.label("Theme:");
                        egui::ComboBox::from_id_salt("theme_selector")
                            .selected_text(self.current_theme.name())
                            .show_ui(ui, |ui| {
                                for theme in crate::editor::themes::DEFAULT_THEMES.iter() {
                                    if ui.selectable_value(&mut self.current_theme, theme.clone(), theme.name()).clicked() {
                                        save_config(theme.name());
                                        action = Some(EditorAction::ThemeChanged(theme.clone()));
                                    }
                                }
                            });
                    });
                    ui.add_space(8.0);
                    if ui.button("Close").clicked() {
                        close_settings = true;
                    }
                });
        }
        if close_settings {
            self.show_settings = false;
        }

        // 6. Render the actual CodeEditor below
        let available_width = ui.available_width();

        // Use the requested MIME type to load the correct syntax rules
        let active_mime = self.active_mime.clone().unwrap_or_else(|| {
            self.filepath.as_ref()
                .map(|p| crate::editor::Syntax::guess_mime_from_path(p))
                .unwrap_or("text/plain")
                .to_string()
        });

        let syntax = crate::editor::Syntax::get_or_load(ui.ctx(), &active_mime);

        let editor_output = crate::editor::CodeEditor::default()
            .with_search_state_id(egui::Id::new("bulat_search").with(ui.id()))
            .id_source(format!("{:?}", ui.id().with("bulat_editor")))
            .with_theme(self.current_theme.clone())
            .with_syntax(syntax)
            .vscroll(true)
            .v_auto_shrink(false)
            .desired_width(available_width)
            .show(ui, &mut self.content);

        // 7. Track modifications
        if editor_output.output.response.changed() {
            self.is_dirty = true;
        }

        // 8. Restore Ctrl+S functionality
        if ui.input_mut(|i| i.consume_shortcut(&egui::KeyboardShortcut::new(egui::Modifiers::COMMAND, egui::Key::S))) {
            self.is_dirty = false;
            action = Some(EditorAction::SaveRequested);
        }

        action
    }
}
