//! Java — what it takes to attach jdtls (Eclipse JDT Language Server), plus debugging (java-debug).
//!
//! Unlike other servers, jdtls needs extra care:
//! - **Workspace dir (`-data`)** — per project (build info, index cache). The `jdtls` launcher by default
//!   hashes only the folder *name*, so same-named projects mix → full-path hash instead:
//!   `~/.cache/tarae/jdtls/<name>-<hash>`.
//! - **java-debug plugin** — debugging is done by a plugin inside jdtls (enabled only by passing the jar via
//!   `initializationOptions.bundles`, at server start only). The jar comes from
//!   `~/.local/share/tarae/java-debug/`, or the one VS Code·Neovim (mason) already downloaded.
//! - **`language/status`** — startup progress (project import) arrives as this → status-line progress.
//! - **jdt:// definitions** — library/JDK class definitions come as `jdt://contents/…` URIs, not files →
//!   `java/classFileContents` gets source (or decompiled) into a read-only buffer (`Document::virtual_uri`).
//! - **Client command `java.apply.workspaceEdit`** — code actions wrap edits in this command (we apply it
//!   ourselves without asking the server back).

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::lsp::ServerSpec;

/// Is this jdtls (even if renamed in config, as long as the command is jdtls).
pub fn is_jdtls(spec: &ServerSpec) -> bool {
    spec.name == "jdtls"
        || Path::new(&spec.command).file_name().is_some_and(|n| n == "jdtls" || n == "jdtls.py")
}

/// Java build markers (Maven·Gradle).
pub(crate) const BUILD_FILES: &[&str] =
    &["pom.xml", "build.gradle", "build.gradle.kts", "settings.gradle", "settings.gradle.kts"];

/// Project root = walking up, the dir with the **topmost** build file (never past the repo root `.git`).
/// Avoids a jdtls per module in multi-module builds (parent pom · settings.gradle) — each is hundreds of MB.
/// None if there's no build file (→ generic rule: .git etc.).
pub fn project_root(file: &Path) -> Option<PathBuf> {
    crate::lsp::root_by_markers(file, BUILD_FILES)
}

/// Before launching the server: (adjusted command, initializationOptions).
pub fn prepare(spec: &ServerSpec, root: &Path) -> (ServerSpec, Value) {
    if !is_jdtls(spec) {
        return (spec.clone(), Value::Null);
    }
    let mut spec = spec.clone();
    if !spec.args.iter().any(|a| a == "-data")
        && let Some(dir) = workspace_dir(root)
    {
        spec.args.extend(["-data".to_string(), dir.display().to_string()]);
    }
    let bundles: Vec<String> = debug_bundle().into_iter().map(|p| p.display().to_string()).collect();
    let options = json!({
        "bundles": bundles,
        "extendedClientCapabilities": {
            "progressReportProvider": false,
            // Library/JDK class definitions as jdt:// URIs — source is fetched via java/classFileContents
            "classFileContentsSupport": true,
            "overrideMethodsPromptSupport": false,
            "advancedOrganizeImportsSupport": false,
            "generateToStringPromptSupport": false,
            "advancedGenerateAccessorsSupport": false,
            "resolveAdditionalTextEditsSupport": true,
        },
        "settings": { "java": {
            "signatureHelp": { "enabled": true },
            "inlayHints": { "parameterNames": { "enabled": "literals" } },
            // Going to dependency classes yields real source, not decompiled (downloads source jars)
            "maven": { "downloadSources": true },
            "eclipse": { "downloadSources": true },
        } },
    });
    (spec, options)
}

fn cache_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
        .map(|d| d.join("tarae"))
}

/// Per-project jdtls workspace dir — name (readable) + full-path hash (no collisions).
pub fn workspace_dir(root: &Path) -> Option<PathBuf> {
    let name = root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "root".into());
    // FNV-1a — must be stable across runs and Rust versions (std hasher doesn't guarantee that)
    let hash = root
        .to_string_lossy()
        .bytes()
        .fold(0xcbf29ce484222325u64, |h, b| (h ^ u64::from(b)).wrapping_mul(0x100000001b3));
    Some(cache_dir()?.join("jdtls").join(format!("{name}-{hash:016x}")))
}

