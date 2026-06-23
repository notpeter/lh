use std::collections::HashSet;
use std::path::PathBuf;

use crate::antigravity::AntiGravityProvider;
use crate::claude::ClaudeProvider;
use crate::codex::CodexProvider;
use crate::common::{AgentKind, AgentProvider, LhResult, MemoryFile, ThreadSummary};
use crate::opencode::OpenCodeProvider;
use crate::pi::PiProvider;
use crate::zed::ZedProvider;

pub fn all() -> Vec<Box<dyn AgentProvider>> {
    vec![
        Box::new(AntiGravityProvider::new()),
        Box::new(ClaudeProvider::new()),
        Box::new(CodexProvider::new()),
        Box::new(OpenCodeProvider::new()),
        Box::new(ZedProvider::new()),
        Box::new(PiProvider::new()),
    ]
}

pub fn by_kind(kind: AgentKind) -> Box<dyn AgentProvider> {
    match kind {
        AgentKind::AntiGravity => Box::new(AntiGravityProvider::new()),
        AgentKind::Claude => Box::new(ClaudeProvider::new()),
        AgentKind::Codex => Box::new(CodexProvider::new()),
        AgentKind::OpenCode => Box::new(OpenCodeProvider::new()),
        AgentKind::Zed => Box::new(ZedProvider::new()),
        AgentKind::Pi => Box::new(PiProvider::new()),
    }
}

pub fn list_all_for_dirs(cwds: &[PathBuf]) -> LhResult<Vec<ThreadSummary>> {
    let mut threads = Vec::new();
    for provider in all() {
        match list_provider_for_dirs(&*provider, cwds) {
            Ok(mut provider_threads) => threads.append(&mut provider_threads),
            Err(error) => eprintln!(
                "warning: failed to read {} history: {error}",
                provider.kind()
            ),
        }
    }
    sort_dedup(&mut threads);
    Ok(threads)
}

pub fn list_provider_for_dirs(
    provider: &dyn AgentProvider,
    cwds: &[PathBuf],
) -> LhResult<Vec<ThreadSummary>> {
    let mut threads = Vec::new();
    for cwd in cwds {
        match provider.list_threads(cwd) {
            Ok(mut provider_threads) => threads.append(&mut provider_threads),
            Err(error) => eprintln!(
                "warning: failed to read {} history for {}: {error}",
                provider.kind(),
                cwd.display()
            ),
        }
    }
    sort_dedup(&mut threads);
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
    sort_dedup(&mut threads);
    Ok(threads)
}

pub fn list_memory_all_for_dirs(cwds: &[PathBuf]) -> LhResult<Vec<MemoryFile>> {
    let mut memories = Vec::new();
    for provider in all() {
        match list_memory_provider_for_dirs(&*provider, cwds) {
            Ok(mut provider_memories) => memories.append(&mut provider_memories),
            Err(error) => eprintln!(
                "warning: failed to read {} memory: {error}",
                provider.kind()
            ),
        }
    }
    sort_dedup_memory(&mut memories);
    Ok(memories)
}

pub fn list_memory_provider_for_dirs(
    provider: &dyn AgentProvider,
    cwds: &[PathBuf],
) -> LhResult<Vec<MemoryFile>> {
    let mut memories = Vec::new();
    for cwd in cwds {
        match provider.list_memory(cwd) {
            Ok(mut provider_memories) => memories.append(&mut provider_memories),
            Err(error) => eprintln!(
                "warning: failed to read {} memory for {}: {error}",
                provider.kind(),
                cwd.display()
            ),
        }
    }
    sort_dedup_memory(&mut memories);
    Ok(memories)
}

pub fn list_memory_global() -> LhResult<Vec<MemoryFile>> {
    let mut memories = Vec::new();
    for provider in all() {
        match provider.list_memory_global() {
            Ok(mut provider_memories) => memories.append(&mut provider_memories),
            Err(error) => eprintln!(
                "warning: failed to read {} memory: {error}",
                provider.kind()
            ),
        }
    }
    sort_dedup_memory(&mut memories);
    Ok(memories)
}

fn sort_dedup(threads: &mut Vec<ThreadSummary>) {
    threads.sort_by_key(|thread| std::cmp::Reverse(thread.updated_sort_key()));
    let mut seen = HashSet::new();
    threads.retain(|thread| seen.insert((thread.agent, thread.id.clone())));
}

fn sort_dedup_memory(memories: &mut Vec<MemoryFile>) {
    memories.sort_by_key(|memory| std::cmp::Reverse(memory.updated_sort_key()));
    let mut seen = HashSet::new();
    memories.retain(|memory| seen.insert((memory.agent, memory.path.clone())));
}
