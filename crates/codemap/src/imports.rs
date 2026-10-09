//! What a file imports, as the files of the project it names.
//!
//! Patterns per language, not a parser: enough for the shape of a codebase,
//! and an import that resolves to no file of the project is dropped rather
//! than guessed at.

use std::collections::HashMap;
use std::path::Path;
use std::sync::LazyLock;

use regex::Regex;

use crate::map::{CodeMap, Language, NodeKind};

static RUST_MOD: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"(?m)^\s*(?:pub(?:\([^)]*\))?\s+)?mod\s+([A-Za-z_]\w*)\s*;").ok());
static RUST_USE: LazyLock<Option<Regex>> = LazyLock::new(|| {
    Regex::new(r"(?m)^\s*(?:pub(?:\([^)]*\))?\s+)?use\s+([A-Za-z_]\w*)((?:::[A-Za-z_]\w*)*)").ok()
});
/// A path through a crate written in code rather than imported:
/// `ironquill_gui::run(..)`.
static RUST_PATH: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"(?:^|[^\w:])([A-Za-z_]\w*)::").ok());
static PY_FROM: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"(?m)^\s*from\s+(\.*)([\w.]*)\s+import\b").ok());
static PY_IMPORT: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"(?m)^\s*import\s+([\w.]+)").ok());
static JS_IMPORT: LazyLock<Option<Regex>> = LazyLock::new(|| {
    Regex::new(r#"(?:\bfrom\s+|\bimport\s*\(?\s*|\brequire\(\s*)['"]([^'"\s]+)['"]"#).ok()
});
static CARGO_NAME: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r#"(?m)^\s*name\s*=\s*"([^"]+)""#).ok());

/// What resolving imports needs to know of the whole project, gathered once:
/// the files by path, the workspace's Rust crates, the folders Python
/// packages are imported from, and the path aliases of TypeScript projects.
pub(crate) struct Index {
    files: HashMap<String, usize>,
    crates: HashMap<String, String>,
    python_modules: HashMap<String, String>,
    aliases: HashMap<String, Vec<Alias>>,
}

/// A TypeScript path alias: imports starting with `prefix` (and, for a
/// pattern with `*`, ending with `suffix`) are looked for at `targets`, a
/// `*` in them standing for what the pattern's `*` matched.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Alias {
    prefix: String,
    suffix: String,
    wildcard: bool,
    targets: Vec<String>,
}

impl Index {
    pub(crate) fn new(root: &Path, map: &CodeMap) -> Self {
        let files = map
            .nodes
            .iter()
            .enumerate()
            .filter(|(_, n)| matches!(n.kind, NodeKind::File { .. }))
            .map(|(i, n)| (n.path.clone(), i))
            .collect();
        let mut index = Self {
            files,
            crates: HashMap::new(),
            python_modules: HashMap::new(),
            aliases: HashMap::new(),
        };
        index.crates = rust_crates(root, &index);
        index.python_modules = python_modules(&index);
        index.aliases = typescript_aliases(root, &index);
        index
    }

    /// The file at `path`, if the project has one: an import names a file,
    /// never a folder of the same name.
    fn file(&self, path: &str) -> Option<usize> {
        self.files.get(path).copied()
    }

    /// The files' paths, for what is found by name.
    fn paths(&self) -> impl Iterator<Item = &str> {
        self.files.keys().map(String::as_str)
    }
}

