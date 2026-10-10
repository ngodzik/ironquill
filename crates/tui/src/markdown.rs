//! Just enough Markdown for model replies: fenced code blocks, inline code,
//! bold, headings and lists. Anything else is shown as written, which for
//! Markdown is readable anyway.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::pictures::{Picture, Pictures};

const CODE_BLOCK: Color = Color::Rgb(229, 192, 123);

/// The colour of code blocks, which references inside are not looked for.
pub(crate) const CODE_BLOCK_COLOR: Color = CODE_BLOCK;
const INLINE_CODE: Color = Color::Rgb(130, 170, 255);

/// The mark drawn over a code block: a click copies the block as written.
pub(crate) const COPY_MARK: &str = "⧉ copy";

/// A run of text in one style.
type Piece = (String, Style);

/// Renders `text` as lines no wider than `width`, in `base` style where
/// Markdown says nothing else.
///
/// Pictures are drawn by `pictures` where it can: a line that is only an
/// image, `![what](path.png)`, and ```mermaid blocks, which keep their copy
/// mark. Where it cannot, they stay text.
pub(crate) fn render(
    text: &str,
    width: usize,
    base: Style,
    pictures: &dyn Pictures,
) -> Vec<Line<'static>> {
    let width = width.max(1);
    let mut out = Vec::new();
    let mut in_code = false;
    // Inside a ```diff block: its lines in the colours of a diff.
    let mut in_diff = false;
    let lines: Vec<&str> = text.lines().collect();
    let mut at = 0;

    while at < lines.len() {
        let raw = lines[at];
        at += 1;
        let trimmed = raw.trim_start();
        if let Some(language) = trimmed.strip_prefix("```") {
            // An opening fence becomes the copy mark, a closing one nothing.
            in_code = !in_code;
            in_diff = in_code && language.trim().starts_with("diff");
            if in_code {
                let mut spans = vec![Span::styled(COPY_MARK, base.fg(Color::DarkGray))];
                if !language.trim().is_empty() {
                    spans.push(Span::styled(
                        format!(" · {}", language.trim()),
                        base.fg(Color::DarkGray),
                    ));
                }
                out.push(Line::from(spans));
                // A diagram drawn takes the place of its code.
                if language.trim() == "mermaid"
                    && let Some(end) = lines[at..]
                        .iter()
                        .position(|l| l.trim_start().starts_with("```"))
                {
                    let source = lines[at..at + end].join("\n");
                    if let Some(drawn) = pictures.lines(Picture::Mermaid(&source), width) {
                        out.extend(drawn);
                        at += end + 1;
                        in_code = false;
                    }
                }
            }
            continue;
        }
        if in_code {
            let style = if in_diff {
                match raw.chars().next() {
                    Some('+') => base.fg(Color::Rgb(110, 180, 120)),
                    Some('-') => base.fg(Color::Rgb(200, 110, 110)),
                    Some('@') => base.fg(Color::Cyan),
                    _ => base.fg(CODE_BLOCK),
                }
            } else {
                base.fg(CODE_BLOCK)
            };
            let chars: Vec<char> = raw.chars().collect();
            if chars.is_empty() {
                out.push(Line::default());
            }
            for chunk in chars.chunks(width) {
                out.push(Line::from(Span::styled(
                    chunk.iter().collect::<String>(),
                    style,
                )));
            }
            continue;
        }
        if trimmed.is_empty() {
            out.push(Line::default());
            continue;
        }

        if let Some(path) = image(trimmed)
            && let Some(drawn) = pictures.lines(Picture::File(path), width)
        {
            out.extend(drawn);
            continue;
        }

        let indent = raw.len() - trimmed.len();
        if let Some(heading) = heading(trimmed) {
            let bold = base.add_modifier(Modifier::BOLD);
            fill(&mut out, &inline(heading, bold), width, "", "");
        } else if let Some(item) = trimmed
            .strip_prefix("- ")
            .or_else(|| trimmed.strip_prefix("* "))
        {
            let lead = format!("{}• ", " ".repeat(indent));
            let rest = " ".repeat(lead.chars().count());
            fill(&mut out, &inline(item, base), width, &lead, &rest);
        } else {
            fill(&mut out, &inline(raw, base), width, "", "");
        }
    }
    out
}

