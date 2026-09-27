//! External agent integration (M5) — Claude Code's IDE protocol (as used by the VS Code·JetBrains·Neovim
//! extensions).
//!
//! - tarae opens a WebSocket server on `127.0.0.1:<free port>` and writes
//!   `{pid, workspaceFolders, ideName: "tarae", transport: "ws", authToken}` to `~/.claude/ide/<port>.lock`.
//!   Run `claude` in the same folder and pick `/ide` (or launch it in a side pane with `space c` — direct
//!   via `CLAUDE_CODE_SSE_PORT`) to attach. Auth = token (128-bit random) in the
//!   `x-claude-code-ide-authorization` header. Bound to localhost only.
//! - Speaks MCP (JSON-RPC 2.0) over WebSocket text frames. Tools: `openFile` · **`openDiff`** ·
//!   `getCurrentSelection` · `getLatestSelection` · `getOpenEditors` · `getWorkspaceFolders` ·
//!   `getDiagnostics` · `checkDocumentDirty` · `saveDocument` · `close_tab` · `closeAllDiffTabs`.
//!   Notifications: `selection_changed` when the selection changes, `at_mentioned` via `space C`.
//! - `openDiff` **defers its answer**: shown as an in-buffer review (M4's y/n view); on accept, applied to
//!   the buffer and answered `FILE_SAVED` + final content (Claude writes the file — disk sync sees identical
//!   content as "saved"), on reject `DIFF_REJECTED`. If a review is already up, it's queued — the per-event
//!   hook (`agent_next_diff`) shows the next one once no review is up (whoever's review it was).
//! - Threads: receive (per connection) → events to the main loop · send (per connection) ← channel.
//!   Main only enqueues.

use std::collections::{HashMap, VecDeque};
use std::io::Write as _;
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Sender, channel};

use serde_json::{Value, json};

use crate::document::DocId;
use crate::editor::Editor;
use crate::event::Event;
use crate::ws;

pub const AUTH_HEADER: &str = "x-claude-code-ide-authorization";

/// What goes to the send thread.
enum Out {
    Text(String),
    Pong(Vec<u8>),
    /// Echo of the peer's close (status code) — the last frame.
    Close(Vec<u8>),
}

/// A diff awaiting an answer (`openDiff` request).
struct PendingDiff {
    conn: u64,
    id: Value,
    path: PathBuf,
    contents: String,
    tab: String,
}

/// Selection fingerprint: (document, version, selections).
type SelKey = (DocId, u64, Vec<(usize, usize)>);

pub struct Agent {
    pub port: u16,
    lock: PathBuf,
    /// Connection → (sender, whether `notifications/initialized` arrived — notifications only after that).
    conns: HashMap<u64, (Sender<Out>, bool)>,
    /// Queued diffs (the review view shows one at a time).
    queue: VecDeque<PendingDiff>,
    /// Tab name of the agent diff currently shown in the review view.
    active_tab: Option<String>,
    /// Last announced selection (not resent if unchanged) · its content (`getLatestSelection`).
    last_key: Option<SelKey>,
    last_selection: Option<Value>,
}

impl Drop for Agent {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.lock);
    }
}

impl Agent {
    pub fn connected(&self) -> bool {
        self.conns.values().any(|(_, ready)| *ready)
    }

    fn send(&self, conn: u64, v: &Value) {
        if let Some((tx, _)) = self.conns.get(&conn) {
            let _ = tx.send(Out::Text(v.to_string()));
        }
    }

    fn broadcast(&self, v: &Value) {
        for (tx, _) in self.conns.values().filter(|(_, ready)| *ready) {
            let _ = tx.send(Out::Text(v.to_string()));
        }
    }
}

/// `~/.claude/ide` (under `CLAUDE_CONFIG_DIR` if Claude Code uses it).
fn lock_dir() -> Option<PathBuf> {
    let base = std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".claude")))?;
    Some(base.join("ide"))
}

/// Cleans up lock files left by dead tarae instances — closing the terminal window (SIGHUP) exits without
/// cleanup, so they pile up (measured). Leaves other IDEs' files and live tarae instances' files alone.
fn clean_stale_locks(dir: &std::path::Path) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        let Some(v) = std::fs::read_to_string(&p).ok().and_then(|s| serde_json::from_str::<Value>(&s).ok())
        else {
            continue;
        };
        if v["ideName"] != "tarae" {
            continue;
        }
        let Some(pid) = v["pid"].as_i64() else { continue };
        // SAFETY: signal 0 = sends nothing, only asks whether it's alive.
        let alive = unsafe { libc::kill(pid as libc::pid_t, 0) } == 0
            || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM);
        if !alive {
            let _ = std::fs::remove_file(&p);
        }
    }
}

