//! Chat panel — talk to Claude in the right-hand panel (M4).
//!
//! - One process lives for the whole conversation (stream-json input takes many turns — remembers context).
//!   Spawned the moment the panel opens (prewarm), so even the first question skips the cold start.
//! - Each send attaches the current file·cursor·selection·diagnostics as `<editor-context>`. The full file
//!   only the first time that version is sent (not resent every time), and only around the cursor if large.
//! - Answers are drawn as they stream; stop (`C-c`) is an interrupt control request — process, memory stay.
//! - A code block in the answer: `C-r` applies it to the selection (via the review diff), `C-y` copies it.

use std::cell::RefCell;
use std::collections::HashMap;
use std::io::Write;
use std::process::ChildStdin;
use std::time::Instant;

use crate::document::DocId;
use crate::editor::Editor;
use crate::key::{Code, Key};
use crate::llm::{self, StreamEv};
use crate::markdown::Span;

const SYSTEM: &str = "You are Claude, built into tarae — a terminal code editor. Each user message starts with \
<editor-context>: the user's current file, cursor line, selection and diagnostics. Answer concisely in Markdown. \
Put code in fenced blocks with a language tag. When the user asks to change the selection, reply with the complete \
replacement for the selection in a single code block (the editor can apply it).";

/// Example questions filled in with Tab in an empty chat.
pub const SUGGESTIONS: [&str; 3] =
    ["Explain what this file does", "Why is this diagnostic happening?", "Write tests for the selection"];

/// Size limit for sending the whole file as context (beyond it, ±40 lines around the cursor).
const WHOLE_FILE_MAX: usize = 60_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
    /// Dimmed notice (stopped·error·new chat).
    Note,
}

pub struct Msg {
    pub role: Role,
    pub text: String,
    /// Context summary attached to a user message (`main.rs · L12–18 · 2 diagnostics`).
    pub chip: String,
    /// Wrapped lines (text length, width, lines) — only changed messages are rewrapped, even while streaming.
    pub cache: RefCell<Rendered>,
}

/// (text length, width, wrapped lines).
pub type Rendered = Option<(usize, usize, Vec<Vec<Span>>)>;

impl Msg {
    fn new(role: Role, text: impl Into<String>, chip: impl Into<String>) -> Self {
        Msg { role, text: text.into(), chip: chip.into(), cache: RefCell::new(None) }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Idle,
    Thinking,
    Writing,
}

struct Proc {
    stdin: ChildStdin,
    pid: u32,
}

pub struct Chat {
    pub msgs: Vec<Msg>,
    pub input: String,
    /// Cursor within the input (bytes).
    pub cursor: usize,
    pub focused: bool,
    pub state: State,
    pub started: Instant,
    /// How many lines scrolled up from the bottom (0 = follow the bottom).
    pub scroll: usize,
    proc: Option<Proc>,
    /// Process generation — drops late events from an old process after a new chat or restart.
    generation: u64,
    /// Per document, the version whose full text was sent.
    sent: HashMap<DocId, u64>,
    stopping: bool,
}

impl Chat {
    fn new() -> Self {
        Chat {
            msgs: Vec::new(),
            input: String::new(),
            cursor: 0,
            focused: true,
            state: State::Idle,
            started: Instant::now(),
            scroll: 0,
            proc: None,
            generation: 0,
            sent: HashMap::new(),
            stopping: false,
        }
    }

    pub fn busy(&self) -> bool {
        self.state != State::Idle
    }

    /// Last code block of the last answer.
    pub fn last_code(&self) -> Option<String> {
        let msg = self.msgs.iter().rev().find(|m| m.role == Role::Assistant)?;
        let mut blocks = Vec::new();
        let mut cur: Option<String> = None;
        for line in msg.text.lines() {
            match (&mut cur, line.trim_start().starts_with("```")) {
                (None, true) => cur = Some(String::new()),
                (Some(_), true) => blocks.push(cur.take().unwrap_or_default()),
                (Some(b), false) => {
                    b.push_str(line);
                    b.push('\n');
                }
                (None, false) => {}
            }
        }
        blocks.pop()
    }

