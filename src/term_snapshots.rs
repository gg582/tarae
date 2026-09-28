//! Screen snapshot ("golden") tests — one frame from `render`, fed through a vt100 emulator, compared with
//! `src/snapshots/<name>.txt`. Catches layout and color-hierarchy regressions without eyeballing
//! screenshots. After an intended change: `TARAE_BLESS=1 cargo test snapshot_` rewrites the files
//! (then read the diff before committing).
//!
//! Snapshot format — four sections; every screen row is prefixed with its two-digit row number, trailing
//! blanks are trimmed (so editors that strip whitespace don't break the files):
//!
//! ```text
//! # <name> · <w>x<h> · theme meok · cursor <row>:<col>     ("cursor hidden" when not shown)
//! ## text
//! 00 <the row's characters — a wide char appears once, though it covers two cells>
//! ## bg · surface per cell, blank = ui.background
//! 00 <one mark per cell: a letter from the legend, 1–9 = other colors numbered by first appearance
//!     (blends — diff/tint backgrounds), ~ = never painted (terminal default), R = reverse video>
//! ## fg · visible glyphs only, blank = any unlisted color, UPPERCASE = bold (B = bold, unlisted color)
//! 00 <one mark per cell>
//! ## legend
//! bg: c=ui.cursorline.primary  p=ui.popup  1=other …   (only the marks that appear, theme key per mark)
//! fg: a=ui.accent  d=ui.virtual  B=bold …
//! ```
//!
//! The marks name theme keys rather than RGB values, so tweaking a palette color doesn't churn every file,
//! while a surface or accent moving to the wrong cells does show up. When two keys share a color the
//! first key in `BG`/`FG` wins.
//!
//! Determinism: fixed theme (meok), no language servers or git unless faked, only the built-in (core)
//! grammars, toasts frozen at full opacity, and every worker job the frame depends on is awaited
//! (`Shot::until`) — no sleeps.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use ropey::Rope;
use serde_json::json;

use super::{Ui, card_style, render};
use crate::config::Config;
use crate::document::DocId;
use crate::editor::Editor;
use crate::event::Event;
use crate::key::{Code, Key};

/// Background marks: (mark, theme key).
const BG: &[(char, &str)] = &[
    ('c', "ui.cursorline.primary"),
    ('p', "ui.popup"),
    ('P', "ui.menu.selected"),
    ('v', "ui.selection"),
    ('V', "ui.selection.primary"),
    ('A', "ui.cursor.primary"),
    ('k', "ui.cursor"),
    ('i', "ui.cursor.primary.insert"),
    ('s', "ui.cursor.primary.select"),
];

/// Foreground marks: (mark, theme key) — lowercase; bold prints the uppercase.
const FG: &[(char, &str)] = &[
    ('a', "ui.accent"),
    ('t', "ui.text.focus"),
    ('d', "ui.virtual"),
    ('f', "ui.linenr"),
    ('e', "error"),
    ('w', "warning"),
    ('g', "diff.plus"),
];

/// Languages whose grammars are always compiled in (`core = true` in languages.toml).
const CORE: &[&str] = &["rust", "toml", "markdown"];

/// Fixture root for scenarios that need real files (picker listing and preview).
fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src/snapshots/fixtures")
}

/// A small Rust file (doc comment, loop, string, multi-byte text) — the scenarios' default document.
const DEMO_RS: &str = r#"use std::collections::HashMap;

/// Counts how often each word appears.
///
/// Words are split on whitespace; case is kept.
pub fn word_counts(text: &str) -> HashMap<&str, usize> {
    let mut counts = HashMap::new();
    for word in text.split_whitespace() {
        *counts.entry(word).or_insert(0) += 1;
    }
    counts
}

fn main() {
    let text = "the quick brown fox jumps over the lazy dog";
    let counts = word_counts(text);
    println!("{} distinct words", counts.len()); // 타래
}
"#;

/// One editor at a fixed screen size, driven like the real loop: every event goes through
/// `handle_event` and is followed by a frame (so `viewport`/`view` are what the next key sees).
struct Shot {
    ed: Editor,
    w: u16,
    h: u16,
}