/// 128-bit random → 32 lowercase hex chars.
fn token() -> String {
    let mut b = [0u8; 16];
    let ok = std::fs::File::open("/dev/urandom").and_then(|mut f| std::io::Read::read_exact(&mut f, &mut b));
    if ok.is_err() {
        // Where there's no /dev/urandom: fall back to time·pid (localhost only, so last resort)
        let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
        b[..8].copy_from_slice(&(t.as_nanos() as u64).to_le_bytes());
        b[8..12].copy_from_slice(&std::process::id().to_le_bytes());
    }
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn text(s: impl Into<String>) -> Value {
    json!({ "content": [{ "type": "text", "text": s.into() }] })
}

fn json_text(v: Value) -> Value {
    text(v.to_string())
}

fn rejected(tab: &str) -> Value {
    json!({ "content": [{ "type": "text", "text": "DIFF_REJECTED" }, { "type": "text", "text": tab }] })
}

/// A selection payload (or `success: false` + message if there's none).
fn selection_result(v: Option<Value>, none: &str) -> Value {
    json_text(v.map_or_else(
        || json!({ "success": false, "message": none }),
        |mut v| {
            v["success"] = json!(true);
            v
        },
    ))
}

/// The lock file holds the auth token — readable by the owner only.
fn write_lock(dir: &std::path::Path, lock: &std::path::Path, body: &str) -> std::io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(lock)?
        .write_all(body.as_bytes())
}

fn tool(name: &str, description: &str, props: Value, required: &[&str]) -> Value {
    json!({ "name": name, "description": description,
            "inputSchema": { "type": "object", "properties": props, "required": required } })
}

fn tools() -> Value {
    let path = json!({ "type": "string", "description": "Absolute path to the file" });
    json!([
        tool(
            "openFile",
            "Open a file in the editor and optionally select a range of text",
            json!({ "filePath": path, "preview": { "type": "boolean" }, "startText": { "type": "string" },
                    "endText": { "type": "string" }, "selectToEndOfLine": { "type": "boolean" },
                    "makeFrontmost": { "type": "boolean" } }),
            &["filePath"]
        ),
        tool(
            "openDiff",
            "Open a diff of proposed changes and wait for the user to accept or reject them",
            json!({ "old_file_path": path, "new_file_path": path, "new_file_contents": { "type": "string" },
                    "tab_name": { "type": "string" } }),
            &["old_file_path", "new_file_path", "new_file_contents", "tab_name"]
        ),
        tool("getCurrentSelection", "Get the current text selection in the active editor", json!({}), &[]),
        tool(
            "getLatestSelection",
            "Get the most recent text selection (even if not in the active editor)",
            json!({}),
            &[]
        ),
        tool("getOpenEditors", "Get information about currently open editors", json!({}), &[]),
        tool("getWorkspaceFolders", "Get all workspace folders currently open in the editor", json!({}), &[]),
        tool(
            "getDiagnostics",
            "Get language diagnostics (errors, warnings) from the editor",
            json!({ "uri": { "type": "string", "description": "Optional file URI; all files if omitted" } }),
            &[]
        ),
        tool(
            "checkDocumentDirty",
            "Check if a document has unsaved changes",
            json!({ "filePath": path }),
            &["filePath"]
        ),
        tool(
            "saveDocument",
            "Save a document with unsaved changes",
            json!({ "filePath": path }),
            &["filePath"]
        ),
        tool("close_tab", "Close a tab by name", json!({ "tab_name": { "type": "string" } }), &["tab_name"]),
        tool("closeAllDiffTabs", "Close all diff tabs in the editor", json!({}), &[]),
    ])
}

/// One connection: handshake → send thread → received messages go to the main loop.
fn serve(stream: TcpStream, token: String, conn: u64, tx: Sender<Event>) {
    let Ok(reader) = ws::handshake(&stream, Some((AUTH_HEADER, &token))) else { return };
    let mut reader = ws::Reader::server(reader);
    // Small JSON-RPC messages — don't hold them back waiting for ACKs (Nagle)
    let _ = stream.set_nodelay(true);
    let (out_tx, out_rx) = channel::<Out>();
    let Ok(mut w) = stream.try_clone() else { return };
    std::thread::spawn(move || {
        for m in out_rx {
            let r = match m {
                Out::Text(s) => ws::write_text(&mut w, &s),
                Out::Pong(p) => ws::write(&mut w, 0xA, &p),
                Out::Close(code) => {
                    let _ = ws::write(&mut w, 0x8, &code);
                    return;
                }
            };
            if r.is_err() {
                return;
            }
        }
    });
    let pong = out_tx.clone();
    let _ = tx.send(Event::Job(Box::new(move |ed: &mut Editor| {
        if let Some(a) = &mut ed.agent {
            a.conns.insert(conn, (out_tx, false));
        }
    })));
    loop {
        match reader.read() {
            Ok(ws::Frame::Text(s)) => {
                let Ok(msg) = serde_json::from_str::<Value>(&s) else { continue };
                if tx
                    .send(Event::Job(Box::new(move |ed: &mut Editor| ed.on_agent_message(conn, msg))))
                    .is_err()
                {
                    return;
                }
            }
            Ok(ws::Frame::Ping(p)) => {
                let _ = pong.send(Out::Pong(p));
            }
            Ok(ws::Frame::Close(code)) => {
                let _ = pong.send(Out::Close(code));
                break;
            }
            Ok(ws::Frame::Other) => {}
            Err(_) => break,
        }
    }
    let _ = tx.send(Event::Job(Box::new(move |ed: &mut Editor| ed.on_agent_closed(conn))));
}

