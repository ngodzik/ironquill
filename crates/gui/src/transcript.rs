//! The conversation, drawn from the state's transcript: what the person
//! wrote, what the models answered and did, and what each request cost.

use std::ops::Range;
use std::sync::Arc;

use bevy_egui::egui::{
    self, Align, Color32, CornerRadius, FontId, Frame, Layout, Margin, RichText, Sense, Stroke,
    TextFormat, Ui, text::LayoutJob,
};
use universe_tools::{DiffLine, ToolSummary};
use universe_ui::markdown::{self as md, Block, Marker};
use universe_ui::references::{self, Reference};
use universe_ui::{App, Effect, Entry, step_title};

use crate::theme::{
    ACCENT, BODY_SIZE, CODE, CYAN, DIM, EDGE, GREEN, LINK, MAGENTA, RED, TEXT, YELLOW,
};

/// Where a click on a cited file or commit is left, in egui's memory, for
/// the window to open once the drawing is done: the transcript only reads
/// the state.
pub(crate) const CITED: &str = "cited";

/// How many lines of a failed check's output show.
const EXCERPT_LINES: usize = 12;

/// How many lines of a diff show under an edit.
const DIFF_LINES: usize = 40;

/// Draws the whole transcript. Clicks that only the loop can carry out, a
/// copy, are added to `effects`.
pub(crate) fn show(ui: &mut Ui, app: &App, effects: &mut Vec<Effect>) {
    let selected = app.selected_reply();
    let reveal = app.take_reveal();
    for (index, entry) in app.transcript().iter().enumerate() {
        let is_selected = selected == Some(index);
        let frame = Frame::new()
            .inner_margin(Margin::symmetric(10, 6))
            .corner_radius(CornerRadius::same(8))
            .fill(if is_selected {
                ui.visuals().selection.bg_fill
            } else {
                Color32::TRANSPARENT
            });
        let response = frame
            .show(ui, |ui| entry_ui(ui, app, entry, effects))
            .response;
        if is_selected && reveal {
            response.scroll_to_me(Some(Align::Center));
        }
    }
}

