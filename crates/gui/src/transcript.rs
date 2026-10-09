//! The conversation, drawn from the state's transcript: what the person
//! wrote, what the models answered and did, and what each request cost.

use bevy_egui::egui::{
    self, Align, Color32, CornerRadius, FontId, Frame, Layout, Margin, RichText, Stroke,
    TextFormat, Ui, text::LayoutJob,
};
use ironquill_tools::{DiffLine, ToolSummary};
use ironquill_ui::{App, Effect, Entry, step_title};

use crate::theme::{
    ACCENT, BODY_SIZE, CODE, CYAN, DIM, EDGE, GREEN, LINK, MAGENTA, RAISED, RED, SELECTED, TEXT,
    YELLOW,
};

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
                SELECTED
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

fn entry_ui(ui: &mut Ui, app: &App, entry: &Entry, effects: &mut Vec<Effect>) {
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
                .show(ui, |ui| markdown(ui, text, effects));
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
                ui.label(
                    RichText::new(step_title(*number, *of, name, model.as_ref(), *effort))
                        .color(colour)
                        .strong(),
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
            ui.label(
                RichText::new(format!("{path} (pictures come to the window next)")).color(DIM),
            );
        }
    }
}

/// The greeting: the project, the model, and how to start.
fn welcome(ui: &mut Ui, app: &App) {
    ui.add_space(8.0);
    ui.label(RichText::new("ironquill").size(26.0).strong().color(ACCENT));
    ui.label(RichText::new(app.root().display().to_string()).color(DIM));
    ui.add_space(4.0);
    if let Some(model) = app.current_model() {
        ui.horizontal(|ui| {
            ui.label(RichText::new("answers").color(DIM));
            ui.label(RichText::new(model.to_string()).color(ACCENT));
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
        .fill(RAISED)
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
fn action(ui: &mut Ui, colour: Color32, name: &str, argument: &str) {
    ui.horizontal(|ui| {
        ui.label(RichText::new("●").color(colour));
        ui.label(RichText::new(name).strong());
        if !argument.is_empty() {
            ui.label(RichText::new(argument).color(DIM));
        }
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

fn plural(n: usize, one: &str, many: &str) -> String {
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
        .fill(RAISED)
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

/// A reply's Markdown, as much of it as reads better drawn: headings, bold,
/// inline code, and fenced blocks with a copy button.
fn markdown(ui: &mut Ui, text: &str, effects: &mut Vec<Effect>) {
    let mut lines = text.lines().peekable();
    let mut paragraph: Vec<&str> = Vec::new();
    while let Some(line) = lines.next() {
        if let Some(info) = line.trim_start().strip_prefix("```") {
            flush(ui, &mut paragraph);
            let mut code = Vec::new();
            for inner in lines.by_ref() {
                if inner.trim_start().starts_with("```") {
                    break;
                }
                code.push(inner);
            }
            code_block(ui, info.trim(), &code.join("\n"), effects);
        } else if let Some(heading) = heading(line) {
            flush(ui, &mut paragraph);
            ui.add_space(4.0);
            ui.label(RichText::new(heading).size(BODY_SIZE + 3.0).strong());
        } else if line.trim().is_empty() {
            flush(ui, &mut paragraph);
            ui.add_space(4.0);
        } else {
            paragraph.push(line);
        }
    }
    flush(ui, &mut paragraph);
}

fn heading(line: &str) -> Option<&str> {
    let trimmed = line.trim_start_matches('#');
    (trimmed.len() < line.len() && trimmed.starts_with(' ')).then(|| trimmed.trim())
}

/// Draws the lines gathered as one block of text, line breaks kept.
fn flush(ui: &mut Ui, paragraph: &mut Vec<&str>) {
    if paragraph.is_empty() {
        return;
    }
    let mut job = LayoutJob::default();
    for (i, line) in paragraph.iter().enumerate() {
        if i > 0 {
            job.append("\n", 0.0, plain());
        }
        inline(&mut job, line);
    }
    job.wrap.max_width = ui.available_width();
    ui.label(job);
    paragraph.clear();
}

fn plain() -> TextFormat {
    TextFormat {
        font_id: FontId::proportional(BODY_SIZE),
        color: TEXT,
        ..TextFormat::default()
    }
}

/// `**bold**` and `` `code` `` within a line; a marker left open shows as
/// written.
fn inline(job: &mut LayoutJob, line: &str) {
    let mut rest = line;
    while !rest.is_empty() {
        let code = rest.find('`');
        let bold = rest.find("**");
        match (code, bold) {
            (Some(c), b) if b.is_none_or(|b| c < b) => {
                if let Some(end) = rest[c + 1..].find('`') {
                    job.append(&rest[..c], 0.0, plain());
                    job.append(
                        &rest[c + 1..c + 1 + end],
                        0.0,
                        TextFormat {
                            font_id: FontId::monospace(BODY_SIZE - 0.5),
                            color: LINK,
                            background: RAISED,
                            ..TextFormat::default()
                        },
                    );
                    rest = &rest[c + 2 + end..];
                    continue;
                }
            }
            (_, Some(b)) => {
                if let Some(end) = rest[b + 2..].find("**") {
                    job.append(&rest[..b], 0.0, plain());
                    job.append(
                        &rest[b + 2..b + 2 + end],
                        0.0,
                        TextFormat {
                            color: Color32::WHITE,
                            ..plain()
                        },
                    );
                    rest = &rest[b + 4 + end..];
                    continue;
                }
            }
            _ => {}
        }
        job.append(rest, 0.0, plain());
        break;
    }
}

/// A fenced block on its own card, its language above it and a copy button
/// beside; a diff tinted line by line.
fn code_block(ui: &mut Ui, language: &str, code: &str, effects: &mut Vec<Effect>) {
    Frame::new()
        .fill(RAISED)
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
        inline(&mut job, line);
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
    fn headings_need_hashes_and_a_space() {
        assert_eq!(heading("## Plan"), Some("Plan"));
        assert_eq!(heading("#hashtag"), None);
        assert_eq!(heading("plain"), None);
    }
}
