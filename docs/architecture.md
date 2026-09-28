# Architecture

Why tarae is built the way it is, where it's going, and where things live in the source. For the working rules when
changing the code, see [CONTRIBUTING.md](../CONTRIBUTING.md).

- [Principles](#principles)
- [How it stays fast](#how-it-stays-fast)
- [Design language](#design-language)
- [Roadmap](#roadmap)
- [Plugins — on hold](#plugins--on-hold)
- [Source layout](#source-layout)

## Principles

1. **Your hands are in Helix.** Keymap, selection → action grammar, and command names match Helix, so key configs carry
   over. Everything else — architecture, UI, configuration, feature set — is tarae's own.
2. **Batteries included.** Needed features go into the core, kept small.
3. **Configuration is data.** No code in config files. Changes apply on save, or at runtime with `:set`, and a setting's
   name is its `:set` path. Bad settings are reported with line numbers instead of blocking startup.
4. **Never make you wait.** Keystrokes and screen updates never block on LSP, the LLM, disk, or parsing. Slow work runs
   in the background, late results are discarded, and performance budgets are tests.
5. **The LLM is a first-class verb.** In a selection-first editor, the LLM takes the action slot — instruct, review the
   diff, accept. Editor state is open to external agents such as Claude Code.
6. **Makes you want to try it at a glance.** Design weighs as much as features. Floating things speak the same color
   language as the code, breathe with whitespace, and fold by priority when space runs out.
7. **Small and testable.** The core doesn't know about the terminal; it's tested whole by feeding it key sequences
   (`"xd"`, `"Ci-<esc>"` …).

## How it stays fast

- **One event loop.** An input thread and a single event queue ([`src/event.rs`](../src/event.rs)); queued events are
  handled together and drawn once. Slow work goes through `Jobs::spawn` on a worker thread, and its result comes back
  as a closure applied on the main loop.
- **Language servers** get a writer thread (owns stdin) and a reader thread (owns stdout) each; the main thread only
  pushes onto channels — even serializing a whole document for `didOpen` happens on the writer thread. Changes go out
  as incremental `didChange` once per event batch. Positions are negotiated as UTF-8, so there's no conversion layer.
- **Parsing** runs on a worker thread and is incremental: edits apply to the existing tree immediately, and edits made
  during a parse are replayed onto the new tree. A full parse of a 20k-line Rust file takes 145 ms — in the background.
- **Positions are byte offsets** everywhere (tree-sitter, regex, and LSP all speak bytes), always on character
  boundaries; "one character" is a grapheme cluster.
- **Budgets are tests.** Worst key → frame on a 200k-line file: 1.0 ms in a debug build, 52 µs in release (budget
  16 ms). Worst key → frame with highlighting on: 1.1 ms (release). A 100 MB file shows its first screen in 18 ms and is
  fully loaded in 75 ms.

## Design language

tarae's look is a pair: **meok** (ink — a dark ink background with a coral accent) and **hanji** (paper — warm paper
with a vermilion accent). The two built-in themes have exactly the same keys.

- **One accent color** (`ui.accent`), used boldly and sparingly; syntax colors stay calm
- **Floating things are cards** — half-cell blocks give half a line of padding; themes without a background color get
  rounded borders instead. The selected row gets an accent bar `▎`; typed characters are accent bold
- **Two dim levels only** — `ui.virtual` (dim) and `ui.linenr` (faint)
- **Glyphs** only from the set every common monospace font has (JetBrains Mono, Hack, Menlo, SF Mono):
  `ƒ τ ν π § ◦ # ¶ ● ▲ › ▎ ▐ ▌ ▄ ▀`

Early mockups that set the direction: [ink](design/a-ink.png) · [paper](design/b-paper.png) ·
[aurora](design/c-aurora.png) (made with [`scripts/design_mockup.py`](../scripts/design_mockup.py)).

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

For the Claude Code integration, the IDE protocol (MCP over WebSocket) was chosen over ACP because it attaches with zero
configuration to the `claude` people already run. Hosting agents inside tarae via ACP remains a later candidate.

## Plugins — on hold

A WASM plugin system was built, then taken out before the first release: the core plus keymap commands, `:sh`, and
Claude Code in a side pane already covered what was needed, and an unused API only costs binary size (+1 MB), threads,
and ABI upkeep.

Notes for if it comes back — sandboxed WASM (no WASI or host functions: JSON in, a list of actions out), execution
capped with fuel, on its own thread, reloaded when its folder changes; API = command registry + transactions. Runtime
measurements (empty host, release/LTO/strip):

| | Binary | Load + instantiate | 50M-iteration loop |
|---|---|---|---|
| wasmi 2 | 1.8 MB | 0.12 ms | 213 ms |
| wasmtime 49 (cranelift, no components) | 5.8 MB | 0.6 ms | 62 ms |

wasmi would be the pick. Pitfalls: it has no component model (use JSON as the transport), its tail-call dispatch grows
the stack on every instruction in `debug_assertions` builds (turn that off for the crate), and fuel exhaustion is
detected by trap code.

## Source layout

```
src/
  main.rs            entry point, `tarae grammar` subcommand
  editor.rs          editor state, key dispatch, key-sequence tests
  term.rs            rendering and the event loop — the only module that knows about the terminal
  term_snapshots.rs  screen snapshot tests (golden files in snapshots/)
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
  cmdline.rs         `:` completion and the `:` command table

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
tests/
  e2e.rs             end-to-end tests — the real binary in a pseudo-terminal
docs/
  reference/         generated from the code — settings.md, keymap.md
scripts/
  screenshot.py      renders the real screen to PNG in a virtual terminal (docs/screenshots)
  design_mockup.py   design direction mockups (docs/design)
xtask/               dev tasks (cargo xtask — not part of the tarae binary)
```