/// One entry of the transcript, as the conversation shows it.
pub(crate) fn entry_ui(ui: &mut Ui, app: &App, entry: &Entry, effects: &mut Vec<Effect>) {
    match entry {
        Entry::Welcome => welcome(ui, app),
        Entry::Info(text) => wrapped(ui, RichText::new(text).color(DIM)),
        Entry::Ended(text) => marked(
            ui,
            "■",
            MAGENTA,
            RichText::new(text).color(MAGENTA).strong(),
        ),
        Entry::Refused(text) => marked(ui, "⊘", YELLOW, RichText::new(text).color(YELLOW)),
        Entry::Interrupted => marked(
            ui,
            "■",
            RED,
            RichText::new("Interrupted: the request stopped before it ended")
                .color(RED)
                .strong(),
        ),
        Entry::Error(text) => marked(ui, "✗", RED, RichText::new(text).color(RED)),
        Entry::User(text) => user(ui, text),
        Entry::Said(text) => {
            // The bullet painted in the margin, so that the reply keeps the
            // whole width: a code block inside a row would overflow it.
            let top = ui.cursor().min;
            let row = ui.text_style_height(&egui::TextStyle::Body);
            ui.painter()
                .circle_filled(egui::pos2(top.x + 5.0, top.y + row / 2.0 + 1.0), 4.0, TEXT);
            Frame::new()
                .inner_margin(Margin {
                    left: 20,
                    ..Margin::ZERO
                })
                .show(ui, |ui| markdown(ui, app, text, effects));
        }
        Entry::Command {
            command,
            status,
            output,
            checked_by,
            ..
        } => {
            action(ui, CYAN, "Run", "");
            indented(ui, |ui| {
                ui.label(RichText::new(command).monospace().color(TEXT));
                let mut details = status.clone();
                if let Some(by) = checked_by {
                    details.push_str(&format!(" · checked by {by}"));
                }
                details.push_str(&format!(
                    " · {}",
                    plural(output.lines().count(), "line", "lines")
                ));
                ui.label(RichText::new(details).small().color(DIM));
                if !output.is_empty() {
                    egui::CollapsingHeader::new(RichText::new("output").small().color(DIM))
                        .id_salt(command)
                        .show(ui, |ui| {
                            ui.label(RichText::new(output).monospace().color(DIM));
                        });
                }
            });
        }
        Entry::Tool {
            name,
            path,
            outcome,
        } => tool(ui, name, path.as_deref(), outcome),
        Entry::Checks(commands) => action(ui, YELLOW, "Checks", &commands.join(" · ")),
        Entry::Passed => result(ui, "All checks passed", GREEN),
        Entry::Failed { command, excerpt } => {
            result(ui, &format!("{command} failed"), RED);
            let excerpt: Vec<&str> = excerpt.lines().take(EXCERPT_LINES).collect();
            indented(ui, |ui| {
                ui.label(RichText::new(excerpt.join("\n")).monospace().color(DIM));
            });
        }
        Entry::Escalating { from, to } => {
            action(ui, MAGENTA, "Escalate", &format!("{from} → {to}"));
            result(
                ui,
                &format!("The checks kept failing, handing over to {to}"),
                DIM,
            );
        }
        Entry::GaveUp => marked(
            ui,
            "✗",
            RED,
            RichText::new(
                "The checks still fail after every model tried. /diff shows what changed",
            )
            .color(RED),
        ),
        Entry::Delegating { from, to, task, .. } => {
            action(ui, MAGENTA, "Delegate", &format!("{from} → {to}"));
            result(ui, task, DIM);
        }
        Entry::OverBudget { spent, budget } => marked(
            ui,
            "✗",
            YELLOW,
            RichText::new(format!(
                "Budget of {budget} for this request reached ({spent}): the work stopped. \
                 /budget <dollars> changes it"
            ))
            .color(YELLOW),
        ),
        Entry::Step {
            number,
            of,
            name,
            model,
            effort,
        } => {
            let coding = name.starts_with("Coding") || name.starts_with("Fixing");
            let colour = match (model, coding) {
                (None, _) => DIM,
                (Some(_), true) => ACCENT,
                (Some(_), false) => MAGENTA,
            };
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.add(
                    egui::Label::new(
                        RichText::new(step_title(*number, *of, name, model.as_ref(), *effort))
                            .color(colour)
                            .strong(),
                    )
                    .truncate(),
                );
                let rest = ui.available_width();
                let (rect, _) = ui.allocate_exact_size(egui::vec2(rest, 2.0), egui::Sense::hover());
                ui.painter()
                    .rect_filled(rect, 1.0, colour.gamma_multiply(0.5));
            });
        }
        Entry::Member { model, entry } => {
            Frame::new()
                .stroke(Stroke::NONE)
                .inner_margin(Margin {
                    left: 12,
                    ..Margin::ZERO
                })
                .show(ui, |ui| {
                    let rect = ui.max_rect();
                    ui.label(RichText::new(model.to_string()).color(MAGENTA).small());
                    entry_ui(ui, app, entry, effects);
                    let bar = egui::Rect::from_min_max(
                        rect.left_top() - egui::vec2(10.0, 0.0),
                        egui::pos2(rect.left() - 8.0, ui.min_rect().bottom()),
                    );
                    ui.painter()
                        .rect_filled(bar, 1.0, MAGENTA.gamma_multiply(0.7));
                });
        }
        Entry::Cost {
            usage,
            cost,
            complete,
            seconds,
            subscription,
            ..
        } => {
            let cost = match (*subscription, cost.0 > 0.0, *complete) {
                (true, false, _) => "subscription".to_owned(),
                (true, true, _) => format!("{cost} + subscription"),
                (false, _, true) => cost.to_string(),
                (false, _, false) => format!("{cost} reported, part of the cost unknown"),
            };
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(
                    RichText::new(format!(
                        "{cost} · {} in · {} out · {seconds}s",
                        usage.input, usage.output
                    ))
                    .small()
                    .color(DIM),
                );
            });
        }
        Entry::Image(path) => {
            ui.label(RichText::new(path).small().color(DIM));
            let pictures: Option<crate::diagrams::Diagrams> =
                ui.data(|d| d.get_temp(egui::Id::new("diagrams")));
            if let Some(pictures) = pictures {
                pictures.picture(ui, &app.root().join(path));
            }
        }
    }
}

