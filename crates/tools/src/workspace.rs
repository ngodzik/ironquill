use std::fs;
use std::path::{Component, Path, PathBuf};

use crate::error::ToolError;

/// The directory a session is allowed to touch, and nothing outside it.
#[derive(Debug, Clone)]
pub struct Workspace {
    root: PathBuf,
}

impl Workspace {
    /// A workspace rooted at `root`, which must exist.
    ///
    /// # Errors
    ///
    /// [`ToolError::Io`] if `root` does not exist or cannot be resolved.
    pub fn new(root: impl AsRef<Path>) -> Result<Self, ToolError> {
        let root = root.as_ref();
        let root = root.canonicalize().map_err(|source| ToolError::Io {
            path: root.to_owned(),
            source,
        })?;
        Ok(Self { root })
    }

    /// The absolute, canonical root.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Turns a path the model wrote into a path inside the workspace.
    ///
    /// Refused: absolute paths, any `..`, and paths that a symbolic link
    /// would carry outside the root. A path that does not exist yet is
    /// accepted when its nearest existing ancestor is inside.
    ///
    /// # Errors
    ///
    /// [`ToolError::OutsideWorkspace`] for any of the refused cases.
    pub fn resolve(&self, relative: &str) -> Result<PathBuf, ToolError> {
        let requested = Path::new(relative);
        let outside = || ToolError::OutsideWorkspace(requested.to_owned());

        // Lexical check first: it is cheap and gives a clear refusal before
        // touching the filesystem.
        if requested
            .components()
            .any(|c| !matches!(c, Component::Normal(_) | Component::CurDir))
        {
            return Err(outside());
        }

        let joined = self.root.join(requested);

        // Then the physical check, because a symbolic link inside the
        // workspace can point anywhere.
        let mut existing = joined.as_path();
        while !existing.exists() {
            existing = existing.parent().ok_or_else(outside)?;
        }
        let real = existing.canonicalize().map_err(|_| outside())?;
        if !real.starts_with(&self.root) {
            return Err(outside());
        }
        Ok(joined)
    }

    /// The path relative to the root, for messages.
    pub(crate) fn display<'a>(&self, path: &'a Path) -> &'a Path {
        path.strip_prefix(&self.root).unwrap_or(path)
    }

    pub(crate) fn read(&self, relative: &str) -> Result<String, ToolError> {
        let path = self.resolve(relative)?;
        let shown = self.display(&path).to_owned();
        let bytes = fs::read(&path).map_err(|source| ToolError::Io {
            path: shown.clone(),
            source,
        })?;
        if is_binary(&bytes) {
            return Err(ToolError::Binary {
                path: shown,
                bytes: bytes.len(),
            });
        }
        String::from_utf8(bytes).map_err(|e| ToolError::Binary {
            path: shown,
            bytes: e.as_bytes().len(),
        })
    }

    pub(crate) fn write(&self, relative: &str, content: &str) -> Result<(), ToolError> {
        if Path::new(relative)
            .components()
            .any(|c| c.as_os_str() == ".git")
        {
            return Err(ToolError::Protected(relative.into()));
        }
        let path = self.resolve(relative)?;
        let io = |source| ToolError::Io {
            path: self.display(&path).to_owned(),
            source,
        };
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(io)?;
        }
        // Write then rename, so that an interrupted session never leaves a
        // half written source file behind.
        let tmp = path.with_extension("ironquill.tmp");
        fs::write(&tmp, content).map_err(io)?;
        fs::rename(&tmp, &path).map_err(io)
    }

    pub(crate) fn replace(&self, relative: &str, old: &str, new: &str) -> Result<(), ToolError> {
        let content = self.read(relative)?;
        let shown = PathBuf::from(relative);
        // An empty pattern matches everywhere, and replacing "all of them" is
        // never what a model meant.
        if old.is_empty() {
            return Err(ToolError::NotFound(shown));
        }
        match content.matches(old).count() {
            0 => Err(ToolError::NotFound(shown)),
            1 => self.write(relative, &content.replacen(old, new, 1)),
            count => Err(ToolError::Ambiguous { path: shown, count }),
        }
    }
}

/// How many leading bytes decide whether a file is binary. The heuristic git
/// itself uses: text files do not contain a NUL byte near their start.
const SNIFF: usize = 8000;

/// Whether `bytes` look like binary data rather than text.
pub(crate) fn is_binary(bytes: &[u8]) -> bool {
    bytes.iter().take(SNIFF).any(|b| *b == 0)
}

/// Whether the file at `path` looks binary, reading only its first bytes.
/// A file that cannot be read counts as binary: there is no point offering
/// it to the model.
pub(crate) fn file_is_binary(path: &Path) -> bool {
    use std::io::Read;
    let mut head = Vec::with_capacity(SNIFF);
    match fs::File::open(path) {
        Ok(file) => file.take(SNIFF as u64).read_to_end(&mut head).is_err() || is_binary(&head),
        Err(_) => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace() -> (tempfile::TempDir, Workspace) {
        let dir = tempfile::tempdir().unwrap();
        let ws = Workspace::new(dir.path()).unwrap();
        (dir, ws)
    }

    #[test]
    fn refuses_escapes() {
        let (_dir, ws) = workspace();
        assert!(ws.resolve("../etc/passwd").is_err());
        assert!(ws.resolve("/etc/passwd").is_err());
        assert!(ws.resolve("src/../../x").is_err());
        assert!(ws.resolve("src/new/file.rs").is_ok());
        assert!(ws.resolve("./a.txt").is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn refuses_a_symlink_that_leads_outside() {
        let (dir, ws) = workspace();
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("link")).unwrap();
        assert!(ws.resolve("link/secret").is_err());
    }

    #[test]
    fn a_binary_file_is_refused_clearly() {
        let (dir, ws) = workspace();
        std::fs::write(dir.path().join("app"), [0x7f, b'E', b'L', b'F', 0, 0, 1]).unwrap();
        assert!(matches!(
            ws.read("app"),
            Err(ToolError::Binary { bytes: 7, .. })
        ));
    }

    #[test]
    fn replace_needs_exactly_one_match() {
        let (_dir, ws) = workspace();
        ws.write("a.txt", "one two two").unwrap();
        assert!(matches!(
            ws.replace("a.txt", "three", "x"),
            Err(ToolError::NotFound(_))
        ));
        assert!(matches!(
            ws.replace("a.txt", "two", "x"),
            Err(ToolError::Ambiguous { count: 2, .. })
        ));
        assert!(matches!(
            ws.replace("a.txt", "", "x"),
            Err(ToolError::NotFound(_))
        ));
        ws.replace("a.txt", "one", "1").unwrap();
        assert_eq!(ws.read("a.txt").unwrap(), "1 two two");
    }
}
