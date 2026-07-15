use similar::{Algorithm, DiffOp, TextDiff};
use std::sync::OnceLock;
use regex::Regex;

pub struct BulatEngine;

impl BulatEngine {
    /// Computes the diff ops between two strings using Patience algorithm
    pub fn compute_diffs(left: &str, right: &str) -> Vec<DiffOp> {
        let diff = TextDiff::configure()
            .algorithm(Algorithm::Patience)
            .diff_lines(left, right);
        diff.ops().to_vec()
    }

    /// Pure function: Applies a DiffOp from the right side onto the left side,
    /// returning the newly merged left string.
    pub fn apply_merge(left_code: &str, right_code: &str, op: &DiffOp) -> String {
        let left_lines: Vec<&str> = left_code.lines().collect();
        let right_lines: Vec<&str> = right_code.lines().collect();

        let mut new_left: Vec<String> = left_lines.iter().map(|s| s.to_string()).collect();

        match op {
            DiffOp::Equal { .. } => return left_code.to_string(),
            DiffOp::Delete { old_index, old_len, .. } => {
                new_left.drain(*old_index..*old_index + *old_len);
            }
            DiffOp::Insert { old_index, new_index, new_len } => {
                let text_to_insert: Vec<String> = right_lines[*new_index..*new_index + *new_len]
                    .iter()
                    .map(|s| s.to_string())
                    .collect();
                new_left.splice(*old_index..*old_index, text_to_insert);
            }
            DiffOp::Replace { old_index, old_len, new_index, new_len } => {
                let text_to_insert: Vec<String> = right_lines[*new_index..*new_index + *new_len]
                    .iter()
                    .map(|s| s.to_string())
                    .collect();
                new_left.splice(*old_index..*old_index + *old_len, text_to_insert);
            }
        }

        let mut final_string = new_left.join("\n");

        // Standard code editor behavior: ensure the file ends with a trailing newline
        if !final_string.ends_with('\n') && !final_string.is_empty() {
            final_string.push('\n');
        }

        final_string
    }

    /// Extracts the real code from a padded view by removing gap lines
    pub fn extract_real_code(view: &str) -> String {
        let mut real = String::new();
        let lines: Vec<&str> = view.split('\n').collect();

        let mut is_first = true;
        for line in lines {
            // If the line is exactly our invisible gap marker, ignore it
            if line == "\u{200B}" {
                continue;
            }

            if !is_first {
                real.push('\n');
            }
            // Strip the marker in case the user manually typed text INTO a gap line
            real.push_str(&line.replace('\u{200B}', ""));
            is_first = false;
        }
        real
    }
}


