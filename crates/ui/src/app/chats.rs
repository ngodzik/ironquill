//! Several conversations in one window: the one shown is the `App`'s, the
//! others wait beside it, each going on with its request, its ticks and its
//! questions while hidden. Swapping one in for a moment lets every rule the
//! shown conversation follows apply to a hidden one unchanged.

use super::{App, Chat, ChatId, Effect, Entry};
use crate::input::{KeyCode, KeyEvent};
use crate::sessions::{self, ProjectState, Task};

/// What a conversation is doing, for the list of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatState {
    /// Nothing: it waits for a message.
    Idle,
    /// A request runs.
    Running,
    /// It waits for the person: a command to approve, or what /compact
    /// keeps.
    Waiting,
}

/// A row of `/chats`.
#[derive(Debug, Clone, PartialEq)]
pub struct ChatRow {
    /// Which conversation.
    pub id: ChatId,
    /// Its name, as `/name` gave it or its first request.
    pub name: String,
    /// What it is doing.
    pub state: ChatState,
    /// Whether it is the one shown.
    pub shown: bool,
    /// How many requests it holds.
    pub requests: usize,
}

/// A row of `/tasks`.
#[derive(Debug, Clone, PartialEq)]
pub struct TaskRow {
    /// What to do.
    pub title: String,
    /// Whether it is done.
    pub done: bool,
    /// The name of the conversation it is tied to, if any, and whether
    /// that one is open.
    pub chat: Option<(String, bool)>,
}

impl Chat {
    fn state(&self) -> ChatState {
        if self.approval.is_some() || self.compact_picker.is_some() {
            ChatState::Waiting
        } else if self.running_since.is_some() {
            ChatState::Running
        } else {
            ChatState::Idle
        }
    }

    /// Whether it was ever sent anything: only those are saved, and so
    /// reopened.
    fn is_saved(&self) -> bool {
        self.transcript.iter().any(|e| matches!(e, Entry::User(_)))
    }
}

impl App {
    /// The conversation shown.
    pub fn chat_id(&self) -> ChatId {
        self.chat.id
    }

    /// Every open conversation, in the order they were opened.
    pub fn chat_ids(&self) -> Vec<ChatId> {
        let mut ids: Vec<ChatId> = self.others.iter().map(|c| c.id).collect();
        ids.push(self.chat.id);
        ids.sort();
        ids
    }

    /// Runs `f` on the conversation `id` as if it were shown, and puts the
    /// shown one back after: a hidden conversation hears from its agent,
    /// ticks and saves by the same rules. `None` when it is not open.
    pub fn with_chat<R>(&mut self, id: ChatId, f: impl FnOnce(&mut App) -> R) -> Option<R> {
        if id == self.chat.id {
            return Some(f(self));
        }
        let at = self.others.iter().position(|c| c.id == id)?;
        std::mem::swap(&mut self.chat, &mut self.others[at]);
        let result = f(self);
        std::mem::swap(&mut self.chat, &mut self.others[at]);
        Some(result)
    }

    /// Opens an empty conversation and shows it; the one shown goes on
    /// beside it.
    pub fn open_chat(&mut self) -> ChatId {
        let id = ChatId(self.next_chat);
        self.next_chat += 1;
        let shown = std::mem::replace(&mut self.chat, Chat::new(id, vec![Entry::Welcome]));
        self.others.push(shown);
        self.leave_chat();
        id
    }

    /// Shows the conversation `id`. Returns whether it is open.
    pub fn show_chat(&mut self, id: ChatId) -> bool {
        if id == self.chat.id {
            return true;
        }
        let Some(at) = self.others.iter().position(|c| c.id == id) else {
            return false;
        };
        std::mem::swap(&mut self.chat, &mut self.others[at]);
        self.leave_chat();
        true
    }

    /// What the window showed of the conversation left: it belongs to it.
    fn leave_chat(&mut self) {
        self.chats_open = None;
        self.picker = None;
        self.entry_lines.borrow_mut().clear();
        self.fold_marks.borrow_mut().clear();
        self.copy_marks.borrow_mut().clear();
        self.link_marks.borrow_mut().clear();
    }

