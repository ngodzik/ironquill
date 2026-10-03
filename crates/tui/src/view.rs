use ironquill_tools::{Container, DiffLine, LineMark, ToolSummary};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Padding, Paragraph};

use crate::app::{App, Entry, LineEditor, Panes};
use crate::editor::{Editor, EditorMode};
use crate::keymap::{Focus, Mode, Pending, SHORTCUTS};
use crate::markdown;
use crate::sessions;
use crate::wrap::wrap;

const ACCENT: Color = Color::Rgb(215, 119, 87);
// Changes since the last commit, in the open file. Backgrounds stay dark and
// soft so that the code's own colours remain readable on top of them.
const ADDED_SIGN: Color = Color::Rgb(110, 180, 120);
const ADDED_BG: Color = Color::Rgb(26, 44, 32);
const CHANGED_SIGN: Color = Color::Rgb(205, 170, 90);
const CHANGED_BG: Color = Color::Rgb(46, 41, 24);
const REMOVED_SIGN: Color = Color::Rgb(200, 110, 110);
const REMOVED_FG: Color = Color::Rgb(175, 125, 125);
const REMOVED_BG: Color = Color::Rgb(46, 26, 28);
const DIM: Color = Color::DarkGray;
const SPINNER: [&str; 6] = ["·", "✢", "✳", "✶", "✻", "✽"];
/// Diff lines shown under an edit before the rest is summarized.
const DIFF_LINES: usize = 12;
/// Lines of a failing check's output shown in the transcript. The model gets
/// more; the person gets the gist and can run the command for the rest.
const EXCERPT_LINES: usize = 6;
const PLACEHOLDER: &str = "Ask a question or describe a change";

