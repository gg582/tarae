//! Tests — run the test at the cursor · this file's tests (test pane below), debug them (debug pane),
//! rerun the last one. Languages: Rust (`cargo test`) · Go (`go test`) · Python (`pytest`). Finding goes
//! **through the tree-sitter tree** — if the grammar is missing, show a download offer and resume the
//! intended work once downloaded (`offer::Resume`). Walks up from the node around the cursor collecting
//! test functions (Rust `#[test]`-like attributes · Go `TestXxx` · Python `test…`) and their enclosers
//! (`mod`·`class`) — if the cursor isn't in a test, the enclosing test module (Rust `mod`·Python
//! `class Test…`), else the whole file.
//!
//! Runs on three worker threads (read stdout·stderr, wait) — editing isn't blocked. Launched in a new session
//! (setsid) so closing kills the whole process group (including the test binaries cargo spawned).

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tree_sitter::{Node, Tree};

use crate::editor::Editor;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    /// The test at the cursor (else the enclosing module, else the file).
    Nearest,
    File,
}

/// One test (or group) to run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    /// Display name (`editor::tests::undo` · `TestParse` · `tests/test_x.py::TestA::test_b`).
    pub label: String,
    pub cwd: PathBuf,
    pub how: How,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum How {
    /// `cargo test [--test t | --bin b] -- <filter> [--exact]` — `crate_root` = the target's root file (for
    /// picking the test binary to debug: the artifact's `target.src_path`).
    Rust { filter: String, exact: bool, flag: Vec<String>, crate_root: PathBuf },
    /// `go test -v -run <regex> .` (`-bench` for benchmarks) — folder = package.
    Go { pattern: String, bench: bool },
    /// `<python> -m pytest <node>` (the venv's python if there is one).
    Python { python: String, node: String },
    /// Gradle `./gradlew :module:test --tests pkg.Class.method` ·
    /// Maven `./mvnw -pl module -Dtest=Class#method test`.
    /// `debug` = make the test JVM wait for a debugger (Gradle `--debug-jvm` ·
    /// surefire `-Dmaven.surefire.debug`, 5005).
    /// `module_dir` = module folder in which to find reports (JUnit XML)·sources.
    Java { tool: String, gradle: bool, module: String, module_dir: PathBuf, filter: String, debug: bool },
}

impl Target {
    /// Command to run (argv).
    pub fn argv(&self) -> Vec<String> {
        let s = |x: &str| x.to_string();
        match &self.how {
            How::Rust { filter, exact, flag, .. } => {
                let mut v = vec![s("cargo"), s("test")];
                v.extend(flag.iter().cloned());
                v.push(s("--"));
                if !filter.is_empty() {
                    v.push(filter.clone());
                }
                if *exact {
                    v.push(s("--exact"));
                }
                v
            }
            How::Go { pattern, bench: false } => {
                vec![s("go"), s("test"), s("-v"), s("-run"), pattern.clone(), s(".")]
            }
            How::Go { pattern, bench: true } => {
                vec![s("go"), s("test"), s("-v"), s("-run"), s("^$"), s("-bench"), pattern.clone(), s(".")]
            }
            How::Python { python, node } => {
                // -v = one line per test · --tb=short = only the failure location and E lines
                // (read by the results pane — test_results.rs)
                vec![
                    python.clone(),
                    s("-m"),
                    s("pytest"),
                    node.clone(),
                    s("-v"),
                    s("--tb=short"),
                    s("--color=no"),
                ]
            }
            How::Java { tool, gradle: true, module, filter, debug, .. } => {
                let task = if module.is_empty() { s("test") } else { format!("{module}:test") };
                let mut v = vec![tool.clone(), task, s("--rerun"), s("--console=plain")];
                if !filter.is_empty() {
                    v.extend([s("--tests"), filter.clone()]);
                }
                if *debug {
                    v.push(s("--debug-jvm"));
                }
                v
            }
            How::Java { tool, gradle: false, module, filter, debug, .. } => {
                let mut v = vec![tool.clone(), s("-B"), s("test")];
                if !module.is_empty() {
                    v.extend([s("-pl"), module.clone()]);
                }
                if !filter.is_empty() {
                    v.extend([format!("-Dtest={filter}"), s("-Dsurefire.failIfNoSpecifiedTests=false")]);
                }
                if *debug {
                    v.push(s("-Dmaven.surefire.debug"));
                }
                v
            }
        }
    }
}

fn lang_of(how: &How) -> &'static str {
    match how {
        How::Rust { .. } => "rust",
        How::Go { .. } => "go",
        How::Python { .. } => "python",
        How::Java { .. } => "java",
    }
}

/// Can this language's tests be run (before finding — when asking whether to download the grammar).
pub fn supported(lang: &str) -> Result<(), String> {
    match lang {
        "rust" | "go" | "python" | "java" => Ok(()),
        "" => Err("not a source file".into()),
        l => Err(format!("no test runner for {l} yet")),
    }
}

/// The test to run for this file·cursor (byte) — `tree` is `src` parsed in that language.
pub fn find(
    lang: &str,
    path: &Path,
    src: &str,
    tree: &Tree,
    cursor: usize,
    scope: Scope,
) -> Result<Target, String> {
    supported(lang)?;
    let at = tree.root_node().descendant_for_byte_range(cursor.min(src.len()), cursor.min(src.len()));
    let scopes = at.map(|n| enclosing(lang, n, src)).unwrap_or_default();
    match lang {
        "rust" => rust(path, &scopes, scope),
        "go" => go(path, tree, src, &scopes, scope),
        "java" => java(path, tree, src, &scopes, scope),
        _ => python(path, &scopes, scope),
    }
}

