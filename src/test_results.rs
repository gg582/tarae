//! Test result parsing — reads runner output line by line into per-test results (pass·fail·skip ·
//! duration · failure message · failure location). Rust = libtest (`test a::b ... ok` ·
//! `---- a::b stdout ----` · `panicked at file:line:col:`), Go = `go test -v`
//! (`--- FAIL: TestX (0.00s)` · `    x_test.go:8: message`), Python = `pytest -v --tb=short`
//! (`file::node PASSED` · `___ node ___` · `file:line: in …` · `E   …`), Java = the JUnit XML reports
//! Gradle·Maven leave (read once after finishing — location from `at pkg.Class.method(File.java:12)`).

use std::path::{Path, PathBuf};
use std::time::SystemTime;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Passed,
    Failed,
    Skipped,
}

/// One test's result.
#[derive(Clone, Debug, PartialEq)]
pub struct Case {
    pub name: String,
    pub status: Status,
    pub secs: Option<f32>,
    /// Failure message (multiple lines — the first is the gist).
    pub message: Vec<String>,
    /// Where it failed (file, 0-based line).
    pub at: Option<(PathBuf, usize)>,
}

impl Case {
    fn new(name: &str, status: Status) -> Case {
        Case { name: name.to_string(), status, secs: None, message: Vec::new(), at: None }
    }
}

/// Relative path → actual file — from the run folder, else its ancestors (paths relative to the
/// workspace root).
fn resolve(cwd: &Path, rel: &str) -> Option<PathBuf> {
    let p = Path::new(rel);
    if p.is_absolute() {
        return std::fs::canonicalize(p).ok();
    }
    // Canonical path so it compares with document paths (macOS /tmp → /private/tmp)
    cwd.ancestors().map(|d| d.join(p)).find(|f| f.is_file()).map(|f| std::fs::canonicalize(&f).unwrap_or(f))
}

/// A fragment like `a/b.rs:21:9` → (path, 0-based line).
fn file_line(s: &str, ext: &str) -> Option<(String, usize)> {
    let i = s.find(ext)?;
    let path = s[..i + ext.len()].rsplit([' ', '(', '\'']).next()?.to_string();
    let n: String = s[i + ext.len()..].strip_prefix(':')?.chars().take_while(char::is_ascii_digit).collect();
    Some((path, n.parse::<usize>().ok()?.saturating_sub(1)))
}

/// Per-runner parse state.
#[derive(Clone, Debug, Default)]
pub enum Parser {
    Rust {
        /// The `---- name stdout ----` section being read.
        section: Option<String>,
    },
    Go {
        /// The current `=== RUN` test (the `file:line: message` lines below it belong to it).
        running: Option<String>,
    },
    Python {
        failures: bool,
        section: Option<String>,
    },
    /// Not read line by line (Java = XML after finishing).
    #[default]
    None,
}

impl Parser {
    pub fn for_lang(lang: &str) -> Parser {
        match lang {
            "rust" => Parser::Rust { section: None },
            "go" => Parser::Go { running: None },
            "python" => Parser::Python { failures: false, section: None },
            _ => Parser::None,
        }
    }

