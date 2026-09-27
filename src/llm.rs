//! LLM — runs `claude -p` (or any CLI with the same stream-json protocol) as a subprocess.
//! No HTTP·SDK·API key inside the editor — uses the user's Claude Code login as is.
//!
//! Cold start is slow (measured 2026-09-27, Claude Code 2.1.280, a single "ok"):
//! default 8–10 s → minimal options (`--tools "" --strict-mcp-config --setting-sources ""`) 4.3 s
//! → **a process spawned early and waiting on stdin (stream-json): 2.5 s** (haiku 1.9 s).
//! So the spare process is spawned the moment the ask prompt opens (overlapping typing); a request only
//! writes the message. One process per request — so contexts don't mix.
//!
//! Flow: selections → one request (answer per selection in `<r i="N">` tags) → review (diff, y/n per change)
//! → applied as one transaction. The answer is received as it streams (`--include-partial-messages`) and
//! shown in a preview card. Cancel = kill the process.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{Sender, channel};
use std::time::{Duration, Instant};

use ropey::Rope;

use crate::document::DocId;
use crate::editor::Editor;
use crate::event::{Apply, Event};
use crate::key::{Code, Key};
use crate::selection::{Range, Selection};
use crate::transaction::{Assoc, Change, Transaction};

/// Speed options: no tools (pure transform), skip MCP·user settings, no session saving (keeps /resume clean).
const DEFAULT_ARGS: &[&str] = &[
    "-p",
    "--input-format",
    "stream-json",
    "--output-format",
    "stream-json",
    "--verbose",
    "--include-partial-messages",
    "--tools",
    "",
    "--no-session-persistence",
    "--strict-mcp-config",
    "--setting-sources",
    "",
];

#[derive(Clone, Debug)]
pub struct LlmConfig {
    pub command: String,
    pub args: Vec<String>,
    pub model: Option<String>,
    /// Lines of context sent before and after the selection.
    pub context_lines: usize,
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            command: "claude".into(),
            args: DEFAULT_ARGS.iter().map(|s| s.to_string()).collect(),
            model: None,
            context_lines: 20,
        }
    }
}

// ── Spare process ───────────────────────────────────────────────────────

/// A pre-spawned process for the next request (at most one).
#[derive(Default)]
pub struct Warm {
    child: Option<Child>,
}

impl Warm {
    /// Spawns one if there's no live spare. spawn itself returns at once (slow init happens in the child).
    pub fn ensure(&mut self, cfg: &LlmConfig) -> Result<(), String> {
        if let Some(c) = &mut self.child
            && matches!(c.try_wait(), Ok(None))
        {
            return Ok(());
        }
        self.child = Some(spawn(cfg, &[]).map_err(|e| format!("{}: {e}", cfg.command))?);
        Ok(())
    }

    fn take(&mut self, cfg: &LlmConfig) -> Result<Child, String> {
        self.ensure(cfg)?;
        Ok(self.child.take().expect("ensured"))
    }
}

impl Drop for Warm {
    fn drop(&mut self) {
        if let Some(c) = &mut self.child {
            let _ = c.kill();
        }
    }
}

pub fn spawn(cfg: &LlmConfig, extra: &[&str]) -> std::io::Result<Child> {
    let mut cmd = Command::new(&cfg.command);
    cmd.args(&cfg.args).args(extra);
    if let Some(m) = &cfg.model {
        cmd.args(["--model", m]);
    }
    cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()
}

/// One user message line (stream-json input).
pub fn user_message(content: &str) -> String {
    serde_json::json!({"type": "user", "message": {"role": "user", "content": content}}).to_string()
}

/// Control request that stops the running turn — the process and conversation memory remain.
pub fn interrupt_message() -> String {
    serde_json::json!({"type": "control_request", "request_id": "tarae-stop", "request": {"subtype": "interrupt"}})
        .to_string()
}

