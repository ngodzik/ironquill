//! Replies' Markdown in the terminal, as `universe_ui::markdown` reads it:
//! headings by level, lists nested, numbered or ticked, quotes behind a bar,
//! rules, fenced code with a copy mark, tables boxed and aligned with wide
//! columns wrapped, and pictures where the terminal draws them.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use universe_ui::markdown::{self, Align, Block, Marker};

use crate::pictures::{Picture, Pictures};

const CODE_BLOCK: Color = Color::Rgb(229, 192, 123);

/// The colour of code blocks, which references inside are not looked for.
pub(crate) const CODE_BLOCK_COLOR: Color = CODE_BLOCK;
const INLINE_CODE: Color = Color::Rgb(130, 170, 255);
const LINK: Color = Color::Rgb(86, 182, 194);
const FRAME: Color = Color::DarkGray;

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
    let mut out = Vec::new();
    blocks(
        &markdown::parse(text),
        width.max(1),
        base,
        pictures,
        &mut out,
    );
    out
}

fn blocks(
    list: &[Block],
    width: usize,
    base: Style,
    pictures: &dyn Pictures,
    out: &mut Vec<Line<'static>>,
) {
    for block in list {
        match block {
            Block::Blank => out.push(Line::default()),
            Block::Heading { level, spans } => {
                let style = match level {
                    1 => base.add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
                    2 => base.add_modifier(Modifier::BOLD),
                    _ => base.add_modifier(Modifier::BOLD).fg(Color::Gray),
                };
                fill(out, &pieces(spans, style), width, "", "");
            }
            Block::Paragraph(lines) => {
                for line in lines {
                    fill(out, &pieces(line, base), width, "", "");
                }
            }
            Block::Item {
                depth,
                marker,
                spans,
            } => {
                let mark = match marker {
                    Marker::Bullet => ["•", "◦", "▪"][depth % 3].to_owned(),
                    Marker::Number(n) => format!("{n}."),
                    Marker::Task(true) => "☑".to_owned(),
                    Marker::Task(false) => "☐".to_owned(),
                };
                let lead = format!("{}{mark} ", "  ".repeat(*depth));
                let rest = " ".repeat(lead.chars().count());
                let style = if matches!(marker, Marker::Task(true)) {
                    base.fg(Color::DarkGray)
                } else {
                    base
                };
                fill(out, &pieces(spans, style), width, &lead, &rest);
            }
            Block::Rule => out.push(Line::styled("─".repeat(width.min(60)), base.fg(FRAME))),
            Block::Quote { blocks: inner, .. } => {
                let mut quoted = Vec::new();
                blocks(
                    inner,
                    width.saturating_sub(2).max(1),
                    base.fg(Color::Gray),
                    pictures,
                    &mut quoted,
                );
                for mut line in quoted {
                    line.spans.insert(0, Span::styled("│ ", base.fg(FRAME)));
                    out.push(line);
                }
            }
            Block::Code { language, text } => code(language, text, width, base, pictures, out),
            // Only a local file is drawn: nothing is fetched from the web.
            Block::Image { alt, path } => match (!path.contains("://"))
                .then(|| pictures.lines(Picture::File(path), width))
                .flatten()
            {
                Some(drawn) => out.extend(drawn),
                None => fill(out, &[(format!("![{alt}]({path})"), base)], width, "", ""),
            },
            Block::Table {
                header,
                aligns,
                rows,
            } => table(header, aligns, rows, width, base, out),
        }
    }
}

