//! The files of a map gathered into groups by what they are: the module
//! that holds them, their language, their layer, what they are for. Groups
//! can be nested, a second criterion splitting each group of the first, and
//! the map narrowed to one group at a time, to look inside it.
//!
//! A criterion that would put every file of what is looked at in the same
//! group says nothing there, and is passed over: once narrowed to the Rust
//! files, grouping by language again would only make one group, and the
//! next criterion takes its place. So is one that would put each file in a
//! group of its own: a crate whose parts are its files. Grouping by module does not run out that
//! way: inside a module, it groups by that module's own parts.

use std::collections::{BTreeSet, HashMap};
use std::path::Path;

use crate::architecture::{Architecture, architecture};
use crate::map::{CodeMap, EdgeKind, Language, NodeKind};

/// The most criteria applied at once: beyond two, groups within groups
/// within groups are too small to tell apart.
pub const MAX_CRITERIA: usize = 2;

/// A file's group under one criterion, and the group's name.
type Labelled = (Option<Key>, String);

/// What files can be grouped by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Criterion {
    /// The part of the project's design that holds them, as
    /// [`architecture`] reads it: a package, a folder, a file.
    Module,
    /// What they are written in.
    Language,
    /// The layer of their module: foundations first.
    Layer,
    /// What they are for: code, tests, documents, settings.
    Role,
}

impl Criterion {
    /// Every criterion, in the order a view offers them.
    pub const ALL: [Self; 4] = [Self::Module, Self::Language, Self::Layer, Self::Role];

    /// Its name, for a view.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Module => "Module",
            Self::Language => "Language",
            Self::Layer => "Layer",
            Self::Role => "Role",
        }
    }
}

/// What a file is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Role {
    /// Source code that runs.
    Code,
    /// Tests of it.
    Tests,
    /// Documents.
    Documents,
    /// Settings and manifests.
    Settings,
    /// Anything else.
    Other,
}

impl Role {
    /// What `path` is for.
    #[must_use]
    pub fn of(path: &Path) -> Self {
        match Language::of(path) {
            Language::Markdown => Self::Documents,
            Language::Config => Self::Settings,
            Language::Other => Self::Other,
            _ if crate::map::is_test(path) => Self::Tests,
            _ => Self::Code,
        }
    }

    /// Its name, for a view.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Code => "code",
            Self::Tests => "tests",
            Self::Documents => "documents",
            Self::Settings => "settings",
            Self::Other => "other",
        }
    }
}

/// Which group a file is in, under one criterion. It is also what a view
/// is narrowed to, to look inside a group.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Key {
    /// The module at this path, from the project's root.
    Module(String),
    /// This language.
    Language(Language),
    /// This layer of the design of the folder `scope`.
    Layer {
        /// The folder whose design the layer is of.
        scope: String,
        /// The layer: 0 for the foundations.
        layer: usize,
    },
    /// This role.
    Role(Role),
}

/// A group of files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    /// What its files share; `None` for those a criterion leaves out, such
    /// as files outside every module.
    pub key: Option<Key>,
    /// Its name, for a view.
    pub label: String,
    /// How many files it holds.
    pub files: usize,
    /// How many lines they have.
    pub lines: usize,
}

/// The nodes of a map, gathered.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Grouping {
    /// The criteria the groups follow: those asked for that say something
    /// about what is looked at, in their order.
    pub criteria: Vec<Criterion>,
    /// The groups of the first criterion, the largest first.
    pub groups: Vec<Group>,
    /// The groups of the second criterion within each group of the first,
    /// the largest first; empty without a second criterion.
    pub subgroups: Vec<Vec<Group>>,
    /// Whether each node of the map is among what is looked at.
    inside: Vec<bool>,
    /// The group, and the group within it, of each node looked at.
    of: Vec<Option<(usize, Option<usize>)>>,
}

