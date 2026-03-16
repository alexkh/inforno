#![allow(rustdoc::invalid_rust_codeblocks)]
//! Text Editor Widget for [egui](https://github.com/emilk/egui) with numbered lines and simple syntax highlighting based on keywords sets.
//!
//! ## Usage with egui
//!
//! ```rust
//! use egui_code_editor::{CodeEditor, ColorTheme, Syntax};
//!
//! CodeEditor::default()
//!   .id_source("code editor")
//!   .with_rows(12)
//!   .with_fontsize(14.0)
//!   .with_theme(ColorTheme::GRUVBOX)
//!   .with_syntax(Syntax::rust())
//!   .with_numlines(true)
//!   .show(ui, &mut self.code);
//! ```
//!
//! ## Usage as lexer without egui
//!
//! **Cargo.toml**
//!
//! ```toml
//! [dependencies]
//! egui_code_editor = { version = "0.2" , default-features = false }
//! colorful = "0.2.2"
//! ```
//!
//! **main.rs**
//!
//! ```rust
//! use colorful::{Color, Colorful};
//! use egui_code_editor::{Syntax, Token, TokenType};
//!
//! fn color(token: TokenType) -> Color {
//!     match token {
//!         TokenType::Comment(_) => Color::Grey37,
//!         TokenType::Function => Color::Yellow3b,
//!         TokenType::Keyword => Color::IndianRed1c,
//!         TokenType::Literal => Color::NavajoWhite1,
//!         TokenType::Numeric(_) => Color::MediumPurple,
//!         TokenType::Punctuation(_) => Color::Orange3,
//!         TokenType::Special => Color::Cyan,
//!         TokenType::Str(_) => Color::Green,
//!         TokenType::Type => Color::GreenYellow,
//!         TokenType::Whitespace(_) => Color::White,
//!         TokenType::Unknown => Color::Pink1,
//!     }
//! }
//!
//! fn main() {
//!     let text = r#"// Code Editor
//! CodeEditor::default()
//!     .id_source("code editor")
//!     .with_rows(12)
//!     .with_fontsize(14.0)
//!     .with_theme(self.theme)
//!     .with_syntax(self.syntax.to_owned())
//!     .with_numlines(true)
//!     .vscroll(true)
//!     .show(ui, &mut self.code);
//!     "#;
//!
//!     let syntax = Syntax::rust();
//!     for token in Token::default().tokens(&syntax, text) {
//!         print!("{}", token.buffer().color(color(token.ty())));
//!     }
//! }
//! ```
mod completer;
pub mod highlighting;
pub mod syntax;
#[cfg(test)]
mod tests;
mod themes;

use egui::{Color32, Rect, Shape, TextBuffer, Vec2};
use egui::text::LayoutJob;
use egui::widgets::text_edit::TextEditOutput;
pub use highlighting::Token;
use highlighting::highlight;
use std::collections::BTreeMap;
use std::hash::{Hash, Hasher};
pub use syntax::{Syntax, TokenType};
pub use themes::ColorTheme;
pub use themes::DEFAULT_THEMES;

pub use completer::Completer;

pub trait Editor: Hash {
    fn append(&self, job: &mut LayoutJob, token: &Token);
    fn syntax(&self) -> &Syntax;
}

/// Output of the CodeEditor::show() method
pub struct CodeEditorOutput {
    /// The standard egui TextEdit output
    pub output: TextEditOutput,
    /// The current vertical scroll offset of the editor
    pub scroll_offset: f32,
    pub hscroll_offset: f32,
    pub max_hscroll_offset: f32,
}

#[derive(Clone, Debug, PartialEq)]
/// CodeEditor struct which stores settings for highlighting.
pub struct CodeEditor {
    id: String,
    theme: ColorTheme,
    syntax: Syntax,
    numlines: bool,
    numlines_shift: isize,
    numlines_only_natural: bool,
    fontsize: f32,
    rows: usize,
    vscroll: bool,
    stick_to_bottom: bool,
    desired_width: f32,
    /// Map of Line Number (0-indexed) -> Background Color
    diff_lines: BTreeMap<usize, Color32>,
    row_height: Option<f32>,
    line_numbers: Option<Vec<Option<usize>>>,
    // NEW: Optional external scroll control
    vscroll_offset: Option<f32>,
    hscroll_offset: Option<f32>,
}

impl Hash for CodeEditor {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.theme.hash(state);
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        (self.fontsize as u32).hash(state);
        self.syntax.hash(state);
    }
}

