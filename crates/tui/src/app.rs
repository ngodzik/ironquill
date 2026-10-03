use std::cell::{Cell, OnceCell};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use ironquill_agent::{AgentConfig, Event, Outcome, Session, Verdict};
use ironquill_core::{ModelId, Usage, Usd};
use ironquill_tools::{Check, Container, ToolSummary};
use ratatui::crossterm::event::{
    KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::{Position, Rect};

use crate::command::{self, Command};
use crate::editor::{Editor, Outcome as EditorOutcome};
use crate::highlight::Highlighter;
use crate::keymap::{self, Action, Focus, Mode, Pending};
use crate::sessions::{self, Saved, Summary};
use crate::tree::FileTree;

/// Lines moved by one turn of the mouse wheel.
const WHEEL_LINES: i32 = 3;

/// Where each pane was drawn, written by the view so that a mouse click can be
/// matched to a pane. Areas include the pane's border.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Panes {
    pub(crate) tree: Option<Rect>,
    pub(crate) file: Option<Rect>,
    pub(crate) chat: Rect,
    pub(crate) docker: Option<Rect>,
}

/// The Docker pane: what `docker ps` said last.
#[derive(Debug, Default)]
pub(crate) struct DockerPane {
    pub(crate) containers: Vec<Container>,
    pub(crate) error: Option<String>,
    /// False until the first answer arrives.
    pub(crate) loaded: bool,
    pub(crate) selected: usize,
}

/// Lines moved by a half page scroll. Fixed rather than measured, so that the
/// state does not depend on the size of the terminal.
const HALF_PAGE: usize = 10;

/// What the agent is set up to do. Every field can be changed from inside the
/// interface with a command.
#[derive(Debug, Clone, Default)]
pub struct Settings {
    /// Models from cheapest to strongest. The first is tried first; empty
    /// until one is chosen.
    pub tiers: Vec<ModelId>,
    /// Commands that must succeed for a change to be kept.
    pub checks: Vec<Check>,
    /// Tries per model before the next takes over.
    pub rounds: u32,
    /// Turns per try.
    pub max_turns: u32,
    /// The models offered by the model picker (Ctrl-E). Identifiers starting
    /// with `claude-code` hand the task to Claude Code.
    pub models: Vec<ModelId>,
}

/// Something only the event loop can do, asked for by the state.
#[derive(Debug)]
pub(crate) enum Effect {
    /// Send this message to the conversation.
    Send { text: String, config: AgentConfig },
    /// Stop the request in progress.
    Cancel,
    /// Start a new conversation.
    Reset,
    /// Show what changed since the last commit.
    Diff,
    /// Write the conversation to disk.
    Save,
    /// List saved conversations, for the person to pick one.
    ListSessions,
    /// Load a saved conversation.
    Resume(String),
    /// Ask docker for its running containers now.
    RefreshDocker,
}

/// What the agent task sends back to the interface.
#[derive(Debug)]
pub(crate) enum AgentMessage {
    Event(Event),
    Done(Result<Outcome, String>),
}

/// One item in the transcript. Saved with the conversation, so that a
/// resumed one shows what it showed.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) enum Entry {
    Welcome,
    Info(String),
    Error(String),
    User(String),
    Said(String),
    Tool {
        name: String,
        path: Option<String>,
        outcome: Result<ToolSummary, String>,
    },
    Checks(Vec<String>),
    Passed,
    Failed {
        command: String,
        excerpt: String,
    },
    Escalating {
        from: ModelId,
        to: ModelId,
    },
    GaveUp,
    /// What one request cost, shown under it.
    Cost {
        usage: Usage,
        cost: Usd,
        complete: bool,
        seconds: u64,
        /// Ran on a subscription (Claude Code): no cost is owed for it.
        #[serde(default)]
        subscription: bool,
    },
}

/// A single line of text being edited, with a cursor counted in characters.
#[derive(Debug, Default)]
pub(crate) struct LineEditor {
    text: String,
    cursor: usize,
}

impl LineEditor {
    pub(crate) fn text(&self) -> &str {
        &self.text
    }

    pub(crate) fn cursor(&self) -> usize {
        self.cursor
    }

    fn byte(&self, chars: usize) -> usize {
        self.text
            .char_indices()
            .nth(chars)
            .map_or(self.text.len(), |(i, _)| i)
    }

    fn insert(&mut self, c: char) {
        let at = self.byte(self.cursor);
        self.text.insert(at, c);
        self.cursor += 1;
    }

    fn backspace(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
            let at = self.byte(self.cursor);
            self.text.remove(at);
        }
    }

    fn delete_word(&mut self) {
        let before: Vec<char> = self.text.chars().take(self.cursor).collect();
        let mut start = before.len();
        while start > 0 && before[start - 1] == ' ' {
            start -= 1;
        }
        while start > 0 && before[start - 1] != ' ' {
            start -= 1;
        }
        let (from, to) = (self.byte(start), self.byte(self.cursor));
        self.text.replace_range(from..to, "");
        self.cursor = start;
    }

    fn take(&mut self) -> String {
        self.cursor = 0;
        std::mem::take(&mut self.text)
    }

    fn apply(&mut self, action: &Action) {
        let len = self.text.chars().count();
        match action {
            Action::Insert(c) => self.insert(*c),
            Action::Backspace => self.backspace(),
            Action::DeleteWord => self.delete_word(),
            Action::ClearLine => {
                self.take();
            }
            Action::Left => self.cursor = self.cursor.saturating_sub(1),
            Action::Right => self.cursor = (self.cursor + 1).min(len),
            Action::Home => self.cursor = 0,
            Action::End => self.cursor = len,
            _ => {}
        }
    }
}