impl Grouping {
    /// Whether the map's `node` is among what is looked at.
    #[must_use]
    pub fn inside(&self, node: usize) -> bool {
        self.inside.get(node).copied().unwrap_or(false)
    }

    /// The group of the map's `node`, as places in [`Grouping::groups`] and
    /// [`Grouping::subgroups`], if it is looked at and grouped. A folder is
    /// in a group only when all the files below it are, and in a cluster
    /// only when they all are in that one.
    #[must_use]
    pub fn of(&self, node: usize) -> Option<(usize, Option<usize>)> {
        self.of.get(node).copied().flatten()
    }
}

/// The nodes of `map`, narrowed to those in every group of `focus` and
/// gathered by `criteria`, of which the first [`MAX_CRITERIA`] that say
/// something count.
///
/// # Examples
///
/// ```
/// use ironquill_codemap::{Criterion, Key, Language, group};
///
/// let dir = tempfile::tempdir().unwrap();
/// let write = |path: &str| {
///     let path = dir.path().join(path);
///     std::fs::create_dir_all(path.parent().unwrap()).unwrap();
///     std::fs::write(path, "").unwrap();
/// };
/// write("app/main.py");
/// write("app/view.py");
/// write("app/README.md");
/// write("lib/tools.py");
/// let map = ironquill_codemap::map(dir.path(), 100);
///
/// let by_module = group(&map, &[], &[Criterion::Module, Criterion::Language]);
/// let names: Vec<&str> = by_module.groups.iter().map(|g| g.label.as_str()).collect();
/// assert_eq!(names, ["app", "lib"]);
///
/// // Narrowed to Python, grouping by language says nothing: it is passed
/// // over.
/// let python = group(&map, &[Key::Language(Language::Python)], &[Criterion::Language, Criterion::Module]);
/// assert_eq!(python.criteria, [Criterion::Module]);
/// let readme = map.find("app/README.md").unwrap();
/// assert!(!python.inside(readme));
/// ```
#[must_use]
pub fn group(map: &CodeMap, focus: &[Key], criteria: &[Criterion]) -> Grouping {
    let mut designs = Designs::new(map);
    let scope = focus
        .iter()
        .rev()
        .find_map(|key| match key {
            Key::Module(path) => Some(path.clone()),
            _ => None,
        })
        .unwrap_or_default();
    let files: Vec<usize> = (0..map.nodes.len())
        .filter(|&n| matches!(map.nodes[n].kind, NodeKind::File { .. }))
        .filter(|&n| {
            focus
                .iter()
                .all(|key| designs.key(n, key) == Some(key.clone()))
        })
        .collect();
    let parents = parents(map);
    let mut inside = vec![false; map.nodes.len()];
    for &file in &files {
        inside[file] = true;
        let mut at = parents[file];
        while let Some(folder) = at {
            if inside[folder] || !within(&map.nodes[folder].path, &scope) {
                break;
            }
            inside[folder] = true;
            at = parents[folder];
        }
    }

    // Each criterion's key of each file, kept only when it parts them.
    let mut chosen: Vec<(Criterion, Vec<Labelled>)> = Vec::new();
    for &criterion in criteria {
        if chosen.len() == MAX_CRITERIA {
            break;
        }
        if chosen.iter().any(|(c, _)| *c == criterion) {
            continue;
        }
        let keys: Vec<Labelled> = files
            .iter()
            .map(|&f| designs.of(f, criterion, &scope))
            .collect();
        // One group says nothing; nor do groups of one file each, which
        // the stars already are.
        let mut sizes: HashMap<&Option<Key>, usize> = HashMap::new();
        for (key, _) in &keys {
            *sizes.entry(key).or_default() += 1;
        }
        if sizes.len() > 1 && sizes.values().any(|n| *n > 1) {
            chosen.push((criterion, keys));
        }
    }

    let mut grouping = Grouping {
        criteria: chosen.iter().map(|(c, _)| *c).collect(),
        groups: Vec::new(),
        subgroups: Vec::new(),
        of: vec![None; map.nodes.len()],
        inside,
    };
    let Some((_, first)) = chosen.first() else {
        return grouping;
    };
    let lines = |f: usize| match map.nodes[f].kind {
        NodeKind::File { lines, .. } => lines,
        NodeKind::Folder => 0,
    };
    let (groups, top) = gather(first, files.iter().map(|&f| lines(f)));
    grouping.groups = groups;
    let mut sub = vec![None; files.len()];
    if let Some((_, second)) = chosen.get(1) {
        grouping.subgroups = (0..grouping.groups.len())
            .map(|g| {
                let members: Vec<usize> = (0..files.len()).filter(|&i| top[i] == g).collect();
                let keys: Vec<Labelled> = members.iter().map(|&i| second[i].clone()).collect();
                let (groups, places) = gather(&keys, members.iter().map(|&i| lines(files[i])));
                for (&i, place) in members.iter().zip(places) {
                    sub[i] = Some(place);
                }
                groups
            })
            .collect();
    }
    for (i, &file) in files.iter().enumerate() {
        grouping.of[file] = Some((top[i], sub[i]));
    }
    // A folder goes with what it holds when all of it is in one group,
    // and in one cluster when all of it is in one; a folder above several
    // groups (the root, `crates/`) is in none, and floats between them.
    let mut below: HashMap<usize, BTreeSet<(usize, Option<usize>)>> = HashMap::new();
    for (i, &file) in files.iter().enumerate() {
        let mut at = parents[file];
        while let Some(folder) = at.filter(|&f| grouping.inside[f]) {
            below.entry(folder).or_default().insert((top[i], sub[i]));
            at = parents[folder];
        }
    }
    for (folder, groups) in below {
        let mut tops = groups.iter().map(|(top, _)| *top);
        let first = tops.next();
        grouping.of[folder] = match first {
            Some(top) if tops.all(|t| t == top) => {
                let cluster = groups.first().and_then(|(_, sub)| *sub);
                Some((top, cluster.filter(|_| groups.len() == 1)))
            }
            _ => None,
        };
    }
    grouping
}

