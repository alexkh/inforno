#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")] // hide console window on Windows in release

use eframe::{egui, NativeOptions};
use egui::{Color32, Pos2, Rect, Vec2};
use std::{collections::BTreeMap, fs};
use similar::{Algorithm, TextDiff, DiffOp};
pub mod editor;
use editor::{CodeEditor, ColorTheme, Syntax};

/*
fn main() -> Result<(), eframe::Error> {
    // 1. Parse CLI Arguments
    let args: Vec<String> = std::env::args().collect();

    // 2. Read Files
    // Default dummy text if no files provided (for testing/development ease)
    let (left_content, right_content) = if args.len() == 3 {
        (
            fs::read_to_string(&args[1]).expect("Could not read left file"),
            fs::read_to_string(&args[2]).expect("Could not read right file"),
        )
    } else {
        (
            "fn main() {\n    println!(\"Hello World\");\n}\n".to_string(),
            "fn main() {\n    println!(\"Hello Rust\");\n    let x = 5;\n}\n".to_string(),
        )
    };

    let options = NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1200.0, 800.0])
            .with_drag_and_drop(true),
        ..Default::default()
    };

    // 3. Launch App with loaded content
    eframe::run_native(
        "Bulat",
        options,
        Box::new(|cc| {
            // Setup default fonts/styles here if needed
            Ok(Box::new(DiffApp::new(cc, left_content, right_content)))
        }),
    )
}
*/

// Stores info about a diff block to render the button
struct DiffBlock {
    op: DiffOp,
    visual_line_idx: usize,
    height_in_lines: usize,
}

pub struct DiffApp {
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
}

