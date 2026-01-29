use std::{collections::HashMap, fs, path::PathBuf, sync::OnceLock};
use regex::Regex;
use rusqlite::{Connection, params, Result, ffi};
use directories::ProjectDirs;
use rust_i18n::t;
use crate::common::{Agent, Chat, ChatMsg, DbChat, MyError, Preset, PresetSelection, Presets};

pub mod cache;

pub const CURRENT_SANDBOX_VERSION: i32 = 2;

pub fn get_sandbox_db_conn(sandbox: &Option<PathBuf>) ->
            Result<(Connection, PathBuf), MyError> {
    if let Some(sandbox) = sandbox {
        let conn = connect_sandbox_db(sandbox)?;
        return Ok((conn, sandbox.clone()));
    }

    if let Some(proj_dirs) = ProjectDirs::from(
            "", "", "inforno") {
        let mut file_path_buf = PathBuf::from(proj_dirs.data_dir());
        if !file_path_buf.is_dir() {
            println!("Directory {} does not exist. Trying to create it...",
                file_path_buf.display());
            fs::create_dir_all(&file_path_buf).map_err(|_| MyError::ProjectDir)?;
        }
        file_path_buf.push("info.rno");

        let conn = connect_sandbox_db(&file_path_buf)?;
        return Ok((conn, file_path_buf));
    }
    Err(MyError::ProjectDir)
}

fn connect_sandbox_db(sandbox: &PathBuf) -> Result<Connection, MyError> {
    let conn = Connection::open(sandbox)?;
    conn.pragma_update(None, "foreign_keys", "ON")?;

    if table_exists(&conn, "schema_version")? {
        // Table exists, check the version
        let current_version: i32 = conn.query_row(
            "SELECT version FROM schema_version",
            [],
            |row| row.get(0)
        )?;

        if current_version != CURRENT_SANDBOX_VERSION {
            // Version mismatch
            return Err(MyError::SandboxVersionMismatch(
                CURRENT_SANDBOX_VERSION,
                current_version));
        }

        println!("Main Database schema exists and is version 2.");
        return Ok(conn);
    } else {
        // Schema does not exist. Create new database schema.
        println!("Main Database schema does not exist. Creating new one...");
        create_database_schema(&conn)?;

        return Ok(conn);
    }
}

pub fn reset_sandbox_db(conn: &Connection) -> rusqlite::Result<()> {
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
    // Transactions are crucial here so you don't end up with a Chat that has no Agents
    // if something fails halfway through.
    conn.execute("BEGIN", [])?;

    let mut run_transaction = || -> rusqlite::Result<()> {
        // 2. Insert Chat Row
        conn.execute("INSERT INTO chat (title) VALUES (?1)", [&chat.title])?;

        // Update the Chat struct immediately
        chat.id = conn.last_insert_rowid();

        // 3. Persist Agents
        // We iterate through the agents already present in the struct.
        // This is perfect for "cloning" a chat or creating a new default one.
        for agent in &mut chat.agents {
            // We delegate to the mutating mk_agent function.
            // This handles serializing msg_ids, preset_json, and updating agent.id.
            mk_agent(conn, chat.id, agent)?;
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
            // If anything fails (e.g., unique constraint on agent_ind), roll back everything.
            let _ = conn.execute("ROLLBACK", []);
            Err(e)
        }
    }
}

pub fn mk_agent(
    conn: &rusqlite::Connection,
    chat_id: i64,
    agent: &mut Agent
) -> rusqlite::Result<()> {

    // 1. Serialize
    let msg_ids_json = serde_json::to_string(&agent.msg_ids).unwrap_or_else(|_| "[]".into());
    let preset_json = agent.preset.as_ref().and_then(|p| p.to_json());

    // 2. Insert
    conn.execute(
        "INSERT INTO agent (
            chat_id,
            agent_ind,
            name,
            msg_ids,
            preset_id,
            preset_json,
            muted,
            hidden,
            deleted
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        rusqlite::params![
            chat_id,
            agent.agent_ind as i64,
            agent.name,
            msg_ids_json,
            agent.preset_selection.id,
            preset_json,
            agent.muted,
            agent.hidden,
            agent.deleted
        ],
    )?;

    // 3. Update the Struct ID immediately
    agent.id = conn.last_insert_rowid();

    Ok(())
}