/// java-debug plugin jar (`com.microsoft.java.debug.plugin-<ver>.jar`) — newest version. Once found it's
/// remembered (no directory scans per F5); while missing, every call looks again (it may get installed).
pub fn debug_bundle() -> Option<PathBuf> {
    static FOUND: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    if let Some(p) = FOUND.get() {
        return Some(p.clone());
    }
    let found = find_debug_bundle()?;
    Some(FOUND.get_or_init(|| found).clone())
}

fn find_debug_bundle() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let mut dirs: Vec<PathBuf> = Vec::new();
    dirs.extend(crate::runtime::data_dir().map(|d| d.join("java-debug")));
    if let Some(h) = &home {
        dirs.push(h.join(".local/share/nvim/mason/packages/java-debug-adapter/extension/server"));
        // VS Code extension (vscjava.vscode-java-debug-<ver>/server)
        for ext in [".vscode/extensions", ".vscode-server/extensions", ".cursor/extensions"] {
            if let Ok(rd) = std::fs::read_dir(h.join(ext)) {
                dirs.extend(
                    rd.flatten()
                        .filter(|e| e.file_name().to_string_lossy().starts_with("vscjava.vscode-java-debug-"))
                        .map(|e| e.path().join("server")),
                );
            }
        }
    }
    dirs.iter()
        .filter_map(|d| std::fs::read_dir(d).ok())
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("com.microsoft.java.debug.plugin-") && n.ends_with(".jar"))
        })
        .max_by_key(|p| version_key(p))
}

/// `…plugin-0.53.1.jar` → [0, 53, 1] (for version comparison).
fn version_key(p: &Path) -> Vec<u64> {
    let n = p.file_stem().and_then(|s| s.to_str()).unwrap_or_default();
    n.rsplit('-').next().unwrap_or_default().split('.').map(|x| x.parse().unwrap_or(0)).collect()
}

/// `language/status` → progress text (None when done).
pub fn status_text(params: &Value) -> Option<String> {
    match params["type"].as_str() {
        Some("Started" | "ServiceReady") => None,
        Some("Error") => Some(format!("error · {}", params["message"].as_str().unwrap_or_default())),
        _ => Some(params["message"].as_str().unwrap_or("starting").to_string()),
    }
}

// ── Debugging ────────────────────────────────────────────────────────────
//
// java-debug runs inside jdtls, so we ask jdtls in turn — main class (`vscode.java.resolveMainClass`) →
// classpath (`resolveClasspath`) → java binary (`resolveJavaExecutable`) → `startDebugSession` (= port).
// Attach to that port over TCP and the rest is the same DAP as other languages (dap.rs).

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step {
    /// Load java-debug into jdtls when it appeared after jdtls started — `java.reloadBundles`, no restart.
    Reload,
    MainClass,
    Classpath,
    JavaExec,
    Session,
}

#[derive(Clone, Debug)]
pub struct DebugPrep {
    generation: u64,
    pub(crate) cid: crate::lsp::ClientId,
    step: Step,
    /// The request for this step — other replies (an abandoned earlier run) are ignored.
    request: Option<u64>,
    file: PathBuf,
    root: PathBuf,
    main: String,
    project: String,
    module_paths: Value,
    class_paths: Value,
    java_exec: Value,
    /// Already loaded once (give up if "no command" again).
    reloaded: bool,
    /// Attach to a running JVM (JDWP host:port) — straight to a session, no main class/classpath lookup.
    attach: Option<(String, u16)>,
}