impl Shot {
    fn new(w: u16, h: u16) -> Self {
        let mut ed = Editor::new(Config::default());
        assert_eq!(ed.theme.name, "meok", "snapshots are drawn with the dark built-in theme");
        ed.config.lsp.enabled = false;
        let mut shot = Shot { ed, w, h };
        shot.frame();
        shot
    }

    /// Shows `text` as the file `rel` (relative to the working directory, so the path bar reads
    /// `tarae › …`) without touching disk, and waits for its syntax tree (core grammars only).
    fn file(&mut self, rel: &str, text: &str) -> DocId {
        let path = std::env::current_dir().unwrap().join(rel);
        let doc = self.ed.doc_mut();
        doc.text = Rope::from_str(text);
        doc.path = Some(path.clone());
        let id = doc.id;
        // Only the built-in (core) grammars — others depend on what this machine has downloaded
        if crate::syntax::detect(&path).is_some_and(|l| CORE.contains(&l.name.as_str())) {
            self.ed.attach_syntax(id);
            self.until("syntax tree", |ed| {
                ed.doc().syntax.as_ref().is_some_and(|s| s.tree.is_some() && !s.dirty && !s.in_flight)
            });
        }
        self.frame();
        id
    }

    /// Keys like the editor tests' `feed`: plain chars, `<name>` for named keys (`<ret>`, `<C-w>`).
    fn keys(&mut self, keys: &str) {
        let mut chars = keys.chars();
        while let Some(c) = chars.next() {
            let key = if c == '<' {
                chars.by_ref().take_while(|&c| c != '>').collect::<String>().parse().unwrap()
            } else {
                Key::plain(Code::Char(c))
            };
            self.ed.handle_event(Event::Key(key));
            self.frame();
        }
    }

    /// Applies job results until `done` holds (each followed by a frame, like the event loop).
    fn until(&mut self, what: &str, done: impl Fn(&Editor) -> bool) {
        let end = Instant::now() + Duration::from_secs(10);
        while !done(&self.ed) {
            let left = end.saturating_duration_since(Instant::now());
            let ev =
                self.ed.events.recv_timeout(left).unwrap_or_else(|| panic!("timed out waiting for {what}"));
            self.ed.handle_event(ev);
            self.frame();
        }
    }

    /// Waits for reparses after edits (syntax dirty or in flight).
    fn settle(&mut self) {
        self.until("reparse", |ed| {
            ed.docs.iter().all(|d| d.syntax.as_ref().is_none_or(|s| !s.dirty && !s.in_flight))
        });
    }

    fn frame(&mut self) -> Vec<u8> {
        let mut buf = Vec::new();
        render(&mut self.ed, &mut buf, self.w, self.h).unwrap();
        buf
    }

    /// Draws the final frame and compares it with `src/snapshots/<name>.txt`.
    fn check(&mut self, name: &str) {
        let bytes = self.frame();
        let text = snapshot(&self.ed, name, &bytes, self.w, self.h);
        compare(name, &text);
    }
}

// ── Screen → text ─────────────────────────────────────────────────────────

fn vt_color(c: Option<crossterm::style::Color>) -> Option<vt100::Color> {
    use crossterm::style::Color as C;
    use vt100::Color::{Default, Idx, Rgb};
    Some(match c? {
        C::Reset => Default,
        C::Rgb { r, g, b } => Rgb(r, g, b),
        C::AnsiValue(i) => Idx(i),
        C::Black => Idx(0),
        C::DarkRed => Idx(1),
        C::DarkGreen => Idx(2),
        C::DarkYellow => Idx(3),
        C::DarkBlue => Idx(4),
        C::DarkMagenta => Idx(5),
        C::DarkCyan => Idx(6),
        C::Grey => Idx(7),
        C::DarkGrey => Idx(8),
        C::Red => Idx(9),
        C::Green => Idx(10),
        C::Yellow => Idx(11),
        C::Blue => Idx(12),
        C::Magenta => Idx(13),
        C::Cyan => Idx(14),
        C::White => Idx(15),
    })
}