/// The workspace's Rust crates, by the name code uses for them
/// (`ironquill_ui`), with the file their path starts from.
fn rust_crates(root: &Path, index: &Index) -> HashMap<String, String> {
    let Some(name) = CARGO_NAME.as_ref() else {
        return HashMap::new();
    };
    let mut crates = HashMap::new();
    for path in index.paths().filter(|p| file_name(p) == "Cargo.toml") {
        let Ok(text) = std::fs::read_to_string(root.join(path)) else {
            continue;
        };
        // The package's name, not a dependency's: the first `name` of the
        // [package] table.
        let Some(package) = text.split("[package]").nth(1) else {
            continue;
        };
        let package = package.split("\n[").next().unwrap_or(package);
        let Some(found) = name.captures(package).and_then(|c| c.get(1)) else {
            continue;
        };
        let dir = parent(path);
        let start = ["src/lib.rs", "src/main.rs"]
            .iter()
            .map(|f| join(dir, f))
            .find(|f| index.file(f).is_some());
        if let Some(start) = start {
            crates.insert(found.as_str().replace('-', "_"), start);
        }
    }
    crates
}

/// Every Python module by its dotted name, as imported from the folders
/// packages are imported from: each Python project's own, and its `src`
/// when it has one, as in a monorepo of many packages sharing a namespace
/// (`airflow.providers.amazon` beside `airflow.models`), then the root.
fn python_modules(index: &Index) -> HashMap<String, String> {
    let mut roots: Vec<String> = index
        .paths()
        .filter(|p| matches!(file_name(p), "pyproject.toml" | "setup.py" | "setup.cfg"))
        .flat_map(|p| {
            let dir = parent(p).to_owned();
            [join(&dir, "src"), dir]
        })
        .collect();
    roots.push(String::new());
    // The deepest first: a module found from its own project's folder
    // rather than as a path from the root.
    roots.sort_by(|a, b| b.len().cmp(&a.len()).then(a.cmp(b)));
    roots.dedup();
    let mut sources: Vec<&str> = index.paths().filter(|p| p.ends_with(".py")).collect();
    sources.sort_unstable();
    let mut modules = HashMap::new();
    for root in &roots {
        for path in &sources {
            let inside = if root.is_empty() {
                Some(*path)
            } else {
                path.strip_prefix(root.as_str())
                    .and_then(|p| p.strip_prefix('/'))
            };
            let Some(inside) = inside else {
                continue;
            };
            let stem = inside.trim_end_matches(".py");
            let stem = stem.strip_suffix("/__init__").unwrap_or(stem);
            if stem == "__init__" {
                continue;
            }
            modules
                .entry(stem.replace('/', "."))
                .or_insert_with(|| (*path).to_owned());
        }
    }
    modules
}

/// The path aliases of each TypeScript project, by the folder of its
/// `tsconfig*.json` files.
fn typescript_aliases(root: &Path, index: &Index) -> HashMap<String, Vec<Alias>> {
    let mut aliases: HashMap<String, Vec<Alias>> = HashMap::new();
    let mut configs: Vec<&str> = index
        .paths()
        .filter(|p| {
            let name = file_name(p);
            name.starts_with("tsconfig") && name.ends_with(".json") || name == "jsconfig.json"
        })
        .collect();
    configs.sort_unstable();
    for config in configs {
        let found = read_aliases(root, config, 0);
        if !found.is_empty() {
            aliases
                .entry(parent(config).to_owned())
                .or_default()
                .extend(found);
        }
    }
    // The longest prefix first: the most precise alias wins.
    for list in aliases.values_mut() {
        list.sort_by(|a, b| {
            b.prefix
                .len()
                .cmp(&a.prefix.len())
                .then(a.prefix.cmp(&b.prefix))
        });
        list.dedup();
    }
    aliases
}

