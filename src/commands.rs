//! Static command registry — names match helix command names (so helix key configs just work).

use std::fmt;

use crate::editor::{Editor, Mode, PromptKind};
use crate::movement::{self as mv, Direction};
use crate::search;
use crate::selection::{Range, Selection};
use crate::textobject as to;
use crate::transaction::{Assoc, Change, Transaction};
use unicode_segmentation::UnicodeSegmentation;

pub struct Context<'a> {
    pub editor: &'a mut Editor,
    pub count: Option<usize>,
}

impl Context<'_> {
    pub fn count(&self) -> usize {
        self.count.unwrap_or(1).max(1)
    }
}

pub type CommandFn = fn(&mut Context);

pub struct StaticCommand {
    pub name: &'static str,
    pub doc: &'static str,
    pub fun: CommandFn,
    /// Whether `repeat_last_motion` repeats this.
    pub motion: bool,
}

impl fmt::Debug for StaticCommand {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name)
    }
}

pub fn find(name: &str) -> Option<&'static StaticCommand> {
    COMMANDS.iter().find(|c| c.name == name)
}

macro_rules! commands {
    (
        motions { $($m:ident => $mdoc:literal,)* }
        others { $($o:ident => $odoc:literal,)* }
    ) => {
        pub static COMMANDS: &[StaticCommand] = &[
            $(StaticCommand { name: stringify!($m), doc: $mdoc, fun: $m, motion: true },)*
            $(StaticCommand { name: stringify!($o), doc: $odoc, fun: $o, motion: false },)*
        ];
    };
}

commands! {
    motions {
        move_char_left => "Move left",
        move_char_right => "Move right",
        move_line_up => "Move up",
        move_line_down => "Move down",
        move_visual_line_up => "Move up (visual line)",
        move_visual_line_down => "Move down (visual line)",
        extend_char_left => "Extend left",
        extend_char_right => "Extend right",
        extend_line_up => "Extend up",
        extend_line_down => "Extend down",
        extend_visual_line_up => "Extend up (visual line)",
        extend_visual_line_down => "Extend down (visual line)",
        move_next_word_start => "Move to start of next word",
        move_prev_word_start => "Move to start of previous word",
        move_next_word_end => "Move to end of next word",
        extend_next_word_start => "Extend to start of next word",
        extend_prev_word_start => "Extend to start of previous word",
        extend_next_word_end => "Extend to end of next word",
        goto_line_start => "Goto line start",
        goto_line_end_newline => "Goto end of line (after the last char)",
        goto_line_end => "Goto line end",
        goto_first_nonwhitespace => "Goto first non-blank in line",
        goto_file_start => "Go to file start (or line <n>)",
        goto_last_line => "Goto last line",
        goto_line => "Go to last line (or line <n>)",
        page_down => "Move page down",
        page_up => "Move page up",
        half_page_down => "Move half page down",
        half_page_up => "Move half page up",
    }
    others {
        normal_mode => "Enter normal mode",
        select_mode => "Enter selection extend mode",
        insert_mode => "Insert before selection",
        append_mode => "Append after selection",
        insert_at_line_start => "Insert at start of line",
        insert_at_line_end => "Insert at end of line",
        open_below => "Open new line below selection",
        open_above => "Open new line above selection",
        command_mode => "Enter command mode",
        delete_selection => "Delete selection",
        delete_selection_noyank => "Delete selection without yanking",
        change_selection => "Change selection",
        change_selection_noyank => "Change selection without yanking",
        yank => "Yank selection",
        paste_after => "Paste after selection",
        paste_before => "Paste before selection",
        undo => "Undo change",
        redo => "Redo change",
        collapse_selection => "Collapse selection into single cursor",
        flip_selections => "Flip selection cursor and anchor",
        keep_primary_selection => "Keep primary selection",
        select_all => "Select whole document",
        extend_line_below => "Select line (again: extend down)",
        extend_line_above => "Select line (again: extend up)",
        extend_to_line_bounds => "Extend selection to line bounds",
        copy_selection_on_next_line => "Copy selection on next line",
        copy_selection_on_prev_line => "Copy selection on previous line",
        repeat_last_motion => "Repeat last motion",
        find_next_char => "Move to next occurrence of char",
        find_till_char => "Move till next occurrence of char",
        find_prev_char => "Move to previous occurrence of char",
        till_prev_char => "Move till previous occurrence of char",
        replace => "Replace with new char",
        switch_case => "Switch (toggle) case",
        switch_to_lowercase => "Switch to lowercase",
        switch_to_uppercase => "Switch to uppercase",
        indent => "Indent selection",
        unindent => "Unindent selection",
        join_selections => "Join lines inside selection",
        select_register => "Select register for the next command",
        match_brackets => "Goto matching bracket",
        goto_next_function => "Next function",
        goto_prev_function => "Previous function",
        goto_next_class => "Next type",
        goto_prev_class => "Previous type",
        goto_next_parameter => "Next argument",
        goto_prev_parameter => "Previous argument",
        goto_next_comment => "Next comment",
        goto_prev_comment => "Previous comment",
        goto_next_test => "Next test",
        goto_prev_test => "Previous test",
        goto_next_paragraph => "Next paragraph",
        goto_prev_paragraph => "Previous paragraph",
        expand_selection => "Grow selection to the enclosing syntax node",
        shrink_selection => "Shrink selection back",
        select_next_sibling => "Select next syntax sibling",
        select_prev_sibling => "Select previous syntax sibling",
        add_newline_below => "Add a blank line below",
        add_newline_above => "Add a blank line above",
        select_textobject_inner => "Select inside object",
        select_textobject_around => "Select around object",
        surround_add => "Surround add",
        surround_replace => "Surround replace",
        surround_delete => "Surround delete",
        yank_to_clipboard => "Copy to clipboard",
        paste_clipboard_after => "Paste clipboard after",
        paste_clipboard_before => "Paste clipboard before",
        record_macro => "Record macro",
        replay_macro => "Replay macro",
        llm_ask => "Ask Claude to edit selection",
        chat_open => "Chat with Claude",
        command_palette => "Find a command",
        theme_picker => "Choose a theme (live preview)",
        symbol_picker => "Symbols in this file",
        vsplit => "Split the window side by side",
        toggle_breakpoint => "Toggle breakpoint",
        dap_launch => "Start debugging / continue",
        dap_next => "Step over",
        dap_step_in => "Step into",
        dap_step_out => "Step out",
        dap_pause => "Pause the program",
        dap_terminate => "Stop debugging",
        dap_attach => "Attach to a running program…",
        test_nearest => "Run the test at the cursor",
        test_debug => "Debug the test at the cursor",
        test_file => "Run this file's tests",
        test_last => "Run the last test again",
        test_close => "Close the test panel",
        goto_next_test_failure => "Next failed test",
        goto_prev_test_failure => "Previous failed test",
        claude_code => "Open Claude Code beside (connected)",
        claude_code_mention => "Send selection to Claude Code (@)",
        dap_edit_condition => "Break only when… (condition)",
        dap_edit_log => "Log here instead of stopping",
        dap_watch => "Watch an expression",
        dap_unwatch => "Remove a watch",
        hsplit => "Split the window top and bottom",
        rotate_view => "Next window",
        jump_view_left => "Window to the left",
        jump_view_right => "Window to the right",
        jump_view_up => "Window above",
        jump_view_down => "Window below",
        wclose => "Close this window",
        wonly => "Close all other windows",
        workspace_symbol_picker => "Symbols in the workspace",
        chat_close => "Close chat",
        code_action => "Code actions",
        rename_symbol => "Rename symbol",
        goto_definition => "Goto definition (LSP)",
        goto_declaration => "Goto declaration (LSP)",
        goto_type_definition => "Goto type definition (LSP)",
        goto_implementation => "Goto implementation (LSP)",
        goto_reference => "Goto references (LSP)",
        goto_last_accessed_file => "Goto last accessed file",
        jumplist_picker => "Jump list",
        last_picker => "Reopen the last picker",
        jump_backward => "Jump back to the previous spot",
        jump_forward => "Jump forward again",
        save_selection => "Save this spot to jump back to",
        hover => "Show docs under cursor",
        completion => "Invoke completion popup (LSP)",
        goto_next_diag => "Next diagnostic",
        goto_prev_diag => "Previous diagnostic",
        goto_next_change => "Next git change",
        goto_prev_change => "Previous git change",
        diagnostics_picker => "Diagnostics",
        file_picker => "Open file picker",
        buffer_picker => "Open buffer picker",
        global_search => "Search in project",
        search => "Search for regex pattern",
        rsearch => "Reverse search for regex pattern",
        search_next => "Select next search match",
        search_prev => "Select previous search match",
        search_selection => "Search for selection",
        select_regex => "Select regex matches",
        split_selection => "Split selections on regex matches",
        split_selection_on_newline => "Split selection on newlines",
        keep_selections => "Keep selections matching the regex",
        remove_selections => "Remove selections matching the regex",
        goto_next_buffer => "Goto next buffer",
        goto_previous_buffer => "Goto previous buffer",
        insert_newline => "Insert newline char",
        insert_tab => "Insert tab char",
        delete_char_backward => "Delete previous char",
        delete_char_forward => "Delete next char",
        delete_word_backward => "Delete previous word",
        delete_word_forward => "Delete next word",
        kill_to_line_start => "Delete to start of line",
        kill_to_line_end => "Delete to end of line",
        insert_register => "Insert register contents",
        commit_undo_checkpoint => "Make what's typed an undo step",
        repeat_last_insert => "Repeat last insert",
        toggle_comments => "Comment or uncomment lines",
        shell_pipe => "Pipe selections through a command",
        shell_pipe_to => "Send selections to a command",
        shell_insert_output => "Insert a command's output before",
        shell_append_output => "Insert a command's output after",
        shell_keep_pipe => "Keep selections a command accepts",
    }
}

