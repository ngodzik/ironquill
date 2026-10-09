//! The design of a project, read from its map, one level at a time: the
//! parts of a folder (its subfolders and files, past the folders that only
//! lead somewhere), what uses what, and the layers that follow, foundations
//! first. A part can be opened, to read its own design the same way.
//!
//! Packages are found by their manifests (`Cargo.toml` beside a crate root,
//! `package.json`, `pyproject.toml`, `setup.py`, `go.mod`), never by their
//! names: a folder that only holds a few packages (`crates/`) shows them
//! rather than itself, one that holds many (`providers/`) shows as one part
//! to open. Only the code counts: tests, documents and examples are left out,
//! as they lean on everything and are not the design. A dependency is an
//! import from a file of one part to a file of another, counted, so that a
//! link shows how much one leans on the other. Parts that use each other,
//! directly or around a loop, share a layer and their links are marked: a
//! cycle is what a layered design forbids, and the drawing shows it.

use std::collections::{BTreeMap, HashSet};
use std::path::Path;

use crate::map::{CodeMap, EdgeKind, Language, NodeKind};

/// How many times the order within each layer is improved: a few sweeps
/// are enough to untangle most links, and the result stays the same for the
/// same project.
const SWEEPS: usize = 8;

/// The most packages a folder that holds nothing else shows in its place;
/// beyond, it shows as one part, to open.
const INLINE_PACKAGES: usize = 12;

/// What a part is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComponentKind {
    /// A source file.
    File,
    /// A folder.
    Folder,
    /// A package, with a manifest of its own.
    Package,
    /// A folder holding several packages.
    Group {
        /// How many.
        packages: usize,
    },
}

/// A part of the project, as its design sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Component {
    /// Its name: its folder's or file's; empty for the project's root.
    pub name: String,
    /// Its folder, or its file when it is one, from the project's root.
    pub path: String,
    /// What it is.
    pub kind: ComponentKind,
    /// Its source files, as nodes of the map, longest first.
    pub files: Vec<usize>,
    /// How many lines its source files have.
    pub lines: usize,
    /// Its layer: 0 for what uses no other component, one above the
    /// highest of what it uses otherwise.
    pub layer: usize,
}

/// One component using another.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Link {
    /// The component that uses, by its place in
    /// [`Architecture::components`].
    pub from: usize,
    /// The component used.
    pub to: usize,
    /// How many imports go from the one to the other.
    pub imports: usize,
    /// How many of its files call, through an OpenAPI spec, files of the
    /// other that serve them: a front end and its back end.
    pub calls: usize,
    /// Whether the two use each other, directly or around a loop.
    pub cyclic: bool,
}

/// The parts of one level of the project, their links, and their layers.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Architecture {
    /// The folder whose parts these are, from the project's root: the one
    /// asked for, or a folder below it when the way there holds nothing
    /// else.
    pub level: String,
    /// The components, by path.
    pub components: Vec<Component>,
    /// What uses what.
    pub links: Vec<Link>,
    /// The components of each layer, foundations first, each layer in the
    /// order that crosses its links the least found.
    pub layers: Vec<Vec<usize>>,
    /// The component each node of the map belongs to, if any.
    owners: Vec<Option<usize>>,
}

impl Architecture {
    /// The component the map's `node` belongs to: a source file's, or a
    /// folder's when the folder is, or is inside, a component.
    #[must_use]
    pub fn owner(&self, node: usize) -> Option<usize> {
        self.owners.get(node).copied().flatten()
    }

    /// How many links run around a loop.
    #[must_use]
    pub fn cycles(&self) -> usize {
        self.links.iter().filter(|l| l.cyclic).count()
    }
}

