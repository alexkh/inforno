use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;
use std::sync::{Arc, RwLock, LazyLock};
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::Sender;
use egui::Color32;
use openrouter_rs::Message;
use openrouter_rs::types::Role;
use rusqlite::{Connection, ToSql};
use rusqlite::types::{FromSql, FromSqlError, FromSqlResult, ToSqlOutput, ValueRef};
use secrecy::SecretString;
use strum::{Display, EnumString};
use thiserror::Error;
use crate::db::{CURRENT_SANDBOX_VERSION, mk_agent};
//use crate::openr::do_openr_chat_que;
//use crate::ollama::{do_ollama_chat_que};
use ollama_rs::generation::chat::{ChatMessage, MessageRole};

pub static KEYRING_INFO: &'static [&str] = &["com.wizstaff.inforno", "openr"];

// Global static storage
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


// when streaming a chat, this structure is passed to the GUI
pub enum ChatStreamEvent {
    Content(usize, String),
    Reasoning(usize, String),
    Finished(usize),
    Error(usize, String),
}

#[derive(Default, Clone)]
pub enum FileOp {
    #[default]
    Open,
    SaveAs,
    SaveCopy,
    Clear,
}

#[derive(Default, Clone)]
pub struct FileOpMsg {
    pub op: FileOp,
    pub cancelled: bool,
    pub path: Option<PathBuf>,
}

#[derive(Default, Clone)]
pub struct ApiKey {
    pub key: SecretString,
    pub is_set: bool,
}

#[derive(Error, Debug)]
pub enum MyError {
    #[error("Project Directory Error: could not get project directory")]
    ProjectDir, // project dir path error
    #[error(transparent)]
    Database(#[from] rusqlite::Error),
    #[error("Sandbox Version Mismatch: expected {0}, found: {1}")]
    SandboxVersionMismatch(i32, i32),
}

#[derive(Default, Clone, PartialEq, serde::Deserialize, serde::Serialize)]
#[repr(usize)]
pub enum MsgRole {
    // System instructions that guide the AI's behavior
    System,
    // Developer/admin context (provider-specific)
    Developer,
    // User input or questions
    #[default]
    User,
    // AI assistant responses
    Assistant,
    // Results from tool/function calls
    Tool,
}

impl fmt::Display for MsgRole {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            MsgRole::System => write!(f, "system"),
            MsgRole::Developer => write!(f, "developer"),
            MsgRole::User => write!(f, "user"),
            MsgRole::Assistant => write!(f, "assistant"),
            MsgRole::Tool => write!(f, "tool"),
        }
    }
}

impl From<&str> for MsgRole {
    fn from(input: &str) -> Self {
        match input {
            "assistant" => MsgRole::Assistant,
            "system" => MsgRole::System,
            // fall back for "user" or any invalid string
            _ => MsgRole::User,
        }
    }
}

impl From<MsgRole> for Role {
    fn from(source: MsgRole) -> Self {
        match source {
            MsgRole::System => Role::System,
            MsgRole::Developer => Role::Developer,
            MsgRole::User => Role::User,
            MsgRole::Assistant => Role::Assistant,
            MsgRole::Tool => Role::Tool,
        }
    }
}

impl From<MsgRole> for MessageRole {
    fn from(source: MsgRole) -> Self {
        match source {
            MsgRole::System => MessageRole::System,
            MsgRole::Developer => MessageRole::System,
            MsgRole::User => MessageRole::User,
            MsgRole::Assistant => MessageRole::Assistant,
            MsgRole::Tool => MessageRole::Tool,
        }
    }
}

#[derive(Default, Display, EnumString, Debug, Clone, PartialEq,
    serde::Deserialize, serde::Serialize)]
// Remove repr(usize) unless you strictly need it for other C-interop
pub enum ChatRouter {
    #[default]
    Ollama,
    Openrouter,
}

impl FromSql for ChatRouter {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        // Parse directly using the derived EnumString trait
        value.as_str()
        .and_then(|s| s.parse().map_err(
        |_| FromSqlError::Other("Invalid Enum".into())))
    }
}