/// Draws the whole interface: panes, activity line, input box, status line.
pub(crate) fn render(frame: &mut Frame, app: &App) {
    let [main, activity, input, status] = Layout::vertical([
        Constraint::Min(1),
        Constraint::Length(1),
        Constraint::Length(3),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    render_panes(frame, app, main);
    render_activity(frame, app, activity);
    render_input(frame, app, input);
    render_status(frame, app, status);
    if app.picker().is_some() {
        render_picker(frame, app);
    }
    if app.model_picker().is_some() {
        render_model_picker(frame, app);
    }
    if app.keys_open().is_some() {
        render_keys(frame, app);
    }
}

/// Every shortcut (Ctrl-S), over everything else, grouped by where it works.
fn render_keys(frame: &mut Frame, app: &App) {
    let Some(offset) = app.keys_open() else {
        return;
    };
    let width_keys = SHORTCUTS
        .iter()
        .flat_map(|(_, keys)| keys.iter().map(|(k, _)| k.chars().count()))
        .max()
        .unwrap_or(0);
    let mut lines: Vec<Line> = Vec::new();
    for (group, keys) in SHORTCUTS {
        if !lines.is_empty() {
            lines.push(Line::default());
        }
        lines.push(Line::styled(
            format!(" {group}"),
            fg(ACCENT).add_modifier(Modifier::BOLD),
        ));
        for (keys, what) in *keys {
            lines.push(Line::from(vec![
                Span::styled(format!("   {keys:<width_keys$}  "), fg(Color::Gray)),
                Span::raw(*what),
            ]));
        }
    }

    let screen = frame.area();
    let width = (screen.width * 4 / 5).clamp(30, 90).min(screen.width);
    let height = (lines.len() as u16 + 2).min(screen.height);
    let area = Rect::new(
        screen.x + (screen.width - width) / 2,
        screen.y + (screen.height - height) / 2,
        width,
        height,
    );
    frame.render_widget(Clear, area);
    let block = pane_block(" Shortcuts ".into(), true);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let visible = usize::from(inner.height);
    let offset = offset.min(lines.len().saturating_sub(visible));
    let shown: Vec<Line> = lines.into_iter().skip(offset).take(visible).collect();
    frame.render_widget(Paragraph::new(shown), inner);
}

/// The model picker (Ctrl-E), over everything else.
fn render_model_picker(frame: &mut Frame, app: &App) {
    let Some(selected) = app.model_picker() else {
        return;
    };
    let models = app.models();
    let screen = frame.area();
    let width = (screen.width * 3 / 5).clamp(30, 80).min(screen.width);
    let height = (models.len() as u16 + 2).clamp(3, screen.height.saturating_sub(4).max(3));
    let area = Rect::new(
        screen.x + (screen.width - width) / 2,
        screen.y + screen.height.saturating_sub(height) / 3,
        width,
        height,
    );
    frame.render_widget(Clear, area);
    let block = pane_block(" Model ".into(), true);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let room = usize::from(inner.width);
    let current = app.current_model();
    let lines: Vec<Line> = models
        .iter()
        .enumerate()
        .map(|(i, model)| {
            let mark = if Some(model) == current { "● " } else { "  " };
            let kind = if model.delegate().is_some() {
                "Claude Code · subscription"
            } else {
                "API · pay per request"
            };
            let name = format!("{mark}{model}");
            let gap = room.saturating_sub(name.chars().count() + kind.chars().count() + 1);
            let style = if i == selected {
                Style::new().add_modifier(Modifier::REVERSED)
            } else {
                Style::new()
            };
            Line::from(vec![
                Span::styled(format!("{name}{}", " ".repeat(gap)), style),
                Span::styled(kind, style.fg(DIM)),
            ])
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), inner);
}

/// The `/resume` list, over everything else.
fn render_picker(frame: &mut Frame, app: &App) {
    let Some(picker) = app.picker() else {
        return;
    };
    let screen = frame.area();
    let width = (screen.width * 4 / 5).clamp(30, 100).min(screen.width);
    let height = (picker.items.len() as u16 + 2).clamp(3, screen.height.saturating_sub(4).max(3));
    let area = Rect::new(
        screen.x + (screen.width - width) / 2,
        screen.y + (screen.height.saturating_sub(height)) / 3,
        width,
        height,
    );
    frame.render_widget(Clear, area);
    let block = pane_block(" Resume a conversation ".into(), true);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let now = sessions::now();
    let rows = usize::from(inner.height);
    let first = picker.selected.saturating_sub(rows.saturating_sub(1));
    let room = usize::from(inner.width);
    let lines: Vec<Line> = picker
        .items
        .iter()
        .enumerate()
        .skip(first)
        .take(rows)
        .map(|(i, item)| {
            let details = format!(
                "{} · {} request{} · {}",
                sessions::ago(item.updated, now),
                item.requests,
                if item.requests == 1 { "" } else { "s" },
                item.cost
            );
            let name_room = room.saturating_sub(details.chars().count() + 3);
            let name: String = item.name.chars().take(name_room).collect();
            let gap = room.saturating_sub(name.chars().count() + details.chars().count() + 1);
            let style = if i == picker.selected {
                Style::new().add_modifier(Modifier::REVERSED)
            } else {
                Style::new()
            };
            Line::from(vec![
                Span::styled(format!(" {name}{}", " ".repeat(gap)), style),
                Span::styled(details, style.fg(DIM)),
            ])
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), inner);
}

/// Narrowest the conversation may get beside an open file before it is
/// hidden instead; below this its lines wrap into noise.
const SIDE_CHAT_MIN: u16 = 36;

/// Tree on the left if open; the open file or the conversation in the middle;
/// the conversation on the right while a file is open, if there is room.
fn render_panes(frame: &mut Frame, app: &App, area: Rect) {
    if app.is_zoomed() {
        // Only the conversation; the other panes keep their state, unseen.
        app.set_panes(Panes {
            chat: area,
            ..Panes::default()
        });
        render_chat(frame, app, area, false);
        return;
    }
    // The Docker pane runs along the bottom, under everything else, sized to
    // its containers but never more than a third of the screen.
    let (area, docker) = match app.docker() {
        Some(pane) => {
            let wanted = pane.containers.len().max(1) as u16 + 3;
            let height = wanted.clamp(5, (area.height / 3).max(5));
            let [top, bottom] =
                Layout::vertical([Constraint::Min(5), Constraint::Length(height)]).areas(area);
            (top, Some(bottom))
        }
        None => (area, None),
    };
    let tree_width = if app.tree().is_some() {
        (area.width / 4).clamp(20, 34)
    } else {
        0
    };
    let rest = area.width.saturating_sub(tree_width);
    // Beside an open file the conversation takes two fifths, but never less
    // than it needs to stay readable, and only if the file keeps as much.
    let side_width = if app.file().is_some() && rest >= 2 * SIDE_CHAT_MIN + 10 {
        (rest * 2 / 5).max(SIDE_CHAT_MIN)
    } else {
        0
    };
    let [tree, center, side] = Layout::horizontal([
        Constraint::Length(tree_width),
        Constraint::Min(1),
        Constraint::Length(side_width),
    ])
    .areas(area);

    let alone = app.tree().is_none() && app.file().is_none();
    let mut panes = Panes {
        tree: (tree_width > 0).then_some(tree),
        file: None,
        chat: center,
        docker,
    };
    if let Some(docker) = docker {
        render_docker(frame, app, docker);
    }
    if tree_width > 0 {
        render_tree(frame, app, tree);
    }
    if app.file().is_some() {
        // The command frame sits under the file, as Vim's command line does.
        let [file, command] =
            Layout::vertical([Constraint::Min(1), Constraint::Length(3)]).areas(center);
        panes.file = Some(file);
        render_file(frame, app, file);
        render_command_frame(frame, app, command);
        panes.chat = side;
        if side_width > 0 {
            render_chat(frame, app, side, true);
        }
    } else {
        render_chat(frame, app, center, !alone);
    }
    app.set_panes(panes);
}

/// The border of a pane, bright when it has the focus.
fn pane_block(title: String, focused: bool) -> Block<'static> {
    let color = if focused { ACCENT } else { DIM };
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(fg(color))
        .title(Span::styled(title, fg(color)))
}

fn render_docker(frame: &mut Frame, app: &App, area: Rect) {
    let Some(pane) = app.docker() else {
        return;
    };
    let focused = app.focus() == Focus::Docker;
    let title = if pane.loaded && pane.error.is_none() {
        format!(" Docker · {} running ", pane.containers.len())
    } else {
        " Docker ".to_owned()
    };
    let block = pane_block(title, focused);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let note =
        |text: &str, color: Color| Paragraph::new(Line::styled(format!(" {text}"), fg(color)));
    if let Some(error) = &pane.error {
        frame.render_widget(note(error, Color::Red), inner);
        return;
    }
    if !pane.loaded {
        frame.render_widget(note("Asking docker…", DIM), inner);
        return;
    }
    if pane.containers.is_empty() {
        frame.render_widget(note("No container running", DIM), inner);
        return;
    }

    // Columns sized to their content, the ports taking whatever is left.
    let width = |pick: fn(&Container) -> &str, header: &str| {
        pane.containers
            .iter()
            .map(|c| pick(c).chars().count())
            .chain([header.chars().count()])
            .max()
            .unwrap_or(0)
            .min(32)
    };
    let name_w = width(|c| &c.name, "NAME");
    let image_w = width(|c| &c.image, "IMAGE");
    let status_w = width(|c| &c.status, "STATUS");
    let room = usize::from(inner.width);
    let row = |name: &str, image: &str, status: &str, ports: &str| {
        let cut = |s: &str, w: usize| format!("{:<w$}", s.chars().take(w).collect::<String>());
        let text = format!(
            " {}  {}  {}  {ports}",
            cut(name, name_w),
            cut(image, image_w),
            cut(status, status_w)
        );
        format!("{:<room$}", text.chars().take(room).collect::<String>())
    };

    let mut lines = vec![Line::styled(
        row("NAME", "IMAGE", "STATUS", "PORTS"),
        fg(DIM),
    )];
    let rows = usize::from(inner.height).saturating_sub(1);
    let first = pane.selected.saturating_sub(rows.saturating_sub(1));
    for (i, c) in pane.containers.iter().enumerate().skip(first).take(rows) {
        let healthy = !c.status.contains("unhealthy") && c.status.starts_with("Up");
        let mut style = fg(if healthy { Color::Green } else { Color::Yellow });
        if focused && i == pane.selected {
            style = style.add_modifier(Modifier::REVERSED);
        }
        lines.push(Line::styled(
            row(&c.name, &c.image, &c.status, &c.ports),
            style,
        ));
    }
    frame.render_widget(Paragraph::new(lines), inner);
}

fn render_chat(frame: &mut Frame, app: &App, area: Rect, framed: bool) {
    if framed {
        let block = pane_block(" Chat ".into(), app.focus() == Focus::Chat);
        let inner = block.inner(area);
        frame.render_widget(block, area);
        render_transcript(frame, app, inner);
    } else {
        render_transcript(frame, app, area);
    }
}

fn render_tree(frame: &mut Frame, app: &App, area: Rect) {
    let Some(tree) = app.tree() else {
        return;
    };
    let focused = app.focus() == Focus::Tree;
    let block = pane_block(" Files ".into(), focused);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Keep the selection on screen, moving the window as little as possible.
    let height = usize::from(inner.height).max(1);
    let mut offset = tree.offset();
    if tree.selected() < offset {
        offset = tree.selected();
    } else if tree.selected() >= offset + height {
        offset = tree.selected() + 1 - height;
    }
    tree.set_offset(offset);

    let width = usize::from(inner.width);
    let lines: Vec<Line> = tree
        .rows()
        .iter()
        .enumerate()
        .skip(offset)
        .take(height)
        .map(|(i, row)| {
            let icon = if !row.is_dir {
                "  "
            } else if tree.is_expanded(&row.path) {
                "▾ "
            } else {
                "▸ "
            };
            let changed = app.is_changed(&row.path, row.is_dir);
            let git = tree.git_status(&row.path, row.is_dir);
            let mut text = format!("{}{icon}{}", "  ".repeat(row.depth), row.name);
            if row.is_dir {
                text.push('/');
            }
            if changed {
                text.push_str(" ●");
            }
            // Git's letter at the right edge, as editors show it.
            let letter = git.map(|l| format!(" {l}")).unwrap_or_default();
            let room = width.saturating_sub(letter.chars().count());
            let mut text: String = text.chars().take(room).collect();
            text = format!("{text:<room$}{letter}");
            let mut style = match git {
                Some('?' | 'A') => fg(ADDED_SIGN),
                Some('D') => fg(REMOVED_SIGN),
                Some(_) => fg(CHANGED_SIGN),
                None if changed => fg(Color::Yellow),
                None if row.is_dir => fg(Color::Blue),
                None => Style::new(),
            };
            if i == tree.selected() {
                style = if focused {
                    style.add_modifier(Modifier::REVERSED)
                } else {
                    style.add_modifier(Modifier::BOLD)
                };
            }
            Line::styled(format!("{text:<width$}"), style)
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), inner);
}

/// A line as styled runs: coloured by its language when known, plain otherwise.
fn line_runs(file: &Editor, row: usize, text: &str) -> Vec<(Style, String)> {
    match file.styled().and_then(|s| s.get(row)) {
        Some(runs) => runs.iter().map(|(c, t)| (fg(*c), t.clone())).collect(),
        None => vec![(Style::new(), text.to_owned())],
    }
}

/// Marks the characters `from..to` as selected, splitting runs where the
/// selection starts and ends. A selection past the end of the line (an empty
/// line in a line selection) shows as one selected space.
fn select(runs: Vec<(Style, String)>, from: usize, to: usize) -> Vec<(Style, String)> {
    let mut out = Vec::new();
    let mut at = 0;
    for (style, text) in runs {
        let chars: Vec<char> = text.chars().collect();
        let (start, end) = (at, at + chars.len());
        at = end;
        let a = from.clamp(start, end) - start;
        let b = to.clamp(start, end) - start;
        for (range, selected) in [(0..a, false), (a..b, true), (b..chars.len(), false)] {
            if !range.is_empty() {
                let piece: String = chars[range].iter().collect();
                let style = if selected {
                    style.add_modifier(Modifier::REVERSED)
                } else {
                    style
                };
                out.push((style, piece));
            }
        }
    }
    if to > at {
        out.push((Style::new().add_modifier(Modifier::REVERSED), " ".into()));
    }
    out
}

/// Runs with the first `skip` characters dropped, cut to `room` columns.
fn clip(runs: &[(Style, String)], skip: usize, room: usize) -> Vec<Span<'static>> {
    let mut to_skip = skip;
    let mut left = room;
    let mut spans = Vec::new();
    for (style, text) in runs {
        if left == 0 {
            break;
        }
        let count = text.chars().count();
        if to_skip >= count {
            to_skip -= count;
            continue;
        }
        // A tab is one character to the editor's cursor, so it is drawn as
        // one column; the file keeps its tab.
        let shown: String = text
            .chars()
            .skip(to_skip)
            .take(left)
            .map(|c| if c == '\t' { ' ' } else { c })
            .collect();
        to_skip = 0;
        left -= shown.chars().count();
        spans.push(Span::styled(shown, *style));
    }
    spans
}