/// The design of folder `scope` of the project `map` was read from, `""`
/// for the whole.
///
/// # Examples
///
/// ```
/// let dir = tempfile::tempdir().unwrap();
/// let write = |path: &str, text: &str| {
///     let path = dir.path().join(path);
///     std::fs::create_dir_all(path.parent().unwrap()).unwrap();
///     std::fs::write(path, text).unwrap();
/// };
/// write("app/main.py", "import lib.tools\n");
/// write("lib/tools.py", "");
/// let map = ironquill_codemap::map(dir.path(), 100);
/// let design = ironquill_codemap::architecture(&map, "");
/// let names: Vec<&str> = design.components.iter().map(|c| c.name.as_str()).collect();
/// assert_eq!(names, ["app", "lib"]);
/// // lib uses nothing, so it is the foundation; app sits above it.
/// assert_eq!(design.components[1].layer, 0);
/// assert_eq!(design.components[0].layer, 1);
/// ```
#[must_use]
pub fn architecture(map: &CodeMap, scope: &str) -> Architecture {
    let scope = scope.trim_matches('/');
    let code: Vec<usize> = (0..map.nodes.len())
        .filter(|&n| is_design(map, n) && rest(&map.nodes[n].path, scope).is_some())
        .collect();
    let packages = packages(map);
    let (level, parts) = parts(map, &code, &packages, scope);
    let mut roots: Vec<(String, ComponentKind)> = parts;
    // Longest first, so that a file belongs to the deepest that holds it.
    roots.sort_by(|a, b| b.0.len().cmp(&a.0.len()).then(a.0.cmp(&b.0)));
    let holder = |path: &str| {
        roots.iter().position(|(root, kind)| {
            if *kind == ComponentKind::File {
                path == root
            } else {
                rest(path, root).is_some()
            }
        })
    };

    let mut members: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for &node in &code {
        if let Some(root) = holder(&map.nodes[node].path) {
            members.entry(root).or_default().push(node);
        }
    }
    // Only what holds code is a component; sorted by path.
    let mut held: Vec<usize> = members.keys().copied().collect();
    held.sort_by(|a, b| roots[*a].0.cmp(&roots[*b].0));
    let mut components: Vec<Component> = held
        .iter()
        .map(|root| {
            let (path, kind) = roots[*root].clone();
            let mut files = members.remove(root).unwrap_or_default();
            files.sort_by(|a, b| lines(map, *b).cmp(&lines(map, *a)).then(a.cmp(b)));
            Component {
                name: name(&path, &level, &packages),
                lines: files.iter().map(|f| lines(map, *f)).sum(),
                files,
                path,
                kind,
                layer: 0,
            }
        })
        .collect();
    // Two parts of the same name (`dbt/cloud`, `google/cloud`) are told
    // apart by their way from the level shown.
    let mut counts_by_name: BTreeMap<String, usize> = BTreeMap::new();
    for component in &components {
        *counts_by_name.entry(component.name.clone()).or_default() += 1;
    }
    for component in &mut components {
        if counts_by_name[&component.name] > 1 {
            component.name = rest(&component.path, &level)
                .unwrap_or(&component.path)
                .to_owned();
        }
    }
    let owners: Vec<Option<usize>> = map
        .nodes
        .iter()
        .map(|node| {
            let root = holder(&node.path)?;
            held.iter().position(|r| *r == root)
        })
        .collect();

    // Imports and calls through the API, per pair of parts.
    let mut counts: BTreeMap<(usize, usize), (usize, usize)> = BTreeMap::new();
    for edge in &map.edges {
        let calls = match edge.kind {
            EdgeKind::Imports => false,
            EdgeKind::Calls => true,
            EdgeKind::Contains => continue,
        };
        if !is_design(map, edge.from) || !is_design(map, edge.to) {
            continue;
        }
        if let (Some(from), Some(to)) = (owners[edge.from], owners[edge.to])
            && from != to
        {
            let count = counts.entry((from, to)).or_default();
            if calls {
                count.1 += 1;
            } else {
                count.0 += 1;
            }
        }
    }
    let uses: Vec<Vec<usize>> = (0..components.len())
        .map(|c| {
            counts
                .keys()
                .filter(|(from, _)| *from == c)
                .map(|(_, to)| *to)
                .collect()
        })
        .collect();

    let groups = strongly_connected(&uses);
    let mut group_of = vec![0; components.len()];
    for (g, members) in groups.iter().enumerate() {
        for &c in members {
            group_of[c] = g;
        }
    }
    // The groups come out with what they use before them, so that each
    // one's layer follows from layers already known.
    let mut group_layer = vec![0; groups.len()];
    for (g, members) in groups.iter().enumerate() {
        group_layer[g] = members
            .iter()
            .flat_map(|&c| &uses[c])
            .filter(|&&to| group_of[to] != g)
            .map(|&to| group_layer[group_of[to]] + 1)
            .max()
            .unwrap_or(0);
    }
    for (c, component) in components.iter_mut().enumerate() {
        component.layer = group_layer[group_of[c]];
    }

    let links: Vec<Link> = counts
        .into_iter()
        .map(|((from, to), (imports, calls))| Link {
            from,
            to,
            imports,
            calls,
            cyclic: group_of[from] == group_of[to],
        })
        .collect();
    let layers = order(&components, &links);
    Architecture {
        level,
        components,
        links,
        layers,
        owners,
    }
}

