//! The ironquill window: the interface's state drawn on the GPU.
//!
//! The state, the keys and what each does live in `ironquill-ui`, and the
//! effects are carried out by its `Host`, as in the terminal. This crate
//! only:
//! - reads the window's keys and hands them to the state (`keys`);
//! - drives the `Host` once a frame, without ever waiting on it;
//! - draws the state with egui on Bevy (`transcript`, `windows`, `usage`).
//!
//! Bevy owns the window and the frame loop, so that the codebase views to
//! come can draw in 2D and 3D under the same panels.

#![deny(unsafe_code)]
#![warn(missing_docs)]

mod error;
mod keys;
mod theme;
mod transcript;
mod usage;
mod windows;

use std::sync::Arc;
use std::time::Duration;

use bevy::app::AppExit;
use bevy::input::ButtonState;
use bevy::input::keyboard::{KeyCode as PhysicalKey, KeyboardInput};
use bevy::prelude::*;
use bevy::window::PrimaryWindow;
use bevy::winit::{UpdateMode, WinitSettings};
use bevy_egui::egui::{
    self, Align, Color32, Frame, Id, LayerId, Layout, Margin, RichText, ScrollArea, Stroke, Ui,
    UiBuilder,
};
use bevy_egui::{
    EguiContexts, EguiInput, EguiInputSet, EguiPlugin, EguiPreUpdateSet, EguiPrimaryContextPass,
};
use ironquill_core::{ChatModel, Delegate};
use ironquill_tools::Workspace;
use ironquill_ui::editor::EditorMode;
use ironquill_ui::input::KeyEvent;
use ironquill_ui::keymap::{Focus, Mode, Pending};
use ironquill_ui::{App, Effect, Host, Settings, Start, Waiting, clipboard};
use tokio::runtime::Handle;

pub use error::GuiError;

use crate::keys::{Held, Pressed};
use crate::theme::{ACCENT, DIM, EDGE, PANEL, RAISED, SELECTED, TEXT, YELLOW};
use crate::usage::UsageView;

/// How long the window may sleep while a request runs: the spinner turns
/// and the agent's messages show within it.
const BUSY_WAIT: Duration = Duration::from_millis(100);

/// How long it may sleep otherwise. Nothing changes then but a key, which
/// wakes it at once, and the timers that keep sessions warm.
const IDLE_WAIT: Duration = Duration::from_secs(1);

/// How long the text cursor stays lit, then dark, while typing: the window
/// wakes at this pace to blink it, and no faster.
const BLINK: Duration = Duration::from_millis(530);

/// The frames of the spinner, as in the terminal.
const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// The width of the usage pane, in points.
const USAGE_WIDTH: f32 = 460.0;

/// Opens the window and runs until the person quits or closes it.
///
/// `runtime` is the Tokio runtime the agent works on. The window must run
/// on the main thread, outside any task of that runtime: from inside one,
/// call it within `tokio::task::block_in_place`.
///
/// # Errors
///
/// [`GuiError::Exited`] when Bevy stops with an error.
pub fn run<M, D>(
    runtime: Handle,
    model: Arc<M>,
    delegate: Arc<D>,
    workspace: Workspace,
    settings: Settings,
    start: Start,
) -> Result<(), GuiError>
where
    M: ChatModel + 'static,
    D: Delegate + 'static,
{
    // The host's timers belong to the runtime.
    let _context = runtime.enter();
    let mut app = App::new(settings, workspace.root().to_owned());
    let mut host = Host::new(model, delegate, workspace);
    runtime.block_on(host.start(&mut app, start));
    let title = format!("{} · ironquill", app.name());
    let shell = Shell {
        app,
        host,
        runtime,
        effects: Vec::new(),
        keys: Vec::new(),
        usage: UsageView::default(),
        scroll_seen: 0,
        transcript_rows: 0,
    };

    let exit = bevy::app::App::new()
        .insert_resource(ClearColor(Color::srgb_u8(11, 13, 18)))
        .insert_resource(WinitSettings {
            focused_mode: UpdateMode::reactive(IDLE_WAIT),
            unfocused_mode: UpdateMode::reactive_low_power(IDLE_WAIT),
        })
        .add_plugins(
            DefaultPlugins
                .set(WindowPlugin {
                    primary_window: Some(Window {
                        title,
                        resolution: (1400, 900).into(),
                        ..default()
                    }),
                    ..default()
                })
                .set(bevy::log::LogPlugin {
                    level: bevy::log::Level::WARN,
                    ..default()
                }),
        )
        .add_plugins(EguiPlugin::default())
        .insert_non_send(shell)
        .add_systems(Startup, |mut commands: Commands| {
            commands.spawn(Camera2d);
        })
        .add_systems(
            PreUpdate,
            quiet_modifiers
                .after(EguiInputSet::WriteEguiEvents)
                .before(EguiPreUpdateSet::BeginPass),
        )
        .add_systems(Update, (read_keys::<M, D>, drive::<M, D>).chain())
        .add_systems(EguiPrimaryContextPass, draw::<M, D>)
        .run();
    match exit {
        AppExit::Success => Ok(()),
        AppExit::Error(code) => Err(GuiError::Exited(code.get())),
    }
}

