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
| `!` | `shell_insert_output` | Insert a command's output before |
| `"` | `select_register` | Select register for the next command |
| `$` | `shell_keep_pipe` | Keep selections a command accepts |
| `%` | `select_all` | Select whole document |
| `*` | `search_selection` | Search for selection |
| `,` | `keep_primary_selection` | Keep primary selection |
| `.` | `repeat_last_insert` | Repeat last insert |
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
| `z` | … | More keys — see `z` below |
| `Z` | … | More keys — see `Z` below |
| `\|` | `shell_pipe` | Pipe selections through a command |
| `~` | `switch_case` | Switch (toggle) case |
| `A-!` | `shell_append_output` | Insert a command's output after |
| `A-.` | `repeat_last_motion` | Repeat last motion |
| `A-;` | `flip_selections` | Flip selection cursor and anchor |
| `` A-` `` | `switch_to_uppercase` | Switch to uppercase |
| `A-C` | `copy_selection_on_prev_line` | Copy selection on previous line |
| `A-c` | `change_selection_noyank` | Change selection without yanking |
| `A-d` | `delete_selection_noyank` | Delete selection without yanking |
| `A-down` | `shrink_selection` | Shrink selection back |
| `A-i` | `shrink_selection` | Shrink selection back |
| `A-K` | `remove_selections` | Remove selections matching the regex |
| `A-left` | `select_prev_sibling` | Select previous syntax sibling |
| `A-n` | `select_next_sibling` | Select next syntax sibling |
| `A-o` | `expand_selection` | Grow selection to the enclosing syntax node |
| `A-p` | `select_prev_sibling` | Select previous syntax sibling |
| `A-right` | `select_next_sibling` | Select next syntax sibling |
| `A-s` | `split_selection_on_newline` | Split selection on newlines |
| `A-up` | `expand_selection` | Grow selection to the enclosing syntax node |
| `A-x` | `extend_to_line_bounds` | Extend selection to line bounds |
| `A-\|` | `shell_pipe_to` | Send selections to a command |
| `C-b` | `page_up` | Move page up |
| `C-c` | `toggle_comments` | Comment or uncomment lines |
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
| `[ a` | `goto_prev_parameter` | Previous argument |
| `[ c` | `goto_prev_comment` | Previous comment |
| `[ d` | `goto_prev_diag` | Previous diagnostic |
| `[ f` | `goto_prev_function` | Previous function |
| `[ g` | `goto_prev_change` | Previous git change |
| `[ p` | `goto_prev_paragraph` | Previous paragraph |
| `[ t` | `goto_prev_class` | Previous type |
| `[ T` | `goto_prev_test` | Previous test |
| `[ x` | `goto_prev_test_failure` | Previous failed test |
| `[ space` | `add_newline_above` | Add a blank line above |

### `]`

| Key | Command | Description |
|---|---|---|
| `] a` | `goto_next_parameter` | Next argument |
| `] c` | `goto_next_comment` | Next comment |
| `] d` | `goto_next_diag` | Next diagnostic |
| `] f` | `goto_next_function` | Next function |
| `] g` | `goto_next_change` | Next git change |
| `] p` | `goto_next_paragraph` | Next paragraph |
| `] t` | `goto_next_class` | Next type |
| `] T` | `goto_next_test` | Next test |
| `] x` | `goto_next_test_failure` | Next failed test |
| `] space` | `add_newline_below` | Add a blank line below |

### `g`

| Key | Command | Description |
|---|---|---|
| `g a` | `goto_last_accessed_file` | Goto last accessed file |
| `g b` | `goto_window_bottom` | Cursor to the screen bottom |
| `g c` | `goto_window_center` | Cursor to mid-screen |
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
| `g t` | `goto_window_top` | Cursor to the screen top |
| `g w` | `goto_word` | Jump to a word by its label |
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

### `z`

| Key | Command | Description |
|---|---|---|
| `z b` | `align_view_bottom` | Cursor line to the bottom |
| `z c` | `align_view_center` | Cursor line to mid-screen |
| `z j` | `scroll_down` | Scroll down a line |
| `z k` | `scroll_up` | Scroll up a line |
| `z t` | `align_view_top` | Cursor line to the top |
| `z z` | `align_view_center` | Cursor line to mid-screen |
| `z C-b` | `page_up` | Move page up |
| `z C-d` | `half_page_down` | Move half page down |
| `z C-f` | `page_down` | Move page down |
| `z C-u` | `half_page_up` | Move half page up |

### `Z`

| Key | Command | Description |
|---|---|---|
| `Z b` | `align_view_bottom` | Cursor line to the bottom |
| `Z c` | `align_view_center` | Cursor line to mid-screen |
| `Z j` | `scroll_down` | Scroll down a line |
| `Z k` | `scroll_up` | Scroll up a line |
| `Z t` | `align_view_top` | Cursor line to the top |
| `Z z` | `align_view_center` | Cursor line to mid-screen |
| `Z C-b` | `page_up` | Move page up |
| `Z C-d` | `half_page_down` | Move half page down |
| `Z C-f` | `page_down` | Move page down |
| `Z C-u` | `half_page_up` | Move half page up |

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
| `space '` | `last_picker` | Reopen the last picker |
| `space /` | `global_search` | Search in project |
| `space ?` | `command_palette` | Find a command |
| `space a` | `code_action` | Code actions |
| `space b` | `buffer_picker` | Open buffer picker |
| `space c` | `claude_code` | Open Claude Code beside (connected) |
| `space C` | `claude_code_mention` | Send selection to Claude Code (@) |
| `space d` | `diagnostics_picker` | Diagnostics |
| `space D` | `workspace_diagnostics_picker` | Diagnostics in every file |
| `space f` | `file_picker` | Open file picker |
| `space g` | … | More keys — see `space g` below |
| `space G` | … | More keys — see `space G` below |
| `space h` | `select_references_to_symbol_under_cursor` | Select this symbol's uses |
| `space i` | `llm_ask` | Ask Claude to edit selection |
| `space j` | `jumplist_picker` | Jump list |
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
| `space g b` | `git_toggle_blame` | Blame on this line (on/off) |
| `space g f` | `changed_file_picker` | Changed files (git) |
| `space g p` | `git_preview_hunk` | Show this change |
| `space g r` | `git_reset_hunk` | Undo this change |
| `space g s` | `git_stage_hunk` | Stage this change |

