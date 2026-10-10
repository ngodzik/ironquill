//! The `:` commands.

use ironquill_core::{Agent, Effort};

/// A parsed `:` command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Command {
    /// `:q`, `:quit`
    Quit,
    /// `:model <id>` sets the first model; `:model` alone shows the chain.
    Model(Option<String>),
    /// `:escalate <id> ...` sets the models tried after the first; empty clears them.
    Escalate(Vec<String>),
    /// `:check <command>` adds a check; `:check` alone lists them.
    Check(Option<String>),
    /// `:nocheck` removes every check.
    NoCheck,
    /// `:rounds <n>`
    Rounds(Option<u32>),
    /// `:diff`
    Diff,
    /// `:clear`
    Clear,
    /// `/name <title>` names the conversation; `/name` alone shows the name.
    Name(Option<String>),
    /// `/resume` lists saved conversations to pick one; `/resume <id>`
    /// continues the one with that id, or whose id starts so.
    Resume(Option<String>),
    /// `/chats` lists the open conversations, to show or close one.
    Chats,
    /// `/newchat` opens an empty conversation beside the others.
    NewChat,
    /// `/task <what>` adds a task tied to this conversation; alone, lists
    /// them.
    Task(Option<String>),
    /// `/tasks` lists the project's tasks.
    Tasks,
    /// `/cost` shows what the conversation has cost so far.
    Cost,
    /// `/usage` shows or hides the usage pane; `/usage 6h` sets its window.
    Usage(Option<String>),
    /// `/secrets` lists the secrets commands may use; `/secrets forget
    /// <name>` takes one back.
    Secrets(Option<String>),
    /// `/strict [on|off]`: whether a program ironquill does not know asks.
    Strict(Option<String>),
    /// `/hosts` lists the servers allowed besides the known ones; `/hosts
    /// forget <host>` takes one back.
    Hosts(Option<String>),
    /// `/claude <task>` hands one task to Claude Code, `/codex <task>` to Codex.
    Delegate(Agent, Option<String>),
    /// `/context` opens the conversation's context in the editor.
    Context,
    /// `/budget <dollars>` sets the most one request may cost; `none`
    /// removes the limit, `/budget` alone shows it.
    Budget(Option<String>),
    /// `/defaults` keeps the current choices for every new session.
    Defaults,
    /// `/team` says who answers and who it may hand tasks to.
    Team,
    /// `/effort <level>` sets how hard models think; `/effort` alone shows
    /// it.
    Effort(Option<String>),
    /// `/pair <question>`: the best of the model that answers and its team
    /// plans, the cheapest codes.
    Pair(Option<String>),
    /// `/newpair <request>`: a pair whose planner starts from the chat
    /// rather than going on from the last pair.
    NewPair(Option<String>),
    /// `/tick`: keep the warm sessions warm while the conversation waits.
    Tick,
    /// `/compact`: sum the conversation up, choosing what to keep.
    Compact,
    /// `/address <pr>`: answer a pull request's review comments.
    Address(Option<String>),
    /// `/apply 1 3`: apply those diff blocks of the last reply.
    Apply(Option<String>),
    /// `/planner <model>` picks the member that plans; alone, says which.
    Planner(Option<String>),
    /// `/copy` opens the conversation as text, to select and copy from.
    Copy,
    /// `/instructions` opens the person's instructions for every model.
    Instructions,
    /// `/claude-reset` ends Claude Code's session, `/codex-reset` Codex's.
    Reset(Agent),
    /// `:help`
    Help,
    /// `/keys` lists every shortcut, as Ctrl-S does.
    Keys,
    /// `/review [base|off]`: read the branch's changes, nothing editable.
    Review(Option<String>),
    /// `/work [base|off]`: the branch's changes, to go on with.
    Work(Option<String>),
}

