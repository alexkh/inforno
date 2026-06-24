#![allow(dead_code)]
pub mod rust;
pub mod loader;

use std::collections::BTreeSet;
use regex::Regex;
use std::hash::{Hash, Hasher};

#[derive(Clone, Debug)]
pub struct DynamicRule {
    pub token_type: TokenType,
    pub pattern: String,
    pub regex: Regex,
}

impl PartialEq for DynamicRule {
    fn eq(&self, other: &Self) -> bool {
        self.token_type == other.token_type && self.pattern == other.pattern
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
            dynamic_rules: None,
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
        }
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
    pub fn get_or_load(ctx: &egui::Context, ext: &str) -> Self {
        // 1. Fallback logic for hardcoded languages
        if ext != "c" && ext != "h" {
            return Syntax::rust();
        }

        let cache_id = egui::Id::new("editor_syntax_cache");

        // 2. Check egui's internal memory cache for a hit
        let cached_syntax = ctx.data_mut(|d| {
            let cache = d.get_temp_mut_or_default::<SyntaxCache>(cache_id);
            cache.plugins.get(ext).cloned()
        });

        if let Some(syntax) = cached_syntax {
            return syntax; // Cache Hit: Instant return
        }

        // 3. Cache Miss: Load the plugin from disk
        // (For a truly standalone editor, you could later pass a base directory here
        // instead of hardcoding the inforno path)
        let plugin_path = std::path::PathBuf::from(
            std::env::var("HOME").unwrap_or_default()
        ).join(".config/bulat/scripts/syntax/v1/my_c_syntax.rhai");

        let loaded_syntax = match crate::bulat::editor::syntax::loader::load_syntax_plugin(&plugin_path) {
            Ok(syn) => {
                println!("✅ Lazy-loaded Rhai syntax plugin for '.{}' into egui memory!", ext);
                syn
            }
            Err(e) => {
                println!("❌ Failed to lazy-load Rhai plugin: {}", e);
                Syntax::text()
            }
        };

        // 4. Save it back to egui's memory for the next frame
        ctx.data_mut(|d| {
            let cache = d.get_temp_mut_or_default::<SyntaxCache>(cache_id);
            cache.plugins.insert(ext.to_string(), loaded_syntax.clone());
        });

        loaded_syntax
    }
}