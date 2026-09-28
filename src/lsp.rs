//! LSP client — **fully asynchronous** (principle 4: input and rendering never block on LSP).
//!
//! Three threads per server: writer (channel → stdin), reader (stdout → main-loop events), stderr drain.
//! The main thread only pushes to channels — editing never stalls even if a busy server stops reading stdin.
//! Big bodies (didOpen, full sync) are serialized on the writer thread too (we pass an O(1) rope clone).
//! Asks for UTF-8 positions first — editor positions are bytes, no conversion. UTF-16-only servers also work.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Sender};
use std::thread;

use ropey::Rope;
use serde_json::{Value, json};

use crate::editor::Editor;
use crate::event::Event;
use crate::movement as mv;

pub type ClientId = usize;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Encoding {
    Utf8,
    Utf16,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServerSpec {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
}

/// (name, command, args)
type Candidate = (&'static str, &'static str, &'static [&'static str]);

/// Default servers per language — the first on PATH wins. Overridden by `[lsp.<name>]`·`[lang.<lang>] lsp`.
const DEFAULTS: &[(&str, &[Candidate])] = &[
    ("rust", &[("rust-analyzer", "rust-analyzer", &[])]),
    ("go", &[("gopls", "gopls", &[])]),
    (
        "python",
        &[
            ("basedpyright", "basedpyright-langserver", &["--stdio"]),
            ("pyright", "pyright-langserver", &["--stdio"]),
            ("pylsp", "pylsp", &[]),
        ],
    ),
    ("typescript", &[("typescript-language-server", "typescript-language-server", &["--stdio"])]),
    ("tsx", &[("typescript-language-server", "typescript-language-server", &["--stdio"])]),
    ("javascript", &[("typescript-language-server", "typescript-language-server", &["--stdio"])]),
    ("jsx", &[("typescript-language-server", "typescript-language-server", &["--stdio"])]),
    ("c", &[("clangd", "clangd", &[])]),
    ("cpp", &[("clangd", "clangd", &[])]),
    ("lua", &[("lua-language-server", "lua-language-server", &[])]),
    ("zig", &[("zls", "zls", &[])]),
    ("bash", &[("bash-language-server", "bash-language-server", &["start"])]),
    ("toml", &[("taplo", "taplo", &["lsp", "stdio"])]),
    ("yaml", &[("yaml-language-server", "yaml-language-server", &["--stdio"])]),
    ("helm", &[("helm_ls", "helm_ls", &["serve"])]),
    ("markdown", &[("marksman", "marksman", &["server"])]),
    ("nix", &[("nil", "nil", &[])]),
    // jdtls workspace dir and debug plugin are added by java.rs
    ("java", &[("jdtls", "jdtls", &[])]),
];

/// Language id (LSP languageId) — mostly the language name as-is.
pub fn language_id(lang: &str) -> &str {
    match lang {
        "tsx" => "typescriptreact",
        "jsx" => "javascriptreact",
        "bash" => "shellscript",
        other => other,
    }
}

/// Servers for this language: the configured list if any, else the first default candidate on PATH.
pub fn find_server(lang: &str, config: &LspConfig) -> Option<ServerSpec> {
    candidates(lang, config).into_iter().find(|s| on_path(&s.command))
}

/// The servers configured for `lang` (or the defaults), installed or not.
pub fn candidates(lang: &str, config: &LspConfig) -> Vec<ServerSpec> {
    let names: Vec<&str> = match config.languages.get(lang) {
        Some(names) => names.iter().map(String::as_str).collect(),
        None => DEFAULTS
            .iter()
            .find(|(l, _)| *l == lang)
            .map(|(_, list)| list.iter().map(|(name, ..)| *name).collect())
            .unwrap_or_default(),
    };
    names.into_iter().filter_map(|n| config.servers.get(n).cloned().or_else(|| default_spec(n))).collect()
}

fn default_spec(name: &str) -> Option<ServerSpec> {
    DEFAULTS.iter().flat_map(|(_, l)| l.iter()).find(|(n, _, _)| *n == name).map(|(n, c, a)| ServerSpec {
        name: n.to_string(),
        command: c.to_string(),
        args: a.iter().map(|s| s.to_string()).collect(),
    })
}

fn on_path(cmd: &str) -> bool {
    if cmd.contains('/') {
        return Path::new(cmd).is_file();
    }
    std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(cmd).is_file()))
}

