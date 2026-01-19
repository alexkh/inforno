/*
adding gui to the openrouter app
[dependencies]
dotenv = "0.15.0"
eframe = "0.33.0"
egui = "0.33.0"
env_logger = "0.11.8"
openrouter_api = { version = "0.1.6", features = ["tracing"] }
tokio = "1.47.1"
*/
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use eframe::egui;

fn main() -> eframe::Result {
    env_logger::init(); // Log to stderr (if you run with `RUST_LOG=debug`).

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([800.0, 1000.0]),
        ..Default::default()
    };

    // Our application state:
    let mut name = "Arthur".to_owned();
    let mut age = 42;

    eframe::run_simple_native("inforno", options, move |ctx, _frame| {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("inforno");
            ui.horizontal(|ui| {
                let name_label = ui.label("Prompt: ");
                ui.text_edit_multiline(&mut name)
                    .labelled_by(name_label.id);
            });
            ui.add(egui::Slider::new(&mut age, 0..=120).text("age"));
            if ui.button("Increment").clicked() {
                age += 1;
            }
            ui.label(format!("Hello '{name}', age {age}"));
        });
    })
}
