//! What a search of the codebase's maps keeps: the plan's parts, the API's
//! routes and the universe's stars, each by the texts that name it.

/// Whether a search for `query` keeps something named by `texts`: each of
/// its words, whatever their case, is in one of them. An empty search keeps
/// everything.
///
/// # Examples
///
/// ```
/// use ironquill_ui::search_matches;
///
/// assert!(search_matches("", ["anything"]));
/// assert!(search_matches("GUI univ", ["crates/gui/src/universe.rs"]));
/// // Words can be found in different texts: a route's path and its method.
/// assert!(search_matches("post users", ["/users/{id}", "POST"]));
/// assert!(!search_matches("gui tui", ["crates/gui/src/lib.rs"]));
/// ```
pub fn search_matches<'a>(query: &str, texts: impl IntoIterator<Item = &'a str>) -> bool {
    let texts: Vec<String> = texts.into_iter().map(str::to_lowercase).collect();
    query
        .split_whitespace()
        .map(str::to_lowercase)
        .all(|word| texts.iter().any(|text| text.contains(&word)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spaces_alone_keep_everything_and_nothing_is_kept_by_no_text() {
        assert!(search_matches("   ", ["x"]));
        assert!(search_matches("", std::iter::empty()));
        assert!(!search_matches("x", std::iter::empty()));
    }

    #[test]
    fn case_does_not_matter_on_either_side() {
        assert!(search_matches("readme", ["README.md"]));
        assert!(search_matches("ReadMe", ["readme.md"]));
    }
}