/// Config `[lsp.<name>] command/args` + `[lang.<lang>] lsp = ["name", …]`.
/// One inlay hint — inserted as dim text before the character at `pos` (text includes padding).
#[derive(Clone, Debug, PartialEq)]
pub struct InlayHint {
    pub pos: usize,
    pub text: String,
}

/// `InlayHint[]` response → hints in position order. label is a string or parts (`{value}`).
pub fn inlay_hints(text: &Rope, v: &Value, enc: Encoding) -> Vec<InlayHint> {
    let mut out: Vec<InlayHint> = v
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|h| {
            let pos = from_position(text, &h["position"], enc)?;
            let label = match &h["label"] {
                Value::String(s) => s.clone(),
                Value::Array(parts) => parts.iter().filter_map(|p| p["value"].as_str()).collect(),
                _ => return None,
            };
            let label = label.replace(['\n', '\r'], " ");
            let pad = |k: &str| if h[k].as_bool() == Some(true) { " " } else { "" };
            Some(InlayHint { pos, text: format!("{}{label}{}", pad("paddingLeft"), pad("paddingRight")) })
        })
        .collect();
    out.sort_by_key(|h| h.pos);
    out
}

#[derive(Clone, Debug, Default)]
pub struct LspConfig {
    pub enabled: bool,
    pub servers: HashMap<String, ServerSpec>,
    pub languages: HashMap<String, Vec<String>>,
}

/// Project markers of any language (`.git` is handled by `root_by_markers`).
const MARKERS: &[&str] = &[
    "Cargo.toml",
    "go.mod",
    "package.json",
    "pyproject.toml",
    "compile_commands.json",
    "pom.xml",
    "build.gradle",
    "build.gradle.kts",
    "settings.gradle",
    "settings.gradle.kts",
];

/// Markers that make a project root for this language — other languages' don't count (a Python file under
/// a repo-root package.json). Unknown languages take any marker.
fn markers_for(lang: &str) -> &'static [&'static str] {
    match lang {
        "rust" => &["Cargo.toml"],
        "go" => &["go.work", "go.mod"],
        "python" => &["pyproject.toml", "setup.py", "setup.cfg"],
        "typescript" | "tsx" | "javascript" | "jsx" => &["package.json", "tsconfig.json", "jsconfig.json"],
        "c" | "cpp" => &["compile_commands.json", "CMakeLists.txt"],
        "java" => crate::java::BUILD_FILES,
        _ => MARKERS,
    }
}

/// The **topmost** dir with a marker, walking up but never past the repo root (`.git`) or into `$HOME` —
/// one server per workspace, not one per member crate/module (each is hundreds of MB). None if no marker.
pub fn root_by_markers(file: &Path, markers: &[&str]) -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let mut dir = file.parent()?.to_path_buf();
    let mut top = None;
    loop {
        if home.as_deref() == Some(dir.as_path()) {
            return top;
        }
        if markers.iter().any(|m| dir.join(m).is_file()) {
            top = Some(dir.clone());
        }
        if dir.join(".git").exists() || !dir.pop() {
            return top;
        }
    }
}

/// Nearest repo root (`.git`), else the file's dir.
fn repo_or_dir(file: &Path) -> PathBuf {
    let start = file.parent().unwrap_or(file);
    start.ancestors().find(|d| d.join(".git").exists()).unwrap_or(start).to_path_buf()
}

/// Project root by any language's markers (`root_by_markers`), else the repo root, else the file's dir.
pub fn find_root(file: &Path) -> PathBuf {
    root_by_markers(file, MARKERS).unwrap_or_else(|| repo_or_dir(file))
}

/// Per-language project root — the topmost of this language's markers below the repo root.
pub fn find_root_for(lang: &str, file: &Path) -> PathBuf {
    root_by_markers(file, markers_for(lang)).unwrap_or_else(|| repo_or_dir(file))
}

// ── URI ──────────────────────────────────────────────────────────────────

pub fn uri(path: &Path) -> String {
    let mut s = String::from("file://");
    for b in path.to_string_lossy().bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'_' | b'.' | b'~' => s.push(b as char),
            _ => s.push_str(&format!("%{b:02X}")),
        }
    }
    s
}

pub fn path_from_uri(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    Some(PathBuf::from(String::from_utf8(percent_decode(rest)).ok()?))
}

/// `%XX` escapes → bytes (a malformed escape stays as-is).
pub fn percent_decode(s: &str) -> Vec<u8> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && let Some(v) = s.get(i + 1..i + 3).and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    out
}