/// One thing enclosing the cursor — outer → inner.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Scoped {
    /// 'm' module (Rust `mod`) · 'c' class · 'f' function.
    kind: char,
    name: String,
    test: bool,
}

fn name_of(n: Node, src: &str) -> String {
    n.child_by_field_name("name")
        .and_then(|x| x.utf8_text(src.as_bytes()).ok())
        .unwrap_or_default()
        .to_string()
}

/// If the cursor is on a function's leading attribute (`#[test]`·`@pytest…`), use that function.
fn decorated_item(n: Node) -> Option<Node> {
    let mut attr = n;
    while attr.kind() != "attribute_item" && attr.kind() != "decorator" {
        attr = attr.parent()?;
    }
    let mut next = attr.next_named_sibling();
    while let Some(x) = next {
        match x.kind() {
            "attribute_item" | "line_comment" | "block_comment" | "decorator" => {
                next = x.next_named_sibling()
            }
            _ => return Some(x),
        }
    }
    None
}

/// Walks from the cursor up to the root collecting modules·classes·functions (outer → inner).
fn enclosing(lang: &str, at: Node, src: &str) -> Vec<Scoped> {
    let mut out = Vec::new();
    let mut node = decorated_item(at).or(Some(at));
    while let Some(n) = node {
        match (lang, n.kind()) {
            ("rust", "function_item") => {
                out.push(Scoped { kind: 'f', name: name_of(n, src), test: rust_is_test(n, src) });
            }
            ("rust", "mod_item") => out.push(Scoped { kind: 'm', name: name_of(n, src), test: false }),
            ("go", "function_declaration") => {
                let name = name_of(n, src);
                out.push(Scoped { kind: 'f', test: go_test_name(&name), name });
            }
            ("python", "function_definition") => {
                let name = name_of(n, src);
                out.push(Scoped { kind: 'f', test: name.starts_with("test"), name });
            }
            ("python", "class_definition") => {
                out.push(Scoped { kind: 'c', name: name_of(n, src), test: false })
            }
            ("java", "method_declaration") => {
                out.push(Scoped { kind: 'f', name: name_of(n, src), test: java_is_test(n, src) })
            }
            ("java", "class_declaration") => {
                out.push(Scoped { kind: 'c', name: name_of(n, src), test: false })
            }
            _ => {}
        }
        node = n.parent();
    }
    out.reverse();
    out
}

fn up_to(start: &Path, found: impl Fn(&Path) -> bool) -> Option<PathBuf> {
    let mut dir = start.parent()?.to_path_buf();
    loop {
        if found(&dir) {
            return Some(dir);
        }
        if !dir.pop() {
            return None;
        }
    }
}

// ── Rust ─────────────────────────────────────────────────────────────────

/// Whether the function's immediately preceding siblings (skipping comments) include a test attribute.
fn rust_is_test(f: Node, src: &str) -> bool {
    let mut prev = f.prev_named_sibling();
    while let Some(p) = prev {
        match p.kind() {
            "attribute_item" => {
                let text = p.utf8_text(src.as_bytes()).unwrap_or_default();
                let inner = text.trim_start_matches("#[").trim_end_matches(']');
                if is_test_attr(inner) {
                    return true;
                }
            }
            "line_comment" | "block_comment" => {}
            _ => return false,
        }
        prev = p.prev_named_sibling();
    }
    false
}

/// `#[test]` · `#[tokio::test]` · `#[rstest]` · `#[test_case(…)]` · `#[async_std::test]` …
fn is_test_attr(attr: &str) -> bool {
    let head = attr.split(['(', ' ', '=']).next().unwrap_or_default().trim();
    let last = head.rsplit("::").next().unwrap_or_default();
    matches!(last, "test" | "rstest" | "test_case" | "quickcheck" | "proptest")
}

/// (package root, target root file, cargo target args, the file's module path).
fn rust_crate(path: &Path) -> Result<(PathBuf, PathBuf, Vec<String>, Vec<String>), String> {
    let root = up_to(path, |d| d.join("Cargo.toml").is_file()).ok_or("not in a Cargo project")?;
    let rel = path.strip_prefix(&root).map_err(|_| "file outside its package")?;
    let parts: Vec<String> = rel.iter().map(|c| c.to_string_lossy().into_owned()).collect();
    let stem = |s: &str| s.trim_end_matches(".rs").to_string();
    match parts.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        // tests/foo.rs · tests/foo/main.rs = integration test target foo
        ["tests", f] => Ok((root.clone(), path.to_path_buf(), vec!["--test".into(), stem(f)], Vec::new())),
        ["tests", dir, rest @ ..] => {
            let crate_root = root.join("tests").join(dir).join("main.rs");
            let mods = module_path(rest);
            Ok((root.clone(), crate_root, vec!["--test".into(), dir.to_string()], mods))
        }
        ["src", "bin", f] => {
            Ok((root.clone(), path.to_path_buf(), vec!["--bin".into(), stem(f)], Vec::new()))
        }
        // src/bin/tool/main.rs (+ its modules) = binary target tool
        ["src", "bin", dir, rest @ ..] => {
            let crate_root = root.join("src/bin").join(dir).join("main.rs");
            Ok((root.clone(), crate_root, vec!["--bin".into(), dir.to_string()], module_path(rest)))
        }
        ["src", rest @ ..] => {
            let lib = root.join("src/lib.rs");
            let crate_root =
                if lib.is_file() && rest != ["main.rs"] { lib } else { root.join("src/main.rs") };
            let flag = if crate_root.ends_with("lib.rs") { vec!["--lib".to_string()] } else { Vec::new() };
            Ok((root.clone(), crate_root, flag, module_path(rest)))
        }
        _ => Err("tests live under src/ or tests/".into()),
    }
}

