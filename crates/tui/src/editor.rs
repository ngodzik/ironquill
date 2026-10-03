//! A small Vim-like editor for the file opened from the tree.
//!
//! Its keys live here rather than in [`keymap`](crate::keymap): inside the
//! file they follow Vim, which has its own modes on top of the interface's.
//! Covered: moving, inserting, visual mode (`v`, `V`), deleting, yanking and
//! putting with registers (`"a`, and `"+` for the system clipboard), undo and
//! redo, `:w`, `:q`, `:42`, `:s` with ranges, and `/` search. Not covered:
//! counts, block visual mode, macros.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use regex::RegexBuilder;

use ironquill_tools::{LineChanges, committed_lines, line_changes};

use crate::clipboard;
use crate::highlight::{Highlighter, StyledLine};

/// Spaces typed by the Tab key in insert mode, and added by `>`. Spaces
/// rather than a tab character, as most Python and Rust code wants.
const TAB: &str = "    ";

/// Above this many lines, colours are recomputed when insert mode ends
/// rather than after every key, so that typing stays instant.
const LIVE_HIGHLIGHT_LINES: usize = 3_000;

/// First and last row of a line range, both included.
type Rows = (usize, usize);

/// The register every yank and delete also lands in, as in Vim.
const UNNAMED: char = '"';

/// The editor's own mode, as in Vim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EditorMode {
    Normal,
    Insert,
    /// `v` selects characters, `V` whole lines.
    Visual {
        line: bool,
    },
    /// Typing a `:` command in the frame under the file.
    Command,
    /// Typing a `/` search in the frame under the file.
    Search,
}

/// What the interface should do after a key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// `:w` on the conversation's context: apply this text to it, and close
    /// the editor too for `:wq`.
    Context {
        text: String,
        close: bool,
    },
    Stay,
    /// `:w`: the file was written, so its git status changed.
    Saved,
    /// `:q`: close the file.
    Close,
}

/// Where the system clipboard is reached. A trait so that tests do not
/// touch the real one.
pub(crate) trait Clipboard {
    fn copy(&self, text: &str) -> Result<&'static str, String>;
    fn paste(&self) -> Result<String, String>;
}

/// The clipboard of the machine ironquill runs on.
pub(crate) struct SystemClipboard;

impl Clipboard for SystemClipboard {
    fn copy(&self, text: &str) -> Result<&'static str, String> {
        clipboard::copy(text)
    }

    fn paste(&self) -> Result<String, String> {
        clipboard::paste()
    }
}

/// Text held by a register.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Register {
    lines: Vec<String>,
    /// Whole lines, put above or below; otherwise put inside a line.
    linewise: bool,
}

impl Register {
    fn to_text(&self) -> String {
        let mut text = self.lines.join("\n");
        if self.linewise {
            text.push('\n');
        }
        text
    }

    fn from_text(text: &str) -> Self {
        let linewise = text.ends_with('\n');
        let body = text.strip_suffix('\n').unwrap_or(text);
        Self {
            lines: body.split('\n').map(str::to_owned).collect(),
            linewise,
        }
    }
}

/// The selected region, ends included, start before end.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Selection {
    pub(crate) start: (usize, usize),
    pub(crate) end: (usize, usize),
    pub(crate) line: bool,
}

impl Selection {
    /// The selected characters of `row`, as a half-open range of columns,
    /// or `None` when the row is outside the selection.
    pub(crate) fn columns(&self, row: usize, len: usize) -> Option<(usize, usize)> {
        if row < self.start.0 || row > self.end.0 {
            return None;
        }
        if self.line {
            return Some((0, len.max(1)));
        }
        let from = if row == self.start.0 { self.start.1 } else { 0 };
        let to = if row == self.end.0 {
            self.end.1 + 1
        } else {
            len + 1
        };
        Some((from, to.max(from + 1)))
    }
}

#[derive(Debug, Clone)]
struct Snapshot {
    lines: Vec<String>,
    row: usize,
    col: usize,
}

/// What the editor holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    /// A file of the project, written to disk by `:w`.
    File,
    /// The conversation's context, handed back to the interface by `:w`.
    Context,
}

/// One open file, or the conversation's context.
pub(crate) struct Editor {
    kind: Kind,
    /// Relative to the project root.
    path: PathBuf,
    root: PathBuf,
    lines: Vec<String>,
    trailing_newline: bool,
    /// Set for binary or unreadable files, which are shown but not edited.
    read_only: bool,
    styled: Option<Vec<StyledLine>>,
    /// The file as of the last commit, `None` outside git.
    base: Option<Vec<String>>,
    /// How the lines on screen differ from `base`.
    changes: LineChanges,
    /// For each row the view drew, the line it shows, or `None` for a line
    /// of the last commit that is gone. Written by the view, read by clicks.
    rows: RefCell<Vec<Option<usize>>>,
    highlighter: Rc<Highlighter>,
    clipboard: Rc<dyn Clipboard>,
    row: usize,
    /// In characters, not bytes.
    col: usize,
    scroll: usize,
    /// Lines on screen, written by the view; movement keeps the cursor inside.
    height: Cell<usize>,
    /// Text columns on screen, written by the view, for mapping mouse clicks.
    width: Cell<usize>,
    mode: EditorMode,
    /// Where visual mode started; the other end is the cursor.
    anchor: (usize, usize),
    /// Rows of the last visual selection, for `:'<,'>`.
    last_visual: Option<Rows>,
    prompt: String,
    /// The first key of `dd`, `yy` or `gg`.
    pending: Option<char>,
    /// `"` was typed: the next key names a register.
    naming_register: bool,
    /// The register named for the next yank, delete or put.
    register: Option<char>,
    registers: HashMap<char, Register>,
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    last_search: Option<String>,
    modified: bool,
    message: Option<(String, bool)>,
}

/// What a file on disk holds, as the editor needs it.
struct Loaded {
    lines: Vec<String>,
    trailing_newline: bool,
    read_only: bool,
}

fn load(absolute: &Path) -> Loaded {
    match fs::read(absolute) {
        Ok(bytes) if bytes.iter().take(8000).any(|b| *b == 0) => Loaded {
            lines: vec![format!("(binary file, {} bytes)", bytes.len())],
            trailing_newline: true,
            read_only: true,
        },
        Ok(bytes) => {
            let text = String::from_utf8_lossy(&bytes);
            let lines: Vec<String> = text.lines().map(str::to_owned).collect();
            Loaded {
                trailing_newline: text.is_empty() || text.ends_with('\n'),
                lines: if lines.is_empty() {
                    vec![String::new()]
                } else {
                    lines
                },
                read_only: false,
            }
        }
        Err(e) => Loaded {
            lines: vec![format!("(cannot read: {e})")],
            trailing_newline: true,
            read_only: true,
        },
    }
}

