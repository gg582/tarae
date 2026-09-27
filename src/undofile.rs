//! Undo that survives restarts — on save, record that file's undo history; restore it when reopening.
//!
//! `$XDG_STATE_HOME/tarae/undo/<path hash>.json` (default `~/.local/state/tarae/undo/`). History is tied
//! to the **saved content's fingerprint** (FNV-1a 64 + length) — dropped if the file changed outside.
//! Stacks are snapshots in memory, but on disk each step is one change between neighboring states
//! (middle minus common prefix/suffix): `undo[0]` = now → most recent undo state,
//! `undo[1]` = that → the one before … (redo alike).
//! Writing, reading and restoring all run on worker threads (ropes copy for free, so pass them whole).

use std::io::Write as _;
use std::path::{Path, PathBuf};

use ropey::Rope;
use serde_json::{Value, json};

use crate::disk::{is_cont, minimal_change};
use crate::document::DocId;
use crate::editor::Editor;
use crate::selection::{Range, Selection};
use crate::transaction::{Change, Transaction};

/// Undo steps kept per file (oldest dropped first).
const KEEP_STEPS: usize = 1000;
/// Files larger than this aren't recorded (keeps the history file from outgrowing the text).
const MAX_FILE_BYTES: usize = 8 << 20;
/// Number of history files — beyond it, the least recently touched are deleted first.
const KEEP_FILES: usize = 400;

pub fn dir() -> Option<PathBuf> {
    Some(crate::config::state_dir()?.join("undo"))
}

fn fnv(bytes: impl IntoIterator<Item = u8>, mut h: u64) -> u64 {
    for b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    h
}

const FNV_START: u64 = 0xcbf2_9ce4_8422_2325;

/// Content fingerprint (per chunk — faster than going byte by byte).
pub fn text_hash(text: &Rope) -> String {
    let h = text.chunks().fold(FNV_START, |h, c| fnv(c.bytes(), h));
    format!("{h:016x}")
}

fn file_for(dir: &Path, path: &Path) -> PathBuf {
    dir.join(format!("{:016x}.json", fnv(path.to_string_lossy().bytes(), FNV_START)))
}

fn sel_json(sel: &Selection) -> Value {
    let r: Vec<Value> = sel.ranges().iter().map(|r| json!([r.anchor, r.head])).collect();
    json!({ "r": r, "p": sel.primary_index() })
}

fn sel_from(v: &Value, len: usize) -> Option<Selection> {
    let ranges: Vec<Range> = v["r"]
        .as_array()?
        .iter()
        .filter_map(|r| Some(Range::new(r[0].as_u64()? as usize, r[1].as_u64()? as usize)))
        .collect();
    if ranges.is_empty() {
        return None;
    }
    let p = (v["p"].as_u64().unwrap_or(0) as usize).min(ranges.len() - 1);
    Some(Selection::new(ranges, p).clamp(len))
}

/// Stack (last = nearest) → list of changes going back from now.
fn chain(current: &Rope, stack: &[(Rope, Selection)]) -> Vec<Value> {
    let mut prev = current;
    let mut out = Vec::new();
    for (text, sel) in stack.iter().rev().take(KEEP_STEPS) {
        let c = minimal_change(prev, text).unwrap_or(Change { from: 0, to: 0, insert: String::new() });
        out.push(json!({ "from": c.from, "to": c.to, "insert": c.insert, "sel": sel_json(sel) }));
        prev = text;
    }
    out
}

/// History file content. None if the history is empty (the file is deleted).
pub fn encode(
    path: &Path,
    current: &Rope,
    undo: &[(Rope, Selection)],
    redo: &[(Rope, Selection)],
) -> Option<String> {
    if undo.is_empty() && redo.is_empty() {
        return None;
    }
    let v = json!({
        "version": 1,
        "path": path.to_string_lossy(),
        "hash": text_hash(current),
        "len": current.len_bytes(),
        "undo": chain(current, undo),
        "redo": chain(current, redo),
    });
    Some(v.to_string())
}

/// Restored stacks (stack order — last = nearest).
pub type Stacks = (Vec<(Rope, Selection)>, Vec<(Rope, Selection)>);

/// Decode a history file against the current text. None if the fingerprint differs or it's corrupt.
pub fn decode(body: &str, current: &Rope) -> Option<Stacks> {
    let v: Value = serde_json::from_str(body).ok()?;
    if v["version"].as_u64() != Some(1)
        || v["len"].as_u64()? as usize != current.len_bytes()
        || v["hash"].as_str()? != text_hash(current)
    {
        return None;
    }
    let unchain = |steps: &Value| -> Option<Vec<(Rope, Selection)>> {
        let mut text = current.clone();
        let mut out = Vec::new();
        for s in steps.as_array()? {
            let (from, to) = (s["from"].as_u64()? as usize, s["to"].as_u64()? as usize);
            // A corrupt file pointing mid-char would make ropey panic — filter it out first
            if from > to || to > text.len_bytes() || is_cont(&text, from) || is_cont(&text, to) {
                return None;
            }
            let tx = Transaction::new(vec![Change { from, to, insert: s["insert"].as_str()?.to_string() }]);
            tx.apply(&mut text, false);
            out.push((text.clone(), sel_from(&s["sel"], text.len_bytes())?));
        }
        out.reverse();
        Some(out)
    };
    Some((unchain(&v["undo"])?, unchain(&v["redo"])?))
}

