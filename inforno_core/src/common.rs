use std::collections::HashMap;
use std::path::{PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::Sender;
use openrouter_rs::Message;
use openrouter_rs::types::Role;
use rusqlite::{Connection, ToSql};
use rusqlite::types::{FromSql, FromSqlError, FromSqlResult, ToSqlOutput, ValueRef};
use secrecy::SecretString;
use strum::{Display, EnumString};
use thiserror::Error;
use crate::db::mk_talker;
use ollama_rs::generation::chat::{ChatMessage, MessageRole};
use ollama_rs::generation::images::Image;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Attachment {
    pub filename: String,
    pub mime_type: String, // e.g., "text/rust", "image/png"
    pub content: String,   // Raw text or base64 encoded data
}

pub fn format_deep_copy(prompt: &str, attachments: &[Attachment]) -> String {
    let mut full_content = prompt.to_string();
    for att in attachments {
        if att.mime_type.starts_with("text/") {
            full_content.push_str(&format!("\n\n--- File: {} ---\n", att.filename));
            full_content.push_str(&att.content);
        } else if att.mime_type.starts_with("image/") {
            full_content.push_str(&format!(
                "\n\n![{}](data:{};base64,{})\n",
                att.filename, att.mime_type, att.content
            ));
        }
    }
    full_content
}

// when streaming a chat, this structure is passed to the GUI
pub enum ChatStreamEvent {
    Content(usize, String),
    Reasoning(usize, String),
    Finished(usize),
    Error(usize, String),
}

/// Sends one Content/Reasoning chunk to the GUI and wakes it up.
/// Empty chunks are skipped, same as every backend did before.
/// `event` is the variant constructor, e.g. `ChatStreamEvent::Content`.
pub(crate) fn emit_text(
    tx: &Sender<ChatStreamEvent>,
    wakeup: &(dyn Fn() + Send + Sync),
    event: fn(usize, String) -> ChatStreamEvent,
    talker_ind: usize,
    text: &str,
) {
    if text.is_empty() {
        return;
    }
    let _ = tx.send(event(talker_ind, text.to_string()));
    wakeup();
}

#[derive(Default, Clone)]
pub enum FileOp {
    #[default]
    Open,
    SaveAs,
    SaveCopy,
    Clear,
    Attach,
    OpenEditor,
    OpenEditorRight,
    OpenMerge,
    OpenMergeRight,
    ExportChat,
    ExportNotebook,
}

/// Notebook export (`.ron`), version "2.0". A note or script cell is
/// `Cell`, and `Developer` is a developer message; in version "1.0",
/// `Developer` meant a cell.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct NotebookExportV2 {
    pub version: String,
    pub title: String,
    pub cells: Vec<NotebookCellExport>,
}

#[derive(serde::Serialize, serde::Deserialize)]
pub struct NotebookCellExport {
    pub role: MsgRole,
    pub content: String,
}

#[derive(Default, Clone)]
pub struct FileOpMsg {
    pub op: FileOp,
    pub cancelled: bool,
    pub path: Option<PathBuf>,
    pub attachments: Option<Vec<Attachment>>,
    pub left_content: Option<String>,
    pub right_content: Option<String>,
}

#[derive(Default, Clone)]
pub struct ApiKey {
    pub key: SecretString,
    pub is_set: bool,
    /// Hosts this key may be sent to (`api_keys::check_host`); none means
    /// nowhere.
    pub hosts: Vec<String>,
}

impl ApiKey {
    pub fn new(key: SecretString, hosts: Vec<String>) -> Self {
        Self { key, is_set: true, hosts }
    }
}

