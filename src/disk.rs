//! Syncing with disk (like IntelliJ) — notice when a file changes elsewhere (git checkout, formatter, agent):
//! - If I changed nothing, **quietly reload**. No wholesale swap — only the changed range, as a transaction —
//!   cursor, scroll and undo stay (`u` even goes back to before the disk change).
//! - If I changed something, it's a conflict — notification + header mark, `:reload` (theirs) / `:w!` (mine).
//! - Auto-save: when the terminal loses focus (`editor.auto-save = "focus"`, default); with `"idle"`, also
//!   after 2 s without input. Files in conflict are never auto-saved.
//!
//! Watching = stat every 0.5 s on a worker thread (tens of µs for a few files — the main loop never waits
//! on disk), plus once immediately on regaining focus.

use std::fs::{File, OpenOptions};
use std::io::{self, BufWriter};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use ropey::Rope;

use crate::document::DocId;
use crate::editor::Editor;
use crate::transaction::{Change, Transaction};

/// Fingerprint of a disk file: (mtime, size).
pub type Stamp = (SystemTime, u64);

const POLL: Duration = Duration::from_millis(500);
const IDLE_SAVE: Duration = Duration::from_secs(2);
/// Files larger than this aren't reloaded automatically (notification only).
const RELOAD_MAX: u64 = 16 << 20;

pub fn stamp(path: &Path) -> Option<Stamp> {
    let m = std::fs::metadata(path).ok()?;
    Some((m.modified().ok()?, m.len()))
}

/// Byte `i` is in the middle of a UTF-8 char.
pub fn is_cont(text: &Rope, i: usize) -> bool {
    i < text.len_bytes() && text.byte(i) & 0xC0 == 0x80
}

/// The smallest single change turning old text into new (middle minus common prefix/suffix, on char
/// boundaries). None if equal.
pub fn minimal_change(old: &Rope, new: &Rope) -> Option<Change> {
    let (ol, nl) = (old.len_bytes(), new.len_bytes());
    let mut pre = old.bytes().zip(new.bytes()).take_while(|(a, b)| a == b).count();
    if pre == ol && pre == nl {
        return None;
    }
    while pre > 0 && (is_cont(old, pre) || is_cont(new, pre)) {
        pre -= 1;
    }
    let max_suf = (ol - pre).min(nl - pre);
    let (mut a, mut b) = (old.bytes_at(ol), new.bytes_at(nl));
    let mut suf = 0;
    while suf < max_suf && a.prev().is_some_and(|x| b.prev() == Some(x)) {
        suf += 1;
    }
    while suf > 0 && (is_cont(old, ol - suf) || is_cont(new, nl - suf)) {
        suf -= 1;
    }
    Some(Change { from: pre, to: ol - suf, insert: new.byte_slice(pre..nl - suf).to_string() })
}

