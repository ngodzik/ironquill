# ironquill

[![CI](https://github.com/ngodzik/ironquill/actions/workflows/ci.yml/badge.svg)](https://github.com/ngodzik/ironquill/actions/workflows/ci.yml) [![Security](https://github.com/ngodzik/ironquill/actions/workflows/security.yml/badge.svg)](https://github.com/ngodzik/ironquill/actions/workflows/security.yml)

**An experimental terminal coding agent, built around my own way of working. It comes with no guarantee of any kind.**

## Read this first

This is a personal experiment, not a product. It is shaped by my habits and my needs, it changes whenever those do, and it has been tested far less than anything you should rely on.

- **It changes your files without asking.** Edits are written to disk as the model makes them, with no confirmation step. Run it in a git repository, commit before you start, and read every diff it shows you.
- **Watch it closely.** It can misread a request, edit the wrong thing, or keep going on a bad path. Stop it with Ctrl-C the moment something looks wrong.
- **It spends money.** Every request goes to a paid API, within a budget per request that you set. The cost shown is the one the provider reports; when a provider reports none, the total is incomplete, and the interface says so.
- **It has seen little real use.** Around a hundred unit tests, most of the agent loop exercised against a fake server, a handful of sessions with a real model. Used on Linux; on macOS it is only built and tested by CI.

**If you are looking for a coding agent to use, use one of these instead:** [OpenCode](https://opencode.ai), [Pi](https://pi.dev) or [Claude Code](https://code.claude.com/docs/en/overview). They are far more mature, far better tested, more general, and supported by people whose work it is. ironquill overlaps with them on purpose: writing one is how I learn what makes them work and try ideas of my own.

## The idea

Deterministic tools before models. Compilers, tests, linters and git are cheap, fast and right, so they do what they can, and a model is called for the rest. A change is kept when the project's checks pass. A cheap model goes first, and a stronger one is called only when the cheap one has failed the checks, starting from a short brief rather than the whole history. Every request shows what it cost.

The interface follows my editor habits: Vim-like modes, a leader key, files opened beside the conversation and edited with Vim keys.

## What it does today

- A conversation in the terminal: questions get answers, requests for changes get edits shown as diffs, then the checks (`cargo check` and `cargo test` in a Rust project, any command with `/check`)
- Escalation from a cheap model to stronger ones when the checks keep failing
- A team: the model that answers may hand a task to another model you picked, a stronger one for a hard change, a cheaper one for a long read, and gets its report back. Ctrl-E searches every model of the provider, with its prices and context; Space puts one in the team
- Scores for every model from [Artificial Analysis](https://artificialanalysis.ai/), with your own free key in `ARTIFICIAL_ANALYSIS_API_KEY`: a coding index and an intelligence index, shown next to the prices and given to the model that hands out tasks. They are kept in `~/.ironquill/rankings.json` and fetched again only when a month old, or when a model you picked is missing
- A budget per request, $0.10 unless `/budget` or `--budget` says otherwise: once it is spent the work stops, and the model says what it did and asks what to do next
- Your choices carry over: the model, the models offered, the team and the budget are kept in `~/.ironquill/config.toml` as soon as they change, and every new session starts with them
- Tasks handed to [Claude Code](https://code.claude.com/docs/en/overview) or [Codex](https://github.com/openai/codex) as sub-agents, through the `claude` or `codex` command installed and signed in on the machine: pick a `claude-code/...` or `codex/...` model with Ctrl-E, or send one task with `/claude <task>` or `/codex <task>`. Each keeps its own session for the whole conversation, across model switches and restarts (`/claude-reset` and `/codex-reset` end them), and is told what was said without it since it last took part; what it does shows live, and ironquill still runs the checks on what it changed. Claude Code may not run commands; Codex needs them to read and edit, so it runs them in its own sandbox, writing inside the project only and without network
- Cost, tokens and how full the context is, per request and for the whole conversation
- The context in your hands: `/context` opens what the next request will send in the Vim editor, to delete passages, shorten tool results or add notes; `:w` applies it. Long replies fold, and Ctrl-Z gives the conversation the whole screen
- `/copy` opens the conversation as text in the editor, to select with `v` or `V` and copy with `y` to the system clipboard; Shift and the mouse select on screen in most terminals
- Conversations saved locally and resumed with `ironquill -c` or `/resume`
- A file tree, an open file coloured by language and editable with Vim keys (visual mode, registers including the system clipboard, `:s`, search)
- A pane listing running Docker containers
- Any OpenAI compatible endpoint, Requesty by default

## What it does not do

- Ask before writing a file
- Stream answers as they are written
- Run on Windows, or on macOS beyond what CI builds and tests
- Sandbox the check commands: they run as you configured them

## Usage

```bash
export IRONQUILL_API_KEY=...        # your provider key
cd your-project
ironquill --model <cheap-model> --escalate <strong-model>
```

`/help` lists the commands, Ctrl-S every shortcut. `IRONQUILL_BASE_URL` points it at another OpenAI compatible endpoint. `IRONQUILL_MODELS` (comma separated) lists the models Ctrl-E offers; `claude-code/opus` and `claude-code/sonnet` are added when the `claude` command is installed.

## Building

```bash
cargo install --path .              # builds the ironquill binary into ~/.cargo/bin
scripts/check.sh                    # what CI runs: formatting, lints, docs, tests, dependency audit
git config core.hooksPath .githooks # run those checks before every commit
```

## License

MIT
