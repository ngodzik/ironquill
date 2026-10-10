//! What the state asks for and only a loop can do: sending to the agent,
//! saving, compacting, asking docker. The terminal and a window drive the
//! same [`Host`], so that neither carries out an effect its own way.

use std::sync::Arc;
use std::time::Duration;

use ironquill_agent::{AgentConfig, Answer, Approver, Session};
use ironquill_core::{ChatModel, Delegate};
use ironquill_tools::{Container, Servers, Toolbox, Workspace};
use tokio::sync::{Mutex, mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio::time::Interval;

use crate::app::{AgentMessage, App, Effect};
use crate::defaults::Defaults;
use crate::sessions::Store;
use crate::{clipboard, review};

/// How the interface starts.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Start {
    /// A new conversation.
    #[default]
    New,
    /// The project's most recent conversation.
    Continue,
    /// The list of saved conversations, to pick one.
    Pick,
    /// The conversation with this id, or whose id starts so.
    Id(String),
}

/// How many tracked file names go to the model at the start of a conversation.
const FILE_LIST_LIMIT: usize = 300;

/// The conversation and the tools it edits with, shared with the task that
/// works on the current request. A stopped request drops its lock, and the
/// session repairs what it left half done before the next one.
struct Conversation {
    session: Session,
    toolbox: Toolbox,
}

/// What the state is doing, read before waiting so that the wait does not
/// hold the state: the timers it needs depend on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Waiting {
    /// A request runs: the spinner turns.
    pub running: bool,
    /// The Docker pane is open: docker is asked again now and then.
    pub docker: bool,
}

impl Waiting {
    /// What `app` is doing now.
    #[must_use]
    pub fn of(app: &App) -> Self {
        Self {
            running: app.is_running(),
            docker: app.docker().is_some(),
        }
    }
}

/// Something that happened while the person did nothing, for
/// [`Host::receive`] to apply.
#[derive(Debug)]
pub enum Incoming {
    /// The agent said something, or asks something.
    Agent(AgentMessage),
    /// Docker listed its containers, or could not.
    Docker(Result<Vec<Container>, String>),
    /// The spinner's next frame is due.
    Spin,
    /// Docker is due to be asked again.
    DockerDue,
    /// Whether to keep the sessions warm is due to be looked at.
    WarmDue,
}

/// Carries out the effects the state returns, holds the conversation the
/// agent works on, and reports back what the agent and the timers say.
pub struct Host<M, D> {
    model: Arc<M>,
    delegate: Arc<D>,
    workspace: Workspace,
    conversation: Arc<Mutex<Conversation>>,
    store: Option<Store>,
    /// The task working on the current request, while one does.
    task: Option<JoinHandle<()>>,
    tx: mpsc::UnboundedSender<AgentMessage>,
    rx: mpsc::UnboundedReceiver<AgentMessage>,
    docker_tx: mpsc::UnboundedSender<Result<Vec<Container>, String>>,
    docker_rx: mpsc::UnboundedReceiver<Result<Vec<Container>, String>>,
    /// Docker is asked one question at a time.
    docker_asking: bool,
    /// Only drives the spinner; nothing else depends on time.
    spin: Interval,
    /// The Docker pane asks again every two seconds while it is open.
    docker_tick: Interval,
    /// Whether keeping the sessions warm is due is looked at every half
    /// minute; it happens every four.
    warm_tick: Interval,
    /// A save asked for while a request runs waits for its end: the
    /// conversation is the request's meanwhile.
    save_pending: bool,
    /// The project's language servers, each started when first needed.
    servers: Arc<Servers>,
}

