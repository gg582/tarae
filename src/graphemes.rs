//! Grapheme clusters — units seen as one char (`é` = e + ◌́, NFD Hangul = ᄒ+ᅡ+ᆫ, 👍🏽, 👨‍👩‍👧, `\r\n`).
//! Cursor motion, "char under the cursor", deletion and drawing follow this unit. Positions are byte offsets.
//!
//! Boundaries via `GraphemeCursor` over rope chunks — no copying whole lines (safe even for a 10 MB line).

use ropey::Rope;
use unicode_segmentation::{GraphemeCursor, GraphemeIncomplete, UnicodeSegmentation};
use unicode_width::UnicodeWidthStr;

/// Next cluster boundary after byte `pos` (document end if at the end).
pub fn next_boundary(text: &Rope, pos: usize) -> usize {
    let len = text.len_bytes();
    if pos >= len {
        return len;
    }
    let (mut chunk, mut chunk_start, _, _) = text.chunk_at_byte(pos);
    let mut gc = GraphemeCursor::new(pos, len, true);
    loop {
        match gc.next_boundary(chunk, chunk_start) {
            Ok(None) => return len,
            Ok(Some(b)) => return b,
            Err(GraphemeIncomplete::NextChunk) => {
                chunk_start += chunk.len();
                chunk = text.chunk_at_byte(chunk_start).0;
            }
            Err(GraphemeIncomplete::PreContext(n)) => {
                let ctx = text.chunk_at_byte(n - 1).0;
                gc.provide_context(ctx, n - ctx.len());
            }
            Err(_) => return next_char(text, pos),
        }
    }
}

/// Previous cluster boundary before byte `pos` (0 if at the start).
pub fn prev_boundary(text: &Rope, pos: usize) -> usize {
    let pos = pos.min(text.len_bytes());
    if pos == 0 {
        return 0;
    }
    let (mut chunk, mut chunk_start, _, _) = text.chunk_at_byte(pos);
    let mut gc = GraphemeCursor::new(pos, text.len_bytes(), true);
    loop {
        match gc.prev_boundary(chunk, chunk_start) {
            Ok(None) => return 0,
            Ok(Some(b)) => return b,
            Err(GraphemeIncomplete::PrevChunk) => {
                let (c, s, _, _) = text.chunk_at_byte(chunk_start - 1);
                chunk = c;
                chunk_start = s;
            }
            Err(GraphemeIncomplete::PreContext(n)) => {
                let ctx = text.chunk_at_byte(n - 1).0;
                gc.provide_context(ctx, n - ctx.len());
            }
            Err(_) => return prev_char(text, pos),
        }
    }
}

/// One codepoint step (for places that look at char kinds, like word motion and char find).
pub fn next_char(text: &Rope, pos: usize) -> usize {
    if pos >= text.len_bytes() { text.len_bytes() } else { pos + char_at(text, pos).len_utf8() }
}

pub fn prev_char(text: &Rope, pos: usize) -> usize {
    let c = text.byte_to_char(pos.min(text.len_bytes()));
    if c == 0 { 0 } else { text.char_to_byte(c - 1) }
}

/// The char starting at `pos` (a char boundary).
pub fn char_at(text: &Rope, pos: usize) -> char {
    text.char(text.byte_to_char(pos))
}

/// Screen cells a cluster takes. Tab runs to the next tab stop, newline is one cell (for selection display).
/// Multi-codepoint emoji (ZWJ, skin tone) are drawn two cells wide by terminals, so cap at 2.
pub fn cluster_width(g: &str, col: usize, tab: usize) -> usize {
    match g {
        "\t" => tab - col % tab,
        "\n" | "\r\n" => 1,
        "\r" => 0,
        _ => {
            let mut chars = g.chars();
            let first = chars.next().unwrap_or(' ');
            if chars.next().is_none() {
                if first.is_control() { 1 } else { g.width() }
            } else {
                g.width().min(2)
            }
        }
    }
}

/// A line (or its head) split into screen cells. `pos` is the document byte position, `len` the cluster
/// byte length, `bytes` the range within the given string `s`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cell {
    pub pos: usize,
    pub len: usize,
    pub col: usize,
    pub width: usize,
    pub bytes: std::ops::Range<usize>,
}

/// `s` is the text from the line start, `first` the document position of its first byte.
pub fn cells(s: &str, first: usize, tab: usize) -> Vec<Cell> {
    let mut col = 0;
    s.grapheme_indices(true)
        .map(|(b, g)| {
            let w = cluster_width(g, col, tab);
            let cell = Cell { pos: first + b, len: g.len(), col, width: w, bytes: b..b + g.len() };
            col += w;
            cell
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boundaries_skip_whole_clusters() {
        let t = Rope::from_str("e\u{301}x👍🏽\r\n한");
        // Bytes: é(1+2) x(1) 👍🏽(4+4) \r\n(2) 한(3)
        let mut walk = vec![0];
        while *walk.last().unwrap() < t.len_bytes() {
            walk.push(next_boundary(&t, *walk.last().unwrap()));
        }
        assert_eq!(walk, vec![0, 3, 4, 12, 14, 17]);
        let mut back = vec![t.len_bytes()];
        while *back.last().unwrap() > 0 {
            back.push(prev_boundary(&t, *back.last().unwrap()));
        }
        assert_eq!(back, vec![17, 14, 12, 4, 3, 0]);
    }

    #[test]
    fn boundaries_across_rope_chunks() {
        // Many ZWJ family emoji (18 B) so some straddle chunk boundaries
        let family = "👨\u{200d}👩\u{200d}👧";
        let t = Rope::from_str(&family.repeat(700));
        let (mut pos, mut steps) = (0, 0);
        while pos < t.len_bytes() {
            let next = next_boundary(&t, pos);
            assert_eq!(next - pos, family.len(), "at {pos}");
            pos = next;
            steps += 1;
        }
        assert_eq!(steps, 700);
        assert_eq!(prev_boundary(&t, t.len_bytes()), t.len_bytes() - family.len());
    }

    #[test]
    fn cell_widths() {
        let s = "👨\u{200d}👩\u{200d}👧x\t한e\u{301}";
        let w: Vec<usize> = cells(s, 0, 4).iter().map(|c| c.width).collect();
        assert_eq!(w, vec![2, 1, 1, 2, 1]); // tab is col 3 → 4
        let pos: Vec<usize> = cells(s, 10, 4).iter().map(|c| c.pos).collect();
        assert_eq!(pos, vec![10, 28, 29, 30, 33]); // family emoji 18 B, x 1, tab 1, 한 3
    }
}
