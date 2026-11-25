# sopen

`sopen` is the command; xtools is the multi-function tool it opens — the file browser, the
viewers, the image, video, audio and hex editors, the git panel and the directory tree.

Open a file or folder on another machine as if it were local:

```sh
sopen myserver:~/project           # folder → the xtools file browser: two panels, Enter views any file
sopen --tui myserver:~/project     # the same file browser in this terminal; F12 moves it into a window and back
sopen myserver:/var/log/app.log    # text → VS Code remote (Remote - SSH), else Zed
sopen myserver:/tmp/shot.png       # image → view, rectangle crop / pixelize, cut / copy / paste, save back
sopen myserver:/tmp/talk.mp4       # video → play, cut / copy / paste, crop → save back or a new file
sopen myserver:/tmp/a.mp3          # audio → play, cut / copy / paste, mono → stereo → save back or a new file
sopen stems myserver:/tmp/a.mp3    # voice and music → two new files next to it
sopen myserver:/tmp/a.zip          # anything else → asks: hex editor (default) or save it to a path
sopen --target <host> myserver:/tmp/a.mp3   # the same, on <host>'s screen instead of this one
sopen --target <host> http://127.0.0.1:1234/  # this machine's web page in <host>'s browser, tunnelled
```

Same syntax as `scp`. Uses your `ssh` and `~/.ssh/config`; the remote needs nothing but sshd.

Remote files are never downloaded just to look at them: sopen reads what is on screen and a
little around it, the rest as you move, like a video stream — a 24 GB file in the hex editor
shows its first screen after reading 64 KiB of it. Edits in the xtools image, video, audio and hex
windows are kept on this machine until you save — in memory and, a moment later, on disk, so a
crash loses at most that moment's — and Save writes back to the remote file, Save As to a local
one.

## Install

```sh
brew install 2lab-ai/tap/sopen-preview    # macOS (arm64, x86_64), Linux (x86_64, aarch64); pulls in ffmpeg
```

## Requirements

- Media needs `ffmpeg` and `ffprobe` (the formula installs them).
- The git panel needs git 2.20 or newer where the repository is (`SOPEN_GIT` names this machine's;
  on macOS `/usr/bin/git` counts only when the developer tools are installed). Without it the
  folder simply shows its list.
- `sopen stems` needs its model, which the formula installs too (67 MB), and memory: about
  6.5 GB at its peak, whatever the length of the track.
- Text (and F4 in the file browser): local files open in VS Code (`code`) when it is installed;
  remote ones open in VS Code when its Remote - SSH extension is installed (sopen does not install
  it), else in Zed. Without VS Code, sopen uses Zed. On Linux under KDE Plasma, KWin would only
  flag the editor's window (often on another desktop), so sopen brings it to your current desktop
  and screen and activates it; `SOPEN_RAISE=0` turns that off.
- Linux: the ALSA runtime library (libasound2 / alsa-lib) and the C++ runtime (libstdc++, for
  `sopen stems`) — both present on desktop Linux; sopen runs
  on the machine whose screen shows the file (the one you sit at, or the `--target`), not on the
  remote. The file browser, the image and media windows, the binary-file dialog and the hex window
  need a desktop session (X11 or Wayland); without one a folder opens in the editor and a binary,
  image or media file is fetched and its path printed — or, with `--tui`, the file browser runs in
  the terminal; with `--gui`, which asks for the window, sopen says so and exits 1.

## What the remote needs

Only sshd with its SFTP subsystem (the default). sopen installs and runs nothing there. When you open
a text file (or press F4 in the file browser), VS Code Remote - SSH or Zed installs its own remote server on first use — that is the editor's behaviour,
not sopen's. Folders are listed over SFTP. What xtools shows itself — text in the file browser, images,
video, audio, the hex editor — is read over SFTP a piece at a time and handled on this machine; video
and audio are decoded here by ffmpeg from that stream. The git panel runs the remote's own `git`, when
it has one, over the same ssh connection — nothing is uploaded. (A `--target` machine is
different: it shows the file, so it needs sopen installed — see below.)

## Open on another machine's screen: `--target`