/// The groups `keys` make, the largest first, and the place of each key's
/// group among them; `lines` are those of each key's file.
fn gather(keys: &[Labelled], lines: impl Iterator<Item = usize>) -> (Vec<Group>, Vec<usize>) {
    let mut groups: Vec<Group> = Vec::new();
    let mut found: Vec<usize> = Vec::with_capacity(keys.len());
    for ((key, label), lines) in keys.iter().zip(lines) {
        let at = match groups.iter().position(|g| g.key == *key) {
            Some(at) => at,
            None => {
                groups.push(Group {
                    key: key.clone(),
                    label: label.clone(),
                    files: 0,
                    lines: 0,
                });
                groups.len() - 1
            }
        };
        groups[at].files += 1;
        groups[at].lines += lines;
        found.push(at);
    }
    // The largest first, then by name, those left out last: the same files
    // always give the same order.
    let mut order: Vec<usize> = (0..groups.len()).collect();
    order.sort_by(|&a, &b| {
        let (a, b) = (&groups[a], &groups[b]);
        a.key
            .is_none()
            .cmp(&b.key.is_none())
            .then(b.files.cmp(&a.files))
            .then(a.label.cmp(&b.label))
    });
    let mut place = vec![0; groups.len()];
    for (new, &old) in order.iter().enumerate() {
        place[old] = new;
    }
    let sorted = order.iter().map(|&old| groups[old].clone()).collect();
    (sorted, found.into_iter().map(|g| place[g]).collect())
}

/// The folder holding each node of `map`; `None` for the root.
fn parents(map: &CodeMap) -> Vec<Option<usize>> {
    let mut parents = vec![None; map.nodes.len()];
    for edge in map.edges.iter().filter(|e| e.kind == EdgeKind::Contains) {
        if let Some(parent) = parents.get_mut(edge.to) {
            *parent = Some(edge.from);
        }
    }
    parents
}

