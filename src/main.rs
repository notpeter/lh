mod claude;
mod codex;
mod common;
mod config;
mod db;
mod fuzzy;
mod gemini;
mod llm;
mod opencode;
mod providers;
mod util;

use std::ffi::OsString;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use clap::{Parser, Subcommand};

use common::{AgentKind, AgentProvider, LaunchCommand, LhResult, RemovalTarget, ThreadSummary};
use fuzzy::MatchResult;
use util::{canonicalize_existing, format_display_time, format_time, shorten_path, terminal_width};

#[derive(Parser)]
#[command(name = "lh", about = "Unified LLM agent thread history")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    #[command(about = "List agent threads for the current directory")]
    #[command(alias = "ls")]
    List {
        #[arg(short = 'g', long = "global", help = "Scan all known agent history")]
        global: bool,
        #[arg(long, help = "Show all matching threads without the default limit")]
        all: bool,
        #[arg(long, value_name = "N", help = "Limit the number of rows shown")]
        limit: Option<usize>,
    },
    #[command(about = "Start a new agent thread")]
    New {
        #[arg(help = "Agent to launch, or the new thread name when used alone")]
        agent: Option<String>,
        #[arg(help = "Name for the new thread when AGENT is provided")]
        name: Option<String>,
    },
    #[command(about = "Resume an existing agent thread")]
    Resume {
        #[arg(help = "Thread name/id to resume, or agent when followed by NAME-OR-ID")]
        agent_or_name: Option<String>,
        #[arg(help = "Thread name/id when AGENT-OR-NAME is an agent")]
        name: Option<String>,
    },
    #[command(about = "Rename a native agent thread")]
    Rename {
        #[arg(short = 'g', long = "global", help = "Search all known agent history")]
        global: bool,
        #[arg(help = "Thread id to rename")]
        thread_id: String,
        #[arg(help = "New title for the selected thread")]
        new_name: Option<String>,
        #[arg(long, help = "Generate a title from the thread transcript")]
        auto: bool,
        #[arg(
            short = 'n',
            long,
            help = "Print the proposed rename without changing history"
        )]
        dry_run: bool,
    },
    #[command(about = "Show full details for a selected thread")]
    Info {
        #[arg(short = 'g', long = "global", help = "Search all known agent history")]
        global: bool,
        #[arg(help = "Thread name/id to inspect, or agent when followed by NAME-OR-ID")]
        agent_or_name: Option<String>,
        #[arg(help = "Thread name/id when AGENT-OR-NAME is an agent")]
        name: Option<String>,
    },
    #[command(about = "Link the current directory with another checkout")]
    #[command(alias = "ln")]
    Alias {
        #[arg(short = 's', help = "Accepted for ln compatibility")]
        symbolic: bool,
        #[arg(help = "Alias source, or target when used alone")]
        source_or_target: Option<PathBuf>,
        #[arg(help = "Alias target when SOURCE-OR-TARGET is provided")]
        target: Option<PathBuf>,
    },
    #[command(about = "Remove directory aliases")]
    Unalias {
        #[arg(help = "Directory whose aliases should be removed")]
        dir: Option<PathBuf>,
    },
    #[command(about = "Open a shell in a matching aliased directory")]
    Cd {
        #[arg(help = "Alias query to match")]
        dir: Option<String>,
    },
    #[command(about = "Remove a selected thread from native history")]
    #[command(alias = "rm", arg_required_else_help = true)]
    Remove {
        #[arg(value_name = "TARGET", help = "Thread name/id to remove")]
        target: String,
        #[arg(
            value_name = "NAME-OR-ID",
            help = "Thread name/id when TARGET is an agent"
        )]
        name: Option<String>,
        #[arg(short, long, help = "Remove without prompting for confirmation")]
        force: bool,
        #[arg(
            short = 'n',
            long,
            help = "Print what would be removed without deleting"
        )]
        dry_run: bool,
    },
    #[command(about = "Inspect configured agent providers")]
    Agents {
        #[command(subcommand)]
        command: AgentsCommand,
    },
    #[command(about = "Manage the optional SQLite cache")]
    Db {
        #[command(subcommand)]
        command: DbCommand,
    },
}