/// Force-kills the process (cancel) — the reader thread ends on EOF.
pub fn kill(pid: u32) {
    // SAFETY: kill only sends a signal to the pid (merely fails if the pid already exited).
    unsafe {
        libc::kill(pid as libc::pid_t, libc::SIGKILL);
    }
}

// ── Reading the streamed answer ─────────────────────────────────────────

/// What one stdout line means.
pub enum StreamEv {
    /// Thinking (thinking fragments·token estimate).
    Thinking,
    /// Answer text fragment (batched every 25 ms).
    Text(String),
    /// End of a turn — final text or error (not logged in·stopped etc.).
    Done(Result<String, String>),
    /// The process exited (for the chat panel — last stderr line).
    Exit(String),
}

/// Reader thread body: parses stdout line by line and sends closures built by `route` to the main loop.
/// With `once`, reaps the process after the first turn (ask). Otherwise runs until the process exits (chat).
/// stderr is drained separately (so a full pipe doesn't stall the child) — its last line is the error text.
pub fn pump(
    mut child: Child,
    once: bool,
    tx: Sender<Event>,
    route: impl Fn(StreamEv) -> Option<Apply> + Send + 'static,
) {
    std::thread::spawn(move || {
        // stderr's last line arrives when the pipe closes
        let (err_tx, err_rx) = channel::<String>();
        if let Some(err) = child.stderr.take() {
            std::thread::spawn(move || {
                let mut last = String::new();
                for line in BufReader::new(err).lines().map_while(Result::ok) {
                    if !line.trim().is_empty() {
                        last = line;
                    }
                }
                let _ = err_tx.send(last);
            });
        }
        let send = |ev: StreamEv| route(ev).is_none_or(|apply| tx.send(Event::Job(apply)).is_ok());
        let mut pending = String::new();
        let mut last_flush = Instant::now();
        let mut thinking_sent = false;
        let mut done = false;
        if let Some(out) = child.stdout.take() {
            for line in BufReader::new(out).lines().map_while(Result::ok) {
                let Ok(ev) = serde_json::from_str::<serde_json::Value>(&line) else { continue };
                match parse_line(&ev) {
                    Some(StreamEv::Text(t)) => {
                        pending.push_str(&t);
                        thinking_sent = false;
                        if last_flush.elapsed() >= Duration::from_millis(25) {
                            last_flush = Instant::now();
                            if !send(StreamEv::Text(std::mem::take(&mut pending))) {
                                break;
                            }
                        }
                    }
                    Some(StreamEv::Thinking) if !thinking_sent => {
                        thinking_sent = true;
                        send(StreamEv::Thinking);
                    }
                    Some(d @ StreamEv::Done(_)) => {
                        if !pending.is_empty() {
                            send(StreamEv::Text(std::mem::take(&mut pending)));
                        }
                        thinking_sent = false;
                        let ok = send(d);
                        if once || !ok {
                            done = true;
                            break;
                        }
                    }
                    _ => {}
                }
            }
        }
        if once {
            let _ = child.kill();
        }
        let _ = child.wait();
        // Let stderr drain after exit (else the real error is lost) — bounded, in case a grandchild holds it
        let err = err_rx.recv_timeout(Duration::from_millis(500)).unwrap_or_default();
        if once && !done {
            let msg = if err.is_empty() { "exited without a result".to_string() } else { err };
            send(StreamEv::Done(Err(msg)));
        } else if !once {
            send(StreamEv::Exit(err));
        }
    });
}

fn parse_line(ev: &serde_json::Value) -> Option<StreamEv> {
    match ev["type"].as_str()? {
        "stream_event" => {
            let delta = &ev["event"]["delta"];
            match delta["type"].as_str()? {
                "text_delta" => Some(StreamEv::Text(delta["text"].as_str()?.to_string())),
                "thinking_delta" => Some(StreamEv::Thinking),
                _ => None,
            }
        }
        "system" if ev["subtype"] == "thinking_tokens" => Some(StreamEv::Thinking),
        "result" => {
            let text = ev["result"].as_str().unwrap_or_default().to_string();
            let failed = ev["is_error"].as_bool() == Some(true)
                || ev["subtype"].as_str().is_some_and(|s| s.starts_with("error"));
            Some(StreamEv::Done(if failed {
                Err(if text.is_empty() { "stopped".into() } else { text })
            } else {
                Ok(text)
            }))
        }
        _ => None,
    }
}