/// Pick from resolveMainClass: the current file's main, else the only one, else the first (+ other names).
fn pick_main(list: &[Value], file: &Path) -> Option<(String, String, Vec<String>)> {
    let same = |v: &Value| {
        v["filePath"].as_str().is_some_and(|f| {
            let f = Path::new(f);
            f == file || std::fs::canonicalize(f).is_ok_and(|c| c == file)
        })
    };
    let chosen = list.iter().find(|v| same(v)).or(list.first())?;
    let others = list
        .iter()
        .filter(|v| !std::ptr::eq(*v, chosen))
        .filter_map(|v| v["mainClass"].as_str().map(str::to_string))
        .collect();
    Some((
        chosen["mainClass"].as_str()?.to_string(),
        chosen["projectName"].as_str().unwrap_or_default().to_string(),
        others,
    ))
}

impl crate::editor::Editor {
    /// `F5` on a Java file — needs jdtls attached; if java-debug is missing, offers to download it.
    pub fn java_debug_start(&mut self, file: PathBuf, root: PathBuf, generation: u64) {
        self.java_debug_or_offer(file, root, generation, None);
    }

    /// Attach to a running JVM (`-agentlib:jdwp=transport=dt_socket,server=y,address=*:5005`) — attach.rs.
    pub fn java_attach(&mut self, file: PathBuf, root: PathBuf, host: String, port: u16, generation: u64) {
        self.java_debug_or_offer(file, root, generation, Some((host, port)));
    }

    fn java_debug_or_offer(
        &mut self,
        file: PathBuf,
        root: PathBuf,
        generation: u64,
        attach: Option<(String, u16)>,
    ) {
        if self.doc().lsp.client.is_none() {
            return self.set_error("debug: Java needs jdtls (the Java language server) on PATH");
        }
        if debug_bundle().is_none() {
            if !self.offer_popups {
                return self.set_error("debug: Java needs the java-debug plugin");
            }
            self.push_offer(crate::offer::Offer::new(
                "Java debugging".into(),
                "Download the java-debug plugin (Maven Central, ~3 MB)?",
                "Downloading java-debug…",
                "Couldn't download java-debug".into(),
                crate::offer::What::JavaDebug { file, root, attach },
            ));
            return;
        }
        self.java_debug_begin(file, root, generation, false, attach);
    }

    /// Start the prep steps — with `reload`, load java-debug into jdtls first (right after downloading it).
    fn java_debug_begin(
        &mut self,
        file: PathBuf,
        root: PathBuf,
        generation: u64,
        reload: bool,
        attach: Option<(String, u16)>,
    ) {
        use crate::dap::{Dap, State};
        // That file's jdtls (the user may have switched buffers during the download)
        let of_file =
            self.docs.iter().find(|d| d.path.as_deref() == Some(file.as_path())).and_then(|d| d.lsp.client);
        let Some(cid) = of_file.or(self.doc().lsp.client) else {
            return self.set_error("debug: Java needs jdtls (the Java language server) on PATH");
        };
        let file = std::fs::canonicalize(&file).unwrap_or(file);
        self.java_debug = Some(DebugPrep {
            generation,
            cid,
            step: match (reload, &attach) {
                (true, _) => Step::Reload,
                (false, Some(_)) => Step::Session,
                (false, None) => Step::MainClass,
            },
            request: None,
            file,
            root,
            main: String::new(),
            project: String::new(),
            module_paths: Value::Null,
            class_paths: Value::Null,
            java_exec: Value::Null,
            reloaded: false,
            attach: attach.clone(),
        });
        let what = match &attach {
            Some((h, p)) => format!("{h}:{p}"),
            None => "resolving main class".into(),
        };
        self.dap = Some(Dap::placeholder(State::Starting, what));
        self.java_debug_send();
    }

    fn java_debug_send(&mut self) {
        let Some(p) = self.java_debug.clone() else { return };
        let (command, args) = match p.step {
            Step::Reload => ("java.reloadBundles", json!([debug_bundle().into_iter().collect::<Vec<_>>()])),
            Step::MainClass => ("vscode.java.resolveMainClass", json!([crate::lsp::uri(&p.root)])),
            Step::Classpath => ("vscode.java.resolveClasspath", json!([p.main, p.project])),
            Step::JavaExec => ("vscode.java.resolveJavaExecutable", json!([p.main, p.project])),
            Step::Session => ("vscode.java.startDebugSession", json!([])),
        };
        let params = json!({ "command": command, "arguments": args });
        match self.lsp_send(p.cid, crate::lsp_editor::Kind::JavaDebug, "workspace/executeCommand", params) {
            Some(id) => {
                if let Some(p) = &mut self.java_debug {
                    p.request = Some(id);
                }
            }
            None => self.java_debug_fail("jdtls is still starting — try again in a moment".into()),
        }
    }

