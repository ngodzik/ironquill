//! Language servers, asked where a name is defined and where it is used:
//! the programs editors ask (pyright for Python, typescript-language-server
//! for TypeScript and JavaScript, rust-analyzer for Rust), spoken to over
//! their standard input and output in the Language Server Protocol.
//!
//! A server is started the first time a file of its language is asked
//! about, and kept: it reads the project once, then answers at once. It
//! is asked from a thread of the caller's choosing, and waited on for a
//! while at most; a server missing, failing or slow is said so, for the
//! caller to fall back on reading the code by patterns.

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};

/// How long a server is waited on for an answer: the first one may take a
/// project's reading; the next ones come at once.
const WAIT: Duration = Duration::from_secs(45);

/// What went wrong asking a server.
#[derive(Debug, thiserror::Error)]
pub enum LspError {
    /// No server is known for the file's language.
    #[error("no language server for {0}")]
    NoServer(String),
    /// The server's program could not be started: it is not installed.
    #[error("could not start {program}: {source}")]
    Start {
        /// The program.
        program: String,
        /// Why.
        #[source]
        source: std::io::Error,
    },
    /// The server could not be written to: it stopped.
    #[error("{program} stopped: {source}")]
    Stopped {
        /// The program.
        program: String,
        /// Why writing failed.
        #[source]
        source: std::io::Error,
    },
    /// The server did not answer in time.
    #[error("{0} did not answer in time")]
    Timeout(String),
    /// The server answered with an error.
    #[error("{program} answered: {message}")]
    Answered {
        /// The program.
        program: String,
        /// What it said.
        message: String,
    },
}

/// A place in the project a server pointed at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Location {
    /// The file, from the project's root; as given when outside it.
    pub path: String,
    /// The line, from 1.
    pub line: usize,
}

/// The languages a server is known for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Language {
    Python,
    TypeScript,
    Rust,
}

impl Language {
    fn of(path: &Path) -> Option<Self> {
        match path.extension()?.to_str()? {
            "py" | "pyi" => Some(Self::Python),
            "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" => Some(Self::TypeScript),
            "rs" => Some(Self::Rust),
            _ => None,
        }
    }

    /// The program, and its arguments.
    fn program(self) -> (&'static str, &'static [&'static str]) {
        match self {
            Self::Python => ("pyright-langserver", &["--stdio"]),
            Self::TypeScript => ("typescript-language-server", &["--stdio"]),
            Self::Rust => ("rust-analyzer", &[]),
        }
    }

    /// The protocol's name for a file's language.
    fn id(path: &Path) -> &'static str {
        match path.extension().and_then(|e| e.to_str()) {
            Some("py" | "pyi") => "python",
            Some("ts") => "typescript",
            Some("tsx") => "typescriptreact",
            Some("jsx") => "javascriptreact",
            Some("rs") => "rust",
            _ => "javascript",
        }
    }
}

/// The language servers of a project, each started when first needed.
pub struct Servers {
    root: PathBuf,
    running: Mutex<HashMap<Language, Arc<Server>>>,
}

impl std::fmt::Debug for Servers {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Servers")
            .field("root", &self.root)
            .finish_non_exhaustive()
    }
}

impl Servers {
    /// The servers of the project at `root`, none started yet.
    #[must_use]
    pub fn new(root: &Path) -> Self {
        Self {
            root: root.to_owned(),
            running: Mutex::new(HashMap::new()),
        }
    }