```sh
sopen myserver:/tmp/a.html                    # opens here, on the machine you typed it on
sopen --target <host> myserver:/tmp/a.html    # opens on <host>'s screen (sopen runs there, over ssh)
sopen --target <host> <host>:/tmp/a.html      # a file on <host> itself opens there as a local file
sopen --target <host> ./notes.txt             # a local file: <host> fetches it from this machine
sopen config target <host>                    # from now on `sopen SOURCE` opens on <host>
sopen --here myserver:/tmp/a.html             # …but this one here
sopen --gui --target <host> <host>:/srv/app   # in a window on <host>, whatever sopen sees of it from here
sopen config                                  # where the config file is, the target (and fkeys, if set)
sopen config target --unset                   # back to this machine
sopen config fkeys system                     # macOS: sopen never touches what the top row sends
sopen config fkeys function                   # …or F1–F12 without fn while xtools is in front (the default)
```

The config file is `~/.config/sopen/config.toml` (`$XDG_CONFIG_HOME/sopen/config.toml` when that is
set, `$SOPEN_CONFIG` overrides both) on macOS and Linux alike:

```toml
target = "<host>"
fkeys = "function"   # or "system"; without the key, "function"
```

`fkeys` is for macOS: with `"function"` (the default), while an xtools window is in front sopen
switches the Mac's top row to F1–F12 without fn — so F5 should copy — and brightness and volume
need fn; once no xtools window is in front (or the Mac sleeps, or you switch user), the Mac's own
setting is back, even if sopen crashed. The switch is measured to reach the keyboard driver;
that a plain F5 then reaches xtools is not yet checked on a real keyboard (`.prd/04-file-keys.md`
§6.2, K-F3). The setting is global,
so other apps see F-keys while xtools is in front. `"system"` leaves the Mac's setting alone; the
F-key row's slots and Cmd/Ctrl+1…0 (F1–F10) work either way. Linux has no such switch: a laptop's
own Fn lock (often Fn+Esc) is the way there.

`--target` or `--here` (not both) decides one run; without them `SOPEN_TARGET=<host>` in the
environment comes first, then the config. `<host>` is `[user@]host` — an ssh alias or a host
name that `ssh <host>` reaches; a port or an IPv6 address goes into `~/.ssh/config` under an
alias. It needs sopen installed; sopen there gets `--here`, so it never sends the file on again.
A local file (or a `localhost:` one) is fetched by `<host>` from this machine by its short host
name, so that name must work for `ssh` on `<host>`.

For what opens in a window, sopen there also gets `--gui`: a folder, an image, video or audio, a
binary (a local file as sopen finds it here, a remote one by its name alone), and anything with
`--hex` or your own `--gui`. A text file goes without it and keeps the editor;
so does a remote one whose name does not say what it is (`<host>:/srv/app`, `<host>:/srv/data`):
`<host>` then picks by what it finds. `--app` and addresses never get it, and a scripted run that
opens no window (`SOPEN_LIST`, `SOPEN_AUTOSAVE`, …) gets it only from you. A `<host>` whose sopen
is older than `--gui` refuses it (its own usage error); sopen then runs the same thing there once
more without it, and says `sopen on <host> is older and does not know --gui; running it there
again without it`. `--gui` without a target opens here as before, and on a machine without a
display (Linux without `DISPLAY` or `WAYLAND_DISPLAY`) it exits 1 with `no display here — sopen
config target <host>, or --target, opens the window on another machine` — as does `<host>`'s
sopen, given `--gui`, when it finds no desktop session there.

The window opens on `<host>`'s desktop session (on Linux sopen finds it through
`systemctl --user`). On macOS an app started over ssh cannot take focus: the xtools window
floats above the other windows until you click, type or scroll in it (or it gets focus), but the editor
(for text) opens behind the active app; under KDE Plasma it is brought to the screen you are on. From a terminal, Ctrl+C reaches `<host>` and closes the window
there; a sopen stopped without a terminal leaves the window on `<host>` open.

The exit code is `<host>`'s; `could not reach <host>` means ssh did not get there, `sopen is not
installed on <host>` (exit 127) means it has no sopen, `sopen on <host> ended with status
N (signal n?)` means it most likely was interrupted there, and exit 2 with `<host>`'s own usage
line (other than the one for `--gui`) means its sopen is too old for the run, or the run is wrong.

## Web addresses: `sopen URL`

```sh
sopen http://127.0.0.1:1234/                  # opens in this machine's default browser, and sopen ends
sopen --target <host> https://example.com/    # opens in <host>'s default browser (Safari, Chrome, Edge…)
sopen --target <host> http://127.0.0.1:1234/  # a page served here, opened on <host> through an ssh tunnel
```

An `http://` or `https://` address (the scheme in any case) is never read as `host:path`; it goes to
the default browser of the machine that shows things — `open` on macOS, `xdg-open` on Linux —
exactly as written. Without a target that is all: the browser opens, sopen ends.