    pub(crate) fn java_debug_fail(&mut self, why: String) {
        self.java_debug = None;
        self.dap = None;
        self.set_error(format!("debug: {why}"));
    }

    /// jdtls reply to request `id` — on to the next step; at the last one (port), attach.
    pub fn java_debug_reply(
        &mut self,
        cid: crate::lsp::ClientId,
        id: u64,
        error: Option<&Value>,
        result: &Value,
    ) {
        if !self.java_debug.as_ref().is_some_and(|p| p.cid == cid && p.request == Some(id)) {
            return;
        }
        let Some(mut p) = self.java_debug.take() else { return };
        if p.generation != self.dap_generation {
            return;
        }
        if let Some(e) = error {
            let m = e["message"].as_str().unwrap_or("request failed");
            // jdtls started before the jar existed (no such command) — load it once and retry
            if m.contains("No delegateCommandHandler") && !p.reloaded && debug_bundle().is_some() {
                (p.step, p.reloaded) = (Step::Reload, true);
                self.java_debug = Some(p);
                return self.java_debug_send();
            }
            let hint = if m.contains("No delegateCommandHandler") || m.contains("not supported") {
                " (java-debug plugin not loaded — restart tarae after installing it)"
            } else {
                ""
            };
            return self.java_debug_fail(format!("{m}{hint}"));
        }
        match p.step {
            Step::Reload => {
                if result != &Value::Bool(true) {
                    return self.java_debug_fail("jdtls couldn't load java-debug — restart tarae".into());
                }
                p.step = if p.attach.is_some() { Step::Session } else { Step::MainClass };
                p.reloaded = true;
            }
            Step::MainClass => {
                let list = result.as_array().cloned().unwrap_or_default();
                let Some((main, project, others)) = pick_main(&list, &p.file) else {
                    return self.java_debug_fail("no main class found (public static void main)".into());
                };
                if !others.is_empty() {
                    self.note(format!("Debugging {main} — also: {}", others.join(", ")));
                }
                if let Some(d) = &mut self.dap {
                    d.program = main.rsplit('.').next().unwrap_or(&main).to_string();
                }
                (p.main, p.project, p.step) = (main, project, Step::Classpath);
            }
            Step::Classpath => {
                p.module_paths = result[0].clone();
                p.class_paths = result[1].clone();
                p.step = Step::JavaExec;
            }
            Step::JavaExec => {
                p.java_exec = result.clone();
                p.step = Step::Session;
            }
            Step::Session => {
                let Some(port) = result.as_u64().and_then(|n| u16::try_from(n).ok()) else {
                    return self.java_debug_fail(format!("no debug port from jdtls ({result})"));
                };
                let (request, args) = match &p.attach {
                    Some((host, port)) => {
                        ("attach", json!({ "hostName": host, "port": port, "timeout": 10000 }))
                    }
                    None => (
                        "launch",
                        json!({
                            "mainClass": p.main,
                            "projectName": p.project,
                            "modulePaths": p.module_paths,
                            "classPaths": p.class_paths,
                            "javaExec": p.java_exec,
                            "cwd": p.root,
                            "args": "",
                            "vmArgs": "",
                            "console": "internalConsole",
                            "shortenCommandLine": "none",
                            "stopOnEntry": false,
                        }),
                    ),
                };
                // Connecting is a socket wait — worker thread; a newer/ended session drops the result
                let generation = p.generation;
                self.events.jobs().spawn(move || {
                    let conn = std::net::TcpStream::connect(("127.0.0.1", port))
                        .and_then(|s| Ok((s.try_clone()?, s)))
                        .map_err(|e| format!("connect to java-debug: {e}"));
                    move |ed: &mut crate::editor::Editor| {
                        if ed.dap_generation != generation {
                            return;
                        }
                        let (reader, writer) = match conn {
                            Ok(c) => c,
                            Err(e) => return ed.java_debug_fail(e),
                        };
                        let program = ed.dap.as_ref().map(|d| d.program.clone()).unwrap_or_default();
                        ed.dap_connect(
                            "java",
                            Box::new(reader),
                            Box::new(writer),
                            None,
                            args,
                            program,
                            generation,
                        );
                        if let Some(d) = &mut ed.dap {
                            d.request = request;
                        }
                    }
                });
                return;
            }
        }
        self.java_debug = Some(p);
        self.java_debug_send();
    }
}