fn snapshot(ed: &Editor, name: &str, bytes: &[u8], w: u16, h: u16) -> String {
    let mut parser = vt100::Parser::new(h, w, 0);
    parser.process(bytes);
    let screen = parser.screen();
    let ui = Ui::new(ed);
    let t = &ed.theme;
    let base_bg = vt_color(ui.base.bg).unwrap_or_default();
    let mut bg_keys: Vec<(vt100::Color, char, &str)> = Vec::new();
    for &(m, key) in BG {
        let bg =
            if key == "ui.popup" { card_style(&ui, ed, key).bg } else { t.try_get(key).and_then(|s| s.bg) };
        if let Some(c) = vt_color(bg).filter(|c| *c != base_bg && !bg_keys.iter().any(|k| k.0 == *c)) {
            bg_keys.push((c, m, key));
        }
    }
    let mut fg_keys: Vec<(vt100::Color, char, &str)> = Vec::new();
    for &(m, key) in FG {
        let fg = if key == "ui.accent" { ui.accent.fg } else { t.try_get(key).and_then(|s| s.fg) };
        if let Some(c) = vt_color(fg).filter(|c| !fg_keys.iter().any(|k| k.0 == *c)) {
            fg_keys.push((c, m, key));
        }
    }
    let mut others: Vec<vt100::Color> = Vec::new();
    let (mut used_bg, mut used_fg) = (Vec::new(), Vec::new());
    let (mut text, mut bgs, mut fgs) = (Vec::new(), Vec::new(), Vec::new());
    for row in 0..h {
        let (mut tl, mut bl, mut fl) = (String::new(), String::new(), String::new());
        for col in 0..w {
            let Some(cell) = screen.cell(row, col) else { continue };
            // A wide char's second cell: text already printed; style marks repeat the first cell's
            let lead = if cell.is_wide_continuation() && col > 0 { screen.cell(row, col - 1) } else { None };
            let c = lead.unwrap_or(cell);
            if lead.is_none() {
                tl.push_str(if cell.has_contents() { cell.contents() } else { " " });
            }
            let bm = match c.bgcolor() {
                _ if c.inverse() => 'R',
                vt100::Color::Default if base_bg != vt100::Color::Default => '~',
                bg if bg == base_bg => ' ',
                bg => match bg_keys.iter().find(|k| k.0 == bg) {
                    Some(k) => k.1,
                    None => {
                        let i = others.iter().position(|o| *o == bg).unwrap_or_else(|| {
                            others.push(bg);
                            others.len() - 1
                        });
                        char::from_digit(i as u32 + 1, 10).unwrap_or('+')
                    }
                },
            };
            if bm != ' ' && !used_bg.contains(&bm) {
                used_bg.push(bm);
            }
            bl.push(bm);
            let visible = c.has_contents() && !c.contents().trim().is_empty();
            let fm = match fg_keys.iter().find(|k| k.0 == c.fgcolor()) {
                _ if !visible => ' ',
                Some(k) if c.bold() => k.1.to_ascii_uppercase(),
                Some(k) => k.1,
                None if c.bold() => 'B',
                None => ' ',
            };
            if fm != ' ' && !used_fg.contains(&fm.to_ascii_lowercase()) {
                used_fg.push(fm.to_ascii_lowercase());
            }
            fl.push(fm);
        }
        text.push(tl);
        bgs.push(bl);
        fgs.push(fl);
    }
    let (cy, cx) = screen.cursor_position();
    let cursor = if screen.hide_cursor() { "cursor hidden".to_string() } else { format!("cursor {cy}:{cx}") };
    let mut out = format!("# {name} · {w}x{h} · theme {} · {cursor}\n", t.name);
    let section = |out: &mut String, title: &str, rows: &[String]| {
        out.push_str(title);
        out.push('\n');
        for (i, r) in rows.iter().enumerate() {
            out.push_str(format!("{i:02} {r}").trim_end());
            out.push('\n');
        }
    };
    section(&mut out, "## text", &text);
    section(&mut out, "## bg · surface per cell, blank = ui.background", &bgs);
    section(&mut out, "## fg · visible glyphs only, blank = unlisted color, UPPERCASE = bold", &fgs);
    // Legend in table order (then digits, then the specials) — independent of where marks first appear
    let rank = |keys: &[(vt100::Color, char, &str)], m: char| {
        keys.iter().position(|k| k.1 == m).unwrap_or(keys.len() + "123456789+~RB".find(m).unwrap_or(99))
    };
    used_bg.sort_by_key(|m| rank(&bg_keys, *m));
    used_fg.sort_by_key(|m| rank(&fg_keys, if *m == 'b' { 'B' } else { *m }));
    let bg_names: Vec<String> = used_bg
        .iter()
        .map(|m| match bg_keys.iter().find(|k| k.1 == *m) {
            Some(k) => format!("{m}={}", k.2),
            None if *m == '~' => "~=unpainted".into(),
            None if *m == 'R' => "R=reverse".into(),
            None => format!("{m}=other"),
        })
        .collect();
    let fg_names: Vec<String> = used_fg
        .iter()
        .map(|m| match fg_keys.iter().find(|k| k.1 == *m) {
            Some(k) => format!("{m}={}", k.2),
            None => "B=bold".into(),
        })
        .collect();
    out.push_str(&format!("## legend\nbg: {}\nfg: {}\n", bg_names.join("  "), fg_names.join("  ")));
    out
}