/// The state, the host that carries out its effects, and what the window
/// remembers between frames. Not `Send`: the state is the main thread's.
struct Shell<M, D> {
    app: App,
    host: Host<M, D>,
    runtime: Handle,
    /// Effects asked for by a click, carried out on the next frame.
    effects: Vec<Effect>,
    /// Keys typed by a button, for the state on the next frame.
    keys: Vec<KeyEvent>,
    usage: UsageView,
    /// The conversation's scroll as the state last had it, in lines: a key
    /// that scrolls changes it, and the view follows by the difference.
    scroll_seen: usize,
    /// How many lines the conversation scrolls through, as last drawn.
    transcript_rows: usize,
}

impl<M, D> Shell<M, D>
where
    M: ChatModel + 'static,
    D: Delegate + 'static,
{
    /// Carries out `effect`, and what is due on every turn, on the runtime.
    /// Quick: what needs the conversation while a request runs waits.
    fn carry_out(&mut self, effect: Option<Effect>) {
        let Self {
            app, host, runtime, ..
        } = self;
        runtime.block_on(host.carry_out(app, effect));
    }

    fn key(&mut self, key: KeyEvent) {
        let effect = self.app.on_key(key);
        self.carry_out(effect);
    }

    /// Applies whatever the agent and the timers said since the last frame,
    /// without waiting for more.
    fn drain(&mut self) {
        loop {
            let waiting = Waiting::of(&self.app);
            let Self {
                app, host, runtime, ..
            } = self;
            let incoming = runtime.block_on(async {
                tokio::select! {
                    biased;
                    incoming = host.next(waiting) => Some(incoming),
                    () = std::future::ready(()) => None,
                }
            });
            let Some(incoming) = incoming else {
                return;
            };
            let effect = runtime.block_on(host.receive(app, incoming));
            self.carry_out(effect);
        }
    }
}

/// Drops the modifiers bevy_egui reports every frame when they did not
/// change. egui takes any event as a reason to draw again at once, so that
/// one, sent each frame, kept the window from ever sleeping: two cores busy
/// with nothing on screen moving.
fn quiet_modifiers(mut inputs: Query<&mut EguiInput>, mut last: Local<Option<egui::Modifiers>>) {
    for mut input in &mut inputs {
        input.events.retain(|event| match event {
            egui::Event::ModifiersChanged(now) if *last == Some(*now) => false,
            egui::Event::ModifiersChanged(now) => {
                *last = Some(*now);
                true
            }
            _ => true,
        });
    }
}

