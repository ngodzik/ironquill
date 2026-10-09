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

mod activity;
mod error;
mod keys;
mod plan;
mod theme;
mod transcript;
mod universe;
mod usage;
mod windows;

use std::sync::Arc;
use std::time::Duration;

use bevy::app::AppExit;
use bevy::camera::CameraOutputMode;
use bevy::input::ButtonState;
use bevy::input::keyboard::{KeyCode as PhysicalKey, KeyboardInput};
use bevy::prelude::*;
use bevy::render::render_resource::BlendState;
use bevy::window::{CompositeAlphaMode, PrimaryWindow};
use bevy::winit::{UpdateMode, WinitSettings};
use bevy_egui::egui::{
    self, Align, Color32, Frame, Id, LayerId, Layout, Margin, RichText, ScrollArea, Stroke, Ui,
    UiBuilder,
};
use bevy_egui::{
    EguiContexts, EguiGlobalSettings, EguiInput, EguiInputSet, EguiPlugin, EguiPreUpdateSet,
    EguiPrimaryContextPass, PrimaryEguiContext,
};
use ironquill_codemap::NodeKind;
use ironquill_core::{ChatModel, Delegate};
use ironquill_tools::Workspace;
use ironquill_ui::editor::EditorMode;
use ironquill_ui::input::{KeyCode, KeyEvent, KeyModifiers};
use ironquill_ui::keymap::{Focus, Mode, Pending};
use ironquill_ui::{App, Effect, Host, MapView, Settings, Start, Waiting, clipboard};
use tokio::runtime::Handle;

pub use error::GuiError;

use crate::keys::{Held, Pressed};
use crate::plan::Plan;
use crate::theme::{ACCENT, DIM, EDGE, PANEL, RAISED, SELECTED, TEXT, YELLOW};
use crate::universe::{Assets3d, Universe};
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
    let universe = Universe::new(app.root().to_owned());
    let plan = Plan::new(app.root().to_owned());
    let shell = Shell {
        plan,
        app,
        host,
        runtime,
        effects: Vec::new(),
        keys: Vec::new(),
        usage: UsageView::default(),
        scroll_seen: 0,
        wheel_rest: 0.0,
        transcript_rows: 0,
    };

    let exit = bevy::app::App::new()
        // Behind the panels, nothing: what shows through a see-through
        // window is the desktop, through the panels' own alpha.
        .insert_resource(ClearColor(Color::NONE))
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
                        // Made see-through from the start: a window cannot
                        // become so later. Opaque, its panels hide it all.
                        transparent: true,
                        composite_alpha_mode: CompositeAlphaMode::PreMultiplied,
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
        // egui draws with the panels' camera, named rather than guessed:
        // the universe has a camera of its own.
        .insert_resource(EguiGlobalSettings {
            auto_create_primary_context: false,
            ..default()
        })
        .insert_non_send(shell)
        .insert_resource(universe)
        .add_systems(Startup, (panels_camera, universe::setup))
        .add_systems(
            PreUpdate,
            quiet_modifiers
                .after(EguiInputSet::WriteEguiEvents)
                .before(EguiPreUpdateSet::BeginPass),
        )
        .add_systems(
            Update,
            (
                read_keys::<M, D>,
                drive::<M, D>,
                universe::show_or_hide,
                universe_activity::<M, D>,
                universe::animate,
            )
                .chain(),
        )
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
    /// The codebase's plan, drawn by egui alone.
    plan: Plan,
    /// The conversation's scroll as the state last had it, in lines: a key
    /// that scrolls changes it, and the view follows by the difference.
    scroll_seen: usize,
    /// How many lines the conversation scrolls through, as last drawn.
    transcript_rows: usize,
    /// What the wheel or the touchpad moved short of a whole line, kept for
    /// the next move: a touchpad sends many small ones.
    wheel_rest: f32,
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

