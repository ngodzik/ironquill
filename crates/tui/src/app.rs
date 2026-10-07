use std::cell::{Cell, OnceCell, RefCell};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use ironquill_agent::{
    AgentConfig, Answer, Approval, Compaction, Event, Member, Outcome, Pair, Question, Session,
    Verdict,
};
use ironquill_core::{Agent, ContextUse, Effort, ModelId, TokenCount, Usage, Usd};
use ironquill_tools::{Check, Container, DiffLine, ToolSummary};
use ratatui::crossterm::event::{
    KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::{Position, Rect};
use tokio::sync::oneshot;

use crate::command::{self, Command};
use crate::defaults::Defaults;
use crate::editor::{Editor, Outcome as EditorOutcome};
use crate::highlight::Highlighter;
use crate::keymap::{self, Action, Focus, Mode, Pending};
use crate::sessions::{self, Saved, Summary};
use crate::tree::FileTree;
use crate::usage::{Sample, UsageLog};

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
    pub(crate) sub: Option<Rect>,
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
    /// with `claude-code` or `codex` hand the task to that agent.
    pub models: Vec<ModelId>,
    /// The models the first one may hand tasks to.
    pub team: Vec<ModelId>,
    /// The most one request may cost; `None` sets no limit.
    pub budget: Option<Usd>,
    /// Every model the provider lists, with a note on its price and context,
    /// to search from the model picker and to tell the team apart.
    pub catalog: Vec<Member>,
    /// Where scores in the notes come from, to credit it.
    pub credits: Option<String>,
    /// How hard models think before answering.
    pub effort: Effort,
    /// The member of the team that plans in a pair; `None` picks
    /// the best scored, or the dearest.
    pub planner: Option<ModelId>,
    /// Without checks set, find the project's own when checking: its test
    /// runner, and tests a model has just written.
    pub detect_checks: bool,
    /// What the person should know at startup, such as a model given on the
    /// command line in place of the one kept.
    pub notes: Vec<String>,
    /// The usage pane's window, in seconds, as kept from the last session.
    pub usage_window: Option<u64>,
    /// The secrets of the environment commands may use, as the person
    /// allowed them.
    pub allowed_secrets: Vec<String>,
    /// Whether a command running a program ironquill does not know asks.
    pub strict_commands: bool,
    /// Servers commands may reach besides those already used.
    pub allowed_hosts: Vec<String>,
    /// `/pair` with Claude Code's Opus planning and its Sonnet coding.
    pub pair_mode: bool,
    /// Whether the warm sessions are kept warm while the conversation
    /// waits, as `/tick` turns on.
    pub tick: bool,
}

/// Tokens and cost of one part of a request.
#[derive(Debug, Clone, Copy, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Spent {
    pub(crate) usage: Usage,
    pub(crate) cost: Usd,
    /// Some turns ran on a subscription, which costs nothing per request.
    pub(crate) subscription: bool,
}

impl std::fmt::Display for Spent {
    /// `$0.012 · 12.3k in · 800 out`, or `subscription · …`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.subscription && self.cost.0 == 0.0 {
            f.write_str("subscription")?;
        } else {
            write!(f, "{}", self.cost)?;
        }
        write!(f, " · {} in · {} out", self.usage.input, self.usage.output)
    }
}

/// A task handed to a model of the team, as the sub-agent pane shows it.
pub(crate) struct SubAgent<'a> {
    pub(crate) from: &'a ModelId,
    pub(crate) to: &'a ModelId,
    pub(crate) task: &'a str,
    pub(crate) spent: Spent,
    /// What it did, in order.
    pub(crate) work: Vec<&'a Entry>,
    /// Still working on it.
    pub(crate) working: bool,
}

/// The model picker (Ctrl-E) while it is open.
#[derive(Debug, Default)]
pub(crate) struct ModelPicker {
    /// What was typed to search the provider's models.
    pub(crate) filter: String,
    pub(crate) selected: usize,
    /// The effort when it opened, to say so when ← → changed it.
    pub(crate) effort_at_open: Effort,
}

/// One row of the model picker.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ModelRow {
    pub(crate) model: ModelId,
    pub(crate) note: String,
    /// Among the models offered every time, not only found by the search.
    pub(crate) offered: bool,
    pub(crate) in_team: bool,
}

/// Something only the event loop can do, asked for by the state.
#[derive(Debug)]
pub(crate) enum Effect {
    /// Put this text on the system clipboard.
    Copy(String),
    /// Read the warm sessions, to keep their caches warm.
    KeepWarm,
    /// Group the conversation's exchanges by subject, for /compact.
    PlanCompaction(AgentConfig),
    /// Compact the conversation to the exchanges kept.
    Compact {
        config: AgentConfig,
        keep: Vec<usize>,
        last_as_is: bool,
    },
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
    /// Open the conversation's context in the editor.
    OpenContext,
    /// Replace the conversation's context with this edited text.
    ApplyContext(String),
    /// End this agent's session.
    ForgetDelegate(Agent),
    /// Keep the current choices for every new session.
    SaveDefaults(Defaults),
    /// Open the person's instructions for every model, creating the file.
    OpenInstructions,
}

/// What the agent task sends back to the interface.
#[derive(Debug)]
pub(crate) enum AgentMessage {
    Event(Event),
    /// A command held for the person, and where to send their answer.
    Approve(Approval, oneshot::Sender<Answer>),
    /// The person stopped the request before it began: it was not sent.
    NotSent(String),
    /// The subjects of the conversation, for the person to pick from.
    Compaction(Result<Compaction, String>),
    /// The conversation was compacted: about how many tokens before, after.
    Compacted(Result<(TokenCount, TokenCount), String>),
    Done(Result<Outcome, String>),
}

/// One item in the transcript. Saved with the conversation, so that a
/// resumed one shows what it showed.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) enum Entry {
    Welcome,
    Info(String),
    /// How a pair ended, who did what and what changed, in one line.
    Ended(String),
    /// Something held back by a safety check: what, and why.
    Refused(String),
    /// A command a model ran, folded to its first line until opened.
    Command {
        model: ModelId,
        command: String,
        status: String,
        output: String,
        checked_by: Option<ModelId>,
    },
    /// The person stopped the request before it ended.
    Interrupted,
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
    /// The model handed a task to another of the team.
    Delegating {
        from: ModelId,
        to: ModelId,
        task: String,
        /// What the member's work cost, as it accrues.
        #[serde(default)]
        spent: Spent,
    },
    /// The request spent its budget and stopped.
    OverBudget {
        spent: Usd,
        budget: Usd,
    },
    /// A step of a request worked on in a pair: what, by whom.
    Step {
        number: u8,
        of: u8,
        name: String,
        model: Option<ModelId>,
        effort: Option<Effort>,
    },
    /// What a model of the team did on a task handed to it, shown apart
    /// from the model that answers.
    Member {
        model: ModelId,
        entry: Box<Entry>,
    },
    /// What one request cost, shown under it.
    Cost {
        usage: Usage,
        cost: Usd,
        complete: bool,
        seconds: u64,
        /// Ran on a subscription (Claude Code): no cost is owed for it.
        #[serde(default)]
        subscription: bool,
        /// How full the context was on the request's last call.
        #[serde(default)]
        context: Option<ContextUse>,
        /// The models that worked on it in turn, consecutive calls to the
        /// same one together, with what each run cost.
        #[serde(default)]
        runs: Vec<(ModelId, Spent)>,
    },
}

impl Entry {
    /// A model's reply, which folds when long.
    pub(crate) fn is_reply(&self) -> bool {
        match self {
            Entry::Said(_) => true,
            Entry::Member { entry, .. } => entry.is_reply(),
            _ => false,
        }
    }

    /// What can be selected, folded and copied: a reply, or a command.
    pub(crate) fn folds(&self) -> bool {
        match self {
            Entry::Said(_) | Entry::Command { .. } => true,
            Entry::Member { entry, .. } => entry.folds(),
            _ => false,
        }
    }

    /// The text copied from it: a reply, or a command and what it printed.
    pub(crate) fn copied(&self) -> Option<String> {
        match self {
            Entry::Said(text) => Some(text.clone()),
            Entry::Command {
                command, output, ..
            } => Some(format!("{command}\n{output}")),
            Entry::Member { entry, .. } => entry.copied(),
            _ => None,
        }
    }
}

/// What /compact keeps: subjects, each open or not, ticked exchanges.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CompactPicker {
    pub(crate) compaction: Compaction,
    /// Whether each exchange is kept, summed up.
    pub(crate) kept: Vec<bool>,
    /// The subject shown with its exchanges.
    pub(crate) open: Option<usize>,
    pub(crate) cursor: usize,
    /// Whether the last exchange stays as it was rather than summed up.
    pub(crate) last_as_is: bool,
}

/// A row of the /compact window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CompactRow {
    Subject(usize),
    Exchange(usize),
}

impl CompactPicker {
    /// The rows shown: each subject, and the exchanges of the open one.
    pub(crate) fn rows(&self) -> Vec<CompactRow> {
        let mut rows = Vec::new();
        for (s, subject) in self.compaction.subjects.iter().enumerate() {
            rows.push(CompactRow::Subject(s));
            if self.open == Some(s) {
                rows.extend(subject.exchanges.iter().map(|e| CompactRow::Exchange(*e)));
            }
        }
        rows
    }
}

/// The messages sent before, gone through with Up and Down as in a shell.
#[derive(Debug, Default)]
struct History {
    sent: Vec<String>,
    /// The message shown, as an index in `sent`; `None` on the draft.
    at: Option<usize>,
    /// What was being typed before going back.
    draft: String,
}

impl History {
    /// Remembers a message sent, once when it is sent again in a row, and
    /// goes back to a new draft.
    fn push(&mut self, text: String) {
        self.at = None;
        self.draft.clear();
        if self.sent.last() != Some(&text) {
            self.sent.push(text);
        }
    }

    /// The message `step` away from the one shown, back when negative; past
    /// the latest, the draft `typing` was when going back. `None` when there
    /// is nothing further that way.
    fn recall(&mut self, step: i32, typing: &str) -> Option<String> {
        let next = match self.at {
            None if step < 0 && !self.sent.is_empty() => {
                self.draft = typing.to_owned();
                self.sent.len() - 1
            }
            None => return None,
            Some(at) if step < 0 => at.checked_sub(1)?,
            Some(at) if at + 1 < self.sent.len() => at + 1,
            Some(_) => {
                self.at = None;
                return Some(std::mem::take(&mut self.draft));
            }
        };
        self.at = Some(next);
        Some(self.sent[next].clone())
    }
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

