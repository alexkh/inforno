// Some LLM's give indented code blocks, which CommonMarkViewer treats as an
// 'embedded' code block, which can start anywhere horizontally and sometimes
// gets squeezed up to 1 character wide which is ridiculous. By applying this
// regexp we are removing indent every time we see tripple backticks.

use eframe::egui;
use egui_commonmark::{CommonMarkCache, CommonMarkViewer};
use regex::Regex;
use std::sync::OnceLock;

struct MyApp {
    cache: CommonMarkCache,
    markdown_initial: String,
    markdown_final: String,
}

impl MyApp {
    fn new() -> Self {
        // Example of "messy" input with indented backticks
        let raw_input = r#"
Here is some text.
    ```rust
    // This code block is indented in the source string
    fn main() {
        println!("Hello");
    }
    ```
The end.
"#;

        Self {
            cache: CommonMarkCache::default(),
            markdown_initial: raw_input.to_owned(),
            // Clean the input immediately
            markdown_final: normalize_code_blocks(raw_input),
        }
    }
}

// Reuse the helper function from above
fn normalize_code_blocks(markdown: &str) -> String {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"(?m)^[ \t]+(```)").unwrap());
    re.replace_all(markdown, "\n$1").to_string()
}

impl eframe::App for MyApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("Raw Markdown Before:");
            ui.label(&self.markdown_initial);
            ui.separator();
            ui.heading("Raw Markdown After:");
            ui.label(&self.markdown_final);
            ui.separator();
            ui.separator();
            ui.heading("Markdown Before:");
            CommonMarkViewer::new()
                .show(ui, &mut self.cache, &self.markdown_initial);
            ui.separator();
            ui.heading("Markdown After:");
            CommonMarkViewer::new()
                .show(ui, &mut self.cache, &self.markdown_final);

        });
    }
}

fn main() -> eframe::Result<()> {
    let native_options = eframe::NativeOptions::default();
    eframe::run_native(
        "Markdown Viewer",
        native_options,
        Box::new(|_| Ok(Box::new(MyApp::new()))),
    )
}