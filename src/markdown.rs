//! Markdown → colored lines (shared by hover and completion docs). The light subset language servers send:
//! paragraphs·headings·lists·quotes·rules·code blocks (tree-sitter colored)·inline `code`·**bold**·*italic*·
//! [links](url)·backslash escapes. Colors come from the theme at build time; wrapping happens at draw time.

use ropey::Rope;
use tree_sitter::QueryCursor;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::syntax::{self, Loader, Syntax};
use crate::theme::{Style, Theme};

#[derive(Clone, Debug, PartialEq)]
pub struct Span {
    pub text: String,
    pub style: Style,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Line {
    /// Paragraph — wrapped to width; wrapped lines indented `indent` cells (aligned after a list bullet).
    Text {
        spans: Vec<Span>,
        indent: usize,
    },
    /// One code line — truncated, not wrapped.
    Code(Vec<Span>),
    Rule,
    Blank,
}

impl Line {
    /// Width before wrapping (for sizing the box).
    pub fn width(&self) -> usize {
        match self {
            Line::Text { spans, .. } | Line::Code(spans) => spans.iter().map(|s| s.text.width()).sum(),
            Line::Rule | Line::Blank => 0,
        }
    }
}

/// `default_lang` = language for code blocks without one (usually the current document's language).
pub fn render(src: &str, theme: &Theme, default_lang: Option<&str>) -> Vec<Line> {
    let heading =
        theme.try_get("markup.heading").unwrap_or_default().patch(Style { bold: true, ..Style::default() });
    let quote = theme.try_get("markup.quote").unwrap_or(Style { italic: true, ..Style::default() });
    let bullet = theme.try_get("markup.list").unwrap_or_default();
    let mut out: Vec<Line> = Vec::new();
    // Paragraph being collected: (text, indent, base style, bullet)
    let mut para: Option<(String, usize, Style, Option<Span>)> = None;
    let flush = |para: &mut Option<(String, usize, Style, Option<Span>)>, out: &mut Vec<Line>| {
        if let Some((text, indent, base, head)) = para.take() {
            let mut spans: Vec<Span> = head.into_iter().collect();
            spans.extend(inline(&text, base, theme));
            out.push(Line::Text { spans, indent });
        }
    };
    let mut lines = src.lines();
    while let Some(raw) = lines.next() {
        let l = raw.trim_end();
        let t = l.trim_start();
        let lead = l.len() - t.len();
        if let Some(info) = t.strip_prefix("```").or_else(|| t.strip_prefix("~~~")) {
            flush(&mut para, &mut out);
            let fence = &t[..3];
            let mut code = String::new();
            for c in lines.by_ref() {
                if c.trim_start().starts_with(fence) {
                    break;
                }
                code.push_str(c);
                code.push('\n');
            }
            let lang = info.split([',', ' ']).next().unwrap_or_default().trim();
            // Always one blank line before and after a code block (even if the server sends it flush)
            if !matches!(out.last(), None | Some(Line::Blank)) {
                out.push(Line::Blank);
            }
            out.extend(code_lines(&code, if lang.is_empty() { default_lang } else { Some(lang) }, theme));
            out.push(Line::Blank);
            continue;
        }
        // Reference link definitions (`[Hash]: https://…`) have nothing to show
        if t.starts_with('[') && t.contains("]: ") {
            continue;
        }
        if t.is_empty() {
            flush(&mut para, &mut out);
            if !matches!(out.last(), None | Some(Line::Blank | Line::Rule)) {
                out.push(Line::Blank);
            }
            continue;
        }
        if t.len() >= 3 && ["-", "*", "_"].iter().any(|c| t.chars().all(|x| x.to_string() == *c || x == ' '))
        {
            flush(&mut para, &mut out);
            // A rule is itself a separator, so it swallows blank lines around it
            while matches!(out.last(), Some(Line::Blank)) {
                out.pop();
            }
            if !out.is_empty() {
                out.push(Line::Rule);
            }
            continue;
        }
        let hashes = t.bytes().take_while(|&b| b == b'#').count();
        if (1..=6).contains(&hashes) && t[hashes..].starts_with(' ') {
            flush(&mut para, &mut out);
            out.push(Line::Text { spans: inline(t[hashes..].trim(), heading, theme), indent: 0 });
            continue;
        }
        if let Some(rest) = t.strip_prefix("> ").or_else(|| (t == ">").then_some("")) {
            flush(&mut para, &mut out);
            let head = Span { text: "▎ ".into(), style: quote };
            para = Some((rest.to_string(), 2, quote, Some(head)));
            continue;
        }
        let item = ["- ", "* ", "+ "]
            .iter()
            .find_map(|m| t.strip_prefix(m).map(|r| ("•".to_string(), r)))
            .or_else(|| {
                let digits = t.bytes().take_while(u8::is_ascii_digit).count();
                (digits > 0 && t[digits..].starts_with(". "))
                    .then(|| (t[..digits + 1].to_string(), &t[digits + 2..]))
            });
        if let Some((mark, rest)) = item {
            flush(&mut para, &mut out);
            let head = Span { text: format!("{}{mark} ", " ".repeat(lead)), style: bullet };
            let indent = head.text.width();
            para = Some((rest.to_string(), indent, Style::default(), Some(head)));
            continue;
        }
        match &mut para {
            Some((text, ..)) => {
                text.push(' ');
                text.push_str(t);
            }
            None => para = Some((t.to_string(), 0, Style::default(), None)),
        }
        // Backslash or two spaces at line end = hard line break
        if raw.ends_with("  ") || raw.ends_with('\\') {
            if let Some((text, ..)) = &mut para
                && text.ends_with('\\')
            {
                text.pop();
            }
            flush(&mut para, &mut out);
        }
    }
    flush(&mut para, &mut out);
    while matches!(out.last(), Some(Line::Blank | Line::Rule)) {
        out.pop();
    }
    out
}

/// Inline formatting of one line only (doc-comment rendering — keeps line structure, changes display only).
pub fn inline_spans(text: &str, theme: &Theme) -> Vec<Span> {
    inline(text, Style::default(), theme)
}

/// Inline formatting → spans grouped by style.
fn inline(text: &str, base: Style, theme: &Theme) -> Vec<Span> {
    let code = theme.try_get("markup.raw.inline").or_else(|| theme.try_get("markup.raw")).unwrap_or_default();
    let link = theme.try_get("markup.link.text").unwrap_or(Style { underline: true, ..Style::default() });
    let (mut strong, mut emph) = (false, false);
    // `_`/`__` pairs, tracked apart from `*` so one never closes the other
    let (mut ustrong, mut uemph) = (false, false);
    let mut spans: Vec<Span> = Vec::new();
    let mut push = |s: &str, style: Style| match spans.last_mut() {
        Some(last) if last.style == style => last.text.push_str(s),
        _ => spans.push(Span { text: s.to_string(), style }),
    };
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let style = base.patch(Style { bold: strong || ustrong, italic: emph || uemph, ..Style::default() });
        let c = chars[i];
        match c {
            '\\' if chars.get(i + 1).is_some_and(char::is_ascii_punctuation) => {
                push(&chars[i + 1].to_string(), style);
                i += 2;
            }
            '`' => {
                let ticks = chars[i..].iter().take_while(|&&x| x == '`').count();
                let body = i + ticks;
                let close = (body..chars.len())
                    .find(|&j| chars[j..].iter().take_while(|&&x| x == '`').count() == ticks);
                match close {
                    Some(j) => {
                        let s: String = chars[body..j].iter().collect();
                        push(s.trim(), style.patch(code));
                        i = j + ticks;
                    }
                    None => {
                        push(&"`".repeat(ticks), style);
                        i = body;
                    }
                }
            }
            '*' if chars.get(i + 1) == Some(&'*') => {
                strong = !strong;
                i += 2;
            }
            // Only a paired standalone '*' is italic (multiplication `a * b` stays as-is)
            '*' => {
                let opens = !emph
                    && chars.get(i + 1).is_some_and(|n| !n.is_whitespace())
                    && chars[i + 1..].contains(&'*');
                let closes = emph && i > 0 && !chars[i - 1].is_whitespace();
                if opens || closes {
                    emph = !emph;
                } else {
                    push("*", style);
                }
                i += 1;
            }
            // `_emphasis_`·`__strong__` only at word edges — `snake_case`·`x_1_y` stay as-is
            '_' => {
                let n = if chars.get(i + 1) == Some(&'_') { 2 } else { 1 };
                let open = if n == 2 { ustrong } else { uemph };
                let word =
                    |j: Option<usize>| j.and_then(|j| chars.get(j)).is_some_and(|c| c.is_alphanumeric());
                let space = |j: Option<usize>| j.and_then(|j| chars.get(j)).is_none_or(|c| c.is_whitespace());
                // A closer: not after a space, not followed by a word character
                let closer = |at: usize| !space(at.checked_sub(1)) && !word(Some(at + n));
                let opens = !open
                    && !word(i.checked_sub(1))
                    && !space(Some(i + n))
                    && chars.get(i + n) != Some(&'_')
                    && (i + n + 1..chars.len()).any(|j| {
                        chars[j..].iter().take_while(|&&c| c == '_').count() == n
                            && chars[j - 1] != '_'
                            && closer(j)
                    });
                if opens || (open && closer(i)) {
                    if n == 2 {
                        ustrong = !ustrong;
                    } else {
                        uemph = !uemph;
                    }
                } else {
                    push(&"_".repeat(n), style);
                }
                i += n;
            }
            '[' => {
                // [text](url) → text only (styled as a link). rustdoc reference links [`Hash`]·[Eq][ref] too
                // (only for a single word without spaces — text like `[1, 2]` stays as-is)
                let close = chars[i..].iter().position(|&x| x == ']').map(|p| i + p);
                let end = close.and_then(|j| match chars.get(j + 1) {
                    Some('(') => chars[j..].iter().position(|&x| x == ')').map(|p| j + p),
                    Some('[') => chars[j + 1..].iter().position(|&x| x == ']').map(|p| j + 1 + p),
                    _ => (j > i + 1 && !chars[i + 1..j].iter().any(|c| c.is_whitespace() || *c == ','))
                        .then_some(j),
                });
                match (close, end) {
                    (Some(j), Some(e)) => {
                        let s: String = chars[i + 1..j].iter().collect();
                        for sp in inline(&s, style.patch(link), theme) {
                            push(&sp.text, sp.style);
                        }
                        i = e + 1;
                    }
                    _ => {
                        push("[", style);
                        i += 1;
                    }
                }
            }
            _ => {
                push(&c.to_string(), style);
                i += 1;
            }
        }
    }
    spans
}

