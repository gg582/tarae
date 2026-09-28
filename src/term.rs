//! Terminal UI — redraws the whole frame every time over crossterm (synchronized updates, so no flicker).
//! A cell-diff buffer gets added when it becomes necessary (remote/slow terminals).

use std::io::{self, Write};
use std::sync::OnceLock;
use std::sync::mpsc::Sender;
use std::thread;

use anyhow::Result;
use crossterm::cursor::{Hide, MoveTo, SetCursorStyle, Show};
use crossterm::event::{
    self, DisableFocusChange, DisableMouseCapture, EnableFocusChange, EnableMouseCapture, Event as CtEvent,
    KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
};
use crossterm::style::{
    Attribute, Print, ResetColor, SetAttribute, SetBackgroundColor, SetForegroundColor, SetUnderlineColor,
};
use crossterm::terminal::{
    self, BeginSynchronizedUpdate, Clear, ClearType, EndSynchronizedUpdate, EnterAlternateScreen,
    LeaveAlternateScreen,
};
use crossterm::{execute, queue};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::config::{CursorShape, LineNumber};
use crate::editor::{Editor, Mode};
use crate::event::Event;
use crate::graphemes;
use crate::key::Key;
use crate::llm::Review;
use crate::movement as mv;
use crate::picker::{Action, Picker};
use crate::selection::Range;
use crate::syntax;
use crate::theme::{Style, Theme};
use tree_sitter::QueryCursor;

pub fn run(editor: &mut Editor) -> Result<()> {
    terminal::enable_raw_mode()?;
    execute!(io::stdout(), EnterAlternateScreen, EnableMouseCapture, EnableFocusChange)?;
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = restore();
        prev(info);
    }));
    let res = event_loop(editor);
    restore()?;
    res
}

/// Is the terminal background light — asked once at startup to pick the `default` theme (meok/hanji).
/// Order: `TARAE_BACKGROUND=light|dark` → query terminal bg color (OSC 11) → `COLORFGBG` → unknown (dark).
/// DA1 (`CSI c`) is sent right after the query — nearly every terminal answers DA1, so even terminals that
/// don't know OSC 11 finish immediately instead of waiting for the timeout (usually under 1 ms).
pub fn detect_light_background() -> Option<bool> {
    match std::env::var("TARAE_BACKGROUND").as_deref() {
        Ok("light") => return Some(true),
        Ok("dark") => return Some(false),
        _ => {}
    }
    query_background().or_else(|| {
        // "fg;bg" — light if bg is 7 (white) or 15 (bright white)
        let v = std::env::var("COLORFGBG").ok()?;
        let bg: u8 = v.rsplit(';').next()?.parse().ok()?;
        Some(bg == 7 || bg == 15)
    })
}

fn query_background() -> Option<bool> {
    use std::io::Read;
    // SAFETY: isatty only looks at the fd.
    if unsafe { libc::isatty(0) == 0 || libc::isatty(1) == 0 } {
        return None;
    }
    terminal::enable_raw_mode().ok()?;
    let mut reply = Vec::new();
    let mut out = io::stdout();
    let sent = out.write_all(b"\x1b]11;?\x1b\\\x1b[c").and_then(|_| out.flush()).is_ok();
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(200);
    let da1_done =
        |r: &[u8]| r.windows(3).position(|w| w == b"\x1b[?").is_some_and(|i| r[i..].contains(&b'c'));
    while sent && !da1_done(&reply) {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        if left.is_zero() {
            break;
        }
        let mut pfd = libc::pollfd { fd: 0, events: libc::POLLIN, revents: 0 };
        // SAFETY: pfd is a single valid pollfd on this stack.
        if unsafe { libc::poll(&mut pfd, 1, left.as_millis() as libc::c_int) } <= 0 {
            break;
        }
        let mut buf = [0u8; 256];
        match io::stdin().read(&mut buf) {
            Ok(n) if n > 0 => reply.extend_from_slice(&buf[..n]),
            _ => break,
        }
    }
    let _ = terminal::disable_raw_mode();
    parse_osc11(&reply).map(|(r, g, b)| 0.2126 * r + 0.7152 * g + 0.0722 * b > 0.5)
}

/// `ESC ] 11 ; rgb:RRRR/GGGG/BBBB` (1–4 hex digits each) → 0..1 components.
fn parse_osc11(reply: &[u8]) -> Option<(f64, f64, f64)> {
    let s = String::from_utf8_lossy(reply);
    let rgb = s.split("11;rgb:").nth(1)?;
    let mut it = rgb.split(['/', '\x1b', '\x07']).take(3).map(|h| {
        let h: String = h.chars().take_while(char::is_ascii_hexdigit).collect();
        let max = 16f64.powi(h.len() as i32) - 1.0;
        u32::from_str_radix(&h, 16).ok().map(|v| v as f64 / max)
    });
    Some((it.next()??, it.next()??, it.next()??))
}

fn restore() -> io::Result<()> {
    execute!(
        io::stdout(),
        DisableFocusChange,
        DisableMouseCapture,
        SetCursorStyle::DefaultUserShape,
        Show,
        LeaveAlternateScreen
    )?;
    terminal::disable_raw_mode()
}

fn event_loop(editor: &mut Editor) -> Result<()> {
    spawn_input(editor.events.sender());
    crate::config::watch(editor.events.sender());
    crate::git::watch(editor.events.sender());
    let mut out = io::stdout();
    while !editor.should_quit {
        let (w, h) = terminal::size()?;
        render(editor, &mut out, w, h)?;
        let Some(ev) = editor.events.recv() else { break };
        editor.handle_event(ev);
        // Process all queued events (pastes, key bursts, job results), then draw only once.
        while !editor.should_quit
            && let Some(ev) = editor.events.try_recv()
        {
            editor.handle_event(ev);
        }
    }
    Ok(())
}

/// Input thread — so terminal reads don't block the main loop.
fn spawn_input(tx: Sender<Event>) {
    thread::spawn(move || {
        while let Ok(ev) = event::read() {
            let ev = match ev {
                CtEvent::Key(k) if k.kind != KeyEventKind::Release => Key::from_event(k).map(Event::Key),
                CtEvent::Resize(..) => Some(Event::Resize),
                CtEvent::FocusGained => Some(Event::Focus(true)),
                CtEvent::FocusLost => Some(Event::Focus(false)),
                CtEvent::Mouse(m) => {
                    use crate::event::{Mouse, MouseKind};
                    let kind = match m.kind {
                        MouseEventKind::Down(MouseButton::Left) => Some(MouseKind::Down),
                        MouseEventKind::Drag(MouseButton::Left) => Some(MouseKind::Drag),
                        MouseEventKind::Up(MouseButton::Left) => Some(MouseKind::Up),
                        MouseEventKind::ScrollUp => Some(MouseKind::ScrollUp),
                        MouseEventKind::ScrollDown => Some(MouseKind::ScrollDown),
                        _ => None,
                    };
                    kind.map(|kind| {
                        Event::Mouse(Mouse {
                            kind,
                            x: m.column,
                            y: m.row,
                            alt: m.modifiers.contains(KeyModifiers::ALT),
                        })
                    })
                }
                _ => None,
            };
            if let Some(ev) = ev
                && tx.send(ev).is_err()
            {
                break;
            }
        }
    });
}

/// One frame: layout → scroll → draw. Callable without a terminal (performance budget test).
pub fn render(editor: &mut Editor, out: &mut impl Write, w: u16, h: u16) -> io::Result<()> {
    let lay = Layout::new(editor, w, h);
    let ui = Ui::new(editor);
    let offer = OfferCard::new(editor, &lay, &ui);
    // With split windows, scroll/viewport follow the focused pane
    let focus_lay = panes(editor, &lay).into_iter().find(|p| p.6).map_or_else(|| lay.clone(), |p| p.2);
    editor.viewport = (focus_lay.text_rows, focus_lay.text_cols);
    editor.screen = crate::editor::Screen {
        panes: Vec::new(),
        chat_x: (lay.chat_w > 0).then_some(lay.edit_w),
        picker_list: editor
            .picker
            .as_ref()
            .map(|p| (lay.text_top as usize + 3, PickerBox::new(&lay, p).list_rows)),
        offer_buttons: offer.as_ref().map(|c| c.buttons.clone()).unwrap_or_default(),
    };
    editor.refresh_search(focus_lay.text_rows);
    // During `/` preview the screen follows the match, not the cursor
    if editor.search_origin.is_none() {
        scroll_to_cursor(editor, &focus_lay);
    }
    if let Some(p) = editor.picker.as_mut() {
        let pb = PickerBox::new(&lay, p);
        p.view = p.visible(pb.list_rows);
        if pb.preview_w > 0 {
            request_preview(p, &editor.events.jobs());
        }
    }
    // Reviewing: put the current change's head at the top 1/4 of the screen (follow — off once j/k scroll)
    let mut review_rows = None;
    if let Some(rev) = editor.review.as_mut().filter(|r| r.ready)
        && let Some(doc) = editor.docs.iter().find(|d| d.id == rev.doc_id)
    {
        let rows = rev.rows(&doc.text);
        if rev.follow
            && let Some(h) = rows.iter().position(|r| *r == crate::llm::RRow::Header(rev.current))
        {
            rev.scroll = h.saturating_sub(lay.text_rows.saturating_sub(1) / 4);
        }
        rev.scroll = rev.scroll.min(rows.len().saturating_sub(1));
        review_rows = Some(rows);
    }
    // Per pane, screen row → doc line (for the mouse); the focused pane's also goes into editor.view
    let mut screen_panes = Vec::new();
    for (id, r, pl, idx, top, left, focused) in panes(editor, &lay) {
        let view = build_view(editor, &editor.docs[idx], top, left, &pl);
        screen_panes.push((id, r, pl.gutter, view.iter().map(|v| v.origin(left)).collect()));
        if focused {
            editor.view = view;
        }
    }
    editor.screen.panes = screen_panes;
    draw(editor, &lay, &ui, review_rows.as_deref(), offer.as_ref(), out)
}

#[derive(Clone)]
struct Layout {
    width: usize,
    /// Leftmost column of this pane (differs per pane when split).
    x0: usize,
    /// Editing area width (minus the chat panel on the right) · chat panel width (0 when closed).
    edit_w: usize,
    chat_w: usize,
    header: bool,
    /// Rightmost cell of the editing area = scrollbar (only when the doc is taller than the screen).
    scrollbar: bool,
    text_top: u16,
    text_rows: usize,
    gutter: usize,
    text_cols: usize,
    /// Debug panel height (0 without a session) — just below the editing area (text_rows); chat panel beside.
    debug_h: usize,
    status_y: u16,
    cmd_y: u16,
}

impl Layout {
    fn new(editor: &Editor, w: u16, h: u16) -> Self {
        let header = editor.config.header || editor.docs.len() > 1;
        let text_top = header as u16;
        let total = (h as usize).saturating_sub(2 + text_top as usize).max(1);
        let debug_h = if editor.dap.is_some() || editor.test_run.is_some() {
            (total / 3).clamp(7, 14).min(total.saturating_sub(4))
        } else {
            0
        };
        let text_rows = (total - debug_h).max(1);
        let digits = editor.doc().text.len_lines().to_string().len();
        // Docs with a language server or breakpoints get one sign column before the line numbers
        let doc = editor.doc();
        let signs = wants_signs(editor, doc);
        let gutter = digits.max(3) + 1 + signs as usize;
        let chat_w = chat_width(editor, w as usize);
        let scrollbar = editor.config.scrollbar && doc.text.len_lines() > total;
        let edit_w = w as usize - chat_w;
        Self {
            width: w as usize,
            x0: 0,
            edit_w,
            chat_w,
            header,
            scrollbar,
            text_top,
            text_rows,
            gutter,
            text_cols: edit_w.saturating_sub(gutter + scrollbar as usize).max(1),
            debug_h,
            status_y: text_top + total as u16,
            cmd_y: text_top + total as u16 + 1,
        }
    }
}

impl Layout {
    /// One split pane's layout: in its rect (below any title row), the doc's line-number column + scrollbar.
    fn pane(
        &self,
        doc: &crate::document::Document,
        r: crate::split::Rect,
        title: bool,
        editor: &Editor,
    ) -> Layout {
        let rows = r.h.saturating_sub(title as usize).max(1);
        let digits = doc.text.len_lines().to_string().len();
        let signs = wants_signs(editor, doc);
        let gutter = digits.max(3) + 1 + signs as usize;
        let scrollbar = editor.config.scrollbar && doc.text.len_lines() > rows;
        Layout {
            x0: r.x,
            edit_w: r.x + r.w,
            text_top: (r.y + title as usize) as u16,
            text_rows: rows,
            gutter,
            scrollbar,
            text_cols: r.w.saturating_sub(gutter + scrollbar as usize).max(1),
            ..self.clone()
        }
    }
}

/// Sign column before line numbers needed? language server · diagnostics · breakpoints · debug session.
fn wants_signs(editor: &Editor, doc: &crate::document::Document) -> bool {
    doc.lsp.client.is_some()
        || !doc.lsp.diagnostics.is_empty()
        || editor.dap.is_some()
        || doc.path.as_ref().is_some_and(|p| editor.breakpoints.get(p).is_some_and(|b| !b.is_empty()))
}

/// Panes: (view, rect, pane layout, doc index, top line, h-scroll, focused?). Left first (clearing to line
/// end wipes the pane to its right, so left goes first). One window = one pane as before (no title row).
#[allow(clippy::type_complexity)]
fn panes(
    editor: &Editor,
    lay: &Layout,
) -> Vec<(crate::split::ViewId, crate::split::Rect, Layout, usize, usize, usize, bool)> {
    let area = crate::split::Rect { x: 0, y: lay.text_top as usize, w: lay.edit_w, h: lay.text_rows };
    let many = editor.views.len() > 1;
    let mut out: Vec<_> = editor
        .split
        .layout(area)
        .into_iter()
        .filter_map(|(id, r)| {
            let v = editor.views.iter().find(|v| v.id == id)?;
            let focused = id == editor.focus;
            let idx =
                if focused { editor.current } else { editor.docs.iter().position(|d| d.id == v.doc)? };
            let doc = &editor.docs[idx];
            // Focused pane uses the doc's scroll (moved by scroll_to_cursor each frame); others their own —
            // clamped, since the doc may have shrunk under them (edited in another pane)
            let (top, left) =
                if focused { (doc.top, doc.left) } else { (v.top.min(mv::last_line(&doc.text)), v.left) };
            Some((id, r, lay.pane(doc, r, many, editor), idx, top, left, focused))
        })
        .collect();
    out.sort_by_key(|p| (p.1.x, p.1.y));
    out
}

/// Bottom debug panel: header row (status pill · what · key hints) + three columns — variables (colored
/// by shape) · call stack (current frame ▶) · output (stderr red). Card background sets it off from editing.
fn draw_debug_panel(
    d: &crate::dap::Dap,
    editor: &Editor,
    ui: &Ui,
    lay: &Layout,
    out: &mut impl Write,
) -> io::Result<()> {
    use crate::dap::State;
    let t = &editor.theme;
    let card = card_style(ui, editor, "ui.popup");
    let dim = Style { fg: ui.virt.fg, ..card };
    let faint = Style { fg: ui.linenr.fg, ..card };
    let strong = card.patch(t.try_get("ui.text.focus").unwrap_or(Style { bold: true, ..Style::default() }));
    let color = |k: &str| t.try_get(k).and_then(|s| s.fg);
    let y0 = lay.text_top as usize + lay.text_rows;
    let w = lay.edit_w;
    // ── Header row
    let (label, pill) = match &d.state {
        State::Building => ("BUILDING", ui.accent.fg),
        State::Starting => ("STARTING", ui.accent.fg),
        State::Running => ("RUNNING", color("diff.plus")),
        State::Stopped { .. } => ("PAUSED", color("warning")),
        State::Exited(_) => ("EXITED", ui.linenr.fg),
    };
    queue!(out, MoveTo(0, y0 as u16))?;
    let mut used = panel_pill(out, card, pill, label)?;
    let what = match &d.state {
        State::Building => format!("  {}  {}", d.program, wave(d.started)),
        State::Stopped { reason } => {
            let at = d
                .frames
                .first()
                .map(|f| {
                    let file =
                        f.path.as_ref().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned());
                    format!(" · {}:{}", file.unwrap_or_default(), f.line + 1)
                })
                .unwrap_or_default();
            format!("  {}  {reason}{at}", d.program)
        }
        State::Exited(Some(c)) => format!("  {}  exit code {c}", d.program),
        _ => format!("  {}", d.program),
    };
    let what = fit_ellipsis(&what, w.saturating_sub(used + 2));
    apply(out, strong)?;
    queue!(out, Print(&what))?;
    used += what.width();
    let hints: &[(&str, &str)] = match d.state {
        State::Stopped { .. } if d.request == "attach" => {
            &[("F5", "continue"), ("F10", "over"), ("F11", "into"), ("F12", "out"), ("space G t", "detach")]
        }
        State::Stopped { .. } => {
            &[("F5", "continue"), ("F10", "over"), ("F11", "into"), ("F12", "out"), ("space G t", "stop")]
        }
        State::Exited(_) => &[("F5", "restart"), ("space G t", "close")],
        // Attached session: only detach (the remote program keeps running)
        _ if d.request == "attach" => &[("space G p", "pause"), ("space G t", "detach")],
        _ => &[("space G p", "pause"), ("space G t", "stop")],
    };
    panel_hints(out, hints, used, w, card, dim, faint)?;
    // ── Three columns: variables 45% · stack 25% · output the rest
    let rows = lay.debug_h.saturating_sub(1);
    let cw = [w * 45 / 100, w * 25 / 100];
    let cols = [(0, cw[0]), (cw[0] + 1, cw[1]), (cw[0] + cw[1] + 2, w.saturating_sub(cw[0] + cw[1] + 2))];
    let var_scope = color("variable").or(ui.base.fg);
    let cwd = canonical_cwd();
    let fn_color = color("function");
    for r in 0..rows {
        let y = (y0 + 1 + r) as u16;
        queue!(out, MoveTo(0, y))?;
        apply(out, card)?;
        queue!(out, Print(" ".repeat(w)))?;
        for (c, &(x, cw)) in cols.iter().enumerate() {
            if c > 0 {
                apply(out, faint)?;
                queue!(out, MoveTo((x - 1) as u16, y), Print("│"))?;
            }
            queue!(out, MoveTo((x + 1) as u16, y))?;
            let inner = cw.saturating_sub(2);
            let nw = editor.watches.len();
            if r == 0 {
                apply(out, Style { bold: true, ..faint })?;
                let first = if nw > 0 { "WATCH" } else { "VARIABLES" };
                queue!(out, Print([first, "CALL STACK", "OUTPUT"][c]))?;
                continue;
            }
            let i = r - 1;
            match c {
                // Watches on top, if any — `◦ expr  value`; if unresolvable in this frame, the reason dimmed
                0 if i < nw => {
                    let expr = &editor.watches[i];
                    apply(out, Style { fg: ui.accent.fg, ..card })?;
                    queue!(out, Print("◦ "))?;
                    let name = fit_ellipsis(expr, inner.saturating_sub(2).min(24));
                    apply(out, Style { fg: var_scope, ..card })?;
                    queue!(out, Print(&name))?;
                    let rest = inner.saturating_sub(name.width() + 4);
                    apply(out, card)?;
                    queue!(out, Print("  "))?;
                    match d.watches.get(expr) {
                        Some(crate::dap::Watch::Value { value, ty }) => {
                            value_and_type(out, editor, ui, card, value, ty, rest)?
                        }
                        Some(crate::dap::Watch::Error(m)) => {
                            apply(out, Style { italic: true, ..faint })?;
                            queue!(out, Print(fit_ellipsis(m, rest)))?;
                        }
                        _ => {
                            apply(out, faint)?;
                            queue!(out, Print(if d.stopped() { "…" } else { "—" }))?;
                        }
                    }
                }
                0 if nw > 0 && i == nw => {
                    apply(out, Style { bold: true, ..faint })?;
                    queue!(out, Print("VARIABLES"))?;
                }
                0 => {
                    let i = if nw > 0 { i - nw - 1 } else { i };
                    let Some(v) = d.vars.get(i) else {
                        if i == 0 {
                            apply(out, faint)?;
                            let msg =
                                if d.stopped() { "no locals" } else { "run to a breakpoint (F9 on a line)" };
                            queue!(out, Print(fit(msg, inner)))?;
                        }
                        continue;
                    };
                    // Expandable values (structs, lists) get ▸
                    apply(out, faint)?;
                    queue!(out, Print(if v.reference > 0 { "▸ " } else { "  " }))?;
                    let name = fit(&v.name, inner.saturating_sub(2).min(18));
                    apply(out, Style { fg: var_scope, ..card })?;
                    queue!(out, Print(&name))?;
                    let rest = inner.saturating_sub(name.width() + 4);
                    apply(out, card)?;
                    queue!(out, Print("  "))?;
                    value_and_type(out, editor, ui, card, &v.value, &v.ty, rest)?;
                }
                1 => {
                    let Some(f) = d.frames.get(i) else { continue };
                    let current = i == 0;
                    // Only my code's frames (inside the working dir) are crisp — stdlib/runtime frames dimmed
                    let mine =
                        f.path.as_ref().is_some_and(|p| cwd.as_ref().is_some_and(|c| p.starts_with(c)));
                    apply(out, if current { Style { fg: color("warning"), ..card } } else { faint })?;
                    queue!(out, Print(if current { "▶ " } else { "  " }))?;
                    let file = f
                        .path
                        .as_ref()
                        .and_then(|p| p.file_name())
                        .map(|n| format!("{}:{}", n.to_string_lossy(), f.line + 1))
                        .unwrap_or_default();
                    let name =
                        fit_ellipsis(&f.name, inner.saturating_sub(2 + file.width().min(inner / 2) + 1));
                    apply(
                        out,
                        match (current, mine) {
                            (true, _) => Style { fg: fn_color, ..card },
                            (false, true) => card,
                            (false, false) => faint,
                        },
                    )?;
                    queue!(out, Print(&name))?;
                    let room = inner.saturating_sub(2 + name.width() + 1);
                    if room >= 4 {
                        apply(out, faint)?;
                        queue!(out, Print(" "), Print(fit_ellipsis(&file, room)))?;
                    }
                }
                _ => {
                    // Output fills from the bottom (so the latest is visible)
                    let lines = rows - 1;
                    let start = d.output.len().saturating_sub(lines);
                    let Some((cat, line)) = d.output.get(start + i) else { continue };
                    let st = match cat.as_str() {
                        "stderr" => Style { fg: ui.error.fg, ..card },
                        "stdout" => card,
                        _ => faint,
                    };
                    apply(out, st)?;
                    queue!(out, Print(fit(&line.replace('\t', "  "), inner)))?;
                }
            }
        }
    }
    Ok(())
}

/// Bottom panel header start: ` ▐ LABEL ▌` — a pill in `pill` color. Returns the cells used.
fn panel_pill(
    out: &mut impl Write,
    card: Style,
    pill: Option<crossterm::style::Color>,
    label: &str,
) -> io::Result<usize> {
    apply(out, card)?;
    queue!(out, Print(" "))?;
    let edge = Style { fg: pill, ..card };
    apply(out, edge)?;
    queue!(out, Print("▐"))?;
    apply(out, Style { fg: card.bg, bg: pill, bold: true, ..Style::default() })?;
    queue!(out, Print(format!(" {label} ")))?;
    apply(out, edge)?;
    queue!(out, Print("▌"))?;
    Ok(3 + label.width() + 2)
}

/// Rest of a bottom panel header (`used` cells drawn of `w`): key hints at the right end if they fit.
fn panel_hints(
    out: &mut impl Write,
    hints: &[(&str, &str)],
    mut used: usize,
    w: usize,
    card: Style,
    dim: Style,
    faint: Style,
) -> io::Result<()> {
    let hint_w: usize = hints.iter().map(|(k, v)| k.width() + v.width() + 3).sum();
    if used + hint_w + 2 <= w {
        apply(out, card)?;
        queue!(out, Print(" ".repeat(w - used - hint_w - 1)))?;
        for (k, v) in hints {
            apply(out, Style { bold: true, ..dim })?;
            queue!(out, Print(k))?;
            apply(out, faint)?;
            queue!(out, Print(format!(" {v}  ")))?;
        }
        used = w - 1;
    }
    apply(out, card)?;
    queue!(out, Print(" ".repeat(w.saturating_sub(used))))
}

/// Debug panel value colored by shape, its type faint at the right end of `rest` cells (dropped if no room).
fn value_and_type(
    out: &mut impl Write,
    editor: &Editor,
    ui: &Ui,
    card: Style,
    value: &str,
    ty: &str,
    rest: usize,
) -> io::Result<()> {
    let ty = if ty.is_empty() { String::new() } else { fit_ellipsis(ty, 14) };
    let val_room = rest.saturating_sub(if ty.is_empty() { 0 } else { ty.width() + 2 });
    let shown = fit_ellipsis(value, val_room);
    let vc = editor.theme.try_get(crate::dap::value_scope(value)).and_then(|s| s.fg).or(ui.base.fg);
    apply(out, Style { fg: vc, ..card })?;
    queue!(out, Print(&shown))?;
    if !ty.is_empty() && rest >= shown.width() + ty.width() + 2 {
        apply(out, Style { fg: ui.linenr.fg, ..card })?;
        queue!(out, Print(" ".repeat(rest - shown.width() - ty.width())), Print(&ty))?;
    }
    Ok(())
}

