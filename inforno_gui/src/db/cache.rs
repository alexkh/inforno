use std::{fs, path::PathBuf};
use rusqlite::{Connection, params, Row};
use directories::ProjectDirs;
use crate::{common::{DbOllamaModel, DbOpenrModel, MyError}, db::table_exists};
use chrono::{TimeZone, Utc};

pub fn get_cache_db_conn() -> Result<Connection, MyError> {
    if let Some(proj_dirs) = ProjectDirs::from(
            "", "", "inforno") {
        let mut file_path_buf = PathBuf::from(proj_dirs.cache_dir());
        if !file_path_buf.is_dir() {
            println!("Directory {} does not exist. Trying to create it...",
                file_path_buf.display());
            if let Ok(_result) = fs::create_dir_all(&file_path_buf) {
            } else {
                return Err(MyError::ProjectDir);
            }
        }
        file_path_buf.push("cache2.db");
        // if cache.db is missing, use the pre-set one in assets. This is needed,
        // because it contains the Ollama model info which is scraped from the
        // website. We don't want every user to run website scrape operation.
        if !file_path_buf.exists() {
            let _ = std::fs::write(&file_path_buf,
                include_bytes!("../../assets/cache2.db"));
        }

        let conn = Connection::open(
                file_path_buf)?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        // check if database schema exists
        if table_exists(&conn, "schema_version")? {
            println!("Cache Database schema exists");
            return Ok(conn);
        } else {
            // schema does not exist. Create new database schema
            println!("Cache Database schema does not exist. Creating new one...");
            create_database_schema(&conn)?;
            return Ok(conn);
        }
    }
    return Err(MyError::ProjectDir);
}

pub fn populate_openr_model(conn: &mut Connection, openr_models: &[DbOpenrModel])
        -> rusqlite::Result<()> {
    let tx = conn.transaction()?;
    let mut stmt = tx.prepare("insert into openr_model (
        provider,
        model_id,
        name,
        description,
        context_length,
        price_prompt,
        price_completion,
        price_image,
        details,
        ts_model
    ) values (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)")?;

    for model in openr_models {
        stmt.execute(params![
            model.provider,
            model.model_id,
            model.name,
            model.description,
            model.context_length,
            model.price_prompt,
            model.price_completion,
            model.price_image,
            model.details,
            model.ts_model,
        ])?;
    }
    stmt.finalize()?;
    tx.commit()?;

    Ok(())
}

pub fn populate_ollama_installed(conn: &mut Connection, ollama_models: &[DbOllamaModel])
        -> rusqlite::Result<()> {
    conn.execute("delete from ollama_installed", ())?;
    let tx = conn.transaction()?;
    let mut stmt = tx.prepare("insert into ollama_installed (
        name,
        size,
        url,
        ts_model
    ) values (?, ?, ?, ?)")?;

    for model in ollama_models {
        stmt.execute(params![
            model.name,
            model.size,
            model.url,
            model.ts_model,
        ])?;
    }
    stmt.finalize()?;
    tx.commit()?;

    Ok(())
}

pub fn save_ollama_model(conn: &mut Connection, model: &DbOllamaModel)
        ->rusqlite::Result<()> {
    let tx = conn.transaction()?;
    let mut stmt = tx.prepare("insert into ollama_model (
        name,
        variants,
        size,
        category,
        summary,
        ts_model
    ) values (?, ?, ?, ?, ?, ?)")?;

    stmt.execute(params![
        model.name,
        serde_json::to_string(&model.variants).ok(),
        model.size,
        model.category,
        model.summary,
        model.ts_model,
    ])?;

    stmt.finalize()?;
    tx.commit()?;

    Ok(())
}

pub fn get_ollama_model_info(conn: &Connection, model_id: &str) ->
        rusqlite::Result<Option<DbOllamaModel>> {
    let sql =
        "select id, name, variants, size, category, summary, ts_model, ts_updated
        from ollama_model where name = ?1";

    let result = conn.query_row(sql, params![model_id], |row: &Row| {
        Ok(DbOllamaModel {
            id: row.get(0)?,
            name: row.get(1)?,
            variants: serde_json::from_str(
                &row.get::<_, String>(2)?.to_owned())
                .unwrap_or_default(),
            size: row.get(3)?,
            url: None,
            category: row.get(4)?,
            summary: row.get(5)?,
            ts_model: row.get::<_, Option<String>>(6)?,
            ts_updated: row.get::<_, Option<String>>(7)?,
        })
    });

    match result {
        Ok(item) => Ok(Some(item)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e),
    }
}


