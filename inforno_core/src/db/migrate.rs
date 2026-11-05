//! Converting a version 2 Folio (tables `agent` and `preset`, long role
//! names) to the current version 3.
//!
//! The original is never written to. It's copied with `VACUUM INTO`, the
//! copy is converted in one transaction, and only then is the original
//! renamed to `<name>.sandbox-v2.bak` and the copy put in its place.

use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags, OptionalExtension};

use crate::common::MyError;
use super::{
    folio_path_for, folios_dir, home_folio_file, msg_table_sql, schema_version,
    MODEL_PRESET_NOT_EMPTY_TRIGGER,
};

/// Each v2 role string and its v3 code. In v2, `developer` only ever meant
/// a note cell, so it becomes `cell`, not `dev`.
const ROLE_CODES: &[(&str, &str)] = &[
    ("user", "usr"),
    ("assistant", "asst"),
    ("system", "sys"),
    ("developer", "cell"),
    ("tool", "tool"),
];

fn failed(why: impl Into<String>) -> MyError {
    MyError::FolioConvert(why.into())
}

fn canonical(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Whether `a` and `b` name the same file. Compares the directories
/// canonically, so it works for a file that doesn't exist (any more).
pub fn same_file(a: &Path, b: &Path) -> bool {
    a == b
        || (a.file_name() == b.file_name()
            && match (a.parent(), b.parent()) {
                (Some(x), Some(y)) => canonical(x) == canonical(y),
                _ => false,
            })
}

/// `path` with `suffix` added to its file name: `x.rno` → `x.rno-wal`.
fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

/// Where "Move into folios" puts a converted Folio: `<stem>.rno` in the
/// folios directory, or `<stem>-2.rno`, `<stem>-3.rno`... if that's taken.
/// A project's `.inforno/info.rno` is named after the project directory.
/// None when the Folio stays where it is: it's already in the folios
/// directory, or it's the Home Folio, which Inforno opens from its fixed
/// place.
pub fn conversion_destination(src: &Path) -> Option<PathBuf> {
    let dir = folios_dir()?;
    let parent = src.parent()?;
    if canonical(parent) == canonical(&dir)
        || home_folio_file().is_some_and(|home| same_file(&home, src))
    {
        return None;
    }

    let is_project_folio = src.file_name().is_some_and(|n| n == "info.rno")
        && parent.file_name().is_some_and(|n| n == ".inforno");
    let stem = if is_project_folio {
        parent.parent().and_then(|p| p.file_name()).and_then(|n| n.to_str())
    } else {
        src.file_stem().and_then(|s| s.to_str())
    }
    .filter(|s| folio_path_for(s).is_ok())
    .unwrap_or("folio");

    (1..=999u32).find_map(|n| {
        let name = if n == 1 { stem.to_string() } else { format!("{}-{}", stem, n) };
        let path = folio_path_for(&name).ok()?;
        (!path.exists()).then_some(path)
    })
}

/// Where the original is kept once its conversion succeeds: next to it, as
/// `<name>.sandbox-v2.bak`, or `<name>.sandbox-v2-2.bak`... if that's taken.
pub fn legacy_backup_path(src: &Path) -> PathBuf {
    let name = src
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "folio.rno".to_string());
    (1u32..)
        .map(|n| {
            if n == 1 {
                format!("{}.sandbox-v2.bak", name)
            } else {
                format!("{}.sandbox-v2-{}.bak", name, n)
            }
        })
        .map(|file| src.with_file_name(file))
        .find(|p| !p.exists())
        .expect("some backup name is free")
}

/// Converts the version 2 Folio `src` and puts the result at `dest`, which
/// may be `src` itself to convert it where it is. Returns where the
/// original was kept. On any failure the original stays as it was.
pub fn convert_legacy_folio(src: &Path, dest: &Path) -> Result<PathBuf, MyError> {
    if !same_file(src, dest) && dest.exists() {
        return Err(failed(format!("{} already exists", dest.display())));
    }
    // A journal or WAL file means the Folio wasn't closed cleanly, and
    // SQLite would apply it to whatever file ends up at that path.
    for side in ["-journal", "-wal"] {
        let side_file = with_suffix(src, side);
        if side_file.exists() {
            return Err(failed(format!(
                "{} exists, so the Folio wasn't closed cleanly. Open it once with the \
                 sqlite3 tool (or the previous Inforno) so SQLite can recover it, then convert it.",
                side_file.display()
            )));
        }
    }

    if let Some(dir) = dest.parent() {
        fs::create_dir_all(dir).map_err(|_| MyError::FolioDir)?;
    }
    let tmp = with_suffix(dest, ".converting");
    let _ = fs::remove_file(&tmp); // left over from an interrupted conversion

    if let Err(e) = copy_and_migrate(src, &tmp) {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }

    let backup = legacy_backup_path(src);
    if let Err(e) = fs::rename(src, &backup) {
        let _ = fs::remove_file(&tmp);
        return Err(failed(format!(
            "couldn't move {} to {}: {}", src.display(), backup.display(), e
        )));
    }
    if let Err(e) = fs::rename(&tmp, dest) {
        let _ = fs::rename(&backup, src);
        let _ = fs::remove_file(&tmp);
        return Err(failed(format!(
            "couldn't put the converted Folio at {}: {}", dest.display(), e
        )));
    }
    Ok(backup)
}

