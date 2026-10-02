use std::path::Path;

use tokio::process::Command;

use crate::error::ToolError;

async fn git(dir: &Path, args: &[&str]) -> Result<String, ToolError> {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
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
