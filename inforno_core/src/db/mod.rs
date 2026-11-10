use std::{collections::HashMap, fs, path::{Path, PathBuf}, sync::OnceLock};
use regex::Regex;
use rusqlite::{Connection, params, Result};
use directories::ProjectDirs;
use crate::common::{Chat, ChatMsg, DbChat, ModelPreset, ModelPresetSelection, ModelPresets, MyError, Talker};

pub mod cache;
pub mod migrate;

pub const CURRENT_FOLIO_VERSION: i32 = 3;

/// Where Inforno creates new Folios unless told otherwise:
/// `<data_dir>/folios`. A Folio can live anywhere; a Realm links to each
/// one by absolute path.
pub fn folios_dir() -> Option<PathBuf> {
    ProjectDirs::from("", "", "inforno").map(|d| d.data_dir().join("folios"))
}

/// The path of a Folio called `name` in `folios_dir()`, adding `.rno` when
/// it's missing. `name` must be a plain file name: a separator, `.` or
/// `..` could take the path outside the folios directory.
pub fn folio_path_for(name: &str) -> Result<PathBuf, MyError> {
    let name = name.trim();
    let file = if name.ends_with(".rno") { name.to_string() } else { format!("{}.rno", name) };
    if file == ".rno" || !crate::realm::is_safe_path_component(&file) {
        return Err(MyError::FolioName);
    }
    folios_dir().map(|dir| dir.join(file)).ok_or(MyError::ProjectDir)
}

// Helper to securely find the user's home folio file
pub fn get_home_folio_path() -> Option<PathBuf> {
    if let Some(proj_dirs) = ProjectDirs::from("", "", "inforno") {
        let file_path = PathBuf::from(proj_dirs.data_dir()).join("info.rno");
        if file_path.exists() {
            return Some(file_path);
        }
    }
    None
}

/// Opens the Folio at `path`, creating it, and its directory, if needed.
/// A newly created Folio can start with a copy of the home Folio's presets;
/// an existing one is left as it is.
pub fn init_folio(path: &Path, copy_presets: bool) -> Result<(), MyError> {
    let is_new = !path.exists();

    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|_| MyError::FolioDir)?;
    }

    // Open/create the Folio database
    let new_conn = connect_folio_db(path)?;

    // If the user wants presets, fetch them from the home DB
    if is_new && copy_presets {
        if let Some(home_path) = get_home_folio_path().filter(|home| home.as_path() != path) {
            if let Ok(home_conn) = connect_folio_db(&home_path) {
                if let Ok(presets) = load_model_presets_vec(&home_conn) {
                    for mut preset in presets {
                        preset.id = 0; // Set to 0 so `save_model_preset` treats it as a brand new insert!
                        let _ = save_model_preset(&new_conn, &mut preset);
                    }
                }
            }
        }
    }
    Ok(())
}

/// Where the Home Folio lives, `<data_dir>/info.rno`, whether or not it
/// exists yet.
pub fn home_folio_file() -> Option<PathBuf> {
    ProjectDirs::from("", "", "inforno").map(|d| d.data_dir().join("info.rno"))
}

/// `folio` if one is given, otherwise the Home Folio's path, creating the
/// directory it goes in.
pub fn folio_or_home_path(folio: &Option<PathBuf>) -> Result<PathBuf, MyError> {
    if let Some(folio) = folio {
        return Ok(folio.clone());
    }
    let path = home_folio_file().ok_or(MyError::ProjectDir)?;
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|_| MyError::ProjectDir)?;
    }
    Ok(path)
}

/// The Folio's schema version, or None if it has no schema yet.
pub(crate) fn schema_version(conn: &Connection) -> rusqlite::Result<Option<i32>> {
    if !table_exists(conn, "schema_version")? {
        return Ok(None);
    }
    conn.query_row("SELECT version FROM schema_version", [], |row| row.get(0))
        .map(Some)
}

/// Opens the Folio at `folio`, creating it with the current schema if it's
/// new. A Folio in another format is refused before anything is written to
/// it: version 2 with `LegacyFolio`, which `migrate::convert_legacy_folio`
/// converts.
pub fn connect_folio_db(folio: &Path) -> Result<Connection, MyError> {
    let conn = Connection::open(folio)?;
    conn.pragma_update(None, "foreign_keys", "ON")?;

    match schema_version(&conn)? {
        Some(CURRENT_FOLIO_VERSION) => Ok(conn),
        Some(2) => Err(MyError::LegacyFolio(2)),
        Some(other) => Err(MyError::FolioVersionMismatch(CURRENT_FOLIO_VERSION, other)),
        None => {
            println!("Folio has no schema yet. Creating it...");
            create_database_schema(&conn)?;
            Ok(conn)
        }
    }
}

/// An empty Folio in memory. It stands in when no Folio could be opened, so
/// the app still runs; nothing in it is saved.
pub fn open_scratch_folio() -> rusqlite::Result<Connection> {
    let conn = Connection::open_in_memory()?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    create_database_schema(&conn)?;
    Ok(conn)
}

