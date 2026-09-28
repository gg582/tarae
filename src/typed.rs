//! `:` typed commands. Names and aliases match helix.

use std::path::PathBuf;
use std::process::Command;

use crate::editor::Editor;
use crate::{config, settings};

pub fn execute(editor: &mut Editor, line: &str) -> Result<(), String> {
    let line = line.trim();
    let (cmd, rest) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
    let rest = rest.trim();
    match cmd {
        "" => Ok(()),
        "w" | "write" if rest.is_empty() && editor.format_then_save(false) => Ok(()),
        "w" | "write" => write(editor, rest, false),
        "w!" | "write!" => write(editor, rest, true),
        "reload" => editor.reload_current(),
        "vs" | "vsplit" | "hs" | "sp" | "hsplit" | "split" => {
            let dir = if cmd.starts_with('v') {
                crate::split::Dir::Vertical
            } else {
                crate::split::Dir::Horizontal
            };
            editor.split_view(dir);
            if !rest.is_empty() {
                editor.open(std::path::Path::new(rest)).map_err(|e| format!("{e:#}"))?;
            }
            Ok(())
        }
        "only" => {
            editor.only_view();
            Ok(())
        }
        "close" => {
            if !editor.close_view() {
                return Err("only one window".into());
            }
            Ok(())
        }
        "tutor" => {
            editor.open_tutor();
            Ok(())
        }
        // Attach the debugger to a running program — a name from the config, or host:port (attach.rs)
        "attach" => editor.attach_command(rest),
        // Debugger watch expressions: `:watch expr` adds, `:unwatch [expr]` removes (all if no expr)
        "watch" if !rest.is_empty() => {
            editor.add_watch(rest);
            Ok(())
        }
        "watch" => Err("usage: :watch <expression>".into()),
        "unwatch" => {
            editor.remove_watch(rest);
            editor.note(if rest.is_empty() {
                "Cleared watches".into()
            } else {
                format!("Stopped watching {rest}")
            });
            Ok(())
        }
        "theme" if rest.is_empty() => {
            editor.run_command_by_name("theme_picker");
            Ok(())
        }
        // `:theme name` = this session only (to save: `:set! theme name` or the picker)
        "theme" => set(editor, &format!("theme {rest}"), false),
        // With several panes :q closes only the current pane (as in Helix), :qa closes all
        "q" | "quit" if editor.close_view() => Ok(()),
        "q" | "quit" | "qa" | "quit-all" => quit(editor, false),
        "q!" | "quit!" | "qa!" | "quit-all!" => quit(editor, true),
        "wa" | "write-all" => write_all(editor),
        "wqa" | "xa" | "write-quit-all" => {
            write_all(editor)?;
            quit(editor, false)
        }
        "wq" | "x" | "write-quit" if rest.is_empty() && editor.format_then_save(true) => Ok(()),
        "wq" | "x" | "write-quit" => {
            write(editor, rest, false)?;
            quit(editor, false)
        }
        "o" | "open" | "e" | "edit" => {
            let paths = shell_words(rest);
            if paths.is_empty() {
                return Err("usage: :open <path>...".into());
            }
            for p in paths {
                editor.open(&PathBuf::from(p)).map_err(|e| format!("{e:#}"))?;
            }
            Ok(())
        }
        "n" | "new" => {
            editor.new_scratch();
            Ok(())
        }
        "bc" | "bclose" | "buffer-close" => editor.close_current(false),
        "bc!" | "bclose!" | "buffer-close!" => editor.close_current(true),
        "bn" | "bnext" | "buffer-next" => {
            editor.cycle_buffer(1);
            Ok(())
        }
        "bp" | "bprev" | "buffer-previous" => {
            editor.cycle_buffer(-1);
            Ok(())
        }
        "sh" | "run-shell-command" => shell(editor, rest),
        "pipe" | "|" | "pipe-to" | "insert-output" | "append-output" => {
            use crate::shell::Pipe;
            let kind = match cmd {
                "pipe-to" => Pipe::To,
                "insert-output" => Pipe::Insert,
                "append-output" => Pipe::Append,
                _ => Pipe::Replace,
            };
            editor.shell_pipe(kind, rest);
            Ok(())
        }
        "set" => set(editor, rest, false),
        "set!" => set(editor, rest, true),
        "toggle" => toggle(editor, rest),
        "config-show" => {
            let text = settings::show(&editor.config);
            editor.new_scratch();
            editor.doc_mut().text = ropey::Rope::from_str(&text);
            Ok(())
        }
        "config-open" => {
            let path = config::config_path().ok_or("no config directory")?;
            editor.open(&path).map_err(|e| format!("{e:#}"))
        }
        "config-reload" => {
            editor.events.jobs().spawn(|| {
                let (c, w) = config::load();
                move |ed: &mut Editor| ed.reload_config(c, w)
            });
            Ok(())
        }
        "lsp-restart" => editor.lsp_restart(),
        "lsp-stop" => {
            let name = editor.lsp_stop()?;
            editor.set_status(format!("{name} stopped"));
            Ok(())
        }
        "format" | "fmt" => {
            let tab = editor.config.tab_width;
            let extra = serde_json::json!({ "options": { "tabSize": tab, "insertSpaces": true } });
            editor.lsp_request(crate::lsp_editor::Kind::Format, "textDocument/formatting", extra);
            Ok(())
        }
        "grammar-install" => editor.grammar_install(rest),
        "lang" | "set-language" => {
            let spec = crate::syntax::spec(rest).ok_or_else(|| format!("unknown language '{rest}'"))?;
            let id = editor.doc().id;
            editor.load_language(id, spec);
            Ok(())
        }
        "ask" => crate::llm::ask(editor, rest),
        "ask-cancel" => {
            crate::llm::cancel(editor);
            Ok(())
        }
        "chat" => {
            crate::chat::open(editor);
            if !rest.is_empty()
                && let Some(c) = &mut editor.chat
            {
                c.input = rest.to_string();
                c.cursor = rest.len();
                crate::chat::send(editor);
            }
            Ok(())
        }
        "chat-close" => {
            crate::chat::close(editor);
            Ok(())
        }
        "chat-new" => {
            crate::chat::open(editor);
            crate::chat::reset(editor);
            Ok(())
        }
        n if n.parse::<usize>().is_ok() => {
            editor.goto_line(n.parse::<usize>().unwrap().saturating_sub(1));
            Ok(())
        }
        _ => Err(format!("no such command: '{cmd}'")),
    }
}

