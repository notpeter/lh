use rusqlite::{Connection, OpenFlags};
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use crate::common::{
    AgentKind, AgentProvider, LaunchCommand, LhResult, RemovalTarget, ThreadSummary,
    default_executable,
};
use crate::util::{canonicalize_existing, home_dir, parse_time, path_is_at_or_under};

pub struct AntiGravityProvider {
    home: PathBuf,
}

impl AntiGravityProvider {
    pub fn new() -> Self {
        Self { home: home_dir() }
    }

    #[cfg(test)]
    pub fn with_home(home: PathBuf) -> Self {
        Self { home }
    }

    fn conversations_dir(&self) -> PathBuf {
        self.home.join(".gemini/antigravity-cli/conversations")
    }

    fn brain_dir(&self) -> PathBuf {
        self.home.join(".gemini/antigravity-cli/brain")
    }

    fn annotations_dir(&self) -> PathBuf {
        self.home.join(".gemini/antigravity-cli/annotations")
    }

    fn list_all_threads(&self) -> LhResult<Vec<ThreadSummary>> {
        let conversations_dir = self.conversations_dir();
        if !conversations_dir.exists() {
            return Ok(Vec::new());
        }

        let entries = fs::read_dir(conversations_dir)?;
        let mut threads = Vec::new();

        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("db") {
                continue;
            }
            let filename = path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .unwrap_or("");
            if filename.ends_with("-shm") || filename.ends_with("-wal") || filename.is_empty() {
                continue;
            }

            let id = filename.to_string();

            // Try to extract workspace path from SQLite db.
            let cwd = match self.get_workspace_from_db(&path) {
                Ok(Some(cwd)) => cwd,
                _ => continue, // If we can't find workspace, skip it
            };

            // Parse transcript to get times and preview if possible.
            let (created_at, updated_at, preview) = self.parse_transcript(&id, &path);

            let brain_dir = self.brain_dir().join(&id);
            let brain_dir_opt = brain_dir.exists().then_some(brain_dir);

            let pbtxt_path = self.annotations_dir().join(format!("{id}.pbtxt"));
            let name = parse_annotation_title(&pbtxt_path)
                .or_else(|| preview.clone().map(|p| crate::common::truncate(&p, 64)));

            threads.push(ThreadSummary {
                agent: AgentKind::AntiGravity,
                id: id.clone(),
                name,
                model: None,
                cwd,
                created_at,
                updated_at,
                source_path: Some(path.clone()),
                preview,
                removable: Some(RemovalTarget::AntiGravityFiles {
                    db_path: path,
                    brain_dir: brain_dir_opt,
                    _session_id: id,
                }),
            });
        }

        threads.sort_by_key(|thread| std::cmp::Reverse(thread.updated_sort_key()));
        Ok(threads)
    }

    fn get_workspace_from_db(&self, db_path: &Path) -> LhResult<Option<PathBuf>> {
        let conn = Connection::open_with_flags(db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;

        // Check if the table trajectory_metadata_blob exists
        let mut stmt = conn.prepare(
            "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='trajectory_metadata_blob'"
        )?;
        let table_exists: i32 = stmt.query_row([], |row| row.get(0))?;
        if table_exists == 0 {
            return Ok(None);
        }

        let mut stmt =
            conn.prepare("SELECT data FROM trajectory_metadata_blob WHERE id = 'main' LIMIT 1")?;
        let data: Vec<u8> = match stmt.query_row([], |row| row.get(0)) {
            Ok(data) => data,
            Err(_) => return Ok(None),
        };

        Ok(extract_workspace_from_blob(&data))
    }

    fn parse_transcript(
        &self,
        id: &str,
        db_path: &Path,
    ) -> (
        Option<time::OffsetDateTime>,
        Option<time::OffsetDateTime>,
        Option<String>,
    ) {
        let transcript_path = self
            .brain_dir()
            .join(id)
            .join(".system_generated/logs/transcript.jsonl");

        let file_metadata = fs::metadata(db_path).ok();
        let fallback_time = file_metadata
            .and_then(|m| m.modified().ok())
            .map(time::OffsetDateTime::from);

        if !transcript_path.exists() {
            return (fallback_time, fallback_time, None);
        }

        let content = match fs::read_to_string(&transcript_path) {
            Ok(c) => c,
            Err(_) => return (fallback_time, fallback_time, None),
        };

        let mut created_at = None;
        let mut updated_at = None;
        let mut preview = None;

        for line in content.lines() {
            let Ok(val) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };

            let step_time = val
                .get("created_at")
                .and_then(|t| t.as_str())
                .and_then(parse_time);

            if let Some(t) = step_time {
                if created_at.is_none() {
                    created_at = Some(t);
                }
                updated_at = Some(t);
            }

            if preview.is_none()
                && val.get("type").and_then(|t| t.as_str()) == Some("USER_INPUT")
                && let Some(c) = val.get("content").and_then(|c| c.as_str())
            {
                preview = extract_user_request(c);
            }
        }

        (
            created_at.or(fallback_time),
            updated_at.or(fallback_time),
            preview,
        )
    }
}