fn render_file(frame: &mut Frame, app: &App, area: Rect) {
    let Some(file) = app.file() else {
        return;
    };
    let focused = app.focus() == Focus::File;
    let changed_by_agent = app.is_changed(file.path(), false);
    let title = format!(
        " {}{}{} ",
        file.path().display(),
        if file.is_modified() { " [+]" } else { "" },
        if changed_by_agent {
            " ● changed by the agent"
        } else {
            ""
        }
    );
    let block = pane_block(title, focused);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let gutter = file.gutter();
    let room = usize::from(inner.width).saturating_sub(gutter);
    file.set_viewport(usize::from(inner.height), room);
    let left = file.left_offset();
    let (cursor_row, cursor_col) = file.cursor();
    let selection = file.selection();

    // Rows to draw: each line, preceded by the lines of the last commit
    // that are gone from that place.
    let height = usize::from(inner.height);
    let changes = file.changes();
    let rows_from = |start: usize| {
        let mut rows: Vec<Option<usize>> = Vec::new();
        let mut removed: Vec<(usize, &String)> = Vec::new();
        for i in start..=file.lines().len() {
            if let Some(gone) = changes.and_then(|c| c.removed.get(&i)) {
                for text in gone {
                    removed.push((rows.len(), text));
                    rows.push(None);
                }
            }
            if i < file.lines().len() {
                rows.push(Some(i));
            }
            if rows.len() >= height {
                break;
            }
        }
        rows.truncate(height);
        (rows, removed)
    };
    // Removed lines take rows too, so the start may need to move down for
    // the cursor to stay on screen.
    let mut start = file.scroll();
    let (mut rows, mut removed) = rows_from(start);
    while start < cursor_row && !rows.contains(&Some(cursor_row)) {
        start += 1;
        (rows, removed) = rows_from(start);
    }

    let lines: Vec<Line> = rows
        .iter()
        .enumerate()
        .map(|(at, row)| match row {
            Some(i) => {
                let i = *i;
                let text = &file.lines()[i];
                let number_style = if i == cursor_row && focused {
                    fg(Color::Gray)
                } else {
                    fg(DIM)
                };
                let mark = changes.map_or(LineMark::Same, |c| c.marks[i]);
                let (sign, background) = match mark {
                    LineMark::Same => (Span::raw(" "), None),
                    LineMark::Added => (Span::styled("▎", fg(ADDED_SIGN)), Some(ADDED_BG)),
                    LineMark::Changed => (Span::styled("▎", fg(CHANGED_SIGN)), Some(CHANGED_BG)),
                };
                let mut spans = vec![
                    Span::styled(
                        format!("{:>width$} ", i + 1, width = gutter - 3),
                        number_style,
                    ),
                    sign,
                    Span::raw(" "),
                ];
                let mut runs = line_runs(file, i, text);
                if let Some(bg) = background {
                    runs = runs
                        .into_iter()
                        .map(|(style, t)| (style.bg(bg), t))
                        .collect();
                }
                if let Some((from, to)) =
                    selection.and_then(|sel| sel.columns(i, text.chars().count()))
                {
                    runs = select(runs, from, to);
                }
                spans.extend(clip(&runs, left, room));
                Line::from(spans)
            }
            None => {
                let text = removed
                    .iter()
                    .find(|(row, _)| *row == at)
                    .map_or("", |(_, t)| t.as_str());
                let style = Style::new().fg(REMOVED_FG).bg(REMOVED_BG);
                let mut spans = vec![
                    Span::raw(" ".repeat(gutter - 2)),
                    Span::styled("-", fg(REMOVED_SIGN)),
                    Span::raw(" "),
                ];
                spans.extend(clip(&[(style, text.to_owned())], left, room));
                Line::from(spans)
            }
        })
        .collect();
    file.set_rows(rows.clone());
    frame.render_widget(Paragraph::new(lines), inner);

    let typing_below = matches!(file.mode(), EditorMode::Command | EditorMode::Search);
    if focused
        && !typing_below
        && let Some(y) = rows.iter().position(|r| *r == Some(cursor_row))
    {
        let x = gutter + cursor_col.saturating_sub(left);
        if x < usize::from(inner.width) {
            frame.set_cursor_position(Position::new(inner.x + x as u16, inner.y + y as u16));
        }
    }
}

