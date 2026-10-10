# ironquill

[![CI](https://github.com/ngodzik/ironquill/actions/workflows/ci.yml/badge.svg)](https://github.com/ngodzik/ironquill/actions/workflows/ci.yml) [![Security](https://github.com/ngodzik/ironquill/actions/workflows/security.yml/badge.svg)](https://github.com/ngodzik/ironquill/actions/workflows/security.yml)

**An experimental coding agent, and a work in progress. It is not stable, it comes with no guarantee of any kind, and it will keep changing a lot.**

## Read this first

ironquill is a personal experiment, not a product. It is shaped by my own way of working and changes whenever that does.

- **It is not stable.** Commands, keys, views, file formats and settings change from one commit to the next, often without notice and without any way back. Whole features appear, get rewritten or disappear. Nothing here is a promise.
- **It is barely tested in real use.** There are unit tests and CI, but few real sessions, on Linux mostly. On macOS it is only built and tested by CI; Windows is not supported.
- **It changes your files without asking.** Edits are written to disk as the model makes them. Run it in a git repository, commit before you start, and read every diff.
- **It spends money.** Requests go to a paid API, within a budget per request you set.

**If you want a coding agent to rely on, use one of these instead:** [OpenCode](https://opencode.ai), [Pi](https://pi.dev) or [Claude Code](https://code.claude.com/docs/en/overview). They are mature, well tested and supported. ironquill overlaps with them on purpose: writing one is how I learn what makes them work, and where I try ideas of my own.

## The idea

Deterministic tools before models. Compilers, tests, linters, language servers and git are cheap, fast and right, so they do what they can, and a model is called for the rest. Every request shows what it cost.

The interface follows my editor habits: Vim-like modes and keys, a leader key, files opened and edited beside the conversation.

## What is there today

All of it experimental, and all of it likely to change.

- **A conversation with a model**, in the terminal or in a window (`--gui`, drawn on the GPU with Bevy and egui). Requests for changes become edits shown as diffs. Any OpenAI compatible endpoint works, Requesty by default.
- **A safety net for commands.** What cannot be undone, leaves the machine or cannot be read is held for you to approve, secrets are never shown to a model, and every command is written to an audit log. It reads commands as written: a net, not a sandbox.
- **Teams of models.** A cheap model can hand tasks to stronger or cheaper ones, work in a pair with a planner that reviews, escalate when checks keep failing, or hand a task to Claude Code or Codex as sub-agents.
- **Costs and context in view.** Cost, tokens and context per request and per conversation, a budget per request, a usage pane, and the conversation's context editable before it is sent or compacted by subject.
- **Several conversations in one window.** `/newchat` opens one beside the others and `/chats` switches between them; hidden ones go on working, ticks included, and those open are reopened at the next start. `/task` and `/tasks` keep a list of things to do per project, each tied to a conversation.
- **Warm caches.** With `/tick`, while a Claude Code session is warm, a one-word tick every four minutes keeps its prompt cache from expiring, for up to an hour without a request, and the usage pane shows what the ticks cost against what they saved. Meanwhile the machine is kept from idle sleep (`caffeinate` on macOS, `systemd-inhibit` on Linux); `keep_awake = false` in `~/.ironquill/config.toml` lets it sleep.
- **Views of the codebase** (in the window, Ctrl-N), read from the code by patterns, no model involved: its design as a plan in layers, its API from the OpenAPI specs with what serves and calls each route, and its files as a universe in 3D, grouped into galaxies. Ctrl-F searches them.
- **Reviewing a branch.** `/review` and `/work` show only what a branch changed: the files it touched, each folded to its changes, the routes, migrations and models it changed, marked in every view.
- **Code navigation.** Go to a definition or list the uses of a name, through the language servers when they are installed (pyright, typescript-language-server, rust-analyzer), else through `git grep`.

## What it does not do

- Ask before writing a file, or before a command it does not hold
- Stream answers as they are written
- Run on Windows
- Sandbox anything: commands and checks run as you would run them

## Usage

```bash
export IRONQUILL_API_KEY=...        # your provider key; without one, only Claude Code and Codex answer
cd your-project
ironquill                           # in the terminal
ironquill --gui                     # in a window
```

`/help` lists the commands and Ctrl-S every shortcut, which are the only reliable reference: they follow the code, this page does not.

## Building

On Linux the window needs Wayland's client library: `sudo apt install libwayland-dev` on Debian and Ubuntu.

```bash
cargo install --path .              # builds the ironquill binary into ~/.cargo/bin
scripts/check.sh                    # what CI runs: formatting, lints, docs, tests, dependency audit
git config core.hooksPath .githooks # run those checks before every commit
```

The language servers are optional: `npm install -g pyright typescript@5 typescript-language-server`, and `rustup component add rust-analyzer`.

## Data from others

ironquill ships none of the data below. Each install fetches it for its own use, keeps a copy on the machine, and says where it comes from.

- **Model prices** for Claude Code's and Codex's calls come from [LiteLLM](https://github.com/BerriAI/litellm)'s public list, [`model_prices_and_context_window.json`](https://github.com/BerriAI/litellm/blob/main/model_prices_and_context_window.json), under the MIT licence. They are list prices: an estimate, not a bill.
- **Model scores** come from [Artificial Analysis](https://artificialanalysis.ai/)' free API, with your own key in `ARTIFICIAL_ANALYSIS_API_KEY`, under their terms, which ask that they be credited; the model picker does so wherever scores show.
- **Model lists, prices and context windows** for the provider's models come from the provider itself.

## License

MIT
