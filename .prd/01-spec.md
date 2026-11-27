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

Naming (owner message 22, 2026-11-24): the project and its command are `sopen`; the multi-function
tool it opens — the file browser, the viewers, the image, media and hex windows, the git panel, the
directory tree and the dialogs — is xtools, the name in the title row's red program block. Message
22 is quoted verbatim in [`xtools/ssot.md`](xtools/ssot.md).

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
13. (2026-11-20) "1. 오디오도 구간별로 자르고 붙여넣기 기능 넣어줘 이미지 편집 정도까지 할수 있도록 채널이 2개면 둘다 보여주고 모노를 스테레오로 복사해서 변환할 수 있게 해줘 / 1.1 이거 스템 처리 못하나? 어케함? 리서치해서 간단하게 보이스랑 음악이랑 분리해줄수 있는지 리서치해주고 할수 있으면 해줘 / 2. 영상도 마찬가지로 자르고 붙여넣기만 해줘. 오래 걸리는데 이거 편집할때 실시간으로 빠르게 보여줄수 있게 편집해주고 저장할때 실제로 ffmpeg이용해서 처리해줘 (뭔말인지 알지? 영상이라 실시간으로 자르고 붙여넣으면 너무 오래 걸리니까) / 3. 텍스트 표시할때 기본으로 워드랩 처리해줘. 그리고 당연히 전체를 메모리에 올리지 말고 청크처리해줘"
14. (2026-11-20)
    > 그리고 만약 폴드가 git 폴더면 vscode중에 git graph있는데 그거 /zworkflow:using-dotprd 사용해서 해당 git graph 기능 모두 뽑아서 해당 기능처럼 자유롭게 git 작업할 수 있도록해줘.
    > 전체 커핏이랑 브랜치 그래프 볼수 있고 커밋 편집 최대한 자유롭게 할수 있게 해줘. 이런 폴더는 다른쪽은 뷰가 더블 디렉토리 뷰에서 다른쪽에 git 뷰로 변경됨.
    >
    > 이건 sopen 모드를 2개로 해주는 셈이고 디폴트가 미리보기 뷰.
    > - 이 모드일때는 듀얼 디렉토리로 보여주다가 현재 폴더가 git이면 git 패널이 반대편에 나와야하고, 미리보기가 이미지등을 미리 반대편에 적절하게 현재 선택중인 파일의 내용을 미리 보여주도록 해줘.
15. (2026-11-20)
    > 그리고 디폴트로 키
    > f2 = rename
    > f5 = copy
    > f6 = move
    > ---
    > 이거 주요 이러한 쉘들 단축키 리서치해주고 가장 일반적인 기준으로 단축키들 제안해줘.
    >
    > 그리고 맥북이나 윈도우나 리눅스가 기본적으로 펑션키는 항상 sopen에서 f1 f2 f3.. 펑션키로 동작하도록해줘. (이거 잘 모르겠는데 가능한지 리서치해줘)
16. (2026-11-20)
    > 그리도 또 기능
    > f10인가 누르면 디렉토리 트리뷰
    > 이거 잘 만들어야함.
    > 1. 예를들어 디폴트로는 루트가 아니라 홈디렉토리 기준임. 현재 cwd가 홈디렉토리 밑에있으면 홈디렉토리가 루트로 트리로 보여줘야함.
    > 만약 홈디렉토리가 아니면 그때 "/" 기준으로 보여줘야하고.
    > 2. 디바이스 폴더는 펼쳐주면 안됨. 즉, 펼쳐주는 폴더랑 안펼쳐주는 폴더를 구분할수 있어야함. 예를들어 어떤 폴더 안에 디렉토리가 5개(예)미만이면 디폴트로 펼쳐서 보여주고 아니면 폴더를 닫은채 보여줘야함. (대신에 안에 어떤 느낌인지 폴더랑 파일이 몇개 있고 용량이 얼마 되는지 예측할수 있는 정보는 즉시 보여줘야함)
    >
    > 이거 리서치해서 영리하게 디렉토리 트리 보여주도록 해줘.

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
is laid out where PC Shell puts things (§3a), and every sopen window wears PC Shell 5.5 / 6's skin
(§3a: palette, VGA font, frames, title and F-key rows, dialogs). Message 13's item 3 makes the
browser's text viewer wrap lines by default — every line whole, in rows — still reading the file a
chunk at a time, never whole, a line of hundreds of megabytes too (A11, §3a); W or F2 goes back to
one row per line. It adds no edit. Its items 1 and 2 are about the media window (§7).

Messages 14–16 open the browser workstream, fixed in [`browser/ssot.md`](browser/ssot.md): message 14
adds the git panel and the browser's two modes, preview (the default) and dual-directory (A12;
[`02-architecture.md`](02-architecture.md), [`03-git-panel.md`](03-git-panel.md)); message 15 the file
keys F2 rename, F5 copy, F6 move with the conventional rest, and function keys without fn (A13, A14;
[`04-file-keys.md`](04-file-keys.md)); message 16 the directory tree (A15;
[`05-directory-tree.md`](05-directory-tree.md)). They supersede message 12's "the browser views": the
browser renames, copies, moves, makes folders and deletes — never over an existing name (04 §5) — and
its git panel changes repositories, every history rewrite or destructive action backed up first (03
§8). File operations in the browser and a directory tree leave §9.

## 2. Intent → acceptance

