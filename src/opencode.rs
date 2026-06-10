use std::ffi::OsString;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use serde_json::Value;

use crate::common::{
    AgentKind, AgentProvider, LaunchCommand, LhResult, RemovalTarget, ThreadSummary,
    default_executable,
};
use crate::util::{
    canonicalize_existing, find_executable, first_model_string_at_paths, home_dir, millis_to_time,
    model_string, path_is_at_or_under,
};

pub struct OpenCodeProvider {
    home: PathBuf,
}

impl OpenCodeProvider {
    pub fn new() -> Self {
        Self { home: home_dir() }
    }

    #[cfg(test)]
    pub fn with_home(home: PathBuf) -> Self {
        Self { home }
    }

    fn db_path(&self) -> PathBuf {
        self.home.join(".local/share/opencode/opencode.db")
    }
}

impl AgentProvider for OpenCodeProvider {
    fn kind(&self) -> AgentKind {
        AgentKind::OpenCode
    }

    fn history_path(&self, _cwd: &Path) -> PathBuf {
        self.db_path()
    }

    fn executable(&self) -> Option<PathBuf> {
        find_executable("opencode").or_else(|| {
            let candidate = self.home.join(".opencode/bin/opencode");
            candidate.is_file().then_some(candidate)
        })
    }

    fn list_threads(&self, cwd: &Path) -> LhResult<Vec<ThreadSummary>> {
        let canonical_cwd = canonicalize_existing(cwd);
        self.list_from_db(Some(&canonical_cwd))
    }

    fn list_threads_global(&self) -> LhResult<Vec<ThreadSummary>> {
        self.list_from_db(None)
    }

    fn new_command(&self, _name: Option<&str>, cwd: &Path) -> LhResult<LaunchCommand> {
        Ok(LaunchCommand::new(
            self.executable()
                .unwrap_or_else(|| default_executable("opencode")),
            [cwd.as_os_str().to_os_string()],
        ))
    }

    fn resume_command(&self, thread: Option<&ThreadSummary>) -> LhResult<LaunchCommand> {
        let thread = thread.ok_or("no OpenCode thread selected")?;
        Ok(LaunchCommand::new(
            self.executable()
                .unwrap_or_else(|| default_executable("opencode")),
            [OsString::from("--session"), OsString::from(&thread.id)],
        ))
    }

    fn supports_rename(&self) -> bool {
        true
    }

    fn rename_thread(&self, thread: &ThreadSummary, name: &str) -> LhResult<()> {
        let conn = Connection::open(self.db_path())?;
        let changed = conn.execute(
            "update session set title = ?1 where id = ?2",
            params![name, thread.id],
        )?;
        if changed == 0 {
            return Err(format!("OpenCode session not found: {}", thread.id).into());
        }
        Ok(())
    }

    fn unset_thread_name(&self, thread: &ThreadSummary) -> LhResult<()> {
        let conn = Connection::open(self.db_path())?;
        let changed = conn.execute(
            "update session set title = '' where id = ?1",
            params![thread.id],
        )?;
        if changed == 0 {
            return Err(format!("OpenCode session not found: {}", thread.id).into());
        }
        Ok(())
    }

