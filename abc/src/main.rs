  //============================================================================
 // abc/src/main.rs
//==============================================================================

//! `cargo run --release -- tune.abc` prints an outline of the file and every
//! diagnostic, with the line and column it refers to.

fn main() {
    let Some(path) = std::env::args().nth(1) else {
        eprintln!("usage: abc <file.abc>");
        std::process::exit(2);
    };
    let source = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{}: {}", path, e);
            std::process::exit(1);
        }
    };

    let doc = abc::parse(&source);
    println!("file header: {} fields", doc.file_header.len());
    for tune in &doc.tunes {
        let symbols: usize = tune.music_lines().map(|l| l.symbols.len()).sum();
        println!(
            "X:{} {:?}: {} music lines, {} symbols",
            tune.field('X').unwrap_or("?"),
            tune.field('T').unwrap_or(""),
            tune.music_lines().count(),
            symbols
        );
    }
    for d in &doc.diagnostics {
        let (line, col) = line_col(&source, d.span.start);
        println!("{}:{}:{}: {}", path, line, col, d.message);
    }
}

/// 1-based line and column (in characters) of byte offset `pos`.
fn line_col(source: &str, pos: usize) -> (usize, usize) {
    let before = &source[..pos.min(source.len())];
    let line = before.matches('\n').count() + 1;
    let col = before.rsplit('\n').next().unwrap_or("").chars().count() + 1;
    (line, col)
}
