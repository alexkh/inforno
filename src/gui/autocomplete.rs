use egui::{
    text::LayoutJob, Context, FontId, Id, Key, Modifiers, Popup, PopupCloseBehavior, TextBuffer,
    Response,
    TextEdit, Widget,
};
use fuzzy_matcher::skim::SkimMatcherV2;
use fuzzy_matcher::FuzzyMatcher;
use std::cmp::Reverse;

/// Trait that can be used to modify the TextEdit
type SetTextEditProperties = dyn FnOnce(TextEdit) -> TextEdit;

pub struct AutoCompleteTextEdit<'a, T> {
    text_field: &'a mut String,
    search: T,
    max_suggestions: usize,
    highlight: bool,
    multiple_words: bool,
    set_properties: Option<Box<SetTextEditProperties>>,
    popup_on_focus: bool,
    width: f32,
}

impl<'a, T, S> AutoCompleteTextEdit<'a, T>
where
    T: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    pub fn new(text_field: &'a mut String, search: T) -> Self {
        Self {
            text_field,
            search,
            max_suggestions: 10,
            highlight: false,
            multiple_words: false,
            set_properties: None,
            popup_on_focus: false,
            width: f32::INFINITY,
        }
    }
}

impl<T, S> AutoCompleteTextEdit<'_, T>
where
    T: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    pub fn max_suggestions(mut self, max_suggestions: usize) -> Self {
        self.max_suggestions = max_suggestions;
        self
    }

    pub fn highlight_matches(mut self, highlight: bool) -> Self {
        self.highlight = highlight;
        self
    }

    pub fn multiple_words(mut self, multiple_words: bool) -> Self {
        self.multiple_words = multiple_words;
        self
    }

    pub fn popup_on_focus(mut self, popup_on_focus: bool) -> Self {
        self.popup_on_focus = popup_on_focus;
        self
    }

    pub fn width(mut self, width: f32) -> Self {
        self.width = width;
        self
    }

    pub fn set_text_edit_properties(
        mut self,
        set_properties: impl FnOnce(TextEdit) -> TextEdit + 'static,
    ) -> Self {
        self.set_properties = Some(Box::new(set_properties));
        self
    }
}

