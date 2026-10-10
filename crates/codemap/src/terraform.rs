//! The cloud a project runs on, as its Terraform says: every resource,
//! data source and module of the project and of the linked repositories,
//! each module read at the ref it is pinned to, its variables' defaults
//! filled in. And the cloud identifiers the rendered environment names (a
//! role, a bucket, a database host, a secret, a user pool), each matched
//! to the blocks that configure it, with how good the match is.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::process::Command;
use std::sync::LazyLock;

use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::hcl::{self, Body, Hcl};
use crate::repos::{Place, Repos, files};

/// Words that name a stage, worth twice as much when places are compared.
const STAGES: [&str; 14] = [
    "dev",
    "development",
    "test",
    "qa",
    "staging",
    "stage",
    "stg",
    "preprod",
    "uat",
    "prod",
    "production",
    "sandbox",
    "demo",
    "live",
];

/// What a block is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BlockKind {
    /// A `resource`.
    Resource,
    /// A `data` source.
    Data,
    /// A `module` call.
    Module,
}

/// A block of Terraform.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TfBlock {
    /// A resource, a data source or a module.
    pub kind: BlockKind,
    /// The resource's type (`aws_s3_bucket`), or the module's source.
    pub what: String,
    /// Its name.
    pub name: String,
    /// Where it is.
    pub place: Place,
    /// Its folder, from its repository's root: the root module it is in.
    pub folder: String,
    pub(crate) body: Body,
}

/// A module read where its call pins it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Module {
    /// The call's source, as written.
    pub(crate) source: String,
    /// Where it was read: its repository and folder, and the ref.
    pub(crate) place: Place,
    pub(crate) reference: Option<String>,
    /// Its files.
    pub(crate) files: Vec<(String, Body)>,
}

/// The Terraform of all the repositories.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Terraform {
    /// Every block.
    pub blocks: Vec<TfBlock>,
    /// The modules the calls pin, read.
    pub(crate) modules: Vec<Module>,
    /// The variables files: their place and their values.
    pub(crate) vars: Vec<(Place, String, Body)>,
    /// Variables and locals declared, by folder.
    pub(crate) declared: Vec<(String, String, Body)>,
    /// What could not be read.
    pub notes: Vec<String>,
}

/// The kinds of cloud identifiers read from an environment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum CloudKind {
    /// An IAM role.
    Role,
    /// An object storage bucket.
    Bucket,
    /// A database, by its host.
    Database,
    /// A secret in a secret store.
    Secret,
    /// A user pool.
    Users,
}

/// A cloud identifier an environment names.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CloudId {
    /// What it names.
    pub kind: CloudKind,
    /// As written.
    pub value: String,
    /// The name it gives: a role's name, a database's identifier.
    pub name: String,
    /// The service that names it, by its label.
    pub service: Option<String>,
    /// Where it was read.
    pub place: Option<Place>,
}

/// How well an identifier matches a block.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Quality {
    /// The block names the identifier as it is.
    Exact,
    /// The block's name is a pattern (`${…}` standing for anything) the
    /// identifier fits.
    Pattern,
    /// They share a part of 8 characters or more, between separators.
    Part,
    /// The identifier only exists once applied: the nearest block that
    /// makes that kind of thing.
    Nearest,
}

