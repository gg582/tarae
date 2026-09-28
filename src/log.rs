//! tarae's log — `$XDG_STATE_HOME/tarae/tarae.log` (`:log-open`). What language servers print on stderr, when
//! they start, get ready, and exit, what they report (`window/logMessage`, `showMessage`), and every
//! message the editor shows. Callable from any thread: a line goes over a channel to a writer thread,
//! so nothing waits on the disk. The file is started fresh each run (the previous one is kept as `.old`).
//! Tests write nothing.

use std::io::Write as _;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::sync::mpsc::{Sender, channel};

pub fn path() -> Option<PathBuf> {
    crate::config::state_dir().map(|d| d.join("tarae.log"))
}

fn sender() -> Option<&'static Sender<String>> {
    static TX: OnceLock<Option<Sender<String>>> = OnceLock::new();
    TX.get_or_init(|| {
        if cfg!(test) {
            return None;
        }
        let path = path()?;
        let (tx, rx) = channel::<String>();
        std::thread::spawn(move || {
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::rename(&path, path.with_extension("log.old"));
            let Ok(mut file) = std::fs::File::create(&path) else { return };
            let (y, mo, d, h, m, sec) = now();
            let _ = writeln!(
                file,
                "tarae {} · started {y}-{mo:02}-{d:02} {h:02}:{m:02}:{sec:02}\n",
                env!("CARGO_PKG_VERSION")
            );
            while let Ok(line) = rx.recv() {
                let _ = file.write_all(line.as_bytes());
                // Whatever else arrived meanwhile goes in the same write, then flush once
                while let Ok(more) = rx.try_recv() {
                    let _ = file.write_all(more.as_bytes());
                }
                let _ = file.flush();
            }
        });
        Some(tx)
    })
    .as_ref()
}

/// Local (year, month, day, hour, minute, second).
fn now() -> (i32, i32, i32, i32, i32, i32) {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let t = secs as libc::time_t;
    // SAFETY: localtime_r only writes the `tm` we own; a zeroed `tm` is a valid value to start from.
    let tm = unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&t, &mut tm);
        tm
    };
    (tm.tm_year + 1900, tm.tm_mon + 1, tm.tm_mday, tm.tm_hour, tm.tm_min, tm.tm_sec)
}

/// One line: `12:03:04  rust-analyzer  error  message` (multi-line text is indented under it).
pub fn line(source: &str, level: &str, text: &str) {
    let Some(tx) = sender() else { return };
    let (.., h, m, sec) = now();
    let text = text.trim_end().replace('\n', &format!("\n{:36}", ""));
    let _ = tx.send(format!("{h:02}:{m:02}:{sec:02}  {source:<16} {level:<8} {text}\n"));
}
