//! Picker — fuzzy list (files `space f`, buffers `space b`, global search results `space /`).
//!
//! The list is filled by a worker thread (file listing and search hit disk or external commands);
//! filtering is nucleo (Helix's matcher). Each keystroke refilters all, but match positions are computed
//! only for visible lines.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use ropey::Rope;

use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};

use crate::document::DocId;
use crate::syntax::{self, Syntax};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    Open(PathBuf),
    Buffer(DocId),
    /// Index in the code action list (the editor holds it).
    Code(usize),
    /// Command palette: named commands / `:` command line.
    Command(&'static str),
    Typed(String),
    /// Theme selection (previewed while choosing).
    Theme(String),
    /// Entry `index` of the pane's jump list — in open document `doc`, at `line` (for the preview).
    Jump {
        index: usize,
        doc: DocId,
        line: usize,
    },
    /// File `n` of the pending replace-across-files plan (replace.rs).
    ReplaceFile(usize),
    /// File `n` of the changed-files list (`space g f` — its diff is the preview).
    ChangedFile(usize),
    /// Line `line` (from 0) of the file, byte `col` within that line.
    Goto {
        path: PathBuf,
        line: usize,
        col: usize,
    },
}

/// Job that builds the list on a worker thread.
pub type Fill = Box<dyn FnOnce() -> Result<Vec<Item>, String> + Send>;

#[derive(Clone, Debug)]
pub struct Item {
    pub label: String,
    pub action: Action,
    /// Dimmed annotation on the right (keys in the command palette etc.) — not used for filtering.
    pub hint: String,
    /// Kind glyph before the name and its color's theme scope (symbol picker: `ƒ` function …).
    pub glyph: Option<(&'static str, &'static str)>,
}

/// One visible line: (label, matched char positions, selected?, annotation, kind glyph).
pub type Row = (String, Vec<u32>, bool, String, Option<(&'static str, &'static str)>);

pub struct Picker {
    /// Number assigned by the editor — so a late list doesn't land in a different (newer) picker.
    pub id: u64,
    pub title: String,
    pub query: String,
    items: Vec<Item>,
    /// Filtered items (items indices, by score).
    matches: Vec<u32>,
    pub selected: usize,
    pub scroll: usize,
    /// Small window (top center) — for pickers that need the editor behind visible (theme preview).
    pub compact: bool,
    /// Picker that refetches the list when the query changes (workspace symbols — the server filters).
    pub requery: bool,
    /// The list is still being filled.
    pub loading: bool,
    /// Lines visible this frame (render fills them, draw draws them).
    pub view: Vec<Row>,
    /// Whether to use the right-hand preview pane (off when there's no file to show, e.g. code actions).
    pub preview: bool,
    /// Global search results: the pattern (`C-r` replaces the listed matches).
    pub grep: Option<String>,
    /// Preview scrolled this many lines from where it opens (back to 0 when the selection changes).
    pub preview_scroll: usize,
    /// Preview cache (path → loading/content). Filled by a worker thread.
    pub previews: HashMap<PathBuf, Preview>,
    matcher: Matcher,
}

impl Picker {
    pub fn new(title: impl Into<String>, items: Vec<Item>, paths: bool) -> Self {
        let config = if paths { Config::DEFAULT.match_paths() } else { Config::DEFAULT };
        let mut p = Picker {
            id: 0,
            title: title.into(),
            query: String::new(),
            items,
            matches: Vec::new(),
            selected: 0,
            scroll: 0,
            compact: false,
            requery: false,
            loading: false,
            view: Vec::new(),
            preview: true,
            grep: None,
            preview_scroll: 0,
            previews: HashMap::new(),
            matcher: Matcher::new(config),
        };
        p.refilter();
        p
    }

    pub fn without_preview(mut self) -> Self {
        self.preview = false;
        self
    }

    pub fn set_items(&mut self, items: Vec<Item>) {
        self.items = items;
        self.loading = false;
        self.refilter();
    }

    pub fn refilter(&mut self) {
        self.preview_scroll = 0;
        let pat = Pattern::parse(&self.query, CaseMatching::Smart, Normalization::Smart);
        let mut buf = Vec::new();
        let mut scored: Vec<(u32, u32)> = self
            .items
            .iter()
            .enumerate()
            .filter_map(|(i, it)| {
                pat.score(Utf32Str::new(&it.label, &mut buf), &mut self.matcher).map(|s| (s, i as u32))
            })
            .collect();
        // Highest score first, ties in original order (empty query = original order)
        scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        self.matches = scored.into_iter().map(|(_, i)| i).collect();
        self.selected = self.selected.min(self.matches.len().saturating_sub(1));
    }

    pub fn push(&mut self, c: char) {
        self.query.push(c);
        self.selected = 0;
        self.refilter();
    }

    pub fn pop(&mut self) {
        self.query.pop();
        self.refilter();
    }

    pub fn move_by(&mut self, delta: isize) {
        let n = self.matches.len();
        if n == 0 {
            return;
        }
        self.selected = (self.selected as isize + delta).rem_euclid(n as isize) as usize;
        self.preview_scroll = 0;
    }