// ── Position conversion ──────────────────────────────────────────────────

pub fn to_position(text: &Rope, byte: usize, enc: Encoding) -> Value {
    let byte = byte.min(text.len_bytes());
    let line = text.byte_to_line(byte);
    let ls = text.line_to_byte(line);
    let character = match enc {
        Encoding::Utf8 => byte - ls,
        Encoding::Utf16 => text.byte_slice(ls..byte).chars().map(char::len_utf16).sum(),
    };
    json!({ "line": line, "character": character })
}

/// Server position → byte (column overflow → line end; line past the end → document end; mid-character →
/// character start).
pub fn from_position(text: &Rope, pos: &Value, enc: Encoding) -> Option<usize> {
    let line = pos["line"].as_u64()? as usize;
    let character = pos["character"].as_u64()? as usize;
    // `{line: lineCount, character: 0}` = end of document (whole-document edits end there)
    if line >= text.len_lines() {
        return Some(text.len_bytes());
    }
    let (ls, le) = (mv::line_start(text, line), mv::line_end(text, line));
    Some(match enc {
        Encoding::Utf8 => text.char_to_byte(text.byte_to_char((ls + character).min(le))),
        Encoding::Utf16 => {
            let (mut units, mut byte) = (0, ls);
            for c in text.byte_slice(ls..le).chars() {
                if units >= character {
                    break;
                }
                units += c.len_utf16();
                byte += c.len_utf8();
            }
            byte
        }
    })
}

// ── Client ───────────────────────────────────────────────────────────────

/// What goes to the writer thread — large bodies are serialized there.
pub enum Out {
    Body(String),
    Lazy(Box<dyn FnOnce() -> String + Send>),
}

/// One `$/progress` token — reports update message/percentage, the title stays from `begin`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Progress {
    pub title: String,
    pub message: Option<String>,
    pub percentage: Option<u64>,
}

impl Progress {
    /// `begin`/`report` value — fields a report leaves out keep their previous value (spec).
    pub fn update(&mut self, v: &Value) {
        if let Some(t) = v["title"].as_str() {
            self.title = t.to_string();
        }
        if let Some(m) = v["message"].as_str() {
            self.message = Some(m.to_string());
        }
        if let Some(p) = v["percentage"].as_u64() {
            self.percentage = Some(p);
        }
    }

    pub fn text(&self) -> String {
        let mut s = self.title.clone();
        if let Some(m) = self.message.as_deref().filter(|m| !m.is_empty()) {
            if !s.is_empty() {
                s.push_str(" · ");
            }
            s.push_str(m);
        }
        if let Some(p) = self.percentage {
            s.push_str(&format!(" {p}%"));
        }
        s
    }
}

