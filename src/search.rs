//! Search and regex selection — `/ ? n N *`, `s` (select within), `S` (split), `A-s` (by line), `K`/`A-K`.
//!
//! Slices the rope into a range string and hands it to `regex` (copying 100 MB ≈ tens of ms — streaming
//! search (regex-cursor) if measurements ever call for it). regex speaks bytes too, so no offset conversion.

use regex::Regex;
use ropey::Rope;

use crate::commands::Context;
use crate::editor::{Editor, Mode, PromptKind};
use crate::selection::{Range, Selection};

fn compile(pat: &str) -> Result<Regex, String> {
    if pat.is_empty() {
        return Err("empty pattern".into());
    }
    Regex::new(pat).map_err(|e| e.to_string().lines().last().unwrap_or("invalid regex").trim().to_string())
}

/// All matches within `[from, to)`. Empty matches are dropped.
fn matches_in(text: &Rope, from: usize, to: usize, re: &Regex) -> Vec<(usize, usize)> {
    let s = text.byte_slice(from..to).to_string();
    re.find_iter(&s).filter(|m| !m.is_empty()).map(|m| (from + m.start(), from + m.end())).collect()
}

/// First non-empty match starting at or after `at` — searched in the whole string, so look-behind
/// (`\b`, `^`) still sees the text before `at`.
fn first_from(s: &str, re: &Regex, mut at: usize) -> Option<(usize, usize)> {
    while at <= s.len() {
        let m = re.find_at(s, at)?;
        if !m.is_empty() {
            return Some((m.start(), m.end()));
        }
        at = m.end() + s[m.end()..].chars().next().map_or(1, char::len_utf8);
    }
    None
}

/// Next (or previous) match after the primary cursor. Wraps at the end (second value = did it wrap).
/// `s` = the whole text as a string (copied once by the caller).
pub fn find(
    text: &Rope,
    s: &str,
    re: &Regex,
    primary: Range,
    reverse: bool,
) -> Option<((usize, usize), bool)> {
    if reverse {
        let before = primary.from();
        let all: Vec<_> = re.find_iter(s).filter(|m| !m.is_empty()).collect();
        let hit = all.iter().rev().find(|m| m.end() <= before).map(|m| (m, false));
        hit.or_else(|| all.last().map(|m| (m, true))).map(|(m, w)| ((m.start(), m.end()), w))
    } else {
        let after = primary.min_width_1(text).to();
        first_from(s, re, after).map(|h| (h, false)).or_else(|| first_from(s, re, 0).map(|h| (h, true)))
    }
}

// ── Commands (on Context) ────────────────────────────────────────────────────

/// Confirm `/`·`?`, `n`·`N`. In select mode, add the match to the selection (as helix does).
pub fn search(cx: &mut Context, pattern: &str, reverse: bool) {
    search_n(cx, pattern, reverse, 1);
}

/// `count` matches on — the pattern compiled and the text copied once.
fn search_n(cx: &mut Context, pattern: &str, reverse: bool, count: usize) {
    let re = match compile(pattern) {
        Ok(re) => re,
        Err(e) => return cx.editor.set_error(e),
    };
    cx.editor.search = Some(pattern.to_string());
    cx.editor.search_hl = true;
    let extend = cx.editor.mode == Mode::Select;
    cx.editor.push_jump();
    let doc = cx.editor.doc_mut();
    let s = doc.text.to_string();
    let mut wrapped = false;
    for _ in 0..count {
        let Some(((a, b), w)) = find(&doc.text, &s, &re, doc.selection().primary(), reverse) else {
            return cx.editor.set_error(format!("no match for /{pattern}/"));
        };
        // Direction follows the primary selection, not the search direction (as helix does).
        let range = if doc.selection().primary().is_forward() { Range::new(a, b) } else { Range::new(b, a) };
        let sel = if extend { doc.selection().clone().push(range) } else { Selection::single(range) };
        doc.set_selection(sel);
        wrapped |= w;
    }
    if wrapped {
        cx.editor.set_status("search wrapped around");
    }
}

pub fn search_next(cx: &mut Context, reverse: bool) {
    match cx.editor.search.clone() {
        Some(p) => {
            let count = cx.count();
            search_n(cx, &p, reverse, count);
        }
        None => cx.editor.set_error("no previous search (/ to search)"),
    }
}

/// `*` — the primary selection's text (escaped) as the search pattern.
pub fn search_selection(cx: &mut Context) {
    let doc = cx.editor.doc();
    let r = doc.selection().primary().min_width_1(&doc.text);
    let pat = regex::escape(&doc.text.byte_slice(r.from()..r.to()).to_string());
    cx.editor.note(format!("search: /{pat}/"));
    cx.editor.search = Some(pat);
    cx.editor.search_hl = true;
}

