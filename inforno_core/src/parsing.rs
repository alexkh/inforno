use std::sync::OnceLock;
use regex::Regex;

pub enum ContentChunk<'a> {
    Markdown(&'a str),
    Code {
        lang: &'a str,
        code: &'a str,
        filepath: Option<String>,
    }
}

pub fn parse_chunks(text: &str) -> Vec<ContentChunk<'_>> {
    static RE_CODE: OnceLock<Regex> = OnceLock::new();
    static RE_FILEPATH: OnceLock<Regex> = OnceLock::new();
    static RE_INNER_FILE: OnceLock<Regex> = OnceLock::new();

    let re_code = RE_CODE.get_or_init(|| {
        Regex::new(r"(?ms)^[ \t]{0,3}\x60{3}([a-zA-Z0-9]*)[ \t]*\r?\n(.*?)\r?\n[ \t]{0,3}\x60{3}[ \t]*(?:\r?\n|$)").unwrap()
    });

    // Broadened regex: Matches strings ending in ".rs" ANYWHERE in the text.
    let re_filepath = RE_FILEPATH.get_or_init(|| {
        Regex::new(r"(?i)([a-z0-9_/\.\-]+\.[a-z]+)").unwrap()
    });

    // Specifically targets filenames embedded directly inside the code block
    let re_inner_file = RE_INNER_FILE.get_or_init(|| {
        Regex::new(r"(?im)(?:^[ \t]*(?://|/\*|#)?[ \t]*(?:---[ \t]*(?:File:)?[ \t]*|File:[ \t]*|<{4,}(?:[ \tA-Z]*))[ \t]*([a-z0-9_/\.\-]+\.[a-z]+)[ \t]*(?:---|\*/)?)|(?:^[ \t]*(?://|/\*|#)?[ \t]*([a-z0-9_/\.\-]+\.[a-z]+)[ \t]*(?:---|\*/)?(?:\r?\n[ \t]*)+<{4,})").unwrap()
    });

    let mut chunks = Vec::new();
    let mut last_end = 0;

    // NEW: State variable to remember the filename across multiple code blocks
    let mut current_filepath: Option<String> = None;

    for caps in re_code.captures_iter(text) {
        // Safe extraction of the full match
        let full_match = if let Some(m) = caps.get(0) { m } else { continue; };

        if full_match.start() > last_end {
            let md_text = &text[last_end..full_match.start()];
            chunks.push(ContentChunk::Markdown(md_text));

            // Search the entire markdown chunk for filepaths
            let mut found_in_chunk = None;
            for path_caps in re_filepath.captures_iter(md_text) {
                if let Some(m) = path_caps.get(1) {
                    found_in_chunk = Some(m.as_str().to_string());
                }
            }

            // If we found a filename in this intermediate text, update our active tracker.
            // If we didn't, `current_filepath` retains whatever file it was already tracking!
            if found_in_chunk.is_some() {
                current_filepath = found_in_chunk;
            }
        }

        let lang_match = caps.get(1).map_or("", |m| m.as_str());
        let code_match = caps.get(2).map_or("", |m| m.as_str());

        // --- Extract filename if it was written inside the code block ---
        if let Some(inner_caps) = re_inner_file.captures(code_match) {
            if let Some(m) = inner_caps.get(1).or_else(|| inner_caps.get(2)) {
                current_filepath = Some(m.as_str().to_string());
            }
        }

        chunks.push(ContentChunk::Code {
            lang: lang_match,
            code: code_match,
            filepath: current_filepath.clone(),
        });

        last_end = full_match.end();
    }

    if last_end < text.len() {
        chunks.push(ContentChunk::Markdown(&text[last_end..]));
    }

    chunks
}
