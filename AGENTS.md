# AGENTS.md

## Project Context

- `lh` is a Rust CLI for listing and resuming LLM agent thread history.
- Providers live in separate modules under `src/` and implement `AgentProvider`.
- Default listing is scoped to the current directory; `lh ls -g` scans all known history.
- Agent history formats are private and best-effort, so parser failures should not break other providers.

## Useful Commands

- Build quietly: `cargo build -q`
- Run tests quietly: `cargo test -q`
- Fix formatting: `cargo fmt`
- Fix simple Clippy findings: `cargo clippy --fix --allow-dirty --all-targets -- -D warnings`
- Run Clippy quietly: `cargo clippy -q --all-targets -- -D warnings`
- Try local output: `cargo run -q -- ls` or `cargo run -q -- ls -g -5`

## Change Guidelines

- Keep provider-specific parsing in the matching provider module.
- Preserve stateless behavior for normal CLI commands unless explicitly changing DB behavior.
- Do not remove or rewrite agent auth/config files; removals should target only thread artifacts.
- Prefer small, focused tests with temporary fixtures for private agent data formats.
