# tarae (타래)

> A skein of thread — winding on from the feel of the helix, but spinning fresh yarn.

![tarae — meok (ink): ink-dark background + skein coral. Completion card, docs window, path bar, mode pill](docs/screenshots/hero-meok.png)
![tarae — hanji (paper): warm paper background + seal vermilion. Picked automatically when the terminal background is light](docs/screenshots/hero-hanji.png)

tarae is **a new editor that inherits Helix's keymap and its "batteries included" approach**.
It is not a fork. From Helix it borrows only the fingers (the key layout) and the selection → action grammar;
everything else is designed from scratch to fix four things that bothered us about Helix.

| What bothered us in Helix | tarae's answer |
|---|---|
| Changing settings is a chore | Applied on save, `:set` and a settings picker, one file |
| Slow when an LSP attaches | Input and rendering never block on anything — LSP is fully asynchronous |
| No LLM support | Select → instruct → diff, a chat panel, and external-agent integration, built in |
| Plugins are immature | Put it straight into the core instead, plus Claude Code in a side pane (WASM plugins on hold — see below) |

## Principles

1. **Your hands are in Helix.** The keymap and the selection → action grammar (`w` *selects* a word, `d` deletes it) match Helix.
   Command names follow Helix too — so key configs carry over as-is.
2. **Batteries included.** Needed features go into the core, kept small. (The plugin system is on hold — not built while the core is enough.)
3. **Configuration should be easy — configuration is data.** No logic in config files.
   Changes apply on save (no restart), can be made at runtime with `:set`, and can be picked in a settings picker with descriptions alongside.
   Bad settings don't block startup; they're reported with line numbers. ("Configuration" section below)
4. **Never make you wait.** Keystrokes and screen updates never block on LSP, LLM, file loading, or parsing.
   Slow work all happens in the background, and late results are discarded.
   Performance budgets (first frame, key → screen) are pinned down as tests.
5. **The LLM is a first-class verb.** In the selection-first model, the LLM takes the "action" slot — apply an instruction to each selection,
   show it as a diff, then accept/reject. Editor state is also open to external agents (Claude Code, etc.).
6. **Makes you want to try it at a glance.** Design weighs as much as features — even someone with no interest in terminal editors should
   see the screen and want to open it. Everything floating speaks the same color language as the code (syntax colors, theme), breathes with whitespace,
   and shows information by priority (when narrow, the least important folds away first). New screen elements are checked with a screenshot before moving on.
7. **Small and testable.** The editor core doesn't know about the terminal — it's tested whole by feeding it key sequences
   (`src/editor.rs` tests: `"xd"`, `"Ci-<esc>"` …).

## Status — M0–M5 done · editor-experience bundle done (M6 plugins on hold)

