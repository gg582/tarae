# tarae tutorial

Ten minutes, hands on. This is a practice buffer — edit it freely (it's never saved).
Forget a key? Press `space` any time — a card shows the keys you can press next.

────────────────────────────────────────────────────────────────

## 1. Modes — a key types a letter, or runs a command

You're in **NORMAL** mode (the pill at the bottom left). Here keys are commands.
Press `i` for **INSERT** mode — from then on, what you type is text. `Esc` brings you back.

  → Put the cursor after "here" at the end of the line below, press `i`, type anything, `Esc`.
     here

────────────────────────────────────────────────────────────────

## 2. Moving around

  `h` ←   `j` ↓   `k` ↑   `l` →      (clicking with the mouse works too)
  `w` next word   `b` previous word   `e` end of word
  `gg` top   `ge` bottom   `gh` line start   `gl` line end

  → Move down with `j` and press `w` a few times.
     one two three four five

────────────────────────────────────────────────────────────────

## 3. Select first, then act (the heart of tarae)

`w` **selects** a word, and `d` deletes what's selected. You see the selection, so no surprises.
`x` selects a line (press again to grow it one line at a time).

  → On the line below, move onto "wrong", select it with `w`, then `d`.
     This sentence has one wrong word in it.
  → Go to the line below and press `x` `d` — the whole line goes.
     This line should be deleted.

`c` deletes the selection and drops you straight into INSERT mode (change).

  → Select "blue", press `c`, type "red", `Esc`.
     The sky is blue

────────────────────────────────────────────────────────────────

## 4. Inserting

  `i` before cursor   `a` after cursor   `I` line start   `A` line end
  `o` new line below   `O` new line above

  → On the line below, press `A` and add " done.".
     This sentence isn't

────────────────────────────────────────────────────────────────

## 5. Undo

  `u` undo   `U` redo — don't be shy, press things and see.

────────────────────────────────────────────────────────────────

## 6. Multiple cursors

`C` adds a cursor on the line below. Everything you type now happens in both places at once.

  → On the first "apple": `w`, `C` twice, `c` "pear" `Esc` — three lines change at once.
     apple one
     apple two
     apple three

Select everything with `%`, then type a pattern after `s` — you get a cursor on every match.

────────────────────────────────────────────────────────────────

## 7. Search

  `/` search (matches light up as you type)   `n` next   `N` previous   `*` search for the selection
  Bottom right shows which match you're on (`3/12`); `Esc` in normal mode clears the highlight.

  → `/thread` `Enter`, then `n` a few times.
     one thread, another thread, a third thread

────────────────────────────────────────────────────────────────

## 8. The space menu — find anything without knowing the keys

  `space f` open a file      `space /` search the project
  `space b` open buffers     `space ?` find any command
  `space t` pick a theme (it changes as you move)

────────────────────────────────────────────────────────────────

## 9. With Claude

  `space i`  ask Claude to rework the selection — the changes unfold in the buffer in red and
             green; `y` accept · `n` discard
  `space l`  chat panel on the right — Claude sees the current file, cursor and selection.
             `C-r` puts code from the answer into the selection

  → Select the line below with `x`, press `space i`, type "make it polite", `Enter`.
     hey, look at this

────────────────────────────────────────────────────────────────

## 10. Save and quit

  `:w` save   `:q` quit   `:q!` quit without saving
  Switch to another window (the terminal loses focus) and files save themselves.
  If a file changes on disk, it's reloaded for you.

That's it! Close this tutorial with `:bc!` and open a file with `space f`.