/// The path of a line that is only a local image, `![what](path)`.
fn image(line: &str) -> Option<&str> {
    let rest = line.trim_end().strip_prefix("![")?;
    let (_, rest) = rest.split_once("](")?;
    let path = rest.strip_suffix(')')?;
    let local = !path.is_empty() && !path.contains("://") && !path.contains(char::is_whitespace);
    local.then_some(path)
}

/// The text of a `#` heading line, without its hashes.
fn heading(line: &str) -> Option<&str> {
    let hashes = line.chars().take_while(|c| *c == '#').count();
    if (1..=6).contains(&hashes) {
        line[hashes..].strip_prefix(' ')
    } else {
        None
    }
}

/// Splits one line into styled pieces at `code` and **bold** markers. A marker
/// without its closing twin is kept as written.
fn inline(line: &str, base: Style) -> Vec<Piece> {
    let mut pieces = Vec::new();
    let mut plain = String::new();
    let mut bold = false;
    let mut rest = line;

    let style_of = |bold: bool| {
        if bold {
            base.add_modifier(Modifier::BOLD)
        } else {
            base
        }
    };

    while let Some(c) = rest.chars().next() {
        if c == '`'
            && let Some(end) = rest[1..].find('`')
        {
            if !plain.is_empty() {
                pieces.push((std::mem::take(&mut plain), style_of(bold)));
            }
            pieces.push((rest[1..=end].to_owned(), style_of(bold).fg(INLINE_CODE)));
            rest = &rest[end + 2..];
            continue;
        }
        if rest.starts_with("**") && (bold || rest[2..].contains("**")) {
            if !plain.is_empty() {
                pieces.push((std::mem::take(&mut plain), style_of(bold)));
            }
            bold = !bold;
            rest = &rest[2..];
            continue;
        }
        plain.push(c);
        rest = &rest[c.len_utf8()..];
    }
    if !plain.is_empty() {
        pieces.push((plain, style_of(bold)));
    }
    pieces
}

