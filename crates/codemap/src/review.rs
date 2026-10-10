//! What a branch changed, read as a reviewer reads it: the API's routes
//! added, removed or changed, as the OpenAPI specs say before and after;
//! what the database migrations do; what the models' tables gained or lost;
//! and every changed file in its area, from the API to the tests.
//!
//! Everything is read from the texts by patterns, never guessed: a
//! migration whose step is not one Alembic names is shown as it is written,
//! and a model the patterns do not see is left out rather than misread.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::LazyLock;

use regex::Regex;

use crate::api::{Operation, Parameter};
use crate::map::Language;

/// How something differs, before and after.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Delta {
    /// It is new.
    Added,
    /// It is gone.
    Removed,
    /// It is in both, and differs.
    Changed,
}

/// A route of the API that differs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteChange {
    /// Its method: `GET`.
    pub method: String,
    /// Its path: `/assets`.
    pub path: String,
    /// How it differs.
    pub delta: Delta,
    /// What differs, one line each: `query parameter uri (string[]) added`.
    pub details: Vec<String>,
}

/// A change to the database: a migration's step, or a model's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaChange {
    /// The table it is about, when one is named.
    pub table: Option<String>,
    /// How.
    pub delta: Delta,
    /// What, in a few words: `index idx_asset_uri on (uri)`.
    pub what: String,
}

/// An Alembic migration, as its text says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Migration {
    /// What it is for: the first line of its docstring.
    pub title: String,
    /// Its revision, and the one it follows.
    pub revision: Option<String>,
    /// The revision it follows.
    pub down_revision: Option<String>,
    /// What its upgrade does, in order.
    pub steps: Vec<SchemaChange>,
}

/// Where a changed file belongs, for a reviewer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Area {
    /// The API's specs, and the code that serves its routes.
    Api,
    /// Migrations, and the models that define the tables.
    Database,
    /// The rest of the code that runs on the server.
    Backend,
    /// The code that runs in the browser.
    FrontEnd,
    /// Code generated from a spec: a client, its types.
    Generated,
    /// Tests.
    Tests,
    /// Documents.
    Documents,
    /// Settings, manifests, the rest.
    Other,
}

impl Area {
    /// Its name, for a view.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Api => "API",
            Self::Database => "Database",
            Self::Backend => "Back end",
            Self::FrontEnd => "Front end",
            Self::Generated => "Generated",
            Self::Tests => "Tests",
            Self::Documents => "Documents",
            Self::Other => "Other",
        }
    }
}

/// A file a branch changed, with its text before and after: `None` when it
/// was not there, or is gone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Revised {
    /// Where it is, from the project's root.
    pub path: String,
    /// Its text in the base.
    pub before: Option<String>,
    /// Its text now.
    pub after: Option<String>,
}

/// A branch's changes, read.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Review {
    /// The routes that differ, by path then method.
    pub routes: Vec<RouteChange>,
    /// The migrations added or changed, by path.
    pub migrations: Vec<(String, Migration)>,
    /// What the models' tables gained or lost, by the file that defines
    /// them.
    pub models: Vec<(String, Vec<SchemaChange>)>,
    /// Every changed file in its area, the areas in their order.
    pub areas: Vec<(Area, Vec<String>)>,
}