    /// Reads one line — adds to or updates `cases`.
    pub fn line(&mut self, cwd: &Path, line: &str, cases: &mut Vec<Case>) {
        let find = |cases: &mut Vec<Case>, name: &str| -> usize {
            match cases.iter().position(|c| c.name == name) {
                Some(i) => i,
                None => {
                    cases.push(Case::new(name, Status::Failed));
                    cases.len() - 1
                }
            }
        };
        match self {
            Parser::Rust { section } => {
                if let Some(rest) = line.strip_prefix("test ")
                    && let Some((name, verdict)) = rest.rsplit_once(" ... ")
                {
                    let name = name.trim_end_matches(" - should panic");
                    let status = match verdict.trim() {
                        "ok" => Status::Passed,
                        "FAILED" => Status::Failed,
                        v if v.starts_with("ignored") => Status::Skipped,
                        _ => return,
                    };
                    let i = find(cases, name);
                    cases[i].status = status;
                    return;
                }
                if let Some(name) = line.strip_prefix("---- ").and_then(|l| l.strip_suffix(" stdout ----")) {
                    *section = Some(name.to_string());
                    return;
                }
                if line.starts_with("failures:") || line.starts_with("test result:") {
                    *section = None;
                    return;
                }
                if let Some(name) = section.clone() {
                    if line.trim().is_empty() || line.starts_with("note: run with") {
                        return;
                    }
                    let i = find(cases, &name);
                    if let Some(p) = line.split_once("panicked at ").map(|x| x.1) {
                        if cases[i].at.is_none() {
                            cases[i].at = file_line(p, ".rs").and_then(|(f, l)| Some((resolve(cwd, &f)?, l)));
                        }
                        // Old format: panicked at 'message', file:line:col
                        if let Some(msg) = p.strip_prefix('\'').and_then(|m| m.rsplit_once("', ")) {
                            cases[i].message.push(msg.0.to_string());
                        }
                        return;
                    }
                    cases[i].message.push(line.to_string());
                }
            }
            Parser::Go { running } => {
                let t = line.trim_start();
                if let Some(name) = t.strip_prefix("=== RUN") {
                    let name = name.trim();
                    find(cases, name);
                    *running = Some(name.to_string());
                    return;
                }
                for (tag, status) in [
                    ("--- PASS: ", Status::Passed),
                    ("--- FAIL: ", Status::Failed),
                    ("--- SKIP: ", Status::Skipped),
                ] {
                    if let Some(rest) = t.strip_prefix(tag) {
                        let (name, secs) = match rest.split_once(" (") {
                            Some((n, s)) => (n, s.trim_end_matches(')').trim_end_matches('s').parse().ok()),
                            None => (rest, None),
                        };
                        let i = find(cases, name.trim());
                        (cases[i].status, cases[i].secs) = (status, secs);
                        return;
                    }
                }
                // Failure message: `    x_test.go:8: Add = 4` (t.Log·t.Error)
                if line.starts_with("    ")
                    && let (Some(name), Some((f, l))) = (running.clone(), file_line(t, ".go"))
                {
                    let i = find(cases, &name);
                    if cases[i].at.is_none() {
                        cases[i].at = resolve(cwd, &f).map(|p| (p, l));
                    }
                    let msg = t.split_once(": ").map_or(t, |x| x.1);
                    cases[i].message.push(msg.to_string());
                }
            }
            Parser::Python { failures, section } => {
                // Result line: `tests/test_x.py::TestA::test_b PASSED [ 50%]`
                for (tag, status) in [
                    (" PASSED", Status::Passed),
                    (" FAILED", Status::Failed),
                    (" ERROR", Status::Failed),
                    (" SKIPPED", Status::Skipped),
                    (" XFAIL", Status::Skipped),
                ] {
                    if !*failures
                        && line.contains("::")
                        && let Some(node) = line.split_once(tag).map(|x| x.0)
                        && !node.contains(' ')
                    {
                        let name = node.split_once("::").map_or(node, |x| x.1);
                        let i = find(cases, name);
                        cases[i].status = status;
                        return;
                    }
                }
                if line.starts_with('=') && line.contains(" FAILURES ") {
                    *failures = true;
                    return;
                }
                if !*failures {
                    return;
                }
                if line.starts_with('=') {
                    *failures = false;
                    *section = None;
                    return;
                }
                // `____ TestA.test_b ____`
                if line.starts_with("__") && line.trim_end().ends_with("__") {
                    let name = line.trim_matches(|c| c == '_' || c == ' ').replace('.', "::");
                    *section = Some(name);
                    return;
                }
                let Some(name) = section.clone() else { return };
                let Some(i) =
                    cases.iter().position(|c| c.name == name || c.name.ends_with(&format!("::{name}")))
                else {
                    return;
                };
                if let Some(msg) = line.strip_prefix("E ") {
                    cases[i].message.push(msg.trim().to_string());
                } else if let Some((f, l)) = file_line(line, ".py")
                    && line.starts_with(&f)
                    && cases[i].message.is_empty()
                {
                    cases[i].at = resolve(cwd, &f).map(|p| (p, l));
                }
            }
            Parser::None => {}
        }
    }
}

/// Unescapes entities in XML text.
fn unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&#10;", "\n")
        .replace("&#13;", "")
        .replace("&amp;", "&")
}

fn attr(tag: &str, name: &str) -> Option<String> {
    let key = format!(" {name}=\"");
    let rest = &tag[tag.find(&key)? + key.len()..];
    Some(unescape(&rest[..rest.find('"')?]))
}