/// `:set path` = show current value and description, `:set path value` = this session only,
/// `:set! path value` = also write it to the file.
fn set(editor: &mut Editor, rest: &str, persist: bool) -> Result<(), String> {
    let (path, raw) = rest.split_once(char::is_whitespace).map_or((rest, ""), |(p, v)| (p, v.trim()));
    if path.is_empty() {
        return Err("usage: :set <path> [value]   (:config-show lists all)".into());
    }
    let setting = settings::find(path).ok_or_else(|| format!("unknown setting '{path}' (:config-show)"))?;
    if raw.is_empty() {
        let cur = (setting.get)(&editor.config);
        editor.set_status(format!("{path} = {cur}   — {} ({})", setting.doc, setting.kind.describe()));
        return Ok(());
    }
    let v = settings::parse_value(raw);
    set_value(editor, path, v, persist)
}

fn set_value(editor: &mut Editor, path: &str, v: toml::Value, persist: bool) -> Result<(), String> {
    settings::apply(&mut editor.config, path, &v)?;
    editor.ensure_theme();
    editor.overrides.retain(|(p, _)| p != path);
    if persist {
        let file = config::persist(path, &v)?;
        editor.set_status(format!("{path} = {v}  (saved to {})", file.display()));
    } else {
        editor.set_status(format!("{path} = {v}  (this session — :set! to save)"));
        editor.overrides.push((path.to_string(), v));
    }
    Ok(())
}

/// Bools are flipped; choice settings advance to the next value.
fn toggle(editor: &mut Editor, rest: &str) -> Result<(), String> {
    let path = rest.trim();
    let setting = settings::find(path).ok_or_else(|| format!("unknown setting '{path}'"))?;
    let cur = (setting.get)(&editor.config);
    let next = match (&setting.kind, &cur) {
        (settings::Kind::Bool, toml::Value::Boolean(b)) => toml::Value::Boolean(!b),
        (settings::Kind::Enum(opts), toml::Value::String(s)) => {
            let i = opts.iter().position(|o| o == s).unwrap_or(0);
            toml::Value::from(opts[(i + 1) % opts.len()])
        }
        _ => return Err(format!("{path} is not a toggle ({})", setting.kind.describe())),
    };
    set_value(editor, path, next, false)
}

fn write(editor: &mut Editor, path: &str, force: bool) -> Result<(), String> {
    let id = editor.doc().id;
    let target = match shell_words(path).into_iter().next() {
        Some(p) => {
            let p = std::path::absolute(&p).map_err(|e| e.to_string())?;
            let p = std::fs::canonicalize(&p).unwrap_or(p); // same form as `open`
            (editor.doc().path.as_ref() != Some(&p)).then_some(p)
        }
        None => None,
    };
    match target {
        // Save as: the new file's own stamp counts (not the old file's), and the name changes only once
        // it's written
        Some(new) => {
            let doc = editor.doc_mut();
            let (old, disk, conflict) = (doc.path.replace(new.clone()), doc.disk.take(), doc.disk_conflict);
            doc.disk_conflict = false;
            let saved = doc.save_as(force);
            doc.path = old;
            if let Err(e) = saved {
                (doc.disk, doc.disk_conflict) = (disk, conflict);
                return Err(format!("{e:#}"));
            }
            // Servers forget the old name; language, server and git base follow the new one
            editor.lsp_did_close(id);
            editor.doc_mut().path = Some(new);
            editor.attach_syntax(id);
            editor.attach_lsp(id);
            editor.git_load_base(id);
        }
        None => return save_doc(editor, id, force),
    }
    saved(editor, id);
    Ok(())
}

