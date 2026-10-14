//! Mermaid diagrams drawn as PNG images by a program of the machine
//! (mermaid-cli, mmdr, or one the settings name), each once, kept in a
//! cache: the terminal and the window show the same files.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// How long a diagram may take to draw.
const DRAW_TIMEOUT: Duration = Duration::from_secs(60);

/// Where drawn diagrams are kept: `~/.universe/cache/diagrams`.
#[must_use]
pub fn cache() -> Option<PathBuf> {
    crate::Defaults::path().map(|p| p.with_file_name("cache").join("diagrams"))
}

/// The program that draws Mermaid diagrams as PNG images, with its
/// arguments; `{input}` and `{output}` stand for the files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Renderer {
    program: String,
    args: Vec<String>,
}

impl Renderer {
    /// The renderer `configured` (`mermaid` in the configuration), or the
    /// first installed of mermaid-cli and mmdr; `None` when there is none,
    /// or the configuration says `[]`.
    pub fn find(configured: Option<&[String]>, installed: impl Fn(&str) -> bool) -> Option<Self> {
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
    pub fn installed(program: &str) -> bool {
        std::env::var_os("PATH")
            .is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join(program).is_file()))
    }

    /// The key of the diagram of `source`: the same for the same source
    /// drawn the same way, from one session to the next.
    pub fn key(&self, source: &str) -> u64 {
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
    pub fn draw(&self, source: &str, cache: &Path) -> Result<PathBuf, String> {
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
