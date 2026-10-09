//! LaTeX math → SVG for chat markdown: converts LaTeX to Typst with MiTeX,
//! then compiles it in a minimal embedded Typst world.

use std::sync::OnceLock;

use regex::Regex;
use typst::{
    diag::{FileError, FileResult},
    foundations::{Bytes, Datetime},
    introspection::Tag,
    layout::{Abs, Frame, FrameItem, Point},
    syntax::{FileId, Source},
    text::{Font, FontBook},
    utils::LazyHash,
    Library, LibraryExt,
    World,
};

/// A minimal environment for the Typst compiler to run inside our app.
pub struct TypstMathWorld {
    library: LazyHash<Library>,
    book: LazyHash<FontBook>,
    fonts: Vec<Font>,
    source: Source,
}

impl TypstMathWorld {
    pub fn new(typst_source: String) -> Self {
        let mut book = FontBook::new();
        let mut fonts = Vec::new();

        // Iterate through all the embedded fonts safely
        for font_data in typst_assets::fonts() {
            let bytes = Bytes::new(font_data.to_vec());
            for font in Font::iter(bytes) {
                book.push(font.info().clone());
                fonts.push(font);
            }
        }

        Self {
            library: LazyHash::new(Library::builder().build()),
            book: LazyHash::new(book),
            fonts,
            source: Source::detached(typst_source),
        }
    }
}

impl World for TypstMathWorld {
    fn library(&self) -> &LazyHash<Library> { &self.library }
    fn book(&self) -> &LazyHash<FontBook> { &self.book }
    fn main(&self) -> FileId { self.source.id() }

    fn source(&self, id: FileId) -> FileResult<Source> {
        if id == self.source.id() {
            Ok(self.source.clone())
        } else {
            Err(FileError::NotFound(id.vpath().as_rootless_path().into()))
        }
    }

    fn file(&self, id: FileId) -> FileResult<Bytes> {
        Err(FileError::NotFound(id.vpath().as_rootless_path().into()))
    }

    fn font(&self, index: usize) -> Option<Font> {
        self.fonts.get(index).cloned()
    }

    fn today(&self, _offset: Option<i64>) -> Option<Datetime> { None }
}

/// A compiled formula plus the numbers needed to place it inline.
pub struct MathSvg {
    pub svg: Vec<u8>,
    pub layout: MathLayout,
}

/// Where a formula's parts are, in Typst pt.
#[derive(Clone, Default)]
pub struct MathLayout {
    /// Size of the whole formula.
    pub size_pt: egui::Vec2,
    /// Distance from the top edge down to the baseline.
    pub baseline_pt: f32,
    /// Where an over-wide inline formula may be split across lines, from
    /// its left edge (see `break_points`). Empty for display math.
    pub breaks_pt: Vec<f32>,
}

pub fn compile_math_to_svg_embedded(math: &str, is_inline: bool) -> Option<MathSvg> {
    let safe_math = normalize_latex(math);

    // 1. Catch Mitex conversion errors
    let typst_math = match mitex::convert_math(&safe_math, None) {
        Ok(t) => fix_mitex_output(&t),
        Err(e) => {
            eprintln!("MiTeX Error on formula [ {} ]: {:?}", math, e);
            return None;
        }
    };

    let actually_inline = is_inline && !math.contains("\\displaystyle");

    // Typst decides inline vs display by syntax alone: `$x$` is inline,
    // `$ x $` (spaces inside) is display, and a `#set math.equation(block:)`
    // rule can't override that.
    let equation = if actually_inline {
        format!("${}$", typst_math.trim())
    } else {
        format!("$ {} $", typst_math)
    };

    // Prepend the helpers MiTeX's output calls (see mitex_scope.typ).
    // `bounds` edges size the page to the actual glyphs; the default
    // (cap-height..baseline) crops inline fractions, limits and descenders.
    let typst_source = format!(
        r##"
{scope}
#set page(width: auto, height: auto, margin: 0pt, fill: none)
#set text(fill: rgb("#FFFFFF"), top-edge: "bounds", bottom-edge: "bounds")

{equation}
        "##,
        scope = MITEX_SCOPE,
        equation = equation,
    );

    let world = TypstMathWorld::new(typst_source);

    // 2. Catch Typst compilation errors
    let compiled = typst::compile(&world);
    let document: typst::layout::PagedDocument = match compiled.output {
        Ok(doc) => doc,
        Err(errs) => {
            eprintln!("Typst Compilation Error on formula [ {} ]:", math);
            // Print the raw diagnostic array so we can see exactly what Typst didn't like
            eprintln!("{:#?}", errs);
            return None;
        }
    };

    if document.pages.is_empty() {
        eprintln!("Typst rendered 0 pages for formula [ {} ]", math);
        return None;
    }

    let page = &document.pages[0];
    let size = page.frame.size();

    // Only inline placement uses the baseline; display math falls back to the bottom edge.
    let baseline = equation_baseline(&page.frame).unwrap_or_else(|| {
        if actually_inline {
            eprintln!("No baseline found for inline formula [ {} ]", math);
        }
        size.y
    });

    let breaks = if actually_inline { break_points(&page.frame, baseline) } else { Vec::new() };

    Some(MathSvg {
        svg: typst_svg::svg(page).into_bytes(),
        layout: MathLayout {
            size_pt: egui::vec2(size.x.to_pt() as f32, size.y.to_pt() as f32),
            baseline_pt: baseline.to_pt() as f32,
            breaks_pt: breaks.iter().map(|b| b.to_pt() as f32).collect(),
        },
    })
}