    /// Items the query leaves listed, best first.
    pub fn shown(&self) -> impl Iterator<Item = &Item> {
        self.matches.iter().map(|&i| &self.items[i as usize])
    }

    pub fn items(&self) -> &[Item] {
        &self.items
    }

    pub fn current(&self) -> Option<&Item> {
        self.matches.get(self.selected).map(|&i| &self.items[i as usize])
    }

    pub fn counts(&self) -> (usize, usize) {
        (self.matches.len(), self.items.len())
    }

    /// Visible lines: (label, matched char positions, selected?, annotation). Scrolls so the selection shows.
    pub fn visible(&mut self, rows: usize) -> Vec<Row> {
        if self.selected < self.scroll {
            self.scroll = self.selected;
        } else if self.selected >= self.scroll + rows {
            self.scroll = self.selected + 1 - rows;
        }
        let pat = Pattern::parse(&self.query, CaseMatching::Smart, Normalization::Smart);
        let mut buf = Vec::new();
        let (scroll, selected) = (self.scroll, self.selected);
        self.matches
            .iter()
            .enumerate()
            .skip(scroll)
            .take(rows)
            .map(|(row, &i)| {
                let item = &self.items[i as usize];
                let mut idx = Vec::new();
                pat.indices(Utf32Str::new(&item.label, &mut buf), &mut self.matcher, &mut idx);
                idx.sort_unstable();
                idx.dedup();
                (item.label.clone(), idx, row == selected, item.hint.clone(), item.glyph)
            })
            .collect()
    }
}

// ── Preview ──────────────────────────────────────────────────────────────────

pub enum Preview {
    Loading,
    Ready { text: Rope, syntax: Option<Syntax> },
    Unavailable(&'static str),
}

impl Action {
    /// File to preview and the line to highlight.
    pub fn preview_target(&self) -> Option<(&Path, Option<usize>)> {
        match self {
            Action::Open(p) => Some((p, None)),
            Action::Goto { path, line, .. } => Some((path, Some(*line))),
            Action::Buffer(_)
            | Action::Jump { .. }
            | Action::ReplaceFile(_)
            | Action::ChangedFile(_)
            | Action::Code(_)
            | Action::Command(_)
            | Action::Typed(_)
            | Action::Theme(_) => None,
        }
    }
}

const PREVIEW_BYTES: usize = 512 * 1024;
pub const PREVIEW_CACHE: usize = 64;

/// On a worker thread — read just the head and, if the language is known, parse it too.
pub fn load_preview(path: &Path) -> Preview {
    let Ok(bytes) = std::fs::read(path) else { return Preview::Unavailable("can't read") };
    if bytes[..bytes.len().min(8192)].contains(&0) {
        return Preview::Unavailable("binary file");
    }
    let mut end = bytes.len().min(PREVIEW_BYTES);
    while end < bytes.len() && (bytes[end] & 0xC0) == 0x80 {
        end -= 1; // don't cut mid-char
    }
    let Ok(s) = std::str::from_utf8(&bytes[..end]) else { return Preview::Unavailable("not UTF-8") };
    let text = Rope::from_str(s);
    let syntax = syntax::detect(path).and_then(|spec| syntax::Loader::global().load(spec).ok()).map(|lang| {
        let mut syn = Syntax::new(lang);
        let job = syn.start_parse(&text);
        let generation = job.generation;
        syn.finish_parse(generation, job.run());
        syn
    });
    Preview::Ready { text, syntax }
}

// ── Building lists (worker thread) ───────────────────────────────────────────

/// File list — `git ls-files` in a git repo (respects .gitignore, fast), else walk the tree directly.
pub fn list_files(root: &Path) -> Vec<PathBuf> {
    let git = Command::new("git")
        .args(["ls-files", "--cached", "--others", "--exclude-standard", "-z"])
        .current_dir(root)
        .output();
    if let Ok(out) = git
        && out.status.success()
    {
        return out
            .stdout
            .split(|&b| b == 0)
            .filter(|s| !s.is_empty())
            .map(|s| PathBuf::from(String::from_utf8_lossy(s).into_owned()))
            .filter(|p| root.join(p).is_file())
            .collect();
    }
    let mut out = Vec::new();
    walk(root, Path::new(""), &mut out);
    out
}

const SKIP_DIRS: &[&str] = &["target", "node_modules", ".git", ".venv", "__pycache__"];
const MAX_FILES: usize = 200_000;

fn walk(root: &Path, rel: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(root.join(rel)) else { return };
    let mut entries: Vec<_> = rd.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        if out.len() >= MAX_FILES {
            return;
        }
        let name = e.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') || SKIP_DIRS.contains(&name.as_ref()) {
            continue;
        }
        let path = rel.join(name.as_ref());
        match e.file_type() {
            Ok(t) if t.is_dir() => walk(root, &path, out),
            Ok(t) if t.is_file() => out.push(path),
            _ => {}
        }
    }
}

pub fn file_items(root: &Path) -> Vec<Item> {
    list_files(root)
        .into_iter()
        .map(|rel| Item {
            label: rel.to_string_lossy().into_owned(),
            action: Action::Open(root.join(rel)),
            hint: String::new(),
            glyph: None,
        })
        .collect()
}

