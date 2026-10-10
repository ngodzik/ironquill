//! A pull request's review comments, fetched with `gh` by reading only,
//! made into a request; and the changes chosen, applied with `git apply`.

use std::path::Path;
use std::process::{Command, Stdio};

use serde_json::Value;

/// What the model is asked for each comment.
const ADDRESS_PROMPT: &str = "Address the review comments of the pull request below, one by \
one, numbered as given. For each: say in one sentence what it asks; say whether it is already \
handled, citing code as `path:line`; give the change as a ```diff N block, N its number, against \
the files as they are now; and write a short reply to the reviewer in its own ```text block. \
Change no file: the person applies the changes they choose with /apply.";

/// The request answering pull request `number`'s review comments, read
/// with `gh` (`gh api --method GET` only), bots left out.
pub fn request(root: &Path, number: &str) -> Result<String, String> {
    let repo = gh(
        root,
        &[
            "repo",
            "view",
            "--json",
            "nameWithOwner",
            "-q",
            ".nameWithOwner",
        ],
    )?;
    let repo = repo.trim();
    let mut comments = Vec::new();
    for kind in ["pulls", "issues"] {
        let path = format!("repos/{repo}/{kind}/{number}/comments");
        let text = gh(root, &["api", "--method", "GET", "--paginate", &path])?;
        for comment in parse_pages(&text) {
            comments.push(comment);
        }
    }
    let comments: Vec<String> = comments
        .iter()
        .filter(|c| !is_bot(c))
        .map(|c| {
            let who = c["user"]["login"].as_str().unwrap_or("someone");
            let at = match (
                c["path"].as_str(),
                c["line"].as_u64().or(c["original_line"].as_u64()),
            ) {
                (Some(path), Some(line)) => format!(" on {path}:{line}"),
                (Some(path), None) => format!(" on {path}"),
                _ => String::new(),
            };
            format!("@{who}{at}: {}", c["body"].as_str().unwrap_or("").trim())
        })
        .collect();
    if comments.is_empty() {
        return Err(format!("#{number} has no review comment but bots'"));
    }
    let list: String = comments
        .iter()
        .enumerate()
        .map(|(i, c)| format!("{}. {c}\n\n", i + 1))
        .collect();
    Ok(format!(
        "{ADDRESS_PROMPT}\n\nPull request #{number}:\n\n{list}"
    ))
}

/// The comments of `gh api --paginate`, which may print several arrays.
fn parse_pages(text: &str) -> Vec<Value> {
    serde_json::Deserializer::from_str(text)
        .into_iter::<Value>()
        .filter_map(Result::ok)
        .flat_map(|page| page.as_array().cloned().unwrap_or_default())
        .collect()
}

/// Whether a comment is a bot's.
fn is_bot(comment: &Value) -> bool {
    comment["user"]["type"] == "Bot"
        || comment["user"]["login"]
            .as_str()
            .is_some_and(|l| l.ends_with("[bot]"))
}

fn gh(root: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new("gh")
        .args(args)
        .current_dir(root)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("cannot run gh: {e}"))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        Err(format!(
            "gh {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

/// Applies `patches` with `git apply`, all or none: each is checked first,
/// with the counts of its hunks worked out again, as models get them wrong.
pub fn apply(root: &Path, patches: &[String]) -> Result<(), String> {
    let run = |patch: &str, check: bool| -> Result<(), String> {
        let mut args = vec!["apply", "--recount", "--whitespace=nowarn"];
        if check {
            args.push("--check");
        }
        let mut child = Command::new("git")
            .args(&args)
            .current_dir(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("cannot run git: {e}"))?;
        if let Some(mut stdin) = child.stdin.take() {
            use std::io::Write as _;
            stdin
                .write_all(patch.as_bytes())
                .map_err(|e| format!("cannot write the patch: {e}"))?;
        }
        let output = child
            .wait_with_output()
            .map_err(|e| format!("git apply: {e}"))?;
        if output.status.success() {
            Ok(())
        } else {
            Err(String::from_utf8_lossy(&output.stderr).trim().to_owned())
        }
    };
    for (i, patch) in patches.iter().enumerate() {
        run(patch, true)
            .map_err(|e| format!("Nothing applied: change {} does not apply: {e}", i + 1))?;
    }
    for patch in patches {
        run(patch, false)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comments_of_people_are_kept_across_pages() {
        let text = r#"[{"user": {"login": "ana", "type": "User"}, "body": "Rename it", "path": "a.py", "line": 3}]
[{"user": {"login": "ci[bot]", "type": "Bot"}, "body": "Coverage"}]"#;
        let comments = parse_pages(text);
        assert_eq!(comments.len(), 2);
        assert!(!is_bot(&comments[0]));
        assert!(is_bot(&comments[1]));
    }

    #[test]
    fn patches_apply_all_or_none() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let git = |args: &[&str]| {
            assert!(
                Command::new("git")
                    .args(args)
                    .current_dir(root)
                    .env_remove("GIT_DIR")
                    .env_remove("GIT_INDEX_FILE")
                    .output()
                    .unwrap()
                    .status
                    .success()
            );
        };
        git(&["init", "-q"]);
        std::fs::write(root.join("a.txt"), "one\ntwo\n").unwrap();
        let good = "--- a/a.txt\n+++ b/a.txt\n@@ -1,2 +1,2 @@\n one\n-two\n+TWO\n".to_owned();
        // Wrong counts in its header: worked out again.
        let miscounted = "--- a/a.txt\n+++ b/a.txt\n@@ -1,9 +1,9 @@\n one\n-two\n+TWO\n".to_owned();
        let bad = "--- a/a.txt\n+++ b/a.txt\n@@ -1,2 +1,2 @@\n one\n-three\n+3\n".to_owned();
        assert!(
            apply(root, &[good.clone(), bad])
                .unwrap_err()
                .starts_with("Nothing applied: change 2")
        );
        assert_eq!(
            std::fs::read_to_string(root.join("a.txt")).unwrap(),
            "one\ntwo\n"
        );
        apply(root, &[miscounted]).unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join("a.txt")).unwrap(),
            "one\nTWO\n"
        );
    }
}