// ── Golden files ──────────────────────────────────────────────────────────

fn compare(name: &str, actual: &str) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/snapshots").join(format!("{name}.txt"));
    if std::env::var_os("TARAE_BLESS").is_some_and(|v| v == "1") {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, actual).unwrap();
        return;
    }
    // The test's own name (one test may check several files)
    let thread = std::thread::current();
    let test = thread.name().and_then(|n| n.rsplit("::").next()).unwrap_or("snapshot_");
    let hint = format!("TARAE_BLESS=1 cargo test {test}");
    let Ok(expected) = std::fs::read_to_string(&path) else {
        panic!("no snapshot {} yet — create it with `{hint}`\n\n{actual}", path.display());
    };
    let expected = expected.replace("\r\n", "\n");
    if expected != actual {
        panic!(
            "snapshot {name} differs ({}) — `-` expected, `+` actual:\n\n{}\nIf the change is intended: `{hint}`",
            path.display(),
            line_diff(&expected, actual)
        );
    }
}

/// Unified-style line diff (LCS) with two lines of context around changes.
fn line_diff(a: &str, b: &str) -> String {
    let (a, b): (Vec<&str>, Vec<&str>) = (a.lines().collect(), b.lines().collect());
    let (n, m) = (a.len(), b.len());
    let mut lcs = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lcs[i][j] = if a[i] == b[j] { lcs[i + 1][j + 1] + 1 } else { lcs[i + 1][j].max(lcs[i][j + 1]) };
        }
    }
    let mut ops: Vec<(char, &str)> = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < n || j < m {
        if i < n && j < m && a[i] == b[j] {
            ops.push((' ', a[i]));
            (i, j) = (i + 1, j + 1);
        } else if i < n && (j == m || lcs[i + 1][j] >= lcs[i][j + 1]) {
            ops.push(('-', a[i]));
            i += 1;
        } else {
            ops.push(('+', b[j]));
            j += 1;
        }
    }
    let near = |k: usize| ops[k.saturating_sub(2)..(k + 3).min(ops.len())].iter().any(|o| o.0 != ' ');
    let mut out = String::new();
    let mut skipped = false;
    for (k, (op, line)) in ops.iter().enumerate() {
        if near(k) {
            out.push_str(&format!("{op} {line}\n"));
            skipped = false;
        } else if !skipped {
            out.push_str("  …\n");
            skipped = true;
        }
    }
    out
}

// ── Scenarios ─────────────────────────────────────────────────────────────

/// Normal editing: line numbers, cursorline, folded doc comment, path bar with the enclosing function.
#[test]
fn snapshot_editing() {
    let mut s = Shot::new(100, 30);
    s.file("src/demo.rs", DEMO_RS);
    s.keys("8ggww");
    s.check("editing");
}

