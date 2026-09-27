//! Themes — TOML. `"scope" = "color"` or `{ fg, bg, bold, italic, underline, dim, reversed, crossed }`
//! (also `modifiers = [...]`·`underline = { color, style = "curl" }`), color = `#rrggbb`·`#rgb`·ANSI name·
//! `[palette]` name (palette entries may point at each other — `accent = "coral"`), `inherits = "meok"`
//! layers over another theme. Bad color names and unknown attributes are not silently dropped — reported
//! via `Theme::warnings`.
//! tarae-only UI keys: `ui.accent` (the single accent color), `ui.tint` (base for blending light colors —
//! transparent variants).
//! Four built-ins: `meok` (먹, dark) · `hanji` (한지, light) · `meok-transparent` · `hanji-transparent`
//! (don't paint the background, the terminal's shows through). `"default"` = meok/hanji to match the terminal
//! (`set_light` — term.rs asks at startup).
//! Otherwise only `~/.config/tarae/themes/*.toml` (Helix folders are not read — user decision 2026-09-27).
//! Scope names are looked up by trimming from the end (`keyword.control.import` → `keyword.control` →
//! `keyword`).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use crossterm::style::Color;
use toml::Value;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Style {
    pub fg: Option<Color>,
    pub bg: Option<Color>,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub dim: bool,
    pub reversed: bool,
    pub crossed: bool,
    /// Underline color (diagnostics), curly underline.
    pub ul_color: Option<Color>,
    pub curly: bool,
}

impl Style {
    #[cfg(test)]
    pub const fn fg(c: Color) -> Self {
        Self {
            fg: Some(c),
            bg: None,
            bold: false,
            italic: false,
            underline: false,
            dim: false,
            reversed: false,
            crossed: false,
            ul_color: None,
            curly: false,
        }
    }

