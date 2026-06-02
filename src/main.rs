mod claude;
mod codex;
mod common;
mod config;
mod fuzzy;
mod gemini;
mod llm;
mod opencode;
mod prices;
mod providers;
mod util;

use std::ffi::OsString;
use std::fs;
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use clap::{Parser, Subcommand};

use common::{
    AgentKind, AgentProvider, LaunchCommand, LhResult, MemoryFile, RemovalTarget, ThreadSummary,
};
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
        #[arg(long, value_name = "N", help = "Limit the number of rows shown")]
        limit: Option<usize>,
        #[arg(
            short = 'o',
            long = "output",
            value_name = "FIELDS",
            value_delimiter = ',',
            num_args = 1..,
            help = "Columns to show, comma-separated or repeated"
        )]
        output: Vec<String>,
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
        #[arg(
            long,
            help = "Generate a title from the thread transcript (default when NEW_NAME is omitted)"
        )]
        auto: bool,
        #[arg(long, value_name = "PROVIDER", help = "Override [llm].provider")]
        provider: Option<String>,
        #[arg(long, value_name = "MODEL", help = "Override [llm].model")]
        model: Option<String>,
        #[arg(long, value_name = "PROMPT", help = "Override [llm].prompt")]
        prompt: Option<String>,
        #[arg(long, help = "Clear the selected thread's native title")]
        unset: bool,
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
    #[command(about = "List or show agent memory files", alias = "mem")]
    Memory {
        #[arg(short = 'g', long = "global", help = "Scan all known memory files")]
        global: bool,
        #[arg(help = "Memory file to show, or agent when followed by NAME-OR-ID")]
        agent_or_name: Option<String>,
        #[arg(help = "Memory file to show when AGENT-OR-NAME is an agent")]
        name: Option<String>,
    },
    #[command(about = "Link the current directory with another checkout")]
    #[command(alias = "ln")]
    Alias {
        #[arg(short = 's', help = "Accepted for ln compatibility")]
        symbolic: bool,
        #[arg(short = 'd', long = "delete", help = "Remove directory aliases")]
        delete: bool,
        #[arg(help = "Alias source, or target when used alone")]
        source_or_target: Option<PathBuf>,
        #[arg(help = "Alias target when SOURCE-OR-TARGET is provided")]
        target: Option<PathBuf>,
    },
    #[command(about = "Remove directory aliases", hide = true)]
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
        #[arg(short = 'g', long = "global", help = "Search all known agent history")]
        global: bool,
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
    #[command(about = "Inspect configured agent providers", alias = "agents")]
    Agent {
        #[command(subcommand)]
        command: AgentsCommand,
    },
}

#[derive(Subcommand)]
enum AgentsCommand {
    #[command(about = "Show provider status", alias = "ls")]
    List,
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
        limit: None,
        output: Vec::new(),
    }) {
        Commands::List {
            global,
            limit,
            output,
        } => list(&cwd, global, limit, output),
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
            provider,
            model,
            prompt,
            unset,
            dry_run,
        } => rename(
            &cwd,
            RenameRequest {
                global,
                thread_id,
                new_name,
                auto,
                llm_provider: provider,
                llm_model: model,
                llm_prompt: prompt,
                unset,
                dry_run,
            },
        ),
        Commands::Info {
            global,
            agent_or_name,
            name,
        } => info(&cwd, global, agent_or_name, name),
        Commands::Memory {
            global,
            agent_or_name,
            name,
        } => memory(&cwd, global, agent_or_name, name),
        Commands::Alias {
            symbolic,
            delete,
            source_or_target,
            target,
        } => alias(&cwd, symbolic, delete, source_or_target, target),
        Commands::Unalias { dir } => unalias(&cwd, dir),
        Commands::Cd { dir } => cd(&cwd, dir),
        Commands::Remove {
            global,
            target,
            name,
            force,
            dry_run,
        } => remove(&cwd, global, target, name, force, dry_run),
        Commands::Agent {
            command: AgentsCommand::List,
        } => agents_list(&cwd),
    }
}

