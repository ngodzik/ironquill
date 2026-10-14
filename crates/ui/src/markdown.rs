//! The Markdown of replies, read once for every front end: blocks
//! (headings by level, paragraphs, nested, numbered and task lists, rules,
//! quotes, fenced code, tables, images alone on a line) and, within them,
//! runs of text (bold, italic, strikethrough, inline code, links). Drawing
//! them is each front end's business.
//!
//! What is not Markdown stays as written: an `_` inside a word
//! (`snake_case`), a `*` between spaces (`2 * 3`), an image within a line,
//! a marker never closed.

/// A run of text in one style.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Span {
    /// The text, markers gone.
    pub text: String,
    /// `**bold**`.
    pub bold: bool,
    /// `*italic*` or `_italic_`.
    pub italic: bool,
    /// `~~struck~~`.
    pub strike: bool,
    /// `` `code` ``.
    pub code: bool,
    /// The target of `[text](target)`.
    pub link: Option<String>,
}

/// What marks a list item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Marker {
    /// `-`, `*` or `+`.
    Bullet,
    /// `1.` or `1)`.
    Number(u64),
    /// `- [ ]` or `- [x]`: whether it is done.
    Task(bool),
}

/// How a table's column is aligned, as its separator says.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Align {
    /// `---` or `:--`.
    #[default]
    Left,
    /// `:-:`.
    Center,
    /// `--:`.
    Right,
}

/// A block of a reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Block {
    /// `#` to `######`.
    Heading {
        /// 1 to 6.
        level: u8,
        /// Its text.
        spans: Vec<Span>,
    },
    /// Lines of text, their breaks kept.
    Paragraph(Vec<Vec<Span>>),
    /// A list item.
    Item {
        /// How deep it is nested, from 0.
        depth: usize,
        /// What marks it.
        marker: Marker,
        /// Its text.
        spans: Vec<Span>,
    },
    /// `---`, `***` or `___`.
    Rule,
    /// `>` lines.
    Quote {
        /// The quote as written, its markers gone: what a copy copies.
        text: String,
        /// The quote read again.
        blocks: Vec<Block>,
    },
    /// A fenced block.
    Code {
        /// What follows the opening fence: `rust`, `diff`, `mermaid`.
        language: String,
        /// Its lines, as written.
        text: String,
    },
    /// A table.
    Table {
        /// Its header's cells.
        header: Vec<Vec<Span>>,
        /// Each column's alignment.
        aligns: Vec<Align>,
        /// Its rows, each a cell per column.
        rows: Vec<Vec<Vec<Span>>>,
    },
    /// A line that is only an image: `![what](path)`.
    Image {
        /// What it shows.
        alt: String,
        /// Where it is.
        path: String,
    },
    /// An empty line between blocks.
    Blank,
}

