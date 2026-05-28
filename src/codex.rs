use std::ffi::OsString;
use std::fs;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use time::OffsetDateTime;

use crate::common::{
    AgentKind, AgentProvider, LaunchCommand, LhResult, RemovalTarget, ThreadSummary,
    default_executable,
};
use crate::util::{
    canonicalize_existing, collect_files_with_name_prefix, first_json_text, format_time, home_dir,
    is_noise_preview_text, parse_time, path_is_at_or_under,
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

    fn supports_rename(&self) -> bool {
        true
    }

    fn rename_thread(&self, thread: &ThreadSummary, name: &str) -> LhResult<()> {
        let path = self.index_path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut file = OpenOptions::new().create(true).append(true).open(path)?;
        writeln!(
            file,
            "{}",
            json!({
                "id": thread.id,
                "thread_name": name,
                "updated_at": format_time(OffsetDateTime::now_utc()),
            })
        )?;
        Ok(())
    }

    fn unset_thread_name(&self, thread: &ThreadSummary) -> LhResult<()> {
        let path = self.index_path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut file = OpenOptions::new().create(true).append(true).open(path)?;
        writeln!(
            file,
            "{}",
            json!({
                "id": thread.id,
                "thread_name": null,
                "updated_at": format_time(OffsetDateTime::now_utc()),
            })
        )?;
        Ok(())
    }

    fn thread_content(&self, thread: &ThreadSummary) -> LhResult<String> {
        let path = thread
            .source_path
            .as_ref()
            .ok_or("Codex thread is missing its rollout path")?;
        codex_thread_content(path)
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
        if let Some(name) = value.get("thread_name").or_else(|| value.get("name")) {
            match name.as_str().map(str::trim).filter(|name| !name.is_empty()) {
                Some(name) => {
                    names.insert(id.to_string(), name.to_string());
                }
                None => {
                    names.remove(id);
                }
            }
        }
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
        && !path_is_at_or_under(&cwd, cwd_filter)
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
                .and_then(|text| (!is_noise_preview_text(&text)).then_some(text))
        }
        Some("event_msg") => {
            let payload = value.get("payload")?;
            if payload.get("type").and_then(|value| value.as_str()) != Some("user_message") {
                return None;
            }
            payload
                .get("message")
                .and_then(|message| message.as_str())
                .filter(|text| !is_noise_preview_text(text))
                .map(ToString::to_string)
        }
        _ => None,
    }
}