    /// Replaces the text, the cursor at its end.
    fn set(&mut self, text: String) {
        self.cursor = text.chars().count();
        self.text = text;
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
    /// The messages sent before, for Up and Down in the message box.
    history: History,
    /// Each call to a model of the last day, for the usage pane.
    usage_log: UsageLog,
    /// The usage pane's window, in seconds, kept while it is hidden.
    usage_window: u64,
    /// Whether the usage pane shows.
    usage_open: bool,
    /// A command held for the person, and where their answer goes.
    approval: Option<(Approval, oneshot::Sender<Answer>)>,
    /// The subjects to keep or drop, while /compact asks.
    compact_picker: Option<CompactPicker>,
    /// When the last request ended, and the sessions were last kept warm.
    idle_since: Instant,
    last_warm: Option<Instant>,
    /// Asking whether to keep the sessions warm, after a long wait.
    ask_keep_warm: bool,
    command: LineEditor,
    transcript: Vec<Entry>,
    /// Lines scrolled up from the bottom; 0 follows new output.
    scroll_back: Cell<usize>,
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
    /// The model of the team working on a task handed to it, if one is.
    member: Option<ModelId>,
    /// The step of a request in a pair being worked on.
    step: Option<String>,
    /// The handover whose work the sub-agent pane shows, by its place in
    /// the transcript.
    sub_view: Option<usize>,
    /// How many lines the sub-agent pane is scrolled up from its end; 0
    /// follows the work as it comes.
    sub_scroll: Cell<usize>,
    /// The most it can be, written by the view.
    sub_max: Cell<usize>,
    /// What the current request cost so far, without its sub-agents.
    request_spent: Spent,
    /// The models of the current request in turn, with what each run cost.
    runs: Vec<(ModelId, Spent)>,
    /// The choices as last kept for new sessions, to keep them again as
    /// soon as they change.
    kept: Defaults,
    /// A one-line note in the status line, cleared by the next key.
    notice: Option<String>,
    session_id: String,
    session_name: Option<String>,
    created: u64,
    requests: usize,
    picker: Option<Picker>,
    /// Replies unfolded by the person, by transcript index. Long replies are
    /// folded otherwise.
    expanded: BTreeSet<usize>,
    /// The reply selected in normal mode, by transcript index.
    selected_reply: Option<usize>,
    /// Asks the view to scroll the selected reply into sight.
    reveal: Cell<bool>,
    /// Where each entry was drawn, as (entry, first line, last line) in the
    /// whole transcript, and the transcript's first visible line and area,
    /// written by the view so that a click finds its entry.
    entry_lines: RefCell<Vec<(usize, usize, usize)>>,
    transcript_view: Cell<(usize, Rect)>,
    /// The transcript's lines a click folds, and those that copy a block.
    fold_marks: RefCell<Vec<usize>>,
    copy_marks: RefCell<Vec<(usize, String)>>,
    /// The conversation has the whole screen; the other panes keep their
    /// state, hidden, until zooming back out.
    zoomed: bool,
    /// The pane that had the focus before zooming in, given back after.
    zoom_focus: Option<Focus>,
    /// Tab completion of a command, while it cycles through candidates.
    completion: Option<command::Completion>,
    /// The list of shortcuts, open at this scroll offset.
    keys_open: Option<usize>,
    /// The model picker, open on the row selected.
    model_picker: Option<ModelPicker>,
    /// How full the context was on the latest call, for the status line.
    context: Option<ContextUse>,
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
    pub(crate) fn new(mut settings: Settings, root: PathBuf) -> Self {
        // The picker offers every model known at startup, the ones in use
        // first, and keeps offering them whatever is picked later.
        let mut models = settings.tiers.clone();
        for model in std::mem::take(&mut settings.models) {
            if !models.contains(&model) {
                models.push(model);
            }
        }
        settings.models = models;
        let mut transcript = vec![Entry::Welcome];
        transcript.extend(settings.notes.drain(..).map(Entry::Info));
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
        let usage_window = settings.usage_window.unwrap_or(3600);
        let mut app = Self {
            settings,
            root,
            project,
            mode: Mode::Insert,
            focus: Focus::Chat,
            pending: None,
            input: LineEditor::default(),
            history: History::default(),
            usage_log: UsageLog::default(),
            usage_window,
            usage_open: false,
            approval: None,
            compact_picker: None,
            idle_since: Instant::now(),
            last_warm: None,
            ask_keep_warm: false,
            command: LineEditor::default(),
            transcript,
            scroll_back: Cell::new(0),
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
            member: None,
            step: None,
            sub_view: None,
            sub_scroll: Cell::new(0),
            sub_max: Cell::new(0),
            request_spent: Spent::default(),
            runs: Vec::new(),
            kept: Defaults::default(),
            notice: None,
            session_id: sessions::new_id(),
            session_name: None,
            created: sessions::now(),
            requests: 0,
            picker: None,
            expanded: BTreeSet::new(),
            selected_reply: None,
            reveal: Cell::new(false),
            entry_lines: RefCell::new(Vec::new()),
            transcript_view: Cell::new((0, Rect::default())),
            fold_marks: RefCell::new(Vec::new()),
            copy_marks: RefCell::new(Vec::new()),
            zoomed: false,
            zoom_focus: None,
            completion: None,
            keys_open: None,
            model_picker: None,
            working_model: None,
            context: None,
            docker: None,
        };
        app.kept = app.defaults();
        app
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
        self.scroll_back.get()
    }

    /// Scrolls the conversation so that a given line range is in view. Called
    /// by the view, the only part that knows where an entry's lines are.
    pub(crate) fn set_scroll_back(&self, back: usize) {
        self.scroll_back.set(back);
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

    /// How full the context was on the latest call, when known.
    pub(crate) fn context(&self) -> Option<ContextUse> {
        self.context
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
            if let Some(effect) = self.approval_key(key) {
                return effect;
            }
            if let Some(effect) = self.compact_key(key) {
                return effect;
            }
            if let Some(effect) = self.picker_key(key) {
                return effect;
            }
            if self.model_picker_key(key) {
                return None;
            }
            if self.keys_key(key) {
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
            let global = ctrl
                && matches!(
                    key.code,
                    KeyCode::Char('c' | 'b' | 'g' | 'k' | 'e' | 'q' | 's' | 'a' | 'z' | 't')
                );
            let pane = editor.is_idle()
                && (key.code == KeyCode::Tab
                    || key.code == KeyCode::Char(',')
                    || (ctrl && key.code == KeyCode::Char('w')));
            if !global && !pane {
                match editor.handle_key(key) {
                    EditorOutcome::Command(line) => return self.run_command(&line),
                    EditorOutcome::Context { text, close } => {
                        if close {
                            self.file = None;
                            self.focus_on(Focus::Chat);
                        }
                        return Some(Effect::ApplyContext(text));
                    }
                    EditorOutcome::Close => {
                        let next = if self.tree.is_some() {
                            Focus::Tree
                        } else {
                            Focus::Chat
                        };
                        self.file = None;
                        self.focus_on(next);
                        if let Some(tree) = &mut self.tree {
                            tree.refresh();
                        }
                    }
                    // Written: the file's git status may have changed.
                    EditorOutcome::Saved => {
                        if let Some(tree) = &mut self.tree {
                            tree.refresh();
                        }
                    }
                    EditorOutcome::Stay => {}
                }
                return None;
            }
        }
        if key.code == KeyCode::Tab && self.complete_command() {
            return None;
        }
        // Any other key ends a completion in progress.
        if key.code != KeyCode::Tab {
            self.completion = None;
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
                if mode == Mode::Normal
                    && self.focus == Focus::Chat
                    && self.selected_reply.is_none()
                {
                    self.selected_reply = self.replies().last().copied();
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
                        if !text.is_empty() {
                            self.history.push(text.clone());
                        }
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
            Action::Recall(step) => {
                if let Some(text) = self.history.recall(step, self.input.text()) {
                    self.input.set(text);
                }
            }
            Action::Move(lines) if self.focus == Focus::Chat && self.mode == Mode::Normal => {
                self.select_reply(lines);
            }
            Action::Move(lines) => self.move_focused(lines),
            Action::CopySelected => {
                if let Some(text) = self
                    .selected_reply
                    .and_then(|i| self.transcript.get(i))
                    .and_then(Entry::copied)
                {
                    return Some(Effect::Copy(text));
                }
            }
            Action::ToggleUsage => self.usage_open = !self.usage_open,
            Action::NextUsageWindow => {
                self.usage_window = match self.usage_window {
                    w if w < 6 * 3600 => 6 * 3600,
                    w if w < 24 * 3600 => 24 * 3600,
                    _ => 3600,
                };
                self.usage_open = true;
            }
            Action::Fold => match self.selected_reply {
                Some(entry) => self.toggle_fold(entry),
                // Nothing to fold: Enter keeps its old meaning.
                None => {
                    self.focus = Focus::Chat;
                    self.mode = Mode::Insert;
                }
            },
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
                Focus::Chat => self.scroll_back.set(self.max_scroll.get()),
                Focus::Docker => self.move_focused(i32::MIN / 2),
                Focus::SubAgent => self.sub_scroll.set(self.sub_max.get()),
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
                Focus::Chat => self.scroll_back.set(0),
                Focus::Docker => self.move_focused(i32::MAX / 2),
                Focus::SubAgent => self.sub_scroll.set(0),
            },
            Action::Open => self.open_selected(),
            Action::Collapse => {
                if let Some(tree) = &mut self.tree {
                    tree.close();
                }
            }
            Action::ToggleTree => {
                self.unzoom();
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
            Action::ToggleSubAgent => {
                self.sub_scroll.set(0);
                if self.focus == Focus::SubAgent {
                    self.focus_on(Focus::Chat);
                }
                self.sub_view = match self.sub_view {
                    Some(_) => None,
                    None => {
                        let last = self
                            .transcript
                            .iter()
                            .rposition(|e| matches!(e, Entry::Delegating { .. }));
                        if last.is_none() {
                            self.info("No task was handed to the team in this conversation yet");
                        }
                        last
                    }
                };
            }
            Action::Zoom => {
                self.zoomed = !self.zoomed;
                if self.zoomed {
                    self.zoom_focus = Some(self.focus);
                    self.focus = Focus::Chat;
                } else if let Some(focus) = self.zoom_focus.take() {
                    self.focus_on(focus);
                }
            }
            Action::FocusTree => {
                self.unzoom();
                match &mut self.tree {
                    Some(tree) => tree.refresh(),
                    None => self.tree = Some(FileTree::new(self.root.clone())),
                }
                self.focus_on(Focus::Tree);
            }
            Action::ShowKeys => {
                self.keys_open = match self.keys_open {
                    Some(_) => None,
                    None => Some(0),
                };
            }
            Action::FocusInput => {
                self.focus = Focus::Chat;
                self.mode = Mode::Insert;
            }
            Action::ToggleDocker => {
                self.unzoom();
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
                Focus::SubAgent => {
                    self.sub_view = None;
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

    /// Transcript indices of the model's replies, the entries that fold.
    fn replies(&self) -> Vec<usize> {
        self.transcript
            .iter()
            .enumerate()
            .filter(|(_, e)| e.folds())
            .map(|(i, _)| i)
            .collect()
    }

    /// Moves the selection `step` replies down (up when negative), starting
    /// from the last reply when none is selected.
    fn select_reply(&mut self, step: i32) {
        let replies = self.replies();
        let Some(last) = replies.len().checked_sub(1) else {
            return;
        };
        let at = match self
            .selected_reply
            .and_then(|e| replies.iter().position(|r| *r == e))
        {
            Some(at) => (at as i64 + i64::from(step)).clamp(0, last as i64) as usize,
            None => last,
        };
        self.selected_reply = Some(replies[at]);
        self.reveal.set(true);
    }

    fn toggle_fold(&mut self, entry: usize) {
        if !self.expanded.remove(&entry) {
            self.expanded.insert(entry);
        }
        self.selected_reply = Some(entry);
        self.reveal.set(true);
    }

    pub(crate) fn is_expanded(&self, entry: usize) -> bool {
        self.expanded.contains(&entry)
    }

    /// The selected reply, shown as such only in normal mode in the conversation.
    pub(crate) fn selected_reply(&self) -> Option<usize> {
        (self.focus == Focus::Chat && self.mode == Mode::Normal)
            .then_some(self.selected_reply)
            .flatten()
    }

    /// Whether the view should scroll the selected reply into sight; reading
    /// it clears it.
    pub(crate) fn take_reveal(&self) -> bool {
        self.reveal.replace(false)
    }

    pub(crate) fn set_marks(&self, fold: Vec<usize>, copy: Vec<(usize, String)>) {
        *self.fold_marks.borrow_mut() = fold;
        *self.copy_marks.borrow_mut() = copy;
    }

    pub(crate) fn set_entry_lines(
        &self,
        lines: Vec<(usize, usize, usize)>,
        top: usize,
        area: Rect,
    ) {
        *self.entry_lines.borrow_mut() = lines;
        self.transcript_view.set((top, area));
    }

    pub(crate) fn is_zoomed(&self) -> bool {
        self.zoomed
    }

    /// Leaves full screen without moving the focus: for actions that need
    /// another pane on screen.
    fn unzoom(&mut self) {
        self.zoomed = false;
        self.zoom_focus = None;
    }

    /// The panes on screen, left to right.
    fn visible_panes(&self) -> Vec<Focus> {
        if self.zoomed {
            return vec![Focus::Chat];
        }
        let mut panes = Vec::new();
        if self.tree.is_some() {
            panes.push(Focus::Tree);
        }
        if self.file.is_some() {
            panes.push(Focus::File);
        }
        panes.push(Focus::Chat);
        if self.sub_view.is_some() {
            panes.push(Focus::SubAgent);
        }
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
            Focus::SubAgent => {
                let up = self.sub_scroll.get() as i64 - i64::from(lines);
                self.sub_scroll
                    .set(up.clamp(0, self.sub_max.get() as i64) as usize);
            }
            Focus::Docker => {
                if let Some(docker) = &mut self.docker {
                    let last = docker.containers.len().saturating_sub(1) as i64;
                    docker.selected =
                        (docker.selected as i64 + i64::from(lines)).clamp(0, last) as usize;
                }
            }
        }
    }

    /// Opens a file in the editor, outside the project too.
    pub(crate) fn open_path(&mut self, path: PathBuf) {
        if self.file.as_ref().is_some_and(Editor::is_modified) {
            if let Some(file) = &mut self.file {
                file.refuse_close();
            }
            self.focus_on(Focus::File);
            return;
        }
        let highlighter = Rc::clone(self.highlighter.get_or_init(|| Rc::new(Highlighter::new())));
        self.unzoom();
        self.file = Some(Editor::open(&self.root, path, highlighter));
        self.focus_on(Focus::File);
        self.info(
            "Your instructions for every model: :w saves them, they count from the next request",
        );
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

    pub(crate) fn on_mouse(&mut self, mouse: MouseEvent) -> Option<Effect> {
        let at = Position::new(mouse.column, mouse.row);
        let panes = self.panes.get();
        let hit = if panes.docker.is_some_and(|r| r.contains(at)) {
            Focus::Docker
        } else if panes.sub.is_some_and(|r| r.contains(at)) {
            Focus::SubAgent
        } else if panes.tree.is_some_and(|r| r.contains(at)) {
            Focus::Tree
        } else if panes.file.is_some_and(|r| r.contains(at)) {
            Focus::File
        } else if panes.chat.contains(at) {
            Focus::Chat
        } else {
            return None;
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
                    let (top, area) = self.transcript_view.get();
                    if area.contains(at) {
                        let line = top + usize::from(mouse.row - area.y);
                        // A code block's mark copies it as written.
                        let code = self
                            .copy_marks
                            .borrow()
                            .iter()
                            .find(|(at, _)| *at == line)
                            .map(|(_, code)| code.clone());
                        if let Some(code) = code {
                            return Some(Effect::Copy(code));
                        }
                        // Only a fold mark folds: a click elsewhere in a
                        // reply should not make it jump.
                        let entry = self
                            .entry_lines
                            .borrow()
                            .iter()
                            .find(|(_, first, last)| (*first..=*last).contains(&line))
                            .map(|(e, _, _)| *e);
                        if let Some(entry) = entry
                            && self.fold_marks.borrow().contains(&line)
                        {
                            self.toggle_fold(entry);
                        }
                    }
                    return None;
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
        None
    }

    fn scroll(&mut self, down: i32) {
        let current = self.scroll_back.get().min(self.max_scroll.get()) as i64;
        let next = (current - i64::from(down)).clamp(0, self.max_scroll.get() as i64);
        self.scroll_back.set(next as usize);
    }

    fn submit(&mut self, text: String) -> Option<Effect> {
        let tiers = self.settings.tiers.clone();
        self.submit_to(text, tiers)
    }

    /// Sends `text` to `tiers` rather than the configured models, as `/claude` does.
    fn submit_to(&mut self, text: String, tiers: Vec<ModelId>) -> Option<Effect> {
        self.submit_with(text, tiers, None)
    }

    /// Sends a request, in a pair when `pair` says so.
    fn submit_with(
        &mut self,
        text: String,
        tiers: Vec<ModelId>,
        pair: Option<Pair>,
    ) -> Option<Effect> {
        if text.is_empty() {
            return None;
        }
        // A secret pasted with a log does not reach a model, nor the saved
        // conversation.
        let (text, secrets) = ironquill_tools::redact_secrets(&text);
        if !secrets.is_empty() {
            self.info(format!(
                "Took the value of {} out of the message",
                secrets.join(", ")
            ));
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
        for member in self.members() {
            builder = builder.member(member);
        }
        if let Some(budget) = self.settings.budget {
            builder = builder.budget(budget);
        }
        builder = builder
            .effort(Some(self.settings.effort))
            .detect_checks(self.settings.detect_checks);
        let allowed = self.settings.allowed_secrets.clone();
        if let Some(pair) = pair {
            builder = builder.pair(pair);
        }
        match builder.build() {
            Ok(config) => {
                let config = config
                    .with_allowed_secrets(allowed)
                    .with_allowed_hosts(self.settings.allowed_hosts.clone())
                    .with_strict_commands(self.settings.strict_commands);
                self.input.take();
                self.transcript.push(Entry::User(text.clone()));
                // A new request: the last sub-agent's work leaves the screen,
                // Ctrl-T brings it back.
                self.sub_view = None;
                self.step = None;
                self.request_spent = Spent::default();
                self.runs.clear();
                if self.focus == Focus::SubAgent {
                    self.focus = Focus::Chat;
                }
                self.running_since = Some(Instant::now());
                self.working_model = tiers.first().cloned();
                self.scroll_back.set(0);
                Some(Effect::Send { text, config })
            }
            Err(e) => {
                self.transcript.push(Entry::Error(e.to_string()));
                None
            }
        }
    }

    pub(crate) fn info(&mut self, text: impl Into<String>) {
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
            Command::Delegate(agent, None) => {
                self.error(format!(
                    "Give it a task: /{} <what to do>",
                    command_name(agent)
                ));
            }
            Command::Delegate(agent, Some(task)) => {
                let model = self.agent_model(agent);
                self.info(format!(
                    "Handed to {model}, in its own session, told what it missed of this conversation"
                ));
                return self.submit_to(task, vec![model]);
            }
            Command::Model(Some(id)) => match ModelId::new(id) {
                Ok(model) => {
                    self.use_model(model);
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
                let list = if !self.settings.checks.is_empty() {
                    self.settings
                        .checks
                        .iter()
                        .map(Check::command)
                        .collect::<Vec<_>>()
                        .join(", then ")
                } else if self.settings.detect_checks {
                    "the project's own, found when checking: pytest or unittest when \
                     there are Python tests, npm test, cargo test"
                        .to_owned()
                } else {
                    "none".to_owned()
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
                self.settings.detect_checks = false;
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
                self.scroll_back.set(0);
                self.expanded.clear();
                self.selected_reply = None;
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
            Command::Resume(id) => {
                if self.is_running() {
                    self.error("Still working on the last message: stop it with Ctrl-C first");
                    return None;
                }
                return Some(match id {
                    Some(id) => Effect::Resume(id),
                    None => Effect::ListSessions,
                });
            }
            Command::Usage(window) => match window.as_deref() {
                None => self.usage_open = !self.usage_open,
                Some(text) => match parse_window(text) {
                    Some(secs) => {
                        self.usage_window = secs;
                        self.usage_open = true;
                    }
                    None => self.error("/usage takes a window such as 1h, 6h or 24h"),
                },
            },
            Command::Hosts(None) => self.info(if self.settings.allowed_hosts.is_empty() {
                "No server allowed besides those the project and its tools already use: \
                 GitHub, AWS, your clusters, the package registries, the git remotes"
                    .to_owned()
            } else {
                format!(
                    "Commands may also reach: {}. /hosts forget <host> takes one back",
                    self.settings.allowed_hosts.join(", ")
                )
            }),
            Command::Hosts(Some(rest)) => match rest.strip_prefix("forget ") {
                Some(host) if self.settings.allowed_hosts.iter().any(|h| h == host.trim()) => {
                    let host = host.trim().to_owned();
                    self.settings.allowed_hosts.retain(|h| *h != host);
                    self.info(format!(
                        "{host} is no longer allowed: a command reaching it asks first"
                    ));
                }
                Some(host) => self.error(format!("{} is not allowed anyway", host.trim())),
                None => self.error("/hosts lists them; /hosts forget <host> takes one back"),
            },
            Command::Compact => {
                if self.is_running() {
                    self.error("Still working on the last message: compact after it");
                    return None;
                }
                let Some(config) = self.small_job_config() else {
                    self.error("Pick a model first (Ctrl-E)");
                    return None;
                };
                self.running_since = Some(Instant::now());
                self.info("Grouping the conversation by subject…");
                return Some(Effect::PlanCompaction(config));
            }
            Command::Tick => {
                self.settings.tick = !self.settings.tick;
                self.last_warm = None;
                self.idle_since = Instant::now();
                self.info(if self.settings.tick {
                    "Keeping Claude Code's warm sessions warm while the conversation waits: a one \
                     word read every four minutes, asking again after half an hour"
                } else {
                    "No longer keeping the sessions warm"
                });
            }
            Command::Strict(on) => {
                match on.as_deref() {
                    None => self.settings.strict_commands = !self.settings.strict_commands,
                    Some("on") => self.settings.strict_commands = true,
                    Some("off") => self.settings.strict_commands = false,
                    Some(other) => {
                        self.error(format!("/strict takes on or off, not {other}"));
                        return None;
                    }
                }
                self.info(if self.settings.strict_commands {
                    "Strict: a command running a program ironquill does not know asks first"
                } else {
                    "Not strict: what ironquill does not know runs, unless it looks dangerous"
                });
            }
            Command::Secrets(None) => self.info(if self.settings.allowed_secrets.is_empty() {
                "No secret of the environment is allowed: a command that names one asks first. \
                 Models never see their values"
                    .to_owned()
            } else {
                format!(
                    "Commands may use: {}. Models never see their values. /secrets forget <name> \
                     takes one back",
                    self.settings.allowed_secrets.join(", ")
                )
            }),
            Command::Secrets(Some(rest)) => match rest.strip_prefix("forget ") {
                Some(name)
                    if self
                        .settings
                        .allowed_secrets
                        .iter()
                        .any(|s| s == name.trim()) =>
                {
                    let name = name.trim().to_owned();
                    self.settings.allowed_secrets.retain(|s| *s != name);
                    self.info(format!(
                        "{name} is no longer allowed: a command naming it asks first"
                    ));
                }
                Some(name) => self.error(format!("{} is not allowed anyway", name.trim())),
                None => self.error("/secrets lists them; /secrets forget <name> takes one back"),
            },
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
            Command::Context => {
                if self.is_running() {
                    self.error("Still working on the last message: stop it with Ctrl-C first");
                    return None;
                }
                return Some(Effect::OpenContext);
            }
            Command::Reset(agent) => {
                if self.is_running() {
                    self.error("Still working on the last message: stop it with Ctrl-C first");
                    return None;
                }
                return Some(Effect::ForgetDelegate(agent));
            }
            Command::Keys => self.keys_open = Some(0),
            Command::Budget(None) => match self.settings.budget {
                Some(budget) => self.info(format!("Budget: {budget} per request")),
                None => self.info("No budget: requests may cost any amount"),
            },
            Command::Budget(Some(amount)) => {
                let amount = amount.trim_start_matches('$');
                if amount == "none" {
                    self.settings.budget = None;
                    self.info("No budget: requests may cost any amount");
                } else {
                    match amount.parse::<f64>() {
                        Ok(dollars) if dollars.is_finite() && dollars > 0.0 => {
                            self.settings.budget = Some(Usd(dollars));
                            self.info(format!("Budget: {} per request", Usd(dollars)));
                        }
                        _ => self.error(format!(
                            "A budget is an amount in dollars, such as /budget 0.25, or none; got {amount:?}"
                        )),
                    }
                }
            }
            Command::Defaults => return Some(Effect::SaveDefaults(self.defaults())),
            Command::Effort(None) => self.info(format!(
                "Effort: {}. /effort <level> changes it: low, medium, high, xhigh, max",
                self.settings.effort
            )),
            Command::Effort(Some(level)) => match level.parse() {
                Ok(effort) => self.set_effort(effort),
                Err(e) => self.error(e),
            },
            Command::Copy => self.open_transcript(),
            Command::Instructions => return Some(Effect::OpenInstructions),
            Command::Pair(None) | Command::NewPair(None) => {
                self.error("Give it a question: /pair <what to do>");
            }
            Command::Pair(Some(text)) => return self.pair(text, false),
            Command::NewPair(Some(text)) => return self.pair(text, true),
            Command::Planner(None) => match self.planner() {
                Some(planner) => self.info(format!(
                    "Planner: {planner}{}",
                    if self.settings.planner.is_some() {
                        ""
                    } else {
                        ", the best of the team; /planner <model> picks another"
                    }
                )),
                None => self.info("No planner: put a model in the team first (Ctrl-E, Space)"),
            },
            Command::Planner(Some(id)) => match ModelId::new(id) {
                Ok(model)
                    if self.settings.team.contains(&model)
                        || self.current_model() == Some(&model) =>
                {
                    self.info(format!("Planner: {model}"));
                    self.settings.planner = Some(model);
                }
                Ok(model) => self.error(format!(
                    "{model} is neither the model that answers nor in the team"
                )),
                Err(e) => self.error(e.to_string()),
            },
            Command::Team => {
                let text = self.describe_team();
                self.info(text);
            }
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
            AgentMessage::Compaction(Ok(compaction)) => {
                self.running_since = None;
                let kept = vec![true; compaction.exchanges.len()];
                if kept.is_empty() {
                    self.info("Nothing to compact yet");
                } else {
                    self.compact_picker = Some(CompactPicker {
                        compaction,
                        kept,
                        open: None,
                        cursor: 0,
                        last_as_is: true,
                    });
                }
                false
            }
            AgentMessage::Compaction(Err(e)) | AgentMessage::Compacted(Err(e)) => {
                self.running_since = None;
                self.error(e);
                false
            }
            AgentMessage::Compacted(Ok((before, after))) => {
                self.running_since = None;
                self.info(format!(
                    "Compacted: about {before} → {after} tokens. The agents' sessions start again \
                     from the summary. /context shows it"
                ));
                true
            }
            AgentMessage::NotSent(text) => {
                self.running_since = None;
                if matches!(self.transcript.last(), Some(Entry::User(t)) if *t == text) {
                    self.transcript.pop();
                }
                self.input.set(text);
                self.info(
                    "Not sent: compact the conversation first (/compact), then send it again",
                );
                false
            }
            AgentMessage::Approve(approval, answer) => {
                // One at a time: the agent waits for the answer.
                if let Some((_, earlier)) = self.approval.replace((approval, answer)) {
                    let _ = earlier.send(Answer::No);
                }
                false
            }
            AgentMessage::Done(Ok(outcome)) => {
                self.idle_since = Instant::now();
                self.last_warm = Some(Instant::now());
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
                    context: outcome.context,
                    runs: std::mem::take(&mut self.runs),
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
            usage_log: self.usage_log.clone(),
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
        self.usage_log = saved.usage_log;
        self.usage_log.prune(sessions::now());
        self.history = History::default();
        for entry in &self.transcript {
            if let Entry::User(text) = entry {
                self.history.push(text.clone());
            }
        }
        self.expanded.clear();
        self.selected_reply = None;
        self.transcript.push(Entry::Info(format!(
            "Resumed \"{}\" ({}): the conversation continues where it stopped",
            saved.name, self.session_id
        )));
        self.scroll_back.set(0);
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

    /// The models the picker offers, in a stable order.
    pub(crate) fn models(&self) -> Vec<ModelId> {
        self.settings.models.clone()
    }

    /// Makes `model` the one requests go to, and remembers it in the picker.
    fn use_model(&mut self, model: ModelId) {
        if !self.settings.models.contains(&model) {
            self.settings.models.push(model.clone());
        }
        if self.settings.tiers.is_empty() {
            self.settings.tiers.push(model);
        } else {
            self.settings.tiers[0] = model;
        }
    }

    /// How far the list of shortcuts is scrolled, while it is open.
    pub(crate) fn keys_open(&self) -> Option<usize> {
        self.keys_open
    }

    /// Keys while the list of shortcuts is open. Returns whether the key was
    /// for it. Ctrl-S itself goes through the keymap, to close it.
    fn keys_key(&mut self, key: KeyEvent) -> bool {
        let Some(offset) = self.keys_open else {
            return false;
        };
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            return false;
        }
        match key.code {
            KeyCode::Up => self.keys_open = Some(offset.saturating_sub(1)),
            KeyCode::Down => self.keys_open = Some(offset + 1),
            KeyCode::PageUp => self.keys_open = Some(offset.saturating_sub(10)),
            KeyCode::PageDown => self.keys_open = Some(offset + 10),
            _ => self.keys_open = None,
        }
        true
    }

    /// The models the first one may hand tasks to.
    pub(crate) fn team(&self) -> &[ModelId] {
        &self.settings.team
    }

    /// The handover the sub-agent pane shows: who, the task, what it did,
    /// and whether it is still at it.
    pub(crate) fn sub_agent(&self) -> Option<SubAgent<'_>> {
        let i = self.sub_view?;
        let Some(Entry::Delegating {
            from,
            to,
            task,
            spent,
        }) = self.transcript.get(i)
        else {
            return None;
        };
        let work: Vec<&Entry> = self.transcript[i + 1..]
            .iter()
            .map_while(|e| match e {
                Entry::Member { entry, .. } => Some(&**entry),
                _ => None,
            })
            .collect();
        let working = self.is_running()
            && self.member.as_ref() == Some(to)
            && i + 1 + work.len() == self.transcript.len();
        Some(SubAgent {
            from,
            to,
            task,
            spent: *spent,
            work,
            working,
        })
    }

    /// How far the sub-agent pane is scrolled up from its end, and the
    /// view's report of how far it can go.
    pub(crate) fn sub_scroll(&self) -> usize {
        self.sub_scroll.get()
    }

    pub(crate) fn set_sub_max(&self, max: usize) {
        self.sub_max.set(max);
        if self.sub_scroll.get() > max {
            self.sub_scroll.set(max);
        }
    }

    /// What the running request cost so far, its sub-agents apart.
    pub(crate) fn request_spent(&self) -> Spent {
        self.request_spent
    }

    /// Where the scores shown come from.
    pub(crate) fn credits(&self) -> Option<&str> {
        self.settings.credits.as_deref()
    }

    /// The step of a request in a pair being worked on, while it runs.
    pub(crate) fn step(&self) -> Option<&str> {
        self.step.as_deref().filter(|_| self.is_running())
    }

    /// The project's root directory.
    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    /// Whether the project's own checks are found when checking.
    pub(crate) fn detects_checks(&self) -> bool {
        self.settings.detect_checks
    }

    /// How hard models think before answering.
    pub(crate) fn effort(&self) -> Effort {
        self.settings.effort
    }

    /// Sets the effort and says so.
    fn set_effort(&mut self, effort: Effort) {
        self.settings.effort = effort;
        self.info(format!(
            "Effort: {effort}. Models that reason think {} before answering",
            match effort {
                Effort::Low => "briefly",
                Effort::Medium => "a while",
                Effort::High => "carefully",
                Effort::Xhigh => "longer",
                Effort::Max => "as long as they can",
            }
        ));
    }

    /// The most one request may cost.
    pub(crate) fn budget(&self) -> Option<Usd> {
        self.settings.budget
    }

    /// The model picker, while it is open.
    pub(crate) fn model_picker(&self) -> Option<&ModelPicker> {
        self.model_picker.as_ref()
    }

    /// The rows of the model picker: the offered models that match what was
    /// typed, then the provider's other models that do.
    pub(crate) fn model_rows(&self) -> Vec<ModelRow> {
        let filter = self
            .model_picker
            .as_ref()
            .map(|p| p.filter.to_lowercase())
            .unwrap_or_default();
        let matches = |model: &ModelId| model.as_str().to_lowercase().contains(&filter);
        let row = |model: &ModelId, offered: bool| ModelRow {
            model: model.clone(),
            note: self.note(model),
            offered,
            in_team: self.settings.team.contains(model),
        };
        let mut rows: Vec<ModelRow> = self
            .settings
            .models
            .iter()
            .filter(|m| matches(m))
            .map(|m| row(m, true))
            .collect();
        if !filter.is_empty() {
            rows.extend(
                self.settings
                    .catalog
                    .iter()
                    .map(|m| &m.model)
                    .filter(|m| matches(m) && !self.settings.models.contains(m))
                    .map(|m| row(m, false)),
            );
        }
        rows
    }

    /// A few words on `model`: its price and context, or the agent it is.
    pub(crate) fn note(&self, model: &ModelId) -> String {
        if let Some((agent, _)) = model.delegate() {
            return format!("{agent} · subscription");
        }
        self.settings
            .catalog
            .iter()
            .find(|m| &m.model == model)
            .map(|m| m.note.clone())
            .unwrap_or_default()
    }

    /// Who plans and who codes in a pair, among the model that answers and
    /// its team only. The planner comes first: the one picked with /planner,
    /// or the best, by Artificial Analysis' score, else by price. The coder
    /// is then the cheapest of the others that can use tools. Without one,
    /// the reason, rather than roles the wrong way round.
    /// Sends `text` to a pair; `fresh` has its planner start from the chat
    /// rather than go on from the last pair.
    fn pair(&mut self, text: String, fresh: bool) -> Option<Effect> {
        if self.settings.pair_mode {
            for model in ["claude-code/opus", "claude-code/sonnet"] {
                let model = ModelId::new(model).expect("a plain name");
                if !self.settings.team.contains(&model) {
                    self.settings.team.push(model);
                }
            }
        }
        let (coder, planner) = match self.pair_roles() {
            Ok(roles) => roles,
            Err(why) => {
                self.error(why);
                return None;
            }
        };
        // High at most for the planner: past it, a step can think for
        // minutes for little more. A coder thinking less rereads
        // everything and stops before writing.
        let planner_effort = self.settings.effort.min(Effort::High);
        let coder_effort = Effort::High;
        self.info(format!(
            "Pair: {planner} plans and reviews (effort {planner_effort}), {coder} codes (effort {coder_effort}){}",
            if fresh {
                ", the planner starting from the chat"
            } else {
                ""
            }
        ));
        let pair = Pair {
            planner,
            planner_effort,
            coder_effort,
            fresh,
        };
        self.submit_with(text, vec![coder], Some(pair))
    }

    pub(crate) fn pair_roles(&self) -> Result<(ModelId, ModelId), String> {
        // Pair mode: Claude Code's Opus plans and reviews, its Sonnet codes.
        if self.settings.pair_mode {
            let planner = ModelId::new("claude-code/opus").expect("a plain name");
            let coder = ModelId::new("claude-code/sonnet").expect("a plain name");
            return Ok((coder, planner));
        }
        let mut models: Vec<Member> = self.members();
        if let Some(lead) = self.current_model()
            && !models.iter().any(|m| &m.model == lead)
        {
            models.push(self.member_of(lead));
        }
        if models.len() < 2 {
            return Err(
                "Working in a pair needs two models: the one that answers and one in the team (Ctrl-E, select a model, Space)"
                    .into(),
            );
        }
        // An agent such as Claude Code or Codex has no price or score
        // here, its work being on a subscription; it is among the strongest
        // there are, so it plans before any model of the provider. Then
        // scores, when every model has one: otherwise a scored cheap model
        // would beat an unscored strong one. Then price.
        let scored = models
            .iter()
            .filter(|m| m.model.delegate().is_none())
            .all(|m| m.score.is_some());
        let best = |m: &Member| {
            let agent = if m.model.delegate().is_some() {
                1.0
            } else {
                0.0
            };
            let score = if scored { m.score.unwrap_or(-1.0) } else { 0.0 };
            (agent, score, m.price.unwrap_or(-1.0))
        };
        let chosen = self
            .settings
            .planner
            .as_ref()
            .filter(|chosen| models.iter().any(|m| &m.model == *chosen));
        // Nothing to tell them apart, the provider's list not read: better
        // ask than pick at random.
        let known = models
            .iter()
            .any(|m| m.price.is_some() || m.score.is_some() || m.model.delegate().is_some());
        if chosen.is_none() && !known {
            return Err(
                "The provider's prices are unknown, so the best model cannot be told: pick the one that plans with /planner <model>"
                    .into(),
            );
        }
        let planner = match chosen {
            Some(chosen) => chosen.clone(),
            None => models
                .iter()
                .max_by(|a, b| {
                    best(a)
                        .partial_cmp(&best(b))
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
                .map(|m| m.model.clone())
                .expect("two models at least"),
        };
        let price = |m: &Member| m.price.unwrap_or(f64::MAX);
        // The coder is a model of the provider when there is one: an agent
        // codes only when nothing else can.
        let coder = models
            .iter()
            .filter(|m| m.model != planner && m.tools)
            .min_by(|a, b| {
                let agent = |m: &Member| m.model.delegate().is_some();
                agent(a)
                    .cmp(&agent(b))
                    .then_with(|| price(a).total_cmp(&price(b)))
            });
        match coder {
            Some(coder) => Ok((coder.model.clone(), planner)),
            None => {
                let without: Vec<String> = models
                    .iter()
                    .filter(|m| m.model != planner && !m.tools)
                    .map(|m| m.model.to_string())
                    .collect();
                Err(format!(
                    "{planner} would plan, but no other model can code: {} cannot use tools, says the provider. Put a model that can in the team (Ctrl-E, select it, Space): \"no tools\" marks those that cannot",
                    without.join(", ")
                ))
            }
        }
    }

    /// The member of the team that plans in a pair, as /pair would pick it.
    pub(crate) fn planner(&self) -> Option<ModelId> {
        self.pair_roles().ok().map(|(_, planner)| planner)
    }

    /// What is known of `model`: from the provider's list, or its name.
    fn member_of(&self, model: &ModelId) -> Member {
        self.settings
            .catalog
            .iter()
            .find(|m| &m.model == model)
            .cloned()
            .unwrap_or_else(|| Member::new(model.clone(), self.note(model)))
    }

    /// The team as the agent gets it, with what tells its members apart.
    /// A configuration for work outside a request, such as compacting: the
    /// model that answers, the team, the budget.
    fn small_job_config(&self) -> Option<AgentConfig> {
        let mut builder = AgentConfig::builder().tier(self.current_model()?.clone());
        for member in self.members() {
            builder = builder.member(member);
        }
        if let Some(budget) = self.settings.budget {
            builder = builder.budget(budget);
        }
        builder.build().ok()
    }

    /// Keys while /compact asks what to keep. Returns `None` when the key
    /// was not for it.
    fn compact_key(&mut self, key: KeyEvent) -> Option<Option<Effect>> {
        let picker = self.compact_picker.as_mut()?;
        let rows = picker.rows();
        let row = rows.get(picker.cursor).copied();
        match key.code {
            KeyCode::Up => picker.cursor = picker.cursor.saturating_sub(1),
            KeyCode::Down => picker.cursor = (picker.cursor + 1).min(rows.len().saturating_sub(1)),
            KeyCode::Right => {
                if let Some(CompactRow::Subject(s)) = row {
                    picker.open = Some(s);
                }
            }
            KeyCode::Left => {
                if let Some(open) = picker.open.take() {
                    picker.cursor = open;
                }
            }
            KeyCode::Char(' ') => match row {
                Some(CompactRow::Subject(s)) => {
                    let exchanges = picker.compaction.subjects[s].exchanges.clone();
                    let on = !exchanges.iter().all(|e| picker.kept[*e]);
                    for e in exchanges {
                        picker.kept[e] = on;
                    }
                }
                Some(CompactRow::Exchange(e)) => picker.kept[e] = !picker.kept[e],
                None => {}
            },
            KeyCode::Char('l' | 'L') => picker.last_as_is = !picker.last_as_is,
            KeyCode::Esc => {
                self.compact_picker = None;
                self.info("Not compacted");
            }
            KeyCode::Enter => {
                let picker = self.compact_picker.take()?;
                let keep: Vec<usize> = (0..picker.kept.len()).filter(|e| picker.kept[*e]).collect();
                let config = self.small_job_config()?;
                self.running_since = Some(Instant::now());
                self.info("Compacting…");
                return Some(Some(Effect::Compact {
                    config,
                    keep,
                    last_as_is: picker.last_as_is,
                }));
            }
            _ => {}
        }
        Some(None)
    }

    /// The /compact window while it is open.
    pub(crate) fn compact_picker(&self) -> Option<&CompactPicker> {
        self.compact_picker.as_ref()
    }

    fn members(&self) -> Vec<Member> {
        self.settings
            .team
            .iter()
            .map(|model| {
                self.settings
                    .catalog
                    .iter()
                    .find(|m| &m.model == model)
                    .cloned()
                    .unwrap_or_else(|| Member::new(model.clone(), self.note(model)))
            })
            .collect()
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
        if self.models().is_empty() && self.settings.catalog.is_empty() {
            self.error("No model configured: start with --model, or set IRONQUILL_MODELS");
            return;
        }
        let selected = self
            .current_model()
            .and_then(|m| self.settings.models.iter().position(|x| x == m))
            .unwrap_or(0);
        self.model_picker = Some(ModelPicker {
            filter: String::new(),
            selected,
            effort_at_open: self.settings.effort,
        });
    }

    /// Keys while a held command waits for the person: `y` runs it, `n` or
    /// Esc refuses it. Returns whether the key was for it.
    /// Whether the warm sessions should be read now: `/tick` is on, no
    /// request runs, four minutes went by since the last time. After half an
    /// hour without a request, the person is asked first.
    pub(crate) fn keep_warm_due(&mut self) -> bool {
        if !self.settings.tick || self.is_running() || self.ask_keep_warm {
            return false;
        }
        if self.idle_since.elapsed() >= KEEP_WARM_ASK {
            self.ask_keep_warm = true;
            return false;
        }
        let due = self
            .last_warm
            .is_none_or(|t| t.elapsed() >= KEEP_WARM_EVERY);
        if due {
            self.last_warm = Some(Instant::now());
        }
        due
    }

    /// The question asked after a long wait, while it is.
    pub(crate) fn keep_warm_question(&self) -> Option<Approval> {
        self.ask_keep_warm.then(|| Approval {
            model: self
                .current_model()
                .cloned()
                .unwrap_or_else(|| ModelId::agent(Agent::ClaudeCode)),
            question: Question::KeepWarm {
                minutes: KEEP_WARM_ASK.as_secs() / 60,
            },
        })
    }

    fn approval_key(&mut self, key: KeyEvent) -> Option<Option<Effect>> {
        if self.ask_keep_warm {
            match key.code {
                // Another half hour.
                KeyCode::Char('y' | 'Y') => self.idle_since = Instant::now(),
                KeyCode::Char('n' | 'N') | KeyCode::Esc => {
                    self.settings.tick = false;
                    self.info("No longer keeping the sessions warm: /tick turns it on again");
                }
                _ => return Some(None),
            }
            self.ask_keep_warm = false;
            return Some(None);
        }
        let (approval, _) = self.approval.as_ref()?;
        let (secrets, hosts) = match &approval.question {
            Question::Command { secrets, hosts, .. } => (secrets.clone(), hosts.clone()),
            Question::MoreTurns { .. } | Question::KeepWarm { .. } | Question::ColdStart { .. } => {
                (Vec::new(), Vec::new())
            }
        };
        let answer = match key.code {
            KeyCode::Char('y' | 'Y') => Answer::Yes,
            // Stop, to compact first: only for a cold start.
            KeyCode::Char('c' | 'C') if matches!(approval.question, Question::ColdStart { .. }) => {
                Answer::Stop
            }
            // The secrets it names, from now on: kept with the defaults.
            KeyCode::Char('a' | 'A') if !secrets.is_empty() || !hosts.is_empty() => {
                for name in secrets {
                    if !self.settings.allowed_secrets.contains(&name) {
                        self.settings.allowed_secrets.push(name);
                    }
                }
                for host in hosts {
                    if !self.settings.allowed_hosts.contains(&host) {
                        self.settings.allowed_hosts.push(host);
                    }
                }
                Answer::Always
            }
            KeyCode::Char('n' | 'N') | KeyCode::Esc => Answer::No,
            // The held command, to look at elsewhere or run by hand.
            KeyCode::Char('c' | 'C') => {
                return Some(match &approval.question {
                    Question::Command { command, .. } => Some(Effect::Copy(command.clone())),
                    Question::MoreTurns { .. }
                    | Question::KeepWarm { .. }
                    | Question::ColdStart { .. } => None,
                });
            }
            _ => return Some(None),
        };
        if let Some((_, sender)) = self.approval.take() {
            // The agent may have been stopped meanwhile.
            let _ = sender.send(answer);
        }
        Some(None)
    }

    /// The usage samples, and the pane's window while it shows.
    pub(crate) fn usage_pane(&self) -> Option<(&UsageLog, u64)> {
        self.usage_open
            .then_some((&self.usage_log, self.usage_window))
    }

    /// The command waiting for the person's answer, if any.
    pub(crate) fn approval(&self) -> Option<&Approval> {
        self.approval.as_ref().map(|(approval, _)| approval)
    }

    /// Closes the model picker, saying the effort when ← → changed it there:
    /// a higher one costs more on every call after.
    fn close_model_picker(&mut self) {
        if let Some(picker) = self.model_picker.take()
            && picker.effort_at_open != self.settings.effort
        {
            self.set_effort(self.settings.effort);
        }
    }

    /// Keys while the model picker is open. Returns whether the key was for it.
    ///
    /// Typing searches the provider's models, Enter answers with the selected
    /// one, Space puts it in the team or takes it out.
    fn model_picker_key(&mut self, key: KeyEvent) -> bool {
        let Some(picker) = &self.model_picker else {
            return false;
        };
        let selected = picker.selected;
        let rows = self.model_rows();
        let last = rows.len().saturating_sub(1);
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let set = |app: &mut Self, selected: usize| {
            if let Some(p) = &mut app.model_picker {
                p.selected = selected;
            }
        };
        match key.code {
            KeyCode::Up => set(self, selected.saturating_sub(1)),
            KeyCode::Down => set(self, (selected + 1).min(last)),
            KeyCode::Enter => {
                self.close_model_picker();
                if let Some(row) = rows.get(selected) {
                    let model = row.model.clone();
                    self.use_model(model.clone());
                    if let Some((agent, _)) = model.delegate() {
                        self.info(format!(
                            "Model: {model}. {agent} works in its own session, kept for this conversation, and is told what it missed"
                        ));
                    } else {
                        self.info(format!("Model: {model}"));
                    }
                }
            }
            // Tab: the pair mode, Opus planning and Sonnet coding.
            KeyCode::Tab => {
                self.settings.pair_mode = !self.settings.pair_mode;
                self.info(if self.settings.pair_mode {
                    "Pair mode: /pair has Claude Code's Opus plan and review, its Sonnet code"
                } else {
                    "Pair mode off: /pair takes its models from the team"
                });
            }
            KeyCode::Char(' ') => {
                if let Some(row) = rows.get(selected) {
                    self.toggle_team(row.model.clone());
                }
            }
            // Delete takes a model off the list, and off the team: it is no
            // longer offered. Never the one that answers.
            KeyCode::Delete => {
                if let Some(row) = rows.get(selected).filter(|r| r.offered) {
                    let model = row.model.clone();
                    if self.current_model() == Some(&model) {
                        self.error(format!(
                            "{model} answers: pick another with Enter before taking it off the list"
                        ));
                    } else {
                        self.settings.models.retain(|m| m != &model);
                        self.settings.team.retain(|m| m != &model);
                        if self.settings.planner.as_ref() == Some(&model) {
                            self.settings.planner = None;
                        }
                        self.info(format!("{model} is off the list"));
                        let last = self.model_rows().len().saturating_sub(1);
                        set(self, selected.min(last));
                    }
                }
            }
            // The effort, shown at the top of the list, beside the search.
            KeyCode::Left => self.settings.effort = self.settings.effort.step(-1),
            KeyCode::Right => self.settings.effort = self.settings.effort.step(1),
            KeyCode::Esc => self.close_model_picker(),
            // Ctrl-E again closes it, as the shortcut that opened it.
            KeyCode::Char('e') if ctrl => self.close_model_picker(),
            KeyCode::Backspace => {
                if let Some(p) = &mut self.model_picker {
                    p.filter.pop();
                    p.selected = 0;
                }
            }
            KeyCode::Char(c) if !ctrl => {
                if let Some(p) = &mut self.model_picker {
                    p.filter.push(c);
                    p.selected = 0;
                }
            }
            _ => {}
        }
        true
    }

    /// Puts `model` in the team, or takes it out. A model found by the search
    /// is offered from then on.
    fn toggle_team(&mut self, model: ModelId) {
        if !self.settings.models.contains(&model) {
            self.settings.models.push(model.clone());
        }
        let lead = self
            .current_model()
            .map_or_else(|| "the model that answers".to_owned(), ToString::to_string);
        if let Some(i) = self.settings.team.iter().position(|m| m == &model) {
            self.settings.team.remove(i);
            self.info(format!("{model} leaves the team"));
        } else {
            self.settings.team.push(model.clone());
            let tools = self
                .settings
                .catalog
                .iter()
                .find(|m| m.model == model)
                .is_none_or(|m| m.tools);
            if tools {
                self.info(format!(
                    "{model} joins the team: {lead} answers you and may hand it tasks. /team shows the team"
                ));
            } else {
                self.error(format!(
                    "{model} joins the team, but the provider says it cannot use tools: it reads and changes no file, and only answers questions put to it in full"
                ));
            }
        }
    }

    /// Who answers and who it may hand tasks to, in words.
    fn describe_team(&self) -> String {
        let lead = self
            .current_model()
            .map_or_else(|| "no model".to_owned(), ToString::to_string);
        let members: Vec<String> = self
            .settings
            .team
            .iter()
            .filter(|m| Some(*m) != self.current_model())
            .map(|m| match self.note(m).as_str() {
                "" => format!("  {m}"),
                note => format!("  {m}  ({note})"),
            })
            .collect();
        if members.is_empty() {
            format!("{lead} answers you, alone. To give it a team: Ctrl-E, select a model, Space")
        } else {
            format!(
                "{lead} answers you, and may hand tasks to:\n{}\nIt decides when; you can also ask it to",
                members.join("\n")
            )
        }
    }

    /// The choices of this session, to keep as the defaults of the next.
    /// The agents found on this machine are offered anyway: only those
    /// picked are kept, so that another machine is not offered them.
    fn defaults(&self) -> Defaults {
        let ids = |models: &[ModelId]| models.iter().map(ToString::to_string).collect();
        let picked = |m: &&ModelId| {
            m.delegate().is_none()
                || self.settings.team.contains(m)
                || self.current_model() == Some(*m)
        };
        Defaults {
            model: self.current_model().map(ToString::to_string),
            models: self
                .settings
                .models
                .iter()
                .filter(picked)
                .map(ToString::to_string)
                .collect(),
            team: ids(&self.settings.team),
            budget: self.settings.budget.map(|b| b.0),
            effort: Some(self.settings.effort.to_string()),
            planner: self.settings.planner.as_ref().map(ToString::to_string),
            usage_window: Some(crate::usage::window_name(self.usage_window)),
            allowed_secrets: self.settings.allowed_secrets.clone(),
            lenient_commands: !self.settings.strict_commands,
            allowed_hosts: self.settings.allowed_hosts.clone(),
            pair_mode: Some(self.settings.pair_mode),
            tick: self.settings.tick,
        }
    }

    /// The choices, when they changed since they were last kept: the model,
    /// the models offered, the team and the budget carry over to the next
    /// session without having to ask.
    pub(crate) fn defaults_to_keep(&mut self) -> Option<Defaults> {
        let now = self.defaults();
        (now != self.kept).then(|| {
            self.kept = now.clone();
            now
        })
    }

    /// The model `/claude` or `/codex` hands tasks to: the first offered
    /// for that agent, or the agent's own default.
    fn agent_model(&self, agent: Agent) -> ModelId {
        self.models()
            .into_iter()
            .find(|m| m.delegate().is_some_and(|(a, _)| a == agent))
            .unwrap_or_else(|| ModelId::agent(agent))
    }

    /// Opens the conversation as text in the editor, to select and copy
    /// from. A file with unsaved edits stays open.
    fn open_transcript(&mut self) {
        if let Some(file) = self.file.as_mut().filter(|f| f.is_modified()) {
            file.refuse_close();
            self.unzoom();
            self.focus_on(Focus::File);
            return;
        }
        let text = self.transcript_text();
        let highlighter = Rc::clone(self.highlighter.get_or_init(|| Rc::new(Highlighter::new())));
        self.unzoom();
        self.file = Some(Editor::transcript(&text, highlighter));
        self.focus_on(Focus::File);
        self.info("The conversation as text: v or V selects, y copies to the clipboard, :q closes");
    }

    /// The conversation as plain text, as it reads on screen.
    pub(crate) fn transcript_text(&self) -> String {
        self.transcript
            .iter()
            .filter_map(Self::entry_text)
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    /// One entry as plain text, `None` for what is not part of the talk.
    fn entry_text(entry: &Entry) -> Option<String> {
        let text = match entry {
            Entry::Welcome | Entry::Cost { .. } => return None,
            Entry::Info(text)
            | Entry::Ended(text)
            | Entry::Refused(text)
            | Entry::Error(text)
            | Entry::Said(text) => text.clone(),
            Entry::User(text) => format!("> {text}"),
            Entry::Command {
                command,
                status,
                output,
                ..
            } => format!("$ {command}\n{status}\n{output}"),
            Entry::Interrupted => "■ Interrupted: the request stopped before it ended".into(),
            Entry::Tool {
                name,
                path,
                outcome,
            } => {
                let mut text = format!("● {name} {}", path.as_deref().unwrap_or_default());
                match outcome {
                    Ok(ToolSummary::Changed { diff, .. }) => {
                        for line in diff {
                            text.push('\n');
                            text.push_str(&match line {
                                DiffLine::Context(l) => format!("  {l}"),
                                DiffLine::Removed(l) => format!("- {l}"),
                                DiffLine::Added(l) => format!("+ {l}"),
                            });
                        }
                    }
                    Ok(ToolSummary::Ran { label, .. }) => text = format!("● {label}"),
                    Ok(_) => {}
                    Err(error) => text.push_str(&format!("  ✗ {error}")),
                }
                text
            }
            Entry::Checks(commands) => format!("▸ {}", commands.join(", then ")),
            Entry::Passed => "✓ checks passed".into(),
            Entry::Failed { command, excerpt } => format!("✗ {command}\n{excerpt}"),
            Entry::Escalating { from, to } => format!("↑ {from} → {to}"),
            Entry::GaveUp => "✗ the checks still fail after every model tried".into(),
            Entry::Delegating {
                from,
                to,
                task,
                spent,
            } => format!("→ {from} → {to}: {task} ({spent})"),
            Entry::Step {
                number,
                of,
                name,
                model,
                effort,
            } => format!(
                "━━ {} ━━",
                step_title(*number, *of, name, model.as_ref(), *effort)
            ),
            Entry::OverBudget { spent, budget } => {
                format!("✗ budget of {budget} spent ({spent})")
            }
            Entry::Member { model, entry } => {
                let inner = Self::entry_text(entry).unwrap_or_default();
                let mut text = format!("│ {model}");
                for line in inner.lines() {
                    text.push_str("\n│ ");
                    text.push_str(line);
                }
                text
            }
        };
        Some(text)
    }

    /// Opens the conversation's context in the editor, in place of a file.
    pub(crate) fn open_context(&mut self, text: &str) {
        if let Some(file) = self.file.as_mut().filter(|f| f.is_modified()) {
            if file.kind() == crate::editor::Kind::Context {
                file.say_pending_context();
            } else {
                file.refuse_close();
            }
            self.unzoom();
            self.focus_on(Focus::File);
            return;
        }
        let highlighter = Rc::clone(self.highlighter.get_or_init(|| Rc::new(Highlighter::new())));
        self.unzoom();
        self.file = Some(Editor::context(text, highlighter));
        self.focus_on(Focus::File);
    }

    /// Reports how applying an edited context went.
    pub(crate) fn on_context_applied(&mut self, result: Result<(u64, u64), String>) {
        match result {
            Ok((before, after)) => self.info(format!(
                "Context applied: about {} tokens, from {}. The next request is sent with it",
                TokenCount(after),
                TokenCount(before)
            )),
            Err(e) => {
                if let Some(file) = &mut self.file
                    && file.kind() == crate::editor::Kind::Context
                {
                    file.mark_modified();
                }
                self.error(format!("Context not applied: {e}"));
            }
        }
    }

    /// Tab while a command is being typed: after `:`, or after `/` in the
    /// message box. Returns whether Tab was used for that.
    fn complete_command(&mut self) -> bool {
        let models: Vec<String> = self.models().iter().map(ToString::to_string).collect();
        let (editor, slash) = match self.mode {
            Mode::Command => (&mut self.command, false),
            Mode::Insert if self.input.text().starts_with('/') => (&mut self.input, true),
            _ => return false,
        };
        let typed = if slash {
            editor.text()[1..].to_owned()
        } else {
            editor.text().to_owned()
        };
        if let Some(line) = command::complete(&typed, &mut self.completion, |l| {
            command::candidates(l, command::NAMES, &models)
        }) {
            editor.set(if slash { format!("/{line}") } else { line });
        }
        true
    }

    /// The candidates of a completion in progress, to show them.
    pub(crate) fn completions(&self) -> Option<&[String]> {
        self.completion.as_ref().map(|c| c.matches.as_slice())
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

    /// The conversation's name in the status line: only one given with
    /// /name. The first message, which names it in the /resume list, is
    /// not repeated there.
    pub(crate) fn session_label(&self) -> String {
        self.session_name.clone().unwrap_or_default()
    }

    pub(crate) fn report_info(&mut self, text: &str) {
        self.info(text);
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
                model,
                usage,
                cost,
                subscription,
                context,
                cache,
            } => {
                // A point for the usage pane; the conversation's context
                // only, not a member's.
                self.usage_log.push(Sample {
                    at: sessions::now(),
                    model: model.to_string(),
                    input: usage.input.0,
                    output: usage.output.0,
                    cost: cost.map(|c| c.0),
                    cache_read: cache.map(|c| c.read.0),
                    cache_written: cache.and_then(|c| c.written.map(|w| w.0)),
                    context: context.filter(|_| self.member.is_none()).map(|c| c.used.0),
                });
                // The model that answers is back: the member is done.
                if self.member.as_ref().is_some_and(|m| *m != model) {
                    self.member = None;
                    self.working_model = Some(model.clone());
                }
                // Who worked, in turn: a new run when the model changes.
                let run = match self.runs.last_mut() {
                    Some((last, spent)) if *last == model => spent,
                    _ => {
                        self.runs.push((model.clone(), Spent::default()));
                        &mut self.runs.last_mut().expect("just pushed").1
                    }
                };
                run.usage += usage;
                match cost {
                    Some(c) => run.cost += c,
                    None => run.subscription |= subscription,
                }
                if self.member.is_none() {
                    self.request_spent.usage += usage;
                    match cost {
                        Some(c) => self.request_spent.cost += c,
                        None => self.request_spent.subscription |= subscription,
                    }
                }
                // The member's turns count for its handover too, live.
                if self.member.is_some()
                    && let Some(Entry::Delegating { spent, .. }) = self
                        .transcript
                        .iter_mut()
                        .rev()
                        .find(|e| matches!(e, Entry::Delegating { .. }))
                {
                    spent.usage += usage;
                    match cost {
                        Some(c) => spent.cost += c,
                        None => spent.subscription |= subscription,
                    }
                }
                if context.is_some() {
                    self.context = context;
                }
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
                let last = match self.transcript.last_mut() {
                    Some(Entry::Member { entry, .. }) if self.member.is_some() => {
                        Some(&mut **entry)
                    }
                    Some(entry) if self.member.is_none() => Some(entry),
                    _ => None,
                };
                match last {
                    Some(Entry::Said(said)) if !new_block => said.push_str(&text),
                    _ => self.push_entry(Entry::Said(text)),
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
            Event::Delegating { from, to, task } => {
                self.transcript.push(Entry::Delegating {
                    from,
                    to: to.clone(),
                    task,
                    spent: Spent::default(),
                });
                // The sub-agent pane opens on it, the status line names it.
                self.sub_view = Some(self.transcript.len() - 1);
                self.sub_scroll.set(0);
                self.working_model = Some(to.clone());
                self.member = Some(to);
                return;
            }
            Event::Step {
                number,
                of,
                name,
                model,
                effort,
            } => {
                self.step = Some(name.clone());
                self.working_model = model.clone();
                Entry::Step {
                    number,
                    of,
                    name,
                    model,
                    effort,
                }
            }
            Event::Compacted {
                dropped,
                before,
                after,
            } => Entry::Info(format!(
                "Context compacted: {} dropped, about {before} → {after} tokens. /context shows what is left",
                if dropped == 1 {
                    "1 old tool result".to_owned()
                } else {
                    format!("{dropped} old tool results")
                }
            )),
            Event::OverBudget { spent, budget } => {
                self.member = None;
                Entry::OverBudget { spent, budget }
            }
            Event::Tried { command, outcome } => {
                // The first lines say it; the rest is for the planner.
                let short: Vec<&str> = outcome.lines().take(3).collect();
                Entry::Info(format!(
                    "Before any change, `{command}` {}",
                    short.join(" ")
                ))
            }
            Event::PairEnded { text } => Entry::Ended(text),
            Event::Command {
                model,
                command,
                status,
                output,
                checked_by,
            } => Entry::Command {
                model,
                command,
                status,
                output,
                checked_by,
            },
            Event::Silent { model } => Entry::Error(format!(
                "{model} answered nothing, twice: the request ends without a reply"
            )),
            Event::Progress { by, text, .. } => Entry::Said(format!(
                "Where the work stands, summed up by {by}:\n\n{text}"
            )),
            Event::OutOfTurns { model, turns } => {
                Entry::Info(format!("{model} used its {turns} turns"))
            }
            Event::Restarted {
                model,
                idle_secs,
                before,
                after,
            } => Entry::Info(format!(
                "{model}'s prompt cache had expired ({} unused): the conversation goes on from \
                 its summary and the latest exchanges, about {before} → {after} tokens. /context \
                 shows it",
                sessions::ago(0, idle_secs).trim_end_matches(" ago")
            )),
            Event::Notice { model, text } => Entry::Info(format!("{model}: {text}")),
            Event::Denied {
                model,
                action,
                reason,
            } => Entry::Refused(format!(
                "{model}'s safety checks refused: {action}{}. To allow it, say so in your next message",
                if reason.is_empty() {
                    String::new()
                } else {
                    format!(" ({})", first_sentence(&reason))
                }
            )),
            Event::Held {
                command,
                reasons,
                approved,
            } => {
                if approved {
                    Entry::Info(format!("Approved: {command}"))
                } else {
                    Entry::Refused(format!("Refused: {command} ({})", reasons.join("; ")))
                }
            }
        };
        self.push_entry(entry);
    }

    /// Adds an entry, set apart under the member's name while one works.
    fn push_entry(&mut self, entry: Entry) {
        let entry = match &self.member {
            Some(model) => Entry::Member {
                model: model.clone(),
                entry: Box::new(entry),
            },
            None => entry,
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
        // Its answer would reach nobody.
        self.approval = None;
        self.transcript.push(Entry::Interrupted);
        self.info("Files already edited stay edited: /diff shows them");
    }

    /// Text pasted in one piece: into the message box whole, line breaks
    /// kept, so that a pasted log is not sent at its first line; on the
    /// command line, on one line.
    pub(crate) fn on_paste(&mut self, text: &str) {
        if self.approval.is_some()
            || self.picker.is_some()
            || self.model_picker.is_some()
            || self.focus == Focus::File
        {
            return;
        }
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        match self.mode {
            Mode::Insert => {
                self.focus = Focus::Chat;
                for c in text.chars() {
                    self.input.insert(c);
                }
            }
            Mode::Command => {
                for c in text.chars() {
                    self.command.insert(if c == '\n' { ' ' } else { c });
                }
            }
            Mode::Normal => {}
        }
    }

    pub(crate) fn on_diff(&mut self, text: &str) {
        self.info(text);
    }

    pub(crate) fn on_tick(&mut self) {
        self.spinner = self.spinner.wrapping_add(1);
    }
}

/// A window such as `90m`, `6h` or `1d`, in seconds, a day at most.
pub fn parse_window(text: &str) -> Option<u64> {
    let text = text.trim();
    let (number, unit) = text.split_at(text.find(|c: char| !c.is_ascii_digit())?);
    let number: u64 = number.parse().ok().filter(|n| *n > 0)?;
    let secs = match unit {
        "m" | "min" => number * 60,
        "h" => number * 60 * 60,
        "d" => number * 24 * 60 * 60,
        _ => return None,
    };
    Some(secs.min(crate::usage::KEEP_SECS))
}

/// How often the warm sessions are read while the conversation waits.
const KEEP_WARM_EVERY: Duration = Duration::from_secs(4 * 60);

/// How long the conversation may wait before the person is asked whether
/// to keep reading them.
const KEEP_WARM_ASK: Duration = Duration::from_secs(30 * 60);

/// The first sentence of `text`, for a line: an agent's reasons run long.
fn first_sentence(text: &str) -> &str {
    let text = text.trim();
    text.find(". ").map_or(text, |end| &text[..end])
}

/// `3/4 Planning · tensorx/glm-5.3 · effort high`, or `by ironquill`.
pub(crate) fn step_title(
    number: u8,
    of: u8,
    name: &str,
    model: Option<&ModelId>,
    effort: Option<Effort>,
) -> String {
    let who = match (model, effort) {
        (Some(model), Some(effort)) => format!("{model} · effort {effort}"),
        (Some(model), None) => model.to_string(),
        (None, _) => "by ironquill, no model".to_owned(),
    };
    // A step outside the numbered ones, such as the summary at the end.
    if of == 0 {
        return format!("{name} · {who}");
    }
    format!("{number}/{of} {name} · {who}")
}

/// The command that hands a task to `agent`.
fn command_name(agent: Agent) -> &'static str {
    match agent {
        Agent::ClaudeCode => "claude",
        Agent::Codex => "codex",
    }
}

#[cfg(test)]
mod tests {
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
                ..Settings::default()
            },
            PathBuf::from("/p"),
        )
    }

    #[test]
    fn the_usage_pane_shows_what_each_model_used() {
        use ironquill_core::CacheUse;
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
        let (log, window) = app.usage_pane().unwrap();
        assert_eq!(window, 6 * 3600);
        assert_eq!(log.samples.len(), 3);

        let backend = ratatui::backend::TestBackend::new(140, 40);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| crate::view::render(frame, &app))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let screen: String = buffer
            .content()
            .chunks(usize::from(buffer.area.width))
            .map(|row| row.iter().map(|c| c.symbol()).collect::<String>() + "\n")
            .collect();
        assert!(screen.contains("Usage · last 6h"));
        assert!(screen.contains("$0.020 · 2 calls · cache 96%"), "{screen}");
        assert!(screen.contains("$0.200 · 1 call · cache 10% · 1 rebuilt"));

        // Kept with the conversation.
        app.transcript.push(Entry::User("hi".into()));
        let saved = app.to_saved(Session::new()).unwrap();
        assert_eq!(saved.usage_log.samples.len(), 3);
        type_text(&mut app, "/usage");
        press(&mut app, KeyCode::Enter);
        assert!(app.usage_pane().is_none());
    }

    fn screen(app: &App) -> String {
        let backend = ratatui::backend::TestBackend::new(120, 40);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| crate::view::render(frame, app))
            .unwrap();
        let buffer = terminal.backend().buffer();
        buffer
            .content()
            .chunks(usize::from(buffer.area.width))
            .map(|row| row.iter().map(|c| c.symbol()).collect::<String>() + "\n")
            .collect()
    }

    #[test]
    fn a_pasted_log_is_typed_whole_and_not_sent() {
        let mut app = ready();
        app.on_paste("error: one\r\nerror: two\n");
        assert_eq!(app.input().text(), "error: one\nerror: two\n");
        assert!(screen(&app).contains("error: one↵error: two↵"));
        // Sent as it was pasted.
        type_text(&mut app, "fix it");
        assert!(matches!(
            press(&mut app, KeyCode::Enter),
            Some(Effect::Send { text, .. }) if text == "error: one\nerror: two\nfix it"
        ));

        // On the command line, on one line.
        let mut app = ready();
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Char(':'));
        app.on_paste("usage\n6h");
        assert_eq!(app.command.text(), "usage 6h");
        // Not while a question waits.
        let mut app = ready();
        let (answer, _answered) = oneshot::channel();
        app.on_agent(AgentMessage::Approve(
            Approval {
                model: ModelId::new("cheap").unwrap(),
                question: Question::MoreTurns { turns: 30 },
            },
            answer,
        ));
        app.on_paste("y");
        assert_eq!(app.input().text(), "");
        assert!(screen(&app).contains(" Go on? "));
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
    fn ctrl_o_and_ctrl_p_show_the_usage_pane_and_its_window() {
        let mut app = ready();
        let ctrl = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL);
        app.on_key(ctrl('o'));
        assert_eq!(app.usage_pane().map(|(_, w)| w), Some(3600));
        app.on_key(ctrl('o'));
        assert!(app.usage_pane().is_none());
        // The next window, shown.
        app.on_key(ctrl('p'));
        assert_eq!(app.usage_pane().map(|(_, w)| w), Some(6 * 3600));
        app.on_key(ctrl('p'));
        app.on_key(ctrl('p'));
        assert_eq!(app.usage_pane().map(|(_, w)| w), Some(3600));
        app.on_key(ctrl('p'));
        // Kept for the next session.
        assert_eq!(app.defaults().usage_window.as_deref(), Some("6h"));
    }

    #[test]
    fn a_stopped_request_says_so() {
        let mut app = ready();
        app.on_cancelled();
        assert!(app.transcript.contains(&Entry::Interrupted));
        assert!(screen(&app).contains("Interrupted: the request stopped before it ended"));
    }

    #[test]
    fn a_secret_may_be_allowed_for_good() {
        let mut app = ready();
        let (answer, mut answered) = oneshot::channel();
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
        press(&mut app, KeyCode::Char('a'));
        assert_eq!(answered.try_recv(), Ok(Answer::Always));
        assert_eq!(app.defaults().allowed_secrets, ["API_TOKEN"]);
        type_text(&mut app, "/secrets forget API_TOKEN");
        press(&mut app, KeyCode::Enter);
        assert!(app.defaults().allowed_secrets.is_empty());
    }

    #[test]
    fn pair_mode_has_opus_plan_and_sonnet_code() {
        let mut app = ready();
        app.settings.pair_mode = true;
        type_text(&mut app, "/newpair make done");
        let Some(Effect::Send { config, .. }) = press(&mut app, KeyCode::Enter) else {
            panic!("a pair is sent");
        };
        assert!(format!("{config:?}").contains("claude-code/opus"));
        assert!(
            app.settings
                .team
                .contains(&ModelId::new("claude-code/sonnet").unwrap())
        );
        // Tab in the model picker turns it off.
        app.on_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL));
        press(&mut app, KeyCode::Tab);
        assert!(!app.settings.pair_mode);
    }

    #[test]
    fn tick_keeps_sessions_warm_and_asks_after_a_long_wait() {
        let mut app = ready();
        assert!(!app.keep_warm_due());
        type_text(&mut app, "/tick");
        press(&mut app, KeyCode::Enter);
        assert!(app.keep_warm_due());
        // Not again before four minutes.
        assert!(!app.keep_warm_due());
        app.last_warm = Instant::now().checked_sub(Duration::from_secs(5 * 60));
        assert!(app.keep_warm_due());
        // Half an hour without a request: asked first.
        app.idle_since = Instant::now()
            .checked_sub(Duration::from_secs(31 * 60))
            .unwrap();
        assert!(!app.keep_warm_due());
        assert!(screen(&app).contains("Keep the sessions warm?"));
        press(&mut app, KeyCode::Char('n'));
        assert!(!app.settings.tick);
        assert!(app.keep_warm_question().is_none());
        assert!(!app.defaults().tick);
    }

    #[test]
    fn a_request_not_sent_comes_back_to_the_box() {
        let mut app = ready();
        type_text(&mut app, "fix it");
        press(&mut app, KeyCode::Enter);
        app.on_agent(AgentMessage::NotSent("fix it".into()));
        assert_eq!(app.input().text(), "fix it");
        assert!(!app.is_running());
        assert!(!app.transcript.contains(&Entry::User("fix it".into())));
    }

    #[test]
    fn a_code_block_copies_with_a_click_and_only_its_mark_folds() {
        use ratatui::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
        let mut app = ready();
        let long: String = (0..20).map(|i| format!("line {i}\n")).collect();
        app.transcript.push(Entry::Said(format!(
            "Try:\n```sh\ncargo test -q\n```\n{long}"
        )));
        app.transcript.push(Entry::Said(long.clone()));
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
    fn compact_lets_the_person_pick_what_to_keep() {
        use ironquill_agent::Subject;
        let mut app = ready();
        type_text(&mut app, "/compact");
        assert!(matches!(
            press(&mut app, KeyCode::Enter),
            Some(Effect::PlanCompaction(_))
        ));
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
        // Open the parser, untick its test.
        press(&mut app, KeyCode::Right);
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Char(' '));
        assert!(screen(&app).contains("[-] Parser (2 exchanges)"));
        // The docs go as well; the last exchange is not kept as it was.
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Char(' '));
        press(&mut app, KeyCode::Char('l'));
        assert!(matches!(
            press(&mut app, KeyCode::Enter),
            Some(Effect::Compact { keep, last_as_is: false, .. }) if keep == [0]
        ));
        assert!(app.compact_picker().is_none());
    }

    #[test]
    fn a_held_command_waits_for_yes_or_no() {
        let mut app = ready();
        let approval = || Approval {
            model: ModelId::new("cheap").unwrap(),
            question: Question::Command {
                command: "git push origin main".into(),
                reasons: vec!["it sends commits to another repository".into()],
                secrets: vec![],
                hosts: vec![],
            },
        };
        let (answer, mut answered) = oneshot::channel();
        app.on_agent(AgentMessage::Approve(approval(), answer));
        assert!(app.approval().is_some());
        // c copies the command, and the window stays.
        assert!(matches!(
            press(&mut app, KeyCode::Char('c')),
            Some(Effect::Copy(text)) if text == "git push origin main"
        ));
        assert!(app.approval().is_some());
        // Other keys wait for the answer, and type nothing.
        press(&mut app, KeyCode::Char('x'));
        assert!(app.approval().is_some());
        assert_eq!(app.input().text(), "");
        press(&mut app, KeyCode::Char('y'));
        assert!(app.approval().is_none());
        assert_eq!(answered.try_recv(), Ok(Answer::Yes));

        let (answer, mut answered) = oneshot::channel();
        app.on_agent(AgentMessage::Approve(approval(), answer));
        press(&mut app, KeyCode::Esc);
        assert_eq!(answered.try_recv(), Ok(Answer::No));
    }

    #[test]
    fn up_and_down_go_through_the_messages_sent_before() {
        let mut app = ready();
        type_text(&mut app, "fix it");
        press(&mut app, KeyCode::Enter);
        for _ in 0..2 {
            type_text(&mut app, "/cost");
            press(&mut app, KeyCode::Enter);
        }
        type_text(&mut app, "draft");
        let mut recall = |code| {
            press(&mut app, code);
            app.input.text().to_owned()
        };
        // Sent twice in a row, it is there once.
        assert_eq!(recall(KeyCode::Up), "/cost");
        assert_eq!(recall(KeyCode::Up), "fix it");
        assert_eq!(recall(KeyCode::Up), "fix it");
        assert_eq!(recall(KeyCode::Down), "/cost");
        assert_eq!(recall(KeyCode::Down), "draft");
        assert_eq!(recall(KeyCode::Down), "draft");
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
                context: None,
                cache: None,
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
            context: None,
            cache: None,
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
            context: None,
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
        // Its messages can be sent again with Up.
        press(&mut other, KeyCode::Up);
        assert_eq!(other.input.text(), "fix the parser please");
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
                model: ModelId::agent(Agent::ClaudeCode),
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
        assert_eq!(app.model_picker().map(|p| p.selected), Some(0));
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Enter);
        assert!(app.model_picker().is_none());
        assert_eq!(app.current_model().unwrap().as_str(), "claude-code/opus");
    }

    #[test]
    fn picking_another_model_keeps_the_first_one_in_the_list() {
        let mut app = App::new(
            Settings {
                tiers: vec![ModelId::new("deepseek/deepseek-chat").unwrap()],
                models: vec![ModelId::new("claude-code/opus").unwrap()],
                ..Settings::default()
            },
            PathBuf::from("/p"),
        );
        let ctrl_e = KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL);
        app.on_key(ctrl_e);
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.current_model().unwrap().as_str(), "claude-code/opus");

        // Back to the first one: it is still offered, at the same place.
        app.on_key(ctrl_e);
        assert_eq!(app.model_picker().map(|p| p.selected), Some(1));
        press(&mut app, KeyCode::Up);
        press(&mut app, KeyCode::Enter);
        assert_eq!(
            app.current_model().unwrap().as_str(),
            "deepseek/deepseek-chat"
        );
        assert_eq!(app.models().len(), 2);
    }

    #[test]
    fn the_picker_searches_the_catalog_and_builds_the_team() {
        let member = |id: &str, note: &str| Member::new(ModelId::new(id).unwrap(), note);
        let mut app = App::new(
            Settings {
                tiers: vec![ModelId::new("cheap").unwrap()],
                models: vec![ModelId::new("cheap").unwrap()],
                catalog: vec![
                    member("cheap", "$0.14 / $0.28 per M tokens"),
                    member("anthropic/claude-sonnet", "$3.00 / $15.00 per M tokens"),
                    member("openai/gpt-mini", "$0.25 / $2.00 per M tokens"),
                ],
                budget: Some(Usd(0.10)),
                ..Settings::default()
            },
            PathBuf::from("/p"),
        );
        app.on_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL));
        // Only the offered models until something is typed.
        assert_eq!(app.model_rows().len(), 1);
        for c in "sonn".chars() {
            press(&mut app, KeyCode::Char(c));
        }
        let rows = app.model_rows();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].model.as_str(), "anthropic/claude-sonnet");
        assert!(!rows[0].offered);
        assert_eq!(rows[0].note, "$3.00 / $15.00 per M tokens");

        // Space puts it in the team, and offers it from then on.
        press(&mut app, KeyCode::Char(' '));
        assert!(app.model_rows()[0].in_team);
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.team().len(), 1);
        assert_eq!(app.models().len(), 2);
        assert_eq!(app.current_model().unwrap().as_str(), "cheap");

