#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::PathBuf;
use tracing_subscriber::prelude::__tracing_subscriber_SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

use clap::Parser;

use crate::state::MyAppPermanent;

// TEMP: heap profiling (remove after debugging).
#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

rust_i18n::i18n!("locales");

// The flattened UI modules
mod agent_config;
mod autocomplete;
mod bottom_panel;
mod chat;
mod emoji_render;
mod key_manager;
mod panes;
mod preset_editor;
mod search_highlight;
mod side_panel;
mod split_button;
mod state;
mod top_panel;
mod realm_config;
mod workflow_editor;

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

// 1. Force a clean, multi-threaded Tokio runtime that wraps the whole process
#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() -> eframe::Result {
    // Tracy queues every span in RAM until a viewer attaches, and the chat
    // view emits many spans per frame, so only enable it on demand.
    if std::env::var_os("INFORNO_TRACY").is_some() {
        tracing_subscriber::registry()
            .with(tracing_tracy::TracyLayer::default())
            .init();
    }

    // TEMP: run with DHAT=1. Close the window normally (no Ctrl+C) so
    // dhat-heap.json gets written.
    let _dhat = std::env::var_os("DHAT").map(|_| dhat::Profiler::new_heap());
    if std::env::var_os("DHAT").is_some() {
        std::thread::spawn(|| loop {
            std::thread::sleep(std::time::Duration::from_secs(1));
            let heap = dhat::HeapStats::get();
            let rss_mb = std::fs::read_to_string("/proc/self/statm")
                .ok()
                .and_then(|s| s.split_whitespace().nth(1)?.parse::<usize>().ok())
                .map_or(0, |pages| pages * 4096 / 1_048_576);
            eprintln!(
                "DHAT rust_heap={} MB blocks={} rss={} MB",
                heap.curr_bytes / 1_048_576, heap.curr_blocks, rss_mb
            );
        });
    }

    let args = Args::parse();

    // 2. Simply grab the handle of the macro-created runtime!
    // No more manual `let rt = ...` and NO MORE `let _enter = ...`
    let rt_handle = tokio::runtime::Handle::current();

	eprintln!("WINREQ requested logical size = {:?}", saved_window_size());
    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder {
            icon: Some(std::sync::Arc::new(egui::IconData {
                rgba: image::load_from_memory(
                        include_bytes!("../assets/inforno_icon.webp"))
                    .unwrap()
                    .to_rgba8()
                    .to_vec(),
                width: 512,
                height: 512,
            })),
            inner_size: saved_window_size(),
            ..Default::default()
        },
        persist_window: false,
        ..Default::default()
    };

    // native_options.wgpu_options.present_mode = PresentMode::AutoVsync;


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

            // restore zoom and theme saved by MyApp::save()
            if let Some(storage) = cc.storage {
                if let Some(zoom) = eframe::get_value::<f32>(storage, "zoom") {
                    cc.egui_ctx.set_zoom_factor(zoom.clamp(0.5, 4.0));
                }
                match eframe::get_value::<String>(storage, "theme").as_deref() {
                    Some("light") => cc.egui_ctx.set_theme(egui::Theme::Light),
                    Some("dark") => cc.egui_ctx.set_theme(egui::Theme::Dark),
                    _ => {}
                }
            }

            // theme persistence
            if let Some(theme) = args.theme {
                println!("{}", theme);
                if theme == "light" {
                    cc.egui_ctx.set_theme(egui::Theme::Light);
                } else if theme == "dark" {
                    cc.egui_ctx.set_theme(egui::Theme::Dark);
                }
            }

            cc.egui_ctx.style_mut_of(cc.egui_ctx.theme(), |style| {
                // Show the url of a hyperlink on hover
                style.url_in_tooltip = true;
            });

            let sandbox_string = args.sandbox;
            let mut sandbox: Option<PathBuf> = sandbox_string.map(PathBuf::from);
            let pending_project_init: Option<PathBuf> = None;
            let mut active_realm_name: Option<String> = None;

            // Determine target realm (CLI arg or positional argument takes top priority over global config)
            let mut target_realm = args.realm.clone();
            let mut positional_path = args.project_dir.clone();

            // If a positional argument is passed, treat it strictly as the realm name
            if target_realm.is_none() {
                if let Some(pos_arg) = &positional_path {
                    target_realm = Some(pos_arg.clone());
                    positional_path = None; // Consume it so it's not treated as a project dir
                }
            }

            if target_realm.is_none() && positional_path.is_none() {
                // If nothing was passed, check the global config.yml for a default realm
                if let Some(proj_dirs) = directories::ProjectDirs::from("", "", "inforno") {
                    let global_config_path = proj_dirs.config_dir().join("config.yml");
                    if let Ok(contents) = std::fs::read_to_string(global_config_path) {
                        #[derive(serde::Deserialize)]
                        struct GlobalConfig {
                            default_realm: Option<String>,
                        }
                        if let Ok(val) = serde_saphyr::from_str::<GlobalConfig>(&contents) {
                            if let Some(r) = val.default_realm {
                                target_realm = Some(r);
                            }
                        }
                    }
                }
            }

            let mut realm_awaiting_sandbox: Option<String> = None;

            if let Some(realm_name) = target_realm {
                // Booting into a Realm Environment. Realms and Sandboxes are fully
                // decoupled: the Sandbox is whatever this Realm's config resolves
                // it to (a Study-backed file, or an explicit path), never
                // something implied by realm_dir.
                if let Some(proj_dirs) = directories::ProjectDirs::from("", "", "inforno") {
                    let realm_dir = proj_dirs.config_dir().join("realms").join(&realm_name);
                    let yaml_path = realm_dir.join("realm2.yml");

                    match std::fs::read_to_string(&yaml_path) {
                        Ok(config_str) => match serde_saphyr::from_str::<inforno_core::realm::RealmConfig>(&config_str) {
                            Ok(realm_config) => {
                                match inforno_core::realm::resolve_default_sandbox_path(&realm_config) {
                                    Ok(resolved) => {
                                        sandbox = Some(resolved);
                                        active_realm_name = Some(realm_name.clone());
                                    }
                                    Err(reason) => {
                                        let err_msg = format!("Realm '{}' has no default sandbox yet ({}); will prompt to create one.", realm_name, reason);
                                        eprintln!("{}", err_msg);
                                        cc.egui_ctx.data_mut(|d| d.insert_temp(egui::Id::new("startup_error"), err_msg));
                                        realm_awaiting_sandbox = Some(realm_name.clone());
                                    }
                                }
                            }
                            Err(e) => {
                                let err_msg = format!("Failed to parse realm2.yml for realm '{}':\n{}", realm_name, e);
                                eprintln!("{}", err_msg);
                                cc.egui_ctx.data_mut(|d| {
                                    d.insert_temp(egui::Id::new("startup_error"), err_msg);
                                    d.insert_temp(egui::Id::new("broken_realm_yaml"), config_str);
                                    d.insert_temp(egui::Id::new("broken_realm_name"), realm_name.clone());
                                });
                            }
                        },
                        Err(e) => {
                            let err_msg = format!("Realm '{}' not found or could not be read at {:?}\nError: {}", realm_name, yaml_path, e);
                            eprintln!("{}", err_msg);
                            cc.egui_ctx.data_mut(|d| d.insert_temp(egui::Id::new("startup_error"), err_msg));
                        }
                    }
                }
            }
            // NOTE: `inforno <directory>` no longer auto-opens or offers to
            // create a bare `.inforno/info.rno` Project sandbox. A
            // directory-rooted sandbox is still supported, but only by
            // pointing a Realm's `sandboxes:` entry at it, or by opening it
            // explicitly via the in-app "Open Sandbox" dialog. `_positional_path`
            // is intentionally unused for this purpose now.
            let _ = positional_path;

            configure_fonts(&cc.egui_ctx);

            Ok(Box::new(state::MyApp::new(cc, MyAppPermanent {
                rt: rt_handle,
                sandbox,
                pending_project_init: std::sync::Mutex::new(pending_project_init),
                active_realm_name: std::sync::Mutex::new(active_realm_name),
                realm_awaiting_sandbox: std::sync::Mutex::new(realm_awaiting_sandbox),
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
    // NotoColorEmoji is intentionally NOT registered here: egui cannot draw
    // color bitmap glyphs. Color emoji go through `emoji_render` instead.

    // 4. Apply the new configuration
    ctx.set_fonts(fonts);
}

pub fn window_size_path() -> Option<PathBuf> {
    directories::ProjectDirs::from("", "", "inforno")
        .map(|d| d.config_dir().join("window_size.txt"))
}

/// Last window size in logical pixels (zoom-independent), if sane.
fn saved_window_size() -> Option<egui::Vec2> {
    let text = std::fs::read_to_string(window_size_path()?).ok()?;
    let mut it = text.split_whitespace().map(|s| s.parse::<f32>().ok());
    let (w, h) = (it.next()??, it.next()??);
    (w >= 400.0 && h >= 300.0).then(|| egui::vec2(w, h))
}