/// The aliases a `tsconfig` declares, or inherits through `extends`.
fn read_aliases(root: &Path, config: &str, depth: usize) -> Vec<Alias> {
    let Some(json) = std::fs::read_to_string(root.join(config))
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&without_comments(&text)).ok())
    else {
        return Vec::new();
    };
    let dir = parent(config);
    let options = &json["compilerOptions"];
    let base = options["baseUrl"]
        .as_str()
        .map_or(dir.to_owned(), |b| join(dir, b));
    let mut aliases: Vec<Alias> = options["paths"]
        .as_object()
        .into_iter()
        .flatten()
        .map(|(pattern, targets)| {
            let (prefix, suffix, wildcard) = match pattern.split_once('*') {
                Some((prefix, suffix)) => (prefix.to_owned(), suffix.to_owned(), true),
                None => (pattern.clone(), String::new(), false),
            };
            let targets = targets
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|t| t.as_str())
                .map(|t| join(&base, t))
                .collect();
            Alias {
                prefix,
                suffix,
                wildcard,
                targets,
            }
        })
        .collect();
    // A `tsconfig` that extends another, from this project: its aliases
    // too, unless it declares its own.
    if aliases.is_empty()
        && depth < 4
        && let Some(extends) = json["extends"].as_str().filter(|e| e.starts_with('.'))
    {
        let mut path = join(dir, extends);
        if !path.ends_with(".json") {
            path.push_str(".json");
        }
        aliases = read_aliases(root, &path, depth + 1);
    }
    aliases
}

/// JSON with comments and trailing commas, as `tsconfig.json` allows, made
/// plain JSON.
fn without_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    let mut in_string = false;
    while let Some(c) = chars.next() {
        if in_string {
            out.push(c);
            match c {
                '\\' => {
                    if let Some(next) = chars.next() {
                        out.push(next);
                    }
                }
                '"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match (c, chars.peek()) {
            ('"', _) => {
                in_string = true;
                out.push(c);
            }
            ('/', Some('/')) => {
                for c in chars.by_ref() {
                    if c == '\n' {
                        out.push('\n');
                        break;
                    }
                }
            }
            ('/', Some('*')) => {
                chars.next();
                let mut last = ' ';
                for c in chars.by_ref() {
                    if last == '*' && c == '/' {
                        break;
                    }
                    last = c;
                }
            }
            _ => out.push(c),
        }
    }
    // A comma before a closing bracket or brace.
    static TRAILING: LazyLock<Option<Regex>> = LazyLock::new(|| Regex::new(r",(\s*[\]}])").ok());
    match TRAILING.as_ref() {
        Some(re) => re.replace_all(&out, "$1").into_owned(),
        None => out,
    }
}

/// The nodes `path`, written in `language`, imports.
pub(crate) fn of(path: &str, language: Language, text: &str, index: &Index) -> Vec<usize> {
    let found = match language {
        Language::Rust => rust(path, text, index),
        Language::Python => python(path, text, index),
        Language::TypeScript | Language::JavaScript => javascript(path, text, index),
        _ => Vec::new(),
    };
    let mut nodes: Vec<usize> = found.iter().filter_map(|p| index.file(p)).collect();
    nodes.sort_unstable();
    nodes.dedup();
    nodes
}

fn rust(path: &str, text: &str, index: &Index) -> Vec<String> {
    let crates = &index.crates;
    let mut found = Vec::new();
    let dir = parent(path);
    let name = path.rsplit('/').next().unwrap_or(path);
    // Where `mod x;` looks: beside a crate root or a mod.rs, in a folder of
    // the file's own name otherwise.
    let modules = match name {
        "lib.rs" | "main.rs" | "mod.rs" => dir.to_owned(),
        _ => join(dir, name.trim_end_matches(".rs")),
    };
    if let Some(re) = RUST_MOD.as_ref() {
        for c in re.captures_iter(text) {
            let name = &c[1];
            found.push(join(&modules, &format!("{name}.rs")));
            found.push(join(&modules, &format!("{name}/mod.rs")));
        }
    }
    let Some(re) = RUST_USE.as_ref() else {
        return found;
    };
    for c in re.captures_iter(text) {
        let segments: Vec<&str> = c[2].split("::").filter(|s| !s.is_empty()).collect();
        let start = match &c[1] {
            "crate" => crate_root(path, index),
            "self" | "super" | "std" | "core" | "alloc" => None,
            other => crates.get(other).cloned(),
        };
        let Some(start) = start else {
            continue;
        };
        // The deepest module the path names, else the crate itself.
        let base = parent(&start).to_owned();
        let mut target = start.clone();
        for depth in (1..=segments.len()).rev() {
            let module = segments[..depth].join("/");
            let candidates = [
                join(&base, &format!("{module}.rs")),
                join(&base, &format!("{module}/mod.rs")),
            ];
            if let Some(hit) = candidates.into_iter().find(|p| index.file(p).is_some()) {
                target = hit;
                break;
            }
        }
        found.push(target);
    }
    // A workspace crate named in a path, not imported: the crate itself,
    // unless something of it was imported already.
    if let Some(re) = RUST_PATH.as_ref() {
        // Outside comments: a doc naming a crate is no use of it.
        let code: String = text
            .lines()
            .map(|line| line.split("//").next().unwrap_or(line))
            .collect::<Vec<_>>()
            .join("\n");
        for c in re.captures_iter(&code) {
            let Some(start) = crates.get(&c[1]) else {
                continue;
            };
            let folder = format!("{}/", parent(parent(start)));
            if !found.iter().any(|f| f.starts_with(&folder)) {
                found.push(start.clone());
            }
        }
    }
    found
}

