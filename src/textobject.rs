//! Text objects (`m` mode) — `mi`/`ma` + target, matching bracket `mm`, surround finding.
//! Pure functions: (text, range) → range. Positions are bytes; char steps use `next_char`/`prev_char`.
//!
//! - Char-based: `w` word, `W` WORD (whitespace-only bounds), `p` paragraph,
//!   pairs `( [ { <` · quotes `" ' `` ` ``
//! - Syntax-based (tree-sitter `textobjects.scm`): `f` function, `t` type/class, `a` argument,
//!   `c` comment, `T` test
//!   — nodes captured under the same name in one match are merged (around like `argument + comma`).

use ropey::Rope;
use tree_sitter::{Query, QueryCursor, StreamingIterator, Tree};

use crate::graphemes::{char_at, next_char, prev_boundary, prev_char};
use crate::movement::{self as mv, CharClass, class_at};
use crate::selection::Range;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Inside,
    Around,
}

/// Target char → pair (open, close). Calling with the closing char gives the same pair.
pub fn pair_of(ch: char) -> Option<(char, char)> {
    Some(match ch {
        '(' | ')' => ('(', ')'),
        '[' | ']' => ('[', ']'),
        '{' | '}' => ('{', '}'),
        '<' | '>' => ('<', '>'),
        '"' | '\'' | '`' => (ch, ch),
        _ => return None,
    })
}

// ── Pairs ────────────────────────────────────────────────────────────────────

/// (open pos, close pos) of the nearest pair surrounding the range. Brackets count nesting.
/// If the cursor is on an opening/closing char, it's that pair.
pub fn find_pair(text: &Rope, r: Range, open: char, close: char) -> Option<(usize, usize)> {
    if open == close {
        return find_quotes(text, r, open);
    }
    let len = text.len_bytes();
    if len == 0 {
        return None;
    }
    // Past the end (cursor after `A<esc>` on a last line without newline) → the last char, not `len - 1`
    // (mid-char for non-ASCII)
    let from = if r.from() >= len { prev_char(text, len) } else { r.from() };
    let to = r.to().max(next_char(text, from));
    // "Cursor on a bracket" means that bracket's pair — only for a one-char selection (cursor). A wider
    // selection finds the pair surrounding it (so `mi(` while covering `(b + c)` widens outward).
    let cursor_like = r.is_empty() || next_char(text, r.from()) >= r.to();
    if cursor_like && char_at(text, from) == close {
        let o = scan_back(text, from, open, close)?;
        return Some((o, from));
    }
    let mut start = if cursor_like && char_at(text, from) == open { next_char(text, from) } else { from };
    loop {
        let o = scan_back(text, start, open, close)?;
        let c = scan_fwd(text, next_char(text, o), open, close)?;
        if c + close.len_utf8() >= to || c >= prev_char(text, to) {
            return Some((o, c));
        }
        start = o; // this pair doesn't cover the whole selection — go one level out
    }
}

/// Unmatched opening bracket before `pos` (exclusive).
fn scan_back(text: &Rope, pos: usize, open: char, close: char) -> Option<usize> {
    let mut it = text.chars_at(text.byte_to_char(pos));
    let (mut p, mut depth) = (pos, 0usize);
    while let Some(c) = it.prev() {
        p -= c.len_utf8();
        if c == close {
            depth += 1;
        } else if c == open {
            if depth == 0 {
                return Some(p);
            }
            depth -= 1;
        }
    }
    None
}

/// Unmatched closing bracket from `pos` (inclusive).
fn scan_fwd(text: &Rope, pos: usize, open: char, close: char) -> Option<usize> {
    let (mut p, mut depth) = (pos, 0usize);
    for c in text.byte_slice(pos..).chars() {
        if c == open {
            depth += 1;
        } else if c == close {
            if depth == 0 {
                return Some(p);
            }
            depth -= 1;
        }
        p += c.len_utf8();
    }
    None
}

/// Quotes don't nest — the one before (inclusive) the cursor on the same line and the one after.
fn find_quotes(text: &Rope, r: Range, q: char) -> Option<(usize, usize)> {
    let line = mv::line_of(text, r.from());
    let (ls, le) = (mv::line_start(text, line), mv::line_end(text, line));
    let from = r.from().min(le);
    let s = text.byte_slice(ls..le).to_string();
    // Through the cursor char — cut at the char end (on Hangul, +1 byte would be mid-char).
    let upto = (next_char(text, from) - ls).min(s.len());
    let open = s[..upto].rfind(q)?;
    let close = open + q.len_utf8() + s[open + q.len_utf8()..].find(q)?;
    Some((ls + open, ls + close))
}

pub fn pair_object(text: &Rope, r: Range, ch: char, kind: Kind) -> Option<Range> {
    let (open, close) = pair_of(ch)?;
    let (o, c) = find_pair(text, r, open, close)?;
    Some(match kind {
        Kind::Inside => Range::new(o + open.len_utf8(), c),
        Kind::Around => Range::new(o, c + close.len_utf8()),
    })
}

