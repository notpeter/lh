use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, params};

use crate::common::{LhResult, ThreadSummary};
use crate::providers;
use crate::util::{format_time, home_dir};

const SCHEMA: &str = "
create table if not exists threads (
    agent text not null,
    id text not null,
    name text,
    cwd text not null,
    created_at text,
    updated_at text,
    source_path text,
    preview text,
    raw_json text not null,
    primary key (agent, id)
);
";

pub fn db_path() -> PathBuf {
    data_dir_for(
        &home_dir(),
        std::env::var_os("XDG_DATA_HOME").map(PathBuf::from),
        std::env::consts::OS,
    )
    .join("lh.sqlite")
}

pub fn data_dir_for(home: &Path, xdg_data_home: Option<PathBuf>, os: &str) -> PathBuf {
    match os {
        "macos" => home.join("Library/Application Support/lh"),
        "linux" => xdg_data_home
            .map(|path| path.join("lh"))
            .unwrap_or_else(|| home.join(".local/state/lh")),
        _ => xdg_data_home
            .map(|path| path.join("lh"))
            .unwrap_or_else(|| home.join(".local/share/lh")),
    }
}

pub fn init() -> LhResult<PathBuf> {
    let path = db_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let conn = Connection::open(&path)?;
    conn.execute_batch(SCHEMA)?;
    Ok(path)
}

pub fn refresh(cwd: &Path) -> LhResult<(PathBuf, usize)> {
    let path = db_path();
    if path.exists() {
        fs::remove_file(&path)?;
    }
    init()?;
    let conn = Connection::open(&path)?;
    let mut count = 0usize;
    for thread in providers::list_all(cwd)? {
        insert_thread(&conn, &thread)?;
        count += 1;
    }
    Ok((path, count))
}

pub fn drop_db() -> LhResult<PathBuf> {
    let path = db_path();
    if path.exists() {
        fs::remove_file(&path)?;
    }
    Ok(path)
}

fn insert_thread(conn: &Connection, thread: &ThreadSummary) -> LhResult<()> {
    conn.execute(
        "insert or replace into threads
         (agent, id, name, cwd, created_at, updated_at, source_path, preview, raw_json)
         values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            thread.agent.as_str(),
            thread.id,
            thread.name,
            thread.cwd.to_string_lossy(),
            thread.created_at.map(format_time),
            thread.updated_at.map(format_time),
            thread
                .source_path
                .as_ref()
                .map(|path| path.to_string_lossy().to_string()),
            thread.preview,
            thread.raw_json(),
        ],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    #[test]
    fn chooses_macos_data_dir() {
        assert_eq!(
            data_dir_for(Path::new("/home/me"), None, "macos"),
            PathBuf::from("/home/me/Library/Application Support/lh")
        );
    }

    #[test]
    fn chooses_linux_xdg_data_dir() {
        assert_eq!(
            data_dir_for(
                Path::new("/home/me"),
                Some(PathBuf::from("/tmp/xdg")),
                "linux"
            ),
            PathBuf::from("/tmp/xdg/lh")
        );
    }

    #[test]
    fn chooses_linux_fallback_data_dir() {
        assert_eq!(
            data_dir_for(Path::new("/home/me"), None, "linux"),
            PathBuf::from("/home/me/.local/state/lh")
        );
    }
}
