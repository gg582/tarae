//! Attach a debugger to a running program (remote debugging) — `space G a` · `:attach [name | host:port]`.
//!
//! Each language attaches differently:
//! - **Python** — the debug server of a program launched with `python -m debugpy --listen 5678 app.py`
//!   speaks DAP → connect directly over TCP and `attach`.
//! - **Go** — the server of `dlv debug|exec|attach --headless --listen :2345 --accept-multiclient` also
//!   speaks DAP → connect directly over TCP and `attach {mode: remote}`.
//! - **Java** — launch the JVM with `-agentlib:jdwp=transport=dt_socket,server=y,suspend=n,address=*:5005`
//!   → java-debug inside jdtls attaches over JDWP (java.rs).
//! - **Rust·C** — remote `gdbserver`·`lldb-server gdbserver :1234 ./app` → lldb-dap via
//!   `gdb-remote host:port`. With `pid`, a local process (lldb-dap·dlv·debugpy all).
//!
//! Config (`~/.config/tarae/config.toml`·`.tarae.toml`) — name targets and pick one in `space G a`:
//! ```toml
//! [[attach]]
//! name = "api (k8s)"
//! lang = "java"                  # defaults to the current file's language
//! port = 5005                    # host defaults to 127.0.0.1
//! before = "kubectl -n app port-forward deploy/api 5005:5005"   # run first, stopped with the session
//! remote-root = "/app"           # source root on the remote (container) ↔ this project's root
//! ```

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::json;

use crate::dap::{kill_group, new_session};
use crate::editor::Editor;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AttachTarget {
    pub name: String,
    /// Defaults to the current file's language.
    pub lang: Option<String>,
    pub host: String,
    pub port: Option<u16>,
    /// Local process id (instead of host·port).
    pub pid: Option<u32>,
    /// Command to launch first (`kubectl port-forward …`·`ssh -L …`) — attach once it prints a line
    /// or after a moment.
    pub before: Option<String>,
    /// Source root on the remote side (`/app` in a container etc.) — maps breakpoint·call stack paths
    /// to this project's root.
    pub remote_root: Option<String>,
    /// Rust·C: executable to read symbols from (lldb).
    pub program: Option<String>,
}

impl AttachTarget {
    /// Debug pane header — shown once if the name is the address.
    pub fn label(&self) -> String {
        let addr = self.address();
        if self.name == addr { addr } else { format!("{} · {addr}", self.name) }
    }

    pub fn address(&self) -> String {
        match (self.pid, self.port) {
            (Some(pid), _) => format!("pid {pid}"),
            (None, Some(p)) => format!("{}:{p}", self.host),
            (None, None) => "?".into(),
        }
    }
}

/// `[[attach]]` tables (from every config layer — for the same name, the later layer wins).
pub fn parse_config(v: &toml::Value, file: &str, warnings: &mut Vec<String>) -> Vec<AttachTarget> {
    let Some(list) = v.as_array() else {
        warnings.push(format!("{file}: attach must be [[attach]] tables"));
        return Vec::new();
    };
    let s = |t: &toml::Value, k: &str| t.get(k).and_then(|x| x.as_str()).map(str::to_string);
    list.iter()
        .enumerate()
        .filter_map(|(i, t)| {
            let port = t.get("port").and_then(|p| p.as_integer()).and_then(|p| u16::try_from(p).ok());
            let pid = t.get("pid").and_then(|p| p.as_integer()).and_then(|p| u32::try_from(p).ok());
            if port.is_none() && pid.is_none() {
                warnings.push(format!("{file}: attach #{}: needs port = … (or pid = …)", i + 1));
                return None;
            }
            Some(AttachTarget {
                name: s(t, "name").unwrap_or_else(|| format!("attach #{}", i + 1)),
                lang: s(t, "lang"),
                host: s(t, "host").unwrap_or_else(|| "127.0.0.1".into()),
                port,
                pid,
                before: s(t, "before"),
                remote_root: s(t, "remote-root"),
                program: s(t, "program"),
            })
        })
        .collect()
}

/// `host:port` · `:port` · `port` → (host, port).
pub fn parse_addr(s: &str) -> Option<(String, u16)> {
    let s = s.trim();
    let (host, port) = match s.rsplit_once(':') {
        Some((h, p)) => (if h.is_empty() { "127.0.0.1" } else { h }, p),
        None => ("127.0.0.1", s),
    };
    Some((host.trim_matches(['[', ']']).to_string(), port.parse().ok()?))
}