    /// The name of the program that answers for `path`'s language, if one
    /// is known.
    #[must_use]
    pub fn program_for(path: &Path) -> Option<&'static str> {
        Language::of(path).map(|l| l.program().0)
    }

    /// Starts the server for `path`'s language, if one is known, so that
    /// it reads the project before it is asked anything.
    ///
    /// # Errors
    ///
    /// When it cannot be started.
    pub fn warm(&self, path: &Path) -> Result<(), LspError> {
        match Language::of(path) {
            Some(language) => self.server(language).map(|_| ()),
            None => Ok(()),
        }
    }

    /// Where the name at `line` and `column` (from 0, in characters) of
    /// `path` (from the root), whose text is `text`, is defined.
    ///
    /// # Errors
    ///
    /// When no server answers for the file, or it fails or is slow.
    pub fn definition(
        &self,
        path: &Path,
        text: &str,
        line: usize,
        column: usize,
    ) -> Result<Vec<Location>, LspError> {
        self.ask(
            path,
            text,
            line,
            column,
            "textDocument/definition",
            json!({}),
        )
    }

    /// Where the name at `line` and `column` of `path` is used, its
    /// definition left out.
    ///
    /// # Errors
    ///
    /// As [`Servers::definition`].
    pub fn references(
        &self,
        path: &Path,
        text: &str,
        line: usize,
        column: usize,
    ) -> Result<Vec<Location>, LspError> {
        // Every use is known once the project is read, not before.
        if let Some(language) = Language::of(path) {
            self.server(language)?.wait_ready(WAIT);
        }
        self.ask(
            path,
            text,
            line,
            column,
            "textDocument/references",
            json!({ "context": { "includeDeclaration": false } }),
        )
    }

    fn ask(
        &self,
        path: &Path,
        text: &str,
        line: usize,
        column: usize,
        method: &str,
        extra: Value,
    ) -> Result<Vec<Location>, LspError> {
        let language =
            Language::of(path).ok_or_else(|| LspError::NoServer(path.display().to_string()))?;
        let server = self.server(language)?;
        let uri = file_uri(&self.root.join(path));
        server.open(&uri, Language::id(path), text)?;
        let character = utf16_column(text.lines().nth(line).unwrap_or(""), column);
        let mut params = json!({
            "textDocument": { "uri": uri },
            "position": { "line": line, "character": character },
        });
        if let (Some(params), Some(extra)) = (params.as_object_mut(), extra.as_object()) {
            params.extend(extra.clone());
        }
        let answer = server.request(method, params)?;
        Ok(locations(&answer, &self.root))
    }

    /// The server for `language`, started if it is not running.
    fn server(&self, language: Language) -> Result<Arc<Server>, LspError> {
        let mut running = self
            .running
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(server) = running.get(&language).filter(|s| s.alive()) {
            return Ok(Arc::clone(server));
        }
        let server = Arc::new(Server::start(language, &self.root)?);
        running.insert(language, Arc::clone(&server));
        Ok(server)
    }
}

/// Answers waited for, by request id.
type Waiting = Arc<Mutex<HashMap<i64, Sender<Result<Value, String>>>>>;

/// Whether a server has read the project, and a way to wait for it.
type Ready = Arc<(Mutex<bool>, std::sync::Condvar)>;

/// Marks a server ready, waking who waits.
fn set_ready(ready: &Ready) {
    let (flag, woken) = &**ready;
    *flag
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = true;
    woken.notify_all();
}

/// One running server.
struct Server {
    program: &'static str,
    child: Mutex<Child>,
    /// Written by the caller, and by the reading thread to answer the
    /// server's own requests.
    input: Arc<Mutex<ChildStdin>>,
    waiting: Waiting,
    next: Mutex<i64>,
    /// The documents opened, with the text they were opened with.
    opened: Mutex<HashMap<String, (i64, String)>>,
    /// Whether it has read the whole project: before, it knows the
    /// definitions it reaches from an open file, not every use.
    ready: Ready,
}

