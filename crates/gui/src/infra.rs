//! Where the project runs, as the services view shows it: its settings,
//! its repositories, its environments and their Terraform, read on a
//! thread of their own and kept in the cache while the repositories do not
//! change; each environment rendered by the tools on another when chosen,
//! and those the settings name after it, one at a time, for the fleet;
//! and the linked repositories brought up to date on a third.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, TryRecvError};

use universe_codemap::{
    Cache, Deploy, Deployment, Fleet, Place, Rendered, Repos, Services, Settings, Terraform,
    Update, cache_folder, read_settings, settings_file,
};
use universe_ui::App;

/// What is read of where the project runs.
pub(crate) struct Read {
    pub(crate) settings: Settings,
    pub(crate) repos: Repos,
    pub(crate) deploy: Deploy,
    pub(crate) terraform: Terraform,
    /// The linked repositories not on their main branch, and the branch
    /// each is on.
    pub(crate) off_main: Vec<(String, String)>,
    /// What could not be read: the settings' notes, the deployment's and
    /// Terraform's.
    pub(crate) notes: Vec<String>,
    fingerprint: String,
}

/// An environment rendered, and its architecture.
pub(crate) struct EnvView {
    pub(crate) rendered: Rendered,
    pub(crate) deployment: Deployment,
}

/// The state of the reading.
#[derive(Default)]
pub(crate) struct Infra {
    root: PathBuf,
    /// universe's own folder, `~/.universe`: where the settings and the
    /// cache are. `None` without a home.
    base: Option<PathBuf>,
    read: Option<Arc<Read>>,
    reading: Option<Receiver<Result<Read, String>>>,
    /// Why the settings could not be read.
    pub(crate) error: Option<String>,
    /// The environment chosen, by its place in the deployment's.
    pub(crate) environment: Option<usize>,
    views: HashMap<String, Arc<EnvView>>,
    rendering: Option<(String, Receiver<EnvView>)>,
    /// The environments to render after the chosen one, by their places.
    queue: Vec<usize>,
    /// Every environment, as last assembled, and how many were rendered
    /// then: assembled again only when another is.
    fleet: Option<(usize, Arc<Fleet>)>,
    updating: Option<Receiver<Vec<(String, Update)>>>,
}

/// universe's own folder, as the rest of it finds it.
fn base() -> Option<PathBuf> {
    match std::env::var_os("UNIVERSE_HOME") {
        Some(home) => Some(PathBuf::from(home)),
        None => Some(PathBuf::from(std::env::var_os("HOME")?).join(".universe")),
    }
}

impl Infra {
    pub(crate) fn new(root: PathBuf) -> Self {
        Self {
            root,
            base: base(),
            ..Self::default()
        }
    }

    /// The settings file of the project, to say where it is.
    pub(crate) fn settings_file(&self) -> Option<PathBuf> {
        Some(settings_file(self.base.as_ref()?, &self.root))
    }

    /// Starts reading when nothing is read or being read.
    pub(crate) fn want(&mut self) {
        if self.read.is_some() || self.reading.is_some() {
            return;
        }
        let (send, receive) = mpsc::channel();
        let (root, base) = (self.root.clone(), self.base.clone());
        std::thread::spawn(move || {
            let _ = send.send(read(&root, base.as_deref()));
        });
        self.reading = Some(receive);
    }

    /// What is read, once read.
    pub(crate) fn read(&self) -> Option<&Read> {
        self.read.as_deref()
    }

    /// Whether something is being read, rendered or updated.
    pub(crate) fn busy(&self) -> bool {
        self.reading.is_some() || self.rendering.is_some() || self.updating.is_some()
    }

    pub(crate) fn rendering(&self) -> Option<&str> {
        self.rendering.as_ref().map(|(name, _)| name.as_str())
    }

    pub(crate) fn updating(&self) -> bool {
        self.updating.is_some()
    }

    /// The chosen environment's view, once rendered.
    pub(crate) fn view(&self) -> Option<&EnvView> {
        let read = self.read.as_ref()?;
        let env = read.deploy.environments.get(self.environment?)?;
        self.views.get(&env.name).map(Arc::as_ref)
    }