    fn kill(&mut self) {
        if let Some(p) = self.proc.take() {
            llm::kill(p.pid);
        }
    }
}

impl Drop for Chat {
    fn drop(&mut self) {
        self.kill();
    }
}

// ── Open·close ──────────────────────────────────────────────────────────────

/// `space l`: opens if absent (prewarming the process), moves focus to the input if already open.
pub fn open(ed: &mut Editor) {
    match &mut ed.chat {
        Some(c) => c.focused = true,
        None => ed.chat = Some(Chat::new()),
    }
    ensure_proc(ed);
}

pub fn close(ed: &mut Editor) {
    ed.chat = None;
}

/// New chat — fresh process (drops memory of the previous conversation).
pub fn reset(ed: &mut Editor) {
    if let Some(c) = &mut ed.chat {
        c.kill();
        c.msgs.clear();
        c.state = State::Idle;
        c.scroll = 0;
    }
    ensure_proc(ed);
}

fn ensure_proc(ed: &mut Editor) {
    let cfg = ed.config.llm.clone();
    let tx = ed.events.sender();
    let Some(c) = &mut ed.chat else { return };
    if c.proc.is_some() {
        return;
    }
    let mut child = match llm::spawn(&cfg, &["--append-system-prompt", SYSTEM]) {
        Ok(child) => child,
        Err(e) => {
            c.msgs.push(Msg::new(Role::Note, format!("{}: {e}", cfg.command), ""));
            return;
        }
    };
    let Some(stdin) = child.stdin.take() else { return };
    // A fresh process has seen no file yet
    c.sent.clear();
    c.generation += 1;
    let generation = c.generation;
    c.proc = Some(Proc { stdin, pid: child.id() });
    llm::pump(child, false, tx, move |ev| {
        Some(Box::new(move |ed: &mut Editor| on_event(ed, generation, ev)))
    });
}

fn on_event(ed: &mut Editor, generation: u64, ev: StreamEv) {
    let Some(c) = ed.chat.as_mut().filter(|c| c.generation == generation) else { return };
    match ev {
        StreamEv::Thinking => {
            if c.state == State::Idle {
                return;
            }
            c.state = State::Thinking;
        }
        StreamEv::Text(t) => {
            if c.state == State::Idle {
                return;
            }
            c.state = State::Writing;
            match c.msgs.last_mut() {
                Some(m) if m.role == Role::Assistant => m.text.push_str(&t),
                _ => c.msgs.push(Msg::new(Role::Assistant, t, "")),
            }
        }
        StreamEv::Done(r) => {
            let secs = c.started.elapsed().as_secs_f32();
            c.state = State::Idle;
            match r {
                // Answers with no streamed pieces (short answers, CLIs without partials) get the final text
                Ok(text) => {
                    if !c.msgs.last().is_some_and(|m| m.role == Role::Assistant) {
                        c.msgs.push(Msg::new(Role::Assistant, text, ""));
                    }
                    c.msgs.push(Msg::new(Role::Note, format!("{secs:.1}s"), ""));
                }
                Err(e) => {
                    let e = if std::mem::take(&mut c.stopping) { "stopped".to_string() } else { e };
                    c.msgs.push(Msg::new(Role::Note, e, ""));
                }
            }
        }
        StreamEv::Exit(err) => {
            c.proc = None;
            if c.state != State::Idle {
                c.state = State::Idle;
                let e = if err.is_empty() {
                    "claude exited".to_string()
                } else {
                    format!("claude exited: {err}")
                };
                c.msgs.push(Msg::new(Role::Note, e, ""));
            }
        }
    }
}

// ── Send ───────────────────────────────────────────────────────────────────

pub fn send(ed: &mut Editor) {
    let Some(c) = &ed.chat else { return };
    let question = c.input.trim().to_string();
    if question.is_empty() || c.busy() {
        return;
    }
    // Process first — a newly spawned one clears `sent`, so the context carries the whole file again
    ensure_proc(ed);
    let (context, chip, sent) = context(ed);
    let Some(c) = &mut ed.chat else { return };
    let Some(p) = &mut c.proc else { return };
    let content = format!("{context}\n\n{question}");
    let ok = writeln!(p.stdin, "{}", llm::user_message(&content)).and_then(|_| p.stdin.flush());
    if let Err(e) = ok {
        c.kill();
        c.msgs.push(Msg::new(Role::Note, format!("claude: {e} — press enter to retry"), ""));
        return;
    }
    if let Some((id, v)) = sent {
        c.sent.insert(id, v);
    }
    c.msgs.push(Msg::new(Role::User, question, chip));
    c.input.clear();
    c.cursor = 0;
    c.scroll = 0;
    c.state = State::Thinking;
    c.started = Instant::now();
}

pub fn stop(ed: &mut Editor) {
    let Some(c) = &mut ed.chat else { return };
    if !c.busy() {
        return;
    }
    if let Some(p) = &mut c.proc {
        c.stopping = true;
        let _ = writeln!(p.stdin, "{}", llm::interrupt_message()).and_then(|_| p.stdin.flush());
    }
}

/// Context to send + summary chip + (if the full text was sent) that document version.
fn context(ed: &Editor) -> (String, String, Option<(DocId, u64)>) {
    let doc = ed.doc();
    let text = &doc.text;
    let name = doc.display_name();
    let lang = doc.syntax.as_ref().map(|s| s.lang.name.clone()).unwrap_or_default();
    let sel = doc.selection().primary();
    let head = sel.cursor(text);
    let line = text.byte_to_line(head.min(text.len_bytes()));
    let mut ctx = format!(
        "<editor-context>\nfile: {name}{}\ncursor: line {}\n",
        if lang.is_empty() { String::new() } else { format!(" ({lang})") },
        line + 1
    );
    let mut chip = doc.display_name_short();
    let mut sent = None;
    // Selection (only when more than one character is selected)
    if sel.to() > crate::graphemes::next_boundary(text, sel.from()) {
        let (l0, l1) =
            (text.byte_to_line(sel.from()), text.byte_to_line(sel.to().saturating_sub(1).max(sel.from())));
        ctx.push_str(&format!(
            "<selection lines=\"{}-{}\">\n{}\n</selection>\n",
            l0 + 1,
            l1 + 1,
            text.byte_slice(sel.from()..sel.to())
        ));
        chip.push_str(&if l0 == l1 {
            format!(" · L{}", l0 + 1)
        } else {
            format!(" · L{}–{}", l0 + 1, l1 + 1)
        });
    }
    // File: if this version hasn't been sent yet, the full text (if small) or around the cursor
    let already = ed.chat.as_ref().and_then(|c| c.sent.get(&doc.id)) == Some(&doc.version());
    if !already {
        if text.len_bytes() <= WHOLE_FILE_MAX {
            ctx.push_str(&format!("<file-content>\n{text}</file-content>\n"));
            sent = Some((doc.id, doc.version()));
        } else {
            let (a, b) = (line.saturating_sub(40), (line + 40).min(text.len_lines().saturating_sub(1)));
            let (fa, fb) = (text.line_to_byte(a), text.line_to_byte(b + 1));
            ctx.push_str(&format!(
                "<excerpt lines=\"{}-{}\">\n{}</excerpt>\n",
                a + 1,
                b + 1,
                text.byte_slice(fa..fb)
            ));
        }
    }
    let diags = &doc.lsp.diagnostics;
    if !diags.is_empty() {
        ctx.push_str("<diagnostics>\n");
        for d in diags.iter().take(30) {
            let l = text.byte_to_line(d.from.min(text.len_bytes()));
            let kind = ["", "error", "warning", "info", "hint"][d.severity as usize % 5];
            ctx.push_str(&format!(
                "line {}: {kind}: {}\n",
                l + 1,
                d.message.lines().next().unwrap_or_default()
            ));
        }
        ctx.push_str("</diagnostics>\n");
        chip.push_str(&format!(" · {} diagnostic{}", diags.len(), if diags.len() == 1 { "" } else { "s" }));
    }
    ctx.push_str("</editor-context>");
    (ctx, chip, sent)
}

// ── Keys ───────────────────────────────────────────────────────────────────

/// Keys while focus is in the chat panel. True if handled.
pub fn key(ed: &mut Editor, key: Key) -> bool {
    let Some(c) = ed.chat.as_mut().filter(|c| c.focused) else { return false };
    let ch = |c: &Chat| c.input[..c.cursor].chars().next_back().map_or(0, char::len_utf8);
    match (key.code, key.ctrl, key.alt) {
        (Code::Esc, ..) => c.focused = false,
        (Code::Enter, false, true) | (Code::Char('j'), true, _) => {
            c.input.insert(c.cursor, '\n');
            c.cursor += 1;
        }
        (Code::Enter, ..) => send(ed),
        (Code::Char('c'), true, _) => {
            if c.busy() {
                stop(ed);
            } else {
                c.input.clear();
                c.cursor = 0;
            }
        }
        (Code::Char('l'), true, _) => reset(ed),
        (Code::Char('r'), true, _) => apply_code(ed),
        (Code::Char('y'), true, _) => match c.last_code() {
            Some(code) => {
                crate::clipboard::copy(&ed.events.jobs(), code);
                ed.set_status("copied the code block");
            }
            None => ed.set_status("no code block in the last answer"),
        },
        (Code::Char('u'), true, _) => {
            c.input.drain(..c.cursor);
            c.cursor = 0;
        }
        (Code::Char('w'), true, _) => {
            let before = &c.input[..c.cursor];
            let trimmed = before.trim_end();
            let start = trimmed.rfind(char::is_whitespace).map_or(0, |i| i + 1);
            c.input.drain(start..c.cursor);
            c.cursor = start;
        }
        (Code::Char('a'), true, _) | (Code::Home, ..) => c.cursor = 0,
        (Code::Char('e'), true, _) | (Code::End, ..) => c.cursor = c.input.len(),
        (Code::Left, ..) => c.cursor -= ch(c),
        (Code::Right, ..) => c.cursor += c.input[c.cursor..].chars().next().map_or(0, char::len_utf8),
        (Code::Backspace, ..) => {
            let n = ch(c);
            c.input.drain(c.cursor - n..c.cursor);
            c.cursor -= n;
        }
        (Code::Delete, ..) if c.cursor < c.input.len() => {
            let n = c.input[c.cursor..].chars().next().map_or(0, char::len_utf8);
            c.input.drain(c.cursor..c.cursor + n);
        }
        (Code::PageUp, ..) | (Code::Up, ..) if c.input.is_empty() || key.code == Code::PageUp => {
            c.scroll += if key.code == Code::PageUp { 10 } else { 1 };
        }
        (Code::PageDown, ..) | (Code::Down, ..) if c.input.is_empty() || key.code == Code::PageDown => {
            c.scroll = c.scroll.saturating_sub(if key.code == Code::PageDown { 10 } else { 1 });
        }
        // Tab in an empty chat = cycles through example questions
        (Code::Tab, ..) if c.msgs.is_empty() => {
            let next =
                SUGGESTIONS.iter().position(|s| *s == c.input).map_or(0, |i| (i + 1) % SUGGESTIONS.len());
            c.input = SUGGESTIONS[next].to_string();
            c.cursor = c.input.len();
        }
        (Code::Char(x), false, false) => {
            c.input.insert(c.cursor, x);
            c.cursor += x.len_utf8();
        }
        _ => {}
    }
    true
}

/// Last answer's code block → review that replaces the current selection (y/n — same path as ask).
pub fn apply_code(ed: &mut Editor) {
    let Some(code) = ed.chat.as_ref().and_then(Chat::last_code) else {
        return ed.set_status("no code block in the last answer");
    };
    let doc = ed.doc();
    let r = doc.selection().primary().min_width_1(&doc.text);
    let old = doc.text.byte_slice(r.from()..r.to()).to_string();
    let new = llm::match_trailing_newline(&old, code);
    let doc_id = doc.id;
    if let Some(c) = &mut ed.chat {
        c.focused = false;
    }
    llm::review_with(ed, doc_id, "apply code from chat", vec![(r.from(), r.to(), old, new)], None);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn editor(text: &str) -> Editor {
        let mut ed = Editor::new(crate::config::Config::default());
        ed.docs[0].text = ropey::Rope::from_str(text);
        ed
    }

    fn settle_until(ed: &mut Editor, done: impl Fn(&Editor) -> bool) {
        let end = Instant::now() + std::time::Duration::from_secs(5);
        while !done(ed) {
            let left = end.saturating_duration_since(Instant::now());
            let ev = ed.events.recv_timeout(left).expect("event before timeout");
            ed.handle_event(ev);
        }
    }

    /// One character (a multi-byte grapheme) under the cursor isn't a selection; two are.
    #[test]
    fn selection_context_counts_graphemes() {
        use crate::selection::{Range, Selection};
        let mut ed = editor("한글\n");
        ed.doc_mut().set_selection(Selection::single(Range::new(0, 3)));
        assert!(!context(&ed).0.contains("<selection"));
        ed.doc_mut().set_selection(Selection::single(Range::new(0, 6)));
        assert!(context(&ed).0.contains("<selection lines=\"1-1\">\n한글\n</selection>"));
    }

    /// claude exits after each turn: the next question goes to a new process, which gets the file again.
    #[test]
    fn new_process_gets_the_file_again() {
        let dir = std::env::temp_dir().join(format!("tarae-chat-respawn-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("stdin.log");
        let result = serde_json::json!({"type": "result", "result": "ok"}).to_string();
        let script = format!(
            "read -r line; printf '%s\\n' \"$line\" >> '{}'; printf '%s\\n' '{result}'",
            log.display()
        );
        let mut ed = editor("fn main() {}\n");
        ed.config.llm.command = "sh".into();
        ed.config.llm.args = vec!["-c".into(), script];
        open(&mut ed);
        for q in ["first", "second"] {
            ed.chat.as_mut().unwrap().input = q.into();
            send(&mut ed);
            settle_until(&mut ed, |ed| ed.chat.as_ref().is_some_and(|c| !c.busy() && c.proc.is_none()));
        }
        let sent = std::fs::read_to_string(&log).unwrap();
        assert_eq!(sent.lines().count(), 2, "{sent}");
        assert!(sent.lines().all(|l| l.contains("<file-content>")), "{sent}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn last_code_block() {
        let mut c = Chat::new();
        c.msgs.push(Msg::new(Role::Assistant, "a\n```rust\nfn a() {}\n```\nand\n```\nb\n```\n", ""));
        assert_eq!(c.last_code().as_deref(), Some("b\n"));
        c.msgs.push(Msg::new(Role::User, "q", ""));
        assert_eq!(c.last_code().as_deref(), Some("b\n"), "skips user messages");
    }
}