impl ToSql for ChatRouter {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        // Use the derived Display trait
        Ok(ToSqlOutput::from(self.to_string()))
    }
}

/*
impl fmt::Display for ChatRouter {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            ChatRouter::Ollama => write!(f, "Ollama"),
            ChatRouter::Openrouter => write!(f, "Openrouter"),
        }
    }
}
*/

#[derive(Default, Clone)]
pub struct ChatMsgUi {
    pub show_raw: bool,
}

// ChatMsg to be stored in the database
#[derive(Default, Clone, serde::Deserialize, serde::Serialize)]
pub struct ChatMsg {
    pub id: i64, // id in a database
    pub msg_role: MsgRole,
    pub content: String,
    pub preset_id: i64,
    pub preset: Option<Preset>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<String>,
}

// convert inhouse ChatMsg to Ollama's ChatMessage:
impl From<ChatMsg> for ChatMessage {
    fn from(item: ChatMsg) -> Self {
        ChatMessage {
            role: item.msg_role.into(),
            content: item.content,
            tool_calls: vec![],
            images: None,
            thinking: item.reasoning,
        }
    }
}

// convert inhouse ChatMsg to OpenRouter's Message
impl From<ChatMsg> for Message {
    fn from(item: ChatMsg) -> Self {
        Message {
            role: item.msg_role.into(),
            content: item.content,
        }
    }
}

#[derive(Default, Clone, serde::Deserialize, serde::Serialize)]
pub struct Agent {
    pub id: i64,
    pub name: String,
    pub agent_ind: usize,
    pub msg_ids: Vec<i64>,
    pub preset_selection: PresetSelection,
    pub preset: Option<Preset>,
    pub muted: bool,
    pub hidden: bool, // not showing in the bottom panel, Omnis has hidden=true
    pub deleted: bool,
}

#[derive(Clone, serde::Deserialize, serde::Serialize)]
pub struct Chat {
    pub id: i64,
    pub title: String,
    pub msg_pool: HashMap<i64, ChatMsg>,
    pub agents: Vec<Agent>,
}

impl Default for Chat {
    fn default() -> Self {
        // 1. Create Agent 0 (Omnis)
        // We assume Agent::default() sets basic fields.
        // We explicitly set what we need (e.g., preset_id = 0, name = "Omnis").
        let mut omnis = Agent::default();
        omnis.agent_ind = 0;
        omnis.name = "Omnis".to_string();
        omnis.preset_selection.id = 0;
        omnis.hidden = true;

        // 2. Create Agent 1 (Agent1)
        let mut agent1 = Agent::default();
        agent1.agent_ind = 1;
        agent1.name = "Agent1".to_string();
        agent1.preset_selection.id = 0;

        Self {
            id: 0, // 0 indicates it hasn't been saved to DB yet
            title: "Unnamed Chat".to_string(),
            msg_pool: HashMap::new(),
            agents: vec![omnis, agent1],
        }
    }
}

impl Chat {
    /// Converts a specific agent history into a vector of OpenRouter Messages.
    /// Returns an empty vector if the hist_id is not found.
    pub fn to_openrouter_messages(&self, agent_ind: usize) -> Vec<Message> {
        if let Some(agent) = self.agents.get(agent_ind) {
            agent.msg_ids
                .iter()
                .filter_map(|msg_id| {
                    self.msg_pool
                        .get(msg_id)
                        .cloned()       // Clone the ChatMsg (From consumes input)
                        .map(Into::into)// Convert ChatMsg -> Message
                })
                .collect()
        } else {
            Vec::new()
        }
    }

