//! What a file imports, as the files of the project it names.
//!
//! Patterns per language, not a parser: enough for the shape of a codebase,
//! and an import that resolves to no file of the project is dropped rather
//! than guessed at.

use std::collections::HashMap;
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
    Regex::new(r#"(?:\bfrom\s+|\bimport\s*\(?\s*|\brequire\(\s*)['"](\.{1,2}/[^'"]+)['"]"#).ok()
});
static CARGO_NAME: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r#"(?m)^\s*name\s*=\s*"([^"]+)""#).ok());

/// The workspace's Rust crates, by the name code uses for them
/// (`ironquill_ui`), with the file their path starts from.
pub(crate) fn rust_crates(root: &std::path::Path, map: &CodeMap) -> HashMap<String, String> {
    let Some(name) = CARGO_NAME.as_ref() else {
        return HashMap::new();
    };
    let mut crates = HashMap::new();
    for node in &map.nodes {
        if node.name() != "Cargo.toml" {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(root.join(&node.path)) else {
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
        let dir = parent(&node.path);
        let start = ["src/lib.rs", "src/main.rs"]
            .iter()
            .map(|f| join(dir, f))
            .find(|f| file(map, f).is_some());
        if let Some(start) = start {
            crates.insert(found.as_str().replace('-', "_"), start);
        }
    }
    crates
}

/// The nodes `path`, written in `language`, imports.
pub(crate) fn of(
    path: &str,
    language: Language,
    text: &str,
    map: &CodeMap,
    crates: &HashMap<String, String>,
) -> Vec<usize> {
    let found = match language {
        Language::Rust => rust(path, text, map, crates),
        Language::Python => python(path, text, map),
        Language::TypeScript | Language::JavaScript => javascript(path, text, map),
        _ => Vec::new(),
    };
    let mut nodes: Vec<usize> = found.iter().filter_map(|p| file(map, p)).collect();
    nodes.sort_unstable();
    nodes.dedup();
    nodes
}

fn rust(path: &str, text: &str, map: &CodeMap, crates: &HashMap<String, String>) -> Vec<String> {
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
            "crate" => crate_root(path, map),
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
            if let Some(hit) = candidates.into_iter().find(|p| file(map, p).is_some()) {
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
fn crate_root(path: &str, map: &CodeMap) -> Option<String> {
    let mut dir = parent(path);
    loop {
        for root in ["lib.rs", "main.rs"] {
            let candidate = join(dir, root);
            if file(map, &candidate).is_some() {
                return Some(candidate);
            }
        }
        if dir.is_empty() {
            return None;
        }
        dir = parent(dir);
    }
}

fn python(path: &str, text: &str, map: &CodeMap) -> Vec<String> {
    let mut found = Vec::new();
    let dir = parent(path);
    let module_files = |base: &str, module: &str| {
        let module = module.replace('.', "/");
        [
            join(base, &format!("{module}.py")),
            join(base, &format!("{module}/__init__.py")),
        ]
    };
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
                found.extend(absolute(dir, module, map, &module_files));
            }
        }
    }
    if let Some(re) = PY_IMPORT.as_ref() {
        for c in re.captures_iter(text) {
            found.extend(absolute(dir, &c[1], map, &module_files));
        }
    }
    found
}

/// An absolute Python module, looked for from the file's folder up to the
/// root, the longest part of its name that is a file of the project.
fn absolute(
    dir: &str,
    module: &str,
    map: &CodeMap,
    files: &dyn Fn(&str, &str) -> [String; 2],
) -> Vec<String> {
    let parts: Vec<&str> = module.split('.').collect();
    let mut base = dir;
    loop {
        for depth in (1..=parts.len()).rev() {
            let name = parts[..depth].join(".");
            if let Some(hit) = files(base, &name)
                .into_iter()
                .find(|p| file(map, p).is_some())
            {
                return vec![hit];
            }
        }
        if base.is_empty() {
            return Vec::new();
        }
        base = parent(base);
    }
}

fn javascript(path: &str, text: &str, map: &CodeMap) -> Vec<String> {
    let Some(re) = JS_IMPORT.as_ref() else {
        return Vec::new();
    };
    let dir = parent(path);
    let mut found = Vec::new();
    for c in re.captures_iter(text) {
        let target = join(dir, &c[1]);
        // As bundlers and TypeScript look: the path as written, with each
        // extension, as a folder's index, and `.js` naming a `.ts` source.
        let stem = target
            .strip_suffix(".js")
            .map_or(target.clone(), str::to_owned);
        let mut candidates = vec![target.clone()];
        for ext in ["ts", "tsx", "js", "jsx", "mjs", "cjs"] {
            candidates.push(format!("{stem}.{ext}"));
            candidates.push(join(&target, &format!("index.{ext}")));
        }
        if let Some(hit) = candidates.into_iter().find(|p| file(map, p).is_some()) {
            found.push(hit);
        }
    }
    found
}

/// The file at `path`, if the project has one: an import names a file,
/// never a folder of the same name.
fn file(map: &CodeMap, path: &str) -> Option<usize> {
    map.find(path)
        .filter(|&n| matches!(map.nodes[n].kind, NodeKind::File { .. }))
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
}
