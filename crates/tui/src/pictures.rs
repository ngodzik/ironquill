//! Pictures in replies: PNG files and Mermaid diagrams, drawn where the
//! terminal draws images, else left as the text that names them.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use ratatui::style::{Color, Style};
use ratatui::text::Line;

use universe_ui::diagrams::Renderer;

use crate::graphics::{self, Terminal, Wanted};

/// What a reply asks to be shown as an image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Picture<'a> {
    /// A PNG file, as a reply or the person named it: from the project, the
    /// home directory (`~/`) or an absolute path.
    File(&'a str),
    /// The source of a Mermaid diagram.
    Mermaid(&'a str),
}

/// What draws pictures as lines of the conversation.
pub(crate) trait Pictures {
    /// The lines `picture` is drawn on, at most `width` wide; `None` when it
    /// cannot be shown, and is left as text.
    fn lines(&self, picture: Picture<'_>, width: usize) -> Option<Vec<Line<'static>>>;
}

/// The gallery draws while the screen is drawn, which only lends it.
impl Pictures for std::cell::RefCell<Gallery> {
    fn lines(&self, picture: Picture<'_>, width: usize) -> Option<Vec<Line<'static>>> {
        self.borrow_mut().lines(picture, width)
    }
}

/// Shows no picture: text stays text.
#[cfg(test)]
pub(crate) struct NoPictures;

#[cfg(test)]
impl Pictures for NoPictures {
    fn lines(&self, _: Picture<'_>, _: usize) -> Option<Vec<Line<'static>>> {
        None
    }
}

/// The most rows a picture takes, so that one fits on screen with its reply.
const MAX_ROWS: u16 = 30;

/// The largest file shown, in bytes.
const MAX_BYTES: u64 = 32 * 1024 * 1024;

/// A diagram, by the hash of its source and of what draws it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Diagram {
    Drawing,
    Drawn(PathBuf),
    Failed,
}

/// A file found to be a PNG: its id with the terminal and its size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Known {
    id: u32,
    pixels: (u32, u32),
}

/// The pictures of the conversation, and what the terminal needs for them.
#[derive(Debug)]
pub(crate) struct Gallery {
    terminal: Option<Terminal>,
    renderer: Option<Renderer>,
    /// Where drawn diagrams are kept: `~/.universe/cache/diagrams`.
    cache: Option<PathBuf>,
    root: PathBuf,
    /// The size of a cell in pixels.
    cell: (u16, u16),
    /// Files by path and when they last changed; `None` for one that cannot
    /// be shown.
    files: HashMap<(PathBuf, SystemTime), Option<Known>>,
    diagrams: HashMap<u64, Diagram>,
    /// Images the terminal could not be given.
    failed: HashSet<u32>,
    next_id: u32,
    /// The images drawn since the terminal was last told.
    wanted: Vec<Wanted>,
    /// Diagrams to draw: their key and source.
    to_draw: Vec<(u64, String)>,
}

impl Gallery {
    /// A gallery that shows nothing, until [`Gallery::show_in`].
    pub(crate) fn new(root: PathBuf) -> Self {
        Self {
            terminal: None,
            renderer: None,
            cache: None,
            root,
            // A common cell, until the terminal says.
            cell: (8, 16),
            files: HashMap::new(),
            diagrams: HashMap::new(),
            failed: HashSet::new(),
            next_id: 1,
            wanted: Vec::new(),
            to_draw: Vec::new(),
        }
    }

    /// Shows pictures in `terminal`, drawing diagrams with `renderer` and
    /// keeping them in `cache`.
    pub(crate) fn show_in(
        &mut self,
        terminal: Option<Terminal>,
        renderer: Option<Renderer>,
        cache: Option<PathBuf>,
    ) {
        self.terminal = terminal;
        self.renderer = renderer;
        self.cache = cache;
    }

    /// Whether pictures are drawn at all.
    pub(crate) fn shows(&self) -> bool {
        self.terminal.is_some()
    }

    /// The size of a cell in pixels, as the terminal says it; ignored when
    /// it says nothing.
    pub(crate) fn set_cell(&mut self, cell: (u16, u16)) {
        if cell.0 > 0 && cell.1 > 0 {
            self.cell = cell;
        }
    }

    /// The images drawn since last asked, for the terminal to be told.
    pub(crate) fn take_wanted(&mut self) -> Vec<Wanted> {
        std::mem::take(&mut self.wanted)
    }

    /// The diagrams to draw since last asked.
    pub(crate) fn take_to_draw(&mut self) -> Vec<(u64, String)> {
        std::mem::take(&mut self.to_draw)
    }

    /// Images the terminal could not be given: shown as text from now on.
    pub(crate) fn failed(&mut self, ids: &[u32]) {
        self.failed.extend(ids);
    }

    /// A diagram is drawn, or could not be.
    pub(crate) fn drawn(&mut self, key: u64, result: &Result<PathBuf, String>) {
        let diagram = match result {
            Ok(path) => Diagram::Drawn(path.clone()),
            Err(_) => Diagram::Failed,
        };
        self.diagrams.insert(key, diagram);
    }

    /// The lines `picture` is drawn on, at most `width` wide.
    pub(crate) fn lines(
        &mut self,
        picture: Picture<'_>,
        width: usize,
    ) -> Option<Vec<Line<'static>>> {
        self.terminal?;
        let path = match picture {
            Picture::File(name) => self.resolve(name),
            Picture::Mermaid(source) => match self.diagram(source)? {
                Diagram::Drawn(path) => path,
                Diagram::Drawing => {
                    return Some(vec![Line::styled(
                        "◌ drawing the diagram…",
                        Style::new().fg(Color::DarkGray),
                    )]);
                }
                Diagram::Failed => return None,
            },
        };
        let known = self.known(&path)?;
        if self.failed.contains(&known.id) {
            return None;
        }
        let width = u16::try_from(width).unwrap_or(u16::MAX);
        let (cols, rows) = graphics::fit(known.pixels, self.cell, width, MAX_ROWS);
        self.wanted.push(Wanted {
            id: known.id,
            path,
            cols,
            rows,
        });
        Some(graphics::cells(known.id, cols, rows))
    }

    /// Where a file named in a reply is.
    fn resolve(&self, name: &str) -> PathBuf {
        if let Some(rest) = name.strip_prefix("~/")
            && let Some(home) = std::env::var_os("HOME")
        {
            return PathBuf::from(home).join(rest);
        }
        self.root.join(name)
    }

    /// The file at `path` as an image, looked at again when it changes.
    fn known(&mut self, path: &Path) -> Option<Known> {
        let meta = std::fs::metadata(path).ok()?;
        if !meta.is_file() || meta.len() > MAX_BYTES {
            return None;
        }
        let key = (path.to_owned(), meta.modified().ok()?);
        if let Some(known) = self.files.get(&key) {
            return *known;
        }
        let mut head = [0; 24];
        let pixels = File::open(path)
            .and_then(|mut f| f.read_exact(&mut head))
            .ok()
            .and_then(|()| graphics::png_size(&head));
        let known = pixels.map(|pixels| {
            let id = self.next_id;
            self.next_id += 1;
            Known { id, pixels }
        });
        self.files.insert(key, known);
        known
    }

    /// Where the diagram of `source` stands, asking for it to be drawn when
    /// first seen; `None` with nothing to draw it.
    fn diagram(&mut self, source: &str) -> Option<Diagram> {
        let renderer = self.renderer.as_ref()?;
        let cache = self.cache.as_ref()?;
        let key = renderer.key(source);
        if let Some(diagram) = self.diagrams.get(&key) {
            return Some(diagram.clone());
        }
        // Drawn in an earlier session.
        let drawn = cache.join(format!("{key:016x}.png"));
        let diagram = if drawn.is_file() {
            Diagram::Drawn(drawn)
        } else {
            self.to_draw.push((key, source.to_owned()));
            Diagram::Drawing
        };
        self.diagrams.insert(key, diagram.clone());
        Some(diagram)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(dir: &Path, name: &str, width: u32, height: u32) {
        let mut data = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
        data.extend(width.to_be_bytes());
        data.extend(height.to_be_bytes());
        std::fs::write(dir.join(name), data).unwrap();
    }

    fn kitty() -> Option<Terminal> {
        graphics::detect(universe_ui::defaults::Images::Kitty, |_| None)
    }

    #[test]
    fn nothing_is_drawn_without_a_terminal_that_draws() {
        let dir = tempfile::tempdir().unwrap();
        png(dir.path(), "a.png", 80, 16);
        let mut gallery = Gallery::new(dir.path().to_owned());
        assert_eq!(gallery.lines(Picture::File("a.png"), 40), None);
    }

    #[test]
    fn a_png_of_the_project_is_drawn_and_wanted_by_the_terminal() {
        let dir = tempfile::tempdir().unwrap();
        png(dir.path(), "a.png", 80, 32);
        std::fs::write(dir.path().join("b.png"), "not a png").unwrap();
        let mut gallery = Gallery::new(dir.path().to_owned());
        gallery.show_in(kitty(), None, None);
        gallery.set_cell((8, 16));
        let lines = gallery.lines(Picture::File("a.png"), 40).unwrap();
        // 80 × 32 pixels: 10 columns, 2 rows.
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].width(), 10);
        let wanted = gallery.take_wanted();
        assert_eq!((wanted[0].cols, wanted[0].rows), (10, 2));
        // The same file keeps its id.
        gallery.lines(Picture::File("a.png"), 40).unwrap();
        assert_eq!(gallery.take_wanted()[0].id, wanted[0].id);
        assert_eq!(gallery.lines(Picture::File("b.png"), 40), None);
        assert_eq!(gallery.lines(Picture::File("none.png"), 40), None);
        // An image the terminal could not take is text again.
        gallery.failed(&[wanted[0].id]);
        assert_eq!(gallery.lines(Picture::File("a.png"), 40), None);
    }

    #[test]
    fn a_diagram_is_drawn_once_and_shown_when_ready() {
        let dir = tempfile::tempdir().unwrap();
        let cache = dir.path().join("cache");
        let renderer = Renderer::find(
            Some(&["draw".into(), "{input}".into(), "{output}".into()]),
            |_| false,
        );
        let mut gallery = Gallery::new(dir.path().to_owned());
        gallery.show_in(kitty(), renderer.clone(), Some(cache.clone()));
        let source = "flowchart LR\n  A --> B";
        let waiting = gallery.lines(Picture::Mermaid(source), 40).unwrap();
        assert_eq!(waiting[0].to_string(), "◌ drawing the diagram…");
        gallery.lines(Picture::Mermaid(source), 40).unwrap();
        let to_draw = gallery.take_to_draw();
        assert_eq!(to_draw.len(), 1);
        let (key, _) = to_draw[0];
        std::fs::create_dir_all(&cache).unwrap();
        png(&cache, "drawn.png", 16, 16);
        gallery.drawn(key, &Ok(cache.join("drawn.png")));
        assert_eq!(
            gallery.lines(Picture::Mermaid(source), 40).unwrap().len(),
            1
        );
        // One that failed stays code.
        gallery.drawn(key, &Err("parse error".into()));
        assert_eq!(gallery.lines(Picture::Mermaid(source), 40), None);
        // Without a renderer, code too.
        let mut bare = Gallery::new(dir.path().to_owned());
        bare.show_in(kitty(), None, Some(cache));
        assert_eq!(bare.lines(Picture::Mermaid(source), 40), None);
    }
}
