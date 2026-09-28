<!-- Generated from src/default_keys.toml and the command registry — do not edit; regenerate with `TARAE_BLESS=1 cargo test keymap_reference`. -->
# Default keymap

The keys tarae ships with, from [`src/default_keys.toml`](../../src/default_keys.toml). Command names are the same as Helix's,
so Helix key configs carry over. Rebind anything in `[keys.normal]`, `[keys.select]`, or `[keys.insert]` —
see the [configuration guide](../configuration.md#keys).

In the editor you don't need this page: `space ?` finds any command by what it does, and after a prefix key
(`space`, `g`, `m`, `[`, `]`) a card shows what comes next.

## Normal mode

| Key | Command | Description |
|---|---|---|
| `"` | `select_register` | Select register for the next command |
| `%` | `select_all` | Select whole document |
| `*` | `search_selection` | Search for selection |
| `,` | `keep_primary_selection` | Keep primary selection |
| `/` | `search` | Search for regex pattern |
| `:` | `command_mode` | Enter command mode |
| `;` | `collapse_selection` | Collapse selection into single cursor |
| `<` | `unindent` | Unindent selection |
| `>` | `indent` | Indent selection |
| `?` | `rsearch` | Reverse search for regex pattern |
| `[` | … | More keys — see `[` below |
| `]` | … | More keys — see `]` below |
| `` ` `` | `switch_to_lowercase` | Switch to lowercase |
| `a` | `append_mode` | Append after selection |
| `A` | `insert_at_line_end` | Insert at end of line |
| `b` | `move_prev_word_start` | Move to start of previous word |
| `c` | `change_selection` | Change selection |
| `C` | `copy_selection_on_next_line` | Copy selection on next line |
| `d` | `delete_selection` | Delete selection |
| `e` | `move_next_word_end` | Move to end of next word |
| `f` | `find_next_char` | Move to next occurrence of char |
| `F` | `find_prev_char` | Move to previous occurrence of char |
| `g` | … | More keys — see `g` below |
| `G` | `goto_line` | Go to last line (or line &lt;n>) |
| `h` | `move_char_left` | Move left |
| `i` | `insert_mode` | Insert before selection |
| `I` | `insert_at_line_start` | Insert at start of line |
| `j` | `move_visual_line_down` | Move down (visual line) |
| `J` | `join_selections` | Join lines inside selection |
| `k` | `move_visual_line_up` | Move up (visual line) |
| `K` | `keep_selections` | Keep selections matching the regex |
| `l` | `move_char_right` | Move right |
| `m` | … | More keys — see `m` below |
| `n` | `search_next` | Select next search match |
| `N` | `search_prev` | Select previous search match |
| `o` | `open_below` | Open new line below selection |
| `O` | `open_above` | Open new line above selection |
| `p` | `paste_after` | Paste after selection |
| `P` | `paste_before` | Paste before selection |
| `q` | `replay_macro` | Replay macro |
| `Q` | `record_macro` | Record macro |
| `r` | `replace` | Replace with new char |
| `s` | `select_regex` | Select regex matches |
| `S` | `split_selection` | Split selections on regex matches |
| `t` | `find_till_char` | Move till next occurrence of char |
| `T` | `till_prev_char` | Move till previous occurrence of char |
| `u` | `undo` | Undo change |
| `U` | `redo` | Redo change |
| `v` | `select_mode` | Enter selection extend mode |
| `w` | `move_next_word_start` | Move to start of next word |
| `x` | `extend_line_below` | Select line (again: extend down) |
| `X` | `extend_line_up` + `extend_to_line_bounds` | Extend up, then extend selection to line bounds |
| `y` | `yank` | Yank selection |
| `~` | `switch_case` | Switch (toggle) case |
| `A-.` | `repeat_last_motion` | Repeat last motion |
| `A-;` | `flip_selections` | Flip selection cursor and anchor |
| `` A-` `` | `switch_to_uppercase` | Switch to uppercase |
| `A-C` | `copy_selection_on_prev_line` | Copy selection on previous line |
| `A-c` | `change_selection_noyank` | Change selection without yanking |
| `A-d` | `delete_selection_noyank` | Delete selection without yanking |
| `A-K` | `remove_selections` | Remove selections matching the regex |
| `A-s` | `split_selection_on_newline` | Split selection on newlines |
| `A-x` | `extend_to_line_bounds` | Extend selection to line bounds |
| `C-b` | `page_up` | Move page up |
| `C-d` | `half_page_down` | Move half page down |
| `C-f` | `page_down` | Move page down |
| `C-i` | `jump_forward` | Jump forward again |
| `C-o` | `jump_backward` | Jump back to the previous spot |
| `C-s` | `save_selection` | Save this spot to jump back to |
| `C-u` | `half_page_up` | Move half page up |
| `C-w` | … | More keys — see `C-w` below |
| `down` | `move_visual_line_down` | Move down (visual line) |
| `end` | `goto_line_end` | Goto line end |
| `esc` | `normal_mode` | Enter normal mode |
| `F10` | `dap_next` | Step over |
| `F11` | `dap_step_in` | Step into |
| `F12` | `dap_step_out` | Step out |
| `F5` | `dap_launch` | Start debugging / continue |
| `F9` | `toggle_breakpoint` | Toggle breakpoint |
| `home` | `goto_line_start` | Goto line start |
| `left` | `move_char_left` | Move left |
| `pagedown` | `page_down` | Move page down |
| `pageup` | `page_up` | Move page up |
| `right` | `move_char_right` | Move right |
| `space` | … | More keys — see `space` below |
| `tab` | `jump_forward` | Jump forward again |
| `up` | `move_visual_line_up` | Move up (visual line) |

### `[`

| Key | Command | Description |
|---|---|---|
| `[ d` | `goto_prev_diag` | Goto previous diagnostic |
| `[ g` | `goto_prev_change` | Previous git change |
| `[ t` | `goto_prev_test_failure` | Previous failed test |

### `]`

| Key | Command | Description |
|---|---|---|
| `] d` | `goto_next_diag` | Goto next diagnostic |
| `] g` | `goto_next_change` | Next git change |
| `] t` | `goto_next_test_failure` | Next failed test |

### `g`

| Key | Command | Description |
|---|---|---|
| `g a` | `goto_last_accessed_file` | Goto last accessed file |
| `g d` | `goto_definition` | Goto definition (LSP) |
| `g D` | `goto_declaration` | Goto declaration (LSP) |
| `g e` | `goto_last_line` | Goto last line |
| `g g` | `goto_file_start` | Go to file start (or line &lt;n>) |
| `g h` | `goto_line_start` | Goto line start |
| `g i` | `goto_implementation` | Goto implementation (LSP) |
| `g l` | `goto_line_end` | Goto line end |
| `g n` | `goto_next_buffer` | Goto next buffer |
| `g p` | `goto_previous_buffer` | Goto previous buffer |
| `g r` | `goto_reference` | Goto references (LSP) |
| `g s` | `goto_first_nonwhitespace` | Goto first non-blank in line |
| `g y` | `goto_type_definition` | Goto type definition (LSP) |

### `m`

| Key | Command | Description |
|---|---|---|
| `m a` | `select_textobject_around` | Select around object |
| `m d` | `surround_delete` | Surround delete |
| `m i` | `select_textobject_inner` | Select inside object |
| `m m` | `match_brackets` | Goto matching bracket |
| `m r` | `surround_replace` | Surround replace |
| `m s` | `surround_add` | Surround add |

### `C-w`

| Key | Command | Description |
|---|---|---|
| `C-w h` | `jump_view_left` | Window to the left |
| `C-w j` | `jump_view_down` | Window below |
| `C-w k` | `jump_view_up` | Window above |
| `C-w l` | `jump_view_right` | Window to the right |
| `C-w o` | `wonly` | Close all other windows |
| `C-w q` | `wclose` | Close this window |
| `C-w s` | `hsplit` | Split the window top and bottom |
| `C-w v` | `vsplit` | Split the window side by side |
| `C-w w` | `rotate_view` | Next window |
| `C-w C-w` | `rotate_view` | Next window |

### `space`

| Key | Command | Description |
|---|---|---|
| `space /` | `global_search` | Search in project |
| `space ?` | `command_palette` | Find a command |
| `space a` | `code_action` | Code actions |
| `space b` | `buffer_picker` | Open buffer picker |
| `space c` | `claude_code` | Open Claude Code beside (connected) |
| `space C` | `claude_code_mention` | Send selection to Claude Code (@) |
| `space d` | `diagnostics_picker` | Diagnostics |
| `space f` | `file_picker` | Open file picker |
| `space g` | … | More keys — see `space g` below |
| `space i` | `llm_ask` | Ask Claude to edit selection |
| `space k` | `hover` | Show docs under cursor |
| `space l` | `chat_open` | Chat with Claude |
| `space L` | `chat_close` | Close chat |
| `space p` | `paste_clipboard_after` | Paste clipboard after |
| `space P` | `paste_clipboard_before` | Paste clipboard before |
| `space r` | `rename_symbol` | Rename symbol |
| `space s` | `symbol_picker` | Symbols in this file |
| `space S` | `workspace_symbol_picker` | Symbols in the workspace |
| `space t` | `theme_picker` | Choose a theme (live preview) |
| `space w` | … | More keys — see `space w` below |
| `space x` | … | More keys — see `space x` below |
| `space y` | `yank_to_clipboard` | Copy to clipboard |

### `space g`

| Key | Command | Description |
|---|---|---|
| `space g a` | `dap_attach` | Attach to a running program… |
| `space g b` | `toggle_breakpoint` | Toggle breakpoint |
| `space g c` | `dap_launch` | Start debugging / continue |
| `space g i` | `dap_step_in` | Step into |
| `space g l` | `dap_launch` | Start debugging / continue |
| `space g n` | `dap_next` | Step over |
| `space g o` | `dap_step_out` | Step out |
| `space g p` | `dap_pause` | Pause the program |
| `space g t` | `dap_terminate` | Stop debugging |
| `space g w` | `dap_watch` | Watch an expression |
| `space g W` | `dap_unwatch` | Remove a watch |
| `space g C-c` | `dap_edit_condition` | Break only when… (condition) |
| `space g C-l` | `dap_edit_log` | Log here instead of stopping |

### `space w`

| Key | Command | Description |
|---|---|---|
| `space w h` | `jump_view_left` | Window to the left |
| `space w j` | `jump_view_down` | Window below |
| `space w k` | `jump_view_up` | Window above |
| `space w l` | `jump_view_right` | Window to the right |
| `space w o` | `wonly` | Close all other windows |
| `space w q` | `wclose` | Close this window |
| `space w s` | `hsplit` | Split the window top and bottom |
| `space w v` | `vsplit` | Split the window side by side |
| `space w w` | `rotate_view` | Next window |
| `space w C-w` | `rotate_view` | Next window |

### `space x`

| Key | Command | Description |
|---|---|---|
| `space x c` | `test_close` | Close the test panel |
| `space x d` | `test_debug` | Debug the test at the cursor |
| `space x f` | `test_file` | Run this file's tests |
| `space x l` | `test_last` | Run the last test again |
| `space x x` | `test_nearest` | Run the test at the cursor |

## Select mode

`v` enters select mode. Every normal-mode key works here too, except these — movement extends the selection instead:

| Key | Command | Description |
|---|---|---|
| `b` | `extend_prev_word_start` | Extend to start of previous word |
| `e` | `extend_next_word_end` | Extend to end of next word |
| `h` | `extend_char_left` | Extend left |
| `j` | `extend_visual_line_down` | Extend down (visual line) |
| `k` | `extend_visual_line_up` | Extend up (visual line) |
| `l` | `extend_char_right` | Extend right |
| `v` | `normal_mode` | Enter normal mode |
| `w` | `extend_next_word_start` | Extend to start of next word |
| `down` | `extend_visual_line_down` | Extend down (visual line) |
| `left` | `extend_char_left` | Extend left |
| `right` | `extend_char_right` | Extend right |
| `up` | `extend_visual_line_up` | Extend up (visual line) |

## Insert mode

| Key | Command | Description |
|---|---|---|
| `backspace` | `delete_char_backward` | Delete previous char |
| `C-x` | `completion` | Invoke completion popup (LSP) |
| `del` | `delete_char_forward` | Delete next char |
| `down` | `move_visual_line_down` | Move down (visual line) |
| `esc` | `normal_mode` | Enter normal mode |
| `left` | `move_char_left` | Move left |
| `ret` | `insert_newline` | Insert newline char |
| `right` | `move_char_right` | Move right |
| `tab` | `insert_tab` | Insert tab char |
| `up` | `move_visual_line_up` | Move up (visual line) |

## `:` commands

Type `:` in normal mode. `Tab` completes command names and their arguments (paths, themes, settings, languages).

| Command | Aliases | Description |
|---|---|---|
| `:write` | `:w` | Save (or save as a new path) |
| `:write!` | `:w!` | Save, overwriting changes made on disk |
| `:write-quit` | `:wq` `:x` | Save and quit |
| `:quit` | `:q` | Close this window, or quit |
| `:quit!` | `:q!` | Quit without saving |
| `:quit-all` | `:qa` | Quit tarae |
| `:quit-all!` | `:qa!` | Quit tarae without saving |
| `:open` | `:o` `:e` `:edit` | Open files |
| `:new` | `:n` | New scratch buffer |
| `:reload` |  | Reload the file from disk (u brings your version back) |
| `:buffer-close` | `:bc` `:bclose` | Close this buffer |
| `:buffer-close!` | `:bc!` `:bclose!` | Close this buffer, dropping changes |
| `:buffer-next` | `:bn` `:bnext` | Next buffer |
| `:buffer-previous` | `:bp` `:bprev` | Previous buffer |
| `:vsplit` | `:vs` | Split side by side (optionally open a file there) |
| `:hsplit` | `:hs` `:sp` `:split` | Split top and bottom (optionally open a file there) |
| `:only` |  | Close every other window |
| `:close` |  | Close this window |
| `:theme` |  | Switch theme (no name = picker with live preview) |
| `:set` |  | Change a setting for this session (no value = show it) |
| `:set!` |  | Change a setting and save it to your config |
| `:toggle` |  | Flip an on/off setting |
| `:config-open` |  | Open your config file |
| `:config-reload` |  | Reload your config file |
| `:config-show` |  | Show every setting with its value |
| `:format` | `:fmt` | Format the file (language server) |
| `:set-language` | `:lang` | Set this buffer's language |
| `:grammar-install` |  | Fetch and build syntax grammars (this file's language, a name, or all) |
| `:run-shell-command` | `:sh` | Run a shell command in the background |
| `:ask` |  | Ask Claude to edit the selection |
| `:ask-cancel` |  | Cancel the Claude request |
| `:chat` |  | Open the Claude chat (optionally send a message) |
| `:chat-close` |  | Close the Claude chat |
| `:chat-new` |  | Start a new Claude chat |
| `:attach` |  | Attach the debugger to a running program (name or host:port) |
| `:watch` |  | Watch an expression while debugging |
| `:unwatch` |  | Stop watching (no expression = all) |
| `:tutor` |  | Learn the keys — a 10-minute tutorial |