#[derive(Error, Debug)]
pub enum MyError {
    #[error("Project Directory Error: could not get project directory")]
    ProjectDir, // project dir path error
    #[error("Folio Directory Error: could not create the directory the Folio goes in")]
    FolioDir,
    #[error("Folio Name Error: a Folio name must be a plain file name, not a path")]
    FolioName,
    #[error(transparent)]
    Database(#[from] rusqlite::Error),
    #[error("Folio Version Mismatch: expected {0}, found: {1}")]
    FolioVersionMismatch(i32, i32),
    #[error("Older Folio Format: this Folio is version {0} and needs converting")]
    LegacyFolio(i32),
    #[error("Folio Conversion Error: {0}")]
    FolioConvert(String),
}

/// Who a message is from. The Folio stores it by its short code
/// (`db_code`); the UI names it by `label`.
#[derive(Default, Clone, PartialEq, serde::Deserialize, serde::Serialize)]
#[repr(usize)]
pub enum MsgRole {
    // System instructions that guide the AI's behavior
    System,
    // Developer instructions, for providers that have them
    Developer,
    // User input or questions
    #[default]
    User,
    // AI assistant responses
    Assistant,
    // Results from tool/function calls
    Tool,
    /// A notebook cell: a note or a Rhai script. Never sent to a model.
    Cell,
}

impl MsgRole {
    /// The code stored in the Folio's `msg.role` column.
    pub fn db_code(&self) -> &'static str {
        match self {
            MsgRole::System => "sys",
            MsgRole::Developer => "dev",
            MsgRole::User => "usr",
            MsgRole::Assistant => "asst",
            MsgRole::Tool => "tool",
            MsgRole::Cell => "cell",
        }
    }

    /// The role stored as `code`, if it's one this version knows.
    pub fn from_db_code(code: &str) -> Option<Self> {
        Some(match code {
            "sys" => MsgRole::System,
            "dev" => MsgRole::Developer,
            "usr" => MsgRole::User,
            "asst" => MsgRole::Assistant,
            "tool" => MsgRole::Tool,
            "cell" => MsgRole::Cell,
            _ => return None,
        })
    }

    /// How the UI names it.
    pub fn label(&self) -> &'static str {
        match self {
            MsgRole::System => "system",
            MsgRole::Developer => "developer",
            MsgRole::User => "user",
            MsgRole::Assistant => "assistant",
            MsgRole::Tool => "tool",
            MsgRole::Cell => "note",
        }
    }
}

impl ToSql for MsgRole {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(ToSqlOutput::from(self.db_code()))
    }
}

/// Reading a role this version doesn't know is an error, never a guess.
impl FromSql for MsgRole {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        let code = value.as_str()?;
        MsgRole::from_db_code(code).ok_or_else(|| {
            FromSqlError::Other(format!("unknown message role {:?}", code).into())
        })
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
            // Never reached: to_openrouter_messages drops cells first.
            MsgRole::Cell => Role::Developer,
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
            // Never reached: to_ollama_messages drops cells first.
            MsgRole::Cell => MessageRole::System,
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

// ChatMsg to be stored in the database
#[derive(Default, Clone, serde::Deserialize, serde::Serialize)]
pub struct ChatMsg {
    pub id: i64, // id in a database
    pub msg_role: MsgRole,
    pub content: String,
    pub preset_id: i64,
    pub preset: Option<ModelPreset>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<String>,

