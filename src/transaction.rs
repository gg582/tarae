//! Transaction — a list of non-overlapping changes (positions in bytes). All multi-selection edits go
//! through this and apply at once; selection positions move to the new document via `map_pos`.

use ropey::Rope;
use tree_sitter::InputEdit;

use crate::selection::{Range, Selection};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Change {
    pub from: usize,
    pub to: usize,
    pub insert: String,
}

impl Change {
    pub fn insert(pos: usize, text: impl Into<String>) -> Self {
        Self { from: pos, to: pos, insert: text.into() }
    }

    pub fn delete(from: usize, to: usize) -> Self {
        Self { from, to, insert: String::new() }
    }
}

/// When a position meets an insertion exactly at it: stay in front (Before) or get pushed after (After).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Assoc {
    Before,
    After,
}

/// Record of one applied change — used by the syntax tree (`ts`) and LSP didChange (line/col + new text).
/// Columns are `ts` byte columns = LSP UTF-8 columns; `u16` is for UTF-16 servers (start, old end).
#[derive(Clone, Debug)]
pub struct Edit {
    pub ts: InputEdit,
    pub insert: String,
    pub u16: Option<(usize, usize)>,
}

fn u16_col(text: &Rope, byte: usize) -> usize {
    let ls = text.line_to_byte(text.byte_to_line(byte));
    text.byte_slice(ls..byte).chars().map(char::len_utf16).sum()
}

#[derive(Clone, Debug, Default)]
pub struct Transaction {
    changes: Vec<Change>,
}

impl Transaction {
    pub fn new(mut changes: Vec<Change>) -> Self {
        changes.sort_by_key(|c| (c.from, c.to));
        let mut out: Vec<Change> = Vec::with_capacity(changes.len());
        for c in changes {
            // Overlapping changes are dropped — can't occur normally with normalized selections.
            if out.last().is_some_and(|l| c.from < l.to) {
                continue;
            }
            out.push(c);
        }
        Self { changes: out }
    }

    pub fn change_by_selection(sel: &Selection, f: impl FnMut(Range) -> Option<Change>) -> Self {
        Self::new(sel.ranges().iter().copied().filter_map(f).collect())
    }

    pub fn is_empty(&self) -> bool {
        self.changes.is_empty()
    }

    /// Apply and return the edit records (each relative to the text just before it — applied back to
    /// front so earlier positions don't shift; feed them in this order to the syntax tree or LSP).
    /// `want_u16`: also count UTF-16 columns, only with a UTF-16 server attached (costs a line scan).
    pub fn apply(&self, text: &mut Rope, want_u16: bool) -> Vec<Edit> {
        // Applying back to front keeps earlier indices stable.
        // ropey 1's edit API uses char indices — the only byte↔char conversion in the whole editor.
        let mut edits = Vec::with_capacity(self.changes.len());
        for c in self.changes.iter().rev() {
            edits.push(Edit {
                ts: crate::syntax::input_edit(text, c.from, c.to, &c.insert),
                insert: c.insert.clone(),
                u16: want_u16.then(|| (u16_col(text, c.from), u16_col(text, c.to))),
            });
            let from = text.byte_to_char(c.from);
            if c.from < c.to {
                text.remove(from..text.byte_to_char(c.to));
            }
            if !c.insert.is_empty() {
                text.insert(from, &c.insert);
            }
        }
        edits
    }

    pub fn map_pos(&self, pos: usize, assoc: Assoc) -> usize {
        let mut delta: isize = 0;
        for c in &self.changes {
            let ins = c.insert.len() as isize;
            let del = (c.to - c.from) as isize;
            let before_pos = c.to < pos || (c.to == pos && (c.from < c.to || assoc == Assoc::After));
            if before_pos {
                delta += ins - del;
            } else if c.from < pos {
                // pos inside the deleted range — collapses to the range start.
                let base = (c.from as isize + delta) as usize;
                return if assoc == Assoc::After { base + ins as usize } else { base };
            } else {
                break;
            }
        }
        (pos as isize + delta) as usize
    }

    pub fn map_selection(&self, sel: &Selection) -> Selection {
        sel.transform(|r| {
            Range::new(self.map_pos(r.anchor, Assoc::After), self.map_pos(r.head, Assoc::After))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_multiple_changes() {
        let mut rope = Rope::from_str("hello world");
        let tx =
            Transaction::new(vec![Change::insert(0, ">"), Change::delete(5, 6), Change::insert(11, "!")]);
        tx.apply(&mut rope, false);
        assert_eq!(rope.to_string(), ">helloworld!");
    }

    #[test]
    fn map_pos_through_insert_and_delete() {
        let tx = Transaction::new(vec![Change::insert(2, "abc"), Change::delete(5, 8)]);
        assert_eq!(tx.map_pos(1, Assoc::After), 1);
        assert_eq!(tx.map_pos(2, Assoc::Before), 2);
        assert_eq!(tx.map_pos(2, Assoc::After), 5);
        assert_eq!(tx.map_pos(4, Assoc::After), 7);
        // Inside the deleted range → range start (shifted position)
        assert_eq!(tx.map_pos(6, Assoc::After), 8);
        assert_eq!(tx.map_pos(8, Assoc::After), 8);
        assert_eq!(tx.map_pos(10, Assoc::After), 10);
    }

    #[test]
    fn overlapping_changes_are_dropped() {
        let tx = Transaction::new(vec![Change::delete(0, 4), Change::delete(2, 6)]);
        assert_eq!(tx.changes.len(), 1);
    }
}
