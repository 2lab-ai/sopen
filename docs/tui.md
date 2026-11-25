# In the terminal: `--tui`

```sh
sopen --tui myserver:/srv/app      # the file browser in this terminal
sopen --tui ./notes.txt            # its folder with its viewer open: F12 shows it, Esc the list
```

`--tui` runs xtools in the terminal you typed it in, drawn in the same cells as its window — the
same frames, colours and keys — Korean and other wide names two cells a character. Its input and
output must be that terminal (in a pipe it is a usage error, exit 2). Without `--tui` nothing
changes: a folder still opens in a window.

- **F12**, **Alt+G** (for a terminal that keeps F12) or a click on the title row's **`[GUI]`**
  moves it into a window on this machine, at the terminal's size, and back: F12 there, its
  `[TUI]`, or closing the window (Cmd/Ctrl+Q and +W too) returns
  to the terminal with everything as it was — the bar, a dialog with what you typed in it. Keys
  typed into the terminal meanwhile are dropped. With `--target`, `SOPEN_TARGET` or a config
  `target` naming another machine, it stays in the terminal and says so (the switch to another
  machine comes later); on Linux without a display it says how to get one.
- The hex editor (`2` in a viewer) is drawn in the terminal as in the window — its rows, cursor,
  Go to field, buttons and status row. What the terminal does not draw yet — the preview's frame,
  the other viewers, the directory tree, the git panel, the dialogs of the viewers — is a box
  saying `F12 shows it in the window`; a click on it is F12. F1's key help is drawn in the
  terminal as in the window.
- **Keys**: where the terminal offers the kitty keyboard protocol sopen turns it on and every key
  arrives as in the window. Elsewhere Ctrl+digits and some Ctrl+Shift chords cannot arrive, so
  **Alt+1…0 are F1…F10** (on a Mac the terminal must send Option as Alt). Ctrl is the window's
  Cmd/Ctrl on every machine: Ctrl+Q quits, Ctrl+C copies.
- **Mouse**: clicks and the wheel act on the rows and the F-key row; to select text with the
  terminal itself, hold Shift while dragging (in most terminals).
- **Copy** goes to the clipboard through the terminal (OSC 52; some terminals ask first, or need it
  turned on), up to 256 KiB — a larger copy says so on the status row instead; paste with the
  terminal's own paste.
- **Colours** are 24-bit where `COLORTERM` says `truecolor` or `24bit`, else the nearest of the
  256-colour palette.
- The terminal is left as it was found however xtools ends — quit, a crash, SIGTERM, SIGHUP; when
  the terminal itself goes away, sopen ends too (exit 129). Messages written to the standard error
  while xtools holds the terminal are printed once it has let go.
