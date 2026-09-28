//! Doc comment rendering (like IntelliJ's "Rendered documentation comments") — detection, unwrapping (pure).
//!
//! Strips the markers of `///` `//!` (Rust·C#·Swift …) and `/** … */` (Java·Kotlin·JS·TS·C++ …) and shows
//! them as markdown. **One line stays one line** — only the display changes, so line numbers, scroll and
//! cursor math hold. When the cursor (or selection) enters a block, that whole block shows as source (so it
//! can be edited). Drawing: term.rs.

/// Whether `///`·`//!`·`/** */` are doc comments in this language — `///` inside a markdown code block or
/// in an unknown file is not a doc comment (must be left as is).
pub fn applies(lang: Option<&str>) -> bool {
    matches!(
        lang,
        Some(
            "rust"
                | "c"
                | "cpp"
                | "c-sharp"
                | "swift"
                | "java"
                | "kotlin"
                | "scala"
                | "javascript"
                | "jsx"
                | "typescript"
                | "tsx"
                | "zig"
                | "dart"
                | "php"
                | "groovy"
                | "objc"
        )
    )
}

/// One doc comment line, unwrapped.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Part {
    /// One markdown line (including blank lines).
    Text(String),
    /// ```` ```lang ```` — code block start (language name, empty string if none).
    FenceOpen(String),
    FenceClose,
    /// One line inside a code block.
    Code(String),
}

/// Per line: if a doc comment, (indent bytes, text after the marker). `in_block` = continuing line in `/**`.
/// The returned bool = whether the next line is also inside the block.
pub fn classify(line: &str, in_block: bool) -> (Option<(usize, String)>, bool) {
    let line = line.trim_end_matches(['\n', '\r']);
    let t = line.trim_start();
    let indent = line.len() - t.len();
    let one_space = |s: &str| s.strip_prefix(' ').unwrap_or(s).to_string();
    // Code after the closing `*/` (`/** @type {number} */ let x = 1;`) = a code line, not a doc line
    let code_after = |s: &str, i: usize| !s[i + 2..].trim().is_empty();
    if in_block {
        if let Some(i) = t.find("*/") {
            if code_after(t, i) {
                return (None, false);
            }
            let before = t[..i].trim_end();
            let body = before.strip_prefix('*').unwrap_or(before);
            return (Some((indent, one_space(body))), false);
        }
        let body = t.strip_prefix('*').unwrap_or(t);
        return (Some((indent, one_space(body))), true);
    }
    if (t.starts_with("///") && !t.starts_with("////")) || t.starts_with("//!") {
        return (Some((indent, one_space(&t[3..]))), false);
    }
    if let Some(rest) = t.strip_prefix("/**").filter(|r| !r.starts_with('/') && !r.starts_with('*')) {
        return match rest.find("*/") {
            Some(i) if code_after(rest, i) => (None, false),
            Some(i) => (Some((indent, one_space(rest[..i].trim_end()))), false),
            None => (Some((indent, one_space(rest))), true),
        };
    }
    (None, false)
}

/// Line range [start, end] of the doc comment block containing `line` (within 200 lines up/down — joins
/// a `/** */` that starts above).
pub fn block_at(text: &ropey::Rope, line: usize) -> Option<(usize, usize)> {
    let last = text.len_lines().saturating_sub(1);
    if line > last {
        return None;
    }
    let (w0, w1) = (line.saturating_sub(200), (line + 200).min(last));
    let mut in_block = false;
    let mut is_doc = Vec::with_capacity(w1 + 1 - w0);
    for l in w0..=w1 {
        let s: String = text.line(l).chars().take(2000).collect();
        let (c, next) = classify(&s, in_block);
        in_block = next;
        is_doc.push(c.is_some());
    }
    let i = line - w0;
    if !is_doc[i] {
        return None;
    }
    let start = (0..=i).rev().take_while(|&k| is_doc[k]).last()?;
    let end = (i..is_doc.len()).take_while(|&k| is_doc[k]).last()?;
    Some((w0 + start, w0 + end))
}

/// Text of one block (consecutive doc comment lines) → what to draw per line (inside/outside code blocks).
pub fn parts(bodies: &[String]) -> Vec<Part> {
    let mut in_fence = false;
    bodies
        .iter()
        .map(|b| {
            let t = b.trim_start();
            if let Some(info) = t.strip_prefix("```") {
                in_fence = !in_fence;
                if in_fence {
                    return Part::FenceOpen(info.split([',', ' ']).next().unwrap_or_default().to_string());
                }
                return Part::FenceClose;
            }
            if in_fence { Part::Code(b.clone()) } else { Part::Text(b.clone()) }
        })
        .collect()
}

/// One row of the edit view: one document line, or one row of a folded doc comment block (a rendered piece).
#[derive(Clone, Debug)]
pub enum ViewRow {
    Line(usize),
    /// One screen row of a soft-wrapped line (`index` from 0; `last` = the row the line ends on).
    Part {
        line: usize,
        row: crate::wrap::Row,
        index: usize,
        last: bool,
    },
    Doc {
        /// Document line range of the block [start, end].
        start: usize,
        end: usize,
        indent: usize,
        /// Which row within the block.
        index: usize,
        spans: Vec<crate::markdown::Span>,
        /// Example code row (background one shade deeper).
        code: bool,
    },
}

impl ViewRow {
    /// Document line this screen row points to (first line if folded — clicking goes there and unfolds it).
    pub fn line(&self) -> usize {
        match self {
            ViewRow::Line(l) | ViewRow::Part { line: l, .. } => *l,
            ViewRow::Doc { start, .. } => *start,
        }
    }

