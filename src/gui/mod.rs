use std::collections::HashMap;
use std::env;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::sync::mpsc::{Receiver, Sender, channel};
use dotenv::dotenv;
use eframe::egui::{self};
use egui_async::{Bind, EguiAsyncPlugin};
use egui_commonmark::CommonMarkCache;
use keyring::Entry;
use rusqlite::Connection;
use tokio::runtime::{Handle, Runtime};
use crate::common::{self, ApiKey, ChatMsg, ChatMsgUi, ChatResp, ChatStreamEvent, DbChat, FileOp, FileOpMsg, KEYRING_INFO, MsgRole, OllamaDownloading, Preset, PresetSelection, Presets, THEME_COLORS, load_presets};
use crate::db::{fetch_chat, fetch_chat_titles, get_sandbox_db_conn, is_table_empty, mk_msg, mod_agent_msgs, mod_msg_content_reasoning};
use crate::db::cache::{get_cache_db_conn, get_ollama_model_installed, get_ollama_model_names, get_openr_model_names, populate_ollama_installed, populate_openr_model};
use crate::gui::agent_config::{AgentConfigState, ui_agent_config};
use crate::gui::bottom_panel::{BottomPanelState, ui_bottom_panel};
use crate::gui::chat::ui_chat;
use crate::gui::key_manager::ui_key_manager;
use crate::gui::preset_editor::{PresetEditorState, ui_preset_editor};
use crate::gui::side_panel::ui_side_panel;
use crate::gui::top_panel::ui_top_panel;
use crate::ollama::ollama_fetch_models;
use crate::openr::openr_fetch_models;

mod top_panel;
mod side_panel;
mod preset_editor;
mod key_manager;
mod bottom_panel;
mod chat;
mod agent_config;

pub struct MyAppPermanent {
    pub rt: Handle,
    pub sandbox: Option<PathBuf>,
    pub app_language: Mutex<String>,
}

pub struct ChatStreamingState {
    pub streaming: bool,
    pub bitmask: u128, // each bit flags which agent is streaming
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
    perma: Arc<MyAppPermanent>,
    chat: common::Chat,
    chat_msg_ui: HashMap<i64, ChatMsgUi>,
    chat_to_rename: Option<i64>,
    chat_rename_buffer: String,
    common_mark_cache: CommonMarkCache,
    presets: Presets,
    cache_conn: Option<rusqlite::Connection>, // connection to cache db
    db_conn: rusqlite::Connection, // connection to main db
    db_chats: Vec<DbChat>, // chat titles fetched from the main db
    show_key_manager: bool,
    show_preset_editor: bool,
    api_key_entered: String,
    openrouter_api_key: ApiKey,
    keyring_used: bool,
    preset_editor_state: PresetEditorState,
    openr_model_names: Vec<String>,
    ollama_model_names: Vec<String>,
    ollama_model_names_installed: Vec<String>,
    op_tx: Sender<FileOpMsg>,
    open_sandbox_showing: bool,
    is_in_home_sandbox: bool,
    sandbox: PathBuf,
    // when receiving reply from the LLM's we need to store them during
    // streaming. Each buffer is indexed by the Agent index.
    chat_streaming_state: ChatStreamingState,
    // error modal's content:
    error_msg: Option<String>,
    is_modal_open: bool,
    bottom_panel_state: BottomPanelState,
    agent_config_state: AgentConfigState,
}

