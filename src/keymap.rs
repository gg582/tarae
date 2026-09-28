//! Keymap trie — reads helix `[keys.<mode>]` TOML syntax as is.
//!
//! Values: `"command"` · `":typed command"` · `["command", ...]` (run in sequence) ·
//! table (submode, e.g. `g`, `space`).
//! Defaults are in `default_keys.toml` — read by the same parser, so that file doubles as the docs
//! and a parser test.

use std::collections::HashMap;

use crate::commands::{self, StaticCommand};
use crate::editor::Mode;
use crate::key::Key;

#[derive(Clone, Debug)]
pub enum MappableCommand {
    Static(&'static StaticCommand),
    /// The command line after `:` (e.g. `"w"`, `"sh zellij run -- lazygit"`).
    Typed(String),
}

impl MappableCommand {
    pub fn parse(s: &str) -> Result<Self, String> {
        match s.strip_prefix(':') {
            Some(line) => Ok(Self::Typed(line.trim().to_string())),
            None => commands::find(s).map(Self::Static).ok_or_else(|| format!("unknown command: {s}")),
        }
    }

    /// What it does, in plain words — the command's doc, or the command line itself for `:cmd`.
    pub fn doc(&self) -> String {
        match self {
            Self::Static(c) => c.doc.to_string(),
            Self::Typed(line) => format!(":{line}"),
        }
    }
}

#[derive(Clone, Debug)]
pub enum KeyTrie {
    Leaf(Vec<MappableCommand>),
    Node(Node),
}

#[derive(Clone, Debug, Default)]
pub struct Node {
    map: HashMap<Key, KeyTrie>,
}

impl Node {
    /// Overrides with `other` — submodes merge recursively, everything else is replaced.
    pub fn merge(&mut self, other: Node) {
        for (key, trie) in other.map {
            match trie {
                KeyTrie::Node(sub) => match self.map.get_mut(&key) {
                    Some(KeyTrie::Node(mine)) => mine.merge(sub),
                    _ => {
                        self.map.insert(key, KeyTrie::Node(sub));
                    }
                },
                leaf => {
                    self.map.insert(key, leaf);
                }
            }
        }
    }

    /// Entries in display order (which-key, docs): single characters before named keys (`esc`, `C-w`),
    /// alphabetical, lowercase before uppercase. Total (`A-C`/`A-c` tie otherwise), so the order is stable.
    fn sorted(&self) -> Vec<(&Key, &KeyTrie)> {
        let mut out: Vec<_> = self.map.iter().collect();
        out.sort_by_cached_key(|(k, _)| {
            let s = k.to_string();
            (s.chars().count() > 1, s.to_lowercase(), s.chars().next().is_some_and(char::is_uppercase), s)
        });
        out
    }

    pub fn search(&self, keys: &[Key]) -> Option<&KeyTrie> {
        let mut node = self;
        for (i, key) in keys.iter().enumerate() {
            let trie = node.map.get(key)?;
            if i + 1 == keys.len() {
                return Some(trie);
            }
            match trie {
                KeyTrie::Node(n) => node = n,
                KeyTrie::Leaf(_) => return None,
            }
        }
        None
    }