/// `a/b.rs` → [a, b] · `a/mod.rs` → [a] · `lib.rs`·`main.rs` → [].
fn module_path(rest: &[&str]) -> Vec<String> {
    let mut v: Vec<String> = rest.iter().map(|s| s.trim_end_matches(".rs").to_string()).collect();
    if v.len() == 1 && (v[0] == "lib" || v[0] == "main") {
        v.clear();
    }
    if v.last().is_some_and(|l| l == "mod") {
        v.pop();
    }
    v
}

fn rust(path: &Path, scopes: &[Scoped], scope: Scope) -> Result<Target, String> {
    let (root, crate_root, flag, mut mods) = rust_crate(path)?;
    let make = |filter: String, exact: bool, label: String| Target {
        label,
        cwd: root.clone(),
        how: How::Rust { filter, exact, flag: flag.clone(), crate_root: crate_root.clone() },
    };
    let prefix =
        |mods: &[String]| if mods.is_empty() { String::new() } else { format!("{}::", mods.join("::")) };
    let label = |mods: &[String]| {
        if mods.is_empty() { "all tests".to_string() } else { format!("{}::*", mods.join("::")) }
    };
    if scope == Scope::File {
        return Ok(make(prefix(&mods), false, label(&mods)));
    }
    // Modules down to the innermost test function — else down to the innermost module
    let test_at = scopes.iter().rposition(|s| s.test);
    let upto =
        test_at.map_or_else(|| scopes.iter().rposition(|s| s.kind == 'm').map_or(0, |i| i + 1), |i| i + 1);
    mods.extend(scopes[..upto].iter().filter(|s| s.kind == 'm').map(|s| s.name.clone()));
    match test_at {
        Some(i) => {
            let full = mods.iter().cloned().chain([scopes[i].name.clone()]).collect::<Vec<_>>().join("::");
            Ok(make(full.clone(), true, full))
        }
        None => Ok(make(prefix(&mods), false, label(&mods))),
    }
}

/// Builds the test binary with `cargo test --no-run` and returns its path (on a worker thread).
pub fn cargo_test_binary(cwd: &Path, flag: &[String], crate_root: &Path) -> Result<PathBuf, String> {
    let root = std::fs::canonicalize(crate_root).unwrap_or_else(|_| crate_root.to_path_buf());
    let mut args = vec!["test", "--no-run"];
    args.extend(flag.iter().map(String::as_str));
    let pick = |v: &Value| {
        let src = v["target"]["src_path"].as_str().map(PathBuf::from);
        v["profile"]["test"] == true && src.is_some_and(|s| std::fs::canonicalize(&s).unwrap_or(s) == root)
    };
    crate::dap::cargo_artifact(&args, cwd, pick)?.ok_or_else(|| "no test binary for this file".to_string())
}

// ── Go ───────────────────────────────────────────────────────────────────

/// `TestXxx` · `BenchmarkXxx` · `FuzzXxx` · `ExampleXxx` (not if a lowercase letter follows the prefix —
/// go test rule).
fn go_test_name(name: &str) -> bool {
    ["Test", "Benchmark", "Fuzz", "Example"]
        .iter()
        .find_map(|p| name.strip_prefix(p))
        .is_some_and(|tail| tail.chars().next().is_none_or(|c| !c.is_lowercase()))
}

fn go(path: &Path, tree: &Tree, src: &str, scopes: &[Scoped], scope: Scope) -> Result<Target, String> {
    if !path.to_string_lossy().ends_with("_test.go") {
        return Err("Go tests live in *_test.go files".into());
    }
    let cwd = path.parent().ok_or("no folder")?.to_path_buf();
    let root = tree.root_node();
    let mut walk = root.walk();
    let all: Vec<String> = root
        .named_children(&mut walk)
        .filter(|n| n.kind() == "function_declaration")
        .map(|n| name_of(n, src))
        .filter(|n| n.starts_with("Test") && go_test_name(n))
        .collect();
    let file = || {
        if all.is_empty() {
            return Err("no Test functions in this file".to_string());
        }
        Ok(Target {
            label: path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
            cwd: cwd.clone(),
            how: How::Go { pattern: format!("^({})$", all.join("|")), bench: false },
        })
    };
    match scopes.iter().find(|s| s.test).filter(|_| scope == Scope::Nearest) {
        Some(f) => Ok(Target {
            label: f.name.clone(),
            cwd,
            how: How::Go { pattern: format!("^{}$", f.name), bench: f.name.starts_with("Benchmark") },
        }),
        None => file(),
    }
}

// ── Python ───────────────────────────────────────────────────────────────

/// Project root (where the pytest config is, else the repo root, else the file's folder).
fn py_root(path: &Path) -> PathBuf {
    const MARKS: &[&str] = &["pyproject.toml", "pytest.ini", "setup.cfg", "tox.ini", "setup.py"];
    up_to(path, |d| MARKS.iter().any(|m| d.join(m).is_file()))
        .or_else(|| up_to(path, |d| d.join(".git").exists()))
        .unwrap_or_else(|| path.parent().unwrap_or(path).to_path_buf())
}