/// The blocks of `text`.
#[must_use]
pub fn parse(text: &str) -> Vec<Block> {
    let lines: Vec<&str> = text.lines().collect();
    let mut blocks = Vec::new();
    let mut paragraph: Vec<Vec<Span>> = Vec::new();
    let flush = |blocks: &mut Vec<Block>, paragraph: &mut Vec<Vec<Span>>| {
        if !paragraph.is_empty() {
            blocks.push(Block::Paragraph(std::mem::take(paragraph)));
        }
    };
    let mut at = 0;
    while at < lines.len() {
        let line = lines[at];
        let trimmed = line.trim_start();
        at += 1;
        if let Some(info) = trimmed.strip_prefix("```") {
            flush(&mut blocks, &mut paragraph);
            let mut code = Vec::new();
            while at < lines.len() && !lines[at].trim_start().starts_with("```") {
                code.push(lines[at]);
                at += 1;
            }
            at += 1;
            blocks.push(Block::Code {
                language: info.trim().to_owned(),
                text: code.join("\n"),
            });
        } else if trimmed.is_empty() {
            flush(&mut blocks, &mut paragraph);
            blocks.push(Block::Blank);
        } else if trimmed.starts_with('>') {
            flush(&mut blocks, &mut paragraph);
            let mut quoted = vec![unquote(line)];
            while at < lines.len() && lines[at].trim_start().starts_with('>') {
                quoted.push(unquote(lines[at]));
                at += 1;
            }
            let text = quoted.join("\n");
            blocks.push(Block::Quote {
                blocks: parse(&text),
                text,
            });
        } else if let Some((level, title)) = heading(trimmed) {
            flush(&mut blocks, &mut paragraph);
            blocks.push(Block::Heading {
                level,
                spans: inline(title),
            });
        } else if rule(trimmed) {
            flush(&mut blocks, &mut paragraph);
            blocks.push(Block::Rule);
        } else if let Some((alt, path)) = image(trimmed) {
            flush(&mut blocks, &mut paragraph);
            blocks.push(Block::Image {
                alt: alt.to_owned(),
                path: path.to_owned(),
            });
        } else if let Some((marker, rest)) = item(trimmed) {
            flush(&mut blocks, &mut paragraph);
            let indent = line.len() - trimmed.len();
            blocks.push(Block::Item {
                depth: indent / 2,
                marker,
                spans: inline(rest),
            });
        } else if line.contains('|')
            && let Some(aligns) = lines.get(at).and_then(|next| separator(next))
        {
            flush(&mut blocks, &mut paragraph);
            let header = cells(line);
            let width = header.len().max(aligns.len());
            at += 1;
            let mut rows = Vec::new();
            while at < lines.len() && lines[at].contains('|') && !lines[at].trim().is_empty() {
                let mut row = cells(lines[at]);
                row.resize(width, Vec::new());
                rows.push(row);
                at += 1;
            }
            let mut header = header;
            header.resize(width, Vec::new());
            let mut aligns = aligns;
            aligns.resize(width, Align::Left);
            blocks.push(Block::Table {
                header,
                aligns,
                rows,
            });
        } else {
            paragraph.push(inline(line.trim_end()));
        }
    }
    flush(&mut blocks, &mut paragraph);
    blocks
}

/// A quoted line without its first `>` and the space after it.
fn unquote(line: &str) -> &str {
    let line = line.trim_start();
    let line = line.strip_prefix('>').unwrap_or(line);
    line.strip_prefix(' ').unwrap_or(line)
}

/// A heading's level and text: hashes, then a space.
fn heading(line: &str) -> Option<(u8, &str)> {
    let hashes = line.chars().take_while(|c| *c == '#').count();
    if !(1..=6).contains(&hashes) {
        return None;
    }
    let title = line[hashes..].strip_prefix(' ')?;
    Some((hashes as u8, title.trim().trim_end_matches('#').trim_end()))
}

/// Whether a line is a thematic break: three or more of one of `-*_`.
fn rule(line: &str) -> bool {
    let marks: String = line.chars().filter(|c| !c.is_whitespace()).collect();
    marks.len() >= 3
        && ['-', '*', '_']
            .iter()
            .any(|m| marks.chars().all(|c| c == *m))
}

/// A line that is only an image: its text and its path.
fn image(line: &str) -> Option<(&str, &str)> {
    let rest = line.trim_end().strip_prefix("![")?;
    let (alt, rest) = rest.split_once("](")?;
    let path = rest.strip_suffix(')')?;
    (!path.is_empty() && !path.contains(char::is_whitespace)).then_some((alt, path))
}

/// A list item's marker and text.
fn item(line: &str) -> Option<(Marker, &str)> {
    for bullet in ["- ", "* ", "+ "] {
        if let Some(rest) = line.strip_prefix(bullet) {
            for (box_, done) in [("[ ] ", false), ("[x] ", true), ("[X] ", true)] {
                if let Some(task) = rest.strip_prefix(box_) {
                    return Some((Marker::Task(done), task));
                }
            }
            return Some((Marker::Bullet, rest));
        }
    }
    let digits = line.chars().take_while(char::is_ascii_digit).count();
    if (1..=9).contains(&digits) {
        let rest = &line[digits..];
        if let Some(text) = rest.strip_prefix(". ").or_else(|| rest.strip_prefix(") ")) {
            let number = line[..digits].parse().ok()?;
            return Some((Marker::Number(number), text));
        }
    }
    None
}

