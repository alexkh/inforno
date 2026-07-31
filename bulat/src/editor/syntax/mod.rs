#![allow(dead_code)]
pub mod c;
pub mod rust;
pub mod loader;

use std::collections::BTreeSet;
use regex::Regex;
use std::hash::{Hash, Hasher};

// native parser is a compiled-in .rs file, as opposed to dynamic parser which
// is a .rhai file
pub type NativeParser = fn(&str) -> Vec<crate::editor::Token>;

#[derive(Clone, Debug)]
pub struct DynamicRule {
    pub token_type: TokenType,
    pub pattern: String,
    pub regex: Regex,
    pub followed_by: Option<String>,
}

impl PartialEq for DynamicRule {
    fn eq(&self, other: &Self) -> bool {
        self.token_type == other.token_type &&
        self.pattern == other.pattern &&
        self.followed_by == other.followed_by
    }
}
impl Eq for DynamicRule {}

impl Hash for DynamicRule {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.token_type.hash(state);
        self.pattern.hash(state);
    }
}

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
    pub dynamic_rules: Option<Vec<DynamicRule>>,
    pub native_parser: Option<NativeParser>,
}

impl Default for Syntax {
    fn default() -> Self {
        Syntax::rust()
    }
}

impl Syntax {
    /// Converts a markdown code block language tag (e.g., "rust ", "cpp\r") into a standard MIME type.
    /// This is highly robust against trailing whitespaces and capitalization.
    pub fn guess_mime_from_markdown_lang(lang: &str) -> &'static str {
        let clean_lang = lang.trim().to_lowercase();

        match clean_lang.as_str() {
            "rust" | "rs" => "text/rust",
            "rhai" => "application/x-rhai",
            "c" | "h" => "text/x-c",
            "cpp" | "cxx" | "c++" | "hpp" => "text/x-c++",
            "python" | "py" => "text/x-python",
            "javascript" | "js" => "text/javascript",
            "typescript" | "ts" => "text/typescript",
            "html" | "htm" => "text/html",
            "markdown" | "md" => "text/markdown",
            "json" => "application/json",
            "toml" => "application/toml",
            "yaml" | "yml" => "application/yaml",
            "sh" | "bash" | "shell" => "application/x-sh",
            _ => {
                // Fallback to the standard extension guesser just in case it recognizes it
                Self::guess_mime_from_ext(&clean_lang)
            }
        }
    }
    /// Standardizes an extension or markdown language tag into a MIME type
    pub fn guess_mime_from_ext(ext: &str) -> &'static str {
        match ext.to_lowercase().as_str() {
            "rs" | "rust" => "text/rust",
            "c" | "h" => "text/x-c",
            "cpp" | "hpp" | "cc" | "cxx" | "c++" => "text/x-c++",
            "rhai" => "application/x-rhai",
            "md" | "markdown" => "text/markdown",
            "json" => "application/json",
            "toml" => "application/toml",
            "yaml" | "yml" => "application/yaml",
            "js" | "javascript" => "text/javascript",
            "ts" | "typescript" => "text/typescript",
            "py" | "python" => "text/x-python",
            "html" | "htm" => "text/html",
            "css" => "text/css",
            "sh" | "bash" => "application/x-sh",
            _ => "text/plain",
        }
    }

    /// Extracts the MIME type from a given file path
    pub fn guess_mime_from_path(path: &std::path::Path) -> &'static str {
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        Self::guess_mime_from_ext(ext)
    }

    /// Returns the corresponding built-in Syntax for a standard MIME type
    pub fn from_mime(mime: &str) -> Self {
        match mime {
            "text/rust" => Self::rust(),
            "text/x-c" | "text/x-c++" => Self::c(),
            "application/x-rhai" => Self::rhai(),
            "text/markdown" => Self::markdown(),
            "application/yaml" | "application/toml" => Self::yaml(),
            // Fallback to plain text for unconfigured types
            _ => Self::text(),
        }
    }

    pub fn markdown() -> Self {
        let mut s = Syntax::text();
        s.language = "Markdown";
        let rules = vec![
            // Headers
            DynamicRule {
                token_type: TokenType::Keyword,
                pattern: r"#{1,6}[ \t]+[^\n]*".to_string(),
                regex: Regex::new(r"^#{1,6}[ \t]+[^\n]*").unwrap(),
                followed_by: None,
            },
            // Multi-line code blocks
            DynamicRule {
                token_type: TokenType::Str('`'),
                pattern: r"(?s)```.*?```".to_string(),
                regex: Regex::new(r"^(?s)```.*?```").unwrap(),
                followed_by: None,
            },
            // Inline code snippets
            DynamicRule {
                token_type: TokenType::Str('`'),
                pattern: r"`[^`\n]+`".to_string(),
                regex: Regex::new(r"^`[^`\n]+`").unwrap(),
                followed_by: None,
            },
            // Hyperlinks
            DynamicRule {
                token_type: TokenType::Hyperlink,
                pattern: r"\[[^\]]*\]\([^)]*\)".to_string(),
                regex: Regex::new(r"^\[[^\]]*\]\([^)]*\)").unwrap(),
                followed_by: None,
            },
        ];
        s.dynamic_rules = Some(rules);
        s
    }

    pub fn yaml() -> Self {
        let mut s = Syntax::text();
        s.language = "YAML";
        s.comment = "#";
        let rules = vec![
            // Comments
            DynamicRule {
                token_type: TokenType::Comment(false),
                pattern: r"#[^\n]*".to_string(),
                regex: Regex::new(r"^#[^\n]*").unwrap(),
                followed_by: None,
            },
            // Keys (matched only if immediately followed by a colon)
            DynamicRule {
                token_type: TokenType::Type,
                pattern: r"[a-zA-Z0-9_-]+".to_string(),
                regex: Regex::new(r"^[a-zA-Z0-9_-]+").unwrap(),
                followed_by: Some(":".to_string()),
            },
            // Keywords
            DynamicRule {
                token_type: TokenType::Keyword,
                pattern: r"(true|false|null)\b".to_string(),
                regex: Regex::new(r"^(true|false|null)\b").unwrap(),
                followed_by: None,
            },
            // Numerics
            DynamicRule {
                token_type: TokenType::Numeric(false),
                pattern: r"[0-9]+(?:\.[0-9]+)?".to_string(),
                regex: Regex::new(r"^[0-9]+(?:\.[0-9]+)?").unwrap(),
                followed_by: None,
            },
            // Double-quoted strings
            DynamicRule {
                token_type: TokenType::Str('"'),
                pattern: r#""[^"\\]*(?:\\.[^"\\]*)*""#.to_string(),
                regex: Regex::new(r#"^"[^"\\]*(?:\\.[^"\\]*)*""#).unwrap(),
                followed_by: None,
            },
            // Single-quoted strings
            DynamicRule {
                token_type: TokenType::Str('\''),
                pattern: r#"'[^'\\]*(?:\\.[^'\\]*)*'"#.to_string(),
                regex: Regex::new(r#"^'[^'\\]*(?:\\.[^'\\]*)*'"#).unwrap(),
                followed_by: None,
            },
        ];
        s.dynamic_rules = Some(rules);
        s
    }

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
            dynamic_rules: None,
            native_parser: None,
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
            dynamic_rules: None,
            native_parser: None,
        }
    }

    /// Native Rhai Syntax Highlighting
    pub fn rhai() -> Self {
        let mut s = Syntax::rust();
        s.language = "Rhai";
        s.keywords = BTreeSet::from([
            "fn", "let", "if", "else", "while", "loop", "break", "continue",
            "return", "switch", "const", "mut", "for", "in", "import", "export",
            "as", "throw", "try", "catch"
        ]);
        s.types = BTreeSet::from([
            "String", "Array", "Map", "bool", "char", "int", "float", "Dynamic"
        ]);
        s
    }

    pub fn language(&self) -> &str { self.language }
    pub fn is_keyword(&self, word: &str) -> bool { self.keywords.contains(word) }
    pub fn is_type(&self, word: &str) -> bool { self.types.contains(word) }
    pub fn is_special(&self, word: &str) -> bool { self.special.contains(word) }
}