// ── jdt:// — library/JDK class sources ──────────────────────────────────

/// If the first location of a definition response is a jdt:// URI: (URI, start position).
pub fn jdt_location(result: &Value) -> Option<(String, Value)> {
    let (uri, pos) = crate::lsp::location_uris(result).into_iter().next()?;
    uri.starts_with("jdt://").then(|| (uri.to_string(), pos))
}

/// `jdt://contents/java.base/java.util/ArrayList.class?=…` → `java.util.ArrayList`.
pub fn class_title(uri: &str) -> String {
    let path = uri.split('?').next().unwrap_or(uri);
    let decoded = String::from_utf8_lossy(&crate::lsp::percent_decode(path)).into_owned();
    let mut parts = decoded.rsplit('/');
    let file = parts.next().unwrap_or_default();
    let class = file.strip_suffix(".class").or_else(|| file.strip_suffix(".java")).unwrap_or(file);
    match parts.next() {
        Some(pkg) if !pkg.is_empty() && pkg != "contents" => format!("{pkg}.{class}"),
        _ => class.to_string(),
    }
}

impl crate::editor::Editor {
    /// Go to a jdt:// definition — its buffer if already open, else fetch the source from jdtls.
    pub fn open_class_file(&mut self, cid: crate::lsp::ClientId, uri: String, pos: Value) {
        if let Some(i) = self.docs.iter().position(|d| d.virtual_uri.as_deref() == Some(uri.as_str())) {
            self.current = i;
            return self.jump_in_current(cid, &pos);
        }
        let params = json!({ "uri": uri });
        let kind = crate::lsp_editor::Kind::ClassFile { uri, pos };
        if self.lsp_send(cid, kind, "java/classFileContents", params).is_none() {
            self.set_status("jdtls is starting…");
        }
    }

    fn jump_in_current(&mut self, cid: crate::lsp::ClientId, pos: &Value) {
        let enc = self.encoding_of(cid);
        let doc = self.doc_mut();
        if let Some(b) = crate::lsp::from_position(&doc.text, pos, enc) {
            doc.set_selection(crate::selection::Selection::point(b));
        }
    }

    /// `java/classFileContents` reply for `uri` — source into a read-only buffer, then jump to `pos`.
    pub fn class_file_reply(
        &mut self,
        cid: crate::lsp::ClientId,
        uri: String,
        pos: &Value,
        error: Option<&Value>,
        result: &Value,
    ) {
        if let Some(e) = error {
            return self.set_error(format!("class source: {}", e["message"].as_str().unwrap_or("failed")));
        }
        let Some(text) = result.as_str().filter(|t| !t.trim().is_empty()) else {
            return self.set_status(format!("No source for {}", class_title(&uri)));
        };
        let doc_id = self.alloc_id();
        let mut doc = crate::document::Document::from_str(doc_id, text);
        doc.title = Some(class_title(&uri));
        doc.throwaway = true;
        doc.virtual_uri = Some(uri);
        self.docs.push(doc);
        self.current = self.docs.len() - 1;
        if let Some(spec) = crate::syntax::spec("java") {
            self.load_language(doc_id, spec);
        }
        self.jump_in_current(cid, pos);
    }
}

// ── java-debug download (Maven Central) ──────────────────────────────────

const MAVEN: &str = "https://repo1.maven.org/maven2/com/microsoft/java/com.microsoft.java.debug.plugin";

