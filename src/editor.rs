//! Editor state + key dispatch. Knows nothing of the terminal — so it can be tested whole via key sequences.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use anyhow::Result;

use crate::commands::{self, Context};
use crate::config::{Config, EditorConfig};
use crate::document::{DocId, Document, Snapshot};
use crate::event::{Event, Queue};
use crate::key::{Code, Key};
use crate::keymap::{Keymaps, Lookup, MappableCommand};
use crate::llm::{self, Review, Warm};
use crate::lsp_editor::LspState;
use crate::movement as mv;
use crate::picker::{Action, Picker};
use crate::search;
use crate::selection::{Range, Selection};
use crate::syntax;
use crate::theme::Theme;
use crate::typed;

/// Files larger than this are read in the background. Small files take only a few ms synchronously,
/// so async would only add flicker.
const ASYNC_LOAD_BYTES: u64 = 1 << 20;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Normal,
    Insert,
    Select,
}

/// The last motion `A-.` repeats (a closure capturing the count, target char, etc.).
pub type Motion = Rc<dyn Fn(&mut Context)>;
/// A command that takes one character from the next key and runs.
pub type CharFn = fn(&mut Context, char);

/// Kind of single-line command-line input — same input box, only what Enter does differs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PromptKind {
    Command,
    Search {
        reverse: bool,
    },
    SelectRegex,
    Split,
    Keep {
        remove: bool,
    },
    GlobalSearch,
    Rename,
    /// Debugger: breakpoint condition / log message / watch expression.
    BreakCondition,
    LogMessage,
    Watch,
    /// `|` `A-|` `!` `A-!` `$` — a shell command for the selections (shell.rs).
    Shell(crate::shell::Pipe),
    /// `C-r` in the global search results — the replacement text.
    Replace,
    /// `space n`: a reply to note N, or (None) a question starting a note on this line.
    Note(Option<u64>),
}

impl PromptKind {
    pub fn label(&self) -> &'static str {
        match self {
            PromptKind::Command => ":",
            PromptKind::Search { reverse: false } => "/",
            PromptKind::Search { reverse: true } => "?",
            PromptKind::SelectRegex => "select:",
            PromptKind::Split => "split:",
            PromptKind::Keep { remove: false } => "keep:",
            PromptKind::Keep { remove: true } => "remove:",
            PromptKind::GlobalSearch => "grep:",
            PromptKind::Rename => "rename:",
            PromptKind::BreakCondition => "break when:",
            PromptKind::LogMessage => "log:",
            PromptKind::Watch => "watch:",
            PromptKind::Shell(p) => p.label(),
            PromptKind::Replace => "replace with:",
            PromptKind::Note(Some(_)) => "reply:",
            PromptKind::Note(None) => "ask claude:",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Prompt {
    pub kind: PromptKind,
    pub text: String,
    /// Cycling `:` completions with Tab: (input when cycling began, current candidate index). Typing ends it.
    pub cycle: Option<(String, usize)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    Info,
    Error,
}

/// A notice that pops up briefly at the top right (save, reload, claude answer …).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToastKind {
    Info,
    Success,
    Warning,
    Error,
}

#[derive(Clone, Debug)]
pub struct Toast {
    pub text: String,
    pub kind: ToastKind,
    pub at: std::time::Instant,
}

impl Toast {
    /// How long it stays up — longer for errors.
    fn life(&self) -> std::time::Duration {
        std::time::Duration::from_millis(match self.kind {
            ToastKind::Success => 2500,
            ToastKind::Info => 3500,
            ToastKind::Warning => 6000,
            ToastKind::Error => 7000,
        })
    }

    pub fn alive(&self) -> bool {
        self.at.elapsed() < self.life()
    }

    /// Opacity 0..1 — fades in over the first 150 ms, fades out over the last 400 ms.
    pub fn alpha(&self) -> f32 {
        let (t, life) = (self.at.elapsed().as_secs_f32(), self.life().as_secs_f32());
        (t / 0.15).min((life - t) / 0.4).clamp(0.0, 1.0)
    }
}

/// Screen layout of the last frame (term.rs fills it on every draw).
#[derive(Clone, Debug, Default)]
pub struct Screen {
    /// Per pane: (view, pane rect, line-number gutter width, per screen row `ViewRow::origin`).
    #[allow(clippy::type_complexity)]
    pub panes: Vec<(crate::split::ViewId, crate::split::Rect, usize, Vec<(usize, usize, usize, usize)>)>,
    /// Column where the chat pane starts.
    pub chat_x: Option<usize>,
    /// First row and row count of the picker list.
    pub picker_list: Option<(usize, usize)>,
    /// Grammar download offer buttons: (row, start col, end col, key to feed when clicked).
    pub offer_buttons: Vec<(usize, usize, usize, char)>,
    /// Column where the picker's preview card starts (the wheel scrolls it there).
    pub picker_preview_x: Option<usize>,
    /// The floating docs card this frame (x, y, width, height) — drawing fills it in; the wheel scrolls it.
    pub popup: std::cell::Cell<Option<(usize, usize, usize, usize)>>,
    /// Clickable spots in the chat pane: (row, start col, end col, where it points) — drawing fills them in.
    pub chat_links: std::cell::RefCell<Vec<(usize, usize, usize, crate::chat::Link)>>,
    /// The minimized chat card (x, y, width, height) — a click on it brings the panel back.
    pub chat_mini: std::cell::Cell<Option<(usize, usize, usize, usize)>>,
    /// The note card and the thought card this frame (x, y, width, height).
    pub note_card: std::cell::Cell<Option<(usize, usize, usize, usize)>>,
    pub thought_card: std::cell::Cell<Option<(usize, usize, usize, usize)>>,
}

/// One pane of a split: what it shows (a document) + its scroll. Cursor and selection are the document's.
#[derive(Clone, Debug)]
pub struct View {
    pub id: crate::split::ViewId,
    pub doc: DocId,
    pub top: usize,
    pub left: usize,
    /// Spots to come back to (`C-o`/`C-i`) · the document shown before this one (`ga`).
    pub jumps: crate::jumplist::JumpList,
    pub last_doc: Option<DocId>,
}

impl View {
    pub fn new(id: crate::split::ViewId, doc: DocId) -> Self {
        Self { id, doc, top: 0, left: 0, jumps: Default::default(), last_doc: None }
    }
}

pub struct Editor {
    pub docs: Vec<Document>,
    pub current: usize,
    /// Panes, the split tree and the focused pane. The focused pane's scroll is its document's top/left
    /// (synced every event).
    pub views: Vec<View>,
    pub split: crate::split::Split,
    pub focus: crate::split::ViewId,
    next_view: crate::split::ViewId,
    pub mode: Mode,
    pub keymaps: Keymaps,
    pub config: EditorConfig,
    pub theme: Theme,
    /// Registers — name → per-range text. `"` is the default, `_` discards, `+` is the system clipboard.
    pub registers: HashMap<char, Vec<String>>,
    /// Register picked with `"x` for the next command.
    pub selected_register: Option<char>,
    /// Command awaiting one character from the next key (`f t F T r "`) + its count.
    pub on_next_char: Option<(CharFn, Option<usize>)>,
    /// First char of a two-char command (`mr x y`).
    pub char_arg: Option<char>,
    /// Recording a macro (register, keys). Replay = feeding the keys in again.
    pub recording: Option<(char, Vec<Key>)>,
    pub macros: HashMap<char, Vec<Key>>,
    /// Registers of the macros being replayed (a macro may replay another one, never itself).
    pub replaying: Vec<char>,
    /// `.`: the insert session being recorded · the last finished one · replaying it now (repeat.rs).
    pub(crate) insert_rec: Option<crate::repeat::LastInsert>,
    pub(crate) last_insert: Option<crate::repeat::LastInsert>,
    pub(crate) repeating_insert: bool,
    /// `A-o` steps (doc, version, selection before, after) — `A-i` walks back while nothing else changed.
    pub(crate) expand_history: Vec<(DocId, u64, Selection, Selection)>,
    /// Replace across files: (pattern, listed matches per file) waiting for the replacement · the plan
    /// shown in the preview picker.
    pub(crate) replace_targets: Option<(String, crate::replace::Targets)>,
    pub(crate) replace_plan: Vec<crate::replace::FilePlan>,
    /// `space g f`: per file, its path, first changed line and diff against HEAD.
    pub(crate) changed_files: Vec<crate::gitmenu::Changed>,
    /// `space g b` blame (gitmenu.rs).
    pub blame: crate::gitmenu::BlameState,
    /// `gw` labels on screen (labels.rs) — the next two keys pick one.
    pub jump_labels: Option<crate::labels::Labels>,
    /// Length of the key sequence behind the last command — to drop `Q` itself when recording stops.
    pub last_trigger_len: usize,
    /// Some while entering a command line.
    pub prompt: Option<Prompt>,
    /// Last search term (`n`/`N`, `*`).
    pub search: Option<String>,
    /// Whether to highlight all matches of the last search (on after a search, off with Esc in normal mode).
    pub search_hl: bool,
    pub search_hits: Option<search::SearchHits>,
    /// Position before the `/` preview moved the view (Esc returns there).
    pub search_origin: Option<usize>,
    pub status: Option<(String, Severity)>,
    pub should_quit: bool,
    /// Text area (rows, cols) — the terminal updates it every frame. Used by page motions.
    pub viewport: (usize, usize),
    pub last_motion: Option<Motion>,
    /// Visual column held across consecutive vertical moves (per range). Dropped on a non-vertical command.
    pub sticky: Option<Vec<usize>>,
    pub sticky_used: bool,
    /// Queue for keys and job results. Slow work goes through `events.jobs().spawn(..)`.
    pub events: Queue,
    /// Language servers and sent requests.
    pub lsp: LspState,
    /// Text floating near the cursor (hover). Any key closes it.
    pub popup: Option<Vec<crate::markdown::Line>>,
    /// How many lines the floating text is scrolled down (C-d / C-u).
    pub popup_scroll: usize,
    /// Insert-mode completion list.
    pub completion: Option<crate::completion::Completion>,
    /// Function signature floating above the cursor (on `(`·`,` in insert mode).
    pub signature: Option<crate::signature::Signature>,
    /// Current git branch (filled by the watcher thread — for the status line).
    pub git_branch: Option<String>,
    /// Open picker (files, buffers, search results).
    pub picker: Option<Picker>,
    /// The last picker closed (`space '`).
    last_picker: Option<Picker>,
    picker_ids: u64,
    /// In-flight LLM request/review (one at a time).
    pub review: Option<Review>,
    pub llm_warm: Warm,
    pub llm_generation: u64,
    /// The ask process awaiting an answer (killed on cancel).
    pub llm_pid: Option<u32>,
    /// Chat pane on the right (if open).
    pub chat: Option<crate::chat::Chat>,
    /// Claude's notes pinned to code (`notes.rs`).
    pub notes: crate::notes::Notes,
    /// Debug session · breakpoints (kept without a session) · session generation (drops late messages).
    pub dap: Option<crate::dap::Dap>,
    pub breakpoints: crate::dap::Breakpoints,
    /// Claude Code IDE server (if running).
    pub agent: Option<crate::agent::Agent>,
    /// Lists used by `:` completion (folders, themes — filled by a worker thread) · folders being read.
    pub cmd_lists: crate::cmdline::Lists,
    /// Grammars already offered for download (this session).
    pub grammar_hinted: std::collections::HashSet<String>,
    /// Offers asking to download (arrival order — the front one gets keys, offer.rs).
    pub offers: Vec<crate::offer::Offer>,
    pub offer_seq: u64,
    /// Whether to show offers (off in tests — so they don't swallow y/n keys).
    pub offer_popups: bool,
    /// Diagnostic card closed with Esc on this line (doc, line) — back once the cursor leaves the line.
    pub diag_card_hidden: Option<(DocId, usize)>,
    cmd_loading: std::collections::HashSet<std::path::PathBuf>,
    /// Debugger watch expressions (evaluated at every stop — survive session changes).
    pub watches: Vec<String>,
    pub dap_generation: u64,
    /// Test pane (running or last result) · what ran last (was it debug) · generation — testing.rs.
    pub test_run: Option<crate::testing::TestRun>,
    pub test_last: Option<(crate::testing::Target, bool)>,
    pub test_generation: u64,
    /// If the current debug session is a test, that test (after it ends, F5 = rerun it).
    pub dap_test: Option<crate::testing::Target>,
    /// Preparing a Java debug session (asking jdtls for the main class and classpath) — java.rs.
    pub java_debug: Option<crate::java::DebugPrep>,
    /// Screen layout of the last frame — for mapping mouse coordinates to text positions.
    pub screen: Screen,
    /// Screen rows of the last frame (folded doc comments make screen row ≠ doc line — used by the mouse).
    pub view: Vec<crate::doccomment::ViewRow>,
    /// Where a drag started (selecting with the mouse).
    mouse_anchor: Option<usize>,
    /// Recently opened files (start screen — open with number keys).
    pub recent: Vec<PathBuf>,
    /// Session file (last position per file, open files per folder).
    pub session: crate::session::State,
    /// Theme before the theme preview (Esc restores it).
    theme_before: Option<crate::theme::Theme>,
    pub toasts: Vec<Toast>,
    /// Disk watch is set up · generation for save-on-idle.
    pub disk_polling: bool,
    /// A git diff is running on a worker thread.
    pub git_diffing: bool,
    /// Whether to read HEAD contents when opening a file (off in tests — keeps event order stable;
    /// turn on when needed).
    pub git_auto: bool,
    pub idle_save_gen: u64,
    /// A tick for the waiting animation is scheduled.
    ticking: bool,
    /// Values `:set` at runtime — they win even when the config file is reloaded (the top layer).
    pub overrides: Vec<(String, toml::Value)>,
    pub pending: Vec<Key>,
    count: Option<usize>,
    /// Starting snapshot of the open edit group. Stays open throughout insert mode.
    undo_group: Option<(DocId, Snapshot)>,
    next_id: DocId,
}

impl Editor {
    pub fn new(config: Config) -> Self {
        let mut ed = Self {
            docs: Vec::new(),
            current: 0,
            views: Vec::new(),
            split: crate::split::Split::Leaf(1),
            focus: 1,
            next_view: 2,
            mode: Mode::Normal,
            keymaps: config.keymaps,
            config: config.editor,
            theme: config.theme,
            registers: HashMap::new(),
            selected_register: None,
            on_next_char: None,
            char_arg: None,
            recording: None,
            macros: HashMap::new(),
            replaying: Vec::new(),
            last_trigger_len: 0,
            prompt: None,
            search: None,
            search_hl: false,
            search_hits: None,
            search_origin: None,
            status: None,
            should_quit: false,
            viewport: (24, 80),
            last_motion: None,
            sticky: None,
            sticky_used: false,
            events: Queue::default(),
            review: None,
            picker: None,
            last_picker: None,
            picker_ids: 0,
            git_branch: None,
            lsp: LspState::default(),
            popup: None,
            popup_scroll: 0,
            completion: None,
            signature: None,
            llm_warm: Warm::default(),
            llm_generation: 0,
            llm_pid: None,
            chat: None,
            notes: Default::default(),
            dap: None,
            breakpoints: Default::default(),
            agent: None,
            cmd_lists: Default::default(),
            grammar_hinted: Default::default(),
            offers: Vec::new(),
            offer_seq: 0,
            offer_popups: !cfg!(test),
            diag_card_hidden: None,
            cmd_loading: Default::default(),
            watches: Vec::new(),
            dap_generation: 0,
            java_debug: None,
            test_run: None,
            test_last: None,
            test_generation: 0,
            dap_test: None,
            ticking: false,
            toasts: Vec::new(),
            disk_polling: false,
            git_diffing: false,
            git_auto: !cfg!(test),
            idle_save_gen: 0,
            recent: Vec::new(),
            session: crate::session::State::default(),
            theme_before: None,
            screen: Screen::default(),
            mouse_anchor: None,
            view: Vec::new(),
            overrides: Vec::new(),
            pending: Vec::new(),
            count: None,
            undo_group: None,
            insert_rec: None,
            last_insert: None,
            repeating_insert: false,
            expand_history: Vec::new(),
            jump_labels: None,
            blame: Default::default(),
            replace_targets: None,
            replace_plan: Vec::new(),
            changed_files: Vec::new(),
            next_id: 0,
        };
        ed.new_scratch();
        ed.views.push(View::new(1, ed.docs[0].id));
        ed
    }

    pub fn doc(&self) -> &Document {
        &self.docs[self.current]
    }

    pub fn doc_mut(&mut self) -> &mut Document {
        &mut self.docs[self.current]
    }

    pub(crate) fn alloc_id(&mut self) -> DocId {
        self.next_id += 1;
        self.next_id
    }

    /// If the only buffer is the empty scratch created at startup, hand over its slot.
    fn replaceable_scratch(&self) -> bool {
        self.docs.len() == 1 && self.docs[0].path.is_none() && !self.docs[0].is_modified()
    }

    /// Adds a document and makes it current — taking over the startup scratch's slot if that's all there
    /// is (panes showing the scratch then show the new document).
    fn add_doc(&mut self, doc: Document) {
        if self.replaceable_scratch() {
            let (old, new) = (self.docs[0].id, doc.id);
            for v in self.views.iter_mut().filter(|v| v.doc == old) {
                (v.doc, v.top, v.left) = (new, 0, 0);
            }
            self.docs[0] = doc;
            self.current = 0;
        } else {
            self.docs.push(doc);
            self.current = self.docs.len() - 1;
        }
    }

    /// `:tutor` — hands-on tutorial (a disposable practice buffer, markdown colors).
    pub fn open_tutor(&mut self) {
        let id = self.alloc_id();
        let mut doc = Document::from_str(id, include_str!("tutor.md"));
        doc.title = Some("tutor".into());
        doc.throwaway = true;
        self.add_doc(doc);
        if let Some(spec) = crate::syntax::spec("markdown") {
            self.load_language(id, spec);
        }
    }

    pub fn new_scratch(&mut self) {
        let id = self.alloc_id();
        self.docs.push(Document::from_str(id, ""));
        self.current = self.docs.len() - 1;
    }

    /// Opens a file. If already open, switches to that buffer.
    pub fn open(&mut self, path: &Path) -> Result<()> {
        let path = std::fs::canonicalize(path).or_else(|_| std::path::absolute(path))?;
        if let Some(i) = self.docs.iter().position(|d| d.path.as_deref() == Some(path.as_path())) {
            self.current = i;
            return Ok(());
        }
        let id = self.alloc_id();
        let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        let doc = if size < ASYNC_LOAD_BYTES {
            Document::open(id, &path)?
        } else {
            // Big files load in the background — the first frame is instant regardless of file size.
            let p = path.clone();
            self.events.jobs().spawn(move || {
                let started = std::time::Instant::now();
                // Stamp before reading — a change during the read is caught by the next watch round
                let disk = crate::disk::stamp(&p);
                let result = crate::document::read_file(&p);
                let ms = started.elapsed().as_millis();
                move |ed: &mut Editor| {
                    if let Some(d) = ed.docs.iter_mut().find(|d| d.id == id) {
                        d.disk = disk;
                    }
                    ed.finish_loading(id, result, ms)
                }
            });
            Document::placeholder(id, &path)
        };
        self.add_doc(doc);
        self.restore_position(self.current);
        self.undo_restore(id);
        self.attach_syntax(id);
        self.attach_lsp(id);
        self.git_load_base(id);
        self.recent = crate::recent::bump(&self.recent, &path);
        if !cfg!(test) {
            let list = self.recent.clone();
            self.events.jobs().spawn(move || {
                crate::recent::save(&list);
                |_: &mut Editor| {}
            });
        }
        Ok(())
    }

