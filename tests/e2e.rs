//! End-to-end smoke tests: the real `tarae` binary inside a pseudo-terminal.
//!
//! Each test gets its own sandbox (HOME, XDG_*, CLAUDE_CONFIG_DIR all point into a fresh temp dir, the
//! environment is cleared otherwise), starts tarae on a pty, types keys, and reads the screen back through a
//! `vt100` parser. Nothing here needs the network, claude, a language server or a debugger.
//!
//! Rules that keep these deterministic:
//! - Never sleep and hope — every key that matters is followed by `wait_for` on its visible effect.
//! - Esc goes out alone and is followed by a wait: crossterm reads `ESC x` arriving in one read as Alt-x.
//! - Keys are sent only after the first frame (raw mode is on by then — no line-discipline echo).
#![cfg(unix)]

use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_tarae");
/// Generous — a loaded CI runner can be slow; a passing test never waits this long.
const TIMEOUT: Duration = Duration::from_secs(10);
const POLL: Duration = Duration::from_millis(20);
const COLS: u16 = 100;
const ROWS: u16 = 30;

/// Baseline config for every sandbox: nothing that reaches outside the sandbox or pops up on its own.
/// (PATH is minimal too, so even with `lsp` on no language server would be found.)
const BASE_CONFIG: &str = "\
# Written by tests/e2e.rs
[editor]
lsp = false
offer-grammars = false
";

// ── Sandbox ──────────────────────────────────────────────────────────────────

/// A fresh directory standing in for HOME, the XDG dirs, and the working directory. Removed on drop.
struct Sandbox {
    root: PathBuf,
}

impl Sandbox {
    fn new(name: &str) -> Self {
        static N: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "tarae-e2e-{}-{}-{name}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        // Real path (macOS: /var → /private/var) — tarae canonicalizes the paths it opens.
        let root = fs::canonicalize(&dir).unwrap();
        let sb = Sandbox { root };
        for d in
            ["home/.config/tarae", "home/.local/state", "home/.local/share", "home/.cache", "claude", "work"]
        {
            fs::create_dir_all(sb.root.join(d)).unwrap();
        }
        sb.write_config(BASE_CONFIG);
        // Project config is searched upward from the working directory — stop the search here so a stray
        // /tmp/.tarae.toml can't leak in.
        sb.write(".tarae.toml", "# tests/e2e.rs\n");
        sb
    }

    fn home(&self) -> PathBuf {
        self.root.join("home")
    }

    /// The working directory tarae runs in — test files go here.
    fn work(&self) -> PathBuf {
        self.root.join("work")
    }

    fn file(&self, name: &str) -> PathBuf {
        self.work().join(name)
    }

    fn write(&self, name: &str, content: &str) -> PathBuf {
        let p = self.file(name);
        fs::write(&p, content).unwrap();
        p
    }

    fn read(&self, name: &str) -> String {
        fs::read_to_string(self.file(name)).unwrap_or_else(|e| panic!("reading {name}: {e}"))
    }

    fn write_config(&self, content: &str) {
        fs::write(self.home().join(".config/tarae/config.toml"), content).unwrap();
    }