// ── Shared helpers ────────────────────────────────────────────────────────

fn is_select(cx: &Context) -> bool {
    cx.editor.mode == Mode::Select
}

/// Applies a pure motion function to every range.
fn motion(cx: &mut Context, f: impl Fn(&ropey::Rope, Range) -> Range) {
    let doc = cx.editor.doc_mut();
    let sel = doc.selection().transform(|r| f(&doc.text, r));
    doc.set_selection(sel);
}

fn horizontal(cx: &mut Context, dir: Direction, extend: bool) {
    let n = cx.count();
    motion(cx, |t, r| mv::move_horizontally(t, r, dir, n, extend));
}

/// Vertical motion uses a sticky column — during consecutive j/k it returns to the original column
/// after passing short lines.
fn vertical(cx: &mut Context, dir: Direction, extend: bool, lines: usize) {
    let tab = cx.editor.config.tab_width;
    let sticky = cx.editor.sticky.take();
    let doc = cx.editor.doc_mut();
    let ranges = doc.selection().ranges().to_vec();
    let cols: Vec<usize> = match sticky {
        Some(c) if c.len() == ranges.len() => c,
        _ => ranges.iter().map(|&r| mv::cursor_col(&doc.text, r, tab)).collect(),
    };
    let moved = ranges
        .iter()
        .zip(&cols)
        .map(|(&r, &col)| mv::move_vertically(&doc.text, r, dir, lines, extend, col, tab))
        .collect();
    let primary = doc.selection().primary_index();
    doc.set_selection(Selection::new(moved, primary));
    cx.editor.sticky = Some(cols);
    cx.editor.sticky_used = true;
}

/// Inserts a string at each point and puts the cursor at `cursor_offset` inside the inserted text.
/// Input: (insert position, text, cursor offset [chars]). Duplicate positions keep only the first entry.
fn insert_at(cx: &mut Context, mut items: Vec<(usize, String, usize)>) {
    items.sort_by_key(|(p, _, _)| *p);
    items.dedup_by_key(|(p, _, _)| *p);
    let doc = cx.editor.doc_mut();
    let primary = doc.selection().primary_index().min(items.len().saturating_sub(1));
    // An empty text only moves the cursor (auto-pairs stepping over a closer)
    let tx = Transaction::new(
        items
            .iter()
            .filter(|(_, s, _)| !s.is_empty())
            .map(|(p, s, _)| Change::insert(*p, s.clone()))
            .collect(),
    );
    let ranges: Vec<Range> =
        items.iter().map(|(p, _, off)| Range::point(tx.map_pos(*p, Assoc::Before) + off)).collect();
    if ranges.is_empty() {
        return;
    }
    doc.apply_with(&tx, Selection::new(ranges, primary));
}

fn yanked_texts(cx: &Context) -> Vec<String> {
    let doc = cx.editor.doc();
    doc.selection()
        .ranges()
        .iter()
        .map(|r| {
            let r = r.min_width_1(&doc.text);
            doc.text.byte_slice(r.from()..r.to()).to_string()
        })
        .collect()
}

fn delete_impl(cx: &mut Context, yank: bool) {
    if yank {
        let texts = yanked_texts(cx);
        let reg = cx.editor.register_name();
        cx.editor.write_register(reg, texts);
    }
    let doc = cx.editor.doc_mut();
    let tx = Transaction::change_by_selection(doc.selection(), |r| {
        let r = r.min_width_1(&doc.text);
        (r.from() < r.to()).then(|| Change::delete(r.from(), r.to()))
    });
    doc.apply(&tx);
}

// ── Motion ───────────────────────────────────────────────────────────────

fn move_char_left(cx: &mut Context) {
    horizontal(cx, Direction::Backward, false)
}
fn move_char_right(cx: &mut Context) {
    horizontal(cx, Direction::Forward, false)
}
fn extend_char_left(cx: &mut Context) {
    horizontal(cx, Direction::Backward, true)
}
fn extend_char_right(cx: &mut Context) {
    horizontal(cx, Direction::Forward, true)
}
fn move_line_up(cx: &mut Context) {
    let n = cx.count();
    vertical(cx, Direction::Backward, false, n)
}
fn move_line_down(cx: &mut Context) {
    let n = cx.count();
    vertical(cx, Direction::Forward, false, n)
}
// Visual line = screen line: no soft wrap, and a folded doc-comment block counts as one line
// (passing through it in normal mode doesn't unfold it).
fn move_visual_line_up(cx: &mut Context) {
    let n = cx.count();
    visual(cx, Direction::Backward, false, n);
    over_folded_docs(cx, false);
}
fn move_visual_line_down(cx: &mut Context) {
    let n = cx.count();
    visual(cx, Direction::Forward, false, n);
    over_folded_docs(cx, true);
}

/// Screen-row motion: over wrapped rows when this document soft-wraps (the sticky value is then the x
/// within the row), else by lines.
fn visual(cx: &mut Context, dir: Direction, extend: bool, n: usize) {
    let width = cx.editor.viewport.1;
    if !cx.editor.wraps(cx.editor.doc()) {
        return vertical(cx, dir, extend, n);
    }
    let tab = cx.editor.config.tab_width;
    let sticky = cx.editor.sticky.take();
    let doc = cx.editor.doc_mut();
    let ranges = doc.selection().ranges().to_vec();
    let xs: Vec<usize> = match sticky {
        Some(x) if x.len() == ranges.len() => x,
        _ => ranges.iter().map(|&r| crate::wrap::x_of(&doc.text, r.cursor(&doc.text), width, tab)).collect(),
    };
    let moved = ranges
        .iter()
        .zip(&xs)
        .map(|(&r, &x)| crate::wrap::move_rows(&doc.text, r, dir, n, extend, x, width, tab))
        .collect();
    let primary = doc.selection().primary_index();
    doc.set_selection(Selection::new(moved, primary));
    cx.editor.sticky = Some(xs);
    cx.editor.sticky_used = true;
}

/// If the cursor landed inside a folded doc-comment block: moving up → the block's first line,
/// moving down within the block → past it.
fn over_folded_docs(cx: &mut Context, down: bool) {
    let line = cx.editor.cursor_line();
    let Some((a, b)) = cx.editor.folded_doc_block_at(line) else { return };
    if line <= a {
        return;
    }
    // Entered the block past its first line — the block is one line: down = past it, up = its first line
    let dir = if down { Direction::Forward } else { Direction::Backward };
    while if down { cx.editor.cursor_line() <= b } else { cx.editor.cursor_line() > a } {
        let before = cx.editor.cursor_line();
        vertical(cx, dir, false, 1);
        if cx.editor.cursor_line() == before {
            break; // edge of the document
        }
    }
}
// Note: in helix, extend_line_up/down is an "extend up/down" motion,
// while line-wise selection is extend_line_above/below (confusing names, but we follow helix).
fn extend_line_up(cx: &mut Context) {
    let n = cx.count();
    vertical(cx, Direction::Backward, true, n)
}
fn extend_line_down(cx: &mut Context) {
    let n = cx.count();
    vertical(cx, Direction::Forward, true, n)
}
fn extend_visual_line_up(cx: &mut Context) {
    let n = cx.count();
    visual(cx, Direction::Backward, true, n)
}
fn extend_visual_line_down(cx: &mut Context) {
    let n = cx.count();
    visual(cx, Direction::Forward, true, n)
}

fn word(cx: &mut Context, f: fn(&ropey::Rope, Range, usize, bool) -> Range, extend: bool) {
    let n = cx.count();
    motion(cx, |t, r| f(t, r, n, extend));
}
fn move_next_word_start(cx: &mut Context) {
    word(cx, mv::next_word_start, false)
}
fn move_prev_word_start(cx: &mut Context) {
    word(cx, mv::prev_word_start, false)
}
fn move_next_word_end(cx: &mut Context) {
    word(cx, mv::next_word_end, false)
}
fn extend_next_word_start(cx: &mut Context) {
    word(cx, mv::next_word_start, true)
}
fn extend_prev_word_start(cx: &mut Context) {
    word(cx, mv::prev_word_start, true)
}
fn extend_next_word_end(cx: &mut Context) {
    word(cx, mv::next_word_end, true)
}