/// Same at 50x15 — path bar and status line fold, text is clipped.
#[test]
fn snapshot_editing_narrow() {
    let mut s = Shot::new(50, 15);
    s.file("src/demo.rs", DEMO_RS);
    s.keys("8ggww");
    s.check("editing_narrow");
}

/// Status line with every segment, at four widths — least important segments drop first.
#[test]
fn snapshot_statusline_widths() {
    for (name, w) in [
        ("statusline_wide", 120),
        ("statusline_medium", 56),
        ("statusline_tight", 50),
        ("statusline_narrow", 30),
    ] {
        let mut s = Shot::new(w, 6);
        s.file("src/demo.rs", DEMO_RS);
        s.ed.git_branch = Some("main".into());
        let diag = |from, to, severity, message: &str| crate::lsp::Diagnostic {
            from,
            to,
            severity,
            message: message.into(),
            raw: json!({}),
        };
        let use_at = DEMO_RS.find("HashMap;").unwrap();
        s.ed.doc_mut().lsp.diagnostics =
            vec![diag(use_at, use_at + 7, 1, "unresolved import"), diag(4, 7, 2, "unused import")];
        s.keys("C");
        s.check(name);
    }
}

/// Diagnostics on the cursor line: an error whose detail is on its second line (+ a hint) → the card below
/// the cursor with all of it, `code` in syntax colors.
fn diagnostic_card_shot() -> Shot {
    let mut s = Shot::new(90, 24);
    s.file("src/demo.rs", DEMO_RS);
    let at = DEMO_RS.find("word_counts(text);").unwrap();
    let diag = |from, severity, message: &str, raw| crate::lsp::Diagnostic {
        from,
        to: from + 11,
        severity,
        message: message.into(),
        raw,
    };
    s.ed.doc_mut().lsp.diagnostics = vec![
        diag(
            at,
            1,
            "mismatched types\nexpected `HashMap<&str, u32>`, found `HashMap<&str, usize>`",
            json!({ "source": "rustc", "code": "E0308" }),
        ),
        diag(at - 9, 4, "expected due to this", json!({ "source": "rustc", "code": "E0308" })),
    ];
    s.keys("16G");
    s
}

#[test]
fn snapshot_diagnostic_card() {
    diagnostic_card_shot().check("diagnostic_card");
}

/// The card only when the line end can't show it all · not in insert mode · Esc hides it until the
/// cursor leaves the line.
#[test]
fn diagnostic_card_shows_only_what_the_line_end_cannot() {
    let mut s = diagnostic_card_shot();
    let shown = |s: &mut Shot| {
        let bytes = s.frame();
        snapshot(&s.ed, "card", &bytes, s.w, s.h).contains("found HashMap")
    };
    assert!(shown(&mut s));
    s.keys("<esc>");
    assert!(!shown(&mut s), "Esc hides it");
    s.keys("l");
    assert!(!shown(&mut s), "still hidden on the same line");
    s.keys("jk");
    assert!(shown(&mut s), "back after leaving the line");
    s.keys("i");
    assert!(!shown(&mut s), "not while typing");
    s.keys("<esc>");
    // One short single-line message fits at the line end — no card
    let at = DEMO_RS.find("word_counts(text);").unwrap();
    s.ed.doc_mut().lsp.diagnostics = vec![crate::lsp::Diagnostic {
        from: at,
        to: at + 11,
        severity: 2,
        message: "unused result".into(),
        raw: json!({ "source": "rustc" }),
    }];
    let bytes = s.frame();
    let t = snapshot(&s.ed, "card", &bytes, s.w, s.h);
    assert!(!t.contains("  rustc"), "{}", &t[..t.find("## bg").unwrap()]);
    // The same message on a line too long for it → card
    s.ed.config.inline_diagnostics = false;
    let bytes = s.frame();
    assert!(snapshot(&s.ed, "card", &bytes, s.w, s.h).contains("  rustc"));
}