/// The venv's python if any (`.venv`·`venv`·`$VIRTUAL_ENV`), else python3.
pub fn py_interpreter(root: &Path) -> String {
    let venv = std::env::var_os("VIRTUAL_ENV").map(PathBuf::from);
    [root.join(".venv"), root.join("venv")]
        .into_iter()
        .chain(venv)
        .map(|v| v.join("bin/python"))
        .find(|p| p.is_file())
        .map_or_else(|| "python3".to_string(), |p| p.display().to_string())
}

fn python(path: &Path, scopes: &[Scoped], scope: Scope) -> Result<Target, String> {
    let root = py_root(path);
    let rel = path.strip_prefix(&root).unwrap_or(path).display().to_string();
    let make = |node: String| Target {
        label: node.clone(),
        cwd: root.clone(),
        how: How::Python { python: py_interpreter(&root), node },
    };
    if scope == Scope::File {
        return Ok(make(rel));
    }
    // The outermost test… function (not a helper inside it) · the class around it — outside a test,
    // the Test… class
    let func = scopes.iter().position(|s| s.kind == 'f' && s.test);
    let class = match func {
        Some(i) => scopes[..i].iter().rev().find(|s| s.kind == 'c'),
        None => scopes.iter().rev().find(|s| s.kind == 'c' && s.name.starts_with("Test")),
    };
    let node = [Some(rel), class.map(|c| c.name.clone()), func.map(|i| scopes[i].name.clone())]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join("::");
    Ok(make(node))
}

// ── Java ─────────────────────────────────────────────────────────────────

/// Whether the method's annotations (in `modifiers`) include a JUnit 4·5·TestNG test annotation.
fn java_is_test(m: Node, src: &str) -> bool {
    let mut walk = m.walk();
    let Some(mods) = m.named_children(&mut walk).find(|c| c.kind() == "modifiers") else { return false };
    let mut w = mods.walk();
    mods.named_children(&mut w).any(|a| {
        matches!(a.kind(), "marker_annotation" | "annotation")
            && matches!(
                name_of(a, src).rsplit('.').next().unwrap_or_default(),
                "Test" | "ParameterizedTest" | "RepeatedTest" | "TestFactory" | "TestTemplate"
            )
    })
}

/// Runs via the build tool — root = topmost build file in the repo (top of a multi-module build),
/// module = the one nearest the file.
fn java(path: &Path, tree: &Tree, src: &str, scopes: &[Scoped], scope: Scope) -> Result<Target, String> {
    let root = crate::java::project_root(path).ok_or("no pom.xml or build.gradle above this file")?;
    let module_dir =
        up_to(path, |d| crate::java::BUILD_FILES.iter().any(|f| d.join(f).is_file())).unwrap_or(root.clone());
    let rel: Vec<String> = module_dir
        .strip_prefix(&root)
        .map(|r| r.iter().map(|c| c.to_string_lossy().into_owned()).collect())
        .unwrap_or_default();
    let gradle = !root.join("pom.xml").is_file();
    let tool = match (gradle, root.join("gradlew").is_file(), root.join("mvnw").is_file()) {
        (true, true, _) => root.join("gradlew").display().to_string(),
        (true, false, _) => "gradle".into(),
        (false, _, true) => root.join("mvnw").display().to_string(),
        (false, _, false) => "mvn".into(),
    };
    let module = match (gradle, rel.is_empty()) {
        (_, true) => String::new(),
        (true, false) => format!(":{}", rel.join(":")),
        (false, false) => rel.join("/"),
    };
    // package · class (nested with $) · method
    let mut walk = tree.root_node().walk();
    let package = tree
        .root_node()
        .named_children(&mut walk)
        .find(|n| n.kind() == "package_declaration")
        .and_then(|p| p.named_child(0))
        .and_then(|n| n.utf8_text(src.as_bytes()).ok())
        .map(|p| format!("{p}."))
        .unwrap_or_default();
    let method = scopes.iter().rposition(|s| s.kind == 'f' && s.test).filter(|_| scope == Scope::Nearest);
    let classes: Vec<&str> = match (method, scope) {
        (Some(i), _) => scopes[..i].iter().filter(|s| s.kind == 'c').map(|s| s.name.as_str()).collect(),
        (None, Scope::Nearest) => scopes.iter().filter(|s| s.kind == 'c').map(|s| s.name.as_str()).collect(),
        (None, Scope::File) => Vec::new(),
    };
    let classes = if classes.is_empty() {
        // The file's outermost class
        let mut w = tree.root_node().walk();
        let top = tree
            .root_node()
            .named_children(&mut w)
            .find(|n| n.kind() == "class_declaration")
            .map(|n| name_of(n, src));
        vec![top.ok_or("no test class in this file")?]
    } else {
        classes.iter().map(|c| c.to_string()).collect()
    };
    let class = classes.join("$");
    let (filter, label) = match method {
        Some(i) => {
            let m = &scopes[i].name;
            let sep = if gradle { "." } else { "#" };
            (format!("{package}{class}{sep}{m}"), format!("{}.{m}", classes.join(".")))
        }
        None => (format!("{package}{class}"), classes.join(".")),
    };
    Ok(Target { label, cwd: root, how: How::Java { tool, gradle, module, module_dir, filter, debug: false } })
}

// ── Running ──────────────────────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RunState {
    Running,
    Passed,
    Failed(Option<i32>),
}

