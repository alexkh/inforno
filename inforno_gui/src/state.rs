use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, LazyLock, RwLock, Mutex};
use std::sync::mpsc::{Receiver, Sender, channel};
use eframe::egui::{self};
use egui_commonmark::CommonMarkCache;
use rusqlite::Connection;
use tokio::runtime::Handle;
use egui::Color32;
use inforno_core::common::{self, ChatRouter, ApiKey, ChatStreamEvent, DbChat, FileOp, FileOpMsg, ModelPresets, OllamaDownloading, load_model_presets};
use inforno_core::db::{fetch_chat, fetch_chat_titles, get_folio_db_conn, mod_msg_content_reasoning};
use inforno_core::db::cache::{get_cache_db_conn, get_ollama_model_names, get_openr_model_names};
use crate::talker_config::{TalkerConfigState, ui_talker_config};
use crate::bottom_panel::{BottomPanelState, ui_bottom_panel};
use crate::chat::ui_chat;
use crate::key_manager::ui_key_manager;
use crate::model_preset_editor::{ModelPresetEditorState, ui_model_preset_editor};
use crate::realm_config::{RealmConfigState, ui_realm_config};
use crate::side_panel::ui_side_panel;
use crate::top_panel::ui_top_panel;
use crate::workflow_editor::{WorkflowEditorState, ui_workflow_editor};
use base64::{Engine as _, engine::general_purpose::STANDARD};


pub static THEME_COLORS: LazyLock<RwLock<AppColors>> = LazyLock::new(|| {
    RwLock::new(AppColors::default())
});

#[derive(Clone, Copy)]
pub struct AppColors {
    pub cloud: Color32,
    pub local: Color32,
    pub text: Color32,
    pub strong: Color32,
    pub err: Color32,
}

impl Default for AppColors {
    fn default() -> Self {
        Self {
            cloud: Color32::from_rgb(0, 0, 255), // Default Blue
            local: Color32::from_rgb(127, 127, 127), // Default Grey
            text: Color32::from_rgb(127, 127, 127), // Default Grey
            strong: Color32::from_rgb(127, 127, 127), // Default Grey
            err: Color32::from_rgb(255, 0, 0), // Default Red
        }
    }
}

// Helper functions for clean access
pub fn cloud_color() -> Color32 {
    THEME_COLORS.read().unwrap().cloud
}

pub fn local_color() -> Color32 {
    THEME_COLORS.read().unwrap().local
}

pub fn text_color() -> Color32 {
    THEME_COLORS.read().unwrap().text
}

pub fn strong_color() -> Color32 {
    THEME_COLORS.read().unwrap().strong
}

pub fn err_color() -> Color32 {
    THEME_COLORS.read().unwrap().err
}

pub fn router_color(router: &ChatRouter) -> Color32 {
    match router {
        ChatRouter::Ollama => local_color(),
        ChatRouter::Openrouter => cloud_color(),
    }
}

#[derive(Default, Clone)]
pub struct ChatMsgUi {
    pub show_raw: bool,
    pub is_rhai: Option<bool>, // NEW: Caches the syntax detection per message

    // Inline Diff Tracking (Index of the chunk -> Diff App State)
    pub inline_diffs: std::collections::HashMap<usize, bulat::DiffApp>,
    pub inline_diffs_saved: std::collections::HashMap<usize, bool>,
}

pub struct MyAppPermanent {
    pub rt: Handle,
    /// Inforno's vault, opened once in main and kept across reloads.
    pub keys: inforno_core::api_keys::KeyStore,
    /// What startup found out about the vault; taken by the first `State::new`.
    pub vault_notices: Mutex<Option<Vec<String>>>,
    pub folio: Option<PathBuf>,
    pub active_realm_name: Mutex<Option<String>>,
    /// Set at boot when `--realm <name>` was given but the Realm has no
    /// resolvable default Folio yet. Consumed once by `State::new`, which
    /// turns it into a modal prompt rather than creating a Folio unasked.
    pub realm_awaiting_folio: Mutex<Option<String>>,
    pub app_language: Mutex<String>,
}

pub struct ChatStreamingState {
    pub streaming: bool,
    pub bitmask: u128, // each bit flags which talker is streaming
    pub msg_ids: Vec<i64>,
    pub content_buffers: Vec<String>, // used when chat streaming
    pub reasoning_buffers: Vec<String>,
    pub abort_flag: Option<Arc<AtomicBool>>,
    pub rx: Receiver<ChatStreamEvent>,
    pub tx: Sender<ChatStreamEvent>,
}

