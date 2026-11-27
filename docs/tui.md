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
  typed into the terminal meanwhile are dropped. On Linux without a display it says how to get
  one.
- **With a target** (`--target`, `SOPEN_TARGET` or a config `target` naming another machine), F12
  opens the window on that machine's screen instead, at the same folders, bars, selections,
  dialog and viewer, and at the terminal's size: sopen there runs over ssh, the way a `--target`
  run does (it needs sopen installed, and reaches this machine's files by its own ssh). Meanwhile
  the terminal waits, dimmed, its line saying where xtools is, the other machine's last message
  on the status row; only Ctrl+C is read. F12 there, its `[TUI]` or closing it (its own unsaved
  edits ask first there) brings the terminal back with what was done in the window. **Ctrl+C**
  here ends the window there — or, if it has unsaved edits, leaves it open and says so; a second
  Ctrl+C stops waiting for the other machine. Before it goes, a running copy, git action or save
  is waited for (`waiting for …` on the status row); unsaved edits in the viewer ask `Save the
  changes to … before switching to …?` — Save, Save As… (it switches once that has landed),
  Discard (the file opens there as stored, and the edits here are dropped once its window hands back — kept here if it never does) or Cancel; and the git panel's
  open dialog or the planner's plan, which do not cross yet, ask whether to switch without them.
  Every way the other side can end — sopen not installed there, a usage error from an older
  sopen, ssh not reaching it, a refused save — is one line on the status row, and the terminal
  lists again.
- The text viewer (Enter on a text file: its rows, wrapped or not, highlighted, Go to, F8's
  encodings), the hex editor (`2` in a viewer: its rows, cursor, Go to field, buttons and
  status row), the preview of a text or hex file beside the list (its frame and rows), the
  directory tree (F10: its rows, sizes, bar and scroll bar; clicks and the wheel act on it) and
  the git panel's graph (its rows, lanes, labels, bar, scroll bar and paging line; clicks, the
  right click and the wheel act on it) are drawn in the terminal as in the window. What the
  terminal does not draw yet — the preview of a picture, a video or a sound, the other viewers,
  the git panel's Find field, banner, details pane, command log, Backups list and planner, and
  the dialogs of the viewers and of the git panel — is a box saying `F12 shows it in the window`
  (inside the git panel's frame for its parts); a click on it is F12. F1's key help is drawn in
  the terminal as in the window.
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
