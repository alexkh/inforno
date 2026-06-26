use logos::{Logos, Lexer};
use super::Syntax;
use std::collections::BTreeSet;
use crate::bulat::editor::Token;
use crate::bulat::editor::syntax::TokenType;

// Custom callback for Rust Raw Strings
fn lex_raw_string(lex: &mut Lexer<RustToken>) -> bool {
    // 1. Calculate how many '#' are in the opening `r#" `
    // `r"` is 2 characters long. Anything extra is a hash.
    let hashes = lex.slice().len() - 2;

    // 2. Build the exact closing delimiter we are looking for (e.g., `"#` or `"##`)
    let mut closing = String::from("\"");
    closing.push_str(&"#".repeat(hashes));

    // 3. Search the rest of the file for that exact string!
    if let Some(end) = lex.remainder().find(&closing) {
        // If found, advance the lexer past the closing delimiter
        lex.bump(end + closing.len());
        true
    } else {
        false // Unclosed raw string (EOF)
    }
}

#[derive(Logos, Debug, PartialEq, Clone, Copy)]
pub enum RustToken {
    // 1. Comments
    #[regex(r"//[^\n]*", allow_greedy = true)]
    Comment,

    #[regex(r"/\*[^*]*\*+(?:[^/*][^*]*\*+)*/")]
    CommentMulti,

    // 2. Chars (Single Quoted Strings)
    #[regex(r#"'(?:[^'\\]|\\[nrt0\\'"]|\\x[0-9a-fA-F]{2}|\\u\{[0-9a-fA-F]{1,6}\})'"#)]
    Char,

    // 3. Strings
    // Standard double quoted strings
    #[regex(r#""(?:[^"\\]|\\.)*""#)]
    // Raw strings delegated to our custom Rust callback!
    #[regex(r#"r#*""#, lex_raw_string)]
    String,

    // 4. Lifetimes
    #[regex(r"'[a-zA-Z_][a-zA-Z0-9_]*")]
    Lifetime,

    // 5. Keywords
    #[token("as")] #[token("break")] #[token("const")] #[token("continue")] #[token("crate")]
    #[token("else")] #[token("enum")] #[token("extern")] #[token("fn")] #[token("for")]
    #[token("if")] #[token("impl")] #[token("in")] #[token("let")] #[token("loop")]
    #[token("match")] #[token("mod")] #[token("move")] #[token("mut")] #[token("pub")]
    #[token("ref")] #[token("return")] #[token("self")] #[token("struct")] #[token("super")]
    #[token("trait")] #[token("type")] #[token("use")] #[token("where")] #[token("while")]
    #[token("async")] #[token("await")] #[token("abstract")] #[token("become")] #[token("box")]
    #[token("do")] #[token("final")] #[token("macro")] #[token("override")] #[token("priv")]
    #[token("typeof")] #[token("unsized")] #[token("virtual")] #[token("yield")] #[token("try")]
    #[token("unsafe")] #[token("dyn")]
    Keyword,

    // 6. Special
    #[token("Self")] #[token("static")] #[token("true")] #[token("false")]
    Special,

    // 7. Types
    #[token("Option")] #[token("Result")] #[token("Error")] #[token("Box")] #[token("Cow")]
    #[token("bool")] #[token("i8")] #[token("u8")] #[token("i16")] #[token("u16")]
    #[token("i32")] #[token("u32")] #[token("i64")] #[token("u64")] #[token("i128")] #[token("u128")]
    #[token("isize")] #[token("usize")] #[token("f32")] #[token("f64")] #[token("char")]
    #[token("str")] #[token("String")] #[token("Vec")] #[token("BTreeMap")] #[token("BTreeSet")]
    #[token("Rc")] #[token("Arc")] #[token("Cell")] #[token("RefCell")] #[token("Mutex")]
    Type,

    // 8. Identifiers
    #[regex(r"[a-zA-Z_][a-zA-Z0-9_]*")]
    Identifier,

    // 9. Numbers
    #[regex(r"\d+(?:\.\d+)?")]
    #[regex(r"0x[0-9a-fA-F]+")]
    #[regex(r"0b[01]+")]
    Number,

    // 10. Punctuation
    #[token("(")] #[token(")")] #[token("{")] #[token("}")]
    #[token("[")] #[token("]")] #[token(",")] #[token(".")]
    #[token(";")] #[token(":")] #[token("::")] #[token("->")]
    #[token("=>")] #[token("#")] #[token("!")] #[token("=")]
    #[token("<")] #[token(">")] #[token("+")] #[token("-")]
    #[token("*")] #[token("/")] #[token("%")] #[token("&")]
    #[token("|")] #[token("^")] #[token("?")]
    Punctuation,

    // 11. Whitespace
    #[regex(r"[ \t\n\f]+")]
    Whitespace,

    // We add this manually so we can use it as a fallback in highlighting.rs
    // Logos won't use it, but our code will.
    Error,
}

pub fn parse(text: &str) -> Vec<Token> {
    let lexer = RustToken::lexer(text);
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
                let mut j = i + 1;
                let mut is_func = false;
                let mut generic_depth = 0;

                // Peek ahead safely
                while let Some((next_tok, next_str)) = raw_tokens.get(j) {
                    match next_tok {
                        RustToken::Whitespace => {
                            // Safely skip whitespace
                        }
                        RustToken::Punctuation => {
                            if *next_str == "<" {
                                generic_depth += 1;
                            } else if *next_str == ">" {
                                if generic_depth > 0 {
                                    generic_depth -= 1;
                                } else {
                                    break; // Unbalanced > or greater-than operator
                                }
                            } else if *next_str == "(" {
                                // If we hit an open paren and we aren't inside generics, it's a function!
                                if generic_depth == 0 {
                                    is_func = true;
                                }
                                break;
                            } else if *next_str == ";" || *next_str == "{" || *next_str == "}" {
                                // Safety break: Prevent runaway scans if `<` was just a less-than math operator
                                break;
                            } else if generic_depth == 0 {
                                // Any other punctuation (like ',', '.', '::') outside generics breaks the match
                                break;
                            }
                        }
                        _ => {
                            // For other tokens (Keywords, Literals, Numbers)
                            // If we are not inside a < > generic block, abort.
                            if generic_depth == 0 {
                                break;
                            }
                        }
                    }
                    j += 1;
                }

                if is_func {
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

        tokens.push(Token::new(ty, *buffer));
    }

    tokens
}

impl Syntax {
    pub fn rust() -> Self {
        Syntax {
            language: "Rust",
            case_sensitive: true,
            comment: "//",
            comment_multiline: ["/*", "*/"],
            hyperlinks: BTreeSet::from(["http"]),
            keywords: BTreeSet::from([
                "fn", "let", "struct", "enum", "impl", "trait", "mod", "use",
                "pub", "crate", "super", "self", "Self", "match", "if", "else",
                "for", "while", "loop", "break", "continue", "return", "as",
                "mut", "const", "static", "type", "unsafe", "async", "await"
            ]),
            types: BTreeSet::from([
                "String", "str", "Vec", "Option", "Result", "Box", "Rc", "Arc",
                "u8", "u16", "u32", "u64", "i8", "i16", "i32", "i64", "f32", "f64",
                "bool", "char", "usize", "isize"
            ]),
            special: BTreeSet::new(),
            dynamic_rules: None,
            native_parser: Some(parse),
        }
    }
}