/// Reads what `files` changed.
///
/// # Examples
///
/// ```
/// use ironquill_codemap::{Area, Delta, Revised, review};
///
/// let before = "openapi: 3.0.0\npaths:\n  /assets:\n    get:\n      summary: List\n";
/// let after = "openapi: 3.0.0\npaths:\n  /assets:\n    get:\n      summary: List\n      \
///              parameters:\n        - name: uri\n          in: query\n          \
///              schema: {type: string}\n";
/// let read = review(&[Revised {
///     path: "api/openapi.yaml".into(),
///     before: Some(before.into()),
///     after: Some(after.into()),
/// }]);
/// assert_eq!(read.routes.len(), 1);
/// assert_eq!(read.routes[0].delta, Delta::Changed);
/// assert_eq!(read.routes[0].details, ["query parameter uri (string) added"]);
/// assert_eq!(read.areas[0].0, Area::Api);
/// ```
#[must_use]
pub fn review(files: &[Revised]) -> Review {
    let mut read = Review::default();
    let mut areas: BTreeMap<Area, Vec<String>> = BTreeMap::new();
    for file in files {
        let before = file.before.as_deref().unwrap_or("");
        let after = file.after.as_deref().unwrap_or("");
        let spec = crate::api::is_spec(before) || crate::api::is_spec(after);
        if spec {
            read.routes.extend(route_changes(
                &crate::api::spec_operations(before),
                &crate::api::spec_operations(after),
            ));
        }
        let migration = migration(after).or_else(|| migration(before));
        let model = if migration.is_none() && file.path.ends_with(".py") {
            let changes = model_changes(before, after);
            let defines = !tables(before).is_empty() || !tables(after).is_empty();
            defines.then_some(changes)
        } else {
            None
        };
        // A test of the API is a test first; code under a folder named for
        // the API serves it, as does a router.
        let area = if spec {
            Area::Api
        } else if crate::map::is_test(Path::new(&file.path)) {
            Area::Tests
        } else if migration.is_some() || model.is_some() {
            Area::Database
        } else if serves_routes(after) || serves_routes(before) || in_api(&file.path) {
            Area::Api
        } else {
            area_of(&file.path)
        };
        if let Some(migration) = migration {
            read.migrations.push((file.path.clone(), migration));
        }
        if let Some(changes) = model.filter(|c| !c.is_empty()) {
            read.models.push((file.path.clone(), changes));
        }
        areas.entry(area).or_default().push(file.path.clone());
    }
    read.routes
        .sort_by(|a, b| a.path.cmp(&b.path).then(a.method.cmp(&b.method)));
    read.migrations.sort_by(|a, b| a.0.cmp(&b.0));
    read.models.sort_by(|a, b| a.0.cmp(&b.0));
    read.areas = areas.into_iter().collect();
    read
}

/// Where a file belongs by its path alone.
fn area_of(path: &str) -> Area {
    let file = Path::new(path);
    let name = file
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    if path.contains("openapi-gen/") || path.contains("/generated/") || name.contains(".gen.") {
        return Area::Generated;
    }
    if crate::map::is_test(file) {
        return Area::Tests;
    }
    match Language::of(file) {
        Language::TypeScript | Language::JavaScript => Area::FrontEnd,
        Language::Python | Language::Rust => Area::Backend,
        Language::Markdown => Area::Documents,
        _ if name.ends_with(".rst") || name.ends_with(".txt") => Area::Documents,
        _ => Area::Other,
    }
}

/// Whether a file of the back end sits under a folder named for the API:
/// `api`, `api_fastapi`, `rest_api`.
fn in_api(path: &str) -> bool {
    let back_end = matches!(
        Language::of(Path::new(path)),
        Language::Python | Language::Rust
    );
    back_end
        && path.split('/').rev().skip(1).any(|folder| {
            folder
                .split(['_', '-'])
                .any(|word| word == "api" || word == "apis")
        })
}

/// Whether a file serves routes: a FastAPI, Flask or Starlette router's
/// decorators.
fn serves_routes(text: &str) -> bool {
    static ROUTE: LazyLock<Option<Regex>> = LazyLock::new(|| {
        Regex::new(r"(?m)^\s*@\w+\.(get|post|put|patch|delete|route|api_route)\(").ok()
    });
    ROUTE.as_ref().is_some_and(|r| r.is_match(text))
}

