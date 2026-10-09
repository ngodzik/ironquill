//! Syntax colouring for open files, with Sublime Text grammars through syntect.

use std::path::Path;

use crate::style::Rgb;
use syntect::highlighting::{
    HighlightIterator, HighlightState, Highlighter as Painter, Theme, ThemeSet,
};
use syntect::parsing::{ParseState, ScopeStack, SyntaxReference, SyntaxSet};

/// A dark theme that reads well on most terminal backgrounds. Only its
/// foreground colours are used: the terminal keeps its own background.
const THEME: &str = "base16-ocean.dark";

/// Beyond this a file is shown plain. Highlighting is done in one go when the
/// file opens, and a huge generated file is not worth the wait.
const MAX_LINES: usize = 20_000;

/// A line as coloured runs of text.
pub(crate) type StyledLine = Vec<(Rgb, String)>;

/// Where the grammar and the theme stand at the start of a line: all that
/// colouring a line needs from the lines before it.
type Start = (ParseState, HighlightState);

/// A file's lines, coloured, with what it takes to colour them again from
/// any line: an edit is coloured from its line on, and only as far as it
/// changes how the lines after it read (an opened string, a comment).
pub(crate) struct Highlighted {
    /// The lines as they were coloured.
    texts: Vec<String>,
    styled: Vec<StyledLine>,
    /// The start of each line, and the end of the last, each the one the
    /// line before led to: colouring a line from its start gives the line
    /// shown and the next start, so equal starts mean all after is equal.
    starts: Vec<Start>,
    /// The first line whose colours may be wrong, and the start it really
    /// has. An edit that changes how the lines after it read (an opened
    /// string or comment) colours only so many of them while typing, so
    /// that no key waits on the whole file; the next keys carry on.
    stale: Option<(usize, Start)>,
}

impl Highlighted {
    /// The coloured lines.
    pub(crate) fn lines(&self) -> &[StyledLine] {
        &self.styled
    }

    /// Whether some lines still have colours from before an edit.
    #[cfg(test)]
    fn stale_from(&self) -> Option<usize> {
        self.stale.as_ref().map(|(line, _)| *line)
    }

    /// Colours `lines` again from the first that differs from those
    /// coloured, until the lines after it read as before; past the edit,
    /// `budget` lines at most, all of them with `None`. `false` when the
    /// grammar fails on them, and they are better shown plain.
    pub(crate) fn update(
        &mut self,
        highlighter: &Highlighter,
        lines: &[String],
        budget: Option<usize>,
    ) -> bool {
        let (old_n, new_n) = (self.texts.len(), lines.len());
        let prefix = self
            .texts
            .iter()
            .zip(lines)
            .take_while(|(a, b)| a == b)
            .count();
        if prefix == old_n && old_n == new_n {
            return self.catch_up(highlighter, budget);
        }
        let suffix = self
            .texts
            .iter()
            .rev()
            .zip(lines.iter().rev())
            .take(old_n.min(new_n) - prefix)
            .take_while(|(a, b)| a == b)
            .count();
        // From here on the lines are the old ones, moved.
        let unchanged_from = new_n - suffix;
        let old = |i: usize| i + old_n - new_n;
        let painter = Painter::new(&highlighter.theme);

        // From the first line that may be wrong: the edit, or before it.
        let stale = self.stale.take();
        let (first, mut state, ahead) = match stale {
            Some((line, start)) if line <= prefix => (line, start, None),
            // Past the edit: counted as the lines are now.
            Some((line, start)) if line >= old_n - suffix => (
                prefix,
                self.starts[prefix].clone(),
                Some((line + new_n - old_n, start)),
            ),
            // Inside the edit, which is coloured whole.
            _ => (prefix, self.starts[prefix].clone(), None),
        };
        let initial = state.clone();
        let mut styled = Vec::new();
        let mut starts = Vec::new();
        let mut i = first;
        let (end, stale) = loop {
            if i >= unchanged_from {
                let j = old(i);
                if j == old_n && i == new_n {
                    break (old_n, None);
                }
                // The lines after read as before: they are kept.
                if self.starts[j] == state {
                    break (j, ahead.filter(|(line, _)| *line > i));
                }
                if budget.is_some_and(|b| i - prefix >= b) {
                    // The start kept is the one its line was coloured from.
                    if let Some(last) = starts.last_mut() {
                        *last = self.starts[j].clone();
                    }
                    break (j, Some((i, state)));
                }
            }
            let Some(line) = highlighter.colour(&painter, &lines[i], &mut state) else {
                self.stale = None;
                return false;
            };
            styled.push(line);
            starts.push(state.clone());
            i += 1;
        };
        self.texts
            .splice(first..end, lines[first..i].iter().cloned());
        self.styled.splice(first..end, styled);
        self.starts.splice(first + 1..=end, starts);
        self.starts[first] = initial;
        self.stale = stale;
        match self.stale {
            Some((line, _)) if line > i => self.catch_up(highlighter, budget),
            _ => true,
        }
    }