    /// Overrides only what `other` sets (modifiers are merged).
    pub fn patch(self, other: Style) -> Style {
        Style {
            fg: other.fg.or(self.fg),
            bg: other.bg.or(self.bg),
            bold: self.bold || other.bold,
            italic: self.italic || other.italic,
            underline: self.underline || other.underline,
            dim: self.dim || other.dim,
            reversed: self.reversed || other.reversed,
            crossed: self.crossed || other.crossed,
            ul_color: other.ul_color.or(self.ul_color),
            curly: self.curly || other.curly,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Theme {
    pub name: String,
    styles: HashMap<String, Style>,
    /// Problems hit while reading (bad color names·unknown attributes) — shown as config warnings.
    pub warnings: Vec<String>,
}

impl Theme {
    /// Exactly that name, otherwise trimming dot by dot.
    pub fn try_get(&self, scope: &str) -> Option<Style> {
        let mut s = scope;
        loop {
            if let Some(st) = self.styles.get(s) {
                return Some(*st);
            }
            s = &s[..s.rfind('.')?];
        }
    }

    pub fn get(&self, scope: &str) -> Style {
        self.try_get(scope).unwrap_or_default()
    }

    /// Built-in default theme — meok/hanji to match the terminal (also fills UI keys other themes lack).
    pub fn builtin() -> Theme {
        let name = if is_light() { "hanji" } else { "meok" };
        let table = builtin_table(name).expect("builtin theme");
        from_table(name, &table)
    }
}

impl Default for Theme {
    fn default() -> Self {
        Theme::builtin()
    }
}

/// Built-in theme sources (name, TOML) — transparent variants are made from these (`builtin_table`).
const SOURCES: [(&str, &str); 2] =
    [("meok", include_str!("themes/meok.toml")), ("hanji", include_str!("themes/hanji.toml"))];

/// Built-in themes (name, human-readable name).
pub const BUILTIN: [(&str, &str); 4] = [
    ("meok", "먹"),
    ("hanji", "한지"),
    ("meok-transparent", "먹 · 투명"),
    ("hanji-transparent", "한지 · 투명"),
];

static LIGHT: AtomicBool = AtomicBool::new(false);

/// Whether the terminal background is light — decides whether `"default"` is meok or hanji.
pub fn set_light(light: bool) {
    LIGHT.store(light, Ordering::Relaxed);
}

pub fn is_light() -> bool {
    LIGHT.load(Ordering::Relaxed)
}

fn builtin_table(name: &str) -> Option<toml::Table> {
    let name = if name == "default" { if is_light() { "hanji" } else { "meok" } } else { name };
    let (base, transparent) = match name.strip_suffix("-transparent") {
        Some(b) => (b, true),
        None => (name, false),
    };
    let src = SOURCES.iter().find(|(n, _)| *n == base)?.1;
    let mut table: toml::Table = toml::from_str(src).expect("builtin theme parses");
    if transparent {
        // Don't paint the background (the terminal's shows through). The blend base is the original
        // background — `ui.tint` (tarae-only key)
        if let Some(bg) =
            table.get_mut("ui.background").and_then(Value::as_table_mut).and_then(|t| t.remove("bg"))
        {
            let mut tint = toml::Table::new();
            tint.insert("bg".into(), bg);
            table.insert("ui.tint".into(), Value::Table(tint));
        }
    }
    Some(table)
}

pub fn theme_dirs() -> Vec<PathBuf> {
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")));
    config.map(|c| c.join("tarae/themes")).into_iter().collect()
}

/// Selectable theme names: the four built-ins first, then `~/.config/tarae/themes` (sorted, deduped).
pub fn available() -> Vec<String> {
    let mut names: Vec<String> = BUILTIN.iter().map(|(n, _)| n.to_string()).collect();
    let mut found: Vec<String> = theme_dirs()
        .into_iter()
        .filter_map(|d| std::fs::read_dir(d).ok())
        .flatten()
        .filter_map(|e| {
            let p = e.ok()?.path();
            (p.extension()? == "toml").then(|| p.file_stem()?.to_str().map(str::to_string)).flatten()
        })
        .filter(|n| !names.contains(n))
        .collect();
    found.sort();
    found.dedup();
    names.extend(found);
    names
}

/// Loads a theme by name (reads files — call from a worker thread).
pub fn load(name: &str) -> Result<Theme, String> {
    if name == "default" {
        return Ok(Theme::builtin());
    }
    let table = load_table(&theme_dirs(), name, 0)?;
    Ok(from_table(name, &table))
}

/// Table with child laid over parent, following `inherits` upward (palette merged key by key).
fn load_table(dirs: &[PathBuf], name: &str, depth: usize) -> Result<toml::Table, String> {
    if depth > 8 {
        return Err(format!("theme '{name}': inherits too deep"));
    }
    // A same-named theme in the user folder wins (customizing a built-in)
    let user = dirs.iter().map(|d| d.join(format!("{name}.toml"))).find(|p| p.is_file());
    if user.is_none()
        && let Some(t) = builtin_table(name)
    {
        return Ok(t);
    }
    let path = user.ok_or_else(|| format!("theme '{name}' not found"))?;
    let src = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut table: toml::Table =
        toml::from_str(&src).map_err(|e| format!("{}: {}", path.display(), e.message()))?;
    let Some(parent) = table.remove("inherits").and_then(|v| v.as_str().map(str::to_string)) else {
        return Ok(table);
    };
    // `meok.toml` with `inherits = "meok"` = customizing the built-in one (not itself again)
    let mut base = match builtin_table(&parent).filter(|_| parent == name) {
        Some(t) => t,
        None => load_table(dirs, &parent, depth + 1)?,
    };
    for (k, v) in table {
        match (k.as_str(), v, base.get_mut("palette")) {
            ("palette", Value::Table(p), Some(Value::Table(bp))) => bp.extend(p),
            (_, v, _) => {
                base.insert(k, v);
            }
        }
    }
    Ok(base)
}

fn from_table(name: &str, table: &toml::Table) -> Theme {
    let mut warnings = Vec::new();
    let palette = parse_palette(table.get("palette").and_then(Value::as_table), &mut warnings);
    let mut styles = HashMap::new();
    for (k, v) in table.iter().filter(|(k, _)| *k != "palette") {
        let mut warn = |what: String| warnings.push(format!("theme '{name}': \"{k}\": {what}"));
        if let Some(st) = parse_style(v, &palette, &mut warn) {
            styles.insert(k.clone(), st);
        }
    }
    Theme { name: name.to_string(), styles, warnings }
}

/// Palette — values may be other palette names (`accent = "coral"`, any depth, warns on cycles).
fn parse_palette(table: Option<&toml::Table>, warnings: &mut Vec<String>) -> HashMap<String, Color> {
    let Some(table) = table else { return HashMap::new() };
    let mut out = HashMap::new();
    for (k, v) in table {
        let mut at = v.as_str();
        let mut seen = vec![k.as_str()];
        let color = loop {
            let Some(s) = at else { break None };
            if let Some(c) = parse_color(s, &HashMap::new()) {
                break Some(c);
            }
            if seen.contains(&s) || !table.contains_key(s) {
                break None;
            }
            seen.push(s);
            at = table.get(s).and_then(Value::as_str);
        };
        match color {
            Some(c) => {
                out.insert(k.clone(), c);
            }
            None => warnings.push(format!("theme palette: {k} = {v} is not a color")),
        }
    }
    out
}

fn parse_style(v: &Value, palette: &HashMap<String, Color>, warn: &mut impl FnMut(String)) -> Option<Style> {
    let color = |s: &str, warn: &mut dyn FnMut(String)| {
        let c = parse_color(s, palette);
        if c.is_none() {
            warn(format!("unknown color '{s}'"));
        }
        c
    };
    match v {
        Value::String(s) => Some(Style { fg: color(s, warn), ..Style::default() }),
        Value::Table(t) => {
            let mut st = Style::default();
            for (k, v) in t {
                match (k.as_str(), v) {
                    ("fg", Value::String(s)) => st.fg = color(s, warn),
                    ("bg", Value::String(s)) => st.bg = color(s, warn),
                    ("modifiers", Value::Array(ms)) => {
                        for m in ms {
                            match m.as_str() {
                                Some(m) if set_modifier(&mut st, m, true) => {}
                                _ => warn(format!("unknown modifier {m}")),
                            }
                        }
                    }
                    ("underline", Value::Table(u)) => {
                        st.underline = true;
                        for (uk, uv) in u {
                            match (uk.as_str(), uv.as_str()) {
                                ("color", Some(s)) => st.ul_color = color(s, warn),
                                ("style", Some("curl")) => st.curly = true,
                                ("style", Some("line")) => {}
                                _ => warn(format!(
                                    "underline.{uk} = {uv} (color = \"…\", style = \"line\" | \"curl\")"
                                )),
                            }
                        }
                    }
                    (m, Value::Boolean(on)) if set_modifier(&mut st, m, *on) => {}
                    _ => warn(format!(
                        "{k} = {v} — expected fg, bg, bold, italic, underline, dim, reversed, crossed"
                    )),
                }
            }
            Some(st)
        }
        _ => {
            warn(format!("{v} — expected \"color\" or {{ fg = …, … }}"));
            None
        }
    }
}

/// Toggles one modifier — true if the name is known. (`underlined`·`crossed_out` are old names)
fn set_modifier(st: &mut Style, name: &str, on: bool) -> bool {
    let slot = match name {
        "bold" => &mut st.bold,
        "italic" => &mut st.italic,
        "underline" | "underlined" => &mut st.underline,
        "dim" => &mut st.dim,
        "reversed" => &mut st.reversed,
        "crossed" | "crossed_out" | "strikethrough" => &mut st.crossed,
        _ => return false,
    };
    *slot = on;
    true
}

fn parse_color(s: &str, palette: &HashMap<String, Color>) -> Option<Color> {
    if let Some(c) = palette.get(s) {
        return Some(*c);
    }
    if let Some(hex) = s.strip_prefix('#') {
        let v = u32::from_str_radix(hex, 16).ok()?;
        return match hex.len() {
            6 => Some(Color::Rgb { r: (v >> 16) as u8, g: (v >> 8) as u8, b: v as u8 }),
            3 => Some(Color::Rgb {
                r: ((v >> 8) & 0xf) as u8 * 17,
                g: ((v >> 4) & 0xf) as u8 * 17,
                b: (v & 0xf) as u8 * 17,
            }),
            _ => None,
        };
    }
    Some(match s {
        "black" => Color::Black,
        "red" => Color::DarkRed,
        "green" => Color::DarkGreen,
        "yellow" => Color::DarkYellow,
        "blue" => Color::DarkBlue,
        "magenta" => Color::DarkMagenta,
        "cyan" => Color::DarkCyan,
        "gray" => Color::DarkGrey,
        "light-red" => Color::Red,
        "light-green" => Color::Green,
        "light-yellow" => Color::Yellow,
        "light-blue" => Color::Blue,
        "light-magenta" => Color::Magenta,
        "light-cyan" => Color::Cyan,
        "light-gray" => Color::Grey,
        "white" => Color::White,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_fallback_and_patch() {
        let t: toml::Table = toml::from_str(
            r##"
            "keyword" = "red"
            "keyword.control" = { fg = "#00ff00", modifiers = ["bold"] }
            "ui.selection" = { bg = "sel" }
            [palette]
            sel = "#112233"
            "##,
        )
        .unwrap();
        let th = from_table("t", &t);
        assert_eq!(th.get("keyword.control.import").fg, Some(Color::Rgb { r: 0, g: 255, b: 0 }));
        assert!(th.get("keyword.control.import").bold);
        assert_eq!(th.get("keyword.function").fg, Some(Color::DarkRed));
        assert_eq!(th.get("ui.selection").bg, Some(Color::Rgb { r: 0x11, g: 0x22, b: 0x33 }));
        assert_eq!(th.try_get("nothing"), None);
        let s = Style::fg(Color::Red).patch(Style { bg: Some(Color::Blue), bold: true, ..Style::default() });
        assert_eq!((s.fg, s.bg, s.bold), (Some(Color::Red), Some(Color::Blue), true));
    }

    #[test]
    fn builtin_theme_has_ui_and_syntax() {
        let t = Theme::builtin();
        assert!(t.try_get("ui.selection").is_some());
        assert!(t.try_get("keyword").is_some());
        assert!(t.try_get("comment").is_some());
    }

    /// Four built-ins — transparent ones don't paint the background, keeping it as blend base (`ui.tint`).
    #[test]
    fn builtin_four_and_transparent_variants() {
        let names: Vec<&str> = BUILTIN.iter().map(|(n, _)| *n).collect();
        assert_eq!(names, ["meok", "hanji", "meok-transparent", "hanji-transparent"]);
        let solid = load("meok").unwrap();
        let clear = load("meok-transparent").unwrap();
        let bg = solid.try_get("ui.background").and_then(|s| s.bg);
        assert!(bg.is_some());
        assert_eq!(clear.try_get("ui.background").and_then(|s| s.bg), None, "does not paint the background");
        assert_eq!(clear.try_get("ui.tint").and_then(|s| s.bg), bg, "blend base is the original background");
        assert_eq!(clear.try_get("keyword"), solid.try_get("keyword"), "the rest is the same");
        assert!(load("dracula").is_err(), "Helix themes are not read");
    }

    #[test]
    fn builtin_themes_parse_without_warnings() {
        for (n, _) in BUILTIN {
            let t = load(n).unwrap();
            assert!(t.warnings.is_empty(), "{n}: {:?}", t.warnings);
        }
    }

    /// A user `meok.toml` inheriting `meok` layers over the built-in (no self-recursion).
    #[test]
    fn user_theme_can_inherit_the_builtin_of_the_same_name() {
        let dir = std::env::temp_dir().join(format!("tarae-theme-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("meok.toml"), "inherits = \"meok\"\n\"keyword\" = \"#010203\"\n").unwrap();
        let table = load_table(std::slice::from_ref(&dir), "meok", 0).unwrap();
        let th = from_table("meok", &table);
        assert_eq!(th.get("keyword").fg, Some(Color::Rgb { r: 1, g: 2, b: 3 }));
        assert!(th.try_get("ui.selection").is_some(), "the rest comes from the built-in");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Palette aliases · flat modifiers · bad names warn.
    #[test]
    fn palette_aliases_flat_modifiers_and_warnings() {
        let t: toml::Table = toml::from_str(
            r##"
            "keyword" = { fg = "accent", bold = true, italic = true }
            "comment" = { fg = "grey", modifiers = ["italic", "blink"] }
            "string" = "corall"
            "markup" = { fg = "coral", colour = "red", underline = { color = "accent", style = "curl" } }
            [palette]
            coral = "#ef8a5a"
            accent = "coral"
            loop_a = "loop_b"
            loop_b = "loop_a"
            "##,
        )
        .unwrap();
        let th = from_table("t", &t);
        let coral = Some(Color::Rgb { r: 0xef, g: 0x8a, b: 0x5a });
        let kw = th.get("keyword");
        assert_eq!((kw.fg, kw.bold, kw.italic), (coral, true, true));
        let mk = th.get("markup");
        assert_eq!((mk.ul_color, mk.curly, mk.underline), (coral, true, true));
        assert!(th.get("comment").italic);
        let w = th.warnings.join("\n");
        for want in [
            "unknown color 'grey'",
            "unknown modifier \"blink\"",
            "unknown color 'corall'",
            "colour",
            "loop_a",
        ] {
            assert!(w.contains(want), "{want} missing:\n{w}");
        }
        assert_eq!(th.warnings.len(), 6, "{w}");
    }
}
