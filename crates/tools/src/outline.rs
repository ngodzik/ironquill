//! A map of the code: the functions, classes and types a file defines,
//! with their line, read by a parser rather than by a model. A model looks
//! at the map first and reads only the lines it needs.

use std::fs;
use std::path::Path;

use ignore::WalkBuilder;
use tree_sitter::{Language, Node, Parser};

/// The most lines of map returned for a directory.
const MAX_LINES: usize = 400;

/// The longest signature shown, in characters.
const MAX_SIGNATURE: usize = 160;

/// The languages the map knows, by file extension.
fn language(path: &Path) -> Option<(Language, &'static Kinds)> {
    let ext = path.extension()?.to_str()?;
    Some(match ext {
        "rs" => (tree_sitter_rust::LANGUAGE.into(), &RUST),
        "py" | "pyi" => (tree_sitter_python::LANGUAGE.into(), &PYTHON),
        "ts" | "mts" | "cts" => (
            tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            &TYPESCRIPT,
        ),
        "tsx" => (tree_sitter_typescript::LANGUAGE_TSX.into(), &TYPESCRIPT),
        "js" | "jsx" | "mjs" | "cjs" => (tree_sitter_javascript::LANGUAGE.into(), &TYPESCRIPT),
        _ => return None,
    })
}

/// Which syntax nodes are definitions, for one language.
struct Kinds {
    /// Shown, and searched inside for more: classes, impls, modules.
    containers: &'static [&'static str],
    /// Shown, not searched inside: functions, methods, type aliases.
    leaves: &'static [&'static str],
    /// Shown at the top level only when they hold a function, as
    /// `const f = () => …` does.
    bindings: &'static [&'static str],
}

const RUST: Kinds = Kinds {
    containers: &["impl_item", "trait_item", "mod_item"],
    leaves: &[
        "function_item",
        "function_signature_item",
        "struct_item",
        "enum_item",
        "union_item",
        "type_item",
        "const_item",
        "static_item",
        "macro_definition",
    ],
    bindings: &[],
};

const PYTHON: Kinds = Kinds {
    containers: &["class_definition"],
    leaves: &["function_definition"],
    bindings: &[],
};

const TYPESCRIPT: Kinds = Kinds {
    containers: &[
        "class_declaration",
        "abstract_class_declaration",
        "interface_declaration",
        "internal_module",
        "module",
    ],
    leaves: &[
        "function_declaration",
        "generator_function_declaration",
        "method_definition",
        "method_signature",
        "abstract_method_signature",
        "type_alias_declaration",
        "enum_declaration",
    ],
    bindings: &["lexical_declaration", "variable_declaration"],
};

/// The map of one file, `None` when its language is not known.
pub(crate) fn outline_file(path: &Path) -> Option<String> {
    let (language, kinds) = language(path)?;
    let source = fs::read_to_string(path).ok()?;
    let mut parser = Parser::new();
    parser.set_language(&language).ok()?;
    let tree = parser.parse(&source, None)?;
    let mut out = Vec::new();
    walk(tree.root_node(), &source, kinds, 0, &mut out);
    Some(out.join("\n"))
}

fn walk(node: Node, source: &str, kinds: &Kinds, depth: usize, out: &mut Vec<String>) {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        let kind = child.kind();
        let binding = depth == 0
            && kinds.bindings.contains(&kind)
            && child
                .utf8_text(source.as_bytes())
                .is_ok_and(|t| t.contains("=>") || t.contains("function"));
        if kinds.leaves.contains(&kind) || binding {
            out.push(signature(child, source, depth));
        } else if kinds.containers.contains(&kind) {
            out.push(signature(child, source, depth));
            walk(child, source, kinds, depth + 1, out);
        } else {
            // Wrappers such as `export`, decorators, class bodies.
            walk(child, source, kinds, depth, out);
        }
    }
}

/// `L12  def login(user):`, the first line of the definition.
fn signature(node: Node, source: &str, depth: usize) -> String {
    let text = node.utf8_text(source.as_bytes()).unwrap_or_default();
    let first = text.lines().next().unwrap_or_default().trim();
    let first = first.trim_end_matches('{').trim_end();
    let first: String = first.chars().take(MAX_SIGNATURE).collect();
    format!(
        "L{:<5}{}{first}",
        node.start_position().row + 1,
        "  ".repeat(depth)
    )
}

/// The map of every file of a known language under `dir`, skipping what
/// git ignores, as `path` then its definitions.
pub(crate) fn outline_dir(root: &Path, dir: &Path) -> String {
    let mut paths: Vec<_> = WalkBuilder::new(dir)
        .hidden(true)
        .git_ignore(true)
        .require_git(false)
        .build()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_some_and(|t| t.is_file()))
        .map(ignore::DirEntry::into_path)
        .filter(|p| language(p).is_some())
        .collect();
    paths.sort();

    let mut out: Vec<String> = Vec::new();
    let mut left = 0;
    for path in &paths {
        let Some(map) = outline_file(path) else {
            continue;
        };
        if map.is_empty() {
            continue;
        }
        let lines = map.lines().count() + 1;
        if out.len() + lines > MAX_LINES {
            left += 1;
            continue;
        }
        out.push(
            path.strip_prefix(root)
                .unwrap_or(path)
                .display()
                .to_string(),
        );
        out.extend(map.lines().map(|l| format!("  {l}")));
    }
    if left > 0 {
        out.push(format!(
            "({left} more files not shown: ask for a smaller directory or one file)"
        ));
    }
    if out.is_empty() {
        "no Rust, Python, TypeScript or JavaScript definitions here: use search".into()
    } else {
        out.join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(name: &str, source: &str) -> String {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(name);
        fs::write(&path, source).unwrap();
        outline_file(&path).unwrap()
    }

    #[test]
    fn python_classes_and_functions() {
        let source = "import os\n\nclass Store:\n    def add(self, x):\n        def inner():\n            pass\n        return x\n\n@cache\ndef load(path):\n    return 1\n";
        assert_eq!(
            map("a.py", source),
            "L3    class Store:\nL4      def add(self, x):\nL10   def load(path):"
        );
    }

    #[test]
    fn typescript_functions_classes_and_arrows() {
        let source = "export interface User {\n  name: string;\n}\nexport class Api {\n  get(id: number) {\n    return id;\n  }\n}\nexport const load = async (id: number) => {\n  return id;\n};\nconst N = 3;\nfunction main() {}\n";
        assert_eq!(
            map("a.ts", source),
            "L1    interface User\nL4    class Api\nL5      get(id: number)\nL9    const load = async (id: number) =>\nL13   function main() {}"
        );
    }

    #[test]
    fn rust_items_and_impls() {
        let source = "pub struct A;\n\nimpl A {\n    pub fn new() -> Self {\n        A\n    }\n}\n\nfn main() {}\n";
        assert_eq!(
            map("a.rs", source),
            "L1    pub struct A;\nL3    impl A\nL4      pub fn new() -> Self\nL9    fn main() {}"
        );
    }

    #[test]
    fn a_directory_maps_known_languages_only() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("app")).unwrap();
        fs::write(root.join("app/a.py"), "def f():\n    pass\n").unwrap();
        fs::write(root.join("app/notes.md"), "# def g()\n").unwrap();
        assert_eq!(outline_dir(root, root), "app/a.py\n  L1    def f():");
        assert!(outline_dir(root, &root.join("nothing")).contains("use search"));
    }
}