impl<M, D> Host<M, D>
where
    M: ChatModel + 'static,
    D: Delegate + 'static,
{
    /// A host for a new conversation in `workspace`, answered by `model`,
    /// with `delegate` for the agents it hands tasks to. Its timers start
    /// now, so it must be made inside a Tokio runtime.
    pub fn new(model: Arc<M>, delegate: Arc<D>, workspace: Workspace) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        let (docker_tx, docker_rx) = mpsc::unbounded_channel();
        let store = Store::for_project(workspace.root());
        let workspace_root = &workspace.root().to_owned();
        Self {
            conversation: Arc::new(Mutex::new(Conversation {
                session: Session::new(),
                toolbox: Toolbox::new(workspace.clone()),
            })),
            model,
            delegate,
            workspace,
            store,
            task: None,
            tx,
            rx,
            docker_tx,
            docker_rx,
            docker_asking: false,
            spin: tokio::time::interval(Duration::from_millis(120)),
            docker_tick: tokio::time::interval(Duration::from_secs(2)),
            warm_tick: tokio::time::interval(Duration::from_secs(30)),
            save_pending: false,
            servers: Arc::new(Servers::new(workspace_root)),
        }
    }

    /// Starts as asked: resumes a saved conversation or lists them.
    pub async fn start(&mut self, app: &mut App, start: Start) {
        let store = self.store.as_ref();
        match start {
            Start::New => {}
            Start::Continue => match store.and_then(|s| s.list().into_iter().next()) {
                Some(latest) => {
                    resume(store, app, &self.conversation, &self.workspace, &latest.id).await;
                }
                None => app.report_error("No saved conversation for this project yet".into()),
            },
            Start::Pick => app.show_picker(store.map(Store::list).unwrap_or_default()),
            Start::Id(id) => resume(store, app, &self.conversation, &self.workspace, &id).await,
        }
    }

    /// Waits for the next thing that is not the person's doing. Safe to
    /// drop unfinished, as `tokio::select!` does when a key comes first:
    /// nothing is lost.
    pub async fn next(&mut self, waiting: Waiting) -> Incoming {
        tokio::select! {
            Some(message) = self.rx.recv() => Incoming::Agent(message),
            Some(result) = self.docker_rx.recv() => Incoming::Docker(result),
            _ = self.docker_tick.tick(), if waiting.docker && !self.docker_asking => Incoming::DockerDue,
            _ = self.spin.tick(), if waiting.running => Incoming::Spin,
            _ = self.warm_tick.tick() => Incoming::WarmDue,
        }
    }

    /// Applies what happened to the state, and returns what it asks for in
    /// turn.
    pub async fn receive(&mut self, app: &mut App, incoming: Incoming) -> Option<Effect> {
        match incoming {
            Incoming::Agent(message) => {
                if app.on_agent(message) {
                    save(self.store.as_ref(), app, &self.conversation).await;
                }
                None
            }
            Incoming::Docker(result) => {
                self.docker_asking = false;
                app.on_docker(result);
                None
            }
            Incoming::DockerDue => Some(Effect::RefreshDocker),
            Incoming::Spin => {
                app.on_tick();
                None
            }
            Incoming::WarmDue => app.keep_warm_due().then_some(Effect::KeepWarm),
        }
    }

    /// Carries out `effect`, if any, after what is due on every turn of the
    /// loop: a save that waited for the request to end, and the choices to
    /// keep for the next session.
    pub async fn carry_out(&mut self, app: &mut App, effect: Option<Effect>) {
        if self.save_pending && !app.is_running() {
            self.save_pending = false;
            save(self.store.as_ref(), app, &self.conversation).await;
        }
        // What needs the conversation waits while a request has it, rather
        // than freeze the interface until its end.
        let effect = match effect {
            Some(Effect::Save) if app.is_running() => {
                self.save_pending = true;
                None
            }
            Some(Effect::OpenContext | Effect::ApplyContext(_) | Effect::ForgetDelegate(_))
                if app.is_running() =>
            {
                app.report_info("That waits for the request to end; Ctrl-C stops it");
                None
            }
            effect => effect,
        };

        // The model, the models offered, the team and the budget carry over
        // to the next session as soon as they change.
        if let Some(defaults) = app.defaults_to_keep()
            && let Some(path) = Defaults::path()
            && let Err(e) = defaults.save(&path)
        {
            app.report_error(format!("Could not keep your choices: {e}"));
        }

        let Some(effect) = effect else {
            return;
        };
        let Self {
            model,
            delegate,
            workspace,
            conversation,
            store,
            task,
            tx,
            docker_tx,
            docker_asking,
            servers,
            ..
        } = self;
        let store = store.as_ref();
        match effect {
            Effect::Send { text, config } => {
                // Read again each time: an edit counts from the next request.
                // A held command is put to the person in a window; the agent
                // waits for the answer.
                let asks = tx.clone();
                let approver = Approver::new(move |approval| {
                    let asks = asks.clone();
                    async move {
                        let (answer, answered) = oneshot::channel();
                        if asks.send(AgentMessage::Approve(approval, answer)).is_err() {
                            return Answer::No;
                        }
                        answered.await.unwrap_or(Answer::No)
                    }
                });
                let config = config
                    .with_instructions(Defaults::instructions())
                    .with_audit_log(Defaults::audit_log_path())
                    .with_project_rules(ironquill_tools::project_instructions(app.root()))
                    .with_approver(approver);
                *task = Some(spawn_agent(
                    Arc::clone(model),
                    Arc::clone(delegate),
                    Arc::clone(conversation),
                    config,
                    text,
                    tx.clone(),
                ));
            }
            Effect::Reset => {
                if let Some(handle) = task.take() {
                    handle.abort();
                }
                *conversation.lock().await = Conversation {
                    session: Session::new(),
                    toolbox: Toolbox::new(workspace.clone()),
                };
            }
            Effect::Cancel => {
                if let Some(handle) = task.take() {
                    handle.abort();
                    // Once it has stopped, the conversation is free: it says
                    // the request was cut short, and is kept as it is.
                    let _ = handle.await;
                    conversation.lock().await.session.interrupted();
                }
                app.on_cancelled();
                save(store, app, conversation).await;
            }
            Effect::KeepWarm => {
                // In the background, while the conversation waits; a request
                // sent meanwhile waits for it.
                let conversation = Arc::clone(conversation);
                let delegate = Arc::clone(delegate);
                let root = workspace.root().to_owned();
                let tx = tx.clone();
                tokio::spawn(async move {
                    let mut guard = conversation.lock().await;
                    guard
                        .session
                        .keep_warm(&*delegate, &root, |e| {
                            let _ = tx.send(AgentMessage::Event(e));
                        })
                        .await;
                });
            }
            Effect::PlanCompaction(config) => {
                let (conversation, model, delegate, tx) = (
                    Arc::clone(conversation),
                    Arc::clone(model),
                    Arc::clone(delegate),
                    tx.clone(),
                );
                tokio::spawn(async move {
                    let mut guard = conversation.lock().await;
                    let Conversation { session, toolbox } = &mut *guard;
                    let events = tx.clone();
                    let result = session
                        .plan_compaction(&*model, &*delegate, toolbox, &config, |e| {
                            let _ = events.send(AgentMessage::Event(e));
                        })
                        .await
                        .map_err(|e| error_chain(&e));
                    let _ = tx.send(AgentMessage::Compaction(result));
                });
            }
            Effect::Compact {
                config,
                keep,
                last_as_is,
            } => {
                let (conversation, model, delegate, tx) = (
                    Arc::clone(conversation),
                    Arc::clone(model),
                    Arc::clone(delegate),
                    tx.clone(),
                );
                tokio::spawn(async move {
                    let mut guard = conversation.lock().await;
                    let Conversation { session, toolbox } = &mut *guard;
                    let events = tx.clone();
                    let result = session
                        .compact(
                            &*model,
                            &*delegate,
                            toolbox,
                            &config,
                            &keep,
                            last_as_is,
                            |e| {
                                let _ = events.send(AgentMessage::Event(e));
                            },
                        )
                        .await
                        .map_err(|e| error_chain(&e));
                    let _ = tx.send(AgentMessage::Compacted(result));
                });
            }
            Effect::Find(lookup) => {
                let root = workspace.root().to_owned();
                let servers = Arc::clone(servers);
                let tx = tx.clone();
                tokio::task::spawn_blocking(move || {
                    let _ = tx.send(AgentMessage::Found(find(&servers, &root, &lookup)));
                });
            }
            Effect::WarmServers(paths) => {
                let servers = Arc::clone(servers);
                tokio::task::spawn_blocking(move || {
                    // One file per language is enough: the server reads the
                    // rest of the project itself.
                    let mut warmed = std::collections::HashSet::new();
                    for path in paths {
                        if let Some(program) = Servers::program_for(&path)
                            && warmed.insert(program)
                        {
                            let _ = servers.warm(&path);
                        }
                    }
                });
            }
            Effect::Address(number) => {
                let root = workspace.root().to_owned();
                let tx = tx.clone();
                tokio::task::spawn_blocking(move || {
                    let _ = tx.send(AgentMessage::Addressed(review::request(&root, &number)));
                });
            }
            Effect::Apply(numbers, patches) => {
                match review::apply(workspace.root(), &patches) {
                    Ok(()) => {
                        let list = numbers
                            .iter()
                            .map(ToString::to_string)
                            .collect::<Vec<_>>()
                            .join(", ");
                        conversation.lock().await.session.note_from_person(&format!(
                            "I applied the changes {list} with git apply."
                        ));
                        app.report_info(&format!("Applied {list}"));
                    }
                    Err(e) => app.report_error(e),
                }
            }
            Effect::ShowCommit(hash) => {
                let shown = std::process::Command::new("git")
                    .args(["show", "--stat", "--format=%h %s%n%an, %ar%n", &hash])
                    .current_dir(workspace.root())
                    .output()
                    .ok()
                    .filter(|o| o.status.success())
                    .map(|o| String::from_utf8_lossy(&o.stdout).trim_end().to_owned());
                match shown {
                    Some(text) => app.report_info(&text),
                    None => app.report_error(format!("git cannot show {hash}")),
                }
            }
            Effect::Copy(text) => match clipboard::copy(&text) {
                Ok(how) => app.info(format!("Copied ({how})")),
                Err(e) => app.report_error(e),
            },
            Effect::Diff => {
                let text = ironquill_tools::diff_stat(workspace.root())
                    .await
                    .unwrap_or_else(|_| "/diff needs the project to be a git repository".into());
                app.on_diff(&text);
            }
            Effect::Save => save(store, app, conversation).await,
            Effect::OpenInstructions => match Defaults::instructions_path() {
                Some(path) => {
                    if !path.exists() {
                        let created = path
                            .parent()
                            .map_or(Ok(()), std::fs::create_dir_all)
                            .and_then(|()| std::fs::write(&path, INSTRUCTIONS_TEMPLATE));
                        if let Err(e) = created {
                            app.report_error(format!("Could not create {}: {e}", path.display()));
                            return;
                        }
                    }
                    app.open_instructions(path);
                }
                None => app.report_error("No home directory to keep instructions in".into()),
            },
            Effect::OpenContext => {
                let text = conversation.lock().await.session.to_text();
                app.open_context(&text);
            }
            Effect::ApplyContext(text) => {
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
                    save(store, app, conversation).await;
                }
            }
            Effect::SaveDefaults(defaults) => {
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
            Effect::ForgetDelegate(agent) => {
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
                save(store, app, conversation).await;
            }
            Effect::RefreshDocker => {
                if !*docker_asking {
                    *docker_asking = true;
                    let tx = docker_tx.clone();
                    tokio::spawn(async move {
                        let _ = tx.send(ironquill_tools::running_containers().await);
                    });
                }
            }
            Effect::ListSessions => {
                app.show_picker(store.map(Store::list).unwrap_or_default());
            }
            Effect::Resume(id) => {
                resume(store, app, conversation, workspace, &id).await;
            }
        }
    }
}