    /// Colours again the lines that may be wrong, as far as they differ
    /// from what they were, `budget` lines at most.
    fn catch_up(&mut self, highlighter: &Highlighter, budget: Option<usize>) -> bool {
        let Some((first, mut state)) = self.stale.take() else {
            return true;
        };
        let painter = Painter::new(&highlighter.theme);
        let mut i = first;
        while i < self.texts.len() {
            if i > first && self.starts[i] == state {
                return true;
            }
            if budget.is_some_and(|b| i - first >= b) {
                self.stale = Some((i, state));
                return true;
            }
            self.starts[i] = state.clone();
            let Some(line) = highlighter.colour(&painter, &self.texts[i], &mut state) else {
                return false;
            };
            self.styled[i] = line;
            i += 1;
        }
        if let Some(end) = self.starts.last_mut() {
            *end = state;
        }
        true
    }
}

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
    pub(crate) fn highlight(&self, path: &Path, lines: &[String]) -> Option<Highlighted> {
        if lines.len() > MAX_LINES {
            return None;
        }
        let syntax = self.syntax_for(path, lines.first().map(String::as_str))?;
        let painter = Painter::new(&self.theme);
        let mut state = (
            ParseState::new(syntax),
            HighlightState::new(&painter, ScopeStack::new()),
        );
        let mut highlighted = Highlighted {
            texts: Vec::with_capacity(lines.len()),
            styled: Vec::with_capacity(lines.len()),
            starts: vec![state.clone()],
            stale: None,
        };
        for line in lines {
            highlighted
                .styled
                .push(self.colour(&painter, line, &mut state)?);
            highlighted.texts.push(line.clone());
            highlighted.starts.push(state.clone());
        }
        Some(highlighted)
    }

    /// Colours one line from `state`, the start of the line, which becomes
    /// the start of the next.
    fn colour(&self, painter: &Painter, line: &str, state: &mut Start) -> Option<StyledLine> {
        let ops = state.0.parse_line(line, &self.syntaxes).ok()?;
        Some(
            HighlightIterator::new(&mut state.1, &ops, line, painter)
                .map(|(style, text)| {
                    let c = style.foreground;
                    (Rgb(c.r, c.g, c.b), text.to_owned())
                })
                .collect(),
        )
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
        let keyword = colour_of(&styled.lines()[0], "def").unwrap();
        let string = colour_of(&styled.lines()[1], "hi").unwrap();
        assert_ne!(keyword, string);

        // Colouring never changes the text.
        let rebuilt: String = styled.lines()[1].iter().map(|(_, t)| t.as_str()).collect();
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

    /// Each edit, coloured from where it is, reads as the whole coloured
    /// again: the line typed on alone, or as far as an opened comment
    /// reaches, lines added and taken away.
    #[test]
    fn an_edit_colours_as_the_whole_file_would() {
        let highlighter = Highlighter::new();
        let path = Path::new("a.rs");
        let mut now =
            lines("fn a() {\n    let x = 1;\n}\n\nfn b() {\n    let y = \"s\";\n}\n// end");
        let mut highlighted = highlighter.highlight(path, &now).unwrap();
        type Edit<'a> = &'a dyn Fn(&mut Vec<String>);
        let edits: [Edit; 7] = [
            &|l| l[1].push_str(" // note"),
            &|l| l[3] = "/* opened".into(),
            &|l| l.insert(5, "still inside".into()),
            &|l| l[3].push_str(" */"),
            &|l| {
                l.drain(1..3);
            },
            &|l| l.push("fn c() {}".into()),
            &|l| l.clear(),
        ];
        for edit in edits {
            edit(&mut now);
            assert!(highlighted.update(&highlighter, &now, None));
            let whole = highlighter.highlight(path, &now).unwrap();
            assert_eq!(highlighted.lines(), whole.lines(), "{now:?}");
            assert!(highlighted.stale_from().is_none());
        }
    }

    /// While typing, an edit that changes how the rest reads is coloured a
    /// few lines at a time, and the next edits carry on until all is right.
    #[test]
    fn a_long_reach_is_coloured_a_budget_at_a_time() {
        let highlighter = Highlighter::new();
        let path = Path::new("a.rs");
        let mut now: Vec<String> = (0..50).map(|i| format!("let v{i} = {i};")).collect();
        let mut highlighted = highlighter.highlight(path, &now).unwrap();
        now[2] = "/* opened".into();
        assert!(highlighted.update(&highlighter, &now, Some(10)));
        // The line edited, and ten more.
        assert_eq!(highlighted.stale_from(), Some(12));
        let whole = highlighter.highlight(path, &now).unwrap();
        assert_eq!(highlighted.lines()[..12], whole.lines()[..12]);
        assert_ne!(highlighted.lines()[20], whole.lines()[20]);
        // Typed on a line of its own, far below: the lines in between
        // are not forgotten.
        now[40].push('x');
        for _ in 0..5 {
            assert!(highlighted.update(&highlighter, &now, Some(10)));
        }
        let whole = highlighter.highlight(path, &now).unwrap();
        assert_eq!(highlighted.lines(), whole.lines());
        assert!(highlighted.stale_from().is_none());
    }

    /// Many edits of every kind, typed with a small budget and then left:
    /// the colours always end as the whole file's.
    #[test]
    fn any_run_of_edits_ends_coloured_as_the_whole() {
        let highlighter = Highlighter::new();
        let path = Path::new("a.rs");
        let pieces = [
            "/* ",
            " */",
            "\"",
            "let a = 1;",
            "",
            "fn f() {",
            "}",
            "// c",
            "'x'",
        ];
        let mut now: Vec<String> = (0..60).map(|i| format!("let v{i} = {i};")).collect();
        let mut highlighted = highlighter.highlight(path, &now).unwrap();
        // A small deterministic generator: the same run every time.
        let mut seed: u64 = 7;
        let mut next = |n: usize| {
            seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
            (seed >> 33) as usize % n.max(1)
        };
        for _ in 0..400 {
            let at = next(now.len() + 1);
            let piece = pieces[next(pieces.len())].to_owned();
            match next(4) {
                0 if at < now.len() => now[at].push_str(&piece),
                1 => now.insert(at.min(now.len()), piece),
                2 if at < now.len() && now.len() > 1 => {
                    now.remove(at);
                }
                _ if at < now.len() => now[at] = piece,
                _ => now.push(piece),
            }
            assert!(highlighted.update(&highlighter, &now, Some(3)));
            // What is shown is always the text, whatever its colours.
            let shown: Vec<String> = highlighted
                .lines()
                .iter()
                .map(|l| l.iter().map(|(_, t)| t.as_str()).collect())
                .collect();
            assert_eq!(shown, now);
        }
        assert!(highlighted.update(&highlighter, &now, None));
        let whole = highlighter.highlight(path, &now).unwrap();
        assert_eq!(highlighted.lines(), whole.lines());
        assert_eq!(highlighted.starts, whole.starts);
    }
}
