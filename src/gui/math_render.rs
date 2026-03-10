use typst::{
    diag::{FileError, FileResult},
    foundations::{Bytes, Datetime},
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

pub fn compile_math_to_svg_embedded(math: &str, is_inline: bool) -> Option<Vec<u8>> {
    let typst_math = mitex::convert_math(math, None).ok()?;

    let actually_inline = is_inline && !math.contains("\\displaystyle");
    let block = if actually_inline { "false" } else { "true" };

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