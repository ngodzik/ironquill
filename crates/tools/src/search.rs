//! Searching the project for a pattern, as ripgrep does, so that a model
//! reads the lines that matter instead of whole files.

use std::fs;
use std::path::Path;

use ignore::WalkBuilder;
use regex::RegexBuilder;

use crate::error::ToolError;
use crate::workspace::is_binary;

/// The most matching lines returned: past it the pattern is too vague to
/// be worth the tokens.
const MAX_MATCHES: usize = 100;

/// The longest line returned, in characters; minified code would otherwise
/// fill the answer.
const MAX_LINE: usize = 200;

/// The lines matching `pattern` under `dir`, as `path:line: text`, in files
/// whose name matches `glob` when given. Files that git ignores and binary
/// files are skipped. The pattern is a regular expression, case sensitive
/// unless it has no capital letter.
pub(crate) fn search(
    root: &Path,
    dir: &Path,
    pattern: &str,
    glob: Option<&str>,
) -> Result<(String, usize), ToolError> {
    let smart_case = !pattern.chars().any(char::is_uppercase);
    let regex = RegexBuilder::new(pattern)
        .case_insensitive(smart_case)
        .build()
        .map_err(|e| ToolError::InvalidArguments {
            tool: "search".into(),
            reason: e.to_string(),
        })?;
    let mut walk = WalkBuilder::new(dir);
    walk.hidden(true).git_ignore(true).require_git(false);
    if let Some(glob) = glob {
        let mut types = ignore::overrides::OverrideBuilder::new(dir);
        types.add(glob).map_err(|e| ToolError::InvalidArguments {
            tool: "search".into(),
            reason: e.to_string(),
        })?;
        walk.overrides(types.build().map_err(|e| ToolError::InvalidArguments {
            tool: "search".into(),
            reason: e.to_string(),
        })?);
    }

    let mut paths: Vec<_> = walk
        .build()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_some_and(|t| t.is_file()))
        .map(ignore::DirEntry::into_path)
        .collect();
    paths.sort();

    let mut out = Vec::new();
    let mut total = 0;
    for path in paths {
        let Ok(bytes) = fs::read(&path) else { continue };
        if is_binary(&bytes) {
            continue;
        }
        let text = String::from_utf8_lossy(&bytes);
        let shown = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .display()
            .to_string();
        for (i, line) in text.lines().enumerate() {
            if !regex.is_match(line) {
                continue;
            }
            total += 1;
            if out.len() < MAX_MATCHES {
                let line: String = line.trim_end().chars().take(MAX_LINE).collect();
                out.push(format!("{shown}:{}: {line}", i + 1));
            }
        }
    }
    let mut text = out.join("\n");
    if total > out.len() {
        text.push_str(&format!(
            "\n({} more matches not shown: narrow the pattern, the path or the glob)",
            total - out.len()
        ));
    }
    if total == 0 {
        text = "no match".into();
    }
    Ok((text, total))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_lines_skipping_what_git_ignores() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(
            root.join("src/a.py"),
            "def login(user):\n    return check(user)\n",
        )
        .unwrap();
        fs::write(root.join("src/b.ts"), "export function Login() {}\n").unwrap();
        fs::write(root.join(".gitignore"), "build/\n").unwrap();
        fs::create_dir_all(root.join("build")).unwrap();
        fs::write(root.join("build/a.py"), "def login(): pass\n").unwrap();

        let (text, n) = search(root, root, "login", None).unwrap();
        assert_eq!(n, 2);
        assert_eq!(
            text,
            "src/a.py:1: def login(user):\nsrc/b.ts:1: export function Login() {}"
        );

        // A capital letter makes it case sensitive; a glob narrows the files.
        assert_eq!(search(root, root, "Login", None).unwrap().1, 1);
        assert_eq!(search(root, root, "login", Some("*.py")).unwrap().1, 1);
        assert_eq!(
            search(root, root, "nothing here", None).unwrap().0,
            "no match"
        );
        assert!(search(root, root, "(", None).is_err());
    }
}
