//! What a front end calls of a back end through an OpenAPI spec: each
//! operation of the spec, the back end's function that serves it, and the
//! front end's files that call it, by the names a generated client gives it.
//!
//! All by reading the files, no model: an operation is named by its
//! `operationId`; the function that serves it is the one of that name under
//! a route decorator (`@router.get(...)`), the nearest to the spec when
//! several are; a call is an identifier that ends with the operation's name
//! in the forms clients are generated with (`getDag`, `useDagServiceGetDag`,
//! `UseDagServiceGetDagKeyFn`). Files of the generated client itself are
//! not callers: they name every operation.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::LazyLock;

use regex::Regex;

use crate::map::{CodeMap, Language, NodeKind, worth};

static OPERATION_ID: LazyLock<Option<Regex>> = LazyLock::new(|| {
    Regex::new(r#"(?m)(?:^\s*operationId:\s*['"]?|"operationId"\s*:\s*")([A-Za-z_][\w-]*)"#).ok()
});
static ROUTE: LazyLock<Option<Regex>> = LazyLock::new(|| {
    Regex::new(
        r"(?m)^\s*@[\w.]+\.(?:get|post|put|patch|delete|head|options|route|api_route|websocket)\(",
    )
    .ok()
});
static DEF: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"(?m)^\s*(?:async\s+)?def\s+(\w+)\s*\(").ok());
static IDENTIFIER: LazyLock<Option<Regex>> = LazyLock::new(|| Regex::new(r"[A-Za-z_$][\w$]*").ok());

/// What generated clients add after an operation's name, in the names of
/// what they generate around it: React Query's keys and loaders.
const CLIENT_SUFFIXES: [&str; 5] = ["KeyFn", "Key", "Data", "Fn", "Query"];

/// The calls through the API: for each file of a front end that calls an
/// operation, the file of the back end that serves it, each pair once.
pub(crate) fn calls(map: &CodeMap, texts: &[(usize, String)]) -> Vec<(usize, usize)> {
    let (Some(operation_id), Some(route), Some(def), Some(identifier)) = (
        OPERATION_ID.as_ref(),
        ROUTE.as_ref(),
        DEF.as_ref(),
        IDENTIFIER.as_ref(),
    ) else {
        return Vec::new();
    };
    let language = |node: usize| match map.nodes[node].kind {
        NodeKind::File { language, .. } => Some(language),
        NodeKind::Folder => None,
    };

    // The operations, each with the spec it is from.
    let mut operations: Vec<(String, usize)> = Vec::new();
    for (node, text) in texts {
        if language(*node) == Some(Language::Config) && is_spec(text) {
            for c in operation_id.captures_iter(text) {
                operations.push((c[1].to_owned(), *node));
            }
        }
    }
    if operations.is_empty() {
        return Vec::new();
    }
    let wanted: HashSet<&str> = operations.iter().map(|(id, _)| id.as_str()).collect();

    // The functions under a route decorator, by name.
    let mut handlers: HashMap<&str, Vec<usize>> = HashMap::new();
    for (node, text) in texts {
        if language(*node) != Some(Language::Python) || !designed(map, *node) {
            continue;
        }
        for decorator in route.find_iter(text) {
            if let Some(name) = def
                .captures(&text[decorator.end()..])
                .and_then(|c| c.get(1))
                .map(|m| m.as_str())
                .filter(|name| wanted.contains(name))
            {
                handlers.entry(name).or_default().push(*node);
            }
        }
    }

    // Each operation's handler, and the names a client calls it by.
    let mut by_name: HashMap<String, Vec<usize>> = HashMap::new();
    for (id, spec) in &operations {
        let Some(candidates) = handlers.get(id.as_str()) else {
            continue;
        };
        let spec_path = &map.nodes[*spec].path;
        let Some(&handler) = candidates
            .iter()
            .max_by_key(|&&c| (shared_prefix(spec_path, &map.nodes[c].path), usize::MAX - c))
        else {
            continue;
        };
        let pascal = pascal(id);
        let camel = lower_first(&pascal);
        for name in [pascal, camel] {
            let list = by_name.entry(name).or_default();
            if !list.contains(&handler) {
                list.push(handler);
            }
        }
    }

    // The front end's files that call them.
    let mut found = HashSet::new();
    for (node, text) in texts {
        let is_script = matches!(
            language(*node),
            Some(Language::TypeScript | Language::JavaScript)
        );
        if !is_script || !designed(map, *node) || generated(&map.nodes[*node].path) {
            continue;
        }
        for word in identifier.find_iter(text) {
            for name in called(word.as_str()) {
                for &handler in by_name.get(name).into_iter().flatten() {
                    found.insert((*node, handler));
                }
            }
        }
    }
    let mut found: Vec<(usize, usize)> = found.into_iter().collect();
    found.sort_unstable();
    found
}