/// Reads the Terraform of `repos`.
#[must_use]
pub fn terraform(repos: &Repos) -> Terraform {
    let mut terraform = Terraform::default();
    for repo in &repos.repos {
        for (path, rel) in files(&repo.root) {
            let is_tf = rel.ends_with(".tf");
            let is_vars = rel.ends_with(".tfvars");
            if !is_tf && !is_vars {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let body = hcl::parse(&text);
            let folder = rel.rsplit_once('/').map_or("", |(f, _)| f).to_owned();
            if is_vars {
                terraform
                    .vars
                    .push((Place::new(&repo.name, &rel, 0), folder, body));
                continue;
            }
            let mut declared = Body::default();
            for block in &body.blocks {
                let kind = match block.kind.as_str() {
                    "resource" => BlockKind::Resource,
                    "data" => BlockKind::Data,
                    "module" => BlockKind::Module,
                    "variable" | "locals" => {
                        declared.blocks.push(block.clone());
                        continue;
                    }
                    _ => continue,
                };
                let (what, name) = match kind {
                    BlockKind::Module => (
                        block.body.text("source").unwrap_or("").to_owned(),
                        block.labels.first().cloned().unwrap_or_default(),
                    ),
                    _ => (
                        block.labels.first().cloned().unwrap_or_default(),
                        block.labels.get(1).cloned().unwrap_or_default(),
                    ),
                };
                terraform.blocks.push(TfBlock {
                    kind,
                    what,
                    name,
                    place: Place::new(&repo.name, &rel, block.line),
                    folder: folder.clone(),
                    body: block.body.clone(),
                });
            }
            if !declared.blocks.is_empty() {
                terraform
                    .declared
                    .push((repo.name.clone(), folder, declared));
            }
        }
    }
    // Each module called, read once at its ref.
    let mut seen: HashSet<String> = HashSet::new();
    let calls: Vec<(String, Place)> = terraform
        .blocks
        .iter()
        .filter(|b| b.kind == BlockKind::Module)
        .map(|b| (b.what.clone(), b.place.clone()))
        .collect();
    for (source, place) in calls {
        let key = module_key(&source, &place);
        if !seen.insert(key) {
            continue;
        }
        match read_module(repos, &source, &place) {
            Ok(Some(module)) => terraform.modules.push(module),
            Ok(None) => {}
            Err(note) => terraform.notes.push(note),
        }
    }
    terraform
}

/// What tells two module calls apart: a local source is relative to its
/// caller.
fn module_key(source: &str, place: &Place) -> String {
    if source.starts_with('.') {
        format!("{}:{}:{source}", place.repo, place.path)
    } else {
        source.to_owned()
    }
}

/// A git source's repository name, folder in it and ref:
/// `git::https://host/org/modules.git//rds?ref=v1`.
fn git_source(source: &str) -> Option<(String, String, Option<String>)> {
    let source = source.strip_prefix("git::").unwrap_or(source);
    let (address, reference) = match source.split_once("?ref=") {
        Some((a, r)) => (a, Some(r.split('&').next().unwrap_or(r).to_owned())),
        None => (source.split('?').next().unwrap_or(source), None),
    };
    let without_scheme = address.split_once("://").map_or(address, |(_, rest)| rest);
    let (repository, folder) = match without_scheme.split_once("//") {
        Some((r, f)) => (r, f.trim_matches('/').to_owned()),
        None => (without_scheme, String::new()),
    };
    let looks_git = source.contains(".git")
        || source.starts_with("github.com/")
        || source.starts_with("git@")
        || reference.is_some();
    if !looks_git {
        return None;
    }
    let name = repository
        .trim_end_matches('/')
        .rsplit(['/', ':'])
        .next()?
        .trim_end_matches(".git")
        .to_owned();
    Some((name, folder, reference))
}

/// Reads the module `source` called from `place`: from the folder next to
/// the caller when local, from a linked clone at the pinned ref when in
/// git; a registry module is not read.
fn read_module(repos: &Repos, source: &str, place: &Place) -> Result<Option<Module>, String> {
    if source.starts_with('.') {
        let Some(caller) = repos.file(place) else {
            return Ok(None);
        };
        let folder = crate::repos::normal(&caller.parent().unwrap_or(Path::new("")).join(source));
        let Some((repo, rel)) = repos.locate(&folder) else {
            return Ok(None);
        };
        let module_files = std::fs::read_dir(&folder)
            .map(|entries| {
                entries
                    .filter_map(Result::ok)
                    .filter(|e| e.path().extension().is_some_and(|x| x == "tf"))
                    .filter_map(|e| {
                        let text = std::fs::read_to_string(e.path()).ok()?;
                        Some((
                            format!("{rel}/{}", e.file_name().to_string_lossy()),
                            hcl::parse(&text),
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default();
        return Ok(Some(Module {
            source: module_key(source, place),
            place: Place::new(&repo.name, &rel, 0),
            reference: None,
            files: module_files,
        }));
    }
    let Some((name, folder, reference)) = git_source(source) else {
        return Ok(None);
    };
    let Some(repo) = repos
        .repos
        .iter()
        .find(|r| r.name == name || r.name.ends_with(&format!("/{name}")))
    else {
        return Err(format!(
            "The module {source} is in {name}, which is not linked, so it is not read"
        ));
    };
    let mut module_files = Vec::new();
    match &reference {
        Some(reference) => {
            let listed = git(
                &repo.root,
                &["ls-tree", "-r", "--name-only", reference, "--", &folder],
            )
            .map_err(|e| {
                format!("The module {source} could not be read at {reference} in {name}: {e}")
            })?;
            for file in listed.lines().filter(|f| f.ends_with(".tf")) {
                // Only the module's own folder, not those under it.
                let within = file
                    .strip_prefix(&folder)
                    .unwrap_or(file)
                    .trim_start_matches('/');
                if within.contains('/') {
                    continue;
                }
                if let Ok(text) = git(&repo.root, &["show", &format!("{reference}:{file}")]) {
                    module_files.push((file.to_owned(), hcl::parse(&text)));
                }
            }
        }
        None => {
            let dir = repo.root.join(&folder);
            for (path, rel) in files(&dir) {
                if rel.ends_with(".tf")
                    && !rel.contains('/')
                    && let Ok(text) = std::fs::read_to_string(path)
                {
                    {
                        let file = if folder.is_empty() {
                            rel
                        } else {
                            format!("{folder}/{rel}")
                        };
                        module_files.push((file, hcl::parse(&text)));
                    }
                }
            }
        }
    }
    Ok(Some(Module {
        source: source.to_owned(),
        place: Place::new(&repo.name, &folder, 0),
        reference,
        files: module_files,
    }))
}

/// Runs git in `dir`, its output or its error.
pub(crate) fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                "git was not found: is it installed?".to_owned()
            } else {
                e.to_string()
            }
        })?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        Err(String::from_utf8_lossy(&output.stderr)
            .lines()
            .next()
            .unwrap_or("failed")
            .to_owned())
    }
}

/// The words of a place, lowercase, for telling how near two are.
pub(crate) fn words(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .collect()
}

/// How near two lists of words are: the words they share, a stage word
/// counting double.
pub(crate) fn nearness(a: &[String], b: &[String]) -> usize {
    let b: HashSet<&str> = b.iter().map(String::as_str).collect();
    let mut shared: Vec<&str> = a
        .iter()
        .map(String::as_str)
        .filter(|w| b.contains(w))
        .collect();
    shared.sort_unstable();
    shared.dedup();
    shared
        .iter()
        .map(|w| if STAGES.contains(w) { 2 } else { 1 })
        .sum()
}

/// Whether two stage words disagree: a block of another stage is never
/// the nearest.
fn other_stage(a: &[String], b: &[String]) -> bool {
    let stages = |w: &[String]| -> HashSet<String> {
        w.iter()
            .filter(|w| STAGES.contains(&w.as_str()))
            .cloned()
            .collect()
    };
    let (a, b) = (stages(a), stages(b));
    !a.is_empty() && !b.is_empty() && a.is_disjoint(&b)
}

/// A block's settings with what can be resolved resolved.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Resolved {
    /// Each top-level setting, its value as resolved.
    pub values: Vec<(String, String)>,
    /// For a module: the blocks in it, their settings resolved against
    /// what the call passes and the module's defaults.
    pub inner: Vec<Inner>,
}

/// A block inside a module, as a call sets it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Inner {
    /// Its type: `aws_rds_cluster`.
    pub what: String,
    /// Its name.
    pub name: String,
    /// Its settings, resolved.
    pub values: Vec<(String, String)>,
}

