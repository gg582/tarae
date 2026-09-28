//! Replace across files — from the `space /` results: `C-r` asks for the replacement; the matches still
//! listed (type to narrow them first) are replaced. A picker shows one row per file with its diff on the
//! right; Enter applies to the files still listed — one undo step per file, then `:wa` to save.
//! Planning runs on a worker thread against each file's text (the open buffer's if it's open).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use ropey::Rope;
use serde_json::{Value, json};

use crate::editdiff::FileDiff;
use crate::lsp::Encoding;

/// Per file, the lines (from 0) whose matches get replaced.
pub type Targets = Vec<(PathBuf, Vec<usize>)>;

/// One file's replacement: LSP TextEdits (UTF-8 positions) against `base`, and its preview.
#[derive(Clone, Debug)]
pub struct FilePlan {
    pub path: PathBuf,
    pub base: Rope,
    pub edits: Vec<Value>,
    /// The file's diff (shared with the preview pane).
    pub diff: std::sync::Arc<Vec<FileDiff>>,
    /// Matches replaced.
    pub count: usize,
}

/// Case like ripgrep's `--smart-case`: insensitive unless the pattern has an uppercase letter.
pub fn regex(pattern: &str) -> Result<regex::Regex, String> {
    regex::RegexBuilder::new(pattern)
        .case_insensitive(!pattern.chars().any(char::is_uppercase))
        .build()
        .map_err(|e| e.to_string().lines().last().unwrap_or("invalid regex").trim().to_string())
}

/// `targets` = per file, the lines (from 0) whose matches get replaced. `open` = texts of open buffers.
pub fn plan(
    pattern: &str,
    replacement: &str,
    targets: &[(PathBuf, Vec<usize>)],
    open: &HashMap<PathBuf, Rope>,
) -> Result<Vec<FilePlan>, String> {
    let re = regex(pattern)?;
    let mut out = Vec::new();
    for (path, lines) in targets {
        let base = match open.get(path) {
            Some(t) => t.clone(),
            None => match crate::document::read_file(path) {
                Ok(t) => t,
                Err(_) => continue, // gone or unreadable since the search
            },
        };
        let (edits, count) = line_edits(&re, replacement, &base, lines);
        if edits.is_empty() {
            continue;
        }
        let edit = json!({ "changes": { crate::lsp::uri(path): edits } });
        let Some(diff) = crate::editdiff::build(&edit, Encoding::Utf8, |_: &Path| Some(base.clone())).pop()
        else {
            continue;
        };
        out.push(FilePlan { path: path.clone(), base, edits, diff: std::sync::Arc::new(vec![diff]), count });
    }
    Ok(out)
}

/// Each listed line with a match → one edit replacing the whole line's text (newline kept).
fn line_edits(re: &regex::Regex, replacement: &str, text: &Rope, lines: &[usize]) -> (Vec<Value>, usize) {
    let mut edits = Vec::new();
    let mut count = 0;
    let mut lines = lines.to_vec();
    lines.sort_unstable();
    lines.dedup();
    for l in lines.into_iter().filter(|&l| l < text.len_lines()) {
        let (start, end) = (crate::movement::line_start(text, l), crate::movement::line_end(text, l));
        let old = text.byte_slice(start..end).to_string();
        let n = re.find_iter(&old).count();
        if n == 0 {
            continue;
        }
        let new = re.replace_all(&old, replacement);
        if new == old {
            continue;
        }
        count += n;
        edits.push(json!({
            "range": { "start": { "line": l, "character": 0 }, "end": { "line": l, "character": old.len() } },
            "newText": new,
        }));
    }
    (edits, count)
}

// ── On the editor ────────────────────────────────────────────────────────────

impl crate::editor::Editor {
    /// `C-r` in the global search results: remember the listed matches, ask for the replacement.
    pub fn replace_ask(&mut self) {
        let Some(p) = self.picker.as_ref() else { return };
        let Some(pattern) = p.grep.clone() else { return };
        let mut by_file: Targets = Vec::new();
        for item in p.shown() {
            if let crate::picker::Action::Goto { path, line, .. } = &item.action {
                match by_file.iter_mut().find(|(f, _)| f == path) {
                    Some((_, lines)) => lines.push(*line),
                    None => by_file.push((path.clone(), vec![*line])),
                }
            }
        }
        // Buffers hold real paths (macOS /tmp = /private/tmp) — match open ones by those
        for (path, _) in &mut by_file {
            if let Ok(real) = std::fs::canonicalize(&*path) {
                *path = real;
            }
        }
        if by_file.is_empty() {
            return self.note("nothing listed to replace");
        }
        self.keep_last_picker(); // `space '` brings the results back
        self.replace_targets = Some((pattern, by_file));
        self.open_prompt(crate::editor::PromptKind::Replace, "");
    }

