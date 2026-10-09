use ironquill_agent::Question;
use ironquill_core::{Effort, ModelId, TokenCount};
use ironquill_tools::{Container, DiffLine, LineMark, ToolSummary};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::symbols::Marker;
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Axis, Block, BorderType, Chart, Clear, Dataset, GraphType, LegendPosition, Padding, Paragraph,
};

use crate::markdown;
use crate::pictures::{Picture, Pictures};
use crate::wrap::wrap;
use ironquill_ui::editor::{Editor, EditorMode, Kind};
use ironquill_ui::keymap::{Focus, Mode, Pending, SHORTCUTS};
use ironquill_ui::references::{self, Reference};
use ironquill_ui::sessions;
use ironquill_ui::{App, CompactRow, Entry, LineEditor, Panes, SubAgent};

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
pub(crate) fn render(frame: &mut Frame, app: &App, pictures: &dyn Pictures) {
    let [main, activity, input, status] = Layout::vertical([
        Constraint::Min(1),
        Constraint::Length(1),
        Constraint::Length(3),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    render_panes(frame, app, pictures, main);
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
    if app.compact_picker().is_some() {
        render_compact(frame, app);
    }
    if app.approval().is_some() || app.keep_warm_question().is_some() {
        render_approval(frame, app);
    }
}

/// A command held for the person, over everything else, until they answer.
fn render_approval(frame: &mut Frame, app: &App) {
    let asked = app.keep_warm_question();
    let Some(approval) = app.approval().or(asked.as_ref()) else {
        return;
    };
    let screen = frame.area();
    let width = (screen.width * 4 / 5).clamp(30, 100).min(screen.width);
    let inner_width = usize::from(width.saturating_sub(2));
    // What cannot be undone in red, not a yellow one skims past; going on
    // costs only money, in cyan.
    let (color, title, keys, mut lines) = match &approval.question {
        Question::Command {
            command,
            reasons,
            secrets,
            hosts,
        } => {
            let mut lines = vec![
                Line::styled(
                    format!(" {} wants to run:", approval.model),
                    fg(Color::Gray),
                ),
                Line::default(),
            ];
            for row in wrap_plain(command, inner_width.saturating_sub(2)) {
                lines.push(Line::styled(
                    format!(" {row}"),
                    fg(Color::Red).add_modifier(Modifier::BOLD),
                ));
            }
            lines.push(Line::default());
            for reason in reasons {
                lines.push(Line::styled(format!(" · {reason}"), fg(Color::Gray)));
            }
            let lasting: Vec<&str> = secrets.iter().chain(hosts).map(String::as_str).collect();
            let keys = if lasting.is_empty() {
                " y: run it · n: refuse · c: copy ".to_owned()
            } else {
                format!(
                    " y: run it · a: always allow {} · n: refuse · c: copy ",
                    lasting.join(", ")
                )
            };
            (Color::Red, " Run this command? ", keys, lines)
        }
        Question::ColdStart {
            idle_minutes,
            tokens,
        } => (
            Color::Cyan,
            " The cache has expired ",
            " y: new session from the summary · n: go on as it is · c: stop, to compact "
                .to_owned(),
            vec![Line::raw(format!(
                " {}'s conversation was left {idle_minutes} minutes: its cache has expired. Going \
                 on as it is writes it all to the cache again{}, at the dearest rate; a new \
                 session starts from its summary and the latest exchanges.",
                approval.model,
                tokens.map_or_else(String::new, |t| format!(", about {}", TokenCount(t)))
            ))],
        ),
        Question::KeepWarm { minutes } => (
            Color::Cyan,
            " Keep the sessions warm? ",
            " y: another half hour · n: stop ".to_owned(),
            vec![Line::raw(format!(
                " No request for {minutes} minutes. Each read of the warm sessions costs a \
                 little; letting them cool, the next request writes them to the cache again."
            ))],
        ),
        Question::MoreTurns { turns } => (
            Color::Cyan,
            " Go on? ",
            " y: go on · n: answer now ".to_owned(),
            vec![Line::raw(format!(
                " {} used its {turns} turns. Go on for {turns} more, or have it answer now \
                 with what it found?",
                approval.model
            ))],
        ),
    };
    lines = lines
        .into_iter()
        .flat_map(|line| {
            // The question may be longer than the window is wide.
            let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
            let style = line.spans.first().map_or(Style::new(), |s| s.style);
            wrap_plain(&text, inner_width)
                .into_iter()
                .map(move |row| Line::styled(row, style))
                .collect::<Vec<_>>()
        })
        .collect();
    let height = (lines.len() as u16 + 2).min(screen.height);
    let area = Rect::new(
        screen.x + (screen.width - width) / 2,
        screen.y + screen.height.saturating_sub(height) / 3,
        width,
        height,
    );
    frame.render_widget(Clear, area);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(fg(color))
        .title(Span::styled(title, fg(color).add_modifier(Modifier::BOLD)))
        .title_bottom(Line::styled(keys, fg(color)).centered());
    let inner = block.inner(area);
    frame.render_widget(block, area);
    frame.render_widget(Paragraph::new(lines), inner);
}

/// `line` with the references in it drawn as links, and where they are:
/// first and last column, what they open. Code blocks are left as they are.
fn link_line(app: &App, line: Line<'static>) -> (Line<'static>, Vec<(usize, usize, Reference)>) {
    if line
        .spans
        .iter()
        .any(|s| s.style.fg == Some(markdown::CODE_BLOCK_COLOR))
    {
        return (line, Vec::new());
    }
    let mut spans = Vec::new();
    let mut links = Vec::new();
    let mut column = 0;
    for span in line.spans {
        let text = span.content.to_string();
        let mut at = 0;
        for (start, end, reference) in references::candidates(&text) {
            if !app.reference_exists(&reference) {
                continue;
            }
            if start > at {
                let piece = text[at..start].to_owned();
                column += piece.chars().count();
                spans.push(Span::styled(piece, span.style));
            }
            let piece = text[start..end].to_owned();
            let width = piece.chars().count();
            spans.push(Span::styled(
                piece,
                span.style
                    .fg(Color::Rgb(130, 170, 255))
                    .add_modifier(Modifier::UNDERLINED),
            ));
            spans.push(Span::styled("↗", fg(DIM)));
            links.push((column, column + width + 1, reference));
            column += width + 1;
            at = end;
        }
        if at < text.len() {
            let piece = text[at..].to_owned();
            column += piece.chars().count();
            spans.push(Span::styled(piece, span.style));
        }
    }
    (Line::from(spans).style(line.style), links)
}

/// How a command ended, who read it when it could not be read alone, and
/// how much it printed.
fn command_details(status: &str, checked_by: Option<&ModelId>, output: &str) -> String {
    let mut details = status.to_owned();
    if let Some(by) = checked_by {
        details.push_str(&format!(" · checked by {by}"));
    }
    details.push_str(&format!(
        " · {}",
        plural(output.lines().count(), "line", "lines")
    ));
    details
}

/// The /compact window: the subjects, ticked to keep, one open with its
/// exchanges.
fn render_compact(frame: &mut Frame, app: &App) {
    let Some(picker) = app.compact_picker() else {
        return;
    };
    let mut lines = vec![Line::styled(
        " Ticked is summed up and kept; unticked goes. ",
        fg(Color::Gray),
    )];
    let tick = |on: bool| if on { "[x]" } else { "[ ]" };
    for (row, kind) in picker.rows().into_iter().enumerate() {
        let selected = row == picker.cursor;
        let text = match kind {
            CompactRow::Subject(s) => {
                let subject = &picker.compaction.subjects[s];
                let kept = subject
                    .exchanges
                    .iter()
                    .filter(|e| picker.kept[**e])
                    .count();
                let mark = if kept == subject.exchanges.len() {
                    "[x]"
                } else if kept == 0 {
                    "[ ]"
                } else {
                    "[-]"
                };
                format!(
                    " {mark} {} ({})",
                    subject.name,
                    plural(subject.exchanges.len(), "exchange", "exchanges")
                )
            }
            CompactRow::Exchange(e) => format!(
                "     {} {}",
                tick(picker.kept[e]),
                picker.compaction.exchanges[e]
            ),
        };
        let style = if selected {
            fg(Color::White)
                .add_modifier(Modifier::BOLD)
                .bg(SELECTED_BG)
        } else {
            Style::new()
        };
        lines.push(Line::styled(text, style));
    }
    lines.push(Line::default());
    lines.push(Line::styled(
        format!(
            " {} The last exchange stays as it was (l)",
            tick(picker.last_as_is)
        ),
        fg(Color::Gray),
    ));
    let screen = frame.area();
    let width = (screen.width * 4 / 5).clamp(30, 100).min(screen.width);
    let height = (lines.len() as u16 + 2).min(screen.height);
    let area = Rect::new(
        screen.x + (screen.width - width) / 2,
        screen.y + screen.height.saturating_sub(height) / 3,
        width,
        height,
    );
    frame.render_widget(Clear, area);
    let block = pane_block(" Compact ".into(), true).title_bottom(
        Line::styled(
            " Space: tick · →: open · ←: close · Enter: compact · Esc: cancel ",
            fg(DIM),
        )
        .centered(),
    );
    let inner = block.inner(area);
    frame.render_widget(block, area);
    frame.render_widget(Paragraph::new(lines), inner);
}

/// `text` cut into rows of `width` characters at most.
fn wrap_plain(text: &str, width: usize) -> Vec<String> {
    let width = width.max(10);
    let mut rows = Vec::new();
    for line in text.lines() {
        let chars: Vec<char> = line.chars().collect();
        if chars.is_empty() {
            rows.push(String::new());
        }
        for chunk in chars.chunks(width) {
            rows.push(chunk.iter().collect());
        }
    }
    rows
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

/// The model picker (Ctrl-E), over everything else: what was typed to
/// search, then the models, the current one marked ●, the team ✓.
fn render_model_picker(frame: &mut Frame, app: &App) {
    let Some(picker) = app.model_picker() else {
        return;
    };
    let rows = app.model_rows();
    let screen = frame.area();
    let width = (screen.width * 4 / 5).clamp(30, 100).min(screen.width);
    let height = (rows.len() as u16 + 4).clamp(5, screen.height.saturating_sub(4).max(5));
    let area = Rect::new(
        screen.x + (screen.width - width) / 2,
        screen.y + screen.height.saturating_sub(height) / 3,
        width,
        height,
    );
    frame.render_widget(Clear, area);
    let block = pane_block(" Model ".into(), true).title_bottom(
        Line::styled(" Enter: choose · Space: team · Esc: close ", fg(DIM)).centered(),
    );
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let room = usize::from(inner.width);
    let current = app.current_model();
    // The effort sits at the right of the search line, ← → changing it.
    let effort = format!("effort ← {} →", app.effort());
    let typed = picker.filter.chars().count() + 8;
    let gap = room.saturating_sub(typed + effort.chars().count());
    let mut lines = vec![Line::from(vec![
        Span::styled("search ", fg(DIM)),
        Span::raw(picker.filter.clone()),
        Span::styled("▏", fg(ACCENT)),
        Span::raw(" ".repeat(gap)),
        // Past high, every call costs more and takes longer: plain to see.
        Span::styled(
            effort,
            if app.effort() > Effort::High {
                fg(Color::Yellow).add_modifier(Modifier::BOLD)
            } else {
                fg(Color::Gray)
            },
        ),
    ])];
    let credits = app
        .credits()
        .map(|c| format!("   scores: {c}"))
        .unwrap_or_default();
    lines.push(Line::styled(
        format!("● answers you   ✓ in its team: it may hand them tasks{credits}"),
        fg(DIM),
    ));
    let visible = usize::from(inner.height).saturating_sub(2);
    let first = picker.selected.saturating_sub(visible.saturating_sub(1));
    for (i, row) in rows.iter().enumerate().skip(first).take(visible) {
        let mark = if Some(&row.model) == current {
            "●"
        } else {
            " "
        };
        let team = if row.in_team { "✓" } else { " " };
        let name = format!("{mark} {team} {}", row.model);
        let gap = room.saturating_sub(name.chars().count() + row.note.chars().count() + 1);
        let mut style = Style::new();
        if !row.offered {
            style = style.fg(DIM);
        }
        if i == picker.selected {
            style = Style::new().add_modifier(Modifier::REVERSED);
        }
        lines.push(Line::from(vec![
            Span::styled(format!("{name}{}", " ".repeat(gap)), style),
            Span::styled(row.note.clone(), style.fg(DIM)),
        ]));
    }
    if rows.is_empty() {
        lines.push(Line::styled("  no model matches", fg(DIM)));
    }
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
    let block = pane_block(" Resume a conversation ".into(), true)
        .title_bottom(Line::styled(" Enter: choose · Esc: close ", fg(DIM)).centered());
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
            // The id, for /resume <id> and ironquill -r <id>.
            let details = format!(
                "{} · {} request{} · {} · {}",
                sessions::ago(item.updated, now),
                item.requests,
                if item.requests == 1 { "" } else { "s" },
                item.cost,
                item.id
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
fn render_panes(frame: &mut Frame, app: &App, pictures: &dyn Pictures, area: Rect) {
    if app.is_zoomed() {
        // Only the conversation; the other panes keep their state, unseen.
        let sub = render_chat(frame, app, pictures, area, false);
        app.set_panes(Panes {
            chat: cells(area),
            sub: sub.map(cells),
            ..Panes::default()
        });
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
    // The usage pane, when shown, on the right of everything above Docker.
    let area = if app.usage_pane().is_some() {
        let width = (area.width / 3).clamp(36, 64).min(area.width / 2);
        let [main, usage] =
            Layout::horizontal([Constraint::Min(20), Constraint::Length(width)]).areas(area);
        render_usage(frame, app, usage);
        main
    } else {
        area
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
        tree: (tree_width > 0).then_some(cells(tree)),
        file: None,
        chat: cells(center),
        docker: docker.map(cells),
        sub: None,
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
        panes.file = Some(cells(file));
        render_file(frame, app, file);
        render_command_frame(frame, app, command);
        panes.chat = cells(side);
        if side_width > 0 {
            panes.sub = render_chat(frame, app, pictures, side, true).map(cells);
        }
    } else {
        panes.sub = render_chat(frame, app, pictures, center, !alone).map(cells);
    }
    app.set_panes(panes);
}

/// A colour of the state, as the terminal draws it.
fn rgb(c: ironquill_ui::style::Rgb) -> Color {
    Color::Rgb(c.0, c.1, c.2)
}

/// Where a pane was drawn, as the state reads it to match a click.
fn cells(area: Rect) -> ironquill_ui::input::Rect {
    ironquill_ui::input::Rect::new(area.x, area.y, area.width, area.height)
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

/// The conversation, and under it the sub-agent pane when it is open,
/// whose area is returned for the mouse.
fn render_chat(
    frame: &mut Frame,
    app: &App,
    pictures: &dyn Pictures,
    area: Rect,
    framed: bool,
) -> Option<Rect> {
    // While a model of the team works, or until the next request, its work
    // takes most of the room: the conversation keeps a third.
    if let Some(sub) = app.sub_agent()
        && area.height >= 14
    {
        let top = (area.height / 3).max(6);
        let [chat, pane] =
            Layout::vertical([Constraint::Length(top), Constraint::Min(6)]).areas(area);
        render_conversation(frame, app, pictures, chat, true);
        render_sub_agent(frame, app, pictures, &sub, pane);
        return Some(pane);
    }
    render_conversation(frame, app, pictures, area, framed);
    None
}

/// The model a task was handed to, at work: who it is, the task, then
/// everything it does, the latest at the bottom.
fn render_sub_agent(
    frame: &mut Frame,
    app: &App,
    pictures: &dyn Pictures,
    sub: &SubAgent<'_>,
    area: Rect,
) {
    let state = if sub.working {
        format!(" working {} ", SPINNER[app.spinner() % SPINNER.len()])
    } else {
        format!(" done · {} ", plural(sub.work.len(), "step", "steps"))
    };
    let focused = app.focus() == Focus::SubAgent;
    let border = if focused {
        fg(Color::LightMagenta).add_modifier(Modifier::BOLD)
    } else {
        fg(Color::Magenta)
    };
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(border)
        .title(Line::from(vec![
            Span::styled(
                " Sub-agent ",
                fg(Color::Magenta).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                sub.to.to_string(),
                fg(Color::White).add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!(" · for {} ·", sub.from), fg(DIM)),
            Span::styled(state, fg(Color::Magenta)),
            Span::styled(format!("· {} ", sub.spent), fg(Color::Gray)),
        ]))
        .padding(Padding::horizontal(1));
    let inner = block.inner(area);

    let width = usize::from(inner.width).saturating_sub(1).max(1);
    let mut lines: Vec<Line> = Vec::new();
    push_wrapped(
        &mut lines,
        Span::styled("Task  ", fg(Color::Magenta)),
        "      ",
        sub.task,
        fg(Color::Gray),
        width,
    );
    lines.push(Line::default());
    for entry in &sub.work {
        lines.extend(entry_lines(entry, app, pictures, width));
        lines.push(Line::default());
    }
    if sub.work.is_empty() {
        lines.push(Line::styled("  starting…", fg(DIM)));
    }
    // The latest at the bottom, as in the conversation, unless scrolled up.
    let height = usize::from(inner.height);
    let max = lines.len().saturating_sub(height);
    app.set_sub_max(max);
    let up = app.sub_scroll().min(max);
    let skip = max - up;
    let below = if up > 0 {
        format!(" ↓ {} below · ", plural(up, "line", "lines"))
    } else {
        String::from(" ")
    };
    let block =
        block.title_bottom(Line::styled(format!("{below}Ctrl-T hides "), fg(DIM)).right_aligned());
    frame.render_widget(block, area);
    let shown: Vec<Line> = lines.into_iter().skip(skip).take(height).collect();
    frame.render_widget(Paragraph::new(shown), inner);
}

fn render_conversation(
    frame: &mut Frame,
    app: &App,
    pictures: &dyn Pictures,
    area: Rect,
    framed: bool,
) {
    if framed {
        let block = pane_block(" Chat ".into(), app.focus() == Focus::Chat);
        let inner = block.inner(area);
        frame.render_widget(block, area);
        render_transcript(frame, app, pictures, inner);
    } else {
        render_transcript(frame, app, pictures, area);
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
        Some(runs) => runs.iter().map(|(c, t)| (fg(rgb(*c)), t.clone())).collect(),
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
    let title = if file.kind() == Kind::Context {
        // What an edit saves, as it is made.
        let chars: usize = file.lines().iter().map(|l| l.len() + 1).sum();
        format!(
            " context · about {} tokens{} ",
            TokenCount((chars / 4) as u64),
            if file.is_modified() { " [+]" } else { "" }
        )
    } else {
        format!(
            " {}{}{} ",
            file.path().display(),
            if file.is_modified() { " [+]" } else { "" },
            if changed_by_agent {
                " ● changed by the agent"
            } else {
                ""
            }
        )
    };
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
    let hidden = file.hidden();
    let rows_from = |start: usize| {
        let mut rows: Vec<Option<usize>> = Vec::new();
        let mut removed: Vec<(usize, &String)> = Vec::new();
        for i in start..=file.lines().len() {
            // Lines inside a closed fold are not drawn.
            if hidden.get(i).copied().unwrap_or(false) {
                continue;
            }
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
                if file.kind() == Kind::Context && text.starts_with("=== ") {
                    runs = vec![(fg(ACCENT).add_modifier(Modifier::BOLD), text.clone())];
                    if let Some(count) = file.folded(i) {
                        // A closed block reads as one line: its size, then
                        // the start of what it holds.
                        let preview = file.lines()[i + 1..=i + count]
                            .iter()
                            .map(|l| l.trim())
                            .find(|l| !l.is_empty())
                            .unwrap_or("");
                        let preview: String = preview.chars().take(80).collect();
                        runs.push((
                            fg(DIM),
                            format!("  ▸ {} · {preview}", plural(count, "line", "lines")),
                        ));
                    }
                }
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

fn entry_lines(
    entry: &Entry,
    app: &App,
    pictures: &dyn Pictures,
    width: usize,
) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    match entry {
        Entry::Welcome => welcome(&mut out, app, width),
        Entry::Command {
            command,
            status,
            output,
            checked_by,
            ..
        } => {
            out.push(Line::from(vec![
                Span::styled("● ", fg(Color::Cyan)),
                Span::styled("Run", Style::new().add_modifier(Modifier::BOLD)),
            ]));
            for line in command.lines() {
                push_wrapped(
                    &mut out,
                    Span::raw("  "),
                    "  ",
                    line,
                    fg(Color::White),
                    width,
                );
            }
            result(
                &mut out,
                &command_details(status, checked_by.as_ref(), output),
                fg(DIM),
                width,
            );
            for line in output.lines() {
                push_wrapped(&mut out, Span::raw("     "), "     ", line, fg(DIM), width);
            }
        }
        Entry::Interrupted => push_wrapped(
            &mut out,
            Span::styled("■ ", fg(Color::Red)),
            "  ",
            "Interrupted: the request stopped before it ended",
            fg(Color::Red).add_modifier(Modifier::BOLD),
            width,
        ),
        Entry::Refused(text) => {
            push_wrapped(
                &mut out,
                Span::styled("⊘ ", fg(Color::Yellow)),
                "  ",
                text,
                fg(Color::Yellow),
                width,
            );
        }
        Entry::Ended(text) => {
            push_wrapped(
                &mut out,
                Span::styled("■ ", fg(Color::Magenta)),
                "  ",
                text,
                fg(Color::Magenta).add_modifier(Modifier::BOLD),
                width,
            );
        }
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
        Entry::Image(path) => match pictures.lines(Picture::File(path), width.saturating_sub(2)) {
            Some(drawn) => {
                out.push(Line::styled(format!("  {path}"), fg(DIM)));
                out.extend(drawn.into_iter().map(|line| {
                    let mut spans = vec![Span::raw("  ")];
                    spans.extend(line.spans);
                    Line::from(spans)
                }));
            }
            None => push_wrapped(
                &mut out,
                Span::raw("  "),
                "  ",
                &format!("{path} cannot be shown here"),
                fg(DIM),
                width,
            ),
        },
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
            let body = markdown::render(text, width.saturating_sub(2), Style::new(), pictures);
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
        // A member's work, set apart by a bar in the colour of the handover,
        // its replies under its name.
        Entry::Member { model, entry } => {
            let bar = || Span::styled("  ┃ ", fg(Color::Magenta));
            if matches!(**entry, Entry::Said(_)) {
                out.push(Line::from(vec![
                    bar(),
                    Span::styled(model.to_string(), fg(Color::Magenta)),
                ]));
            }
            for line in entry_lines(entry, app, pictures, width.saturating_sub(4)) {
                let mut spans = vec![bar()];
                spans.extend(line.spans);
                out.push(Line::from(spans));
            }
        }
        Entry::Step {
            number,
            of,
            name,
            model,
            effort,
        } => {
            // Who works, plain to see: the coder in the accent colour, the
            // planner in the colour of handovers, ironquill in grey.
            let coding = name.starts_with("Coding") || name.starts_with("Fixing");
            let colour = match (model, coding) {
                (None, _) => Color::Gray,
                (Some(_), true) => ACCENT,
                (Some(_), false) => Color::Magenta,
            };
            let title = ironquill_ui::step_title(*number, *of, name, model.as_ref(), *effort);
            let rule = width.saturating_sub(title.chars().count() + 6);
            out.push(Line::styled(
                format!("━━ {title} {}", "━".repeat(rule.min(40))),
                fg(colour).add_modifier(Modifier::BOLD),
            ));
        }
        Entry::Delegating { from, to, task, .. } => {
            out.push(action(
                Color::Magenta,
                "Delegate",
                &format!("{from} → {to}"),
            ));
            result(&mut out, task, fg(DIM), width);
        }
        Entry::OverBudget { spent, budget } => {
            push_wrapped(
                &mut out,
                Span::styled("✗ ", fg(Color::Yellow)),
                "  ",
                &if spent.0 > budget.0 {
                    format!(
                        "Budget of {budget} for this request passed ({spent}): the work stopped. \
                         /budget <dollars> changes it"
                    )
                } else {
                    format!(
                        "Budget of {budget} for this request: {spent} spent, and the next call \
                         would go past it, so the work stopped. /budget <dollars> changes it"
                    )
                },
                fg(Color::Yellow),
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
            context,
            runs,
        } => {
            // When several models took turns: who, in order, and what each
            // run cost, the total under it.
            if runs.len() > 1 {
                let chain: Vec<String> = runs
                    .iter()
                    .map(|(model, spent)| {
                        if spent.subscription && spent.cost.0 == 0.0 {
                            format!("{model} subscription")
                        } else {
                            format!("{model} {}", spent.cost)
                        }
                    })
                    .collect();
                push_wrapped(
                    &mut out,
                    Span::raw("  "),
                    "    ",
                    &chain.join(" → "),
                    fg(Color::Gray),
                    width,
                );
            }
            let cost = match (*subscription, cost.0 > 0.0, *complete) {
                (true, false, _) => "subscription".to_owned(),
                (true, true, _) => format!("{cost} + subscription"),
                (false, _, true) => cost.to_string(),
                (false, _, false) => format!("{cost} reported, part of the cost unknown"),
            };
            let total = if runs.len() > 1 { "total " } else { "" };
            let mut text = format!(
                "{total}{cost} · {} in · {} out · {seconds}s",
                usage.input, usage.output
            );
            if let Some(context) = context {
                text.push_str(&format!(" · {context}"));
            }
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
    let checks = if !app.checks().is_empty() {
        app.checks()
            .iter()
            .map(ironquill_tools::Check::command)
            .collect::<Vec<_>>()
            .join(", ")
    } else if app.detects_checks() {
        "the project's tests, found when checking".to_owned()
    } else {
        "none, changes are kept as written (/check adds one)".to_owned()
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

/// A reply longer than this many lines is folded unless unfolded.
const FOLD_AT: usize = 12;
/// How many lines of a folded reply stay visible.
const FOLD_SHOW: usize = 6;
/// The background of the reply selected in normal mode: a shade lighter than
/// the terminal's, enough to see it, not enough to hurt reading.
const SELECTED_BG: Color = Color::Rgb(34, 36, 44);

fn render_transcript(frame: &mut Frame, app: &App, pictures: &dyn Pictures, area: Rect) {
    let width = usize::from(area.width).saturating_sub(1).max(1);
    let selected = app.selected_reply();
    let mut lines: Vec<Line> = Vec::new();
    let mut ranges = Vec::new();
    // The lines a click folds or unfolds, and those that copy a code block.
    let mut fold_marks = Vec::new();
    let mut copy_marks = Vec::new();
    let mut link_marks = Vec::new();
    // The latest reply stays open: it folds once another follows it.
    let latest = app.transcript().iter().rposition(Entry::is_reply);
    for (i, entry) in app.transcript().iter().enumerate() {
        // A member's work shows in the sub-agent pane; here a line says how
        // much there was.
        if matches!(entry, Entry::Member { .. }) {
            continue;
        }
        let first = lines.len();
        let mut block = match entry {
            // A command shows one line until opened.
            Entry::Command {
                command,
                status,
                output,
                checked_by,
                ..
            } if !app.is_expanded(i) => {
                let room = width.saturating_sub(10).max(10);
                let first_line = command.lines().next().unwrap_or_default();
                let mut shown: String = first_line.chars().take(room).collect();
                if shown.chars().count() < command.chars().count() {
                    shown.push('…');
                }
                let mut block = vec![action(Color::Cyan, "Run", &shown)];
                result(
                    &mut block,
                    &format!(
                        "{} · click or Enter to show",
                        command_details(status, checked_by.as_ref(), output)
                    ),
                    fg(DIM),
                    width,
                );
                fold_marks.push(first + block.len() - 1);
                block
            }
            Entry::Command { .. } => {
                let mut block = entry_lines(entry, app, pictures, width);
                fold_marks.push(first + block.len());
                block.push(Line::styled("  ▾ fold", fg(DIM)));
                block
            }
            _ => entry_lines(entry, app, pictures, width),
        };
        if let Entry::Delegating { to, spent, .. } = entry {
            let steps = app.transcript()[i + 1..]
                .iter()
                .take_while(|e| matches!(e, Entry::Member { .. }))
                .count();
            block.push(Line::styled(
                format!(
                    "  ┃ {to}: {} · {spent} · Ctrl-T shows them",
                    plural(steps, "step", "steps")
                ),
                fg(Color::Magenta),
            ));
        }
        let long = entry.is_reply() && block.len() > FOLD_AT;
        // Folded by default but for the latest; a click on its mark, or
        // Enter, turns it the other way.
        let folded = long && ((Some(i) != latest) != app.is_expanded(i));
        if folded {
            let hidden = block.len() - FOLD_SHOW;
            block.truncate(FOLD_SHOW);
            fold_marks.push(first + block.len());
            block.push(Line::styled(
                format!("  ▸ {}", plural(hidden, "more line", "more lines")),
                fg(DIM),
            ));
        } else if long {
            fold_marks.push(first + block.len());
            block.push(Line::styled("  ▾ fold", fg(DIM)));
        }
        // Cited files and commits, as links.
        if matches!(entry, Entry::Said(_)) {
            for (row, line) in block.iter_mut().enumerate() {
                let (linked, links) = link_line(app, std::mem::take(line));
                *line = linked;
                link_marks.extend(links.into_iter().map(|(a, b, r)| (first + row, a, b, r)));
            }
        }
        // The copy marks of a reply's code blocks, as far as shown.
        if let Entry::Said(text) = entry {
            let codes = ironquill_ui::blocks::code_blocks(text);
            let marked = block.iter().enumerate().filter(|(_, line)| {
                line.spans
                    .iter()
                    .any(|s| s.content.as_ref() == markdown::COPY_MARK)
            });
            for ((at, _), code) in marked.zip(codes) {
                copy_marks.push((first + at, code));
            }
        }
        if selected == Some(i) {
            block = block
                .into_iter()
                .map(|line| {
                    // Pad to the full width so that the shade reads as a block.
                    let pad = width.saturating_sub(line.width());
                    let mut spans = line.spans;
                    spans.push(Span::raw(" ".repeat(pad)));
                    Line::from(spans).style(Style::new().bg(SELECTED_BG))
                })
                .collect();
        }
        lines.extend(block);
        ranges.push((i, first, lines.len().saturating_sub(1).max(first)));
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

    // Bring the selected reply into sight when it was just selected or folded.
    if app.take_reveal()
        && let Some((_, first, last)) = ranges.iter().find(|(e, _, _)| Some(*e) == selected)
    {
        let top = max_scroll - app.scroll_back().min(max_scroll);
        let new_top = if *first < top {
            *first
        } else if *last >= top + height {
            (last + 1).saturating_sub(height).min(*first)
        } else {
            top
        };
        app.set_scroll_back(max_scroll - new_top.min(max_scroll));
    }

    let back = app.scroll_back().min(max_scroll);
    let top = max_scroll - back;
    app.set_entry_lines(ranges, top, cells(area));
    app.set_marks(fold_marks, copy_marks, link_marks);
    let visible: Vec<Line> = lines.into_iter().skip(top).take(height).collect();
    frame.render_widget(Paragraph::new(visible), area);
}

fn render_activity(frame: &mut Frame, app: &App, area: Rect) {
    let Some(elapsed) = app.elapsed() else {
        // While a command is being completed, its candidates take this line.
        if let Some(matches) = app.completions() {
            let line = Line::styled(format!("  {}", matches.join("   ")), fg(Color::Gray));
            frame.render_widget(Paragraph::new(line), area);
        }
        return;
    };
    let glyph = SPINNER[app.spinner() % SPINNER.len()];
    let mut spans = vec![Span::styled(format!("{glyph} Working… "), fg(ACCENT))];
    if let Some(step) = app.step() {
        spans.push(Span::styled(
            format!("{} · ", step.to_lowercase()),
            fg(Color::Gray),
        ));
    }
    match app.working_model() {
        Some(model) => spans.push(Span::styled(
            format!("{model} "),
            fg(Color::White).add_modifier(Modifier::BOLD),
        )),
        None if app.step().is_some() => {
            spans.push(Span::styled("ironquill ", fg(Color::Gray)));
        }
        None => {}
    }
    // The answering model's own cost so far, named when a sub-agent is the
    // one working, whose cost is in its pane.
    let spent = app.request_spent();
    if spent.usage.input.0 > 0 {
        // In a pair every step counts toward it; with a sub-agent at work,
        // its own cost is in its pane and this is the rest.
        let lead = app
            .current_model()
            .filter(|lead| app.working_model() != Some(*lead));
        let text = match lead {
            _ if app.step().is_some() => format!("· request so far {spent} "),
            Some(lead) => format!("· {lead} so far {spent} "),
            None => format!("· {spent} "),
        };
        spans.push(Span::styled(text, fg(Color::Gray)));
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
        .title(team_title(app))
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
    // A pasted line break shows as ↵; the message keeps it.
    let full: Vec<char> = prompt
        .chars()
        .chain(
            editor
                .text()
                .chars()
                .map(|c| if c == '\n' { '↵' } else { c }),
        )
        .collect();
    let cursor = prompt.chars().count() + editor.cursor();
    let width = width.max(1);
    let start = (cursor + 1).saturating_sub(width);
    let shown: String = full.iter().skip(start).take(width).collect();
    (shown, cursor - start)
}

/// Who answers and its team, on the message box, always in view.
fn team_title(app: &App) -> Line<'static> {
    let Some(lead) = app.current_model() else {
        return Line::default();
    };
    let mut spans = Vec::new();
    // While a step of a pair runs, who works comes first, so that nobody
    // takes the model that usually answers for the one at work.
    if let Some(step) = app.step() {
        let who = app
            .working_model()
            .map_or_else(|| "ironquill".to_owned(), ToString::to_string);
        spans.push(Span::styled(" now ", fg(DIM)));
        spans.push(Span::styled(
            who,
            fg(Color::White).add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(format!(" {} ·", step.to_lowercase()), fg(DIM)));
    }
    spans.push(Span::styled(" answers ", fg(DIM)));
    spans.push(Span::styled(lead.to_string(), fg(ACCENT)));
    let team: Vec<String> = app
        .team()
        .iter()
        .filter(|m| *m != lead)
        .map(ToString::to_string)
        .collect();
    spans.push(Span::styled(" · team ", fg(DIM)));
    if team.is_empty() {
        spans.push(Span::styled("none", fg(DIM)));
    } else {
        spans.push(Span::raw(team.join(", ")));
    }
    spans.push(Span::raw(" "));
    Line::from(spans)
}

/// The effort and the budget, for the status line.
fn team_and_budget(app: &App) -> String {
    let budget = app
        .budget()
        .map(|b| format!(" · budget {b}"))
        .unwrap_or_default();
    format!(" · effort {}{budget}", app.effort())
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
                "{rest}{} · {} in · {} out · {}{}{}  ",
                team_and_budget(app),
                usage.input,
                usage.output,
                cost,
                if complete { "" } else { "+?" },
                app.context()
                    .map(|c| format!(" · ctx {}%", c.percent()))
                    .unwrap_or_default(),
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

/// The usage pane: each model's cost added up over the window, the
/// conversation's context with the calls that rebuilt their cache, and a
/// line per model.
fn render_usage(frame: &mut Frame, app: &App, area: Rect) {
    let Some((log, window)) = app.usage_pane() else {
        return;
    };
    let now = sessions::now();
    let from = now.saturating_sub(window);
    let span = window as f64;
    let block = pane_block(
        format!(
            " Usage · last {} ",
            ironquill_ui::usage::window_name(window)
        ),
        false,
    );
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let models = log.models(from);
    if models.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::styled(" No call in this window yet", fg(DIM))),
            inner,
        );
        return;
    }
    let table_height = models.len() as u16 * 2;
    let [costs, contexts, table] = Layout::vertical([
        Constraint::Percentage(50),
        Constraint::Min(4),
        Constraint::Length(table_height),
    ])
    .areas(inner);

    // Cost, each model a line that steps up at each of its calls and runs
    // on to now.
    let steps: Vec<(Color, Vec<(f64, f64)>)> = models
        .iter()
        .map(|m| {
            let mut points = log.cost_steps(&m.model, from);
            let last = points.last().map_or(0.0, |p| p.1);
            points.push((span, last));
            (rgb(m.color), points)
        })
        .collect();
    let top = models
        .iter()
        .map(|m| m.cost)
        .fold(0.0_f64, f64::max)
        .max(0.001)
        * 1.1;
    let datasets: Vec<Dataset> = steps
        .iter()
        .map(|(color, points)| {
            Dataset::default()
                .marker(Marker::Braille)
                .graph_type(GraphType::Line)
                .style(fg(*color))
                .data(points)
        })
        .collect();
    let time_axis = || {
        Axis::default().bounds([0.0, span]).labels([
            Span::styled(
                format!("-{}", ironquill_ui::usage::window_name(window)),
                fg(DIM),
            ),
            Span::styled("now", fg(DIM)),
        ])
    };
    frame.render_widget(
        Chart::new(datasets)
            .x_axis(time_axis())
            .y_axis(Axis::default().bounds([0.0, top]).labels([
                Span::styled("$0", fg(DIM)),
                Span::styled(format!("${top:.2}"), fg(DIM)),
            ])),
        costs,
    );

    // The conversation's context, and the calls that wrote most of their
    // input to the cache: it had expired.
    let context = log.context_line(from);
    let rebuilds = log.rebuilds(from);
    let most = context
        .iter()
        .chain(&rebuilds)
        .map(|p| p.1)
        .fold(0.0_f64, f64::max)
        .max(1_000.0)
        * 1.1;
    let datasets = vec![
        Dataset::default()
            .name("context")
            .marker(Marker::Braille)
            .graph_type(GraphType::Line)
            .style(fg(Color::Gray))
            .data(&context),
        Dataset::default()
            .name("cache rebuilt")
            .marker(Marker::Braille)
            .graph_type(GraphType::Scatter)
            .style(fg(Color::Rgb(213, 94, 0)))
            .data(&rebuilds),
    ];
    frame.render_widget(
        Chart::new(datasets)
            .x_axis(time_axis())
            .y_axis(Axis::default().bounds([0.0, most]).labels([
                Span::styled("0", fg(DIM)),
                Span::styled(TokenCount(most as u64).to_string(), fg(DIM)),
            ]))
            .legend_position(Some(LegendPosition::TopLeft)),
        contexts,
    );

    let mut lines = Vec::new();
    for m in &models {
        let cache = m.cache_share.map_or_else(
            || "cache ?".to_owned(),
            |s| format!("cache {:.0}%", s * 100.0),
        );
        let rebuilt = if m.rebuilds == 0 {
            String::new()
        } else {
            format!(" · {} rebuilt", m.rebuilds)
        };
        lines.push(Line::from(vec![
            Span::styled("● ", fg(rgb(m.color))),
            Span::raw(m.model.clone()),
        ]));
        lines.push(Line::styled(
            format!(
                "  ${:.3} · {} call{} · {cache}{rebuilt}",
                m.cost,
                m.calls,
                if m.calls == 1 { "" } else { "s" }
            ),
            fg(Color::Gray),
        ));
    }
    frame.render_widget(Paragraph::new(lines), table);
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use ironquill_agent::{Approval, Compaction, Event, Question, Subject};
    use ironquill_core::{CacheUse, ContextUse, ModelId, TokenCount, Usage, Usd};
    use ironquill_tools::Check;
    use ironquill_ui::input::{KeyCode, KeyEvent, KeyModifiers};
    use ironquill_ui::{AgentMessage, Effect, Settings};
    use tokio::sync::oneshot;

    use super::*;
    use crate::pictures::NoPictures;

    fn press(app: &mut App, code: KeyCode) -> Option<Effect> {
        app.on_key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn type_text(app: &mut App, text: &str) {
        for c in text.chars() {
            press(app, KeyCode::Char(c));
        }
    }

    fn ready() -> App {
        App::new(
            Settings {
                tiers: vec![ModelId::new("cheap").unwrap()],
                checks: vec![Check::parse("cargo check").unwrap()],
                rounds: 2,
                max_turns: 30,
                ..Settings::default()
            },
            PathBuf::from("/p"),
        )
    }

    fn project() -> (tempfile::TempDir, App) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/lib.rs"), "one\ntwo\n").unwrap();
        let app = App::new(
            Settings {
                tiers: vec![ModelId::new("cheap").unwrap()],
                ..Settings::default()
            },
            dir.path().to_owned(),
        );
        (dir, app)
    }

    /// A reply from the model, as the agent reports it.
    fn said(app: &mut App, text: &str) {
        app.on_agent(AgentMessage::Event(Event::Said {
            model: ModelId::new("cheap").unwrap(),
            text: text.into(),
        }));
    }

    fn screen_of(app: &App, width: u16, height: u16) -> String {
        let backend = ratatui::backend::TestBackend::new(width, height);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| render(frame, app, &NoPictures))
            .unwrap();
        let buffer = terminal.backend().buffer();
        buffer
            .content()
            .chunks(usize::from(buffer.area.width))
            .map(|row| row.iter().map(|c| c.symbol()).collect::<String>() + "\n")
            .collect()
    }

    fn screen(app: &App) -> String {
        screen_of(app, 120, 40)
    }

    #[test]
    fn the_usage_pane_shows_what_each_model_used() {
        let mut app = ready();
        for (model, cost, written) in [("glm", 0.01, 500), ("opus", 0.2, 9_000), ("glm", 0.01, 400)]
        {
            app.on_agent(AgentMessage::Event(Event::Turn {
                model: ModelId::new(model).unwrap(),
                usage: Usage {
                    input: TokenCount(10_000),
                    output: TokenCount(100),
                },
                cost: Some(Usd(cost)),
                subscription: false,
                context: Some(ContextUse {
                    used: TokenCount(10_000),
                    window: TokenCount(200_000),
                }),
                cache: Some(CacheUse {
                    read: TokenCount(10_000 - written),
                    written: Some(TokenCount(written)),
                }),
            }));
        }
        type_text(&mut app, "/usage 6h");
        press(&mut app, KeyCode::Enter);
        let screen = screen_of(&app, 140, 40);
        assert!(screen.contains("Usage · last 6h"));
        assert!(screen.contains("$0.020 · 2 calls · cache 96%"), "{screen}");
        assert!(screen.contains("$0.200 · 1 call · cache 10% · 1 rebuilt"));
    }

    #[test]
    fn a_pasted_log_shows_its_line_breaks_and_a_question_its_window() {
        let mut app = ready();
        app.on_paste("error: one\r\nerror: two\n");
        assert!(screen(&app).contains("error: one↵error: two↵"));

        let (answer, _answered) = oneshot::channel();
        app.on_agent(AgentMessage::Approve(
            Approval {
                model: ModelId::new("cheap").unwrap(),
                question: Question::MoreTurns { turns: 30 },
            },
            answer,
        ));
        assert!(screen(&app).contains(" Go on? "));
    }

    #[test]
    fn a_stopped_request_says_so() {
        let mut app = ready();
        app.on_cancelled();
        assert!(screen(&app).contains("Interrupted: the request stopped before it ended"));
    }

    #[test]
    fn a_secret_can_be_allowed_for_good_from_its_window() {
        let mut app = ready();
        let (answer, _answered) = oneshot::channel();
        app.on_agent(AgentMessage::Approve(
            Approval {
                model: ModelId::new("cheap").unwrap(),
                question: Question::Command {
                    command: "curl -H \"X: $API_TOKEN\" https://x.example.com".into(),
                    reasons: vec!["it uses the secret API_TOKEN".into()],
                    secrets: vec!["API_TOKEN".into()],
                    hosts: vec![],
                },
            },
            answer,
        ));
        assert!(screen(&app).contains("a: always allow API_TOKEN"));
    }

    #[test]
    fn the_compact_window_ticks_subjects_whole_or_in_part() {
        let mut app = ready();
        type_text(&mut app, "/compact");
        press(&mut app, KeyCode::Enter);
        app.on_agent(AgentMessage::Compaction(Ok(Compaction {
            exchanges: vec![
                "fix the parser".into(),
                "add a test".into(),
                "the docs".into(),
            ],
            subjects: vec![
                Subject {
                    name: "Parser".into(),
                    exchanges: vec![0, 1],
                },
                Subject {
                    name: "Docs".into(),
                    exchanges: vec![2],
                },
            ],
        })));
        assert!(screen(&app).contains("[x] Parser (2 exchanges)"));
        press(&mut app, KeyCode::Right);
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Char(' '));
        assert!(screen(&app).contains("[-] Parser (2 exchanges)"));
    }

    #[test]
    fn a_command_folds_to_a_line_and_copies() {
        let mut app = ready();
        app.on_agent(AgentMessage::Event(Event::Command {
            model: ModelId::new("cheap").unwrap(),
            command: "kubectl -n web get pods\n  -o wide".into(),
            status: "exit status 0".into(),
            output: "api-1 Running\napi-2 Running\n".into(),
            checked_by: Some(ModelId::new("glm").unwrap()),
        }));
        let folded = screen(&app);
        assert!(folded.contains("Run(kubectl -n web get pods…)"), "{folded}");
        assert!(
            folded.contains("exit status 0 · checked by glm · 2 lines · click or Enter to show")
        );
        assert!(!folded.contains("api-1 Running"));

        // Selected and opened, then copied.
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Up);
        press(&mut app, KeyCode::Enter);
        assert!(screen(&app).contains("api-1 Running"));
        assert!(matches!(
            press(&mut app, KeyCode::Char('y')),
            Some(Effect::Copy(text)) if text.starts_with("kubectl -n web get pods") && text.ends_with("api-2 Running\n")
        ));
    }

    #[test]
    fn a_code_block_copies_with_a_click_and_only_its_mark_folds() {
        use ironquill_ui::input::{MouseButton, MouseEvent, MouseEventKind};
        let mut app = ready();
        let long: String = (0..20).map(|i| format!("line {i}\n")).collect();
        said(
            &mut app,
            &format!("Try:\n```sh\ncargo test -q\n```\n{long}"),
        );
        said(&mut app, &long);
        let screen_text = screen(&app);
        let rows: Vec<&str> = screen_text.lines().collect();
        let row_of = |needle: &str| rows.iter().position(|r| r.contains(needle)).unwrap() as u16;
        let click = |app: &mut App, row: u16| {
            app.on_mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 5,
                row,
                modifiers: KeyModifiers::NONE,
            })
        };
        // The first reply is folded, the latest open.
        assert!(screen_text.contains("▸ "));
        assert!(screen_text.contains("▾ fold"));
        assert!(matches!(
            click(&mut app, row_of("⧉ copy")),
            Some(Effect::Copy(code)) if code == "cargo test -q"
        ));
        // A click on the text does not fold; one on the mark does.
        click(&mut app, row_of("Try:"));
        assert!(!app.is_expanded(1));
        screen(&app);
        click(&mut app, row_of("▸ "));
        assert!(app.is_expanded(1));
    }

    #[test]
    fn a_cited_file_opens_at_its_line_with_a_click() {
        use ironquill_ui::input::{MouseButton, MouseEvent, MouseEventKind};
        let (_dir, mut app) = project();
        said(
            &mut app,
            "The second line is in src/lib.rs:2, not in nowhere.rs:1.",
        );
        let shown = screen(&app);
        assert!(shown.contains("src/lib.rs:2↗"));
        assert!(!shown.contains("nowhere.rs:1↗"));
        let (row, column) = shown
            .lines()
            .enumerate()
            .find_map(|(r, l)| l.find("src/lib.rs:2").map(|c| (r, l[..c].chars().count())))
            .unwrap();
        app.on_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: column as u16 + 2,
            row: row as u16,
            modifiers: KeyModifiers::NONE,
        });
        let file = app.file().expect("the file is open");
        assert_eq!(file.lines(), ["one", "two"]);
        assert_eq!(file.cursor().0, 1);
    }
}