#[derive(Subcommand)]
enum AgentsCommand {
    #[command(about = "Show provider status")]
    List,
}

#[derive(Subcommand)]
enum DbCommand {
    #[command(about = "Initialize the optional SQLite cache")]
    Init,
    #[command(about = "Refresh the optional SQLite cache")]
    Refresh,
    #[command(about = "Drop the optional SQLite cache")]
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
        Commands::Rename {
            global,
            thread_id,
            new_name,
            auto,
            dry_run,
        } => rename(&cwd, global, thread_id, new_name, auto, dry_run),
        Commands::Info {
            global,
            agent_or_name,
            name,
        } => info(&cwd, global, agent_or_name, name),
        Commands::Alias {
            symbolic,
            source_or_target,
            target,
        } => alias(&cwd, symbolic, source_or_target, target),
        Commands::Unalias { dir } => unalias(&cwd, dir),
        Commands::Cd { dir } => cd(&cwd, dir),
        Commands::Remove {
            target,
            name,
            force,
            dry_run,
        } => remove(&cwd, target, name, force, dry_run),
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
        let dirs = config::alias_group(cwd)?;
        providers::list_all_for_dirs(&dirs)?
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
    print_threads(&threads);
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
    let (provider, thread) = select_provider_thread(cwd, false, agent, query.as_deref())?;
    if let Some(thread) = &thread {
        ensure_resumable_from_cwd(cwd, thread)?;
    }
    let command = provider.resume_command(thread.as_ref())?;
    command.exec()
}

fn rename(
    cwd: &Path,
    global: bool,
    thread_id: String,
    new_name: Option<String>,
    auto: bool,
    dry_run: bool,
) -> LhResult<()> {
    if auto == new_name.is_some() {
        return Err("provide exactly one of [newname] or --auto".into());
    }

    let (provider, thread) = select_provider_thread(cwd, global, None, Some(&thread_id))?;
    let thread = thread.ok_or("no thread selected")?;
    if !provider.supports_rename() {
        return Err(format!("{} does not support native rename", thread.agent).into());
    }

    let name = if auto {
        let config = config::load()?;
        let content = provider.thread_content(&thread)?;
        llm::generate_thread_name(&config, &content)?
    } else {
        let name = new_name.unwrap();
        validate_thread_name(&name)?
    };

    if dry_run {
        println!("would rename {} {} to {}", thread.agent, thread.id, name);
        return Ok(());
    }

    provider.rename_thread(&thread, &name)?;
    println!("renamed {} {} to {}", thread.agent, thread.id, name);
    Ok(())
}

fn info(
    cwd: &Path,
    global: bool,
    agent_or_name: Option<String>,
    name: Option<String>,
) -> LhResult<()> {
    let (agent, query) = parse_selector(agent_or_name, name)?;
    let (provider, thread) = select_provider_thread(cwd, global, agent, query.as_deref())?;
    let thread = thread.ok_or("no thread selected")?;
    print_thread_info(&*provider, &thread);
    Ok(())
}

fn alias(
    cwd: &Path,
    _symbolic: bool,
    source_or_target: Option<PathBuf>,
    target: Option<PathBuf>,
) -> LhResult<()> {
    let Some(source_or_target) = source_or_target else {
        return print_aliases(cwd);
    };

    let (source, target) = match target {
        Some(target) if source_or_target == Path::new(".") => (cwd.to_path_buf(), target),
        Some(target) => (source_or_target, target),
        None => (cwd.to_path_buf(), source_or_target),
    };

    let (source, target, path) = config::add_alias(cwd, &source, &target)?;
    println!("aliased {source} -> {target}");
    println!("config {}", path.display());
    Ok(())
}

fn print_aliases(cwd: &Path) -> LhResult<()> {
    let aliases = config::aliases_for_dir(cwd)?;
    if aliases.is_empty() {
        println!("No aliases configured for current directory");
        println!("config {}", config::config_path().display());
        return Ok(());
    }

    for (source, target) in aliases {
        println!("{source} -> {target}");
    }
    Ok(())
}