//#[derive(serde::Deserialize, serde::Serialize)]
//#[serde(default)]
pub struct State {
    pub perma: Arc<MyAppPermanent>,
    pub tile_labels: std::collections::HashMap<egui_tiles::TileId, String>,
    pub active_tile_id: Option<egui_tiles::TileId>,
    pub chat_locations: std::collections::HashMap<i64, Vec<String>>,
    // chat: common::Chat,
    pub open_chats: HashMap<i64, common::Chat>,
    pub active_chat_id: Option<i64>,
    pub chat_msg_ui: HashMap<i64, ChatMsgUi>,
    pub chat_to_rename: Option<i64>,
    pub chat_rename_buffer: String,
    pub common_mark_cache: CommonMarkCache,
    pub presets: ModelPresets,
    pub cache_conn: Option<rusqlite::Connection>, // connection to cache db
    pub db_conn: rusqlite::Connection, // connection to main db
    pub db_chats: Vec<DbChat>, // chat titles fetched from the main db
    pub show_key_manager: bool,
    pub show_preset_editor: bool,
    pub show_realm_config: bool,
    pub show_workflow_editor: bool,
    pub realm_config_state: RealmConfigState,
    pub workflow_editor_state: WorkflowEditorState,
    pub api_key_entered: String,
    pub openrouter_api_key: ApiKey,
    /// The active OpenRouter key is saved in the encrypted key file.
    pub key_saved: bool,
    /// Why saved keys couldn't be read or saved, if they couldn't.
    pub vault_error: Option<String>,
    /// Which vaultorno backend holds saved keys, and until when.
    pub key_storage: crate::key_manager::KeyStorage,
    /// Startup warnings about the vault, shown in the key manager.
    pub vault_notices: Vec<String>,
    pub preset_editor_state: ModelPresetEditorState,
    pub openr_model_names: Vec<String>,
    pub ollama_model_names: Vec<String>,
    pub ollama_model_names_installed: Vec<String>,
    pub op_tx: Sender<FileOpMsg>,
    pub pending_file_dialog_op: Option<FileOp>,
    pub pending_export_content: Option<String>,
    pub is_in_home_folio: bool,
    pub folio: PathBuf,
    // when receiving reply from the LLM's we need to store them during
    // streaming. Each buffer is indexed by the Talker index.
    pub chat_streaming_state: ChatStreamingState,
    // error modal's content:
    pub error_msg: Option<String>,
    pub is_modal_open: bool,
    pub bottom_panel_state: BottomPanelState,
    pub talker_config_state: TalkerConfigState,
    pub math_cache: std::rc::Rc<std::cell::RefCell<std::collections::HashMap<String, std::sync::Arc<[u8]>>>>,
    pub copy_presets_checked: bool,
    pub show_realm_folio_init_modal: bool,
    pub realm_awaiting_folio: Option<String>,
    pub folio_name_buffer: String,
    pub project_root: Option<PathBuf>,
    pub active_realm: Option<inforno_core::realm::ActiveRealm>,
    pub active_workspace_name: Option<String>,
    pub merge_apps: std::collections::HashMap<egui_tiles::TileId, bulat::DiffApp>,
    pub editor_apps: std::collections::HashMap<egui_tiles::TileId, bulat::BulatEditorApp>,
    pub file_dialog: egui_file_dialog::FileDialog,
    pub pane_tree: egui_tiles::Tree<crate::panes::Pane>,
    pub search_query: String,
    pub chat_widths: std::collections::HashMap<i64, f32>,
    /// Search term highlighted in a chat opened from the search results, by chat id
    pub chat_highlights: std::collections::HashMap<i64, crate::search_highlight::ChatHighlight>,
    /// Old key names the open Realm's realm2.yml still uses, for the banner
    /// (`inforno_core::realm::legacy_realm_keys`). Empty outside a Realm.
    pub realm_legacy_keys: Vec<String>,
}