/// Updates an EXISTING agent using its unique database row ID.
/// (Use this when you have the 'id' field from the Agent struct)
pub fn mod_agent_msgs(conn: &Connection, id: i64, msg_ids: &[i64])
        -> rusqlite::Result<()> {
    let json_ids = serde_json::to_string(msg_ids)
            .unwrap_or_else(|_| "[]".to_string());

    conn.execute(
        "UPDATE agent
         SET msg_ids = ?1
         WHERE id = ?2",
        (json_ids, id),
    )?;
    Ok(())
}

/// Updates an agent's base preset selection and preset_snapshot, of course
pub fn mod_agent_preset(
    conn: &Connection,
    id: i64,
    preset_id: i64,
    preset_snapshot: Option<&Preset>
) -> Result<()> {

    // Serialize (or get None)
    let json = preset_snapshot.and_then(|p| p.to_json());

    conn.execute(
        "UPDATE agent
         SET preset_id = ?1, preset_json = ?2
         WHERE id = ?3",
        params![preset_id, json, id],
    )?;
    Ok(())
}

// only update snapshot - when tweaking parameters in the bottom_panel
pub fn update_agent_preset_snapshot(
    conn: &Connection,
    agent_id: i64,
    preset: Option<&Preset>
) -> rusqlite::Result<()> {

    let json = preset.and_then(|p| p.to_json());

    conn.execute(
        "UPDATE agent SET preset_json = ?1 WHERE id = ?2",
        rusqlite::params![json, agent_id]
    )?;

    Ok(())
}

// some LLMs produce code blocks where the initial triple tick is
// indented. CommonMark creates squeezed boxes instead of a normal full-sized
// box, which is annoying. This fix applies a regexp to remove any
// indentation from triple backticks. In the database, we store verbatim original
// data not to introduce any unforeseen issues.
fn normalize_code_blocks(markdown: &str) -> String {
    // Regex explanation:
    // (?m) : Enable multiline mode (so ^ matches start of line).
    // ^    : Start of a line.
    // \s+  : One or more whitespace characters (the indentation we want to remove).
    // (```): Capture group 1 - the triple backticks (and potentially language tag).
    //
    // We replace the whole match with just the capture group ($1) and a preceding newline.

    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"(?m)^[ \t]+(```)").unwrap());

    // Replace "   ```" with "\n```"
    // The \n ensures the block breaks out of previous paragraphs cleanly.
    re.replace_all(markdown, "\n$1").to_string()
}