impl AgentProvider for AntiGravityProvider {
    fn kind(&self) -> AgentKind {
        AgentKind::AntiGravity
    }

    fn history_path(&self, _cwd: &Path) -> PathBuf {
        self.conversations_dir()
    }

    fn executable(&self) -> Option<PathBuf> {
        crate::util::find_executable("agy")
    }

    fn list_threads(&self, cwd: &Path) -> LhResult<Vec<ThreadSummary>> {
        let canonical_cwd = canonicalize_existing(cwd);
        let mut threads = self.list_all_threads()?;
        threads.retain(|thread| path_is_at_or_under(&thread.cwd, &canonical_cwd));
        Ok(threads)
    }

    fn list_threads_global(&self) -> LhResult<Vec<ThreadSummary>> {
        self.list_all_threads()
    }

    fn new_command(&self, _name: Option<&str>, cwd: &Path) -> LhResult<LaunchCommand> {
        Ok(
            LaunchCommand::new(default_executable("agy"), [] as [OsString; 0])
                .with_current_dir(cwd),
        )
    }

    fn resume_command(&self, thread: Option<&ThreadSummary>) -> LhResult<LaunchCommand> {
        if let Some(thread) = thread {
            Ok(LaunchCommand::new(
                default_executable("agy"),
                [OsString::from("--conversation"), OsString::from(&thread.id)],
            )
            .with_current_dir(&thread.cwd))
        } else {
            Ok(LaunchCommand::new(
                default_executable("agy"),
                [OsString::from("--continue")],
            ))
        }
    }

    fn supports_rename(&self) -> bool {
        true
    }

    fn rename_thread(&self, thread: &ThreadSummary, name: &str) -> LhResult<()> {
        let annotations_dir = self.annotations_dir();
        if !annotations_dir.exists() {
            fs::create_dir_all(&annotations_dir)?;
        }
        let pbtxt_path = annotations_dir.join(format!("{}.pbtxt", thread.id));
        fs::write(pbtxt_path, format!("title:\"{}\"\n", name))?;
        Ok(())
    }

    fn unset_thread_name(&self, thread: &ThreadSummary) -> LhResult<()> {
        let pbtxt_path = self.annotations_dir().join(format!("{}.pbtxt", thread.id));
        if pbtxt_path.exists() {
            fs::remove_file(pbtxt_path)?;
        }
        Ok(())
    }
}

fn parse_annotation_title(path: &Path) -> Option<String> {
    let content = fs::read_to_string(path).ok()?;
    let prefix = "title:\"";
    let start_idx = content.find(prefix)? + prefix.len();
    let end_idx = content[start_idx..].find('"')?;
    Some(content[start_idx..start_idx + end_idx].to_string())
}

fn extract_user_request(content: &str) -> Option<String> {
    let prefix = "<USER_REQUEST>";
    let suffix = "</USER_REQUEST>";
    if let Some(start_idx) = content.find(prefix) {
        let start = start_idx + prefix.len();
        if let Some(end_idx) = content[start..].find(suffix) {
            let request = content[start..start + end_idx].trim();
            if !request.is_empty() {
                return Some(request.to_string());
            }
        }
    }
    let trimmed = content.trim();
    if !trimmed.is_empty() {
        Some(crate::common::truncate(trimmed, 120))
    } else {
        None
    }
}

