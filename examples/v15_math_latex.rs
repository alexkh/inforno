use eframe::egui;
use egui_commonmark::{CommonMarkCache, CommonMarkViewer};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use typst::{
    diag::{FileError, FileResult},
    foundations::{Bytes, Datetime},
    syntax::{FileId, Source},
    text::{Font, FontBook},
    utils::LazyHash,
    Library, LibraryExt,
    World,
};

fn main() -> eframe::Result<()> {
    eframe::run_native(
        "Typst + MiTeX Embedded Sandbox",
        eframe::NativeOptions::default(),
        Box::new(|cc| {
            egui_extras::install_image_loaders(&cc.egui_ctx);
            Ok(Box::new(MyApp::default()))
        }),
    )
}

// ==========================================
// TYPST EMBEDDED WORLD BOILERPLATE
// ==========================================

/// A minimal environment for the Typst compiler to run inside our app.
struct TypstMathWorld {
    library: LazyHash<Library>,
    book: LazyHash<FontBook>,
    fonts: Vec<Font>,
    source: Source,
}

impl TypstMathWorld {
    fn new(typst_source: String) -> Self {
        let mut book = FontBook::new();
        let mut fonts = Vec::new();

        // Iterate through all the embedded fonts safely
        for font_data in typst_assets::fonts() {
            // Convert the raw bytes into a format Typst's Font::iter expects
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

// Typst requires this trait to know how to resolve files, fonts, and time.
impl World for TypstMathWorld {
    fn library(&self) -> &LazyHash<Library> { &self.library }
    fn book(&self) -> &LazyHash<FontBook> { &self.book }

    // Updated to return a FileId instead of Source
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

// ==========================================
// RENDER LOGIC
// ==========================================

fn compile_math_to_svg_embedded(math: &str, is_inline: bool) -> Option<Vec<u8>> {
    let typst_math = mitex::convert_math(math, None).ok()?;

    let actually_inline = is_inline && !math.contains("\\displaystyle");
    let block = if actually_inline { "false" } else { "true" };

    // We removed the font_size completely. Typst now defaults to 11pt.
    let typst_source = format!(
        r##"
#set page(width: auto, height: auto, margin: 0pt, fill: none)
#set text(fill: rgb("#FFFFFF"))
#set math.equation(block: {block})

$ {typst_math} $
        "##,
        block = block,
        typst_math = typst_math
    );

    let world = TypstMathWorld::new(typst_source);

    let document: typst::layout::PagedDocument = typst::compile(&world).output.ok()?;

    if document.pages.is_empty() { return None; }
    let svg_string = typst_svg::svg(&document.pages[0]);

    Some(svg_string.into_bytes())
}

// ==========================================
// GUI APP LOGIC
// ==========================================

struct MyApp {
    cache: CommonMarkCache,
    markdown: String,
    math_cache: Rc<RefCell<HashMap<String, Arc<[u8]>>>>,
}

impl Default for MyApp {
    fn default() -> Self {
        let markdown_text = r#"
The landscape of modern mathematics is full of fascinating, unsolved mysteries. Many of these problems are incredibly complex to even define, while others are simple enough to explain to a child but have stumped the brightest minds for centuries.

* **The Riemann Hypothesis:** Proposed in 1859, this is often considered the most important open problem in pure mathematics. It deals with the distribution of prime numbers and is rooted in the behavior of the Riemann zeta function:

$$\zeta(s) = \sum_{n=1}^{\infty} \frac{1}{n^s}$$


### 2. The Sophie Germain Prime Conjecture
A Sophie Germain prime is a prime number $p$ where $2p + 1$ is also a prime number. For example, 11 is prime, and $2(11) + 1 = 23$, which is also prime.

The $6k \pm 1$ rule strictly dictates where these primes can live. If $p$ is of the form $6k + 1$, then $2p + 1$ becomes $2(6k + 1) + 1 = 12k + 3$. You can factor out a 3 to get $3(4k + 1)$, meaning the result is *always* divisible by 3 and can never be prime! Therefore, every Sophie Germain prime greater than 3 **must strictly be of the form $6k - 1$**.
* **The Unsolved Problem:** Are there infinitely many Sophie Germain primes? Like the twin prime conjecture, this remains heavily suspected but entirely unproven.
        "#;

        Self {
            cache: CommonMarkCache::default(),
            markdown: markdown_text.to_string(),
            math_cache: Rc::new(RefCell::new(HashMap::new())),
        }
    }
}

impl eframe::App for MyApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::TopBottomPanel::top("top_panel").show(ctx, |ui| {
            egui::widgets::global_theme_preference_switch(ui);
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                let local_math_cache = Rc::clone(&self.math_cache);

                CommonMarkViewer::new()
                    .render_math_fn(Some(&mut move |ui, math, is_inline| {
                        let mut cache_map = local_math_cache.borrow_mut();

                        let svg_bytes = cache_map.entry(math.to_string()).or_insert_with(|| {
                            let bytes = compile_math_to_svg_embedded(math, is_inline).unwrap_or_default();
                            bytes.into()
                        });

                        let uri = format!("bytes://math_{}.svg", egui::Id::new(math).value());

                        let mut image = egui::Image::new(egui::ImageSource::Bytes {
                            uri: uri.into(),
                            bytes: egui::load::Bytes::Shared(svg_bytes.clone()),
                        });

                        image = image.tint(ui.visuals().text_color());

                        // ==========================================
                        // DYNAMIC NORMALIZATION
                        // ==========================================
                        // 1. Get egui's current default body text size (usually ~14.0px)
                        let egui_font_size = ui.text_style_height(&egui::TextStyle::Body);

                        // 2. Divide by Typst's default 11pt.
                        // Note: We multiply by a tiny optical adjustment (e.g., 1.15) because math
                        // fonts usually look a bit physically smaller than standard UI letters.
                        let optical_adjustment = 0.8;
                        let scale_factor = (egui_font_size / 11.0) * optical_adjustment;

                        image = image.fit_to_original_size(scale_factor);

                        let actually_inline = is_inline && !math.contains("\\displaystyle");

                        if !actually_inline {
                            image = image.max_width(ui.available_width());
                        }

                        ui.add(image);
                    }))
                    .show(ui, &mut self.cache, &self.markdown);

            });
        });
    }
}