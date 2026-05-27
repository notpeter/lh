use std::path::Path;

use crate::claude::ClaudeProvider;
use crate::codex::CodexProvider;
use crate::common::{AgentKind, AgentProvider, LhResult, ThreadSummary};
use crate::gemini::GeminiProvider;
use crate::opencode::OpenCodeProvider;

pub fn all() -> Vec<Box<dyn AgentProvider>> {
    vec![
        Box::new(ClaudeProvider::new()),
        Box::new(CodexProvider::new()),
        Box::new(OpenCodeProvider::new()),
        Box::new(GeminiProvider::new()),
    ]
}

pub fn by_kind(kind: AgentKind) -> Box<dyn AgentProvider> {
    match kind {
        AgentKind::Claude => Box::new(ClaudeProvider::new()),
        AgentKind::Codex => Box::new(CodexProvider::new()),
        AgentKind::OpenCode => Box::new(OpenCodeProvider::new()),
        AgentKind::Gemini => Box::new(GeminiProvider::new()),
    }
}

pub fn list_all(cwd: &Path) -> LhResult<Vec<ThreadSummary>> {
    let mut threads = Vec::new();
    for provider in all() {
        match provider.list_threads(cwd) {
            Ok(mut provider_threads) => threads.append(&mut provider_threads),
            Err(error) => eprintln!(
                "warning: failed to read {} history: {error}",
                provider.kind()
            ),
        }
    }
    threads.sort_by_key(|thread| std::cmp::Reverse(thread.updated_sort_key()));
    Ok(threads)
}

pub fn list_global() -> LhResult<Vec<ThreadSummary>> {
    let mut threads = Vec::new();
    for provider in all() {
        match provider.list_threads_global() {
            Ok(mut provider_threads) => threads.append(&mut provider_threads),
            Err(error) => eprintln!(
                "warning: failed to read {} history: {error}",
                provider.kind()
            ),
        }
    }
    threads.sort_by_key(|thread| std::cmp::Reverse(thread.updated_sort_key()));
    Ok(threads)
}
