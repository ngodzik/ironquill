//! The database a project's models define, read from their code: the
//! tables of SQLAlchemy (classes with a `__tablename__`, `Table(...)`
//! objects) and SQLModel (classes with `table=True`), their columns with
//! their types, keys and constraints, their indexes, and the foreign keys
//! that link them.
//!
//! Read by patterns, never run: a table whose name is computed, or whose
//! columns come from elsewhere, is marked as read in part rather than
//! guessed.

use std::collections::HashMap;
use std::sync::LazyLock;

use regex::Regex;

/// Where a foreign key points.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reference {
    /// The table.
    pub table: String,
    /// The column; empty when not said.
    pub column: String,
}

/// A column of a table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Column {
    /// Its name in the database.
    pub name: String,
    /// Its type, as written: `String(250)`, `Integer`, `UtcDateTime`.
    pub kind: String,
    /// Whether it is part of the primary key.
    pub primary: bool,
    /// Whether it may be null.
    pub nullable: bool,
    /// Whether its values are unique.
    pub unique: bool,
    /// Whether it has an index of its own.
    pub indexed: bool,
    /// The column it points at, when it is a foreign key.
    pub references: Option<Reference>,
    /// Its line, from 1.
    pub line: usize,
    /// Its definition as written, spaces evened out: what a change to it
    /// changes.
    pub(crate) definition: String,
}

/// An index or a unique constraint of a table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Index {
    /// Its name.
    pub name: String,
    /// The columns it covers, in order.
    pub columns: Vec<String>,
    /// Whether it keeps values unique.
    pub unique: bool,
    /// Its definition as written, spaces evened out.
    pub(crate) definition: String,
}

/// A table of the database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Table {
    /// Its name.
    pub name: String,
    /// The class that maps it, when one does.
    pub model: Option<String>,
    /// The file that defines it, from the project's root.
    pub path: String,
    /// Its line there, from 1.
    pub line: usize,
    /// Its columns, in the order defined.
    pub columns: Vec<Column>,
    /// Its indexes and unique constraints.
    pub indexes: Vec<Index>,
    /// Why it is read in part only, when it is.
    pub partly: Option<String>,
}

/// The database a project's models define.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Schema {
    /// The tables, by name.
    pub tables: Vec<Table>,
}

/// A foreign key between two tables, by their places in
/// [`Schema::tables`] and their columns'.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ForeignKey {
    /// The table that points.
    pub from: usize,
    /// Its column.
    pub from_column: usize,
    /// The table pointed at.
    pub to: usize,
    /// Its column, when found.
    pub to_column: Option<usize>,
}

impl Schema {
    /// The table named `name`.
    #[must_use]
    pub fn table(&self, name: &str) -> Option<usize> {
        self.tables.iter().position(|t| t.name == name)
    }

    /// Every foreign key whose table pointed at is in the schema.
    #[must_use]
    pub fn foreign_keys(&self) -> Vec<ForeignKey> {
        let mut links = Vec::new();
        for (from, table) in self.tables.iter().enumerate() {
            for (from_column, column) in table.columns.iter().enumerate() {
                let Some(reference) = &column.references else {
                    continue;
                };
                let Some(to) = self.table(&reference.table) else {
                    continue;
                };
                let to_column = self.tables[to]
                    .columns
                    .iter()
                    .position(|c| c.name == reference.column);
                links.push(ForeignKey {
                    from,
                    from_column,
                    to,
                    to_column,
                });
            }
        }
        links
    }
}

