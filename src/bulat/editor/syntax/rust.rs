use logos::Logos;
use super::Syntax;
use std::collections::BTreeSet;

#[derive(Logos, Debug, PartialEq, Clone, Copy)]
pub enum RustToken {
    // 1. Comments
    #[regex(r"//[^\n]*", allow_greedy = true)]
    Comment,

    #[regex(r"/\*[^*]*\*+(?:[^/*][^*]*\*+)*/")]
    CommentMulti,

    // 2. Chars (Single Quoted Strings)
    // We restrict this to EXACTLY one character, or one valid escape sequence
    // Notice the r#" ... "# which allows us to safely include the " character inside
    #[regex(r#"'(?:[^'\\]|\\[nrt0\\'"]|\\x[0-9a-fA-F]{2}|\\u\{[0-9a-fA-F]{1,6}\})'"#)]
    Char,

    // 3. Strings (Double Quoted)
    #[regex(r#""(?:[^"\\]|\\.)*""#)]
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
        }
    }
}