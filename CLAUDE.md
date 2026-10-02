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
          ironquill  (bin, depends on all)
```

| Crate             | Single responsibility                                                      |
|-------------------|----------------------------------------------------------------------------|
| `ironquill-core`  | Domain types and traits, zero I/O, zero network                            |
| `ironquill-llm`   | Model providers, one `impl ChatModel` per protocol                         |
| `ironquill-tools` | Deterministic tools: sandboxed workspace, file edits, checks, git          |
| `ironquill-agent` | The loop: edit, check, retry, escalate. Generic over `ChatModel`           |
| `ironquill`       | CLI entry point                                                            |

`ironquill-agent` does not depend on `ironquill-llm`: it is generic over `ChatModel`, which is what lets its tests run against a scripted model with no network.

The model never runs commands. It edits files through `Toolbox`; checks are run by the agent, so a model can neither skip nor fake them.

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