/// The schema the models in `files`, each a path from the project's root
/// and its text, define: their tables by name, the foreign keys written
/// through a model (`DagModel.dag_id`) followed to its table.
///
/// # Examples
///
/// ```
/// let text = r#"
/// class Team(Base):
///     __tablename__ = "team"
///     id: Mapped[int] = mapped_column(Integer, primary_key=True)
///
/// class Hero(SQLModel, table=True):
///     id: int | None = Field(default=None, primary_key=True)
///     team_id: int | None = Field(default=None, foreign_key="team.id")
/// "#;
/// let schema = ironquill_codemap::schema(&[("models.py".into(), text.into())]);
/// let names: Vec<&str> = schema.tables.iter().map(|t| t.name.as_str()).collect();
/// assert_eq!(names, ["hero", "team"]);
/// let links = schema.foreign_keys();
/// assert_eq!(links.len(), 1);
/// assert_eq!(schema.tables[links[0].to].name, "team");
/// ```
#[must_use]
pub fn schema(files: &[(String, String)]) -> Schema {
    let mut tables: Vec<Table> = files
        .iter()
        .flat_map(|(path, text)| tables_in(path, text))
        .collect();
    // A model's name stands for its table in `ForeignKey(Model.column)`.
    let by_model: HashMap<String, String> = tables
        .iter()
        .filter_map(|t| Some((t.model.clone()?, t.name.clone())))
        .collect();
    for table in &mut tables {
        for column in &mut table.columns {
            if let Some(reference) = &mut column.references
                && let Some(name) = by_model.get(&reference.table)
            {
                reference.table.clone_from(name);
            }
        }
    }
    tables.sort_by(|a, b| a.name.cmp(&b.name).then(a.path.cmp(&b.path)));
    tables.dedup_by(|a, b| a.name == b.name);
    Schema { tables }
}