/// The routes that differ between two versions of a spec's operations.
#[must_use]
pub fn route_changes(before: &[Operation], after: &[Operation]) -> Vec<RouteChange> {
    let key = |o: &Operation| (o.path.clone(), o.method.clone());
    let old: BTreeMap<(String, String), &Operation> = before.iter().map(|o| (key(o), o)).collect();
    let new: BTreeMap<(String, String), &Operation> = after.iter().map(|o| (key(o), o)).collect();
    let mut changes = Vec::new();
    for ((path, method), operation) in &new {
        let delta = match old.get(&(path.clone(), method.clone())) {
            None => Some((Delta::Added, describe(operation))),
            Some(was) => {
                let details = differences(was, operation);
                (!details.is_empty()).then_some((Delta::Changed, details))
            }
        };
        if let Some((delta, details)) = delta {
            changes.push(RouteChange {
                method: method.clone(),
                path: path.clone(),
                delta,
                details,
            });
        }
    }
    for ((path, method), operation) in &old {
        if !new.contains_key(&(path.clone(), method.clone())) {
            changes.push(RouteChange {
                method: method.clone(),
                path: path.clone(),
                delta: Delta::Removed,
                details: describe(operation),
            });
        }
    }
    changes
}

/// A new or removed route, in a line or two.
fn describe(operation: &Operation) -> Vec<String> {
    let mut lines = Vec::new();
    if !operation.summary.is_empty() {
        lines.push(operation.summary.clone());
    }
    let names: Vec<String> = operation.parameters.iter().map(parameter_name).collect();
    if !names.is_empty() {
        lines.push(format!("takes {}", names.join(", ")));
    }
    lines
}

fn parameter_name(p: &Parameter) -> String {
    format!("{} {} ({})", p.location, p.name, p.kind)
}

/// What differs between two versions of an operation, one line each.
fn differences(was: &Operation, now: &Operation) -> Vec<String> {
    let mut lines = Vec::new();
    if was.summary != now.summary {
        lines.push(format!("summary now: {}", now.summary));
    }
    if was.deprecated != now.deprecated {
        lines.push(
            if now.deprecated {
                "deprecated"
            } else {
                "no longer deprecated"
            }
            .to_owned(),
        );
    }
    if was.secured != now.secured {
        lines.push(
            if now.secured {
                "now needs credentials"
            } else {
                "no longer needs credentials"
            }
            .to_owned(),
        );
    }
    let find = |list: &[Parameter], p: &Parameter| {
        list.iter()
            .find(|q| q.name == p.name && q.location == p.location)
            .cloned()
    };
    for p in &now.parameters {
        match find(&was.parameters, p) {
            None => lines.push(format!(
                "{} parameter {} ({}){} added",
                p.location,
                p.name,
                p.kind,
                if p.required { ", required" } else { "" }
            )),
            Some(q) if q.kind != p.kind || q.required != p.required => lines.push(format!(
                "{} parameter {}: {}{} → {}{}",
                p.location,
                p.name,
                q.kind,
                if q.required { ", required" } else { "" },
                p.kind,
                if p.required { ", required" } else { "" }
            )),
            Some(_) => {}
        }
    }
    for q in &was.parameters {
        if find(&now.parameters, q).is_none() {
            lines.push(format!("{} parameter {} removed", q.location, q.name));
        }
    }
    match (&was.body, &now.body) {
        (None, Some(b)) => lines.push(format!(
            "body {} added",
            b.schema.clone().unwrap_or_else(|| b.media.clone())
        )),
        (Some(_), None) => lines.push("body removed".to_owned()),
        (Some(a), Some(b)) if a.schema != b.schema || a.required != b.required => {
            lines.push(format!(
                "body: {} → {}",
                a.schema.clone().unwrap_or_else(|| a.media.clone()),
                b.schema.clone().unwrap_or_else(|| b.media.clone())
            ));
        }
        _ => {}
    }
    for r in &now.responses {
        match was.responses.iter().find(|s| s.status == r.status) {
            None => lines.push(format!("answer {} added", r.status)),
            Some(s) if s.schema != r.schema => lines.push(format!(
                "answer {}: {} → {}",
                r.status,
                s.schema.as_deref().unwrap_or("-"),
                r.schema.as_deref().unwrap_or("-")
            )),
            Some(_) => {}
        }
    }
    for s in &was.responses {
        if !now.responses.iter().any(|r| r.status == s.status) {
            lines.push(format!("answer {} removed", s.status));
        }
    }
    lines
}