impl Server {
    /// Starts `language`'s server on the project at `root`, and greets it.
    fn start(language: Language, root: &Path) -> Result<Self, LspError> {
        let (program, args) = language.program();
        let mut child = Command::new(program)
            .args(args)
            .current_dir(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|source| LspError::Start {
                program: program.to_owned(),
                source,
            })?;
        let (Some(input), Some(output)) = (child.stdin.take(), child.stdout.take()) else {
            let _ = child.kill();
            return Err(LspError::Start {
                program: program.to_owned(),
                source: std::io::Error::other("no standard input or output"),
            });
        };
        let waiting: Waiting = Arc::new(Mutex::new(HashMap::new()));
        let input = Arc::new(Mutex::new(input));
        let answering = Arc::clone(&input);
        // TypeScript's server reads a project as a file of it opens; the
        // others say when they are done.
        let ready: Ready = Arc::new((
            Mutex::new(language == Language::TypeScript),
            std::sync::Condvar::new(),
        ));
        let readying = Arc::clone(&ready);
        let server = Self {
            program,
            child: Mutex::new(child),
            input,
            waiting: Arc::clone(&waiting),
            next: Mutex::new(0),
            opened: Mutex::new(HashMap::new()),
            ready,
        };
        // What the server sends: answers to us, and its own requests,
        // which are answered at once with nothing, as an editor that
        // offers nothing more would.
        std::thread::spawn(move || {
            let mut reader = BufReader::new(output);
            // The progress reported, begun and not ended, by token.
            let mut working: HashSet<String> = HashSet::new();
            while let Some(message) = read_message(&mut reader) {
                let id = message.get("id").cloned();
                match message.get("method").and_then(Value::as_str) {
                    // pyright says how many files it found, once it has.
                    Some("window/logMessage")
                        if message["params"]["message"].as_str().is_some_and(|m| {
                            m.starts_with("Found ") && m.contains("source file")
                        }) =>
                    {
                        set_ready(&readying);
                    }
                    // rust-analyzer reports its indexing as progress.
                    Some("$/progress") => {
                        let token = message["params"]["token"].to_string();
                        match message["params"]["value"]["kind"].as_str() {
                            Some("begin") => {
                                working.insert(token);
                            }
                            Some("end") => {
                                working.remove(&token);
                                if working.is_empty() {
                                    set_ready(&readying);
                                }
                            }
                            _ => {}
                        }
                    }
                    _ => {}
                }
                if let Some(method) = message.get("method").and_then(Value::as_str) {
                    if let Some(id) = id {
                        // Settings are asked for one per section: none set,
                        // each as the server's default.
                        let result = if method == "workspace/configuration" {
                            let asked = message["params"]["items"].as_array().map_or(0, Vec::len);
                            Value::Array(vec![Value::Null; asked])
                        } else {
                            Value::Null
                        };
                        let answer = json!({ "jsonrpc": "2.0", "id": id, "result": result });
                        let mut input = answering
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        let _ = send(&mut *input, &answer);
                    }
                    continue;
                }
                let Some(id) = id.and_then(|i| i.as_i64()) else {
                    continue;
                };
                let answer = match message.get("error") {
                    Some(error) => Err(error
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("an error")
                        .to_owned()),
                    None => Ok(message.get("result").cloned().unwrap_or(Value::Null)),
                };
                let sender = waiting
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .remove(&id);
                if let Some(sender) = sender {
                    let _ = sender.send(answer);
                }
            }
        });
        let root_uri = file_uri(root);
        // TypeScript's server needs TypeScript: the project's, or else the
        // one installed with `tsc`.
        let options = match (language, global_tsserver()) {
            (Language::TypeScript, Some(tsserver))
                if !root.join("node_modules/typescript").is_dir() =>
            {
                json!({ "tsserver": { "path": tsserver } })
            }
            _ => json!({}),
        };
        server.request(
            "initialize",
            json!({
                "initializationOptions": options,
                "processId": std::process::id(),
                "rootUri": root_uri,
                "workspaceFolders": [{ "uri": root_uri, "name": "project" }],
                "capabilities": {
                    "textDocument": {
                        "definition": { "linkSupport": true },
                        "references": {},
                        "synchronization": { "didSave": false },
                    },
                    "workspace": { "workspaceFolders": true, "configuration": true },
                    "window": { "workDoneProgress": true },
                },
            }),
        )?;
        server.notify("initialized", json!({}))?;
        Ok(server)
    }

    /// Waits until the server has read the project, `limit` at most.
    fn wait_ready(&self, limit: Duration) {
        let (flag, woken) = &*self.ready;
        let ready = flag
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _ = woken
            .wait_timeout_while(ready, limit, |ready| !*ready)
            .unwrap_or_else(std::sync::PoisonError::into_inner);
    }

    /// Whether the server still runs.
    fn alive(&self) -> bool {
        self.child
            .lock()
            .map(|mut c| matches!(c.try_wait(), Ok(None)))
            .unwrap_or(false)
    }