    /// Claude Code IDE lock files tarae wrote (`$CLAUDE_CONFIG_DIR/ide/*.lock`).
    fn lock_files(&self) -> Vec<PathBuf> {
        let Ok(rd) = fs::read_dir(self.root.join("claude/ide")) else { return Vec::new() };
        rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "lock")).collect()
    }

    /// The whole environment the child sees — everything else is cleared (CLAUDE_*, TMUX, ZELLIJ*,
    /// TARAE_*, COLORFGBG, TERM_PROGRAM … can't leak in from the developer's shell or CI).
    fn env(&self) -> Vec<(&'static str, OsString)> {
        let home = self.home();
        vec![
            ("HOME", home.clone().into()),
            ("XDG_CONFIG_HOME", home.join(".config").into()),
            ("XDG_STATE_HOME", home.join(".local/state").into()),
            ("XDG_DATA_HOME", home.join(".local/share").into()),
            ("XDG_CACHE_HOME", home.join(".cache").into()),
            // tarae writes a Claude Code lock file on startup — never into the real ~/.claude
            ("CLAUDE_CONFIG_DIR", self.root.join("claude").into()),
            // Minimal PATH: no language servers, debuggers or claude from ~/.cargo/bin etc.
            ("PATH", "/usr/bin:/bin".into()),
            ("SHELL", "/bin/sh".into()),
            ("TERM", "xterm-256color".into()),
            // The harness doesn't answer the OSC 11 background query — skip it (and its 200 ms timeout)
            ("TARAE_BACKGROUND", "dark".into()),
        ]
    }

    fn spawn(&self, args: &[&str]) -> Term {
        Term::spawn(self, args, COLS, ROWS)
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

// ── Pseudo-terminal harness ──────────────────────────────────────────────────

/// What the reader thread fills in: the parsed screen and every raw byte tarae wrote.
struct Output {
    parser: vt100::Parser,
    raw: Vec<u8>,
    /// Panics of the vt100 parser itself (it overflows on a 1-column screen). The parser is replaced with a
    /// blank one — tarae redraws whole frames, so the next frame fills it again.
    parser_panics: Vec<String>,
}

type Shared = Arc<Mutex<Output>>;

/// A panicking test thread must not wedge the others — ignore poisoning.
fn lock(out: &Shared) -> std::sync::MutexGuard<'_, Output> {
    out.lock().unwrap_or_else(|e| e.into_inner())
}

/// A screen snapshot — one string per row (wide characters appear once).
struct Screen {
    rows: Vec<String>,
}

impl Screen {
    fn contains(&self, s: &str) -> bool {
        self.rows.iter().any(|r| r.contains(s))
    }

    /// The status line (one above the command line at the bottom).
    fn status(&self) -> &str {
        &self.rows[self.rows.len().saturating_sub(2)]
    }
}

impl std::fmt::Display for Screen {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let width = self.rows.iter().map(|r| r.chars().count()).max().unwrap_or(0);
        writeln!(f, "    ┌{}┐", "─".repeat(width))?;
        for (i, r) in self.rows.iter().enumerate() {
            let pad = " ".repeat(width - r.chars().count());
            writeln!(f, "{i:>3} │{r}{pad}│")?;
        }
        write!(f, "    └{}┘", "─".repeat(width))
    }
}

/// tarae running on a pty. The child is killed on drop.
struct Term {
    child: Child,
    master: File,
    /// The parent keeps a slave fd open for the child's whole life: when tarae exits, its last bytes (the
    /// terminal restore sequence) stay readable on the master instead of racing the slave's close.
    _slave: OwnedFd,
    out: Shared,
}

fn cvt(rc: libc::c_int) -> io::Result<libc::c_int> {
    if rc == -1 { Err(io::Error::last_os_error()) } else { Ok(rc) }
}