/// Lays `pieces` out in lines of at most `width` characters, breaking at
/// spaces, with `lead` before the first line and `rest` before the others.
fn fill(out: &mut Vec<Line<'static>>, pieces: &[Piece], width: usize, lead: &str, rest: &str) {
    // Words may change style midway (`**a**b`), so a word is a list of pieces.
    let mut words: Vec<Vec<Piece>> = vec![Vec::new()];
    for (text, style) in pieces {
        for (i, part) in text.split(' ').enumerate() {
            if i > 0 {
                words.push(Vec::new());
            }
            if !part.is_empty()
                && let Some(word) = words.last_mut()
            {
                word.push((part.to_owned(), *style));
            }
        }
    }

    let room = width.saturating_sub(lead.chars().count()).max(1);
    let mut line: Vec<Span<'static>> = vec![Span::raw(lead.to_owned())];
    let mut used = 0;
    for word in words.into_iter().filter(|w| !w.is_empty()) {
        let len: usize = word.iter().map(|(t, _)| t.chars().count()).sum();
        if used > 0 && used + 1 + len > room {
            out.push(Line::from(std::mem::take(&mut line)));
            line.push(Span::raw(rest.to_owned()));
            used = 0;
        }
        if used > 0 {
            line.push(Span::raw(" "));
            used += 1;
        }
        for (text, style) in word {
            // A word longer than the whole line is cut where the line ends.
            let mut chars: Vec<char> = text.chars().collect();
            while used + chars.len() > room && used < room {
                let take = room - used;
                let head: String = chars.drain(..take).collect();
                line.push(Span::styled(head, style));
                out.push(Line::from(std::mem::take(&mut line)));
                line.push(Span::raw(rest.to_owned()));
                used = 0;
            }
            used += chars.len();
            line.push(Span::styled(chars.into_iter().collect::<String>(), style));
        }
    }
    out.push(Line::from(line));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pictures::NoPictures;
    use ironquill_ui::blocks::code_blocks;

    fn render(text: &str, width: usize, base: Style) -> Vec<Line<'static>> {
        super::render(text, width, base, &NoPictures)
    }

    fn text(lines: &[Line]) -> Vec<String> {
        lines.iter().map(ToString::to_string).collect()
    }

    #[test]
    fn fences_become_a_copy_mark_and_code_keeps_its_lines() {
        let text_in = "Run:\n```python\nprint(\"hi\")\nx = 1\n```\nDone.";
        let out = render(text_in, 40, Style::new());
        assert_eq!(
            text(&out),
            ["Run:", "⧉ copy · python", "print(\"hi\")", "x = 1", "Done."]
        );
        assert_eq!(out[2].spans[0].style.fg, Some(CODE_BLOCK));
        assert_eq!(code_blocks(text_in), ["print(\"hi\")\nx = 1"]);
    }

    #[test]
    fn diff_lines_are_coloured_by_their_sign() {
        let out = render("```diff\n-a\n+b\n```", 40, Style::new());
        assert_ne!(out[1].spans[0].style.fg, out[2].spans[0].style.fg);
    }

    #[test]
    fn inline_code_and_bold_are_styled_without_markers() {
        let out = render("Use `cargo` **now**", 40, Style::new());
        assert_eq!(text(&out), ["Use cargo now"]);
        let code = out[0].spans.iter().find(|s| s.content == "cargo").unwrap();
        assert_eq!(code.style.fg, Some(INLINE_CODE));
        let bold = out[0].spans.iter().find(|s| s.content == "now").unwrap();
        assert!(bold.style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn an_unclosed_marker_is_shown_as_written() {
        assert_eq!(
            text(&render("a ** b and `c", 40, Style::new())),
            ["a ** b and `c"]
        );
    }

    #[test]
    fn list_items_get_bullets_and_hanging_indent() {
        let out = render("- one two three four", 10, Style::new());
        assert_eq!(text(&out), ["• one two", "  three", "  four"]);
    }

    /// Draws every picture as one line naming it.
    struct Named;

    impl Pictures for Named {
        fn lines(&self, picture: Picture<'_>, _: usize) -> Option<Vec<Line<'static>>> {
            Some(vec![Line::raw(match picture {
                Picture::File(path) => format!("[{path}]"),
                Picture::Mermaid(source) => format!("[{}]", source.replace('\n', "/")),
            })])
        }
    }

    #[test]
    fn pictures_take_the_place_of_their_text_where_drawn() {
        let reply = "See:\n![plan](docs/plan.png)\n```mermaid\ngraph LR\n  A --> B\n```\nok ![inline](x.png)\n![web](https://a.b/c.png)";
        assert_eq!(
            text(&super::render(reply, 40, Style::new(), &Named)),
            [
                "See:",
                "[docs/plan.png]",
                "⧉ copy · mermaid",
                "[graph LR/  A --> B]",
                "ok ![inline](x.png)",
                "![web](https://a.b/c.png)",
            ]
        );
        // The copy mark still copies the diagram's source.
        assert_eq!(code_blocks(reply), ["graph LR\n  A --> B"]);
        // Where none is drawn, all stays text.
        assert_eq!(
            text(&render(
                "![plan](a.png)\n```mermaid\ngraph\n```",
                40,
                Style::new()
            )),
            ["![plan](a.png)", "⧉ copy · mermaid", "graph"]
        );
    }

    #[test]
    fn headings_lose_their_hashes() {
        assert_eq!(text(&render("## Plan", 40, Style::new())), ["Plan"]);
    }

    #[test]
    fn long_words_are_cut() {
        assert_eq!(
            text(&render("abcdefghij", 4, Style::new())),
            ["abcd", "efgh", "ij"]
        );
    }
}