/// Test panel (in place of the debug panel):
///
/// ```text
///  FAILED  math::tests::*   ● 2  ▲ 1  ◦ 1   0.4s                 space x l again · space x d debug …
///  TESTS                          │ FAILURE  src/math.rs:21                          ]x next · [x prev
///  ● adds                         │ assertion `left == right` failed: 7 / 2 rounds down
/// ▎▲ divides                      │   left: 3
///  ◦ slow                         │  right: 4
/// ```
///
/// Until results can be read (building, compile error), the whole output. Shapes differ too
/// (● passed · ▲ failed · ◦ skipped).
fn draw_test_panel(
    r: &crate::testing::TestRun,
    editor: &Editor,
    ui: &Ui,
    lay: &Layout,
    out: &mut impl Write,
) -> io::Result<()> {
    use crate::test_results::Status;
    use crate::testing::RunState;
    let t = &editor.theme;
    let card = card_style(ui, editor, "ui.popup");
    let dim = Style { fg: ui.virt.fg, ..card };
    let faint = Style { fg: ui.linenr.fg, ..card };
    let strong = card.patch(t.try_get("ui.text.focus").unwrap_or(Style { bold: true, ..Style::default() }));
    let color = |k: &str| t.try_get(k).and_then(|s| s.fg);
    let (good, bad) = (color("diff.plus"), ui.error.fg);
    let mark = |st: Status| match st {
        Status::Passed => ("●", Style { fg: good, ..card }),
        Status::Failed => ("▲", Style { fg: bad, ..card }),
        Status::Skipped => ("◦", faint),
    };
    let y0 = lay.text_top as usize + lay.text_rows;
    let w = lay.edit_w;
    let count = |st: Status| r.cases.iter().filter(|c| c.status == st).count();
    let (passed, failed, skipped) = (count(Status::Passed), count(Status::Failed), count(Status::Skipped));
    // Judge by results (if the build tool exits non-zero but no test failed, the reason is in the output)
    let (label, pill) = match r.state {
        RunState::Running => ("RUNNING", ui.accent.fg),
        RunState::Passed => ("PASSED", good),
        RunState::Failed(_) => ("FAILED", bad),
    };
    // ── Header row
    queue!(out, MoveTo(0, y0 as u16))?;
    let mut used = panel_pill(out, card, pill, label)?;
    let name = fit_ellipsis(&format!("  {}", r.target.label), (w / 3).max(12));
    apply(out, strong)?;
    queue!(out, Print(&name))?;
    used += name.width();
    // Count summary · elapsed time (wave while running)
    let mut summary: Vec<(String, Style)> = Vec::new();
    for (n, st) in [(passed, Status::Passed), (failed, Status::Failed), (skipped, Status::Skipped)] {
        if n > 0 {
            let (g, s) = mark(st);
            summary.push((format!("   {g} "), s));
            summary.push((n.to_string(), Style { bold: true, ..card }));
        }
    }
    let dur = |s: f32| if s < 1.0 { format!("{:.0} ms", s * 1000.0) } else { format!("{s:.1}s") };
    let when = match (&r.state, r.took) {
        (RunState::Running, _) => format!("   {}", wave(r.started)),
        (_, Some(d)) => format!("   {}", dur(d.as_secs_f32())),
        _ => String::new(),
    };
    summary.push((when, dim));
    let sum_w: usize = summary.iter().map(|(s, _)| s.width()).sum();
    if used + sum_w + 2 < w {
        for (s, st) in &summary {
            apply(out, *st)?;
            queue!(out, Print(s))?;
        }
        used += sum_w;
    }
    let hints: &[(&str, &str)] = match r.state {
        RunState::Running => &[("space x c", "stop")],
        _ => &[("space x l", "again"), ("space x d", "debug"), ("space x c", "close")],
    };
    panel_hints(out, hints, used, w, card, dim, faint)?;

    let rows = lay.debug_h.saturating_sub(1);
    let line_style = |err: bool, l: &str| {
        let l = l.trim_start();
        let passing = l.starts_with("test result: ok")
            || l.starts_with("BUILD SUCCESS")
            || (l.starts_with("test ") && l.ends_with(" ok"))
            || l.starts_with("--- PASS")
            || l.starts_with("ok ")
            || l == "PASS"
            || (l.starts_with("===") && l.contains(" passed") && !l.contains(" failed"));
        let failing = !passing
            && (l.contains("FAILED")
                || l.contains("panicked at")
                || l.starts_with("--- FAIL")
                || l.starts_with("FAIL")
                || l.starts_with("E ")
                || l.starts_with("error")
                || (l.contains(" failed") && !l.contains(" 0 failed")));
        if passing {
            Style { fg: good, ..card }
        } else if failing {
            Style { fg: bad, ..card }
        } else if err {
            faint
        } else {
            card
        }
    };
    // No results yet — the whole output (from the bottom)
    if r.cases.is_empty() {
        let start = r.output.len().saturating_sub(rows);
        for i in 0..rows {
            let y = (y0 + 1 + i) as u16;
            queue!(out, MoveTo(0, y))?;
            apply(out, card)?;
            queue!(out, Print(" ".repeat(w)), MoveTo(2, y))?;
            match r.output.get(start + i) {
                Some((err, line)) => {
                    apply(out, line_style(*err, line))?;
                    queue!(out, Print(fit(&line.replace('\t', "  "), w.saturating_sub(4))))?;
                }
                None if i == 0 && r.output.is_empty() => {
                    apply(out, faint)?;
                    queue!(out, Print(fit(&r.target.argv().join(" "), w.saturating_sub(4))))?;
                }
                None => {}
            }
        }
        return Ok(());
    }
    // ── Two columns: result list · selected failure (or output if none)
    // List names drop the shared prefix (`math::tests::adds` → `adds` — the full name is in the header)
    let short = short_names(r.cases.iter().map(|c| c.name.as_str()));
    let name_w = short.iter().map(|n| n.width()).max().unwrap_or(10);
    let left_w = (name_w + 14).clamp(26, (w * 45 / 100).max(26)).min(w.saturating_sub(20));
    let right_x = left_w + 1;
    let right_w = w.saturating_sub(right_x + 1);
    let selected = r.cases.get(r.selected).filter(|c| c.status == Status::Failed);
    let list_rows = rows.saturating_sub(1);
    // Keep the selection visible; while running, keep the latest visible
    let first = if r.state == RunState::Running {
        r.cases.len().saturating_sub(list_rows)
    } else {
        r.selected.saturating_sub(list_rows.saturating_sub(1))
    };
    let cwd = &r.target.cwd;
    let rel = |p: &std::path::Path| p.strip_prefix(cwd).unwrap_or(p).display().to_string();
    let fails = r.failures();
    // Right column's lines
    let right: Vec<(String, Style)> = match selected {
        Some(c) => {
            let mut v: Vec<(String, Style)> = Vec::new();
            for (i, m) in c.message.iter().enumerate() {
                let st = if i == 0 { Style { fg: bad, ..card } } else { card };
                v.push((m.replace('\t', "  "), st));
            }
            if v.is_empty() {
                v.push(("failed (no message)".into(), faint));
            }
            v
        }
        None => {
            let n = list_rows;
            let start = r.output.len().saturating_sub(n);
            r.output[start..].iter().map(|(e, l)| (l.replace('\t', "  "), line_style(*e, l))).collect()
        }
    };
    for i in 0..rows {
        let y = (y0 + 1 + i) as u16;
        queue!(out, MoveTo(0, y))?;
        apply(out, card)?;
        queue!(out, Print(" ".repeat(w)))?;
        apply(out, faint)?;
        queue!(out, MoveTo(left_w as u16, y), Print("│"))?;
        if i == 0 {
            // Column title
            apply(out, Style { bold: true, ..faint })?;
            queue!(out, MoveTo(2, y), Print("TESTS"))?;
            queue!(out, MoveTo((right_x + 1) as u16, y))?;
            match selected {
                Some(c) => {
                    queue!(out, Print("FAILURE"))?;
                    if let Some((p, l)) = &c.at {
                        apply(out, faint)?;
                        queue!(out, Print(format!("  {}:{}", rel(p), l + 1)))?;
                    }
                    if fails.len() > 1 {
                        let h = format!(
                            "]x next · [x prev  {}/{}",
                            fails.iter().position(|&f| f == r.selected).unwrap_or(0) + 1,
                            fails.len()
                        );
                        if right_w > h.width() + 30 {
                            apply(out, faint)?;
                            queue!(out, MoveTo((w - h.width() - 2) as u16, y), Print(h))?;
                        }
                    }
                }
                None => queue!(out, Print("OUTPUT"))?,
            }
            continue;
        }
        // List
        if let Some(c) = r.cases.get(first + i - 1) {
            let idx = first + i - 1;
            let is_sel = selected.is_some() && idx == r.selected;
            let row_bg = if is_sel {
                Style { bg: blend(ui.accent.fg, card.bg, 0.14).or(card.bg), ..card }
            } else {
                card
            };
            queue!(out, MoveTo(0, y))?;
            apply(out, if is_sel { Style { fg: ui.accent.fg, ..row_bg } } else { row_bg })?;
            queue!(out, Print(if is_sel { "▎" } else { " " }))?;
            let (g, st) = mark(c.status);
            apply(out, Style { bg: row_bg.bg, ..st })?;
            queue!(out, Print(format!(" {g} ")))?;
            let secs = c.secs.filter(|s| *s >= 0.001).map(dur).unwrap_or_default();
            let room = left_w.saturating_sub(5 + secs.width() + 2);
            let nm = fit_ellipsis(&short[idx], room);
            apply(
                out,
                if c.status == Status::Failed || is_sel {
                    Style { bold: is_sel, ..row_bg }
                } else {
                    Style { fg: dim.fg, ..row_bg }
                },
            )?;
            queue!(out, Print(&nm))?;
            apply(out, Style { fg: faint.fg, ..row_bg })?;
            queue!(
                out,
                Print(" ".repeat(left_w.saturating_sub(4 + nm.width() + secs.width() + 1))),
                Print(&secs),
                Print(" ")
            )?;
        }
        // Right
        if let Some((text, st)) = right.get(i - 1) {
            apply(out, *st)?;
            queue!(out, MoveTo((right_x + 1) as u16, y), Print(fit(text, right_w.saturating_sub(1))))?;
        }
    }
    Ok(())
}

/// The working directory, canonical (frame paths are) — `canonicalize` hits the disk, so only once per cwd.
fn canonical_cwd() -> Option<std::path::PathBuf> {
    static CACHE: std::sync::Mutex<Option<(std::path::PathBuf, Option<std::path::PathBuf>)>> =
        std::sync::Mutex::new(None);
    let cwd = std::env::current_dir().ok()?;
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    match &*cache {
        Some((at, canon)) if *at == cwd => canon.clone(),
        _ => {
            let canon = std::fs::canonicalize(&cwd).ok();
            *cache = Some((cwd, canon.clone()));
            canon
        }
    }
}

/// Strip test names' shared prefix (up to a `::` · `.` · `/` boundary) — if only one, keep the last segment.
fn short_names<'a>(names: impl Iterator<Item = &'a str> + Clone) -> Vec<String> {
    let seps = |s: &str| {
        s.char_indices()
            .filter(|(_, c)| matches!(c, ':' | '.' | '/'))
            .map(|(i, c)| i + c.len_utf8())
            .collect::<Vec<_>>()
    };
    let list: Vec<&str> = names.collect();
    let cut = match list.as_slice() {
        [] => 0,
        [one] => seps(one).last().copied().unwrap_or(0),
        [first, rest @ ..] => seps(first)
            .into_iter()
            .rev()
            .find(|&i| rest.iter().all(|n| n.len() > i && n.as_bytes()[..i] == first.as_bytes()[..i]))
            .unwrap_or(0),
    };
    list.iter().map(|n| n[cut.min(n.len())..].to_string()).collect()
}

/// Draws the panes (one if single window). Several get a title row each and dividers between adjacent ones.
/// Returns the focused pane's cursor position.
fn draw_panes(
    editor: &Editor,
    ui: &Ui,
    lay: &Layout,
    out: &mut impl Write,
) -> io::Result<Option<(u16, u16)>> {
    let ps = panes(editor, lay);
    let many = ps.len() > 1;
    let mut cursor = None;
    for (_, r, pl, idx, top, left, focused) in &ps {
        let doc = &editor.docs[*idx];
        let own;
        let view = if *focused {
            &editor.view
        } else {
            own = build_view(editor, doc, *top, *left, pl);
            &own
        };
        if many {
            draw_pane_title(editor, doc, *r, *focused, ui, out)?;
        }
        let c = draw_text(editor, doc, *top, *left, *focused, view, ui, pl, out)?;
        if *focused {
            cursor = c;
        }
    }
    // Dividers between side-by-side panes (plain line at title-row height, no ┬) — last (after left clears)
    let faint = Style { fg: ui.linenr.fg, ..ui.base };
    for (_, r, ..) in ps.iter().filter(|p| p.1.x > 0) {
        apply(out, faint)?;
        for y in r.y..r.y + r.h {
            queue!(out, MoveTo((r.x - 1) as u16, y as u16), Print("│"))?;
        }
    }
    Ok(cursor)
}

/// Pane title row: `▎ main.rs ●` — focused pane gets accent bar, bold name, faint band; others dimmed.
fn draw_pane_title(
    editor: &Editor,
    doc: &crate::document::Document,
    r: crate::split::Rect,
    focused: bool,
    ui: &Ui,
    out: &mut impl Write,
) -> io::Result<()> {
    let t = &editor.theme;
    let band = if focused { blend(ui.accent.fg, ui.tint, 0.10).or(ui.base.bg) } else { ui.base.bg };
    let base = Style { bg: band, ..ui.base };
    queue!(out, MoveTo(r.x as u16, r.y as u16))?;
    apply(
        out,
        if focused { Style { fg: ui.accent.fg, ..base } } else { Style { fg: ui.linenr.fg, ..base } },
    )?;
    queue!(out, Print(" ▎ "))?;
    let name = fit_ellipsis(&doc.display_name(), r.w.saturating_sub(8));
    let name_style = if focused {
        base.patch(t.try_get("ui.text.focus").unwrap_or(Style { bold: true, ..Style::default() }))
    } else {
        Style { fg: ui.virt.fg, ..base }
    };
    apply(out, Style { bg: band, ..name_style })?;
    queue!(out, Print(&name))?;
    let mut used = 3 + name.width();
    if doc.is_modified() {
        apply(out, Style { fg: ui.accent.fg, ..base })?;
        queue!(out, Print(" ●"))?;
        used += 2;
    }
    apply(out, base)?;
    queue!(out, Print(" ".repeat(r.w.saturating_sub(used))))
}

/// Chat panel width: 2/5 of the screen (36–72 cells). If editing area < 40 cells, it takes the whole screen.
fn chat_width(editor: &Editor, w: usize) -> usize {
    if editor.chat.is_none() {
        return 0;
    }
    let c = (w * 2 / 5).clamp(36, 72);
    if w < c + 40 { w } else { c }
}

/// Line-number column width (excluding the diagnostic sign column).
fn num_w(doc: &crate::document::Document) -> usize {
    doc.text.len_lines().to_string().len().max(3) + 1
}

/// Cursor char to draw: in insert mode the head (between chars), otherwise the block cursor char.
fn cursor_idx(mode: Mode, r: Range, text: &ropey::Rope) -> usize {
    if mode == Mode::Insert { r.head } else { r.cursor(text) }
}

fn scroll_to_cursor(editor: &mut Editor, lay: &Layout) {
    let so = editor.config.scrolloff.min(lay.text_rows.saturating_sub(1) / 2);
    let tab = editor.config.tab_width;
    let mode = editor.mode;
    let wrap_w = editor.wraps(editor.doc()).then_some(lay.text_cols);
    let doc = editor.doc_mut();
    let pos = cursor_idx(mode, doc.selection().primary(), &doc.text);
    let line = mv::line_of(&doc.text, pos);
    if let Some(w) = wrap_w {
        // Screen rows, not lines: `so` rows kept above and below the cursor's row
        let rows = |l: usize| crate::wrap::rows(&doc.text, l, w, tab);
        let here = rows(line);
        let k = crate::wrap::row_of(&here, pos);
        let mut above = k; // rows between the top of the screen and the cursor's row
        let mut top = line;
        while top > 0 && above < so {
            top -= 1;
            above += rows(top).len();
        }
        if doc.top > top {
            doc.top = top;
        } else {
            // Each line is at least a row — a top more than a screen above can't show the cursor
            if line >= doc.top + lay.text_rows {
                doc.top = line + 1 - lay.text_rows;
            }
            // Rows from the top through the cursor's, plus `so` below, must fit
            let mut used: usize = (doc.top..line).map(|l| rows(l).len()).sum::<usize>() + k + 1;
            while doc.top < line && used + so > lay.text_rows {
                used -= rows(doc.top).len();
                doc.top += 1;
            }
        }
        // Wrapped lines never scroll sideways (a line too long to wrap still does)
        if here.len() > 1
            || mv::line_end(&doc.text, line) - mv::line_start(&doc.text, line) <= crate::wrap::MAX_LINE
        {
            doc.left = 0;
            return;
        }
    } else if line < doc.top + so {
        doc.top = line.saturating_sub(so);
    } else if line + so >= doc.top + lay.text_rows {
        doc.top = line + so + 1 - lay.text_rows;
    }
    let col = mv::visual_col(&doc.text, pos, tab);
    // The whole cursor cell must fit (a wide char at the right edge would be clipped, hiding the cursor)
    let g = doc.text.byte_slice(pos..graphemes::next_boundary(&doc.text, pos)).to_string();
    let end = col + graphemes::cluster_width(&g, col, tab.max(1)).max(1);
    if col < doc.left {
        doc.left = col;
    } else if end > doc.left + lay.text_cols {
        doc.left = end.saturating_sub(lay.text_cols).min(col);
    }
}

// ── Theme → this frame's UI styles ─────────────────────────────────────────

/// UI keys the theme lacks use built-in values — so a partial user theme never hides the selection or cursor.
fn builtin() -> &'static Theme {
    static B: OnceLock<Theme> = OnceLock::new();
    B.get_or_init(Theme::builtin)
}

#[derive(Clone)]
struct Ui {
    base: Style,
    /// Base bg for blending faint colors — usually the bg; for transparent themes the original (`ui.tint`).
    /// Painting uses `base.bg` (terminal bg if none).
    tint: Option<crossterm::style::Color>,
    selection: Style,
    selection_primary: Style,
    cursor: Style,
    cursor_primary: Style,
    cursorline: Style,
    linenr: Style,
    linenr_selected: Style,
    status: Style,
    status_mode: Style,
    virt: Style,
    error: Style,
    diff_plus: Style,
    diff_minus: Style,
    /// Accent color (ui.accent → primary cursor bg → special) — selected-row bar, typed chars, mode pill…
    accent: Style,
    /// Diagnostics [_, error, warning, info, hint]: underline style, sign (●) color
    diag: [Style; 5],
    sign: [Style; 5],
    /// Set while drawing the focused pane: the cursor line's diagnostics all fit at its end (no card needed).
    cursor_diags_shown: std::cell::Cell<bool>,
}

impl Ui {
    fn new(editor: &Editor) -> Self {
        let t = &editor.theme;
        let get = |k: &str| t.try_get(k).or_else(|| builtin().try_get(k)).unwrap_or_default();
        let mode = match editor.mode {
            Mode::Normal => "normal",
            Mode::Insert => "insert",
            Mode::Select => "select",
        };
        let base = t.get("ui.background").patch(get("ui.text"));
        let tint = t.try_get("ui.tint").and_then(|s| s.bg).or(base.bg);
        let status = base.patch(get("ui.statusline"));
        let selection = base.patch(get("ui.selection"));
        let cursor = base.patch(get("ui.cursor"));
        Ui {
            tint,
            selection_primary: t.try_get("ui.selection.primary").map_or(selection, |s| base.patch(s)),
            cursor_primary: t
                .try_get(&format!("ui.cursor.primary.{mode}"))
                .or_else(|| t.try_get("ui.cursor.primary"))
                .map_or(cursor, |s| base.patch(s)),
            cursorline: base.patch(get("ui.cursorline.primary")),
            linenr: base.patch(get("ui.linenr")),
            // Current line number in the mode color too (same as the pill — the mode shows where the eye is)
            linenr_selected: {
                let sel = base.patch(get("ui.linenr.selected"));
                match t.try_get(&format!("ui.statusline.{mode}")).and_then(|s| s.bg) {
                    Some(c) if editor.config.color_modes && mode != "normal" => Style { fg: Some(c), ..sel },
                    _ => sel,
                }
            },
            status_mode: if editor.config.color_modes {
                status.patch(get(&format!("ui.statusline.{mode}")))
            } else {
                Style { bold: true, ..status }
            },
            accent: Style {
                fg: t
                    .try_get("ui.accent")
                    .and_then(|s| s.fg)
                    .or_else(|| t.try_get("ui.cursor.primary").and_then(|s| s.bg))
                    .or_else(|| t.try_get("special").and_then(|s| s.fg))
                    .or_else(|| builtin().try_get("ui.accent").and_then(|s| s.fg)),
                ..Style::default()
            },
            virt: base.patch(get("ui.virtual")),
            error: base.patch(get("error")),
            diff_plus: base.patch(get("diff.plus")),
            diff_minus: base.patch(get("diff.minus")),
            diag: ["", "error", "warning", "info", "hint"].map(|k| get(&format!("diagnostic.{k}"))),
            sign: ["", "error", "warning", "info", "hint"]
                .map(|k| Style { fg: get(k).fg, ..Style::default() }),
            cursor_diags_shown: std::cell::Cell::new(false),
            base,
            selection,
            cursor,
            status,
        }
    }
}

/// Apply a style wholesale (clearing the previous one).
fn apply(out: &mut impl Write, s: Style) -> io::Result<()> {
    queue!(out, SetAttribute(Attribute::Reset), ResetColor)?;
    if let Some(c) = s.fg {
        queue!(out, SetForegroundColor(c))?;
    }
    if let Some(c) = s.bg {
        queue!(out, SetBackgroundColor(c))?;
    }
    let fancy = undercurl_supported();
    if fancy && let Some(c) = s.ul_color {
        queue!(out, SetUnderlineColor(c))?;
    }
    let curly = s.curly && fancy;
    for (on, a) in [
        (s.bold, Attribute::Bold),
        (s.italic, Attribute::Italic),
        (s.underline && !curly, Attribute::Underlined),
        (s.underline && curly, Attribute::Undercurled),
        (s.dim, Attribute::Dim),
        (s.reversed, Attribute::Reverse),
        (s.crossed, Attribute::CrossedOut),
    ] {
        if on {
            queue!(out, SetAttribute(a))?;
        }
    }
    Ok(())
}

/// Does the terminal grok undercurl/underline color (`CSI 4:3 m`, `CSI 58…`)? Unknown terminals print them
/// as text ("3m") — so only where certain, plain underline elsewhere. Force with `TARAE_UNDERCURL=1/0`.
fn undercurl_supported() -> bool {
    static S: OnceLock<bool> = OnceLock::new();
    *S.get_or_init(|| {
        let env = |k: &str| std::env::var(k).unwrap_or_default();
        match env("TARAE_UNDERCURL").as_str() {
            "1" => return true,
            "0" => return false,
            _ => {}
        }
        let (term, prog) = (env("TERM"), env("TERM_PROGRAM"));
        ["kitty", "wezterm", "ghostty", "alacritty", "foot", "contour"].iter().any(|t| term.contains(t))
            || ["WezTerm", "ghostty", "iTerm.app", "vscode", "kitty"].contains(&prog.as_str())
            || std::env::var_os("ZELLIJ").is_some()
    })
}

/// Clear the rest of the line with the default style (background).
fn clear_rest(out: &mut impl Write, base: Style) -> io::Result<()> {
    apply(out, base)?;
    queue!(out, Clear(ClearType::UntilNewLine))
}

fn cursor_shape(editor: &Editor) -> CursorShape {
    let (n, i, s) = editor.config.cursor_shape;
    match editor.mode {
        Mode::Normal => n,
        Mode::Insert => i,
        Mode::Select => s,
    }
}

/// `review_rows` = the ready review's rows, `offer` = the front offer card (both worked out by `render`).
fn draw(
    editor: &Editor,
    lay: &Layout,
    ui: &Ui,
    review_rows: Option<&[crate::llm::RRow]>,
    offer: Option<&OfferCard>,
    out: &mut impl Write,
) -> io::Result<()> {
    queue!(out, BeginSynchronizedUpdate, Hide)?;
    if lay.header {
        draw_header(editor, ui, lay, out)?;
    }
    // Narrow screen with the chat open: the chat takes it all — no text, nothing floating over it
    let text_cursor = match editor.review.as_ref().filter(|r| r.ready) {
        _ if lay.edit_w == 0 => None,
        Some(rev) => {
            if let Some(rows) = review_rows {
                draw_review(rev, rows, editor, ui, lay, out)?;
            }
            None
        }
        None if editor.welcome() => {
            draw_welcome(editor, ui, lay, out)?;
            None
        }
        None => draw_panes(editor, ui, lay, out)?,
    };
    if lay.debug_h > 0 && lay.edit_w > 0 {
        if let Some(d) = &editor.dap {
            draw_debug_panel(d, editor, ui, lay, out)?;
        } else if let Some(r) = &editor.test_run {
            draw_test_panel(r, editor, ui, lay, out)?;
        }
    }
    let chat_cursor = match &editor.chat {
        Some(c) if lay.chat_w > 0 => draw_chat(c, editor, ui, lay, out)?,
        _ => None,
    };
    if let Some(rev) = editor.review.as_ref().filter(|r| !r.ready) {
        draw_ask_stream(rev, editor, ui, lay, out)?;
    }
    if let (Some(lines), Some(at)) = (&editor.popup, text_cursor) {
        draw_popup(lines, at, ui, editor, lay, out)?;
    }
    if let (Some(sig), Some(at)) = (&editor.signature, text_cursor) {
        draw_signature(sig, at, ui, editor, lay, out)?;
    }
    if let (Some(c), Some(at)) = (&editor.completion, text_cursor) {
        draw_completion(c, at, ui, editor, lay, out)?;
    }
    if let Some(at) = text_cursor {
        draw_diagnostic_card(editor, at, ui, lay, out)?;
    }
    draw_toasts(editor, ui, lay, out)?;
    if let Some(card) = offer {
        card.draw(editor, ui, out)?;
    }
    if !editor.pending.is_empty() && editor.picker.is_none() {
        draw_which_key(editor, ui, lay, out)?;
    }
    let picker_cursor = match &editor.picker {
        Some(p) => Some(draw_picker(p, editor, ui, lay, out)?),
        None => None,
    };
    draw_statusline(editor, ui, lay, out)?;
    let completion = editor.cmdline_completion();
    draw_cmdline_menu(editor, completion.as_ref(), ui, lay, out)?;
    let prompt_cursor =
        draw_cmdline(editor, completion.as_ref(), ui, lay, out)?.or(picker_cursor).or(chat_cursor);

    let (pos, shape) = match prompt_cursor {
        Some(p) => (Some(p), CursorShape::Bar),
        None => (text_cursor, cursor_shape(editor)),
    };
    if let Some((x, y)) = pos {
        let style = match shape {
            CursorShape::Block => SetCursorStyle::SteadyBlock,
            CursorShape::Bar => SetCursorStyle::SteadyBar,
            CursorShape::Underline => SetCursorStyle::SteadyUnderScore,
        };
        queue!(out, MoveTo(x, y), style, Show)?;
    }
    queue!(out, SetAttribute(Attribute::Reset), ResetColor, EndSynchronizedUpdate)?;
    out.flush()
}

