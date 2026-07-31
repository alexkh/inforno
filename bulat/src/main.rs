use bulat::editor::{CodeEditor, ColorTheme, Syntax};
use bulat::DiffApp;
use eframe::egui;
use std::env;
use std::fs;
use std::path::Path;

#[derive(serde::Serialize, serde::Deserialize, Default)]
struct GlobalConfig {
    theme: Option<String>,
}

fn global_config_path() -> Option<std::path::PathBuf> {
    directories::ProjectDirs::from("", "", "bulat")
        .map(|proj_dirs| proj_dirs.config_dir().join("global.yml"))
}

fn load_global_config() -> GlobalConfig {
    if let Some(path) = global_config_path() {
        if let Ok(content) = std::fs::read_to_string(&path) {
            if let Ok(config) = serde_yaml::from_str::<GlobalConfig>(&content) {
                return config;
            }
        }
    }
    GlobalConfig::default()
}

fn save_global_config(theme_name: &str) {
    if let Some(path) = global_config_path() {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let config = GlobalConfig {
            theme: Some(theme_name.to_string()),
        };
        if let Ok(yaml) = serde_yaml::to_string(&config) {
            let _ = std::fs::write(path, yaml);
        }
    }
}

fn main() -> eframe::Result {
    // 1. Parse CLI Arguments
    let args: Vec<String> = env::args().collect();

    // 2. Determine Mode based on argument count
    let mode = if args.len() >= 3 {
        // --- TWO FILES: Diff Mode ---
        let left_path = args[1].clone();
        let right_path = args[2].clone();

        let left_content = fs::read_to_string(&left_path)
            .unwrap_or_else(|_| format!("// Could not read {}\n", left_path));
        let right_content = fs::read_to_string(&right_path)
            .unwrap_or_else(|_| format!("// Could not read {}\n", right_path));

        AppMode::Diff {
            left_filepath: left_path,
            right_filepath: right_path,
            app: DiffApp::new(left_content, right_content),
        }
    } else if args.len() == 2 {
        // --- ONE FILE: Editor Mode ---
        let path = args[1].clone();
        let code = fs::read_to_string(&path)
            .unwrap_or_else(|_| format!("// Could not read {}\n", path));

        AppMode::Editor {
            filepath: Some(path),
            code,
            language_override: None,
        }
    } else {
        // --- ZERO FILES: Empty Editor Mode ---
        AppMode::Editor {
            filepath: None,
            code: String::new(),
            language_override: None,
        }
    };

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([1200.0, 800.0]),
        ..Default::default()
    };

    // Load user theme preference
    let global_config = load_global_config();
    let mut initial_theme = ColorTheme::default();

    if let Some(saved_theme_name) = global_config.theme {
        if let Some(found_theme) = ColorTheme::available_themes().iter().find(|t| t.name() == saved_theme_name) {
            initial_theme = *found_theme;
        }
    }

    // 3. Launch the Application
    eframe::run_native(
        "Bulat Merge & Editor",
        options,
        Box::new(move |cc| {
            // Set the correct dark/light visual style on the very first frame!
            if initial_theme.is_dark() {
                cc.egui_ctx.set_visuals(egui::Visuals::dark());
            } else {
                cc.egui_ctx.set_visuals(egui::Visuals::light());
            }

            Ok(Box::new(StandaloneBulat {
                mode,
                current_theme: initial_theme,
                show_settings: false,
            }))
        }),
    )
}

/// Stores the specific state and files for the active mode
enum AppMode {
    Editor {
        filepath: Option<String>,
        code: String,
        language_override: Option<String>,
    },
    Diff {
        left_filepath: String,
        right_filepath: String,
        app: DiffApp,
    },
}

struct StandaloneBulat {
    mode: AppMode,
    current_theme: ColorTheme,
    show_settings: bool,
}