impl Editor {
    pub(crate) fn open(root: &Path, path: PathBuf, highlighter: Rc<Highlighter>) -> Self {
        let loaded = load(&root.join(&path));
        let mut editor = Self {
            kind: Kind::File,
            path,
            root: root.to_owned(),
            lines: loaded.lines,
            trailing_newline: loaded.trailing_newline,
            read_only: loaded.read_only,
            styled: None,
            base: None,
            changes: LineChanges::default(),
            rows: RefCell::new(Vec::new()),
            highlighter,
            clipboard: Rc::new(SystemClipboard),
            row: 0,
            col: 0,
            scroll: 0,
            height: Cell::new(0),
            width: Cell::new(0),
            mode: EditorMode::Normal,
            anchor: (0, 0),
            last_visual: None,
            prompt: String::new(),
            pending: None,
            naming_register: false,
            register: None,
            registers: HashMap::new(),
            undo: Vec::new(),
            redo: Vec::new(),
            last_search: None,
            modified: false,
            message: None,
        };
        editor.load_base();
        editor.restyle();
        editor
    }

    /// Reads the last committed version of the file, to show what changed.
    fn load_base(&mut self) {
        self.base = if self.read_only {
            None
        } else {
            committed_lines(&self.root, &self.path)
        };
    }

    /// The conversation's context as a document: edited like a file, but
    /// `:w` hands it back instead of writing anything to disk.
    pub(crate) fn context(text: &str, highlighter: Rc<Highlighter>) -> Self {
        let mut editor = Self::open(Path::new("/"), PathBuf::from("<context>"), highlighter);
        editor.kind = Kind::Context;
        editor.lines = text.lines().map(str::to_owned).collect();
        if editor.lines.is_empty() {
            editor.lines.push(String::new());
        }
        editor.read_only = false;
        editor.base = None;
        editor.styled = None;
        editor.message = None;
        editor
    }

    pub(crate) fn kind(&self) -> Kind {
        self.kind
    }

    /// Marks the document as edited again, when applying it failed.
    pub(crate) fn mark_modified(&mut self) {
        self.modified = true;
    }

    #[cfg(test)]
    fn with_clipboard(mut self, clipboard: Rc<dyn Clipboard>) -> Self {
        self.clipboard = clipboard;
        self
    }

    /// Reads the file again, keeping registers, search and history.
    fn reload(&mut self) {
        let loaded = load(&self.root.join(&self.path));
        self.lines = loaded.lines;
        self.trailing_newline = loaded.trailing_newline;
        self.read_only = loaded.read_only;
        self.modified = false;
        self.mode = EditorMode::Normal;
        self.row = self.row.min(self.lines.len() - 1);
        self.clamp_col();
        self.scroll = self.scroll.min(self.row);
        self.load_base();
        self.restyle();
    }

    // Read access for the view.

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn lines(&self) -> &[String] {
        &self.lines
    }

    pub(crate) fn styled(&self) -> Option<&[StyledLine]> {
        self.styled.as_deref()
    }

    /// How the lines differ from the last commit, while that is known and in
    /// step with the lines; `None` outside git or while it is being redone.
    pub(crate) fn changes(&self) -> Option<&LineChanges> {
        (self.base.is_some() && self.changes.marks.len() == self.lines.len())
            .then_some(&self.changes)
    }

    /// Records which line each drawn row shows.
    pub(crate) fn set_rows(&self, rows: Vec<Option<usize>>) {
        *self.rows.borrow_mut() = rows;
    }

    pub(crate) fn cursor(&self) -> (usize, usize) {
        (self.row, self.col)
    }

    pub(crate) fn scroll(&self) -> usize {
        self.scroll
    }

    /// Records the size of the text area, line numbers excluded.
    pub(crate) fn set_viewport(&self, height: usize, width: usize) {
        self.height.set(height);
        self.width.set(width);
    }

    pub(crate) fn mode(&self) -> EditorMode {
        self.mode
    }

    pub(crate) fn prompt(&self) -> &str {
        &self.prompt
    }

    pub(crate) fn message(&self) -> Option<(&str, bool)> {
        self.message.as_ref().map(|(m, e)| (m.as_str(), *e))
    }

    pub(crate) fn is_modified(&self) -> bool {
        self.modified
    }

    /// Keys typed so far of a command not yet complete, as Vim's `showcmd`
    /// shows them: `"+`, `d`, `"+y`.
    pub(crate) fn partial_command(&self) -> String {
        let mut shown = String::new();
        if let Some(name) = self.register {
            shown.push('"');
            shown.push(name);
        } else if self.naming_register {
            shown.push('"');
        }
        if let Some(first) = self.pending {
            shown.push(first);
        }
        shown
    }

    pub(crate) fn selection(&self) -> Option<Selection> {
        let EditorMode::Visual { line } = self.mode else {
            return None;
        };
        let (start, end) = if self.anchor <= (self.row, self.col) {
            (self.anchor, (self.row, self.col))
        } else {
            ((self.row, self.col), self.anchor)
        };
        Some(Selection { start, end, line })
    }

    /// Whether keys should go to the interface (Tab, the `,` leader) rather
    /// than to the editor: only in normal mode with nothing half typed.
    pub(crate) fn is_idle(&self) -> bool {
        self.mode == EditorMode::Normal
            && self.pending.is_none()
            && !self.naming_register
            && self.register.is_none()
    }

    /// Width of the line number column, gutter spaces included.
    pub(crate) fn gutter(&self) -> usize {
        // The number, a space, the change mark, a space.
        self.lines.len().max(1).to_string().len() + 3
    }

    /// The first column shown, so that the cursor stays on screen in long
    /// lines. Every line is shifted by the same amount, as in Vim with `nowrap`.
    pub(crate) fn left_offset(&self) -> usize {
        (self.col + 1).saturating_sub(self.width.get().max(1))
    }

    // Changes from outside.

    /// Explains why the file stays open.
    pub(crate) fn refuse_close(&mut self) {
        self.say_error("Unsaved changes: :w to save, or :q! to discard them");
    }

    /// The agent wrote the file. Reloaded unless there are unsaved edits,
    /// which are never thrown away silently.
    pub(crate) fn changed_on_disk(&mut self) {
        if self.modified {
            self.say_error(
                "The agent changed this file on disk. :e! loads its version, :w keeps yours",
            );
            return;
        }
        self.reload();
        self.say("Reloaded: the agent changed this file");
    }

    /// Scrolls the view by `lines`, dragging the cursor along, as the mouse
    /// wheel does.
    pub(crate) fn scroll_by(&mut self, lines: i32) {
        let last = self.lines.len().saturating_sub(1) as i64;
        self.scroll = (self.scroll as i64 + i64::from(lines)).clamp(0, last) as usize;
        let height = self.view_height();
        self.row = self
            .row
            .clamp(self.scroll, (self.scroll + height - 1).min(last as usize));
        self.clamp_col();
    }