    fn write(&self, message: &Value) -> Result<(), LspError> {
        let mut input = self
            .input
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        send(&mut *input, message).map_err(|source| LspError::Stopped {
            program: self.program.to_owned(),
            source,
        })
    }

    fn notify(&self, method: &str, params: Value) -> Result<(), LspError> {
        self.write(&json!({ "jsonrpc": "2.0", "method": method, "params": params }))
    }

    fn request(&self, method: &str, params: Value) -> Result<Value, LspError> {
        let id = {
            let mut next = self
                .next
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            *next += 1;
            *next
        };
        let (send, receive) = mpsc::channel();
        self.waiting
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(id, send);
        self.write(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))?;
        match receive.recv_timeout(WAIT) {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(message)) => Err(LspError::Answered {
                program: self.program.to_owned(),
                message,
            }),
            Err(_) => {
                self.waiting
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .remove(&id);
                Err(LspError::Timeout(self.program.to_owned()))
            }
        }
    }

    /// Tells the server what `uri` holds: opened the first time, changed
    /// when its text differs from what it was told.
    fn open(&self, uri: &str, language: &str, text: &str) -> Result<(), LspError> {
        let mut opened = self
            .opened
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match opened.get_mut(uri) {
            Some((_, known)) if known == text => Ok(()),
            Some((version, known)) => {
                *version += 1;
                known.clear();
                known.push_str(text);
                let version = *version;
                drop(opened);
                self.notify(
                    "textDocument/didChange",
                    json!({
                        "textDocument": { "uri": uri, "version": version },
                        "contentChanges": [{ "text": text }],
                    }),
                )
            }
            None => {
                opened.insert(uri.to_owned(), (1, text.to_owned()));
                drop(opened);
                self.notify(
                    "textDocument/didOpen",
                    json!({
                        "textDocument": {
                            "uri": uri,
                            "languageId": language,
                            "version": 1,
                            "text": text,
                        },
                    }),
                )
            }
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        // Asked to stop, then stopped: a server must not outlive us.
        let _ = self.write(&json!({ "jsonrpc": "2.0", "method": "exit" }));
        if let Ok(mut child) = self.child.lock() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// TypeScript's server as installed with `tsc`, found on the path:
/// `…/typescript/bin/tsc` next to `…/typescript/lib/tsserver.js`.
fn global_tsserver() -> Option<String> {
    let paths = std::env::var_os("PATH")?;
    let tsc = std::env::split_paths(&paths)
        .map(|dir| dir.join("tsc"))
        .find(|candidate| candidate.is_file())?;
    let real = std::fs::canonicalize(tsc).ok()?;
    let server = real.parent()?.parent()?.join("lib/tsserver.js");
    server
        .is_file()
        .then(|| server.to_string_lossy().into_owned())
}

/// Writes one message, its length first.
fn send(output: &mut impl Write, message: &Value) -> std::io::Result<()> {
    let body = message.to_string();
    write!(output, "Content-Length: {}\r\n\r\n{body}", body.len())?;
    output.flush()
}

/// Reads one message: its headers, then as many bytes as they say. `None`
/// when the server closed its output.
fn read_message(reader: &mut impl BufRead) -> Option<Value> {
    let mut length = None;
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header).ok()? == 0 {
            return None;
        }
        let header = header.trim_end();
        if header.is_empty() {
            break;
        }
        if let Some(value) = header
            .split_once(':')
            .filter(|(name, _)| name.eq_ignore_ascii_case("content-length"))
            .map(|(_, v)| v.trim())
        {
            length = value.parse::<usize>().ok();
        }
    }
    let mut body = vec![0; length?];
    reader.read_exact(&mut body).ok()?;
    serde_json::from_slice(&body).ok()
}

/// The places an answer names: a location, a list of them, or of links,
/// the files relative to `root` when inside it, each line once.
fn locations(answer: &Value, root: &Path) -> Vec<Location> {
    let list: Vec<&Value> = match answer {
        Value::Array(items) => items.iter().collect(),
        Value::Null => Vec::new(),
        one => vec![one],
    };
    let mut seen = HashSet::new();
    list.into_iter()
        .filter_map(|item| {
            let uri = item
                .get("targetUri")
                .or_else(|| item.get("uri"))?
                .as_str()?;
            let range = item
                .get("targetSelectionRange")
                .or_else(|| item.get("range"))?;
            let line = usize::try_from(range["start"]["line"].as_u64()?).ok()? + 1;
            let path = uri_path(uri)?;
            let path = path
                .strip_prefix(root)
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|_| path.to_string_lossy().into_owned());
            seen.insert((path.clone(), line))
                .then_some(Location { path, line })
        })
        .collect()
}

