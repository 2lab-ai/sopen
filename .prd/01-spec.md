---
title: sopen — spec v1
type: spec
tags: [sopen, spec]
---

# sopen — spec v1 (2026-11-11)

`sopen host:/path` opens any file or folder on any of my machines as if it were local:
folders and text go to Zed's remote mode, images and audio/video open in a small built-in
window where the few edits I actually need are one drag away.

## 1. Owner intent (verbatim, in order — the SSOT for scope)

1. "scp 처럼 그냥 `snote <host>:/tmp/a.sh` 로 리모트 파일을 그냥 로컬처럼 사용하고 싶은거야. … 디렉토리 폴더 프로젝트로 볼수 있게하고 파일 편집할수 있고 신택스 하일라이트 볼수 있고 이미지나 영상도 볼수 있는 … rust면 좋겠고 linux, macos에서 다 지원되고 brew install .../snote 로 설치하고 즉시 `snote <host>:/tmp/a.mp3` 를 쓸수 있으면 좋겠음. llmux 오픈소스 처럼 개발 해줬으면 좋겠고"
2. "먼저 만들가치가 있는지부터 리서치해줘"
3. "그냥 일종의 파일 만능툴이 필요함 … 내 머신들에서 아무렇게나 아무 머신에 이는 파일을 볼수 있고 편집할수 있는 기능 - mp3, wav, 아무 파일, 영상등 / mp3나 영상등은 즉시 원하는 구간만 잘라내는 기능 … ffmpeg로 하면 되잖아 / 이미지 편집은 irfanview에 있는 이미지 편집 기능 정도 / 파일은 그냥 텍스트 편집이랑 원하면 hex 편집기능정도까지"
4. "기존에 macshot 소스강 있는데 여기서 시작하는게 좋을것 같기도하고"
5. "너무 쓸데 없는 기능까지 요구 하는건 아닌가 싶기도 하고"
6. (after the research verdict "a router, not an editor") "그럼 이름이 snote가 아니라 sopen이지 그렇게 진행해줘 근데 이미지나 영상은 보고 바로 편집할수 있으면 좋겠는데 사각형으로 자르고 시간상에서 시간으로 컷 정도랑 이미지의 경우 사각형으로 지정해서 pixelize 정도"

Message 6 supersedes 3's "IrfanView 정도" with an exact list. Message 5 is a standing constraint:
anything not in the table below is out of scope until usage says otherwise (§10).

## 2. Intent → acceptance

| # | Owner intent | Acceptance (observable) |
|---|---|---|
| A1 | `sopen host:/path`, scp syntax, any machine in `~/.ssh/config` | `sopen <alias>:/abs/file`, `sopen <alias>:rel/to/home`, `sopen user@host:/x` all open; aliases, ProxyJump, agent behave exactly as `ssh <alias>` does (system ssh is used). Local paths (`sopen ./x.png`) open too. |
| A2 | Folder as a project, text edit, syntax highlight | Folder or text file → Zed opens the remote path (`zed ssh://…` on macOS, `zeditor`/`zed` on Linux). Save in Zed writes the remote file (Zed owns this). |
| A3 | Image: view + rectangle crop + rectangle pixelize | Built-in window shows the image; drag draws a rectangle in image pixels; Crop and Pixelize act on it; Undo; Save writes back to the remote file. |
| A4 | Video: view + rectangle crop + time cut | Built-in window plays the video with sound; scrub timeline; set In/Out; drag a crop rectangle; Export writes `<stem>.cut-<in>-<out>.<ext>` next to the original on the remote. |
| A5 | Audio (mp3/wav): view + time cut | Same window: waveform, play, In/Out, Export. |
| A6 | "아무 파일" | Any other type is fetched to the local cache and its path printed; `--app <App>` opens it in a named local app and uploads it back when the user presses Enter in the terminal. (This is the hex-editor path: `--app ImHex`.) |
| A7 | Rust, macOS + Linux, `brew install …` and use immediately | `brew install 2lab-ai/tap/sopen-preview` on macOS (arm64, x86_64) and Linux (x86_64, aarch64) installs `sopen` and the `ffmpeg` it needs; the first `sopen host:/a.mp3` works with no further setup. |
| A8 | Developed like llmux | Public repo `2lab-ai/sopen`, MIT; `ci.yml` (macOS + Linux), `preview.yml` (every main push → prerelease + tap bump), `release.yml` (`v*` tag = owner gate); `AGENTS.md` SSOT; `.prd/` spec. |

