//! Just enough Markdown for model replies: fenced code blocks, inline code,
//! bold, headings and lists. Anything else is shown as written, which for
//! Markdown is readable anyway.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

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
pub(crate) fn render(text: &str, width: usize, base: Style) -> Vec<Line<'static>> {
    let width = width.max(1);
    let mut out = Vec::new();
    let mut in_code = false;
    // Inside a ```diff block: its lines in the colours of a diff.
    let mut in_diff = false;

    for raw in text.lines() {
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

/// The ```diff blocks of `text`, numbered: by the number after `diff`, as
/// `/address` asks for, else in order from 1.
pub(crate) fn diff_blocks(text: &str) -> Vec<(usize, String)> {
    let mut blocks = Vec::new();
    // The block being read: whether a diff, its number, its lines.
    let mut current: Option<(bool, Option<usize>, Vec<&str>)> = None;
    let mut order = 0;
    for line in text.lines() {
        if let Some(info) = line.trim_start().strip_prefix("```") {
            match current.take() {
                Some((true, number, lines)) => {
                    order += 1;
                    blocks.push((number.unwrap_or(order), lines.join("\n") + "\n"));
                }
                Some((false, _, _)) => {}
                None => {
                    let mut words = info.split_whitespace();
                    let diff = words.next() == Some("diff");
                    let number = words.next().and_then(|n| n.parse().ok());
                    current = Some((diff, number, Vec::new()));
                }
            }
        } else if let Some((_, _, lines)) = current.as_mut() {
            lines.push(line);
        }
    }
    blocks
}

/// The code blocks of `text`, as written, in order: what each copy mark
/// copies.
pub(crate) fn code_blocks(text: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut current: Option<Vec<&str>> = None;
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            match current.take() {
                Some(lines) => blocks.push(lines.join("\n")),
                None => current = Some(Vec::new()),
            }
        } else if let Some(lines) = current.as_mut() {
            lines.push(line);
        }
    }
    // A block left open still copies what it has.
    if let Some(lines) = current {
        blocks.push(lines.join("\n"));
    }
    blocks
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
    fn diff_blocks_are_numbered_as_labelled_or_in_order() {
        let text = "1.\n```diff 3\n-a\n+b\n```\n```text\nreply\n```\n```diff\n-c\n+d\n```";
        assert_eq!(
            diff_blocks(text),
            [(3, "-a\n+b\n".to_owned()), (2, "-c\n+d\n".to_owned())]
        );
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