pub struct Client {
    pub id: ClientId,
    pub name: String,
    pub root: PathBuf,
    tx: Sender<Out>,
    next_req: u64,
    /// Whether the initialize response has arrived — until then, outgoing messages are queued.
    pub ready: bool,
    queue: Vec<Out>,
    pub encoding: Encoding,
    /// Work in progress (token → what it's doing).
    pub progress: HashMap<String, Progress>,
    /// Server capabilities (initialize response) — completion trigger characters etc.
    pub caps: Value,
    child: Child,
    /// For the log: how long it took to be ready.
    pub started: std::time::Instant,
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

impl Client {
    pub fn start(
        id: ClientId,
        spec: &ServerSpec,
        root: &Path,
        init_options: Value,
        events: Sender<Event>,
    ) -> Result<Client, String> {
        let mut child = Command::new(&spec.command)
            .args(&spec.args)
            .current_dir(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("{}: {e}", spec.command))?;
        let mut stdin = child.stdin.take().ok_or("no stdin")?;
        let stdout = child.stdout.take().ok_or("no stdout")?;
        let mut stderr = child.stderr.take().ok_or("no stderr")?;
        let (tx, rx) = mpsc::channel::<Out>();
        thread::spawn(move || {
            while let Ok(out) = rx.recv() {
                let body = match out {
                    Out::Body(s) => s,
                    Out::Lazy(f) => f(),
                };
                let frame = format!("Content-Length: {}\r\n\r\n{body}", body.len());
                if stdin.write_all(frame.as_bytes()).and_then(|_| stdin.flush()).is_err() {
                    break;
                }
            }
        });
        thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            while let Some(msg) = read_message(&mut reader) {
                let apply = move |ed: &mut Editor| ed.on_lsp_message(id, msg);
                if events.send(Event::Job(Box::new(apply))).is_err() {
                    return;
                }
            }
            // stdout closed — the server exited (or crashed)
            let _ = events.send(Event::Job(Box::new(move |ed: &mut Editor| ed.lsp_exited(id))));
        });
        // stderr → the log (`:log-open`) — also keeps the pipe from filling up and stalling the server
        let name = spec.name.clone();
        thread::spawn(move || {
            let mut lines = BufReader::new(&mut stderr);
            let mut line = String::new();
            loop {
                line.clear();
                match lines.read_line(&mut line) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => crate::log::line(&name, "stderr", &line),
                }
            }
        });
        crate::log::line(
            &spec.name,
            "start",
            &format!("{} {} (in {})", spec.command, spec.args.join(" "), root.display()),
        );
        let client = Client {
            id,
            name: spec.name.clone(),
            root: root.to_path_buf(),
            tx,
            next_req: 1,
            ready: false,
            queue: Vec::new(),
            encoding: Encoding::Utf16,
            progress: HashMap::new(),
            caps: Value::Null,
            child,
            started: std::time::Instant::now(),
        };
        let root_uri = uri(root);
        let init = json!({
            "jsonrpc": "2.0", "id": 0, "method": "initialize",
            "params": {
                "processId": std::process::id(),
                "initializationOptions": init_options,
                "clientInfo": { "name": "tarae", "version": env!("CARGO_PKG_VERSION") },
                "rootUri": root_uri,
                "rootPath": root.to_string_lossy(),
                "workspaceFolders": [{ "uri": root_uri, "name": root.file_name().map(|n| n.to_string_lossy()).unwrap_or_default() }],
                "capabilities": {
                    "general": { "positionEncodings": ["utf-8", "utf-16"] },
                    "window": { "workDoneProgress": true, "showMessage": {} },
                    "workspace": {
                        "inlayHint": { "refreshSupport": true },
                        "configuration": true,
                        "workspaceFolders": true,
                        "symbol": {},
                        "applyEdit": true,
                        "workspaceEdit": {
                            "documentChanges": true,
                            "resourceOperations": ["create", "rename", "delete"],
                            "failureHandling": "abort",
                        },
                        "executeCommand": {},
                    },
                    "textDocument": {
                        "synchronization": { "didSave": true, "dynamicRegistration": false },
                        "publishDiagnostics": { "relatedInformation": false },
                        "hover": { "contentFormat": ["plaintext", "markdown"] },
                        "definition": { "linkSupport": true },
                        "declaration": { "linkSupport": true },
                        "typeDefinition": { "linkSupport": true },
                        "implementation": { "linkSupport": true },
                        "documentSymbol": { "hierarchicalDocumentSymbolSupport": true },
                        "references": {},
                        "documentHighlight": {},
                        // Without this the server may send only Commands — rust-analyzer sends none at all.
                        "codeAction": {
                            "codeActionLiteralSupport": { "codeActionKind": { "valueSet": [
                                "", "quickfix", "refactor", "refactor.extract", "refactor.inline",
                                "refactor.rewrite", "source", "source.organizeImports"
                            ] } },
                            "resolveSupport": { "properties": ["edit", "command"] },
                            "dataSupport": true,
                            "isPreferredSupport": true,
                        },
                        "rename": { "prepareSupport": false },
                        "inlayHint": { "dynamicRegistration": false },
                        "signatureHelp": {
                            "signatureInformation": {
                                "documentationFormat": ["markdown", "plaintext"],
                                "parameterInformation": { "labelOffsetSupport": true },
                                "activeParameterSupport": true,
                            },
                            "contextSupport": true,
                        },
                        "completion": {
                            "completionItem": {
                                "snippetSupport": true,
                                "labelDetailsSupport": true,
                                "insertReplaceSupport": false,
                                "documentationFormat": ["markdown", "plaintext"],
                                "resolveSupport": { "properties": ["documentation", "detail", "additionalTextEdits"] },
                            },
                            "contextSupport": true,
                        },
                        "formatting": {},
                    },
                },
            },
        });
        let _ = client.tx.send(Out::Body(init.to_string()));
        Ok(client)
    }

    fn send(&mut self, out: Out) {
        if self.ready {
            let _ = self.tx.send(out);
        } else {
            self.queue.push(out);
        }
    }

    pub fn request(&mut self, method: &str, params: Value) -> u64 {
        let id = self.next_req;
        self.next_req += 1;
        self.send(Out::Body(
            json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }).to_string(),
        ));
        id
    }

    pub fn notify(&mut self, method: &str, params: Value) {
        self.send(Out::Body(json!({ "jsonrpc": "2.0", "method": method, "params": params }).to_string()));
    }

    /// Notification serialized on the writer thread (didOpen, full sync — carries the whole document).
    pub fn notify_lazy(&mut self, f: impl FnOnce() -> String + Send + 'static) {
        self.send(Out::Lazy(Box::new(f)));
    }

    pub fn reply(&mut self, id: &Value, result: Value) {
        let _ = self.tx.send(Out::Body(json!({ "jsonrpc": "2.0", "id": id, "result": result }).to_string()));
    }

    pub fn on_initialized(&mut self, result: &Value) {
        self.caps = result["capabilities"].clone();
        self.encoding = match result["capabilities"]["positionEncoding"].as_str() {
            Some("utf-8") => Encoding::Utf8,
            _ => Encoding::Utf16,
        };
        self.ready = true;
        let _ = self
            .tx
            .send(Out::Body(json!({ "jsonrpc": "2.0", "method": "initialized", "params": {} }).to_string()));
        for out in std::mem::take(&mut self.queue) {
            let _ = self.tx.send(out);
        }
    }

    pub fn progress_text(&self) -> Option<String> {
        self.progress.values().next().map(|p| format!("{}: {}", self.name, p.text()))
    }
}

