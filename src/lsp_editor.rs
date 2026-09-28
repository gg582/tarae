//! Editor ↔ LSP — attaching servers, sending changes (at the end of each event batch), handling server
//! messages, requests (`gd` `gr` hover), diagnostic jumps. Late results (cursor moved/edited) are dropped.

use std::collections::HashMap;

use serde_json::{Value, json};

use crate::completion::{self, Completion};
use crate::document::DocId;
use crate::editor::Editor;
use crate::lsp::{self, Client, ClientId, Diagnostic, Encoding};
use crate::movement as mv;
use crate::picker::{Action, Item, Picker};
use crate::selection::Selection;

#[derive(Default)]
pub struct LspState {
    pub clients: Vec<Client>,
    next_id: ClientId,
    pub(crate) pending: HashMap<(ClientId, u64), Pending>,
    /// Last code action list (picker picks are taken from here) · new id per list · per-item previews.
    actions: (ClientId, Vec<Value>),
    actions_gen: u64,
    action_previews: HashMap<usize, ActionPreview>,
    /// A completion request is in flight — one at a time.
    completion_inflight: bool,
    /// Signature request in flight / more edits since (ask once more when the response arrives).
    signature_inflight: bool,
    signature_again: bool,
    /// Inlay hints: in-flight request (doc, version, line range) / waiting briefly (ask once input pauses).
    inlay_inflight: Option<(DocId, u64, (usize, usize))>,
    inlay_waiting: bool,
    /// Id of the last completion list (a late resolve for an older list is dropped).
    completion_gen: u64,
    /// Go-to target in a file still loading in the background — applied once it has loaded.
    jump_after_load: Option<(DocId, Value, Encoding)>,
}

/// What applying one code action would change (picker's right pane).
#[derive(Clone)]
pub enum ActionPreview {
    /// Asking the server for the edit (codeAction/resolve) / building the diff.
    Working,
    Ready(std::sync::Arc<Vec<crate::editdiff::FileDiff>>),
    /// Server command only, no edit — can't preview.
    CommandOnly,
    Failed(String),
}

/// Where `g d`·`g D`·`g y`·`g i`·`g r` go — one request shape, one reply handler.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Goto {
    Definition,
    Declaration,
    TypeDefinition,
    Implementation,
    References,
}

impl Goto {
    fn method(self) -> &'static str {
        match self {
            Goto::Definition => "textDocument/definition",
            Goto::Declaration => "textDocument/declaration",
            Goto::TypeDefinition => "textDocument/typeDefinition",
            Goto::Implementation => "textDocument/implementation",
            Goto::References => "textDocument/references",
        }
    }

    /// Server capability to check first — only the ones servers often lack (every server does
    /// definitions and references; some leave the capability out anyway).
    fn provider(self) -> Option<&'static str> {
        match self {
            Goto::Declaration => Some("declarationProvider"),
            Goto::TypeDefinition => Some("typeDefinitionProvider"),
            Goto::Implementation => Some("implementationProvider"),
            Goto::Definition | Goto::References => None,
        }
    }

    /// Picker title / what "nothing found" names.
    fn noun(self) -> &'static str {
        match self {
            Goto::Definition => "definitions",
            Goto::Declaration => "declarations",
            Goto::TypeDefinition => "type definitions",
            Goto::Implementation => "implementations",
            Goto::References => "references",
        }
    }
}

/// Inlay hints: ask once input has paused this long.
const INLAY_DEBOUNCE_MS: u64 = 120;

/// What a request was for — plus what's needed to use its reply.
#[derive(Clone, Debug, PartialEq)]
pub enum Kind {
    Goto(Goto),
    Hover,
    CodeAction,
    ResolveAction,
    /// resolve for preview of action `index` in code action list `list` — fetch the edit only, don't execute.
    ResolvePreview {
        list: u64,
        index: usize,
    },
    ExecuteCommand,
    Rename,
    Format,
    /// `:w` with format-on-save: format, then save (and quit for `:wq`).
    FormatSave {
        quit: bool,
    },
    Completion,
    /// Docs/extra edits for item `index` of completion list `list`.
    ResolveCompletion {
        list: u64,
        index: usize,
    },
    SignatureHelp,
    /// Hints for this line range.
    InlayHint {
        range: (usize, usize),
    },
    DocumentSymbols,
    WorkspaceSymbols,
    /// Java debug prep (jdtls `vscode.java.*` commands) — java.rs.
    JavaDebug,
    /// Library/JDK class source (`java/classFileContents`, jdt:// definitions) — java.rs.
    ClassFile {
        uri: String,
        pos: Value,
    },
}

/// A sent request — with what's needed to judge whether it's still valid when the response arrives.
#[derive(Clone, Debug)]
pub struct Pending {
    pub kind: Kind,
    doc: DocId,
    version: u64,
    head: usize,
}

impl Editor {
    pub(crate) fn client_mut(&mut self, cid: ClientId) -> Option<&mut Client> {
        self.lsp.clients.iter_mut().find(|c| c.id == cid)
    }

    pub(crate) fn client(&self, cid: ClientId) -> Option<&Client> {
        self.lsp.clients.iter().find(|c| c.id == cid)
    }

    /// Position encoding of server `cid` (UTF-16 until it says otherwise).
    pub(crate) fn encoding_of(&self, cid: ClientId) -> Encoding {
        self.client(cid).map_or(Encoding::Utf16, |c| c.encoding)
    }

    /// Attach a language server to the doc (spawning it if needed). Startup/init are all async — no waiting.
    /// Also called once a background-loaded doc has its text (`finish_loading`).
    pub fn attach_lsp(&mut self, id: DocId) {
        self.jump_if_loaded(id);
        if !self.config.lsp.enabled {
            return;
        }
        let Some(doc) = self.docs.iter().find(|d| d.id == id) else { return };
        if doc.loading || doc.lsp.client.is_some() {
            return;
        }
        let Some(path) = doc.path.clone() else { return };
        let Some(spec) = crate::syntax::detect(&path) else { return };
        let Some(server) = lsp::find_server(&spec.name, &self.config.lsp) else { return };
        let root = lsp::find_root_for(&spec.name, &path);
        let cid = match self.lsp.clients.iter().find(|c| c.name == server.name && c.root == root) {
            Some(c) => c.id,
            None => {
                self.lsp.next_id += 1;
                let cid = self.lsp.next_id;
                let (server, options) = crate::java::prepare(&server, &root);
                match Client::start(cid, &server, &root, options, self.events.sender()) {
                    Ok(c) => self.lsp.clients.push(c),
                    Err(e) => {
                        self.set_error(format!("lsp: {e}"));
                        return;
                    }
                }
                cid
            }
        };
        // Already-running UTF-16 server: count UTF-16 columns from the first edit (else it's a full sync)
        let u16 = self.client(cid).is_some_and(|c| c.ready && c.encoding == Encoding::Utf16);
        let doc = self.docs.iter_mut().find(|d| d.id == id).expect("checked above");
        doc.lsp.client = Some(cid);
        doc.lsp.version = 0;
        doc.lsp.changes.clear();
        doc.lsp.full_sync = false;
        doc.lsp.u16 = u16;
        let (text, uri, lang) = (doc.text.clone(), lsp::uri(&path), lsp::language_id(&spec.name).to_string());
        let client = self.client_mut(cid).expect("just ensured");
        client.notify_lazy(move || {
            let params = json!({ "textDocument": { "uri": uri, "languageId": lang, "version": 0, "text": text.to_string() } });
            json!({ "jsonrpc": "2.0", "method": "textDocument/didOpen", "params": params }).to_string()
        });
    }

    /// Flush queued changes as didChange (at the end of each event batch — key bursts become one message).
    pub fn lsp_flush(&mut self) {
        let clients = &mut self.lsp.clients;
        for doc in &mut self.docs {
            let Some(cid) = doc.lsp.client else { continue };
            if doc.lsp.changes.is_empty() && !doc.lsp.full_sync {
                continue;
            }
            let Some(client) = clients.iter_mut().find(|c| c.id == cid) else { continue };
            if !client.ready {
                continue; // before init — queued, sent once ready
            }
            let utf16 = client.encoding == Encoding::Utf16;
            doc.lsp.u16 = utf16;
            doc.lsp.version += 1;
            let changes = std::mem::take(&mut doc.lsp.changes);
            // Changes without known UTF-16 columns (queued before the server encoding was known) → full sync.
            let full =
                std::mem::take(&mut doc.lsp.full_sync) || (utf16 && changes.iter().any(|c| c.u16.is_none()));
            let uri = lsp::uri(doc.path.as_deref().unwrap_or(std::path::Path::new("")));
            let version = doc.lsp.version;
            if full {
                let text = doc.text.clone();
                client.notify_lazy(move || {
                    let params = json!({ "textDocument": { "uri": uri, "version": version }, "contentChanges": [{ "text": text.to_string() }] });
                    json!({ "jsonrpc": "2.0", "method": "textDocument/didChange", "params": params }).to_string()
                });
            } else {
                let content: Vec<Value> = changes
                    .iter()
                    .map(|c| {
                        let (sc, ec) = match c.u16 {
                            Some((s, e)) if utf16 => (s, e),
                            _ => (c.ts.start_position.column, c.ts.old_end_position.column),
                        };
                        json!({
                            "range": {
                                "start": { "line": c.ts.start_position.row, "character": sc },
                                "end": { "line": c.ts.old_end_position.row, "character": ec },
                            },
                            "text": c.insert,
                        })
                    })
                    .collect();
                let params =
                    json!({ "textDocument": { "uri": uri, "version": version }, "contentChanges": content });
                client.notify("textDocument/didChange", params);
            }
        }
    }

    pub fn lsp_did_save(&mut self, id: DocId) {
        self.lsp_flush();
        let Some(doc) = self.docs.iter().find(|d| d.id == id) else { return };
        let (Some(cid), Some(path)) = (doc.lsp.client, doc.path.clone()) else { return };
        if let Some(c) = self.client_mut(cid) {
            c.notify("textDocument/didSave", json!({ "textDocument": { "uri": lsp::uri(&path) } }));
        }
    }

