//! What opens over everything else: a held command, the pickers, the
//! shortcuts. Each button types the key the window says, so that the state
//! decides as it does for the keyboard.

use bevy_egui::egui::{
    self, Align2, Color32, Context, Frame, Margin, RichText, ScrollArea, Stroke, Ui,
};
use universe_agent::Question;
use universe_core::{Effort, TokenCount};
use universe_ui::input::{KeyCode, KeyEvent, KeyModifiers};
use universe_ui::keymap::SHORTCUTS;
use universe_ui::{App, ChatState, CompactRow, sessions};

use crate::theme::{ACCENT, CYAN, DIM, RAISED, RED, SELECTED, TEXT, YELLOW};

/// Draws whatever window the state has open. Keys its buttons type are
/// added to `keys`.
pub(crate) fn show(ctx: &Context, app: &App, keys: &mut Vec<KeyEvent>) {
    if app.picker().is_some() {
        resume(ctx, app);
    }
    if app.workspace_picker().is_some() {
        workspace(ctx, app, keys);
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
    if app.chats_open().is_some() {
        chats(ctx, app);
    }
    if app.tasks_open().is_some() {
        tasks(ctx, app);
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
    sized_window(ctx, title, colour, 520.0..=760.0, add);
}

/// A window as wide as `widths` allows.
fn sized_window(
    ctx: &Context,
    title: &str,
    colour: Color32,
    widths: std::ops::RangeInclusive<f32>,
    add: impl FnOnce(&mut Ui),
) {
    egui::Window::new(RichText::new(title).color(colour).strong())
        .id(egui::Id::new(("window", title)))
        .anchor(
            Align2::CENTER_TOP,
            egui::vec2(0.0, ctx.content_rect().height() / 6.0),
        )
        .collapsible(false)
        .resizable(false)
        .min_width(*widths.start())
        .max_width(*widths.end())
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
        Question::Command {
            secrets,
            hosts,
            programs,
            ..
        } => {
            let lasting: Vec<&str> = secrets
                .iter()
                .chain(hosts)
                .chain(programs)
                .map(String::as_str)
                .collect();
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
        Question::AddFolder { .. } => (
            YELLOW,
            "Add a folder?",
            vec![
                ('y', "Add it for this conversation".to_owned()),
                ('n', "Refuse".to_owned()),
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
                command,
                description,
                reasons,
                ..
            } => {
                ui.label(RichText::new(format!("{} wants to run:", approval.model)).color(DIM));
                if !description.is_empty() {
                    ui.label(RichText::new(format!("It says: {description}")).color(TEXT));
                }
                // A long command stays within the screen and scrolls, with
                // the wheel or ↑ ↓.
                let line = ui.text_style_height(&egui::TextStyle::Monospace);
                Frame::new()
                    .fill(RAISED)
                    .corner_radius(6)
                    .inner_margin(Margin::same(10))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        // ↑ ↓ move it when they were pressed; the wheel
                        // any time in between.
                        let keyed = egui::Id::new("held-command-keyed");
                        let wanted = app.approval_scroll();
                        let last = ui.data(|d| d.get_temp::<usize>(keyed));
                        let mut area = ScrollArea::vertical()
                            .id_salt("held-command")
                            .max_height(line * 14.0);
                        if last != Some(wanted) {
                            ui.data_mut(|d| d.insert_temp(keyed, wanted));
                            area = area.vertical_scroll_offset(wanted as f32 * line);
                        }
                        area.show(ui, |ui| {
                            ui.label(RichText::new(command).monospace().color(RED).strong());
                        });
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
            Question::AddFolder { path, folder } => {
                ui.label(format!(
                    "{} wants {}, outside the workspace. Add {} for the rest of the \
                     conversation?",
                    approval.model,
                    path.display(),
                    folder.display()
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

/// The folder a new conversation works in: those the settings list, the
/// current one, or a path typed.
fn workspace(ctx: &Context, app: &App, keys: &mut Vec<KeyEvent>) {
    let Some(picker) = app.workspace_picker() else {
        return;
    };
    window(ctx, "Where does this conversation work?", ACCENT, |ui| {
        for (i, folder) in picker.options.iter().enumerate() {
            let chosen = row(ui, i == picker.selected, |ui| {
                ui.label(RichText::new(folder.display().to_string()).color(TEXT));
                if folder == picker.here() {
                    ui.label(RichText::new("here").small().color(DIM));
                }
            });
            // A click chooses it: as many ↑ ↓ as it takes, then Enter.
            if chosen.interact(egui::Sense::click()).clicked() {
                let key = if i < picker.selected {
                    KeyCode::Up
                } else {
                    KeyCode::Down
                };
                for _ in 0..i.abs_diff(picker.selected) {
                    keys.push(key.into());
                }
                keys.push(KeyCode::Enter.into());
            }
        }
        ui.add_space(6.0);
        let typing = picker.selected == picker.options.len();
        row(ui, typing, |ui| {
            ui.label(RichText::new("Path:").color(DIM));
            ui.label(
                RichText::new(format!("{}{}", picker.typed, if typing { "▏" } else { "" }))
                    .monospace()
                    .color(TEXT),
            );
        });
        if let Some(error) = &picker.error {
            ui.label(RichText::new(error).color(RED));
        }
        hint(
            ui,
            "↑ ↓ to choose · type a path · Enter to go there · Esc to stay here",
        );
    });
}

/// The open conversations, for `/chats`: the one shown marked, those that
/// run or wait for the person said so.
fn chats(ctx: &Context, app: &App) {
    let Some(selected) = app.chats_open() else {
        return;
    };
    window(ctx, "Conversations", ACCENT, |ui| {
        ScrollArea::vertical().max_height(420.0).show(ui, |ui| {
            for (i, chat) in app.chat_rows().into_iter().enumerate() {
                let shown = row(ui, i == selected, |ui| {
                    ui.label(RichText::new(if chat.shown { "●" } else { " " }).color(ACCENT));
                    ui.label(RichText::new(&chat.name).color(TEXT));
                    let (state, colour) = match chat.state {
                        ChatState::Idle => ("", DIM),
                        ChatState::Running => ("working", CYAN),
                        ChatState::Waiting => ("waiting for you", YELLOW),
                    };
                    ui.label(
                        RichText::new(format!(
                            "{} request{}",
                            chat.requests,
                            if chat.requests == 1 { "" } else { "s" }
                        ))
                        .small()
                        .color(DIM),
                    );
                    if !state.is_empty() {
                        ui.label(RichText::new(state).small().color(colour));
                    }
                });
                if i == selected {
                    shown.scroll_to_me(None);
                }
            }
        });
        hint(
            ui,
            "↑ ↓ to move · Enter to show · n for a new one · x to close one not shown · Esc to close",
        );
    });
}

/// The project's tasks, for `/tasks`: done ones struck through, each with
/// the conversation it is tied to.
fn tasks(ctx: &Context, app: &App) {
    let Some(selected) = app.tasks_open() else {
        return;
    };
    window(ctx, "Tasks", ACCENT, |ui| {
        ScrollArea::vertical().max_height(420.0).show(ui, |ui| {
            for (i, task) in app.task_rows().into_iter().enumerate() {
                let shown = row(ui, i == selected, |ui| {
                    ui.label(RichText::new(if task.done { "[x]" } else { "[ ]" }).monospace());
                    let title = RichText::new(&task.title);
                    ui.label(if task.done {
                        title.strikethrough().color(DIM)
                    } else {
                        title.color(TEXT)
                    });
                    if let Some((name, open)) = &task.chat {
                        ui.label(
                            RichText::new(if *open {
                                format!("· {name}")
                            } else {
                                format!("· saved {name}")
                            })
                            .small()
                            .color(DIM),
                        );
                    }
                });
                if i == selected {
                    shown.scroll_to_me(None);
                }
            }
        });
        hint(
            ui,
            "Space to tick done · Enter to open its conversation · t to tie it here · d to delete · Esc to close",
        );
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
    // Most of the screen wide: each model's price, context and scores
    // show beside its name.
    let wide = (ctx.content_rect().width() * 0.85).max(520.0);
    sized_window(ctx, "Model", ACCENT, wide..=wide, |ui| {
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
                    let fresh = if model.fresh { "◆" } else { " " };
                    let name = RichText::new(format!(
                        "{mark} {team}{fresh} {}",
                        app.model_label(&model.model)
                    ));
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
            "type to search · Enter to choose · Space for the team · Ctrl-F for fresh work ◆ · ← → effort · Tab pair mode · Esc to close",
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
