//! Finding how to test a project when nobody said, from its files: what a
//! person would run, for the languages ironquill knows.

use std::fs;
use std::path::Path;

use ignore::WalkBuilder;

use crate::check::Check;

/// The checks a project's files call for, looked for each time they are
/// needed, so that tests a model has just written are run too. Only
/// commands found on the machine are returned: a missing one would stop
/// the request rather than judge it.
pub fn detect_checks(root: &Path) -> Vec<Check> {
    let mut lines: Vec<String> = Vec::new();
    if root.join("Cargo.toml").is_file() {
        lines.push("cargo check --all-targets".into());
        lines.push("cargo test".into());
    }
    if let Some(line) = python(root) {
        lines.push(line);
    }
    if npm_has_tests(root) {
        lines.push("npm test --silent".into());
    }
    lines
        .iter()
        .filter_map(|line| Check::parse(line))
        .filter(|check| on_path(check.program()))
        .collect()
}

/// pytest when the project uses it, else unittest when it has test files.
fn python(root: &Path) -> Option<String> {
    let files = project_files(root);
    let is_test = |name: &str| {
        name.ends_with(".py") && (name.starts_with("test_") || name.ends_with("_test.py"))
    };
    let tests: Vec<&String> = files
        .iter()
        .filter(|p| is_test(p.rsplit('/').next().unwrap_or(p)))
        .collect();
    let mentions = |file: &str, needle: &str| {
        fs::read_to_string(root.join(file)).is_ok_and(|text| text.contains(needle))
    };
    let pytest = files.iter().any(|p| p.ends_with("conftest.py"))
        || root.join("pytest.ini").is_file()
        || mentions("pyproject.toml", "[tool.pytest")
        || mentions("setup.cfg", "[tool:pytest]")
        || mentions("tox.ini", "[pytest]");
    // -B: without bytecode files, a change written in the same second as
    // the last run, at the same size, is not hidden by a stale one.
    if pytest && on_path("pytest") {
        return Some("python3 -B -m pytest -q".into());
    }
    if tests.is_empty() {
        return None;
    }
    // From the root, so that the tests import the project as it runs.
    if root.join("tests").is_dir() && tests.iter().all(|p| p.starts_with("tests/")) {
        Some("python3 -B -m unittest discover -s tests".into())
    } else {
        Some("python3 -B -m unittest discover".into())
    }
}

/// Whether package.json has a test script other than npm's placeholder.
fn npm_has_tests(root: &Path) -> bool {
    let Ok(text) = fs::read_to_string(root.join("package.json")) else {
        return false;
    };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) else {
        return false;
    };
    json["scripts"]["test"]
        .as_str()
        .is_some_and(|script| !script.contains("no test specified"))
}

/// The project's files, relative to its root, skipping what git ignores.
fn project_files(root: &Path) -> Vec<String> {
    WalkBuilder::new(root)
        .hidden(true)
        .git_ignore(true)
        .require_git(false)
        .build()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_some_and(|t| t.is_file()))
        .filter_map(|e| {
            e.path()
                .strip_prefix(root)
                .ok()
                .map(|p| p.to_string_lossy().replace('\\', "/"))
        })
        .collect()
}

/// Whether `program` can be run from the PATH.
fn on_path(program: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join(program).is_file()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn commands(root: &Path) -> Vec<String> {
        detect_checks(root).iter().map(Check::command).collect()
    }

    #[test]
    fn python_tests_are_found_when_they_appear() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("app")).unwrap();
        fs::write(root.join("app/store.py"), "x = 1\n").unwrap();
        // No test yet: nothing to run.
        assert!(commands(root).iter().all(|c| !c.contains("unittest")));

        // A model writes some: they are run from then on.
        fs::create_dir_all(root.join("tests")).unwrap();
        fs::write(root.join("tests/test_store.py"), "import unittest\n").unwrap();
        if on_path("python3") {
            assert!(
                commands(root).contains(&"python3 -B -m unittest discover -s tests".to_owned())
            );
        }

        fs::write(root.join("app/store_test.py"), "import unittest\n").unwrap();
        if on_path("python3") {
            assert!(commands(root).contains(&"python3 -B -m unittest discover".to_owned()));
        }
    }

    #[test]
    fn npm_placeholder_is_not_a_test() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(
            root.join("package.json"),
            r#"{"scripts": {"test": "echo \"Error: no test specified\" && exit 1"}}"#,
        )
        .unwrap();
        assert!(!npm_has_tests(root));
        fs::write(
            root.join("package.json"),
            r#"{"scripts": {"test": "vitest run"}}"#,
        )
        .unwrap();
        assert!(npm_has_tests(root));
    }

    #[test]
    fn a_rust_project_is_checked_and_tested() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("Cargo.toml"), "[package]\n").unwrap();
        if on_path("cargo") {
            assert_eq!(
                commands(dir.path()),
                ["cargo check --all-targets", "cargo test"]
            );
        }
    }
}
