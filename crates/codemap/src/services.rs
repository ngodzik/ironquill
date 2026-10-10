//! The services a project runs and who reaches whom: those its Compose
//! files declare, and the top folders that serve an API or MCP tools; the
//! routes and tools each offers; and the links between them, through an
//! OpenAPI spec, over MCP, or by a URL that names a service.
//!
//! Read from the files, never run: a service is where Compose builds it
//! from, a tool is a decorated function, a link is a call found or a URL
//! written, and each link keeps where it was read.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Component, Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;
use yaml_rust2::{Yaml, YamlLoader};

use crate::map::{CodeMap, Language, NodeKind};

/// The Compose files read at the project's root, the first found.
const COMPOSE: [&str; 4] = [
    "compose.yaml",
    "compose.yml",
    "docker-compose.yaml",
    "docker-compose.yml",
];

/// How a service reaches another.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Via {
    /// Through the operations of an OpenAPI spec.
    OpenApi,
    /// Over MCP.
    Mcp,
    /// By plain HTTP, to a URL that names it.
    Http,
}

/// What an MCP server offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolKind {
    /// A tool, for a model to call.
    Tool,
    /// A prompt.
    Prompt,
}

/// A tool or a prompt an MCP server offers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tool {
    /// Its name.
    pub name: String,
    /// A tool or a prompt.
    pub kind: ToolKind,
    /// The first line of its docstring.
    pub summary: String,
    /// The file that defines it, from the project's root.
    pub path: String,
    /// The line of its function, from 1.
    pub line: usize,
}

/// A service of the project, or one outside it that it reaches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Service {
    /// Its name.
    pub name: String,
    /// The folder it is built from, from the project's root; `None` for
    /// an image used as it is, or a service outside the project.
    pub folder: Option<String>,
    /// Whether it is outside the project.
    pub external: bool,
    /// The operations it serves, by their place in the map's.
    pub operations: Vec<usize>,
    /// The MCP tools and prompts it offers.
    pub tools: Vec<Tool>,
}

/// Where a link was read.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Evidence {
    /// The file, from the project's root.
    pub path: String,
    /// The line, from 1.
    pub line: usize,
    /// The line, trimmed.
    pub text: String,
}

/// One service reaching another.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceLink {
    /// The service that reaches, by its place in [`Services::services`].
    pub from: usize,
    /// The service reached.
    pub to: usize,
    /// How.
    pub via: Via,
    /// The operations called, by their place in the map's, through an
    /// OpenAPI spec.
    pub operations: Vec<usize>,
    /// Where the link was read, each place once.
    pub evidence: Vec<Evidence>,
}

/// The services of a project, and their links.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Services {
    /// The services, those Compose declares first, in its order.
    pub services: Vec<Service>,
    /// Who reaches whom, one link per pair and way.
    pub links: Vec<ServiceLink>,
    /// The service linked to the most others, to draw in the middle.
    pub centre: Option<usize>,
}

impl Services {
    /// The service a file belongs to: the deepest whose folder holds it;
    /// among those sharing a folder, the one named after it, else the
    /// first declared.
    #[must_use]
    pub fn owner(&self, path: &str) -> Option<usize> {
        let mut best: Option<(usize, usize, bool)> = None;
        for (i, service) in self.services.iter().enumerate() {
            let Some(folder) = &service.folder else {
                continue;
            };
            if !within(path, folder) {
                continue;
            }
            let depth = if folder.is_empty() { 0 } else { folder.len() };
            let named = folder.rsplit('/').next() == Some(service.name.as_str());
            let better = match best {
                None => true,
                Some((_, d, n)) => depth > d || (depth == d && named && !n),
            };
            if better {
                best = Some((i, depth, named));
            }
        }
        best.map(|(i, _, _)| i)
    }
}

