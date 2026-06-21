use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde_json::Value;
use time::OffsetDateTime;

use crate::common::{
    AgentKind, AgentProvider, LaunchCommand, LhResult, RemovalTarget, ThreadSummary,
    default_executable, truncate,
};
use crate::util::{
    canonicalize_existing, collect_files, home_dir, parse_time, path_is_at_or_under,
};

pub struct PiProvider {
    home: PathBuf,
}

impl PiProvider {
    pub fn new() -> Self {
        Self { home: home_dir() }
    }

    #[cfg(test)]
    pub fn with_home(home: PathBuf) -> Self {
        Self { home }
    }

    fn sessions_dir(&self) -> PathBuf {
        self.home.join(".pi/agent/sessions")
    }
}

impl AgentProvider for PiProvider {
    fn kind(&self) -> AgentKind {
        AgentKind::Pi
    }

    fn history_path(&self, _cwd: &Path) -> PathBuf {
        self.sessions_dir()
    }

    fn executable(&self) -> Option<PathBuf> {
        crate::util::find_executable("pi")
    }

    fn list_threads(&self, cwd: &Path) -> LhResult<Vec<ThreadSummary>> {
        let canonical_cwd = canonicalize_existing(cwd);
        self.list_sessions(Some(&canonical_cwd))
    }

    fn list_threads_global(&self) -> LhResult<Vec<ThreadSummary>> {
        self.list_sessions(None)
    }

    fn new_command(&self, name: Option<&str>, cwd: &Path) -> LhResult<LaunchCommand> {
        let mut args: Vec<OsString> = Vec::new();
        if let Some(name) = name {
            args.push("--name".into());
            args.push(name.into());
        }
        Ok(LaunchCommand::new(default_executable("pi"), args).with_current_dir(cwd))
    }

    fn resume_command(&self, thread: Option<&ThreadSummary>) -> LhResult<LaunchCommand> {
        let Some(thread) = thread else {
            return Ok(LaunchCommand::new(default_executable("pi"), ["-r"]));
        };
        let path = thread
            .source_path
            .as_ref()
            .ok_or("selected Pi session does not expose a source path")?;
        Ok(LaunchCommand::new(
            default_executable("pi"),
            ["--session".into(), path.as_os_str().to_os_string()],
        )
        .with_current_dir(&thread.cwd))
    }

    fn supports_rename(&self) -> bool {
        true
    }

    fn rename_thread(&self, thread: &ThreadSummary, name: &str) -> LhResult<()> {
        append_session_info(thread, name)
    }

    fn unset_thread_name(&self, thread: &ThreadSummary) -> LhResult<()> {
        append_session_info(thread, "")
    }

    fn thread_content(&self, thread: &ThreadSummary) -> LhResult<String> {
        let path = thread
            .source_path
            .as_ref()
            .ok_or("selected Pi session does not expose source content")?;
        pi_thread_content(path)
    }
}

impl PiProvider {
    fn list_sessions(&self, cwd_filter: Option<&Path>) -> LhResult<Vec<ThreadSummary>> {
        let sessions_dir = self.sessions_dir();
        if !sessions_dir.exists() {
            return Ok(Vec::new());
        }

        let mut paths = Vec::new();
        collect_files(&sessions_dir, &mut |path| {
            if path.extension().and_then(|ext| ext.to_str()) == Some("jsonl") {
                paths.push(path.to_path_buf());
            }
        });

        let mut threads = Vec::new();
        for path in paths {
            match parse_pi_session(&path) {
                Ok(Some(thread)) => {
                    if let Some(cwd_filter) = cwd_filter
                        && !path_is_at_or_under(&thread.cwd, cwd_filter)
                    {
                        continue;
                    }
                    threads.push(thread);
                }
                Ok(None) => {}
                Err(error) => eprintln!(
                    "warning: failed to parse Pi session {}: {error}",
                    path.display()
                ),
            }
        }

        threads.sort_by_key(|thread| std::cmp::Reverse(thread.updated_sort_key()));
        Ok(threads)
    }
}