    pub fn last_line(&self) -> usize {
        match self {
            ViewRow::Line(l) | ViewRow::Part { line: l, .. } => *l,
            ViewRow::Doc { end, .. } => *end,
        }
    }

    /// For the mouse: (doc line, display column at the row's first text cell, cells before the text,
    /// column the row stops before).
    pub fn origin(&self, left: usize) -> (usize, usize, usize, usize) {
        match self {
            ViewRow::Part { line, row, .. } => (*line, row.col, row.x0, row.end_col),
            _ => (self.line(), left, 0, usize::MAX),
        }
    }
}

/// Chunks of a folded (compacted) doc comment — regrouped by meaning, not by line breaks.
/// Consecutive text lines are merged into one paragraph and rewrapped to the width (markdown rules — same as
/// rustdoc), blank lines become one line between paragraphs (one even if several), code fence lines
/// are dropped.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Chunk {
    Para(String),
    Heading(String),
    Item(String),
    /// `@param x description` — (tag name, rest).
    Tag(String, String),
    /// One line of example code (verbatim).
    Code(String),
    /// Blank line (between paragraphs — several collapse into one, none at start/end).
    Gap,
}

pub fn chunks(parts: &[Part]) -> Vec<Chunk> {
    let mut out: Vec<Chunk> = Vec::new();
    let mut open = false; // can we append to the last text chunk
    for p in parts {
        match p {
            Part::Text(s) => {
                let t = s.trim();
                if t.is_empty() {
                    open = false;
                    if out.last().is_some_and(|c| *c != Chunk::Gap) {
                        out.push(Chunk::Gap);
                    }
                    continue;
                }
                let hashes = t.bytes().take_while(|&b| b == b'#').count();
                if (1..=6).contains(&hashes) && t[hashes..].starts_with(' ') {
                    out.push(Chunk::Heading(t[hashes..].trim().to_string()));
                    open = false;
                } else if let Some(rest) = t.strip_prefix("- ").or_else(|| t.strip_prefix("* ")) {
                    out.push(Chunk::Item(rest.to_string()));
                    open = true;
                } else if let Some(tag) = t.strip_prefix('@') {
                    let (name, rest) = tag.split_once(' ').unwrap_or((tag, ""));
                    out.push(Chunk::Tag(name.to_string(), rest.trim().to_string()));
                    open = true;
                } else if open
                    && let Some(Chunk::Para(p) | Chunk::Item(p) | Chunk::Tag(_, p)) = out.last_mut()
                {
                    if !p.is_empty() {
                        p.push(' ');
                    }
                    p.push_str(t);
                } else {
                    out.push(Chunk::Para(t.to_string()));
                    open = true;
                }
            }
            Part::Code(s) => {
                out.push(Chunk::Code(s.clone()));
                open = false;
            }
            Part::FenceOpen(_) | Part::FenceClose => open = false,
        }
    }
    if out.last() == Some(&Chunk::Gap) {
        out.pop();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rust_and_block_comments() {
        assert_eq!(classify("    /// Returns `x`.", false), (Some((4, "Returns `x`.".into())), false));
        assert_eq!(classify("//! crate docs", false).0, Some((0, "crate docs".into())));
        assert_eq!(classify("//// not a doc", false).0, None);
        assert_eq!(classify("// plain", false).0, None);
        // /** … */ multi-line
        assert_eq!(classify("  /**", false), (Some((2, String::new())), true));
        assert_eq!(
            classify("   * Adds **two** numbers.", true),
            (Some((3, "Adds **two** numbers.".into())), true)
        );
        assert_eq!(classify("   */", true), (Some((3, String::new())), false));
        assert_eq!(classify("/** one line */", false), (Some((0, "one line".into())), false));
        assert_eq!(classify("/**/", false).0, None);
    }

    /// A closing `*/` followed by code ends the block on a code line — the code isn't taken as docs.
    #[test]
    fn code_after_block_close_is_not_doc() {
        assert_eq!(classify("/** @type {number} */ let x = 1;", false), (None, false));
        assert_eq!(classify(" * foo */ bar();", true), (None, false));
        assert_eq!(classify(" * foo */", true), (Some((1, "foo".into())), false));
        let text = ropey::Rope::from_str("/** @type {number} */ let x = 1;\nlet y = 2;\n");
        assert_eq!(block_at(&text, 0), None);
        assert_eq!(block_at(&text, 1), None, "the block closed on line 0");
        let text = ropey::Rope::from_str("/**\n * doc\n */ let a = 1;\nlet b = 2;\n");
        assert_eq!(block_at(&text, 1), Some((0, 1)));
        assert_eq!(block_at(&text, 3), None);
    }

    #[test]
    fn chunks_join_paragraphs_and_drop_blank_lines() {
        let b: Vec<String> = [
            "# Title",
            "",
            "First line",
            "continues here.",
            "",
            "- item one",
            "  more",
            "@param x the x",
            "```",
            "let a = 1;",
            "```",
        ]
        .map(String::from)
        .to_vec();
        assert_eq!(
            chunks(&parts(&b)),
            [
                Chunk::Heading("Title".into()),
                Chunk::Gap,
                Chunk::Para("First line continues here.".into()),
                Chunk::Gap,
                Chunk::Item("item one more".into()),
                Chunk::Tag("param".into(), "x the x".into()),
                Chunk::Code("let a = 1;".into()),
            ]
        );
    }

    #[test]
    fn fences_split_code_from_text() {
        let b: Vec<String> = ["Example:", "```rust", "let x = 1;", "```", "done"].map(String::from).to_vec();
        assert_eq!(
            parts(&b),
            [
                Part::Text("Example:".into()),
                Part::FenceOpen("rust".into()),
                Part::Code("let x = 1;".into()),
                Part::FenceClose,
                Part::Text("done".into())
            ]
        );
    }
}