// ── Prompt·response ─────────────────────────────────────────────────────

pub fn build_prompt(
    instruction: &str,
    file: &str,
    text: &Rope,
    ranges: &[(usize, usize)],
    ctx: usize,
) -> String {
    let mut p = String::from(
        "You are the editing engine inside a text editor. Rewrite each <selection> according to the \
         instruction.\nReply with ONLY the replacement text of every selection, each wrapped in \
         <r i=\"N\">...</r> in the same order — no explanations, no markdown fences, nothing else. \
         Keep the surrounding code style and indentation. If a selection needs no change, return it \
         unchanged.\n\n",
    );
    p.push_str(&format!("Instruction: {instruction}\nFile: {file}\n"));
    let last_line = text.len_lines().saturating_sub(1);
    for (i, &(from, to)) in ranges.iter().enumerate() {
        // Last line = that of the last selected byte (a line-wise selection ends at the next line's start)
        let (l0, l1) = (text.byte_to_line(from), text.byte_to_line(to.saturating_sub(1).max(from)));
        let c0 = text.line_to_byte(l0.saturating_sub(ctx));
        let c1 = text.line_to_byte((l1 + ctx + 1).min(last_line + 1));
        p.push_str(&format!(
            "\n<selection i=\"{}\" lines=\"{}-{}\">\n<before>{}</before>\n<text>{}</text>\n<after>{}</after>\n</selection>\n",
            i + 1,
            l0 + 1,
            l1 + 1,
            text.byte_slice(c0..from),
            text.byte_slice(from..to),
            text.byte_slice(to..c1.max(to)),
        ));
    }
    p
}

/// Extracts N `<r i="N">…</r>`. With a single selection, an untagged answer is accepted too.
pub fn parse_replies(out: &str, n: usize) -> Result<Vec<String>, String> {
    let mut replies = Vec::with_capacity(n);
    for i in 1..=n {
        let open = format!("<r i=\"{i}\">");
        let found = out.find(&open).and_then(|s| {
            let body = &out[s + open.len()..];
            body.find("</r>").map(|e| &body[..e])
        });
        match found {
            Some(body) => {
                let body = body.strip_prefix('\n').unwrap_or(body);
                replies.push(body.strip_suffix('\n').unwrap_or(body).to_string());
            }
            None if n == 1 => replies.push(strip_fences(out.trim_matches('\n')).to_string()),
            None => return Err(format!("reply {i}/{n} missing")),
        }
    }
    Ok(replies)
}

fn strip_fences(s: &str) -> &str {
    let Some(rest) = s.strip_prefix("```") else { return s };
    let rest = rest.split_once('\n').map_or("", |(_, r)| r);
    rest.strip_suffix("```").map(|r| r.strip_suffix('\n').unwrap_or(r)).unwrap_or(rest)
}

/// If the original ended with a newline (line-wise selection), end the answer with one; otherwise strip it.
pub fn match_trailing_newline(old: &str, new: String) -> String {
    match (old.ends_with('\n'), new.ends_with('\n')) {
        (true, false) => new + "\n",
        (false, true) => new.trim_end_matches('\n').to_string(),
        _ => new,
    }
}