impl<T, S> Widget for AutoCompleteTextEdit<'_, T>
where
    T: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    fn ui(self, ui: &mut egui::Ui) -> Response {
        let Self {
            text_field,
            search,
            max_suggestions,
            highlight,
            multiple_words,
            set_properties,
            popup_on_focus,
            width,
        } = self;

        let id = ui.next_auto_id();
        ui.skip_ahead_auto_ids(1);
        let mut state = AutoCompleteTextEditState::load(ui.ctx(), id).unwrap_or_default();

        let up_pressed = state.focused
            && ui.input_mut(|input| input.consume_key(Modifiers::default(), Key::ArrowUp));
        let down_pressed = state.focused
            && ui.input_mut(|input| input.consume_key(Modifiers::default(), Key::ArrowDown));

        let mut text_edit = TextEdit::singleline(text_field);
        if let Some(set_properties) = set_properties {
            text_edit = set_properties(text_edit);
        }
        let text_edit_output = text_edit.show(ui);

        let completion_input = if multiple_words {
            if let Some(cursor_range) = text_edit_output.cursor_range {
                let index = cursor_range.primary.index;
                let mut start = index;
                let mut end = index;
                while start > 0
                    && !text_field[start - 1..start]
                        .chars()
                        .next()
                        .map(|c| c.is_whitespace())
                        .unwrap_or(false)
                {
                    start -= 1;
                }
                while end < text_field.len()
                    && !text_field[end..end + 1]
                        .chars()
                        .next()
                        .map(|c| c.is_whitespace())
                        .unwrap_or(false)
                {
                    end += 1;
                }
                state.start = start;
                state.end = end;
                text_field[start..end].trim()
            } else {
                text_field.as_str()
            }
        } else {
            text_field.as_str()
        };

        let mut text_response = text_edit_output.response;
        state.focused = text_response.has_focus();

        let matcher = SkimMatcherV2::default().ignore_case();

        let match_results = {
            let mut match_results = search
                .into_iter()
                .filter_map(|s| {
                    let score = matcher.fuzzy_indices(s.as_ref(), completion_input);
                    score.map(|(score, indices)| (s, score, indices))
                })
                .collect::<Vec<_>>();
            match_results.sort_by_key(|k| Reverse(k.1));
            match_results
        };

        if text_response.changed()
            || (state.selected_index.is_some()
                && state.selected_index.unwrap() >= match_results.len())
        {
            state.selected_index = None;
        }

        state.update_index(
            down_pressed,
            up_pressed,
            match_results.len(),
            max_suggestions,
        );

        let popup = Popup::from_response(&text_response)
            .layout(egui::Layout::top_down_justified(egui::Align::LEFT))
            .close_behavior(PopupCloseBehavior::IgnoreClicks)
            .id(id)
            .align(egui::RectAlign::BOTTOM_START)
            .width(width)
            .open(
                state.focused
                    && (!text_field.is_empty() || popup_on_focus)
                    && !match_results.is_empty(),
            );

        let accepted_by_keyboard = ui.input(|input| input.key_pressed(Key::Enter))
            || ui.input(|input| input.key_pressed(Key::Tab));
        if let (Some(index), true) = (
            state.selected_index,
            accepted_by_keyboard || !popup.is_open(),
        ) {
            let match_result = match_results[index].0.as_ref();
            if multiple_words {
                text_field.replace_range(state.start..state.end, match_result);
                let text_edit_id = text_response.id;
                if let Some(mut state) = TextEdit::load_state(ui.ctx(), text_edit_id) {
                    let ccursor = egui::text::CCursor::new(text_field.chars().count());
                    state
                        .cursor
                        .set_char_range(Some(egui::text::CCursorRange::one(ccursor)));
                    state.store(ui.ctx(), text_edit_id);
                    text_response.request_focus();
                }
            } else {
                text_field.replace_with(match_result);
            }
            state.selected_index = None;
            text_response.mark_changed();
        }

        popup.show(|ui| {
            for (i, (output, _, match_indices)) in
                match_results.iter().take(max_suggestions).enumerate()
            {
                let mut selected = if let Some(x) = state.selected_index {
                    x == i
                } else {
                    false
                };

                let text = if highlight {
                    highlight_matches(
                        output.as_ref(),
                        match_indices,
                        ui.style().visuals.widgets.active.text_color(),
                    )
                } else {
                    let mut job = LayoutJob::default();
                    job.append(output.as_ref(), 0.0, egui::TextFormat::default());
                    job
                };
                if ui.toggle_value(&mut selected, text).hovered() {
                    state.selected_index = Some(i);
                }
            }
        });

        state.store(ui.ctx(), id);
        text_response.response
    }
}

fn highlight_matches(text: &str, match_indices: &[usize], color: egui::Color32) -> LayoutJob {
    let mut formatted = LayoutJob::default();
    let mut it = text.char_indices().enumerate().peekable();
    while let Some((char_idx, (byte_idx, c))) = it.next() {
        let start = byte_idx;
        let mut end = byte_idx + (c.len_utf8() - 1);
        let match_state = match_indices.contains(&char_idx);
        while let Some((peek_char_idx, (_, k))) = it.peek() {
            if match_state == match_indices.contains(peek_char_idx) {
                end += k.len_utf8();
                _ = it.next();
            } else {
                break;
            }
        }
        let format = if match_state {
            egui::TextFormat::simple(FontId::default(), color)
        } else {
            egui::TextFormat::default()
        };
        let slice = &text[start..=end];
        formatted.append(slice, 0.0, format);
    }
    formatted
}

#[derive(Debug, Clone, Default, serde::Deserialize, serde::Serialize)]
struct AutoCompleteTextEditState {
    selected_index: Option<usize>,
    focused: bool,
    start: usize,
    end: usize,
}

impl AutoCompleteTextEditState {
    fn store(self, ctx: &Context, id: Id) {
        ctx.data_mut(|d| d.insert_persisted(id, self));
    }
    fn load(ctx: &Context, id: Id) -> Option<Self> {
        ctx.data_mut(|d| d.get_persisted(id))
    }
    fn update_index(
        &mut self,
        down_pressed: bool,
        up_pressed: bool,
        match_results_count: usize,
        max_suggestions: usize,
    ) {
        self.selected_index = match self.selected_index {
            _ if match_results_count == 0 || max_suggestions == 0 => None,
            Some(index) if down_pressed => {
                if index + 1 < match_results_count.min(max_suggestions) {
                    Some(index + 1)
                } else {
                    None
                }
            }
            Some(index) if up_pressed => {
                if index == 0 {
                    None
                } else {
                    Some(index - 1)
                }
            }
            None if down_pressed => Some(0),
            None if up_pressed => Some(match_results_count.min(max_suggestions) - 1),
            Some(index) => Some(index),
            None => None,
        }
    }
}