    /// One message from a server (reader thread → event → here, on the main loop).
    pub fn on_lsp_message(&mut self, cid: ClientId, msg: Value) {
        let method = msg["method"].as_str().map(str::to_string);
        match (method, msg.get("id")) {
            // Response to our request
            (None, Some(id)) => {
                let id = id.as_u64().unwrap_or(u64::MAX);
                if id == 0 {
                    let Some(c) = self.client_mut(cid) else { return };
                    c.on_initialized(&msg["result"]);
                    // Now the encoding is known — edits from here on carry UTF-16 columns if it needs them
                    let u16 = c.encoding == Encoding::Utf16;
                    for d in self.docs.iter_mut().filter(|d| d.lsp.client == Some(cid)) {
                        d.lsp.u16 = u16;
                    }
                    self.lsp_flush();
                    return;
                }
                let Some(p) = self.lsp.pending.remove(&(cid, id)) else { return };
                let (error, result) = (msg.get("error"), &msg["result"]);
                match &p.kind {
                    Kind::JavaDebug => self.java_debug_reply(cid, id, error, result),
                    Kind::ClassFile { uri, pos } => {
                        self.class_file_reply(cid, uri.clone(), pos, error, result)
                    }
                    Kind::InlayHint { range } => self.on_inlay(cid, &p, *range, result),
                    // The server couldn't format — save as is
                    &Kind::FormatSave { quit } if error.is_some() => {
                        self.format_save_done(p.doc, quit, Some("the server couldn't format — saved as is"))
                    }
                    Kind::SignatureHelp => self.on_signature(&p, result, error.is_some()),
                    Kind::Completion => {
                        self.lsp.completion_inflight = false;
                        if error.is_none() {
                            self.on_completion(cid, &p, result);
                        }
                    }
                    // Preview failure stays in that pane (picking it asks again on execution)
                    &Kind::ResolvePreview { list, index } if error.is_some() => {
                        if list == self.lsp.actions_gen {
                            let m = error.and_then(|e| e["message"].as_str()).unwrap_or("request failed");
                            self.lsp.action_previews.insert(index, ActionPreview::Failed(m.to_string()));
                        }
                    }
                    _ => match error {
                        Some(e) => self
                            .set_error(format!("lsp: {}", e["message"].as_str().unwrap_or("request failed"))),
                        None => self.on_response(cid, p, result),
                    },
                }
            }
            // A request from the server to us — some servers stall if unanswered.
            (Some(m), Some(id)) => {
                let result = match m.as_str() {
                    // Server says "hints changed" — drop all and ask again
                    "workspace/inlayHint/refresh" => {
                        for d in &mut self.docs {
                            d.lsp.inlay_have = None;
                        }
                        Value::Null
                    }
                    "workspace/configuration" => {
                        let n = msg["params"]["items"].as_array().map_or(0, Vec::len);
                        Value::Array(vec![Value::Null; n])
                    }
                    "workspace/workspaceFolders" => {
                        let root = self.lsp.clients.iter().find(|c| c.id == cid).map(|c| c.root.clone());
                        json!([{ "uri": root.as_deref().map(lsp::uri), "name": "root" }])
                    }
                    // Many code action commands send their edits this way.
                    "workspace/applyEdit" => match self.apply_workspace_edit(cid, &msg["params"]["edit"]) {
                        Ok(_) => json!({ "applied": true }),
                        Err(e) => {
                            self.set_error(format!("lsp edit: {e}"));
                            json!({ "applied": false, "failureReason": e })
                        }
                    },
                    _ => Value::Null,
                };
                let id = id.clone();
                if let Some(c) = self.client_mut(cid) {
                    c.reply(&id, result);
                }
            }
            (Some(m), None) => self.on_notification(cid, &m, &msg["params"]),
            (None, None) => {}
        }
    }

    fn on_notification(&mut self, cid: ClientId, method: &str, params: &Value) {
        match method {
            "textDocument/publishDiagnostics" => {
                let Some(path) = params["uri"].as_str().and_then(lsp::path_from_uri) else { return };
                let enc = self.encoding_of(cid);
                // Doc paths are canonical — only a URI matching no open doc as-is is resolved on disk
                let open = |p: &std::path::Path| self.docs.iter().position(|d| d.path.as_deref() == Some(p));
                let Some(i) = open(&path).or_else(|| open(&std::fs::canonicalize(&path).ok()?)) else {
                    return;
                };
                let doc = &mut self.docs[i];
                let mut list: Vec<Diagnostic> = params["diagnostics"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|d| {
                        let from = lsp::from_position(&doc.text, &d["range"]["start"], enc)?;
                        let to = lsp::from_position(&doc.text, &d["range"]["end"], enc)?;
                        Some(Diagnostic {
                            from,
                            to: to.max(from),
                            severity: d["severity"].as_u64().unwrap_or(1).clamp(1, 4) as u8,
                            message: d["message"].as_str().unwrap_or_default().trim().to_string(),
                            raw: d.clone(),
                        })
                    })
                    .collect();
                list.sort_by_key(|d| (d.from, d.severity));
                doc.lsp.diagnostics = list;
            }
            "$/progress" => {
                let token = params["token"].to_string();
                let v = &params["value"];
                let Some(c) = self.client_mut(cid) else { return };
                match v["kind"].as_str() {
                    Some("end") => {
                        c.progress.remove(&token);
                    }
                    Some("begin") => {
                        let mut p = lsp::Progress::default();
                        p.update(v);
                        c.progress.insert(token, p);
                    }
                    Some(_) => c.progress.entry(token).or_default().update(v),
                    None => {}
                }
            }
            // jdtls startup progress (project import, indexing)
            "language/status" => {
                let Some(c) = self.client_mut(cid) else { return };
                let key = "language/status".to_string();
                match crate::java::status_text(params) {
                    Some(title) => c.progress.insert(key, lsp::Progress { title, ..Default::default() }),
                    None => c.progress.remove(&key),
                };
            }
            "window/showMessage" => {
                let m = params["message"].as_str().unwrap_or_default().to_string();
                if params["type"].as_u64() == Some(1) {
                    self.set_error(m);
                } else {
                    self.set_status(m);
                }
            }
            _ => {}
        }
    }

    /// Send a request at the cursor. If there's no server or it's still starting, say so and return false.
    pub fn lsp_request(&mut self, kind: Kind, method: &str, extra: Value) -> bool {
        let Some(cid) = self.ready_client() else { return false };
        let doc = self.doc();
        let Some(path) = &doc.path else { return false };
        let head = doc.selection().primary().cursor(&doc.text);
        let position = lsp::to_position(&doc.text, head, self.encoding_of(cid));
        let mut params = json!({ "textDocument": { "uri": lsp::uri(path) }, "position": position });
        if let (Value::Object(p), Value::Object(e)) = (&mut params, extra) {
            p.extend(e);
        }
        self.lsp_send(cid, kind, method, params).is_some()
    }

    /// The current doc's server, if it's ready — else says why. Flushes pending edits first (else the server
    /// answers from the old doc — completion asks on new chars).
    fn ready_client(&mut self) -> Option<ClientId> {
        self.lsp_flush();
        let Some(cid) = self.doc().lsp.client else {
            self.set_status("no language server for this file");
            return None;
        };
        let c = self.client(cid)?;
        if !c.ready {
            let name = c.name.clone();
            self.set_status(format!("{name} is starting…"));
            return None;
        }
        Some(cid)
    }

    fn on_response(&mut self, cid: ClientId, p: Pending, result: &Value) {
        // Late response: drop if the doc changed (edit kinds) or the cursor moved (position kinds).
        let Some(doc) = self.docs.iter().find(|d| d.id == p.doc) else { return };
        let cursor_bound = matches!(p.kind, Kind::Goto(_) | Kind::Hover | Kind::CodeAction);
        let version_bound = !matches!(
            p.kind,
            Kind::ResolvePreview { .. }
                | Kind::ExecuteCommand
                | Kind::ResolveCompletion { .. }
                | Kind::WorkspaceSymbols
        );
        let moved = self.doc().id != p.doc || doc.selection().primary().cursor(&doc.text) != p.head;
        if (version_bound && doc.version() != p.version) || (cursor_bound && moved) {
            // Typed on after :w — save what's there now, unformatted (a save is never lost)
            if let Kind::FormatSave { quit } = p.kind {
                return self.format_save_done(p.doc, quit, Some("changed while formatting — saved as is"));
            }
            if p.kind == Kind::ResolveAction {
                self.set_status("code action dropped — the file changed meanwhile");
            }
            return;
        }
        let enc = self.encoding_of(cid);
        match p.kind {
            Kind::CodeAction => {
                let mut list: Vec<Value> = result.as_array().cloned().unwrap_or_default();
                if list.is_empty() {
                    return self.set_status("no code actions");
                }
                // Server-preferred → quick fixes → the rest (server order within each tier)
                list.sort_by_key(|a| {
                    let quickfix = a["kind"].as_str().is_some_and(|k| k.starts_with("quickfix"));
                    (!a["isPreferred"].as_bool().unwrap_or(false), !quickfix)
                });
                let items = list
                    .iter()
                    .enumerate()
                    .map(|(i, a)| {
                        // Kind as a short dim note on the right (`refactor.rewrite` → `rewrite`)
                        let kind = a["kind"].as_str().unwrap_or_default();
                        let hint = kind.rsplit('.').next().unwrap_or_default().to_string();
                        let preferred = a["isPreferred"].as_bool().unwrap_or(false);
                        Item {
                            label: a["title"].as_str().unwrap_or("?").to_string(),
                            action: Action::Code(i),
                            hint: if preferred { format!("● {hint}") } else { hint },
                            glyph: None,
                        }
                    })
                    .collect();
                self.lsp.actions = (cid, list);
                self.lsp.actions_gen += 1;
                self.lsp.action_previews.clear();
                self.open_picker(Picker::new("code actions", items, false), None);
                self.code_action_preview();
            }
            Kind::ResolveAction => self.run_code_action_value(cid, result.clone(), false),
            Kind::ResolvePreview { list, index } => {
                // Keep the received edit in the list — picking it applies directly without asking again
                if list == self.lsp.actions_gen
                    && let Some(slot) = self.lsp.actions.1.get_mut(index)
                {
                    if result.is_object() {
                        *slot = result.clone();
                    }
                    self.lsp.action_previews.remove(&index);
                    self.code_action_preview_of(index, false);
                }
            }
            Kind::ResolveCompletion { list, index } => {
                if let Some(c) = self.completion.as_mut().filter(|c| c.list == list) {
                    c.merge_resolved(index, result);
                    self.completion_docs();
                }
            }
            // Nothing to do with the reply / handled in on_lsp_message
            Kind::ExecuteCommand
            | Kind::Completion
            | Kind::SignatureHelp
            | Kind::InlayHint { .. }
            | Kind::JavaDebug
            | Kind::ClassFile { .. } => {}
            Kind::Rename => match self.apply_workspace_edit(cid, result) {
                Ok(n) => self.set_status(format!("renamed in {n} file(s)")),
                Err(e) => self.set_error(format!("rename: {e}")),
            },
            Kind::FormatSave { quit } => {
                let edits = result.as_array().cloned().unwrap_or_default();
                let why =
                    self.apply_text_edits(p.doc, enc, &edits).err().map(|e| format!("not formatted: {e}"));
                self.format_save_done(p.doc, quit, why.as_deref());
            }
            Kind::Format => {
                let edits = result.as_array().cloned().unwrap_or_default();
                if edits.is_empty() {
                    return self.set_status("already formatted");
                }
                match self.apply_text_edits(p.doc, enc, &edits) {
                    Ok(()) => self.set_status("formatted"),
                    Err(e) => self.set_error(format!("format: {e}")),
                }
            }
            Kind::Hover => {
                let text = lsp::hover_text(result);
                if text.is_empty() {
                    self.set_status("no hover information");
                } else {
                    let lang = self.doc().syntax.as_ref().map(|s| s.lang.name.clone());
                    self.popup = Some(crate::markdown::render(&text, &self.theme, lang.as_deref()));
                }
            }
            Kind::DocumentSymbols => self.on_document_symbols(p.doc, result),
            Kind::WorkspaceSymbols => self.on_workspace_symbols(result),
            Kind::Goto(goto) => {
                // jdtls: library/JDK classes come as jdt:// URIs — source into a read-only buffer (java.rs)
                if goto != Goto::References
                    && let Some((uri, pos)) = crate::java::jdt_location(result)
                {
                    self.push_jump();
                    return self.open_class_file(cid, uri, pos);
                }
                let locs = lsp::locations(result);
                match locs.as_slice() {
                    [] => self.set_status(format!("no {} found", goto.noun())),
                    // One place: go straight there (references always list — even one is worth seeing)
                    [(path, pos)] if goto != Goto::References => self.lsp_jump(path, pos, enc),
                    _ => {
                        let root = std::env::current_dir().unwrap_or_default();
                        let items = locs
                            .iter()
                            .map(|(path, pos)| {
                                let line = pos["line"].as_u64().unwrap_or(0) as usize;
                                let col = pos["character"].as_u64().unwrap_or(0) as usize;
                                let rel = path.strip_prefix(&root).unwrap_or(path);
                                Item {
                                    label: format!("{}:{}", rel.display(), line + 1),
                                    // Approximate column (UTF-16 may drift after non-ASCII; line is exact)
                                    action: Action::Goto { path: path.clone(), line, col },
                                    hint: String::new(),
                                    glyph: None,
                                }
                            })
                            .collect();
                        self.open_picker(Picker::new(goto.noun(), items, true), None);
                    }
                }
            }
        }
    }