impl Term {
    fn spawn(sb: &Sandbox, args: &[&str], cols: u16, rows: u16) -> Term {
        let (mut master, mut slave) = (-1, -1);
        let mut ws = libc::winsize { ws_row: rows, ws_col: cols, ws_xpixel: 0, ws_ypixel: 0 };
        // SAFETY: out-pointers to live locals; no name buffer, default termios.
        cvt(unsafe {
            libc::openpty(&mut master, &mut slave, std::ptr::null_mut(), std::ptr::null_mut(), &mut ws)
        })
        .expect("openpty");
        // SAFETY: openpty just returned these fds and nothing else owns them.
        let (master, slave) = unsafe { (File::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) };
        // Tests run in parallel threads: don't let another test's child inherit this pty.
        for fd in [master.as_raw_fd(), slave.as_raw_fd()] {
            // SAFETY: fcntl on an fd we own.
            cvt(unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) }).unwrap();
        }
        let stdio = || Stdio::from(slave.try_clone().expect("dup slave"));
        let mut cmd = Command::new(BIN);
        cmd.args(args)
            .current_dir(sb.work())
            .env_clear()
            .envs(sb.env())
            .stdin(stdio())
            .stdout(stdio())
            .stderr(stdio());
        // SAFETY: only async-signal-safe calls between fork and exec.
        unsafe {
            cmd.pre_exec(|| {
                // New session + the pty as its controlling terminal: /dev/tty works, SIGWINCH arrives.
                cvt(libc::setsid())?;
                cvt(libc::ioctl(0, libc::TIOCSCTTY as _, 0))?;
                Ok(())
            });
        }
        let child = cmd.spawn().expect("spawn tarae");

        let out = Arc::new(Mutex::new(Output {
            parser: vt100::Parser::new(rows, cols, 0),
            raw: Vec::new(),
            parser_panics: Vec::new(),
        }));
        let mut reader = master.try_clone().expect("dup master");
        let sink = Arc::clone(&out);
        // Drains the master continuously (tarae never blocks on a full pty buffer, tests never block on
        // reads) and must never die early: on macOS an exiting process whose tty output nobody reads hangs
        // in the kernel, and so would our `wait`. Ends when the slave side is fully closed (EOF, or EIO on
        // Linux) — i.e. after the drop.
        thread::spawn(move || {
            let mut buf = [0u8; 16 * 1024];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        let mut o = lock(&sink);
                        o.raw.extend_from_slice(&buf[..n]);
                        let (rows, cols) = o.parser.screen().size();
                        let parser = &mut o.parser;
                        let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            parser.process(&buf[..n]);
                        }));
                        if let Err(e) = res {
                            let msg = e
                                .downcast_ref::<String>()
                                .cloned()
                                .or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string()));
                            o.parser_panics.push(msg.unwrap_or_else(|| "vt100 panicked".into()));
                            o.parser = vt100::Parser::new(rows, cols, 0);
                        }
                    }
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                    Err(_) => break,
                }
            }
        });

        let mut term = Term { child, master, _slave: slave, out };
        // First frame drawn = raw mode is on — safe to type from here.
        term.wait_for("the first frame (status line with the mode)", |s| s.status().contains("NORMAL"));
        term
    }

    /// Types keys (UTF-8). Esc = "\x1b" (send it alone), Enter = "\r".
    fn send(&mut self, keys: &str) {
        self.master.write_all(keys.as_bytes()).expect("write to pty");
        self.master.flush().unwrap();
    }

    /// Esc on its own, then wait for its visible effect.
    fn esc(&mut self, what: &str, done: impl Fn(&Screen) -> bool) -> Screen {
        self.send("\x1b");
        self.wait_for(what, done)
    }

    fn screen(&self) -> Screen {
        let o = lock(&self.out);
        let s = o.parser.screen();
        let (_, cols) = s.size();
        Screen { rows: s.rows(0, cols).collect() }
    }

    fn raw(&self) -> Vec<u8> {
        lock(&self.out).raw.clone()
    }

    /// Polls the screen until `done` holds. On timeout, panics with the whole screen.
    fn wait_for(&mut self, what: &str, done: impl Fn(&Screen) -> bool) -> Screen {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            let s = self.screen();
            if done(&s) {
                return s;
            }
            if Instant::now() > deadline {
                let state = match self.child.try_wait() {
                    Ok(Some(st)) => format!("tarae EXITED ({st})"),
                    _ => "tarae still running".to_string(),
                };
                let vt = &lock(&self.out).parser_panics;
                let vt = if vt.is_empty() { String::new() } else { format!(" — vt100 panicked: {vt:?}") };
                panic!("timed out after {TIMEOUT:?} waiting for {what} — {state}{vt}\n{s}");
            }
            thread::sleep(POLL);
        }
    }

    fn wait_text(&mut self, text: &str) -> Screen {
        self.wait_for(&format!("{text:?} on screen"), |s| s.contains(text))
    }

    /// Resizes the pty (the kernel sends tarae SIGWINCH) and the parser with it.
    fn resize(&mut self, cols: u16, rows: u16) {
        // Lock first: whatever tarae draws after the SIGWINCH is parsed at the new size.
        let mut o = lock(&self.out);
        // (vt100 can't hold a 0-sized screen — keep the old one; tarae still sees the real size)
        if rows > 0 && cols > 0 {
            o.parser.screen_mut().set_size(rows, cols);
        }
        let ws = libc::winsize { ws_row: rows, ws_col: cols, ws_xpixel: 0, ws_ypixel: 0 };
        // SAFETY: TIOCSWINSZ reads one winsize from a live local.
        cvt(unsafe { libc::ioctl(self.master.as_raw_fd(), libc::TIOCSWINSZ as _, &ws) }).expect("TIOCSWINSZ");
    }

    fn running(&mut self) -> bool {
        self.child.try_wait().expect("try_wait").is_none()
    }

    fn wait_exit(&mut self) -> ExitStatus {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            if let Some(st) = self.child.try_wait().expect("try_wait") {
                return st;
            }
            if Instant::now() > deadline {
                panic!("tarae didn't exit within {TIMEOUT:?}\n{}", self.screen());
            }
            thread::sleep(POLL);
        }
    }

    /// Waits until the raw output (everything tarae ever wrote) satisfies `done`.
    fn wait_raw(&mut self, what: &str, done: impl Fn(&[u8]) -> bool) -> Vec<u8> {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            let raw = self.raw();
            if done(&raw) {
                return raw;
            }
            if Instant::now() > deadline {
                let tail = &raw[raw.len().saturating_sub(400)..];
                panic!(
                    "timed out waiting for {what} in the raw output; last bytes: {:?}",
                    String::from_utf8_lossy(tail)
                );
            }
            thread::sleep(POLL);
        }
    }

    /// `:cmd` Enter.
    fn command(&mut self, cmd: &str) {
        self.send(&format!(":{cmd}\r"));
    }

    /// Leave insert mode and wait until the status line says NORMAL again.
    fn normal_mode(&mut self) -> Screen {
        self.esc("NORMAL mode", |s| s.status().contains("NORMAL"))
    }

    /// Enter insert mode with `i`, type `text`, return to normal mode.
    fn insert(&mut self, text: &str) -> Screen {
        self.send("i");
        self.wait_for("INSERT mode", |s| s.status().contains("INSERT"));
        self.send(text);
        let last_line = text.lines().last().unwrap_or_default().to_string();
        self.wait_text(&last_line);
        self.normal_mode()
    }
}