    /// Converts a specific history branch into a vector of Ollama ChatMessages.
    pub fn to_ollama_messages(&self, agent_ind: usize) -> Vec<ChatMessage> {
        if let Some(agent) = self.agents.get(agent_ind) {
            agent.msg_ids
                .iter()
                .filter_map(|msg_id| {
                    self.msg_pool
                        .get(msg_id)
                        .cloned()        // Clone the ChatMsg
                        .map(Into::into) // Convert ChatMsg -> ollama_rs ChatMessage
                })
                .collect()
        } else {
            Vec::new()
        }
    }

    /// Adds a new agent sequentially if the limit has not been reached.
    pub fn add_agent_try_sync(&mut self, conn: &Connection) ->
                Result<(), Box<dyn std::error::Error>> {
        // Enforce limit of 127 agents (indices 0-126)
        if self.agents.len() >= 127 {
            return Ok(()); // Or return an Err if you want to log a warning
        }

        let new_ind = self.agents.len();

        // 2. Create the struct locally first
        let mut new_agent = Agent::default();
        new_agent.agent_ind = new_ind;
        new_agent.name = format!("Agent{}", new_ind);
        new_agent.hidden = false;
        new_agent.deleted = false;
        new_agent.muted = false;

        // 3. Persist to DB immediately, but only if chat id is not 0
        // We pass &mut new_agent so mk_agent can update new_agent.id
        if self.id != 0 {
            mk_agent(conn, self.id, &mut new_agent)?;
        }

        // 4. Push to state (now containing the correct DB ID)
        self.agents.push(new_agent);

        Ok(())
    }
}

// this is only used for loading chat titles to show them in the side pane
#[derive(Debug)]
pub struct DbChat {
    pub id: i64,
    pub title: String
}

#[derive(Debug, Default, Clone, serde::Deserialize, serde::Serialize)]
pub struct ModelOptions {
    pub include_reasoning: Option<bool>,
    pub seed: Option<i32>, // we use i32 but do not allow negative values
    pub temperature: Option<f64>,
}

// Preset is the essential data structure, because it will hide all
// the llm options under a unique short title and an optional explanation
// (tooltip), so that it can be stored in the database, in json format and so on
#[derive(Clone, serde::Deserialize, serde::Serialize)]
pub struct Preset {
    pub id: i64,
    pub title: String,
    pub tooltip: String,
    pub chat_router: ChatRouter,
    pub model: String,
    pub options: ModelOptions,
    pub hidden: bool, // true when used as an override
    pub deleted: bool,
    #[serde(skip)]
    pub api_key: ApiKey,
    pub inforno_preset: i32, // version info only used for exporting to json
}

impl Default for Preset {
    fn default() -> Self {
        Self {
            id: 0,
            title: "Unnamed Preset".to_string(),
            tooltip: "".to_string(),
            chat_router: ChatRouter::Ollama,
            model: "".to_string(),
            options: ModelOptions::default(),
            hidden: false,
            deleted: false,
            api_key: ApiKey::default(),
            inforno_preset: CURRENT_SANDBOX_VERSION,
        }
    }
}

impl Preset {
    /// Serializes self to a JSON string.
    /// automatically updates 'inforno_preset' to the current version before export.
    pub fn to_json(&self) -> Option<String> {
        // Clone to update version without mutating the live object
        let mut export_copy = self.clone();
        export_copy.inforno_preset = CURRENT_SANDBOX_VERSION;

        match serde_json::to_string(&export_copy) {
            Ok(s) => Some(s),
            Err(e) => {
                eprintln!("Error serializing preset '{}': {}", self.title, e);
                None
            }
        }
    }

    /// Deserializes from a JSON string (DB or File).
    /// Handles legacy JSONs (missing 'inforno_preset') via serde defaults.
    pub fn from_json(json_str: Option<&String>) -> Option<Self> {
        let s = json_str?;

        match serde_json::from_str::<Preset>(s) {
            Ok(p) => {
                // (Optional) Migration Logic Placeholder
                // if p.inforno_preset < CURRENT_SANDBOX_VERSION { ... }

                Some(p)
            },
            Err(e) => {
                eprintln!("Error deserializing preset: {}", e);
                None
            }
        }
    }
}

