use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::common::{
    AgentKind, AgentProvider, LaunchCommand, LhResult, MemoryFile, RemovalTarget, ResumeHint,
    ThreadSummary, default_executable, markdown_memory_file,
};
use crate::util::{
    canonicalize_existing, collect_files, home_dir, parse_time, path_is_at_or_under, read_to_string,
};

pub struct GeminiProvider {
    home: PathBuf,
}

impl GeminiProvider {
    pub fn new() -> Self {
        Self { home: home_dir() }
    }

    #[cfg(test)]
    pub fn with_home(home: PathBuf) -> Self {
        Self { home }
    }

    fn tmp_dir(&self) -> PathBuf {
        self.home.join(".gemini/tmp")
    }
}

impl AgentProvider for GeminiProvider {
    fn kind(&self) -> AgentKind {
        AgentKind::Gemini
    }

    fn history_path(&self, _cwd: &Path) -> PathBuf {
        self.tmp_dir()
    }

    fn executable(&self) -> Option<PathBuf> {
        crate::util::find_executable("gemini")
    }

    fn list_threads(&self, cwd: &Path) -> LhResult<Vec<ThreadSummary>> {
        let canonical_cwd = canonicalize_existing(cwd);
        let Ok(entries) = fs::read_dir(self.tmp_dir()) else {
            return Ok(Vec::new());
        };

        let mut threads = Vec::new();
        for entry in entries.flatten() {
            let project_dir = entry.path();
            if !project_dir.is_dir() {
                continue;
            }
            let Some(project_root) = read_to_string(&project_dir.join(".project_root")) else {
                continue;
            };
            let project_root = canonicalize_existing(Path::new(project_root.trim()));
            if path_is_at_or_under(&project_root, &canonical_cwd) {
                threads.extend(self.list_project_dir(&project_dir, &project_root));
            }
        }
        threads.sort_by_key(|thread| std::cmp::Reverse(thread.updated_sort_key()));
        Ok(threads)
    }

    fn list_threads_global(&self) -> LhResult<Vec<ThreadSummary>> {
        let Ok(entries) = fs::read_dir(self.tmp_dir()) else {
            return Ok(Vec::new());
        };

        let mut threads = Vec::new();
        for entry in entries.flatten() {
            let project_dir = entry.path();
            if !project_dir.is_dir() {
                continue;
            }
            let Some(project_root) = read_to_string(&project_dir.join(".project_root")) else {
                continue;
            };
            let cwd = canonicalize_existing(Path::new(project_root.trim()));
            threads.extend(self.list_project_dir(&project_dir, &cwd));
        }
        threads.sort_by_key(|thread| std::cmp::Reverse(thread.updated_sort_key()));
        Ok(threads)
    }

    fn list_memory(&self, cwd: &Path) -> LhResult<Vec<MemoryFile>> {
        let mut memories = self.context_files_for_dir(&canonicalize_existing(cwd));
        sort_dedup_memory(&mut memories);
        Ok(memories)
    }

    fn list_memory_global(&self) -> LhResult<Vec<MemoryFile>> {
        let mut memories = Vec::new();
        if let Some(memory) = markdown_memory_file(
            AgentKind::Gemini,
            "global",
            None,
            self.home.join(".gemini/GEMINI.md"),
        ) {
            memories.push(memory);
        }

        for root in self.known_project_roots() {
            memories.extend(self.context_files_for_dir(&root));
        }
        sort_dedup_memory(&mut memories);
        Ok(memories)
    }

    fn new_command(&self, _name: Option<&str>, _cwd: &Path) -> LhResult<LaunchCommand> {
        Ok(LaunchCommand::new(
            default_executable("gemini"),
            [] as [OsString; 0],
        ))
    }

    fn resume_command(&self, thread: Option<&ThreadSummary>) -> LhResult<LaunchCommand> {
        if let Some(ThreadSummary {
            resume_hint: Some(ResumeHint::GeminiSessionFile(path)),
            ..
        }) = thread
        {
            return Ok(LaunchCommand::new(
                default_executable("gemini"),
                [
                    OsString::from("--session-file"),
                    path.as_os_str().to_os_string(),
                ],
            ));
        }

        Ok(LaunchCommand::new(
            default_executable("gemini"),
            [OsString::from("--resume"), OsString::from("latest")],
        ))
    }
}