/// The whole state of the interface.
pub(crate) struct App {
    settings: Settings,
    root: PathBuf,
    project: String,
    mode: Mode,
    focus: Focus,
    pending: Option<Pending>,
    input: LineEditor,
    command: LineEditor,
    transcript: Vec<Entry>,
    /// Lines scrolled up from the bottom; 0 follows new output.
    scroll_back: usize,
    /// The largest useful `scroll_back`, written by the view, which is the
    /// only part that knows how many lines the transcript wraps to.
    max_scroll: Cell<usize>,
    tree: Option<FileTree>,
    file: Option<Editor>,
    /// Files the agent changed during this conversation, relative to the root.
    changed: BTreeSet<String>,
    panes: Cell<Panes>,
    /// Loaded on the first file opened: the grammars take a moment, and a
    /// session that never opens a file should not pay for them.
    highlighter: OnceCell<Rc<Highlighter>>,
    running_since: Option<Instant>,
    spinner: usize,
    usage: Usage,
    cost: Usd,
    cost_complete: bool,
    quit: bool,
    /// The first Ctrl-C was pressed: a second one quits.
    quit_armed: bool,
    /// A one-line note in the status line, cleared by the next key.
    notice: Option<String>,
    session_id: String,
    session_name: Option<String>,
    created: u64,
    requests: usize,
    picker: Option<Picker>,
    /// The model picker, open on the row selected.
    model_picker: Option<usize>,
    /// The model working on the current request, for the activity line.
    working_model: Option<ModelId>,
    docker: Option<DockerPane>,
}

/// The list of saved conversations shown by `/resume`.
#[derive(Debug)]
pub(crate) struct Picker {
    pub(crate) items: Vec<Summary>,
    pub(crate) selected: usize,
}

impl App {
    pub(crate) fn new(settings: Settings, root: PathBuf) -> Self {
        let mut transcript = vec![Entry::Welcome];
        if settings.tiers.is_empty() {
            transcript.push(Entry::Error(
                "No model set. Choose one with /model <id>".into(),
            ));
        }
        // The folder name says which project this is; the full path only
        // crowds the screen.
        let project = root.file_name().map_or_else(
            || root.display().to_string(),
            |name| name.to_string_lossy().into_owned(),
        );
        Self {
            settings,
            root,
            project,
            mode: Mode::Insert,
            focus: Focus::Chat,
            pending: None,
            input: LineEditor::default(),
            command: LineEditor::default(),
            transcript,
            scroll_back: 0,
            max_scroll: Cell::new(0),
            tree: None,
            file: None,
            changed: BTreeSet::new(),
            panes: Cell::new(Panes::default()),
            highlighter: OnceCell::new(),
            running_since: None,
            spinner: 0,
            usage: Usage::default(),
            cost: Usd::default(),
            cost_complete: true,
            quit: false,
            quit_armed: false,
            notice: None,
            session_id: sessions::new_id(),
            session_name: None,
            created: sessions::now(),
            requests: 0,
            picker: None,
            model_picker: None,
            working_model: None,
            docker: None,
        }
    }

    // Read access for the view.

    pub(crate) fn mode(&self) -> Mode {
        self.mode
    }

    pub(crate) fn input(&self) -> &LineEditor {
        &self.input
    }

    pub(crate) fn command_line(&self) -> &LineEditor {
        &self.command
    }

    pub(crate) fn transcript(&self) -> &[Entry] {
        &self.transcript
    }

    pub(crate) fn scroll_back(&self) -> usize {
        self.scroll_back
    }

    pub(crate) fn set_max_scroll(&self, max: usize) {
        self.max_scroll.set(max);
    }

    pub(crate) fn project(&self) -> &str {
        &self.project
    }

    pub(crate) fn focus(&self) -> Focus {
        self.focus
    }

    pub(crate) fn pending(&self) -> Option<Pending> {
        self.pending
    }

    pub(crate) fn tree(&self) -> Option<&FileTree> {
        self.tree.as_ref()
    }

    pub(crate) fn file(&self) -> Option<&Editor> {
        self.file.as_ref()
    }

    /// Whether the agent changed `path`, or a file under it, this conversation.
    pub(crate) fn is_changed(&self, path: &Path, is_dir: bool) -> bool {
        let path = path.to_string_lossy();
        if is_dir {
            let prefix = format!("{path}/");
            self.changed.iter().any(|c| c.starts_with(&prefix))
        } else {
            self.changed.contains(path.as_ref())
        }
    }

    pub(crate) fn set_panes(&self, panes: Panes) {
        self.panes.set(panes);
    }

    pub(crate) fn checks(&self) -> &[Check] {
        &self.settings.checks
    }

    pub(crate) fn spinner(&self) -> usize {
        self.spinner
    }

    pub(crate) fn elapsed(&self) -> Option<Duration> {
        self.running_since.map(|t| t.elapsed())
    }

    pub(crate) fn totals(&self) -> (Usage, Usd, bool) {
        (self.usage, self.cost, self.cost_complete)
    }

    pub(crate) fn is_running(&self) -> bool {
        self.running_since.is_some()
    }

    pub(crate) fn should_quit(&self) -> bool {
        self.quit
    }

    /// The model chain as shown to the person: `cheap → strong`.
    pub(crate) fn chain(&self) -> String {
        if self.settings.tiers.is_empty() {
            return "no model".into();
        }
        self.settings
            .tiers
            .iter()
            .map(ModelId::as_str)
            .collect::<Vec<_>>()
            .join(" → ")
    }

    // Inputs.

    pub(crate) fn on_key(&mut self, key: KeyEvent) -> Option<Effect> {
        let ctrl_c =
            key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c');
        if !ctrl_c {
            // Any other key between the two Ctrl-C means the person changed
            // their mind.
            self.quit_armed = false;
            self.notice = None;
            if let Some(effect) = self.picker_key(key) {
                return effect;
            }
            if self.model_picker_key(key) {
                return None;
            }
        }
        if self.focus == Focus::File
            && self.pending.is_none()
            && let Some(editor) = &mut self.file
        {
            let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
            // Stopping a request and the tree work from anywhere; Tab, Ctrl-W
            // and the leader leave the file only when Vim is not mid-command.
            let global = ctrl && matches!(key.code, KeyCode::Char('c' | 'b' | 'g' | 'k' | 'e'));
            let pane = editor.is_idle()
                && (key.code == KeyCode::Tab
                    || key.code == KeyCode::Char(',')
                    || (ctrl && key.code == KeyCode::Char('w')));
            if !global && !pane {
                if editor.handle_key(key) == EditorOutcome::Close {
                    let next = if self.tree.is_some() {
                        Focus::Tree
                    } else {
                        Focus::Chat
                    };
                    self.file = None;
                    self.focus_on(next);
                }
                return None;
            }
        }
        let pending = self.pending.take();
        let action = keymap::action(self.mode, self.focus, pending, key)?;
        self.on_action(action)
    }