/// Line-level diff → (' ' | '-' | '+', line).
pub fn line_diff<'a>(old: &'a str, new: &'a str) -> Vec<(char, &'a str)> {
    use imara_diff::intern::InternedInput;
    use imara_diff::{Algorithm, diff};
    // imara's `&str` tokens = `str::lines` (terminator excluded) — indexes line up
    let (a, b): (Vec<&str>, Vec<&str>) = (old.lines().collect(), new.lines().collect());
    let input = InternedInput::new(old, new);
    let mut out = Vec::with_capacity(a.len().max(b.len()));
    let mut next = 0; // first old line not yet emitted
    diff(Algorithm::Histogram, &input, |before: std::ops::Range<u32>, after: std::ops::Range<u32>| {
        let (bs, be) = (before.start as usize, before.end as usize);
        out.extend(a[next..bs].iter().map(|l| (' ', *l)));
        out.extend(a[bs..be].iter().map(|l| ('-', *l)));
        out.extend(b[after.start as usize..after.end as usize].iter().map(|l| ('+', *l)));
        next = be;
    });
    out.extend(a[next..].iter().map(|l| (' ', *l)));
    out
}

// ── Review state ────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    Undecided,
    Accept,
    Reject,
}

pub struct Hunk {
    pub from: usize,
    pub to: usize,
    pub old: String,
    pub new: String,
    pub decision: Decision,
    /// The original changed while waiting and can't be located — cannot apply.
    pub conflict: bool,
}

/// One row of the in-buffer review view.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RRow {
    /// This document line as is.
    Doc(usize),
    /// Change header (change number).
    Header(usize),
    /// Line inside a change: ' ' kept · '-' removed · '+' added, old line number (none for added lines).
    Diff { kind: char, text: String, old_no: Option<usize>, hunk: usize },
}

pub struct Review {
    pub generation: u64,
    pub doc_id: DocId,
    pub instruction: String,
    pub hunks: Vec<Hunk>,
    pub ready: bool,
    pub current: usize,
    pub scroll: usize,
    /// Whether the view follows the current change (off when scrolling directly with j/k).
    pub follow: bool,
    pub started: Instant,
    /// Streamed answer (preview before it's done) · whether thinking.
    pub streamed: String,
    pub thinking: bool,
    /// What to call when done (applied?) — an agent-sent diff is awaiting an answer (agent.rs).
    pub on_done: Option<OnDone>,
    /// Who proposed it (in the header) — "Claude" for ask·chat, "Claude Code" for the side-pane agent.
    pub by: &'static str,
}

/// Called when the review ends (applied?).
pub type OnDone = Box<dyn FnOnce(&mut Editor, bool)>;

/// Closes the open review as rejected (before another review takes its place — so the waiter gets an answer).
/// An ask still waiting for its answer is killed (its result would no longer find its review).
pub fn close_review(ed: &mut Editor) {
    let Some(mut rev) = ed.review.take() else { return };
    if !rev.ready
        && let Some(pid) = ed.llm_pid.take()
    {
        kill(pid);
    }
    if let Some(cb) = rev.on_done.take() {
        cb(ed, false);
    }
}

impl Review {
    /// Waiting for an answer (`ready: false`), proposed by Claude, nobody to call when done.
    fn new(generation: u64, doc_id: DocId, instruction: &str, hunks: Vec<Hunk>) -> Self {
        Review {
            generation,
            doc_id,
            instruction: instruction.to_string(),
            hunks,
            ready: false,
            current: 0,
            scroll: 0,
            follow: true,
            started: Instant::now(),
            streamed: String::new(),
            thinking: false,
            on_done: None,
            by: "Claude",
        }
    }

    fn next_undecided(&self) -> Option<usize> {
        let n = self.hunks.len();
        (1..=n)
            .map(|d| (self.current + d) % n)
            .find(|&i| self.hunks[i].decision == Decision::Undecided && !self.hunks[i].conflict)
    }