/// Draw the text area and return the primary cursor's screen position.
#[allow(clippy::too_many_arguments)]
fn draw_text(
    editor: &Editor,
    doc: &crate::document::Document,
    top: usize,
    left: usize,
    focused: bool,
    view: &[crate::doccomment::ViewRow],
    ui: &Ui,
    lay: &Layout,
    out: &mut impl Write,
) -> io::Result<Option<(u16, u16)>> {
    // The frame's flag, not the copy's below
    let cursor_diags_shown = &ui.cursor_diags_shown;
    // Unfocused panes are toned down a step (dim attribute — set on the line bg, all text inherits it)
    let ui = &Ui {
        base: Style { dim: !focused, ..ui.base },
        linenr: Style { dim: !focused, ..ui.linenr },
        ..ui.clone()
    };
    let text = &doc.text;
    let len = text.len_bytes();
    let tab = editor.config.tab_width;
    let sel = doc.selection();
    let ranges = sel.ranges();
    // Cursor position once per frame (per cell, grapheme math would run cells × ranges times).
    let cursors: Vec<usize> = ranges.iter().map(|&r| cursor_idx(editor.mode, r, text)).collect();
    let primary_idx = sel.primary_index();
    let primary = cursors[primary_idx];
    let cur_line = mv::line_of(text, primary);
    // The phantom line (after the final newline) shows only when the cursor is there.
    let max_line = mv::last_line(text).max(cur_line);
    // Screen row → doc line (not 1:1: folded doc-comment blocks). Per-line tables index by "Nth row from top"
    let span = view.iter().map(|v| v.last_line()).max().map_or(1, |m| m + 1 - top.min(m)).max(1);
    let paint_primary = focused && editor.mode != Mode::Insert && cursor_shape(editor) == CursorShape::Block;
    // Syntax highlight: capture name → theme style once per frame (hundreds of lookups, µs).
    let syntax = doc.syntax.as_ref().filter(|s| s.tree.is_some());
    let capture_styles: Vec<Option<Style>> =
        if syntax.is_some() { syntax::capture_styles(|n| editor.theme.try_get(n)) } else { Vec::new() };
    let mut qcursor = QueryCursor::new();
    let mut cursor = None;
    // Only diagnostics on visible lines (so cells don't scan all) + the most severe per line (sign column)
    let last_row_line = (top + span).min(max_line + 1);
    let (vis_from, vis_to) =
        (mv::line_start(text, top.min(max_line)), mv::line_full_end(text, last_row_line.max(1) - 1));
    let visible_diags: Vec<&crate::lsp::Diagnostic> =
        doc.lsp.diagnostics.iter().filter(|d| d.to >= vis_from && d.from <= vis_to).collect();
    let mut line_severity = vec![0u8; span];
    // End-of-line message (Error Lens): 1st line of the most severe diagnostic starting on that line + N more
    // + whether that line is all of its message
    let mut line_message: Vec<Option<(u8, &str, usize, bool)>> = vec![None; span];
    if editor.config.inline_diagnostics {
        for d in &visible_diags {
            let Some(slot) = mv::line_of(text, d.from).checked_sub(top).and_then(|r| line_message.get_mut(r))
            else {
                continue;
            };
            let msg = d.message.lines().next().unwrap_or_default();
            let whole =
                d.message.lines().filter(|l| !l.trim().is_empty() && !lint_origin(l)).nth(1).is_none();
            *slot = match *slot {
                None => Some((d.severity, msg, 0, whole)),
                Some((s, _, n, _)) if d.severity < s => Some((d.severity, msg, n + 1, whole)),
                Some((s, m, n, w)) => Some((s, m, n + 1, w)),
            };
        }
    }
    // Failed test sites: same ▲ as the results panel + first message line (ahead of diagnostics — just run)
    let mut test_fail = vec![false; span];
    if let (Some(r), Some(p)) = (&editor.test_run, doc.path.as_deref()) {
        for c in r.cases.iter().filter(|c| c.status == crate::test_results::Status::Failed) {
            let Some((fp, l)) = &c.at else { continue };
            if fp.as_path() != p {
                continue;
            }
            if let Some(row) = l.checked_sub(top).filter(|&row| row < span) {
                let msg = c.message.iter().map(|m| m.trim()).find(|m| !m.is_empty()).unwrap_or("test failed");
                line_message[row] = Some((1, msg, 0, false));
                test_fail[row] = true;
            }
        }
    }
    for d in &visible_diags {
        let row = mv::line_of(text, d.from).wrapping_sub(top);
        if let Some(s) = line_severity.get_mut(row)
            && (*s == 0 || d.severity < *s)
        {
            *s = d.severity;
        }
    }
    // Query the whole visible area once (instead of 50 times per line). If the area is abnormally large
    // (very long lines), split per line — then the loop below queries each line chunk separately.
    let whole: Option<Vec<(usize, usize, u32)>> = syntax
        .filter(|_| vis_to - vis_from <= 1 << 20)
        .map(|syn| syntax::highlights(syn, text, vis_from..vis_to, &mut qcursor));

    // Inlay hints: visible range only, off while h-scrolled (so virtual text and scroll column don't drift)
    let inlay_style = ui.base.patch(
        editor.theme.try_get("ui.virtual.inlay-hint").unwrap_or(Style { fg: ui.virt.fg, ..Style::default() }),
    );
    let wrapping = editor.wraps(doc);
    let hints: &[crate::lsp::InlayHint] = if editor.config.inlay_hints && left == 0 {
        let h = &doc.lsp.inlay;
        &h[h.partition_point(|x| x.pos < vis_from)..h.partition_point(|x| x.pos <= vis_to)]
    } else {
        &[]
    };
    // Debugger: this doc's breakpoints · stopped line · variable values to append at line end
    let doc_bps = doc.path.as_ref().and_then(|p| editor.breakpoints.get(p));
    let stop_color = editor.theme.try_get("warning").and_then(|s| s.fg).or(ui.accent.fg);
    let stop_line = editor
        .dap
        .as_ref()
        .and_then(|d| d.stop_at())
        .filter(|(p, _)| doc.path.as_deref() == Some(*p))
        .map(|(_, l)| l);
    let inline_vals = match (stop_line, editor.dap.as_ref()) {
        (Some(l), Some(d)) => crate::dap::inline_values(text, l, &d.vars),
        _ => Default::default(),
    };
    // Search matches (visible span only) — faint accent background, the currently selected match stronger
    let accent_bg = |t: f32| blend(ui.accent.fg, ui.tint, t);
    let hit_style = Style { bg: accent_bg(0.22), ..Style::default() };
    let hit_current = Style { bg: accent_bg(0.5), bold: true, ..Style::default() };
    let visible_hits: &[(usize, usize)] = match &editor.search_hits {
        Some(h) if hit_style.bg.is_some() => {
            let m = &h.matches;
            &m[m.partition_point(|x| x.1 <= vis_from)..m.partition_point(|x| x.0 <= vis_to)]
        }
        _ => &[],
    };
    let primary_range = sel.primary();
    // `gw` labels: label char per position (after the first key, only the matching labels' second char);
    // the rest of the text steps back so they stand out
    let mut label_cells: std::collections::HashMap<usize, char> = std::collections::HashMap::new();
    let labels_on = match editor.jump_labels.as_ref().filter(|l| focused && l.doc == doc.id) {
        Some(l) => {
            for (i, &(s, _)) in l.targets.iter().enumerate() {
                let (a, b) = crate::labels::Labels::label(i);
                match l.typed {
                    None => {
                        label_cells.insert(s, a);
                        label_cells.insert(graphemes::next_boundary(text, s), b);
                    }
                    Some(t) if t == a => {
                        label_cells.insert(s, b);
                    }
                    Some(_) => {}
                }
            }
            true
        }
        None => false,
    };
    let label_style =
        Style { fg: ui.accent.fg, bg: blend(ui.accent.fg, ui.tint, 0.16), bold: true, ..Style::default() };
    // git: sign per visible line (glyph, color)
    let git_color = |k: &str| Style { fg: editor.theme.try_get(k).and_then(|s| s.fg), ..ui.base };
    let mut git_marks: Vec<Option<(&str, Style)>> = vec![None; span];
    for h in &doc.git_hunks {
        let (glyph, st, lines) = match h.kind {
            crate::git::HunkKind::Added => ("▎", git_color("diff.plus"), h.lines.clone()),
            crate::git::HunkKind::Modified => ("▎", git_color("diff.delta"), h.lines.clone()),
            // Deleted spots get an underline under the preceding line
            crate::git::HunkKind::Deleted => {
                let l = h.lines.start.saturating_sub(1);
                ("▁", git_color("diff.minus"), l..l + 1)
            }
        };
        for l in lines {
            if let Some(m) = l.checked_sub(top).and_then(|r| git_marks.get_mut(r)) {
                *m = Some((glyph, st));
            }
        }
    }
    // Indent guides: per line, the depth to draw guides to (blank lines take the shallower of the nearest
    // lines above/below — so guides don't break inside a block)
    let unit = tab.max(1);
    let guide_style = editor
        .theme
        .try_get("ui.virtual.indent-guide")
        .map(|s| Style { bg: None, ..s })
        .unwrap_or(Style { fg: ui.linenr.fg, ..Style::default() });
    let depths: Vec<usize> = if editor.config.indent_guides && left == 0 {
        let first = top.saturating_sub(40);
        let last = (top + span + 40).min(max_line);
        let raw: Vec<Option<usize>> = (first..=last).map(|l| line_indent(text, l, tab)).collect();
        let mut up = vec![0usize; raw.len()];
        let mut down = vec![0usize; raw.len()];
        let mut last_seen = 0;
        for (i, r) in raw.iter().enumerate() {
            if let Some(w) = r {
                last_seen = *w;
            }
            up[i] = last_seen;
        }
        last_seen = 0;
        for (i, r) in raw.iter().enumerate().rev() {
            if let Some(w) = r {
                last_seen = *w;
            }
            down[i] = last_seen;
        }
        (0..span)
            .map(|row| {
                let i = top + row - first;
                match raw.get(i) {
                    Some(Some(w)) => *w,
                    Some(None) => up[i].min(down[i]),
                    None => 0,
                }
            })
            .collect()
    } else {
        vec![0; span]
    };
    // Guides of the block holding the cursor are a step brighter (that column, that line range)
    let active_guide: Option<(usize, std::ops::Range<usize>)> = cur_line.checked_sub(top).and_then(|r| {
        let d = *depths.get(r)?;
        let next_deeper = (cur_line < max_line)
            .then(|| line_indent(text, cur_line + 1, tab))
            .flatten()
            .is_some_and(|n| n > d);
        let col = if next_deeper { d } else { d.checked_sub(unit)? };
        if col == 0 {
            return None;
        }
        let mut a = r;
        while a > 0 && depths[a - 1] > col {
            a -= 1;
        }
        let mut b = r + 1;
        while b < depths.len() && depths[b] > col {
            b += 1;
        }
        if next_deeper {
            a = r + 1; // the head line itself has no guide
        }
        Some((col, a..b))
    });
    let active_guide_style = Style { fg: ui.virt.fg, ..Style::default() };
    let signs = lay.gutter - num_w(doc);
    for row in 0..lay.text_rows {
        let y = lay.text_top + row as u16;
        queue!(out, MoveTo(lay.x0 as u16, y))?;
        let line = match view.get(row) {
            None => {
                clear_rest(out, ui.base)?;
                continue;
            }
            // Folded doc comment: line number only on the block's first row; git sign if any in the block
            Some(crate::doccomment::ViewRow::Doc { start, end, indent, index, spans, code }) => {
                apply(out, ui.base)?;
                queue!(out, Print(" ".repeat(signs)))?;
                // Cursor inside a folded block (normal mode): current-line number color, accent bar
                let here = (*start..=*end).contains(&cur_line);
                apply(out, if focused && here { ui.linenr_selected } else { ui.linenr })?;
                let w = lay.gutter - 1 - signs;
                if *index == 0 {
                    let num = match editor.config.line_number {
                        LineNumber::Relative if !here => start.abs_diff(cur_line),
                        _ => start + 1,
                    };
                    queue!(out, Print(format!("{num:>w$}")))?;
                } else {
                    queue!(out, Print(" ".repeat(w)))?;
                }
                let touched = *index == 0
                    && doc.git_hunks.iter().any(|h| h.lines.start <= *end && h.lines.end > *start);
                if touched {
                    apply(out, git_color("diff.delta"))?;
                    queue!(out, Print("▎"))?;
                } else {
                    apply(out, ui.base)?;
                    queue!(out, Print(" "))?;
                }
                draw_folded_row(out, editor, ui, lay, *indent, spans, *code, focused && here, labels_on)?;
                continue;
            }
            Some(crate::doccomment::ViewRow::Line(l)) => (*l, None),
            Some(crate::doccomment::ViewRow::Part { line, row, index, last }) => {
                (*line, Some((*row, *index, *last)))
            }
        };
        let (line, part) = line;
        // This row's slice of the line: display columns [row_left, row_end), drawn from screen x `x0`
        let (row_left, row_end, x0, first, last) = match part {
            Some((r, index, last)) => (r.col, r.end_col, r.x0, index == 0, last),
            None => (left, usize::MAX, 0, true, true),
        };
        let li = line - top;
        let depth = depths.get(li).copied().unwrap_or(0);
        let guide_at = |col: usize| {
            let active = active_guide.as_ref().is_some_and(|(c, rows)| *c == col && rows.contains(&li));
            if active { active_guide_style } else { guide_style }
        };
        if line > max_line {
            clear_rest(out, ui.base)?;
            continue;
        }
        let num = match editor.config.line_number {
            LineNumber::Relative if line != cur_line => line.abs_diff(cur_line),
            _ => line + 1,
        };
        if !first {
            // Continuation row of a wrapped line: no sign or number
            apply(out, ui.base)?;
            queue!(out, Print(" ".repeat(lay.gutter - 1)))?;
        } else if signs > 0 {
            // Sign column: stopped line ▶ > breakpoint ● > diagnostic bar
            let sev = line_severity.get(line.wrapping_sub(top)).copied().unwrap_or(0);
            if stop_line == Some(line) {
                apply(out, Style { fg: stop_color, bold: true, ..ui.base })?;
                queue!(out, Print("▶"))?;
            } else if let Some(bp) = doc_bps.and_then(|b| b.get(&line)) {
                // Breakpoint = red · condition = orange · logpoint = accent (the line-end text says which)
                let fg = if bp.log.is_some() {
                    ui.accent.fg
                } else if bp.condition.is_some() {
                    stop_color
                } else {
                    ui.error.fg
                };
                apply(out, Style { fg, ..ui.base })?;
                queue!(out, Print("●"))?;
            } else {
                apply(out, if sev > 0 { ui.base.patch(ui.sign[sev as usize]) } else { ui.base })?;
                queue!(out, Print(if sev > 0 { "▎" } else { " " }))?;
            }
        }
        if first {
            apply(out, if focused && line == cur_line { ui.linenr_selected } else { ui.linenr })?;
            queue!(out, Print(format!("{num:>w$}", w = lay.gutter - 1 - signs)))?;
        }
        // Between line number and text: git bar (added green · modified yellow · deleted red underline) —
        // the bar runs down a wrapped line's rows, the underline goes under its last
        match git_marks.get(li).copied().flatten().filter(|(g, _)| last || *g != "▁") {
            Some((glyph, st)) => {
                apply(out, st)?;
                queue!(out, Print(glyph))?;
            }
            None => queue!(out, Print(" "))?,
        }

        // Current-line highlight: this line's background = ui.cursorline.primary over the default
        let line_message_here = line_message.get(li).copied().flatten();
        let line_base = if stop_line == Some(line) {
            // Stopped line: accent band (takes precedence over the cursor line)
            Style { bg: blend(stop_color, ui.tint, 0.18).or(ui.base.bg), ..ui.base }
        } else if focused && editor.config.cursorline && line == cur_line {
            ui.cursorline
        } else {
            // Error/warning lines get a very faint wash of that color — so problems stand out when skimming
            match line_message_here {
                Some((sev @ 1..=2, ..)) => {
                    Style { bg: blend(ui.sign[sev as usize].fg, ui.tint, 0.07).or(ui.base.bg), ..ui.base }
                }
                _ => ui.base,
            }
        };
        let start = mv::line_start(text, line);
        // Fetch only visible width + slack — so even a one-line giant file isn't copied whole every frame.
        let (sc, ec) = (text.byte_to_char(start), text.byte_to_char(mv::line_full_end(text, line)));
        let end = text.char_to_byte(ec.min(sc + row_left + lay.text_cols + 256)); // cut at a char boundary
        let s = text.byte_slice(start..end).to_string();
        // Per-byte captures for this line chunk (outer painted first so inner/later patterns win)
        let mut paint: Vec<u32> = Vec::new();
        if let Some(syn) = syntax {
            paint = vec![u32::MAX; s.len()];
            let per_line;
            let spans = match &whole {
                Some(w) => w,
                None => {
                    per_line = syntax::highlights(syn, text, start..end, &mut qcursor);
                    &per_line
                }
            };
            for &(a, b, c) in spans {
                let (a, b) = (a.max(start), b.min(end));
                if a < b {
                    paint[a - start..b - start].fill(c);
                }
            }
        }
        let mut col = 0;
        let mut current: Option<Style> = None;
        // This line's hints (virtual text) — inserted before the char there; later chars shift by `shift`
        let line_end = mv::line_end(text, line);
        let mut line_hints = hints.iter().filter(|h| h.pos >= start && h.pos <= line_end).peekable();
        let mut shift = 0;
        let mut clipped = false;
        // Wrapped: hints only where the whole line plus them fits one row (they'd push code off it)
        let hint_room = match (wrapping, part) {
            (false, _) => usize::MAX,
            (true, Some(_)) => 0,
            (true, None) => lay.text_cols.saturating_sub(mv::visual_col(text, line_end, tab)),
        };
        // Continuation row: its indent (with the line's guides)
        if x0 > 0 {
            for k in 0..x0 {
                let guide = k > 0 && k < depth && k % unit == 0;
                apply(out, if guide { line_base.patch(guide_at(k)) } else { line_base })?;
                queue!(out, Print(if guide { "│" } else { " " }))?;
            }
            current = None;
        }
        for c in graphemes::cells(&s, start, tab) {
            let g = &s[c.bytes.clone()];
            if c.col >= row_end {
                clipped = true; // the rest is the next row's
                break;
            }
            col = c.col + c.width;
            if c.col < row_left {
                // A wide char/tab cut by the scroll edge: blank its visible part so what follows lines up
                if col > row_left {
                    apply(out, line_base)?;
                    current = Some(line_base);
                    queue!(out, Print(" ".repeat((col - row_left).min(lay.text_cols))))?;
                }
                continue;
            }
            if c.width == 0 && g == "\r" {
                continue;
            }
            while let Some(h) = line_hints.next_if(|h| h.pos <= c.pos) {
                let room = lay
                    .text_cols
                    .saturating_sub(c.col - row_left + shift)
                    .min(hint_room.saturating_sub(shift));
                let t = fit(&h.text, room);
                apply(out, line_base.patch(Style { bg: None, ..inlay_style }))?;
                current = None;
                queue!(out, Print(&t))?;
                shift += t.width();
            }
            let x = x0 + c.col - row_left + shift;
            if x + c.width > lay.text_cols {
                clipped = true;
                break;
            }
            if (c.pos..c.pos + c.len).contains(&primary) {
                cursor = Some(((lay.x0 + lay.gutter + x) as u16, y));
            }
            // Style = default (cursorline if current line) → syntax → selection → cursor
            let mut st = line_base;
            if let Some(&cap) = paint.get(c.bytes.start)
                && let Some(Some(hs)) = capture_styles.get(cap as usize)
            {
                st = st.patch(*hs);
            }
            let span = c.pos..c.pos + c.len.max(1);
            let hit = cursors.iter().position(|p| span.contains(p));
            let selected = ranges.iter().position(|r| r.from() <= c.pos && c.pos < r.to());
            // Underline diagnostic ranges (the most severe)
            if let Some(d) = visible_diags.iter().filter(|d| underlines(d, c.pos)).min_by_key(|d| d.severity)
            {
                st = st.patch(ui.diag[d.severity as usize]);
            }
            if !visible_hits.is_empty() {
                let k = visible_hits.partition_point(|m| m.1 <= c.pos);
                if let Some(&(a, b)) = visible_hits.get(k)
                    && a <= c.pos
                {
                    let current = (a, b) == (primary_range.from(), primary_range.to());
                    st = st.patch(if current { hit_current } else { hit_style });
                }
            }
            if let Some(i) = selected {
                st = st.patch(if i == primary_idx { ui.selection_primary } else { ui.selection });
            }
            match hit {
                Some(i) if i == primary_idx && paint_primary => st = st.patch(ui.cursor_primary),
                Some(i) if i != primary_idx => st = st.patch(ui.cursor),
                _ => {}
            }
            let eol = g == "\n" || g == "\r\n";
            if eol && selected.is_none() && hit.is_none() {
                col = c.col;
                continue;
            }
            // A space inside indentation at a guide position becomes │ (only without selection/cursor)
            if g == " "
                && c.col > 0
                && c.col < depth
                && c.col % unit == 0
                && selected.is_none()
                && hit.is_none()
            {
                let gs = line_base.patch(guide_at(c.col));
                apply(out, gs)?;
                current = Some(gs);
                queue!(out, Print("│"))?;
                continue;
            }
            if labels_on {
                if let Some(&ch) = label_cells.get(&c.pos) {
                    apply(out, line_base.patch(label_style))?;
                    current = None;
                    queue!(out, Print(ch), Print(" ".repeat(c.width.saturating_sub(1))))?;
                    continue;
                }
                st = Style { fg: ui.virt.fg, ..st };
            }
            if current != Some(st) {
                apply(out, st)?;
                current = Some(st);
            }
            if eol || g == "\t" {
                queue!(out, Print(" ".repeat(c.width)))?;
            } else if g.chars().next().is_some_and(char::is_control) {
                queue!(out, Print('\u{fffd}'))?;
            } else {
                queue!(out, Print(g))?;
            }
            if eol {
                col = c.col;
            }
        }
        // Guides on blank lines — no chars there, so fill spaces up to that spot
        if col == 0 && depth > unit && s.trim().is_empty() {
            let mut at = usize::from(line == cur_line && hit_eol(&cursors, ranges, start, &s));
            for k in (unit..depth).step_by(unit) {
                if k < at || k >= lay.text_cols {
                    continue;
                }
                apply(out, line_base)?;
                queue!(out, Print(" ".repeat(k - at)))?;
                apply(out, line_base.patch(guide_at(k)))?;
                queue!(out, Print("│"))?;
                at = k + 1;
            }
        }
        // Hints left at line end (a line without a newline, like the last one)
        if !clipped {
            for h in line_hints {
                let room = lay
                    .text_cols
                    .saturating_sub(x0 + col.saturating_sub(row_left) + shift)
                    .min(hint_room.saturating_sub(shift));
                let t = fit(&h.text, room);
                apply(out, line_base.patch(Style { bg: None, ..inlay_style }))?;
                queue!(out, Print(&t))?;
                shift += t.width();
            }
        }
        // Cursor at end of doc (EOF) — there's no corresponding char.
        if primary == len && line == cur_line && cursor.is_none() && col >= row_left && last {
            let x = x0 + col - row_left + shift;
            if x < lay.text_cols {
                cursor = Some(((lay.x0 + lay.gutter + x) as u16, y));
            }
        }
        // Stopped: line-end values of vars used there (`count = 12  total = 15`) — names dim, values by shape
        let mut extra_used = 0;
        if let Some(vals) = inline_vals.get(&line)
            && !clipped
        {
            let used = x0 + col.saturating_sub(row_left) + shift + 1;
            let mut room = lay.text_cols.saturating_sub(used + 3);
            if room > 6 {
                apply(out, line_base)?;
                queue!(out, Print("   "))?;
                extra_used += 3;
                for (k, (name, value)) in vals.iter().enumerate() {
                    let sep = if k > 0 { "  " } else { "" };
                    let value = fit_ellipsis(value, 32);
                    let w = sep.width() + name.width() + 3 + value.width();
                    if w > room {
                        break;
                    }
                    apply(out, Style { fg: ui.linenr.fg, italic: true, ..line_base })?;
                    queue!(out, Print(sep), Print(name), Print(" = "))?;
                    let vc = editor
                        .theme
                        .try_get(crate::dap::value_scope(&value))
                        .and_then(|s| s.fg)
                        .or(ui.base.fg);
                    apply(out, Style { fg: vc, italic: true, ..line_base })?;
                    queue!(out, Print(&value))?;
                    room -= w;
                    extra_used += w;
                }
            }
        }
        // Condition/logpoint at line end: `● when n > 3` / `● log step {i}` — reason in red if rejected
        if let Some(bp) = doc_bps
            .and_then(|b| b.get(&line))
            .filter(|b| b.condition.is_some() || b.log.is_some() || b.rejected.is_some())
            && !clipped
        {
            let used = x0 + col.saturating_sub(row_left) + shift + 1 + extra_used;
            let mut room = lay.text_cols.saturating_sub(used + 3);
            if room >= 10 {
                let mut parts: Vec<(&str, String)> = Vec::new();
                if let Some(c) = &bp.condition {
                    parts.push(("when ", c.clone()));
                }
                if let Some(m) = &bp.log {
                    parts.push(("log ", m.clone()));
                }
                apply(out, line_base)?;
                queue!(out, Print("   "))?;
                extra_used += 3;
                let glyph_fg = if bp.log.is_some() { ui.accent.fg } else { stop_color };
                apply(out, Style { fg: glyph_fg, ..line_base })?;
                queue!(out, Print("● "))?;
                (extra_used, room) = (extra_used + 2, room - 2);
                for (k, (label, body)) in parts.iter().enumerate() {
                    let sep = if k > 0 { "  " } else { "" };
                    let body = fit_ellipsis(body, room.saturating_sub(sep.len() + label.len()).min(48));
                    apply(out, Style { fg: ui.linenr.fg, italic: true, ..line_base })?;
                    queue!(out, Print(sep), Print(label))?;
                    apply(out, Style { fg: ui.virt.fg, italic: true, ..line_base })?;
                    queue!(out, Print(&body))?;
                    let w = sep.len() + label.len() + body.width();
                    (extra_used, room) = (extra_used + w, room.saturating_sub(w));
                }
                if let Some(why) = &bp.rejected {
                    let why = fit_ellipsis(why, room.saturating_sub(2));
                    apply(out, Style { fg: ui.error.fg, italic: true, ..line_base })?;
                    queue!(out, Print("  "), Print(&why))?;
                    extra_used += 2 + why.width();
                }
            }
        }
        // Line-end diagnostic: `  ● message +2` — symbol in severity color, text a tone lower. …if no room
        if let Some((sev, msg, more, whole)) = line_message_here
            && !clipped
        {
            let used = x0 + col.saturating_sub(row_left) + shift + 1 + extra_used;
            let room = lay.text_cols.saturating_sub(used + 3);
            if room >= 8 {
                let color = ui.sign[sev as usize].fg;
                let glyph = match sev {
                    _ if test_fail.get(li).copied().unwrap_or(false) => "▲",
                    1 => "●",
                    2 => "▲",
                    3 => "●",
                    _ => "·",
                };
                let more = if more > 0 { format!("  +{more}") } else { String::new() };
                let body = fit_ellipsis(msg, room.saturating_sub(2 + more.width()));
                if focused && line == cur_line && whole && more.is_empty() && body == msg {
                    cursor_diags_shown.set(true);
                }
                apply(out, line_base)?;
                queue!(out, Print("   "))?;
                apply(out, Style { fg: color, ..line_base })?;
                queue!(out, Print(glyph), Print(" "))?;
                apply(
                    out,
                    Style {
                        fg: blend(color, line_base.bg.or(ui.tint), 0.72).or(color),
                        italic: true,
                        ..line_base
                    },
                )?;
                queue!(out, Print(&body))?;
                apply(out, Style { fg: ui.linenr.fg, ..line_base })?;
                queue!(out, Print(&more))?;
            }
        }
        clear_rest(out, line_base)?;
    }
    if lay.scrollbar {
        draw_scrollbar(doc, top, ui, lay, out)?;
    }
    Ok(cursor)
}

/// Screen rows: doc lines in order, but doc-comment blocks without cursor/selection are **folded** (text
/// rewrapped to width, no blank lines/fences) into fewer rows. Once the cursor enters, it unfolds to raw.
/// Detection looks up to 200 lines above/below the screen (so a `/** */` block starting off-screen connects).
fn build_view(
    editor: &Editor,
    doc: &crate::document::Document,
    top: usize,
    left: usize,
    lay: &Layout,
) -> Vec<crate::doccomment::ViewRow> {
    use crate::doccomment::{ViewRow, classify, parts};
    let text = &doc.text;
    let rows = lay.text_rows;
    let last = mv::last_line(text);
    let cur_line = mv::line_of(text, doc.selection().primary().cursor(text));
    let max_line = last.max(cur_line);
    let lang = doc
        .syntax
        .as_ref()
        .map(|s| s.lang.name.as_str())
        .or_else(|| doc.path.as_deref().and_then(crate::syntax::detect).map(|s| s.name.as_str()));
    // Soft wrap: a line → its rows (the top line taller than the screen shows from the cursor's part)
    let wrap_w = editor.wraps(doc).then_some(lay.text_cols);
    let tab = editor.config.tab_width;
    let push_line = |view: &mut Vec<ViewRow>, line: usize| {
        let Some(w) = wrap_w else { return view.push(ViewRow::Line(line)) };
        let parts = crate::wrap::rows(text, line, w, tab);
        if parts.len() == 1 {
            return view.push(ViewRow::Line(line));
        }
        let skip = if line == top && line == cur_line {
            let k = crate::wrap::row_of(&parts, doc.selection().primary().cursor(text));
            (k + 1).saturating_sub(rows)
        } else {
            0
        };
        let n = parts.len();
        for (index, row) in parts.into_iter().enumerate().skip(skip) {
            if view.len() == rows {
                break;
            }
            view.push(ViewRow::Part { line, row, index, last: index + 1 == n });
        }
    };
    if !editor.config.render_doc_comments
        || left > 0
        || editor.review.is_some()
        || !crate::doccomment::applies(lang)
    {
        let mut view = Vec::with_capacity(rows);
        let mut line = top;
        while view.len() < rows && line <= max_line {
            push_line(&mut view, line);
            line += 1;
        }
        return view;
    }
    // Find blocks: runs of consecutive doc-comment lines within [w0, w1]
    let (w0, w1) = (top.saturating_sub(200), (top + rows * 3 + 200).min(last));
    let mut info: Vec<Option<(usize, String)>> = Vec::with_capacity(w1 + 1 - w0);
    let mut in_block = false;
    for l in w0..=w1 {
        let line: String = text.line(l).chars().take(2000).collect();
        let (c, next) = classify(&line, in_block);
        in_block = next;
        info.push(c);
    }
    // Unfold only when editing/selecting: INSERT/SELECT mode, or a selection wider than one char touching it.
    // In normal mode a passing cursor leaves it folded (j/k skip the block like one line — commands.rs)
    let opening = matches!(editor.mode, Mode::Insert | Mode::Select);
    let sel_lines: Vec<(usize, usize)> = doc
        .selection()
        .ranges()
        .iter()
        .filter(|r| opening || r.to() > crate::graphemes::next_boundary(text, r.from()))
        .map(|r| (text.byte_to_line(r.from()), text.byte_to_line(r.to().max(r.from()))))
        .collect();
    // (start, end, indent, texts) — blocks being edited or selected are left out (raw)
    let mut blocks: Vec<(usize, usize, usize, Vec<String>)> = Vec::new();
    let mut i = 0;
    while i < info.len() {
        if info[i].is_none() {
            i += 1;
            continue;
        }
        let start = i;
        while i < info.len() && info[i].is_some() {
            i += 1;
        }
        let (a, b) = (w0 + start, w0 + i - 1);
        if sel_lines.iter().any(|&(x, y)| x <= b && y >= a) {
            continue;
        }
        let bodies =
            info[start..i].iter().map(|x| x.as_ref().map(|v| v.1.clone()).unwrap_or_default()).collect();
        blocks.push((a, b, info[start].as_ref().map_or(0, |v| v.0), bodies));
    }
    let lang = doc.syntax.as_ref().map(|s| s.lang.name.clone());
    let mut view = Vec::with_capacity(rows);
    let mut line = top;
    while view.len() < rows && line <= max_line {
        match blocks.iter().find(|b| b.0 <= line && line <= b.1) {
            Some((a, b, indent, bodies)) => {
                let width = lay.text_cols.saturating_sub(indent + 3).max(8);
                let folded = fold_rows(editor, &parts(bodies), width, lang.as_deref());
                // If the top of the screen is mid-block, skip that proportion
                let skip = (line - a).min(folded.len().saturating_sub(1));
                for (k, (spans, code)) in folded.into_iter().enumerate().skip(skip) {
                    if view.len() == rows {
                        break;
                    }
                    view.push(ViewRow::Doc { start: *a, end: *b, indent: *indent, index: k, spans, code });
                }
                line = b + 1;
            }
            None => {
                push_line(&mut view, line);
                line += 1;
            }
        }
    }
    view
}