    /// TOML table → node. Invalid entries are skipped and collected as warnings.
    pub fn from_toml(table: &toml::Table, path: &str, warnings: &mut Vec<String>) -> Node {
        let mut node = Node::default();
        for (k, v) in table {
            let here = format!("{path}.{k}");
            let key = match k.parse::<Key>() {
                Ok(key) => key,
                Err(e) => {
                    warnings.push(format!("{here}: {e}"));
                    continue;
                }
            };
            let names: Vec<&str> = match v {
                toml::Value::Table(t) => {
                    node.map.insert(key, KeyTrie::Node(Node::from_toml(t, &here, warnings)));
                    continue;
                }
                toml::Value::String(s) => vec![s.as_str()],
                toml::Value::Array(a) => a.iter().filter_map(|v| v.as_str()).collect(),
                _ => {
                    warnings.push(format!("{here}: expected command, list or table"));
                    continue;
                }
            };
            let parsed: Result<Vec<_>, _> = names.iter().map(|s| MappableCommand::parse(s)).collect();
            match parsed {
                Ok(cmds) if !cmds.is_empty() => {
                    node.map.insert(key, KeyTrie::Leaf(cmds));
                }
                Ok(_) => warnings.push(format!("{here}: empty command list")),
                Err(e) => warnings.push(format!("{here}: {e}")),
            }
        }
        node
    }
}

pub enum Lookup<'a> {
    Matched(&'a [MappableCommand]),
    Pending,
    NotFound,
}

#[derive(Clone, Debug)]
pub struct Keymaps {
    pub normal: Node,
    pub select: Node,
    pub insert: Node,
}

const DEFAULT_KEYS: &str = include_str!("default_keys.toml");

impl Default for Keymaps {
    fn default() -> Self {
        let table: toml::Table = toml::from_str(DEFAULT_KEYS).expect("default_keys.toml parses");
        let mut warnings = Vec::new();
        let km = Self::from_toml_tables(&table, "default", &mut warnings);
        assert!(warnings.is_empty(), "default keymap: {warnings:?}");
        km
    }
}

/// Names of the built-in key groups (which-key shows them; groups from a user config show `…`).
pub fn group_name(path: &str) -> Option<&'static str> {
    Some(match path {
        "space g" => "Git",
        "space G" => "Debug",
        "space w" | "C-w" => "Window",
        "space x" => "Tests",
        "g" => "Goto",
        "m" => "Match",
        "[" => "Previous",
        "]" => "Next",
        _ => return None,
    })
}

impl Keymaps {
    /// A bundle of `{normal, select, insert}` tables. select = normal with overrides on top (as in helix).
    fn from_toml_tables(table: &toml::Table, path: &str, warnings: &mut Vec<String>) -> Self {
        let part = |mode: &str, warnings: &mut Vec<String>| match table.get(mode) {
            Some(toml::Value::Table(t)) => Node::from_toml(t, &format!("{path}.{mode}"), warnings),
            _ => Node::default(),
        };
        let normal = part("normal", warnings);
        let mut select = normal.clone();
        select.merge(part("select", warnings));
        Self { normal, select, insert: part("insert", warnings) }
    }

    /// Merges the user's `[keys]` table. As in helix, `keys.normal` does not spill into select.
    pub fn merge_user(&mut self, keys: &toml::Table, warnings: &mut Vec<String>) {
        for (mode, node) in
            [("normal", &mut self.normal), ("select", &mut self.select), ("insert", &mut self.insert)]
        {
            if let Some(toml::Value::Table(t)) = keys.get(mode) {
                node.merge(Node::from_toml(t, &format!("keys.{mode}"), warnings));
            }
        }
    }

    fn root(&self, mode: Mode) -> &Node {
        match mode {
            Mode::Normal => &self.normal,
            Mode::Select => &self.select,
            Mode::Insert => &self.insert,
        }
    }

    /// Keys that can be pressed next in a submode (`space`, `g` …) — for the which-key card.
    /// (key, description, is submode). Description = command doc (the line for `:cmd`), lowercase first.
    pub fn children(&self, mode: Mode, keys: &[Key]) -> Vec<(Key, String, bool)> {
        let Some(KeyTrie::Node(node)) = self.root(mode).search(keys) else { return Vec::new() };
        node.sorted()
            .into_iter()
            .map(|(k, t)| match t {
                KeyTrie::Node(_) => {
                    let path: Vec<String> = keys.iter().chain([k]).map(Key::to_string).collect();
                    let name = group_name(&path.join(" ")).map_or("…".to_string(), |n| format!("{n} …"));
                    (*k, name, true)
                }
                KeyTrie::Leaf(cmds) => {
                    let desc = cmds.first().map(|c| c.doc().trim_end_matches(" (LSP)").to_string());
                    (*k, desc.unwrap_or_default(), false)
                }
            })
            .collect()
    }