impl Editor {
    /// At startup: opens the server, announced via a lock file (not opened if `agent.claude-code = false`).
    pub fn agent_start(&mut self) {
        if !self.config.agent_claude_code || self.agent.is_some() {
            return;
        }
        let Ok(listener) = TcpListener::bind("127.0.0.1:0") else { return };
        let Ok(port) = listener.local_addr().map(|a| a.port()) else { return };
        let (Some(dir), Ok(cwd)) = (lock_dir(), std::env::current_dir()) else { return };
        clean_stale_locks(&dir);
        let token = token();
        let lock = dir.join(format!("{port}.lock"));
        let body = json!({ "pid": std::process::id(), "workspaceFolders": [cwd], "ideName": "tarae",
                           "transport": "ws", "authToken": token });
        if write_lock(&dir, &lock, &body.to_string()).is_err() {
            return;
        }
        let tx = self.events.sender();
        std::thread::spawn(move || {
            static NEXT: AtomicU64 = AtomicU64::new(1);
            for stream in listener.incoming().flatten() {
                let (token, tx) = (token.clone(), tx.clone());
                let conn = NEXT.fetch_add(1, Ordering::Relaxed);
                std::thread::spawn(move || serve(stream, token, conn, tx));
            }
        });
        self.agent = Some(Agent {
            port,
            lock,
            conns: HashMap::new(),
            queue: VecDeque::new(),
            active_tab: None,
            last_key: None,
            last_selection: None,
        });
    }

    fn on_agent_closed(&mut self, conn: u64) {
        let Some(a) = &mut self.agent else { return };
        // Drop the diffs that connection was waiting on (nowhere to answer)
        a.queue.retain(|d| d.conn != conn);
        if a.conns.remove(&conn).is_some_and(|(_, ready)| ready) && !a.connected() {
            self.set_status("Claude Code disconnected");
        }
    }

    fn agent_reply(&self, conn: u64, id: &Value, result: Value) {
        if let Some(a) = &self.agent {
            a.send(conn, &json!({ "jsonrpc": "2.0", "id": id, "result": result }));
        }
    }

