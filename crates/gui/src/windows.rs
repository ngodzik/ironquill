//! What opens over everything else: a held command, the pickers, the
//! shortcuts. Each button types the key the window says, so that the state
//! decides as it does for the keyboard.

use bevy_egui::egui::{
    self, Align2, Color32, Context, Frame, Margin, RichText, ScrollArea, Stroke, Ui,
};
use ironquill_agent::Question;
use ironquill_core::{Effort, TokenCount};
use ironquill_ui::input::{KeyCode, KeyEvent, KeyModifiers};
use ironquill_ui::keymap::SHORTCUTS;
use ironquill_ui::{App, CompactRow, sessions};

use crate::theme::{ACCENT, CYAN, DIM, RAISED, RED, SELECTED, TEXT, YELLOW};

/// Draws whatever window the state has open. Keys its buttons type are
/// added to `keys`.
pub(crate) fn show(ctx: &Context, app: &App, keys: &mut Vec<KeyEvent>) {
    if app.picker().is_some() {
        resume(ctx, app);
    }
    if app.model_picker().is_some() {
        model_picker(ctx, app);
    }
    if app.keys_open().is_some() {
        shortcuts(ctx);
    }
    if app.compact_picker().is_some() {
        compact(ctx, app);
    }
    if app.definition_choice().is_some() {
        definitions(ctx, app, keys);
    }
    if app.approval().is_some() {
        approval(ctx, app, keys);
    }
}

/// A window centred a third of the way down, its edge in `colour`.
fn window(ctx: &Context, title: &str, colour: Color32, add: impl FnOnce(&mut Ui)) {
    egui::Window::new(RichText::new(title).color(colour).strong())
        .id(egui::Id::new(("window", title)))
        .anchor(
            Align2::CENTER_TOP,
            egui::vec2(0.0, ctx.content_rect().height() / 6.0),
        )
        .collapsible(false)
        .resizable(false)
        .min_width(520.0)
        .max_width(760.0)
        .frame(
            Frame::window(&ctx.global_style())
                .stroke(Stroke::new(1.5, colour))
                .inner_margin(Margin::same(18)),
        )
        .show(ctx, add);
}

/// The keys a window takes, under it.
fn hint(ui: &mut Ui, text: &str) {
    ui.add_space(8.0);
    ui.label(RichText::new(text).small().color(DIM));
}

/// A held command or a question that only costs money: what is asked, and
/// a button for each answer.
fn approval(ctx: &Context, app: &App, keys: &mut Vec<KeyEvent>) {
    let Some(approval) = app.approval() else {
        return;
    };
    // What cannot be undone in red; going on costs only money, in cyan.
    let (colour, title, answers): (Color32, &str, Vec<(char, String)>) = match &approval.question {
        Question::Command { secrets, hosts, .. } => {
            let lasting: Vec<&str> = secrets.iter().chain(hosts).map(String::as_str).collect();
            let mut answers = vec![('y', "Run it".to_owned())];
            if !lasting.is_empty() {
                answers.push(('a', format!("Always allow {}", lasting.join(", "))));
            }
            answers.push(('n', "Refuse".to_owned()));
            answers.push(('c', "Copy".to_owned()));
            (RED, "Run this command?", answers)
        }
        Question::ColdStart { .. } => (
            CYAN,
            "The cache has expired",
            vec![
                ('y', "New session from the summary".to_owned()),
                ('n', "Go on as it is".to_owned()),
                ('c', "Stop, to compact".to_owned()),
            ],
        ),
        Question::KeepWarm { .. } => (
            CYAN,
            "Keep the sessions warm?",
            vec![
                ('y', "Another half hour".to_owned()),
                ('n', "Stop".to_owned()),
            ],
        ),
        Question::MoreTurns { .. } => (
            CYAN,
            "Go on?",
            vec![('y', "Go on".to_owned()), ('n', "Answer now".to_owned())],
        ),
    };
    window(ctx, title, colour, |ui| {
        match &approval.question {
            Question::Command {
                command, reasons, ..
            } => {
                ui.label(RichText::new(format!("{} wants to run:", approval.model)).color(DIM));
                Frame::new()
                    .fill(RAISED)
                    .corner_radius(6)
                    .inner_margin(Margin::same(10))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.label(RichText::new(command).monospace().color(RED).strong());
                    });
                for reason in reasons {
                    ui.label(RichText::new(format!("· {reason}")).color(DIM));
                }
            }
            Question::ColdStart {
                idle_minutes,
                tokens,
            } => {
                ui.label(format!(
                    "{}'s conversation was left {idle_minutes} minutes: its cache has expired. \
                     Going on as it is writes it all to the cache again{}, at the dearest rate; \
                     a new session starts from its summary and the latest exchanges.",
                    approval.model,
                    tokens.map_or_else(String::new, |t| format!(", about {}", TokenCount(t)))
                ));
            }
            Question::KeepWarm { minutes } => {
                ui.label(format!(
                    "No request for {minutes} minutes. Each read of the warm sessions costs a \
                     little; letting them cool, the next request writes them to the cache again."
                ));
            }
            Question::MoreTurns { turns } => {
                ui.label(format!(
                    "{} used its {turns} turns. Go on for {turns} more, or have it answer now \
                     with what it found?",
                    approval.model
                ));
            }
        }
        ui.add_space(10.0);
        ui.horizontal_wrapped(|ui| {
            for (key, label) in answers {
                let text = RichText::new(format!("{label}  {key}")).color(if key == 'y' {
                    colour
                } else {
                    TEXT
                });
                if ui.button(text).clicked() {
                    keys.push(KeyCode::Char(key).into());
                }
            }
        });
    });
}