/// Whether a node is a source file whose imports are read.
fn is_code(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::File {
            language: Language::Rust
                | Language::Python
                | Language::TypeScript
                | Language::JavaScript,
            ..
        }
    )
}

fn lines(map: &CodeMap, node: usize) -> usize {
    match map.nodes[node].kind {
        NodeKind::File { lines, .. } => lines,
        NodeKind::Folder => 0,
    }
}

/// The folders that hold a package, by the manifest beside them. A
/// `Cargo.toml` counts only beside a crate root: a workspace's own is not a
/// package.
fn packages(map: &CodeMap) -> Vec<String> {
    let paths: HashSet<&str> = map.nodes.iter().map(|n| n.path.as_str()).collect();
    let mut found = Vec::new();
    for node in map
        .nodes
        .iter()
        .filter(|n| matches!(n.kind, NodeKind::Folder))
    {
        let inside = |name: &str| {
            if node.path.is_empty() {
                name.to_owned()
            } else {
                format!("{}/{name}", node.path)
            }
        };
        let cargo = paths.contains(inside("Cargo.toml").as_str())
            && (paths.contains(inside("src/lib.rs").as_str())
                || paths.contains(inside("src/main.rs").as_str()));
        let other = ["package.json", "pyproject.toml", "setup.py", "go.mod"]
            .iter()
            .any(|m| paths.contains(inside(m).as_str()));
        if cargo || other {
            found.push(node.path.clone());
        }
    }
    found
}

/// Whether a node is code of the design: a source file, not a test, a
/// document or an example.
fn is_design(map: &CodeMap, node: usize) -> bool {
    let path = &map.nodes[node].path;
    // `setup.py` names a package, so it is read first, but builds it.
    is_code(map.nodes[node].kind)
        && crate::map::worth(Path::new(path)) == 0
        && !path.ends_with("setup.py")
}

/// The parts of `scope` and the folder they are in: past the folders that
/// only lead to the code, each folder and file there, a folder that only
/// holds a few packages giving way to them.
fn parts(
    map: &CodeMap,
    code: &[usize],
    packages: &[String],
    scope: &str,
) -> (String, Vec<(String, ComponentKind)>) {
    let mut level = scope.to_owned();
    let (firsts, below) = loop {
        let below: Vec<&str> = code
            .iter()
            .map(|&n| map.nodes[n].path.as_str())
            .filter_map(|p| rest(p, &level))
            .collect();
        let mut firsts: Vec<&str> = below.iter().map(|p| first(p)).collect();
        firsts.sort_unstable();
        firsts.dedup();
        let loose = below.iter().any(|p| !p.contains('/'));
        match firsts.as_slice() {
            [only] if !loose => level = child(&level, only),
            _ => break (firsts, below),
        }
    };
    let mut parts = Vec::new();
    for name in firsts {
        let path = child(&level, name);
        if below.contains(&name) {
            parts.push((path, ComponentKind::File));
            continue;
        }
        let inside: Vec<&String> = packages
            .iter()
            .filter(|p| rest(p, &path).is_some_and(|r| !r.is_empty()))
            .collect();
        // The packages not inside another of them.
        let tops: Vec<&String> = inside
            .iter()
            .filter(|p| !inside.iter().any(|q| q != *p && rest(p, q).is_some()))
            .copied()
            .collect();
        let covered = code
            .iter()
            .map(|&n| map.nodes[n].path.as_str())
            .filter(|f| rest(f, &path).is_some())
            .all(|f| tops.iter().any(|t| rest(f, t).is_some()));
        let own = packages.contains(&path);
        if !own && covered && !tops.is_empty() && tops.len() <= INLINE_PACKAGES {
            parts.extend(
                tops.into_iter()
                    .map(|t| (t.clone(), ComponentKind::Package)),
            );
        } else if own {
            parts.push((path, ComponentKind::Package));
        } else if inside.len() >= 2 {
            parts.push((
                path,
                ComponentKind::Group {
                    packages: inside.len(),
                },
            ));
        } else {
            parts.push((path, ComponentKind::Folder));
        }
    }
    // Only parts that hold code of the design.
    parts.retain(|(path, kind)| {
        *kind == ComponentKind::File
            || code
                .iter()
                .any(|&n| rest(&map.nodes[n].path, path).is_some_and(|r| !r.is_empty()))
    });
    (level, parts)
}

