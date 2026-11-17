//! Writes an [`Engraving`] as SVG. The view box is in staff spaces, so the
//! picture scales cleanly; `px_per_space` only sets the default size.

use crate::{Anchor, Engraving, Item, TextStyle};
use std::fmt::Write;

pub fn to_svg(e: &Engraving, px_per_space: f32) -> String {
    let mut s = String::with_capacity(64 * e.items.len() + 256);
    let _ = write!(
        s,
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{:.1}" height="{:.1}" viewBox="0 0 {:.3} {:.3}">"#,
        e.width * px_per_space,
        e.height * px_per_space,
        e.width,
        e.height
    );
    s.push('\n');
    let music = escape(&e.music_font);
    let text_font = escape(&e.text_font);

    for item in &e.items {
        match item {
            Item::Glyph { x, y, glyph, scale } => {
                let _ = writeln!(
                    s,
                    r#"<text x="{:.3}" y="{:.3}" font-family="{}" font-size="{:.3}">&#x{:X};</text>"#,
                    x,
                    y,
                    music,
                    4.0 * scale,
                    *glyph as u32
                );
            }
            Item::Shape { x, y, shape, scale } => {
                let _ = writeln!(
                    s,
                    r#"<path transform="translate({:.3},{:.3}) scale({:.5})" fill-rule="evenodd" d="{}"/>"#,
                    x + shape.width() * scale / 2.0,
                    y,
                    scale / crate::glyphs::SHAPE_UNITS,
                    shape.path()
                );
            }
            Item::Line { x1, y1, x2, y2, width } => {
                let _ = writeln!(
                    s,
                    r##"<line x1="{:.3}" y1="{:.3}" x2="{:.3}" y2="{:.3}" stroke="#000" stroke-width="{:.3}"/>"##,
                    x1, y1, x2, y2, width
                );
            }
            Item::Polygon { points } => {
                s.push_str(r#"<polygon points=""#);
                for (x, y) in points {
                    let _ = write!(s, "{:.3},{:.3} ", x, y);
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
                let _ = writeln!(
                    s,
                    r##"<path d="M{:.3},{:.3} C{:.3},{:.3} {:.3},{:.3} {:.3},{:.3} C{:.3},{:.3} {:.3},{:.3} {:.3},{:.3} Z" stroke="#000" stroke-width="0.04"/>"##,
                    p0.0,
                    p0.1,
                    c1.0,
                    c1.1,
                    c2.0,
                    c2.1,
                    p3.0,
                    p3.1,
                    c2.0 - nx,
                    c2.1 - ny,
                    c1.0 - nx,
                    c1.1 - ny,
                    p0.0,
                    p0.1
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
                    x,
                    y,
                    text_font,
                    size,
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
