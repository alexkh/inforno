//! Markdown (GFM) tables, rendered by us instead of egui_commonmark: its
//! cells never wrap, so one long cell made the whole table as wide as that
//! cell's text. Here every column gets a width that fits the pane, and each
//! cell is drawn through the normal markdown path, so its text and inline
//! math wrap inside the cell.

use egui::{Rect, Ui, UiBuilder, vec2};

/// A parsed table. Every row has exactly as many cells as the header.
#[derive(Debug, PartialEq)]
pub struct Table {
    pub header: Vec<String>,
    /// One per column, from the delimiter row.
    pub aligns: Vec<ColumnAlign>,
    pub rows: Vec<Vec<String>>,
}

/// Column alignment from the delimiter row: `---` or `:--` left, `:-:`
/// center, `--:` right.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ColumnAlign {
    Left,
    Center,
    Right,
}

impl ColumnAlign {
    /// Fraction of a cell's leftover width that goes before its content.
    fn offset_factor(self) -> f32 {
        match self {
            ColumnAlign::Left => 0.0,
            ColumnAlign::Center => 0.5,
            ColumnAlign::Right => 1.0,
        }
    }
}

/// A piece of a markdown chunk: plain markdown, or a table lifted out of it.
#[derive(Debug, PartialEq)]
pub enum MdBlock<'a> {
    Text(&'a str),
    Table(Table),
}

/// Splits markdown into text and the top-level tables in it. A table is a
/// header line with a pipe, followed by a delimiter row (`|---|:--:|`) with
/// the same number of cells, then rows until a blank or pipe-less line.
/// Tables nested in lists or quotes are left to egui_commonmark.
pub fn split_tables(md: &str) -> Vec<MdBlock<'_>> {
    let lines: Vec<(usize, &str)> = md
        .split_inclusive('\n')
        .scan(0, |offset, line| {
            let start = *offset;
            *offset += line.len();
            Some((start, line))
        })
        .collect();

    let mut blocks = Vec::new();
    let mut text_start = 0;
    let mut i = 0;

    while i + 1 < lines.len() {
        let header_line = lines[i].1;
        let header = is_table_line(header_line).then(|| split_row(header_line));
        let aligns = parse_delimiter_row(lines[i + 1].1);

        let (Some(header), Some(aligns)) = (header, aligns) else {
            i += 1;
            continue;
        };
        if header.len() != aligns.len() {
            i += 1;
            continue;
        }

        let mut rows = Vec::new();
        let mut end = i + 2;
        while end < lines.len() && is_table_line(lines[end].1) {
            let mut cells = split_row(lines[end].1);
            cells.resize(header.len(), String::new()); // pad short rows, drop extra cells
            rows.push(cells);
            end += 1;
        }

        let table_start = lines[i].0;
        if table_start > text_start {
            blocks.push(MdBlock::Text(&md[text_start..table_start]));
        }
        blocks.push(MdBlock::Table(Table { header, aligns, rows }));

        text_start = lines.get(end).map_or(md.len(), |l| l.0);
        i = end;
    }

    if text_start < md.len() {
        blocks.push(MdBlock::Text(&md[text_start..]));
    }
    blocks
}

/// A candidate table line: at most 3 spaces of indentation (more is code or
/// a nested block) and at least one pipe.
fn is_table_line(line: &str) -> bool {
    let indent = line.len() - line.trim_start_matches(' ').len();
    let trimmed = line.trim();
    indent <= 3 && !trimmed.is_empty() && !trimmed.starts_with('>') && trimmed.contains('|')
}

/// Column alignments of a delimiter row like `|---|:--:|---:|`, if it is one.
fn parse_delimiter_row(line: &str) -> Option<Vec<ColumnAlign>> {
    // A pipe is required: a bare `---` is a horizontal rule or heading underline.
    if !is_table_line(line) {
        return None;
    }
    split_row(line)
        .iter()
        .map(|c| {
            let left = c.starts_with(':');
            let right = c.ends_with(':') && c.len() > 1;
            let dashes = &c[usize::from(left)..c.len() - usize::from(right)];
            if dashes.is_empty() || !dashes.chars().all(|ch| ch == '-') {
                return None;
            }
            Some(match (left, right) {
                (true, true) => ColumnAlign::Center,
                (false, true) => ColumnAlign::Right,
                _ => ColumnAlign::Left,
            })
        })
        .collect()
}

