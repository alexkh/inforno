#![allow(dead_code)]
pub mod rust;

use std::collections::BTreeSet;

// Added Hash to TokenType
#[derive(Default, Clone, Copy, PartialEq, PartialOrd, Eq, Ord, Debug, Hash)]
pub enum TokenType {
    Comment(bool),
    Function,
    Keyword,
    Literal,
    Hyperlink,
    Numeric(bool),
    Punctuation(char),
    Special,
    Str(char),
    Type,
    Whitespace(char),
    #[default]
    Unknown,
}

// Added Hash and Eq to Syntax
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Syntax {
    pub language: &'static str,
    pub case_sensitive: bool,
    pub comment: &'static str,
    pub comment_multiline: [&'static str; 2],
    pub hyperlinks: BTreeSet<&'static str>,
    pub keywords: BTreeSet<&'static str>,
    pub types: BTreeSet<&'static str>,
    pub special: BTreeSet<&'static str>,
}

impl Default for Syntax {
    fn default() -> Self {
        Syntax::rust()
    }
}

impl Syntax {
    /// plain text syntax (No highlighting, fast parsing)
    pub fn text() -> Self {
        Syntax {
            language: "Text",
            case_sensitive: false,
            comment: "",
            comment_multiline: ["", ""],
            hyperlinks: BTreeSet::new(),
            keywords: BTreeSet::new(),
            types: BTreeSet::new(),
            special: BTreeSet::new(),
        }
    }

    // This is the function the demo was missing
    pub fn simple(comment: &'static str) -> Self {
        Syntax {
            language: "Simple",
            case_sensitive: false,
            comment,
            comment_multiline: [comment; 2], // Placeholder
            hyperlinks: BTreeSet::new(),
            keywords: BTreeSet::new(),
            types: BTreeSet::new(),
            special: BTreeSet::new(),
        }
    }

    pub fn language(&self) -> &str { self.language }
    pub fn is_keyword(&self, word: &str) -> bool { self.keywords.contains(word) }
    pub fn is_type(&self, word: &str) -> bool { self.types.contains(word) }
    pub fn is_special(&self, word: &str) -> bool { self.special.contains(word) }
}