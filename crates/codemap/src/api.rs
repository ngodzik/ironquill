//! What a front end calls of a back end through an OpenAPI spec: each
//! operation of the spec, the back end's function that serves it, and the
//! front end's files that call it, by the names a generated client gives it.
//!
//! All by reading the files, no model: the spec is read as YAML (JSON being
//! YAML too), and an operation is named by its `operationId`; the function that serves it is the one of that name under
//! a route decorator (`@router.get(...)`), the nearest to the spec when
//! several are; a call is an identifier that ends with the operation's name
//! in the forms clients are generated with (`getDag`, `useDagServiceGetDag`,
//! `UseDagServiceGetDagKeyFn`). Files of the generated client itself are
//! not callers: they name every operation.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::LazyLock;

use regex::Regex;
use yaml_rust2::{Yaml, YamlLoader};

use crate::map::{CodeMap, Language, NodeKind, worth};

static ROUTE: LazyLock<Option<Regex>> = LazyLock::new(|| {
    Regex::new(
        r"(?m)^\s*@[\w.]+\.(?:get|post|put|patch|delete|head|options|route|api_route|websocket)\(",
    )
    .ok()
});
static OPERATION_ID_ARG: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r#"\boperation_id\s*=\s*["']([^"']+)["']"#).ok());
static DEF: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"(?m)^\s*(?:async\s+)?def\s+(\w+)\s*\(").ok());
static IDENTIFIER: LazyLock<Option<Regex>> = LazyLock::new(|| Regex::new(r"[A-Za-z_$][\w$]*").ok());

/// What generated clients add after an operation's name, in the names of
/// what they generate around it: React Query's keys and loaders.
const CLIENT_SUFFIXES: [&str; 5] = ["KeyFn", "Key", "Data", "Fn", "Query"];

/// An operation of an API's spec, with the function that serves it and the
/// files that call it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Operation {
    /// Its `operationId`, or its method and path when it has none.
    pub id: String,
    /// `GET`, `POST`…
    pub method: String,
    /// Its path, parameters in braces: `/api/v2/dags/{dag_id}`.
    pub path: String,
    /// What it does, in a line, as the spec says.
    pub summary: String,
    /// The spec's tags for it: the resource it is about, mostly.
    pub tags: Vec<String>,
    /// Whether the spec says it is deprecated.
    pub deprecated: bool,
    /// The spec it is from, as a node of the map.
    pub spec: usize,
    /// The file and the line, from 1, of the function that serves it.
    pub handler: Option<(usize, usize)>,
    /// The front end's files that call it.
    pub callers: Vec<usize>,
}

/// The methods an OpenAPI path item may hold.
const METHODS: [&str; 8] = [
    "get", "post", "put", "patch", "delete", "head", "options", "trace",
];

