# ironquill

**A terminal coding agent that lets deterministic tools do the work before any model does.**

## Idea

Most coding agents are a chat with a model that can run commands. ironquill is the other way round: compilers, tests, linters, the LSP and git do everything they can, and a model is called only for what they cannot do. A patch is kept when the checks pass, and a more capable model is called only when a cheaper one has failed them.

The goals, in order: reliability, cost, then speed. Every request shows what it cost.

## Status

Early-stage personal project. What works today:

- `ironquill` with no subcommand opens a conversation in the terminal. Ask a question and it answers; ask for a change and it edits the files, shows each edit as a diff, then runs the checks. Only a change triggers the checks, and only failing checks call the stronger models. History carries over from one message to the next until `/clear`. Commands start with `/` (`/help` lists them), or with `:` from Vim's normal mode, where `j`/`k` scroll. The status line shows the models and the session's tokens and cost as they accrue. Ctrl-C stops a request
- Conversations are saved after every request under `~/.ironquill/sessions/<project>/` (or `$IRONQUILL_HOME`), with what the model saw, so that `ironquill -c` continues the last one and `ironquill -r` or `/resume` picks one from a list. `/name` names the current one. Each request shows its cost and tokens under it, the status line shows the conversation's total, `/cost` sums it up. Ctrl-C stops a request, Ctrl-C twice quits
- Ctrl-G goes back to typing a message from anywhere, the open file included. Ctrl-K shows or hides a pane listing the running Docker containers, refreshed every two seconds
- A file tree opens beside the conversation with Ctrl-B, or `,n` from normal mode. Arrows move, Enter opens a file in the middle pane with the conversation moved to the right, Tab switches panes, and the mouse clicks and scrolls. Files the agent changed are marked, and an open file follows its edits as they happen
- An open file is coloured by its language and edited with Vim keys: insert, visual mode (`v`, `V`), delete, yank and put with registers (`"+` is the system clipboard), undo and redo, `:w`, `:q`, `:42`, `:s` with ranges, `/` search. A frame under the file shows the mode and the command being typed. Unsaved edits are never overwritten by the agent
- `ironquill do "<task>"` changes the project in the current directory until its checks pass. The model reads and edits files through a sandbox that refuses paths outside the project; it cannot run commands. When it stops, ironquill runs the checks itself (`cargo check --all-targets` and `cargo test` by default, any command with `--check`). A failure goes back to the same model, cut to the lines worth reading; after two rounds the next model given with `--escalate` takes over from a short brief, not the whole history. Every turn shows its tokens and cost
- `ironquill ask` sends one question to any OpenAI compatible endpoint (Requesty by default) and prints the answer with its token counts and its cost, priced from the provider's model list
- TLS is verified against the operating system trust store, so a machine behind a corporate proxy works without extra setup

## Usage

```bash
export IRONQUILL_API_KEY=...        # your provider key
export IRONQUILL_MODEL=<model-id>        # as your provider names it
ironquill ask "What does Rust's ? operator do?"

# in a git repository: the interface
ironquill --model <cheap-model> --escalate <strong-model>

# or one task, no interface
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