pub fn reset_folio_db(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute("PRAGMA foreign_keys = OFF", [])?;

    let tx = conn.unchecked_transaction()?;

    // 1. Get Views (Wrap in block so `stmt` dies at the closing brace)
    let views: Vec<String> = {
        let mut stmt = tx.prepare(
            "SELECT name FROM sqlite_master WHERE type='view' AND name NOT LIKE 'sqlite_%'"
        )?;
        stmt.query_map([], |row| row.get(0))?
            .collect::<Result<_, _>>()?
    }; // <--- `stmt` is dropped here, releasing the borrow on `tx`

    // 2. Get Tables (Wrap in block here too)
    let tables: Vec<String> = {
        let mut stmt = tx.prepare(
            "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'"
        )?;
        stmt.query_map([], |row| row.get(0))?
            .collect::<Result<_, _>>()?
    }; // <--- `stmt` is dropped here

    // 3. Now we can use `tx` freely to drop things
    for view in views {
        tx.execute(&format!("DROP VIEW IF EXISTS \"{}\"", view), [])?;
    }

    for table in tables {
        tx.execute(&format!("DROP TABLE IF EXISTS \"{}\"", table), [])?;
    }

    // 4. Finally, commit (requires ownership of `tx`)
    tx.commit()?;

    // 5. Cleanup
    conn.execute("PRAGMA foreign_keys = ON", [])?;
    conn.execute("VACUUM", [])?;

    create_database_schema(&conn)?;

    Ok(())
}

pub fn is_table_empty(conn: &Connection, table_name: &str) -> rusqlite::Result<bool> {
    let query = format!("select exists(select 1 from {})", table_name);
    let exists: bool = conn.query_row(&query, [], |row| row.get(0))?;
    Ok(!exists)
}

fn table_exists(conn: &Connection, table_name: &str) -> rusqlite::Result<bool> {
    // SQLite stores table metadata in 'sqlite_master'
    let mut stmt = conn.prepare(
        "SELECT count(*) FROM sqlite_master WHERE type='table' AND name=?1"
    )?;

    // query_row returns a Result. We read the count (i32).
    let count: i32 = stmt.query_row(params![table_name], |row| {
        row.get(0)
    })?;

    // If count > 0, the table exists
    Ok(count > 0)
}

pub fn mk_chat(conn: &Connection, chat: &mut Chat) -> rusqlite::Result<()> {
    // 1. Start Transaction
    // Transactions are crucial here so you don't end up with a Chat that has no Talkers
    // if something fails halfway through.
    conn.execute("BEGIN", [])?;

    let mut run_transaction = || -> rusqlite::Result<()> {
        // Clean title to ensure no newlines are saved
        chat.title = chat.title.split('\n').next().unwrap_or(&chat.title).trim().to_string();

        // 2. Insert Chat Row
        conn.execute("INSERT INTO chat (title) VALUES (?1)", [&chat.title])?;

        // Update the Chat struct immediately
        chat.id = conn.last_insert_rowid();

        // 3. Persist Talkers
        // We iterate through the talkers already present in the struct.
        // This is perfect for "cloning" a chat or creating a new default one.
        for talker in &mut chat.talkers {
            // We delegate to the mutating mk_talker function.
            // This handles serializing msg_ids, model_preset_json, and updating talker.id.
            mk_talker(conn, chat.id, talker)?;
        }

        Ok(())
    };

    // 4. Commit or Rollback
    match run_transaction() {
        Ok(_) => {
            conn.execute("COMMIT", [])?;
            Ok(())
        }
        Err(e) => {
            // If anything fails (e.g., unique constraint on talker_ind), roll back everything.
            let _ = conn.execute("ROLLBACK", []);
            Err(e)
        }
    }
}

pub fn mk_talker(
    conn: &rusqlite::Connection,
    chat_id: i64,
    talker: &mut Talker
) -> rusqlite::Result<()> {

    // 1. Serialize
    let msg_ids_json = serde_json::to_string(&talker.msg_ids).unwrap_or_else(|_| "[]".into());
    let preset_json = talker.preset.as_ref().and_then(|p| p.to_json());

    // 2. Insert
    conn.execute(
        "INSERT INTO talker (
            chat_id,
            talker_ind,
            name,
            msg_ids,
            model_preset_id,
            model_preset_json,
            muted,
            hidden,
            deleted
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        rusqlite::params![
            chat_id,
            talker.talker_ind as i64,
            talker.name,
            msg_ids_json,
            talker.preset_selection.id,
            preset_json,
            talker.muted,
            talker.hidden,
            talker.deleted
        ],
    )?;

    // 3. Update the Struct ID immediately
    talker.id = conn.last_insert_rowid();

    Ok(())
}

/// Updates an EXISTING talker using its unique database row ID.
/// (Use this when you have the 'id' field from the Talker struct)
pub fn mod_talker_msgs(conn: &Connection, id: i64, msg_ids: &[i64])
        -> rusqlite::Result<()> {
    let json_ids = serde_json::to_string(msg_ids)
            .unwrap_or_else(|_| "[]".to_string());

    conn.execute(
        "UPDATE talker
         SET msg_ids = ?1
         WHERE id = ?2",
        (json_ids, id),
    )?;
    Ok(())
}

