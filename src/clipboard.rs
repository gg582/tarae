//! System clipboard = the `+` register. External commands (pbcopy/pbpaste · on WSL the Windows clipboard
//! (win32yank, PowerShell) · wl-copy/wl-paste · xclip/xsel), so
//! per principle 4 they run on a worker thread. Tests use memory so the user's clipboard stays untouched.

use crate::editor::Editor;
use crate::event::Jobs;

pub fn copy(jobs: &Jobs, text: String) {
    jobs.spawn(move || {
        let r = system::write(&text);
        move |ed: &mut Editor| {
            if let Err(e) = r {
                ed.set_error(format!("clipboard: {e}"));
            }
        }
    });
}

/// Read the clipboard and pass it to `then` (on the main loop, after the read finishes).
pub fn paste(jobs: &Jobs, then: impl FnOnce(&mut Editor, String) + Send + 'static) {
    jobs.spawn(move || {
        let r = system::read();
        move |ed: &mut Editor| match r {
            Ok(s) if s.is_empty() => ed.set_status("clipboard is empty"),
            Ok(s) => then(ed, s),
            Err(e) => ed.set_error(format!("clipboard: {e}")),
        }
    });
}

/// Text from Windows is CRLF — LF inside the editor (PowerShell also appends a trailing newline).
fn from_powershell(s: &str) -> String {
    let s = s.replace("\r\n", "\n");
    match s.strip_suffix('\n') {
        Some(t) => t.to_string(),
        None => s,
    }
}

#[cfg(not(test))]
mod system {
    use std::io::Write;
    use std::process::{Command, Stdio};

    fn on_path(cmd: &str) -> bool {
        std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(cmd).is_file()))
    }

    /// WSL: the Linux side usually has no clipboard tool — use the Windows clipboard.
    fn wsl() -> bool {
        std::env::var_os("WSL_DISTRO_NAME").is_some()
            || std::fs::read_to_string("/proc/sys/kernel/osrelease")
                .is_ok_and(|s| s.to_lowercase().contains("microsoft"))
    }

    // PowerShell needs UTF-8 set explicitly or Hangul breaks (clip.exe uses the console code page — breaks)
    const PS_SET: &[&str] = &[
        "-NoProfile",
        "-NonInteractive",
        "-Command",
        "[Console]::InputEncoding=[Text.Encoding]::UTF8; Set-Clipboard -Value ([Console]::In.ReadToEnd())",
    ];
    const PS_GET: &[&str] = &[
        "-NoProfile",
        "-NonInteractive",
        "-Command",
        "[Console]::OutputEncoding=[Text.Encoding]::UTF8; Get-Clipboard -Raw",
    ];

    /// (command, args) — macOS: pb* · WSL: win32yank → (WSLg) wl-* → PowerShell · Linux: wl-* → xclip → xsel.
    fn pick(copy: bool) -> (&'static str, &'static [&'static str]) {
        let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some()
            && on_path(if copy { "wl-copy" } else { "wl-paste" });
        if cfg!(target_os = "macos") {
            return if copy { ("pbcopy", &[]) } else { ("pbpaste", &[]) };
        }
        if wsl() {
            if on_path("win32yank.exe") {
                return if copy {
                    ("win32yank.exe", &["-i", "--crlf"])
                } else {
                    ("win32yank.exe", &["-o", "--lf"])
                };
            }
            if wayland {
                return if copy { ("wl-copy", &[]) } else { ("wl-paste", &["-n"]) };
            }
            return if copy { ("powershell.exe", PS_SET) } else { ("powershell.exe", PS_GET) };
        }
        if wayland {
            return if copy { ("wl-copy", &[]) } else { ("wl-paste", &["-n"]) };
        }
        if on_path("xclip") || !on_path("xsel") {
            return if copy {
                ("xclip", &["-selection", "clipboard"])
            } else {
                ("xclip", &["-selection", "clipboard", "-o"])
            };
        }
        if copy { ("xsel", &["--clipboard", "--input"]) } else { ("xsel", &["--clipboard", "--output"]) }
    }

    pub fn write(text: &str) -> Result<(), String> {
        let (cmd, args) = pick(true);
        let mut child = Command::new(cmd)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("{cmd}: {e}"))?;
        child.stdin.take().ok_or("no stdin")?.write_all(text.as_bytes()).map_err(|e| e.to_string())?;
        child.wait().map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn read() -> Result<String, String> {
        let (cmd, args) = pick(false);
        let out = Command::new(cmd).args(args).output().map_err(|e| format!("{cmd}: {e}"))?;
        let s = String::from_utf8_lossy(&out.stdout).into_owned();
        Ok(if cmd == "powershell.exe" { super::from_powershell(&s) } else { s })
    }
}

#[cfg(test)]
mod system {
    use std::sync::Mutex;

    static CLIP: Mutex<String> = Mutex::new(String::new());

    pub fn write(text: &str) -> Result<(), String> {
        *CLIP.lock().unwrap() = text.to_string();
        Ok(())
    }

    pub fn read() -> Result<String, String> {
        Ok(CLIP.lock().unwrap().clone())
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn powershell_text_becomes_lf() {
        assert_eq!(super::from_powershell("a\r\nb\r\n"), "a\nb");
        assert_eq!(super::from_powershell("a\r\nb"), "a\nb", "no trailing newline — still LF");
    }
}