// ── Show all matches (once before drawing) ───────────────────────────────────

/// Document size limit for counting matches — beyond it, only move (no highlight or count). Recounted on
/// every edit (whole-text copy + regex before drawing), so kept small enough to stay within a frame.
const HIGHLIGHT_MAX: usize = 2 << 20;
/// Very common patterns (`.` etc.) are counted only up to here.
const COUNT_MAX: usize = 100_000;

/// Search to highlight now (pattern, matches — by position). `SearchHits` doesn't recount
/// for the same doc, version and pattern.
pub struct SearchHits {
    pub pattern: String,
    doc: crate::document::DocId,
    version: u64,
    pub matches: Vec<(usize, usize)>,
    pub capped: bool,
}

impl SearchHits {
    /// Number of the match the primary selection is on (from 1).
    pub fn current(&self, sel: Range) -> Option<usize> {
        let (a, b) = (sel.from(), sel.to());
        self.matches.binary_search(&(a, b)).ok().map(|i| i + 1)
    }
}

impl Editor {
    /// Pattern to show: typed text while entering `/`·`?` (preview), else the last search if highlighting.
    fn shown_pattern(&self) -> Option<String> {
        match &self.prompt {
            Some(p) if matches!(p.kind, PromptKind::Search { .. }) && !p.text.is_empty() => {
                Some(p.text.clone())
            }
            Some(p) if matches!(p.kind, PromptKind::Search { .. }) => None,
            _ if self.search_hl => self.search.clone(),
            _ => None,
        }
    }

    /// Just before drawing (term::render): sync the match list; while typing `/`, only scroll so the first
    /// match after the cursor shows (not confirmed yet, so the cursor stays; Esc returns to the start).
    pub fn refresh_search(&mut self, rows: usize) {
        let Some(pat) = self.shown_pattern() else {
            self.search_hits = None;
            return;
        };
        let doc = self.doc();
        let stale = self
            .search_hits
            .as_ref()
            .is_none_or(|h| h.pattern != pat || h.doc != doc.id || h.version != doc.version());
        if stale {
            self.search_hits = Regex::new(&pat).ok().map(|re| {
                let (matches, capped) = if doc.text.len_bytes() <= HIGHLIGHT_MAX {
                    let s = doc.text.to_string();
                    let mut v: Vec<(usize, usize)> = re
                        .find_iter(&s)
                        .filter(|m| !m.is_empty())
                        .take(COUNT_MAX + 1)
                        .map(|m| (m.start(), m.end()))
                        .collect();
                    let capped = v.len() > COUNT_MAX;
                    v.truncate(COUNT_MAX);
                    (v, capped)
                } else {
                    (Vec::new(), true)
                };
                SearchHits { pattern: pat.clone(), doc: doc.id, version: doc.version(), matches, capped }
            });
        }
        // Preview: if the first match after the cursor is off screen, center it
        let reverse =
            matches!(self.prompt.as_ref().map(|p| p.kind), Some(PromptKind::Search { reverse: true }));
        if matches!(self.prompt.as_ref().map(|p| p.kind), Some(PromptKind::Search { .. }))
            && let Some(h) = &self.search_hits
        {
            let doc = self.doc();
            let head = doc.selection().primary().cursor(&doc.text);
            let target = if reverse {
                h.matches.iter().rev().find(|m| m.1 <= head).or(h.matches.last())
            } else {
                h.matches.iter().find(|m| m.0 > head).or(h.matches.first())
            };
            if let Some(&(a, _)) = target {
                let line = doc.text.byte_to_line(a);
                let top = doc.top;
                if self.search_origin.is_none() {
                    self.search_origin = Some(top);
                }
                if line < top || line >= top + rows {
                    self.doc_mut().top = line.saturating_sub(rows / 3);
                }
            }
        }
    }
}

/// Per selection, `f(text, range)` → new ranges. If empty, keep the original selection and error.
fn reshape(cx: &mut Context, pattern: &str, f: impl Fn(&Rope, Range, &Regex) -> Vec<Range>) {
    let re = match compile(pattern) {
        Ok(re) => re,
        Err(e) => return cx.editor.set_error(e),
    };
    let doc = cx.editor.doc_mut();
    let ranges: Vec<Range> =
        doc.selection().ranges().iter().flat_map(|&r| f(&doc.text, r.min_width_1(&doc.text), &re)).collect();
    if ranges.is_empty() {
        return cx.editor.set_error(format!("no selections left for /{pattern}/"));
    }
    let n = ranges.len();
    doc.set_selection(Selection::new(ranges, n - 1));
}