impl Default for CodeEditor {
    fn default() -> CodeEditor {
        let syntax = Syntax::rust();
        CodeEditor {
            id: String::from("Code Editor"),
            theme: ColorTheme::GRUVBOX,
            syntax,
            numlines: true,
            numlines_shift: 0,
            numlines_only_natural: false,
            fontsize: 10.0,
            rows: 10,
            vscroll: true,
            stick_to_bottom: false,
            desired_width: f32::INFINITY,
            diff_lines: BTreeMap::new(),
            row_height: None,
            line_numbers: None,
            vscroll_offset: None,
            hscroll_offset: None,
        }
    }
}

impl CodeEditor {
    pub fn id_source(self, id_source: impl Into<String>) -> Self {
        CodeEditor {
            id: id_source.into(),
            ..self
        }
    }

    /// Minimum number of rows to show.
    ///
    /// **Default: 10**
    pub fn with_rows(self, rows: usize) -> Self {
        CodeEditor { rows, ..self }
    }

    /// Use custom Color Theme
    ///
    /// **Default: Gruvbox**
    pub fn with_theme(self, theme: ColorTheme) -> Self {
        CodeEditor { theme, ..self }
    }

    /// Use custom font size
    ///
    /// **Default: 10.0**
    pub fn with_fontsize(self, fontsize: f32) -> Self {
        CodeEditor { fontsize, ..self }
    }

    /// Use UI font size
    pub fn with_ui_fontsize(self, ui: &mut egui::Ui) -> Self {
        CodeEditor {
            fontsize: egui::TextStyle::Monospace.resolve(ui.style()).size,
            ..self
        }
    }

    /// Show or hide lines numbering
    ///
    /// **Default: true**
    pub fn with_numlines(self, numlines: bool) -> Self {
        CodeEditor { numlines, ..self }
    }

    /// Shift lines numbering by this value
    ///
    /// **Default: 0**
    pub fn with_numlines_shift(self, numlines_shift: isize) -> Self {
        CodeEditor {
            numlines_shift,
            ..self
        }
    }

    /// Show lines numbering only above zero, useful for enabling numbering since nth row
    ///
    /// **Default: false**
    pub fn with_numlines_only_natural(self, numlines_only_natural: bool) -> Self {
        CodeEditor {
            numlines_only_natural,
            ..self
        }
    }

    // custom line number mapping
    pub fn with_line_numbers(self, map: Vec<Option<usize>>) -> Self {
        CodeEditor {
            line_numbers: Some(map),
            ..self
        }
    }

    /// Use custom syntax for highlighting
    ///
    /// **Default: Rust**
    pub fn with_syntax(self, syntax: Syntax) -> Self {
        CodeEditor { syntax, ..self }
    }

    /// Turn on/off scrolling on the vertical axis.
    ///
    /// **Default: true**
    pub fn vscroll(self, vscroll: bool) -> Self {
        CodeEditor { vscroll, ..self }
    }
    /// Should the containing area shrink if the content is small?
    ///
    /// **Default: false**
    pub fn auto_shrink(self, shrink: bool) -> Self {
        CodeEditor {
            desired_width: if shrink { 0.0 } else { self.desired_width },
            ..self
        }
    }

    /// Sets the desired width of the code editor
    ///
    /// **Default: `f32::INFINITY`**
    pub fn desired_width(self, width: f32) -> Self {
        CodeEditor {
            desired_width: width,
            ..self
        }
    }

    /// Stick to bottom
    /// The scroll handle will stick to the bottom position even while the content size
    /// changes dynamically. This can be useful to simulate terminal UIs or log/info scrollers.
    /// The scroll handle remains stuck until user manually changes position. Once "unstuck"
    /// it will remain focused on whatever content viewport the user left it on. If the scroll
    /// handle is dragged to the bottom it will again become stuck and remain there until manually
    /// pulled from the end position.
    ///
    /// **Default: false**
    pub fn stick_to_bottom(self, stick_to_bottom: bool) -> Self {
        CodeEditor {
            stick_to_bottom,
            ..self
        }
    }

    pub fn format_token(&self, ty: TokenType) -> egui::text::TextFormat {
        format_token(&self.theme, self.fontsize, ty)
    }

    pub fn with_diff(self, diff_lines: BTreeMap<usize, Color32>) -> Self {
        CodeEditor { diff_lines, ..self }
    }
    // NEW: Manual row height setter
    pub fn with_row_height(self, row_height: f32) -> Self {
        CodeEditor { row_height: Some(row_height), ..self }
    }