fn list(cwd: &Path, global: bool, limit: Option<usize>, output: Vec<String>) -> LhResult<()> {
    let columns = parse_list_columns(&output)?;
    let mut threads = if global {
        providers::list_global()?
    } else {
        let dirs = config::alias_group(cwd)?;
        providers::list_all_for_dirs(&dirs)?
    };
    if let Some(limit) = limit {
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
    page_or_print(&format_threads(&threads, &columns))?;
    Ok(())
}

fn normalize_args(args: impl IntoIterator<Item = OsString>) -> Vec<OsString> {
    let mut args = args.into_iter().collect::<Vec<_>>();
    if args.len() <= 1 {
        return args;
    }

    if is_global_arg(&args[1])
        && args
            .get(2)
            .and_then(|arg| arg.to_str())
            .is_some_and(global_flag_subcommand)
    {
        let global = args.remove(1);
        args.insert(2, global);
    }

    if is_list_shortcut_arg(&args[1]) || is_numeric_limit_arg(&args[1]) {
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

fn is_list_shortcut_arg(arg: &OsString) -> bool {
    let value = arg.to_string_lossy();
    is_global_arg(arg) || matches!(value.as_ref(), "--limit" | "-o" | "--output")
}

fn is_global_arg(arg: &OsString) -> bool {
    let value = arg.to_string_lossy();
    matches!(value.as_ref(), "-g" | "--global")
}

fn global_flag_subcommand(arg: &str) -> bool {
    matches!(
        arg,
        "list" | "ls" | "rename" | "info" | "memory" | "mem" | "remove" | "rm"
    )
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

struct RenameRequest {
    global: bool,
    new_name: Option<String>,
    thread_id: String,
    auto: bool,
    llm_provider: Option<String>,
    llm_model: Option<String>,
    llm_prompt: Option<String>,
    unset: bool,
    dry_run: bool,
}

fn rename(cwd: &Path, request: RenameRequest) -> LhResult<()> {
    let mode = parse_rename_mode(request.new_name, request.auto, request.unset)?;

    let (provider, thread) =
        select_provider_thread(cwd, request.global, None, Some(&request.thread_id))?;
    let thread = thread.ok_or("no thread selected")?;
    if !provider.supports_rename() {
        return Err(format!("{} does not support native rename", thread.agent).into());
    }

    let mut pricing = None;
    let name = match mode {
        RenameMode::Unset => {
            if request.dry_run {
                println!("would unset name for {} {}", thread.agent, thread.id);
                return Ok(());
            }

            provider.unset_thread_name(&thread)?;
            println!("unset name for {} {}", thread.agent, thread.id);
            return Ok(());
        }
        RenameMode::Manual(name) => name,
        RenameMode::Auto => {
            let config = rename_llm_config(
                config::load()?,
                request.llm_provider,
                request.llm_model,
                request.llm_prompt,
            )?;
            if config.llm.is_none() {
                return Err(
                    "provide a new name to rename this thread (or for auto rename configure an llm provider, or pass both --provider and --prompt)"
                        .into(),
                );
            }
            let content = provider.thread_content(&thread)?;
            let generated = llm::generate_thread_name_for_rename(&config, &thread, &content)?;
            pricing = generated.pricing;
            generated.name
        }
    };

    if let Some(pricing) = &pricing {
        print_rename_pricing(pricing);
    }

    if request.dry_run {
        println!("would rename {} {} to {}", thread.agent, thread.id, name);
        return Ok(());
    }

    provider.rename_thread(&thread, &name)?;
    println!("renamed {} {} to {}", thread.agent, thread.id, name);
    Ok(())
}

fn rename_llm_config(
    mut config: config::Config,
    provider: Option<String>,
    model: Option<String>,
    prompt: Option<String>,
) -> LhResult<config::Config> {
    if provider.is_none() && model.is_none() && prompt.is_none() {
        return Ok(config);
    }

    config.llm = Some(match config.llm.take() {
        Some(mut llm) => {
            if let Some(provider) = provider {
                llm.provider = provider;
            }
            if let Some(model) = model {
                llm.model = Some(model);
            }
            if let Some(prompt) = prompt {
                llm.prompt = prompt;
            }
            llm
        }
        None => config::LlmConfig {
            provider: provider
                .ok_or("auto rename with no [llm] config requires both --provider and --prompt")?,
            prompt: prompt
                .ok_or("auto rename with no [llm] config requires both --provider and --prompt")?,
            model,
        },
    });

    Ok(config)
}

fn print_rename_pricing(pricing: &prices::RequestCost) {
    let token_text = match pricing.total_tokens {
        Some(total) => format!(
            "{} input, {} output, {} total tokens",
            pricing.input_tokens, pricing.output_tokens, total
        ),
        None => format!(
            "{} input, {} output tokens",
            pricing.input_tokens, pricing.output_tokens
        ),
    };
    eprintln!(
        "rename llm cost: ${:.6} ({token_text}, {})",
        pricing.total_cost_usd, pricing.model
    );
}

#[derive(Debug, PartialEq, Eq)]
enum RenameMode {
    Auto,
    Manual(String),
    Unset,
}

fn parse_rename_mode(new_name: Option<String>, auto: bool, unset: bool) -> LhResult<RenameMode> {
    let rename_modes = usize::from(new_name.is_some()) + usize::from(unset);
    if rename_modes > 1 || (auto && rename_modes > 0) {
        return Err("provide at most one of [newname], --auto, or --unset".into());
    }
    if unset {
        return Ok(RenameMode::Unset);
    }
    if let Some(name) = new_name {
        return Ok(RenameMode::Manual(validate_thread_name(&name)?));
    }
    Ok(RenameMode::Auto)
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

fn memory(
    cwd: &Path,
    global: bool,
    agent_or_name: Option<String>,
    name: Option<String>,
) -> LhResult<()> {
    let (agent, query) = parse_selector(agent_or_name, name)?;
    let memories = list_memory(cwd, global, agent)?;

    if let Some(query) = query.as_deref() {
        let memory = select_memory(&memories, query)?;
        print_memory_file(memory)?;
        return Ok(());
    }

    if memories.is_empty() {
        if global {
            println!("No memory files found");
        } else {
            println!("No memory files found for {}", cwd.display());
        }
        return Ok(());
    }

    page_or_print(&format_memory_files(&memories))?;
    Ok(())
}

fn alias(
    cwd: &Path,
    symbolic: bool,
    delete: bool,
    source_or_target: Option<PathBuf>,
    target: Option<PathBuf>,
) -> LhResult<()> {
    if delete {
        if symbolic {
            return Err("alias -d cannot be combined with -s".into());
        }
        if target.is_some() {
            return Err("alias -d accepts at most one directory".into());
        }
        return unalias(cwd, source_or_target);
    }

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
    global: bool,
    target: String,
    name: Option<String>,
    force: bool,
    dry_run: bool,
) -> LhResult<()> {
    let (agent, query) = parse_remove_selector(target, name)?;
    let (_provider, thread) = select_provider_thread(cwd, global, agent, Some(&query))?;
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
    for (index, provider) in providers::all().into_iter().enumerate() {
        if index > 0 {
            println!();
        }
        let status = provider.status(cwd);
        let path = status.executable.as_ref().map(|path| shorten_path(path));
        let target = status
            .executable
            .as_ref()
            .and_then(|path| symlink_target(path))
            .map(|path| shorten_path(&path));
        let path = path.unwrap_or_else(|| "-".to_string());
        let version = status
            .version
            .as_ref()
            .map(|version| version_display(version))
            .unwrap_or_else(|| "-".to_string());
        println!("{}:", status.agent.as_str());
        print_agent_value("path:", &path);
        if let Some(target) = target.as_deref() {
            print_agent_value("target:", target);
        }
        print_agent_value("version:", &version);
        print_agent_value("history:", &shorten_path(&status.history_path));
        print_agent_value("threads:", &status.thread_count.to_string());
        if let Some(caveat) = status.caveat.as_deref() {
            print_agent_value("caveat:", caveat);
        }
    }
    Ok(())
}

fn print_agent_value(label: &str, value: &str) {
    println!("  {label:<11}{value}");
}

fn symlink_target(path: &Path) -> Option<PathBuf> {
    if !fs::symlink_metadata(path).ok()?.file_type().is_symlink() {
        return None;
    }
    let target = fs::read_link(path).ok()?;
    let target = if target.is_absolute() {
        target
    } else {
        path.parent()?.join(target)
    };
    Some(fs::canonicalize(&target).unwrap_or(target))
}

fn version_display(version: &str) -> String {
    version
        .split_whitespace()
        .find_map(|part| {
            let part = part.trim_matches(|ch: char| {
                ch == '(' || ch == ')' || ch == ',' || ch == ';' || ch == ':'
            });
            part.chars()
                .next()
                .is_some_and(|ch| ch.is_ascii_digit())
                .then_some(part)
        })
        .unwrap_or(version)
        .to_string()
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

fn list_memory(cwd: &Path, global: bool, agent: Option<AgentKind>) -> LhResult<Vec<MemoryFile>> {
    if let Some(agent) = agent {
        let provider = providers::by_kind(agent);
        return if global {
            provider.list_memory_global()
        } else {
            let dirs = config::alias_group(cwd)?;
            providers::list_memory_provider_for_dirs(&*provider, &dirs)
        };
    }

    if global {
        providers::list_memory_global()
    } else {
        let dirs = config::alias_group(cwd)?;
        providers::list_memory_all_for_dirs(&dirs)
    }
}

fn select_memory<'a>(memories: &'a [MemoryFile], query: &str) -> LhResult<&'a MemoryFile> {
    let query = query.trim().to_ascii_lowercase();
    if query.is_empty() {
        return Err("no memory file selected".into());
    }

    let matches = memories
        .iter()
        .filter(|memory| memory_matches(memory, &query))
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [memory] => Ok(memory),
        [] => Err("no matching memory file found".into()),
        _ => Err(ambiguous_memory_error(matches).into()),
    }
}

fn memory_matches(memory: &MemoryFile, query: &str) -> bool {
    let id = memory.id.to_ascii_lowercase();
    let stem = memory
        .path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let path = memory.path.display().to_string().to_ascii_lowercase();
    let scope = memory.scope.to_ascii_lowercase();
    let cwd = memory
        .cwd
        .as_ref()
        .map(|cwd| cwd.display().to_string().to_ascii_lowercase())
        .unwrap_or_default();
    let preview = memory
        .preview
        .as_deref()
        .unwrap_or_default()
        .to_ascii_lowercase();

    id == query
        || stem == query
        || id.contains(query)
        || path.contains(query)
        || scope.contains(query)
        || cwd.contains(query)
        || preview.contains(query)
}

fn ambiguous_memory_error(candidates: Vec<&MemoryFile>) -> String {
    let mut out = String::from("ambiguous memory match; use a more specific query:");
    for memory in candidates.into_iter().take(8) {
        out.push_str(&format!(
            "\n  {:<7} {:<9} {:<30} {}",
            memory.agent.as_str(),
            memory.scope,
            memory.id,
            shorten_path(&memory.path)
        ));
    }
    out
}

fn format_memory_files(memories: &[MemoryFile]) -> String {
    format_memory_files_for_rows(memories)
}

fn format_memory_files_for_rows(memories: &[MemoryFile]) -> String {
    let updated_width = 19;
    let agent_width = bounded_column_width(
        memories
            .iter()
            .map(|memory| memory.agent.as_str().to_string()),
        10,
    )
    .max("agent".len());
    let scope_width = bounded_column_width(memories.iter().map(|memory| memory.scope.clone()), 12)
        .max("scope".len());
    let dir_width = bounded_column_width(memories.iter().map(memory_dir), 30).max("dir".len());
    let mut out = String::new();
    out.push_str(&format!(
        "{updated:<updated_width$} {agent:<agent_width$} {scope:<scope_width$} {dir:<dir_width$} {path}\n",
        updated = "updated",
        agent = "agent",
        scope = "scope",
        dir = "dir",
        path = "path",
    ));
    for memory in memories {
        let updated = memory
            .updated_at
            .map(format_display_time)
            .unwrap_or_else(|| "-".to_string());
        let dir = memory_dir(memory);
        let path = shorten_path(&memory.path);
        out.push_str(&format!(
            "{updated:<updated_width$} {agent:<agent_width$} {scope:<scope_width$} {dir:<dir_width$} {path}\n",
            updated = common::truncate(&updated, updated_width),
            agent = memory.agent.as_str(),
            scope = memory.scope,
            dir = common::truncate(&dir, dir_width),
            path = path,
        ));
    }
    out
}

fn memory_dir(memory: &MemoryFile) -> String {
    memory
        .cwd
        .as_ref()
        .map(|cwd| shorten_path(cwd))
        .unwrap_or_else(|| "-".to_string())
}

fn print_memory_file(memory: &MemoryFile) -> LhResult<()> {
    let field = |name: &str, value: String| {
        println!("{name:<8} {value}");
    };

    field("Agent", memory.agent.display_name().to_string());
    field("Scope", memory.scope.clone());
    if let Some(cwd) = &memory.cwd {
        field("CWD", cwd.display().to_string());
    }
    field("Path", memory.path.display().to_string());
    field(
        "Updated",
        memory
            .updated_at
            .map(format_time)
            .unwrap_or_else(|| "-".to_string()),
    );
    println!();
    page_or_print(&fs::read_to_string(&memory.path)?)?;
    Ok(())
}

fn format_threads(threads: &[ThreadSummary], columns: &[ListColumn]) -> String {
    let mut out = String::new();
    let widths = list_column_widths_for_columns(threads, columns, terminal_width());
    for thread in threads {
        for (index, (column, width)) in columns.iter().zip(widths.iter()).enumerate() {
            if index > 0 {
                out.push(' ');
            }

            let value = common::truncate(&column.value(thread), *width);
            if index + 1 == columns.len() {
                out.push_str(&value);
            } else {
                out.push_str(&format!("{value:<width$}", width = *width));
            }
        }
        out.push('\n');
    }
    out
}

fn page_or_print(output: &str) -> LhResult<()> {
    if !io::stdout().is_terminal() {
        print!("{output}");
        return Ok(());
    }

    let mut child = pager_command()?.stdin(Stdio::piped()).spawn()?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(output.as_bytes())?;
    }

    let status = child.wait()?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("pager exited with {status}").into())
    }
}

