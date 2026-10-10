//! Where a name is defined, found by `git grep` rather than by a model: the
//! lines that define a function, a class, a type or a constant of that name,
//! in Python, TypeScript, JavaScript and Rust; then ranked by what the file
//! it is looked for from imports, so that the one it means comes first.

use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};

/// The most definitions returned: a name defined more often than this is
/// too common to pick from.
const MOST: usize = 60;

/// The code searched: what the language servers read.
const SOURCES: [&str; 8] = [
    "*.py", "*.pyi", "*.ts", "*.tsx", "*.js", "*.jsx", "*.mjs", "*.rs",
];

/// The most files searched for definitions, of those that hold the name: a
/// name in more is a word everyone uses.
const MAX_FILES: usize = 2_000;

/// A line that defines a name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Definition {
    /// The file, from the project's root.
    pub path: String,
    /// The line, from 1.
    pub line: usize,
    /// The line as written, trimmed.
    pub text: String,
}

/// Where `name` is defined in the project at `root`, the most likely first
/// for code in `from` (from the root) whose text is `lines`: the file
/// itself, then what it imports the name from, then the rest by path.
///
/// # Examples
///
/// ```no_run
/// let lines = vec!["from app.models.asset import AssetModel".to_owned()];
/// let found = ironquill_tools::definitions(
///     std::path::Path::new("."),
///     "AssetModel",
///     std::path::Path::new("app/api/routes.py"),
///     &lines,
/// );
/// if let Some(first) = found.first() {
///     println!("{}:{} {}", first.path, first.line, first.text);
/// }
/// ```
pub fn definitions(root: &Path, name: &str, from: &Path, lines: &[String]) -> Vec<Definition> {
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '$')
    {
        return Vec::new();
    }
    let name_re = regex_escape(name);
    // Keywords that define, then the name, in every language read; or, in
    // Python, the name assigned at the top of a module.
    let pattern = format!(
        r"(^|[^A-Za-z0-9_])(def|class|function|interface|type|enum|const|let|var|fn|struct|trait|mod|static|macro_rules!)[[:space:]]+{name_re}([^A-Za-z0-9_]|$)|^{name_re}[[:space:]]*(:[^=]*)?=([^=]|$)"
    );
    // First the files that hold the name at all, a plain search that is
    // quick even in a large project; then the pattern, in those alone.
    let mut listing = vec!["grep", "-l", "-I", "-w", "-F", "--untracked", name, "--"];
    listing.extend(SOURCES);
    let Some(files) = git(root, &listing) else {
        return Vec::new();
    };
    let files: Vec<&str> = files.lines().take(MAX_FILES).collect();
    if files.is_empty() {
        return Vec::new();
    }
    let mut search = vec!["grep", "-n", "-I", "--untracked", "-E", &pattern, "--"];
    search.extend(files.iter().copied());
    let Some(output) = git(root, &search) else {
        return Vec::new();
    };
    let mut found: Vec<Definition> = output
        .lines()
        .filter_map(|line| {
            let mut parts = line.splitn(3, ':');
            let path = parts.next()?.to_owned();
            let number = parts.next()?.parse().ok()?;
            let text = parts.next()?.trim().to_owned();
            // A use of the name in an expression is not a definition, nor
            // is a line that only mentions it in a comment.
            let code = text.trim_start();
            if code.starts_with('#') || code.starts_with("//") || code.starts_with('*') {
                return None;
            }
            Some(Definition {
                path,
                line: number,
                text,
            })
        })
        .take(MOST * 4)
        .collect();
    let from_text = from.to_string_lossy().replace('\\', "/");
    let imported = imported_from(from, name, lines);
    found.sort_by_key(|d| {
        let rank = if d.path == from_text {
            0
        } else if imported.iter().any(|m| d.path.ends_with(m.as_str())) {
            1
        } else if is_test(&d.path) {
            3
        } else {
            2
        };
        (rank, d.path.matches('/').count(), d.path.clone(), d.line)
    });
    found.truncate(MOST);
    found
}