/// Hands each key pressed to the state, and pastes when asked.
///
/// The modifiers are followed through the events in their order rather
/// than read once a frame: Ctrl pressed and released within one frame, as
/// a quick Ctrl-O is, would otherwise be missed.
fn read_keys<M, D>(
    mut pressed: MessageReader<KeyboardInput>,
    mut held: Local<Held>,
    mut shell: NonSendMut<Shell<M, D>>,
) where
    M: ChatModel + 'static,
    D: Delegate + 'static,
{
    for event in pressed.read() {
        let down = event.state == ButtonState::Pressed;
        match event.key_code {
            PhysicalKey::ShiftLeft | PhysicalKey::ShiftRight => held.shift = down,
            PhysicalKey::ControlLeft | PhysicalKey::ControlRight => held.control = down,
            PhysicalKey::AltLeft => held.alt = down,
            _ => {}
        }
        if !down {
            continue;
        }
        match keys::translate(&event.logical_key, *held) {
            Some(Pressed::Key(key)) => shell.key(key),
            Some(Pressed::Paste) => match clipboard::paste() {
                Ok(text) => shell.app.on_paste(&text),
                Err(e) => shell.app.report_error(e),
            },
            None => {}
        }
    }
}

/// Once a frame: what the buttons asked for, what the agent said, what the
/// state queued; then the window's title, how long it may sleep, and
/// whether to quit.
fn drive<M, D>(
    mut shell: NonSendMut<Shell<M, D>>,
    mut exit: MessageWriter<AppExit>,
    mut winit: ResMut<WinitSettings>,
    mut window: Single<&mut Window, With<PrimaryWindow>>,
) where
    M: ChatModel + 'static,
    D: Delegate + 'static,
{
    for key in std::mem::take(&mut shell.keys) {
        shell.key(key);
    }
    for effect in std::mem::take(&mut shell.effects) {
        shell.carry_out(Some(effect));
    }
    shell.drain();
    while let Some(effect) = shell.app.take_queued() {
        shell.carry_out(Some(effect));
    }
    shell.carry_out(None);

    let title = format!("{} · ironquill", shell.app.name());
    if window.title != title {
        window.title = title;
    }
    let wait = if shell.app.is_running() || shell.app.docker().is_some() {
        BUSY_WAIT
    } else if shell.app.mode() != Mode::Normal {
        BLINK
    } else {
        IDLE_WAIT
    };
    // Written only when it changes: a write wakes the loop, and a write
    // every frame would keep it from ever sleeping.
    let focused = UpdateMode::reactive(wait);
    if winit.focused_mode != focused {
        winit.focused_mode = focused;
        winit.unfocused_mode = UpdateMode::reactive_low_power(wait);
    }
    if shell.app.should_quit() {
        exit.write(AppExit::Success);
    }
}

/// Draws the state. Changes nothing in it but what only the drawing knows:
/// how far the conversation and the panes scroll.
fn draw<M, D>(
    mut contexts: EguiContexts,
    mut shell: NonSendMut<Shell<M, D>>,
    mut styled: Local<bool>,
) -> Result
where
    M: ChatModel + 'static,
    D: Delegate + 'static,
{
    let ctx = contexts.ctx_mut()?.clone();
    if !*styled {
        ctx.set_theme(egui::Theme::Dark);
        theme::apply(&ctx);
        *styled = true;
    }
    let shell = &mut *shell;
    let mut root = Ui::new(
        ctx.clone(),
        Id::new("ironquill"),
        UiBuilder::new()
            .layer_id(LayerId::background())
            .max_rect(ctx.content_rect()),
    );
    let pane = Frame::new().fill(PANEL).inner_margin(Margin::same(14));

    egui::Panel::bottom("status")
        .frame(
            Frame::new()
                .fill(theme::BACKGROUND)
                .inner_margin(Margin::symmetric(14, 4)),
        )
        .show_separator_line(false)
        .show(&mut root, |ui| status(ui, &shell.app));
    egui::Panel::bottom("input")
        .frame(
            Frame::new()
                .fill(PANEL)
                .inner_margin(Margin::symmetric(14, 10)),
        )
        .show(&mut root, |ui| {
            activity(ui, &shell.app);
            input(ui, &shell.app);
        });

    match shell.app.usage_pane() {
        Some((log, window)) => {
            // Slides in from the right as it opens.
            let open = ctx.animate_bool_with_time(Id::new("usage-open"), true, 0.25);
            egui::Panel::right("usage")
                .resizable(false)
                .exact_size(USAGE_WIDTH * open)
                .frame(pane.fill(theme::BACKGROUND).stroke(Stroke::new(1.0, EDGE)))
                .show(&mut root, |ui| {
                    ScrollArea::vertical().show(ui, |ui| {
                        usage::show(ui, &mut shell.usage, log, window);
                    });
                });
        }
        None => {
            ctx.animate_bool_with_time(Id::new("usage-open"), false, 0.0);
            shell.usage.closed();
        }
    }

    if shell.app.tree().is_some() {
        egui::Panel::left("tree")
            .default_size(280.0)
            .frame(pane)
            .show(&mut root, |ui| tree(ui, &shell.app));
    }
    if shell.app.file().is_some() {
        egui::Panel::right("chat-beside-file")
            .default_size(520.0)
            .frame(pane)
            .show(&mut root, |ui| conversation(ui, shell));
        egui::CentralPanel::default()
            .frame(pane.fill(theme::BACKGROUND))
            .show(&mut root, |ui| file(ui, &shell.app));
    } else {
        egui::CentralPanel::default()
            .frame(pane.fill(theme::BACKGROUND))
            .show(&mut root, |ui| conversation(ui, shell));
    }

    windows::show(&ctx, &shell.app, &mut shell.keys);
    if !shell.effects.is_empty() || !shell.keys.is_empty() {
        ctx.request_repaint();
    }
    Ok(())
}