/// Vim's command line in a frame: the command or search being typed, else
/// the last message, else the mode and where the cursor is.
fn render_command_frame(frame: &mut Frame, app: &App, area: Rect) {
    let Some(file) = app.file() else {
        return;
    };
    let focused = app.focus() == Focus::File;
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(fg(if focused { ACCENT } else { DIM }))
        .padding(Padding::horizontal(1));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let label = |text: &'static str, color: Color| {
        Line::styled(text, fg(color).add_modifier(Modifier::BOLD))
    };
    let left = match file.mode() {
        EditorMode::Command | EditorMode::Search => {
            let lead = if file.mode() == EditorMode::Command {
                ':'
            } else {
                '/'
            };
            let text = format!("{lead}{}", file.prompt());
            if focused {
                let x = text.chars().count().min(usize::from(inner.width));
                frame.set_cursor_position(Position::new(inner.x + x as u16, inner.y));
            }
            Line::raw(text)
        }
        mode => match file.message() {
            Some((text, true)) => Line::styled(text.to_owned(), fg(Color::Red)),
            Some((text, false)) => Line::styled(text.to_owned(), fg(Color::Gray)),
            None => match mode {
                EditorMode::Insert => label("-- INSERT --", Color::Green),
                EditorMode::Visual { line: true } => label("-- VISUAL LINE --", Color::Magenta),
                EditorMode::Visual { line: false } => label("-- VISUAL --", Color::Magenta),
                _ => Line::default(),
            },
        },
    };
    let (row, col) = file.cursor();
    // Keys of a command not yet complete, as Vim's showcmd, then the position.
    let right = format!("{}   {}:{}", file.partial_command(), row + 1, col + 1);
    frame.render_widget(Paragraph::new(left), inner);
    frame.render_widget(
        Paragraph::new(Line::styled(right, fg(DIM)).right_aligned()),
        inner,
    );
}