impl<M, D> Drop for Host<M, D> {
    /// A request still running stops with the interface.
    fn drop(&mut self) {
        if let Some(handle) = self.task.take() {
            handle.abort();
        }
    }
}

/// Writes the conversation to disk. A failure is reported, never fatal: the
/// conversation goes on in memory.
async fn save(store: Option<&Store>, app: &mut App, conversation: &Mutex<Conversation>) {
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

/// What `/instructions` starts from when the file does not exist yet.
const INSTRUCTIONS_TEMPLATE: &str = "<!-- Your own instructions for every model ironquill runs, in every project: \
the one that answers, the planner and the coder of /pair, the team. Lines like this one are \
left out. Saved with :w, they count from the next request. -->\n";

/// Loads a saved conversation into the screen and into the agent.
async fn resume(
    store: Option<&Store>,
    app: &mut App,
    conversation: &Mutex<Conversation>,
    workspace: &Workspace,
    id: &str,
) {
    let Some(store) = store else {
        return;
    };
    match store.find(id).and_then(|id| store.load(&id)) {
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
        let message = match result {
            // Stopped before it began: the message goes back to the box.
            Err(ironquill_agent::AgentError::NotSent) => AgentMessage::NotSent(text),
            result => AgentMessage::Done(result.map_err(|e| error_chain(&e))),
        };
        let _ = tx.send(message);
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

/// Where `lookup`'s name is defined or used: as the language server for
/// its file says, or, when none answers or it only points at an import of
/// the file itself (an alias it could not follow), as `git grep` reads it.
fn find(
    servers: &Servers,
    root: &std::path::Path,
    lookup: &crate::app::Lookup,
) -> crate::app::Found {
    let asked = if lookup.uses {
        servers.references(&lookup.path, &lookup.text, lookup.line, lookup.column)
    } else {
        servers.definition(&lookup.path, &lookup.text, lookup.line, lookup.column)
    };
    let program = Servers::program_for(&lookup.path).unwrap_or("the language server");
    let mut files: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();
    let mut line_of = |path: &str, line: usize| -> String {
        let lines = files.entry(path.to_owned()).or_insert_with(|| {
            std::fs::read_to_string(root.join(path))
                .map(|t| t.lines().map(str::to_owned).collect())
                .unwrap_or_default()
        });
        lines
            .get(line.saturating_sub(1))
            .map(|l| l.trim().to_owned())
            .unwrap_or_default()
    };
    let here = lookup.path.to_string_lossy().into_owned();
    if let Ok(places) = asked {
        let places: Vec<ironquill_tools::Definition> = places
            .into_iter()
            .map(|p| ironquill_tools::Definition {
                text: line_of(&p.path, p.line),
                path: p.path,
                line: p.line,
            })
            .collect();
        let only_imports = places.iter().all(|p| {
            p.path == here && (p.text.starts_with("import ") || p.text.starts_with("from "))
        });
        if !places.is_empty() && (lookup.uses || !only_imports) {
            return crate::app::Found {
                name: lookup.name.clone(),
                uses: lookup.uses,
                places,
                by: program.to_owned(),
            };
        }
    }
    let lines: Vec<String> = lookup.text.lines().map(str::to_owned).collect();
    let places = if lookup.uses {
        ironquill_tools::uses(root, &lookup.name)
    } else {
        ironquill_tools::definitions(root, &lookup.name, &lookup.path, &lines)
    };
    crate::app::Found {
        name: lookup.name.clone(),
        uses: lookup.uses,
        places,
        by: "git grep".to_owned(),
    }
}
