//! Jump list — spots to come back to (`C-o` back · `C-i` forward · `C-s` save this spot). One per pane,
//! as in helix. An entry names a mark kept by its document (`Document::marks`), so it follows edits.

use std::collections::VecDeque;

use crate::document::DocId;
use crate::editor::Editor;

/// Oldest entries drop off past this many.
const CAPACITY: usize = 30;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Jump {
    pub doc: DocId,
    pub mark: u64,
}

/// `current` = where `C-o` steps back from; `len` means "past the newest entry" (not travelling).
#[derive(Clone, Debug, Default)]
pub struct JumpList {
    jumps: VecDeque<Jump>,
    current: usize,
}

impl JumpList {
    /// Drop what's ahead of the current point — a new jump starts a new branch (as in a browser).
    pub fn truncate(&mut self) {
        self.jumps.truncate(self.current);
    }

    pub fn last(&self) -> Option<Jump> {
        self.jumps.back().copied()
    }

    pub fn push(&mut self, jump: Jump) {
        self.truncate();
        if self.jumps.len() >= CAPACITY {
            self.jumps.pop_front();
        }
        self.jumps.push_back(jump);
        self.current = self.jumps.len();
    }

    pub fn current(&self) -> usize {
        self.current
    }

    pub fn at_end(&self) -> bool {
        self.current >= self.jumps.len()
    }

    pub fn get(&self, i: usize) -> Option<Jump> {
        self.jumps.get(i).copied()
    }

    /// Move the current point to entry `i` and return it.
    pub fn go(&mut self, i: usize) -> Option<Jump> {
        let j = self.get(i)?;
        self.current = i;
        Some(j)
    }

    pub fn len(&self) -> usize {
        self.jumps.len()
    }

    pub fn iter(&self) -> impl ExactSizeIterator<Item = &Jump> + DoubleEndedIterator {
        self.jumps.iter()
    }

    /// A closed document's entries go; the current point stays on the same entry.
    pub fn remove_doc(&mut self, doc: DocId) {
        let before = self.jumps.iter().take(self.current).filter(|j| j.doc == doc).count();
        self.jumps.retain(|j| j.doc != doc);
        self.current = (self.current - before).min(self.jumps.len());
    }
}

// ── On the editor (the focused pane's list) ─────────────────────────────────

impl Editor {
    fn focused_jumps(&mut self) -> Option<&mut JumpList> {
        let focus = self.focus;
        self.views.iter_mut().find(|v| v.id == focus).map(|v| &mut v.jumps)
    }

    /// Whether jump `j` is exactly where the cursor is now.
    fn is_here(&self, j: Jump) -> bool {
        let doc = self.doc();
        j.doc == doc.id && doc.mark_raw(j.mark) == Some(doc.selection())
    }

    /// Remember the current spot before moving away (go-to, search, `C-s`).
    pub fn push_jump(&mut self) {
        let focus = self.focus;
        let Some(v) = self.views.iter().position(|v| v.id == focus) else { return };
        self.record(v, self.doc().id);
    }

    /// Document `doc`'s current selection onto pane `v`'s list — not twice in a row.
    fn record(&mut self, v: usize, doc: DocId) {
        let Some(d) = self.docs.iter_mut().find(|d| d.id == doc) else { return };
        let list = &mut self.views[v].jumps;
        list.truncate();
        if list.last().is_some_and(|j| j.doc == doc && d.mark_raw(j.mark) == Some(d.selection())) {
            return;
        }
        let mark = d.add_mark(d.selection().clone());
        list.push(Jump { doc, mark });
        self.prune_marks();
    }

    /// Documents keep only the marks some pane's list still points at.
    fn prune_marks(&mut self) {
        let live: std::collections::HashSet<Jump> =
            self.views.iter().flat_map(|v| v.jumps.iter().copied()).collect();
        for d in &mut self.docs {
            let id = d.id;
            d.retain_marks(|mark| live.contains(&Jump { doc: id, mark }));
        }
    }

    /// The document changed under the focused pane (opening a file, a picker, `:b` …) — the spot left
    /// behind becomes a jump, and `ga` remembers the document.
    pub fn track_doc_switch(&mut self) {
        let now = self.doc().id;
        let focus = self.focus;
        let Some(i) = self.views.iter().position(|v| v.id == focus) else { return };
        let old = self.views[i].doc;
        if old == now {
            return;
        }
        (self.views[i].doc, self.views[i].last_doc) = (now, Some(old));
        self.record(i, old);
    }

