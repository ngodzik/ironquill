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

- A conversation in the terminal: questions get answers, requests for changes get edits shown as diffs, then the checks: the project's own, found each time they run, so that tests a model has just written are run too (`pytest` or `unittest` when there are Python tests, run with the project's own Python: `uv run` when uv manages it, else its virtual environment, else the Python `pytest` belongs to, and in each directory that is a Python project of its own, such as a `backend`; `npm test`, `cargo check` and `cargo test`), or any command given with `/check`. When the checks fail and nothing was edited since, they are not run again: they would fail the same way
- Escalation from a cheap model to stronger ones when the checks keep failing
- A team: the model that answers may hand a task to another model you picked, a stronger one for a hard change, a cheaper one for a long read, and gets its report back. Ctrl-E searches every model of the provider, with its prices and context; Space puts one in the team
- Scores for every model from [Artificial Analysis](https://artificialanalysis.ai/), with your own free key in `ARTIFICIAL_ANALYSIS_API_KEY`: a coding index and an intelligence index, shown next to the prices and given to the model that hands out tasks. They are kept in `~/.ironquill/rankings.json` and fetched again only when a month old, or when a model you picked is missing
- While a model of the team works on a task handed to it, a sub-agent pane takes most of the screen with its name, the task and everything it does, live; the conversation keeps a third and one line per handover. Ctrl-T hides or shows it
- An effort for models that reason, high unless `/effort`, `--effort` or ← → in the model picker say otherwise: low, medium, high, xhigh or max, shown in the status line. It goes to Requesty as `reasoning_effort` (only for models its list says can reason), to Claude Code as `--effort` and to Codex as `model_reasoning_effort`
- Working in pairs, `/pair <question>`, as an architect and an editor, among the model that answers and its team only. The best, unless `/planner` picks one: Claude Code or Codex first, then the best scored, then the dearest; when no price is known, ironquill asks rather than guesses. The planner sees a map of the project built by a parser and says which lines it needs; ironquill reads them, no model does; it plans from them at a high effort. The cheapest that can use tools implements the plan at a low effort, without the earlier conversation, and the checks judge; when they keep failing, the planner revises its plan; when the coder changes nothing, the pair ends there. The planner may answer `STOP` instead of a plan, when nothing needs to change or no change to the code can help, such as checks failing for a reason outside it. Claude Code or Codex plan in one session, read only, that follows the conversation. Once they pass, the planner reviews the diff against the request and its plan, so that nobody has to review it after, and the coder fixes what it finds, for two rounds at most. Each step is announced with the model doing it, each message says what the request has cost so far, and the conversation keeps a short account of the work
- Instructions, given to every model with each request, read again each time: your own for every project in `~/.ironquill/instructions.md` (`/instructions` opens it in the editor), and those a project already has for coding agents, `CLAUDE.md`, `.claude/CLAUDE.md`, `CLAUDE.local.md`, `AGENTS.md` and `.claude/rules/`, with their `@path` imports and the files a rule is scoped to. ironquill reads them and never writes any
- A budget per request, $0.10 unless `/budget` or `--budget` says otherwise: once it is spent the work stops, and the model says what it did and asks what to do next
- Your choices carry over: the model, the models offered, the team and the budget are kept in `~/.ironquill/config.toml` as soon as they change, and every new session starts with them
- Tasks handed to [Claude Code](https://code.claude.com/docs/en/overview) or [Codex](https://github.com/openai/codex) as sub-agents, through the `claude` or `codex` command installed and signed in on the machine: pick a `claude-code/...` or `codex/...` model with Ctrl-E, or send one task with `/claude <task>` or `/codex <task>`. Each keeps its own session for the whole conversation, across model switches and restarts (`/claude-reset` and `/codex-reset` end them), and is told what was said without it since it last took part; a session unused for five minutes, whose prompt cache has expired, is not resumed, as resending all of it would cost more than a new one told the conversation. A new session, or a `/pair` planner, is told it from a summary and what was said since: once enough has been said, the cheapest model of the team with a known price updates the summary at the end of a request, at a low effort, its cost counted with the request and within the budget; what it does shows live, and ironquill still runs the checks on what it changed. Claude Code may not run commands; Codex needs them to read and edit, so it runs them in its own sandbox, writing inside the project only and without network
- Reading little: models find code with `search` (as ripgrep, skipping what git ignores) and `outline` (the functions, classes and types of a file or directory, parsed with tree-sitter for Rust, Python, TypeScript and JavaScript), then read a range of lines rather than whole files
- Compaction: past 40k tokens, or half the model's context, the results of old tool calls are dropped all at once, down to half that, so the conversation is resent shorter and the provider's cache can take over again
- Claude Code's and Codex's cost as they work: each message Claude Code ends, and each call Codex logs in its session file (`~/.codex/sessions`, as ccusage reads it), is priced with [LiteLLM](https://github.com/BerriAI/litellm)'s public price list (see [Data from others](#data-from-others)), as ccusage does, kept a week in `~/.ironquill/prices.json`; on an API key the cost is owed, shows live and counts against the budget; Claude Code's own total, which covers its whole session, fills in only for a new session whose messages could not all be priced; on a subscription it shows as such. Whether Codex is billed comes from `codex login status`; its credentials are never read
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

`/help` lists the commands, Ctrl-S every shortcut. Up and Down in the message box go through the messages sent before, as in a shell. `IRONQUILL_BASE_URL` points it at another OpenAI compatible endpoint. `IRONQUILL_MODELS` (comma separated) lists the models Ctrl-E offers; `claude-code/opus` and `claude-code/sonnet` are added when the `claude` command is installed.

## Building

```bash
cargo install --path .              # builds the ironquill binary into ~/.cargo/bin
scripts/check.sh                    # what CI runs: formatting, lints, docs, tests, dependency audit
git config core.hooksPath .githooks # run those checks before every commit
```

## Data from others

ironquill ships none of the data below. Each install fetches it for its own use, keeps a copy on the machine, and says where it comes from.

- **Model prices** for Claude Code's and Codex's calls come from [LiteLLM](https://github.com/BerriAI/litellm)'s public list, [`model_prices_and_context_window.json`](https://github.com/BerriAI/litellm/blob/main/model_prices_and_context_window.json), under the MIT licence. They are list prices: an estimate of what a message costs, not a bill. [ccusage](https://github.com/ccusage/ccusage) prices Claude Code's usage from the same list.
- **Model scores** come from [Artificial Analysis](https://artificialanalysis.ai/)' free API, with your own key, under their terms, which ask that they be credited; the model picker does so wherever scores show.
- **Model lists, prices, context windows and descriptions** for the provider's models come from the provider itself, such as Requesty's `/v1/models`.

## License

MIT