With a target, an address that would mean `<host>` itself there — `localhost`, `127.x.y.z`, `[::1]`
(and the other loopback names `--target` treats as this machine) — is served from this machine
instead: sopen opens an ssh connection of its own to `<host>` (never its shared one) with
`-R 127.0.0.1:<port>:<address's host>:<port>` (`[::1]` for an IPv6 address), so the port listens on
`<host>`'s loopback only, and `<host>`'s sopen opens the address in its browser and waits. If
`<host>`'s sshd has `GatewayPorts yes`, which binds a forward to every interface whatever is asked,
`<host>`'s sopen sees the port open to the network and refuses before the browser opens — as it
does whenever it cannot see the tunnel's listener on loopback alone. sopen prints
`<address> is open on <host> through a tunnel — Ctrl-C closes it` and stays; Ctrl-C (or SIGTERM)
closes the tunnel and exits 0. When sopen ends any other way — killed outright included — its pipe
to ssh closes: `<host>`'s sopen ends, and so does the ssh under sopen's guard, even while the browser
still holds a connection, so the page no longer opens. When `<host>`'s sopen ends first (killed
there), sopen closes the tunnel too and exits 1. When the address's port cannot be forwarded on
`<host>` — taken there, or forwarding refused (ssh says the same for both), as for a port below 1024
for a user — sopen tries two random ports from 49152–65535 and says which one it took: the address
then opens with that port, a different origin; if those fail too, `<host>` refused the tunnel
(`AllowTcpForwarding`?). Any other address (`https://example.com/`) is handed to `<host>`'s sopen as
it is, with no tunnel. `--app` and `--hex` open files, so with an address they are a usage error
(exit 2).

What sopen prints of an address — results, errors, `SOPEN_LOG=debug` lines — never has its password,
query or fragment: `http://me:***@host:3000/path?…#…`. Only the browser gets it whole. (It is in the
ssh command line, so `ps` on either machine can show it.) A host a browser reads as another address —
`127.1`, `0x7f.0.0.1`, `%`-escapes, full-width digits, `[::ffff:127.0.0.1]` — is refused: write it
plainly.

The tunnel needs only what `ssh -R` needs: `<host>`'s sshd allowing TCP forwarding (the default). A
`LocalForward` or `RemoteForward` your `~/.ssh/config` sets for `<host>` is requested by this
connection too and, if it fails, ends it. So far the tunnel is tested against a stand-in `ssh`, not
yet a real one.

## Folders: the file browser

```sh
sopen myserver:/srv/app        # two panels on the folder, as in Total Commander
```

A folder opens in an xtools window: two file panels side by side, each listing `..`, then the
folders, then the files (Name · Ext · Size · Date · Time; hidden files dimmed), with
`N Listed = N bytes` and `N Bytes Free` at the foot.

The window opens in **preview mode**: rest the bar on a file and the other side shows it — text
(wrapped, highlighted), an image, a video's first frame, a sound's waveform (its first 10
minutes), or the hex rows of anything else, whatever the file's size. On a folder the other side is
its own list again — or, inside a git work tree, the **git panel** (below). `F9` switches
to two plain lists (**dual mode**) and back. A preview is only looked at: `Enter` makes it the
viewer.

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

`F10` shows the **directory tree** in place of the list: rooted at your home when the folder is
under it, else at `/`, the bar on the folder you were in. Folders with few subfolders open by
themselves; every row shows at once how many folders and files it holds and how big it is — a size
with `+` is still a lower bound and grows as the tree reads below it. Hidden folders are one dimmed
row (`· 3 hidden folders`); device, system, network and mounted folders are never opened by
themselves, and a folder that does not answer says so instead of holding the tree up. `Enter` lists
the folder in the panel; `Esc` or `F10` closes the tree with the panel as it was. While the tree is
open the other side keeps what it showed.