impl GeminiProvider {
    fn context_files_for_dir(&self, cwd: &Path) -> Vec<MemoryFile> {
        let mut memories = Vec::new();
        if let Some(memory) = markdown_memory_file(
            AgentKind::Gemini,
            "global",
            None,
            self.home.join(".gemini/GEMINI.md"),
        ) {
            memories.push(memory);
        }

        let mut current = Some(cwd);
        while let Some(dir) = current {
            let path = dir.join("GEMINI.md");
            if let Some(memory) =
                markdown_memory_file(AgentKind::Gemini, "project", Some(dir.to_path_buf()), path)
            {
                memories.push(memory);
            }
            if dir.join(".git").exists() {
                break;
            }
            current = dir.parent();
        }

        collect_files(cwd, &mut |path| {
            if path.file_name().and_then(|name| name.to_str()) == Some("GEMINI.md")
                && let Some(parent) = path.parent()
                && let Some(memory) = markdown_memory_file(
                    AgentKind::Gemini,
                    "project",
                    Some(parent.to_path_buf()),
                    path.to_path_buf(),
                )
            {
                memories.push(memory);
            }
        });

        memories
    }

    fn known_project_roots(&self) -> Vec<PathBuf> {
        let mut roots = Vec::new();
        for dir in [
            self.home.join(".gemini/tmp"),
            self.home.join(".gemini/history"),
        ] {
            let Ok(entries) = fs::read_dir(dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let project_dir = entry.path();
                if !project_dir.is_dir() {
                    continue;
                }
                let Some(project_root) = read_to_string(&project_dir.join(".project_root")) else {
                    continue;
                };
                roots.push(canonicalize_existing(Path::new(project_root.trim())));
            }
        }
        roots.sort();
        roots.dedup();
        roots
    }

    fn list_project_dir(&self, project_dir: &Path, cwd: &Path) -> Vec<ThreadSummary> {
        let logs_path = project_dir.join("logs.json");
        let logs = read_gemini_logs(&logs_path);
        let chats_dir = project_dir.join("chats");
        let Ok(entries) = fs::read_dir(&chats_dir) else {
            return Vec::new();
        };

        let mut threads = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("jsonl") {
                continue;
            }
            if let Some(thread) = parse_gemini_chat(&path, logs.as_ref(), &logs_path, cwd) {
                threads.push(thread);
            }
        }
        threads.sort_by_key(|thread| std::cmp::Reverse(thread.updated_sort_key()));
        threads
    }
}

fn sort_dedup_memory(memories: &mut Vec<MemoryFile>) {
    memories.sort_by_key(|memory| std::cmp::Reverse(memory.updated_sort_key()));
    let mut seen = std::collections::HashSet::new();
    memories.retain(|memory| seen.insert(memory.path.clone()));
}

fn read_gemini_logs(path: &Path) -> Option<Vec<Value>> {
    let text = fs::read_to_string(path).ok()?;
    serde_json::from_str::<Vec<Value>>(&text).ok()
}

fn parse_gemini_chat(
    path: &Path,
    logs: Option<&Vec<Value>>,
    logs_path: &Path,
    cwd: &Path,
) -> Option<ThreadSummary> {
    let text = fs::read_to_string(path).ok()?;
    let first = text.lines().find(|line| !line.trim().is_empty())?;
    let value = serde_json::from_str::<Value>(first).ok()?;
    let id = value
        .get("sessionId")
        .and_then(|value| value.as_str())
        .map(ToString::to_string)
        .or_else(|| {
            path.file_stem()
                .and_then(|name| name.to_str())
                .map(ToString::to_string)
        })?;
    let created_at = value
        .get("startTime")
        .and_then(|value| value.as_str())
        .and_then(parse_time);
    let mut updated_at = value
        .get("lastUpdated")
        .and_then(|value| value.as_str())
        .and_then(parse_time)
        .or(created_at);
    let mut preview = None;

    if let Some(logs) = logs {
        for log in logs {
            if log.get("sessionId").and_then(|value| value.as_str()) != Some(id.as_str()) {
                continue;
            }
            if preview.is_none() && log.get("type").and_then(|value| value.as_str()) == Some("user")
            {
                preview = log
                    .get("message")
                    .and_then(|value| value.as_str())
                    .map(ToString::to_string);
            }
            if let Some(timestamp) = log
                .get("timestamp")
                .and_then(|value| value.as_str())
                .and_then(parse_time)
            {
                updated_at = Some(
                    updated_at.map_or(timestamp, |current: time::OffsetDateTime| {
                        current.max(timestamp)
                    }),
                );
            }
        }
    }

    Some(ThreadSummary {
        agent: AgentKind::Gemini,
        id: id.clone(),
        name: preview
            .clone()
            .map(|value| crate::common::truncate(&value, 64)),
        cwd: cwd.to_path_buf(),
        created_at,
        updated_at,
        source_path: Some(path.to_path_buf()),
        preview,
        removable: Some(RemovalTarget::GeminiFiles {
            chat_path: path.to_path_buf(),
            logs_path: logs_path.exists().then(|| logs_path.to_path_buf()),
            session_id: id,
        }),
        resume_hint: Some(ResumeHint::GeminiSessionFile(path.to_path_buf())),
    })
}

