mod claude;
mod codex;
mod common;
mod db;
mod fuzzy;
mod gemini;
mod opencode;
mod providers;
mod util;

use std::ffi::OsString;
use std::fs;
use std::io::{self, Write};
use std::path::Path;

use clap::{Parser, Subcommand};

use common::{AgentKind, AgentProvider, LaunchCommand, LhResult, RemovalTarget, ThreadSummary};
use fuzzy::MatchResult;
use util::{canonicalize_existing, format_display_time, shorten_path};

#[derive(Parser)]
#[command(name = "lh", about = "Unified LLM agent thread history")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    #[command(alias = "ls")]
    List {
        #[arg(short = 'g', long = "global")]
        global: bool,
        #[arg(long)]
        all: bool,
        #[arg(long, value_name = "N")]
        limit: Option<usize>,
    },
    New {
        agent: Option<String>,
        name: Option<String>,
    },
    Resume {
        agent_or_name: Option<String>,
        name: Option<String>,
    },
    #[command(alias = "rm")]
    Remove {
        agent: Option<String>,
        name: Option<String>,
        #[arg(short, long)]
        force: bool,
        #[arg(short = 'n', long)]
        dry_run: bool,
    },
    Agents {
        #[command(subcommand)]
        command: AgentsCommand,
    },
    Db {
        #[command(subcommand)]
        command: DbCommand,
    },
}

#[derive(Subcommand)]
enum AgentsCommand {
    List,
}

#[derive(Subcommand)]
enum DbCommand {
    Init,
    Refresh,
    Drop,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("lh: {error}");
        std::process::exit(1);
    }
}

fn run() -> LhResult<()> {
    let cli = Cli::parse_from(normalize_args(std::env::args_os()));
    let cwd = canonicalize_existing(&std::env::current_dir()?);

    match cli.command.unwrap_or(Commands::List {
        global: false,
        all: false,
        limit: None,
    }) {
        Commands::List { global, all, limit } => list(&cwd, global, all, limit),
        Commands::New { agent, name } => new_thread(&cwd, agent, name),
        Commands::Resume {
            agent_or_name,
            name,
        } => resume(&cwd, agent_or_name, name),
        Commands::Remove {
            agent,
            name,
            force,
            dry_run,
        } => remove(&cwd, agent, name, force, dry_run),
        Commands::Agents {
            command: AgentsCommand::List,
        } => agents_list(&cwd),
        Commands::Db { command } => match command {
            DbCommand::Init => {
                let path = db::init()?;
                println!("initialized {}", path.display());
                Ok(())
            }
            DbCommand::Refresh => {
                let (path, count) = db::refresh(&cwd)?;
                println!("refreshed {} with {count} thread(s)", path.display());
                Ok(())
            }
            DbCommand::Drop => {
                let path = db::drop_db()?;
                println!("dropped {}", path.display());
                Ok(())
            }
        },
    }
}

fn list(cwd: &Path, global: bool, all: bool, limit: Option<usize>) -> LhResult<()> {
    let mut threads = if global {
        providers::list_global()?
    } else {
        providers::list_all(cwd)?
    };
    let effective_limit = if all {
        None
    } else {
        limit.or(global.then_some(10))
    };
    if let Some(limit) = effective_limit {
        threads.truncate(limit);
    }

    if threads.is_empty() {
        if global {
            println!("No threads found");
        } else {
            println!("No threads found for {}", cwd.display());
        }
        return Ok(());
    }
    print_threads(&threads, global);
    Ok(())
}

fn normalize_args(args: impl IntoIterator<Item = OsString>) -> Vec<OsString> {
    let mut args = args.into_iter().collect::<Vec<_>>();
    if args.len() <= 1 {
        return args;
    }

    let first = args[1].to_string_lossy();
    if matches!(first.as_ref(), "-g" | "--global" | "--all" | "--limit")
        || is_numeric_limit_arg(&args[1])
    {
        args.insert(1, OsString::from("list"));
    }

    let is_list_command = args
        .get(1)
        .and_then(|arg| arg.to_str())
        .is_some_and(|arg| arg == "list" || arg == "ls");
    if !is_list_command {
        return args;
    }

    let mut normalized = Vec::with_capacity(args.len() + 2);
    for arg in args {
        if is_numeric_limit_arg(&arg) {
            let value = arg.to_string_lossy().trim_start_matches('-').to_string();
            normalized.push(OsString::from("--limit"));
            normalized.push(OsString::from(value));
        } else {
            normalized.push(arg);
        }
    }
    normalized
}

fn is_numeric_limit_arg(arg: &OsString) -> bool {
    let Some(arg) = arg.to_str() else {
        return false;
    };
    let Some(rest) = arg.strip_prefix('-') else {
        return false;
    };
    !rest.is_empty() && rest.chars().all(|ch| ch.is_ascii_digit())
}