    /// The replacement was typed — plan on a worker thread (open buffers' text, else disk), then preview.
    pub fn replace_plan_start(&mut self, replacement: String) {
        let Some((pattern, targets)) = self.replace_targets.take() else { return };
        let open: HashMap<PathBuf, Rope> = targets
            .iter()
            .filter_map(|(p, _)| self.open_doc_at(p).map(|d| (p.clone(), d.text.clone())))
            .collect();
        self.note("working out the replacement…");
        self.events.jobs().spawn(move || {
            let result = plan(&pattern, &replacement, &targets, &open);
            move |ed: &mut crate::editor::Editor| ed.replace_preview(&pattern, &replacement, result)
        });
    }

    fn replace_preview(&mut self, pattern: &str, replacement: &str, result: Result<Vec<FilePlan>, String>) {
        let plans = match result {
            Ok(p) if p.is_empty() => return self.note("nothing to replace"),
            Ok(p) => p,
            Err(e) => return self.set_error(e),
        };
        self.status = None;
        let root = std::env::current_dir().unwrap_or_default();
        let items = plans
            .iter()
            .enumerate()
            .map(|(n, f)| crate::picker::Item {
                label: f.path.strip_prefix(&root).unwrap_or(&f.path).display().to_string(),
                action: crate::picker::Action::ReplaceFile(n),
                hint: format!("{}×", f.count),
                glyph: None,
            })
            .collect();
        let total: usize = plans.iter().map(|f| f.count).sum();
        self.replace_plan = plans;
        let title = format!("replace {total} · /{pattern}/ → {replacement}");
        self.open_picker(crate::picker::Picker::new(title, items, true), None);
    }

    /// Apply plan files `files` — one undo step each; a file changed since the plan is skipped.
    pub fn replace_apply(&mut self, files: &[usize]) {
        let current = self.doc().id;
        let (mut done, mut matches, mut skipped) = (0, 0, Vec::new());
        for &n in files {
            let Some(f) = self.replace_plan.get(n).cloned() else { continue };
            let id = match self.open_doc_at(&f.path) {
                Some(d) => d.id,
                None => match self.open(&f.path) {
                    Ok(()) => self.doc().id,
                    Err(e) => {
                        skipped.push(format!("{}: {e:#}", f.path.display()));
                        continue;
                    }
                },
            };
            let same = self.docs.iter().find(|d| d.id == id).is_some_and(|d| d.text == f.base);
            if !same {
                skipped.push(format!("{} changed", f.path.display()));
                continue;
            }
            match self.apply_text_edits(id, Encoding::Utf8, &f.edits) {
                Ok(()) => (done, matches) = (done + 1, matches + f.count),
                Err(e) => skipped.push(format!("{}: {e}", f.path.display())),
            }
        }
        if let Some(i) = self.docs.iter().position(|d| d.id == current) {
            self.current = i;
        }
        self.replace_plan.clear();
        if done > 0 {
            self.set_success(format!("Replaced {matches} in {done} file(s) · :wa saves them"));
        }
        if !skipped.is_empty() {
            self.set_warning(format!("skipped {}", skipped.join(", ")));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaces_only_listed_lines_with_captures_and_smart_case() {
        let dir = std::env::temp_dir().join(format!("tarae-replace-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (a, b) = (dir.join("a.rs"), dir.join("b.rs"));
        std::fs::write(&a, "let old_name = 1;\nold_name + OLD_NAME;\nkeep old_name\n").unwrap();
        std::fs::write(&b, "nothing here\n").unwrap();
        // b.rs is open with unsaved text — the buffer counts, not the disk
        let open = HashMap::from([(b.clone(), Rope::from_str("old_value\n"))]);
        let targets = vec![(a.clone(), vec![0, 1]), (b.clone(), vec![0])];
        let plans = plan(r"old_(\w+)", "new_$1", &targets, &open).unwrap();
        assert_eq!(plans.len(), 2);
        let t = crate::editdiff::apply_edits(&plans[0].base, &plans[0].edits, Encoding::Utf8);
        assert_eq!(
            t.to_string(),
            "let new_name = 1;\nnew_name + new_NAME;\nkeep old_name\n",
            "line 2 not listed"
        );
        assert_eq!(plans[0].count, 3, "smart case: lowercase pattern matches OLD_NAME too");
        assert_eq!(plans[1].count, 1);
        assert!(plan("(", "x", &targets, &open).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }
}