// goto commands extend on their own in select mode, like helix.
fn goto_in_line(cx: &mut Context, f: fn(&ropey::Rope, usize) -> usize) {
    let extend = is_select(cx);
    motion(cx, |t, r| r.put_cursor(t, f(t, mv::line_of(t, r.cursor(t))), extend));
}
fn goto_line_start(cx: &mut Context) {
    goto_in_line(cx, mv::line_start)
}
fn goto_line_end(cx: &mut Context) {
    // Onto the last character, not the newline (line start if the line is empty).
    goto_in_line(cx, |t, line| {
        let (start, end) = (mv::line_start(t, line), mv::line_end(t, line));
        if end > start { crate::graphemes::prev_boundary(t, end) } else { start }
    })
}
fn goto_line_end_newline(cx: &mut Context) {
    goto_in_line(cx, mv::line_end)
}
fn goto_first_nonwhitespace(cx: &mut Context) {
    goto_in_line(cx, mv::first_non_whitespace)
}
fn goto_line_number(cx: &mut Context, line: usize) {
    let extend = is_select(cx);
    motion(cx, |t, r| r.put_cursor(t, mv::line_start(t, line.min(mv::last_line(t))), extend));
}
fn goto_file_start(cx: &mut Context) {
    let line = cx.count.map(|n| n.saturating_sub(1)).unwrap_or(0);
    cx.editor.push_jump();
    goto_line_number(cx, line)
}
fn goto_last_line(cx: &mut Context) {
    cx.editor.push_jump();
    goto_line_number(cx, usize::MAX)
}
fn goto_line(cx: &mut Context) {
    let line = cx.count.map(|n| n.saturating_sub(1)).unwrap_or(usize::MAX);
    cx.editor.push_jump();
    goto_line_number(cx, line)
}

fn scroll_move(cx: &mut Context, dir: Direction, divisor: usize) {
    let lines = (cx.editor.viewport.0 / divisor).max(1) * cx.count();
    let extend = is_select(cx);
    visual(cx, dir, extend, lines);
}
fn page_down(cx: &mut Context) {
    scroll_move(cx, Direction::Forward, 1)
}
fn page_up(cx: &mut Context) {
    scroll_move(cx, Direction::Backward, 1)
}
fn half_page_down(cx: &mut Context) {
    scroll_move(cx, Direction::Forward, 2)
}
fn half_page_up(cx: &mut Context) {
    scroll_move(cx, Direction::Backward, 2)
}

fn repeat_last_motion(cx: &mut Context) {
    if let Some(m) = cx.editor.last_motion.clone() {
        m(cx);
    }
}

// f/t/F/T — take one character from the next key and select up to it (t/T: up to just before it).
fn find_char(cx: &mut Context, forward: bool, inclusive: bool) {
    let f: crate::editor::CharFn = match (forward, inclusive) {
        (true, true) => |cx, ch| do_find_char(cx, ch, true, true),
        (true, false) => |cx, ch| do_find_char(cx, ch, true, false),
        (false, true) => |cx, ch| do_find_char(cx, ch, false, true),
        (false, false) => |cx, ch| do_find_char(cx, ch, false, false),
    };
    cx.editor.on_next_char = Some((f, cx.count));
}
fn do_find_char(cx: &mut Context, ch: char, forward: bool, inclusive: bool) {
    let (n, extend) = (cx.count(), is_select(cx));
    motion(cx, |t, r| mv::find_char(t, r, ch, forward, inclusive, n, extend));
    let count = cx.count;
    cx.editor.last_motion = Some(std::rc::Rc::new(move |cx: &mut Context| {
        do_find_char(&mut Context { editor: cx.editor, count }, ch, forward, inclusive)
    }));
}
fn find_next_char(cx: &mut Context) {
    find_char(cx, true, true)
}
fn find_till_char(cx: &mut Context) {
    find_char(cx, true, false)
}
fn find_prev_char(cx: &mut Context) {
    find_char(cx, false, true)
}
fn till_prev_char(cx: &mut Context) {
    find_char(cx, false, false)
}

// ── Modes ────────────────────────────────────────────────────────────────

fn normal_mode(cx: &mut Context) {
    cx.editor.mode = Mode::Normal;
}
fn select_mode(cx: &mut Context) {
    cx.editor.mode = if is_select(cx) { Mode::Normal } else { Mode::Select };
}
fn command_mode(cx: &mut Context) {
    cx.editor.open_prompt(PromptKind::Command, "");
}

fn enter_insert(cx: &mut Context, f: impl Fn(&ropey::Rope, Range) -> usize) {
    motion(cx, |t, r| Range::point(f(t, r)));
    cx.editor.mode = Mode::Insert;
}
fn insert_mode(cx: &mut Context) {
    enter_insert(cx, |_, r| r.from())
}
fn append_mode(cx: &mut Context) {
    enter_insert(cx, |t, r| r.min_width_1(t).to())
}
fn insert_at_line_start(cx: &mut Context) {
    enter_insert(cx, |t, r| mv::first_non_whitespace(t, mv::line_of(t, r.cursor(t))))
}
fn insert_at_line_end(cx: &mut Context) {
    enter_insert(cx, |t, r| mv::line_end(t, mv::line_of(t, r.cursor(t))))
}
fn open_below(cx: &mut Context) {
    let doc = cx.editor.doc();
    let items = doc
        .selection()
        .ranges()
        .iter()
        .map(|&r| {
            let line = mv::line_span(&doc.text, r).1;
            let text = format!("\n{}", mv::indent_of(&doc.text, line));
            let off = text.len();
            (mv::line_end(&doc.text, line), text, off)
        })
        .collect();
    insert_at(cx, items);
    cx.editor.mode = Mode::Insert;
}
fn open_above(cx: &mut Context) {
    let doc = cx.editor.doc();
    let items = doc
        .selection()
        .ranges()
        .iter()
        .map(|&r| {
            let line = mv::line_span(&doc.text, r).0;
            let indent = mv::indent_of(&doc.text, line);
            let off = indent.len();
            (mv::line_start(&doc.text, line), format!("{indent}\n"), off)
        })
        .collect();
    insert_at(cx, items);
    cx.editor.mode = Mode::Insert;
}

// ── Editing ──────────────────────────────────────────────────────────────

fn delete_selection(cx: &mut Context) {
    delete_impl(cx, true)
}
fn delete_selection_noyank(cx: &mut Context) {
    delete_impl(cx, false)
}
fn change_selection(cx: &mut Context) {
    delete_impl(cx, true);
    cx.editor.mode = Mode::Insert;
}
fn change_selection_noyank(cx: &mut Context) {
    delete_impl(cx, false);
    cx.editor.mode = Mode::Insert;
}

fn yank(cx: &mut Context) {
    let texts = yanked_texts(cx);
    let n = texts.len();
    let reg = cx.editor.register_name();
    cx.editor.write_register(reg, texts);
    let to = match reg {
        '"' => String::new(),
        '+' => " to clipboard".into(),
        r => format!(" to register {r}"),
    };
    cx.editor.set_status(format!("yanked {n} selection{}{to}", if n == 1 { "" } else { "s" }));
}

/// Pastes from a register. Reading `+` (clipboard) is an external command, so it pastes once the
/// result arrives from a worker thread.
fn paste_from_register(cx: &mut Context, before: bool) {
    let count = cx.count();
    match cx.editor.register_name() {
        '+' => crate::clipboard::paste(&cx.editor.events.jobs(), move |ed, s| {
            ed.with_group(|cx| {
                for _ in 0..count {
                    paste(cx, before, std::slice::from_ref(&s));
                }
            })
        }),
        name => {
            let Some(values) = cx.editor.registers.get(&name).cloned() else {
                return cx.editor.set_error(format!("register {name} is empty"));
            };
            for _ in 0..count {
                paste(cx, before, &values);
            }
        }
    }
}

fn paste(cx: &mut Context, before: bool, reg: &[String]) {
    if reg.is_empty() {
        return;
    }
    let doc = cx.editor.doc_mut();
    let text = &doc.text;
    let len = text.len_bytes();
    // (position, inserted text, selection start offset, selection length) — all in bytes
    let mut items: Vec<(usize, String, usize, usize)> = Vec::new();
    for (i, &r) in doc.selection().ranges().iter().enumerate() {
        let s = &reg[i.min(reg.len() - 1)];
        let linewise = s.ends_with('\n');
        let (first, last) = mv::line_span(text, r);
        let r = r.min_width_1(text);
        let pos = match (before, linewise) {
            (false, false) => r.to(),
            (true, false) => r.from(),
            (false, true) => mv::line_full_end(text, last),
            (true, true) => mv::line_start(text, first),
        };
        let n = s.len();
        // When pasting line-wise after a final line with no newline, put a newline in front.
        if linewise && pos == len && len > 0 && text.byte(len - 1) != b'\n' {
            items.push((pos, format!("\n{}", s.trim_end_matches('\n')), 1, n - 1));
        } else {
            items.push((pos, s.clone(), 0, n));
        }
    }
    items.sort_by_key(|it| it.0);
    items.dedup_by_key(|it| it.0);
    let tx = Transaction::new(items.iter().map(|(p, s, _, _)| Change::insert(*p, s.clone())).collect());
    let ranges = items
        .iter()
        .map(|(p, _, off, n)| {
            let start = tx.map_pos(*p, Assoc::Before) + off;
            Range::new(start, start + n)
        })
        .collect();
    let primary = doc.selection().primary_index();
    doc.apply_with(&tx, Selection::new(ranges, primary));
}
fn paste_after(cx: &mut Context) {
    paste_from_register(cx, false)
}
fn paste_before(cx: &mut Context) {
    paste_from_register(cx, true)
}
fn yank_to_clipboard(cx: &mut Context) {
    cx.editor.selected_register = Some('+');
    yank(cx)
}
fn paste_clipboard_after(cx: &mut Context) {
    cx.editor.selected_register = Some('+');
    paste_from_register(cx, false)
}
fn paste_clipboard_before(cx: &mut Context) {
    cx.editor.selected_register = Some('+');
    paste_from_register(cx, true)
}