    /// Closes a conversation not shown, when no request runs in it; its
    /// file stays, to resume it later.
    fn close_chat(&mut self, id: ChatId) -> Result<(), &'static str> {
        if id == self.chat.id {
            return Err("The conversation shown cannot be closed: show another one first");
        }
        let at = self
            .others
            .iter()
            .position(|c| c.id == id)
            .ok_or("No such conversation")?;
        if self.others[at].running_since.is_some() {
            return Err("A request runs in it: stop it first");
        }
        self.others.remove(at);
        self.closed.push(id);
        Ok(())
    }

    /// The conversations closed since last asked, for the host to let go
    /// of what it holds for them.
    pub fn take_closed(&mut self) -> Vec<ChatId> {
        std::mem::take(&mut self.closed)
    }

    /// The conversation open with this saved id, if one is.
    fn open_with_id(&self, session: &str) -> Option<ChatId> {
        std::iter::once(&self.chat)
            .chain(&self.others)
            .find(|c| c.session_id == session)
            .map(|c| c.id)
    }

    /// Continues the saved conversation `session`: shows it when it is
    /// open already, else asks the host to load it in place of the one
    /// shown.
    pub(super) fn resume_or_show(&mut self, session: String) -> Option<Effect> {
        match self.open_with_id(&session) {
            Some(id) => {
                self.show_chat(id);
                None
            }
            None => Some(Effect::Resume(session)),
        }
    }

    /// Every open conversation, for `/chats`, in the order they were
    /// opened.
    pub fn chat_rows(&self) -> Vec<ChatRow> {
        let mut rows: Vec<ChatRow> = std::iter::once(&self.chat)
            .chain(&self.others)
            .map(|chat| ChatRow {
                id: chat.id,
                name: chat.name(),
                state: chat.state(),
                shown: chat.id == self.chat.id,
                requests: chat.requests,
            })
            .collect();
        rows.sort_by_key(|row| row.id);
        rows
    }

    /// The `/chats` list's selected row, while it is open.
    pub fn chats_open(&self) -> Option<usize> {
        self.chats_open
    }

    /// How many conversations wait for the person while hidden: shown in
    /// the status line, so that none waits unseen.
    pub fn hidden_waiting(&self) -> usize {
        self.others
            .iter()
            .filter(|c| c.state() == ChatState::Waiting)
            .count()
    }

    /// The other open conversations in a few words for a status line, and
    /// how many wait for the person; nothing with only one open.
    pub fn chats_label(&self) -> Option<String> {
        if self.others.is_empty() {
            return None;
        }
        let open = self.others.len() + 1;
        Some(match self.hidden_waiting() {
            0 => format!("{open} chats"),
            n => format!("{open} chats · {n} waiting"),
        })
    }

    /// Whether any open conversation would keep the machine awake.
    pub fn any_wants_awake(&mut self) -> bool {
        self.chat_ids()
            .into_iter()
            .any(|id| self.with_chat(id, |app| app.wants_awake()).unwrap_or(false))
    }

    /// Keys while `/chats` is open: Enter shows one, n opens a new one, x
    /// closes one. `None` when the key was not for it.
    pub(super) fn chats_key(&mut self, key: KeyEvent) -> Option<Option<Effect>> {
        let selected = self.chats_open?;
        let rows = self.chat_rows();
        let last = rows.len().saturating_sub(1);
        match key.code {
            KeyCode::Up => self.chats_open = Some(selected.saturating_sub(1)),
            KeyCode::Down => self.chats_open = Some((selected + 1).min(last)),
            KeyCode::Enter => {
                if let Some(row) = rows.get(selected) {
                    self.show_chat(row.id);
                }
                self.chats_open = None;
            }
            KeyCode::Char('n') => {
                self.open_chat();
            }
            KeyCode::Char('x') => {
                if let Some(row) = rows.get(selected) {
                    match self.close_chat(row.id) {
                        Ok(()) => {
                            self.chats_open = Some(selected.min(last.saturating_sub(1)));
                        }
                        Err(why) => self.notice = Some(why.to_owned()),
                    }
                }
            }
            KeyCode::Esc | KeyCode::Char('q') => self.chats_open = None,
            _ => {}
        }
        Some(None)
    }

    /// Opens `/chats` on the conversation shown.
    pub(super) fn show_chats(&mut self) {
        let shown = self.chat_ids().iter().position(|id| *id == self.chat.id);
        self.chats_open = Some(shown.unwrap_or(0));
    }

    /// What the project keeps: the conversations open that were saved, the
    /// one shown, the tasks. `Some` only when it changed since last kept.
    pub fn project_state_to_keep(&mut self) -> Option<ProjectState> {
        let mut open: Vec<(ChatId, String)> = std::iter::once(&self.chat)
            .chain(&self.others)
            .filter(|c| c.is_saved())
            .map(|c| (c.id, c.session_id.clone()))
            .collect();
        open.sort();
        let state = ProjectState {
            open: open.into_iter().map(|(_, id)| id).collect(),
            shown: self.chat.is_saved().then(|| self.chat.session_id.clone()),
            tasks: self.tasks.clone(),
        };
        if self.project_kept.as_ref() == Some(&state) {
            return None;
        }
        self.project_kept = Some(state.clone());
        Some(state)
    }

    /// Takes the project's tasks as saved, and what is kept as it was.
    pub fn project_state_loaded(&mut self, state: &ProjectState) {
        self.tasks = state.tasks.clone();
        self.project_kept = Some(state.clone());
    }

    /// Adds a task, tied to the conversation shown.
    pub(super) fn add_task(&mut self, title: String) {
        self.tasks.push(Task {
            title: title.clone(),
            done: false,
            chat: Some(self.chat.session_id.clone()),
            created: sessions::now(),
        });
        self.info(format!(
            "Task added, tied to this conversation: {title}. /tasks lists them"
        ));
    }

    /// The project's tasks, for `/tasks`.
    pub fn task_rows(&self) -> Vec<TaskRow> {
        self.tasks
            .iter()
            .map(|task| TaskRow {
                title: task.title.clone(),
                done: task.done,
                chat: task.chat.as_ref().map(|session| {
                    std::iter::once(&self.chat)
                        .chain(&self.others)
                        .find(|c| &c.session_id == session)
                        .map_or_else(|| (session.clone(), false), |c| (c.name(), true))
                }),
            })
            .collect()
    }

    /// The `/tasks` list's selected row, while it is open.
    pub fn tasks_open(&self) -> Option<usize> {
        self.tasks_open
    }

    /// Keys while `/tasks` is open: Space ticks one done, Enter opens its
    /// conversation, t ties it to the one shown, d deletes it. `None` when
    /// the key was not for it.
    pub(super) fn tasks_key(&mut self, key: KeyEvent) -> Option<Option<Effect>> {
        let selected = self.tasks_open?;
        let last = self.tasks.len().saturating_sub(1);
        match key.code {
            KeyCode::Up => self.tasks_open = Some(selected.saturating_sub(1)),
            KeyCode::Down => self.tasks_open = Some((selected + 1).min(last)),
            KeyCode::Char(' ') => {
                if let Some(task) = self.tasks.get_mut(selected) {
                    task.done = !task.done;
                }
            }
            KeyCode::Char('t') => {
                let session = self.chat.session_id.clone();
                if let Some(task) = self.tasks.get_mut(selected) {
                    task.chat = Some(session);
                }
            }
            KeyCode::Char('d') => {
                if selected < self.tasks.len() {
                    self.tasks.remove(selected);
                    self.tasks_open = Some(selected.min(self.tasks.len().saturating_sub(1)));
                }
            }
            KeyCode::Enter => {
                let Some(session) = self.tasks.get(selected).and_then(|t| t.chat.clone()) else {
                    self.notice =
                        Some("This task is tied to no conversation: t ties it to this one".into());
                    return Some(None);
                };
                self.tasks_open = None;
                if let Some(id) = self.open_with_id(&session) {
                    self.show_chat(id);
                    return Some(None);
                }
                return Some(Some(Effect::OpenSaved(session)));
            }
            KeyCode::Esc | KeyCode::Char('q') => self.tasks_open = None,
            _ => {}
        }
        Some(None)
    }

    /// Opens `/tasks`, or says there is none yet.
    pub(super) fn show_tasks(&mut self) {
        if self.tasks.is_empty() {
            self.info("No task yet: /task <what to do> adds one, tied to this conversation");
            return;
        }
        self.tasks_open = Some(0);
    }
}