fn fg(color: Color) -> Style {
    Style::new().fg(color)
}

/// Adds `text` wrapped to `width`, the first line after `first`, the others
/// after `rest`, which should be as wide.
fn push_wrapped(
    out: &mut Vec<Line<'static>>,
    first: Span<'static>,
    rest: &'static str,
    text: &str,
    style: Style,
    width: usize,
) {
    let indent = first.content.chars().count();
    let mut lead = Some(first);
    for piece in wrap(text, width.saturating_sub(indent).max(1)) {
        let prefix = lead.take().unwrap_or_else(|| Span::raw(rest));
        out.push(Line::from(vec![prefix, Span::styled(piece, style)]));
    }
}

/// `● Name(argument)`, the head line of an action.
fn action(bullet: Color, name: &str, argument: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled("● ", fg(bullet)),
        Span::styled(name.to_owned(), Style::new().add_modifier(Modifier::BOLD)),
        Span::raw(format!("({argument})")),
    ])
}

/// `  ⎿  text`, the result line under an action.
fn result(out: &mut Vec<Line<'static>>, text: &str, style: Style, width: usize) {
    push_wrapped(
        out,
        Span::styled("  ⎿  ", fg(DIM)),
        "     ",
        text,
        style,
        width,
    );
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

fn entry_lines(entry: &Entry, app: &App, width: usize) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    match entry {
        Entry::Welcome => welcome(&mut out, app, width),
        Entry::Info(text) => {
            push_wrapped(
                &mut out,
                Span::raw("  "),
                "  ",
                text,
                fg(Color::Gray),
                width,
            );
        }
        Entry::Error(text) => {
            push_wrapped(
                &mut out,
                Span::styled("✗ ", fg(Color::Red)),
                "  ",
                text,
                fg(Color::Red),
                width,
            );
        }
        Entry::User(text) => {
            push_wrapped(
                &mut out,
                Span::styled("> ", fg(DIM)),
                "  ",
                text,
                fg(Color::Gray),
                width,
            );
        }
        Entry::Said(text) => {
            let body = markdown::render(text, width.saturating_sub(2), Style::new());
            for (i, line) in body.into_iter().enumerate() {
                let lead = if i == 0 {
                    Span::styled("● ", fg(Color::White))
                } else {
                    Span::raw("  ")
                };
                let mut spans = vec![lead];
                spans.extend(line.spans);
                out.push(Line::from(spans));
            }
        }
        Entry::Tool {
            name,
            path,
            outcome,
        } => tool_lines(&mut out, name, path.as_deref(), outcome, width),
        Entry::Checks(commands) => out.push(action(Color::Yellow, "Checks", &commands.join(" · "))),
        Entry::Passed => result(&mut out, "All checks passed", fg(Color::Green), width),
        Entry::Failed { command, excerpt } => {
            result(
                &mut out,
                &format!("{command} failed"),
                fg(Color::Red),
                width,
            );
            for line in excerpt.lines().take(EXCERPT_LINES) {
                push_wrapped(&mut out, Span::raw("     "), "     ", line, fg(DIM), width);
            }
        }
        Entry::Escalating { from, to } => {
            out.push(action(
                Color::Magenta,
                "Escalate",
                &format!("{from} → {to}"),
            ));
            result(
                &mut out,
                &format!("The checks kept failing, handing over to {to}"),
                fg(DIM),
                width,
            );
        }
        Entry::GaveUp => {
            push_wrapped(
                &mut out,
                Span::styled("✗ ", fg(Color::Red)),
                "  ",
                "The checks still fail after every model tried. /diff shows what changed",
                fg(Color::Red),
                width,
            );
        }
        Entry::Cost {
            usage,
            cost,
            complete,
            seconds,
            subscription,
        } => {
            let cost = match (*subscription, cost.0 > 0.0, *complete) {
                (true, false, _) => "subscription".to_owned(),
                (true, true, _) => format!("{cost} + subscription"),
                (false, _, true) => cost.to_string(),
                (false, _, false) => format!("{cost} reported, part of the cost unknown"),
            };
            let text = format!(
                "{cost} · {} in · {} out · {seconds}s",
                usage.input, usage.output
            );
            out.push(Line::styled(format!("  {text}"), fg(DIM)));
        }
    }
    out
}

