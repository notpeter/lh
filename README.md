# lh: LLM History

Unified LLM agent thread history

## Usage

`lh` lists recent LLM agent threads for the current directory across Claude Code,
Codex, OpenCode, and Gemini.

```sh
lh list
lh ls
lh ls -g
lh ls -g -5
lh ls -g -10
lh ls -o updated,agent,id,dir,name
lh info [agent] [name-or-id]
lh info -g [agent] [name-or-id]
lh alias [target]
lh alias [alias] [target]
lh alias -d [dir]
lh ln [-s] [alias] [target]
lh cd [alias-query]
lh agent list
lh agent ls

lh new [agent] [name]
lh resume [agent] [name-or-id]
lh rename [thread-id] [new-name]
lh rename [thread-id] --auto
lh rename [thread-id] --auto --dry-run
lh remove <name-or-id> --dry-run
lh remove <agent> <name-or-id> --force
```

Agent aliases include `claude`, `claude-code`, `codex`, `opencode`, `open-code`,
`gemini`, and `gemini-cli`. Commands read agent histories directly from their
native stores.

`lh ls` is scoped to the current directory. `lh ls -g` scans all known agent
history. List output is unlimited by default and uses `$PAGER` or `less` when
stdout is a terminal, with Git-style `LESS=FRX` defaults when `LESS` is unset;
when stdout is piped, it prints directly. Use `-5`, `-10`, or any other numeric
shorthand to limit the row count.
Use `-o`/`--output` to choose list columns. Fields can be comma-separated or
repeated, and supported fields are `updated`, `created`, `agent`, `id`, `dir`,
`cwd`, `name`, `preview`, and `source`.
`lh info` prints full details for a selected thread, including its source path.
`lh rename` updates the native agent title for providers with known writable
title storage. `lh rename [thread-id] --auto` uses the optional `[llm]` config:

```toml
[llm]
provider = "openai"
# Optional; defaults to "gpt-5.4-nano" for OpenAI.
model = "gpt-5.4-nano"
prompt = """Summarize the contents of this thread extremely concisely.
Create a multi-word-kebab-case-string of between 10-60 chars as title for this thread.
Only output that single kebab-case string.
"""
```

For `provider = "openai"`, `OPENAI_API_KEY` must be set in the environment.
Supported providers and default models:

| Provider | API key env var | Default model |
| --- | --- | --- |
| `openai` | `OPENAI_API_KEY` | `gpt-5.4-nano` |
| `anthropic` | `ANTHROPIC_API_KEY` | `claude-haiku-4-5` |
| `gemini` | `GEMINI_API_KEY` | `gemini-3.1-flash-lite` |

`lh alias ../other-clone` records the current directory as an alias of another
directory in `~/.config/llm-history.toml`. `lh alias . ../other-clone` is
equivalent. Running `lh alias` without arguments prints aliases that impact the
current directory. Aliased directories share local listings and selection, while
the `DIR` column shows which checkout owns each thread. `lh cd foo2` opens a
shell in a matching aliased directory. `lh alias -d` removes aliases involving
the current directory; pass a directory to remove aliases involving that
directory.

## License

MIT Licensed