impl State {
    /// This function handles the heavy lifting of initialization.
    /// It can be called by `Default` or manually to reload the state.
    pub fn new(
        permanent: Arc<MyAppPermanent>,
        new_folio: Option<PathBuf>,
        op_tx: Sender<FileOpMsg>
    ) -> Self {
        let is_home = new_folio.is_none();
        let mut openr_model_names: Vec<String> = vec![];
        let mut ollama_model_names: Vec<String> = vec![];
        let ollama_model_names_installed: Vec<String> = vec![];

        // --- 1. Main Database Connection ---

        let mut chats: Vec<DbChat> = vec![];
        let mut presets = ModelPresets::default();

        // 1. Establish Connection or Die
        let (conn, folio) = match get_folio_db_conn(&new_folio) {
            Ok(tuple) => tuple,
            Err(error) => {
                eprintln!("Error opening the Folio: {}", error);
                std::process::exit(1); // Quit immediately
            }
        };

        let mut project_root = None;
        let mut active_realm = None;
        let mut active_workspace_name = None;

        let realm_name_lock = permanent.active_realm_name.lock().unwrap().clone();

        if let Some(realms_dir) = directories::ProjectDirs::from("", "", "inforno").map(|d| d.config_dir().join("realms")) {
            if !inforno_core::realm::folio_path_is_valid(&folio, &realms_dir) {
                eprintln!(
                    "Warning: Folio at {:?} lives inside the Realms directory ({:?}). \
                     Folios must stay outside .config/inforno/realms.",
                    folio, realms_dir
                );
            }
        }

        let mut initial_error_msg = None;
        let mut realm_legacy_keys = Vec::new();
        let mut is_fixing_broken_realm = false;
        let mut broken_realm_yaml = String::new();
        let mut broken_realm_name = String::new();

        if let Some(realm_name) = realm_name_lock {
            // We are in a Realm! Realms always live under the fixed realms
            // directory now — never derived from the Folio's location,
            // since a Folio can live anywhere.
            let realm_dir = directories::ProjectDirs::from("", "", "inforno")
                .map(|d| d.config_dir().join("realms").join(&realm_name));

            if let Some(realm_dir) = realm_dir {
                let yaml_path = realm_dir.join("realm2.yml");
                match std::fs::read_to_string(&yaml_path) {
                    Ok(config_str) => {
                        realm_legacy_keys = inforno_core::realm::legacy_realm_keys(&config_str);
                        match serde_saphyr::from_str::<inforno_core::realm::RealmConfig>(&config_str) {
                            Ok(raw_config) => {
                                match inforno_core::realm::ActiveRealm::from_config(realm_name.clone(), raw_config) {
                                    Ok(realm) => {
                                        // Resolve the default place (Fallback to the first mount point)
                                        let mut selected_workspace_name = None;
                                        let mut selected_project_root = None;

                                        // 1. Try to grab the first globally defined Place
                                        if let Some(first_place_vpath) = realm.raw_config.places.values().next() {
                                            selected_workspace_name = Some(first_place_vpath.0.clone());

                                            let mut found = false;
                                            for mount in &realm.mounts {
                                                if first_place_vpath.0.starts_with(&mount.virtual_path) {
                                                    let relative = first_place_vpath.0.strip_prefix(&mount.virtual_path).unwrap_or("").trim_start_matches('/');
                                                    selected_project_root = Some(mount.host_path.join(relative));
                                                    found = true;
                                                    break;
                                                }
                                            }
                                            // Fallback for raw host paths
                                            if !found {
                                                selected_project_root = Some(std::path::PathBuf::from(&first_place_vpath.0));
                                            }
                                        }

                                        // 2. Fallback to the first Mount if there are no Places
                                        if selected_workspace_name.is_none() {
                                            if let Some(first_mount) = realm.mounts.first() {
                                                selected_workspace_name = Some(first_mount.virtual_path.clone());
                                                selected_project_root = Some(first_mount.host_path.clone());
                                            }
                                        }

                                        if selected_workspace_name.is_some() {
                                            active_workspace_name = selected_workspace_name;
                                            project_root = selected_project_root;
                                        }
                                        active_realm = Some(realm);
                                    }
                                    Err(e) => {
                                        initial_error_msg = Some(format!("Failed to compile Realm '{}': {}", realm_name, e));
                                        is_fixing_broken_realm = true;
                                        broken_realm_yaml = config_str;
                                        broken_realm_name = realm_name;
                                    }
                                }
                            }
                            Err(e) => {
                                initial_error_msg = Some(format!("Failed to parse realm2.yml for Realm '{}': {}", realm_name, e));
                                is_fixing_broken_realm = true;
                                broken_realm_yaml = config_str;
                                broken_realm_name = realm_name;
                            }
                        }
                    }
                    Err(e) => {
                        initial_error_msg = Some(format!("Failed to read realm2.yml for Realm '{}': {}", realm_name, e));
                    }
                }
            }
        } else if let Some(parent) = folio.parent() {
            // Legacy: a Folio inside a project's `.inforno` directory
            if parent.file_name().and_then(|n| n.to_str()) == Some(".inforno") {
                if let Some(root) = parent.parent() {
                    project_root = Some(root.to_path_buf());
                }
            }
        }

        // Did main.rs ask for a Realm whose default Folio doesn't resolve
        // to anything yet? If so, we're sitting in the home Folio for now
        // and need to prompt rather than silently create one.
        let realm_awaiting_folio = permanent.realm_awaiting_folio.lock().unwrap().take();
        let show_realm_folio_init = realm_awaiting_folio.is_some();
        let folio_name_default = realm_awaiting_folio.clone().unwrap_or_default();

        // 2. Load Initial Data (using the valid 'conn')
        load_model_presets(&conn, &mut presets);

        chats = fetch_chat_titles(&conn).unwrap_or_else(|e| {
            eprintln!("CRITICAL: Could not fetch chat titles: {}", e);
            std::process::exit(1);
        });

        let mut open_chats: HashMap<i64, common::Chat> = HashMap::new();
        let active_chat_id: Option<i64>;
        if let Some(first_chat_info) = chats.first() {
            let chat = fetch_chat(&conn, first_chat_info.id, &presets)
                .unwrap_or_else(|e| {
                    eprintln!("CRITICAL: Could not fetch initial chat: {}", e);
                    std::process::exit(1);
                });
            open_chats.insert(first_chat_info.id, chat);
            active_chat_id = Some(first_chat_info.id);
        } else {
            // FIX: Ensure a blank slate properly initializes the chat state
            // so the bottom panel doesn't start locked out.
            open_chats.insert(0, common::Chat::default());
            active_chat_id = Some(0);
        }

        // --- 2. API Key: saved in vaultorno, or entered for this session only ---
        let mut api_key = ApiKey::default();
        let mut key_saved = false;
        let mut vault_error = None;
        let mut key_storage = crate::key_manager::KeyStorage::default();

        match crate::key_manager::load_openrouter_key(&permanent.keys) {
            Ok((key, storage)) => {
                key_storage = storage;
                if let Some(key) = key {
                    api_key = ApiKey::new(key.key, key.hosts);
                    key_saved = true;
                }
            }
            Err(error) => {
                eprintln!("Saved API keys aren't available: {error}");
                vault_error = Some(error);
            }
        }

        // Once per run, not on every Folio reload: what startup found out
        // about the vault, and whether to ask for keys (vaultorno design doc
        // §15): none saved, or about to expire.
        let mut vault_notices = Vec::new();
        let mut ask_for_keys = false;
        if let Some(notices) = permanent.vault_notices.lock().unwrap().take() {
            ask_for_keys = !key_saved
                || inforno_core::api_keys::expires_within(
                    key_storage.expires,
                    inforno_core::api_keys::RENEW_DAYS,
                )
                || !notices.is_empty();
            vault_notices = notices;
        }

        // --- 3. Cache Database and Async Model Fetching ---
        let mut cache_conn: Option<rusqlite::Connection> = None;

        match get_cache_db_conn() {
            Ok(mut value) => {
                println!("Cache Local Database connection established");

                /*
                permanent.rt.block_on(async {
                    // Task A: Fetch OpenRouter Models if table is empty
                    if let Ok(is_empty) = is_table_empty(&value, "openr_model") {
                         if is_empty && api_key.is_set {
                            println!("  ---->   Openr_model table is empty. Fetching...");
                            if let Ok(openr_models) = openr_fetch_models(&api_key).await {
                                println!("Fetched {} Openrouter models", openr_models.len());
                                match populate_openr_model(&mut value, &openr_models) {
                                    Ok(_) => println!("... success!"),
                                    Err(error) => println!("Error: {}", error),
                                }
                            }
                        }
                    }

                    // Task B: Fetch Ollama Installed Models
                    if let Ok(ollama_models) = ollama_fetch_models().await {
                        println!("Fetched {} Ollama models", ollama_models.len());
                        match populate_ollama_installed(&mut value, &ollama_models) {
                            Ok(_) => println!("... success!"),
                            Err(error) => println!("Error: {}", error),
                        }
                        ollama_model_names_installed = ollama_models.into_iter()
                            .map(|m| m.name).collect();
                    } else {
                        // Fallback to cache if live fetch fails
                        if let Ok(names) = get_ollama_model_installed(&mut value) {
                            ollama_model_names_installed = names;
                        }
                    }
                });
                */

                // Task C: Retrieve names from Cache DB (Sync operations)
                if let Ok(names) = get_openr_model_names(&mut value) {
                    openr_model_names = names;
                }

                if let Ok(names) = get_ollama_model_names(&mut value) {
                    ollama_model_names = names;
                    // Merge installed models into available models list
                    ollama_model_names.extend(ollama_model_names_installed.iter().cloned());
                    ollama_model_names.sort();
                    ollama_model_names.dedup();
                }

                cache_conn = Some(value);
            },
            Err(error) => {
                eprintln!("Error establishing Cache Local Database connection: {}", error);
            },
        }

        // create the communication channel for streaming chat messages
        let (chat_tx, chat_rx) = channel();

        // attempt to load the layout from the .ron file
        let mut pane_tree = load_layout_from_ron(&project_root);

        if pane_tree.is_empty() {
            let mut tiles = egui_tiles::Tiles::default();
            let chat_pane = tiles.insert_pane(crate::panes::Pane::Chat {
                chat_id: active_chat_id.unwrap_or(0)
            });

            // FIX 1: Wrap the initial pane in a Tabs container!
            let tabs_container = tiles.insert_tab_tile(vec![chat_pane]);
            pane_tree = egui_tiles::Tree::new("main_pane_tree", tabs_container, tiles);
        }

        // Find the first available pane to act as our active tile on startup
        let mut initial_active_tile = None;
        if pane_tree.root.is_some() {
            // Step A: Just grab the ID we need (Immutable borrow)
            for (id, tile) in pane_tree.tiles.iter() {
                if matches!(tile, egui_tiles::Tile::Pane(_)) {
                    initial_active_tile = Some(*id);
                    break;
                }
            }
        }

        // Step B: Now that the loop is over, we can safely mutate the tree!
        if let Some(target_id) = initial_active_tile {
            pane_tree.make_active(|tid, _| tid == target_id);
        }

        // --- 4. Construct State ---
        Self {
            perma: permanent,
            tile_labels: std::collections::HashMap::new(),
            active_tile_id: initial_active_tile,
            chat_locations: std::collections::HashMap::new(),
            open_chats,
            active_chat_id,
            chat_msg_ui: HashMap::new(),
            chat_to_rename: None,
            chat_rename_buffer: String::new(),
            common_mark_cache: CommonMarkCache::default(),
            presets,
            cache_conn,
            db_conn: conn,
            db_chats: chats,
            show_key_manager: ask_for_keys,
            show_preset_editor: false,
            show_realm_config: false,
            show_workflow_editor: false,
            workflow_editor_state: WorkflowEditorState::default(),
            realm_config_state: RealmConfigState {
                yaml_buffer: broken_realm_yaml.clone(),
                original_yaml: broken_realm_yaml,
                is_fixing_broken_realm,
                realm_name: if is_fixing_broken_realm { Some(broken_realm_name) } else { None },
                ..Default::default()
            },
            api_key_entered: String::new(),
            openrouter_api_key: api_key,
            key_saved,
            vault_error,
            key_storage,
            vault_notices,
            preset_editor_state: ModelPresetEditorState {
                ollama_downloading: Arc::new(
                Mutex::new(OllamaDownloading::default())),
                ..Default::default()
            },
            openr_model_names,
            ollama_model_names,
            ollama_model_names_installed,
            op_tx,
            pending_file_dialog_op: None,
            pending_export_content: None,
            is_in_home_folio: is_home,
            folio,
            chat_streaming_state: ChatStreamingState {
                streaming: false,
                bitmask: 0,
                msg_ids: vec![],
                content_buffers: vec![],
                reasoning_buffers: vec![],
                abort_flag: None,
                rx: chat_rx,
                tx: chat_tx,
            },
            error_msg: initial_error_msg.clone(), // if there is an error, modal will auto open
            is_modal_open: initial_error_msg.is_some() || show_realm_folio_init,
            bottom_panel_state: BottomPanelState::default(),
            talker_config_state: TalkerConfigState::default(),
            math_cache: std::rc::Rc::new(std::cell::RefCell::new(std::collections::HashMap::new())),
            copy_presets_checked: true,
            show_realm_folio_init_modal: show_realm_folio_init,
            realm_awaiting_folio,
            folio_name_buffer: folio_name_default,
            project_root,
            active_realm,
            active_workspace_name,
            merge_apps: std::collections::HashMap::new(),
            editor_apps: std::collections::HashMap::new(),
            file_dialog: egui_file_dialog::FileDialog::new(),
            pane_tree,
            search_query: String::new(),
            chat_widths: std::collections::HashMap::new(),
            chat_highlights: std::collections::HashMap::new(),
            realm_legacy_keys,
        }
    }