/// Splits a row into trimmed cells on unescaped pipes. Unlike strict GFM, a
/// pipe inside a code span or inline math (`$|x|$`) doesn't split the cell,
/// since LLMs write absolute values that way. `\|` becomes `|`.
fn split_row(line: &str) -> Vec<String> {
    let mut row = line.trim();
    row = row.strip_prefix('|').unwrap_or(row);
    if row.ends_with('|') && !row.ends_with("\\|") {
        row = &row[..row.len() - 1];
    }

    let bytes = row.as_bytes();
    let mut cells = Vec::new();
    let mut cell_start = 0;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i += 2,
            b'`' => i = skip_code_span(row, i),
            b'$' => i = skip_inline_math(row, i),
            b'|' => {
                cells.push(row[cell_start..i].trim().replace("\\|", "|"));
                i += 1;
                cell_start = i;
            }
            _ => i += 1,
        }
    }
    cells.push(row[cell_start.min(row.len())..].trim().replace("\\|", "|"));
    cells
}

/// Index just past the code span starting at `start`, or `start + 1` if the
/// backticks are never closed.
fn skip_code_span(s: &str, start: usize) -> usize {
    let ticks = s[start..].bytes().take_while(|&b| b == b'`').count();
    let fence = &s[start..start + ticks];
    match s[start + ticks..].find(fence) {
        Some(rel) => start + ticks + rel + ticks,
        None => start + ticks,
    }
}