    // Set the vertical scroll offset manually
    pub fn with_vscroll_offset(self, offset: f32) -> Self {
        CodeEditor {
            vscroll_offset: Some(offset),
            ..self
        }
    }

    // Set the horizontal scroll offset manually
    pub fn with_hscroll_offset(self, offset: f32) -> Self {
        CodeEditor {
            hscroll_offset: Some(offset),
            ..self
        }
    }

    fn numlines_show(&self, ui: &mut egui::Ui, text: &str) {
        let total = if text.ends_with('\n') || text.is_empty() {
            text.lines().count() + 1
        } else {
            text.lines().count()
        }
        .max(self.rows) as isize;

        // Calculate max indentation based on the largest REAL line number,
        // not visual total
        let max_indent = if let Some(map) = &self.line_numbers {
            map.iter().flatten().max().unwrap_or(&0).to_string().len()
        } else {
            total.to_string().len()
        };

        let mut counter = (0..total)
            .map(|i| {
                // If we have a custom map, use it
                if let Some(map) = &self.line_numbers {
                    match map.get(i as usize) {
                        Some(Some(n)) => {
                            let label = n.to_string();
                            format!("{}{label}", " ".repeat(max_indent.saturating_sub(label.len())))
                        }
                        // It's a gap or out of bounds -> Empty string
                        _ => " ".repeat(max_indent),
                    }
                } else {
                    // Standard fallback logic
                    let num = i as isize + 1 + self.numlines_shift;
                    if num <= 0 && self.numlines_only_natural {
                        String::new()
                    } else {
                        let label = num.to_string();
                        format!("{}{label}", " ".repeat(max_indent.saturating_sub(label.len())))
                    }
                }
            })
            .collect::<Vec<String>>()
            .join("\n");

        #[allow(clippy::cast_precision_loss)]
        let width = max_indent as f32 * self.fontsize * 0.5 + 4.0; // with padding

        let mut layouter = |ui: &egui::Ui, text_buffer: &dyn TextBuffer, _wrap_width: f32| {
            let layout_job = egui::text::LayoutJob::single_section(
                text_buffer.as_str().to_string(),
                egui::TextFormat::simple(
                    egui::FontId::monospace(self.fontsize),
                    Color32::from_rgb(50, 50, 50),
                ),
            );
            ui.fonts_mut(|f| f.layout_job(layout_job))
        };

        ui.add(
            egui::TextEdit::multiline(&mut counter)
                .id_source(format!("{}_numlines", self.id))
                .font(egui::TextStyle::Monospace)
                .interactive(false)
                .frame(true)
                .desired_rows(self.rows)
                .desired_width(width)
                .layouter(&mut layouter),
        );
    }

    /// Show Code Editor with auto-completion feature
    pub fn show_with_completer(
        &mut self,
        ui: &mut egui::Ui,
        text: &mut dyn egui::TextBuffer,
        completer: &mut Completer,
    ) -> CodeEditorOutput {
        completer.handle_input(ui.ctx());
        let mut editor_output = self.show(ui, text);
        completer.show(&self.syntax, &self.theme, self.fontsize, &mut editor_output.output);
        editor_output
    }