Every xtools window looks like PC Shell from the DOS PC Tools 5.5 / 6: the VGA text font, blue
windows framed in box lines (a double line around the active one) with their title in the frame,
the light-red highlight bar, the title row with the clock, the F-key row at the bottom (only the
keys that work there are labelled), cyan dialogs with a shadow.

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

Text shows read-only with syntax highlighting and line numbers, a page at a time (a file of any
size opens alike); images open in the image window, video and audio in the media window, anything
else in the hex editor — each with its own edits and Save, as below. `2` shows any file's bytes in
the hex editor, `1` its own view again, as it was; there the hex editor opens viewing: digits and
letters are not typed until `Enter` (or `Insert`) starts editing, and `Esc` ends it. Leaving a
viewer, or switching from a view, with unsaved changes asks first, as its own window does. A remote file is not downloaded: it shows as soon as
its first part arrives and the rest is read as you move (an image is read whole into memory first,
with its progress; `Esc` stops that).

Rename, copy, move and make folder never replace anything: a name already there asks **Skip**,
**Rename** (a free name is proposed) or **Cancel** (a rename or a new folder onto a taken name is
an error instead). A copy shows its progress in the status row, `Esc` stops it, and a report lists
what was done, skipped and not reached. On a remote a folder is renamed or moved in one step by
SFTP's plain rename, which refuses a name that exists; where the server has no
`hardlink@openssh.com`, sopen first checks once per connection that its rename refuses one, and
copies there only if it does. Delete removes a folder with everything inside it, and a link as a
link (never what it points at); `Esc` stops it between entries.

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

## In the terminal: `--tui`

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
- What the terminal does not draw yet — the preview, the viewers, the directory tree, the git
  panel, their dialogs — is a box saying `F12 shows it in the window`; a click on it is F12.
  F1's key help is drawn in the terminal as in the window.
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

## Images

The window shows the image fitted; the mouse wheel or a pinch zooms, a trackpad scroll or a
right-drag pans (`F` fits again, `A` is 100 % — `1` too in a window of its own; in the browser's
viewer `1` is view 1). Drag a rectangle (handles adjust it, dragging
inside moves it), then:

| Key | |
|---|---|
| `C` | crop to the rectangle |
| `P` | pixelize the rectangle (block size slider, 16 px by default) |
| `⌘C` (Ctrl+C on Linux) | copy the rectangle — or, with none, the whole image — to the clipboard as a picture |
| `⌘X` | cut: copy the rectangle, then fill it with the background colour; the rest of the picture is untouched |
| `⌘V` | paste into the rectangle: a picture as the toolbar's **Paste** choice says (**Stretch**, the default, to the rectangle's size; **Fit** keeps its aspect, centred, the rest in the background colour; **1:1** unscaled at the rectangle's top-left, cut off at its edges), text in the **Text** colour, wrapped to the rectangle's width and as large as fits. With no rectangle a picture goes 1:1 at the top-left, text at the top-left in letters a tenth of the image's height, and what was pasted becomes the rectangle |
| `Esc` | clear the rectangle |
| `⌘Z` / `⇧⌘Z` (Ctrl on Linux) | undo / redo |
| `⌘S` or `F5` | Save: the original is first backed up locally (path printed), then overwritten in place in the same format (PNG, WebP, BMP, TIFF lossless; JPEG quality 95). A server without `posix-rename@openssh.com` cannot replace a file safely: Save is off there, Save copy works. While it runs nothing is edited; `Esc` stops it before the original is replaced |
| `⇧⌘S` | Save copy: `<name>.edited.<ext>` next to the original |
| `⌥⌘S` or `F7` | Save As: the edited image to a new file on this machine (never over an existing one) |

The second toolbar row holds the **Paste** choice and two colours: **Background** (white by
default; on an image with alpha, **transparent** too) and **Text** (black). A pasted picture's
own transparency blends over what is there. When the clipboard holds both text and a picture,
text wins (that is what the window is handed); Korean text — also as Finder copies a file name,
decomposed into jamo — is drawn as syllables with an installed font that has Hangul; without one
Hangul shows as boxes and the status line says Hangul may not render. Every cut and paste is one
undo step, saved by Save or Save copy like the other edits; a key held until it repeats counts
once, a second press pastes again. Pictures go through the clipboard as 8-bit RGBA, so a 16-bit
image's copy is 8-bit.