fn tool_lines(
    out: &mut Vec<Line<'static>>,
    name: &str,
    path: Option<&str>,
    outcome: &Result<ToolSummary, String>,
    width: usize,
) {
    let path = path.unwrap_or("");
    match outcome {
        Err(error) => {
            let label = match name {
                "read_file" => "Read",
                "list_dir" => "List",
                "replace" => "Update",
                "write_file" => "Write",
                other => other,
            };
            out.push(action(Color::Red, label, path));
            result(out, &format!("Error: {error}"), fg(Color::Red), width);
        }
        Ok(ToolSummary::Read { path, lines }) => {
            out.push(action(Color::Green, "Read", path));
            result(
                out,
                &format!("Read {}", plural(*lines, "line", "lines")),
                fg(DIM),
                width,
            );
        }
        Ok(ToolSummary::Ran { label, lines }) => {
            out.push(Line::from(vec![
                Span::styled("● ", fg(Color::Green)),
                Span::styled(label.clone(), Style::new().add_modifier(Modifier::BOLD)),
            ]));
            result(out, &plural(*lines, "line", "lines"), fg(DIM), width);
        }
        Ok(ToolSummary::Listed { path, entries }) => {
            out.push(action(Color::Green, "List", path));
            result(
                out,
                &format!("Listed {}", plural(*entries, "entry", "entries")),
                fg(DIM),
                width,
            );
        }
        Ok(ToolSummary::Changed {
            path,
            created,
            diff,
        }) => {
            let added = diff
                .iter()
                .filter(|l| matches!(l, DiffLine::Added(_)))
                .count();
            let removed = diff
                .iter()
                .filter(|l| matches!(l, DiffLine::Removed(_)))
                .count();
            let summary = if *created {
                out.push(action(Color::Green, "Create", path));
                format!("Created {path} with {}", plural(added, "line", "lines"))
            } else {
                out.push(action(Color::Green, "Update", path));
                format!(
                    "Updated {path} with {} and {}",
                    plural(added, "addition", "additions"),
                    plural(removed, "removal", "removals")
                )
            };
            result(out, &summary, fg(DIM), width);
            diff_lines(out, diff, *created, width);
        }
    }
}

