//! The branch's tickets (Ctrl-L), on the right: each with its items and how
//! far each is, what was verified elsewhere, every changed file with the
//! items it serves; and on the left, the pieces of the diff an item or a
//! file is served by, with lines around them, more on a click at an edge.

use std::collections::HashMap;
use std::path::Path;

use bevy_egui::egui::{self, Color32, CornerRadius, Frame, Margin, RichText, Sense, Ui};
use universe_tools::{LineChanges, LineMark};
use universe_ui::tickets::{Progress, Tickets};
use universe_ui::{App, Effect, Shown};

use crate::theme::{CODE, CYAN, DIM, GREEN, LINK, RED, TEXT, YELLOW};

/// What the pane keeps between frames: each file as it is and as it was,
/// read once per check.
#[derive(Default)]
pub(crate) struct TicketsPane {
    files: HashMap<String, (Vec<String>, LineChanges)>,
    /// The check the files were read for.
    read_for: Option<usize>,
}

impl TicketsPane {
    /// `path` as it is now, and how it differs from where the branch left
    /// the main one.
    fn file(&mut self, root: &Path, path: &str) -> &(Vec<String>, LineChanges) {
        self.files.entry(path.to_owned()).or_insert_with(|| {
            let now: Vec<String> = std::fs::read_to_string(root.join(path))
                .map(|t| t.lines().map(str::to_owned).collect())
                .unwrap_or_default();
            let before = universe_tools::branch_base(root)
                .and_then(|base| universe_tools::lines_at(root, Path::new(path), &base))
                .unwrap_or_default();
            let changes = universe_tools::line_changes(&before, &now);
            (now, changes)
        })
    }
}

/// A share, as a percentage.
fn percent(share: f64) -> String {
    format!("{:.0}%", share * 100.0)
}