/// Start screen (no file).
#[test]
fn snapshot_welcome() {
    let mut s = Shot::new(100, 30);
    s.check("welcome");
}

/// Which-key card after `space`.
#[test]
fn snapshot_which_key() {
    let mut s = Shot::new(100, 30);
    s.file("src/demo.rs", DEMO_RS);
    s.keys(" ");
    s.check("which_key");
}

/// Command palette (`space ?`) with a query typed — matched chars in accent, key hints dimmed.
#[test]
fn snapshot_command_palette() {
    let mut s = Shot::new(100, 30);
    s.file("src/demo.rs", DEMO_RS);
    s.keys(" ?split");
    s.check("command_palette");
}

/// File picker over the fixture folder, filtered, with the selected file's preview (worker-filled).
#[test]
fn snapshot_file_picker() {
    let mut s = Shot::new(100, 30);
    let root = fixtures().join("demo");
    s.ed.open_picker(
        crate::picker::Picker::new("files in demo", Vec::new(), true),
        Some(Box::new(move || Ok(crate::picker::file_items(&root)))),
    );
    s.until("file list", |ed| ed.picker.as_ref().is_some_and(|p| !p.loading));
    s.keys("main");
    let ready = |ed: &Editor| {
        let Some(p) = ed.picker.as_ref() else { return false };
        let Some((path, _)) = p.current().and_then(|i| i.action.preview_target()) else { return false };
        matches!(p.previews.get(path), Some(crate::picker::Preview::Ready { .. }))
    };
    s.until("preview", ready);
    s.check("file_picker");
}

/// Vertical split (`C-w v`) — two panes of the same file; the focused (right) one jumps to the end and
/// scrolls, the other keeps its own scroll.
#[test]
fn snapshot_split_vertical() {
    let mut s = Shot::new(100, 16);
    s.file("src/demo.rs", DEMO_RS);
    s.keys("<C-w>vG");
    s.check("split_vertical");
}

/// Doc comments render as markdown (bold, code, list, heading, code block) while the cursor is elsewhere.
#[test]
fn snapshot_doc_comment_folded() {
    const SRC: &str = r#"//! Tiny geometry helpers.

/// A point on the **plane**.
///
/// - `x` grows to the right
/// - `y` grows downward
///
/// # Examples
///
/// ```
/// let p = Point { x: 1, y: 2 };
/// ```
#[derive(Clone, Copy, Debug)]
pub struct Point {
    pub x: i32,
    pub y: i32,
}

impl Point {
    /// Distance to `other`, _squared_ — cheap to compare.
    pub fn dist2(self, other: Point) -> i32 {
        let (dx, dy) = (self.x - other.x, self.y - other.y);
        dx * dx + dy * dy
    }
}
"#;
    let mut s = Shot::new(100, 30);
    s.file("src/point.rs", SRC);
    s.keys("22gg");
    s.check("doc_comment_folded");
}

/// Error toast, frozen at full opacity (between fade-in and fade-out).
#[test]
fn snapshot_toast_error() {
    let mut s = Shot::new(100, 30);
    s.file("src/demo.rs", DEMO_RS);
    s.ed.set_error("Couldn't save demo.rs: permission denied");
    s.ed.set_warning("demo.rs changed on disk");
    // Half a second in: fully faded in, seconds before the fade-out starts (errors live 7 s, warnings 6 s)
    let fresh = Instant::now().checked_sub(Duration::from_millis(500)).unwrap();
    for t in &mut s.ed.toasts {
        t.at = fresh;
        assert_eq!(t.alpha(), 1.0);
    }
    s.check("toast_error");
}

/// Grammar download offer card (offers are off in tests — on just here; the card only, no download).
#[test]
fn snapshot_offer_grammar() {
    let mut s = Shot::new(100, 30);
    s.ed.offer_popups = true;
    s.file("app.py", "def greet(name):\n    return f\"hello, {name}\"\n\n\nprint(greet(\"tarae\"))\n");
    s.ed.offer_grammar(crate::syntax::spec("python").unwrap());
    assert_eq!(s.ed.offers.len(), 1);
    s.check("offer_grammar");
}

