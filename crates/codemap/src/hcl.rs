//! A small reader of HCL, the language Terraform is written in: blocks,
//! attributes, objects, lists, strings, heredocs and comments. What is
//! computed (references, calls, conditions) is kept as its text, to be
//! matched or resolved later, never evaluated.

use serde::{Deserialize, Serialize};

/// A value as written.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum Hcl {
    /// A string, its `${…}` kept as written.
    Str(String),
    /// A number, as written.
    Number(String),
    /// `true` or `false`.
    Bool(bool),
    /// `null`.
    Null,
    /// A list or a tuple.
    List(Vec<Hcl>),
    /// An object, its keys in their order.
    Object(Vec<(String, Hcl)>),
    /// Anything computed, as written: `var.name`, `lookup(…)`, `a ? b : c`.
    Expr(String),
}

impl Hcl {
    /// The value under `key`, when an object.
    pub(crate) fn get(&self, key: &str) -> Option<&Hcl> {
        match self {
            Hcl::Object(entries) => entries.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    /// The text of a string, a number or an expression.
    pub(crate) fn text(&self) -> Option<&str> {
        match self {
            Hcl::Str(s) | Hcl::Number(s) | Hcl::Expr(s) => Some(s),
            _ => None,
        }
    }

    /// How the value reads, in one line.
    pub(crate) fn show(&self) -> String {
        match self {
            Hcl::Str(s) => s.clone(),
            Hcl::Number(s) | Hcl::Expr(s) => s.clone(),
            Hcl::Bool(b) => b.to_string(),
            Hcl::Null => "null".to_owned(),
            Hcl::List(items) => format!(
                "[{}]",
                items.iter().map(Hcl::show).collect::<Vec<_>>().join(", ")
            ),
            Hcl::Object(entries) => format!(
                "{{{}}}",
                entries
                    .iter()
                    .map(|(k, v)| format!("{k} = {}", v.show()))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }
    }
}

/// An attribute: `name = value`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Attribute {
    pub(crate) name: String,
    pub(crate) value: Hcl,
    /// Its line, from 1.
    pub(crate) line: usize,
}

/// A block: `kind "label" "label" { body }`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Block {
    pub(crate) kind: String,
    pub(crate) labels: Vec<String>,
    pub(crate) body: Body,
    /// Its line, from 1.
    pub(crate) line: usize,
}

/// What a file or a block holds.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Body {
    pub(crate) attributes: Vec<Attribute>,
    pub(crate) blocks: Vec<Block>,
}

impl Body {
    /// The attribute `name`'s value.
    pub(crate) fn get(&self, name: &str) -> Option<&Hcl> {
        self.attributes
            .iter()
            .find(|a| a.name == name)
            .map(|a| &a.value)
    }

    /// The text of the attribute `name`.
    pub(crate) fn text(&self, name: &str) -> Option<&str> {
        self.get(name).and_then(Hcl::text)
    }
}

/// Reads `text`. What cannot be read is skipped, a line at a time.
pub(crate) fn parse(text: &str) -> Body {
    let mut reader = Reader {
        chars: text.chars().collect(),
        at: 0,
        line: 1,
    };
    reader.body(false)
}

struct Reader {
    chars: Vec<char>,
    at: usize,
    line: usize,
}

