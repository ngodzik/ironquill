//! YAML read with the line of every value, which `yaml_rust2`'s own
//! loader drops: a setting shown is a setting whose place can be opened.

use serde::{Deserialize, Serialize};
use yaml_rust2::parser::{Event, MarkedEventReceiver, Parser};
use yaml_rust2::scanner::{Marker, TScalarStyle};

/// A YAML value and the line it starts on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Node {
    /// What it holds.
    pub(crate) value: Value,
    /// Its line, from 1.
    pub(crate) line: usize,
}

/// What a YAML node holds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum Value {
    /// A mapping, its keys in their order.
    Map(Vec<(String, Node)>),
    /// A sequence.
    Seq(Vec<Node>),
    /// A scalar, as written; `quoted` when it cannot be a number, a
    /// boolean or null.
    Scalar { text: String, quoted: bool },
    /// Nothing, or an alias left unresolved.
    Null,
}

impl Node {
    /// A scalar node.
    pub(crate) fn scalar(text: impl Into<String>, line: usize) -> Self {
        Self {
            value: Value::Scalar {
                text: text.into(),
                quoted: true,
            },
            line,
        }
    }

    /// The value under `key`, when a mapping.
    pub(crate) fn get(&self, key: &str) -> Option<&Node> {
        match &self.value {
            Value::Map(entries) => entries.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    /// The value at `path`, key after key.
    pub(crate) fn at(&self, path: &[&str]) -> Option<&Node> {
        path.iter().try_fold(self, |node, key| node.get(key))
    }

    /// The text of a scalar.
    pub(crate) fn str(&self) -> Option<&str> {
        match &self.value {
            Value::Scalar { text, .. } => Some(text),
            _ => None,
        }
    }

    /// The text of the scalar at `path`.
    pub(crate) fn str_at(&self, path: &[&str]) -> Option<&str> {
        self.at(path).and_then(Node::str)
    }

    /// The items of a sequence, none for anything else.
    pub(crate) fn items(&self) -> &[Node] {
        match &self.value {
            Value::Seq(items) => items,
            _ => &[],
        }
    }

    /// The scalars of a sequence of scalars at `path`.
    pub(crate) fn strs_at(&self, path: &[&str]) -> Vec<String> {
        self.at(path)
            .map(|n| {
                n.items()
                    .iter()
                    .filter_map(|i| i.str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The value as JSON: what a tool is given back, numbers and booleans
    /// as they were written unquoted.
    pub(crate) fn to_json(&self) -> serde_json::Value {
        match &self.value {
            Value::Map(entries) => serde_json::Value::Object(
                entries
                    .iter()
                    .map(|(k, v)| (k.clone(), v.to_json()))
                    .collect(),
            ),
            Value::Seq(items) => {
                serde_json::Value::Array(items.iter().map(Node::to_json).collect())
            }
            Value::Scalar { text, quoted: true } => serde_json::Value::String(text.clone()),
            Value::Scalar {
                text,
                quoted: false,
            } => plain(text),
            Value::Null => serde_json::Value::Null,
        }
    }

    /// Sets the value at `path`, making the mappings on the way.
    pub(crate) fn set(&mut self, path: &[String], value: Node) {
        let Some((first, rest)) = path.split_first() else {
            *self = value;
            return;
        };
        if let Value::Seq(items) = &mut self.value
            && let Ok(i) = first.parse::<usize>()
        {
            if let Some(item) = items.get_mut(i) {
                item.set(rest, value);
            }
            return;
        }
        if first == "-"
            && let Value::Seq(items) = &mut self.value
        {
            let mut node = Node {
                value: Value::Null,
                line: value.line,
            };
            node.set(rest, value);
            items.push(node);
            return;
        }
        if !matches!(self.value, Value::Map(_)) {
            self.value = Value::Map(Vec::new());
        }
        if let Value::Map(entries) = &mut self.value {
            let line = value.line;
            let at = match entries.iter().position(|(k, _)| k == first) {
                Some(at) => at,
                None => {
                    entries.push((
                        first.clone(),
                        Node {
                            value: Value::Null,
                            line,
                        },
                    ));
                    entries.len() - 1
                }
            };
            entries[at].1.set(rest, value);
        }
    }

    /// Takes away the value at `path`.
    pub(crate) fn remove(&mut self, path: &[String]) {
        let Some((last, parents)) = path.split_last() else {
            return;
        };
        let mut node = self;
        for key in parents {
            let next = match &mut node.value {
                Value::Map(entries) => entries.iter_mut().find(|(k, _)| k == key).map(|(_, v)| v),
                Value::Seq(items) => key.parse::<usize>().ok().and_then(|i| items.get_mut(i)),
                _ => None,
            };
            match next {
                Some(next) => node = next,
                None => return,
            }
        }
        match &mut node.value {
            Value::Map(entries) => entries.retain(|(k, _)| k != last),
            Value::Seq(items) => {
                if let Ok(i) = last.parse::<usize>()
                    && i < items.len()
                {
                    items.remove(i);
                }
            }
            _ => {}
        }
    }
}

/// An unquoted scalar as JSON: YAML 1.2's core schema.
fn plain(text: &str) -> serde_json::Value {
    match text {
        "" | "~" | "null" | "Null" | "NULL" => serde_json::Value::Null,
        "true" | "True" | "TRUE" => serde_json::Value::Bool(true),
        "false" | "False" | "FALSE" => serde_json::Value::Bool(false),
        _ => {
            if let Ok(i) = text.parse::<i64>() {
                return serde_json::Value::from(i);
            }
            if let Ok(f) = text.parse::<f64>()
                && f.is_finite()
                && text
                    .chars()
                    .all(|c| c.is_ascii_digit() || ".-+eE".contains(c))
            {
                return serde_json::Value::from(f);
            }
            serde_json::Value::String(text.to_owned())
        }
    }
}

/// Builds nodes from the parser's events.
#[derive(Default)]
struct Builder {
    /// What is open: a container, and for a mapping the key waiting for
    /// its value.
    stack: Vec<(Node, Option<String>)>,
    docs: Vec<Node>,
}

impl Builder {
    fn push(&mut self, node: Node) {
        match self.stack.last_mut() {
            None => self.docs.push(node),
            Some((parent, key)) => match &mut parent.value {
                Value::Seq(items) => items.push(node),
                Value::Map(entries) => match key.take() {
                    Some(k) => entries.push((k, node)),
                    None => {
                        *key = Some(match node.value {
                            Value::Scalar { text, .. } => text,
                            _ => String::new(),
                        });
                    }
                },
                _ => {}
            },
        }
    }
}

impl MarkedEventReceiver for Builder {
    fn on_event(&mut self, event: Event, mark: Marker) {
        let line = mark.line();
        match event {
            Event::Scalar(text, style, _, _) => self.push(Node {
                value: Value::Scalar {
                    text,
                    quoted: style != TScalarStyle::Plain,
                },
                line,
            }),
            Event::Alias(_) => self.push(Node {
                value: Value::Null,
                line,
            }),
            Event::SequenceStart(..) => self.stack.push((
                Node {
                    value: Value::Seq(Vec::new()),
                    line,
                },
                None,
            )),
            Event::MappingStart(..) => self.stack.push((
                Node {
                    value: Value::Map(Vec::new()),
                    line,
                },
                None,
            )),
            Event::SequenceEnd | Event::MappingEnd => {
                if let Some((node, _)) = self.stack.pop() {
                    self.push(node);
                }
            }
            _ => {}
        }
    }
}

/// The documents of `text`, none when it is not YAML.
pub(crate) fn docs(text: &str) -> Vec<Node> {
    let mut builder = Builder::default();
    let mut parser = Parser::new_from_str(text);
    if parser.load(&mut builder, true).is_err() {
        return Vec::new();
    }
    builder
        .docs
        .into_iter()
        .filter(|d| d.value != Value::Null)
        .collect()
}

/// The first document of `text`.
pub(crate) fn doc(text: &str) -> Option<Node> {
    docs(text).into_iter().next()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_value_keeps_its_line() {
        let text = "kind: HelmRelease\nspec:\n  values:\n    image:\n      tag: \"1.2\"\n    replicas: 3\n    hosts:\n      - a.example.com\n---\nkind: Other\n";
        let docs = docs(text);
        assert_eq!(docs.len(), 2);
        let release = &docs[0];
        assert_eq!(release.str_at(&["kind"]), Some("HelmRelease"));
        let tag = release.at(&["spec", "values", "image", "tag"]).unwrap();
        assert_eq!((tag.str(), tag.line), (Some("1.2"), 5));
        assert_eq!(release.at(&["spec", "values", "replicas"]).unwrap().line, 6);
        assert_eq!(
            release.strs_at(&["spec", "values", "hosts"]),
            ["a.example.com"]
        );
        let json = release.at(&["spec", "values"]).unwrap().to_json();
        assert_eq!(json["replicas"], serde_json::json!(3));
        assert_eq!(json["image"]["tag"], serde_json::json!("1.2"));
        assert!(super::docs("a: [").is_empty());
    }

    #[test]
    fn set_and_remove_by_path() {
        let mut node = doc("spec:\n  path: ./a\n  list:\n    - x\n").unwrap();
        let path = |p: &str| p.split('/').map(str::to_owned).collect::<Vec<_>>();
        node.set(&path("spec/path"), Node::scalar("./b", 9));
        node.set(&path("spec/new/deep"), Node::scalar("v", 9));
        node.set(&path("spec/list/-"), Node::scalar("y", 9));
        node.remove(&path("spec/list/0"));
        assert_eq!(node.str_at(&["spec", "path"]), Some("./b"));
        assert_eq!(node.str_at(&["spec", "new", "deep"]), Some("v"));
        assert_eq!(node.strs_at(&["spec", "list"]), ["y"]);
    }
}