/// Code block → lines (same colors as the editor if that language is already loaded — loading a grammar is
/// slow, and this runs on the main thread; usually it's the open doc's language anyway).
pub fn code_lines(code: &str, lang: Option<&str>, theme: &Theme) -> Vec<Line> {
    let code = code.trim_end_matches('\n').replace('\t', "    ");
    let rope = Rope::from_str(&code);
    let mut paint = vec![u32::MAX; code.len()];
    let mut styles: Vec<Option<Style>> = Vec::new();
    // Language name (`rust`) or an extension-like name (`rs`·`py`·`ts`)
    let spec = lang.and_then(syntax::resolve);
    if let Some(data) = spec.and_then(|s| Loader::global().loaded(&s.name)) {
        styles = syntax::capture_styles(|n| theme.try_get(n));
        let mut syn = Syntax::new(data);
        let job = syn.start_parse(&rope);
        let generation = job.generation;
        let tree = job.run();
        syn.finish_parse(generation, tree);
        for (a, b, c) in syntax::highlights(&syn, &rope, 0..code.len(), &mut QueryCursor::new()) {
            paint[a..b.min(code.len())].fill(c);
        }
    }
    let mut out = Vec::new();
    let mut start = 0;
    for line in code.split('\n') {
        let mut spans: Vec<Span> = Vec::new();
        for (off, ch) in line.char_indices() {
            let cap = paint[start + off];
            let style = styles.get(cap as usize).copied().flatten().unwrap_or_default();
            match spans.last_mut() {
                Some(last) if last.style == style => last.text.push(ch),
                _ => spans.push(Span { text: ch.to_string(), style }),
            }
        }
        out.push(Line::Code(spans));
        start += line.len() + 1;
    }
    out
}