/// Parses the text typed after `:`. Errors are messages for the person.
pub(crate) fn parse(line: &str) -> Result<Command, String> {
    let line = line.trim();
    let (name, rest) = line
        .split_once(char::is_whitespace)
        .map_or((line, ""), |(n, r)| (n, r.trim()));
    let rest_opt = (!rest.is_empty()).then(|| rest.to_owned());

    match name {
        "q" | "quit" | "q!" => Ok(Command::Quit),
        "model" | "m" => Ok(Command::Model(rest_opt)),
        "escalate" | "esc" => Ok(Command::Escalate(
            rest.split_whitespace().map(str::to_owned).collect(),
        )),
        "check" => Ok(Command::Check(rest_opt)),
        "nocheck" => Ok(Command::NoCheck),
        "rounds" => match rest_opt {
            None => Ok(Command::Rounds(None)),
            Some(n) => n
                .parse()
                .ok()
                .filter(|n| *n > 0)
                .map(|n| Command::Rounds(Some(n)))
                .ok_or_else(|| format!("rounds must be a positive number, got {n:?}")),
        },
        "diff" => Ok(Command::Diff),
        "clear" | "new" => Ok(Command::Clear),
        "name" | "rename" => Ok(Command::Name(rest_opt)),
        "resume" => Ok(Command::Resume(rest_opt)),
        "chats" => Ok(Command::Chats),
        "newchat" => Ok(Command::NewChat),
        "task" => Ok(Command::Task(rest_opt)),
        "tasks" => Ok(Command::Tasks),
        "cost" => Ok(Command::Cost),
        "usage" => Ok(Command::Usage(rest_opt)),
        "secrets" => Ok(Command::Secrets(rest_opt)),
        "strict" => Ok(Command::Strict(rest_opt)),
        "hosts" => Ok(Command::Hosts(rest_opt)),
        "claude" | "cc" => Ok(Command::Delegate(Agent::ClaudeCode, rest_opt)),
        "codex" | "cx" => Ok(Command::Delegate(Agent::Codex, rest_opt)),
        "budget" => Ok(Command::Budget(rest_opt)),
        "defaults" => Ok(Command::Defaults),
        "team" => Ok(Command::Team),
        "effort" => Ok(Command::Effort(rest_opt)),
        "pair" => Ok(Command::Pair(rest_opt)),
        "newpair" => Ok(Command::NewPair(rest_opt)),
        "tick" => Ok(Command::Tick),
        "compact" => Ok(Command::Compact),
        "address" => Ok(Command::Address(rest_opt)),
        "apply" => Ok(Command::Apply(rest_opt)),
        "planner" => Ok(Command::Planner(rest_opt)),
        "copy" | "chat" => Ok(Command::Copy),
        "instructions" => Ok(Command::Instructions),
        "context" | "ctx" => Ok(Command::Context),
        "claude-reset" => Ok(Command::Reset(Agent::ClaudeCode)),
        "codex-reset" => Ok(Command::Reset(Agent::Codex)),
        "help" | "h" => Ok(Command::Help),
        "keys" | "shortcuts" => Ok(Command::Keys),
        "review" => Ok(Command::Review(rest_opt)),
        "work" => Ok(Command::Work(rest_opt)),
        "" => Err("Empty command".into()),
        other => Err(format!("Unknown command /{other}, see /help")),
    }
}

/// Every command name, as Tab completes them. A test checks that each one
/// parses, so that the list cannot drift from `parse`.
pub(crate) const NAMES: &[&str] = &[
    "budget",
    "chats",
    "check",
    "claude",
    "claude-reset",
    "clear",
    "codex",
    "codex-reset",
    "context",
    "copy",
    "cost",
    "usage",
    "secrets",
    "strict",
    "hosts",
    "defaults",
    "diff",
    "effort",
    "escalate",
    "help",
    "instructions",
    "keys",
    "model",
    "name",
    "new",
    "nocheck",
    "pair",
    "newpair",
    "newchat",
    "tick",
    "compact",
    "address",
    "apply",
    "planner",
    "q",
    "quit",
    "rename",
    "resume",
    "rounds",
    "task",
    "tasks",
    "team",
    "review",
    "work",
];

/// Commands of the Vim editor, completed on top of ironquill's inside a file.
pub(crate) const EDITOR_NAMES: &[&str] = &["e!", "q!", "w", "wq", "x"];

/// Where Tab completion stands while it cycles through several candidates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Completion {
    /// Every candidate for what was typed before the first Tab.
    pub(crate) matches: Vec<String>,
    next: usize,
}

/// Completes `line` among `candidates(line)`: the only candidate at once,
/// else the part all candidates share, then each candidate in turn on the
/// following Tabs. Returns the new line, or `None` when nothing matches.
pub(crate) fn complete(
    line: &str,
    state: &mut Option<Completion>,
    candidates: impl Fn(&str) -> Vec<String>,
) -> Option<String> {
    if let Some(current) = state.as_mut()
        && current.matches.iter().any(|m| m == line)
    {
        let pick = current.matches[current.next % current.matches.len()].clone();
        current.next += 1;
        return Some(pick);
    }
    let mut matches = candidates(line);
    matches.sort();
    matches.dedup();
    match matches.len() {
        0 => {
            *state = None;
            None
        }
        1 => {
            *state = None;
            matches.pop()
        }
        _ => {
            let shared = matches
                .iter()
                .skip(1)
                .fold(matches[0].clone(), |common, m| {
                    common
                        .chars()
                        .zip(m.chars())
                        .take_while(|(a, b)| a == b)
                        .map(|(a, _)| a)
                        .collect()
                });
            let first = if shared.len() > line.len() {
                shared
            } else {
                matches[0].clone()
            };
            let next = usize::from(first == matches[0]);
            *state = Some(Completion { matches, next });
            Some(first)
        }
    }
}