/// The saved conversations, for `/resume`.
fn resume(ctx: &Context, app: &App) {
    let Some(picker) = app.picker() else {
        return;
    };
    let now = sessions::now();
    window(ctx, "Resume a conversation", ACCENT, |ui| {
        ScrollArea::vertical().max_height(420.0).show(ui, |ui| {
            for (i, item) in picker.items.iter().enumerate() {
                let row = row(ui, i == picker.selected, |ui| {
                    ui.label(RichText::new(&item.name).color(TEXT));
                    ui.label(
                        RichText::new(format!(
                            "{} · {} request{} · {} · {}",
                            sessions::ago(item.updated, now),
                            item.requests,
                            if item.requests == 1 { "" } else { "s" },
                            item.cost,
                            item.id
                        ))
                        .small()
                        .color(DIM),
                    );
                });
                if i == picker.selected {
                    row.scroll_to_me(None);
                }
            }
        });
        hint(ui, "↑ ↓ to move · Enter to choose · Esc to close");
    });
}

/// The places a name is defined (gd, Ctrl-click), to choose one: a click
/// goes there, as the arrows then Enter do.
fn definitions(ctx: &Context, app: &App, keys: &mut Vec<KeyEvent>) {
    let Some(choice) = app.definition_choice() else {
        return;
    };
    let title = format!(
        "Where {} is {} · {} {}",
        choice.name,
        if choice.uses { "used" } else { "defined" },
        choice.items.len(),
        if choice.uses { "uses" } else { "places" }
    );
    window(ctx, &title, ACCENT, |ui| {
        ScrollArea::vertical().max_height(420.0).show(ui, |ui| {
            for (i, item) in choice.items.iter().enumerate() {
                let shown = row(ui, i == choice.selected, |ui| {
                    ui.vertical(|ui| {
                        ui.label(
                            RichText::new(format!("{}:{}", item.path, item.line))
                                .small()
                                .color(DIM),
                        );
                        ui.label(RichText::new(&item.text).monospace().color(TEXT));
                    });
                });
                if i == choice.selected {
                    shown.scroll_to_me(None);
                }
                if shown.interact(egui::Sense::click()).clicked() {
                    // The state moves by keys: down or up to it, then Enter.
                    let (code, steps) = if i >= choice.selected {
                        (KeyCode::Down, i - choice.selected)
                    } else {
                        (KeyCode::Up, choice.selected - i)
                    };
                    for _ in 0..steps {
                        keys.push(KeyEvent::new(code, KeyModifiers::NONE));
                    }
                    keys.push(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
                }
            }
        });
        hint(
            ui,
            &format!(
                "↑ ↓ to move · Enter or a click to go · Esc to close · found by {}",
                choice.by
            ),
        );
    });
}

/// A row of a list, highlighted when selected.
fn row(ui: &mut Ui, selected: bool, add: impl FnOnce(&mut Ui)) -> egui::Response {
    Frame::new()
        .fill(if selected {
            SELECTED
        } else {
            Color32::TRANSPARENT
        })
        .corner_radius(6)
        .inner_margin(Margin::symmetric(8, 4))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(add);
        })
        .response
}

