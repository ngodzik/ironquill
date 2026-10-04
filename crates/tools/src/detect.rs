//! Finding how to test a project when nobody said, from its files: what a
//! person would run, for the languages ironquill knows.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use ignore::WalkBuilder;

use crate::check::Check;

/// The checks a project's files call for, looked for each time they are
/// needed, so that tests a model has just written are run too. Only
/// commands found on the machine are returned: a missing one would stop
/// the request rather than judge it.
pub fn detect_checks(root: &Path) -> Vec<Check> {
    let mut checks: Vec<Check> = Vec::new();
    if root.join("Cargo.toml").is_file() {
        checks.extend(Check::parse("cargo check --all-targets"));
        checks.extend(Check::parse("cargo test"));
    }
    checks.extend(python(root));
    if npm_has_tests(root) {
        checks.extend(Check::parse("npm test --silent"));
    }
    checks
        .into_iter()
        .filter(|check| runnable(check.program()))
        .collect()
}

/// Files found at the root of a Python project.
const PYTHON_PROJECT: [&str; 7] = [
    "pyproject.toml",
    "setup.py",
    "setup.cfg",
    "pytest.ini",
    "tox.ini",
    "uv.lock",
    "requirements.txt",
];

/// One check for each Python project with tests: the repository's own, and
/// those of its directories that are projects of their own, such as the
/// `backend` of one that also holds a frontend.
fn python(root: &Path) -> Vec<Check> {
    let files = project_files(root);
    let name = |p: &str| p.rsplit('/').next().unwrap_or(p).to_owned();
    let is_test = |name: &str| {
        name.ends_with(".py") && (name.starts_with("test_") || name.ends_with("_test.py"))
    };
    // Each project's test files, and whether it has a conftest.py.
    let mut projects: BTreeMap<String, (Vec<&str>, bool)> = BTreeMap::new();
    for file in &files {
        if is_test(&name(file)) {
            projects
                .entry(project_of(root, file))
                .or_default()
                .0
                .push(file);
        }
    }
    for file in files.iter().filter(|f| name(f) == "conftest.py") {
        if let Some(project) = projects.get_mut(&project_of(root, file)) {
            project.1 = true;
        }
    }
    projects
        .iter()
        .map(|(dir, (tests, conftest))| python_check(root, dir, tests, *conftest))
        .collect()
}

/// The directory of the nearest Python project holding `file`, relative to
/// the root: empty for the root itself.
fn project_of(root: &Path, file: &str) -> String {
    let mut dir = Path::new(file).parent();
    while let Some(d) = dir.filter(|d| !d.as_os_str().is_empty()) {
        if PYTHON_PROJECT
            .iter()
            .any(|f| root.join(d).join(f).is_file())
        {
            return d.to_string_lossy().replace('\\', "/");
        }
        dir = d.parent();
    }
    String::new()
}

/// pytest when the project uses it and its Python has it, else unittest, run
/// with the project's own Python: uv's when uv manages it, else its virtual
/// environment's, else the one pytest was installed for.
fn python_check(root: &Path, dir: &str, tests: &[&str], conftest: bool) -> Check {
    let base = root.join(dir);
    let mentions = |file: &str, needle: &str| {
        fs::read_to_string(base.join(file)).is_ok_and(|text| text.contains(needle))
    };
    let pytest = conftest
        || base.join("pytest.ini").is_file()
        || mentions("pyproject.toml", "pytest")
        || mentions("requirements.txt", "pytest")
        || mentions("requirements-dev.txt", "pytest")
        || mentions("setup.cfg", "[tool:pytest]")
        || mentions("tox.ini", "[pytest]");
    let (python, has_pytest): (Vec<String>, bool) = if uv_project(root, dir) && on_path("uv") {
        // uv installs what the project locks, pytest included when it does.
        (vec!["uv".into(), "run".into(), "python".into()], pytest)
    } else if let Some(venv) = venv(root, dir) {
        let has = venv
            .parent()
            .is_some_and(|bin| bin.join("pytest").is_file());
        (vec![venv.to_string_lossy().into_owned()], pytest && has)
    } else if let Some(python) = pytest.then(pytest_python).flatten() {
        (vec![python], true)
    } else {
        (vec!["python3".into()], false)
    };
    // -B: without bytecode files, a change written in the same second as
    // the last run, at the same size, is not hidden by a stale one.
    let mut args: Vec<String> = python[1..].to_vec();
    args.push("-B".into());
    if has_pytest {
        args.extend(["-m", "pytest", "-q"].map(String::from));
    } else {
        // From the project's directory, so that the tests import the project
        // as it runs.
        args.extend(["-m", "unittest", "discover"].map(String::from));
        let prefix = if dir.is_empty() {
            String::new()
        } else {
            format!("{dir}/")
        };
        if base.join("tests").is_dir()
            && tests.iter().all(|p| {
                p.strip_prefix(prefix.as_str())
                    .is_some_and(|p| p.starts_with("tests/"))
            })
        {
            args.extend(["-s", "tests"].map(String::from));
        }
    }
    Check::new(python[0].clone(), args).in_dir(dir)
}

