//! tarae (타래) — a selection-first modal editor carrying on helix's spirit and keymap.

mod agent;
mod attach;
mod chat;
mod clipboard;
mod cmdline;
mod commands;
mod comment;
mod completion;
mod config;
mod dap;
mod disk;
mod doccomment;
mod document;
mod editdiff;
mod editor;
mod event;
mod git;
mod grammar;
mod graphemes;
mod java;
mod jumplist;
mod key;
mod keymap;
mod labels;
mod llm;
mod lsp;
mod lsp_editor;
mod markdown;
mod movement;
mod offer;
mod pairs;
mod picker;
mod recent;
mod repeat;
mod runtime;
mod search;
mod selection;
mod session;
mod settings;
mod shell;
mod signature;
mod split;
mod structure;
mod syntax;
mod term;
mod test_results;
mod testing;
mod textobject;
mod theme;
mod transaction;
mod typed;
mod undofile;
mod wrap;
mod ws;

use std::path::PathBuf;

use anyhow::Result;

const USAGE: &str = "\
tarae — a selection-first modal editor in the spirit of Helix

usage: tarae [FILE]...
       tarae grammar install [LANG]...       fetch and build syntax grammars (no LANG = all;
                                             needs git and a C compiler) → ~/.local/share/tarae
       tarae grammar list                    which grammars are installed

config: $XDG_CONFIG_HOME/tarae/config.toml (default ~/.config/tarae/config.toml)
        project overrides: .tarae.toml (searched upward from the current directory)
        inside tarae: :set <path> [value], :set! (also saves), :toggle, :config-show";

fn main() -> Result<()> {
    let mut files = Vec::new();
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "-h" | "--help" => {
                println!("{USAGE}");
                return Ok(());
            }
            "grammar" if files.is_empty() => {
                let rest: Vec<String> = std::env::args().skip(2).collect();
                return grammar::cli(&rest);
            }
            "-V" | "--version" => {
                println!("tarae {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            _ => files.push(PathBuf::from(arg)),
        }
    }

    // Theme "default" (meok/hanji) follows the terminal background — before reading config (config reads it)
    if let Some(light) = term::detect_light_background() {
        theme::set_light(light);
    }
    let (config, warnings) = config::load();
    let mut editor = editor::Editor::new(config);
    // Started without a file: recent files on the start screen (existing ones only; reads one small file)
    editor.recent = recent::load();
    editor.session = session::State::load();
    for f in &files {
        if let Err(e) = editor.open(f) {
            editor.set_error(format!("{e:#}"));
        }
    }
    // Started without arguments: this folder's last session (otherwise the start screen)
    if files.is_empty() && editor.config.restore_session {
        editor.restore_session();
    }
    if !files.is_empty() {
        editor.current = 0;
    }
    if !warnings.is_empty() {
        editor.set_error(format!("config: {}", warnings.join("; ")));
    }
    editor.disk_watch();
    editor.agent_start();
    let result = term::run(&mut editor);
    editor.save_session();
    editor.agent = None; // remove the lock file
    result
}
