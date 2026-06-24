use rhai::{Engine, Scope, Map};
use regex::Regex;
use super::{Syntax, DynamicRule, TokenType};
use std::collections::BTreeSet;
use std::path::Path;

pub fn load_syntax_plugin(script_path: &Path) -> Result<Syntax, Box<dyn std::error::Error>> {
    let mut engine = Engine::new();
    engine.set_max_operations(5000); // Prevent infinite loops in user scripts

    let script = std::fs::read_to_string(script_path)?;
    let result: Map = engine.eval_with_scope(&mut Scope::new(), &script)?;

    // Safely leak the language string so it satisfies the &'static str requirement
    // (This is standard practice for plugin names loaded once at startup)
    let lang_string = result.get("language").unwrap().clone().into_string().unwrap();
    let language: &'static str = Box::leak(lang_string.into_boxed_str());

    let raw_rules = result.get("rules").unwrap().clone().into_array().unwrap();
    let mut dynamic_rules = Vec::new();

    for rule_val in raw_rules {
        let rule_map = rule_val.try_cast::<Map>().unwrap();
        let token_str = rule_map.get("token").unwrap().clone().into_string().unwrap();
        let pattern = rule_map.get("regex").unwrap().clone().into_string().unwrap();

        let token_type = match token_str.as_str() {
            "keyword" => TokenType::Keyword,
            "type" => TokenType::Type,
            "special" => TokenType::Special,
            "string" => TokenType::Str('"'),
            "comment" => TokenType::Comment(false),
            "numeric" => TokenType::Numeric(false),
            "punctuation" => TokenType::Punctuation(' '),
            _ => TokenType::Literal,
        };

        // CRITICAL: We wrap the user's regex in ^(...) so it only matches
        // the EXACT beginning of the remaining text buffer.
        let regex = Regex::new(&format!("^({})", pattern))?;

        dynamic_rules.push(DynamicRule {
            token_type,
            pattern,
            regex
        });
    }

    Ok(Syntax {
        language,
        case_sensitive: true,
        comment: "//",
        comment_multiline: ["/*", "*/"],
        hyperlinks: BTreeSet::new(),
        keywords: BTreeSet::new(),
        types: BTreeSet::new(),
        special: BTreeSet::new(),
        dynamic_rules: Some(dynamic_rules),
    })
}