/// Updates a talker's base model preset selection and its snapshot
pub fn mod_talker_model_preset(
    conn: &Connection,
    id: i64,
    preset_id: i64,
    preset_snapshot: Option<&ModelPreset>
) -> Result<()> {

    // Serialize (or get None)
    let json = preset_snapshot.and_then(|p| p.to_json());

    conn.execute(
        "UPDATE talker
         SET model_preset_id = ?1, model_preset_json = ?2
         WHERE id = ?3",
        params![preset_id, json, id],
    )?;
    Ok(())
}

// only update snapshot - when tweaking parameters in the bottom_panel
pub fn update_talker_model_preset_snapshot(
    conn: &Connection,
    talker_id: i64,
    preset: Option<&ModelPreset>
) -> rusqlite::Result<()> {

    let json = preset.and_then(|p| p.to_json());

    conn.execute(
        "UPDATE talker SET model_preset_json = ?1 WHERE id = ?2",
        rusqlite::params![json, talker_id]
    )?;

    Ok(())
}

// some LLMs produce code blocks where the initial triple tick is
// indented. CommonMark creates squeezed boxes instead of a normal full-sized
// box, which is annoying. This fix applies a regexp to remove any
// indentation from triple backticks. In the database, we store verbatim original
// data not to introduce any unforeseen issues.
// We also use this to safely pad display math ($$) and blockquotes (>)
// with newlines to force proper block rendering in egui_commonmark.
pub fn normalize_code_blocks(markdown: &str) -> String {
    static RE_CODE_UNINDENT: OnceLock<Regex> = OnceLock::new();
    static RE_BQ_UNINDENT: OnceLock<Regex> = OnceLock::new();
    static RE_MATH_UNINDENT: OnceLock<Regex> = OnceLock::new();
    static RE_TABLE_UNINDENT: OnceLock<Regex> = OnceLock::new();
    static RE_TABLE_PRE_PAD: OnceLock<Regex> = OnceLock::new();
    static RE_TABLE_POST_PAD: OnceLock<Regex> = OnceLock::new();

    // NEW: Regexes for native LaTeX delimiters
    static RE_LATEX_DISPLAY: OnceLock<Regex> = OnceLock::new();
    static RE_LATEX_INLINE: OnceLock<Regex> = OnceLock::new();

    static RE_MATH_PAD: OnceLock<Regex> = OnceLock::new();

    // NEW: Regexes to extract and restore code blocks
    static RE_CODE_BLOCKS: OnceLock<Regex> = OnceLock::new();
    static RE_RESTORE: OnceLock<Regex> = OnceLock::new();

    // 1. Unindent Code Blocks (Run on raw markdown so backticks are clean)
    let re_code = RE_CODE_UNINDENT.get_or_init(|| Regex::new(r"(?m)^[ \t]+(`{3})").unwrap());
    let step1 = re_code.replace_all(markdown, "\n$1");

    // --- NEW: Shield Code Blocks ---
    // We extract all code blocks (both block and inline) and replace them
    // with a placeholder. This guarantees that table and math formatting
    // does not modify the content inside code blocks.
    let re_code_blocks = RE_CODE_BLOCKS.get_or_init(|| {
        Regex::new(r"(?s)(`{3}.*?`{3}|`[^`]*`)").unwrap()
    });

    let mut code_blocks = Vec::new();
    let step1_no_code = re_code_blocks.replace_all(&step1, |caps: &regex::Captures| {
        code_blocks.push(caps[0].to_string());
        format!("\x00CODEBLOCK_{}\x00", code_blocks.len() - 1)
    });

    // 2. Unindent Blockquotes
    let re_bq = RE_BQ_UNINDENT.get_or_init(|| Regex::new(r"(?m)^[ \t]+(>)").unwrap());
    let step2 = re_bq.replace_all(&step1_no_code, "$1");

    // 3. Unindent Display Math
    let re_math_un = RE_MATH_UNINDENT.get_or_init(|| Regex::new(r"(?m)^[ \t]+(\$\$)").unwrap());
    let step3 = re_math_un.replace_all(&step2, "$1");

    // 4. Unindent Tables
    let re_table_un = RE_TABLE_UNINDENT.get_or_init(|| Regex::new(r"(?m)^[ \t]+(\|)").unwrap());
    let step4 = re_table_un.replace_all(&step3, "$1");

    // 5. Ensure an empty line BEFORE tables
    let re_table_pre = RE_TABLE_PRE_PAD.get_or_init(|| {
        Regex::new(r"(?m)^([ \t]*[^|\s][^\n]*)\r?\n(\|)").unwrap()
    });
    let step5 = re_table_pre.replace_all(&step4, "$1\n\n$2");

    // 6. Ensure an empty line AFTER tables
    let re_table_post = RE_TABLE_POST_PAD.get_or_init(|| {
        Regex::new(r"(?m)^(\|.*)\r?\n([^|\r\n].*)").unwrap()
    });
    let step6 = re_table_post.replace_all(&step5, "$1\n\n$2");

    // Converts \[ ... \] to $$ ... $$
    let re_latex_disp = RE_LATEX_DISPLAY.get_or_init(|| Regex::new(r"(?s)\\\[(.*?)\\\]").unwrap());
    let step6_5 = re_latex_disp.replace_all(&step6, |caps: &regex::Captures| {
        // We trim() to safely remove leading/trailing spaces or newlines
        format!("$${}$$", caps[1].trim())
    });

    // Converts \( ... \) to $ ... $
    let re_latex_in = RE_LATEX_INLINE.get_or_init(|| Regex::new(r"(?s)\\\((.*?)\\\)").unwrap());
    let step6_6 = re_latex_in.replace_all(&step6_5, |caps: &regex::Captures| {
        // CRITICAL: .trim() removes spaces so `$ \sqrt{-4} $` becomes `$\sqrt{-4}$`
        // which completely satisfies strict Markdown inline math rules!
        format!("${}$", caps[1].trim())
    });

    // 7. Pad Display Math (Simplified since Code Blocks are safely hidden!)
    let re_math_pad = RE_MATH_PAD.get_or_init(|| {
        Regex::new(r"(\$\$(?s:.*?)\$\$)").unwrap()
    });

    let mut step7 = String::with_capacity(step6_6.len() + 128);
    let mut last_end = 0;

    for caps in re_math_pad.captures_iter(&step6_6) {
        let full_match = caps.get(0).unwrap();

        // Push everything before the matched part
        step7.push_str(&step6_6[last_end..full_match.start()]);

        let math = caps.get(1).unwrap();
        let before_match = &step6_6[..full_match.start()];
        let last_newline = before_match.rfind('\n').map(|i| i + 1).unwrap_or(0);
        let line_start = &before_match[last_newline..];

        // Inside a table row, padding would split the row into paragraphs
        // and break the table: leave the formula where it is.
        if line_start.trim_start().starts_with('|') {
            step7.push_str(full_match.as_str());
            last_end = full_match.end();
            continue;
        }

        let mut prefix = String::new();
        for c in line_start.chars() {
            if c == ' ' || c == '\t' || c == '>' {
                prefix.push(c);
            } else {
                break;
            }
        }

        if !prefix.contains('>') {
            prefix.clear();
        }

        let empty_prefix = prefix.trim_end();
        let m = math.as_str().trim_matches(|c| c == '\n' || c == '\r');

        let next_char = step6_6[full_match.end()..].chars().next();
        let append_prefix = match next_char {
            Some('\n') | Some('\r') | None => "",
            _ => prefix.as_str(),
        };

        let padded = format!("\n{empty_prefix}\n{prefix}{m}\n{empty_prefix}\n{append_prefix}");
        step7.push_str(&padded);

        last_end = full_match.end();
    }

    step7.push_str(&step6_6[last_end..]);

    // 8. Restore Code Blocks
    let re_restore = RE_RESTORE.get_or_init(|| Regex::new(r"\x00CODEBLOCK_(\d+)\x00").unwrap());
    let final_result = re_restore.replace_all(&step7, |caps: &regex::Captures| {
        if let Ok(idx) = caps[1].parse::<usize>() {
            if let Some(code) = code_blocks.get(idx) {
                return code.clone();
            }
        }
        caps[0].to_string() // Fallback
    });

    final_result.into_owned()
}