/// The model picker (Ctrl-E): the search, then the models, the current
/// one marked ●, the team ✓.
fn model_picker(ctx: &Context, app: &App) {
    let Some(picker) = app.model_picker() else {
        return;
    };
    let rows = app.model_rows();
    let current = app.current_model();
    window(ctx, "Model", ACCENT, |ui| {
        ui.horizontal(|ui| {
            ui.label(RichText::new("search").color(DIM));
            ui.label(RichText::new(format!("{}▏", picker.filter)).color(TEXT));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let effort = RichText::new(format!("effort ← {} →", app.effort()));
                ui.label(if app.effort() > Effort::High {
                    effort.color(YELLOW).strong()
                } else {
                    effort.color(DIM)
                });
            });
        });
        let credits = app
            .credits()
            .map(|c| format!("   scores: {c}"))
            .unwrap_or_default();
        ui.label(
            RichText::new(format!(
                "● answers you   ✓ in its team: it may hand them tasks{credits}"
            ))
            .small()
            .color(DIM),
        );
        ScrollArea::vertical().max_height(420.0).show(ui, |ui| {
            for (i, model) in rows.iter().enumerate() {
                let selected = i == picker.selected;
                let response = row(ui, selected, |ui| {
                    let mark = if Some(&model.model) == current {
                        "●"
                    } else {
                        " "
                    };
                    let team = if model.in_team { "✓" } else { " " };
                    let name = RichText::new(format!("{mark} {team} {}", model.model));
                    ui.label(if model.offered {
                        name.color(TEXT)
                    } else {
                        name.color(DIM)
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(RichText::new(&model.note).small().color(DIM));
                    });
                });
                if selected {
                    response.scroll_to_me(None);
                }
            }
            if rows.is_empty() {
                ui.label(RichText::new("no model matches").color(DIM));
            }
        });
        hint(
            ui,
            "type to search · Enter to choose · Space for the team · ← → effort · Tab pair mode · Esc to close",
        );
    });
}

/// Every shortcut (Ctrl-S), grouped by where it works.
fn shortcuts(ctx: &Context) {
    window(ctx, "Shortcuts", ACCENT, |ui| {
        ScrollArea::vertical().max_height(520.0).show(ui, |ui| {
            for (group, keys) in SHORTCUTS {
                ui.add_space(6.0);
                ui.label(RichText::new(*group).color(ACCENT).strong());
                egui::Grid::new(group).num_columns(2).show(ui, |ui| {
                    for (keys, what) in *keys {
                        ui.label(RichText::new(*keys).monospace().color(DIM));
                        ui.label(*what);
                        ui.end_row();
                    }
                });
            }
        });
        hint(ui, "Esc to close");
    });
}

/// The subjects of the conversation, for `/compact` to keep or drop: each
/// with what it adds to the context and how old it is, the list scrolling
/// to the row the cursor is on.
fn compact(ctx: &Context, app: &App) {
    let Some(picker) = app.compact_picker() else {
        return;
    };
    let now = sessions::now();
    window(ctx, "Compact", ACCENT, |ui| {
        ui.label(RichText::new(picker.header()).color(DIM));
        ui.add_space(6.0);
        ScrollArea::vertical().max_height(420.0).show(ui, |ui| {
            for (i, kind) in picker.rows().into_iter().enumerate() {
                let text = match kind {
                    CompactRow::Subject(s) => picker.subject_text(s, now),
                    CompactRow::Exchange(e) => format!("      {}", picker.exchange_text(e, now)),
                };
                let selected = i == picker.cursor;
                let shown = row(ui, selected, |ui| {
                    ui.label(RichText::new(text).monospace());
                });
                if selected {
                    shown.scroll_to_me(None);
                }
            }
        });
        ui.add_space(6.0);
        ui.label(
            RichText::new(format!(
                "{} The last exchange stays as it was (l)",
                if picker.last_as_is { "[x]" } else { "[ ]" }
            ))
            .color(DIM),
        );
        hint(
            ui,
            "Space to tick · → to open · ← to close · Enter to compact · d to drop the unticked · Esc to cancel",
        );
    });
}
