//! Auto-pairs in insert mode (helix's rules): an opener gets its closer when the next char isn't a letter
//! or digit (quotes also look at the previous char — `don't` stays one quote); typing a closer that's
//! already next steps over it; backspace between an empty pair deletes both.

use ropey::Rope;

use crate::graphemes;

/// What typing a char at a position does.
#[derive(Debug, PartialEq, Eq)]
pub enum Typed {
    /// Insert the char and this closer, cursor between.
    Pair(char),
    /// Move over the same char already there.
    Skip,
    Plain,
}

fn next(text: &Rope, pos: usize) -> Option<char> {
    (pos < text.len_bytes()).then(|| graphemes::char_at(text, pos))
}

fn prev(text: &Rope, pos: usize) -> Option<char> {
    (pos > 0).then(|| graphemes::char_at(text, graphemes::prev_char(text, pos)))
}

fn not_alnum(c: Option<char>) -> bool {
    c.is_none_or(|c| !c.is_alphanumeric())
}

pub fn typed(text: &Rope, pos: usize, c: char, pairs: &[(char, char)]) -> Typed {
    let (before, after) = (prev(text, pos), next(text, pos));
    for &(open, close) in pairs {
        if open == close && c == open {
            return if after == Some(c) {
                Typed::Skip
            } else if not_alnum(after) && not_alnum(before) {
                Typed::Pair(close)
            } else {
                Typed::Plain
            };
        }
        if c == close && after == Some(c) {
            return Typed::Skip;
        }
        if c == open && not_alnum(after) {
            return Typed::Pair(close);
        }
    }
    Typed::Plain
}

/// Backspace at `pos` sits inside an empty pair — delete the closer too.
pub fn inside_pair(text: &Rope, pos: usize, pairs: &[(char, char)]) -> bool {
    matches!((prev(text, pos), next(text, pos)), (Some(a), Some(b)) if pairs.contains(&(a, b)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::syntax::DEFAULT_PAIRS as P;

    fn at(s: &str, c: char) -> Typed {
        let pos = s.find('|').unwrap();
        let text = Rope::from_str(&s.replace('|', ""));
        typed(&text, pos, c, P)
    }

    #[test]
    fn helix_rules() {
        assert_eq!(at("f|", '('), Typed::Pair(')'));
        assert_eq!(at("f| x", '('), Typed::Pair(')'));
        assert_eq!(at("f|x", '('), Typed::Plain, "before a word char: no closer");
        assert_eq!(at("f(|)", ')'), Typed::Skip);
        assert_eq!(at("f(|", ')'), Typed::Plain);
        assert_eq!(at("x = |", '"'), Typed::Pair('"'));
        assert_eq!(at("don|", '\''), Typed::Plain, "apostrophe after a letter");
        assert_eq!(at("\"|\"", '"'), Typed::Skip);
        assert_eq!(at("|한", '"'), Typed::Plain, "letters in any script");
    }

    #[test]
    fn backspace_inside_an_empty_pair() {
        let t = Rope::from_str("f()");
        assert!(inside_pair(&t, 2, P));
        assert!(!inside_pair(&t, 1, P));
        assert!(!inside_pair(&Rope::from_str("(x)"), 2, P));
    }
}