    /// Any request to this server, remembered against the current doc (version, cursor) — returns the
    /// request id. None if the server is gone or not ready.
    pub(crate) fn lsp_send(&mut self, cid: ClientId, kind: Kind, method: &str, params: Value) -> Option<u64> {
        let doc = self.doc();
        let head = doc.selection().primary().cursor(&doc.text);
        let p = Pending { kind, doc: doc.id, version: doc.version(), head };
        let id = self.client_mut(cid).filter(|c| c.ready)?.request(method, params);
        self.lsp.pending.insert((cid, id), p);
        Some(id)
    }

    // ── Symbol search ─────────────────────────────────────────────────────

    /// Request to the current doc's server without a position (symbols etc.).
    fn lsp_request_raw(&mut self, kind: Kind, method: &str, params: Value) -> bool {
        let Some(cid) = self.ready_client() else { return false };
        self.lsp_send(cid, kind, method, params).is_some()
    }

    /// `space s` — symbols in this file (functions·types·fields …).
    pub fn document_symbols(&mut self) {
        let Some(path) = self.doc().path.clone() else { return self.set_status("no file") };
        self.lsp_request_raw(
            Kind::DocumentSymbols,
            "textDocument/documentSymbol",
            json!({ "textDocument": { "uri": lsp::uri(&path) } }),
        );
    }

    /// Symbols of doc `doc_id` — dropped if another buffer is showing by now.
    fn on_document_symbols(&mut self, doc_id: DocId, result: &Value) {
        if self.doc().id != doc_id {
            return;
        }
        let Some(path) = self.doc().path.clone() else { return };
        let mut items = Vec::new();
        let mut lines = Vec::new();
        fn walk(
            v: &Value,
            parents: &[String],
            path: &std::path::Path,
            items: &mut Vec<Item>,
            lines: &mut Vec<usize>,
        ) {
            for s in v.as_array().into_iter().flatten() {
                let name = s["name"].as_str().unwrap_or_default().to_string();
                // DocumentSymbol (hierarchical) → selectionRange, SymbolInformation (flat) → location.range
                let start = if s["selectionRange"].is_object() {
                    &s["selectionRange"]["start"]
                } else {
                    &s["location"]["range"]["start"]
                };
                let line = start["line"].as_u64().unwrap_or(0) as usize;
                let col = start["character"].as_u64().unwrap_or(0) as usize;
                let container = s["containerName"].as_str().filter(|c| !c.is_empty()).map(str::to_string);
                let hint = container.unwrap_or_else(|| parents.join(" › "));
                items.push(Item {
                    label: name.clone(),
                    action: Action::Goto { path: path.to_path_buf(), line, col },
                    hint,
                    glyph: Some(symbol_glyph(s["kind"].as_u64().unwrap_or(0))),
                });
                lines.push(line);
                if s["children"].is_array() {
                    let mut p = parents.to_vec();
                    p.push(name);
                    walk(&s["children"], &p, path, items, lines);
                }
            }
        }
        walk(result, &[], &path, &mut items, &mut lines);
        if items.is_empty() {
            return self.set_status("no symbols in this file");
        }
        // Start at the symbol enclosing (or just before) the cursor
        let doc = self.doc();
        let cur = doc.text.byte_to_line(doc.selection().primary().head.min(doc.text.len_bytes()));
        let start = lines
            .iter()
            .enumerate()
            .filter(|(_, l)| **l <= cur)
            .max_by_key(|(_, l)| **l)
            .map_or(0, |(i, _)| i);
        let mut p = Picker::new(format!("symbols in {}", doc.display_name_short()), items, false);
        p.selected = start;
        self.open_picker(p, None);
    }

    /// `space S` — workspace-wide symbols. Re-asks the server as you type (after a short pause).
    pub fn workspace_symbols(&mut self) {
        if self.doc().lsp.client.is_none() {
            return self.set_status("no language server for this file");
        }
        let mut p = Picker::new("workspace symbols", Vec::new(), false);
        p.requery = true;
        p.loading = true;
        self.open_picker(p, None);
        self.lsp_request_raw(Kind::WorkspaceSymbols, "workspace/symbol", json!({ "query": "" }));
    }

