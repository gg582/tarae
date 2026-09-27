//! Edits previewed before applying — LSP WorkspaceEdit (code actions, rename) as per-file before/after diffs.
//!
//! The pure part (`build`) only takes a function returning the old text — an open buffer's rope,
//! else disk (worker thread).
//! Line diff (imara-diff) → hunks with 2 lines of context; paired −/+ lines also highlight the changed
//! middle, syntax colors per line (before and after each parsed in that file's language).

use std::path::{Path, PathBuf};

use ropey::Rope;
use serde_json::Value;
use tree_sitter::QueryCursor;

use crate::lsp::{self, Encoding};
use crate::syntax::{self, Syntax};
use crate::transaction::{Change, Transaction};

/// Context lines shown around a hunk.
const CONTEXT: usize = 2;
/// Cap on diff lines shown per file (huge reformatting etc.).
const MAX_LINES: usize = 400;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineKind {
    Context,
    Minus,
    Plus,
    /// Skipped context (`⋯`).
    Gap,
}

#[derive(Clone, Debug)]
pub struct DiffLine {
    pub kind: LineKind,
    /// Line number (from 1) — old number for deleted lines, new number otherwise.
    pub number: usize,
    pub text: String,
    /// Syntax colors: (start, end, global capture id) — bytes within the line.
    pub spans: Vec<(usize, usize, u32)>,
    /// The actually changed middle of a paired −/+ line (bytes).
    pub emph: Option<(usize, usize)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FileOp {
    Edit,
    Create,
    Rename(PathBuf),
    Delete,
}

#[derive(Clone, Debug)]
pub struct FileDiff {
    pub path: PathBuf,
    pub op: FileOp,
    pub lines: Vec<DiffLine>,
    pub added: usize,
    pub removed: usize,
}

/// Summary of one edit (for picker title and hints).
pub fn summary(files: &[FileDiff]) -> String {
    let (a, r) = files.iter().fold((0, 0), |(a, r), f| (a + f.added, r + f.removed));
    let n = files.len();
    let files = if n == 1 { "1 file".to_string() } else { format!("{n} files") };
    format!("{files}  +{a} −{r}")
}

/// Result of applying TextEdit[] to the text.
pub fn apply_edits(text: &Rope, edits: &[Value], enc: Encoding) -> Rope {
    let changes = edits
        .iter()
        .filter_map(|e| {
            let from = lsp::from_position(text, &e["range"]["start"], enc)?;
            let to = lsp::from_position(text, &e["range"]["end"], enc)?;
            Some(Change { from, to: to.max(from), insert: e["newText"].as_str()?.to_string() })
        })
        .collect();
    let mut out = text.clone();
    Transaction::new(changes).apply(&mut out, false);
    out
}

/// WorkspaceEdit → per-file diffs. `old(path)` = current text (None if the file doesn't exist).
pub fn build(edit: &Value, enc: Encoding, mut old: impl FnMut(&Path) -> Option<Rope>) -> Vec<FileDiff> {
    // File → accumulated text (several edits to one file chain on)
    let mut files: Vec<(PathBuf, FileOp, Option<Rope>, Rope)> = Vec::new();
    type Files = Vec<(PathBuf, FileOp, Option<Rope>, Rope)>;
    fn touch(
        files: &mut Files,
        old: &mut impl FnMut(&Path) -> Option<Rope>,
        path: PathBuf,
        edits: &[Value],
        enc: Encoding,
    ) {
        let i = match files.iter().position(|f| f.0 == path) {
            Some(i) => i,
            None => {
                let before = old(&path);
                let now = before.clone().unwrap_or_default();
                files.push((path, FileOp::Edit, before, now));
                files.len() - 1
            }
        };
        files[i].3 = apply_edits(&files[i].3, edits, enc);
    }
    let path_of = |u: &Value| u.as_str().and_then(lsp::path_from_uri);
    // `documentChanges` wins when both are present (LSP spec — servers send both for old clients)
    if let Some(map) = edit["changes"].as_object().filter(|_| edit["documentChanges"].is_null()) {
        for (u, e) in map {
            if let Some(p) = lsp::path_from_uri(u) {
                touch(&mut files, &mut old, p, e.as_array().map(Vec::as_slice).unwrap_or_default(), enc);
            }
        }
    }
    for dc in edit["documentChanges"].as_array().into_iter().flatten() {
        match dc["kind"].as_str() {
            Some("create") => {
                if let Some(p) = path_of(&dc["uri"]) {
                    files.push((p, FileOp::Create, None, Rope::new()));
                }
            }
            Some("rename") => {
                if let (Some(from), Some(to)) = (path_of(&dc["oldUri"]), path_of(&dc["newUri"])) {
                    // Edits after a rename come under the new name — carry the old text over to the new name
                    let before = files
                        .iter()
                        .position(|f| f.0 == from)
                        .map(|i| files.remove(i).3)
                        .or_else(|| old(&from));
                    let now = before.clone().unwrap_or_default();
                    files.push((to, FileOp::Rename(from), before, now));
                }
            }
            Some("delete") => {
                if let Some(p) = path_of(&dc["uri"]) {
                    let before = old(&p);
                    files.push((p, FileOp::Delete, before, Rope::new()));
                }
            }
            _ => {
                if let Some(p) = path_of(&dc["textDocument"]["uri"]) {
                    touch(
                        &mut files,
                        &mut old,
                        p,
                        dc["edits"].as_array().map(Vec::as_slice).unwrap_or_default(),
                        enc,
                    );
                }
            }
        }
    }
    files
        .into_iter()
        .map(|(path, op, before, now)| diff_file(path, op, before.unwrap_or_default(), now))
        .collect()
}

/// One file: line diff + context + highlight + syntax colors.
fn diff_file(path: PathBuf, op: FileOp, before: Rope, after: Rope) -> FileDiff {
    use imara_diff::intern::InternedInput;
    use imara_diff::{Algorithm, diff};
    let (bs, as_) = (before.to_string(), after.to_string());
    let input = InternedInput::new(bs.as_str(), as_.as_str());
    let mut hunks: Vec<(std::ops::Range<usize>, std::ops::Range<usize>)> = Vec::new();
    diff(Algorithm::Histogram, &input, |b: std::ops::Range<u32>, a: std::ops::Range<u32>| {
        hunks.push((b.start as usize..b.end as usize, a.start as usize..a.end as usize));
    });
    let (bl, al): (Vec<&str>, Vec<&str>) = (lines(&bs), lines(&as_));
    let highlighter = |text: &Rope| -> Option<Syntax> {
        let spec = syntax::detect(&path)?;
        let lang = syntax::Loader::global().load(spec).ok()?;
        let mut syn = Syntax::new(lang);
        let job = syn.start_parse(text);
        let generation = job.generation;
        syn.finish_parse(generation, job.run());
        Some(syn)
    };
    let (hb, ha) = if bs.len() + as_.len() < 1 << 20 {
        (highlighter(&before), highlighter(&after))
    } else {
        (None, None)
    };
    let mut cursor = QueryCursor::new();
    let mut spans_of = |syn: &Option<Syntax>, text: &Rope, line: usize| -> Vec<(usize, usize, u32)> {
        let Some(syn) = syn else { return Vec::new() };
        if line >= text.len_lines() {
            return Vec::new();
        }
        let start = text.line_to_byte(line);
        let end = start + text.line(line).len_bytes();
        syntax::highlights(syn, text, start..end, &mut cursor)
            .into_iter()
            .map(|(s, e, c)| (s - start, e - start, c))
            .collect()
    };
    // Expand the whole file into rows: (kind, old line, new line) — context has both, − only old, + only new.
    let mut rows: Vec<(LineKind, usize, usize)> = Vec::new();
    let (mut ob, mut oa) = (0, 0);
    for (b, a) in &hunks {
        while ob < b.start {
            rows.push((LineKind::Context, ob, oa));
            (ob, oa) = (ob + 1, oa + 1);
        }
        rows.extend(b.clone().map(|l| (LineKind::Minus, l, 0)));
        rows.extend(a.clone().map(|l| (LineKind::Plus, 0, l)));
        (ob, oa) = (b.end, a.end);
    }
    while ob < bl.len() {
        rows.push((LineKind::Context, ob, oa));
        (ob, oa) = (ob + 1, oa + 1);
    }
    // Keep only context within CONTEXT of changed rows
    let mut keep = vec![false; rows.len()];
    for (i, r) in rows.iter().enumerate() {
        if r.0 != LineKind::Context {
            keep[i.saturating_sub(CONTEXT)..(i + CONTEXT + 1).min(rows.len())].fill(true);
        }
    }
    let clip = |s: &str| s.trim_end_matches(['\n', '\r']).to_string();
    let mut out: Vec<DiffLine> = Vec::new();
    let mut skipped = false;
    for (i, &(kind, o, n)) in rows.iter().enumerate() {
        if !keep[i] {
            skipped = true;
            continue;
        }
        if out.len() >= MAX_LINES {
            break;
        }
        if skipped {
            out.push(DiffLine {
                kind: LineKind::Gap,
                number: 0,
                text: String::new(),
                spans: Vec::new(),
                emph: None,
            });
            skipped = false;
        }
        out.push(match kind {
            LineKind::Minus => DiffLine {
                kind,
                number: o + 1,
                text: clip(bl[o]),
                spans: spans_of(&hb, &before, o),
                emph: None,
            },
            _ => DiffLine {
                kind,
                number: n + 1,
                text: clip(al[n]),
                spans: spans_of(&ha, &after, n),
                emph: None,
            },
        });
    }
    if skipped && !out.is_empty() {
        out.push(DiffLine {
            kind: LineKind::Gap,
            number: 0,
            text: String::new(),
            spans: Vec::new(),
            emph: None,
        });
    }
    // Pairing: consecutive −s with the +s right after them, in order
    let mut i = 0;
    while i < out.len() {
        let m0 = i;
        while i < out.len() && out[i].kind == LineKind::Minus {
            i += 1;
        }
        let p0 = i;
        while i < out.len() && out[i].kind == LineKind::Plus {
            i += 1;
        }
        for k in 0..(p0 - m0).min(i - p0) {
            if let Some((m, p)) = emphasis(&out[m0 + k].text, &out[p0 + k].text) {
                out[m0 + k].emph = Some(m);
                out[p0 + k].emph = Some(p);
            }
        }
        if i == m0 {
            i += 1;
        }
    }
    let removed = rows.iter().filter(|r| r.0 == LineKind::Minus).count();
    let added = rows.iter().filter(|r| r.0 == LineKind::Plus).count();
    FileDiff { path, op, lines: out, added, removed }
}

fn lines(s: &str) -> Vec<&str> {
    s.split_inclusive('\n').collect()
}

/// Changed middle of two paired lines (minus common prefix/suffix, on char boundaries).
/// No highlight if nearly all changed.
fn emphasis(a: &str, b: &str) -> Option<((usize, usize), (usize, usize))> {
    let c = crate::disk::minimal_change(&Rope::from_str(a), &Rope::from_str(b))?;
    let (af, at) = (c.from, c.to);
    let (bf, bt) = (c.from, c.from + c.insert.len());
    let longest = a.len().max(b.len()).max(1);
    if (at - af).max(bt - bf) * 10 > longest * 8 {
        return None;
    }
    Some(((af, at), (bf, bt)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn edit(line: u64, from: u64, to: u64, text: &str) -> Value {
        json!({ "range": { "start": { "line": line, "character": from }, "end": { "line": line, "character": to } }, "newText": text })
    }

    #[test]
    fn diff_of_a_text_edit_with_context_and_emphasis() {
        let src = "fn main() {\n    let x = 1;\n    let y = 2;\n    println!(\"{x}\");\n}\n";
        let we = json!({ "changes": { "file:///p/a.rs": [edit(1, 8, 9, "_x")] } });
        let files = build(&we, Encoding::Utf8, |_| Some(Rope::from_str(src)));
        assert_eq!(files.len(), 1);
        let f = &files[0];
        assert_eq!((f.added, f.removed, &f.op), (1, 1, &FileOp::Edit));
        let kinds: Vec<LineKind> = f.lines.iter().map(|l| l.kind).collect();
        use LineKind::*;
        assert_eq!(kinds, [Context, Minus, Plus, Context, Context, Gap]);
        let minus = &f.lines[1];
        assert_eq!((minus.number, minus.text.as_str()), (2, "    let x = 1;"));
        assert_eq!(minus.emph, Some((8, 8)), "nothing deleted");
        assert_eq!(f.lines[2].emph, Some((8, 9)), "inserted one `_`");
        assert_eq!(summary(&files), "1 file  +1 −1");
    }

    #[test]
    fn document_changes_win_over_changes() {
        let we = json!({
            "changes": { "file:///p/a.rs": [edit(0, 0, 0, "y")] },
            "documentChanges": [
                { "textDocument": { "uri": "file:///p/a.rs", "version": 1 }, "edits": [edit(0, 0, 0, "y")] },
            ],
        });
        let files = build(&we, Encoding::Utf8, |_| Some(Rope::from_str("x\n")));
        assert_eq!(files.len(), 1);
        let plus: Vec<&str> =
            files[0].lines.iter().filter(|l| l.kind == LineKind::Plus).map(|l| l.text.as_str()).collect();
        assert_eq!(plus, ["yx"], "applied once, not twice");
    }

    #[test]
    fn file_operations() {
        let we = json!({ "documentChanges": [
            { "kind": "create", "uri": "file:///p/new.rs" },
            { "textDocument": { "uri": "file:///p/new.rs", "version": null }, "edits": [edit(0, 0, 0, "pub fn f() {}\n")] },
            { "kind": "rename", "oldUri": "file:///p/old.rs", "newUri": "file:///p/moved.rs" },
            { "kind": "delete", "uri": "file:///p/gone.rs" },
        ] });
        let files =
            build(&we, Encoding::Utf8, |p| (p != Path::new("/p/new.rs")).then(|| Rope::from_str("x\n")));
        let ops: Vec<(&str, &FileOp)> = files.iter().map(|f| (f.path.to_str().unwrap(), &f.op)).collect();
        assert_eq!(ops[0], ("/p/new.rs", &FileOp::Create), "edits following a created file form one entry");
        assert_eq!(files[0].added, 1);
        assert_eq!(ops[1], ("/p/moved.rs", &FileOp::Rename("/p/old.rs".into())));
        assert_eq!(files[1].lines.len(), 0, "move with content unchanged");
        assert_eq!(ops[2], ("/p/gone.rs", &FileOp::Delete));
        assert_eq!(files[2].removed, 1);
    }
}