## 3. Routing

| Kind | Detected by | Opens in |
|---|---|---|
| directory | remote `stat` | Zed remote (`ssh://[user@]host[:port]/abs`) |
| text | extension list, else first 8 KiB has no NUL and is valid UTF-8 | Zed remote |
| image | `png jpg jpeg webp bmp gif tif tiff` | image window |
| video | `mp4 mov m4v mkv webm avi` (and ffprobe says it has a video stream); `ts` only when it is not text and ffprobe sees a video stream (`.ts` is also TypeScript) | media window |
| audio | `mp3 wav flac m4a aac ogg opus` | media window |
| other | everything else | fetch → print cache path (A6) |

- Zed not found (`zed` / `zeditor` on PATH, macOS also `/Applications/Zed.app/Contents/MacOS/cli`): text → fetch, run `$VISUAL`/`$EDITOR` in the terminal, upload on exit if changed; directory → error that names the missing editor.
- Remote relative paths resolve against the remote home (SFTP `realpath`), as scp does.

## 4. Remote layer

- Transport = the system `ssh` binary (so `~/.ssh/config` is honoured completely), one multiplexed master per host
  (`ControlMaster=auto`, `ControlPath=<state>/cm-%C`, `ControlPersist=60s`).
- File operations = SFTP over that session (`openssh` + `openssh-sftp-client` crates): `stat`, `realpath`, read, write.
  SFTP keeps the code independent of the remote OS (macOS BSD tools vs GNU) — no remote shell parsing.
- Nothing is installed on the remote. Only sshd's SFTP subsystem is required. (Zed installs its own server when the
  owner opens folders/text; that is Zed's behaviour, documented in the README.)

## 5. Writes back (conflict-safe, never silent)

- Every fetch records the remote `(size, mtime)` and the SHA-256 of the fetched bytes. The write target is the
  `realpath` resolved at fetch time.
- Before any write: `lstat` the target. If it is gone, now a symlink or not a regular file, or its size or mtime
  differ — or, up to 256 MiB, its content hash differs (above that: size and mtime only, and the result says so) —
  refuse, keep the edited local copy, print its path. No overwrite. A server that leaves out size, mtime,
  permissions, type or owner gets no write.
- Write = upload to `.<name>.sopen-<random>` (created exclusively) in the same remote directory → give it the
  target's owner (refuse when that is impossible) and permission bits → `fsync@openssh.com` if offered → re-check →
  `posix-rename@openssh.com` over the target (plain rename if not offered). Only a temp this write created is ever
  removed; one that cannot be removed is named. The next baseline is the staged temp — its size and mtime as stat'ed
  right before the rename, plus the uploaded bytes' hash — never the target's stat after the rename; if the target
  differs from it right after the rename, that is reported ("saved, but the remote changed right after") and the next
  save refuses. A server without `posix-rename@openssh.com` cannot replace a file in one step: Save is refused before
  anything is uploaded, and Save copy (a new sibling) is offered.
- Conflict detection is best-effort: it catches changes made before you save (size, mtime, and content hash up to
  256 MiB); it cannot lock the remote file, so a write racing the final rename is not detected.
- Any save that does not land moves the edited copy aside to `<copy>.conflict-<UTC stamp>` and prints that path. A
  fetch never overwrites a cached copy holding unsaved edits: it moves it aside first.