    fn on_action(&mut self, action: Action) -> Option<Effect> {
        match action {
            Action::Enter(mode) => {
                if mode == Mode::Command {
                    self.command.take();
                }
                // Typing always goes to the conversation.
                if mode == Mode::Insert {
                    self.focus = Focus::Chat;
                }
                self.mode = mode;
            }
            Action::Wait(pending) => self.pending = Some(pending),
            Action::Submit => {
                return match self.mode {
                    Mode::Insert => {
                        let text = self.input.text().trim().to_owned();
                        // Slash commands as in other coding agents, colon
                        // commands as in Vim: both reach the same place.
                        if let Some(line) = text.strip_prefix('/') {
                            self.input.take();
                            return self.run_command(line);
                        }
                        self.submit(text)
                    }
                    Mode::Command => {
                        let line = self.command.take();
                        self.mode = Mode::Normal;
                        self.run_command(&line)
                    }
                    Mode::Normal => None,
                };
            }
            Action::Move(lines) => self.move_focused(lines),
            Action::HalfPage(down) => {
                let lines = HALF_PAGE as i32;
                self.move_focused(if down { lines } else { -lines });
            }
            Action::Top => match self.focus {
                Focus::Tree => {
                    if let Some(tree) = &mut self.tree {
                        tree.select(0);
                    }
                }
                Focus::File => {
                    if let Some(file) = &mut self.file {
                        file.scroll_by(i32::MIN / 2);
                    }
                }
                Focus::Chat => self.scroll_back = self.max_scroll.get(),
                Focus::Docker => self.move_focused(i32::MIN / 2),
            },
            Action::Bottom => match self.focus {
                Focus::Tree => {
                    if let Some(tree) = &mut self.tree {
                        tree.select_last();
                    }
                }
                Focus::File => {
                    if let Some(file) = &mut self.file {
                        file.scroll_by(i32::MAX / 2);
                    }
                }
                Focus::Chat => self.scroll_back = 0,
                Focus::Docker => self.move_focused(i32::MAX / 2),
            },
            Action::Open => self.open_selected(),
            Action::Collapse => {
                if let Some(tree) = &mut self.tree {
                    tree.close();
                }
            }
            Action::ToggleTree => {
                if self.tree.take().is_some() {
                    if self.focus == Focus::Tree {
                        self.focus_on(if self.file.is_some() {
                            Focus::File
                        } else {
                            Focus::Chat
                        });
                    }
                } else {
                    self.tree = Some(FileTree::new(self.root.clone()));
                    self.focus_on(Focus::Tree);
                }
            }
            Action::ShowChat => self.close_file(Focus::Chat),
            Action::PickModel => self.open_model_picker(),
            Action::FocusInput => {
                self.focus = Focus::Chat;
                self.mode = Mode::Insert;
            }
            Action::ToggleDocker => {
                if self.docker.take().is_some() {
                    if self.focus == Focus::Docker {
                        self.focus_on(Focus::Chat);
                    }
                } else {
                    // Opening it leaves the focus where it was: it is something
                    // to glance at while working, not to type into.
                    self.docker = Some(DockerPane::default());
                    return Some(Effect::RefreshDocker);
                }
            }
            Action::ClosePane => match self.focus {
                Focus::Tree => {
                    self.tree = None;
                    self.focus_on(if self.file.is_some() {
                        Focus::File
                    } else {
                        Focus::Chat
                    });
                }
                Focus::File => {
                    let next = if self.tree.is_some() {
                        Focus::Tree
                    } else {
                        Focus::Chat
                    };
                    self.close_file(next);
                }
                Focus::Docker => {
                    self.docker = None;
                    self.focus_on(Focus::Chat);
                }
                Focus::Chat => {}
            },
            Action::FocusNext => self.cycle_focus(1, true),
            Action::FocusLeft => self.cycle_focus(-1, false),
            Action::FocusRight => self.cycle_focus(1, false),
            Action::Cancel => {
                if self.quit_armed {
                    self.quit = true;
                    return None;
                }
                self.quit_armed = true;
                let unsaved = self
                    .file
                    .as_ref()
                    .filter(|f| f.is_modified())
                    .map(|f| f.path().display().to_string());
                self.notice = Some(match unsaved {
                    Some(path) => format!("{path} has unsaved changes. Ctrl-C again quits anyway"),
                    None => "Ctrl-C again to quit".into(),
                });
                if self.is_running() {
                    return Some(Effect::Cancel);
                }
            }
            Action::Quit => self.quit = true,
            editing => match self.mode {
                Mode::Insert => self.input.apply(&editing),
                Mode::Command => self.command.apply(&editing),
                Mode::Normal => {}
            },
        }
        None
    }

    /// The panes on screen, left to right.
    fn visible_panes(&self) -> Vec<Focus> {
        let mut panes = Vec::new();
        if self.tree.is_some() {
            panes.push(Focus::Tree);
        }
        if self.file.is_some() {
            panes.push(Focus::File);
        }
        panes.push(Focus::Chat);
        if self.docker.is_some() {
            panes.push(Focus::Docker);
        }
        panes
    }

    fn cycle_focus(&mut self, step: i32, wrap: bool) {
        let panes = self.visible_panes();
        let len = panes.len() as i32;
        let current = panes.iter().position(|p| *p == self.focus).unwrap_or(0) as i32;
        let next = if wrap {
            (current + step).rem_euclid(len)
        } else {
            (current + step).clamp(0, len - 1)
        };
        self.focus_on(panes[next as usize]);
    }

    /// Moves the focus. Leaving the conversation leaves insert mode: the
    /// other panes are read with movement keys, not typed into.
    fn focus_on(&mut self, focus: Focus) {
        self.focus = focus;
        if focus != Focus::Chat && self.mode != Mode::Normal {
            self.mode = Mode::Normal;
        }
    }