A remote image shows its size (and its Exif thumbnail) at once, then fills in as it is read —
JPEG and PNG as a partial picture. An image whose decoding needs more than 512 MiB of memory (or
a file over 512 MiB) first says how much it needs and what this machine has, and waits:
**Continue** or **Close** — and for a large decode **Hex view** too (`2` in the browser, `H`), its
bytes in the hex editor.
A file that does not decode opens in the hex editor, saying why.

Animated GIF/PNG/WebP and multi-page TIFF open read-only (first frame / page; `⌘C` still copies).
Closing with unsaved changes asks first. Save keeps the ICC profile and Exif where the format
holds them; anything it cannot keep (XMP, text chunks, IPTC, comments) is named in the window when
the image opens and again in the result line.

## Video and audio

Video plays in the window with sound; audio shows its waveform, a stereo file in two lanes (L and
R). Both are edited on the timeline: cut, copy and paste stretches of it, as the image window
edits a rectangle. Editing only changes the list of pieces, so playback and the paused frame show
the edit at once; ffmpeg renders it only when you Save (over the original, after keeping it — see
below) or Export (a new file next to it). A remote file plays without being downloaded: ffmpeg
reads it through sopen, which fetches only what it asks for, and the waveform of a long sound fills
in as it is read. If no audio device can be opened the video plays silently and says so.

| Key | |
|---|---|
| click on the timeline | move the playhead (and drop the selection) |
| drag on the timeline | select a stretch (the playhead follows, so you see the frame there) |
| `I` / `O` | the selection starts / ends at the playhead (to the end / from the start without one) |
| `←` / `→` | move the playhead 1 s (`⇧` 5 s; `⌥`/Alt one frame on video, 10 ms on audio) |
| `Space` | play / pause the edited timeline |
| `⌘C` (Ctrl+C on Linux) | copy the selection (to this window only, not the system clipboard) |
| `⌘X` | cut: the selection goes, the gap closes, and it can be pasted |
| `Delete` / `Backspace` | remove the selection without keeping it |
| `⌘V` | paste what was copied or cut at the playhead, or over the selection |
| `M` | audio, mono file: Mono → Stereo — left and right are both the one channel (`M` again: mono) |
| `Esc` | clear the crop rectangle, then the selection |
| `⌘Z` / `⇧⌘Z` | undo / redo |
| `⌘S` or `F5` | Save: the edit is rendered (video: H.264 + AAC; audio: its own codec) and written over the original in the same format — refused, nothing rendered, if the file changed since it was opened. The original is kept first: a local file as a local backup (path printed); a remote one on its own server, as a hidden hard link beside it (`.<name>.sopen-backup-<time>`, named in the result — not a copy: a program that still has the original open can change it; delete it when you no longer need it). Then the window plays the saved file. `Esc` stops a Save before the original is replaced |
| `⌥⌘S` or `F7` | Save As: render the edit into a new file on this machine (never over an existing one) |
| `E` | Export In–Out of the timeline (all of it without a selection) and the crop as `<stem>.cut-<in>-<out>.<ext>` next to the original, and show its remote path; **Cancel** (or `Esc`) stops it and publishes nothing |

