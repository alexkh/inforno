// bulat/src/main.rs
use eframe::egui;
use bulat::editor::{CodeEditor, ColorTheme, Syntax};
// You might also import DiffApp if you want to test the merge tool here

fn main() -> Result<(), eframe::Error> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([800.0, 600.0]),
        ..Default::default()
    };

    eframe::run_native(
        "Bulat Standalone Editor",
        options,
        Box::new(|_cc| Ok(Box::<StandaloneBulat>::default())),
    )
}

#[derive(Default)]
struct StandaloneBulat {
    code: String,
}

impl eframe::App for StandaloneBulat {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ctx, |ui| {
            CodeEditor::default()
                .id_source("standalone_editor")
                .with_theme(ColorTheme::default())
                .with_syntax(Syntax::rust())
                .with_numlines(true)
                .show(ui, &mut self.code);
        });
    }
}
