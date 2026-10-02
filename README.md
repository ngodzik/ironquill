# ironquill

**A terminal coding agent that lets deterministic tools do the work before any model does.**

## Idea

Most coding agents are a chat with a model that can run commands. ironquill is the other way round: compilers, tests, linters, the LSP and git do everything they can, and a model is called only for what they cannot do. A patch is kept when the checks pass, and a more capable model is called only when a cheaper one has failed them.

The goals, in order: reliability, cost, then speed. Every request shows what it cost.

## Status

Early-stage personal project. What works today:

- `ironquill do "<task>"` changes the project in the current directory until its checks pass. The model reads and edits files through a sandbox that refuses paths outside the project; it cannot run commands. When it stops, ironquill runs the checks itself (`cargo check --all-targets` and `cargo test` by default, any command with `--check`). A failure goes back to the same model, cut to the lines worth reading; after two rounds the next model given with `--escalate` takes over from a short brief, not the whole history. Every turn shows its tokens and cost
- It refuses to start on a git tree with uncommitted changes, so that undoing it never undoes you
- `ironquill ask` sends one question to any OpenAI compatible endpoint (Requesty by default) and prints the answer with its token counts and its cost, priced from the provider's model list
- TLS is verified against the operating system trust store, so a machine behind a corporate proxy works without extra setup

## Usage

```bash
export IRONQUILL_API_KEY=...        # your provider key
export IRONQUILL_MODEL=<model-id>        # as your provider names it
ironquill ask "What does Rust's ? operator do?"

# in a git repository with a clean working tree
ironquill do "make the parser accept trailing commas" \
    --model <cheap-model> --escalate <strong-model>
```

`IRONQUILL_BASE_URL` points it at another OpenAI compatible endpoint.

## Building

```bash
cargo build --release               # the binary is target/release/ironquill
scripts/check.sh                    # formatting, lints, docs, tests, dependency audit
git config core.hooksPath .githooks # run the checks before every commit
```

## License

MIT
