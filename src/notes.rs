//! Notes — Claude's comments pinned to lines of code, each a thread you answer in place.
//!
//! - Claude writes them with the `note` tool of the chat process (an MCP server living in tarae — the chat
//!   answers `mcp_message` control requests, `chat.rs`). `reply_to` answers a thread instead.
//! - A note sits on a document mark (it follows edits); a file that isn't open keeps the line numbers and
//!   gets its mark when it opens (`attach`, every event). The line end shows `¶ first words +N`, and with
//!   the cursor on the lines a card shows the whole thread (`term::note_lines`).
//! - `space n`: reply to the note here, or ask Claude about this line (a new note starting with you).
//!   The message goes to the chat with the lines and the thread; Claude answers with `reply_to` — if it
//!   answers in plain text, that text goes into the thread. `]n`/`[n` walk notes, `space N` lists them,
//!   `:note-close` drops the one here.
//! - Session only (not saved).

use std::ops::Range;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::document::DocId;
use crate::editor::Editor;
use crate::movement as mv;
use crate::selection::{Range as SelRange, Selection};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum By {
    Claude,
    You,
}

#[derive(Clone, Debug)]
pub struct Entry {
    pub by: By,
    pub text: String,
}

#[derive(Clone, Debug)]
pub struct Note {
    pub id: u64,
    pub path: PathBuf,
    /// The mark in the open document (follows edits).
    pub at: Option<(DocId, u64)>,
    /// 0-based lines, as of the last event.
    pub lines: Range<usize>,
    pub thread: Vec<Entry>,
    /// You wrote last and Claude hasn't answered yet.
    pub waiting: bool,
}

impl Note {
    /// The first thing said — what the line end shows.
    pub fn gist(&self) -> &str {
        self.thread.first().map_or("", |e| e.text.lines().find(|l| !l.trim().is_empty()).unwrap_or(""))
    }
}

#[derive(Default)]
pub struct Notes {
    pub list: Vec<Note>,
    next: u64,
    /// The note the chat turn in flight is about (its answer lands there if Claude doesn't use the tool).
    pub turn: Option<u64>,
    /// The note shown when several share a line — the one just pinned, or the one `]n`/`[n` stepped to.
    pub focus: Option<u64>,
}

/// The `note` tool as the MCP server lists it.
pub fn tool_schema() -> Value {
    json!({
        "name": "note",
        "description": "Pin a short note to lines of code in the user's editor — it shows beside that code and \
    the user can reply there. Use it for observations tied to specific lines (a bug, a risk, a question, an idea); \
    one or two sentences each, a few per answer at most. To answer the user on an existing note, pass reply_to \
    with its id (path and line aren't needed then).",
        "inputSchema": {
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "File path, absolute or relative to the project root" },
                "line": { "type": "integer", "description": "First line, 1-based" },
                "end_line": { "type": "integer", "description": "Last line, 1-based (optional)" },
                "text": { "type": "string", "description": "The note — Markdown, one or two sentences" },
                "reply_to": { "type": "integer", "description": "Id of the note you're answering" }
            },
            "required": ["text"]
        }
    })
}

fn find(ed: &Editor, id: u64) -> Option<&Note> {
    ed.notes.list.iter().find(|n| n.id == id)
}

fn find_mut(ed: &mut Editor, id: u64) -> Option<&mut Note> {
    ed.notes.list.iter_mut().find(|n| n.id == id)
}

/// A new note on `lines` of `path`.
pub fn add(ed: &mut Editor, path: &Path, lines: Range<usize>, by: By, text: &str) -> u64 {
    ed.notes.next += 1;
    let id = ed.notes.next;
    ed.notes.list.push(Note {
        id,
        path: path.to_path_buf(),
        at: None,
        lines,
        thread: vec![Entry { by, text: text.trim().to_string() }],
        waiting: by == By::You,
    });
    ed.notes.focus = Some(id);
    attach(ed);
    id
}

/// Every event: open documents get marks for their notes, and every note's lines follow its mark.
pub fn attach(ed: &mut Editor) {
    if ed.notes.list.is_empty() {
        return;
    }
    let Editor { notes, docs, .. } = ed;
    for n in &mut notes.list {
        match n
            .at
            .and_then(|(d, m)| docs.iter().find(|doc| doc.id == d).and_then(|doc| Some((doc, doc.mark(m)?))))
        {
            Some((doc, sel)) => {
                let r = sel.primary();
                let (a, b) = (mv::line_of(&doc.text, r.anchor), mv::line_of(&doc.text, r.head));
                n.lines = a.min(b)..a.max(b) + 1;
            }
            None => {
                n.at = None;
                if let Some(doc) =
                    docs.iter_mut().find(|d| d.path.as_deref() == Some(n.path.as_path()) && !d.loading)
                {
                    let last = mv::last_line(&doc.text);
                    let (a, b) =
                        (n.lines.start.min(last), n.lines.end.saturating_sub(1).clamp(n.lines.start, last));
                    let sel = Selection::single(SelRange::new(
                        mv::line_start(&doc.text, a),
                        mv::line_start(&doc.text, b),
                    ));
                    n.at = Some((doc.id, doc.add_mark(sel)));
                    n.lines = a..b + 1;
                }
            }
        }
    }
}