#[cfg(test)]
mod tests {
    use ironquill_agent::{Outcome, Verdict};
    use ironquill_core::{ModelId, Usage, Usd};
    use ironquill_tools::Check;

    use super::*;
    use crate::app::{AgentMessage, Settings};
    use crate::input::KeyModifiers;

    fn press(app: &mut App, code: KeyCode) -> Option<Effect> {
        app.on_key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn run(app: &mut App, text: &str) -> Option<Effect> {
        for c in text.chars() {
            press(app, KeyCode::Char(c));
        }
        press(app, KeyCode::Enter)
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
            std::env::temp_dir(),
        )
    }

    fn done() -> AgentMessage {
        AgentMessage::Done(Ok(Outcome {
            verdict: Verdict::Answered,
            usage: Usage::default(),
            cost: Usd(0.0),
            cost_complete: true,
            subscription: false,
            context: None,
            changed: vec![],
        }))
    }

    #[test]
    fn a_hidden_conversation_goes_on_and_hears_its_own_agent() {
        let mut app = ready();
        assert!(matches!(
            run(&mut app, "fix the parser"),
            Some(Effect::Send { .. })
        ));
        let first = app.chat_id();
        assert!(run(&mut app, "/newchat").is_none());
        let second = app.chat_id();
        assert_ne!(first, second);
        assert!(!app.is_running());
        assert_eq!(app.chats_label().as_deref(), Some("2 chats"));
        let rows = app.chat_rows();
        assert_eq!(rows[0].state, ChatState::Running);
        assert!(rows[1].shown);
        // The running one cannot be closed; once done, it can.
        run(&mut app, "/chats");
        press(&mut app, KeyCode::Up);
        press(&mut app, KeyCode::Char('x'));
        assert_eq!(app.chat_ids().len(), 2);
        assert_eq!(app.with_chat(first, |app| app.on_agent(done())), Some(true));
        assert_eq!(app.chat_rows()[0].state, ChatState::Idle);
        assert!(
            app.transcript()
                .iter()
                .all(|e| !matches!(e, Entry::Cost { .. }))
        );
        press(&mut app, KeyCode::Char('x'));
        assert_eq!(app.chat_ids(), [second]);
        assert_eq!(app.take_closed(), [first]);
    }