const MAX_RESULTS: usize = 10_000;

/// Global search — ripgrep if `rg` exists (smart case, respects .gitignore), else built-in regex scan.
pub fn grep(root: &Path, pattern: &str) -> Result<Vec<Item>, String> {
    let rg = Command::new("rg")
        .args([
            "--vimgrep",
            "--no-heading",
            "--color=never",
            "--smart-case",
            "--max-columns=240",
            "-e",
            pattern,
        ])
        .current_dir(root)
        .output();
    match rg {
        Ok(out) if out.status.success() || out.status.code() == Some(1) => {
            Ok(String::from_utf8_lossy(&out.stdout)
                .lines()
                .filter_map(|l| parse_vimgrep(root, l))
                .take(MAX_RESULTS)
                .collect())
        }
        Ok(out) => {
            Err(String::from_utf8_lossy(&out.stderr).lines().next().unwrap_or("rg failed").to_string())
        }
        Err(_) => grep_builtin(root, pattern),
    }
}

/// `path:line:col:text` (line and col from 1, col in bytes).
fn parse_vimgrep(root: &Path, l: &str) -> Option<Item> {
    let mut it = l.splitn(4, ':');
    let (path, line, col, text) = (it.next()?, it.next()?, it.next()?, it.next().unwrap_or(""));
    let (line, col): (usize, usize) = (line.parse().ok()?, col.parse().ok()?);
    Some(Item {
        label: format!("{path}:{line}: {}", text.trim()),
        action: Action::Goto { path: root.join(path), line: line - 1, col: col - 1 },
        hint: String::new(),
        glyph: None,
    })
}

fn grep_builtin(root: &Path, pattern: &str) -> Result<Vec<Item>, String> {
    let re = regex::RegexBuilder::new(pattern)
        .case_insensitive(!pattern.chars().any(char::is_uppercase))
        .build()
        .map_err(|e| e.to_string().lines().last().unwrap_or("invalid regex").trim().to_string())?;
    let mut out = Vec::new();
    for rel in list_files(root) {
        let Ok(src) = std::fs::read_to_string(root.join(&rel)) else { continue }; // skip binary and non-UTF-8
        for (i, line) in src.lines().enumerate() {
            if let Some(m) = re.find(line) {
                out.push(Item {
                    label: format!("{}:{}: {}", rel.display(), i + 1, line.trim()),
                    action: Action::Goto { path: root.join(&rel), line: i, col: m.start() },
                    hint: String::new(),
                    glyph: None,
                });
                if out.len() >= MAX_RESULTS {
                    return Ok(out);
                }
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn items(labels: &[&str]) -> Vec<Item> {
        labels
            .iter()
            .map(|l| Item {
                label: l.to_string(),
                action: Action::Open(PathBuf::from(l)),
                hint: String::new(),
                glyph: None,
            })
            .collect()
    }

    #[test]
    fn fuzzy_filters_and_ranks() {
        let mut p =
            Picker::new("files", items(&["src/main.rs", "src/editor.rs", "README.md", "src/term.rs"]), true);
        assert_eq!(p.counts(), (4, 4));
        for c in "edrs".chars() {
            p.push(c);
        }
        assert_eq!(p.current().map(|i| i.label.as_str()), Some("src/editor.rs"));
        let vis = p.visible(10);
        assert_eq!(vis[0].0, "src/editor.rs");
        assert!(!vis[0].1.is_empty(), "highlighting needs match positions");
        p.pop();
        p.pop();
        p.pop();
        p.pop();
        assert_eq!(p.counts().0, 4);
        p.move_by(-1);
        assert_eq!(p.current().map(|i| i.label.as_str()), Some("src/term.rs"), "wraps around upward");
    }

    #[test]
    fn vimgrep_lines() {
        let it = parse_vimgrep(Path::new("/r"), "src/a.rs:12:5:    let 타래 = 1;").unwrap();
        assert_eq!(it.action, Action::Goto { path: PathBuf::from("/r/src/a.rs"), line: 11, col: 4 });
        assert_eq!(it.label, "src/a.rs:12: let 타래 = 1;");
    }

    #[test]
    fn builtin_grep_and_walk() {
        let dir = std::env::temp_dir().join(format!("tarae-grep-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::create_dir_all(dir.join("target")).unwrap();
        std::fs::write(dir.join("a.txt"), "hello\n타래 world\n").unwrap();
        std::fs::write(dir.join("sub/b.txt"), "no\nWorld\n").unwrap();
        std::fs::write(dir.join("target/skip.txt"), "world\n").unwrap();
        let hits = grep_builtin(&dir, "world").unwrap();
        let labels: Vec<_> = hits.iter().map(|i| i.label.clone()).collect();
        assert_eq!(
            labels,
            vec!["a.txt:2: 타래 world", "sub/b.txt:2: World"],
            "lowercase = case-insensitive, target skipped"
        );
        assert_eq!(hits[0].action, Action::Goto { path: dir.join("a.txt"), line: 1, col: 7 }, "col in bytes");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