/// A table's separator line: each column's alignment.
fn separator(line: &str) -> Option<Vec<Align>> {
    let line = line.trim();
    if !line.contains('-') || !line.contains('|') && !line.starts_with(':') {
        return None;
    }
    let inner = line.trim_start_matches('|').trim_end_matches('|');
    inner
        .split('|')
        .map(|cell| {
            let cell = cell.trim();
            let dashes = cell.trim_matches(':');
            if dashes.len() < 3 || !dashes.chars().all(|c| c == '-') {
                return None;
            }
            Some(match (cell.starts_with(':'), cell.ends_with(':')) {
                (true, true) => Align::Center,
                (false, true) => Align::Right,
                _ => Align::Left,
            })
        })
        .collect()
}

/// A table line's cells, read as text.
fn cells(line: &str) -> Vec<Vec<Span>> {
    let line = line.trim();
    let inner = line.strip_prefix('|').unwrap_or(line);
    let inner = inner.strip_suffix('|').unwrap_or(inner);
    // A `|` escaped, or inside code, is not a border.
    let mut out = Vec::new();
    let mut cell = String::new();
    let mut code = false;
    let mut chars = inner.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' if chars.peek() == Some(&'|') => {
                cell.push('|');
                chars.next();
            }
            '`' => {
                code = !code;
                cell.push(c);
            }
            '|' if !code => out.push(inline(std::mem::take(&mut cell).trim())),
            _ => cell.push(c),
        }
    }
    out.push(inline(cell.trim()));
    out
}

/// What styles a run, while reading one.
#[derive(Clone, Copy, Default)]
struct Style {
    bold: bool,
    italic: bool,
    strike: bool,
}

/// The runs of one line of text.
#[must_use]
pub fn inline(text: &str) -> Vec<Span> {
    let mut out = Vec::new();
    read(text, Style::default(), &mut out);
    // Neighbours in the same style are one run.
    let mut merged: Vec<Span> = Vec::new();
    for span in out.into_iter().filter(|s| !s.text.is_empty()) {
        match merged.last_mut() {
            Some(last)
                if last.bold == span.bold
                    && last.italic == span.italic
                    && last.strike == span.strike
                    && last.code == span.code
                    && last.link == span.link =>
            {
                last.text.push_str(&span.text);
            }
            _ => merged.push(span),
        }
    }
    merged
}

fn push(out: &mut Vec<Span>, text: &str, style: Style, code: bool, link: Option<&str>) {
    out.push(Span {
        text: text.to_owned(),
        bold: style.bold,
        italic: style.italic,
        strike: style.strike,
        code,
        link: link.map(str::to_owned),
    });
}

/// Whether `c` is part of a word, for `_` and the edges of emphasis.
fn wordy(c: Option<char>) -> bool {
    c.is_some_and(char::is_alphanumeric)
}

/// Where the closing `marker` of an emphasis opened at the start of `rest`
/// (after the marker) is: not after a space, and for `_`, not inside a word.
fn closing(rest: &str, marker: &str) -> Option<usize> {
    let mut from = 0;
    while let Some(found) = rest[from..].find(marker) {
        let mut at = from + found;
        // `***`: a double marker closes on the last two of the run.
        if marker.len() == 2 {
            let mark = marker.chars().next().unwrap_or('*');
            while rest[at + marker.len()..].starts_with(mark) {
                at += 1;
            }
        }
        let before = rest[..at].chars().last();
        let after = rest[at + marker.len()..].chars().next();
        let underscore = marker.starts_with('_');
        if at > 0
            && before.is_some_and(|c| !c.is_whitespace())
            && !(underscore && wordy(after))
            // `**` inside `***` is not a single `*`.
            && !(marker == "*" && rest[at + 1..].starts_with('*') && !rest[at + 2..].is_empty())
        {
            return Some(at);
        }
        from = at + marker.len();
    }
    None
}