/// `mm` — the pair of the bracket under the cursor.
pub fn match_bracket(text: &Rope, pos: usize) -> Option<usize> {
    if pos >= text.len_bytes() {
        return None;
    }
    let c = char_at(text, pos);
    let (open, close) = pair_of(c).filter(|(o, cl)| o != cl)?;
    if c == open {
        scan_fwd(text, next_char(text, pos), open, close)
    } else {
        scan_back(text, pos, open, close)
    }
}

// ── Word and paragraph ───────────────────────────────────────────────────────

pub fn word_object(text: &Rope, r: Range, kind: Kind, long: bool) -> Range {
    let len = text.len_bytes();
    if len == 0 {
        return r;
    }
    let pos = r.cursor(text).min(prev_boundary(text, len));
    let class = |p: usize| {
        let c = class_at(text, p);
        if long && c == CharClass::Punct { CharClass::Word } else { c }
    };
    let k = class(pos);
    if k == CharClass::Eol {
        return r;
    }
    let mut start = pos;
    while start > 0 && class(prev_char(text, start)) == k {
        start = prev_char(text, start);
    }
    let mut end = next_char(text, pos);
    while end < len && class(end) == k {
        end = next_char(text, end);
    }
    if kind == Kind::Around && k != CharClass::Whitespace {
        // Attach trailing whitespace, or leading whitespace if there is none.
        let mut e = end;
        while e < len && class(e) == CharClass::Whitespace {
            e = next_char(text, e);
        }
        if e > end {
            end = e;
        } else {
            while start > 0 && class(prev_char(text, start)) == CharClass::Whitespace {
                start = prev_char(text, start);
            }
        }
    }
    Range::new(start, end)
}

fn blank(text: &Rope, line: usize) -> bool {
    mv::first_non_whitespace(text, line) == mv::line_end(text, line)
}

pub fn paragraph_object(text: &Rope, r: Range, kind: Kind) -> Range {
    let last = mv::last_line(text);
    let (mut a, mut b) = mv::line_span(text, r);
    let is_blank = blank(text, a);
    while a > 0 && blank(text, a - 1) == is_blank {
        a -= 1;
    }
    while b < last && blank(text, b + 1) == is_blank {
        b += 1;
    }
    if kind == Kind::Around && !is_blank {
        while b < last && blank(text, b + 1) {
            b += 1;
        }
    }
    Range::new(mv::line_start(text, a), mv::line_full_end(text, b))
}

// ── Syntax (tree-sitter) ─────────────────────────────────────────────────────

pub fn treesitter_name(ch: char) -> Option<&'static str> {
    Some(match ch {
        'f' => "function",
        't' => "class",
        'a' => "parameter",
        'c' => "comment",
        'T' => "test",
        'e' => "entry",
        _ => return None,
    })
}