/// Copies `src` to `tmp` and converts the copy. `src` is opened read-only.
fn copy_and_migrate(src: &Path, tmp: &Path) -> Result<(), MyError> {
    let src_conn = Connection::open_with_flags(src, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    match schema_version(&src_conn)? {
        Some(2) => {}
        Some(v) => return Err(failed(format!("it's version {}, not 2", v))),
        None => return Err(failed("it isn't a Folio")),
    }
    let tmp_str = tmp
        .to_str()
        .ok_or_else(|| failed(format!("{} isn't valid UTF-8", tmp.display())))?;
    src_conn.execute("VACUUM INTO ?1", [tmp_str])?;
    drop(src_conn);

    let mut conn = Connection::open(tmp)?;
    v2_to_v3(&mut conn)?;
    conn.close().map_err(|(_, e)| MyError::Database(e))?;
    Ok(())
}

/// Converts a version 2 Folio to version 3, in one transaction: the Folio
/// is either fully converted or left as it was.
pub(crate) fn v2_to_v3(conn: &mut Connection) -> Result<(), MyError> {
    // Rebuilding a table needs foreign keys off (SQLite's procedure for
    // changing a table), and they can't be switched inside a transaction.
    conn.pragma_update(None, "foreign_keys", "OFF")?;
    let result = migrate_v2_to_v3(conn);
    let restored = conn.pragma_update(None, "foreign_keys", "ON");
    result?;
    restored?;
    Ok(())
}

fn migrate_v2_to_v3(conn: &mut Connection) -> Result<(), MyError> {
    let tx = conn.transaction()?;

    match schema_version(&tx)? {
        Some(2) => {}
        Some(v) => return Err(failed(format!("expected a version 2 Folio, found version {}", v))),
        None => return Err(failed("this isn't a Folio")),
    }

    // A role this doesn't know about stops the conversion.
    let known = ROLE_CODES
        .iter()
        .map(|(old, _)| format!("'{}'", old))
        .collect::<Vec<_>>()
        .join(", ");
    let mut stmt = tx.prepare(&format!(
        "SELECT DISTINCT role FROM msg WHERE role NOT IN ({})", known
    ))?;
    let unknown = stmt
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<String>>>()?;
    drop(stmt);
    if !unknown.is_empty() {
        return Err(failed(format!(
            "some messages have a role this version doesn't know ({}); nothing was converted",
            unknown.join(", ")
        )));
    }

    // 1. msg: rebuilt, since its CHECK constraint lists the role strings.
    let old_seq: Option<i64> = tx
        .query_row("SELECT seq FROM sqlite_sequence WHERE name = 'msg'", [], |row| row.get(0))
        .optional()?;
    let role_case = ROLE_CODES
        .iter()
        .map(|(old, new)| format!("WHEN '{}' THEN '{}'", old, new))
        .collect::<Vec<_>>()
        .join(" ");
    tx.execute_batch(&msg_table_sql("msg_v3"))?;
    tx.execute(&format!(
        "INSERT INTO msg_v3 (id, content, reasoning, role, name, details,
             model_preset_id, model_preset_json, prompt_tokens, completion_tokens,
             cost, ts_created)
         SELECT id, content, reasoning, CASE role {} END, name, details,
             preset_id, preset_json, prompt_tokens, completion_tokens,
             cost, ts_created
         FROM msg", role_case
    ), [])?;
    tx.execute_batch("DROP TABLE msg; ALTER TABLE msg_v3 RENAME TO msg;")?;
    // Keep the ids of deleted messages from being handed out again.
    if let Some(seq) = old_seq {
        let updated = tx.execute(
            "UPDATE sqlite_sequence SET seq = max(seq, ?1) WHERE name = 'msg'", [seq],
        )?;
        if updated == 0 {
            tx.execute("INSERT INTO sqlite_sequence (name, seq) VALUES ('msg', ?1)", [seq])?;
        }
    }

    // 2. agent -> talker
    tx.execute_batch(
        "ALTER TABLE agent RENAME TO talker;
         ALTER TABLE talker RENAME COLUMN agent_ind TO talker_ind;
         ALTER TABLE talker RENAME COLUMN preset_id TO model_preset_id;
         ALTER TABLE talker RENAME COLUMN preset_json TO model_preset_json;",
    )?;

    // 3. preset -> model_preset, with its trigger recreated under the new names.
    tx.execute_batch(
        "DROP TRIGGER IF EXISTS ensure_preset_not_empty_trigger;
         ALTER TABLE preset RENAME TO model_preset;",
    )?;
    tx.execute_batch(MODEL_PRESET_NOT_EMPTY_TRIGGER)?;

    tx.execute("UPDATE schema_version SET version = 3", [])?;
    tx.commit()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A version 2 Folio, as Inforno wrote them before this release.
    fn v2_folio() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("
            create table msg (
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
            );
            create table chat (
                id integer primary key autoincrement,
                parent integer,
                title text,
                ts_created datetime default current_timestamp,
                ts_modified datetime default current_timestamp
            );
            create table agent (
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
            );
            create table schema_version (
                id integer primary key check (id = 0),
                name text,
                version int,
                applied_on datetime default current_timestamp
            );
            create table preset (
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
            );
            create trigger ensure_preset_not_empty_trigger
                after delete on preset
                when not exists (select 1 from preset)
                begin
                    insert into preset (title, tooltip, chat_router, model, options)
                    values ('DeepSeek: R1 0528 (free)', '', 'Openrouter',
                        'deepseek/deepseek-r1-0528:free', '');
                end;
            insert into schema_version (id, name, version) values (0, 'inforno_main_db', 2);
            insert into chat (title) values ('chat');
            insert into agent (chat_id, agent_ind, name, msg_ids, preset_id, preset_json)
                values (1, 0, 'Omnis', '[1,2,3,4,5]', 0, null),
                       (1, 1, 'Agent1', '[1,2,3,4,5]', 7, '{}');
            insert into msg (content, role) values
                ('u', 'user'), ('a', 'assistant'), ('s', 'system'),
                ('n', 'developer'), ('t', 'tool'), ('gone', 'user');
            delete from msg where id = 6;
            insert into preset (title) values ('p');
        ").unwrap();
        conn
    }

    #[test]
    fn v2_converts_to_v3() {
        let mut conn = v2_folio();
        v2_to_v3(&mut conn).unwrap();
        assert_eq!(schema_version(&conn).unwrap(), Some(3));

        let roles: Vec<String> = conn
            .prepare("select role from msg order by id").unwrap()
            .query_map([], |r| r.get(0)).unwrap()
            .collect::<rusqlite::Result<_>>().unwrap();
        assert_eq!(roles, ["usr", "asst", "sys", "cell", "tool"]);

        let (preset_id, preset_json): (i64, String) = conn.query_row(
            "select model_preset_id, model_preset_json from talker where talker_ind = 1",
            [], |r| Ok((r.get(0)?, r.get(1)?)),
        ).unwrap();
        assert_eq!((preset_id, preset_json.as_str()), (7, "{}"));

        // The deleted message's id isn't handed out again, and the v3 role
        // check applies.
        conn.execute("insert into msg (content, role) values ('d', 'dev')", []).unwrap();
        assert_eq!(conn.last_insert_rowid(), 7);
        assert!(conn.execute("insert into msg (content, role) values ('x', 'developer')", []).is_err());

        // The trigger came along under its new name.
        conn.execute("delete from model_preset", []).unwrap();
        let presets: i64 = conn.query_row("select count(*) from model_preset", [], |r| r.get(0)).unwrap();
        assert_eq!(presets, 1);
    }

    #[test]
    fn unknown_role_stops_conversion() {
        let mut conn = v2_folio();
        conn.pragma_update(None, "ignore_check_constraints", true).unwrap();
        conn.execute("insert into msg (content, role) values ('?', 'oracle')", []).unwrap();
        conn.pragma_update(None, "ignore_check_constraints", false).unwrap();

        let err = v2_to_v3(&mut conn).unwrap_err();
        assert!(err.to_string().contains("oracle"));
        assert_eq!(schema_version(&conn).unwrap(), Some(2));
        assert!(conn.prepare("select agent_ind from agent").is_ok());
    }
}