/// Read one `Content-Length` frame.
pub fn read_message(r: &mut impl BufRead) -> Option<Value> {
    let mut len = None;
    loop {
        let mut line = String::new();
        if r.read_line(&mut line).ok()? == 0 {
            return None;
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some(v) = line.strip_prefix("Content-Length:") {
            len = v.trim().parse::<usize>().ok();
        }
    }
    let mut buf = vec![0; len?];
    r.read_exact(&mut buf).ok()?;
    serde_json::from_slice(&buf).ok()
}

/// One diagnostic (byte range).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    pub from: usize,
    pub to: usize,
    /// 1 error · 2 warning · 3 info · 4 hint
    pub severity: u8,
    pub message: String,
    /// Raw object from the server — must be sent back as-is in the code action request's context.
    pub raw: Value,
}

/// Location list (Location | Location[] | LocationLink[]) → (URI, start position object).
pub fn location_uris(v: &Value) -> Vec<(&str, Value)> {
    fn one(l: &Value) -> Option<(&str, Value)> {
        let uri = l["uri"].as_str().or_else(|| l["targetUri"].as_str())?;
        let range =
            if l["targetSelectionRange"].is_object() { &l["targetSelectionRange"] } else { &l["range"] };
        Some((uri, range["start"].clone()))
    }
    match v {
        Value::Array(a) => a.iter().filter_map(one).collect(),
        Value::Object(_) => one(v).into_iter().collect(),
        _ => Vec::new(),
    }
}

/// Location list → (file path, start position object) — non-file URIs are skipped.
pub fn locations(v: &Value) -> Vec<(PathBuf, Value)> {
    location_uris(v).into_iter().filter_map(|(u, pos)| Some((path_from_uri(u)?, pos))).collect()
}

