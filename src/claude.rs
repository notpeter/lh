use std::collections::HashMap;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::common::{
    AgentKind, AgentProvider, LaunchCommand, LhResult, RemovalTarget, ThreadSummary,
    default_executable,
};
use crate::util::{
    canonicalize_existing, first_json_text, home_dir, parse_time, path_is_at_or_under,
};

pub struct ClaudeProvider {
    home: PathBuf,
}

impl ClaudeProvider {
    pub fn new() -> Self {
        Self { home: home_dir() }
    }

    #[cfg(test)]
    pub fn with_home(home: PathBuf) -> Self {
        Self { home }
    }

    pub fn project_dir_for(&self, cwd: &Path) -> PathBuf {
        self.home
            .join(".claude/projects")
            .join(encode_project_path(&canonicalize_existing(cwd)))
    }

    fn sessions_dir(&self) -> PathBuf {
        self.home.join(".claude/sessions")
    }
}

impl AgentProvider for ClaudeProvider {
    fn kind(&self) -> AgentKind {
        AgentKind::Claude
    }

    fn history_path(&self, cwd: &Path) -> PathBuf {
        self.project_dir_for(cwd)
    }

    fn executable(&self) -> Option<PathBuf> {
        crate::util::find_executable("claude")
    }

    fn list_threads(&self, cwd: &Path) -> LhResult<Vec<ThreadSummary>> {
        let canonical_cwd = canonicalize_existing(cwd);
        let projects_dir = self.home.join(".claude/projects");
        let Ok(entries) = fs::read_dir(projects_dir) else {
            return Ok(Vec::new());
        };
        let names = read_session_names(&self.sessions_dir());
        let mut threads = Vec::new();
        for entry in entries.flatten() {
            let project_dir = entry.path();
            if !project_dir.is_dir() {
                continue;
            }
            threads.extend(self.list_project_dir(&project_dir, Some(&canonical_cwd), &names));
        }
        threads.sort_by_key(|thread| std::cmp::Reverse(thread.updated_sort_key()));
        Ok(threads)
    }

    fn list_threads_global(&self) -> LhResult<Vec<ThreadSummary>> {
        let projects_dir = self.home.join(".claude/projects");
        let Ok(entries) = fs::read_dir(projects_dir) else {
            return Ok(Vec::new());
        };
        let names = read_session_names(&self.sessions_dir());
        let mut threads = Vec::new();
        for entry in entries.flatten() {
            let project_dir = entry.path();
            if !project_dir.is_dir() {
                continue;
            }
            threads.extend(self.list_project_dir(&project_dir, None, &names));
        }
        threads.sort_by_key(|thread| std::cmp::Reverse(thread.updated_sort_key()));
        Ok(threads)
    }

    fn new_command(&self, name: Option<&str>, _cwd: &Path) -> LhResult<LaunchCommand> {
        let mut args = Vec::new();
        if let Some(name) = name {
            args.push(OsString::from("--name"));
            args.push(OsString::from(name));
        }
        Ok(LaunchCommand::new(default_executable("claude"), args))
    }

    fn resume_command(&self, thread: Option<&ThreadSummary>) -> LhResult<LaunchCommand> {
        let thread = thread.ok_or("no Claude thread selected")?;
        Ok(LaunchCommand::new(
            default_executable("claude"),
            [OsString::from("--resume"), OsString::from(&thread.id)],
        ))
    }
}

impl ClaudeProvider {
    fn list_project_dir(
        &self,
        project_dir: &Path,
        cwd_filter: Option<&Path>,
        names: &HashMap<String, String>,
    ) -> Vec<ThreadSummary> {
        let Ok(entries) = fs::read_dir(project_dir) else {
            return Vec::new();
        };

        let fallback_cwd = project_dir
            .file_name()
            .and_then(|name| name.to_str())
            .map(decode_project_path)
            .unwrap_or_else(|| PathBuf::from("."));
        let mut threads = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("jsonl") {
                continue;
            }
            if let Some(thread) = parse_claude_jsonl(&path, cwd_filter, &fallback_cwd, names) {
                threads.push(thread);
            }
        }
        threads.sort_by_key(|thread| std::cmp::Reverse(thread.updated_sort_key()));
        threads
    }
}