/// The panels' camera: drawn over the universe's, blending into what it
/// drew, or over nothing while the universe is hidden.
fn panels_camera(mut commands: Commands) {
    commands.spawn((
        Camera2d,
        Camera {
            order: 1,
            clear_color: ClearColorConfig::Custom(Color::NONE),
            output_mode: CameraOutputMode::Write {
                blend_state: Some(BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                clear_color: ClearColorConfig::Custom(Color::NONE),
            },
            ..default()
        },
        PrimaryEguiContext,
    ));
}

/// Shows the panels over the universe while it is shown, over a cleared
/// window otherwise.
fn keep_panels_over(universe: &Universe, camera: &mut Mut<Camera>) {
    let CameraOutputMode::Write { clear_color, .. } = &camera.output_mode else {
        return;
    };
    // Checked before writing: a write would mark the camera changed.
    if matches!(clear_color, ClearColorConfig::None) == universe.shown {
        return;
    }
    if let CameraOutputMode::Write { clear_color, .. } = &mut camera.output_mode {
        *clear_color = if universe.shown {
            ClearColorConfig::None
        } else {
            ClearColorConfig::Custom(Color::NONE)
        };
    }
}

/// Lights the files the agent works on in the universe.
fn universe_activity<M, D>(
    shell: NonSend<Shell<M, D>>,
    mut universe: ResMut<Universe>,
    mut commands: Commands,
    assets: Option<Res<Assets3d>>,
) where
    M: ChatModel + 'static,
    D: Delegate + 'static,
{
    universe::light_up(
        &mut universe,
        shell.app.transcript(),
        &mut commands,
        assets.as_deref(),
    );
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
    mut universe: ResMut<Universe>,
    mut panels: Query<&mut Camera, With<Camera2d>>,
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
    let shown = shell.app.map_view() == Some(MapView::Universe);
    if universe.shown != shown {
        universe.shown = shown;
    }
    if let Ok(mut camera) = panels.single_mut() {
        keep_panels_over(&universe, &mut camera);
    }
    // Written only when it changes: a write wakes the loop, and a write
    // every frame would keep it from ever sleeping. The universe moves all
    // the time, and only it draws without rest.
    let focused = if shown {
        UpdateMode::Continuous
    } else {
        UpdateMode::reactive(wait)
    };
    if winit.focused_mode != focused {
        winit.focused_mode = focused;
        winit.unfocused_mode = if shown {
            UpdateMode::Continuous
        } else {
            UpdateMode::reactive_low_power(wait)
        };
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
    mut universe: ResMut<Universe>,
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
    // Ctrl-M fades the background in or out rather than switching it.
    let opacity = ctx.animate_value_with_time(Id::new("opacity"), shell.app.opacity(), 0.25);
    let panel = theme::see(PANEL, opacity);
    let background = theme::see(theme::BACKGROUND, opacity);
    let pane = Frame::new().fill(panel).inner_margin(Margin::same(14));

    egui::Panel::bottom("status")
        .frame(
            Frame::new()
                .fill(background)
                .inner_margin(Margin::symmetric(14, 4)),
        )
        .show_separator_line(false)
        .show(&mut root, |ui| status(ui, &shell.app));
    egui::Panel::bottom("input")
        .frame(
            Frame::new()
                .fill(panel)
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
                .frame(pane.fill(background).stroke(Stroke::new(1.0, EDGE)))
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

    let now = ctx.input(|i| i.time);
    let view = shell.app.map_view();
    shell
        .plan
        .showing(view == Some(MapView::Plan), now, shell.app.transcript());
    shell.plan.light_up(shell.app.transcript(), now);
    if let Some(view) = view {
        // The conversation beside the codebase, translucent so that what
        // is drawn behind shows through; the codebase takes the rest.
        // Held to a share of the window: its long lines would otherwise
        // widen it until the codebase had no room left.
        let chat = egui::Panel::right("chat-beside-map")
            .default_size(480.0)
            .max_size(ctx.content_rect().width() * 0.45)
            .frame(pane.fill(theme::see(PANEL, 0.62 * opacity)))
            .show(&mut root, |ui| conversation(ui, shell));
        let window = ctx.content_rect().width().max(1.0);
        universe.covered = (window - chat.response.rect.left()).max(0.0) / window;
        egui::CentralPanel::default()
            .frame(match view {
                MapView::Universe => Frame::NONE,
                MapView::Plan => Frame::NONE.fill(background),
            })
            .show(&mut root, |ui| match view {
                MapView::Universe => {
                    universe_view(ui, &mut universe, &mut shell.app);
                }
                MapView::Plan => plan::show(ui, &mut shell.plan, &mut shell.app),
            });
    } else if shell.app.tree().is_some() {
        egui::Panel::left("tree")
            .default_size(280.0)
            .frame(pane)
            .show(&mut root, |ui| {
                tree(ui, &mut shell.app, &mut shell.wheel_rest, &mut shell.keys);
            });
    }
    if view.is_some() {
    } else if shell.app.file().is_some() {
        egui::Panel::right("chat-beside-file")
            .default_size(520.0)
            .frame(pane)
            .show(&mut root, |ui| conversation(ui, shell));
        egui::CentralPanel::default()
            .frame(pane.fill(background))
            .show(&mut root, |ui| {
                file(ui, &mut shell.app, &mut shell.wheel_rest, &mut shell.keys);
            });
    } else {
        egui::CentralPanel::default()
            .frame(pane.fill(background))
            .show(&mut root, |ui| conversation(ui, shell));
    }

    windows::show(&ctx, &shell.app, &mut shell.keys);
    if !shell.effects.is_empty() || !shell.keys.is_empty() {
        ctx.request_repaint();
    }
    Ok(())
}

/// The universe's own controls: dragging turns it, the wheel comes closer,
/// a click centres a star, a double click opens its file. Names show for
/// the star under the pointer, the one chosen, and those the agent just
/// touched.
fn universe_view(ui: &mut Ui, universe: &mut Universe, app: &mut App) {
    let rect = ui.max_rect();
    let response = ui.allocate_rect(rect, egui::Sense::click_and_drag());
    if response.dragged() {
        let delta = response.drag_delta();
        universe.drag += Vec2::new(delta.x, delta.y);
    }
    if response.hovered() {
        universe.zoom += ui.input(|i| i.smooth_scroll_delta.y);
    }
    let pointer = response.hover_pos();
    universe.hovered = pointer.and_then(|at| {
        universe
            .on_screen
            .iter()
            .enumerate()
            .filter_map(|(node, p)| p.map(|p| (node, egui::pos2(p.x, p.y).distance(at))))
            .filter(|(_, d)| *d < 18.0)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(node, _)| node)
    });
    if response.double_clicked()
        && let Some(node) = universe.hovered
        && let Some(map) = universe.map()
        && matches!(map.nodes[node].kind, NodeKind::File { .. })
    {
        let path = universe.root().join(&map.nodes[node].path);
        app.open_path(path);
        app.show_map(None);
    } else if response.clicked() {
        // A click frames again: the star chosen, or the whole.
        universe.chosen = universe.hovered;
        universe.zoomed = false;
    }

    let painter = ui.painter_at(rect);
    let Some(map) = universe.map() else {
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            "Mapping the codebase…",
            egui::FontId::proportional(18.0),
            DIM,
        );
        return;
    };
    // The names worth reading: under the pointer, chosen, or just touched.
    for (node, at) in universe.on_screen.iter().enumerate() {
        let Some(at) = at else {
            continue;
        };
        let glow = universe.glow(node);
        let picked = universe.hovered == Some(node) || universe.chosen == Some(node);
        if !picked && glow < 1.0 {
            continue;
        }
        let entry = &map.nodes[node];
        let [r, g, b] = universe::srgb(entry.kind);
        let fade = if picked { 1.0 } else { (glow / 8.0).min(1.0) };
        let colour = Color32::from_rgb(r, g, b).gamma_multiply(0.4 + 0.6 * fade);
        let text = if picked {
            match entry.kind {
                NodeKind::File { lines, .. } => format!("{}  ·  {lines} lines", entry.path),
                NodeKind::Folder => format!(
                    "{}/",
                    if entry.path.is_empty() {
                        "."
                    } else {
                        &entry.path
                    }
                ),
            }
        } else {
            entry.name().to_owned()
        };
        let at = egui::pos2(at.x + 12.0, at.y - 10.0);
        let font = egui::FontId::proportional(if picked { 15.0 } else { 13.0 });
        painter.text(
            at + egui::vec2(1.0, 1.0),
            egui::Align2::LEFT_BOTTOM,
            &text,
            font.clone(),
            Color32::from_black_alpha(200),
        );
        painter.text(at, egui::Align2::LEFT_BOTTOM, &text, font, colour);
    }

    // What the universe is, in the corner, with the colours' meaning.
    let files = map
        .nodes
        .iter()
        .filter(|n| matches!(n.kind, NodeKind::File { .. }))
        .count();
    let imports = map
        .edges
        .iter()
        .filter(|e| e.kind == ironquill_codemap::EdgeKind::Imports)
        .count();
    let corner = rect.left_top() + egui::vec2(24.0, 22.0);
    painter.text(
        corner,
        egui::Align2::LEFT_TOP,
        "Universe",
        egui::FontId::proportional(26.0),
        TEXT,
    );
    let mut summary = format!("{} · {files} files · {imports} imports", app.project());
    if map.left_out > 0 {
        summary.push_str(&format!(" · {} more not shown", map.left_out));
    }
    painter.text(
        corner + egui::vec2(0.0, 34.0),
        egui::Align2::LEFT_TOP,
        summary,
        egui::FontId::proportional(13.0),
        DIM,
    );
    let legend = [
        ("Rust", ironquill_codemap::Language::Rust),
        ("Python", ironquill_codemap::Language::Python),
        ("TypeScript", ironquill_codemap::Language::TypeScript),
        ("JavaScript", ironquill_codemap::Language::JavaScript),
        ("Markdown", ironquill_codemap::Language::Markdown),
        ("Settings", ironquill_codemap::Language::Config),
    ];
    let mut at = corner + egui::vec2(0.0, 58.0);
    for (name, language) in legend {
        let [r, g, b] = universe::srgb(NodeKind::File { language, lines: 0 });
        painter.circle_filled(at + egui::vec2(5.0, 7.0), 4.0, Color32::from_rgb(r, g, b));
        painter.text(
            at + egui::vec2(16.0, 0.0),
            egui::Align2::LEFT_TOP,
            name,
            egui::FontId::proportional(12.0),
            DIM,
        );
        at.y += 18.0;
    }
    let [r, g, b] = universe::srgb(NodeKind::Folder);
    painter.circle_filled(at + egui::vec2(5.0, 7.0), 4.0, Color32::from_rgb(r, g, b));
    painter.text(
        at + egui::vec2(16.0, 0.0),
        egui::Align2::LEFT_TOP,
        "folders",
        egui::FontId::proportional(12.0),
        DIM,
    );
    painter.text(
        rect.left_bottom() + egui::vec2(24.0, -18.0),
        egui::Align2::LEFT_BOTTOM,
        "drag to turn · scroll to come closer · click a star to centre it · double-click to open it · Ctrl-N to leave",
        egui::FontId::proportional(12.0),
        DIM,
    );
    plan::switch(ui, rect, app);
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
            ui.set_max_width(ui.available_width().min(980.0));
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
fn tree(ui: &mut Ui, app: &mut App, rest: &mut f32, keys: &mut Vec<KeyEvent>) {
    let Some(tree) = app.tree() else {
        return;
    };
    let focused = app.focus() == Focus::Tree;
    ui.horizontal(|ui| {
        ui.label(
            RichText::new("Files")
                .strong()
                .color(if focused { ACCENT } else { DIM }),
        );
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui
                .small_button("hide")
                .on_hover_text("Ctrl-B shows or hides the files")
                .clicked()
            {
                keys.push(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL));
            }
        });
    });
    ui.add_space(4.0);
    let row = ui.text_style_height(&egui::TextStyle::Body) + 6.0;
    let visible = ((ui.available_height() / row) as usize).max(1);
    let area = ui.available_rect_before_wrap();
    let selected = tree.selected();
    let mut offset = tree.offset();
    if selected < offset {
        offset = selected;
    } else if selected >= offset + visible {
        offset = selected + 1 - visible;
    }
    tree.set_offset(offset);
    let mut clicked = None;
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
        let shown = Frame::new()
            .fill(if i == selected && focused {
                SELECTED
            } else {
                Color32::TRANSPARENT
            })
            .corner_radius(4)
            .inner_margin(Margin::symmetric(4, 1))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.add(egui::Label::new(text).selectable(false));
            });
        let response = shown.response.interact(egui::Sense::click());
        if response.hovered() {
            ui.painter()
                .rect_filled(response.rect, 4, Color32::from_white_alpha(6));
        }
        if response.clicked() {
            clicked = Some(i);
        }
    }
    // The wheel moves the choice, as the arrows do, so that what is chosen
    // stays in sight.
    let lines = wheel_lines(ui, area, rest, row);
    if lines != 0 {
        app.wheel(Focus::Tree, lines);
    }
    if let Some(index) = clicked {
        app.click_tree(index);
    }
}

