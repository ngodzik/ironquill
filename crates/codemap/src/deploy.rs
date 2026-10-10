//! Where the project runs, read from its deployment files by the tools'
//! own conventions: Helm charts, Kustomize trees, and Flux's objects. An
//! environment is a Kustomize tree nothing else includes; a service is a
//! Flux `HelmRelease`; and each of its settings keeps every layer that set
//! it, from the chart's default to the last patch, with its file and line.
//!
//! Nothing is run here: [`crate::render`] builds the same trees with the
//! tools themselves. Encrypted files are named, never read.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::repos::{Place, Repos, files, normal};
use crate::secret;
use crate::settings::NamedEnvironment;
use crate::yaml::{self, Node, Value};

/// The names a Kustomize tree's file goes by.
const KUSTOMIZATION: [&str; 3] = ["kustomization.yaml", "kustomization.yml", "Kustomization"];

/// Folders that are only ever included, never an environment.
const NEVER_ROOTS: [&str; 3] = ["base", "bases", "components"];

/// A value set by an earlier layer, and replaced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Earlier {
    /// The value it had.
    pub value: String,
    /// Where it was set.
    pub place: Place,
}

/// A setting of a service, as its layers leave it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Setting {
    /// Its key, dotted, lists indexed: `image.tag`, `env[0].name`.
    pub key: String,
    /// Its value; a secret's is hidden.
    pub value: String,
    /// Where the value was set.
    pub place: Place,
    /// What it replaced, the latest first.
    pub replaced: Vec<Earlier>,
}

/// A Helm chart of one of the repositories.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Chart {
    /// Its name, as `Chart.yaml` says.
    pub name: String,
    /// Its version.
    pub version: String,
    /// Its repository.
    pub repo: String,
    /// Its folder, from the repository's root.
    pub folder: String,
    /// What it depends on: each chart's name and where it comes from.
    pub dependencies: Vec<(String, String)>,
}

/// Where a release's chart comes from, as Flux's source says.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChartSource {
    /// The source's kind: `OCIRepository`, `GitRepository`, `HelmRepository`.
    pub kind: String,
    /// Its name.
    pub name: String,
    /// Its URL.
    pub url: Option<String>,
    /// The tag, branch or version it is pinned to.
    pub reference: Option<String>,
}

/// A service as a Flux `HelmRelease` deploys it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Release {
    /// Its name.
    pub name: String,
    /// The name shown: its name, with its namespace when another release
    /// of the environment has the same name.
    pub label: String,
    /// The namespace it is installed in.
    pub namespace: Option<String>,
    /// The chart's name.
    pub chart_name: Option<String>,
    /// The chart, by its place in [`Deploy::charts`], when it is in one of
    /// the repositories.
    pub chart: Option<usize>,
    /// Where Flux takes the chart from.
    pub source: Option<ChartSource>,
    /// Every setting, its layers kept.
    pub settings: Vec<Setting>,
    /// The values given to the chart, its own defaults left out: what
    /// `helm template` is given.
    pub values: serde_json::Value,
    /// Where it is declared.
    pub place: Place,
    /// What could not be read of it.
    pub notes: Vec<String>,
}

/// A Kubernetes object an environment declares, other than a release.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    /// Its kind.
    pub kind: String,
    /// Its name.
    pub name: String,
    /// Its namespace.
    pub namespace: Option<String>,
    /// Where it is declared.
    pub place: Place,
}

/// A Kustomize tree an environment is built from.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Part {
    /// Its repository.
    pub repo: String,
    /// Its folder, from the repository's root.
    pub folder: String,
}

/// An environment: a Kustomize tree that nothing else includes, and the
/// trees Flux's `Kustomization`s in it point at.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Environment {
    /// Its name: as the settings name it, else its folder.
    pub name: String,
    /// Whether the settings name it, to list it first.
    pub chosen: bool,
    /// The trees it is built from, its own first.
    pub parts: Vec<Part>,
    /// The services it deploys.
    pub releases: Vec<Release>,
    /// The other objects it declares.
    pub manifests: Vec<Manifest>,
    /// The encrypted files it includes, named and never read.
    pub encrypted: Vec<Place>,
    /// What could not be read.
    pub notes: Vec<String>,
}

/// What the repositories say of where the project runs.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Deploy {
    /// The Helm charts.
    pub charts: Vec<Chart>,
    /// The environments, those the settings name first, in their order.
    pub environments: Vec<Environment>,
    /// What could not be read.
    pub notes: Vec<String>,
}

impl Deploy {
    /// The folder of a chart, on this machine.
    #[must_use]
    pub fn chart_folder(&self, repos: &Repos, chart: usize) -> Option<PathBuf> {
        let chart = self.charts.get(chart)?;
        Some(repos.get(&chart.repo)?.root.join(&chart.folder))
    }
}

/// Whether a file is encrypted, by its name: `*.enc.*`, `*.sops.*`.
pub(crate) fn encrypted_name(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    name.contains(".enc.") || name.contains(".sops.") || name.ends_with(".enc")
}