/// Index just past the inline math starting at `start`, or `start + 1` if it
/// isn't math. Same rule as pulldown-cmark: the opening `$` (or `$$`) is not
/// followed by whitespace and the closing one is not preceded by it, so a
/// cell like "$5 | $10" still splits.
fn skip_inline_math(s: &str, start: usize) -> usize {
    let delim = if s[start..].starts_with("$$") { "$$" } else { "$" };
    let body_start = start + delim.len();
    let opens = s[body_start..].chars().next().is_some_and(|c| !c.is_whitespace());
    if !opens {
        return start + 1;
    }
    let mut search = body_start;
    while let Some(rel) = s[search..].find(delim) {
        let close = search + rel;
        let escaped = s[..close].ends_with('\\');
        let closes = close > body_start && !s[..close].ends_with(char::is_whitespace);
        if closes && !escaped {
            return close + delim.len();
        }
        search = close + 1;
    }
    start + 1
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

/// Draws `table` with striped rows. `render_cell` draws one cell's markdown
/// (with an id unique to that cell) into the given ui, wrapping at its width.
///
/// Rows are plain top-aligned horizontal strips rather than an `egui::Grid`:
/// Grid centers each cell in the row height it remembered from the previous
/// frame, so wrapped cells started halfway down and spilled into the next
/// row. Column widths are known up front here, so every row is laid out
/// correctly on the first frame and is exactly as tall as its tallest cell.
pub fn render_table(
    ui: &mut Ui,
    table: &Table,
    id_salt: impl std::hash::Hash + std::fmt::Debug,
    mut render_cell: impl FnMut(&mut Ui, &str, egui::Id),
) {
    let id = ui.id().with(("md_table", id_salt));
    let spacing = vec2(12.0, 6.0);
    let n_cols = table.header.len();

    egui::Frame::group(ui.style()).show(ui, |ui| {
        let cell_widths = natural_widths(ui, table, id, &mut render_cell);
        let natural: Vec<f32> = (0..n_cols)
            .map(|c| cell_widths.iter().map(|row| row[c]).fold(0.0, f32::max))
            .collect();
        let gaps = spacing.x * n_cols.saturating_sub(1) as f32;
        let min_col = ui.text_style_height(&egui::TextStyle::Body) * 3.0;
        let widths = fit_columns(&natural, ui.available_width() - gaps, min_col);
        let table_width = widths.iter().sum::<f32>() + gaps;

        // Table spacing between cells and rows; cells keep the normal spacing inside.
        let cell_spacing = ui.spacing().item_spacing;
        ui.spacing_mut().item_spacing = spacing;
        let stripe = ui.visuals().faint_bg_color;

        for (r, row) in std::iter::once(&table.header).chain(&table.rows).enumerate() {
            // Reserve a slot behind the row; its height is only known afterwards.
            let background = ui.painter().add(egui::Shape::Noop);

            let row_rect = ui
                .horizontal_top(|ui| {
                    for (c, cell) in row.iter().enumerate() {
                        let w = widths[c];
                        // Centered/right cells shift by their leftover width. A cell
                        // that wraps has none and stays left-aligned: egui's wrapping
                        // layout can't align each line separately. The 1pt of slack
                        // keeps float rounding from wrapping the last word.
                        let leftover = (w - cell_widths[r][c] - 1.0).max(0.0);
                        let shift = leftover * table.aligns[c].offset_factor();

                        ui.allocate_ui_with_layout(
                            vec2(w, 0.0),
                            egui::Layout::top_down(egui::Align::Min),
                            |ui| {
                                ui.set_width(w);
                                ui.spacing_mut().item_spacing = cell_spacing;
                                let content = Rect::from_min_max(
                                    ui.cursor().min + vec2(shift, 0.0),
                                    egui::pos2(ui.max_rect().right(), ui.max_rect().bottom()),
                                );
                                ui.scope_builder(UiBuilder::new().max_rect(content), |ui| {
                                    render_cell(ui, cell, id.with((r, c)));
                                });
                            },
                        );
                    }
                })
                .response
                .rect;

            // Same striping as egui::Grid: every other row, header excluded.
            if r % 2 == 1 {
                let rect = Rect::from_min_size(row_rect.min, vec2(table_width, row_rect.height()))
                    .expand2(vec2(2.0, 0.5 * spacing.y));
                ui.painter().set(background, egui::Shape::rect_filled(rect, 2.0, stripe));
            }
        }
    });
}

/// Every cell's width if nothing wrapped (`[row][column]`, header first),
/// measured by drawing each cell once in an invisible, unbounded ui. Cached
/// in egui's temp memory per table content and text size, so this only
/// runs again when the table changes (e.g. while it streams in).
fn natural_widths(
    ui: &mut Ui,
    table: &Table,
    id: egui::Id,
    render_cell: &mut impl FnMut(&mut Ui, &str, egui::Id),
) -> Vec<Vec<f32>> {
    let row_height = ui.text_style_height(&egui::TextStyle::Body);
    let cache_id = id.with(("natural_widths", egui::Id::new((&table.header, &table.rows)), row_height.to_bits()));
    if let Some(widths) = ui.data(|d| d.get_temp::<Vec<Vec<f32>>>(cache_id)) {
        return widths;
    }

    let unbounded = Rect::from_min_size(ui.cursor().min, vec2(100_000.0, 100_000.0));
    let widths: Vec<Vec<f32>> = std::iter::once(&table.header)
        .chain(&table.rows)
        .enumerate()
        .map(|(r, row)| {
            row.iter()
                .enumerate()
                .map(|(c, cell)| {
                    let mut probe = ui.new_child(
                        UiBuilder::new()
                            .id_salt(("sizing", r, c))
                            .max_rect(unbounded)
                            .sizing_pass()
                            .invisible(),
                    );
                    render_cell(&mut probe, cell, id.with(("sizing", r, c)));
                    probe.min_rect().width()
                })
                .collect()
        })
        .collect();

    ui.data_mut(|d| d.insert_temp(cache_id, widths.clone()));
    widths
}

/// Column widths for `available` space. If every column fits at its natural
/// width, that's what it gets. Otherwise narrow columns keep their natural
/// width and the space left is shared evenly by the wide ones, which then
/// wrap. No column goes below `min_col`, so a very narrow pane can still
/// overflow (and scroll) instead of squeezing columns to nothing.
fn fit_columns(natural: &[f32], available: f32, min_col: f32) -> Vec<f32> {
    let natural: Vec<f32> = natural.iter().map(|w| w.ceil()).collect();
    if natural.iter().sum::<f32>() <= available {
        return natural;
    }

    let mut order: Vec<usize> = (0..natural.len()).collect();
    order.sort_by(|&a, &b| natural[a].total_cmp(&natural[b]));

    let mut widths = vec![0.0; natural.len()];
    let mut remaining = available;
    for (k, &col) in order.iter().enumerate() {
        let share = remaining / (order.len() - k) as f32;
        widths[col] = natural[col].min(share).max(min_col).floor();
        remaining -= widths[col];
    }
    widths
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(md: &str) -> Table {
        match split_tables(md).into_iter().find_map(|b| match b {
            MdBlock::Table(t) => Some(t),
            _ => None,
        }) {
            Some(t) => t,
            None => panic!("no table in {md:?}"),
        }
    }

    #[test]
    fn splits_text_and_table() {
        let md = "Intro\n\n| a | b |\n|---|:-:|\n| 1 | 2 |\n\nOutro\n";
        let blocks = split_tables(md);
        assert_eq!(blocks.len(), 3);
        assert_eq!(blocks[0], MdBlock::Text("Intro\n\n"));
        assert_eq!(
            blocks[1],
            MdBlock::Table(Table {
                header: vec!["a".into(), "b".into()],
                aligns: vec![ColumnAlign::Left, ColumnAlign::Center],
                rows: vec![vec!["1".into(), "2".into()]],
            })
        );
        assert_eq!(blocks[2], MdBlock::Text("\nOutro\n"));
    }

    #[test]
    fn table_at_end_without_newline() {
        let t = table("| a | b |\n|---|---|\n| 1 | 2 |");
        assert_eq!(t.rows, vec![vec!["1".to_string(), "2".to_string()]]);
    }

    #[test]
    fn pipes_inside_math_and_code_dont_split() {
        let t = table("| f | g |\n|---|---|\n| $|x|$ | `a|b` |\n");
        assert_eq!(t.rows[0], vec!["$|x|$".to_string(), "`a|b`".to_string()]);
    }

    #[test]
    fn currency_is_not_math() {
        let t = table("| a | b |\n|---|---|\n| $5 | $10 |\n");
        assert_eq!(t.rows[0], vec!["$5".to_string(), "$10".to_string()]);
    }

    #[test]
    fn escaped_pipe() {
        let t = table("| a | b |\n|---|---|\n| x \\| y | z |\n");
        assert_eq!(t.rows[0], vec!["x | y".to_string(), "z".to_string()]);
    }

    #[test]
    fn short_and_long_rows_are_normalized() {
        let t = table("| a | b |\n|---|---|\n| 1 |\n| 1 | 2 | 3 |\n");
        assert_eq!(t.rows, vec![vec!["1".to_string(), String::new()], vec!["1".to_string(), "2".to_string()]]);
    }

    #[test]
    fn mismatched_delimiter_is_not_a_table() {
        let blocks = split_tables("| a | b |\n|---|\n| 1 | 2 |\n");
        assert!(blocks.iter().all(|b| matches!(b, MdBlock::Text(_))));
    }

    #[test]
    fn no_outer_pipes() {
        let t = table("a | b\n--- | ---\n1 | 2\n");
        assert_eq!(t.header, vec!["a".to_string(), "b".to_string()]);
        assert_eq!(t.rows[0], vec!["1".to_string(), "2".to_string()]);
    }

    #[test]
    fn blank_line_ends_table() {
        let blocks = split_tables("| a |\n|---|\n| 1 |\n\n| not a row |\n");
        assert_eq!(blocks.len(), 2);
        assert!(matches!(&blocks[0], MdBlock::Table(t) if t.rows.len() == 1));
    }

    #[test]
    fn column_alignments() {
        let t = table("| a | b | c | d |\n|---|:--|:-:|--:|\n");
        use ColumnAlign::*;
        assert_eq!(t.aligns, vec![Left, Left, Center, Right]);
    }

    #[test]
    fn colon_only_is_not_a_delimiter() {
        let blocks = split_tables("| a |\n| : |\n");
        assert!(blocks.iter().all(|b| matches!(b, MdBlock::Text(_))));
    }

    #[test]
    fn fit_keeps_natural_widths_when_they_fit() {
        assert_eq!(fit_columns(&[100.0, 200.0], 400.0, 30.0), vec![100.0, 200.0]);
    }

    #[test]
    fn fit_shares_space_among_wide_columns() {
        // 100 fits its share; the two wide ones split the remaining 500.
        assert_eq!(fit_columns(&[100.0, 900.0, 700.0], 600.0, 30.0), vec![100.0, 250.0, 250.0]);
    }
}