/// The wheel's or the touchpad's move over `area` since last frame, in
/// whole rows of `row` points, down when positive; what is left over waits
/// in `rest`.
fn wheel_lines(ui: &Ui, area: egui::Rect, rest: &mut f32, row: f32) -> i32 {
    if !ui.rect_contains_pointer(area) {
        return 0;
    }
    let delta = ui.input(|i| i.smooth_scroll_delta.y);
    if delta == 0.0 {
        return 0;
    }
    *rest -= delta / row.max(1.0);
    let lines = rest.trunc();
    *rest -= lines;
    lines as i32
}

/// The open file, its lines numbered and coloured by its language, the
/// cursor's line lit; the editor's mode and message under it.
fn file(ui: &mut Ui, app: &mut App, rest: &mut f32, keys: &mut Vec<KeyEvent>) {
    let Some(editor) = app.file() else {
        return;
    };
    let focused = app.focus() == Focus::File;
    let mut close = false;
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(editor.path().display().to_string())
                .strong()
                .color(if focused { ACCENT } else { DIM }),
        );
        if editor.is_modified() {
            ui.label(RichText::new("modified").small().color(YELLOW));
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui
                .small_button("close")
                .on_hover_text("back to the conversation (, then c)")
                .clicked()
            {
                close = true;
            }
            if app.tree().is_none()
                && ui
                    .small_button("files")
                    .on_hover_text("the file tree (Ctrl-A)")
                    .clicked()
            {
                keys.push(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL));
            }
        });
    });
    ui.add_space(6.0);
    let row = ui.text_style_height(&egui::TextStyle::Monospace) + ui.spacing().item_spacing.y;
    let glyph =
        ui.fonts_mut(|f| f.glyph_width(&egui::TextStyle::Monospace.resolve(ui.style()), 'm'));
    let rows = ((ui.available_height() - 40.0) / row).max(1.0) as usize;
    let columns = ((ui.available_width() - 60.0) / glyph.max(1.0)).max(1.0) as usize;
    editor.set_viewport(rows, columns);
    let (cursor_row, cursor_column) = editor.cursor();
    let styled = editor.styled();
    let text_area = ui.available_rect_before_wrap();
    let first = editor.scroll();
    let mut clicked = None;
    // Long lines shift left together to keep the cursor in sight, as in
    // Vim with `nowrap`.
    let left = editor.left_offset();
    let font = egui::TextStyle::Monospace.resolve(ui.style());
    let insert = editor.mode() == EditorMode::Insert;
    for (i, line) in editor
        .lines()
        .iter()
        .enumerate()
        .skip(editor.scroll())
        .take(rows)
    {
        let current = i == cursor_row;
        // One piece of text per line, its runs coloured: a widget per run
        // made every key cost a frame of hundreds of them.
        let mut job = egui::text::LayoutJob::default();
        let format = |colour: Color32| egui::TextFormat::simple(font.clone(), colour);
        job.append(
            &format!("{:>5}  ", i + 1),
            0.0,
            format(if current { ACCENT } else { DIM }),
        );
        let mut skip = left;
        let mut runs = |text: &str, colour: Color32| {
            let count = text.chars().count();
            if skip >= count {
                skip -= count;
                return;
            }
            let from = text.char_indices().nth(skip).map_or(text.len(), |(b, _)| b);
            skip = 0;
            job.append(&text[from..], 0.0, format(colour));
        };
        match styled.as_deref().and_then(|s| s.get(i)) {
            Some(styled) => {
                for (colour, text) in styled {
                    runs(text, theme::rgb(*colour));
                }
            }
            None => runs(line, TEXT),
        }
        job.wrap.max_rows = 1;
        job.wrap.max_width = f32::INFINITY;
        let shown = Frame::new()
            .fill(if current {
                SELECTED
            } else {
                Color32::TRANSPARENT
            })
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.add(egui::Label::new(job).selectable(false).extend());
            });
        let rect = shown.response.rect;
        if current && focused {
            // The cursor: a bar while typing, a block otherwise, as in Vim.
            let x = rect.left() + (7 + cursor_column.saturating_sub(left)) as f32 * glyph;
            let caret = if insert {
                egui::Rect::from_min_size(
                    egui::pos2(x - 1.0, rect.top()),
                    egui::vec2(2.0, rect.height()),
                )
            } else {
                egui::Rect::from_min_size(
                    egui::pos2(x, rect.top()),
                    egui::vec2(glyph, rect.height()),
                )
            };
            ui.painter().rect_filled(
                caret,
                1,
                if insert {
                    ACCENT
                } else {
                    theme::see(ACCENT, 0.45)
                },
            );
        }
        let response = shown.response.interact(egui::Sense::click());
        if response.clicked()
            && let Some(at) = response.interact_pointer_pos()
        {
            // Past the line numbers, in characters on screen: the editor
            // adds the shift.
            let gutter = 7.0 * glyph;
            let column = ((at.x - rect.left() - gutter) / glyph.max(1.0)).max(0.0);
            clicked = Some((i - first, column as usize));
        }
    }
    // The colours are borrowed from the editor, which the clicks change.
    drop(styled);
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
    let lines = wheel_lines(ui, text_area, rest, row);
    if lines != 0 {
        app.wheel(Focus::File, lines);
    }
    if let Some((row, column)) = clicked {
        app.click_file(row, column);
    }
    if close {
        app.close_file(Focus::Chat);
    }
}