    /// Show Code Editor
    pub fn show(&mut self, ui: &mut egui::Ui, text: &mut dyn egui::TextBuffer) -> CodeEditorOutput {
        use egui::TextBuffer;

        // 0. PRE-CALCULATE / CLONE VALUES
        let vscroll = self.vscroll;
        let stick_to_bottom = self.stick_to_bottom;
        let id_source = self.id.clone();
        let row_height = self.row_height.unwrap_or(16.0);

        let mut text_edit_output: Option<TextEditOutput> = None;
        let mut current_hscroll_offset = 0.0; // Variable to capture the offset
        let mut max_hscroll_offset = 0.0;

        let mut code_editor = |ui: &mut egui::Ui| {
            ui.horizontal_top(|h| {
                self.theme.modify_style(h, self.fontsize);
                if self.numlines {
                    self.numlines_show(h, text.as_str());
                }

                let mut h_scroll = egui::ScrollArea::horizontal()
                    .id_salt(format!("{}_inner_scroll", self.id));

                if let Some(offset) = self.hscroll_offset {
                    h_scroll = h_scroll.horizontal_scroll_offset(offset);
                }

                let h_scroll_output = h_scroll.show(h, |ui| {

                        // *** FIX START: Wrapped content in a Frame to apply theme background ***
                        egui::Frame::new()
                            .fill(self.theme.bg())
                            .inner_margin(0.0) // No margin ensures text aligns with line numbers
                            .show(ui, |ui: &mut egui::Ui| {

                        // --- 1. DRAW BACKGROUNDS ---
                        if !self.diff_lines.is_empty() {
                            let available_width = 100_000.0;
                            let start_offset = ui.cursor().min;
                            let mut shapes = Vec::new();

                            for (line_idx, color) in &self.diff_lines {
                                let top_y = start_offset.y + (*line_idx as f32 * row_height);
                                let bottom_y = top_y + row_height;

                                let rect = Rect::from_min_size(
                                    egui::Pos2::new(start_offset.x, top_y),
                                    Vec2::new(available_width, row_height)
                                );

                                // 1. Draw the background fill
                                shapes.push(Shape::rect_filled(rect, 0.0, *color));

                                // 2. Create a border color that pops!
                                // We do this by safely adding brightness to the existing RGB values.
                                if *color != Color32::from_rgb(25, 25, 25) {
                                    let edge_color = egui::Color32::from_rgb(
                                        color.r().saturating_add(30),
                                        color.g().saturating_add(30),
                                        color.b().saturating_add(30),
                                    );
                                    let stroke = egui::Stroke::new(1.0, edge_color);

                                    // 3. Draw Top Edge (only if the line above is NOT part of this diff block)
                                    if !self.diff_lines.contains_key(&line_idx.saturating_sub(1)) {
                                        shapes.push(egui::Shape::hline(start_offset.x..=available_width, top_y, stroke));
                                    }

                                    // 4. Draw Bottom Edge (only if the line below is NOT part of this diff block)
                                    if !self.diff_lines.contains_key(&(line_idx + 1)) {
                                        // We subtract 0.5 from bottom_y to ensure the stroke stays perfectly inside the row bounds
                                        shapes.push(egui::Shape::hline(start_offset.x..=available_width, bottom_y - 0.5, stroke));
                                    }
                                }
                            }
                            ui.painter().add(Shape::Vec(shapes));
                        }

                        // --- 2. SETUP LAYOUTER ---
                        let mut layouter =
                            |ui: &egui::Ui, text_buffer: &dyn TextBuffer, _wrap_width: f32| {
                                let mut layout_job = highlight(ui.ctx(), self, text_buffer.as_str());

                                // Enforce the cached height to ensure stability
                                layout_job.first_row_min_height = row_height;

                                ui.fonts_mut(|f| f.layout_job(layout_job))
                            };

                        // --- 3. DRAW TEXT ---
                        let output = egui::TextEdit::multiline(text)
                            .id_source(&self.id)
                            .lock_focus(true)
                            .desired_rows(self.rows)
                            .frame(false)
                            .desired_width(self.desired_width)
                            .layouter(&mut layouter)
                            .show(ui);
                        text_edit_output = Some(output);
                    });
                });

                current_hscroll_offset = h_scroll_output.state.offset.x;
                max_hscroll_offset = (h_scroll_output.content_size.x -
                        h_scroll_output.inner_rect.width()).max(0.0);
            });
        };

        // Scroll Logic
        let current_scroll_offset;

        if vscroll {
            let mut scroll_area = egui::ScrollArea::vertical()
                .id_salt(format!("{}_outer_scroll", id_source))
                .stick_to_bottom(stick_to_bottom);

            // Apply external scroll if provided
            if let Some(offset) = self.vscroll_offset {
                scroll_area = scroll_area.vertical_scroll_offset(offset);
            }

            let scroll_output = scroll_area.show(ui, code_editor);
            current_scroll_offset = scroll_output.state.offset.y;
        } else {
            code_editor(ui);
            current_scroll_offset = 0.0;
        }

        CodeEditorOutput {
            output: text_edit_output.expect("TextEditOutput should exist"),
            scroll_offset: current_scroll_offset,
            hscroll_offset: current_hscroll_offset,
            max_hscroll_offset,
        }
    }
}

impl Editor for CodeEditor {
    fn append(&self, job: &mut LayoutJob, token: &Token) {
        if !token.buffer().is_empty() {
            job.append(token.buffer(), 0.0, self.format_token(token.ty()));
        }
    }

    fn syntax(&self) -> &Syntax {
        &self.syntax
    }
}

pub fn format_token(theme: &ColorTheme,
    fontsize: f32, ty: TokenType) -> egui::text::TextFormat {
    let font_id = egui::FontId::monospace(fontsize);
    let color = theme.type_color(ty);
    egui::text::TextFormat::simple(font_id, color)
}