impl eframe::App for StandaloneBulat {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // --- Settings Window ---
        let mut show_settings = self.show_settings;
        if show_settings {
            egui::Window::new("⚙ Editor Configuration")
                .collapsible(false)
                .resizable(false)
                .open(&mut show_settings)
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        ui.label("Theme:");
                        egui::ComboBox::from_id_salt("global_theme_selector")
                            .selected_text(self.current_theme.name())
                            .show_ui(ui, |ui| {
                                for theme in ColorTheme::available_themes() {
                                    if ui.selectable_value(&mut self.current_theme, *theme, theme.name()).changed() {
                                        if theme.is_dark() {
                                            ctx.set_visuals(egui::Visuals::dark());
                                        } else {
                                            ctx.set_visuals(egui::Visuals::light());
                                        }
                                        save_global_config(theme.name());
                                    }
                                }
                            });
                    });
                });
        }
        self.show_settings = show_settings;

        egui::CentralPanel::default().show(ctx, |ui| {
            match &mut self.mode {
                // ==========================================
                // SINGLE FILE EDITOR VIEW
                // ==========================================
                AppMode::Editor { filepath, code, language_override } => {
                    ui.horizontal(|ui| {
                        if ui.button("🔧").on_hover_text("Open Settings").clicked() {
                            self.show_settings = true;
                        }

                        // Safely extract the current MIME directly (no extensions involved!)
                        let current_mime = language_override.clone().unwrap_or_else(|| {
                            filepath.as_ref()
                                .map(|p| Syntax::guess_mime_from_path(Path::new(p)))
                                .unwrap_or("text/plain")
                                .to_string()
                        });

                        if let Some(path) = &*filepath {
                            ui.heading(format!("Editing: {}", path));
                        } else {
                            ui.heading("New File");
                        }

                        egui::ComboBox::from_id_salt("mime_type_selector")
                            .selected_text(&current_mime)
                            .show_ui(ui, |ui| {
                                let supported_mimes = [
                                    "text/plain", "text/rust", "text/x-c", "text/x-c++", 
                                    "application/x-rhai", "text/markdown", "application/json", 
                                    "application/toml", "application/yaml", "text/javascript", 
                                    "text/typescript", "text/x-python", "text/html", "text/css", 
                                    "application/x-sh"
                                ];
                                for &mime_opt in &supported_mimes {
                                    if ui.selectable_label(current_mime == mime_opt, mime_opt).clicked() {
                                        *language_override = Some(mime_opt.to_string());
                                    }
                                }
                            });

                        if let Some(path) = &*filepath {
                            if ui.button("💾 Save").clicked() {
                                if let Err(e) = fs::write(path, &*code) {
                                    eprintln!("Failed to save file: {}", e);
                                } else {
                                    println!("File saved successfully!");
                                }
                            }
                        }
                    });
                    ui.separator();

                    let active_mime = language_override.clone().unwrap_or_else(|| {
                        filepath.as_ref()
                            .map(|p| Syntax::guess_mime_from_path(Path::new(p)))
                            .unwrap_or("text/plain")
                            .to_string()
                    });

                    // Engine uses MIME type exclusively now
                    let syntax = Syntax::get_or_load(ctx, &active_mime);

                    CodeEditor::default()
                        .id_source("standalone_editor")
                        .with_theme(self.current_theme)
                        .with_syntax(syntax)
                        .with_numlines(true)
                        .vscroll(true)
                        .v_auto_shrink(false) // Force the editor to fill the window
                        .show(ui, code);
                }

                // ==========================================
                // DUAL FILE MERGE VIEW
                // ==========================================
                AppMode::Diff { left_filepath, right_filepath, app } => {
                    ui.horizontal(|ui| {
                        if ui.button("🔧").on_hover_text("Open Settings").clicked() {
                            self.show_settings = true;
                        }

                        ui.heading("Diff Merge");

                        // Push save buttons to the far right
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.button("💾 Save Right File").clicked() {
                                if let Err(e) = fs::write(&right_filepath, &app.right_code_real) {
                                    eprintln!("Failed to save right file: {}", e);
                                }
                            }
                            if ui.button("💾 Save Left File").clicked() {
                                if let Err(e) = fs::write(&left_filepath, &app.left_code_real) {
                                    eprintln!("Failed to save left file: {}", e);
                                }
                            }
                        });
                    });
                    ui.separator();

                    app.set_theme(self.current_theme);
                    app.show(ui);
                }
            }
        });
    }
}