/// Launches the `before` command (new session — stopped with `kill_group`) and waits until it's ready
/// (prints its first line, or 2 s) — if it dies first, its reason (on a worker thread).
fn start_helper(cmd: &str, cwd: &Path) -> Result<Child, String> {
    let mut c = Command::new("sh");
    c.args(["-c", cmd]).current_dir(cwd).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = new_session(&mut c).spawn().map_err(|e| format!("before: {e}"))?;
    let (tx, rx) = std::sync::mpsc::channel();
    if let Some(out) = child.stdout.take() {
        let tx = tx.clone();
        std::thread::spawn(move || {
            let mut lines = BufReader::new(out).lines();
            let _ = tx.send(lines.next().and_then(Result::ok));
            for _ in lines {} // keep draining (so a full pipe doesn't stall it)
        });
    }
    let err = child.stderr.take();
    let err_text = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    if let Some(e) = err {
        let t = err_text.clone();
        std::thread::spawn(move || {
            for l in BufReader::new(e).lines().map_while(Result::ok) {
                let mut t = t.lock().unwrap();
                if t.len() < 400 {
                    t.push_str(l.trim());
                    t.push(' ');
                }
            }
        });
    }
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(2) {
        // Printed a line → ready (unless it simply closed — check whether it exited right away)
        if let Ok(Some(_)) = rx.try_recv() {
            break;
        }
        if let Ok(Some(status)) = child.try_wait() {
            std::thread::sleep(Duration::from_millis(50));
            let why = err_text.lock().unwrap().trim().to_string();
            return Err(format!(
                "`{cmd}` exited ({status}){}",
                if why.is_empty() { String::new() } else { format!(": {why}") }
            ));
        }
        std::thread::sleep(Duration::from_millis(30));
    }
    Ok(child)
}