/// Whether `path` is `scope` or below it.
fn within(path: &str, scope: &str) -> bool {
    scope.is_empty()
        || path == scope
        || path
            .strip_prefix(scope)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// The designs of the folders a grouping needs, each read once.
struct Designs<'a> {
    map: &'a CodeMap,
    read: HashMap<String, Architecture>,
}

impl<'a> Designs<'a> {
    fn new(map: &'a CodeMap) -> Self {
        Self {
            map,
            read: HashMap::new(),
        }
    }

    fn design(&mut self, scope: &str) -> &Architecture {
        let map = self.map;
        self.read
            .entry(scope.to_owned())
            .or_insert_with(|| architecture(map, scope))
    }

    /// The key `file` has under the same criterion as `like`.
    fn key(&mut self, file: usize, like: &Key) -> Option<Key> {
        match like {
            Key::Module(path) => within(&self.map.nodes[file].path, path).then(|| like.clone()),
            Key::Language(_) => self.of(file, Criterion::Language, "").0,
            Key::Layer { scope, .. } => self.of(file, Criterion::Layer, scope).0,
            Key::Role(_) => self.of(file, Criterion::Role, "").0,
        }
    }

    /// The key and the label of `file`'s group under `criterion`, its
    /// modules and layers read in the folder `scope`.
    fn of(&mut self, file: usize, criterion: Criterion, scope: &str) -> Labelled {
        let node = &self.map.nodes[file];
        match criterion {
            Criterion::Language => {
                let NodeKind::File { language, .. } = node.kind else {
                    return (None, String::new());
                };
                (Some(Key::Language(language)), language.name().to_owned())
            }
            Criterion::Role => {
                let role = Role::of(Path::new(&node.path));
                (Some(Key::Role(role)), role.name().to_owned())
            }
            Criterion::Module => {
                let design = self.design(scope);
                match design.owner(file).map(|c| &design.components[c]) {
                    Some(c) => (Some(Key::Module(c.path.clone())), c.name.clone()),
                    None => (None, "elsewhere".to_owned()),
                }
            }
            Criterion::Layer => {
                let design = self.design(scope);
                match design.owner(file).map(|c| design.components[c].layer) {
                    Some(layer) => (
                        Some(Key::Layer {
                            scope: scope.to_owned(),
                            layer,
                        }),
                        if layer == 0 {
                            "layer 0 · foundations".to_owned()
                        } else {
                            format!("layer {layer}")
                        },
                    ),
                    None => (None, "outside the layers".to_owned()),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(root: &Path, path: &str, text: &str) {
        let path = root.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    /// Two Python packages, one using the other, each with a document and
    /// a test, and a README at the root.
    fn project() -> (tempfile::TempDir, CodeMap) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "README.md", "");
        write(root, "app/pyproject.toml", "");
        write(root, "app/app/main.py", "import lib.tools\n");
        write(root, "app/app/view.py", "");
        write(root, "app/tests/test_main.py", "");
        write(root, "app/NOTES.md", "");
        write(root, "lib/pyproject.toml", "");
        write(root, "lib/lib/tools.py", "");
        write(root, "lib/GUIDE.md", "");
        let map = crate::map(root, 100);
        (dir, map)
    }

    fn labels(groups: &[Group]) -> Vec<&str> {
        groups.iter().map(|g| g.label.as_str()).collect()
    }

    #[test]
    fn by_module_then_language_every_file_has_both_and_the_rest_comes_last() {
        let (_dir, map) = project();
        let grouping = group(&map, &[], &[Criterion::Module, Criterion::Language]);
        assert_eq!(grouping.criteria, [Criterion::Module, Criterion::Language]);
        assert_eq!(labels(&grouping.groups), ["app", "lib", "elsewhere"]);
        assert_eq!(grouping.groups[0].files, 5);
        assert_eq!(grouping.groups[2].key, None);
        assert_eq!(
            labels(&grouping.subgroups[0]),
            ["Python", "Markdown", "Settings"]
        );
        let main = map.find("app/app/main.py").unwrap();
        assert_eq!(grouping.of(main), Some((0, Some(0))));
        // A folder goes with what it holds: in app's galaxy, but in no one
        // cluster, as it holds Python, Markdown and settings; the root holds
        // all galaxies, and is in none.
        let app = map.find("app").unwrap();
        assert_eq!(grouping.of(app), Some((0, None)));
        let code = map.find("app/app").unwrap();
        assert_eq!(grouping.of(code), Some((0, Some(0))));
        assert_eq!(grouping.of(0), None);
        // Everything is looked at.
        assert!((0..map.nodes.len()).all(|n| grouping.inside(n)));
    }

    #[test]
    fn inside_a_module_its_own_parts_are_its_groups() {
        let (_dir, map) = project();
        let grouping = group(&map, &[Key::Module("app".into())], &[Criterion::Module]);
        let readme = map.find("README.md").unwrap();
        let lib = map.find("lib/lib/tools.py").unwrap();
        let notes = map.find("app/NOTES.md").unwrap();
        assert!(!grouping.inside(readme) && !grouping.inside(lib));
        assert!(grouping.inside(notes));
        // The way down to the module is not inside it; the module is.
        assert!(!grouping.inside(0));
        assert!(grouping.inside(map.find("app").unwrap()));
        assert!(grouping.groups.len() >= 2, "{:?}", grouping.groups);
    }

    #[test]
    fn groups_of_one_file_each_say_nothing() {
        let (_dir, map) = project();
        // Inside lib's code, each part is one file: no galaxies, but the
        // files are all looked at.
        let focus = [Key::Module("lib".into()), Key::Role(Role::Code)];
        let grouping = group(&map, &focus, &[Criterion::Module]);
        assert!(grouping.criteria.is_empty(), "{:?}", grouping.groups);
        assert!(grouping.inside(map.find("lib/lib/tools.py").unwrap()));
    }

    #[test]
    fn a_criterion_that_parts_nothing_gives_its_place_to_the_next() {
        let (_dir, map) = project();
        let focus = [Key::Role(Role::Documents)];
        let grouping = group(&map, &focus, &[Criterion::Role, Criterion::Language]);
        // Documents are all Markdown: neither criterion parts them.
        assert!(grouping.criteria.is_empty());
        assert!(grouping.groups.is_empty());
        let notes = map.find("app/NOTES.md").unwrap();
        assert!(grouping.inside(notes));
        assert_eq!(grouping.of(notes), None);

        let grouping = group(&map, &[], &[Criterion::Role, Criterion::Role]);
        assert_eq!(grouping.criteria, [Criterion::Role]);
        assert_eq!(
            labels(&grouping.groups),
            ["code", "documents", "settings", "tests"]
        );
    }

    #[test]
    fn by_layer_the_used_package_is_the_foundation() {
        let (_dir, map) = project();
        let grouping = group(&map, &[], &[Criterion::Layer]);
        let tools = map.find("lib/lib/tools.py").unwrap();
        let main = map.find("app/app/main.py").unwrap();
        let (lib, app) = (grouping.of(tools).unwrap().0, grouping.of(main).unwrap().0);
        assert_eq!(
            grouping.groups[lib].key,
            Some(Key::Layer {
                scope: String::new(),
                layer: 0
            })
        );
        assert_ne!(lib, app);
        // Narrowed to the foundations, only lib's files are left.
        let focus = [grouping.groups[lib].key.clone().unwrap()];
        let narrowed = group(&map, &focus, &[Criterion::Module]);
        assert!(narrowed.inside(tools) && !narrowed.inside(main));
    }
}