/// A part's name: its folder's or file's, and for the `src` of a package
/// the package's own.
fn name(path: &str, level: &str, packages: &[String]) -> String {
    let last = path.rsplit('/').next().unwrap_or_default();
    if last == "src" && packages.iter().any(|p| p == level) {
        level.rsplit('/').next().unwrap_or_default().to_owned()
    } else {
        last.to_owned()
    }
}

/// `path` from inside `folder`, if it is inside.
fn rest<'a>(path: &'a str, folder: &str) -> Option<&'a str> {
    if folder.is_empty() {
        Some(path)
    } else {
        path.strip_prefix(folder)?.strip_prefix('/')
    }
}

fn first(path: &str) -> &str {
    path.split('/').next().unwrap_or(path)
}

fn child(folder: &str, name: &str) -> String {
    if folder.is_empty() {
        name.to_owned()
    } else {
        format!("{folder}/{name}")
    }
}

/// The groups of nodes that reach each other (Tarjan's), each group after
/// every group it reaches.
fn strongly_connected(uses: &[Vec<usize>]) -> Vec<Vec<usize>> {
    struct Search<'a> {
        uses: &'a [Vec<usize>],
        index: Vec<Option<usize>>,
        low: Vec<usize>,
        stack: Vec<usize>,
        on_stack: Vec<bool>,
        next: usize,
        groups: Vec<Vec<usize>>,
    }
    impl Search<'_> {
        fn visit(&mut self, v: usize) {
            self.index[v] = Some(self.next);
            self.low[v] = self.next;
            self.next += 1;
            self.stack.push(v);
            self.on_stack[v] = true;
            for i in 0..self.uses[v].len() {
                let w = self.uses[v][i];
                match self.index[w] {
                    None => {
                        self.visit(w);
                        self.low[v] = self.low[v].min(self.low[w]);
                    }
                    Some(index) if self.on_stack[w] => self.low[v] = self.low[v].min(index),
                    Some(_) => {}
                }
            }
            if Some(self.low[v]) == self.index[v] {
                let mut group = Vec::new();
                while let Some(w) = self.stack.pop() {
                    self.on_stack[w] = false;
                    group.push(w);
                    if w == v {
                        break;
                    }
                }
                group.sort_unstable();
                self.groups.push(group);
            }
        }
    }
    let n = uses.len();
    let mut search = Search {
        uses,
        index: vec![None; n],
        low: vec![0; n],
        stack: Vec::new(),
        on_stack: vec![false; n],
        next: 0,
        groups: Vec::new(),
    };
    for v in 0..n {
        if search.index[v].is_none() {
            search.visit(v);
        }
    }
    search.groups
}