#[derive(Clone, Default)]
pub struct SyntaxCache {
    pub plugins: std::collections::HashMap<String, Syntax>,
}

impl Syntax {
    /// Lazily loads a syntax plugin and caches it securely inside egui's context memory.
    pub fn get_or_load(ctx: &egui::Context, mime_type: &str) -> Self {
        let cache_id = egui::Id::new("editor_syntax_cache");

        // 1. Check egui's internal memory cache for a hit using MIME type
        let cached_syntax = ctx.data_mut(|d| {
            let cache = d.get_temp_mut_or_default::<SyntaxCache>(cache_id);
            cache.plugins.get(mime_type).cloned()
        });

        if let Some(syntax) = cached_syntax {
            return syntax; // Cache Hit: Instant return
        }

        // 2. Use the robust built-in MIME-based constructor
        let builtin = Syntax::from_mime(mime_type);

        // 3. Check for an overriding user Rhai script
        // We sanitize the mime_type into a valid filename (e.g., text/x-c -> text_x-c.rhai)
        let safe_filename = mime_type.replace('/', "_") + ".rhai";
        let plugin_path = std::path::PathBuf::from(
            std::env::var("HOME").unwrap_or_default()
        ).join(".config/bulat/scripts/syntax/v1/").join(&safe_filename);

        if plugin_path.exists() {
            if let Ok(syn) = crate::editor::syntax::loader::load_syntax_plugin(&plugin_path) {
                println!("✅ Lazy-loaded Rhai syntax plugin '{}' into egui memory!", safe_filename);
                
                ctx.data_mut(|d| {
                    let cache = d.get_temp_mut_or_default::<SyntaxCache>(cache_id);
                    cache.plugins.insert(mime_type.to_string(), syn.clone());
                });
                return syn;
            }
        }

        // If no plugin exists, cache the built-in to skip disk checks next frame
        ctx.data_mut(|d| {
            let cache = d.get_temp_mut_or_default::<SyntaxCache>(cache_id);
            cache.plugins.insert(mime_type.to_string(), builtin.clone());
        });

        builtin
    }
}
