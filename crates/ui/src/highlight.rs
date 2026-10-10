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

/// Beyond this a file is shown plain: a generated file that long is not
/// read for its colours.
const MAX_LINES: usize = 100_000;

/// A line as coloured runs of text.
pub(crate) type StyledLine = Vec<(Rgb, String)>;

/// Where the grammar and the theme stand at the start of a line: all that
/// colouring a line needs from the lines before it.
type Start = (ParseState, HighlightState);

/// A file's lines, coloured as far as they were shown, with what it takes
/// to colour them again from any line: a file opens with only what is in
/// sight coloured, and an edit is coloured from its line on, only as far as
/// it changes how the lines after it read (an opened string, a comment).
pub(crate) struct Highlighted {
    /// The lines as they were coloured.
    texts: Vec<String>,
    /// Each line as coloured runs; one plain run past `coloured`.
    styled: Vec<StyledLine>,
    /// The start of each line, and the end of the last, each the one the
    /// line before led to: colouring a line from its start gives the line
    /// shown and the next start, so equal starts mean all after is equal.
    /// Past `coloured`, they mean nothing.
    starts: Vec<Start>,
    /// How many lines, from the first, are coloured.
    coloured: usize,
    /// The first line whose colours may be wrong, and the start it really
    /// has. An edit that changes how the lines after it read colours only so
    /// many of them while typing, so that no key waits on the whole file;
    /// the next keys carry on.
    stale: Option<(usize, Start)>,
}

/// A line not coloured yet: its text, plain.
fn plain(line: &str) -> StyledLine {
    vec![(Rgb(192, 197, 206), line.to_owned())]
}

impl Highlighted {
    /// The lines, coloured as far as [`Highlighted::colour_to`] was asked.
    pub(crate) fn lines(&self) -> &[StyledLine] {
        &self.styled
    }

    /// Whether some lines still have colours from before an edit.
    #[cfg(test)]
    fn stale_from(&self) -> Option<usize> {
        self.stale.as_ref().map(|(line, _)| *line)
    }

    /// Colours the lines up to `end`, for them to be shown. `false` when
    /// the grammar fails on them, and they are better shown plain.
    pub(crate) fn colour_to(&mut self, highlighter: &Highlighter, end: usize) -> bool {
        let end = end.min(self.texts.len());
        if self.coloured >= end {
            return true;
        }
        // What is coloured is made right first: the rest follows from it.
        if !self.catch_up(highlighter, None) {
            return false;
        }
        let painter = Painter::new(&highlighter.theme);
        let mut state = self.starts[self.coloured].clone();
        for i in self.coloured..end {
            let Some(line) = highlighter.colour(&painter, &self.texts[i], &mut state) else {
                return false;
            };
            self.styled[i] = line;
            self.starts[i + 1] = state.clone();
        }
        self.coloured = end;
        true
    }

    /// Colours `lines` again from the first that differs from those
    /// coloured, until the lines after it read as before; past the edit,
    /// `budget` lines at most, all of them with `None`. Lines not coloured
    /// yet are only taken as they are. `false` when the grammar fails on
    /// them, and they are better shown plain.
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
        let coloured = self.coloured;

        // An edit past what is coloured: nothing to colour yet.
        if prefix >= coloured {
            let changed = lines[prefix..unchanged_from].iter();
            self.styled
                .splice(prefix..old_n - suffix, changed.clone().map(|l| plain(l)));
            self.texts.splice(prefix..old_n - suffix, changed.cloned());
            let filler = self.starts[coloured].clone();
            self.starts.resize(new_n + 1, filler);
            return true;
        }