impl State {
    /// This function handles the heavy lifting of initialization.
    /// It can be called by `Default` or manually to reload the state.
    pub fn new(
        permanent: Arc<MyAppPermanent>,
        new_sandbox: Option<PathBuf>,
        op_tx: Sender<FileOpMsg>
    ) -> Self {
        let mut updated_sandbox = new_sandbox;
        let mut is_home = true;
        if let Some(_) = updated_sandbox {
            is_home = false;
        }
        let mut openr_model_names: Vec<String> = vec![];
        let mut ollama_model_names: Vec<String> = vec![];
        let mut ollama_model_names_installed: Vec<String> = vec![];

        // --- 1. Main Database Connection ---

        let mut chats: Vec<DbChat> = vec![];
        let mut chat = common::Chat::default();
        let mut presets = Presets::default();

        // 1. Establish Connection or Die
        let (conn, sandbox) = match get_sandbox_db_conn(&updated_sandbox) {
            Ok(tuple) => tuple,
            Err(error) => {
                eprintln!("Error opening the default Sandbox: {}", error);
                std::process::exit(1); // Quit immediately
            }
        };

        // 2. Load Initial Data (using the valid 'conn')
        load_presets(&conn, &mut presets);

        chats = fetch_chat_titles(&conn).unwrap_or_else(|e| {
            eprintln!("CRITICAL: Could not fetch chat titles: {}", e);
            std::process::exit(1);
        });

        if let Some(first_chat_info) = chats.first() {
            chat = fetch_chat(&conn, first_chat_info.id, &presets)
                .unwrap_or_else(|e| {
                    eprintln!("CRITICAL: Could not fetch initial chat: {}", e);
                    std::process::exit(1);
                });
        }

        // --- 2. API Key Retrieval (Env or Keyring) ---
        let mut api_key = ApiKey::default();
        let mut is_keyring_used = false;

        dotenv().ok(); // Reload env vars if they changed

        if let Ok(env_key) = env::var("OPENROUTER_API_KEY") {
            api_key.key = env_key.into();
            api_key.is_set = true;
        } else {
            if let Ok(entry) = Entry::new(KEYRING_INFO[0], KEYRING_INFO[1]) {
                match entry.get_password() {
                    Ok(retrieved) => {
                        println!("key retrieved!");
                        is_keyring_used = true;
                        api_key.key = retrieved.into();
                        api_key.is_set = true;
                    },
                    Err(error) => {
                        println!("Failed to retrieve key: {}", error);
                    },
                }
            } else {
                println!("Failed to access the system keyring to get the API key.");
            }
        }

        // --- 3. Cache Database and Async Model Fetching ---
        let mut cache_conn: Option<rusqlite::Connection> = None;

        match get_cache_db_conn() {
            Ok(mut value) => {
                println!("Cache Local Database connection established");

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

        // --- 4. Construct State ---
        Self {
            perma: permanent,
            chat: chat,
            chat_msg_ui: HashMap::new(),
            chat_to_rename: None,
            chat_rename_buffer: String::new(),
            common_mark_cache: CommonMarkCache::default(),
            presets,
            cache_conn,
            db_conn: conn,
            db_chats: chats,
            show_key_manager: false,
            show_preset_editor: false,
            api_key_entered: String::new(),
            openrouter_api_key: api_key,
            keyring_used: is_keyring_used,
            preset_editor_state: PresetEditorState {
                ollama_downloading: Arc::new(
                Mutex::new(OllamaDownloading::default())),
                ..Default::default()
            },
            openr_model_names,
            ollama_model_names,
            ollama_model_names_installed,
            op_tx,
            open_sandbox_showing: false,
            is_in_home_sandbox: is_home,
            sandbox,
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
            error_msg: None, // if there is an error, modal will auto open
            is_modal_open: false, // if file dialog is open this needs to be true
            bottom_panel_state: BottomPanelState::default(),
            agent_config_state: AgentConfigState::default(),
        }
    }

    /// Re-initializes the state in place.
    /// Useful if you want to refresh DB connections or reload API keys
    /// without restarting the application.
    pub fn reload(&mut self, sandbox: Option<PathBuf>) {
        *self = Self::new(self.perma.clone(), sandbox, self.op_tx.clone());
    }
}

pub struct MyApp {
    perma: Arc<MyAppPermanent>,
    state: State,
    op_rx: Receiver<FileOpMsg>,
}

impl MyApp {
    pub fn new(cc: &eframe::CreationContext<'_>,
            permanent: MyAppPermanent) -> Self {
        egui_extras::install_image_loaders(&cc.egui_ctx);
        let (tx, rx) = channel();
        let sandbox = permanent.sandbox.clone();
        let perma_arc = Arc::new(permanent);
        let state_perma = perma_arc.clone();

        Self {
            perma: perma_arc,
            state: State::new(state_perma, sandbox, tx),
            op_rx: rx,
        }
    }
}

impl eframe::App for MyApp {
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, "app_language", &self.perma.app_language);
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // --- SYNC GLOBAL COLORS ---
        {
            let visuals = ctx.style().visuals.clone();
            let mut colors = THEME_COLORS.write().unwrap();
            colors.cloud = visuals.hyperlink_color;
            colors.local = visuals.strong_text_color();
            colors.text = visuals.text_color();
            colors.strong = visuals.strong_text_color();
            colors.err = visuals.error_fg_color;
        }

        ctx.plugin_or_default::<EguiAsyncPlugin>();
        let state = &mut self.state;

        if state.error_msg.is_some() {
            state.is_modal_open = true;
        }

        // when user selects a file in Open Sandbox dialog, we reload State
        if let Ok(file_op_msg) = self.op_rx.try_recv() {
            match file_op_msg.op {
                FileOp::Open => {
                    // "open window dialog" closed
                    state.open_sandbox_showing = false;
                    state.is_modal_open = false;
                    // if user didn't cancel, we should get path of file to open
                    if !file_op_msg.cancelled {
                        // let self.initializer.sandbox keep the initial value
                        state.reload(file_op_msg.path);
                    }
                },
                FileOp::SaveAs => {
                    // "Save As window dialog" closed
                    state.open_sandbox_showing = false;
                    state.is_modal_open = false;
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
                                println!("Sandbox closed. Proceeding to move file...");

                                // 3. Now it is safe to move the file
                                let old_path = &state.sandbox;
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
                                eprintln!("Could not close Sandbox: {}. Aborting move.", err);
                                // Put the connection back into state so the app doesn't crash if used again
                                state.db_conn = restored_conn;
                            }
                        }
                    }
                },
                FileOp::SaveCopy => {
                    // "Save Copy window dialog" closed. Same as SaveAs, but we
                    // stay in the same old sandbox
                    state.open_sandbox_showing = false;
                    state.is_modal_open = false;
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
                                println!("Sandbox closed. Proceeding to move file...");

                                // 3. Now it is safe to copy the file
                                let old_path = &state.sandbox;
                                if let Some(new_path) = &file_op_msg.path {
                                    match std::fs::copy(old_path, new_path) {
                                        Ok(_) => {
                                            println!("File copied successfully!");
                                            state.reload(file_op_msg.path);
                                        },
                                        Err(e) => {
                                            eprintln!("Failed to copy file: {}", e);
                                        },
                                    }
                                }
                            },
                            Err((restored_conn, err)) => {
                                eprintln!("Could not close Sandbox: {}. Aborting copying.", err);
                                // Put the connection back into state so the app doesn't crash if used again
                                state.db_conn = restored_conn;
                            }
                        }
                    }
                },
                FileOp::Clear => {
                    if !file_op_msg.cancelled {
                        state.reload(Some(state.sandbox.clone()));
                    }
                }
            }
        }

        while let Ok(event) = state.chat_streaming_state.rx.try_recv() {
            match event {
                ChatStreamEvent::Content(ind, text) => {
                    println!("Received content");
                    if let Some(buf) = state.chat_streaming_state
                                .content_buffers.get_mut(ind) {
                        buf.push_str(&text);
                        // update the message in the chat.msg_pool
                        let msg_id = state.chat_streaming_state.msg_ids[ind];
                        if let Some(msg) = state.chat.msg_pool.get_mut(&msg_id) {
                            msg.content = buf.clone();
                        }
                    }
                }
                ChatStreamEvent::Reasoning(ind, text) => {
                    println!("Received reasoning");
                    if let Some(buf) = state.chat_streaming_state
                                .reasoning_buffers.get_mut(ind) {
                        buf.push_str(&text);
                        // update the message in the chat.msg_pool
                        let msg_id = state.chat_streaming_state.msg_ids[ind];
                        if let Some(msg) = state.chat.msg_pool.get_mut(&msg_id) {
                            msg.reasoning = Some(buf.clone());
                        }
                    }
                }
                ChatStreamEvent::Finished(ind) => {
                    // tur off the bit for this agent
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

                    // check if all agents are done
                    if state.chat_streaming_state.bitmask == 0 {
                        state.chat_streaming_state.streaming = false;
                        println!("Streaming finished");
                    }
                }
                ChatStreamEvent::Error(ind, err) => {
                    // Check for minor serialization error first
                    if err.starts_with("Serialization error") {
                        eprintln!("Agent {} Ignored Error: {}", ind, err);
                        // Do nothing else here - this keeps the agent "streaming"
                        // and does not clear the bitmask.
                    } else {
                        // Handle actual fatal errors
                        eprintln!("Agent {} Error: {}", ind, err);

                        if let Some(buf) = state.chat_streaming_state
                                    .content_buffers.get_mut(ind) {
                            buf.push_str(&format!(
                            "**Error! Is the Server Running? Details: {}**\n", err));
                            // update the message in the chat.msg_pool
                            let msg_id = state.chat_streaming_state.msg_ids[ind];
                            if let Some(msg) = state.chat.msg_pool.get_mut(&msg_id) {
                                msg.content = buf.clone();
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

        ui_top_panel(ctx, state);

        ui_side_panel(ctx, state);

        ui_key_manager(ctx, state);

        ui_preset_editor(ctx, state);

        ui_agent_config(ctx, state);

        ui_bottom_panel(ctx, state);

        ui_chat(ctx, state);

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
                .show(ctx, |ui| {
                    ui.set_min_width(300.0); // Make it look substantial

                    ui.vertical_centered(|ui| {
                        ui.add_space(10.0);
                        ui.label(&msg_text);
                        ui.add_space(20.0);

                        if ui.button("OK").clicked() {
                            // Close logic
                            state.error_msg = None;
                            state.is_modal_open = false;
                        }
                    });
                });

            // Handle the "X" button on the window frame
            if !open {
                state.error_msg = None;
                state.is_modal_open = false;
            }
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        // Manually exit the process with code 0 (success).
        // This prevents the OS from attempting the faulty Wayland cleanup.
        std::process::exit(0);
    }
}

pub fn reload_db_chats(conn: &Connection, db_chats: &mut Vec<DbChat>) {
    let titles = crate::db::fetch_chat_titles(conn).unwrap_or_else(|e| {
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