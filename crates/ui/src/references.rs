//! Code a model cites in a reply, found deterministically: `path:line`,
//! `path:start-end`, a file of the project, a commit. Drawn as links; a
//! click opens it.

use std::sync::LazyLock;

use regex::Regex;

/// What a reference points at.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Reference {
    /// A file of the project, at a line when one was given.
    File {
        /// The path, relative to the project.
        path: String,
        /// The line, from 1, when one was given.
        line: Option<usize>,
    },
    /// A commit.
    Commit(String),
}

static CANDIDATE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?P<path>(?:[A-Za-z0-9_.-]+/)*[A-Za-z0-9_-][A-Za-z0-9_.-]*\.[A-Za-z0-9]{1,10})(?::(?P<start>\d+)(?:-\d+)?)?|(?P<hash>\b[0-9a-f]{7,40}\b)",
    )
    .expect("a valid pattern")
});

/// The places in `text` that may be references, as byte ranges with what
/// they would point at; whether they exist is for the caller to check.
pub fn candidates(text: &str) -> Vec<(usize, usize, Reference)> {
    let mut out = Vec::new();
    for found in CANDIDATE.captures_iter(text) {
        let whole = found.get(0).expect("a match");
        // Part of a URL or a longer word is not a reference.
        let before = text[..whole.start()].chars().next_back();
        if before.is_some_and(|c| c.is_alphanumeric() || matches!(c, '/' | '.' | ':' | '@'))
            || text[..whole.start()]
                .rsplit(char::is_whitespace)
                .next()
                .is_some_and(|word| word.contains("://"))
        {
            continue;
        }
        let reference = if let Some(hash) = found.name("hash") {
            Reference::Commit(hash.as_str().to_owned())
        } else {
            let path = found["path"].trim_start_matches("./").to_owned();
            let line = found.name("start").and_then(|l| l.as_str().parse().ok());
            Reference::File { path, line }
        };
        out.push((whole.start(), whole.end(), reference));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn found(text: &str) -> Vec<(String, Reference)> {
        candidates(text)
            .into_iter()
            .map(|(a, b, r)| (text[a..b].to_owned(), r))
            .collect()
    }

    #[test]
    fn references_are_found_in_prose() {
        let refs = found("See src/app.rs:42 and lib/x.py:3-9, in 1a2b3c4d, not https://x.io/a.rs.");
        assert_eq!(
            refs,
            [
                (
                    "src/app.rs:42".to_owned(),
                    Reference::File {
                        path: "src/app.rs".into(),
                        line: Some(42)
                    }
                ),
                (
                    "lib/x.py:3-9".to_owned(),
                    Reference::File {
                        path: "lib/x.py".into(),
                        line: Some(3)
                    }
                ),
                ("1a2b3c4d".to_owned(), Reference::Commit("1a2b3c4d".into())),
            ]
        );
    }
}