fn pager_command() -> LhResult<Command> {
    let mut command =
        if let Some(pager) = std::env::var_os("PAGER").filter(|value| !value.is_empty()) {
            #[cfg(unix)]
            {
                let shell = std::env::var_os("SHELL").unwrap_or_else(|| OsString::from("/bin/sh"));
                let mut command = Command::new(shell);
                command.arg("-c").arg(pager);
                command
            }

            #[cfg(not(unix))]
            {
                Command::new(pager)
            }
        } else {
            Command::new("less")
        };

    if std::env::var_os("LESS").is_none() {
        command.env("LESS", "FRX");
    }

    Ok(command)
}

fn thread_list_name(thread: &ThreadSummary) -> String {
    thread
        .name
        .clone()
        .or_else(|| thread.preview.clone())
        .unwrap_or_else(|| "-".to_string())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ListColumn {
    Updated,
    Created,
    Agent,
    Id,
    Dir,
    Name,
    Preview,
    Source,
}

impl ListColumn {
    const DEFAULT: [Self; 5] = [Self::Updated, Self::Agent, Self::Id, Self::Dir, Self::Name];

    fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "updated" | "updated_at" => Some(Self::Updated),
            "created" | "created_at" => Some(Self::Created),
            "agent" => Some(Self::Agent),
            "id" => Some(Self::Id),
            "dir" | "cwd" => Some(Self::Dir),
            "name" | "title" => Some(Self::Name),
            "preview" => Some(Self::Preview),
            "source" | "source_path" => Some(Self::Source),
            _ => None,
        }
    }

    fn value(self, thread: &ThreadSummary) -> String {
        match self {
            Self::Updated => thread
                .updated_at
                .or(thread.created_at)
                .map(format_display_time)
                .unwrap_or_else(|| "-".to_string()),
            Self::Created => thread
                .created_at
                .map(format_display_time)
                .unwrap_or_else(|| "-".to_string()),
            Self::Agent => thread.agent.as_str().to_string(),
            Self::Id => thread.id.clone(),
            Self::Dir => shorten_path(&thread.cwd),
            Self::Name => thread_list_name(thread),
            Self::Preview => thread.preview.clone().unwrap_or_else(|| "-".to_string()),
            Self::Source => thread
                .source_path
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "-".to_string()),
        }
    }

    fn non_last_width(self, threads: &[ThreadSummary]) -> usize {
        match self {
            Self::Updated | Self::Created => 19,
            Self::Agent => bounded_column_width(
                threads
                    .iter()
                    .map(|thread| thread.agent.as_str().to_string()),
                10,
            ),
            Self::Id => bounded_column_width(threads.iter().map(|thread| thread.id.clone()), 36),
            Self::Dir => {
                bounded_column_width(threads.iter().map(|thread| shorten_path(&thread.cwd)), 30)
            }
            Self::Name => bounded_column_width(threads.iter().map(thread_list_name), 60),
            Self::Preview => bounded_column_width(
                threads
                    .iter()
                    .map(|thread| thread.preview.clone().unwrap_or_else(|| "-".to_string())),
                60,
            ),
            Self::Source => bounded_column_width(
                threads.iter().map(|thread| {
                    thread
                        .source_path
                        .as_ref()
                        .map(|path| path.display().to_string())
                        .unwrap_or_else(|| "-".to_string())
                }),
                60,
            ),
        }
    }
}