/// The crate root above `path`: the nearest lib.rs or main.rs beside it or
/// in a folder above.
fn crate_root(path: &str, index: &Index) -> Option<String> {
    let mut dir = parent(path);
    loop {
        for root in ["lib.rs", "main.rs"] {
            let candidate = join(dir, root);
            if index.file(&candidate).is_some() {
                return Some(candidate);
            }
        }
        if dir.is_empty() {
            return None;
        }
        dir = parent(dir);
    }
}

fn python(path: &str, text: &str, index: &Index) -> Vec<String> {
    let mut found = Vec::new();
    let dir = parent(path);
    if let Some(re) = PY_FROM.as_ref() {
        for c in re.captures_iter(text) {
            let dots = c[1].len();
            let module = &c[2];
            if dots > 0 {
                // `from ..pkg import x`: one dot is the file's own package.
                let mut base = dir.to_owned();
                for _ in 1..dots {
                    base = parent(&base).to_owned();
                }
                if !module.is_empty() {
                    found.extend(module_files(&base, module));
                }
            } else {
                found.extend(absolute(dir, module, index));
            }
        }
    }
    if let Some(re) = PY_IMPORT.as_ref() {
        for c in re.captures_iter(text) {
            found.extend(absolute(dir, &c[1], index));
        }
    }
    found
}

/// Where module `module` is, from folder `base`: a file of its name, or a
/// package's `__init__.py`.
fn module_files(base: &str, module: &str) -> [String; 2] {
    let module = module.replace('.', "/");
    [
        join(base, &format!("{module}.py")),
        join(base, &format!("{module}/__init__.py")),
    ]
}

/// An absolute Python module: the longest part of its name that is a
/// module of the project, looked for from the file's folder up to the
/// root, then among the modules of every Python project.
fn absolute(dir: &str, module: &str, index: &Index) -> Option<String> {
    let parts: Vec<&str> = module.split('.').collect();
    for depth in (1..=parts.len()).rev() {
        let name = parts[..depth].join(".");
        let mut base = dir;
        loop {
            if let Some(hit) = module_files(base, &name)
                .into_iter()
                .find(|p| index.file(p).is_some())
            {
                return Some(hit);
            }
            if base.is_empty() {
                break;
            }
            base = parent(base);
        }
        if let Some(hit) = index.python_modules.get(&name) {
            return Some(hit.clone());
        }
    }
    None
}

fn javascript(path: &str, text: &str, index: &Index) -> Vec<String> {
    let Some(re) = JS_IMPORT.as_ref() else {
        return Vec::new();
    };
    let dir = parent(path);
    let mut found = Vec::new();
    for c in re.captures_iter(text) {
        let written = &c[1];
        let targets = if written.starts_with("./") || written.starts_with("../") {
            vec![join(dir, written)]
        } else {
            aliased(dir, written, index)
        };
        if let Some(hit) = targets.iter().find_map(|t| script(t, index)) {
            found.push(hit);
        }
    }
    found
}

