//! Recently opened files — shown on the welcome screen, opened directly with number keys.
//! `$XDG_STATE_HOME/tarae/recent` (default `~/.local/state/tarae/recent`), one path per line, newest on top.
//! Read once at startup (only when started without a file); written on a worker thread (disk never
//! blocks input).

use std::io::Write as _;
use std::path::{Path, PathBuf};

const KEEP: usize = 20;

fn file() -> Option<PathBuf> {
    Some(crate::config::state_dir()?.join("recent"))
}

/// Only files that still exist.
pub fn load() -> Vec<PathBuf> {
    let Some(f) = file() else { return Vec::new() };
    std::fs::read_to_string(f)
        .unwrap_or_default()
        .lines()
        .map(PathBuf::from)
        .filter(|p| p.is_file())
        .take(KEEP)
        .collect()
}

/// Returns the list with this one moved to the top (saving via `save` — the caller runs it off-thread).
pub fn bump(list: &[PathBuf], path: &Path) -> Vec<PathBuf> {
    let mut out = vec![path.to_path_buf()];
    out.extend(list.iter().filter(|p| p.as_path() != path).take(KEEP - 1).cloned());
    out
}

pub fn save(list: &[PathBuf]) {
    let Some(f) = file() else { return };
    if let Some(dir) = f.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let body: String = list.iter().map(|p| format!("{}\n", p.display())).collect();
    let _ = crate::disk::write_atomic(&f, |w| w.write_all(body.as_bytes()));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bump_moves_to_front_without_duplicates() {
        let l = vec![PathBuf::from("/a"), PathBuf::from("/b"), PathBuf::from("/c")];
        let out = bump(&l, Path::new("/b"));
        assert_eq!(out, [PathBuf::from("/b"), PathBuf::from("/a"), PathBuf::from("/c")]);
    }
}
