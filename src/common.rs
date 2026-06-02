use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use time::OffsetDateTime;

use crate::util::find_executable;

pub type LhResult<T> = Result<T, Box<dyn std::error::Error>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum AgentKind {
    Claude,
    Codex,
    OpenCode,
    Gemini,
}

impl AgentKind {
    pub fn as_str(self) -> &'static str {
        match self {
            AgentKind::Claude => "claude",
            AgentKind::Codex => "codex",
            AgentKind::OpenCode => "opencode",
            AgentKind::Gemini => "gemini",
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            AgentKind::Claude => "Claude",
            AgentKind::Codex => "Codex",
            AgentKind::OpenCode => "OpenCode",
            AgentKind::Gemini => "Gemini",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().as_str() {
            "claude" | "claude-code" | "claudecode" => Some(Self::Claude),
            "codex" | "openai-codex" | "openai" => Some(Self::Codex),
            "opencode" | "open-code" | "oc" => Some(Self::OpenCode),
            "gemini" | "gemini-cli" | "google-gemini" => Some(Self::Gemini),
            _ => None,
        }
    }
}

impl fmt::Display for AgentKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone)]
pub struct ThreadSummary {
    pub agent: AgentKind,
    pub id: String,
    pub name: Option<String>,
    pub cwd: PathBuf,
    pub created_at: Option<OffsetDateTime>,
    pub updated_at: Option<OffsetDateTime>,
    pub source_path: Option<PathBuf>,
    pub preview: Option<String>,
    pub removable: Option<RemovalTarget>,
    pub resume_hint: Option<ResumeHint>,
}

#[derive(Debug, Clone)]
pub struct MemoryFile {
    pub agent: AgentKind,
    pub id: String,
    pub scope: String,
    pub cwd: Option<PathBuf>,
    pub path: PathBuf,
    pub updated_at: Option<OffsetDateTime>,
    pub preview: Option<String>,
}

impl MemoryFile {
    pub fn updated_sort_key(&self) -> i128 {
        self.updated_at
            .map(|time| time.unix_timestamp_nanos())
            .unwrap_or_default()
    }
}

impl ThreadSummary {
    pub fn display_name(&self) -> String {
        self.name
            .clone()
            .or_else(|| self.preview.clone().map(|value| truncate(&value, 60)))
            .unwrap_or_else(|| self.id.clone())
    }

    pub fn updated_sort_key(&self) -> i128 {
        self.updated_at
            .or(self.created_at)
            .map(|time| time.unix_timestamp_nanos())
            .unwrap_or_default()
    }
}

#[derive(Debug, Clone)]
pub enum ResumeHint {
    GeminiSessionFile(PathBuf),
}

#[derive(Debug, Clone)]
pub enum RemovalTarget {
    File(PathBuf),
    Command(LaunchCommand),
    OpenCodeDb {
        db_path: PathBuf,
        session_id: String,
    },
    GeminiFiles {
        chat_path: PathBuf,
        logs_path: Option<PathBuf>,
        session_id: String,
    },
}

#[derive(Debug, Clone)]
pub struct LaunchCommand {
    pub program: OsString,
    pub args: Vec<OsString>,
}

impl LaunchCommand {
    pub fn new(
        program: impl Into<OsString>,
        args: impl IntoIterator<Item = impl Into<OsString>>,
    ) -> Self {
        Self {
            program: program.into(),
            args: args.into_iter().map(Into::into).collect(),
        }
    }

    pub fn display(&self) -> String {
        let mut parts = vec![self.program.to_string_lossy().into_owned()];
        parts.extend(
            self.args
                .iter()
                .map(|arg| arg.to_string_lossy().into_owned()),
        );
        parts.join(" ")
    }

    pub fn exec(self) -> LhResult<()> {
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            let err = Command::new(&self.program).args(&self.args).exec();
            Err(Box::new(err))
        }

        #[cfg(not(unix))]
        {
            let status = Command::new(&self.program).args(&self.args).status()?;
            if status.success() {
                Ok(())
            } else {
                Err(format!("command exited with {status}").into())
            }
        }
    }

    pub fn run(self) -> LhResult<()> {
        let status = Command::new(&self.program).args(&self.args).status()?;
        if status.success() {
            Ok(())
        } else {
            Err(format!("command exited with {status}: {}", self.display()).into())
        }
    }
}

#[derive(Debug, Clone)]
pub struct AgentStatus {
    pub agent: AgentKind,
    pub history_path: PathBuf,
    pub thread_count: usize,
    pub executable: Option<PathBuf>,
    pub version: Option<String>,
    pub caveat: Option<String>,
}