/// Screen lines wrapped to width. Paragraphs wrap by word; code is truncated.
pub fn wrap(lines: &[Line], width: usize, rule: Style) -> Vec<Vec<Span>> {
    let mut out = Vec::new();
    for line in lines {
        match line {
            Line::Blank => out.push(Vec::new()),
            Line::Rule => out.push(vec![Span { text: "─".repeat(width), style: rule }]),
            Line::Code(spans) => out.push(cut(spans, width)),
            Line::Text { spans, indent } => {
                // Narrow box: a deep indent mustn't leave no room for text
                let indent = (*indent).min(width / 2);
                let cells: Vec<(char, Style)> =
                    spans.iter().flat_map(|s| s.text.chars().map(move |c| (c, s.style))).collect();
                let mut row: Vec<(char, Style)> = Vec::new();
                let mut w = 0;
                let mut i = 0;
                while i < cells.len() {
                    let (c, _) = cells[i];
                    let cw = c.width().unwrap_or(0);
                    // A row holding only the indent takes the char even if it overflows (else no progress)
                    if w + cw > width && row.len() > indent {
                        // Break at the last space (excluding spaces inside the indent), else mid-word
                        let brk = row.iter().rposition(|(x, _)| *x == ' ').filter(|&p| p >= indent.max(1));
                        let rest: Vec<(char, Style)> = match brk {
                            Some(p) => {
                                let rest = row.split_off(p + 1);
                                row.pop();
                                rest
                            }
                            None => Vec::new(),
                        };
                        out.push(group(&row));
                        row = vec![(' ', Style::default()); indent];
                        row.extend(rest);
                        w = row.iter().map(|(x, _)| x.width().unwrap_or(0)).sum();
                        continue;
                    }
                    row.push(cells[i]);
                    w += cw;
                    i += 1;
                }
                out.push(group(&row));
            }
        }
    }
    out
}