pub fn apply_llm_diffs(original: &str, snippet: &str) -> Option<String> {
    static RE_DIFF: OnceLock<Regex> = OnceLock::new();
    let re_diff = RE_DIFF.get_or_init(|| {
        Regex::new(r"(?ms)^[ \t]*<{4,}[ \t]*[^\n]*\r?\n(.*?)\r?\n^[ \t]*={4,}[ \t]*[^\n]*\r?\n(.*?)\r?\n^[ \t]*>{4,}[ \t]*[^\n]*").unwrap()
    });

    let mut current_text = original.to_string();
    let mut diff_block_found = false;

    fn clean_block(mut block: &str) -> &str {
        // Remove leading empty lines
        while let Some(nl) = block.find('\n') {
            if block[..nl].trim().is_empty() {
                block = &block[nl + 1..];
            } else {
                break;
            }
        }

        let first_line_end = block.find('\n').unwrap_or(block.len());
        let first_line = block[..first_line_end].trim();

        // Detect if the first line is a file path header or purely a dashed divider
        if (first_line.starts_with("---") || first_line.starts_with("//") || first_line.starts_with("/*") || first_line.starts_with("#"))
            && first_line.contains("---") {
            let lower = first_line.to_lowercase();
            let is_file_marker = lower.contains("file:") || lower.contains(".rs") || lower.contains(".md") || lower.contains(".toml") || lower.contains(".c") || lower.contains(".h") || first_line.chars().all(|c| c == '-' || c == ' ' || c == '/' || c == '*' || c == '#');

            if is_file_marker {
                let next_start = if first_line_end < block.len() { first_line_end + 1 } else { first_line_end };
                block = &block[next_start..];
            }
        }

        // Remove trailing empty lines
        while block.ends_with('\n') || block.ends_with('\r') {
            block = &block[..block.len() - 1];
        }
        block
    };

    for caps in re_diff.captures_iter(snippet) {
        diff_block_found = true;

        let search_block = clean_block(caps.get(1).map_or("", |m| m.as_str()));
        let replace_block = clean_block(caps.get(2).map_or("", |m| m.as_str()));

        if search_block.trim().is_empty() && replace_block.trim().is_empty() {
            continue;
        }

        // --- 0. Prevent Double Application ---
        let replace_norm = replace_block.replace("\r\n", "\n").trim().to_string();
        let search_norm = search_block.replace("\r\n", "\n").trim().to_string();
        let orig_norm = current_text.replace("\r\n", "\n");

        if !replace_norm.is_empty() && orig_norm.contains(&replace_norm) {
            // If the replacement is already in the text, and it's substantial or contains the search block
            // (which is the classic cause of infinite duplication), we skip it!
            if replace_norm.contains(&search_norm) || replace_norm.lines().count() > 1 || replace_norm.len() > 20 {
                continue;
            }
        }

        // 1. Try exact match
        if !search_block.is_empty() {
            if let Some(idx) = current_text.find(search_block) {
                let mut new_text = String::with_capacity(current_text.len() + replace_block.len());
                new_text.push_str(&current_text[..idx]);
                new_text.push_str(replace_block);
                new_text.push_str(&current_text[idx + search_block.len()..]);
                current_text = new_text;
                continue;
            }
        }

        // 2. Try normalized match (ignoring \r and leading/trailing whitespace of the block)
        let search_norm = search_block.replace("\r\n", "\n").trim().to_string();
        let orig_norm = current_text.replace("\r\n", "\n");
        if !search_norm.is_empty() {
            if let Some(idx) = orig_norm.find(&search_norm) {
                let replace_norm = replace_block.replace("\r\n", "\n");
                let mut new_text = String::with_capacity(orig_norm.len() + replace_norm.len());
                new_text.push_str(&orig_norm[..idx]);
                new_text.push_str(&replace_norm);
                new_text.push_str(&orig_norm[idx + search_norm.len()..]);
                current_text = new_text;
                continue;
            }
        }

        // 3. Try highly tolerant fuzzy match (ignores all internal whitespace differences)
        let search_trimmed = search_block.trim();
        if !search_trimmed.is_empty() {
            let escaped_search = regex::escape(search_trimmed);
            let fuzzy_pattern = escaped_search.split_whitespace().collect::<Vec<_>>().join(r"\s+");
            if let Ok(re) = Regex::new(&fuzzy_pattern) {
                if let Some(mat) = re.find(&current_text) {
                    let mut new_text = String::with_capacity(current_text.len() + replace_block.len());
                    new_text.push_str(&current_text[..mat.start()]);
                    new_text.push_str(replace_block);
                    new_text.push_str(&current_text[mat.end()..]);
                    current_text = new_text;
                    continue;
                }
            }
        }
    }

    // If we detected diff markers, we MUST return Some() so we don't fall back
    // to passing raw <<<< markers to the merge tool. If patching failed, returning
    // the un-patched original simply shows 0 diffs in the merge tool instead of a broken file.
    if diff_block_found {
        Some(current_text)
    } else {
        None
    }
}

// when LLM sends only one function, we want to pre-merge it with the target
// file before sending it to the GUI merge tool
pub fn find_function_spans(code: &str, fn_name: &str) -> Vec<(usize, usize)> {
    // Looks for: fn my_function_name( or fn my_function_name<
    let pattern = format!(r"(?m)^[ \t]*(?:pub\s+)?(?:async\s+)?fn\s+{}(?:\s|<|\()", regex::escape(fn_name));
    let re = match Regex::new(&pattern) {
        Ok(r) => r,
        Err(_) => return vec![],
    };

    let mut spans = Vec::new();
    for mat in re.find_iter(code) {
        let start_idx = mat.start();
        let mut brace_count = 0;
        let mut found_first_brace = false;

        let mut chars = code[start_idx..].char_indices().peekable();
        let mut in_string = false;
        let mut in_char = false;
        let mut in_comment = false;
        let mut in_multi_comment = false;

        // A lightweight lexer to safely count braces without tripping on strings/comments
        while let Some((i, c)) = chars.next() {
            if in_comment {
                if c == '\n' { in_comment = false; }
                continue;
            }
            if in_multi_comment {
                if c == '*' {
                    if let Some(&(_, '/')) = chars.peek() {
                        chars.next();
                        in_multi_comment = false;
                    }
                }
                continue;
            }
            if in_string {
                if c == '\\' { chars.next(); } // skip escaped char
                else if c == '"' { in_string = false; }
                continue;
            }
            if in_char {
                if c == '\\' { chars.next(); }
                else if c == '\'' { in_char = false; }
                continue;
            }

            match c {
                '"' => in_string = true,
                '\'' => in_char = true,
                '/' => {
                    if let Some(&(_, '/')) = chars.peek() {
                        in_comment = true;
                        chars.next();
                    } else if let Some(&(_, '*')) = chars.peek() {
                        in_multi_comment = true;
                        chars.next();
                    }
                },
                '{' => {
                    brace_count += 1;
                    found_first_brace = true;
                },
                '}' => {
                    brace_count -= 1;
                    if found_first_brace && brace_count == 0 {
                        spans.push((start_idx, start_idx + i + 1));
                        break;
                    }
                },
                _ => {}
            }
        }
    }
    spans
}