/// Each layer's components in the order that crosses links the least
/// found: each moved toward the middle of those it is linked to, a few
/// times over.
fn order(components: &[Component], links: &[Link]) -> Vec<Vec<usize>> {
    let height = components.iter().map(|c| c.layer + 1).max().unwrap_or(0);
    let mut layers: Vec<Vec<usize>> = vec![Vec::new(); height];
    for (c, component) in components.iter().enumerate() {
        layers[component.layer].push(c);
    }
    let mut place = vec![0.0_f32; components.len()];
    let measure = |layer: &[usize], place: &mut [f32]| {
        let n = layer.len().max(1) as f32;
        for (i, &c) in layer.iter().enumerate() {
            place[c] = (i as f32 + 0.5) / n;
        }
    };
    for layer in &layers {
        measure(layer, &mut place);
    }
    // Up from the foundations, then down, and so on: each layer placed
    // against where its neighbours just went, which settles rather than
    // swapping back and forth.
    for sweep in 0..SWEEPS {
        let mut rows: Vec<usize> = (0..height).collect();
        if sweep % 2 == 1 {
            rows.reverse();
        }
        for row in rows {
            let centre = |c: usize| {
                let linked: Vec<f32> = links
                    .iter()
                    .filter_map(|l| match (l.from == c, l.to == c) {
                        (true, _) => Some(l.to),
                        (_, true) => Some(l.from),
                        _ => None,
                    })
                    .filter(|&o| components[o].layer != components[c].layer)
                    .map(|o| place[o])
                    .collect();
                if linked.is_empty() {
                    place[c]
                } else {
                    linked.iter().sum::<f32>() / linked.len() as f32
                }
            };
            let mut keyed: Vec<(f32, usize)> =
                layers[row].iter().map(|&c| (centre(c), c)).collect();
            keyed.sort_by(|a, b| {
                a.0.total_cmp(&b.0)
                    .then_with(|| components[a.1].path.cmp(&components[b.1].path))
            });
            layers[row] = keyed.into_iter().map(|(_, c)| c).collect();
            measure(&layers[row], &mut place);
        }
    }
    layers
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(root: &std::path::Path, path: &str, text: &str) {
        let path = root.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn named<'a>(design: &'a Architecture, name: &str) -> &'a Component {
        design.components.iter().find(|c| c.name == name).unwrap()
    }

    #[test]
    fn crates_of_a_workspace_stack_up_from_the_one_that_uses_none() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "Cargo.toml", "[workspace]\n");
        write(
            root,
            "crates/base/Cargo.toml",
            "[package]\nname = \"base\"\n",
        );
        write(root, "crates/base/src/lib.rs", "pub fn f() {}\n");
        write(root, "crates/mid/Cargo.toml", "[package]\nname = \"mid\"\n");
        write(root, "crates/mid/src/lib.rs", "use base::f;\nmod inner;\n");
        write(root, "crates/mid/src/inner.rs", "use base::f;\n\n\n");
        write(root, "crates/top/Cargo.toml", "[package]\nname = \"top\"\n");
        write(
            root,
            "crates/top/src/main.rs",
            "use mid::x;\nuse base::f;\n",
        );
        write(root, "README.md", "# hi\n");
        let map = crate::map(root, 100);
        let design = architecture(&map, "");

        let names: Vec<&str> = design.components.iter().map(|c| c.name.as_str()).collect();
        // The workspace's own Cargo.toml is no package, and Markdown no code.
        assert_eq!(names, ["base", "mid", "top"]);
        assert_eq!(named(&design, "base").layer, 0);
        assert_eq!(named(&design, "mid").layer, 1);
        assert_eq!(named(&design, "top").layer, 2);
        assert_eq!(design.layers, [vec![0], vec![1], vec![2]]);
        // Both of mid's files import base: the link counts them.
        let mid_base = design.links.iter().find(|l| l.from == 1 && l.to == 0);
        assert_eq!(mid_base.map(|l| l.imports), Some(2));
        assert_eq!(design.cycles(), 0);
        // Longest first, and a file knows its component.
        let mid = named(&design, "mid");
        assert_eq!(map.nodes[mid.files[0]].path, "crates/mid/src/inner.rs");
        assert_eq!(design.owner(mid.files[1]), Some(1));
        assert_eq!(design.owner(0), None);
    }

    #[test]
    fn folders_that_use_each_other_share_a_layer_and_their_links_are_a_cycle() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        // One package: its parts are past `src`, a folder each, a file each.
        write(root, "pyproject.toml", "");
        write(root, "src/a/x.py", "from ..b.y import f\n");
        write(root, "src/b/y.py", "from ..a.x import f\n");
        write(root, "src/c/z.py", "from ..a.x import f\n");
        write(root, "src/main.py", "from .c.z import f\n");
        let map = crate::map(root, 100);
        let design = architecture(&map, "");

        let paths: Vec<&str> = design.components.iter().map(|c| c.path.as_str()).collect();
        assert_eq!(paths, ["src/a", "src/b", "src/c", "src/main.py"]);
        assert_eq!(named(&design, "a").layer, 0);
        assert_eq!(named(&design, "b").layer, 0);
        assert_eq!(named(&design, "c").layer, 1);
        assert_eq!(named(&design, "main.py").layer, 2);
        assert_eq!(design.cycles(), 2);
        let c_a = design.links.iter().find(|l| l.from == 2 && l.to == 0);
        assert!(c_a.is_some_and(|l| !l.cyclic));
    }

    #[test]
    fn the_order_in_a_layer_follows_what_each_is_linked_to() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        // Named so that alphabetical order would cross both links.
        write(root, "a/x.py", "import d.w\n");
        write(root, "b/x.py", "import c.w\n");
        write(root, "c/w.py", "");
        write(root, "d/w.py", "");
        write(root, "e/w.py", "import c.w\nimport d.w\n");
        let map = crate::map(root, 100);
        let design = architecture(&map, "");
        let name = |c: usize| design.components[c].name.as_str();
        let top: Vec<&str> = design.layers[1].iter().map(|&c| name(c)).collect();
        let bottom: Vec<&str> = design.layers[0].iter().map(|&c| name(c)).collect();
        let a = top.iter().position(|n| *n == "a").unwrap();
        let b = top.iter().position(|n| *n == "b").unwrap();
        let c = bottom.iter().position(|n| *n == "c").unwrap();
        let d = bottom.iter().position(|n| *n == "d").unwrap();
        assert_eq!(a < b, d < c, "top {top:?}, bottom {bottom:?}");
    }
}
