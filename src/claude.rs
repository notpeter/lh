use std::collections::HashMap;
use std::ffi::OsString;
use std::fs;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use time::OffsetDateTime;

use crate::common::{
    AgentKind, AgentProvider, LaunchCommand, LhResult, MemoryFile, RemovalTarget, ThreadSummary,
    default_executable, markdown_memory_file,
};
use crate::util::{
    canonicalize_existing, first_json_text, first_model_string_at_paths, format_time, home_dir,
    is_noise_preview_text, parse_time, path_is_at_or_under,
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

    fn project_dir_for(&self, cwd: &Path) -> PathBuf {
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

    fn history_path(&self, _cwd: &Path) -> PathBuf {
        self.home.join(".claude/projects")
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

    fn list_memory(&self, cwd: &Path) -> LhResult<Vec<MemoryFile>> {
        let canonical_cwd = canonicalize_existing(cwd);
        let projects_dir = self.home.join(".claude/projects");
        let mut exact_dirs = vec![canonical_cwd.clone()];
        if let Some(git_root) = git_root(&canonical_cwd)
            && !exact_dirs.contains(&git_root)
        {
            exact_dirs.push(git_root);
        }

        let mut memories = Vec::new();
        for dir in exact_dirs {
            memories.extend(
                self.list_memory_project_dir(
                    &projects_dir.join(encode_project_path(&dir)),
                    Some(dir),
                ),
            );
        }

        memories.extend(self.list_memory_global()?.into_iter().filter(|memory| {
            memory.cwd.as_ref().is_some_and(|memory_cwd| {
                path_is_at_or_under(&canonical_cwd, memory_cwd)
                    || path_is_at_or_under(memory_cwd, &canonical_cwd)
            })
        }));
        memories.sort_by_key(|memory| std::cmp::Reverse(memory.updated_sort_key()));
        let mut seen = std::collections::HashSet::new();
        memories.retain(|memory| seen.insert(memory.path.clone()));
        Ok(memories)
    }

    fn list_memory_global(&self) -> LhResult<Vec<MemoryFile>> {
        let projects_dir = self.home.join(".claude/projects");
        let Ok(entries) = fs::read_dir(projects_dir) else {
            return Ok(Vec::new());
        };

        let mut memories = Vec::new();
        for entry in entries.flatten() {
            let project_dir = entry.path();
            if !project_dir.is_dir() {
                continue;
            }
            let cwd = project_dir
                .file_name()
                .and_then(|name| name.to_str())
                .map(decode_project_path);
            memories.extend(self.list_memory_project_dir(&project_dir, cwd));
        }
        memories.sort_by_key(|memory| std::cmp::Reverse(memory.updated_sort_key()));
        Ok(memories)
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

    fn supports_rename(&self) -> bool {
        true
    }

    fn rename_thread(&self, thread: &ThreadSummary, name: &str) -> LhResult<()> {
        let path = thread
            .source_path
            .as_ref()
            .ok_or("Claude thread is missing its transcript path")?;
        let timestamp = format_time(OffsetDateTime::now_utc());
        let mut file = OpenOptions::new().append(true).open(path)?;
        writeln!(
            file,
            "{}",
            json!({
                "type": "custom-title",
                "customTitle": name,
                "sessionId": thread.id,
                "timestamp": timestamp,
            })
        )?;
        writeln!(
            file,
            "{}",
            json!({
                "type": "agent-name",
                "agentName": name,
                "sessionId": thread.id,
                "timestamp": timestamp,
            })
        )?;
        set_session_name(&self.sessions_dir(), &thread.id, Some(name))?;
        Ok(())
    }

    fn unset_thread_name(&self, thread: &ThreadSummary) -> LhResult<()> {
        let path = thread
            .source_path
            .as_ref()
            .ok_or("Claude thread is missing its transcript path")?;
        let timestamp = format_time(OffsetDateTime::now_utc());
        let mut file = OpenOptions::new().append(true).open(path)?;
        writeln!(
            file,
            "{}",
            json!({
                "type": "custom-title",
                "customTitle": "",
                "sessionId": thread.id,
                "timestamp": timestamp,
            })
        )?;
        writeln!(
            file,
            "{}",
            json!({
                "type": "agent-name",
                "agentName": "",
                "sessionId": thread.id,
                "timestamp": timestamp,
            })
        )?;
        set_session_name(&self.sessions_dir(), &thread.id, None)?;
        Ok(())
    }

    fn supports_move_thread(&self) -> bool {
        true
    }

    fn move_thread(&self, thread: &ThreadSummary, target_cwd: &Path) -> LhResult<()> {
        let source_path = thread
            .source_path
            .as_ref()
            .ok_or("Claude thread is missing its transcript path")?;
        let target_dir = self.project_dir_for(target_cwd);
        fs::create_dir_all(&target_dir)?;
        let file_name = source_path
            .file_name()
            .ok_or("Claude thread transcript path is missing a file name")?;
        let target_path = target_dir.join(file_name);
        if source_path != &target_path && target_path.exists() {
            return Err(format!(
                "target Claude transcript already exists: {}",
                target_path.display()
            )
            .into());
        }

        if source_path == &target_path {
            rewrite_jsonl_cwd(source_path, target_cwd)?;
        } else {
            let rewritten = rewrite_jsonl_cwd_text(&fs::read_to_string(source_path)?, target_cwd)?;
            fs::write(&target_path, rewritten)?;
            fs::remove_file(source_path)?;
        }
        Ok(())
    }

    fn thread_content(&self, thread: &ThreadSummary) -> LhResult<String> {
        let path = thread
            .source_path
            .as_ref()
            .ok_or("Claude thread is missing its transcript path")?;
        claude_thread_content(path)
    }
}

impl ClaudeProvider {
    fn list_memory_project_dir(&self, project_dir: &Path, cwd: Option<PathBuf>) -> Vec<MemoryFile> {
        let memory_dir = project_dir.join("memory");
        let Ok(entries) = fs::read_dir(memory_dir) else {
            return Vec::new();
        };

        let mut memories = Vec::new();
        for entry in entries.flatten() {
            if let Some(memory) =
                markdown_memory_file(AgentKind::Claude, "project", cwd.clone(), entry.path())
            {
                memories.push(memory);
            }
        }
        memories
    }

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
    let mut model = None;

    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        model = model.or_else(|| claude_model_from_event(&value));

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
                .and_then(first_json_text)
                .filter(|text| !is_noise_preview_text(text));
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
        model,
        cwd,
        created_at,
        updated_at,
        source_path: Some(path.to_path_buf()),
        preview,
        removable: Some(RemovalTarget::File(path.to_path_buf())),
    })
}

fn claude_model_from_event(value: &Value) -> Option<String> {
    first_model_string_at_paths(
        value,
        &[
            &["message", "model"],
            &["message", "model_id"],
            &["message", "modelId"],
            &["message", "modelName"],
            &["model"],
            &["model_id"],
            &["modelId"],
            &["modelName"],
        ],
    )
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

fn set_session_name(sessions_dir: &Path, session_id: &str, name: Option<&str>) -> LhResult<()> {
    let Ok(entries) = fs::read_dir(sessions_dir) else {
        return Ok(());
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        let Ok(mut value) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        if value.get("sessionId").and_then(|value| value.as_str()) != Some(session_id) {
            continue;
        }
        if let Some(name) = name {
            value["name"] = Value::String(name.to_string());
        } else if let Some(object) = value.as_object_mut() {
            object.remove("name");
        }
        fs::write(path, serde_json::to_string_pretty(&value)?)?;
    }
    Ok(())
}

fn claude_thread_content(path: &Path) -> LhResult<String> {
    let text = fs::read_to_string(path)?;
    let mut out = Vec::new();
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if value.get("isMeta").and_then(|value| value.as_bool()) == Some(true) {
            continue;
        }
        let Some(message) = value.get("message") else {
            continue;
        };
        let role = message
            .get("role")
            .and_then(|value| value.as_str())
            .or_else(|| value.get("type").and_then(|value| value.as_str()))
            .unwrap_or("message");
        let Some(content) = message.get("content").and_then(first_json_text) else {
            continue;
        };
        out.push(format!("{role}: {content}"));
    }
    Ok(out.join("\n\n"))
}

fn rewrite_jsonl_cwd(path: &Path, cwd: &Path) -> LhResult<()> {
    let text = fs::read_to_string(path)?;
    fs::write(path, rewrite_jsonl_cwd_text(&text, cwd)?)?;
    Ok(())
}

fn rewrite_jsonl_cwd_text(text: &str, cwd: &Path) -> LhResult<String> {
    let cwd = cwd.display().to_string();
    let mut out = String::new();
    for line in text.lines() {
        if line.trim().is_empty() {
            out.push('\n');
            continue;
        }
        match serde_json::from_str::<Value>(line) {
            Ok(mut value) => {
                if let Some(object) = value.as_object_mut()
                    && object.contains_key("cwd")
                {
                    object.insert("cwd".to_string(), Value::String(cwd.clone()));
                }
                out.push_str(&serde_json::to_string(&value)?);
            }
            Err(_) => out.push_str(line),
        }
        out.push('\n');
    }
    Ok(out)
}

fn encode_project_path(path: &Path) -> String {
    path.to_string_lossy().replace('/', "-")
}

fn decode_project_path(value: &str) -> PathBuf {
    if let Some(rest) = value.strip_prefix('-') {
        PathBuf::from(format!("/{rest}").replace('-', "/"))
    } else {
        PathBuf::from(value.replace('-', "/"))
    }
}

fn git_root(cwd: &Path) -> Option<PathBuf> {
    let mut current = Some(cwd);
    while let Some(dir) = current {
        if dir.join(".git").exists() {
            return Some(dir.to_path_buf());
        }
        current = dir.parent();
    }
    None
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
                "{{\"type\":\"user\",\"sessionId\":\"abc\",\"cwd\":\"{}\",\"timestamp\":\"2026-05-01T00:00:00Z\",\"message\":{{\"content\":\"hello claude\"}}}}\n{{\"type\":\"assistant\",\"sessionId\":\"abc\",\"cwd\":\"{}\",\"timestamp\":\"2026-05-01T00:01:00Z\",\"message\":{{\"model\":\"claude-sonnet-4-5\",\"content\":[{{\"type\":\"text\",\"text\":\"hi\"}}]}}}}\n",
                cwd.display(),
                cwd.display()
            ),
        )
        .unwrap();

        let threads = provider.list_threads(&cwd).unwrap();
        assert_eq!(threads.len(), 1);
        assert_eq!(threads[0].id, "abc");
        assert_eq!(threads[0].name.as_deref(), Some("named claude"));
        assert_eq!(threads[0].model.as_deref(), Some("claude-sonnet-4-5"));
        assert_eq!(threads[0].preview.as_deref(), Some("hello claude"));
    }

    #[test]
    fn lists_claude_project_memory() {
        let root = temp_dir("claude-memory");
        let cwd = root.join("work");
        let child = cwd.join("child");
        fs::create_dir_all(cwd.join(".git")).unwrap();
        fs::create_dir_all(&child).unwrap();
        let provider = ClaudeProvider::with_home(root);
        let project_dir = provider.project_dir_for(&cwd);
        fs::create_dir_all(project_dir.join("memory")).unwrap();
        fs::write(
            project_dir.join("memory/MEMORY.md"),
            "# Project memory\n\nremember this",
        )
        .unwrap();

        let memories = provider.list_memory(&child).unwrap();
        assert_eq!(memories.len(), 1);
        assert_eq!(memories[0].agent, AgentKind::Claude);
        assert_eq!(memories[0].scope, "project");
        assert_eq!(memories[0].id, "MEMORY.md");
        assert_eq!(memories[0].preview.as_deref(), Some("# Project memory"));
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
    fn skips_injected_claude_setup_when_building_preview() {
        let root = temp_dir("claude-preview-noise");
        let cwd = root.join("work");
        fs::create_dir_all(&cwd).unwrap();
        let provider = ClaudeProvider::with_home(root);
        let project_dir = provider.project_dir_for(&cwd);
        fs::create_dir_all(&project_dir).unwrap();
        fs::write(
            project_dir.join("abc.jsonl"),
            format!(
                "{{\"type\":\"user\",\"sessionId\":\"abc\",\"cwd\":\"{}\",\"timestamp\":\"2026-05-01T00:00:00Z\",\"message\":{{\"content\":\"# AGENTS.md instructions for /tmp/project\\n\\n<INSTRUCTIONS>details</INSTRUCTIONS>\"}}}}\n{{\"type\":\"user\",\"sessionId\":\"abc\",\"cwd\":\"{}\",\"timestamp\":\"2026-05-01T00:01:00Z\",\"message\":{{\"content\":\"real claude request\"}}}}\n",
                cwd.display(),
                cwd.display()
            ),
        )
        .unwrap();

        let threads = provider.list_threads(&cwd).unwrap();
        assert_eq!(threads.len(), 1);
        assert_eq!(threads[0].preview.as_deref(), Some("real claude request"));
    }

    #[test]
    fn skips_claude_local_command_wrappers_when_building_preview() {
        let root = temp_dir("claude-preview-command-wrapper");
        let cwd = root.join("work");
        fs::create_dir_all(&cwd).unwrap();
        let provider = ClaudeProvider::with_home(root);
        let project_dir = provider.project_dir_for(&cwd);
        fs::create_dir_all(&project_dir).unwrap();
        fs::write(
            project_dir.join("abc.jsonl"),
            format!(
                "{{\"type\":\"user\",\"sessionId\":\"abc\",\"cwd\":\"{}\",\"timestamp\":\"2026-05-01T00:00:00Z\",\"message\":{{\"content\":\"<command-name>/exit</command-name>\\n            <command-message>exit</command-message>\\n            <command-args></command-args>\"}}}}\n{{\"type\":\"user\",\"sessionId\":\"abc\",\"cwd\":\"{}\",\"timestamp\":\"2026-05-01T00:01:00Z\",\"message\":{{\"content\":\"<local-command-stdout>Goodbye!</local-command-stdout>\"}}}}\n",
                cwd.display(),
                cwd.display()
            ),
        )
        .unwrap();

        let threads = provider.list_threads(&cwd).unwrap();
        assert_eq!(threads.len(), 1);
        assert_eq!(threads[0].preview, None);
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
    fn renames_claude_thread() {
        let root = temp_dir("claude-rename");
        let cwd = root.join("work");
        fs::create_dir_all(&cwd).unwrap();
        fs::create_dir_all(root.join(".claude/sessions")).unwrap();
        fs::write(
            root.join(".claude/sessions/active.json"),
            "{\"sessionId\":\"abc\",\"name\":\"old-name\"}\n",
        )
        .unwrap();
        let provider = ClaudeProvider::with_home(root.clone());
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

        let thread = provider.list_threads(&cwd).unwrap().remove(0);
        provider.rename_thread(&thread, "new-name").unwrap();

        let renamed = provider.list_threads(&cwd).unwrap().remove(0);
        assert_eq!(renamed.name.as_deref(), Some("new-name"));
        let session = fs::read_to_string(root.join(".claude/sessions/active.json")).unwrap();
        assert!(session.contains("\"name\": \"new-name\""));
    }

    #[test]
    fn unsets_claude_thread_name() {
        let root = temp_dir("claude-unset");
        let cwd = root.join("work");
        fs::create_dir_all(&cwd).unwrap();
        fs::create_dir_all(root.join(".claude/sessions")).unwrap();
        fs::write(
            root.join(".claude/sessions/active.json"),
            "{\"sessionId\":\"abc\",\"name\":\"old-name\"}\n",
        )
        .unwrap();
        let provider = ClaudeProvider::with_home(root.clone());
        let project_dir = provider.project_dir_for(&cwd);
        fs::create_dir_all(&project_dir).unwrap();
        fs::write(
            project_dir.join("abc.jsonl"),
            format!(
                "{{\"type\":\"user\",\"sessionId\":\"abc\",\"cwd\":\"{}\",\"timestamp\":\"2026-05-01T00:00:00Z\",\"message\":{{\"content\":\"hello claude\"}}}}\n{{\"type\":\"custom-title\",\"customTitle\":\"old-name\",\"sessionId\":\"abc\"}}\n",
                cwd.display()
            ),
        )
        .unwrap();

        let thread = provider.list_threads(&cwd).unwrap().remove(0);
        provider.unset_thread_name(&thread).unwrap();

        let renamed = provider.list_threads(&cwd).unwrap().remove(0);
        assert_eq!(renamed.name, None);
        let session = fs::read_to_string(root.join(".claude/sessions/active.json")).unwrap();
        assert!(!session.contains("\"name\""));
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

    #[test]
    fn moves_claude_thread_to_target_project_dir() {
        let root = temp_dir("claude-move");
        let cwd = root.join("work");
        let target = root.join("target");
        fs::create_dir_all(&cwd).unwrap();
        fs::create_dir_all(&target).unwrap();
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
        let thread = provider.list_threads(&cwd).unwrap().remove(0);

        provider.move_thread(&thread, &target).unwrap();

        assert!(provider.list_threads(&cwd).unwrap().is_empty());
        let moved = provider.list_threads(&target).unwrap().remove(0);
        assert_eq!(moved.id, "abc");
        assert_eq!(moved.cwd, canonicalize_existing(&target));
        assert!(
            thread
                .source_path
                .unwrap()
                .parent()
                .unwrap()
                .read_dir()
                .unwrap()
                .next()
                .is_none()
        );
    }
}