    /// Command name → shortest key bound to it (normal mode) — shown in the command palette.
    pub fn bindings(&self) -> HashMap<&'static str, String> {
        fn walk(node: &Node, prefix: &str, out: &mut HashMap<&'static str, String>) {
            for (k, t) in &node.map {
                let seq = if prefix.is_empty() { k.to_string() } else { format!("{prefix} {k}") };
                match t {
                    KeyTrie::Node(n) => walk(n, &seq, out),
                    KeyTrie::Leaf(cmds) => {
                        if let [MappableCommand::Static(c)] = cmds.as_slice() {
                            let better =
                                out.get(c.name).is_none_or(|old| (seq.len(), &seq) < (old.len(), old));
                            if better {
                                out.insert(c.name, seq);
                            }
                        }
                    }
                }
            }
        }
        let mut out = HashMap::new();
        walk(&self.normal, "", &mut out);
        out
    }

    pub fn lookup(&self, mode: Mode, keys: &[Key]) -> Lookup<'_> {
        let node = self.root(mode);
        match node.search(keys) {
            Some(KeyTrie::Leaf(cmds)) => Lookup::Matched(cmds),
            Some(KeyTrie::Node(_)) => Lookup::Pending,
            None => Lookup::NotFound,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(s: &str) -> Vec<Key> {
        s.split(' ').map(|k| k.parse().unwrap()).collect()
    }

    fn name(km: &Keymaps, mode: Mode, s: &str) -> Option<String> {
        match km.lookup(mode, &keys(s)) {
            Lookup::Matched(cmds) => Some(cmd_names(cmds).join(",")),
            _ => None,
        }
    }

    /// Command names of a binding (`:line` for typed commands) — several run in sequence.
    fn cmd_names(cmds: &[MappableCommand]) -> Vec<String> {
        cmds.iter()
            .map(|c| match c {
                MappableCommand::Static(c) => c.name.to_string(),
                MappableCommand::Typed(t) => format!(":{t}"),
            })
            .collect()
    }

    fn row(out: &mut String, keys: &str, cmds: &[MappableCommand]) {
        use crate::settings::{md_cell, md_code};
        let names: Vec<String> = cmd_names(cmds).iter().map(|n| md_code(n)).collect();
        // "Extend up, then extend selection to line bounds"
        let docs: Vec<String> = cmds
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let doc = c.doc();
                let mut chars = doc.chars();
                match (i, chars.next(), chars.next()) {
                    (1.., Some(first), Some(second)) if second.is_lowercase() => {
                        format!("{}{}", first.to_lowercase(), &doc[first.len_utf8()..])
                    }
                    _ => doc,
                }
            })
            .map(|d| md_cell(&d))
            .collect();
        out.push_str(&format!("| {} | {} | {} |\n", md_code(keys), names.join(" + "), docs.join(", then ")));
    }

    const TABLE_HEAD: &str = "| Key | Command | Description |\n|---|---|---|\n";

    /// A node's keys (submodes point below), then one table per submode, depth first.
    fn node_tables(out: &mut String, node: &Node, prefix: &mut Vec<String>, heading: &str) {
        out.push_str(&format!("\n{heading}\n\n{TABLE_HEAD}"));
        for (k, t) in node.sorted() {
            let seq = prefix.iter().cloned().chain([k.to_string()]).collect::<Vec<_>>().join(" ");
            match t {
                KeyTrie::Leaf(cmds) => row(out, &seq, cmds),
                KeyTrie::Node(_) => out.push_str(&format!(
                    "| {} | … | More keys — see {} below |\n",
                    crate::settings::md_code(&seq),
                    crate::settings::md_code(&seq)
                )),
            }
        }
        for (k, t) in node.sorted() {
            if let KeyTrie::Node(sub) = t {
                prefix.push(k.to_string());
                let title = format!("### {}", crate::settings::md_code(&prefix.join(" ")));
                node_tables(out, sub, prefix, &title);
                prefix.pop();
            }
        }
    }

    fn leaves<'a>(node: &'a Node, prefix: &[Key], out: &mut Vec<(Vec<Key>, &'a [MappableCommand])>) {
        for (k, t) in node.sorted() {
            let seq: Vec<Key> = prefix.iter().copied().chain([*k]).collect();
            match t {
                KeyTrie::Leaf(cmds) => out.push((seq, cmds)),
                KeyTrie::Node(sub) => leaves(sub, &seq, out),
            }
        }
    }

    /// `docs/reference/keymap.md` — the default keymap per mode and submode (descriptions = command docs,
    /// the same text which-key and the palette show), then every `:` command from `cmdline::COMMANDS`.
    fn reference() -> String {
        use crate::settings::{md_cell, md_code};
        let km = Keymaps::default();
        let mut out = String::from(
            "<!-- Generated from src/default_keys.toml and the command registry — do not edit; regenerate \
             with `TARAE_BLESS=1 cargo test keymap_reference`. -->\n\
             # Default keymap\n\n\
             The keys tarae ships with, from [`src/default_keys.toml`](../../src/default_keys.toml). Command names \
             are the same as Helix's,\nso Helix key configs carry over. Rebind anything in `[keys.normal]`, \
             `[keys.select]`, or `[keys.insert]` —\nsee the [configuration guide](../configuration.md#keys).\n\n\
             In the editor you don't need this page: `space ?` finds any command by what it does, and after a \
             prefix key\n(`space`, `g`, `m`, `[`, `]`) a card shows what comes next.\n",
        );
        node_tables(&mut out, &km.normal, &mut Vec::new(), "## Normal mode");

        out.push_str(
            "\n## Select mode\n\n`v` enters select mode. Every normal-mode key works here too, except these — \
             movement extends the selection instead:\n\n",
        );
        out.push_str(TABLE_HEAD);
        let mut select = Vec::new();
        leaves(&km.select, &[], &mut select);
        for (seq, cmds) in select {
            let same = matches!(km.lookup(Mode::Normal, &seq), Lookup::Matched(n) if cmd_names(n) == cmd_names(cmds));
            if !same {
                let seq = seq.iter().map(Key::to_string).collect::<Vec<_>>().join(" ");
                row(&mut out, &seq, cmds);
            }
        }

        node_tables(&mut out, &km.insert, &mut Vec::new(), "## Insert mode");

        out.push_str(
            "\n## `:` commands\n\nType `:` in normal mode. `Tab` completes command names and their arguments \
             (paths, themes, settings, languages).\n\n| Command | Aliases | Description |\n|---|---|---|\n",
        );
        for c in crate::cmdline::COMMANDS {
            let aliases: Vec<String> = c.names[1..].iter().map(|a| md_code(&format!(":{a}"))).collect();
            out.push_str(&format!(
                "| {} | {} | {} |\n",
                md_code(&format!(":{}", c.names[0])),
                aliases.join(" "),
                md_cell(c.doc)
            ));
        }
        out
    }

    #[test]
    fn keymap_reference_is_up_to_date() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/docs/reference/keymap.md");
        crate::settings::check_generated(path, &reference(), "keymap_reference");
    }

    #[test]
    fn defaults_load() {
        let km = Keymaps::default();
        assert_eq!(name(&km, Mode::Normal, "w").as_deref(), Some("move_next_word_start"));
        assert_eq!(name(&km, Mode::Select, "w").as_deref(), Some("extend_next_word_start"));
        assert_eq!(name(&km, Mode::Normal, "g g").as_deref(), Some("goto_file_start"));
        assert!(matches!(km.lookup(Mode::Normal, &keys("g")), Lookup::Pending));
        // select inherits the rest of normal
        assert_eq!(name(&km, Mode::Select, "d").as_deref(), Some("delete_selection"));
    }

    #[test]
    fn user_config_merges_like_helix() {
        // Part of the user's actual helix config
        let user: toml::Table = toml::from_str(
            r#"
            [normal]
            "A-/" = "repeat_last_motion"
            "X" = ["extend_line_up", "extend_to_line_bounds"]
            C-l = ":sh zellij run -- lazygit"
            g = { a = "goto_last_line" }
            "A-," = "no_such_command"
            "#,
        )
        .unwrap();
        let mut km = Keymaps::default();
        let mut warnings = Vec::new();
        km.merge_user(&user, &mut warnings);
        assert_eq!(name(&km, Mode::Normal, "A-/").as_deref(), Some("repeat_last_motion"));
        assert_eq!(name(&km, Mode::Normal, "X").as_deref(), Some("extend_line_up,extend_to_line_bounds"));
        assert_eq!(name(&km, Mode::Normal, "C-l").as_deref(), Some(":sh zellij run -- lazygit"));
        // Submodes merge — keeps the existing gg + adds ga
        assert_eq!(name(&km, Mode::Normal, "g a").as_deref(), Some("goto_last_line"));
        assert_eq!(name(&km, Mode::Normal, "g g").as_deref(), Some("goto_file_start"));
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("no_such_command"));
    }
}