/// The conversation, following new output unless a key scrolled it back.
fn conversation<M, D>(ui: &mut Ui, shell: &mut Shell<M, D>) {
    let row = ui.text_style_height(&egui::TextStyle::Body) + 4.0;
    let back = shell.app.scroll_back();
    let delta = back as f32 - shell.scroll_seen as f32;
    shell.scroll_seen = back;
    let output = ScrollArea::vertical()
        .id_salt("transcript")
        .stick_to_bottom(true)
        .auto_shrink(false)
        .show(ui, |ui| {
            if delta != 0.0 {
                ui.scroll_with_delta(egui::vec2(0.0, delta * row));
            }
            ui.set_max_width(980.0);
            transcript::show(ui, &shell.app, &mut shell.effects);
        });
    shell.transcript_rows =
        ((output.content_size.y - output.inner_rect.height()).max(0.0) / row) as usize;
    shell.app.set_max_scroll(shell.transcript_rows);
}

/// What the current request is doing, while one runs.
fn activity(ui: &mut Ui, app: &App) {
    let Some(elapsed) = app.elapsed() else {
        if let Some(matches) = app.completions() {
            ui.label(RichText::new(matches.join("   ")).color(DIM));
        }
        return;
    };
    ui.horizontal(|ui| {
        let glyph = SPINNER[app.spinner() % SPINNER.len()];
        ui.label(RichText::new(format!("{glyph} Working…")).color(ACCENT));
        if let Some(step) = app.step() {
            ui.label(RichText::new(step.to_lowercase()).color(DIM));
        }
        if let Some(model) = app.working_model() {
            ui.label(RichText::new(model.to_string()).strong().color(TEXT));
        }
        let spent = app.request_spent();
        if spent.usage.input.0 > 0 {
            ui.label(RichText::new(spent.to_string()).color(DIM));
        }
        ui.label(
            RichText::new(format!("{}s · Ctrl-C to stop", elapsed.as_secs()))
                .small()
                .color(DIM),
        );
    });
    ui.add_space(4.0);
}