/// Reads where the project runs from `repos`, the environments `named`
/// listed first under their names.
#[must_use]
pub fn deploy(repos: &Repos, named: &[NamedEnvironment]) -> Deploy {
    let mut deploy = Deploy::default();
    let mut kustomizations: Vec<PathBuf> = Vec::new();
    let mut sources: Vec<(Node, Place)> = Vec::new();
    for repo in &repos.repos {
        for (path, rel) in files(&repo.root) {
            let name = rel.rsplit('/').next().unwrap_or(&rel);
            if name == "Chart.yaml" {
                if let Some(chart) = read_chart(&path, &repo.name, &rel) {
                    deploy.charts.push(chart);
                }
            } else if KUSTOMIZATION.contains(&name) {
                if let Some(dir) = path.parent() {
                    kustomizations.push(dir.to_owned());
                }
            } else if (rel.ends_with(".yaml") || rel.ends_with(".yml")) && !encrypted_name(&rel) {
                let Ok(text) = std::fs::read_to_string(&path) else {
                    continue;
                };
                if !["OCIRepository", "GitRepository", "HelmRepository"]
                    .iter()
                    .any(|k| text.contains(k))
                {
                    continue;
                }
                for doc in yaml::docs(&text) {
                    if doc
                        .str_at(&["kind"])
                        .is_some_and(|k| k.ends_with("Repository"))
                    {
                        let line = doc.line;
                        sources.push((doc, Place::new(&repo.name, &rel, line)));
                    }
                }
            }
        }
    }

    // The trees nothing includes are the environments.
    let mut included: HashSet<PathBuf> = HashSet::new();
    for dir in &kustomizations {
        included.extend(includes(repos, dir));
    }
    let mut roots: Vec<(PathBuf, String, String)> = kustomizations
        .iter()
        .filter(|dir| !included.contains(&normal(dir)))
        .filter(|dir| {
            dir.file_name()
                .is_none_or(|n| !NEVER_ROOTS.contains(&n.to_string_lossy().as_ref()))
        })
        .filter_map(|dir| {
            let (repo, rel) = repos.locate(dir)?;
            Some((dir.clone(), repo.name.clone(), rel))
        })
        .collect();
    roots.sort_by(|a, b| (&a.1, &a.2).cmp(&(&b.1, &b.2)));

    let mut environments: Vec<Environment> = roots
        .iter()
        .map(|(dir, repo, folder)| {
            let chosen = named.iter().find(|n| same_folder(&n.path, folder));
            let mut environment = environment(repos, dir, repo, folder, &sources, &deploy.charts);
            if let Some(chosen) = chosen {
                environment.name.clone_from(&chosen.name);
                environment.chosen = true;
            }
            environment
        })
        .collect();
    for named in named {
        if !environments
            .iter()
            .any(|e| e.chosen && e.name == named.name)
        {
            deploy.notes.push(format!(
                "The environment {} is at {}, where no Kustomize tree was found",
                named.name, named.path
            ));
        }
    }
    environments.sort_by_key(|e| {
        (
            named
                .iter()
                .position(|n| e.chosen && n.name == e.name)
                .unwrap_or(usize::MAX),
            e.name.clone(),
        )
    });
    deploy.environments = environments;
    deploy
}

fn same_folder(a: &str, b: &str) -> bool {
    let clean = |s: &str| s.trim_start_matches("./").trim_matches('/').to_owned();
    clean(a) == clean(b)
}

