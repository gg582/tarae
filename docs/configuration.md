# Configuration

tarae is configured with one TOML file in tarae's own schema. Configuration is data — no code runs in it. Every
setting, with its type and default, is listed in the [settings reference](reference/settings.md); every default key in
the [keymap reference](reference/keymap.md).

- [Where it lives](#where-it-lives)
- [Changing settings while you work](#changing-settings-while-you-work)
- [Full example](#full-example)
- [Keys](#keys)
- [Language servers](#language-servers)
- [Custom themes](#custom-themes)

## Where it lives

- **User config**: `~/.config/tarae/config.toml` (or `$XDG_CONFIG_HOME/tarae/config.toml`). `:config-open` opens it
- **Project config**: `.tarae.toml`, found by searching upward from the current directory — handy for per-project
  debugger [attach targets](testing-and-debugging.md#attaching-to-a-running-program)
- **Layers**, later ones winning: defaults < user config < project `.tarae.toml` < `:set` at runtime

One file holds everything — editor, keys, languages, language servers, and Claude. There's no separate
`languages.toml`.

- **Applied on save** — no restart (`:config-reload` forces a reread)
- **Mistakes don't block startup** — a bad value or an unknown name is reported as `file:line: message`, and the rest
  of the file still applies
- **Helix config files aren't read.** Only the key notation (`"A-x"`) and command names are shared, so a Helix
  `[keys.normal]` table carries over as it is

## Changing settings while you work

The name of a setting is its `:set` path — the same dotted name as in the file.

| Command | Does |
|---|---|
| `:set editor.scrolloff` | Show the current value, its description, and the allowed values |
| `:set editor.scrolloff 8` | Change it for this session (it stays even if the config file is reloaded) |
| `:set! editor.scrolloff 8` | Change it and write it to your config file — comments and formatting are kept |
| `:toggle editor.color-modes` | Flip an on/off setting, or move a choice setting to its next value |
| `:config-show` | The full effective configuration, with descriptions as comments, in a new buffer — usable as a config file as it is |
| `:config-open` · `:config-reload` | Open your config file · reread it |

`Tab` completes setting paths and, for on/off and choice settings, their values. `space t` picks a theme with a live
preview and saves your choice.

## Full example

```toml
theme = "default"            # meok · hanji · meok-transparent · hanji-transparent · default (follows the terminal)

[editor]
line-numbers = "relative"
scrolloff = 5
tab-width = 4
color-modes = true           # color the mode pill, cursor, and line number per mode
auto-save = "focus"          # "focus" · "idle" · "off"

[editor.cursor]
normal = "block"             # "block" · "bar" · "underline"
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

## Keys

Bindings go in `[keys.normal]`, `[keys.select]`, and `[keys.insert]` and are merged on top of the
[default keymap](reference/keymap.md). The notation and command names are Helix's:

```toml
[keys.normal]
X = ["extend_line_up", "extend_to_line_bounds"]  # a list runs in order
"A-w" = ":buffer-close"                          # a leading ':' runs a : command
C-k = ":sh tmux split-window -h"                 # … including shell commands

[keys.normal.space]
e = "file_picker"                                # tables are sub-menus (space e)
```

- Keys: `a`, `A` (shift), `C-x` (ctrl), `A-x` (alt), and names like `space`, `ret`, `esc`, `tab`, `backspace`, `del`,
  `left`, `pageup`, `home`, `F5`
- Sub-menus merge key by key — adding `space e` keeps the rest of `space`
- Like Helix, `[keys.normal]` doesn't spill into select mode; bind a key in `[keys.select]` too if you want it there
- `space ?` lists every command with a description, so you can find a name to bind

## Language servers

tarae tries a built-in list of servers per language and uses the first one on your `PATH` (the list:
[Language support](languages.md)). Define a server with `[lsp.<name>]` and pick servers per language with
`[lang.<language>]`:

```toml
[lsp.basedpyright]
command = "basedpyright-langserver"
args = ["--stdio"]

[lang.python]
lsp = ["basedpyright"]     # tried in order; the first one on PATH starts
```

`editor.lsp = false` turns language servers off entirely.

## Custom themes

Themes live in `~/.config/tarae/themes/<name>.toml`; select one with `theme = "<name>"`, `:theme <name>`, or
`space t`.

```toml
inherits = "meok"                      # build on a built-in theme or one of your own
"keyword" = "accent"                   # color only
"comment" = { fg = "dim", italic = true }
"diagnostic.error" = { underline = { color = "rose", style = "curl" } }

[palette]
accent = "#ef8a5a"
link = "accent"                        # palette entries can point to each other
```

- Colors: `#rrggbb`, `#rgb`, ANSI names (`red`, `light-blue`, …), or palette names
- Modifiers: `bold`, `italic`, `underline`, `dim`, `reversed`, `crossed` (a `modifiers = [...]` list works too)
- Typos in color names and unknown attributes are reported as warnings, not silently ignored
- tarae-only keys: `ui.accent` (the single accent color) and `ui.tint` (the base that faint colors blend into — this is
  what the transparent variants set instead of a background)
- Built-in themes: `meok`, `hanji`, `meok-transparent`, `hanji-transparent`; `default` picks meok or hanji to match the
  terminal background