/// The string literals of `call`, in order, quotes left out.
pub(crate) fn strings(call: &str) -> Vec<String> {
    static STRING: LazyLock<Option<Regex>> =
        LazyLock::new(|| Regex::new(r#""([^"\\]*)"|'([^'\\]*)'"#).ok());
    STRING.as_ref().map_or_else(Vec::new, |re| {
        re.captures_iter(call)
            .filter_map(|c| c.get(1).or_else(|| c.get(2)))
            .map(|m| m.as_str().to_owned())
            .collect()
    })
}

/// The text of the call that opens at `open` in `text`, up to its closing
/// parenthesis, strings minded.
pub(crate) fn call_at(text: &str, open: usize) -> &str {
    let mut depth = 0;
    let mut quote: Option<char> = None;
    for (i, c) in text[open..].char_indices() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '"' | '\'') => quote = Some(c),
            (None, '(' | '[' | '{') => depth += 1,
            (None, ')' | ']' | '}') => {
                depth -= 1;
                if depth == 0 {
                    return &text[open..=open + i];
                }
            }
            _ => {}
        }
    }
    &text[open..]
}

/// What an Alembic migration does, if `text` is one: its `upgrade()` read
/// step by step.
///
/// # Examples
///
/// ```
/// let text = r#"
/// """Add index on asset.uri."""
/// revision = "c4e7"
/// down_revision = "436d"
///
/// def upgrade():
///     with op.batch_alter_table("asset", schema=None) as batch_op:
///         batch_op.create_index("idx_asset_uri", ["uri"], unique=False)
///
/// def downgrade():
///     with op.batch_alter_table("asset", schema=None) as batch_op:
///         batch_op.drop_index("idx_asset_uri")
/// "#;
/// let migration = ironquill_codemap::migration(text).unwrap();
/// assert_eq!(migration.title, "Add index on asset.uri.");
/// assert_eq!(migration.revision.as_deref(), Some("c4e7"));
/// assert_eq!(migration.steps.len(), 1);
/// assert_eq!(migration.steps[0].table.as_deref(), Some("asset"));
/// assert_eq!(migration.steps[0].what, "index idx_asset_uri on (uri)");
/// ```
#[must_use]
pub fn migration(text: &str) -> Option<Migration> {
    static UPGRADE: LazyLock<Option<Regex>> =
        LazyLock::new(|| Regex::new(r"(?m)^def upgrade\(").ok());
    static ASSIGN: LazyLock<Option<Regex>> = LazyLock::new(|| {
        Regex::new(r#"(?m)^(revision|down_revision)\s*(?::\s*[\w\[\], |]+)?=\s*["']([^"']*)["']"#)
            .ok()
    });
    static STEP: LazyLock<Option<Regex>> = LazyLock::new(|| {
        Regex::new(
            r"\b(\w+)\.(create_table|drop_table|rename_table|add_column|drop_column|alter_column|create_index|drop_index|create_foreign_key|create_unique_constraint|create_primary_key|create_check_constraint|drop_constraint|execute|batch_alter_table)\(",
        )
        .ok()
    });
    let start = UPGRADE.as_ref()?.find(text)?.start();
    if !text.contains("alembic") && !text.contains("revision") {
        return None;
    }
    let body = &text[start..];
    // The upgrade ends where the next function at the top starts.
    let end = body[1..].find("\ndef ").map_or(body.len(), |i| i + 1);
    let body = &body[..end];
    let mut revisions = BTreeMap::new();
    if let Some(assign) = ASSIGN.as_ref() {
        for c in assign.captures_iter(text) {
            revisions.insert(c[1].to_owned(), c[2].to_owned());
        }
    }
    let title = docstring(text).unwrap_or_default();
    // The table a `with op.batch_alter_table("t") as name:` names, for the
    // steps called on `name`.
    let mut batches: BTreeMap<String, String> = BTreeMap::new();
    let mut steps = Vec::new();
    for c in STEP.as_ref()?.captures_iter(body) {
        let (Some(whole), receiver, op) = (c.get(0), &c[1], &c[2]) else {
            continue;
        };
        let call = call_at(body, whole.end() - 1);
        let args = strings(call);
        if op == "batch_alter_table" {
            let after = &body[whole.end() - 1 + call.len()..];
            if let Some(name) = after
                .lines()
                .next()
                .and_then(|l| l.split(" as ").nth(1))
                .map(|n| n.trim().trim_end_matches(':').trim())
            {
                batches.insert(name.to_owned(), args.first().cloned().unwrap_or_default());
            }
            continue;
        }
        let batch = batches.get(receiver).cloned();
        // Called on a batch, the table is the batch's and is not repeated.
        let (table, rest) = match (&batch, op) {
            (Some(t), _) => (Some(t.clone()), args.clone()),
            (None, "create_index") => (args.get(1).cloned(), args.clone()),
            (None, "drop_index" | "execute") => (None, args.clone()),
            (None, _) => (
                args.first().cloned(),
                args.iter().skip(1).cloned().collect(),
            ),
        };
        let first = |i: usize| rest.get(i).cloned().unwrap_or_default();
        let columns = || {
            // The columns of an index: what its list holds.
            call.find('[')
                .map(|open| strings(call_at(call, open)).join(", "))
                .unwrap_or_default()
        };
        let unique = call.contains("unique=True");
        let (delta, what) = match op {
            "create_table" => (
                Delta::Added,
                format!("table {}", args.first().cloned().unwrap_or_default()),
            ),
            "drop_table" => (
                Delta::Removed,
                format!("table {}", args.first().cloned().unwrap_or_default()),
            ),
            "rename_table" => (Delta::Changed, format!("table renamed to {}", first(0))),
            "add_column" => (Delta::Added, format!("column {}", first(0))),
            "drop_column" => (Delta::Removed, format!("column {}", first(0))),
            "alter_column" => (Delta::Changed, format!("column {}", first(0))),
            "create_index" => (
                Delta::Added,
                format!(
                    "{}index {} on ({})",
                    if unique { "unique " } else { "" },
                    args.first().cloned().unwrap_or_default(),
                    columns()
                ),
            ),
            "drop_index" => (
                Delta::Removed,
                format!("index {}", args.first().cloned().unwrap_or_default()),
            ),
            "create_foreign_key" => (
                Delta::Added,
                format!("foreign key {}", args.first().cloned().unwrap_or_default()),
            ),
            "create_unique_constraint" => (
                Delta::Added,
                format!(
                    "unique constraint {}",
                    args.first().cloned().unwrap_or_default()
                ),
            ),
            "create_primary_key" => (
                Delta::Added,
                format!("primary key {}", args.first().cloned().unwrap_or_default()),
            ),
            "create_check_constraint" => (
                Delta::Added,
                format!("check {}", args.first().cloned().unwrap_or_default()),
            ),
            "drop_constraint" => (
                Delta::Removed,
                format!("constraint {}", args.first().cloned().unwrap_or_default()),
            ),
            _ => (Delta::Changed, "SQL run as written".to_owned()),
        };
        steps.push(SchemaChange { table, delta, what });
    }
    Some(Migration {
        title,
        revision: revisions.get("revision").cloned(),
        down_revision: revisions.get("down_revision").cloned(),
        steps,
    })
}

/// The first line of a module's docstring.
fn docstring(text: &str) -> Option<String> {
    let start = text.find("\"\"\"")? + 3;
    text[start..]
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(|l| l.trim_end_matches("\"\"\"").to_owned())
}

/// A table a model defines, as a change to it shows: its columns and its
/// indexes by name, each with its definition as written.
struct Table {
    columns: BTreeMap<String, String>,
    indexes: BTreeMap<String, String>,
}

/// The tables the models in `text` define, by name.
fn tables(text: &str) -> BTreeMap<String, Table> {
    crate::schema::tables_in("", text)
        .into_iter()
        .map(|t| {
            let table = Table {
                columns: t
                    .columns
                    .into_iter()
                    .map(|c| (c.name, c.definition))
                    .collect(),
                indexes: t
                    .indexes
                    .into_iter()
                    .map(|i| (i.name, i.definition))
                    .collect(),
            };
            (t.name, table)
        })
        .collect()
}

/// What the tables of a models file gained, lost or changed between two
/// versions of it.
///
/// # Examples
///
/// ```
/// let before = "class Asset(Base):\n    __tablename__ = \"asset\"\n    uri: Mapped[str] = mapped_column(String(1500))\n    __table_args__ = (Index(\"idx_name\", name),)\n";
/// let after = "class Asset(Base):\n    __tablename__ = \"asset\"\n    uri: Mapped[str] = mapped_column(String(1500))\n    group: Mapped[str] = mapped_column(String(100))\n    __table_args__ = (Index(\"idx_name\", name), Index(\"idx_asset_uri\", uri))\n";
/// let changes = ironquill_codemap::model_changes(before, after);
/// let lines: Vec<String> = changes.iter().map(|c| format!("{:?} {}", c.delta, c.what)).collect();
/// assert_eq!(lines, ["Added column group", "Added index idx_asset_uri"]);
/// ```
#[must_use]
pub fn model_changes(before: &str, after: &str) -> Vec<SchemaChange> {
    let old = tables(before);
    let new = tables(after);
    let mut changes = Vec::new();
    let change = |table: &str, delta, what: String| SchemaChange {
        table: Some(table.to_owned()),
        delta,
        what,
    };
    for (name, table) in &new {
        let Some(was) = old.get(name) else {
            changes.push(change(name, Delta::Added, format!("table {name}")));
            continue;
        };
        for (kind, now, then) in [
            ("column", &table.columns, &was.columns),
            ("index", &table.indexes, &was.indexes),
        ] {
            for (item, definition) in now {
                match then.get(item) {
                    None => changes.push(change(name, Delta::Added, format!("{kind} {item}"))),
                    Some(old) if old != definition => {
                        changes.push(change(name, Delta::Changed, format!("{kind} {item}")));
                    }
                    Some(_) => {}
                }
            }
            for item in then.keys().filter(|k| !now.contains_key(*k)) {
                changes.push(change(name, Delta::Removed, format!("{kind} {item}")));
            }
        }
    }
    for name in old.keys().filter(|k| !new.contains_key(*k)) {
        changes.push(change(name, Delta::Removed, format!("table {name}")));
    }
    changes
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(parameters: &str) -> String {
        format!(
            "openapi: 3.0.0\npaths:\n  /assets:\n    get:\n      summary: List assets\n{parameters}      responses:\n        '200':\n          description: ok\n  /old:\n    delete:\n      summary: Gone soon\n"
        )
    }

    #[test]
    fn a_route_gains_a_parameter_another_is_removed_a_third_appears() {
        let before = crate::api::spec_operations(&spec(""));
        let mut after_text = spec(
            "      parameters:\n        - name: uri\n          in: query\n          schema:\n            type: array\n            items: {type: string}\n",
        );
        after_text = after_text.replace(
            "  /old:\n    delete:\n      summary: Gone soon\n",
            "  /new:\n    post:\n      summary: Make one\n",
        );
        let after = crate::api::spec_operations(&after_text);
        let changes = route_changes(&before, &after);
        let lines: Vec<String> = changes
            .iter()
            .map(|c| {
                format!(
                    "{:?} {} {}: {}",
                    c.delta,
                    c.method,
                    c.path,
                    c.details.join("; ")
                )
            })
            .collect();
        assert_eq!(
            lines,
            [
                "Changed GET /assets: query parameter uri (string[]) added",
                "Added POST /new: Make one",
                "Removed DELETE /old: Gone soon",
            ]
        );
    }

    #[test]
    fn files_go_to_their_areas_and_a_router_belongs_to_the_api() {
        let file = |path: &str, after: &str| Revised {
            path: path.into(),
            before: None,
            after: Some(after.into()),
        };
        let read = review(&[
            file(
                "api/routes/assets.py",
                "@router.get(\"/assets\")\ndef get_assets(): ...\n",
            ),
            file(
                "ui/openapi-gen/requests/types.gen.ts",
                "export type A = {};\n",
            ),
            file("ui/src/pages/Assets.tsx", "export const A = () => null;\n"),
            file("tests/test_assets.py", "def test_a(): ...\n"),
            file("docs/migrations-ref.rst", "x\n"),
            file("utils/db.py", "HEADS = {}\n"),
            file("api_fastapi/common/parameters.py", "QueryLimit = int\n"),
            file(
                "tests/api/test_routes.py",
                "@router.get(\"/x\")\ndef t(): ...\n",
            ),
            file(
                "models/asset.py",
                "class AssetModel(Base):\n    __tablename__ = \"asset\"\n    uri = Column(String)\n",
            ),
        ]);
        let areas: Vec<(Area, Vec<&str>)> = read
            .areas
            .iter()
            .map(|(a, files)| (*a, files.iter().map(String::as_str).collect()))
            .collect();
        assert_eq!(
            areas,
            [
                (
                    Area::Api,
                    vec!["api/routes/assets.py", "api_fastapi/common/parameters.py"]
                ),
                (Area::Database, vec!["models/asset.py"]),
                (Area::Backend, vec!["utils/db.py"]),
                (Area::FrontEnd, vec!["ui/src/pages/Assets.tsx"]),
                (
                    Area::Generated,
                    vec!["ui/openapi-gen/requests/types.gen.ts"]
                ),
                (
                    Area::Tests,
                    vec!["tests/test_assets.py", "tests/api/test_routes.py"]
                ),
                (Area::Documents, vec!["docs/migrations-ref.rst"]),
            ]
        );
        // A new model file: its table, added.
        assert_eq!(read.models.len(), 1);
        assert_eq!(read.models[0].1[0].what, "table asset");
    }

    #[test]
    fn a_migration_on_its_own_names_its_tables_and_a_table_object_is_a_model() {
        let text = "\"\"\"Add columns.\"\"\"\nrevision = \"a\"\ndown_revision = \"b\"\n\ndef upgrade():\n    op.add_column(\"dag\", sa.Column(\"draining\", sa.Boolean()))\n    op.create_index(\"idx_dag_draining\", \"dag\", [\"draining\"], unique=True)\n    op.create_table(\n        \"note\",\n        sa.Column(\"id\", sa.Integer()),\n    )\n\ndef downgrade():\n    op.drop_column(\"dag\", \"draining\")\n";
        let migration = migration(text).unwrap();
        let steps: Vec<String> = migration
            .steps
            .iter()
            .map(|s| {
                format!(
                    "{:?} {} {}",
                    s.delta,
                    s.table.as_deref().unwrap_or("-"),
                    s.what
                )
            })
            .collect();
        assert_eq!(
            steps,
            [
                "Added dag column draining",
                "Added dag unique index idx_dag_draining on (draining)",
                "Added note table note",
            ]
        );
        assert_eq!(migration.down_revision.as_deref(), Some("b"));

        let before = "t = Table(\n    \"link\",\n    Base.metadata,\n    Column(\"a_id\", ForeignKey(\"a.id\")),\n)\n";
        let after = "t = Table(\n    \"link\",\n    Base.metadata,\n    Column(\"a_id\", ForeignKey(\"a.id\"), primary_key=True),\n    Index(\"idx_link_a\", \"a_id\"),\n)\n";
        let lines: Vec<String> = model_changes(before, after)
            .iter()
            .map(|c| format!("{:?} {}", c.delta, c.what))
            .collect();
        assert_eq!(lines, ["Changed column a_id", "Added index idx_link_a"]);
        assert!(migration_free("def upgrade():\n    pass\n"));
    }

    fn migration_free(text: &str) -> bool {
        migration(text).is_none()
    }
}