/// The message box, or the command line in command mode, with who answers
/// and the team above it.
fn input(ui: &mut Ui, app: &App) {
    let (editor, prompt) = match app.mode() {
        Mode::Command => (app.command_line(), ":"),
        Mode::Insert | Mode::Normal => (app.input(), "›"),
    };
    let focused = app.mode() != Mode::Normal;
    Frame::new()
        .fill(RAISED)
        .stroke(Stroke::new(
            1.0,
            if focused {
                ACCENT.gamma_multiply(0.7)
            } else {
                EDGE
            },
        ))
        .corner_radius(10)
        .inner_margin(Margin::symmetric(12, 10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                ui.label(RichText::new(format!("{prompt} ")).color(ACCENT).strong());
                // A pasted line break shows as ↵; the message keeps it.
                let text: Vec<char> = editor
                    .text()
                    .chars()
                    .map(|c| if c == '\n' { '↵' } else { c })
                    .collect();
                if text.is_empty() && app.mode() == Mode::Insert {
                    caret(ui);
                    ui.label(RichText::new("Ask, or describe a change").color(DIM));
                    return;
                }
                let cursor = editor.cursor().min(text.len());
                let before: String = text[..cursor].iter().collect();
                let after: String = text[cursor..].iter().collect();
                ui.label(RichText::new(before).color(TEXT));
                if focused {
                    caret(ui);
                }
                ui.label(RichText::new(after).color(TEXT));
            });
        });
    if let Some(model) = app.current_model() {
        ui.horizontal(|ui| {
            ui.label(RichText::new("answers").small().color(DIM));
            ui.label(RichText::new(model.to_string()).small().color(ACCENT));
            let team: Vec<String> = app
                .team()
                .iter()
                .filter(|m| *m != model)
                .map(ToString::to_string)
                .collect();
            ui.label(RichText::new("· team").small().color(DIM));
            ui.label(
                RichText::new(if team.is_empty() {
                    "none".to_owned()
                } else {
                    team.join(", ")
                })
                .small()
                .color(DIM),
            );
        });
    }
}

/// The text cursor: a bar that blinks at the pace the window wakes to while
/// typing. No repaint is asked for here: bevy_egui takes any request, even
/// one for later, as one for now, which would draw without rest.
fn caret(ui: &mut Ui) {
    let time = ui.input(|i| i.time);
    let on = ((time / BLINK.as_secs_f64()).floor() as u64).is_multiple_of(2);
    let height = ui.text_style_height(&egui::TextStyle::Body);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(2.0, height), egui::Sense::hover());
    if on {
        ui.painter().rect_filled(rect, 1.0, ACCENT);
    }
}

/// The mode, what a key waits for, the conversation's name; the model, the
/// effort, the budget and what was spent.
fn status(ui: &mut Ui, app: &App) {
    ui.horizontal(|ui| {
        let mode = match app.mode() {
            Mode::Normal => "NORMAL",
            Mode::Insert => "INSERT",
            Mode::Command => "COMMAND",
        };
        Frame::new()
            .fill(if app.mode() == Mode::Normal {
                RAISED
            } else {
                ACCENT.gamma_multiply(0.3)
            })
            .corner_radius(4)
            .inner_margin(Margin::symmetric(6, 1))
            .show(ui, |ui| {
                ui.label(RichText::new(mode).small().strong().color(TEXT));
            });
        match app.pending() {
            Some(Pending::Leader) => {
                ui.label(RichText::new(",").small().color(TEXT));
            }
            Some(Pending::Window) => {
                ui.label(RichText::new("^W").small().color(TEXT));
            }
            None => {}
        }
        match app.notice() {
            Some(notice) => ui.label(RichText::new(notice).small().color(YELLOW)),
            None => ui.label(RichText::new(app.session_label()).small().color(DIM)),
        };
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let (usage, cost, complete) = app.totals();
            let budget = app
                .budget()
                .map(|b| format!(" · budget {b}"))
                .unwrap_or_default();
            let context = app
                .context()
                .map(|c| format!(" · ctx {}%", c.percent()))
                .unwrap_or_default();
            ui.label(
                RichText::new(format!(
                    "effort {}{budget} · {} in · {} out · {cost}{}{context}",
                    app.effort(),
                    usage.input,
                    usage.output,
                    if complete { "" } else { "+?" },
                ))
                .small()
                .color(DIM),
            );
            if let Some(model) = app.current_model() {
                ui.label(RichText::new(model.to_string()).small().color(ACCENT));
            }
        });
    });
}