    /// Rows of the in-buffer review (like Cursor): shows the whole document, expanding only the changes.
    /// Undecided = both removed and added lines, accepted = new text only, rejected = original only.
    pub fn rows(&self, text: &Rope) -> Vec<RRow> {
        let last = text.len_lines().saturating_sub(1);
        let line_of = |b: usize| text.byte_to_line(b.min(text.len_bytes()));
        let mut order: Vec<usize> = (0..self.hunks.len()).collect();
        order.sort_by_key(|&i| self.hunks[i].from);
        let mut out = Vec::new();
        let mut next = 0; // first document line not yet emitted
        for i in order {
            let h = &self.hunks[i];
            let lf = line_of(h.from);
            let lt = if h.to > h.from { line_of(h.to - 1) } else { lf };
            out.extend((next..lf.max(next)).map(RRow::Doc));
            out.push(RRow::Header(i));
            next = next.max(lt + 1);
            if h.conflict {
                out.extend((lf..=lt).map(RRow::Doc));
                continue;
            }
            // Changes line-wise: attach same-line pieces around the selection so whole lines are compared
            let ls = text.line_to_byte(lf);
            let le = text.line_to_byte((lt + 1).min(last + 1)).max(h.to);
            let (pre, post) =
                (text.byte_slice(ls..h.from.max(ls)).to_string(), text.byte_slice(h.to..le).to_string());
            let old = format!("{pre}{}{post}", h.old);
            let new = format!("{pre}{}{post}", h.new);
            let mut old_no = lf + 1;
            for (kind, line) in line_diff(&old, &new) {
                let kind = match (h.decision, kind) {
                    (Decision::Accept, '-') | (Decision::Reject, '+') => {
                        if kind == '-' {
                            old_no += 1;
                        }
                        continue;
                    }
                    (Decision::Reject, '-') => ' ',
                    _ => kind,
                };
                let no = (kind != '+').then_some(old_no);
                if kind != '+' {
                    old_no += 1;
                }
                out.push(RRow::Diff { kind, text: line.to_string(), old_no: no, hunk: i });
            }
        }
        out.extend((next..=last).map(RRow::Doc));
        out
    }
}

// ── Commands ────────────────────────────────────────────────────────────

/// When the ask prompt opens or "ask" starts being typed — spawns the spare process (overlapping typing).
pub fn prewarm(ed: &mut Editor) {
    let cfg = ed.config.llm.clone();
    if let Err(e) = ed.llm_warm.ensure(&cfg) {
        ed.set_error(e);
    }
}

pub fn ask(ed: &mut Editor, instruction: &str) -> Result<(), String> {
    let instruction = instruction.trim();
    if instruction.is_empty() {
        return Err("usage: :ask <instruction>".into());
    }
    if ed.review.is_some() {
        return Err("an ask is already in progress (:ask-cancel)".into());
    }
    let cfg = ed.config.llm.clone();
    let doc = ed.doc();
    let ranges: Vec<(usize, usize)> = doc
        .selection()
        .ranges()
        .iter()
        .map(|r| {
            let r = r.min_width_1(&doc.text);
            (r.from(), r.to())
        })
        .collect();
    let prompt = build_prompt(instruction, &doc.display_name(), &doc.text, &ranges, cfg.context_lines);
    let hunks = ranges
        .iter()
        .map(|&(from, to)| Hunk {
            from,
            to,
            old: doc.text.byte_slice(from..to).to_string(),
            new: String::new(),
            decision: Decision::Undecided,
            conflict: false,
        })
        .collect::<Vec<_>>();
    let doc_id = doc.id;
    let mut child = ed.llm_warm.take(&cfg)?;
    // Write the message and close stdin — the process exits on its own after this one turn.
    // Before anything is installed: a failed write leaves no review waiting forever.
    let sent = child.stdin.take().ok_or_else(|| "claude: no stdin".to_string()).and_then(|mut stdin| {
        writeln!(stdin, "{}", user_message(&prompt))
            .and_then(|_| stdin.flush())
            .map_err(|e| format!("claude: {e}"))
    });
    if let Err(e) = sent {
        // Reaped off the main thread
        std::thread::spawn(move || {
            let _ = child.kill();
            let _ = child.wait();
        });
        return Err(e);
    }
    ed.llm_generation += 1;
    let generation = ed.llm_generation;
    let n = hunks.len();
    ed.review = Some(Review::new(generation, doc_id, instruction, hunks));
    ed.llm_pid = Some(child.id());
    pump(child, true, ed.events.sender(), move |ev| {
        Some(match ev {
            StreamEv::Thinking => Box::new(move |ed: &mut Editor| {
                if let Some(r) = ed.review.as_mut().filter(|r| r.generation == generation) {
                    r.thinking = true;
                }
            }),
            StreamEv::Text(t) => Box::new(move |ed: &mut Editor| {
                if let Some(r) = ed.review.as_mut().filter(|r| r.generation == generation) {
                    r.thinking = false;
                    r.streamed.push_str(&t);
                }
            }),
            StreamEv::Done(r) => {
                let result = r.and_then(|out| parse_replies(&out, n));
                Box::new(move |ed: &mut Editor| on_result(ed, generation, result))
            }
            StreamEv::Exit(_) => return None,
        })
    });
    Ok(())
}