fn read_chart(path: &Path, repo: &str, rel: &str) -> Option<Chart> {
    let doc = yaml::doc(&std::fs::read_to_string(path).ok()?)?;
    Some(Chart {
        name: doc.str_at(&["name"])?.to_owned(),
        version: doc.str_at(&["version"]).unwrap_or("").to_owned(),
        repo: repo.to_owned(),
        folder: rel.rsplit_once('/').map_or("", |(f, _)| f).to_owned(),
        dependencies: doc
            .get("dependencies")
            .map(|d| {
                d.items()
                    .iter()
                    .filter_map(|i| {
                        Some((
                            i.str_at(&["name"])?.to_owned(),
                            i.str_at(&["repository"]).unwrap_or("").to_owned(),
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default(),
    })
}

/// The Kustomize file of `dir`.
fn kustomization_file(dir: &Path) -> Option<PathBuf> {
    KUSTOMIZATION
        .iter()
        .map(|n| dir.join(n))
        .find(|p| p.is_file())
}

/// The folders a tree includes: its resources and components that are
/// folders, and those its Flux `Kustomization`s point at, also when a
/// patch changes where.
fn includes(repos: &Repos, dir: &Path) -> Vec<PathBuf> {
    let Some(doc) = kustomization_file(dir)
        .and_then(|f| std::fs::read_to_string(f).ok())
        .and_then(|t| yaml::doc(&t))
    else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries(&doc) {
        if remote(&entry) {
            continue;
        }
        let path = normal(&dir.join(&entry));
        if path.is_dir() {
            out.push(path);
        } else if path.is_file()
            && let Ok(text) = std::fs::read_to_string(&path)
        {
            for d in yaml::docs(&text) {
                if is_flux_kustomization(&d)
                    && let Some(target) = d.str_at(&["spec", "path"])
                    && let Some(found) = flux_path(repos, &path, target)
                {
                    out.push(found);
                }
            }
        }
    }
    for (text, _) in patch_texts(&doc, dir) {
        for d in yaml::docs(&text) {
            for op in d.items() {
                if op.str_at(&["path"]) == Some("/spec/path")
                    && let Some(target) = op.str_at(&["value"])
                    && let Some(found) = flux_path(repos, dir, target)
                {
                    out.push(found);
                }
            }
            if let Some(target) = d.str_at(&["spec", "path"])
                && let Some(found) = flux_path(repos, dir, target)
            {
                out.push(found);
            }
        }
    }
    out
}

/// A tree's resources and components, as written.
fn entries(doc: &Node) -> Vec<String> {
    let mut out = doc.strs_at(&["resources"]);
    out.extend(doc.strs_at(&["bases"]));
    out.extend(doc.strs_at(&["components"]));
    out
}

fn remote(entry: &str) -> bool {
    entry.contains("://") || entry.starts_with("github.com/") || entry.starts_with("git@")
}

fn is_flux_kustomization(doc: &Node) -> bool {
    doc.str_at(&["kind"]) == Some("Kustomization")
        && doc
            .str_at(&["apiVersion"])
            .is_some_and(|v| v.starts_with("kustomize.toolkit.fluxcd.io"))
}

/// The folder a Flux `Kustomization` read from `from` points at: its path
/// is from its source's root, taken to be the repository that holds it,
/// else any repository that has that folder.
fn flux_path(repos: &Repos, from: &Path, target: &str) -> Option<PathBuf> {
    let target = target.trim_start_matches("./");
    let own = repos.locate(from).map(|(r, _)| r.root.clone());
    own.into_iter()
        .chain(repos.repos.iter().map(|r| r.root.clone()))
        .map(|root| normal(&root.join(target)))
        .find(|p| p.is_dir())
}

/// A patch's text and where it is: from its file, or written in the tree's
/// own file (then its lines are offset from the tree's).
fn patch_texts(doc: &Node, dir: &Path) -> Vec<(String, PatchAt)> {
    let mut out = Vec::new();
    let mut add = |node: &Node| {
        if let Some(path) = node.str_at(&["path"]).or_else(|| node.str()).filter(|p| {
            !p.contains('\n')
                && (p.ends_with(".yaml") || p.ends_with(".yml") || p.ends_with(".json"))
        }) {
            let file = normal(&dir.join(path));
            if let Ok(text) = std::fs::read_to_string(&file) {
                out.push((text, PatchAt::File(file)));
            }
        } else if let Some(inline) = node.get("patch").or(Some(node)).and_then(Node::str) {
            let line = node.get("patch").map_or(node.line, |p| p.line);
            out.push((inline.to_owned(), PatchAt::Inline(line)));
        }
    };
    for key in ["patches", "patchesStrategicMerge", "patchesJson6902"] {
        if let Some(list) = doc.get(key) {
            for item in list.items() {
                add(item);
            }
        }
    }
    out
}

/// Where a patch's text is.
#[derive(Debug, Clone)]
enum PatchAt {
    File(PathBuf),
    /// Written in the tree's own file, its first line on this one.
    Inline(usize),
}

/// An object read, the layers that set its Helm values, and where.
#[derive(Debug, Clone)]
struct Doc {
    node: Node,
    place: Place,
    layers: Vec<Layer>,
}

/// One layer of a release's values: those a file sets, and those it takes
/// away.
#[derive(Debug, Clone)]
struct Layer {
    place: Place,
    values: Option<Node>,
    removed: Vec<Vec<String>>,
}

/// A file a `configMapGenerator` makes a config map of.
#[derive(Debug, Clone)]
struct Generated {
    name: String,
    files: Vec<(String, Place, String)>,
}

/// Loads trees, each once.
struct Loader<'a> {
    repos: &'a Repos,
    visited: HashSet<PathBuf>,
    encrypted: Vec<Place>,
    notes: Vec<String>,
    generated: Vec<Generated>,
}

impl Loader<'_> {
    fn place(&self, path: &Path, line: usize) -> Option<Place> {
        let (repo, rel) = self.repos.locate(path)?;
        Some(Place::new(&repo.name, &rel, line))
    }

    /// The objects of the file `path`, encrypted ones named only.
    fn file(&mut self, path: &Path) -> Vec<Doc> {
        let Some(place) = self.place(path, 0) else {
            return Vec::new();
        };
        if encrypted_name(&place.path) {
            self.encrypted.push(place);
            return Vec::new();
        }
        let Ok(text) = std::fs::read_to_string(path) else {
            self.notes.push(format!("{place} could not be read"));
            return Vec::new();
        };
        let docs = yaml::docs(&text);
        if docs.iter().any(|d| d.get("sops").is_some()) {
            self.encrypted.push(place);
            return Vec::new();
        }
        docs.into_iter()
            .filter(|d| d.get("kind").is_some())
            .map(|node| {
                let place = place.at(node.line);
                let layers = node
                    .at(&["spec", "values"])
                    .map(|values| Layer {
                        place: place.at(values.line),
                        values: Some(values.clone()),
                        removed: Vec::new(),
                    })
                    .into_iter()
                    .collect();
                Doc {
                    node,
                    place,
                    layers,
                }
            })
            .collect()
    }

    /// The objects of the folder `dir`: as its tree says, or every YAML
    /// file in it when it has none, as Flux does.
    fn tree(&mut self, dir: &Path) -> Vec<Doc> {
        let dir = normal(dir);
        if !self.visited.insert(dir.clone()) {
            return Vec::new();
        }
        let Some(file) = kustomization_file(&dir) else {
            let mut docs = Vec::new();
            for (path, rel) in files(&dir) {
                if rel.ends_with(".yaml") || rel.ends_with(".yml") {
                    docs.extend(self.file(&path));
                }
            }
            return docs;
        };
        let Some(doc) = std::fs::read_to_string(&file)
            .ok()
            .and_then(|t| yaml::doc(&t))
        else {
            if let Some(place) = self.place(&file, 0) {
                self.notes.push(format!("{place} is not valid YAML"));
            }
            return Vec::new();
        };
        let mut docs = Vec::new();
        for entry in entries(&doc) {
            if remote(&entry) {
                if let Some(place) = self.place(&file, 0) {
                    self.notes
                        .push(format!("{place} includes {entry}, which is not read"));
                }
                continue;
            }
            let path = normal(&dir.join(&entry));
            if path.is_dir() {
                docs.extend(self.tree(&path));
            } else if path.is_file() {
                docs.extend(self.file(&path));
            } else if let Some(place) = self.place(&file, 0) {
                self.notes
                    .push(format!("{place} includes {entry}, which is not there"));
            }
        }
        docs.extend(self.generators(&doc, &dir, &file));
        for (text, at) in patch_texts(&doc, &dir) {
            let (place, offset) = match &at {
                PatchAt::File(path) => (self.place(path, 0), 0),
                // A block scalar's mark is on its first line of text.
                PatchAt::Inline(line) => (self.place(&file, 0), line.saturating_sub(1)),
            };
            let Some(place) = place else {
                continue;
            };
            if encrypted_name(&place.path) {
                self.encrypted.push(place);
                continue;
            }
            let target = doc
                .get("patches")
                .into_iter()
                .chain(doc.get("patchesJson6902"))
                .flat_map(Node::items)
                .find(|item| match &at {
                    PatchAt::File(path) => item
                        .str_at(&["path"])
                        .is_some_and(|p| normal(&dir.join(p)) == *path),
                    PatchAt::Inline(line) => item.get("patch").is_some_and(|p| p.line == *line),
                })
                .and_then(|item| item.get("target"))
                .cloned();
            apply_patch(&mut docs, &text, target.as_ref(), &place, offset);
        }
        let namespace = doc.str_at(&["namespace"]);
        let prefix = doc.str_at(&["namePrefix"]).unwrap_or("");
        let suffix = doc.str_at(&["nameSuffix"]).unwrap_or("");
        for d in &mut docs {
            if let Some(namespace) = namespace
                && !cluster_wide(d.node.str_at(&["kind"]).unwrap_or(""))
            {
                d.node.set(
                    &["metadata".into(), "namespace".into()],
                    Node::scalar(namespace, d.node.line),
                );
            }
            if !prefix.is_empty() || !suffix.is_empty() {
                let name = d
                    .node
                    .str_at(&["metadata", "name"])
                    .unwrap_or("")
                    .to_owned();
                d.node.set(
                    &["metadata".into(), "name".into()],
                    Node::scalar(format!("{prefix}{name}{suffix}"), d.node.line),
                );
            }
        }
        docs
    }

    /// The config maps and secrets a tree generates; a secret's keys only.
    fn generators(&mut self, doc: &Node, dir: &Path, file: &Path) -> Vec<Doc> {
        let mut docs = Vec::new();
        for (key, kind) in [
            ("configMapGenerator", "ConfigMap"),
            ("secretGenerator", "Secret"),
        ] {
            let Some(list) = doc.get(key) else {
                continue;
            };
            for item in list.items() {
                let Some(name) = item.str_at(&["name"]) else {
                    continue;
                };
                let Some(place) = self.place(file, item.line) else {
                    continue;
                };
                let mut data: Vec<(String, Node)> = Vec::new();
                let mut generated = Generated {
                    name: name.to_owned(),
                    files: Vec::new(),
                };
                for literal in item.strs_at(&["literals"]) {
                    if let Some((k, v)) = literal.split_once('=') {
                        let shown = if kind == "Secret" {
                            secret::HIDDEN.to_owned()
                        } else {
                            secret::keep(k, v)
                        };
                        data.push((k.to_owned(), Node::scalar(shown, item.line)));
                    }
                }
                for entry in item.strs_at(&["files"]) {
                    let (k, path) = entry.split_once('=').map_or_else(
                        || (entry.rsplit('/').next().unwrap_or(&entry), entry.as_str()),
                        |(k, p)| (k, p),
                    );
                    let path = normal(&dir.join(path));
                    let Some(at) = self.place(&path, 0) else {
                        continue;
                    };
                    if kind == "Secret" || encrypted_name(&at.path) {
                        if encrypted_name(&at.path) {
                            self.encrypted.push(at.clone());
                        }
                        data.push((k.to_owned(), Node::scalar(secret::HIDDEN, item.line)));
                        continue;
                    }
                    data.push((k.to_owned(), Node::scalar(at.path.clone(), item.line)));
                    if let Ok(text) = std::fs::read_to_string(&path) {
                        generated.files.push((k.to_owned(), at, text));
                    }
                }
                if kind == "ConfigMap" {
                    self.generated.push(generated);
                }
                let mut node = Node {
                    value: Value::Map(Vec::new()),
                    line: item.line,
                };
                node.set(&["kind".into()], Node::scalar(kind, item.line));
                node.set(
                    &["metadata".into(), "name".into()],
                    Node::scalar(name, item.line),
                );
                node.set(
                    &["data".into()],
                    Node {
                        value: Value::Map(data),
                        line: item.line,
                    },
                );
                docs.push(Doc {
                    node,
                    place,
                    layers: Vec::new(),
                });
            }
        }
        docs
    }
}

/// Kinds a namespace does not apply to.
fn cluster_wide(kind: &str) -> bool {
    matches!(
        kind,
        "Namespace"
            | "ClusterRole"
            | "ClusterRoleBinding"
            | "CustomResourceDefinition"
            | "StorageClass"
            | "PersistentVolume"
            | "ClusterIssuer"
            | "PriorityClass"
    )
}

/// Every line of `node` moved down by `offset`.
fn shift(node: &mut Node, offset: usize) {
    node.line += offset;
    match &mut node.value {
        Value::Map(entries) => entries.iter_mut().for_each(|(_, v)| shift(v, offset)),
        Value::Seq(items) => items.iter_mut().for_each(|v| shift(v, offset)),
        _ => {}
    }
}

/// Whether an object is one a patch's target selects.
fn selects(target: &Node, doc: &Node) -> bool {
    let field = |path: &[&str]| doc.str_at(path).unwrap_or("");
    let matches = |pattern: &str, value: &str| {
        Regex::new(&format!("^(?:{pattern})$")).map_or(pattern == value, |re| re.is_match(value))
    };
    if let Some(kind) = target.str_at(&["kind"])
        && !matches(kind, field(&["kind"]))
    {
        return false;
    }
    if let Some(name) = target.str_at(&["name"])
        && !matches(name, field(&["metadata", "name"]))
    {
        return false;
    }
    if let Some(namespace) = target.str_at(&["namespace"])
        && !matches(namespace, field(&["metadata", "namespace"]))
    {
        return false;
    }
    if let Some(selector) = target.str_at(&["labelSelector"]) {
        for pair in selector.split(',') {
            let Some((k, v)) = pair.split_once('=') else {
                continue;
            };
            if doc
                .at(&["metadata", "labels", k.trim()])
                .and_then(Node::str)
                != Some(v.trim())
            {
                return false;
            }
        }
    }
    true
}

/// Applies a patch, strategic merge or JSON 6902, to the objects it
/// selects, and records the Helm values it sets as a layer of each.
fn apply_patch(docs: &mut [Doc], text: &str, target: Option<&Node>, place: &Place, offset: usize) {
    for mut patch in yaml::docs(text) {
        shift(&mut patch, offset);
        let operations = matches!(&patch.value, Value::Seq(items) if items.iter().any(|i| i.get("op").is_some()));
        if operations {
            let Some(target) = target else {
                continue;
            };
            for doc in docs.iter_mut().filter(|d| selects(target, &d.node)) {
                for op in patch.items() {
                    json_operation(doc, op, place);
                }
            }
            continue;
        }
        let selector = target.cloned().unwrap_or_else(|| {
            let mut selector = Node {
                value: Value::Map(Vec::new()),
                line: 0,
            };
            for (key, path) in [("kind", &["kind"][..]), ("name", &["metadata", "name"][..])] {
                if let Some(value) = patch.str_at(path) {
                    selector.set(&[key.into()], Node::scalar(regex::escape(value), 0));
                }
            }
            selector
        });
        for doc in docs.iter_mut().filter(|d| selects(&selector, &d.node)) {
            merge(&mut doc.node, &patch);
            if let Some(values) = patch.at(&["spec", "values"]) {
                doc.layers.push(Layer {
                    place: place.at(values.line),
                    values: Some(values.clone()),
                    removed: Vec::new(),
                });
            }
        }
    }
}

/// One JSON 6902 operation on an object.
fn json_operation(doc: &mut Doc, op: &Node, place: &Place) {
    let Some(path) = op.str_at(&["path"]) else {
        return;
    };
    let tokens: Vec<String> = path
        .split('/')
        .skip(1)
        .map(|t| t.replace("~1", "/").replace("~0", "~"))
        .collect();
    let values = tokens.len() >= 2 && tokens[0] == "spec" && tokens[1] == "values";
    match op.str_at(&["op"]) {
        Some("add" | "replace") => {
            let Some(value) = op.get("value") else {
                return;
            };
            doc.node.set(&tokens, value.clone());
            if values {
                let mut layer = Node {
                    value: Value::Map(Vec::new()),
                    line: op.line,
                };
                layer.set(&tokens[2..], value.clone());
                doc.layers.push(Layer {
                    place: place.at(value.line),
                    values: Some(layer),
                    removed: Vec::new(),
                });
            }
        }
        Some("remove") => {
            doc.node.remove(&tokens);
            if values {
                doc.layers.push(Layer {
                    place: place.at(op.line),
                    values: None,
                    removed: vec![tokens[2..].to_vec()],
                });
            }
        }
        _ => {}
    }
}

/// `over` merged into `base`: mappings key by key, anything else replaced.
pub(crate) fn merge(base: &mut Node, over: &Node) {
    match (&mut base.value, &over.value) {
        (Value::Map(entries), Value::Map(others)) => {
            for (key, value) in others {
                match entries.iter_mut().find(|(k, _)| k == key) {
                    Some((_, existing)) => merge(existing, value),
                    None => entries.push((key.clone(), value.clone())),
                }
            }
        }
        _ => *base = over.clone(),
    }
}

/// Reads the environment whose tree is at `dir`.
fn environment(
    repos: &Repos,
    dir: &Path,
    repo: &str,
    folder: &str,
    sources: &[(Node, Place)],
    charts: &[Chart],
) -> Environment {
    let mut loader = Loader {
        repos,
        visited: HashSet::new(),
        encrypted: Vec::new(),
        notes: Vec::new(),
        generated: Vec::new(),
    };
    let mut parts = vec![Part {
        repo: repo.to_owned(),
        folder: folder.to_owned(),
    }];
    let mut docs = loader.tree(dir);
    // Follow Flux's Kustomizations, as patched, to the trees they build.
    let mut next = 0;
    while next < docs.len() {
        if is_flux_kustomization(&docs[next].node)
            && let Some(target) = docs[next].node.str_at(&["spec", "path"]).map(str::to_owned)
            && let Some(from) = repos.file(&docs[next].place)
            && let Some(found) = flux_path(repos, &from, &target)
            && let Some((r, rel)) = repos.locate(&found)
        {
            let part = Part {
                repo: r.name.clone(),
                folder: rel,
            };
            if !parts.contains(&part) {
                parts.push(part);
                let more = loader.tree(&found);
                docs.extend(more);
            }
        }
        next += 1;
    }

    // Several pieces of one release are one release.
    let mut merged: Vec<Doc> = Vec::new();
    for doc in docs {
        let key = |d: &Node| {
            (
                d.str_at(&["kind"]).map(str::to_owned),
                d.str_at(&["metadata", "name"]).map(str::to_owned),
                d.str_at(&["metadata", "namespace"]).map(str::to_owned),
            )
        };
        let release = doc.node.str_at(&["kind"]) == Some("HelmRelease");
        match merged
            .iter_mut()
            .find(|m| release && key(&m.node) == key(&doc.node))
        {
            Some(existing) => {
                merge(&mut existing.node, &doc.node);
                existing.layers.extend(doc.layers);
            }
            None => merged.push(doc),
        }
    }

    let mut all_sources: Vec<(&Node, &Place)> = merged
        .iter()
        .filter(|d| {
            d.node
                .str_at(&["kind"])
                .is_some_and(|k| k.ends_with("Repository"))
        })
        .map(|d| (&d.node, &d.place))
        .collect();
    all_sources.extend(sources.iter().map(|(n, p)| (n, p)));

    let mut releases = Vec::new();
    let mut manifests = Vec::new();
    for doc in &merged {
        let kind = doc.node.str_at(&["kind"]).unwrap_or("");
        if kind == "HelmRelease" {
            releases.push(release(
                repos,
                doc,
                &merged,
                &loader.generated,
                &all_sources,
                charts,
            ));
        } else {
            manifests.push(Manifest {
                kind: kind.to_owned(),
                name: doc
                    .node
                    .str_at(&["metadata", "name"])
                    .unwrap_or("")
                    .to_owned(),
                namespace: doc
                    .node
                    .str_at(&["metadata", "namespace"])
                    .map(str::to_owned),
                place: doc.place.clone(),
            });
        }
    }
    let mut seen: HashMap<String, usize> = HashMap::new();
    for r in &releases {
        *seen.entry(r.name.clone()).or_default() += 1;
    }
    for r in &mut releases {
        if seen[&r.name] > 1 {
            r.label = format!(
                "{} ({})",
                r.name,
                r.namespace.as_deref().unwrap_or("default")
            );
        }
    }
    loader.encrypted.sort();
    loader.encrypted.dedup();
    Environment {
        name: if folder.is_empty() {
            repo.to_owned()
        } else {
            folder.to_owned()
        },
        chosen: false,
        parts,
        releases,
        manifests,
        encrypted: loader.encrypted,
        notes: loader.notes,
    }
}

/// A `HelmRelease` read: its chart, where Flux takes it from, and its
/// settings layer by layer.
fn release(
    repos: &Repos,
    doc: &Doc,
    docs: &[Doc],
    generated: &[Generated],
    sources: &[(&Node, &Place)],
    charts: &[Chart],
) -> Release {
    let node = &doc.node;
    let name = node.str_at(&["metadata", "name"]).unwrap_or("").to_owned();
    let namespace = node
        .str_at(&["spec", "targetNamespace"])
        .or_else(|| node.str_at(&["metadata", "namespace"]))
        .map(str::to_owned);
    let mut notes = Vec::new();

    // The chart: by a reference to a source, or by its own spec.
    let (mut chart_name, mut chart_path, mut source) = (None, None, None);
    let find_source = |kind: &str, name: &str| -> Option<ChartSource> {
        let (s, _) = sources.iter().find(|(s, _)| {
            s.str_at(&["kind"]) == Some(kind) && s.str_at(&["metadata", "name"]) == Some(name)
        })?;
        let reference = ["tag", "semver", "branch", "digest", "commit"]
            .iter()
            .find_map(|k| s.str_at(&["spec", "ref", k]))
            .map(str::to_owned);
        Some(ChartSource {
            kind: kind.to_owned(),
            name: name.to_owned(),
            url: s.str_at(&["spec", "url"]).map(str::to_owned),
            reference,
        })
    };
    if let Some(reference) = node.at(&["spec", "chartRef"]) {
        let kind = reference.str_at(&["kind"]).unwrap_or("OCIRepository");
        let source_name = reference.str_at(&["name"]).unwrap_or("");
        source = find_source(kind, source_name);
        chart_name = source
            .as_ref()
            .and_then(|s| s.url.as_deref())
            .and_then(|u| u.trim_end_matches('/').rsplit('/').next())
            .map(str::to_owned);
        if source.is_none() {
            notes.push(format!(
                "Its chart's source, {kind} {source_name}, was not found"
            ));
        }
    } else if let Some(spec) = node.at(&["spec", "chart", "spec"]) {
        let chart = spec.str_at(&["chart"]).unwrap_or("");
        let kind = spec
            .str_at(&["sourceRef", "kind"])
            .unwrap_or("HelmRepository");
        let source_name = spec.str_at(&["sourceRef", "name"]).unwrap_or("");
        source = find_source(kind, source_name).map(|mut s| {
            if s.reference.is_none() {
                s.reference = spec.str_at(&["version"]).map(str::to_owned);
            }
            s
        });
        if chart.contains('/') {
            chart_path = Some(chart.trim_start_matches("./").to_owned());
        }
        chart_name = chart
            .trim_end_matches('/')
            .rsplit('/')
            .next()
            .map(str::to_owned);
    }
    let release_repo = doc.place.repo.as_str();
    let chart = chart_path
        .as_ref()
        .and_then(|path| {
            charts
                .iter()
                .enumerate()
                .filter(|(_, c)| same_folder(&c.folder, path))
                .min_by_key(|(_, c)| c.repo != release_repo)
                .map(|(i, _)| i)
        })
        .or_else(|| {
            let name = chart_name.as_deref()?;
            charts
                .iter()
                .enumerate()
                .filter(|(_, c)| c.name == name)
                .min_by_key(|(_, c)| {
                    (
                        c.repo != release_repo,
                        !repos.get(&c.repo).is_some_and(|r| r.own),
                    )
                })
                .map(|(i, _)| i)
        });
    if chart.is_none()
        && let Some(name) = &chart_name
    {
        notes.push(format!(
            "Its chart, {name}, is in none of the repositories read"
        ));
    }

    // The layers, lowest first: the chart's defaults, the values taken
    // from config maps, then the release's own values and its patches.
    let mut layers: Vec<Layer> = Vec::new();
    if let Some(c) = chart.and_then(|c| charts.get(c)) {
        let path = format!(
            "{}{}values.yaml",
            c.folder,
            if c.folder.is_empty() { "" } else { "/" }
        );
        if let Some(defaults) = repos
            .get(&c.repo)
            .and_then(|r| std::fs::read_to_string(r.root.join(&path)).ok())
            .and_then(|t| yaml::doc(&t))
        {
            layers.push(Layer {
                place: Place::new(&c.repo, &path, defaults.line),
                values: Some(defaults),
                removed: Vec::new(),
            });
        }
    }
    let mut overrides: Vec<Layer> = Vec::new();
    for from in node
        .at(&["spec", "valuesFrom"])
        .map_or(&[][..], Node::items)
    {
        let kind = from.str_at(&["kind"]).unwrap_or("ConfigMap");
        let map_name = from.str_at(&["name"]).unwrap_or("");
        let key = from.str_at(&["valuesKey"]).unwrap_or("values.yaml");
        if kind != "ConfigMap" {
            notes.push(format!(
                "Its values from the {kind} {map_name} are not read"
            ));
            continue;
        }
        let found = generated
            .iter()
            .filter(|g| map_name.starts_with(&g.name) || g.name == map_name)
            .flat_map(|g| g.files.iter())
            .find(|(k, _, _)| k == key)
            .and_then(|(_, place, text)| Some((place.clone(), yaml::doc(text)?)))
            .or_else(|| {
                // A config map written out, its values a string in it.
                let d = docs.iter().find(|d| {
                    d.node.str_at(&["kind"]) == Some("ConfigMap")
                        && d.node.str_at(&["metadata", "name"]) == Some(map_name)
                })?;
                let text = d.node.at(&["data", key])?;
                let mut values = yaml::doc(text.str()?)?;
                shift(&mut values, text.line);
                Some((d.place.at(values.line), values))
            });
        match found {
            Some((place, mut values)) => {
                if let Some(target) = from.str_at(&["targetPath"]) {
                    let mut wrapped = Node {
                        value: Value::Map(Vec::new()),
                        line: values.line,
                    };
                    let path: Vec<String> = target.split('.').map(str::to_owned).collect();
                    wrapped.set(&path, values);
                    values = wrapped;
                }
                overrides.push(Layer {
                    place: place.at(values.line.max(1)),
                    values: Some(values),
                    removed: Vec::new(),
                });
            }
            None => notes.push(format!(
                "Its values from the config map {map_name} ({key}) were not found"
            )),
        }
    }
    overrides.extend(doc.layers.iter().cloned());
    let mut values = Node {
        value: Value::Map(Vec::new()),
        line: 0,
    };
    for layer in &overrides {
        if let Some(v) = &layer.values {
            merge(&mut values, v);
        }
        for path in &layer.removed {
            values.remove(path);
        }
    }
    layers.extend(overrides);

    Release {
        label: name.clone(),
        name,
        namespace,
        chart_name,
        chart,
        source,
        settings: layered(&layers),
        values: values.to_json(),
        place: doc.place.clone(),
        notes,
    }
}

/// The settings left by `layers`, lowest first, each with what it
/// replaced. A list set again replaces the whole list, as Helm does.
fn layered(layers: &[Layer]) -> Vec<Setting> {
    let mut settings: BTreeMap<String, Setting> = BTreeMap::new();
    let mut order: Vec<String> = Vec::new();
    for layer in layers {
        let mut leaves = Vec::new();
        let mut lists = Vec::new();
        if let Some(values) = &layer.values {
            flatten(values, "", &mut leaves, &mut lists);
        }
        let mut gone: Vec<String> = lists.iter().map(|l| format!("{l}[")).collect();
        gone.extend(layer.removed.iter().map(|p| p.join(".")));
        for prefix in &gone {
            let fresh: HashSet<&str> = leaves.iter().map(|(k, _, _)| k.as_str()).collect();
            settings.retain(|k, _| {
                !(k.starts_with(prefix.as_str())
                    || k == prefix
                    || k.starts_with(&format!("{prefix}.")))
                    || fresh.contains(k.as_str())
            });
        }
        for (key, value, line) in leaves {
            let value = secret::keep(key.rsplit(['.', '[']).next().unwrap_or(&key), &value);
            let place = layer.place.at(line);
            match settings.get_mut(&key) {
                Some(existing) => {
                    if existing.value == value && existing.place == place {
                        continue;
                    }
                    let earlier = Earlier {
                        value: std::mem::replace(&mut existing.value, value),
                        place: std::mem::replace(&mut existing.place, place),
                    };
                    existing.replaced.insert(0, earlier);
                }
                None => {
                    order.push(key.clone());
                    settings.insert(
                        key.clone(),
                        Setting {
                            key,
                            value,
                            place,
                            replaced: Vec::new(),
                        },
                    );
                }
            }
        }
    }
    order.dedup();
    let mut out: Vec<Setting> = settings.into_values().collect();
    out.sort_by(|a, b| a.key.cmp(&b.key));
    out
}

/// The leaves of `node` under `prefix`, with their lines, and the lists.
fn flatten(
    node: &Node,
    prefix: &str,
    leaves: &mut Vec<(String, String, usize)>,
    lists: &mut Vec<String>,
) {
    let join = |key: &str| {
        if prefix.is_empty() {
            key.to_owned()
        } else {
            format!("{prefix}.{key}")
        }
    };
    match &node.value {
        Value::Map(entries) if entries.is_empty() => {
            if !prefix.is_empty() {
                leaves.push((prefix.to_owned(), "{}".to_owned(), node.line));
            }
        }
        Value::Map(entries) => {
            for (key, value) in entries {
                flatten(value, &join(key), leaves, lists);
            }
            // `{name: API_TOKEN, value: …}`, as containers' variables are
            // written: the name says whether the value is a secret.
            if let (Some(name), Some(_)) = (node.str_at(&["name"]), node.str_at(&["value"]))
                && secret::secret_name(name)
                && let Some(leaf) = leaves.iter_mut().rfind(|l| l.0 == join("value"))
            {
                leaf.1 = secret::keep(name, &leaf.1);
            }
        }
        Value::Seq(items) => {
            lists.push(prefix.to_owned());
            if items.is_empty() {
                leaves.push((prefix.to_owned(), "[]".to_owned(), node.line));
            }
            for (i, item) in items.iter().enumerate() {
                flatten(item, &format!("{prefix}[{i}]"), leaves, lists);
            }
        }
        Value::Scalar { text, .. } => leaves.push((prefix.to_owned(), text.clone(), node.line)),
        Value::Null => leaves.push((prefix.to_owned(), "null".to_owned(), node.line)),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn write(root: &Path, path: &str, text: &str) {
        let path = root.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    /// A project with its chart, and a deployment repository: a base, two
    /// overlays and a Flux tree that points at one of them.
    pub(crate) fn project(root: &Path) -> Repos {
        let app = root.join("app");
        let ops = root.join("ops");
        write(
            &app,
            "charts/api/Chart.yaml",
            "apiVersion: v2\nname: api\nversion: 1.4.0\ndependencies:\n  - name: common\n    version: 0.1.0\n    repository: file://../common\n",
        );
        write(
            &app,
            "charts/api/values.yaml",
            "replicaCount: 1\nimage:\n  repository: registry.example.com/api\n  tag: latest\nenv:\n  - name: LOG\n    value: info\ndatabase:\n  password: changeme\n",
        );
        write(
            &app,
            "charts/common/Chart.yaml",
            "apiVersion: v2\nname: common\nversion: 0.1.0\ntype: library\n",
        );
        write(
            &ops,
            "apps/base/kustomization.yaml",
            "resources:\n  - api.yaml\n  - source.yaml\n",
        );
        write(
            &ops,
            "apps/base/api.yaml",
            "apiVersion: helm.toolkit.fluxcd.io/v2\nkind: HelmRelease\nmetadata:\n  name: api\nspec:\n  chartRef:\n    kind: OCIRepository\n    name: api-chart\n  valuesFrom:\n    - kind: ConfigMap\n      name: api-values\n  values:\n    image:\n      tag: \"1.0.0\"\n",
        );
        write(
            &ops,
            "apps/base/source.yaml",
            "apiVersion: source.toolkit.fluxcd.io/v1beta2\nkind: OCIRepository\nmetadata:\n  name: api-chart\nspec:\n  url: oci://registry.example.com/charts/api\n  ref:\n    tag: 1.4.0\n",
        );
        write(
            &ops,
            "apps/prod/kustomization.yaml",
            "namespace: shop\nresources:\n  - ../base\n  - secrets.enc.yaml\nconfigMapGenerator:\n  - name: api-values\n    files:\n      - values.yaml=api-values.yaml\npatches:\n  - path: replicas.yaml\n  - target:\n      kind: HelmRelease\n      name: api\n    patch: |\n      - op: replace\n        path: /spec/values/image/tag\n        value: \"1.2.0\"\n",
        );
        write(
            &ops,
            "apps/prod/secrets.enc.yaml",
            "apiVersion: v1\nkind: Secret\nmetadata:\n  name: keys\ndata:\n  key: ENC[AES256_GCM,data:abc]\nsops:\n  version: 3.9.0\n",
        );
        write(
            &ops,
            "apps/prod/api-values.yaml",
            "env:\n  - name: LOG\n    value: warn\n  - name: API_TOKEN\n    value: abcdef123456\n",
        );
        write(
            &ops,
            "apps/prod/replicas.yaml",
            "apiVersion: helm.toolkit.fluxcd.io/v2\nkind: HelmRelease\nmetadata:\n  name: api\nspec:\n  values:\n    replicaCount: 3\n",
        );
        write(
            &ops,
            "apps/staging/kustomization.yaml",
            "namespace: shop-staging\nresources:\n  - ../base\n",
        );
        write(
            &ops,
            "clusters/prod/kustomization.yaml",
            "resources:\n  - apps.yaml\n",
        );
        write(
            &ops,
            "clusters/prod/apps.yaml",
            "apiVersion: kustomize.toolkit.fluxcd.io/v1\nkind: Kustomization\nmetadata:\n  name: apps\nspec:\n  path: ./apps/prod\n  sourceRef:\n    kind: GitRepository\n    name: flux-system\n",
        );
        Repos::new(&app, &[ops])
    }

    #[test]
    fn environments_releases_and_their_layered_settings() {
        let dir = tempfile::tempdir().unwrap();
        let repos = project(dir.path());
        let named = [NamedEnvironment {
            name: "prod".into(),
            path: "clusters/prod".into(),
        }];
        let deploy = deploy(&repos, &named);
        let names: Vec<&str> = deploy
            .environments
            .iter()
            .map(|e| e.name.as_str())
            .collect();
        // The chosen one first; apps/prod is included by Flux, the base by
        // both overlays.
        assert_eq!(names, ["prod", "apps/staging"]);
        assert_eq!(deploy.charts.len(), 2);
        let prod = &deploy.environments[0];
        assert!(prod.chosen);
        assert_eq!(
            prod.parts
                .iter()
                .map(|p| p.folder.as_str())
                .collect::<Vec<_>>(),
            ["clusters/prod", "apps/prod"]
        );
        assert_eq!(
            prod.encrypted,
            [Place::new("ops", "apps/prod/secrets.enc.yaml", 0)]
        );
        let api = &prod.releases[0];
        assert_eq!(api.namespace.as_deref(), Some("shop"));
        assert_eq!(api.chart_name.as_deref(), Some("api"));
        assert_eq!(
            api.source.as_ref().unwrap().reference.as_deref(),
            Some("1.4.0")
        );
        assert_eq!(deploy.charts[api.chart.unwrap()].folder, "charts/api");
        let setting = |key: &str| api.settings.iter().find(|s| s.key == key).unwrap();

        // Default, base, then the JSON patch.
        let tag = setting("image.tag");
        assert_eq!(tag.value, "1.2.0");
        assert_eq!(tag.place.path, "apps/prod/kustomization.yaml");
        assert_eq!(tag.place.line, 17);
        let earlier: Vec<(&str, &str)> = tag
            .replaced
            .iter()
            .map(|e| (e.value.as_str(), e.place.path.as_str()))
            .collect();
        assert_eq!(
            earlier,
            [
                ("1.0.0", "apps/base/api.yaml"),
                ("latest", "charts/api/values.yaml")
            ]
        );
        let replicas = setting("replicaCount");
        assert_eq!((replicas.value.as_str(), replicas.place.line), ("3", 7));
        // The config map's list replaces the chart's; a token is hidden.
        assert_eq!(setting("env[0].value").value, "warn");
        assert_eq!(setting("env[1].value").value, secret::HIDDEN);
        assert_eq!(setting("database.password").value, secret::HIDDEN);
        assert_eq!(api.values["image"]["tag"], serde_json::json!("1.2.0"));
        assert_eq!(api.values["replicaCount"], serde_json::json!(3));
        assert!(api.values.get("database").is_none());

        let staging = &deploy.environments[1];
        assert!(!staging.chosen);
        assert_eq!(
            staging.releases[0].namespace.as_deref(),
            Some("shop-staging")
        );
        assert!(
            staging.releases[0]
                .notes
                .iter()
                .any(|n| n.contains("api-values"))
        );
    }

    #[test]
    fn a_patched_flux_path_is_followed_and_pieces_merge() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(
            root,
            "apps/a/kustomization.yaml",
            "resources:\n  - r.yaml\n",
        );
        write(
            root,
            "apps/a/r.yaml",
            "kind: HelmRelease\nmetadata:\n  name: web\n  namespace: w\nspec:\n  values:\n    a: 1\n",
        );
        write(
            root,
            "apps/b/kustomization.yaml",
            "resources:\n  - r.yaml\n  - r2.yaml\n",
        );
        write(
            root,
            "apps/b/r2.yaml",
            "kind: HelmRelease\nmetadata:\n  name: web\n  namespace: w\nspec:\n  values:\n    c: 3\n",
        );
        write(
            root,
            "apps/b/r.yaml",
            "kind: HelmRelease\nmetadata:\n  name: web\n  namespace: w\nspec:\n  values:\n    b: 2\n",
        );
        write(
            root,
            "clusters/x/kustomization.yaml",
            "resources:\n  - flux.yaml\npatches:\n  - target:\n      kind: Kustomization\n    patch: |\n      - op: replace\n        path: /spec/path\n        value: ./apps/b\n",
        );
        write(
            root,
            "clusters/x/flux.yaml",
            "apiVersion: kustomize.toolkit.fluxcd.io/v1\nkind: Kustomization\nmetadata:\n  name: apps\nspec:\n  path: ./apps/a\n",
        );
        write(
            root,
            "clusters/x/more/kustomization.yaml",
            "resources:\n  - ../../../apps/a\n",
        );
        let repos = Repos::new(root, &[]);
        let deploy = deploy(&repos, &[]);
        let x = deploy
            .environments
            .iter()
            .find(|e| e.name == "clusters/x")
            .unwrap();
        assert_eq!(x.parts[1].folder, "apps/b");
        assert_eq!(x.releases.len(), 1);
        assert_eq!(x.releases[0].values, serde_json::json!({"b": 2, "c": 3}));
        assert!(!deploy.environments.iter().any(|e| e.name == "apps/b"));
    }
}
