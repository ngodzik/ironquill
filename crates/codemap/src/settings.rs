//! What is particular to one project, kept on this machine and never in
//! its repository: the repositories read with it, the service drawn in
//! the middle, and the environments to list first. The code reads only
//! the tools' conventions; a project's own names live here.

use std::path::{Path, PathBuf};

use serde::Deserialize;

/// The settings file of a project: `<base>/projects/<folder>.toml`, the
/// base being ironquill's own folder, `~/.ironquill`.
///
/// # Examples
///
/// ```
/// use std::path::Path;
/// let file = ironquill_codemap::settings_file(Path::new("/home/me/.ironquill"), Path::new("/code/shop"));
/// assert_eq!(file, Path::new("/home/me/.ironquill/projects/shop.toml"));
/// ```
#[must_use]
pub fn settings_file(base: &Path, root: &Path) -> PathBuf {
    base.join("projects")
        .join(format!("{}.toml", folder_name(root)))
}

/// Where what is read of a project is cached: `<base>/cache/<folder>`.
#[must_use]
pub fn cache_folder(base: &Path, root: &Path) -> PathBuf {
    base.join("cache").join(folder_name(root))
}

fn folder_name(root: &Path) -> String {
    root.file_name().map_or_else(
        || "project".to_owned(),
        |n| n.to_string_lossy().into_owned(),
    )
}

/// An environment named in the settings.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NamedEnvironment {
    /// Its name, as shown.
    pub name: String,
    /// Its folder, from the root of the repository that holds it.
    pub path: String,
}

/// A project's own settings.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Settings {
    /// The repositories read with the project, `~` expanded.
    pub linked: Vec<PathBuf>,
    /// The service drawn in the middle.
    pub centre: Option<String>,
    /// The environments listed first, in this order.
    pub environments: Vec<NamedEnvironment>,
    /// What the settings say that could not be used, such as a linked
    /// folder that is not there.
    pub notes: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    #[serde(default)]
    linked: Vec<String>,
    centre: Option<String>,
    #[serde(default)]
    environments: Vec<NamedEnvironment>,
}

/// Why the settings could not be read.
#[derive(Debug, thiserror::Error)]
pub enum SettingsError {
    /// The file is there but could not be read.
    #[error("cannot read {path}")]
    Read {
        /// The file.
        path: PathBuf,
        /// Why.
        #[source]
        source: std::io::Error,
    },
    /// The file is not valid, or names a setting that does not exist.
    #[error("{path} is not valid")]
    Parse {
        /// The file.
        path: PathBuf,
        /// What is wrong, and where.
        #[source]
        source: toml::de::Error,
    },
}

/// Reads the settings in `file`, `~` standing for `home`. No file is no
/// settings.
///
/// # Errors
///
/// When the file cannot be read, is not TOML, or names a setting that
/// does not exist (a typo is said rather than ignored).
pub fn read_settings(file: &Path, home: &Path) -> Result<Settings, SettingsError> {
    let text = match std::fs::read_to_string(file) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Settings::default()),
        Err(source) => {
            return Err(SettingsError::Read {
                path: file.to_owned(),
                source,
            });
        }
    };
    let parsed: File = toml::from_str(&text).map_err(|source| SettingsError::Parse {
        path: file.to_owned(),
        source,
    })?;
    let mut notes = Vec::new();
    let linked = parsed
        .linked
        .iter()
        .map(|l| match l.strip_prefix("~/") {
            Some(rest) => home.join(rest),
            None if l == "~" => home.to_owned(),
            None => PathBuf::from(l),
        })
        .filter(|path| {
            let there = path.is_dir();
            if !there {
                notes.push(format!(
                    "The linked folder {} is not there, so it is not read",
                    path.display()
                ));
            }
            there
        })
        .collect();
    Ok(Settings {
        linked,
        centre: parsed.centre,
        environments: parsed.environments,
        notes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_read_with_home_expanded_and_typos_refused() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        std::fs::create_dir_all(home.join("code/deploy")).unwrap();
        let file = home.join("shop.toml");
        std::fs::write(
            &file,
            "linked = [\"~/code/deploy\", \"~/code/gone\"]\ncentre = \"api\"\nenvironments = [{ name = \"prod\", path = \"clusters/prod/app\" }]\n",
        )
        .unwrap();
        let settings = read_settings(&file, home).unwrap();
        assert_eq!(settings.linked, [home.join("code/deploy")]);
        assert_eq!(settings.centre.as_deref(), Some("api"));
        assert_eq!(settings.environments[0].path, "clusters/prod/app");
        assert_eq!(settings.notes.len(), 1);

        std::fs::write(&file, "centr = \"api\"\n").unwrap();
        let error = read_settings(&file, home).unwrap_err();
        assert!(matches!(error, SettingsError::Parse { .. }));
        assert!(error.to_string().contains("shop.toml"));
        assert_eq!(
            read_settings(&home.join("none.toml"), home).unwrap(),
            Settings::default()
        );
    }
}