pub fn mk_msg(conn: &rusqlite::Connection, msg: &mut ChatMsg)
            -> rusqlite::Result<()> {
    // 1. Serialize fields
    let preset_json = msg.preset.as_ref().and_then(|p| p.to_json());

    // 2. Insert
    conn.execute(
        "INSERT INTO msg (
            role, content, reasoning, name, details, model_preset_json, model_preset_id
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        rusqlite::params![
            msg.msg_role,
            msg.content,
            msg.reasoning,
            msg.name,
            msg.details,
            preset_json,
            msg.preset_id
        ],
    )?;

    msg.id = conn.last_insert_rowid();
    msg.content = normalize_code_blocks(&msg.content);

    Ok(())
}

/// Saves a message's fields, all but its role: the only change of role is
/// `convert_cell_to_assistant`.
pub fn mod_msg(conn: &Connection, msg: &ChatMsg) -> Result<()> {
    // 1. Serialize the embedded preset
    let preset_json = msg.preset.as_ref().and_then(|p| p.to_json());

    // 2. Update the row
    conn.execute(
        "UPDATE msg
         SET content = ?1,
             reasoning = ?2,
             name = ?3,
             details = ?4,
             model_preset_json = ?5,
             model_preset_id = ?6
         WHERE id = ?7",
        params![
            msg.content,
            msg.reasoning,
            msg.name,
            msg.details,
            preset_json,
            msg.preset_id,
            msg.id
        ],
    )?;

    Ok(())
}

pub fn mod_msg_content(conn: &Connection, id: i64, content: &str) -> Result<()> {
    conn.execute(
        "UPDATE msg
         SET content = ?2
         WHERE id = ?1",
        params![id, content],
    )?;
    Ok(())
}