fn parse_list_columns(values: &[String]) -> LhResult<Vec<ListColumn>> {
    if values.is_empty() {
        return Ok(ListColumn::DEFAULT.to_vec());
    }

    let mut columns = Vec::new();
    for value in values {
        let value = value.trim();
        if value.is_empty() {
            continue;
        }
        let Some(column) = ListColumn::parse(value) else {
            return Err(format!(
                "unknown list column '{value}'; expected one of updated, created, agent, id, dir, cwd, name, preview, source"
            )
            .into());
        };
        columns.push(column);
    }

    if columns.is_empty() {
        return Err("no list columns specified".into());
    }

    Ok(columns)
}

#[cfg(test)]
#[derive(Debug, PartialEq, Eq)]
struct ListColumnWidths {
    agent: usize,
    id: usize,
    dir: usize,
    name: usize,
}

#[cfg(test)]
fn list_column_widths(threads: &[ThreadSummary], terminal_width: usize) -> ListColumnWidths {
    let widths = list_column_widths_for_columns(threads, &ListColumn::DEFAULT, terminal_width);
    let agent = widths[1];
    let id = widths[2];
    let dir = widths[3];
    let name = widths[4];

    ListColumnWidths {
        agent,
        id,
        dir,
        name,
    }
}

fn list_column_widths_for_columns(
    threads: &[ThreadSummary],
    columns: &[ListColumn],
    terminal_width: usize,
) -> Vec<usize> {
    let Some((_last, non_last)) = columns.split_last() else {
        return Vec::new();
    };

    let mut widths = non_last
        .iter()
        .map(|column| column.non_last_width(threads))
        .collect::<Vec<_>>();
    let fixed_width = widths.iter().sum::<usize>();
    let separator_count = columns.len().saturating_sub(1);
    let last_width = terminal_width
        .saturating_sub(1)
        .saturating_sub(fixed_width)
        .saturating_sub(separator_count)
        .max(1);
    widths.push(last_width);
    widths
}