/// Delete history files, least recently touched first, down to `KEEP_FILES`.
fn prune(dir: &Path) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut files: Vec<(std::time::SystemTime, PathBuf)> = rd
        .filter_map(|e| e.ok())
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .collect();
    if files.len() <= KEEP_FILES {
        return;
    }
    files.sort();
    for (_, p) in files.iter().take(files.len() - KEEP_FILES) {
        let _ = std::fs::remove_file(p);
    }
}

impl Editor {
    /// Right after saving: record this document's history (worker thread).
    pub fn undo_persist(&mut self, id: DocId) {
        if !self.config.persistent_undo || cfg!(test) {
            return;
        }
        let Some(doc) = self.docs.iter().find(|d| d.id == id) else { return };
        let Some(path) = doc.path.clone() else { return };
        if doc.is_modified() || doc.text.len_bytes() > MAX_FILE_BYTES {
            return;
        }
        let Some(dir) = dir() else { return };
        let text = doc.text.clone();
        let (undo, redo) = doc.history();
        self.events.jobs().spawn(move || {
            let file = file_for(&dir, &path);
            match encode(&path, &text, &undo, &redo) {
                Some(body) => {
                    let _ = std::fs::create_dir_all(&dir);
                    // No reading half-written files (a late old write that wins just mismatches the
                    // fingerprint and gets dropped — no corruption).
                    let _ = crate::disk::write_atomic(&file, |w| w.write_all(body.as_bytes()));
                    prune(&dir);
                }
                None => {
                    let _ = std::fs::remove_file(&file);
                }
            }
            |_: &mut Editor| {}
        });
    }

    /// Right after opening (fully reading) a file: restore if a history file exists and the fingerprint
    /// matches (decoded on a worker thread; dropped if edited in the meantime).
    pub fn undo_restore(&mut self, id: DocId) {
        if !self.config.persistent_undo || cfg!(test) {
            return;
        }
        let Some(doc) = self.docs.iter().find(|d| d.id == id) else { return };
        let (Some(path), Some(dir)) = (doc.path.clone(), dir()) else { return };
        if doc.loading || doc.has_history() || doc.text.len_bytes() > MAX_FILE_BYTES {
            return;
        }
        let (text, version) = (doc.text.clone(), doc.version());
        self.events.jobs().spawn(move || {
            let stacks = std::fs::read_to_string(file_for(&dir, &path)).ok().and_then(|b| decode(&b, &text));
            move |ed: &mut Editor| {
                let Some((undo, redo)) = stacks else { return };
                let Some(doc) = ed.docs.iter_mut().find(|d| d.id == id) else { return };
                if doc.version() != version || doc.has_history() {
                    return; // edited while restoring — the history no longer lines up
                }
                doc.install_history((undo, redo));
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn st(s: &str, head: usize) -> (Rope, Selection) {
        (Rope::from_str(s), Selection::point(head))
    }

    #[test]
    fn round_trip_restores_every_state() {
        // History: "" → "a" → "ab" → "a타b" (now), simulating one redo left: redo from now gives "a타bc"
        let undo = vec![st("", 0), st("a", 1), st("ab", 2)];
        let redo = vec![st("a타bc", 6)];
        let now = Rope::from_str("a타b");
        let body = encode(Path::new("/x.rs"), &now, &undo, &redo).unwrap();
        let (u, r) = decode(&body, &now).unwrap();
        let texts = |v: &[(Rope, Selection)]| v.iter().map(|(t, _)| t.to_string()).collect::<Vec<_>>();
        assert_eq!(texts(&u), ["", "a", "ab"]);
        assert_eq!(texts(&r), ["a타bc"]);
        assert_eq!(u[1].1.primary().head, 1);
        assert_eq!(r[0].1.primary().head, 6);
        // Dropped if the file changed outside
        assert!(decode(&body, &Rope::from_str("a타B")).is_none());
        assert!(decode("{broken", &now).is_none());
        assert!(encode(Path::new("/x.rs"), &now, &[], &[]).is_none(), "no history, no file");
    }

    #[test]
    fn document_undo_after_install() {
        let mut doc = crate::document::Document::from_str(1, "abc");
        doc.install_history((vec![st("", 0), st("ab", 2)], vec![]));
        assert!(!doc.is_modified());
        assert!(doc.undo());
        assert_eq!(doc.text.to_string(), "ab");
        assert!(doc.is_modified());
        assert!(doc.undo());
        assert_eq!(doc.text.to_string(), "");
        assert!(doc.redo() && doc.redo());
        assert_eq!(doc.text.to_string(), "abc");
        assert!(!doc.is_modified(), "clean when back at the saved state");
    }
}