/// `:wa` — every modified buffer with a file (not ones still loading); errors are collected.
fn write_all(editor: &mut Editor) -> Result<(), String> {
    let ids: Vec<_> = editor
        .docs
        .iter()
        .filter(|d| d.is_modified() && d.path.is_some() && !d.loading && d.virtual_uri.is_none())
        .map(|d| d.id)
        .collect();
    let mut failed = Vec::new();
    for &id in &ids {
        if let Err(e) = save_doc(editor, id, false) {
            failed.push(e);
        }
    }
    if !failed.is_empty() {
        return Err(failed.join(" · "));
    }
    let n = ids.len();
    editor.set_success(if n == 0 { "nothing to save".to_string() } else { format!("Saved {n} file(s)") });
    Ok(())
}

/// Save document `id` to its own path (also when it isn't the current one — format-on-save finishes later).
pub(crate) fn save_doc(editor: &mut Editor, id: crate::document::DocId, force: bool) -> Result<(), String> {
    let doc = editor.docs.iter_mut().find(|d| d.id == id).ok_or("the buffer was closed")?;
    doc.save_as(force).map_err(|e| format!("{e:#}"))?;
    saved(editor, id);
    Ok(())
}

fn saved(editor: &mut Editor, id: crate::document::DocId) {
    let Some(doc) = editor.docs.iter().find(|d| d.id == id) else { return };
    let msg = format!("Saved {} · {} lines", doc.display_name(), doc.text.len_lines());
    editor.set_success(msg);
    editor.lsp_did_save(id);
    editor.undo_persist(id);
}

pub(crate) fn quit(editor: &mut Editor, force: bool) -> Result<(), String> {
    if !force {
        let dirty: Vec<String> = editor
            .docs
            .iter()
            .filter(|d| d.is_modified() && !d.throwaway)
            .map(|d| d.display_name())
            .collect();
        if !dirty.is_empty() {
            return Err(format!("unsaved changes: {} (use :q! to discard)", dirty.join(", ")));
        }
    }
    editor.should_quit = true;
    Ok(())
}

/// Runs a shell command **in the background** and puts the first output line in the status line
/// (helix freezes the editor here).
/// Used by keymaps to open zellij/tmux popups.
fn shell(editor: &mut Editor, cmd: &str) -> Result<(), String> {
    if cmd.is_empty() {
        return Err("usage: :sh <command>".into());
    }
    let cmd = cmd.to_string();
    editor.events.jobs().spawn(move || {
        let result = run_shell(&cmd);
        move |ed: &mut Editor| match result {
            Ok(Some(line)) => ed.set_status(line),
            Ok(None) => {}
            Err(e) => ed.set_error(e),
        }
    });
    Ok(())
}

fn run_shell(cmd: &str) -> Result<Option<String>, String> {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "sh".into());
    let out = Command::new(shell).arg("-c").arg(cmd).output().map_err(|e| e.to_string())?;
    let text = String::from_utf8_lossy(if out.status.success() { &out.stdout } else { &out.stderr });
    let first = text.lines().find(|l| !l.trim().is_empty()).map(str::to_string);
    if out.status.success() {
        Ok(first)
    } else {
        Err(first.unwrap_or_else(|| format!("command failed: {}", out.status)))
    }
}

/// Whitespace splitting + backslash escapes + quotes. Accepts paths the yazi picker passes via `printf %q`.
pub fn shell_words(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_word = false;
    let mut quote: Option<char> = None;
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some('"'), '\\') | (None, '\\') => {
                if let Some(n) = chars.next() {
                    cur.push(n);
                }
                in_word = true;
            }
            (Some(_), c) => cur.push(c),
            (None, '\'' | '"') => {
                quote = Some(c);
                in_word = true;
            }
            (None, c) if c.is_whitespace() => {
                if in_word {
                    out.push(std::mem::take(&mut cur));
                    in_word = false;
                }
            }
            (None, c) => {
                cur.push(c);
                in_word = true;
            }
        }
    }
    if in_word {
        out.push(cur);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_words_handles_escapes_and_quotes() {
        assert_eq!(shell_words(r"a b\ c 'd e' "), vec!["a", "b c", "d e"]);
        assert_eq!(shell_words(r#""x \"y\"""#), vec![r#"x "y""#]);
        assert!(shell_words("   ").is_empty());
    }
}