/// The operations of every OpenAPI spec of the project, each with its
/// handler and callers, and the calls through them: for each file that
/// calls an operation, the file that serves it, each pair once.
pub(crate) fn read(
    map: &CodeMap,
    texts: &[(usize, String)],
) -> (Vec<Operation>, Vec<(usize, usize)>) {
    let (Some(route), Some(def), Some(identifier)) =
        (ROUTE.as_ref(), DEF.as_ref(), IDENTIFIER.as_ref())
    else {
        return (Vec::new(), Vec::new());
    };
    let language = |node: usize| match map.nodes[node].kind {
        NodeKind::File { language, .. } => Some(language),
        NodeKind::Folder => None,
    };

    let mut operations: Vec<Operation> = Vec::new();
    for (node, text) in texts {
        if language(*node) == Some(Language::Config) && is_spec(text) {
            operations.extend(operations_of(*node, text));
        }
    }
    if operations.is_empty() {
        return (Vec::new(), Vec::new());
    }
    let wanted: HashSet<&str> = operations.iter().map(|o| o.id.as_str()).collect();

    // The functions under a route decorator, by name, with their line.
    let mut handlers: HashMap<String, Vec<(usize, usize, bool)>> = HashMap::new();
    for (node, text) in texts {
        if language(*node) != Some(Language::Python) || !designed(map, *node) {
            continue;
        }
        for decorator in route.find_iter(text) {
            let after = &text[decorator.end()..];
            let Some(name) = def.captures(after).and_then(|c| c.get(1)) else {
                continue;
            };
            // An id given to the decorator (`operation_id="get_dags_ui"`)
            // rather than taken from the function's name.
            let given = OPERATION_ID_ARG
                .as_ref()
                .and_then(|re| re.captures(&after[..name.start()]))
                .and_then(|c| c.get(1))
                .map(|m| m.as_str());
            let id = given.unwrap_or(name.as_str());
            if !wanted.contains(id) {
                continue;
            }
            let at = decorator.end() + name.start();
            let line = text[..at].matches('\n').count() + 1;
            handlers
                .entry(id.to_owned())
                .or_default()
                .push((*node, line, given.is_some()));
        }
    }

    // Each operation's handler: the one given its id, else the nearest to
    // its spec, when several are.
    let mut by_name: HashMap<String, Vec<usize>> = HashMap::new();
    for (index, operation) in operations.iter_mut().enumerate() {
        let spec_path = &map.nodes[operation.spec].path;
        operation.handler = handlers.get(&operation.id).and_then(|candidates| {
            candidates
                .iter()
                .copied()
                .max_by_key(|&(c, _, given)| {
                    (
                        given,
                        shared_prefix(spec_path, &map.nodes[c].path),
                        usize::MAX - c,
                    )
                })
                .map(|(c, line, _)| (c, line))
        });
        let pascal = pascal(&operation.id);
        let camel = lower_first(&pascal);
        for name in [pascal, camel] {
            let list = by_name.entry(name).or_default();
            if !list.contains(&index) {
                list.push(index);
            }
        }
    }

    // The front end's files that call them.
    for (node, text) in texts {
        let is_script = matches!(
            language(*node),
            Some(Language::TypeScript | Language::JavaScript)
        );
        if !is_script || !designed(map, *node) || generated(&map.nodes[*node].path) {
            continue;
        }
        let mut called_here = HashSet::new();
        for word in identifier.find_iter(text) {
            let after_service = text[..word.start()].ends_with("Service.");
            let word = word.as_str();
            for name in called(word) {
                // A name of one word (`state`, `login`) is everywhere
                // (`useState`): it counts only in the forms a client is
                // generated with, `useEdgeServiceState`, `EdgeService.state`.
                if one_word(name) {
                    let before = word.rfind(name).map_or(word, |at| &word[..at]);
                    let generated_form =
                        before.ends_with("Service") || (before.is_empty() && after_service);
                    if !generated_form {
                        continue;
                    }
                }
                called_here.extend(by_name.get(name).into_iter().flatten().copied());
            }
        }
        for index in called_here {
            operations[index].callers.push(*node);
        }
    }

    let mut calls = HashSet::new();
    for operation in &mut operations {
        operation.callers.sort_unstable();
        operation.callers.dedup();
        if let Some((handler, _)) = operation.handler {
            calls.extend(operation.callers.iter().map(|&c| (c, handler)));
        }
    }
    let mut calls: Vec<(usize, usize)> = calls.into_iter().collect();
    calls.sort_unstable();
    (operations, calls)
}

