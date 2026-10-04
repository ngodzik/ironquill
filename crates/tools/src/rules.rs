//! The instructions a project already has for coding agents, as other tools
//! read them: CLAUDE.md, CLAUDE.local.md, AGENTS.md and the rules under
//! .claude/rules. Read only, never written: a model does not know they
//! exist unless it is given them.

use std::fs;
use std::path::{Path, PathBuf};

/// The most that is passed on, in bytes: instructions are paid for on every
/// call, and long ones are followed less well anyway.
const MAX_BYTES: usize = 40_000;

/// How deep `@path` imports are followed, as Claude Code does.
const MAX_IMPORT_DEPTH: usize = 4;

/// The project's instructions for coding agents, each file under its name,
/// `None` when it has none.
pub fn project_instructions(root: &Path) -> Option<String> {
    let mut files: Vec<PathBuf> = [
        "CLAUDE.md",
        ".claude/CLAUDE.md",
        "CLAUDE.local.md",
        "AGENTS.md",
    ]
    .iter()
    .map(|name| root.join(name))
    .filter(|path| path.is_file())
    .collect();
    let mut rules = Vec::new();
    collect_markdown(&root.join(".claude/rules"), &mut rules);
    rules.sort();
    files.extend(rules);

    let mut out = String::new();
    for path in files {
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        let name = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .display()
            .to_string();
        let (scope, body) = split_paths(&text);
        let body = expand_imports(body, path.parent().unwrap_or(root), root, 0);
        let header = match scope {
            Some(paths) => format!("## {name} (only for files matching: {paths})"),
            None => format!("## {name}"),
        };
        out.push_str(&format!("{header}\n{}\n\n", body.trim()));
        if out.len() > MAX_BYTES {
            let cut = (0..=MAX_BYTES)
                .rev()
                .find(|i| out.is_char_boundary(*i))
                .unwrap_or(0);
            out.truncate(cut);
            out.push_str("\n(the rest of the project's instructions was left out: too long)");
            break;
        }
    }
    (!out.trim().is_empty()).then(|| out.trim_end().to_owned())
}

/// The Markdown files under `dir`, in its subdirectories too.
fn collect_markdown(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_markdown(&path, out);
        } else if path.extension().is_some_and(|e| e == "md") {
            out.push(path);
        }
    }
}

/// The `paths:` a rule is scoped to, from its front matter, and its body.
fn split_paths(text: &str) -> (Option<String>, &str) {
    let Some(rest) = text.strip_prefix("---\n") else {
        return (None, text);
    };
    let Some(end) = rest.find("\n---") else {
        return (None, text);
    };
    let front = &rest[..end];
    let body = rest[end + 4..].trim_start_matches(['-', '\n']);
    let mut paths = Vec::new();
    let mut in_paths = false;
    for line in front.lines() {
        if let Some(value) = line.strip_prefix("paths:") {
            in_paths = true;
            let value = value.trim().trim_matches(['[', ']']);
            paths.extend(
                value
                    .split(',')
                    .map(|p| p.trim().trim_matches(['"', '\'']).to_owned())
                    .filter(|p| !p.is_empty()),
            );
        } else if in_paths && line.trim_start().starts_with('-') {
            let p = line.trim_start().trim_start_matches('-').trim();
            paths.push(p.trim_matches(['"', '\'']).to_owned());
        } else {
            in_paths = false;
        }
    }
    let scope = (!paths.is_empty()).then(|| paths.join(", "));
    (scope, body)
}

/// `text` with each `@path` replaced by that file, when it is inside the
/// project; paths in backticks or code blocks are left as written.
fn expand_imports(text: &str, base: &Path, root: &Path, depth: usize) -> String {
    let mut out = String::new();
    let mut in_block = false;
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            in_block = !in_block;
        }
        out.push_str(line);
        out.push('\n');
        if in_block || depth >= MAX_IMPORT_DEPTH {
            continue;
        }
        let mut in_code = false;
        for word in line.split_whitespace() {
            if word.matches('`').count() % 2 == 1 {
                in_code = !in_code;
            }
            let Some(target) = word.strip_prefix('@').filter(|_| !in_code) else {
                continue;
            };
            let target = target.trim_end_matches(['.', ',', ';', ')']);
            let path = base.join(target);
            let inside = path
                .canonicalize()
                .ok()
                .zip(root.canonicalize().ok())
                .is_some_and(|(p, r)| p.starts_with(r));
            if !inside {
                continue;
            }
            if let Ok(imported) = fs::read_to_string(&path) {
                let nested =
                    expand_imports(&imported, path.parent().unwrap_or(base), root, depth + 1);
                out.push_str(&format!("(from {target})\n{}\n", nested.trim()));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_projects_own_instructions_are_found() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        assert_eq!(project_instructions(root), None);

        fs::write(
            root.join("CLAUDE.md"),
            "Use tabs.\nSee @docs/style.md for more.\n",
        )
        .unwrap();
        fs::create_dir_all(root.join("docs")).unwrap();
        fs::write(root.join("docs/style.md"), "Name things plainly.").unwrap();
        fs::write(root.join("AGENTS.md"), "Run pytest.").unwrap();
        fs::create_dir_all(root.join(".claude/rules/frontend")).unwrap();
        fs::write(root.join(".claude/rules/testing.md"), "Test every route.").unwrap();
        fs::write(
            root.join(".claude/rules/frontend/react.md"),
            "---\npaths:\n  - \"app/frontend/**\"\n---\nUse function components.",
        )
        .unwrap();

        let text = project_instructions(root).unwrap();
        assert!(text.starts_with("## CLAUDE.md\nUse tabs."));
        assert!(text.contains("(from docs/style.md)\nName things plainly."));
        assert!(text.contains("## AGENTS.md\nRun pytest."));
        assert!(text.contains("## .claude/rules/testing.md\nTest every route."));
        assert!(text.contains(
            "## .claude/rules/frontend/react.md (only for files matching: app/frontend/**)\nUse function components."
        ));
    }

    #[test]
    fn imports_outside_the_project_or_in_code_are_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("p");
        fs::create_dir_all(&root).unwrap();
        fs::write(dir.path().join("secret.md"), "outside").unwrap();
        fs::write(root.join("x.md"), "inside").unwrap();
        fs::write(root.join("CLAUDE.md"), "@../secret.md and `@x.md`\n").unwrap();
        let text = project_instructions(&root).unwrap();
        assert!(!text.contains("outside") && !text.contains("(from x.md)"));
    }
}