    /// Puts the cursor where the mouse clicked, in rows and columns from the
    /// top left of the text area, line numbers included.
    pub(crate) fn click(&mut self, row: usize, column: usize) {
        if matches!(self.mode, EditorMode::Command | EditorMode::Search) {
            self.mode = EditorMode::Normal;
        }
        // A row showing a removed line maps to no line: the click lands on
        // the line after it.
        let target = {
            let rows = self.rows.borrow();
            rows.iter()
                .skip(row)
                .find_map(|r| *r)
                .unwrap_or(self.scroll + row)
        };
        self.row = target.min(self.lines.len() - 1);
        self.col = column.saturating_sub(self.gutter()) + self.left_offset();
        self.clamp_col();
    }

    // Keys.

    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> Outcome {
        match self.mode {
            EditorMode::Normal => self.normal_key(key),
            EditorMode::Visual { line } => {
                self.visual_key(key, line);
                Outcome::Stay
            }
            EditorMode::Insert => {
                self.insert_key(key);
                Outcome::Stay
            }
            EditorMode::Command | EditorMode::Search => self.prompt_key(key),
        }
    }

    /// `"x` names a register for the next command. Returns whether the key
    /// was part of that.
    fn register_key(&mut self, key: KeyEvent) -> bool {
        if self.naming_register {
            self.naming_register = false;
            if let KeyCode::Char(name) = key.code
                && (name.is_ascii_alphanumeric() || matches!(name, '+' | '*' | '"'))
            {
                self.register = Some(name);
            }
            return true;
        }
        if key.code == KeyCode::Char('"') {
            self.naming_register = true;
            return true;
        }
        false
    }

    /// Cursor movement shared by normal and visual mode. Returns whether the
    /// key was a movement.
    fn motion(&mut self, key: KeyEvent) -> bool {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let half = (self.view_height() / 2).max(1) as i32;
        match key.code {
            KeyCode::Left | KeyCode::Char('h') if !ctrl => self.col = self.col.saturating_sub(1),
            KeyCode::Right | KeyCode::Char('l') if !ctrl => {
                self.col += 1;
                self.clamp_col();
            }
            KeyCode::Up | KeyCode::Char('k') if !ctrl => self.move_rows(-1),
            KeyCode::Down | KeyCode::Char('j') if !ctrl => self.move_rows(1),
            KeyCode::Char('d') if ctrl => self.move_rows(half),
            KeyCode::Char('u') if ctrl => self.move_rows(-half),
            KeyCode::PageDown => self.move_rows(half),
            KeyCode::PageUp => self.move_rows(-half),
            KeyCode::Home | KeyCode::Char('0') => self.col = 0,
            KeyCode::End | KeyCode::Char('$') => {
                self.col = usize::MAX;
                self.clamp_col();
            }
            KeyCode::Char('^') => self.col = self.first_non_blank(),
            KeyCode::Char('w') => self.word_forward(),
            KeyCode::Char('b') => self.word_backward(),
            KeyCode::Char('G') => self.go_to(self.lines.len() - 1),
            KeyCode::Char('n') => self.search_next(true),
            KeyCode::Char('N') => self.search_next(false),
            _ => return false,
        }
        true
    }

    fn normal_key(&mut self, key: KeyEvent) -> Outcome {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if let Some(first) = self.pending.take() {
            match (first, key.code) {
                ('d', KeyCode::Char('d')) => self.delete_line(),
                ('y', KeyCode::Char('y')) => self.yank_line(),
                ('g', KeyCode::Char('g')) => self.go_to(0),
                _ => {}
            }
            self.register = None;
            self.keep_visible();
            return Outcome::Stay;
        }
        if self.register_key(key) {
            return Outcome::Stay;
        }
        self.message = None;
        if self.motion(key) {
            self.keep_visible();
            return Outcome::Stay;
        }
        match key.code {
            KeyCode::Char('r') if ctrl => self.restore(true),
            KeyCode::Char(c @ ('g' | 'd' | 'y')) if !ctrl => {
                self.pending = Some(c);
                // The register stays named until the second key arrives.
                return Outcome::Stay;
            }
            KeyCode::Char('i') => self.start_insert(self.col),
            KeyCode::Char('a') => {
                let col = (self.col + 1).min(self.line_len());
                self.start_insert(col);
            }
            KeyCode::Char('I') => self.start_insert(self.first_non_blank()),
            KeyCode::Char('A') => self.start_insert(self.line_len()),
            KeyCode::Char('o') => self.open_line(true),
            KeyCode::Char('O') => self.open_line(false),
            KeyCode::Char('v') => self.start_visual(false),
            KeyCode::Char('V') => self.start_visual(true),
            KeyCode::Char('x') => self.delete_char(),
            KeyCode::Char('D') => self.delete_to_end(),
            KeyCode::Char('p') => self.put(true),
            KeyCode::Char('P') => self.put(false),
            KeyCode::Char('u') => self.restore(false),
            KeyCode::Char(':') => self.open_prompt(EditorMode::Command, ""),
            KeyCode::Char('/') => self.open_prompt(EditorMode::Search, ""),
            _ => {}
        }
        self.register = None;
        self.keep_visible();
        Outcome::Stay
    }

    fn visual_key(&mut self, key: KeyEvent, line: bool) {
        if let Some(first) = self.pending.take() {
            if (first, key.code) == ('g', KeyCode::Char('g')) {
                self.go_to(0);
            }
            self.keep_visible();
            return;
        }
        if self.register_key(key) {
            return;
        }
        self.message = None;
        if self.motion(key) {
            self.keep_visible();
            return;
        }
        match key.code {
            KeyCode::Esc => self.end_visual(),
            KeyCode::Char('v') if line => self.mode = EditorMode::Visual { line: false },
            KeyCode::Char('V') if !line => self.mode = EditorMode::Visual { line: true },
            KeyCode::Char('v' | 'V') => self.end_visual(),
            KeyCode::Char('g') => self.pending = Some('g'),
            KeyCode::Char('o') => {
                let cursor = (self.row, self.col);
                (self.row, self.col) = self.anchor;
                self.anchor = cursor;
            }
            KeyCode::Char('y') => {
                if let Some(selection) = self.selection() {
                    let text = self.selected(selection);
                    let count = text.lines.len();
                    self.end_visual();
                    (self.row, self.col) = selection.start;
                    self.store(text, &format!("{count} line{} yanked", plural(count)));
                }
            }
            KeyCode::Char('d' | 'x') => {
                self.delete_selection();
            }
            KeyCode::Char('c') => {
                // The delete is the undo step; the typing that follows joins it.
                if self.delete_selection() {
                    self.mode = EditorMode::Insert;
                    self.col = self.col.min(self.line_len());
                }
            }
            KeyCode::Char('>') => self.indent_selection(true),
            KeyCode::Char('<') => self.indent_selection(false),
            KeyCode::Char(':') => {
                if let Some(selection) = self.selection() {
                    self.last_visual = Some((selection.start.0, selection.end.0));
                }
                self.end_visual();
                self.open_prompt(EditorMode::Command, "'<,'>");
            }
            _ => {}
        }
        self.register = None;
        self.keep_visible();
    }