pub fn mod_msg_content_reasoning(conn: &Connection, id: i64, content: &str,
        reasoning: &str) -> Result<()> {
    conn.execute(
        "UPDATE msg
         SET content = ?2, reasoning = ?3
         WHERE id = ?1",
        params![id, content, reasoning],
    )?;
    Ok(())
}

pub fn delete_msg(conn: &Connection, msg_id: i64) -> rusqlite::Result<()> {
    conn.execute("DELETE FROM msg WHERE id = ?1", params![msg_id])?;
    Ok(())
}

/// Turns the note cell `msg_id` into an assistant message, for good: from
/// then on it's part of what models see. This is the only change of role
/// there is, and nothing ever becomes a cell. Returns false if `msg_id`
/// isn't a cell.
pub fn convert_cell_to_assistant(conn: &Connection, msg_id: i64) -> rusqlite::Result<bool> {
    let changed = conn.execute(
        "UPDATE msg SET role = ?1 WHERE id = ?2 AND role = ?3",
        params![
            crate::common::MsgRole::Assistant,
            msg_id,
            crate::common::MsgRole::Cell
        ],
    )?;
    Ok(changed == 1)
}

pub fn fetch_chat_titles(conn: &Connection) -> rusqlite::Result<Vec<DbChat>> {
    let mut stmt = conn.prepare(
        "select id, title, ts_created from chat where parent is null or
        parent = 0 order by ts_created desc"
    )?;

    let chat_iter = stmt.query_map([], |row| {
        Ok(DbChat {
            id: row.get(0)?,
            title: row.get(1)?,
        })
    })?;

    chat_iter.collect()
}

pub fn fetch_chat(conn: &Connection, chat_id: i64, presets: &ModelPresets)
        -> rusqlite::Result<Chat> {
    // ---------------------------------------------------------
    // 0. Fetch Chat Metadata (Title)
    // ---------------------------------------------------------
    // We do this first so we fail fast if the chat_id doesn't exist.
    let title: String = conn.query_row(
        "SELECT title FROM chat WHERE id = ?1",
        [chat_id],
        |row| row.get(0),
    )?;

    // ---------------------------------------------------------
    // 1. Fetch Chat Histories (aka Talkers)
    // We expect 'msg_ids' column to store JSON: "[1, 2, 3]"
    let mut stmt_talker = conn.prepare(
        "SELECT talker_ind, msg_ids, id, model_preset_id, model_preset_json,
                name, muted, hidden, deleted
         FROM talker
         WHERE chat_id = ?1
         ORDER BY talker_ind ASC"
    )?;

    let talker_iter = stmt_talker.query_map([chat_id], |row| {
        let talker_ind: usize = row.get::<_, i64>(0)? as usize;
        let json_str: String = row.get(1).unwrap_or_else(|_| "[]".to_string());
        let msg_ids: Vec<i64> = serde_json::from_str(&json_str)
                .unwrap_or_default();
        let id: i64 = row.get(2)?;
        let preset_id: i64 = row.get(3)?;
        let preset_json_raw: Option<String> = row.get(4)?;
        let preset_snapshot = ModelPreset::from_json(preset_json_raw.as_ref());
        let name: String = row.get(5)?;
        let muted: bool = row.get(6)?;
        let hidden: bool = row.get(7)?;
        let deleted: bool = row.get(8)?;

    Ok(Talker {
            id,
            talker_ind,
            msg_ids,
            preset_selection: ModelPresetSelection::from_id(preset_id, presets),
            preset: preset_snapshot, // <--- Populated from DB
            name,
            muted,
            hidden,
            deleted
        })
    })?;

    let mut talkers = Vec::new();
    let mut all_msg_ids = Vec::new();

    for talker_res in talker_iter {
        let talker = talker_res?;
        if talker.talker_ind == 0 {
            all_msg_ids = talker.msg_ids.clone();
        }

        talkers.push(talker);
    }

    // ---------------------------------------------------------
    // 2. Fetch Messages into HashMap (Only for talker_ind == 0)
    // ---------------------------------------------------------
    let mut msg_pool = HashMap::new();

    if !all_msg_ids.is_empty() {
        // Create placeholders: "?, ?, ?"
        let placeholders = vec!["?"; all_msg_ids.len()].join(",");

        let sql = format!(
            "SELECT id, role, content, name, reasoning, details, model_preset_json,
                    model_preset_id
             FROM msg
             WHERE id IN ({})",
            placeholders
        );

        let mut stmt_msgs = conn.prepare(&sql)?;

        // Convert Vec<i64> -> Vec<&dyn ToSql> for rusqlite
        let params: Vec<&dyn rusqlite::ToSql> = all_msg_ids
            .iter()
            .map(|id| id as &dyn rusqlite::ToSql)
            .collect();

        let msgs_iter = stmt_msgs.query_map(&*params, |row| {
            // Load the Preset Snapshot for the message
            let p_json: Option<String> = row.get(6)?;
            let msg_preset = ModelPreset::from_json(p_json.as_ref());

            Ok(ChatMsg {
                id: row.get(0)?,
                msg_role: row.get(1)?,
                content: row.get(2)?,
                name: row.get(3)?,
                reasoning: row.get(4)?,
                details: row.get(5)?,
                preset: msg_preset,
                preset_id: row.get(7)?,
                ..Default::default()
            })
        })?;

        for msg_res in msgs_iter {
            let mut msg = msg_res?;
            // clean up the input immediately:
            msg.content = normalize_code_blocks(&msg.content);
            msg_pool.insert(msg.id, msg);
        }
    }

    Ok(Chat {
        id: chat_id,
        title,
        msg_pool,
        talkers,
        draft_note: String::new(),
        draft_is_rhai: false,
    })

}

