//! `C-c` — toggle comments. With a line comment token: every non-blank line the selections touch —
//! all commented → uncomment, else comment them at the shallowest indentation (blank lines untouched).
//! Languages with only block comments (HTML, CSS, OCaml …): wrap/unwrap each selection's lines.

use ropey::Rope;

use crate::movement as mv;
use crate::selection::Selection;
use crate::transaction::{Change, Transaction};

/// The change that toggles comments over `sel` — None if there's nothing to comment (blank lines only,
/// or the language has no comment syntax).
pub fn toggle(
    text: &Rope,
    sel: &Selection,
    line_token: Option<&str>,
    block: Option<(&str, &str)>,
) -> Option<Transaction> {
    match (line_token, block) {
        (Some(token), _) => toggle_lines(text, sel, token),
        (None, Some((start, end))) => toggle_blocks(text, sel, start, end),
        (None, None) => None,
    }
}

/// Lines the selections touch, sorted, each once.
fn lines(text: &Rope, sel: &Selection) -> Vec<usize> {
    let mut v: Vec<usize> = sel
        .ranges()
        .iter()
        .flat_map(|&r| {
            let (a, b) = mv::line_span(text, r);
            a..=b
        })
        .collect();
    v.sort_unstable();
    v.dedup();
    v
}

fn toggle_lines(text: &Rope, sel: &Selection, token: &str) -> Option<Transaction> {
    // (line start, first non-blank) of each non-blank line
    let spots: Vec<(usize, usize)> = lines(text, sel)
        .into_iter()
        .map(|l| (mv::line_start(text, l), mv::first_non_whitespace(text, l), mv::line_end(text, l)))
        .filter(|&(_, fnw, end)| fnw < end)
        .map(|(start, fnw, _)| (start, fnw))
        .collect();
    if spots.is_empty() {
        return None;
    }
    let starts_with = |pos: usize, s: &str| {
        let end = (pos + s.len()).min(text.len_bytes());
        text.byte_slice(pos..end) == s
    };
    let commented = spots.iter().all(|&(_, fnw)| starts_with(fnw, token));
    let changes = if commented {
        spots
            .iter()
            .map(|&(_, fnw)| {
                let after = fnw + token.len();
                let space = usize::from(starts_with(after, " "));
                Change::delete(fnw, after + space)
            })
            .collect()
    } else {
        let indent = spots.iter().map(|&(start, fnw)| fnw - start).min().unwrap_or(0);
        spots.iter().map(|&(start, _)| Change::insert(start + indent, format!("{token} "))).collect()
    };
    Some(Transaction::new(changes))
}

fn toggle_blocks(text: &Rope, sel: &Selection, start: &str, end: &str) -> Option<Transaction> {
    // Each selection's lines, trimmed of indentation and trailing blanks
    let mut spans: Vec<(usize, usize)> = sel
        .ranges()
        .iter()
        .filter_map(|&r| {
            let (a, b) = mv::line_span(text, r);
            let from = mv::first_non_whitespace(text, a);
            let to = mv::line_start(text, b) + text.line(b).to_string().trim_end().len();
            (from < to).then_some((from, to))
        })
        .collect();
    spans.sort_unstable();
    spans.dedup();
    if spans.is_empty() {
        return None;
    }
    let mut changes = Vec::new();
    for (from, to) in spans {
        let s = text.byte_slice(from..to).to_string();
        if s.len() >= start.len() + end.len() && s.starts_with(start) && s.ends_with(end) {
            let open = start.len() + usize::from(s[start.len()..].starts_with(' '));
            let inner = &s[..s.len() - end.len()];
            let close = end.len() + usize::from(inner.len() > open && inner.ends_with(' '));
            changes.push(Change::delete(from, from + open));
            changes.push(Change::delete(to - close, to));
        } else {
            changes.push(Change::insert(from, format!("{start} ")));
            changes.push(Change::insert(to, format!(" {end}")));
        }
    }
    Some(Transaction::new(changes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::selection::Range;

    fn run(src: &str, sel: Selection, line: Option<&str>, block: Option<(&str, &str)>) -> String {
        let mut text = Rope::from_str(src);
        if let Some(tx) = toggle(&text, &sel, line, block) {
            tx.apply(&mut text, false);
        }
        text.to_string()
    }

    #[test]
    fn line_comments_at_the_shallowest_indent_and_back() {
        let src = "fn a() {\n    let x = 1;\n\n        y();\n}\n";
        let sel = Selection::single(Range::new(9, 38)); // lines 1..=3
        let on = run(src, sel.clone(), Some("//"), None);
        assert_eq!(on, "fn a() {\n    // let x = 1;\n\n    //     y();\n}\n", "blank line untouched");
        let back = run(&on, Selection::single(Range::new(9, 44)), Some("//"), None);
        assert_eq!(back, src);
    }

    #[test]
    fn mixed_lines_get_commented_and_no_space_is_fine_to_remove() {
        let src = "# a\nb\n";
        assert_eq!(run(src, Selection::single(Range::new(0, 6)), Some("#"), None), "# # a\n# b\n");
        assert_eq!(run("#a\n#b\n", Selection::single(Range::new(0, 6)), Some("#"), None), "a\nb\n");
    }

    #[test]
    fn several_cursors_on_one_line_toggle_it_once() {
        let sel = Selection::new(vec![Range::point(0), Range::point(2)], 0);
        assert_eq!(run("abc\n", sel, Some("--"), None), "-- abc\n");
    }

    #[test]
    fn block_comments_wrap_and_unwrap() {
        let src = "  <p>hi</p>\n";
        let on = run(src, Selection::point(3), None, Some(("<!--", "-->")));
        assert_eq!(on, "  <!-- <p>hi</p> -->\n");
        assert_eq!(run(&on, Selection::point(3), None, Some(("<!--", "-->"))), src);
        assert!(toggle(&Rope::from_str("\n"), &Selection::point(0), Some("//"), None).is_none());
        assert!(toggle(&Rope::from_str("x\n"), &Selection::point(0), None, None).is_none());
    }
}