/// Candidates for a command line: command names while the first word is
/// being typed, then `extra` for the rest, such as model names after `model `.
pub(crate) fn candidates(line: &str, names: &[&str], models: &[String]) -> Vec<String> {
    match line.split_once(' ') {
        None => names
            .iter()
            .filter(|n| n.starts_with(line))
            .map(|n| (*n).to_owned())
            .collect(),
        Some(("model" | "m" | "escalate" | "planner", arg)) => {
            let command = &line[..line.len() - arg.len()];
            models
                .iter()
                .filter(|m| m.starts_with(arg))
                .map(|m| format!("{command}{m}"))
                .collect()
        }
        Some(("effort", arg)) => Effort::ALL
            .iter()
            .filter(|e| e.as_str().starts_with(arg))
            .map(|e| format!("effort {e}"))
            .collect(),
        Some(_) => Vec::new(),
    }
}

pub(crate) const HELP: &str = "\
Type a question or a change and press Enter. Changes are checked before they are kept.
/model               pick the model that answers (Ctrl-E); /model <id> sets it.
                     In the list, type to search every model of the provider,
                     Space puts the selected one in the team or takes it out,
                     Delete takes it off the list:
                     the model that answers may hand tasks to the team
/budget <dollars>    the most one request may cost (none: no limit); past it the
                     work stops and the model says where it is and asks what next
/team                who answers and who it may hand tasks to
/pair <request>      in a pair: the planner reads the code and plans, the coder
                     codes, the planner reviews; it goes on from the last pair
/newpair <request>   the same, the planner starting from a copy of the chat
/address <pr>        answer a pull request's review comments, one by one: what each
                     asks, whether it is handled, the change as a diff, a reply
/apply 1 3           apply those diff blocks of the last reply with git, all or none
/compact             sum the conversation up: pick the subjects to keep, the rest
                     goes; the agents start again from the summary
/tick                keep Claude Code's warm sessions warm while you think: a one
                     word read every four minutes, stopping after an hour without
                     a request; the machine is kept awake meanwhile
/planner <model>     the model that plans in a pair; by default the best scored,
                     or the dearest
/effort <level>      how hard models think: low, medium, high (the default),
                     xhigh, max; /effort alone shows it. ← → in Ctrl-E too
/defaults            keep the current model, list, team and budget for new
                     sessions; done by itself whenever they change
/claude <task>       hand one task to Claude Code, told what it missed of this conversation
/codex <task>        the same with Codex, which runs commands in its sandbox, without network
/instructions        your own instructions for every model, in every project, kept
                     in ~/.ironquill/instructions.md; :w saves, the next request
                     uses them
/copy                the conversation as text in the editor: v or V selects,
                     y copies to the clipboard, :q closes
/context             edit what the next request sends: delete, shorten, annotate; :w applies
/claude-reset        end Claude Code's session: its next request starts from nothing
/codex-reset         end Codex's session
/escalate <id> ...   stronger models for `ironquill do`, when the checks keep failing
/check <command>     a check offered to /pair's planner, run without a shell;
                     /check alone lists them. A request in the chat runs none
/nocheck             remove every check
/rounds <n>          tries per model before handing over
/review [base]       read the branch's changes: the tree shows only the files it
                     changed, each opens folded to its changes (zR: whole, zM: back),
                     nothing can be edited; against where it left main, or <base>.
                     /review off ends it
/work [base]         the same, to go on with the work: files stay editable
/diff                what changed since the last commit
/clear               start a new conversation in place of this one
/newchat             open a new conversation beside this one, which goes on
/chats               the open conversations: Enter shows one, n opens a new one,
                     x closes one not shown (its file stays); hidden ones go on
/task <what>         add a task, tied to this conversation
/tasks               the project's tasks: Space ticks one done, Enter opens its
                     conversation, t ties it to this one, d deletes it