    fn insert_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => {
                self.mode = EditorMode::Normal;
                // As in Vim, the cursor steps back onto the last character typed.
                self.col = self.col.saturating_sub(1);
                self.clamp_col();
                self.restyle();
            }
            KeyCode::Enter => self.split_line(),
            KeyCode::Backspace => self.backspace(),
            KeyCode::Delete => self.delete_char(),
            KeyCode::Tab => self.insert_str(TAB),
            KeyCode::Left => self.col = self.col.saturating_sub(1),
            KeyCode::Right => self.col = (self.col + 1).min(self.line_len()),
            KeyCode::Up => self.move_rows(-1),
            KeyCode::Down => self.move_rows(1),
            KeyCode::Home => self.col = 0,
            KeyCode::End => self.col = self.line_len(),
            KeyCode::Char(c) if !ctrl => self.insert_str(c.encode_utf8(&mut [0; 4])),
            _ => {}
        }
        self.keep_visible();
    }

    fn open_prompt(&mut self, mode: EditorMode, text: &str) {
        self.prompt = text.to_owned();
        self.mode = mode;
    }

    fn prompt_key(&mut self, key: KeyEvent) -> Outcome {
        match key.code {
            KeyCode::Esc => self.mode = EditorMode::Normal,
            KeyCode::Backspace => {
                if self.prompt.pop().is_none() {
                    self.mode = EditorMode::Normal;
                }
            }
            KeyCode::Enter => {
                let text = std::mem::take(&mut self.prompt);
                let mode = self.mode;
                self.mode = EditorMode::Normal;
                let outcome = if mode == EditorMode::Command {
                    self.command(text.trim())
                } else {
                    if !text.is_empty() {
                        self.last_search = Some(text);
                    }
                    self.search_next(true);
                    Outcome::Stay
                };
                self.clamp_col();
                self.keep_visible();
                return outcome;
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.prompt.push(c);
            }
            _ => {}
        }
        Outcome::Stay
    }

    fn command(&mut self, command: &str) -> Outcome {
        let (range, rest) = match self.parse_range(command) {
            Ok(parsed) => parsed,
            Err(e) => {
                self.say_error(&e);
                return Outcome::Stay;
            }
        };
        // `:s/a/b/`, but not a word that happens to start with s.
        if let Some(spec) = rest.strip_prefix('s')
            && spec
                .chars()
                .next()
                .is_some_and(|c| !c.is_alphanumeric() && c != ' ')
        {
            let range = range.unwrap_or((self.row, self.row));
            match self.substitute(range, spec) {
                Ok(message) => self.say(&message),
                Err(e) => self.say_error(&e),
            }
            return Outcome::Stay;
        }
        if let Some((_, last)) = range {
            if rest.is_empty() {
                self.go_to(last);
            } else {
                self.say_error(&format!("A range is not accepted here: {command}"));
            }
            return Outcome::Stay;
        }
        if self.kind == Kind::Context {
            match rest {
                "w" | "wq" | "x" => {
                    self.modified = false;
                    return Outcome::Context {
                        text: self.lines.join("\n"),
                        close: rest != "w",
                    };
                }
                "e!" => {
                    self.say_error("The context is not a file: :q! drops the edits");
                    return Outcome::Stay;
                }
                _ => {}
            }
        }
        match rest {
            "w" => {
                if self.save() {
                    Outcome::Saved
                } else {
                    Outcome::Stay
                }
            }
            "wq" | "x" => {
                if self.save() {
                    Outcome::Close
                } else {
                    Outcome::Stay
                }
            }
            "q" => {
                if self.modified {
                    self.refuse_close();
                    Outcome::Stay
                } else {
                    Outcome::Close
                }
            }
            "q!" => Outcome::Close,
            "e!" => {
                self.reload();
                self.say("Reloaded from disk");
                Outcome::Stay
            }
            "" => Outcome::Stay,
            other => {
                self.say_error(&format!("Not an editor command: {other}"));
                Outcome::Stay
            }
        }
    }

    /// Reads a line range at the start of a command: `%`, `'<,'>`, `.`, `$`,
    /// a number, or two of those separated by a comma. Rows are 0 based.
    fn parse_range<'a>(&self, command: &'a str) -> Result<(Option<Rows>, &'a str), String> {
        let last = self.lines.len() - 1;
        if let Some(rest) = command.strip_prefix('%') {
            return Ok((Some((0, last)), rest));
        }
        if let Some(rest) = command.strip_prefix("'<,'>") {
            let rows = self
                .last_visual
                .ok_or_else(|| "No visual selection yet".to_owned())?;
            return Ok((Some(rows), rest));
        }
        // A row and how many characters spelled it.
        let line = |s: &str| -> Option<(usize, usize)> {
            let end = s
                .find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '$'))
                .unwrap_or(s.len());
            let row = match &s[..end] {
                "" => return None,
                "." => self.row,
                "$" => last,
                digits => digits.parse::<usize>().ok()?.saturating_sub(1).min(last),
            };
            Some((row, end))
        };
        let Some((first, used)) = line(command) else {
            return Ok((None, command));
        };
        let rest = &command[used..];
        if let Some(after) = rest.strip_prefix(',')
            && let Some((second, used2)) = line(after)
        {
            return Ok((
                Some((first.min(second), first.max(second))),
                &after[used2..],
            ));
        }
        Ok((Some((first, first)), rest))
    }

    /// `:s` after its range: `/pattern/replacement/flags`, with any delimiter.
    /// Patterns use Rust's regex syntax (as Vim's `\v` very magic mode), with
    /// Vim's `\<` and `\>` word boundaries; replacements accept `&` and `\1`.
    fn substitute(&mut self, (first, last): Rows, spec: &str) -> Result<String, String> {
        let mut chars = spec.chars();
        let delimiter = chars.next().ok_or("Empty substitution")?;
        let parts = split_unescaped(chars.as_str(), delimiter);
        let (pattern, replacement, flags) = match parts.as_slice() {
            [p] => (p.as_str(), "", ""),
            [p, r] => (p.as_str(), r.as_str(), ""),
            [p, r, f] => (p.as_str(), r.as_str(), f.as_str()),
            _ => return Err(format!("Too many {delimiter} in the substitution")),
        };
        let pattern = if pattern.is_empty() {
            self.last_search.clone().ok_or("No previous pattern")?
        } else {
            pattern.to_owned()
        };
        let mut global = false;
        let mut ignore_case = false;
        for flag in flags.chars() {
            match flag {
                'g' => global = true,
                'i' => ignore_case = true,
                'I' => ignore_case = false,
                other => return Err(format!("Flag not supported: {other}")),
            }
        }
        let regex = RegexBuilder::new(&pattern.replace("\\<", "\\b").replace("\\>", "\\b"))
            .case_insensitive(ignore_case)
            .build()
            .map_err(|e| format!("Invalid pattern: {e}"))?;
        let replacement = vim_replacement(replacement);

        let mut changes = Vec::new();
        let mut count = 0;
        for row in first..=last {
            let line = &self.lines[row];
            let found = if global {
                regex.find_iter(line).count()
            } else {
                usize::from(regex.is_match(line))
            };
            if found > 0 {
                let new = if global {
                    regex.replace_all(line, replacement.as_str())
                } else {
                    regex.replace(line, replacement.as_str())
                };
                changes.push((row, new.into_owned()));
                count += found;
            }
        }
        if changes.is_empty() {
            return Err(format!("Pattern not found: {pattern}"));
        }
        if !self.begin_change() {
            return Err("This file cannot be edited".into());
        }
        let lines = changes.len();
        for (row, text) in changes {
            self.lines[row] = text;
            self.row = row;
        }
        self.col = self.first_non_blank();
        self.last_search = Some(pattern);
        self.changed();
        Ok(format!(
            "{count} substitution{} on {lines} line{}",
            plural(count),
            plural(lines)
        ))
    }

    // Registers.

    /// Puts `text` in the named register (or the unnamed one), and in the
    /// unnamed one too, as Vim does.
    fn store(&mut self, text: Register, done: &str) {
        match self.register.take() {
            Some(name @ ('+' | '*')) => match self.clipboard.copy(&text.to_text()) {
                Ok(how) if done.is_empty() => {
                    self.say(&format!("Copied to the clipboard ({how})"));
                }
                Ok(how) => self.say(&format!("{done}, copied to the clipboard ({how})")),
                Err(e) => self.say_error(&format!("\"{name}: {e}")),
            },
            Some(name) if name != UNNAMED => {
                self.registers
                    .insert(name.to_ascii_lowercase(), text.clone());
                self.say(done);
            }
            _ => self.say(done),
        }
        self.registers.insert(UNNAMED, text);
    }

    fn fetch(&mut self) -> Option<Register> {
        match self.register.take() {
            Some(name @ ('+' | '*')) => match self.clipboard.paste() {
                Ok(text) if !text.is_empty() => Some(Register::from_text(&text)),
                Ok(_) => {
                    self.say_error(&format!("\"{name}: the clipboard is empty"));
                    None
                }
                Err(e) => {
                    self.say_error(&format!("\"{name}: {e}"));
                    None
                }
            },
            Some(name) => {
                let found = self.registers.get(&name.to_ascii_lowercase()).cloned();
                if found.is_none() {
                    self.say_error(&format!("Nothing in register {name}"));
                }
                found
            }
            None => self.registers.get(&UNNAMED).cloned(),
        }
    }

    // Editing.

    fn snapshot(&mut self) {
        self.undo.push(Snapshot {
            lines: self.lines.clone(),
            row: self.row,
            col: self.col,
        });
        self.redo.clear();
    }

    /// Undo when `forward` is false, redo when it is true.
    fn restore(&mut self, forward: bool) {
        let (from, to) = if forward {
            (&mut self.redo, &mut self.undo)
        } else {
            (&mut self.undo, &mut self.redo)
        };
        let Some(state) = from.pop() else {
            self.say(if forward {
                "Already at newest change"
            } else {
                "Already at oldest change"
            });
            return;
        };
        to.push(Snapshot {
            lines: std::mem::replace(&mut self.lines, state.lines),
            row: self.row,
            col: self.col,
        });
        self.row = state.row.min(self.lines.len() - 1);
        self.col = state.col;
        self.clamp_col();
        self.modified = true;
        self.restyle();
    }

    /// Checks that the file may be edited, and records the state for undo.
    fn begin_change(&mut self) -> bool {
        if self.read_only {
            self.say_error("This file cannot be edited");
            return false;
        }
        self.snapshot();
        true
    }

    fn changed(&mut self) {
        self.modified = true;
        if self.mode != EditorMode::Insert || self.lines.len() <= LIVE_HIGHLIGHT_LINES {
            self.restyle();
        } else {
            self.styled = None;
        }
    }

    fn start_insert(&mut self, col: usize) {
        if self.begin_change() {
            self.col = col;
            self.mode = EditorMode::Insert;
        }
    }

    fn start_visual(&mut self, line: bool) {
        self.anchor = (self.row, self.col);
        self.mode = EditorMode::Visual { line };
    }

    fn end_visual(&mut self) {
        if let Some(selection) = self.selection() {
            self.last_visual = Some((selection.start.0, selection.end.0));
        }
        self.mode = EditorMode::Normal;
        self.clamp_col();
    }

    fn open_line(&mut self, below: bool) {
        if !self.begin_change() {
            return;
        }
        let indent: String = self.lines[self.row]
            .chars()
            .take_while(|c| c.is_whitespace())
            .collect();
        let at = if below { self.row + 1 } else { self.row };
        self.lines.insert(at, indent.clone());
        self.row = at;
        self.col = indent.chars().count();
        self.mode = EditorMode::Insert;
        self.changed();
    }

    fn insert_str(&mut self, text: &str) {
        let at = byte_index(&self.lines[self.row], self.col);
        self.lines[self.row].insert_str(at, text);
        self.col += text.chars().count();
        self.changed();
    }

    /// Enter in insert mode: the new line keeps the indentation of this one.
    fn split_line(&mut self) {
        let line = &mut self.lines[self.row];
        let at = byte_index(line, self.col);
        let rest = line.split_off(at);
        let indent: String = line.chars().take_while(|c| c.is_whitespace()).collect();
        self.row += 1;
        self.col = indent.chars().count();
        self.lines.insert(self.row, indent + rest.trim_start());
        self.changed();
    }

    fn backspace(&mut self) {
        if self.col > 0 {
            self.col -= 1;
            let at = byte_index(&self.lines[self.row], self.col);
            self.lines[self.row].remove(at);
        } else if self.row > 0 {
            let line = self.lines.remove(self.row);
            self.row -= 1;
            self.col = self.line_len();
            self.lines[self.row].push_str(&line);
        } else {
            return;
        }
        self.changed();
    }

    fn delete_char(&mut self) {
        if self.line_len() == 0 {
            return;
        }
        if self.mode == EditorMode::Normal && !self.begin_change() {
            return;
        }
        let at = byte_index(&self.lines[self.row], self.col);
        if at < self.lines[self.row].len() {
            self.lines[self.row].remove(at);
        }
        self.clamp_col();
        self.changed();
    }

    fn delete_to_end(&mut self) {
        if !self.begin_change() {
            return;
        }
        let at = byte_index(&self.lines[self.row], self.col);
        let cut = self.lines[self.row].split_off(at);
        self.store(
            Register {
                lines: vec![cut],
                linewise: false,
            },
            "",
        );
        self.clamp_col();
        self.changed();
    }

    fn delete_line(&mut self) {
        if !self.begin_change() {
            return;
        }
        let line = self.lines.remove(self.row);
        if self.lines.is_empty() {
            self.lines.push(String::new());
        }
        self.store(
            Register {
                lines: vec![line],
                linewise: true,
            },
            "",
        );
        self.row = self.row.min(self.lines.len() - 1);
        self.col = self.first_non_blank();
        self.changed();
    }

    fn yank_line(&mut self) {
        let text = Register {
            lines: vec![self.lines[self.row].clone()],
            linewise: true,
        };
        self.store(text, "1 line yanked");
    }

    /// The selected text, as a register would hold it.
    fn selected(&self, selection: Selection) -> Register {
        let (first, last) = (selection.start.0, selection.end.0);
        if selection.line {
            return Register {
                lines: self.lines[first..=last].to_vec(),
                linewise: true,
            };
        }
        let mut lines = Vec::new();
        for row in first..=last {
            let len = self.lines[row].chars().count();
            let (from, to) = selection.columns(row, len).unwrap_or((0, 0));
            let line = &self.lines[row];
            let (a, b) = (byte_index(line, from), byte_index(line, to.min(len)));
            lines.push(line[a..b].to_owned());
        }
        Register {
            lines,
            linewise: false,
        }
    }

    /// Deletes the visual selection into the register. Returns whether it did.
    fn delete_selection(&mut self) -> bool {
        let Some(selection) = self.selection() else {
            return false;
        };
        if !self.begin_change() {
            self.end_visual();
            return false;
        }
        let text = self.selected(selection);
        let (first, last) = (selection.start.0, selection.end.0);
        if selection.line {
            self.lines.drain(first..=last);
            if self.lines.is_empty() {
                self.lines.push(String::new());
            }
            self.row = first.min(self.lines.len() - 1);
            self.col = 0;
        } else {
            let end_len = self.lines[last].chars().count();
            let head = {
                let line = &self.lines[first];
                line[..byte_index(line, selection.start.1)].to_owned()
            };
            let tail = {
                let line = &self.lines[last];
                let to = (selection.end.1 + 1).min(end_len);
                line[byte_index(line, to)..].to_owned()
            };
            self.lines.drain(first + 1..=last);
            self.lines[first] = head + &tail;
            self.row = first;
            self.col = selection.start.1;
        }
        self.mode = EditorMode::Normal;
        self.last_visual = Some((first, last));
        let count = text.lines.len();
        self.store(text, &format!("{count} line{} deleted", plural(count)));
        self.clamp_col();
        self.changed();
        true
    }

    fn indent_selection(&mut self, deeper: bool) {
        let Some(selection) = self.selection() else {
            return;
        };
        if !self.begin_change() {
            return;
        }
        for row in selection.start.0..=selection.end.0 {
            let line = &mut self.lines[row];
            if deeper {
                if !line.is_empty() {
                    line.insert_str(0, TAB);
                }
            } else {
                let spaces = line
                    .chars()
                    .take(TAB.len())
                    .take_while(|c| *c == ' ')
                    .count();
                let cut = if spaces == 0 && line.starts_with('\t') {
                    1
                } else {
                    spaces
                };
                line.drain(..cut);
            }
        }
        self.end_visual();
        self.row = selection.start.0;
        self.col = self.first_non_blank();
        self.changed();
    }

    /// `p` puts after the cursor (below for lines), `P` before (above).
    fn put(&mut self, after: bool) {
        let Some(text) = self.fetch() else {
            return;
        };
        if !self.begin_change() {
            return;
        }
        if text.linewise {
            let at = if after { self.row + 1 } else { self.row };
            for (i, line) in text.lines.iter().enumerate() {
                self.lines.insert(at + i, line.clone());
            }
            self.row = at;
            self.col = self.first_non_blank();
        } else {
            let line = &self.lines[self.row];
            let col = if after && !line.is_empty() {
                self.col + 1
            } else {
                self.col
            };
            let at = byte_index(line, col);
            let tail = self.lines[self.row].split_off(at);
            let mut pieces = text.lines.iter();
            if let Some(first) = pieces.next() {
                self.lines[self.row].push_str(first);
            }
            let mut row = self.row;
            for piece in pieces {
                row += 1;
                self.lines.insert(row, piece.clone());
            }
            self.lines[row].push_str(&tail);
            if text.lines.len() == 1 {
                self.col = col + text.lines[0].chars().count().saturating_sub(1);
            } else {
                self.col = col;
            }
        }
        self.clamp_col();
        self.changed();
    }

    /// Writes the file. Returns whether it worked.
    fn save(&mut self) -> bool {
        if self.read_only {
            self.say_error("This file cannot be written");
            return false;
        }
        let mut text = self.lines.join("\n");
        if self.trailing_newline {
            text.push('\n');
        }
        match fs::write(self.root.join(&self.path), &text) {
            Ok(()) => {
                self.modified = false;
                let n = self.lines.len();
                self.say(&format!(
                    "\"{}\" {n} line{} written",
                    self.path.display(),
                    plural(n)
                ));
                true
            }
            Err(e) => {
                self.say_error(&format!("Cannot write: {e}"));
                false
            }
        }
    }

    // Movement.

    fn view_height(&self) -> usize {
        match self.height.get() {
            0 => 20,
            h => h,
        }
    }

    fn line_len(&self) -> usize {
        self.lines[self.row].chars().count()
    }

    fn first_non_blank(&self) -> usize {
        self.lines[self.row]
            .chars()
            .take_while(|c| c.is_whitespace())
            .count()
    }

    /// In normal and visual mode the cursor sits on a character, so it stops
    /// one before the end; in insert mode it may sit after the last one.
    fn clamp_col(&mut self) {
        let len = self.line_len();
        let max = if self.mode == EditorMode::Insert {
            len
        } else {
            len.saturating_sub(1)
        };
        self.col = self.col.min(max);
    }

    fn move_rows(&mut self, delta: i32) {
        let last = (self.lines.len() - 1) as i64;
        self.row = (self.row as i64 + i64::from(delta)).clamp(0, last) as usize;
        self.clamp_col();
    }

    fn go_to(&mut self, row: usize) {
        self.row = row;
        self.col = self.first_non_blank();
        self.keep_visible();
    }

    fn keep_visible(&mut self) {
        let height = self.view_height();
        if self.row < self.scroll {
            self.scroll = self.row;
        } else if self.row >= self.scroll + height {
            self.scroll = self.row + 1 - height;
        }
    }

    fn word_forward(&mut self) {
        let chars: Vec<char> = self.lines[self.row].chars().collect();
        let mut i = self.col;
        let class = |c: char| {
            if c.is_alphanumeric() || c == '_' {
                1
            } else if c.is_whitespace() {
                0
            } else {
                2
            }
        };
        if i < chars.len() {
            let start = class(chars[i]);
            while i < chars.len() && class(chars[i]) == start && start != 0 {
                i += 1;
            }
        }
        while i < chars.len() && chars[i].is_whitespace() {
            i += 1;
        }
        if i >= chars.len() && self.row + 1 < self.lines.len() {
            self.row += 1;
            self.col = self.first_non_blank();
        } else {
            self.col = i;
            self.clamp_col();
        }
    }

    fn word_backward(&mut self) {
        if self.col == 0 {
            if self.row > 0 {
                self.row -= 1;
                self.col = usize::MAX;
                self.clamp_col();
            }
            return;
        }
        let chars: Vec<char> = self.lines[self.row].chars().collect();
        let mut i = self.col.min(chars.len());
        while i > 0 && chars[i - 1].is_whitespace() {
            i -= 1;
        }
        let word = |c: char| c.is_alphanumeric() || c == '_';
        if i > 0 {
            let in_word = word(chars[i - 1]);
            while i > 0 && !chars[i - 1].is_whitespace() && word(chars[i - 1]) == in_word {
                i -= 1;
            }
        }
        self.col = i;
    }

    fn search_next(&mut self, forward: bool) {
        let Some(pattern) = self.last_search.clone() else {
            self.say_error("No previous search");
            return;
        };
        let count = self.lines.len();
        // Scan whole lines from the cursor, wrapping around the file once.
        for step in 0..=count {
            let row = if forward {
                (self.row + step) % count
            } else {
                (self.row + count - step % count) % count
            };
            let line = &self.lines[row];
            let found = if step == 0 {
                let here = byte_index(line, self.col);
                if forward {
                    let from = byte_index(line, self.col + 1).max(here);
                    line[from..].find(&pattern).map(|i| from + i)
                } else {
                    line[..here].rfind(&pattern)
                }
            } else if forward {
                line.find(&pattern)
            } else {
                line.rfind(&pattern)
            };
            if let Some(at) = found {
                self.row = row;
                self.col = line[..at].chars().count();
                self.say(&format!("/{pattern}"));
                return;
            }
        }
        self.say_error(&format!("Pattern not found: {pattern}"));
    }

    // Messages and colours.

    fn say(&mut self, text: &str) {
        self.message = (!text.is_empty()).then(|| (text.to_owned(), false));
    }

    fn say_error(&mut self, text: &str) {
        self.message = Some((text.to_owned(), true));
    }

    fn restyle(&mut self) {
        if let Some(base) = &self.base {
            self.changes = line_changes(base, &self.lines);
        }
        self.styled = if self.read_only {
            None
        } else {
            self.highlighter.highlight(&self.path, &self.lines)
        };
    }
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