    /// The workspace symbol picker's query changed — re-ask after a 120 ms pause.
    pub fn workspace_symbols_requery(&mut self) {
        let Some(query) = self.picker.as_ref().filter(|p| p.requery).map(|p| p.query.clone()) else { return };
        self.events.jobs().spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(120));
            move |ed: &mut Editor| {
                if ed.picker.as_ref().is_some_and(|p| p.requery && p.query == query) {
                    ed.lsp_request_raw(Kind::WorkspaceSymbols, "workspace/symbol", json!({ "query": query }));
                }
            }
        });
    }

    fn on_workspace_symbols(&mut self, result: &Value) {
        let Some(p) = self.picker.as_mut().filter(|p| p.requery) else { return };
        let root = std::env::current_dir().unwrap_or_default();
        let items: Vec<Item> = result
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|s| {
                let loc = &s["location"];
                let path = lsp::path_from_uri(loc["uri"].as_str()?)?;
                let start = &loc["range"]["start"];
                let line = start["line"].as_u64().unwrap_or(0) as usize;
                let col = start["character"].as_u64().unwrap_or(0) as usize;
                let rel = path.strip_prefix(&root).unwrap_or(&path).display().to_string();
                let container = s["containerName"].as_str().filter(|c| !c.is_empty());
                let hint = match container {
                    Some(c) => format!("{c} · {rel}:{}", line + 1),
                    None => format!("{rel}:{}", line + 1),
                };
                Some(Item {
                    label: s["name"].as_str()?.to_string(),
                    action: Action::Goto { path, line, col },
                    hint,
                    glyph: Some(symbol_glyph(s["kind"].as_u64().unwrap_or(0))),
                })
            })
            .collect();
        p.loading = false;
        p.set_items(items);
    }

    // ── Autocompletion ───────────────────────────────────────────────────

    /// A char was typed (`Some`) or deleted (`None`) in insert mode: filter the open list, re-ask, or close.
    pub fn completion_after_edit(&mut self, typed: Option<char>) {
        if self.mode != crate::editor::Mode::Insert {
            self.completion = None;
            return;
        }
        let doc = self.doc();
        let Some(cid) = doc.lsp.client else { return };
        let head = doc.selection().primary().head;
        let start = completion::word_start(&doc.text, head);
        let prefix = doc.text.byte_slice(start..head).to_string();
        let doc_id = doc.id;
        // Open list: filter while in the same word, close when leaving it
        if let Some(c) = &mut self.completion {
            let same_word = c.doc == doc_id && c.start == start && head >= c.start;
            if same_word && typed.is_none_or(completion::is_word) {
                c.filter(&prefix);
                let incomplete = c.incomplete;
                if self.completion.as_ref().is_some_and(|c| c.shown.is_empty()) && !incomplete {
                    self.completion = None;
                }
                if !incomplete {
                    return;
                }
            } else {
                self.completion = None;
            }
        }
        let Some(t) = typed else { return };
        let Some(provider) = self.client(cid).map(|c| &c.caps["completionProvider"]).filter(|p| !p.is_null())
        else {
            return;
        };
        let is_trigger = lists_char(&provider["triggerCharacters"], t);
        // Trigger chars always ask anew (the earlier response has a different word, dropped in on_completion)
        if is_trigger || (completion::is_word(t) && !prefix.is_empty() && !self.lsp.completion_inflight) {
            let context = if is_trigger {
                json!({ "triggerKind": 2, "triggerCharacter": t.to_string() })
            } else {
                json!({ "triggerKind": 1 })
            };
            if self.lsp_request(Kind::Completion, "textDocument/completion", json!({ "context": context })) {
                self.lsp.completion_inflight = true;
            }
        }
    }

    /// Fill the selected item's docs pane — if it has none, ask the server to resolve once.
    pub fn completion_docs(&mut self) {
        let lang = self.doc().syntax.as_ref().map_or_else(String::new, |s| s.lang.name.clone());
        let Some(c) = &mut self.completion else { return };
        let Some(index) = c.current_index() else {
            c.docs = None;
            return;
        };
        if c.docs.as_ref().is_some_and(|(i, _)| *i == index) {
            return;
        }
        let (cid, list) = (c.client, c.list);
        let Some(item) = c.item_mut(index) else { return };
        let can_resolve = self
            .lsp
            .clients
            .iter()
            .find(|x| x.id == cid)
            .is_some_and(|x| x.caps["completionProvider"]["resolveProvider"].as_bool() == Some(true));
        if item.raw["documentation"].is_null() && !item.resolving && can_resolve {
            item.resolving = true;
            let raw = item.raw.clone();
            self.lsp_send(cid, Kind::ResolveCompletion { list, index }, "completionItem/resolve", raw);
        }
        let Some(c) = &mut self.completion else { return };
        let md = c.item_mut(index).map(|it| completion::doc_markdown(&it.raw, &lang)).unwrap_or_default();
        let lines = crate::markdown::render(&md, &self.theme, Some(&lang));
        c.docs = Some((index, lines));
    }

    // ── Inlay hints ───────────────────────────────────────────────────────

    /// Line range to request for the current screen (visible area + one screen above and below).
    fn inlay_range(&self) -> (usize, usize) {
        let doc = self.doc();
        let rows = self.viewport.0.max(1);
        (doc.top.saturating_sub(rows), doc.top + 2 * rows)
    }

    /// Every event: if our hints are stale (version differs or don't cover the visible range), ask shortly —
    /// while typing, wait and ask once it pauses (don't hammer the server on every key).
    pub fn inlay_schedule(&mut self) {
        if !self.config.inlay_hints {
            if !self.doc().lsp.inlay.is_empty() {
                self.doc_mut().lsp.inlay.clear();
                self.doc_mut().lsp.inlay_have = None;
            }
            return;
        }
        if self.lsp.inlay_waiting || !self.inlay_stale() {
            return;
        }
        self.lsp.inlay_waiting = true;
        let version = self.doc().version();
        self.events.jobs().spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(INLAY_DEBOUNCE_MS));
            move |ed: &mut Editor| {
                ed.lsp.inlay_waiting = false;
                // Typed again meanwhile — wait once more
                if ed.doc().version() != version {
                    return ed.inlay_schedule();
                }
                ed.inlay_request();
            }
        });
    }

    pub(crate) fn inlay_stale(&self) -> bool {
        let doc = self.doc();
        let Some(cid) = doc.lsp.client else { return false };
        let ready = self.lsp.clients.iter().find(|c| c.id == cid).is_some_and(|c| {
            c.ready && !c.caps["inlayHintProvider"].is_null() && c.caps["inlayHintProvider"] != json!(false)
        });
        if !ready || doc.loading {
            return false;
        }
        let (a, b) = self.inlay_range();
        let covers = |(v, (x, y)): (u64, (usize, usize))| {
            v == doc.version() && x <= a.max(doc.top) && y >= b.min(doc.top + self.viewport.0)
        };
        let inflight = self.lsp.inlay_inflight.is_some_and(|(d, v, r)| d == doc.id && covers((v, r)));
        !inflight && !doc.lsp.inlay_have.is_some_and(covers)
    }

    pub fn inlay_request(&mut self) {
        if !self.inlay_stale() {
            return;
        }
        self.lsp_flush();
        let (a, b) = self.inlay_range();
        let doc = self.doc();
        let (Some(cid), Some(path), id, version) = (doc.lsp.client, doc.path.clone(), doc.id, doc.version())
        else {
            return;
        };
        let last = mv::last_line(&doc.text);
        let range = json!({ "start": { "line": a.min(last), "character": 0 }, "end": { "line": (b + 1).min(last + 1), "character": 0 } });
        let params = json!({ "textDocument": { "uri": lsp::uri(&path) }, "range": range });
        if self.lsp_send(cid, Kind::InlayHint { range: (a, b) }, "textDocument/inlayHint", params).is_some() {
            self.lsp.inlay_inflight = Some((id, version, (a, b)));
        }
    }

    /// Hint response: drop if the doc changed since (shifted old hints hold their place until new ones come).
    fn on_inlay(&mut self, cid: ClientId, p: &Pending, range: (usize, usize), result: &Value) {
        self.lsp.inlay_inflight = None;
        let enc = self.encoding_of(cid);
        let Some(doc) = self.docs.iter_mut().find(|d| d.id == p.doc) else { return };
        if doc.version() != p.version {
            return;
        }
        doc.lsp.inlay = lsp::inlay_hints(&doc.text, result, enc);
        doc.lsp.inlay_have = Some((p.version, range));
    }

    // ── Signature help ────────────────────────────────────────────────────

    /// After a key in insert mode: ask on trigger chars (`(` `,` …); while shown, re-ask on every edit/move
    /// (the server reports when the current argument changes or the parens are left — empty reply = close).
    pub fn signature_after_key(&mut self, typed: Option<char>) {
        let Some(cid) = self.doc().lsp.client else { return };
        let provider = self.client(cid).map(|c| c.caps["signatureHelpProvider"].clone());
        let Some(provider) = provider.filter(|p| !p.is_null()) else { return };
        let listed = |key: &str, t: char| lists_char(&provider[key], t);
        let trigger = typed.filter(|&t| listed("triggerCharacters", t));
        let open = self.signature.is_some();
        if trigger.is_none() && !open {
            return;
        }
        if self.lsp.signature_inflight {
            self.lsp.signature_again = true;
            return;
        }
        let context = match (trigger, typed) {
            (Some(t), _) => {
                json!({ "triggerKind": 2, "triggerCharacter": t.to_string(), "isRetrigger": open })
            }
            (None, Some(t)) if listed("retriggerCharacters", t) => {
                json!({ "triggerKind": 2, "triggerCharacter": t.to_string(), "isRetrigger": true })
            }
            _ => json!({ "triggerKind": 3, "isRetrigger": true }),
        };
        if self.lsp_request(Kind::SignatureHelp, "textDocument/signatureHelp", json!({ "context": context }))
        {
            self.lsp.signature_inflight = true;
        }
    }

    /// Signature response: if the doc changed since the request, drop it (edits happened) and ask once more.
    fn on_signature(&mut self, p: &Pending, result: &Value, failed: bool) {
        self.lsp.signature_inflight = false;
        let again = std::mem::take(&mut self.lsp.signature_again);
        if self.mode != crate::editor::Mode::Insert || self.doc().id != p.doc {
            return;
        }
        if self.doc().version() != p.version || again {
            if self.signature.is_some() || !failed {
                let context = json!({ "triggerKind": 3, "isRetrigger": self.signature.is_some() });
                let params = json!({ "context": context });
                if self.lsp_request(Kind::SignatureHelp, "textDocument/signatureHelp", params) {
                    self.lsp.signature_inflight = true;
                }
            }
            return;
        }
        self.signature = if failed { None } else { crate::signature::Signature::from_lsp(p.doc, result) };
    }

    /// Invoke explicitly (insert-mode `C-x`) — asks even with an empty word.
    pub fn completion_request(&mut self) {
        self.completion = None;
        if self.lsp_request(
            Kind::Completion,
            "textDocument/completion",
            json!({ "context": { "triggerKind": 1 } }),
        ) {
            self.lsp.completion_inflight = true;
        }
    }

    /// Completion response — accepted and filtered even if more was typed since, if still in the same word
    /// (a plain "drop if the cursor moved" would mean the list never shows during fast typing).
    fn on_completion(&mut self, cid: ClientId, p: &Pending, result: &Value) {
        if self.mode != crate::editor::Mode::Insert || self.doc().id != p.doc {
            return;
        }
        let doc = self.doc();
        let head = doc.selection().primary().head;
        let start = completion::word_start(&doc.text, p.head);
        if head < start || completion::word_start(&doc.text, head) != start {
            return;
        }
        let prefix = doc.text.byte_slice(start..head).to_string();
        self.lsp.completion_gen += 1;
        let mut c = Completion::new(cid, p.doc, start, result);
        c.list = self.lsp.completion_gen;
        c.filter(&prefix);
        self.completion = (!c.shown.is_empty()).then_some(c);
    }

    /// Insert the selected item (snippet expanded, cursor at first placeholder, extra edits like imports).
    pub fn completion_accept(&mut self) {
        let Some(c) = self.completion.take() else { return };
        let Some(item) = c.current() else { return };
        let raw = item.raw.clone();
        let enc = self.encoding_of(c.client);
        self.with_group(|cx| {
            let doc = cx.editor.doc_mut();
            let head = doc.selection().primary().head;
            let edit = &raw["textEdit"];
            let text = edit["newText"]
                .as_str()
                .or_else(|| raw["insertText"].as_str())
                .or_else(|| raw["label"].as_str())
                .unwrap_or_default();
            let (plain, cursor) = if raw["insertTextFormat"].as_u64() == Some(2) {
                completion::strip_snippet(text)
            } else {
                (text.to_string(), text.len())
            };
            // Start of the server-given range (else word start) .. current cursor
            let range_start = edit["range"]["start"]
                .is_object()
                .then(|| &edit["range"]["start"])
                .or_else(|| edit["insert"]["start"].is_object().then(|| &edit["insert"]["start"]));
            let from = range_start
                .and_then(|s| lsp::from_position(&doc.text, s, enc))
                .map_or(c.start, |f| f.min(c.start));
            let replaced = (head.saturating_sub(from), plain.clone(), cursor);
            let mut changes = vec![crate::transaction::Change { from, to: head, insert: plain }];
            for e in raw["additionalTextEdits"].as_array().into_iter().flatten() {
                if let (Some(a), Some(b), Some(t)) = (
                    lsp::from_position(&doc.text, &e["range"]["start"], enc),
                    lsp::from_position(&doc.text, &e["range"]["end"], enc),
                    e["newText"].as_str(),
                ) {
                    changes.push(crate::transaction::Change { from: a, to: b.max(a), insert: t.to_string() });
                }
            }
            let tx = crate::transaction::Transaction::new(changes);
            let pos = tx.map_pos(from, crate::transaction::Assoc::Before) + cursor;
            doc.apply_with(&tx, Selection::point(pos));
            // `.` replays this as the text it put in (repeat.rs)
            let (replace, text, cursor) = replaced;
            cx.editor.insert_completion(replace, text, cursor);
        });
    }

    // ── Applying edits (shared by code actions, rename, format) ─────────────

    /// TextEdit[] → one transaction (one undo per doc). Ranges use this server's position encoding.
    pub(crate) fn apply_text_edits(
        &mut self,
        doc_id: DocId,
        enc: Encoding,
        edits: &[Value],
    ) -> Result<(), String> {
        let doc = self.docs.iter_mut().find(|d| d.id == doc_id).ok_or("buffer closed")?;
        if doc.loading {
            return Err("still loading".into());
        }
        let changes = edits
            .iter()
            .filter_map(|e| {
                let from = lsp::from_position(&doc.text, &e["range"]["start"], enc)?;
                let to = lsp::from_position(&doc.text, &e["range"]["end"], enc)?;
                Some(crate::transaction::Change {
                    from,
                    to: to.max(from),
                    insert: e["newText"].as_str()?.to_string(),
                })
            })
            .collect();
        let before = doc.snapshot();
        doc.apply(&crate::transaction::Transaction::new(changes));
        doc.commit_undo(before);
        Ok(())
    }

    /// WorkspaceEdit (`changes` or `documentChanges`) — multiple files, in order. Unopened files are opened
    /// and edited (current buffer stays). Also file create/move/delete (`resourceOperations`) — not undoable.
    /// Returns the number of files changed (including created·renamed·deleted).
    pub fn apply_workspace_edit(&mut self, cid: ClientId, edit: &Value) -> Result<usize, String> {
        let enc = self.encoding_of(cid);
        let current = self.doc().id;
        let mut n = 0;
        let path_of = |u: &Value| -> Result<std::path::PathBuf, String> {
            let u = u.as_str().ok_or("missing uri")?;
            lsp::path_from_uri(u).ok_or_else(|| format!("bad uri {u}"))
        };
        // Both present → documentChanges only (spec)
        let doc_changes = edit["documentChanges"].as_array();
        if let Some(map) = edit["changes"].as_object().filter(|_| doc_changes.is_none()) {
            for (u, edits) in map {
                let path = path_of(&Value::String(u.clone()))?;
                self.edit_file(&path, enc, edits.as_array().map(Vec::as_slice).unwrap_or_default())?;
                n += 1;
            }
        }
        // Edits for a versioned doc the user has changed since → the offsets are stale; apply nothing
        for dc in doc_changes.into_iter().flatten() {
            if let (Some(v), Ok(path)) =
                (dc["textDocument"]["version"].as_i64(), path_of(&dc["textDocument"]["uri"]))
                && let Some(doc) = self.open_doc_at(&path).filter(|d| d.lsp.client == Some(cid))
                && (i64::from(doc.lsp.version) != v || !doc.lsp.changes.is_empty() || doc.lsp.full_sync)
            {
                return Err(format!("{} changed since the server computed this edit", doc.display_name()));
            }
        }
        for dc in doc_changes.into_iter().flatten() {
            let opts = &dc["options"];
            let flag = |k: &str| opts[k].as_bool().unwrap_or(false);
            match dc["kind"].as_str() {
                Some("create") => {
                    let path = path_of(&dc["uri"])?;
                    if path.exists() && !flag("overwrite") {
                        if flag("ignoreIfExists") {
                            continue;
                        }
                        return Err(format!("{} already exists", path.display()));
                    }
                    if let Some(dir) = path.parent() {
                        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
                    }
                    std::fs::write(&path, "").map_err(|e| format!("create {}: {e}", path.display()))?;
                }
                Some("rename") => {
                    let (from, to) = (path_of(&dc["oldUri"])?, path_of(&dc["newUri"])?);
                    if to.exists() && !flag("overwrite") {
                        if flag("ignoreIfExists") {
                            continue;
                        }
                        return Err(format!("{} already exists", to.display()));
                    }
                    self.rename_file(&from, &to)?;
                }
                Some("delete") => {
                    let path = path_of(&dc["uri"])?;
                    if !path.exists() {
                        if flag("ignoreIfNotExists") {
                            continue;
                        }
                        return Err(format!("{} does not exist", path.display()));
                    }
                    let r = if path.is_dir() {
                        if flag("recursive") {
                            std::fs::remove_dir_all(&path)
                        } else {
                            std::fs::remove_dir(&path)
                        }
                    } else {
                        std::fs::remove_file(&path)
                    };
                    r.map_err(|e| format!("delete {}: {e}", path.display()))?;
                    // Close buffers that were open (including files under it)
                    let gone: Vec<DocId> = self
                        .docs
                        .iter()
                        .filter(|d| d.path.as_deref().is_some_and(|p| p.starts_with(&path)))
                        .map(|d| d.id)
                        .collect();
                    for id in gone {
                        self.close_doc(id);
                    }
                }
                _ => {
                    let path = path_of(&dc["textDocument"]["uri"])?;
                    self.edit_file(
                        &path,
                        enc,
                        dc["edits"].as_array().map(Vec::as_slice).unwrap_or_default(),
                    )?;
                }
            }
            n += 1;
        }
        if let Some(i) = self.docs.iter().position(|d| d.id == current) {
            self.current = i;
        }
        self.lsp_flush();
        Ok(n)
    }

    /// Open doc for this path (paths of docs are canonical).
    pub(crate) fn open_doc_at(&self, path: &std::path::Path) -> Option<&crate::document::Document> {
        let path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        self.docs.iter().find(|d| d.path.as_deref() == Some(path.as_path()))
    }

    /// TextEdit[] for one file — opened if not already open.
    fn edit_file(&mut self, path: &std::path::Path, enc: Encoding, edits: &[Value]) -> Result<(), String> {
        let id = match self.open_doc_at(path) {
            Some(d) => d.id,
            None => {
                self.open(path).map_err(|e| format!("{e:#}"))?;
                self.doc().id
            }
        };
        self.apply_text_edits(id, enc, edits)
    }

    /// Move a file — an open buffer takes the new name too (content intact); the server gets close + reopen.
    fn rename_file(&mut self, from: &std::path::Path, to: &std::path::Path) -> Result<(), String> {
        if let Some(dir) = to.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        std::fs::rename(from, to).map_err(|e| format!("rename {}: {e}", from.display()))?;
        let from = std::fs::canonicalize(from.parent().unwrap_or(from))
            .map(|d| d.join(from.file_name().unwrap_or_default()))
            .unwrap_or_else(|_| from.to_path_buf());
        let to = std::fs::canonicalize(to).unwrap_or_else(|_| to.to_path_buf());
        let moved: Vec<DocId> = self
            .docs
            .iter()
            .filter(|d| d.path.as_deref().is_some_and(|p| p.starts_with(&from)))
            .map(|d| d.id)
            .collect();
        for id in moved {
            self.lsp_did_close(id);
            let doc = self.docs.iter_mut().find(|d| d.id == id).expect("listed above");
            let old = doc.path.take().unwrap_or_default();
            let rest = old.strip_prefix(&from).map(|r| r.to_path_buf()).unwrap_or_default();
            let new = if rest.as_os_str().is_empty() { to.clone() } else { to.join(rest) };
            doc.disk = crate::disk::stamp(&new);
            doc.path = Some(new);
            self.attach_lsp(id);
        }
        Ok(())
    }

    /// Tell the server "this document was closed" (buffer closed or renamed).
    pub fn lsp_did_close(&mut self, id: DocId) {
        let Some(doc) = self.docs.iter_mut().find(|d| d.id == id) else { return };
        let (Some(cid), Some(path)) = (doc.lsp.client.take(), doc.path.clone()) else { return };
        doc.lsp.changes.clear();
        doc.lsp.diagnostics.clear();
        doc.lsp.inlay.clear();
        if let Some(c) = self.client_mut(cid) {
            c.notify("textDocument/didClose", json!({ "textDocument": { "uri": lsp::uri(&path) } }));
        }
    }

    /// Code action preview state (for drawing).
    pub fn action_preview(&self, i: usize) -> Option<&ActionPreview> {
        self.lsp.action_previews.get(&i)
    }

    /// Selected row in the code action picker — starts building its preview if needed (called on row moves).
    pub fn code_action_preview(&mut self) {
        let Some(Action::Code(i)) =
            self.picker.as_ref().and_then(|p| p.current()).map(|it| it.action.clone())
        else {
            return;
        };
        self.code_action_preview_of(i, true);
    }

    fn code_action_preview_of(&mut self, i: usize, may_resolve: bool) {
        if self.lsp.action_previews.contains_key(&i) {
            return;
        }
        let (cid, gen_) = (self.lsp.actions.0, self.lsp.actions_gen);
        let Some(a) = self.lsp.actions.1.get(i).cloned() else { return };
        let edit = &a["edit"];
        // Command-only actions have nothing to preview. Empty edits (lazy action) → ask for the edit first
        if a["command"].is_string() || (edit.is_null() && !may_resolve) {
            self.lsp.action_previews.insert(i, ActionPreview::CommandOnly);
            return;
        }
        if edit.is_null() {
            // The reply is dropped if the list changed by then
            let kind = Kind::ResolvePreview { list: gen_, index: i };
            if self.lsp_send(cid, kind, "codeAction/resolve", a).is_some() {
                self.lsp.action_previews.insert(i, ActionPreview::Working);
            }
            return;
        }
        // Open buffers use their current content; the rest is read from disk on a worker thread
        let enc = self.encoding_of(cid);
        let open: HashMap<std::path::PathBuf, ropey::Rope> =
            self.docs.iter().filter_map(|d| Some((d.path.clone()?, d.text.clone()))).collect();
        let edit = edit.clone();
        self.lsp.action_previews.insert(i, ActionPreview::Working);
        self.events.jobs().spawn(move || {
            let files = crate::editdiff::build(&edit, enc, |p| {
                let p = std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
                open.get(&p).cloned().or_else(|| crate::document::read_file(&p).ok().filter(|_| p.exists()))
            });
            move |ed: &mut Editor| {
                if ed.lsp.actions_gen == gen_ {
                    ed.lsp.action_previews.insert(i, ActionPreview::Ready(std::sync::Arc::new(files)));
                }
            }
        });
    }

    /// Code action picked in the picker.
    pub fn run_code_action(&mut self, i: usize) {
        let (cid, list) = &self.lsp.actions;
        let (cid, action) = (*cid, list.get(i).cloned());
        if let Some(a) = action {
            self.run_code_action_value(cid, a, true);
        }
    }

    /// Run a Command | CodeAction: apply edits if any, ask the server to execute the command if any.
    /// If neither edit nor command (lazily computed), codeAction/resolve first.
    fn run_code_action_value(&mut self, cid: ClientId, a: Value, may_resolve: bool) {
        if a["command"].is_string() {
            return self.execute_command(cid, &a);
        }
        let (edit, cmd) = (&a["edit"], &a["command"]);
        if edit.is_null() && cmd.is_null() && may_resolve {
            self.lsp_send(cid, Kind::ResolveAction, "codeAction/resolve", a);
            return;
        }
        if !edit.is_null()
            && let Err(e) = self.apply_workspace_edit(cid, edit)
        {
            return self.set_error(format!("code action: {e}"));
        }
        if cmd.is_object() {
            self.execute_command(cid, cmd);
        }
    }

    fn execute_command(&mut self, cid: ClientId, cmd: &Value) {
        // jdtls: wraps the edit in a client command — apply without asking the server
        if cmd["command"] == "java.apply.workspaceEdit" {
            for edit in cmd["arguments"].as_array().into_iter().flatten() {
                if let Err(e) = self.apply_workspace_edit(cid, edit) {
                    return self.set_error(format!("code action: {e}"));
                }
            }
            return;
        }
        let params = json!({ "command": cmd["command"], "arguments": cmd["arguments"] });
        self.lsp_send(cid, Kind::ExecuteCommand, "workspace/executeCommand", params);
    }

    /// `space a` — code actions for the selection (cursor). Overlapping diagnostics go along as context.
    pub fn code_action(&mut self) {
        let doc = self.doc();
        let Some(cid) = doc.lsp.client else { return self.set_status("no language server for this file") };
        let enc = self.encoding_of(cid);
        let r = doc.selection().primary().min_width_1(&doc.text);
        let diags: Vec<Value> = doc
            .lsp
            .diagnostics
            .iter()
            .filter(|d| d.from <= r.to() && d.to >= r.from())
            .map(|d| d.raw.clone())
            .collect();
        let range = json!({ "start": lsp::to_position(&doc.text, r.from(), enc), "end": lsp::to_position(&doc.text, r.to(), enc) });
        self.lsp_request(
            Kind::CodeAction,
            "textDocument/codeAction",
            json!({ "range": range, "context": { "diagnostics": diags, "triggerKind": 1 } }),
        );
    }

    /// `:w` / `:wq` with format-on-save: ask the server to format and save when it answers — true if
    /// asked (the caller then doesn't save). A slow server gets 2 s, then the file is saved as is.
    pub fn format_then_save(&mut self, quit: bool) -> bool {
        let doc = self.doc();
        let (Some(path), Some(cid)) = (doc.path.clone(), doc.lsp.client) else { return false };
        if !self.config.format_on_save || doc.loading || doc.virtual_uri.is_some() {
            return false;
        }
        let can = self.client(cid).is_some_and(|c| {
            let cap = &c.caps["documentFormattingProvider"];
            c.ready && !cap.is_null() && *cap != json!(false)
        });
        if !can {
            return false;
        }
        self.lsp_flush();
        let tab = self.config.tab_width;
        let params = json!({
            "textDocument": { "uri": lsp::uri(&path) },
            "options": { "tabSize": tab, "insertSpaces": true },
        });
        let Some(id) = self.lsp_send(cid, Kind::FormatSave { quit }, "textDocument/formatting", params)
        else {
            return false;
        };
        let doc_id = self.doc().id;
        self.note("formatting…");
        self.events.jobs().spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs(2));
            move |ed: &mut Editor| {
                if ed.lsp.pending.remove(&(cid, id)).is_some() {
                    ed.format_save_done(doc_id, quit, Some("the formatter took too long — saved as is"));
                }
            }
        });
        true
    }

    /// Formatting settled (or gave up with `why`): save the document, then quit for `:wq`.
    fn format_save_done(&mut self, id: DocId, quit: bool, why: Option<&str>) {
        if let Err(e) = crate::typed::save_doc(self, id, false) {
            return self.set_error(e);
        }
        if let Some(why) = why {
            self.set_warning(why.to_string());
        }
        if quit && let Err(e) = crate::typed::quit(self, false) {
            self.set_error(e);
        }
    }

    /// `g d`·`g D`·`g y`·`g i`·`g r` — asks only a server that says it can answer.
    pub fn goto_location(&mut self, goto: Goto) {
        let Some(cid) = self.ready_client() else { return };
        let Some(c) = self.client(cid) else { return };
        if let Some(cap) = goto.provider().map(|p| &c.caps[p])
            && (cap.is_null() || *cap == json!(false))
        {
            let name = c.name.clone();
            return self.set_status(format!("{name} can't find {}", goto.noun()));
        }
        let extra = match goto {
            Goto::References => json!({ "context": { "includeDeclaration": true } }),
            _ => json!({}),
        };
        self.lsp_request(Kind::Goto(goto), goto.method(), extra);
    }

    fn lsp_jump(&mut self, path: &std::path::Path, pos: &Value, enc: Encoding) {
        self.push_jump();
        if let Err(e) = self.open(path) {
            return self.set_error(format!("{e:#}"));
        }
        self.lsp.jump_after_load = Some((self.doc().id, pos.clone(), enc));
        self.jump_if_loaded(self.doc().id);
    }

    /// A go-to target waiting for this doc — taken once its text is there (big files load in the background).
    fn jump_if_loaded(&mut self, id: DocId) {
        let Some(doc) = self.docs.iter_mut().find(|d| d.id == id && !d.loading) else { return };
        let Some((_, pos, enc)) = self.lsp.jump_after_load.take_if(|(d, ..)| *d == id) else { return };
        if let Some(b) = lsp::from_position(&doc.text, &pos, enc) {
            doc.set_selection(Selection::point(b));
        }
    }

    /// `]d` / `[d` — next/previous diagnostic (wraps around). Selects the diagnostic range.
    pub fn goto_diagnostic(&mut self, forward: bool) {
        let doc = self.doc_mut();
        let head = doc.selection().primary().cursor(&doc.text);
        let list = &doc.lsp.diagnostics;
        if list.is_empty() {
            return self.set_status("no diagnostics");
        }
        let d = if forward {
            list.iter().find(|d| d.from > head).or(list.first())
        } else {
            list.iter().rev().find(|d| d.from < head).or(list.last())
        };
        let d = d.expect("non-empty").clone();
        let end = d.to.max(crate::graphemes::next_boundary(&doc.text, d.from));
        doc.set_selection(Selection::single(crate::selection::Range::new(d.from, end)));
        self.note(d.message);
    }

    /// `space d` — diagnostics list for this document.
    pub fn diagnostics_picker(&mut self) {
        let doc = self.doc();
        let Some(path) = doc.path.clone() else { return };
        let items: Vec<Item> = doc
            .lsp
            .diagnostics
            .iter()
            .map(|d| {
                let line = mv::line_of(&doc.text, d.from);
                let col = d.from - mv::line_start(&doc.text, line);
                let sev = ["error", "error", "warning", "info", "hint"][d.severity as usize];
                Item {
                    label: format!(
                        "{}:{} {sev}: {}",
                        line + 1,
                        col + 1,
                        d.message.lines().next().unwrap_or_default()
                    ),
                    action: Action::Goto { path: path.clone(), line, col },
                    hint: String::new(),
                    glyph: None,
                }
            })
            .collect();
        self.open_picker(Picker::new("diagnostics", items, false), None);
    }

    /// For the status line — what the current document's server is doing.
    pub fn lsp_progress(&self) -> Option<String> {
        let c = self.client(self.doc().lsp.client?)?;
        if !c.ready {
            return Some(format!("{}: starting", c.name));
        }
        c.progress_text()
    }

    /// Server `cid` closed its stdout (exited or crashed) — forget it and everything waiting on it; its docs
    /// go without a server (reopening the file starts a new one).
    pub fn lsp_exited(&mut self, cid: ClientId) {
        let Some(i) = self.lsp.clients.iter().position(|c| c.id == cid) else { return };
        let name = self.lsp.clients.remove(i).name.clone();
        self.lsp.pending.retain(|(c, _), _| *c != cid);
        self.lsp.completion_inflight = false;
        (self.lsp.signature_inflight, self.lsp.signature_again) = (false, false);
        self.lsp.inlay_inflight = None;
        for d in self.docs.iter_mut().filter(|d| d.lsp.client == Some(cid)) {
            d.lsp.client = None;
            d.lsp.changes.clear();
            d.lsp.full_sync = false;
            d.lsp.diagnostics.clear();
            d.lsp.inlay.clear();
            d.lsp.inlay_have = None;
        }
        if self.completion.as_ref().is_some_and(|c| c.client == cid) {
            self.completion = None;
        }
        self.signature = None;
        if self.lsp.actions.0 == cid {
            self.lsp.actions.1.clear();
            self.lsp.action_previews.clear();
        }
        if self.java_debug.as_ref().is_some_and(|p| p.cid == cid) {
            self.java_debug_fail(format!("{name} exited"));
        }
        self.set_error(format!("lsp: {name} exited"));
    }
}