/// The tickets' pane.
pub(crate) fn show(ui: &mut Ui, pane: &mut TicketsPane, app: &mut App, effects: &mut Vec<Effect>) {
    ui.style_mut().interaction.selectable_labels = false;
    let tickets: Tickets = app.tickets().clone();
    let check = tickets.progress.as_ref().map(|p| p.hunks.len());
    if pane.read_for != check {
        pane.files.clear();
        pane.read_for = check;
    }
    ui.label(RichText::new("Tickets").size(20.0).color(TEXT));
    if let Some(busy) = app.tickets_busy() {
        ui.label(RichText::new(busy).size(12.5).color(CYAN));
        ui.ctx().request_repaint();
    }
    if tickets.tickets.is_empty() {
        ui.add_space(8.0);
        ui.label(
            RichText::new("None linked to this branch: /ticket <id or link>")
                .size(12.5)
                .color(DIM),
        );
        return;
    }
    let shown = app.ticket_view().map(|v| v.shown.clone());
    let progress = tickets.progress.as_ref();
    for (t, ticket) in tickets.tickets.iter().enumerate() {
        ui.add_space(10.0);
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new(&ticket.id).strong().color(LINK));
            ui.label(RichText::new(&ticket.title).color(TEXT));
        });
        ui.horizontal(|ui| {
            if ui
                .small_button("↻")
                .on_hover_text("Read it again")
                .clicked()
                && let Some(effect) = app.reread_ticket(t)
            {
                effects.push(effect);
            }
            if ui
                .small_button("⧉")
                .on_hover_text("Copy its items")
                .clicked()
                && let Some(text) = app.ticket_items_text(t)
            {
                effects.push(Effect::Copy(text));
            }
            if ui
                .small_button("→ message")
                .on_hover_text("Put its items in the message box")
                .clicked()
            {
                app.ticket_items_to_input(t);
            }
            if ui.small_button("✕").on_hover_text("Unlink it").clicked() {
                app.unlink_ticket(t);
            }
        });
        for (i, item) in ticket.items.iter().enumerate() {
            let judged = progress.and_then(|p| p.items.get(t)?.get(i)?.as_ref());
            let (mark, colour) = match judged {
                Some(p) if p.done => ("☑", GREEN),
                Some(_) => ("☐", YELLOW),
                None => ("☐", DIM),
            };
            let chosen = shown == Some(Shown::Item(t, i));
            let row = Frame::new()
                .fill(if chosen {
                    ui.visuals().selection.bg_fill
                } else {
                    Color32::TRANSPARENT
                })
                .corner_radius(CornerRadius::same(4))
                .inner_margin(Margin::symmetric(4, 2))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new(mark).color(colour));
                        ui.label(RichText::new(item).size(13.0).color(TEXT));
                        if let Some(p) = judged {
                            ui.label(RichText::new(percent(p.share)).size(11.5).color(DIM));
                        }
                    });
                })
                .response;
            let row = ui
                .interact(row.rect, ui.id().with(("item", t, i)), Sense::click())
                .on_hover_cursor(egui::CursorIcon::PointingHand);
            let row = match judged {
                Some(p) if !p.why.is_empty() => row.on_hover_text(&p.why),
                _ => row,
            };
            if row.clicked() {
                app.show_ticket_part(Shown::Item(t, i));
            }
        }
    }

    ui.add_space(12.0);
    ui.label(RichText::new("Verified elsewhere").size(14.0).color(TEXT));
    if tickets.notes.is_empty() {
        ui.label(
            RichText::new("nothing: /ticket note <what was verified>")
                .size(12.0)
                .color(DIM),
        );
    }
    let mut remove = None;
    for (n, note) in tickets.notes.iter().enumerate() {
        ui.horizontal_wrapped(|ui| {
            if ui.small_button("✕").clicked() {
                remove = Some(n);
            }
            ui.label(RichText::new(note).size(12.5).color(TEXT));
        });
    }
    if let Some(n) = remove {
        app.remove_note(n);
    }

    ui.add_space(12.0);
    if ui
        .add_enabled(
            app.tickets_busy().is_none(),
            egui::Button::new("Check progress"),
        )
        .on_hover_text("Judge the branch's diff against every item")
        .clicked()
        && let Some(effect) = app.check_progress()
    {
        effects.push(effect);
    }
    if let Some(progress) = progress {
        files(ui, progress, &tickets, shown.as_ref(), app);
    }
}

/// The shares, then every changed file with the items it serves and what in
/// it serves none, generated ones apart.
fn files(
    ui: &mut Ui,
    progress: &Progress,
    tickets: &Tickets,
    shown: Option<&Shown>,
    app: &mut App,
) {
    ui.add_space(6.0);
    let items: f64 = progress
        .items
        .iter()
        .flatten()
        .flatten()
        .map(|p| p.share)
        .sum();
    ui.label(
        RichText::new(format!(
            "{} serves the items · {} serves none · {} generated",
            percent(items),
            percent(progress.unattached),
            percent(progress.generated)
        ))
        .size(12.0)
        .color(DIM),
    );
    ui.add_space(8.0);
    ui.label(RichText::new("Files").size(14.0).color(TEXT));
    for generated in [false, true] {
        if generated && progress.files.iter().any(|f| f.generated) {
            ui.add_space(6.0);
            ui.label(RichText::new("Generated, never sent").size(12.0).color(DIM));
        }
        for file in progress.files.iter().filter(|f| f.generated == generated) {
            ui.horizontal_wrapped(|ui| {
                let mut looked = tickets.looked_at.contains(&file.path);
                if ui
                    .checkbox(&mut looked, "")
                    .on_hover_text("looked at, kept across checks")
                    .changed()
                {
                    app.toggle_looked_at(&file.path);
                }
                let chosen = shown == Some(&Shown::File(file.path.clone()));
                let name = RichText::new(&file.path).size(12.5).color(if generated {
                    DIM
                } else if chosen {
                    LINK
                } else {
                    TEXT
                });
                if ui
                    .add(egui::Label::new(name).sense(Sense::click()))
                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                    .clicked()
                {
                    app.show_ticket_part(Shown::File(file.path.clone()));
                }
                let served: Vec<String> = file
                    .items
                    .iter()
                    .map(|(t, i)| format!("{}.{}", t + 1, i + 1))
                    .collect();
                if !served.is_empty() {
                    ui.label(RichText::new(served.join(" ")).size(11.5).color(GREEN));
                }
                if file.unattached > 0 {
                    ui.label(
                        RichText::new(format!("{} lines serve none", file.unattached))
                            .size(11.5)
                            .color(YELLOW),
                    );
                }
            });
        }
    }
}