/// Helper to extract raw SEARCH and REPLACE blocks without attempting to apply them to the file.
pub fn extract_raw_diff_blocks(snippet: &str) -> Option<(String, String)> {
    static RE_DIFF: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re_diff = RE_DIFF.get_or_init(|| {
        regex::Regex::new(r"(?ms)^[ \t]*<{4,}[ \t]*[^\n]*\r?\n(.*?)\r?\n^[ \t]*={4,}[ \t]*[^\n]*\r?\n(.*?)\r?\n^[ \t]*>{4,}[ \t]*[^\n]*").unwrap()
    });

    fn clean_block(mut block: &str) -> &str {
        while let Some(nl) = block.find('\n') {
            if block[..nl].trim().is_empty() { block = &block[nl + 1..]; } else { break; }
        }
        let first_line_end = block.find('\n').unwrap_or(block.len());
        let first_line = block[..first_line_end].trim();
        if (first_line.starts_with("---") || first_line.starts_with("//") || first_line.starts_with("/*") || first_line.starts_with("#")) && first_line.contains("---") {
            let lower = first_line.to_lowercase();
            let is_file_marker = lower.contains("file:") || lower.contains(".rs") || lower.contains(".md") || lower.contains(".toml") || lower.contains(".c") || lower.contains(".h") || first_line.chars().all(|c| c == '-' || c == ' ' || c == '/' || c == '*' || c == '#');
            if is_file_marker {
                let next_start = if first_line_end < block.len() { first_line_end + 1 } else { first_line_end };
                block = &block[next_start..];
            }
        }
        while block.ends_with('\n') || block.ends_with('\r') { block = &block[..block.len() - 1]; }
        block
    }

    let mut matches = re_diff.captures_iter(snippet);
    if let Some(caps) = matches.next() {
        // --- NEW: Abort if there are multiple diff blocks in the same snippet!
        // This prevents the embedded merge tool from rendering incorrectly.
        if matches.next().is_some() {
            return None;
        }

        let search_block = clean_block(caps.get(1).map_or("", |m| m.as_str()));
        let replace_block = clean_block(caps.get(2).map_or("", |m| m.as_str()));
        return Some((search_block.to_string(), replace_block.to_string()));
    }
    None
}

pub fn try_splice_snippet(original: &str, snippet: &str) -> Option<String> {
    static RE_FN: OnceLock<Regex> = OnceLock::new();
    let re_fn = RE_FN.get_or_init(|| Regex::new(r"(?m)^[ \t]*(?:pub\s+)?(?:async\s+)?fn\s+([a-zA-Z0-9_]+)(?:\s|<|\()").unwrap());

    let mut fn_names = Vec::new();
    for caps in re_fn.captures_iter(snippet) {
        if let Some(name) = caps.get(1) {
            fn_names.push(name.as_str());
        }
    }

    // Safety check: Only attempt splice if there is EXACTLY one function in the snippet
    if fn_names.len() == 1 {
        let fn_name = fn_names[0];

        let orig_spans = find_function_spans(original, fn_name);
        let snip_spans = find_function_spans(snippet, fn_name);

        println!("orig_spans: {:?}", orig_spans);
        println!("snip_spans: {:?}", snip_spans);

        // Safety check: Only splice if the function name is completely unique in BOTH strings
        // (This prevents accidentally overwriting the wrong `fn new()` in a file with multiple structs)
        if orig_spans.len() == 1 && snip_spans.len() == 1 {
            let (orig_start, orig_end) = orig_spans[0];
            let (snip_start, snip_end) = snip_spans[0];

            let spliced_function = &snippet[snip_start..snip_end];

            let mut new_code = String::with_capacity(original.len() + spliced_function.len());
            new_code.push_str(&original[..orig_start]);
            new_code.push_str(spliced_function);
            new_code.push_str(&original[orig_end..]);

            return Some(new_code);
        }
    }
    None
}
