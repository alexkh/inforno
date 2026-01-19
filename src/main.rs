/*
taffy demo
[dependencies]
eframe = "0.33.0"
egui = "0.33.0"
egui_taffy = "0.10.0"
taffy = "0.9.1"
*/

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::PathBuf;

use clap::Parser;
use egui::ViewportBuilder;
use tokio::runtime::Runtime;

use crate::gui::MyAppPermanent;

rust_i18n::i18n!("locales");

mod common;
mod openr;
mod ollama;
mod gui;
mod db;

#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    #[arg(long)]
    // Set the application theme (e.g., "light", "dark")
    theme: Option<String>,
    #[arg(long)]
    sandbox: Option<String>,
    #[arg(long)]
    la: Option<String>,
}

fn main() -> eframe::Result {
    let args = Args::parse();

    // create the tokio runtime
    let rt = Runtime::new().expect("Unable to create Runtime");

    // enter the runtime context
    // this variable must live as long as the app runs!
    let _enter = rt.enter();

    let native_options = eframe::NativeOptions {
        viewport: ViewportBuilder {
            icon: Some(std::sync::Arc::new(egui::IconData {
                rgba: image::load_from_memory(
                        include_bytes!("../assets/inforno_icon.webp"))
                    .unwrap()
                    .to_rgba8()
                    .to_vec(),
                width: 512,
                height: 512,
            })),
            ..Default::default()
        },
        ..Default::default()
    };

    let rt_handle = rt.handle().clone();

    eframe::run_native(
        "inforno",
        native_options,
        Box::new(move |cc| {
            // language setting persistence and overriding
            let mut  app_language = "en".to_string();
            if let Some(storage) = cc.storage {
                if let Some(saved_lang) = eframe::get_value::<String>(
                        storage, "app_language") {
                    app_language = saved_lang;
                }
            }
            if let Some(la) = args.la {
                match la.as_str() {
                    "ru" | "en" => app_language = la,
                    _ => {
                        eprintln!("Warning: Unsupported language '{}'.
                            Supported: 'en', 'ru'.", la);
                    }
                }
            }
            rust_i18n::set_locale(&app_language);

            // theme persistence
            if let Some(theme) = args.theme {
                println!("{}", theme);
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

            let sandbox_string = args.sandbox;
            let sandbox: Option<PathBuf> = sandbox_string.map(PathBuf::from);

            configure_fonts(&cc.egui_ctx);

            Ok(Box::new(gui::MyApp::new(cc, MyAppPermanent {
                rt: rt_handle,
                sandbox,
                app_language: std::sync::Mutex::new(app_language),
            })))
        }),
    )
}

fn configure_fonts(ctx: &egui::Context) {
    // 1. Start with the default fonts
    let mut fonts = egui::FontDefinitions::default();

    // 2. Load the font data
    // easiest way: embed it in the binary so you don't have file path issues
    fonts.font_data.insert(
        "noto_sans_living_regular".to_owned(),
        egui::FontData::from_static(include_bytes!(
                "../assets/fonts/NotoSansLiving-Regular.ttf")).into(),
    );

    // 3. Add it to the font families
    // Put it *last* in the list so it acts as a fallback.
    // Egui will try the primary font first, then fallback to this one for missing glyphs.

    // Add to Proportional (Standard Text)
    if let Some(vec) = fonts.families.get_mut(&egui::FontFamily::Proportional) {
        vec.push("noto_sans_living_regular".to_owned());
    }

    // Add to Monospace (Code blocks)
    if let Some(vec) = fonts.families.get_mut(&egui::FontFamily::Monospace) {
        vec.push("noto_sans_living_regular".to_owned());
    }

    // 2. Load the font data
    // easiest way: embed it in the binary so you don't have file path issues
    fonts.font_data.insert(
        "noto_sans_historical_regular".to_owned(),
        egui::FontData::from_static(include_bytes!(
                "../assets/fonts/NotoSansHistorical-Regular.ttf")).into(),
    );

    // 3. Add it to the font families
    // Put it *last* in the list so it acts as a fallback.
    // Egui will try the primary font first, then fallback to this one for missing glyphs.

    // Add to Proportional (Standard Text)
    if let Some(vec) = fonts.families.get_mut(&egui::FontFamily::Proportional) {
        vec.push("noto_sans_historical_regular".to_owned());
    }

    // Add to Monospace (Code blocks)
    if let Some(vec) = fonts.families.get_mut(&egui::FontFamily::Monospace) {
        vec.push("noto_sans_historical_regular".to_owned());
    }

    // 4. Apply the new configuration
    ctx.set_fonts(fonts);
}