| # | Owner intent | Acceptance (observable) |
|---|---|---|
| A1 | `sopen host:/path`, scp syntax, any machine in `~/.ssh/config` | `sopen <alias>:/abs/file`, `sopen <alias>:rel/to/home`, `sopen user@host:/x` all open; aliases, ProxyJump, agent behave exactly as `ssh <alias>` does (system ssh is used). Local paths (`sopen ./x.png`) open too. |
| A2 | Text edit, syntax highlight; VS Code first (msg 10; folders: msg 12, A11) | Text file → VS Code when `code` is found; a remote one only when VS Code's Remote - SSH extension is installed (`code --folder-uri` / `--file-uri vscode-remote://ssh-remote+[user@]host/<percent-encoded abs>` by the kind sopen detected, since a bare `--remote` path's kind is guessed from its name; the extension is never installed by sopen), else Zed with `VS Code has no Remote - SSH extension; opening in Zed` on stderr. Without VS Code, Zed (`zed ssh://…` on macOS, `zeditor`/`zed` on Linux). A VS Code launch failure is reported, not followed by Zed. The editor owns Save. The file browser's F4 hands a folder or file to the same editor the same way (A11). On Linux under KDE Plasma (msg 11) the editor's window that asked for attention is brought to where the user is — the active window's desktop and screen — and activated (§3). |
| A3 | Image: view + rectangle crop + rectangle pixelize + clipboard copy / cut / paste (msg 12) | Built-in window shows the image; drag draws a rectangle in image pixels; Crop and Pixelize act on it; Cmd/Ctrl+C copies it (the whole image without one) to the system clipboard as a picture, Cmd/Ctrl+X copies it and leaves it in the background colour, Cmd/Ctrl+V pastes the clipboard's picture (Stretch by default, Fit or 1:1) or text into it; Undo; Save writes back to the remote file. |
| A4 | Video: view + rectangle crop + time cut; cut / copy / paste on the timeline, previewed at once, rendered only on Save (msg 13) | Built-in window plays the video with sound; scrub timeline; a drag on it selects (`I`/`O` set the selection's edges at the playhead); Cmd/Ctrl+C / X / V, Delete, Cmd/Ctrl+Z / Shift+Cmd/Ctrl+Z edit the timeline and never start ffmpeg — playback and the paused frame show the edited timeline at once; drag a crop rectangle (Export crops its whole output by it); Save (Cmd/Ctrl+S, F5) renders the edit list with ffmpeg and writes it back over the original after a backup (§5: a local copy of a local file, a hard link beside a remote one) — the edit list only, never the rectangle; Export writes `<stem>.cut-<in>-<out>.<ext>` of the timeline, cropped, next to the original on the remote. |
| A5 | Audio (mp3/wav): view + time cut; cut / copy / paste, both channels shown, Mono → Stereo (msg 13) | Same window: waveform (a stereo file in two lanes, L and R), play, the same selection and edits, `M` = Mono → Stereo on a mono file (left = right = its channel, heard in playback and written by Save), Save, Export. |
| A6 | "아무 파일"; binary files open in a hex editor by default, after a dialog (msg 9) | Any other type (a zip, any binary) opens a small dialog before anything is downloaded — name, size, type when the extension says it: Enter = "Open in hex editor" (the default: sopen's hex window, §6a — a remote file is viewed as a stream, with nothing fetched first, [06](06-streaming.md) §7.1); "Save to…" downloads it to a path the owner picks (the download folder by default, never over an existing file); Esc or closing it downloads nothing. `sopen --hex SOURCE` opens any file straight in the hex window. `--app <App>` still opens it in a named local app and uploads it back when the user presses Enter in the terminal. With no display (Linux over ssh) the file is fetched and its path printed, and one line says why. |
| A7 | Rust, macOS + Linux, `brew install …` and use immediately | `brew install 2lab-ai/tap/sopen-preview` on macOS (arm64, x86_64) and Linux (x86_64, aarch64) installs `sopen` and the `ffmpeg` it needs; the first `sopen host:/a.mp3` works with no further setup. |
| A8 | Developed like llmux | Public repo `2lab-ai/sopen`, MIT; `ci.yml` (macOS + Linux), `preview.yml` (every main push → prerelease + tap bump), `release.yml` (`v*` tag = owner gate); `AGENTS.md` SSOT; `.prd/` spec. |
| A9 | `sopen SOURCE` opens here; `sopen --target <host> SOURCE` opens it on `<host>` (msg 7) | `sopen <host-a>:/tmp/a.html` opens on the machine it was typed on, as before. `sopen --target <host-b> <host-a>:/tmp/a.html` opens it on `<host-b>`'s screen: the same window or editor, started by `<host-b>`'s own sopen (on macOS sopen's window floats above the others until it is used; the editor opens behind the active app, §4a); exit code is `<host-b>`'s; `could not reach <host-b>` when ssh cannot get there; `sopen is not installed on <host-b> …` (exit 127) when it has no sopen. A file on `<host-b>` itself opens there as a local file; a local file reaches `<host-b>` as `<this machine>:<path>`. `sopen --tui --target <host-b> DIR`: F12 opens the file browser in a window on `<host-b>`'s screen at the terminal's state, and closing it there (or F12 there) brings the terminal back with that window's state ([10](10-tui.md) §7.3, X2-switch). |
| A10 | Default target in sopen's settings (msg 8) | `sopen config target <host-b>` writes `target = "<host-b>"` to the config file (§4a); from then on `sopen SOURCE` opens on `<host-b>`, `--here` opens on this machine anyway, `--target` picks another; `sopen config` shows the file and the target; `sopen config target --unset` removes it. A config sopen cannot read is an error naming the file, never ignored. The target is read again when `sopen --tui`'s F12 switches, so its window opens on `<host-b>` too, a target set meanwhile included (10 §7.1). |
| A10a | `sopen URL`: a web address in a real browser on the machine that shows things, tunnelled when that is another machine, never after sopen ends (owner, 2026-11-23: "sopen http://127.0.0.1:1234/ — 해당 머신의 safari 혹은 크롬 혹은 edge로 열어줘. (자연스럽게 터널링 포함해서 처리해주고 sopen이 종료되면 안열려야함)") | `sopen http://127.0.0.1:1234/` opens it in this machine's default browser and exits 0. `sopen --target <host-b> http://127.0.0.1:1234/` opens it in `<host-b>`'s default browser through an ssh tunnel from this machine, prints `… is open on <host-b> through a tunnel — Ctrl-C closes it` and stays; when that port cannot be forwarded on `<host-b>` it takes one of two random ones in 49152–65535 and says which. The port listens on `<host-b>`'s loopback only; one open to the network there (`GatewayPorts yes`) is refused before the browser opens. Ctrl-C closes it (exit 0), and once sopen has ended — killed outright included — or `<host-b>`'s sopen has, the page no longer loads on `<host-b>`. No printed line carries the address's password, query or fragment. A non-loopback address opens on `<host-b>` as written, with no tunnel. `--app` / `--hex` with an address is a usage error (exit 2); a malformed address, or a host spelled otherwise than plainly (`127.1`), is one error line (§4a). |
| A11 | A folder opens in a Total Commander-like program; Enter on a file runs its default viewer there (msg 12) | `sopen host:/dir` opens sopen's file browser (§3a): two panels on the folder, folders first; Tab switches panels, Enter enters a folder, Backspace goes up, a letter jumps to the next name with it; Enter on a file shows it in the same window — text highlighted with line numbers, its lines wrapped (msg 13; W or F2: one row per line), an image in the image window, video/audio in the media window, anything else in the hex window — and Esc returns to the list with the bar where it was; F4 opens the entry in VS Code / Zed (A2's choice). An edited image, hex or media file is never dropped unasked: leaving asks as its own window does. `--app` still opens a file in a named app; a folder with `--app` / `--hex` is an error, as before. |
| A12 | Git panel and browser modes (msg 14) | In a folder inside a git work tree, local or remote, the opposite side shows the git panel: every commit and branch as a graph, commit details and diffs, every Git Graph action and free history editing, each history rewrite or destructive action confirmed and backed up; the browser opens in preview mode (the opposite side previews the file under the bar) and F9 switches to dual-directory mode. Scenarios: 02 §4.5, 03 §11. |
| A13 | F2 rename, F5 copy, F6 move, the conventional rest (msg 15) | The list's F-key row reads `1Help 2Rename 3View 4Edit 5Copy 6Move 7MkDir 8Delete 9Dual 10Tree` once every slice has landed (until then a slot not built yet is empty, 02 §8); every file-changing key asks first and never overwrites an existing name, locally and over SFTP. Scenarios: 04 §8 (K-R, K-S, K-C, K-M, K-N, K-K, K-D). |
| A14 | Function keys act as F1…F12 in sopen (msg 15) | Every F-key row is clickable and Cmd/Ctrl+1…0 act as F1…F10 in every window; on macOS the top row sends F-keys while a sopen window is focused and the Mac's setting comes back on blur, quit and crash; Linux and Windows have the fallback only. Scenarios: 04 §8 (K-F). |
| A15 | F10 directory tree (msg 16) | F10 (or Alt+F10) shows a tree rooted at the home when the folder is under it, else at `/`; device, virtual, network and mount folders never open by themselves; folders with fewer than 5 subfolders open within a row budget; every row shows its folder and file counts and a growing size at once. Scenarios: 05 §8. |
| A16 | Split an audio track into voice and music (msg 13) | `sopen stems host:/a.mp3` writes `a.voice.mp3` and `a.music.mp3` next to it — new files, never over anything — and prints both paths; a video gives `.m4a` (AAC) stems; on a 0 dB voice/music mix each stem is ≈ 14 dB SDR (§7a). `brew install` brings the model; a missing or different model is one error naming the path it expected. The window gets its button in a later change; its child-process API is §7a's. |

Owner message 17 (2026-11-20, quoted in [`06-streaming.md`](06-streaming.md)) asks that every viewer
stream and that edits live locally until saved. Its contract shipped on 2026-11-23: viewing is
[06](06-streaming.md), editing [07](07-streaming-edit.md), and this file says what they mean for
each window (§3a, §5–§8):

| # | Owner intent | Acceptance (observable) |
|---|---|---|
| A17a | View any remote file without downloading it first, like a video stream: the first screen first, a buffer around it, a far jump on demand (msg 17) | [06](06-streaming.md) §12: e.g. a remote 24 GB file's first hex screen shows within 1.0 s of Enter having fetched ≤ 64 KiB (plus the 8 KiB kind sniff), and memory grows ≤ 80 MiB and the disk not at all while scrolling it (V1–V3). |
| A17b | Edits kept locally until saved; Save writes to the remote by default; Save As to a local path (msg 17) | [07](07-streaming-edit.md) §12: e.g. a 4-byte hex edit of a remote 24 GB file saves with ≤ 4 KiB read and 4 bytes written of the file by the save itself (then ≤ 128 KiB as the view reloads), a changed remote is refused with the original untouched, and a crash offers Restore (E1, E3–E6, E10). |

Owner message 25 (2026-11-27) adds llmux's channel commands; it adds no edit:

| # | Owner intent | Acceptance (observable) |
|---|---|---|
| A18 | Pick the release channel and update, as llmux does (msg 25: "그리고 llmux 처럼 sopen channel stable\|preview 고를수 있고 sopen update 할수 있도록해줘") | §4b. `sopen channel` on brew's sopen-preview prints `channel: preview (via brew)`. `sopen channel stable` before the first `v*` release prints `the stable channel has no release yet (its first v* release is the owner's step); staying on preview`, exit 1, with nothing installed or removed; once `2lab-ai/tap/sopen` exists the same command installs it, removes sopen-preview and prints `switched to the stable channel: sopen-preview <v> → sopen <v>`. `sopen update` prints `sopen-preview <before> → <after>` or `sopen-preview is up to date (<v>)`. A sopen brew did not install refuses both with `this sopen was not installed by brew (…); install it with: brew install 2lab-ai/tap/sopen-preview`, exit 1, and runs no brew. |

## 3. Routing

| Kind | Detected by | Opens in |
|---|---|---|
| directory | remote `stat` | sopen's file browser (§3a, msg 12); F4 there opens VS Code remote (`code --folder-uri vscode-remote://ssh-remote+[user@]host/abs`), else Zed remote (`ssh://[user@]host[:port]/abs`) |
| text | extension list, else first 8 KiB has no NUL and is valid UTF-8, or is UTF-16/32 by a BOM or the NUL pattern ([08](08-viewer.md) §7.2 steps 1–2, D18) | VS Code remote (`code --file-uri vscode-remote://…`), else Zed remote |
| image | `png jpg jpeg webp bmp gif tif tiff` | image window |
| video | `mp4 mov m4v mkv webm avi` (and ffprobe says it has a video stream); `ts` only when it is not text and ffprobe sees a video stream (`.ts` is also TypeScript) | media window |
| audio | `mp3 wav flac m4a aac ogg opus` | media window |
| other | everything else | the binary dialog: hex window (default) or Save to… (A6, §6a) |

- A remote file that opens in one of sopen's windows is read as a stream ([06](06-streaming.md)
  §13): nothing is fetched before the window shows, whatever the file's size. The connection
  that resolved the path, stat'ed it and sniffed its kind becomes the stream's transport, and
  the window's own connection is made at its first use: a Save, or an earlier session's journal
  to check on open ([07](07-streaming-edit.md) §3). An image's window reads the header,
  then the whole file into memory (its decode needs every byte) and gives way to the hex window
  on the same stream where the image does not decode or the owner picks Hex view (§6). A video
  or a sound is read by ffmpeg through the window's loopback byte server (§7); a `.ts` is probed
  through it, and one ffprobe does not read as media goes to the binary dialog. Without a
  display, and with no scripted save or export, the file is fetched and its local path printed
  instead (nothing to stream to). A local file is read in place, but for an image, which is
  copied into the cache first (its Save needs the bytes as opened).
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
frame drawn heavier; the status row (an opening's or an operation's progress, the last result, else what the keys do);
the bottom row lists the F-keys that work there and nothing else (the list, since slices S1 and S5
of the browser workstream: 1 Help, 2 Rename, 3 View, 4 Edit, 5 Copy, 6 Move, 9 Dual — `Prevw` in
dual mode — and 10 Tree; the directory tree: 1 Help, 10 Tree; the git panel, since S2: 1 Help, 3 View,
4 Edit, 9 Dual, and since S3 the slots that act on the object under the bar — 2 Reword (a branch's
label: rename; since S4 a commit's reword), 5 Pick, 6 Rebase, 7 Branch (Commit on the uncommitted
row), 8 Delete (since S4 a commit's drop), 10 Menu; with the planner open only 1 Help and 9 Dual; a viewer: 1 Help, 3 Exit, 4 Edit, 9 View (then 1, 2 or 3: 08 §4) and the viewer's own — the text viewer's 2 Unwrap (Wrap once unwrapped),
an alias of W, and 8 Encode, an alias of E (08 §7.5); the hex window's 5 Save, 6 Goto, 7 SaveAs, 8 Asc/hx, aliases of ⌘S, ⌘G, ⌥⌘S and
Tab; and the image and media windows' 5 Save and 7 SaveAs; a plain F-key never acts while a text
field has the typing). In a viewer 1, 2 and 3 choose its views (08 §3, §8): 1 the file's own, 2 Hex
for every file — opened viewing, Enter edits (08 D35) — 3 the other (Raw for text, the text of a
binary); a view switched away from is kept hidden, and one with unsaved edits asks first. The foot shows `N Listed = N bytes` whole inside the frame whatever the width: the
free space gives way first, then the end of the total. Every cell fits its column at a glyph
boundary (a name or an extension too long ends in "…"; a size too long for its column is said in
the smallest unit that fits, KB to EB, never cut). All drawing of those rows, the frames, the bar and every colour sits in
one module (`gui::chrome`): the PC Tools skin (msg 12 item 4) of every sopen window — light grey
on DOS blue, the VGA font (PxPlus IBM VGA 8x16, CC BY-SA 4.0) on its 8×16 grid, the active
window's double frame, the white-on-light-red bar, the red program block on the cyan title row
with the clock, digit-on-black / label-on-cyan F-keys, the ░ scroll bar with its red thumb, cyan
dialogs with a 2×1 shadow, a sixteen-colour syntax theme, and the hex window's `dddd(hhhh)`
offsets and code page 437 ASCII column. A window of its own (image, media, hex) has the F-key row
too, where F3 closes it as ⌘W does.

- **A panel** lists `..` (not at `/`), then folders and links to folders, then everything else,
  each by name regardless of case. Columns: Name · Ext (a file's extension; a folder, a dot file
  — any name starting with `.`, `.config.json` too — and a name without a dot keep the whole
  name) · Size (`<DIR>`, `<LINK>` for a link to a folder,
  `<SPECIAL>` for a fifo, socket or device, `<BROKEN>` for a link to nothing, `?` when the entry's
  attributes could not be read) · Date · Time (local time, of a link's target). Hidden (dot)
  entries are shown, dimmed. A name that is not valid UTF-8 shows with U+FFFD for its bad bytes
  and is never opened, entered or handed to the editor (sopen's paths are UTF-8; the name as
  shown may even be another file's), and nothing is looked up by it. The foot reads `N Listed = N bytes` (the entries but `..`; the bytes
  of the files) and, when one request can tell, `N Bytes Free` — PC Shell's words. Both panels start on the opened
  folder; a folder is listed again each time it is entered.
- **Listing** is one round trip: SFTP's readdir carries each name's attributes, and only a
  symbolic link costs a stat more (to say what it points at). A link is followed only to classify
  it. An entry whose type the server left out gets an `lstat` first (a link stays a link, a
  dangling one is `<BROKEN>`), then is followed only if that says link. Every stat goes by the
  name's own bytes. Each READDIR reply has the 30 s request timeout, not the listing as a whole,
  so a big folder on a slow link lists as long as the server answers. Locally: `read_dir` and an
  `lstat` per name. The free space is `statvfs@openssh.com` when
  the server offers it, `statvfs` locally.
- **Two modes** (msg 14; [02](02-architecture.md) §2–§4, landed with slice S1). The window opens
  in **preview mode**: the side opposite the active list follows its bar — on a file, once the bar
  has rested 150 ms, it shows that file's preview in a single-lined frame titled with the file
  (text wrapped and highlighted, an image fitted, the hex rows of anything else — sixteen bytes
  a row where the side is wide enough, else eight, without the hex window's path row; a video's
  first frame with its size and duration, a sound's waveform — of its first 10 minutes when it is
  longer — with its whole duration; a file over a preview budget, 02 §4.3, or one that cannot be
  read as an info card saying why); on a folder or `..` it shows its own list, or — when the
  folder is inside a git work tree — the git panel of that repository (below). A preview takes no key, click, drag or wheel, claims nothing in the cache and
  writes nothing to disk (a remote file of any kind is read through a stream on the window's one
  preview connection, which becomes the stream's transport: an image read whole into memory,
  text, hex rows, a video and a sound only as far as they show, 02 §4.3–§4.4), and is prepared
  one at a time on that connection; Enter or F3 on the file makes the live preview the viewer
  (a streamed preview hands its stream over, nothing read twice; a preview still reading goes on
  as the opening). **F9** switches to **dual mode** — two lists, as before — and
  back; slot 9 names the mode it switches to. The mode is the window's and is not saved.
- **The directory tree** (msg 16; [05](05-directory-tree.md), landed with slice S5) is a third
  content of a side (02 §3.1): **F10** (or Alt+F10) shows it in place of the active list — rooted
  at the home when the folder is under it, else at `/`, the bar on the panel's folder — and the
  other side keeps what it showed, frozen, until it closes. It lists folders and links to folders
  only; unlike the list, a folder's hidden subfolders are one dimmed row, `· N hidden folders`
  (→ or Enter opens it into a row each). Every row ends with its subfolder and file counts and its
  size, a lower bound marked `+` until a background scan has counted everything below; device,
  virtual, network and mounted folders never open by themselves, and every local call of the tree
  runs behind a 2 s watchdog (`not answering`). Enter lists the row's folder in the panel (a
  link's target, resolved; a link whose target is gone says so) and closes the tree; Esc or F10
  closes it with the panel as it was; Esc first stops an owner's whole listing or Space's scan.
  Remotely the tree has an SFTP connection of its own.
- **The git panel** (msg 14; [03](03-git-panel.md); read-only in slice S2, its actions since S3, history editing since S4) is the other side's
  content when the bar rests on a folder, `..` or an empty list and the active folder is inside a
  git work tree (detected once per folder with git's own `rev-parse`, locally or over the ssh
  master; a bare repository, the inside of `.git`, a folder git refuses or a host without git
  keep the list, the status row saying why once). It is the graph of every commit and branch —
  lanes, labels (`►main↔origin`, tags, `stash@{n}`), subject, date, author, hash, the
  uncommitted changes first — read 500 rows at a time from one `git log` stream that waits
  between pages; Enter or F3 opens a commit's (a stash's, the uncommitted row's) details below
  it, Enter on a file its diff and Shift+F3 the file at that revision, each in its viewer and
  read-only (Save and Save copy off, `a file at a revision has no place to be saved`), F3 with
  one marked row compares the two. Find (a typed letter, Cmd/Ctrl+F), history search
  (Cmd/Ctrl+Shift+F), the filters (Cmd/Ctrl+K) and Cmd/Ctrl+R (refresh) act in it. **Tab**
  gives it the keyboard and back; **Cmd/Ctrl+G** pins it to that side, so it stays while the bar
  moves over files and while the list goes elsewhere (titled with its repository); Esc unpins.
  Since S3 it acts (03 §7–§8): F10, Shift+F10 or a right click opens the menu of the object under
  the bar (a commit and its marked commits, the label under the label cursor, a stash, the
  uncommitted row or a file of it, the repository), and the F-keys act on it (03 §9.1); Space on a
  changed file stages or unstages it. Every action with an effect beyond the view opens its dialog
  first — what changes, the backup refs written before anything is rewritten or deleted, the exact
  command lines — and runs on a worker holding the repository's write lock (one write at a time;
  a second waits, and the status row says so); the panel refreshes after it. Esc stops a read or a
  network command; a write after 10 s, asking "Stop git?". A run's failure shows each step's
  outcome and git's text; a run that wrote a backup says `… · backup <stamp> · Cmd/Ctrl+Z undoes`.
  Cmd/Ctrl+Z restores the newest group this window wrote (again: redo), Cmd/Ctrl+B lists the
  backups (Restore, Show, Delete), Cmd/Ctrl+L the command log, Cmd/Ctrl+Shift+R fetches all. While
  git has an operation in progress — whoever started it — the panel's top rows say so (`MERGE of
  feature · 1 conflict`, `BISECT · Reset`), with its Continue, Skip, Abort (Cmd/Ctrl+Shift+C, S,
  A) and bisect's Reset (Cmd/Ctrl+Shift+B), each through its dialog.
  Since S4 history is free to edit (03 §7.8): a commit's menu — on HEAD and its ancestors, as the
  loaded graph allows — offers the quick edits (reword, author and date, squash, fix up, move up or
  down, drop, stop to edit, split, fold the staged changes) and Edit history from here…
  (Cmd/Ctrl+E), the planner over the commits above it: a row's verb by its letter (P R E S F D), its
  author (A), its place (Cmd/Ctrl+↑/↓), a reword's or squash group's message (Enter), the run and its
  options (Cmd/Ctrl+Enter); Esc closes, asking when something changed. Every edit is one run of git's
  own interactive rebase whose todo sopen writes, confirmed and backed up as every rewrite is, messages
  kept byte for byte; a range holding a merge is refused, naming it. An edit stop adds Amend and Split
  (Cmd/Ctrl+Shift+M, P); after a split the window keeps its paths and Continue is refused until every
  one is committed again. Rebase's dialog offers "Plan the commits first…".
- **Selection and file operations** (msg 15; [04](04-file-keys.md) §3–§5, S1's and S6's parts). Space or
  Ins selects the bar's entry and steps down; `+` and `-` select and unselect by a pattern, `*`
  inverts, Cmd/Ctrl+A selects all; selected entries are yellow and the foot counts them first.
  **F2** (or Shift+F6) renames the bar's entry in a field in its row; **F5** copies and **F6**
  moves the selection, or the bar's entry, to the other panel's folder — a dialog first, its
  target editable to any folder on any host (`host:/abs`). Nothing is ever replaced: a name in
  the way asks Skip / Rename / Cancel, never Overwrite. An operation runs on a worker with
  connections of its own, shows its progress on the status row, stops on Esc, and reports what
  it did (one line for one item done, else a dialog); the panels showing a changed folder
  re-read. An operation goes on whatever the window shows: while a viewer is open its question
  waits (the status row says `copy waiting for an answer — F3 back to the list`) and its end is
  told when the list shows again; Esc stops it also while a folder is still being listed.
  Closing the window waits for a running operation. A panel whose listing failed says so on
  the status row until that panel lists again. **F7** makes a folder in the active panel's folder
  (a one-line dialog; one level, a name with `/` refused; a name that exists is an error) and puts
  the bar on it. **F8** (Del, ⌘⌫ on macOS) deletes the selection or the bar's entry after a dialog
  that says what (`Delete <name>?`, `Delete N items (F files, D folders)?`, a folder's `and
  everything inside it`) and where: on this machine to the system Trash (macOS's, loaded at
  runtime; the freedesktop Trash elsewhere), `permanent — no Trash here` where there is none; on a
  remote `permanent — no Trash on <host>` (SFTP has none), removed bottom-up; **Shift+F8**
  (Shift+Del) deletes here without the Trash, `permanent` in the error colour. A link goes as a
  link; Esc stops between entries. A remote folder's F2 and F6 are one SFTP plain rename, on a
  second `sftp` channel of the same master, which refuses a name that exists; a copy to a server
  without `hardlink@openssh.com` is published the same way once a probe per connection found that
  the server's rename refuses (04 §5.6).
- **Keys** (the list; the one key table, `gui::browser::keys`, matches modifiers exactly, and
  Cmd/Ctrl+1…0 and a click on an F-key slot act as that F-key): F1 lists the keys; Cmd/Ctrl+R
  re-reads the panel, Cmd/Ctrl+U swaps the panels, Cmd/Ctrl+← / → show this folder in the left /
  right panel, Cmd/Ctrl+PgUp goes up; ↑/↓, PgUp/PgDn, Home/End move the bar; Tab (or Shift+Tab) switches the
  active panel; Enter on a folder enters it; Backspace, or Enter on `..`, goes up with the bar on
  the folder just left; a typed letter puts the bar on the next name starting with it (any case,
  around the end); Enter or F3 on a file opens its viewer (F3 leaves folders alone); F4 opens the
  entry — folder or file — in the external editor, VS Code then Zed, chosen as A2 chooses (§3);
  a click puts the bar on a row, a double click opens it; the wheel scrolls the rows, and the
  next key brings them back to the bar. While a file is being made ready only Esc works (it
  stops the opening); while a folder is listed, no key; a preview being prepared holds no key, and a
  running file operation only a second operation (02 §6).
- **Enter on a file** opens the viewer for its kind (§3: extension, then the sniff) in the same
  window, in place of the two panels: text → the text viewer; image → the image window; video
  and audio → the media window (a `.ts` ffprobe cannot read as media → hex); anything else → the
  hex window (the binary dialog's "ask first" is not needed here: the hex view is the default).
  The path is resolved first (a link opens, and saves, as the file it points at: §5). A remote
  file of any kind and any size is viewed as a stream ([06](06-streaming.md) §7, §13; the text
  viewer is the browser's alone — `sopen host:/file.txt` on its own still opens the editor, §3):
  nothing is fetched before it shows, and nothing is written to the cache. Text and hex rows open
  at once, show their first 64 KiB block when it arrives and read on as the view moves. An image
  reads its header, then the whole file into memory with its progress (06 §7.3; Esc stops the
  read, keeps nothing and shows the list again), and gives way to the hex window on the same
  stream when it does not decode or the owner picks Hex view (§6). A video or a sound is probed
  through the window's byte server before its viewer shows (§7); Esc stops that wherever it
  waits — a stalled read, the probe (ffprobe is killed, one not started yet is never started) —
  and an empty one is said to be empty. A local image is copied into the cache first (claimed,
  recorded, an earlier copy's unsaved edits moved aside: its Save needs the bytes as opened),
  with its progress on the status row; local media, text and other files are read in place.
  Esc stops a local image's copy and a local text file's index pass (checked every MiB): the
  partial copy is removed and the cached copy left as it was, also when Esc comes after the last
  byte but before the copy is in place. Esc once the copy is in place (while the image is
  decoded) keeps the viewer from opening; the copy stays in the cache, recorded, and the status
  row says so. A streamed viewer's own connection is made at its first use — a Save, Save As or
  Export (§5), or an earlier session's journal to check on open — and a cached copy a viewer shows
  stays claimed until it is gone. The viewers keep exactly their edits,
  keys and Save rules (§6, §6a, §7); the browser adds no save path of its own.
- **The text viewer** is read-only: line numbers, syntax highlighting (syntect — Sublime
  syntaxes and themes, on the pure-Rust regex engine, nothing linked) chosen by the extension or
  the whole name, else by the first line (a shebang) as read through an index that may be read —
  at open and again from each new index. It never reads the file whole for a screen. Its index
  holds a mark every 256 lines, and one at the first line start 64 KiB past the last mark.
  - **A local file** is indexed in one pass at open, on the opener's worker, before its first
    screen: every line has its number.
  - **A remote file**, read through its stream ([06](06-streaming.md) §7.2), shows its first
    screen from its first 64 KiB block, then indexes its first 16 MiB in the background (the top
    bar says `lines known to 3.1 MB of 24 GB` until the index is whole). Past that prefix the
    place on screen is a byte offset: the index grows from the blocks the screen reads next to
    it, the gutter shows a line's byte offset (`0x…`, dim) where its number is not known, the
    status row names the page by its bytes, and a line whose start is not found within 64 KiB
    back is shown from the 16 KiB grid point before the place (`line start not known`).
  - **Go to** (Cmd/Ctrl+G) opens a field in the status row: a line number, or `0x…` for a byte
    offset; Enter goes there, Esc leaves the field. A line past a stream's prefix runs the index
    pass up to it, with its progress (`indexing… 1.2 GB of 24 GB · Esc stops`); it is the only
    thing that reads a large file through, and only when asked.
  - The index's memory is bounded, for local files too: 16 MiB, or 48 bytes per 64 KiB of
    indexed text where that is more (17.6 MB for 24 GB); past 1 Mi marks the line spacing
    doubles and the marks no longer due are dropped.

  Invalid UTF-8 shows as U+FFFD (each maximal bad sequence one, as Rust's lossy decoding has it);
  a control character as its code page 437 glyph.
  - **Wrapped** (the default, msg 13): every line is shown whole, in rows as wide as the window,
    its number on its first row. A row ends where the next grapheme cluster (a letter and its
    marks, an Indic syllable, an emoji sequence: never split) would not fit, measured by laying
    the row out as egui paints it (Hangul, CJK and emoji are wider than a cell; Arabic and Hebrew
    are shaped right to left), in a few layouts per row — at the last place a row may break when
    what follows it fits on the next row, else where the row is full. A row may break after a
    space, and next to an ideograph, kana or fullwidth form (as UAX #14 allows: Chinese and
    Japanese have no spaces; Hangul words, which are spaced, and Latin words stay whole), never
    before closing punctuation (`。、」）！？`…) or a space, never after opening punctuation
    (`「（`…). A space the row is full at is not carried to the next row. A row is painted left
    to right: each run of right-to-left letters (Hebrew, Arabic…, with the spaces between them;
    by grapheme cluster, so a mark stays with its letter) is shaped as one, right to left, and
    everything else left to right, in the row's order —
    egui would otherwise shape a row whose first strong letter is right to left reversed whole,
    its English too; painting only, never the measured width. A tab goes to the next stop of four
    cells counted from its row's start. A line is wrapped afresh at every 16 KiB of it — at the
    first word start (after a space) within 128 bytes, else the first grapheme cluster start, else
    (a cluster longer than that) the first character start — so a row's end depends only on where
    the row starts: the rows are
    the same reached going down or back up. The place on screen is a line and the byte of it the
    top row starts at. ↑/↓ move a row, PgUp/PgDn a page of rows (one row of context), the wheel
    by rows; going back up reads at most 16 KiB of a line before the row (never a long line
    whole). Home: the line's first row when the top is inside a line, else the start of the file;
    End: the line's last row at the bottom when the line at the top runs past the page, else the
    end of the file; Cmd/Ctrl+Home and +End: the start and end of the file. Never past the last
    page (the file's last row at the bottom). A page finds its lines through the index as an
    unwrapped page does (from the mark before them, at most a span of each block), then reads its
    rows' bytes — a row at most 16 bytes per column and 256 more (a row of zero-width characters is
    cut there) — in reads of at most 16 KiB, never past where the index says a line ends; a
    character is never decoded cut in two at a read's edge. The status row says which
    lines are on screen and, when the top row is inside a line, how far into it (`line 7 (12 % of
    it) of 120`); the scroll bar's thumb goes by bytes (rows cannot be counted without reading
    everything), and a click on it, or a drag let go, is a jump there (a place the thumb only
    passes is not read). A narrower or wider window, or a new index, puts the top back on the start of the
    row it is in. A highlighted line's colours run on across its rows.
  - **Unwrapped** (W or F2 again): one row per line, ↑/↓, PgUp/PgDn (a page keeps one line of
    context), Cmd/Ctrl+Home/End (start and end of the file), the wheel scrolls; a page reads at
    most 64 KiB before it plus the shown part of each line. A line of any length can be scrolled
    to any byte: the place within a line is a byte of it, which ←/→, Home, End and the wheel
    sideways move, and a row is read under the wrapped row's bound around it. Home is the
    line's start, and the file's start when already there; End is the line's end at the right
    edge (placed by what its last characters paint as), and the file's end when the line already
    ends on screen. The scroll bar goes by lines once every line is known, else by bytes.

  Highlighting carries state from the top of the file: the
  state at each mark is computed once, on a worker of the viewer's own with its own handle on the
  file — never on a frame (one parse call cannot be interrupted, and a pathological line can take
  minutes). A page shows plain and its highlighted lines replace the plain ones when the worker
  sends them; the worker works on the page on screen only (a newer page makes it drop the older,
  checked between lines). Leaving the viewer or closing the window tells it to stop and never
  waits for it. At most 3 such workers run in the whole process, counting those whose viewer is
  gone but whose parse call still runs (a slot is given back only when the worker's thread
  ends): with none free, a viewer shows plain for its lifetime and its status row says
  highlighting is busy in other viewers. A worker that fails (a read error, a panic) leaves the
  page plain, ready, with the reason on the status row. A line is parsed only whole, so
  highlighting stops at the first line over 4096 bytes (the status row says which) and the rest
  is plain — wrapped, still whole; a file over 16 MiB is highlighted over its first 16 MiB and
  plain past them, read in place or through a stream alike (the status row says `highlighted over
  its first 16.8 MB`; 06 Tensions). A run of more than 128
  letters, digits and underscores (a hex blob, a long name) reaches the parser shortened, to
  more than 128 and fewer than 256 of its own characters in their order — its first and last 32
  and, between them, its stretches of one character, a lone one kept alone and a repeated one
  kept repeated, its length modulo 16; of a middle of many stretches, the first ones and each
  character the rest holds, up to 64 different ones — and is coloured as the whole run, the line
  around it and the lines after it as they are: a grammar's patterns cost the square of such a
  run's length (parsed whole, a Rust line with a comment of 1,000 `x`s costs 20 ms, 1,000 bytes
  of ordinary code 1.5 ms). The differences known: in a varied run, the left-out part takes the
  colour before it where a grammar colours inside a word (CSS colours digits in one), two
  characters meet where the middle is cut that are not neighbours in the run, and past 64
  different characters a test over the whole run sees the first 64; and a back-reference (a
  heredoc's delimiter) reads two long runs as one when they shorten alike — the same ends and
  stretches, each the same length modulo 16 (200 and 216 `x`s). So does a chain of more than 128
  characters — names joined by one `.`, `,`, `-` or `$`, the same each time (a dotted name in
  Python, Java or JavaScript, an argument list in Go, Java's names with `$`: 10–61 ms a line of
  1,000 parsed whole) — and in YAML any stretch of a line without `: # [ ] { } " ' \` of more
  than 128 (a long value, prose, quoted or in a list: YAML scans the rest of the line for a
  second key, 5–7 ms a value of 500): whole units of it — a name, or what spaces part — from
  each end, an even number left out between (TeX's `$` turns math on and off), the left-out
  part coloured as the character kept before it. That is its colour but where a grammar colours
  a chain's names apart from what joins them (SQL's `a.b.c`, TeX's `$`, JavaScript's
  `a.b.c.f = function`, PHP's constants, a list of `true,false,null`) or a YAML flow collection
  item by item. A number or a run over 128 parts a chain, shortened apart; a list of numbers
  is no chain. A Makefile is parsed with its grammar's two lookaheads for a rule and for a
  variable rewritten to mean the same with no two runs of characters side by side (as bundled,
  every line cost a power of its length: a 73-character recipe line 720,000 backtracking steps,
  a 300-line Makefile 239 ms; rewritten, 36,297 at most and 9 ms), on the highlighting worker
  the first time a Makefile is highlighted; a grammar that is not the bundled one is parsed as
  it is. Where the bundled grammar went past the backtrack limit and gave up on a line, the line
  is read as what it is (`objects := $(patsubst …)` a variable's). Two costs remain. A Makefile
  line with no parentheses is still searched from every start, quadratic in its length, and
  from about 500 characters the variable lookahead reaches the backtrack limit (1,000
  characters: 15–25 ms a line, against 42–62 ms bundled; a 2 MB file of such lines takes about
  22 s to its last page). Markdown's mixed `.+-` email shape (`a.b-c+d…`, no `@`) is parsed as
  bundled: about 7 ms a line at 1,000 characters, held at about 14 ms a line by the backtrack
  limit.
  The file is read live, the index is of the file as
  scanned, checked when the viewer opens and before each page against the open handle and the
  path it was opened by. Both still showing the indexed device, inode, size and modification
  time: unchanged. Both still showing the indexed device and inode, the scan having read every
  byte the file had when it started, and both no shorter than that (modification time may
  differ): only grown — a log being written to — and the index still describes the bytes as
  scanned, so pages are read through it (reads stay bounded, below) while a re-read is due. Any
  other difference — truncated, another file renamed over the path by an editor's save, a shrink
  the scan met — and the file is never read through the old index. A due re-read indexes the
  file again from its path, on a worker (stopped by Esc or closing the window), while the page on
  screen stays shown with "re-reading the file…" and scrolling (keys and wheel) waits. A change
  the highlighting worker finds starts that re-read, or schedules it, in the same frame. At most
  one re-read runs, and the next starts no sooner than max(1 s, 4 × the last index pass) after
  the previous ended (until then "the file changed; re-reading it in a moment…"), whether it
  succeeded, failed or was overtaken, and whether a page is shown or not; only a viewer's very
  first attempt, with nothing shown yet, starts at once. A just-built index — the one the viewer
  opens with, and each re-read's — is used when the file is unchanged or only grown since its
  pass started, checked on the worker right after the pass and again when the viewer takes the
  result; a fresh index is used while the file only grows; any other change is picked up by the
  next re-read, at most one re-read period later (an index taken while the file grew is never
  unchanged against it, so the next re-read is always scheduled). A shrink, another inode or a
  vanished path during the pass: the index is not used, the page on screen stays, nothing is
  read through the old index either (scrolling waits; the file may look grown to it), and the
  next pass waits as above (a vanished path: "the file was deleted"). Even a used index replaces
  the page on screen only with a page read through it, so a change landing first also leaves
  the last page up. The new index starts highlighting afresh; the viewer keeps its top line
  (clamped; wrapped, put back on the start of the row it is in), or follows the new end if it was
  at the end — wrapped, when the file's last row was on screen — ("the file grew; following the end",
  like `tail -f`; else "the file changed; re-read"). A local file viewed at its end is looked at
  again each second with no input (one stat), so a growing log is followed without a key press —
  also one written to without pause (each re-read is used though the file grew during it); away
  from the end it is looked at only on the next frame. A path that names nothing any more keeps
  the last page with "the file was deleted" and reads no more. A remote file read through its
  stream is checked against the handle's size and modification time taken at open, by an `fstat`
  after every batch of reads and by a poll while it is shown ([06](06-streaming.md) §5): a file
  changed under the handle freezes the page (`the file changed on the remote — Cmd/Ctrl+R
  reloads`), and a file replaced under the path keeps showing the version opened, saying so; it
  is not followed like `tail -f`. A write that lands
  between a change and its detection, or one that only looks like growth (written in place, no
  shorter), cannot make a read run away: a line is never read past where the index says it must
  end — the next mark, or 64 KiB past its own block's mark when the next line was no mark — and
  one that does not end there makes the index unusable and starts a re-read. Offsets from the
  index are always checked.
- **Leaving a viewer** — Esc or F3 (when the viewer does not use Esc itself: the image and media
  windows' Esc first clears a drawn rectangle; while a Save runs, Esc stops that Save before its
  rename instead — the image and media windows' at any step before it, a streamed hex file's
  between its requests — and the viewer stays (07 §10); while the media window exports, Esc is
  the export's Cancel (§7); an Esc that takes the keyboard
  from a text field is the field's), or closing the window — goes through that viewer's own close
  question: its unsaved-changes prompt (Save / Save As… / Discard / Cancel), its "closes when the
  save is done" (leaving never cuts a running save off), its second close during an export. The
  viewer goes only once it says yes; Esc then shows the list with the bar where it was, and
  closing the window closes it. Cancel keeps the viewer. A save or export still running when a
  viewer goes is waited for before sopen exits, its result printed, as its own window's opener
  does. An opening still running when the window closes is stopped.
- F4 in a viewer opens its file in the external editor too; the viewer stays.
- **Without a display** (Linux over ssh) a folder opens in the editor as before message 12, with
  one line on stderr saying why. `SOPEN_LIST` prints the browser's listing (the path, a row per
  entry, the foot) instead of a window; a scripted save or export (`SOPEN_AUTOSAVE`,
  `SOPEN_AUTOEXPORT`) on a folder is an error (nothing to act on).
- With `--target` the browser runs on HOST, which lists the folder over SFTP as it streams any
  file; nothing new is forwarded but the receipt hooks (`SOPEN_LIST`, and the stream's
  `SOPEN_STREAM_STATS` and `SOPEN_STREAM_THROTTLE`).
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
- The git panel (03) runs the `git` already installed on a remote over the same master, as an ssh
  command whose words are quoted as `--target`'s are (03 §3.2); it uploads and installs nothing, and
  without git there it is not offered. File operations (04) and the directory tree (05) stay SFTP.

## 4a. Where it opens

`sopen SOURCE` opens SOURCE on the machine it was typed on. `sopen --target HOST SOURCE` opens it on HOST's screen
instead: sopen starts HOST's own sopen as `sopen --here [--app APP] [--gui] [--hex] -- SOURCE` over the system `ssh`, with the same
multiplexed master and connection options as §4, waits for it, and exits with its exit code (0, 1 and 3 mean what
they mean here; 127 is no sopen on HOST; 2 is a usage error from a sopen on HOST too old to know `--here`, passed on
as it is). `--gui` (the window on HOST, [10](10-tui.md) §11 row 2, T35) goes when the owner gave it, and until
10's XF only then: an automatic one would turn HOST's fallbacks without a display (a folder to the editor, a file
fetched and its path printed) into exit 1. XF turns on the automatic one (10 §7.3): for a window run, with `--hex` or
when the kind this machine sees opens in a window — a folder, an image, video or audio, a binary (a local SOURCE by
its stat and first bytes, a remote one by its name alone); never text, a name that does not decide the kind, or a run
the hooks keep without a window. `--app` and an address never get it. HOST's exit 2 with clap's line for an unknown
`--gui` (`error: unexpected argument '--gui' found`, a sopen older than it) as the first line it wrote on stderr
(colours and blank lines aside) and nothing on its stdout — clap's refusal before anything runs — makes sopen run the
same forward once more without it, after `sopen on HOST is older and does not know --gui; running it there again
without it` on stderr; any other exit 2 is passed on as it is. HOST's output passes through this side byte by byte as
it is read, only watched for that line (a terminal query a program there waits on is never held). `--gui` without a HOST opens
here as before, and with no display here (Linux without `DISPLAY` / `WAYLAND_DISPLAY`) exits 1 with `no display here
— sopen config target <host>, or --target, opens the window on
another machine`. When ssh itself fails (its exit code 255) sopen exits 1 with `could not
reach HOST`; any other status above 128 (sopen itself exits only 0, 1 or 3 — 2 for a usage error — so most likely a
signal, 128 + n)
exits 1 with `sopen on HOST ended with status N (signal n?)`. HOST needs sopen installed; the machine holding the file still needs only sshd. Because HOST is always
told `--here`, it never forwards again, whatever its own default is. `sopen cut` and `sopen stems` always run here.

Which machine opens it: `--here` (this machine) or `--target HOST` — one or the other, the command line refuses
both — else the environment variable `SOPEN_TARGET`, else the `target` key of the config file, else this machine.
`SOPEN_TARGET` and the config file are read only when they are the ones deciding, so a broken file or an unreadable
variable does not stop `--here` or `--target`, and `cut` and `stems` read neither. HOST is `[user@]host` — an ssh alias or a
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
command starts. A file on any other machine is passed unchanged, and HOST opens it over SFTP itself. A local path
becomes `<this machine's short host name>:<absolute path>`, and a loopback source (by the same rule:
`localhost:path`, `[::1]:path`, not `localhost.example.com:path`), which would mean HOST itself there, becomes
`<this machine's short host name>:path` — so HOST must reach this machine
by that name with `ssh`. A local file that does not exist is the same error a local open gives.

A SOURCE that starts with `http://` or `https://` (the scheme in any case) is an address, recognised before anything
reads SOURCE as a path or `host:path` (`http` would be a host). A small parser (`src/url.rs`) takes the scheme, the
host (an IPv6 one in brackets), the port (80 / 443 when absent) and the rest — path, query, fragment — as written. No
host, a port outside 1–65535, an unclosed `[`, a blank or control character, or a character a host cannot have is one
error line naming the address (exit 1); so is a host a browser would read as another address — a last label that is a
number (digits, `0x…`) anywhere but in a plain dotted quad (`127.1`, `2130706433`, `0177.0.0.1`), a `%`, a non-ASCII
host, an IPv6 zone, an IPv4 address inside IPv6 (`[::ffff:127.0.0.1]`) — so that what sopen calls loopback is what the
browser reaches. `--app` and `--hex` open files, so with an address they are a usage error (exit 2). The address goes
to the browser whole and nowhere else: every line sopen prints, errors and debug lines included, shows it with
`user:***@` (`***@` for a user alone), `?…` and `#…` for the userinfo, query and fragment (`url::redacted`). It is in
the tunnel's ssh command line, so `ps` can show it. Opened here — no HOST, or HOST is this machine — the address goes
to the default browser exactly as written (`open` on macOS, `xdg-open` elsewhere; `SOPEN_BROWSER` replaces it,
AGENTS.md "Receipts"), started in its own process group with no input or output and waited for as an editor CLI is;
sopen prints `opened <shown address> in the browser` and exits 0. With a HOST, an address that is not loopback (by the
rule above) goes to HOST's sopen as it is, with no tunnel. A loopback one would mean HOST itself there, so it is served
from this machine: `ssh -S none -o ControlMaster=no -o ExitOnForwardFailure=yes <§4's keepalives and connect bound> -T
-R 127.0.0.1:P:<host>:<port> <run options> HOST -- <the script> --hold-until-eof -- <the address on P>` (`[::1]:P:…`
for an IPv6 address) — a connection of its own, because a `-R` asked through the multiplexed master belongs to the
master and would outlive sopen; no terminal; and this side's input, a pipe sopen holds and never writes. The explicit
loopback bind address keeps the port on HOST's loopback under sshd's `GatewayPorts no` and `clientspecified`; `yes`
binds every interface whatever is asked, so HOST's sopen, before it opens the browser, reads what listens on P
(`/proc/net/tcp{,6}` on Linux, `netstat -anv -p tcp` on macOS — both list every user's sockets, sshd's included;
`SOPEN_LISTENERS` in tests) and opens only on positive evidence: at least one listener on P, every one on `127.0.0.1`,
`::1` or `::ffff:127.0.0.1`. Anything else refuses — exit 1, the browser not opened, the tunnel torn down: a listener
elsewhere, no listener seen (`cannot confirm the tunnel is private on <HOST> (no listener seen on port P)`), a table
it cannot read. P is
the address's port first, so its origin is unchanged. When ssh reports that remote port not forwarded (`remote port
forwarding failed for listen port P`, exit 255 — the same for a port that is taken, a port below 1024 for a user, and a
server that refuses forwarding) sopen says `port P could not be forwarded on HOST (taken there, or forwarding refused)
— trying Q` and tries at most two random ports in 49152–65535, each a new ssh; the address then opens with the port
it got, another origin. When those fail too: `HOST refused the tunnel: forwarding is off there, or no port was free
(AllowTcpForwarding?)`. Any other ssh failure is not retried. HOST's sopen prints a fixed cue (`sopen-tunnel-open`,
nothing of the address), which sopen reads instead of passing on, and sopen prints `<shown address> is open on HOST
through a tunnel — Ctrl-C closes it` (after a fallback, `… as <shown address on Q> through a tunnel (port P could not
be forwarded there) — …`); HOST's sopen then waits for its input's end. ssh's output passes through but for the cue
and the forward-failure line. Ctrl-C or SIGTERM closes the tunnel and sopen exits 0 (128 + n when the address had not
opened yet); ssh ending on its own leaves with the exit codes above.

The tunnel must not outlive sopen, however sopen ends, nor HOST's sopen. ssh alone does not end with HOST's command:
it stays while a browser holds a forwarded connection (ssh(1), `~&`), and new connections would still be served. So
ssh runs under a `sh` guard whose watcher reads the same pipe (through fd 3: dash gives an asynchronous command
`/dev/null` before its own redirections) and kills ssh at its end: a SIGKILL of sopen closes the pipe, HOST's sopen
ends, and the guard ends ssh; the guard reaps its watcher. HOST's command ending is seen as the end of ssh's output —
OpenSSH hands its stdout to the session channel and closes it with the session — which the guard therefore keeps none
of: if ssh has not exited 2 s later (it stays for the browser's connections), sopen closes the tunnel and exits 1
(`sopen on HOST ended while ssh stayed for the browser's connections; the tunnel to … is closed`); if it has, HOST's
exit code. The tunnel is tested against a stand-in ssh; a live receipt against a real HOST is still open.

HOST's half of the switch from `sopen --tui` ([10](10-tui.md) §7.3) is `sopen --here --gui --state-stdin --
FOLDER`: a hidden flag that needs `--here` and `--gui` and goes with nothing that opens something else (`--tui`,
`--app`, `--hex`, `--browser`, `--hold-until-eof`). The first line of its input is the switch's state (10 §7.4, one
JSON line). A state it cannot take — a newer `v` (`the sender sent switch state v<n>; this sopen reads up to v<m> —
update one of them`), no line, one that does not read, a state of another host than FOLDER's — and a file or an
address as FOLDER are usage errors (exit 2), said before any window opens; FOLDER is any folder SOURCE can name, on
HOST or on another machine (the switch sends one on its own machine as `<that machine>:<abs>`). The file browser
opens at the state in a switched window: F12, its title row's `[TUI]`, the close button and Cmd/Ctrl+Q and +W
return, unsaved edits first asking the window's own close prompt (its Cancel keeps the window); once it has
returned and what still writes has ended, HOST's sopen prints `sopen-xtools-state <the state's JSON line>` — a fixed
cue, as `sopen-tunnel-open` is — and exits 0 (3 after a refused save, as any window). The rest of its input is read
to its end, as `--hold-until-eof` reads it, and heard by the window drawn or hidden. At the end — the other side's
sopen gone — nothing is read from or written to the session again, and the window closes when it holds nothing it
would lose; with unsaved edits or a job that writes it stays, after the fixed line `sopen-xtools-kept`, until it is
closed there, no longer showing `[TUI]`. The session can die at any time while something still writes (a report
after the window, a Save's progress), so HOST's sopen relays its standard output and error to the session through
pipes of its own: a relay whose session is gone drains what it is given, and no write in sopen fails. Each relay is
asked before the session is let go, and before sopen exits, to pass on all it was given and answer; so the kept line
said just before the session is let go reaches it (at most 2 s), and the state's cue reaches a peer that reads it
slowly (at most 30 s: a peer that keeps the session open and stops reading holds sopen's end that long, as it held
its writes before the relays); a dead session answers at once. Then the relays have at most 2 s more to end, for a
child still holding one of the pipes.

The other side, `sopen --tui` with a HOST (10 §7.3, X2-switch), runs that half as `ssh <the master's options> -T -o
RemoteCommand=none -o LogLevel=ERROR -o BatchMode=yes HOST -- exec [env …] sh -c <the script> sopen --gui
--state-stdin -- FOLDER`, with `SSH_ASKPASS_REQUIRE=never` in ssh's environment — the terminal is the TUI's, so ssh may
ask nothing — and never retries it without `--gui`: exit 2 there is HOST's answer. The state goes out with its paths
rewritten as SOURCE is (this machine's `/abs` as `<this machine>:/abs`, HOST's own as `/abs`), FOLDER is its active
folder so rewritten, and the state's line goes on ssh's input, which then stays open. It carries the receipt hooks a
`--target` run carries but the twelve that script HOST's run or make it one without a window (`SOPEN_STATE`,
`SOPEN_SCREENSHOT`, `SOPEN_AUTOSAVE` and its delay, `SOPEN_AUTOEXPORT`, `SOPEN_EXPORT_HOLD_MS`, `SOPEN_CHOICE`,
`SOPEN_SAVE_TO`, `SOPEN_SAVE_HOLD_MS`, `SOPEN_SAVE_AS` and its hold, `SOPEN_LIST`), and `SOPEN_WINDOW_SIZE` is the
terminal's grid when none is set. The state HOST hands back comes back with `<this machine>:/abs` as `/abs` and its
folders on the terminal's own host label, given to a browser made anew on the terminal's folder; Ctrl+C here closes
HOST's input (10 §7.3: its window closes, or stays with unsaved edits and says so), a second one ends this run's ssh
client alone — never the master — and each way HOST's half can end is one line on the terminal's status row.

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
`~/.config/sopen/config.toml`, on macOS as on Linux. It is TOML with two keys, `target = "<host>"` and
`fkeys = "function" | "system"`. A key sopen does
not know, a value it cannot use or text that is not TOML is an error naming the file and the line. `sopen config`
prints the file's path and its target (or `target is not set`), and `fkeys = <function|system>` when the file sets it.
`sopen config target HOST` checks HOST, creates the
directories, and replaces the file atomically (a temp file in the same directory, renamed over it; a config that is a
symlink keeps its link). A new file is readable by its owner only (0600); a replaced one keeps its mode.
`sopen config target --unset` removes the key and leaves the file (empty when nothing else is in it).

`fkeys` is what the Mac's top row sends while a sopen window is in front ([04](04-file-keys.md) §6.2, A14):
`"function"` — the default, also when the key is absent — F1…F12 without fn: sopen sets the HID system's
`HIDFKeyMode` to 1 when one of its windows comes to the front and puts back what it found once none is, through a
lock, a state file and a watchdog that restore it even when sopen dies (04 §6.3); `"system"` — sopen never touches
the setting. The setting is global: while sopen is in front every app sees F-keys, and brightness and volume need fn.
A run a hook drives (AGENTS.md "Receipts") and the tests never switch it; Linux and Windows have no such switch
(04 §6.4). `sopen config fkeys function|system` sets the key as `sopen config target` sets the target. The config
is read for it when sopen's first window opens; a config sopen cannot read then leaves the setting alone (the run
goes on — it was not deciding where the file opens).

## 4b. Release channel and update (A18, msg 25)

`sopen channel` prints the release channel this sopen is on and `sopen channel stable|preview` switches to the
other; `sopen update` updates sopen on its channel. Both go through Homebrew, as llmux's `channel` and `update` do
(`src/channel.rs`, whose header keeps these decisions): the tap `2lab-ai/tap` has `sopen-preview` (the preview
prereleases) and, from the first `v*` release on, `sopen` (stable); both link `bin/sopen`, so at most one is linked.

- **The channel** is derived from what brew has installed; there is no config field. One formula installed is the
  channel: `channel: preview (via brew)`. With both installed, the one whose keg `bin/sopen` resolves into decides,
  and a warning on stderr names it and what each switch removes; when no link says, preview wins. Only a sopen
  that runs from a sopen keg in brew's own Cellar (`<brew --cellar>/<sopen|sopen-preview>/<version>/bin/sopen`,
  links resolved) is installed by brew. When brew cannot answer, the build marker does (`SOPEN_BUILD_CHANNEL`:
  `preview` from preview.yml, `release` from release.yml): `channel: preview (via build marker (brew
  unavailable))`. A sopen brew did not install — a source build, a copied binary — answers from its marker too,
  `… (via build marker (not installed by brew))`, whatever brew has installed beside it, since `sopen update` would
  not update that binary (llmux reports brew's formula here). A `dev` marker is no channel: `this sopen has no
  channel: it was not installed by brew and its build marker is dev`, exit 1.
- **Not installed by brew:** `sopen update` and `sopen channel stable|preview` refuse before brew runs at all —
  `this sopen was not installed by brew (<why>); install it with: brew install 2lab-ai/tap/sopen-preview`, exit 1.
  A binary that is not shaped like a keg is decided without asking brew anything. The line names no path.
- **The tap refresh** before a switch or an update is `brew update`, its output the user's. When it fails (one
  third-party tap that fails to update is enough), one stderr line says so — `` `brew update` failed (exit 1):
  <its last line>; refreshing only 2lab-ai/tap by its git `` — and only the tap's checkout is fast-forwarded:
  `git fetch -q https://github.com/2lab-ai/homebrew-tap.git master` and `git merge -q --ff-only FETCH_HEAD` in
  `brew --repo 2lab-ai/tap`; that failing too ends the command with nothing installed or removed. brew always runs
  with `HOMEBREW_NO_AUTO_UPDATE=1`.
- **A switch** refreshes the tap, then looks the target up in it (`brew info --formula 2lab-ai/tap/<formula>`).
  The stable channel without a release is refused there, before anything is installed or removed: `the stable
  channel has no release yet (its first v* release is the owner's step); staying on preview`, exit 1 — read from
  brew, so the same command switches the day the release lands. Then, the target first so that a failure leaves the
  old channel whole (llmux's order): what depends on the outgoing formula is asked (`brew uses --installed`; no
  answer ends it with nothing changed); the target is installed (`brew install 2lab-ai/tap/<formula>`, `upgrade`
  when it already is); the outgoing formula is removed with every keg (`uninstall --force`), or only unlinked when
  another installed formula depends on it (`kept sopen installed (required by …) — unlinked`); the target is
  relinked (`unlink`, `link --overwrite`). Last: `switched to the stable channel: sopen-preview 2026.11.27.0138 →
  sopen 0.2.0`.
- **The same channel again** does what llmux's does: `already on the preview channel — nothing to do`, a relink
  when `bin/sopen` is not its binary, or the other formula retired when both are installed. It never upgrades —
  that is `sopen update` — and refreshes no tap.
- **`sopen update`** refreshes the tap, runs `brew upgrade 2lab-ai/tap/<the channel's formula>` and prints
  `sopen-preview 2026.11.27.0138 → 2026.11.28.0101`, or `sopen-preview is up to date (2026.11.27.0138)`.
- After a switch, or an update that changed the version: `sopen windows already open keep running the old version
  until they are reopened` — each window is a sopen process of its own, on the binary it started from.
- Exit codes: 0 done or up to date; 1 refused, or brew or git failed — the failure names the command, its exit and
  the last line it wrote on stderr (`` `brew upgrade 2lab-ai/tap/sopen-preview` failed (exit 1): Error: … ``); 2
  a usage error. brew's own output reaches the terminal as it runs.
- `channel` and `update` are commands: a file or folder of that name is given as `./channel` / `./update`, as
  `./config` is.

## 5. Writes back (conflict-safe, never silent)

- **The baseline** is the file as sopen opened it. Every fetch records the remote `(size, mtime)` and the
  SHA-256 of the fetched bytes. A file read through a stream ([06](06-streaming.md)) is not fetched: its
  baseline is the handle's `(size, mtime)` at open and, up to 256 MiB, the SHA-256 of the file as opened — the
  image window's from its whole read, the hex and media windows' from a baseline pass over the stream that
  starts at the first edit (`checking the file for Save — 37 %`; a Save waits for it). Above 256 MiB a
  media file's baseline, local or remote, is the SHA-256 of its first and last MiB instead. A hex edit also keeps the
  hash of each edited 4 KiB page as the window showed it, which every Save checks whatever the size
  ([07](07-streaming-edit.md) §4.5). The write target is the `realpath` resolved when the file was opened.
- Before any write: `lstat` the target. If it is gone, now a symlink or not a regular file, or its size or mtime
  differ — or, up to 256 MiB, its content hash differs (above that: size and mtime, the hex window's edited pages
  and the media window's first and last MiB, and the result says what was checked) — refuse: nothing is
  written, and the edits stay where they were kept (the edited local copy, the window and its journal), named.
  No overwrite. A server that leaves out size, mtime, permissions, type or owner gets no write.
- **The upload route** (image and media Save, a local file's hex Save, `--app`, the terminal editor). Write =
  create `.<name>.sopen-<random>` exclusively in the same remote directory → through its open handle
  (fsetstat, never by path): the target's owner, its permission bits, the content, then the full mode again (writing
  clears set-id bits) → `fsync@openssh.com` if offered → check the temp has the target's owner and mode (refuse if
  not) → re-check → `posix-rename@openssh.com` over the target (plain rename if not offered). Only a temp this
  write created is ever
  removed; one that cannot be removed is named. The next baseline is the staged temp — its size and mtime as stat'ed
  right before the rename, plus the uploaded bytes' hash — never the target's stat after the rename; if the target
  differs from it right after the rename, that is reported ("saved, but the remote changed right after") and the next
  save refuses. A server without `posix-rename@openssh.com` cannot replace a file in one step: Save is refused before
  anything is uploaded, and Save copy (a new sibling) is offered.
- **A streamed hex file's Save moves bytes, not the file** ([07](07-streaming-edit.md) §4). Its route is decided
  before anything is written, from what the server offers and the free space in the target's folder:
  - **Server copy** (`copy-data` and `posix-rename`, room for the file's size + 1 GiB, or no `statvfs`): the server
    copies the file into the temp, in requests sized to about 1 s each with an fsync every 2 GiB or 10 s
    (`saving — copying on <host> 37 % (8.9 of 24.0 GB)`; Esc stops it between requests, the temp removed); the
    changed runs are written into the temp at their offsets; the temp is fsynced, given the target's owner and
    mode, and renamed over the target. Size and mtime are checked four times — at open, before the copy, after
    it and right before the rename — and the content once the copy is made: up to 256 MiB the whole temp is read
    back and hashed against the baseline, and at every size each edited page is read back and compared with the
    page as shown. A 4-byte Save of a 24 GB file sends 4 bytes of file content.
  - **Streamed rewrite** (no `copy-data`, at most 256 MiB): the same steps, with the copy, the patch and the
    content check one fresh pass through this machine, the patches applied on the way.
  - **Refused**, with the reason and the cost, before any transfer: above 256 MiB without `copy-data`
    (`rewriting 24.0 GB through this machine moves 48.0 GB …`), or without room for size + 1 GiB (`needs 24.0 GB
    free in /data, has 3.1 GB`). The refusal row offers **Write in place…** and **Save As…**.
  - **Write in place…**, only from that row, behind a dialog that says it is not atomic: the target is opened
    read-write without truncating, its edited pages are checked, the undo file is made durable, then the runs are
    written and fsynced. A write or fsync with no reply is an unknown outcome on the target itself (`<file> may
    hold some of your changes — check it; the original bytes are at <undo file>`).
  - A server without `fsync@openssh.com` cannot flush: the Save goes ahead and says so before it starts and in
    its result.
- Conflict detection is best-effort: it catches changes made before you save (size, mtime, and content hash up to
  256 MiB); it cannot lock the remote file, so a write racing the final rename is not detected. Above 256 MiB a
  Save checks size, mtime and what its window holds evidence for (a hex edit's pages, a media file's first and
  last MiB), and its result says so (`checked size, modification time and the 8 KiB under your edits, not the
  whole content`). The hex window's first Save of a session on a file above 256 MiB asks once: `A program
  writing to the file while it saves can lose that write`. Which writes a Save can lose — a change made after the
  copy read its region with the mtime put back, one between the last check and the rename — and which it keeps
  (one the copy captured outside the edited pages, above 256 MiB) is 07 §4.5's table.
- **The journal.** Every unsaved edit of the hex, image and media windows is kept on the local disk as well as in
  memory, in a new owner-only kind of file: `<cache>/.edits/<host>/<abs path>.journal-<UTC stamp>-<8 hex>/`
  (0700, its files 0600, created exclusively, never through a link, its creation fsynced in its parent), made at
  the first edit ([07](07-streaming-edit.md) §3). Each generation is one immutable file holding the baseline, the
  evidence and the edits together, made durable (fsynced, renamed to a new name, the folder fsynced) and named by
  `CURRENT`, which is replaced atomically. One is written at most 250 ms after a change, and at once on focus
  loss, at close and before a Save; the status bar says `unsaved — keeping…`, then `unsaved — kept at <journal>`,
  or `unsaved — NOT kept on disk (<why>)`. It is removed once nothing is unsaved, or by Discard, and kept on any
  other ending. The next open offers it in a recovery row — Restore (after checking the file's content against
  the journal's evidence), Later, Discard. A scripted run keeps none.
- **The save intent.** Every Save of every route records what it is doing in the journal's `save` file, durably,
  before each request that changes the remote: the temp's name before its exclusive create, the temp's stat once
  created, the staged temp's stat and the post-save evidence, then `renaming` ([07](07-streaming-edit.md) §3.1). A
  Save cut off (sopen killed, a reply lost) is reconciled from fresh reads on the next open or at the next Save's
  start: landed (`… landed (found now): nothing was written again`), not landed (its edits offered back), or
  unknown (the target and the temp named). A temp is removed only when the record proves it was the Save's;
  any other is kept, named, and its removal asked (Remove it / Keep it).
- Any save that does not land moves the edited copy aside to `<copy>.conflict-<UTC stamp>` and prints that path. A
  fetch never overwrites a cached copy holding unsaved edits: it moves it aside first, and says what is known — "had
  unsaved edits" when the copy differs from what sopen recorded, "no record of what was fetched … kept it … in case
  it holds edits" when there is no record.
- Local copies are the owner's only: cache, edit, backup and export directories are created 0700; fetched copies,
  staged and kept image, hex and media edits, backups and edits moved aside are 0600. A staged or kept image, hex or media edit gets a new name
  (`<stem>.<tag>-<UTC stamp>-<random>.<ext>`), created exclusively — never over or through an existing file.
- The cache's fixed roots are dot-named — `<cache>/.backup`, `<cache>/.edits` (staged and kept image, hex and media edits),
  `<cache>/.exports` (local exports before they are published), `<cache>/.revisions` (the git panel's diffs and files
  at a revision, a folder per browser window, removed when it closes; 03 §6.2), `<cache>/.sessions`, `<cache>/.meta` — and a host
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
  A lock found held is asked for again, every 5 ms for up to 100 ms, before the session takes a copy of its own
  (02 §4.4: a child of sopen's may hold a just-released lock for an instant).
  Records and edits moved aside are never written or chmodded through a symlink.
- Nothing waits forever: ssh keepalives (10 s × 3), 30 s per SFTP request, 60 s without transfer progress. Only an
  SFTP status reply proves a rename, link or exclusive create did not happen; a timeout or a lost reply (connection
  closed, EOF, garbled reply) is "outcome unknown — check <path>", never "nothing was written", and a temp or export
  that may have been created is named, not removed.
- **Backups.** Image Save (Cmd/Ctrl+S) overwrites the original **after** writing the original as opened (its
  bytes, held in memory) to the local backup `<cache>/.backup/<host>/<abs path>.<UTC stamp>` and printing that
  path; a local file's hex Save does the same (§6a), and a local file's media Save (§7) copies the file it
  plays, then renders. Two Saves keep no local copy of the original:
  - **A streamed hex file's Save** keeps a local undo file, `<cache>/.backup/<host>/<abs path>.<UTC
    stamp>.undo.txt` (0600): the bytes this session's Saves replaced, one `0xOFF: OLD` line per run, made
    durable before the target changes. It reverts sopen's own overwrites, not another writer's change a Save
    kept. The hex window lists a file's undo files in its recovery rows, and **Load as edits** makes those
    bytes unsaved edits, which a Save then writes with every check; nothing writes one back without a window
    and a Save ([07](07-streaming-edit.md) §5.1).
  - **A remote media Save** keeps the original on its own host: the session's first Save, once its render is
    staged, links the original to `.<name>.sopen-backup-<UTC stamp>` beside it (`hardlink@openssh.com`, never
    over anything), then overwrites. The status line says so while it renders, and the result names the link
    and its size: it is the original's inode, not a copy, so other links or programs that have it open can
    still change it. A Save that does not land removes the link (an unknown outcome keeps and names it). A
    server without hard links has no remote media Save; Save As and Export are offered (07 §5.3).

  "Save copy" writes `<stem>.edited.<ext>` next to the original instead (a streamed hex file's by the server
  copy or the streamed rewrite, published by a hard link, never over anything). Media Export never overwrites
  (sibling file; `-N` suffix if the name exists): it is published by hard link from a temp, or — where links
  are unavailable — written into a name created exclusively.
- **Save As** (Alt+Cmd/Ctrl+S, F7: the hex, image and media windows) writes the edited result to a new local
  file, by "Save to…"'s rules (§6a: a private staging folder, a hard-link publish, never over anything, nor
  through a link), after checking the chosen folder's free space; the hex window reads the unchanged bytes
  fresh from the remote and applies the edits on the way, the image window writes its encoder's output, and
  the media window renders in sopen's own exports folder and copies the render in. Its modal shows the size and
  the time expected, its progress and Cancel ([07](07-streaming-edit.md) §7).
- Live receipts run only on files created under the remote `/tmp` for that purpose.

## 6. Image window (A3)

- Opening ([06](06-streaming.md) §7.3): the header first, by a walk capped at 1 MiB (a JPEG's marker segments
  up to its SOF, read by sopen's own walk), then the whole file into memory, with `W×H, N MB, loading N %`, the
  Exif thumbnail scaled up and, for JPEG and PNG, a partial picture as the bytes come; Esc stops the read and
  keeps nothing. A decode that needs over 512 MiB, or a file over 512 MiB, first shows the memory needed and
  the machine's (`… — Continue (Enter) · Hex view (H) · Close (Esc)`), and nothing more is read until the owner
  continues; this applies to local images too. Hex view, and a file that does not decode, open the hex window
  in place, on the same stream for a remote file, the reason first in its rows.
- Fit-to-window view, zoom (wheel / pinch), pan; status bar shows image size, rectangle `x,y w×h`.
- Drag = rectangle in image pixels (handles to adjust). `C` Crop, `P` Pixelize (block size, default 16 px), Undo/Redo
  (Cmd/Ctrl+Z / Shift+Cmd/Ctrl+Z), Cmd/Ctrl+S (F5) Save, Shift+Cmd/Ctrl+S Save copy, Alt+Cmd/Ctrl+S (F7) Save
  As (§5), Esc clears the rectangle.
- Edits are kept in the journal (§5) as operations on the file as last saved — `crop`, `pixelize`, `cut`,
  `paste` with its picture kept by its hash, `text` — and Restore replays them on the decoded original once its
  SHA-256 equals the journal's. An Undo back past a Save leaves a picture the journal cannot replay: the status
  bar says `unsaved — NOT kept on disk (undone past the last save, …)` until the next Save, and the close
  prompt still guards it ([07](07-streaming-edit.md) §8).
- While a Save, Save copy or Save As runs the window takes no edit, undo or redo (`saving — editing resumes
  when the save is done`); the status row says where the Save is (`encoding`, `uploading`, `checking the file`,
  `replacing the file`) and Esc stops it up to the rename — the temp removed, the original untouched. Closing
  or leaving the viewer waits for it. The result is `saved host:/abs · original backed up at <path>`.
- Closing with unsaved edits asks Save · Save As… · Discard · Cancel (Save copy where Save is off); the
  window closes only after a save that landed.
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
is refused the field keeps the keyboard, so the next Enter saves again); during a "Save to…" Enter
does nothing, and Esc, Cancel or closing the window cancels it: the transfer stops between two
responses, nothing is left behind, and the terminal says `cancelled; nothing was saved`
(message 17, [06](06-streaming.md) §7.5). A cancel that comes once the complete file has its
name is too late, and the save is reported as saved. While the hex window is being opened Enter and Esc do
nothing and closing waits for it.

- Hex: a remote file is opened as a stream on the dialog's own connection (message 17,
  [06](06-streaming.md) §7.1, §7.5): nothing is fetched, the same window becomes the hex window,
  and it reads the blocks its screen needs. A local file is read in place, and a copy already in
  the cache (a `.ts` that turned out not to be media) is opened as it is. Every step that can fail
  (opening the file, its handle's stat) comes before the connection is handed to the hex window:
  after a failure the dialog is as it was, with its connection, for a retry or "Save to…". The
  hex window's first Save connects again and saves by §5's routes for a streamed file: nothing
  of the file is fetched for it.
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
  names inside its private folder, then the folder (a failure to remove it is said). Without a
  window, Ctrl-C and SIGTERM cancel the save and leave nothing; a save killed outright leaves only
  that hidden folder, which the next save into the same folder removes when it is a partial —
  each save holds a lock on `lock` inside its private folder and records there the size it
  expects, and a folder goes only when that lock is free, `part` is shorter than the recorded
  size and nothing but sopen's own staging files is in it; a complete `part` is never removed
  (06 §7.5). A download
  whose source changes meanwhile is not saved — a best-effort check: the remote's size and mtime
  after the transfer, and the bytes received, against its stat before. A local file (or the cached
  copy) is copied the same way, checked by its handle: size, mtime, change time and bytes copied.
  Then "saved <path>" shows a moment, the same line is printed on stdout, and the window closes —
  unless there are notes to read (a Close button then).
- Closing the window while the hex window is being opened does not cut the opening: the window says
  it closes when that is done, and does; closing during a "Save to…" cancels it (above). A transfer
  whose worker ends without a result is a final, visible failure (the window says so and offers
  only Close; a close asked for meanwhile closes it). The line on the terminal when nothing was
  opened or saved says "nothing was downloaded" only when nothing was started.

`sopen --hex SOURCE` opens any file (any kind; not a folder) straight in the hex window — no dialog;
a remote one opens as a stream on the open's connection, nothing fetched ([06](06-streaming.md)
§7.1). `--hex` and `--app` are exclusive. With no display (Linux, neither `DISPLAY` nor
`WAYLAND_DISPLAY`) the dialog and `--hex` fall back to fetching the file and printing its local
path, with one line on stderr saying why.

**The hex window** has the image and media windows' look and layout rules: a one-line toolbar on
top, a one-line status bar at the bottom, nothing that changes size while you work. The grid's top
and every row's position never move: not when the status changes, and not when a report arrives —
the disclosure rows (what must be read whole) sit between the grid and the status bar, appear only
once the primary button is up, and take rows off the grid's bottom.

- Rows of 16 bytes: the offset (hex, as many digits as the file size needs, at least 4), the 16 bytes
  in hex grouped 8 + 8, and the ASCII column (non-printable bytes as `.`). Only the rows on screen are
  read and painted: the file is read through a small page cache (at most 64 pages of 4 KiB; the least
  recently used goes first), never whole, so a file of any size scrolls the same. The grid has its
  own scroll bar (row positions are kept as 64-bit row numbers, not as pixels). Over a stream
  ([06](06-streaming.md) §7.1) a page not here yet shows `··` in the dim colour and the status row
  says `loading 0x…`; a failed one says why and is retried; the stream's warning (the file changed
  or was replaced on the remote, a failed read) stays in the rows below the grid, and Cmd/Ctrl+R
  reloads (refused while edits are saved nowhere). A drag of the scroll bar reads only where the
  thumb rests.
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
  and redo step records the bytes shown before and after) — the file and any cached copy are
  never written by an edit. Each edit keeps the byte it replaced, read when it was typed, and
  whether a byte is changed is a comparison with that byte — the file is not read again — so a
  byte typed back to its value is no longer a change. A key on a byte whose page is not here
  changes nothing (`0x… is still loading`). "Unsaved" is by content: edits undone back to what
  the file holds (as opened, or as last saved) are not unsaved. At most 100,000 offsets are
  changed at once (`too many changes for one save — save first`). Changed bytes are marked in both
  columns. Undo/Redo: Cmd/Ctrl+Z, Shift+Cmd/Ctrl+Z.
- Status bar: the cursor's offset (hex and decimal), its byte (hex, decimal, the character when
  printable), the file's size (the exact count on hover), "N bytes changed", "unsaved", and the
  last result.
- Save (Cmd/Ctrl+S, F5) of a **streamed file** — a remote file the window reads through its stream —
  is §5's server copy or streamed rewrite on the window's own connection, made at that first use
  (an sshd that refuses the session fails the Save before anything is sent, the edits kept):
  nothing of the file is fetched, its backup is the undo file, and every phase is in the journal
  first. Its progress is the status row's; Esc stops it between its requests. After it lands the
  window reloads its stream, and each saved edit drops out without a read. Save copy
  (Shift+Cmd/Ctrl+S) publishes `<stem>.edited<ext>` beside the original by the same routes, never
  replacing anything.
- Save (Cmd/Ctrl+S, F5) of a **file with a local base** — a local file read in place, or a remote
  file's copy in the cache — is image Save's contract (§5), from one pass over the bytes as opened: the
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
- Save As (Alt+Cmd/Ctrl+S, F7, or the toolbar's Save As… button) writes the edited file to a new
  local file (§5); a completed one counts as Save copy does for the close prompt.
- While a Save, Save copy or Save As runs the window takes no edit, undo or redo; a key that would
  edit is refused and the status row says `saving — editing resumes when the save is done`. The view
  still scrolls.
- Every edit is in the journal (§5) as well as in memory; the status bar's right end says `unsaved —
  kept at <journal>` (`unsaved — kept` where there is no room for the path). On open, the window
  lists in its recovery rows, as buttons (a letter types a byte): the journals an earlier session
  left (Restore — which reads the pages under the edits and applies nothing if any differs —
  Later, Discard), a Save that session left cut off (its outcome, as §5 reconciles it), and the
  file's undo files, newest three (Load as edits, Later). A row's buttons are off while a Save runs.
- Closing with unsaved edits asks: Save (Save copy where Save is off) / Save As… / Discard / Cancel;
  Discard's hover names the kept edits it deletes. Closing during a save waits for it. Either way
  the window closes only when the save landed with nothing to read and nothing unsaved; one that
  did not land, or landed with notes (the remote changed right after), keeps the window open; newer
  edits ask again. A close asked for during a save is used up by that save, whatever it reports: it
  never closes the window after a later Save. Once closing is decided nothing edits.

## 7. Media window (A4, A5)

- Probe with `ffprobe` (duration, streams, size, each audio track's channels and rate, chapters). `ffmpeg`/`ffprobe`
  are resolved at runtime (`SOPEN_FFMPEG`, then PATH; the brew formula depends on `ffmpeg`). Every run on the file —
  probe, playback, the paused frame, the waveform, Save, Save As, Export — reads one input value (`ffmpeg::Input`):
  a local file's path, or — for a remote file — a URL on the window's loopback byte server
  (`http://127.0.0.1:<port>/<token>/<kind>/<name>`, one server per window, 64 connections at most), whose bytes
  all come from the window's stream ([06](06-streaming.md) §7.4, §9). The URL's kind picks the reader: `play`
  for playback, the paused frame and the probe; `bulk` for the waveform; `render` for Save, Save As and Export.
  A probe has 30 s, then ffprobe is killed. A render one of whose responses the server cut short (the file
  changed on the remote, say) is failed whatever ffmpeg's exit status — ffmpeg can exit 0 on a short input — and
  publishes and uploads nothing.
- Video: playback in the window with sound (ffmpeg subprocess decode → frames; audio → `cpal`; audio clock master),
  timeline scrub, drag = crop rectangle in source pixels, Space play/pause, Export.
- Audio: waveform — min/max peaks per channel from one decode at 8 kHz, at most 100 per second and 2^19 per lane (an
  hour of stereo is a few MB; the PCM is never held), handed over as they fill; a stereo file shows two lanes, L and
  R (more channels are mixed down to two); same transport, Export. A remote sound's decode reads its `bulk` URL,
  so it yields to playback, and the waveform is drawn as far as it has been read while the rest comes. A decode
  that fails (a read error, ffmpeg's error exit — its last words are kept) is said where the waveform is, never
  drawn as a finished flat line.
- Editing (msg 13): the timeline is a list of clips, each a range of the source in ticks — video frames (a cut is a
  frame time, and the sound cut with it cannot drift) or audio samples. A drag on the timeline selects (the playhead
  follows the pointer, so the frame there shows), a click moves the playhead and clears the selection, `I`/`O` set
  the selection's edges at the playhead (`←`/`→` 1 s, `⇧` 5 s, `⌥`/Alt one frame or 10 ms). Cmd/Ctrl+C copies the
  selection, Cmd/Ctrl+X cuts it (removed, the gap closes, kept to paste), Delete/Backspace removes it, Cmd/Ctrl+V
  inserts what was copied at the playhead or replaces the selection; Cmd/Ctrl+Z / Shift+Cmd/Ctrl+Z undo / redo (the
  playhead and selection come back with the step). The clipboard holds ranges of this file only — the system
  clipboard is not used (a Cmd/Ctrl+V whose press egui-winit swallows on an empty system clipboard is taken at its
  key-up). `M` (audio, mono file only) = Mono → Stereo: left = right = the channel, at its level. Neighbouring
  pieces that continue each other are one clip, so an edit undone by hand is no edit. Nothing a key does runs ffmpeg
  or ffprobe; keys follow the image window's gating (none while a text field has the typing or a prompt is up, and
  no edit while a Save runs), and the row shows `5 Save` and `7 SaveAs`.
- Preview: playback plays the clips in order, each piece by an ffmpeg of its own started as soon as the one before it
  starts. Picture and sound each lay one grid over the whole play — piece `k` gets `round(end_k · rate) −
  round(start_k · rate)` frames or samples — so no rounding builds up between them and a piece shorter than a frame
  adds no frame (never stretched to one); frames are timed by their index. Both are anchored at the cut, as Save
  is: a stream that starts later than the cut keeps its offset (silence first, the first picture held). A decoder
  that ends early by itself is padded (silence in blocks of at most 8192 samples, the last picture held), never
  after a stop; one that cannot start, whose output cannot be read or that exits with an error ends playback with
  the reason on the status line. A decoder taken out of the running list to be waited for is killed and reaped
  however its piece ended (stopped, unreadable, short); its error output is read to the end and only its last lines kept
  (64, at most 2 KB; a line that never ends cut and marked `…`, as `sopen stems` keeps 16 KB). Measured 2026-11-20 (release build, null sink; each wait from the end of the piece before — its padding
  too — to the piece's first output): 64×64 at 25 fps — video 0.02–1.8 ms, audio 0.01–1.5 ms; 1080p long-GOP
  (keyframes 10 s apart, cuts 8–9 s past one, decoded 1280×720) — video 1.1–4.9 ms, audio 0.02–1.6 ms; against
  ~240 ms of queued frames; no frame dropped. Short pieces late in a GOP at the start of a play (0.6 s then 0.4 s,
  1080p) wait up to ~150 ms for sound: the first second of sound is read far faster than it plays (it fills the
  1 s queue), so the next decoder has had that long, not the piece's length, to seek; the queue covers it (no
  underrun). Paused, the frame at the playhead is the source frame the timeline maps it to — at most the picture's last
  frame (past it, a longer sound track, that frame holds; a last frame that starts before its slot is the latest
  picture of the two seconds before the slot's end, read no further; anywhere else a frame that cannot be decoded
  is an error, never another picture; each of its decodes reads at most 3 s past the slot, and all of them together
  have 5 s: then the ffmpeg is stopped and reaped, and the status says there is no frame here); the last decoded frames are kept (64 MB), so the frame an
  edit brings under the playhead is usually there at once (measured 1–2 ms; a frame not kept: one decode, 70–95 ms).
  An edit while playing restarts playback at the playhead. On video, timeline frame slot `k` is the source frame at
  `origin + k / fps`, `origin` the picture's offset within a frame period from the file's start (a transport stream
  whose sound starts first has its frames 21.3 ms in): picture and sound are cut at the same instant.
- Save (Cmd/Ctrl+S, F5): refused before anything is backed up or rendered when the file changed since it was opened —
  by the overwrite's own rule: type, size, modification time and (up to 256 MiB) content, against the baseline a
  worker takes (§5: the file's whole SHA-256 up to 256 MiB, above that the SHA-256 of its first and last MiB,
  read again right before the overwrite; a local file's from when the window opens, a remote file's from its
  first edit, a Bulk read of its stream with `checking the file for Save — 37 %` on the status row; a render
  can take minutes; a baseline that failed refuses every Save until the file is opened again) — or when the server cannot replace a file in one
  step, or, for a remote file, has no hard links; else the original is backed up and the edit rendered into a
  staged file under `.edits` (§5, once per session: a local file is copied to the local backup before the
  render, path printed; a remote one, once its render is staged, is linked to `.<name>.sopen-backup-<UTC stamp>`
  beside it, which the status line says while it renders and the result names), and the staged file is written
  over the original by the conflict-checked overwrite (§5), each of its phases in the journal first. While it runs nothing is edited, and Esc stops it before its rename. Then the window plays the
  saved file, unedited (the clipboard and the history were ranges of the old one); a remote file plays from the
  staged render — still locked, so a sopen killed later leaves it to the sweep below — removed when the window
  goes, and the window's stream and byte server are closed. Save keeps the file's format: a file whose container
  cannot take the re-encoded streams (`.webm` video) is not saved — Export writes `.mp4`. Edits make the window
  unsaved and are kept in the journal (§5: the clip list in ticks, Mono → Stereo, the crop, In/Out; Restore checks
  the file's whole content at most 256 MiB, above that its size, time and first and last MiB): closing, F3,
  `close=ask` and leaving it in the browser ask Save / Save As… / Discard / Cancel as the image window does; a
  running save closes the window when it is done; a running export keeps the close-again rule. A save or export
  whose worker ends without a result (it panicked) is a visible failure; the edits stay and the window is usable.
- Save As (Alt+Cmd/Ctrl+S, F7) renders the edit in sopen's own exports folder, after checking the free space there
  against its estimated size, and copies the render into a new local file by §5's Save As rules; a render cut
  short publishes nothing.
- A render's work folder (`.exports/<random>`), its staged file (`<stem>.render-…` in `.edits`) and a `sopen stems`
  job's work folder (`.exports/<random>` from the command, `.exports/stems-<random>` for the window's child) each have a
  `<name>.lock` held while the job runs; a sopen killed meanwhile leaves them to the next media window's, `sopen
  cut`'s or `sopen stems`' start, which removes every one whose lock nobody holds (a kept render — a failed save names it — has no
  lock and stays). A job removes its lock file before letting go of it, and the sweep removes a job only while the
  lock it got is still the file at that path, so a job that ends while the sweep looks keeps what it kept. The render's ffmpeg dies with sopen, SIGKILL included: a small `sh` in a process group of its own
  reads a pipe only sopen writes, and when the pipe closes it kills that ffmpeg if its command line still names the
  program.
- Render (Save and Export): exact for any number of pieces without decoding the source once per piece — pieces that
  follow each other in the source (within 10 s) share one input (`-ss` 0.2 s before the first, `-t` past the last,
  `split`), a piece out of order gets its own; every stream of a piece is cut at the piece's start and moved by the
  same amount (video from half a frame before it), so picture, sound and tracks keep their offsets — a stream that
  starts late keeps its gap, filled (`fps=…:start_time=0` holds the first picture, `aresample=first_pts=0` gives
  silence) — then counted out to the grid's budget (`trim=end_frame`; `atrim=end_sample` per track at its own rate;
  padded if short), and each stream joined by a `concat` of its own; the joined frames are timed by index (a
  one-frame piece's frame carried no duration through `concat`, and the next frame took its time and was dropped).
  Frame rates are given as fractions (`30000/1001`). Input-side `-t` alone is not exact (AAC came out a frame short), nor a trim right at the seek
  point (AAC 154 samples off, MP3 9); 0.2 s of run-up made both sample-exact. One run opens at most 24 inputs (300
  took 1.6 GB and ran out of descriptors at `ulimit -n 256`); more are rendered in lossless passes (H.264 `-qp 0`;
  each track as PCM of its own sample format — s16, s32, s64, f32 or f64 — in Matroska) and a final one whose pieces
  carry the sums of the grid's budgets, so the result is the one-pass result sample for sample; more than 576
  separate stretches are refused. A long filter graph goes to a file
  (`-/filter_complex`, ffmpeg 7+). Export's crop applies once, to its whole output (Save writes the edit list only:
  a rectangle drawn is said on the status line, not saved); Mono → Stereo pans each mono track
  `pan=stereo|c0=c0|c1=c0`.
- Export: In–Out of the timeline (the whole timeline without a selection) and the crop, re-encoded (H.264 + AAC for
  video; the source codec for audio) by the render above; progress shown; result uploaded as a sibling (§5).
  Its progress row has **Cancel**, and Esc does the same while it runs: during the render ffmpeg is killed and the
  render removed, during the upload the upload stops and its remote temp is removed, and the result says `export
  cancelled: nothing was published`; once the publishing link has landed it is too late, and the result names
  the sibling. A close still waits (the second close closes, and sopen waits and prints the result).
- Save and Export map streams explicitly: the first picture stream and every audio track (one, for one-track
  containers); >8-bit sources stay 10-bit where libx264 has it; colour tags are carried. What is not kept (subtitles,
  data, attachments, cover art, alpha, depth, chapters) is named in the window before Save or Export and in the
  result line.
- Every save, export and `sopen cut` run has a progress-stall watchdog: an ffmpeg whose output position, frame count
  and size have not moved for 120 s is stopped and the error says so (ffmpeg 7+ keeps printing progress blocks while
  stuck, so it watches what they say, not whether they come; a libx264 deadlock once ran 44 h unstopped).
- `sopen cut <source> <in> <out> [--crop x,y,w,h]` = the unedited export (`-ss In -to Out -i src`), headless
  (scripting + CI receipts); it always runs on this machine (§4a).
- A raw `.aac` (ADTS) or `.ts` output decodes longer than its cut by the AAC encoder's 1024-sample priming plus the
  padding of its last frame (1280–1883 samples measured): neither container can say to skip them (MP4 and Matroska
  carry it). Inherent to the containers; the picture and the sound's timing are right.
- Renderer / playback details fixed by the spike (§11).

## 7a. Voice / music separation (A16)

`sopen stems SOURCE [--model PATH]` splits a file's audio into the voice and the music. It has
`sopen cut`'s shape and rules: SOURCE is local or `[user@]host:path`, a remote file is fetched
into the cache (claimed while it runs), and it always runs on this machine (§4a).

- **Engine.** sherpa-onnx v2.0.2's C API (Apache-2.0; ONNX Runtime, MIT, inside), linked
  statically behind the cargo feature `stems` (on by default; every release build has it).
  build.rs downloads the pinned prebuilt archive for the *target* and checks its SHA-256
  (`SHERPA_ONNX_LIB_DIR` = an extracted `lib/` for offline builds; a changed archive there, or
  one gone from the download, rebuilds; the download's stamp is removed before its extraction
  changes and written only once every library is there); `--no-default-features`
  builds without it and `sopen stems` then says so. sherpa-onnx's Rust crates do not bind source
  separation, so the few C declarations are sopen's own. CPU, every core.
- **Model.** UVR-MDX-NET-Voc_FT from Ultimate Vocal Remover (Anjok07 & aufr33, MIT — credited in
  `--version`, the long help and the README, as its authors ask) in sherpa-onnx's ONNX
  conversion: `UVR-MDX-NET-Voc_FT.onnx`, 66,762,795 bytes, SHA-256
  `e411182ce2c53541fefdcc99a8f46f2fe03978eb22038b9497c1d9d95a00fad4`. It is not one of sopen's
  release assets: the formula's `resource` stages it into `pkgshare`. sopen looks for it at
  `--model PATH`, else `SOPEN_STEMS_MODEL`, else `<keg>/share/<formula>/UVR-MDX-NET-Voc_FT.onnx`
  next to the canonical binary's `<keg>/bin/` (`<formula>` = `sopen` for a release build,
  `sopen-preview` otherwise), and checks size and SHA-256 before anything is fetched. Missing
  or not exactly that file: one error naming the path it expected. sopen has no network code.
- **Separation.** ffprobe (an audio stream is required) → ffmpeg decodes the first audio track
  to stereo f32 at the model's rate (44.1 kHz) on a pipe → the engine separates it in pieces of
  30 s, each with 1 s of the file's own audio on both sides (silence past its ends) that is cut
  off its output again — the engine distorts a window's edges, and the context keeps that out
  (the research spike measured it lossless against one pass) → each stem goes to ffmpeg on a
  pipe; of each ffmpeg's stderr only a bounded tail is kept (64 lines, 16 KiB; an endless line
  cut), its last line being the error. Memory is one window plus the model, however long the track (measured flat: 6,529 MiB
  after the first of 8 pieces, 6,535 MiB after the last). The model's music is the window minus
  its voice, so voice + music give the source back sample for sample (the test).
- **Output.** Each stem in the source's audio codec, sample rate and channel count (mono stays
  mono — through the engine as two equal channels, back as their mean, by explicit `pan`s, so
  the level is kept; more than two channels are separated as stereo, downmixed normalised as
  ffmpeg downmixes for integer output (`-rematrix_maxval 1.0`: nothing over full scale), and a
  note says so). At the source's own rate each stem has exactly the source's sample count: the
  resampling to the engine's rate and back can add a sample at the end (in AAC a whole frame),
  so when the rates differ the source's count is taken from a decode of its own, run while the
  first piece is separated, and each stem is padded or cut to it (a lossy codec such as AAC may
  then decode up to one codec frame longer: its encoder pads the last frame). A video source
  gives AAC in `.m4a`. Only the first audio track is separated (a note names the others). Both
  are published as new siblings the way `sopen cut` publishes (§5: never over anything, `-N`
  when the name is taken — the first number free for both, so the pair carries the same number
  unless another program takes one of the names between that look and the two publishes, which
  are each on their own; nothing is overwritten either way): `<stem>.voice.<ext>`, then
  `<stem>.music.<ext>`; the paths actually published are printed on stdout, voice first. A music stem that cannot be published after the voice was is
  an error that names the published voice.
- **Progress** on stderr, one line per finished piece, parseable:
  `stems: piece 3/8, 61s elapsed, 102s remaining` (`0/N` with no `remaining` before the first;
  `N` from ffprobe's duration until the decode ends, then exact; a terminal sees the line
  updated in place). Exit codes as `sopen cut`: 0, or 1 with one error line.
- **Cost** (measured on an 18-core Apple M-series CPU, 2026-11-20): a 4-minute track took 198 s
  while the machine was busy with other work (load average 150–200; the research spike's idle
  runs: 147–160 s), at 6.4 GiB peak RSS; on the spike's 0 dB voice/music mix both stems scored
  14.04 dB SDR, the spike's own figure. So the window never separates in its own process: `stems::spawn_child` runs `sopen stems --work-dir
  DIR --wait-to-publish -- SOURCE` as a child in a process group of its own (its ffmpeg children
  with it), in a work folder the parent made, and reads its progress lines. Cancel kills the
  group, removes the folder and publishes nothing. The child does not publish on its own: once
  both stems are encoded it prints `stems: ready to publish` and waits for `publish` on its
  stdin; the parent answers under the lock Cancel takes, so a cancel either comes first (nothing
  reaches the remote — an upload is never cut half-way, leaving a temp there) or after the
  go-ahead, when it is too late and the stems are published. The child's exit and its output are
  separate: once the child has exited — seen without reaping it, so its id is still its own —
  whatever it left in its group is killed, then it is reaped and the work folder removed; its
  result is read only when both pipes are closed, and looking for it never blocks. Nothing
  signals a reaped child's id again (it may be anyone's by then). The window's button is a later
  change.

## 8. Usage log

One JSON line per run in `<state>/usage.jsonl`: `ts, host_hash, kind, ext, bytes, fetch_ms, bytes_fetched, action (open|save|export|cut|stems|discard), conflict, ok, forwarded, target_hash`.
`bytes_fetched` is what every stream of the run received from its remote ([06](06-streaming.md) §11); a
streamed open keeps `bytes` (the file's size) and has `fetch_ms` 0.
Hosts are hashed, never written by name. A run forwarded with `--target` (§4a) is logged on both machines: here with
`forwarded: true` and the target's hash (its kind is known only there), there as an ordinary open.
Local only, never sent anywhere. Exists for §10.

## 9. Out of scope (until §10 says otherwise)

Own text editor (the browser's text viewer is read-only; VS Code or Zed edits),
hex insert / delete or anything else that changes a binary file's size, image edits beyond crop, pixelize and
clipboard copy / cut / paste (no drawing or text tool, no layers, no filters), media edits beyond the time cut,
cut / copy / paste on the timeline, the video crop, (audio only) Mono → Stereo and `sopen stems`' voice / music
split into new files (msg 13: no fades, gain, mixing, effects, transitions or separately edited audio and video),
the system clipboard for media, frame-accurate copy without re-encode, directory sync / mounts, file watching,
Windows, a `.app` bundle or cask, any agent on the remote, macshot (Swift, macOS-only, GPL-3 — incompatible with A7/A8).

## 10. Two-week review (from 2026-11-xx first preview)

From `usage.jsonl` across machines: < 5 runs/week, or ≥ 80 % text/dir → archive, keep a `zed ssh://` alias.
A conflict refusal or failed write → fix before anything else. Streaming no longer waits on this
review: owner message 17 asked for it ([06](06-streaming.md), [07](07-streaming-edit.md)). The owner
naming the same missing edit 3 times → that one edit, nothing more.

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
