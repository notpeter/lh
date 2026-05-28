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
lh ls -g --all
lh info [agent] [name-or-id]
lh info -g [agent] [name-or-id]
lh alias [target]
lh alias [alias] [target]
lh unalias [dir]
lh ln [-s] [alias] [target]
lh cd [alias-query]
lh agents list

lh new [agent] [name]
lh resume [agent] [name-or-id]
lh remove [agent] [name-or-id] --dry-run
lh remove [agent] [name-or-id] --force

lh db init
lh db refresh
lh db drop
```

Agent aliases include `claude`, `claude-code`, `codex`, `opencode`, `open-code`,
`gemini`, and `gemini-cli`. By default, commands read agent histories directly
from their native stores; the SQLite database is an explicit cache under the
platform data directory.

`lh ls` is scoped to the current directory. `lh ls -g` scans all known agent
history and shows the 10 most recent rows by default. Use `-5`, `-10`, or any
other numeric shorthand to change the row count; use `--all` for no limit.
`lh info` prints full details for a selected thread, including its source path.

`lh alias ../other-clone` records the current directory as an alias of another
directory in `~/.config/llm-history.toml`. `lh alias . ../other-clone` is
equivalent. Running `lh alias` without arguments prints aliases that impact the
current directory. Aliased directories share local listings and selection, while
the `DIR` column shows which checkout owns each thread. `lh cd foo2` opens a
shell in a matching aliased directory. `lh unalias` removes aliases involving
the current directory; pass a directory to remove aliases involving that
directory.

## License

MIT Licensed
