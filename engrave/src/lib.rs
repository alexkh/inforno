//! Engraves a parsed ABC [`Document`] into positioned glyphs, lines and curves.
//!
//! All coordinates are in staff spaces (the distance between two staff lines),
//! with y growing downwards. Glyphs come from a SMuFL music font (Bravura),
//! where 1 em is 4 staff spaces. Every note, rest, bar and text also gets a
//! [`Hit`] box that maps back to its source text, for clicking and highlighting.
//!
//! ```
//! let doc = abc::parse("X:1\nT:Example\nK:D\nA2 B|\n");
//! let score = engrave::engrave(&doc, &engrave::Options::default());
//! assert!(!score.items.is_empty());
//! let svg = engrave::to_svg(&score, 10.0);
//! assert!(svg.starts_with("<svg"));
//! ```

mod glyphs;
mod layout;
mod lyrics;
pub mod music;
mod score_layout;
mod svg;
mod voices;

use abc::datatypes::{Diagnostic, Document, Span};

pub use svg::to_svg;

#[derive(Clone, Debug)]
pub struct Options {
    /// Page width in staff spaces.
    pub width: f32,
    /// Space around the music, in staff spaces.
    pub margin: f32,
    /// SVG font-family list for titles and other text.
    pub text_font: String,
    /// SVG font-family of the SMuFL music font.
    pub music_font: String,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            width: 120.0,
            margin: 3.0,
            text_font: "Liberation Serif, Times New Roman, serif".to_string(),
            music_font: "Bravura".to_string(),
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Rect {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
}

impl Rect {
    pub fn new(x0: f32, y0: f32, x1: f32, y1: f32) -> Rect {
        Rect {
            x0: x0.min(x1),
            y0: y0.min(y1),
            x1: x0.max(x1),
            y1: y0.max(y1),
        }
    }

    pub fn contains(&self, x: f32, y: f32) -> bool {
        self.x0 <= x && x <= self.x1 && self.y0 <= y && y <= self.y1
    }

    pub fn union(&self, o: &Rect) -> Rect {
        Rect::new(self.x0.min(o.x0), self.y0.min(o.y0), self.x1.max(o.x1), self.y1.max(o.y1))
    }

    pub fn expand(&self, by: f32) -> Rect {
        Rect::new(self.x0 - by, self.y0 - by, self.x1 + by, self.y1 + by)
    }

    pub fn translate(&self, dx: f32, dy: f32) -> Rect {
        Rect::new(self.x0 + dx, self.y0 + dy, self.x1 + dx, self.y1 + dy)
    }

    pub fn width(&self) -> f32 {
        self.x1 - self.x0
    }