impl Drop for Term {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.child.kill();
        }
        // Bounded: a failing test must fail, not hang the suite (a zombie at worst).
        let deadline = Instant::now() + TIMEOUT;
        while matches!(self.child.try_wait(), Ok(None)) && Instant::now() < deadline {
            thread::sleep(POLL);
        }
    }
}

/// Polls a file on disk until it holds `want` (saves are immediate today, but the test shouldn't care).
fn wait_file(path: &Path, want: &str) {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let got = fs::read_to_string(path).ok();
        if got.as_deref() == Some(want) {
            return;
        }
        if Instant::now() > deadline {
            panic!("{} never held {want:?}; last read: {got:?}", path.display());
        }
        thread::sleep(POLL);
    }
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

fn rfind(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).rposition(|w| w == needle)
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[test]
fn version_and_help_print_without_a_terminal() {
    let out = Command::new(BIN).arg("--version").output().unwrap();
    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), format!("tarae {}", env!("CARGO_PKG_VERSION")));
    let out = Command::new(BIN).arg("--help").output().unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("usage: tarae [FILE]"));
}

#[test]
fn starts_shows_the_file_and_quits_restoring_the_terminal() {
    let sb = Sandbox::new("quit");
    sb.write("hello.txt", "first line\nsecond line\n");
    let mut t = sb.spawn(&["hello.txt"]);
    let s = t.wait_text("second line");
    assert!(s.contains("first line"), "{s}");
    assert!(s.contains("hello.txt"), "file name in the header\n{s}");

    t.command("q");
    assert_eq!(t.wait_exit().code(), Some(0));

    // term::restore(): DisableFocusChange, DisableMouseCapture, SetCursorStyle::DefaultUserShape, Show,
    // LeaveAlternateScreen — all after the last time the alternate screen was entered.
    let leave = b"\x1b[?1049l";
    let raw = t.wait_raw("LeaveAlternateScreen", |r| contains(r, leave));
    let entered = rfind(&raw, b"\x1b[?1049h").expect("entered the alternate screen");
    let after = &raw[entered..];
    for (name, seq) in [
        ("focus reporting off", &b"\x1b[?1004l"[..]),
        ("mouse capture off", b"\x1b[?1000l"),
        ("default cursor shape", b"\x1b[0 q"),
        ("cursor shown", b"\x1b[?25h"),
        ("alternate screen left", leave),
    ] {
        assert!(contains(after, seq), "{name}: {seq:?} missing after entering the alternate screen");
    }
    // The alternate screen is left last — nothing draws over the user's shell afterwards.
    let left_at = rfind(&raw, leave).unwrap();
    assert!(left_at > entered);
    assert!(!contains(&raw[left_at..], b"\x1b[?1049h"));
    assert!(!t.screen().contains("NORMAL"), "back on the main screen:\n{}", t.screen());
}

