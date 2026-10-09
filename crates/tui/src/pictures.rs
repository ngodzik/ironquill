//! Pictures in replies: PNG files and Mermaid diagrams, drawn where the
//! terminal draws images, else left as the text that names them.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime};

use ratatui::style::{Color, Style};
use ratatui::text::Line;

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

/// How long a diagram may take to draw.
const DRAW_TIMEOUT: Duration = Duration::from_secs(60);

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
    /// Where drawn diagrams are kept: `~/.ironquill/cache/diagrams`.
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

/// The program that draws Mermaid diagrams as PNG images, with its
/// arguments; `{input}` and `{output}` stand for the files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Renderer {
    program: String,
    args: Vec<String>,
}

impl Renderer {
    /// The renderer `configured` (`mermaid` in the configuration), or the
    /// first installed of mermaid-cli and mmdr; `None` when there is none,
    /// or the configuration says `[]`.
    pub(crate) fn find(
        configured: Option<&[String]>,
        installed: impl Fn(&str) -> bool,
    ) -> Option<Self> {
        if let Some(command) = configured {
            let (program, args) = command.split_first()?;
            return Some(Self {
                program: program.clone(),
                args: args.to_vec(),
            });
        }
        let words = |text: &str| text.split(' ').map(str::to_owned).collect();
        if installed("mmdc") {
            // Transparent, drawn for a dark background, at twice the size so
            // that it stays sharp when fitted.
            Some(Self {
                program: "mmdc".into(),
                args: words("-q -i {input} -o {output} -b transparent -t dark -s 2"),
            })
        } else if installed("mmdr") {
            Some(Self {
                program: "mmdr".into(),
                args: words("-i {input} -o {output} -e png"),
            })
        } else {
            None
        }
    }

    /// Whether `program` is a file in one of the directories of `PATH`.
    pub(crate) fn installed(program: &str) -> bool {
        std::env::var_os("PATH")
            .is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join(program).is_file()))
    }

    /// The key of the diagram of `source`: the same for the same source
    /// drawn the same way, from one session to the next.
    fn key(&self, source: &str) -> u64 {
        // FNV-1a: stable across versions, unlike the standard hasher.
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        let parts = std::iter::once(self.program.as_str())
            .chain(self.args.iter().map(String::as_str))
            .chain(std::iter::once(source));
        for part in parts {
            for byte in part.bytes().chain(std::iter::once(0)) {
                hash ^= u64::from(byte);
                hash = hash.wrapping_mul(0x0100_0000_01b3);
            }
        }
        hash
    }

    /// Draws the diagram of `source` into `cache`, waiting for it; the
    /// image, or why it could not be drawn.
    ///
    /// # Errors
    ///
    /// What the renderer said, or that it could not start or took too long.
    pub(crate) fn draw(&self, source: &str, cache: &Path) -> Result<PathBuf, String> {
        let key = self.key(source);
        std::fs::create_dir_all(cache).map_err(|e| format!("{}: {e}", cache.display()))?;
        let name = format!("{key:016x}");
        let input = cache.join(format!("{name}.mmd"));
        let output = cache.join(format!("{name}.part.png"));
        let done = cache.join(format!("{name}.png"));
        let log_path = cache.join(format!("{name}.log"));
        std::fs::write(&input, source).map_err(|e| e.to_string())?;
        let log = File::create(&log_path).map_err(|e| e.to_string())?;
        let args: Vec<String> = self
            .args
            .iter()
            .map(|a| {
                a.replace("{input}", &input.to_string_lossy())
                    .replace("{output}", &output.to_string_lossy())
            })
            .collect();
        let mut child = Command::new(&self.program)
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(log)
            .spawn()
            .map_err(|e| format!("{}: {e}", self.program))?;
        let started = Instant::now();
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if started.elapsed() > DRAW_TIMEOUT => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!("{} took over a minute", self.program));
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(50)),
                Err(e) => return Err(e.to_string()),
            }
        };
        let said = std::fs::read_to_string(&log_path).unwrap_or_default();
        let _ = std::fs::remove_file(&input);
        let _ = std::fs::remove_file(&log_path);
        if status.success() && output.is_file() {
            std::fs::rename(&output, &done).map_err(|e| e.to_string())?;
            return Ok(done);
        }
        let _ = std::fs::remove_file(&output);
        // The line that says what went wrong, else the last one.
        let lines: Vec<&str> = said
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .collect();
        let reason = lines
            .iter()
            .find(|l| l.to_lowercase().contains("error"))
            .or(lines.last())
            .map_or_else(
                || format!("{} failed ({status})", self.program),
                |l| (*l).to_owned(),
            );
        Err(reason)
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
        graphics::detect(crate::defaults::Images::Kitty, |_| None)
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

    #[test]
    fn the_renderer_is_the_configured_one_else_the_first_installed() {
        assert_eq!(Renderer::find(Some(&[]), |_| true), None);
        assert_eq!(Renderer::find(None, |_| false), None);
        assert_eq!(
            Renderer::find(None, |p| p == "mmdr").unwrap().program,
            "mmdr"
        );
        let mmdc = Renderer::find(None, |_| true).unwrap();
        assert_eq!(mmdc.program, "mmdc");
        assert!(mmdc.args.join(" ").contains("-b transparent"));
        // Another source, or another renderer, is another diagram.
        let mmdr = Renderer::find(None, |p| p == "mmdr").unwrap();
        assert_ne!(mmdc.key("a"), mmdc.key("b"));
        assert_ne!(mmdc.key("a"), mmdr.key("a"));
        assert_eq!(mmdc.key("a"), mmdc.key("a"));
    }

    #[cfg(unix)]
    #[test]
    fn draw_runs_the_renderer_and_says_why_it_failed() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("render.sh");
        std::fs::write(
            &script,
            "#!/bin/sh\ncase \"$(cat \"$1\")\" in bad*) echo 'Error: Parse error on line 1' >&2; exit 1;; esac\ncp \"$1\" \"$2\"\n",
        )
        .unwrap();
        let renderer = Renderer {
            program: "sh".into(),
            args: vec![
                script.to_string_lossy().into_owned(),
                "{input}".into(),
                "{output}".into(),
            ],
        };
        let cache = dir.path().join("cache");
        let drawn = renderer.draw("graph", &cache).unwrap();
        assert_eq!(std::fs::read_to_string(&drawn).unwrap(), "graph");
        assert!(drawn.ends_with(format!("{:016x}.png", renderer.key("graph"))));
        assert_eq!(
            renderer.draw("bad", &cache),
            Err("Error: Parse error on line 1".into())
        );
        // Nothing is left behind but the drawn image.
        assert_eq!(std::fs::read_dir(&cache).unwrap().count(), 1);
    }
}
