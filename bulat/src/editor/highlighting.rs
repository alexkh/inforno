use super::Editor;

use super::syntax::{TokenType, rust::RustToken};
use super::Syntax;
use logos::Logos;

use crate::editor::syntax::DynamicRule;

#[derive(Default, Debug, PartialEq, PartialOrd, Eq, Ord)]
pub struct Token {
    ty: TokenType,
    buffer: String,
}

impl Token {
    pub fn new<S: Into<String>>(ty: TokenType, buffer: S) -> Self {
        Token {
            ty,
            buffer: buffer.into(),
        }
    }
    pub fn ty(&self) -> TokenType {
        self.ty
    }
    pub fn buffer(&self) -> &str {
        &self.buffer
    }

    pub fn highlight<T: Editor>(&mut self, editor: &T, text: &str) -> LayoutJob {
        let tokens = self.tokens(editor.syntax(), text);
        let mut job = LayoutJob::default();
        for token in tokens {
            editor.append(&mut job, &token);
        }
        job
    }

        pub fn tokens(&mut self, syntax: &Syntax, text: &str) -> Vec<Self> {
        // 1. If dynamic rules exist, use the Rhai Runtime Engine
        if let Some(rules) = &syntax.dynamic_rules {
            return self.tokens_dynamic(rules, text);
        }

        // 2. If a Native Parser is attached, execute it dynamically
        if let Some(parser) = syntax.native_parser {
            return parser(text);
        }

        // 3. True fallback (Plain Text)
        vec![Self {
            ty: TokenType::Literal,
            buffer: text.to_string(),
        }]
    }

    fn tokens_dynamic(&self, dynamic_rules: &[DynamicRule], mut text: &str) -> Vec<Self> {
        let mut tokens = Vec::new();

        while !text.is_empty() {
            let mut matched = false;

            for rule in dynamic_rules {
                if let Some(mat) = rule.regex.find(text) {
                    let matched_str = mat.as_str();
                    let remainder = &text[matched_str.len()..];

                    // --- NEW: Process the optional lookahead ---
                    if let Some(req_char) = &rule.followed_by {
                        // If the remainder doesn't start with the required string
                        // (ignoring leading whitespace), this is NOT a match.
                        if !remainder.trim_start().starts_with(req_char) {
                            continue; // Skip to the next regex rule!
                        }
                    }

                    tokens.push(Token {
                        ty: rule.token_type,
                        buffer: matched_str.to_string(),
                    });

                    text = remainder;
                    matched = true;
                    break;
                }
            }

            if !matched {
                let mut chars = text.chars();
                let c = chars.next().unwrap();

                let ty = if c.is_whitespace() {
                    TokenType::Whitespace(c)
                } else {
                    TokenType::Unknown
                };

                tokens.push(Token {
                    ty,
                    buffer: c.to_string(),
                });
                text = chars.as_str();
            }
        }
        tokens
    }
}

use egui::text::LayoutJob;

impl<T: Editor> egui::cache::ComputerMut<(&T, &str), LayoutJob> for Token {
    fn compute(&mut self, (cache, text): (&T, &str)) -> LayoutJob {
        self.highlight(cache, text)
    }
}

pub type HighlightCache = egui::cache::FrameCache<LayoutJob, Token>;

pub fn highlight<T: Editor>(ctx: &egui::Context, cache: &T, text: &str) -> LayoutJob {
    let mut job = ctx.memory_mut(|mem| mem.caches.cache::<HighlightCache>().get((cache, text)).clone());
    let search_term = cache.search_term();
    let active_match = cache.active_search_match_byte_range();

    if !search_term.is_empty() {
        let term_lower = search_term.to_lowercase();
        let text_lower = text.to_lowercase();
        let mut match_ranges = Vec::new();
        let mut start = 0;

        while let Some(idx) = text_lower[start..].find(&term_lower) {
            let match_start = start + idx;
            let match_end = match_start + term_lower.len();
            match_ranges.push(match_start..match_end);
            start = match_end;
        }

        if !match_ranges.is_empty() {
            let mut new_sections = Vec::new();
            let highlight_bg = egui::Color32::from_rgba_premultiplied(200, 200, 0, 150); // Yellow for background matches
            let active_bg = egui::Color32::from_rgba_premultiplied(255, 128, 0, 200);   // Orange for the active match!

            for section in job.sections {
                let sec_start = section.byte_range.start;
                let sec_end = section.byte_range.end;
                let mut current_start = sec_start;

                for range in &match_ranges {
                    if range.end <= usize::from(current_start) { continue; }
                    if range.start >= usize::from(sec_end) { break; }

                    if usize::from(current_start) < range.start {
                        let format = section.format.clone();
                        new_sections.push(egui::text::LayoutSection {
                            leading_space: if current_start == sec_start { section.leading_space } else { 0.0 },
                            byte_range: current_start..egui::text::ByteIndex(range.start),
                            format,
                        });
                        current_start = egui::text::ByteIndex(range.start);
                    }

                    let overlap_end = range.end.min(usize::from(sec_end));
                    let mut format = section.format.clone();

                    let is_active = active_match.as_ref().map_or(false, |active| active.start == range.start && active.end == range.end);
                    format.background = if is_active { active_bg } else { highlight_bg };
                    format.color = egui::Color32::BLACK; // Override text color to ensure readability

                    new_sections.push(egui::text::LayoutSection {
                        leading_space: if current_start == sec_start { section.leading_space } else { 0.0 },
                        byte_range: current_start..egui::text::ByteIndex(overlap_end),
                        format,
                    });
                    current_start = egui::text::ByteIndex(overlap_end);
                }

                if current_start < sec_end {
                    new_sections.push(egui::text::LayoutSection {
                        leading_space: if current_start == sec_start { section.leading_space } else { 0.0 },
                        byte_range: current_start..sec_end,
                        format: section.format.clone(),
                    });
                }
            }
            job.sections = new_sections;
        }
    }

    job
}
