use std::path::Path;

use tokio::process::Command;

use crate::error::ToolError;

/// How many lines of a failing check's output reach the model. Compiler and
/// test output is mostly noise around a few useful lines, and every line sent
/// is paid for on every later turn.
const EXCERPT_LINES: usize = 60;

/// One command that judges a change: it passes when it exits with status 0.
///
/// The command is run directly, never through a shell, so what runs is
/// exactly what was configured.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    program: String,
    args: Vec<String>,
}

/// A check that did not pass, with the part of its output worth reading.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckFailure {
    /// The command line, for display.
    pub command: String,
    /// The output, stripped of progress lines and cut to a bounded length.
    pub excerpt: String,
}

/// The verdict of a list of checks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckReport {
    /// Every check passed.
    Passed,
    /// The first check that failed. Later checks were not run: a test suite
    /// has nothing to say about code that does not compile.
    Failed(CheckFailure),
}

impl Check {
    /// Parses a command line split on whitespace, such as `cargo test -q`.
    ///
    /// Returns `None` for a blank line. Quoting is not supported, on purpose:
    /// a check that needs a shell belongs in a script the check then calls.
    pub fn parse(line: &str) -> Option<Self> {
        let mut words = line.split_whitespace().map(str::to_owned);
        let program = words.next()?;
        Some(Self {
            program,
            args: words.collect(),
        })
    }

    /// The program the check runs.
    pub fn program(&self) -> &str {
        &self.program
    }

    /// The command line, for display.
    pub fn command(&self) -> String {
        std::iter::once(self.program.as_str())
            .chain(self.args.iter().map(String::as_str))
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Runs every check in `checks`, in order, in `dir`, stopping at the first
    /// failure.
    ///
    /// # Errors
    ///
    /// [`ToolError::Spawn`] when a command cannot be started at all, which is
    /// a configuration problem rather than a verdict on the change.
    pub async fn run_all(checks: &[Self], dir: &Path) -> Result<CheckReport, ToolError> {
        for check in checks {
            if let Some(failure) = check.run(dir).await? {
                return Ok(CheckReport::Failed(failure));
            }
        }
        Ok(CheckReport::Passed)
    }

    async fn run(&self, dir: &Path) -> Result<Option<CheckFailure>, ToolError> {
        let output = Command::new(&self.program)
            .args(&self.args)
            .current_dir(dir)
            .output()
            .await
            .map_err(|source| ToolError::Spawn {
                command: self.command(),
                source,
            })?;
        if output.status.success() {
            return Ok(None);
        }
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stderr),
            String::from_utf8_lossy(&output.stdout)
        );
        Ok(Some(CheckFailure {
            command: self.command(),
            excerpt: excerpt(&text),
        }))
    }
}

/// `text` without the escape sequences that colour terminal output.
fn plain(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            // Parameters, then the letter that ends the sequence.
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Keeps the lines a reader would look at and drops the progress chatter.
fn excerpt(text: &str) -> String {
    // Colours are for a terminal; to a model they are tokens of noise.
    let text = &plain(text);
    const NOISE: [&str; 7] = [
        "Compiling ",
        "Checking ",
        "Finished ",
        "Running ",
        "Doc-tests ",
        "Downloaded ",
        "Blocking ",
    ];
    let useful: Vec<&str> = text
        .lines()
        .filter(|line| {
            let trimmed = line.trim_start();
            !trimmed.is_empty() && !NOISE.iter().any(|n| trimmed.starts_with(n))
        })
        .collect();
    let mut out = useful
        .iter()
        .take(EXCERPT_LINES)
        .copied()
        .collect::<Vec<_>>()
        .join("\n");
    if useful.len() > EXCERPT_LINES {
        out.push_str(&format!(
            "\n[{} more lines cut]",
            useful.len() - EXCERPT_LINES
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_splits_on_whitespace() {
        let check = Check::parse("  cargo   test -q ").unwrap();
        assert_eq!(check.command(), "cargo test -q");
        assert_eq!(Check::parse("   "), None);
    }

    #[test]
    fn colours_are_dropped() {
        assert_eq!(
            plain("\u{1b}[31mFAIL\u{1b}[0m: \u{1b}[1;31mtest_add\u{1b}[0m"),
            "FAIL: test_add"
        );
    }

    #[test]
    fn excerpt_drops_progress_and_bounds_length() {
        let mut text = String::from(
            "   Compiling foo v0.1.0\n    Checking bar\nerror[E0425]: cannot find value `x`\n",
        );
        for i in 0..100 {
            text.push_str(&format!("line {i}\n"));
        }
        let out = excerpt(&text);
        assert!(out.starts_with("error[E0425]"));
        assert!(!out.contains("Compiling"));
        assert!(out.ends_with("[41 more lines cut]"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn stops_at_the_first_failure() {
        let dir = tempfile::tempdir().unwrap();
        let checks = [
            Check::parse("true").unwrap(),
            Check::parse("false").unwrap(),
            Check::parse("this-program-does-not-exist").unwrap(),
        ];
        // The third would be a spawn error; reaching it would mean the second
        // failure did not stop the run.
        let report = Check::run_all(&checks, dir.path()).await.unwrap();
        assert!(matches!(report, CheckReport::Failed(f) if f.command == "false"));
    }
}