pub struct TestRun {
    pub target: Target,
    pub state: RunState,
    /// (is stderr, line).
    pub output: Vec<(bool, String)>,
    pub started: Instant,
    pub took: Option<Duration>,
    pid: Option<u32>,
    generation: u64,
    /// Java test debug: attach once the test JVM prints
    /// "Listening for transport dt_socket at address: N" (file, root).
    java_attach: Option<(PathBuf, PathBuf)>,
    /// Per-test results (parsed per line — Java: JUnit XML after finishing) · the one picked in the
    /// results pane.
    pub cases: Vec<crate::test_results::Case>,
    parser: crate::test_results::Parser,
    pub selected: usize,
}

impl TestRun {
    /// Indices of the failed ones.
    pub fn failures(&self) -> Vec<usize> {
        use crate::test_results::Status;
        self.cases.iter().enumerate().filter(|(_, c)| c.status == Status::Failed).map(|(i, _)| i).collect()
    }
}

impl Drop for TestRun {
    fn drop(&mut self) {
        self.kill();
    }
}

impl TestRun {
    fn kill(&mut self) {
        if let Some(pid) = self.pid.take()
            && self.state == RunState::Running
        {
            crate::dap::term_group(pid);
        }
    }
}

/// Strips color codes (`ESC [ … letter`).
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c == '\x1b' && it.peek() == Some(&'[') {
            it.next();
            for d in it.by_ref() {
                if d.is_ascii_alphabetic() {
                    break;
                }
            }
        } else if c != '\r' {
            out.push(c);
        }
    }
    out
}

impl Editor {
    /// Test for the current file·cursor — if the grammar is missing, shows a download offer and returns None
    /// (`resume` continues once downloaded).
    fn test_target(&mut self, scope: Scope, resume: crate::offer::Resume) -> Option<Target> {
        let doc = self.doc();
        let Some(path) = doc.path.clone() else {
            self.set_status("save the file first");
            return None;
        };
        let spec = crate::syntax::detect(&path);
        if let Err(e) = supported(spec.map_or("", |s| s.name.as_str())) {
            self.set_status(e);
            return None;
        }
        let spec = spec.expect("supported");
        let lang = match crate::syntax::Loader::global().load(spec) {
            Ok(l) => l,
            Err(_) => {
                self.offer_grammar_then(spec, resume);
                return None;
            }
        };
        // Parse the current text as-is (the doc's tree may lag a beat behind edits) — once per press, few ms
        let src = doc.text.to_string();
        let cursor = doc.selection().primary().cursor(&doc.text);
        let mut parser = tree_sitter::Parser::new();
        let tree = parser.set_language(&lang.language).ok().and_then(|_| parser.parse(&src, None));
        let Some(tree) = tree else {
            self.set_error(format!("test: couldn't parse {}", spec.name));
            return None;
        };
        match find(&spec.name, &path, &src, &tree, cursor, scope) {
            Ok(t) => Some(t),
            Err(e) => {
                self.set_status(e);
                None
            }
        }
    }

    /// `space x x` · `space x f`.
    pub fn test_run(&mut self, scope: Scope) {
        if let Some(t) = self.test_target(scope, crate::offer::Resume::Test(scope)) {
            self.test_start(t);
        }
    }

    /// `space x l` — rerun the last one (as debug if it was debug).
    pub fn test_rerun(&mut self) {
        match self.test_last.clone() {
            Some((t, false)) => self.test_start(t),
            Some((t, true)) => self.test_debug_target(t),
            None => self.set_status("no test run yet — space x x runs the test at the cursor"),
        }
    }

    /// `]x` · `[x` — go to a failed test's location (first the picked one; if already there, next·prev).
    /// The results pane picks it too.
    pub fn test_failure_step(&mut self, forward: bool) {
        let here = self.doc().path.clone().map(|p| {
            let d = self.doc();
            (p, d.text.byte_to_line(d.selection().primary().cursor(&d.text)))
        });
        let Some(r) = self.test_run.as_mut() else {
            return self.set_status("no test results — space x x runs a test");
        };
        let fails = r.failures();
        if fails.is_empty() {
            return self.set_status("no failed tests");
        }
        let at_selected = r.cases.get(r.selected).and_then(|c| c.at.clone()).is_some_and(|a| Some(a) == here);
        let pos = fails.iter().position(|&i| i == r.selected);
        let next = match (pos, at_selected) {
            (Some(p), true) => fails[(p + if forward { 1 } else { fails.len() - 1 }) % fails.len()],
            (Some(p), false) => fails[p],
            (None, _) => fails[0],
        };
        r.selected = next;
        let case = r.cases[next].clone();
        let n = fails.iter().position(|&i| i == next).unwrap_or(0) + 1;
        if let Some((path, line)) = case.at {
            if let Err(e) = self.open(&path) {
                return self.set_error(format!("{e:#}"));
            }
            let doc = self.doc_mut();
            let line = line.min(crate::movement::last_line(&doc.text));
            let at = crate::movement::line_start(&doc.text, line);
            doc.set_selection(crate::selection::Selection::point(at));
        }
        let msg = case.message.first().map(|m| format!(" — {}", m.trim())).unwrap_or_default();
        self.note(format!("failure {n}/{} · {}{msg}", fails.len(), case.name));
    }

    /// `space x c` — closes the test pane (stops the run if it's going).
    pub fn test_close(&mut self) {
        self.test_run = None;
    }

