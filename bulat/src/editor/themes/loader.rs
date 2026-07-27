use serde::Deserialize;
use std::sync::OnceLock;
use super::{ColorTheme, DEFAULT_THEMES};

#[derive(Deserialize)]
struct ThemeConfig {
    name: String,
    dark: bool,
    bg: String,
    cursor: String,
    selection: String,
    comments: String,
    functions: String,
    keywords: String,
    literals: String,
    numerics: String,
    punctuation: String,
    strs: String,
    types: String,
    special: String,
}

impl ThemeConfig {
    /// Converts standard Strings into &'static str by leaking them.
    /// This is perfectly safe and idiomatic because themes live for the 
    /// entire lifetime of the application.
    fn into_color_theme(self) -> ColorTheme {
        ColorTheme {
            name: Box::leak(self.name.into_boxed_str()),
            dark: self.dark,
            bg: Box::leak(self.bg.into_boxed_str()),
            cursor: Box::leak(self.cursor.into_boxed_str()),
            selection: Box::leak(self.selection.into_boxed_str()),
            comments: Box::leak(self.comments.into_boxed_str()),
            functions: Box::leak(self.functions.into_boxed_str()),
            keywords: Box::leak(self.keywords.into_boxed_str()),
            literals: Box::leak(self.literals.into_boxed_str()),
            numerics: Box::leak(self.numerics.into_boxed_str()),
            punctuation: Box::leak(self.punctuation.into_boxed_str()),
            strs: Box::leak(self.strs.into_boxed_str()),
            types: Box::leak(self.types.into_boxed_str()),
            special: Box::leak(self.special.into_boxed_str()),
        }
    }
}

fn load_custom_themes() -> Vec<ColorTheme> {
    let mut custom_themes = Vec::new();

    // Resolves to:
    // Linux: ~/.config/bulat/themes/
    // Windows: C:\Users\Username\AppData\Roaming\bulat\config\themes\
    // macOS: ~/Library/Application Support/bulat/themes/
    if let Some(proj_dirs) = directories::ProjectDirs::from("", "", "bulat") {
        let themes_dir = proj_dirs.config_dir().join("themes");

        // Create the directory automatically so the user can easily find it
        if !themes_dir.exists() {
            let _ = std::fs::create_dir_all(&themes_dir);
        }

        if themes_dir.is_dir() {
            if let Ok(entries) = std::fs::read_dir(themes_dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.extension().is_some_and(|ext| ext == "yml" || ext == "yaml") {
                        if let Ok(content) = std::fs::read_to_string(&path) {
                            match serde_yaml::from_str::<ThemeConfig>(&content) {
                                Ok(config) => custom_themes.push(config.into_color_theme()),
                                Err(e) => eprintln!("Failed to parse theme {:?}: {}", path, e),
                            }
                        }
                    }
                }
            }
        }
    }
    custom_themes
}

/// A globally cached array containing both built-in and user YAML themes
pub fn available_themes() -> &'static [ColorTheme] {
    static THEMES: OnceLock<Vec<ColorTheme>> = OnceLock::new();
    THEMES.get_or_init(|| {
        let mut themes = DEFAULT_THEMES.to_vec();
        themes.extend(load_custom_themes());
        themes
    })
}
