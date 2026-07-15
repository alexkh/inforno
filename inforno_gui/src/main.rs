#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::PathBuf;
use tracing_subscriber::prelude::__tracing_subscriber_SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

use clap::Parser;
use egui::ViewportBuilder;
use tokio::runtime::Runtime;

use crate::state::MyAppPermanent;

rust_i18n::i18n!("locales");

// The flattened UI modules
mod agent_config;
mod autocomplete;
mod bottom_panel;
mod chat;
mod key_manager;
mod math_render;
mod panes;
mod preset_editor;
mod side_panel;
mod split_button;
mod state;
mod top_panel;

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
    // Optional project directory to load a local Sandbox from
    #[arg(required = false)]
    project_dir: Option<String>,
    #[arg(long)]
    // Optional Realm to load
    realm: Option<String>,
}

fn main() -> eframe::Result {
    // Initialize Tracy profiling
    tracing_subscriber::registry()
        .with(tracing_tracy::TracyLayer::default())
        .init();

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

    // native_options.wgpu_options.present_mode = PresentMode::AutoVsync;

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
            let mut sandbox: Option<PathBuf> = sandbox_string.map(PathBuf::from);
            let mut pending_project_init: Option<PathBuf> = None;
            let mut active_realm_name: Option<String> = None;

            // Determine target realm (CLI arg takes priority over global config)
            let mut target_realm = args.realm.clone();
            let mut positional_path = args.project_dir.clone();

            // Allow positional argument to act as a realm name (e.g., `inforno inforno`)
            if target_realm.is_none() {
                if let Some(pos_arg) = &positional_path {
                    if let Some(proj_dirs) = directories::ProjectDirs::from("", "", "inforno") {
                        let realm_dir = proj_dirs.config_dir().join("realms").join(pos_arg);
                        if realm_dir.exists() && realm_dir.join("realm.yml").exists() {
                            target_realm = Some(pos_arg.clone());
                            positional_path = None; // Consume it so it's not treated as a project dir
                        }
                    }
                }
            }

            if target_realm.is_none() && positional_path.is_none() {
                // If nothing was passed, check the global config.yml for a default realm
                if let Some(proj_dirs) = directories::ProjectDirs::from("", "", "inforno") {
                    let global_config_path = proj_dirs.config_dir().join("config.yml");
                    if let Ok(contents) = std::fs::read_to_string(global_config_path) {
                        if let Ok(val) = serde_yaml::from_str::<serde_yaml::Value>(&contents) {
                            if let Some(r) = val.get("default_realm").and_then(|v| v.as_str()) {
                                target_realm = Some(r.to_string());
                            }
                        }
                    }
                }
            }

            if let Some(realm_name) = target_realm {
                // 1. Booting into a Realm Environment
                if let Some(proj_dirs) = directories::ProjectDirs::from("", "", "inforno") {
                    let realm_dir = proj_dirs.config_dir().join("realms").join(&realm_name);

                    if realm_dir.exists() {
                        sandbox = Some(realm_dir.join("info.rno"));
                        active_realm_name = Some(realm_name);
                    } else {
                        eprintln!("Warning: Realm '{}' not found at {:?}", realm_name, realm_dir);
                    }
                }
            } else if let Some(proj_str) = positional_path {
                // 2. Standard single-project mode (Fallback)
                let proj_path = PathBuf::from(proj_str);
                if proj_path.is_dir() {
                    let db_path = proj_path.join(".inforno").join("info.rno");
                    if db_path.exists() {
                        // Project sandbox exists, load it directly
                        sandbox = Some(db_path);
                    } else {
                        // Directory exists, but no sandbox yet. Flag for UI modal.
                        pending_project_init = Some(proj_path);
                    }
                }
            }

            configure_fonts(&cc.egui_ctx);

            Ok(Box::new(state::MyApp::new(cc, MyAppPermanent {
                rt: rt_handle,
                sandbox,
                pending_project_init: std::sync::Mutex::new(pending_project_init),
                active_realm_name: std::sync::Mutex::new(active_realm_name),
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

    // 2. Load the font data
    // easiest way: embed it in the binary so you don't have file path issues
    fonts.font_data.insert(
        "noto_sans_cjk_regular".to_owned(),
        egui::FontData::from_static(include_bytes!(
                "../assets/fonts/NotoSansCJKsc-Regular.otf")).into(),
    );

    // 3. Add it to the font families
    // Put it *last* in the list so it acts as a fallback.
    // Egui will try the primary font first, then fallback to this one for missing glyphs.

    // Add to Proportional (Standard Text)
    if let Some(vec) = fonts.families.get_mut(&egui::FontFamily::Proportional) {
        vec.push("noto_sans_cjk_regular".to_owned());
    }

    // Add to Monospace (Code blocks)
    if let Some(vec) = fonts.families.get_mut(&egui::FontFamily::Monospace) {
        vec.push("noto_sans_cjk_regular".to_owned());
    }

    // 2. Load the font data
    // easiest way: embed it in the binary so you don't have file path issues
    fonts.font_data.insert(
        "noto_emoji_regular".to_owned(),
        egui::FontData::from_static(include_bytes!(
                "../assets/fonts/NotoEmoji-Regular.ttf")).into(),
    );

    // 3. Add it to the font families
    // Put it *last* in the list so it acts as a fallback.
    // Egui will try the primary font first, then fallback to this one for missing glyphs.

    // Add to Proportional (Standard Text)
    if let Some(vec) = fonts.families.get_mut(&egui::FontFamily::Proportional) {
        vec.push("noto_emoji_regular".to_owned());
    }

    // Add to Monospace (Code blocks)
    if let Some(vec) = fonts.families.get_mut(&egui::FontFamily::Monospace) {
        vec.push("noto_emoji_regular".to_owned());
    }

    // 4. Apply the new configuration
    ctx.set_fonts(fonts);
}