fn diff_lines(out: &mut Vec<Line<'static>>, diff: &[DiffLine], created: bool, width: usize) {
    // A new file's content says little in a transcript; its size says enough.
    if created {
        return;
    }
    let room = width.saturating_sub(7).max(1);
    for line in diff.iter().take(DIFF_LINES) {
        let (mark, text, style) = match line {
            DiffLine::Context(t) => (" ", t, fg(DIM)),
            DiffLine::Removed(t) => (
                "-",
                t,
                Style::new().fg(Color::Red).bg(Color::Rgb(60, 20, 20)),
            ),
            DiffLine::Added(t) => (
                "+",
                t,
                Style::new().fg(Color::Green).bg(Color::Rgb(20, 50, 20)),
            ),
        };
        let shown: String = text.chars().take(room).collect();
        out.push(Line::from(vec![
            Span::raw("     "),
            Span::styled(format!("{mark} {shown}"), style),
        ]));
    }
    if diff.len() > DIFF_LINES {
        out.push(Line::styled(
            format!(
                "     … {} more",
                plural(diff.len() - DIFF_LINES, "line", "lines")
            ),
            fg(DIM),
        ));
    }
}

fn welcome(out: &mut Vec<Line<'static>>, app: &App, width: usize) {
    let inner = width.clamp(20, 64) - 2;
    let checks = if app.checks().is_empty() {
        "none, changes are kept as written (/check adds one)".to_owned()
    } else {
        app.checks()
            .iter()
            .map(ironquill_tools::Check::command)
            .collect::<Vec<_>>()
            .join(", ")
    };
    let rows: Vec<(String, Style)> = vec![
        (
            "✻ ironquill".into(),
            fg(ACCENT).add_modifier(Modifier::BOLD),
        ),
        (String::new(), Style::new()),
        (
            "  /help for commands, Ctrl-C to stop a request".into(),
            fg(DIM),
        ),
        (String::new(), Style::new()),
        (format!("  project: {}", app.project()), fg(Color::Gray)),
        (format!("  model:   {}", app.chain()), fg(Color::Gray)),
        (format!("  checks:  {checks}"), fg(Color::Gray)),
    ];
    let border = fg(ACCENT);
    out.push(Line::styled(format!("╭{}╮", "─".repeat(inner)), border));
    for (text, style) in rows {
        let mut shown: String = format!(" {text}").chars().take(inner).collect();
        let pad = inner - shown.chars().count();
        shown.push_str(&" ".repeat(pad));
        out.push(Line::from(vec![
            Span::styled("│", border),
            Span::styled(shown, style),
            Span::styled("│", border),
        ]));
    }
    out.push(Line::styled(format!("╰{}╯", "─".repeat(inner)), border));
}

fn render_transcript(frame: &mut Frame, app: &App, area: Rect) {
    let width = usize::from(area.width).saturating_sub(1).max(1);
    let mut lines: Vec<Line> = Vec::new();
    for (i, entry) in app.transcript().iter().enumerate() {
        lines.extend(entry_lines(entry, app, width));
        // A blank line after each block, except where a result line follows
        // the action it belongs to.
        let next_is_result = matches!(
            app.transcript().get(i + 1),
            Some(Entry::Passed | Entry::Failed { .. })
        );
        if !next_is_result {
            lines.push(Line::default());
        }
    }

    let height = usize::from(area.height);
    let max_scroll = lines.len().saturating_sub(height);
    app.set_max_scroll(max_scroll);
    let back = app.scroll_back().min(max_scroll);
    let top = max_scroll - back;
    let visible: Vec<Line> = lines.into_iter().skip(top).take(height).collect();
    frame.render_widget(Paragraph::new(visible), area);
}

