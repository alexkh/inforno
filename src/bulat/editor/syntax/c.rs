use logos::Logos;
use crate::bulat::editor::Token;
use crate::bulat::editor::syntax::{Syntax, TokenType};
use std::collections::BTreeSet;

#[derive(Logos, Debug, PartialEq, Clone, Copy)]
pub enum CToken {
    #[regex(r"//[^\n]*", allow_greedy = true)]
    Comment,

    #[regex(r"/\*[^*]*\*+(?:[^/*][^*]*\*+)*/")]
    CommentMulti,

    #[regex(r"#[ \t]*[a-zA-Z_]+")]
    Preprocessor,

    #[regex(r#"'(?:[^'\\]|\\.)'"#)]
    Char,
    #[regex(r#""(?:[^"\\]|\\.)*""#)]
    String,

    #[token("auto")] #[token("break")] #[token("case")] #[token("class")] #[token("const")]
    #[token("continue")] #[token("default")] #[token("do")] #[token("else")] #[token("enum")]
    #[token("extern")] #[token("for")] #[token("goto")] #[token("if")] #[token("inline")]
    #[token("namespace")] #[token("register")] #[token("return")] #[token("sizeof")]
    #[token("static")] #[token("struct")] #[token("switch")] #[token("template")]
    #[token("typedef")] #[token("union")] #[token("virtual")] #[token("volatile")] #[token("while")]
    Keyword,

    #[token("bool")] #[token("char")] #[token("double")] #[token("float")] #[token("int")]
    #[token("long")] #[token("short")] #[token("signed")] #[token("unsigned")] #[token("void")]
    #[token("size_t")] #[token("ssize_t")] #[token("int8_t")] #[token("uint8_t")]
    #[token("int16_t")] #[token("uint16_t")] #[token("int32_t")] #[token("uint32_t")]
    #[token("int64_t")] #[token("uint64_t")]
    BuiltinType,

    #[regex(r"[a-zA-Z_][a-zA-Z0-9_]*")]
    Identifier,

    #[regex(r"\d+(?:\.\d+)?(?:[fFlLuU]+)?")]
    #[regex(r"0[xX][0-9a-fA-F]+(?:[uUlL]+)?")]
    Number,

    #[regex(r"[{}()\[\];,.*+\-/%&|^!=<>~?]")]
    Punctuation,

    #[regex(r"[ \t\n\f]+")]
    Whitespace,

    Error,
}

pub fn parse(text: &str) -> Vec<Token> {
    let lexer = CToken::lexer(text);

    let raw_tokens: Vec<(CToken, &str)> = lexer
        .spanned()
        .map(|(token, span)| (token.unwrap_or(CToken::Error), &text[span]))
        .collect();

    let mut tokens = Vec::with_capacity(raw_tokens.len());

    for (i, (token, buffer)) in raw_tokens.iter().enumerate() {
        let ty = match token {
            CToken::Keyword => TokenType::Keyword,
            CToken::BuiltinType => TokenType::Type,
            CToken::Preprocessor => TokenType::Special,
            CToken::String => TokenType::Str('"'),
            CToken::Char => TokenType::Str('\''),
            CToken::Comment => TokenType::Comment(false),
            CToken::CommentMulti => TokenType::Comment(true),
            CToken::Number => TokenType::Numeric(buffer.contains('.')),
            CToken::Punctuation => {
                let c = buffer.chars().next().unwrap_or('?');
                TokenType::Punctuation(c)
            }
            CToken::Whitespace => {
                let c = buffer.chars().next().unwrap_or(' ');
                TokenType::Whitespace(c)
            }
            CToken::Identifier => {
                let mut j = i + 1;
                let mut is_func = false;
                let mut generic_depth = 0;

                while let Some((next_tok, next_str)) = raw_tokens.get(j) {
                    match next_tok {
                        CToken::Whitespace => {}
                        CToken::Punctuation => {
                            if *next_str == "<" {
                                generic_depth += 1;
                            } else if *next_str == ">" {
                                if generic_depth > 0 { generic_depth -= 1; }
                                else { break; }
                            } else if *next_str == "(" {
                                if generic_depth == 0 { is_func = true; }
                                break;
                            } else if *next_str == ";" || *next_str == "{" || *next_str == "}" {
                                break;
                            } else if generic_depth == 0 {
                                break;
                            }
                        }
                        _ => { if generic_depth == 0 { break; } }
                    }
                    j += 1;
                }

                if is_func {
                    TokenType::Function
                } else {
                    TokenType::Literal
                }
            }
            CToken::Error => TokenType::Unknown,
        };

        // THE FIX: Use the public constructor method!
        tokens.push(Token::new(ty, *buffer));
    }

    tokens
}

// ---------------------------------------------------------
// The Native Builder
// ---------------------------------------------------------
impl Syntax {
    pub fn c() -> Self {
        Syntax {
            language: "C",
            case_sensitive: true,
            comment: "//",
            comment_multiline: ["/*", "*/"],
            hyperlinks: BTreeSet::new(),
            keywords: BTreeSet::new(),
            types: BTreeSet::new(),
            special: BTreeSet::new(),
            dynamic_rules: None,
            // ATTACH THE NATIVE PARSER
            native_parser: Some(parse),
        }
    }
}