/// The tables the models of one file define.
pub(crate) fn tables_in(path: &str, text: &str) -> Vec<Table> {
    static CLASS: LazyLock<Option<Regex>> =
        LazyLock::new(|| Regex::new(r"(?m)^class (\w+)\b(?:\(([^)]*)\))?\s*:").ok());
    static TABLENAME: LazyLock<Option<Regex>> = LazyLock::new(|| {
        Regex::new(r#"(?m)^\s+__tablename__\s*(?::\s*[\w\[\]]+\s*)?=\s*(.+)$"#).ok()
    });
    static TABLE: LazyLock<Option<Regex>> = LazyLock::new(|| {
        Regex::new(
            r#"(?m)^\w+\s*(?::[^=\n]+)?=\s*(?:sa\.|sqlalchemy\.)?Table\(\s*["']([^"']+)["']"#,
        )
        .ok()
    });
    let (Some(class), Some(tablename), Some(table_re)) =
        (CLASS.as_ref(), TABLENAME.as_ref(), TABLE.as_ref())
    else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for c in class.captures_iter(text) {
        let (Some(whole), Some(model)) = (c.get(0), c.get(1)) else {
            continue;
        };
        let bases = c.get(2).map_or("", |b| b.as_str());
        let body = class_body(&text[whole.start()..]);
        let start = whole.start();
        let sqlmodel = bases.contains("table=True") || bases.contains("table = True");
        let named = tablename.captures(body).map(|t| t[1].trim().to_owned());
        let (name, partly) = match (named, sqlmodel) {
            (Some(value), _) => match literal(&value) {
                Some(name) => (name, None),
                None => (
                    model.as_str().to_lowercase(),
                    Some(format!("its name is computed: {value}")),
                ),
            },
            // SQLModel names a table after its class, in lower case.
            (None, true) => (model.as_str().to_lowercase(), None),
            (None, false) => continue,
        };
        let line = line_of(text, start);
        let mut table = Table {
            name,
            model: Some(model.as_str().to_owned()),
            path: path.to_owned(),
            line,
            columns: class_columns(body, line, sqlmodel),
            indexes: Vec::new(),
            partly,
        };
        if body.contains("declared_attr") && table.partly.is_none() {
            table.partly = Some("some columns come from @declared_attr".to_owned());
        }
        if let Some(open) = body.find("__table_args__") {
            let rest = &body[open..];
            if let Some(paren) = rest.find(['(', '{']) {
                let args = call_at(rest, paren);
                constraints(args, &mut table);
            }
        }
        found.push(table);
    }
    for c in table_re.captures_iter(text) {
        let Some(whole) = c.get(0) else { continue };
        let open = text[..whole.end()].rfind('(').unwrap_or(whole.end());
        let call = call_at(text, open);
        let line = line_of(text, whole.start());
        let mut table = Table {
            name: c[1].to_owned(),
            model: None,
            path: path.to_owned(),
            line,
            columns: Vec::new(),
            indexes: Vec::new(),
            partly: None,
        };
        let inner = &call[1..call.len().saturating_sub(1)];
        let mut offset = 1;
        for arg in split_args(inner) {
            let trimmed = arg.trim();
            let here = line + call[..offset].matches('\n').count();
            if let Some(rest) = strip_call(trimmed, "Column")
                && let Some(column) = column_of(None, None, rest, here)
            {
                table.columns.push(column);
            }
            offset += arg.len() + 1;
        }
        constraints(inner, &mut table);
        found.push(table);
    }
    found
}

/// A class's body: its first line, then up to the next line that is not
/// indented and not blank.
fn class_body(rest: &str) -> &str {
    let first = rest.find('\n').map_or(rest.len(), |i| i + 1);
    let mut at = first;
    for line in rest[first..].split_inclusive('\n') {
        if !line.trim().is_empty() && !line.starts_with(char::is_whitespace) {
            return &rest[..at];
        }
        at += line.len();
    }
    rest
}

/// The columns a class's `body`, starting at `line`, declares.
fn class_columns(body: &str, line: usize, sqlmodel: bool) -> Vec<Column> {
    static ATTRIBUTE: LazyLock<Option<Regex>> =
        LazyLock::new(|| Regex::new(r"(?m)^    (\w+)\s*(?::\s*([^=\n]+?))?\s*(?:=\s*(.*))?$").ok());
    let Some(attribute) = ATTRIBUTE.as_ref() else {
        return Vec::new();
    };
    let mut columns = Vec::new();
    for c in attribute.captures_iter(body) {
        let (Some(whole), Some(name)) = (c.get(0), c.get(1)) else {
            continue;
        };
        let name = name.as_str();
        if name.starts_with("__") {
            continue;
        }
        let annotation = c.get(2).map(|a| a.as_str().trim());
        let value = c.get(3).map_or("", |v| v.as_str().trim());
        let here = line + body[..whole.start()].matches('\n').count();
        let call = |callee: &str| {
            let open = whole.start() + whole.as_str().find(&format!("{callee}("))? + callee.len();
            Some(call_at(body, open))
        };
        let callee = value
            .split('(')
            .next()
            .unwrap_or("")
            .rsplit('.')
            .next()
            .unwrap_or("");
        let column = match callee {
            "mapped_column" | "Column" => {
                call(callee).and_then(|args| column_of(Some(name), annotation, args, here))
            }
            "Field" if sqlmodel => {
                call("Field").and_then(|args| field_of(name, annotation, args, here))
            }
            "relationship" | "Relationship" | "association_proxy" | "column_property"
            | "synonym" | "hybrid_property" => None,
            // In an SQLModel table, every field annotated is a column.
            _ if sqlmodel => annotation
                .filter(|a| {
                    !a.starts_with("ClassVar") && !a.contains("list[") && !a.contains("List[")
                })
                .map(|a| Column {
                    name: name.to_owned(),
                    kind: type_of(a),
                    primary: false,
                    nullable: optional(a),
                    unique: false,
                    indexed: false,
                    references: None,
                    line: here,
                    definition: even(whole.as_str()),
                }),
            _ => None,
        };
        columns.extend(column);
    }
    columns
}

/// A column from the arguments of `mapped_column(...)` or `Column(...)`,
/// `args` with its parentheses, for the attribute `attribute` annotated
/// `annotation`.
fn column_of(
    attribute: Option<&str>,
    annotation: Option<&str>,
    args: &str,
    line: usize,
) -> Option<Column> {
    let inner = args.get(1..args.len().saturating_sub(1)).unwrap_or("");
    let parts = split_args(inner);
    let mut name = attribute.map(str::to_owned);
    let mut kind = None;
    let mut references = None;
    let mut keywords: HashMap<String, String> = HashMap::new();
    for part in &parts {
        let part = part.trim();
        if let Some((key, value)) = keyword(part) {
            keywords.insert(key.to_owned(), value.trim().to_owned());
            continue;
        }
        if let Some(text) = literal(part) {
            // The column's own name in the database, before its type.
            if kind.is_none() {
                name = Some(text);
            }
            continue;
        }
        if let Some(rest) = strip_call(part, "ForeignKey") {
            references = reference_of(rest);
            continue;
        }
        if kind.is_none() && !part.is_empty() {
            kind = Some(clean_type(part));
        }
    }
    let flag = |key: &str| keywords.get(key).map(String::as_str) == Some("True");
    let primary = flag("primary_key");
    let annotated = annotation.map(mapped_inner);
    let nullable = match keywords.get("nullable").map(String::as_str) {
        Some("False") => false,
        Some("True") => true,
        _ => match annotation {
            Some(a) => optional(a),
            None => !primary,
        },
    };
    Some(Column {
        name: name?,
        kind: kind
            .or_else(|| annotated.map(type_of))
            .unwrap_or_else(|| "?".to_owned()),
        primary,
        nullable: nullable && !primary,
        unique: flag("unique"),
        indexed: flag("index"),
        references,
        line,
        definition: even(args),
    })
}

/// A column from SQLModel's `Field(...)`.
fn field_of(name: &str, annotation: Option<&str>, args: &str, line: usize) -> Option<Column> {
    let inner = args.get(1..args.len().saturating_sub(1)).unwrap_or("");
    let mut keywords: HashMap<String, String> = HashMap::new();
    for part in split_args(inner) {
        if let Some((key, value)) = keyword(part.trim()) {
            keywords.insert(key.to_owned(), value.trim().to_owned());
        }
    }
    let flag = |key: &str| keywords.get(key).map(String::as_str) == Some("True");
    let primary = flag("primary_key");
    let references = keywords
        .get("foreign_key")
        .and_then(|k| literal(k))
        .and_then(|target| reference_of(&format!("(\"{target}\")")));
    let nullable = match keywords.get("nullable").map(String::as_str) {
        Some("False") => false,
        Some("True") => true,
        _ => annotation.is_some_and(optional),
    };
    Some(Column {
        name: name.to_owned(),
        kind: annotation.map_or_else(|| "?".to_owned(), type_of),
        primary,
        nullable: nullable && !primary,
        unique: flag("unique"),
        indexed: flag("index"),
        references,
        line,
        definition: even(args),
    })
}

/// The indexes, unique constraints, composite keys and foreign keys that
/// `args` (a `__table_args__` or a `Table(...)`'s arguments) declares, set
/// on `table`.
fn constraints(args: &str, table: &mut Table) {
    static CALL: LazyLock<Option<Regex>> = LazyLock::new(|| {
        Regex::new(
            r"\b(?:sa\.|sqlalchemy\.)?(Index|UniqueConstraint|PrimaryKeyConstraint|ForeignKeyConstraint)\(",
        )
        .ok()
    });
    let Some(call_re) = CALL.as_ref() else {
        return;
    };
    // An attribute names its column, which may be named otherwise.
    let column_named = |table: &Table, word: &str| -> String {
        table
            .columns
            .iter()
            .find(|c| c.name == word)
            .map_or_else(|| word.to_owned(), |c| c.name.clone())
    };
    for c in call_re.captures_iter(args) {
        let (Some(whole), Some(kind)) = (c.get(0), c.get(1)) else {
            continue;
        };
        let call = call_at(args, whole.end() - 1);
        let inner = call.get(1..call.len().saturating_sub(1)).unwrap_or("");
        let parts = split_args(inner);
        let mut positional: Vec<String> = Vec::new();
        let mut keywords: HashMap<String, String> = HashMap::new();
        for part in &parts {
            let part = part.trim();
            match keyword(part) {
                Some((key, value)) => {
                    keywords.insert(key.to_owned(), value.trim().to_owned());
                }
                None if !part.is_empty() => positional.push(part.to_owned()),
                None => {}
            }
        }
        let word = |part: &str| {
            literal(part)
                .unwrap_or_else(|| part.rsplit('.').next().unwrap_or(part).trim().to_owned())
        };
        match kind.as_str() {
            "Index" => {
                let mut items = positional.iter();
                let name = items.next().map(|n| word(n)).unwrap_or_default();
                let columns = items.map(|p| column_named(table, &word(p))).collect();
                table.indexes.push(Index {
                    name,
                    columns,
                    unique: keywords.get("unique").map(String::as_str) == Some("True"),
                    definition: even(call),
                });
            }
            "UniqueConstraint" => {
                let columns = positional
                    .iter()
                    .map(|p| column_named(table, &word(p)))
                    .collect();
                let name = keywords
                    .get("name")
                    .and_then(|n| literal(n))
                    .unwrap_or_else(|| "unique".to_owned());
                table.indexes.push(Index {
                    name,
                    columns,
                    unique: true,
                    definition: even(call),
                });
            }
            "PrimaryKeyConstraint" => {
                for part in &positional {
                    let name = column_named(table, &word(part));
                    if let Some(column) = table.columns.iter_mut().find(|c| c.name == name) {
                        column.primary = true;
                        column.nullable = false;
                    }
                }
            }
            _ => {
                // `ForeignKeyConstraint(["a", "b"], ["t.a", "t.b"])`.
                // Given in order, or named: `columns=`, `refcolumns=`.
                let given: Vec<&String> =
                    match (keywords.get("columns"), keywords.get("refcolumns")) {
                        (Some(local), Some(remote)) => vec![local, remote],
                        _ => positional.iter().take(2).collect(),
                    };
                let lists: Vec<Vec<String>> = given
                    .into_iter()
                    .map(|p| {
                        // Names or attributes, in a list.
                        let inner = p
                            .trim()
                            .trim_start_matches(['[', '('])
                            .trim_end_matches([']', ')']);
                        split_args(inner)
                            .into_iter()
                            .map(|item| word(item.trim()))
                            .collect()
                    })
                    .collect();
                if let [locals, remotes] = lists.as_slice() {
                    for (local, remote) in locals.iter().zip(remotes) {
                        let name = column_named(table, local);
                        let remote = remote.as_str();
                        if let Some(column) = table.columns.iter_mut().find(|c| c.name == name) {
                            column.references = reference_of(&format!("(\"{remote}\")"));
                        }
                    }
                }
            }
        }
    }
}

/// Where `ForeignKey(...)`'s arguments point: `"dag.dag_id"`, or
/// `DagModel.dag_id`, whose model [`schema`] follows to its table.
fn reference_of(args: &str) -> Option<Reference> {
    let inner = args.trim().strip_prefix('(').unwrap_or(args);
    let first = split_args(inner.strip_suffix(')').unwrap_or(inner))
        .into_iter()
        .next()?;
    let first = first.trim();
    let target = literal(first).unwrap_or_else(|| first.to_owned());
    // A schema may come first: `"public.dag.dag_id"`.
    let mut parts: Vec<&str> = target.split('.').collect();
    let column = if parts.len() >= 2 { parts.pop()? } else { "" };
    let table = parts.pop()?;
    Some(Reference {
        table: table.to_owned(),
        column: column.to_owned(),
    })
}

/// `Mapped[X]` as `X`.
fn mapped_inner(annotation: &str) -> &str {
    annotation
        .trim()
        .strip_prefix("Mapped[")
        .and_then(|a| a.strip_suffix(']'))
        .unwrap_or(annotation)
}

/// Whether a type annotation lets the value be `None`.
fn optional(annotation: &str) -> bool {
    let inner = mapped_inner(annotation);
    inner.contains("None") || inner.starts_with("Optional[")
}

/// A type annotation as a column's type: `int | None` as `int`.
fn type_of(annotation: &str) -> String {
    let inner = mapped_inner(annotation).trim();
    let inner = inner
        .strip_prefix("Optional[")
        .and_then(|i| i.strip_suffix(']'))
        .unwrap_or(inner);
    inner
        .split('|')
        .map(str::trim)
        .find(|part| *part != "None")
        .unwrap_or(inner)
        .to_owned()
}

/// A type written in a call, its module left out: `sa.String(250)` as
/// `String(250)`.
fn clean_type(part: &str) -> String {
    let head = part.split('(').next().unwrap_or(part);
    let short = head.rsplit('.').next().unwrap_or(head);
    format!("{short}{}", &part[head.len()..])
}

/// `key=value`, at the top of an argument, not `==`.
fn keyword(part: &str) -> Option<(&str, &str)> {
    let (key, value) = part.split_once('=')?;
    let key = key.trim();
    (key.chars().all(|c| c.is_alphanumeric() || c == '_')
        && !value.starts_with('=')
        && !key.is_empty())
    .then_some((key, value))
}

/// The text of a string literal, quotes left out.
fn literal(text: &str) -> Option<String> {
    let text = text.trim();
    let quote = text.chars().next().filter(|c| *c == '"' || *c == '\'')?;
    let inner = text.strip_prefix(quote)?.strip_suffix(quote)?;
    (!inner.contains(quote)).then(|| inner.to_owned())
}

/// The arguments of `Name(...)` with their parentheses, when `part` is a
/// call to `Name` (or `module.Name`).
fn strip_call<'a>(part: &'a str, name: &str) -> Option<&'a str> {
    let open = part.find('(')?;
    let callee = part[..open].trim();
    (callee == name || callee.ends_with(&format!(".{name}"))).then(|| &part[open..])
}

