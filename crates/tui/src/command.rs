//! The `:` commands.

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
    /// `/resume` lists saved conversations to pick one.
    Resume,
    /// `/cost` shows what the conversation has cost so far.
    Cost,
    /// `/claude <task>` hands one task to Claude Code.
    Claude(Option<String>),
    /// `:help`
    Help,
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
        "resume" => Ok(Command::Resume),
        "cost" => Ok(Command::Cost),
        "claude" | "cc" => Ok(Command::Claude(rest_opt)),
        "help" | "h" => Ok(Command::Help),
        "" => Err("Empty command".into()),
        other => Err(format!("Unknown command /{other}, see /help")),
    }
}

pub(crate) const HELP: &str = "\
Type a question or a change and press Enter. Changes are checked before they are kept.
/model               pick the model that answers (Ctrl-E); /model <id> sets it
/claude <task>       hand one task to Claude Code, which works without this conversation
/escalate <id> ...   stronger models used only when the checks keep failing (empty: none)
/check <command>     add a check, run without a shell; /check alone lists them
/nocheck             remove every check
/rounds <n>          tries per model before handing over
/diff                what changed since the last commit
/clear               start a new conversation
/name <title>        name this conversation (it is saved after every request)
/resume              pick a saved conversation to continue (ironquill -c: the last one)
/cost                what this conversation has cost
/q                   quit
Files: Ctrl-B, or Esc then ,n, shows the file tree. Arrows move, → or Enter opens,
  ← closes a folder, q hides the pane. Files the agent changed are marked ●.
  The mouse works too: click a file, scroll with the wheel.
Open file: Vim keys. i a o insert, Esc stops, x dd yy p edit, u undo, Ctrl-R redo,
  v V visual (y d c > <), \"a named registers, \"+y \"+p system clipboard,
  :w save, :q close, :42 go to line, :s/a/b/g with % '<,'> or 2,5 ranges,
  /text search then n N, gg G, w b, 0 $.
Panes: Tab or Ctrl-W ← → switches between tree, file, chat and Docker; ,c closes the file.
Ctrl-G (or ,i): back to typing a message, from anywhere.
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
    fn refuses_nonsense() {
        assert!(parse("rounds 0").is_err());
        assert!(parse("rounds many").is_err());
        assert!(parse("frobnicate").is_err());
    }
}
