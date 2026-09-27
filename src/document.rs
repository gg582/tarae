//! Document = text (rope) + selection + path + undo history.
//!
//! Undo is snapshot-based. Cloning a ropey rope is O(1) structural sharing, so pushing the whole
//! (text, selection, version) per edit group is cheap. Only when writing to disk (to survive restarts)
//! are neighboring snapshots turned into diffs (`undofile.rs`).

use std::fs::File;
use std::io::{self, BufReader};
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};
use ropey::Rope;

use crate::lsp::{ClientId, Diagnostic};
use crate::selection::Selection;
use crate::syntax::Syntax;
use crate::transaction::{Assoc, Edit, Transaction};

/// LSP-side state of a document.
#[derive(Default)]
pub struct DocLsp {
    pub client: Option<ClientId>,
    /// Changes not yet sent (as didChange once the server is ready / at the end of an event batch).
    pub changes: Vec<Edit>,
    /// Send the full text next time instead of changes (after undo etc.).
    pub full_sync: bool,
    /// LSP document version — only increases, even on undo (separate from the editor version).
    pub version: i32,
    /// The attached server uses UTF-16 columns.
    pub u16: bool,
    pub diagnostics: Vec<Diagnostic>,
    /// Inlay hints (by position) + the (doc version, line range) they're for — covers the visible range
    /// and the version matches → don't ask again.
    pub inlay: Vec<crate::lsp::InlayHint>,
    pub inlay_have: Option<(u64, (usize, usize))>,
}

pub type DocId = u64;

/// File → rope. A missing file gives an empty rope (created on save).
pub fn read_file(path: &Path) -> Result<Rope> {
    match File::open(path) {
        Ok(f) => Rope::from_reader(BufReader::new(f)).with_context(|| format!("reading {}", path.display())),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Rope::new()),
        Err(e) => Err(e).with_context(|| format!("opening {}", path.display())),
    }
}

#[derive(Clone)]
pub struct Snapshot {
    text: Rope,
    selection: Selection,
    version: u64,
    /// Syntax tree matching that text (O(1) copy) — so highlights aren't empty after undo until reparsed.
    tree: Option<crate::syntax::Trees>,
}

pub struct Document {
    pub id: DocId,
    pub text: Rope,
    pub path: Option<PathBuf>,
    /// View scroll (line, screen column) — kept when switching buffers.
    pub top: usize,
    pub left: usize,
    /// Loading a big file in the background — text still empty. No saving (would overwrite with nothing).
    pub loading: bool,
    /// Syntax tree and language (None if the language is unknown).
    pub syntax: Option<Syntax>,
    /// Attached language servers, pending changes, diagnostics.
    pub lsp: DocLsp,
    selection: Selection,
    /// Version of the current text. A new edit always gets a new number; undo restores an old one
    /// — so `version == saved_version` means exactly "matches disk".
    version: u64,
    saved_version: u64,
    next_version: u64,
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    /// Fingerprint of the disk file last read or written — detects changes made elsewhere (disk.rs).
    pub disk: Option<crate::disk::Stamp>,
    /// Disk changed too while I was editing (resolve with `:reload` / `:w!`).
    pub disk_conflict: bool,
    /// This file at HEAD (None outside a repo) · changed lines against it · version that diff is for.
    pub git_base: Option<std::sync::Arc<String>>,
    pub git_hunks: Vec<crate::git::Hunk>,
    pub git_version: u64,
    /// Display name of an unnamed buffer (tutorial etc.) · disposable (quit isn't blocked as "unsaved").
    pub title: Option<String>,
    pub throwaway: bool,
    /// Address of non-file content (jdtls jdt:// class source) — reopening uses this buffer. Read-only.
    pub virtual_uri: Option<String>,
}

impl Document {
    pub fn from_str(id: DocId, text: &str) -> Self {
        Self {
            id,
            text: Rope::from_str(text),
            path: None,
            top: 0,
            left: 0,
            loading: false,
            syntax: None,
            lsp: DocLsp::default(),
            selection: Selection::point(0),
            version: 0,
            saved_version: 0,
            next_version: 0,
            undo: Vec::new(),
            redo: Vec::new(),
            disk: None,
            disk_conflict: false,
            git_base: None,
            git_hunks: Vec::new(),
            git_version: u64::MAX,
            title: None,
            throwaway: false,
            virtual_uri: None,
        }
    }

    /// A missing file gives an empty document at that path (created on save).
    pub fn open(id: DocId, path: &Path) -> Result<Self> {
        let mut doc = Self::placeholder(id, path);
        doc.disk = crate::disk::stamp(path);
        doc.text = read_file(path)?;
        doc.loading = false;
        Ok(doc)
    }

    /// Mark the current content as matching disk (after reloading).
    pub fn mark_saved(&mut self) {
        self.saved_version = self.version;
    }

    /// Empty placeholder for background loading — `finish_loading` fills it.
    pub fn placeholder(id: DocId, path: &Path) -> Self {
        let mut doc = Self::from_str(id, "");
        doc.path = Some(path.to_path_buf());
        doc.loading = true;
        doc
    }

    /// `disk` is set by the loader to the stamp taken *before* reading — a change made during the read
    /// then shows up on the next watch round.
    pub fn finish_loading(&mut self, text: Rope) {
        self.text = text;
        self.loading = false;
        if let Some(s) = &mut self.syntax {
            s.reset(None);
        }
        let sel = self.selection.clone();
        self.set_selection(sel);
    }

    pub fn selection(&self) -> &Selection {
        &self.selection
    }

    pub fn set_selection(&mut self, sel: Selection) {
        self.selection = sel.clamp(self.text.len_bytes());
    }