/// The file tree, the selected row kept in view.
fn tree(ui: &mut Ui, app: &App) {
    let Some(tree) = app.tree() else {
        return;
    };
    let focused = app.focus() == Focus::Tree;
    ui.label(
        RichText::new("Files")
            .strong()
            .color(if focused { ACCENT } else { DIM }),
    );
    ui.add_space(4.0);
    let row = ui.text_style_height(&egui::TextStyle::Body) + 6.0;
    let visible = ((ui.available_height() / row) as usize).max(1);
    let selected = tree.selected();
    let mut offset = tree.offset();
    if selected < offset {
        offset = selected;
    } else if selected >= offset + visible {
        offset = selected + 1 - visible;
    }
    tree.set_offset(offset);
    for (i, entry) in tree.rows().iter().enumerate().skip(offset).take(visible) {
        let icon = if entry.is_dir {
            if tree.is_expanded(&entry.path) {
                "▾ "
            } else {
                "▸ "
            }
        } else {
            "  "
        };
        let changed = app.is_changed(&entry.path, entry.is_dir);
        let text = RichText::new(format!("{}{icon}{}", "   ".repeat(entry.depth), entry.name))
            .color(if changed {
                ACCENT
            } else if entry.is_dir {
                TEXT
            } else {
                DIM
            });
        Frame::new()
            .fill(if i == selected && focused {
                SELECTED
            } else {
                Color32::TRANSPARENT
            })
            .corner_radius(4)
            .inner_margin(Margin::symmetric(4, 1))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(text);
            });
    }
}

/// The open file, its lines numbered and coloured by its language, the
/// cursor's line lit; the editor's mode and message under it.
fn file(ui: &mut Ui, app: &App) {
    let Some(editor) = app.file() else {
        return;
    };
    let focused = app.focus() == Focus::File;
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(editor.path().display().to_string())
                .strong()
                .color(if focused { ACCENT } else { DIM }),
        );
        if editor.is_modified() {
            ui.label(RichText::new("modified").small().color(YELLOW));
        }
    });
    ui.add_space(6.0);
    let row = ui.text_style_height(&egui::TextStyle::Monospace) + 2.0;
    let glyph =
        ui.fonts_mut(|f| f.glyph_width(&egui::TextStyle::Monospace.resolve(ui.style()), 'm'));
    let rows = ((ui.available_height() - 40.0) / row).max(1.0) as usize;
    let columns = ((ui.available_width() - 60.0) / glyph.max(1.0)).max(1.0) as usize;
    editor.set_viewport(rows, columns);
    let (cursor_row, cursor_column) = editor.cursor();
    let styled = editor.styled();
    for (i, line) in editor
        .lines()
        .iter()
        .enumerate()
        .skip(editor.scroll())
        .take(rows)
    {
        let current = i == cursor_row;
        Frame::new()
            .fill(if current {
                SELECTED
            } else {
                Color32::TRANSPARENT
            })
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    ui.label(
                        RichText::new(format!("{:>5}  ", i + 1))
                            .monospace()
                            .color(if current { ACCENT } else { DIM }),
                    );
                    match styled.and_then(|s| s.get(i)) {
                        Some(runs) => {
                            for (colour, text) in runs {
                                ui.label(
                                    RichText::new(text).monospace().color(theme::rgb(*colour)),
                                );
                            }
                        }
                        None => {
                            ui.label(RichText::new(line).monospace().color(TEXT));
                        }
                    }
                    if current && focused && line.chars().count() <= cursor_column {
                        ui.label(RichText::new("▏").monospace().color(ACCENT));
                    }
                });
            });
    }
    ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
        let mode = match editor.mode() {
            EditorMode::Normal => "NORMAL".to_owned(),
            EditorMode::Insert => "INSERT".to_owned(),
            EditorMode::Visual { line: true } => "VISUAL LINE".to_owned(),
            EditorMode::Visual { line: false } => "VISUAL".to_owned(),
            EditorMode::Command => format!(":{}", editor.prompt()),
            EditorMode::Search => format!("/{}", editor.prompt()),
        };
        ui.horizontal(|ui| {
            ui.label(RichText::new(mode).small().color(DIM));
            if let Some((message, error)) = editor.message() {
                ui.label(RichText::new(message).small().color(if error {
                    theme::RED
                } else {
                    DIM
                }));
            }
        });
    });
}
