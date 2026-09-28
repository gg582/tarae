//! Session restore — started without arguments, reopen the files last open in this folder at their spots
//! (cursor, scroll); opening a file by name goes to where you last were in it.
//! `$XDG_STATE_HOME/tarae/session.json` (default `~/.local/state/tarae/session.json`):
//! `{ "positions": { path: [cursor, top line] }, "sessions": { dir: { "files": [...], "current": n } } }`.
//! Read once at startup, written once on quit (a small file).

use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use crate::editor::Editor;
use crate::graphemes;
use crate::selection::Selection;

/// Number of file positions remembered (oldest dropped first).
const KEEP_POSITIONS: usize = 500;

fn file() -> Option<PathBuf> {
    Some(crate::config::state_dir()?.join("session.json"))
}

#[derive(Default)]
pub struct State {
    root: Map<String, Value>,
}

impl State {
    pub fn load() -> State {
        let root = file()
            .and_then(|f| std::fs::read_to_string(f).ok())
            .and_then(|s| serde_json::from_str::<Value>(&s).ok())
            .and_then(|v| v.as_object().cloned())
            .unwrap_or_default();
        State { root }
    }

    #[cfg(test)]
    pub fn from_json(v: Value) -> State {
        State { root: v.as_object().cloned().unwrap_or_default() }
    }

    /// A file's last position (cursor byte, top line).
    pub fn position(&self, path: &Path) -> Option<(usize, usize)> {
        let p = self.root.get("positions")?.get(path.to_str()?)?.as_array()?;
        Some((p.first()?.as_u64()? as usize, p.get(1)?.as_u64()? as usize))
    }

    /// This folder's last session: (files, index being viewed).
    pub fn session(&self, cwd: &Path) -> Option<(Vec<PathBuf>, usize)> {
        let s = self.root.get("sessions")?.get(cwd.to_str()?)?;
        let files: Vec<PathBuf> =
            s.get("files")?.as_array()?.iter().filter_map(|f| f.as_str().map(PathBuf::from)).collect();
        Some((files, s.get("current").and_then(Value::as_u64).unwrap_or(0) as usize))
    }
}

impl Editor {
    /// Right after opening a document: go to the remembered spot if any (clamped to the end / a cluster
    /// boundary if the file changed meanwhile).
    pub fn restore_position(&mut self, idx: usize) {
        let Some(path) = self.docs.get(idx).and_then(|d| d.path.clone()) else { return };
        let Some((head, top)) = self.session.position(&path) else { return };
        let doc = &mut self.docs[idx];
        if doc.loading {
            return;
        }
        let head = graphemes::snap(&doc.text, head);
        doc.set_selection(Selection::point(head));
        doc.top = top.min(doc.text.len_lines().saturating_sub(1));
    }

    /// Started without arguments: this folder's last session (existing files only). Number of files opened.
    pub fn restore_session(&mut self) -> usize {
        let Ok(cwd) = std::env::current_dir() else { return 0 };
        let Some((files, current)) = self.session.session(&cwd) else { return 0 };
        let mut opened = 0;
        // Saved `current` indexes the saved list — missing files shift it. Take the file itself, or the
        // nearest opened one before it (else the first).
        let mut pick = None;
        for (i, f) in files.iter().enumerate() {
            if !f.is_file() || self.open(f).is_err() {
                continue;
            }
            opened += 1;
            if i <= current || pick.is_none() {
                pick = Some(self.current);
            }
        }
        if let Some(pick) = pick {
            self.current = pick;
            let name = if opened == 1 { "1 file".to_string() } else { format!("{opened} files") };
            self.set_status(format!("Restored {name} from your last session"));
        }
        opened
    }

    /// On quit: record open files' positions and this folder's session.
    pub fn save_session(&mut self) {
        let Some(f) = file() else { return };
        let root = &mut self.session.root;
        // Sequence: each entry is [cursor, top line, seq] — on overflow, drop the smallest (oldest) seq first
        let mut seq = root.get("seq").and_then(Value::as_u64).unwrap_or(0);
        let positions = root.entry("positions").or_insert_with(|| json!({}));
        if let Some(map) = positions.as_object_mut() {
            for d in &self.docs {
                let Some(p) = d.path.as_ref().and_then(|p| p.to_str()) else { continue };
                seq += 1;
                map.insert(p.to_string(), json!([d.selection().primary().cursor(&d.text), d.top, seq]));
            }
            if map.len() > KEEP_POSITIONS {
                let mut by_age: Vec<(u64, String)> = map
                    .iter()
                    .map(|(k, v)| (v.get(2).and_then(Value::as_u64).unwrap_or(0), k.clone()))
                    .collect();
                by_age.sort();
                for (_, k) in by_age.into_iter().take(map.len() - KEEP_POSITIONS) {
                    map.remove(&k);
                }
            }
        }
        root.insert("seq".into(), json!(seq));
        if let Ok(cwd) = std::env::current_dir()
            && let Some(cwd) = cwd.to_str()
        {
            let files: Vec<&str> = self.docs.iter().filter_map(|d| d.path.as_ref()?.to_str()).collect();
            let sessions = root.entry("sessions").or_insert_with(|| json!({}));
            if let Some(map) = sessions.as_object_mut() {
                if files.is_empty() {
                    map.remove(cwd);
                } else {
                    map.insert(cwd.to_string(), json!({ "files": files, "current": self.current }));
                }
            }
        }
        if let Some(dir) = f.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let body = Value::Object(root.clone()).to_string();
        let _ = crate::disk::write_atomic(&f, |w| w.write_all(body.as_bytes()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_positions_and_sessions() {
        let s = State {
            root: json!({
                "positions": { "/a.rs": [12, 3] },
                "sessions": { "/proj": { "files": ["/a.rs", "/b.rs"], "current": 1 } }
            })
            .as_object()
            .cloned()
            .unwrap(),
        };
        assert_eq!(s.position(Path::new("/a.rs")), Some((12, 3)));
        assert_eq!(s.position(Path::new("/nope")), None);
        let (files, cur) = s.session(Path::new("/proj")).unwrap();
        assert_eq!((files.len(), cur), (2, 1));
    }

    #[test]
    fn restore_skips_missing_files_and_snaps_to_clusters() {
        let dir = std::env::temp_dir().join(format!("tarae-session-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (a, c) = (dir.join("a.txt"), dir.join("c.txt"));
        std::fs::write(&a, "a\n").unwrap();
        std::fs::write(&c, "e\u{301}x\n").unwrap();
        let (a, c) = (std::fs::canonicalize(a).unwrap(), std::fs::canonicalize(c).unwrap());
        let cwd = std::env::current_dir().unwrap();
        let mut ed = Editor::new(crate::config::Config::default());
        ed.session = State::from_json(json!({
            "positions": { c.to_str().unwrap(): [1, 0] }, // on the combining mark
            "sessions": { cwd.to_str().unwrap(): {
                "files": [dir.join("gone.txt").to_str().unwrap(), a.to_str().unwrap(), c.to_str().unwrap()],
                "current": 1,
            } },
        }));
        assert_eq!(ed.restore_session(), 2);
        assert_eq!(ed.doc().path.as_deref(), Some(a.as_path()), "saved current = a, not shifted by gone.txt");
        let doc_c = ed.docs.iter().find(|d| d.path.as_deref() == Some(c.as_path())).unwrap();
        assert_eq!(doc_c.selection().primary().head, 0, "cluster start");
        std::fs::remove_dir_all(&dir).ok();
    }
}