- Modes: normal / insert / select, `:` command line, count prefixes (`3w`, `2gg`, `2x`)
- **`:` command-line completion**: a card of candidates above the command line as you type — command names (fuzzy, with aliases and descriptions), then arguments:
  file paths (`:o`·`:w`·`:vs`… folders first, in blue; hidden ones too once you type a dot), themes, setting paths and their values
  (`:set editor.auto-save "idle"`), toggleable settings (`:toggle`), languages (`:lang`). `Tab`/`Shift-Tab` = cycle and fill
  (if there's only one candidate, it fills through the trailing space → the next Tab moves to the next argument); the rest of the first candidate is shown dimmed after the cursor — take it with `→`
  ([screenshot](docs/screenshots/ux-cmdline-complete.png))
- Movement: `h j k l`, `w b e`, `gg ge gh gl gs`, `G`, `C-f C-b C-d C-u`, `A-.` (repeat_last_motion)
- Selection: `x` (line by line downward), `X` (line by line upward), `A-x` (to line bounds), `%`, `;`, `A-;`, `,`, **multiple cursors** `C` / `A-C`
- Search and regex selection: `/ ? n N *` (`n` in select mode adds), **`s`** (select every match within the selection),
  `S` (split on matches), `A-s` (split into lines), `K`/`A-K` (keep/remove selections that match)
- Editing: `i a I A o O`, `d c y p P` (including linewise registers), `A-d A-c`, `u U` (per edit group),
  auto-indent in insert mode, `r` (replace character), `~` `` ` `` `` A-` `` (case), `> <` (indent), `J` (join lines)
- Character find: `f t F T` (repeat with `A-.` — remembers the target character too)
- `m` mode: `mm` matching bracket, `mi`/`ma` + `w W p ( [ { < " ' `` ` `` (character-based) · `f t a c T` (function, type, argument,
  comment, test — tree-sitter `textobjects.scm`; repeat to widen one layer at a time), `ms`/`mr`/`md` surround
- **Design (principle 6)** — built-in themes **meok (ink, `meok`) and hanji (paper, `hanji`)** + **meok-transparent and hanji-transparent**, which don't paint a background
  (`meok-transparent`·`hanji-transparent` — the terminal's background and blur show through): the default `theme = "default"` asks the terminal
  for its background color at startup (OSC 11 + DA1 — never waits, even on terminals that don't answer) and picks meok if it's dark, hanji if it's light.
  Force it with `TARAE_BACKGROUND=light|dark`. Only one accent color (`ui.accent` — coral in meok, vermilion in hanji) is used boldly;
  syntax colors are desaturated and calm. Floating things (completion, docs, hover, pickers) are **cards** — half-cell blocks (`▄ ▀`) give
  half a line of padding above and below, and the selected row gets an accent bar `▎`. The kind glyphs `ƒ τ ν π § ◦ # ¶` were chosen because all four
  common monospace fonts have them. Themes without a background color get rounded borders instead of cards. Mockups exploring the direction are in `docs/design/`
- **Top bar (path bar)**: `project › folder › file ● › ƒ definition` — the function/type/module enclosing the cursor, via tree-sitter
  (from node names, no per-grammar queries); with several buffers open, a list on the right. Turn it off with `editor.header = false`
- Current-line highlight (`editor.cursorline`, on by default, theme key `ui.cursorline.primary`)
- **Status line (tarae style)**: mode pill (`▐ NORMAL ▌` — a color per mode) · git branch (watches HEAD — switch branches in lazygit and it updates
  immediately) …… claude status (`…` awaiting a response / `● review` awaiting review) · diagnostics `● errors ▲ warnings` · language ·
  encoding and line endings · selection count · position. When narrow, the least important items drop first. With the header off, the file name moves here
- **Key hints**: while a completion or docs window is open, the keys that operate it appear dimmed on the command line (`tab select  enter accept …`)
- **Usable right away, even the first time** — no need to memorize keys:
  - **which-key**: press `space`·`g`·`m`·`[`·`]` and a card shows the next keys and what they do ([screenshot](docs/screenshots/ux-which-key.png))
  - **Command palette** `space ?`: find any command by its description, with its bound key on the right (`:` commands like `:w`·`:chat-new` too)
    ([screenshot](docs/screenshots/ux-palette.png))
  - **Start screen**: launched without a file, it shows first-step keys + recent files (open them with number keys — `$XDG_STATE_HOME/tarae/recent`)
    ([screenshot](docs/screenshots/ux-welcome.png))
  - **`:tutor`**: a 10-minute hands-on tutorial (in a practice buffer — editing it doesn't block quitting)
    ([screenshot](docs/screenshots/ux-tutor.png))
  - **Theme picker** `space t` (`:theme`): a small window at the top center — the theme is applied live to the code behind it as you move,
    `Esc` reverts, `Enter` saves it to your config. meok · hanji · meok-transparent · hanji-transparent (+ anything in `~/.config/tarae/themes/`) ([screenshot](docs/screenshots/ux-theme-picker.png))
  - **Session restore** (`editor.restore-session`): launched with no arguments, it reopens the files you had open in this folder at their positions (cursor, scroll);
    open a file by name and you land where you last were in it (`$XDG_STATE_HOME/tarae/session.json`)
  - **Undo that survives restarts** (`editor.persistent-undo`): on save, the undo history (last 1000 steps) is written to
    `$XDG_STATE_HOME/tarae/undo/`, and when you reopen the file, `u`/`U` pick up right where they left off. The history is tied to a fingerprint of the saved
    content, so if the file changed outside, it's silently dropped (writing and reading both happen on a worker thread)
  - **Mouse**: click = cursor, drag = select, alt+click = add a cursor, wheel = 3 lines, click the chat panel = focus it;
    in pickers, wheel = move the selection, click = open (terminal text selection is usually shift+drag)
- **Notifications (toasts)**: status messages appear as cards at the top right of the editing area — the color of the left bar gives the kind (info, saved, warning, error);
  they fade in (150 ms) and fade out (400 ms). The more serious, the longer they stay (2.5 s – 7 s). The command line is only for input, key hints, and
  the diagnostic at the cursor ([screenshot](docs/screenshots/ux-toasts-sync.png))
- **Automatic disk sync (like IntelliJ)**: when an open file changes elsewhere (git checkout, a formatter, an agent), it's noticed
  within 0.5 s (stat on a worker thread; immediately when the terminal regains focus) — if you haven't edited it, **only the changed ranges** are
  reloaded (cursor, scroll, and undo kept; `u` brings back the previous content). If you have, you get a conflict notice + `▲ changed on disk` in the top bar;
  `:reload` (the disk version) / `:w!` (yours) — plain `:w` is refused during a conflict. **Auto-save** `editor.auto-save`:
  `"focus"` (default — when the terminal loses focus) · `"idle"` (+ when typing pauses for 2 s) · `"off"`
- **git changed lines**: a thin bar between the line numbers and the text — added lines green · modified lines yellow · deletions a red underline
  (compared against HEAD; histogram diff via `imara-diff` on a worker thread, 80 ms after typing pauses). `+3 −2` next to the branch in the status line,
  colored ticks on the scrollbar too, `]g` `[g` next/previous hunk. Commit outside and come back (focus), and the baseline is
  reread ([screenshot](docs/screenshots/ux-git.png))
- **Rendered doc comments** (like IntelliJ's Rendered documentation comments): `///` `//!` `/** */` blocks are shown
  **folded into a single unit** — markers stripped, continuation lines merged into paragraphs and rewrapped to the width (Markdown rules — same as
  rustdoc), blank lines between paragraphs collapsed to one, code fences removed, so it's tighter than the source. Body text is one tone lower than code (terminals
  can't change font size, so contrast makes it "smaller"), grouped by a panel lightly tinted with the comment color + a left bar; headings in bold accent, `code`,
  list `•`, `@param` tags, and example code in its language's colors in a well of the editor background. **In normal mode it stays folded as you pass through**
  — `j`/`k` step over the block as one line, and the bar of the block under the cursor turns accent. **When editing or selecting (INSERT/SELECT, or when
  a selection wider than one character overlaps it), it unfolds into the source lines** (`editor.render-doc-comments`,
  [folded](docs/screenshots/ux-doc-comments.png) · [unfolded](docs/screenshots/ux-doc-comments-open.png)).
  Plain comments (`//`) aren't rendered — that would mangle commented-out code, tables, and aligned text (only `TODO`·`NOTE` are colored)
- **Tests** (`space x`): `x` the test at the cursor · `d` that test under the debugger · `f` this file's tests · `l` rerun the last one
  (as debug if it was) · `c` close the panel (kills the run if it's going). **Java** (JUnit 4/5, TestNG `@Test` and friends — Gradle
  `./gradlew :module:test --tests package.Class.method`, nested classes as `Outer$Inner` · Maven `./mvnw -pl module
  -Dtest=Class#method test`; debug = when the build tool starts the test JVM waiting for a debugger (`--debug-jvm` ·
  `-Dmaven.surefire.debug`), it attaches on its own — [screenshot](docs/screenshots/test-debug-java.png)) · Rust (`cargo test -- module::path::name --exact`,
  integration tests with `--test`) · Go (`go test -v -run '^TestX$'`, benchmarks with `-bench`) · Python (`pytest file::Class::function`,
  with the project's `.venv` or `$VIRTUAL_ENV` Python). Discovery works **on the tree-sitter tree** (`#[test]`-style attributes, `mod` paths, `TestXxx`,
  `class Test…`/`def test…`) — if that language's grammar is missing, it asks "Python grammar needed · Download it, then run the test?"
  and a single `y` downloads it and **runs the test right away** (same for F5 debug and attach). If the cursor is outside a test, the enclosing
  `mod tests`/`class Test…`; failing that, the whole file. Modified files are saved before running, and results go to the **test panel**
  in the debug panel's spot — a header like `FAILED  math::*   ● 1  ▲ 1   26 ms`, on the left each test as `●` passed · `▲` failed · `◦` skipped
  (distinguishable by shape too; shared name prefixes are trimmed), on the right the selected failure's message (`left: 3` / `right: 4`) and location.
  **A red `▲ message` at the end of the failing line** (like Error Lens, with a faint red band on the line); `]t`/`[t` jump between failures.
  Results are read from libtest output · `go test -v` · `pytest -v --tb=short` · for Java, Gradle/Maven JUnit XML reports
  ([Rust](docs/screenshots/test-results-rust.png) · [Go](docs/screenshots/test-results-go.png) ·
  [Python](docs/screenshots/test-results-python.png) · [Java](docs/screenshots/test-results-java.png)).
  If the results can't be read yet (still building, compile error), the full output is shown. Debugging runs just that one test
  (Rust = the test binary from `cargo test --no-run` under lldb-dap, Go = dlv `mode: test`, Python = pytest under debugpy),
  and afterward `F5` = the same test again ([run](docs/screenshots/test-run.png) · [debug](docs/screenshots/test-debug.png)).
- **Remote debugging — attach to a running program** (`space g a` picker · `:attach name|host:port`): Python (over TCP to `python -m debugpy
  --listen 5678 app.py`) · Go (over TCP to `dlv … --headless --listen :2345 --accept-multiclient`) · Java (JVM
  `-agentlib:jdwp=transport=dt_socket,server=y,address=*:5005` — via JDWP through jdtls's java-debug) · Rust/C (lldb-dap `gdb-remote` to `gdbserver`·
  `lldb-server gdbserver`·`debugserver`, or a local process with `pid`). In an attached session, `space g t`
  **detaches** (the remote program keeps running). Give the places you attach to often a name in your config:
  ```toml
  [[attach]]
  name = "api (k8s)"
  lang = "java"                  # defaults to the current file's language
  port = 5005                    # host defaults to 127.0.0.1 · for a local process, pid = …
  before = "kubectl -n app port-forward deploy/api 5005:5005"   # started first, stopped when the session ends
  remote-root = "/app"           # source root inside the container ↔ this project (maps breakpoint paths)
  # program = "target/debug/app" # Rust/C: the executable to read symbols from
  ```
  ([Python](docs/screenshots/attach-python.png) · [Go](docs/screenshots/attach-go.png) · [Java](docs/screenshots/attach-java.png) · [Rust](docs/screenshots/attach-rust.png)).
- **Debugger (DAP)**: Rust/C/C++ (lldb-dap — for Rust, the executable is built with `cargo build` first), Go (dlv), Python (debugpy),
  **Java** (java-debug inside jdtls — the current file's `main`, otherwise the project's first `main`; Maven, Gradle, or a folder with no build file).
  `F9` breakpoint (red `●` before the line number) · `F5` start/continue · `F10` step over · `F11` step in · `F12` step out ·
  `space g t` stop (everything is under `space g`). When paused, a `▶` and a band on that line, **the values of variables used up to that line at the line's end**
  (`a = 2  b = 3`), and the debug panel below — state pill (RUNNING·PAUSED·EXITED) · variables (values colored by shape, with types) ·
  call stack (frames in your code crisp, standard-library frames dimmed) · program output (stderr in red)
  ([Rust](docs/screenshots/ux-debugger.png) · [Python](docs/screenshots/ux-debugger-python.png) · [Go](docs/screenshots/ux-debugger-go.png) · [Java](docs/screenshots/ux-debugger-java.png)).
  **Conditional breakpoints** `space g C-c` (`break when: step == 3` — stops only when true) · **logpoints** `space g C-l`
  (prints `a is {a}` to the output panel without stopping) — the `●` before the line number turns orange (condition) or accent (log), and
  `● when step == 3` is appended dimmed at the end of that line. If the adapter rejects it (a bad expression), the reason appears right there in red.
  **Watch expressions** `space g w` (prefilled with the selection or the word under the cursor) / `:watch expr` — evaluated in the current frame on every pause and shown
  in WATCH at the top of the debug panel (`◦ a * b  6  int`; if it doesn't evaluate, `NameError …` dimmed); remove with `space g W` / `:unwatch`
  ([screenshot](docs/screenshots/ux-debugger-conditions.png)).
  For Go, dlv must be newer than the installed Go — if it's out of date, the notification shows dlv's own explanation verbatim
  (`go install github.com/go-delve/delve/cmd/dlv@latest`).
  On macOS, launching processes with lldb needs a one-time `sudo DevToolsSecurity -enable` (or approving the password prompt) —
  if it can't start within 8 seconds, a notification tells you
- **Split windows** (Helix keys): `C-w v` side by side · `C-w s` stacked · `C-w w`/`h j k l` move between panes · `C-w q` close ·
  `C-w o` only this pane (`space w` works too), `:vsplit file` `:hsplit file`; with several panes, `:q` closes just that one. Each pane
  has a title line — the focused pane gets an accent bar and a faint band, the rest sink back dimmed (dim attribute). Show the same document in two panes and
  the cursor is shared, while scroll is per pane. Clicking a pane focuses it ([screenshot](docs/screenshots/ux-splits.png))
- **Search**: every match in a faint accent, the current one strong, `/pattern  8/13` in the status line. While typing after `/`,
  a **preview** — matches highlight as you type, and if the first match is off-screen only the view moves (Esc puts you back). In normal mode
  `Esc` clears the highlight ([screenshot](docs/screenshots/ux-search.png))
- **Indent guides** (`editor.indent-guides`, unbroken across blank lines, the guide of the block containing the cursor one tone brighter) · **scrollbar** (`editor.scrollbar`,
  with colored ticks for error and warning lines across the whole file) ([screenshot](docs/screenshots/ux-guides.png))
- **Pickers** (fuzzy — nucleo, the same matcher as Helix): `space f` files (`git ls-files` in a git repository — respects .gitignore,
  otherwise a direct walk), `space b` buffers, `space /` global search (ripgrep if `rg` is installed, otherwise built-in regex).
  Lists and searches fill in on a worker thread, showing "loading…" meanwhile. `C-n/C-p`, arrows, `Tab`, `Enter`, `Esc`.
  With enough width (≥ 100 columns), a **preview** on the right — syntax-highlighted, with search results highlighted at their line
  (only the first 512 KB of the file is read and parsed, on a worker thread; binary and non-UTF-8 files just get a notice; 64-entry cache).
  Two cards (list · preview), with the title and count to the right of the `›` input line
  ([file picker](docs/screenshots/m2-file-picker.png) · [global search](docs/screenshots/m2-global-search.png))
- Registers: pick one with `"x`, `_` (black hole), **`+` = the system clipboard** (`space y/p/P`, pbcopy·wl-copy·xclip —
  external commands, so on a worker thread)
- Macros: `Q` (start/stop recording), `q` (replay, takes a count), `"xQ` to record into a named register
- Buffers: multiple files, `gn gp`, a bufferline (when there are 2 or more)
- Commands: `:w [path]` `:q` `:q!` `:wq`/`:x` `:open`/`:e` `:new` `:bc[!]` `:bn` `:bp` `:sh` `:<line>`
- Configuration: tarae schema ("Configuration" section below), `:set`/`:set!`/`:toggle`/`:config-show`
- Rendering: relative line numbers, selections and secondary cursors, per-mode cursor shapes, correct width for wide characters such as Hangul

- Character unit: movement, deletion, `r`, `f`, and rendering all work in grapheme clusters (`é`, NFD Hangul, 👍🏽, 👨‍👩‍👧, `\r\n`)
- **Syntax highlighting (tree-sitter)** — a Helix installation is never read (2026-09-27). Grammars are built **from the upstream repositories'
  C sources, without crates.io crates**, pinned to the same commits as the queries (`[[grammar]]` in `src/languages.toml`).
  - **The default build includes only what's needed to work on tarae itself** — Rust, TOML, Markdown, and `TODO` in comments (comment) (5.7 MB binary,
    `core = true` in `languages.toml`). The first time you open a file in any other language, a notification appears at the bottom right —
    "Python syntax colors · Download and build the tree-sitter grammar?" → `y` download · `n` not now (normal mode; the mouse works too).
    A wave shows while it downloads, and the colors switch on in place when it's done. On failure, it shows the reason plus `y` retry · `n` dismiss. A language you
    answered `n` to isn't asked about again this session. Multiple notifications **stack** — the ones behind peek out one line each (title only) above the front card, which
    shows `1/3`; only the front card takes keys, and new notifications go to the back (so the front card doesn't change while you're answering). A card you answered `y` to steps
    to the back and shows its wave while downloading, and the next one comes forward; click a card behind to bring it to the front ([screenshot](docs/screenshots/offer-stack.png)).
    Downloading = fetching the same C source with git and building it with `cc` into `~/.local/share/tarae/grammars/` (needs git and a C compiler).
    To stop being asked, set `editor.offer-grammars = false` — then use `:grammar-install [lang|all]` / `tarae grammar install [lang…]`.
    Check with `tarae grammar list`. 54 supported — C, Java, Python, JS/TS, Rust, Go, Bash, Terraform (HCL), YAML, **Helm templates**,
    Markdown, Dockerfile, C++, SQL, Lua, Zig …
    Helm = a chart's `templates/*.yaml`·`_*.tpl`·`NOTES.txt` — `{{ }}` gets Go template colors, and the parts in between are stitched together and colored as YAML
    (templates inside quotes get their own colors too). The `helm_ls` language server attaches if it's installed.
  - **Build with the grammars included** (e.g. on a locked-down network): `cargo install --path . --features bundled-grammars` — compiles the 26
    grammars marked `bundle = true` in `languages.toml` (C, Java, Python, JS, Rust, Go, go.mod, Bash, fish, HTML, CSS, XML, JSON, YAML, TOML, HCL, Helm/Go templates,
    protobuf, Markdown, Dockerfile, Makefile, justfile, `.gitignore`, diff, comment) from `runtime/grammars.tar.gz` (2.0 MB,
    packed by `cargo xtask vendor-grammars`) into the binary (no git or GitHub access needed). 9.8 MB binary.
  - **Queries** (the `.scm` coloring rules) in `runtime/queries/` (taken from Helix 25.07.1 — MPL-2.0, LICENSE and NOTICE in the folder) are embedded in the binary.
  Parsing is background + incremental (edits are applied to the existing tree immediately, so highlighting keeps up); queries run only on the visible lines.
  Measured: full parse of a 20k-line Rust file 145 ms (in the background), worst key → frame with highlighting on 1.1 ms (release).
  **Injections**: Markdown code blocks in their language (` ```rust `·`rs`·`sh`·`py`…; the shebang if unnamed),
  bold, code, and links inside paragraphs, `TODO`·`NOTE` in comments, even inside macros (`injections.scm`).
  Layer parsing happens in the background too (3,700-line Rust file = 472 layers, 30 ms), and edits are applied to the layer trees immediately as well
  ([screenshot](docs/screenshots/syntax-injections.png))
  `:lang <name>` to set the language manually. Not yet: locals
- **Themes**: four built-ins (meok, hanji, meok-transparent, hanji-transparent) + `~/.config/tarae/themes/*.toml`. `theme = "name"`, `:theme name`.
  `"default"` = meok or hanji to match the terminal background. Format:
  ```toml
  inherits = "meok"                      # layer on top of a built-in theme or one of your own
  "keyword" = "accent"                   # color only
  "comment" = { fg = "dim", italic = true }
  "diagnostic.error" = { underline = { color = "rose", style = "curl" } }
  [palette]
  accent = "#ef8a5a"
  link = "accent"                        # palette entries can point to each other
  ```
  Modifiers = `bold`·`italic`·`underline`·`dim`·`reversed`·`crossed` (`modifiers = [...]` is read too), colors = `#rrggbb`·`#rgb`·
  ANSI names (`red`·`light-blue` …)·palette names. Misspelled color names and unknown attributes aren't silently dropped — they're reported as warnings.
  tarae-only keys: `ui.accent` (the single accent color), `ui.tint` (the base that faint colors are blended with — for the transparent variants)
- **LSP (phase 1)**: opening a file launches and attaches a language server (default candidates: rust-analyzer, gopls, pyright,
  typescript-language-server, clangd, jdtls, marksman, taplo, etc. — the first one on PATH; change with `[lsp.<name>]` / `[lang.<language>] lsp`,
  disable with `editor.lsp`). **Fully asynchronous** — reader, writer, and stderr threads per server; the main thread only pushes onto a channel;
  even serializing large bodies happens on the writer thread. Positions request UTF-8 first (the editor is byte-based, so zero conversion); UTF-16 servers are supported too.
  Changes go out as incremental didChange (once per event batch); undo does a full sync.
- **Java** — unpack [jdtls](https://download.eclipse.org/jdtls/snapshots/jdt-language-server-latest.tar.gz) (runs on Java 21+)
  and put its `bin/` on PATH. Project root = the **topmost** `pom.xml`·`settings.gradle`·
  `build.gradle` within the repository (`.git`) (so multi-module projects don't start a jdtls per module); the workspace folder is `~/.cache/tarae/jdtls/<name>-<hash>`.
  While importing (Maven/Gradle import), progress shows in the status line. Debugging needs the java-debug plugin (a jdtls extension jar — not included in the jdtls
  distribution); if it's missing, `F5` offers to download it (`y` → `curl` the latest release from Maven Central into
  `~/.local/share/tarae/java-debug/` → load it into jdtls with `java.reloadBundles` and start debugging right away, **with no restart**).
  Copies already downloaded by VS Code or Neovim's mason are found too. **`gd` into library and JDK classes** — the source behind the `jdt://` URIs jdtls
  returns (real source for Maven/Gradle dependencies via source jars, otherwise decompiled) opens in a read-only buffer (no language server attaches
  there yet — so another `gd` from inside it doesn't work).
  - **Inline diagnostic messages (like Error Lens)**: `● message +N` at the end of lines with errors or warnings — the symbol in the severity color, the text
    in italics one tone lower, and error/warning lines get a very faint wash of that color (`editor.inline-diagnostics`,
    [screenshot](docs/screenshots/ux-error-lens.png))
  - Diagnostics: `●` before the line number, range underlines (curly only on terminals known to support it — unknown terminals would print `3m` as text),
    the message at the cursor, counts in the status line, `]d` `[d`, a `space d` list. Diagnostic positions move along with edits
  - **Symbol search**: `space s` symbols in this file (starting from the symbol containing the cursor, with its container like `impl Chat` on the right) ·
    `space S` the whole workspace — re-queries the server as you type (after a 120 ms pause). The kind glyphs `ƒ τ § ◦ π ν` use
    the same colors as code, and the preview jumps to the line ([file](docs/screenshots/ux-symbols.png) ·
    [workspace](docs/screenshots/ux-workspace-symbols.png))
  - `gd` definition (a picker if there are several), `gr` references, `space k` hover, server progress (indexing, etc.) in the status line
  - **`space a` code actions** (preferred ones and quick fixes first; edits, commands, and resolve supported) — **see what will change as a diff on the right
    before you pick**: each time you move the selection (for lazy actions, only the edit is fetched first), before and after in their syntax colors,
    deleted lines faint red, inserted lines faint green, the part that actually changed one tone stronger; with multiple files, a header per file
    (`+ new file` · `→ move` · `− delete`). Prefetched edits apply immediately when picked
    ([screenshot](docs/screenshots/m3-code-action-preview.png)).
    `space r` rename (across files), `:format`/`:fmt`. Edits sent by the server (`workspace/applyEdit`) are applied too.
    One undo step per file. **Creating, moving, and deleting files** (`resourceOperations`) happen in order too — a move renames the open buffer
    too (closed and reopened on the server), a delete closes the buffer (this one is outside undo)
  - **Completion** — pops up as you type (including trigger characters like `.`); further typing is filtered with fuzzy matching
    without asking the server again. Kinds in the same colors as code, typed characters highlighted, the name column aligned with the word you're typing.
    `Tab`/`C-n` to select → `Enter` to insert (snippets expanded with the cursor at the first stop, auto-import edits included), `C-x` to invoke it manually.
    The selected item's signature and docs in a side window (below when narrow) — docs are fetched with `completionItem/resolve` when an item is selected
    ([screenshot](docs/screenshots/m3-completion.png))
  - **Signature help** — typing `(` or `,` (the triggers the server announced) in insert mode shows a signature card above the cursor line:
    in the language's syntax colors, the current parameter in bold accent and underlined, the parameter's docs below (or the first paragraph of the function's docs if there are none),
    `1/3` for overloads. While it's shown, it re-queries on every edit and movement (type again while a request is in flight and the stale response is dropped and it asks once more),
    and it closes when you leave the parentheses or insert mode ([screenshot](docs/screenshots/m3-signature.png))
  - **Inlay hints** — types and parameter names as dimmed italic virtual text (`let t: Thread = splice(left: &a, …)`).
    Only the visible range plus one screen above and below is requested; while you type it waits and asks once when you pause (120 ms). Until
    new hints arrive, the old ones follow your edits and hold their place. The server's `workspace/inlayHint/refresh` is honored too.
    Turn them off with `editor.inlay-hints = false`
    ([screenshot](docs/screenshots/m3-inlay-hints.png))
  - **Markdown docs window** (shared by hover and completion) — code blocks in the same tree-sitter colors as the editor, paragraphs wrapped to the width,
    lists, quotes, and rustdoc links tidied up. It never covers the cursor line; scroll long ones with `C-d`/`C-u`
  - Claude requests moved to `space i` (`space a` = code actions, same as Helix — decided 2026-09-27)
  - Late responses (the cursor or document changed in the meantime) are discarded
  ([diagnostics](docs/screenshots/m3-diagnostics.png) · [hover](docs/screenshots/m3-hover.png))
- Positions are **byte offsets** internally — tree-sitter, regex, and LSP all speak bytes, so there's no conversion layer
- Vertical movement remembers the visual column (accounting for tabs and wide characters) — you return to the original column after passing short lines

**M1 ✅ done** — event loop rebuilt: an input thread + a single event queue (`src/event.rs`, zero dependencies);
queued events are processed together and drawn once; slow work goes through `Jobs::spawn` (`:sh` now runs in the background too);
config file changes apply on save (a watcher thread reads and parses them — the main thread never waits on disk).
Performance budget test: worst key → frame on a 200k-line file **debug 1.0 ms / release 52 µs** (budget 16 ms).
Large files (≥ 1 MB) load in the background — **100 MB file: first screen 18 ms, fully loaded 75 ms** (measured, release).
Editing and saving are blocked while loading (prevents the accident of saving a half-empty buffer and wiping the file).

**LLM (Claude) — M4 ✅**

![Chat panel — ask with the selection as context; code in answers sits in a well in its language's colors](docs/screenshots/m4-chat.png)

- **Select → instruct → diff**: select → `space i` (or `:ask <instruction>`) → Claude answers for each selection → **diff review**
  (`y` accept · `n` reject · `a` all · `q` reject the rest · `tab` next · `j/k` scroll) → applied as a single transaction
  (one `u` undoes it all). Answers **stream in as they come**, in a card at the bottom right (wave `▂▅▇` · thinking/writing · elapsed time);
  you can keep editing while you wait, and if the original text moved in the meantime it's located again and applied. `:ask-cancel` = **kills the process**,
  so token generation stops too. **Review happens inside the buffer (like Cursor)** — the whole file is shown as-is, with only the changed spots expanded:
  deleted lines on a faint red and added lines on a faint green, in the original syntax colors, with a header line per change (key hints on the current one); accept and
  only the new text remains, reject and only the original does ([screenshot](docs/screenshots/m4-review.png))
- **Chat panel** `space l` (close with `space L`, `:chat [question]`, `:chat-new`): a panel on the right. One process lives for the whole conversation
  and remembers what came before (warmed up when the panel opens), and every message carries **the current file, cursor, selection, and diagnostics** as context
  (the full file only the first time that version is sent; over 60 KB, just the area around the cursor). The context sent is shown as chips above the message
  (`main.rs · L13–18 · 3 diagnostics`). Answers are Markdown — code blocks in the language's colors + a "well" of the editor background.
  `enter` send · `alt-enter`/`C-j` newline · `C-c` **stop** (interrupt — the process and its memory stay) ·
  **`C-r` replace the selection with the answer's code** (same diff review) · `C-y` copy code · `C-l` new conversation · `esc` back to the editor.
  An empty conversation shows a guide + example questions via `tab` ([hanji](docs/screenshots/m4-chat-hanji.png))
`claude -p` is invoked as a subprocess, so the editor has no HTTP, SDK, or API key (it uses your Claude Code login as-is).
Speed: spawning fresh every time takes 8–10 s, so the process is started ahead of time the moment the ask prompt opens, overlapping with your typing
→ **Enter → diff 3.4 s** (measured, default model). Cancel with `:ask-cancel`.

**Agent (Claude Code) — M5 ✅**

![Edits from Claude Code in a side pane arrive as a review inside the tarae buffer — y/n/a](docs/screenshots/m5-agent-review.png)

- **The `claude` running in a side pane connects to tarae** — it's Claude Code's IDE integration protocol as-is (the same one the VS Code and JetBrains extensions use:
  MCP over WebSocket), so there's nothing to configure on the Claude Code side. `space c` = launch it, already connected, in a zellij/tmux side pane;
  for a `claude` you already have running, `/ide` → tarae. Once connected, the status line shows `◦ claude code`
  ([screenshot](docs/screenshots/m5-agent-connected.png)).
- What Claude sees: **your current selection** (sent on every change — its input shows `⧉ 1 line selected`), open files and unsaved
  edits, language-server diagnostics (`getDiagnostics` — "fix this error" just works). `space C` = insert the selected lines into Claude's input
  as `@file#L10-20`.
- **Claude's edits are received in tarae**: when an edit or a rewrite comes in, tarae opens the file and expands it in the in-buffer review (the same screen as ask —
  headed `Claude Code`). `y`/`n` per change, `a` for all. Accept only part of it and Claude understands
  ("you edited the proposed content") — Claude writes the file, and tarae treats the same content arriving on disk as "saved".
  A bell gets the attention of whoever is waiting in the side pane.
- Security: listens only on `127.0.0.1` and checks the 128-bit token from `~/.claude/ide/<port>.lock` in a header. Disable with `llm.claude-code = false`.
  WebSocket, SHA-1, and base64 are implemented in-house (zero dependencies).

## Running

```sh
cargo install --path .     # install (Rust 1.88+, a C compiler) — Rust, TOML, and Markdown are colored out of the box; other languages offer a download the first time you open them (git)
# cargo install --path . --features bundled-grammars   # with the 26 most common grammars built in (C compiler only)
tarae path/to/file
cargo test                 # editor core tests (key sequence → text/selection)
```

## Configuration

`$XDG_CONFIG_HOME/tarae/config.toml` (default `~/.config/tarae/config.toml`).

**TOML format, tarae's own schema.** ✅

- **One file** — editor, key, and LLM settings all live here (no separate `languages.toml` as in Helix; `[lang.*]`·`[lsp.*]` were added to this file in M2–M3).
- **Applied on save** — no restart. Invalid values don't block startup; they're reported as `file:line: message`.
- **Setting name = `:set` path.**
  - `:set editor.scrolloff` — show the current value, description, and allowed values
  - `:set editor.scrolloff 8` — this session only (this value wins even if the config file is reloaded)
  - `:set! editor.scrolloff 8` — also write it to the file (comments and formatting preserved via `toml_edit`)
  - `:toggle editor.color-modes` — flips a bool; for a choice setting, moves to the next value
  - `:config-show` — the full effective configuration, with descriptions as comments, in a new buffer (usable as a config file as-is)
  - `:config-open` · `:config-reload`
- **The schema lives in one place in the code** (`src/settings.rs`) — one line per setting: path, kind, description, get/set.
- **Layer order:** defaults < user config < project `.tarae.toml` (searched upward from the current directory) < runtime `:set`.
- **All that's borrowed from Helix is the key notation (`"A-x"`) and the command names.** Helix config, themes, and grammars are not read.
  Example: [`examples/config.toml`](examples/config.toml).

```toml
[editor]
line-numbers = "relative"     # "absolute" | "relative" (default relative)
scrolloff = 5
tab-width = 4
color-modes = true             # a color per mode: status-line pill, cursor, current line number (NORMAL orange · INSERT green · SELECT purple)

[editor.cursor]                # default = exactly this (normal block · insert bar · select underline)
normal = "block"              # "block" | "bar" | "underline"
insert = "bar"
select = "underline"

[llm]
command = "claude"            # a CLI speaking the stream-json protocol (default claude -p, with speed flags)
model = "haiku"               # "" = the CLI's default model
context-lines = 20            # lines before and after the selection to send along

[keys.normal]
"A-," = "goto_previous_buffer"
C-l = ":sh zellij run -c -f -- lazygit"
```

## Plugins — on hold (user decision, 2026-09-27)

Built once (commit `601325e`), then taken out. Why: there's no one to use it yet (it's a personal editor — putting what you want straight into the core is
faster), chores are covered by combining keymap commands, `:sh`, and Claude Code in a side pane (M5), and an unused API only leaves the cost of binary size (+1 MB), threads, and ABI
maintenance. If it's ever needed, reverting that commit brings it back. Notes for that day:

- **Design**: WASM, sandboxed (no WASI, no host functions — it just receives JSON and returns a list of things to do), execution time capped with fuel,
  a plugin thread, reload on folder watch (`tarae plugin dev`), API surface = command registry + transactions.
- **Runtime measurements** (empty host, same release/LTO/strip settings) — if we do it again, wasmi:

  | | Binary | Load + instantiate | 50M-iteration loop |
  |---|---|---|---|
  | wasmi 2 | 1.8 MB | 0.12 ms | 213 ms |
  | wasmtime 49 (cranelift, no components) | 5.8 MB | 0.6 ms | 62 ms |

- **Pitfalls**: wasmi has no component model (WIT only as a contract document; transport is JSON) · wasmi's tail-call dispatch grows the stack
  on every instruction in `debug_assertions` builds (turn it off for that crate only) · detect fuel exhaustion by the trap code.

## Roadmap

| Stage | Scope |
|---|---|
| **M0** ✅ | Core model (rope, multiple selections, transactions, snapshot undo), basic movement/editing, multiple buffers, Helix key config, `:sh` |
| **M1** ✅ | Event loop rebuild · ✅ own config schema, live reload, `:set`/`:set!`, `:toggle`, error line numbers · ✅ performance budget tests · ✅ background loading of large files · ✅ search `/ ? n N *` · ✅ `s S A-s K A-K` · ✅ `f t F T r > < ~ J`, registers `"` and clipboard, macros `Q q` · grapheme clusters · sticky column |
| **M2** ✅ daily-driver baseline | ✅ positions = bytes · ✅ tree-sitter highlighting (lazy grammar loading, background incremental parsing) · ✅ themes (meok, hanji) · ✅ cursorline · ✅ `m` mode (text objects, matching, surround) · ✅ pickers `space f/b` · ✅ global search `space /`. · ✅ status line overhaul (git, claude, diagnostics slots) · ✅ picker preview |
| M3 LSP | ✅ Phase 1: non-blocking client (per-server threads, UTF-8 negotiation, incremental sync), diagnostics (display, underline, navigation, list), `gd` `gr` hover, progress display, discarding late responses. ✅ Part of phase 2: code actions, rename, format, applying server edits, completion (docs window, resolve), Markdown docs window, signature help, inlay hints = **M3 done** (+ code action diff preview, file create/move/delete edits) |
| M4 LLM | ✅ Select → instruct → diff review, streaming preview, cancel = kill the process, chat panel (many turns in one process, file/selection/diagnostics context, stop, apply code) |
| **M5 Agent** ✅ | Claude Code IDE protocol (MCP over WebSocket — chosen over ACP after research: it attaches with zero config to the `claude` users already run): lock-file advertisement, token auth, 11 tools, selection notifications, `@` mentions, edits = in-buffer y/n review (including partial accept), `space c` to launch in a side pane. ACP (hosting agents inside tarae) is a later candidate |
| ~~M6 Plugins~~ on hold | Built once and taken out — notes in the "Plugins" section above, code in commit `601325e` |
| **Experience** ✅ | meok/hanji themes (automatic background detection) · which-key cards · command palette · start screen · mouse · indent guides · scrollbar · notifications · git gutter · search highlighting and count · session restore · persistent undo · `:tutor` · theme preview · automatic disk sync and auto-save · rendered doc comments · split windows · symbol pickers · Error Lens · **debugger (DAP — Rust, C, C++, Go, Python, Java; conditional breakpoints, logpoints, watch expressions)** · code action diff preview · syntax injections |
| Later | Soft wrap |

Non-goals: a GUI, vim keymap emulation, copying Helix behavior unconditionally.

## Structure

```
src/
  editor.rs       editor state + key dispatch + integration tests
  term.rs         crossterm rendering, event loop
  selection.rs    Range/Selection — the selection model
  transaction.rs  Change/Transaction — multi-selection edits + position mapping
  document.rs     Document — rope + selection + path + snapshot undo
  movement.rs     pure movement functions (text, range) → range, visual column
  graphemes.rs    grapheme cluster boundaries (over rope chunks), screen cell width
  commands.rs     static command registry (Helix command names)
  typed.rs        `:` commands
  keymap.rs       keymap trie + TOML parser/merge
  default_keys.toml  default keymap
  settings.rs     settings schema table (path, kind, description, get/set) — shared by :set, :config-show, and file parsing
  config.rs       config file layers, watching, :set! writes
  search.rs       search and regex selection (regex — only the Unicode features Hangul needs)
  clipboard.rs    + register = system clipboard (worker thread; in-memory in tests)
  event.rs        event queue + Jobs (slow work on threads, results back to the main loop)
  llm.rs          claude -p prewarmed process, prompt, response parsing, diff review
  runtime.rs      runtime (embedded queries + ~/.local/share/tarae)
  grammar.rs      grammar download and build (tarae grammar install, the prompt shown when opening a file)
  syntax.rs       language detection, grammar dlopen, queries (inherits), background parsing, highlight ranges
  languages.toml  language table (name, grammar, extensions, file names)
  theme.rs        themes (built-in meok, hanji, and transparent variants; TOML — palette, inherits, warnings)
  textobject.rs   text objects, matching brackets, surround lookup (character-based + tree-sitter)
  picker.rs       picker (nucleo fuzzy), file list (git ls-files/walk), global search (rg/built-in)
  git.rs          branch (reads and watches HEAD directly — no processes)
  lsp.rs          LSP client (process, threads, framing, position conversion, server candidates)
  lsp_editor.rs   editor ↔ LSP (attach, didChange, message handling, gd/gr/hover, diagnostic navigation, completion request/apply)
  completion.rs   completion state, fuzzy filtering, snippet expansion
  chat.rs         chat panel (one process, context, keys, applying code)
  signature.rs    signature help response parsing (current parameter range — UTF-16 offsets and strings)
  markdown.rs     Markdown → colored lines (code blocks via tree-sitter) + wrapping to width
scripts/
  screenshot.py   renders the real screen to PNG in a pyte virtual terminal (docs/screenshots)
xtask/            dev tasks (cargo xtask — not included in the user binary)
  vendor-grammars bundled grammar C sources → runtime/grammars.tar.gz (when grammar commits change)
```

## License

[MPL-2.0](LICENSE) — the same license as Helix. The query files in `runtime/queries/` come from Helix
(MPL-2.0, see [NOTICE](runtime/queries/NOTICE)); grammar sources in `runtime/grammars.tar.gz` keep their upstream licenses.