        let members = app.members();
        assert_eq!(members[0].note, "$3.00 / $15.00 per M tokens");

        type_text(&mut app, "/budget 0.25");
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.budget(), Some(Usd(0.25)));
        type_text(&mut app, "/budget lots");
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.budget(), Some(Usd(0.25)));

        type_text(&mut app, "/defaults");
        let effect = app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        // Every change is handed over once, to be kept.
        assert!(app.defaults_to_keep().is_some());
        assert_eq!(app.defaults_to_keep(), None);
        let Some(Effect::SaveDefaults(defaults)) = effect else {
            panic!("expected the defaults to be saved, got {effect:?}");
        };
        assert_eq!(
            defaults,
            Defaults {
                model: Some("cheap".into()),
                models: vec!["cheap".into(), "anthropic/claude-sonnet".into()],
                team: vec!["anthropic/claude-sonnet".into()],
                budget: Some(0.25),
                effort: Some("high".into()),
                planner: None,
                usage_window: Some("1h".into()),
                allowed_secrets: vec![],
                lenient_commands: true,
                allowed_hosts: vec![],
                pair_mode: Some(false),
                tick: false,
            }
        );
    }

    #[test]
    fn copy_opens_the_conversation_as_text_to_select_from() {
        let mut app = ready();
        app.transcript.push(Entry::User("fix it".into()));
        app.transcript
            .push(Entry::Said("Here:\n```py\nprint(1)\n```".into()));
        app.transcript.push(Entry::Tool {
            name: "replace".into(),
            path: Some("a.py".into()),
            outcome: Ok(ToolSummary::Changed {
                path: "a.py".into(),
                created: false,
                diff: vec![
                    DiffLine::Removed("x = 1".into()),
                    DiffLine::Added("x = 2".into()),
                ],
            }),
        });
        let text = app.transcript_text();
        assert!(text.contains("> fix it\n\nHere:\n```py\nprint(1)\n```"));
        assert!(text.ends_with("● replace a.py\n- x = 1\n+ x = 2"));

        type_text(&mut app, "/copy");
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.focus(), Focus::File);
        assert!(
            app.file()
                .is_some_and(|f| f.lines().iter().any(|l| l == "print(1)"))
        );
    }

    #[test]
    fn a_members_work_is_set_apart_until_the_lead_is_back() {
        let mut app = ready();
        let id = |s: &str| ModelId::new(s).unwrap();
        let turn = |model: &str| {
            AgentMessage::Event(Event::Turn {
                model: id(model),
                usage: Usage::default(),
                cost: None,
                subscription: false,
                context: None,
                cache: None,
            })
        };
        app.on_agent(AgentMessage::Event(Event::Delegating {
            from: id("cheap"),
            to: id("strong"),
            task: "read a.py".into(),
        }));
        app.on_agent(turn("strong"));
        app.on_agent(AgentMessage::Event(Event::Turn {
            model: id("strong"),
            usage: Usage {
                input: TokenCount(12_000),
                output: TokenCount(300),
            },
            cost: Some(Usd(0.012)),
            subscription: false,
            context: None,
            cache: None,
        }));
        app.on_agent(AgentMessage::Event(Event::Said {
            model: id("strong"),
            text: "a.py prints 1.".into(),
        }));
        app.on_agent(turn("cheap"));
        app.on_agent(AgentMessage::Event(Event::Said {
            model: id("cheap"),
            text: "Done.".into(),
        }));
        let n = app.transcript.len();
        assert!(matches!(
            &app.transcript[n - 2],
            Entry::Member { model, entry } if model.as_str() == "strong" && **entry == Entry::Said("a.py prints 1.".into())
        ));
        assert_eq!(app.transcript[n - 1], Entry::Said("Done.".into()));
        assert!(app.transcript_text().contains("│ strong\n│ a.py prints 1."));

        // The sub-agent pane shows the handover and its work, Ctrl-T hides
        // it and brings it back.
        let sub = app.sub_agent().unwrap();
        assert_eq!(sub.to.as_str(), "strong");
        assert_eq!(sub.task, "read a.py");
        // Its cost, counted apart; the answering model's turns are not in it.
        assert_eq!(sub.spent.cost, Usd(0.012));
        assert_eq!(sub.spent.to_string(), "$0.012 · 12.0k in · 300 out");
        assert_eq!(sub.work, [&Entry::Said("a.py prints 1.".into())]);
        // Only the answering model's turns count for the request's own cost.
        assert_eq!(app.request_spent().cost, Usd(0.0));

        // Tab reaches the pane; Up scrolls it back, End follows the work again.
        app.set_sub_max(10);
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.focus(), Focus::SubAgent);
        press(&mut app, KeyCode::Up);
        press(&mut app, KeyCode::Up);
        assert_eq!(app.sub_scroll(), 2);
        press(&mut app, KeyCode::End);
        assert_eq!(app.sub_scroll(), 0);
        press(&mut app, KeyCode::Home);
        assert_eq!(app.sub_scroll(), 10);
        let ctrl_t = KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL);
        app.on_key(ctrl_t);
        assert!(app.sub_agent().is_none());
        app.on_key(ctrl_t);
        assert!(app.sub_agent().is_some());
    }

    #[test]
    fn the_effort_is_high_and_easy_to_change() {
        let mut app = ready();
        assert_eq!(app.effort(), Effort::High);

        // Alone, /effort only shows it.
        type_text(&mut app, "/effort");
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.effort(), Effort::High);
        assert!(
            matches!(app.transcript.last(), Some(Entry::Info(t)) if t.starts_with("Effort: high."))
        );
        type_text(&mut app, "/effort low");
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.effort(), Effort::Low);
        type_text(&mut app, "/effort extreme");
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.effort(), Effort::Low);

        // Left and right in the model picker.
        app.on_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL));
        press(&mut app, KeyCode::Right);
        press(&mut app, KeyCode::Right);
        assert_eq!(app.effort(), Effort::High);
        press(&mut app, KeyCode::Left);
        assert_eq!(app.effort(), Effort::Medium);
        press(&mut app, KeyCode::Esc);
        // Closing the picker says what it changed.
        assert!(
            matches!(app.transcript.last(), Some(Entry::Info(t)) if t.starts_with("Effort: medium."))
        );

        // Kept with the other choices.
        assert_eq!(app.defaults().effort.as_deref(), Some("medium"));
    }

    #[test]
    fn a_pair_codes_with_the_cheapest_and_plans_with_the_best() {
        let scored = |id: &str, score: Option<f64>, price: f64| Member {
            score,
            price: Some(price),
            ..Member::new(ModelId::new(id).unwrap(), "")
        };
        // As in a real run: the model that answers is the dearest.
        let mut app = App::new(
            Settings {
                tiers: vec![ModelId::new("smart").unwrap()],
                catalog: vec![
                    scored("cheap", Some(40.0), 1e-7),
                    scored("smart", Some(55.0), 2e-6),
                    scored("dear", Some(50.0), 9e-6),
                ],
                rounds: 2,
                max_turns: 30,
                ..Settings::default()
            },
            PathBuf::from("/p"),
        );
        // Alone: nobody to pair with.
        type_text(&mut app, "/pair add a feature");
        assert!(
            app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
                .is_none()
        );
        assert!(matches!(app.transcript.last(), Some(Entry::Error(e)) if e.contains("two models")));

        // The cheapest codes, the best scored plans, whoever answers.
        app.settings.team = ["cheap", "dear"]
            .iter()
            .map(|m| ModelId::new(*m).unwrap())
            .collect();
        let id = |s: &str| ModelId::new(s).unwrap();
        assert_eq!(app.pair_roles(), Ok((id("cheap"), id("smart"))));
        // Without a score for every one, the dearest plans.
        app.settings.catalog[1].score = None;
        assert_eq!(app.pair_roles(), Ok((id("cheap"), id("dear"))));
        // Picked by hand, among the model that answers and the team only.
        type_text(&mut app, "/planner outsider");
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.planner(), Some(id("dear")));
        type_text(&mut app, "/planner smart");
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.pair_roles(), Ok((id("cheap"), id("smart"))));

        // Without any price known, it asks rather than guesses.
        let catalog = std::mem::take(&mut app.settings.catalog);
        app.settings.planner = None;
        assert!(app.pair_roles().unwrap_err().contains("/planner"));
        app.settings.catalog = catalog;

        // Claude Code plans before any model of the provider, though it has
        // no price here; a model of the provider codes.
        app.settings.team = vec![id("claude-code/opus"), id("cheap")];
        app.settings.planner = None;
        assert_eq!(app.pair_roles(), Ok((id("cheap"), id("claude-code/opus"))));
        app.settings.team = vec![id("cheap"), id("dear")];
        app.settings.planner = Some(id("smart"));

        // A real case: the only cheaper model cannot use tools. The best
        // still plans, and ironquill says why nobody can code rather than
        // swapping the roles.
        let no_tools = Member {
            tools: false,
            ..scored("flash", None, 2e-7)
        };
        app.settings.catalog.push(no_tools);
        app.settings.planner = None;
        app.settings.team = vec![id("flash")];
        let why = app.pair_roles().unwrap_err();
        assert!(
            why.contains("smart would plan") && why.contains("flash cannot use tools"),
            "{why}"
        );
        app.settings.team = vec![id("cheap"), id("dear")];
        app.settings.planner = Some(id("smart"));

        type_text(&mut app, "/pair add a feature");
        let effect = app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(effect, Some(Effect::Send { ref text, .. }) if text == "add a feature"));
        assert!(app.transcript.iter().any(|e| matches!(
            e,
            Entry::Info(t) if t.contains("smart plans and reviews (effort high)")
                && t.contains("cheap codes (effort high)")
        )));
        // The model that answers stays the one picked.
        assert_eq!(app.current_model(), Some(&id("smart")));
    }

    #[test]
    fn the_cost_line_shows_the_models_in_turn() {
        let mut app = ready();
        let id = |s: &str| ModelId::new(s).unwrap();
        let turn = |model: &str, cost: f64| {
            AgentMessage::Event(Event::Turn {
                model: id(model),
                usage: Usage {
                    input: TokenCount(1_000),
                    output: TokenCount(10),
                },
                cost: Some(Usd(cost)),
                subscription: false,
                context: None,
                cache: None,
            })
        };
        type_text(&mut app, "do it");
        press(&mut app, KeyCode::Enter);
        for (model, cost) in [
            ("strong", 0.002),
            ("strong", 0.004),
            ("cheap", 0.01),
            ("strong", 0.001),
        ] {
            app.on_agent(turn(model, cost));
        }
        app.on_agent(AgentMessage::Done(Ok(Outcome {
            verdict: Verdict::Answered,
            usage: Usage::default(),
            cost: Usd(0.017),
            cost_complete: true,
            subscription: false,
            context: None,
            changed: vec![],
        })));
        let Some(Entry::Cost { runs, .. }) = app.transcript.last() else {
            panic!("a cost line ends the request");
        };
        let chain: Vec<(&str, Usd)> = runs.iter().map(|(m, s)| (m.as_str(), s.cost)).collect();
        assert_eq!(
            chain,
            [
                ("strong", Usd(0.006)),
                ("cheap", Usd(0.01)),
                ("strong", Usd(0.001))
            ]
        );
    }

    #[test]
    fn delete_takes_a_model_off_the_list_but_not_the_one_that_answers() {
        let mut app = App::new(
            Settings {
                tiers: vec![ModelId::new("a").unwrap()],
                models: vec![ModelId::new("a").unwrap(), ModelId::new("b").unwrap()],
                team: vec![ModelId::new("b").unwrap()],
                ..Settings::default()
            },
            PathBuf::from("/p"),
        );
        app.on_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL));
        press(&mut app, KeyCode::Up);
        press(&mut app, KeyCode::Delete);
        assert_eq!(app.models().len(), 2, "the one that answers stays");
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Delete);
        assert_eq!(app.models(), [ModelId::new("a").unwrap()]);
        assert!(app.team().is_empty());
    }

    #[test]
    fn a_model_set_by_name_joins_the_list() {
        let mut app = ready();
        type_text(&mut app, "/model openai/gpt-5-mini");
        press(&mut app, KeyCode::Enter);
        let names: Vec<String> = app.models().iter().map(ToString::to_string).collect();
        assert_eq!(names, ["cheap", "openai/gpt-5-mini"]);
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
            model: ModelId::agent(Agent::ClaudeCode),
            usage: Usage {
                input: TokenCount(30_000),
                output: TokenCount(200),
            },
            cost: None,
            subscription: true,
            context: None,
            cache: None,
        }));
        let (usage, cost, complete) = app.totals();
        assert_eq!(usage.input, TokenCount(30_000));
        assert_eq!(cost, Usd(0.0));
        assert!(complete);
    }

    #[test]
    fn ctrl_q_goes_back_to_typing_from_the_editor_in_insert_mode() {
        let (_dir, mut app) = project();
        app.on_key(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL));
        press(&mut app, KeyCode::Right);
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Enter);
        press(&mut app, KeyCode::Char('i'));
        app.on_key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL));
        assert_eq!((app.focus(), app.mode()), (Focus::Chat, Mode::Insert));
    }

    #[test]
    fn ctrl_s_and_question_mark_show_the_shortcuts_and_esc_hides_them() {
        let mut app = ready();
        let ctrl_s = KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL);
        app.on_key(ctrl_s);
        assert_eq!(app.keys_open(), Some(0));
        press(&mut app, KeyCode::Down);
        assert_eq!(app.keys_open(), Some(1));
        app.on_key(ctrl_s);
        assert_eq!(app.keys_open(), None);

        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Char('?'));
        assert_eq!(app.keys_open(), Some(0));
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.keys_open(), None);
        // Typing went nowhere while the list was open.
        assert_eq!(app.input().text(), "");
    }

    #[test]
    fn ctrl_a_opens_the_tree_and_goes_to_it_from_anywhere() {
        let (_dir, mut app) = project();
        let ctrl_a = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL);
        app.on_key(ctrl_a);
        assert!(app.tree().is_some());
        assert_eq!(app.focus(), Focus::Tree);

        // From the open file, in insert mode, back to the tree.
        press(&mut app, KeyCode::Right);
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Enter);
        press(&mut app, KeyCode::Char('i'));
        app.on_key(ctrl_a);
        assert_eq!((app.focus(), app.mode()), (Focus::Tree, Mode::Normal));
        assert!(app.tree().is_some());
    }

    #[test]
    fn ctrl_z_zooms_the_conversation_and_gives_the_panes_back() {
        let (_dir, mut app) = project();
        let ctrl = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL);
        app.on_key(ctrl('a'));
        press(&mut app, KeyCode::Right);
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.focus(), Focus::File);

        app.on_key(ctrl('z'));
        assert!(app.is_zoomed());
        assert_eq!(app.focus(), Focus::Chat);
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.focus(), Focus::Chat);

        app.on_key(ctrl('z'));
        assert!(!app.is_zoomed());
        assert_eq!(app.focus(), Focus::File);
        assert!(app.tree().is_some() && app.file().is_some());
    }

    #[test]
    fn arrows_select_replies_and_enter_folds_them_in_normal_mode() {
        let mut app = ready();
        app.on_agent(AgentMessage::Event(Event::Said {
            model: ModelId::new("cheap").unwrap(),
            text: "first".into(),
        }));
        app.on_agent(AgentMessage::Event(Event::Said {
            model: ModelId::new("cheap").unwrap(),
            text: "second".into(),
        }));
        let replies = app.replies();
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.selected_reply(), Some(replies[1]));
        press(&mut app, KeyCode::Up);
        assert_eq!(app.selected_reply(), Some(replies[0]));
        assert!(!app.is_expanded(replies[0]));
        press(&mut app, KeyCode::Enter);
        assert!(app.is_expanded(replies[0]));
        press(&mut app, KeyCode::Char(' '));
        assert!(!app.is_expanded(replies[0]));
        // Not shown as selected while typing.
        press(&mut app, KeyCode::Char('i'));
        assert_eq!(app.selected_reply(), None);
    }

    #[test]
    fn slash_context_opens_the_editor_and_w_applies_it() {
        let mut app = ready();
        type_text(&mut app, "/context");
        assert!(matches!(
            press(&mut app, KeyCode::Enter),
            Some(Effect::OpenContext)
        ));

        app.open_context("=== user\nhello\n=== assistant\nhi");
        assert_eq!(app.focus(), Focus::File);
        for c in "Gdd:w".chars() {
            press(&mut app, KeyCode::Char(c));
        }
        let effect = press(&mut app, KeyCode::Enter);
        assert!(
            matches!(effect, Some(Effect::ApplyContext(ref text)) if text == "=== user\nhello")
        );

        app.on_context_applied(Err("Unknown block === robot".into()));
        assert!(app.file().unwrap().is_modified());
    }

    #[test]
    fn colon_commands_work_from_inside_the_editor_and_the_open_context() {
        let mut app = ready();
        app.open_context("=== user\nhello");
        assert_eq!(app.focus(), Focus::File);
        for c in ":context".chars() {
            press(&mut app, KeyCode::Char(c));
        }
        assert!(matches!(
            press(&mut app, KeyCode::Enter),
            Some(Effect::OpenContext)
        ));

        for c in ":model other".chars() {
            press(&mut app, KeyCode::Char(c));
        }
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.current_model().unwrap().as_str(), "other");
    }

    #[test]
    fn reopening_the_context_keeps_edits_not_applied() {
        let mut app = ready();
        app.open_context("=== user\nhello\n=== user\nbye");
        for c in "Gdd".chars() {
            press(&mut app, KeyCode::Char(c));
        }
        app.open_context("=== user\nfresh");
        assert!(app.file().unwrap().is_modified());
        assert!(app.file().unwrap().lines().iter().all(|l| l != "fresh"));
    }

    #[test]
    fn tab_completes_colon_and_slash_commands() {
        let mut app = ready();
        type_text(&mut app, "/cont");
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.input().text(), "/context");
        assert_eq!(app.focus(), Focus::Chat);

        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Char(':'));
        type_text(&mut app, "cl");
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.command_line().text(), "claude");
        assert_eq!(app.completions().map(<[String]>::len), Some(3));
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.command_line().text(), "claude-reset");
        type_text(&mut app, "x");
        assert!(app.completions().is_none());
    }

    #[test]
    fn tab_still_switches_panes_while_typing_a_message() {
        let (_dir, mut app) = project();
        app.on_key(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL));
        press(&mut app, KeyCode::Char('i'));
        type_text(&mut app, "hello");
        press(&mut app, KeyCode::Tab);
        assert_ne!(app.focus(), Focus::Chat);
    }

    #[test]
    fn scrolling_stays_within_the_transcript() {
        let mut app = ready();
        app.set_max_scroll(5);
        press(&mut app, KeyCode::Esc);
        for _ in 0..20 {
            press(&mut app, KeyCode::PageUp);
        }
        assert_eq!(app.scroll_back(), 5);
        press(&mut app, KeyCode::End);
        assert_eq!(app.scroll_back(), 0);
    }
}