/// Screen rows of a folded doc-comment block: (chunk, is example code line).
fn fold_rows(
    editor: &Editor,
    parts: &[crate::doccomment::Part],
    width: usize,
    lang: Option<&str>,
) -> Vec<(Vec<crate::markdown::Span>, bool)> {
    use crate::doccomment::{Chunk, chunks};
    use crate::markdown::{self as md, Line, Span};
    let t = &editor.theme;
    let accent = t.try_get("ui.accent").and_then(|s| s.fg);
    let plain = |text: String, style: Style| Span { text, style };
    let mut out = Vec::new();
    let wrap = |spans: Vec<Span>, indent: usize, out: &mut Vec<(Vec<Span>, bool)>| {
        for row in md::wrap(&[Line::Text { spans, indent }], width, Style::default()) {
            out.push((row, false));
        }
    };
    for c in chunks(parts) {
        match c {
            Chunk::Heading(h) => {
                out.push((vec![plain(h, Style { fg: accent, bold: true, ..Style::default() })], false))
            }
            Chunk::Para(p) => wrap(md::inline_spans(&p, t), 0, &mut out),
            Chunk::Item(p) => {
                let mut v = vec![plain("• ".into(), Style { fg: accent, ..Style::default() })];
                v.extend(md::inline_spans(&p, t));
                wrap(v, 2, &mut out);
            }
            Chunk::Tag(name, rest) => {
                let kw =
                    t.try_get("keyword").map(|s| Style { bg: None, bold: true, ..s }).unwrap_or_default();
                let mut v = vec![plain(format!("@{name} "), kw)];
                v.extend(md::inline_spans(&rest, t));
                wrap(v, 2, &mut out);
            }
            // Blank line between paragraphs — the panel continues, only the text is empty
            Chunk::Gap => out.push((Vec::new(), false)),
            Chunk::Code(line) => {
                let spans = match md::code_lines(&line, lang, t).into_iter().next() {
                    Some(Line::Code(s)) => s,
                    _ => Vec::new(),
                };
                out.push((spans, true));
            }
        }
    }
    if out.is_empty() {
        out.push((Vec::new(), false));
    }
    out
}

/// One screen row of a folded doc comment: indent · bar · text on a faint panel. Body a tone lower — reads
/// smaller, quieter than code (terminals can't resize fonts: contrast); example code = a well of editor bg.
#[allow(clippy::too_many_arguments)]
fn draw_folded_row(
    out: &mut impl Write,
    editor: &Editor,
    ui: &Ui,
    lay: &Layout,
    indent: usize,
    spans: &[crate::markdown::Span],
    code: bool,
    current: bool,
    dim: bool,
) -> io::Result<()> {
    let comment_fg = editor.theme.try_get("comment").and_then(|s| s.fg).or(ui.virt.fg);
    let panel_bg = blend(comment_fg, ui.tint, if current { 0.16 } else { 0.09 }).or(ui.base.bg);
    let panel = Style { bg: panel_bg, fg: blend(ui.base.fg, comment_fg, 0.55).or(ui.base.fg), ..ui.base };
    let well = Style { bg: ui.base.bg, ..panel };
    let width = lay.text_cols;
    let indent = indent.min(width.saturating_sub(4));
    apply(out, ui.base)?;
    queue!(out, Print(" ".repeat(indent)))?;
    apply(out, Style { fg: if current { ui.accent.fg.or(comment_fg) } else { comment_fg }, ..panel })?;
    queue!(out, Print("▎ "))?;
    let mut used = indent + 2;
    let base = if code { well } else { panel };
    if code {
        apply(out, well)?;
        queue!(out, Print(" "))?;
        used += 1;
    }
    for sp in spans {
        if used >= width {
            break;
        }
        let text = fit(&sp.text, width - used);
        if text.is_empty() {
            continue;
        }
        let st = base.patch(Style { bg: None, ..sp.style });
        // `gw` labels up: step back like the rest of the text
        apply(out, if dim { Style { fg: ui.virt.fg, ..st } } else { st })?;
        queue!(out, Print(&text))?;
        used += text.width();
    }
    apply(out, if code { well } else { panel })?;
    let pad = if code { (width.saturating_sub(used)).saturating_sub(1) } else { width.saturating_sub(used) };
    queue!(out, Print(" ".repeat(pad)))?;
    if code {
        apply(out, panel)?;
        queue!(out, Print(" "))?;
    }
    clear_rest(out, ui.base)
}

/// Indent width of a line (None if blank).
fn line_indent(text: &ropey::Rope, line: usize, tab: usize) -> Option<usize> {
    let mut w = 0;
    for ch in text.line(line).chars() {
        match ch {
            ' ' => w += 1,
            '\t' => w += tab - w % tab.max(1),
            '\n' | '\r' => return None,
            _ => return Some(w),
        }
    }
    None
}

/// Was the blank line's newline cell already drawn (did a cursor/selection there take a cell)?
fn hit_eol(cursors: &[usize], ranges: &[Range], start: usize, s: &str) -> bool {
    let nl = start + s.trim_end_matches(['\r', '\n']).len();
    !s.is_empty() && (cursors.contains(&nl) || ranges.iter().any(|r| r.from() <= nl && nl < r.to()))
}

/// Right scrollbar: visible span = dim thumb, error/warning lines across the file = ticks in that color.
fn draw_scrollbar(
    doc: &crate::document::Document,
    top: usize,
    ui: &Ui,
    lay: &Layout,
    out: &mut impl Write,
) -> io::Result<()> {
    let total = doc.text.len_lines().max(1);
    let rows = lay.text_rows.max(1);
    let (a, b) = (top * rows / total, ((top + rows) * rows).div_ceil(total).max(top * rows / total + 1));
    let mut marks = vec![0u8; rows];
    // git changes (only cells without diagnostics): 7 = added/modified, 8 = deleted
    for h in &doc.git_hunks {
        let r = (h.lines.start * rows / total).min(rows - 1);
        let end = ((h.lines.end.max(h.lines.start + 1)) * rows).div_ceil(total).clamp(r + 1, rows);
        for m in &mut marks[r..end] {
            if *m == 0 {
                *m = if h.kind == crate::git::HunkKind::Deleted { 8 } else { 7 };
            }
        }
    }
    for d in &doc.lsp.diagnostics {
        let r = (doc.text.byte_to_line(d.from.min(doc.text.len_bytes())) * rows / total).min(rows - 1);
        if d.severity <= 2 && (marks[r] == 0 || marks[r] > 2 || d.severity < marks[r]) {
            marks[r] = d.severity;
        }
    }
    let x = lay.edit_w.saturating_sub(1) as u16;
    for (row, &mark) in marks.iter().enumerate() {
        queue!(out, MoveTo(x, lay.text_top + row as u16))?;
        let st = match mark {
            0 if (a..b).contains(&row) => Style { fg: ui.linenr.fg, ..ui.base },
            0 => {
                apply(out, ui.base)?;
                queue!(out, Print(" "))?;
                continue;
            }
            7 => Style { fg: ui.diff_plus.fg, ..ui.base },
            8 => Style { fg: ui.diff_minus.fg, ..ui.base },
            s => ui.base.patch(ui.sign[s as usize]),
        };
        apply(out, st)?;
        queue!(out, Print("▐"))?;
    }
    Ok(())
}

/// Blend two colors (truecolor only — None for named colors).
fn blend(
    a: Option<crossterm::style::Color>,
    b: Option<crossterm::style::Color>,
    t: f32,
) -> Option<crossterm::style::Color> {
    use crossterm::style::Color::Rgb;
    match (a?, b?) {
        (Rgb { r, g, b: bb }, Rgb { r: r2, g: g2, b: b2 }) => {
            let m = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
            Some(Rgb { r: m(r2, r), g: m(g2, g), b: m(b2, bb) })
        }
        _ => None,
    }
}

/// Review screen — **inside the buffer** (like Cursor): the whole file as in the editor, changes expanded.
/// Deleted lines on faint red, added on faint green bg, keeping syntax colors. Each change has a head row —
/// the current one an accent band with key hints, decided ones "accepted"/"rejected" (accept leaves only
/// the new text visible, reject only the original). The top row is a fixed title (instruction · # decided).
fn draw_review(
    rev: &Review,
    rows: &[crate::llm::RRow],
    editor: &Editor,
    ui: &Ui,
    lay: &Layout,
    out: &mut impl Write,
) -> io::Result<()> {
    use crate::llm::{Decision, RRow};
    let t = &editor.theme;
    let Some(doc) = editor.docs.iter().find(|d| d.id == rev.doc_id) else { return Ok(()) };
    let text = &doc.text;
    let lang = doc.syntax.as_ref().map(|s| s.lang.name.clone());
    let syntax = doc.syntax.as_ref().filter(|s| s.tree.is_some());
    let capture_styles: Vec<Option<Style>> =
        if syntax.is_some() { syntax::capture_styles(|n| t.try_get(n)) } else { Vec::new() };
    let dim = Style { fg: ui.virt.fg, ..ui.base };
    let faint = Style { fg: ui.linenr.fg, ..ui.base };
    let strong =
        ui.base.patch(t.try_get("ui.text.focus").unwrap_or(Style { bold: true, ..Style::default() }));
    let accent = Style { fg: ui.accent.fg, ..ui.base };
    let (minus_fg, plus_fg) = (ui.diff_minus.fg, ui.diff_plus.fg);
    let minus_bg = blend(minus_fg, ui.tint, 0.16);
    let plus_bg = blend(plus_fg, ui.tint, 0.16);
    // Head band is neutral card color — an accent tint (hanji's vermilion) would look like deleted-line red
    let band = card_style(ui, editor, "ui.popup").bg;
    let width = lay.edit_w;
    let num_w = lay.gutter.saturating_sub(2).max(3);
    let code_w = width.saturating_sub(num_w + 3);
    let body = lay.text_rows.saturating_sub(1);
    let shown: &[RRow] = &rows[rev.scroll.min(rows.len())..];
    let shown = &shown[..shown.len().min(body)];
    // Syntax highlight for all visible doc lines at once
    let doc_lines: Vec<usize> =
        shown.iter().filter_map(|r| if let RRow::Doc(l) = r { Some(*l) } else { None }).collect();
    let spans = match (syntax, doc_lines.first(), doc_lines.last()) {
        (Some(syn), Some(&a), Some(&b)) => syntax::highlights(
            syn,
            text,
            mv::line_start(text, a)..mv::line_full_end(text, b),
            &mut QueryCursor::new(),
        ),
        _ => Vec::new(),
    };
    // Fixed title
    let n = rev.hunks.len();
    let decided = rev.hunks.iter().filter(|h| h.decision != Decision::Undecided).count();
    queue!(out, MoveTo(0, lay.text_top))?;
    apply(out, accent)?;
    queue!(out, Print(" ▎ "))?;
    apply(out, strong)?;
    queue!(out, Print(rev.by))?;
    let right = format!("{decided} of {n} decided ");
    // Human instructions in quotes, agent proposals as their description as-is
    let said = if rev.by == "Claude" {
        format!("   “{}”", rev.instruction)
    } else {
        format!("   {}", rev.instruction)
    };
    let head = 3 + rev.by.width();
    let instr = fit_ellipsis(&said, width.saturating_sub(head + right.len() + 2));
    apply(out, dim)?;
    queue!(out, Print(&instr), Print(" ".repeat(width.saturating_sub(head + instr.width() + right.len()))))?;
    apply(out, faint)?;
    queue!(out, Print(&right))?;
    for (k, row) in shown.iter().enumerate() {
        let y = lay.text_top + 1 + k as u16;
        queue!(out, MoveTo(0, y))?;
        let mut used;
        match row {
            RRow::Doc(l) => {
                apply(out, faint)?;
                queue!(out, Print(format!("{:>num_w$}   ", l + 1)))?;
                used = num_w + 3;
                used += draw_code_line(
                    out,
                    text,
                    *l,
                    code_w,
                    editor.config.tab_width,
                    &spans,
                    &capture_styles,
                    ui.base,
                )?;
            }
            RRow::Header(i) => {
                let h = &rev.hunks[*i];
                let current = *i == rev.current && h.decision == Decision::Undecided && !h.conflict;
                let bg = if current { band.or(ui.base.bg) } else { ui.base.bg };
                let base = Style { bg, ..ui.base };
                let line = text.byte_to_line(h.from.min(text.len_bytes())) + 1;
                apply(out, base)?;
                queue!(out, Print(" ".repeat(num_w + 1)))?;
                apply(
                    out,
                    if current {
                        Style { fg: ui.accent.fg, ..base }
                    } else {
                        Style { fg: ui.linenr.fg, ..base }
                    },
                )?;
                queue!(out, Print("▎ "))?;
                let label = format!("Change {} of {n} · line {line}", i + 1);
                apply(
                    out,
                    if current {
                        Style { bold: true, ..base.patch(Style { fg: ui.accent.fg, ..Style::default() }) }
                    } else {
                        Style { fg: ui.virt.fg, ..base }
                    },
                )?;
                queue!(out, Print(&label))?;
                used = num_w + 3 + label.width();
                let (status, st): (Vec<(&str, &str)>, Style) = match (h.conflict, h.decision) {
                    (true, _) => (vec![("", "text changed while waiting — skipped")], base.patch(ui.error)),
                    (_, Decision::Accept) => (vec![("", "accepted")], Style { fg: plus_fg, ..base }),
                    (_, Decision::Reject) => (vec![("", "rejected")], Style { fg: minus_fg, ..base }),
                    _ if current => {
                        (vec![("y", "accept"), ("n", "reject"), ("a", "all"), ("tab", "next")], base)
                    }
                    _ => (vec![("", "pending")], Style { fg: ui.linenr.fg, ..base }),
                };
                for (key, desc) in status {
                    let piece =
                        if key.is_empty() { format!("   {desc}") } else { format!("   {key} {desc}") };
                    if used + piece.width() > width {
                        break;
                    }
                    if key.is_empty() {
                        apply(out, st)?;
                        queue!(out, Print(&piece))?;
                    } else {
                        apply(out, Style { fg: ui.accent.fg, bold: true, ..base })?;
                        queue!(out, Print(format!("   {key}")))?;
                        apply(out, Style { fg: ui.virt.fg, ..base })?;
                        queue!(out, Print(format!(" {desc}")))?;
                    }
                    used += piece.width();
                }
                apply(out, base)?;
                queue!(out, Print(" ".repeat(width.saturating_sub(used))))?;
                used = width;
            }
            RRow::Diff { kind, text: line, old_no, hunk } => {
                let decided = rev.hunks[*hunk].decision != Decision::Undecided;
                let bg = match kind {
                    '-' => minus_bg,
                    '+' if !decided => plus_bg,
                    _ => None,
                }
                .or(ui.base.bg);
                let base = Style { bg, ..ui.base };
                apply(out, Style { fg: ui.linenr.fg, ..base })?;
                queue!(
                    out,
                    Print(match old_no {
                        Some(no) => format!("{no:>num_w$} "),
                        None => " ".repeat(num_w + 1),
                    })
                )?;
                let (sign, sign_st) = match kind {
                    '-' => ("−", Style { fg: minus_fg, bold: true, ..base }),
                    '+' => ("▎", Style { fg: plus_fg, ..base }),
                    _ => (" ", base),
                };
                apply(out, sign_st)?;
                queue!(out, Print(sign), Print(" "))?;
                used = num_w + 3;
                for l in crate::markdown::code_lines(line, lang.as_deref(), t) {
                    if let crate::markdown::Line::Code(sp) = l {
                        for s in sp {
                            let txt = fit(&s.text, width.saturating_sub(used));
                            apply(out, base.patch(Style { bg: None, ..s.style }))?;
                            queue!(out, Print(&txt))?;
                            used += txt.width();
                        }
                    }
                }
                apply(out, base)?;
                queue!(out, Print(" ".repeat(width.saturating_sub(used))))?;
                used = width;
            }
        }
        apply(out, ui.base)?;
        queue!(out, Print(" ".repeat(width.saturating_sub(used))))?;
    }
    for k in shown.len()..body {
        queue!(out, MoveTo(0, lay.text_top + 1 + k as u16))?;
        apply(out, ui.base)?;
        queue!(out, Print(" ".repeat(width)))?;
    }
    Ok(())
}

/// Picker box placement: floats over the text area.
struct PickerBox {
    /// Box's leftmost column (centered for the small window).
    x: usize,
    compact: bool,
    list_rows: usize,
    /// Text width of left card (input, list) · right card (preview) (preview 0 when narrow — one card).
    list_w: usize,
    preview_w: usize,
}

impl PickerBox {
    /// Covers the whole text area — so text/line numbers behind don't leak. Cards inset one cell each side;
    /// when wide enough (≥ 100 cells) left list card / one-cell gap / right preview card.
    /// Vertical: half-row edge · input row · divider · list… · half-row edge.
    fn new(lay: &Layout, p: &Picker) -> Self {
        if p.compact {
            // Small card at top center: up to 60 cells wide, list as tall as its items (up to 12 rows)
            let w = lay.edit_w.saturating_sub(4).clamp(24, 60);
            let list_rows = p.counts().0.clamp(3, 12).min(lay.text_rows.saturating_sub(4).max(1));
            return PickerBox {
                x: lay.edit_w.saturating_sub(w) / 2,
                compact: true,
                list_rows,
                list_w: w - 2,
                preview_w: 0,
            };
        }
        let preview = p.preview;
        let w = lay.width.max(24);
        let h = lay.text_rows.max(6);
        let (left, right) = if preview && w >= 100 {
            let l = (w * 2 / 5).max(40);
            (l, w - 3 - l)
        } else {
            (w - 2, 0)
        };
        PickerBox {
            x: 0,
            compact: false,
            list_rows: h - 4,
            list_w: left - 2,
            preview_w: right.saturating_sub(2),
        }
    }
}

/// Have a worker thread read the selected item's file (if not loaded yet).
fn request_preview(p: &mut Picker, jobs: &crate::event::Jobs) {
    let Some(path) = p.current().and_then(|i| i.action.preview_target()).map(|(p, _)| p.to_path_buf()) else {
        return;
    };
    if p.previews.contains_key(&path) {
        return;
    }
    if p.previews.len() >= crate::picker::PREVIEW_CACHE {
        p.previews.clear();
    }
    p.previews.insert(path.clone(), crate::picker::Preview::Loading);
    let id = p.id;
    jobs.spawn(move || {
        let preview = crate::picker::load_preview(&path);
        move |ed: &mut Editor| {
            if let Some(p) = ed.picker.as_mut().filter(|p| p.id == id) {
                p.previews.insert(path, preview);
            }
        }
    });
}

/// One line of code within `width` cells (applying syntax colors `spans`). Returns the number of cells used.
#[allow(clippy::too_many_arguments)]
fn draw_code_line(
    out: &mut impl Write,
    text: &ropey::Rope,
    line: usize,
    width: usize,
    tab: usize,
    spans: &[(usize, usize, u32)],
    capture_styles: &[Option<Style>],
    base: Style,
) -> io::Result<usize> {
    let start = mv::line_start(text, line);
    let (sc, ec) = (text.byte_to_char(start), text.byte_to_char(mv::line_end(text, line)));
    let end = text.char_to_byte(ec.min(sc + width + 64));
    let s = text.byte_slice(start..end).to_string();
    let mut paint = vec![u32::MAX; s.len()];
    for &(a, b, c) in spans {
        let (a, b) = (a.max(start), b.min(end));
        if a < b {
            paint[a - start..b - start].fill(c);
        }
    }
    let mut current: Option<Style> = None;
    let mut used = 0;
    for c in graphemes::cells(&s, start, tab) {
        if c.col + c.width > width {
            break;
        }
        let g = &s[c.bytes.clone()];
        let mut st = base;
        if let Some(Some(hs)) = paint.get(c.bytes.start).and_then(|&cap| capture_styles.get(cap as usize)) {
            st = st.patch(*hs);
        }
        if current != Some(st) {
            apply(out, st)?;
            current = Some(st);
        }
        if g == "\t" {
            queue!(out, Print(" ".repeat(c.width)))?;
        } else if g.chars().next().is_some_and(char::is_control) {
            queue!(out, Print('\u{fffd}'))?;
        } else {
            queue!(out, Print(g))?;
        }
        used = c.col + c.width;
    }
    Ok(used)
}

/// Bordered box + input row + list. Returns the input row's cursor position.
fn draw_picker(
    p: &Picker,
    editor: &Editor,
    ui: &Ui,
    lay: &Layout,
    out: &mut impl Write,
) -> io::Result<(u16, u16)> {
    let b = PickerBox::new(lay, p);
    let t = &editor.theme;
    let card = card_style(ui, editor, "ui.popup");
    let pad = card_padding(ui, card);
    let dim = Style { fg: ui.virt.fg, ..card };
    let faint = Style { fg: ui.linenr.fg, ..card };
    let strong = card.patch(t.try_get("ui.text.focus").unwrap_or(Style { bold: true, ..Style::default() }));
    let selected = card.patch(t.try_get("ui.menu.selected").unwrap_or(ui.selection_primary));
    let matched = Style { fg: ui.accent.fg, bold: true, ..Style::default() };
    let lw = b.list_w;
    let (m, n) = p.counts();
    let count = if p.loading { "loading…".to_string() } else { format!("{m}/{n}") };
    // Preview target
    let target = p.current().map(|i| i.action.clone());
    let preview_title = match &target {
        Some(Action::Open(path) | Action::Goto { path, .. }) => path
            .strip_prefix(std::env::current_dir().unwrap_or_default())
            .unwrap_or(path)
            .display()
            .to_string(),
        Some(Action::Buffer(id) | Action::Jump { doc: id, .. }) => {
            editor.docs.iter().find(|d| d.id == *id).map(|d| d.display_name()).unwrap_or_default()
        }
        Some(Action::Code(i)) => match editor.action_preview(*i) {
            Some(crate::lsp_editor::ActionPreview::Ready(files)) => match files.as_slice() {
                [f] => format!(
                    "{}  {}",
                    rel_path(&f.path),
                    crate::editdiff::summary(files).split("  ").nth(1).unwrap_or("")
                ),
                _ => crate::editdiff::summary(files),
            },
            _ => String::new(),
        },
        Some(Action::ReplaceFile(n)) => match editor.replace_plan.get(*n) {
            Some(f) => format!(
                "{}  {}",
                rel_path(&f.path),
                crate::editdiff::summary(&f.diff).split("  ").nth(1).unwrap_or("")
            ),
            None => String::new(),
        },
        Some(Action::Command(_) | Action::Typed(_) | Action::Theme(_)) | None => String::new(),
    };
    let rows = b.list_rows + 4;
    let preview = if b.preview_w > 0 { preview_lines(p, editor, b.list_rows) } else { None };
    let (left_w, right_w) = (lw + 2, if b.preview_w > 0 { b.preview_w + 2 } else { 0 });
    let edge = Style { fg: card.bg, bg: ui.base.bg, ..Style::default() };
    for row in 0..rows {
        let y = lay.text_top + row as u16;
        if b.compact {
            queue!(out, MoveTo(b.x as u16, y))?;
        } else {
            queue!(out, MoveTo(0, y))?;
            apply(out, ui.base)?;
            queue!(out, Print(" "))?;
        }
        let last = row + 1 == rows;
        // ── Left card
        if row == 0 || last {
            if pad == 1 {
                apply(out, edge)?;
                queue!(out, Print((if row == 0 { "▄" } else { "▀" }).repeat(left_w)))?;
            } else {
                apply(out, faint)?;
                queue!(out, Print("─".repeat(left_w)))?;
            }
        } else if row == 1 {
            // Input row: › query ……… title · count
            apply(out, Style { fg: ui.accent.fg, bold: true, ..card })?;
            queue!(out, Print(" › "))?;
            let right = format!("{}  {count} ", p.title);
            let q = fit(&p.query, lw.saturating_sub(right.width() + 3));
            apply(out, strong)?;
            queue!(out, Print(&q))?;
            apply(out, card)?;
            queue!(out, Print(" ".repeat((lw + 2).saturating_sub(3 + q.width() + right.width()))))?;
            apply(out, faint)?;
            queue!(out, Print(fit(&right, lw.saturating_sub(q.width() + 1))))?;
        } else if row == 2 {
            apply(out, card)?;
            queue!(out, Print(" "))?;
            apply(out, faint)?;
            queue!(out, Print("─".repeat(lw)))?;
            apply(out, card)?;
            queue!(out, Print(" "))?;
        } else {
            let mut used = 0;
            match p.view.get(row - 3) {
                Some((label, idx, is_sel, hint, glyph)) => {
                    let base = if *is_sel { selected } else { card };
                    if *is_sel {
                        apply(out, Style { fg: ui.accent.fg, ..base })?;
                        queue!(out, Print("▎ "))?;
                    } else {
                        apply(out, base)?;
                        queue!(out, Print("  "))?;
                    }
                    // Kind glyph (same color as in code)
                    if let Some((g, scope)) = glyph {
                        let color = t.try_get(scope).and_then(|s| s.fg).or(ui.virt.fg);
                        apply(out, Style { fg: color, ..base })?;
                        queue!(out, Print(*g), Print(" "))?;
                        used += 2;
                    }
                    let name = if *is_sel {
                        base.patch(t.try_get("ui.text.focus").unwrap_or_default())
                    } else {
                        base
                    };
                    let name = Style { bg: base.bg, ..name };
                    // Annotation (keys, a line of code …) dimmed at the right end — the label comes first, the
                    // annotation gets what's left (cut with …)
                    let room = lw.saturating_sub(1 + used + label.width() + 2);
                    let hint = if room >= 4 { fit_ellipsis(hint, room) } else { String::new() };
                    let hint = hint.as_str();
                    let hint_w = if hint.is_empty() { 0 } else { hint.width() + 2 };
                    let label_room = lw.saturating_sub(1 + hint_w);
                    used += print_matched(out, label, idx, name, matched, label_room.saturating_sub(used))?;
                    apply(out, base)?;
                    if hint_w > 0 && hint_w < lw {
                        queue!(out, Print(" ".repeat(lw - used - hint_w + 1)))?;
                        apply(out, Style { fg: ui.virt.fg, ..base })?;
                        queue!(out, Print(hint), Print(" "))?;
                        apply(out, base)?;
                        used = lw;
                    }
                }
                None => {
                    apply(out, card)?;
                    queue!(out, Print("  "))?;
                }
            }
            queue!(out, Print(" ".repeat(lw - used)))?;
        }
        // ── Gap + right card (preview)
        if right_w > 0 {
            apply(out, ui.base)?;
            queue!(out, Print(" "))?;
            if row == 0 || last {
                if pad == 1 {
                    apply(out, edge)?;
                    queue!(out, Print((if row == 0 { "▄" } else { "▀" }).repeat(right_w)))?;
                } else {
                    apply(out, faint)?;
                    queue!(out, Print("─".repeat(right_w)))?;
                }
            } else if row == 1 {
                // Title: folder dimmed, file name crisp
                let (dir, file) = match preview_title.rfind('/') {
                    Some(k) => preview_title.split_at(k + 1),
                    None => ("", preview_title.as_str()),
                };
                let dir = fit(dir, b.preview_w.saturating_sub(file.width()));
                let file = fit(file, b.preview_w - dir.width());
                apply(out, dim)?;
                queue!(out, Print(" "), Print(&dir))?;
                apply(out, strong)?;
                queue!(out, Print(&file))?;
                apply(out, card)?;
                queue!(out, Print(" ".repeat(b.preview_w + 1 - dir.width() - file.width())))?;
            } else if row == 2 {
                apply(out, card)?;
                queue!(out, Print(" "))?;
                apply(out, faint)?;
                queue!(out, Print("─".repeat(b.preview_w)))?;
                apply(out, card)?;
                queue!(out, Print(" "))?;
            } else {
                apply(out, card)?;
                queue!(out, Print(" "))?;
                let used = match &preview {
                    Some(pv) => draw_preview_row(out, pv, row - 3, b.preview_w, editor, ui, card)?,
                    None => 0,
                };
                apply(out, card)?;
                queue!(out, Print(" ".repeat(b.preview_w - used + 1)))?;
            }
        }
        if !b.compact {
            clear_rest(out, ui.base)?;
        }
    }
    let q = fit(&p.query, lw.saturating_sub(format!("{}  {count} ", p.title).width() + 3));
    Ok(((b.x + 1 + 3 + q.width()) as u16 - u16::from(b.compact), lay.text_top + 1))
}

