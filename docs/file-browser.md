# Folders: the file browser

The xtools file browser: two panels, a preview, the git panel, the directory tree, file
operations and the text viewer. The same browser runs in a terminal with [`--tui`](tui.md).

```sh
sopen myserver:/srv/app        # two panels on the folder, as in Total Commander
```

A folder opens in an xtools window: two file panels side by side, each listing `..`, then the
folders, then the files (Name · Ext · Size · Date · Time; hidden files dimmed), with
`N Listed = N bytes` and `N Bytes Free` at the foot.

## Preview mode and dual mode

The window opens in **preview mode**: rest the bar on a file and the other side shows it — text
(wrapped, highlighted), an image, a video's first frame, a sound's waveform (its first 10
minutes), or the hex rows of anything else, whatever the file's size. On a folder the other side is
its own list again — or, inside a git work tree, the **git panel** (below). `F9` switches
to two plain lists (**dual mode**) and back. A preview is only looked at: `Enter` makes it the
viewer.

## The git panel

In a folder inside a git work tree, rest the bar on `..` (or any folder) and the other side shows
the repository's **git panel**: every commit and branch as a graph — lanes, branch and tag labels
(`►main↔origin`, `v1.0`, `stash@{0}`), subject, date, author, hash, the uncommitted changes on
top — read 500 commits at a time, more as you scroll. `Tab` gives it the keyboard; `Enter` on a
commit opens its details below the graph (summary, message, changed files with `+adds -dels`),
`Enter` on a file shows its diff and `Shift+F3` the file as it was at that commit, each in its
viewer and read-only. Mark a commit (`Space`) and `F3` on another to compare them. A typed letter
finds in the loaded rows, `⌘⇧F` searches the whole history, `⌘K` filters it (branches, remotes,
tags, stashes, first parent, order), `⌘R` refreshes. `⌘G` pins the panel to its side: it stays
while you move over files or go elsewhere; `Esc` unpins. It runs the host's own `git` (on a remote
over the same ssh connection, nothing installed).

### Git Graph actions

