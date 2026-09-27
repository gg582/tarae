//! git branch — for the status line. Reads the `HEAD` file directly instead of spawning `git` (a few µs).
//! A watcher thread polls HEAD's mtime and notifies the main loop when it changes (checkout in lazygit etc.).

use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::thread;
use std::time::Duration;

use crate::editor::Editor;
use crate::event::Event;

/// The repo's HEAD file, walking up from `start`. Follows worktree/submodule `.git` files (`gitdir: …`) too.
pub fn find_head(start: &Path) -> Option<PathBuf> {
    let mut dir = start.to_path_buf();
    loop {
        let dot = dir.join(".git");
        if dot.is_dir() {
            return Some(dot.join("HEAD"));
        }
        if dot.is_file() {
            let s = std::fs::read_to_string(&dot).ok()?;
            let gitdir = s.lines().find_map(|l| l.strip_prefix("gitdir:"))?.trim();
            let gitdir =
                if Path::new(gitdir).is_absolute() { PathBuf::from(gitdir) } else { dir.join(gitdir) };
            return Some(gitdir.join("HEAD"));
        }
        if !dir.pop() {
            return None;
        }
    }
}

/// HEAD content → branch name, or the first 7 chars of the commit if HEAD is detached.
pub fn parse_head(content: &str) -> Option<String> {
    let s = content.trim();
    match s.strip_prefix("ref:") {
        Some(r) => Some(r.trim().trim_start_matches("refs/heads/").to_string()),
        None if s.len() >= 7 && s.bytes().all(|b| b.is_ascii_hexdigit()) => Some(s[..7].to_string()),
        None => None,
    }
}

pub fn read_branch(head: &Path) -> Option<String> {
    parse_head(&std::fs::read_to_string(head).ok()?)
}

/// Watcher thread — reads once at first, then whenever HEAD changes.
pub fn watch(tx: Sender<Event>) {
    thread::spawn(move || {
        let Some(head) = std::env::current_dir().ok().and_then(|d| find_head(&d)) else { return };
        let stamp = || std::fs::metadata(&head).and_then(|m| m.modified()).ok();
        let (mut last, mut first) = (None, true);
        loop {
            let now = stamp();
            if first || now != last {
                (last, first) = (now, false);
                let branch = read_branch(&head);
                let apply = move |ed: &mut Editor| ed.git_branch = branch;
                if tx.send(Event::Job(Box::new(apply))).is_err() {
                    break;
                }
            }
            thread::sleep(Duration::from_millis(500));
        }
    });
}

// ── Changed-line markers (bar next to line numbers) ──────────────────────────

/// Changed-line hunks against HEAD. `lines` = line range in the current document (empty for a deletion
/// — deleted before that line).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hunk {
    pub kind: HunkKind,
    pub lines: std::ops::Range<usize>,
    /// Lines missing on the HEAD side (status line −N).
    pub removed: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HunkKind {
    Added,
    Modified,
    Deleted,
}

/// HEAD content vs current content → hunks (histogram diff — same family as Helix and git).
pub fn hunks(base: &str, now: &str) -> Vec<Hunk> {
    use imara_diff::intern::InternedInput;
    use imara_diff::{Algorithm, diff};
    let input = InternedInput::new(base, now);
    let mut out = Vec::new();
    diff(Algorithm::Histogram, &input, |before: std::ops::Range<u32>, after: std::ops::Range<u32>| {
        let kind = match (before.is_empty(), after.is_empty()) {
            (true, _) => HunkKind::Added,
            (_, true) => HunkKind::Deleted,
            _ => HunkKind::Modified,
        };
        out.push(Hunk { kind, lines: after.start as usize..after.end as usize, removed: before.len() });
    });
    out
}

/// This file's HEAD content in the repo (None outside a repo or for a new file). Runs on a worker thread.
pub fn head_blob(path: &Path) -> Option<String> {
    let dir = path.parent()?;
    let name = path.file_name()?.to_str()?;
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["show", &format!("HEAD:./{name}")])
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    out.status.success().then(|| String::from_utf8(out.stdout).ok()).flatten()
}

impl Editor {
    /// (Re)load the document's HEAD content — on open and on regaining focus (may have committed outside).
    pub fn git_load_base(&mut self, id: crate::document::DocId) {
        if !self.git_auto {
            return;
        }
        let Some(path) = self.docs.iter().find(|d| d.id == id).and_then(|d| d.path.clone()) else { return };
        self.events.jobs().spawn(move || {
            let base = head_blob(&path).map(std::sync::Arc::new);
            move |ed: &mut Editor| {
                if let Some(d) = ed.docs.iter_mut().find(|d| d.id == id)
                    && d.git_base.as_deref() != base.as_deref()
                {
                    d.git_base = base;
                    d.git_version = u64::MAX; // re-diff
                    if d.git_base.is_none() {
                        d.git_hunks.clear();
                    }
                }
            }
        });
    }

    pub fn git_reload_bases(&mut self) {
        let ids: Vec<_> = self.docs.iter().filter(|d| d.path.is_some()).map(|d| d.id).collect();
        for id in ids {
            self.git_load_base(id);
        }
    }

    /// On every event: if the current document's diff is stale, redo it on a worker thread shortly after
    /// (once input stops).
    pub fn git_schedule(&mut self) {
        if self.git_diffing {
            return;
        }
        let doc = self.doc();
        let Some(base) = doc.git_base.clone() else { return };
        if doc.git_version == doc.version() || doc.loading {
            return;
        }
        let (id, version, text) = (doc.id, doc.version(), doc.text.clone());
        self.git_diffing = true;
        self.events.jobs().spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(if cfg!(test) { 0 } else { 80 }));
            let hunks = hunks(&base, &text.to_string());
            move |ed: &mut Editor| {
                ed.git_diffing = false;
                if let Some(d) = ed.docs.iter_mut().find(|d| d.id == id)
                    && d.version() == version
                {
                    d.git_hunks = hunks;
                    d.git_version = version;
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn head_contents() {
        assert_eq!(parse_head("ref: refs/heads/master\n").as_deref(), Some("master"));
        assert_eq!(parse_head("ref: refs/heads/feat/타래\n").as_deref(), Some("feat/타래"));
        assert_eq!(parse_head("0123456789abcdef0123\n").as_deref(), Some("0123456"));
        assert_eq!(parse_head("garbage"), None);
    }

    #[test]
    fn hunks_added_modified_deleted() {
        let base = "a\nb\nc\nd\n";
        let now = "a\nB\nc\nnew\n";
        let h = hunks(base, now);
        assert_eq!(h[0], Hunk { kind: HunkKind::Modified, lines: 1..2, removed: 1 });
        assert_eq!(h[1], Hunk { kind: HunkKind::Modified, lines: 3..4, removed: 1 });
        let h = hunks("a\nb\n", "a\n");
        assert_eq!(h, [Hunk { kind: HunkKind::Deleted, lines: 1..1, removed: 1 }]);
        let h = hunks("a\n", "a\nb\nc\n");
        assert_eq!(h, [Hunk { kind: HunkKind::Added, lines: 1..3, removed: 0 }]);
    }

    #[test]
    fn finds_this_repo() {
        let head = find_head(Path::new(env!("CARGO_MANIFEST_DIR")).join("src").as_path()).unwrap();
        assert!(read_branch(&head).is_some());
    }
}