/// One preview page: text, syntax, first line, line to highlight — or a hint message.
enum PreviewView<'a> {
    Code {
        text: &'a ropey::Rope,
        syntax: Option<&'a crate::syntax::Syntax>,
        first: usize,
        focus: Option<usize>,
        spans: Vec<(usize, usize, u32)>,
    },
    /// Changes the code action would make — per screen row (file index, line number | None = file header).
    /// Blank row between files = usize::MAX.
    Diff {
        files: std::sync::Arc<Vec<crate::editdiff::FileDiff>>,
        rows: Vec<(usize, Option<usize>)>,
        styles: Vec<Option<Style>>,
    },
    Note(String),
}

fn preview_lines<'a>(p: &'a Picker, editor: &'a Editor, rows: usize) -> Option<PreviewView<'a>> {
    let action = &p.current()?.action;
    let (text, syntax, focus) = match action {
        Action::Buffer(id) => {
            let d = editor.docs.iter().find(|d| d.id == *id)?;
            let cur = mv::line_of(&d.text, d.selection().primary().head);
            (&d.text, d.syntax.as_ref(), Some(cur))
        }
        Action::ReplaceFile(n) => {
            let f = editor.replace_plan.get(*n)?;
            let rows = (0..f.diff[0].lines.len()).map(|li| (0, Some(li))).collect();
            let styles = crate::syntax::capture_styles(|n| editor.theme.try_get(n));
            return Some(PreviewView::Diff { files: f.diff.clone(), rows, styles });
        }
        Action::Jump { doc, line, .. } => {
            let d = editor.docs.iter().find(|d| d.id == *doc)?;
            (&d.text, d.syntax.as_ref(), Some(*line))
        }
        Action::Code(i) => {
            use crate::lsp_editor::ActionPreview;
            return Some(match editor.action_preview(*i)? {
                ActionPreview::Working => PreviewView::Note("working out the change…".into()),
                ActionPreview::CommandOnly => {
                    PreviewView::Note("runs a server command — nothing to preview".into())
                }
                ActionPreview::Failed(why) => PreviewView::Note(format!("no preview: {why}")),
                ActionPreview::Ready(files)
                    if files.iter().all(|f| f.lines.is_empty() && f.op == crate::editdiff::FileOp::Edit) =>
                {
                    PreviewView::Note("no changes".into())
                }
                ActionPreview::Ready(files) => {
                    let mut rows = Vec::new();
                    let headers =
                        files.len() > 1 || files.iter().any(|f| f.op != crate::editdiff::FileOp::Edit);
                    for (fi, f) in files.iter().enumerate() {
                        if fi > 0 {
                            rows.push((usize::MAX, None));
                        }
                        if headers {
                            rows.push((fi, None));
                        }
                        rows.extend((0..f.lines.len()).map(|li| (fi, Some(li))));
                    }
                    let styles = crate::syntax::capture_styles(|n| editor.theme.try_get(n));
                    PreviewView::Diff { files: files.clone(), rows, styles }
                }
            });
        }
        _ => {
            let (path, focus) = action.preview_target()?;
            match p.previews.get(path)? {
                crate::picker::Preview::Loading => return Some(PreviewView::Note("loading…".into())),
                crate::picker::Preview::Unavailable(why) => return Some(PreviewView::Note(why.to_string())),
                crate::picker::Preview::Ready { text, syntax } => (text, syntax.as_ref(), focus),
            }
        }
    };
    let last = mv::last_line(text);
    let first = focus.map_or(0, |f| f.saturating_sub(rows / 3)).min(last);
    let end_line = (first + rows).min(last + 1).max(first + 1) - 1;
    let spans = match syntax.filter(|s| s.tree.is_some()) {
        Some(syn) => crate::syntax::highlights(
            syn,
            text,
            mv::line_start(text, first)..mv::line_full_end(text, end_line),
            &mut QueryCursor::new(),
        ),
        None => Vec::new(),
    };
    Some(PreviewView::Code { text, syntax, first, focus, spans })
}

fn draw_preview_row(
    out: &mut impl Write,
    pv: &PreviewView,
    row: usize,
    width: usize,
    editor: &Editor,
    ui: &Ui,
    popup: Style,
) -> io::Result<usize> {
    match pv {
        PreviewView::Diff { files, rows, styles } => {
            draw_diff_row(out, files, rows.get(row).copied(), width, editor, ui, popup, styles)
        }
        PreviewView::Note(msg) => {
            if row != 0 {
                return Ok(0);
            }
            let msg = fit(&format!("  {msg}"), width);
            apply(out, Style { fg: ui.virt.fg, ..popup })?;
            queue!(out, Print(&msg))?;
            Ok(msg.width())
        }
        PreviewView::Code { text, syntax, first, focus, spans } => {
            let line = first + row;
            if line > mv::last_line(text) {
                return Ok(0);
            }
            let base = if *focus == Some(line) {
                popup.patch(Style { bg: ui.cursorline.bg, ..Style::default() })
            } else {
                popup
            };
            let num = format!("{:>4}  ", line + 1);
            apply(out, Style { fg: ui.linenr.fg, ..base })?;
            queue!(out, Print(&num))?;
            let styles: Vec<Option<Style>> = if syntax.is_some() {
                crate::syntax::capture_styles(|n| editor.theme.try_get(n))
            } else {
                Vec::new()
            };
            let used = draw_code_line(
                out,
                text,
                line,
                width - num.len(),
                editor.config.tab_width,
                spans,
                &styles,
                base,
            )?;
            if *focus == Some(line) {
                apply(out, base)?;
                queue!(out, Print(" ".repeat(width - num.len() - used)))?;
                return Ok(width);
            }
            Ok(num.len() + used)
        }
    }
}

/// Path relative to the current folder (unchanged if outside).
fn rel_path(p: &std::path::Path) -> String {
    let cwd = std::env::current_dir().unwrap_or_default();
    p.strip_prefix(&cwd).unwrap_or(p).display().to_string()
}

/// Code action preview line: file header (what it does + path + lines added/removed) or one diff line
/// (line number · −/+ · syntax-colored text — deleted lines on faint red, inserted on faint green background,
/// the changed middle a tone stronger).
#[allow(clippy::too_many_arguments)]
fn draw_diff_row(
    out: &mut impl Write,
    files: &[crate::editdiff::FileDiff],
    at: Option<(usize, Option<usize>)>,
    width: usize,
    editor: &Editor,
    ui: &Ui,
    card: Style,
    styles: &[Option<Style>],
) -> io::Result<usize> {
    use crate::editdiff::{FileOp, LineKind};
    let Some((fi, li)) = at else { return Ok(0) };
    let Some(f) = files.get(fi) else { return Ok(0) };
    let t = &editor.theme;
    let faint = Style { fg: ui.linenr.fg, ..card };
    let dim = Style { fg: ui.virt.fg, ..card };
    let strong = card.patch(t.try_get("ui.text.focus").unwrap_or(Style { bold: true, ..Style::default() }));
    let (minus_fg, plus_fg) = (ui.diff_minus.fg, ui.diff_plus.fg);
    let Some(li) = li else {
        // File header
        let path = rel_path(&f.path);
        let mut parts: Vec<(String, Style)> = match &f.op {
            FileOp::Edit => vec![("● ".into(), Style { fg: ui.accent.fg, ..card }), (path, strong)],
            FileOp::Create => vec![
                ("+ ".into(), Style { fg: plus_fg, bold: true, ..card }),
                (path, strong),
                ("  new file".into(), dim),
            ],
            FileOp::Rename(from) => vec![
                ("→ ".into(), Style { fg: ui.accent.fg, bold: true, ..card }),
                (rel_path(from), dim),
                ("  →  ".into(), faint),
                (path, strong),
            ],
            FileOp::Delete => vec![
                ("− ".into(), Style { fg: minus_fg, bold: true, ..card }),
                (path, Style { crossed: true, ..strong }),
                ("  deleted".into(), dim),
            ],
        };
        if f.added + f.removed > 0 && f.op != FileOp::Delete {
            parts.push((format!("  +{}", f.added), Style { fg: plus_fg, ..card }));
            parts.push((format!(" −{}", f.removed), Style { fg: minus_fg, ..card }));
        }
        let mut used = 0;
        for (text, st) in parts {
            let s = fit(&text, width.saturating_sub(used));
            apply(out, st)?;
            queue!(out, Print(&s))?;
            used += s.width();
        }
        return Ok(used);
    };
    let Some(l) = f.lines.get(li) else { return Ok(0) };
    if l.kind == LineKind::Gap {
        apply(out, faint)?;
        queue!(out, Print("      ⋯"))?;
        return Ok(7);
    }
    let (bg, strong_bg, sign, sign_fg) = match l.kind {
        LineKind::Minus => (blend(minus_fg, card.bg, 0.16), blend(minus_fg, card.bg, 0.34), "−", minus_fg),
        LineKind::Plus => (blend(plus_fg, card.bg, 0.16), blend(plus_fg, card.bg, 0.34), "+", plus_fg),
        _ => (card.bg, card.bg, " ", ui.linenr.fg),
    };
    let base = Style { bg: bg.or(card.bg), ..card };
    // Without truecolor (can't blend), use foreground color instead of background
    let tinted = bg.is_some() && l.kind != LineKind::Context;
    apply(out, Style { fg: ui.linenr.fg, ..base })?;
    let num = format!("{:>4} ", l.number);
    queue!(out, Print(&num))?;
    apply(out, Style { fg: sign_fg, bold: true, ..base })?;
    queue!(out, Print(sign), Print(" "))?;
    let mut used = num.len() + 2;
    let room = width.saturating_sub(used);
    let mut paint = vec![u32::MAX; l.text.len()];
    for &(a, b, c) in &l.spans {
        let (a, b) = (a.min(l.text.len()), b.min(l.text.len()));
        if a < b {
            paint[a..b].fill(c);
        }
    }
    let mut current: Option<Style> = None;
    let mut col = 0;
    for c in graphemes::cells(&l.text, 0, editor.config.tab_width) {
        if c.col + c.width > room {
            break;
        }
        let g = &l.text[c.bytes.clone()];
        let mut st = base;
        if let Some(Some(hs)) = paint.get(c.bytes.start).and_then(|&cap| styles.get(cap as usize)) {
            st = st.patch(Style { bg: None, ..*hs });
        }
        if !tinted && l.kind != LineKind::Context && st.fg == card.fg {
            st.fg = sign_fg; // terminal can't paint backgrounds: distinguish by foreground
        }
        if let Some((a, b)) = l.emph
            && (a..b).contains(&c.bytes.start)
        {
            st.bg = strong_bg.or(st.bg);
        }
        if current != Some(st) {
            apply(out, st)?;
            current = Some(st);
        }
        if g == "\t" {
            queue!(out, Print(" ".repeat(c.width)))?;
        } else if g.chars().next().is_some_and(char::is_control) {
            queue!(out, Print('\u{fffd}'))?;
        } else {
            queue!(out, Print(g))?;
        }
        col = c.col + c.width;
    }
    used += col;
    // Deleted/inserted lines get background to the end
    if tinted {
        apply(out, base)?;
        queue!(out, Print(" ".repeat(width.saturating_sub(used))))?;
        used = width;
    }
    Ok(used)
}

fn fit(s: &str, width: usize) -> String {
    let mut out = String::new();
    let mut w = 0;
    for ch in s.chars() {
        let cw = ch.width().unwrap_or(0);
        if w + cw > width {
            break;
        }
        w += cw;
        out.push(ch);
    }
    out
}

/// Top row: `project › folder › file ● › ƒ definition` ……… open buffers (current one bold).
/// When narrow, drop less important parts first (project, folders, outer definitions).
fn draw_header(editor: &Editor, ui: &Ui, lay: &Layout, out: &mut impl Write) -> io::Result<()> {
    let doc = editor.doc();
    let t = &editor.theme;
    let dim = Style { fg: ui.virt.fg, ..ui.base };
    let faint = Style { fg: ui.linenr.fg, ..ui.base };
    let strong =
        ui.base.patch(t.try_get("ui.text.focus").unwrap_or(Style { bold: true, ..Style::default() }));
    let sep = |keep| Seg { text: "  ›  ".into(), style: faint, keep };
    let mut left = vec![Seg { text: " ".into(), style: ui.base, keep: 255 }];
    let cwd = std::env::current_dir().ok();
    let project = cwd.as_ref().and_then(|d| d.file_name()).map(|p| p.to_string_lossy().into_owned());
    // Tests: a fixed project name (the checkout folder's name varies — worktrees, CI)
    #[cfg(test)]
    let project = project.map(|_| "tarae".to_string());
    if let Some(p) = project {
        left.push(Seg { text: p, style: dim, keep: 20 });
        left.push(sep(20));
    }
    let rel = doc.path.as_ref().map(|p| cwd.as_ref().and_then(|c| p.strip_prefix(c).ok()).unwrap_or(p));
    let dirs: Vec<String> = rel
        .and_then(|r| r.parent())
        .map(|d| d.iter().map(|c| c.to_string_lossy().into_owned()).collect())
        .unwrap_or_default();
    for (i, d) in dirs.iter().enumerate() {
        let keep = 30 + i as u8; // folders closer to the file survive longer
        left.push(Seg { text: d.clone(), style: dim, keep });
        left.push(sep(keep));
    }
    if editor.welcome() {
        left.pop(); // separator after the project
    } else {
        left.push(Seg { text: doc.display_name_short(), style: strong, keep: 250 });
    }
    if doc.loading {
        left.push(Seg { text: " …".into(), style: dim, keep: 240 });
    } else if doc.is_modified() {
        left.push(Seg { text: " ●".into(), style: ui.base.patch(ui.accent), keep: 245 });
    }
    if doc.disk_conflict {
        let warn = t.try_get("warning").map(|s| Style { bg: None, ..s }).unwrap_or_default();
        left.push(Seg { text: "  ▲ changed on disk".into(), style: ui.base.patch(warn), keep: 246 });
    }
    if let Some(syn) = doc.syntax.as_ref() {
        let head = doc.selection().primary().cursor(&doc.text);
        let path = syntax::scope_path(syn, &doc.text, head);
        let n = path.len();
        for (i, (kind, name)) in path.into_iter().enumerate() {
            let keep = 100 + (i * 10 / n.max(1)) as u8; // inner definitions survive longer
            let (glyph, scope) = match kind {
                'f' => ("ƒ", "function"),
                'm' => ("§", "namespace"),
                _ => ("τ", "type"),
            };
            left.push(sep(keep));
            let color = t.try_get(scope).map_or(dim, |s| Style { bg: None, bold: false, italic: false, ..s });
            left.push(Seg { text: format!("{glyph} "), style: ui.base.patch(color), keep });
            left.push(Seg { text: name, style: dim, keep });
        }
    }
    let mut right = Vec::new();
    if editor.docs.len() > 1 {
        for (i, d) in editor.docs.iter().enumerate() {
            let current = i == editor.current;
            let mark = if d.is_modified() { " ●" } else { "" };
            let keep = if current { 90 } else { 10 };
            right.push(Seg {
                text: format!("{}{mark}  ", d.display_name_short()),
                style: if current { strong } else { dim },
                keep,
            });
        }
    }
    fit_segments(&mut left, &mut right, lay.width);
    queue!(out, MoveTo(0, 0))?;
    let mut used = 0;
    for s in &left {
        apply(out, s.style)?;
        queue!(out, Print(&s.text))?;
        used += s.text.width();
    }
    let right_w: usize = right.iter().map(|s| s.text.width()).sum();
    apply(out, ui.base)?;
    queue!(out, Print(" ".repeat(lay.width.saturating_sub(used + right_w))))?;
    for s in &right {
        apply(out, s.style)?;
        queue!(out, Print(&s.text))?;
    }
    clear_rest(out, ui.base)
}

/// A statusline segment. Lowest `keep` is dropped first (when width runs short).
struct Seg {
    text: String,
    style: Style,
    keep: u8,
}

/// Drop less important segments from left/right until they fit in `width`.
fn fit_segments(left: &mut Vec<Seg>, right: &mut Vec<Seg>, width: usize) {
    let total = |l: &Vec<Seg>, r: &Vec<Seg>| l.iter().chain(r).map(|s| s.text.width()).sum::<usize>();
    // A blank spacer right after a segment with the same `keep` belongs to it — they go together, or a
    // stray gap is left behind (and the fill that should separate left from right goes missing)
    let drop = |v: &mut Vec<Seg>, i: usize| {
        let keep = v.remove(i).keep;
        if v.get(i).is_some_and(|s| s.keep == keep && s.text.trim().is_empty()) {
            v.remove(i);
        }
    };
    // Left and right never touch (`main● 1`) — keep at least a two-cell gap while both sides have content
    let gap = |l: &Vec<Seg>, r: &Vec<Seg>| if l.is_empty() || r.is_empty() { 0 } else { 2 };
    while total(left, right) + gap(left, right) > width {
        let lmin = left.iter().enumerate().min_by_key(|(_, s)| s.keep).map(|(i, s)| (s.keep, i));
        let rmin = right.iter().enumerate().min_by_key(|(_, s)| s.keep).map(|(i, s)| (s.keep, i));
        match (lmin, rmin) {
            (Some((lk, li)), Some((rk, _))) if lk <= rk => drop(left, li),
            (_, Some((_, ri))) => drop(right, ri),
            (Some((_, li)), None) => drop(left, li),
            (None, None) => break,
        }
    }
}

/// tarae statusline: `▐ NORMAL ▌  branch ……… claude · diag · progress · lang · encoding EOL · sel · pos`
/// Mode = pill (half blocks widen its bg half a cell each side), the rest dimmed — only mode, diagnostics,
/// position catch the eye. With the header off, the file name goes here too. Symbols only from common
/// monospace fonts (● ▲ · … ▐ ▌).
fn draw_statusline(editor: &Editor, ui: &Ui, lay: &Layout, out: &mut impl Write) -> io::Result<()> {
    let doc = editor.doc();
    let t = &editor.theme;
    let dim = Style { fg: ui.virt.fg.or(ui.linenr.fg), ..ui.status };
    let accent = ui.status.patch(ui.accent);
    let strong =
        ui.status.patch(t.try_get("ui.text.focus").unwrap_or(Style { bold: true, ..Style::default() }));
    let mode = match editor.mode {
        Mode::Normal => "NORMAL",
        Mode::Insert => "INSERT",
        Mode::Select => "SELECT",
    };
    let mut left = Vec::new();
    match ui.status_mode.bg.filter(|bg| Some(*bg) != ui.status.bg) {
        Some(pill) => {
            let edge = Style { fg: Some(pill), bg: ui.status.bg, ..Style::default() };
            left.push(Seg { text: " ▐".into(), style: edge, keep: 255 });
            left.push(Seg { text: format!(" {mode} "), style: ui.status_mode, keep: 255 });
            left.push(Seg { text: "▌".into(), style: edge, keep: 255 });
        }
        None => left.push(Seg { text: format!(" {} ", &mode[..3]), style: ui.status_mode, keep: 255 }),
    }
    if !lay.header {
        let modified = if doc.is_modified() { " ●" } else { "" };
        left.push(Seg { text: format!("  {}", doc.display_name()), style: strong, keep: 200 });
        left.push(Seg { text: modified.into(), style: accent, keep: 200 });
    }
    if let Some(b) = &editor.git_branch {
        left.push(Seg { text: format!("   {b}"), style: dim, keep: 80 });
    }
    // How much this file changed from HEAD: +added/modified lines −removed lines (colors match the git bar)
    let (added, removed) = doc.git_hunks.iter().fold((0, 0), |(a, r), h| (a + h.lines.len(), r + h.removed));
    if added + removed > 0 {
        let color =
            |k: &str| ui.status.patch(Style { fg: t.try_get(k).and_then(|s| s.fg), ..Style::default() });
        if added > 0 {
            left.push(Seg { text: format!("  +{added}"), style: color("diff.plus"), keep: 85 });
        }
        if removed > 0 {
            left.push(Seg { text: format!("  −{removed}"), style: color("diff.minus"), keep: 85 });
        }
    }

    let sel = doc.selection();
    let pos = cursor_idx(editor.mode, sel.primary(), &doc.text);
    let line = mv::line_of(&doc.text, pos);
    let col = doc.text.byte_slice(mv::line_start(&doc.text, line)..pos).len_chars() + 1; // char col for users
    let gap = |keep| Seg { text: "   ".into(), style: ui.status, keep };
    let mut right = Vec::new();
    match &editor.review {
        Some(r) if r.ready => {
            right.push(Seg {
                text: "claude ● review".into(),
                style: Style { bold: true, ..accent },
                keep: 220,
            });
            right.push(gap(220));
        }
        Some(r) => {
            right.push(Seg { text: format!("{} claude", wave(r.started)), style: accent, keep: 220 });
            right.push(gap(220));
        }
        None => {}
    }
    // Claude Code in the adjacent pane is attached (sees selection/diagnostics; edits come here as reviews)
    if editor.agent.as_ref().is_some_and(|a| a.connected()) && editor.review.is_none() {
        right.push(Seg { text: "◦ claude code".into(), style: accent, keep: 150 });
        right.push(gap(150));
    }
    let [errors, warnings] = doc.diagnostic_counts();
    if errors > 0 {
        right.push(Seg {
            text: format!("● {errors}"),
            style: ui.status.patch(Style { bg: None, ..ui.error }),
            keep: 210,
        });
        right.push(gap(210));
    }
    if warnings > 0 {
        let warn = t.try_get("warning").map(|s| Style { bg: None, ..s }).unwrap_or_default();
        right.push(Seg { text: format!("▲ {warnings}"), style: ui.status.patch(warn), keep: 210 });
        right.push(gap(210));
    }
    if let Some(h) = &editor.search_hits {
        let n = h.matches.len();
        let count = match h.current(sel.primary()) {
            Some(i) => format!("{i}/{n}{}", if h.capped { "+" } else { "" }),
            None if n == 0 && !h.capped => "no matches".to_string(),
            None => format!("{n}{} matches", if h.capped { "+" } else { "" }),
        };
        right.push(Seg { text: format!("/{}  ", fit_ellipsis(&h.pattern, 24)), style: dim, keep: 205 });
        right.push(Seg { text: count, style: accent, keep: 206 });
        right.push(gap(206));
    }
    if let Some(p) = editor.lsp_progress() {
        right.push(Seg { text: fit(&p, 40), style: dim, keep: 100 });
        right.push(gap(100));
    }
    if let Some(s) = &doc.syntax {
        right.push(Seg { text: s.lang.name.clone(), style: dim, keep: 60 });
        right.push(gap(60));
    }
    let crlf = doc.text.len_lines() > 1 && mv::line_full_end(&doc.text, 0) - mv::line_end(&doc.text, 0) == 2;
    right.push(Seg { text: format!("utf-8 {}", if crlf { "CRLF" } else { "LF" }), style: dim, keep: 40 });
    right.push(gap(40));
    if sel.len() > 1 {
        right.push(Seg { text: format!("{} sels", sel.len()), style: accent, keep: 150 });
        right.push(gap(150));
    }
    right.push(Seg { text: format!("{}:{} ", line + 1, col), style: strong, keep: 250 });

    fit_segments(&mut left, &mut right, lay.width);
    queue!(out, MoveTo(0, lay.status_y))?;
    let mut used = 0;
    for s in &left {
        apply(out, s.style)?;
        queue!(out, Print(&s.text))?;
        used += s.text.width();
    }
    let right_w: usize = right.iter().map(|s| s.text.width()).sum();
    apply(out, ui.status)?;
    queue!(out, Print(" ".repeat(lay.width.saturating_sub(used + right_w))))?;
    for s in &right {
        apply(out, s.style)?;
        queue!(out, Print(&s.text))?;
    }
    clear_rest(out, ui.status)
}

/// Completion list — below the word so the name column lines up with the typed word (above if no room).
///
/// ```text
/// ▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄   ← half-row margin (card: when the theme has a floating bg color)
/// ▎ƒ get          Option<&V>  ← selected row: accent bar + strong background
///  ƒ get_mut   Option<&mut V>▐ ← kind glyph in code colors, typed chars in accent, right edge = scrollbar
/// ▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀
/// ```
fn draw_completion(
    c: &crate::completion::Completion,
    (cx, cy): (u16, u16),
    ui: &Ui,
    editor: &Editor,
    lay: &Layout,
    out: &mut impl Write,
) -> io::Result<()> {
    const ROWS: usize = 10;
    const LABEL_MAX: usize = 42;
    const DETAIL_MAX: usize = 30;
    let t = &editor.theme;
    let card = card_style(ui, editor, "ui.menu");
    let selected = card.patch(t.try_get("ui.menu.selected").unwrap_or(ui.selection_primary));
    let dim = Style { fg: ui.virt.fg, ..card };
    let matched = Style { bold: true, ..Style { fg: ui.accent.fg, ..Style::default() } };
    let pad = card_padding(ui, card);
    let n = c.shown.len();
    let rows = n.min(ROWS).min(lay.text_rows.saturating_sub(2 * pad));
    if rows == 0 {
        return Ok(());
    }
    let first = c.selected.map_or(0, |s| s.saturating_sub(rows - 1));
    let visible = first..first + rows;
    let label_w = visible.clone().map(|i| c.item(i).label.width()).max().unwrap_or(0).min(LABEL_MAX);
    let detail_w = visible.clone().map(|i| c.item(i).detail.width()).max().unwrap_or(0).min(DETAIL_MAX);
    let scroll = n > rows;
    // [bar][glyph][ ][name][   detail][ |scroll]
    let width = (3 + label_w + if detail_w > 0 { 3 + detail_w } else { 0 } + 2).min(lay.edit_w);
    let doc = editor.doc();
    let head = doc.selection().primary().head;
    let typed = mv::visual_col(&doc.text, head, editor.config.tab_width)
        - mv::visual_col(&doc.text, c.start.min(head), editor.config.tab_width);
    let word_x = (cx as usize).saturating_sub(typed);
    let x0 = word_x.saturating_sub(3).min(lay.edit_w.saturating_sub(width));
    let (top, bottom) = (lay.text_top as usize, lay.text_top as usize + lay.text_rows);
    let h = rows + 2 * pad;
    let below = cy as usize + 1 + h <= bottom;
    let y0 = if below { cy as usize + 1 } else { (cy as usize).saturating_sub(h).max(top) };
    card_edge(out, ui, card, x0, y0, width, pad, true)?;
    let thumb = scroll_thumb(rows, n, first);
    for row in 0..rows {
        let i = first + row;
        let item = c.item(i);
        let is_sel = c.selected == Some(i);
        let base = if is_sel { selected } else { card };
        queue!(out, MoveTo(x0 as u16, (y0 + pad + row) as u16))?;
        if is_sel {
            apply(out, Style { fg: ui.accent.fg, ..base })?;
            queue!(out, Print("▎"))?;
        } else {
            apply(out, base)?;
            queue!(out, Print(" "))?;
        }
        let glyph = crate::completion::kind_glyph(item.kind);
        let color = t.try_get(crate::completion::kind_scope(item.kind)).map_or(dim, |s| Style {
            bg: None,
            bold: false,
            italic: false,
            ..s
        });
        apply(out, base.patch(color))?;
        queue!(out, Print(glyph), Print(" "))?;
        // Name: highlight typed chars, … on overflow
        let label = fit_ellipsis(&item.label, label_w);
        let hits = c.hits.get(i).map_or(&[][..], Vec::as_slice);
        let name = if is_sel { base.patch(t.try_get("ui.text.focus").unwrap_or_default()) } else { base };
        let name = Style { bg: base.bg, ..name };
        print_matched(out, &label, hits, name, matched, label_w)?;
        apply(out, base)?;
        queue!(out, Print(" ".repeat(label_w - label.width())))?;
        let mut used = 3 + label_w;
        if detail_w > 0 {
            let d = fit_ellipsis(&item.detail, detail_w);
            apply(out, Style { fg: ui.virt.fg, ..base })?;
            queue!(out, Print("   "), Print(" ".repeat(detail_w - d.width())), Print(&d))?;
            used += 3 + detail_w;
        }
        apply(out, base)?;
        queue!(out, Print(" ".repeat(width.saturating_sub(used + 1))))?;
        if scroll && thumb.contains(&row) {
            apply(out, Style { fg: ui.linenr.fg, ..card })?;
            queue!(out, Print("▐"))?;
        } else {
            apply(out, card)?;
            queue!(out, Print(" "))?;
        }
    }
    card_edge(out, ui, card, x0, y0 + pad + rows, width, pad, false)?;
    // Selected item's docs: one cell right of the list (left if no room), top-aligned with the list
    if let Some((_, lines)) = c.docs.as_ref().filter(|(_, l)| !l.is_empty()) {
        const MIN: usize = 24;
        let natural = lines.iter().map(crate::markdown::Line::width).max().unwrap_or(0).max(MIN);
        let right = lay.edit_w.saturating_sub(x0 + width + 1);
        // If the list floats above the cursor, keep the docs off the cursor line too
        let limit = if below { bottom } else { cy as usize };
        let side = if right >= MIN + 4 {
            Some((x0 + width + 1, natural.min(64).min(right - 4)))
        } else if x0 > MIN + 4 {
            let inner = natural.min(64).min(x0 - 5);
            Some((x0 - inner - 5, inner))
        } else {
            None
        };
        match side {
            Some((x, inner)) => {
                let wrapped = crate::markdown::wrap(lines, inner, dim);
                let max_rows = limit.saturating_sub(y0 + 2).clamp(1, 16);
                draw_doc_box(&wrapped, (x, y0), inner, max_rows, ui, editor, out)?;
            }
            // No room beside: outside the list (below a downward list, above an upward one), same width
            None => {
                let inner = width.max(MIN + 4).min(lay.edit_w).saturating_sub(4);
                let wrapped = crate::markdown::wrap(lines, inner, dim);
                let room = if below { bottom.saturating_sub(y0 + h) } else { y0.saturating_sub(top) };
                if room >= 4 {
                    let max_rows = (room - 2).min(12);
                    let dh = wrapped.len().min(max_rows) + 2;
                    let y = if below { y0 + h } else { y0 - dh };
                    draw_doc_box(&wrapped, (x0, y), inner, max_rows, ui, editor, out)?;
                }
            }
        }
    }
    Ok(())
}