On video, drag a rectangle on the picture to crop what Export writes (Save writes the edit only,
never a crop over the original). Cuts land on frame times, so
the sound never drifts from the picture. Save and Export keep the picture and every audio track;
what they cannot keep (subtitles, data streams, cover art, chapters, alpha, more than 8 bits where
this ffmpeg's libx264 has no 10-bit) is named in the window before you save or export and again in
the result line. Closing with unsaved edits asks first. A save or export whose ffmpeg makes no
progress for two minutes is stopped with an error; if sopen itself is killed during one, its ffmpeg
stops with it and the half-made file is removed the next time a media window or `sopen cut` starts.

## Binary files

```sh
sopen myserver:/srv/data.zip          # a dialog: Open in hex editor (Enter) · Save to… · Cancel (Esc)
sopen --hex myserver:/srv/notes.txt   # any file straight in the hex editor, no dialog
```

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

## Any other app: `--app`

```sh
sopen myserver:/srv/data.bin --app ImHex    # macOS: the app's name, as `open -a` takes it
sopen myserver:/srv/data.bin --app imhex    # Linux: the command on PATH
```

Fetches a copy, opens it in the named local app, and waits: Enter uploads your changes, `q`
discards them.

## Cut audio and video: `sopen cut`

```sh
sopen cut myserver:/tmp/talk.mp4 1:05 1:20.5 --crop 0,0,1280,720
sopen cut myserver:/tmp/a.mp3 3 9
```

`IN` and `OUT` are `SS`, `MM:SS` or `HH:MM:SS(.ms)`. The cut is re-encoded so it is exact (video:
H.264 + AAC; audio: the source's codec) and uploaded next to the original as
`<stem>.cut-<in>-<out>.<ext>`. The remote path of the result is printed.

## Split voice and music: `sopen stems`

```sh
sopen stems myserver:/tmp/talk.mp3    # → talk.voice.mp3 and talk.music.mp3 next to it
sopen stems ./clip.mp4                # a video → clip.voice.m4a and clip.music.m4a (AAC)
```

Separates the voice from the music on this machine's CPU — a 4-minute song takes about 3 minutes
on an Apple-silicon Mac, and memory peaks at about 6.5 GB however long the track is — and uploads
both as new files next to the original, never over anything (`-1`, `-2`, … when a name is taken,
the same number for both unless another program takes one of the names in between); their paths
are printed, voice first. Each keeps the original's audio codec, sample rate, channels and exact
length (a lossy codec such as AAC may decode up to one codec frame longer: its encoder pads the
last frame) — mono stays mono, more than two channels come out as stereo, downmixed without
clipping (a note says so); a video gives `.m4a` (AAC); only the first audio track is
separated. Progress goes to stderr, one line per 30-second piece:
`stems: piece 3/8, 61s elapsed, 102s remaining`.

The model (UVR-MDX-NET-Voc_FT) comes with the Homebrew formula, and sopen checks it is exactly
that file before it uses it; `--model PATH` or `SOPEN_STEMS_MODEL` names another copy of it.

## Safety

- Every write first re-checks the remote file: still the same regular file (not a symlink), same
  size and modification time, and — up to 256 MiB — the same content. If anything changed since
  sopen opened it, nothing is written; your edits are kept and where is printed (exit 3).
- Conflict detection is best-effort: sopen cannot lock the remote file, so a write that races the
  final rename is not detected.
- Writes go to a temporary file in the same directory, get the original's owner and permission
  bits, are flushed, and are renamed over the original atomically. If the owner cannot be kept,
  nothing is written.
- A save that fails for any reason moves your edited copy aside (`<file>.conflict-<time>`) and
  prints where. Opening the file again never overwrites an unsaved edit.
- Unsaved edits in the hex, image and media windows are kept on disk as well as in memory until you
  save or discard them. After a crash the next open offers them back — **Restore**, **Later** or
  **Discard** — once the file is checked to be the one they were made on. A save that was cut off
  is checked on the next open: sopen says whether it landed and never writes it twice.
- The local copies sopen creates (the cache, image, hex and media backups, hex undo files, kept
  edits) are readable by you only. A binary file saved with "Save to…" is created new, never over an existing file.
  A second sopen session on a file that is still open in the first gets its own copy, so neither
  overwrites the other's edits.
- Exports never overwrite: if the name exists, sopen adds `-1`, `-2`, ….

## Status

Under construction — see [`.prd/01-spec.md`](.prd/01-spec.md).

License: MIT (the font and the libraries below keep their own licences).

## Credits

The windows' font is PxPlus IBM VGA 8x16 (`assets/fonts/PxPlus_IBM_VGA_8x16.ttf`) from
[The Ultimate Oldschool PC Font Pack](https://int10h.org/oldschool-pc-fonts/) by VileR, licensed
under [CC BY-SA 4.0](https://creativecommons.org/licenses/by-sa/4.0/)
(`assets/fonts/LICENSE-PxPlus_IBM_VGA_8x16.txt`); `sopen --version` and `sopen --help` print the
credit too. The look follows PC Tools by Central Point Software.

Voice/music separation: UVR-MDX-NET-Voc_FT from Ultimate Vocal Remover (Anjok07 & aufr33, MIT —
credit requested by its authors); ONNX conversion and engine: k2-fsa/sherpa-onnx (Apache-2.0).
sherpa-onnx and the ONNX Runtime it brings are linked into the binary; their licences, and those
of what they include, are in [`THIRD-PARTY-NOTICES`](THIRD-PARTY-NOTICES), which the formula
installs with sopen. `sopen --version` and `sopen --help` print this credit too.