    fn move_focused(&mut self, lines: i32) {
        match self.focus {
            Focus::Tree => {
                if let Some(tree) = &mut self.tree {
                    tree.move_by(lines);
                }
            }
            Focus::File => {
                if let Some(file) = &mut self.file {
                    file.scroll_by(lines);
                }
            }
            Focus::Chat => self.scroll(lines),
            Focus::Docker => {
                if let Some(docker) = &mut self.docker {
                    let last = docker.containers.len().saturating_sub(1) as i64;
                    docker.selected =
                        (docker.selected as i64 + i64::from(lines)).clamp(0, last) as usize;
                }
            }
        }
    }

    fn open_selected(&mut self) {
        let Some(path) = self.tree.as_mut().and_then(FileTree::open) else {
            return;
        };
        if self.file.as_ref().is_some_and(|f| f.path() == path) {
            // Already open: keep the cursor and any unsaved edits.
            self.focus_on(Focus::File);
            return;
        }
        if self.file.as_ref().is_some_and(Editor::is_modified) {
            if let Some(file) = &mut self.file {
                file.refuse_close();
            }
            self.focus_on(Focus::File);
            return;
        }
        let highlighter = Rc::clone(self.highlighter.get_or_init(|| Rc::new(Highlighter::new())));
        self.file = Some(Editor::open(&self.root, path, highlighter));
        self.focus_on(Focus::File);
    }

