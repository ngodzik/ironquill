# universe

[![CI](https://github.com/ngodzik/universe/actions/workflows/ci.yml/badge.svg)](https://github.com/ngodzik/universe/actions/workflows/ci.yml) [![Security](https://github.com/ngodzik/universe/actions/workflows/security.yml/badge.svg)](https://github.com/ngodzik/universe/actions/workflows/security.yml)

**An experimental coding agent. It is not stable, it comes with no guarantee of any kind, and it changes all the time.**

## Read this first

This is a personal experiment, not a product.

- **It is not stable.** Commands, keys, views, settings and file formats change from one commit to the next, without notice and without a way back. Features appear, get rewritten or disappear.
- **It is barely tested in real use.** There are tests and CI, but few real sessions, on Linux mostly. macOS is only built and tested by CI; Windows is not supported.
- **It changes your files without asking.** Work in a git repository, commit before you start, and read every diff.
- **It spends money.** Requests go to a paid API, within a budget you set.

**If you want a coding agent to rely on, use [OpenCode](https://opencode.ai), [Pi](https://pi.dev) or [Claude Code](https://code.claude.com/docs/en/overview) instead.** This one overlaps with them on purpose: writing one is how I learn what makes them work, and where I try ideas of my own.

## The idea

Deterministic tools before models: compilers, tests, linters, language servers and git do what they can, and a model is called for the rest. Every request shows what it cost. The keys follow Vim's habits.

## What is there today

All of it experimental, all of it likely to change.

- **A conversation with a model**, in the terminal or in a window (`--gui`), its changes shown as diffs. Any OpenAI compatible endpoint works, Requesty by default; Claude Code and Codex can work as sub-agents.
- **A safety net for commands**: what cannot be undone, leaves the machine or cannot be read waits for your approval, secrets are never shown to a model, and every command is logged. A net, not a sandbox.
- **Costs and context in view**, per request and per conversation, with a budget per request.
- **Views of the codebase** in the window (Ctrl-N), read from the code with no model: its design in layers, its API, its services, its database tables, and its files as a universe of stars.
- **Where it runs**: the deployment read from Compose, Helm, Kustomize, Flux and Terraform, each environment rendered by `kustomize` and `helm`, every environment shown at once as galaxies, and each service laid out flat with its objects and its values layer by layer. Secrets are never read or shown.
- **Reviewing and walking a branch**: `/review` shows what a branch changed, `/walk` what is not committed yet, with questions about it sent to the cheapest model.

`/help` lists the commands and Ctrl-S every shortcut. They follow the code; this page does not always.

## Usage

```bash
export REQUESTY_API_KEY=...         # without one, only Claude Code and Codex answer
cd your-project
universe                           # in the terminal
universe --gui                     # in a window
```

Settings live in `~/.universe/config.toml`, and a project's own (linked repositories, environments) in `~/.universe/projects/<folder>.toml`.

With `workspaces = ["~/code/app", ...]` in the settings, a new conversation asks which folder it works in and starts again there (`--here` skips the question). `allowed_programs = [...]` lists a project's own tools, which strict mode then lets run without asking.

## Building

On Linux the window needs Wayland's client library: `sudo apt install libwayland-dev` on Debian and Ubuntu.

```bash
cargo install --path .              # builds the universe binary into ~/.cargo/bin
scripts/check.sh                    # what CI runs: formatting, lints, docs, tests, dependency audit
git config core.hooksPath .githooks # run those checks before every commit
```

Optional language servers: `npm install -g pyright typescript@5 typescript-language-server`, and `rustup component add rust-analyzer`.

## Data from others

None of this data ships with the program: each install fetches it for its own use and says where it comes from.

- **Model prices** for Claude Code's and Codex's calls come from [LiteLLM](https://github.com/BerriAI/litellm)'s public list, [`model_prices_and_context_window.json`](https://github.com/BerriAI/litellm/blob/main/model_prices_and_context_window.json), under the MIT licence. They are list prices: an estimate, not a bill.
- **Model scores** come from [Artificial Analysis](https://artificialanalysis.ai/)' free API, with your own key in `ARTIFICIAL_ANALYSIS_API_KEY`, under their terms, which ask that they be credited; the model picker does so wherever scores show.
- **Model lists, prices and context windows** for the provider's models come from the provider itself.

## License

MIT
