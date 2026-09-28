//! Selections through shell commands (helix): `|` replaces each selection with the command's output
//! (the selection is its input), `A-|` only feeds it in, `!`/`A-!` insert the output before/after each
//! selection, `$` keeps the selections the command succeeds on. Commands run on a worker thread; the
//! result applies only if the document hasn't changed meanwhile — one undo step.

use std::io::Write as _;
use std::process::{Command, Stdio};

use crate::document::DocId;
use crate::editor::Editor;
use crate::selection::{Range, Selection};
use crate::transaction::{Change, Transaction};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pipe {
    /// `|` — output replaces each selection.
    Replace,
    /// `A-|` — the selection goes in, nothing comes back.
    To,
    /// `!` / `A-!` — output (run once) inserted before / after each selection.
    Insert,
    Append,
    /// `$` — keep the selections the command exits 0 on.
    Keep,
}

impl Pipe {
    pub fn label(self) -> &'static str {
        match self {
            Pipe::Replace => "pipe:",
            Pipe::To => "pipe-to:",
            Pipe::Insert => "insert-output:",
            Pipe::Append => "append-output:",
            Pipe::Keep => "keep-pipe:",
        }
    }

    /// Whether the selection is the command's input.
    fn feeds(self) -> bool {
        matches!(self, Pipe::Replace | Pipe::To | Pipe::Keep)
    }
}

/// One run: exit success, stdout, the last line of stderr.
struct Run {
    ok: bool,
    out: String,
    err: String,
}

fn run(cmd: &str, input: Option<&str>) -> Result<Run, String> {
    let mut child = Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("sh: {e}"))?;
    // Feed stdin from another thread — a command that writes before reading all its input would
    // otherwise deadlock against us
    let feeder = input.zip(child.stdin.take()).map(|(s, mut stdin)| {
        let s = s.to_string();
        std::thread::spawn(move || {
            let _ = stdin.write_all(s.as_bytes());
        })
    });
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    if let Some(f) = feeder {
        let _ = f.join();
    }
    let err = String::from_utf8_lossy(&out.stderr);
    Ok(Run {
        ok: out.status.success(),
        out: String::from_utf8_lossy(&out.stdout).into_owned(),
        err: err.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or_default().to_string(),
    })
}

/// A selection that didn't end in a newline shouldn't gain one from the command (`echo`, `sort` …).
fn fit_output(input: &str, mut out: String) -> String {
    if !input.ends_with('\n') && out.ends_with('\n') {
        out.pop();
        if out.ends_with('\r') {
            out.pop();
        }
    }
    out
}

impl Editor {
    pub fn shell_pipe(&mut self, kind: Pipe, cmd: &str) {
        let cmd = cmd.trim().to_string();
        if cmd.is_empty() {
            return;
        }
        let doc = self.doc();
        let (id, version) = (doc.id, doc.version());
        let ranges: Vec<Range> = doc.selection().ranges().iter().map(|r| r.min_width_1(&doc.text)).collect();
        let inputs: Vec<String> =
            ranges.iter().map(|r| doc.text.byte_slice(r.from()..r.to()).to_string()).collect();
        // Quiet note, not a toast — most commands finish before it would be read, and the change shows
        self.note(format!("running {cmd}…"));
        self.events.jobs().spawn(move || {
            let runs: Result<Vec<Run>, String> = if kind.feeds() {
                inputs.iter().map(|s| run(&cmd, Some(s))).collect()
            } else {
                run(&cmd, None).map(|r| vec![r])
            };
            move |ed: &mut Editor| ed.shell_done(kind, &cmd, id, version, &ranges, &inputs, runs)
        });
    }

    #[allow(clippy::too_many_arguments)]
    fn shell_done(
        &mut self,
        kind: Pipe,
        cmd: &str,
        id: DocId,
        version: u64,
        ranges: &[Range],
        inputs: &[String],
        runs: Result<Vec<Run>, String>,
    ) {
        let runs = match runs {
            Ok(r) => r,
            Err(e) => return self.set_error(e),
        };
        if let Some(r) = runs.iter().find(|r| !r.ok && kind != Pipe::Keep) {
            let why = if r.err.is_empty() { "failed".to_string() } else { r.err.clone() };
            return self.set_error(format!("{cmd}: {why}"));
        }
        if kind == Pipe::To {
            return self.set_success(format!("{cmd} ✓"));
        }
        let Some(i) = self.docs.iter().position(|d| d.id == id) else { return };
        if self.docs[i].version() != version {
            return self.set_warning(format!("the file changed while {cmd} ran — nothing applied"));
        }
        if self.current != i {
            return self.set_warning(format!("{cmd} finished in another file — nothing applied"));
        }
        self.status = None;
        let primary = self.docs[i].selection().primary_index();
        if kind == Pipe::Keep {
            let kept: Vec<Range> = ranges.iter().zip(&runs).filter(|(_, r)| r.ok).map(|(g, _)| *g).collect();
            if kept.is_empty() {
                return self.set_error(format!("{cmd}: no selection passed"));
            }
            return self.docs[i].set_selection(Selection::new(kept, 0));
        }
        // (position, text) per selection; the new selection covers what went in
        let items: Vec<(usize, usize, String)> = ranges
            .iter()
            .enumerate()
            .map(|(k, r)| match kind {
                Pipe::Replace => (r.from(), r.to(), fit_output(&inputs[k], runs[k].out.clone())),
                Pipe::Insert => (r.from(), r.from(), runs[0].out.clone()),
                _ => (r.to(), r.to(), runs[0].out.clone()),
            })
            .collect();
        self.with_group(|cx| {
            let doc = cx.editor.doc_mut();
            let tx = Transaction::new(
                items.iter().map(|(a, b, t)| Change { from: *a, to: *b, insert: t.clone() }).collect(),
            );
            let sel = Selection::new(
                items
                    .iter()
                    .map(|(a, _, t)| {
                        let from = tx.map_pos(*a, crate::transaction::Assoc::Before);
                        Range::new(from, from + t.len())
                    })
                    .collect(),
                primary,
            );
            doc.apply_with(&tx, sel);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runs_with_input_and_reports_failure() {
        let r = run("tr a-z A-Z", Some("hi")).unwrap();
        assert!(r.ok);
        assert_eq!(r.out, "HI");
        let r = run("echo nope >&2; exit 3", None).unwrap();
        assert!(!r.ok);
        assert_eq!(r.err, "nope");
    }

    #[test]
    fn output_keeps_the_selections_line_ending() {
        assert_eq!(fit_output("abc", "cba\n".into()), "cba");
        assert_eq!(fit_output("abc\n", "cba\n".into()), "cba\n");
    }
}
