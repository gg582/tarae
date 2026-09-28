# tarae (타래)

> *Tarae* is a skein of thread — wound on from the feel of Helix, spun from fresh yarn.

<p align="center">
  <img src="docs/screenshots/hero-meok.png" width="49%" alt="tarae in meok (ink): ink-dark background, coral accent — completion card, docs window, path bar, mode pill">
  <img src="docs/screenshots/hero-hanji.png" width="49%" alt="tarae in hanji (paper): warm paper background, vermilion accent — picked automatically on light terminals">
</p>

tarae is a terminal editor that keeps **Helix's keymap, its selection → action grammar, and its batteries-included spirit** —
and rebuilds everything else to fix what bothered us about Helix. It is not a fork.

| Helix pain point | tarae |
|---|---|
| Settings are a chore to change | One TOML file, applied on save; `:set` at runtime with descriptions and completion |
| Slow when a language server attaches | Keystrokes and rendering never wait — LSP, parsing, disk, and git all run in the background |
| No LLM support | Select → instruct → diff review, a chat panel, and Claude Code integration, built in |
| Immature plugins | Features go straight into the core (plugins are [on hold](#plugins--on-hold)) |

## Quick start

```sh
git clone https://github.com/eth219/tarae && cd tarae
cargo install --path .                               # Rust 1.88+ and a C compiler
# cargo install --path . --features bundled-grammars # or: 26 common grammars built in, nothing to download later
tarae path/to/file
```

No need to learn the keys first:

- **`space ?`** — command palette: find any command by what it does, with its key on the right ([screenshot](docs/screenshots/ux-palette.png))
- **which-key** — press `space`, `g`, `m`, `[`, or `]` and a card shows what comes next ([screenshot](docs/screenshots/ux-which-key.png))
- **`:tutor`** — a 10-minute hands-on tutorial in a practice buffer ([screenshot](docs/screenshots/ux-tutor.png))
- **Start screen** — launched without a file, it shows first steps and recent files ([screenshot](docs/screenshots/ux-welcome.png))

Rust, TOML, and Markdown are highlighted out of the box. Open a file in any other language and tarae offers to download its grammar — press `y`.

## Highlights

- **Never makes you wait.** Worst key → frame on a 200k-line file: 1.0 ms debug, 52 µs release (budget 16 ms, enforced by a test).
  A 100 MB file shows its first screen in 18 ms.
- **Claude as a verb.** Select, `space i`, type an instruction — the answer streams in and lands as an in-buffer diff you accept change by change.
- **Claude Code knows your editor.** The `claude` in a side pane sees your selection and diagnostics, and its edits arrive in tarae for review.
- **Language tooling included.** LSP (completion, signature help, inlay hints, code actions with a diff preview, rename),
  plus a test runner and debugger for Rust, Go, Python, and Java — including attaching to remote programs.
- **Made to be looked at.** Two themes, meok (ink) and hanji (paper), picked from your terminal's background. Everything floating is a card
  that speaks the same color language as the code.

## Features

### Editing — Helix keys

Same keys, same command names, same selection model (`w` *selects* a word, `d` deletes it), so Helix key configs carry over.

- Normal / insert / select modes, count prefixes (`3w`, `2gg`), `:` command line
- Movement `h j k l` `w b e` `gg ge gh gl gs G` `C-f C-b C-d C-u` · character find `f t F T` · `A-.` repeats the last motion
- Selection `x`/`X` (lines down/up), `A-x`, `%`, `;`, `A-;`, `,` · multiple cursors `C`/`A-C`
- Regex selection `s` (select matches within), `S` (split on matches), `A-s` (split into lines), `K`/`A-K` (keep/remove)
- Editing `i a I A o O` `d c y p P` `A-d A-c` `r` `~` `` ` `` `` A-` `` `> <` `J` · `u U` — a whole insert session is one undo step
- `m` mode — `mm` matching bracket; `mi`/`ma` + `w W p ( [ { < " ' `` ` `` or tree-sitter objects `f t a c T`
  (function, type, argument, comment, test — repeat to widen); `ms` `mr` `md` surround
- Registers `"x`, `_`, and `+` = system clipboard (`space y/p/P`) · macros `Q` record, `q` replay
- Grapheme-cluster aware everywhere (`é`, NFD Hangul, 👍🏽, 👨‍👩‍👧, `\r\n`); vertical movement remembers the visual column

### Look and feel

- **Themes** — meok (ink) and hanji (paper), plus `-transparent` variants that let the terminal's background and blur show through.
  The default `theme = "default"` asks the terminal for its background color (OSC 11 — never waits) and picks one;
  force it with `TARAE_BACKGROUND=light|dark`. `space t` previews themes live on your code
  ([screenshot](docs/screenshots/ux-theme-picker.png))
- **Cards** — completion, docs, hover, and pickers float as cards with half-line padding and an accent bar `▎` on the selected row.
  Themes without a background color get rounded borders instead
- **Path bar** — `project › folder › file ● › ƒ enclosing_fn`, from the tree-sitter tree
- **Status line** — mode pill · git branch and `+3 −2` · Claude status · `● errors ▲ warnings` · language and encoding · position.
  The least important items fold away first when the terminal is narrow
- **Toasts** — status messages fade in at the top right, the left bar colored by kind ([screenshot](docs/screenshots/ux-toasts-sync.png))
- **Rendered doc comments** — `///`, `//!`, and `/** */` blocks show as rewrapped prose with headings, lists, and highlighted code.
  `j`/`k` step over a block as one line; it unfolds into source when you edit or select it
  ([folded](docs/screenshots/ux-doc-comments.png) · [unfolded](docs/screenshots/ux-doc-comments-open.png))
- Indent guides, a scrollbar with error and warning ticks, current-line highlight, per-mode colors and cursor shapes
  ([screenshot](docs/screenshots/ux-guides.png))

### Getting around

- **Pickers** (nucleo fuzzy matching) — `space f` files (respects `.gitignore`), `space b` buffers, `space /` global search
  (ripgrep when installed), with a syntax-highlighted preview on wide terminals ([screenshot](docs/screenshots/m2-file-picker.png))
- **Search** `/ ? n N *` — matches highlight as you type, with a `8/13` count in the status line ([screenshot](docs/screenshots/ux-search.png))
- **Splits** `C-w v` `C-w s` · `C-w h j k l` · `C-w q` · `C-w o` — each pane has a title; unfocused panes dim
  ([screenshot](docs/screenshots/ux-splits.png))
- **`:` completion** — commands, file paths, themes, setting paths and values; `Tab` cycles, `→` takes the dimmed suggestion
  ([screenshot](docs/screenshots/ux-cmdline-complete.png))
- Buffers `gn` `gp` with a bufferline · mouse: click, drag to select, alt+click for another cursor, wheel

### Files and git

- **Disk sync** — files changed elsewhere (git checkout, a formatter, an agent) reload within 0.5 s — only the changed ranges, with cursor
  and undo kept. If you've edited them too, you get a conflict notice: `:reload` takes the disk version, `:w!` keeps yours
- **Auto-save** `editor.auto-save` — `"focus"` (default, when the terminal loses focus), `"idle"`, or `"off"`
- **Session restore** — launched without arguments, tarae reopens this folder's files where you left them
- **Persistent undo** — `u`/`U` keep working after a restart (dropped if the file changed outside)
- **Git gutter** — added, modified, and deleted lines against HEAD, scrollbar ticks, `]g` `[g` between hunks
  ([screenshot](docs/screenshots/ux-git.png))
- Large files (≥ 1 MB) load in the background — the first screen appears before the file is fully read

### Syntax highlighting

Tree-sitter, parsed incrementally in the background; queries run only on visible lines. Grammars are built from their upstream C
sources at the commits pinned in [`src/languages.toml`](src/languages.toml) — 54 languages, including C, C++, Java, Python, JS/TS, Go,
Rust, Bash, SQL, Lua, Zig, YAML, Terraform, Dockerfile, and Helm templates ([screenshot](docs/screenshots/syntax-helm.png)).

- The default build embeds Rust, TOML, Markdown, and comment (5.7 MB binary). Other languages are **offered on first open** —
  `y` downloads and builds the grammar (needs git and a C compiler) and colors switch on in place
  ([screenshot](docs/screenshots/grammar-offer.png)). Several offers stack ([screenshot](docs/screenshots/offer-stack.png))
- `--features bundled-grammars` embeds 26 common grammars instead (9.8 MB binary, no network needed)
- `tarae grammar list` · `tarae grammar install [lang…]` · `:grammar-install [lang|all]` · turn the offers off with `editor.offer-grammars = false`
- Injections — code blocks in Markdown, `TODO`/`NOTE` in comments, Go templates inside Helm YAML
  ([screenshot](docs/screenshots/syntax-injections.png))

### Language servers

Opening a file starts the first matching server on `PATH` — rust-analyzer, gopls, pyright, typescript-language-server, clangd, jdtls,
marksman, taplo, and more (configure with `[lsp.<name>]` and `[lang.<language>]`). Each server gets its own reader and writer threads,
so the editor never waits on one.

- **Diagnostics** — `●` in the gutter, underlines, messages at the end of the line ([screenshot](docs/screenshots/ux-error-lens.png)),
  `]d` `[d`, and a `space d` list
- **Completion** — filtered locally as you type, with snippets, auto-imports, and docs alongside ([screenshot](docs/screenshots/m3-completion.png))
- **Signature help** above the cursor line ([screenshot](docs/screenshots/m3-signature.png)) · **inlay hints** ([screenshot](docs/screenshots/m3-inlay-hints.png))
- **Code actions** `space a` — see a diff of what will change before you pick ([screenshot](docs/screenshots/m3-code-action-preview.png)) ·
  `space r` rename across files · `:format`
- `gd` definition · `gr` references · `space k` hover ([screenshot](docs/screenshots/m3-hover.png)) ·
  `space s`/`space S` symbols ([file](docs/screenshots/ux-symbols.png) · [workspace](docs/screenshots/ux-workspace-symbols.png))

**Java** — put [jdtls](https://download.eclipse.org/jdtls/snapshots/jdt-language-server-latest.tar.gz) (Java 21+) on `PATH`.
Multi-module Maven and Gradle projects share one server rooted at the topmost build file. `gd` into libraries and the JDK opens their
source, or the decompiled class, read-only ([screenshot](docs/screenshots/java-jdt-source.png)). Debugging needs the java-debug extension —
`F5` offers to download it and loads it without restarting jdtls ([screenshot](docs/screenshots/java-debug-offer.png)).

### Tests

`space x`, then: `x` the test at the cursor · `d` debug it · `f` this file's tests · `l` rerun the last · `c` close the panel.

| Language | Runs with |
|---|---|
| Rust | `cargo test -- path::name --exact` |
| Go | `go test -v -run '^TestX$'` (benchmarks with `-bench`) |
| Python | `pytest file::Class::function`, with the project's `.venv` or `$VIRTUAL_ENV` |
| Java | Gradle `--tests` or Maven `-Dtest=`, per module — JUnit 4/5 and TestNG |

Tests are found on the tree-sitter tree. If the grammar is missing, one `y` downloads it and runs the test
([screenshot](docs/screenshots/grammar-needed.png)). Results fill a panel — `●` passed, `▲` failed, `◦` skipped — with the failure message
on the right and a red `▲` at the end of the failing line; `]t` `[t` jump between failures
([Rust](docs/screenshots/test-results-rust.png) · [Go](docs/screenshots/test-results-go.png) ·
[Python](docs/screenshots/test-results-python.png) · [Java](docs/screenshots/test-results-java.png) ·
[run output](docs/screenshots/test-run.png) · [Java build output](docs/screenshots/test-run-java.png)).

### Debugger

DAP for Rust/C/C++ (lldb-dap), Go (dlv), Python (debugpy), and Java (java-debug inside jdtls). Everything lives under `space g`.

- `F9` breakpoint · `F5` start/continue · `F10` step over · `F11` step in · `F12` step out · `space g t` stop
- When paused: variable values at the end of the line, and a panel with variables, call stack, and program output
  ([Rust](docs/screenshots/ux-debugger.png) · [Python](docs/screenshots/ux-debugger-python.png) ·
  [Go](docs/screenshots/ux-debugger-go.png) · [Java](docs/screenshots/ux-debugger-java.png))
- Conditional breakpoints `space g C-c` · logpoints `space g C-l` · watch expressions `space g w`
  ([screenshot](docs/screenshots/ux-debugger-conditions.png))
- Debug a single test with `space x d` ([screenshot](docs/screenshots/test-debug.png) · [Java](docs/screenshots/test-debug-java.png))
- **Attach** to a running program with `space g a` or `:attach host:port` — debugpy `--listen`, dlv `--headless`, a JVM with JDWP,
  gdbserver / lldb-server, or a local pid. `space g t` detaches and leaves it running. Name the targets you use often:

  ```toml
  [[attach]]
  name = "api (k8s)"
  lang = "java"                                                # defaults to the current file's language
  port = 5005                                                  # host defaults to 127.0.0.1 · or pid = …
  before = "kubectl -n app port-forward deploy/api 5005:5005"  # started first, stopped with the session
  remote-root = "/app"                                         # source root in the container ↔ this project
  ```

  ([Python](docs/screenshots/attach-python.png) · [Go](docs/screenshots/attach-go.png) ·
  [Java](docs/screenshots/attach-java.png) · [Rust](docs/screenshots/attach-rust.png))

On macOS, lldb needs a one-time `sudo DevToolsSecurity -enable`. For Go, dlv must be newer than the installed Go.

### Claude

tarae runs `claude -p` as a subprocess — no API key or SDK in the editor; it uses your Claude Code login.

![Chat panel — ask with the selection as context; code in answers sits in a well in its language's colors](docs/screenshots/m4-chat.png)

- **Select → instruct → diff** — select, `space i` (or `:ask`), type an instruction. Claude answers for each selection; the answer streams
  in while you keep editing, then opens as an **in-buffer review**: `y` accept · `n` reject · `a` all · `tab` next. One `u` undoes it all,
  and `:ask-cancel` kills the process ([screenshot](docs/screenshots/m4-review.png)).
  A process is prewarmed the moment the prompt opens, so Enter → diff takes about 3.4 s instead of 8–10 s
- **Chat panel** `space l` — one process per conversation, with the current file, selection, and diagnostics attached as context
  (shown as chips). `C-r` replaces the selection with the answer's code through the same review, `C-c` stops, `C-l` starts over
  ([hanji](docs/screenshots/m4-chat-hanji.png))

#### Claude Code

tarae speaks Claude Code's IDE protocol — the same one the VS Code and JetBrains extensions use — so there's nothing to set up.
`space c` launches `claude` in a zellij or tmux side pane, already connected; a `claude` you already have running connects with `/ide`.

![Edits from Claude Code in a side pane arrive as a review inside the tarae buffer — y/n/a](docs/screenshots/m5-agent-review.png)

- Claude sees your selection, open files and unsaved edits, and diagnostics — "fix this error" just works
- `space C` sends the selected lines to Claude's input as `@file#L10-20`
- Claude's edits open in the in-buffer review; accept part of one and Claude knows what you kept
- Listens on `127.0.0.1` only, behind a per-session token ([connected](docs/screenshots/m5-agent-connected.png)).
  Turn it off with `llm.claude-code = false`

## Configuration

`~/.config/tarae/config.toml` (or `$XDG_CONFIG_HOME/tarae/config.toml`), plus an optional project `.tarae.toml` found upward from the
current directory. It's TOML with tarae's own schema — Helix config files aren't read; only the key notation (`"A-x"`) and command names
are shared.

- **One file** for editor, keys, languages, language servers, and Claude
- **Applied on save**, no restart. Mistakes don't block startup; they're reported as `file:line: message`
- **Setting name = `:set` path**
  - `:set editor.scrolloff` — current value, description, and allowed values
  - `:set editor.scrolloff 8` — this session only · `:set! editor.scrolloff 8` — also write it to the file, comments preserved
  - `:toggle editor.color-modes` — flip a bool or cycle a choice
  - `:config-show` — the full effective configuration with descriptions, usable as a config file · `:config-open` · `:config-reload`
- **Layers**: defaults < user config < project `.tarae.toml` < `:set`

```toml
theme = "default"            # meok · hanji · meok-transparent · hanji-transparent · default (follows the terminal)

[editor]
line-numbers = "relative"
scrolloff = 5
color-modes = true           # color the mode pill, cursor, and line number per mode
auto-save = "focus"          # "focus" · "idle" · "off"

[editor.cursor]
normal = "block"
insert = "bar"
select = "underline"

[llm]
model = "haiku"              # "" = the CLI's default model
context-lines = 20           # lines around the selection sent along

[keys.normal]
"A-," = "goto_previous_buffer"
"A-." = "goto_next_buffer"
C-l = ":sh zellij run -c -f -- lazygit"
```

**Custom themes** live in `~/.config/tarae/themes/<name>.toml`:

```toml
inherits = "meok"                      # build on a built-in theme or one of your own
"keyword" = "accent"
"comment" = { fg = "dim", italic = true }
"diagnostic.error" = { underline = { color = "rose", style = "curl" } }

[palette]
accent = "#ef8a5a"
link = "accent"                        # palette entries can point to each other
```

Colors are `#rrggbb`, `#rgb`, ANSI names, or palette names; modifiers are `bold italic underline dim reversed crossed`. Typos are
reported, not silently ignored. tarae-only keys: `ui.accent` (the single accent color) and `ui.tint` (the base faint colors blend into).

## Principles

1. **Your hands are in Helix.** Keymap, selection → action grammar, and command names match Helix.
2. **Batteries included.** Needed features go into the core, kept small.
3. **Configuration is data.** No code in config files. Changes apply on save, or at runtime with `:set`.
4. **Never make you wait.** Keystrokes and screen updates never block on LSP, the LLM, disk, or parsing. Slow work runs in the background,
   late results are discarded, and performance budgets are tests.
5. **The LLM is a first-class verb.** In a selection-first editor, the LLM takes the action slot — instruct, review the diff, accept.
6. **Makes you want to try it at a glance.** Design weighs as much as features. Floating things speak the same color language as the code,
   breathe with whitespace, and fold by priority when space runs out.
7. **Small and testable.** The core doesn't know about the terminal; it's tested whole by feeding it key sequences.

## Roadmap

| Stage | Scope |
|---|---|
| **M0** ✅ | Core model (rope, multiple selections, transactions, snapshot undo), movement and editing, buffers, `:sh` |
| **M1** ✅ | Non-blocking event loop, config schema and live reload, performance budget tests, background loading, search and regex selection, registers, macros |
| **M2** ✅ | Byte positions, tree-sitter highlighting, themes, `m` mode, pickers, global search, status line |
| **M3** ✅ | LSP — diagnostics, navigation, hover, completion, signature help, inlay hints, code actions with preview, rename, format |
| **M4** ✅ | Claude — select → instruct → diff, streaming answers, chat panel |
| **M5** ✅ | Claude Code IDE protocol — selection sharing, diagnostics, in-buffer review of edits |
| **Experience** ✅ | which-key, palette, tutor, start screen, mouse, toasts, git gutter, session restore, persistent undo, disk sync, doc comments, splits, tests, debugger, attach |
| M6 plugins | On hold — see below |
| Next | Soft wrap, locals queries, ACP (hosting agents inside tarae) |

Non-goals: a GUI, vim emulation, copying Helix behavior for its own sake.

### Plugins — on hold

A WASM plugin system was built, then taken out before the first release: the core plus keymap commands, `:sh`, and Claude Code in a
side pane already covered what was needed, and an unused API only costs binary size (+1 MB), threads, and ABI upkeep.

Notes for if it comes back — sandboxed WASM (no WASI or host functions: JSON in, a list of actions out), capped with fuel, on its own
thread, reloaded when its folder changes; API = command registry + transactions. Runtime measurements (empty host, release/LTO/strip):

| | Binary | Load + instantiate | 50M-iteration loop |
|---|---|---|---|
| wasmi 2 | 1.8 MB | 0.12 ms | 213 ms |
| wasmtime 49 (cranelift, no components) | 5.8 MB | 0.6 ms | 62 ms |

wasmi would be the pick. Pitfalls: it has no component model (use JSON as the transport), its tail-call dispatch grows the stack on every
instruction in `debug_assertions` builds (turn that off for the crate), and fuel exhaustion is detected by trap code.

## Development

```sh
cargo test                               # key-sequence tests + performance budgets
cargo test --features bundled-grammars   # the bundled-grammar path
cargo clippy --all-targets
cargo xtask vendor-grammars              # repack runtime/grammars.tar.gz after changing bundled grammars
```

```
src/
  main.rs            entry point, `tarae grammar` subcommand
  editor.rs          editor state, key dispatch, key-sequence tests
  term.rs            rendering and the event loop — the only module that knows about the terminal
  event.rs           event queue + Jobs (slow work on threads, results back on the main loop)

  selection.rs       selection model (byte ranges)
  transaction.rs     multi-selection edits + position mapping — every edit goes through here
  document.rs        rope + selection + path + snapshot undo
  movement.rs        pure movement functions, visual column
  graphemes.rs       grapheme cluster boundaries, cell width
  textobject.rs      text objects, matching brackets, surround
  search.rs          search and regex selection
  split.rs           split-window tree

  key.rs keymap.rs   key notation, keymap trie + TOML parser
  default_keys.toml  the default keymap
  commands.rs        command registry (Helix command names)
  typed.rs           `:` commands
  cmdline.rs         `:` completion

  settings.rs        settings schema — one row per setting, shared by parsing, :set, and :config-show
  config.rs          config layers, watching, :set! writes
  theme.rs themes/   theme loading; built-in meok and hanji

  disk.rs            sync with changes on disk, auto-save
  session.rs         session restore
  recent.rs          recent files (start screen)
  undofile.rs        persistent undo
  clipboard.rs       + register = system clipboard
  git.rs             branch and changed-line hunks

  syntax.rs          language detection, grammar loading, background parsing, highlights, injections
  languages.toml     language table (grammars, file types)
  grammar.rs         grammar download and build
  runtime.rs         embedded queries and grammars
  offer.rs           download offer cards
  doccomment.rs      rendered doc comments
  markdown.rs        Markdown → colored lines (hover, docs, chat)
  picker.rs          fuzzy pickers, file listing, global search
  tutor.md           the :tutor text

  lsp.rs             LSP client (threads, framing, position encoding)
  lsp_editor.rs      editor ↔ LSP (didChange, requests, responses)
  completion.rs      completion state, filtering, snippets
  signature.rs       signature help
  editdiff.rs        workspace edit previews (code actions, rename)
  java.rs            jdtls and java-debug specifics

  testing.rs         test discovery and runs
  test_results.rs    test output parsing
  dap.rs             debugger (DAP) client
  attach.rs          attaching to running programs

  llm.rs             claude -p process, streaming, response parsing, review
  chat.rs            chat panel
  agent.rs ws.rs     Claude Code IDE protocol over an in-house WebSocket server
runtime/
  queries/           tree-sitter queries (from Helix 25.07.1, MPL-2.0)
  grammars.tar.gz    C sources of the bundled grammars
scripts/
  screenshot.py      renders the real screen to PNG in a virtual terminal (docs/screenshots)
  design_mockup.py   design direction mockups (docs/design)
xtask/               dev tasks (cargo xtask — not part of the tarae binary)
```

## License

[MPL-2.0](LICENSE), the same license as Helix. The query files in `runtime/queries/` come from Helix
(MPL-2.0, see [NOTICE](runtime/queries/NOTICE)); grammar sources in `runtime/grammars.tar.gz` keep their upstream licenses.