    pub(crate) fn on_mouse(&mut self, mouse: MouseEvent) {
        let at = Position::new(mouse.column, mouse.row);
        let panes = self.panes.get();
        let hit = if panes.docker.is_some_and(|r| r.contains(at)) {
            Focus::Docker
        } else if panes.tree.is_some_and(|r| r.contains(at)) {
            Focus::Tree
        } else if panes.file.is_some_and(|r| r.contains(at)) {
            Focus::File
        } else if panes.chat.contains(at) {
            Focus::Chat
        } else {
            return;
        };

        match mouse.kind {
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                let lines = if mouse.kind == MouseEventKind::ScrollUp {
                    -WHEEL_LINES
                } else {
                    WHEEL_LINES
                };
                // The wheel scrolls what is under the pointer without moving
                // the focus, as in most editors.
                let focus = self.focus;
                self.focus = hit;
                self.move_focused(lines);
                self.focus = focus;
            }
            MouseEventKind::Down(MouseButton::Left) => {
                if hit == Focus::Chat {
                    self.focus = Focus::Chat;
                    return;
                }
                self.focus_on(hit);
                if hit == Focus::File
                    && let (Some(area), Some(file)) = (panes.file, &mut self.file)
                {
                    // Inside the border, in text rows and columns.
                    let row = usize::from(mouse.row.saturating_sub(area.y + 1));
                    let column = usize::from(mouse.column.saturating_sub(area.x + 1));
                    file.click(row, column);
                }
                if hit == Focus::Tree
                    && let (Some(area), Some(tree)) = (panes.tree, &mut self.tree)
                {
                    // One row of border above the first entry.
                    let row = usize::from(mouse.row.saturating_sub(area.y + 1));
                    let index = tree.offset() + row;
                    if index < tree.rows().len() {
                        tree.select(index);
                        self.open_selected();
                    }
                }
            }
            _ => {}
        }
    }

    fn scroll(&mut self, down: i32) {
        let current = self.scroll_back.min(self.max_scroll.get()) as i64;
        let next = (current - i64::from(down)).clamp(0, self.max_scroll.get() as i64);
        self.scroll_back = next as usize;
    }

    fn submit(&mut self, text: String) -> Option<Effect> {
        let tiers = self.settings.tiers.clone();
        self.submit_to(text, tiers)
    }

    /// Sends `text` to `tiers` rather than the configured models, as `/claude` does.
    fn submit_to(&mut self, text: String, tiers: Vec<ModelId>) -> Option<Effect> {
        if text.is_empty() {
            return None;
        }
        if self.is_running() {
            self.transcript.push(Entry::Error(
                "Still working on the last message. Wait, or stop it with Ctrl-C".into(),
            ));
            return None;
        }
        let mut builder = AgentConfig::builder()
            .rounds_per_tier(self.settings.rounds)
            .max_turns(self.settings.max_turns);
        for tier in &tiers {
            builder = builder.tier(tier.clone());
        }
        for check in &self.settings.checks {
            builder = builder.check(check.clone());
        }
        match builder.build() {
            Ok(config) => {
                self.input.take();
                self.transcript.push(Entry::User(text.clone()));
                self.running_since = Some(Instant::now());
                self.working_model = tiers.first().cloned();
                self.scroll_back = 0;
                Some(Effect::Send { text, config })
            }
            Err(e) => {
                self.transcript.push(Entry::Error(e.to_string()));
                None
            }
        }
    }

    fn info(&mut self, text: impl Into<String>) {
        self.transcript.push(Entry::Info(text.into()));
    }

    fn error(&mut self, text: impl Into<String>) {
        self.transcript.push(Entry::Error(text.into()));
    }

    fn run_command(&mut self, line: &str) -> Option<Effect> {
        let command = match command::parse(line) {
            Ok(c) => c,
            Err(message) => {
                self.error(message);
                return None;
            }
        };
        match command {
            Command::Quit => self.quit = true,
            Command::Model(None) => self.open_model_picker(),
            Command::Claude(None) => {
                self.error("Give it a task: /claude <what to do>");
            }
            Command::Claude(Some(task)) => {
                let claude = self.claude_model();
                self.info(format!(
                    "Handed to {claude}, which works on the task alone, without this conversation"
                ));
                return self.submit_to(task, vec![claude]);
            }
            Command::Model(Some(id)) => match ModelId::new(id) {
                Ok(model) => {
                    if self.settings.tiers.is_empty() {
                        self.settings.tiers.push(model);
                    } else {
                        self.settings.tiers[0] = model;
                    }
                    let chain = self.chain();
                    self.info(format!("Models: {chain}"));
                }
                Err(e) => self.error(e.to_string()),
            },
            Command::Escalate(ids) => {
                if self.settings.tiers.is_empty() {
                    self.error("Set the first model with /model first");
                    return None;
                }
                let mut models = Vec::new();
                for id in ids {
                    match ModelId::new(id) {
                        Ok(m) => models.push(m),
                        Err(e) => {
                            self.error(e.to_string());
                            return None;
                        }
                    }
                }
                self.settings.tiers.truncate(1);
                self.settings.tiers.extend(models);
                let chain = self.chain();
                self.info(format!("Models: {chain}"));
            }
            Command::Check(None) => {
                let list = if self.settings.checks.is_empty() {
                    "none".to_owned()
                } else {
                    self.settings
                        .checks
                        .iter()
                        .map(Check::command)
                        .collect::<Vec<_>>()
                        .join(", then ")
                };
                self.info(format!("Checks: {list}"));
            }
            Command::Check(Some(line)) => match Check::parse(&line) {
                Some(check) => {
                    self.settings.checks.push(check);
                    self.info(format!("Added check: {line}"));
                }
                None => self.error("Empty check"),
            },
            Command::NoCheck => {
                self.settings.checks.clear();
                self.info("Checks removed. Add one with /check before asking for a change");
            }
            Command::Rounds(None) => {
                let rounds = self.settings.rounds;
                self.info(format!("Tries per model: {rounds}"));
            }
            Command::Rounds(Some(n)) => {
                self.settings.rounds = n;
                self.info(format!("Tries per model: {n}"));
            }
            Command::Diff => return Some(Effect::Diff),
            Command::Clear => {
                if self.is_running() {
                    self.error("Still working on the last message: stop it with Ctrl-C first");
                    return None;
                }
                self.transcript = vec![Entry::Welcome];
                self.scroll_back = 0;
                // A new conversation is a new file; the old one stays resumable.
                self.session_id = sessions::new_id();
                self.session_name = None;
                self.created = sessions::now();
                self.requests = 0;
                self.usage = Usage::default();
                self.cost = Usd::default();
                self.cost_complete = true;
                return Some(Effect::Reset);
            }
            Command::Name(None) => {
                let name = self.name();
                self.info(format!("This conversation: {name}"));
            }
            Command::Name(Some(name)) => {
                self.session_name = Some(name.clone());
                self.info(format!("Named: {name}"));
                return Some(Effect::Save);
            }
            Command::Resume => {
                if self.is_running() {
                    self.error("Still working on the last message: stop it with Ctrl-C first");
                    return None;
                }
                return Some(Effect::ListSessions);
            }
            Command::Cost => {
                let partial = if self.cost_complete {
                    ""
                } else {
                    " (some requests did not report a cost)"
                };
                let text = format!(
                    "This conversation: {} request{}, {} tokens in, {} out, {}{partial}",
                    self.requests,
                    if self.requests == 1 { "" } else { "s" },
                    self.usage.input,
                    self.usage.output,
                    self.cost
                );
                self.info(text);
            }
            Command::Help => self.info(command::HELP),
        }
        None
    }

    /// Applies what the agent reported. Returns whether a request ended,
    /// which is when the conversation is saved.
    pub(crate) fn on_agent(&mut self, message: AgentMessage) -> bool {
        match message {
            AgentMessage::Event(event) => {
                self.on_event(event);
                false
            }
            AgentMessage::Done(Ok(outcome)) => {
                let seconds = self
                    .running_since
                    .take()
                    .map_or(0, |t| t.elapsed().as_secs());
                if let Verdict::GaveUp { .. } = outcome.verdict {
                    self.transcript.push(Entry::GaveUp);
                }
                self.requests += 1;
                self.transcript.push(Entry::Cost {
                    usage: outcome.usage,
                    cost: outcome.cost,
                    complete: outcome.cost_complete,
                    seconds,
                    subscription: outcome.subscription,
                });
                true
            }
            AgentMessage::Done(Err(error)) => {
                self.running_since = None;
                self.requests += 1;
                self.error(error);
                true
            }
        }
    }

    /// The conversation's name: the one given with `/name`, else the start
    /// of the first message.
    pub(crate) fn name(&self) -> String {
        if let Some(name) = &self.session_name {
            return name.clone();
        }
        let first = self.transcript.iter().find_map(|e| match e {
            Entry::User(text) => Some(text.as_str()),
            _ => None,
        });
        match first {
            Some(text) if text.chars().count() > 60 => {
                format!("{}…", text.chars().take(60).collect::<String>())
            }
            Some(text) => text.to_owned(),
            None => "new conversation".into(),
        }
    }

    /// The conversation as it should be written to disk, or `None` while
    /// nothing has been said: an empty conversation is not worth a file.
    pub(crate) fn to_saved(&self, session: Session) -> Option<Saved> {
        if !self.transcript.iter().any(|e| matches!(e, Entry::User(_))) {
            return None;
        }
        Some(Saved {
            id: self.session_id.clone(),
            name: self.name(),
            project: self.root.clone(),
            created: self.created,
            updated: sessions::now(),
            requests: self.requests,
            usage: self.usage,
            cost: self.cost,
            cost_complete: self.cost_complete,
            transcript: self.transcript.clone(),
            session,
        })
    }

    /// Shows a saved conversation as it was, and continues it.
    pub(crate) fn load_saved(&mut self, saved: Saved) {
        self.session_id = saved.id;
        self.session_name = Some(saved.name.clone());
        self.created = saved.created;
        self.requests = saved.requests;
        self.usage = saved.usage;
        self.cost = saved.cost;
        self.cost_complete = saved.cost_complete;
        self.transcript = saved.transcript;
        self.transcript.push(Entry::Info(format!(
            "Resumed \"{}\": the conversation continues where it stopped",
            saved.name
        )));
        self.scroll_back = 0;
        self.picker = None;
    }

    pub(crate) fn show_picker(&mut self, items: Vec<Summary>) {
        if items.is_empty() {
            self.info("No saved conversation for this project yet");
            return;
        }
        self.picker = Some(Picker { items, selected: 0 });
    }

    pub(crate) fn picker(&self) -> Option<&Picker> {
        self.picker.as_ref()
    }

    /// The models the picker offers: those configured, the ones in use first.
    pub(crate) fn models(&self) -> Vec<ModelId> {
        let mut models: Vec<ModelId> = self.settings.tiers.clone();
        for model in &self.settings.models {
            if !models.contains(model) {
                models.push(model.clone());
            }
        }
        models
    }

    /// The row selected in the model picker, while it is open.
    pub(crate) fn model_picker(&self) -> Option<usize> {
        self.model_picker
    }

    /// The model the first request goes to.
    pub(crate) fn current_model(&self) -> Option<&ModelId> {
        self.settings.tiers.first()
    }

    /// The model working right now, while a request runs.
    pub(crate) fn working_model(&self) -> Option<&ModelId> {
        self.working_model.as_ref().filter(|_| self.is_running())
    }

    fn open_model_picker(&mut self) {
        let models = self.models();
        if models.is_empty() {
            self.error("No model configured: start with --model, or set IRONQUILL_MODELS");
            return;
        }
        let current = self
            .current_model()
            .and_then(|m| models.iter().position(|x| x == m))
            .unwrap_or(0);
        self.model_picker = Some(current);
    }

    /// Keys while the model picker is open. Returns whether the key was for it.
    fn model_picker_key(&mut self, key: KeyEvent) -> bool {
        let Some(selected) = self.model_picker else {
            return false;
        };
        let models = self.models();
        let last = models.len().saturating_sub(1);
        match key.code {
            KeyCode::Up => self.model_picker = Some(selected.saturating_sub(1)),
            KeyCode::Down => self.model_picker = Some((selected + 1).min(last)),
            KeyCode::Enter => {
                self.model_picker = None;
                if let Some(model) = models.get(selected).cloned() {
                    if self.settings.tiers.is_empty() {
                        self.settings.tiers.push(model.clone());
                    } else {
                        self.settings.tiers[0] = model.clone();
                    }
                    self.info(format!("Model: {model}"));
                }
            }
            KeyCode::Esc | KeyCode::Char('q') => self.model_picker = None,
            // Ctrl-E again closes it, as the shortcut that opened it.
            KeyCode::Char('e') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.model_picker = None;
            }
            _ => {}
        }
        true
    }

    /// The Claude Code model `/claude` hands tasks to: the first offered, or
    /// Claude Code's own default.
    fn claude_model(&self) -> ModelId {
        self.models()
            .into_iter()
            .find(|m| m.delegate().is_some())
            .unwrap_or_else(ModelId::claude_code)
    }

    pub(crate) fn docker(&self) -> Option<&DockerPane> {
        self.docker.as_ref()
    }

    pub(crate) fn on_docker(&mut self, result: Result<Vec<Container>, String>) {
        let Some(pane) = &mut self.docker else {
            // Closed while docker was answering.
            return;
        };
        pane.loaded = true;
        match result {
            Ok(containers) => {
                pane.selected = pane.selected.min(containers.len().saturating_sub(1));
                pane.containers = containers;
                pane.error = None;
            }
            Err(e) => pane.error = Some(e),
        }
    }

    pub(crate) fn notice(&self) -> Option<&str> {
        self.notice.as_deref()
    }

    pub(crate) fn session_label(&self) -> String {
        self.name()
    }

    pub(crate) fn report_error(&mut self, text: String) {
        self.error(text);
    }

    /// Keys while the resume list is open. Returns `None` when the key was
    /// not for the list.
    fn picker_key(&mut self, key: KeyEvent) -> Option<Option<Effect>> {
        let picker = self.picker.as_mut()?;
        let last = picker.items.len().saturating_sub(1);
        match key.code {
            KeyCode::Up => picker.selected = picker.selected.saturating_sub(1),
            KeyCode::Down => picker.selected = (picker.selected + 1).min(last),
            KeyCode::Home => picker.selected = 0,
            KeyCode::End => picker.selected = last,
            KeyCode::Enter => {
                let id = picker.items[picker.selected].id.clone();
                self.picker = None;
                return Some(Some(Effect::Resume(id)));
            }
            KeyCode::Esc | KeyCode::Char('q') => self.picker = None,
            _ => {}
        }
        Some(None)
    }

    fn on_event(&mut self, event: Event) {
        let entry = match event {
            // Tokens and cost go to the status line, not the transcript.
            Event::Turn {
                usage,
                cost,
                subscription,
                ..
            } => {
                self.usage += usage;
                match cost {
                    Some(c) => self.cost += c,
                    // A subscription owes nothing per request: the total is
                    // not incomplete for lack of a cost here.
                    None if subscription => {}
                    None => self.cost_complete = false,
                }
                return;
            }
            Event::Saying {
                text, new_block, ..
            } => {
                match self.transcript.last_mut() {
                    Some(Entry::Said(said)) if !new_block => said.push_str(&text),
                    _ => self.transcript.push(Entry::Said(text)),
                }
                return;
            }
            Event::Said { text, .. } => Entry::Said(text),
            Event::Tool {
                name,
                path,
                outcome,
            } => {
                if let Ok(ToolSummary::Changed { path, .. }) = &outcome {
                    self.on_file_changed(path);
                }
                Entry::Tool {
                    name,
                    path,
                    outcome,
                }
            }
            Event::Checking { commands } => Entry::Checks(commands),
            Event::Passed => Entry::Passed,
            Event::Failed { command, excerpt } => Entry::Failed { command, excerpt },
            Event::Escalating { from, to } => {
                self.working_model = Some(to.clone());
                Entry::Escalating { from, to }
            }
        };
        self.transcript.push(entry);
    }

    /// Closes the open file unless it has unsaved edits, which stay on screen
    /// with a note saying how to keep or drop them.
    fn close_file(&mut self, next: Focus) {
        if let Some(file) = &mut self.file
            && file.is_modified()
        {
            file.refuse_close();
            self.focus_on(Focus::File);
            return;
        }
        self.file = None;
        self.focus_on(next);
    }

    /// Keeps the tree and the open file in step with what the agent wrote.
    fn on_file_changed(&mut self, path: &str) {
        self.changed.insert(path.to_owned());
        if let Some(tree) = &mut self.tree {
            tree.refresh();
        }
        if let Some(file) = &mut self.file
            && file.path() == Path::new(path)
        {
            file.changed_on_disk();
        }
    }

    pub(crate) fn on_cancelled(&mut self) {
        self.running_since = None;
        self.info("Stopped. Files already edited stay edited: /diff shows them");
    }

    pub(crate) fn on_diff(&mut self, text: &str) {
        self.info(text);
    }

    pub(crate) fn on_tick(&mut self) {
        self.spinner = self.spinner.wrapping_add(1);
    }
}

