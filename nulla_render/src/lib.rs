//! ABC text to pixels: parses with `abc`, lays out with `engrave` and draws
//! with resvg. It uses no GUI types, so apps on different egui versions can
//! share it; each turns a [`Raster`] into its own texture.

use std::path::PathBuf;
use std::sync::Arc;

use abc::datatypes::{Diagnostic, Document};
use engrave::{Engraving, Options};
use fontdb::Database;
use resvg::usvg;

//------------------------------------------------------------------------------
// fonts

/// Where a font comes from.
pub enum FontSource {
    File(PathBuf),
    Data(Vec<u8>),
}

/// The fonts a score is drawn with: a SMuFL music font (Bravura) and text fonts.
/// Cheap to clone.
#[derive(Clone)]
pub struct Fonts {
    db: Arc<Database>,
    missing: Vec<String>,
}

impl Fonts {
    /// Loads the given fonts. Files that cannot be read are listed in [`Fonts::missing`].
    pub fn new(sources: impl IntoIterator<Item = FontSource>) -> Fonts {
        let mut db = Database::new();
        let mut missing = vec![];
        for source in sources {
            match source {
                FontSource::File(path) => {
                    if let Err(e) = db.load_font_file(&path) {
                        eprintln!("font {}: {}", path.display(), e);
                        missing.push(path.display().to_string());
                    }
                }
                FontSource::Data(data) => db.load_font_data(data),
            }
        }
        // the score asks for "serif" last; point it at a font that is actually loaded
        let serif = db
            .faces()
            .flat_map(|f| f.families.iter().map(|(name, _)| name.clone()))
            .find(|name| name.contains("Roman") || name.contains("Serif"));
        if let Some(name) = serif {
            db.set_serif_family(name);
        }
        Fonts {
            db: Arc::new(db),
            missing,
        }
    }

    /// The font files that could not be loaded.
    pub fn missing(&self) -> &[String] {
        &self.missing
    }
}

//------------------------------------------------------------------------------
// scores

/// ABC text, parsed and engraved, with everything found wrong with it.
#[derive(Default)]
pub struct Score {
    doc: Document,
    pub engraving: Engraving,
    /// What the parser could not read and, once engraved, what the engraver
    /// found wrong, in source order.
    diagnostics: Vec<Diagnostic>,
}

impl Score {
    /// Parses `abc_str` and returns what could not be read.
    pub fn load_abc(&mut self, abc_str: &str) -> &[Diagnostic] {
        self.doc = abc::parse(abc_str);
        self.diagnostics = self.doc.diagnostics.clone();
        &self.doc.diagnostics
    }

    pub fn doc(&self) -> &Document {
        &self.doc
    }

    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }

    /// Lays out the parsed music `width` staff spaces wide.
    pub fn engrave(&mut self, width: f32) {
        let options = Options {
            width,
            ..Options::default()
        };
        self.engraving = engrave::engrave(&self.doc, &options);
        self.diagnostics = self.doc.diagnostics.clone();
        self.diagnostics.extend(self.engraving.diagnostics.iter().cloned());
        self.diagnostics.sort_by_key(|d| d.span.start);
    }
}

//------------------------------------------------------------------------------
// drawing

/// A drawn picture on a transparent background.
pub struct Raster {
    pub width: u32,
    pub height: u32,
    /// Size of the picture in its own units: staff spaces for a score.
    pub size: (f32, f32),
    /// Premultiplied RGBA, `width * height * 4` bytes.
    pub pixels: Vec<u8>,
}

impl Raster {
    /// Opaque RGBA: the picture on white paper.
    pub fn on_paper(&self) -> Vec<u8> {
        self.pixels
            .chunks_exact(4)
            // premultiplied, so each colour is at most the alpha
            .flat_map(|p| {
                let paper = 255 - p[3];
                [p[0] + paper, p[1] + paper, p[2] + paper, 255]
            })
            .collect()
    }

    /// Unmultiplied RGBA: white wherever something is drawn, with its coverage
    /// as alpha. Tint it with any colour when drawing, so a theme change needs
    /// no new picture.
    pub fn white_mask(&self) -> Vec<u8> {
        self.pixels
            .chunks_exact(4)
            .flat_map(|p| [255, 255, 255, p[3]])
            .collect()
    }
}

/// Draws an engraving at `pixels_per_space` pixels per staff space.
pub fn rasterize(engraving: &Engraving, fonts: &Fonts, pixels_per_space: f32) -> Option<Raster> {
    rasterize_svg(&engrave::to_svg(engraving, 1.0), fonts, pixels_per_space)
}

/// Draws SVG at `scale` pixels per SVG unit, at most 8192 pixels on its
/// longer side so that GPUs accept it as a texture.
pub fn rasterize_svg(svg: &str, fonts: &Fonts, scale: f32) -> Option<Raster> {
    let mut options = usvg::Options::default();
    options.fontdb = fonts.db.clone();
    let tree = match usvg::Tree::from_str(svg, &options) {
        Ok(tree) => tree,
        Err(error) => {
            eprintln!("Failed to parse SVG: {}", error);
            return None;
        }
    };
    let size = tree.size();
    let scale = scale.min(8192.0 / size.width().max(size.height()));
    let width = ((size.width() * scale).ceil() as u32).max(1);
    let height = ((size.height() * scale).ceil() as u32).max(1);

    let mut pixmap = tiny_skia::Pixmap::new(width, height)?;
    resvg::render(&tree, tiny_skia::Transform::from_scale(scale, scale), &mut pixmap.as_mut());
    Some(Raster {
        width,
        height,
        size: (size.width(), size.height()),
        pixels: pixmap.take(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use abc::datatypes::MusicSymbol;

    #[test]
    fn slur() {
        let mut score = Score::default();
        score.load_abc("X:1\nT:Testing Slurs\nK:D\n(a3/2bcd)\n");
        let line = score.doc().tunes[0].music_lines().next().expect("a music line");
        assert_eq!(line.symbols[0].kind, MusicSymbol::SlurBegin);
        assert!(score.diagnostics().is_empty());
    }

    #[test]
    fn a_score_is_drawn() {
        let mut score = Score::default();
        score.load_abc("X:1\nK:C\nCDEF|\n");
        score.engrave(60.0);
        // no fonts: the staff lines and bar line are still drawn
        let r = rasterize(&score.engraving, &Fonts::new(Vec::new()), 4.0).expect("a picture");
        assert_eq!(r.pixels.len(), (r.width * r.height * 4) as usize);
        assert!(r.pixels.chunks_exact(4).any(|p| p[3] > 0));
        assert!(r.white_mask().chunks_exact(4).all(|p| p[0] == 255));
        assert!(r.on_paper().chunks_exact(4).all(|p| p[3] == 255));
    }
}