/// The file `target` names, as bundlers and TypeScript look: the path as
/// written, with each extension, as a folder's index, and `.js` naming a
/// `.ts` source.
fn script(target: &str, index: &Index) -> Option<String> {
    let stem = target.strip_suffix(".js").unwrap_or(target);
    let mut candidates = vec![target.to_owned()];
    for ext in ["ts", "tsx", "js", "jsx", "mjs", "cjs", "d.ts"] {
        candidates.push(format!("{stem}.{ext}"));
        candidates.push(join(target, &format!("index.{ext}")));
    }
    candidates.into_iter().find(|p| index.file(p).is_some())
}

/// Where an import that is not a relative path may be, by the aliases of
/// the nearest TypeScript project above `dir`: `src/utils` and
/// `openapi/queries` rather than a package of `node_modules`.
fn aliased(dir: &str, written: &str, index: &Index) -> Vec<String> {
    let mut at = dir;
    let aliases = loop {
        if let Some(aliases) = index.aliases.get(at) {
            break aliases;
        }
        if at.is_empty() {
            return Vec::new();
        }
        at = parent(at);
    };
    let mut targets = Vec::new();
    for alias in aliases {
        let matched = if alias.wildcard {
            written
                .strip_prefix(alias.prefix.as_str())
                .and_then(|rest| rest.strip_suffix(alias.suffix.as_str()))
        } else {
            (written == alias.prefix).then_some("")
        };
        if let Some(matched) = matched {
            targets.extend(alias.targets.iter().map(|t| t.replacen('*', matched, 1)));
        }
    }
    targets
}

/// The last part of a path.
fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// The folder part of a path, empty at the root.
fn parent(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(dir, _)| dir)
}