    #[test]
    fn enter_in_the_list_shows_a_conversation() {
        let mut app = ready();
        let first = app.chat_id();
        run(&mut app, "first question");
        run(&mut app, "/newchat");
        run(&mut app, "/chats");
        assert_eq!(app.chats_open(), Some(1));
        press(&mut app, KeyCode::Up);
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.chat_id(), first);
        assert!(app.is_running());
        assert_eq!(app.chats_open(), None);
    }

    #[test]
    fn tasks_are_tied_ticked_and_open_their_conversation() {
        let mut app = ready();
        run(&mut app, "/task write the release notes");
        let session = app.chat.session_id.clone();
        let rows = app.task_rows();
        assert_eq!(rows[0].title, "write the release notes");
        assert_eq!(rows[0].chat, Some(("new conversation".into(), true)));
        run(&mut app, "/tasks");
        press(&mut app, KeyCode::Char(' '));
        assert!(app.task_rows()[0].done);
        // Its conversation is shown when open, loaded when not.
        let first = app.chat_id();
        run(&mut app, "/newchat");
        run(&mut app, "/tasks");
        assert!(press(&mut app, KeyCode::Enter).is_none());
        assert_eq!(app.chat_id(), first);
        app.tasks[0].chat = Some("1234-abc".into());
        run(&mut app, "/tasks");
        assert!(matches!(
            press(&mut app, KeyCode::Enter),
            Some(Effect::OpenSaved(id)) if id == "1234-abc"
        ));
        run(&mut app, "/tasks");
        press(&mut app, KeyCode::Char('t'));
        assert_eq!(app.tasks[0].chat.as_ref(), Some(&session));
        press(&mut app, KeyCode::Char('d'));
        assert!(app.tasks.is_empty());
    }

    #[test]
    fn the_project_is_kept_only_when_it_changed() {
        let mut app = ready();
        app.project_state_loaded(&ProjectState::default());
        assert_eq!(app.project_state_to_keep(), None);
        run(&mut app, "hello");
        let state = app.project_state_to_keep().unwrap();
        assert_eq!(state.open, [app.chat.session_id.clone()]);
        assert_eq!(state.shown.as_ref(), Some(&app.chat.session_id));
        assert_eq!(app.project_state_to_keep(), None);
        run(&mut app, "/newchat");
        let state = app.project_state_to_keep().unwrap();
        // The new one is empty: not saved, so not reopened.
        assert_eq!((state.open.len(), state.shown), (1, None));
        run(&mut app, "/task ship it");
        assert_eq!(app.project_state_to_keep().unwrap().tasks.len(), 1);
    }
}
