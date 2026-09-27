//! Settings schema — one setting = one row of (path, kind, description, get, set).
//! File parsing, `:set`/`:toggle`, `:config-show` and (M4) the settings picker all use this one table.
//! New setting = one row here + an `EditorConfig` field.

use toml::Value;

use crate::config::{CursorShape, EditorConfig, LineNumber};

pub enum Kind {
    Bool,
    Int { min: i64, max: i64 },
    Enum(&'static [&'static str]),
    Str,
    StrList,
}

impl Kind {
    pub fn check(&self, v: &Value) -> Result<(), String> {
        let ok = match (self, v) {
            (Kind::Bool, Value::Boolean(_)) => true,
            (Kind::Int { min, max }, Value::Integer(n)) => (min..=max).contains(&n),
            (Kind::Enum(opts), Value::String(s)) => opts.contains(&s.as_str()),
            (Kind::Str, Value::String(_)) => true,
            (Kind::StrList, Value::Array(a)) => a.iter().all(Value::is_str),
            _ => false,
        };
        if ok { Ok(()) } else { Err(format!("expected {}, got {v}", self.describe())) }
    }

    pub fn describe(&self) -> String {
        match self {
            Kind::Bool => "true | false".into(),
            Kind::Int { min, max } => format!("integer {min}..={max}"),
            Kind::Enum(opts) => opts.iter().map(|o| format!("\"{o}\"")).collect::<Vec<_>>().join(" | "),
            Kind::Str => "string".into(),
            Kind::StrList => "list of strings".into(),
        }
    }
}

pub struct Setting {
    pub path: &'static str,
    pub kind: Kind,
    pub doc: &'static str,
    pub get: fn(&EditorConfig) -> Value,
    /// Values come in only after passing `kind.check`.
    pub set: fn(&mut EditorConfig, &Value),
}

fn s(v: &Value) -> &str {
    v.as_str().unwrap_or_default()
}

fn b(v: &Value) -> bool {
    v.as_bool().unwrap_or_default()
}

fn n(v: &Value) -> usize {
    v.as_integer().unwrap_or_default().max(0) as usize
}

const SHAPES: &[&str] = &["block", "bar", "underline"];

fn shape_str(c: CursorShape) -> Value {
    Value::from(match c {
        CursorShape::Block => "block",
        CursorShape::Bar => "bar",
        CursorShape::Underline => "underline",
    })
}

fn shape(v: &Value) -> CursorShape {
    match s(v) {
        "bar" => CursorShape::Bar,
        "underline" => CursorShape::Underline,
        _ => CursorShape::Block,
    }
}