impl Resolved {
    /// The value of `key`.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&str> {
        self.values
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    /// The first value, of the block or of those in it, named one of `keys`.
    #[must_use]
    pub fn find(&self, keys: &[&str]) -> Option<&str> {
        keys.iter().find_map(|k| {
            self.get(k).or_else(|| {
                self.inner.iter().find_map(|inner| {
                    inner
                        .values
                        .iter()
                        .find(|(key, value)| {
                            key == k && !value.contains("var.") && !value.contains("local.")
                        })
                        .map(|(_, v)| v.as_str())
                })
            })
        })
    }
}

impl Terraform {
    /// The variables and locals known in the folder of `block`: its
    /// defaults, then the variables files nearest `near`.
    fn scope(&self, block: &TfBlock, near: &[String]) -> HashMap<String, Hcl> {
        let mut scope: HashMap<String, Hcl> = HashMap::new();
        for (repo, folder, body) in &self.declared {
            if *repo != block.place.repo || *folder != block.folder {
                continue;
            }
            for b in &body.blocks {
                match b.kind.as_str() {
                    "variable" => {
                        if let (Some(name), Some(default)) =
                            (b.labels.first(), b.body.get("default"))
                        {
                            scope.insert(format!("var.{name}"), default.clone());
                        }
                    }
                    "locals" => {
                        for a in &b.body.attributes {
                            scope.insert(format!("local.{}", a.name), a.value.clone());
                        }
                    }
                    _ => {}
                }
            }
        }
        // terraform.tfvars and *.auto.tfvars always; of the others, those
        // nearest the environment.
        let here: Vec<&(Place, String, Body)> = self
            .vars
            .iter()
            .filter(|(p, f, _)| p.repo == block.place.repo && *f == block.folder)
            .collect();
        let always =
            |p: &Place| p.path.ends_with("terraform.tfvars") || p.path.ends_with(".auto.tfvars");
        let best = here
            .iter()
            .filter(|(p, _, _)| !always(p))
            .map(|(p, _, _)| nearness(&words(&p.path), near))
            .max()
            .unwrap_or(0);
        for (place, _, body) in here {
            if always(place) || nearness(&words(&place.path), near) == best {
                for a in &body.attributes {
                    scope.insert(format!("var.{}", a.name), a.value.clone());
                }
            }
        }
        // Locals that use variables, once.
        let resolved: Vec<(String, Hcl)> = scope
            .iter()
            .filter(|(k, _)| k.starts_with("local."))
            .map(|(k, v)| (k.clone(), substitute(v, &scope)))
            .collect();
        scope.extend(resolved);
        scope
    }

