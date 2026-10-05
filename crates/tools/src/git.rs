use std::path::Path;

use tokio::process::Command;

use crate::error::ToolError;
use crate::workspace::file_is_binary;

async fn git(dir: &Path, args: &[&str]) -> Result<String, ToolError> {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        // The repository of the directory given, whatever started ironquill.
        .env_remove("GIT_DIR")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_WORK_TREE")
        .output()
        .await
        .map_err(|source| ToolError::Spawn {
            command: format!("git {}", args.join(" ")),
            source,
        })?;
    if !output.status.success() {
        return Err(ToolError::Git(
            String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Whether the working tree at `dir` has no uncommitted changes.
///
/// The agent refuses to start on a dirty tree by default: git is its undo,
/// and an undo that would also throw away the person's own work is not one.
///
/// # Errors
///
/// [`ToolError::Git`] when `dir` is not inside a git repository.
pub async fn is_clean(dir: &Path) -> Result<bool, ToolError> {
    Ok(git(dir, &["status", "--porcelain"])
        .await?
        .trim()
        .is_empty())
}

/// The files git tracks, relative to `dir`, at most `limit` of them.
///
/// Handed to the model up front, this answers for free the question it would
/// otherwise spend several turns of directory listing on.
///
/// # Errors
///
/// [`ToolError::Git`] when `dir` is not inside a git repository.
pub async fn tracked_files(dir: &Path, limit: usize) -> Result<Vec<String>, ToolError> {
    Ok(git(dir, &["ls-files"])
        .await?
        .lines()
        .take(limit)
        .map(str::to_owned)
        .collect())
}

/// A short account of what differs from the last commit: one line per changed
/// or new file, then the line counts.
///
/// # Errors
///
/// [`ToolError::Git`] when `dir` is not inside a git repository.
pub async fn diff_stat(dir: &Path) -> Result<String, ToolError> {
    let status = git(dir, &["status", "--short"]).await?;
    if status.trim().is_empty() {
        return Ok("no changes since the last commit".into());
    }
    let stat = git(dir, &["diff", "--stat"]).await?;
    Ok(format!("{}\n{}", status.trim_end(), stat.trim_end()))
}

/// What changed in `paths` since the last commit, as a reviewer reads it:
/// a diff for a file git knows, the whole text of a new one. Outside a
/// repository, or when git fails, the files as they are now. At most
/// `limit` bytes.
pub async fn changes_text(dir: &Path, paths: &[String], limit: usize) -> String {
    let mut out = String::new();
    for path in paths {
        let tracked = git(dir, &["ls-files", "--error-unmatch", "--", path])
            .await
            .is_ok();
        let part = if tracked {
            git(dir, &["diff", "--no-color", "HEAD", "--", path])
                .await
                .unwrap_or_default()
        } else {
            std::fs::read_to_string(dir.join(path))
                .map(|text| format!("new file {path}:\n{text}"))
                .unwrap_or_else(|_| format!("{path}: deleted or unreadable\n"))
        };
        out.push_str(&part);
        out.push('\n');
        if out.len() > limit {
            let cut = (0..=limit)
                .rev()
                .find(|i| out.is_char_boundary(*i))
                .unwrap_or(0);
            out.truncate(cut);
            out.push_str("\n(the rest of the changes was left out: too long)");
            break;
        }
    }
    out
}

/// The project's files, relative to `dir`, at most `limit` of them: the ones
/// git tracks in a repository, otherwise every file found by walking the
/// directory, skipping hidden directories and build output.
///
/// Never fails: an empty list only means the model will look for itself.
pub async fn project_files(dir: &Path, limit: usize) -> Vec<String> {
    // Binary files are left out: the model can do nothing with them, and
    // seeing one by name invites it to try.
    if let Ok(files) = tracked_files(dir, limit).await {
        return files
            .into_iter()
            .filter(|f| !file_is_binary(&dir.join(f)))
            .collect();
    }
    let mut files = Vec::new();
    walk(dir, dir, limit, &mut files);
    files.sort();
    files
}

/// Directories that hold what a build produced or downloaded, never sources.
const SKIPPED: [&str; 3] = ["target", "node_modules", "__pycache__"];

fn walk(root: &Path, dir: &Path, limit: usize, files: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = entries.flatten().collect();
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        if files.len() >= limit {
            return;
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') {
            continue;
        }
        let path = entry.path();
        match entry.file_type() {
            Ok(t) if t.is_dir() => {
                if !SKIPPED.contains(&name.as_ref()) {
                    walk(root, &path, limit, files);
                }
            }
            Ok(t) if t.is_file() && !file_is_binary(&path) => {
                if let Ok(relative) = path.strip_prefix(root) {
                    files.push(relative.to_string_lossy().into_owned());
                }
            }
            _ => {}
        }
    }
}

/// The opening context for a model: the project's text files, or a sentence
/// saying there are none, so that it does not spend turns finding out.
pub async fn project_context(dir: &Path, limit: usize) -> String {
    let files = project_files(dir, limit).await;
    if files.is_empty() {
        "The project has no text files yet.".to_owned()
    } else {
        format!("Files in the project:\n{}", files.join("\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn outside_git_the_directory_is_walked() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::create_dir_all(dir.path().join("target/debug")).unwrap();
        std::fs::create_dir_all(dir.path().join(".hidden")).unwrap();
        std::fs::write(dir.path().join("src/main.rs"), "").unwrap();
        std::fs::write(dir.path().join("README.md"), "").unwrap();
        std::fs::write(dir.path().join("target/debug/app"), "").unwrap();
        std::fs::write(dir.path().join(".hidden/x"), "").unwrap();
        std::fs::write(dir.path().join("ironquill"), [0x7f, b'E', b'L', b'F', 0]).unwrap();

        let files = project_files(dir.path(), 100).await;

        assert_eq!(files, ["README.md", "src/main.rs"]);
    }
}