/// `"x` — pick the register for the next command.
fn select_register(cx: &mut Context) {
    cx.editor.on_next_char = Some((|cx, ch| cx.editor.selected_register = Some(ch), None));
}

/// Replaces each selection's text with `f`; the selection covers the new text (direction kept).
fn replace_each(cx: &mut Context, f: impl Fn(&str) -> String) {
    let doc = cx.editor.doc_mut();
    let items: Vec<(Range, String)> = doc
        .selection()
        .ranges()
        .iter()
        .map(|&r| {
            let w = r.min_width_1(&doc.text);
            (r, f(&doc.text.byte_slice(w.from()..w.to()).to_string()))
        })
        .collect();
    let tx = Transaction::new(
        items
            .iter()
            .map(|(r, s)| {
                let w = r.min_width_1(&doc.text);
                Change { from: w.from(), to: w.to(), insert: s.clone() }
            })
            .collect(),
    );
    let ranges = items
        .iter()
        .map(|(r, s)| {
            let start = tx.map_pos(r.from(), Assoc::Before);
            let end = start + s.len();
            if r.is_empty() {
                Range::point(start)
            } else if r.is_forward() {
                Range::new(start, end)
            } else {
                Range::new(end, start)
            }
        })
        .collect();
    let primary = doc.selection().primary_index();
    doc.apply_with(&tx, Selection::new(ranges, primary));
}

/// `r x` — every character in the selection becomes x (newlines stay).
fn replace(cx: &mut Context) {
    cx.editor.on_next_char = Some((
        |cx, ch| {
            // One per character (grapheme cluster) — 👍🏽 is one character too. Newlines stay.
            replace_each(cx, |s| {
                s.graphemes(true)
                    .map(|g| if g.ends_with('\n') || g == "\r" { g.to_string() } else { ch.to_string() })
                    .collect()
            })
        },
        None,
    ));
}
fn switch_case(cx: &mut Context) {
    replace_each(cx, |s| {
        s.chars()
            .flat_map(|c| {
                if c.is_lowercase() {
                    c.to_uppercase().collect::<Vec<_>>()
                } else {
                    c.to_lowercase().collect()
                }
            })
            .collect()
    })
}
fn switch_to_lowercase(cx: &mut Context) {
    replace_each(cx, |s| s.to_lowercase())
}
fn switch_to_uppercase(cx: &mut Context) {
    replace_each(cx, |s| s.to_uppercase())
}

/// Line numbers the selections span (deduplicated, ascending).
fn selected_lines(cx: &Context) -> Vec<usize> {
    let doc = cx.editor.doc();
    let mut lines: Vec<usize> = doc
        .selection()
        .ranges()
        .iter()
        .flat_map(|&r| {
            let (a, b) = mv::line_span(&doc.text, r);
            a..=b
        })
        .collect();
    lines.sort_unstable();
    lines.dedup();
    lines
}

/// `>` — indent each line by one unit (skips empty lines).
fn indent(cx: &mut Context) {
    let unit = " ".repeat(cx.editor.config.tab_width * cx.count());
    let lines = selected_lines(cx);
    let doc = cx.editor.doc_mut();
    let changes = lines
        .into_iter()
        .filter(|&l| mv::line_end(&doc.text, l) > mv::line_start(&doc.text, l))
        .map(|l| Change::insert(mv::line_start(&doc.text, l), unit.clone()))
        .collect();
    doc.apply(&Transaction::new(changes));
}

/// `<` — remove one indent unit per line (one tab = one unit).
fn unindent(cx: &mut Context) {
    let width = cx.editor.config.tab_width * cx.count();
    let lines = selected_lines(cx);
    let doc = cx.editor.doc_mut();
    let changes = lines
        .into_iter()
        .filter_map(|l| {
            let start = mv::line_start(&doc.text, l);
            let (mut end, mut cols) = (start, 0);
            for c in doc.text.byte_slice(start..).chars() {
                match c {
                    ' ' if cols < width => cols += 1,
                    '\t' if cols < width => cols = width,
                    _ => break,
                }
                end += 1; // space and tab are 1 byte
            }
            (end > start).then(|| Change::delete(start, end))
        })
        .collect();
    doc.apply(&Transaction::new(changes));
}

/// `J` — joins the lines the selection spans (a single-line selection joins with the next line).
/// Newline + the next line's leading whitespace → one space.
fn join_selections(cx: &mut Context) {
    let doc = cx.editor.doc_mut();
    let last = mv::last_line(&doc.text);
    let mut changes = Vec::new();
    for &r in doc.selection().ranges() {
        let (a, b) = mv::line_span(&doc.text, r);
        let b = if a == b { (a + 1).min(last) } else { b };
        for l in a..b {
            let from = mv::line_end(&doc.text, l);
            let to = mv::first_non_whitespace(&doc.text, l + 1);
            let sep = if to == mv::line_end(&doc.text, l + 1) { "" } else { " " };
            changes.push(Change { from, to, insert: sep.into() });
        }
    }
    doc.apply(&Transaction::new(changes));
}

// ── m mode: pairs, text objects, surround (body in textobject.rs) ─────────

fn match_brackets(cx: &mut Context) {
    let extend = is_select(cx);
    motion(cx, |t, r| match to::match_bracket(t, r.cursor(t)) {
        Some(p) => r.put_cursor(t, p, extend),
        None => r,
    });
}

fn select_textobject_inner(cx: &mut Context) {
    cx.editor.on_next_char = Some((|cx, ch| textobject(cx, ch, to::Kind::Inside), cx.count));
}
fn select_textobject_around(cx: &mut Context) {
    cx.editor.on_next_char = Some((|cx, ch| textobject(cx, ch, to::Kind::Around), cx.count));
}

fn textobject(cx: &mut Context, ch: char, kind: to::Kind) {
    let doc = cx.editor.doc();
    let text = &doc.text;
    let syn = doc.syntax.as_ref().and_then(|s| Some((s.lang.textobjects.as_ref()?, s.tree.as_ref()?)));
    let mut missing = false;
    let sel = doc.selection().transform(|r| {
        let found = match ch {
            'w' => Some(to::word_object(text, r, kind, false)),
            'W' => Some(to::word_object(text, r, kind, true)),
            'p' => Some(to::paragraph_object(text, r, kind)),
            c if to::pair_of(c).is_some() => to::pair_object(text, r, c, kind),
            c => match (to::treesitter_name(c), syn) {
                (Some(name), Some((q, tree))) => to::treesitter_object(q, tree, text, r, name, kind),
                _ => None,
            },
        };
        missing |= found.is_none();
        found.unwrap_or(r)
    });
    let has_syntax = syn.is_some();
    cx.editor.doc_mut().set_selection(sel);
    if missing {
        let what = if to::treesitter_name(ch).is_some() && !has_syntax {
            "needs syntax (tree-sitter) for this file"
        } else {
            "not found around every selection"
        };
        cx.editor.set_status(format!("m{ch}: {what}"));
    }
}

// ── Syntax structure (body in structure.rs) ───────────────────────────────

/// Each range through `f(tree, text, range)`; ranges it has no answer for stay. Says so when there's no
/// syntax tree, or nothing was found anywhere.
fn by_syntax(
    cx: &mut Context,
    what: &str,
    f: impl Fn(&tree_sitter::Tree, &ropey::Rope, Range) -> Option<Range>,
) {
    let doc = cx.editor.doc();
    let Some(tree) = doc.syntax.as_ref().and_then(|s| s.tree.as_ref()) else {
        return cx.editor.set_status(format!("{what} needs syntax (tree-sitter) for this file"));
    };
    let mut found = false;
    let sel = doc.selection().transform(|r| match f(tree, &doc.text, r) {
        Some(n) => {
            found = true;
            n
        }
        None => r,
    });
    if found {
        cx.editor.doc_mut().set_selection(sel);
    }
}

fn expand_selection(cx: &mut Context) {
    let before = cx.editor.doc().selection().clone();
    for _ in 0..cx.count() {
        by_syntax(cx, "A-o", crate::structure::expand);
    }
    let doc = cx.editor.doc();
    let after = doc.selection().clone();
    if after != before {
        let entry = (doc.id, doc.version(), before, after);
        cx.editor.expand_history.push(entry);
    }
}