/name <title>        name this conversation (it is saved after every request)
/resume [id]         continue a saved conversation: pick it, or give its id or
                     the start of it (ironquill -c: the last one, -r <id>)
/cost                what this conversation has cost
/usage [1h|6h|24h]   a pane of cost per model, context and cache rebuilds over time
/secrets             the secrets commands may use; /secrets forget <name>
/strict [on|off]     whether a program ironquill does not know asks first (on)
/hosts               servers allowed besides those already used; /hosts forget
                     (every command is written down in ~/.ironquill/audit.log)
/q                   quit
Files: Ctrl-B, or Esc then ,n, shows the file tree. Arrows move, → or Enter opens,
  ← closes a folder, q hides the pane. Files the agent changed are marked ●.
  The mouse works too: click a file, scroll with the wheel.
Open file: Vim keys. i a o insert, Esc stops, x dd yy p edit, u undo, Ctrl-R redo,
  v V visual (y d c > <), \"a named registers, \"+y \"+p system clipboard,
  :w save, :q close, :42 go to line, :s/a/b/g with % '<,'> or 2,5 ranges,
  /text search then n N, gg G, w b, 0 $.
Panes: Tab or Ctrl-W ← → switches between tree, file, chat and Docker; ,c closes the file.
Ctrl-Q (or ,i): back to typing a message, from anywhere. Ctrl-S (or /keys, or ? in normal mode): every shortcut.
Ctrl-K (or ,d): show or hide the running Docker containers.
Ctrl-E (or ,m): pick the model; the one in use shows at the bottom right.
Vim: Esc for normal mode, i to type, : for commands. Ctrl-C stops a request; Ctrl-C twice quits";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_arguments() {
        assert_eq!(
            parse("model deepseek/deepseek-chat"),
            Ok(Command::Model(Some("deepseek/deepseek-chat".into())))
        );
        assert_eq!(parse("model"), Ok(Command::Model(None)));
        assert_eq!(
            parse("escalate a b"),
            Ok(Command::Escalate(vec!["a".into(), "b".into()]))
        );
        assert_eq!(
            parse("check cargo test -q"),
            Ok(Command::Check(Some("cargo test -q".into())))
        );
        assert_eq!(parse("rounds 3"), Ok(Command::Rounds(Some(3))));
        assert_eq!(
            parse("name fix the parser"),
            Ok(Command::Name(Some("fix the parser".into())))
        );
    }

    #[test]
    fn effort_levels_complete() {
        assert_eq!(candidates("effort h", NAMES, &[]), ["effort high"]);
        assert_eq!(candidates("effort ", NAMES, &[]).len(), 5);
    }

    #[test]
    fn every_completed_name_parses() {
        for name in NAMES {
            assert!(
                parse(name).is_ok(),
                "{name} is completed but does not parse"
            );
        }
    }

    #[test]
    fn tab_completes_one_match_then_the_shared_part_then_cycles() {
        let names = |l: &str| candidates(l, NAMES, &[]);
        let mut state = None;
        assert_eq!(
            complete("cont", &mut state, names).as_deref(),
            Some("context")
        );

        // Several matches sharing nothing more than what was typed: each
        // Tab gives the next one.
        let mut state = None;
        assert_eq!(complete("cl", &mut state, names).as_deref(), Some("claude"));
        assert!(state.as_ref().is_some_and(|s| s.matches.len() == 3));
        assert_eq!(
            complete("claude", &mut state, names).as_deref(),
            Some("claude-reset")
        );
        assert_eq!(
            complete("claude-reset", &mut state, names).as_deref(),
            Some("clear")
        );

        // Several matches sharing more than was typed: the shared part first.
        let mut state = None;
        assert_eq!(complete("na", &mut state, names).as_deref(), Some("name"));
        let mut state = None;
        assert_eq!(
            complete("no", &mut state, names).as_deref(),
            Some("nocheck")
        );

        assert_eq!(complete("zzz", &mut None, names), None);
    }

    #[test]
    fn model_names_complete_after_model() {
        let models = vec![
            "claude-code/opus".to_owned(),
            "deepseek/deepseek-chat".to_owned(),
        ];
        let mut state = None;
        let got = complete("model dee", &mut state, |l| candidates(l, NAMES, &models));
        assert_eq!(got.as_deref(), Some("model deepseek/deepseek-chat"));
    }

    #[test]
    fn refuses_nonsense() {
        assert!(parse("rounds 0").is_err());
        assert!(parse("rounds many").is_err());
        assert!(parse("frobnicate").is_err());
    }
}