/// Signature card — **above** the cursor line (completion opens below, so no overlap), below if no room above
/// (skipped if the list is open). Colored by that language's grammar; current param bold accent + underline.
/// Long signatures fold the front into `…` to keep the current param visible. Param docs dimmed, ≤ 3 lines.
fn draw_signature(
    sig: &crate::signature::Signature,
    (cx, cy): (u16, u16),
    ui: &Ui,
    editor: &Editor,
    lay: &Layout,
    out: &mut impl Write,
) -> io::Result<()> {
    if sig.doc != editor.doc().id {
        return Ok(());
    }
    let card = card_style(ui, editor, "ui.popup");
    let pad = card_padding(ui, card);
    let lang = editor.doc().syntax.as_ref().map(|s| s.lang.name.clone());
    let label = sig.label.replace(['\n', '\t'], " ");
    // Signature → (char, style) cells, overlaying emphasis on the current param span only
    let mut cells: Vec<(char, Style)> = Vec::new();
    let mut byte = 0;
    let active = Style { fg: ui.accent.fg, bold: true, underline: true, ..Style::default() };
    for line in crate::markdown::code_lines(&label, lang.as_deref(), &editor.theme) {
        if let crate::markdown::Line::Code(spans) = line {
            for sp in spans {
                for ch in sp.text.chars() {
                    let on = sig.active.is_some_and(|(a, b)| a <= byte && byte < b);
                    cells.push((
                        ch,
                        if on { card.patch(sp.style).patch(active) } else { card.patch(sp.style) },
                    ));
                    byte += ch.len_utf8();
                }
            }
        }
    }
    let counter = if sig.count > 1 { format!("  {}/{}", sig.index + 1, sig.count) } else { String::new() };
    let full: usize = cells.iter().map(|(c, _)| c.width().unwrap_or(0)).sum();
    // So room (= inner − count indicator) never goes below 0 even on very narrow screens
    let inner =
        (full + counter.width()).clamp(10, 96).min(lay.edit_w.saturating_sub(4)).max(counter.width() + 4);
    let room = inner - counter.width();
    // If long, cut the front so the current param lands at the 1/3 mark
    let active_col: usize = sig
        .active
        .map(|(a, _)| label[..a.min(label.len())].chars().map(|c| c.width().unwrap_or(0)).sum())
        .unwrap_or(0);
    let skip_cols = if full > room { active_col.saturating_sub(room / 3).min(full - room + 1) } else { 0 };
    let docs = if sig.docs.is_empty() {
        Vec::new()
    } else {
        let lines = crate::markdown::render(&sig.docs, &editor.theme, lang.as_deref());
        crate::markdown::wrap(&lines, inner, Style { fg: ui.virt.fg, ..Style::default() })
    };
    let doc_rows = docs.len().min(3);
    let h = 1 + doc_rows + 2 * pad;
    let (top, bottom) = (lay.text_top as usize, lay.text_top as usize + lay.text_rows);
    let y0 = if cy as usize >= top + h {
        cy as usize - h
    } else if editor.completion.is_none() && cy as usize + 1 + h <= bottom {
        cy as usize + 1
    } else {
        return Ok(());
    };
    let width = inner + 4;
    let x0 = (cx as usize).saturating_sub(2).min(lay.edit_w.saturating_sub(width));
    card_edge(out, ui, card, x0, y0, width, pad, true)?;
    // Signature row
    let y = y0 + pad;
    queue!(out, MoveTo(x0 as u16, y as u16))?;
    apply(out, card)?;
    queue!(out, Print("  "))?;
    let mut used = 0;
    let mut col = 0;
    if skip_cols > 0 {
        apply(out, Style { fg: ui.virt.fg, ..card })?;
        queue!(out, Print("…"))?;
        used = 1;
    }
    let mut current = Style::default();
    for &(ch, st) in &cells {
        let w = ch.width().unwrap_or(0);
        col += w;
        if col <= skip_cols {
            continue;
        }
        if used + w > room {
            break;
        }
        if st != current {
            apply(out, st)?;
            current = st;
        }
        queue!(out, Print(ch))?;
        used += w;
    }
    apply(out, card)?;
    queue!(out, Print(" ".repeat(inner - used - counter.width())))?;
    apply(out, Style { fg: ui.linenr.fg, ..card })?;
    queue!(out, Print(&counter), Print("  "))?;
    // Param docs
    for (i, spans) in docs.iter().take(doc_rows).enumerate() {
        queue!(out, MoveTo(x0 as u16, (y + 1 + i) as u16))?;
        apply(out, card)?;
        queue!(out, Print("  "))?;
        let mut used = 0;
        for sp in spans {
            let t = fit(&sp.text, inner - used);
            apply(
                out,
                card.patch(Style { fg: ui.virt.fg, ..Style::default() })
                    .patch(Style { fg: sp.style.fg.or(ui.virt.fg), ..sp.style }),
            )?;
            queue!(out, Print(&t))?;
            used += t.width();
        }
        apply(out, card)?;
        queue!(out, Print(" ".repeat(inner - used + 2)))?;
    }
    card_edge(out, ui, card, x0, y + 1 + doc_rows, width, pad, false)
}

/// Welcome screen (no file): spaced-out name · one-line intro · first-step keys · recent files (number keys).
/// One block in the middle — first-timers instantly know what to press.
fn draw_welcome(editor: &Editor, ui: &Ui, lay: &Layout, out: &mut impl Write) -> io::Result<()> {
    let t = &editor.theme;
    let dim = Style { fg: ui.virt.fg, ..ui.base };
    let faint = Style { fg: ui.linenr.fg, ..ui.base };
    let strong =
        ui.base.patch(t.try_get("ui.text.focus").unwrap_or(Style { bold: true, ..Style::default() }));
    let key = Style { fg: ui.accent.fg, bold: true, ..ui.base };
    let home = std::env::var("HOME").unwrap_or_default();
    let mut rows: Vec<Vec<(String, Style)>> = vec![
        vec![("t a r a e".into(), Style { fg: ui.accent.fg, bold: true, ..ui.base })],
        vec![("타래 · Helix keys, Claude inside".into(), dim)],
        vec![],
    ];
    for (k, d) in [
        ("space f", "Open a file"),
        ("space /", "Search in project"),
        ("space l", "Chat with Claude"),
        ("space ?", "Find a command"),
        (":tutor", "Learn the keys in 10 minutes"),
        (":q", "Quit"),
    ] {
        rows.push(vec![(format!("{k:<9}"), key), (d.into(), ui.base)]);
    }
    if !editor.recent.is_empty() {
        rows.push(vec![]);
        rows.push(vec![("Recent".into(), faint)]);
        for (i, p) in editor.recent.iter().take(5).enumerate() {
            let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let dir = p.parent().map(|d| d.display().to_string()).unwrap_or_default();
            let dir = match dir.strip_prefix(&home) {
                Some(rest) if !home.is_empty() => format!("~{rest}"),
                _ => dir,
            };
            rows.push(vec![(format!("{:<9}", i + 1), key), (format!("{name}  "), strong), (dir, faint)]);
        }
    }
    let block_w = rows
        .iter()
        .map(|r| r.iter().map(|(s, _)| s.width()).sum::<usize>())
        .max()
        .unwrap_or(0)
        .min(lay.edit_w.saturating_sub(4))
        .max(32);
    let top = lay.text_top as usize + lay.text_rows.saturating_sub(rows.len() + 2) / 2;
    let x0 = lay.edit_w.saturating_sub(block_w) / 2;
    for row in 0..lay.text_rows {
        let y = lay.text_top as usize + row;
        queue!(out, MoveTo(0, y as u16))?;
        apply(out, ui.base)?;
        queue!(out, Print(" ".repeat(lay.edit_w)))?;
        if let Some(r) = (y >= top).then(|| rows.get(y - top)).flatten() {
            // First two rows (name, intro) centered, the rest aligned to the block's left
            let w: usize = r.iter().map(|(s, _)| s.width()).sum();
            let x = if y - top < 2 { lay.edit_w.saturating_sub(w) / 2 } else { x0 };
            queue!(out, MoveTo(x as u16, y as u16))?;
            let mut used = 0;
            for (s, st) in r {
                let s = fit_ellipsis(s, block_w.saturating_sub(used));
                apply(out, *st)?;
                queue!(out, Print(&s))?;
                used += s.width();
            }
        }
    }
    // Bottom center: version · theme
    let foot = format!("tarae {} · {}", env!("CARGO_PKG_VERSION"), t.name);
    queue!(
        out,
        MoveTo((lay.edit_w.saturating_sub(foot.width()) / 2) as u16, lay.text_top + lay.text_rows as u16 - 1)
    )?;
    apply(out, faint)?;
    queue!(out, Print(foot))?;
    Ok(())
}

/// Toasts: stacked top right of the editing area. Left bar color = kind (info · success · warning · error),
/// fade in on appear (150 ms), fade out before vanishing (400 ms) — blending even the card bg into the editor
fn draw_toasts(editor: &Editor, ui: &Ui, lay: &Layout, out: &mut impl Write) -> io::Result<()> {
    use crate::editor::ToastKind;
    if editor.toasts.is_empty() || editor.picker.is_some() {
        return Ok(());
    }
    let t = &editor.theme;
    let card = card_style(ui, editor, "ui.popup");
    let pad = card_padding(ui, card);
    let max_inner = (lay.edit_w / 2).clamp(24, 56).min(lay.edit_w.saturating_sub(8));
    let mut y = lay.text_top as usize;
    let bottom = lay.text_top as usize + lay.text_rows;
    for toast in editor.toasts.iter().rev() {
        let a = toast.alpha();
        let color = match toast.kind {
            ToastKind::Info => ui.accent.fg,
            ToastKind::Success => t.try_get("diff.plus").and_then(|s| s.fg),
            ToastKind::Warning => t.try_get("warning").and_then(|s| s.fg),
            ToastKind::Error => ui.error.fg,
        };
        let bg = blend(card.bg, ui.tint, a).or(card.bg);
        let fade = |c: Option<crossterm::style::Color>| blend(c, bg, a).or(c);
        let body = Style { fg: fade(card.fg), bg, ..Style::default() };
        let bar = Style { fg: fade(color), bg, ..Style::default() };
        // Messages with newlines (language server notices etc.) wrap per paragraph
        let para: Vec<crate::markdown::Line> = toast
            .text
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| crate::markdown::Line::Text {
                spans: vec![crate::markdown::Span {
                    text: l.trim_end().to_string(),
                    style: Style::default(),
                }],
                indent: 0,
            })
            .collect();
        let natural = toast.text.lines().map(|l| l.trim_end().width()).max().unwrap_or(0).min(max_inner);
        let lines: Vec<String> = crate::markdown::wrap(&para, natural.max(1), Style::default())
            .into_iter()
            .take(4)
            .map(|l| l.into_iter().map(|s| s.text).collect::<String>())
            .collect();
        let inner = lines.iter().map(|l| l.width()).max().unwrap_or(0);
        let width = inner + 5; // margin 1 · bar · margin 1 · text · margin 2
        let h = lines.len() + 2 * pad;
        if y + h > bottom {
            break;
        }
        let x0 = lay.edit_w.saturating_sub(width + 1 + usize::from(lay.scrollbar));
        let edge_card = Style { bg, ..card };
        card_edge(out, ui, edge_card, x0, y, width, pad, true)?;
        for (i, l) in lines.iter().enumerate() {
            queue!(out, MoveTo(x0 as u16, (y + pad + i) as u16))?;
            apply(out, body)?;
            queue!(out, Print(" "))?;
            apply(out, bar)?;
            queue!(out, Print(if i == 0 { "▎" } else { " " }))?;
            apply(out, body)?;
            queue!(out, Print(" "), Print(l), Print(" ".repeat(inner - l.width() + 2)))?;
        }
        card_edge(out, ui, edge_card, x0, y + pad + lines.len(), width, pad, false)?;
        y += h + usize::from(pad == 0);
    }
    Ok(())
}

/// Grammar download offer (like IntelliJ's "install plugin" notice) — bottom right of the editing area:
///
/// ```text
///  ▎ Rust syntax colors
///    Download and build the tree-sitter grammar?
///
///    y Install   n Not now
/// ```
///
/// While downloading, a wave + "Fetching and building…"; on failure the reason + `y Retry` `n Dismiss`.
struct OfferCard {
    x: usize,
    y: usize,
    inner: usize,
    /// (text, dimmed?)
    lines: Vec<(String, bool)>,
    /// Button row (None while downloading)
    buttons_row: Option<usize>,
    /// (row, start col, end col, key) — for mouse (`Screen.offer_buttons`; digit = bring back card N forward)
    buttons: Vec<(usize, usize, usize, char)>,
    labels: [&'static str; 2],
    pad: usize,
    failed: bool,
    /// When stacked, the "1/3" at the right of the title row (else empty).
    badge: String,
    /// Downloading — the wave at the start of the second line in accent.
    busy: bool,
    /// Cards stacked behind (nearest first): (depth 1.., title, start time if downloading, failed?).
    peeks: Vec<(usize, String, Option<std::time::Instant>, bool)>,
}

/// How many cells a back card is inset on each side relative to the one in front.
const PEEK_INSET: usize = 2;

impl OfferCard {
    fn new(editor: &Editor, lay: &Layout, ui: &Ui) -> Option<Self> {
        use crate::offer::OfferState;
        let o = editor.offers.first()?;
        if editor.picker.is_some() || !editor.pending.is_empty() || lay.text_rows < 8 || lay.edit_w == 0 {
            return None;
        }
        let pad = card_padding(ui, card_style(ui, editor, "ui.popup"));
        let max_inner = (lay.edit_w / 2).clamp(30, 56).min(lay.edit_w.saturating_sub(8));
        let count = editor.offers.len();
        let badge = if count > 1 { format!("1/{count}") } else { String::new() };
        let title = o.title.clone();
        let (mut lines, labels) = match &o.state {
            OfferState::Asking => {
                (vec![(title, false), (o.question.clone(), true)], Some(["Install", "Not now"]))
            }
            OfferState::Installing(t) => {
                (vec![(title, false), (format!("{} {}", wave(*t), o.working), true)], None)
            }
            OfferState::Failed(why) => {
                let mut v = vec![(o.failed.clone(), false)];
                let para = [crate::markdown::Line::Text {
                    spans: vec![crate::markdown::Span { text: why.clone(), style: Style::default() }],
                    indent: 0,
                }];
                v.extend(
                    crate::markdown::wrap(&para, max_inner, Style::default())
                        .into_iter()
                        .take(3)
                        .map(|l| (l.into_iter().map(|s| s.text).collect::<String>(), true)),
                );
                (v, Some(["Retry", "Dismiss"]))
            }
        };
        let badge_w = if badge.is_empty() { 0 } else { badge.width() + 2 };
        for (i, l) in lines.iter_mut().enumerate() {
            l.0 = fit_ellipsis(&l.0, max_inner.saturating_sub(if i == 0 { badge_w } else { 0 }));
        }
        let labels = labels.unwrap_or(["", ""]);
        let has_buttons = !labels[0].is_empty();
        // Button row: "y Install   n Not now"
        let btn_w = 2 + labels[0].width() + 3 + 2 + labels[1].width();
        // Keep card width steady across states (ask → downloading → failed): ask text width is the floor
        // The longer of the ask and downloading texts (including the four wave cells)
        let ask = o.question.width().max(o.working.width() + 4);
        let inner = lines
            .iter()
            .enumerate()
            .map(|(i, l)| l.0.width() + if i == 0 { badge_w } else { 0 })
            .max()
            .unwrap_or(0)
            .max(if has_buttons { btn_w } else { 0 })
            .max(ask.min(max_inner));
        let width = inner + 5;
        let h = lines.len() + if has_buttons { 2 } else { 0 } + 2 * pad;
        let top = lay.text_top as usize;
        let bottom = top + lay.text_rows;
        let y = bottom.checked_sub(h + 1)?.max(top);
        let x = lay.edit_w.saturating_sub(width + 1 + usize::from(lay.scrollbar));
        // Back cards: one row each above the front card (as room allows, max three — beyond that "1/N" tells)
        let room = y.saturating_sub(top + pad);
        let peeks = editor.offers[1..]
            .iter()
            .take(3.min(room))
            .enumerate()
            .filter(|(k, _)| width > 2 * PEEK_INSET * (k + 1) + 8)
            .map(|(k, p)| {
                let started = match p.state {
                    OfferState::Installing(t) => Some(t),
                    _ => None,
                };
                (k + 1, p.title.clone(), started, matches!(p.state, OfferState::Failed(_)))
            })
            .collect::<Vec<_>>();
        let buttons_row = has_buttons.then_some(y + pad + lines.len() + 1);
        let mut buttons = Vec::new();
        if let Some(row) = buttons_row {
            let b0 = x + 3;
            let e0 = b0 + 2 + labels[0].width();
            let b1 = e0 + 3;
            buttons.push((row, b0, e0, 'y'));
            buttons.push((row, b1, b1 + 2 + labels[1].width(), 'n'));
        }
        for (k, ..) in &peeks {
            let px = x + PEEK_INSET * k;
            buttons.push((y - k, px, px + width - 2 * PEEK_INSET * k, char::from(b'0' + *k as u8)));
        }
        let failed = matches!(o.state, OfferState::Failed(_));
        let busy = o.installing();
        Some(Self { x, y, inner, lines, buttons_row, buttons, labels, pad, failed, badge, busy, peeks })
    }