fn shrink_selection(cx: &mut Context) {
    for _ in 0..cx.count() {
        // Retrace A-o while the selection is still what it left
        let doc = cx.editor.doc();
        let (id, version, now) = (doc.id, doc.version(), doc.selection().clone());
        match cx.editor.expand_history.pop() {
            Some((d, v, before, after)) if (d, v) == (id, version) && after == now => {
                cx.editor.doc_mut().set_selection(before);
            }
            _ => {
                cx.editor.expand_history.clear();
                by_syntax(cx, "A-i", crate::structure::shrink);
            }
        }
    }
}

fn select_next_sibling(cx: &mut Context) {
    for _ in 0..cx.count() {
        by_syntax(cx, "A-n", |t, x, r| crate::structure::sibling(t, x, r, Direction::Forward));
    }
}
fn select_prev_sibling(cx: &mut Context) {
    for _ in 0..cx.count() {
        by_syntax(cx, "A-p", |t, x, r| crate::structure::sibling(t, x, r, Direction::Backward));
    }
}

/// `]f` `[f` … — select the next/previous `name` textobject (count times). In select mode the selection
/// grows to it instead.
fn goto_object(cx: &mut Context, name: &str, dir: Direction) {
    let doc = cx.editor.doc();
    let Some(q) = doc.syntax.as_ref().and_then(|s| s.lang.textobjects.as_ref()) else {
        return cx.editor.set_status(format!("no {name}s here — needs syntax (tree-sitter) for this file"));
    };
    let extend = is_select(cx);
    let n = cx.count();
    let mut found = false;
    let sel = {
        let Some(tree) = doc.syntax.as_ref().and_then(|s| s.tree.as_ref()) else { return };
        let text = &doc.text;
        doc.selection().transform(|r| {
            let mut cur = r;
            for _ in 0..n {
                match crate::structure::textobject(q, tree, text, cur, name, dir) {
                    Some(o) => (cur, found) = (o, true),
                    None => break,
                }
            }
            if extend && cur != r {
                let to = if dir == Direction::Forward { cur.to() } else { cur.from() };
                r.put_cursor(text, to, true)
            } else {
                cur
            }
        })
    };
    if found {
        cx.editor.push_jump();
        cx.editor.doc_mut().set_selection(sel);
    } else {
        cx.editor
            .set_status(format!("no {} {name}", if dir == Direction::Forward { "next" } else { "previous" }));
    }
}
fn goto_next_function(cx: &mut Context) {
    goto_object(cx, "function", Direction::Forward)
}
fn goto_prev_function(cx: &mut Context) {
    goto_object(cx, "function", Direction::Backward)
}
fn goto_next_class(cx: &mut Context) {
    goto_object(cx, "class", Direction::Forward)
}
fn goto_prev_class(cx: &mut Context) {
    goto_object(cx, "class", Direction::Backward)
}
fn goto_next_parameter(cx: &mut Context) {
    goto_object(cx, "parameter", Direction::Forward)
}
fn goto_prev_parameter(cx: &mut Context) {
    goto_object(cx, "parameter", Direction::Backward)
}
fn goto_next_comment(cx: &mut Context) {
    goto_object(cx, "comment", Direction::Forward)
}
fn goto_prev_comment(cx: &mut Context) {
    goto_object(cx, "comment", Direction::Backward)
}
fn goto_next_test(cx: &mut Context) {
    goto_object(cx, "test", Direction::Forward)
}
fn goto_prev_test(cx: &mut Context) {
    goto_object(cx, "test", Direction::Backward)
}

/// `]p` / `[p` — to the first line of the next paragraph / the start of this (or the previous) one,
/// selecting what was passed over.
fn goto_paragraph(cx: &mut Context, dir: Direction) {
    let n = cx.count();
    let extend = is_select(cx);
    motion(cx, |t, r| {
        let blank = |l: usize| mv::first_non_whitespace(t, l) == mv::line_end(t, l);
        let last = mv::last_line(t);
        let mut line = mv::line_of(t, r.cursor(t));
        for _ in 0..n {
            match dir {
                Direction::Forward => {
                    while line < last && !blank(line) {
                        line += 1;
                    }
                    while line < last && blank(line) {
                        line += 1;
                    }
                }
                Direction::Backward => {
                    line = line.saturating_sub(1);
                    while line > 0 && blank(line) {
                        line -= 1;
                    }
                    while line > 0 && !blank(line - 1) {
                        line -= 1;
                    }
                }
            }
        }
        let pos = mv::line_start(t, line);
        if extend { r.put_cursor(t, pos, true) } else { Range::new(r.cursor(t), pos) }
    });
}
fn goto_next_paragraph(cx: &mut Context) {
    goto_paragraph(cx, Direction::Forward)
}
fn goto_prev_paragraph(cx: &mut Context) {
    goto_paragraph(cx, Direction::Backward)
}

/// `]space` / `[space` — blank lines below/above each selection's lines (count of them), staying in
/// normal mode with the selection where it was.
fn add_newline(cx: &mut Context, below: bool) {
    let n = cx.count();
    let doc = cx.editor.doc_mut();
    let text = &doc.text;
    let mut spots: Vec<usize> = doc
        .selection()
        .ranges()
        .iter()
        .map(|&r| {
            let (first, last) = mv::line_span(text, r);
            if below { mv::line_full_end(text, last) } else { mv::line_start(text, first) }
        })
        .collect();
    spots.sort_unstable();
    spots.dedup();
    let tx = Transaction::new(spots.iter().map(|&p| Change::insert(p, "\n".repeat(n))).collect());
    // Above: text at the insert point moves down with its line; below: it's the next line's, stays put
    let assoc = if below { Assoc::Before } else { Assoc::After };
    let sel =
        doc.selection().transform(|r| Range::new(tx.map_pos(r.anchor, assoc), tx.map_pos(r.head, assoc)));
    doc.apply_with(&tx, sel);
}
fn add_newline_below(cx: &mut Context) {
    add_newline(cx, true)
}
fn add_newline_above(cx: &mut Context) {
    add_newline(cx, false)
}

fn surround_add(cx: &mut Context) {
    cx.editor.on_next_char = Some((
        |cx, ch| {
            let (open, close) = to::pair_of(ch).unwrap_or((ch, ch));
            let doc = cx.editor.doc_mut();
            let ranges: Vec<Range> =
                doc.selection().ranges().iter().map(|r| r.min_width_1(&doc.text)).collect();
            let tx = Transaction::new(
                ranges
                    .iter()
                    .flat_map(|r| {
                        [
                            Change::insert(r.from(), open.to_string()),
                            Change::insert(r.to(), close.to_string()),
                        ]
                    })
                    .collect(),
            );
            let new = ranges
                .iter()
                .map(|r| Range::new(tx.map_pos(r.from(), Assoc::Before), tx.map_pos(r.to(), Assoc::After)))
                .collect();
            let primary = doc.selection().primary_index();
            doc.apply_with(&tx, Selection::new(new, primary));
        },
        None,
    ));
}

/// Finds the pair surrounding each selection and turns it into edits via
/// `f(open pos, close pos, open char, close char)`.
fn surround_edit(cx: &mut Context, ch: char, f: impl Fn(usize, usize, char, char) -> [Change; 2]) {
    let (open, close) = to::pair_of(ch).unwrap_or((ch, ch));
    let doc = cx.editor.doc_mut();
    let pairs: Vec<(usize, usize)> =
        doc.selection().ranges().iter().filter_map(|&r| to::find_pair(&doc.text, r, open, close)).collect();
    if pairs.is_empty() {
        return cx.editor.set_error(format!("no surrounding {open}{close}"));
    }
    let tx = Transaction::new(pairs.iter().flat_map(|&(o, c)| f(o, c, open, close)).collect());
    doc.apply(&tx);
}

fn surround_delete(cx: &mut Context) {
    cx.editor.on_next_char = Some((
        |cx, ch| {
            surround_edit(cx, ch, |o, c, open, close| {
                [Change::delete(o, o + open.len_utf8()), Change::delete(c, c + close.len_utf8())]
            })
        },
        None,
    ));
}

/// `mr x y` — keeps the first char in char_arg and waits again for the second.
fn surround_replace(cx: &mut Context) {
    cx.editor.on_next_char = Some((
        |cx, from| {
            cx.editor.char_arg = Some(from);
            cx.editor.on_next_char = Some((
                |cx, to_ch| {
                    let Some(from) = cx.editor.char_arg.take() else { return };
                    let (o2, c2) = to::pair_of(to_ch).unwrap_or((to_ch, to_ch));
                    surround_edit(cx, from, move |o, c, open, close| {
                        [
                            Change { from: o, to: o + open.len_utf8(), insert: o2.to_string() },
                            Change { from: c, to: c + close.len_utf8(), insert: c2.to_string() },
                        ]
                    })
                },
                None,
            ));
        },
        None,
    ));
}

// ── Macros ───────────────────────────────────────────────────────────────