/// Replace `path` without a window where it's half-written: temp file in the same directory (permissions
/// of the old file) → fsync → rename. Symlinks are followed (the target is replaced, the link stays).
/// If the directory isn't writable, falls back to writing in place.
pub fn write_atomic(
    path: &Path,
    fill: impl FnOnce(&mut BufWriter<File>) -> io::Result<()>,
) -> io::Result<()> {
    let mut target = path.to_path_buf();
    for _ in 0..40 {
        let Ok(link) = std::fs::read_link(&target) else { break };
        target = match target.parent() {
            Some(dir) => dir.join(link),
            None => link,
        };
    }
    let dir = target.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let name = target.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    // Numbered so back-to-back writes of one file (worker threads) don't collide
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp = dir.join(format!(".{name}.tarae-{}-{n}.tmp", std::process::id()));
    let f = match OpenOptions::new().write(true).create_new(true).open(&tmp) {
        Ok(f) => f,
        Err(_) => {
            let mut w = BufWriter::new(File::create(&target)?);
            fill(&mut w)?;
            return w.into_inner().map_err(|e| e.into_error())?.sync_all();
        }
    };
    let result = (|| {
        if let Ok(m) = std::fs::metadata(&target) {
            f.set_permissions(m.permissions())?;
        }
        let mut w = BufWriter::new(f);
        fill(&mut w)?;
        w.into_inner().map_err(|e| e.into_error())?.sync_all()?;
        std::fs::rename(&tmp, &target)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// What the worker thread returns: document, fingerprint it compared against, current fingerprint, content
/// (if changed and worth reading).
type Stat = (DocId, Option<Stamp>, Option<Stamp>, Option<String>);

impl Editor {
    /// Start watching (once at startup — it keeps itself going after that).
    pub fn disk_watch(&mut self) {
        self.disk_poll(POLL);
    }

    fn disk_poll(&mut self, delay: Duration) {
        if self.disk_polling {
            return;
        }
        self.disk_polling = true;
        let files: Vec<(DocId, PathBuf, Option<Stamp>)> = self
            .docs
            .iter()
            .filter(|d| !d.loading)
            .filter_map(|d| Some((d.id, d.path.clone()?, d.disk)))
            .collect();
        self.events.jobs().spawn(move || {
            std::thread::sleep(delay);
            let stats: Vec<Stat> = files
                .into_iter()
                .filter_map(|(id, path, known)| {
                    let now = stamp(&path);
                    if now == known {
                        return None;
                    }
                    let body = now
                        .filter(|(_, len)| *len <= RELOAD_MAX)
                        .and_then(|_| std::fs::read_to_string(&path).ok());
                    Some((id, known, now, body))
                })
                .collect();
            move |ed: &mut Editor| {
                ed.disk_polling = false;
                ed.on_disk(stats);
                ed.disk_poll(POLL);
            }
        });
    }

    fn on_disk(&mut self, stats: Vec<Stat>) {
        for (id, known, now, body) in stats {
            let Some(doc) = self.docs.iter_mut().find(|d| d.id == id) else { continue };
            // Saved (or reloaded) since the stat started — the change it saw may be our own write
            if doc.disk != known {
                continue;
            }
            let name = doc.display_name_short();
            // The log follows along like `tail -f`: reloaded without a toast, the cursor kept at the end
            let log = crate::log::is_log(doc.path.as_deref());
            match (now, body) {
                // Deleted (or unreadable) — notify only once
                (None, _) => {
                    if doc.disk.is_some() {
                        doc.disk = None;
                        self.set_warning(format!("{name} was deleted on disk — :w to write it back"));
                    }
                }
                (Some(stamp), Some(body)) if !doc.is_modified() => {
                    let before = doc.text.len_lines();
                    let head = doc.selection().primary().cursor(&doc.text);
                    let at_end = crate::movement::line_of(&doc.text, head) + 1
                        >= crate::movement::last_line(&doc.text);
                    let changed = reload_text(doc, &body);
                    doc.disk = Some(stamp);
                    doc.disk_conflict = false;
                    if log {
                        if changed && at_end {
                            let last = crate::movement::last_line(&doc.text);
                            let line = last.saturating_sub(usize::from(doc.text.line(last).len_chars() == 0));
                            let pos = crate::movement::line_start(&doc.text, line);
                            doc.set_selection(crate::selection::Selection::point(pos));
                        }
                    } else if changed {
                        let delta = doc.text.len_lines() as isize - before as isize;
                        let lines = match delta {
                            0 => String::new(),
                            d => format!(" · {}{} lines", if d > 0 { "+" } else { "" }, d),
                        };
                        self.set_status(format!("{name} reloaded from disk{lines}"));
                    }
                }
                // Written outside = my buffer (agent saving an edit I accepted, etc.) — counts as saved
                (Some(stamp), Some(body)) if doc.text == body.as_str() => {
                    doc.disk = Some(stamp);
                    doc.disk_conflict = false;
                    doc.mark_saved();
                }
                // Only the trailing newline is missing — Claude Code does that when writing accepted content
                // (measured). Write back the buffer (= accepted content)
                (Some(stamp), Some(body))
                    if doc.is_modified()
                        && doc.text.len_bytes() == body.len() + 1
                        && doc.text == format!("{body}\n").as_str() =>
                {
                    doc.disk = Some(stamp);
                    if let Err(e) = doc.save_as(true) {
                        self.set_error(format!("{name}: {e:#}"));
                    }
                }
                (Some(stamp), _) => {
                    doc.disk = Some(stamp);
                    if !doc.disk_conflict {
                        doc.disk_conflict = true;
                        self.set_warning(format!(
                            "{name} changed on disk while you were editing — :reload takes theirs, :w! keeps yours"
                        ));
                    }
                }
            }
        }
    }

    /// `:reload` — take the disk content (my changes are discarded but recoverable with undo).
    pub fn reload_current(&mut self) -> Result<(), String> {
        let doc = self.doc_mut();
        let path = doc.path.clone().ok_or("no file name")?;
        let body = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        reload_text(doc, &body);
        doc.disk = stamp(&path);
        doc.disk_conflict = false;
        let name = doc.display_name_short();
        self.set_status(format!("{name} reloaded from disk"));
        Ok(())
    }

    /// Terminal focus: auto-save on loss, check disk right away on regain.
    pub fn on_focus(&mut self, gained: bool) {
        if gained {
            // One more check, separate from the pending watch — so changes show the moment focus returns
            self.disk_polling = false;
            self.disk_poll(Duration::ZERO);
            self.git_reload_bases();
        } else if self.config.auto_save != "off" {
            self.auto_save("");
        }
    }

    /// When input stops (idle mode) — called on every event.
    pub fn auto_save_idle(&mut self) {
        if self.config.auto_save != "idle" || !self.docs.iter().any(|d| d.is_modified() && d.path.is_some()) {
            return;
        }
        self.idle_save_gen += 1;
        let generation = self.idle_save_gen;
        self.events.jobs().spawn(move || {
            std::thread::sleep(IDLE_SAVE);
            move |ed: &mut Editor| {
                if ed.idle_save_gen == generation {
                    ed.auto_save(" (idle)");
                }
            }
        });
    }

    /// Save all modified files (skipping conflicts, loading, and unnamed ones).
    pub(crate) fn auto_save(&mut self, why: &str) {
        let mut saved = Vec::new();
        let mut failed = Vec::new();
        for i in 0..self.docs.len() {
            let d = &mut self.docs[i];
            if !d.is_modified() || d.path.is_none() || d.loading || d.disk_conflict {
                continue;
            }
            match d.save() {
                Ok(()) => saved.push((d.id, d.display_name_short())),
                Err(e) => failed.push(format!("{}: {e:#}", d.display_name_short())),
            }
        }
        for (id, _) in &saved {
            self.lsp_did_save(*id);
            self.undo_persist(*id);
        }
        if !failed.is_empty() {
            self.set_error(format!("auto-save failed — {}", failed.join("; ")));
        }
        match saved.as_slice() {
            [] => {}
            [(_, one)] => self.set_success(format!("Saved {one}{why}")),
            many => self.set_success(format!("Saved {} files{why}", many.len())),
        }
    }
}

/// Replace the document with new content — changed range only, marked as saved. true if it changed.
fn reload_text(doc: &mut crate::document::Document, body: &str) -> bool {
    let Some(change) = minimal_change(&doc.text, &Rope::from_str(body)) else {
        doc.mark_saved();
        return false;
    };
    let before = doc.snapshot();
    let tx = Transaction::new(vec![change]);
    let sel = tx.map_selection(doc.selection());
    doc.apply_with(&tx, sel);
    doc.commit_undo(before); // u = back to before the disk change
    doc.mark_saved();
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::config::Config;
    use crate::key::Key;

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
    }

    /// One watch round (no delay) — processes the resulting events.
    fn poll_once(ed: &mut Editor) {
        ed.disk_polling = false;
        ed.disk_poll(Duration::ZERO);
        let ev = ed.events.recv_timeout(Duration::from_secs(5)).expect("disk stat");
        ed.handle_event(ev);
    }

    /// The open log follows new lines quietly — a toast would be logged, change the file, and loop.
    #[test]
    fn open_log_follows_without_toasts() {
        let Some(log) = crate::log::path() else { return };
        let dir = std::env::temp_dir().join(format!("tarae-logfollow-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("t.log");
        std::fs::write(&file, "a\nb\n").unwrap();
        let mut ed = Editor::new(Config::default());
        ed.open(&file).unwrap();
        let id = ed.doc().id;
        feed(&mut ed, "ge");
        ed.doc_mut().path = Some(log); // stands in for the log (tests never write the real one)
        std::fs::write(&file, "a\nb\nc\nd\n").unwrap();
        let known = ed.doc().disk;
        ed.on_disk(vec![(id, known, stamp(&file), Some("a\nb\nc\nd\n".into()))]);
        assert_eq!(ed.doc().text.to_string(), "a\nb\nc\nd\n");
        assert!(ed.status.is_none() && ed.toasts.is_empty(), "no toast");
        let head = ed.doc().selection().primary().head;
        assert_eq!(ed.doc().text.byte_to_line(head), 3, "followed to the new last line");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn external_changes_reload_conflict_and_autosave() {
        let dir = std::env::temp_dir().join(format!("tarae-disk-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.txt");
        std::fs::write(&path, "a\nb\nc\n").unwrap();
        let mut ed = Editor::new(Config::default());
        ed.open(&path).unwrap();
        feed(&mut ed, "jj"); // cursor = 'c'
        // A line added at the top from outside → nothing changed here, so reload; cursor follows 'c'
        std::fs::write(&path, "x\na\nb\nc\n").unwrap();
        poll_once(&mut ed);
        assert_eq!(ed.doc().text.to_string(), "x\na\nb\nc\n");
        assert_eq!(ed.doc().selection().primary().cursor(&ed.doc().text), 6);
        assert!(!ed.doc().is_modified(), "matches disk");
        assert!(ed.toasts.iter().any(|t| t.text.contains("reloaded")));
        // u = back to before the disk change (now differs from disk)
        feed(&mut ed, "u");
        assert_eq!(ed.doc().text.to_string(), "a\nb\nc\n");
        assert!(ed.doc().is_modified());
        // Changed here and disk changed too → conflict (don't overwrite)
        std::fs::write(&path, "theirs\n").unwrap();
        poll_once(&mut ed);
        assert!(ed.doc().disk_conflict);
        assert_eq!(ed.doc().text.to_string(), "a\nb\nc\n", "my text untouched");
        feed(&mut ed, ":w<ret>");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "theirs\n", ":w refused during conflict");
        feed(&mut ed, ":reload<ret>");
        assert_eq!(ed.doc().text.to_string(), "theirs\n");
        assert!(!ed.doc().disk_conflict && !ed.doc().is_modified());
        // Losing focus auto-saves
        feed(&mut ed, "ggimine <esc>");
        ed.on_focus(false);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "mine theirs\n");
        assert!(!ed.doc().is_modified());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Agent writing the same content as my buffer is "saved", not a conflict — if only the trailing
    /// newline is missing, write it back.
    #[test]
    fn agent_writes_matching_buffer_are_saves() {
        let dir = std::env::temp_dir().join(format!("tarae-disk-agent-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.txt");
        std::fs::write(&path, "one\n").unwrap();
        let mut ed = Editor::new(Config::default());
        ed.open(&path).unwrap();
        feed(&mut ed, "wcTWOO<esc>"); // length must change so the fingerprint (size, mtime) surely differs
        assert!(ed.doc().is_modified());
        assert_eq!(ed.doc().text.to_string(), "TWOO\n");
        std::fs::write(&path, "TWOO\n").unwrap();
        poll_once(&mut ed);
        assert!(!ed.doc().is_modified() && !ed.doc().disk_conflict, "same content = saved");
        feed(&mut ed, "ggwcTHREEE<esc>");
        std::fs::write(&path, "THREEE").unwrap(); // without trailing newline
        poll_once(&mut ed);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "THREEE\n", "newline written back");
        assert!(!ed.doc().is_modified() && !ed.doc().disk_conflict);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A watch round that started before `:w` must not report our own write as a conflict.
    #[test]
    fn own_save_during_watch_is_not_a_conflict() {
        let dir = std::env::temp_dir().join(format!("tarae-disk-own-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.txt");
        std::fs::write(&path, "one\n").unwrap();
        let mut ed = Editor::new(Config::default());
        ed.open(&path).unwrap();
        ed.disk_polling = false;
        ed.disk_poll(Duration::from_millis(300)); // captured the old stamp; stats after the save below
        feed(&mut ed, "cTWO<esc>:w<ret>iX<esc>");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "TWOne\n");
        // Handle whatever arrives for a while (the round re-arms itself, so don't wait for it to stop)
        let until = std::time::Instant::now() + Duration::from_millis(700);
        while let Some(left) = until.checked_duration_since(std::time::Instant::now()) {
            if let Some(ev) = ed.events.recv_timeout(left) {
                ed.handle_event(ev);
            }
        }
        assert!(!ed.doc().disk_conflict, "own save seen as an outside change");
        feed(&mut ed, ":w<ret>");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), ed.doc().text.to_string(), ":w not refused");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn write_atomic_follows_symlinks_and_keeps_permissions() {
        use std::io::Write as _;
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("tarae-disk-atomic-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (real, link) = (dir.join("real.sh"), dir.join("link.sh"));
        std::fs::write(&real, "old").unwrap();
        std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o755)).unwrap();
        let _ = std::fs::remove_file(&link);
        std::os::unix::fs::symlink("real.sh", &link).unwrap();
        write_atomic(&link, |w| w.write_all(b"new")).unwrap();
        assert!(std::fs::symlink_metadata(&link).unwrap().file_type().is_symlink(), "link kept");
        assert_eq!(std::fs::read_to_string(&real).unwrap(), "new");
        assert_eq!(std::fs::metadata(&real).unwrap().permissions().mode() & 0o777, 0o755);
        let names: Vec<_> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(names.len(), 2, "no temp file left: {names:?}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn minimal_change_keeps_common_ends() {
        let c = |a: &str, b: &str| minimal_change(&Rope::from_str(a), &Rope::from_str(b));
        let x = c("fn a() {\n    1\n}\n", "fn a() {\n    2\n}\n").unwrap();
        assert_eq!((x.from, x.to, x.insert.as_str()), (13, 14, "2"));
        assert!(c("same", "same").is_none());
        // Char boundary: "타래" → "타리" replaces the whole second char
        let x = c("타래", "타리").unwrap();
        assert_eq!((x.from, x.to, x.insert.as_str()), (3, 6, "리"));
        // Overlapping prefix/suffix (aaa → aa)
        let x = c("aaa", "aa").unwrap();
        assert_eq!((x.to - x.from, x.insert.as_str()), (1, ""));
        let x = c("", "new").unwrap();
        assert_eq!((x.from, x.to, x.insert.as_str()), (0, 0, "new"));
    }
}