pub fn mod_chat_title(conn: &Connection, chat_id: i64,  new_title: &str)
        -> rusqlite::Result<()> {
    let clean_title = new_title.split('\n').next().unwrap_or(new_title).trim();
    conn.execute("update chat set title = ?1 where id = ?2",
        params![clean_title, chat_id])?;
    Ok(())
}

pub fn delete_chat(conn: &Connection, chat_id: i64) -> rusqlite::Result<()> {
    conn.execute("delete from chat where id = ?1", params![chat_id])?;
    Ok(())
}

pub fn delete_model_preset(conn: &Connection, id: i64) ->
        rusqlite::Result<()> {
    conn.execute("delete from model_preset where id = ?1", params![id])?;
    Ok(())
}

// upsert the model preset
pub fn save_model_preset(conn: &Connection, entry: &mut ModelPreset) ->
            rusqlite::Result<i64> {
    let options_json = serde_json::to_string(&entry.options)
        .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
    if entry.id == 0 {
        // 0 means that the preset is brand new, we need to insert it
        conn.execute(
            "INSERT INTO model_preset (title, tooltip, chat_router, model, options)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                entry.title,
                entry.tooltip,
                &entry.chat_router, // CLEANER: Pass directly, ToSql handles the string conversion
                entry.model,
                options_json
            ]
        )?;
        Ok(conn.last_insert_rowid())
    } else {
        // the preset is already in the database, we just need to update it
        let changes = conn.execute(
            "UPDATE model_preset
             SET title = ?2,
                 tooltip = ?3,
                 chat_router = ?4,
                 model = ?5,
                 options = ?6,
                 ts_modified = current_timestamp
             WHERE id = ?1",
            params![
                entry.id,
                entry.title,
                entry.tooltip,
                &entry.chat_router, // CLEANER: Pass directly
                entry.model,
                options_json
            ]
        )?;
        if changes == 0 {
            Ok(0) // special result meaning that no update was made
        } else {
            Ok(entry.id)
        }
    }
}

pub fn load_model_presets_vec(conn: &Connection)
        -> rusqlite::Result<Vec<ModelPreset>> {
    let mut stmt = conn.prepare(
        "select id, title, tooltip, chat_router, model, options, deleted
        from model_preset order by title"
    )?;

    let rows = stmt.query_map([], |row| {
        Ok(ModelPreset {
            id: row.get(0)?,
            title: row.get(1)?,
            tooltip: row.get(2)?,
            chat_router: row.get(3)?,
            model: row.get(4)?,
            options: serde_json::from_str(&row.get::<_, String>(5)?)
                    .unwrap_or_default(),
            deleted: row.get(6)?,
            ..Default::default()
        })
    })?;

    let mut results = Vec::new();
    for row in rows {
        results.push(row?);
    }

    Ok(results)
}

/// Helper to extract a grep-like context snippet surrounding the keyword
pub fn extract_snippet(text: &str, keyword: &str) -> String {
    let text_lower = text.to_lowercase();
    let kw_lower = keyword.to_lowercase();

    if let Some(idx) = text_lower.find(&kw_lower) {
        // Grab roughly 40 chars before and 80 after
        let start = idx.saturating_sub(40);
        let end = (idx + keyword.len() + 80).min(text.len());

        // Snap to valid UTF-8 character boundaries
        let mut start_idx = start;
        while start_idx > 0 && !text.is_char_boundary(start_idx) {
            start_idx -= 1;
        }
        let mut end_idx = end;
        while end_idx < text.len() && !text.is_char_boundary(end_idx) {
            end_idx += 1;
        }

        let mut snippet = text[start_idx..end_idx].replace('\n', " ").replace('\r', "");
        if start_idx > 0 {
            snippet.insert_str(0, "...");
        }
        if end_idx < text.len() {
            snippet.push_str("...");
        }
        snippet
    } else {
        // Fallback: just return the beginning of the string
        let end = text.len().min(120);
        let mut end_idx = end;
        while end_idx < text.len() && !text.is_char_boundary(end_idx) {
            end_idx += 1;
        }
        let mut snippet = text[..end_idx].replace('\n', " ").replace('\r', "");
        if end_idx < text.len() {
            snippet.push_str("...");
        }
        snippet
    }
}