fn new_thread(cwd: &Path, agent: Option<String>, name: Option<String>) -> LhResult<()> {
    let (agent, name) = parse_new_args(agent, name)?;
    let provider = providers::by_kind(agent);
    let command = provider.new_command(name.as_deref(), cwd)?;
    command.exec()
}

fn resume(cwd: &Path, agent_or_name: Option<String>, name: Option<String>) -> LhResult<()> {
    let (agent, query) = parse_selector(agent_or_name, name)?;
    let (provider, thread) = select_provider_thread(cwd, agent, query.as_deref())?;
    let command = provider.resume_command(thread.as_ref())?;
    command.exec()
}

fn remove(
    cwd: &Path,
    agent_or_name: Option<String>,
    name: Option<String>,
    force: bool,
    dry_run: bool,
) -> LhResult<()> {
    let (agent, query) = parse_selector(agent_or_name, name)?;
    let (_provider, thread) = select_provider_thread(cwd, agent, query.as_deref())?;
    let thread = thread.ok_or("no thread selected")?;
    let target = thread
        .removable
        .clone()
        .ok_or("selected provider does not support removal for this thread")?;
    let description = removal_description(&thread, &target);

    if dry_run {
        println!("would remove {description}");
        return Ok(());
    }

    if !force && !confirm(&format!("Remove {description}?"))? {
        println!("not removed");
        return Ok(());
    }

    execute_removal(target)?;
    println!("removed {description}");
    Ok(())
}

fn agents_list(cwd: &Path) -> LhResult<()> {
    println!(
        "{:<10} {:<7} {:<28} CAVEAT",
        "AGENT", "HISTORY", "EXECUTABLE"
    );
    for provider in providers::all() {
        let status = provider.status(cwd);
        let executable = status
            .executable
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "-".to_string());
        let executable = status
            .version
            .as_ref()
            .map(|version| format!("{executable} ({version})"))
            .unwrap_or(executable);
        println!(
            "{:<10} {:<7} {:<28} {}",
            status.agent.display_name(),
            if status.history_exists { "yes" } else { "no" },
            executable,
            status.caveat.unwrap_or_default()
        );
        println!("           history: {}", status.history_path.display());
    }
    Ok(())
}

fn parse_new_args(
    first: Option<String>,
    second: Option<String>,
) -> LhResult<(AgentKind, Option<String>)> {
    match (first, second) {
        (None, None) => Ok((default_new_agent(), None)),
        (Some(first), None) => {
            if let Some(agent) = AgentKind::parse(&first) {
                Ok((agent, None))
            } else {
                Ok((default_new_agent(), Some(first)))
            }
        }
        (Some(first), Some(second)) => {
            let agent = AgentKind::parse(&first)
                .ok_or_else(|| format!("unknown agent '{first}' in two-argument new command"))?;
            Ok((agent, Some(second)))
        }
        (None, Some(_)) => unreachable!(),
    }
}

fn parse_selector(
    first: Option<String>,
    second: Option<String>,
) -> LhResult<(Option<AgentKind>, Option<String>)> {
    match (first, second) {
        (None, None) => Ok((None, None)),
        (Some(first), None) => {
            if let Some(agent) = AgentKind::parse(&first) {
                Ok((Some(agent), None))
            } else {
                Ok((None, Some(first)))
            }
        }
        (Some(first), Some(second)) => {
            let agent = AgentKind::parse(&first)
                .ok_or_else(|| format!("unknown agent '{first}' in two-argument selector"))?;
            Ok((Some(agent), Some(second)))
        }
        (None, Some(_)) => unreachable!(),
    }
}

fn default_new_agent() -> AgentKind {
    for candidate in [
        AgentKind::Codex,
        AgentKind::Claude,
        AgentKind::OpenCode,
        AgentKind::Gemini,
    ] {
        let provider = providers::by_kind(candidate);
        if provider.executable().is_some() {
            return candidate;
        }
    }
    AgentKind::Codex
}

fn select_provider_thread(
    cwd: &Path,
    agent: Option<AgentKind>,
    query: Option<&str>,
) -> LhResult<(Box<dyn AgentProvider>, Option<ThreadSummary>)> {
    if let Some(agent) = agent {
        let provider = providers::by_kind(agent);
        let threads = provider.list_threads(cwd)?;
        return match fuzzy::select_thread(&threads, query) {
            MatchResult::One(thread) => Ok((provider, Some(thread.clone()))),
            MatchResult::None if query.is_none() => Ok((provider, None)),
            MatchResult::None => Err("no matching thread found".into()),
            MatchResult::Ambiguous(candidates) => Err(ambiguous_error(candidates).into()),
        };
    }

    let threads = providers::list_all(cwd)?;
    match fuzzy::select_thread(&threads, query) {
        MatchResult::One(thread) => Ok((providers::by_kind(thread.agent), Some(thread.clone()))),
        MatchResult::None => Err("no matching thread found".into()),
        MatchResult::Ambiguous(candidates) => Err(ambiguous_error(candidates).into()),
    }
}