fn codex_thread_content(path: &Path) -> LhResult<String> {
    let text = fs::read_to_string(path)?;
    let mut out = Vec::new();
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if let Some(text) = codex_user_text(&value) {
            out.push(format!("user: {text}"));
            continue;
        }
        if value.get("type").and_then(|value| value.as_str()) == Some("response_item") {
            let Some(payload) = value.get("payload") else {
                continue;
            };
            let role = payload
                .get("role")
                .and_then(|value| value.as_str())
                .unwrap_or("assistant");
            if role == "user" {
                continue;
            }
            if let Some(text) = payload.get("content").and_then(first_json_text) {
                out.push(format!("{role}: {text}"));
            }
        }
    }
    Ok(out.join("\n\n"))
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

    #[test]
    fn skips_injected_codex_setup_when_building_preview() {
        let root = temp_dir("codex-preview-noise");
        let cwd = root.join("work");
        fs::create_dir_all(root.join(".codex/sessions/2026/05/27")).unwrap();
        fs::create_dir_all(&cwd).unwrap();
        fs::write(
            root.join(".codex/sessions/2026/05/27/rollout-test.jsonl"),
            format!(
                "{{\"timestamp\":\"2026-05-01T00:00:00Z\",\"type\":\"session_meta\",\"payload\":{{\"id\":\"abc\",\"cwd\":\"{}\"}}}}\n{{\"timestamp\":\"2026-05-01T00:01:00Z\",\"type\":\"response_item\",\"payload\":{{\"type\":\"message\",\"role\":\"user\",\"content\":[{{\"type\":\"input_text\",\"text\":\"# AGENTS.md instructions for /tmp/project\\n\\n<INSTRUCTIONS>details</INSTRUCTIONS>\"}}]}}}}\n{{\"timestamp\":\"2026-05-01T00:02:00Z\",\"type\":\"event_msg\",\"payload\":{{\"type\":\"user_message\",\"message\":\"real user request\"}}}}\n",
                cwd.display()
            ),
        )
        .unwrap();

        let threads = CodexProvider::with_home(root).list_threads(&cwd).unwrap();
        assert_eq!(threads.len(), 1);
        assert_eq!(threads[0].name, None);
        assert_eq!(threads[0].preview.as_deref(), Some("real user request"));
    }

    #[test]
    fn renames_codex_thread() {
        let root = temp_dir("codex-rename");
        let cwd = root.join("work");
        fs::create_dir_all(root.join(".codex/sessions/2026/05/27")).unwrap();
        fs::create_dir_all(&cwd).unwrap();
        fs::write(
            root.join(".codex/sessions/2026/05/27/rollout-test.jsonl"),
            format!(
                "{{\"timestamp\":\"2026-05-01T00:00:00Z\",\"type\":\"session_meta\",\"payload\":{{\"id\":\"abc\",\"cwd\":\"{}\"}}}}\n",
                cwd.display()
            ),
        )
        .unwrap();
        let provider = CodexProvider::with_home(root);
        let thread = provider.list_threads(&cwd).unwrap().remove(0);

        provider.rename_thread(&thread, "new-codex-name").unwrap();

        let renamed = provider.list_threads(&cwd).unwrap().remove(0);
        assert_eq!(renamed.name.as_deref(), Some("new-codex-name"));
    }

    #[test]
    fn unsets_codex_thread_name() {
        let root = temp_dir("codex-unset");
        let cwd = root.join("work");
        fs::create_dir_all(root.join(".codex/sessions/2026/05/27")).unwrap();
        fs::create_dir_all(&cwd).unwrap();
        fs::write(
            root.join(".codex/session_index.jsonl"),
            "{\"id\":\"abc\",\"thread_name\":\"old-codex-name\",\"updated_at\":\"2026-05-01T00:00:00Z\"}\n",
        )
        .unwrap();
        fs::write(
            root.join(".codex/sessions/2026/05/27/rollout-test.jsonl"),
            format!(
                "{{\"timestamp\":\"2026-05-01T00:00:00Z\",\"type\":\"session_meta\",\"payload\":{{\"id\":\"abc\",\"cwd\":\"{}\"}}}}\n",
                cwd.display()
            ),
        )
        .unwrap();
        let provider = CodexProvider::with_home(root);
        let thread = provider.list_threads(&cwd).unwrap().remove(0);

        provider.unset_thread_name(&thread).unwrap();

        let renamed = provider.list_threads(&cwd).unwrap().remove(0);
        assert_eq!(renamed.name, None);
    }

    #[test]
    fn list_threads_includes_subdirectories() {
        let root = temp_dir("codex-subdir");
        let cwd = root.join("work");
        let child = cwd.join("child");
        fs::create_dir_all(root.join(".codex/sessions/2026/05/27")).unwrap();
        fs::create_dir_all(&child).unwrap();
        fs::write(
            root.join(".codex/sessions/2026/05/27/rollout-child.jsonl"),
            format!(
                "{{\"timestamp\":\"2026-05-01T00:00:00Z\",\"type\":\"session_meta\",\"payload\":{{\"id\":\"child\",\"cwd\":\"{}\"}}}}\n",
                child.display()
            ),
        )
        .unwrap();

        let threads = CodexProvider::with_home(root).list_threads(&cwd).unwrap();
        assert_eq!(threads.len(), 1);
        assert_eq!(threads[0].id, "child");
        assert_eq!(threads[0].cwd, canonicalize_existing(&child));
    }
}
