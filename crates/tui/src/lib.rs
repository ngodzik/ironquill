//! The ironquill terminal interface.
//!
//! Split so that each part can change without the others:
//! - [`keymap`] turns keys into actions; rebinding a key touches only it.
//! - `App` holds the state and decides what each action does. It performs
//!   no I/O: it returns an `Effect` for the loop to carry out, which is what
//!   makes it testable without a terminal.
//! - `view` draws the state and changes nothing.
//! - The loop below wires the terminal, the keyboard and the agent together.

#![deny(unsafe_code)]
#![warn(missing_docs)]

mod app;
mod clipboard;
mod command;
mod defaults;
mod editor;
mod error;
mod highlight;
pub mod keymap;
mod markdown;
mod sessions;
mod tree;
mod usage;
mod view;
mod wrap;

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use ironquill_agent::{AgentConfig, Approver, Session};
use ironquill_core::{ChatModel, Delegate};
use ironquill_tools::{Toolbox, Workspace};
use ratatui::crossterm::event::{
    DisableMouseCapture, EnableMouseCapture, Event as TermEvent, EventStream, KeyEventKind,
};
use ratatui::crossterm::execute;
use tokio::sync::{Mutex, mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::app::{AgentMessage, App, Effect};
use crate::sessions::Store;

pub use app::Settings;
pub use defaults::Defaults;

/// How the interface starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Start {
    /// A new conversation.
    #[default]
    New,
    /// The project's most recent conversation.
    Continue,
    /// The list of saved conversations, to pick one.
    Pick,
}
pub use error::TuiError;

/// How many tracked file names go to the model at the start of a conversation.
const FILE_LIST_LIMIT: usize = 300;

/// The conversation and the tools it edits with, shared with the task that
/// works on the current request. A stopped request drops its lock, and the
/// session repairs what it left half done before the next one.
struct Conversation {
    session: Session,
    toolbox: Toolbox,
}

/// Opens the interface in the current terminal and runs until the person quits.
///
/// `workspace` is the project the agent works on.
///
/// # Errors
///
/// [`TuiError::Terminal`] when the terminal cannot be set up or drawn to.
pub async fn run<M, D>(
    model: Arc<M>,
    delegate: Arc<D>,
    workspace: Workspace,
    settings: Settings,
    start: Start,
) -> Result<(), TuiError>
where
    M: ChatModel + 'static,
    D: Delegate + 'static,
{
    let mut terminal = ratatui::try_init().map_err(TuiError::Terminal)?;
    // Clicks and the wheel reach the interface. Selecting text with the mouse
    // then needs Shift held, as in most terminal applications that do this.
    let mouse = execute!(std::io::stdout(), EnableMouseCapture).map_err(TuiError::Terminal);
    let result = match mouse {
        Ok(()) => event_loop(&mut terminal, model, delegate, workspace, settings, start).await,
        Err(e) => Err(e),
    };
    // Restored whatever happened, or the person's shell is left in raw mode.
    let _ = execute!(std::io::stdout(), DisableMouseCapture);
    ratatui::restore();
    result
}