    fn finish_loading(&mut self, id: DocId, result: Result<ropey::Rope>, ms: u128) {
        let Some(i) = self.docs.iter().position(|d| d.id == id) else { return }; // closed while loading
        match result {
            Ok(text) => {
                let mb = text.len_bytes() as f64 / 1e6;
                self.docs[i].finish_loading(text);
                self.restore_position(i);
                self.attach_lsp(id);
                self.undo_restore(id);
                self.set_status(format!("{} loaded — {mb:.1} MB in {ms} ms", self.docs[i].display_name()));
            }
            Err(e) => {
                self.close_doc(id);
                self.set_error(format!("{e:#}"));
            }
        }
    }

    pub fn close_current(&mut self, force: bool) -> Result<(), String> {
        if !force && self.doc().is_modified() && !self.doc().throwaway {
            return Err(format!("unsaved changes in {} (use :bc! to discard)", self.doc().display_name()));
        }
        self.close_doc(self.docs[self.current].id);
        Ok(())
    }

    /// Closes a document (without asking) — notifies servers; panes showing it switch to another document.
    pub fn close_doc(&mut self, gone: DocId) {
        let Some(i) = self.docs.iter().position(|d| d.id == gone) else { return };
        self.lsp_did_close(gone);
        self.docs.remove(i);
        if self.docs.is_empty() {
            self.new_scratch();
        }
        if self.current > i || self.current >= self.docs.len() {
            self.current = self.current.saturating_sub(1).min(self.docs.len() - 1);
        }
        let now = self.docs[self.current].id;
        for v in self.views.iter_mut() {
            if v.doc == gone {
                (v.doc, v.top, v.left) = (now, 0, 0);
            }
            v.jumps.remove_doc(gone);
            v.last_doc = v.last_doc.filter(|&d| d != gone);
        }
    }

    // ── Splits ────────────────────────────────────────────────────────────

    fn view_mut(&mut self, id: crate::split::ViewId) -> Option<&mut View> {
        self.views.iter_mut().find(|v| v.id == id)
    }

    /// Every event: focused pane = the current document and its scroll (even if the document changed by
    /// another path — opening a file, switching buffers).
    pub fn sync_focus_view(&mut self) {
        self.track_doc_switch();
        let (doc, top, left) = {
            let d = self.doc();
            (d.id, d.top, d.left)
        };
        let focus = self.focus;
        if let Some(v) = self.view_mut(focus) {
            (v.doc, v.top, v.left) = (doc, top, left);
        }
    }

    /// Moves focus to another pane: saves the current pane's scroll, then restores the new pane's
    /// document and scroll.
    pub fn focus_view(&mut self, id: crate::split::ViewId) {
        if id == self.focus {
            return;
        }
        self.sync_focus_view();
        let Some(v) = self.views.iter().find(|v| v.id == id).cloned() else { return };
        self.focus = id;
        if let Some(i) = self.docs.iter().position(|d| d.id == v.doc) {
            self.current = i;
            let d = &mut self.docs[i];
            (d.top, d.left) = (v.top, v.left);
        }
        self.completion = None;
        self.popup = None;
    }

    /// Splits the current pane — the new pane shows the same document at the same spot; focus moves to it.
    pub fn split_view(&mut self, dir: crate::split::Dir) {
        self.sync_focus_view();
        let id = self.next_view;
        self.next_view += 1;
        let Some(cur) = self.views.iter().find(|v| v.id == self.focus).cloned() else { return };
        self.views.push(View { id, ..cur });
        self.split.split(self.focus, id, dir);
        self.focus = id;
    }

    /// Closes the current pane — false if it's the last one (then `:q` quits the editor).
    pub fn close_view(&mut self) -> bool {
        if self.views.len() <= 1 {
            return false;
        }
        let gone = self.focus;
        let order = self.split.leaves();
        let i = order.iter().position(|v| *v == gone).unwrap_or(0);
        self.split.remove(gone);
        self.views.retain(|v| v.id != gone);
        let next = self.split.leaves()[i.saturating_sub(1).min(self.views.len() - 1)];
        self.focus_view(next); // syncing the gone pane finds nothing
        true
    }

    /// Keep only this pane.
    pub fn only_view(&mut self) {
        self.sync_focus_view();
        self.views.retain(|v| v.id == self.focus);
        self.split = crate::split::Split::Leaf(self.focus);
    }

    /// Next pane (`C-w w`) / by direction (`C-w h j k l` — using the last frame's pane layout).
    pub fn cycle_view(&mut self, delta: isize) {
        let order = self.split.leaves();
        let i = order.iter().position(|v| *v == self.focus).unwrap_or(0) as isize;
        let next = order[(i + delta).rem_euclid(order.len() as isize) as usize];
        self.focus_view(next);
    }

    pub fn focus_dir(&mut self, dx: i32, dy: i32) {
        let panes: Vec<_> = self.screen.panes.iter().map(|(v, r, _, _)| (*v, *r)).collect();
        if let Some(v) = crate::split::neighbor(&panes, self.focus, dx, dy) {
            self.focus_view(v);
        }
    }

    pub fn cycle_buffer(&mut self, delta: isize) {
        let n = self.docs.len() as isize;
        self.current = (self.current as isize + delta).rem_euclid(n) as usize;
    }

    /// Opens `path` with the cursor at (line, byte column) — a jump point is left behind.
    pub fn open_at(&mut self, path: &Path, line: usize, col: usize) -> Result<()> {
        self.push_jump();
        self.open(path)?;
        let doc = self.doc_mut();
        let line = line.min(mv::last_line(&doc.text));
        let (ls, le) = (mv::line_start(&doc.text, line), mv::line_end(&doc.text, line));
        // If the byte column is mid-character (the file changed), snap to the char start
        let pos = doc.text.char_to_byte(doc.text.byte_to_char((ls + col).min(le)));
        doc.set_selection(Selection::point(pos));
        Ok(())
    }

    pub fn goto_line(&mut self, line: usize) {
        self.push_jump();
        let doc = self.doc_mut();
        let line = line.min(mv::last_line(&doc.text));
        let pos = mv::line_start(&doc.text, line);
        doc.set_selection(Selection::point(pos));
    }

    pub fn set_status(&mut self, msg: impl Into<String>) {
        self.report(ToastKind::Info, msg.into());
    }

    pub fn set_error(&mut self, msg: impl Into<String>) {
        self.report(ToastKind::Error, msg.into());
    }

    /// Success (save, apply) — green bar, short.
    pub fn set_success(&mut self, msg: impl Into<String>) {
        self.report(ToastKind::Success, msg.into());
    }

    /// Caution (changed on disk, conflict) — yellow bar, long.
    pub fn set_warning(&mut self, msg: impl Into<String>) {
        self.report(ToastKind::Warning, msg.into());
    }

    fn report(&mut self, kind: ToastKind, msg: String) {
        let level = match kind {
            ToastKind::Error => "error",
            ToastKind::Warning => "warning",
            _ => "info",
        };
        crate::log::line("tarae", level, &msg);
        self.toast(kind, msg.clone());
        let severity = if kind == ToastKind::Error { Severity::Error } else { Severity::Info };
        self.status = Some((msg, severity));
    }

    /// Record only, no toast (things already on screen — diagnostic at the cursor, etc.).
    pub fn note(&mut self, msg: impl Into<String>) {
        self.status = Some((msg.into(), Severity::Info));
    }

    fn toast(&mut self, kind: ToastKind, text: String) {
        // If the same message comes again, extend its time instead of popping a new one
        if let Some(t) = self.toasts.iter_mut().find(|t| t.text == text && t.kind == kind) {
            t.at = std::time::Instant::now();
            return;
        }
        self.toasts.push(Toast { text, kind, at: std::time::Instant::now() });
        if self.toasts.len() > 4 {
            self.toasts.remove(0);
        }
    }

    /// For the status line — pending count and keys.
    pub fn pending_display(&self) -> String {
        let mut s = self.count.map(|c| c.to_string()).unwrap_or_default();
        for k in &self.pending {
            s.push_str(&k.to_string());
        }
        s
    }

    // ── Undo groups ───────────────────────────────────────────────────────

    fn open_undo_group(&mut self) {
        // Focus moved to another document mid-group (pane click in insert mode) — the group stays with its own
        if self.undo_group.as_ref().is_some_and(|(id, _)| *id != self.doc().id) {
            self.close_undo_group();
        }
        if self.undo_group.is_none() {
            let doc = self.doc();
            self.undo_group = Some((doc.id, doc.snapshot()));
        }
    }

    /// Insert mode `C-s`: what was typed so far becomes its own undo step; typing goes on in a new one.
    pub fn commit_undo_checkpoint(&mut self) {
        self.close_undo_group();
        self.open_undo_group();
    }

    /// This document's language (by its grammar, else by its path — also when the grammar isn't there).
    pub fn lang_spec(&self) -> Option<&'static syntax::LangSpec> {
        let doc = self.doc();
        doc.syntax
            .as_ref()
            .and_then(|s| syntax::spec(&s.lang.name))
            .or_else(|| doc.path.as_deref().and_then(syntax::detect))
    }

    /// Whether `doc`'s long lines wrap (`editor.soft-wrap`) — "prose" = Markdown, commit messages, and
    /// files with no language.
    pub fn wraps(&self, doc: &Document) -> bool {
        match self.config.soft_wrap.as_str() {
            "always" => true,
            "never" => false,
            _ => {
                let lang = doc
                    .syntax
                    .as_ref()
                    .map(|s| s.lang.name.as_str())
                    .or_else(|| doc.path.as_deref().and_then(syntax::detect).map(|s| s.name.as_str()));
                matches!(lang, None | Some("markdown" | "git-commit"))
            }
        }
    }