    /// Re-initializes the state in place.
    /// Useful if you want to refresh DB connections or reload API keys
    /// without restarting the application.
    pub fn reload(&mut self, folio: Option<PathBuf>) {
        *self = Self::new(self.perma.clone(), folio, self.op_tx.clone());
    }
}

pub struct MyApp {
    ctx: egui::Context,
    perma: Arc<MyAppPermanent>,
    state: State,
    op_rx: Receiver<FileOpMsg>,
}

impl MyApp {
    pub fn new(cc: &eframe::CreationContext<'_>,
            permanent: MyAppPermanent) -> Self {
        egui_extras::install_image_loaders(&cc.egui_ctx);
        let (tx, rx) = channel();
        let folio = permanent.folio.clone();
        let perma_arc = Arc::new(permanent);
        let state_perma = perma_arc.clone();

        Self {
            ctx: cc.egui_ctx.clone(),
            perma: perma_arc,
            state: State::new(state_perma, folio, tx),
            op_rx: rx,
        }
    }
}

impl eframe::App for MyApp {
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, "app_language", &self.perma.app_language);
        eframe::set_value(storage, "zoom", &self.ctx.zoom_factor());
        let theme = if self.ctx.theme() == egui::Theme::Light { "light" } else { "dark" };
        eframe::set_value(storage, "theme", &theme.to_string());
        /*
        eprintln!(
            "WINPROBE native_ppp={:?} zoom={} ppp={} inner_rect={:?} monitor={:?}",
            self.ctx.input(|i| i.viewport().native_pixels_per_point),
            self.ctx.zoom_factor(),
            self.ctx.pixels_per_point(),
            self.ctx.input(|i| i.viewport().inner_rect),
            self.ctx.input(|i| i.viewport().monitor_size),
        );
        {
            // points -> logical pixels
            let size = self.ctx.content_rect().size() * self.ctx.zoom_factor();
            match crate::window_size_path() {
                Some(path) => {
                    if let Some(dir) = path.parent() {
                        let _ = std::fs::create_dir_all(dir);
                    }
                    match std::fs::write(&path, format!("{} {}", size.x, size.y)) {
                        Ok(()) => eprintln!("WINSIZE saved {:?} to {:?}", size, path),
                        Err(e) => eprintln!("WINSIZE write failed: {e}"),
                    }
                }
                None => eprintln!("WINSIZE no config dir"),
            }
        }
        */

        // Wipe the stale egui Memory blob left by earlier versions
        // (scroll offsets, cursor states, ~7.5 MB).
        storage.set_string("egui", String::new());
    }

    // Widget state (scroll, cursors, collapsing headers) is not worth keeping.
    fn persist_egui_memory(&self) -> bool {
        false
    }

    #[tracing::instrument(skip_all, name = "MyApp::ui")]
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();

        // Testing aid: FRAMESTATS=1 prints, once a second, the frame rate and the
        // CPU time eframe measured per frame (UI code and tessellation, not GPU work)
        if std::env::var_os("FRAMESTATS").is_some() {
            let id = egui::Id::new("frame_stats");
            let now = ctx.input(|i| i.time);
            let (start, frames, cpu) = ctx.data(|d| d.get_temp::<(f64, u32, f32)>(id)).unwrap_or((now, 0, 0.0));
            let (frames, cpu) = (frames + 1, cpu + frame.info().cpu_usage.unwrap_or(0.0));
            if now - start >= 1.0 {
                eprintln!(
                    "FRAMESTATS {:.0} frames/s, {:.1} ms CPU per frame, {:.0}% of a core",
                    frames as f64 / (now - start),
                    cpu * 1000.0 / frames as f32,
                    cpu as f64 / (now - start) * 100.0,
                );
                ctx.data_mut(|d| d.insert_temp(id, (now, 0u32, 0.0f32)));
            } else {
                ctx.data_mut(|d| d.insert_temp(id, (start, frames, cpu)));
            }
        }

        // Draw at most ~60 frames a second. egui redraws on every input event,
        // and presenting a frame (wgpu/Vulkan) costs several ms of CPU, so a
        // fast mouse alone kept a core busy. Events arriving while we wait are
        // handled together in the next frame. Only the first pass of a frame
        // waits: a second pass (requested for layout) belongs to the same frame.
        if ctx.current_pass_index() == 0 {
            const MIN_FRAME: std::time::Duration = std::time::Duration::from_micros(16_667);
            let id = egui::Id::new("frame_cap_last");
            if let Some(last) = ctx.data(|d| d.get_temp::<std::time::Instant>(id)) {
                let elapsed = last.elapsed();
                if elapsed < MIN_FRAME {
                    std::thread::sleep(MIN_FRAME - elapsed);
                }
            }
            ctx.data_mut(|d| d.insert_temp(id, std::time::Instant::now()));
        }
        /*
        // TEMP probe: window size history in logical pixels (points * zoom).
        {
            let size = ctx.content_rect().size() * ctx.zoom_factor();
            let id = egui::Id::new("win_size_probe");
            let last: Option<egui::Vec2> = ctx.data(|d| d.get_temp(id));
            if last != Some(size) {
                ctx.data_mut(|d| d.insert_temp(id, size));
                eprintln!(
                    "WINCHG t={:.2}s logical={:?} zoom={}",
                    ctx.input(|i| i.time), size, ctx.zoom_factor()
                );
            }
        }

        // TEMP probe: how big is egui's persisted Memory?
        {
            let t = ui.input(|i| i.time);
            let last = ctx.data(|d| d.get_temp::<f64>(egui::Id::new("mem_probe_t")).unwrap_or(0.0));
            if t - last > 5.0 {
                ctx.data_mut(|d| d.insert_temp(egui::Id::new("mem_probe_t"), t));
                if let Ok(s) = ctx.memory(|m| ron::to_string(m)) {
                    eprintln!("MEMPROBE egui_memory_ron={} KB", s.len() / 1024);
                    let _ = std::fs::write("/tmp/egui_mem.ron", s);
                }
            }
        }
        */
        // --- SYNC GLOBAL COLORS ---
        {
            let visuals = ui.visuals().clone();
            let mut colors = THEME_COLORS.write().unwrap();
            colors.cloud = visuals.hyperlink_color;
            colors.local = visuals.strong_text_color();
            colors.text = visuals.text_color();
            colors.strong = visuals.strong_text_color();
            colors.err = visuals.error_fg_color;
        }

        let state = &mut self.state;

        // Generate our Tile Map ---
        let (labels, locs) = crate::panes::compute_tile_locations(&state.pane_tree);
        state.tile_labels = labels;
        state.chat_locations = locs;

        if state.error_msg.is_some() {
            state.is_modal_open = true;
        }

        // when user selects a file in the Open Folio dialog, we reload State
        if let Ok(file_op_msg) = self.op_rx.try_recv() {
            match file_op_msg.op {
                FileOp::Open => {
                    // "open window dialog" closed
                    // if user didn't cancel, we should get path of file to open
                    if !file_op_msg.cancelled {
                        // perma.folio keeps the Folio the app started with
                        state.reload(file_op_msg.path);
                    }
                },
                FileOp::SaveAs => {
                    // "Save As window dialog" closed
                    if !file_op_msg.cancelled {
                        // 1. Replace the current connection with a dummy Memory DB.
                        //    This gives us ownership of 'old_conn' and keeps 'state' valid.
                        let old_conn = std::mem::replace(
                            &mut state.db_conn,
                            rusqlite::Connection::open_in_memory().unwrap()
                        );
                        // 2. Explicitly close it
                        match old_conn.close() {
                            Ok(_) => {
                                println!("Folio closed. Proceeding to move file...");

                                // 3. Now it is safe to move the file
                                let old_path = &state.folio;
                                if let Some(new_path) = &file_op_msg.path {
                                    match std::fs::rename(old_path, new_path) {
                                        Ok(_) => {
                                            println!("File moved successfully!");
                                            state.reload(file_op_msg.path);
                                        },
                                        Err(e) => {
                                            eprintln!("Failed to move file: {}", e);
                                        },
                                    }
                                }
                            },
                            Err((restored_conn, err)) => {
                                eprintln!("Could not close Folio: {}. Aborting move.", err);
                                // Put the connection back into state so the app doesn't crash if used again
                                state.db_conn = restored_conn;
                            }
                        }
                    }
                },
                FileOp::SaveCopy => {
                    // "Save Copy window dialog" closed. Same as SaveAs, but we
                    // stay in the same old Folio
                    if !file_op_msg.cancelled {
                        let old_path = &state.folio;
                        if let Some(new_path) = &file_op_msg.path {
                            match std::fs::copy(old_path, new_path) {
                                Ok(_) => {
                                    println!("File copied successfully!");
                                },
                                Err(e) => {
                                    state.error_msg = Some(
                                        format!("Failed to copy file: {}", e));
                                },
                            }
                        }
                    }
                },
                FileOp::Clear => {
                    if !file_op_msg.cancelled {
                        state.reload(Some(state.folio.clone()));
                    }
                }
                FileOp::Attach => {
                    if !file_op_msg.cancelled {
                        if let Some(mut atts) = file_op_msg.attachments {
                            state.bottom_panel_state.pending_attachments.append(&mut atts);
                        }
                    }
                }
                FileOp::OpenEditor | FileOp::OpenEditorRight => {
                    if !file_op_msg.cancelled {
                        if let Some(path) = file_op_msg.path {
                            match std::fs::read_to_string(&path) {
                                Ok(content) => {
                                    // Route to the correct placement function
                                    if matches!(file_op_msg.op, FileOp::OpenEditorRight) {
                                        crate::panes::open_editor_in_right_pane(state, path, content);
                                    } else {
                                        crate::panes::open_editor_in_tab(state, path, content);
                                    }
                                }
                                Err(e) => {
                                    state.error_msg = Some(format!("Could not read file: {}", e));
                                    state.is_modal_open = true;
                                }
                            }
                        }
                    }
                }
                FileOp::OpenMerge | FileOp::OpenMergeRight => {
                    if !file_op_msg.cancelled {
                        if let (Some(path), Some(left), Some(right)) = (file_op_msg.path, file_op_msg.left_content, file_op_msg.right_content) {
                            if matches!(file_op_msg.op, FileOp::OpenMergeRight) {
                                crate::panes::open_merge_in_right_pane(state, path, left, right);
                            } else {
                                crate::panes::open_merge_in_tab(state, path, left, right);
                            }
                        }
                    }
                }
                FileOp::ExportChat | FileOp::ExportNotebook => {
                    // The actual file writing is handled immediately when the file
                    // is picked via state.file_dialog.take_picked().
                    // This arm simply satisfies Rust's exhaustive match requirement.
                }
            }
        }

        while let Ok(event) = state.chat_streaming_state.rx.try_recv() {
            let active_chat_id = state.active_chat_id.unwrap_or(0);

            match event {
                ChatStreamEvent::Content(ind, text) => {
                    if let Some(buf) = state.chat_streaming_state
                                .content_buffers.get_mut(ind) {
                        buf.push_str(&text);
                        // update the message in the chat.msg_pool
                        let msg_id = state.chat_streaming_state.msg_ids[ind];
                        if let Some(chat) = state.open_chats.get_mut(&active_chat_id) {
                            if let Some(msg) = chat.msg_pool.get_mut(&msg_id) {
                                msg.content = buf.clone();
                            }
                        }
                    }
                }
                ChatStreamEvent::Reasoning(ind, text) => {
                    if let Some(buf) = state.chat_streaming_state
                                .reasoning_buffers.get_mut(ind) {
                        buf.push_str(&text);
                        // update the message in the chat.msg_pool
                        let msg_id = state.chat_streaming_state.msg_ids[ind];
                        if let Some(chat) = state.open_chats.get_mut(&active_chat_id) {
                            if let Some(msg) = chat.msg_pool.get_mut(&msg_id) {
                                msg.reasoning = Some(buf.clone());
                            }
                        }
                    }
                }
                ChatStreamEvent::Finished(ind) => {
                    // turn off the bit for this talker
                    state.chat_streaming_state.bitmask &= !(1 << ind as u128);
                    // persist the result to db
                    let content = state.chat_streaming_state
                            .content_buffers[ind].clone();
                    let reasoning = state.chat_streaming_state
                            .reasoning_buffers[ind].clone();

                    // save the message content and reasoning to the database
                    let _ = mod_msg_content_reasoning(
                            &state.db_conn,
                            state.chat_streaming_state.msg_ids[ind],
                            &content, &reasoning);

                    // check if all talkers are done
                    if state.chat_streaming_state.bitmask == 0 {
                        state.chat_streaming_state.streaming = false;
                        println!("Streaming finished");
                    }
                }
                ChatStreamEvent::Error(ind, err) => {
                    // Check for minor serialization error first
                    if err.starts_with("Serialization error") {
                        eprintln!("Talker {} Ignored Error: {}", ind, err);
                        // Do nothing else here - this keeps the talker "streaming"
                        // and does not clear the bitmask.
                    } else {
                        // Handle actual fatal errors
                        eprintln!("Talker {} Error: {}", ind, err);

                        if let Some(buf) = state.chat_streaming_state
                                    .content_buffers.get_mut(ind) {
                            buf.push_str(&format!(
                            "**Error! Is the Server Running? Details: {}**\n", err));
                            // update the message in the chat.msg_pool
                            let msg_id = state.chat_streaming_state.msg_ids[ind];
                            if let Some(chat) = state.open_chats.get_mut(&active_chat_id) {
                                if let Some(msg) = chat.msg_pool.get_mut(&msg_id) {
                                    msg.content = buf.clone();
                                }
                            }
                        }

                        state.chat_streaming_state.bitmask &= !(1 << ind as u128);
                        println!("Stream {} finished with error", ind);
                        if state.chat_streaming_state.bitmask == 0 {
                            state.chat_streaming_state.streaming = false;
                        }
                    }
                }
            }
        }

        ui_top_panel(ui, state);

        crate::top_panel::ui_realm_legacy_banner(ui, state);

        ui_side_panel(ui, state);

        ui_key_manager(ui, state);

        ui_model_preset_editor(ui, state);

        ui_talker_config(ui, state);

        ui_realm_config(ui, state);

        ui_workflow_editor(ui, state);

        ui_bottom_panel(ui, state);

        ui_chat(ui, state);

        // File Dialog Start
        state.file_dialog.update(&ctx);

        // Handle single file selection (Open / Save) ---
        if let Some(path) = state.file_dialog.take_picked() {
            if let Some(op) = state.pending_file_dialog_op.take() {
                // If it's an export, write it directly and skip the channel
                if matches!(op, inforno_core::common::FileOp::ExportChat | inforno_core::common::FileOp::ExportNotebook) {
                    if let Some(content) = state.pending_export_content.take() {
                        let _ = std::fs::write(&path, content);
                    }
                } else {
                    let _ = state.op_tx.send(inforno_core::common::FileOpMsg {
                        op,
                        cancelled: false,
                        path: Some(path.to_path_buf()),
                        attachments: None,
                        left_content: None,
                        right_content: None,
                    });
                }
                ctx.request_repaint();
            }
        }

        // Check if the user confirmed their selection
        if let Some(paths) = state.file_dialog.take_picked_multiple() {
            let tx_clone = state.op_tx.clone();
            let ctx_clone = ctx.clone();
            let project_root_clone = state.project_root.clone();
            let active_realm_clone = state.active_realm.clone();

            tokio::spawn(async move {
                let mut attachments = Vec::new();

                // Helper to check if a file is an image
                let get_image_mime = |ext: &str| -> Option<String> {
                    match ext.to_lowercase().as_str() {
                        "png" => Some("image/png".to_string()),
                        "jpg" | "jpeg" => Some("image/jpeg".to_string()),
                        "webp" => Some("image/webp".to_string()),
                        "gif" => Some("image/gif".to_string()),
                        _ => None,
                    }
                };

                for path in paths {
                    if path.is_dir() {
                        // Recursively read directory (we can leave this for text only as before,
                        // or you could expand it to images. Let's keep it safe and just do text for folders)
                        fn read_dir_recursive(dir: &std::path::Path, out: &mut Vec<inforno_core::common::Attachment>, realm: &Option<inforno_core::realm::ActiveRealm>, proj_root: &Option<std::path::PathBuf>) {
                            if let Ok(entries) = std::fs::read_dir(dir) {
                                for entry in entries.flatten() {
                                    let p = entry.path();
                                    if p.is_dir() {
                                        read_dir_recursive(&p, out, realm, proj_root);
                                    } else {
                                        if let Ok(content) = std::fs::read_to_string(&p) {
                                            let relative_path = inforno_core::realm::get_relative_path(realm, proj_root, &p);
                                            out.push(inforno_core::common::Attachment {
                                                filename: relative_path,
                                                mime_type: "text/plain".to_string(),
                                                content,
                                            });
                                        }
                                    }
                                }
                            }
                        }
                        read_dir_recursive(&path, &mut attachments, &active_realm_clone, &project_root_clone);
                    } else {
                        // Read single file: Check if it's an image!
                        let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("");

                        // 1. Try to get the relative path robustly
                        let relative_name = inforno_core::realm::get_relative_path(&active_realm_clone, &project_root_clone, &path);

                        if let Some(mime) = get_image_mime(ext) {
                            // It's an image, read as binary and base64 encode
                            if let Ok(bytes) = std::fs::read(&path) {
                                let base64_str = STANDARD.encode(&bytes);
                                attachments.push(inforno_core::common::Attachment {
                                    filename: relative_name.clone(),
                                    mime_type: mime,
                                    content: base64_str,
                                });
                            }
                        } else {
                            // Read as standard text
                            if let Ok(content) = std::fs::read_to_string(&path) {
                                attachments.push(inforno_core::common::Attachment {
                                    filename: relative_name,
                                    mime_type: "text/plain".to_string(),
                                    content,
                                });
                            }
                        }
                    }
                }

                // Send the payload back to the UI thread
                let _ = tx_clone.send(inforno_core::common::FileOpMsg {
                    op: inforno_core::common::FileOp::Attach,
                    cancelled: false,
                    path: None,
                    attachments: Some(attachments),
                    left_content: None,
                    right_content: None,
                });

                ctx_clone.request_repaint();
            });
        }
        // File Dialog End

        // 3. Draw the Modal (Foreground)
        if let Some(msg) = &state.error_msg {
            // We clone the message to avoid borrowing issues inside the closure
            let msg_text = msg.clone();
            let mut open = true;

            egui::Window::new("Error")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0]) // Center on screen
                .open(&mut open) // Helper to handle the "X" close button
                .show(ui, |ui| {
                    ui.set_min_width(300.0); // Make it look substantial

                    ui.vertical_centered(|ui| {
                        ui.add_space(10.0);
                        ui.label(&msg_text);
                        ui.add_space(20.0);

                        ui.horizontal(|ui| {
                            if ui.button("OK").clicked() {
                                // Close logic
                                state.error_msg = None;
                                state.is_modal_open = false;
                            }

                            if state.realm_config_state.is_fixing_broken_realm {
                                if ui.button("🔧 Fix Configuration").clicked() {
                                    state.error_msg = None;
                                    state.is_modal_open = false;
                                    state.show_realm_config = true; // Automatically open the editor
                                }
                            }
                        });
                    });
                });

            // Handle the "X" button on the window frame
            if !open {
                state.error_msg = None;
                state.is_modal_open = false;
            }
        }

        // Realm Folio Initialization Modal. Only ever shown, never triggered
        // automatically: a Realm with no resolvable `folios:` entry is
        // *asked* about, not repaired for you. Creating the Folio doesn't
        // edit the Realm either: the entry that links it is shown here, to
        // be added to realm2.yml by hand.
        if state.show_realm_folio_init_modal {
            let mut open = true;

            egui::Window::new("Create Folio for Realm")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .open(&mut open)
                .show(ui, |ui| {
                    let realm_label = state.realm_awaiting_folio.clone().unwrap_or_default();
                    ui.label(format!("Realm '{}' links to no Folio yet.", realm_label));
                    let folios_dir = inforno_core::db::folios_dir().unwrap_or_default();
                    ui.label(format!("Create one in {}:", folios_dir.display()));

                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        ui.label("Folio name:");
                        ui.text_edit_singleline(&mut state.folio_name_buffer);
                    });
                    ui.add_space(10.0);
                    ui.checkbox(&mut state.copy_presets_checked, "Copy Presets from Home Folio");
                    ui.add_space(10.0);

                    let folio_path = inforno_core::db::folio_path_for(&state.folio_name_buffer);
                    match &folio_path {
                        Ok(path) => {
                            let entry = format!(
                                "folios:\n  default:\n    path: {}\n    duties: [{}]",
                                path.display(),
                                crate::chat::GUI_DUTY
                            );
                            ui.label("To make it this Realm's default Folio, add this to realm2.yml by hand:");
                            ui.horizontal(|ui| {
                                ui.code(entry.as_str());
                                if ui.button("🗐").on_hover_text("Copy to clipboard").clicked() {
                                    ui.ctx().copy_text(entry.clone());
                                }
                            });
                        }
                        Err(e) => {
                            ui.colored_label(ui.visuals().error_fg_color, e.to_string());
                        }
                    }
                    ui.add_space(10.0);

                    ui.horizontal(|ui| {
                        if ui.add_enabled(folio_path.is_ok(), egui::Button::new("Create Folio")).clicked() {
                            if let Ok(path) = &folio_path {
                                match inforno_core::db::init_folio(path, state.copy_presets_checked) {
                                    Ok(()) => {
                                        // The user just made this Folio to open the
                                        // Realm they asked for — re-enter it on reload
                                        // instead of landing back in a bare, realm-less
                                        // Folio. `perma` is the same Arc `reload` hands
                                        // to the fresh `State::new`, so this write is
                                        // visible to it.
                                        if let Some(realm_name) = state.realm_awaiting_folio.clone() {
                                            *state.perma.active_realm_name.lock().unwrap() = Some(realm_name);
                                        }
                                        state.reload(Some(path.clone()));
                                    }
                                    Err(e) => {
                                        state.error_msg = Some(format!("Failed to create Folio: {}", e));
                                        state.is_modal_open = true;
                                    }
                                }
                            }
                            state.show_realm_folio_init_modal = false;
                        }

                        if ui.button("Cancel").clicked() {
                            // Stay in the home Folio; don't enter the Realm.
                            state.show_realm_folio_init_modal = false;
                        }
                    });
                });

            if !open {
                state.show_realm_folio_init_modal = false;
            }
        }
    }
}