    /// Takes what the threads finished; reports the repositories updated
    /// in the conversation, and reads again after.
    pub(crate) fn poll(&mut self, services: Option<&Services>, app: &mut App) {
        if let Some(reading) = &self.reading {
            match reading.try_recv() {
                Ok(Ok(read)) => {
                    self.reading = None;
                    self.error = None;
                    // The first environment the settings name, else none.
                    if self
                        .environment
                        .is_none_or(|e| e >= read.deploy.environments.len())
                    {
                        self.environment = read
                            .deploy
                            .environments
                            .iter()
                            .position(|e| e.chosen)
                            .or_else(|| (!read.deploy.environments.is_empty()).then_some(0));
                    }
                    self.read = Some(Arc::new(read));
                    self.fleet = None;
                }
                Ok(Err(e)) => {
                    self.reading = None;
                    self.error = Some(e);
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => self.reading = None,
            }
        }
        if let Some((name, rendering)) = &self.rendering {
            match rendering.try_recv() {
                Ok(view) => {
                    self.views.insert(name.clone(), Arc::new(view));
                    self.rendering = None;
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => self.rendering = None,
            }
        }
        if let Some(updating) = &self.updating {
            match updating.try_recv() {
                Ok(updates) => {
                    self.updating = None;
                    if updates.is_empty() {
                        app.report_info("No linked repository to update: the settings link none");
                    } else {
                        let lines: Vec<String> = updates
                            .iter()
                            .map(|(name, update)| format!("{name}: {update}"))
                            .collect();
                        app.report_info(&format!("Linked repositories: {}", lines.join("; ")));
                    }
                    // Read again, whatever changed.
                    self.read = None;
                    self.views.clear();
                    self.queue.clear();
                    self.fleet = None;
                    self.want();
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => self.updating = None,
            }
        }
        // The chosen environment, rendered once the services are read;
        // then those asked for after it.
        if self.rendering.is_some() {
            return;
        }
        let (Some(read), Some(services)) = (&self.read, services) else {
            return;
        };
        let views = &self.views;
        let unrendered = |i: &usize| {
            read.deploy
                .environments
                .get(*i)
                .is_some_and(|e| !views.contains_key(&e.name))
        };
        let next = self
            .environment
            .filter(unrendered)
            .or_else(|| self.queue.iter().copied().find(unrendered));
        self.queue.retain(|i| Some(*i) != next && unrendered(i));
        if let Some(chosen) = next
            && let Some(env) = read.deploy.environments.get(chosen)
        {
            let (send, receive) = mpsc::channel();
            let read = Arc::clone(read);
            let services = services.clone();
            let name = env.name.clone();
            let base = self.base.clone();
            let root = self.root.clone();
            std::thread::spawn(move || {
                let _ = send.send(render(&read, chosen, &services, base.as_deref(), &root));
            });
            self.rendering = Some((name, receive));
        }
    }

    /// Chooses the environment at `index`.
    pub(crate) fn choose(&mut self, index: usize) {
        self.environment = Some(index);
    }

    /// Renders the environments the settings name, after the chosen one,
    /// one at a time.
    pub(crate) fn render_all(&mut self) {
        let Some(read) = &self.read else {
            return;
        };
        let named: Vec<usize> = (0..read.deploy.environments.len())
            .filter(|i| read.deploy.environments[*i].chosen)
            .collect();
        for index in named {
            self.render_one(index);
        }
    }

    /// Renders the environment at `index` after those already asked for,
    /// unless it is rendered or asked for.
    pub(crate) fn render_one(&mut self, index: usize) {
        let Some(env) = self
            .read
            .as_ref()
            .and_then(|r| r.deploy.environments.get(index))
        else {
            return;
        };
        if !self.views.contains_key(&env.name)
            && self.rendering() != Some(env.name.as_str())
            && !self.queue.contains(&index)
        {
            self.queue.push(index);
        }
    }

    /// Every environment, those rendered with their objects, assembled
    /// again only when another was rendered.
    pub(crate) fn fleet(&mut self) -> Option<Arc<Fleet>> {
        let read = self.read.as_ref()?;
        if let Some((count, fleet)) = &self.fleet
            && *count == self.views.len()
        {
            return Some(Arc::clone(fleet));
        }
        let views = &self.views;
        let fleet = Arc::new(universe_codemap::fleet(&read.deploy, |env| {
            views.get(&env.name).map(|v| &v.rendered)
        }));
        self.fleet = Some((self.views.len(), Arc::clone(&fleet)));
        Some(fleet)
    }

    /// Brings the linked repositories up to date, on a thread.
    pub(crate) fn update_repos(&mut self) {
        let Some(read) = &self.read else {
            return;
        };
        if self.updating.is_some() {
            return;
        }
        let (send, receive) = mpsc::channel();
        let repos = read.repos.clone();
        std::thread::spawn(move || {
            let _ = send.send(universe_codemap::update_repos(&repos));
        });
        self.updating = Some(receive);
    }

    /// The file a place names, on this machine.
    pub(crate) fn file(&self, place: &Place) -> Option<PathBuf> {
        let read = self.read.as_ref()?;
        let path = read.repos.file(place)?;
        Some(path)
    }
}

/// Reads the settings, the repositories, the deployment and Terraform,
/// from the cache when the repositories have not changed.
fn read(root: &Path, base: Option<&Path>) -> Result<Read, String> {
    let home = std::env::var_os("HOME").map_or_else(|| root.to_owned(), PathBuf::from);
    let settings = match base {
        Some(base) => read_settings(&settings_file(base, root), &home).map_err(|e| {
            // The cause says where in the file.
            let cause = std::error::Error::source(&e)
                .map(ToString::to_string)
                .unwrap_or_default();
            format!("{e}: {cause}")
        })?,
        None => Settings::default(),
    };
    let repos = Repos::new(root, &settings.linked);
    let fingerprint = format!("{}|{:?}", universe_codemap::fingerprint(&repos), settings);
    let cache = base.map(|b| Cache::new(&cache_folder(b, root)));
    let cached: Option<(Deploy, Terraform)> =
        cache.as_ref().and_then(|c| c.get("deploy", &fingerprint));
    let (deploy, terraform) = cached.unwrap_or_else(|| {
        let read = (
            universe_codemap::deploy(&repos, &settings.environments),
            universe_codemap::terraform(&repos),
        );
        if let Some(cache) = &cache {
            let _ = cache.put("deploy", &fingerprint, &read);
        }
        read
    });
    let mut notes = settings.notes.clone();
    notes.extend(deploy.notes.iter().cloned());
    notes.extend(terraform.notes.iter().cloned());
    Ok(Read {
        off_main: universe_codemap::off_main(&repos),
        settings,
        repos,
        deploy,
        terraform,
        notes,
        fingerprint,
    })
}

/// Renders the environment `chosen` and draws its architecture, from the
/// cache when it holds them for these repositories.
fn render(
    read: &Read,
    chosen: usize,
    services: &Services,
    base: Option<&Path>,
    root: &Path,
) -> EnvView {
    let env = &read.deploy.environments[chosen];
    let cache = base.map(|b| Cache::new(&cache_folder(b, root)));
    let key = format!("env-{}", env.name);
    let cached: Option<(Rendered, Deployment)> =
        cache.as_ref().and_then(|c| c.get(&key, &read.fingerprint));
    let (rendered, deployment) = cached.unwrap_or_else(|| {
        let scratch = base.map_or_else(
            || std::env::temp_dir().join("universe-render"),
            |b| {
                cache_folder(b, root)
                    .join("scratch")
                    .join(chosen.to_string())
            },
        );
        let rendered = universe_codemap::render(&read.repos, &read.deploy, env, &scratch);
        let deployment = universe_codemap::deployment(env, &rendered, &read.terraform, services);
        let _ = std::fs::remove_dir_all(&scratch);
        if let Some(cache) = &cache {
            let _ = cache.put(&key, &read.fingerprint, &(&rendered, &deployment));
        }
        (rendered, deployment)
    });
    EnvView {
        rendered,
        deployment,
    }
}