/// `Q` — start/stop recording (default register `@`).
fn record_macro(cx: &mut Context) {
    let ed = &mut *cx.editor;
    match ed.recording.take() {
        Some((reg, mut keys)) => {
            keys.truncate(keys.len().saturating_sub(ed.last_trigger_len));
            ed.set_status(format!("recorded @{reg} ({} keys)", keys.len()));
            ed.macros.insert(reg, keys);
        }
        None => {
            let reg = ed.selected_register.unwrap_or('@');
            ed.recording = Some((reg, Vec::new()));
            ed.set_status(format!("recording @{reg} — Q to stop"));
        }
    }
}

/// `q` — replay (feeds the keys back in as is).
fn replay_macro(cx: &mut Context) {
    let reg = cx.editor.selected_register.unwrap_or('@');
    let Some(keys) = cx.editor.macros.get(&reg).cloned() else {
        return cx.editor.set_error(format!("no macro in @{reg}"));
    };
    if cx.editor.replaying.contains(&reg) || cx.editor.recording.as_ref().is_some_and(|(r, _)| *r == reg) {
        return cx.editor.set_error("macro can't replay itself");
    }
    cx.editor.selected_register = None;
    cx.editor.replaying.push(reg);
    for _ in 0..cx.count() {
        for &k in &keys {
            cx.editor.handle_key(k);
        }
    }
    cx.editor.replaying.pop();
}

fn undo(cx: &mut Context) {
    cx.editor.abandon_undo_group();
    for _ in 0..cx.count() {
        if !cx.editor.doc_mut().undo() {
            cx.editor.set_status("already at oldest change");
            break;
        }
    }
}
fn redo(cx: &mut Context) {
    cx.editor.abandon_undo_group();
    for _ in 0..cx.count() {
        if !cx.editor.doc_mut().redo() {
            cx.editor.set_status("already at newest change");
            break;
        }
    }
}

// ── Selection manipulation ───────────────────────────────────────────────

fn collapse_selection(cx: &mut Context) {
    motion(cx, |t, r| Range::point(r.cursor(t)))
}
fn flip_selections(cx: &mut Context) {
    motion(cx, |_, r| r.flip())
}
fn keep_primary_selection(cx: &mut Context) {
    let doc = cx.editor.doc_mut();
    let sel = doc.selection().keep_primary();
    doc.set_selection(sel);
}
fn select_all(cx: &mut Context) {
    let doc = cx.editor.doc_mut();
    let len = doc.text.len_bytes();
    doc.set_selection(Selection::single(Range::new(0, len)));
}
fn extend_line_below(cx: &mut Context) {
    let n = cx.count();
    motion(cx, |t, r| mv::extend_line_below(t, r, n))
}
fn extend_line_above(cx: &mut Context) {
    let n = cx.count();
    motion(cx, |t, r| mv::extend_line_above(t, r, n))
}
fn extend_to_line_bounds(cx: &mut Context) {
    motion(cx, mv::extend_to_line_bounds)
}

fn copy_selection_on_line(cx: &mut Context, delta: isize) {
    let n = cx.count();
    let tab = cx.editor.config.tab_width;
    let doc = cx.editor.doc_mut();
    let mut sel = doc.selection().clone();
    let mut frontier = sel.ranges().to_vec();
    for _ in 0..n {
        frontier = frontier.iter().filter_map(|&r| mv::copy_on_line(&doc.text, r, delta, tab)).collect();
        // Order the pushes so the copy farthest in the direction of motion becomes the primary.
        let ordered: Vec<Range> =
            if delta > 0 { frontier.clone() } else { frontier.iter().rev().copied().collect() };
        for r in ordered {
            sel = sel.push(r);
        }
    }
    doc.set_selection(sel);
}
fn copy_selection_on_next_line(cx: &mut Context) {
    copy_selection_on_line(cx, 1)
}
fn copy_selection_on_prev_line(cx: &mut Context) {
    copy_selection_on_line(cx, -1)
}

// ── Search, regex select (body in search.rs) ────────────────────────────

fn search(cx: &mut Context) {
    cx.editor.open_prompt(PromptKind::Search { reverse: false }, "");
}
fn rsearch(cx: &mut Context) {
    cx.editor.open_prompt(PromptKind::Search { reverse: true }, "");
}
fn search_next(cx: &mut Context) {
    search::search_next(cx, false)
}
fn search_prev(cx: &mut Context) {
    search::search_next(cx, true)
}
fn search_selection(cx: &mut Context) {
    search::search_selection(cx)
}
fn select_regex(cx: &mut Context) {
    cx.editor.open_prompt(PromptKind::SelectRegex, "");
}
fn split_selection(cx: &mut Context) {
    cx.editor.open_prompt(PromptKind::Split, "");
}
fn split_selection_on_newline(cx: &mut Context) {
    search::split_selection_on_newline(cx)
}
fn keep_selections(cx: &mut Context) {
    cx.editor.open_prompt(PromptKind::Keep { remove: false }, "");
}
fn remove_selections(cx: &mut Context) {
    cx.editor.open_prompt(PromptKind::Keep { remove: true }, "");
}

// ── LSP (body in lsp_editor.rs) ──────────────────────────────────────────

fn code_action(cx: &mut Context) {
    cx.editor.code_action()
}
/// `space r` — opens an input prefilled with the word under the cursor.
fn rename_symbol(cx: &mut Context) {
    let doc = cx.editor.doc();
    let r = to::word_object(&doc.text, doc.selection().primary(), to::Kind::Inside, false);
    let word = doc.text.byte_slice(r.from()..r.to()).to_string();
    cx.editor.open_prompt(PromptKind::Rename, word.trim());
}
fn goto_definition(cx: &mut Context) {
    cx.editor.goto_location(crate::lsp_editor::Goto::Definition)
}
fn goto_declaration(cx: &mut Context) {
    cx.editor.goto_location(crate::lsp_editor::Goto::Declaration)
}
fn goto_type_definition(cx: &mut Context) {
    cx.editor.goto_location(crate::lsp_editor::Goto::TypeDefinition)
}
fn goto_implementation(cx: &mut Context) {
    cx.editor.goto_location(crate::lsp_editor::Goto::Implementation)
}
fn goto_reference(cx: &mut Context) {
    cx.editor.goto_location(crate::lsp_editor::Goto::References)
}

// ── Jump list (body in jumplist.rs) ──────────────────────────────────────

fn jump_backward(cx: &mut Context) {
    let n = cx.count();
    cx.editor.jump_backward(n)
}
fn jump_forward(cx: &mut Context) {
    let n = cx.count();
    cx.editor.jump_forward(n)
}
fn save_selection(cx: &mut Context) {
    cx.editor.push_jump();
    cx.editor.note("saved to the jump list (C-o returns here)");
}
fn jumplist_picker(cx: &mut Context) {
    cx.editor.jumplist_picker()
}
fn last_picker(cx: &mut Context) {
    cx.editor.reopen_last_picker()
}
fn goto_last_accessed_file(cx: &mut Context) {
    cx.editor.goto_last_accessed()
}
fn completion(cx: &mut Context) {
    cx.editor.completion_request();
}

fn hover(cx: &mut Context) {
    cx.editor.lsp_request(crate::lsp_editor::Kind::Hover, "textDocument/hover", serde_json::json!({}));
}
/// To the next/previous git hunk — selects its lines (for a deletion, the line start at that spot).
fn goto_change(cx: &mut Context, forward: bool) {
    let doc = cx.editor.doc();
    let text = &doc.text;
    let line = crate::movement::line_of(text, doc.selection().primary().cursor(text));
    let hunks = &doc.git_hunks;
    let target = if forward {
        hunks.iter().find(|h| h.lines.start > line)
    } else {
        hunks.iter().rev().find(|h| h.lines.start < line)
    };
    let Some(h) = target.cloned() else {
        return cx.editor.note(if hunks.is_empty() { "no git changes" } else { "no more changes" });
    };
    let last = crate::movement::last_line(text);
    let from = crate::movement::line_start(text, h.lines.start.min(last));
    let to = if h.lines.is_empty() {
        from
    } else {
        crate::movement::line_full_end(text, (h.lines.end - 1).min(last))
    };
    let sel = if to > from {
        crate::selection::Selection::single(crate::selection::Range::new(from, to))
    } else {
        crate::selection::Selection::point(from)
    };
    cx.editor.doc_mut().set_selection(sel);
}

fn goto_next_change(cx: &mut Context) {
    goto_change(cx, true);
}

fn goto_prev_change(cx: &mut Context) {
    goto_change(cx, false);
}

fn goto_next_diag(cx: &mut Context) {
    cx.editor.goto_diagnostic(true)
}
fn goto_prev_diag(cx: &mut Context) {
    cx.editor.goto_diagnostic(false)
}
fn diagnostics_picker(cx: &mut Context) {
    cx.editor.diagnostics_picker()
}

// ── Pickers ──────────────────────────────────────────────────────────────

fn file_picker(cx: &mut Context) {
    let root = std::env::current_dir().unwrap_or_default();
    let title = format!("files in {}", root.file_name().map(|n| n.to_string_lossy()).unwrap_or_default());
    cx.editor.open_picker(
        crate::picker::Picker::new(title, Vec::new(), true),
        Some(Box::new(move || Ok(crate::picker::file_items(&root)))),
    );
}