    /// `block`'s settings resolved, the variables files nearest `near`
    /// used; a module's own blocks too, as the call sets them.
    #[must_use]
    pub fn resolve(&self, block: &TfBlock, near: &[String]) -> Resolved {
        let scope = self.scope(block, near);
        let values: Vec<(String, Hcl)> = block
            .body
            .attributes
            .iter()
            .map(|a| (a.name.clone(), substitute(&a.value, &scope)))
            .collect();
        let mut resolved = Resolved {
            values: values.iter().map(|(k, v)| (k.clone(), v.show())).collect(),
            inner: Vec::new(),
        };
        if block.kind != BlockKind::Module {
            return resolved;
        }
        let key = module_key(&block.what, &block.place);
        let Some(module) = self.modules.iter().find(|m| m.source == key) else {
            return resolved;
        };
        // What the module gets: what the call passes over its defaults,
        // objects' optional attributes filled in.
        let mut inputs: HashMap<String, Hcl> = HashMap::new();
        for (_, body) in &module.files {
            for b in body.blocks.iter().filter(|b| b.kind == "variable") {
                let Some(name) = b.labels.first() else {
                    continue;
                };
                let passed = values
                    .iter()
                    .find(|(k, _)| k == name)
                    .map(|(_, v)| v.clone());
                let mut value = passed.or_else(|| b.body.get("default").cloned());
                if let Some(Hcl::Expr(kind)) = b.body.get("type") {
                    let defaults = optional_defaults(kind);
                    if !defaults.is_empty() {
                        let mut entries = match value {
                            Some(Hcl::Object(entries)) => entries,
                            _ => Vec::new(),
                        };
                        for (k, v) in defaults {
                            if !entries.iter().any(|(e, _)| *e == k) {
                                entries.push((k, v));
                            }
                        }
                        value = Some(Hcl::Object(entries));
                    }
                }
                if let Some(value) = value {
                    inputs.insert(format!("var.{name}"), value);
                }
            }
        }
        for (_, body) in &module.files {
            for b in body.blocks.iter().filter(|b| b.kind == "locals") {
                for a in &b.body.attributes {
                    let value = substitute(&a.value, &inputs);
                    inputs.insert(format!("local.{}", a.name), value);
                }
            }
        }
        for (_, body) in &module.files {
            for b in body.blocks.iter().filter(|b| b.kind == "resource") {
                let what = b.labels.first().cloned().unwrap_or_default();
                let name = b.labels.get(1).cloned().unwrap_or_default();
                let values = b
                    .body
                    .attributes
                    .iter()
                    .map(|a| (a.name.clone(), substitute(&a.value, &inputs).show()))
                    .collect();
                resolved.inner.push(Inner { what, name, values });
            }
        }
        resolved
    }

    /// The block of one of `kinds` nearest `near`, never one of another
    /// stage.
    #[must_use]
    pub fn nearest(&self, matches: impl Fn(&TfBlock) -> bool, near: &[String]) -> Option<usize> {
        self.blocks
            .iter()
            .enumerate()
            .filter(|(_, b)| matches(b))
            .map(|(i, b)| (i, block_words(b)))
            .filter(|(_, w)| !other_stage(w, near))
            .max_by_key(|(i, w)| (nearness(w, near), std::cmp::Reverse(*i)))
            .map(|(i, _)| i)
    }
}

/// A block's words: its repository, folder, file and name.
fn block_words(block: &TfBlock) -> Vec<String> {
    let mut w = words(&block.place.repo);
    w.extend(words(&block.place.path));
    w.extend(words(&block.name));
    w
}

/// The defaults of `optional(type, default)` attributes in an object type.
fn optional_defaults(kind: &str) -> Vec<(String, Hcl)> {
    static OPTIONAL: LazyLock<Option<Regex>> = LazyLock::new(|| {
        Regex::new(
            r#"(\w+)\s*=\s*optional\(\s*[\w]+(?:\([^()]*\))?\s*,\s*("[^"]*"|[^,()\s]+)\s*\)"#,
        )
        .ok()
    });
    OPTIONAL.as_ref().map_or_else(Vec::new, |re| {
        re.captures_iter(kind)
            .map(|c| {
                let raw = &c[2];
                let value = if let Some(s) = raw.strip_prefix('"').and_then(|s| s.strip_suffix('"'))
                {
                    Hcl::Str(s.to_owned())
                } else if raw == "true" || raw == "false" {
                    Hcl::Bool(raw == "true")
                } else if raw.parse::<f64>().is_ok() {
                    Hcl::Number(raw.to_owned())
                } else {
                    Hcl::Expr(raw.to_owned())
                };
                (c[1].to_owned(), value)
            })
            .collect()
    })
}

