//! Autocompletion — state, filtering, snippet expansion (pure part). Requests/applying in `lsp_editor.rs`.
//!
//! Ask the server once; further typed chars are filtered **here** (nucleo fuzzy, same matcher as pickers) —
//! only lists the server marked "incomplete" are re-requested.

use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};
use ropey::Rope;
use serde_json::Value;

use crate::document::DocId;
use crate::graphemes::{char_at, prev_char};
use crate::lsp::ClientId;

pub struct Item {
    pub label: String,
    pub kind: u8,
    pub detail: String,
    filter: String,
    pub raw: Value,
    /// Whether completionItem/resolve was already sent (docs and auto-import edits are fetched on selection).
    pub resolving: bool,
}

pub struct Completion {
    pub client: ClientId,
    pub doc: DocId,
    /// Which response this list came from — resolve replies carry it (item indices of another list differ).
    pub list: u64,
    /// Start of the word being completed (bytes) — from here to the cursor is the filter text.
    pub start: usize,
    items: Vec<Item>,
    /// Filtered items (items indices, by score).
    pub shown: Vec<usize>,
    /// Match positions in the names of the leading shown items (for highlighting) — only as many as fit.
    pub hits: Vec<Vec<u32>>,
    /// Selected row — none at first (Enter stays a newline; pick with Tab/C-n — like Helix).
    pub selected: Option<usize>,
    pub incomplete: bool,
    /// Docs of the selected item (items index, rendered lines) — built on selection.
    pub docs: Option<(usize, Vec<crate::markdown::Line>)>,
    matcher: Matcher,
}

impl Completion {
    pub fn new(client: ClientId, doc: DocId, start: usize, result: &Value) -> Self {
        let (list, incomplete) = match result {
            Value::Array(a) => (a.clone(), false),
            Value::Object(o) => (
                o.get("items").and_then(Value::as_array).cloned().unwrap_or_default(),
                o.get("isIncomplete").and_then(Value::as_bool).unwrap_or(false),
            ),
            _ => (Vec::new(), false),
        };
        let items = list
            .into_iter()
            .map(|raw| {
                let label = raw["label"].as_str().unwrap_or_default().to_string();
                let detail = raw["labelDetails"]["detail"]
                    .as_str()
                    .or_else(|| raw["detail"].as_str())
                    .unwrap_or_default()
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .to_string();
                let filter = raw["filterText"].as_str().map_or_else(|| label.clone(), str::to_string);
                Item {
                    kind: raw["kind"].as_u64().unwrap_or(0) as u8,
                    label,
                    detail,
                    filter,
                    raw,
                    resolving: false,
                }
            })
            .collect();
        Completion {
            client,
            doc,
            list: 0,
            start,
            items,
            shown: Vec::new(),
            hits: Vec::new(),
            selected: None,
            incomplete,
            docs: None,
            matcher: Matcher::new(Config::DEFAULT),
        }
    }