/// Reads `text` in `style` into `out`.
fn read(text: &str, style: Style, out: &mut Vec<Span>) {
    let mut plain = String::new();
    let mut at = 0;
    let mut previous: Option<char> = None;
    while at < text.len() {
        let rest = &text[at..];
        let c = rest.chars().next().unwrap_or(' ');
        let next = rest[c.len_utf8()..].chars().next();
        // `code`: nothing inside it is read.
        if c == '`'
            && let Some(end) = rest[1..].find('`')
        {
            push(out, &std::mem::take(&mut plain), style, false, None);
            push(out, &rest[1..=end], style, true, None);
            at += end + 2;
            previous = Some('`');
            continue;
        }
        // An image within a line stays as written.
        if c == '!' && next == Some('[') {
            plain.push_str("![");
            at += 2;
            previous = Some('[');
            continue;
        }
        // [text](target)
        if c == '['
            && let Some(close) = rest.find("](")
            && let Some(end) = rest[close + 2..].find(')')
        {
            let label = &rest[1..close];
            let target = &rest[close + 2..close + 2 + end];
            if !target.contains(char::is_whitespace) {
                push(out, &std::mem::take(&mut plain), style, false, None);
                let mut inner = Vec::new();
                read(label, style, &mut inner);
                for mut span in inner {
                    span.link = Some(target.to_owned());
                    out.push(span);
                }
                at += close + 3 + end;
                previous = Some(')');
                continue;
            }
        }
        // Emphasis: the opener is followed by a non-space, and `_` does not
        // open inside a word.
        let marker = ["**", "__", "~~", "*", "_"]
            .into_iter()
            .find(|m| rest.starts_with(m));
        if let Some(marker) = marker {
            let after = rest[marker.len()..].chars().next();
            let opens = after.is_some_and(|a| !a.is_whitespace())
                && !(marker.starts_with('_') && wordy(previous));
            if opens && let Some(end) = closing(&rest[marker.len()..], marker) {
                push(out, &std::mem::take(&mut plain), style, false, None);
                let inner = &rest[marker.len()..marker.len() + end];
                let mut inner_style = style;
                match marker {
                    "**" | "__" => inner_style.bold = true,
                    "~~" => inner_style.strike = true,
                    _ => inner_style.italic = true,
                }
                read(inner, inner_style, out);
                at += marker.len() * 2 + end;
                previous = marker.chars().last();
                continue;
            }
            // Not emphasis: written as it is, the whole marker at once.
            plain.push_str(marker);
            at += marker.len();
            previous = marker.chars().last();
            continue;
        }
        plain.push(c);
        at += c.len_utf8();
        previous = Some(c);
    }
    push(out, &plain, style, false, None);
}