/// The operations a spec declares, in its order.
fn operations_of(spec: usize, text: &str) -> Vec<Operation> {
    let Ok(documents) = YamlLoader::load_from_str(text) else {
        return Vec::new();
    };
    let Some(Yaml::Hash(paths)) = documents.first().map(|d| &d["paths"]) else {
        return Vec::new();
    };
    let mut operations = Vec::new();
    for (path, item) in paths {
        let Some(path) = path.as_str() else {
            continue;
        };
        for method in METHODS {
            let operation = &item[method];
            if operation.is_badvalue() {
                continue;
            }
            let text = |key: &str| {
                operation[key]
                    .as_str()
                    .unwrap_or_default()
                    .trim()
                    .to_owned()
            };
            let id = match text("operationId") {
                id if id.is_empty() => format!("{} {path}", method.to_uppercase()),
                id => id,
            };
            operations.push(Operation {
                id,
                method: method.to_uppercase(),
                path: path.to_owned(),
                summary: text("summary"),
                tags: operation["tags"]
                    .as_vec()
                    .into_iter()
                    .flatten()
                    .filter_map(|t| t.as_str().map(str::to_owned))
                    .collect(),
                deprecated: operation["deprecated"].as_bool().unwrap_or(false),
                spec,
                handler: None,
                callers: Vec::new(),
            });
        }
    }
    operations
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

/// Whether a name is one word: `State`, `login`, not `GetDag`.
fn one_word(name: &str) -> bool {
    name.chars().skip(1).all(|c| !c.is_ascii_uppercase())
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
            "openapi: 3.1.0\npaths:\n  /dags/{id}:\n    get:\n      tags:\n      - DAG\n      summary: Get Dag\n      operationId: get_dag\n  /dags:\n    get:\n      operationId: get_dags\n  /jobs/{state}:\n    patch:\n      operationId: state\n",
        );
        write(
            root,
            "api/routes/dags.py",
            "@router.get(\n    \"/{id}\",\n    responses={},\n)\ndef get_dag(id):\n    pass\n\n@router.get(\"\")\nasync def get_dags():\n    pass\n\n@router.patch(\"/jobs\")\ndef state():\n    pass\n",
        );
        // An id given to the decorator rather than the function's name.
        write(
            root,
            "api/routes/ui.py",
            "@ui_router.get(\"/ui/dags\", operation_id=\"get_dags\")\ndef dags_for_the_ui():\n    pass\n",
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
        write(
            root,
            "ui/src/Other.tsx",
            "const forgetDag = 1;\nconst [s, setState] = useState(0);\n",
        );
        write(root, "ui/src/Jobs.tsx", "EdgeService.state(job);\n");
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
        assert_eq!(
            calls,
            [
                ("api/routes/dags.py", "ui/src/Dag.tsx"),
                ("api/routes/ui.py", "ui/src/Dag.tsx"),
                ("api/routes/dags.py", "ui/src/Jobs.tsx"),
            ]
            .map(|(to, from)| (from, to))
        );
        // A one-word operation: called by the client's form only.
        let state = map.operations.iter().find(|o| o.id == "state").unwrap();
        let callers: Vec<&str> = state
            .callers
            .iter()
            .map(|&c| map.nodes[c].path.as_str())
            .collect();
        assert_eq!(callers, ["ui/src/Jobs.tsx"]);

        // Each operation, with what serves and what calls it.
        let get_dag = map.operations.iter().find(|o| o.id == "get_dag").unwrap();
        assert_eq!(
            (get_dag.method.as_str(), get_dag.path.as_str()),
            ("GET", "/dags/{id}")
        );
        assert_eq!(get_dag.summary, "Get Dag");
        assert_eq!(get_dag.tags, ["DAG"]);
        let (handler, line) = get_dag.handler.unwrap();
        assert_eq!(
            (map.nodes[handler].path.as_str(), line),
            ("api/routes/dags.py", 5)
        );
        let callers: Vec<&str> = get_dag
            .callers
            .iter()
            .map(|&c| map.nodes[c].path.as_str())
            .collect();
        assert_eq!(callers, ["ui/src/Dag.tsx"]);
        // Called by its key's name too; served by the function given its id.
        let get_dags = map.operations.iter().find(|o| o.id == "get_dags").unwrap();
        let handlers: Vec<&str> = get_dags
            .handler
            .iter()
            .map(|(n, _)| map.nodes[*n].path.as_str())
            .collect();
        assert_eq!(handlers, ["api/routes/ui.py"]);
        assert_eq!(get_dags.callers, get_dag.callers);
    }

    #[test]
    fn a_json_spec_and_an_operation_with_no_id() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "spec.json",
            r#"{"openapi": "3.0.0", "paths": {"/ping": {"post": {"summary": "Ping", "deprecated": true}}}}"#,
        );
        let map = map(dir.path(), 100);
        assert_eq!(map.operations.len(), 1);
        assert_eq!(map.operations[0].id, "POST /ping");
        assert!(map.operations[0].deprecated);
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