fn group(cells: &[(char, Style)]) -> Vec<Span> {
    let mut spans: Vec<Span> = Vec::new();
    for &(c, style) in cells {
        match spans.last_mut() {
            Some(last) if last.style == style => last.text.push(c),
            _ => spans.push(Span { text: c.to_string(), style }),
        }
    }
    spans
}

fn cut(spans: &[Span], width: usize) -> Vec<Span> {
    let mut out = Vec::new();
    let mut w = 0;
    for s in spans {
        let mut text = String::new();
        for c in s.text.chars() {
            let cw = c.width().unwrap_or(0);
            if w + cw > width {
                break;
            }
            w += cw;
            text.push(c);
        }
        if !text.is_empty() {
            out.push(Span { text, style: s.style });
        }
        if w >= width {
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(lines: &[Vec<Span>]) -> Vec<String> {
        lines.iter().map(|l| l.iter().map(|s| s.text.as_str()).collect()).collect()
    }

    #[test]
    fn blocks_and_inline() {
        let t = Theme::builtin();
        let md = "```rust\nfn get(&self) -> u32\n```\n\n---\n\n# Title\n\nReturns a **reference** to `x`,\nor *none*.\n\n- one\n- two \\* three\n\n[docs](https://x)\n\n\n";
        // Only already-loaded languages are colored (the open doc's, normally)
        let rust = syntax::spec("rust").and_then(|s| Loader::global().load(s).ok()).is_some();
        let lines = render(md, &t, None);
        let text = plain(&wrap(&lines, 40, Style::default()));
        assert_eq!(
            text,
            [
                "fn get(&self) -> u32",
                "────────────────────────────────────────",
                "Title",
                "",
                "Returns a reference to x, or none.",
                "",
                "• one",
                "• two * three",
                "",
                "docs"
            ]
        );
        let Line::Text { spans, .. } = &lines[4] else { panic!() };
        assert!(spans.iter().any(|s| s.text == "reference" && s.style.bold));
        assert!(spans.iter().any(|s| s.text == "none" && s.style.italic));
        assert!(spans.iter().any(|s| s.text == "x" && s.style == t.get("markup.raw")));
        // Code gets colored if the grammar exists (plain text otherwise)
        if rust {
            let Line::Code(spans) = &lines[0] else { panic!() };
            assert!(spans.iter().any(|s| s.text == "fn" && s.style == t.get("keyword")));
        }
    }

    #[test]
    fn underscore_emphasis_only_at_word_edges() {
        let t = Theme::builtin();
        let spans = |s: &str| inline_spans(s, &t);
        let text = |s: &str| spans(s).iter().map(|sp| sp.text.clone()).collect::<String>();
        let italic = |s: &str, word: &str| spans(s).iter().any(|sp| sp.text == word && sp.style.italic);
        let bold = |s: &str, word: &str| spans(s).iter().any(|sp| sp.text == word && sp.style.bold);
        assert!(italic("the _squared_ value", "squared"));
        assert_eq!(text("the _squared_ value"), "the squared value");
        assert!(bold("a __strong__ one", "strong"));
        assert!(italic("(_x_),", "x"), "punctuation around is a word edge");
        // Inside words, alone, or unclosed: literal
        for s in ["snake_case", "x_1_y", "a _ b", "_unclosed", "trailing_", "__", "a__b__c"] {
            assert_eq!(text(s), s);
            assert!(spans(s).iter().all(|sp| !sp.style.italic && !sp.style.bold), "{s}");
        }
        // `_` never closes `*` and vice versa
        assert_eq!(text("*a_b* _c*d_"), "a_b c*d");
        assert!(italic("*a_b*", "a_b"));
    }

    #[test]
    fn wraps_words_and_list_indent() {
        let t = Theme::builtin();
        let lines = render(
            "- 실타래 한 가닥을 이어 붙인다 aaaaaaaaaaaa\n\nsnake_case * 2 [Hash] [`Eq`][e] [1, 2]\n\n[e]: https://x",
            &t,
            None,
        );
        assert_eq!(
            plain(&wrap(&lines, 16, Style::default())),
            [
                "• 실타래 한",
                "  가닥을 이어",
                "  붙인다",
                "  aaaaaaaaaaaa",
                "",
                "snake_case * 2",
                "Hash Eq [1, 2]"
            ]
        );
    }

    #[test]
    fn deep_indent_in_a_narrow_box_still_wraps() {
        let text = |s: &str, indent| Line::Text {
            spans: vec![Span { text: s.into(), style: Style::default() }],
            indent,
        };
        // Indent wider than the box (used to loop forever: rows of only indent)
        let rows = plain(&wrap(&[text("abcdef", 6)], 4, Style::default()));
        assert_eq!(rows, ["abcd", "  ef"]);
        // A wide char that never fits next to the indent still lands somewhere
        let rows = plain(&wrap(&[text("ab타래", 2)], 3, Style::default()));
        assert_eq!(rows.concat().replace(' ', ""), "ab타래");
        assert!(plain(&wrap(&[text("xyz", 3)], 0, Style::default())).len() <= 3);
    }
}