/// The byte offset of character `chars` in `line`, or its end.
fn byte_index(line: &str, chars: usize) -> usize {
    line.char_indices()
        .nth(chars)
        .map_or(line.len(), |(i, _)| i)
}

/// Splits on `delimiter` where it is not escaped with a backslash; `\` before
/// the delimiter is dropped, other escapes are kept for the regex.
fn split_unescaped(text: &str, delimiter: char) -> Vec<String> {
    let mut parts = vec![String::new()];
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' && chars.peek() == Some(&delimiter) {
            if let Some(part) = parts.last_mut() {
                part.push(delimiter);
            }
            chars.next();
        } else if c == delimiter {
            parts.push(String::new());
        } else if let Some(part) = parts.last_mut() {
            part.push(c);
            if c == '\\'
                && let Some(next) = chars.next()
            {
                part.push(next);
            }
        }
    }
    // A trailing delimiter (`s/a/b/`) leaves an empty flags part.
    if parts.len() == 3 && parts[2].is_empty() {
        parts.pop();
    }
    parts
}

/// Vim's replacement syntax in the regex crate's: `&` and `\0` the whole
/// match, `\1`..`\9` groups, `\&` a literal `&`, and `$` kept literal.
fn vim_replacement(text: &str) -> String {
    let mut out = String::new();
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        match c {
            '$' => out.push_str("$$"),
            '&' => out.push_str("${0}"),
            '\\' => match chars.next() {
                Some(d) if d.is_ascii_digit() => out.push_str(&format!("${{{d}}}")),
                Some('$') => out.push_str("$$"),
                Some(other) => out.push(other),
                None => out.push('\\'),
            },
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;

    /// A clipboard in memory.
    #[derive(Default)]
    struct Memory(RefCell<String>);

    impl Clipboard for Memory {
        fn copy(&self, text: &str) -> Result<&'static str, String> {
            *self.0.borrow_mut() = text.to_owned();
            Ok("memory")
        }

        fn paste(&self) -> Result<String, String> {
            Ok(self.0.borrow().clone())
        }
    }

    fn editor(text: &str) -> (tempfile::TempDir, Editor) {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.py"), text).unwrap();
        let editor = Editor::open(
            dir.path(),
            PathBuf::from("a.py"),
            Rc::new(Highlighter::new()),
        )
        .with_clipboard(Rc::new(Memory::default()));
        (dir, editor)
    }

    fn keys(editor: &mut Editor, keys: &str) -> Outcome {
        let mut outcome = Outcome::Stay;
        for c in keys.chars() {
            let code = match c {
                '\n' => KeyCode::Enter,
                '\u{1b}' => KeyCode::Esc,
                c => KeyCode::Char(c),
            };
            outcome = editor.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
        }
        outcome
    }

    #[test]
    fn insert_then_write_saves_the_file() {
        let (dir, mut ed) = editor("print(1)\n");
        keys(&mut ed, "A  # one\u{1b}");
        assert!(ed.is_modified());
        keys(&mut ed, ":w\n");
        assert!(!ed.is_modified());
        assert_eq!(
            fs::read_to_string(dir.path().join("a.py")).unwrap(),
            "print(1)  # one\n"
        );
    }

    #[test]
    fn enter_keeps_indentation() {
        let (_dir, mut ed) = editor("def f():\n    a = 1\n");
        keys(&mut ed, "jA\nb = 2\u{1b}");
        assert_eq!(ed.lines(), ["def f():", "    a = 1", "    b = 2"]);
    }

    #[test]
    fn dd_p_moves_a_line_and_u_undoes_each_step() {
        let (_dir, mut ed) = editor("one\ntwo\nthree\n");
        keys(&mut ed, "ddp");
        assert_eq!(ed.lines(), ["two", "one", "three"]);
        keys(&mut ed, "u");
        assert_eq!(ed.lines(), ["two", "three"]);
        keys(&mut ed, "u");
        assert_eq!(ed.lines(), ["one", "two", "three"]);
        ed.handle_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL));
        assert_eq!(ed.lines(), ["two", "three"]);
    }

    #[test]
    fn colon_number_and_search_move_the_cursor() {
        let (_dir, mut ed) = editor("a\nfoo\nb\nfoo bar\n");
        keys(&mut ed, ":3\n");
        assert_eq!(ed.cursor(), (2, 0));
        keys(&mut ed, "/foo\n");
        assert_eq!(ed.cursor(), (3, 0));
        keys(&mut ed, "n");
        assert_eq!(ed.cursor(), (1, 0));
        keys(&mut ed, "/nothing\n");
        assert_eq!(ed.message(), Some(("Pattern not found: nothing", true)));
    }

    #[test]
    fn q_refuses_unsaved_changes_and_q_bang_does_not() {
        let (_dir, mut ed) = editor("x\n");
        keys(&mut ed, "x");
        assert_eq!(keys(&mut ed, ":q\n"), Outcome::Stay);
        assert_eq!(keys(&mut ed, ":q!\n"), Outcome::Close);
    }

    #[test]
    fn words_and_line_ends() {
        let (_dir, mut ed) = editor("let total = price * 2\n");
        keys(&mut ed, "ww");
        assert_eq!(ed.cursor(), (0, 10));
        keys(&mut ed, "b");
        assert_eq!(ed.cursor(), (0, 4));
        keys(&mut ed, "$");
        assert_eq!(ed.cursor(), (0, 20));
        keys(&mut ed, "0");
        assert_eq!(ed.cursor(), (0, 0));
    }

    #[test]
    fn unsaved_edits_survive_an_agent_write() {
        let (dir, mut ed) = editor("mine\n");
        keys(&mut ed, "A!\u{1b}");
        fs::write(dir.path().join("a.py"), "theirs\n").unwrap();
        ed.changed_on_disk();
        assert_eq!(ed.lines(), ["mine!"]);
        assert!(ed.message().is_some_and(|(_, error)| error));
    }

    #[test]
    fn binary_files_are_read_only() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("bin"), [0x7f, 0, 1]).unwrap();
        let mut ed = Editor::open(
            dir.path(),
            PathBuf::from("bin"),
            Rc::new(Highlighter::new()),
        );
        keys(&mut ed, "dd");
        assert_eq!(ed.lines(), ["(binary file, 3 bytes)"]);
        assert_eq!(ed.message(), Some(("This file cannot be edited", true)));
    }

    #[test]
    fn visual_line_yank_and_put() {
        let (_dir, mut ed) = editor("a\nb\nc\n");
        keys(&mut ed, "Vjy");
        assert_eq!(ed.mode(), EditorMode::Normal);
        keys(&mut ed, "Gp");
        assert_eq!(ed.lines(), ["a", "b", "c", "a", "b"]);
    }

    #[test]
    fn visual_characters_delete_across_lines() {
        let (_dir, mut ed) = editor("hello world\nsecond line\n");
        keys(&mut ed, "wvjd");
        assert_eq!(ed.lines(), ["hello line"]);
        keys(&mut ed, "$p");
        assert_eq!(ed.lines(), ["hello lineworld", "second "]);
    }

    #[test]
    fn visual_indent_and_change() {
        let (_dir, mut ed) = editor("a\nb\n");
        keys(&mut ed, "Vj>");
        assert_eq!(ed.lines(), ["    a", "    b"]);
        keys(&mut ed, "Vj<");
        assert_eq!(ed.lines(), ["a", "b"]);
        keys(&mut ed, "vcz\u{1b}");
        assert_eq!(ed.lines(), ["z", "b"]);
    }

    #[test]
    fn edits_are_compared_with_the_last_commit() {
        let dir = tempfile::tempdir().unwrap();
        let run = |args: &[&str]| {
            assert!(
                std::process::Command::new("git")
                    .args(args)
                    .current_dir(dir.path())
                    .output()
                    .unwrap()
                    .status
                    .success()
            );
        };
        run(&["init", "-q"]);
        fs::write(dir.path().join("a.py"), "one\ntwo\nthree\n").unwrap();
        run(&["add", "a.py"]);
        run(&[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-q",
            "-m",
            "x",
        ]);

        let mut ed = Editor::open(
            dir.path(),
            PathBuf::from("a.py"),
            Rc::new(Highlighter::new()),
        )
        .with_clipboard(Rc::new(Memory::default()));
        assert!(ed.changes().unwrap().is_empty());

        keys(&mut ed, "jddOnew\u{1b}");
        let changes = ed.changes().unwrap();
        assert_eq!(ed.lines(), ["one", "new", "three"]);
        assert_eq!(changes.marks[1], ironquill_tools::LineMark::Changed);
        assert_eq!(changes.removed[&1], ["two"]);
        assert_eq!(keys(&mut ed, ":w\n"), Outcome::Saved);
    }

    #[test]
    fn the_context_is_handed_back_on_w_not_written() {
        let mut ed = Editor::context(
            "=== user\nhello\n=== assistant\nhi",
            Rc::new(Highlighter::new()),
        );
        assert_eq!(ed.kind(), Kind::Context);
        assert!(ed.changes().is_none());
        keys(&mut ed, "Gdd");
        assert!(ed.is_modified());
        assert_eq!(
            keys(&mut ed, ":w\n"),
            Outcome::Context {
                text: "=== user\nhello\n=== assistant".into(),
                close: false
            }
        );
        assert!(!ed.is_modified());
        assert!(matches!(
            keys(&mut ed, ":wq\n"),
            Outcome::Context { close: true, .. }
        ));
    }

    #[test]
    fn outside_git_nothing_is_marked() {
        let (_dir, ed) = editor("x\n");
        assert!(ed.changes().is_none());
    }

    #[test]
    fn plus_register_goes_through_the_clipboard() {
        let (_dir, mut ed) = editor("copy me\nother\n");
        keys(&mut ed, "\"+yy");
        assert_eq!(ed.clipboard.paste().unwrap(), "copy me\n");
        assert!(ed.message().is_some_and(|(m, _)| m.contains("clipboard")));

        ed.clipboard.copy("from outside").unwrap();
        keys(&mut ed, "j\"+P");
        assert_eq!(ed.lines(), ["copy me", "from outsideother"]);
    }

    #[test]
    fn named_registers_keep_their_text() {
        let (_dir, mut ed) = editor("one\ntwo\n");
        keys(&mut ed, "\"ayyjyy\"ap");
        assert_eq!(ed.lines(), ["one", "two", "one"]);
    }

    #[test]
    fn substitute_on_a_line_the_file_and_a_visual_range() {
        let (_dir, mut ed) = editor("foo foo\nfoo\nbar foo\n");
        keys(&mut ed, ":s/foo/x/\n");
        assert_eq!(ed.lines(), ["x foo", "foo", "bar foo"]);
        keys(&mut ed, ":%s/foo/y/g\n");
        assert_eq!(ed.lines(), ["x y", "y", "bar y"]);
        assert_eq!(ed.message(), Some(("3 substitutions on 3 lines", false)));

        keys(&mut ed, "ggVj:s/y/[&]/\n");
        assert_eq!(ed.lines(), ["x [y]", "[y]", "bar y"]);
        keys(&mut ed, ":2,3s/\\[?y\\]?/Z/\n");
        assert_eq!(ed.lines(), ["x [y]", "Z", "bar Z"]);
    }

    #[test]
    fn substitute_groups_and_errors() {
        let (_dir, mut ed) = editor("name = value\n");
        keys(&mut ed, r":s/(\w+) = (\w+)/\2 = \1/");
        keys(&mut ed, "\n");
        assert_eq!(ed.lines(), ["value = name"]);
        keys(&mut ed, ":s/nothing/x/\n");
        assert_eq!(ed.message(), Some(("Pattern not found: nothing", true)));
        keys(&mut ed, ":s/a/b/z\n");
        assert_eq!(ed.message(), Some(("Flag not supported: z", true)));
    }
}