/// Marks notes point at (the jump list's pruning keeps them).
pub fn marks(ed: &Editor) -> Vec<(DocId, u64)> {
    ed.notes.list.iter().filter_map(|n| n.at).collect()
}

/// Notes over the cursor line in the current document, in (first line, age) order.
pub fn all_here(ed: &Editor) -> Vec<&Note> {
    let Some(path) = ed.doc().path.as_deref() else { return Vec::new() };
    let line = ed.cursor_line();
    let mut v: Vec<&Note> =
        ed.notes.list.iter().filter(|n| n.path == path && n.lines.contains(&line)).collect();
    v.sort_by_key(|n| (n.lines.start, n.id));
    v
}

/// The note under the cursor: the focused one (just pinned, or stepped to) if it's here, else the first —
/// so `]n` reads a crowded line in order.
pub fn here(ed: &Editor) -> Option<&Note> {
    let v = all_here(ed);
    v.iter().find(|n| Some(n.id) == ed.notes.focus).or(v.first()).copied()
}

// ── The tool ────────────────────────────────────────────────────────────────

/// `note` called by Claude → the text it gets back (Err = a tool error it can correct).
pub fn tool_call(ed: &mut Editor, args: &Value) -> Result<String, String> {
    let text = args["text"].as_str().map(str::trim).filter(|t| !t.is_empty()).ok_or("text is required")?;
    if let Some(id) = args["reply_to"].as_u64() {
        let n = find_mut(ed, id).ok_or(format!("there's no note #{id}"))?;
        n.thread.push(Entry { by: By::Claude, text: text.to_string() });
        n.waiting = false;
        return Ok(format!("replied on note #{id}"));
    }
    let path =
        args["path"].as_str().filter(|p| !p.is_empty()).ok_or("path and line are required for a new note")?;
    let path = crate::chat::resolve(path);
    let path = std::fs::canonicalize(&path).unwrap_or(path);
    if !path.is_file() {
        return Err(format!("{} isn't a file", path.display()));
    }
    let line = args["line"].as_u64().ok_or("path and line are required for a new note")?.max(1) as usize - 1;
    let end = args["end_line"].as_u64().map_or(line, |e| (e.max(1) as usize - 1).max(line));
    let id = add(ed, &path, line..end + 1, By::Claude, text);
    Ok(format!("note #{id} pinned at {}:{}", crate::chat::shown(&path), line + 1))
}

/// The chat turn ended: a note still waiting gets Claude's plain answer (it didn't use `reply_to`).
pub fn turn_done(ed: &mut Editor, answer: Option<String>) {
    let Some(id) = ed.notes.turn.take() else { return };
    let Some(n) = find_mut(ed, id) else { return };
    if n.waiting {
        n.waiting = false;
        if let Some(a) = answer.map(|a| a.trim().to_string()).filter(|a| !a.is_empty()) {
            n.thread.push(Entry { by: By::Claude, text: a });
        }
    }
}

// ── You ───────────────────────────────────────────────────────────────────

/// `space n`: reply to the note here, or start one (asking Claude about the line or selection).
pub fn start(ed: &mut Editor) {
    let target = here(ed).map(|n| n.id);
    ed.open_prompt(crate::editor::PromptKind::Note(target), "");
}