pub trait AgentProvider {
    fn kind(&self) -> AgentKind;
    fn history_path(&self, cwd: &std::path::Path) -> PathBuf;
    fn executable(&self) -> Option<PathBuf>;
    fn list_threads(&self, cwd: &std::path::Path) -> LhResult<Vec<ThreadSummary>>;
    fn list_threads_global(&self) -> LhResult<Vec<ThreadSummary>>;
    fn new_command(&self, name: Option<&str>, cwd: &std::path::Path) -> LhResult<LaunchCommand>;
    fn resume_command(&self, thread: Option<&ThreadSummary>) -> LhResult<LaunchCommand>;
    fn list_memory(&self, _cwd: &Path) -> LhResult<Vec<MemoryFile>> {
        Ok(Vec::new())
    }
    fn list_memory_global(&self) -> LhResult<Vec<MemoryFile>> {
        Ok(Vec::new())
    }
    fn supports_rename(&self) -> bool {
        false
    }
    fn rename_thread(&self, _thread: &ThreadSummary, _name: &str) -> LhResult<()> {
        Err(format!("{} does not support native rename", self.kind()).into())
    }
    fn unset_thread_name(&self, _thread: &ThreadSummary) -> LhResult<()> {
        Err(format!("{} does not support native rename unset", self.kind()).into())
    }
    fn thread_content(&self, thread: &ThreadSummary) -> LhResult<String> {
        let path = thread
            .source_path
            .as_ref()
            .ok_or("selected thread does not expose source content")?;
        Ok(std::fs::read_to_string(path)?)
    }

    fn status(&self, cwd: &std::path::Path) -> AgentStatus {
        let history_path = self.history_path(cwd);
        let executable = self.executable();
        let thread_count = self
            .list_threads_global()
            .map(|threads| threads.len())
            .unwrap_or_default();
        let history_exists = history_path.exists() || thread_count > 0;
        let version = executable.as_ref().and_then(|path| command_version(path));
        let caveat = match (history_exists, executable.is_some()) {
            (true, false) => Some("history found, executable missing".to_string()),
            (false, true) => Some("no history found".to_string()),
            (false, false) => Some("history and executable missing".to_string()),
            (true, true) => None,
        };

        AgentStatus {
            agent: self.kind(),
            history_path,
            thread_count,
            executable,
            version,
            caveat,
        }
    }
}

pub fn markdown_memory_file(
    agent: AgentKind,
    scope: impl Into<String>,
    cwd: Option<PathBuf>,
    path: PathBuf,
) -> Option<MemoryFile> {
    if path.extension().and_then(|ext| ext.to_str()) != Some("md") || !path.is_file() {
        return None;
    }

    let id = path.file_name()?.to_str()?.to_string();
    let preview = fs::read_to_string(&path).ok().and_then(|text| {
        text.lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .map(ToString::to_string)
    });
    let updated_at = fs::metadata(&path)
        .ok()
        .and_then(|metadata| metadata.modified().ok())
        .map(OffsetDateTime::from);

    Some(MemoryFile {
        agent,
        id,
        scope: scope.into(),
        cwd,
        path,
        updated_at,
        preview,
    })
}

pub fn default_executable(name: &str) -> PathBuf {
    find_executable(name).unwrap_or_else(|| PathBuf::from(name))
}

pub fn truncate(value: &str, max_chars: usize) -> String {
    let value = value.trim().replace('\n', " ");
    if value.chars().count() <= max_chars {
        return value;
    }
    if max_chars <= 3 {
        return ".".repeat(max_chars);
    }
    let mut out = value.chars().take(max_chars - 3).collect::<String>();
    out.push_str("...");
    out
}

fn command_version(path: &std::path::Path) -> Option<String> {
    let output = Command::new(path).arg("--version").output().ok()?;
    let text = if output.stdout.is_empty() {
        String::from_utf8_lossy(&output.stderr).into_owned()
    } else {
        String::from_utf8_lossy(&output.stdout).into_owned()
    };
    let text = text.lines().next()?.trim();
    (!text.is_empty()).then(|| text.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_agent_aliases() {
        assert_eq!(AgentKind::parse("claude-code"), Some(AgentKind::Claude));
        assert_eq!(AgentKind::parse("gemini-cli"), Some(AgentKind::Gemini));
        assert_eq!(AgentKind::parse("open-code"), Some(AgentKind::OpenCode));
        assert_eq!(AgentKind::parse("nope"), None);
    }

    #[test]
    fn truncate_respects_tiny_widths() {
        assert_eq!(truncate("abcd", 0), "");
        assert_eq!(truncate("abcd", 1), ".");
        assert_eq!(truncate("abcd", 2), "..");
        assert_eq!(truncate("abcd", 3), "...");
    }
}
