//! `:` command-line completion — command names (aliases, descriptions), then arguments depending on
//! the command (file paths, themes, setting paths, values, languages).
//!
//! Pure part: the command table (`COMMANDS` — every command in typed.rs must be here, a test enforces it)
//! and candidate computation (`complete`).
//! Lists that need disk reads (folders, themes) are read by the editor on a worker thread into `Lists`
//! (no disk waits on the key-input path).
//! Drawing is in term.rs (`draw_cmdline_menu`), keys (Tab·Shift-Tab·→) in editor.rs.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};

/// Argument kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Arg {
    None,
    /// File path (possibly several — completes the last word).
    File,
    Theme,
    /// Setting path, then value (if the type is bool or choice).
    Setting,
    /// Only setting paths that can be toggled.
    Toggle,
    Lang,
}

pub struct TypedCmd {
    /// First = the name used when completing, the rest = aliases.
    pub names: &'static [&'static str],
    pub arg: Arg,
    pub doc: &'static str,
}

const fn c(names: &'static [&'static str], arg: Arg, doc: &'static str) -> TypedCmd {
    TypedCmd { names, arg, doc }
}

/// Every `:` command — new commands go in both typed.rs and here (test `every_typed_command_is_listed`).
pub const COMMANDS: &[TypedCmd] = &[
    c(&["write", "w"], Arg::File, "Save (or save as a new path)"),
    c(&["write!", "w!"], Arg::File, "Save, overwriting changes made on disk"),
    c(&["write-quit", "wq", "x"], Arg::File, "Save and quit"),
    c(&["write-all", "wa"], Arg::None, "Save every modified file"),
    c(&["write-quit-all", "wqa", "xa"], Arg::None, "Save every modified file and quit"),
    c(&["quit", "q"], Arg::None, "Close this window, or quit"),
    c(&["quit!", "q!"], Arg::None, "Quit without saving"),
    c(&["quit-all", "qa"], Arg::None, "Quit tarae"),
    c(&["quit-all!", "qa!"], Arg::None, "Quit tarae without saving"),
    c(&["open", "o", "e", "edit"], Arg::File, "Open files"),
    c(&["new", "n"], Arg::None, "New scratch buffer"),
    c(&["reload"], Arg::None, "Reload the file from disk (u brings your version back)"),
    c(&["buffer-close", "bc", "bclose"], Arg::None, "Close this buffer"),
    c(&["buffer-close!", "bc!", "bclose!"], Arg::None, "Close this buffer, dropping changes"),
    c(&["buffer-next", "bn", "bnext"], Arg::None, "Next buffer"),
    c(&["buffer-previous", "bp", "bprev"], Arg::None, "Previous buffer"),
    c(&["vsplit", "vs"], Arg::File, "Split side by side (optionally open a file there)"),
    c(&["hsplit", "hs", "sp", "split"], Arg::File, "Split top and bottom (optionally open a file there)"),
    c(&["only"], Arg::None, "Close every other window"),
    c(&["close"], Arg::None, "Close this window"),
    c(&["theme"], Arg::Theme, "Switch theme (no name = picker with live preview)"),
    c(&["set"], Arg::Setting, "Change a setting for this session (no value = show it)"),
    c(&["set!"], Arg::Setting, "Change a setting and save it to your config"),
    c(&["toggle"], Arg::Toggle, "Flip an on/off setting"),
    c(&["config-open"], Arg::None, "Open your config file"),
    c(&["config-reload"], Arg::None, "Reload your config file"),
    c(&["config-show"], Arg::None, "Show every setting with its value"),
    c(&["format", "fmt"], Arg::None, "Format the file (language server)"),
    c(&["set-language", "lang"], Arg::Lang, "Set this buffer's language"),
    c(
        &["grammar-install"],
        Arg::Lang,
        "Fetch and build syntax grammars (this file's language, a name, or all)",
    ),
    c(&["run-shell-command", "sh"], Arg::None, "Run a shell command in the background"),
    c(&["pipe", "|"], Arg::None, "Pipe the selections through a command (output replaces them)"),
    c(&["pipe-to"], Arg::None, "Send the selections to a command (output ignored)"),
    c(&["insert-output"], Arg::None, "Insert a command's output before each selection"),
    c(&["append-output"], Arg::None, "Insert a command's output after each selection"),
    c(&["ask"], Arg::None, "Ask Claude to edit the selection"),
    c(&["ask-cancel"], Arg::None, "Cancel the Claude request"),
    c(&["chat"], Arg::None, "Open the Claude chat (optionally send a message)"),
    c(&["chat-close"], Arg::None, "Close the Claude chat"),
    c(&["chat-new"], Arg::None, "Start a new Claude chat"),
    c(&["attach"], Arg::None, "Attach the debugger to a running program (name or host:port)"),
    c(&["watch"], Arg::None, "Watch an expression while debugging"),
    c(&["unwatch"], Arg::None, "Stop watching (no expression = all)"),
    c(&["tutor"], Arg::None, "Learn the keys — a 10-minute tutorial"),
];