/// The prompt was sent.
pub fn submit(ed: &mut Editor, target: Option<u64>, text: &str) {
    let text = text.trim();
    if text.is_empty() {
        return;
    }
    if ed.chat.as_ref().is_some_and(|c| c.busy()) {
        return ed.set_status("Claude is still answering — reply once it's done (C-c in the chat stops it)");
    }
    let fresh = target.filter(|&id| find(ed, id).is_some()).is_none();
    let id = match target.filter(|&id| find(ed, id).is_some()) {
        Some(id) => {
            let n = find_mut(ed, id).expect("checked");
            n.thread.push(Entry { by: By::You, text: text.to_string() });
            n.waiting = true;
            id
        }
        None => {
            let Some(path) = ed.doc().path.clone() else {
                return ed.set_status("save the file first — notes live in files");
            };
            let doc = ed.doc();
            let r = doc.selection().primary();
            let (a, b) = (
                mv::line_of(&doc.text, r.from()),
                mv::line_of(&doc.text, r.to().saturating_sub(1).max(r.from())),
            );
            add(ed, &path, a..b + 1, By::You, text)
        }
    };
    let n = find(ed, id).expect("just made");
    let chip = format!(
        "¶ #{id} · {} L{}",
        n.path.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default(),
        n.lines.start + 1
    );
    let about = message(ed, n);
    // The chat panel carries the conversation — opened in the background if closed; the keys stay here
    if ed.chat.is_none() {
        crate::chat::open(ed);
        if let Some(c) = &mut ed.chat {
            c.focused = false;
            c.unfolded = false;
        }
    }
    if crate::chat::submit(ed, text, Some(about), Some(chip)) {
        ed.notes.turn = Some(id);
        return;
    }
    // Claude never saw it — take it back (the chat panel says why)
    if fresh {
        ed.notes.list.retain(|n| n.id != id);
    } else if let Some(n) = find_mut(ed, id) {
        n.thread.pop();
        n.waiting = false;
    }
    ed.set_warning("couldn't reach claude — the chat panel says why");
}

/// What goes to Claude with a reply: where the note is, the code there and the thread so far.
fn message(ed: &Editor, n: &Note) -> String {
    let code = ed
        .docs
        .iter()
        .find(|d| d.path.as_deref() == Some(n.path.as_path()))
        .map(|d| {
            let last = mv::last_line(&d.text);
            let (a, b) = (n.lines.start.min(last), n.lines.end.saturating_sub(1).min(last));
            d.text
                .slice(d.text.line_to_char(a)..d.text.line_to_char((b + 1).min(d.text.len_lines())))
                .to_string()
        })
        .unwrap_or_default();
    let mut s = format!(
        "<note id=\"{}\" path=\"{}\" lines=\"{}-{}\">\n<code>\n{}</code>\n",
        n.id,
        crate::chat::shown(&n.path),
        n.lines.start + 1,
        n.lines.end,
        code
    );
    // The thread up to (not including) the message being sent
    for e in &n.thread[..n.thread.len().saturating_sub(1)] {
        s.push_str(if e.by == By::Claude { "claude: " } else { "you: " });
        s.push_str(&e.text);
        s.push('\n');
    }
    s.push_str("</note>");
    s
}

/// `:note-close` — drop the note here.
pub fn close_here(ed: &mut Editor) -> Result<(), String> {
    let id = here(ed).map(|n| n.id).ok_or("no note on this line")?;
    ed.notes.list.retain(|n| n.id != id);
    ed.set_status(format!("note #{id} closed"));
    Ok(())
}

/// `]n`/`[n` — the next/previous note in (file, line, age) order across every file, wrapping around; notes
/// sharing a line are visited one by one (the card shows the one stepped to).
pub fn step(ed: &mut Editor, forward: bool) {
    let key = |n: &Note| (n.path.clone(), n.lines.start, n.id);
    let path = ed.doc().path.clone().unwrap_or_default();
    // From the note shown here, else from the cursor line
    let here = here(ed).map(key).unwrap_or((path, ed.cursor_line(), 0));
    let mut all: Vec<(PathBuf, usize, u64)> = ed.notes.list.iter().map(key).collect();
    all.sort();
    let target = if forward {
        all.iter().find(|t| **t > here).or(all.first())
    } else {
        all.iter().rev().find(|t| **t < here).or(all.last())
    };
    match target.cloned() {
        Some((p, l, id)) => {
            ed.notes.focus = Some(id);
            if let Err(e) = ed.open_at(&p, l, 0) {
                ed.set_error(format!("{e:#}"));
            }
        }
        None => {
            ed.set_status("no notes yet — Claude pins them while it explores, or space n asks about a line")
        }
    }
}

/// `space N` — every note, newest first.
pub fn picker(ed: &mut Editor) {
    use crate::picker::{Action, Item};
    if ed.notes.list.is_empty() {
        return ed
            .set_status("no notes yet — Claude pins them while it explores, or space n asks about a line");
    }
    let items: Vec<Item> = ed
        .notes
        .list
        .iter()
        .rev()
        .map(|n| Item {
            label: format!("{}:{}  {}", crate::chat::shown(&n.path), n.lines.start + 1, n.gist()),
            action: Action::Goto { path: n.path.clone(), line: n.lines.start, col: 0 },
            hint: match n.thread.len() {
                1 => String::new(),
                k => format!("+{} replies", k - 1),
            },
            glyph: Some(("¶", "ui.accent")),
        })
        .collect();
    ed.open_picker(crate::picker::Picker::new("notes", items, false), None);
}