/// Whether a capability's character list (`triggerCharacters` …) has `t`.
fn lists_char(list: &Value, t: char) -> bool {
    let mut buf = [0; 4];
    let t = &*t.encode_utf8(&mut buf);
    list.as_array().is_some_and(|a| a.iter().any(|x| x.as_str() == Some(t)))
}

/// LSP SymbolKind → (glyph, theme scope) — same glyph rules as autocompletion.
pub fn symbol_glyph(kind: u64) -> (&'static str, &'static str) {
    match kind {
        6 | 9 | 12 => ("ƒ", "function"),
        5 | 10 | 11 | 19 | 23 | 26 => ("τ", "type"),
        1..=4 => ("§", "namespace"),
        7 | 8 | 20 => ("◦", "variable.other.member"),
        14 | 22 => ("π", "constant"),
        13 => ("ν", "variable"),
        _ => ("·", "ui.virtual"),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::Duration;

    use super::*;
    use crate::key::Key;
    use crate::selection::Range;

    fn feed(ed: &mut Editor, keys: &str) {
        let mut chars = keys.chars();
        while let Some(c) = chars.next() {
            let key: Key = if c == '<' {
                chars.by_ref().take_while(|&c| c != '>').collect::<String>().parse().unwrap()
            } else {
                c.to_string().parse().unwrap()
            };
            ed.handle_key(key);
        }
        ed.lsp_flush();
    }

    /// Fake server = `cat` writing everything it receives to a file. Tests feed responses via on_lsp_message.
    fn setup(name: &str, src: &str) -> (Editor, PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!("tarae-lsp-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("main.rs");
        std::fs::write(&file, src).unwrap();
        let log = dir.join("sent.log");
        let cfg = format!(
            "[lsp.fake]\ncommand = \"sh\"\nargs = [\"-c\", \"cat > '{}'\"]\n[lang.rust]\nlsp = [\"fake\"]\n",
            log.display()
        );
        let (config, w) = crate::config::parse(&cfg);
        assert!(w.is_empty(), "{w:?}");
        let mut ed = Editor::new(config);
        ed.open(&file).unwrap();
        let file = std::fs::canonicalize(&file).unwrap();
        (ed, file, log)
    }

    fn sent(log: &PathBuf) -> String {
        std::thread::sleep(Duration::from_millis(150)); // writer thread
        std::fs::read_to_string(log).unwrap_or_default()
    }

    fn ready(ed: &mut Editor) -> ClientId {
        let cid = ed.doc().lsp.client.expect("attached");
        let init = json!({ "jsonrpc": "2.0", "id": 0, "result": { "capabilities": { "positionEncoding": "utf-8" } } });
        ed.on_lsp_message(cid, init);
        cid
    }

    #[test]
    fn lifecycle_and_incremental_sync() {
        let (mut ed, file, log) = setup("sync", "fn main() {\n    let 타래 = 1;\n}\n");
        assert!(!ed.lsp.clients[0].ready);
        assert!(!ed.lsp_request(Kind::Hover, "textDocument/hover", json!({})), "no requests before ready");
        // Edits before ready are queued
        feed(&mut ed, "jglix<esc>");
        assert!(!sent(&log).contains("didChange"));
        ready(&mut ed);
        assert_eq!(ed.lsp.clients[0].encoding, Encoding::Utf8);
        let out = sent(&log);
        assert!(out.contains("\"method\":\"initialize\""));
        assert!(out.contains("textDocument/didOpen") && out.contains(&lsp::uri(&file)));
        // `gl` = line-end char (';'), x before it: line 1, byte col = "    let 타래 = 1" = 4+4+6+1+1+1+1 = 18
        assert!(
            out.contains(
                r#""range":{"end":{"character":18,"line":1},"start":{"character":18,"line":1}},"text":"x""#
            ),
            "{out}"
        );
        std::fs::remove_dir_all(file.parent().unwrap()).ok();
    }

    #[test]
    fn diagnostics_follow_edits_and_navigate() {
        let (mut ed, file, _log) = setup("diag", "fn main() {\n    let x = 1;\n}\n");
        let cid = ready(&mut ed);
        let diag = json!({ "method": "textDocument/publishDiagnostics", "params": { "uri": lsp::uri(&file), "diagnostics": [
            { "range": { "start": { "line": 1, "character": 8 }, "end": { "line": 1, "character": 9 } }, "severity": 2, "message": "unused variable: `x`" }
        ]}});
        ed.on_lsp_message(cid, diag);
        assert_eq!(ed.doc().diagnostic_counts(), [0, 1]);
        feed(&mut ed, "]d");
        assert_eq!(ed.doc().selection().primary(), Range::new(20, 21), "selects the diagnostic range");
        assert_eq!(ed.status.as_ref().map(|s| s.0.as_str()), Some("unused variable: `x`"));
        // Inserting a line above moves the diagnostic down too
        feed(&mut ed, "ggO// hi<esc>");
        assert_eq!(ed.doc().lsp.diagnostics[0].from, 26);
        std::fs::remove_dir_all(file.parent().unwrap()).ok();
    }

    /// Latest request of this kind (payloads aren't compared).
    fn pending_of(ed: &Editor, kind: Kind) -> u64 {
        ed.lsp
            .pending
            .iter()
            .filter(|(_, p)| std::mem::discriminant(&p.kind) == std::mem::discriminant(&kind))
            .map(|((_, id), _)| *id)
            .max()
            .expect("request sent")
    }

    #[test]
    fn code_action_rename_format_and_server_edits() {
        let (mut ed, file, log) = setup("edit", "fn main() {\n    let x = 1;\n}\n");
        let cid = ready(&mut ed);
        let uri = lsp::uri(&file);
        let edit = |line: u64, from: u64, to: u64, text: &str| json!({ "range": { "start": { "line": line, "character": from }, "end": { "line": line, "character": to } }, "newText": text });
        // Code action: list → pick in picker → apply edit, one undo reverts it
        feed(&mut ed, "j8l<space>a");
        assert!(sent(&log).contains("textDocument/codeAction"));
        let id = pending_of(&ed, Kind::CodeAction);
        let actions = json!([
            { "title": "Rename to _x", "kind": "quickfix", "edit": { "changes": { uri.clone(): [edit(1, 8, 9, "_x")] } } },
            { "title": "Inline", "kind": "refactor.inline", "edit": { "changes": {} } }
        ]);
        ed.on_lsp_message(cid, json!({ "id": id, "result": actions }));
        assert_eq!(ed.picker.as_ref().map(|p| p.counts()), Some((2, 2)));
        feed(&mut ed, "<ret>");
        assert_eq!(ed.doc().text.to_string(), "fn main() {\n    let _x = 1;\n}\n");
        feed(&mut ed, "u");
        assert_eq!(ed.doc().text.to_string(), "fn main() {\n    let x = 1;\n}\n", "applying is one undo");
        // Rename: prompt (prefilled with the word under cursor) → request → edits
        feed(&mut ed, "<space>r");
        assert_eq!(ed.prompt.as_ref().map(|p| p.text.as_str()), Some("x"));
        feed(&mut ed, "<backspace>count<ret>");
        let id = pending_of(&ed, Kind::Rename);
        // Versioned edits for an older version of the doc (we've sent 2: the action, then the undo) → refused
        let stale = json!({ "documentChanges": [
            { "textDocument": { "uri": uri.clone(), "version": 1 }, "edits": [edit(1, 8, 9, "count")] }
        ]});
        assert!(ed.apply_workspace_edit(cid, &stale).is_err());
        assert_eq!(ed.doc().text.to_string(), "fn main() {\n    let x = 1;\n}\n", "nothing applied");
        // `changes` alongside `documentChanges` is ignored (spec)
        ed.on_lsp_message(
            cid,
            json!({ "id": id, "result": {
                "changes": { uri.clone(): [edit(0, 0, 2, "XX")] },
                "documentChanges": [
                    { "textDocument": { "uri": uri.clone(), "version": 2 }, "edits": [edit(1, 8, 9, "count")] }
                ]
            }}),
        );
        assert_eq!(ed.doc().text.to_string(), "fn main() {\n    let count = 1;\n}\n");
        // Format
        feed(&mut ed, ":fmt<ret>");
        let id = pending_of(&ed, Kind::Format);
        ed.on_lsp_message(cid, json!({ "id": id, "result": [edit(1, 0, 4, "  ")] }));
        assert!(ed.doc().text.to_string().contains("\n  let count = 1;"));
        // Server-initiated edit (workspace/applyEdit) → apply and reply applied: true
        ed.on_lsp_message(cid, json!({ "id": 77, "method": "workspace/applyEdit", "params": { "edit": { "changes": { uri: [edit(0, 3, 7, "start")] } } } }));
        assert!(ed.doc().text.to_string().starts_with("fn start()"));
        assert!(
            sent(&log).contains(r#""id":77,"jsonrpc":"2.0","result":{"applied":true}"#),
            "{}",
            sent(&log)
        );
        std::fs::remove_dir_all(file.parent().unwrap()).ok();
    }

    /// Apply worker-thread results until the condition holds.
    fn settle_until(ed: &mut Editor, done: impl Fn(&Editor) -> bool) {
        let end = std::time::Instant::now() + Duration::from_secs(5);
        while !done(ed) {
            let left = end.saturating_duration_since(std::time::Instant::now());
            let ev = ed.events.recv_timeout(left).expect("event before timeout");
            ed.handle_event(ev);
        }
    }

    #[test]
    fn code_action_preview_resolves_lazily_and_shows_the_diff() {
        let (mut ed, file, log) = setup("preview", "fn main() {\n    let x = 1;\n}\n");
        let cid = ready(&mut ed);
        let uri = lsp::uri(&file);
        feed(&mut ed, "j8l<space>a");
        let id = pending_of(&ed, Kind::CodeAction);
        // The first is a lazy action (no edit + data) — once selected, only its edit is fetched via resolve
        let actions = json!([
            { "title": "Rename to _x", "kind": "quickfix", "data": { "n": 1 } },
            { "title": "Run tests", "command": "rust-analyzer.runSingle" }
        ]);
        ed.on_lsp_message(cid, json!({ "id": id, "result": actions }));
        assert!(matches!(ed.action_preview(0), Some(ActionPreview::Working)));
        assert!(sent(&log).contains("codeAction/resolve"));
        let rid = pending_of(&ed, Kind::ResolvePreview { list: 0, index: 0 });
        let resolved = json!({ "title": "Rename to _x", "edit": { "changes": { uri.clone(): [
            { "range": { "start": { "line": 1, "character": 8 }, "end": { "line": 1, "character": 9 } }, "newText": "_x" }
        ] } } });
        ed.on_lsp_message(cid, json!({ "id": rid, "result": resolved }));
        settle_until(&mut ed, |ed| matches!(ed.action_preview(0), Some(ActionPreview::Ready(_))));
        let Some(ActionPreview::Ready(files)) = ed.action_preview(0) else { unreachable!() };
        assert_eq!(crate::editdiff::summary(files), "1 file  +1 −1");
        assert_eq!(ed.doc().text.to_string(), "fn main() {\n    let x = 1;\n}\n", "preview doesn't modify");
        // The diff is drawn in the right pane (wide screen)
        let mut buf = Vec::new();
        crate::term::render(&mut ed, &mut buf, 140, 20).unwrap();
        let screen = String::from_utf8_lossy(&buf);
        assert!(
            screen.contains("main.rs  +1 −1") && screen.contains("− ") && screen.contains("+ "),
            "{screen}"
        );
        // Command-only actions have nothing to preview
        feed(&mut ed, "<down>");
        assert!(matches!(ed.action_preview(1), Some(ActionPreview::CommandOnly)));
        // Picking applies the stored edit directly (no re-ask)
        feed(&mut ed, "<up><ret>");
        assert_eq!(ed.doc().text.to_string(), "fn main() {\n    let _x = 1;\n}\n");
        std::fs::remove_dir_all(file.parent().unwrap()).ok();
    }

    #[test]
    fn workspace_edit_creates_renames_and_deletes_files() {
        let (mut ed, file, _log) = setup("fileops", "mod util;\nfn main() {}\n");
        let cid = ready(&mut ed);
        let dir = file.parent().unwrap().to_path_buf();
        std::fs::write(dir.join("util.rs"), "pub fn f() {}\n").unwrap();
        std::fs::write(dir.join("junk.rs"), "x").unwrap();
        ed.open(&dir.join("util.rs")).unwrap();
        ed.open(&dir.join("junk.rs")).unwrap();
        ed.current = 0;
        let u = |n: &str| lsp::uri(&dir.join(n));
        let we = json!({ "documentChanges": [
            { "kind": "create", "uri": u("new/extra.rs") },
            { "textDocument": { "uri": u("new/extra.rs"), "version": null }, "edits": [
                { "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 0 } }, "newText": "pub fn g() {}\n" }
            ] },
            { "kind": "rename", "oldUri": u("util.rs"), "newUri": u("helpers.rs") },
            { "kind": "delete", "uri": u("junk.rs") },
        ] });
        assert_eq!(ed.apply_workspace_edit(cid, &we), Ok(4));
        // Created file was opened and edited (saving is up to the user)
        let extra =
            ed.docs.iter().find(|d| d.path.as_deref().is_some_and(|p| p.ends_with("new/extra.rs"))).unwrap();
        assert_eq!(extra.text.to_string(), "pub fn g() {}\n");
        assert!(dir.join("new/extra.rs").exists());
        // Renamed file: on disk and the open buffer's name
        assert!(!dir.join("util.rs").exists() && dir.join("helpers.rs").exists());
        assert!(ed.docs.iter().any(|d| d.path.as_deref().is_some_and(|p| p.ends_with("helpers.rs"))));
        // Deleted file's buffer is closed too
        assert!(!dir.join("junk.rs").exists());
        assert!(!ed.docs.iter().any(|d| d.path.as_deref().is_some_and(|p| p.ends_with("junk.rs"))));
        assert_eq!(ed.doc().path.as_deref(), Some(file.as_path()), "the viewed buffer stays");
        // Creating an existing file is refused (without the overwrite option)
        let again = json!({ "documentChanges": [{ "kind": "create", "uri": u("helpers.rs") }] });
        assert!(ed.apply_workspace_edit(cid, &again).is_err());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn completion_request_filter_and_accept() {
        let (mut ed, file, log) = setup("comp", "fn main() {\n    let v = Vec::new();\n    \n}\n");
        let cid = ed.doc().lsp.client.unwrap();
        let caps =
            json!({ "positionEncoding": "utf-8", "completionProvider": { "triggerCharacters": ["."] } });
        ed.on_lsp_message(cid, json!({ "id": 0, "result": { "capabilities": caps } }));
        // Ask at '.' (trigger), and filter with chars typed before the response arrives
        feed(&mut ed, "jjAv.pu");
        assert!(sent(&log).contains(r#""triggerCharacter":".""#), "{}", sent(&log));
        let id = pending_of(&ed, Kind::Completion);
        let items = json!({ "isIncomplete": false, "items": [
            { "label": "push", "kind": 2, "insertTextFormat": 2, "detail": "fn(&mut self, value: T)", "textEdit": {
                "range": { "start": { "line": 2, "character": 6 }, "end": { "line": 2, "character": 8 } },
                "newText": "push(${1:value})$0" },
              "additionalTextEdits": [{ "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 0 } }, "newText": "use x;\n" }] },
            { "label": "pop", "kind": 2 },
            { "label": "len", "kind": 2 },
        ]});
        ed.on_lsp_message(cid, json!({ "id": id, "result": items }));
        let c = ed.completion.as_ref().expect("list shows");
        assert_eq!(c.shown.len(), 1, "\"pu\" filters to push only");
        assert!(c.current().is_none());
        feed(&mut ed, "x");
        assert!(ed.completion.is_none(), "closes when nothing matches");
        feed(&mut ed, "<backspace>");
        assert!(ed.completion.is_none(), "a closed list doesn't reopen on delete");
        feed(&mut ed, "<C-x>");
        ed.on_lsp_message(cid, json!({ "id": pending_of(&ed, Kind::Completion), "result": items }));
        assert!(ed.completion.is_some(), "C-x asks again");
        // Pick with Tab, insert with Enter: snippet expanded + extra edits, cursor at the first placeholder
        feed(&mut ed, "<tab>");
        let docs = ed.completion.as_ref().and_then(|c| c.docs.as_ref()).expect("docs show when selected");
        assert!(!docs.1.is_empty());
        feed(&mut ed, "<ret>");
        assert_eq!(
            ed.doc().text.to_string(),
            "use x;\nfn main() {\n    let v = Vec::new();\n    v.push(value)\n}\n"
        );
        feed(&mut ed, "!<esc>");
        assert!(ed.doc().text.to_string().contains("v.push(!value)"));
        std::fs::remove_dir_all(file.parent().unwrap()).ok();
    }

    #[test]
    fn late_resolve_for_an_older_completion_list_is_dropped() {
        let (mut ed, file, _log) = setup("resolve", "fn main() {\n    \n}\n");
        let cid = ed.doc().lsp.client.unwrap();
        let caps = json!({ "positionEncoding": "utf-8", "completionProvider": { "resolveProvider": true } });
        ed.on_lsp_message(cid, json!({ "id": 0, "result": { "capabilities": caps } }));
        let list = |labels: &[&str]| json!({ "items": labels.iter().map(|l| json!({ "label": l, "kind": 3 })).collect::<Vec<_>>() });
        feed(&mut ed, "jA<C-x>");
        ed.on_lsp_message(
            cid,
            json!({ "id": pending_of(&ed, Kind::Completion), "result": list(&["alpha"]) }),
        );
        feed(&mut ed, "<tab>");
        let resolve = pending_of(&ed, Kind::ResolveCompletion { list: 0, index: 0 });
        // A new list before the resolve reply — same index, different item
        feed(&mut ed, "<C-x>");
        ed.on_lsp_message(cid, json!({ "id": pending_of(&ed, Kind::Completion), "result": list(&["beta"]) }));
        let old = json!({ "label": "alpha", "documentation": "alpha docs" });
        ed.on_lsp_message(cid, json!({ "id": resolve, "result": old }));
        let c = ed.completion.as_ref().expect("new list shows");
        assert_eq!(c.item(0).label, "beta");
        assert!(c.item(0).raw["documentation"].is_null(), "alpha's docs didn't land on beta");
        std::fs::remove_dir_all(file.parent().unwrap()).ok();
    }

    #[test]
    fn document_symbols_for_a_buffer_no_longer_shown_are_dropped() {
        let (mut ed, file, _log) = setup("symbols", "fn main() {}\n");
        let cid = ready(&mut ed);
        ed.document_symbols();
        let id = pending_of(&ed, Kind::DocumentSymbols);
        let other = file.with_file_name("other.txt");
        std::fs::write(&other, "text\n").unwrap();
        ed.open(&other).unwrap();
        let sym = json!([{ "name": "main", "kind": 12, "selectionRange": { "start": { "line": 0, "character": 3 } } }]);
        ed.on_lsp_message(cid, json!({ "id": id, "result": sym }));
        assert!(ed.picker.is_none(), "main.rs symbols don't open over other.txt");
        std::fs::remove_dir_all(file.parent().unwrap()).ok();
    }

    #[test]
    fn resolved_code_action_is_dropped_if_the_file_changed() {
        let (mut ed, file, _log) = setup("stale-action", "fn main() {\n    let x = 1;\n}\n");
        let cid = ready(&mut ed);
        let uri = lsp::uri(&file);
        feed(&mut ed, "j8l<space>a");
        let lazy = json!([{ "title": "Rename to _x", "kind": "quickfix", "data": { "n": 1 } }]);
        ed.on_lsp_message(cid, json!({ "id": pending_of(&ed, Kind::CodeAction), "result": lazy }));
        // Preview's resolve fails, so picking asks again (ResolveAction)
        let rid = pending_of(&ed, Kind::ResolvePreview { list: 0, index: 0 });
        ed.on_lsp_message(cid, json!({ "id": rid, "error": { "message": "later" } }));
        feed(&mut ed, "<ret>");
        let id = pending_of(&ed, Kind::ResolveAction);
        feed(&mut ed, "ggOuse a;<esc>");
        let resolved = json!({ "title": "Rename to _x", "edit": { "changes": { uri: [
            { "range": { "start": { "line": 1, "character": 8 }, "end": { "line": 1, "character": 9 } }, "newText": "_x" }
        ] } } });
        ed.on_lsp_message(cid, json!({ "id": id, "result": resolved }));
        assert_eq!(
            ed.doc().text.to_string(),
            "use a;\nfn main() {\n    let x = 1;\n}\n",
            "not at stale offsets"
        );
        std::fs::remove_dir_all(file.parent().unwrap()).ok();
    }

    #[test]
    fn utf16_server_gets_incremental_changes_from_the_first_edit() {
        let (mut ed, file, log) = setup("u16", "fn main() {}\n");
        let cid = ed.doc().lsp.client.unwrap();
        ed.on_lsp_message(cid, json!({ "id": 0, "result": { "capabilities": {} } }));
        assert_eq!(ed.lsp.clients[0].encoding, Encoding::Utf16);
        feed(&mut ed, "i타<esc>");
        let out = sent(&log);
        assert!(out.contains(r#""contentChanges":[{"range""#), "incremental, not full sync: {out}");
        std::fs::remove_dir_all(file.parent().unwrap()).ok();
    }

    #[test]
    fn server_exit_clears_its_state() {
        let (mut ed, file, _log) = setup("exit", "fn main() {}\n");
        let cid = ready(&mut ed);
        feed(&mut ed, "i<C-x>");
        assert!(ed.lsp.completion_inflight && !ed.lsp.pending.is_empty());
        ed.lsp_exited(cid);
        assert!(!ed.lsp.completion_inflight && ed.lsp.pending.is_empty());
        assert!(ed.lsp.clients.is_empty() && ed.doc().lsp.client.is_none());
        assert!(ed.status.as_ref().is_some_and(|s| s.0.contains("fake exited")));
        std::fs::remove_dir_all(file.parent().unwrap()).ok();
    }

    #[test]
    fn inlay_hints_request_map_and_draw() {
        let (mut ed, file, log) = setup("inlay", "fn main() {\n    let x = add(1, 2);\n}\n");
        let cid = ed.doc().lsp.client.unwrap();
        let caps = json!({ "positionEncoding": "utf-8", "inlayHintProvider": true });
        ed.on_lsp_message(cid, json!({ "id": 0, "result": { "capabilities": caps } }));
        ed.viewport = (10, 80);
        ed.inlay_request();
        assert!(sent(&log).contains("textDocument/inlayHint"));
        assert!(!ed.inlay_stale(), "no re-ask while in flight");
        let hints = json!([
            { "position": { "line": 1, "character": 9 }, "label": ": i32", "kind": 1 },
            { "position": { "line": 1, "character": 16 }, "label": [{ "value": "a" }, { "value": ":" }], "paddingRight": true },
        ]);
        ed.on_lsp_message(
            cid,
            json!({ "id": pending_of(&ed, Kind::InlayHint { range: (0, 0) }), "result": hints }),
        );
        let pos: Vec<usize> = ed.doc().lsp.inlay.iter().map(|h| h.pos).collect();
        assert_eq!(pos, [21, 28]);
        assert_eq!(ed.doc().lsp.inlay[1].text, "a: ");
        assert!(!ed.inlay_stale(), "same version and range → no re-ask");
        // Typing before it pushes it along, and the version changed, so it's time to re-ask
        feed(&mut ed, "jwiy<esc>");
        assert_eq!(ed.doc().lsp.inlay.iter().map(|h| h.pos).collect::<Vec<_>>(), [22, 29]);
        assert!(ed.inlay_stale());
        // On screen: `let yx: i32 = add(a: 1, 2);`
        let mut buf = Vec::new();
        crate::term::render(&mut ed, &mut buf, 80, 8).unwrap();
        let screen = String::from_utf8_lossy(&buf);
        assert!(screen.contains(": i32") && screen.contains("a: "), "{screen}");
        std::fs::remove_dir_all(file.parent().unwrap()).ok();
    }

    #[test]
    fn signature_help_follows_typing() {
        let (mut ed, file, log) = setup("sig", "fn main() {\n    \n}\n");
        let cid = ed.doc().lsp.client.unwrap();
        let caps = json!({ "positionEncoding": "utf-8", "signatureHelpProvider": { "triggerCharacters": ["(", ","] } });
        ed.on_lsp_message(cid, json!({ "id": 0, "result": { "capabilities": caps } }));
        let help = |active: u64| {
            json!({ "signatures": [{ "label": "fn add(a: i32, b: &str)",
            "parameters": [{ "label": [7, 13] }, { "label": [15, 22] }] }], "activeParameter": active })
        };
        feed(&mut ed, "jAadd");
        assert!(ed.lsp.pending.values().all(|p| p.kind != Kind::SignatureHelp), "no ask before a trigger");
        feed(&mut ed, "(");
        assert!(sent(&log).contains(r#""triggerCharacter":"(""#));
        ed.on_lsp_message(cid, json!({ "id": pending_of(&ed, Kind::SignatureHelp), "result": help(0) }));
        assert_eq!(ed.signature.as_ref().and_then(|s| s.active), Some((7, 13)));
        // While shown, re-ask on every edit. Typing again (",") while in flight → stale reply: drop, re-ask
        feed(&mut ed, "1");
        let stale = pending_of(&ed, Kind::SignatureHelp);
        feed(&mut ed, ",");
        ed.on_lsp_message(cid, json!({ "id": stale, "result": help(0) }));
        let fresh = pending_of(&ed, Kind::SignatureHelp);
        assert!(fresh > stale, "re-asked after the stale response");
        ed.on_lsp_message(cid, json!({ "id": fresh, "result": help(1) }));
        assert_eq!(ed.signature.as_ref().and_then(|s| s.active), Some((15, 22)), "next argument");
        // Leaving the parens → server replies empty → closes. Leaving insert mode closes too
        feed(&mut ed, "2)");
        for _ in 0..2 {
            // The reply to "2" is stale (")" was typed after) — the re-asked one is the real one
            ed.on_lsp_message(cid, json!({ "id": pending_of(&ed, Kind::SignatureHelp), "result": null }));
        }
        assert!(ed.signature.is_none());
        feed(&mut ed, "(");
        ed.on_lsp_message(cid, json!({ "id": pending_of(&ed, Kind::SignatureHelp), "result": help(0) }));
        assert!(ed.signature.is_some());
        feed(&mut ed, "<esc>");
        assert!(ed.signature.is_none());
        std::fs::remove_dir_all(file.parent().unwrap()).ok();
    }

    #[test]
    fn goto_kinds_ask_their_method_and_check_the_server_can() {
        let (mut ed, file, log) = setup("gotos", "trait T {}\nstruct S;\nimpl T for S {}\n");
        let cid = ed.doc().lsp.client.unwrap();
        let caps = json!({ "positionEncoding": "utf-8", "implementationProvider": true, "typeDefinitionProvider": {} });
        ed.on_lsp_message(cid, json!({ "id": 0, "result": { "capabilities": caps } }));
        // gD: the server didn't say it does declarations — nothing sent, the toast says why
        feed(&mut ed, "6lgD");
        assert!(ed.lsp.pending.is_empty());
        assert!(ed.status.as_ref().is_some_and(|(m, _)| m.contains("declarations")), "{:?}", ed.status);
        // gi: several → picker titled after the kind
        feed(&mut ed, "gi");
        assert!(sent(&log).contains(r#""method":"textDocument/implementation""#));
        let at = |line: u64| json!({ "uri": lsp::uri(&file), "range": { "start": { "line": line, "character": 0 }, "end": { "line": line, "character": 1 } } });
        let id = pending_of(&ed, Kind::Goto(Goto::Implementation));
        ed.on_lsp_message(cid, json!({ "id": id, "result": [at(1), at(2)] }));
        assert_eq!(ed.picker.as_ref().map(|p| p.title.as_str()), Some("implementations"));
        feed(&mut ed, "<esc>");
        // gy: one → straight there, and C-o comes back
        feed(&mut ed, "gy");
        assert!(sent(&log).contains(r#""method":"textDocument/typeDefinition""#));
        let id = pending_of(&ed, Kind::Goto(Goto::TypeDefinition));
        ed.on_lsp_message(cid, json!({ "id": id, "result": at(1) }));
        assert_eq!(ed.doc().selection().primary(), Range::point(11));
        feed(&mut ed, "<C-o>");
        assert_eq!(ed.doc().selection().primary(), Range::point(6), "back on `T`");
        std::fs::remove_dir_all(file.parent().unwrap()).ok();
    }

    /// `:w` formats first, then saves; a server error or an edit made meanwhile still saves (as is); a
    /// server that can't format saves right away.
    #[test]
    fn format_on_save() {
        let (mut ed, file, log) = setup("fmtsave", "fn main() {\n    let x = 1;\n}\n");
        let cid = ed.doc().lsp.client.unwrap();
        let caps = json!({ "positionEncoding": "utf-8", "documentFormattingProvider": true });
        ed.on_lsp_message(cid, json!({ "id": 0, "result": { "capabilities": caps } }));
        let disk = || std::fs::read_to_string(&file).unwrap();
        let two = json!([{ "range": { "start": { "line": 1, "character": 0 }, "end": { "line": 1, "character": 4 } }, "newText": "  " }]);
        feed(&mut ed, "ggA//<esc>:w<ret>");
        assert!(sent(&log).contains(r#""method":"textDocument/formatting""#));
        assert_eq!(disk(), "fn main() {\n    let x = 1;\n}\n", "not saved before the answer");
        ed.on_lsp_message(
            cid,
            json!({ "id": pending_of(&ed, Kind::FormatSave { quit: false }), "result": two }),
        );
        assert_eq!(disk(), "fn main() {//\n  let x = 1;\n}\n", "formatted, then saved");
        assert!(!ed.doc().is_modified());
        // Server error → saved as is
        feed(&mut ed, "A!<esc>:w<ret>");
        let id = pending_of(&ed, Kind::FormatSave { quit: false });
        ed.on_lsp_message(cid, json!({ "id": id, "error": { "message": "no" } }));
        assert_eq!(disk(), "fn main() {//!\n  let x = 1;\n}\n");
        // Typed on while formatting → the reply is stale, what's there is saved
        feed(&mut ed, "A?<esc>:w<ret>A#<esc>");
        let id = pending_of(&ed, Kind::FormatSave { quit: false });
        ed.on_lsp_message(cid, json!({ "id": id, "result": two }));
        assert_eq!(disk(), "fn main() {//!?#\n  let x = 1;\n}\n");
        // No formatter → saved right away
        let caps = json!({ "positionEncoding": "utf-8" });
        ed.lsp.clients[0].caps = caps;
        feed(&mut ed, "A%<esc>:w<ret>");
        assert_eq!(disk(), "fn main() {//!?#%\n  let x = 1;\n}\n");
        std::fs::remove_dir_all(file.parent().unwrap()).ok();
    }

    #[test]
    fn definition_hover_and_stale_responses() {
        let (mut ed, file, _log) = setup("def", "fn helper() {}\nfn main() { helper(); }\n");
        let cid = ready(&mut ed);
        let pending_id = |ed: &Editor| *ed.lsp.pending.keys().next().map(|(_, id)| id).unwrap();
        // gd → response → jump
        feed(&mut ed, "j3wgd");
        let id = pending_id(&ed);
        let loc = json!({ "uri": lsp::uri(&file), "range": { "start": { "line": 0, "character": 3 }, "end": { "line": 0, "character": 9 } } });
        ed.on_lsp_message(cid, json!({ "id": id, "result": loc }));
        assert_eq!(ed.doc().selection().primary(), Range::point(3));
        // hover → popup text, any key closes it
        feed(&mut ed, " k");
        let id = pending_id(&ed);
        let hover =
            json!({ "contents": { "kind": "markdown", "value": "```rust\nfn helper()\n```\nhelps" } });
        ed.on_lsp_message(cid, json!({ "id": id, "result": hover }));
        let shown = crate::markdown::wrap(ed.popup.as_deref().unwrap(), 40, Default::default());
        let plain: Vec<String> = shown.iter().map(|l| l.iter().map(|s| s.text.as_str()).collect()).collect();
        assert_eq!(plain, ["fn helper()", "", "helps"]);
        feed(&mut ed, "<C-d>");
        assert!(ed.popup.is_some() && ed.popup_scroll > 0, "C-d scrolls (doesn't close)");
        feed(&mut ed, "<esc>");
        assert!(ed.popup.is_none());
        assert_eq!(ed.popup_scroll, 0);
        // Late response: drop if the cursor moved after the request
        feed(&mut ed, " kl");
        let id = pending_id(&ed);
        ed.on_lsp_message(cid, json!({ "id": id, "result": hover }));
        assert!(ed.popup.is_none(), "hover arriving after the cursor moved is dropped");
        std::fs::remove_dir_all(file.parent().unwrap()).ok();
    }
}