    // NEW: Transient fields for notebook cells (never saved to SQLite)
    #[serde(skip)]
    pub volatile_output: Option<String>,
    #[serde(skip)]
    pub is_unsaved: bool,
}

// convert inhouse ChatMsg to Ollama's ChatMessage:
impl From<ChatMsg> for ChatMessage {
    fn from(item: ChatMsg) -> Self {
        let mut full_content = item.content;
        let mut images = Vec::new(); // Rust will now infer this as Vec<Image>

        // Deserialize the JSON array and append text or gather images
        if let Some(details) = item.details {
            if let Ok(attachments) = serde_json::from_str::<Vec<Attachment>>(&details) {
                for att in attachments {
                    if att.mime_type.starts_with("text/") {
                        full_content.push_str(&format!("\n\n--- File: {} ---\n", att.filename));
                        full_content.push_str(&att.content);
                    } else if att.mime_type.starts_with("image/") {
                        // FIX: Convert the base64 string into ollama_rs's Image type
                        images.push(Image::from_base64(&att.content));
                    }
                }
            }
        }

        ChatMessage {
            role: item.msg_role.into(),
            content: full_content,
            tool_calls: vec![],
            images: if images.is_empty() { None } else { Some(images) },
            thinking: item.reasoning,
        }
    }
}

// convert inhouse ChatMsg to OpenRouter's Message
impl From<ChatMsg> for Message {
    fn from(item: ChatMsg) -> Self {
        let mut full_content = item.content;

        // Deserialize the JSON array and append text attachments
        if let Some(details) = item.details {
            if let Ok(attachments) = serde_json::from_str::<Vec<Attachment>>(&details) {
                for att in attachments {
                    if att.mime_type.starts_with("text/") {
                        full_content.push_str(&format!("\n\n--- File: {} ---\n", att.filename));
                        full_content.push_str(&att.content);
                    } else if att.mime_type.starts_with("image/") {
                        // For OpenRouter, passing standard Markdown Data URIs works perfectly for multi-modal parsing
                        full_content.push_str(&format!("\n\n![{}]({})\n", att.filename, format!("data:{};base64,{}", att.mime_type, att.content)));
                    }
                }
            }
        }

        // Use the library's constructor instead of a struct expression
        let mut msg = Message::new(item.msg_role.into(), full_content);

        // Since `name` is an optional public field, we can assign it after creation
        msg.name = item.name;

        msg
    }
}

/// A chat participant: a name and a model preset, with its own view of the
/// chat (`msg_ids`). Talker 0, Omnis, is hidden and holds every message in
/// order.
#[derive(Default, Clone, serde::Deserialize, serde::Serialize)]
pub struct Talker {
    pub id: i64,
    pub name: String,
    pub talker_ind: usize,
    pub msg_ids: Vec<i64>,
    pub preset_selection: ModelPresetSelection,
    pub preset: Option<ModelPreset>,
    pub muted: bool,
    pub hidden: bool, // not showing in the bottom panel, Omnis has hidden=true
    pub deleted: bool,
}

#[derive(Clone, serde::Deserialize, serde::Serialize)]
pub struct Chat {
    pub id: i64,
    pub title: String,
    pub msg_pool: HashMap<i64, ChatMsg>,
    pub talkers: Vec<Talker>,
    #[serde(skip)]
    pub draft_note: String, // Buffer for the bottom notebook cell
    #[serde(skip)]
    pub draft_is_rhai: bool,
}

impl Default for Chat {
    fn default() -> Self {
        // 1. Create Talker 0 (Omnis): hidden, holds every message in order.
        // We explicitly set what we need (e.g., preset_id = 0, name = "Omnis").
        let mut omnis = Talker::default();
        omnis.talker_ind = 0;
        omnis.name = "Omnis".to_string();
        omnis.preset_selection.id = 0;
        omnis.hidden = true;

        // 2. Create Talker 1 (Talker1)
        let mut talker1 = Talker::default();
        talker1.talker_ind = 1;
        talker1.name = "Talker1".to_string();
        talker1.preset_selection.id = 0;

        Self {
            id: 0, // 0 indicates it hasn't been saved to DB yet
            title: "Unnamed Chat".to_string(),
            msg_pool: HashMap::new(),
            talkers: vec![omnis, talker1],
            draft_note: String::new(),
            draft_is_rhai: false,
        }
    }
}

impl Chat {
    /// Converts a specific talker's history into a vector of OpenRouter Messages.
    /// Returns an empty vector if there is no talker at `talker_ind`.
    pub fn to_openrouter_messages(&self, talker_ind: usize) -> Vec<Message> {
        if let Some(talker) = self.talkers.get(talker_ind) {
            talker.msg_ids
                .iter()
                .filter_map(|msg_id| {
                    let msg = self.msg_pool.get(msg_id)?;
                    // Notebook cells are never sent
                    if msg.msg_role == MsgRole::Cell {
                        return None;
                    }
                    Some(msg.clone().into()) // Clone ChatMsg, Convert to openrouter
                })
                .collect()
        } else {
            Vec::new()
        }
    }

