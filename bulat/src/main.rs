use bulat::{DiffApp, BulatEditorApp, EditorAction};
use eframe::egui;
use std::env;
use std::fs;
use std::path::PathBuf;

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
        let path = PathBuf::from(&args[1]);
        let code = fs::read_to_string(&path)
            .unwrap_or_else(|_| format!("// Could not read {}\n", path.display()));

        AppMode::Editor(BulatEditorApp::new(Some(path), code))
    } else {
        // --- NO FILES: Empty Editor ---
        AppMode::Editor(BulatEditorApp::new(None, String::new()))
    };

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([1200.0, 800.0]),
        ..Default::default()
    };

    // Load user theme preference natively directly through Bulat!
    let config = bulat::load_config();

    // 3. Launch the Application
    eframe::run_native(
        "Bulat Merge & Editor",
        options,
        Box::new(move |cc| {
            // Set the correct dark/light visual style on the very first frame!
            if let Some(t_name) = config.theme {
                if let Some(t) = bulat::editor::ColorTheme::available_themes().iter().find(|t| t.name() == t_name) {
                    if t.is_dark() {
                        cc.egui_ctx.set_visuals(egui::Visuals::dark());
                    } else {
                        cc.egui_ctx.set_visuals(egui::Visuals::light());
                    }
                }
            }

            Ok(Box::new(StandaloneBulat {
                mode,
            }))
        }),
    )
}

/// Stores the specific state and files for the active mode
enum AppMode {
    Editor(crate::BulatEditorApp),
    Diff {
        left_filepath: String,
        right_filepath: String,
        app: DiffApp,
    },
}

struct StandaloneBulat {
    mode: AppMode,
}

impl eframe::App for StandaloneBulat {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ctx, |ui| {
            match &mut self.mode {
                AppMode::Editor(app) => {
                    if let Some(action) = app.show(ui) {
                        match action {
                            crate::EditorAction::SaveRequested => {
                                if let Some(path) = &app.filepath {
                                    if let Err(e) = std::fs::write(path, &app.content) {
                                        eprintln!("Failed to save file: {}", e);
                                    } else {
                                        println!("Saved successfully!");
                                    }
                                }
                            }
                            crate::EditorAction::ThemeChanged(_new_theme) => {
                                // If you want standalone Bulat to apply themes, you can do it here!
                            }
                        }
                    }
                },
                AppMode::Diff { left_filepath, right_filepath, app } => {
                    // Ensure standalone specific paths are loaded
                    if app.left_filepath.is_none() {
                        app.left_filepath = Some(left_filepath.clone());
                    }
                    if app.right_filepath.is_none() {
                        app.right_filepath = Some(right_filepath.clone());
                    }

                    let (save_left, save_right) = app.show(ui);

                    if save_left {
                        if let Err(e) = fs::write(left_filepath, &app.left_code_real) {
                            eprintln!("Failed to save left file: {}", e);
                        }
                    }
                    if save_right {
                        if let Err(e) = fs::write(right_filepath, &app.right_code_real) {
                            eprintln!("Failed to save right file: {}", e);
                        }
                    }
                }
            }
        });
    }
}