/// The equation's baseline, measured from the top of `frame`: the equation's
/// start tag sits on it. Searched recursively, because Typst sometimes nests
/// the line in a group.
fn equation_baseline(frame: &Frame) -> Option<Abs> {
    frame.items().find_map(|(pos, item)| match item {
        FrameItem::Tag(Tag::Start(elem, ..)) if elem.func().name() == "equation" => Some(pos.y),
        FrameItem::Group(group) => equation_baseline(&group.frame).map(|y| pos.y + y),
        _ => None,
    })
}

/// One drawn item of a formula, flattened to absolute coordinates.
struct Leaf {
    x0: Abs,
    x1: Abs,
    /// For a single-character glyph: the character, its baseline y and size.
    glyph: Option<(char, Abs, Abs)>,
}

/// Flattens the frame into leaves. A transformed group (scaled, rotated)
/// stays one opaque leaf, so nothing can be cut through it.
fn collect_leaves(frame: &Frame, offset: Point, out: &mut Vec<Leaf>) {
    for (pos, item) in frame.items() {
        let p = offset + *pos;
        let (x0, x1, glyph) = match item {
            FrameItem::Group(g) if g.transform.is_identity() => {
                collect_leaves(&g.frame, p, out);
                continue;
            }
            FrameItem::Group(g) => (p.x, p.x + g.frame.width(), None),
            FrameItem::Text(t) => {
                let mut chars = t.text.chars();
                let glyph = match (chars.next(), chars.next()) {
                    (Some(c), None) => Some((c, p.y, t.size)),
                    _ => None,
                };
                (p.x, p.x + t.width(), glyph)
            }
            FrameItem::Shape(shape, _) => (p.x, p.x + shape.geometry.bbox_size().x, None),
            FrameItem::Image(_, size, _) => (p.x, p.x + size.x, None),
            _ => continue,
        };
        out.push(Leaf { x0, x1, glyph });
    }
}

/// Relations and binary operators: the only places TeX breaks inline math.
fn is_break_operator(c: char) -> bool {
    matches!(c,
        '=' | '≠' | '<' | '>' | '≤' | '≥' | '≈' | '≡' | '∼' | '≃' | '≅' | '∝' | '≪' | '≫'
        | '∈' | '∉' | '⊂' | '⊃' | '⊆' | '⊇' | '→' | '←' | '↔' | '⇒' | '⇐' | '⇔' | '↦'
        | '⟶' | '⟹' | '⟺' | '≔'
        | '+' | '−' | '±' | '∓' | '×' | '·' | '⋅' | '∗' | '∘' | '÷' | '∪' | '∩' | '⊕' | '⊗' | '∧' | '∨')
}