async fn event_loop<M, D>(
    terminal: &mut ratatui::DefaultTerminal,
    model: Arc<M>,
    delegate: Arc<D>,
    workspace: Workspace,
    settings: Settings,
    start: Start,
) -> Result<(), TuiError>
where
    M: ChatModel + 'static,
    D: Delegate + 'static,
{
    let mut app = App::new(settings, workspace.root().to_owned());
    let conversation = Arc::new(Mutex::new(Conversation {
        session: Session::new(),
        toolbox: Toolbox::new(workspace.clone()),
    }));
    let store = Store::for_project(workspace.root());
    match start {
        Start::New => {}
        Start::Continue => match store.as_ref().and_then(|s| s.list().into_iter().next()) {
            Some(latest) => resume(&store, &mut app, &conversation, &workspace, &latest.id).await,
            None => app.report_error("No saved conversation for this project yet".into()),
        },
        Start::Pick => app.show_picker(store.as_ref().map(Store::list).unwrap_or_default()),
    }
    let mut keys = EventStream::new();
    let (tx, mut rx) = mpsc::unbounded_channel::<AgentMessage>();
    let mut task: Option<JoinHandle<()>> = None;
    // Only drives the spinner; nothing else depends on time.
    let mut tick = tokio::time::interval(Duration::from_millis(120));
    // The Docker pane asks again every two seconds while it is open, one
    // question at a time.
    let (docker_tx, mut docker_rx) = mpsc::unbounded_channel();
    let mut docker_tick = tokio::time::interval(Duration::from_secs(2));
    let mut docker_asking = false;

    while !app.should_quit() {
        terminal
            .draw(|frame| view::render(frame, &app))
            .map_err(TuiError::Terminal)?;

        let effect = tokio::select! {
            key = keys.next() => match key {
                Some(Ok(TermEvent::Key(key))) if key.kind == KeyEventKind::Press => app.on_key(key),
                Some(Ok(TermEvent::Mouse(mouse))) => {
                    app.on_mouse(mouse);
                    None
                }
                Some(Ok(_)) => None,
                Some(Err(e)) => return Err(TuiError::Terminal(e)),
                None => break,
            },
            Some(message) = rx.recv() => {
                if app.on_agent(message) {
                    save(&store, &mut app, &conversation).await;
                }
                None
            },
            Some(result) = docker_rx.recv() => {
                docker_asking = false;
                app.on_docker(result);
                None
            },
            _ = docker_tick.tick(), if app.docker().is_some() && !docker_asking => {
                Some(Effect::RefreshDocker)
            },
            _ = tick.tick(), if app.is_running() => {
                app.on_tick();
                None
            },
        };

        // The model, the models offered, the team and the budget carry over
        // to the next session as soon as they change.
        if let Some(defaults) = app.defaults_to_keep()
            && let Some(path) = Defaults::path()
            && let Err(e) = defaults.save(&path)
        {
            app.report_error(format!("Could not keep your choices: {e}"));
        }

        match effect {
            None => {}
            Some(Effect::Send { text, config }) => {
                // Read again each time: an edit counts from the next request.
                // A held command is put to the person in a window; the agent
                // waits for the answer.
                let asks = tx.clone();
                let approver = Approver::new(move |approval| {
                    let asks = asks.clone();
                    async move {
                        let (answer, answered) = oneshot::channel();
                        if asks.send(AgentMessage::Approve(approval, answer)).is_err() {
                            return false;
                        }
                        answered.await.unwrap_or(false)
                    }
                });
                let config = config
                    .with_instructions(Defaults::instructions())
                    .with_project_rules(ironquill_tools::project_instructions(app.root()))
                    .with_approver(approver);
                task = Some(spawn_agent(
                    Arc::clone(&model),
                    Arc::clone(&delegate),
                    Arc::clone(&conversation),
                    config,
                    text,
                    tx.clone(),
                ));
            }
            Some(Effect::Reset) => {
                if let Some(handle) = task.take() {
                    handle.abort();
                }
                *conversation.lock().await = Conversation {
                    session: Session::new(),
                    toolbox: Toolbox::new(workspace.clone()),
                };
            }
            Some(Effect::Cancel) => {
                if let Some(handle) = task.take() {
                    handle.abort();
                }
                app.on_cancelled();
            }
            Some(Effect::Diff) => {
                let text = ironquill_tools::diff_stat(workspace.root())
                    .await
                    .unwrap_or_else(|_| "/diff needs the project to be a git repository".into());
                app.on_diff(&text);
            }
            Some(Effect::Save) => save(&store, &mut app, &conversation).await,
            Some(Effect::OpenInstructions) => match Defaults::instructions_path() {
                Some(path) => {
                    if !path.exists() {
                        let created = path
                            .parent()
                            .map_or(Ok(()), std::fs::create_dir_all)
                            .and_then(|()| std::fs::write(&path, INSTRUCTIONS_TEMPLATE));
                        if let Err(e) = created {
                            app.report_error(format!("Could not create {}: {e}", path.display()));
                            continue;
                        }
                    }
                    app.open_path(path);
                }
                None => app.report_error("No home directory to keep instructions in".into()),
            },
            Some(Effect::OpenContext) => {
                let text = conversation.lock().await.session.to_text();
                app.open_context(&text);
            }
            Some(Effect::ApplyContext(text)) => {
                let result = {
                    let mut guard = conversation.lock().await;
                    let before = guard.session.approx_tokens();
                    guard
                        .session
                        .apply_text(&text)
                        .map(|()| (before, guard.session.approx_tokens()))
                };
                let applied = result.is_ok();
                app.on_context_applied(result);
                if applied {
                    save(&store, &mut app, &conversation).await;
                }
            }
            Some(Effect::SaveDefaults(defaults)) => {
                let saved = Defaults::path()
                    .ok_or_else(|| "no home directory to keep them in".to_owned())
                    .and_then(|path| {
                        defaults
                            .save(&path)
                            .map(|()| path)
                            .map_err(|e| e.to_string())
                    });
                match saved {
                    Ok(path) => app.report_info(&format!(
                        "New sessions will start with these choices ({})",
                        path.display()
                    )),
                    Err(e) => app.report_error(format!("Could not keep the defaults: {e}")),
                }
            }
            Some(Effect::ForgetDelegate(agent)) => {
                let ended = {
                    let mut guard = conversation.lock().await;
                    let had = guard.session.delegate_session(agent).is_some();
                    guard.session.forget_delegate(agent);
                    had
                };
                app.report_info(&if ended {
                    format!("{agent}'s session ended: its next request starts from nothing")
                } else {
                    format!("No {agent} session to end")
                });
                save(&store, &mut app, &conversation).await;
            }
            Some(Effect::RefreshDocker) => {
                if !docker_asking {
                    docker_asking = true;
                    let tx = docker_tx.clone();
                    tokio::spawn(async move {
                        let _ = tx.send(ironquill_tools::running_containers().await);
                    });
                }
            }
            Some(Effect::ListSessions) => {
                app.show_picker(store.as_ref().map(Store::list).unwrap_or_default());
            }
            Some(Effect::Resume(id)) => {
                resume(&store, &mut app, &conversation, &workspace, &id).await;
            }
        }
    }

    if let Some(handle) = task {
        handle.abort();
    }
    Ok(())
}

