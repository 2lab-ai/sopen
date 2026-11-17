# sopen

Open a file or folder on another machine as if it were local:

```sh
sopen myserver:/var/log/app.log    # text → Zed remote
sopen myserver:~/project           # folder → Zed remote, as a project
sopen myserver:/tmp/shot.png       # image → view, rectangle crop / pixelize, save back
sopen myserver:/tmp/talk.mp4       # video → play, rectangle crop, time cut → new file next to it
sopen myserver:/tmp/a.mp3          # audio → play, time cut
sopen --target <host> myserver:/tmp/a.mp3   # the same, on <host>'s screen instead of this one
```

Same syntax as `scp`. Uses your `ssh` and `~/.ssh/config`; the remote needs nothing but sshd.

## Install

```sh
brew install 2lab-ai/tap/sopen-preview    # macOS (arm64, x86_64), Linux (x86_64, aarch64); pulls in ffmpeg
```

## Requirements

- Media needs `ffmpeg` and `ffprobe` (the formula installs them).
- Linux: the ALSA runtime library (libasound2 / alsa-lib) — present on desktop Linux; sopen runs
  on the machine whose screen shows the file (the one you sit at, or the `--target`), not on the
  remote. The image and media windows need a desktop session (X11 or Wayland).

## What the remote needs

Only sshd with its SFTP subsystem (the default). sopen installs and runs nothing there. When you open
a folder or text file, Zed installs its own remote server on first use — that is Zed's behaviour,
not sopen's. Images and media are fetched over SFTP and handled locally. (A `--target` machine is
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
floats above the other windows for a moment (until 1.5 s pass or you click or type in it), but
Zed opens behind the active app. From a terminal, Ctrl+C reaches `<host>` and closes the window
there; a sopen stopped without a terminal leaves the window on `<host>` open.

The exit code is `<host>`'s; `could not reach <host>` means ssh did not get there, `sopen is not
installed on <host>` (exit 127) means it has no sopen, and `sopen on <host> was stopped by
signal N` means it was interrupted there.

## Images

The window shows the image fitted; the mouse wheel or a pinch zooms, a trackpad scroll or a
right-drag pans (`F` fits again, `1` is 100 %). Drag a rectangle (handles adjust it, dragging
inside moves it), then:

| Key | |
|---|---|
| `C` | crop to the rectangle |
| `P` | pixelize the rectangle (block size slider, 16 px by default) |
| `Esc` | clear the rectangle |
| `⌘Z` / `⇧⌘Z` (Ctrl on Linux) | undo / redo |
| `⌘S` | Save: the original is first backed up locally (path printed), then overwritten in place in the same format (PNG, WebP, BMP, TIFF lossless; JPEG quality 95). A server without `posix-rename@openssh.com` cannot replace a file safely: Save is off there, Save copy works |
| `⇧⌘S` | Save copy: `<name>.edited.<ext>` next to the original |

Animated GIF/PNG/WebP and multi-page TIFF open read-only (first frame / page). Closing with unsaved
changes asks first. Save keeps the ICC profile and Exif where the format holds them; anything it
cannot keep (XMP, text chunks, IPTC, comments) is named in the window when the image opens and
again in the result line.

## Video and audio

Video plays in the window with sound; audio shows its waveform. Click or drag the timeline to
seek, `Space` plays/pauses, `←`/`→` step 1 s (`⇧` 5 s), `I` / `O` set In and Out. On video, drag
a rectangle to crop. `E` exports In–Out (and the crop) as a new file next to the original,
exactly like `sopen cut`, and shows its remote path. If no audio device can be opened the video
plays silently and says so. Export keeps the picture and every audio track; what it cannot keep
(subtitles, data streams, cover art, alpha, more than 8 bits where this ffmpeg's libx264 has no
10-bit) is named in the window before you export and again in the result line.

## Any other file: `--app`

```sh
sopen myserver:/srv/data.bin --app ImHex    # macOS: the app's name, as `open -a` takes it
sopen myserver:/srv/data.bin --app imhex    # Linux: the command on PATH
```

Fetches a copy, opens it in the named local app, and waits: Enter uploads your changes, `q`
discards them. Files with no better route are fetched to the cache and their path is printed.

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
- Exports never overwrite: if the name exists, sopen adds `-1`, `-2`, ….

## Status

Under construction — see [`.prd/01-spec.md`](.prd/01-spec.md).

License: MIT