    pub fn test_start(&mut self, target: Target) {
        // Save modified files first — tests run the code on disk
        self.auto_save(" · before test");
        self.test_last = Some((target.clone(), false));
        let argv = target.argv();
        let mut cmd = Command::new(&argv[0]);
        cmd.args(&argv[1..])
            .current_dir(&target.cwd)
            .env("NO_COLOR", "1")
            .env("CARGO_TERM_COLOR", "never")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let wall = std::time::SystemTime::now();
        let mut child = match crate::dap::new_session(&mut cmd).spawn() {
            Ok(c) => c,
            // (the previous run, if any, stays current — its lines·end still land)
            Err(e) => return self.set_error(format!("test: {}: {e}", argv[0])),
        };
        self.test_generation += 1;
        let generation = self.test_generation;
        let pid = child.id();
        let (stdout, stderr) = (child.stdout.take(), child.stderr.take());
        let tx = self.events.sender();
        let send_line = move |tx: &std::sync::mpsc::Sender<crate::event::Event>, err: bool, line: String| {
            let apply = move |ed: &mut Editor| {
                let Some(r) = ed.test_run.as_mut().filter(|r| r.generation == generation) else { return };
                let line = strip_ansi(&line);
                // Java test debug: the test JVM waits for a debugger — attach to that port
                let listen = line
                    .split_once("Listening for transport dt_socket at address:")
                    .and_then(|(_, a)| a.trim().rsplit(':').next()?.parse::<u16>().ok());
                let attach =
                    listen.and_then(|port| r.java_attach.take().map(|a| (a, port, r.target.label.clone())));
                r.parser.line(&r.target.cwd, &line, &mut r.cases);
                r.output.push((err, line));
                let n = r.output.len();
                if n > 5000 {
                    r.output.drain(..n - 5000);
                }
                if let Some(((file, root), port, label)) = attach {
                    ed.dap_generation += 1;
                    let g = ed.dap_generation;
                    ed.java_attach(file, root, "127.0.0.1".into(), port, g);
                    // Debug pane header = the test name, not the address
                    if let Some(d) = ed.dap.as_mut() {
                        d.program = label;
                    }
                }
            };
            tx.send(crate::event::Event::Job(Box::new(apply))).is_ok()
        };
        // Java: results from the JUnit XML the build tool leaves (only files written since this run) —
        // read here on the waiter thread once it ends
        let junit = match &target.how {
            How::Java { gradle, module_dir, .. } => {
                let reports = module_dir.join(if *gradle {
                    "build/test-results/test"
                } else {
                    "target/surefire-reports"
                });
                let src = [module_dir.join("src/test/java"), module_dir.join("src/main/java")];
                Some((reports, src))
            }
            _ => None,
        };
        std::thread::spawn(move || {
            let err_reader = stderr.map(|e| {
                let tx = tx.clone();
                std::thread::spawn(move || {
                    for line in BufReader::new(e).lines().map_while(Result::ok) {
                        if !send_line(&tx, true, line) {
                            break;
                        }
                    }
                })
            });
            if let Some(o) = stdout {
                for line in BufReader::new(o).lines().map_while(Result::ok) {
                    if !send_line(&tx, false, line) {
                        break;
                    }
                }
            }
            if let Some(h) = err_reader {
                let _ = h.join();
            }
            let status = child.wait().ok();
            let cases = junit.map(|(reports, src)| crate::test_results::junit(&[reports], wall, &src));
            let done = move |ed: &mut Editor| {
                let Some(r) = ed.test_run.as_mut().filter(|r| r.generation == generation) else { return };
                if r.state != RunState::Running {
                    return;
                }
                r.took = Some(r.started.elapsed());
                r.pid = None;
                if let Some(cases) = cases {
                    r.cases = cases;
                }
                // Pick the first failure (right side of the results pane · ]x)
                r.selected = r.failures().first().copied().unwrap_or(0);
                r.state = match status.and_then(|s| s.code()) {
                    Some(0) => RunState::Passed,
                    code => RunState::Failed(code),
                };
            };
            let _ = tx.send(crate::event::Event::Job(Box::new(done)));
        });
        let parser = crate::test_results::Parser::for_lang(lang_of(&target.how));
        self.test_run = Some(TestRun {
            target,
            state: RunState::Running,
            output: Vec::new(),
            started: Instant::now(),
            took: None,
            pid: Some(pid),
            generation,
            java_attach: None,
            cases: Vec::new(),
            parser,
            selected: 0,
        });
    }

    /// Java test debug — launch the test JVM waiting for a debugger via the build tool (build output in the
    /// test pane), and once it prints that it's waiting, attach with java-debug (debug pane).
    fn java_test_debug(&mut self, target: Target) {
        let Some(file) = self.doc().path.clone() else { return };
        let mut t = target.clone();
        if let How::Java { debug, .. } = &mut t.how {
            *debug = true;
        }
        let root = target.cwd.clone();
        let before = self.test_generation;
        self.test_start(t);
        if self.test_generation == before {
            return; // couldn't launch (reason shown)
        }
        self.test_last = Some((target.clone(), true));
        self.dap_test = Some(target);
        if let Some(r) = self.test_run.as_mut() {
            r.java_attach = Some((file, root));
        }
        self.note("Building and starting the test JVM — attaching when it's ready");
    }

    /// `space x d` — debugs the test at the cursor (stops at breakpoints).
    pub fn test_debug(&mut self) {
        if let Some(t) = self.test_target(Scope::Nearest, crate::offer::Resume::TestDebug) {
            self.test_debug_target(t);
        }
    }