/// Cancel: if a request is pending, kill the process (stops tokens too); if reviewing, reject all.
pub fn cancel(ed: &mut Editor) {
    if let Some(pid) = ed.llm_pid.take() {
        kill(pid);
    }
    if ed.review.is_some() {
        close_review(ed);
        ed.llm_generation += 1;
        ed.set_status("claude: cancelled");
    }
}

/// Opens a review directly with changes in hand (chat code apply, agent diffs — same y/n flow·one undo as
/// ask) + what to call when done (applied?).
pub fn review_with(
    ed: &mut Editor,
    doc_id: DocId,
    instruction: &str,
    changes: Vec<(usize, usize, String, String)>,
    on_done: Option<OnDone>,
) {
    close_review(ed);
    ed.llm_generation += 1;
    let hunks: Vec<Hunk> = changes
        .into_iter()
        .map(|(from, to, old, new)| Hunk {
            from,
            to,
            decision: if old == new { Decision::Reject } else { Decision::Undecided },
            old,
            new,
            conflict: false,
        })
        .collect();
    if hunks.iter().all(|h| h.decision == Decision::Reject) {
        if let Some(cb) = on_done {
            cb(ed, false);
        }
        return ed.set_status("claude: no changes");
    }
    ed.review =
        Some(Review { ready: true, on_done, ..Review::new(ed.llm_generation, doc_id, instruction, hunks) });
}

/// Preview of the streaming answer: the last few lines inside the `<r i="N">` being written.
pub fn preview(streamed: &str, rows: usize) -> Vec<&str> {
    let body = match streamed.rfind("<r i=\"") {
        Some(i) => {
            let rest = &streamed[i..];
            rest.find('>').map_or("", |g| &rest[g + 1..])
        }
        None => streamed,
    };
    let body = body.split("</r>").next().unwrap_or_default().trim_start_matches('\n');
    let lines: Vec<&str> = body.lines().collect();
    lines[lines.len().saturating_sub(rows)..].to_vec()
}

fn on_result(ed: &mut Editor, generation: u64, result: Result<Vec<String>, String>) {
    let Some(rev) = ed.review.as_mut().filter(|r| r.generation == generation) else { return };
    ed.llm_pid = None;
    let secs = rev.started.elapsed().as_secs_f32();
    let replies = match result {
        Ok(r) => r,
        Err(e) => {
            ed.review = None;
            ed.set_error(format!("claude: {e}"));
            return;
        }
    };
    let Some(doc_idx) = ed.docs.iter().position(|d| d.id == rev.doc_id) else {
        ed.review = None;
        ed.set_error("claude: buffer was closed");
        return;
    };
    let text = &ed.docs[doc_idx].text;
    for (h, new) in rev.hunks.iter_mut().zip(replies) {
        h.new = match_trailing_newline(&h.old, new);
        if h.new == h.old {
            h.decision = Decision::Reject;
        } else if !relocate(h, text) {
            h.conflict = true;
        }
    }
    if rev.hunks.iter().all(|h| h.decision == Decision::Reject || h.conflict) {
        let msg = if rev.hunks.iter().any(|h| h.conflict) {
            "claude: text changed while waiting — nothing applied"
        } else {
            "claude: no changes"
        };
        ed.review = None;
        ed.set_status(msg);
        return;
    }
    rev.ready = true;
    rev.current =
        rev.hunks.iter().position(|h| h.decision == Decision::Undecided && !h.conflict).unwrap_or(0);
    ed.current = doc_idx;
    ed.set_status(format!("claude answered in {secs:.1}s"));
}

