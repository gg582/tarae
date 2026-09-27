//! Key representation — 1:1 with helix config notation ("C-y", "A-x", "space", "ret").

use std::fmt;
use std::str::FromStr;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Code {
    Char(char),
    Enter,
    Esc,
    Tab,
    BackTab,
    Backspace,
    Delete,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    PageUp,
    PageDown,
    F(u8),
}

/// Shift isn't carried separately — char keys become uppercase, Tab becomes BackTab (same as helix).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Key {
    pub code: Code,
    pub ctrl: bool,
    pub alt: bool,
}

impl Key {
    pub const fn plain(code: Code) -> Self {
        Key { code, ctrl: false, alt: false }
    }

    pub fn from_event(ev: KeyEvent) -> Option<Key> {
        let code = match ev.code {
            KeyCode::Char(c) => Code::Char(c),
            KeyCode::Enter => Code::Enter,
            KeyCode::Esc => Code::Esc,
            KeyCode::Tab => Code::Tab,
            KeyCode::BackTab => Code::BackTab,
            KeyCode::Backspace => Code::Backspace,
            KeyCode::Delete => Code::Delete,
            KeyCode::Left => Code::Left,
            KeyCode::Right => Code::Right,
            KeyCode::Up => Code::Up,
            KeyCode::Down => Code::Down,
            KeyCode::Home => Code::Home,
            KeyCode::End => Code::End,
            KeyCode::PageUp => Code::PageUp,
            KeyCode::PageDown => Code::PageDown,
            KeyCode::F(n) => Code::F(n),
            _ => return None,
        };
        Some(Key {
            code,
            ctrl: ev.modifiers.contains(KeyModifiers::CONTROL),
            alt: ev.modifiers.contains(KeyModifiers::ALT),
        })
    }

    /// The char, if this is a char key without modifiers.
    pub fn plain_char(&self) -> Option<char> {
        match self.code {
            Code::Char(c) if !self.ctrl && !self.alt => Some(c),
            _ => None,
        }
    }
}

impl FromStr for Key {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        let (mut ctrl, mut alt, mut shift) = (false, false, false);
        let mut rest = s;
        // "C-", "A-", "S-" prefixes. Also works when the key itself is '-', as in "C--".
        while rest.len() > 2 && rest.as_bytes()[1] == b'-' {
            match rest.as_bytes()[0] {
                b'C' => ctrl = true,
                b'A' => alt = true,
                b'S' => shift = true,
                _ => break,
            }
            rest = &rest[2..];
        }
        let code = match rest {
            "space" => Code::Char(' '),
            "ret" | "enter" => Code::Enter,
            "esc" => Code::Esc,
            "tab" => Code::Tab,
            "backtab" => Code::BackTab,
            "backspace" => Code::Backspace,
            "del" => Code::Delete,
            "left" => Code::Left,
            "right" => Code::Right,
            "up" => Code::Up,
            "down" => Code::Down,
            "home" => Code::Home,
            "end" => Code::End,
            "pageup" => Code::PageUp,
            "pagedown" => Code::PageDown,
            "minus" => Code::Char('-'),
            "lt" => Code::Char('<'),
            "gt" => Code::Char('>'),
            f if f.len() > 1 && f.starts_with('F') && f[1..].parse::<u8>().is_ok() => {
                Code::F(f[1..].parse().unwrap())
            }
            _ => {
                let mut it = rest.chars();
                match (it.next(), it.next()) {
                    (Some(c), None) => Code::Char(c),
                    _ => return Err(format!("unknown key: {s:?}")),
                }
            }
        };
        let code = match (code, shift) {
            (Code::Char(c), true) => Code::Char(c.to_ascii_uppercase()),
            (Code::Tab, true) => Code::BackTab,
            (c, _) => c,
        };
        Ok(Key { code, ctrl, alt })
    }
}

impl fmt::Display for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.ctrl {
            f.write_str("C-")?;
        }
        if self.alt {
            f.write_str("A-")?;
        }
        match self.code {
            Code::Char(' ') => f.write_str("space"),
            Code::Char('-') => f.write_str("minus"),
            Code::Char(c) => write!(f, "{c}"),
            Code::Enter => f.write_str("ret"),
            Code::Esc => f.write_str("esc"),
            Code::Tab => f.write_str("tab"),
            Code::BackTab => f.write_str("backtab"),
            Code::Backspace => f.write_str("backspace"),
            Code::Delete => f.write_str("del"),
            Code::Left => f.write_str("left"),
            Code::Right => f.write_str("right"),
            Code::Up => f.write_str("up"),
            Code::Down => f.write_str("down"),
            Code::Home => f.write_str("home"),
            Code::End => f.write_str("end"),
            Code::PageUp => f.write_str("pageup"),
            Code::PageDown => f.write_str("pagedown"),
            Code::F(n) => write!(f, "F{n}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(s: &str) -> Key {
        s.parse().unwrap()
    }

    #[test]
    fn parses_helix_notation() {
        assert_eq!(k("x"), Key::plain(Code::Char('x')));
        assert_eq!(k("A-x"), Key { code: Code::Char('x'), ctrl: false, alt: true });
        assert_eq!(k("C-y"), Key { code: Code::Char('y'), ctrl: true, alt: false });
        assert_eq!(k("A-,"), Key { code: Code::Char(','), ctrl: false, alt: true });
        assert_eq!(k("C--"), Key { code: Code::Char('-'), ctrl: true, alt: false });
        assert_eq!(k("-"), Key::plain(Code::Char('-')));
        assert_eq!(k("space"), Key::plain(Code::Char(' ')));
        assert_eq!(k("S-x"), Key::plain(Code::Char('X')));
        assert_eq!(k("S-tab"), Key::plain(Code::BackTab));
        assert_eq!(k("F12"), Key::plain(Code::F(12)));
        assert_eq!(k("ret"), Key::plain(Code::Enter));
        assert!("nope".parse::<Key>().is_err());
    }

    #[test]
    fn display_roundtrips() {
        for s in ["x", "A-x", "C-y", "space", "minus", "ret", "C-A-backspace", "F3"] {
            assert_eq!(k(s).to_string(), s);
        }
    }
}