/// `:` commands to list in the palette (description, command line).
const TYPED_IN_PALETTE: &[(&str, &str)] = &[
    ("Learn the keys (tutorial)", "tutor"),
    ("Save file", "w"),
    ("Quit", "q"),
    ("Save and quit", "wq"),
    ("Quit without saving", "q!"),
    ("Format file", "format"),
    ("New chat with Claude", "chat-new"),
    ("Cancel Claude request", "ask-cancel"),
    ("Open config file", "config-open"),
    ("Reload config", "config-reload"),
    ("Show current config", "config-show"),
    ("Toggle inlay hints", "toggle editor.inlay-hints"),
    ("Toggle top bar", "toggle editor.header"),
    ("Toggle current line highlight", "toggle editor.cursorline"),
    ("Close buffer", "bc"),
    ("Next buffer", "bn"),
    ("Previous buffer", "bp"),
];

/// Command palette (`space ?`): find every command by its description, bound keys on the right —
/// so everything is usable without knowing the keys.
fn command_palette(cx: &mut Context) {
    use crate::picker::{Action, Item};
    let keys = cx.editor.keymaps.bindings();
    let mut items: Vec<Item> = COMMANDS
        .iter()
        .filter(|c| c.name != "command_palette")
        .map(|c| Item {
            label: c.doc.trim_end_matches(" (LSP)").to_string(),
            action: Action::Command(c.name),
            hint: keys.get(c.name).cloned().unwrap_or_default(),
            glyph: None,
        })
        .collect();
    items.extend(TYPED_IN_PALETTE.iter().map(|(doc, line)| Item {
        label: doc.to_string(),
        action: Action::Typed(line.to_string()),
        hint: format!(":{line}"),
        glyph: None,
    }));
    // Bound commands first (the frequently used ones), then by name
    items.sort_by(|a, b| (a.hint.is_empty(), &a.label).cmp(&(b.hint.is_empty(), &b.label)));
    cx.editor.open_picker(crate::picker::Picker::new("commands", items, false).without_preview(), None);
}

/// Theme picker: small window at top center — the code behind previews the highlighted theme live
/// (Esc = revert, Enter = save).
fn theme_picker(cx: &mut Context) {
    // User themes are files — list them on a worker thread, open the picker when they arrive
    let current = cx.editor.theme.name.clone();
    cx.editor.events.jobs().spawn(move || {
        let names = crate::theme::available();
        move |ed: &mut Editor| {
            // Something else took the keys meanwhile — don't cover it
            if ed.picker.is_none() && ed.prompt.is_none() {
                open_theme_picker(ed, &names, &current);
            }
        }
    });
}

fn open_theme_picker(ed: &mut Editor, names: &[String], current: &str) {
    use crate::picker::{Action, Item};
    let items: Vec<Item> = names
        .iter()
        .map(|n| Item {
            label: n.clone(),
            action: Action::Theme(n.clone()),
            hint: {
                let korean = crate::theme::BUILTIN.iter().find(|(b, _)| *b == n.as_str()).map(|(_, k)| *k);
                match (korean, *n == current) {
                    (Some(k), true) => format!("{k}  · current"),
                    (Some(k), false) => k.to_string(),
                    (None, true) => "current".into(),
                    (None, false) => String::new(),
                }
            },
            glyph: None,
        })
        .collect();
    let mut p = crate::picker::Picker::new("themes", items, false).without_preview();
    p.compact = true;
    p.selected = names.iter().position(|n| n == current).unwrap_or(0);
    ed.open_picker(p, None);
}

fn toggle_breakpoint(cx: &mut Context) {
    cx.editor.toggle_breakpoint();
}

fn dap_attach(cx: &mut Context) {
    cx.editor.attach_picker();
}

fn test_nearest(cx: &mut Context) {
    cx.editor.test_run(crate::testing::Scope::Nearest);
}

fn test_debug(cx: &mut Context) {
    cx.editor.test_debug();
}

fn test_file(cx: &mut Context) {
    cx.editor.test_run(crate::testing::Scope::File);
}

fn test_last(cx: &mut Context) {
    cx.editor.test_rerun();
}

fn goto_next_test_failure(cx: &mut Context) {
    cx.editor.test_failure_step(true);
}

fn goto_prev_test_failure(cx: &mut Context) {
    cx.editor.test_failure_step(false);
}

fn test_close(cx: &mut Context) {
    cx.editor.test_close();
}

fn dap_launch(cx: &mut Context) {
    cx.editor.dap_launch();
}

fn dap_next(cx: &mut Context) {
    cx.editor.dap_step("next");
}

fn dap_step_in(cx: &mut Context) {
    cx.editor.dap_step("stepIn");
}

fn dap_step_out(cx: &mut Context) {
    cx.editor.dap_step("stepOut");
}

fn dap_pause(cx: &mut Context) {
    cx.editor.dap_pause();
}

fn claude_code(cx: &mut Context) {
    cx.editor.agent_open_claude();
}
fn claude_code_mention(cx: &mut Context) {
    cx.editor.agent_mention();
}
fn dap_edit_condition(cx: &mut Context) {
    let now = cx.editor.breakpoint_field(false).unwrap_or_default();
    cx.editor.open_prompt(PromptKind::BreakCondition, &now);
}
fn dap_edit_log(cx: &mut Context) {
    let now = cx.editor.breakpoint_field(true).unwrap_or_default();
    cx.editor.open_prompt(PromptKind::LogMessage, &now);
}
/// Watch expression — prefilled with the selection (one line) if any, else the word under the cursor.
fn dap_watch(cx: &mut Context) {
    let doc = cx.editor.doc();
    let r = doc.selection().primary();
    let picked = doc.text.byte_slice(r.from()..r.to()).to_string();
    let text = if crate::graphemes::next_boundary(&doc.text, r.from()) < r.to() && !picked.contains('\n') {
        picked
    } else {
        let w = to::word_object(&doc.text, r, to::Kind::Inside, false);
        doc.text.byte_slice(w.from()..w.to()).to_string()
    };
    cx.editor.open_prompt(PromptKind::Watch, text.trim());
}
/// Remove a watch expression — directly if there's one, pick from a list if several.
fn dap_unwatch(cx: &mut Context) {
    match cx.editor.watches.as_slice() {
        [] => cx.editor.note("no watches"),
        [one] => {
            let one = one.clone();
            cx.editor.remove_watch(&one);
            cx.editor.note(format!("Stopped watching {one}"));
        }
        many => {
            let mut items: Vec<crate::picker::Item> = many
                .iter()
                .map(|w| crate::picker::Item {
                    label: w.clone(),
                    action: crate::picker::Action::Typed(format!("unwatch {w}")),
                    hint: String::new(),
                    glyph: Some(("◦", "ui.text")),
                })
                .collect();
            items.push(crate::picker::Item {
                label: "all watches".into(),
                action: crate::picker::Action::Typed("unwatch".into()),
                hint: String::new(),
                glyph: None,
            });
            let picker = crate::picker::Picker::new("remove watch", items, false).without_preview();
            cx.editor.open_picker(picker, None);
        }
    }
}
fn dap_terminate(cx: &mut Context) {
    cx.editor.dap_terminate();
}

fn vsplit(cx: &mut Context) {
    cx.editor.split_view(crate::split::Dir::Vertical);
}

fn hsplit(cx: &mut Context) {
    cx.editor.split_view(crate::split::Dir::Horizontal);
}

fn rotate_view(cx: &mut Context) {
    cx.editor.cycle_view(1);
}

fn jump_view_left(cx: &mut Context) {
    cx.editor.focus_dir(-1, 0);
}

fn jump_view_right(cx: &mut Context) {
    cx.editor.focus_dir(1, 0);
}

fn jump_view_up(cx: &mut Context) {
    cx.editor.focus_dir(0, -1);
}

fn jump_view_down(cx: &mut Context) {
    cx.editor.focus_dir(0, 1);
}

fn wclose(cx: &mut Context) {
    if !cx.editor.close_view() {
        cx.editor.note("only one window");
    }
}

fn wonly(cx: &mut Context) {
    cx.editor.only_view();
}

fn symbol_picker(cx: &mut Context) {
    cx.editor.document_symbols();
}

fn workspace_symbol_picker(cx: &mut Context) {
    cx.editor.workspace_symbols();
}

fn buffer_picker(cx: &mut Context) {
    let items = cx
        .editor
        .docs
        .iter()
        .map(|d| crate::picker::Item {
            label: format!("{}{}", d.display_name(), if d.is_modified() { " [+]" } else { "" }),
            action: crate::picker::Action::Buffer(d.id),
            hint: String::new(),
            glyph: None,
        })
        .collect();
    cx.editor.open_picker(crate::picker::Picker::new("buffers", items, true), None);
}

fn global_search(cx: &mut Context) {
    cx.editor.open_prompt(PromptKind::GlobalSearch, "");
}

// ── LLM ──────────────────────────────────────────────────────────────────

fn chat_open(cx: &mut Context) {
    crate::chat::open(cx.editor);
}

fn chat_close(cx: &mut Context) {
    crate::chat::close(cx.editor);
}

fn llm_ask(cx: &mut Context) {
    cx.editor.open_prompt(PromptKind::Command, "ask ");
    crate::llm::prewarm(cx.editor);
}