/// One candidate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cand {
    /// Text to insert (replaces the current word).
    pub insert: String,
    pub label: String,
    /// Dim annotation on the right (alias, description, value kind).
    pub hint: String,
    /// Positions in label that matched the typed text (char indices).
    pub hits: Vec<u32>,
    /// Folder (completing it continues inside).
    pub dir: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Completion {
    /// Where the word being completed starts (byte offset in the input).
    pub start: usize,
    pub cands: Vec<Cand>,
    /// Lists still loading (folders, themes).
    pub loading: bool,
    /// Completing a command name (otherwise an argument).
    pub command: bool,
}

/// Lists read from disk (the editor fills them on a worker thread).
#[derive(Default)]
pub struct Lists {
    /// Folder → (name, is folder). Missing = not read yet.
    pub dirs: HashMap<PathBuf, Vec<(String, bool)>>,
    pub themes: Option<Vec<String>>,
}

pub fn find(name: &str) -> Option<&'static TypedCmd> {
    COMMANDS.iter().find(|c| c.names.contains(&name))
}

/// File argument: split the word into (folder to read, name typed within it). `~` = home.
pub fn file_query(token: &str, cwd: &Path) -> (PathBuf, String) {
    let (dir, prefix) = match token.rfind('/') {
        Some(i) => (&token[..=i], &token[i + 1..]),
        None => ("", token),
    };
    let dir_path = if let Some(rest) = dir.strip_prefix("~/") {
        std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default().join(rest)
    } else if dir.starts_with('/') {
        PathBuf::from(dir)
    } else {
        cwd.join(dir)
    };
    (dir_path, prefix.to_string())
}

/// Filters name candidates by the typed text: exact → prefix match (shorter first) → fuzzy score.
fn rank(query: &str, items: Vec<Cand>, keep_order_when_empty: bool) -> Vec<Cand> {
    if query.is_empty() {
        let mut v = items;
        if !keep_order_when_empty {
            v.sort_by(|a, b| a.label.cmp(&b.label));
        }
        return v;
    }
    let mut matcher = Matcher::new(Config::DEFAULT);
    let pat = Pattern::parse(query, CaseMatching::Smart, Normalization::Smart);
    let mut buf = Vec::new();
    let lower = query.to_lowercase();
    let mut scored: Vec<(u8, usize, u32, usize, Cand)> = items
        .into_iter()
        .enumerate()
        .filter_map(|(i, mut c)| {
            let score = pat.score(Utf32Str::new(&c.label, &mut buf), &mut matcher)?;
            let mut hits = Vec::new();
            pat.indices(Utf32Str::new(&c.label, &mut buf), &mut matcher, &mut hits);
            hits.sort_unstable();
            hits.dedup();
            c.hits = hits;
            let l = c.label.to_lowercase();
            let tier = if l == lower {
                0
            } else if l.starts_with(&lower) {
                1
            } else {
                2
            };
            Some((tier, if tier == 1 { c.label.len() } else { 0 }, u32::MAX - score, i, c))
        })
        .collect();
    scored.sort_by_key(|a| (a.0, a.1, a.2, a.3));
    scored.into_iter().map(|t| t.4).collect()
}