    fn thread_content(&self, thread: &ThreadSummary) -> LhResult<String> {
        let conn = Connection::open_with_flags(self.db_path(), OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        Ok(text_parts(&conn, &thread.id)?.join("\n\n"))
    }
}

impl OpenCodeProvider {
    fn list_from_db(&self, cwd_filter: Option<&Path>) -> LhResult<Vec<ThreadSummary>> {
        let db_path = self.db_path();
        if !db_path.exists() {
            return Ok(Vec::new());
        }

        let conn = Connection::open_with_flags(&db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let mut stmt = conn.prepare(
            "select s.id, s.slug, s.title, s.directory, s.time_created, s.time_updated, p.worktree
             from session s left join project p on s.project_id = p.id",
        )?;
        let mut rows = stmt.query([])?;
        let mut threads = Vec::new();

        while let Some(row) = rows.next()? {
            let id: String = row.get(0)?;
            let slug: String = row.get(1)?;
            let title: String = row.get(2)?;
            let directory: String = row.get(3)?;
            let created_ms: i64 = row.get(4)?;
            let updated_ms: i64 = row.get(5)?;
            let worktree: Option<String> = row.get(6).ok();
            let model = session_model(&conn, &id)
                .ok()
                .flatten()
                .or_else(|| first_model(&conn, &id).ok().flatten());

            let directory_path = PathBuf::from(&directory);
            let worktree_path = worktree.as_deref().map(PathBuf::from);
            let canonical_directory = canonicalize_existing(&directory_path);
            let thread_cwd = worktree_path
                .as_ref()
                .map(|path| canonicalize_existing(path))
                .unwrap_or_else(|| canonical_directory.clone());
            if let Some(cwd_filter) = cwd_filter
                && !path_is_at_or_under(&canonical_directory, cwd_filter)
                && !path_is_at_or_under(&thread_cwd, cwd_filter)
            {
                continue;
            }

            let preview = first_text_part(&conn, &id).ok().flatten();
            let executable = self.executable();
            let removable = if let Some(executable) = executable {
                Some(RemovalTarget::Command(LaunchCommand::new(
                    executable,
                    [
                        OsString::from("session"),
                        OsString::from("delete"),
                        OsString::from(&id),
                    ],
                )))
            } else {
                Some(RemovalTarget::OpenCodeDb {
                    db_path: db_path.clone(),
                    session_id: id.clone(),
                })
            };

            threads.push(ThreadSummary {
                agent: AgentKind::OpenCode,
                id,
                name: Some(if title.trim().is_empty() { slug } else { title }),
                model,
                cwd: thread_cwd,
                created_at: millis_to_time(created_ms),
                updated_at: millis_to_time(updated_ms),
                source_path: Some(db_path.clone()),
                preview,
                removable,
                resume_hint: None,
            });
        }

        threads.sort_by_key(|thread| std::cmp::Reverse(thread.updated_sort_key()));
        Ok(threads)
    }
}

fn session_model(conn: &Connection, session_id: &str) -> rusqlite::Result<Option<String>> {
    let columns = table_columns(conn, "session")?;
    for candidate in ["model", "model_id", "modelID", "modelId"] {
        let Some(column) = columns
            .iter()
            .find(|column| column.eq_ignore_ascii_case(candidate))
        else {
            continue;
        };
        let escaped = column.replace('"', "\"\"");
        let sql = format!("select \"{escaped}\" from session where id = ?1 limit 1");
        let model = conn
            .query_row(&sql, params![session_id], |row| {
                row.get::<_, Option<String>>(0)
            })
            .optional()?
            .flatten()
            .and_then(|value| model_string(&Value::String(value)));
        if model.is_some() {
            return Ok(model);
        }
    }
    Ok(None)
}

fn table_columns(conn: &Connection, table: &str) -> rusqlite::Result<Vec<String>> {
    let mut stmt = conn.prepare(&format!("pragma table_info({table})"))?;
    let mut rows = stmt.query([])?;
    let mut columns = Vec::new();
    while let Some(row) = rows.next()? {
        columns.push(row.get(1)?);
    }
    Ok(columns)
}

fn first_model(conn: &Connection, session_id: &str) -> rusqlite::Result<Option<String>> {
    let mut stmt = conn.prepare(
        "select m.data
         from message m
         where m.session_id = ?1
         order by m.time_created asc",
    )?;
    let mut rows = stmt.query(params![session_id])?;
    while let Some(row) = rows.next()? {
        let data: String = row.get(0)?;
        let Ok(value) = serde_json::from_str::<Value>(&data) else {
            continue;
        };
        if let Some(model) = opencode_model_from_json(&value) {
            return Ok(Some(model));
        }
    }
    Ok(None)
}

fn opencode_model_from_json(value: &Value) -> Option<String> {
    first_model_string_at_paths(
        value,
        &[
            &["model"],
            &["modelID"],
            &["modelId"],
            &["model_id"],
            &["model", "id"],
            &["model", "name"],
            &["model", "model"],
            &["model", "modelID"],
            &["model", "modelId"],
            &["model", "model_id"],
            &["request", "model"],
            &["request", "modelID"],
            &["request", "modelId"],
            &["metadata", "model"],
            &["metadata", "modelID"],
            &["metadata", "modelId"],
        ],
    )
}

fn first_text_part(conn: &Connection, session_id: &str) -> rusqlite::Result<Option<String>> {
    Ok(text_parts(conn, session_id)?.into_iter().next())
}

fn text_parts(conn: &Connection, session_id: &str) -> rusqlite::Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "select p.data
         from part p
         join message m on p.message_id = m.id
         where p.session_id = ?1
         order by p.time_created asc",
    )?;
    let mut rows = stmt.query(params![session_id])?;
    let mut parts = Vec::new();
    while let Some(row) = rows.next()? {
        let data: String = row.get(0)?;
        let Ok(value) = serde_json::from_str::<Value>(&data) else {
            continue;
        };
        if value.get("type").and_then(|value| value.as_str()) == Some("text")
            && let Some(text) = value.get("text").and_then(|value| value.as_str())
        {
            parts.push(text.to_string());
        }
    }
    Ok(parts)
}