- Local copies are the owner's only: cache, edit, backup and export directories are created 0700; fetched copies,
  staged and kept image edits, backups and edits moved aside are 0600. A staged or kept image edit gets a new name
  (`<stem>.<tag>-<UTC stamp>-<random>.<ext>`), created exclusively — never over or through an existing file.
- The cache's fixed roots are dot-named — `<cache>/.backup`, `<cache>/.edits` (staged and kept image edits),
  `<cache>/.exports` (local exports before they are published), `<cache>/.meta` — and a host label never starts with
  `.`, so no mirrored host lands in one (a host named `backup` is `<cache>/backup/…`, plain cache). Roots an older
  build made without the dot are just cache: never read or written.
- What sopen knows about a cached copy lives outside the mirrored tree, where no remote path can reach it:
  `<cache>/.meta/record/<host>/<abs path>` (the SHA-256 the copy held when last fetched or saved) and
  `<cache>/.meta/lock/<host>/<abs path>` (host labels never start with `.`). Records from older versions, kept next
  to the copy, are not read: such a copy counts as possibly edited and is moved aside on the next fetch.
- One session at a time works on a cached copy: a session (`--app`, the terminal editor, a window) holds an advisory
  lock on it until it ends; a second session on the same file — or any session where the copy cannot be locked —
  fetches into its own `<stem>.session-<random>.<ext>` (the copy itself created exclusively) and says so in one
  line. The next session that claims the shared copy revisits ended sessions' copies (their lock is free): one
  holding unsaved edits is moved aside and said, a clean one is removed; a running session's copy is never touched.
  Records and edits moved aside are never written or chmodded through a symlink.
- Nothing waits forever: ssh keepalives (10 s × 3), 30 s per SFTP request, 60 s without transfer progress. Only an
  SFTP status reply proves a rename, link or exclusive create did not happen; a timeout or a lost reply (connection
  closed, EOF, garbled reply) is "outcome unknown — check <path>", never "nothing was written", and a temp or export
  that may have been created is named, not removed.
- Image Save (Cmd/Ctrl+S) overwrites the original **after** copying the fetched original to the local backup
  `<cache>/.backup/<host>/<abs path>.<UTC stamp>` and printing that path. "Save copy" writes `<stem>.edited.<ext>`
  next to the original instead. Media Export never overwrites (sibling file; `-N` suffix if the name exists): it is
  published by hard link from a temp, or — where links are unavailable — written into a name created exclusively.
- Live receipts run only on files created under the remote `/tmp` for that purpose.

## 6. Image window (A3)

- Fit-to-window view, zoom (wheel / pinch), pan; status bar shows image size, rectangle `x,y w×h`.
- Drag = rectangle in image pixels (handles to adjust). `C` Crop, `P` Pixelize (block size, default 16 px), Undo/Redo
  (Cmd/Ctrl+Z / Shift+Cmd/Ctrl+Z), Cmd/Ctrl+S Save, Save copy, Esc clears the rectangle.
- Encoding keeps the format: PNG lossless, JPEG quality 95, WebP lossless, BMP, TIFF. Animated GIF opens read-only
  (frame 1 shown, Save disabled with a reason).
- Never lose data silently: Save keeps the ICC profile and Exif where the format holds them; metadata it does not
  carry (XMP, text chunks, IPTC, comments) or an encoder refuses is named when the image opens and in the result.

## 7. Media window (A4, A5)

- Probe with `ffprobe` (duration, streams, size). `ffmpeg`/`ffprobe` are resolved at runtime (`SOPEN_FFMPEG`, then PATH;
  the brew formula depends on `ffmpeg`).
- Video: playback in the window with sound (ffmpeg subprocess decode → frames; audio → `cpal`; audio clock master),
  timeline scrub, `I`/`O` set In/Out, drag = crop rectangle in source pixels, Space play/pause, Export.
