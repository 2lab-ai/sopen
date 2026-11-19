---
title: sopen — spec v1
type: spec
tags: [sopen, spec]
---

# sopen — spec v1 (2026-11-11)

`sopen host:/path` opens any file or folder on any of my machines as if it were local:
a folder opens in sopen's own file browser — two panels, and Enter shows any file in a
built-in viewer (text highlighted, images, video and audio, anything else as hex); text goes to
VS Code when it is installed (remote ones only with its Remote - SSH extension), otherwise
Zed's remote mode; images and audio/video open in a small built-in window where the few edits I
actually need are one drag away, and any other file (a zip, a binary) asks first — open it in a
built-in hex editor, or save it where I choose — on the machine I typed it on, or with
`--target` on another machine's screen.

## 1. Owner intent (verbatim, in order — the SSOT for scope)

1. "scp 처럼 그냥 `snote <host>:/tmp/a.sh` 로 리모트 파일을 그냥 로컬처럼 사용하고 싶은거야. … 디렉토리 폴더 프로젝트로 볼수 있게하고 파일 편집할수 있고 신택스 하일라이트 볼수 있고 이미지나 영상도 볼수 있는 … rust면 좋겠고 linux, macos에서 다 지원되고 brew install .../snote 로 설치하고 즉시 `snote <host>:/tmp/a.mp3` 를 쓸수 있으면 좋겠음. llmux 오픈소스 처럼 개발 해줬으면 좋겠고"
2. "먼저 만들가치가 있는지부터 리서치해줘"
3. "그냥 일종의 파일 만능툴이 필요함 … 내 머신들에서 아무렇게나 아무 머신에 이는 파일을 볼수 있고 편집할수 있는 기능 - mp3, wav, 아무 파일, 영상등 / mp3나 영상등은 즉시 원하는 구간만 잘라내는 기능 … ffmpeg로 하면 되잖아 / 이미지 편집은 irfanview에 있는 이미지 편집 기능 정도 / 파일은 그냥 텍스트 편집이랑 원하면 hex 편집기능정도까지"
4. "기존에 macshot 소스강 있는데 여기서 시작하는게 좋을것 같기도하고"
5. "너무 쓸데 없는 기능까지 요구 하는건 아닌가 싶기도 하고"
6. (after the research verdict "a router, not an editor") "그럼 이름이 snote가 아니라 sopen이지 그렇게 진행해줘 근데 이미지나 영상은 보고 바로 편집할수 있으면 좋겠는데 사각형으로 자르고 시간상에서 시간으로 컷 정도랑 이미지의 경우 사각형으로 지정해서 pixelize 정도"
7. "그리고 이거 기능 추가해줘 / soepn <host-a>:/tmp/a.html (현재 머신에서 저 파일 오픈) / sopen --target <host-b> <host-a>:/tmp/a.html / 명백하지 진행 ㄱ"
8. "그리고 sopen 설정으로 디폴트 타겟 설정할 수 있도록해줘"
9. "mp4랑 html 잘 열리는데 (<host-a>에서 <host-a>파일을 <host-b>우로) (내 작업 디폴트 터미널이 <host-b>임), zip파일은 그냥 다운만 받아지는데 디폴트는 헥스에디터로 여는거야 바이너리들은, 다이얼로그 뜨고 원하는 위치로 저장혹은 헥스에디터 여는것 이게 좋겠음"
10. "sopen이 vscode가 있으면 먼저 vscode로 열개해줘 vscode -> zeditor"
11. (on KDE Plasma, after `sopen .` printed "opened … in VS Code") "vscode 안나옴", then "해당 앱이 현재 화면으로 이동시켜줘"
12. (2026-11-19) "다음처럼 개선해줘. 1. 해당 앱이 현재 화면으로 이동시켜줘. 2. 디폴트로 폴더를 고르면 토탈 커맨드 같은 프로그램이 떠야함 거기서 파일별로 엔터 누르면 해당 프로그램에서 디폴트 뷰어가 다 동작해야함 신택스 하이라이트 들어간 파일 뷰어 이미지 뷰어 영상 뷰어 헥스뷰어 모두 3. 이미지 편집은 ctrl+x ctrl+v작동하도록 해줘. 선택은 선택이고 선택하고 ctrl+c나 ctrl+x(자르기-해당부분만 배경색으로 잘리고 그림은 클리보드) ctrl+v(현재 선택된 영역으로 클립보드 내용 붙여넣기. 이미지면 그냥 스트레츠로 붙여넣고(고를수 있음), 텍스트면 텍스트채로 붙여줘). 픽셀라이즈는 ㅇㅋ 4. 디렉토리 뷰어 + 뷰어 + 헥스에디터 이거 pctools 레퍼런스 찾고 pctools 냄새 나게 해줘."

Message 6 supersedes 3's "IrfanView 정도" with an exact list. Message 5 is a standing constraint:
anything not in the table below is out of scope until usage says otherwise (§10). Messages 7–8 add
where a file opens — this machine, or another machine's screen (§4a); they add no edit. Message 9
replaces A6's "fetched to the cache and its path printed" for binary files with a dialog — the hex
editor (the default) or save to a chosen path — and takes the hex editor off §9's list: it is
message 3's "원하면 hex 편집기능정도까지", built in (§6a). Message 10 puts VS Code before Zed for
folders and text (A2, §3); it adds no edit. Message 11 is a bug report: on KDE Plasma the editor sopen
opened never came up — KWin refused its activation and only flagged the window, on another desktop —
and the window is to come to the screen the owner is on, not the owner to it (A2, §3). It adds no
edit. Message 12's item 3 adds copy, cut and paste through the system clipboard to the image window
(A3, §6): the rectangle stays the selection, cut leaves the background colour, paste lays a picture
(stretched by default, or fitted, or 1:1) or text into the rectangle, and pixelize is unchanged. Its
item 1 is message 11's request in the same words (A2, §3). Its item 2 supersedes message 10 for
folders only: a folder opens in sopen's own file browser (A11, §3a), where Enter on a file shows the
viewer for its kind in the same window — a read-only text viewer with syntax highlighting (taking
"own highlighting" off §9), and the existing image, media and hex windows; text opened directly
still goes to VS Code / Zed. The browser views: copy, move, delete and rename are not part of it
(§9), and the viewers keep exactly their edits. Its item 4 asks for the PC Tools look: the browser
is laid out where PC Shell puts things (§3a); the skin itself (colours, VGA font, frames) comes
after.