/// The plain text of runs, markers gone.
#[must_use]
pub fn plain(spans: &[Span]) -> String {
    spans.iter().map(|s| s.text.as_str()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn styled(spans: &[Span]) -> Vec<(String, &'static str)> {
        spans
            .iter()
            .map(|s| {
                let kind = match (s.bold, s.italic, s.strike, s.code, s.link.is_some()) {
                    (_, _, _, true, _) => "code",
                    (_, _, _, _, true) => "link",
                    (true, true, _, _, _) => "bold italic",
                    (true, _, _, _, _) => "bold",
                    (_, true, _, _, _) => "italic",
                    (_, _, true, _, _) => "strike",
                    _ => "plain",
                };
                (s.text.clone(), kind)
            })
            .collect()
    }

    #[test]
    fn runs_take_their_styles_and_lose_their_markers() {
        assert_eq!(
            styled(&inline(
                "a **b** *c* ~~d~~ `e` [f](https://x.example.com) ***g***"
            )),
            [
                ("a ".into(), "plain"),
                ("b".into(), "bold"),
                (" ".into(), "plain"),
                ("c".into(), "italic"),
                (" ".into(), "plain"),
                ("d".into(), "strike"),
                (" ".into(), "plain"),
                ("e".into(), "code"),
                (" ".into(), "plain"),
                ("f".into(), "link"),
                (" ".into(), "plain"),
                ("g".into(), "bold italic"),
            ]
        );
    }

    #[test]
    fn what_is_not_markdown_stays_as_written() {
        for text in [
            "snake_case_name and a_b_c files",
            "2 * 3 * 4",
            "see ![shot](a.png) here",
            "a ** b and `c",
            "under_scored_ words",
        ] {
            assert_eq!(plain(&inline(text)), text, "{text}");
            assert_eq!(inline(text).len(), 1, "{text}: {:?}", inline(text));
        }
        assert_eq!(styled(&inline("_it_")), [("it".into(), "italic")]);
    }

    #[test]
    fn blocks_are_told_apart() {
        let text = "# Title\n## Sub\n\nSome text\non two lines.\n\n- a\n  - nested\n1. first\n2) second\n- [ ] todo\n- [x] done\n\n---\n> quoted **bold**\n> more\n\n```rust\nfn x() {}\n```\n![plan](docs/plan.png)";
        let blocks: Vec<Block> = parse(text)
            .into_iter()
            .filter(|b| *b != Block::Blank)
            .collect();
        assert!(
            matches!(&blocks[0], Block::Heading { level: 1, spans } if plain(spans) == "Title")
        );
        assert!(matches!(&blocks[1], Block::Heading { level: 2, .. }));
        assert!(matches!(&blocks[2], Block::Paragraph(lines) if lines.len() == 2));
        assert!(matches!(
            &blocks[3],
            Block::Item {
                depth: 0,
                marker: Marker::Bullet,
                ..
            }
        ));
        assert!(matches!(
            &blocks[4],
            Block::Item {
                depth: 1,
                marker: Marker::Bullet,
                ..
            }
        ));
        assert!(matches!(
            &blocks[5],
            Block::Item {
                marker: Marker::Number(1),
                ..
            }
        ));
        assert!(matches!(
            &blocks[6],
            Block::Item {
                marker: Marker::Number(2),
                ..
            }
        ));
        assert!(matches!(
            &blocks[7],
            Block::Item {
                marker: Marker::Task(false),
                ..
            }
        ));
        assert!(matches!(
            &blocks[8],
            Block::Item {
                marker: Marker::Task(true),
                ..
            }
        ));
        assert_eq!(blocks[9], Block::Rule);
        assert!(
            matches!(&blocks[10], Block::Quote { text, blocks } if text == "quoted **bold**\nmore" && blocks.len() == 1)
        );
        assert!(
            matches!(&blocks[11], Block::Code { language, text } if language == "rust" && text == "fn x() {}")
        );
        assert!(matches!(&blocks[12], Block::Image { path, .. } if path == "docs/plan.png"));
    }

    #[test]
    fn a_table_has_its_header_alignments_and_rows() {
        let text = "| Name | Size | Note |\n|:-----|-----:|:----:|\n| a | 1 | `x|y` |\n| b \\| c | 22 |\nafter";
        let blocks = parse(text);
        let Block::Table {
            header,
            aligns,
            rows,
        } = &blocks[0]
        else {
            panic!("a table: {blocks:?}");
        };
        assert_eq!(
            header.iter().map(|c| plain(c)).collect::<Vec<_>>(),
            ["Name", "Size", "Note"]
        );
        assert_eq!(*aligns, [Align::Left, Align::Right, Align::Center]);
        assert_eq!(plain(&rows[0][2]), "x|y");
        assert_eq!(plain(&rows[1][0]), "b | c");
        assert!(rows[1][2].is_empty());
        assert!(matches!(&blocks[1], Block::Paragraph(_)));
    }
}