fn unalias(cwd: &Path, dir: Option<PathBuf>) -> LhResult<()> {
    let dir = dir.unwrap_or_else(|| PathBuf::from("."));
    let dir = if dir == Path::new(".") {
        cwd.to_path_buf()
    } else {
        dir
    };
    let (removed, path) = config::remove_alias(cwd, &dir)?;

    if removed.is_empty() {
        println!("No aliases removed");
    } else {
        for (source, target) in removed {
            println!("removed alias {source} -> {target}");
        }
    }
    println!("config {}", path.display());
    Ok(())
}

fn cd(cwd: &Path, query: Option<String>) -> LhResult<()> {
    let target = select_alias_dir(cwd, query.as_deref())?;
    exec_shell_in_dir(&target)
}

fn remove(
    cwd: &Path,
    target: String,
    name: Option<String>,
    force: bool,
    dry_run: bool,
) -> LhResult<()> {
    let (agent, query) = parse_remove_selector(target, name)?;
    let (_provider, thread) = select_provider_thread(cwd, false, agent, Some(&query))?;
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

fn parse_remove_selector(
    target: String,
    name: Option<String>,
) -> LhResult<(Option<AgentKind>, String)> {
    match name {
        Some(name) => {
            let agent = AgentKind::parse(&target).ok_or_else(|| {
                format!("unknown agent '{target}' in two-argument remove command")
            })?;
            Ok((Some(agent), name))
        }
        None => {
            if AgentKind::parse(&target).is_some() {
                Err(
                    format!("remove requires a thread name or id; '{target}' is an agent name")
                        .into(),
                )
            } else {
                Ok((None, target))
            }
        }
    }
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

fn validate_thread_name(name: &str) -> LhResult<String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("thread name cannot be empty".into());
    }
    if name.contains('\n') || name.contains('\r') {
        return Err("thread name must be a single line".into());
    }
    Ok(name.to_string())
}

fn select_provider_thread(
    cwd: &Path,
    global: bool,
    agent: Option<AgentKind>,
    query: Option<&str>,
) -> LhResult<(Box<dyn AgentProvider>, Option<ThreadSummary>)> {
    if let Some(agent) = agent {
        let provider = providers::by_kind(agent);
        let threads = if global {
            provider.list_threads_global()?
        } else {
            let dirs = config::alias_group(cwd)?;
            providers::list_provider_for_dirs(&*provider, &dirs)?
        };
        return match fuzzy::select_thread(&threads, query) {
            MatchResult::One(thread) => Ok((provider, Some(thread.clone()))),
            MatchResult::None if query.is_none() => Ok((provider, None)),
            MatchResult::None => Err("no matching thread found".into()),
            MatchResult::Ambiguous(candidates) => Err(ambiguous_error(candidates).into()),
        };
    }

    let threads = if global {
        providers::list_global()?
    } else {
        let dirs = config::alias_group(cwd)?;
        providers::list_all_for_dirs(&dirs)?
    };
    match fuzzy::select_thread(&threads, query) {
        MatchResult::One(thread) => Ok((providers::by_kind(thread.agent), Some(thread.clone()))),
        MatchResult::None => Err("no matching thread found".into()),
        MatchResult::Ambiguous(candidates) => Err(ambiguous_error(candidates).into()),
    }
}

fn ambiguous_error(candidates: Vec<&ThreadSummary>) -> String {
    let candidates = candidates.into_iter().take(5).collect::<Vec<_>>();
    let agent_width = candidates
        .iter()
        .map(|thread| thread.agent.as_str().len())
        .chain([5])
        .max()
        .unwrap_or(5);
    let id_width = candidates
        .iter()
        .map(|thread| thread.id.len())
        .chain([2])
        .max()
        .unwrap_or(2);

    let mut out = String::from("ambiguous match; use a more specific query:");
    for thread in candidates {
        out.push_str(&format!(
            "\n  {agent:<agent_width$}  {id:<id_width$}  {name}",
            agent = thread.agent.as_str(),
            id = thread.id,
            name = thread.display_name(),
            agent_width = agent_width,
            id_width = id_width
        ));
    }
    out
}