/// The pieces an item or a file is served by, on the left: each with lines
/// around it, more on a click at an edge, added and changed lines tinted
/// and removed ones shown where they were.
pub(crate) fn pieces(ui: &mut Ui, pane: &mut TicketsPane, app: &mut App) {
    let Some(view) = app.ticket_view().cloned() else {
        return;
    };
    let Some(progress) = app.tickets().progress.clone() else {
        return;
    };
    let tickets = app.tickets().clone();
    let (title, chosen): (String, Vec<usize>) = match &view.shown {
        Shown::Item(t, i) => (
            tickets
                .tickets
                .get(*t)
                .and_then(|ticket| ticket.items.get(*i))
                .cloned()
                .unwrap_or_default(),
            progress
                .items
                .get(*t)
                .and_then(|items| items.get(*i)?.as_ref())
                .map(|p| p.hunks.clone())
                .unwrap_or_default(),
        ),
        Shown::File(path) => (
            path.clone(),
            (0..progress.hunks.len())
                .filter(|h| progress.hunks[*h].path == *path)
                .collect(),
        ),
    };
    ui.horizontal(|ui| {
        ui.label(RichText::new(&title).size(16.0).color(TEXT));
        if ui.small_button("✕").clicked() {
            app.show_ticket_part(view.shown.clone());
        }
    });
    if chosen.is_empty() {
        ui.label(RichText::new("No piece of the diff serves it").color(DIM));
        return;
    }
    let root = app.root().to_owned();
    let mut widen = false;
    egui::ScrollArea::vertical().show(ui, |ui| {
        for h in chosen {
            let Some(piece) = progress.hunks.get(h) else {
                continue;
            };
            ui.add_space(8.0);
            ui.label(
                RichText::new(format!("{} line {}", piece.path, piece.start))
                    .size(12.0)
                    .color(LINK),
            );
            let (now, changes) = pane.file(&root, &piece.path).clone();
            let first = piece.start.saturating_sub(1).saturating_sub(view.context);
            let last = (piece.start.saturating_sub(1) + piece.lines + view.context).min(now.len());
            Frame::new()
                .fill(ui.visuals().faint_bg_color)
                .corner_radius(CornerRadius::same(6))
                .inner_margin(Margin::symmetric(8, 6))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    if first > 0 && ui.small_button("⋯ 10 more above").clicked() {
                        widen = true;
                    }
                    for (at, text) in now.iter().enumerate().take(last).skip(first) {
                        if let Some(gone) = changes.removed.get(&at) {
                            for line in gone {
                                ui.label(
                                    RichText::new(format!("- {line}"))
                                        .monospace()
                                        .color(RED)
                                        .strikethrough(),
                                );
                            }
                        }
                        let (sign, colour) = match changes.marks.get(at) {
                            Some(LineMark::Added) => ("+", GREEN),
                            Some(LineMark::Changed) => ("~", YELLOW),
                            _ => (" ", CODE),
                        };
                        ui.label(
                            RichText::new(format!("{sign} {:>4} {text}", at + 1))
                                .monospace()
                                .color(colour),
                        );
                    }
                    if last < now.len() && ui.small_button("⋯ 10 more below").clicked() {
                        widen = true;
                    }
                });
        }
    });
    if widen {
        app.widen_ticket_view();
    }
}
