//! `gw` — jump labels (helix's `goto_word`): every word on screen (two or more chars) gets a two-letter
//! label, the nearest to the cursor first; typing a label selects that word. Pure: text + visible lines
//! in, targets and labels out.

use ropey::Rope;

use crate::graphemes;
use crate::movement::{self as mv, CharClass};

pub const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz";

/// Words on screen being labelled · the first label char typed so far.
#[derive(Clone, Debug)]
pub struct Labels {
    pub doc: crate::document::DocId,
    /// (word start, word end), in label order.
    pub targets: Vec<(usize, usize)>,
    pub typed: Option<char>,
    /// Select mode (`extend_to_word`): the selection grows to the word instead of becoming it.
    pub extend: bool,
}

impl Labels {
    /// Label of target `i`.
    pub fn label(i: usize) -> (char, char) {
        let n = ALPHABET.len();
        (ALPHABET[(i / n) % n] as char, ALPHABET[i % n] as char)
    }

    /// Target labelled `a` `b`.
    pub fn find(&self, a: char, b: char) -> Option<(usize, usize)> {
        (0..self.targets.len()).find(|&i| Self::label(i) == (a, b)).map(|i| self.targets[i])
    }

    /// Whether any label starts with `a`.
    pub fn starts(&self, a: char) -> bool {
        (0..self.targets.len()).any(|i| Self::label(i).0 == a)
    }
}

/// Words of two or more chars on `lines`, starting at or after `from_col` and before `to_col` (the visible
/// columns) — ordered by distance from `cursor`, alternating after/before, at most 26×26.
pub fn targets(
    text: &Rope,
    lines: &[usize],
    cursor: usize,
    (from_col, to_col): (usize, usize),
    tab: usize,
) -> Vec<(usize, usize)> {
    let mut words = Vec::new();
    for &line in lines {
        let (start, end) = (mv::line_start(text, line), mv::line_end(text, line));
        if end - start > crate::wrap::MAX_LINE {
            continue;
        }
        let mut p = start;
        let mut col = 0;
        while p < end {
            let next = graphemes::next_boundary(text, p);
            let g = text.byte_slice(p..next).to_string();
            let w = graphemes::cluster_width(&g, col, tab);
            if mv::class_at(text, p) == CharClass::Word
                && (p == start || mv::class_at(text, graphemes::prev_boundary(text, p)) != CharClass::Word)
            {
                // A word starts here — find its end
                let mut q = next;
                let mut len = 1;
                while q < end && mv::class_at(text, q) == CharClass::Word {
                    q = graphemes::next_boundary(text, q);
                    len += 1;
                }
                // Both label chars must be on screen
                if len >= 2 && col >= from_col && col + 1 < to_col {
                    words.push((p, q));
                }
            }
            col += w;
            p = next;
        }
    }
    // Nearest first: after and before the cursor in turn
    let split = words.partition_point(|&(s, _)| s <= cursor);
    let (before, after) = words.split_at(split);
    let mut out = Vec::with_capacity(words.len());
    let (mut a, mut b) = (after.iter(), before.iter().rev());
    loop {
        match (a.next(), b.next()) {
            (None, None) => break,
            (x, y) => out.extend(x.into_iter().chain(y).copied()),
        }
    }
    out.truncate(ALPHABET.len() * ALPHABET.len());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_nearest_first_and_labels() {
        let t = Rope::from_str("alpha be c delta\nzeta eta\n");
        let all = targets(&t, &[0, 1], 9, (0, usize::MAX), 4); // cursor on `c`
        let words: Vec<String> = all.iter().map(|&(a, b)| t.byte_slice(a..b).to_string()).collect();
        assert_eq!(words, ["delta", "be", "zeta", "alpha", "eta"], "after/before in turn; `c` is too short");
        assert_eq!(Labels::label(0), ('a', 'a'));
        assert_eq!(Labels::label(27), ('b', 'b'));
        let l = Labels { doc: 0, targets: all, typed: None, extend: false };
        assert_eq!(l.find('a', 'b'), Some((6, 8)));
        assert!(l.starts('a') && !l.starts('b'));
    }

    #[test]
    fn only_visible_columns() {
        let t = Rope::from_str("one two three\n");
        let words = targets(&t, &[0], 0, (4, 9), 4);
        assert_eq!(words, [(4, 7)], "`one` is left of the view; `three` at col 8 would show one label char");
    }
}