fn print_threads(threads: &[ThreadSummary]) {
    const UPDATED_WIDTH: usize = 19;

    let widths = list_column_widths(threads, terminal_width());
    println!(
        "{:<UPDATED_WIDTH$} {:<agent_width$} {:<id_width$} {:<dir_width$} NAME",
        "UPDATED",
        "AGENT",
        "ID",
        "DIR",
        agent_width = widths.agent,
        id_width = widths.id,
        dir_width = widths.dir,
    );
    for thread in threads {
        let updated = thread
            .updated_at
            .or(thread.created_at)
            .map(format_display_time)
            .unwrap_or_else(|| "-".to_string());
        println!(
            "{:<UPDATED_WIDTH$} {:<agent_width$} {:<id_width$} {:<dir_width$} {}",
            updated,
            thread.agent.as_str(),
            common::truncate(&thread.id, widths.id),
            common::truncate(&shorten_path(&thread.cwd), widths.dir),
            common::truncate(&thread_list_name(thread), widths.name),
            agent_width = widths.agent,
            id_width = widths.id,
            dir_width = widths.dir,
        );
    }
}

fn thread_list_name(thread: &ThreadSummary) -> String {
    thread
        .name
        .clone()
        .or_else(|| thread.preview.clone())
        .unwrap_or_else(|| thread.id.clone())
}

#[derive(Debug, PartialEq, Eq)]
struct ListColumnWidths {
    agent: usize,
    id: usize,
    dir: usize,
    name: usize,
}

fn list_column_widths(threads: &[ThreadSummary], terminal_width: usize) -> ListColumnWidths {
    const AGENT_MAX_WIDTH: usize = 10;
    const ID_MAX_WIDTH: usize = 36;
    const DIR_MAX_WIDTH: usize = 30;

    let agent = bounded_column_width(
        "AGENT",
        threads
            .iter()
            .map(|thread| thread.agent.as_str().to_string()),
        AGENT_MAX_WIDTH,
    );
    let id = bounded_column_width(
        "ID",
        threads.iter().map(|thread| thread.id.clone()),
        ID_MAX_WIDTH,
    );
    let dir = bounded_column_width(
        "DIR",
        threads.iter().map(|thread| shorten_path(&thread.cwd)),
        DIR_MAX_WIDTH,
    );
    let name = list_name_width_for_columns(terminal_width, agent, id, dir);

    ListColumnWidths {
        agent,
        id,
        dir,
        name,
    }
}

fn bounded_column_width(
    header: &str,
    values: impl IntoIterator<Item = String>,
    max_width: usize,
) -> usize {
    values
        .into_iter()
        .map(|value| value.chars().count())
        .chain(std::iter::once(header.chars().count()))
        .max()
        .unwrap_or(1)
        .min(max_width)
}

fn list_name_width_for_columns(
    terminal_width: usize,
    agent_width: usize,
    id_width: usize,
    dir_width: usize,
) -> usize {
    const UPDATED_WIDTH: usize = 19;

    let fixed_width = UPDATED_WIDTH + agent_width + id_width + dir_width;
    let separator_count = 4;
    terminal_width
        .saturating_sub(1)
        .saturating_sub(fixed_width)
        .saturating_sub(separator_count)
        .max(1)
}

fn print_thread_info(provider: &dyn AgentProvider, thread: &ThreadSummary) {
    let field = |name: &str, value: String| {
        println!("{name:<14} {value}");
    };

    field("Agent", thread.agent.display_name().to_string());
    field("ID", thread.id.clone());
    field(
        "Name",
        thread.name.clone().unwrap_or_else(|| "<unset>".to_string()),
    );
    field("CWD", thread.cwd.display().to_string());
    field(
        "Created",
        thread
            .created_at
            .map(format_time)
            .unwrap_or_else(|| "-".to_string()),
    );
    field(
        "Updated",
        thread
            .updated_at
            .map(format_time)
            .unwrap_or_else(|| "-".to_string()),
    );
    field(
        "Source",
        thread
            .source_path
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "-".to_string()),
    );
    if let Some(preview) = &thread.preview {
        field("Preview", common::truncate(preview, 500));
    }
    if let Some(target) = &thread.removable {
        field("Removable", removal_description(thread, target));
    }
    if let Ok(command) = provider.resume_command(Some(thread)) {
        field("Resume", command.display());
    }
}