fn bounded_column_width(values: impl IntoIterator<Item = String>, max_width: usize) -> usize {
    values
        .into_iter()
        .map(|value| value.chars().count())
        .max()
        .unwrap_or(1)
        .min(max_width)
}

#[cfg(test)]
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
    fn inserts_default_list_for_output_flags() {
        assert_eq!(
            normalize_args(strings(&["lh", "-o", "agent,id"])),
            strings(&["lh", "list", "-o", "agent,id"])
        );
    }

    #[test]
    fn list_parses_output_columns() {
        let cli = Cli::try_parse_from(strings(&["lh", "ls", "-o", "agent,id", "name"])).unwrap();

        assert!(matches!(
            cli.command,
            Some(Commands::List { output, .. })
                if output == vec![
                    "agent".to_string(),
                    "id".to_string(),
                    "name".to_string()
                ]
        ));
    }

    #[test]
    fn moves_global_flag_in_front_of_rename() {
        assert_eq!(
            normalize_args(strings(&[
                "lh",
                "-g",
                "rename",
                "abc123",
                "--unset",
                "--dry-run"
            ])),
            strings(&["lh", "rename", "-g", "abc123", "--unset", "--dry-run"])
        );
    }

    #[test]
    fn moves_global_flag_in_front_of_info() {
        assert_eq!(
            normalize_args(strings(&["lh", "--global", "info", "abc123"])),
            strings(&["lh", "info", "--global", "abc123"])
        );
    }

    #[test]
    fn moves_global_flag_in_front_of_memory() {
        assert_eq!(
            normalize_args(strings(&["lh", "-g", "memory", "MEMORY.md"])),
            strings(&["lh", "memory", "-g", "MEMORY.md"])
        );
    }

    #[test]
    fn moves_global_flag_in_front_of_remove() {
        assert_eq!(
            normalize_args(strings(&["lh", "-g", "rm", "abc123", "--dry-run"])),
            strings(&["lh", "rm", "-g", "abc123", "--dry-run"])
        );
    }

    #[test]
    fn rename_defaults_to_auto_when_name_is_omitted() {
        assert_eq!(
            parse_rename_mode(None, false, false).unwrap(),
            RenameMode::Auto
        );
    }

    #[test]
    fn rename_accepts_explicit_auto() {
        assert_eq!(
            parse_rename_mode(None, true, false).unwrap(),
            RenameMode::Auto
        );
    }

    #[test]
    fn rename_accepts_manual_name() {
        assert_eq!(
            parse_rename_mode(Some("manual name".to_string()), false, false).unwrap(),
            RenameMode::Manual("manual name".to_string())
        );
    }

    #[test]
    fn rename_rejects_manual_name_with_auto() {
        let error = parse_rename_mode(Some("manual name".to_string()), true, false).unwrap_err();

        assert_eq!(
            error.to_string(),
            "provide at most one of [newname], --auto, or --unset"
        );
    }

    #[test]
    fn rename_llm_overrides_merge_with_config() {
        let config = config::Config {
            llm: Some(config::LlmConfig {
                provider: "anthropic".to_string(),
                model: Some("claude-haiku-4-5".to_string()),
                prompt: "base prompt".to_string(),
            }),
            ..Default::default()
        };

        let merged = rename_llm_config(
            config,
            Some("gemini".to_string()),
            Some("gemini-3.1-flash-lite".to_string()),
            Some("override prompt".to_string()),
        )
        .unwrap();
        let llm = merged.llm.unwrap();
        assert_eq!(llm.provider, "gemini");
        assert_eq!(llm.model.as_deref(), Some("gemini-3.1-flash-lite"));
        assert_eq!(llm.prompt, "override prompt");
    }

    #[test]
    fn rename_llm_overrides_can_supply_missing_config() {
        let merged = rename_llm_config(
            config::Config::default(),
            Some("openai".to_string()),
            Some("gpt-5.4-nano".to_string()),
            Some("name this thread".to_string()),
        )
        .unwrap();
        let llm = merged.llm.unwrap();
        assert_eq!(llm.provider, "openai");
        assert_eq!(llm.model.as_deref(), Some("gpt-5.4-nano"));
        assert_eq!(llm.prompt, "name this thread");
    }

    #[test]
    fn rename_llm_overrides_require_provider_and_prompt_without_config() {
        let error = rename_llm_config(
            config::Config::default(),
            Some("openai".to_string()),
            Some("gpt-5.4-nano".to_string()),
            None,
        )
        .unwrap_err();

        assert_eq!(
            error.to_string(),
            "auto rename with no [llm] config requires both --provider and --prompt"
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
    fn top_level_help_hides_unalias() {
        let result = Cli::try_parse_from(strings(&["lh", "--help"]));

        assert!(matches!(
            result,
            Err(error)
                if error.kind() == clap::error::ErrorKind::DisplayHelp
                    && !error.to_string().contains("unalias")
        ));
    }

    #[test]
    fn alias_delete_parses_optional_directory() {
        let cli = Cli::try_parse_from(strings(&["lh", "alias", "-d", "../other"])).unwrap();

        assert!(matches!(
            cli.command,
            Some(Commands::Alias {
                delete: true,
                source_or_target: Some(_),
                target: None,
                ..
            })
        ));
    }

    #[test]
    fn agent_ls_aliases_agent_list() {
        let cli = Cli::try_parse_from(strings(&["lh", "agent", "ls"])).unwrap();

        assert!(matches!(
            cli.command,
            Some(Commands::Agent {
                command: AgentsCommand::List
            })
        ));
    }

    #[test]
    fn agents_aliases_agent() {
        let cli = Cli::try_parse_from(strings(&["lh", "agents", "list"])).unwrap();

        assert!(matches!(
            cli.command,
            Some(Commands::Agent {
                command: AgentsCommand::List
            })
        ));
    }

    #[test]
    fn memory_command_parses_agent_selector() {
        let cli = Cli::try_parse_from(strings(&["lh", "mem", "claude", "MEMORY.md"])).unwrap();

        assert!(matches!(
            cli.command,
            Some(Commands::Memory {
                agent_or_name: Some(agent),
                name: Some(name),
                ..
            }) if agent == "claude" && name == "MEMORY.md"
        ));
    }

    #[test]
    fn format_memory_files_shows_dir_and_path_columns() {
        let memories = vec![
            MemoryFile {
                agent: AgentKind::Claude,
                id: "MEMORY.md".to_string(),
                scope: "project".to_string(),
                cwd: Some(PathBuf::from("/tmp/project")),
                path: PathBuf::from("/tmp/memory/MEMORY.md"),
                updated_at: None,
                preview: Some("# Project memory".to_string()),
            },
            MemoryFile {
                agent: AgentKind::Codex,
                id: "podman-preference.md".to_string(),
                scope: "global".to_string(),
                cwd: None,
                path: PathBuf::from("/tmp/memories/podman-preference.md"),
                updated_at: None,
                preview: Some("Prefer podman over docker".to_string()),
            },
        ];

        let output = format_memory_files_for_rows(&memories);

        assert!(output.starts_with("updated"));
        assert!(output.lines().next().unwrap().contains("dir"));
        assert!(output.lines().next().unwrap().contains("path"));
        assert!(!output.lines().next().unwrap().contains("preview"));
        assert!(output.contains("claude project /tmp/project"));
        assert!(output.contains("/tmp/memory/MEMORY.md"));
        assert!(output.contains("codex  global  -"));
        assert!(output.contains("/tmp/memories/podman-preference.md"));
        assert!(!output.contains("Prefer podman over docker"));
    }

    #[test]
    fn version_display_uses_first_version_token() {
        assert_eq!(version_display("2.1.154 (Claude Code)"), "2.1.154");
        assert_eq!(version_display("codex-cli 0.134.0"), "0.134.0");
    }

    #[cfg(unix)]
    #[test]
    fn symlink_target_resolves_relative_links() {
        let root = crate::util::temp_dir("agent-path-display");
        let target = root.join("target");
        let link = root.join("link");
        fs::write(&target, "").unwrap();
        std::os::unix::fs::symlink("target", &link).unwrap();

        assert_eq!(
            symlink_target(&link),
            Some(fs::canonicalize(&target).unwrap())
        );
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
    fn remove_parses_global_flag() {
        let cli = Cli::try_parse_from(strings(&["lh", "rm", "-g", "abc123"])).unwrap();

        assert!(matches!(
            cli.command,
            Some(Commands::Remove {
                global: true,
                target,
                ..
            }) if target == "abc123"
        ));
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
    fn parses_list_columns_from_field_names() {
        assert_eq!(
            parse_list_columns(&[
                "updated".to_string(),
                "agent".to_string(),
                "id".to_string(),
                "dir".to_string(),
                "name".to_string()
            ])
            .unwrap(),
            ListColumn::DEFAULT
        );
    }

    #[test]
    fn rejects_unknown_list_columns() {
        let error = parse_list_columns(&["bogus".to_string()]).unwrap_err();

        assert_eq!(
            error.to_string(),
            "unknown list column 'bogus'; expected one of updated, created, agent, id, dir, cwd, name, preview, source"
        );
    }

    #[test]
    fn format_threads_respects_selected_columns() {
        let thread = ThreadSummary {
            agent: AgentKind::Codex,
            id: "abc123".to_string(),
            name: Some("short thread".to_string()),
            cwd: PathBuf::from("/tmp"),
            created_at: None,
            updated_at: None,
            source_path: None,
            preview: Some("preview".to_string()),
            removable: None,
            resume_hint: None,
        };

        assert_eq!(
            format_threads(
                &[thread],
                &[ListColumn::Agent, ListColumn::Id, ListColumn::Name]
            ),
            "codex abc123 short thread\n"
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
    fn list_name_uses_dash_when_thread_has_no_name_or_preview() {
        let thread = ThreadSummary {
            agent: AgentKind::Claude,
            id: "abc123".to_string(),
            name: None,
            cwd: PathBuf::from("/tmp"),
            created_at: None,
            updated_at: None,
            source_path: None,
            preview: None,
            removable: None,
            resume_hint: None,
        };

        assert_eq!(thread_list_name(&thread), "-");
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