pub static SETTINGS: &[Setting] = &[
    Setting {
        path: "theme",
        kind: Kind::Str,
        doc: "Theme name — built-in meok, hanji, meok-transparent, hanji-transparent, or a file in ~/.config/tarae/themes; \"default\" = meok/hanji by terminal background",
        get: |c| Value::from(c.theme.as_str()),
        set: |c, v| c.theme = s(v).to_string(),
    },
    Setting {
        path: "editor.line-numbers",
        kind: Kind::Enum(&["absolute", "relative"]),
        doc: "Gutter line numbers",
        get: |c| {
            Value::from(match c.line_number {
                LineNumber::Absolute => "absolute",
                LineNumber::Relative => "relative",
            })
        },
        set: |c, v| {
            c.line_number = if s(v) == "relative" { LineNumber::Relative } else { LineNumber::Absolute }
        },
    },
    Setting {
        path: "editor.scrolloff",
        kind: Kind::Int { min: 0, max: 100 },
        doc: "Lines kept visible above/below the cursor",
        get: |c| Value::from(c.scrolloff as i64),
        set: |c, v| c.scrolloff = n(v),
    },
    Setting {
        path: "editor.tab-width",
        kind: Kind::Int { min: 1, max: 16 },
        doc: "Display width of a tab, and spaces inserted by <tab>",
        get: |c| Value::from(c.tab_width as i64),
        set: |c, v| c.tab_width = n(v),
    },
    Setting {
        path: "editor.color-modes",
        kind: Kind::Bool,
        doc: "Color the mode indicator by mode",
        get: |c| Value::from(c.color_modes),
        set: |c, v| c.color_modes = b(v),
    },
    Setting {
        path: "editor.cursorline",
        kind: Kind::Bool,
        doc: "Highlight the line of the primary cursor (theme: ui.cursorline.primary)",
        get: |c| Value::from(c.cursorline),
        set: |c, v| c.cursorline = b(v),
    },
    Setting {
        path: "editor.header",
        kind: Kind::Bool,
        doc: "Top bar: path › definition under the cursor, and open buffers",
        get: |c| Value::from(c.header),
        set: |c, v| c.header = b(v),
    },
    Setting {
        path: "editor.inlay-hints",
        kind: Kind::Bool,
        doc: "Show language server inlay hints (types, parameter names) as dim virtual text",
        get: |c| Value::from(c.inlay_hints),
        set: |c, v| c.inlay_hints = b(v),
    },
    Setting {
        path: "editor.auto-save",
        kind: Kind::Enum(&["off", "focus", "idle"]),
        doc: "Save modified files automatically: when the terminal loses focus, or also after 2 s idle",
        get: |c| Value::from(c.auto_save.as_str()),
        set: |c, v| c.auto_save = v.as_str().unwrap_or("focus").to_string(),
    },
    Setting {
        path: "editor.persistent-undo",
        kind: Kind::Bool,
        doc: "Keep undo history across restarts (saved next to your state, dropped if the file changed elsewhere)",
        get: |c| Value::from(c.persistent_undo),
        set: |c, v| c.persistent_undo = b(v),
    },
    Setting {
        path: "editor.offer-grammars",
        kind: Kind::Bool,
        doc: "When a file's language has no syntax grammar yet, offer to download and build it (y/n)",
        get: |c| Value::from(c.offer_grammars),
        set: |c, v| c.offer_grammars = b(v),
    },
    Setting {
        path: "editor.render-doc-comments",
        kind: Kind::Bool,
        doc: "Show doc comments (///, //!, /** */) rendered as Markdown; raw when the cursor enters the block",
        get: |c| Value::from(c.render_doc_comments),
        set: |c, v| c.render_doc_comments = b(v),
    },
    Setting {
        path: "editor.restore-session",
        kind: Kind::Bool,
        doc: "Reopen the files you had open in this folder (with cursor positions) when started without files",
        get: |c| Value::from(c.restore_session),
        set: |c, v| c.restore_session = b(v),
    },
    Setting {
        path: "editor.inline-diagnostics",
        kind: Kind::Bool,
        doc: "Show diagnostic messages at the end of their line and tint error/warning lines",
        get: |c| Value::from(c.inline_diagnostics),
        set: |c, v| c.inline_diagnostics = b(v),
    },
    Setting {
        path: "editor.indent-guides",
        kind: Kind::Bool,
        doc: "Faint vertical guides in leading indentation (theme: ui.virtual.indent-guide)",
        get: |c| Value::from(c.indent_guides),
        set: |c, v| c.indent_guides = b(v),
    },
    Setting {
        path: "editor.scrollbar",
        kind: Kind::Bool,
        doc: "Scrollbar on the right edge, with error/warning marks for the whole file",
        get: |c| Value::from(c.scrollbar),
        set: |c, v| c.scrollbar = b(v),
    },
    Setting {
        path: "editor.lsp",
        kind: Kind::Bool,
        doc: "Start language servers (servers: [lsp.<name>] command/args, per language: [lang.<lang>] lsp = [...])",
        get: |c| Value::from(c.lsp.enabled),
        set: |c, v| c.lsp.enabled = b(v),
    },
    Setting {
        path: "editor.cursor.normal",
        kind: Kind::Enum(SHAPES),
        doc: "Cursor shape in normal mode",
        get: |c| shape_str(c.cursor_shape.0),
        set: |c, v| c.cursor_shape.0 = shape(v),
    },
    Setting {
        path: "editor.cursor.insert",
        kind: Kind::Enum(SHAPES),
        doc: "Cursor shape in insert mode",
        get: |c| shape_str(c.cursor_shape.1),
        set: |c, v| c.cursor_shape.1 = shape(v),
    },
    Setting {
        path: "editor.cursor.select",
        kind: Kind::Enum(SHAPES),
        doc: "Cursor shape in select mode",
        get: |c| shape_str(c.cursor_shape.2),
        set: |c, v| c.cursor_shape.2 = shape(v),
    },
    Setting {
        path: "llm.claude-code",
        kind: Kind::Bool,
        doc: "Let Claude Code (run `claude` → /ide, or space c) connect: it sees your selection and diagnostics, and its edits come here as a y/n diff (restart to apply)",
        get: |c| Value::from(c.agent_claude_code),
        set: |c, v| c.agent_claude_code = b(v),
    },
    Setting {
        path: "llm.command",
        kind: Kind::Str,
        doc: "LLM CLI speaking claude's stream-json protocol",
        get: |c| Value::from(c.llm.command.as_str()),
        set: |c, v| c.llm.command = s(v).to_string(),
    },
    Setting {
        path: "llm.args",
        kind: Kind::StrList,
        doc: "Arguments for llm.command (default: claude -p with speed flags)",
        get: |c| Value::Array(c.llm.args.iter().map(|a| Value::from(a.as_str())).collect()),
        set: |c, v| {
            c.llm.args =
                v.as_array().into_iter().flatten().filter_map(|a| a.as_str().map(str::to_string)).collect()
        },
    },
    Setting {
        path: "llm.model",
        kind: Kind::Str,
        doc: "Model for llm.command, e.g. \"haiku\" (\"\" = the CLI's default)",
        get: |c| Value::from(c.llm.model.as_deref().unwrap_or("")),
        set: |c, v| c.llm.model = Some(s(v).to_string()).filter(|m| !m.is_empty()),
    },
    Setting {
        path: "llm.context-lines",
        kind: Kind::Int { min: 0, max: 1000 },
        doc: "Lines of context sent around each selection",
        get: |c| Value::from(c.llm.context_lines as i64),
        set: |c, v| c.llm.context_lines = n(v),
    },
];

