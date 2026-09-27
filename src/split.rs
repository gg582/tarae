//! Window splits — the split tree (pure part). Leaf = view id, node = direction + children (split evenly).
//! What a view shows (document, scroll) lives in editor.rs; drawing in term.rs.

pub type ViewId = u64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dir {
    /// Side by side (vertical divider) — `C-w v`.
    Vertical,
    /// Stacked (horizontal divider) — `C-w s`.
    Horizontal,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Split {
    Leaf(ViewId),
    Node(Dir, Vec<Split>),
}

/// Screen cell rectangle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: usize,
    pub y: usize,
    pub w: usize,
    pub h: usize,
}

impl Split {
    /// Insert `new` next to `target`. Inside a same-direction node as a sibling, else wrap the leaf
    /// in a new node.
    pub fn split(&mut self, target: ViewId, new: ViewId, dir: Dir) -> bool {
        match self {
            Split::Leaf(v) if *v == target => {
                *self = Split::Node(dir, vec![Split::Leaf(target), Split::Leaf(new)]);
                true
            }
            Split::Leaf(_) => false,
            Split::Node(d, kids) => {
                if *d == dir
                    && let Some(i) = kids.iter().position(|k| *k == Split::Leaf(target))
                {
                    kids.insert(i + 1, Split::Leaf(new));
                    return true;
                }
                kids.iter_mut().any(|k| k.split(target, new, dir))
            }
        }
    }

    /// Remove a leaf. A node left with one child collapses into it. true if removed (the last leaf can't be).
    pub fn remove(&mut self, target: ViewId) -> bool {
        let Split::Node(_, kids) = self else { return false };
        if let Some(i) = kids.iter().position(|k| *k == Split::Leaf(target)) {
            kids.remove(i);
        } else if !kids.iter_mut().any(|k| k.remove(target)) {
            return false;
        }
        if kids.len() == 1 {
            *self = kids.pop().expect("one child");
        }
        true
    }

    pub fn leaves(&self) -> Vec<ViewId> {
        match self {
            Split::Leaf(v) => vec![*v],
            Split::Node(_, kids) => kids.iter().flat_map(Split::leaves).collect(),
        }
    }

    /// Lay out panes: side by side has a 1-cell divider between each; stacked panes touch
    /// (each pane's top line is its title bar, which separates them).
    pub fn layout(&self, r: Rect) -> Vec<(ViewId, Rect)> {
        match self {
            Split::Leaf(v) => vec![(*v, r)],
            Split::Node(dir, kids) => {
                let n = kids.len().max(1);
                let (total, gaps) = match dir {
                    Dir::Vertical => (r.w, n - 1),
                    Dir::Horizontal => (r.h, 0),
                };
                let avail = total.saturating_sub(gaps);
                let mut out = Vec::new();
                let mut at = 0;
                for (i, k) in kids.iter().enumerate() {
                    // the remainder goes one cell each to the leading panes
                    let size = avail / n + usize::from(i < avail % n);
                    let sub = match dir {
                        Dir::Vertical => Rect { x: r.x + at, y: r.y, w: size, h: r.h },
                        Dir::Horizontal => Rect { x: r.x, y: r.y + at, w: r.w, h: size },
                    };
                    out.extend(k.layout(sub));
                    at += size + usize::from(*dir == Dir::Vertical);
                }
                out
            }
        }
    }
}

/// Nearest pane from pane `from` in direction (dx, dy) — overlapping ones preferred.
pub fn neighbor(panes: &[(ViewId, Rect)], from: ViewId, dx: i32, dy: i32) -> Option<ViewId> {
    let (_, a) = *panes.iter().find(|(v, _)| *v == from)?;
    let (acx, acy) = (a.x as i64 * 2 + a.w as i64, a.y as i64 * 2 + a.h as i64);
    panes
        .iter()
        .filter(|(v, _)| *v != from)
        .filter(|(_, b)| match (dx, dy) {
            (1, _) => b.x >= a.x + a.w,
            (-1, _) => b.x + b.w <= a.x,
            (_, 1) => b.y >= a.y + a.h,
            _ => b.y + b.h <= a.y,
        })
        .min_by_key(|(_, b)| {
            let (bcx, bcy) = (b.x as i64 * 2 + b.w as i64, b.y as i64 * 2 + b.h as i64);
            // Distance along the direction + sideways offset (in half cells)
            if dx != 0 {
                ((bcx - acx).abs(), (bcy - acy).abs())
            } else {
                ((bcy - acy).abs(), (bcx - acx).abs())
            }
        })
        .map(|(v, _)| *v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_remove_and_layout() {
        let mut s = Split::Leaf(1);
        assert!(s.split(1, 2, Dir::Vertical));
        assert!(s.split(2, 3, Dir::Vertical), "same direction = sibling");
        assert_eq!(s, Split::Node(Dir::Vertical, vec![Split::Leaf(1), Split::Leaf(2), Split::Leaf(3)]));
        assert!(s.split(3, 4, Dir::Horizontal), "other direction = wrap");
        assert_eq!(s.leaves(), [1, 2, 3, 4]);
        let panes = s.layout(Rect { x: 0, y: 0, w: 32, h: 10 });
        // 32 cells - 2 dividers = 30 → 10 each, the third pane stacked 5·5
        assert_eq!(panes[0].1, Rect { x: 0, y: 0, w: 10, h: 10 });
        assert_eq!(panes[1].1, Rect { x: 11, y: 0, w: 10, h: 10 });
        assert_eq!(panes[2].1, Rect { x: 22, y: 0, w: 10, h: 5 });
        assert_eq!(panes[3].1, Rect { x: 22, y: 5, w: 10, h: 5 });
        assert_eq!(neighbor(&panes, 1, 1, 0), Some(2));
        assert_eq!(neighbor(&panes, 3, 0, 1), Some(4));
        assert_eq!(neighbor(&panes, 4, -1, 0), Some(2));
        assert!(s.remove(3));
        assert!(s.remove(4));
        assert_eq!(s, Split::Node(Dir::Vertical, vec![Split::Leaf(1), Split::Leaf(2)]));
        assert!(s.remove(2));
        assert_eq!(s, Split::Leaf(1), "collapses when one is left");
        assert!(!s.remove(1), "can't remove the last");
    }
}