/// The text of the call that opens at `open` in `text`.
fn call_at(text: &str, open: usize) -> &str {
    crate::review::call_at(text, open)
}

/// The arguments of a call, split at its commas, nested brackets and
/// strings minded.
fn split_args(text: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth = 0_i32;
    let mut quote: Option<char> = None;
    let mut start = 0;
    for (i, c) in text.char_indices() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '"' | '\'') => quote = Some(c),
            (None, '(' | '[' | '{') => depth += 1,
            (None, ')' | ']' | '}') => depth -= 1,
            (None, ',') if depth == 0 => {
                parts.push(&text[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    if !text[start..].trim().is_empty() {
        parts.push(&text[start..]);
    }
    parts
}

/// The line, from 1, of the byte `at` of `text`.
fn line_of(text: &str, at: usize) -> usize {
    text[..at].matches('\n').count() + 1
}

/// Spaces evened out.
fn even(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(text: &str) -> Schema {
        schema(&[("models.py".to_owned(), text.to_owned())])
    }

    #[test]
    fn sqlalchemy_columns_keys_and_constraints() {
        let text = r#"
class DagModel(Base):
    __tablename__ = "dag"
    dag_id: Mapped[str] = mapped_column(StringID(), primary_key=True)
    is_paused: Mapped[bool] = mapped_column(Boolean, default=False)
    description: Mapped[str | None] = mapped_column(Text, nullable=True)
    owners = Column(String(2000))

class DagRun(Base):
    __tablename__ = "dag_run"
    id: Mapped[int] = mapped_column(Integer, primary_key=True)
    dag_id: Mapped[str] = mapped_column(
        StringID(),
        ForeignKey("dag.dag_id", ondelete="CASCADE"),
        nullable=False,
    )
    run_id: Mapped[str] = mapped_column("run_id", String(250), index=True)
    dag = relationship("DagModel")
    __table_args__ = (
        Index("dag_id_state", dag_id, "state"),
        UniqueConstraint("dag_id", "run_id", name="dag_run_dag_id_run_id_key"),
    )

class TaskInstance(Base):
    __tablename__ = "task_instance"
    run_id = Column(String(250), nullable=False)
    dag_id = Column(String(250), nullable=False)
    trigger_id = Column(Integer)
    __table_args__ = (
        PrimaryKeyConstraint("dag_id", "run_id", name="task_instance_pkey"),
        ForeignKeyConstraint([dag_id, run_id], ["dag_run.dag_id", "dag_run.run_id"]),
        ForeignKeyConstraint(
            columns=(trigger_id,),
            refcolumns=["trigger.id"],
            name="ti_trigger_id_fkey",
        ),
    )
"#;
        let schema = read(text);
        let names: Vec<&str> = schema.tables.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["dag", "dag_run", "task_instance"]);

        let dag = &schema.tables[0];
        assert_eq!(dag.model.as_deref(), Some("DagModel"));
        assert_eq!(dag.line, 2);
        let columns: Vec<(&str, &str, bool, bool)> = dag
            .columns
            .iter()
            .map(|c| (c.name.as_str(), c.kind.as_str(), c.primary, c.nullable))
            .collect();
        assert_eq!(
            columns,
            [
                ("dag_id", "StringID()", true, false),
                ("is_paused", "Boolean", false, false),
                ("description", "Text", false, true),
                ("owners", "String(2000)", false, true),
            ]
        );

        let run = &schema.tables[1];
        assert_eq!(run.columns.len(), 3, "the relationship is no column");
        let dag_id = &run.columns[1];
        assert_eq!(
            dag_id.references,
            Some(Reference {
                table: "dag".into(),
                column: "dag_id".into()
            })
        );
        assert!(!dag_id.nullable);
        assert_eq!(dag_id.line, 12);
        assert!(run.columns[2].indexed);
        let indexes: Vec<(&str, Vec<&str>, bool)> = run
            .indexes
            .iter()
            .map(|i| {
                (
                    i.name.as_str(),
                    i.columns.iter().map(String::as_str).collect(),
                    i.unique,
                )
            })
            .collect();
        assert_eq!(
            indexes,
            [
                ("dag_id_state", vec!["dag_id", "state"], false),
                ("dag_run_dag_id_run_id_key", vec!["dag_id", "run_id"], true),
            ]
        );

        let ti = &schema.tables[2];
        assert!(ti.columns[..2].iter().all(|c| c.primary));
        assert_eq!(ti.columns[1].references.as_ref().unwrap().table, "dag_run");
        // Named, as some models write them.
        assert_eq!(ti.columns[2].references.as_ref().unwrap().table, "trigger");

        let links = schema.foreign_keys();
        let pairs: Vec<(&str, &str)> = links
            .iter()
            .map(|l| {
                (
                    schema.tables[l.from].name.as_str(),
                    schema.tables[l.to].name.as_str(),
                )
            })
            .collect();
        assert_eq!(
            pairs,
            [
                ("dag_run", "dag"),
                ("task_instance", "dag_run"),
                ("task_instance", "dag_run")
            ]
        );
        assert_eq!(links[0].to_column, Some(0));
    }

    #[test]
    fn table_objects_models_named_in_keys_and_what_cannot_be_read() {
        let text = r#"
asset_alias_asset = Table(
    "asset_alias_asset",
    Base.metadata,
    Column("alias_id", ForeignKey("asset_alias.id", ondelete="CASCADE"), primary_key=True),
    Column("asset_id", ForeignKey(AssetModel.id), primary_key=True),
    Index("idx_asset_alias_asset_alias_id", "alias_id"),
)

class AssetModel(Base):
    __tablename__ = "asset"
    id: Mapped[int] = mapped_column(Integer, primary_key=True)

class AssetAlias(Base):
    __tablename__ = "asset_alias"
    id: Mapped[int] = mapped_column(Integer, primary_key=True)

class Dynamic(Base):
    __tablename__ = prefix + "_dynamic"
    id = Column(Integer, primary_key=True)

class Mixin:
    created = Column(DateTime)
"#;
        let schema = read(text);
        let names: Vec<&str> = schema.tables.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(
            names,
            ["asset", "asset_alias", "asset_alias_asset", "dynamic"]
        );
        let link = &schema.tables[2];
        assert_eq!(link.model, None);
        assert_eq!(link.columns.len(), 2);
        assert!(link.columns.iter().all(|c| c.primary && c.kind == "?"));
        // A model named in a key stands for its table.
        assert_eq!(link.columns[1].references.as_ref().unwrap().table, "asset");
        assert_eq!(link.indexes[0].columns, ["alias_id"]);
        assert_eq!(schema.foreign_keys().len(), 2);
        assert!(
            schema.tables[3]
                .partly
                .as_deref()
                .unwrap()
                .contains("computed")
        );
    }

    #[test]
    fn sqlmodel_tables_take_their_class_name_and_every_field() {
        let text = r#"
class HeroBase(SQLModel):
    name: str

class Hero(HeroBase, table=True):
    id: int | None = Field(default=None, primary_key=True)
    name: str = Field(index=True)
    age: Optional[int] = None
    team_id: int | None = Field(default=None, foreign_key="team.id")
    team: Team | None = Relationship(back_populates="heroes")
    tags: list["Tag"] = Relationship()
"#;
        let schema = read(text);
        assert_eq!(schema.tables.len(), 1);
        let hero = &schema.tables[0];
        assert_eq!(hero.name, "hero");
        let columns: Vec<(&str, &str, bool, bool)> = hero
            .columns
            .iter()
            .map(|c| (c.name.as_str(), c.kind.as_str(), c.primary, c.nullable))
            .collect();
        assert_eq!(
            columns,
            [
                ("id", "int", true, false),
                ("name", "str", false, false),
                ("age", "int", false, true),
                ("team_id", "int", false, true),
            ]
        );
        assert!(hero.columns[1].indexed);
        assert_eq!(hero.columns[3].references.as_ref().unwrap().table, "team");
    }
}