/// The greeting: the project, the model, and how to start.
fn welcome(ui: &mut Ui, app: &App) {
    ui.add_space(8.0);
    ui.label(RichText::new("universe").size(26.0).strong().color(ACCENT));
    ui.label(RichText::new(app.root().display().to_string()).color(DIM));
    ui.add_space(4.0);
    if let Some(model) = app.current_model() {
        ui.horizontal(|ui| {
            ui.label(RichText::new("answers").color(DIM));
            ui.label(RichText::new(app.model_label(model)).color(ACCENT));
        });
    }
    ui.label(
        RichText::new(
            "i to write · Ctrl-O for the costs · Ctrl-S for every shortcut · Ctrl-C twice to quit",
        )
        .small()
        .color(DIM),
    );
    ui.add_space(8.0);
}

/// What the person wrote, set apart on a card.
fn user(ui: &mut Ui, text: &str) {
    Frame::new()
        .fill(ui.visuals().faint_bg_color)
        .stroke(Stroke::new(1.0, EDGE))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::symmetric(12, 8))
        .show(ui, |ui| {
            ui.horizontal_top(|ui| {
                ui.label(RichText::new("›").color(ACCENT).strong());
                wrapped(ui, RichText::new(text).color(TEXT));
            });
        });
}

fn marked(ui: &mut Ui, mark: &str, colour: Color32, text: RichText) {
    ui.horizontal_top(|ui| {
        ui.label(RichText::new(mark).color(colour));
        wrapped(ui, text);
    });
}

/// Text that wraps in what is left of the row, as a label beside a mark
/// does not by itself.
fn wrapped(ui: &mut Ui, text: RichText) {
    ui.vertical(|ui| {
        ui.add(egui::Label::new(text).wrap());
    });
}

/// A step a model took: a coloured bullet, its name in bold, what it acted on.
///
/// The name and what it acted on wrap, as all text here does: one line
/// wider than the pane (a long command) widens all that comes after it,
/// which then wraps past the pane's edge, and the pane springs back to that
/// width when dragged narrower.
fn action(ui: &mut Ui, colour: Color32, name: &str, argument: &str) {
    ui.horizontal_top(|ui| {
        ui.label(RichText::new("●").color(colour));
        let mut job = LayoutJob::default();
        let style = ui.style().clone();
        RichText::new(name).strong().color(TEXT).append_to(
            &mut job,
            &style,
            egui::FontSelection::Default,
            Align::Min,
        );
        if !argument.is_empty() {
            RichText::new(format!("  {argument}")).color(DIM).append_to(
                &mut job,
                &style,
                egui::FontSelection::Default,
                Align::Min,
            );
        }
        ui.vertical(|ui| {
            ui.add(egui::Label::new(job).wrap());
        });
    });
}

/// What came of a step, under it.
fn result(ui: &mut Ui, text: &str, colour: Color32) {
    ui.horizontal_top(|ui| {
        ui.label(RichText::new("  └").color(DIM));
        wrapped(ui, RichText::new(text).color(colour));
    });
}

fn indented(ui: &mut Ui, add: impl FnOnce(&mut Ui)) {
    ui.horizontal_top(|ui| {
        ui.add_space(22.0);
        ui.vertical(add);
    });
}

pub(crate) fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