/// Writes the conversation to disk. A failure is reported, never fatal: the
/// conversation goes on in memory.
async fn save(store: &Option<Store>, app: &mut App, conversation: &Mutex<Conversation>) {
    let Some(store) = store else {
        return;
    };
    let session = conversation.lock().await.session.clone();
    if let Some(saved) = app.to_saved(session)
        && let Err(e) = store.save(&saved)
    {
        app.report_error(format!("Could not save the conversation: {e}"));
    }
}

/// Loads a saved conversation into the screen and into the agent.
/// What `/instructions` starts from when the file does not exist yet.
const INSTRUCTIONS_TEMPLATE: &str = "<!-- Your own instructions for every model ironquill runs, in every project: \
the one that answers, the planner and the coder of /pair, the team. Lines like this one are \
left out. Saved with :w, they count from the next request. -->\n";

async fn resume(
    store: &Option<Store>,
    app: &mut App,
    conversation: &Mutex<Conversation>,
    workspace: &Workspace,
    id: &str,
) {
    let Some(store) = store else {
        return;
    };
    match store.load(id) {
        Ok(saved) => {
            *conversation.lock().await = Conversation {
                session: saved.session.clone(),
                toolbox: Toolbox::new(workspace.clone()),
            };
            app.load_saved(saved);
        }
        Err(e) => app.report_error(e),
    }
}

fn spawn_agent<M, D>(
    model: Arc<M>,
    delegate: Arc<D>,
    conversation: Arc<Mutex<Conversation>>,
    config: AgentConfig,
    text: String,
    tx: mpsc::UnboundedSender<AgentMessage>,
) -> JoinHandle<()>
where
    M: ChatModel + 'static,
    D: Delegate + 'static,
{
    tokio::spawn(async move {
        let mut guard = conversation.lock().await;
        let Conversation { session, toolbox } = &mut *guard;
        let root = toolbox.workspace().root().to_owned();
        let context = ironquill_tools::project_context(&root, FILE_LIST_LIMIT).await;
        let events = tx.clone();
        let result = session
            .send(
                &*model,
                &*delegate,
                toolbox,
                &config,
                &text,
                &context,
                |e| {
                    // The receiver only goes away when the interface is closing.
                    let _ = events.send(AgentMessage::Event(e));
                },
            )
            .await;
        let _ = tx.send(AgentMessage::Done(result.map_err(|e| error_chain(&e))));
    })
}

/// An error and its causes on one line, so that "the request failed" says why.
fn error_chain(error: &dyn std::error::Error) -> String {
    let mut text = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        text.push_str(": ");
        text.push_str(&cause.to_string());
        source = cause.source();
    }
    text
}