/// Where an inline formula may be broken across lines, in pt from its left
/// edge: right before the item that follows a top-level relation or binary
/// operator, like TeX. "Top-level" means full size and on the baseline (so
/// not inside an exponent, subscript or fraction), and nothing else drawn
/// may span the cut (so never through a fraction bar, root or brace). The
/// space after the operator stays with the left piece, where a line end
/// hides it. An operator not followed by any space (a unary minus) is skipped.
fn break_points(frame: &Frame, baseline: Abs) -> Vec<Abs> {
    let mut leaves = Vec::new();
    collect_leaves(frame, Point::zero(), &mut leaves);

    let main_size = leaves
        .iter()
        .filter_map(|l| l.glyph)
        .filter(|&(_, y, _)| (y - baseline).abs() < Abs::pt(0.01))
        .map(|(_, _, size)| size)
        .fold(Abs::zero(), Abs::max);
    let eps = Abs::pt(0.01);

    let mut breaks = Vec::new();
    for op in &leaves {
        let Some((c, y, size)) = op.glyph else { continue };
        if !is_break_operator(c) || (y - baseline).abs() > eps || (size - main_size).abs() > eps {
            continue;
        }
        // The cut sits where the next item starts.
        let Some(cut) = leaves.iter().map(|l| l.x0).filter(|&x| x > op.x1 - eps).fold(None, |m: Option<Abs>, x| Some(m.map_or(x, |m| m.min(x)))) else {
            continue;
        };
        let spaced = cut - op.x1 > Abs::pt(0.5);
        let clear = leaves.iter().all(|l| l.x1 <= op.x1 + eps || l.x0 >= cut - eps);
        if spaced && clear && cut < frame.width() - eps {
            breaks.push(cut);
        }
    }
    breaks.sort_by(|a, b| a.partial_cmp(b).unwrap());
    breaks.dedup_by(|a, b| (*a - *b).abs() < eps);
    breaks
}

/// Typst definitions for every helper MiTeX's output may call.
const MITEX_SCOPE: &str = include_str!("mitex_scope.typ");

/// Patches the two kinds of MiTeX output a Typst prelude can't fix:
/// - `\set{...}` becomes `set(...)`, but `set` is a Typst keyword and
///   can't be defined as a function, so it's renamed to `mitexset(`.
/// - `\dashrightarrow`/`\dashleftarrow` become `arrow.r.dash`/`arrow.l.dash`,
///   which Typst 0.14 renamed to `arrow.r.dashed`/`arrow.l.dashed`.
fn fix_mitex_output(typst_math: &str) -> String {
    static RE_SET: OnceLock<Regex> = OnceLock::new();
    static RE_DASH_ARROW: OnceLock<Regex> = OnceLock::new();

    let re_set = RE_SET.get_or_init(|| Regex::new(r"\bset\(").unwrap());
    let re_dash = RE_DASH_ARROW.get_or_init(|| Regex::new(r"arrow\.([lr])\.dash\b").unwrap());

    let fixed = re_set.replace_all(typst_math, "mitexset(");
    re_dash.replace_all(&fixed, "arrow.$1.dashed").into_owned()
}

/// Rewrites LaTeX that MiTeX doesn't understand into equivalent LaTeX it
/// does. (Adding commands to MiTeX's command table would be cleaner, but
/// MiTeX 0.2.4 only uses a custom table for parsing, not for conversion.)
/// Commands are matched as whole tokens, so `\\mbox` (a line break, then the
/// word "mbox") is left alone.
fn normalize_latex(math: &str) -> String {
    static RE_TOKEN: OnceLock<Regex> = OnceLock::new();
    static RE_ENV: OnceLock<Regex> = OnceLock::new();
    let re_token = RE_TOKEN.get_or_init(|| Regex::new(r"\\(?:[A-Za-z]+|.)").unwrap());
    let re_env = RE_ENV.get_or_init(|| Regex::new(r"\\(begin|end)\{(multline|eqnarray)(\*?)\}").unwrap());

    let math = math.replace("^*", "^\\ast").replace("^{*}", "^{\\ast}");

    let math = re_token.replace_all(&math, |caps: &regex::Captures| {
        match &caps[0] {
            "\\mathscr" => "\\mathcal", // closest available style
            "\\mbox" => "\\text",
            "\\iiiint" => "⨌",
            "\\idotsint" => "\\int\\cdots\\int",
            "\\varliminf" => "\\mathop{\\underline{\\lim}}",
            "\\varlimsup" => "\\mathop{\\overline{\\lim}}",
            "\\overparen" => "\\overgroup",
            "\\underparen" => "\\undergroup",
            "\\euro" => "€",
            other => other,
        }
        .to_string()
    });

    // multline → gather (MiTeX has no gather*; numbering is never shown anyway),
    // eqnarray → align (its `&=&` is just two alignment points).
    re_env
        .replace_all(&math, |caps: &regex::Captures| match &caps[2] {
            "multline" => format!("\\{}{{gather}}", &caps[1]),
            _ => format!("\\{}{{align{}}}", &caps[1], &caps[3]),
        })
        .into_owned()
}