/// `s` — matches within the selections become the selection.
pub fn select_regex(cx: &mut Context, pattern: &str) {
    reshape(cx, pattern, |t, r, re| {
        matches_in(t, r.from(), r.to(), re).into_iter().map(|(a, b)| Range::new(a, b)).collect()
    });
}

/// `S` — split selections at matches (matches themselves dropped, empty pieces discarded).
pub fn split_selection(cx: &mut Context, pattern: &str) {
    reshape(cx, pattern, split_on);
}

fn split_on(t: &Rope, r: Range, re: &Regex) -> Vec<Range> {
    let mut out = Vec::new();
    let mut start = r.from();
    for (a, b) in matches_in(t, r.from(), r.to(), re) {
        if a > start {
            out.push(Range::new(start, a));
        }
        start = b;
    }
    if r.to() > start {
        out.push(Range::new(start, r.to()));
    }
    out
}

/// `A-s` — split per line.
pub fn split_selection_on_newline(cx: &mut Context) {
    reshape(cx, r"\r?\n", split_on);
}

/// `K` / `A-K` — keep only selections that match (don't match).
pub fn keep_selections(cx: &mut Context, pattern: &str, remove: bool) {
    reshape(cx, pattern, |t, r, re| {
        let hit = re.is_match(&t.byte_slice(r.from()..r.to()).to_string());
        if hit != remove { vec![r] } else { vec![] }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn find(t: &Rope, re: &Regex, r: Range, reverse: bool) -> Option<((usize, usize), bool)> {
        super::find(t, &t.to_string(), re, r, reverse)
    }

    #[test]
    fn find_wraps_both_ways() {
        let t = Rope::from_str("foo bar foo baz");
        let re = Regex::new("foo").unwrap();
        assert_eq!(find(&t, &re, Range::point(0), false), Some(((8, 11), false)));
        assert_eq!(find(&t, &re, Range::point(9), false), Some(((0, 3), true)));
        assert_eq!(find(&t, &re, Range::point(9), true), Some(((0, 3), false)));
        assert_eq!(find(&t, &re, Range::point(1), true), Some(((8, 11), true)));
    }

    #[test]
    fn find_forward_keeps_look_behind() {
        // From inside "foobar", `\bbar` must not match the "bar" glued to "foo"
        let t = Rope::from_str("foobar bar");
        let re = Regex::new(r"\bbar").unwrap();
        assert_eq!(find(&t, &re, Range::point(2), false), Some(((7, 10), false)));
        // `^` only at the real line start (multi-line mode), not where the search resumes
        let t = Rope::from_str("xab\nab");
        let re = Regex::new(r"(?m)^ab").unwrap();
        assert_eq!(find(&t, &re, Range::point(0), false), Some(((4, 6), false)));
        // Empty matches are skipped without splitting a char
        let t = Rope::from_str("한x");
        let re = Regex::new("x*").unwrap();
        assert_eq!(find(&t, &re, Range::point(0), false), Some(((3, 4), false)));
    }

    #[test]
    fn multibyte_offsets_are_chars() {
        let t = Rope::from_str("타래 편집기 타래");
        let re = Regex::new("타래").unwrap();
        // Bytes: 타래(0..6) ' ' 편집기(7..16) ' ' 타래(17..23)
        assert_eq!(find(&t, &re, Range::point(0), false), Some(((17, 23), false)));
        assert_eq!(matches_in(&t, 0, t.len_bytes(), &re), vec![(0, 6), (17, 23)]);
    }

    #[test]
    fn unicode_classes_we_keep() {
        // regex Unicode features were trimmed for size — check what Hangul needs is still there
        let t = Rope::from_str("abc 타래 XYZ");
        let hangul = Regex::new(r"\p{Hangul}+").unwrap();
        assert_eq!(matches_in(&t, 0, t.len_bytes(), &hangul), vec![(4, 10)]);
        let word = Regex::new(r"\w+").unwrap();
        assert_eq!(matches_in(&t, 0, t.len_bytes(), &word).len(), 3);
        let ci = Regex::new(r"(?i)xyz").unwrap();
        assert_eq!(matches_in(&t, 0, t.len_bytes(), &ci), vec![(11, 14)]);
    }

    #[test]
    fn split_drops_separators() {
        let t = Rope::from_str("a, b,c");
        let re = Regex::new(r",\s*").unwrap();
        assert_eq!(
            split_on(&t, Range::new(0, 6), &re),
            vec![Range::new(0, 1), Range::new(3, 4), Range::new(5, 6)]
        );
    }
}