/// A path as a `file://` URI, the characters a URI reserves escaped.
fn file_uri(path: &Path) -> String {
    let mut uri = String::from("file://");
    for byte in path.to_string_lossy().bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'_' | b'.' | b'~' => {
                uri.push(char::from(byte));
            }
            other => uri.push_str(&format!("%{other:02X}")),
        }
    }
    uri
}

/// The path a `file://` URI names, its escapes read.
fn uri_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let bytes = rest.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(hex) = rest.get(i + 1..i + 3)
            && let Ok(byte) = u8::from_str_radix(hex, 16)
        {
            out.push(byte);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    Some(PathBuf::from(String::from_utf8(out).ok()?))
}

/// A column in characters as the protocol counts it: in UTF-16 units.
fn utf16_column(line: &str, column: usize) -> usize {
    line.chars().take(column).map(char::len_utf16).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_are_read_by_their_length_header() {
        let body = r#"{"jsonrpc":"2.0","id":1,"result":null}"#;
        let text = format!(
            "Content-Length: {}\r\nContent-Type: application/vscode-jsonrpc\r\n\r\n{body}Content-Length: 2\r\n\r\n{{}}",
            body.len()
        );
        let mut reader = BufReader::new(text.as_bytes());
        assert_eq!(read_message(&mut reader).unwrap()["id"], 1);
        assert_eq!(read_message(&mut reader).unwrap(), json!({}));
        assert!(read_message(&mut reader).is_none());
    }

    #[test]
    fn locations_and_links_become_lines_of_the_project() {
        let root = Path::new("/home/me/my project");
        // A list of locations, as pyright answers; one repeated.
        let answer = json!([
            { "uri": "file:///home/me/my%20project/a/b.py",
              "range": { "start": { "line": 9, "character": 4 }, "end": { "line": 9, "character": 8 } } },
            { "uri": "file:///home/me/my%20project/a/b.py",
              "range": { "start": { "line": 9, "character": 0 }, "end": { "line": 9, "character": 2 } } },
            { "uri": "file:///usr/lib/python3/typing.py",
              "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 1 } } },
        ]);
        assert_eq!(
            locations(&answer, root),
            [
                Location {
                    path: "a/b.py".into(),
                    line: 10
                },
                Location {
                    path: "/usr/lib/python3/typing.py".into(),
                    line: 1
                },
            ]
        );
        // A link, as typescript-language-server answers with linkSupport.
        let answer = json!([{
            "targetUri": "file:///home/me/my%20project/ui/api.ts",
            "targetRange": { "start": { "line": 2, "character": 0 }, "end": { "line": 8, "character": 1 } },
            "targetSelectionRange": { "start": { "line": 3, "character": 13 }, "end": { "line": 3, "character": 20 } },
        }]);
        assert_eq!(
            locations(&answer, root),
            [Location {
                path: "ui/api.ts".into(),
                line: 4
            }]
        );
        // One location alone, and nothing.
        let answer = json!({ "uri": "file:///home/me/my%20project/x.rs",
            "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 0 } } });
        assert_eq!(locations(&answer, root).len(), 1);
        assert!(locations(&Value::Null, root).is_empty());
    }

    #[test]
    fn uris_escape_and_unescape_and_columns_count_in_utf16() {
        let path = Path::new("/a b/é#.py");
        assert_eq!(file_uri(path), "file:///a%20b/%C3%A9%23.py");
        assert_eq!(uri_path(&file_uri(path)).unwrap(), path);
        assert_eq!(utf16_column("x = '😀'; y", 9), 10);
        assert_eq!(
            Servers::program_for(Path::new("a.tsx")),
            Some("typescript-language-server")
        );
        assert_eq!(Servers::program_for(Path::new("a.md")), None);
    }
}