fn parse_pi_session(path: &Path) -> LhResult<Option<ThreadSummary>> {
    let text = fs::read_to_string(path)?;
    let mut lines = text.lines();
    let Some(header_line) = lines.next() else {
        return Ok(None);
    };
    let header: Value = serde_json::from_str(header_line)?;
    if header.get("type").and_then(Value::as_str) != Some("session") {
        return Ok(None);
    }

    let id = header
        .get("id")
        .and_then(Value::as_str)
        .map(ToString::to_string)
        .or_else(|| {
            path.file_stem()
                .and_then(|stem| stem.to_str())
                .map(ToString::to_string)
        })
        .unwrap_or_else(|| "unknown".to_string());
    let cwd = header
        .get("cwd")
        .and_then(Value::as_str)
        .filter(|cwd| !cwd.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| path.parent().unwrap_or(Path::new(".")).to_path_buf());
    let cwd = canonicalize_existing(&cwd);
    let created_at = header
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(parse_time);

    let mut updated_at = created_at;
    let mut name = None;
    let mut model = None;
    let mut preview = None;
    let mut last_entry_id = None;

    for line in lines {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if let Some(timestamp) = value
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(parse_time)
        {
            updated_at = Some(timestamp);
        }
        if let Some(entry_id) = value.get("id").and_then(Value::as_str) {
            last_entry_id = Some(entry_id.to_string());
        }
        match value.get("type").and_then(Value::as_str) {
            Some("session_info") => {
                name = value
                    .get("name")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|name| !name.is_empty())
                    .map(ToString::to_string);
            }
            Some("model_change") => {
                model = value
                    .get("modelId")
                    .and_then(Value::as_str)
                    .map(ToString::to_string);
            }
            Some("message") => {
                let Some(message) = value.get("message") else {
                    continue;
                };
                if model.is_none() {
                    model = message
                        .get("model")
                        .and_then(Value::as_str)
                        .map(ToString::to_string);
                }
                if preview.is_none() && message.get("role").and_then(Value::as_str) == Some("user")
                {
                    preview = message_text(message).map(|text| truncate(&text, 100));
                }
            }
            _ => {}
        }
    }

    let updated_at = updated_at.or_else(|| {
        fs::metadata(path)
            .ok()
            .and_then(|metadata| metadata.modified().ok())
            .map(OffsetDateTime::from)
    });

    let id = if id.is_empty() {
        path.file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("unknown")
            .to_string()
    } else {
        id
    };

    let _ = last_entry_id;
    Ok(Some(ThreadSummary {
        agent: AgentKind::Pi,
        id,
        name,
        model,
        cwd,
        created_at,
        updated_at,
        source_path: Some(path.to_path_buf()),
        preview,
        removable: Some(RemovalTarget::File(path.to_path_buf())),
        resume_hint: None,
    }))
}

fn append_session_info(thread: &ThreadSummary, name: &str) -> LhResult<()> {
    let path = thread
        .source_path
        .as_ref()
        .ok_or("selected Pi session does not expose a source path")?;
    let parent_id = latest_entry_id(path)?;
    let now = OffsetDateTime::now_utc();
    let timestamp = crate::util::format_time(now);
    let id = format!("{:08x}", (now.unix_timestamp_nanos() as u128) & 0xffff_ffff);
    let entry = serde_json::json!({
        "type": "session_info",
        "id": id,
        "parentId": parent_id,
        "timestamp": timestamp,
        "name": name,
    });
    let mut file = OpenOptions::new().append(true).open(path)?;
    writeln!(file, "{entry}")?;
    Ok(())
}

fn latest_entry_id(path: &Path) -> LhResult<Option<String>> {
    let text = fs::read_to_string(path)?;
    Ok(text
        .lines()
        .rev()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|value| value.get("type").and_then(Value::as_str) != Some("session"))
        .find_map(|value| {
            value
                .get("id")
                .and_then(Value::as_str)
                .map(ToString::to_string)
        }))
}