fn ambiguous_error(candidates: Vec<&ThreadSummary>) -> String {
    let mut out = String::from("ambiguous match; use a more specific query. Candidates:");
    for thread in candidates.into_iter().take(5) {
        out.push_str(&format!(
            "\n  {} {} {}",
            thread.agent,
            thread.id,
            thread.display_name()
        ));
    }
    out
}

fn print_threads(threads: &[ThreadSummary], show_cwd: bool) {
    const UPDATED_WIDTH: usize = 19;

    if show_cwd {
        println!(
            "{:<UPDATED_WIDTH$} {:<10} {:<36} {:<34} {:<36} SOURCE",
            "UPDATED", "AGENT", "ID", "NAME", "CWD"
        );
    } else {
        println!(
            "{:<UPDATED_WIDTH$} {:<10} {:<36} {:<34} SOURCE",
            "UPDATED", "AGENT", "ID", "NAME"
        );
    }
    for thread in threads {
        let updated = thread
            .updated_at
            .or(thread.created_at)
            .map(format_display_time)
            .unwrap_or_else(|| "-".to_string());
        let source = thread
            .source_path
            .as_ref()
            .map(|path| shorten_path(path))
            .unwrap_or_else(|| "-".to_string());
        if show_cwd {
            println!(
                "{:<UPDATED_WIDTH$} {:<10} {:<36} {:<34} {:<36} {}",
                updated,
                thread.agent.as_str(),
                thread.id,
                common::truncate(&thread.display_name(), 34),
                common::truncate(&shorten_path(&thread.cwd), 36),
                source
            );
        } else {
            println!(
                "{:<UPDATED_WIDTH$} {:<10} {:<36} {:<34} {}",
                updated,
                thread.agent.as_str(),
                thread.id,
                common::truncate(&thread.display_name(), 34),
                source
            );
        }
    }
}

fn removal_description(thread: &ThreadSummary, target: &RemovalTarget) -> String {
    match target {
        RemovalTarget::File(path) => {
            format!("{} {} file {}", thread.agent, thread.id, path.display())
        }
        RemovalTarget::Command(command) => {
            format!("{} {} via `{}`", thread.agent, thread.id, command.display())
        }
        RemovalTarget::OpenCodeDb { db_path, .. } => {
            format!("opencode {} from {}", thread.id, db_path.display())
        }
        RemovalTarget::GeminiFiles { chat_path, .. } => {
            format!("gemini {} file {}", thread.id, chat_path.display())
        }
    }
}

fn execute_removal(target: RemovalTarget) -> LhResult<()> {
    match target {
        RemovalTarget::File(path) => {
            fs::remove_file(path)?;
            Ok(())
        }
        RemovalTarget::Command(command) => command.run(),
        RemovalTarget::OpenCodeDb {
            db_path,
            session_id,
        } => opencode::delete_session_from_db(&db_path, &session_id),
        RemovalTarget::GeminiFiles {
            chat_path,
            logs_path,
            session_id,
        } => gemini::delete_gemini_files(&chat_path, logs_path.as_deref(), &session_id),
    }
}

fn confirm(prompt: &str) -> LhResult<bool> {
    print!("{prompt} [y/N] ");
    io::stdout().flush()?;
    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;
    Ok(matches!(
        answer.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}

#[allow(dead_code)]
fn launch_for_display(program: &str, args: &[&str]) -> LaunchCommand {
    LaunchCommand::new(
        OsString::from(program),
        args.iter().map(|arg| OsString::from(*arg)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(args: &[&str]) -> Vec<OsString> {
        args.iter().map(OsString::from).collect()
    }

    #[test]
    fn normalizes_numeric_list_limit() {
        assert_eq!(
            normalize_args(strings(&["lh", "ls", "-g", "-5"])),
            strings(&["lh", "ls", "-g", "--limit", "5"])
        );
    }

    #[test]
    fn inserts_default_list_for_global_flags() {
        assert_eq!(
            normalize_args(strings(&["lh", "-g", "-10"])),
            strings(&["lh", "list", "-g", "--limit", "10"])
        );
    }

    #[test]
    fn leaves_non_list_numeric_args_alone() {
        assert_eq!(
            normalize_args(strings(&["lh", "resume", "-10"])),
            strings(&["lh", "resume", "-10"])
        );
    }
}
