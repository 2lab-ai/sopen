# sopen

Open a file or folder on another machine as if it were local:

```sh
sopen myserver:~/project           # folder → sopen's file browser: two panels, Enter views any file
sopen myserver:/var/log/app.log    # text → VS Code remote (Remote - SSH), else Zed
sopen myserver:/tmp/shot.png       # image → view, rectangle crop / pixelize, cut / copy / paste, save back
sopen myserver:/tmp/talk.mp4       # video → play, cut / copy / paste, crop → save back or a new file
sopen myserver:/tmp/a.mp3          # audio → play, cut / copy / paste, mono → stereo → save back or a new file
sopen myserver:/tmp/a.zip          # anything else → asks: hex editor (default) or save it to a path
sopen --target <host> myserver:/tmp/a.mp3   # the same, on <host>'s screen instead of this one
```

Same syntax as `scp`. Uses your `ssh` and `~/.ssh/config`; the remote needs nothing but sshd.

## Install

```sh
brew install 2lab-ai/tap/sopen-preview    # macOS (arm64, x86_64), Linux (x86_64, aarch64); pulls in ffmpeg
```

## Requirements

- Media needs `ffmpeg` and `ffprobe` (the formula installs them).
- Text (and F4 in the file browser): local files open in VS Code (`code`) when it is installed;
  remote ones open in VS Code when its Remote - SSH extension is installed (sopen does not install
  it), else in Zed. Without VS Code, sopen uses Zed. On Linux under KDE Plasma, KWin would only
  flag the editor's window (often on another desktop), so sopen brings it to your current desktop
  and screen and activates it; `SOPEN_RAISE=0` turns that off.
- Linux: the ALSA runtime library (libasound2 / alsa-lib) — present on desktop Linux; sopen runs
  on the machine whose screen shows the file (the one you sit at, or the `--target`), not on the
  remote. The file browser, the image and media windows, the binary-file dialog and the hex window
  need a desktop session (X11 or Wayland); without one a folder opens in the editor and a binary
  file is fetched and its path printed.

## What the remote needs

Only sshd with its SFTP subsystem (the default). sopen installs and runs nothing there. When you open
a text file (or press F4 in the file browser), VS Code Remote - SSH or Zed installs its own remote server on first use — that is the editor's behaviour,
not sopen's. Folders are listed over SFTP. Images and media are fetched over SFTP and handled locally. (A `--target` machine is
different: it shows the file, so it needs sopen installed — see below.)

## Open on another machine's screen: `--target`

```sh
sopen myserver:/tmp/a.html                    # opens here, on the machine you typed it on
sopen --target <host> myserver:/tmp/a.html    # opens on <host>'s screen (sopen runs there, over ssh)
sopen --target <host> <host>:/tmp/a.html      # a file on <host> itself opens there as a local file
sopen --target <host> ./notes.txt             # a local file: <host> fetches it from this machine
sopen config target <host>                    # from now on `sopen SOURCE` opens on <host>
sopen --here myserver:/tmp/a.html             # …but this one here
sopen config                                  # where the config file is, and the target
sopen config target --unset                   # back to this machine
```

The config file is `~/.config/sopen/config.toml` (`$XDG_CONFIG_HOME/sopen/config.toml` when that is
set, `$SOPEN_CONFIG` overrides both) on macOS and Linux alike:

```toml
target = "<host>"
```

`--target` or `--here` (not both) decides one run; without them `SOPEN_TARGET=<host>` in the
environment comes first, then the config. `<host>` is `[user@]host` — an ssh alias or a host
name that `ssh <host>` reaches; a port or an IPv6 address goes into `~/.ssh/config` under an
alias. It needs sopen installed; sopen there gets `--here`, so it never sends the file on again.
A local file (or a `localhost:` one) is fetched by `<host>` from this machine by its short host
name, so that name must work for `ssh` on `<host>`.