impl Reader {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.at).copied()
    }

    fn peek_at(&self, ahead: usize) -> Option<char> {
        self.chars.get(self.at + ahead).copied()
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.at += 1;
        if c == '\n' {
            self.line += 1;
        }
        Some(c)
    }

    fn starts(&self, s: &str) -> bool {
        s.chars()
            .enumerate()
            .all(|(i, c)| self.peek_at(i) == Some(c))
    }

    /// Skips spaces and comments; newlines too when `lines`.
    fn skip(&mut self, lines: bool) {
        loop {
            match self.peek() {
                Some(' ' | '\t' | '\r') => {
                    self.bump();
                }
                Some('\n') if lines => {
                    self.bump();
                }
                Some('#') => self.skip_line(),
                Some('/') if self.peek_at(1) == Some('/') => self.skip_line(),
                Some('/') if self.peek_at(1) == Some('*') => {
                    self.bump();
                    self.bump();
                    while self.peek().is_some() && !self.starts("*/") {
                        self.bump();
                    }
                    self.bump();
                    self.bump();
                }
                _ => return,
            }
        }
    }

    fn skip_line(&mut self) {
        while self.peek().is_some_and(|c| c != '\n') {
            self.bump();
        }
    }

    fn identifier(&mut self) -> Option<String> {
        let c = self.peek()?;
        if !(c.is_alphabetic() || c == '_') {
            return None;
        }
        let mut out = String::new();
        while let Some(c) = self.peek() {
            if c.is_alphanumeric() || c == '_' || c == '-' {
                out.push(c);
                self.bump();
            } else {
                break;
            }
        }
        Some(out)
    }

    /// A body, up to its closing brace when `nested`.
    fn body(&mut self, nested: bool) -> Body {
        let mut body = Body::default();
        loop {
            self.skip(true);
            match self.peek() {
                None => return body,
                Some('}') if nested => {
                    self.bump();
                    return body;
                }
                _ => {}
            }
            let line = self.line;
            let Some(name) = self.identifier() else {
                // Not something this reader knows: skip the line.
                self.bump();
                self.skip_line();
                continue;
            };
            self.skip(false);
            if self.peek() == Some('=') && self.peek_at(1) != Some('=') {
                self.bump();
                self.skip(false);
                let value = self.expression(&['\n']);
                body.attributes.push(Attribute { name, value, line });
                continue;
            }
            let mut labels = Vec::new();
            loop {
                self.skip(false);
                match self.peek() {
                    Some('"') => labels.push(self.string()),
                    Some('{') => {
                        self.bump();
                        let inner = self.body(true);
                        body.blocks.push(Block {
                            kind: name,
                            labels,
                            body: inner,
                            line,
                        });
                        break;
                    }
                    Some(c) if c.is_alphabetic() || c == '_' => {
                        if let Some(label) = self.identifier() {
                            labels.push(label);
                        }
                    }
                    _ => {
                        self.skip_line();
                        break;
                    }
                }
            }
        }
    }

    /// A quoted string's text, escapes read, `${…}` kept.
    fn string(&mut self) -> String {
        self.bump();
        let mut out = String::new();
        let mut depth = 0usize;
        while let Some(c) = self.bump() {
            match c {
                '\\' if depth == 0 => match self.bump() {
                    Some('n') => out.push('\n'),
                    Some('t') => out.push('\t'),
                    Some(other) => out.push(other),
                    None => {}
                },
                '"' if depth == 0 => return out,
                '$' if self.peek() == Some('{') => {
                    out.push('$');
                    out.push('{');
                    self.bump();
                    depth += 1;
                }
                '{' if depth > 0 => {
                    out.push(c);
                    depth += 1;
                }
                '}' if depth > 0 => {
                    out.push(c);
                    depth -= 1;
                }
                '"' => {
                    // A string inside an interpolation.
                    out.push(c);
                    while let Some(inner) = self.bump() {
                        out.push(inner);
                        if inner == '"' {
                            break;
                        }
                    }
                }
                '\n' if depth == 0 => return out,
                _ => out.push(c),
            }
        }
        out
    }

    /// A heredoc's text, `<<EOT` or `<<-EOT` read.
    fn heredoc(&mut self) -> String {
        self.bump();
        self.bump();
        let indented = self.peek() == Some('-');
        if indented {
            self.bump();
        }
        let marker = self.identifier().unwrap_or_default();
        self.skip_line();
        self.bump();
        let mut lines = Vec::new();
        loop {
            let mut line = String::new();
            while let Some(c) = self.peek() {
                if c == '\n' {
                    break;
                }
                line.push(c);
                self.bump();
            }
            let done = line.trim() == marker || self.peek().is_none();
            if !done {
                lines.push(line);
                self.bump();
                continue;
            }
            break;
        }
        if indented {
            let indent = lines
                .iter()
                .filter(|l| !l.trim().is_empty())
                .map(|l| l.len() - l.trim_start().len())
                .min()
                .unwrap_or(0);
            lines = lines
                .into_iter()
                .map(|l| l.chars().skip(indent).collect())
                .collect();
        }
        lines.join("\n")
    }

    /// Whether the next character ends an expression.
    fn ends(&self, stops: &[char]) -> bool {
        match self.peek() {
            None => true,
            Some('#') => true,
            Some('/') => matches!(self.peek_at(1), Some('/' | '*')),
            Some(c) => stops.contains(&c),
        }
    }

    /// An expression up to one of `stops` at its own depth.
    fn expression(&mut self, stops: &[char]) -> Hcl {
        let (start, line) = (self.at, self.line);
        let value = match self.peek() {
            Some('"') => Some(Hcl::Str(self.string())),
            Some('<') if self.peek_at(1) == Some('<') => Some(Hcl::Str(self.heredoc())),
            Some('[') => Some(self.list()),
            Some('{') => Some(self.object()),
            Some(c) if c.is_ascii_digit() || c == '-' => {
                let mut number = String::new();
                while let Some(c) = self.peek() {
                    if c.is_ascii_alphanumeric() || c == '.' || (number.is_empty() && c == '-') {
                        number.push(c);
                        self.bump();
                    } else {
                        break;
                    }
                }
                number.parse::<f64>().ok().map(|_| Hcl::Number(number))
            }
            _ => match self.identifier().as_deref() {
                Some("true") => Some(Hcl::Bool(true)),
                Some("false") => Some(Hcl::Bool(false)),
                Some("null") => Some(Hcl::Null),
                _ => None,
            },
        };
        self.skip(false);
        if let Some(value) = value
            && self.ends(stops)
        {
            return value;
        }
        // Computed: read it again as text.
        self.at = start;
        self.line = line;
        Hcl::Expr(self.raw(stops))
    }

    /// The text of an expression up to one of `stops` at depth zero.
    fn raw(&mut self, stops: &[char]) -> String {
        let mut out = String::new();
        let mut depth = 0usize;
        while let Some(c) = self.peek() {
            if depth == 0 && self.ends(stops) {
                break;
            }
            match c {
                '"' => {
                    let s = self.string();
                    out.push('"');
                    out.push_str(&s);
                    out.push('"');
                    continue;
                }
                '(' | '[' | '{' => depth += 1,
                ')' | ']' | '}' => depth = depth.saturating_sub(1),
                _ => {}
            }
            out.push(c);
            self.bump();
        }
        out.trim().to_owned()
    }

    fn list(&mut self) -> Hcl {
        self.bump();
        let mut items = Vec::new();
        loop {
            self.skip(true);
            match self.peek() {
                None => break,
                Some(']') => {
                    self.bump();
                    break;
                }
                Some(',') => {
                    self.bump();
                }
                _ => {
                    let before = self.at;
                    items.push(self.expression(&[',', ']', '\n']));
                    if self.at == before {
                        self.bump();
                    }
                }
            }
        }
        Hcl::List(items)
    }

    fn object(&mut self) -> Hcl {
        self.bump();
        let mut entries = Vec::new();
        loop {
            self.skip(true);
            let key = match self.peek() {
                None => break,
                Some('}') => {
                    self.bump();
                    break;
                }
                Some(',') => {
                    self.bump();
                    continue;
                }
                Some('"') => self.string(),
                Some('(') => self.raw(&['=', ':']),
                _ => match self.identifier() {
                    Some(k) => k,
                    None => {
                        self.bump();
                        continue;
                    }
                },
            };
            self.skip(false);
            if matches!(self.peek(), Some('=' | ':')) {
                self.bump();
                self.skip(false);
                let value = self.expression(&[',', '}', '\n']);
                entries.push((key, value));
            }
        }
        Hcl::Object(entries)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_attributes_and_values() {
        let text = r#"
# A comment
module "db" {
  source  = "git::https://example.com/org/modules.git//rds?ref=v1.2.0"
  name    = "${var.stage}-shop" // trailing
  engine  = "postgres"
  count   = 2
  enabled = true
  tags    = { team = "core", "cost-centre" : "42" }
  subnets = [ "a", "b",
    "c" ]
  size    = var.stage == "prod" ? "large" : "small"
  policy  = <<-EOT
    {
      "Version": "2012"
    }
  EOT
  /* block
     comment */
  settings {
    parameter = lookup(var.map, "x", "y")
  }
}

resource "aws_s3_bucket" "files" {
  bucket = "shop-files"
}
"#;
        let body = parse(text);
        assert_eq!(body.blocks.len(), 2);
        let db = &body.blocks[0];
        assert_eq!(
            (db.kind.as_str(), db.labels.as_slice(), db.line),
            ("module", &["db".to_owned()][..], 3)
        );
        assert_eq!(db.body.text("name"), Some("${var.stage}-shop"));
        assert_eq!(db.body.get("count"), Some(&Hcl::Number("2".into())));
        assert_eq!(db.body.get("enabled"), Some(&Hcl::Bool(true)));
        assert_eq!(
            db.body.get("tags").and_then(|t| t.get("cost-centre")),
            Some(&Hcl::Str("42".into()))
        );
        assert_eq!(
            db.body.get("subnets"),
            Some(&Hcl::List(vec![
                Hcl::Str("a".into()),
                Hcl::Str("b".into()),
                Hcl::Str("c".into())
            ]))
        );
        assert_eq!(
            db.body.get("size"),
            Some(&Hcl::Expr(
                "var.stage == \"prod\" ? \"large\" : \"small\"".into()
            ))
        );
        assert_eq!(
            db.body.text("policy"),
            Some("{\n  \"Version\": \"2012\"\n}")
        );
        assert_eq!(
            db.body
                .attributes
                .iter()
                .find(|a| a.name == "size")
                .unwrap()
                .line,
            12
        );
        assert_eq!(
            db.body.blocks[0].body.text("parameter"),
            Some("lookup(var.map, \"x\", \"y\")")
        );
        let bucket = &body.blocks[1];
        assert_eq!(bucket.labels, ["aws_s3_bucket", "files"]);
        assert_eq!(bucket.line, 25);
        assert_eq!(bucket.body.text("bucket"), Some("shop-files"));
    }
}