    /// Converts a specific talker's history into a vector of Ollama ChatMessages.
    pub fn to_ollama_messages(&self, talker_ind: usize) -> Vec<ChatMessage> {
        if let Some(talker) = self.talkers.get(talker_ind) {
            talker.msg_ids
                .iter()
                .filter_map(|msg_id| {
                    let msg = self.msg_pool.get(msg_id)?;
                    // Notebook cells are never sent
                    if msg.msg_role == MsgRole::Cell {
                        return None;
                    }
                    Some(msg.clone().into()) // Clone ChatMsg, Convert to ollama_rs
                })
                .collect()
        } else {
            Vec::new()
        }
    }

    /// Adds a new talker sequentially if the limit has not been reached.
    pub fn add_talker_try_sync(&mut self, conn: &Connection) ->
                Result<(), Box<dyn std::error::Error>> {
        // Enforce limit of 127 talkers (indices 0-126)
        if self.talkers.len() >= 127 {
            return Err("Maximum number of talkers (127) reached".into());
        }

        let new_ind = self.talkers.len();

        // 2. Create the struct locally first
        let mut new_talker = Talker::default();
        new_talker.talker_ind = new_ind;
        new_talker.name = format!("Talker{}", new_ind);
        new_talker.hidden = false;
        new_talker.deleted = false;
        new_talker.muted = false;

        // 3. Persist to DB immediately, but only if chat id is not 0
        // We pass &mut new_talker so mk_talker can update new_talker.id
        if self.id != 0 {
            mk_talker(conn, self.id, &mut new_talker)?;
        }

        // 4. Push to state (now containing the correct DB ID)
        self.talkers.push(new_talker);

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
#[serde(default)]
pub struct ModelOptions {
    pub include_reasoning: Option<bool>,
    pub seed: Option<i32>, // we use i32 but do not allow negative values
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub max_tokens: Option<i32>,
    pub top_k: Option<i32>,
    pub frequency_penalty: Option<f64>,
    pub presence_penalty: Option<f64>,
    pub repetition_penalty: Option<f64>,
    pub min_p: Option<f64>,
    pub stop_sequences: Option<Vec<String>>,
    pub stream: Option<bool>,
    pub openrouter_custom_url: Option<String>,
}

pub struct CustomOpenrConfig {
    pub nickname: String,
    pub url: String,
    pub token: String,
}

pub fn get_custom_openr_configs() -> Vec<CustomOpenrConfig> {
    let mut configs = Vec::new();
    if let Ok(env_val) = std::env::var("OPENROUTER_API_URLS") {
        let parts: Vec<&str> = env_val.split(',').collect();
        for chunk in parts.chunks(3) {
            if chunk.len() == 3 {
                configs.push(CustomOpenrConfig {
                    nickname: chunk[0].trim().to_string(),
                    url: chunk[1].trim().to_string(),
                    token: chunk[2].trim().to_string(),
                });
            }
        }
    }
    configs
}

// A ModelPreset is a named set of LLM parameters: router, model and options
// under a unique short title and an optional explanation (tooltip), so that
// it can be stored in the database, in json format and so on
#[derive(Clone, serde::Deserialize, serde::Serialize)]
pub struct ModelPreset {
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
}

impl Default for ModelPreset {
    fn default() -> Self {
        Self {
            id: 0,
            title: "Unnamed Model Preset".to_string(),
            tooltip: "".to_string(),
            chat_router: ChatRouter::Ollama,
            model: "".to_string(),
            options: ModelOptions::default(),
            hidden: false,
            deleted: false,
            api_key: ApiKey::default(),
        }
    }
}

impl ModelPreset {
    /// Serializes self to a JSON string. Snapshots live inside a Folio, so
    /// the Folio's schema version is also theirs.
    pub fn to_json(&self) -> Option<String> {
        match serde_json::to_string(self) {
            Ok(s) => Some(s),
            Err(e) => {
                eprintln!("Error serializing preset '{}': {}", self.title, e);
                None
            }
        }
    }

    /// Deserializes a snapshot stored in the Folio. Snapshots written while
    /// presets still carried an `inforno_preset` version stamp load as
    /// well: serde skips keys it doesn't know.
    pub fn from_json(json_str: Option<&String>) -> Option<Self> {
        let s = json_str?;

        match serde_json::from_str::<ModelPreset>(s) {
            Ok(p) => Some(p),
            Err(e) => {
                eprintln!("Error deserializing preset: {}", e);
                None
            }
        }
    }
}

// all model presets from the database are kept here:
#[derive(Default)]
pub struct ModelPresets {
    pub hash: HashMap<i64, ModelPreset>,
    pub cache: Vec<(i64, String)>,

    // increment this whenever 'hash' or 'cache' is modified
    generation: usize,
}

impl ModelPresets {
    // get a model preset by id
    pub fn get(&self, id: i64) -> Option<&ModelPreset> {
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
    pub fn replace_all(&mut self, new_presets: Vec<ModelPreset>) {
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

pub fn load_model_presets(conn: &rusqlite::Connection, presets: &mut ModelPresets) {
    match crate::db::load_model_presets_vec(conn) {
        Ok(val) => {
            presets.replace_all(val);
        },
        Err(error) => {
            eprintln!("Error loading presets: {}", error);
        },
    }
}

// selection stores both index in the ModelPresets::cache array and the
// ModelPreset id in db
#[derive(Clone, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct ModelPresetSelection {
    pub ind: usize,
    pub id: i64,
    pub title: String,
    #[serde(skip)]
    last_sync_gen: usize,
}

impl Default for ModelPresetSelection {
    fn default() -> Self {
        Self {
            ind: usize::MAX,
            id: 0,
            title: "".to_string(),
            last_sync_gen: 0,
        }
    }
}

impl ModelPresetSelection {
    pub fn from_id(id: i64, presets: &ModelPresets) -> Self {
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

    pub fn sync_with_presets(&mut self, presets: &ModelPresets) {
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
	pub abort_flag: Arc<AtomicBool>,
}

#[derive(Default, Clone, serde::Deserialize, serde::Serialize)]
pub struct ChatQue {
    pub talker_ind: usize,
    pub preset: ModelPreset,
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

pub async fn run_chat_stream_router(
    query: ChatQue,
    tx: Sender<ChatStreamEvent>,
    wakeup: Arc<dyn Fn() + Send + Sync>,
    abort_flag: Arc<AtomicBool>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    println!("Talker options: {:?}", &query.preset.options);

    // Default to streaming unless explicitly set to false
    let use_streaming = query.preset.options.stream.unwrap_or(true);

    match query.preset.chat_router {
        ChatRouter::Openrouter => {
            if use_streaming {
                crate::openr::do_openr_chat_stream(query, tx, wakeup, abort_flag).await
            } else {
                crate::openr::do_openr_chat_sync(query, tx, wakeup, abort_flag).await
            }
        }
        ChatRouter::Ollama => {
            if use_streaming {
                crate::ollama::do_ollama_chat_stream(query, tx, wakeup, abort_flag).await
            } else {
                crate::ollama::do_ollama_chat_sync(query, tx, wakeup, abort_flag).await
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct SearchResult {
    pub chat_id: i64,
    pub chat_title: String,
    pub snippet: String,
}

pub fn core_test() {
    println!("Core is linked!");
}
