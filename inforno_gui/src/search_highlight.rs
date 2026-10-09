//! Highlighting of chat search terms: in the search results' snippets, and in the
//! rendered chat itself, which also scrolls to the first occurrence when opened.
//!
//! Matches are found in the text egui actually paints, so they work for any label
//! the chat renderer produces (paragraphs, lists, tables, code) without touching it.

/// A search term to highlight in one open chat.
#[derive(Clone, Debug)]
pub struct ChatHighlight {
    pub term: String,
    /// Frames left in which to scroll the first match into view. More than one,
    /// because a freshly opened chat's layout (math, images, tables) settles over a few frames.
    pub scroll_frames: u8,
}

impl ChatHighlight {
    pub fn new(term: &str) -> Self {
        Self { term: term.trim().to_string(), scroll_frames: 3 }
    }
}

fn highlight_fill(ui: &egui::Ui) -> egui::Color32 {
    if ui.visuals().dark_mode {
        egui::Color32::from_rgb(110, 85, 0)
    } else {
        egui::Color32::from_rgb(255, 225, 120)
    }
}

/// Case folding for matching; the same rule SQLite's LIKE uses for ASCII.
fn fold(c: char) -> char {
    c.to_lowercase().next().unwrap_or(c)
}

/// Char offsets of the non-overlapping occurrences of `term` (already folded) in `chars`.
fn match_starts(chars: &[char], term: &[char]) -> Vec<usize> {
    let mut starts = Vec::new();
    if term.is_empty() {
        return starts;
    }
    let mut i = 0;
    while i + term.len() <= chars.len() {
        if chars[i..i + term.len()].iter().zip(term).all(|(c, t)| fold(*c) == *t) {
            starts.push(i);
            i += term.len();
        } else {
            i += 1;
        }
    }
    starts
}

/// `text` as a label layout with every occurrence of `term` marked (search result snippets).
pub fn snippet_job(ui: &egui::Ui, text: &str, term: &str) -> egui::text::LayoutJob {
    let font = egui::TextStyle::Body.resolve(ui.style());
    let normal = egui::TextFormat::simple(font.clone(), ui.visuals().weak_text_color());
    let marked = egui::TextFormat {
        background: highlight_fill(ui),
        ..egui::TextFormat::simple(font, ui.visuals().strong_text_color())
    };
    let chars: Vec<char> = text.chars().collect();
    let term: Vec<char> = term.trim().chars().map(fold).collect();
    let mut bytes: Vec<usize> = text.char_indices().map(|(b, _)| b).collect();
    bytes.push(text.len());

    let mut job = egui::text::LayoutJob::default();
    let mut done = 0;
    for start in match_starts(&chars, &term) {
        let end = start + term.len();
        job.append(&text[bytes[done]..bytes[start]], 0.0, normal.clone());
        job.append(&text[bytes[start]..bytes[end]], 0.0, marked.clone());
        done = end;
    }
    job.append(&text[bytes[done]..], 0.0, normal);
    job
}

/// Screen rects of the occurrences of `term` (already folded) in a painted text shape,
/// one rect per occurrence and row.
fn match_rects(text: &egui::epaint::TextShape, term: &[char]) -> Vec<egui::Rect> {
    let galley = &text.galley;
    // Glyphs of all rows; paragraph breaks become '\n' so no match spans them.
    let mut chars = Vec::new();
    let mut glyph_at = Vec::new();
    for (r, row) in galley.rows.iter().enumerate() {
        for (g, glyph) in row.glyphs.iter().enumerate() {
            chars.push(glyph.chr);
            glyph_at.push(Some((r, g)));
        }
        if row.ends_with_newline {
            chars.push('\n');
            glyph_at.push(None);
        }
    }

    let mut rects = Vec::new();
    for start in match_starts(&chars, term) {
        let mut current: Option<(usize, egui::Rect)> = None;
        for &(r, g) in glyph_at[start..start + term.len()].iter().flatten() {
            let row = &galley.rows[r];
            let glyph = &row.glyphs[g];
            let x = text.pos.x + row.pos.x + glyph.pos.x;
            let y = row.rect().translate(text.pos.to_vec2()).y_range();
            let rect = egui::Rect::from_x_y_ranges(x..=x + glyph.advance_width, y);
            current = match current {
                Some((cr, cur)) if cr == r => Some((r, cur.union(rect))),
                Some((_, cur)) => {
                    rects.push(cur);
                    Some((r, rect))
                }
                None => Some((r, rect)),
            };
        }
        rects.extend(current.map(|(_, rect)| rect));
    }
    rects
}

/// Runs `add_contents` (the inside of a vertical `ScrollArea`) and marks every occurrence
/// of the highlight's term in the text it paints.
///
/// While `scroll_frames` lasts, the whole content is painted (and clipped back to the
/// viewport afterwards), so the first occurrence is found and scrolled to even when it
/// is out of view. Its widgets are disabled during those frames, so the parts outside
/// the viewport can't take clicks meant for the surrounding panels.
pub fn highlight_contents<R>(
    ui: &mut egui::Ui,
    highlight: Option<&mut ChatHighlight>,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    let Some(highlight) = highlight.filter(|h| !h.term.is_empty()) else {
        return add_contents(ui);
    };
    let viewport = ui.clip_rect();
    let reveal = highlight.scroll_frames > 0;
    if reveal {
        ui.set_clip_rect(egui::Rect::from_x_y_ranges(
            viewport.x_range(),
            egui::Rect::EVERYTHING.y_range(),
        ));
        ui.style_mut().visuals.disabled_alpha = 1.0; // disabled, but not faded
        ui.disable();
    }

    let layer = ui.layer_id();
    let first_shape = ui.ctx().graphics_mut(|g| g.entry(layer).next_idx());
    let result = add_contents(ui);

    let term: Vec<char> = highlight.term.chars().map(fold).collect();
    let fill = highlight_fill(ui);
    let mut first_match: Option<egui::Rect> = None;
    ui.ctx().graphics_mut(|g| {
        let list = g.entry(layer);
        for idx in first_shape.0..list.next_idx().0 {
            list.mutate_shape(egui::layers::ShapeIdx(idx), |clipped| {
                if reveal {
                    clipped.clip_rect = clipped.clip_rect.intersect(viewport);
                }
                let egui::Shape::Text(text) = &clipped.shape else { return };
                let marks = match_rects(text, &term);
                if marks.is_empty() {
                    return;
                }
                for mark in &marks {
                    if first_match.is_none_or(|f| (mark.min.y, mark.min.x) < (f.min.y, f.min.x)) {
                        first_match = Some(*mark);
                    }
                }
                // Marks go under the text: replace the shape with [marks.., text]
                let text = std::mem::replace(&mut clipped.shape, egui::Shape::Noop);
                let mut shapes: Vec<egui::Shape> = marks
                    .into_iter()
                    .map(|r| egui::Shape::rect_filled(r, 2.0, fill))
                    .collect();
                shapes.push(text);
                clipped.shape = egui::Shape::Vec(shapes);
            });
        }
    });

    if reveal {
        ui.set_clip_rect(viewport);
        if let Some(rect) = first_match {
            ui.scroll_to_rect_animation(
                rect,
                Some(egui::Align::Center),
                egui::style::ScrollAnimation::none(),
            );
        }
        highlight.scroll_frames -= 1;
        ui.ctx().request_repaint();
    }
    result
}