    /// `C-o` — `n` spots back. Leaving the newest spot records it first, so `C-i` returns there.
    pub fn jump_backward(&mut self, n: usize) {
        self.track_doc_switch();
        let Some(list) = self.focused_jumps() else { return };
        let Some(mut target) = list.current().checked_sub(n) else { return self.note("no earlier jump") };
        if list.at_end() {
            self.push_jump();
        }
        // An entry that is exactly here would look like nothing happened — skip over it
        let list = self.focused_jumps().cloned().unwrap_or_default();
        while target > 0 && list.get(target).is_some_and(|j| self.is_here(j)) {
            target -= 1;
        }
        self.goto_jump(target);
    }

    /// `C-i` / Tab — `n` spots forward again (after `C-o`).
    pub fn jump_forward(&mut self, n: usize) {
        let Some(list) = self.focused_jumps() else { return };
        let target = list.current() + n;
        if target >= list.len() {
            return self.note("no later jump");
        }
        self.goto_jump(target);
    }

    fn goto_jump(&mut self, i: usize) {
        let Some(j) = self.focused_jumps().and_then(|l| l.go(i)) else { return };
        let Some(di) = self.docs.iter().position(|d| d.id == j.doc) else { return };
        let Some(sel) = self.docs[di].mark(j.mark) else { return };
        let old = self.doc().id;
        self.current = di;
        self.doc_mut().set_selection(sel);
        // Switching documents here is travel, not a new jump
        let focus = self.focus;
        if let Some(v) = self.views.iter_mut().find(|v| v.id == focus)
            && v.doc != j.doc
        {
            (v.doc, v.last_doc) = (j.doc, Some(old));
        }
    }

    /// Picked in the jump list: travel there (the current spot is kept first, like `C-o` does).
    pub fn jump_to_entry(&mut self, i: usize) {
        if self.focused_jumps().is_some_and(|l| l.at_end()) {
            self.push_jump();
        }
        self.goto_jump(i);
    }

    /// `space j` — this pane's jumps, newest first: `file:line` and the line's text.
    pub fn jumplist_picker(&mut self) {
        let Some(list) = self.focused_jumps().cloned() else { return };
        let items: Vec<crate::picker::Item> = list
            .iter()
            .enumerate()
            .rev()
            .filter_map(|(index, j)| {
                let doc = self.docs.iter().find(|d| d.id == j.doc)?;
                let sel = doc.mark(j.mark)?;
                let line = doc.text.byte_to_line(sel.primary().cursor(&doc.text));
                let text: String = doc.text.line(line).chars().take(200).collect();
                Some(crate::picker::Item {
                    label: format!("{}:{}", doc.display_name(), line + 1),
                    action: crate::picker::Action::Jump { index, doc: j.doc, line },
                    hint: text.trim().to_string(),
                    glyph: None,
                })
            })
            .collect();
        if items.is_empty() {
            return self.note("no jumps yet (go-tos, searches, gg/ge and file switches add them)");
        }
        self.open_picker(crate::picker::Picker::new("jumps", items, true), None);
    }

    /// `ga` — back to the document shown before this one in this pane.
    pub fn goto_last_accessed(&mut self) {
        let focus = self.focus;
        let last = self.views.iter().find(|v| v.id == focus).and_then(|v| v.last_doc);
        match last.and_then(|d| self.docs.iter().position(|x| x.id == d)) {
            Some(i) => self.current = i,
            None => self.note("no other file yet"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn j(doc: DocId, mark: u64) -> Jump {
        Jump { doc, mark }
    }

    #[test]
    fn push_truncates_the_forward_branch() {
        let mut l = JumpList::default();
        l.push(j(1, 1));
        l.push(j(1, 2));
        l.push(j(1, 3));
        // Standing on entry 1 (came back to it): a new jump replaces it and everything after
        assert_eq!(l.go(1), Some(j(1, 2)));
        l.push(j(2, 4));
        assert_eq!(l.iter().copied().collect::<Vec<_>>(), [j(1, 1), j(2, 4)]);
        assert!(l.at_end());
    }

    #[test]
    fn capacity_drops_the_oldest() {
        let mut l = JumpList::default();
        for m in 0..40 {
            l.push(j(1, m));
        }
        assert_eq!(l.len(), CAPACITY);
        assert_eq!(l.get(0), Some(j(1, 10)));
        assert_eq!(l.current(), CAPACITY);
    }

    #[test]
    fn removing_a_doc_keeps_the_current_entry() {
        let mut l = JumpList::default();
        for (d, m) in [(1, 1), (2, 2), (1, 3), (2, 4)] {
            l.push(j(d, m));
        }
        l.go(2);
        l.remove_doc(2);
        assert_eq!(l.iter().copied().collect::<Vec<_>>(), [j(1, 1), j(1, 3)]);
        assert_eq!(l.get(l.current()), Some(j(1, 3)));
        l.remove_doc(1);
        assert_eq!((l.len(), l.current()), (0, 0));
    }
}
