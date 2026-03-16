use super::Editor;

use super::syntax::{TokenType, rust::RustToken};
use super::Syntax;
use logos::Logos;

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

    pub fn tokens(&mut self, _syntax: &Syntax, text: &str) -> Vec<Self> {
        let lexer = RustToken::lexer(text);

        // 1. Collect all raw tokens first so we can look ahead
        let raw_tokens: Vec<(RustToken, &str)> = lexer
            .spanned()
            .map(|(token, span)| (token.unwrap_or(RustToken::Error), &text[span]))
            .collect();

        let mut tokens = Vec::new();

        for (i, (token, buffer)) in raw_tokens.iter().enumerate() {
            let ty = match token {
                RustToken::Keyword => TokenType::Keyword,
                RustToken::Type => TokenType::Type,
                RustToken::Special | RustToken::Lifetime => TokenType::Special,
                RustToken::String => TokenType::Str('"'),
                RustToken::Char => TokenType::Str('\''),
                RustToken::Comment => TokenType::Comment(false),
                RustToken::CommentMulti => TokenType::Comment(true),
                RustToken::Number => TokenType::Numeric(buffer.contains('.')),

                RustToken::Punctuation => {
                    let c = buffer.chars().next().unwrap_or('?');
                    TokenType::Punctuation(c)
                }

                RustToken::Identifier => {
                    // Check if the NEXT token is a Punctuation "("
                    // raw_tokens[i+1] is a reference to (RustToken, &str)
                    let next_token = raw_tokens.get(i + 1);

                    // We match against the Tuple reference
                    if let Some((RustToken::Punctuation, "(")) = next_token {
                        TokenType::Function
                    } else {
                        TokenType::Literal
                    }
                }

                RustToken::Whitespace => {
                    let c = buffer.chars().next().unwrap_or(' ');
                    TokenType::Whitespace(c)
                }

                RustToken::Error => TokenType::Unknown,
            };

            tokens.push(Token {
                ty,
                buffer: buffer.to_string(),
            });
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
    ctx.memory_mut(|mem| mem.caches.cache::<HighlightCache>().get((cache, text)))
}