fn parse_claude_jsonl(
    path: &Path,
    cwd_filter: Option<&Path>,
    fallback_cwd: &Path,
    names: &HashMap<String, String>,
) -> Option<ThreadSummary> {
    let text = fs::read_to_string(path).ok()?;
    let mut id = None;
    let mut created_at = None;
    let mut updated_at = None;
    let mut preview = None;
    let mut custom_title = None;
    let mut cwd_from_file = None;

    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };

        if id.is_none() {
            id = value
                .get("sessionId")
                .or_else(|| value.get("session_id"))
                .and_then(|value| value.as_str())
                .map(ToString::to_string);
        }

        if cwd_from_file.is_none() {
            cwd_from_file = value
                .get("cwd")
                .and_then(|value| value.as_str())
                .map(PathBuf::from);
        }

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

        if preview.is_none() && value.get("type").and_then(|value| value.as_str()) == Some("user") {
            preview = value
                .get("message")
                .and_then(|message| message.get("content"))
                .and_then(first_json_text);
        }

        match value.get("type").and_then(|value| value.as_str()) {
            Some("custom-title") => {
                custom_title = value
                    .get("customTitle")
                    .and_then(|value| value.as_str())
                    .map(str::trim)
                    .filter(|title| !title.is_empty())
                    .map(ToString::to_string);
            }
            Some("agent-name") if custom_title.is_none() => {
                custom_title = value
                    .get("agentName")
                    .and_then(|value| value.as_str())
                    .map(str::trim)
                    .filter(|title| !title.is_empty())
                    .map(ToString::to_string);
            }
            _ => {}
        }
    }

    let cwd = cwd_from_file
        .map(|path| canonicalize_existing(&path))
        .unwrap_or_else(|| fallback_cwd.to_path_buf());

    if let Some(cwd_filter) = cwd_filter
        && !path_is_at_or_under(&cwd, cwd_filter)
    {
        return None;
    }

    let id = id.or_else(|| {
        path.file_stem()
            .and_then(|name| name.to_str())
            .map(ToString::to_string)
    })?;
    let name = custom_title.or_else(|| names.get(&id).cloned());

    Some(ThreadSummary {
        agent: AgentKind::Claude,
        id,
        name,
        cwd,
        created_at,
        updated_at,
        source_path: Some(path.to_path_buf()),
        preview,
        removable: Some(RemovalTarget::File(path.to_path_buf())),
        resume_hint: None,
    })
}

fn read_session_names(sessions_dir: &Path) -> HashMap<String, String> {
    let Ok(entries) = fs::read_dir(sessions_dir) else {
        return HashMap::new();
    };

    let mut names = HashMap::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        let Some(session_id) = value.get("sessionId").and_then(|value| value.as_str()) else {
            continue;
        };
        let Some(name) = value
            .get("name")
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|name| !name.is_empty())
        else {
            continue;
        };
        names.insert(session_id.to_string(), name.to_string());
    }
    names
}

pub fn encode_project_path(path: &Path) -> String {
    path.to_string_lossy().replace('/', "-")
}