/// JUnit XML reports (Gradle `build/test-results/test` · Maven `target/surefire-reports`) written
/// after `since`.
/// Failure location = that class's stack line `at pkg.Class.method(File.java:12)` → file found in
/// `src_roots`.
pub fn junit(report_dirs: &[PathBuf], since: SystemTime, src_roots: &[PathBuf]) -> Vec<Case> {
    let mut out = Vec::new();
    let mut files: Vec<PathBuf> = report_dirs
        .iter()
        .filter_map(|d| std::fs::read_dir(d).ok())
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "xml"))
        .filter(|p| std::fs::metadata(p).and_then(|m| m.modified()).is_ok_and(|t| t >= since))
        .collect();
    files.sort();
    for f in files {
        let Ok(xml) = std::fs::read_to_string(&f) else { continue };
        let mut rest = xml.as_str();
        while let Some(i) = rest.find("<testcase ") {
            rest = &rest[i..];
            let head_end = rest.find('>').unwrap_or(rest.len());
            let head = &rest[..head_end];
            let self_closing = head.ends_with('/');
            let body_end = if self_closing { head_end } else { rest.find("</testcase>").unwrap_or(head_end) };
            let body = &rest[head_end.min(body_end)..body_end];
            let class = attr(head, "classname").unwrap_or_default();
            let short = class.rsplit('.').next().unwrap_or(&class).replace('$', ".");
            let name = format!("{short}.{}", attr(head, "name").unwrap_or_default().trim_end_matches("()"));
            let mut c = Case::new(&name, Status::Passed);
            c.secs = attr(head, "time").and_then(|t| t.parse().ok());
            if body.contains("<skipped") {
                c.status = Status::Skipped;
            }
            if let Some(j) = body.find("<failure").or_else(|| body.find("<error")) {
                c.status = Status::Failed;
                let tag = &body[j..body[j..].find('>').map_or(body.len(), |k| j + k)];
                if let Some(m) = attr(tag, "message") {
                    // Gradle prefixes the message with the exception name
                    // (`org.opentest4j.AssertionFailedError: …`) — strip it
                    let ty = attr(tag, "type").unwrap_or_default();
                    let m = m.strip_prefix(&format!("{ty}: ")).unwrap_or(&m);
                    c.message.extend(m.lines().map(str::to_string));
                }
                // This class's line in the stack → source file
                let outer = class.split('$').next().unwrap_or(&class);
                let text = unescape(&body[j..]);
                c.at = text.lines().map(str::trim).filter_map(|l| l.strip_prefix("at ")).find_map(|l| {
                    if !l.starts_with(outer) {
                        return None;
                    }
                    let (file, line) = file_line(l.split_once('(')?.1, ".java")?;
                    let pkg_dir =
                        outer.rsplit_once('.').map(|(p, _)| p.replace('.', "/")).unwrap_or_default();
                    let path =
                        src_roots.iter().map(|r| r.join(&pkg_dir).join(&file)).find(|p| p.is_file())?;
                    Some((std::fs::canonicalize(&path).unwrap_or(path), line))
                });
            }
            out.push(c);
            rest = &rest[body_end.max(1)..];
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(p: &mut Parser, cwd: &Path, text: &str) -> Vec<Case> {
        let mut cases = Vec::new();
        for l in text.lines() {
            p.line(cwd, l, &mut cases);
        }
        cases
    }

    fn tmp_with(tag: &str, files: &[&str]) -> PathBuf {
        let d = std::env::temp_dir().join(format!("tarae-results-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        for f in files {
            let p = d.join(f);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, "x").unwrap();
        }
        std::fs::canonicalize(&d).unwrap()
    }

    #[test]
    fn reads_libtest_output() {
        let d = tmp_with("rust", &["src/math.rs"]);
        let out = "running 3 tests\ntest math::tests::adds ... ok\ntest math::tests::divides ... FAILED\ntest math::tests::slow ... ignored, takes long\n\nfailures:\n\n---- math::tests::divides stdout ----\n\nthread 'math::tests::divides' panicked at src/math.rs:21:9:\nassertion `left == right` failed: 7 / 2 rounds down\n  left: 3\n right: 4\nnote: run with `RUST_BACKTRACE=1` environment variable to display a backtrace\n\n\nfailures:\n    math::tests::divides\n\ntest result: FAILED. 1 passed; 1 failed; 1 ignored";
        let c = feed(&mut Parser::for_lang("rust"), &d, out);
        let st: Vec<_> = c.iter().map(|c| (c.name.as_str(), c.status)).collect();
        assert_eq!(
            st,
            [
                ("math::tests::adds", Status::Passed),
                ("math::tests::divides", Status::Failed),
                ("math::tests::slow", Status::Skipped)
            ]
        );
        assert_eq!(
            c[1].message,
            ["assertion `left == right` failed: 7 / 2 rounds down", "  left: 3", " right: 4"]
        );
        assert_eq!(c[1].at, Some((d.join("src/math.rs"), 20)));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn reads_go_test_v_output() {
        let d = tmp_with("go", &["calc_test.go"]);
        let out = "=== RUN   TestAdd\n    calc_test.go:8: Add = 4\n--- FAIL: TestAdd (0.00s)\n=== RUN   TestAddNegative\n--- PASS: TestAddNegative (0.01s)\nFAIL\nexit status 1";
        let c = feed(&mut Parser::for_lang("go"), &d, out);
        assert_eq!((c[0].name.as_str(), c[0].status, c[0].secs), ("TestAdd", Status::Failed, Some(0.0)));
        assert_eq!(c[0].message, ["Add = 4"]);
        assert_eq!(c[0].at, Some((d.join("calc_test.go"), 7)));
        assert_eq!((c[1].status, c[1].secs), (Status::Passed, Some(0.01)));
    }

    #[test]
    fn reads_pytest_verbose_short_tracebacks() {
        let d = tmp_with("py", &["tests/test_math.py"]);
        let out = "tests/test_math.py::test_add FAILED                                   [ 50%]\ntests/test_math.py::TestDiv::test_half PASSED                        [100%]\n\n=================================== FAILURES ===================================\n___________________________________ test_add ___________________________________\ntests/test_math.py:7: in test_add\n    assert total == 6\nE   assert 5 == 6\n=========================== short test summary info ============================\nFAILED tests/test_math.py::test_add - assert 5 == 6";
        let c = feed(&mut Parser::for_lang("python"), &d, out);
        assert_eq!(
            c.iter().map(|c| (c.name.as_str(), c.status)).collect::<Vec<_>>(),
            [("test_add", Status::Failed), ("TestDiv::test_half", Status::Passed)]
        );
        assert_eq!(c[0].message, ["assert 5 == 6"]);
        assert_eq!(c[0].at, Some((d.join("tests/test_math.py"), 6)));
    }

    #[test]
    fn reads_junit_xml_reports() {
        let d = tmp_with("junit", &["src/test/java/demo/AppTest.java"]);
        let reports = d.join("build/test-results/test");
        std::fs::create_dir_all(&reports).unwrap();
        let since = SystemTime::now() - std::time::Duration::from_secs(5);
        std::fs::write(
            reports.join("TEST-demo.AppTest.xml"),
            r#"<?xml version="1.0"?>
<testsuite name="demo.AppTest" tests="3">
  <testcase name="greets()" classname="demo.AppTest" time="0.012"/>
  <testcase name="fails()" classname="demo.AppTest" time="0.003">
    <failure message="org.opentest4j.AssertionFailedError: expected: &lt;1&gt; but was: &lt;2&gt;" type="org.opentest4j.AssertionFailedError">org.opentest4j.AssertionFailedError: expected: &lt;1&gt; but was: &lt;2&gt;
	at org.junit.jupiter.api.AssertionUtils.fail(AssertionUtils.java:151)
	at demo.AppTest.fails(AppTest.java:17)
</failure>
  </testcase>
  <testcase name="empty()" classname="demo.AppTest$Edge" time="0"><skipped/></testcase>
</testsuite>"#,
        )
        .unwrap();
        let c = junit(&[reports], since, &[d.join("src/test/java")]);
        let st: Vec<_> = c.iter().map(|c| (c.name.as_str(), c.status)).collect();
        assert_eq!(
            st,
            [
                ("AppTest.greets", Status::Passed),
                ("AppTest.fails", Status::Failed),
                ("AppTest.Edge.empty", Status::Skipped)
            ]
        );
        assert_eq!(c[1].message, ["expected: <1> but was: <2>"]);
        assert_eq!(c[1].at, Some((d.join("src/test/java/demo/AppTest.java"), 16)));
        assert_eq!(c[0].secs, Some(0.012));
        let _ = std::fs::remove_dir_all(&d);
    }
}