/// Whether uv manages the project in `dir`: a `uv.lock` there or above it,
/// as in a uv workspace.
fn uv_project(root: &Path, dir: &str) -> bool {
    Path::new(dir)
        .ancestors()
        .any(|d| root.join(d).join("uv.lock").is_file())
}

/// The Python of the project's virtual environment, in `dir` or at the root.
fn venv(root: &Path, dir: &str) -> Option<PathBuf> {
    let python = if cfg!(windows) {
        "Scripts/python.exe"
    } else {
        "bin/python"
    };
    [dir, ""]
        .iter()
        .flat_map(|d| [".venv", "venv"].map(|v| root.join(d).join(v).join(python)))
        .find(|p| p.is_file())
}

/// The Python that the `pytest` found on the PATH runs with, read from its
/// first lines: `python3 -m pytest` may run another one, without pytest.
fn pytest_python() -> Option<String> {
    let pytest = std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|dir| dir.join("pytest"))
            .find(|p| p.is_file())
    })?;
    interpreter(&fs::read_to_string(pytest).ok()?)
}

/// The interpreter a script names: on its `#!` line, or, for a path with
/// spaces, on the `'''exec' "path"` line pip writes under `#!/bin/sh`.
fn interpreter(script: &str) -> Option<String> {
    let first = script.lines().next()?.strip_prefix("#!")?.trim();
    let mut words = first.split_whitespace();
    let program = words.next()?;
    if program.ends_with("/env") {
        return words.find(|w| !w.starts_with('-')).map(str::to_owned);
    }
    if program.ends_with("/sh") {
        let line = script.lines().find(|l| l.starts_with("'''exec' \""))?;
        return line.split('"').nth(1).map(str::to_owned);
    }
    Some(program.to_owned())
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
        // Installed packages, when not ignored, are not the project's.
        .filter(|e| {
            !e.path().components().any(|c| {
                matches!(
                    c.as_os_str().to_str(),
                    Some("site-packages" | "node_modules")
                )
            })
        })
        .filter_map(|e| {
            e.path()
                .strip_prefix(root)
                .ok()
                .map(|p| p.to_string_lossy().replace('\\', "/"))
        })
        .collect()
}

/// Whether `program` can be run: a file at that path, or one on the PATH.
fn runnable(program: &str) -> bool {
    let path = Path::new(program);
    if path.is_absolute() {
        path.is_file()
    } else {
        on_path(program)
    }
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
    fn a_python_project_in_a_directory_is_tested_there_with_its_own_python() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let backend = root.join("backend");
        fs::create_dir_all(backend.join("tests")).unwrap();
        fs::create_dir_all(backend.join(".venv/bin")).unwrap();
        fs::create_dir_all(root.join("frontend")).unwrap();
        fs::write(
            backend.join("pyproject.toml"),
            "[project]\nname = \"api\"\n",
        )
        .unwrap();
        fs::write(backend.join("tests/test_api.py"), "import unittest\n").unwrap();
        fs::write(backend.join(".venv/bin/python"), "").unwrap();
        let python = backend.join(".venv/bin/python");
        let python = python.to_string_lossy();

        assert_eq!(
            commands(root),
            [format!(
                "{python} -B -m unittest discover -s tests (in backend/)"
            )]
        );

        // It uses pytest, which its environment has.
        fs::write(
            backend.join("pyproject.toml"),
            "[dependency-groups]\ndev = [\"pytest>=8\"]\n",
        )
        .unwrap();
        fs::write(backend.join(".venv/bin/pytest"), "").unwrap();
        assert_eq!(
            commands(root),
            [format!("{python} -B -m pytest -q (in backend/)")]
        );

        // Managed by uv: uv runs it, with what the project locks.
        fs::write(backend.join("uv.lock"), "").unwrap();
        if on_path("uv") {
            assert_eq!(
                commands(root),
                ["uv run python -B -m pytest -q (in backend/)"]
            );
        }
    }

    #[test]
    fn installed_packages_are_not_the_projects_tests() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let site = root.join("venv/lib/python3.12/site-packages/pkg");
        fs::create_dir_all(&site).unwrap();
        fs::write(site.join("test_pkg.py"), "").unwrap();
        assert!(python(root).is_empty());
    }

    #[test]
    fn the_python_of_a_script_is_read_from_its_first_lines() {
        assert_eq!(
            interpreter("#!/opt/homebrew/bin/python3.12\nimport sys\n").as_deref(),
            Some("/opt/homebrew/bin/python3.12")
        );
        assert_eq!(
            interpreter("#!/usr/bin/env -S python3\n").as_deref(),
            Some("python3")
        );
        assert_eq!(
            interpreter("#!/bin/sh\n'''exec' \"/My Tools/venv/bin/python\" \"$0\" \"$@\"\n' '''\n")
                .as_deref(),
            Some("/My Tools/venv/bin/python")
        );
        assert_eq!(interpreter("not a script"), None);
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