/// `value` with the variables and locals of `scope` put in, where they
/// are known and plain.
fn substitute(value: &Hcl, scope: &HashMap<String, Hcl>) -> Hcl {
    static REFERENCE: LazyLock<Option<Regex>> = LazyLock::new(|| {
        Regex::new(r"\b((?:var|local)\.[A-Za-z_][\w-]*)((?:\.[A-Za-z_][\w-]*)*)").ok()
    });
    let lookup = |name: &str, path: &str| -> Option<Hcl> {
        let mut found = scope.get(name)?.clone();
        for key in path.split('.').filter(|k| !k.is_empty()) {
            found = found.get(key)?.clone();
        }
        Some(found)
    };
    let Some(re) = REFERENCE.as_ref() else {
        return value.clone();
    };
    match value {
        Hcl::Expr(text) => {
            if let Some(c) = re.captures(text)
                && c.get(0).is_some_and(|m| m.as_str() == text.trim())
                && let Some(found) = lookup(&c[1], &c[2])
            {
                return found;
            }
            value.clone()
        }
        Hcl::Str(text) if text.contains("${") => {
            let replaced =
                re.replace_all(text, |c: &regex::Captures<'_>| match lookup(&c[1], &c[2]) {
                    Some(Hcl::Str(s) | Hcl::Number(s)) => s,
                    Some(Hcl::Bool(b)) => b.to_string(),
                    _ => c[0].to_owned(),
                });
            // `${prod}` left once its variable is in.
            static PLAIN: LazyLock<Option<Regex>> =
                LazyLock::new(|| Regex::new(r"\$\{([^${}.()\s]+)\}").ok());
            let replaced = PLAIN.as_ref().map_or_else(
                || replaced.to_string(),
                |p| p.replace_all(&replaced, "$1").into_owned(),
            );
            Hcl::Str(replaced)
        }
        Hcl::List(items) => Hcl::List(items.iter().map(|i| substitute(i, scope)).collect()),
        Hcl::Object(entries) => Hcl::Object(
            entries
                .iter()
                .map(|(k, v)| (k.clone(), substitute(v, scope)))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// The cloud identifiers in a rendered environment's objects: roles in
/// service accounts' annotations, and roles, buckets, database hosts,
/// secrets and user pools in any value, by their shapes and their keys.
#[must_use]
pub fn cloud_ids(objects: &[crate::render::Object]) -> Vec<CloudId> {
    let mut ids: Vec<CloudId> = Vec::new();
    for object in objects {
        let mut found = Vec::new();
        strings(&object.body, "", &mut found);
        for (key, value) in found {
            for (kind, name) in identify(&key, &value) {
                let id = CloudId {
                    kind,
                    value: value.clone(),
                    name,
                    service: object.service.clone(),
                    place: object.origin.clone(),
                };
                if !ids
                    .iter()
                    .any(|i| i.kind == id.kind && i.name == id.name && i.service == id.service)
                {
                    ids.push(id);
                }
            }
        }
    }
    ids
}

/// Every string in `value`, with the key it is under; a container's
/// variable by its name.
fn strings(value: &serde_json::Value, key: &str, out: &mut Vec<(String, String)>) {
    match value {
        serde_json::Value::Object(map) => {
            let name = map.get("name").and_then(|n| n.as_str());
            for (k, v) in map {
                let key = if k == "value" { name.unwrap_or(k) } else { k };
                strings(v, key, out);
            }
        }
        serde_json::Value::Array(items) => items.iter().for_each(|i| strings(i, key, out)),
        serde_json::Value::String(s) => {
            // A config file in a value: its lines, each `key: value`.
            if s.contains('\n') {
                for line in s.lines() {
                    if let Some((k, v)) = line.split_once([':', '=']) {
                        out.push((
                            k.trim().trim_matches(['"', '-', ' ']).to_owned(),
                            v.trim().trim_matches(['"', '\'', ',']).to_owned(),
                        ));
                    }
                }
            } else {
                out.push((key.to_owned(), s.clone()));
            }
        }
        _ => {}
    }
}

/// What a value under `key` identifies in the cloud, and its name.
fn identify(key: &str, value: &str) -> Vec<(CloudKind, String)> {
    static ROLE: LazyLock<Option<Regex>> =
        LazyLock::new(|| Regex::new(r"arn:aws[\w-]*:iam::\d{12}:role/([\w+=,.@/-]+)").ok());
    static SECRET: LazyLock<Option<Regex>> = LazyLock::new(|| {
        Regex::new(r"arn:aws[\w-]*:secretsmanager:[\w-]+:\d{12}:secret:([\w/+=.@-]+?)(?:-[A-Za-z0-9]{6})?$").ok()
    });
    static HOST: LazyLock<Option<Regex>> = LazyLock::new(|| {
        Regex::new(r"([a-z0-9][a-z0-9-]*)\.(?:cluster-(?:ro-)?)?[a-z0-9]+\.[a-z0-9-]+\.rds\.amazonaws\.com").ok()
    });
    static POOL: LazyLock<Option<Regex>> =
        LazyLock::new(|| Regex::new(r"^[a-z]{2}(?:-[a-z]+)+-\d_[A-Za-z0-9]{9,}$").ok());
    static S3: LazyLock<Option<Regex>> =
        LazyLock::new(|| Regex::new(r"^s3://([a-z0-9][a-z0-9.-]{1,61}[a-z0-9])").ok());
    static BUCKET: LazyLock<Option<Regex>> =
        LazyLock::new(|| Regex::new(r"^[a-z0-9][a-z0-9.-]{1,61}[a-z0-9]$").ok());
    let capture = |re: &LazyLock<Option<Regex>>| -> Option<String> {
        re.as_ref()?
            .captures(value)
            .and_then(|c| c.get(1))
            .map(|m| m.as_str().to_owned())
    };
    let mut out = Vec::new();
    if let Some(role) = capture(&ROLE) {
        out.push((
            CloudKind::Role,
            role.rsplit('/').next().unwrap_or(&role).to_owned(),
        ));
    }
    if let Some(secret) = capture(&SECRET) {
        out.push((CloudKind::Secret, secret));
    } else if key == "objectName" && !value.is_empty() {
        out.push((CloudKind::Secret, value.to_owned()));
    }
    if let Some(host) = capture(&HOST) {
        out.push((CloudKind::Database, host));
    }
    if POOL.as_ref().is_some_and(|re| re.is_match(value)) {
        out.push((CloudKind::Users, value.to_owned()));
    }
    let lower = key.to_lowercase();
    if let Some(bucket) = capture(&S3) {
        out.push((CloudKind::Bucket, bucket));
    } else if lower.contains("bucket")
        && !lower.contains("region")
        && !lower.contains("url")
        && BUCKET.as_ref().is_some_and(|re| re.is_match(value))
        && value.parse::<f64>().is_err()
    {
        out.push((CloudKind::Bucket, value.to_owned()));
    }
    out
}

/// Whether a block makes a thing of `kind`.
#[must_use]
pub fn makes(block: &TfBlock, kind: CloudKind) -> bool {
    let what = block.what.to_lowercase();
    match block.kind {
        BlockKind::Resource => match kind {
            CloudKind::Role => what == "aws_iam_role",
            CloudKind::Bucket => what == "aws_s3_bucket",
            CloudKind::Database => what == "aws_db_instance" || what == "aws_rds_cluster",
            CloudKind::Secret => what == "aws_secretsmanager_secret",
            CloudKind::Users => what == "aws_cognito_user_pool",
        },
        BlockKind::Module => {
            let source = what.rsplit("//").next().unwrap_or(&what).to_owned() + " " + &what;
            match kind {
                CloudKind::Role => {
                    source.contains("iam") || source.contains("irsa") || source.contains("role")
                }
                CloudKind::Bucket => source.contains("s3") || source.contains("bucket"),
                CloudKind::Database => {
                    source.contains("rds")
                        || source.contains("aurora")
                        || source.contains("database")
                        || source.contains("postgres")
                }
                CloudKind::Secret => source.contains("secret"),
                CloudKind::Users => {
                    source.contains("cognito")
                        || source.contains("user-pool")
                        || source.contains("userpool")
                }
            }
        }
        BlockKind::Data => false,
    }
}

/// The names a block gives what it makes: its naming settings, its own
/// and its module's.
fn names(resolved: &Resolved) -> Vec<String> {
    const NAMING: [&str; 9] = [
        "name",
        "name_prefix",
        "bucket",
        "bucket_prefix",
        "identifier",
        "cluster_identifier",
        "role_name",
        "secret_name",
        "user_pool_name",
    ];
    let mut out: Vec<String> = resolved
        .values
        .iter()
        .chain(resolved.inner.iter().flat_map(|i| i.values.iter()))
        .filter(|(k, _)| NAMING.contains(&k.as_str()))
        .map(|(_, v)| v.clone())
        .filter(|v| !v.is_empty())
        .collect();
    out.dedup();
    out
}

/// The part of 8 characters or more, between separators, that two names
/// share, its length.
fn shared_part(a: &str, b: &str) -> usize {
    let split = |s: &str| -> Vec<String> {
        s.to_lowercase()
            .split(['-', '_', '.', '/', ':'])
            .map(str::to_owned)
            .collect()
    };
    let (a, b) = (split(a), split(b));
    let mut best = 0;
    for i in 0..a.len() {
        for j in 0..b.len() {
            let mut k = 0;
            while i + k < a.len() && j + k < b.len() && a[i + k] == b[j + k] && !a[i + k].is_empty()
            {
                k += 1;
            }
            if k > 0 {
                let length = a[i..i + k].join("-").len();
                if length >= 8 {
                    best = best.max(length);
                }
            }
        }
    }
    best
}

/// Whether `name` fits a block's name written with `${…}` in it.
fn fits(pattern: &str, name: &str) -> bool {
    if !pattern.contains("${") {
        return false;
    }
    let mut regex = String::from("^");
    let mut rest = pattern;
    while let Some(start) = rest.find("${") {
        regex.push_str(&regex::escape(&rest[..start]));
        regex.push_str(".+");
        let after = &rest[start + 2..];
        let mut depth = 1;
        let mut end = after.len();
        for (i, c) in after.char_indices() {
            match c {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        end = i + 1;
                        break;
                    }
                }
                _ => {}
            }
        }
        rest = &after[end.min(after.len())..];
    }
    regex.push_str(&regex::escape(rest));
    regex.push('$');
    Regex::new(&regex).is_ok_and(|re| re.is_match(name))
}