fn tool(ui: &mut Ui, name: &str, path: Option<&str>, outcome: &Result<ToolSummary, String>) {
    match outcome {
        Err(error) => {
            let label = match name {
                "read_file" => "Read",
                "list_dir" => "List",
                "replace" => "Update",
                "write_file" => "Write",
                other => other,
            };
            action(ui, RED, label, path.unwrap_or(""));
            result(ui, &format!("Error: {error}"), RED);
        }
        Ok(ToolSummary::Read { path, lines }) => {
            action(ui, GREEN, "Read", path);
            result(
                ui,
                &format!("Read {}", plural(*lines, "line", "lines")),
                DIM,
            );
        }
        Ok(ToolSummary::Ran { label, lines }) => {
            action(ui, GREEN, label, "");
            result(ui, &plural(*lines, "line", "lines"), DIM);
        }
        Ok(ToolSummary::Listed { path, entries }) => {
            action(ui, GREEN, "List", path);
            result(
                ui,
                &format!("Listed {}", plural(*entries, "entry", "entries")),
                DIM,
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
            if *created {
                action(ui, GREEN, "Create", path);
                result(
                    ui,
                    &format!("Created with {}", plural(added, "line", "lines")),
                    DIM,
                );
            } else {
                action(ui, GREEN, "Update", path);
                result(
                    ui,
                    &format!(
                        "{} and {}",
                        plural(added, "addition", "additions"),
                        plural(removed, "removal", "removals")
                    ),
                    DIM,
                );
            }
            indented(ui, |ui| diff_block(ui, diff));
        }
    }
}

/// A file's change, its added and removed lines on tinted rows.
fn diff_block(ui: &mut Ui, diff: &[DiffLine]) {
    Frame::new()
        .fill(ui.visuals().faint_bg_color)
        .corner_radius(CornerRadius::same(6))
        .inner_margin(Margin::symmetric(8, 6))
        .show(ui, |ui| {
            for line in diff.iter().take(DIFF_LINES) {
                let (sign, text, colour) = match line {
                    DiffLine::Added(t) => ("+", t, GREEN),
                    DiffLine::Removed(t) => ("-", t, RED),
                    DiffLine::Context(t) => (" ", t, DIM),
                };
                ui.label(
                    RichText::new(format!("{sign} {text}"))
                        .monospace()
                        .color(colour),
                );
            }
            if diff.len() > DIFF_LINES {
                ui.label(
                    RichText::new(format!("… {} more", diff.len() - DIFF_LINES))
                        .small()
                        .color(DIM),
                );
            }
        });
}

/// A reply's Markdown, as `universe_ui::markdown` reads it: headings by
/// level, lists nested, numbered or ticked, quotes behind a bar with a copy
/// button, rules, tables, and fenced blocks with a copy button.
fn markdown(ui: &mut Ui, app: &App, text: &str, effects: &mut Vec<Effect>) {
    blocks(ui, app, &md::parse(text), effects);
}

fn blocks(ui: &mut Ui, app: &App, list: &[Block], effects: &mut Vec<Effect>) {
    for block in list {
        match block {
            Block::Blank => ui.add_space(4.0),
            Block::Heading { level, spans } => {
                ui.add_space(4.0);
                let size = match level {
                    1 => BODY_SIZE + 6.0,
                    2 => BODY_SIZE + 3.5,
                    3 => BODY_SIZE + 1.5,
                    _ => BODY_SIZE,
                };
                let mut job = LayoutJob::default();
                runs(
                    &mut job,
                    spans,
                    size,
                    Color32::WHITE,
                    ui.visuals().faint_bg_color,
                );
                job.wrap.max_width = ui.available_width();
                citing(ui, app, job);
            }
            Block::Paragraph(lines) => {
                let mut job = LayoutJob::default();
                for (i, line) in lines.iter().enumerate() {
                    if i > 0 {
                        job.append("\n", 0.0, plain());
                    }
                    runs(&mut job, line, BODY_SIZE, TEXT, ui.visuals().faint_bg_color);
                }
                job.wrap.max_width = ui.available_width();
                citing(ui, app, job);
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
                let colour = if matches!(marker, Marker::Task(true)) {
                    DIM
                } else {
                    TEXT
                };
                ui.horizontal_top(|ui| {
                    ui.add_space(4.0 + 16.0 * *depth as f32);
                    ui.label(RichText::new(mark).size(BODY_SIZE).color(DIM));
                    let mut job = LayoutJob::default();
                    runs(
                        &mut job,
                        spans,
                        BODY_SIZE,
                        colour,
                        ui.visuals().faint_bg_color,
                    );
                    job.wrap.max_width = ui.available_width();
                    citing(ui, app, job);
                });
            }
            Block::Rule => {
                ui.add_space(4.0);
                ui.separator();
                ui.add_space(4.0);
            }
            Block::Quote { text, blocks } => quote(ui, app, text, blocks, effects),
            Block::Code { language, text } => code_block(ui, language, text, effects),
            Block::Image { alt, path } => {
                ui.label(RichText::new(format!("![{alt}]({path})")).color(DIM));
            }
            Block::Table {
                header,
                aligns,
                rows,
            } => table(ui, app, header, aligns, rows),
        }
    }
}

/// A table on a card, its header bold, its columns aligned as written.
fn table(
    ui: &mut Ui,
    app: &App,
    header: &[Vec<md::Span>],
    aligns: &[md::Align],
    rows: &[Vec<Vec<md::Span>>],
) {
    Frame::new()
        .fill(ui.visuals().faint_bg_color)
        .stroke(Stroke::new(1.0, EDGE))
        .corner_radius(CornerRadius::same(6))
        .inner_margin(Margin::symmetric(10, 6))
        .show(ui, |ui| {
            egui::Grid::new(ui.next_auto_id())
                .striped(true)
                .spacing(egui::vec2(18.0, 4.0))
                .show(ui, |ui| {
                    let cells = |ui: &mut Ui, row: &[Vec<md::Span>], colour: Color32| {
                        for (c, cell) in row.iter().enumerate() {
                            let mut job = LayoutJob::default();
                            runs(
                                &mut job,
                                cell,
                                BODY_SIZE - 0.5,
                                colour,
                                ui.visuals().faint_bg_color,
                            );
                            let align = match aligns.get(c).copied().unwrap_or_default() {
                                md::Align::Left => Align::Min,
                                md::Align::Center => Align::Center,
                                md::Align::Right => Align::Max,
                            };
                            // A cell keeps to one line: the grid gives each
                            // column the room its widest cell needs.
                            job.wrap.max_width = f32::INFINITY;
                            ui.with_layout(Layout::top_down(align), |ui| citing(ui, app, job));
                        }
                        ui.end_row();
                    };
                    cells(ui, header, Color32::WHITE);
                    for row in rows {
                        cells(ui, row, TEXT);
                    }
                });
        });
}

/// A quote set apart by a bar on its left, its markers gone and its
/// text drawn as Markdown again, with a button that copies it as written
/// without them: a quoted draft is often what is wanted.
fn quote(ui: &mut Ui, app: &App, text: &str, inner: &[Block], effects: &mut Vec<Effect>) {
    let shown = Frame::new()
        .inner_margin(Margin {
            left: 12,
            right: 0,
            top: 2,
            bottom: 2,
        })
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                if ui.small_button(RichText::new("copy").color(DIM)).clicked() {
                    effects.push(Effect::Copy(text.to_owned()));
                }
                ui.with_layout(Layout::top_down(Align::Min), |ui| {
                    blocks(ui, app, inner, effects);
                });
            });
        });
    let rect = shown.response.rect;
    ui.painter().rect_filled(
        egui::Rect::from_min_size(rect.min, egui::vec2(3.0, rect.height())),
        CornerRadius::same(1),
        EDGE,
    );
}