/// Input text (after `:`) → candidates.
pub fn complete(text: &str, cwd: &Path, lists: &Lists) -> Completion {
    // Command name
    let Some(space) = text.find(char::is_whitespace) else {
        let items = COMMANDS
            .iter()
            .map(|c| {
                // Displayed name: exact match → name starting with the typed text → canonical name
                // (typing vs shows vs, wr shows write)
                let label = c
                    .names
                    .iter()
                    .find(|n| **n == text)
                    .or_else(|| c.names.iter().find(|n| !text.is_empty() && n.starts_with(text)))
                    .unwrap_or(&c.names[0]);
                let others: Vec<&str> = c.names.iter().filter(|n| *n != label).copied().collect();
                let hint = if others.is_empty() {
                    c.doc.to_string()
                } else {
                    format!("{}  · {}", c.doc, others.join(" "))
                };
                Cand {
                    insert: label.to_string(),
                    label: label.to_string(),
                    hint,
                    hits: Vec::new(),
                    dir: false,
                }
            })
            .collect();
        let ranked = rank(text, items, true);
        return Completion { start: 0, cands: ranked, loading: false, command: true };
    };
    let name = &text[..space];
    let Some(cmd) = find(name) else { return Completion::default() };
    let args = &text[space..];
    // The word being completed = after the last space (rsplit — NBSP·U+3000 are multi-byte)
    let token = text.rsplit(char::is_whitespace).next().unwrap_or_default();
    let token_start = text.len() - token.len();
    let arg_index = args.split_whitespace().count() - usize::from(!token.is_empty());
    let (cands, loading) = match cmd.arg {
        Arg::None => (Vec::new(), false),
        Arg::File => {
            let (dir, prefix) = file_query(token, cwd);
            match lists.dirs.get(&dir) {
                None => (Vec::new(), true),
                Some(entries) => {
                    let base = &token[..token.len() - prefix.len()];
                    let lower = prefix.to_lowercase();
                    let mut v: Vec<Cand> = entries
                        .iter()
                        .filter(|(n, _)| !n.starts_with('.') || prefix.starts_with('.'))
                        .filter(|(n, _)| n.to_lowercase().starts_with(&lower))
                        .map(|(n, is_dir)| {
                            let label = if *is_dir { format!("{n}/") } else { n.clone() };
                            Cand {
                                insert: format!("{base}{label}"),
                                hits: (0..prefix.chars().count() as u32).collect(),
                                label,
                                hint: String::new(),
                                dir: *is_dir,
                            }
                        })
                        .collect();
                    // Folders first, then by name
                    v.sort_by(|a, b| (!a.dir, &a.label).cmp(&(!b.dir, &b.label)));
                    (v, false)
                }
            }
        }
        Arg::Theme if arg_index == 0 => match &lists.themes {
            None => (Vec::new(), true),
            Some(t) => {
                let items = t
                    .iter()
                    .map(|n| Cand {
                        insert: n.clone(),
                        label: n.clone(),
                        hint: crate::theme::BUILTIN
                            .iter()
                            .find(|(b, _)| *b == n.as_str())
                            .map(|(_, k)| k.to_string())
                            .unwrap_or_default(),
                        hits: Vec::new(),
                        dir: false,
                    })
                    .collect();
                (rank(token, items, true), false)
            }
        },
        Arg::Setting | Arg::Toggle if arg_index == 0 => {
            let items = crate::settings::SETTINGS
                .iter()
                .filter(|s| {
                    cmd.arg == Arg::Setting
                        || matches!(s.kind, crate::settings::Kind::Bool | crate::settings::Kind::Enum(_))
                })
                .map(|s| Cand {
                    insert: s.path.to_string(),
                    label: s.path.to_string(),
                    hint: s.doc.to_string(),
                    hits: Vec::new(),
                    dir: false,
                })
                .collect();
            (rank(token, items, true), false)
        }
        // After `:set path ` = value (bool, choice)
        Arg::Setting if arg_index == 1 => {
            let path = args.split_whitespace().next().unwrap_or_default();
            let values: Vec<String> = match crate::settings::find(path).map(|s| &s.kind) {
                Some(crate::settings::Kind::Bool) => vec!["true".into(), "false".into()],
                Some(crate::settings::Kind::Enum(opts)) => opts.iter().map(|o| format!("\"{o}\"")).collect(),
                _ => Vec::new(),
            };
            let items = values
                .into_iter()
                .map(|v| Cand {
                    insert: v.clone(),
                    label: v,
                    hint: String::new(),
                    hits: Vec::new(),
                    dir: false,
                })
                .collect();
            (rank(token, items, true), false)
        }
        Arg::Lang if arg_index == 0 => {
            let items = crate::syntax::names()
                .map(|n| Cand {
                    insert: n.to_string(),
                    label: n.to_string(),
                    hint: String::new(),
                    hits: Vec::new(),
                    dir: false,
                })
                .collect();
            (rank(token, items, false), false)
        }
        _ => (Vec::new(), false),
    };
    Completion { start: token_start, cands, loading, command: false }
}

/// Input text with the candidate applied.
pub fn apply(text: &str, comp: &Completion, i: usize) -> String {
    let Some(c) = comp.cands.get(i) else { return text.to_string() };
    format!("{}{}", &text[..comp.start.min(text.len())], c.insert)
}