## 2. Intent → acceptance

| # | Owner intent | Acceptance (observable) |
|---|---|---|
| A1 | `sopen host:/path`, scp syntax, any machine in `~/.ssh/config` | `sopen <alias>:/abs/file`, `sopen <alias>:rel/to/home`, `sopen user@host:/x` all open; aliases, ProxyJump, agent behave exactly as `ssh <alias>` does (system ssh is used). Local paths (`sopen ./x.png`) open too. |
| A2 | Text edit, syntax highlight; VS Code first (msg 10; folders: msg 12, A11) | Text file → VS Code when `code` is found; a remote one only when VS Code's Remote - SSH extension is installed (`code --folder-uri` / `--file-uri vscode-remote://ssh-remote+[user@]host/<percent-encoded abs>` by the kind sopen detected, since a bare `--remote` path's kind is guessed from its name; the extension is never installed by sopen), else Zed with `VS Code has no Remote - SSH extension; opening in Zed` on stderr. Without VS Code, Zed (`zed ssh://…` on macOS, `zeditor`/`zed` on Linux). A VS Code launch failure is reported, not followed by Zed. The editor owns Save. The file browser's F4 hands a folder or file to the same editor the same way (A11). On Linux under KDE Plasma (msg 11) the editor's window that asked for attention is brought to where the user is — the active window's desktop and screen — and activated (§3). |
| A3 | Image: view + rectangle crop + rectangle pixelize + clipboard copy / cut / paste (msg 12) | Built-in window shows the image; drag draws a rectangle in image pixels; Crop and Pixelize act on it; Cmd/Ctrl+C copies it (the whole image without one) to the system clipboard as a picture, Cmd/Ctrl+X copies it and leaves it in the background colour, Cmd/Ctrl+V pastes the clipboard's picture (Stretch by default, Fit or 1:1) or text into it; Undo; Save writes back to the remote file. |
| A4 | Video: view + rectangle crop + time cut | Built-in window plays the video with sound; scrub timeline; set In/Out; drag a crop rectangle; Export writes `<stem>.cut-<in>-<out>.<ext>` next to the original on the remote. |
| A5 | Audio (mp3/wav): view + time cut | Same window: waveform, play, In/Out, Export. |
| A6 | "아무 파일"; binary files open in a hex editor by default, after a dialog (msg 9) | Any other type (a zip, any binary) opens a small dialog before anything is downloaded — name, size, type when the extension says it: Enter = "Open in hex editor" (the default: fetched to the cache, then sopen's hex window, §6a); "Save to…" downloads it to a path the owner picks (the download folder by default, never over an existing file); Esc or closing it downloads nothing. `sopen --hex SOURCE` opens any file straight in the hex window. `--app <App>` still opens it in a named local app and uploads it back when the user presses Enter in the terminal. With no display (Linux over ssh) the file is fetched and its path printed, and one line says why. |
| A7 | Rust, macOS + Linux, `brew install …` and use immediately | `brew install 2lab-ai/tap/sopen-preview` on macOS (arm64, x86_64) and Linux (x86_64, aarch64) installs `sopen` and the `ffmpeg` it needs; the first `sopen host:/a.mp3` works with no further setup. |
| A8 | Developed like llmux | Public repo `2lab-ai/sopen`, MIT; `ci.yml` (macOS + Linux), `preview.yml` (every main push → prerelease + tap bump), `release.yml` (`v*` tag = owner gate); `AGENTS.md` SSOT; `.prd/` spec. |
| A9 | `sopen SOURCE` opens here; `sopen --target <host> SOURCE` opens it on `<host>` (msg 7) | `sopen <host-a>:/tmp/a.html` opens on the machine it was typed on, as before. `sopen --target <host-b> <host-a>:/tmp/a.html` opens it on `<host-b>`'s screen: the same window or editor, started by `<host-b>`'s own sopen (on macOS sopen's window floats above the others until it is used; the editor opens behind the active app, §4a); exit code is `<host-b>`'s; `could not reach <host-b>` when ssh cannot get there; `sopen is not installed on <host-b> …` (exit 127) when it has no sopen. A file on `<host-b>` itself opens there as a local file; a local file reaches `<host-b>` as `<this machine>:<path>`. |
| A10 | Default target in sopen's settings (msg 8) | `sopen config target <host-b>` writes `target = "<host-b>"` to the config file (§4a); from then on `sopen SOURCE` opens on `<host-b>`, `--here` opens on this machine anyway, `--target` picks another; `sopen config` shows the file and the target; `sopen config target --unset` removes it. A config sopen cannot read is an error naming the file, never ignored. |
| A11 | A folder opens in a Total Commander-like program; Enter on a file runs its default viewer there (msg 12) | `sopen host:/dir` opens sopen's file browser (§3a): two panels on the folder, folders first; Tab switches panels, Enter enters a folder, Backspace goes up, a letter jumps to the next name with it; Enter on a file shows it in the same window — text highlighted with line numbers, an image in the image window, video/audio in the media window, anything else in the hex window — and Esc returns to the list with the bar where it was; F4 opens the entry in VS Code / Zed (A2's choice). An edited image or hex file is never dropped unasked: leaving asks as its own window does. `--app` still opens a file in a named app; a folder with `--app` / `--hex` is an error, as before. |

## 3. Routing

| Kind | Detected by | Opens in |
|---|---|---|
| directory | remote `stat` | sopen's file browser (§3a, msg 12); F4 there opens VS Code remote (`code --folder-uri vscode-remote://ssh-remote+[user@]host/abs`), else Zed remote (`ssh://[user@]host[:port]/abs`) |
| text | extension list, else first 8 KiB has no NUL and is valid UTF-8 | VS Code remote (`code --file-uri vscode-remote://…`), else Zed remote |
| image | `png jpg jpeg webp bmp gif tif tiff` | image window |
| video | `mp4 mov m4v mkv webm avi` (and ffprobe says it has a video stream); `ts` only when it is not text and ffprobe sees a video stream (`.ts` is also TypeScript) | media window |
| audio | `mp3 wav flac m4a aac ogg opus` | media window |
| other | everything else | the binary dialog: hex window (default) or Save to… (A6, §6a) |

- VS Code is found as `code` on PATH, then on macOS `/Applications/Visual Studio Code.app/Contents/Resources/app/bin/code` or the same path under `~/Applications`. Remote - SSH counts as installed when a directory `ms-vscode-remote.remote-ssh-<version>` is in `$VSCODE_EXTENSIONS` (when non-empty) or `~/.vscode/extensions` and is not a key of the `.obsolete` JSON object there; VS Code is not started to check. Without a usable VS Code, Zed is found as `zed` / `zeditor` on PATH, macOS also `/Applications/Zed.app/Contents/MacOS/cli`. Neither found: text → fetch, run `$VISUAL`/`$EDITOR` in the terminal, upload on exit if changed; F4 in the browser says in its status row that neither was found. A folder opens in an editor only where the browser cannot open (no display, §3a); then neither found is an error that names the missing editors.
- On Linux, when `XDG_CURRENT_DESKTOP` names KDE (any case, anywhere in its colon-separated list) and
  `SOPEN_RAISE` is not `0`, sopen brings the editor's window to the user after the hand-off — the CLI's own and the
  file browser's F4 alike (msg 11).
  The editor asks KWin to activate its window, but a CLI started by sopen carries no activation token,
  so KWin only marks the window as demanding attention — maybe on another desktop or screen. sopen
  starts a detached `sopen --raise-window code|zed --raise-caption=<name>` (a session of its own, no
  input or output; sopen and a `--target` run's ssh session end without waiting for it), where
  `<name>` is the opened folder's or file's last path component. It writes one script for this open
  — a file only the user can read (0600) in `$XDG_RUNTIME_DIR`, else the temp dir — then loads it
  into KWin once and runs it once (`org.kde.KWin` `/Scripting` `loadScript`, then
  `/Scripting/Script<id>` `run`). About 3 s later it unloads it (`unloadScript`) and removes the
  file. It talks to KWin through `busctl --user`, else `qdbus6` or `qdbus`.
  - Which window: one whose resource class is the editor's (`code`; `zed`, `dev.zed.Zed` or
    `dev.zed.Zed-<channel>`), and the first of:
    - (a) one that demands attention and shows `<name>` in its title: newest first at load, or
      when its title changes while the script is loaded;
    - (b) one whose demand for attention starts while the script is loaded;
    - (c) one that appears while the script is loaded (a folder not open yet opens a new window).
  - A window already demanding attention at load whose title shows another name asked for
    something else, and is left alone. `<name>` reaches the script as one escaped string literal.
  - Once one window is brought, the script disconnects every handler and does nothing more.
  - The script also keeps its own deadline: the hold plus 0.5 s, measured from its run. Every
    handler and the load-time check test it first. Once it has passed, the script disconnects
    every handler and does nothing. KWin's script timer (`QTimer`) stops it at that moment; without
    the timer, the first signal after the deadline does. So a script whose unload never reaches KWin
    cannot act later. The unload is only cleanup.
  - Bringing it: the window goes to where the user is, un-minimized (a window on all desktops stays
    so), and is activated. Where the user is means the active window's desktops (desktop objects:
    their names need not be unique) and screen. Without an active window, or when it is on all
    desktops, the current desktop and the active screen are used. The user's current desktop is
    never changed, and no other window is touched.
  - Any failure ends it without a word: no client, a call not answered within 0.5 s, a refused or
    failed load or run (then it is unloaded at once). Elsewhere (macOS, GNOME, any other window
    manager) nothing changes.