fn pi_thread_content(path: &Path) -> LhResult<String> {
    let text = fs::read_to_string(path)?;
    let mut out = Vec::new();
    for line in text.lines() {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        match value.get("type").and_then(Value::as_str) {
            Some("message") => {
                if let Some(message) = value.get("message") {
                    let role = message
                        .get("role")
                        .and_then(Value::as_str)
                        .unwrap_or("message");
                    if let Some(text) = message_text(message) {
                        out.push(format!("## {role}\n\n{text}"));
                    }
                }
            }
            Some("compaction") => {
                if let Some(summary) = value.get("summary").and_then(Value::as_str) {
                    out.push(format!("## compaction\n\n{summary}"));
                }
            }
            Some("branch_summary") => {
                if let Some(summary) = value.get("summary").and_then(Value::as_str) {
                    out.push(format!("## branch summary\n\n{summary}"));
                }
            }
            _ => {}
        }
    }
    Ok(out.join("\n\n"))
}

fn message_text(message: &Value) -> Option<String> {
    if let Some(text) = message.get("content").and_then(content_text) {
        return Some(text);
    }
    message
        .get("summary")
        .and_then(Value::as_str)
        .map(ToString::to_string)
        .or_else(|| {
            message
                .get("output")
                .and_then(Value::as_str)
                .map(ToString::to_string)
        })
}

fn content_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => non_empty(text),
        Value::Array(items) => {
            let parts = items
                .iter()
                .filter_map(|item| {
                    item.get("text")
                        .and_then(Value::as_str)
                        .or_else(|| item.get("thinking").and_then(Value::as_str))
                })
                .filter(|text| !text.trim().is_empty())
                .collect::<Vec<_>>();
            (!parts.is_empty()).then(|| parts.join("\n"))
        }
        Value::Object(_) => value
            .get("text")
            .and_then(Value::as_str)
            .or_else(|| value.get("content").and_then(Value::as_str))
            .and_then(non_empty),
        _ => None,
    }
}

fn non_empty(text: &str) -> Option<String> {
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_home(name: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("lh-pi-test-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn lists_and_renames_pi_sessions() {
        let home = temp_home("list-rename");
        let cwd = home.join("project");
        fs::create_dir_all(&cwd).unwrap();
        let session_dir = home.join(".pi/agent/sessions/--tmp--project--");
        fs::create_dir_all(&session_dir).unwrap();
        let session = session_dir.join("20260101_abc.jsonl");
        fs::write(
            &session,
            format!(
                "{}\n{}\n{}\n",
                serde_json::json!({"type":"session","version":3,"id":"session-1","timestamp":"2026-01-01T00:00:00Z","cwd":cwd}),
                serde_json::json!({"type":"message","id":"aaaaaaaa","parentId":null,"timestamp":"2026-01-01T00:00:01Z","message":{"role":"user","content":"Hello Pi"}}),
                serde_json::json!({"type":"message","id":"bbbbbbbb","parentId":"aaaaaaaa","timestamp":"2026-01-01T00:00:02Z","message":{"role":"assistant","content":[{"type":"text","text":"Hi"}],"model":"claude-sonnet-4-5"}}),
            ),
        )
        .unwrap();

        let provider = PiProvider::with_home(home.clone());
        let threads = provider.list_threads(&cwd).unwrap();
        assert_eq!(threads.len(), 1);
        assert_eq!(threads[0].agent, AgentKind::Pi);
        assert_eq!(threads[0].preview.as_deref(), Some("Hello Pi"));
        assert_eq!(threads[0].model.as_deref(), Some("claude-sonnet-4-5"));

        provider.rename_thread(&threads[0], "new name").unwrap();
        let threads = provider.list_threads(&cwd).unwrap();
        assert_eq!(threads[0].name.as_deref(), Some("new name"));

        let _ = fs::remove_dir_all(home);
    }
}