    pub fn filter(&mut self, prefix: &str) {
        let pat = Pattern::parse(prefix, CaseMatching::Smart, Normalization::Smart);
        let mut buf = Vec::new();
        let mut scored: Vec<(u32, usize)> = self
            .items
            .iter()
            .enumerate()
            .filter_map(|(i, it)| {
                pat.score(Utf32Str::new(&it.filter, &mut buf), &mut self.matcher).map(|s| (s, i))
            })
            .collect();
        // score → server's sortText → original order (stable sort)
        let items = &self.items;
        let key = |i: usize| items[i].raw["sortText"].as_str().unwrap_or(&items[i].label);
        scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| key(a.1).cmp(key(b.1))));
        self.shown = scored.into_iter().map(|(_, i)| i).collect();
        // Highlights index the name (label) — that's what's shown. Only leading items (none past the scroll).
        self.hits = self
            .shown
            .iter()
            .take(HIT_ROWS)
            .map(|&i| {
                let mut idx = Vec::new();
                pat.indices(Utf32Str::new(&self.items[i].label, &mut buf), &mut self.matcher, &mut idx);
                idx.sort_unstable();
                idx.dedup();
                idx
            })
            .collect();
        self.selected = self.selected.filter(|&s| s < self.shown.len());
    }

    pub fn move_by(&mut self, delta: isize) {
        let n = self.shown.len() as isize;
        if n == 0 {
            return;
        }
        self.selected = Some(match self.selected {
            None if delta > 0 => 0,
            None => (n - 1) as usize,
            Some(s) => (s as isize + delta).rem_euclid(n) as usize,
        });
    }

    pub fn current(&self) -> Option<&Item> {
        self.selected.and_then(|s| self.shown.get(s)).map(|&i| &self.items[i])
    }

    pub fn item(&self, shown_idx: usize) -> &Item {
        &self.items[self.shown[shown_idx]]
    }

    /// items index of the selected item.
    pub fn current_index(&self) -> Option<usize> {
        self.selected.and_then(|s| self.shown.get(s)).copied()
    }

    pub fn item_mut(&mut self, index: usize) -> Option<&mut Item> {
        self.items.get_mut(index)
    }

    /// resolve response: overwrite with the received fields (docs, additional edits …).
    pub fn merge_resolved(&mut self, index: usize, resolved: &Value) {
        if let (Some(it), Value::Object(new)) = (self.items.get_mut(index), resolved) {
            if let Value::Object(raw) = &mut it.raw {
                raw.extend(new.clone());
            }
            if let Some(d) = resolved["detail"].as_str().and_then(|d| d.lines().next())
                && it.detail.is_empty()
            {
                it.detail = d.to_string();
            }
        }
        if self.docs.as_ref().is_some_and(|(i, _)| *i == index) {
            self.docs = None;
        }
    }
}

/// Markdown for the docs pane: full signature (code) + documentation.
pub fn doc_markdown(raw: &Value, lang: &str) -> String {
    let mut md = String::new();
    if let Some(d) = raw["detail"].as_str().filter(|d| !d.trim().is_empty()) {
        md.push_str(&format!("```{lang}\n{}\n```\n\n", d.trim()));
    }
    match &raw["documentation"] {
        Value::String(s) => md.push_str(s),
        Value::Object(o) => md.push_str(o.get("value").and_then(Value::as_str).unwrap_or_default()),
        _ => {}
    }
    md.trim().to_string()
}

/// Number of leading items whose highlight positions are precomputed.
const HIT_ROWS: usize = 200;

/// Identifier character (keeps a completion going).
pub fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Start of the identifier running up to `pos`.
pub fn word_start(text: &Rope, pos: usize) -> usize {
    let mut p = pos;
    while p > 0 {
        let q = prev_char(text, p);
        if !is_word(char_at(text, q)) {
            break;
        }
        p = q;
    }
    p
}

/// Kind → one glyph. Only ones every common monospace font has (JetBrains Mono·Hack·Menlo·SF Mono) —
/// ƒ function · τ type · ν value · π constant · § module · ◦ field · # keyword · ¶ snippet.
pub fn kind_glyph(kind: u8) -> &'static str {
    match kind {
        2..=4 => "ƒ",
        5 | 10 => "◦",
        6 | 11 | 12 | 18 => "ν",
        7 | 8 | 13 | 22 | 25 => "τ",
        9 | 17 | 19 => "§",
        14 | 24 => "#",
        15 => "¶",
        20 | 21 => "π",
        _ => "·",
    }
}

/// Kind → theme scope (text colored like code — the list reads like code).
pub fn kind_scope(kind: u8) -> &'static str {
    match kind {
        2 => "function.method",
        3 => "function",
        4 => "constructor",
        5 | 10 => "variable.other.member",
        6 => "variable",
        7 | 8 | 22 | 25 => "type",
        9 => "namespace",
        13 => "type.enum",
        14 => "keyword",
        15 => "string",
        20 => "type.enum.variant",
        21 => "constant",
        _ => "ui.text",
    }
}

/// Snippet (`$1`, `${1:default}`, `${1|a,b|}`, `$0`, `\$`) → (plain text, cursor position).
/// Cursor goes to the first placeholder (lowest number > 0, else `$0`, else the end).
pub fn strip_snippet(s: &str) -> (String, usize) {
    let mut out = String::new();
    let mut stops: Vec<(usize, usize)> = Vec::new(); // (number, byte position)
    parse_snippet(&mut s.chars().peekable(), &mut out, &mut stops, false);
    let cursor = stops
        .iter()
        .filter(|(n, _)| *n > 0)
        .min_by_key(|(n, _)| *n)
        .or_else(|| stops.iter().find(|(n, _)| *n == 0))
        .map_or(out.len(), |(_, p)| *p);
    (out, cursor)
}