    /// Change the text and move selections along with the changes.
    pub fn apply(&mut self, tx: &Transaction) {
        let sel = tx.map_selection(&self.selection);
        self.apply_with(tx, sel);
    }

    /// Change the text; the selection is whatever the caller decided.
    pub fn apply_with(&mut self, tx: &Transaction, sel: Selection) {
        if tx.is_empty() {
            self.set_selection(sel);
            return;
        }
        let attached = self.lsp.client.is_some();
        let edits = tx.apply(&mut self.text, attached && self.lsp.u16);
        if let Some(s) = &mut self.syntax {
            s.edit(&edits.iter().map(|e| e.ts).collect::<Vec<_>>());
        }
        if attached {
            self.lsp.changes.extend(edits);
        }
        // Diagnostic ranges follow edits too — stay in place until the server sends new ones.
        for d in &mut self.lsp.diagnostics {
            d.from = tx.map_pos(d.from, Assoc::Before);
            d.to = tx.map_pos(d.to, Assoc::Before).max(d.from);
        }
        // Hints stick to the following character (typing before it pushes them along)
        for h in &mut self.lsp.inlay {
            h.pos = tx.map_pos(h.pos, Assoc::After);
        }
        self.set_selection(sel);
        self.next_version += 1;
        self.version = self.next_version;
    }

    pub fn version(&self) -> u64 {
        self.version
    }

    /// [error, warning] counts — for the status line.
    pub fn diagnostic_counts(&self) -> [usize; 2] {
        let n = |s: u8| self.lsp.diagnostics.iter().filter(|d| d.severity == s).count();
        [n(1), n(2)]
    }

    pub fn is_modified(&self) -> bool {
        self.version != self.saved_version
    }

    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            text: self.text.clone(),
            selection: self.selection.clone(),
            version: self.version,
            tree: self.syntax.as_ref().and_then(|s| s.trees()),
        }
    }

    /// Close the edit group — if now differs from the group-start snapshot, push onto the undo stack.
    pub fn commit_undo(&mut self, before: Snapshot) {
        if before.version != self.version {
            self.undo.push(before);
            self.redo.clear();
        }
    }

    pub fn undo(&mut self) -> bool {
        let Some(snap) = self.undo.pop() else { return false };
        let cur = self.snapshot();
        self.redo.push(cur);
        self.restore(snap);
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(snap) = self.redo.pop() else { return false };
        let cur = self.snapshot();
        self.undo.push(cur);
        self.restore(snap);
        true
    }

    /// Persistent undo: (text, selection) of (undo stack, redo stack) — in stack order (last = nearest).
    pub fn history(&self) -> crate::undofile::Stacks {
        let f = |v: &Vec<Snapshot>| v.iter().map(|s| (s.text.clone(), s.selection.clone())).collect();
        (f(&self.undo), f(&self.redo))
    }

    /// Install restored history right after opening (current text = saved state). Versions renumbered.
    pub fn install_history(&mut self, (undo, redo): crate::undofile::Stacks) {
        let mut snap = |(text, selection): (Rope, Selection)| {
            self.next_version += 1;
            Snapshot { text, selection, version: self.next_version, tree: None }
        };
        let undo: Vec<Snapshot> = undo.into_iter().map(&mut snap).collect();
        let redo: Vec<Snapshot> = redo.into_iter().map(&mut snap).collect();
        (self.undo, self.redo) = (undo, redo);
    }

    pub fn has_history(&self) -> bool {
        !self.undo.is_empty() || !self.redo.is_empty()
    }

    fn restore(&mut self, snap: Snapshot) {
        self.text = snap.text;
        self.selection = snap.selection;
        self.version = snap.version;
        if let Some(s) = &mut self.syntax {
            s.reset(snap.tree);
        }
        // Text replaced wholesale → resend everything to the server.
        self.lsp.changes.clear();
        self.lsp.full_sync = true;
        self.lsp.diagnostics.clear();
        self.lsp.inlay.clear();
        self.lsp.inlay_have = None;
    }

    pub fn save(&mut self) -> Result<()> {
        self.save_as(false)
    }

    /// Unless `force`, don't overwrite a file changed on disk since read (don't clobber others' changes).
    pub fn save_as(&mut self, force: bool) -> Result<()> {
        if self.loading {
            bail!("still loading — not saved");
        }
        let Some(path) = &self.path else { bail!("no file name — use :w <path>") };
        let now = crate::disk::stamp(path);
        if !force && (self.disk_conflict || (self.disk.is_some() && now.is_some() && now != self.disk)) {
            self.disk_conflict = true;
            bail!(
                "{} changed on disk since it was read — :w! to overwrite, :reload to take it",
                self.display_name_short()
            );
        }
        crate::disk::write_atomic(path, |w| self.text.write_to(w))
            .with_context(|| format!("writing {}", path.display()))?;
        self.saved_version = self.version;
        self.disk = crate::disk::stamp(path);
        self.disk_conflict = false;
        Ok(())
    }

    pub fn display_name(&self) -> String {
        match &self.path {
            Some(p) => {
                let cwd = std::env::current_dir().ok();
                let rel = cwd.as_deref().and_then(|c| p.strip_prefix(c).ok()).unwrap_or(p);
                rel.display().to_string()
            }
            None => self.title.clone().unwrap_or_else(|| "[scratch]".to_string()),
        }
    }

    /// File name only (for header and buffer list).
    pub fn display_name_short(&self) -> String {
        match self.path.as_ref().and_then(|p| p.file_name()) {
            Some(n) => n.to_string_lossy().into_owned(),
            None => self.title.clone().unwrap_or_else(|| "[scratch]".to_string()),
        }
    }
}