/// If edits happened while waiting, relocates the original (nearest match). False if not found.
fn relocate(h: &mut Hunk, text: &Rope) -> bool {
    // Edits while waiting may leave the old position mid-character — check boundaries before slicing.
    let boundary = |b: usize| b <= text.len_bytes() && text.char_to_byte(text.byte_to_char(b)) == b;
    if boundary(h.from) && boundary(h.to) && h.from <= h.to && text.byte_slice(h.from..h.to) == h.old.as_str()
    {
        return true;
    }
    if h.old.is_empty() {
        return false;
    }
    let s = text.to_string();
    let best = s.match_indices(&h.old).map(|(b, _)| b).min_by_key(|&b| b.abs_diff(h.from));
    match best {
        Some(from) => {
            h.to = from + h.old.len();
            h.from = from;
            true
        }
        None => false,
    }
}

/// Keys during review. True if handled.
pub fn review_key(ed: &mut Editor, key: Key) -> bool {
    let Some(rev) = ed.review.as_mut().filter(|r| r.ready) else { return false };
    let decide = |rev: &mut Review, d: Decision| {
        rev.hunks[rev.current].decision = d;
        rev.follow = true;
        match rev.next_undecided() {
            Some(i) => {
                rev.current = i;
                false
            }
            None => true,
        }
    };
    let done = match (key.code, key.ctrl || key.alt) {
        (Code::Char('y'), false) => decide(rev, Decision::Accept),
        (Code::Char('n'), false) => decide(rev, Decision::Reject),
        (Code::Char('a'), false) => {
            rev.hunks
                .iter_mut()
                .filter(|h| h.decision == Decision::Undecided)
                .for_each(|h| h.decision = Decision::Accept);
            true
        }
        (Code::Char('q'), false) | (Code::Esc, _) => true,
        (Code::Char('j') | Code::Down, false) => {
            rev.scroll += 1;
            rev.follow = false;
            false
        }
        (Code::Char('k') | Code::Up, false) => {
            rev.scroll = rev.scroll.saturating_sub(1);
            rev.follow = false;
            false
        }
        (Code::Tab, _) => {
            rev.current = rev.next_undecided().unwrap_or(rev.current);
            rev.follow = true;
            false
        }
        _ => false,
    };
    if done {
        finish(ed);
    }
    true
}

/// Accepted changes as one transaction (= one undo).
fn finish(ed: &mut Editor) {
    let Some(mut rev) = ed.review.take() else { return };
    let on_done = rev.on_done.take();
    let applied = apply_accepted(ed, &rev);
    if let Some(cb) = on_done {
        cb(ed, applied);
    }
}