impl DiffApp {
    pub fn new(mut left_code: String, mut right_code: String) -> Self {
        // --- NEW LINE SANITIZATION ---
        // Ensure both files end with a newline to prevent un-mergeable
        // EOF (End of File) diffs caused by missing '\n' characters.
        if !left_code.is_empty() && !left_code.ends_with('\n') {
            left_code.push('\n');
        }
        if !right_code.is_empty() && !right_code.ends_with('\n') {
            right_code.push('\n');
        }
        // ------------------------------

        let mut app = Self {
            left_code_real: left_code,
            right_code_real: right_code,
            left_view: String::new(),
            right_view: String::new(),
            left_line_map: Vec::new(),
            right_line_map: Vec::new(),
            syntax: Syntax::rust(),
            theme: ColorTheme::SV,
            left_diff_map: BTreeMap::new(),
            right_diff_map: BTreeMap::new(),
            scroll_offset: 0.0,
            hscroll_ratio: 0.0,
            left_max_hscroll: 0.0,
            right_max_hscroll: 0.0,
            diff_blocks: Vec::new(),
        };

        // compute initial diffs
        app.recalculate_diff();
        app
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

        //let diff = TextDiff::from_lines(&self.left_code_real, &self.right_code_real)
        let diff = TextDiff::configure()
            .algorithm(Algorithm::Patience)
            .diff_lines(&self.left_code_real, &self.right_code_real);

        // Track current visual line index
        let mut visual_line_idx = 0;

        // Helper to grab slices
        let left_lines: Vec<&str> = self.left_code_real.lines().collect();
        let right_lines: Vec<&str> = self.right_code_real.lines().collect();

        for op in diff.ops() {
            match op {
                DiffOp::Equal { old_index, new_index, len } => {
                    // Just append the content
                    for i in 0..*len {
                        self.left_view.push_str(left_lines[old_index + i]);
                        self.left_view.push('\n');
                        // Map visual line to real line (1-based)
                        self.left_line_map.push(Some(old_index + i + 1));

                        self.right_view.push_str(right_lines[new_index + i]);
                        self.right_view.push('\n');
                        self.right_line_map.push(Some(new_index + i + 1));
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
                        self.left_line_map.push(Some(old_index + i + 1));
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
                        self.right_line_map.push(Some(new_index + i + 1));
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
                            self.left_line_map.push(Some(old_index + i + 1));
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
                            self.right_line_map.push(Some(new_index + i + 1));
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
        // Step 1: Split both real files into individual lines
        let left_lines: Vec<&str> = self.left_code_real.lines().collect();
        let right_lines: Vec<&str> = self.right_code_real.lines().collect();

        // Convert the left lines into owned strings so we can mutate the list
        let mut new_left: Vec<String> = left_lines.iter().map(|s| s.to_string()).collect();

        // Step 2: Apply the specific Diff Operation
        match op {
            DiffOp::Equal { .. } => return, // Nothing to do for matching lines

            DiffOp::Delete { old_index, old_len, .. } => {
                // DELETE: Remove `old_len` lines starting at `old_index` from the Left file
                new_left.drain(old_index..old_index + old_len);
            }

            DiffOp::Insert { old_index, new_index, new_len } => {
                // INSERT: Grab the new lines from the Right file
                let text_to_insert: Vec<String> = right_lines[new_index..new_index + new_len]
                    .iter()
                    .map(|s| s.to_string())
                    .collect();

                // Splice them into the Left file exactly at the `old_index` insertion point
                // (Using `old_index..old_index` means we delete 0 lines and just insert)
                new_left.splice(old_index..old_index, text_to_insert);
            }

            DiffOp::Replace { old_index, old_len, new_index, new_len } => {
                // REPLACE: Grab the replacement lines from the Right file
                let text_to_insert: Vec<String> = right_lines[new_index..new_index + new_len]
                    .iter()
                    .map(|s| s.to_string())
                    .collect();

                // Delete the old lines and insert the new ones in their place
                new_left.splice(old_index..old_index + old_len, text_to_insert);
            }
        }

        // Step 3: Rebuild the Left string
        // Note: `join` does not add a trailing newline at the very end of the file.
        self.left_code_real = new_left.join("\n");

        // Standard code editor behavior: ensure the file ends with a trailing newline
        if !self.left_code_real.ends_with('\n') && !self.left_code_real.is_empty() {
            self.left_code_real.push('\n');
        }

        // Step 4: Recalculate the diff!
        // This generates fresh, accurate buttons and indices for the next user action.
        self.recalculate_diff();
    }

    // Extracts the real code from a padded view by removing gap lines
    fn extract_real_code(view: &str) -> String {
        let mut real = String::new();
        let lines: Vec<&str> = view.split('\n').collect();

        let mut is_first = true;
        for line in lines {
            // If the line is exactly our invisible gap marker, ignore it
            if line == "\u{200B}" {
                continue;
            }

            if !is_first {
                real.push('\n');
            }
            // Strip the marker in case the user manually typed text INTO a gap line
            real.push_str(&line.replace('\u{200B}', ""));
            is_first = false;
        }
        real
    }

    pub fn show(&mut self, ui: &mut egui::Ui) {
        let mut left_changed = false;
        let mut right_changed = false;
        let row_height = 12.0;

        // prepare the horizontal offset we will apply next frame
        let mut next_hscroll_ratio = self.hscroll_ratio;

        // 1. Single Outer ScrollArea for everything
        egui::ScrollArea::vertical()
            .id_salt("global_scroll")
            .show(ui, |ui| {

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
                            ui.heading("File 1 (Filesystem)");
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui.button("🗐").on_hover_text("Copy original file").clicked() {
                                    ui.ctx().copy_text(self.left_code_real.clone());
                                }
                            });
                        });

                        let expected_left_offset = self.hscroll_ratio * self.left_max_hscroll;

                        let left_out = CodeEditor::default()
                            .id_source("left_editor")
                            .with_rows(self.left_line_map.len()) // Grow to fit content
                            //.with_fontsize(14.0)
                            .with_row_height(row_height)
                            .with_theme(self.theme)
                            .with_syntax(self.syntax.clone())
                            .vscroll(false) // IMPORTANT: No internal scroll
                            .with_diff(self.left_diff_map.clone())
                            .with_line_numbers(self.left_line_map.clone())
                            // Optional but good practice: tell the editor its desired width
                            .desired_width(side_width)
                            .with_hscroll_offset(expected_left_offset)
                            .show(ui, &mut self.left_view);

                        if left_out.output.response.changed() {
                            left_changed = true;
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
                            ui.heading("File 2 (Diff)");
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui.button("🗐").on_hover_text("Copy modified file").clicked() {
                                    ui.ctx().copy_text(self.right_code_real.clone());
                                }
                            });
                        });

                        let expected_right_offset = self.hscroll_ratio * self.right_max_hscroll;

                        let right_out = CodeEditor::default()
                            .id_source("right_editor")
                            .with_rows(self.right_line_map.len())
                            //.with_fontsize(14.0)
                            .with_row_height(row_height)
                            .with_theme(self.theme)
                            .with_syntax(self.syntax.clone())
                            .vscroll(false) // IMPORTANT: No internal scroll
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
    }
}