fn render_activity(frame: &mut Frame, app: &App, area: Rect) {
    let Some(elapsed) = app.elapsed() else {
        return;
    };
    let glyph = SPINNER[app.spinner() % SPINNER.len()];
    let mut spans = vec![Span::styled(format!("{glyph} Working… "), fg(ACCENT))];
    if let Some(model) = app.working_model() {
        spans.push(Span::styled(format!("{model} "), fg(Color::Gray)));
    }
    spans.push(Span::styled(
        format!("({}s · Ctrl-C to stop)", elapsed.as_secs()),
        fg(DIM),
    ));
    let line = Line::from(spans);
    frame.render_widget(Paragraph::new(line), area);
}

fn render_input(frame: &mut Frame, app: &App, area: Rect) {
    let (editor, prompt) = match app.mode() {
        Mode::Command => (app.command_line(), ":"),
        Mode::Insert | Mode::Normal => (app.input(), "> "),
    };
    let focused = app.mode() != Mode::Normal;
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(fg(if focused { Color::Gray } else { DIM }))
        .padding(Padding::horizontal(1));
    let inner = block.inner(area);

    let line = if editor.text().is_empty() && app.mode() == Mode::Insert {
        Line::from(vec![Span::raw(prompt), Span::styled(PLACEHOLDER, fg(DIM))])
    } else {
        let (shown, _) = visible_window(editor, prompt, usize::from(inner.width));
        Line::raw(shown)
    };
    frame.render_widget(Paragraph::new(line).block(block), area);

    if focused {
        let (_, cursor) = visible_window(editor, prompt, usize::from(inner.width));
        frame.set_cursor_position(Position::new(inner.x + cursor as u16, inner.y));
    }
}

/// The slice of the line that fits, scrolled so that the cursor stays in view,
/// and the cursor column within it.
fn visible_window(editor: &LineEditor, prompt: &str, width: usize) -> (String, usize) {
    let full: Vec<char> = prompt.chars().chain(editor.text().chars()).collect();
    let cursor = prompt.chars().count() + editor.cursor();
    let width = width.max(1);
    let start = (cursor + 1).saturating_sub(width);
    let shown: String = full.iter().skip(start).take(width).collect();
    (shown, cursor - start)
}

fn render_status(frame: &mut Frame, app: &App, area: Rect) {
    let mode = match app.mode() {
        Mode::Normal => "-- NORMAL --",
        Mode::Insert => "-- INSERT --",
        Mode::Command => "-- COMMAND --",
    };
    // Only what a key already typed is waiting for; no shortcut reminders.
    let waiting = match app.pending() {
        Some(Pending::Leader) => "  ,",
        Some(Pending::Window) => "  ^W",
        None => "",
    };
    // A notice (the first Ctrl-C) takes the place of the conversation's name.
    let label = match app.notice() {
        Some(notice) => Span::styled(format!("  {notice}"), fg(Color::Yellow)),
        None => Span::styled(format!("  {}", app.session_label()), fg(DIM)),
    };
    let left = Line::from(vec![
        Span::styled(format!("  {mode}"), fg(DIM)),
        Span::styled(waiting, fg(Color::Gray)),
        label,
    ]);

    let (usage, cost, complete) = app.totals();
    // The model in use stands out: it is what Ctrl-E changes.
    let current = app
        .current_model()
        .map_or_else(|| "no model".to_owned(), ToString::to_string);
    let rest = app
        .chain()
        .strip_prefix(&current)
        .unwrap_or_default()
        .to_owned();
    let right = Line::from(vec![
        // Room between the conversation's name and the model, however long
        // the name.
        Span::raw("  "),
        Span::styled(current, fg(ACCENT)),
        Span::styled(
            format!(
                "{rest} · {} in · {} out · {}{}  ",
                usage.input,
                usage.output,
                cost,
                if complete { "" } else { "+?" }
            ),
            fg(DIM),
        ),
    ]);

    // The model and the cost matter more than the hint: they get their room
    // first, and the left side gets what is left.
    let right_width = u16::try_from(right.width()).unwrap_or(area.width);
    let [left_area, right_area] =
        Layout::horizontal([Constraint::Min(0), Constraint::Length(right_width)]).areas(area);
    frame.render_widget(Paragraph::new(left), left_area);
    frame.render_widget(Paragraph::new(right), right_area);
}