// all presets from the database will be stored here:
#[derive(Default)]
pub struct Presets {
    pub hash: HashMap<i64, Preset>,
    pub cache: Vec<(i64, String)>,

    // increment this whenever 'hash' or 'cache' is modified
    generation: usize,
}

impl Presets {
    // get a preset by id
    pub fn get(&self, id: i64) -> Option<&Preset> {
        self.hash.get(&id)
    }

    // Call this whenever you modify the presets
    pub fn mark_changed(&mut self) {
        self.generation = self.generation.wrapping_add(1);
    }

    // Helper to get current generation
    pub fn generation(&self) -> usize {
        self.generation
    }

    /// Replaces all current presets with new ones loaded from DB
    pub fn replace_all(&mut self, new_presets: Vec<Preset>) {
        self.hash.clear();
        self.cache.clear();

        for preset in new_presets {
            // Logic for cache/hash split
            if !preset.deleted {
                self.cache.push((preset.id, preset.title.clone()));
            }
            self.hash.insert(preset.id, preset);
        }

        // CRITICAL: Ensure the UI knows data changed
        self.mark_changed();
    }
}

pub fn load_presets(conn: &rusqlite::Connection, presets: &mut Presets) {
    match crate::db::load_presets_vec(conn) {
        Ok(val) => {
            presets.replace_all(val);
        },
        Err(error) => {
            eprintln!("Error loading presets: {}", error);
        },
    }
}

// selection stores both index in the Presets::cache array and Preset id in db
#[derive(Clone, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct PresetSelection {
    pub ind: usize,
    pub id: i64,
    pub title: String,
    #[serde(skip)]
    last_sync_gen: usize,
}

impl Default for PresetSelection {
    fn default() -> Self {
        Self {
            ind: usize::MAX,
            id: 0,
            title: "".to_string(),
            last_sync_gen: 0,
        }
    }
}

impl PresetSelection {
    pub fn from_id(id: i64, presets: &Presets) -> Self {
        // 1. Create a default instance
        // We need 'mut' because sync_with_presets requires a mutable reference
        let mut selection = Self::default();

        // 2. Set the ID to the supplied parameter
        selection.id = id;

        // 3. Sync with presets to resolve index and title
        selection.sync_with_presets(presets);

        // Return the fully initialized struct
        selection
    }

    pub fn sync_with_presets(&mut self, presets: &Presets) {
        if self.ind == usize::MAX {
            if let Some(position) = presets.cache.iter()
                    .position(|(id, _)| *id == self.id) {
                self.ind = position;
                self.title = presets.cache[position].1.clone();
            } else {
                // failure to find any matching preset
                self.ind = usize::MAX;
            }
            return;
        }

        // check if presets were updated since last sync
        if self.last_sync_gen == presets.generation() {
            return;
        }
        self.last_sync_gen = presets.generation();

        // happy path
        if self.ind != usize::MAX {
            if let Some((cached_id, cached_title)) =
                    presets.cache.get(self.ind) {
                if *cached_id == self.id {
                    if self.title != *cached_title {
                        self.title = cached_title.clone();
                    }
                    return;
                }
            }
        }
    }
}

#[derive(Debug, Default)]
pub struct DbOpenrModel {
    pub id: i64,
    pub provider: String,
    pub model_id: String,
    pub name: String,
    pub description: String,
    pub context_length: f64,
    pub price_prompt: Option<f64>,
    pub price_completion: Option<f64>,
    pub price_image: Option<f64>,
    pub details: Option<String>,
    pub ts_model: Option<String>,
}

#[derive(Debug, Default)]
pub struct DbOllamaModel {
    pub id: i64,
    pub name: String,
    pub size: i64,
    pub url: Option<String>,
    pub variants: Vec<(String, String)>, // (token size, size)
    pub category: Option<String>,
    pub summary: Option<String>,
    pub ts_model: Option<String>,
    pub ts_updated: Option<String>,
}