/// Download one URL — curl, else wget (both honor `https_proxy` for corporate proxies).
fn http_get(url: &str) -> Result<Vec<u8>, String> {
    let run = |cmd: &str, args: &[&str]| std::process::Command::new(cmd).args(args).output();
    let out = match run("curl", &["-fsSL", "--retry", "2", url]) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            run("wget", &["-qO-", url]).map_err(|_| "needs curl or wget".to_string())?
        }
        r => r.map_err(|e| format!("curl: {e}"))?,
    };
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(err.lines().last().unwrap_or("download failed").trim().to_string());
    }
    Ok(out.stdout)
}

/// Release version from maven-metadata.xml (`<release>`, else `<latest>`).
fn release_version(meta: &str) -> Option<&str> {
    let tag = |t: &str| meta.split(&format!("<{t}>")).nth(1).and_then(|r| r.split('<').next()).map(str::trim);
    tag("release").or_else(|| tag("latest")).filter(|v| !v.is_empty())
}

/// Download the latest version into `~/.local/share/tarae/java-debug/` (on a worker thread).
pub fn download_debug_plugin() -> Result<PathBuf, String> {
    let meta = String::from_utf8_lossy(&http_get(&format!("{MAVEN}/maven-metadata.xml"))?).into_owned();
    let version = release_version(&meta).ok_or("no version in maven-metadata.xml")?;
    let name = format!("com.microsoft.java.debug.plugin-{version}.jar");
    let jar = http_get(&format!("{MAVEN}/{version}/{name}"))?;
    if !jar.starts_with(b"PK") {
        return Err("download is not a jar".into());
    }
    let dir = crate::runtime::data_dir().ok_or("no home directory")?.join("java-debug");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let tmp = dir.join(format!(".{name}.part"));
    std::fs::write(&tmp, &jar).map_err(|e| e.to_string())?;
    let out = dir.join(&name);
    std::fs::rename(&tmp, &out).map_err(|e| e.to_string())?;
    Ok(out)
}