/// Smallest `<name>.<inside|around>` covering the range. If already exactly that, the next one out
/// (repeat to widen).
pub fn treesitter_object(
    query: &Query,
    tree: &Tree,
    text: &Rope,
    r: Range,
    name: &str,
    kind: Kind,
) -> Option<Range> {
    let cap = format!("{name}.{}", if kind == Kind::Inside { "inside" } else { "around" });
    let idx = query.capture_names().iter().position(|n| *n == cap)? as u32;
    let (from, to) = (r.from(), r.to().max(r.from() + 1).min(text.len_bytes()));
    let mut cursor = QueryCursor::new();
    cursor.set_byte_range(from..to.max(from));
    let provider = |node: tree_sitter::Node| {
        let len = text.len_bytes();
        let snap = |b: usize| text.char_to_byte(text.byte_to_char(b.min(len)));
        let (s, e) = (snap(node.start_byte()), snap(node.end_byte()));
        text.byte_slice(s..e.max(s)).chunks().map(str::as_bytes)
    };
    let mut best: Option<(usize, usize)> = None;
    let mut matches = cursor.matches(query, tree.root_node(), provider);
    while let Some(m) = matches.next() {
        let mut span: Option<(usize, usize)> = None;
        for c in m.captures().iter().filter(|c| c.index == idx) {
            let (s, e) = (c.node.start_byte(), c.node.end_byte());
            span = Some(span.map_or((s, e), |(a, b)| (a.min(s), b.max(e))));
        }
        let Some((s, e)) = span else { continue };
        let covers = s <= from && e >= to && !(s == r.from() && e == r.to());
        if covers && best.is_none_or(|(bs, be)| e - s < be - bs) {
            best = Some((s, e));
        }
    }
    best.map(|(s, e)| Range::new(s, e))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rope(s: &str) -> Rope {
        Rope::from_str(s)
    }

    #[test]
    fn pairs_nest_and_expand() {
        let t = rope("f(a, (b + c), d)");
        // Cursor on b: inner parens
        assert_eq!(pair_object(&t, Range::point(6), '(', Kind::Inside), Some(Range::new(6, 11)));
        assert_eq!(pair_object(&t, Range::point(6), ')', Kind::Around), Some(Range::new(5, 12)));
        // Selection already covering the inside → outer parens
        assert_eq!(pair_object(&t, Range::new(5, 12), '(', Kind::Inside), Some(Range::new(2, 15)));
        // On an opening bracket → its pair
        assert_eq!(pair_object(&t, Range::point(1), '(', Kind::Inside), Some(Range::new(2, 15)));
        assert_eq!(match_bracket(&t, 1), Some(15));
        assert_eq!(match_bracket(&t, 11), Some(5));
        assert_eq!(match_bracket(&t, 0), None);
    }

    /// Keys fed to an editor holding `text` (like editor.rs `run`).
    fn run(text: &str, keys: &str) -> crate::editor::Editor {
        let mut ed = crate::editor::Editor::new(crate::config::Config::default());
        ed.docs[0].text = rope(text);
        let mut chars = keys.chars();
        while let Some(c) = chars.next() {
            let key: crate::key::Key = if c == '<' {
                chars.by_ref().take_while(|&c| c != '>').collect::<String>().parse().unwrap()
            } else {
                c.to_string().parse().unwrap()
            };
            ed.handle_key(key);
        }
        ed
    }

    #[test]
    fn pair_at_end_of_non_ascii_text() {
        // Cursor past the last char (no trailing newline) — `len - 1` would be mid-char
        for s in ["(한한", "f(한한"] {
            let t = rope(s);
            assert_eq!(pair_object(&t, Range::point(t.len_bytes()), '(', Kind::Inside), None, "{s}");
        }
        let t = rope("(한)한");
        assert_eq!(pair_object(&t, Range::point(t.len_bytes()), '(', Kind::Inside), None);
        let t = rope("(한한)");
        assert_eq!(pair_object(&t, Range::point(t.len_bytes()), '(', Kind::Around), Some(Range::new(0, 8)));
    }

    #[test]
    fn word_keys_keep_combining_marks() {
        let text = |ed: crate::editor::Editor| ed.doc().text.to_string();
        assert_eq!(text(run("cafe\u{301} x", "wd")), "x");
        assert_eq!(text(run("x cafe\u{301} y", "llmiwd")), "x  y");
        assert_eq!(text(run("x cafe\u{301} y", "lllllbd")), "x  y");
    }

    #[test]
    fn quotes_on_line() {
        let t = rope("let s = \"타래 edit\";");
        // '타' is 9..12 (bytes)
        assert_eq!(pair_object(&t, Range::point(9), '"', Kind::Inside), Some(Range::new(9, 20)));
        assert_eq!(pair_object(&t, Range::point(12), '"', Kind::Around), Some(Range::new(8, 21)));
    }

    #[test]
    fn words_and_paragraphs() {
        let t = rope("foo bar.baz qux");
        assert_eq!(word_object(&t, Range::point(5), Kind::Inside, false), Range::new(4, 7));
        assert_eq!(word_object(&t, Range::point(5), Kind::Inside, true), Range::new(4, 11));
        // No trailing whitespace → leading whitespace
        assert_eq!(word_object(&t, Range::point(13), Kind::Around, false), Range::new(11, 15));
        // A combining mark stays with its word (NFD é = e + U+0301)
        let t = rope("x cafe\u{301} y");
        assert_eq!(word_object(&t, Range::point(4), Kind::Inside, false), Range::new(2, 8));
        let t = rope("a\nb\n\n\nc\n");
        assert_eq!(paragraph_object(&t, Range::point(2), Kind::Inside), Range::new(0, 4));
        assert_eq!(paragraph_object(&t, Range::point(2), Kind::Around), Range::new(0, 6));
    }

    #[test]
    fn treesitter_function_when_grammar_available() {
        let Ok(lang) = crate::syntax::Loader::global().load(crate::syntax::spec("rust").unwrap()) else {
            return;
        };
        let Some(q) = &lang.textobjects else { return };
        let src = "fn a() {}\nfn f(x: u8, y: u8) -> u8 {\n    x + y\n}\n";
        let t = rope(src);
        let mut p = tree_sitter::Parser::new();
        p.set_language(&lang.language).unwrap();
        let tree = p.parse(src, None).unwrap();
        let pos = src.find("x + y").unwrap();
        let around = treesitter_object(q, &tree, &t, Range::point(pos), "function", Kind::Around).unwrap();
        assert_eq!(&src[around.from()..around.to()], "fn f(x: u8, y: u8) -> u8 {\n    x + y\n}");
        let pos = src.find("y: u8").unwrap();
        let arg = treesitter_object(q, &tree, &t, Range::point(pos), "parameter", Kind::Inside).unwrap();
        assert_eq!(&src[arg.from()..arg.to()], "y: u8");
    }
}