pub fn find(path: &str) -> Option<&'static Setting> {
    SETTINGS.iter().find(|s| s.path == path)
}

pub fn apply(cfg: &mut EditorConfig, path: &str, v: &Value) -> Result<(), String> {
    let setting = find(path).ok_or_else(|| format!("unknown setting '{path}'"))?;
    setting.kind.check(v).map_err(|e| format!("{path}: {e}"))?;
    (setting.set)(cfg, v);
    Ok(())
}

/// `:set` argument → value. As-is if it parses as TOML (`8`, `true`, `["a"]`), else a string (`relative`).
pub fn parse_value(raw: &str) -> Value {
    toml::from_str::<toml::Table>(&format!("v = {raw}"))
        .ok()
        .and_then(|mut t| t.remove("v"))
        .unwrap_or_else(|| Value::from(raw))
}

/// Apply a config file table (`keys` belongs to the keymap, skipped). Problems as `file:line: msg` warnings.
pub fn apply_table(
    cfg: &mut EditorConfig,
    table: &toml::Table,
    src: &str,
    file: &str,
    warnings: &mut Vec<String>,
) {
    walk(cfg, "", table, src, file, warnings);
}

fn walk(cfg: &mut EditorConfig, prefix: &str, t: &toml::Table, src: &str, file: &str, w: &mut Vec<String>) {
    for (k, v) in t {
        let path = if prefix.is_empty() { k.clone() } else { format!("{prefix}.{k}") };
        // Free-form-named tables (keymap, language servers, per-language settings, `[[attach]]`) are read by
        // config.rs.
        if prefix.is_empty() && matches!(path.as_str(), "keys" | "lsp" | "lang" | "attach") {
            continue;
        }
        let at = || match locate(src, &path) {
            Some(line) => format!("{file}:{line}: "),
            None => format!("{file}: "),
        };
        if find(&path).is_some() {
            if let Err(e) = apply(cfg, &path, v) {
                w.push(format!("{}{e}", at()));
            }
        } else if let Value::Table(sub) = v
            && SETTINGS.iter().any(|s| s.path.starts_with(&format!("{path}.")))
        {
            walk(cfg, &path, sub, src, file, w);
        } else {
            w.push(format!("{}unknown setting '{path}'", at()));
        }
    }
}

