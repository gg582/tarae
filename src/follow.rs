//! Follow mode — while Claude explores in the chat, the editor goes where it is reading and a thought card
//! beside the code says what it is thinking (`C-f` in the chat · `llm.follow`).
//!
//! - Read → that file, cursor on the first line read, centered; the lines read get a faint accent wash.
//!   Grep in content mode → its first hit (from the tool result). A note → its lines (its card opens there).
//!   The answer's first `path:line` at the end.
//! - Files follow opened are previews: unedited ones close when it moves on (they stay once the turn ends).
//! - The user wins: moving the cursor or switching files in the editor pauses following until `C-f` or
//!   the next question. `C-o` goes back to where you were before the turn (one jump point per turn).
//! - The card (drawn by `term::draw_thought_card`) shows what Claude is doing and its latest words —
//!   the thinking summary (`--thinking-display summarized`) or what it says between lookups.

use std::ops::Range;
use std::path::{Path, PathBuf};

use crate::chat::{Look, Role, refs_in};
use crate::document::DocId;
use crate::editor::Editor;
use crate::selection::Selection;

/// Where Claude is looking right now.
#[derive(Clone, Debug)]
pub struct Gaze {
    pub look: Look,
    /// The file, when the lookup has one.
    pub path: Option<PathBuf>,
    /// 0-based lines read (washed) — none for whole-file reads and searches.
    pub lines: Option<Range<usize>>,
}

#[derive(Default)]
pub struct Follow {
    pub on: bool,
    /// The user took over this turn.
    pub paused: bool,
    pub gaze: Option<Gaze>,
    /// Tool call id → name (to read a Grep result when it comes back).
    calls: Vec<(String, String)>,
    /// A jump point was left this turn.
    jumped: bool,
    /// Documents follow opened this turn (closed when it moves on, unless edited).
    previews: Vec<DocId>,
    /// Where follow last put the cursor — anything else means the user moved.
    placed: Option<(DocId, usize, usize)>,
}

impl Follow {
    /// A new question: follow again, forget this turn's previews (they're the user's now).
    pub fn new_turn(&mut self) {
        self.paused = false;
        self.gaze = None;
        self.calls.clear();
        self.jumped = false;
        self.previews.clear();
        self.placed = None;
    }

    pub fn active(&self) -> bool {
        self.on && !self.paused
    }
}

fn follow(ed: &mut Editor) -> Option<&mut Follow> {
    ed.chat.as_mut().map(|c| &mut c.follow)
}

/// Where the cursor is (document, selection) — compared with `placed`.
fn cursor_key(ed: &Editor) -> (DocId, usize, usize) {
    let r = ed.doc().selection().primary();
    (ed.doc().id, r.anchor, r.head)
}

/// A tool call started.
pub fn on_tool(ed: &mut Editor, id: &str, name: &str, input: &serde_json::Value, look: &Look) {
    let Some(f) = follow(ed) else { return };
    f.calls.push((id.to_string(), name.to_string()));
    let (path, lines, line) = match name {
        "Read" => {
            let n = |k: &str| input[k].as_u64().map(|v| v as usize);
            let from = n("offset").unwrap_or(1).max(1) - 1;
            let lines = n("offset").or(n("limit")).map(|_| from..from + n("limit").unwrap_or(2000));
            (look.link.as_ref().map(|l| l.path.clone()), lines, from)
        }
        // A note (or a reply on one): to its lines — the card shows there
        "mcp__tarae__note" => match &look.link {
            Some(l) => {
                let end = input["end_line"].as_u64().map_or(l.line + 1, |e| (e as usize).max(l.line + 1));
                (Some(l.path.clone()), Some(l.line..end), l.line)
            }
            None => (None, None, 0),
        },
        _ => (None, None, 0),
    };
    f.gaze = Some(Gaze { look: look.clone(), path: path.clone(), lines });
    if let Some(p) = path {
        go(ed, &p, line);
    }
}

/// A tool finished — a content-mode Grep moves to its first hit.
pub fn on_result(ed: &mut Editor, id: &str, text: &str) {
    let Some(f) = follow(ed) else { return };
    if !f.calls.iter().any(|(i, n)| i == id && n == "Grep") {
        return;
    }
    let Some((_, hit)) = text.lines().take(20).find_map(|l| refs_in(l).into_iter().next()) else { return };
    if let Some(g) = &mut f.gaze {
        g.path = Some(hit.path.clone());
        g.lines = Some(hit.line..hit.line + 1);
    }
    go(ed, &hit.path, hit.line);
}

/// The answer is complete: to its first reference, and the gaze ends.
pub fn on_done(ed: &mut Editor) {
    let answer = ed.chat.as_ref().and_then(|c| {
        let turn = c.msgs.iter().rposition(|m| m.role == Role::User).map_or(0, |i| i + 1);
        c.msgs[turn..].iter().rev().find(|m| m.role == Role::Assistant).map(|m| m.text.clone())
    });
    if let Some((_, link)) = answer.as_deref().and_then(|a| refs_in(a).into_iter().next()) {
        go(ed, &link.path, link.line);
    }
    if let Some(f) = follow(ed) {
        f.gaze = None;
    }
}

/// Moves the editor there (when following and the user hasn't taken over).
fn go(ed: &mut Editor, path: &Path, line: usize) {
    if !ed.chat.as_ref().is_some_and(|c| c.follow.active()) || !path.is_file() {
        return;
    }
    let already = ed.docs.iter().any(|d| d.path.as_deref() == Some(path));
    if !ed.chat.as_ref().is_some_and(|c| c.follow.jumped) {
        ed.push_jump();
    }
    if let Err(e) = ed.open(path) {
        return ed.set_error(format!("{e:#}"));
    }
    let doc = ed.doc_mut();
    let line = line.min(crate::movement::last_line(&doc.text));
    doc.set_selection(Selection::point(crate::movement::line_start(&doc.text, line)));
    ed.align_view(crate::viewalign::Place::Center);
    let here = ed.doc().id;
    let placed = cursor_key(ed);
    let Some(f) = follow(ed) else { return };
    f.jumped = true;
    f.placed = Some(placed);
    // Previews it opened before and nobody edited go away
    let stale: Vec<DocId> = f.previews.iter().copied().filter(|&id| id != here).collect();
    f.previews.retain(|&id| id == here);
    if !already {
        f.previews.push(here);
    }
    for id in stale {
        if ed.docs.iter().any(|d| d.id == id && !d.is_modified()) {
            ed.close_doc(id);
        }
    }
}

/// Every event: the user moved the cursor or switched files since follow last did → pause.
pub fn watch(ed: &mut Editor) {
    let now = cursor_key(ed);
    let Some(f) = follow(ed) else { return };
    if f.active()
        && let Some(p) = f.placed
        && p != now
    {
        f.paused = true;
        f.placed = None;
    }
}

/// `C-f`: off → on · paused → following again · on → off. Turning it on goes to the current gaze.
pub fn toggle(ed: &mut Editor) {
    let Some(f) = follow(ed) else { return };
    let resume = !f.active();
    if resume {
        f.on = true;
        f.paused = false;
    } else {
        f.on = false;
    }
    let target =
        f.gaze.as_ref().and_then(|g| Some((g.path.clone()?, g.lines.as_ref().map_or(0, |l| l.start))));
    if resume {
        ed.set_status("following Claude — move in the editor to take over");
        if let Some((path, line)) = target {
            go(ed, &path, line);
        }
    } else {
        ed.set_status("follow off");
    }
}