    pub fn on_agent_message(&mut self, conn: u64, msg: Value) {
        let Some(method) = msg["method"].as_str().map(str::to_string) else { return }; // reply to ours — none
        let id = msg["id"].clone();
        let params = &msg["params"];
        let result = match method.as_str() {
            "initialize" => json!({
                "protocolVersion": params["protocolVersion"].as_str().unwrap_or("2025-03-26"),
                "capabilities": { "tools": { "listChanged": true }, "prompts": { "listChanged": true }, "logging": {} },
                "serverInfo": { "name": "tarae", "version": env!("CARGO_PKG_VERSION") },
            }),
            "notifications/initialized" => {
                if let Some((_, ready)) = self.agent.as_mut().and_then(|a| a.conns.get_mut(&conn)) {
                    *ready = true;
                }
                self.set_success(
                    "Claude Code connected — it sees your selection, diagnostics and open files",
                );
                self.agent_selection_tick(true);
                return;
            }
            "tools/list" => json!({ "tools": tools() }),
            "tools/call" => {
                let name = params["name"].as_str().unwrap_or_default().to_string();
                let args = params["arguments"].clone();
                match self.agent_tool(conn, &id, &name, &args) {
                    Some(r) => r,
                    None => return, // deferred answer (openDiff)
                }
            }
            "ping" => json!({}),
            "prompts/list" => json!({ "prompts": [] }),
            "resources/list" => json!({ "resources": [] }),
            "resources/templates/list" => json!({ "resourceTemplates": [] }),
            _ if id.is_null() => return, // unknown notification
            m => {
                let err = json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32601, "message": format!("method not found: {m}") } });
                if let Some(a) = &self.agent {
                    a.send(conn, &err);
                }
                return;
            }
        };
        self.agent_reply(conn, &id, result);
    }

    /// One tool. None if the answer is deferred.
    fn agent_tool(&mut self, conn: u64, id: &Value, name: &str, args: &Value) -> Option<Value> {
        let path_arg = |k: &str| args[k].as_str().map(PathBuf::from);
        Some(match name {
            "openFile" => {
                let Some(path) = path_arg("filePath") else { return Some(text("filePath required")) };
                let front = args["makeFrontmost"].as_bool().unwrap_or(true);
                let before = self.current;
                if let Err(e) = self.open(&path) {
                    return Some(text(format!("Failed to open {}: {e:#}", path.display())));
                }
                if let Some(start) = args["startText"].as_str().filter(|s| !s.is_empty()) {
                    self.agent_select_text(
                        start,
                        args["endText"].as_str(),
                        args["selectToEndOfLine"] == true,
                    );
                }
                if front {
                    return Some(text(format!("Opened file: {}", path.display())));
                }
                let doc = self.doc();
                let info = json!({ "success": true, "filePath": doc.path, "languageId": doc.syntax.as_ref().map(|s| s.lang.name.clone()),
                                   "lineCount": doc.text.len_lines() });
                self.current = before;
                json_text(info)
            }
            "openDiff" => {
                let Some(path) = path_arg("new_file_path").or_else(|| path_arg("old_file_path")) else {
                    return Some(text("new_file_path required"));
                };
                let d = PendingDiff {
                    conn,
                    id: id.clone(),
                    path,
                    contents: args["new_file_contents"].as_str().unwrap_or_default().to_string(),
                    tab: args["tab_name"].as_str().unwrap_or("Proposed changes").to_string(),
                };
                let a = self.agent.as_mut()?;
                a.queue.push_back(d);
                self.agent_next_diff();
                return None;
            }
            "getCurrentSelection" => {
                selection_result(self.agent_selection_payload(), "No active editor found")
            }
            "getLatestSelection" => selection_result(
                self.agent.as_ref().and_then(|a| a.last_selection.clone()),
                "No selection available",
            ),
            "getOpenEditors" => {
                let cur = self.doc().id;
                let tabs: Vec<Value> = self
                    .docs
                    .iter()
                    .filter_map(|d| {
                        let p = d.path.as_ref()?;
                        Some(json!({ "uri": crate::lsp::uri(p), "isActive": d.id == cur,
                                     "label": d.display_name_short(),
                                     "languageId": d.syntax.as_ref().map(|s| s.lang.name.clone()),
                                     "isDirty": d.is_modified() }))
                    })
                    .collect();
                json_text(json!({ "tabs": tabs }))
            }
            "getWorkspaceFolders" => {
                let cwd = std::env::current_dir().unwrap_or_default();
                let name = cwd.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                json_text(
                    json!({ "success": true, "folders": [{ "name": name, "uri": crate::lsp::uri(&cwd), "path": cwd }],
                                  "rootPath": cwd }),
                )
            }
            "getDiagnostics" => {
                let want = args["uri"].as_str().and_then(crate::lsp::path_from_uri);
                let out: Vec<Value> = self
                    .docs
                    .iter()
                    .filter(|d| want.is_none() || d.path == want)
                    .filter_map(|d| {
                        let p = d.path.as_ref()?;
                        let diags: Vec<Value> = d
                            .lsp
                            .diagnostics
                            .iter()
                            .map(|x| {
                                let sev = match x.severity {
                                    1 => "Error",
                                    2 => "Warning",
                                    3 => "Information",
                                    _ => "Hint",
                                };
                                json!({ "message": x.message, "severity": sev, "range": x.raw["range"],
                                        "source": x.raw["source"] })
                            })
                            .collect();
                        Some(json!({ "uri": crate::lsp::uri(p), "diagnostics": diags }))
                    })
                    .collect();
                json_text(Value::Array(out))
            }
            "checkDocumentDirty" => {
                let Some(p) = path_arg("filePath") else { return Some(text("filePath required")) };
                match self.docs.iter().find(|d| d.path.as_deref() == Some(p.as_path())) {
                    Some(d) => json_text(
                        json!({ "success": true, "filePath": p, "isDirty": d.is_modified(), "isUntitled": false }),
                    ),
                    None => json_text(
                        json!({ "success": false, "message": format!("Document not open: {}", p.display()) }),
                    ),
                }
            }
            "saveDocument" => {
                let Some(p) = path_arg("filePath") else { return Some(text("filePath required")) };
                let Some(i) = self.docs.iter().position(|d| d.path.as_deref() == Some(p.as_path())) else {
                    return Some(json_text(
                        json!({ "success": false, "message": format!("Document not open: {}", p.display()) }),
                    ));
                };
                let id = self.docs[i].id;
                match self.docs[i].save() {
                    Ok(()) => {
                        // Same follow-ups as `:w`
                        self.lsp_did_save(id);
                        self.undo_persist(id);
                        json_text(
                            json!({ "success": true, "filePath": p, "saved": true, "message": "Document saved successfully" }),
                        )
                    }
                    Err(e) => {
                        json_text(json!({ "success": false, "filePath": p, "message": format!("{e:#}") }))
                    }
                }
            }
            "close_tab" => {
                let tab = args["tab_name"].as_str().unwrap_or_default();
                self.agent_close_diffs(Some(tab));
                text("TAB_CLOSED")
            }
            "closeAllDiffTabs" => {
                let n = self.agent_close_diffs(None);
                text(format!("CLOSED_{n}_DIFF_TABS"))
            }
            other => text(format!("Unknown tool: {other}")),
        })
    }

    /// Selects startText~endText of `openFile`.
    fn agent_select_text(&mut self, start: &str, end: Option<&str>, to_eol: bool) {
        let doc = self.doc_mut();
        let s = doc.text.to_string();
        let Some(a) = s.find(start) else { return };
        let mut b = match end.filter(|e| !e.is_empty()) {
            Some(e) => s[a..].find(e).map_or(a + start.len(), |i| a + i + e.len()),
            None => a + start.len(),
        };
        if to_eol {
            b = s[b..].find('\n').map_or(s.len(), |i| b + i);
        }
        doc.set_selection(crate::selection::Selection::single(crate::selection::Range::new(a, b)));
        let line = doc.text.byte_to_line(a);
        doc.top = line.saturating_sub(5);
    }

    /// The current selection (Claude Code format — line·column from 0, columns in UTF-16).
    fn agent_selection_payload(&self) -> Option<Value> {
        let doc = self.doc();
        let path = doc.path.as_ref()?;
        let r = doc.selection().primary();
        // A one-character cursor in normal mode = "nothing selected"
        let one = crate::graphemes::next_boundary(&doc.text, r.from());
        let empty = r.to() <= one;
        let (from, to) = if empty { (r.cursor(&doc.text), r.cursor(&doc.text)) } else { (r.from(), r.to()) };
        let enc = crate::lsp::Encoding::Utf16;
        let sel = doc.text.byte_slice(from..to.min(doc.text.len_bytes()));
        let body: String = if sel.len_bytes() > 200_000 { String::new() } else { sel.to_string() };
        Some(json!({
            "text": body,
            "filePath": path,
            "fileUrl": crate::lsp::uri(path),
            "selection": {
                "start": crate::lsp::to_position(&doc.text, from, enc),
                "end": crate::lsp::to_position(&doc.text, to, enc),
                "isEmpty": empty,
            }
        }))
    }

    /// Every event: `selection_changed` if the selection changed (does nothing if nothing is attached).
    pub fn agent_selection_tick(&mut self, force: bool) {
        let Some(a) = &self.agent else { return };
        if !a.connected() || self.doc().path.is_none() {
            return;
        }
        let doc = self.doc();
        let key =
            (doc.id, doc.version(), doc.selection().ranges().iter().map(|r| (r.anchor, r.head)).collect());
        if !force && a.last_key.as_ref() == Some(&key) {
            return;
        }
        let Some(payload) = self.agent_selection_payload() else { return };
        let a = self.agent.as_mut().expect("checked");
        a.broadcast(&json!({ "jsonrpc": "2.0", "method": "selection_changed", "params": payload }));
        a.last_key = Some(key);
        a.last_selection = Some(payload);
    }

    /// `space C` — inserts the selected lines into Claude Code's input as `@file#L10-20`.
    pub fn agent_mention(&mut self) {
        let Some(a) = self.agent.as_ref().filter(|a| a.connected()) else {
            return self.set_warning("Claude Code isn't connected — space c opens it next to tarae");
        };
        let doc = self.doc();
        let Some(path) = doc.path.clone() else { return self.note("save the file first") };
        let r = doc.selection().primary();
        let (l0, l1) =
            (doc.text.byte_to_line(r.from()), doc.text.byte_to_line(r.to().saturating_sub(1).max(r.from())));
        a.broadcast(&json!({ "jsonrpc": "2.0", "method": "at_mentioned",
                             "params": { "filePath": path, "lineStart": l0, "lineEnd": l1 } }));
        let name = doc.display_name_short();
        let lines = if l0 == l1 { format!("{}", l0 + 1) } else { format!("{}-{}", l0 + 1, l1 + 1) };
        self.set_success(format!("Sent @{name}#L{lines} to Claude Code"));
    }

    /// Every event (and on `openDiff`): puts the next queued diff in the review view once none is up —
    /// also after a review that isn't ours (`:ask`, chat `C-r`) ends, so Claude's tool call isn't left
    /// hanging. Only here is the queue popped (not from a review's callback — that would install a review
    /// inside `review_with`, which then overwrites it).
    pub fn agent_next_diff(&mut self) {
        while self.review.is_none() {
            let Some(d) = self.agent.as_mut().and_then(|a| a.queue.pop_front()) else { return };
            self.agent_show_diff(d);
        }
    }

    fn agent_show_diff(&mut self, d: PendingDiff) {
        if let Err(e) = self.open(&d.path) {
            self.set_error(format!("Claude Code: {e:#}"));
            return self.agent_diff_answer(d, false);
        }
        if self.doc().loading {
            self.set_warning("Claude Code: file is still loading — rejected the proposal");
            return self.agent_diff_answer(d, false);
        }
        let old = self.doc().text.to_string();
        let changes = line_changes(&old, &d.contents);
        if changes.is_empty() {
            return self.agent_diff_answer(d, true);
        }
        let doc_id = self.doc().id;
        let n = changes.len();
        // The tab name (`✻ [Claude Code] a.rs (473e33) ⧉`) is for VS Code tabs — here just the file name
        let title =
            format!("proposes {n} change{} to {}", if n == 1 { "" } else { "s" }, self.doc().display_name());
        if let Some(a) = &mut self.agent {
            a.active_tab = Some(d.tab.clone());
        }
        crate::llm::review_with(
            self,
            doc_id,
            &title,
            changes,
            Some(Box::new(move |ed: &mut Editor, applied: bool| {
                if let Some(a) = &mut ed.agent {
                    a.active_tab = None;
                }
                ed.agent_diff_answer(d, applied);
            })),
        );
        if let Some(r) = &mut self.review {
            r.by = "Claude Code";
        }
        // Call whoever is waiting in the side pane (terminal bell)
        if !cfg!(test) {
            let _ = std::io::stdout().write_all(b"\x07");
        }
    }

    /// Answers a diff: if accepted, `FILE_SAVED` + final content (already applied to the buffer —
    /// **Claude Code writes the file**; saving first here makes Claude's edit fail to find the original
    /// (measured)), otherwise `DIFF_REJECTED`.
    fn agent_diff_answer(&mut self, d: PendingDiff, applied: bool) {
        let result = if applied {
            let content = self
                .docs
                .iter()
                .find(|x| x.path.as_deref() == Some(d.path.as_path()))
                .map_or_else(|| d.contents.clone(), |doc| doc.text.to_string());
            json!({ "content": [{ "type": "text", "text": "FILE_SAVED" }, { "type": "text", "text": content }] })
        } else {
            rejected(&d.tab)
        };
        self.agent_reply(d.conn, &d.id, result);
    }

    /// Closes agent diffs (that tab name only / all) — pending ones are answered as rejected. Returns count.
    fn agent_close_diffs(&mut self, tab: Option<&str>) -> usize {
        let Some(a) = &mut self.agent else { return 0 };
        let (keep, gone): (VecDeque<PendingDiff>, VecDeque<PendingDiff>) =
            std::mem::take(&mut a.queue).into_iter().partition(|d| tab.is_some_and(|t| t != d.tab));
        a.queue = keep;
        let active = a.active_tab.clone().filter(|t| tab.is_none_or(|x| x == t));
        let mut n = gone.len();
        for d in gone {
            self.agent_reply(d.conn, &d.id, rejected(&d.tab));
        }
        if active.is_some() {
            n += 1;
            crate::llm::close_review(self); // the callback answers reject (the hook opens the next)
        }
        n
    }

    /// `space c` — launches Claude Code in a side pane and attaches (zellij·tmux), else explains how.
    pub fn agent_open_claude(&mut self) {
        let Some(a) = &self.agent else {
            return self.set_warning("Claude Code integration is off (agent.claude-code)");
        };
        if a.connected() {
            return self.note("Claude Code is already connected");
        }
        let env = format!("CLAUDE_CODE_SSE_PORT={} ENABLE_IDE_INTEGRATION=true", a.port);
        // Claude matches the lock file's workspace against its own folder — launch from the same folder
        let cwd = std::env::current_dir().unwrap_or_default().to_string_lossy().replace('\'', "'\\''");
        let cmd = if std::env::var_os("ZELLIJ").is_some() {
            format!(
                "zellij run --name claude --direction right --close-on-exit --cwd '{cwd}' -- env {env} claude"
            )
        } else if std::env::var_os("TMUX").is_some() {
            format!("tmux split-window -h -c '{cwd}' 'env {env} claude'")
        } else {
            return self.set_status("Run `claude` in this folder and type /ide → tarae (it will connect)");
        };
        if let Err(e) = crate::typed::execute(self, &format!("sh {cmd}")) {
            self.set_error(e);
        } else {
            self.set_status("Opening Claude Code next to tarae…");
        }
    }
}