    fn draw(&self, editor: &Editor, ui: &Ui, out: &mut impl Write) -> io::Result<()> {
        let card = card_style(ui, editor, "ui.popup");
        let body = Style { fg: card.fg, bg: card.bg, ..Style::default() };
        let dim = Style { fg: blend(card.fg, card.bg, 0.55).or(card.fg), bg: card.bg, ..Style::default() };
        let bar = Style {
            fg: if self.failed { ui.error.fg } else { ui.accent.fg },
            bg: card.bg,
            ..Style::default()
        };
        let strong = Style { bold: true, ..body };
        let key = Style { fg: ui.accent.fg, bold: true, ..body };
        let width = self.inner + 5;
        // Back cards first (deeper = narrower, dimmer) — one title row each, the topmost up to the top edge
        let peek_bg = |k: usize| blend(card.bg, ui.tint, 1.0 - 0.3 * k as f32).or(card.bg);
        for (k, title, started, failed) in self.peeks.iter().rev() {
            let (px, pw, py) = (self.x + PEEK_INSET * k, width - 2 * PEEK_INSET * k, self.y - k);
            let bg = peek_bg(*k);
            let text = Style { fg: blend(card.fg, bg, 0.5).or(card.fg), bg, ..Style::default() };
            let mark = Style {
                fg: blend(if *failed { ui.error.fg } else { ui.accent.fg }, bg, 0.6),
                bg,
                ..Style::default()
            };
            if self.pad > 0 && *k == self.peeks.len() {
                apply(out, Style { fg: bg, bg: ui.base.bg, ..Style::default() })?;
                queue!(out, MoveTo(px as u16, (py - 1) as u16), Print("▄".repeat(pw)))?;
            }
            let tail = started.map(|t| format!(" {}", wave(t))).unwrap_or_default();
            let room = pw.saturating_sub(5 + tail.width());
            let t = fit_ellipsis(title, room);
            queue!(out, MoveTo(px as u16, py as u16))?;
            apply(out, text)?;
            queue!(out, Print(" "))?;
            apply(out, mark)?;
            queue!(out, Print("▎"))?;
            apply(out, text)?;
            queue!(
                out,
                Print(" "),
                Print(&t),
                Print(" ".repeat(pw.saturating_sub(3 + t.width() + tail.width())))
            )?;
            apply(out, mark)?;
            queue!(out, Print(&tail))?;
        }
        // Front card's top edge: over the back card's color where there is one (so they look stacked)
        if self.pad > 0 {
            let behind =
                self.peeks.first().map(|(k, ..)| (self.x + PEEK_INSET * k, width - 2 * PEEK_INSET * k));
            queue!(out, MoveTo(self.x as u16, self.y as u16))?;
            for col in 0..width {
                let under = behind.is_some_and(|(bx, bw)| (bx..bx + bw).contains(&(self.x + col)));
                let bg = if under { peek_bg(1) } else { ui.base.bg };
                apply(out, Style { fg: card.bg, bg, ..Style::default() })?;
                queue!(out, Print("▄"))?;
            }
        }
        let mut row = self.y + self.pad;
        for (i, (text, faint)) in self.lines.iter().enumerate() {
            queue!(out, MoveTo(self.x as u16, row as u16))?;
            apply(out, body)?;
            queue!(out, Print(" "))?;
            apply(out, bar)?;
            queue!(out, Print(if i == 0 { "▎" } else { " " }))?;
            apply(out, body)?;
            queue!(out, Print(" "))?;
            let badge = if i == 0 { self.badge.as_str() } else { "" };
            apply(
                out,
                if *faint {
                    dim
                } else if i == 0 {
                    strong
                } else {
                    body
                },
            )?;
            match text.split_once(' ').filter(|_| self.busy && i == 1) {
                // Downloading: wave in accent, text dimmed
                Some((w, rest)) => {
                    apply(out, Style { fg: ui.accent.fg, ..body })?;
                    queue!(out, Print(w))?;
                    apply(out, dim)?;
                    queue!(out, Print(" "), Print(rest))?;
                }
                None => queue!(out, Print(text))?,
            }
            apply(out, body)?;
            queue!(out, Print(" ".repeat(self.inner - text.width() - badge.width())))?;
            apply(out, dim)?;
            queue!(out, Print(badge))?;
            apply(out, body)?;
            queue!(out, Print("  "))?;
            row += 1;
        }
        if self.buttons_row.is_some() {
            apply(out, body)?;
            queue!(out, MoveTo(self.x as u16, row as u16), Print(" ".repeat(width)))?;
            row += 1;
            queue!(out, MoveTo(self.x as u16, row as u16))?;
            apply(out, body)?;
            queue!(out, Print("   "))?;
            apply(out, key)?;
            queue!(out, Print("y"))?;
            apply(out, body)?;
            queue!(out, Print(" "), Print(self.labels[0]), Print("   "))?;
            apply(out, key)?;
            queue!(out, Print("n"))?;
            apply(out, dim)?;
            queue!(out, Print(" "), Print(self.labels[1]))?;
            let used = 3 + 2 + self.labels[0].width() + 3 + 2 + self.labels[1].width();
            apply(out, body)?;
            queue!(out, Print(" ".repeat(width.saturating_sub(used))))?;
            row += 1;
        }
        card_edge(out, ui, card, self.x, row, width, self.pad, false)
    }
}

/// which-key card: after a prefix key (`space`·`g`·`m`), shows the next possible keys and meanings at the
/// bottom right of the editing area — usable without memorizing keys. Many → multiple columns. Submodes: `…`.
fn draw_which_key(editor: &Editor, ui: &Ui, lay: &Layout, out: &mut impl Write) -> io::Result<()> {
    let entries = editor.keymaps.children(editor.mode, &editor.pending);
    if entries.is_empty() {
        return Ok(());
    }
    let card = card_style(ui, editor, "ui.popup");
    let pad = card_padding(ui, card);
    let key_style = Style { fg: ui.accent.fg, bold: true, ..card };
    let dim = Style { fg: ui.virt.fg, ..card };
    let title = match editor.pending.first().map(|k| k.to_string()).as_deref() {
        Some("space") => "space",
        Some("g") => "goto",
        Some("m") => "match",
        Some("[") => "previous",
        Some("]") => "next",
        Some("z") | Some("Z") => "view",
        _ => "keys",
    };
    let pending: Vec<String> = editor.pending.iter().map(|k| k.to_string()).collect();
    let kw = entries.iter().map(|(k, _, _)| k.to_string().width()).max().unwrap_or(1);
    const DESC: usize = 30;
    let dw = entries.iter().map(|(_, d, _)| d.width()).max().unwrap_or(0).min(DESC);
    let cell = kw + 2 + dw;
    let max_rows = lay.text_rows.saturating_sub(2 * pad + 3).max(1);
    let cols = entries.len().div_ceil(max_rows).min((lay.edit_w.saturating_sub(4) / (cell + 3)).max(1));
    let rows = entries.len().div_ceil(cols);
    let inner = cols * cell + (cols - 1) * 3;
    let width = inner + 4;
    let h = rows + 2 + 2 * pad;
    if h > lay.text_rows || width >= lay.edit_w {
        return Ok(());
    }
    let x0 = lay.edit_w - width - 1;
    let y0 = lay.text_top as usize + lay.text_rows - h;
    card_edge(out, ui, card, x0, y0, width, pad, true)?;
    // Title: pressed keys · mode name
    let y = y0 + pad;
    queue!(out, MoveTo(x0 as u16, y as u16))?;
    apply(out, card)?;
    queue!(out, Print("  "))?;
    apply(out, Style { bold: true, ..card })?;
    let head = pending.join(" ");
    queue!(out, Print(&head))?;
    apply(out, dim)?;
    let rest = if head == title { String::new() } else { format!("  {title}") };
    queue!(out, Print(&rest), Print(" ".repeat((inner + 2).saturating_sub(head.width() + rest.width()))))?;
    queue!(out, MoveTo(x0 as u16, (y + 1) as u16))?;
    apply(out, card)?;
    queue!(out, Print(" ".repeat(width)))?;
    for r in 0..rows {
        queue!(out, MoveTo(x0 as u16, (y + 2 + r) as u16))?;
        apply(out, card)?;
        queue!(out, Print("  "))?;
        for c in 0..cols {
            let i = c * rows + r;
            let used = match entries.get(i) {
                Some((k, d, group)) => {
                    let ks = k.to_string();
                    apply(out, key_style)?;
                    queue!(out, Print(format!("{ks:<kw$}  ")))?;
                    let d = fit_ellipsis(d, dw);
                    apply(out, if *group { dim } else { card })?;
                    queue!(out, Print(&d))?;
                    kw + 2 + d.width()
                }
                None => 0,
            };
            apply(out, card)?;
            let gap = if c + 1 < cols { 3 } else { 0 };
            queue!(out, Print(" ".repeat(cell - used + gap)))?;
        }
        queue!(out, Print("  "))?;
    }
    card_edge(out, ui, card, x0, y + 2 + rows, width, pad, false)
}

/// Waiting animation: a three-cell equalizer wave (`▂▅▇` …) — only block chars in common monospace fonts.
fn wave(since: std::time::Instant) -> String {
    const W: [char; 10] = ['▂', '▃', '▄', '▅', '▆', '▇', '▆', '▅', '▄', '▃'];
    let f = (since.elapsed().as_millis() / 70) as usize;
    (0..3).map(|k| W[(f + k * 3) % W.len()]).collect()
}

/// While an ask answer streams: last few lines being written, in a card at the editing area's bottom right
/// (in that language's colors). Editing isn't blocked — the card covers only the corner.
fn draw_ask_stream(
    rev: &crate::llm::Review,
    editor: &Editor,
    ui: &Ui,
    lay: &Layout,
    out: &mut impl Write,
) -> io::Result<()> {
    const ROWS: usize = 6;
    let card = card_style(ui, editor, "ui.popup");
    let pad = card_padding(ui, card);
    let lang = editor
        .docs
        .iter()
        .find(|d| d.id == rev.doc_id)
        .and_then(|d| d.syntax.as_ref())
        .map(|s| s.lang.name.clone());
    let preview = crate::llm::preview(&rev.streamed, ROWS);
    let code = crate::markdown::code_lines(&preview.join("\n"), lang.as_deref(), &editor.theme);
    let inner = (lay.edit_w / 2).clamp(30, 72).min(lay.edit_w.saturating_sub(4));
    let body = if rev.streamed.is_empty() { 0 } else { code.len().min(ROWS) };
    let h = 1 + body + 2 * pad;
    if lay.text_rows < h + 1 {
        return Ok(());
    }
    let (x0, y0) = (lay.edit_w.saturating_sub(inner + 5), lay.text_top as usize + lay.text_rows - h);
    let width = inner + 4;
    card_edge(out, ui, card, x0, y0, width, pad, true)?;
    // Title: wave · status · instruction ……… elapsed
    let y = y0 + pad;
    let secs = format!("{:.1}s", rev.started.elapsed().as_secs_f32());
    let phase = if rev.thinking || rev.streamed.is_empty() { "thinking" } else { "writing" };
    queue!(out, MoveTo(x0 as u16, y as u16))?;
    apply(out, card)?;
    queue!(out, Print("  "))?;
    apply(out, Style { fg: ui.accent.fg, ..card })?;
    queue!(out, Print(wave(rev.started)))?;
    apply(out, Style { bold: true, ..card })?;
    queue!(out, Print(format!(" claude {phase}")))?;
    let instr = fit_ellipsis(
        &format!("  “{}”", rev.instruction),
        // One row = margin 2 + wave 3 + " claude status" + instruction + gap + elapsed + margin 2 = inner + 4
        (inner + 4).saturating_sub(2 + 3 + 8 + phase.len() + 1 + secs.len() + 2),
    );
    apply(out, Style { fg: ui.virt.fg, ..card })?;
    queue!(out, Print(&instr))?;
    let used = 2 + 3 + 8 + phase.len() + instr.width();
    queue!(out, Print(" ".repeat((inner + 4).saturating_sub(used + secs.len() + 2))))?;
    apply(out, Style { fg: ui.linenr.fg, ..card })?;
    queue!(out, Print(&secs), Print("  "))?;
    for (i, line) in code.iter().take(body).enumerate() {
        queue!(out, MoveTo(x0 as u16, (y + 1 + i) as u16))?;
        apply(out, card)?;
        queue!(out, Print("  "))?;
        let mut used = 0;
        if let crate::markdown::Line::Code(spans) = line {
            for sp in spans {
                let t = fit(&sp.text, inner - used);
                apply(out, card.patch(sp.style))?;
                queue!(out, Print(&t))?;
                used += t.width();
            }
        }
        // End of last line = where it's writing
        if i + 1 == body && used < inner {
            apply(out, Style { fg: ui.accent.fg, ..card })?;
            queue!(out, Print("▍"))?;
            used += 1;
        }
        apply(out, card)?;
        queue!(out, Print(" ".repeat(inner - used + 2)))?;
    }
    card_edge(out, ui, card, x0, y + 1 + body, width, pad, false)
}

/// Chat panel — right of the editing area. Half-block left edge (`▐`) so it floats a bit; accent if focused.
///
/// ```text
/// ▐ claude                ▂▅▇ writing 1.2s
/// ▐ ────────────────────────────────────
/// ▐ main.rs · L12–18                       ← context chip sent
/// ▐ ▎ explain this function                ← my message (accent bar)
/// ▐ It does … (markdown, code in color)▍   ← streaming answer
/// ▐                                  2.1s
/// ▐ ────────────────────────────────────
/// ▐ › input…
/// ▐ enter send · esc editor · C-r apply
/// ```
/// Returns the input cursor position (when focused).
fn draw_chat(
    c: &crate::chat::Chat,
    editor: &Editor,
    ui: &Ui,
    lay: &Layout,
    out: &mut impl Write,
) -> io::Result<Option<(u16, u16)>> {
    use crate::chat::{Role, State};
    use crate::markdown::{self as md, Span};
    let t = &editor.theme;
    let card = card_style(ui, editor, "ui.popup");
    let dim = Style { fg: ui.virt.fg, ..card };
    let faint = Style { fg: ui.linenr.fg, ..card };
    let accent = Style { fg: ui.accent.fg, ..card };
    let strong = card.patch(t.try_get("ui.text.focus").unwrap_or(Style { bold: true, ..Style::default() }));
    let x0 = lay.edit_w;
    let w = lay.chat_w.saturating_sub(4).max(8); // text width: edge 1 + margin 2 + right margin 1
    let top = lay.text_top as usize;
    let rows = lay.text_rows + lay.debug_h;
    let lang = editor.doc().syntax.as_ref().map(|s| s.lang.name.clone());
    // ── Input (wrapped, up to 5 rows) + cursor position
    let placeholder = format!("Ask about {}…", editor.doc().display_name_short());
    let mut input_rows: Vec<String> = vec![String::new()];
    let (mut cur_row, mut cur_col) = (0, 0);
    let mut col = 0;
    for (b, ch) in c.input.char_indices() {
        if b == c.cursor {
            (cur_row, cur_col) = (input_rows.len() - 1, col);
        }
        let cw = ch.width().unwrap_or(0);
        if ch == '\n' || col + cw > w - 2 {
            input_rows.push(String::new());
            col = 0;
            if ch == '\n' {
                continue;
            }
        }
        input_rows.last_mut().unwrap().push(ch);
        col += cw;
    }
    if c.cursor >= c.input.len() {
        (cur_row, cur_col) = (input_rows.len() - 1, col);
    }
    let first_input = input_rows.len().saturating_sub(5).min(cur_row);
    let input_h = input_rows.len().min(5);
    // ── Message rows (filled from the bottom)
    let area = rows.saturating_sub(input_h + 4); // title·divider + divider·hint
    let mut lines: Vec<Vec<Span>> = Vec::new();
    let sp = |text: String, style: Style| Span { text, style };
    let n = c.msgs.len();
    for (i, m) in c.msgs.iter().enumerate() {
        match m.role {
            Role::User => {
                if !lines.is_empty() {
                    lines.push(Vec::new());
                }
                if !m.chip.is_empty() {
                    lines.push(vec![sp(fit_ellipsis(&m.chip, w), faint)]);
                }
                let para = [md::Line::Text { spans: vec![sp(m.text.clone(), strong)], indent: 0 }];
                for l in md::wrap(&para, w - 2, faint) {
                    let mut row = vec![sp("▎ ".into(), accent)];
                    row.extend(l);
                    lines.push(row);
                }
            }
            Role::Assistant => {
                lines.push(Vec::new());
                let mut cache = m.cache.borrow_mut();
                if !matches!(&*cache, Some((len, cw, _)) if *len == m.text.len() && *cw == w) {
                    let rendered = md::render(&m.text, t, lang.as_deref());
                    *cache = Some((m.text.len(), w, chat_markdown(&rendered, w, faint, ui.base.bg)));
                }
                let body = &cache.as_ref().unwrap().2;
                lines.extend(body.iter().cloned());
                if i + 1 == n && c.state == State::Writing {
                    match lines.last_mut() {
                        Some(l) => l.push(sp("▍".into(), accent)),
                        None => lines.push(vec![sp("▍".into(), accent)]),
                    }
                }
            }
            // Elapsed time dimmed at the right end; stopped/error on the left
            Role::Note => {
                let timing = m.text.ends_with('s') && m.text[..m.text.len() - 1].parse::<f32>().is_ok();
                if timing {
                    lines.push(vec![sp(format!("{:>w$}", m.text), faint)]);
                } else {
                    lines.push(vec![sp(fit_ellipsis(&format!("· {}", m.text), w), dim)]);
                }
            }
        }
    }
    if c.state == State::Thinking {
        lines.push(Vec::new());
        lines.push(vec![sp(wave(c.started), accent), sp(" thinking…".into(), dim)]);
    }
    // Empty chat: guidance + example questions in the middle
    if c.msgs.is_empty() && !c.busy() {
        let name = editor.doc().display_name_short();
        let mut intro = vec![
            vec![sp("▎ ".into(), accent), sp("Ask Claude".into(), strong)],
            vec![],
            vec![sp(fit(&format!("It sees {name} — your cursor,"), w), dim)],
            vec![sp(fit("selection and diagnostics.", w), dim)],
            vec![],
            vec![sp("tab ".into(), Style { bold: true, ..dim }), sp("for a suggestion".into(), faint)],
        ];
        for s in crate::chat::SUGGESTIONS {
            intro.push(vec![sp("› ".into(), faint), sp(fit(s, w - 2), dim)]);
        }
        let pad = area.saturating_sub(intro.len()) / 2;
        lines = std::iter::repeat_n(Vec::new(), pad).chain(intro).collect();
    }
    let max_scroll = lines.len().saturating_sub(area);
    let scroll = c.scroll.min(max_scroll);
    let from = lines.len().saturating_sub(area + scroll);
    let shown = &lines[from..lines.len() - scroll];

    // ── Draw row by row: [edge][margin 2][text w][margin 1]
    let edge = Style { fg: card.bg, bg: ui.base.bg, ..Style::default() };
    let mut y = top;
    // Title
    chat_row_start(out, x0, y, edge, card)?;
    apply(out, if c.focused { strong } else { dim })?;
    queue!(out, Print("claude"))?;
    let status: Vec<Span> = match c.state {
        State::Idle => vec![sp("ready".into(), faint)],
        s => vec![
            sp(wave(c.started), accent),
            sp(
                format!(
                    " {} {:.1}s",
                    if s == State::Thinking { "thinking" } else { "writing" },
                    c.started.elapsed().as_secs_f32()
                ),
                dim,
            ),
        ],
    };
    let sw: usize = status.iter().map(|s| s.text.width()).sum();
    apply(out, card)?;
    queue!(out, Print(" ".repeat(w.saturating_sub(6 + sw))))?;
    let used = 6 + w.saturating_sub(6 + sw) + chat_spans(out, &status, card, w)?;
    chat_row_end(out, card, lay.chat_w, used)?;
    y += 1;
    chat_row_start(out, x0, y, edge, card)?;
    apply(out, faint)?;
    queue!(out, Print("─".repeat(w)))?;
    chat_row_end(out, card, lay.chat_w, w)?;
    y += 1;
    for i in 0..area {
        chat_row_start(out, x0, y, edge, card)?;
        let used = match shown.get(i) {
            Some(l) => chat_spans(out, l, card, w)?,
            None => 0,
        };
        chat_row_end(out, card, lay.chat_w, used)?;
        y += 1;
    }
    // Input
    chat_row_start(out, x0, y, edge, card)?;
    apply(out, faint)?;
    queue!(out, Print("─".repeat(w)))?;
    chat_row_end(out, card, lay.chat_w, w)?;
    y += 1;
    let input_top = y;
    for (i, r) in input_rows.iter().skip(first_input).take(input_h).enumerate() {
        chat_row_start(out, x0, y, edge, card)?;
        let prompt = if i == 0 { "› " } else { "  " };
        apply(out, if c.focused { Style { bold: true, ..accent } } else { faint })?;
        queue!(out, Print(prompt))?;
        let used = if c.input.is_empty() {
            apply(out, faint)?;
            let p = fit(&placeholder, w - 2);
            queue!(out, Print(&p))?;
            p.width()
        } else {
            apply(out, card)?;
            queue!(out, Print(r))?;
            r.width()
        };
        chat_row_end(out, card, lay.chat_w, 2 + used)?;
        y += 1;
    }
    // Hint
    chat_row_start(out, x0, y, edge, card)?;
    let key = Style { bold: true, ..dim };
    let mut hints: Vec<(&str, &str)> = Vec::new();
    if !c.focused {
        hints.push(("space l", "focus"));
        hints.push(("space L", "close"));
    } else if c.busy() {
        hints.push(("C-c", "stop"));
        hints.push(("esc", "editor"));
    } else {
        hints.push(("enter", "send"));
        hints.push(("esc", "editor"));
        if c.last_code().is_some() {
            hints.push(("C-r", "apply"));
            hints.push(("C-y", "copy"));
        }
        hints.push(("C-l", "new"));
    }
    let mut used = 0;
    for (k, d) in hints {
        let wd = k.width() + d.width() + 3;
        if used + wd > w {
            break;
        }
        apply(out, key)?;
        queue!(out, Print(k))?;
        apply(out, faint)?;
        queue!(out, Print(format!(" {d}  ")))?;
        used += wd;
    }
    chat_row_end(out, card, lay.chat_w, used)?;
    y += 1;
    // Remaining rows (when the panel is taller than the input)
    while y < top + rows {
        chat_row_start(out, x0, y, edge, card)?;
        chat_row_end(out, card, lay.chat_w, 0)?;
        y += 1;
    }
    Ok(c.focused.then(|| {
        let r = cur_row.saturating_sub(first_input).min(input_h - 1);
        ((x0 + 3 + 2 + cur_col) as u16, (input_top + r) as u16)
    }))
}

/// Answer markdown → rows. Code blocks as a "well" of editor bg (sunk a tone inside the card) — text and
/// code separate at a glance. Code is truncated, not wrapped (`…` at the end if cut).
fn chat_markdown(
    lines: &[crate::markdown::Line],
    w: usize,
    rule: Style,
    well: Option<crossterm::style::Color>,
) -> Vec<Vec<crate::markdown::Span>> {
    use crate::markdown::{self as md, Line, Span};
    let mut out = Vec::new();
    for l in lines {
        match l {
            Line::Code(spans) => {
                let full: usize = spans.iter().map(|s| s.text.width()).sum();
                let inner = w.saturating_sub(2);
                let mut row = vec![Span { text: " ".into(), style: Style { bg: well, ..Style::default() } }];
                let mut used = 0;
                for s in spans {
                    let room = inner.saturating_sub(used + usize::from(full > inner));
                    let t = fit(&s.text, room);
                    used += t.width();
                    row.push(Span { text: t, style: Style { bg: well, ..s.style } });
                }
                if full > inner {
                    row.push(Span {
                        text: "…".into(),
                        style: Style { bg: well, fg: rule.fg, ..Style::default() },
                    });
                    used += 1;
                }
                row.push(Span {
                    text: " ".repeat(inner.saturating_sub(used) + 1),
                    style: Style { bg: well, ..Style::default() },
                });
                out.push(row);
            }
            other => out.extend(md::wrap(std::slice::from_ref(other), w, rule)),
        }
    }
    out
}

fn chat_row_start(out: &mut impl Write, x0: usize, y: usize, edge: Style, card: Style) -> io::Result<()> {
    queue!(out, MoveTo(x0 as u16, y as u16))?;
    apply(out, edge)?;
    queue!(out, Print("▐"))?;
    apply(out, card)?;
    queue!(out, Print("  "))
}

fn chat_row_end(out: &mut impl Write, card: Style, chat_w: usize, used: usize) -> io::Result<()> {
    apply(out, card)?;
    queue!(out, Print(" ".repeat(chat_w.saturating_sub(3 + used))))
}

fn chat_spans(
    out: &mut impl Write,
    spans: &[crate::markdown::Span],
    base: Style,
    w: usize,
) -> io::Result<usize> {
    let mut used = 0;
    for s in spans {
        let text = fit(&s.text, w.saturating_sub(used));
        apply(out, base.patch(s.style))?;
        queue!(out, Print(&text))?;
        used += text.width();
    }
    Ok(used)
}

/// Background of floating cards (ui.menu / ui.popup, else the built-in theme's).
fn card_style(ui: &Ui, editor: &Editor, key: &str) -> Style {
    let t = &editor.theme;
    ui.base.patch(
        t.try_get(key)
            .or_else(|| t.try_get("ui.popup"))
            .or_else(|| builtin().try_get(key))
            .unwrap_or_default(),
    )
}

/// For cards (bg differs from the editor), one half-row margin row above and below, else 0.
fn card_padding(ui: &Ui, card: Style) -> usize {
    usize::from(card.bg.is_some() && card.bg != ui.base.bg)
}

/// Card top (`▄`) and bottom (`▀`) edges — half blocks paint only half a row so the margin looks thin.
#[allow(clippy::too_many_arguments)]
fn card_edge(
    out: &mut impl Write,
    ui: &Ui,
    card: Style,
    x: usize,
    y: usize,
    width: usize,
    pad: usize,
    top: bool,
) -> io::Result<()> {
    if pad == 0 {
        return Ok(());
    }
    apply(out, Style { fg: card.bg, bg: ui.base.bg, ..Style::default() })?;
    queue!(out, MoveTo(x as u16, y as u16), Print((if top { "▄" } else { "▀" }).repeat(width)))
}

/// A label with fuzzy-matched chars (`hits` = ascending char indices) in `matched`, up to `room` cells.
/// Returns the cells used.
fn print_matched(
    out: &mut impl Write,
    label: &str,
    hits: &[u32],
    name: Style,
    matched: Style,
    room: usize,
) -> io::Result<usize> {
    let mut hit = hits.iter().peekable();
    let mut current = None;
    let mut used = 0;
    for (ci, ch) in label.chars().enumerate() {
        let cw = ch.width().unwrap_or(0);
        if used + cw > room {
            break;
        }
        while hit.next_if(|&&h| (h as usize) < ci).is_some() {}
        let want = if hit.peek().is_some_and(|&&h| h as usize == ci) { name.patch(matched) } else { name };
        if current != Some(want) {
            apply(out, want)?;
            current = Some(want);
        }
        queue!(out, Print(ch))?;
        used += cw;
    }
    Ok(used)
}

/// List scrollbar thumb: rows `first..` of `n` shown in `rows` — thumb sized by the visible share.
fn scroll_thumb(rows: usize, n: usize, first: usize) -> std::ops::Range<usize> {
    let len = (rows * rows).div_ceil(n).clamp(1, rows);
    let at = if n > rows { first * (rows - len) / (n - rows) } else { 0 };
    at..at + len
}

/// Truncate to width, ending with '…' if cut.
fn fit_ellipsis(s: &str, width: usize) -> String {
    if s.width() <= width {
        return s.to_string();
    }
    let mut out = fit(s, width.saturating_sub(1));
    out.push('…');
    out
}

/// Floating docs (hover) — below the cursor (above if no room), text's first cell at the cursor column.
fn draw_popup(
    lines: &[crate::markdown::Line],
    at: (u16, u16),
    ui: &Ui,
    editor: &Editor,
    lay: &Layout,
    out: &mut impl Write,
) -> io::Result<()> {
    draw_float(lines, at, editor.popup_scroll, 20, ui, editor, lay, out)
}

/// Lines in a doc box by the cursor (hover, diagnostic card): at most `cap` rows, scrolled `scroll` rows.
#[allow(clippy::too_many_arguments)]
fn draw_float(
    lines: &[crate::markdown::Line],
    (cx, cy): (u16, u16),
    scroll: usize,
    cap: usize,
    ui: &Ui,
    editor: &Editor,
    lay: &Layout,
    out: &mut impl Write,
) -> io::Result<()> {
    let natural = lines.iter().map(crate::markdown::Line::width).max().unwrap_or(0);
    let inner = natural.clamp(10, 76).min(lay.edit_w.saturating_sub(4));
    let rows = crate::markdown::wrap(lines, inner, dim_style(ui, editor));
    // Never cover the cursor line: below if it fits, else above; if neither fits, the larger side, truncated
    let (top, bottom) = (lay.text_top as usize, lay.text_top as usize + lay.text_rows);
    let (below, above) = (bottom.saturating_sub(cy as usize + 1), (cy as usize).saturating_sub(top));
    let want = rows.len().min(cap) + 2;
    let down = want <= below || (want > above && below >= above);
    let room = if down { below } else { above };
    if room < 3 {
        return Ok(());
    }
    let max_rows = (room - 2).min(cap);
    let h = rows.len().min(max_rows) + 2;
    let y0 = if down { cy as usize + 1 } else { cy as usize - h };
    let x0 = (cx as usize).saturating_sub(2).min(lay.edit_w.saturating_sub(inner + 4));
    // Skip as far as scrolled, but stop at the last page (so more C-d doesn't give an empty box)
    let skip = scroll.min(rows.len().saturating_sub(max_rows));
    draw_doc_box(&rows[skip..], (x0, y0), inner, max_rows, ui, editor, out)
}

fn dim_style(ui: &Ui, editor: &Editor) -> Style {
    Style { fg: ui.virt.fg, ..card_style(ui, editor, "ui.popup") }
}

/// Doc box: card-like padding, no border, if the theme has a bg color; if bg = the editor's, rounded border.
/// Width = inner + 4 (padding 1 per side + border/padding 1), height = visible rows + 2. "+N" below if cut.
fn draw_doc_box(
    rows: &[Vec<crate::markdown::Span>],
    (x0, y0): (usize, usize),
    inner: usize,
    max_rows: usize,
    ui: &Ui,
    editor: &Editor,
    out: &mut impl Write,
) -> io::Result<()> {
    let popup = card_style(ui, editor, "ui.popup");
    let dim = Style { fg: ui.virt.fg, ..popup };
    let card = card_padding(ui, popup) == 1;
    let shown = rows.len().min(max_rows);
    let width = inner + 4;
    let more = if rows.len() > shown { format!(" +{} ", rows.len() - shown) } else { String::new() };
    // Top edge: half-row block for cards, else a rounded border
    if card {
        card_edge(out, ui, popup, x0, y0, width, 1, true)?;
    } else {
        apply(out, dim)?;
        queue!(out, MoveTo(x0 as u16, y0 as u16), Print(format!("╭{}╮", "─".repeat(inner + 2))))?;
    }
    let side = if card { " " } else { "│" };
    for (i, spans) in rows.iter().take(shown).enumerate() {
        apply(out, dim)?;
        queue!(out, MoveTo(x0 as u16, (y0 + 1 + i) as u16), Print(side), Print(" "))?;
        let mut used = 0;
        for sp in spans {
            let text = fit(&sp.text, inner - used);
            apply(out, popup.patch(sp.style))?;
            queue!(out, Print(&text))?;
            used += text.width();
            if used >= inner {
                break;
            }
        }
        apply(out, popup)?;
        // Overflowed row count dimmed at the right end of the last row (if there's room)
        let last = i + 1 == shown && !more.is_empty() && inner >= used + more.width();
        if last && card {
            queue!(out, Print(" ".repeat(inner - used - more.width() + 1)))?;
            apply(out, Style { fg: ui.linenr.fg, ..popup })?;
            queue!(out, Print(more.trim_end()), Print(" "))?;
        } else {
            queue!(out, Print(" ".repeat(inner - used + 1)))?;
        }
        apply(out, dim)?;
        queue!(out, Print(side))?;
    }
    let y = y0 + 1 + shown;
    if card {
        card_edge(out, ui, popup, x0, y, width, 1, false)
    } else {
        apply(out, dim)?;
        queue!(
            out,
            MoveTo(x0 as u16, y as u16),
            Print(format!("╰{}{more}╯", "─".repeat((inner + 2).saturating_sub(more.width()))))
        )
    }
}

/// Whether diagnostic `d` underlines the cell at `pos` (an empty range marks the one cell at its start).
fn underlines(d: &crate::lsp::Diagnostic, pos: usize) -> bool {
    d.from <= pos && (pos < d.to || (d.from == d.to && pos == d.from))
}

/// The diagnostics under the cursor in full — the line end shows only a first line, cut to fit. A card by
/// the cursor, only when it sits on an underline whose message the line end didn't show whole, and
/// nothing else floats (normal/select mode).
fn draw_diagnostic_card(
    editor: &Editor,
    at: (u16, u16),
    ui: &Ui,
    lay: &Layout,
    out: &mut impl Write,
) -> io::Result<()> {
    let busy = editor.mode == Mode::Insert
        || editor.popup.is_some()
        || editor.completion.is_some()
        || editor.signature.is_some()
        || editor.picker.is_some()
        || editor.prompt.is_some()
        || !editor.pending.is_empty()
        || editor.chat.as_ref().is_some_and(|c| c.focused);
    if !editor.config.cursor_diagnostics
        || busy
        || editor.diag_card_hidden == Some((editor.doc().id, editor.cursor_line()))
    {
        return Ok(());
    }
    let here = cursor_diagnostics(editor);
    // The line end shows the line's only diagnostic whole — and that's the one under the cursor
    let line = editor.cursor_line();
    let text = &editor.doc().text;
    if ui.cursor_diags_shown.get() && here.iter().all(|d| mv::line_of(text, d.from) == line) {
        return Ok(());
    }
    let lines = diagnostic_lines(&here, editor, ui);
    if lines.is_empty() {
        return Ok(());
    }
    draw_float(&lines, at, 0, 12, ui, editor, lay, out)
}

/// Diagnostics underlining the cell under the cursor — most severe first.
fn cursor_diagnostics(editor: &Editor) -> Vec<&crate::lsp::Diagnostic> {
    let doc = editor.doc();
    let head = doc.selection().primary().cursor(&doc.text);
    let mut list: Vec<_> = doc.lsp.diagnostics.iter().filter(|d| underlines(d, head)).collect();
    list.sort_by_key(|d| (d.severity, d.from));
    list
}

/// Card text: per diagnostic `● first line  source code`, then the rest of its lines; `code` in the
/// buffer's syntax colors. A blank row between diagnostics.
fn diagnostic_lines(
    diags: &[&crate::lsp::Diagnostic],
    editor: &Editor,
    ui: &Ui,
) -> Vec<crate::markdown::Line> {
    use crate::markdown::{Line, Span};
    let lang = editor.doc().syntax.as_ref().map(|s| s.lang.name.clone());
    let faint = Style { fg: ui.linenr.fg, ..Style::default() };
    let mut out = Vec::new();
    let mut last_source = None;
    for (i, d) in diags.iter().enumerate() {
        if i > 0 {
            out.push(Line::Blank);
        }
        let glyph = match d.severity {
            2 => "▲",
            1 | 3 => "●",
            _ => "·",
        };
        let mut lines =
            d.message.lines().map(str::trim_end).filter(|l| !l.trim().is_empty() && !lint_origin(l));
        let color = Style { fg: ui.sign[d.severity.min(4) as usize].fg, ..Style::default() };
        let mut spans = vec![Span { text: format!("{glyph} "), style: color }];
        // Errors/warnings lead in bold; info/hints (often "expected due to this") step back
        let head_style = if d.severity <= 2 {
            Style { bold: true, ..Style::default() }
        } else {
            Style { fg: ui.virt.fg, ..Style::default() }
        };
        let head = lines.next().unwrap_or_default();
        spans.extend(message_spans(head, head_style, lang.as_deref(), editor));
        // Source only when it changes (a hint right after its error repeats it)
        let source = diagnostic_source(&d.raw);
        if let Some(src) = source.as_ref().filter(|&s| last_source.as_ref() != Some(s)) {
            spans.push(Span { text: format!("  {src}"), style: faint });
        }
        last_source = source;
        out.push(Line::Text { spans, indent: 2 });
        for l in lines {
            let mut spans = vec![Span { text: "  ".into(), style: Style::default() }];
            spans.extend(message_spans(l, Style::default(), lang.as_deref(), editor));
            out.push(Line::Text { spans, indent: 2 });
        }
    }
    out
}

/// Message text → spans: `code` (backticks dropped) colored as code of `lang`, the rest in `base`.
fn message_spans(text: &str, base: Style, lang: Option<&str>, editor: &Editor) -> Vec<crate::markdown::Span> {
    use crate::markdown::{Line, Span};
    let t = &editor.theme;
    let raw = t.try_get("markup.raw.inline").or_else(|| t.try_get("markup.raw")).unwrap_or_default();
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(open) = rest.find('`') {
        let Some(len) = rest[open + 1..].find('`') else { break };
        if open > 0 {
            out.push(Span { text: rest[..open].to_string(), style: base });
        }
        let code = &rest[open + 1..open + 1 + len];
        // Inline-code color underneath, syntax colors on top (unparsed bits still read as code)
        match crate::markdown::code_lines(code, lang, t).into_iter().next() {
            Some(Line::Code(spans)) => {
                out.extend(spans.into_iter().map(|sp| Span { style: raw.patch(sp.style), ..sp }))
            }
            _ => out.push(Span { text: code.to_string(), style: raw }),
        }
        rest = &rest[open + 1 + len + 1..];
    }
    if !rest.is_empty() {
        out.push(Span { text: rest.to_string(), style: base });
    }
    out
}

/// rustc's "`#[warn(unused_variables)]` (part of …) on by default" — says where a lint comes from, not
/// what's wrong. Left out of the card.
fn lint_origin(line: &str) -> bool {
    let l = line.trim();
    l.starts_with("`#[") && l.ends_with("on by default")
}

/// `rustc E0308` — who reported it and its code, if the server says.
fn diagnostic_source(raw: &serde_json::Value) -> Option<String> {
    let code = match &raw["code"] {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Number(n) => Some(n.to_string()),
        _ => None,
    };
    let parts: Vec<String> = raw["source"].as_str().map(str::to_string).into_iter().chain(code).collect();
    (!parts.is_empty()).then(|| parts.join(" "))
}

fn cursor_diagnostic(editor: &Editor) -> Option<&crate::lsp::Diagnostic> {
    let doc = editor.doc();
    let head = doc.selection().primary().cursor(&doc.text);
    let line = mv::line_of(&doc.text, head);
    let list = &doc.lsp.diagnostics;
    list.iter()
        .filter(|d| d.from <= head && head <= d.to)
        .min_by_key(|d| d.severity)
        .or_else(|| list.iter().filter(|d| mv::line_of(&doc.text, d.from) == line).min_by_key(|d| d.severity))
}

/// Keys for whatever is floating now (shown dimmed when the command line is empty).
fn key_hints(editor: &Editor) -> Option<&'static [(&'static str, &'static str)]> {
    let replace_list = |p: &Picker| p.current().is_some_and(|i| matches!(i.action, Action::ReplaceFile(_)));
    if editor.completion.is_some() {
        Some(&[("tab", "select"), ("enter", "accept"), ("C-x", "complete"), ("esc", "close")])
    } else if editor.picker.as_ref().is_some_and(|p| p.grep.is_some()) {
        Some(&[("enter", "open"), ("C-r", "replace the listed matches"), ("esc", "close")])
    } else if editor.picker.as_ref().is_some_and(replace_list) {
        Some(&[("enter", "replace in every listed file"), ("type", "narrow"), ("esc", "cancel")])
    } else if editor.popup.is_some() {
        Some(&[("C-d", "scroll down"), ("C-u", "scroll up"), ("esc", "close")])
    } else {
        None
    }
}

