# Binary files

```sh
sopen myserver:/srv/data.zip          # a dialog: Open in hex editor (Enter) · Save to… · Cancel (Esc)
sopen --hex myserver:/srv/notes.txt   # any file straight in the hex editor, no dialog
```

## The binary-file dialog

Anything that is not a folder, text, an image or media opens a small dialog first — its name, size
and type; nothing is downloaded yet. **Enter** opens the xtools hex editor (the default) at once,
reading only what is on screen. **Save to…** downloads it to the path below the buttons — your
download folder at first, or the one "Browse…" picks — and never over an existing file: if the name
is taken it says so and offers the next free one (`data-1.zip`). **Esc** (or closing the window)
downloads nothing. Exceptions: Enter in the path field saves there (it never opens the hex editor);
while a download runs, Enter does nothing, and Esc, Cancel or closing the window stops it and
leaves nothing behind (`cancelled; nothing was saved`).

The file appears under its name only complete: it is written into a private hidden folder
(`.sopen-…`) in the chosen folder and published from there with a hard link, never over anything.
A folder whose filesystem has no hard links (exFAT, some network shares), a folder that does not
exist, or a path ending in '/' is refused before anything is transferred. A download whose source
changes meanwhile is not saved; that check is best-effort (the remote file's size and modification
time, and the bytes received).

## The hex editor

The hex editor shows rows of 16 bytes — the offset in decimal and hex (`0016(0010)`), the hex, and
the bytes as a DOS screen shows them (code page 437: `☺ ♥ ▲`, `é`, `░ ═`) — and reads only what is
on screen, so a file of any size scrolls the same. It overwrites bytes and never changes the file's
size:

| Key | |
|---|---|
| arrows, `PgUp` / `PgDn`, `Home` / `End`, `⌘Home` / `⌘End` | move the cursor (row start / end, file start / end); a click places it |
| hex digits / characters | overwrite: in the hex column two digits per byte, in the ASCII column one character (printable ASCII); `Tab` or `F8` switches the column |
| `⌘G` or `F6` | go to an offset (`0x…` or decimal) |
| `⌘Z` / `⇧⌘Z` (Ctrl on Linux) | undo / redo |
| `⌘S` or `F5` | Save. A remote file is copied on its own server, your changed bytes are written into the copy, and the copy replaces the original: a 4-byte change to a 24 GB file sends 4 bytes, not the file. The bytes it replaced are kept in a local undo file (named in the result), which the hex editor offers back as **Load as edits**. A server without `copy-data` (OpenSSH before 9.0) has a file of up to 256 MiB rewritten through this machine; above that, or without room on the server for the copy, Save is refused before anything moves and offers **Write in place…** (not atomic, and it says so) or **Save As…**. A local file is first backed up locally (path printed), then overwritten in place, as image Save does. If the file changed since it was opened, nothing is written and your edits stay. While a Save runs nothing is edited; `Esc` stops a remote Save before it replaces anything |
| `⇧⌘S` | Save copy: `<name>.edited.<ext>` next to the original |
| `⌥⌘S` or `F7` | Save As: the edited file to a new file on this machine (never over an existing one), the unchanged bytes read from the remote on the way |

A remote file above 256 MiB cannot have its whole content checked before a Save: sopen checks its
size, modification time and the bytes under your edits, and the first Save asks once, since a
program writing to the file at the same time can lose that write.

If a local file saved in place is changed by someone else right after the save, the window turns
read-only and lists your edits in a `.hexpatch` file it names. When even that list cannot be
written, the window offers **Keep edits…** (try again) and **Write the list there** (a path you
choose), and closing asks before your edits are lost.