### `space G`

| Key | Command | Description |
|---|---|---|
| `space G a` | `dap_attach` | Attach to a running program… |
| `space G b` | `toggle_breakpoint` | Toggle breakpoint |
| `space G c` | `dap_launch` | Start debugging / continue |
| `space G i` | `dap_step_in` | Step into |
| `space G l` | `dap_launch` | Start debugging / continue |
| `space G n` | `dap_next` | Step over |
| `space G o` | `dap_step_out` | Step out |
| `space G p` | `dap_pause` | Pause the program |
| `space G t` | `dap_terminate` | Stop debugging |
| `space G w` | `dap_watch` | Watch an expression |
| `space G W` | `dap_unwatch` | Remove a watch |
| `space G C-c` | `dap_edit_condition` | Break only when… (condition) |
| `space G C-l` | `dap_edit_log` | Log here instead of stopping |

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
| `A-backspace` | `delete_word_backward` | Delete previous word |
| `A-d` | `delete_word_forward` | Delete next word |
| `A-del` | `delete_word_forward` | Delete next word |
| `backspace` | `delete_char_backward` | Delete previous char |
| `C-d` | `delete_char_forward` | Delete next char |
| `C-h` | `delete_char_backward` | Delete previous char |
| `C-j` | `insert_newline` | Insert newline char |
| `C-k` | `kill_to_line_end` | Delete to end of line |
| `C-r` | `insert_register` | Insert register contents |
| `C-s` | `commit_undo_checkpoint` | Make what's typed an undo step |
| `C-u` | `kill_to_line_start` | Delete to start of line |
| `C-w` | `delete_word_backward` | Delete previous word |
| `C-x` | `completion` | Invoke completion popup (LSP) |
| `del` | `delete_char_forward` | Delete next char |
| `down` | `move_visual_line_down` | Move down (visual line) |
| `end` | `goto_line_end_newline` | Goto end of line (after the last char) |
| `esc` | `normal_mode` | Enter normal mode |
| `home` | `goto_line_start` | Goto line start |
| `left` | `move_char_left` | Move left |
| `pagedown` | `page_down` | Move page down |
| `pageup` | `page_up` | Move page up |
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
| `:write-all` | `:wa` | Save every modified file |
| `:write-quit-all` | `:wqa` `:xa` | Save every modified file and quit |
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
| `:log-open` |  | Open tarae's log (language servers, errors) |
| `:lsp-restart` |  | Restart this file's language server |
| `:lsp-stop` |  | Stop this file's language server |
| `:format` | `:fmt` | Format the file (language server) |
| `:set-language` | `:lang` | Set this buffer's language |
| `:grammar-install` |  | Fetch and build syntax grammars (this file's language, a name, or all) |
| `:run-shell-command` | `:sh` | Run a shell command in the background |
| `:pipe` | `:\|` | Pipe the selections through a command (output replaces them) |
| `:pipe-to` |  | Send the selections to a command (output ignored) |
| `:insert-output` |  | Insert a command's output before each selection |
| `:append-output` |  | Insert a command's output after each selection |
| `:ask` |  | Ask Claude to edit the selection |
| `:ask-cancel` |  | Cancel the Claude request |
| `:chat` |  | Open the Claude chat (optionally send a message) |
| `:chat-close` |  | Close the Claude chat |
| `:chat-new` |  | Start a new Claude chat |
| `:attach` |  | Attach the debugger to a running program (name or host:port) |
| `:watch` |  | Watch an expression while debugging |
| `:unwatch` |  | Stop watching (no expression = all) |
| `:tutor` |  | Learn the keys — a 10-minute tutorial |