#[derive(Default, Clone)] // Clone allows easy passing to threads
pub struct OllamaDownloading {
    pub progress: f32,       // 0.0 to 1.0
    pub status_text: String, // e.g. "pulling sha256..."
    pub progress_text: String,
    pub is_downloading: bool,
    pub error_msg: Option<String>,
}

#[derive(Default, Clone, serde::Deserialize, serde::Serialize)]
pub struct ChatQue {
    pub agent_ind: usize,
    pub preset: Preset,
    pub chat: Arc<Chat>,
}

#[derive(Default, serde::Deserialize, serde::Serialize)]
pub struct ChatResp {
    pub chat_msg: ChatMsg,
}

// helper function for displaying file sizes
pub fn format_bytes(bytes: u64) -> String {
    const GB: u64 = 1024 * 1024 * 1024;
    const MB: u64 = 1024 * 1024;

    if bytes >= GB {
        format!("{:.2} GB", bytes as f64 / GB as f64)
    } else {
        format!("{:.2} MB", bytes as f64 / MB as f64)
    }
}

pub fn mask_key_secure(key: &str) -> String {
    let char_count = key.chars().count();

    // If too short, don't even return it. Just return placeholders.
    if char_count <= 4 {
        return "***".to_string();
    }

    // 1. Grab first 2 chars (only allocates space for 2 chars)
    let start: String = key.chars().take(2).collect();

    // 2. Grab last 2 chars (only allocates space for 2 chars)
    // We reverse, take 2, collect to vec to un-reverse them.
    let end: String = key.chars().rev().take(2).collect::<Vec<_>>()
    .into_iter().rev().collect();

    format!("{}..{}", start, end)
}

/*
// do chat completion request
pub async fn do_chat_que(cq: ChatQue) -> Result<ChatResp, String> {
    let model = cq.preset.model.clone();
    match cq.preset.chat_router {
        ChatRouter::Ollama => {
            let response = do_ollama_chat_que(cq)
                .await;

            match response {
                Ok(res) => {
                    println!("{:?}", res);
                    return Ok(ChatResp {
                        chat_msg: ChatMsg {
                            msg_role: MsgRole::Assistant,
                            content: res.message.content.clone(),
                            name: None,
                            model: Some(model),
                            reasoning: res.message.thinking.clone(),
                            ..Default::default()
                        }
                    });
                },
                Err(_err) => {
                    return Err(format!(
                    "Error accessing Ollama. Is it running? \
                    Try typing in terminal: ollama serve \n\
                    If Ollama is running, check if it has the model '{}' \
                    installed: ollama list\n\
                    to install it: ollama pull {}", model, model).to_string());
                }
            }
        },
        ChatRouter::Openrouter => {
            let response = do_openr_chat_que(cq)
                .await;

            match response {
                Ok(res) => {
                    match &res.choices[0].content() {
                        Some(str_output) => {
                            println!("{:#?}", res);
                            return Ok(ChatResp {
                                chat_msg: ChatMsg {
                                    msg_role: MsgRole::Assistant,
                                    content: str_output.to_string(),
                                    name: None,
                                    model: Some(res.model),
                                    //reasoning :
                                    ..Default::default()
                                }
                            });
                        }
                        None => {
                            return Err(format!("Empty reply from openrouter").to_string());
                        }
                    }
                },
                Err(err) => {
                    return Err(format!("Error accessing openrouter: {}", err).to_string());
                }
            }
        }
    }
}
*/

pub async fn run_chat_stream_router(
    query: ChatQue,
    tx: Sender<ChatStreamEvent>,
    ctx: &egui::Context,
    abort_flag: Arc<AtomicBool>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    println!("Agent options: {:?}", &query.preset.options);
    match query.preset.chat_router {
        ChatRouter::Openrouter => {
            crate::openr::do_openr_chat_stream(query, tx, ctx, abort_flag).await
        }
        ChatRouter::Ollama => {
            crate::ollama::do_ollama_chat_stream(query, tx, ctx, abort_flag).await
        }
    }
}