    pub fn test_debug_target(&mut self, target: Target) {
        // Save modified files first — tests run the code on disk
        self.auto_save(" · before test");
        self.test_last = Some((target.clone(), true));
        self.dap_test = Some(target.clone());
        let lang = lang_of(&target.how);
        if lang == "java" {
            return self.java_test_debug(target);
        }
        self.dap_generation += 1;
        let generation = self.dap_generation;
        let cwd = target.cwd.clone();
        let label = target.label.clone();
        match target.how {
            How::Rust { filter, exact, flag, crate_root } => {
                self.dap_build_and_start("rust", label.clone(), cwd.clone(), generation, move || {
                    let exe = cargo_test_binary(&cwd, &flag, &crate_root)?;
                    let mut args = vec![filter];
                    if exact {
                        args.push("--exact".into());
                    }
                    args.extend(["--nocapture".into(), "--test-threads=1".into()]);
                    args.retain(|a| !a.is_empty());
                    Ok((json!({ "program": exe, "args": args, "cwd": cwd, "stopOnEntry": false }), label))
                });
            }
            How::Go { pattern, bench } => {
                let args = if bench {
                    vec![
                        "-test.run".to_string(),
                        "^$".into(),
                        "-test.bench".into(),
                        pattern,
                        "-test.v".into(),
                    ]
                } else {
                    vec!["-test.run".to_string(), pattern, "-test.v".into()]
                };
                let launch = json!({ "mode": "test", "program": cwd, "cwd": cwd, "args": args, "outputMode": "remote" });
                self.dap_start(lang, launch, label, generation);
            }
            How::Java { .. } => unreachable!("java_test_debug"),
            How::Python { python, node } => {
                let launch = json!({
                    "module": "pytest", "args": [node, "-q", "--color=no"], "python": python,
                    "cwd": cwd, "console": "internalConsole", "justMyCode": false,
                });
                self.dap_start(lang, launch, label, generation);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("tarae-testing-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// Parse with that language's grammar — None where the grammar isn't available (that test is skipped).
    fn parse(lang: &str, src: &str) -> Option<Tree> {
        let l = crate::syntax::Loader::global().load(crate::syntax::spec(lang)?).ok()?;
        let mut p = tree_sitter::Parser::new();
        p.set_language(&l.language).ok()?;
        p.parse(src, None)
    }

    /// Where `needle` first appears (cursor).
    fn at(src: &str, needle: &str) -> usize {
        src.find(needle).unwrap_or_else(|| panic!("{needle}"))
    }

    #[test]
    fn rust_finds_the_test_through_the_tree() {
        let d = tmp("rust");
        std::fs::write(d.join("Cargo.toml"), "[package]\nname = \"x\"\n").unwrap();
        std::fs::create_dir_all(d.join("src/editor")).unwrap();
        let file = d.join("src/editor/keys.rs");
        let src = r####"
fn helper() { let s = "}"; let c = '}'; let r = r#"}"#; } // } 주석
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    // 표지와 함수 사이 주석
    fn undo_works() {
        assert!(true);
    }

    fn not_a_test() {
        let x = 1;
    }

    #[tokio::test]
    async fn async_one() {}
}
"####;
        let tree = parse("rust", src).expect("rust grammar is always built in");
        let find_at =
            |needle: &str, f: &Path| find("rust", f, src, &tree, at(src, needle), Scope::Nearest).unwrap();
        let t = find_at("assert!(true)", &file);
        assert_eq!(t.label, "editor::keys::tests::undo_works");
        assert_eq!(t.argv(), ["cargo", "test", "--", "editor::keys::tests::undo_works", "--exact"]);
        // On the attribute line too, and async tests too
        assert_eq!(find_at("#[tokio::test]", &file).label, "editor::keys::tests::async_one");
        // Inside a non-test function = the enclosing module
        let t = find_at("let x = 1", &file);
        assert_eq!(
            (t.label.as_str(), t.argv().last().unwrap().as_str()),
            ("editor::keys::tests::*", "editor::keys::tests::")
        );
        // Outside a module = the file
        assert_eq!(find_at("fn helper", &file).label, "editor::keys::*");
        // Integration tests use the --test target
        std::fs::create_dir_all(d.join("tests")).unwrap();
        let t = find_at("assert!", &d.join("tests/api.rs"));
        assert_eq!(t.argv(), ["cargo", "test", "--test", "api", "--", "tests::undo_works", "--exact"]);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn go_picks_the_enclosing_test_func() {
        let src = "package p\n\nfunc helper() {}\n\nfunc TestParse(t *testing.T) {\n\tx := 1\n}\n\nfunc TestLex(t *testing.T) {}\n\nfunc Testify() {}\n\nfunc BenchmarkParse(b *testing.B) {}\n";
        let Some(tree) = parse("go", src) else { return };
        let f = Path::new("/p/parse_test.go");
        let find_at = |needle: &str| find("go", f, src, &tree, at(src, needle), Scope::Nearest).unwrap();
        assert_eq!(find_at("x := 1").argv(), ["go", "test", "-v", "-run", "^TestParse$", "."]);
        assert_eq!(
            find_at("BenchmarkParse").argv(),
            ["go", "test", "-v", "-run", "^$", "-bench", "^BenchmarkParse$", "."]
        );
        assert_eq!(
            find_at("helper").argv()[4],
            "^(TestParse|TestLex)$",
            "outside a test = the file (Testify is not a test)"
        );
        assert!(find("go", Path::new("/p/parse.go"), src, &tree, 0, Scope::Nearest).is_err());
        assert!(go_test_name("Test2") && !go_test_name("Testing"), "Test+lowercase isn't one (go rule)");
    }

    #[test]
    fn python_builds_pytest_node_ids() {
        let d = tmp("py");
        std::fs::write(d.join("pyproject.toml"), "").unwrap();
        std::fs::create_dir_all(d.join("tests")).unwrap();
        let file = d.join("tests/test_math.py");
        let src = "import x\n\ndef test_add():\n    def inner():\n        return 1\n    assert inner() == 1\n\nclass TestDiv:\n    def helper(self):\n        pass\n\n    @pytest.mark.slow\n    def test_zero(self):\n        # 주석\n        assert True\n";
        let Some(tree) = parse("python", src) else { return };
        let node = |needle: &str| match find("python", &file, src, &tree, at(src, needle), Scope::Nearest)
            .unwrap()
            .how
        {
            How::Python { node, .. } => node,
            _ => unreachable!(),
        };
        assert_eq!(node("return 1"), "tests/test_math.py::test_add", "the outer test, not the inner helper");
        assert_eq!(node("assert True"), "tests/test_math.py::TestDiv::test_zero");
        assert_eq!(node("@pytest"), "tests/test_math.py::TestDiv::test_zero", "on the decorator too");
        assert_eq!(node("pass"), "tests/test_math.py::TestDiv", "non-test method = its class");
        assert_eq!(node("import x"), "tests/test_math.py");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn java_runs_through_gradle_or_maven_with_nested_classes() {
        let src = "package demo;\n\nimport org.junit.jupiter.api.*;\n\nclass AppTest {\n    @Test void greets() {\n        int x = 1;\n    }\n\n    void helper() {}\n\n    @Nested\n    class Edge {\n        @ParameterizedTest\n        void empty() {}\n    }\n}\n";
        let Some(tree) = parse("java", src) else { return };
        let d = tmp("java");
        std::fs::write(d.join("settings.gradle.kts"), "").unwrap();
        std::fs::write(d.join("gradlew"), "").unwrap();
        std::fs::create_dir_all(d.join("app/src/test/java/demo")).unwrap();
        std::fs::write(d.join("app/build.gradle.kts"), "").unwrap();
        let file = d.join("app/src/test/java/demo/AppTest.java");
        let find_at = |needle: &str, scope| find("java", &file, src, &tree, at(src, needle), scope).unwrap();
        let t = find_at("int x", Scope::Nearest);
        assert_eq!(t.label, "AppTest.greets");
        let gw = d.join("gradlew").display().to_string();
        assert_eq!(
            t.argv(),
            [gw.as_str(), ":app:test", "--rerun", "--console=plain", "--tests", "demo.AppTest.greets"]
        );
        assert_eq!(t.cwd, d, "runs from the multi-module top");
        assert_eq!(find_at("void empty", Scope::Nearest).argv()[5], "demo.AppTest$Edge.empty", "nested: $");
        assert_eq!(
            find_at("@ParameterizedTest", Scope::Nearest).label,
            "AppTest.Edge.empty",
            "on the annotation too"
        );
        assert_eq!(
            find_at("void helper", Scope::Nearest).argv()[5],
            "demo.AppTest",
            "non-test method = its class"
        );
        assert_eq!(find_at("int x", Scope::File).argv()[5], "demo.AppTest");
        // Maven: -pl module -Dtest=Class#method
        std::fs::remove_file(d.join("settings.gradle.kts")).unwrap();
        std::fs::remove_file(d.join("app/build.gradle.kts")).unwrap();
        std::fs::write(d.join("pom.xml"), "").unwrap();
        std::fs::write(d.join("app/pom.xml"), "").unwrap();
        let t = find_at("int x", Scope::Nearest);
        assert_eq!(
            t.argv(),
            [
                "mvn",
                "-B",
                "test",
                "-pl",
                "app",
                "-Dtest=demo.AppTest#greets",
                "-Dsurefire.failIfNoSpecifiedTests=false"
            ]
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn rust_binaries_in_their_own_folder() {
        let d = tmp("rust-bin");
        std::fs::write(d.join("Cargo.toml"), "").unwrap();
        std::fs::create_dir_all(d.join("src/bin/tool")).unwrap();
        let (_, crate_root, flag, mods) = rust_crate(&d.join("src/bin/tool/main.rs")).unwrap();
        assert_eq!(
            (crate_root, flag, mods),
            (d.join("src/bin/tool/main.rs"), vec!["--bin".into(), "tool".into()], vec![])
        );
        let (_, _, flag, mods) = rust_crate(&d.join("src/bin/tool/util.rs")).unwrap();
        assert_eq!((flag, mods), (vec!["--bin".to_string(), "tool".into()], vec!["util".to_string()]));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_run_that_cannot_launch_leaves_the_previous_one_current() {
        let d = tmp("spawn");
        let mut ed = Editor::new(crate::config::Config::default());
        let target = |python: &str| Target {
            label: "t".into(),
            cwd: d.clone(),
            how: How::Python { python: python.into(), node: "x".into() },
        };
        // `sh -m pytest …` — ends at once (no such script)
        ed.test_start(target("sh"));
        ed.test_start(target("/nonexistent/python"));
        assert!(ed.status.as_ref().is_some_and(|s| s.0.contains("/nonexistent/python")), "{:?}", ed.status);
        assert_eq!(ed.test_run.as_ref().map(|r| r.generation), Some(ed.test_generation));
        // …so its end still lands (not "running" forever)
        let end = Instant::now() + Duration::from_secs(10);
        while ed.test_run.as_ref().is_some_and(|r| r.state == RunState::Running) {
            let ev = ed.events.recv_timeout(end.saturating_duration_since(Instant::now())).expect("event");
            ed.handle_event(ev);
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn strips_color_codes() {
        assert_eq!(strip_ansi("\x1b[32mok\x1b[0m done\r"), "ok done");
    }
}