pub fn mk_msg(conn: &rusqlite::Connection, msg: &mut ChatMsg)
            -> rusqlite::Result<()> {
    // 1. Serialize fields
    let preset_json = msg.preset.as_ref().and_then(|p| p.to_json());

    // 2. Insert
    conn.execute(
        "INSERT INTO msg (
            role, content, reasoning, name, details, preset_json, preset_id
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        rusqlite::params![
            msg.msg_role.to_string(),
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

pub fn mod_msg(conn: &Connection, msg: &ChatMsg) -> Result<()> {
    // 1. Serialize the embedded preset
    let preset_json = msg.preset.as_ref().and_then(|p| p.to_json());

    // 2. Convert role to string (assuming MsgRole implements ToString or into)
    // If MsgRole is an enum, ensure you convert it to "user"/"assistant"/etc.
    let role_str = msg.msg_role.to_string();

    // 3. Update the row
    conn.execute(
        "UPDATE msg
         SET content = ?1,
             reasoning = ?2,
             role = ?3,
             name = ?4,
             details = ?5,
             preset_json = ?6,
             preset_id = ?7
         WHERE id = ?8",
        params![
            msg.content,
            msg.reasoning,
            role_str,
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

pub fn fetch_chat(conn: &Connection, chat_id: i64, presets: &Presets)
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
    // 1. Fetch Chat Histories (aka Agents)
    // We expect 'msg_ids' column to store JSON: "[1, 2, 3]"
    let mut stmt_agent = conn.prepare(
        "SELECT agent_ind, msg_ids, id, preset_id, preset_json,
                name, muted, hidden, deleted
         FROM agent
         WHERE chat_id = ?1
         ORDER BY agent_ind ASC"
    )?;

    let agent_iter = stmt_agent.query_map([chat_id], |row| {
        let agent_ind: usize = row.get::<_, i64>(0)? as usize;
        let json_str: String = row.get(1).unwrap_or_else(|_| "[]".to_string());
        let msg_ids: Vec<i64> = serde_json::from_str(&json_str)
                .unwrap_or_default();
        let id: i64 = row.get(2)?;
        let preset_id: i64 = row.get(3)?;
        let preset_json_raw: Option<String> = row.get(4)?;
        let preset_snapshot = Preset::from_json(preset_json_raw.as_ref());
        let name: String = row.get(5)?;
        let muted: bool = row.get(6)?;
        let hidden: bool = row.get(7)?;
        let deleted: bool = row.get(8)?;

    Ok(Agent {
            id,
            agent_ind,
            msg_ids,
            preset_selection: PresetSelection::from_id(preset_id, presets),
            preset: preset_snapshot, // <--- Populated from DB
            name,
            muted,
            hidden,
            deleted
        })
    })?;

    let mut agents = Vec::new();
    let mut all_msg_ids = Vec::new();

    for agent_res in agent_iter {
        let agent = agent_res?;
        if agent.agent_ind == 0 {
            all_msg_ids = agent.msg_ids.clone();
        }

        agents.push(agent);
    }

    // ---------------------------------------------------------
    // 2. Fetch Messages into HashMap (Only for agent_ind == 0)
    // ---------------------------------------------------------
    let mut msg_pool = HashMap::new();

    if !all_msg_ids.is_empty() {
        // Create placeholders: "?, ?, ?"
        let placeholders = vec!["?"; all_msg_ids.len()].join(",");

        let sql = format!(
            "SELECT id, role, content, name, reasoning, details, preset_json,
                    preset_id
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
            let msg_preset = Preset::from_json(p_json.as_ref());

            Ok(ChatMsg {
                id: row.get(0)?,
                msg_role: row.get::<_, String>(1)?.as_str().into(),
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
        agents,
    })

}

pub fn mod_chat_title(conn: &Connection, chat_id: i64,  new_title: &str)
        -> rusqlite::Result<()> {
    conn.execute("update chat set title = ?1 where id = ?2",
        params![new_title, chat_id])?;
    Ok(())
}

pub fn delete_chat(conn: &Connection, chat_id: i64) -> rusqlite::Result<()> {
    conn.execute("delete from chat where id = ?1", params![chat_id])?;
    Ok(())
}

pub fn delete_preset(conn: &Connection, id: i64) ->
        rusqlite::Result<()> {
    conn.execute("delete from preset where id = ?1", params![id])?;
    Ok(())
}

// upsert the preset
pub fn save_preset(conn: &Connection, entry: &mut Preset) ->
            rusqlite::Result<i64> {
    let options_json = serde_json::to_string(&entry.options)
        .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
    if entry.id == 0 {
        // 0 means that the preset is brand new, we need to insert it
        conn.execute(
            "INSERT INTO preset (title, tooltip, chat_router, model, options)
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
            "UPDATE preset
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

pub fn load_presets_vec(conn: &Connection)
        -> rusqlite::Result<Vec<Preset>> {
    let mut stmt = conn.prepare(
        "select id, title, tooltip, chat_router, model, options, deleted
        from preset order by title"
    )?;

    let rows = stmt.query_map([], |row| {
        Ok(Preset {
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

fn create_database_schema(conn: &Connection) -> rusqlite::Result<()> {
    // we keep a history of all the messages in this database for easy access
    conn.execute("create table msg (
        id integer primary key autoincrement,
        content text not null,
        reasoning text,
        role text not null check(role in ('user', 'assistant', 'system',
            'developer', 'tool')),
        name text,
        details text,
        preset_id integer not null default 0,
        preset_json text,
        prompt_tokens integer,
        completion_tokens integer,
        cost real,
        ts_created datetime default current_timestamp
    );", ())?;
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
    // a chat message can be shared between different chats
    conn.execute("create table agent (
        id integer primary key autoincrement,
        name text not null default '',
        chat_id integer not null,
        agent_ind integer not null,
        msg_ids text default '[]',
        preset_id integer not null default 0,
        preset_json text,
        muted integer not null default 0,
        hidden integer not null default 0,
        deleted integer not null default 0,
        ts_created datetime default current_timestamp,
        unique(chat_id, agent_ind),
        foreign key (chat_id) references chat(id) on delete cascade
    );", ())?;
    // this version is needed to update older tables to newer versions
    conn.execute("create table schema_version (
        id integer primary key check (id = 0),
        name text,
        version int,
        applied_on datetime default current_timestamp
    );", ())?;
    // presets are for ease of use, to fine-tune settings ane keep them handy
    conn.execute("create table preset (
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
    // create a trigger that will add a "default" preset if all were deleted
    conn.execute("create trigger ensure_preset_not_empty_trigger
        after delete on preset
        when not exists (select 1 from preset)
        begin
            insert into preset (title, tooltip, chat_router, model, options)
            values ('DeepSeek: R1 0528 (free)', '',
                'Openrouter',
                'deepseek/deepseek-r1-0528:free',
                '');
        end;
    ", ())?;
    // trigger that event so that the defualt preset is added:
    conn.execute("insert into preset (id, title) values (0, 'temporary')", ())?;
    conn.execute("delete from preset", ())?;

    conn.execute("insert into schema_version (id, name, version)
        values (0, 'inforno_main_db', ?1);",
        (CURRENT_SANDBOX_VERSION,))?;
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

    // Helper to print table content
    fn print_agent_table(conn: &Connection, buffer: &mut impl Write) {
        // Adjusted widths to fit msg_ids
        let _ = writeln!(buffer,
            "id   | chat_id  | agent_ind| name            | msg_ids");

        let mut stmt = conn.prepare(
            "SELECT id, chat_id, agent_ind, name, msg_ids FROM agent")
            .unwrap();

        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,    // id
                row.get::<_, i64>(1)?,    // chat_id
                row.get::<_, i64>(2)?,    // agent_ind
                row.get::<_, String>(3)?, // name
                row.get::<_, String>(4)?, // msg_ids (New Column)
            ))
        }).unwrap();

        for row in rows {
            let (id, chat_id, agent_ind, name, msg_ids) = row.unwrap();
            let _ = writeln!(buffer, "{:<4} | {:<8} | {:<8} | {:<15} | {}",
                    id, chat_id, agent_ind, name, msg_ids);
        }
    }

    #[test]
    fn test_agents() {
/*        let conn = setup_db();

        // create chats
        mk_chat(&conn, "What is the ultimate answer?").expect("failed to create chat");

        let mut output = String::new();
        print_agent_table(&conn, &mut output);
        print!("{}", output);

        let expected = "\
id   | chat_id  | agent_ind| name            | msg_ids
1    | 1        | 0        | Omnis           | []
";
        assert_eq!(output, expected);

        let _ = mk_agent(&conn, 1, "", &[], 0);
        output = String::new();
        print_agent_table(&conn, &mut output);
        print!("{}", output);
*/
    }

}