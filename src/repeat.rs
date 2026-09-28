//! `.` — repeat the last insert (helix's `repeat_last_insert`): the keys' commands that entered insert mode
//! (`c`, `o`, `A`, a user binding …) run again at the current selections, then what was typed is fed in
//! again. An accepted completion is kept as the text it inserted (the list won't be there on replay).

use crate::editor::{Editor, Mode};
use crate::key::Key;
use crate::keymap::MappableCommand;
use crate::transaction::{Assoc, Change, Transaction};

#[derive(Clone, Debug)]
pub struct LastInsert {
    cmds: Vec<MappableCommand>,
    count: Option<usize>,
    events: Vec<InsertEvent>,
}

#[derive(Clone, Debug)]
enum InsertEvent {
    Key(Key),
    /// `replace` bytes before the cursor became `text`, cursor `cursor` bytes into it.
    Completion {
        replace: usize,
        text: String,
        cursor: usize,
    },
}

impl Editor {
    /// `cmds` just took us into insert mode — start recording (not while `.` itself replays).
    pub(crate) fn insert_started(&mut self, cmds: &[MappableCommand], count: Option<usize>) {
        if !self.repeating_insert {
            self.insert_rec = Some(LastInsert { cmds: cmds.to_vec(), count, events: Vec::new() });
        }
    }

    /// A key reaching insert mode.
    pub(crate) fn insert_key(&mut self, key: Key) {
        if self.mode == Mode::Insert
            && let Some(rec) = &mut self.insert_rec
        {
            rec.events.push(InsertEvent::Key(key));
        }
    }

    pub(crate) fn insert_completion(&mut self, replace: usize, text: String, cursor: usize) {
        if let Some(rec) = &mut self.insert_rec {
            rec.events.push(InsertEvent::Completion { replace, text, cursor });
        }
    }

    /// Insert mode was left — the recording becomes `.`'s. The last `trigger` keys are what left it.
    pub(crate) fn insert_finished(&mut self, trigger: usize) {
        if self.mode == Mode::Insert {
            return;
        }
        if let Some(mut rec) = self.insert_rec.take() {
            let keep = rec.events.len().saturating_sub(trigger);
            rec.events.truncate(keep);
            self.last_insert = Some(rec);
        }
    }

    pub fn repeat_last_insert(&mut self, times: usize) {
        let Some(last) = self.last_insert.clone() else { return self.note("nothing to repeat yet") };
        self.repeating_insert = true;
        for _ in 0..times {
            self.run(&last.cmds, last.count);
            if self.mode != Mode::Insert {
                break;
            }
            for ev in &last.events {
                match ev {
                    InsertEvent::Key(k) => self.handle_key(*k),
                    InsertEvent::Completion { replace, text, cursor } => {
                        self.replay_completion(*replace, text, *cursor)
                    }
                }
            }
            self.completion = None;
            if self.mode == Mode::Insert
                && let Some(c) = crate::commands::find("normal_mode")
            {
                self.run(&[MappableCommand::Static(c)], None);
            }
        }
        self.repeating_insert = false;
    }

    fn replay_completion(&mut self, replace: usize, text: &str, cursor: usize) {
        self.with_group(|cx| {
            let doc = cx.editor.doc_mut();
            let snap = |p| crate::graphemes::snap(&doc.text, p);
            let heads: Vec<usize> = doc.selection().ranges().iter().map(|r| r.head).collect();
            let tx = Transaction::new(
                heads
                    .iter()
                    .map(|&h| Change {
                        from: snap(h.saturating_sub(replace)),
                        to: h,
                        insert: text.to_string(),
                    })
                    .collect(),
            );
            let sel = doc.selection().transform(|r| {
                let from = tx.map_pos(snap(r.head.saturating_sub(replace)), Assoc::Before);
                crate::selection::Range::point(from + cursor)
            });
            doc.apply_with(&tx, sel);
        });
    }
}