    /// Insert-mode auto-pairs for this document — empty when turned off.
    pub fn pairs(&self) -> &'static [(char, char)] {
        if !self.config.auto_pairs {
            return &[];
        }
        self.lang_spec().and_then(|s| s.auto_pairs.as_deref()).unwrap_or(syntax::DEFAULT_PAIRS)
    }

    fn close_undo_group(&mut self) {
        if let Some((id, snap)) = self.undo_group.take()
            && let Some(doc) = self.docs.iter_mut().find(|d| d.id == id)
        {
            doc.commit_undo(snap);
        }
    }

    /// undo/redo itself doesn't create an edit group.
    pub fn abandon_undo_group(&mut self) {
        self.undo_group = None;
    }

    // ── Dispatch ─────────────────────────────────────────────────────────

    pub fn handle_event(&mut self, ev: Event) {
        match ev {
            Event::Key(k) => {
                self.handle_key(k);
                self.auto_save_idle();
            }
            Event::Mouse(m) => self.handle_mouse(m),
            Event::Focus(gained) => self.on_focus(gained),
            Event::Resize => {}
            Event::Job(apply) => apply(self),
        }
        self.sync_focus_view();
        self.schedule_parses();
        self.lsp_flush();
        self.inlay_schedule();
        self.git_schedule();
        self.blame_schedule();
        self.agent_next_diff();
        crate::follow::watch(self);
        crate::notes::attach(self);
        self.agent_selection_tick(false);
        self.schedule_tick();
    }

    // ── Mouse ────────────────────────────────────────────────────────────

    /// Press = place cursor (alt adds one) · drag = select · wheel = 3 lines. Clicking the chat pane
    /// focuses it; in a picker, wheel = move selection, click = open.
    pub fn handle_mouse(&mut self, m: crate::event::Mouse) {
        use crate::event::MouseKind as K;
        let s = self.screen.clone();
        let (x, y) = (m.x as usize, m.y as usize);
        self.status = None;
        if m.kind == K::Down
            && let Some(&(_, _, _, k)) = s.offer_buttons.iter().find(|b| b.0 == y && (b.1..b.2).contains(&x))
        {
            match k {
                'y' => self.offer_accept(),
                'n' => self.offer_dismiss(),
                d => self.offer_to_front(d.to_digit(10).unwrap_or(0) as usize),
            }
            return;
        }
        if let Some(p) = self.picker.as_mut() {
            let over_preview = s.picker_preview_x.is_some_and(|px| x >= px);
            match m.kind {
                K::ScrollUp if over_preview => p.preview_scroll = p.preview_scroll.saturating_sub(3),
                K::ScrollDown if over_preview => p.preview_scroll += 3,
                K::ScrollUp => p.move_by(-3),
                K::ScrollDown => p.move_by(3),
                K::Down => {
                    if let Some((top, rows)) = s.picker_list
                        && (top..top + rows).contains(&y)
                        && p.scroll + (y - top) < p.counts().0
                    {
                        p.selected = p.scroll + (y - top);
                        self.handle_key(Key::plain(Code::Enter));
                    }
                }
                _ => {}
            }
            return;
        }
        // Wheel over the floating docs (hover, `space g p`) scrolls them
        if let (Some(lines), Some((px, py, pw, ph))) = (self.popup.as_ref(), s.popup.get())
            && (px..px + pw).contains(&x)
            && (py..py + ph).contains(&y)
        {
            match m.kind {
                K::ScrollDown => return self.popup_scroll = (self.popup_scroll + 3).min(lines.len()),
                K::ScrollUp => return self.popup_scroll = self.popup_scroll.saturating_sub(3),
                _ => {}
            }
        }
        // The minimized chat: a trail row jumps there, anywhere else brings the panel back
        if let (Some((mx, my, mw, mh)), Some(c)) = (s.chat_mini.get(), self.chat.as_mut())
            && (mx..mx + mw).contains(&x)
            && (my..my + mh).contains(&y)
        {
            if m.kind == K::Down {
                let links = s.chat_links.borrow();
                match links.iter().find(|l| l.0 == y && (l.1..l.2).contains(&x)) {
                    Some((.., link)) => crate::chat::jump(self, link.clone()),
                    None => {
                        c.unfolded = true;
                        c.focused = true;
                    }
                }
            }
            return;
        }
        if let (Some(cx), Some(c)) = (s.chat_x, self.chat.as_mut())
            && x >= cx
        {
            match m.kind {
                K::Down => {
                    c.focused = true;
                    let links = s.chat_links.borrow();
                    if let Some((.., link)) = links.iter().find(|l| l.0 == y && (l.1..l.2).contains(&x)) {
                        crate::chat::jump(self, link.clone());
                    }
                }
                K::ScrollUp => c.scroll += 3,
                K::ScrollDown => c.scroll = c.scroll.saturating_sub(3),
                _ => {}
            }
            return;
        }
        if self.review.is_some() || self.welcome() {
            return;
        }
        // Which pane (with splits, focus moves to the pane clicked/scrolled). With several panes the top
        // row is the title row.
        let Some((pid, rect, gutter, lines)) = s
            .panes
            .iter()
            .find(|(_, r, ..)| (r.x..r.x + r.w).contains(&x) && (r.y..r.y + r.h).contains(&y))
            .cloned()
        else {
            return;
        };
        if matches!(m.kind, K::Down | K::ScrollUp | K::ScrollDown) {
            self.focus_view(pid);
        }
        // Scrolling the code yourself takes over from follow mode (clicks do too, via the cursor)
        if matches!(m.kind, K::ScrollUp | K::ScrollDown)
            && let Some(c) = self.chat.as_mut().filter(|c| c.follow.active() && c.busy())
        {
            c.follow.paused = true;
        }
        if pid != self.focus {
            return;
        }
        let title = usize::from(self.views.len() > 1);
        if y < rect.y + title {
            return;
        }
        let row = y - rect.y - title;
        if let Some(c) = self.chat.as_mut() {
            c.focused = false;
        }
        self.completion = None;
        self.popup = None;
        let tab = self.config.tab_width;
        let doc = self.doc();
        let last = mv::last_line(&doc.text);
        match m.kind {
            K::ScrollUp | K::ScrollDown => {
                let head = doc.selection().primary().cursor(&doc.text);
                let col = mv::visual_col(&doc.text, head, tab);
                let line = mv::line_of(&doc.text, head);
                let (line, top) = if m.kind == K::ScrollDown {
                    ((line + 3).min(last), (doc.top + 3).min(last))
                } else {
                    (line.saturating_sub(3), doc.top.saturating_sub(3))
                };
                let pos = mv::pos_at_col(&doc.text, line, col, tab);
                let doc = self.doc_mut();
                doc.top = top;
                doc.set_selection(Selection::point(pos));
            }
            K::Down | K::Drag => {
                let (line, c0, x0, end) =
                    lines.get(row).copied().unwrap_or((doc.top + row, doc.left, 0, usize::MAX));
                let line = line.min(last);
                // A wrapped row: cells after its indent, never past its end (that's the next row's)
                let col =
                    (c0 + x.saturating_sub(rect.x + gutter).saturating_sub(x0)).min(end.saturating_sub(1));
                let pos = mv::pos_at_col(&doc.text, line, col, tab);
                let text = doc.text.clone();
                let anchor = self.mouse_anchor;
                let doc = self.doc_mut();
                let mut new_anchor = anchor;
                match (m.kind, anchor) {
                    (K::Drag, Some(a)) => {
                        // Dragging forward includes the last char (selection = half-open byte range)
                        let r = if pos >= a {
                            Range::new(a, crate::graphemes::next_boundary(&text, pos))
                        } else {
                            Range::new(crate::graphemes::next_boundary(&text, a), pos)
                        };
                        doc.set_selection(Selection::new(vec![r], 0));
                    }
                    (K::Down, _) if m.alt => {
                        let mut ranges = doc.selection().ranges().to_vec();
                        ranges.push(Range::point(pos));
                        let n = ranges.len() - 1;
                        doc.set_selection(Selection::new(ranges, n));
                        new_anchor = Some(pos);
                    }
                    _ => {
                        doc.set_selection(Selection::point(pos));
                        new_anchor = Some(pos);
                    }
                }
                self.mouse_anchor = new_anchor;
            }
            K::Up => self.mouse_anchor = None,
        }
    }

    /// When to show the start screen: a single empty unnamed buffer and nothing typed.
    pub fn welcome(&self) -> bool {
        self.docs.len() == 1
            && self.docs[0].path.is_none()
            && self.docs[0].text.len_bytes() == 0
            && !self.docs[0].is_modified()
            && self.mode == Mode::Normal
    }

    /// While waiting on something (claude answer), redraw again after 70 ms — wave animation, elapsed time.
    /// Nothing to wait for, no tick (an idle editor uses 0 CPU).
    fn schedule_tick(&mut self) {
        self.toasts.retain(Toast::alive);
        let waiting = self.review.as_ref().is_some_and(|r| !r.ready)
            || self.chat.as_ref().is_some_and(|c| c.busy())
            || !self.toasts.is_empty()
            || self.dap.as_ref().is_some_and(|d| d.state == crate::dap::State::Building)
            || self.test_run.as_ref().is_some_and(|r| r.state == crate::testing::RunState::Running)
            || self.offer_busy();
        if !waiting || self.ticking || cfg!(test) {
            return;
        }
        self.ticking = true;
        self.events.jobs().spawn(|| {
            std::thread::sleep(std::time::Duration::from_millis(70));
            |ed: &mut Editor| ed.ticking = false
        });
    }

    // ── Pickers ──────────────────────────────────────────────────────────

    /// Opens a picker; if `fill` is given, fills the list on a worker thread.
    pub fn open_picker(&mut self, mut picker: Picker, fill: Option<crate::picker::Fill>) {
        self.picker_ids += 1;
        let id = self.picker_ids;
        picker.id = id;
        if let Some(fill) = fill {
            picker.loading = true;
            self.events.jobs().spawn(move || {
                let result = fill();
                move |ed: &mut Editor| {
                    let Some(p) = ed.picker.as_mut().filter(|p| p.id == id) else { return };
                    match result {
                        Ok(items) => p.set_items(items),
                        Err(e) => {
                            ed.picker = None;
                            ed.set_error(e);
                        }
                    }
                }
            });
        }
        self.picker = Some(picker);
    }

    /// A closing picker is kept for `space '` — not code actions (their list goes stale with the text).
    pub(crate) fn keep_last_picker(&mut self) {
        if let Some(p) = self.picker.take()
            && !p.items().iter().any(|i| matches!(i.action, Action::Code(_)))
        {
            self.last_picker = Some(p);
        }
    }

    /// `space '` — the last picker again, as it was left (query, selection).
    pub fn reopen_last_picker(&mut self) {
        match self.last_picker.take() {
            Some(p) => self.open_picker(p, None),
            None => self.note("no picker to reopen yet"),
        }
    }

    fn global_search(&mut self, pattern: String) {
        if pattern.is_empty() {
            return;
        }
        let root = std::env::current_dir().unwrap_or_default();
        let title = format!("grep /{pattern}/");
        let mut picker = Picker::new(title, Vec::new(), false);
        picker.grep = Some(pattern.clone());
        self.open_picker(picker, Some(Box::new(move || crate::picker::grep(&root, &pattern))));
    }

    fn picker_key(&mut self, key: Key) {
        let Some(p) = self.picker.as_mut() else { return };
        match (key.code, key.ctrl) {
            (Code::Esc, _) | (Code::Char('c'), true) => {
                self.keep_last_picker();
                if let Some(t) = self.theme_before.take() {
                    self.theme = t;
                }
                return;
            }
            // File tree: Enter on a folder opens/folds it, → opens, ← folds or goes up
            (Code::Enter, _) if p.current().is_some_and(|i| matches!(i.action, Action::Dir(_))) => {
                return p.tree_toggle();
            }
            (Code::Right, _) if p.in_tree() => return p.tree_right(),
            (Code::Left, _) if p.in_tree() => return p.tree_left(),
            // Global search results: replace what's listed
            (Code::Char('r'), true) if p.grep.is_some() => return self.replace_ask(),
            // Replace preview: Enter applies to every file still listed
            (Code::Enter, _) if p.current().is_some_and(|i| matches!(i.action, Action::ReplaceFile(_))) => {
                let files: Vec<usize> = p
                    .shown()
                    .filter_map(|i| match i.action {
                        Action::ReplaceFile(n) => Some(n),
                        _ => None,
                    })
                    .collect();
                self.picker = None;
                return self.replace_apply(&files);
            }
            (Code::Enter, _) => {
                let action = p.current().map(|i| i.action.clone());
                self.keep_last_picker();
                self.theme_before = None;
                if let Some(a) = action {
                    self.run_picker_action(a);
                }
                return;
            }
            (Code::Down | Code::Tab, _) | (Code::Char('n'), true) => p.move_by(1),
            (Code::Up | Code::BackTab, _) | (Code::Char('p'), true) => p.move_by(-1),
            (Code::PageDown, _) | (Code::Char('d'), true) => p.move_by(10),
            (Code::PageUp, _) | (Code::Char('u'), true) => p.move_by(-10),
            // The preview pane
            (Code::Char('f'), true) => p.preview_scroll += 10,
            (Code::Char('b'), true) => p.preview_scroll = p.preview_scroll.saturating_sub(10),
            (Code::Backspace, _) => p.pop(),
            (Code::Char(c), false) if !key.alt => p.push(c),
            _ => {}
        }
        if matches!(key.code, Code::Backspace | Code::Char(_)) && !key.ctrl {
            self.workspace_symbols_requery();
        }
        self.preview_theme();
        self.code_action_preview();
    }

    /// Runs one command by name (`:theme` → theme picker, etc.).
    pub fn run_command_by_name(&mut self, name: &str) {
        if let Some(c) = commands::find(name) {
            self.run(&[MappableCommand::Static(c)], None);
        }
    }

    /// Theme picker: applies the highlighted theme right away (remembering the original theme).
    pub fn preview_theme(&mut self) {
        let Some(name) = self.picker.as_ref().and_then(|p| p.current()).and_then(|i| match &i.action {
            Action::Theme(n) => Some(n.clone()),
            _ => None,
        }) else {
            return;
        };
        if self.theme.name == name {
            return;
        }
        // User themes are files — read on a worker thread; apply only if that row is still highlighted
        self.events.jobs().spawn(move || {
            let result = crate::theme::load(&name);
            move |ed: &mut Editor| {
                let row = ed.picker.as_ref().and_then(|p| p.current()).map(|i| &i.action);
                if let (Some(Action::Theme(n)), Ok(t)) = (row, result)
                    && *n == name
                {
                    let before = std::mem::replace(&mut ed.theme, t);
                    ed.theme_before.get_or_insert(before);
                }
            }
        });
    }

    fn run_picker_action(&mut self, action: Action) {
        let result = match action {
            Action::Code(i) => {
                self.run_code_action(i);
                Ok(())
            }
            Action::ReplaceFile(n) => {
                self.replace_apply(&[n]);
                Ok(())
            }
            Action::Dir(_) => Ok(()), // folders open in the picker (picker_key)
            // Open it at its first change
            Action::ChangedFile(n) => match self.changed_files.get(n).cloned() {
                Some(c) => {
                    return self.run_picker_action(Action::Goto { path: c.path, line: c.line, col: 0 });
                }
                None => Ok(()),
            },
            Action::Jump { index, .. } => {
                self.jump_to_entry(index);
                Ok(())
            }
            Action::Buffer(id) => {
                if let Some(i) = self.docs.iter().position(|d| d.id == id) {
                    self.current = i;
                }
                Ok(())
            }
            Action::Open(path) => self.open(&path),
            Action::Command(name) => {
                self.run_command_by_name(name);
                Ok(())
            }
            Action::Typed(line) => typed::execute(self, &line).map_err(|e| anyhow::anyhow!(e)),
            Action::Theme(name) => typed::execute(self, &format!("set! theme {name}"))
                .map_err(|e| anyhow::anyhow!(e))
                .map(|_| {
                    self.set_success(format!("Theme {name} · saved to your config"));
                }),
            Action::Goto { path, line, col } => self.open_at(&path, line, col),
        };
        if let Err(e) = result {
            self.set_error(format!("{e:#}"));
        }
    }

    // ── Syntax, themes (slow work all on worker threads) ──────────────────

    /// Picks a language by path, loads its grammar in the background and attaches it.
    pub fn attach_syntax(&mut self, id: DocId) {
        let Some(doc) = self.docs.iter().find(|d| d.id == id) else { return };
        let Some(spec) = doc.path.as_deref().and_then(syntax::detect) else { return };
        self.load_language(id, spec);
    }

    pub fn load_language(&mut self, id: DocId, spec: &'static syntax::LangSpec) {
        self.events.jobs().spawn(move || {
            let result = syntax::Loader::global().load(spec);
            move |ed: &mut Editor| {
                let Some(doc) = ed.docs.iter_mut().find(|d| d.id == id) else { return };
                match result {
                    Ok(lang) => doc.syntax = Some(syntax::Syntax::new(lang)),
                    // Grammar not downloaded yet — offer to download it (once per language per session)
                    Err(e) if e.starts_with("no grammar") => ed.offer_grammar(spec),
                    Err(e) => ed.set_status(format!("no highlighting: {e}")),
                }
            }
        });
    }

    /// `:grammar-install [lang|all]` — downloads and builds grammars (worker thread; when done, open
    /// documents get colored).
    /// Without a language, the current file's language.
    pub fn grammar_install(&mut self, arg: &str) -> Result<(), String> {
        let names: Vec<String> = match arg.trim() {
            "all" => crate::grammar::sources()
                .into_iter()
                .filter(|s| !crate::grammar::bundled(s))
                .map(|s| s.name)
                .collect(),
            "" => {
                let lang = self.doc().path.as_deref().and_then(syntax::detect).map(|s| s.name.clone());
                lang.map(|l| crate::grammar::for_language(&l)).unwrap_or_default()
            }
            some => some.split_whitespace().flat_map(crate::grammar::for_language).collect(),
        };
        if names.is_empty() {
            return Err("which language? :grammar-install rust (or all)".into());
        }
        let n = names.len();
        self.set_status(if n == 1 {
            format!("Fetching and building the {} grammar…", names[0])
        } else {
            format!("Fetching and building {n} grammars…")
        });
        self.install_grammars(names, None);
        Ok(())
    }

    /// Downloads and builds grammars (worker thread). `offer` = the card number if started from an offer
    /// card (results go to that card).
    pub fn install_grammars(&mut self, names: Vec<String>, offer: Option<u64>) {
        self.events.jobs().spawn(move || {
            let results = std::sync::Mutex::new(Vec::new());
            let r = crate::grammar::install(&names, false, |p| results.lock().unwrap().push(p));
            let results = results.into_inner().unwrap();
            move |ed: &mut Editor| {
                use crate::grammar::Progress;
                let mut resume = None;
                let mut failed: Vec<String> = results
                    .iter()
                    .filter_map(|p| match p {
                        Progress::Failed { name, why } => Some(format!("{name}: {why}")),
                        _ => None,
                    })
                    .collect();
                let whole_ok = r.is_ok();
                if let Err(e) = r {
                    failed.push(e);
                }
                let built = results.iter().filter(|p| matches!(p, Progress::Done { .. })).count();
                if offer.is_some() {
                    // If every reason is the same (no network, etc.), report it once
                    let whys: Vec<&str> = results
                        .iter()
                        .filter_map(|p| match p {
                            Progress::Failed { why, .. } => Some(why.as_str()),
                            _ => None,
                        })
                        .collect();
                    let reason = match whys.first() {
                        Some(w) if whole_ok && whys.iter().all(|x| x == w) => w.to_string(),
                        _ => failed.join("; "),
                    };
                    let what = offer.and_then(|id| ed.offer_by_id(id)).map(|o| o.what.clone());
                    if let (Some(crate::offer::What::Grammars { lang, then, .. }), Some(id)) = (what, offer) {
                        let ok = failed.is_empty();
                        ed.offer_finished(id, if ok { Ok(()) } else { Err(reason) });
                        if ok {
                            ed.set_success(format!("{lang} colors are on"));
                            resume = then;
                        }
                    }
                } else if failed.is_empty() {
                    ed.set_success(match built {
                        0 => "Grammars already up to date".to_string(),
                        1 => "Grammar installed — colors are on".to_string(),
                        n => format!("{n} grammars installed — colors are on"),
                    });
                } else {
                    ed.set_error(format!(
                        "grammar install failed — {} (needs git and a C compiler)",
                        failed.join("; ")
                    ));
                }
                syntax::Loader::global().forget_missing();
                let ids: Vec<DocId> = ed.docs.iter().filter(|d| d.syntax.is_none()).map(|d| d.id).collect();
                for id in ids {
                    ed.attach_syntax(id);
                }
                // Work that stopped for lack of a grammar (tests, debugging) — continue it now
                if let Some(r) = resume {
                    ed.resume(r);
                }
            }
        });
    }

    /// Starts a background parse for each edited document not already parsing (at the end of every event).
    pub fn schedule_parses(&mut self) {
        for doc in &mut self.docs {
            let Some(syn) = doc.syntax.as_mut() else { continue };
            if doc.loading || !syn.dirty || syn.in_flight {
                continue;
            }
            let job = syn.start_parse(&doc.text);
            let id = doc.id;
            self.events.jobs().spawn(move || {
                let generation = job.generation;
                let tree = job.run();
                move |ed: &mut Editor| {
                    if let Some(s) = ed.docs.iter_mut().find(|d| d.id == id).and_then(|d| d.syntax.as_mut()) {
                        s.finish_parse(generation, tree);
                    }
                }
            });
        }
    }

    /// If the configured theme name differs from the current theme, loads and swaps it in the background.
    pub fn ensure_theme(&mut self) {
        let name = self.config.theme.clone();
        if self.theme.name == name {
            return;
        }
        self.events.jobs().spawn(move || {
            let result = crate::theme::load(&name);
            move |ed: &mut Editor| match result {
                Ok(t) if ed.config.theme == t.name => {
                    if let Some(w) = t.warnings.first() {
                        let more = t.warnings.len() - 1;
                        ed.set_warning(if more > 0 { format!("{w} (+{more} more)") } else { w.clone() });
                    }
                    ed.theme = t;
                }
                Ok(_) => {}
                Err(e) => ed.set_error(e),
            }
        });
    }

    /// Replaces the config (already read and parsed by the watcher thread — no disk wait on the main thread).
    pub fn reload_config(&mut self, config: Config, warnings: Vec<String>) {
        self.keymaps = config.keymaps;
        self.config = config.editor;
        self.theme = config.theme;
        for (path, v) in &self.overrides {
            let _ = crate::settings::apply(&mut self.config, path, v);
        }
        self.ensure_theme();
        self.pending.clear();
        self.count = None;
        if warnings.is_empty() {
            self.set_status("config reloaded");
        } else {
            self.set_error(format!("config: {}", warnings.join("; ")));
        }
    }

    pub fn handle_key(&mut self, key: Key) {
        // Grammar offers: y·n·Esc in normal mode (something else open or a pending prefix key goes first)
        if !self.offers.is_empty()
            && self.mode == Mode::Normal
            && self.pending.is_empty()
            && self.prompt.is_none()
            && self.picker.is_none()
            && self.review.is_none()
            && self.offer_key(key)
        {
            return;
        }
        // If the completion list is up, intercept selection keys first (as in Helix: Tab/C-n down,
        // S-Tab/C-p up; Enter inserts only when something is selected — otherwise a plain newline).
        if self.mode == Mode::Insert
            && let Some(c) = self.completion.as_mut()
        {
            match (key.code, key.ctrl) {
                (Code::Tab | Code::Down, false) | (Code::Char('n'), true) => {
                    c.move_by(1);
                    return self.completion_docs();
                }
                (Code::BackTab | Code::Up, false) | (Code::Char('p'), true) => {
                    c.move_by(-1);
                    return self.completion_docs();
                }
                (Code::Enter, false) if c.selected.is_some() => return self.completion_accept(),
                (Code::Esc, _) => self.completion = None,
                _ => {}
            }
        }
        let was_insert = self.mode == Mode::Insert;
        self.insert_finished(0); // left insert mode some other way (mouse …)
        self.insert_key(key);
        self.handle_key_inner(key);
        self.insert_finished(self.last_trigger_len);
        self.track_doc_switch();
        if self.diag_card_hidden.is_some_and(|h| h != (self.doc().id, self.cursor_line())) {
            self.diag_card_hidden = None;
        }
        let typed = key.plain_char();
        // Replaying `.` — no completion or signature requests for the replayed keys
        if self.repeating_insert {
            return;
        }
        if was_insert || self.completion.is_some() {
            match key.code {
                Code::Char(_) if typed.is_some() => self.completion_after_edit(typed),
                Code::Backspace => self.completion_after_edit(None),
                _ => self.completion = None,
            }
        }
        if self.mode != Mode::Insert {
            self.signature = None;
        } else if was_insert {
            self.signature_after_key(typed);
        }
    }

    fn handle_key_inner(&mut self, key: Key) {
        self.status = None;
        // Esc in normal mode = clear search highlight (and still does its other work)
        if key.code == Code::Esc && self.mode == Mode::Normal && self.prompt.is_none() {
            self.search_hl = false;
            if self.popup.is_none() {
                self.diag_card_hidden = Some((self.doc().id, self.cursor_line()));
            }
        }
        // Floating text: C-d/C-u scroll it, other keys close it (esc only closes).
        if let Some(lines) = &self.popup
            && key.ctrl
        {
            match key.code {
                Code::Char('d') => return self.popup_scroll = (self.popup_scroll + 8).min(lines.len()),
                Code::Char('u') => return self.popup_scroll = self.popup_scroll.saturating_sub(8),
                _ => {}
            }
        }
        self.popup_scroll = 0;
        if self.popup.take().is_some() && key.code == Code::Esc {
            return;
        }
        // Keys a replayed macro feeds in aren't recorded — the `q` that replays them already was
        if self.replaying.is_empty()
            && !self.repeating_insert
            && let Some((_, keys)) = &mut self.recording
        {
            keys.push(key);
        }
        if llm::review_key(self, key) {
            return;
        }
        if self.picker.is_some() {
            self.picker_key(key);
            return;
        }
        if crate::chat::key(self, key) {
            return;
        }
        // Start screen: digits = open a recent file
        if self.welcome()
            && self.pending.is_empty()
            && let Some(d) = key.plain_char().and_then(|c| c.to_digit(10)).filter(|d| *d >= 1)
            && let Some(path) = self.recent.get(d as usize - 1).cloned()
        {
            if let Err(e) = self.open(&path) {
                self.set_error(format!("{e:#}"));
            }
            return;
        }
        if self.prompt.is_some() {
            self.handle_prompt_key(key);
            return;
        }
        // A loading buffer ignores edit keys (`:` commands and buffer moves are allowed — :q·:bn while
        // reading).
        if self.doc().loading && key.plain_char() != Some(':') && self.pending.is_empty() {
            let name = self.doc().display_name();
            self.set_status(format!("{name} is still loading…"));
            return;
        }
        // `f x`·`r x`·`"a` — the awaited character. A non-character key (esc, etc.) cancels.
        if let Some((f, count)) = self.on_next_char.take() {
            let ch = match (key.code, key.ctrl || key.alt) {
                (Code::Char(c), false) => Some(c),
                (Code::Enter, _) => Some('\n'),
                (Code::Tab, _) => Some('\t'),
                _ => None,
            };
            match ch {
                Some(ch) => self.with_group_count(count, |cx| f(cx, ch)),
                None => self.jump_labels = None, // a `gw` label cancelled
            }
            return;
        }
        // Count prefix (normal/select, no pending key). A leading 0 isn't a count.
        if self.mode != Mode::Insert
            && self.pending.is_empty()
            && let Some(c @ '0'..='9') = key.plain_char()
            && (c != '0' || self.count.is_some())
        {
            let digit = c.to_digit(10).unwrap() as usize;
            self.count = Some(self.count.unwrap_or(0).saturating_mul(10).saturating_add(digit));
            return;
        }
        self.pending.push(key);
        self.dispatch_pending();
    }

    /// Looks up the pending keys: run the command, wait for more, or (insert mode) type them.
    fn dispatch_pending(&mut self) {
        let cmds = match self.keymaps.lookup(self.mode, &self.pending) {
            Lookup::Pending => return,
            Lookup::Matched(cmds) => Some(cmds.to_vec()),
            Lookup::NotFound => None,
        };
        let pending = std::mem::take(&mut self.pending);
        let count = self.count.take();
        self.last_trigger_len = pending.len();
        // Sticky view mode: `Z` stays open for the next key (Esc, or any key it doesn't have, leaves)
        let sticky = cmds.is_some() && pending.len() > 1 && pending[0].plain_char() == Some('Z');
        match (cmds, pending.split_last()) {
            (Some(cmds), _) => {
                self.run(&cmds, count);
                if sticky && self.mode == Mode::Normal {
                    self.pending = vec![pending[0]];
                }
            }
            // Unmapped keys in insert mode = input. A sequence that broke off (`jx` under `j k`): the keys
            // before are text, the last goes through the keymap again (`j<esc>` = "j", then leave insert).
            (None, Some((&last, before))) if self.mode == Mode::Insert => {
                let chars: Vec<char> = before.iter().filter_map(Key::plain_char).collect();
                let last_char = last.plain_char().filter(|_| before.is_empty());
                if !chars.is_empty() || last_char.is_some() {
                    self.with_group(|cx| {
                        for c in chars.into_iter().chain(last_char) {
                            commands::insert_char(cx, c);
                        }
                    });
                }
                if !before.is_empty() {
                    self.pending.push(last);
                    self.dispatch_pending();
                }
            }
            (None, _) => {}
        }
    }

    pub(crate) fn run(&mut self, cmds: &[MappableCommand], count: Option<usize>) {
        let was_insert = self.mode == Mode::Insert;
        self.with_group(|cx| {
            for cmd in cmds {
                match cmd {
                    MappableCommand::Static(c) => {
                        (c.fun)(&mut Context { editor: cx.editor, count });
                        if c.motion {
                            let fun = c.fun;
                            cx.editor.last_motion = Some(Rc::new(move |cx: &mut Context| {
                                fun(&mut Context { editor: cx.editor, count })
                            }));
                        }
                    }
                    MappableCommand::Typed(line) => {
                        if let Err(e) = typed::execute(cx.editor, line) {
                            cx.editor.set_error(e);
                        }
                    }
                }
                if cx.editor.should_quit {
                    break;
                }
            }
        });
        if !was_insert && self.mode == Mode::Insert {
            self.insert_started(cmds, count);
        }
        // `"a` applies only to the very next command.
        self.selected_register = None;
    }

    // ── Registers ────────────────────────────────────────────────────────

    pub fn register_name(&self) -> char {
        self.selected_register.unwrap_or('"')
    }

    pub fn write_register(&mut self, name: char, values: Vec<String>) {
        match name {
            '_' => {}
            '+' => crate::clipboard::copy(&self.events.jobs(), values.join("\n")),
            _ => {
                self.registers.insert(name, values);
            }
        }
    }

    /// Runs inside an edit group. Closes the group if it ends outside insert mode.
    pub(crate) fn with_group(&mut self, f: impl FnOnce(&mut Context)) {
        self.with_group_count(None, f)
    }

    pub(crate) fn with_group_count(&mut self, count: Option<usize>, f: impl FnOnce(&mut Context)) {
        self.open_undo_group();
        self.sticky_used = false;
        f(&mut Context { editor: self, count });
        if !self.sticky_used {
            self.sticky = None;
        }
        if self.mode != Mode::Insert {
            self.close_undo_group();
            self.clamp_for_normal();
        }
    }

    /// In normal/select mode the block cursor can't sit past the end of the document (EOF).
    fn clamp_for_normal(&mut self) {
        let doc = self.doc_mut();
        let len = doc.text.len_bytes();
        if len == 0 {
            return;
        }
        let sel = doc.selection().transform(|r| {
            if r.is_empty() && r.head >= len {
                Range::point(crate::graphemes::prev_boundary(&doc.text, len))
            } else {
                r
            }
        });
        doc.set_selection(sel);
    }

    /// The primary cursor's line.
    pub fn cursor_line(&self) -> usize {
        let doc = self.doc();
        doc.text.byte_to_line(doc.selection().primary().cursor(&doc.text).min(doc.text.len_bytes()))
    }

    /// Doc-comment block shown folded on screen (normal mode, rendering on, language uses that marker,
    /// no horizontal scroll) — `j`/`k` step over it as one line.
    pub fn folded_doc_block_at(&self, line: usize) -> Option<(usize, usize)> {
        let doc = self.doc();
        let lang = doc
            .syntax
            .as_ref()
            .map(|s| s.lang.name.as_str())
            .or_else(|| doc.path.as_deref().and_then(crate::syntax::detect).map(|s| s.name.as_str()));
        if !self.config.render_doc_comments
            || self.mode != Mode::Normal
            || self.review.is_some()
            || doc.left > 0
            || !crate::doccomment::applies(lang)
        {
            return None;
        }
        crate::doccomment::block_at(&doc.text, line)
    }

    pub fn open_prompt(&mut self, kind: PromptKind, text: &str) {
        self.prompt = Some(Prompt { kind, text: text.to_string(), cycle: None });
        if kind == PromptKind::Command {
            self.cmdline_prefetch();
        }
    }

    /// Completion candidates for the `:` input (relative to the input when cycling began, if Tab-cycling).
    pub fn cmdline_completion(&self) -> Option<crate::cmdline::Completion> {
        let p = self.prompt.as_ref().filter(|p| p.kind == PromptKind::Command)?;
        let base = p.cycle.as_ref().map_or(p.text.as_str(), |(b, _)| b.as_str());
        let cwd = std::env::current_dir().unwrap_or_default();
        Some(crate::cmdline::complete(base, &cwd, &self.cmd_lists))
    }

    /// If the lists a candidate needs (folders, themes) aren't read yet, read them on a worker thread.
    fn cmdline_prefetch(&mut self) {
        let Some(p) = self.prompt.as_ref().filter(|p| p.kind == PromptKind::Command) else { return };
        let text = p.text.clone();
        let Some((name, _)) = text.split_once(char::is_whitespace) else { return };
        match crate::cmdline::find(name).map(|c| c.arg) {
            Some(crate::cmdline::Arg::File) => {
                let token = text.rsplit(char::is_whitespace).next().unwrap_or_default();
                let cwd = std::env::current_dir().unwrap_or_default();
                let (dir, _) = crate::cmdline::file_query(token, &cwd);
                if self.cmd_lists.dirs.contains_key(&dir) || !self.cmd_loading.insert(dir.clone()) {
                    return;
                }
                self.events.jobs().spawn(move || {
                    let mut entries: Vec<(String, bool)> = std::fs::read_dir(&dir)
                        .map(|rd| {
                            rd.flatten()
                                .take(5000)
                                .map(|e| {
                                    let is_dir = e.path().is_dir(); // folder symlinks count as folders
                                    (e.file_name().to_string_lossy().into_owned(), is_dir)
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    entries.sort();
                    move |ed: &mut Editor| {
                        ed.cmd_loading.remove(&dir);
                        ed.cmd_lists.dirs.insert(dir, entries);
                    }
                });
            }
            Some(crate::cmdline::Arg::Theme) if self.cmd_lists.themes.is_none() => {
                let key = std::path::PathBuf::from("\0themes");
                if !self.cmd_loading.insert(key.clone()) {
                    return;
                }
                self.events.jobs().spawn(move || {
                    let names = crate::theme::available();
                    move |ed: &mut Editor| {
                        ed.cmd_loading.remove(&key);
                        ed.cmd_lists.themes = Some(names);
                    }
                });
            }
            _ => {}
        }
    }

    /// Tab (forward)·Shift-Tab (backward): cycle through candidates, filling each in. With only one, fill
    /// and stop — a command gets a trailing space, a folder can be completed further inside.
    fn cmdline_tab(&mut self, forward: bool) {
        let Some(comp) = self.cmdline_completion() else { return };
        let n = comp.cands.len();
        let Some(p) = self.prompt.as_mut() else { return };
        if n == 0 {
            return;
        }
        let base = p.cycle.as_ref().map_or_else(|| p.text.clone(), |(b, _)| b.clone());
        if p.cycle.is_none() && n == 1 {
            let mut text = crate::cmdline::apply(&base, &comp, 0);
            // Trailing space = the next Tab shows what comes next (argument, value). Folders continue inside;
            // commands without arguments stay as is
            let more = if comp.command {
                crate::cmdline::find(&comp.cands[0].insert)
                    .is_some_and(|c| c.arg != crate::cmdline::Arg::None)
            } else {
                !comp.cands[0].dir
            };
            if more {
                text.push(' ');
            }
            p.text = text;
            p.cycle = None;
            return self.cmdline_prefetch();
        }
        let i = match (p.cycle.as_ref(), forward) {
            (None, true) => 0,
            (None, false) => n - 1,
            (Some((_, i)), true) => (i + 1) % n,
            (Some((_, i)), false) => (i + n - 1) % n,
        };
        p.text = crate::cmdline::apply(&base, &comp, i);
        p.cycle = Some((base, i));
    }

    fn handle_prompt_key(&mut self, key: Key) {
        let Some(prompt) = self.prompt.as_mut() else { return };
        match (key.code, key.ctrl || key.alt) {
            (Code::Esc, _) => self.cancel_prompt(),
            (Code::Enter, _) => {
                let Some(Prompt { kind, text, .. }) = self.prompt.take() else { return };
                self.search_origin = None;
                self.submit_prompt(kind, text);
            }
            // `:` completion — Tab·Shift-Tab = cycle and fill, → = accept the dim suggestion
            (Code::Tab | Code::BackTab, false) if prompt.kind == PromptKind::Command => {
                self.cmdline_tab(key.code == Code::Tab);
            }
            (Code::Right, false) if prompt.kind == PromptKind::Command => {
                let comp = self.cmdline_completion();
                if let (Some(p), Some(c)) = (self.prompt.as_mut(), comp)
                    && let Some(g) = crate::cmdline::ghost(&p.text, &c)
                {
                    p.text.push_str(g);
                    p.cycle = None;
                }
            }
            (Code::Backspace, _) => {
                prompt.cycle = None;
                if prompt.text.pop().is_none() {
                    self.cancel_prompt();
                }
            }
            (Code::Char('c'), true) if key.ctrl => self.cancel_prompt(),
            (Code::Char(c), false) => {
                prompt.cycle = None;
                prompt.text.push(c);
            }
            _ => {}
        }
        if self.prompt.as_ref().is_some_and(|p| p.kind == PromptKind::Command) {
            self.cmdline_prefetch();
        }
        // Once ":ask …" starts being typed, pre-spawn claude — typing time hides the cold start.
        if self.prompt.as_ref().is_some_and(|p| p.kind == PromptKind::Command && p.text.starts_with("ask ")) {
            llm::prewarm(self);
        }
    }

    /// Closes the prompt without running it (Esc·C-c·Backspace on empty) — reverts the view moved by the
    /// search preview, so the screen follows the cursor again.
    fn cancel_prompt(&mut self) {
        self.prompt = None;
        if let Some(top) = self.search_origin.take() {
            self.doc_mut().top = top;
        }
    }

    fn submit_prompt(&mut self, kind: PromptKind, text: String) {
        match kind {
            PromptKind::Command => self.run(&[MappableCommand::Typed(text)], None),
            PromptKind::Search { reverse } => {
                // Empty search = repeat the previous search term
                let pat = if text.is_empty() { self.search.clone().unwrap_or_default() } else { text };
                self.with_group(|cx| search::search(cx, &pat, reverse));
            }
            PromptKind::SelectRegex => self.with_group(|cx| search::select_regex(cx, &text)),
            PromptKind::Split => self.with_group(|cx| search::split_selection(cx, &text)),
            PromptKind::Keep { remove } => self.with_group(|cx| search::keep_selections(cx, &text, remove)),
            PromptKind::GlobalSearch => self.global_search(text),
            PromptKind::BreakCondition => self.set_breakpoint_field(false, &text),
            PromptKind::LogMessage => self.set_breakpoint_field(true, &text),
            PromptKind::Watch => self.add_watch(&text),
            PromptKind::Shell(p) => self.shell_pipe(p, &text),
            PromptKind::Replace => self.replace_plan_start(text),
            PromptKind::Note(target) => crate::notes::submit(self, target, &text),
            PromptKind::Rename => {
                if !text.is_empty() {
                    let extra = serde_json::json!({ "newName": text });
                    self.lsp_request(crate::lsp_editor::Kind::Rename, "textDocument/rename", extra);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Feeds a string like "xd" as keys. Angle brackets are named keys, like `<esc>`, `<A-x>`, `<ret>`.
    fn feed(ed: &mut Editor, keys: &str) {
        let mut chars = keys.chars();
        while let Some(c) = chars.next() {
            let key = if c == '<' {
                let name: String = chars.by_ref().take_while(|&c| c != '>').collect();
                name.parse().unwrap()
            } else {
                Key::plain(Code::Char(c))
            };
            ed.handle_key(key);
        }
    }

    fn editor(text: &str) -> Editor {
        let mut ed = Editor::new(Config::default());
        ed.docs[0].text = ropey::Rope::from_str(text);
        ed
    }

    fn run(text: &str, keys: &str) -> Editor {
        let mut ed = editor(text);
        feed(&mut ed, keys);
        ed
    }

    fn text(ed: &Editor) -> String {
        ed.doc().text.to_string()
    }

    #[test]
    fn delete_line() {
        assert_eq!(text(&run("a\nb\n", "xd")), "b\n");
        assert_eq!(text(&run("a\nb\nc\n", "2xd")), "c\n");
        assert_eq!(text(&run("a\nb\nc\n", "xxd")), "c\n");
    }

    #[test]
    fn word_delete_and_change() {
        assert_eq!(text(&run("foo bar", "wd")), "bar");
        assert_eq!(text(&run("foo bar", "ecqux<esc>")), "qux bar");
    }

    #[test]
    fn insert_and_append() {
        assert_eq!(text(&run("", "ihello<esc>")), "hello");
        assert_eq!(text(&run("ab", "aX<esc>")), "aXb");
        assert_eq!(text(&run("ab", "AX<esc>")), "abX");
        assert_eq!(text(&run("  ab", "lllIX<esc>")), "  Xab");
        assert_eq!(text(&run("ab", "i<backspace>x<del><esc>")), "xb");
    }

    #[test]
    fn open_lines_keep_indent() {
        assert_eq!(text(&run("  a\n", "oX<esc>")), "  a\n  X\n");
        assert_eq!(text(&run("  a\n", "OX<esc>")), "  X\n  a\n");
        assert_eq!(text(&run("  a", "A<ret>b<esc>")), "  a\n  b");
    }

    #[test]
    fn multi_cursor_insert() {
        assert_eq!(text(&run("a\nb\nc\n", "Ci-<esc>")), "-a\n-b\nc\n");
        assert_eq!(text(&run("a\nb\nc\n", "2Ci-<esc>")), "-a\n-b\n-c\n");
        assert_eq!(text(&run("a\nb\nc\n", "Ci-<esc>,i+<esc>")), "-a\n-+b\nc\n");
    }

    #[test]
    fn undo_redo_groups() {
        let mut ed = run("a\nb\n", "xd");
        feed(&mut ed, "u");
        assert_eq!(text(&ed), "a\nb\n");
        assert!(!ed.doc().is_modified());
        feed(&mut ed, "U");
        assert_eq!(text(&ed), "b\n");
        // One insert = one undo
        let mut ed = run("", "ihello world<esc>");
        feed(&mut ed, "u");
        assert_eq!(text(&ed), "");
    }

    #[test]
    fn yank_paste() {
        assert_eq!(text(&run("ab", "yp")), "aab");
        assert_eq!(text(&run("ab", "yP")), "aab");
        assert_eq!(text(&run("a\nb\n", "xyp")), "a\na\nb\n");
        assert_eq!(text(&run("a\nb\n", "jxyP")), "a\nb\nb\n");
        // Line-wise paste after a final line with no newline
        assert_eq!(text(&run("a\nb", "xyjp")), "a\nb\na");
        // The pasted text is selected
        let ed = run("ab", "yp");
        assert_eq!(ed.doc().selection().primary(), Range::new(1, 2));
    }

    #[test]
    fn select_mode_extends() {
        assert_eq!(text(&run("abcd", "vlld")), "d");
        assert_eq!(text(&run("foo bar baz", "vwwd")), "baz");
        assert_eq!(run("abcd", "vl<esc>").mode, Mode::Normal);
    }

    #[test]
    fn goto_and_counts() {
        let ed = run("a\nb\nc\n", "ge");
        assert_eq!(ed.doc().selection().primary(), Range::point(4));
        let ed = run("a\nb\nc\n", "2gg");
        assert_eq!(ed.doc().selection().primary(), Range::point(2));
        let ed = run("abcdef", "3l");
        assert_eq!(ed.doc().selection().primary(), Range::point(3));
        let ed = run("abc  \n", "gl");
        assert_eq!(ed.doc().selection().primary(), Range::point(4));
    }

    #[test]
    fn repeat_last_motion() {
        let ed = run("a b c d", "w<A-.><A-.>");
        assert_eq!(ed.doc().selection().primary(), Range::new(4, 6));
    }

    /// `:` completion: Tab cycles and fills candidates; a single one fills up to a trailing space;
    /// → accepts the dim suggestion.
    #[test]
    fn command_line_completion_keys() {
        let mut ed = Editor::new(Config::default());
        let text = |ed: &Editor| ed.prompt.as_ref().map(|p| p.text.clone()).unwrap_or_default();
        feed(&mut ed, ":wr<tab>");
        assert_eq!(text(&ed), "write", "several → the first");
        feed(&mut ed, "<tab>");
        assert_ne!(text(&ed), "write", "pressing again → next candidate");
        feed(&mut ed, "<backtab>");
        assert_eq!(text(&ed), "write", "Shift-Tab goes backward");
        feed(&mut ed, "<esc>:tut<tab>");
        assert_eq!(text(&ed), "tutor", "only one — argless command, no trailing space");
        feed(&mut ed, "<esc>:them<tab>");
        assert_eq!(text(&ed), "theme ", "command with an argument gets a trailing space");
        feed(&mut ed, "<esc>:set editor.auto-sa<tab><tab>");
        assert_eq!(text(&ed), "set editor.auto-save \"off\"", "unique path: trailing space, Tab = value");
        feed(&mut ed, "<esc>:toggle editor.inlay<right>");
        assert_eq!(text(&ed), "toggle editor.inlay-hints", "→ = accept the dim suggestion");
        feed(&mut ed, "<ret>");
        assert!(!ed.config.inlay_hints, "the completed command runs as is");
    }

    /// In normal mode j/k treat a folded doc-comment block as one line (stepping over it without unfolding).
    #[test]
    fn j_and_k_step_over_folded_doc_comments() {
        let mut ed = Editor::new(Config::default());
        ed.docs[0].text = ropey::Rope::from_str("fn a() {}\n/// one\n/// two\n/// three\nfn b() {}\n");
        ed.docs[0].path = Some("/tmp/x.rs".into());
        feed(&mut ed, "j");
        assert_eq!(ed.cursor_line(), 1, "lands on the block (first line)");
        feed(&mut ed, "j");
        assert_eq!(ed.cursor_line(), 4, "past the block");
        feed(&mut ed, "k");
        assert_eq!(ed.cursor_line(), 1, "coming up lands on the block's first line");
        feed(&mut ed, "k");
        assert_eq!(ed.cursor_line(), 0);
        // With rendering off, line by line
        ed.config.render_doc_comments = false;
        feed(&mut ed, "jj");
        assert_eq!(ed.cursor_line(), 2);
    }

    /// Default `X` = line-select upward one line at a time (mirror of `x`) — one more line per press.
    #[test]
    fn default_shift_x_selects_lines_upward() {
        let mut ed = Editor::new(Config::default());
        ed.docs[0].text = ropey::Rope::from_str("a\nb\nc\nd\n");
        feed(&mut ed, "jjj");
        feed(&mut ed, "X");
        let sel = |ed: &Editor| {
            let r = ed.doc().selection().primary();
            ed.doc().text.byte_slice(r.from()..r.to()).to_string()
        };
        assert_eq!(sel(&ed), "c\nd\n");
        feed(&mut ed, "X");
        assert_eq!(sel(&ed), "b\nc\nd\n");
        feed(&mut ed, "d");
        assert_eq!(text(&ed), "a\n");
    }

    #[test]
    fn x_binding_selects_lines_upward() {
        // X = ["extend_line_up", "extend_to_line_bounds"] — line-select upward one line at a time
        let (config, w) =
            crate::config::parse("[keys.normal]\nX = [\"extend_line_up\", \"extend_to_line_bounds\"]\n");
        assert!(w.is_empty());
        let mut ed = Editor::new(config);
        ed.docs[0].text = ropey::Rope::from_str("a\nb\nc\n");
        feed(&mut ed, "jjXd");
        assert_eq!(text(&ed), "a\n");
    }

    #[test]
    fn select_all_and_collapse() {
        assert_eq!(text(&run("abc\ndef", "%d")), "");
        let ed = run("abc", "%;");
        assert_eq!(ed.doc().selection().primary(), Range::point(2));
    }

    #[test]
    fn typed_goto_and_quit_guard() {
        let mut ed = run("a\nb\nc\n", ":2<ret>");
        assert_eq!(ed.doc().selection().primary(), Range::point(2));
        feed(&mut ed, "d:q<ret>");
        assert!(!ed.should_quit);
        assert_eq!(ed.status.as_ref().map(|s| s.1), Some(Severity::Error));
        feed(&mut ed, ":q!<ret>");
        assert!(ed.should_quit);
    }

    /// Waits until one background job finishes and is applied.
    fn settle(ed: &mut Editor) {
        let ev = ed.events.recv_timeout(std::time::Duration::from_secs(5)).expect("job result");
        ed.handle_event(ev);
    }

    /// An sh standing in for `claude` that reads one stdin line and emits a fixed result event.
    fn fake_claude(ed: &mut Editor, reply: &str, is_error: bool) {
        let json = serde_json::json!({"type": "result", "is_error": is_error, "result": reply}).to_string();
        ed.config.llm.command = "sh".into();
        ed.config.llm.args = vec!["-c".into(), format!("read line; printf '%s\\n' '{json}'")];
    }

    /// Processes events until the condition holds (a streamed answer is several events).
    fn settle_until(ed: &mut Editor, done: impl Fn(&Editor) -> bool) {
        let end = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !done(ed) {
            let left = end.saturating_duration_since(std::time::Instant::now());
            let ev = ed.events.recv_timeout(left).expect("event before timeout");
            ed.handle_event(ev);
        }
    }

    /// `|` replaces each selection with the command's output (one undo step), `!`/`A-!` insert output
    /// before/after, `$` keeps the selections the command accepts; a failing command changes nothing.
    #[test]
    fn shell_pipes_on_selections() {
        let done = |ed: &Editor| ed.status.as_ref().is_none_or(|(m, _)| !m.starts_with("running"));
        let mut ed = run("abc def\n", "%s\\w+<ret>|tr a-z A-Z<ret>");
        settle_until(&mut ed, done);
        assert_eq!(text(&ed), "ABC DEF\n");
        assert_eq!(ed.doc().selection().len(), 2, "each selection piped on its own");
        feed(&mut ed, "u");
        assert_eq!(text(&ed), "abc def\n", "one undo step");
        feed(&mut ed, "%s\\w+<ret>$grep b<ret>");
        settle_until(&mut ed, done);
        assert_eq!(ed.doc().selection().len(), 1);
        assert_eq!(selected(&ed), "abc");
        feed(&mut ed, "!printf x<ret>");
        settle_until(&mut ed, done);
        assert_eq!(text(&ed), "xabc def\n");
        feed(&mut ed, ";<A-!>printf y<ret>");
        settle_until(&mut ed, done);
        assert_eq!(text(&ed), "xyabc def\n");
        feed(&mut ed, "%|exit 1<ret>");
        settle_until(&mut ed, done);
        assert_eq!(text(&ed), "xyabc def\n", "failure: nothing changes");
        assert!(ed.status.as_ref().is_some_and(|(m, _)| m.contains("exit 1")), "{:?}", ed.status);
    }

    /// Global search results → `C-r` → replacement → per-file preview → Enter replaces in the files still
    /// listed (one undo step each, open buffers included) → `:wa` saves them.
    #[test]
    fn replace_across_files() {
        let dir = std::env::temp_dir().join(format!("tarae-repl-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let dir = std::fs::canonicalize(&dir).unwrap(); // buffers hold real paths (macOS /private/var)
        let (a, b) = (dir.join("a.txt"), dir.join("b.txt"));
        std::fs::write(&a, "foo one\nbar\nfoo two\n").unwrap();
        std::fs::write(&b, "foo three\n").unwrap();
        let mut ed = editor("");
        let hit = |p: &std::path::Path, line: usize| crate::picker::Item {
            label: format!("{}:{}", p.display(), line + 1),
            action: Action::Goto { path: p.to_path_buf(), line, col: 0 },
            hint: String::new(),
            glyph: None,
        };
        let mut picker = Picker::new("grep /foo/", vec![hit(&a, 0), hit(&a, 2), hit(&b, 0)], false);
        picker.grep = Some("foo".into());
        ed.open_picker(picker, None);
        feed(&mut ed, "<C-r>baz<ret>");
        settle_until(&mut ed, |ed| ed.picker.is_some());
        let p = ed.picker.as_ref().unwrap();
        assert_eq!(p.items().len(), 2, "one row per file");
        assert!(p.title.starts_with("replace 3"), "{}", p.title);
        feed(&mut ed, "<ret>");
        let text_of = |ed: &Editor, p: &std::path::Path| {
            ed.docs.iter().find(|d| d.path.as_deref() == Some(p)).map(|d| d.text.to_string())
        };
        assert_eq!(text_of(&ed, &a).as_deref(), Some("baz one\nbar\nbaz two\n"));
        assert_eq!(text_of(&ed, &b).as_deref(), Some("baz three\n"));
        assert_eq!(std::fs::read_to_string(&a).unwrap(), "foo one\nbar\nfoo two\n", "not saved yet");
        feed(&mut ed, ":wa<ret>");
        assert_eq!(std::fs::read_to_string(&b).unwrap(), "baz three\n");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// `space g`: with the index as the gutter's base, `r` resets a hunk, `s` stages it (it leaves the
    /// gutter and shows in `git diff --cached`), `b` blames the cursor line.
    #[test]
    fn git_menu_reset_stage_blame() {
        let dir = std::env::temp_dir().join(format!("tarae-gitmenu-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let dir = std::fs::canonicalize(&dir).unwrap();
        let git = |args: &[&str]| {
            std::process::Command::new("git").arg("-C").arg(&dir).args(args).output().expect("git").stdout
        };
        git(&["init", "-q"]);
        git(&["config", "user.name", "Kim"]);
        git(&["config", "user.email", "kim@example.com"]);
        let file = dir.join("a.txt");
        std::fs::write(&file, "one\ntwo\nthree\n").unwrap();
        git(&["add", "a.txt"]);
        git(&["commit", "-qm", "first"]);
        let mut ed = editor("");
        ed.git_auto = true;
        ed.open(&file).unwrap();
        settle_until(&mut ed, |ed| ed.doc().git_base.is_some());
        feed(&mut ed, "jmiwcTWO<esc>");
        settle_until(&mut ed, |ed| !ed.doc().git_hunks.is_empty());
        feed(&mut ed, " gr");
        assert_eq!(text(&ed), "one\ntwo\nthree\n", "reset");
        feed(&mut ed, "u");
        settle_until(&mut ed, |ed| ed.doc().git_hunks.len() == 1);
        feed(&mut ed, " gs");
        settle_until(&mut ed, |ed| ed.doc().git_hunks.is_empty());
        let staged = String::from_utf8(git(&["diff", "--cached"])).unwrap();
        assert!(staged.contains("-two") && staged.contains("+TWO"), "{staged}");
        feed(&mut ed, "ggOnew<esc>"); // blame is on by default
        ed.blame_schedule(); // the event loop does this after every event
        settle_until(&mut ed, |ed| ed.blame.of.is_some_and(|(_, v)| v == ed.doc().version()));
        assert_eq!(ed.blame_here(0).as_deref(), Some("not committed yet"));
        assert!(
            ed.blame_here(1).is_some_and(|b| b.starts_with("Kim, ") && b.ends_with("· first")),
            "{:?}",
            ed.blame_here(1)
        );
        // Changed files: each file's diff against HEAD is the preview, +N −M on the right; Enter opens it
        // at its first change
        feed(&mut ed, ":w<ret>");
        ed.git_changed_files_in(dir.clone());
        settle_until(&mut ed, |ed| ed.picker.is_some());
        let p = ed.picker.as_ref().unwrap();
        assert_eq!((p.items()[0].label.as_str(), p.items()[0].hint.as_str()), ("a.txt", "+2 −1"));
        assert_eq!(ed.changed_files[0].line, 0, "the first change");
        assert!(!ed.changed_files[0].diff[0].lines.is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn chat_streams_keeps_one_process_and_applies_code() {
        let dir = std::env::temp_dir().join(format!("tarae-chat-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("stdin.log");
        let delta = |t: &str| {
            serde_json::json!({"type": "stream_event", "event": {"type": "content_block_delta", "delta": {"type": "text_delta", "text": t}}}).to_string()
        };
        let result = serde_json::json!({"type": "result", "result": "done"}).to_string();
        // Fake claude: logs received lines; per line two chunks + a result — one process takes several turns
        let script = format!(
            "while read -r line; do case \"$line\" in '{{\"message\"'*) ;; *) continue;; esac; printf '%s\\n' \"$line\" >> '{}'; printf '%s\\n' '{}' '{}' '{}'; done",
            log.display(),
            delta("Here:\n```\nBAR"),
            delta("\n```\n"),
            result
        );
        let mut ed = editor("foo bar");
        ed.config.llm.command = "sh".into();
        ed.config.llm.args = vec!["-c".into(), script];
        feed(&mut ed, "ww<space>l");
        assert!(ed.chat.as_ref().is_some_and(|c| c.focused), "space l = open and focus");
        feed(&mut ed, "explain<ret>");
        let c = ed.chat.as_ref().unwrap();
        assert_eq!(c.msgs[0].text, "explain");
        assert!(c.busy() && c.input.is_empty());
        settle_until(&mut ed, |ed| !ed.chat.as_ref().unwrap().busy());
        let c = ed.chat.as_ref().unwrap();
        assert_eq!(c.msgs[1].text, "Here:\n```\nBAR\n```\n", "chunks are concatenated");
        assert_eq!(c.last_code().as_deref(), Some("BAR\n"));
        // Second turn: same process; the same file version isn't resent in full
        feed(&mut ed, "again<ret>");
        settle_until(&mut ed, |ed| !ed.chat.as_ref().unwrap().busy());
        let sent = std::fs::read_to_string(&log).unwrap();
        let turns: Vec<String> = sent
            .lines()
            .map(|l| {
                serde_json::from_str::<serde_json::Value>(l).unwrap()["message"]["content"]
                    .as_str()
                    .unwrap()
                    .to_string()
            })
            .collect();
        assert_eq!(turns.len(), 2, "one process received both turns");
        assert!(turns[0].contains("<editor-context>") && turns[0].contains("<file-content>"));
        assert!(
            turns[0].contains("<selection lines=\"1-1\">\nbar\n</selection>"),
            "selection as context: {}",
            turns[0]
        );
        assert!(!turns[1].contains("<file-content>"), "same version only once");
        // C-r: review replacing the selection with the last code block → y
        feed(&mut ed, "<C-r>");
        assert!(ed.review.as_ref().is_some_and(|r| r.ready));
        feed(&mut ed, "y");
        assert_eq!(text(&ed), "foo BAR");
        // esc = back to the editor (pane stays), space L = close
        feed(&mut ed, "<space>l<esc>");
        assert!(ed.chat.as_ref().is_some_and(|c| !c.focused));
        feed(&mut ed, "<space>L");
        assert!(ed.chat.is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Claude reads other files itself: its tool calls become rows, open files (and unsaved text) go in the
    /// context, and `C-g` walks the places it pointed at — newest first — while the chat keeps focus.
    #[test]
    fn chat_explores_the_project() {
        let dir = std::env::temp_dir().join(format!("tarae-chat-explore-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("src")).unwrap();
        let (main, lib) = (dir.join("src/main.rs"), dir.join("src/lib.rs"));
        std::fs::write(&main, "fn main() {}\n").unwrap();
        std::fs::write(&lib, "pub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n").unwrap();
        let lib = std::fs::canonicalize(&lib).unwrap();
        let log = dir.join("stdin.log");
        let tool = serde_json::json!({"type": "assistant", "message": {"content": [
            {"type": "tool_use", "name": "Read", "input": {"file_path": lib, "offset": 2, "limit": 2}}]}});
        let delta = serde_json::json!({"type": "stream_event", "event": {"type": "content_block_delta",
            "delta": {"type": "text_delta", "text": format!("Defined at {}:1.", lib.display())}}});
        let result = serde_json::json!({"type": "result", "result": "done"});
        let script = format!(
            "while read -r line; do case \"$line\" in '{{\"message\"'*) ;; *) continue;; esac; printf '%s\\n' \"$line\" >> '{}'; printf '%s\\n' '{tool}' '{delta}' '{result}'; done",
            log.display()
        );
        let mut ed = editor("");
        ed.open(&lib).unwrap();
        feed(&mut ed, "iX<esc>");
        ed.open(&main).unwrap();
        ed.config.llm.command = "sh".into();
        ed.config.llm.args = vec!["-c".into(), script];
        feed(&mut ed, "<space>lwhere is add?<ret>");
        settle_until(&mut ed, |ed| !ed.chat.as_ref().unwrap().busy());
        let c = ed.chat.as_ref().unwrap();
        let roles: Vec<_> = c.msgs.iter().map(|m| m.role).collect();
        use crate::chat::Role;
        assert_eq!(roles, [Role::User, Role::Tool, Role::Assistant, Role::Note]);
        assert_eq!(c.msgs[1].look.as_ref().unwrap().range, "L2–3");
        let sent = std::fs::read_to_string(&log).unwrap();
        assert!(sent.contains("<open-files>") && sent.contains("lib.rs (unsaved)"), "{sent}");
        assert!(sent.contains("<unsaved-file"), "unsaved text rides along: {sent}");
        // C-g: the answer's reference, then the read before it — the chat keeps the keys
        feed(&mut ed, "<C-g>");
        assert_eq!(ed.doc().path.as_deref(), Some(lib.as_path()));
        assert_eq!(ed.doc().text.byte_to_line(ed.doc().selection().primary().cursor(&ed.doc().text)), 0);
        feed(&mut ed, "<C-g>");
        assert_eq!(ed.doc().text.byte_to_line(ed.doc().selection().primary().cursor(&ed.doc().text)), 1);
        assert!(ed.chat.as_ref().unwrap().focused);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Fake claude that answers every line with `events` (JSON lines; `sleep N` entries pause).
    fn fake_claude_stream(ed: &mut Editor, events: &[serde_json::Value]) {
        let body: Vec<String> = events
            .iter()
            .map(|e| match e.as_str() {
                Some(cmd) => cmd.to_string(),
                None => format!("printf '%s\\n' '{e}'"),
            })
            .collect();
        ed.config.llm.command = "sh".into();
        ed.config.llm.args = vec![
            "-c".into(),
            format!(
                "while read -r line; do case \"$line\" in '{{\"message\"'*) ;; *) continue;; esac; {}; done",
                body.join("; ")
            ),
        ];
    }

    fn follow_project(name: &str) -> (PathBuf, PathBuf, PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!("tarae-follow-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let dir = std::fs::canonicalize(&dir).unwrap();
        let lines = |n: usize| (1..=n).map(|i| format!("line {i}\n")).collect::<String>();
        for (f, n) in [("main.rs", 3), ("util.rs", 5), ("lib.rs", 10)] {
            std::fs::write(dir.join(f), lines(n)).unwrap();
        }
        (dir.clone(), dir.join("main.rs"), dir.join("util.rs"), dir.join("lib.rs"))
    }

    fn cursor_line(ed: &Editor) -> usize {
        ed.doc().text.byte_to_line(ed.doc().selection().primary().cursor(&ed.doc().text))
    }

    /// Follow mode: the editor goes to each file Claude reads, to a search's first hit, and at the end to
    /// the answer's first reference; files it opened on the way close; C-o returns to where you were.
    #[test]
    fn chat_follow_goes_where_claude_reads() {
        use serde_json::json;
        let (dir, main, util, lib) = follow_project("go");
        let delta = |kind: &str, key: &str, t: &str| {
            let mut d = json!({"type": kind});
            d[key] = json!(t);
            json!({"type": "stream_event", "event": {"type": "content_block_delta", "delta": d}})
        };
        let tool = |id: &str, name: &str, input: serde_json::Value| json!({"type": "assistant", "message": {"content": [{"type": "tool_use", "id": id, "name": name, "input": input}]}});
        let result = |id: &str, text: String| json!({"type": "user", "message": {"content": [{"type": "tool_result", "tool_use_id": id, "content": text}]}});
        let mut ed = editor("");
        ed.open(&main).unwrap();
        fake_claude_stream(
            &mut ed,
            &[
                delta("thinking_delta", "thinking", "Checking the helpers first."),
                tool("r1", "Read", json!({"file_path": util})),
                tool("g1", "Grep", json!({"pattern": "line 3", "output_mode": "content"})),
                result("g1", format!("{}:3:line 3", lib.display())),
                delta("text_delta", "text", &format!("Defined at {}:5.", lib.display())),
                json!({"type": "result", "result": "done"}),
            ],
        );
        feed(&mut ed, "<space>lwhere?<ret>");
        settle_until(&mut ed, |ed| !ed.chat.as_ref().unwrap().busy());
        let c = ed.chat.as_ref().unwrap();
        assert!(c.msgs.iter().any(|m| m.role == crate::chat::Role::Thought && m.text.contains("helpers")));
        assert!(c.follow.gaze.is_none(), "the gaze ends with the turn");
        assert_eq!(ed.doc().path.as_deref(), Some(lib.as_path()), "at the answer's reference");
        assert_eq!(cursor_line(&ed), 4);
        assert!(!ed.docs.iter().any(|d| d.path.as_deref() == Some(util.as_path())), "the preview closed");
        assert!(ed.chat.as_ref().unwrap().focused, "the chat keeps the keys");
        feed(&mut ed, "<esc><C-o>");
        assert_eq!(ed.doc().path.as_deref(), Some(main.as_path()), "one jump back = before the turn");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Moving in the editor while Claude explores pauses following — it stays where the user is.
    #[test]
    fn chat_follow_pauses_when_the_user_moves() {
        use serde_json::json;
        let (dir, main, util, lib) = follow_project("pause");
        let read = |id: &str, p: &PathBuf| json!({"type": "assistant", "message": {"content": [{"type": "tool_use", "id": id, "name": "Read", "input": {"file_path": p}}]}});
        let mut ed = editor("");
        ed.open(&main).unwrap();
        fake_claude_stream(
            &mut ed,
            &[
                read("r1", &util),
                json!("sleep 0.5"),
                read("r2", &lib),
                json!({"type": "result", "result": "done"}),
            ],
        );
        feed(&mut ed, "<space>lwhere?<ret>");
        assert!(ed.chat.as_ref().unwrap().follow.active(), "on by default");
        settle_until(&mut ed, |ed| ed.doc().path.as_deref() == Some(util.as_path()));
        feed(&mut ed, "<esc>j");
        settle_until(&mut ed, |ed| !ed.chat.as_ref().unwrap().busy());
        assert!(ed.chat.as_ref().unwrap().follow.paused);
        assert_eq!(ed.doc().path.as_deref(), Some(util.as_path()), "stayed with the user");
        assert_eq!(cursor_line(&ed), 1);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Notes end to end over the control protocol: Claude pins one with the `note` tool (tarae answers the
    /// `mcp_message`), you reply in place with `space n`, Claude answers on the thread with `reply_to`.
    #[test]
    fn notes_pinned_by_claude_and_answered_in_place() {
        use serde_json::json;
        let (dir, main, _, lib) = follow_project("notes");
        let log = dir.join("stdin.log");
        let call = |id: u64, args: serde_json::Value| {
            json!({"type": "control_request", "request_id": format!("c{id}"), "request": {"subtype": "mcp_message",
                "server_name": "tarae", "message": {"jsonrpc": "2.0", "id": id, "method": "tools/call",
                "params": {"name": "note", "arguments": args}}}})
        };
        let tool = |args: serde_json::Value| json!({"type": "assistant", "message": {"content": [{"type": "tool_use", "id": "t", "name": "mcp__tarae__note", "input": args}]}});
        let pin = json!({"path": lib, "line": 3, "text": "Line 3 repeats line 2."});
        let answer = json!({"reply_to": 1, "text": "Because the fixture counts lines."});
        let result = json!({"type": "result", "result": "done"});
        let turn = |events: &[serde_json::Value]| {
            events.iter().map(|e| format!("printf '%s\\n' '{e}'")).collect::<Vec<_>>().join("; ")
        };
        let script = format!(
            "n=0; while read -r line; do printf '%s\\n' \"$line\" >> '{}'; case \"$line\" in '{{\"message\"'*) ;; *) continue;; esac; \
             n=$((n+1)); if [ $n = 1 ]; then {}; else {}; fi; done",
            log.display(),
            turn(&[tool(pin.clone()), call(7, pin), result.clone()]),
            turn(&[tool(answer.clone()), call(8, answer), result]),
        );
        let mut ed = editor("");
        ed.open(&main).unwrap();
        ed.config.llm.command = "sh".into();
        ed.config.llm.args = vec!["-c".into(), script];
        feed(&mut ed, "<space>lany notes?<ret>");
        settle_until(&mut ed, |ed| !ed.chat.as_ref().unwrap().busy());
        assert_eq!(ed.notes.list.len(), 1);
        let n = &ed.notes.list[0];
        assert_eq!(
            (n.path.as_path(), n.lines.clone(), n.gist()),
            (lib.as_path(), 2..3, "Line 3 repeats line 2.")
        );
        // Follow went to the note: its card is under the cursor
        assert_eq!(ed.doc().path.as_deref(), Some(lib.as_path()));
        assert_eq!(crate::notes::here(&ed).map(|n| n.id), Some(1));
        // The fake logs our control_response when it reads it — wait for that
        let logged = |needle: &str| {
            let end = std::time::Instant::now() + std::time::Duration::from_secs(5);
            loop {
                let sent = std::fs::read_to_string(&log).unwrap_or_default();
                if sent.contains(needle) || std::time::Instant::now() > end {
                    return sent;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        };
        let sent = logged("note #1 pinned at");
        assert!(
            sent.lines().next().unwrap().contains("\"sdkMcpServers\":[\"tarae\"]"),
            "registered first: {sent}"
        );
        assert!(sent.contains("\"request_id\":\"c7\"") && sent.contains("note #1 pinned at"), "{sent}");
        // Reply in place — the keys stay in the editor
        feed(&mut ed, "<esc><space>nwhy?<ret>");
        assert!(ed.notes.list[0].waiting);
        assert!(!ed.chat.as_ref().unwrap().focused, "replying doesn't move the keys to the chat");
        settle_until(&mut ed, |ed| !ed.chat.as_ref().unwrap().busy());
        let n = &ed.notes.list[0];
        let thread: Vec<_> = n.thread.iter().map(|e| (e.by, e.text.as_str())).collect();
        use crate::notes::By;
        assert_eq!(
            thread,
            [
                (By::Claude, "Line 3 repeats line 2."),
                (By::You, "why?"),
                (By::Claude, "Because the fixture counts lines.")
            ]
        );
        assert!(!n.waiting);
        let sent = logged("replied on note #1");
        assert!(
            sent.contains("<note id=\\\"1\\\"") && sent.contains("claude: Line 3 repeats line 2."),
            "{sent}"
        );
        assert!(sent.contains("replied on note #1"), "{sent}");
        let c = ed.chat.as_ref().unwrap();
        assert!(c.msgs.iter().any(|m| m.role == crate::chat::Role::User && m.chip.starts_with("¶ #1")));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Asking about a line starts a note with you; Claude's plain answer (no tool) lands in the thread.
    #[test]
    fn a_question_on_a_line_becomes_a_note() {
        use serde_json::json;
        let (dir, main, ..) = follow_project("ask");
        let mut ed = editor("");
        ed.open(&main).unwrap();
        fake_claude_stream(
            &mut ed,
            &[
                json!({"type": "stream_event", "event": {"type": "content_block_delta", "delta": {"type": "text_delta", "text": "It prints line 2."}}}),
                json!({"type": "result", "result": "done"}),
            ],
        );
        feed(&mut ed, "j<space>nwhat is this?<ret>");
        let n = &ed.notes.list[0];
        assert_eq!((n.lines.clone(), n.waiting), (1..2, true));
        assert!(ed.chat.as_ref().is_some_and(|c| !c.focused), "the chat opens without taking the keys");
        settle_until(&mut ed, |ed| !ed.chat.as_ref().unwrap().busy());
        let n = &ed.notes.list[0];
        assert_eq!(n.thread.len(), 2);
        assert_eq!(n.thread[1].text, "It prints line 2.");
        assert!(!n.waiting);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// An older claude that rejects `--thinking-display` is respawned without it — the chat still works.
    #[test]
    fn chat_survives_a_claude_without_thinking_display() {
        let mut ed = editor("fn main() {}\n");
        let result = serde_json::json!({"type": "result", "result": "fine"});
        ed.config.llm.command = "sh".into();
        ed.config.llm.args = vec![
            "-c".into(),
            format!(
                "case \"$*\" in *thinking-display*) echo \"error: unknown option '--thinking-display'\" >&2; exit 1;; esac; \
                 while read -r line; do case \"$line\" in '{{\"message\"'*) ;; *) continue;; esac; printf '%s\\n' '{result}'; done"
            ),
            "sh".into(),
        ];
        feed(&mut ed, "<space>l");
        settle_until(&mut ed, |ed| {
            ed.chat.as_ref().unwrap().proc_alive() && ed.chat.as_ref().unwrap().generation() == 2
        });
        feed(&mut ed, "hi<ret>");
        settle_until(&mut ed, |ed| !ed.chat.as_ref().unwrap().busy());
        let c = ed.chat.as_ref().unwrap();
        assert!(
            c.msgs.iter().any(|m| m.role == crate::chat::Role::Assistant && m.text == "fine"),
            "{:?}",
            c.msgs.iter().map(|m| &m.text).collect::<Vec<_>>()
        );
    }

    /// Several notes on one line: the card shows the first, `]n` walks them one by one, then moves on;
    /// a note just pinned is the one shown.
    #[test]
    fn a_crowded_line_is_read_in_order() {
        let (dir, _, _, lib) = follow_project("crowded");
        let mut ed = editor("");
        ed.open(&lib).unwrap();
        for (line, text) in [(3, "one"), (3, "two"), (3, "three"), (6, "later")] {
            crate::notes::tool_call(&mut ed, &serde_json::json!({"path": lib, "line": line, "text": text}))
                .unwrap();
        }
        let shown = |ed: &Editor| crate::notes::here(ed).map(|n| n.gist().to_string());
        ed.goto_line(2);
        assert_eq!(shown(&ed).as_deref(), Some("one"), "no note stepped to here → the first");
        ed.notes.focus = Some(3);
        assert_eq!(shown(&ed).as_deref(), Some("three"), "just pinned → that one");
        ed.notes.focus = None;
        feed(&mut ed, "]n");
        assert_eq!((ed.cursor_line(), shown(&ed).as_deref()), (2, Some("two")));
        feed(&mut ed, "]n");
        assert_eq!(shown(&ed).as_deref(), Some("three"));
        feed(&mut ed, "]n");
        assert_eq!((ed.cursor_line(), shown(&ed).as_deref()), (5, Some("later")));
        feed(&mut ed, "[n[n");
        assert_eq!(shown(&ed).as_deref(), Some("two"));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// If claude can't be reached, a reply or a new note is taken back instead of waiting forever.
    #[test]
    fn a_note_claude_never_saw_is_taken_back() {
        let (dir, main, ..) = follow_project("unreachable");
        let mut ed = editor("");
        ed.open(&main).unwrap();
        ed.config.llm.command = "/nonexistent/claude".into();
        feed(&mut ed, "<space>nhello<ret>");
        assert!(ed.notes.list.is_empty(), "the new note went away");
        crate::notes::add(&mut ed, &main, 0..1, crate::notes::By::Claude, "hi");
        feed(&mut ed, "<space>nreply<ret>");
        let n = &ed.notes.list[0];
        assert_eq!((n.thread.len(), n.waiting), (1, false), "the reply was taken back");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A note rides its mark: lines added above push it down, and the jump list's pruning keeps it.
    #[test]
    fn notes_follow_edits() {
        let (dir, _, _, lib) = follow_project("edits");
        let mut ed = editor("");
        ed.open(&lib).unwrap();
        let args = serde_json::json!({"path": lib, "line": 5, "end_line": 6, "text": "here"});
        crate::notes::tool_call(&mut ed, &args).unwrap();
        feed(&mut ed, "ggOa<ret>b<esc>");
        ed.push_jump();
        crate::notes::attach(&mut ed);
        assert_eq!(ed.notes.list[0].lines, 6..8);
        feed(&mut ed, "]n");
        assert_eq!(ed.cursor_line(), 6);
        // Across files, in (file, line) order, wrapping
        let util = dir.join("util.rs");
        crate::notes::tool_call(&mut ed, &serde_json::json!({"path": util, "line": 2, "text": "there"}))
            .unwrap();
        feed(&mut ed, "]n");
        assert_eq!(ed.doc().path.as_deref(), Some(util.as_path()), "the next note is in another file");
        feed(&mut ed, "]n");
        assert_eq!((ed.doc().path.as_deref(), ed.cursor_line()), (Some(lib.as_path()), 6), "wraps around");
        feed(&mut ed, "[n");
        assert_eq!(ed.doc().path.as_deref(), Some(util.as_path()));
        ed.open(&lib).unwrap();
        ed.goto_line(6);
        assert!(crate::notes::tool_call(&mut ed, &serde_json::json!({"path": lib, "text": "x"})).is_err());
        assert!(crate::notes::tool_call(&mut ed, &serde_json::json!({"reply_to": 9, "text": "x"})).is_err());
        feed(&mut ed, ":note-close<ret>");
        assert_eq!(ed.notes.list.len(), 1, "only the one here");
        feed(&mut ed, ":notes-clear<ret>");
        assert!(ed.notes.list.is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn splits_focus_scroll_and_close() {
        let mut ed = editor(&"x\n".repeat(100));
        ed.handle_event(crate::event::Event::Resize); // first sync
        feed(&mut ed, "<C-w>v");
        assert_eq!(ed.views.len(), 2);
        assert_eq!(ed.focus, 2, "focus on the new pane");
        // Scroll down in the new pane → the old pane stays put
        ed.doc_mut().top = 50;
        ed.handle_event(crate::event::Event::Resize);
        feed(&mut ed, "<C-w>w");
        assert_eq!(ed.focus, 1);
        assert_eq!(ed.doc().top, 0, "scroll is per pane");
        feed(&mut ed, "<C-w>w");
        assert_eq!(ed.doc().top, 50);
        // :q closes only the pane (quits only on the last pane)
        feed(&mut ed, ":q<ret>");
        assert_eq!(ed.views.len(), 1);
        assert!(!ed.should_quit);
        feed(&mut ed, "<C-w>s<C-w>o");
        assert_eq!(ed.views.len(), 1, "only this pane");
    }

    #[test]
    fn tutor_is_a_throwaway_buffer() {
        let mut ed = editor("");
        feed(&mut ed, ":tutor<ret>");
        assert_eq!(ed.doc().display_name(), "tutor");
        assert!(ed.doc().text.to_string().starts_with("# tarae tutorial"));
        feed(&mut ed, "ihello<esc>:q<ret>");
        assert!(ed.should_quit, "an edited tutorial doesn't block quitting");
    }

    #[test]
    fn remembers_file_positions() {
        let dir = std::env::temp_dir().join(format!("tarae-pos-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.txt");
        std::fs::write(&path, "one\ntwo\nthree\n").unwrap();
        let mut ed = editor("");
        ed.open(&path).unwrap();
        feed(&mut ed, "jjl");
        // Instead of saving and reopening: record the position in session state; a new editor reads it
        let head = ed.doc().selection().primary().head;
        let key = std::fs::canonicalize(&path).unwrap();
        ed.session = crate::session::State::from_json(
            serde_json::json!({ "positions": { key.to_str().unwrap(): [head, 0] } }),
        );
        let session = std::mem::take(&mut ed.session);
        let mut ed2 = editor("");
        ed2.session = session;
        ed2.open(&path).unwrap();
        assert_eq!(ed2.doc().selection().primary().head, head, "back to the last position");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn search_highlights_count_preview_and_esc() {
        let mut ed = editor(&(0..40).map(|i| format!("line {i} foo\n")).collect::<String>());
        // Preview while typing: if the first match after the cursor is off screen, only the view moves
        // (Esc goes back)
        feed(&mut ed, "/line 30");
        ed.refresh_search(10);
        assert_eq!(ed.search_hits.as_ref().map(|h| h.matches.len()), Some(1));
        assert!(ed.doc().top > 20, "view moved to the match");
        feed(&mut ed, "<esc>");
        assert_eq!(ed.doc().top, 0, "Esc = original position");
        // Confirm → highlight all + which match this is
        feed(&mut ed, "/foo<ret>n");
        ed.refresh_search(10);
        let h = ed.search_hits.as_ref().unwrap();
        assert_eq!(h.matches.len(), 40);
        assert_eq!(h.current(ed.doc().selection().primary()), Some(2));
        // Esc in normal mode = highlight off
        feed(&mut ed, "<esc>");
        ed.refresh_search(10);
        assert!(ed.search_hits.is_none());
    }

    #[test]
    fn git_hunks_and_change_navigation() {
        let mut ed = editor("a\nB\nc\nd\nnew\n");
        ed.docs[0].git_base = Some(std::sync::Arc::new("a\nb\nc\nd\n".to_string()));
        ed.git_schedule();
        let ev = ed.events.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        ed.handle_event(ev);
        let kinds: Vec<_> = ed.doc().git_hunks.iter().map(|h| (h.kind, h.lines.clone())).collect();
        assert_eq!(kinds, [(crate::git::HunkKind::Modified, 1..2), (crate::git::HunkKind::Added, 4..5)]);
        feed(&mut ed, "]g");
        assert_eq!(ed.doc().selection().primary(), Range::new(2, 4), "edited line selected");
        feed(&mut ed, "]g");
        assert_eq!(ed.doc().selection().primary(), Range::new(8, 12), "added line");
        feed(&mut ed, "[g");
        assert_eq!(ed.doc().selection().primary(), Range::new(2, 4));
    }

    #[test]
    fn grammar_offer_asks_once_and_answers_only_in_normal_mode() {
        use crate::offer::{OfferState, What};
        let mut ed = editor("fn main() {}\n");
        ed.offer_popups = true;
        let rust = syntax::spec("rust").unwrap();
        ed.offer_grammar(rust);
        let o = ed.offers.first().expect("offer");
        assert_eq!(o.title, "Rust syntax colors");
        let What::Grammars { names, .. } = &o.what else { panic!("grammar offer") };
        assert!(names.contains(&"rust".to_string()));
        assert!(matches!(o.state, OfferState::Asking));
        // In insert mode y·n are characters
        feed(&mut ed, "i");
        feed(&mut ed, "n");
        assert_eq!(ed.offers.len(), 1);
        assert_eq!(ed.doc().text.to_string(), "nfn main() {}\n");
        feed(&mut ed, "<esc>");
        // n in normal mode = not now; the same language isn't asked again
        feed(&mut ed, "n");
        assert!(ed.offers.is_empty());
        ed.offer_grammar(rust);
        assert!(ed.offers.is_empty(), "once per session");
        // A different language is asked — "Not now" via the mouse
        ed.offer_grammar(syntax::spec("python").unwrap());
        assert_eq!(ed.offers.first().map(|o| o.title.as_str()), Some("Python syntax colors"));
        ed.screen.offer_buttons = vec![(20, 50, 59, 'y'), (20, 62, 71, 'n')];
        ed.handle_mouse(crate::event::Mouse {
            kind: crate::event::MouseKind::Down,
            x: 65,
            y: 20,
            alt: false,
        });
        assert!(ed.offers.is_empty());
    }

    /// Multiple offers stack in arrival order and only the front one gets keys — clicking a back card
    /// brings it forward; a card that is downloading doesn't take keys.
    #[test]
    fn offers_stack_in_arrival_order() {
        use crate::offer::OfferState;
        let mut ed = editor("x\n");
        ed.offer_popups = true;
        for l in ["rust", "python", "go"] {
            ed.offer_grammar(syntax::spec(l).unwrap());
        }
        let titles = |ed: &Editor| {
            ed.offers.iter().map(|o| o.title.split(' ').next().unwrap().to_string()).collect::<Vec<_>>()
        };
        assert_eq!(titles(&ed), ["Rust", "Python", "Go"], "new ones go last — front card unchanged");
        // The same offer isn't stacked twice
        let again = ed.offers[1].clone();
        ed.push_offer(again);
        assert_eq!(ed.offers.len(), 3);
        // Clicking a back card (depth 2) brings it forward
        ed.screen.offer_buttons = vec![(10, 40, 70, '2')];
        ed.handle_mouse(crate::event::Mouse {
            kind: crate::event::MouseKind::Down,
            x: 50,
            y: 10,
            alt: false,
        });
        assert_eq!(titles(&ed), ["Go", "Rust", "Python"]);
        feed(&mut ed, "n");
        assert_eq!(titles(&ed), ["Rust", "Python"]);
        // A downloading card in front doesn't take keys · failures go to that card
        let id = ed.offers[0].id;
        ed.offers[0].state = OfferState::Installing(std::time::Instant::now());
        assert!(ed.offer_busy());
        feed(&mut ed, "n");
        assert_eq!(ed.offers.len(), 2, "n doesn't close the card while it's downloading");
        ed.offer_finished(id, Err("no network".into()));
        assert!(matches!(&ed.offers[0].state, OfferState::Failed(w) if w == "no network"));
        ed.offer_finished(id, Ok(()));
        assert_eq!(titles(&ed), ["Python"]);
    }

    #[test]
    fn mouse_click_drag_alt_and_wheel() {
        use crate::event::{Mouse, MouseKind as K};
        let mut ed = editor(&(0..30).map(|i| format!("line {i}\n")).collect::<String>());
        let rect = crate::split::Rect { x: 0, y: 1, w: 80, h: 10 };
        ed.screen = Screen {
            panes: vec![(1, rect, 5, (0..10).map(|l| (l, 0, 0, usize::MAX)).collect())],
            ..Screen::default()
        };
        let at = |kind, x, y| Mouse { kind, x, y, alt: false };
        // Screen (5+2, 3) = line 2, column 2
        ed.handle_mouse(at(K::Down, 7, 3));
        assert_eq!(ed.doc().selection().primary(), Range::point(14 + 2));
        // Drag = from the pressed spot through the last char
        ed.handle_mouse(at(K::Drag, 9, 3));
        assert_eq!(ed.doc().selection().primary(), Range::new(16, 19));
        ed.handle_mouse(at(K::Up, 9, 3));
        // alt+press = add a cursor
        ed.handle_mouse(Mouse { alt: true, ..at(K::Down, 5, 1) });
        assert_eq!(ed.doc().selection().len(), 2);
        // Wheel = 3 lines (the cursor moves too)
        ed.handle_mouse(at(K::ScrollDown, 7, 3));
        assert_eq!(ed.doc().top, 3);
        assert_eq!(mv::line_of(&ed.doc().text, ed.doc().selection().primary().head), 3);
    }

    #[test]
    fn ask_review_accept_then_undo() {
        let mut ed = editor("foo bar");
        fake_claude(&mut ed, "<r i=\"1\">FOO</r>", false);
        feed(&mut ed, "e i");
        assert_eq!(ed.prompt.as_ref().map(|p| p.text.as_str()), Some("ask "));
        feed(&mut ed, "upper<ret>");
        assert!(ed.review.as_ref().is_some_and(|r| !r.ready), "editing isn't blocked while waiting");
        settle(&mut ed);
        assert!(ed.review.as_ref().is_some_and(|r| r.ready));
        feed(&mut ed, "y");
        assert_eq!(text(&ed), "FOO bar");
        assert!(ed.review.is_none());
        feed(&mut ed, "u");
        assert_eq!(text(&ed), "foo bar");
    }

    #[test]
    fn ask_multi_selection_per_change_decisions() {
        let mut ed = editor("foo\nbar\n");
        fake_claude(&mut ed, "<r i=\"1\">X</r>\n<r i=\"2\">Y</r>", false);
        feed(&mut ed, "eC:ask shout<ret>");
        settle(&mut ed);
        feed(&mut ed, "yn");
        assert_eq!(text(&ed), "X\nbar\n");
    }

    #[test]
    fn ask_survives_edits_while_waiting() {
        let mut ed = editor("foo bar");
        fake_claude(&mut ed, "BAR", false);
        feed(&mut ed, "ww:ask upper<ret>");
        feed(&mut ed, "ghizz<esc>");
        settle(&mut ed);
        feed(&mut ed, "a");
        assert_eq!(text(&ed), "zzfoo BAR");
    }

    #[test]
    fn ask_error_is_reported() {
        let mut ed = editor("foo");
        fake_claude(&mut ed, "Not logged in", true);
        feed(&mut ed, ":ask x<ret>");
        settle(&mut ed);
        assert!(ed.review.is_none());
        assert_eq!(ed.status, Some(("claude: Not logged in".into(), Severity::Error)));
    }

    #[test]
    fn large_file_loads_in_background_and_cannot_be_saved_meanwhile() {
        let dir = std::env::temp_dir().join(format!("tarae-load-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("big.txt");
        let body = "0123456789 abcdefghij\n".repeat(60_000); // ≈ 1.3 MB > threshold
        std::fs::write(&path, &body).unwrap();
        let mut ed = editor("");
        ed.open(&path).unwrap();
        assert!(ed.doc().loading, "big files load in the background");
        feed(&mut ed, "d");
        assert!(ed.status.as_ref().is_some_and(|s| s.0.contains("still loading")));
        feed(&mut ed, ":w<ret>");
        assert_eq!(ed.status.as_ref().map(|s| s.1), Some(Severity::Error), "no saving while loading");
        assert_eq!(std::fs::read_to_string(&path).unwrap().len(), body.len(), "the file wasn't emptied");
        settle(&mut ed);
        assert!(!ed.doc().loading);
        assert_eq!(ed.doc().text.len_bytes(), body.len());
        feed(&mut ed, "d");
        assert_eq!(ed.doc().text.len_bytes(), body.len() - 1, "editable once fully read");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn shell_runs_in_background() {
        let mut ed = run("", ":sh echo hi<ret>");
        assert!(ed.status.is_none(), "the result comes from a worker thread");
        settle(&mut ed);
        assert_eq!(ed.status, Some(("hi".to_string(), Severity::Info)));
        let mut ed = run("", ":sh false<ret>");
        settle(&mut ed);
        assert_eq!(ed.status.as_ref().map(|s| s.1), Some(Severity::Error));
    }

    #[test]
    fn config_reload_swaps_keymap() {
        let mut ed = editor("ab");
        let (config, w) = crate::config::parse(
            "[keys.normal]
q = \"delete_selection\"\n",
        );
        ed.reload_config(config, w);
        assert_eq!(ed.status.as_ref().map(|s| s.0.as_str()), Some("config reloaded"));
        feed(&mut ed, "q");
        assert_eq!(text(&ed), "b");
    }

    #[test]
    fn search_and_repeat() {
        let ed = run("foo bar foo baz", "/foo<ret>");
        assert_eq!(ed.doc().selection().primary(), Range::new(8, 11));
        let ed = run("foo bar foo baz", "/foo<ret>n");
        assert_eq!(ed.doc().selection().primary(), Range::new(0, 3));
        assert_eq!(ed.status.as_ref().map(|s| s.0.as_str()), Some("search wrapped around"));
        let ed = run("foo bar foo baz", "/ba.<ret>nN");
        assert_eq!(ed.doc().selection().primary(), Range::new(4, 7));
        // *: selection as search term → n
        let ed = run("foo bar foo baz", "e*n");
        assert_eq!(ed.doc().selection().primary(), Range::new(8, 11));
        // n in select mode adds
        let ed = run("foo bar foo baz", "e*vn");
        assert_eq!(ed.doc().selection().len(), 2);
    }

    #[test]
    fn select_regex_then_edit_all() {
        // Core helix flow: select all → s selects every match → c changes them all at once
        assert_eq!(
            text(&run("let a = 1;\nlet b = 2;\n", "%slet<ret>cconst<esc>")),
            "const a = 1;\nconst b = 2;\n"
        );
        assert_eq!(text(&run("a, b,c", "%S,\\s*<ret>i-<esc>")), "-a, -b,-c");
        assert_eq!(text(&run("x1\ny2\nx3\n", "%<A-s>Kx<ret>d")), "\ny2\n\n");
        assert_eq!(text(&run("x1\ny2\nx3\n", "%<A-s><A-K>x<ret>d")), "x1\n\nx3\n");
        // No match → selection kept + error
        let ed = run("abc", "%szzz<ret>");
        assert_eq!(ed.doc().selection().primary(), Range::new(0, 3));
        assert_eq!(ed.status.as_ref().map(|s| s.1), Some(Severity::Error));
    }

    #[test]
    fn find_char_family() {
        assert_eq!(run("a,b,c,d", "f,").doc().selection().primary(), Range::new(0, 2));
        assert_eq!(run("a,b,c,d", "2f,").doc().selection().primary(), Range::new(0, 4));
        assert_eq!(run("a,b,c,d", "t,").doc().selection().primary(), Range::new(0, 3));
        assert_eq!(run("a,b,c", "glF,").doc().selection().primary(), Range::new(5, 3));
        assert_eq!(text(&run("a,b,c,d", "fcd")), ",d");
        // A-. remembers the target char too
        assert_eq!(run("a,b,c,d", "f,<A-.>").doc().selection().primary(), Range::new(1, 4));
        // esc cancels the awaited char
        assert_eq!(text(&run("abc", "f<esc>d")), "bc");
    }

    #[test]
    fn replace_case_indent_join() {
        assert_eq!(text(&run("abc", "lrx")), "axc");
        assert_eq!(text(&run("ab\ncd", "%rx")), "xx\nxx");
        assert_eq!(text(&run("aBc", "%~")), "AbC");
        assert_eq!(text(&run("aBc", "%`")), "abc");
        assert_eq!(text(&run("aBc", "%<A-`>")), "ABC");
        assert_eq!(text(&run("a\n  b\n\nc\n", "%>")), "    a\n      b\n\n    c\n");
        assert_eq!(text(&run("    a\n  b\n\tc\n", "%<lt>")), "a\nb\nc\n");
        assert_eq!(text(&run("a\n   b\nc\n", "J")), "a b\nc\n");
        assert_eq!(text(&run("a\nb\nc\n", "%J")), "a b c\n");
    }

    #[test]
    fn named_and_blackhole_registers() {
        assert_eq!(text(&run("ab", "\"ayl\"ap")), "aba");
        let ed = run("ab", "\"ayp");
        assert_eq!(ed.status.as_ref().map(|s| s.1), Some(Severity::Error), "the default register is empty");
        // Deleting with _ keeps the default register (the previous y) alive
        assert_eq!(text(&run("ab", "yl\"_dP")), "aa");
    }

    #[test]
    fn clipboard_roundtrip() {
        let mut ed = run("ab", " y");
        settle(&mut ed); // write job done
        feed(&mut ed, "l p");
        settle(&mut ed); // read → paste
        assert_eq!(text(&ed), "aba");
    }

    #[test]
    fn macros_record_and_replay() {
        assert_eq!(text(&run("a\nb\nc\n", "QA;<esc>jQqq")), "a;\nb;\nc;\n");
        assert_eq!(text(&run("a\nb\nc\n", "QA;<esc>jQ2q")), "a;\nb;\nc;\n");
        assert_eq!(text(&run("a\nb\n", "\"xQA!<esc>jQ\"xq")), "a!\nb!\n");
    }

    #[test]
    fn graphemes_move_and_delete_as_one() {
        assert_eq!(text(&run("e\u{301}x", "d")), "x");
        assert_eq!(run("e\u{301}x", "l").doc().selection().primary(), Range::point(3)); // é = 3 B
        assert_eq!(text(&run("x👍🏽", "A<backspace><esc>")), "x");
        assert_eq!(text(&run("👍🏽x", "i<del><esc>")), "x");
        assert_eq!(text(&run("👍🏽x", "rz")), "zx", "r treats one cluster as one char");
    }

    #[test]
    fn sticky_column() {
        assert_eq!(run("abcdef\nab\nabcdef", "5ljj").doc().selection().primary(), Range::point(15));
        // Wide chars: below '글' (display column 2) is 'c' (bytes: 한글자 9 + newline 1 + ab 2)
        assert_eq!(run("한글자\nabcdef", "lj").doc().selection().primary(), Range::point(12));
        // A non-vertical command in between resets the column
        assert_eq!(run("abcdef\nab\nabcdef", "5ljhj").doc().selection().primary(), Range::point(11));
    }

    /// Byte-position safety net: 4×1000 pseudo-random keys over text mixing Hangul, emoji, combining
    /// marks and CRLF — no panics, and every selection endpoint always stays on a char boundary.
    #[test]
    fn stress_positions_stay_on_char_boundaries() {
        const KEYS: &[&str] = &[
            "h",
            "j",
            "k",
            "l",
            "w",
            "b",
            "e",
            "x",
            "X",
            "d",
            "u",
            "U",
            "y",
            "p",
            "P",
            "C",
            ",",
            ";",
            "%",
            "J",
            ">",
            "<lt>",
            "~",
            "v",
            "<esc>",
            "gl",
            "gh",
            "ge",
            "gg",
            "<C-o>",
            "<tab>",
            "<C-s>",
            ".",
            "<C-c>",
            "i(\"'<esc>",
            "A<C-w><esc>",
            "i<A-d><esc>",
            "i<C-u><esc>",
            "a<C-k><esc>",
            "i(<backspace><esc>",
            "]f",
            "[f",
            "]c",
            "<A-o>",
            "<A-i>",
            "<A-n>",
            "<A-p>",
            "]p",
            "[p",
            "] ",
            "[ ",
            "fa",
            "t한",
            "F,",
            "r*",
            "i한<esc>",
            "a👍🏽<esc>",
            "o<esc>",
            "<A-;>",
            "<A-C>",
            "A<backspace><esc>",
            "i<del><esc>",
            "2l",
            "3w",
        ];
        let base = "한글 abc 👨\u{200d}👩\u{200d}👧 e\u{301}t,\r\n\t타래 fn(x) 👍🏽\n마지막\n";
        for mut seed in [0x7a2a_e5ee_d000_0001u64, 1, 42, 0xdead_beef] {
            let mut ed = editor(&base.repeat(3));
            for step in 0..1000 {
                seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                let k = KEYS[(seed >> 33) as usize % KEYS.len()];
                feed(&mut ed, k);
                let t = &ed.doc().text;
                for r in ed.doc().selection().ranges() {
                    for b in [r.anchor, r.head] {
                        assert!(
                            b <= t.len_bytes() && t.char_to_byte(t.byte_to_char(b)) == b,
                            "step {step} key {k:?}: {b} is not a char boundary"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn match_mode() {
        assert_eq!(text(&run("f(a, (b), c)", "7lmi(d")), "f(a, (), c)");
        assert_eq!(text(&run("f(a, (b), c)", "7lma(d")), "f(a, , c)");
        assert_eq!(text(&run("say \"타래\" ok", "5lmi\"cX<esc>")), "say \"X\" ok");
        assert_eq!(text(&run("foo bar baz", "4lmawd")), "foo baz");
        assert_eq!(run("f(a(b))", "lmm").doc().selection().primary(), Range::point(6));
        // Surround: add · replace · delete
        assert_eq!(text(&run("word", "miwms(")), "(word)");
        assert_eq!(text(&run("(word)", "2lmr([")), "[word]");
        assert_eq!(text(&run("'word'", "2lmd'")), "word");
        // In a file without a grammar, `mif` says so
        let ed = run("fn x() {}", "mif");
        assert!(ed.status.as_ref().is_some_and(|s| s.0.contains("needs syntax")));
    }

    #[test]
    fn buffer_picker_switches() {
        let mut ed = editor("first");
        ed.new_scratch();
        ed.doc_mut().text = ropey::Rope::from_str("second");
        assert_eq!(text(&ed), "second");
        // From the second buffer, the list's first row = the first document → select it to switch
        feed(&mut ed, " b");
        assert!(ed.picker.is_some());
        feed(&mut ed, "<ret>");
        assert!(ed.picker.is_none());
        assert_eq!(text(&ed), "first");
        feed(&mut ed, " b<esc>");
        assert!(ed.picker.is_none(), "closes with esc");
    }

    #[test]
    fn file_picker_and_global_search_open_files() {
        // On this repo itself: the file list fills, and a search result jumps to its line.
        let mut ed = editor("");
        feed(&mut ed, " f");
        settle(&mut ed);
        let p = ed.picker.as_ref().unwrap();
        assert!(!p.loading && p.counts().1 > 10, "file list {:?}", p.counts());
        feed(&mut ed, "selection.rs<ret>");
        assert!(
            ed.doc().path.as_ref().is_some_and(|p| p.ends_with("src/selection.rs")),
            "{:?}",
            ed.doc().path
        );
        feed(&mut ed, " /fn put_cursor<ret>");
        // Other jobs such as grammar loading are queued too — keep going until the search result arrives.
        while ed.picker.as_ref().is_some_and(|p| p.loading) {
            settle(&mut ed);
        }
        feed(&mut ed, "<ret>");
        let doc = ed.doc();
        let pos = doc.selection().primary().head;
        let line = doc.text.line(mv::line_of(&doc.text, pos)).to_string();
        assert!(line.contains("fn put_cursor"), "{line:?}");
    }

    #[test]
    fn prompt_backspace_on_empty_closes() {
        let ed = run("", ":<backspace>");
        assert!(ed.prompt.is_none());
    }

    /// NBSP (Option+Space on macOS)·U+3000 are multi-byte whitespace — completion must not slice mid-char.
    #[test]
    fn command_line_takes_multibyte_whitespace() {
        let mut ed = editor("");
        feed(&mut ed, ":o a\u{a0}b");
        assert!(ed.cmdline_completion().is_some());
        feed(&mut ed, "<esc>:theme\u{3000}m");
        assert!(ed.cmdline_completion().is_some());
    }

    /// Esc·C-c·Backspace on empty all close the `/` prompt the same way — the view returns and follows
    /// the cursor again. A `:` prompt doesn't preview the last search.
    #[test]
    fn every_way_out_of_search_restores_the_view() {
        let body: String = (0..40).map(|i| format!("line {i} foo\n")).collect();
        for close in ["<esc>", "<C-c>", &"<backspace>".repeat(8)] {
            let mut ed = editor(&body);
            feed(&mut ed, "/line 30");
            ed.refresh_search(10);
            assert!(ed.doc().top > 20);
            feed(&mut ed, close);
            assert!(ed.prompt.is_none(), "{close}");
            assert!(ed.search_origin.is_none(), "{close}: view follows the cursor again");
            assert_eq!(ed.doc().top, 0, "{close}");
        }
        let mut ed = editor(&body);
        feed(&mut ed, "/line 30<ret>gg:");
        ed.refresh_search(10);
        assert!(ed.search_origin.is_none(), "`:` isn't a search preview");
    }

    /// Moving focus to another document's pane in insert mode — each document's edits undo on their own.
    #[test]
    fn insert_edits_stay_with_their_document_across_panes() {
        let mut ed = editor("aaa");
        ed.handle_event(crate::event::Event::Resize);
        feed(&mut ed, ":vs<ret>:n<ret>");
        ed.handle_event(crate::event::Event::Resize); // focused pane shows the new scratch
        feed(&mut ed, "iB");
        ed.focus_view(1);
        feed(&mut ed, "x<esc>u");
        assert_eq!(text(&ed), "aaa", "x undone in its own document");
        ed.focus_view(2);
        assert_eq!(text(&ed), "B");
        feed(&mut ed, "u");
        assert_eq!(text(&ed), "");
    }

    /// Insert-mode sequence that breaks off (`j k` bound, `jx` typed) = the keys are typed, the last one
    /// goes through the keymap again.
    #[test]
    fn broken_insert_sequence_types_its_keys() {
        let run_jk = |keys: &str| {
            let (config, w) = crate::config::parse("[keys.insert]\nj = { k = \"normal_mode\" }\n");
            assert!(w.is_empty(), "{w:?}");
            let mut ed = Editor::new(config);
            feed(&mut ed, keys);
            ed
        };
        let ed = run_jk("ijx");
        assert_eq!((text(&ed), ed.mode), ("jx".into(), Mode::Insert));
        let ed = run_jk("ijk");
        assert_eq!((text(&ed), ed.mode), ("".into(), Mode::Normal));
        let ed = run_jk("ij<esc>");
        assert_eq!((text(&ed), ed.mode), ("j".into(), Mode::Normal), "Esc still leaves");
        let ed = run_jk("ijjk");
        assert_eq!((text(&ed), ed.mode), ("j".into(), Mode::Normal), "second j starts the sequence anew");
        assert_eq!(text(&run_jk("ijx<esc>u")), "", "one undo unit");
    }

    /// A file opened over the startup scratch shows in every pane that showed the scratch; a failed
    /// background load closes its buffer like `:bc` (panes move off it).
    #[test]
    fn opening_over_the_scratch_and_failed_loads_keep_panes_valid() {
        let dir = std::env::temp_dir().join(format!("tarae-panes-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.txt");
        std::fs::write(&path, "hello\n").unwrap();
        let mut ed = editor("");
        ed.handle_event(crate::event::Event::Resize);
        feed(&mut ed, &format!(":vs {}<ret>", path.display()));
        ed.handle_event(crate::event::Event::Resize);
        let id = ed.doc().id;
        assert!(ed.views.iter().all(|v| v.doc == id), "both panes show a.txt");
        // A big file that fails to read
        let gone = ed.alloc_id();
        ed.docs.push(Document::placeholder(gone, &dir.join("big.txt")));
        ed.current = 1;
        ed.handle_event(crate::event::Event::Resize);
        ed.finish_loading(gone, Err(anyhow::anyhow!("boom")), 0);
        assert!(ed.docs.iter().all(|d| d.id != gone));
        assert!(ed.views.iter().all(|v| v.doc == id), "panes moved back to a.txt");
        assert_eq!(ed.doc().id, id);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// `.` repeats the last insert: the command that entered insert mode, then what was typed — at the
    /// current selections, with a count.
    #[test]
    fn dot_repeats_the_last_insert() {
        assert_eq!(text(&run("a\nb\n", "Ahi<esc>j.")), "ahi\nbhi\n");
        assert_eq!(
            text(&run("one\ntwo\n", "ecX<esc>jghe.")),
            "X\nX\n",
            "change replays on the new selection"
        );
        assert_eq!(text(&run("a\n", "Ax<esc>2.")), "axxx\n");
        assert_eq!(text(&run("a\n", "Ax<esc>.u")), "ax\n", "one undo step per repeat");
        assert_eq!(text(&run("a\n", "A x<C-w>y<esc>.")), "a y y\n", "editing keys replay too");
        assert_eq!(text(&run("a\n", ".")), "a\n", "nothing yet");
    }

    fn rust_editor(text: &str) -> Editor {
        let mut ed = editor(text);
        ed.docs[0].path = Some("/tmp/tarae-test.rs".into());
        ed
    }

    #[test]
    fn auto_pairs_in_insert_mode() {
        assert_eq!(text(&run("\n", "i(x)<esc>")), "(x)\n", "closer added, then stepped over");
        assert_eq!(text(&run("\n", "i(<backspace><esc>")), "\n", "backspace removes both");
        assert_eq!(text(&run("\n", "idon't<esc>")), "don't\n");
        assert_eq!(text(&run("\n", "i\"a\"<esc>")), "\"a\"\n");
        assert_eq!(text(&run("x\n", "i(<esc>")), "(x\n", "before a word char: no closer");
        let mut ed = rust_editor("\n");
        feed(&mut ed, "i&'a<esc>");
        assert_eq!(text(&ed), "&'a\n", "Rust lifetimes don't pair");
        let mut ed = editor("\n");
        ed.config.auto_pairs = false;
        feed(&mut ed, "i(<esc>");
        assert_eq!(text(&ed), "(\n");
    }

    #[test]
    fn insert_mode_editing_keys() {
        assert_eq!(text(&run("foo bar\n", "A<C-w><esc>")), "foo \n");
        assert_eq!(text(&run("foo bar\n", "A<C-w><C-w><esc>")), "\n");
        assert_eq!(text(&run("a\nb\n", "jI<C-w><esc>")), "ab\n", "at a line start: the line break");
        assert_eq!(text(&run("foo.bar\n", "A<A-backspace><esc>")), "foo.\n", "punctuation is its own run");
        assert_eq!(text(&run("foo bar\n", "i<A-d><esc>")), " bar\n");
        assert_eq!(text(&run("    foo bar\n", "A<C-u><esc>")), "    \n", "to the indentation first");
        assert_eq!(text(&run("    foo\n", "A<C-u><C-u><esc>")), "\n");
        assert_eq!(text(&run("a\nb\n", "jI<C-u><esc>")), "ab\n");
        assert_eq!(text(&run("foo bar\n", "i<C-k><esc>")), "\n");
        assert_eq!(text(&run("a\nb\n", "i<C-k><C-k><esc>")), "b\n", "at the line end: joins");
        assert_eq!(text(&run("abc\n", "yA<C-r>\"<esc>")), "abca\n");
        assert_eq!(text(&run("\n", "iab<C-s>cd<esc>u")), "ab\n", "C-s = undo step");
        assert_eq!(text(&run("abc\n", "A<home>X<esc>")), "Xabc\n");
        assert_eq!(text(&run("abc\n", "i<end>X<esc>")), "abcX\n");
        assert_eq!(text(&run("ab\n", "A<C-h><C-j>x<esc>")), "a\nx\n");
    }

    #[test]
    fn c_c_toggles_line_comments() {
        let mut ed = rust_editor("fn a() {\n    x();\n}\n");
        feed(&mut ed, "jgs<C-c>");
        assert_eq!(text(&ed), "fn a() {\n    // x();\n}\n");
        assert_eq!(
            ed.doc().text.byte_slice(ed.doc().selection().primary().from()..).chars().next(),
            Some('x'),
            "the cursor stays on its char"
        );
        feed(&mut ed, "<C-c>");
        assert_eq!(text(&ed), "fn a() {\n    x();\n}\n");
        feed(&mut ed, "%<C-c>");
        assert_eq!(text(&ed), "// fn a() {\n//     x();\n// }\n");
        feed(&mut ed, "u");
        assert_eq!(text(&ed), "fn a() {\n    x();\n}\n", "one undo step");
        let ed = run("x\n", "<C-c>");
        assert_eq!(text(&ed), "x\n", "unknown language: nothing");
    }

    /// A Rust document with its tree parsed now — None when the grammar isn't built in.
    fn parsed_rust(text: &str) -> Option<Editor> {
        let lang = syntax::Loader::global().load(syntax::spec("rust")?).ok()?;
        let mut ed = rust_editor(text);
        let mut syn = syntax::Syntax::new(lang);
        let job = syn.start_parse(&ed.docs[0].text);
        let generation = job.generation;
        let tree = job.run();
        syn.finish_parse(generation, tree);
        ed.docs[0].syntax = Some(syn);
        Some(ed)
    }

    fn selected(ed: &Editor) -> String {
        let (doc, r) = (ed.doc(), ed.doc().selection().primary());
        doc.text.byte_slice(r.from()..r.to()).to_string()
    }

    const STRUCT_SRC: &str = "fn a(x: u8, y: u8) {\n    call(x);\n}\n\n// note\nfn b() {}\n";

    /// `]f` `[f` `]c` select the next/previous textobject; `C-o` comes back.
    #[test]
    fn bracket_keys_select_textobjects() {
        let Some(mut ed) = parsed_rust(STRUCT_SRC) else { return };
        feed(&mut ed, "]f");
        assert_eq!(selected(&ed), "fn b() {}");
        feed(&mut ed, "[f");
        assert_eq!(selected(&ed), "fn a(x: u8, y: u8) {\n    call(x);\n}");
        feed(&mut ed, "]c");
        assert_eq!(selected(&ed), "// note");
        feed(&mut ed, "<C-o>");
        assert_eq!(selected(&ed), "fn a(x: u8, y: u8) {\n    call(x);\n}", "a jump");
    }

    /// `A-o` grows by syntax node, `A-i` retraces it; `A-n`/`A-p` step through siblings.
    #[test]
    fn alt_o_grows_and_alt_i_retraces() {
        let Some(mut ed) = parsed_rust(STRUCT_SRC) else { return };
        let x = STRUCT_SRC.find("x)").unwrap();
        ed.docs[0].set_selection(Selection::point(x));
        feed(&mut ed, "<A-o><A-o>");
        assert_eq!(selected(&ed), "call(x)");
        feed(&mut ed, "<A-i><A-i>");
        assert_eq!(ed.doc().selection().primary(), Range::point(x), "back where it started");
        ed.docs[0].set_selection(Selection::point(STRUCT_SRC.find("x:").unwrap()));
        feed(&mut ed, "<A-o><A-n>");
        assert_eq!(selected(&ed), "y: u8");
        feed(&mut ed, "<A-p>");
        assert_eq!(selected(&ed), "x: u8");
        let mut plain = editor("x\n");
        feed(&mut plain, "<A-o>");
        assert!(plain.status.as_ref().is_some_and(|(m, _)| m.contains("tree-sitter")));
    }

    #[test]
    fn paragraphs_and_blank_lines() {
        let line = |ed: &Editor| ed.doc().text.byte_to_line(ed.doc().selection().primary().head);
        assert_eq!(line(&run("a\nb\n\nc\n", "]p")), 3);
        assert_eq!(line(&run("a\nb\n\nc\n", "]p[p")), 0);
        let ed = run("a\nb\n", "] ");
        assert_eq!((text(&ed), line(&ed)), ("a\n\nb\n".into(), 0), "cursor stays");
        let ed = run("a\nb\n", "j2[ ");
        assert_eq!((text(&ed), line(&ed)), ("a\n\n\nb\n".into(), 3), "moves down with its line");
    }

    /// `C-o` returns from big moves (`ge`, `gg`, search) and `C-i`/Tab goes forward again; a spot
    /// follows edits made after leaving it; `C-s` saves a spot by hand.
    #[test]
    fn jumplist_back_and_forth() {
        let head = |ed: &Editor| ed.doc().selection().primary().cursor(&ed.doc().text);
        let line = |ed: &Editor| ed.doc().text.byte_to_line(head(ed));
        let ed = run("one\ntwo\nthree\nfour\n", "jge<C-o>");
        assert_eq!(line(&ed), 1, "back where ge started");
        let mut ed = run("one\ntwo\nthree\nfour\n", "jge<C-o><tab>");
        assert_eq!(line(&ed), 3, "Tab = forward again");
        feed(&mut ed, "<C-i>");
        assert_eq!(line(&ed), 3, "nothing further ahead");
        let ed = run("one\ntwo\nthree\nfour\n", "/fo<ret>/tw<ret><C-o><C-o>");
        assert_eq!(line(&ed), 0, "two searches back");
        // Lines added above the saved spot: it moves with its text
        let ed = run("one\ntwo\nthree\n", "jjggOx<ret>y<esc><C-o>");
        assert_eq!(ed.doc().text.line(line(&ed)).to_string(), "three\n");
        let ed = run("one\ntwo\nthree\n", "j<C-s>jj<C-o>");
        assert_eq!(line(&ed), 1, "C-s spot");
        // A new jump after going back drops the forward branch
        let ed = run("a\nb\nc\nd\n", "jgej<C-o>gg<tab>");
        assert_eq!(line(&ed), 0, "nothing ahead of gg any more");
    }

    /// `space j` lists the pane's jumps; picking one goes there (and `Tab` comes back). `space '` reopens
    /// the last picker as it was left.
    #[test]
    fn jumplist_picker_and_reopening_the_last_picker() {
        let line = |ed: &Editor| ed.doc().text.byte_to_line(ed.doc().selection().primary().head);
        let mut ed = run("a\nb\nc\nd\n", "jge");
        feed(&mut ed, " j");
        let p = ed.picker.as_ref().expect("jump list open");
        assert_eq!((p.title.as_str(), p.items().len()), ("jumps", 1));
        assert!(p.items()[0].label.ends_with(":2"), "{}", p.items()[0].label);
        feed(&mut ed, "<ret>");
        assert_eq!(line(&ed), 1);
        feed(&mut ed, "<tab>");
        assert_eq!(line(&ed), 3, "the spot left is kept");
        feed(&mut ed, " jd<esc> '");
        let p = ed.picker.as_ref().expect("reopened");
        assert_eq!((p.title.as_str(), p.query.as_str()), ("jumps", "d"));
        feed(&mut ed, "<esc>");
        let mut fresh = editor("x\n");
        feed(&mut fresh, " '");
        assert!(fresh.picker.is_none());
    }

    /// The file picker's tree: it starts on the file being edited (its folders open); ← goes to the folder,
    /// Enter folds it, → opens it again; typing switches to the flat fuzzy list, emptying the query back.
    #[test]
    fn file_picker_tree() {
        let root = PathBuf::from("/r");
        let item = |r: &str| crate::picker::Item {
            label: r.into(),
            action: Action::Open(root.join(r)),
            hint: String::new(),
            glyph: None,
        };
        let mut ed = editor("");
        let picker =
            Picker::new("files", Vec::new(), true).with_tree(root.clone(), Some("src/main.rs".into()));
        ed.open_picker(picker, None);
        ed.picker.as_mut().unwrap().set_items(vec![
            item("README.md"),
            item("src/main.rs"),
            item("src/lib.rs"),
        ]);
        let cur = |ed: &Editor| ed.picker.as_ref().and_then(|p| p.current()).map(|i| i.label.clone());
        let n = |ed: &Editor| ed.picker.as_ref().unwrap().counts().0;
        assert_eq!(cur(&ed).as_deref(), Some("  main.rs"), "on the file being edited");
        assert_eq!(n(&ed), 4, "src/ open: src/, lib.rs, main.rs, README.md");
        feed(&mut ed, "<left>");
        assert_eq!(cur(&ed).as_deref(), Some("src/"));
        feed(&mut ed, "<ret>");
        assert_eq!((cur(&ed).as_deref(), n(&ed)), (Some("src/"), 2), "folded, still on it");
        feed(&mut ed, "<right>");
        assert_eq!(n(&ed), 4);
        feed(&mut ed, "lib");
        assert_eq!(cur(&ed).as_deref(), Some("src/lib.rs"), "typing: the flat list");
        feed(&mut ed, "<backspace><backspace><backspace>");
        assert_eq!(n(&ed), 4, "the tree again");
    }

    /// Switching files is a jump too (`C-o` returns to the previous file at its spot); `ga` toggles
    /// between the last two files; closing a file drops its jumps.
    #[test]
    fn jumplist_across_files_and_last_accessed() {
        let dir = std::env::temp_dir().join(format!("tarae-jumps-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (a, b) = (dir.join("a.txt"), dir.join("b.txt"));
        std::fs::write(&a, "a1\na2\na3\n").unwrap();
        std::fs::write(&b, "b1\nb2\n").unwrap();
        let mut ed = editor("");
        let name = |ed: &Editor| ed.doc().display_name_short();
        feed(&mut ed, &format!(":o {}<ret>jj:o {}<ret>j", a.display(), b.display()));
        assert_eq!(name(&ed), "b.txt");
        feed(&mut ed, "<C-o>");
        assert_eq!(name(&ed), "a.txt");
        assert_eq!(ed.doc().text.byte_to_line(ed.doc().selection().primary().head), 2, "at its spot");
        feed(&mut ed, "<tab>");
        assert_eq!(name(&ed), "b.txt");
        assert_eq!(ed.doc().text.byte_to_line(ed.doc().selection().primary().head), 1);
        feed(&mut ed, "ga");
        assert_eq!(name(&ed), "a.txt");
        feed(&mut ed, "ga");
        assert_eq!(name(&ed), "b.txt");
        feed(&mut ed, ":bc<ret>");
        assert_eq!(name(&ed), "a.txt");
        assert!(ed.views[0].jumps.iter().all(|j| j.doc == ed.doc().id), "b's jumps went with it");
        feed(&mut ed, "ga<C-o>");
        assert_eq!(name(&ed), "a.txt");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Big files (loaded in the background) also return to the last position.
    #[test]
    fn background_load_restores_position() {
        let mut ed = editor("");
        let path = PathBuf::from("/nonexistent/tarae-big.txt");
        ed.session = crate::session::State::from_json(
            serde_json::json!({ "positions": { path.to_str().unwrap(): [4, 1] } }),
        );
        let id = ed.alloc_id();
        ed.docs.push(Document::placeholder(id, &path));
        ed.current = 1;
        ed.finish_loading(id, Ok(ropey::Rope::from_str("one\ntwo\nthree\n")), 0);
        assert_eq!(ed.doc().selection().primary().head, 4);
        assert_eq!(ed.doc().top, 1);
    }

    /// Recording while replaying another macro keeps only the replaying keys (not their expansion too),
    /// and that macro replays the other one again. A macro never replays itself.
    #[test]
    fn recording_skips_keys_a_replay_feeds_in() {
        let ed = run("a\nb\nc\n", "\"bQA!<esc>Qj\"aQ\"bqQj\"aq");
        assert_eq!(text(&ed), "a!\nb!\nc!\n");
        assert_eq!(ed.macros[&'a'].len(), 3, "\"bq only");
        let ed = run("a\nb\n", "QA!<esc>qQq");
        assert_eq!(text(&ed), "a!!\nb\n");
        assert_eq!(ed.status.as_ref().map(|s| s.1), Some(Severity::Error));
    }

    /// Theme list and preview are read on a worker thread; a preview that lands after its row was left is
    /// dropped; Esc brings the original back.
    #[test]
    fn theme_picker_lists_and_previews_off_the_main_thread() {
        let mut ed = editor("");
        let original = ed.theme.name.clone();
        feed(&mut ed, ":theme<ret>");
        assert!(ed.picker.is_none(), "list comes from a worker thread");
        settle(&mut ed);
        let current = |ed: &Editor| ed.picker.as_ref().and_then(|p| p.current()).map(|i| i.label.clone());
        assert_eq!(current(&ed), Some(original.clone()), "starts on the current theme");
        feed(&mut ed, "<down>");
        let row = current(&ed).unwrap();
        settle(&mut ed);
        assert_eq!(ed.theme.name, row, "previewed");
        feed(&mut ed, "<down><up><up>");
        while let Some(ev) = ed.events.recv_timeout(std::time::Duration::from_millis(300)) {
            ed.handle_event(ev);
        }
        assert_eq!(
            Some(ed.theme.name.clone()),
            current(&ed),
            "late previews of rows already left are dropped"
        );
        feed(&mut ed, "<esc>");
        assert_eq!(ed.theme.name, original);
    }

    /// `space G w` with a one-character cursor takes the word — even when that character is multi-byte.
    #[test]
    fn watch_takes_the_word_under_a_wide_cursor() {
        let prompt = |ed: &Editor| ed.prompt.as_ref().map(|p| p.text.clone());
        assert_eq!(prompt(&run("값x + 1", " Gw")), Some("값x".into()));
        assert_eq!(prompt(&run("a.b", "% Gw")), Some("a.b".into()), "a wider selection is taken as is");
    }

    /// `:w other` — a failed write keeps the old name; writing over an existing file isn't a conflict
    /// judged by the old file's stamp; the buffer takes the new name.
    #[test]
    fn write_as_new_path() {
        let dir = std::env::temp_dir().join(format!("tarae-write-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (a, b) = (dir.join("a.txt"), dir.join("b.txt"));
        std::fs::write(&a, "a\n").unwrap();
        std::fs::write(&b, "old b\n").unwrap();
        let mut ed = editor("");
        ed.open(&a).unwrap();
        let a = ed.doc().path.clone().unwrap();
        feed(&mut ed, &format!("ix<esc>:w {}/nowhere/c.txt<ret>", dir.display()));
        assert_eq!(ed.status.as_ref().map(|s| s.1), Some(Severity::Error));
        assert_eq!(ed.doc().path.as_ref(), Some(&a), "failed write keeps the name");
        feed(&mut ed, &format!(":w {}<ret>", b.display()));
        assert_ne!(ed.status.as_ref().map(|s| s.1), Some(Severity::Error), "{:?}", ed.status);
        assert_eq!(std::fs::read_to_string(&b).unwrap(), "xa\n");
        assert_eq!(std::fs::read_to_string(&a).unwrap(), "a\n");
        assert!(ed.doc().path.as_ref().is_some_and(|p| p.ends_with("b.txt")));
        assert!(!ed.doc().is_modified());
        std::fs::remove_dir_all(&dir).ok();
    }
}