pub fn delete_gemini_files(
    chat_path: &Path,
    logs_path: Option<&Path>,
    session_id: &str,
) -> LhResult<()> {
    if chat_path.exists() {
        fs::remove_file(chat_path)?;
    }

    if let Some(logs_path) = logs_path.filter(|path| path.exists()) {
        let logs = read_gemini_logs(logs_path).unwrap_or_default();
        let filtered = logs
            .into_iter()
            .filter(|entry| {
                entry.get("sessionId").and_then(|value| value.as_str()) != Some(session_id)
            })
            .collect::<Vec<_>>();
        fs::write(logs_path, serde_json::to_string_pretty(&filtered)?)?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::util::temp_dir;

    #[test]
    fn parses_gemini_fixture() {
        let root = temp_dir("gemini");
        let cwd = root.join("work");
        let project = root.join(".gemini/tmp/lh");
        fs::create_dir_all(project.join("chats")).unwrap();
        fs::create_dir_all(&cwd).unwrap();
        fs::write(
            project.join(".project_root"),
            cwd.to_string_lossy().as_bytes(),
        )
        .unwrap();
        fs::write(
            project.join("chats/session.jsonl"),
            "{\"sessionId\":\"g\",\"startTime\":\"2026-05-01T00:00:00Z\",\"lastUpdated\":\"2026-05-01T00:00:00Z\"}\n",
        )
        .unwrap();
        fs::write(
            project.join("logs.json"),
            "[{\"sessionId\":\"g\",\"type\":\"user\",\"message\":\"hello gemini\",\"timestamp\":\"2026-05-01T00:01:00Z\"}]",
        )
        .unwrap();

        let threads = GeminiProvider::with_home(root).list_threads(&cwd).unwrap();
        assert_eq!(threads.len(), 1);
        assert_eq!(threads[0].preview.as_deref(), Some("hello gemini"));
    }

    #[test]
    fn lists_gemini_context_memory() {
        let root = temp_dir("gemini-memory");
        let cwd = root.join("work");
        let child = cwd.join("child");
        fs::create_dir_all(root.join(".gemini")).unwrap();
        fs::create_dir_all(&child).unwrap();
        fs::write(root.join(".gemini/GEMINI.md"), "global gemini memory").unwrap();
        fs::write(cwd.join("GEMINI.md"), "project gemini memory").unwrap();

        let memories = GeminiProvider::with_home(root).list_memory(&child).unwrap();
        assert_eq!(memories.len(), 2);
        assert!(memories.iter().any(|memory| memory.scope == "global"
            && memory.preview.as_deref() == Some("global gemini memory")));
        assert!(memories.iter().any(|memory| memory.scope == "project"
            && memory.preview.as_deref() == Some("project gemini memory")));
    }

    #[test]
    fn list_threads_includes_subdirectories() {
        let root = temp_dir("gemini-subdir");
        let cwd = root.join("work");
        let child = cwd.join("child");
        let project = root.join(".gemini/tmp/child");
        fs::create_dir_all(project.join("chats")).unwrap();
        fs::create_dir_all(&child).unwrap();
        fs::write(
            project.join(".project_root"),
            child.to_string_lossy().as_bytes(),
        )
        .unwrap();
        fs::write(
            project.join("chats/session.jsonl"),
            "{\"sessionId\":\"g\",\"startTime\":\"2026-05-01T00:00:00Z\",\"lastUpdated\":\"2026-05-01T00:00:00Z\"}\n",
        )
        .unwrap();

        let threads = GeminiProvider::with_home(root).list_threads(&cwd).unwrap();
        assert_eq!(threads.len(), 1);
        assert_eq!(threads[0].cwd, canonicalize_existing(&child));
    }
}