pub fn delete_session_from_db(db_path: &Path, session_id: &str) -> LhResult<()> {
    let conn = Connection::open(db_path)?;
    conn.execute(
        "delete from part where session_id = ?1",
        params![session_id],
    )?;
    conn.execute(
        "delete from session_message where session_id = ?1",
        params![session_id],
    )?;
    conn.execute(
        "delete from message where session_id = ?1",
        params![session_id],
    )?;
    conn.execute("delete from session where id = ?1", params![session_id])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use rusqlite::params;

    use super::*;
    use crate::util::temp_dir;

    #[test]
    fn parses_opencode_fixture() {
        let root = temp_dir("opencode");
        let cwd = root.join("work");
        let db_dir = root.join(".local/share/opencode");
        fs::create_dir_all(&db_dir).unwrap();
        fs::create_dir_all(&cwd).unwrap();
        let conn = Connection::open(db_dir.join("opencode.db")).unwrap();
        conn.execute_batch(
            "create table project (id text primary key, worktree text not null);
             create table session (id text primary key, project_id text not null, slug text not null, title text not null, directory text not null, time_created integer not null, time_updated integer not null, model text);
             create table message (id text primary key, session_id text not null, time_created integer not null, time_updated integer not null, data text not null);
             create table part (id text primary key, message_id text not null, session_id text not null, time_created integer not null, time_updated integer not null, data text not null);",
        )
        .unwrap();
        conn.execute(
            "insert into project values ('p', ?1)",
            params![cwd.to_string_lossy()],
        )
        .unwrap();
        conn.execute(
            "insert into session values ('s', 'p', 'slug', 'Title', ?1, 1000, 2000, '{\"id\":\"big-pickle\",\"providerID\":\"opencode\"}')",
            params![cwd.to_string_lossy()],
        )
        .unwrap();
        conn.execute(
            "insert into message values ('m', 's', 1000, 1000, '{\"model\":{\"providerID\":\"opencode\",\"modelID\":\"big-pickle\"}}')",
            [],
        )
        .unwrap();
        conn.execute(
            "insert into part values ('part', 'm', 's', 1000, 1000, '{\"type\":\"text\",\"text\":\"hello opencode\"}')",
            [],
        )
        .unwrap();

        let threads = OpenCodeProvider::with_home(root)
            .list_threads(&cwd)
            .unwrap();
        assert_eq!(threads.len(), 1);
        assert_eq!(threads[0].name.as_deref(), Some("Title"));
        assert_eq!(threads[0].model.as_deref(), Some("big-pickle"));
        assert_eq!(threads[0].preview.as_deref(), Some("hello opencode"));
    }

    #[test]
    fn renames_opencode_thread() {
        let root = temp_dir("opencode-rename");
        let cwd = root.join("work");
        let db_dir = root.join(".local/share/opencode");
        fs::create_dir_all(&db_dir).unwrap();
        fs::create_dir_all(&cwd).unwrap();
        let conn = Connection::open(db_dir.join("opencode.db")).unwrap();
        conn.execute_batch(
            "create table project (id text primary key, worktree text not null);
             create table session (id text primary key, project_id text not null, slug text not null, title text not null, directory text not null, time_created integer not null, time_updated integer not null);
             create table message (id text primary key, session_id text not null, time_created integer not null, time_updated integer not null, data text not null);
             create table part (id text primary key, message_id text not null, session_id text not null, time_created integer not null, time_updated integer not null, data text not null);",
        )
        .unwrap();
        conn.execute(
            "insert into project values ('p', ?1)",
            params![cwd.to_string_lossy()],
        )
        .unwrap();
        conn.execute(
            "insert into session values ('s', 'p', 'slug', 'Old', ?1, 1000, 2000)",
            params![cwd.to_string_lossy()],
        )
        .unwrap();
        let provider = OpenCodeProvider::with_home(root);
        let thread = provider.list_threads(&cwd).unwrap().remove(0);

        provider.rename_thread(&thread, "New").unwrap();

        let renamed = provider.list_threads(&cwd).unwrap().remove(0);
        assert_eq!(renamed.name.as_deref(), Some("New"));
    }

    #[test]
    fn unsets_opencode_thread_name() {
        let root = temp_dir("opencode-unset");
        let cwd = root.join("work");
        let db_dir = root.join(".local/share/opencode");
        fs::create_dir_all(&db_dir).unwrap();
        fs::create_dir_all(&cwd).unwrap();
        let conn = Connection::open(db_dir.join("opencode.db")).unwrap();
        conn.execute_batch(
            "create table project (id text primary key, worktree text not null);
             create table session (id text primary key, project_id text not null, slug text not null, title text not null, directory text not null, time_created integer not null, time_updated integer not null);
             create table message (id text primary key, session_id text not null, time_created integer not null, time_updated integer not null, data text not null);
             create table part (id text primary key, message_id text not null, session_id text not null, time_created integer not null, time_updated integer not null, data text not null);",
        )
        .unwrap();
        conn.execute(
            "insert into project values ('p', ?1)",
            params![cwd.to_string_lossy()],
        )
        .unwrap();
        conn.execute(
            "insert into session values ('s', 'p', 'slug', 'Old', ?1, 1000, 2000)",
            params![cwd.to_string_lossy()],
        )
        .unwrap();
        let provider = OpenCodeProvider::with_home(root);
        let thread = provider.list_threads(&cwd).unwrap().remove(0);

        provider.unset_thread_name(&thread).unwrap();

        let renamed = provider.list_threads(&cwd).unwrap().remove(0);
        assert_eq!(renamed.name.as_deref(), Some("slug"));
    }

    #[test]
    fn list_threads_includes_subdirectories() {
        let root = temp_dir("opencode-subdir");
        let cwd = root.join("work");
        let child = cwd.join("child");
        let db_dir = root.join(".local/share/opencode");
        fs::create_dir_all(&db_dir).unwrap();
        fs::create_dir_all(&child).unwrap();
        let conn = Connection::open(db_dir.join("opencode.db")).unwrap();
        conn.execute_batch(
            "create table project (id text primary key, worktree text not null);
             create table session (id text primary key, project_id text not null, slug text not null, title text not null, directory text not null, time_created integer not null, time_updated integer not null);
             create table message (id text primary key, session_id text not null, time_created integer not null, time_updated integer not null, data text not null);
             create table part (id text primary key, message_id text not null, session_id text not null, time_created integer not null, time_updated integer not null, data text not null);",
        )
        .unwrap();
        conn.execute(
            "insert into project values ('p', ?1)",
            params![child.to_string_lossy()],
        )
        .unwrap();
        conn.execute(
            "insert into session values ('s', 'p', 'slug', 'Title', ?1, 1000, 2000)",
            params![child.to_string_lossy()],
        )
        .unwrap();

        let threads = OpenCodeProvider::with_home(root)
            .list_threads(&cwd)
            .unwrap();
        assert_eq!(threads.len(), 1);
        assert_eq!(threads[0].cwd, canonicalize_existing(&child));
    }
}
