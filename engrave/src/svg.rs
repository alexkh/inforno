//! Writes an [`Engraving`] as SVG.
//!
//! [`to_svg`] keeps the view box in staff spaces, so the picture scales
//! cleanly: for files. [`to_pixel_svg`] is for drawing at one size. Its
//! units are pixels, and the straight lines of the score (staff and ledger
//! lines, stems, bar lines, brackets) lie on the pixel grid, each a whole
//! number of pixels thick and drawn without antialiasing, so they come out
//! sharp. Glyphs keep their smooth outlines but stand at the same place
//! against the grid as the staff lines, so a notehead on a line is centred
//! on it and every notehead looks the same.

use crate::{Anchor, Engraving, Item, TextStyle};
use std::fmt::Write;

pub fn to_svg(e: &Engraving, px_per_space: f32) -> String {
    let header = format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{:.1}" height="{:.1}" viewBox="0 0 {:.3} {:.3}">"#,
        e.width * px_per_space,
        e.height * px_per_space,
        e.width,
        e.height
    );
    write_svg(e, header, &Grid::exact())
}

/// The engraving in pixels, `px_per_space` of them to a staff space. That
/// should be a whole number, or the staff lines can't all lie on the grid.
pub fn to_pixel_svg(e: &Engraving, px_per_space: f32) -> String {
    let (w, h) = ((e.width * px_per_space).ceil().max(1.0), (e.height * px_per_space).ceil().max(1.0));
    let header = format!(r#"<svg xmlns="http://www.w3.org/2000/svg" width="{w}" height="{h}" viewBox="0 0 {w} {h}">"#);
    write_svg(e, header, &Grid::pixels(px_per_space))
}

/// How positions in staff spaces become SVG coordinates.
struct Grid {
    /// SVG units per staff space.
    scale: f32,
    /// Lay straight lines and glyph origins on whole SVG units (pixels).
    snap: bool,
    /// Where the middle of a staff line falls within a pixel: 0.5 when the
    /// lines are an odd number of pixels thick, 0 when even. Glyph origins
    /// get the same, so a notehead on a line is centred on it.
    phase: f32,
}

impl Grid {
    fn exact() -> Grid {
        Grid { scale: 1.0, snap: false, phase: 0.0 }
    }

    fn pixels(px_per_space: f32) -> Grid {
        let staff_line = Self::thickness(crate::glyphs::STAFF_LINE * px_per_space);
        Grid {
            scale: px_per_space,
            snap: true,
            phase: if staff_line % 2.0 == 1.0 { 0.5 } else { 0.0 },
        }
    }

    /// A line `px` pixels thick, as a whole number of pixels, at least one.
    fn thickness(px: f32) -> f32 {
        px.round().max(1.0)
    }

    /// A length or a position that isn't snapped.
    fn len(&self, v: f32) -> f32 {
        v * self.scale
    }

    /// A position snapped to whole pixels: an x, or a text baseline.
    fn at(&self, v: f32) -> f32 {
        if self.snap { (v * self.scale).round() } else { v * self.scale }
    }

    /// A vertical position snapped like the middle of a staff line: a
    /// glyph's origin, or the end of a vertical line.
    fn staff_y(&self, y: f32) -> f32 {
        if self.snap {
            (y * self.scale - self.phase).round() + self.phase
        } else {
            y * self.scale
        }
    }

    /// A band `width` thick around the middle `c`, as its start and its
    /// thickness in whole pixels.
    fn band(&self, c: f32, width: f32) -> (f32, f32) {
        let t = Self::thickness(width * self.scale);
        ((c * self.scale - t / 2.0).round(), t)
    }
}

fn write_svg(e: &Engraving, header: String, grid: &Grid) -> String {
    let mut s = header;
    s.reserve(64 * e.items.len() + 256);
    s.push('\n');
    let music = escape(&e.music_font);
    let text_font = escape(&e.text_font);

    for item in &e.items {
        match item {
            Item::Glyph { x, y, glyph, scale } => {
                let _ = writeln!(
                    s,
                    r#"<text x="{:.3}" y="{:.3}" font-family="{}" font-size="{:.3}">&#x{:X};</text>"#,
                    grid.at(*x),
                    grid.staff_y(*y),
                    music,
                    grid.len(4.0 * scale),
                    *glyph as u32
                );
            }
            Item::Shape { x, y, shape, scale } => {
                let _ = writeln!(
                    s,
                    r#"<path transform="translate({:.3},{:.3}) scale({:.5})" fill-rule="evenodd" d="{}"/>"#,
                    grid.at(x + shape.width() * scale / 2.0),
                    grid.staff_y(*y),
                    grid.len(*scale) / crate::glyphs::SHAPE_UNITS,
                    shape.path()
                );
            }
            // a straight line across or down the page: a band of whole
            // pixels, drawn without antialiasing
            Item::Line { x1, y1, x2, y2, width } if grid.snap && (x1 == x2 || y1 == y2) => {
                let (x, y, w, h) = if y1 == y2 {
                    let (top, t) = grid.band(*y1, *width);
                    let (a, b) = (grid.at(x1.min(*x2)), grid.at(x1.max(*x2)));
                    (a, top, (b - a).max(1.0), t)
                } else {
                    // ending on a staff line, it covers that line's pixels
                    let (left, t) = grid.band(*x1, *width);
                    let a = grid.staff_y(y1.min(*y2)).floor();
                    let b = grid.staff_y(y1.max(*y2)).ceil();
                    (left, a, t, (b - a).max(1.0))
                };
                let _ = writeln!(s, r#"<rect x="{x}" y="{y}" width="{w}" height="{h}" shape-rendering="crispEdges"/>"#);
            }
            Item::Line { x1, y1, x2, y2, width } => {
                let _ = writeln!(
                    s,
                    r##"<line x1="{:.3}" y1="{:.3}" x2="{:.3}" y2="{:.3}" stroke="#000" stroke-width="{:.3}"/>"##,
                    grid.len(*x1),
                    grid.len(*y1),
                    grid.len(*x2),
                    grid.len(*y2),
                    grid.len(*width)
                );
            }
            Item::Polygon { points } => {
                s.push_str(r#"<polygon points=""#);
                for (x, y) in points {
                    let _ = write!(s, "{:.3},{:.3} ", grid.len(*x), grid.len(*y));
                }
                s.push_str("\"/>\n");
            }
            Item::Curve { p0, c1, c2, p3, thickness } => {
                // the inner edge of the crescent sits `thickness` closer to the chord
                let mx = (p0.0 + p3.0) / 2.0;
                let my = (p0.1 + p3.1) / 2.0;
                let bx = (c1.0 + c2.0) / 2.0 - mx;
                let by = (c1.1 + c2.1) / 2.0 - my;
                let len = (bx * bx + by * by).sqrt().max(1e-3);
                let (nx, ny) = (bx / len * thickness * 1.33, by / len * thickness * 1.33);
                let p = [
                    p0.0, p0.1, c1.0, c1.1, c2.0, c2.1, p3.0, p3.1,
                    c2.0 - nx, c2.1 - ny, c1.0 - nx, c1.1 - ny, p0.0, p0.1,
                ]
                .map(|v| grid.len(v));
                let _ = writeln!(
                    s,
                    r##"<path d="M{:.3},{:.3} C{:.3},{:.3} {:.3},{:.3} {:.3},{:.3} C{:.3},{:.3} {:.3},{:.3} {:.3},{:.3} Z" stroke="#000" stroke-width="{:.3}"/>"##,
                    p[0], p[1], p[2], p[3], p[4], p[5], p[6], p[7], p[8], p[9], p[10], p[11], p[12], p[13],
                    grid.len(0.04)
                );
            }
            Item::Text { x, y, text, size, anchor, style } => {
                let anchor = match anchor {
                    Anchor::Start => "start",
                    Anchor::Middle => "middle",
                    Anchor::End => "end",
                };
                let style = match style {
                    TextStyle::Normal => "",
                    TextStyle::Italic => r#" font-style="italic""#,
                    TextStyle::Bold => r#" font-weight="bold""#,
                };
                let _ = writeln!(
                    s,
                    r#"<text x="{:.3}" y="{:.3}" font-family="{}" font-size="{:.3}" text-anchor="{}"{}>{}</text>"#,
                    grid.at(*x),
                    grid.at(*y),
                    text_font,
                    grid.len(*size),
                    anchor,
                    style,
                    escape(text)
                );
            }
        }
    }
    s.push_str("</svg>\n");
    s
}

fn escape(t: &str) -> String {
    let mut out = String::with_capacity(t.len());
    for c in t.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn straight_lines_lie_on_the_pixel_grid() {
        let mut e = Engraving::default();
        e.width = 10.0;
        e.height = 6.0;
        // a staff line and a stem at awkward places
        e.items.push(Item::Line { x1: 0.3, y1: 1.37, x2: 9.1, y2: 1.37, width: crate::glyphs::STAFF_LINE });
        e.items.push(Item::Line { x1: 2.21, y1: 1.37, x2: 2.21, y2: 5.37, width: crate::glyphs::STEM });
        let svg = to_pixel_svg(&e, 9.0);
        let value = |line: &str, name: &str| -> f32 {
            let key = format!(" {name}=\"");
            let start = line.find(&key).expect("attribute") + key.len();
            line[start..].split('"').next().unwrap().parse().unwrap()
        };
        let rects: Vec<&str> = svg.lines().filter(|l| l.starts_with("<rect")).collect();
        assert_eq!(rects.len(), 2);
        for r in rects {
            for name in ["x", "y", "width", "height"] {
                assert_eq!(value(r, name).fract(), 0.0, "{name} in {r}");
            }
            assert!(r.contains("crispEdges"));
        }
    }
}
