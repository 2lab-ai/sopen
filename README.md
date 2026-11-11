# sopen

Open a file or folder on another machine as if it were local:

```sh
sopen myserver:/var/log/app.log    # text → Zed remote
sopen myserver:~/project           # folder → Zed remote, as a project
sopen myserver:/tmp/shot.png       # image → view, rectangle crop / pixelize, save back
sopen myserver:/tmp/talk.mp4       # video → play, rectangle crop, time cut → new file next to it
sopen myserver:/tmp/a.mp3          # audio → play, time cut
```

Same syntax as `scp`. Uses your `ssh` and `~/.ssh/config`; the remote needs nothing but sshd.

## Install

```sh
brew install 2lab-ai/tap/sopen-preview    # macOS (arm64, x86_64), Linux (x86_64, aarch64); pulls in ffmpeg
```

## What the remote needs

Only sshd with its SFTP subsystem (the default). sopen installs and runs nothing there. When you open
a folder or text file, Zed installs its own remote server on first use — that is Zed's behaviour,
not sopen's. Images and media are fetched over SFTP and handled locally.

The image and media window is in progress; this release fetches images, video and audio to the
local cache and prints the path.

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