/// Line number (from 1) where a setting path is written. Lightweight tracking of only `[section]` headers and
/// `key =` lines — keys inside an inline table (`cursor = { … }`) resolve to that line.
pub fn locate(src: &str, path: &str) -> Option<usize> {
    let mut section = String::new();
    for (i, line) in src.lines().enumerate() {
        let line = line.trim();
        if let Some(h) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            section = h.trim_matches(['[', ']']).trim().to_string();
            if section == path {
                return Some(i + 1);
            }
            continue;
        }
        let Some((key, _)) = line.split_once('=') else { continue };
        let key = key.trim().trim_matches('"');
        let full = if section.is_empty() { key.to_string() } else { format!("{section}.{key}") };
        if full == path || path.starts_with(&format!("{full}.")) {
            return Some(i + 1);
        }
    }
    None
}

/// The whole effective config as TOML (with description comments) — `:config-show`.
pub fn show(cfg: &EditorConfig) -> String {
    let mut out = String::from(
        "# tarae — effective settings\n# :set <path> <value> changes it for this session, :set! also saves it\n",
    );
    let mut section = "";
    for s in SETTINGS {
        let (sec, key) = s.path.rsplit_once('.').unwrap_or(("", s.path));
        if sec != section && !sec.is_empty() {
            out.push_str(&format!("\n[{sec}]\n"));
            section = sec;
        }
        out.push_str(&format!("# {} ({})\n{key} = {}\n", s.doc, s.kind.describe(), (s.get)(cfg)));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_setting_roundtrips_its_default() {
        let mut cfg = EditorConfig::default();
        for s in SETTINGS {
            let v = (s.get)(&cfg);
            s.kind.check(&v).unwrap_or_else(|e| panic!("{}: default fails its own kind: {e}", s.path));
            apply(&mut cfg, s.path, &v).unwrap();
            assert_eq!((s.get)(&cfg), v, "{}", s.path);
        }
    }

    #[test]
    fn parse_value_accepts_bare_words() {
        assert_eq!(parse_value("8"), Value::Integer(8));
        assert_eq!(parse_value("true"), Value::Boolean(true));
        assert_eq!(parse_value("relative"), Value::from("relative"));
        assert_eq!(parse_value("\"a b\""), Value::from("a b"));
        assert_eq!(parse_value("[\"-p\"]"), Value::Array(vec![Value::from("-p")]));
    }

    #[test]
    fn warnings_carry_line_numbers() {
        let src = "[editor]\nscrolloff = 500\nnope = 1\ncursor = { insert = \"wiggle\" }\n";
        let table: toml::Table = toml::from_str(src).unwrap();
        let mut cfg = EditorConfig::default();
        let mut w = Vec::new();
        apply_table(&mut cfg, &table, src, "config.toml", &mut w);
        w.sort();
        assert_eq!(w.len(), 3, "{w:?}");
        assert!(w.iter().any(|m| m.starts_with("config.toml:2: editor.scrolloff: expected integer 0..=100")));
        assert!(w.iter().any(|m| m.starts_with("config.toml:3: unknown setting 'editor.nope'")));
        assert!(w.iter().any(|m| m.starts_with("config.toml:4: editor.cursor.insert")));
    }

    #[test]
    fn show_is_loadable_config() {
        let text = show(&EditorConfig::default());
        let table: toml::Table = toml::from_str(&text).unwrap();
        let mut w = Vec::new();
        apply_table(&mut EditorConfig::default(), &table, &text, "x", &mut w);
        assert!(w.is_empty(), "{w:?}");
    }
}