/// Who a search is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchScope {
    /// The user, looking through their chats: every message, cells included.
    User,
    /// Anything whose results a model will see: notebook cells are left
    /// out, as everywhere else a model is concerned (`WireRole`).
    Model,
}

/// Searches the messages `scope` allows for `keyword` and returns the chats
/// containing them, one result per chat.
pub fn search_chats(conn: &Connection, keyword: &str, scope: SearchScope) -> rusqlite::Result<Vec<crate::common::SearchResult>> {
    let scope_filter = match scope {
        SearchScope::User => String::new(),
        SearchScope::Model => format!(" AND m.role != '{}'", crate::common::MsgRole::Cell.db_code()),
    };
    // We use json_each to unpack the msg_ids JSON array directly in SQLite
    let mut stmt = conn.prepare(&format!(
        "SELECT DISTINCT c.id, c.title, m.content, c.ts_created
         FROM msg m
         JOIN talker t ON EXISTS (SELECT 1 FROM json_each(t.msg_ids) WHERE value = m.id)
         JOIN chat c ON c.id = t.chat_id
         WHERE m.content LIKE ?1{}
         ORDER BY c.ts_created DESC",
        scope_filter
    ))?;

    let kw_param = format!("%{}%", keyword);
    let rows = stmt.query_map(rusqlite::params![kw_param], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?
        ))
    })?;

    let mut unique_results = Vec::new();
    let mut seen = std::collections::HashSet::new();

    for row in rows {
        let (chat_id, title, content) = row?;

        // We only want one snippet per chat to keep the list clean
        if !seen.contains(&chat_id) {
            seen.insert(chat_id);
            let clean_title = title.split('\n').next().unwrap_or(&title).trim().to_string();
            unique_results.push(crate::common::SearchResult {
                chat_id,
                chat_title: clean_title,
                snippet: extract_snippet(&content, keyword),
            });
        }
    }

    Ok(unique_results)
}

/// The `msg` table. `name` lets the v2 migration build it under a temporary
/// name. Roles are stored by their codes (`MsgRole::db_code`).
pub(crate) fn msg_table_sql(name: &str) -> String {
    format!("create table {name} (
        id integer primary key autoincrement,
        content text not null,
        reasoning text,
        role text not null check(role in ('usr', 'asst', 'sys', 'dev',
            'tool', 'cell')),
        name text,
        details text,
        model_preset_id integer not null default 0,
        model_preset_json text,
        prompt_tokens integer,
        completion_tokens integer,
        cost real,
        ts_created datetime default current_timestamp
    );")
}

/// Puts a default model preset back whenever the last one is deleted.
pub(crate) const MODEL_PRESET_NOT_EMPTY_TRIGGER: &str =
    "create trigger ensure_model_preset_not_empty_trigger
        after delete on model_preset
        when not exists (select 1 from model_preset)
        begin
            insert into model_preset (title, tooltip, chat_router, model, options)
            values ('DeepSeek: R1 0528 (free)', '',
                'Openrouter',
                'deepseek/deepseek-r1-0528:free',
                '');
        end;";

fn create_database_schema(conn: &Connection) -> rusqlite::Result<()> {
    // we keep a history of all the messages in this database for easy access
    conn.execute_batch(&msg_table_sql("msg"))?;
    // chats contain messages
    conn.execute("create table chat (
        id integer primary key autoincrement,
        parent integer,
        title text,
        ts_created datetime default current_timestamp,
        ts_modified datetime default current_timestamp
    );", ())?;
    // . create the Loop-Safe Trigger that will update ts_modified on any update
    conn.execute(
        "CREATE TRIGGER update_chat_timestamp
         AFTER UPDATE ON chat
         FOR EACH ROW
         WHEN NEW.ts_modified IS OLD.ts_modified
         BEGIN
             UPDATE chat SET ts_modified = CURRENT_TIMESTAMP WHERE id = NEW.id;
         END;",
        (),
    )?;
    // a talker's view of a chat: which messages it sees, and its model preset
    conn.execute("create table talker (
        id integer primary key autoincrement,
        name text not null default '',
        chat_id integer not null,
        talker_ind integer not null,
        msg_ids text default '[]',
        model_preset_id integer not null default 0,
        model_preset_json text,
        muted integer not null default 0,
        hidden integer not null default 0,
        deleted integer not null default 0,
        ts_created datetime default current_timestamp,
        unique(chat_id, talker_ind),
        foreign key (chat_id) references chat(id) on delete cascade
    );", ())?;
    // this version is needed to update older tables to newer versions
    conn.execute("create table schema_version (
        id integer primary key check (id = 0),
        name text,
        version int,
        applied_on datetime default current_timestamp
    );", ())?;
    // model presets are for ease of use, to fine-tune settings and keep them handy
    conn.execute("create table model_preset (
        id integer primary key autoincrement,
        title text not null unique,
        tooltip text,
        chat_router text,
        model text,
        options text,
        hidden integer not null default 0,
        deleted integer not null default 0,
        ts_created datetime default current_timestamp,
        ts_modified datetime default current_timestamp
    )", ())?;
    conn.execute_batch(MODEL_PRESET_NOT_EMPTY_TRIGGER)?;
    // fire the trigger once so that the default model preset is added:
    conn.execute("insert into model_preset (id, title) values (0, 'temporary')", ())?;
    conn.execute("delete from model_preset", ())?;

    conn.execute("insert into schema_version (id, name, version)
        values (0, 'inforno_main_db', ?1);",
        (CURRENT_FOLIO_VERSION,))?;
/*
    mk_chat(conn, "Welcome to Inforno!")?;

    let usr_msg_id = mk_msg(conn,
        &t!("how_to_use"),
        "user", None, None, None,
    )?;

    let answer_msg_id = mk_msg(conn,
        &t!("welcome_tour"),
        "assistant", None, None, None,
    )?;

    mod_agent_msgs(conn, 1, &[1, 2])?;
    mod_agent_preset(conn, 2, 1)?;
*/
    Ok(())
}

