# lh: LLM History

Unified LLM agent thread history

`lh` lists recent CLI agent threads for Claude, Codex, OpenCode, Antigravity, Zed, and Pi.

## Usage

### Example

```shell
% lh
2026-06-23 09:16:02 agy      ea2efcfa-0f4f-474a-9a43-4ea92cd70fae ~/code/lh add-antianti-gravy-support
2026-06-21 14:44:45 pi       019eeb59-b4e8-7e62-b141-d0a257646678 ~/code/lh pi-name-sessions
2026-06-18 10:54:14 codex    019edb26-8740-7bc2-95b5-3c872ee81a32 ~/code/lh agent-filtering-in-list
2026-06-18 10:32:42 codex    019edb00-57b9-7241-bd5c-2547dc18afb7 ~/code/lh load-in-claude-gui
2026-06-18 10:05:24 codex    019edb0c-ebd5-77e0-a513-f32c34050a2a ~/code/lh lf-pager-like-git-diff
2026-05-29 08:12:34 codex    019e73a5-8862-7433-947a-a4e16bf39220 ~/code/lh remove-lh-ls-output-headers-thread
2026-05-28 15:14:14 opencode ses_18ffe230fffecfckr370K3M0XM       ~/code/lh renaming-opencode-sessions-via-lh-cli-tool
```

### Help

```shell
% lh --help
Unified LLM agent thread history

Usage: lh [COMMAND]

Commands:
  list    List agent threads for the current directory
  new     Start a new agent thread
  resume  Resume an existing agent thread
  rename  Rename a native agent thread
  move    Reattach a thread to another directory
  info    Show full details for a selected thread
  alias   Link the current directory with another checkout
  remove  Remove a selected thread from native history
  agent   Inspect configured agent providers
  help    Print this message or the help of the given subcommand(s)
```

### Options

```
  -g, --global              Scan all known agent history
  -C, --directory <DIR>     Search threads for a specific directory
  -a, --agent <AGENT>       Only list threads for one agent
      --limit <N>           Limit the number of rows shown
  -o, --output <FIELDS>...  Columns to show, comma-separated or repeated: updated, created, agent, id, model, dir, cwd, name, preview, source
      --search <TERM>...    Filter rows by one or more case-insensitive terms; all terms must match
      --regex <REGEX>...    Filter rows by one or more regular expressions; all regexes must match
  -h, --help                Print help
```

lh new [agent] [name]
lh resume [agent] [name-or-id]
lh resume -g [agent] [name-or-id]
lh rename [thread-id] [new-name]
lh mv <name-or-id> [dir]
lh remove <agent> <name-or-id> --force
```

### Alias / Moves

If you have multiple local checkouts of a single project that you would like to be linked
you can use the alias command.

```shell

user@host~/code/lh % cd ~/code/lh2
user@host~/code/lh % lh alias ~/code/my-project
user@host~/code/lh % lh ls
2026-05-27 10:00:24 codex    019e69bb-0df3-7c60-8e13-166cd939bb08 ~/code/my-project  create-readme.md
2026-05-26 15:13:21 opencode ses_196465859ffeQn1sT67NGi5Pof       ~/code/my-project2 adding-mit-license-to-cargo-toml
```

You can also move a thread between directories:
```
user@host~/code/lh % lh mv 019e69bb-0df3-7c60-8e13-166cd939bb08
user@host~/code/lh % lh ls
2026-05-27 10:00:24 codex    019e69bb-0df3-7c60-8e13-166cd939bb08 ~/code/my-project2  create-readme.md
2026-05-26 15:13:21 opencode ses_196465859ffeQn1sT67NGi5Pof       ~/code/my-project2 adding-mit-license-to-cargo-toml
user@host~/code/lh % lh resume
# ... runs codex resume 019e69bb-0df3-7c60-8e13-166cd939bb08
```

### Rename

You can manually rename threads with `lh rename <id> <new-name>` or from within your agent with `/rename`.

#### LLM Rename (Beta)

Alternatively you can spend a fraction of a penny and ask an llm API to do it.

```
lh rename <thread-id> --auto

```

This requires a small bit of config:

```toml
[llm]
provider = "" # [openai|anthropic|gemini] 
# model = ""  # optional, defaults to a cheap model 
prompt = """Summarize the contents of this thread extremely concisely.
Create a multi-word-kebab-case-string of between 10-60 chars as title for this thread.
Only output that single kebab-case string.
"""
```

Also requires a matching environment variable:

| Provider | API key env var | Default model |
| ----------- | ------------------ | ----------------------- |
| `openai`    | `OPENAI_API_KEY`   | `gpt-5.4-nano`          |
| `anthropic` | `ANTHROPIC_API_KEY`| `claude-haiku-4-5`      |
| `gemini`    | `GEMINI_API_KEY`   | `gemini-3.1-flash-lite` |

## License

MIT Licensed