/// `rel` from `dir`, with `.` and `..` resolved; `..` past the root stays
/// at the root.
fn join(dir: &str, rel: &str) -> String {
    let mut parts: Vec<&str> = dir.split('/').filter(|p| !p.is_empty()).collect();
    for part in rel.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            name => parts.push(name),
        }
    }
    parts.join("/")
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use crate::map::{EdgeKind, map};

    fn write(root: &Path, path: &str, text: &str) {
        let path = root.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn imports(root: &Path, from: &str) -> Vec<String> {
        let map = map(root, 1000);
        let node = map.find(from).unwrap();
        let mut found: Vec<String> = map
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Imports && e.from == node)
            .map(|e| map.nodes[e.to].path.clone())
            .collect();
        found.sort();
        found
    }

    #[test]
    fn rust_modules_crate_paths_and_workspace_crates() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(
            root,
            "a/Cargo.toml",
            "[package]\nname = \"my-ui\"\n[dependencies]\nname = \"x\"\n",
        );
        write(root, "a/src/lib.rs", "mod view;\npub mod keys;\n");
        write(root, "a/src/view.rs", "use crate::keys::Key;\nmod parts;\n");
        write(root, "a/src/view/parts.rs", "");
        write(root, "a/src/keys/mod.rs", "");
        write(root, "b/Cargo.toml", "[package]\nname = \"app\"\n");
        write(
            root,
            "b/src/main.rs",
            "use my_ui::view::Thing;\nuse std::io;\nfn main() { my_ui::run(); }\n",
        );
        write(
            root,
            "b/src/cli.rs",
            "/// Not `my_ui::view::Thing`.\nfn f() { my_ui::run(); } // my_ui::view\n",
        );
        assert_eq!(
            imports(root, "a/src/lib.rs"),
            ["a/src/keys/mod.rs", "a/src/view.rs"]
        );
        assert_eq!(
            imports(root, "a/src/view.rs"),
            ["a/src/keys/mod.rs", "a/src/view/parts.rs"]
        );
        assert_eq!(imports(root, "b/src/main.rs"), ["a/src/view.rs"]);
        assert_eq!(imports(root, "b/src/cli.rs"), ["a/src/lib.rs"]);
    }

    #[test]
    fn python_relative_and_absolute_imports() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "app/__init__.py", "");
        write(root, "app/models.py", "");
        write(
            root,
            "app/api/views.py",
            "from ..models import User\nimport app.models\nimport os\n",
        );
        write(root, "app/api/__init__.py", "from .views import index\n");
        assert_eq!(imports(root, "app/api/views.py"), ["app/models.py"]);
        assert_eq!(imports(root, "app/api/__init__.py"), ["app/api/views.py"]);
    }

    #[test]
    fn javascript_relative_paths_with_or_without_extensions() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(
            root,
            "src/app.ts",
            "import { a } from './util';\nimport b from \"../lib/index.js\";\nimport React from 'react';\nconst c = require('./c');\n",
        );
        write(root, "src/util.ts", "");
        write(root, "src/c/index.js", "");
        write(root, "lib/index.ts", "");
        assert_eq!(
            imports(root, "src/app.ts"),
            ["lib/index.ts", "src/c/index.js", "src/util.ts"]
        );
    }

    #[test]
    fn paths_join_with_dots_resolved() {
        assert_eq!(super::join("a/b", "../c/./d"), "a/c/d");
        assert_eq!(super::join("", "../x"), "x");
        assert_eq!(super::parent("a/b/c.rs"), "a/b");
        assert_eq!(super::parent("c.rs"), "");
    }

    #[test]
    fn python_packages_of_a_monorepo_import_each_other_from_their_src() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "core/pyproject.toml", "");
        write(root, "core/src/airflow/__init__.py", "");
        write(root, "core/src/airflow/models/__init__.py", "");
        write(root, "core/src/airflow/models/dag.py", "");
        write(root, "providers/amazon/pyproject.toml", "");
        write(
            root,
            "providers/amazon/src/airflow/providers/amazon/hooks.py",
            "from airflow.models.dag import DAG\nimport airflow.models\nimport boto3\n",
        );
        assert_eq!(
            imports(
                root,
                "providers/amazon/src/airflow/providers/amazon/hooks.py"
            ),
            [
                "core/src/airflow/models/__init__.py",
                "core/src/airflow/models/dag.py"
            ]
        );
    }

    #[test]
    fn typescript_aliases_from_a_tsconfig_with_comments() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(
            root,
            "ui/tsconfig.app.json",
            "{\n  // Aliases.\n  \"compilerOptions\": {\n    /* bundler */\n    \"paths\": {\n      \"src/*\": [\"./src/*\"],\n      \"openapi/*\": [\"./openapi-gen/*\"],\n      \"*\": [\"./*\"],\n    },\n  },\n}\n",
        );
        write(
            root,
            "ui/src/pages/Dag.tsx",
            "import { useDag } from \"openapi/queries\";\nimport Time from \"src/components/Time\";\nimport React from \"react\";\n",
        );
        write(root, "ui/openapi-gen/queries/index.ts", "");
        write(root, "ui/src/components/Time.tsx", "");
        assert_eq!(
            imports(root, "ui/src/pages/Dag.tsx"),
            [
                "ui/openapi-gen/queries/index.ts",
                "ui/src/components/Time.tsx"
            ]
        );
    }

    #[test]
    fn comments_and_trailing_commas_leave_json() {
        let text = "{ \"a\": \"// not a comment\", /* gone */ \"b\": [1, 2,], } // end";
        let json: serde_json::Value = serde_json::from_str(&super::without_comments(text)).unwrap();
        assert_eq!(json["a"], "// not a comment");
        assert_eq!(json["b"], serde_json::json!([1, 2]));
    }
}