pub fn get_openr_model_names(conn: &Connection) -> rusqlite::Result<Vec<String>> {
    let sql = "select model_id from openr_model order by name";
    let mut stmt = conn.prepare(sql)?;
    let name_iter = stmt.query_map(params![], |row| {
        row.get(0)
    })?;

    let mut names = Vec::new();
    for name_result in name_iter {
        names.push(name_result?);
    }

    Ok(names)
}

pub fn get_ollama_model_installed(conn: &Connection) -> rusqlite::Result<Vec<String>> {
    let sql = "select name from ollama_installed order by name";
    let mut stmt = conn.prepare(sql)?;
    let name_iter = stmt.query_map(params![], |row| {
        row.get(0)
    })?;

    let mut names = Vec::new();
    for name_result in name_iter {
        names.push(name_result?);
    }

    Ok(names)
}

pub fn get_ollama_model_names(conn: &Connection) -> rusqlite::Result<Vec<String>> {
    let sql = "select name, variants from ollama_model order by name";
    let mut stmt = conn.prepare(sql)?;
    let name_iter = stmt.query_map(params![], |row| {
        let name: String = row.get(0)?;
        let variants: String = row.get(1)?;
        Ok((name, variants))
    })?;

    let mut names = Vec::new();
    for name_result in name_iter {
        if let Ok((name, variants)) = name_result {
            names.push(name.clone());
            // unpack variants which contain json array
            if let Ok(variants) =
                serde_json::from_str::<Vec<(String, String)>>(&variants) {
                for variant in variants {
                    names.push(format!("{}:{}", name, variant.0))
                }
            }
        }
    }

    names.sort();
    names.dedup();

    Ok(names)
}

pub fn get_openr_model_info(conn: &Connection, model_id: &str) ->
        rusqlite::Result<Option<DbOpenrModel>> {
    let sql =
        "select id, provider, model_id, name, description, context_length,
        price_prompt, price_completion, price_image, details, ts_model
        from openr_model where model_id = ?1";

    let result = conn.query_row(sql, params![model_id], |row: &Row| {
        Ok(DbOpenrModel {
            id: row.get(0)?,
            provider: row.get(1)?,
            model_id: row.get(2)?,
            name: row.get(3)?,
            description: row.get(4)?,
            context_length: row.get(5)?,
            price_prompt: row.get(6)?,
            price_completion: row.get(7)?,
            price_image: row.get(8)?,
            details: row.get(9)?,
            ts_model: Utc.timestamp_opt(row.get(10)?, 0)
                .single()
                .map(|dt| dt.format("%Y-%m-%d").to_string()),
        })
    });

    match result {
        Ok(item) => Ok(Some(item)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e),
    }
}

pub fn clear_ollama_cache(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute("delete from ollama_model", ())?;
    Ok(())
}

fn create_database_schema(conn: &Connection) -> rusqlite::Result<()> {
    // this database keeps all known models to make them accessible in presets
    // table openr_model caches all known models supported by Openrouter
    conn.execute("create table openr_model (
        id integer primary key autoincrement,
        provider text not null,
        model_id text not null unique,
        name text not null,
        description text not null default '',
        context_length real not null default 0.0,
        price_prompt real,
        price_completion real,
        price_image real,
        details text,
        ts_model datetime,
        ts_updated datetime default current_timestamp
    );", ())?;
    // ollama_model caches all the known models supported by Ollama
    conn.execute("create table ollama_model (
        id integer primary key autoincrement,
        name text not null unique,
        size integer,
        variants text,
        category text,
        summary text,
        ts_model datetime,
        ts_updated datetime default current_timestamp
    );", ())?;
    conn.execute("create table ollama_installed (
        id integer primary key autoincrement,
        name text not null,
        size integer,
        url text,
        ts_model datetime,
        ts_updated datetime default current_timestamp,
        unique(name, url)
    );", ())?;
    // this version is needed to update older tables to newer versions
    conn.execute("create table schema_version (
        id integer primary key check (id = 0),
        name text,
        version int,
        applied_on datetime default current_timestamp
    ) without rowid;", ())?;

    conn.execute("insert into schema_version (id, name, version)
            values (0, 'inforno_cache_db', 1);",
    ())?;

    Ok(())
}