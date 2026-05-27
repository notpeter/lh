use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::common::{
    AgentKind, AgentProvider, LaunchCommand, LhResult, RemovalTarget, ThreadSummary,
    default_executable,
};
use crate::util::{
    canonicalize_existing, collect_files_with_name_prefix, first_json_text, home_dir, parse_time,
};

pub struct CodexProvider {
    home: PathBuf,
}

impl CodexProvider {
    pub fn new() -> Self {
        Self { home: home_dir() }
    }

    #[cfg(test)]
    pub fn with_home(home: PathBuf) -> Self {
        Self { home }
    }

    fn sessions_dir(&self) -> PathBuf {
        self.home.join(".codex/sessions")
    }

    fn index_path(&self) -> PathBuf {
        self.home.join(".codex/session_index.jsonl")
    }
}

impl AgentProvider for CodexProvider {
    fn kind(&self) -> AgentKind {
        AgentKind::Codex
    }

    fn history_path(&self, _cwd: &Path) -> PathBuf {
        self.sessions_dir()
    }

    fn executable(&self) -> Option<PathBuf> {
        crate::util::find_executable("codex")
    }

    fn list_threads(&self, cwd: &Path) -> LhResult<Vec<ThreadSummary>> {
        let canonical_cwd = canonicalize_existing(cwd);
        Ok(self.list_rollouts(Some(&canonical_cwd)))
    }

    fn list_threads_global(&self) -> LhResult<Vec<ThreadSummary>> {
        Ok(self.list_rollouts(None))
    }

    fn new_command(&self, _name: Option<&str>, _cwd: &Path) -> LhResult<LaunchCommand> {
        Ok(LaunchCommand::new(
            default_executable("codex"),
            [] as [OsString; 0],
        ))
    }

    fn resume_command(&self, thread: Option<&ThreadSummary>) -> LhResult<LaunchCommand> {
        let mut args = vec![OsString::from("resume")];
        if let Some(thread) = thread {
            args.push(OsString::from(&thread.id));
        } else {
            args.push(OsString::from("--last"));
        }
        Ok(LaunchCommand::new(default_executable("codex"), args))
    }
}

impl CodexProvider {
    fn list_rollouts(&self, cwd_filter: Option<&Path>) -> Vec<ThreadSummary> {
        let names = read_session_index(&self.index_path());
        let mut threads =
            collect_files_with_name_prefix(&self.sessions_dir(), "rollout-", ".jsonl")
                .into_iter()
                .filter_map(|path| parse_codex_rollout(&path, cwd_filter, &names))
                .collect::<Vec<_>>();
        threads.sort_by_key(|thread| std::cmp::Reverse(thread.updated_sort_key()));
        threads
    }
}

fn read_session_index(path: &Path) -> std::collections::HashMap<String, String> {
    let mut names = std::collections::HashMap::new();
    let Ok(text) = fs::read_to_string(path) else {
        return names;
    };

    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let Some(id) = value.get("id").and_then(|value| value.as_str()) else {
            continue;
        };
        let Some(name) = value
            .get("thread_name")
            .or_else(|| value.get("name"))
            .and_then(|value| value.as_str())
        else {
            continue;
        };
        names.insert(id.to_string(), name.to_string());
    }
    names
}

fn parse_codex_rollout(
    path: &Path,
    cwd_filter: Option<&Path>,
    names: &std::collections::HashMap<String, String>,
) -> Option<ThreadSummary> {
    let text = fs::read_to_string(path).ok()?;
    let mut id = None;
    let mut file_cwd = None;
    let mut created_at = None;
    let mut updated_at = None;
    let mut preview = None;

    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };

        let timestamp = value
            .get("timestamp")
            .and_then(|value| value.as_str())
            .and_then(parse_time);
        if let Some(timestamp) = timestamp {
            created_at = created_at.or(Some(timestamp));
            updated_at = Some(
                updated_at.map_or(timestamp, |current: time::OffsetDateTime| {
                    current.max(timestamp)
                }),
            );
        }

        if value.get("type").and_then(|value| value.as_str()) == Some("session_meta") {
            let payload = value.get("payload")?;
            id = payload
                .get("id")
                .and_then(|value| value.as_str())
                .map(ToString::to_string);
            file_cwd = payload
                .get("cwd")
                .and_then(|value| value.as_str())
                .map(PathBuf::from);
            if let Some(meta_time) = payload
                .get("timestamp")
                .and_then(|value| value.as_str())
                .and_then(parse_time)
            {
                created_at = created_at.or(Some(meta_time));
            }
            continue;
        }

        if preview.is_none() {
            preview = codex_user_text(&value);
        }
    }

    let cwd = canonicalize_existing(&file_cwd?);
    if let Some(cwd_filter) = cwd_filter
        && cwd != cwd_filter
    {
        return None;
    }

    let id = id?;
    Some(ThreadSummary {
        agent: AgentKind::Codex,
        id: id.clone(),
        name: names.get(&id).cloned(),
        cwd,
        created_at,
        updated_at,
        source_path: Some(path.to_path_buf()),
        preview,
        removable: Some(RemovalTarget::File(path.to_path_buf())),
        resume_hint: None,
    })
}

fn codex_user_text(value: &Value) -> Option<String> {
    match value.get("type").and_then(|value| value.as_str()) {
        Some("response_item") => {
            let payload = value.get("payload")?;
            if payload.get("role").and_then(|value| value.as_str()) != Some("user") {
                return None;
            }
            payload
                .get("content")
                .and_then(first_json_text)
                .and_then(|text| (!is_noise_user_text(&text)).then_some(text))
        }
        Some("event_msg") => {
            let payload = value.get("payload")?;
            if payload.get("type").and_then(|value| value.as_str()) != Some("user_message") {
                return None;
            }
            payload
                .get("message")
                .and_then(|message| message.as_str())
                .filter(|text| !is_noise_user_text(text))
                .map(ToString::to_string)
        }
        _ => None,
    }
}

fn is_noise_user_text(text: &str) -> bool {
    let trimmed = text.trim_start();
    trimmed.starts_with("<environment_context>")
        || trimmed.starts_with("<user_info>")
        || trimmed.starts_with("<system_context>")
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::util::temp_dir;

    #[test]
    fn parses_codex_fixture() {
        let root = temp_dir("codex");
        let cwd = root.join("work");
        fs::create_dir_all(root.join(".codex/sessions/2026/05/27")).unwrap();
        fs::create_dir_all(&cwd).unwrap();
        fs::write(
            root.join(".codex/session_index.jsonl"),
            "{\"id\":\"abc\",\"thread_name\":\"named codex\",\"updated_at\":\"2026-05-01T00:00:00Z\"}\n",
        )
        .unwrap();
        fs::write(
            root.join(".codex/sessions/2026/05/27/rollout-test.jsonl"),
            format!(
                "{{\"timestamp\":\"2026-05-01T00:00:00Z\",\"type\":\"session_meta\",\"payload\":{{\"id\":\"abc\",\"cwd\":\"{}\"}}}}\n{{\"timestamp\":\"2026-05-01T00:01:00Z\",\"type\":\"event_msg\",\"payload\":{{\"type\":\"user_message\",\"message\":\"hello codex\"}}}}\n",
                cwd.display()
            ),
        )
        .unwrap();

        let threads = CodexProvider::with_home(root).list_threads(&cwd).unwrap();
        assert_eq!(threads.len(), 1);
        assert_eq!(threads[0].name.as_deref(), Some("named codex"));
        assert_eq!(threads[0].preview.as_deref(), Some("hello codex"));
    }
}
