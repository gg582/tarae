<!-- Generated from src/settings.rs — do not edit; regenerate with `TARAE_BLESS=1 cargo test settings_reference`. -->
# Settings reference

Every setting tarae reads from `~/.config/tarae/config.toml` (or a project `.tarae.toml`).
The name is also the `:set` path: `:set editor.scrolloff 8` changes it for this session,
`:set!` also writes it to your config file. How files and layers work: [configuration guide](../configuration.md).

## Top level

| Setting | Type | Default | Description |
|---|---|---|---|
| `theme` | string | `"default"` | Theme name — built-in meok, hanji, meok-transparent, hanji-transparent, or a file in ~/.config/tarae/themes; "default" = meok/hanji by terminal background |

## `[editor]`

| Setting | Type | Default | Description |
|---|---|---|---|
| `editor.line-numbers` | `"absolute"` · `"relative"` | `"relative"` | Gutter line numbers |
| `editor.scrolloff` | integer 0–100 | `5` | Lines kept visible above/below the cursor |
| `editor.tab-width` | integer 1–16 | `4` | Display width of a tab, and spaces inserted by &lt;tab> |
| `editor.color-modes` | bool | `true` | Color the mode indicator by mode |
| `editor.cursorline` | bool | `true` | Highlight the line of the primary cursor (theme: ui.cursorline.primary) |
| `editor.header` | bool | `true` | Top bar: path › definition under the cursor, and open buffers |
| `editor.inlay-hints` | bool | `true` | Show language server inlay hints (types, parameter names) as dim virtual text |
| `editor.auto-save` | `"off"` · `"focus"` · `"idle"` | `"focus"` | Save modified files automatically: when the terminal loses focus, or also after 2 s idle |
| `editor.persistent-undo` | bool | `true` | Keep undo history across restarts (saved next to your state, dropped if the file changed elsewhere) |
| `editor.offer-grammars` | bool | `true` | When a file's language has no syntax grammar yet, offer to download and build it (y/n) |
| `editor.render-doc-comments` | bool | `true` | Show doc comments (///, //!, /** */) rendered as Markdown; raw when the cursor enters the block |
| `editor.restore-session` | bool | `true` | Reopen the files you had open in this folder (with cursor positions) when started without files |
| `editor.inline-diagnostics` | bool | `true` | Show diagnostic messages at the end of their line and tint error/warning lines |
| `editor.cursor-diagnostics` | bool | `true` | Cursor on an underlined problem whose message doesn't fit at the line end: show it in full in a card (Esc hides it) |
| `editor.soft-wrap` | `"prose"` · `"always"` · `"never"` | `"prose"` | Show long lines as several rows: in prose (Markdown, commit messages, plain text), always, or never |
| `editor.format-on-save` | bool | `true` | :w formats with the language server first (saves as is if it can't within 2 s) |
| `editor.git-blame` | bool | `true` | Show who last changed the cursor line, when, and why at its end (space g b flips it) |
| `editor.auto-pairs` | bool | `true` | Typing ( [ { " ' ` adds the closer; typing the closer steps over it; backspace removes both |
| `editor.indent-guides` | bool | `true` | Faint vertical guides in leading indentation (theme: ui.virtual.indent-guide) |
| `editor.scrollbar` | bool | `true` | Scrollbar on the right edge, with error/warning marks for the whole file |
| `editor.lsp` | bool | `true` | Start language servers (servers: [lsp.&lt;name>] command/args, per language: [lang.&lt;lang>] lsp = [...]) |

## `[editor.cursor]`

| Setting | Type | Default | Description |
|---|---|---|---|
| `editor.cursor.normal` | `"block"` · `"bar"` · `"underline"` | `"block"` | Cursor shape in normal mode |
| `editor.cursor.insert` | `"block"` · `"bar"` · `"underline"` | `"bar"` | Cursor shape in insert mode |
| `editor.cursor.select` | `"block"` · `"bar"` · `"underline"` | `"underline"` | Cursor shape in select mode |

## `[llm]`

| Setting | Type | Default | Description |
|---|---|---|---|
| `llm.claude-code` | bool | `true` | Let Claude Code (run `claude` → /ide, or space c) connect: it sees your selection and diagnostics, and its edits come here as a y/n diff (restart to apply) |
| `llm.command` | string | `"claude"` | LLM CLI speaking claude's stream-json protocol |
| `llm.args` | list of strings | `["-p", "--input-format", "stream-json", "--output-format", "stream-json", "--verbose", "--include-partial-messages", "--tools", "", "--no-session-persistence", "--strict-mcp-config", "--setting-sources", ""]` | Arguments for llm.command (default: claude -p with speed flags) |
| `llm.model` | string | `""` | Model for llm.command, e.g. "haiku" ("" = the CLI's default) |
| `llm.follow` | bool | `false` | Chat: the editor follows the code Claude is reading, with its thoughts beside it (C-f toggles) |
| `llm.context-lines` | integer 0–1000 | `20` | Lines of context sent around each selection |

## Tables you name yourself

These are read alongside the settings above; see the [configuration guide](../configuration.md).

| Table | What it holds |
|---|---|
| `[keys.normal]` · `[keys.select]` · `[keys.insert]` | Key bindings on top of the [default keymap](keymap.md) |
| `[lsp.<name>]` | A language server: `command`, `args` |
| `[lang.<language>]` | Per language: `lsp = ["name", …]` — which servers to try, in order |
| `[[attach]]` | A named debugger attach target: `name`, `lang`, `host`, `port` or `pid`, `before`, `remote-root`, `program` |
