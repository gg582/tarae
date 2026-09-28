//! The file picker's tree (`space f` with nothing typed): folders first, then files, each level sorted;
//! a folder shows `+` folded / `−` open and how many files it holds. Typing switches the picker to the flat
//! fuzzy list (picker.rs). Pure — built from the picker's file list, paths relative to its root.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use crate::picker::{Action, Item};

#[derive(Default)]
struct Node {
    dirs: BTreeMap<String, Node>,
    files: Vec<String>,
    count: usize,
}

impl Node {
    fn insert(&mut self, parts: &[&str]) {
        self.count += 1;
        match parts {
            [] => {}
            [file] => self.files.push(file.to_string()),
            [dir, rest @ ..] => self.dirs.entry(dir.to_string()).or_default().insert(rest),
        }
    }
}

pub struct Tree {
    root: PathBuf,
    top: Node,
    /// Open folders (relative, `/`-separated, no trailing slash).
    pub expanded: HashSet<String>,
}

impl Tree {
    /// `files` = the picker's items (`Action::Open` of paths under `root`).
    pub fn new(root: &Path, files: &[Item]) -> Self {
        let mut top = Node::default();
        let mut rels: Vec<String> = files
            .iter()
            .filter_map(|i| match &i.action {
                Action::Open(p) => {
                    Some(p.strip_prefix(root).unwrap_or(p).to_string_lossy().replace('\\', "/"))
                }
                _ => None,
            })
            .collect();
        // Sorted paths insert each folder's files in order
        rels.sort();
        for r in &rels {
            top.insert(&r.split('/').collect::<Vec<_>>());
        }
        Tree { root: root.to_path_buf(), top, expanded: HashSet::new() }
    }

    /// Open every folder above `rel` (a file) — so it shows.
    pub fn reveal(&mut self, rel: &str) {
        let parts: Vec<&str> = rel.split('/').collect();
        for i in 1..parts.len() {
            self.expanded.insert(parts[..i].join("/"));
        }
    }

    pub fn toggle(&mut self, dir: &str) {
        if !self.expanded.remove(dir) {
            self.expanded.insert(dir.to_string());
        }
    }

    /// The rows to show: folders (with their file count) then files, open folders' contents indented.
    pub fn rows(&self) -> Vec<Item> {
        let mut out = Vec::new();
        self.walk(&self.top, "", 0, &mut out);
        out
    }

    fn walk(&self, node: &Node, prefix: &str, depth: usize, out: &mut Vec<Item>) {
        let pad = "  ".repeat(depth);
        for (name, child) in &node.dirs {
            let rel = if prefix.is_empty() { name.clone() } else { format!("{prefix}/{name}") };
            let open = self.expanded.contains(&rel);
            out.push(Item {
                label: format!("{pad}{name}/"),
                action: Action::Dir(rel.clone()),
                hint: child.count.to_string(),
                glyph: Some((if open { "−" } else { "+" }, "ui.virtual")),
            });
            if open {
                self.walk(child, &rel, depth + 1, out);
            }
        }
        for name in &node.files {
            let rel = if prefix.is_empty() { name.clone() } else { format!("{prefix}/{name}") };
            out.push(Item {
                label: format!("{pad}{name}"),
                action: Action::Open(self.root.join(&rel)),
                hint: String::new(),
                glyph: Some((" ", "ui.virtual")),
            });
        }
    }
}

/// The parent folder of a row (`src/a/b.rs` → `src/a`, a top-level entry → None).
pub fn parent(rel: &str) -> Option<&str> {
    rel.rfind('/').map(|i| &rel[..i])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn items(root: &Path, rels: &[&str]) -> Vec<Item> {
        rels.iter()
            .map(|r| Item {
                label: r.to_string(),
                action: Action::Open(root.join(r)),
                hint: String::new(),
                glyph: None,
            })
            .collect()
    }

    fn labels(t: &Tree) -> Vec<String> {
        t.rows().iter().map(|i| format!("{}{}", i.glyph.map_or("", |g| g.0).trim(), i.label)).collect()
    }

    #[test]
    fn folders_first_toggle_and_reveal() {
        let root = Path::new("/r");
        let mut t =
            Tree::new(root, &items(root, &["b.txt", "src/main.rs", "src/ui/card.rs", "a.md", "docs/x.md"]));
        assert_eq!(labels(&t), ["+docs/", "+src/", "a.md", "b.txt"]);
        t.toggle("src");
        assert_eq!(labels(&t), ["+docs/", "−src/", "+  ui/", "  main.rs", "a.md", "b.txt"]);
        assert_eq!(t.rows()[1].hint, "2", "files inside");
        let mut t = Tree::new(root, &items(root, &["src/ui/card.rs", "src/main.rs"]));
        t.reveal("src/ui/card.rs");
        assert!(labels(&t).contains(&"    card.rs".to_string()));
        assert_eq!(parent("src/ui"), Some("src"));
        assert_eq!(parent("src"), None);
    }
}