/// Exports a chat as an executable Notebook in .ron format
pub fn export_notebook_to_ron(conn: &Connection, chat_id: i64, presets: &ModelPresets) -> rusqlite::Result<String> {
    let chat = fetch_chat(conn, chat_id, presets)?;
    let mut cells = Vec::new();

    // Export the linear timeline from Omnis (talker 0)
    if let Some(omnis) = chat.talkers.first() {
        for msg_id in &omnis.msg_ids {
            if let Some(msg) = chat.msg_pool.get(msg_id) {
                cells.push(crate::common::NotebookCellExport {
                    role: msg.msg_role.clone(),
                    content: msg.content.clone(),
                });
            }
        }
    }

    let export = crate::common::NotebookExportV2 {
        version: "2.0".to_string(),
        title: chat.title,
        cells,
    };

    Ok(ron::ser::to_string_pretty(&export, ron::ser::PrettyConfig::default()).unwrap_or_default())
}

/// Exports a chat to a Markdown-formatted string.
pub fn export_chat_to_markdown(conn: &Connection, chat_id: i64, presets: &ModelPresets) -> rusqlite::Result<String> {
    let chat = fetch_chat(conn, chat_id, presets)?;
    let mut md = String::new();
    md.push_str(&format!("# {}\n\n", chat.title));

    if let Some(omnis) = chat.talkers.first() {
        for msg_id in &omnis.msg_ids {
            if let Some(msg) = chat.msg_pool.get(msg_id) {
                let role_label = match msg.msg_role {
                    crate::common::MsgRole::User => "User",
                    crate::common::MsgRole::Assistant => {
                        msg.name.as_deref().unwrap_or("Assistant")
                    },
                    crate::common::MsgRole::System => "System",
                    crate::common::MsgRole::Developer => "Developer",
                    crate::common::MsgRole::Tool => "Tool",
                    crate::common::MsgRole::Cell => "Note",
                };

                md.push_str(&format!("## {}\n\n", role_label));

                if let Some(reasoning) = &msg.reasoning {
                    if !reasoning.is_empty() {
                        md.push_str("<details>\n<summary>Thought Process</summary>\n\n");
                        md.push_str(reasoning);
                        md.push_str("\n\n</details>\n\n");
                    }
                }

                md.push_str(&msg.content);
                md.push_str("\n\n---\n\n");
            }
        }
    }

    Ok(md)
}

#[cfg(test)]
mod tests {
    use std::fmt::Write;

    use super::*;
    use rusqlite::Connection;

    // Helper to create an in-memory DB and apply the schema
    fn setup_db() -> Connection {
        let conn = Connection::open_in_memory().expect("failed to open in-memory db");
        create_database_schema(&conn).expect("failed to create schema");
        conn
    }

    // Prints the talker table, one row per talker
    fn print_talker_table(conn: &Connection, buffer: &mut impl Write) {
        let _ = writeln!(buffer,
            "id   | chat_id  | talker_ind | name            | msg_ids");

        let mut stmt = conn.prepare(
            "SELECT id, chat_id, talker_ind, name, msg_ids FROM talker")
            .unwrap();

        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,    // id
                row.get::<_, i64>(1)?,    // chat_id
                row.get::<_, i64>(2)?,    // talker_ind
                row.get::<_, String>(3)?, // name
                row.get::<_, String>(4)?, // msg_ids
            ))
        }).unwrap();

        for row in rows {
            let (id, chat_id, talker_ind, name, msg_ids) = row.unwrap();
            let _ = writeln!(buffer, "{:<4} | {:<8} | {:<10} | {:<15} | {}",
                    id, chat_id, talker_ind, name, msg_ids);
        }
    }

    #[test]
    fn test_talkers() {
        let conn = setup_db();

        // A new chat gets Omnis (hidden, holds every message) and Talker1
        let mut chat = crate::common::Chat::default();
        mk_chat(&conn, &mut chat).expect("failed to create chat");

        let mut output = String::new();
        print_talker_table(&conn, &mut output);

        let expected = "\
id   | chat_id  | talker_ind | name            | msg_ids
1    | 1        | 0          | Omnis           | []
2    | 1        | 1          | Talker1         | []
";
        assert_eq!(output, expected);
    }

}
