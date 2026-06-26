use super::Editor;

use super::syntax::{TokenType, rust::RustToken};
use super::Syntax;
use logos::Logos;

use crate::bulat::editor::syntax::DynamicRule;

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

impl<T: Editor> egui::util::cache::ComputerMut<(&T, &str), LayoutJob> for Token {
    fn compute(&mut self, (cache, text): (&T, &str)) -> LayoutJob {
        self.highlight(cache, text)
    }
}

pub type HighlightCache = egui::util::cache::FrameCache<LayoutJob, Token>;

pub fn highlight<T: Editor>(ctx: &egui::Context, cache: &T, text: &str) -> LayoutJob {
    ctx.memory_mut(|mem| mem.caches.cache::<HighlightCache>().get((cache, text)).clone())
}