fn plain() -> TextFormat {
    TextFormat {
        font_id: FontId::proportional(BODY_SIZE),
        color: TEXT,
        ..TextFormat::default()
    }
}

/// The runs of the shared reader, appended to `job` at `size`.
/// Inline code sits on `background`, the window's raised colour, as
/// see-through as the window.
fn runs(job: &mut LayoutJob, spans: &[md::Span], size: f32, colour: Color32, background: Color32) {
    for span in spans {
        let mut format = TextFormat {
            font_id: FontId::proportional(size),
            color: if span.bold { Color32::WHITE } else { colour },
            italics: span.italic,
            ..TextFormat::default()
        };
        if span.strike {
            format.strikethrough = Stroke::new(1.0, colour);
        }
        if span.code {
            format.font_id = FontId::monospace(size - 0.5);
            format.color = LINK;
            format.background = background;
        } else if span.link.is_some() {
            format.color = CYAN;
            format.underline = Stroke::new(1.0, CYAN);
        }
        job.append(&span.text, 0.0, format);
    }
}

/// `job` as a label, the files and commits it cites that exist drawn as
/// links: a click on one is left under [`CITED`] for the window to open.
fn citing(ui: &mut Ui, app: &App, mut job: LayoutJob) {
    let cited: Vec<(Range<usize>, Reference)> = references::candidates(&job.text)
        .into_iter()
        .filter(|(_, _, reference)| app.reference_exists(reference))
        .map(|(start, end, reference)| (start..end, reference))
        .collect();
    if cited.is_empty() {
        ui.label(job);
        return;
    }
    for (range, _) in &cited {
        link(&mut job, range);
    }
    let galley = ui.fonts_mut(|fonts| fonts.layout_job(job));
    let response = ui.add(egui::Label::new(Arc::clone(&galley)).sense(Sense::click()));
    // The reference under the pointer, by the character it is over.
    let under = |at: egui::Pos2| {
        let index = galley
            .cursor_from_pos(at - response.rect.left_top())
            .index
            .0;
        let byte = galley
            .job
            .text
            .char_indices()
            .nth(index)
            .map_or(galley.job.text.len(), |(byte, _)| byte);
        cited
            .iter()
            .find(|(range, _)| range.contains(&byte))
            .map(|(_, reference)| reference)
    };
    if let Some(reference) = response.hover_pos().and_then(under) {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        if response.clicked() {
            let reference = Some(reference.clone());
            ui.ctx()
                .data_mut(|d| d.insert_temp(egui::Id::new(CITED), reference));
        }
    }
}