The window opens on `<host>`'s desktop session (on Linux sopen finds it through
`systemctl --user`). On macOS an app started over ssh cannot take focus: sopen's own window
floats above the other windows until you click, type or scroll in it (or it gets focus), but the editor
(for text) opens behind the active app; under KDE Plasma it is brought to the screen you are on. From a terminal, Ctrl+C reaches `<host>` and closes the window
there; a sopen stopped without a terminal leaves the window on `<host>` open.

The exit code is `<host>`'s; `could not reach <host>` means ssh did not get there, `sopen is not
installed on <host>` (exit 127) means it has no sopen, and `sopen on <host> ended with status
N (signal n?)` means it most likely was interrupted there.

## Folders: the file browser

```sh
sopen myserver:/srv/app        # two panels on the folder, as in Total Commander
```

A folder opens in sopen's own window: two file panels side by side, each listing `..`, then the
folders, then the files (Name · Ext · Size · Date · Time; hidden files dimmed), with
`N Listed = N bytes` and `N Bytes Free` at the foot.

Every sopen window looks like PC Shell from the DOS PC Tools 5.5 / 6: the VGA text font, blue
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
| `F3` | view the file (as Enter) |
| `F4` | open the folder or file in VS Code / Zed |
| `Esc` / `F3` in a viewer | back to the list, the bar where it was |
| `F3` in a window of its own | close it, as `⌘W` (asks first when there are unsaved changes) |

Text shows read-only with syntax highlighting and line numbers, a page at a time (a file of any
size opens alike); images open in the image window, video and audio in the media window, anything
else in the hex editor — each with its own edits and Save, as below. Leaving a viewer with unsaved
changes asks first, as its own window does. A remote file is fetched with its progress in the
status row; `Esc` stops the fetch. The browser views only: it does not copy, move, delete or
rename.

Lines wrap by default: every line is shown whole, in rows as wide as the window, its number on its
first row. A row is broken by the measured width of its characters (Korean, CJK and emoji are
wider than a cell), after a space when the next word fits on the next row — Chinese and Japanese
between any two characters, never before closing punctuation such as `。` or `」` — and a tab goes
to the next stop of four cells. A row starting with a Hebrew or Arabic word still reads left to
right: its English stays as written, each right-to-left word reads right to left. The file is still read a chunk at a time — a line of hundreds of
megabytes too, from wherever you are in it. Inside a line longer than the window the status row
says how far into it you are (`line 7 (12 % of it) of 120`), and the scroll bar goes by bytes.

| Key in the text viewer | |
|---|---|
| `W` / `F2` | switch wrapping off (one row per line, `←` / `→` scroll sideways, 4096 bytes of a line shown) and on again; the F-key row says `Unwrap` or `Wrap` |
| `↑` / `↓`, `PgUp` / `PgDn`, the wheel | wrapped: move by rows (a page keeps one row); unwrapped: by lines |
| `Home` / `End` | wrapped, inside a long line: its first / last row first, then the start / end of the file; otherwise the start / end of the file |
| `⌘Home` / `⌘End` (`Ctrl` elsewhere) | the start / end of the file |

## Images

The window shows the image fitted; the mouse wheel or a pinch zooms, a trackpad scroll or a
right-drag pans (`F` fits again, `1` is 100 %). Drag a rectangle (handles adjust it, dragging
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
| `⌘S` or `F5` | Save: the original is first backed up locally (path printed), then overwritten in place in the same format (PNG, WebP, BMP, TIFF lossless; JPEG quality 95). A server without `posix-rename@openssh.com` cannot replace a file safely: Save is off there, Save copy works |
| `⇧⌘S` | Save copy: `<name>.edited.<ext>` next to the original |

The second toolbar row holds the **Paste** choice and two colours: **Background** (white by
default; on an image with alpha, **transparent** too) and **Text** (black). A pasted picture's
own transparency blends over what is there. When the clipboard holds both text and a picture,
text wins (that is what the window is handed); Korean text — also as Finder copies a file name,
decomposed into jamo — is drawn as syllables with an installed font that has Hangul; without one
Hangul shows as boxes and the status line says Hangul may not render. Every cut and paste is one
undo step, saved by Save or Save copy like the other edits; a key held until it repeats counts
once, a second press pastes again. Pictures go through the clipboard as 8-bit RGBA, so a 16-bit
image's copy is 8-bit.

Animated GIF/PNG/WebP and multi-page TIFF open read-only (first frame / page; `⌘C` still copies).
Closing with unsaved changes asks first. Save keeps the ICC profile and Exif where the format
holds them; anything it cannot keep (XMP, text chunks, IPTC, comments) is named in the window when
the image opens and again in the result line.

## Video and audio

Video plays in the window with sound; audio shows its waveform, a stereo file in two lanes (L and
R). Both are edited on the timeline: cut, copy and paste stretches of it, as the image window
edits a rectangle. Editing only changes the list of pieces, so playback and the paused frame show
the edit at once; ffmpeg renders it only when you Save (over the original, after a local backup)
or Export (a new file next to it). If no audio device can be opened the video plays silently and
says so.

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
| `⌘S` or `F5` | Save: the original is backed up locally (path printed), the edit is rendered (video: H.264 + AAC; audio: its own codec) and written over it in the same format — refused, nothing rendered, if the file changed since it was opened. Then the window plays the saved file |
| `E` | Export In–Out of the timeline (all of it without a selection) and the crop as `<stem>.cut-<in>-<out>.<ext>` next to the original, and show its remote path |

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
and type; nothing is downloaded yet. **Enter** fetches it and opens sopen's hex editor (the
default). **Save to…** downloads it to the path below the buttons — your download folder at first,
or the one "Browse…" picks — and never over an existing file: if the name is taken it says so and
offers the next free one (`data-1.zip`). **Esc** (or closing the window) downloads nothing.
Exceptions: Enter in the path field saves there (it never opens the hex editor); while a transfer
runs, Enter and Esc do nothing and closing the window waits for it to finish.

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
| `⌘S` or `F5` | Save: the original is first backed up locally (path printed), then overwritten in place, as image Save does. When it is refused, the window names where your edits were kept: if the file changed since it was opened, nothing is written and the edits are listed in a `.hexpatch-….txt` file; if the overwrite itself is refused (the remote changed), the staged edited file is kept |
| `⇧⌘S` | Save copy: `<name>.edited.<ext>` next to the original |

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

## Safety

- Every write first re-checks the remote file: still the same regular file (not a symlink), same
  size and modification time, and — up to 256 MiB — the same content. If anything changed since
  sopen fetched it, nothing is written; your edited copy is kept and its path printed (exit 3).
- Conflict detection is best-effort: sopen cannot lock the remote file, so a write that races the
  final rename is not detected.
- Writes go to a temporary file in the same directory, get the original's owner and permission
  bits, are flushed, and are renamed over the original atomically. If the owner cannot be kept,
  nothing is written.
- A save that fails for any reason moves your edited copy aside (`<file>.conflict-<time>`) and
  prints where. Opening the file again never overwrites an unsaved edit.
- The local copies sopen creates (the cache, image, hex and media backups, kept edits) are readable by you
  only. A binary file saved with "Save to…" is created new, never over an existing file.
  A second sopen session on a file that is still open in the first gets its own copy, so neither
  overwrites the other's edits.
- Exports never overwrite: if the name exists, sopen adds `-1`, `-2`, ….

## Status

Under construction — see [`.prd/01-spec.md`](.prd/01-spec.md).

License: MIT (the font below keeps its own licence).

## Credits

The windows' font is PxPlus IBM VGA 8x16 (`assets/fonts/PxPlus_IBM_VGA_8x16.ttf`) from
[The Ultimate Oldschool PC Font Pack](https://int10h.org/oldschool-pc-fonts/) by VileR, licensed
under [CC BY-SA 4.0](https://creativecommons.org/licenses/by-sa/4.0/)
(`assets/fonts/LICENSE-PxPlus_IBM_VGA_8x16.txt`); `sopen --version` and `sopen --help` print the
credit too. The look follows PC Tools by Central Point Software.