fn decode_project_path(value: &str) -> PathBuf {
    if let Some(rest) = value.strip_prefix('-') {
        PathBuf::from(format!("/{rest}").replace('-', "/"))
    } else {
        PathBuf::from(value.replace('-', "/"))
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::util::temp_dir;

    #[test]
    fn encodes_claude_project_path() {
        assert_eq!(
            encode_project_path(Path::new("/Users/peter/code/lh")),
            "-Users-peter-code-lh"
        );
    }

    #[test]
    fn parses_claude_fixture() {
        let root = temp_dir("claude");
        let cwd = root.join("work");
        fs::create_dir_all(&cwd).unwrap();
        fs::create_dir_all(root.join(".claude/sessions")).unwrap();
        let provider = ClaudeProvider::with_home(root.clone());
        let project_dir = provider.project_dir_for(&cwd);
        fs::create_dir_all(&project_dir).unwrap();
        fs::write(
            root.join(".claude/sessions/abc.json"),
            "{\"sessionId\":\"abc\",\"name\":\"named claude\"}\n",
        )
        .unwrap();
        fs::write(
            project_dir.join("abc.jsonl"),
            format!(
                "{{\"type\":\"user\",\"sessionId\":\"abc\",\"cwd\":\"{}\",\"timestamp\":\"2026-05-01T00:00:00Z\",\"message\":{{\"content\":\"hello claude\"}}}}\n",
                cwd.display()
            ),
        )
        .unwrap();

        let threads = provider.list_threads(&cwd).unwrap();
        assert_eq!(threads.len(), 1);
        assert_eq!(threads[0].id, "abc");
        assert_eq!(threads[0].name.as_deref(), Some("named claude"));
        assert_eq!(threads[0].preview.as_deref(), Some("hello claude"));
    }

    #[test]
    fn leaves_name_empty_when_claude_session_name_is_missing() {
        let root = temp_dir("claude-preview-name");
        let cwd = root.join("work");
        fs::create_dir_all(&cwd).unwrap();
        let provider = ClaudeProvider::with_home(root);
        let project_dir = provider.project_dir_for(&cwd);
        fs::create_dir_all(&project_dir).unwrap();
        fs::write(
            project_dir.join("abc.jsonl"),
            format!(
                "{{\"type\":\"user\",\"sessionId\":\"abc\",\"cwd\":\"{}\",\"timestamp\":\"2026-05-01T00:00:00Z\",\"message\":{{\"content\":\"hello claude\"}}}}\n",
                cwd.display()
            ),
        )
        .unwrap();

        let threads = provider.list_threads(&cwd).unwrap();
        assert_eq!(threads.len(), 1);
        assert_eq!(threads[0].name, None);
        assert_eq!(threads[0].preview.as_deref(), Some("hello claude"));
    }

    #[test]
    fn parses_claude_custom_title_from_transcript() {
        let root = temp_dir("claude-custom-title");
        let cwd = root.join("work");
        fs::create_dir_all(&cwd).unwrap();
        let provider = ClaudeProvider::with_home(root);
        let project_dir = provider.project_dir_for(&cwd);
        fs::create_dir_all(&project_dir).unwrap();
        fs::write(
            project_dir.join("abc.jsonl"),
            format!(
                "{{\"type\":\"user\",\"sessionId\":\"abc\",\"cwd\":\"{}\",\"timestamp\":\"2026-05-01T00:00:00Z\",\"message\":{{\"content\":\"hello claude\"}}}}\n{{\"type\":\"custom-title\",\"customTitle\":\"renamed claude\",\"sessionId\":\"abc\"}}\n{{\"type\":\"agent-name\",\"agentName\":\"ignored fallback\",\"sessionId\":\"abc\"}}\n",
                cwd.display()
            ),
        )
        .unwrap();

        let threads = provider.list_threads(&cwd).unwrap();
        assert_eq!(threads.len(), 1);
        assert_eq!(threads[0].name.as_deref(), Some("renamed claude"));
        assert_eq!(threads[0].preview.as_deref(), Some("hello claude"));
    }

    #[test]
    fn list_threads_includes_subdirectories() {
        let root = temp_dir("claude-subdir");
        let cwd = root.join("work");
        let child = cwd.join("child");
        fs::create_dir_all(&child).unwrap();
        let provider = ClaudeProvider::with_home(root);
        let project_dir = provider.project_dir_for(&child);
        fs::create_dir_all(&project_dir).unwrap();
        fs::write(
            project_dir.join("abc.jsonl"),
            format!(
                "{{\"type\":\"user\",\"sessionId\":\"abc\",\"cwd\":\"{}\",\"timestamp\":\"2026-05-01T00:00:00Z\",\"message\":{{\"content\":\"hello child\"}}}}\n",
                child.display()
            ),
        )
        .unwrap();

        let threads = provider.list_threads(&cwd).unwrap();
        assert_eq!(threads.len(), 1);
        assert_eq!(threads[0].id, "abc");
        assert_eq!(threads[0].cwd, canonicalize_existing(&child));
    }
}