/// Where `name` is used in the project at `root`, as a word, in its code:
/// what a language server would say when none answers.
///
/// # Examples
///
/// ```no_run
/// for place in ironquill_tools::uses(std::path::Path::new("."), "paginated_select") {
///     println!("{}:{}", place.path, place.line);
/// }
/// ```
pub fn uses(root: &Path, name: &str) -> Vec<Definition> {
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '$')
    {
        return Vec::new();
    }
    let mut search = vec!["grep", "-n", "-I", "-w", "-F", "--untracked", name, "--"];
    search.extend(SOURCES);
    let Some(output) = git(root, &search) else {
        return Vec::new();
    };
    output
        .lines()
        .filter_map(|line| {
            let mut parts = line.splitn(3, ':');
            Some(Definition {
                path: parts.next()?.to_owned(),
                line: parts.next()?.parse().ok()?,
                text: parts.next()?.trim().to_owned(),
            })
        })
        .take(MOST * 4)
        .collect()
}

/// The files `lines`, the text of `from`, imports `name` from, as ends of
/// paths: `app/models/asset.py` for `from app.models.asset import
/// AssetModel`, `src/api.ts` for `import { get } from "./api"` in `src/`.
fn imported_from(from: &Path, name: &str, lines: &[String]) -> Vec<String> {
    let mut found = Vec::new();
    let joined = lines.join("\n");
    // Python: `from a.b import x, y` and its parenthesised form.
    for (at, _) in joined.match_indices("from ") {
        let rest = &joined[at + 5..];
        let Some((module, after)) = rest.split_once(" import ") else {
            continue;
        };
        let module = module.trim();
        if module.contains(char::is_whitespace) || module.is_empty() {
            continue;
        }
        let names: String = if let Some(open) = after.strip_prefix('(') {
            open.split(')').next().unwrap_or_default().to_owned()
        } else {
            after.lines().next().unwrap_or_default().to_owned()
        };
        let listed = names
            .split(',')
            .map(|n| n.split(" as ").next().unwrap_or_default().trim())
            .any(|n| n == name);
        if !listed {
            continue;
        }
        let relative = module.trim_start_matches('.');
        let path = relative.replace('.', "/");
        if module.starts_with('.') {
            // Relative to the importing file's package.
            let folder = from.parent().unwrap_or(Path::new(""));
            let ups = module.len() - relative.len() - 1;
            let mut base: PathBuf = folder.to_owned();
            for _ in 0..ups {
                base.pop();
            }
            let joined = base.join(&path);
            let joined = joined.to_string_lossy().replace('\\', "/");
            found.push(format!("{joined}.py"));
            found.push(format!("{joined}/__init__.py"));
        } else {
            found.push(format!("{path}.py"));
            found.push(format!("{path}/__init__.py"));
        }
    }
    // TypeScript and JavaScript: `import { a, b as c } from "./x"`, and a
    // default import.
    for line in &lines.join("\n").split(';').collect::<Vec<_>>() {
        let line = line.trim();
        if !line.starts_with("import ") && !line.contains("\nimport ") {
            continue;
        }
        let Some((names, module)) = line.rsplit_once(" from ") else {
            continue;
        };
        let module = module.trim().trim_matches(['"', '\'', '`', ';']);
        let listed = names
            .split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '$'))
            .any(|n| n == name);
        if !listed || !module.starts_with('.') {
            continue;
        }
        let folder = from.parent().unwrap_or(Path::new(""));
        let target = normal(&folder.join(module));
        for ending in [
            ".ts",
            ".tsx",
            ".js",
            ".jsx",
            "/index.ts",
            "/index.tsx",
            "/index.js",
        ] {
            found.push(format!("{target}{ending}"));
        }
    }
    found
}

