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
    /// Where it runs, relative to the project: empty for the project itself.
    dir: String,
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
            dir: String::new(),
        })
    }

    /// A check that runs `program` with `args`, which may hold spaces.
    pub(crate) fn new(program: String, args: Vec<String>) -> Self {
        Self {
            program,
            args,
            dir: String::new(),
        }
    }

    /// The same check, run in `dir`, a directory of the project such as the
    /// `backend` of a repository that holds more than one.
    #[must_use]
    pub fn in_dir(self, dir: impl Into<String>) -> Self {
        Self {
            dir: dir.into(),
            ..self
        }
    }

    /// The program the check runs.
    pub fn program(&self) -> &str {
        &self.program
    }

    /// The check as a planner names it: `Check: <command>`, or
    /// `Check in <dir>: <command>`.
    pub fn line(&self) -> String {
        let command = Self {
            dir: String::new(),
            ..self.clone()
        }
        .command();
        if self.dir.is_empty() {
            format!("Check: {command}")
        } else {
            format!("Check in {}: {command}", self.dir)
        }
    }

    /// The command line, for display, with the directory it runs in when
    /// that is not the project's.
    pub fn command(&self) -> String {
        let line = std::iter::once(self.program.as_str())
            .chain(self.args.iter().map(String::as_str))
            .collect::<Vec<_>>()
            .join(" ");
        if self.dir.is_empty() {
            line
        } else {
            format!("{line} (in {}/)", self.dir)
        }
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
            .current_dir(dir.join(&self.dir))
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

/// How a check went when tried before any change: whether it can judge one,
/// and how the project stood.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Trial {
    /// It passes already.
    Passed,
    /// It fails already, before any change: the environment, most likely,
    /// or what the request is about.
    Failed(CheckFailure),
    /// It cannot judge a change: why, for the planner.
    Unusable(String),
}

impl Check {
    /// Runs the check once, before any change, to see whether it can judge
    /// one: it must start, in a directory that exists, and find tests to run.
    pub async fn try_out(&self, root: &Path) -> Trial {
        let dir = root.join(&self.dir);
        if !dir.is_dir() {
            return Trial::Unusable(format!("there is no directory {}", self.dir));
        }
        let output = match Command::new(&self.program)
            .args(&self.args)
            .current_dir(&dir)
            .output()
            .await
        {
            Ok(output) => output,
            Err(e) => return Trial::Unusable(format!("it cannot start: {e}")),
        };
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stderr),
            String::from_utf8_lossy(&output.stdout)
        );
        if finds_no_tests(output.status.code(), &plain(&text)) {
            return Trial::Unusable("it finds no tests to run".into());
        }
        if output.status.success() {
            Trial::Passed
        } else {
            Trial::Failed(CheckFailure {
                command: self.command(),
                excerpt: excerpt(&text),
            })
        }
    }
}

/// Whether a test runner's output says it found nothing to run, whatever
/// its exit status: such a check would judge nothing.
fn finds_no_tests(code: Option<i32>, text: &str) -> bool {
    let lower = text.to_lowercase();
    // pytest's own status for "no tests collected".
    if code == Some(5) && lower.contains("no tests ran") {
        return true;
    }
    const NOTHING: [&str; 5] = [
        "ran 0 tests",
        "no tests found",
        "no test files found",
        "[no test files]",
        "no test specified",
    ];
    if NOTHING.iter().any(|n| lower.contains(n)) {
        return true;
    }
    // cargo test: every test binary ran none.
    let counts: Vec<&str> = lower
        .lines()
        .filter_map(|l| l.trim().strip_prefix("running "))
        .filter_map(|rest| rest.strip_suffix(" tests").or(rest.strip_suffix(" test")))
        .collect();
    !counts.is_empty() && counts.iter().all(|n| *n == "0")
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
    async fn a_check_may_run_in_a_directory_of_the_project() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("backend")).unwrap();
        std::fs::write(dir.path().join("backend/here"), "").unwrap();
        let check = Check::parse("test -f here").unwrap().in_dir("backend");
        assert_eq!(check.command(), "test -f here (in backend/)");
        let report = Check::run_all(&[check], dir.path()).await.unwrap();
        assert_eq!(report, CheckReport::Passed);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_check_is_tried_before_it_judges() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let trial = |line: &str| Check::parse(line).unwrap();
        assert_eq!(trial("true").try_out(root).await, Trial::Passed);
        assert!(matches!(
            trial("false").try_out(root).await,
            Trial::Failed(_)
        ));
        assert!(matches!(
            trial("no-such-program-here").try_out(root).await,
            Trial::Unusable(why) if why.starts_with("it cannot start")
        ));
        assert!(matches!(
            trial("true").in_dir("backend").try_out(root).await,
            Trial::Unusable(why) if why == "there is no directory backend"
        ));
        assert!(matches!(
            trial("echo Ran 0 tests in 0.000s").try_out(root).await,
            Trial::Unusable(why) if why == "it finds no tests to run"
        ));
    }

    #[test]
    fn runners_that_found_nothing_are_recognised() {
        assert!(finds_no_tests(
            Some(5),
            "collected 0 items\n\nno tests ran in 0.01s"
        ));
        assert!(finds_no_tests(
            Some(0),
            "running 0 tests\n\ntest result: ok. 0 passed"
        ));
        assert!(!finds_no_tests(
            Some(0),
            "running 0 tests\nrunning 3 tests\ntest result: ok. 3 passed"
        ));
        assert!(!finds_no_tests(Some(1), "FAILED tests/test_a.py::test_x"));
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