/// The block that configures `id`, and how well it matches, the
/// variables files nearest `near` used.
#[must_use]
pub fn configures(
    terraform: &Terraform,
    id: &CloudId,
    near: &[String],
) -> Option<(usize, Quality)> {
    let mut best: Option<(usize, Quality, usize, usize)> = None;
    for (i, block) in terraform.blocks.iter().enumerate() {
        if !makes(block, id.kind) {
            continue;
        }
        let bw = block_words(block);
        if other_stage(&bw, near) {
            continue;
        }
        let resolved = terraform.resolve(block, near);
        let mut quality: Option<(Quality, usize)> = None;
        for name in names(&resolved) {
            let found = if name == id.name || name == id.value {
                Some((Quality::Exact, 0))
            } else if fits(&name, &id.name) {
                Some((Quality::Pattern, 0))
            } else {
                let part = shared_part(&name, &id.name);
                (part > 0).then_some((Quality::Part, usize::MAX - part))
            };
            if let Some(found) = found
                && quality.is_none_or(|q| found < q)
            {
                quality = Some(found);
            }
        }
        let Some((quality, score)) = quality else {
            continue;
        };
        let near_score = usize::MAX - nearness(&bw, near);
        if best.is_none_or(|(_, q, s, n)| (quality, score, near_score) < (q, s, n)) {
            best = Some((i, quality, score, near_score));
        }
    }
    if let Some((i, quality, _, _)) = best {
        return Some((i, quality));
    }
    // An identifier the cloud makes up when applied names no block: the
    // nearest that makes its kind.
    if id.kind == CloudKind::Users {
        return terraform
            .nearest(|b| makes(b, id.kind), near)
            .map(|i| (i, Quality::Nearest));
    }
    None
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::deploy::tests::write;

    fn commit(dir: &Path, message: &str) {
        for args in [
            &["add", "-A"][..],
            &[
                "-c",
                "user.email=a@example.com",
                "-c",
                "user.name=a",
                "commit",
                "-qm",
                message,
            ],
        ] {
            git(dir, args).unwrap();
        }
    }

    /// Infrastructure for two stages, and a modules repository whose
    /// database module changed after the tag the stages pin.
    pub(crate) fn infrastructure(root: &Path) -> Repos {
        let infra = root.join("infra");
        let modules = root.join("tf-modules");
        write(
            &modules,
            "rds/variables.tf",
            "variable \"name\" {}\nvariable \"settings\" {\n  type = object({\n    engine = optional(string, \"postgres\")\n    class  = optional(string, \"db.t4g.medium\")\n    count  = optional(number, 2)\n  })\n  default = {}\n}\n",
        );
        write(
            &modules,
            "rds/main.tf",
            "resource \"aws_rds_cluster\" \"this\" {\n  cluster_identifier = var.name\n  engine             = var.settings.engine\n}\nresource \"aws_rds_cluster_instance\" \"this\" {\n  count          = var.settings.count\n  instance_class = var.settings.class\n}\n",
        );
        git(&modules, &["init", "-q"]).unwrap();
        commit(&modules, "rds");
        git(&modules, &["tag", "v1.0.0"]).unwrap();
        write(
            &modules,
            "rds/variables.tf",
            "variable \"name\" {}\nvariable \"settings\" {\n  type = object({\n    engine = optional(string, \"mysql\")\n  })\n}\n",
        );
        commit(&modules, "later");
        for stage in ["prod", "staging"] {
            write(
                &infra,
                &format!("envs/{stage}/main.tf"),
                &format!(
                    "module \"db\" {{\n  source   = \"git::https://git.example.com/org/tf-modules.git//rds?ref=v1.0.0\"\n  name     = \"${{var.stage}}-shop\"\n  settings = {{ class = \"db.r6g.large\" }}\n}}\n\nresource \"aws_iam_role\" \"api\" {{\n  name = \"shop-${{var.stage}}-api\"\n}}\n\nresource \"aws_s3_bucket\" \"files\" {{\n  bucket = \"shop-{stage}-files-archive\"\n}}\n\nresource \"aws_cognito_user_pool\" \"users\" {{\n  name = \"shop\"\n}}\n\nvariable \"stage\" {{}}\n"
                ),
            );
            write(
                &infra,
                &format!("envs/{stage}/terraform.tfvars"),
                &format!("stage = \"{stage}\"\n"),
            );
        }
        Repos::new(&infra, &[modules])
    }

    #[test]
    fn modules_read_at_their_ref_and_variables_resolved() {
        let dir = tempfile::tempdir().unwrap();
        let repos = infrastructure(dir.path());
        let terraform = terraform(&repos);
        assert!(terraform.notes.is_empty(), "{:?}", terraform.notes);
        let near = words("clusters/prod");
        let db = terraform
            .blocks
            .iter()
            .find(|b| b.kind == BlockKind::Module && b.place.path == "envs/prod/main.tf")
            .unwrap();
        let resolved = terraform.resolve(db, &near);
        assert_eq!(resolved.get("name"), Some("prod-shop"));
        // At v1.0.0 the engine's default was postgres; the call sets the
        // class; the count is the module's default.
        assert_eq!(resolved.find(&["engine"]), Some("postgres"));
        assert_eq!(resolved.find(&["instance_class"]), Some("db.r6g.large"));
        assert_eq!(resolved.find(&["count"]), Some("2"));
    }

    #[test]
    fn identifiers_matched_to_the_blocks_that_make_them() {
        let dir = tempfile::tempdir().unwrap();
        let repos = infrastructure(dir.path());
        let terraform = terraform(&repos);
        let near = words("clusters/prod/app");
        let id = |kind, name: &str| CloudId {
            kind,
            value: name.to_owned(),
            name: name.to_owned(),
            service: None,
            place: None,
        };
        let path = |found: Option<(usize, Quality)>| {
            found.map(|(i, q)| {
                (
                    terraform.blocks[i].place.path.clone(),
                    terraform.blocks[i].what.clone(),
                    q,
                )
            })
        };
        assert_eq!(
            path(configures(
                &terraform,
                &id(CloudKind::Role, "shop-prod-api"),
                &near
            )),
            Some((
                "envs/prod/main.tf".into(),
                "aws_iam_role".into(),
                Quality::Exact
            ))
        );
        assert_eq!(
            path(configures(
                &terraform,
                &id(CloudKind::Database, "prod-shop"),
                &near
            ))
            .map(|p| p.2),
            Some(Quality::Exact)
        );
        assert_eq!(
            path(configures(
                &terraform,
                &id(CloudKind::Bucket, "shop-prod-files"),
                &near
            ))
            .map(|p| p.2),
            Some(Quality::Part)
        );
        assert_eq!(
            path(configures(
                &terraform,
                &id(CloudKind::Users, "eu-west-1_AbCdEfGhI"),
                &near
            )),
            Some((
                "envs/prod/main.tf".into(),
                "aws_cognito_user_pool".into(),
                Quality::Nearest
            ))
        );
        assert!(fits("shop-${var.stage}-api", "shop-prod-api"));
        assert!(!fits("shop-${var.stage}-api", "shop-prod-web"));
    }

    #[test]
    fn identifiers_read_by_shape_and_key() {
        let kinds = |key: &str, value: &str| identify(key, value);
        assert_eq!(
            kinds(
                "eks.amazonaws.com/role-arn",
                "arn:aws:iam::123456789012:role/shop-prod-api"
            ),
            [(CloudKind::Role, "shop-prod-api".to_owned())]
        );
        assert_eq!(
            kinds(
                "DB_HOST",
                "prod-shop.cluster-abc123xyz.eu-west-1.rds.amazonaws.com"
            ),
            [(CloudKind::Database, "prod-shop".to_owned())]
        );
        assert_eq!(
            kinds(
                "SECRET",
                "arn:aws:secretsmanager:eu-west-1:123456789012:secret:shop/prod/db-AbC123"
            ),
            [(CloudKind::Secret, "shop/prod/db".to_owned())]
        );
        assert_eq!(
            kinds("FILES_BUCKET", "shop-prod-files"),
            [(CloudKind::Bucket, "shop-prod-files".to_owned())]
        );
        assert_eq!(kinds("BUCKET_REGION", "eu-west-1"), []);
        assert_eq!(
            kinds("USER_POOL", "eu-west-1_AbCdEfGhI"),
            [(CloudKind::Users, "eu-west-1_AbCdEfGhI".to_owned())]
        );
        assert_eq!(
            git_source("git::https://git.example.com/org/tf-modules.git//rds?ref=v1.0.0"),
            Some(("tf-modules".into(), "rds".into(), Some("v1.0.0".into())))
        );
        assert_eq!(git_source("terraform-aws-modules/vpc/aws"), None);
    }
}
