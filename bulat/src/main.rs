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
        }
    } else {
        // --- ZERO FILES: Empty Editor Mode ---
        AppMode::Editor {
            filepath: None,
            code: String::new(),
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
            }))
        }),
    )
}

/// Stores the specific state and files for the active mode
enum AppMode {
    Editor {
        filepath: Option<String>,
        code: String,
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
}

impl eframe::App for StandaloneBulat {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ctx, |ui| {
            match &mut self.mode {
                // ==========================================
                // SINGLE FILE EDITOR VIEW
                // ==========================================
                AppMode::Editor { filepath, code } => {
                    ui.horizontal(|ui| {
                        if let Some(path) = filepath {
                            ui.heading(format!("Editing: {}", path));
                            if ui.button("💾 Save").clicked() {
                                if let Err(e) = fs::write(path, &*code) {
                                    eprintln!("Failed to save file: {}", e);
                                } else {
                                    println!("File saved successfully!");
                                }
                            }
                        } else {
                            ui.heading("New File");
                        }
                        
                        // Push the theme selector to the far right
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            egui::ComboBox::from_id_salt("editor_theme_selector")
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
                    ui.separator();

                    // Dynamically pick syntax based on file extension!
                    let syntax = if let Some(path) = filepath {
                        let ext = Path::new(path)
                            .extension()
                            .and_then(|s| s.to_str())
                            .unwrap_or("");
                        Syntax::get_or_load(ctx, ext)
                    } else {
                        Syntax::rust()
                    };

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
                        ui.heading("Diff Merge");

                        // Push save buttons and theme selector to the far right
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            egui::ComboBox::from_id_salt("diff_theme_selector")
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