fn select_alias_dir(cwd: &Path, query: Option<&str>) -> LhResult<PathBuf> {
    let current = canonicalize_existing(cwd);
    let mut candidates = config::alias_group(cwd)?;
    if candidates.len() <= 1 {
        candidates = config::all_alias_dirs()?;
    }
    candidates.retain(|dir| *dir != current);

    let Some(query) = query.map(str::trim).filter(|query| !query.is_empty()) else {
        return match candidates.as_slice() {
            [target] => Ok(target.clone()),
            [] => Err("no aliased directories found".into()),
            _ => Err(alias_cd_ambiguous_error(candidates).into()),
        };
    };

    let query = query.to_ascii_lowercase();
    let matches = candidates
        .into_iter()
        .filter(|dir| {
            let display = config::compact_home_path(dir).to_ascii_lowercase();
            let name = dir
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default()
                .to_ascii_lowercase();
            name == query || name.contains(&query) || display.contains(&query)
        })
        .collect::<Vec<_>>();

    match matches.as_slice() {
        [target] => Ok(target.clone()),
        [] => Err("no matching aliased directory found".into()),
        _ => Err(alias_cd_ambiguous_error(matches).into()),
    }
}

fn alias_cd_ambiguous_error(candidates: Vec<PathBuf>) -> String {
    let mut out = String::from("ambiguous aliased directory; use a more specific query:");
    for candidate in candidates.into_iter().take(8) {
        out.push_str(&format!("\n  {}", config::compact_home_path(&candidate)));
    }
    out
}

fn exec_shell_in_dir(dir: &Path) -> LhResult<()> {
    let shell = std::env::var_os("SHELL").unwrap_or_else(|| OsString::from("/bin/sh"));

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let err = Command::new(&shell).current_dir(dir).exec();
        Err(Box::new(err))
    }

    #[cfg(not(unix))]
    {
        let status = Command::new(&shell).current_dir(dir).status()?;
        if status.success() {
            Ok(())
        } else {
            Err(format!("shell exited with {status}").into())
        }
    }
}

fn ensure_resumable_from_cwd(cwd: &Path, thread: &ThreadSummary) -> LhResult<()> {
    let current = canonicalize_existing(cwd);
    let thread_cwd = canonicalize_existing(&thread.cwd);
    if current == thread_cwd {
        return Ok(());
    }

    let alias_group = config::alias_group(cwd)?;
    if alias_group.contains(&thread_cwd) {
        return Err(alternate_directory_resume_message(thread, &thread_cwd).into());
    }

    Ok(())
}

fn alternate_directory_resume_message(thread: &ThreadSummary, thread_cwd: &Path) -> String {
    format!(
        "That session was created under an alternate directory. To resume run:\n    cd {} && lh resume {}",
        shell_path(thread_cwd),
        shell_arg(&thread.id),
    )
}

fn shell_path(path: &Path) -> String {
    let compact = config::compact_home_path(path);
    if is_shell_safe(&compact) {
        compact
    } else {
        shell_arg(&path.display().to_string())
    }
}