impl crate::editor::Editor {
    /// `y` on the offer — download, load into jdtls (no restart), then continue debugging that file.
    pub fn java_debug_download(&mut self, id: u64) {
        self.events.jobs().spawn(move || {
            let result = download_debug_plugin();
            move |ed: &mut crate::editor::Editor| {
                let Some(crate::offer::What::JavaDebug { file, root, attach }) =
                    ed.offer_by_id(id).map(|o| o.what.clone())
                else {
                    return;
                };
                match result {
                    Ok(_) => {
                        ed.offer_finished(id, Ok(()));
                        ed.set_success("java-debug installed — starting the debugger");
                        ed.dap_generation += 1;
                        let generation = ed.dap_generation;
                        ed.java_debug_begin(file, root, generation, true, attach);
                    }
                    Err(e) => ed.offer_finished(id, Err(e)),
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jdtls_gets_a_workspace_per_project_path() {
        let spec = ServerSpec { name: "jdtls".into(), command: "jdtls".into(), args: vec![] };
        let (a, opts) = prepare(&spec, Path::new("/work/a/app"));
        let (b, _) = prepare(&spec, Path::new("/work/b/app"));
        let data = |s: &ServerSpec| s.args[s.args.iter().position(|x| x == "-data").unwrap() + 1].clone();
        assert!(data(&a).contains("/jdtls/app-"), "{}", data(&a));
        assert_ne!(data(&a), data(&b), "same name, different path → different dir");
        assert_eq!(data(&a), data(&prepare(&spec, Path::new("/work/a/app")).0), "same path → same dir");
        assert!(opts["bundles"].is_array());
        // If the user passed -data, keep it
        let own = ServerSpec { args: vec!["-data".into(), "/x".into()], ..spec.clone() };
        assert_eq!(prepare(&own, Path::new("/p")).0.args, ["-data", "/x"]);
        // Other servers are left untouched
        let ra = ServerSpec { name: "rust-analyzer".into(), command: "rust-analyzer".into(), args: vec![] };
        assert_eq!(prepare(&ra, Path::new("/p")), (ra.clone(), Value::Null));
    }

    #[test]
    fn project_root_is_the_topmost_build_below_the_repo() {
        let t = std::env::temp_dir().join(format!("tarae-java-root-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&t);
        let module = t.join("repo/svc/app/src/main/java/demo");
        std::fs::create_dir_all(&module).unwrap();
        std::fs::create_dir_all(t.join("repo/.git")).unwrap();
        std::fs::write(t.join("pom.xml"), "").unwrap(); // outside the repo — not crossed
        std::fs::write(t.join("repo/svc/settings.gradle.kts"), "").unwrap();
        std::fs::write(t.join("repo/svc/app/build.gradle.kts"), "").unwrap();
        let file = module.join("App.java");
        assert_eq!(project_root(&file), Some(t.join("repo/svc")), "the settings.gradle dir, not the module");
        std::fs::remove_file(t.join("repo/svc/settings.gradle.kts")).unwrap();
        assert_eq!(project_root(&file), Some(t.join("repo/svc/app")));
        std::fs::remove_file(t.join("repo/svc/app/build.gradle.kts")).unwrap();
        assert_eq!(project_root(&file), None, "no build file → generic rule");
        let _ = std::fs::remove_dir_all(&t);
    }

    #[test]
    fn picks_the_main_of_the_current_file() {
        let list = [
            json!({ "mainClass": "app.Tool", "projectName": "p", "filePath": "/p/src/app/Tool.java" }),
            json!({ "mainClass": "app.Main", "projectName": "p", "filePath": "/p/src/app/Main.java" }),
        ];
        let (m, proj, others) = pick_main(&list, Path::new("/p/src/app/Main.java")).unwrap();
        assert_eq!((m.as_str(), proj.as_str(), others), ("app.Main", "p", vec!["app.Tool".to_string()]));
        let (m, ..) = pick_main(&list, Path::new("/p/src/app/Util.java")).unwrap();
        assert_eq!(m, "app.Tool", "first one if the current file has no main");
        assert!(pick_main(&[], Path::new("/x")).is_none());
    }

    #[test]
    fn jdt_definitions_and_titles() {
        let uri = "jdt://contents/java.base/java.util/ArrayList.class?=jproj/%5C/usr/lib/jvm%3Cjava.util(ArrayList.class";
        let r = json!([{ "uri": uri, "range": { "start": { "line": 40, "character": 13 }, "end": {} } }]);
        let (u, pos) = jdt_location(&r).unwrap();
        assert_eq!((u.as_str(), pos["line"].as_u64()), (uri, Some(40)));
        assert!(jdt_location(&json!([{ "uri": "file:///a/B.java", "range": { "start": {} } }])).is_none());
        assert_eq!(class_title(uri), "java.util.ArrayList");
        assert_eq!(class_title("jdt://contents/rt.jar/java.util/Map%24Entry.class"), "java.util.Map$Entry");
        assert_eq!(class_title("jdt://contents/src.zip/java.util/List.java"), "java.util.List");
    }

    #[test]
    fn reads_the_release_version_from_maven_metadata() {
        let meta = "<metadata><versioning><latest>0.54.0-SNAPSHOT</latest><release>0.53.1</release></versioning></metadata>";
        assert_eq!(release_version(meta), Some("0.53.1"));
        assert_eq!(release_version("<latest>0.9</latest>"), Some("0.9"));
        assert_eq!(release_version("<x/>"), None);
    }

    #[test]
    fn picks_newest_debug_plugin_and_reads_status() {
        let a = Path::new("com.microsoft.java.debug.plugin-0.9.0.jar");
        let b = Path::new("com.microsoft.java.debug.plugin-0.53.1.jar");
        assert!(version_key(b) > version_key(a));
        assert_eq!(
            status_text(&json!({"type": "Starting", "message": "Init..."})).as_deref(),
            Some("Init...")
        );
        assert_eq!(status_text(&json!({"type": "ServiceReady", "message": "ServiceReady"})), None);
    }
}