    pub fn height(&self) -> f32 {
        self.y1 - self.y0
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Anchor {
    Start,
    Middle,
    End,
}

/// Noteheads that are not in the music font, drawn as paths.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Shape {
    /// Sharp notation: a black notehead with curled tips (quarter notes and shorter).
    RaisedBlack,
    /// Sharp notation: the curled notehead with an oval counter (half notes).
    RaisedHalf,
    /// Sharp notation: the curled whole note with a pointed counter.
    RaisedWhole,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum TextStyle {
    Normal,
    Italic,
    Bold,
}

/// One drawing primitive.
#[derive(Clone, Debug, PartialEq)]
pub enum Item {
    /// A music-font glyph with its origin (left end of the baseline) at `x, y`.
    /// `scale` is 1 for normal size and smaller for grace notes.
    Glyph { x: f32, y: f32, glyph: char, scale: f32 },
    /// A path notehead with its left edge at `x` and its centre line at `y`.
    Shape { x: f32, y: f32, shape: Shape, scale: f32 },
    Line { x1: f32, y1: f32, x2: f32, y2: f32, width: f32 },
    /// A filled polygon (beams).
    Polygon { points: Vec<(f32, f32)> },
    /// A tie or slur: a filled crescent between `p0` and `p3`, `thickness` thick in the middle.
    Curve { p0: (f32, f32), c1: (f32, f32), c2: (f32, f32), p3: (f32, f32), thickness: f32 },
    /// Text with its baseline at `y`.
    Text { x: f32, y: f32, text: String, size: f32, anchor: Anchor, style: TextStyle },
}

impl Item {
    /// An approximate bounding box.
    pub fn bounds(&self) -> Rect {
        match self {
            Item::Glyph { x, y, glyph, scale } => {
                let (up, down) = glyphs::extent(*glyph);
                let w = glyphs::width(*glyph).max(0.5);
                Rect::new(*x, y - up * scale, x + w * scale, y + down * scale)
            }
            Item::Shape { x, y, shape, scale } => {
                let h = shape.half_height() * scale;
                Rect::new(*x, y - h, x + shape.width() * scale, y + h)
            }
            Item::Line { x1, y1, x2, y2, width } => {
                Rect::new(*x1, *y1, *x2, *y2).expand(width / 2.0)
            }
            Item::Polygon { points } => points.iter().fold(
                Rect::new(points[0].0, points[0].1, points[0].0, points[0].1),
                |r, p| r.union(&Rect::new(p.0, p.1, p.0, p.1)),
            ),
            Item::Curve { p0, c1, c2, p3, thickness } => {
                // the control points bound the curve
                [c1, c2, p3]
                    .iter()
                    .fold(Rect::new(p0.0, p0.1, p0.0, p0.1), |r, p| {
                        r.union(&Rect::new(p.0, p.1, p.0, p.1))
                    })
                    .expand(*thickness)
            }
            Item::Text { x, y, text, size, anchor, .. } => {
                let w = text_width(text, *size);
                let x0 = match anchor {
                    Anchor::Start => *x,
                    Anchor::Middle => x - w / 2.0,
                    Anchor::End => x - w,
                };
                Rect::new(x0, y - size * 0.75, x0 + w, y + size * 0.25)
            }
        }
    }

    pub fn translate(&mut self, dx: f32, dy: f32) {
        match self {
            Item::Glyph { x, y, .. } | Item::Text { x, y, .. } | Item::Shape { x, y, .. } => {
                *x += dx;
                *y += dy;
            }
            Item::Line { x1, y1, x2, y2, .. } => {
                *x1 += dx;
                *x2 += dx;
                *y1 += dy;
                *y2 += dy;
            }
            Item::Polygon { points } => {
                for p in points.iter_mut() {
                    p.0 += dx;
                    p.1 += dy;
                }
            }
            Item::Curve { p0, c1, c2, p3, .. } => {
                for p in [p0, c1, c2, p3] {
                    p.0 += dx;
                    p.1 += dy;
                }
            }
        }
    }
}

/// The width of `text` set at `size` staff spaces in the text font. The
/// widths are those of Times, which Nimbus Roman and TeX Gyre Termes share;
/// characters outside ASCII are estimated.
pub fn text_width(text: &str, size: f32) -> f32 {
    text.chars().map(char_width).sum::<f32>() * size
}

/// The width of a character as a fraction of the font size.
fn char_width(c: char) -> f32 {
    // Times Roman, in thousandths, from the space to the tilde
    const WIDTHS: [u16; 95] = [
        250, 333, 408, 500, 500, 833, 778, 180, 333, 333, 500, 564, 250, 333, 250, 278, // space to /
        500, 500, 500, 500, 500, 500, 500, 500, 500, 500, 278, 278, 564, 564, 564, 444, // 0 to ?
        921, 722, 667, 667, 722, 611, 556, 722, 722, 333, 389, 722, 611, 889, 722, 722, // @ to O
        556, 722, 667, 556, 611, 722, 722, 944, 722, 722, 611, 333, 278, 333, 469, 500, // P to _
        333, 444, 500, 444, 500, 444, 333, 500, 500, 278, 278, 500, 278, 778, 500, 500, // ` to o
        500, 500, 333, 389, 278, 500, 500, 722, 500, 500, 444, 480, 200, 480, 541, // p to ~
    ];
    match c {
        ' '..='~' => WIDTHS[c as usize - 32] as f32 / 1000.0,
        c if c.is_uppercase() => 0.7,
        c if c.is_lowercase() => 0.47,
        _ => 0.5,
    }
}

/// A clickable area and the source text it came from.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Hit {
    pub rect: Rect,
    pub span: Span,
}

/// The engraved page.
#[derive(Clone, Debug, Default)]
pub struct Engraving {
    pub width: f32,
    pub height: f32,
    pub items: Vec<Item>,
    pub hits: Vec<Hit>,
    /// What is wrong with the music itself, such as voices of different
    /// lengths. The parser's diagnostics are in the document.
    pub diagnostics: Vec<Diagnostic>,
    pub text_font: String,
    pub music_font: String,
}

impl Engraving {
    /// The smallest hit box containing the point, if any.
    pub fn hit_at(&self, x: f32, y: f32) -> Option<&Hit> {
        self.hits
            .iter()
            .filter(|h| h.rect.contains(x, y))
            .min_by(|a, b| {
                let area = |h: &Hit| h.rect.width() * h.rect.height();
                area(a).total_cmp(&area(b))
            })
    }

    /// Hit boxes whose source text contains byte `pos`.
    pub fn hits_at_source(&self, pos: usize) -> impl Iterator<Item = &Hit> {
        self.hits.iter().filter(move |h| h.span.contains(pos))
    }
}

/// Lays out every tune of `doc`, one after the other.
pub fn engrave(doc: &Document, options: &Options) -> Engraving {
    layout::engrave(doc, options)
}
