//! Syntax colouring for open files, with Sublime Text grammars through syntect.

use std::path::Path;

use crate::style::Rgb;
use syntect::easy::HighlightLines;
use syntect::highlighting::{Theme, ThemeSet};
use syntect::parsing::{SyntaxReference, SyntaxSet};

/// A dark theme that reads well on most terminal backgrounds. Only its
/// foreground colours are used: the terminal keeps its own background.
const THEME: &str = "base16-ocean.dark";

/// Beyond this a file is shown plain. Highlighting is done in one go when the
/// file opens, and a huge generated file is not worth the wait.
const MAX_LINES: usize = 20_000;

/// A line as coloured runs of text.
pub(crate) type StyledLine = Vec<(Rgb, String)>;

/// The grammars and the theme, loaded once and shared by every file opened.
pub(crate) struct Highlighter {
    syntaxes: SyntaxSet,
    theme: Theme,
}

impl Highlighter {
    pub(crate) fn new() -> Self {
        let mut themes = ThemeSet::load_defaults();
        Self {
            // Lines are handed over without their newline, which is what the
            // "nonewlines" grammars expect.
            syntaxes: SyntaxSet::load_defaults_nonewlines(),
            theme: themes.themes.remove(THEME).unwrap_or_default(),
        }
    }

    /// The grammar for `path`: by extension, then by file name (`Makefile`),
    /// then by first line (a `#!/usr/bin/env python3` shebang).
    fn syntax_for(&self, path: &Path, first_line: Option<&str>) -> Option<&SyntaxReference> {
        let by_name = |name: Option<&std::ffi::OsStr>| {
            name.and_then(|n| n.to_str())
                .and_then(|n| self.syntaxes.find_syntax_by_extension(n))
        };
        by_name(path.extension())
            .or_else(|| by_name(path.file_name()))
            .or_else(|| first_line.and_then(|l| self.syntaxes.find_syntax_by_first_line(l)))
    }

    /// Colours `lines`, or returns `None` when the file is better shown plain:
    /// no known grammar, too long, or a grammar that fails on this input.
    pub(crate) fn highlight(&self, path: &Path, lines: &[String]) -> Option<Vec<StyledLine>> {
        if lines.len() > MAX_LINES {
            return None;
        }
        let syntax = self.syntax_for(path, lines.first().map(String::as_str))?;
        let mut state = HighlightLines::new(syntax, &self.theme);
        lines
            .iter()
            .map(|line| {
                let runs = state.highlight_line(line, &self.syntaxes).ok()?;
                Some(
                    runs.into_iter()
                        .map(|(style, text)| {
                            let c = style.foreground;
                            (Rgb(c.r, c.g, c.b), text.to_owned())
                        })
                        .collect(),
                )
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(text: &str) -> Vec<String> {
        text.lines().map(str::to_owned).collect()
    }

    #[test]
    fn python_keywords_and_strings_get_different_colours() {
        let highlighter = Highlighter::new();
        let styled = highlighter
            .highlight(
                Path::new("hello.py"),
                &lines("def greet():\n    return \"hi\""),
            )
            .unwrap();

        let colour_of = |line: &StyledLine, word: &str| {
            line.iter()
                .find(|(_, text)| text.contains(word))
                .map(|(c, _)| *c)
        };
        let keyword = colour_of(&styled[0], "def").unwrap();
        let string = colour_of(&styled[1], "hi").unwrap();
        assert_ne!(keyword, string);

        // Colouring never changes the text.
        let rebuilt: String = styled[1].iter().map(|(_, t)| t.as_str()).collect();
        assert_eq!(rebuilt, "    return \"hi\"");
    }

    #[test]
    fn a_shebang_is_enough_without_an_extension() {
        let highlighter = Highlighter::new();
        assert!(
            highlighter
                .highlight(Path::new("run"), &lines("#!/usr/bin/env python3\nprint(1)"))
                .is_some()
        );
    }

    #[test]
    fn unknown_files_stay_plain() {
        let highlighter = Highlighter::new();
        assert!(
            highlighter
                .highlight(Path::new("notes.unknownext"), &lines("just text"))
                .is_none()
        );
    }
}