/// The bytes of `range` in `job` drawn as a link: the sections split at its
/// edges, those inside it coloured and underlined.
fn link(job: &mut LayoutJob, range: &Range<usize>) {
    let mut sections = Vec::with_capacity(job.sections.len() + 2);
    for section in job.sections.drain(..) {
        let (start, end) = (
            usize::from(section.byte_range.start),
            usize::from(section.byte_range.end),
        );
        let cuts = [
            start,
            range.start.clamp(start, end),
            range.end.clamp(start, end),
            end,
        ];
        for pair in cuts.windows(2) {
            let (from, to) = (pair[0], pair[1]);
            if from >= to {
                continue;
            }
            let mut part = section.clone();
            part.byte_range = from.into()..to.into();
            if from != start {
                part.leading_space = 0.0;
            }
            if from >= range.start && to <= range.end {
                part.format.color = CYAN;
                part.format.underline = Stroke::new(1.0, CYAN);
            }
            sections.push(part);
        }
    }
    job.sections = sections;
}

/// A fenced block on its own card, its language above it and a copy button
/// beside; a diff tinted line by line.
fn code_block(ui: &mut Ui, language: &str, code: &str, effects: &mut Vec<Effect>) {
    Frame::new()
        .fill(ui.visuals().faint_bg_color)
        .stroke(Stroke::new(1.0, EDGE))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::symmetric(10, 8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                if ui.small_button(RichText::new("copy").color(DIM)).clicked() {
                    effects.push(Effect::Copy(code.to_owned()));
                }
                ui.label(RichText::new(language).small().color(DIM));
            });
            // A diagram drawn takes the place of its code.
            if language.split_whitespace().next() == Some("mermaid") {
                let diagrams: Option<crate::diagrams::Diagrams> =
                    ui.data(|d| d.get_temp(egui::Id::new("diagrams")));
                if diagrams.is_some_and(|d| d.show(ui, code)) {
                    return;
                }
            }
            let diff = language.split_whitespace().next() == Some("diff");
            for line in code.lines() {
                let colour = match line.chars().next() {
                    Some('+') if diff => GREEN,
                    Some('-') if diff => RED,
                    Some('@') if diff => CYAN,
                    _ => CODE,
                };
                ui.label(RichText::new(line).monospace().color(colour));
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pieces(line: &str) -> Vec<String> {
        let mut job = LayoutJob::default();
        runs(
            &mut job,
            &md::inline(line),
            BODY_SIZE,
            TEXT,
            crate::theme::RAISED,
        );
        job.sections
            .iter()
            .map(|s| job.text[s.byte_range.start.0..s.byte_range.end.0].to_owned())
            .filter(|s| !s.is_empty())
            .collect()
    }

    #[test]
    fn inline_code_and_bold_lose_their_markers() {
        assert_eq!(
            pieces("run `cargo test` now **please**"),
            ["run ", "cargo test", " now ", "please"]
        );
    }

    #[test]
    fn an_unclosed_marker_stays_as_written() {
        assert_eq!(pieces("a `b"), ["a `b"]);
        assert_eq!(pieces("2 ** 3"), ["2 ** 3"]);
    }

    #[test]
    fn a_link_splits_the_sections_it_crosses_and_keeps_the_rest() {
        let mut job = LayoutJob::default();
        // Formats apart, or appending would merge them into one section.
        let code = TextFormat {
            font_id: FontId::monospace(BODY_SIZE),
            ..plain()
        };
        job.append("see ", 0.0, plain());
        job.append("src/a.rs:3", 0.0, code);
        job.append(" now", 0.0, plain());
        // From inside the first section to inside the second.
        link(&mut job, &(2..8));
        let parts: Vec<(&str, bool)> = job
            .sections
            .iter()
            .map(|s| {
                let range = usize::from(s.byte_range.start)..usize::from(s.byte_range.end);
                (&job.text[range], s.format.color == CYAN)
            })
            .collect();
        assert_eq!(
            parts,
            [
                ("se", false),
                ("e ", true),
                ("src/", true),
                ("a.rs:3", false),
                (" now", false)
            ]
        );
    }
}