- Remote relative paths resolve against the remote home (SFTP `realpath`), as scp does.

## 3a. The file browser (A11, msg 12)

A folder opens in a sopen window titled `host:/abs` (the local path for a local folder). Its rows
are where PC Shell puts them: the program's name on the top row; two file panels side by side,
each in a frame with its folder (`host:/abs`) centred in the top border and the active panel's
frame drawn heavier; the status row (a fetch's progress, the last result, else what the keys do);
the bottom row lists the F-keys that work there and nothing else (the list: 3 View, 4 Edit; a
viewer: 3 Exit, 4 Edit). All drawing of those rows, the frames, the bar and every colour sits in
one module (`gui::chrome`), so the PC Tools skin (msg 12 item 4: palette, VGA font, frames)
replaces that module alone.

- **A panel** lists `..` (not at `/`), then folders and links to folders, then everything else,
  each by name regardless of case. Columns: Name · Ext (a file's extension; a folder, a dot file
  and a name without a dot keep the whole name) · Size (`<DIR>`, `<LINK>` for a link to a folder,
  `<SPECIAL>` for a fifo, socket or device, `<BROKEN>` for a link to nothing, `?` when the entry's
  attributes could not be read) · Date · Time (local time, of a link's target). Hidden (dot)
  entries are shown, dimmed. The foot reads `N listed = N bytes` (the entries but `..`; the bytes
  of the files) and, when one request can tell, `N bytes free`. Both panels start on the opened
  folder; a folder is listed again each time it is entered.
- **Listing** is one round trip: SFTP's readdir carries each name's attributes, and only a
  symbolic link costs a stat more (to say what it points at). A link is followed only to classify
  it. Locally: `read_dir` and an `lstat` per name. The free space is `statvfs@openssh.com` when
  the server offers it, `statvfs` locally.
- **Keys** (the list): ↑/↓, PgUp/PgDn, Home/End move the bar; Tab (or Shift+Tab) switches the
  active panel; Enter on a folder enters it; Backspace, or Enter on `..`, goes up with the bar on
  the folder just left; a typed letter puts the bar on the next name starting with it (any case,
  around the end); Enter or F3 on a file opens its viewer (F3 leaves folders alone); F4 opens the
  entry — folder or file — in the external editor, VS Code then Zed, chosen as A2 chooses (§3);
  a click puts the bar on a row, a double click opens it; the wheel scrolls the rows, and the
  next key brings them back to the bar. While a file is being made ready only Esc works (it
  stops a fetch); while a folder is listed, no key.
- **Enter on a file** opens the viewer for its kind (§3: extension, then the sniff) in the same
  window, in place of the two panels: text → the text viewer; image → the image window; video
  and audio → the media window (a `.ts` ffprobe cannot read as media → hex); anything else → the
  hex window (the binary dialog's "ask first" is not needed here: the hex view is the default).
  The path is resolved first (a link opens, and saves, as the file it points at: §5). A remote
  file is fetched into the cache exactly as the standalone routes fetch it (claimed, recorded, an
  earlier copy's unsaved edits moved aside), with its progress on the status row; Esc stops the
  fetch — the partial copy is removed and the cached copy left as it was. An image is always a
  copy (Save needs the bytes as opened); local media, text and other files are read in place.
  Each viewer gets a connection of its own (its Save and Export use it), and the copy it shows
  stays claimed until it is gone. The viewers keep exactly their edits, keys and Save rules
  (§6, §6a, §7); no new save path.
- **The text viewer** is read-only: line numbers, syntax highlighting (syntect — Sublime
  syntaxes and themes, on the pure-Rust regex engine, nothing linked) chosen by the extension, the
  whole name or the first line; ↑/↓, PgUp/PgDn (a page keeps one line of context), Home/End
  (start and end of the file), ←/→ scroll sideways, the wheel scrolls. It never reads the file
  whole: opening indexes the lines in one pass (a mark every 256 lines, and at the first line
  start 64 KiB past the last mark), and a page reads at most 64 KiB before it plus the shown part
  of each line. 4096 bytes of a line are shown (the bar says when lines are cut); invalid UTF-8
  shows as U+FFFD. Highlighting carries state from the top of the file: the state at each mark is
  computed once, a few milliseconds per frame, and a page is shown plain until it is reached;
  files over 16 MiB are shown plain.
- **Leaving a viewer** — Esc or F3 (when the viewer does not use Esc itself: the image and media
  windows' Esc first clears a drawn rectangle; an Esc that takes the keyboard from a text field
  is the field's), or closing the window — goes through that viewer's own close question: its
  unsaved-changes prompt (Save / Discard / Cancel), its "closes when the save is done", its second
  close during an export. The viewer goes only once it says yes; Esc then shows the list with the
  bar where it was, and closing the window closes it. Cancel keeps the viewer. A save or export
  still running when a viewer goes is waited for before sopen exits, its result printed, as its
  own window's opener does. A fetch running when the window closes is stopped.
- F4 in a viewer opens its file in the external editor too; the viewer stays.
- **Without a display** (Linux over ssh) a folder opens in the editor as before message 12, with
  one line on stderr saying why. `SOPEN_LIST` prints the browser's listing (the path, a row per
  entry, the foot) instead of a window; a scripted save or export (`SOPEN_AUTOSAVE`,
  `SOPEN_AUTOEXPORT`) on a folder is an error (nothing to act on).
- With `--target` the browser runs on HOST, which lists the folder over SFTP as it fetches any
  file; nothing new is forwarded but the `SOPEN_LIST` receipt hook.
- **Usage log:** a browser session is one line, kind `dir`; its action is `save` / `export` when a
  viewer in it saved or exported, and a viewer whose last save was refused makes the exit code 3.

## 4. Remote layer

- Transport = the system `ssh` binary (so `~/.ssh/config` is honoured completely), one multiplexed master per host
  (`ControlMaster=auto`, `ControlPath=<state>/cm-%C`, `ControlPersist=60s`).
- File operations = SFTP over that session (`openssh` + `openssh-sftp-client` crates): `stat`, `realpath`, read, write.
  SFTP keeps the code independent of the remote OS (macOS BSD tools vs GNU) — no remote shell parsing.
- Nothing is installed on the remote. Only sshd's SFTP subsystem is required. (VS Code Remote - SSH or Zed installs its own server when the
  owner opens folders/text; that is the editor's behaviour, documented in the README.) A `--target` machine (§4a) is not a
  file's remote: it is a machine the owner uses sopen on, and runs the sopen installed there.

## 4a. Where it opens

`sopen SOURCE` opens SOURCE on the machine it was typed on. `sopen --target HOST SOURCE` opens it on HOST's screen
instead: sopen starts HOST's own sopen as `sopen --here [--app APP] [--hex] -- SOURCE` over the system `ssh`, with the same
multiplexed master and connection options as §4, waits for it, and exits with its exit code (0, 1 and 3 mean what
they mean here; 127 is no sopen on HOST; 2 is a usage error from a sopen on HOST too old to know `--here`, passed on
as it is). When ssh itself fails (its exit code 255) sopen exits 1 with `could not
reach HOST`; any other status above 128 (sopen itself exits only 0, 1 or 3 — 2 for a usage error — so most likely a
signal, 128 + n)
exits 1 with `sopen on HOST ended with status N (signal n?)`. HOST needs sopen installed; the machine holding the file still needs only sshd. Because HOST is always
told `--here`, it never forwards again, whatever its own default is. `sopen cut` always runs here.

Which machine opens it: `--here` (this machine) or `--target HOST` — one or the other, the command line refuses
both — else the environment variable `SOPEN_TARGET`, else the `target` key of the config file, else this machine.
`SOPEN_TARGET` and the config file are read only when they are the ones deciding, so a broken file or an unreadable
variable does not stop `--here` or `--target`, and `cut` reads neither. HOST is `[user@]host` — an ssh alias or a
host name; a port or an IPv6 address goes into `~/.ssh/config` under an alias. Wherever it comes from it must be
non-empty, without whitespace or `:`, and must not start with `-`. HOST is this machine — opened here, no ssh — only when
its host part (without `user@`) is loopback (exactly `localhost`, a name under `.localhost`, or one of the
`/etc/hosts` aliases `localhost.localdomain`, `localhost6`, `localhost6.localdomain6`, `ip6-localhost`,
`ip6-loopback`; `127.0.0.0/8`; `::1`)
or equals this machine's host name as `gethostname` gives it, or that name up to the first `.` — in any case,
trailing dot ignored. Nothing else counts, because opening on the wrong machine is worse than an ssh hop to this
one: `box.lan` from a machine named just `box` is forwarded, and so is `box.other` from `box.lan`.

SOURCE is rewritten to what HOST must be given. A file on HOST itself (`[user@]HOST:path`, with no user or HOST's
user) becomes `path`, which HOST opens as a local file; a relative path stays relative to HOST's home, where the
command starts. A file on any other machine is passed unchanged, and HOST fetches it over SFTP itself. A local path
becomes `<this machine's short host name>:<absolute path>`, and a loopback source (by the same rule:
`localhost:path`, `[::1]:path`, not `localhost.example.com:path`), which would mean HOST itself there, becomes
`<this machine's short host name>:path` — so HOST must reach this machine
by that name with `ssh`. A local file that does not exist is the same error a local open gives.

On HOST, sopen is looked for on PATH, then in the usual Homebrew and cargo install paths; when there is none HOST
prints `sopen is not installed on HOST (brew install 2lab-ai/tap/sopen-preview)` and the run exits 127. A
non-interactive ssh session on macOS has no Homebrew on PATH, and sopen finds ffmpeg on PATH: HOST's sopen runs with
its own directory first on PATH (brew's ffmpeg sits next to brew's sopen) and Homebrew's bin directories that exist
appended after the session's own entries. On Linux an ssh session has no display, so when neither DISPLAY nor
WAYLAND_DISPLAY is set the desktop session's DISPLAY, WAYLAND_DISPLAY, XAUTHORITY, XDG_RUNTIME_DIR,
DBUS_SESSION_BUS_ADDRESS, XDG_SESSION_TYPE and XDG_CURRENT_DESKTOP (§3: KDE Plasma) are taken from
`systemctl --user show-environment` — exported by name,
never evaluated. If that gives none, the first Wayland socket in `/run/user/<uid>` is used, else X display `:0` when
it exists. HOST's sopen runs as a foreground child of the script, which catches INT, TERM and HUP (sopen itself
keeps their default handling), so a sopen stopped by a signal comes back as an exit code rather than as a lost
connection. It is not started in the background, where a non-interactive shell would have it ignore SIGINT. Ctrl+C
under `ssh -t` reaches the whole process group, sopen included; a signal sent to the script's shell alone leaves
sopen running.

A window asks for focus on its first frames. On macOS an app started over ssh cannot take focus: macOS keeps it
behind the active app. sopen's own window (the file browser, image, media and hex windows, the binary dialog), still
unfocused after asking, floats above the other
windows until it gains focus or gets its first click, key, scroll or typed text, and then returns to the normal
level — with no timeout, because back at the normal level while unfocused macOS puts it straight behind the active
app again. VS Code or Zed, opened for
text (or from the browser's F4), comes up behind the active app. On Linux under KDE Plasma sopen
brings it to the screen the user is on (§3).

HOST gets a terminal (`ssh -t`) when this side's input and output are both terminals. Otherwise it gets none
(`-T`) and no input either (`-n`) — ssh would read this side's input to its end, eating the rest of a
`while read f; do sopen "$f"; done < list` — except with `--app`, whose Enter prompt reads it. With a terminal,
Ctrl+C reaches HOST and closes the window there; without one, stopping this side's sopen leaves the window on HOST
open. The forwarding ssh also sets `RemoteCommand=none` (a `RemoteCommand` in `~/.ssh/config`, such as a tmux
auto-attach, would refuse the command) and `LogLevel=ERROR` (no "Shared connection … closed." after each run;
errors still print). The command starts with a bare `exec`, so the login shell on HOST that parses it is replaced
rather than left as a parent that a Ctrl+C would kill (dash and fish do not replace themselves on their own); every
other word is single-quoted (a `'` or `\` is written outside the quotes) so that shell — sh, bash, zsh, fish — reads
each one back exactly. The receipt hooks (AGENTS.md "Receipts")
travel to HOST; settings that name local paths (`SOPEN_CODE`, `SOPEN_ZED`, `SOPEN_FFMPEG`, `SOPEN_FFPROBE`, `SOPEN_CACHE_DIR`,
`SOPEN_STATE_DIR`, `SOPEN_CONFIG`) do not.

The config file is `$SOPEN_CONFIG` if set, else `$XDG_CONFIG_HOME/sopen/config.toml`, else
`~/.config/sopen/config.toml`, on macOS as on Linux. It is TOML with one key, `target = "<host>"`. A key sopen does
not know, a value it cannot use or text that is not TOML is an error naming the file and the line. `sopen config`
prints the file's path and its target (or `target is not set`). `sopen config target HOST` checks HOST, creates the
directories, and replaces the file atomically (a temp file in the same directory, renamed over it; a config that is a
symlink keeps its link). A new file is readable by its owner only (0600); a replaced one keeps its mode.
`sopen config target --unset` removes the key and leaves the file, empty.

## 5. Writes back (conflict-safe, never silent)

- Every fetch records the remote `(size, mtime)` and the SHA-256 of the fetched bytes. The write target is the
  `realpath` resolved at fetch time.
- Before any write: `lstat` the target. If it is gone, now a symlink or not a regular file, or its size or mtime
  differ — or, up to 256 MiB, its content hash differs (above that: size and mtime only, and the result says so) —
  refuse, keep the edited local copy, print its path. No overwrite. A server that leaves out size, mtime,
  permissions, type or owner gets no write.
- Write = create `.<name>.sopen-<random>` exclusively in the same remote directory → through its open handle
  (fsetstat, never by path): the target's owner, its permission bits, the content, then the full mode again (writing
  clears set-id bits) → `fsync@openssh.com` if offered → check the temp has the target's owner and mode (refuse if
  not) → re-check → `posix-rename@openssh.com` over the target (plain rename if not offered). Only a temp this
  write created is ever
  removed; one that cannot be removed is named. The next baseline is the staged temp — its size and mtime as stat'ed
  right before the rename, plus the uploaded bytes' hash — never the target's stat after the rename; if the target
  differs from it right after the rename, that is reported ("saved, but the remote changed right after") and the next
  save refuses. A server without `posix-rename@openssh.com` cannot replace a file in one step: Save is refused before
  anything is uploaded, and Save copy (a new sibling) is offered.
- Conflict detection is best-effort: it catches changes made before you save (size, mtime, and content hash up to
  256 MiB); it cannot lock the remote file, so a write racing the final rename is not detected.
- Any save that does not land moves the edited copy aside to `<copy>.conflict-<UTC stamp>` and prints that path. A
  fetch never overwrites a cached copy holding unsaved edits: it moves it aside first, and says what is known — "had
  unsaved edits" when the copy differs from what sopen recorded, "no record of what was fetched … kept it … in case
  it holds edits" when there is no record.
- Local copies are the owner's only: cache, edit, backup and export directories are created 0700; fetched copies,
  staged and kept image and hex edits, backups and edits moved aside are 0600. A staged or kept image or hex edit gets a new name
  (`<stem>.<tag>-<UTC stamp>-<random>.<ext>`), created exclusively — never over or through an existing file.
- The cache's fixed roots are dot-named — `<cache>/.backup`, `<cache>/.edits` (staged and kept image and hex edits),
  `<cache>/.exports` (local exports before they are published), `<cache>/.sessions`, `<cache>/.meta` — and a host
  label never starts with `.`, so no mirrored host lands in one (a host named `backup` is `<cache>/backup/…`, plain
  cache). Roots an older build made without the dot are just cache: never read or written.
- What sopen knows about a cached copy lives outside the mirrored tree, where no remote path can reach it:
  `<cache>/.meta/record/<host>/<abs path>` (the SHA-256 the copy held when last fetched or saved) and
  `<cache>/.meta/lock/<host>/<abs path>` (host labels never start with `.`). Records from older versions, kept next
  to the copy, are not read: such a copy counts as possibly edited and is moved aside on the next fetch.
- One session at a time works on a cached copy: a session (`--app`, the terminal editor, a window) holds an advisory
  lock on it until it ends; a second session on the same file — or any session where the copy cannot be locked —
  fetches into a copy of its own outside the mirror, `<cache>/.sessions/<host>/<abs path>.<random><ext>` (created
  exclusively, owner-only; no lock — only that session uses it), and says so in one line naming its path. Session
  copies stay in the cache until the cache is cleared: sopen never deletes them. Lock files are never deleted.
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
- Clipboard (msg 12 item 3): Cmd/Ctrl+C copies the rectangle — the whole image without one — as a picture;
  Cmd/Ctrl+X copies it and fills it with the background colour (white; transparent offered on an image with alpha);
  Cmd/Ctrl+V pastes into it a picture — stretched (the default), fitted (aspect kept, centred, the rest in the
  background colour) or 1:1 (top-left, cut off), chosen on the toolbar's second row — over what is there, or text in
  the text colour (black), composed to NFC (Finder copies Korean file names decomposed) with `\r\n` and `\r` as line
  breaks, wrapped to the rectangle's width and as large as fits; Hangul is drawn with an installed font that has it
  (eframe's system-font fallback finds the same one for the window's own text), and without one it shows as
  missing-glyph boxes and the status line says Hangul may not render. No rectangle: a picture goes 1:1 at the
  top-left, text at the top-left in letters a tenth of the image's height, and what was pasted becomes the
  rectangle. Each cut or paste is one undo step, written only by Save / Save copy; a key held until it repeats
  counts once (the shortcut is over at its key-up, at Cmd/Ctrl-up or when the window loses focus), a second press
  pastes again. The shortcuts reach the window as egui's Copy / Cut / Paste / PasteImage events (egui-winit reads
  and writes the system clipboard with arboard): text wins when the clipboard holds both, and pictures cross it as
  8-bit RGBA.
- Encoding keeps the format: PNG lossless, JPEG quality 95, WebP lossless, BMP, TIFF. Animated GIF opens read-only
  (frame 1 shown, Save disabled with a reason; it can still be copied).
- Never lose data silently: Save keeps the ICC profile and Exif where the format holds them; metadata it does not
  carry (XMP, text chunks, IPTC, comments) or an encoder refuses is named when the image opens and in the result.

## 6a. Binary files: the dialog and the hex window (A6, msg 9)

**The dialog.** A file of kind "other" (§3) opens a small sopen window titled `host:/abs` before
anything is fetched, from the stat alone: the file's name, its size (human and exact), and its type
when the extension says it cheaply (`ZIP archive`, `PDF document`, …). The window is as tall as its
content. Buttons: "Open in hex editor" (the default — Enter), "Save to…" and "Cancel" (Esc, or
closing the window: nothing is downloaded). Under them, the path "Save to…" saves to:
`<download folder>/<name>` (the platform's download folder, else the home folder; `~` for the home
folder), or the next free `<stem>-N<ext>` when that name is taken (the dialog says so). "Browse…"
opens the system's save dialog (AppKit on macOS, xdg-desktop-portal on Linux — loaded at runtime,
nothing linked); choosing a path there saves to it. When it gives no path — cancelled, or no system
dialog available — one line says the path field still works. A one-line hint says what Enter and
Esc do, and — only when something would be downloaded — that nothing is until a choice is made.
The exceptions: Enter in the path field saves there (it never opens the hex editor; when that save
is refused the field keeps the keyboard, so the next Enter saves again); during a transfer Enter and
Esc do nothing and closing waits for it.

- Hex: the file is fetched into the cache as every fetch is (claimed, recorded, an earlier copy's
  unsaved edits moved aside first — §5), with the progress in the dialog; then the same window
  becomes the hex window. A local file is read in place. Every step that can fail (the fetch,
  opening the copy) comes before the connection is handed to the hex window: after a failure the
  dialog is as it was, for a retry or "Save to…" (from the copy already fetched).
- Save to…: bytes are never put under the chosen name before they are complete, and never over, or
  through, anything already there (a file, a link). The chosen path's folder is opened once (a link
  to a folder is followed at that moment) and everything after goes through that descriptor, never
  the path again. A path ending in '/', a folder that does not exist or a file where the folder
  should be fails at once; a taken name is refused before anything moves. sopen makes a private
  folder `.sopen-<16 hex>` (0700; 02700 in a setgid folder, so on Linux `part` and the published
  file still get the shared folder's group) in it and, inside, `part` — created exclusively, without following
  a link, owner-only (0600) by the creating call — so the staging name never grows with the chosen
  one, and no other user can put or swap anything in it (other processes of the same user are
  trusted). Before a byte is transferred, `part` is linked to a second name inside the private
  folder and unlinked again: a filesystem without hard links (exFAT, some network shares) fails
  here, nothing transferred, the private folder removed ("this folder's filesystem has no hard
  links … choose another folder"). Then the bytes go into `part`, which is flushed to disk, given the
  mode any new file there gets (not the cache's 0600), and published by a hard link to the chosen
  name — which fails if anything is there — then `part` and the private folder are removed. A
  publish that fails for another reason keeps the complete `part` and the error says why and where;
  sopen never copies into a final name. A name taken at
  the last moment is refused like any taken name: the dialog says so and puts the next free
  `<stem>-N<ext>` in the field (the stem gives way, on a character boundary, so the name stays
  within 255 bytes); "Save to…" again saves there. Cleanup only ever removes sopen's own
  names inside its private folder, then the folder (a failure to remove it is said); an interrupted
  save leaves only that hidden folder. A download
  whose source changes meanwhile is not saved — a best-effort check: the remote's size and mtime
  after the transfer, and the bytes received, against its stat before. A local file (or the cached
  copy) is copied the same way, checked by its handle: size, mtime, change time and bytes copied.
  Then "saved <path>" shows a moment, the same line is printed on stdout, and the window closes —
  unless there are notes to read (a Close button then).
- Closing the window during a fetch or a download does not cut it: the window says it closes when
  the transfer is done, and does. A transfer whose worker ends without a result is a final, visible
  failure (the window says so and offers only Close; a close asked for meanwhile closes it). The
  line on the terminal when nothing was opened or saved says "nothing was downloaded" only when no
  transfer was started; a hex fetch that landed while the window closed is said as such.

`sopen --hex SOURCE` opens any file (any kind; not a folder) straight in the hex window — no dialog;
a remote one is fetched first, with the progress on the terminal. `--hex` and `--app` are exclusive.
With no display (Linux, neither `DISPLAY` nor `WAYLAND_DISPLAY`) the dialog and `--hex` fall back to
fetching the file and printing its local path, with one line on stderr saying why.

**The hex window** has the image and media windows' look and layout rules: a one-line toolbar on
top, a one-line status bar at the bottom, nothing that changes size while you work. The grid's top
and every row's position never move: not when the status changes, and not when a report arrives —
the disclosure rows (what must be read whole) sit between the grid and the status bar, appear only
once the primary button is up, and take rows off the grid's bottom.

- Rows of 16 bytes: the offset (hex, as many digits as the file size needs, at least 4), the 16 bytes
  in hex grouped 8 + 8, and the ASCII column (non-printable bytes as `.`). Only the rows on screen are
  read and painted: the file is read through a small page cache (at most 64 pages of 4 KiB; the least
  recently used goes first), never whole, so a file of any size scrolls the same. The grid has its
  own scroll bar (row positions are kept as 64-bit row numbers, not as pixels).
- The cursor is on a byte, shown in both columns (filled in the column that types, outlined in the
  other). Arrows, PgUp/PgDn, Home/End (start / end of the row), Cmd/Ctrl+Home/End (start / end of the
  file), a click places it (in the column clicked), Tab switches the column, the wheel scrolls (three
  rows a line). Cmd/Ctrl+G = the "Go to" field in the toolbar: an offset as `0x…` (hex) or decimal,
  Enter goes there. Typing goes to exactly one place: the Go to field when it has the keyboard —
  focused, clicked, or given it by Cmd/Ctrl+G in that same frame — else the bytes.
- Editing only overwrites; the file's size never changes. In the hex column a hex digit overwrites
  the high nibble, the next one the low nibble, and the cursor moves on (the two digits are one undo
  step, also at the undo bound); in the ASCII column a printable ASCII character overwrites the
  byte. Edits are a sparse map offset → the byte wanted there, in absolute values (and each undo
  and redo step records the bytes shown before and after) — the cached copy is never written;
  whether a byte is changed is a comparison with the file, so a byte typed back to its value is no
  longer a change. "Unsaved" is by content: edits undone back to what the file holds (as opened,
  or as last saved) are not unsaved. Changed
  bytes are marked in both columns. Undo/Redo: Cmd/Ctrl+Z, Shift+Cmd/Ctrl+Z.
- Status bar: the cursor's offset (hex and decimal), its byte (hex, decimal, the character when
  printable), the file's size (the exact count on hover), "N bytes changed", "unsaved", and the
  last result.
- Save (Cmd/Ctrl+S) is image Save's contract (§5), from one pass over the bytes as opened: the
  same reads write the local backup of the original under `<cache>/.backup` (the session's first
  Save; path printed), the staged copy with the edits applied (created exclusively and owner-only
  under `<cache>/.edits`), and their SHA-256. The handle's size, mtime, change time and inode are
  compared with what was opened before the pass and with before after it; any difference and the
  save is refused with nothing staged or backed up left (so is any failure during or after the
  pass) — the edits are written to
  `<name>.hexpatch-<stamp>.txt` among the edits (0600, set on the file whatever the umask; a header
  naming the source as opened,
  then one `offset: bytes` line per run of changed bytes), named in the disclosure rows. Then the
  conflict-checked overwrite; a local file opened in place has no hash from a fetch, so the pass's
  hash is its baseline (the target's content is checked against it up to 256 MiB). A refused or
  failed overwrite keeps the staged edit (`<stem>.conflict-<stamp>-<random><ext>` when refused) and
  names it in the disclosure rows; the remote is untouched. Save copy (Shift+Cmd/Ctrl+S) is staged by
  the same pass (refused, and the edits listed, the same way) and publishes `<stem>.edited<ext>` next
  to the original (`-N` when taken), never replacing anything, as image Save copy does. The bytes are
  read through the handle opened with the file, never again by path. The change-time part of that
  check also refuses after a change to the file's metadata alone (chmod, an extended attribute):
  accepted as conservative. A fetched copy's identity is never renewed after a Save (an upload does
  not change it), so a copy changed during an upload — even with its mtime put back — is refused by
  the next Save. A local file saved in place was replaced by the rename: the window re-binds to the
  file now at the path only if its content's SHA-256 equals the stage's (taken in the same pass);
  it becomes the new original. Nothing is read from the old file then: the edits and the history
  are absolute and stay as they are, and an edit is dropped only where it equals the new file's
  byte, read from the new file. That rebase is all or nothing: a byte the new file cannot give
  leaves the edits as they were and turns the window read-only. If the hash does not match
  (another writer, right after), the window turns read-only too. Read-only says so whole in the
  rows and lists the edits (the bytes wanted, absolute) in a `.hexpatch` file. When that list
  cannot be written, the rows say why and offer "Keep edits…" (try again) and "Write the list
  there" (a path the owner types, written as Save to… writes: a new file, never over anything);
  while the edits are kept nowhere and unsaved, closing asks (Discard / Cancel). That path field
  owns the typing of a frame it has the keyboard in, as the Go to field does (the grid does not
  take the keyboard back); what writing the list had to say (a staging folder it could not
  remove) is shown with where the list went. A re-bind forgets what the last Save copy holds
  (it was remembered against the old file): closing asks unless the bytes equal the saved file. A save whose worker ends without a result is a visible failure; the edits stay
  and the window stays usable.
- Where the server cannot replace a file in one step (`posix-rename@openssh.com`), Save is off with
  the reason on hover and in the rows; editing and Save copy work — the image window's rule.
- Closing with unsaved edits asks: Save (Save copy where Save is off) / Discard / Cancel. Closing
  during a save waits for it. Either way the window closes only when the save landed with nothing
  to read and nothing unsaved — a finished save is taken after the frame's input, so an edit or an
  undo in that frame counts; one that did not land, or landed with notes (the remote changed right
  after), keeps the window open; newer edits ask again. A close asked for during a save is used up
  by that save, whatever it reports: it never closes the window after a later Save. Once closing
  is decided nothing edits.

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
- `sopen cut <source> <in> <out> [--crop x,y,w,h]` = the same export, headless (scripting + CI receipts); it always
  runs on this machine (§4a).
- Renderer / playback details fixed by the spike (§11).

## 8. Usage log

One JSON line per run in `<state>/usage.jsonl`: `ts, host_hash, kind, ext, bytes, fetch_ms, action (open|save|export|cut|discard), conflict, ok, forwarded, target_hash`.
Hosts are hashed, never written by name. A run forwarded with `--target` (§4a) is logged on both machines: here with
`forwarded: true` and the target's hash (its kind is known only there), there as an ordinary open.
Local only, never sent anywhere. Exists for §10.

## 9. Out of scope (until §10 says otherwise)

Own text editor (the browser's text viewer is read-only; VS Code or Zed edits), a directory tree,
file operations in the browser (copy, move, delete, rename, make folder — msg 12 asks for viewing),
hex insert / delete or anything that changes a file's size, image edits beyond crop, pixelize and
clipboard copy / cut / paste (no drawing or text tool, no layers, no filters), frame-accurate
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
  frames (one started over ssh cannot take focus at all; §4a). A window can screenshot itself (`ViewportCommand::Screenshot`), which is how window receipts are
  taken (AGENTS.md "Receipts").
- **Measured in the spike (2026-11-11):** time to first frame — image 128–180 ms warm (541 ms first launch),
  video ~150 ms, audio waveform ~140 ms; A/V drift with a real-time null sink mean −11 ms / max 16.6 ms; a
  2.5–4.0 s cut = 45 frames starting at 00:00:02.500; 0.03–0.11 s per 2–4 s export; on Arch, Homebrew's
  pkg-config misses `/usr/lib/pkgconfig` (set `PKG_CONFIG_PATH` to build).
