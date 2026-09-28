//! View moves (helix's `z` mode, `Z` = sticky until Esc): `zz`/`zc` put the cursor line mid-screen, `zt` at the
//! top, `zb` at the bottom, `zj`/`zk` scroll a line (the cursor stays unless the view leaves it). `gt` `gc`
//! `gb` do the opposite: the cursor goes to the top / middle / bottom of what's on screen. Rows, not lines,
//! when the document soft-wraps.

use crate::editor::Editor;
use crate::movement as mv;
use crate::selection::Selection;

/// Where to put the cursor line (`z` mode) or the cursor (`g` + t c b).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Place {
    Top,
    Center,
    Bottom,
}

impl Editor {
    /// Screen rows line `l` takes in the focused pane.
    fn rows_of(&self, l: usize) -> usize {
        let doc = self.doc();
        if !self.wraps(doc) {
            return 1;
        }
        crate::wrap::rows(&doc.text, l, self.viewport.1, self.config.tab_width).len()
    }

    fn scrolloff(&self) -> usize {
        self.config.scrolloff.min(self.viewport.0.saturating_sub(1) / 2)
    }

    /// The top line that puts `line` (its first row) `above` rows down the screen.
    fn top_for(&self, line: usize, above: usize) -> usize {
        let mut top = line;
        let mut rows = 0;
        while top > 0 && rows + self.rows_of(top - 1) <= above {
            top -= 1;
            rows += self.rows_of(top);
        }
        top
    }

    /// `zz` `zt` `zb` — scroll so the cursor line sits there (the scrolloff margin still applies).
    pub fn align_view(&mut self, at: Place) {
        let rows = self.viewport.0.max(1);
        let line = self.cursor_line();
        let above = match at {
            Place::Top => self.scrolloff(),
            Place::Center => rows.saturating_sub(self.rows_of(line)) / 2,
            Place::Bottom => rows.saturating_sub(self.rows_of(line) + self.scrolloff()),
        };
        let top = self.top_for(line, above);
        self.doc_mut().top = top;
    }

    /// First and last lines fully on screen, inside the scrolloff margin.
    fn inner_lines(&self) -> (usize, usize) {
        let doc = self.doc();
        let (top, last) = (doc.top, mv::last_line(&doc.text));
        let (rows, so) = (self.viewport.0.max(1), self.scrolloff());
        let first = if top == 0 { 0 } else { (top + so).min(last) };
        let (mut l, mut used) = (top, 0);
        while l < last && used + self.rows_of(l) + self.rows_of(l + 1) <= rows {
            used += self.rows_of(l);
            l += 1;
        }
        let bottom = if l >= last { last } else { l.saturating_sub(so).max(first) };
        (first, bottom)
    }

    /// Put the primary cursor on `line`, same column (select mode: extend).
    fn cursor_to_line(&mut self, line: usize, extend: bool) {
        let tab = self.config.tab_width;
        let doc = self.doc_mut();
        let r = doc.selection().primary();
        let col = mv::cursor_col(&doc.text, r, tab);
        let pos = mv::pos_at_col(&doc.text, line, col, tab);
        let r = r.put_cursor(&doc.text, pos, extend);
        doc.set_selection(Selection::single(r));
    }

    /// `zj` / `zk` — the view moves `delta` lines; the cursor only moves if the view would leave it.
    pub fn scroll_view(&mut self, delta: isize, extend: bool) {
        let last = mv::last_line(&self.doc().text);
        let top = (self.doc().top as isize + delta).clamp(0, last as isize) as usize;
        self.doc_mut().top = top;
        let (first, bottom) = self.inner_lines();
        let line = self.cursor_line();
        if line < first {
            self.cursor_to_line(first, extend);
        } else if line > bottom {
            self.cursor_to_line(bottom, extend);
        }
    }

    /// `gt` `gc` `gb` — the cursor to the top / middle / bottom line on screen.
    pub fn goto_window(&mut self, at: Place, extend: bool) {
        let (first, bottom) = self.inner_lines();
        let line = match at {
            Place::Top => first,
            Place::Bottom => bottom,
            Place::Center => first + (bottom - first) / 2,
        };
        self.cursor_to_line(line, extend);
    }
}