// ── Buffers ──────────────────────────────────────────────────────────────

fn goto_next_buffer(cx: &mut Context) {
    let n = cx.count();
    cx.editor.cycle_buffer(n as isize)
}
fn goto_previous_buffer(cx: &mut Context) {
    let n = cx.count();
    cx.editor.cycle_buffer(-(n as isize))
}

// ── Insert mode ──────────────────────────────────────────────────────────

pub fn insert_char(cx: &mut Context, c: char) {
    use crate::pairs::Typed;
    let pairs = cx.editor.pairs();
    let doc = cx.editor.doc();
    let items = doc
        .selection()
        .ranges()
        .iter()
        .map(|r| {
            let text = match crate::pairs::typed(&doc.text, r.head, c, pairs) {
                Typed::Pair(close) => format!("{c}{close}"),
                Typed::Skip => String::new(),
                Typed::Plain => c.to_string(),
            };
            (r.head, text, c.len_utf8())
        })
        .collect();
    insert_at(cx, items);
}

fn insert_newline(cx: &mut Context) {
    let doc = cx.editor.doc();
    let items = doc
        .selection()
        .ranges()
        .iter()
        .map(|r| {
            // Auto-indent: inherit the current line's indentation (only up to the cursor).
            let line = mv::line_of(&doc.text, r.head);
            let ws_end = mv::first_non_whitespace(&doc.text, line).min(r.head);
            let indent = doc.text.byte_slice(mv::line_start(&doc.text, line)..ws_end).to_string();
            let text = format!("\n{indent}");
            let off = text.len();
            (r.head, text, off)
        })
        .collect();
    insert_at(cx, items);
}

fn insert_tab(cx: &mut Context) {
    let unit = " ".repeat(cx.editor.config.tab_width);
    let n = unit.len();
    let items = cx.editor.doc().selection().ranges().iter().map(|r| (r.head, unit.clone(), n)).collect();
    insert_at(cx, items);
}

fn delete_char_backward(cx: &mut Context) {
    let pairs = cx.editor.pairs();
    delete_around(cx, |t, head| {
        // Inside an empty pair `(|)` — the closer goes too
        let to = if crate::pairs::inside_pair(t, head, pairs) {
            crate::graphemes::next_char(t, head)
        } else {
            head
        };
        (crate::graphemes::prev_boundary(t, head), to)
    });
}

/// Insert mode: per cursor, delete the span `f(text, head)` gives (nothing if empty).
fn delete_around(cx: &mut Context, f: impl Fn(&ropey::Rope, usize) -> (usize, usize)) {
    let doc = cx.editor.doc_mut();
    let tx = Transaction::change_by_selection(doc.selection(), |r| {
        let (from, to) = f(&doc.text, r.head);
        (from < to).then(|| Change::delete(from, to))
    });
    doc.apply(&tx);
}

/// `C-w` — back over blanks, then a run of the same kind (word chars or punctuation). At a line start, the
/// line break.
fn delete_word_backward(cx: &mut Context) {
    use crate::graphemes::prev_boundary;
    use mv::CharClass::{Eol, Whitespace};
    delete_around(cx, |t, head| {
        let mut p = head;
        while p > 0 && mv::class_at(t, prev_boundary(t, p)) == Whitespace {
            p = prev_boundary(t, p);
        }
        if p == 0 {
            return (0, head);
        }
        let class = mv::class_at(t, prev_boundary(t, p));
        if class == Eol {
            return (prev_boundary(t, p), head);
        }
        while p > 0 && mv::class_at(t, prev_boundary(t, p)) == class {
            p = prev_boundary(t, p);
        }
        (p, head)
    });
}

/// `A-d` — over blanks, then a run of the same kind. At a line end, the line break.
fn delete_word_forward(cx: &mut Context) {
    use crate::graphemes::next_boundary;
    use mv::CharClass::{Eol, Whitespace};
    delete_around(cx, |t, head| {
        let len = t.len_bytes();
        let mut p = head;
        while p < len && mv::class_at(t, p) == Whitespace {
            p = next_boundary(t, p);
        }
        if p == len {
            return (head, len);
        }
        let class = mv::class_at(t, p);
        if class == Eol {
            return (head, next_boundary(t, p));
        }
        while p < len && mv::class_at(t, p) == class {
            p = next_boundary(t, p);
        }
        (head, p)
    });
}

/// `C-u` — back to the indentation (then to the line start; at the line start, joins the line above).
fn kill_to_line_start(cx: &mut Context) {
    delete_around(cx, |t, head| {
        let line = mv::line_of(t, head);
        let (start, indent) = (mv::line_start(t, line), mv::first_non_whitespace(t, line));
        let from = if head == start && line > 0 {
            mv::line_end(t, line - 1)
        } else if indent < head {
            indent
        } else {
            start
        };
        (from, head)
    });
}

/// `C-k` — to the line end (at the line end, joins the line below).
fn kill_to_line_end(cx: &mut Context) {
    delete_around(cx, |t, head| {
        let line = mv::line_of(t, head);
        let end = mv::line_end(t, line);
        (head, if head == end { mv::line_full_end(t, line) } else { end })
    });
}

/// `C-r x` — type register x's contents at each cursor.
fn insert_register(cx: &mut Context) {
    cx.editor.on_next_char = Some((
        |cx, name| match name {
            '+' => crate::clipboard::paste(&cx.editor.events.jobs(), |ed, s| {
                ed.with_group(|cx| insert_texts(cx, std::slice::from_ref(&s)))
            }),
            _ => match cx.editor.registers.get(&name).cloned() {
                Some(values) if !values.is_empty() => insert_texts(cx, &values),
                _ => cx.editor.set_error(format!("register {name} is empty")),
            },
        },
        cx.count,
    ));
}

/// Each cursor gets its own value if there's one per cursor, else the last one.
fn insert_texts(cx: &mut Context, values: &[String]) {
    let ranges = cx.editor.doc().selection().ranges().to_vec();
    let items = ranges
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let s = values[i.min(values.len() - 1)].clone();
            let n = s.len();
            (r.head, s, n)
        })
        .collect();
    insert_at(cx, items);
}

fn commit_undo_checkpoint(cx: &mut Context) {
    cx.editor.commit_undo_checkpoint();
}

fn repeat_last_insert(cx: &mut Context) {
    let n = cx.count();
    cx.editor.repeat_last_insert(n);
}

/// `C-c` — line comments over the selections' lines (block comments where a language has only those).
fn shell_pipe(cx: &mut Context) {
    cx.editor.open_prompt(PromptKind::Shell(crate::shell::Pipe::Replace), "");
}
fn shell_pipe_to(cx: &mut Context) {
    cx.editor.open_prompt(PromptKind::Shell(crate::shell::Pipe::To), "");
}
fn shell_insert_output(cx: &mut Context) {
    cx.editor.open_prompt(PromptKind::Shell(crate::shell::Pipe::Insert), "");
}
fn shell_append_output(cx: &mut Context) {
    cx.editor.open_prompt(PromptKind::Shell(crate::shell::Pipe::Append), "");
}
fn shell_keep_pipe(cx: &mut Context) {
    cx.editor.open_prompt(PromptKind::Shell(crate::shell::Pipe::Keep), "");
}

fn toggle_comments(cx: &mut Context) {
    let Some(spec) = cx.editor.lang_spec() else {
        return cx.editor.set_status("no comment syntax for this file");
    };
    let block = spec.block_comment.as_ref().map(|(a, b)| (a.as_str(), b.as_str()));
    let doc = cx.editor.doc_mut();
    let Some(tx) = crate::comment::toggle(&doc.text, doc.selection(), spec.comment_token.as_deref(), block)
    else {
        if spec.comment_token.is_none() && block.is_none() {
            cx.editor.set_status(format!("{} has no comments", spec.name));
        }
        return;
    };
    // A cursor stays on its char; a wider selection keeps covering the comment markers it spans
    let text = &doc.text;
    let sel = doc.selection().transform(|r| {
        if r.to() <= crate::graphemes::next_boundary(text, r.from()) {
            Range::new(tx.map_pos(r.anchor, Assoc::After), tx.map_pos(r.head, Assoc::After))
        } else if r.is_forward() {
            Range::new(tx.map_pos(r.anchor, Assoc::Before), tx.map_pos(r.head, Assoc::After))
        } else {
            Range::new(tx.map_pos(r.anchor, Assoc::After), tx.map_pos(r.head, Assoc::Before))
        }
    });
    doc.apply_with(&tx, sel);
}

fn delete_char_forward(cx: &mut Context) {
    let doc = cx.editor.doc_mut();
    let len = doc.text.len_bytes();
    let tx = Transaction::change_by_selection(doc.selection(), |r| {
        (r.head < len).then(|| Change::delete(r.head, crate::graphemes::next_boundary(&doc.text, r.head)))
    });
    doc.apply(&tx);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_unique() {
        let mut names: Vec<_> = COMMANDS.iter().map(|c| c.name).collect();
        names.sort();
        let before = names.len();
        names.dedup();
        assert_eq!(before, names.len());
    }
}