/// A fenced block: its copy mark, then its lines; a diff coloured by sign,
/// a diagram drawn in its place where the terminal can.
fn code(
    language: &str,
    text: &str,
    width: usize,
    base: Style,
    pictures: &dyn Pictures,
    out: &mut Vec<Line<'static>>,
) {
    let mut spans = vec![Span::styled(COPY_MARK, base.fg(Color::DarkGray))];
    if !language.is_empty() {
        spans.push(Span::styled(
            format!(" · {language}"),
            base.fg(Color::DarkGray),
        ));
    }
    out.push(Line::from(spans));
    if language == "mermaid"
        && let Some(drawn) = pictures.lines(Picture::Mermaid(text), width)
    {
        out.extend(drawn);
        return;
    }
    let diff = language.starts_with("diff");
    for raw in text.lines() {
        let style = if diff {
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
    }
}

/// A table in a box, its columns aligned; when wider than `width`, the
/// widest columns narrow and their cells wrap.
fn table(
    header: &[Vec<markdown::Span>],
    aligns: &[Align],
    rows: &[Vec<Vec<markdown::Span>>],
    width: usize,
    base: Style,
    out: &mut Vec<Line<'static>>,
) {
    let columns = header.len();
    if columns == 0 {
        return;
    }
    let text = |cell: &[markdown::Span]| markdown::plain(cell);
    let mut widths: Vec<usize> = (0..columns)
        .map(|c| {
            std::iter::once(&header[c])
                .chain(rows.iter().map(|r| &r[c]))
                .map(|cell| text(cell).chars().count())
                .max()
                .unwrap_or(0)
                .max(1)
        })
        .collect();
    // Borders and padding: `│ a │ b │`.
    let room = width.saturating_sub(3 * columns + 1).max(columns);
    while widths.iter().sum::<usize>() > room {
        let (widest, _) = widths
            .iter()
            .enumerate()
            .max_by_key(|(_, w)| **w)
            .unwrap_or((0, &0));
        if widths[widest] <= 3 {
            break;
        }
        widths[widest] -= 1;
    }
    let frame = base.fg(FRAME);
    let border = |left: &str, middle: &str, right: &str| {
        let parts: Vec<String> = widths.iter().map(|w| "─".repeat(w + 2)).collect();
        Line::styled(format!("{left}{}{right}", parts.join(middle)), frame)
    };
    out.push(border("┌", "┬", "┐"));
    let row = |out: &mut Vec<Line<'static>>, cells: &[Vec<markdown::Span>], style: Style| {
        let wrapped: Vec<Vec<Vec<Piece>>> = cells
            .iter()
            .zip(&widths)
            .map(|(cell, w)| {
                let mut lines = Vec::new();
                fill(&mut lines, &pieces(cell, style), *w, "", "");
                lines
                    .into_iter()
                    .map(|l| {
                        l.spans
                            .into_iter()
                            .map(|s| (s.content.into_owned(), s.style))
                            .collect()
                    })
                    .collect()
            })
            .collect();
        let height = wrapped.iter().map(Vec::len).max().unwrap_or(1);
        for line in 0..height {
            let mut spans = vec![Span::styled("│", frame)];
            for (c, cell) in wrapped.iter().enumerate() {
                let parts = cell.get(line).cloned().unwrap_or_default();
                let used: usize = parts.iter().map(|(t, _)| t.chars().count()).sum();
                let gap = widths[c].saturating_sub(used);
                let (left, right) = match aligns.get(c).copied().unwrap_or_default() {
                    Align::Left => (0, gap),
                    Align::Right => (gap, 0),
                    Align::Center => (gap / 2, gap - gap / 2),
                };
                spans.push(Span::raw(format!(" {}", " ".repeat(left))));
                spans.extend(parts.into_iter().map(|(t, s)| Span::styled(t, s)));
                spans.push(Span::raw(format!("{} ", " ".repeat(right))));
                spans.push(Span::styled("│", frame));
            }
            out.push(Line::from(spans));
        }
    };
    row(out, header, base.add_modifier(Modifier::BOLD));
    out.push(border("├", "┼", "┤"));
    for cells in rows {
        row(out, cells, base);
    }
    out.push(border("└", "┴", "┘"));
}

/// The runs of the shared reader as styled pieces.
fn pieces(spans: &[markdown::Span], base: Style) -> Vec<Piece> {
    spans
        .iter()
        .map(|span| {
            let mut style = base;
            if span.bold {
                style = style.add_modifier(Modifier::BOLD);
            }
            if span.italic {
                style = style.add_modifier(Modifier::ITALIC);
            }
            if span.strike {
                style = style.add_modifier(Modifier::CROSSED_OUT);
            }
            if span.code {
                style = style.fg(INLINE_CODE);
            } else if span.link.is_some() {
                style = style.fg(LINK).add_modifier(Modifier::UNDERLINED);
            }
            (span.text.clone(), style)
        })
        .collect()
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
    use universe_ui::blocks::code_blocks;

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
    fn lists_quotes_rules_and_tables_are_drawn() {
        let out = render(
            "1. first\n- [x] done\n  - nested\n---\n> quoted",
            40,
            Style::new(),
        );
        assert_eq!(
            text(&out),
            [
                "1. first",
                "☑ done",
                "  ◦ nested",
                &"─".repeat(40),
                "│ quoted"
            ]
        );
        let table = render("| a | bb |\n|---|---:|\n| x | 1 |", 40, Style::new());
        assert_eq!(
            text(&table),
            [
                "┌───┬────┐",
                "│ a │ bb │",
                "├───┼────┤",
                "│ x │  1 │",
                "└───┴────┘"
            ]
        );
        // Too wide: the widest column narrows and its cells wrap.
        let wide = render(
            "| k | value |\n|---|---|\n| a | one two three four |",
            16,
            Style::new(),
        );
        assert!(wide.iter().all(|l| l.width() <= 16), "{:?}", text(&wide));
        assert!(wide.len() > 5);
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