fn apply_accepted(ed: &mut Editor, rev: &Review) -> bool {
    let accepted: Vec<&Hunk> =
        rev.hunks.iter().filter(|h| h.decision == Decision::Accept && !h.conflict).collect();
    if accepted.is_empty() {
        ed.set_status("claude: nothing applied");
        return false;
    }
    let Some(idx) = ed.docs.iter().position(|d| d.id == rev.doc_id) else { return false };
    ed.current = idx;
    let tx = Transaction::new(
        accepted.iter().map(|h| Change { from: h.from, to: h.to, insert: h.new.clone() }).collect(),
    );
    let ranges: Vec<Range> = accepted
        .iter()
        .map(|h| {
            let start = tx.map_pos(h.from, Assoc::Before);
            Range::new(start, start + h.new.len())
        })
        .collect();
    let n = ranges.len();
    ed.with_group(|cx| cx.editor.doc_mut().apply_with(&tx, Selection::new(ranges, 0)));
    ed.set_status(format!("claude: applied {n} change(s) — u to undo"));
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_lines_and_preview() {
        let delta = serde_json::json!({"type": "stream_event", "event": {"type": "content_block_delta", "delta": {"type": "text_delta", "text": "ab"}}});
        assert!(matches!(parse_line(&delta), Some(StreamEv::Text(t)) if t == "ab"));
        let think =
            serde_json::json!({"type": "system", "subtype": "thinking_tokens", "estimated_tokens": 5});
        assert!(matches!(parse_line(&think), Some(StreamEv::Thinking)));
        let stopped =
            serde_json::json!({"type": "result", "subtype": "error_during_execution", "result": ""});
        assert!(matches!(parse_line(&stopped), Some(StreamEv::Done(Err(e))) if e == "stopped"));
        // Preview: the last lines inside the tag being written
        assert_eq!(preview("<r i=\"1\">a</r>\n<r i=\"2\">\nx\ny\nz", 2), vec!["y", "z"]);
        assert_eq!(preview("plain\ntext", 5), vec!["plain", "text"]);
    }

    #[test]
    fn parses_tagged_replies() {
        let out = "<r i=\"1\">\nfoo\n</r>\n<r i=\"2\">bar</r>";
        assert_eq!(parse_replies(out, 2).unwrap(), vec!["foo", "bar"]);
        assert!(parse_replies("<r i=\"1\">x</r>", 2).is_err());
        // With one selection, also an answer without tags·fences
        assert_eq!(parse_replies("```rust\nlet x = 1;\n```", 1).unwrap(), vec!["let x = 1;"]);
    }

    #[test]
    fn review_rows_expand_changes_in_place() {
        let text = Rope::from_str("a\nb\nc\n");
        let hunk = |decision| Hunk {
            from: 2,
            to: 4,
            old: "b\n".into(),
            new: "B\nB2\n".into(),
            decision,
            conflict: false,
        };
        let rev = |h| Review::new(0, 0, "", vec![h]);
        let diff = |kind, t: &str, no| RRow::Diff { kind, text: t.into(), old_no: no, hunk: 0 };
        let rows = rev(hunk(Decision::Undecided)).rows(&text);
        assert_eq!(
            rows,
            [
                RRow::Doc(0),
                RRow::Header(0),
                diff('-', "b", Some(2)),
                diff('+', "B", None),
                diff('+', "B2", None),
                RRow::Doc(2),
                RRow::Doc(3)
            ]
        );
        // Accept = new text only, reject = original only
        let acc = rev(hunk(Decision::Accept)).rows(&text);
        assert!(
            acc.contains(&diff('+', "B", None))
                && !acc.iter().any(|r| matches!(r, RRow::Diff { kind: '-', .. }))
        );
        let rej = rev(hunk(Decision::Reject)).rows(&text);
        assert_eq!(rej[2], diff(' ', "b", Some(2)));
        assert_eq!(rej.len(), 5);
    }

    #[test]
    fn trailing_newline_follows_original() {
        assert_eq!(match_trailing_newline("a\n", "b".into()), "b\n");
        assert_eq!(match_trailing_newline("a", "b\n\n".into()), "b");
    }

    #[test]
    fn line_diff_marks_changes() {
        let d = line_diff("a\nb\nc", "a\nB\nc");
        assert_eq!(d, vec![(' ', "a"), ('-', "b"), ('+', "B"), (' ', "c")]);
    }

    #[test]
    fn prompt_carries_selection_and_context() {
        let text = Rope::from_str("one\ntwo\nthree\n");
        let p = build_prompt("upper", "x.txt", &text, &[(4, 7)], 1);
        assert!(p.contains("Instruction: upper"));
        assert!(p.contains("<text>two</text>"));
        assert!(p.contains("<before>one\n</before>"));
        assert!(p.contains("lines=\"2-2\""));
        // A line-wise selection (up to the next line's start) is still that one line
        let p = build_prompt("upper", "x.txt", &text, &[(4, 8)], 1);
        assert!(p.contains("lines=\"2-2\""), "{p}");
        assert!(p.contains("<after>three\n</after>"), "{p}");
    }
}