/// hover contents → text lines (for markdown, only code fences are stripped).
pub fn hover_text(v: &Value) -> String {
    // MarkedString {language, value} → code block, merged into one markdown (rendered by markdown.rs)
    fn part(v: &Value) -> String {
        match v {
            Value::String(s) => s.clone(),
            Value::Object(o) => {
                let value = o.get("value").and_then(Value::as_str).unwrap_or_default();
                match o.get("language").and_then(Value::as_str) {
                    Some(lang) => format!("```{lang}\n{value}\n```"),
                    None => value.to_string(),
                }
            }
            Value::Array(a) => a.iter().map(part).collect::<Vec<_>>().join("\n\n---\n\n"),
            _ => String::new(),
        }
    }
    part(&v["contents"]).trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uri_roundtrip() {
        let p = Path::new("/tmp/타래 dir/a b.rs");
        let u = uri(p);
        assert!(u.starts_with("file:///tmp/%ED%83%80"));
        assert_eq!(path_from_uri(&u).as_deref(), Some(p));
    }

    #[test]
    fn positions_in_both_encodings() {
        let t = Rope::from_str("ab\n타래👍x\n");
        // 'x' = after 3 + 3 + 3 + 4 bytes / after 1 + 1 + 2 units in UTF-16
        let x = 3 + 3 + 3 + 4;
        assert_eq!(to_position(&t, x, Encoding::Utf8), json!({"line": 1, "character": 10}));
        assert_eq!(to_position(&t, x, Encoding::Utf16), json!({"line": 1, "character": 4}));
        assert_eq!(from_position(&t, &json!({"line": 1, "character": 10}), Encoding::Utf8), Some(x));
        assert_eq!(from_position(&t, &json!({"line": 1, "character": 4}), Encoding::Utf16), Some(x));
        // Overflowing column → line end
        assert_eq!(from_position(&t, &json!({"line": 0, "character": 99}), Encoding::Utf8), Some(2));
    }

    #[test]
    fn framing_and_locations() {
        let body = r#"{"jsonrpc":"2.0","id":1,"result":[{"targetUri":"file:///a.rs","targetSelectionRange":{"start":{"line":3,"character":4}}}]}"#;
        let raw = format!("Content-Length: {}\r\n\r\n{body}", body.len());
        let msg = read_message(&mut raw.as_bytes()).unwrap();
        let locs = locations(&msg["result"]);
        assert_eq!(locs, vec![(PathBuf::from("/a.rs"), json!({"line": 3, "character": 4}))]);
        let h = hover_text(&json!({"contents": {"kind": "markdown", "value": "```rust\nfn f()\n```\ndocs"}}));
        assert_eq!(h, "```rust\nfn f()\n```\ndocs", "markdown as-is (rendering parses it)");
        let old = hover_text(&json!({"contents": [{"language": "rust", "value": "fn f()"}, "docs"]}));
        assert_eq!(old, "```rust\nfn f()\n```\n\n---\n\ndocs", "old MarkedString becomes a code block");
    }

    #[test]
    fn document_end_position_is_the_end_of_the_text() {
        let t = Rope::from_str("a\nb\n");
        // Whole-document edit range ends at {line: lineCount, character: 0}
        assert_eq!(from_position(&t, &json!({"line": 2, "character": 0}), Encoding::Utf8), Some(4));
        assert_eq!(from_position(&t, &json!({"line": 9, "character": 0}), Encoding::Utf16), Some(4));
        assert_eq!(from_position(&t, &json!({"line": 1, "character": 5}), Encoding::Utf8), Some(3));
        let t = Rope::from_str("a\nb");
        assert_eq!(from_position(&t, &json!({"line": 2, "character": 0}), Encoding::Utf8), Some(3));
    }

    #[test]
    fn progress_reports_keep_the_title() {
        let mut p = Progress::default();
        p.update(&json!({"kind": "begin", "title": "Indexing", "percentage": 0}));
        for n in [5, 6, 7] {
            p.update(&json!({"kind": "report", "percentage": n}));
        }
        assert_eq!(p.text(), "Indexing 7%");
        p.update(&json!({"kind": "report", "message": "3/9 crates"}));
        assert_eq!(p.text(), "Indexing · 3/9 crates 7%");
    }

    #[test]
    fn finds_root_by_markers() {
        let here = Path::new(env!("CARGO_MANIFEST_DIR"));
        assert_eq!(find_root(&here.join("src/lsp.rs")), here);
    }

    #[test]
    fn root_is_the_topmost_marker_below_the_repo() {
        let t = std::env::temp_dir().join(format!("tarae-lsp-root-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&t);
        let member = t.join("repo/crates/core/src");
        std::fs::create_dir_all(&member).unwrap();
        std::fs::create_dir_all(t.join("repo/.git")).unwrap();
        std::fs::write(t.join("Cargo.toml"), "").unwrap(); // outside the repo — not crossed
        std::fs::write(t.join("repo/Cargo.toml"), "[workspace]").unwrap();
        std::fs::write(t.join("repo/crates/core/Cargo.toml"), "").unwrap();
        std::fs::write(t.join("repo/crates/package.json"), "").unwrap();
        let file = member.join("lib.rs");
        assert_eq!(find_root_for("rust", &file), t.join("repo"), "one server per workspace, not per member");
        assert_eq!(find_root_for("python", &file), t.join("repo"), "no python marker → repo root");
        assert_eq!(find_root_for("typescript", &file), t.join("repo/crates"));
        std::fs::remove_file(t.join("repo/Cargo.toml")).unwrap();
        assert_eq!(find_root_for("rust", &file), t.join("repo/crates/core"));
        let _ = std::fs::remove_dir_all(&t);
    }
}