/// Line-level changes old text → new text: (start byte, end byte, old piece, new piece) — one per
/// review change.
pub fn line_changes(old: &str, new: &str) -> Vec<(usize, usize, String, String)> {
    use imara_diff::intern::InternedInput;
    use imara_diff::{Algorithm, diff};
    let input = InternedInput::new(old, new);
    let starts = |s: &str| {
        let mut v = vec![0];
        v.extend(s.match_indices('\n').map(|(i, _)| i + 1));
        if *v.last().unwrap() != s.len() {
            v.push(s.len());
        }
        v
    };
    let (os, ns) = (starts(old), starts(new));
    let at = |v: &Vec<usize>, i: u32| v.get(i as usize).copied().unwrap_or(*v.last().unwrap());
    let mut out = Vec::new();
    diff(Algorithm::Histogram, &input, |b: std::ops::Range<u32>, a: std::ops::Range<u32>| {
        let (bf, bt) = (at(&os, b.start), at(&os, b.end));
        let (af, at_) = (at(&ns, a.start), at(&ns, a.end));
        out.push((bf, bt, old[bf..bt].to_string(), new[af..at_].to_string()));
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};

    #[test]
    fn line_changes_cover_edits_inserts_and_deletes() {
        let c = line_changes("a\nb\nc\n", "a\nB\nc\nd\n");
        assert_eq!(
            c,
            [(2, 4, "b\n".to_string(), "B\n".to_string()), (6, 6, String::new(), "d\n".to_string())]
        );
        // even without a trailing newline on the last line
        let c = line_changes("x\ny", "x\nz");
        assert_eq!(c, [(2, 3, "y".to_string(), "z".to_string())]);
        assert!(line_changes("same\n", "same\n").is_empty());
    }

    /// An agent with one ready connection (no socket) — what's sent to it arrives on the receiver.
    fn fake_agent(ed: &mut Editor) -> std::sync::mpsc::Receiver<Out> {
        let (tx, rx) = channel();
        ed.agent = Some(Agent {
            port: 0,
            lock: PathBuf::from("/nonexistent/tarae-test.lock"),
            conns: HashMap::from([(1, (tx, true))]),
            queue: VecDeque::new(),
            active_tab: None,
            last_key: None,
            last_selection: None,
        });
        rx
    }

    /// Next reply sent (skips notifications).
    fn sent_reply(rx: &std::sync::mpsc::Receiver<Out>) -> Value {
        loop {
            let Ok(Out::Text(t)) = rx.try_recv() else { panic!("no reply") };
            let v: Value = serde_json::from_str(&t).unwrap();
            if v.get("method").is_none() {
                return v;
            }
        }
    }

    fn call_tool(ed: &mut Editor, id: u64, name: &str, args: Value) {
        let msg = json!({ "jsonrpc": "2.0", "id": id, "method": "tools/call",
                          "params": { "name": name, "arguments": args } });
        ed.handle_event(Event::Job(Box::new(move |ed: &mut Editor| ed.on_agent_message(1, msg))));
    }

    /// Missing required arguments get an error text, not a never-sent "deferred" answer.
    #[test]
    fn tool_calls_without_required_arguments_are_answered() {
        let mut ed = Editor::new(crate::config::Config::default());
        let rx = fake_agent(&mut ed);
        for (id, name) in [(1, "openDiff"), (2, "checkDocumentDirty"), (3, "saveDocument")] {
            call_tool(&mut ed, id, name, json!({}));
            let v = sent_reply(&rx);
            assert_eq!(v["id"], id);
            assert!(v["result"]["content"][0]["text"].as_str().unwrap().contains("required"), "{v}");
        }
    }

    /// A diff queued behind a review that isn't the agent's (chat `C-r`) shows once that one ends —
    /// and a review replacing an agent diff doesn't swallow the next queued one.
    #[test]
    fn queued_diffs_show_after_any_review_ends() {
        let dir = std::env::temp_dir().join(format!("tarae-agent-queue-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.txt");
        std::fs::write(&file, "one\ntwo\n").unwrap();
        let mut ed = Editor::new(crate::config::Config::default());
        ed.open(&file).unwrap();
        let file = ed.doc().path.clone().unwrap();
        let rx = fake_agent(&mut ed);
        let diff = |c: &str, tab: &str| json!({ "old_file_path": file, "new_file_path": file, "new_file_contents": c, "tab_name": tab });
        // A1 shown, A2 queued
        call_tool(&mut ed, 1, "openDiff", diff("ONE\ntwo\n", "a1"));
        call_tool(&mut ed, 2, "openDiff", diff("one\nTWO\n", "a2"));
        assert_eq!(ed.agent.as_ref().unwrap().queue.len(), 1);
        // Chat's review takes A1's place: A1 is rejected, A2 stays queued (not overwritten)
        let id = ed.doc().id;
        crate::llm::review_with(&mut ed, id, "chat", vec![(0, 4, "one\n".into(), "uno\n".into())], None);
        let v = sent_reply(&rx);
        assert_eq!(
            (v["id"].clone(), v["result"]["content"][0]["text"].clone()),
            (json!(1), json!("DIFF_REJECTED"))
        );
        assert_eq!(ed.review.as_ref().unwrap().by, "Claude");
        assert_eq!(ed.agent.as_ref().unwrap().queue.len(), 1);
        // The chat review ends → A2 shows
        ed.handle_event(Event::Key("q".parse().unwrap()));
        assert_eq!(ed.review.as_ref().map(|r| r.by), Some("Claude Code"));
        ed.handle_event(Event::Key("y".parse().unwrap()));
        let v = sent_reply(&rx);
        assert_eq!(v["id"], 2);
        assert_eq!(v["result"]["content"][0]["text"], "FILE_SAVED");
        assert_eq!(ed.doc().text.to_string(), "one\nTWO\n");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Masked text frame (client → server).
    fn client_frame(s: &str) -> Vec<u8> {
        let mask = [7u8, 1, 9, 3];
        let p = s.as_bytes();
        let mut f = vec![0x81];
        if p.len() < 126 {
            f.push(0x80 | p.len() as u8);
        } else {
            f.push(0x80 | 126);
            f.extend((p.len() as u16).to_be_bytes());
        }
        f.extend(mask);
        f.extend(p.iter().enumerate().map(|(i, b)| b ^ mask[i % 4]));
        f
    }

    /// One reply (skips notifications that arrive in between).
    fn reply(r: &mut ws::Reader<BufReader<TcpStream>>) -> Value {
        loop {
            let ws::Frame::Text(t) = r.read().unwrap() else { panic!("text frame") };
            let v: Value = serde_json::from_str(&t).unwrap();
            if v.get("method").is_none() {
                return v;
            }
        }
    }

    /// Over a real socket: lock file → auth → initialize → tools → openDiff (deferred) → y → FILE_SAVED.
    #[test]
    fn claude_code_protocol_end_to_end() {
        let dir = std::env::temp_dir().join(format!("tarae-agent-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.txt");
        std::fs::write(&file, "one\ntwo\n").unwrap();
        // SAFETY: a variable only this test uses (lock files to a temp folder)
        unsafe { std::env::set_var("CLAUDE_CONFIG_DIR", dir.join("claude")) };
        let mut ed = Editor::new(crate::config::Config::default());
        ed.open(&file).unwrap();
        let file = ed.doc().path.clone().unwrap();
        // Dead tarae's lock file is removed, other IDEs' are kept
        std::fs::create_dir_all(dir.join("claude/ide")).unwrap();
        std::fs::write(dir.join("claude/ide/1.lock"), r#"{"pid":999999,"ideName":"tarae"}"#).unwrap();
        std::fs::write(dir.join("claude/ide/2.lock"), r#"{"pid":999999,"ideName":"VS Code"}"#).unwrap();
        ed.agent_start();
        assert!(!dir.join("claude/ide/1.lock").exists() && dir.join("claude/ide/2.lock").exists());
        let port = ed.agent.as_ref().unwrap().port;
        let lock: Value = serde_json::from_str(
            &std::fs::read_to_string(dir.join(format!("claude/ide/{port}.lock"))).unwrap(),
        )
        .unwrap();
        assert_eq!(lock["ideName"], "tarae");
        let mode = |p: PathBuf| {
            std::os::unix::fs::PermissionsExt::mode(&std::fs::metadata(p).unwrap().permissions())
        };
        assert_eq!(mode(dir.join(format!("claude/ide/{port}.lock"))) & 0o777, 0o600, "holds the token");
        let token = lock["authToken"].as_str().unwrap().to_string();
        assert_eq!(token.len(), 32);
        // Wrong token is rejected
        let mut bad = TcpStream::connect(("127.0.0.1", port)).unwrap();
        write!(bad, "GET / HTTP/1.1\r\nUpgrade: websocket\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n{AUTH_HEADER}: nope\r\n\r\n").unwrap();
        let mut line = String::new();
        BufReader::new(&bad).read_line(&mut line).unwrap();
        assert!(line.contains("401"), "{line}");
        // Correct token
        let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        write!(s, "GET / HTTP/1.1\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Protocol: mcp\r\n{AUTH_HEADER}: {token}\r\n\r\n").unwrap();
        let mut r = BufReader::new(s.try_clone().unwrap());
        let mut head = String::new();
        loop {
            let mut l = String::new();
            r.read_line(&mut l).unwrap();
            head.push_str(&l);
            if l == "\r\n" {
                break;
            }
        }
        assert!(head.contains("101") && head.contains("s3pPLMBiTxaQ9kYGzzhZRbK+xOo="), "{head}");
        assert!(head.contains("Sec-WebSocket-Protocol: mcp"), "echoes back the requested subprotocol");
        let mut r = ws::Reader::client(r);
        let pump = |ed: &mut Editor| {
            let ev = ed.events.recv_timeout(std::time::Duration::from_secs(5)).expect("event");
            ed.handle_event(ev);
        };
        pump(&mut ed); // register the connection
        let call = |ed: &mut Editor,
                    s: &mut TcpStream,
                    r: &mut ws::Reader<BufReader<TcpStream>>,
                    msg: Value|
         -> Option<Value> {
            s.write_all(&client_frame(&msg.to_string())).unwrap();
            pump(ed);
            if msg.get("id").is_none() || msg["params"]["name"] == "openDiff" {
                return None;
            }
            Some(reply(r))
        };
        let init = call(&mut ed, &mut s, &mut r, json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": { "protocolVersion": "2025-06-18" } })).unwrap();
        assert_eq!(init["result"]["serverInfo"]["name"], "tarae", "{init}");
        assert_eq!(init["result"]["protocolVersion"], "2025-06-18");
        call(&mut ed, &mut s, &mut r, json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));
        // Announces the current selection right after attaching
        let ws::Frame::Text(t) = r.read().unwrap() else { panic!() };
        let note: Value = serde_json::from_str(&t).unwrap();
        assert_eq!(note["method"], "selection_changed");
        assert_eq!(note["params"]["filePath"], json!(file));
        let list =
            call(&mut ed, &mut s, &mut r, json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }))
                .unwrap();
        assert!(list["result"]["tools"].as_array().unwrap().iter().any(|t| t["name"] == "openDiff"));
        let editors = call(
            &mut ed,
            &mut s,
            &mut r,
            json!({ "jsonrpc": "2.0", "id": 3, "method": "tools/call",
            "params": { "name": "getOpenEditors", "arguments": {} } }),
        )
        .unwrap();
        assert!(editors["result"]["content"][0]["text"].as_str().unwrap().contains("a.txt"));
        // openDiff: defers the answer and opens the review view
        call(
            &mut ed,
            &mut s,
            &mut r,
            json!({ "jsonrpc": "2.0", "id": 4, "method": "tools/call",
            "params": { "name": "openDiff", "arguments": { "old_file_path": file, "new_file_path": file,
                "new_file_contents": "one\nTWO\n", "tab_name": "✻ [Claude Code] a.txt" } } }),
        );
        assert!(ed.review.as_ref().is_some_and(|r| r.ready));
        ed.handle_key("y".parse().unwrap());
        assert!(ed.review.is_none());
        let saved = reply(&mut r);
        assert_eq!(saved["id"], 4);
        assert_eq!(saved["result"]["content"][0]["text"], "FILE_SAVED");
        assert_eq!(saved["result"]["content"][1]["text"], "one\nTWO\n");
        assert_eq!(ed.doc().text.to_string(), "one\nTWO\n", "applied to the buffer");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "one\ntwo\n", "Claude Code writes the file");
        // Reject
        call(
            &mut ed,
            &mut s,
            &mut r,
            json!({ "jsonrpc": "2.0", "id": 5, "method": "tools/call",
            "params": { "name": "openDiff", "arguments": { "old_file_path": file, "new_file_path": file,
                "new_file_contents": "zero\n", "tab_name": "t2" } } }),
        );
        ed.handle_key("n".parse().unwrap());
        let rejected = reply(&mut r);
        assert_eq!(rejected["result"]["content"][0]["text"], "DIFF_REJECTED");
        assert_eq!(ed.doc().text.to_string(), "one\nTWO\n");
        // A close is echoed back
        let mut close = vec![0x88, 0x80 | 2];
        close.extend([7u8, 1, 9, 3]);
        close.extend([0x03 ^ 7, 0xE8 ^ 1]);
        s.write_all(&close).unwrap();
        assert_eq!(r.read().unwrap(), ws::Frame::Close(vec![0x03, 0xE8]));
        while ed.agent.as_ref().unwrap().connected() {
            pump(&mut ed); // until the connection-closed event
        }
        // On disconnect the lock file stays (next connection); closing the editor removes it
        drop(s);
        drop(r);
        let mut rest = Vec::new();
        let _ = bad.read_to_end(&mut rest);
        ed.agent = None;
        assert!(!dir.join(format!("claude/ide/{port}.lock")).exists());
        std::fs::remove_dir_all(&dir).ok();
    }
}