/// Attaches over TCP — the server may still be starting, so keep trying for a few seconds
/// (on a worker thread).
fn connect(host: &str, port: u16) -> Result<std::net::TcpStream, String> {
    use std::net::ToSocketAddrs;
    let addrs: Vec<_> = (host, port).to_socket_addrs().map_err(|e| format!("{host}: {e}"))?.collect();
    let started = Instant::now();
    loop {
        let mut last = None;
        for a in &addrs {
            match std::net::TcpStream::connect_timeout(a, Duration::from_secs(2)) {
                Ok(s) => return Ok(s),
                Err(e) => last = Some(e),
            }
        }
        if started.elapsed() > Duration::from_secs(5) {
            return Err(format!(
                "can't reach {host}:{port} — {}",
                last.map(|e| e.to_string()).unwrap_or_default()
            ));
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

impl Editor {
    /// `space G a` — pick a target to attach to (asks for an address if none are configured;
    /// targets are layered up to this project's `.tarae.toml`).
    pub fn attach_picker(&mut self) {
        let targets = &self.config.attach;
        if targets.is_empty() {
            self.open_prompt(crate::editor::PromptKind::Command, "attach ");
            return self.set_status("attach to host:port (or add [[attach]] to .tarae.toml)");
        }
        let items = targets
            .iter()
            .map(|t| crate::picker::Item {
                label: t.name.clone(),
                action: crate::picker::Action::Typed(format!("attach {}", t.name)),
                hint: [t.lang.clone().unwrap_or_default(), t.address()]
                    .iter()
                    .filter(|s| !s.is_empty())
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(" · "),
                glyph: None,
            })
            .collect();
        let mut p = crate::picker::Picker::new("attach", items, false).without_preview();
        p.compact = true;
        self.open_picker(p, None);
    }

    /// `:attach [name | host:port]`.
    pub fn attach_command(&mut self, arg: &str) -> Result<(), String> {
        let arg = arg.trim();
        if arg.is_empty() {
            self.attach_picker();
            return Ok(());
        }
        let target = match self.config.attach.iter().find(|t| t.name == arg).cloned() {
            Some(t) => t,
            None => {
                let (host, port) =
                    parse_addr(arg).ok_or_else(|| format!("no attach target '{arg}' (name or host:port)"))?;
                AttachTarget { name: format!("{host}:{port}"), host, port: Some(port), ..Default::default() }
            }
        };
        self.attach(target);
        Ok(())
    }

    /// Attaches — helper·TCP on a worker thread (editing isn't blocked even if the remote is slow).
    pub fn attach(&mut self, t: AttachTarget) {
        use crate::dap::{Dap, State};
        if matches!(
            self.dap.as_ref().map(|d| &d.state),
            Some(State::Running | State::Stopped { .. } | State::Starting)
        ) {
            return self.note("already debugging — space G t first");
        }
        let doc = self.doc();
        let file = doc.path.clone();
        // Language = config's lang, else the current file's — if its grammar is missing, offer a download
        // and attach once it's downloaded
        let lang = match (t.lang.clone(), &doc.syntax, file.as_deref().and_then(crate::syntax::detect)) {
            (Some(l), ..) => l,
            (None, Some(s), _) => s.lang.name.clone(),
            (None, None, Some(spec)) if crate::syntax::Loader::global().load(spec).is_err() => {
                return self.offer_grammar_then(spec, crate::offer::Resume::Attach(t));
            }
            (None, None, Some(spec)) => spec.name.clone(),
            (None, None, None) => String::new(),
        };
        let root = file
            .as_deref()
            .map(|f| crate::lsp::find_root_for(&lang, f))
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_default();
        let root = std::fs::canonicalize(&root).unwrap_or(root);
        if !matches!(lang.as_str(), "python" | "go" | "java" | "rust" | "c" | "cpp") {
            return self.set_error(format!(
                "attach: no debugger for {}",
                if lang.is_empty() { "this file" } else { &lang }
            ));
        }
        self.dap_test = None;
        self.dap_generation += 1;
        let generation = self.dap_generation;
        self.dap = Some(Dap::placeholder(State::Starting, t.label()));
        let direct = matches!(lang.as_str(), "python" | "go") && t.pid.is_none();
        let (before, host, port, cwd) = (t.before.clone(), t.host.clone(), t.port, root.clone());
        self.events.jobs().spawn(move || {
            let helper = before.as_deref().map(|b| start_helper(b, &cwd)).transpose();
            let stream = match (&helper, direct, port) {
                (Ok(_), true, Some(p)) => Some(connect(&host, p)),
                _ => None,
            };
            move |ed: &mut Editor| {
                if ed.dap_generation != generation {
                    if let Ok(Some(mut h)) = helper {
                        kill_group(&mut h);
                    }
                    return;
                }
                let helper = match helper {
                    Ok(h) => h,
                    Err(e) => {
                        ed.dap = None;
                        return ed.set_error(format!("attach: {e}"));
                    }
                };
                ed.attach_continue(t, lang, root, file, stream, helper, generation);
            }
        });
    }

    #[allow(clippy::too_many_arguments)]
    fn attach_continue(
        &mut self,
        t: AttachTarget,
        lang: String,
        root: PathBuf,
        file: Option<PathBuf>,
        stream: Option<Result<std::net::TcpStream, String>>,
        helper: Option<Child>,
        generation: u64,
    ) {
        let program = t.label();
        let remote = t.remote_root.clone();
        let fail = |ed: &mut Editor, helper: Option<Child>, e: String| {
            if let Some(mut h) = helper {
                kill_group(&mut h);
            }
            ed.dap = None;
            ed.set_error(format!("attach: {e}"));
        };
        match (lang.as_str(), stream) {
            // Directly over TCP — remote debugpy·dlv servers speak DAP
            (l @ ("python" | "go"), Some(stream)) => {
                let s = match stream {
                    Ok(s) => s,
                    Err(e) => return fail(self, helper, e),
                };
                let args = if l == "python" {
                    let mut a = json!({ "justMyCode": false });
                    if let Some(r) = &remote {
                        a["pathMappings"] = json!([{ "localRoot": root, "remoteRoot": r }]);
                    }
                    a
                } else {
                    let mut a = json!({ "mode": "remote" });
                    if let Some(r) = &remote {
                        a["substitutePath"] = json!([{ "from": root, "to": r }]);
                    }
                    a
                };
                let id = if l == "python" { "debugpy" } else { "go" };
                let reader = Box::new(s.try_clone().expect("tcp clone"));
                self.dap_connect(id, reader, Box::new(s), None, args, program, generation);
            }
            ("java", _) => {
                let Some(port) = t.port else {
                    return fail(self, helper, "Java attaches to host:port (JDWP)".into());
                };
                let file = file.unwrap_or_else(|| root.join("x.java"));
                // (a leftover prep from an earlier session must not look like this one's)
                self.java_debug = None;
                self.java_attach(file, root, t.host.clone(), port, generation);
                if self.java_debug.is_none() {
                    // Couldn't attach (no jdtls etc. — reason already reported) or a download offer (which
                    // starts over once downloaded) — don't stay "starting" forever
                    self.dap = None;
                    if let Some(mut h) = helper {
                        kill_group(&mut h);
                    }
                    return;
                }
                // On java.rs's placeholder — dap_connect takes it over once java-debug gives a port
                if let Some(d) = &mut self.dap {
                    d.helper = helper;
                }
                return;
            }
            // Launch a local adapter to attach — lldb-dap (gdb-remote·pid); with pid, dlv·debugpy too
            (l, _) => {
                let adapter_lang = if l == "cpp" || l == "c" { "rust" } else { l };
                let Some(id) = crate::dap::adapter_id(adapter_lang) else {
                    return fail(self, helper, format!("no debug adapter for {l}"));
                };
                let args = match (id, t.pid) {
                    ("lldb-dap", Some(pid)) => json!({ "pid": pid, "program": t.program }),
                    ("lldb-dap", None) => {
                        let mut a = json!({ "attachCommands": [format!("gdb-remote {}:{}", t.host, t.port.unwrap_or(0))] });
                        if let Some(p) = &t.program {
                            a["program"] = json!(root.join(p));
                        }
                        if let Some(r) = &remote {
                            a["sourceMap"] = json!([[r, root]]);
                        }
                        a
                    }
                    ("go", Some(pid)) => json!({ "mode": "local", "processId": pid }),
                    ("debugpy", Some(pid)) => json!({ "processId": pid, "justMyCode": false }),
                    _ => return fail(self, helper, "needs a pid or host:port".into()),
                };
                let mut args = args;
                args["cwd"] = json!(root);
                self.dap_start(adapter_lang, args, program, generation);
            }
        }
        // On the session (or dap_start's placeholder, which the session takes over)
        match &mut self.dap {
            Some(d) => {
                d.request = "attach";
                d.helper = helper;
            }
            None => {
                if let Some(mut h) = helper {
                    kill_group(&mut h);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_attach_targets_and_addresses() {
        let v: toml::Table = toml::from_str(
            r#"
            [[attach]]
            name = "api (k8s)"
            lang = "java"
            port = 5005
            before = "kubectl port-forward deploy/api 5005:5005"
            remote-root = "/app"

            [[attach]]
            pid = 4242

            [[attach]]
            name = "broken"
            "#,
        )
        .unwrap();
        let mut w = Vec::new();
        let list = parse_config(&v["attach"], "x.toml", &mut w);
        assert_eq!(list.len(), 2);
        assert_eq!(
            (list[0].name.as_str(), list[0].lang.as_deref(), list[0].address().as_str()),
            ("api (k8s)", Some("java"), "127.0.0.1:5005")
        );
        assert_eq!(list[0].remote_root.as_deref(), Some("/app"));
        assert_eq!((list[1].name.as_str(), list[1].address().as_str()), ("attach #2", "pid 4242"));
        assert!(w[0].contains("attach #3: needs port"), "{w:?}");
        assert_eq!(parse_addr("10.0.0.5:2345"), Some(("10.0.0.5".into(), 2345)));
        assert_eq!(parse_addr(":5678"), Some(("127.0.0.1".into(), 5678)));
        assert_eq!(parse_addr("5005"), Some(("127.0.0.1".into(), 5005)));
        assert_eq!(parse_addr("[::1]:9"), Some(("::1".into(), 9)));
        assert_eq!(parse_addr("host"), None);
    }

    #[test]
    fn helper_that_dies_reports_why() {
        let e = start_helper("echo nope >&2; exit 3", Path::new("/")).unwrap_err();
        assert!(e.contains("nope"), "{e}");
        let mut ok = start_helper("echo ready; sleep 30", Path::new("/")).unwrap();
        kill_group(&mut ok);
    }

    #[test]
    fn java_attach_that_cannot_start_does_not_stay_starting() {
        let mut ed = Editor::new(crate::config::Config::default());
        let t = AttachTarget {
            name: "jvm".into(),
            lang: Some("java".into()),
            host: "127.0.0.1".into(),
            port: Some(5005),
            ..Default::default()
        };
        ed.attach(t);
        assert!(ed.dap.is_some(), "starting");
        let ev = ed.events.recv_timeout(Duration::from_secs(5)).expect("attach job");
        ed.handle_event(ev);
        // No jdtls here — the reason is shown and F5·attach work again
        assert!(ed.dap.is_none());
        assert!(ed.status.as_ref().is_some_and(|s| s.0.contains("jdtls")), "{:?}", ed.status);
    }

    #[test]
    fn connects_to_a_listening_server() {
        // Fake remote server — hangs up right after connecting
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        std::thread::spawn(move || drop(l.accept()));
        assert!(connect("127.0.0.1", port).is_ok());
    }
}
