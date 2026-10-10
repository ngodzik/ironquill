# ironquill coding rules

## Language

**All code, comments, doc-comments, variable names, error messages, and commit messages must be in English.**

## Prose and Markdown

**Never use em dashes or en dashes in Markdown files (README, CLAUDE.md, docs).** Use a comma, a colon, parentheses, or a new sentence instead.

**Never hard-wrap prose at a fixed column.** One paragraph is one line, one bullet is one line. Fenced blocks and table rows keep their own line structure.

## Principle

**A deterministic tool does the work before any model does.** Compilers, tests, linters, the LSP and git are cheap, fast and right. A model is called for what they cannot do, and its output is checked by them before it is kept. A feature that sends a model what a tool could have answered is a bug.

## Architecture

Separation of concerns is **enforced by the crate dependency graph**, not by convention. The compiler refuses inverted dependencies.

```
                crates/core   ← no internal dependencies, no I/O
                ↑         ↑
      crates/llm          crates/tools
     (core)               (core)
          ↑               ↑
          |         crates/agent  (core + tools, never llm)
          |               ↑
          |         crates/ui     (core + tools + agent, never llm, never a terminal or window library)
          |           ↑         ↑
          |   crates/tui       crates/gui   (each: core + tools + agent + ui, never llm, never the other;
          |                         ↑        gui also: codemap)
          |                    crates/codemap  (no internal dependencies, no rendering)
          |           ↑         ↑
          ironquill  (bin, depends on all)
```

| Crate             | Single responsibility                                                      |
|-------------------|----------------------------------------------------------------------------|
| `ironquill-core`  | Domain types and traits, zero I/O, zero network                            |
| `ironquill-llm`   | Model providers, one `impl ChatModel` per protocol, and the Claude Code `Delegate` |
| `ironquill-tools` | Deterministic tools: sandboxed workspace, file edits, checks, git          |
| `ironquill-agent` | The loop: edit, check, retry, escalate. Generic over `ChatModel`           |
| `ironquill-ui`    | The interface's state: modes, keys, commands, the editor. Draws nothing    |
| `ironquill-tui`   | Draws `ironquill-ui`'s state in the terminal and runs its loop. Generic over `ChatModel` |
| `ironquill-gui`   | Draws `ironquill-ui`'s state in a window, with egui on Bevy (`--gui`). Generic over `ChatModel` |
| `ironquill-codemap` | The codebase as a graph (folders, files, imports read by patterns, Python packages and TypeScript aliases resolved, a front end's calls to its back end through an OpenAPI spec), its design one level at a time (parts, packages found by their manifests, what uses what, layers, loops), its files grouped by module, language, layer or role, its services (those of its Compose files and the folders that serve an API or MCP tools) and who reaches whom, through an OpenAPI spec, over MCP or by a URL naming a service, each link with where it was read, the tables its SQLAlchemy and SQLModel models define (columns, keys, indexes, foreign keys), a branch's changes read for review (routes from the OpenAPI specs before and after, Alembic migrations, models' tables, files by area), and a force-directed layout that gathers groups into galaxies. Draws nothing |
| `ironquill`       | CLI entry point                                                            |

In `ironquill-ui`, keys become actions only in `keymap.rs`, and `App` turns actions into state changes and returns an `Effect` for the loop instead of doing I/O. It reads keys, clicks and areas in its own types (`input.rs`) and colours as RGB (`style.rs`), never a terminal's or a window's: a backend translates its events into them. Effects are carried out by `ironquill-ui`'s `Host`, the same for every backend, never by a backend itself. `ironquill-tui`'s `view.rs` draws the state without changing anything, and so does `ironquill-gui`, which drives the `Host` once a frame without ever waiting on it. The window sleeps between keys: nothing in it may ask egui to repaint unless something moves, since bevy_egui takes any request, even one for later, as one for now. A new key binding touches `keymap.rs` only; a new `:` command touches `command.rs` and `App::run_command`.

`ironquill-agent` does not depend on `ironquill-llm`: it is generic over `ChatModel`, which is what lets its tests run against a scripted model with no network.

A `Delegate` (Claude Code, Codex) is an agent rather than a model: it gets a whole task, edits files and runs commands itself, and reports what it does as events. Claude Code runs in its own auto permission mode, Codex in its sandbox; a planner or reviewer only reads. What their safety checks refuse is shown, for the person to approve in their next message.

A model edits files through `Toolbox` and runs commands through the agent's `run_command`, where `ironquill_tools::assess` holds what cannot be undone, leaves the machine or cannot be read, for the person to approve; a command held only because it cannot be read is first read by the cheapest priced model. Writing where code runs later (hooks, CI workflows, shell start-up files, tools' settings) is held, and every command is written to `~/.ironquill/audit.log`. A command that would show a secret is refused outright; secrets reach a command only when a program reads its own or the person allowed one it names, and output is redacted of them. Checks are run by the agent, so a model can neither skip nor fake them; they judge a pair and `ironquill do`, not a request in the conversation.

A new crate (workflows, context, TUI) is added when its first real code lands, not before, and this graph is updated in the same commit.

## Idiomatic Rust patterns

1. **Traits** are the only abstraction mechanism. `impl Trait` for static dispatch, `dyn Trait` only for heterogeneous collections.
2. **Newtypes** for domain primitives: `TokenCount`, `Usd`, `ModelId`. Never a bare `u64` or `f64` where a domain type makes sense.
3. **Typestate** for lifecycles where calling a method in the wrong state is a bug the compiler can catch.
4. **Builders** for complex configuration, validated at `build()`, returning `Result`.
5. **Iterators** over indexed loops.

## Non-negotiable rules

### Error handling
- `thiserror` in library crates: explicit error types
- `anyhow` in the binary
- Never `.unwrap()` or `.expect("...")` in library code
- An error keeps its source: wrap with `#[source]`, never stringify the cause away

### Secrets
- API keys come from the environment, never from a flag value in shell history, never from a committed file
- A type that holds a key implements `Debug` by hand and redacts it

### Unsafe
- `#![deny(unsafe_code)]` in every crate, the binary included

### Visibility
- `pub` only for items that are part of the crate's public API
- `pub(crate)` to share between modules within a crate
- No `pub use *`: re-exports are explicit

### Testing
- Unit tests `#[cfg(test)]` in the relevant module
- Integration tests in `crate/tests/`
- `proptest` for invariants (a session total does not depend on how it was split into requests)
- Wire formats are tested against fixed JSON bodies, never against a live provider

### Documentation
- Every `pub` item: `///` doc-comment required
- `# Examples` for non-trivial functions
- Comments explain WHY, not what the name already says

## Commands

```bash
scripts/check.sh    # everything CI runs, locally
```