fn shell_arg(value: &str) -> String {
    if is_shell_safe(value) {
        return value.to_string();
    }
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn is_shell_safe(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '/' | '.' | '_' | '-' | '~'))
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

    #[test]
    fn bare_remove_shows_help() {
        let result = Cli::try_parse_from(strings(&["lh", "rm"]));

        assert!(matches!(
            result,
            Err(error)
                if error.kind() == clap::error::ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
                    && error.to_string().contains("Usage: lh remove")
        ));
    }

    #[test]
    fn remove_rejects_lone_agent_selector() {
        let error = parse_remove_selector("codex".to_string(), None).unwrap_err();

        assert_eq!(
            error.to_string(),
            "remove requires a thread name or id; 'codex' is an agent name"
        );
    }

    #[test]
    fn remove_accepts_agent_when_name_is_present() {
        assert_eq!(
            parse_remove_selector("codex".to_string(), Some("abc123".to_string())).unwrap(),
            (Some(AgentKind::Codex), "abc123".to_string())
        );
    }

    #[test]
    fn ambiguous_error_aligns_candidate_columns() {
        let opencode = ThreadSummary {
            agent: AgentKind::OpenCode,
            id: "ses_196465859ffeQn1sT67NGi5Pof".to_string(),
            name: Some("adding-mit-license-to-cargo-toml".to_string()),
            cwd: PathBuf::from("/tmp"),
            created_at: None,
            updated_at: None,
            source_path: None,
            preview: None,
            removable: None,
            resume_hint: None,
        };
        let codex = ThreadSummary {
            agent: AgentKind::Codex,
            id: "019e69d6-1fae-7052-b26c-71824873dae7".to_string(),
            name: Some("create-llm-history-lh-cli".to_string()),
            cwd: PathBuf::from("/tmp"),
            created_at: None,
            updated_at: None,
            source_path: None,
            preview: None,
            removable: None,
            resume_hint: None,
        };

        assert_eq!(
            ambiguous_error(vec![&opencode, &codex]),
            "ambiguous match; use a more specific query:\n  opencode  ses_196465859ffeQn1sT67NGi5Pof        adding-mit-license-to-cargo-toml\n  codex     019e69d6-1fae-7052-b26c-71824873dae7  create-llm-history-lh-cli"
        );
    }

    #[test]
    fn list_widths_use_actual_non_name_widths() {
        let thread = ThreadSummary {
            agent: AgentKind::Codex,
            id: "abc123".to_string(),
            name: Some("short thread".to_string()),
            cwd: PathBuf::from("/tmp"),
            created_at: None,
            updated_at: None,
            source_path: None,
            preview: None,
            removable: None,
            resume_hint: None,
        };

        assert_eq!(
            list_column_widths(&[thread], 120),
            ListColumnWidths {
                agent: 5,
                id: 6,
                dir: 4,
                name: 81,
            }
        );
    }

    #[test]
    fn list_widths_cap_long_non_name_columns() {
        let thread = ThreadSummary {
            agent: AgentKind::OpenCode,
            id: "x".repeat(80),
            name: None,
            cwd: PathBuf::from("/a/very/long/path/that/should/not/consume/the/name/column"),
            created_at: None,
            updated_at: None,
            source_path: None,
            preview: Some("preview".to_string()),
            removable: None,
            resume_hint: None,
        };

        assert_eq!(
            list_column_widths(&[thread], 120),
            ListColumnWidths {
                agent: 8,
                id: 36,
                dir: 30,
                name: 22,
            }
        );
    }

    #[test]
    fn small_terminal_preserves_some_name_width() {
        assert_eq!(list_name_width_for_columns(80, 10, 36, 30), 1);
    }

    #[test]
    fn list_name_uses_full_preview_before_width_truncation() {
        let preview = "x".repeat(100);
        let thread = ThreadSummary {
            agent: AgentKind::Codex,
            id: "abc123".to_string(),
            name: None,
            cwd: PathBuf::from("/tmp"),
            created_at: None,
            updated_at: None,
            source_path: None,
            preview: Some(preview.clone()),
            removable: None,
            resume_hint: None,
        };

        assert_eq!(thread_list_name(&thread), preview);
    }

    #[test]
    fn alternate_directory_resume_message_points_at_owner_dir() {
        let thread = ThreadSummary {
            agent: AgentKind::Codex,
            id: "abc123".to_string(),
            name: None,
            cwd: PathBuf::from("/tmp/other clone"),
            created_at: None,
            updated_at: None,
            source_path: None,
            preview: None,
            removable: None,
            resume_hint: None,
        };

        assert_eq!(
            alternate_directory_resume_message(&thread, Path::new("/tmp/other clone")),
            "That session was created under an alternate directory. To resume run:\n    cd '/tmp/other clone' && lh resume abc123"
        );
    }
}