#[test]
fn edits_and_saves_an_existing_file() {
    let sb = Sandbox::new("save");
    let path = sb.write("note.txt", "world\n");
    let mut t = sb.spawn(&["note.txt"]);
    t.wait_text("world");
    t.insert("hello ");
    t.wait_text("hello world");
    t.command("w");
    wait_file(&path, "hello world\n");
    t.wait_text("Saved note.txt");
    // Saved, so a plain :q is allowed
    t.command("q");
    assert_eq!(t.wait_exit().code(), Some(0));
}

#[test]
fn saving_creates_a_file_that_did_not_exist() {
    let sb = Sandbox::new("create");
    let path = sb.file("fresh.txt");
    assert!(!path.exists());
    let mut t = sb.spawn(&["fresh.txt"]);
    t.wait_text("fresh.txt");
    t.insert("brand new");
    assert!(!path.exists(), "nothing written before :w");
    t.command("w");
    wait_file(&path, "brand new");
    t.command("q");
    assert_eq!(t.wait_exit().code(), Some(0));
}

#[test]
fn welcome_screen_without_a_file_then_scratch_save_as() {
    let sb = Sandbox::new("welcome");
    let mut t = sb.spawn(&[]);
    let s = t.wait_text("t a r a e");
    for hint in ["Open a file", "Find a command", "Learn the keys in 10 minutes"] {
        assert!(s.contains(hint), "{hint:?} missing\n{s}");
    }
    // Typing leaves the start screen; the scratch buffer saves under a new name
    t.insert("scratch text");
    let s = t.screen();
    assert!(!s.contains("t a r a e"), "{s}");
    t.command("w scratch.txt");
    wait_file(&sb.file("scratch.txt"), "scratch text");
    t.command("q");
    assert_eq!(t.wait_exit().code(), Some(0));
}

#[test]
fn survives_resizing_to_a_tiny_terminal_and_back() {
    let sb = Sandbox::new("resize");
    let body: String = (1..=60).map(|i| format!("line number {i}\n")).collect();
    sb.write("long.txt", &body);
    let mut t = sb.spawn(&["long.txt"]);
    t.wait_text("line number 1");
    // A processed key proves the input thread is up — crossterm installs its SIGWINCH handler there, and a
    // resize before that would go unnoticed.
    t.send("j");
    t.wait_for("cursor on line 2", |s| s.status().contains("2:1"));

    t.resize(20, 5);
    t.wait_for("a redraw at 20x5 with the status line", |s| s.rows.len() == 5 && s.status().contains("NOR"));
    // Still responsive at that size
    t.send("j");
    t.wait_for("cursor on line 3 at 20x5", |s| s.status().contains("3:1"));

    // Degenerate sizes (a pane dragged shut, a minimized window) with keys arriving meanwhile. The screen
    // isn't checked here (vt100 itself overflows on one column), nor where `j` lands (with no rows there's
    // no view to move in) — what matters is that tarae doesn't panic.
    for (w, h) in [(1, 1), (0, 0), (100, 1)] {
        t.resize(w, h);
        t.send("j");
    }

    t.resize(COLS, ROWS);
    t.wait_for("a full redraw at 100x30", |s| {
        s.rows.len() == ROWS as usize && s.status().contains("NORMAL") && s.contains("line number 20")
    });
    // Still responsive after all that
    t.send("gg");
    t.wait_for("cursor back on line 1", |s| s.status().contains("1:1"));
    assert!(t.running());
    t.command("q");
    assert_eq!(t.wait_exit().code(), Some(0));
}

/// Config errors never block startup: tarae starts on defaults and reports `file:line: problem`.
fn starts_despite_config(config: &str, line: usize, problem: &str) {
    let sb = Sandbox::new("badconfig");
    sb.write_config(&format!("{BASE_CONFIG}{config}"));
    sb.write("a.txt", "still editable\n");
    let mut t = sb.spawn(&["a.txt"]);
    let at = format!("config.toml:{line}:");
    let s = t.wait_for(&format!("the config error at {at}"), |s| s.contains(&at) && s.contains(problem));
    assert!(s.contains("still editable"), "{s}");
    t.insert("ok ");
    t.wait_text("ok still editable");
    t.command("q!");
    assert_eq!(t.wait_exit().code(), Some(0));
}