/// Whether a settings file is an OpenAPI (or Swagger) spec: said near its
/// start.
fn is_spec(text: &str) -> bool {
    let start: String = text.chars().take(400).collect();
    start.lines().any(|line| {
        let line = line.trim_start_matches(['{', ' ', '\t']);
        line.starts_with("openapi:")
            || line.starts_with("swagger:")
            || line.starts_with("\"openapi\"")
            || line.starts_with("\"swagger\"")
    })
}

/// Code of the design: not a test, an example or a document.
fn designed(map: &CodeMap, node: usize) -> bool {
    worth(Path::new(&map.nodes[node].path)) == 0
}

/// Whether a file is generated from a spec, as clients are: it names every
/// operation, and calls none.
fn generated(path: &str) -> bool {
    path.contains("openapi-gen/") || path.contains("/generated/") || path.contains(".gen.")
}

/// The names an identifier may call an operation by: itself, and each of
/// its ends that starts with a capital (`useDagServiceGetDag` ends with
/// `GetDag`), with or without what clients add after (`GetDagKeyFn`).
fn called(word: &str) -> Vec<&str> {
    let mut names = vec![word];
    for (i, c) in word.char_indices().skip(1) {
        if c.is_ascii_uppercase() {
            names.push(&word[i..]);
        }
    }
    let mut trimmed = Vec::new();
    for name in &names {
        for suffix in CLIENT_SUFFIXES {
            if let Some(stem) = name.strip_suffix(suffix).filter(|s| !s.is_empty()) {
                trimmed.push(stem);
            }
        }
    }
    names.extend(trimmed);
    names
}

/// `get_dag_runs` as `GetDagRuns`; `getDag` as `GetDag`.
fn pascal(id: &str) -> String {
    id.split(['_', '-'])
        .filter(|p| !p.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            chars.next().map_or(String::new(), |first| {
                first.to_uppercase().chain(chars).collect()
            })
        })
        .collect()
}

fn lower_first(word: &str) -> String {
    let mut chars = word.chars();
    chars.next().map_or(String::new(), |first| {
        first.to_lowercase().chain(chars).collect()
    })
}

/// How many leading path parts two paths share.
fn shared_prefix(a: &str, b: &str) -> usize {
    a.split('/')
        .zip(b.split('/'))
        .take_while(|(x, y)| x == y)
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::map::{EdgeKind, map};

    fn write(root: &Path, path: &str, text: &str) {
        let path = root.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn a_page_calls_the_route_that_serves_the_operation_it_names() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(
            root,
            "api/openapi/v2.yaml",
            "openapi: 3.1.0\npaths:\n  /dags/{id}:\n    get:\n      operationId: get_dag\n  /dags:\n    get:\n      operationId: get_dags\n",
        );
        write(
            root,
            "api/routes/dags.py",
            "@router.get(\n    \"/{id}\",\n    responses={},\n)\ndef get_dag(id):\n    pass\n\n@router.get(\"\")\nasync def get_dags():\n    pass\n",
        );
        // The same name elsewhere, farther from the spec, unrouted or not.
        write(
            root,
            "worker/routes.py",
            "@app.get('/x')\ndef get_dag():\n    pass\n",
        );
        write(
            root,
            "ui/src/Dag.tsx",
            "const { data } = useDagServiceGetDag({ id });\nconst key = UseDagServiceGetDagsKeyFn();\n",
        );
        write(root, "ui/src/Other.tsx", "const forgetDag = 1;\n");
        write(
            root,
            "ui/openapi-gen/queries.ts",
            "export const useDagServiceGetDag = () => DagService.getDag();\n",
        );
        let map = map(root, 100);
        let calls: Vec<(&str, &str)> = map
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Calls)
            .map(|e| {
                (
                    map.nodes[e.from].path.as_str(),
                    map.nodes[e.to].path.as_str(),
                )
            })
            .collect();
        assert_eq!(calls, [("ui/src/Dag.tsx", "api/routes/dags.py")]);
    }

    #[test]
    fn names_a_client_calls_an_operation_by() {
        assert_eq!(pascal("get_dag_runs"), "GetDagRuns");
        assert_eq!(pascal("listBackfills"), "ListBackfills");
        assert_eq!(lower_first("GetDag"), "getDag");
        let names = called("UseDagServiceGetDagKeyFn");
        assert!(names.contains(&"GetDag"));
        assert!(!called("forgetDag").contains(&"GetDag"));
        assert!(is_spec("{\n  \"openapi\": \"3.0.0\""));
        assert!(!is_spec("name: provider\n"));
    }
}