fn percent_decode(s: &str) -> String {
    let mut decoded = String::new();
    let mut chars = s.chars();
    while let Some(ch) = chars.next() {
        if ch == '%' {
            let mut hex = String::new();
            if let Some(h1) = chars.next() {
                hex.push(h1);
            }
            if let Some(h2) = chars.next() {
                hex.push(h2);
            }
            if let Ok(val) = u8::from_str_radix(&hex, 16) {
                decoded.push(val as char);
            } else {
                decoded.push('%');
                decoded.push_str(&hex);
            }
        } else {
            decoded.push(ch);
        }
    }
    decoded
}

fn extract_workspace_from_blob(blob: &[u8]) -> Option<PathBuf> {
    let prefix = b"file://";
    let pos = blob
        .windows(prefix.len())
        .position(|window| window == prefix)?;
    let start = pos + prefix.len();
    let mut end = start;
    while end < blob.len() {
        let b = blob[end];
        if !(32..=126).contains(&b) {
            break;
        }
        end += 1;
    }
    let url_str = std::str::from_utf8(&blob[start..end]).ok()?;
    let path_str = url_str.trim_start_matches('/');

    let decoded_path_str = percent_decode(path_str);
    #[cfg(unix)]
    let path = PathBuf::from(format!("/{decoded_path_str}"));
    #[cfg(not(unix))]
    let path = PathBuf::from(decoded_path_str);

    Some(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::temp_dir;

    #[test]
    fn parses_antigravity_fixture() {
        let root = temp_dir("antigravity");
        let conversations = root.join(".gemini/antigravity-cli/conversations");
        let brain = root.join(".gemini/antigravity-cli/brain");
        fs::create_dir_all(&conversations).unwrap();
        fs::create_dir_all(&brain).unwrap();

        let db_path = conversations.join("test-session.db");
        let conn = Connection::open(&db_path).unwrap();
        conn.execute(
            "CREATE TABLE trajectory_metadata_blob (id text PRIMARY KEY, data blob)",
            [],
        )
        .unwrap();

        // Binary blob containing file:///Users/peter/code/lh
        let mut blob_data = vec![0x0A, 0x1B];
        blob_data.extend_from_slice(b"file:///Users/peter/code/lh");
        conn.execute(
            "INSERT INTO trajectory_metadata_blob (id, data) VALUES ('main', ?1)",
            [blob_data],
        )
        .unwrap();

        let transcript_dir = brain.join("test-session/.system_generated/logs");
        fs::create_dir_all(&transcript_dir).unwrap();
        fs::write(
            transcript_dir.join("transcript.jsonl"),
            "{\"step_index\":0,\"source\":\"USER_EXPLICIT\",\"type\":\"USER_INPUT\",\"status\":\"DONE\",\"created_at\":\"2026-06-21T18:44:34Z\",\"content\":\"<USER_REQUEST>\\nHello AGY!\\n</USER_REQUEST>\"}\n",
        )
        .unwrap();

        let provider = AntiGravityProvider::with_home(root.clone());
        let threads = provider.list_threads_global().unwrap();
        assert_eq!(threads.len(), 1);
        let thread = &threads[0];
        assert_eq!(thread.id, "test-session");
        assert_eq!(thread.cwd, PathBuf::from("/Users/peter/code/lh"));
        assert_eq!(thread.preview.as_deref(), Some("Hello AGY!"));
        assert_eq!(thread.name.as_deref(), Some("Hello AGY!"));

        // Test renaming
        assert!(provider.supports_rename());
        provider.rename_thread(thread, "Renamed AGY").unwrap();

        // Verify the file was written
        let annotations = root.join(".gemini/antigravity-cli/annotations");
        let pbtxt = annotations.join("test-session.pbtxt");
        assert!(pbtxt.exists());
        assert_eq!(
            fs::read_to_string(&pbtxt).unwrap(),
            "title:\"Renamed AGY\"\n"
        );

        // Reload threads and verify name is loaded
        let threads2 = provider.list_threads_global().unwrap();
        assert_eq!(threads2[0].name.as_deref(), Some("Renamed AGY"));

        // Test unsetting name
        provider.unset_thread_name(thread).unwrap();
        assert!(!pbtxt.exists());
    }
}