pub fn reload_db_chats(conn: &Connection, db_chats: &mut Vec<DbChat>) {
    let titles = inforno_core::db::fetch_chat_titles(conn).unwrap_or_else(|e| {
        eprintln!("Error: {}", e);
        vec![]
    });
    *db_chats = titles;
}

#[macro_export]
macro_rules! mybtn {
    // Matches: mybtn!(ui, "label_key", "tooltip_key")
    ($ui:expr, $key:literal) => {
        $ui.button(rust_i18n::t!($key))
            .on_hover_text(
                ::egui::RichText::new(
                    rust_i18n::t!(concat!($key, "_tooltip"))
                )
                .strong()
                .heading()
            )
            .clicked()
    };
}

// Helper to construct the path to the layout.ron file
fn get_layout_path(project_root: &Option<PathBuf>) -> Option<PathBuf> {
    project_root.as_ref().map(|root| root.join(".inforno").join("layout.ron"))
}

// Loads the layout from disk, returning an empty tree if it fails
fn load_layout_from_ron(project_root: &Option<PathBuf>) -> egui_tiles::Tree<crate::panes::Pane> {
    if let Some(path) = get_layout_path(project_root) {
        if let Ok(file_content) = std::fs::read_to_string(path) {
            if let Ok(tree) = ron::from_str(&file_content) {
                return tree;
            }
        }
    }
    egui_tiles::Tree::empty("main_pane_tree")
}

// Call this function whenever you want to save the current window layout
// TODO: not wired up yet — `load_layout_from_ron` reads layout.ron on startup,
// but nothing ever writes it, so pane layouts don't currently persist.
#[allow(dead_code)]
pub fn save_layout_to_ron(state: &State) {
    if let Some(path) = get_layout_path(&state.project_root) {
        if let Ok(ron_string) = ron::ser::to_string_pretty(&state.pane_tree, ron::ser::PrettyConfig::default()) {
            let _ = std::fs::write(path, ron_string);
        }
    }
}
