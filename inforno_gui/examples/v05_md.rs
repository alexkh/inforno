// markdown example using commonmark. Had to create this file because the
// markdown was skewed when I introduced it into my code directly. Will have
// to work from this dexample up, because in this example everything works fine.

//! Make sure to run this example from the repo directory and not the example
//! directory. To see all the features in full effect, run this example with
//! `cargo r --features better_syntax_highlighting,svg,fetch`
//! Add `light` or `dark` to the end of the command to specify theme. Default
//! is system theme. `cargo r --features better_syntax_highlighting,svg,fetch -- dark`

use eframe::egui;
use egui_commonmark::*;

struct App {
    cache: CommonMarkCache,
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let text = "Montreal, Quebec is home to several universities. Here is a list of the major universities in the city:\n\n1. **McGill University** - A leading English-language institution known for its research programs and diverse student body.\n\n2. **Université de Montréal (UdeM)** - A major French-language university offering a wide range of programs.\n\n3. **Concordia University** - An English-language university known for its flexible approach to education and its business and engineering programs.\n\n4. **Université du Québec à Montréal (UQAM)** - Part of the Université du Québec network, UQAM is a French-language university known for its social sciences and arts programs.\n\n5. **HEC Montréal** - A French-language business school affiliated with Université de Montréal, offering various business-oriented programs.\n\n6. **Polytechnique Montréal** - An engineering school affiliated with Université de Montréal, focusing on applied research and engineering education.\n\nThese institutions contribute to Montreal's reputation as a major hub for higher education in Canada.";
        egui::CentralPanel::default().show(ctx, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                CommonMarkViewer::new()
                    .max_image_width(Some(512))
                    .show(ui, &mut self.cache, text);
            });
        });
    }
}

fn main() -> eframe::Result {
    let mut args = std::env::args();
    args.next();

    eframe::run_native(
        "Markdown viewer",
        eframe::NativeOptions::default(),
        Box::new(move |cc| {
            if let Some(theme) = args.next() {
                if theme == "light" {
                    cc.egui_ctx.set_theme(egui::Theme::Light);
                } else if theme == "dark" {
                    cc.egui_ctx.set_theme(egui::Theme::Dark);
                }
            }

            cc.egui_ctx.style_mut(|style| {
                // Show the url of a hyperlink on hover
                style.url_in_tooltip = true;
            });

            Ok(Box::new(App {
                cache: CommonMarkCache::default(),
            }))
        }),
    )
}