/// Whether `path` is `folder` or inside it; the root holds everything.
fn within(path: &str, folder: &str) -> bool {
    folder.is_empty()
        || path == folder
        || path
            .strip_prefix(folder)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// A service as Compose declares it, before the map is looked at.
#[derive(Debug, Default)]
struct Declared {
    folder: Option<String>,
    /// Its environment's values, with the Compose file they were read in.
    environment: Vec<(String, String)>,
}

/// Reads the services of the project at `root`, whose map is `map` and
/// whose files read are `sources`, by node.
pub(crate) fn read(root: &Path, map: &CodeMap, sources: &[(usize, String)]) -> Services {
    let mut declared: Vec<(String, Declared)> = Vec::new();
    if let Some(first) = COMPOSE.iter().find(|name| root.join(name).is_file()) {
        let mut seen = HashSet::new();
        compose(root, Path::new(first), &mut declared, &mut seen, 0);
    }
    let mut services = Services {
        services: declared
            .iter()
            .map(|(name, d)| Service {
                name: name.clone(),
                folder: d.folder.clone(),
                external: false,
                operations: Vec::new(),
                tools: Vec::new(),
            })
            .collect(),
        ..Services::default()
    };
    let path_of = |node: usize| map.nodes[node].path.as_str();
    let tools = mcp_tools(map, sources);

    // A top folder that serves an API or MCP tools, and that Compose does
    // not cover, is a service too.
    let mut serving: Vec<String> = map
        .operations
        .iter()
        .map(|o| path_of(o.spec).to_owned())
        .chain(tools.iter().map(|t| t.path.clone()))
        .collect();
    serving.sort();
    for path in serving {
        let Some((top, _)) = path.split_once('/') else {
            continue;
        };
        if services.owner(&path).is_none() && !services.services.iter().any(|s| s.name == top) {
            services.services.push(Service {
                name: top.to_owned(),
                folder: Some(top.to_owned()),
                external: false,
                operations: Vec::new(),
                tools: Vec::new(),
            });
        }
    }

    // What each serves.
    for (i, operation) in map.operations.iter().enumerate() {
        if let Some(owner) = services.owner(path_of(operation.spec)) {
            services.services[owner].operations.push(i);
        }
    }
    for tool in tools {
        if let Some(owner) = services.owner(&tool.path) {
            services.services[owner].tools.push(tool);
        }
    }

    let mut links: BTreeMap<(usize, usize, Via), ServiceLink> = BTreeMap::new();
    let add = |links: &mut BTreeMap<(usize, usize, Via), ServiceLink>,
               from: usize,
               to: usize,
               via: Via,
               operation: Option<usize>,
               evidence: Option<Evidence>| {
        if from == to {
            return;
        }
        let link = links.entry((from, to, via)).or_insert_with(|| ServiceLink {
            from,
            to,
            via,
            operations: Vec::new(),
            evidence: Vec::new(),
        });
        if let Some(op) = operation
            && !link.operations.contains(&op)
        {
            link.operations.push(op);
        }
        if let Some(e) = evidence
            && !link.evidence.contains(&e)
        {
            link.evidence.push(e);
        }
    };

    // Through OpenAPI: each caller of an operation reaches the service of
    // its spec. A name several specs share is put down to the service the
    // caller reaches most through names that are not shared.
    let text_of: HashMap<usize, &str> = sources.iter().map(|(n, t)| (*n, t.as_str())).collect();
    let mut specs_of_name: HashMap<&str, HashSet<usize>> = HashMap::new();
    for operation in &map.operations {
        specs_of_name
            .entry(operation.id.as_str())
            .or_default()
            .insert(operation.spec);
    }
    let mut calls: Vec<(usize, usize, usize, bool)> = Vec::new();
    for (i, operation) in map.operations.iter().enumerate() {
        let shared = specs_of_name
            .get(operation.id.as_str())
            .is_some_and(|specs| specs.len() > 1);
        for &caller in &operation.callers {
            if let Some(from) = services.owner(path_of(caller)) {
                calls.push((caller, from, i, shared));
            }
        }
    }
    let mut favourite: HashMap<usize, usize> = HashMap::new();
    {
        let mut counts: HashMap<(usize, usize), usize> = HashMap::new();
        for &(caller, _, i, shared) in &calls {
            if !shared && let Some(to) = services.owner(path_of(map.operations[i].spec)) {
                *counts.entry((caller, to)).or_default() += 1;
            }
        }
        let mut best: HashMap<usize, (usize, usize)> = HashMap::new();
        for ((caller, to), n) in counts {
            let entry = best.entry(caller).or_insert((to, 0));
            if n > entry.1 || (n == entry.1 && to < entry.0) {
                *entry = (to, n);
            }
        }
        favourite.extend(best.into_iter().map(|(c, (to, _))| (c, to)));
    }
    for (caller, from, i, shared) in calls {
        let operation = &map.operations[i];
        let Some(to) = services.owner(path_of(operation.spec)) else {
            continue;
        };
        if shared && favourite.get(&caller) != Some(&to) {
            continue;
        }
        let evidence = text_of
            .get(&caller)
            .and_then(|text| where_named(path_of(caller), text, &operation.id));
        add(&mut links, from, to, Via::OpenApi, Some(i), evidence);
    }

    // By URLs that name a service: in Compose's environment, or in a
    // service's files.
    let names: HashMap<String, usize> = services
        .services
        .iter()
        .enumerate()
        .map(|(i, s)| (s.name.clone(), i))
        .collect();
    for (name, declaration) in &declared {
        let Some(&from) = names.get(name) else {
            continue;
        };
        for (file, value) in &declaration.environment {
            for (host, path) in urls(value) {
                if let Some(&to) = names.get(&host) {
                    let text = std::fs::read_to_string(root.join(file)).unwrap_or_default();
                    let evidence = where_written(file, &text, value);
                    add(&mut links, from, to, via_of(&path), None, evidence);
                }
            }
        }
    }
    for (node, text) in sources {
        let path = path_of(*node);
        let language = match map.nodes[*node].kind {
            NodeKind::File { language, .. } => language,
            NodeKind::Folder => continue,
        };
        if language == Language::Markdown || crate::map::is_test(Path::new(path)) {
            continue;
        }
        let Some(from) = services.owner(path) else {
            continue;
        };
        for (number, line) in text.lines().enumerate() {
            // A URL in a comment calls nothing.
            let code = line.trim_start();
            if !line.contains("://")
                || code.starts_with('#')
                || code.starts_with("//")
                || code.starts_with('*')
                || code.starts_with("/*")
            {
                continue;
            }
            for (host, url_path) in urls(line) {
                if let Some(&to) = names.get(&host) {
                    let evidence = Evidence {
                        path: path.to_owned(),
                        line: number + 1,
                        text: line.trim().to_owned(),
                    };
                    add(
                        &mut links,
                        from,
                        to,
                        via_of(&url_path),
                        None,
                        Some(evidence),
                    );
                }
            }
        }
    }

    // MCP servers outside the project, as a JSON file of `mcpServers` says.
    for (node, text) in sources {
        let path = path_of(*node);
        if !path.ends_with(".json") || !text.contains("mcpServers") {
            continue;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(text) else {
            continue;
        };
        let Some(servers) = value.get("mcpServers").and_then(|s| s.as_object()) else {
            continue;
        };
        for (name, server) in servers {
            let address = ["url", "serverUrl", "httpUrl"]
                .iter()
                .find_map(|k| server.get(*k).and_then(|v| v.as_str()));
            let Some(address) = address else {
                continue;
            };
            let host = address
                .split("://")
                .nth(1)
                .and_then(|rest| rest.split(['/', ':']).next())
                .unwrap_or("");
            if !host.contains('.') || host == "host.docker.internal" {
                continue;
            }
            let to = match services.services.iter().position(|s| s.name == *name) {
                Some(i) => i,
                None => {
                    services.services.push(Service {
                        name: name.clone(),
                        folder: None,
                        external: true,
                        operations: Vec::new(),
                        tools: Vec::new(),
                    });
                    services.services.len() - 1
                }
            };
            if let Some(from) = services.owner(path) {
                let evidence = where_written(path, text, address);
                add(&mut links, from, to, Via::Mcp, None, evidence);
            }
        }
    }

    services.links = links.into_values().collect();
    services.centre = centre(&services);
    services
}

/// The service linked to the most others, then serving the most.
fn centre(services: &Services) -> Option<usize> {
    let mut neighbours: Vec<HashSet<usize>> = vec![HashSet::new(); services.services.len()];
    for link in &services.links {
        neighbours[link.from].insert(link.to);
        neighbours[link.to].insert(link.from);
    }
    (0..services.services.len()).max_by(|&a, &b| {
        let serves =
            |i: usize| services.services[i].operations.len() + services.services[i].tools.len();
        neighbours[a]
            .len()
            .cmp(&neighbours[b].len())
            .then(serves(a).cmp(&serves(b)))
            // The first declared wins a tie: max_by keeps the last.
            .then(b.cmp(&a))
    })
}

/// How a URL's path reaches a service: MCP under `/mcp`, else HTTP.
fn via_of(path: &str) -> Via {
    if path.starts_with("/mcp") {
        Via::Mcp
    } else {
        Via::Http
    }
}

/// The hosts without a dot and the paths of the `http(s)://` URLs in
/// `text`: what can name a service.
fn urls(text: &str) -> Vec<(String, String)> {
    static URL: LazyLock<Option<Regex>> =
        LazyLock::new(|| Regex::new(r#"https?://([A-Za-z0-9_.-]+)(?::\d+)?(/[^\s"'<>`)]*)?"#).ok());
    URL.as_ref().map_or_else(Vec::new, |re| {
        re.captures_iter(text)
            .filter(|c| !c[1].contains('.'))
            .map(|c| {
                (
                    c[1].to_owned(),
                    c.get(2).map_or("/", |p| p.as_str()).to_owned(),
                )
            })
            .collect()
    })
}

/// Where `needle` is first written in `text` of the file `path`.
fn where_written(path: &str, text: &str, needle: &str) -> Option<Evidence> {
    text.lines().enumerate().find_map(|(i, line)| {
        line.contains(needle).then(|| Evidence {
            path: path.to_owned(),
            line: i + 1,
            text: line.trim().to_owned(),
        })
    })
}

/// Where a client in `text` first names the operation `id`, by any name a
/// client gives it.
fn where_named(path: &str, text: &str, id: &str) -> Option<Evidence> {
    let pascal = crate::api::pascal(id);
    let lower = crate::api::lower_first(&pascal);
    [id, pascal.as_str(), lower.as_str()]
        .iter()
        .find_map(|name| where_written(path, text, name))
}

/// Reads the Compose file `file` (from `root`) and those it includes into
/// `declared`, a service named again adding to the first.
fn compose(
    root: &Path,
    file: &Path,
    declared: &mut Vec<(String, Declared)>,
    seen: &mut HashSet<PathBuf>,
    depth: usize,
) {
    if depth > 8 || !seen.insert(file.to_owned()) {
        return;
    }
    // Read from disk: a base file is often hidden, and the map skips those.
    let Ok(text) = std::fs::read_to_string(root.join(file)) else {
        return;
    };
    let Ok(documents) = YamlLoader::load_from_str(&text) else {
        return;
    };
    let Some(doc) = documents.first() else {
        return;
    };
    let folder = file.parent().unwrap_or(Path::new(""));
    let file_text = file.to_string_lossy().replace('\\', "/");
    // What it includes first, as Compose merges them.
    let mut included: Vec<String> = Vec::new();
    let mut take = |entry: &Yaml| match entry {
        Yaml::String(s) => included.push(s.clone()),
        Yaml::Hash(_) => match &entry["path"] {
            Yaml::String(s) => included.push(s.clone()),
            Yaml::Array(list) => {
                included.extend(list.iter().filter_map(|p| p.as_str().map(str::to_owned)))
            }
            _ => {}
        },
        _ => {}
    };
    match &doc["include"] {
        Yaml::Array(list) => list.iter().for_each(&mut take),
        other => take(other),
    }
    for include in included {
        let path = normal(&folder.join(include));
        compose(root, Path::new(&path), declared, seen, depth + 1);
    }
    let Yaml::Hash(services) = &doc["services"] else {
        return;
    };
    for (name, service) in services {
        let Some(name) = name.as_str() else {
            continue;
        };
        let context = match &service["build"] {
            Yaml::String(s) => Some(s.clone()),
            Yaml::Hash(_) => Some(
                service["build"]["context"]
                    .as_str()
                    .unwrap_or(".")
                    .to_owned(),
            ),
            _ => None,
        };
        let mut environment = Vec::new();
        match &service["environment"] {
            Yaml::Hash(map) => {
                for value in map.values() {
                    if let Some(v) = value.as_str() {
                        environment.push((file_text.clone(), v.to_owned()));
                    }
                }
            }
            Yaml::Array(list) => {
                for item in list.iter().filter_map(Yaml::as_str) {
                    let value = item.split_once('=').map_or(item, |(_, v)| v);
                    environment.push((file_text.clone(), value.to_owned()));
                }
            }
            _ => {}
        }
        let at = match declared.iter().position(|(n, _)| n == name) {
            Some(at) => at,
            None => {
                declared.push((name.to_owned(), Declared::default()));
                declared.len() - 1
            }
        };
        let entry = &mut declared[at].1;
        if let Some(context) = context
            && entry.folder.is_none()
        {
            entry.folder = Some(normal(&folder.join(context)));
        }
        entry.environment.extend(environment);
    }
}

/// A path with its `.` and `..` resolved, in `/` form, from the root.
fn normal(path: &Path) -> String {
    let mut parts: Vec<String> = Vec::new();
    for part in path.components() {
        match part {
            Component::ParentDir => {
                parts.pop();
            }
            Component::Normal(p) => parts.push(p.to_string_lossy().into_owned()),
            _ => {}
        }
    }
    parts.join("/")
}

/// The MCP tools and prompts of the project's Python files, tests left
/// out: functions decorated by a server's `.tool` or `.prompt`, or by a
/// decorator of the file's own that registers them with `.tool(...)`.
fn mcp_tools(map: &CodeMap, sources: &[(usize, String)]) -> Vec<Tool> {
    static SERVER: LazyLock<Option<Regex>> =
        LazyLock::new(|| Regex::new(r"(?m)^(\w+)\s*(?::[^=\n]+)?=\s*(?:\w+\.)*FastMCP\(").ok());
    static DECORATOR: LazyLock<Option<Regex>> =
        LazyLock::new(|| Regex::new(r"^\s*@(\w+)(?:\.(tool|prompt))?\s*(\(.*)?$").ok());
    static DEF: LazyLock<Option<Regex>> =
        LazyLock::new(|| Regex::new(r"^(\s*)(?:async\s+)?def\s+(\w+)\s*\(").ok());
    static NAME: LazyLock<Option<Regex>> =
        LazyLock::new(|| Regex::new(r#"name\s*=\s*["']([^"']+)["']"#).ok());
    static FIRST: LazyLock<Option<Regex>> =
        LazyLock::new(|| Regex::new(r#"^\(\s*["']([^"']+)["']"#).ok());
    let (Some(server_re), Some(decorator_re), Some(def_re), Some(name_re), Some(first_re)) = (
        SERVER.as_ref(),
        DECORATOR.as_ref(),
        DEF.as_ref(),
        NAME.as_ref(),
        FIRST.as_ref(),
    ) else {
        return Vec::new();
    };
    let mut tools = Vec::new();
    for (node, text) in sources {
        let path = &map.nodes[*node].path;
        if !path.ends_with(".py") || crate::map::is_test(Path::new(path)) {
            continue;
        }
        let mut servers: HashSet<String> = server_re
            .captures_iter(text)
            .map(|c| c[1].to_owned())
            .collect();
        servers.insert("mcp".to_owned());
        if !servers
            .iter()
            .any(|s| text.contains(&format!("{s}.tool")) || text.contains(&format!("{s}.prompt")))
        {
            continue;
        }
        let lines: Vec<&str> = text.lines().collect();
        // The file's own decorators that register a tool: a function whose
        // body calls `<server>.tool(`.
        let mut registering: HashSet<String> = HashSet::new();
        for (i, line) in lines.iter().enumerate() {
            let Some(c) = def_re.captures(line) else {
                continue;
            };
            if !c[1].is_empty() {
                continue;
            }
            let body = lines[i + 1..]
                .iter()
                .take_while(|l| l.trim().is_empty() || l.starts_with([' ', '\t']));
            let registers = body
                .clone()
                .any(|l| servers.iter().any(|s| l.contains(&format!("{s}.tool("))));
            if registers {
                registering.insert(c[2].to_owned());
            }
        }
        let mut pending: Option<(ToolKind, String)> = None;
        for (i, line) in lines.iter().enumerate() {
            if let Some(c) = decorator_re.captures(line) {
                let target = &c[1];
                let args = c.get(3).map_or("", |a| a.as_str());
                let kind = match c.get(2).map(|m| m.as_str()) {
                    Some("tool") if servers.contains(target) => Some(ToolKind::Tool),
                    Some("prompt") if servers.contains(target) => Some(ToolKind::Prompt),
                    None if registering.contains(target) => Some(ToolKind::Tool),
                    _ => None,
                };
                if let Some(kind) = kind {
                    pending = Some((kind, args.to_owned()));
                }
                continue;
            }
            let Some((kind, args)) = pending.take() else {
                continue;
            };
            let Some(c) = def_re.captures(line) else {
                continue;
            };
            let function = c[2].to_owned();
            let name = name_re
                .captures(&args)
                .map(|n| n[1].to_owned())
                .or_else(|| {
                    (kind == ToolKind::Prompt)
                        .then(|| first_re.captures(&args).map(|n| n[1].to_owned()))
                        .flatten()
                })
                .unwrap_or(function);
            tools.push(Tool {
                name,
                kind,
                summary: docstring(&lines[i + 1..]),
                path: path.clone(),
                line: i + 1,
            });
        }
    }
    tools
}

/// The first line of the docstring that starts a function's body, if one
/// does: past the rest of its signature.
fn docstring(after: &[&str]) -> String {
    for line in after.iter().take(12) {
        let line = line.trim();
        for quote in ["\"\"\"", "'''", "\"", "'"] {
            if let Some(rest) = line.strip_prefix(quote) {
                let first = rest.split(quote).next().unwrap_or(rest).trim();
                return first.to_owned();
            }
        }
        // Still the signature, until its end.
        if line.ends_with(':')
            || line.is_empty()
            || line.ends_with(',')
            || line.ends_with('(')
            || line.starts_with(')')
        {
            continue;
        }
        return String::new();
    }
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(root: &Path, path: &str, text: &str) {
        let path = root.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn services_their_tools_and_who_reaches_whom() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        // A hidden base file, included; both forms of build.
        write(
            root,
            "compose.yaml",
            "include:\n  - .compose.base.yaml\nservices:\n  web:\n    build: ./web\n    environment:\n      API_URL: http://api:8000/v1\n",
        );
        write(
            root,
            ".compose.base.yaml",
            "services:\n  api:\n    build:\n      context: ./api\n  agent:\n    build: ./agent\n    environment:\n      - TOOLS_URL=http://tools:9000/mcp\n      - API_URL=http://api:8000\n      - DATABASE_URL=postgresql://db:5432/app\n  tools:\n    build: ./tools\n  db:\n    image: postgres\n",
        );
        write(
            root,
            "tools/server.py",
            "from mcp.server.fastmcp import FastMCP\n\nserver = FastMCP(\"tools\")\n\n@server.tool()\ndef search(query: str) -> str:\n    \"\"\"Search the catalogue.\n\n    More.\n    \"\"\"\n    return query\n\n@server.prompt(\"review\")\ndef review_prompt() -> str:\n    \"\"\"Ask for a review.\"\"\"\n    return \"\"\n\ndef optional_tool(func):\n    if ENABLED:\n        server.tool(name=func.__name__)(func)\n    return func\n\n@optional_tool\ndef export(path: str):\n    \"\"\"Export a report.\"\"\"\n",
        );
        write(root, "api/main.py", "app = FastAPI()\n");
        write(
            root,
            "agent/app.py",
            "MCP = \"http://tools:9000/mcp\"\n# formerly http://db:8080/\n",
        );
        write(root, "agent/.mcp.json", "");
        write(
            root,
            "agent/mcp.json",
            "{\"mcpServers\": {\"docs\": {\"url\": \"https://docs.example.com/mcp\"}, \"local\": {\"url\": \"http://host.docker.internal:7000/mcp\"}}}",
        );
        write(root, "web/README.md", "See http://api:8000/docs\n");
        let map = crate::map(root, 100);
        let services = &map.services;
        let names: Vec<&str> = services.services.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["api", "agent", "tools", "db", "web", "docs"]);
        let api = &services.services[0];
        assert_eq!(api.folder.as_deref(), Some("api"));
        assert_eq!(services.services[3].folder, None);
        let docs = &services.services[5];
        assert!(docs.external);

        let tools = &services.services[2].tools;
        let found: Vec<(&str, ToolKind, &str)> = tools
            .iter()
            .map(|t| (t.name.as_str(), t.kind, t.summary.as_str()))
            .collect();
        assert_eq!(
            found,
            [
                ("search", ToolKind::Tool, "Search the catalogue."),
                ("review", ToolKind::Prompt, "Ask for a review."),
                ("export", ToolKind::Tool, "Export a report."),
            ]
        );
        assert_eq!(tools[0].line, 6);

        let index = |name: &str| names.iter().position(|n| *n == name).unwrap();
        let links: Vec<(&str, &str, Via)> = services
            .links
            .iter()
            .map(|l| (names[l.from], names[l.to], l.via))
            .collect();
        // HTTP and MCP between the same two services; nothing to the
        // database, whose URL is not HTTP; nothing from a README.
        assert!(links.contains(&("agent", "api", Via::Http)));
        assert!(links.contains(&("agent", "tools", Via::Mcp)));
        assert!(links.contains(&("web", "api", Via::Http)));
        assert!(links.contains(&("agent", "docs", Via::Mcp)));
        assert!(!links.iter().any(|(_, to, _)| *to == "db"));
        assert!(!links.iter().any(|(_, to, _)| *to == "local"));
        let agent_tools = services
            .links
            .iter()
            .find(|l| l.from == index("agent") && l.to == index("tools"))
            .unwrap();
        // Read in Compose and in the code, each place once.
        assert_eq!(agent_tools.evidence.len(), 2);
        assert!(
            agent_tools
                .evidence
                .iter()
                .any(|e| e.path == "agent/app.py" && e.line == 1)
        );
        // The agent reaches the most.
        assert_eq!(services.centre, Some(index("agent")));
        assert_eq!(services.owner("agent/app.py"), Some(index("agent")));
    }

    #[test]
    fn a_shared_folder_goes_to_the_service_named_after_it() {
        let services = Services {
            services: vec![
                Service {
                    name: "migrate".into(),
                    folder: Some("backend".into()),
                    external: false,
                    operations: Vec::new(),
                    tools: Vec::new(),
                },
                Service {
                    name: "backend".into(),
                    folder: Some("backend".into()),
                    external: false,
                    operations: Vec::new(),
                    tools: Vec::new(),
                },
            ],
            ..Services::default()
        };
        assert_eq!(services.owner("backend/app/main.py"), Some(1));
        assert_eq!(services.owner("frontend/x.ts"), None);
        assert_eq!(via_of("/mcp/sse"), Via::Mcp);
        assert_eq!(
            urls("see https://name.app/x and http://api:80/y"),
            [("api".to_owned(), "/y".to_owned())]
        );
    }
}