fn parse_snippet(
    it: &mut std::iter::Peekable<std::str::Chars>,
    out: &mut String,
    stops: &mut Vec<(usize, usize)>,
    nested: bool,
) {
    let number = |it: &mut std::iter::Peekable<std::str::Chars>| {
        let mut n = String::new();
        while let Some(c) = it.next_if(char::is_ascii_digit) {
            n.push(c);
        }
        n.parse::<usize>().ok()
    };
    while let Some(c) = it.next() {
        match c {
            '\\' => {
                if let Some(e) = it.next() {
                    out.push(e);
                }
            }
            '}' if nested => return,
            '$' => match it.peek() {
                Some(d) if d.is_ascii_digit() => {
                    let n = number(it).unwrap_or(0);
                    stops.push((n, out.len()));
                }
                Some('{') => {
                    it.next();
                    match number(it) {
                        Some(n) => {
                            stops.push((n, out.len()));
                            match it.next() {
                                Some(':') => parse_snippet(it, out, stops, true),
                                Some('|') => {
                                    // choice: first one only
                                    let mut first = true;
                                    while let Some(c) = it.next() {
                                        match c {
                                            '|' => {
                                                it.next_if_eq(&'}');
                                                break;
                                            }
                                            ',' => first = false,
                                            c if first => out.push(c),
                                            _ => {}
                                        }
                                    }
                                }
                                _ => {} // `${1}`
                            }
                        }
                        None => {
                            // variable `${NAME:default}` — default only
                            while it.next_if(|c| *c != ':' && *c != '}').is_some() {}
                            if it.next() == Some(':') {
                                parse_snippet(it, out, stops, true);
                            }
                        }
                    }
                }
                _ => {
                    // `$NAME` variable — dropped
                    while it.next_if(|c| c.is_alphanumeric() || *c == '_').is_some() {}
                }
            },
            c => out.push(c),
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn snippets() {
        assert_eq!(strip_snippet("push(${1:value})$0"), ("push(value)".into(), 5));
        assert_eq!(strip_snippet("fn $1() {\n    $0\n}"), ("fn () {\n    \n}".into(), 3));
        assert_eq!(strip_snippet("${1|a,b|} x"), ("a x".into(), 0));
        assert_eq!(strip_snippet(r"cost \$${1:5}"), ("cost $5".into(), 6));
        assert_eq!(strip_snippet("${1:outer ${2:inner}}"), ("outer inner".into(), 0));
        assert_eq!(strip_snippet("plain"), ("plain".into(), 5));
    }

    #[test]
    fn fuzzy_filter_and_navigation() {
        let result = json!({ "isIncomplete": false, "items": [
            { "label": "push", "kind": 2, "detail": "fn(&mut self, T)" },
            { "label": "pop", "kind": 2 },
            { "label": "len", "kind": 2 },
            { "label": "push_str", "kind": 2, "sortText": "0" },
        ]});
        let mut c = Completion::new(1, 1, 0, &result);
        c.filter("pu");
        let labels: Vec<&str> = (0..c.shown.len()).map(|i| c.item(i).label.as_str()).collect();
        assert_eq!(labels.len(), 2);
        assert!(labels.contains(&"push") && labels.contains(&"push_str"));
        assert!(c.current().is_none(), "nothing selected at first");
        c.move_by(1);
        assert_eq!(c.selected, Some(0), "first press selects the first item");
        c.move_by(-1);
        assert_eq!(c.selected, Some(1), "wraps around going up");
    }

    #[test]
    fn word_start_handles_hangul() {
        let t = Rope::from_str("let 타래_x = a.");
        assert_eq!(word_start(&t, 4 + 6 + 2), 4); // from the end of "타래_x" → start
        assert_eq!(word_start(&t, t.len_bytes()), t.len_bytes(), "empty word after '.'");
    }
}
