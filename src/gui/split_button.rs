use eframe::egui::{self, Rect, Sense, Ui};
use rust_i18n::t;

pub struct SplitButton {
    text: String,
    id_salt: egui::Id,
    arrow_width: Option<f32>,
    main_tooltip: Option<String>,
    arrow_tooltip: Option<String>,
    is_selected: bool,
    transparent_bg: bool,
    desired_width: Option<f32>,
}

impl SplitButton {
    /// Start building a new SplitButton with the primary text
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            id_salt: egui::Id::new("split_btn"),
            arrow_width: None,
            main_tooltip: None,
            arrow_tooltip: None,
            is_selected: false,
            transparent_bg: false, // Default: Normal button background
            desired_width: None,
        }
    }

    /// Provide a unique ID salt, critical for lists of buttons
    pub fn id_salt(mut self, salt: impl std::hash::Hash) -> Self {
        self.id_salt = egui::Id::new(salt);
        self
    }

    /// Set the width of the right-arrow button (Defaults to the button's height)
    pub fn arrow_width(mut self, width: f32) -> Self {
        self.arrow_width = Some(width);
        self
    }

    /// Set a tooltip for the main portion of the button
    pub fn main_tooltip(mut self, tooltip: impl Into<String>) -> Self {
        self.main_tooltip = Some(tooltip.into());
        self
    }

    /// Set a tooltip specifically for the arrow portion
    pub fn arrow_tooltip(mut self, tooltip: impl Into<String>) -> Self {
        self.arrow_tooltip = Some(tooltip.into());
        self
    }

    /// Force the button into a highlighted "selected" state
    pub fn selected(mut self, is_selected: bool) -> Self {
        self.is_selected = is_selected;
        self
    }

    /// If true, the button background is invisible until hovered (great for sidebars)
    pub fn transparent(mut self, transparent: bool) -> Self {
        self.transparent_bg = transparent;
        self
    }

    /// Set a strict width for the entire component
    pub fn desired_width(mut self, width: f32) -> Self {
        self.desired_width = Some(width);
        self
    }

    /// Render the widget. Returns (main_clicked, arrow_clicked)
    pub fn show(self, ui: &mut Ui) -> (bool, bool) {
        let button_padding = ui.spacing().button_padding;
        let font_id = egui::TextStyle::Button.resolve(ui.style());
        let text_height = ui.text_style_height(&egui::TextStyle::Button);
        let height = text_height + button_padding.y * 2.0;

        let arrow_w = self.arrow_width.unwrap_or(height); // Default to square!

        // --- NEW: Smart Width Calculation ---
        let width = self.desired_width.unwrap_or_else(|| {
            // Measure the exact width of the text
            let text_width = ui.painter().layout_no_wrap(
                self.text.clone(), font_id.clone(), egui::Color32::TRANSPARENT
            ).size().x;
            // Total width = text + padding + reserved space for the arrow
            text_width + button_padding.x * 2.0
        });

        let desired_size = egui::vec2(width, height);
        let (rect, _) = ui.allocate_exact_size(desired_size, Sense::hover());

        let is_hovered = ui.rect_contains_pointer(rect);
        let active_arrow_w = if is_hovered { arrow_w } else { 0.0 };

        let main_rect = Rect::from_min_max(rect.min, egui::pos2(rect.max.x - active_arrow_w, rect.max.y));
        let arrow_rect = Rect::from_min_max(egui::pos2(rect.max.x - active_arrow_w, rect.min.y), rect.max);

        let base_id = ui.id().with(self.id_salt);
        let main_response = ui.interact(main_rect, base_id.with("main"), Sense::click());
        let arrow_response = ui.interact(arrow_rect, base_id.with("arrow"), Sense::click());

        if ui.is_rect_visible(rect) {
            let active = &ui.style().visuals.widgets.active;
            let hovered = &ui.style().visuals.widgets.hovered;
            let inactive = &ui.style().visuals.widgets.inactive;

            // 1. Base selection background
            if self.is_selected {
                ui.painter().rect(rect, active.corner_radius, active.bg_fill, egui::Stroke::NONE, egui::StrokeKind::Inside);
            }

            // 2. Draw Main Button Border & Hover
            let mut main_rounding = if self.is_selected { active.corner_radius } else { inactive.corner_radius };
            if active_arrow_w > 0.0 { main_rounding.ne = 0; main_rounding.se = 0; }

            if main_response.hovered() {
                ui.painter().rect(main_rect, main_rounding, hovered.bg_fill, hovered.bg_stroke, egui::StrokeKind::Inside);
            } else if !self.transparent_bg && !self.is_selected {
                ui.painter().rect(main_rect, main_rounding, inactive.bg_fill, inactive.bg_stroke, egui::StrokeKind::Inside);
            } else if self.is_selected {
                ui.painter().rect_stroke(main_rect, main_rounding, active.bg_stroke, egui::StrokeKind::Inside);
            }

            // 3. Draw Arrow Button
            if active_arrow_w > 0.0 {
                let mut arrow_rounding = inactive.corner_radius;
                arrow_rounding.nw = 0; arrow_rounding.sw = 0;

                let (bg, stroke, text_color) = if arrow_response.hovered() {
                    (hovered.bg_fill, hovered.bg_stroke, hovered.text_color())
                } else {
                    let s = if self.is_selected { active.bg_stroke } else if !self.transparent_bg { inactive.bg_stroke } else { egui::Stroke::NONE };
                    let bg_fill = if !self.transparent_bg && !self.is_selected { inactive.bg_fill } else { egui::Color32::TRANSPARENT };
                    (bg_fill, s, inactive.text_color())
                };

                ui.painter().rect(arrow_rect, arrow_rounding, bg, stroke, egui::StrokeKind::Inside);

                let div_stroke = if self.is_selected { active.bg_stroke } else { inactive.bg_stroke };
                if !self.transparent_bg || is_hovered || self.is_selected {
                    ui.painter().vline(arrow_rect.min.x, arrow_rect.y_range(), div_stroke);
                }

                ui.painter().text(arrow_rect.center(), egui::Align2::CENTER_CENTER, "➡", font_id.clone(), text_color);
            }

            // 4. Draw Main Text (NEW: Centered vertically so it looks like a normal egui button)
            let text_color = if main_response.hovered() { hovered.text_color() } else if self.is_selected { active.text_color() } else { inactive.text_color() };
            let painter = ui.painter().with_clip_rect(main_rect);
            painter.text(
                egui::pos2(main_rect.min.x + button_padding.x, main_rect.center().y),
                egui::Align2::LEFT_CENTER,
                &self.text,
                font_id,
                text_color
            );
        }

        // Attach Tooltips
        if let Some(tooltip) = self.main_tooltip {
            main_response.clone().on_hover_ui(|ui| { ui.heading(egui::RichText::new(tooltip).strong()); });
        }
        if active_arrow_w > 0.0 {
            if let Some(tooltip) = self.arrow_tooltip {
                arrow_response.clone().on_hover_ui(|ui| { ui.heading(egui::RichText::new(tooltip).strong()); });
            }
        }

        (main_response.clicked(), arrow_response.clicked())
    }
}