#[test]
fn bad_config_value_is_reported_with_file_and_line() {
    // BASE_CONFIG is 4 lines; the bad value lands on line 5
    starts_despite_config("tab-width = 999\n", 5, "tab-width");
}

#[test]
fn broken_config_syntax_is_reported_with_file_and_line() {
    starts_despite_config("scrolloff = = 3\n", 5, "config.toml");
}

#[test]
fn hangul_and_emoji_move_and_delete_by_grapheme() {
    let sb = Sandbox::new("unicode");
    // 👍🏽 = thumbs up + skin tone modifier: two code points, one grapheme (8 bytes)
    let path = sb.write("u.txt", "한글👍🏽x\n");
    let mut t = sb.spawn(&["u.txt"]);
    t.wait_text("한글");
    // Two steps right = on the emoji (columns shown to humans count characters)
    t.send("ll");
    t.wait_for("cursor on the emoji", |s| s.status().contains("1:3"));
    // `d` deletes the whole grapheme, never half of it
    t.send("d");
    t.wait_for("emoji deleted", |s| s.contains("한글x"));
    t.insert("타래");
    t.wait_text("한글타래x");
    t.command("w");
    wait_file(&path, "한글타래x\n");
    assert_eq!(fs::read(&path).unwrap(), "한글타래x\n".as_bytes());
    t.command("q");
    assert_eq!(t.wait_exit().code(), Some(0));
}

#[test]
fn quit_with_unsaved_changes_is_refused_until_forced() {
    let sb = Sandbox::new("dirty");
    sb.write("keep.txt", "original\n");
    let mut t = sb.spawn(&["keep.txt"]);
    t.wait_text("original");
    t.insert("changed ");
    t.command("q");
    t.wait_text("unsaved changes");
    assert!(t.running(), "`:q` must not quit with unsaved changes");
    assert!(t.screen().contains("changed original"));

    t.command("q!");
    assert_eq!(t.wait_exit().code(), Some(0));
    assert_eq!(sb.read("keep.txt"), "original\n", "`:q!` discards, never writes");
}

#[test]
fn which_key_and_command_palette_open_and_close() {
    let sb = Sandbox::new("palette");
    sb.write("p.txt", "palette test\n");
    let mut t = sb.spawn(&["p.txt"]);
    t.wait_text("palette test");
    // `space` alone: which-key card lists what follows, described in plain words
    t.send(" ");
    t.wait_text("Find a command");
    // `?` opens the palette: input row `› …  commands  n/n`
    t.send("?");
    t.wait_for("the command palette", |s| s.rows.iter().any(|r| r.contains('›') && r.contains("commands")));
    t.send("save");
    t.wait_for("the query in the palette", |s| s.contains("› save"));
    let s = t.esc("the palette to close", |s| !s.contains("commands") && s.contains("palette test"));
    assert!(s.status().contains("NORMAL"), "{s}");
    assert!(t.running());
    t.command("q");
    assert_eq!(t.wait_exit().code(), Some(0));
}

#[test]
fn claude_code_lock_file_stays_in_the_sandbox_and_is_removed_on_quit() {
    let sb = Sandbox::new("lock");
    sb.write("x.txt", "x\n");
    let mut t = sb.spawn(&["x.txt"]);
    // Written before the first frame (main.rs: agent_start runs before term::run)
    let locks = sb.lock_files();
    assert_eq!(locks.len(), 1, "one lock file under CLAUDE_CONFIG_DIR/ide: {locks:?}");
    let body = fs::read_to_string(&locks[0]).unwrap();
    assert!(body.contains("\"ideName\":\"tarae\""), "{body}");
    assert!(body.contains(&format!("\"pid\":{}", t.child.id())), "{body}");
    t.command("q");
    assert_eq!(t.wait_exit().code(), Some(0));
    assert!(sb.lock_files().is_empty(), "lock file removed on quit");
}