/// A path with its `.` and `..` resolved, in `/` form.
fn normal(path: &Path) -> String {
    let mut parts: Vec<String> = Vec::new();
    for part in path.components() {
        match part {
            Component::ParentDir => {
                parts.pop();
            }
            Component::Normal(p) => parts.push(p.to_string_lossy().into_owned()),
            _ => {}
        }
    }
    parts.join("/")
}

fn is_test(path: &str) -> bool {
    path.split('/')
        .any(|p| p == "tests" || p == "test" || p == "__tests__")
        || path
            .rsplit('/')
            .next()
            .is_some_and(|f| f.starts_with("test_") || f.contains(".test."))
}

/// `name` with what an extended regular expression would read specially
/// escaped: only `$` can be in a name.
fn regex_escape(name: &str) -> String {
    name.replace('$', "\\$")
}

fn git(root: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        .env_remove("GIT_DIR")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_WORK_TREE")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    // No line found is exit 1, with nothing to read.
    Some(String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (path, text) in files {
            let path = dir.path().join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
        let ok = Command::new("git")
            .args(["init", "-q"])
            .current_dir(dir.path())
            .env_remove("GIT_DIR")
            .env_remove("GIT_INDEX_FILE")
            .env_remove("GIT_WORK_TREE")
            .status()
            .unwrap()
            .success();
        assert!(ok);
        dir
    }

    fn lines(text: &str) -> Vec<String> {
        text.lines().map(str::to_owned).collect()
    }

    #[test]
    fn the_definition_imported_comes_before_one_of_the_same_name() {
        let dir = repo(&[
            ("app/models/asset.py", "class AssetModel(Base):\n    pass\n"),
            ("app/other/asset.py", "class AssetModel:\n    pass\n"),
            ("tests/test_asset.py", "class AssetModel:\n    pass\n"),
            (
                "app/api/routes.py",
                "from app.models.asset import AssetModel\n\nx = AssetModel()\n",
            ),
        ]);
        let from = Path::new("app/api/routes.py");
        let text = lines(&std::fs::read_to_string(dir.path().join(from)).unwrap());
        let found = definitions(dir.path(), "AssetModel", from, &text);
        let paths: Vec<&str> = found.iter().map(|d| d.path.as_str()).collect();
        assert_eq!(
            paths,
            [
                "app/models/asset.py",
                "app/other/asset.py",
                "tests/test_asset.py"
            ]
        );
        assert_eq!(found[0].line, 1);
        // A use is not a definition.
        assert!(found.iter().all(|d| !d.text.contains("x = ")));
    }

    #[test]
    fn module_assignments_typescript_and_the_file_itself() {
        let dir = repo(&[
            (
                "params.py",
                "QueryUriExactMatch = Annotated[\n    int,\n]\nX: int = 1\nif X == 1: pass\n",
            ),
            ("ui/api.ts", "export const useAssets = () => 1;\n"),
            (
                "ui/pages/Assets.tsx",
                "import { useAssets } from \"../api\";\nfunction Local() {}\n",
            ),
        ]);
        let found = definitions(dir.path(), "QueryUriExactMatch", Path::new("x.py"), &[]);
        assert_eq!(found.len(), 1);
        assert_eq!((found[0].path.as_str(), found[0].line), ("params.py", 1));
        // `X == 1` compares; `X: int = 1` defines.
        let found = definitions(dir.path(), "X", Path::new("x.py"), &[]);
        assert_eq!(found.iter().map(|d| d.line).collect::<Vec<_>>(), [4]);

        let from = Path::new("ui/pages/Assets.tsx");
        let text = lines(&std::fs::read_to_string(dir.path().join(from)).unwrap());
        let found = definitions(dir.path(), "useAssets", from, &text);
        assert_eq!(found[0].path, "ui/api.ts");
        let found = definitions(dir.path(), "Local", from, &text);
        assert_eq!(found[0].path, "ui/pages/Assets.tsx");
        assert!(definitions(dir.path(), "a b", from, &text).is_empty());
    }
}