- Audio: waveform (downsampled PCM), same transport and In/Out, Export.
- Export: `ffmpeg -ss In -to Out -i src [-vf crop=w:h:x:y] …` → re-encode (H.264 + AAC for video; the source codec for
  audio) so cuts are exact; progress shown; result uploaded as a sibling (§5).
- Export maps streams explicitly: the first picture stream and every audio track (one, for one-track containers);
  >8-bit sources stay 10-bit where libx264 has it; colour tags are carried. What is not kept (subtitles, data,
  attachments, cover art, alpha, depth) is named in the window before Export and in the result line.
- `sopen cut <target> <in> <out> [--crop x,y,w,h]` = the same export, headless (scripting + CI receipts).
- Renderer / playback details fixed by the spike (§11).

## 8. Usage log

One JSON line per run in `<state>/usage.jsonl`: `ts, host_hash, kind, ext, bytes, fetch_ms, action (open|save|export|cut|discard), conflict`.
Local only, never sent anywhere. Exists for §10.

## 9. Out of scope (until §10 says otherwise)

Own text editor / tree / highlighting (Zed does it), hex editor (`--app`), filters beyond crop/pixelize, frame-accurate
copy without re-encode, streaming playback before download completes, directory sync / mounts, file watching,
Windows, a `.app` bundle or cask, any agent on the remote, macshot (Swift, macOS-only, GPL-3 — incompatible with A7/A8).

## 10. Two-week review (from 2026-11-xx first preview)

From `usage.jsonl` across machines: < 5 runs/week, or ≥ 80 % text/dir → archive, keep a `zed ssh://` alias.
A conflict refusal or failed write → fix before anything else. Large-media waits (≥ 500 MB or fetch ≥ 30 s, 3+ times)
→ consider streaming. The owner naming the same missing edit 3 times → that one edit, nothing more.

## 11. Spike results (2026-11-11, throwaway spike, ~1.4k LOC)

- **Toolkit:** eframe/egui 0.37 — one dependency for window, input and texture upload, no system UI
  libraries to link. Renderer: wgpu by default, glow (OpenGL) compiled in as the fallback — chosen with
  `SOPEN_RENDERER`, and used automatically when wgpu does not start. Both rendered the same frames on macOS.
- **Playback in the window:** two ffmpeg subprocesses from the play position — video as RGBA frames
  (decoded ≤ 1280 wide, forced to a constant frame rate so a frame's time is its index over the rate),
  audio as f32 PCM at the output device's rate — feeding `cpal` 0.18 through an `rtrb` ring. Audio is the
  master clock (frames the device consumed, minus its output latency); video frames are shown when due
  and dropped when late. 720p30 for 5 s: 152 frames shown, 0 dropped. No audio device → the clock falls
  back to the wall clock and the window says so.
- **Scrub:** one ffmpeg run per requested frame, latest request wins.
- **Export:** input-side `-ss`/`-to` + re-encode is frame-exact (duration = Out − In).
- **Linux build:** `cpal` links ALSA (`libasound2-dev` in CI); X11, Wayland, EGL, Vulkan and fontconfig are
  loaded at runtime.
- **macOS:** a window started from a terminal can come up behind it — sopen asks for focus on its first
  frames. A window can screenshot itself (`ViewportCommand::Screenshot`), which is how window receipts are
  taken (AGENTS.md "Receipts").
- **Measured in the spike (2026-11-11):** time to first frame — image 128–180 ms warm (541 ms first launch),
  video ~150 ms, audio waveform ~140 ms; A/V drift with a real-time null sink mean −11 ms / max 16.6 ms; a
  2.5–4.0 s cut = 45 frames starting at 00:00:02.500; 0.03–0.11 s per 2–4 s export; on Arch, Homebrew's
  pkg-config misses `/usr/lib/pkgconfig` (set `PKG_CONFIG_PATH` to build).