/// Dim suggestion (fish-style): if the first candidate starts with the current word, the rest of it.
pub fn ghost<'a>(text: &str, comp: &'a Completion) -> Option<&'a str> {
    let token = &text[comp.start.min(text.len())..];
    let first = comp.cands.first()?;
    (!token.is_empty() || comp.command).then_some(())?;
    first.insert.strip_prefix(token).filter(|g| !g.is_empty() && !token.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lists() -> Lists {
        let mut l = Lists::default();
        l.dirs.insert(
            PathBuf::from("/p/"),
            vec![
                ("src".into(), true),
                ("Cargo.toml".into(), false),
                (".git".into(), true),
                ("README.md".into(), false),
            ],
        );
        l.dirs.insert(PathBuf::from("/p/src/"), vec![("main.rs".into(), false), ("editor.rs".into(), false)]);
        l.themes = Some(vec!["meok".into(), "hanji".into(), "dracula".into()]);
        l
    }

    fn labels(c: &Completion) -> Vec<&str> {
        c.cands.iter().map(|c| c.label.as_str()).collect()
    }

    #[test]
    fn command_names_prefix_first_then_fuzzy() {
        let l = lists();
        let c = complete("wr", Path::new("/p/"), &l);
        assert!(c.command);
        assert_eq!(c.cands[0].label, "write", "{:?}", labels(&c));
        // Typing an alias puts that alias in the name slot
        let c = complete("vs", Path::new("/p/"), &l);
        assert_eq!(c.cands[0].label, "vs");
        assert!(c.cands[0].hint.contains("vsplit"));
        // Empty input = everything (table order)
        assert_eq!(complete("", Path::new("/p/"), &l).cands.len(), COMMANDS.len());
        // Fuzzy: "cfo" → config-open
        assert!(labels(&complete("cfo", Path::new("/p/"), &l)).contains(&"config-open"));
        assert_eq!(ghost("wr", &complete("wr", Path::new("/p/"), &l)), Some("ite"));
    }

    #[test]
    fn file_arguments_list_the_folder_dirs_first() {
        let l = lists();
        let c = complete("o ", Path::new("/p/"), &l);
        assert_eq!(labels(&c), ["src/", "Cargo.toml", "README.md"], "hidden entries excluded, folders first");
        let c = complete("o src/ma", Path::new("/p/"), &l);
        assert_eq!(labels(&c), ["main.rs"]);
        assert_eq!(apply("o src/ma", &c, 0), "o src/main.rs");
        // Several files: only the last word
        let c = complete("o README.md sr", Path::new("/p/"), &l);
        assert_eq!(apply("o README.md sr", &c, 0), "o README.md src/");
        // Folder not read yet
        assert!(complete("o nowhere/", Path::new("/p/"), &l).loading);
        assert!(labels(&complete("o .g", Path::new("/p/"), &l)).contains(&".git/"), "dot shows hidden too");
    }

    #[test]
    fn themes_settings_values_and_languages() {
        let l = lists();
        assert_eq!(labels(&complete("theme ha", Path::new("/p/"), &l)), ["hanji"]);
        let c = complete("set editor.auto", Path::new("/p/"), &l);
        assert_eq!(c.cands[0].label, "editor.auto-save");
        assert_eq!(
            labels(&complete("set editor.auto-save ", Path::new("/p/"), &l)),
            ["\"off\"", "\"focus\"", "\"idle\""]
        );
        assert!(labels(&complete("toggle ", Path::new("/p/"), &l)).iter().all(|p| {
            crate::settings::find(p).is_some_and(|s| {
                matches!(s.kind, crate::settings::Kind::Bool | crate::settings::Kind::Enum(_))
            })
        }));
        assert_eq!(complete("lang rus", Path::new("/p/"), &l).cands[0].label, "rust");
        assert!(complete("sh ls ", Path::new("/p/"), &l).cands.is_empty(), "command with unknown arguments");
    }

    /// NBSP (Option+Space on macOS) and U+3000 are whitespace wider than one byte.
    #[test]
    fn multibyte_whitespace_does_not_split_a_char() {
        let l = lists();
        let c = complete("o README.md\u{a0}sr", Path::new("/p/"), &l);
        assert_eq!(apply("o README.md\u{a0}sr", &c, 0), "o README.md\u{a0}src/");
        let c = complete("theme\u{3000}ha", Path::new("/p/"), &l);
        assert_eq!(labels(&c), ["hanji"]);
    }

    /// Every command name in typed.rs is in the table (so no command is missing from completion).
    #[test]
    fn every_typed_command_is_listed() {
        let src = include_str!("typed.rs");
        let body = &src[src.find("match cmd {").unwrap()..src.find("fn write(").unwrap()];
        for line in body.lines() {
            let t = line.trim();
            if !t.starts_with('"') || !t.contains("=>") {
                continue;
            }
            for name in t.split("=>").next().unwrap().split(['|', ' ']).filter(|s| s.starts_with('"')) {
                let name = name.trim_matches('"');
                if !name.is_empty() {
                    assert!(find(name).is_some(), ":{name} is missing from cmdline::COMMANDS");
                }
            }
        }
    }
}