        let painter = Painter::new(&highlighter.theme);
        // From the first line that may be wrong: the edit, or before it.
        let (first, mut state, ahead) = match self.stale.take() {
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
        let (end, stale, now_coloured) = loop {
            if i >= unchanged_from {
                let j = old(i);
                // What follows was never coloured: it stays so.
                if j >= coloured {
                    break (j, None, i);
                }
                // The lines after read as before: they are kept.
                if self.starts[j] == state {
                    let ahead = ahead.filter(|(line, _)| *line > i);
                    break (j, ahead, coloured + new_n - old_n);
                }
                if budget.is_some_and(|b| i - prefix >= b) {
                    // The start kept is the one its line was coloured from.
                    if let Some(last) = starts.last_mut() {
                        *last = self.starts[j].clone();
                    }
                    break (j, Some((i, state)), coloured + new_n - old_n);
                }
            }
            let Some(line) = highlighter.colour(&painter, &lines[i], &mut state) else {
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
        let filler = self.starts[now_coloured].clone();
        self.starts.resize(new_n + 1, filler);
        self.coloured = now_coloured;
        self.stale = stale;
        match self.stale {
            Some((line, _)) if line > i => self.catch_up(highlighter, budget),
            _ => true,
        }
    }

    /// Colours again the coloured lines that may be wrong, as far as they
    /// differ from what they were, `budget` lines at most.
    fn catch_up(&mut self, highlighter: &Highlighter, budget: Option<usize>) -> bool {
        let Some((first, mut state)) = self.stale.take() else {
            return true;
        };
        let painter = Painter::new(&highlighter.theme);
        let mut i = first;
        while i < self.coloured {
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
        self.starts[self.coloured] = state;
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

    /// `lines` ready to be coloured, none of them yet: they are as they are
    /// shown, with [`Highlighted::colour_to`]. `None` when the file is
    /// better shown plain: no known grammar, or too long.
    pub(crate) fn highlight(&self, path: &Path, lines: &[String]) -> Option<Highlighted> {
        if lines.len() > MAX_LINES {
            return None;
        }
        let syntax = self.syntax_for(path, lines.first().map(String::as_str))?;
        let painter = Painter::new(&self.theme);
        let start = (
            ParseState::new(syntax),
            HighlightState::new(&painter, ScopeStack::new()),
        );
        // Nothing coloured yet: what is shown is, when it is.
        Some(Highlighted {
            texts: lines.to_vec(),
            styled: lines.iter().map(|l| plain(l)).collect(),
            starts: vec![start; lines.len() + 1],
            coloured: 0,
            stale: None,
        })
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

    /// `lines` coloured whole, as a file shown from top to bottom.
    fn whole(highlighter: &Highlighter, path: &Path, lines: &[String]) -> Highlighted {
        let mut highlighted = highlighter.highlight(path, lines).unwrap();
        assert!(highlighted.colour_to(highlighter, lines.len()));
        highlighted
    }

    #[test]
    fn python_keywords_and_strings_get_different_colours() {
        let highlighter = Highlighter::new();
        let styled = whole(
            &highlighter,
            Path::new("hello.py"),
            &lines("def greet():\n    return \"hi\""),
        );

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
        let mut highlighted = whole(&highlighter, path, &now);
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
            // Shown whole, as the view would.
            assert!(highlighted.colour_to(&highlighter, now.len()));
            let all = whole(&highlighter, path, &now);
            assert_eq!(highlighted.lines(), all.lines(), "{now:?}");
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
        let mut highlighted = whole(&highlighter, path, &now);
        now[2] = "/* opened".into();
        assert!(highlighted.update(&highlighter, &now, Some(10)));
        // The line edited, and ten more.
        assert_eq!(highlighted.stale_from(), Some(12));
        let all = whole(&highlighter, path, &now);
        assert_eq!(highlighted.lines()[..12], all.lines()[..12]);
        assert_ne!(highlighted.lines()[20], all.lines()[20]);
        // Typed on a line of its own, far below: the lines in between
        // are not forgotten.
        now[40].push('x');
        for _ in 0..5 {
            assert!(highlighted.update(&highlighter, &now, Some(10)));
        }
        let all = whole(&highlighter, path, &now);
        assert_eq!(highlighted.lines(), all.lines());
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
        let mut highlighted = whole(&highlighter, path, &now);
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
            // Shown here and there, as when scrolling.
            if next(3) == 0 {
                assert!(highlighted.colour_to(&highlighter, next(now.len() + 1)));
            }
            // What is shown is always the text, whatever its colours.
            let shown: Vec<String> = highlighted
                .lines()
                .iter()
                .map(|l| l.iter().map(|(_, t)| t.as_str()).collect())
                .collect();
            assert_eq!(shown, now);
        }
        assert!(highlighted.update(&highlighter, &now, None));
        assert!(highlighted.colour_to(&highlighter, now.len()));
        let all = whole(&highlighter, path, &now);
        assert_eq!(highlighted.lines(), all.lines());
        assert_eq!(highlighted.starts, all.starts);
    }

    /// A file opens with nothing coloured; what is shown is coloured when
    /// shown, edits past it are only taken in, and all ends as the whole.
    #[test]
    fn only_what_is_shown_is_coloured() {
        let highlighter = Highlighter::new();
        let path = Path::new("a.rs");
        let mut now: Vec<String> = (0..100).map(|i| format!("let v{i} = {i};")).collect();
        let mut highlighted = highlighter.highlight(path, &now).unwrap();
        assert!(highlighted.colour_to(&highlighter, 10));
        let all = whole(&highlighter, path, &now);
        assert_eq!(highlighted.lines()[..10], all.lines()[..10]);
        assert_eq!(highlighted.lines()[50], plain("let v50 = 50;"));

        // Past what is coloured: taken as is.
        now[60] = "/* far".into();
        now.insert(70, "*/".into());
        assert!(highlighted.update(&highlighter, &now, Some(3)));
        assert_eq!(highlighted.lines()[60], plain("/* far"));
        // Within it, reaching past it: coloured up to it.
        now[5] = "/* near".into();
        assert!(highlighted.update(&highlighter, &now, Some(3)));
        assert_eq!(highlighted.coloured, 10);
        assert!(highlighted.colour_to(&highlighter, now.len()));
        assert_eq!(highlighted.lines(), whole(&highlighter, path, &now).lines());
    }
}