/// Command line row. Returns the prompt's cursor position while typing.
/// `:` completion list — a card attached just above the statusline, first cell at the word being completed.
/// One row = [bar][name (typed chars highlighted, folders in accent)][   description·aliases dimmed][scroll].
fn draw_cmdline_menu(
    editor: &Editor,
    completion: Option<&crate::cmdline::Completion>,
    ui: &Ui,
    lay: &Layout,
    out: &mut impl Write,
) -> io::Result<()> {
    const ROWS: usize = 8;
    const LABEL_MAX: usize = 30;
    const HINT_MAX: usize = 60;
    let Some(p) = editor.prompt.as_ref() else { return Ok(()) };
    let Some(c) = completion else { return Ok(()) };
    let n = c.cands.len();
    let t = &editor.theme;
    let card = card_style(ui, editor, "ui.menu");
    let pad = card_padding(ui, card);
    let status_y = lay.cmd_y as usize - 1;
    let rows = n.min(ROWS).min(status_y.saturating_sub(lay.text_top as usize + 2 * pad));
    if rows == 0 {
        return Ok(());
    }
    let selected_i = p.cycle.as_ref().map(|(_, i)| *i);
    let first = selected_i.map_or(0, |s| s.saturating_sub(rows - 1));
    let visible = first..first + rows;
    let label_w = visible.clone().map(|i| c.cands[i].label.width()).max().unwrap_or(0).min(LABEL_MAX);
    let hint_w = visible.clone().map(|i| c.cands[i].hint.width()).max().unwrap_or(0).min(HINT_MAX);
    let scroll = n > rows;
    let width = (2 + label_w + if hint_w > 0 { 3 + hint_w } else { 0 } + 2).min(lay.width);
    // Column of the word being completed: prompt head (`:`) + text before the word
    let base = p.cycle.as_ref().map_or(p.text.as_str(), |(b, _)| b.as_str());
    let col = p.kind.label().width() + base[..c.start.min(base.len())].width();
    let x0 = col.saturating_sub(2).min(lay.width.saturating_sub(width));
    let h = rows + 2 * pad;
    let y0 = status_y - h;
    let selected = card.patch(t.try_get("ui.menu.selected").unwrap_or(ui.selection_primary));
    let matched = Style { fg: ui.accent.fg, bold: true, ..Style::default() };
    let dir_fg = t.try_get("ui.text.directory").and_then(|s| s.fg).or(ui.accent.fg);
    card_edge(out, ui, card, x0, y0, width, pad, true)?;
    let thumb = scroll_thumb(rows, n, first);
    for row in 0..rows {
        let i = first + row;
        let cand = &c.cands[i];
        let is_sel = selected_i == Some(i);
        let base_st = if is_sel { selected } else { card };
        queue!(out, MoveTo(x0 as u16, (y0 + pad + row) as u16))?;
        apply(out, if is_sel { Style { fg: ui.accent.fg, ..base_st } } else { base_st })?;
        queue!(out, Print(if is_sel { "▎ " } else { "  " }))?;
        let name =
            if is_sel { base_st.patch(t.try_get("ui.text.focus").unwrap_or_default()) } else { base_st };
        let name = Style { bg: base_st.bg, fg: if cand.dir { dir_fg } else { name.fg }, ..name };
        let label = fit_ellipsis(&cand.label, label_w);
        print_matched(out, &label, &cand.hits, name, matched, label_w)?;
        apply(out, base_st)?;
        queue!(out, Print(" ".repeat(label_w - label.width())))?;
        let mut used = 2 + label_w;
        if hint_w > 0 {
            let hnt = fit_ellipsis(&cand.hint, hint_w);
            apply(out, Style { fg: ui.virt.fg, ..base_st })?;
            queue!(out, Print("   "), Print(&hnt), Print(" ".repeat(hint_w - hnt.width())))?;
            used += 3 + hint_w;
        }
        apply(out, base_st)?;
        queue!(out, Print(" ".repeat(width.saturating_sub(used + 1))))?;
        if scroll && thumb.contains(&row) {
            apply(out, Style { fg: ui.linenr.fg, ..card })?;
            queue!(out, Print("▐"))?;
        } else {
            apply(out, card)?;
            queue!(out, Print(" "))?;
        }
    }
    card_edge(out, ui, card, x0, y0 + pad + rows, width, pad, false)
}

fn draw_cmdline(
    editor: &Editor,
    completion: Option<&crate::cmdline::Completion>,
    ui: &Ui,
    lay: &Layout,
    out: &mut impl Write,
) -> io::Result<Option<(u16, u16)>> {
    queue!(out, MoveTo(0, lay.cmd_y))?;
    clear_rest(out, ui.base)?;
    if let Some(prompt) = &editor.prompt {
        let shown = fit(&format!("{}{}", prompt.kind.label(), prompt.text), lay.width.saturating_sub(1));
        queue!(out, Print(&shown))?;
        // Rest of the first candidate dimmed after the cursor (accept with →)
        if let Some(c) = completion.filter(|_| prompt.cycle.is_none())
            && let Some(g) = crate::cmdline::ghost(&prompt.text, c)
        {
            apply(out, Style { fg: ui.linenr.fg, ..ui.base })?;
            queue!(out, Print(fit(g, lay.width.saturating_sub(shown.width() + 1))))?;
            apply(out, ui.base)?;
        }
        return Ok(Some((shown.width() as u16, lay.cmd_y)));
    }
    let pending = editor.pending_display();
    let room = lay.width.saturating_sub(pending.width() + 1);
    // Status messages go to top-right toasts — the command line has only input, key hints, cursor diagnostics
    if let Some(hints) = key_hints(editor) {
        // If something is floating, show its keys: keys crisp, descriptions dimmed
        let key = Style { fg: ui.virt.fg, bold: true, ..ui.base };
        let desc = Style { fg: ui.linenr.fg, ..ui.base };
        let mut used = 0;
        for (k, d) in hints {
            let w = k.width() + d.width() + 4;
            if used + w > room {
                break;
            }
            apply(out, key)?;
            queue!(out, Print(" "), Print(k))?;
            apply(out, desc)?;
            queue!(out, Print(" "), Print(d), Print("  "))?;
            used += w;
        }
        apply(out, ui.base)?;
    } else if let Some(d) = cursor_diagnostic(editor) {
        // If the cursor is on a diagnostic (else that line), show its message
        let msg = format!("● {}", d.message.lines().next().unwrap_or_default());
        apply(out, ui.base.patch(ui.sign[d.severity as usize]))?;
        queue!(out, Print(fit(&msg, room)))?;
        apply(out, ui.base)?;
    }
    if !pending.is_empty() {
        queue!(out, MoveTo(lay.width.saturating_sub(pending.width()) as u16, lay.cmd_y), Print(&pending))?;
    }
    Ok(None)
}

#[cfg(test)]
#[path = "term_snapshots.rs"]
mod snapshot_tests;

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use ropey::Rope;

    use super::*;
    use crate::config::Config;

    #[test]
    fn doc_comments_render_until_cursor_enters() {
        let mut ed = Editor::new(Config::default());
        ed.docs[0].text = Rope::from_str("/// Adds `two` numbers.\nfn add() {}\n");
        ed.docs[0].path = Some("/tmp/x.rs".into()); // doc comments only in languages that use that marker
        ed.docs[0].set_selection(crate::selection::Selection::point(24)); // fn line
        let mut buf = Vec::new();
        render(&mut ed, &mut buf, 80, 10).unwrap();
        let screen = String::from_utf8_lossy(&buf);
        assert!(!screen.contains("///") && screen.contains("▎ ") && screen.contains("numbers."), "{screen}");
        ed.docs[0].set_selection(crate::selection::Selection::point(5)); // inside the comment
        buf.clear();
        render(&mut ed, &mut buf, 80, 10).unwrap();
        assert!(!String::from_utf8_lossy(&buf).contains("///"), "passing cursor in normal mode stays folded");
        ed.mode = Mode::Insert;
        buf.clear();
        render(&mut ed, &mut buf, 80, 10).unwrap();
        assert!(String::from_utf8_lossy(&buf).contains("///"), "entering to edit (INSERT) shows raw");
        ed.mode = Mode::Normal;
        ed.docs[0].set_selection(crate::selection::Selection::single(crate::selection::Range::new(4, 9)));
        buf.clear();
        render(&mut ed, &mut buf, 80, 10).unwrap();
        assert!(String::from_utf8_lossy(&buf).contains("///"), "a selection wider than a char shows raw");
        // Markdown (`///` inside a code block) and unknown files stay as-is
        ed.docs[0].path = Some("/tmp/x.md".into());
        ed.docs[0].set_selection(crate::selection::Selection::point(24));
        buf.clear();
        render(&mut ed, &mut buf, 80, 10).unwrap();
        assert!(String::from_utf8_lossy(&buf).contains("///"));
    }

    /// Minimal terminal: applies `MoveTo`, `Clear(UntilNewLine)` and prints (other escapes ignored) —
    /// the screen rows as text (a wide char's second cell is empty).
    fn screen(bytes: &[u8], w: usize, h: usize) -> Vec<String> {
        let s = String::from_utf8_lossy(bytes);
        let mut grid = vec![vec![" ".to_string(); w]; h];
        let (mut x, mut y) = (0, 0);
        let mut it = s.chars().peekable();
        while let Some(c) = it.next() {
            if c == '\x1b' {
                if it.next_if_eq(&'[').is_none() {
                    it.next();
                    continue;
                }
                let mut params = String::new();
                while let Some(p) = it.next_if(|p| ('\x20'..='\x3f').contains(p)) {
                    params.push(p);
                }
                match it.next() {
                    Some('H') => {
                        let mut n = params.split(';').map(|n| n.parse::<usize>().unwrap_or(1));
                        (y, x) = (n.next().unwrap_or(1) - 1, n.next().unwrap_or(1) - 1);
                    }
                    Some('K') if y < h => grid[y][x.min(w)..].fill(" ".into()),
                    _ => {}
                }
                continue;
            }
            let cw = c.width().unwrap_or(0);
            if y < h && x + cw <= w && cw > 0 {
                grid[y][x] = c.to_string();
                grid[y][x + 1..x + cw].fill(String::new());
            }
            x += cw;
        }
        grid.into_iter().map(|r| r.concat()).collect()
    }

    #[test]
    fn unfocused_pane_survives_its_doc_shrinking() {
        let mut ed = Editor::new(Config::default());
        let text: String = (0..1000).map(|i| format!("line {i}\n")).collect();
        ed.docs[0].text = Rope::from_str(&text);
        ed.docs[0].path = Some("/tmp/x.rs".into()); // the doc-comment scan runs for languages that use it
        let at = mv::line_start(&ed.docs[0].text, 980);
        ed.docs[0].set_selection(crate::selection::Selection::point(at));
        let mut buf = Vec::new();
        render(&mut ed, &mut buf, 80, 24).unwrap();
        ed.split_view(crate::split::Dir::Vertical);
        for k in ["%", "d"] {
            ed.handle_key(k.parse().unwrap());
        }
        assert_eq!(ed.docs[0].text.len_bytes(), 0);
        buf.clear();
        render(&mut ed, &mut buf, 80, 24).unwrap(); // the other pane still scrolled to ~960
    }

    #[test]
    fn narrow_screens_never_panic() {
        let text: String = (0..100).map(|i| format!("fn line_{i}() {{ 타래 }}\n")).collect();
        let lines =
            crate::markdown::render("**hover** docs that are long enough to wrap a bit", builtin(), None);
        for chat in [false, true] {
            let mut ed = Editor::new(Config::default());
            ed.config.llm.command = "/nonexistent/tarae-test-claude".into();
            ed.docs[0].text = Rope::from_str(&text);
            let at = mv::line_start(&ed.docs[0].text, 50);
            ed.docs[0].set_selection(crate::selection::Selection::point(at));
            if chat {
                crate::chat::open(&mut ed);
                ed.chat.as_mut().unwrap().focused = false;
            }
            ed.push_offer(crate::offer::Offer {
                id: 0,
                title: "Rust syntax colors".into(),
                question: "Download and build the tree-sitter grammar?".into(),
                working: "Fetching and building…".into(),
                failed: "Couldn't build".into(),
                what: crate::offer::What::Grammars { lang: "Rust".into(), names: vec![], then: None },
                state: crate::offer::OfferState::Asking,
            });
            let result =
                serde_json::json!([{"label": "line_one", "detail": "fn line_one()"}, {"label": "lx"}]);
            let doc_id = ed.docs[0].id;
            let completion = || {
                let mut c = crate::completion::Completion::new(0, doc_id, at, &result);
                c.filter("l");
                c.selected = Some(0);
                c.docs = Some((0, lines.clone()));
                c
            };
            let mut buf = Vec::new();
            for h in [3, 10, 24] {
                for w in 1..=90 {
                    let ctx = format!("chat {chat}, {w}x{h}");
                    ed.popup = Some(lines.clone());
                    buf.clear();
                    render(&mut ed, &mut buf, w, h).unwrap_or_else(|e| panic!("{ctx}: {e}"));
                    ed.popup = None;
                    ed.completion = Some(completion());
                    render(&mut ed, &mut buf, w, h).unwrap();
                    ed.completion = None;
                    ed.pending = vec!["space".parse().unwrap()];
                    render(&mut ed, &mut buf, w, h).unwrap();
                    ed.pending.clear();
                    let mut p = crate::picker::Picker::new("themes", Vec::new(), false);
                    p.compact = true;
                    ed.picker = Some(p);
                    render(&mut ed, &mut buf, w, h).unwrap();
                    ed.picker = None;
                }
            }
        }
    }

    /// Horizontal scroll: a wide char cut by the left edge leaves a blank (what follows keeps its column),
    /// and a wide char under the cursor at the right edge scrolls fully into view (cursor stays visible).
    #[test]
    fn wide_chars_at_scroll_edges() {
        let mut ed = Editor::new(Config::default());
        ed.config.soft_wrap = "never".into(); // sideways scrolling
        ed.docs[0].text = Rope::from_str(&"漢".repeat(30));
        ed.docs[0].set_selection(crate::selection::Selection::point(29 * 3));
        let (w, h) = (21, 6); // gutter 4 → 17 text columns
        let mut buf = Vec::new();
        render(&mut ed, &mut buf, w, h).unwrap();
        assert_eq!(ed.docs[0].left, 60 - 17, "the last 漢 (cols 58–59) fits whole");
        assert!(String::from_utf8_lossy(&buf).contains("\x1b[?25h"), "cursor shown");
        let rows = screen(&buf, w as usize, h as usize);
        let row = rows.iter().find(|r| r.contains('漢')).unwrap();
        // 漢 at col 42 straddles left = 43 → one blank, then 漢 from col 44 at x = 1
        assert_eq!(row.trim_end(), format!("  1  {}", "漢".repeat(8)), "{rows:#?}");
    }

    /// Prose wraps at word boundaries with list items hanging; `j`/`k` go by rows; a click on a
    /// continuation row lands in that row; a line taller than the screen shows the cursor's part.
    #[test]
    fn soft_wrap_rows_motion_and_clicks() {
        let mut ed = Editor::new(Config::default());
        let src = "- one two three four five six\nnext\n";
        ed.docs[0].text = Rope::from_str(src);
        let (w, h) = (20, 8); // gutter 4 → 16 text columns
        let mut buf = Vec::new();
        render(&mut ed, &mut buf, w, h).unwrap();
        let rows = screen(&buf, w as usize, h as usize);
        // Row 0 is the path bar; line numbers are relative
        assert_eq!(rows[1].trim_end(), "  1 - one two three", "{rows:#?}");
        assert_eq!(rows[2].trim_end(), "      four five six", "no number, hangs under the text");
        assert_eq!(rows[3].trim_end(), "  1 next");
        let head = |ed: &Editor| ed.doc().selection().primary().cursor(&ed.doc().text);
        let key = |ed: &mut Editor, k: &str| {
            ed.handle_key(k.parse().unwrap());
            render(ed, &mut Vec::new(), w, h).unwrap();
        };
        key(&mut ed, "j");
        assert_eq!(head(&ed), src.find("four").unwrap(), "down one row, same screen x");
        key(&mut ed, "j");
        assert_eq!(head(&ed), src.find("next").unwrap());
        key(&mut ed, "k");
        assert_eq!(head(&ed), src.find("four").unwrap());
        let click =
            crate::event::Mouse { kind: crate::event::MouseKind::Down, x: 4 + 2 + 5, y: 2, alt: false };
        ed.handle_mouse(click);
        assert_eq!(head(&ed), src.find("five").unwrap());
        // One line taller than the screen: the rows around the cursor show
        let long = "word ".repeat(60);
        ed.docs[0].text = Rope::from_str(&long);
        ed.docs[0].set_selection(crate::selection::Selection::point(long.len() - 3));
        let mut buf = Vec::new();
        render(&mut ed, &mut buf, w, h).unwrap();
        assert!(String::from_utf8_lossy(&buf).contains("\x1b[?25h"), "cursor shown");
        assert_eq!(ed.docs[0].left, 0, "never sideways");
    }

    /// `gw` puts two-letter labels on the words (nearest first); the first key narrows them to their second
    /// letter, the second picks the word (a jump). Any other key cancels.
    #[test]
    fn gw_labels_pick_a_word() {
        let mut ed = Editor::new(Config::default());
        ed.docs[0].text = Rope::from_str("alpha beta gamma\n");
        let (w, h) = (30, 6);
        let key = |ed: &mut Editor, k: &str| {
            ed.handle_key(k.parse().unwrap());
            let mut buf = Vec::new();
            render(ed, &mut buf, w, h).unwrap();
            screen(&buf, w as usize, h as usize)[1].trim_end().to_string()
        };
        render(&mut ed, &mut Vec::new(), w, h).unwrap();
        key(&mut ed, "g");
        // After the cursor first: beta = aa, then alpha (the cursor's own word) = ab, gamma = ac
        assert_eq!(key(&mut ed, "w"), "  1 abpha aata acmma", "labels cover the first two chars");
        assert_eq!(key(&mut ed, "a"), "  1 blpha aeta camma", "only the second letters are left");
        key(&mut ed, "c");
        let r = ed.doc().selection().primary();
        assert_eq!((r.from(), r.to()), (11, 16), "gamma selected");
        assert!(ed.jump_labels.is_none());
        key(&mut ed, "g");
        key(&mut ed, "w");
        assert_eq!(key(&mut ed, "esc"), "  1 alpha beta gamma", "cancelled");
        assert_eq!(ed.doc().selection().primary(), r);
    }

    #[test]
    fn osc11_replies() {
        let dark = parse_osc11(b"\x1b]11;rgb:1010/1212/1717\x1b\\\x1b[?62;22c").unwrap();
        assert!(dark.0 < 0.1);
        let light = parse_osc11(b"\x1b]11;rgb:f5/f1/e8\x07").unwrap();
        assert!(light.0 > 0.9 && light.2 > 0.85);
        assert_eq!(parse_osc11(b"\x1b[?1;2c"), None, "terminal that doesn't know OSC 11 (DA1 only)");
    }

    /// Time for one key → one frame. Key handling once (it changes state), drawing the fastest of 3 —
    /// excludes scheduling noise from parallel tests (ones spawning processes, etc.) to time just the code.
    fn key_to_frame(ed: &mut Editor, buf: &mut Vec<u8>, k: &str, w: u16, h: u16) -> Duration {
        let t = Instant::now();
        ed.handle_key(k.parse().unwrap());
        let key = t.elapsed();
        let frame = (0..3)
            .map(|_| {
                buf.clear();
                let t = Instant::now();
                render(ed, buf, w, h).unwrap();
                t.elapsed()
            })
            .min()
            .unwrap();
        key + frame
    }

    /// One frame at 60 Hz. `TARAE_PERF_SLACK` (a multiplier) loosens it on slow shared machines — CI sets 3,
    /// which still catches an order-of-magnitude regression.
    fn frame_budget() -> Duration {
        let slack = std::env::var("TARAE_PERF_SLACK").ok().and_then(|s| s.parse::<u32>().ok()).unwrap_or(1);
        Duration::from_millis(16) * slack.max(1)
    }

    /// Performance budget: in a 200k-line file, key input → finished frame within one frame (16 ms, 60 Hz).
    /// Held even for debug builds — release is much faster.
    #[test]
    fn perf_budget_key_to_frame_on_large_file() {
        let budget = frame_budget();
        let text: String =
            (0..200_000).map(|i| format!("line {i} 타래 fn main() {{ let x = {i}; }}\n")).collect();
        let mut ed = Editor::new(Config::default());
        ed.docs[0].text = Rope::from_str(&text);
        let mut buf = Vec::with_capacity(1 << 16);
        render(&mut ed, &mut buf, 120, 40).unwrap();

        let keys = [
            "j", "j", "w", "w", "e", "x", "x", "C", "C", "g", "e", "g", "g", "5", "0", "0", "0", "G", "i",
            "a", "esc", "u", "%", ";", "C-d", "C-u",
        ];
        let mut worst = (Duration::ZERO, "");
        for k in keys {
            let dt = key_to_frame(&mut ed, &mut buf, k, 120, 40);
            if dt > worst.0 {
                worst = (dt, k);
            }
        }
        assert!(worst.0 < budget, "key {:?} took {:?} (budget {:?})", worst.1, worst.0, budget);
        eprintln!("perf: worst key->frame {:?} on {:?}", worst.0, worst.1);
    }

    #[test]
    fn test_names_drop_their_shared_prefix() {
        let v = |x: &[&str]| short_names(x.iter().copied());
        assert_eq!(v(&["math::tests::adds", "math::tests::divides"]), ["adds", "divides"]);
        assert_eq!(v(&["AppTest.greets", "AppTest.Edge.empty"]), ["greets", "Edge.empty"]);
        assert_eq!(
            v(&["TestAdd", "TestAddNegative"]),
            ["TestAdd", "TestAddNegative"],
            "no stripping when not at a boundary"
        );
        assert_eq!(v(&["TestDiv::test_half"]), ["test_half"]);
        assert_eq!(v(&["test_add", "TestDiv::test_half"]), ["test_add", "TestDiv::test_half"]);
    }

    #[test]
    fn statusline_drops_least_important_first() {
        let seg = |t: &str, keep| Seg { text: t.into(), style: Style::default(), keep };
        let fit = |width| {
            let mut l = vec![seg(" NOR ", 255), seg("  tarae · main", 80), seg("  a.rs", 200)];
            let mut r = vec![seg("rust  ", 60), seg("utf-8 LF  ", 40), seg("1:1 ", 250)];
            fit_segments(&mut l, &mut r, width);
            l.iter().chain(&r).map(|s| s.text.clone()).collect::<Vec<_>>()
        };
        assert_eq!(fit(31), [" NOR ", "  tarae · main", "  a.rs", "1:1 "], "drops encoding, then language");
        // One cell less and `a.rs` would touch `1:1` — the branch goes instead
        assert_eq!(fit(30), [" NOR ", "  a.rs", "1:1 "]);
    }

    /// Same budget with syntax highlighting on — a big Rust file (only on machines with the grammar).
    #[test]
    fn perf_budget_with_syntax_highlighting() {
        let budget = frame_budget();
        let Ok(lang) = crate::syntax::Loader::global().load(crate::syntax::spec("rust").unwrap()) else {
            return;
        };
        let body: String = (0..20_000)
            .map(|i| {
                format!("fn f{i}(x: &str) -> Option<u32> {{ let s = \"타래 {i}\"; x.parse().ok() }} // c\n")
            })
            .collect();
        let mut ed = Editor::new(Config::default());
        ed.docs[0].text = Rope::from_str(&body);
        let mut syn = crate::syntax::Syntax::new(lang);
        let job = syn.start_parse(&ed.docs[0].text);
        let (generation, t0) = (job.generation, Instant::now());
        syn.finish_parse(generation, job.run());
        let full_parse = t0.elapsed();
        ed.docs[0].syntax = Some(syn);
        let mut buf = Vec::with_capacity(1 << 16);
        render(&mut ed, &mut buf, 160, 50).unwrap();
        let mut worst = (Duration::ZERO, "");
        for k in ["j", "w", "x", "d", "u", "g", "e", "C-u", "%", ";", "i", "a", "esc"] {
            let dt = key_to_frame(&mut ed, &mut buf, k, 160, 50);
            if dt > worst.0 {
                worst = (dt, k);
            }
        }
        assert!(worst.0 < budget, "key {:?} took {:?} (budget {:?})", worst.1, worst.0, budget);
        eprintln!(
            "perf+syntax: worst key->frame {:?} on {:?} (full parse of 20k lines {full_parse:?}, off main thread)",
            worst.0, worst.1
        );
    }
}