#[cfg(test)]
mod tests {
    use ironquill_core::TokenCount;
    use ratatui::crossterm::event::{KeyCode, KeyModifiers};

    use super::*;

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
                models: vec![],
            },
            PathBuf::from("/p"),
        )
    }

    #[test]
    fn enter_sends_the_message() {
        let mut app = ready();
        type_text(&mut app, "fix it");
        let effect = press(&mut app, KeyCode::Enter);
        assert!(matches!(effect, Some(Effect::Send { ref text, .. }) if text == "fix it"));
        assert!(app.is_running());
        assert_eq!(app.input().text(), "");
    }

    #[test]
    fn a_slash_command_is_a_command_not_a_message() {
        let mut app = ready();
        type_text(&mut app, "/escalate strong");
        assert!(press(&mut app, KeyCode::Enter).is_none());
        assert_eq!(app.chain(), "cheap → strong");
        assert!(!app.is_running());
        assert_eq!(app.mode(), Mode::Insert);
    }

    #[test]
    fn colon_commands_work_from_normal_mode() {
        let mut app = ready();
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Char(':'));
        type_text(&mut app, "escalate strong");
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.chain(), "cheap → strong");
        assert_eq!(app.mode(), Mode::Normal);
    }

    #[test]
    fn a_message_without_a_model_is_refused_with_a_message() {
        let mut app = App::new(Settings::default(), PathBuf::from("/p"));
        type_text(&mut app, "hello");
        assert!(press(&mut app, KeyCode::Enter).is_none());
        assert!(matches!(app.transcript().last(), Some(Entry::Error(_))));
        assert!(!app.is_running());
    }

    #[test]
    fn clear_starts_a_new_conversation() {
        let mut app = ready();
        type_text(&mut app, "/clear");
        assert!(matches!(
            press(&mut app, KeyCode::Enter),
            Some(Effect::Reset)
        ));
        assert_eq!(app.transcript(), [Entry::Welcome]);
    }

    #[test]
    fn turns_add_up_in_the_status_line_not_the_transcript() {
        let mut app = ready();
        let before = app.transcript().len();
        for _ in 0..2 {
            app.on_agent(AgentMessage::Event(Event::Turn {
                model: ModelId::new("cheap").unwrap(),
                usage: Usage {
                    input: TokenCount(1_000),
                    output: TokenCount(100),
                },
                cost: Some(Usd(0.002)),
                subscription: false,
            }));
        }
        let (usage, cost, complete) = app.totals();
        assert_eq!(usage.input, TokenCount(2_000));
        assert!((cost.0 - 0.004).abs() < 1e-12);
        assert!(complete);
        assert_eq!(app.transcript().len(), before);
    }

    #[test]
    fn ctrl_c_stops_only_a_running_request() {
        let mut app = ready();
        let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert!(app.on_key(ctrl_c).is_none());
        type_text(&mut app, "t");
        press(&mut app, KeyCode::Enter);
        assert!(matches!(app.on_key(ctrl_c), Some(Effect::Cancel)));
    }

    #[test]
    fn line_editing_counts_characters() {
        let mut app = ready();
        type_text(&mut app, "café au lait");
        let ctrl_w = KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL);
        app.on_key(ctrl_w);
        assert_eq!(app.input().text(), "café au ");
        press(&mut app, KeyCode::Home);
        for _ in 0..4 {
            press(&mut app, KeyCode::Right);
        }
        press(&mut app, KeyCode::Backspace);
        assert_eq!(app.input().text(), "caf au ");
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

    #[test]
    fn the_tree_opens_a_file_with_arrows_and_comma_c_returns_to_the_chat() {
        let (_dir, mut app) = project();
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Char(','));
        press(&mut app, KeyCode::Char('n'));
        assert_eq!(app.focus(), Focus::Tree);

        press(&mut app, KeyCode::Right); // expand src/
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Enter); // open src/lib.rs
        assert_eq!(app.focus(), Focus::File);
        assert_eq!(app.file().unwrap().lines(), ["one", "two"]);

        press(&mut app, KeyCode::Tab);
        assert_eq!(app.focus(), Focus::Chat);
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.focus(), Focus::Tree);

        press(&mut app, KeyCode::Char(','));
        press(&mut app, KeyCode::Char('c'));
        assert_eq!(app.focus(), Focus::Chat);
        assert!(app.file().is_none());
    }

    #[test]
    fn typing_i_from_the_tree_goes_back_to_the_message() {
        let (_dir, mut app) = project();
        app.on_key(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL));
        assert_eq!((app.focus(), app.mode()), (Focus::Tree, Mode::Normal));
        press(&mut app, KeyCode::Char('i'));
        assert_eq!((app.focus(), app.mode()), (Focus::Chat, Mode::Insert));
    }

    #[test]
    fn an_edit_by_the_agent_marks_the_file_and_refreshes_the_open_view() {
        let (dir, mut app) = project();
        app.on_key(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL));
        press(&mut app, KeyCode::Right);
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Enter);

        std::fs::write(dir.path().join("src/lib.rs"), "one\n2\n").unwrap();
        app.on_agent(AgentMessage::Event(Event::Tool {
            name: "replace".into(),
            path: Some("src/lib.rs".into()),
            outcome: Ok(ToolSummary::Changed {
                path: "src/lib.rs".into(),
                created: false,
                diff: vec![],
            }),
        }));

        assert!(app.is_changed(Path::new("src/lib.rs"), false));
        assert!(app.is_changed(Path::new("src"), true));
        assert_eq!(app.file().unwrap().lines(), ["one", "2"]);
    }

    fn ctrl_c(app: &mut App) -> Option<Effect> {
        app.on_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL))
    }

    #[test]
    fn ctrl_c_twice_quits_and_another_key_in_between_does_not() {
        let mut app = ready();
        ctrl_c(&mut app);
        assert!(!app.should_quit());
        assert_eq!(app.notice(), Some("Ctrl-C again to quit"));
        press(&mut app, KeyCode::Char('x'));
        ctrl_c(&mut app);
        assert!(!app.should_quit());
        ctrl_c(&mut app);
        assert!(app.should_quit());
    }

    #[test]
    fn the_first_ctrl_c_also_stops_a_running_request() {
        let mut app = ready();
        type_text(&mut app, "t");
        press(&mut app, KeyCode::Enter);
        assert!(matches!(ctrl_c(&mut app), Some(Effect::Cancel)));
        assert!(!app.should_quit());
    }

    #[test]
    fn a_conversation_is_saved_once_something_was_said_and_comes_back_whole() {
        let mut app = ready();
        assert!(app.to_saved(Session::new()).is_none());

        type_text(&mut app, "fix the parser please");
        press(&mut app, KeyCode::Enter);
        app.on_agent(AgentMessage::Event(Event::Turn {
            model: ModelId::new("cheap").unwrap(),
            usage: Usage {
                input: TokenCount(500),
                output: TokenCount(50),
            },
            cost: Some(Usd(0.0003)),
            subscription: false,
        }));
        let finished = app.on_agent(AgentMessage::Done(Ok(Outcome {
            verdict: Verdict::Answered,
            usage: Usage {
                input: TokenCount(500),
                output: TokenCount(50),
            },
            cost: Usd(0.0003),
            cost_complete: true,
            subscription: false,
            changed: vec![],
        })));
        assert!(finished);
        assert!(matches!(app.transcript().last(), Some(Entry::Cost { .. })));

        let saved = app.to_saved(Session::new()).unwrap();
        assert_eq!(saved.name, "fix the parser please");
        assert_eq!(saved.requests, 1);

        let mut other = ready();
        other.load_saved(saved);
        assert_eq!(other.name(), "fix the parser please");
        assert_eq!(other.totals().0.input, TokenCount(500));
        assert!(
            other
                .transcript()
                .contains(&Entry::User("fix the parser please".into()))
        );
    }

    #[test]
    fn the_resume_list_picks_with_arrows_and_enter() {
        let mut app = ready();
        let item = |id: &str| Summary {
            id: id.into(),
            name: id.into(),
            updated: 0,
            requests: 1,
            cost: Usd(0.0),
        };
        app.show_picker(vec![item("a"), item("b")]);
        press(&mut app, KeyCode::Down);
        let effect = press(&mut app, KeyCode::Enter);
        assert!(matches!(effect, Some(Effect::Resume(ref id)) if id == "b"));
        assert!(app.picker().is_none());
    }

    #[test]
    fn ctrl_g_goes_back_to_typing_even_from_inside_the_editor() {
        let (_dir, mut app) = project();
        app.on_key(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL));
        press(&mut app, KeyCode::Right);
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Enter);
        press(&mut app, KeyCode::Char('i')); // insert mode in the file
        assert_eq!(app.focus(), Focus::File);

        app.on_key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL));
        assert_eq!((app.focus(), app.mode()), (Focus::Chat, Mode::Insert));
        type_text(&mut app, "hi");
        assert_eq!(app.input().text(), "hi");
    }

    #[test]
    fn ctrl_k_opens_and_closes_the_docker_pane() {
        let mut app = ready();
        let ctrl_k = KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL);
        assert!(matches!(app.on_key(ctrl_k), Some(Effect::RefreshDocker)));
        assert_eq!(app.focus(), Focus::Chat);
        app.on_docker(Ok(vec![Container {
            id: "1".into(),
            name: "db".into(),
            image: "postgres".into(),
            status: "Up 2 hours".into(),
            ports: String::new(),
        }]));
        assert_eq!(app.docker().unwrap().containers.len(), 1);

        press(&mut app, KeyCode::Tab);
        assert_eq!(app.focus(), Focus::Docker);
        app.on_key(ctrl_k);
        assert!(app.docker().is_none());
        assert_eq!(app.focus(), Focus::Chat);
    }

    #[test]
    fn streamed_text_builds_one_reply_and_a_new_block_starts_another() {
        let mut app = ready();
        let saying = |text: &str, new_block: bool| {
            AgentMessage::Event(Event::Saying {
                model: ModelId::claude_code(),
                text: text.into(),
                new_block,
            })
        };
        app.on_agent(saying("", true));
        app.on_agent(saying("Working", false));
        app.on_agent(saying(" on it.", false));
        app.on_agent(saying("", true));
        app.on_agent(saying("Done.", false));
        let said: Vec<&Entry> = app
            .transcript()
            .iter()
            .filter(|e| matches!(e, Entry::Said(_)))
            .collect();
        assert_eq!(
            said,
            [
                &Entry::Said("Working on it.".into()),
                &Entry::Said("Done.".into())
            ]
        );
    }

    #[test]
    fn ctrl_e_picks_the_model_with_arrows() {
        let mut app = App::new(
            Settings {
                tiers: vec![ModelId::new("cheap").unwrap()],
                models: vec![ModelId::new("claude-code/opus").unwrap()],
                ..Settings::default()
            },
            PathBuf::from("/p"),
        );
        app.on_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL));
        assert_eq!(app.model_picker(), Some(0));
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.model_picker(), None);
        assert_eq!(app.current_model().unwrap().as_str(), "claude-code/opus");
    }

    #[test]
    fn slash_claude_hands_one_task_over_without_changing_the_model() {
        let mut app = ready();
        type_text(&mut app, "/claude refactor the parser");
        let effect = press(&mut app, KeyCode::Enter);
        let Some(Effect::Send { text, .. }) = effect else {
            panic!("/claude should send the task");
        };
        assert_eq!(text, "refactor the parser");
        assert_eq!(app.working_model().unwrap().as_str(), "claude-code");
        assert_eq!(app.current_model().unwrap().as_str(), "cheap");
    }

    #[test]
    fn a_subscription_turn_leaves_the_total_complete() {
        let mut app = ready();
        app.on_agent(AgentMessage::Event(Event::Turn {
            model: ModelId::claude_code(),
            usage: Usage {
                input: TokenCount(30_000),
                output: TokenCount(200),
            },
            cost: None,
            subscription: true,
        }));
        let (usage, cost, complete) = app.totals();
        assert_eq!(usage.input, TokenCount(30_000));
        assert_eq!(cost, Usd(0.0));
        assert!(complete);
    }

    #[test]
    fn scrolling_stays_within_the_transcript() {
        let mut app = ready();
        app.set_max_scroll(5);
        press(&mut app, KeyCode::Esc);
        for _ in 0..20 {
            press(&mut app, KeyCode::Up);
        }
        assert_eq!(app.scroll_back(), 5);
        press(&mut app, KeyCode::End);
        assert_eq!(app.scroll_back(), 0);
    }
}