/// `:ask` review — the answer shown as an inline diff inside the buffer (fake claude).
#[test]
fn snapshot_ask_review() {
    let mut s = Shot::new(100, 30);
    s.file("src/demo.rs", DEMO_RS);
    let reply = "<r i=\"1\">    println!(\"{} distinct words in {:?}\", counts.len(), text);\n</r>";
    let json = json!({"type": "result", "is_error": false, "result": reply}).to_string();
    s.ed.config.llm.command = "sh".into();
    s.ed.config.llm.args = vec!["-c".into(), format!("read -r line; printf '%s\\n' '{json}'")];
    s.keys("17ggx i");
    s.keys("show the text too<ret>");
    s.until("review", |ed| ed.review.as_ref().is_some_and(|r| r.ready));
    s.ed.toasts.clear(); // "claude answered in 0.0s" — wall-clock time
    s.check("ask_review");
}

/// Chat panel, empty state (a fake claude that just waits).
#[test]
fn snapshot_chat_empty() {
    let mut s = Shot::new(100, 30);
    s.file("src/demo.rs", DEMO_RS);
    s.ed.config.llm.command = "sh".into();
    s.ed.config.llm.args = vec!["-c".into(), "cat > /dev/null".into()];
    s.keys(" l");
    assert!(s.ed.chat.as_ref().is_some_and(|c| c.msgs.is_empty()), "chat opened without a note");
    s.check("chat_empty");
}

/// Completion card with the selected item's docs — the list comes from a fake server (`cat`), the
/// response is injected with `on_lsp_message`.
#[test]
fn snapshot_lsp_completion() {
    let log = std::env::temp_dir().join(format!("tarae-snapshot-lsp-{}.log", std::process::id()));
    let cfg = format!(
        "[lsp.fake]\ncommand = \"sh\"\nargs = [\"-c\", \"cat > '{}'\"]\n[lang.rust]\nlsp = [\"fake\"]\n",
        log.display()
    );
    let (config, warnings) = crate::config::parse(&cfg);
    assert!(warnings.is_empty(), "{warnings:?}");
    let mut s = Shot::new(100, 30);
    let lsp = config.editor.lsp;
    s.ed.config.lsp = lsp;
    let id = s.file("src/demo.rs", "fn main() {\n    let v = vec![3, 1, 2];\n    \n}\n");
    s.ed.attach_lsp(id);
    let cid = s.ed.doc().lsp.client.expect("fake server attached");
    let caps = json!({ "positionEncoding": "utf-8", "completionProvider": { "triggerCharacters": ["."] } });
    s.ed.on_lsp_message(cid, json!({ "id": 0, "result": { "capabilities": caps } }));
    s.keys("jjAv.");
    let id =
        s.ed.lsp
            .pending
            .iter()
            .filter(|(_, p)| matches!(p.kind, crate::lsp_editor::Kind::Completion))
            .map(|((_, id), _)| *id)
            .max()
            .expect("completion requested");
    let item = |n: u8, label: &str, detail: &str| json!({ "label": label, "kind": 2, "detail": detail, "sortText": n.to_string() });
    let mut push = item(0, "push", "fn(&mut self, value: i32)");
    push["documentation"] = json!({ "kind": "markdown", "value": "Appends an element to the back of a collection.\n\n# Panics\n\nPanics if the new capacity exceeds `isize::MAX` bytes." });
    let items = json!({ "isIncomplete": false, "items": [
        push,
        item(1, "pop", "fn(&mut self) -> Option<i32>"),
        item(2, "len", "fn(&self) -> usize"),
        item(3, "iter", "fn(&self) -> Iter<'_, i32>"),
        item(4, "capacity", "fn(&self) -> usize"),
    ]});
    s.ed.on_lsp_message(cid, json!({ "id": id, "result": items }));
    s.frame();
    s.keys("<tab>");
    s.settle();
    assert!(s.ed.completion.as_ref().is_some_and(|c| c.docs.is_some()), "docs shown for the selected item");
    s.check("lsp_completion");
    let _ = std::fs::remove_file(&log);
}