Every Git Graph action is there too: `F10` (or `Shift+F10`, a right click) opens the menu of what
is under the bar — a commit, the branch or tag label under the label cursor (`←` `→`), a stash,
the uncommitted row or one of its files, the repository — and the F-keys keep their meaning: `F5`
cherry-pick (on a stash: apply), `F6` rebase onto it, `F7` new branch (on the uncommitted row:
commit), `F8` delete or discard (on a commit: drop it), `F2` reword a commit or rename a branch;
`Space` on a changed file stages or unstages it. Each action shows a dialog first that says exactly what changes, the backup refs it writes
before anything rewrites or deletes (`refs/sopen-backup/<stamp>/…`) and the exact git command
lines; `Enter` runs it, `Esc` cancels. A run shows its time on the status row (and, when git goes
quiet, what it may be waiting for); `Esc` stops a fetch, and a write after 10 s, asking first.
`⌘Z` undoes the newest backup written in this window (again: redo), `⌘B` lists every backup
(restore, show, delete), `⌘L` shows the command log, `⌘⇧R` fetches all. While a merge, rebase,
cherry-pick, revert or bisect is in progress the panel's top row says so (`MERGE of feature · 1
conflict`), with its Continue (`⌘⇧C`), Skip (`⌘⇧S`), Abort (`⌘⇧A`) and bisect's Reset (`⌘⇧B`).

### Editing history

History is free to edit, every edit backed up first: a commit's menu (on HEAD and its ancestors)
also offers reword, author and date, squash or fix up into the commit below, move up or down, drop,
stop here to edit, split, fold the staged changes into it, and **Edit history from here…** (`⌘E`):
the **planner** over the commits above it — `P` `R` `E` `S` `F` `D` set each row's verb (pick,
reword, edit, squash, fixup, drop), `A` its author and dates, `⌘↑` `⌘↓` move it, `Enter` edits a
reword's or a squash group's message, `⌘Enter` shows the run and its options, `Esc` closes (asking
when something changed). One `git rebase -i` runs the plan, its todo written by sopen, messages kept
byte for byte; a range holding a merge is refused, naming it. At an edit stop the top row offers
Amend (`⌘⇧M`) and Split (`⌘⇧P`) too; after a split, Continue waits until every file of the split
commit is committed again. Rebase's dialog offers "Plan the commits first…".

## The directory tree

`F10` shows the **directory tree** in place of the list: rooted at your home when the folder is
under it, else at `/`, the bar on the folder you were in. Folders with few subfolders open by
themselves; every row shows at once how many folders and files it holds and how big it is — a size
with `+` is still a lower bound and grows as the tree reads below it. Hidden folders are one dimmed
row (`· 3 hidden folders`); device, system, network and mounted folders are never opened by
themselves, and a folder that does not answer says so instead of holding the tree up. `Enter` lists
the folder in the panel; `Esc` or `F10` closes the tree with the panel as it was. While the tree is
open the other side keeps what it showed.

## Rename, copy, move, make folder and delete

Rename, copy, move and make folder never replace anything: a name already there asks **Skip**,
**Rename** (a free name is proposed) or **Cancel** (a rename or a new folder onto a taken name is
an error instead). A copy shows its progress in the status row, `Esc` stops it, and a report lists
what was done, skipped and not reached. On a remote a folder is renamed or moved in one step by
SFTP's plain rename, which refuses a name that exists; where the server has no
`hardlink@openssh.com`, sopen first checks once per connection that its rename refuses one, and
copies there only if it does. Delete removes a folder with everything inside it, and a link as a
link (never what it points at); `Esc` stops it between entries.

## Keys

| Key | |
|---|---|
| arrows, `PgUp` / `PgDn`, `Home` / `End` | move the bar |
| `Tab` | the other panel |
| `Enter` | a folder: go in; `..`: go up; a file: show it in the viewer for its kind, in the same window |
| `Backspace` | up one folder |
| a letter | the next name starting with it |
| `Space` / `Ins` | select the entry and step down (`+` / `-`: by a pattern, `*`: invert, `⌘A`: all) |
| `F1` | the keys of where you are |
| `F2` (or `Shift+F6`) | rename the entry, in its row |
| `F3` | view the file (as Enter) |
| `F4` | open the folder or file in VS Code / Zed |
| `F5` / `F6` | copy / move the selection (or the entry) to the other panel's folder — asks first; the target can be any folder, `host:/path` too |
| `F7` | make a folder here (asks its name); the bar goes on it |
| `F8` / `Del` / `⌘⌫` (macOS) | delete the selection (or the entry) — asks first, and says where it goes: a local entry to the Trash (macOS's, or the freedesktop Trash on Linux; where there is none, permanently, said before anything is removed), a remote one permanently (SFTP has no Trash) |
| `Shift+F8` / `Shift+Del` | delete without the Trash (asks first) |
| `F9` | preview mode ↔ dual mode |
| `⌘G` | pin the git panel to the other side (again, or `Esc`: unpin) |
| in the git panel | `Enter` / `F3` details (one marked row: compare) · `Shift+F3` the file at that commit · a letter / `⌘F` find · `⌘⇧F` history search · `⌘K` filters · `⌘R` refresh · `⌘J` to HEAD · `⌘⇧G` over both sides · `Tab` back to the list · `Esc` closes the innermost thing |
| actions in the git panel | `F10` / `Shift+F10` / right click the menu · `F5` pick · `F6` rebase · `F7` branch (uncommitted row: commit) · `F8` delete / discard (a commit: drop) · `F2` reword a commit / rename a branch · `Space` on a changed file stage / unstage · `⌘⇧R` fetch all · `⌘Z` undo · `⌘B` backups · `⌘L` command log · `⌘⇧C` / `⌘⇧S` / `⌘⇧A` continue / skip / abort, `⌘⇧B` bisect reset · `⌘E` the history planner · at an edit stop `⌘⇧M` / `⌘⇧P` amend / split |
| `F10` (or `Alt+F10`) | the directory tree; in it `→` / `←` open and close a folder, `Space` sizes a row now (`Esc` stops that), `⌘R` reads a row again, `Enter` lists the folder in the panel, `Esc` / `F10` close the tree |
| `⌘R` · `⌘U` · `⌘←` / `⌘→` | re-read · swap the panels · this folder in the left / right panel |
| `⌘1`…`⌘0`, or a click on the bottom row | `F1`…`F10` (Ctrl on Linux) |
| `Esc` / `F3` in a viewer | back to the list, the bar where it was |
| `1` / `2` / `3` in a viewer | the file's own view / its bytes in the hex editor (any file) / the other view: Raw (text unhighlighted) for text, the text of a binary; the title row names them (`1 Text · 2 Hex · 3 Raw`). Where digits are typed (the hex editor being edited, a field), `F9` then the digit |
| `F8` / `E` in a text or Raw view | the list of encodings (ASCII, UTF-8, UTF-16LE/BE, UCS-2LE/BE, UTF-32LE/BE, Latin-1, CP437, CP949): `↑` `↓` and `Enter` read the file again in the one chosen, `Esc` keeps it |
| `F3` in a window of its own | close it, as `⌘W` (asks first when there are unsaved changes) |

## Viewing a file

Text shows read-only with syntax highlighting and line numbers, a page at a time (a file of any
size opens alike); images open in the image window, video and audio in the media window, anything
else in the hex editor — each with its own edits and Save, as in [Images](images.md), [Video and
audio](video-audio.md) and [the hex editor](binary.md#the-hex-editor). `2` shows any file's bytes in
the hex editor, `1` its own view again, as it was; there the hex editor opens viewing: digits and
letters are not typed until `Enter` (or `Insert`) starts editing, and `Esc` ends it. Leaving a
viewer, or switching from a view, with unsaved changes asks first, as its own window does. A remote file is not downloaded: it shows as soon as
its first part arrives and the rest is read as you move (an image is read whole into memory first,
with its progress; `Esc` stops that).

## The text viewer

Lines wrap by default: every line is shown whole, in rows as wide as the window, its number on its
first row. A row is broken by the measured width of its characters (Korean, CJK and emoji are
wider than a cell), after a space when the next word fits on the next row — Chinese and Japanese
between any two characters, never before closing punctuation such as `。` or `」` — and a tab goes
to the next stop of four cells. A row starting with a Hebrew or Arabic word still reads left to
right: its English stays as written, each right-to-left word reads right to left. The file is still read a chunk at a time — a line of hundreds of
megabytes too, from wherever you are in it. Inside a line longer than the window the status row
says how far into it you are (`line 7 (12 % of it) of 120`), and the scroll bar goes by bytes; a
click on it jumps there. A remote file's lines are numbered as far as they have been read (its
first 16 MiB in the background); past that the gutter shows byte offsets (`0x…`) until you get
there.

| Key in the text viewer | |
|---|---|
| `W` / `F2` | switch wrapping off (one row per line; `←` / `→`, `Home` / `End` move along a line of any length) and on again; the F-key row says `Unwrap` or `Wrap` |
| `⌘G` (`Ctrl+G` elsewhere) | go to a line number, or a byte offset (`0x…`); in a large remote file a line not reached yet is counted up to, with its progress (`Esc` stops) |
| `↑` / `↓`, `PgUp` / `PgDn`, the wheel | wrapped: move by rows (a page keeps one row); unwrapped: by lines |
| `Home` / `End` | wrapped, inside a long line: its first / last row first, then the start / end of the file; otherwise the start / end of the file |
| `⌘Home` / `⌘End` (`Ctrl` elsewhere) | the start / end of the file |

## The look

Every xtools window looks like PC Shell from the DOS PC Tools 5.5 / 6: the VGA text font, blue
windows framed in box lines (a double line around the active one) with their title in the frame,
the light-red highlight bar, the title row with the clock, the F-key row at the bottom (only the
keys that work there are labelled), cyan dialogs with a shadow.
