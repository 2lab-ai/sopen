# AGENTS.md — sopen

Single source of truth for anyone (human or agent) changing this repo. `CLAUDE.md` only imports this file.

## What sopen is

`sopen [user@]host:path` opens a remote file or folder as if it were local. A folder opens in sopen's own file
browser (two panels, Total Commander-like; in preview mode, the default, the side opposite the active list previews
the file under the bar, and F9 switches to two lists; Enter shows a file in the viewer for its kind in the same
window — a read-only text viewer with syntax highlighting, or the image, media or hex window below; F2 renames, F5
copies and F6 moves, never over anything; F4 hands the entry to the editor; F10 shows the directory tree in
place of the active list, the other side frozen until it closes). Text goes to VS Code when it
is installed (remote ones only with its Remote - SSH extension), otherwise Zed's remote mode; images, video and
audio open in sopen's own small window with exactly these edits: rectangle crop, rectangle pixelize and clipboard
copy / cut / paste (images: cut leaves the background colour; paste lays a picture — stretched, fitted or 1:1 — or
text into the rectangle), rectangle crop, time cut and cut / copy / paste on the timeline (video), time cut,
cut / copy / paste and Mono → Stereo (audio), and — as a command, `sopen stems` — voice / music separation into
two new files (spec §7a). Any other file (a zip, any binary) opens a dialog before
anything is downloaded: sopen's hex window (the default; byte overwrite only) or "Save to…" a chosen path;
`--hex` opens any file in the hex window. It opens on the machine it was typed on, or — with `--target HOST`,
`SOPEN_TARGET` or `target` in sopen's config — on HOST's screen, by running HOST's installed sopen with `--here`
over ssh: HOST
is a machine the owner uses sopen on, not a file's remote, which still needs only sshd (spec §4a). An `http://` or
`https://` address as SOURCE opens in the default browser of the machine that shows things; on HOST a loopback one is
served from this machine through an ssh tunnel of its own that lives exactly as long as this sopen (spec §4a,
`src/url.rs`, `forward::tunnel`). On macOS,
while a sopen window is in front, the top row is meant to send F1–F12 without fn (the key half is the owner's
receipt K-F3, still open): `fkeys` in sopen's config —
`"function"` (the default) or `"system"` (never touched), set by `sopen config fkeys function|system` — and
sopen sets the global `HIDFKeyMode` to 1 and puts back what it found once none of its windows is in front,
even when it dies (lock, state file, watchdog child; `.prd/04-file-keys.md` §6, `src/fnkeys/`); a run a hook
drives (`SOPEN_SCREENSHOT`, `SOPEN_AUTOSAVE`, `SOPEN_AUTOEXPORT`, `SOPEN_LIST`, `SOPEN_KEYS`) and the tests never
touch it, and Linux has no such switch. A remote
file that one of sopen's own windows shows is read as a stream, never downloaded first, whatever its size
([`.prd/06-streaming.md`](.prd/06-streaming.md)); its edits live in memory and in a local journal until they
are saved, and Save writes to the remote by default, Save As to a local path
([`.prd/07-streaming-edit.md`](.prd/07-streaming-edit.md)). Spec and
scope: [`.prd/01-spec.md`](.prd/01-spec.md) — its §1 owner intent and §9 out-of-scope list decide every "should
we add X" question; the browser workstream (modes, git panel, file keys, directory tree) is
[`.prd/02-architecture.md`](.prd/02-architecture.md) to [`.prd/05-directory-tree.md`](.prd/05-directory-tree.md).

## Gate (run before every commit, CI runs the same)

```sh
just check   # cargo fmt --check · cargo clippy --all-targets --locked -- -D warnings · cargo test --locked
```

A change is done when the gate is green on macOS and Linux (CI) and, for anything user-visible, a receipt
exists: a command output or a window screenshot that someone actually looked at. Locally, tests that need ffmpeg,
zsh, fish, node or OpenSSH's `sftp-server` skip with a note when it is missing; CI installs them (node is on the
runners, `sftp-server` on macOS) and sets `SOPEN_REQUIRE_FFMPEG=1`, `SOPEN_REQUIRE_SHELLS=1`,
`SOPEN_REQUIRE_NODE=1` and `SOPEN_REQUIRE_SFTP_SERVER=1`, so there they fail instead. The separation tests
(`sopen stems`) need the model: they skip unless `SOPEN_STEMS_MODEL` names it; CI downloads it (cached, its
SHA-256 checked) and sets `SOPEN_REQUIRE_STEMS=1`. The default build links sherpa-onnx (feature `stems`,
downloaded and checked by build.rs; `SHERPA_ONNX_LIB_DIR` for offline builds); `cargo build
--no-default-features` builds without it.

## Rules

- **The remote needs only sshd.** Never install, upload or run a helper on the remote. Transport is the system
  `ssh` binary (so `~/.ssh/config` is honoured); file operations are SFTP over that session. The git panel
  runs the remote's own `git` over the same master, its words quoted as `--target` quotes them, and uploads
  nothing (`.prd/03-git-panel.md` §3.2); without a git there, it is not offered.
- **Never overwrite silently.** Every write re-checks the remote file first (`lstat`: still a regular file,
  same `(size, mtime)`, same content hash up to 256 MiB) and refuses on change; writes go to a temp name in the
  same directory with the target's owner and mode and are renamed over the target. Spec §5 is the full rule.
  Above 256 MiB the content check is what the window holds evidence for — the hex window's edited 4 KiB
  pages, the media window's first and last MiB — besides size and mtime; the result says what was checked,
  and the hex window's first Save of a session on such a file asks once, because a concurrent writer's
  change can be lost (07 §4.5 has the table of what a Save can lose). These checks are best effort, never a
  compare-and-swap. Every unsaved hex, image and media edit is kept in a local journal as well as in memory,
  and every Save records its phases there before each request that changes the remote, so a Save cut off is
  reconciled, never repeated blindly (07 §3–§3.1). Image and hex Save, and media Save of a local file, keep a
  local backup of the original first — except a
  streamed hex file (a remote file in the hex window), whose Save copies the file on its host, patches the
  copy and renames it over the target, checking the pages under the edits (and up to 256 MiB the whole
  content) on the way; its backup is a local undo file of the bytes it replaced, durable before anything is
  patched (`.prd/07-streaming-edit.md` §4–§5). Writing a streamed hex file in place is offered only from the
  refusal row where no atomic route fits, behind a dialog that says it is not atomic — never as a fallback
  (07 §4.4). Media Save of
  a remote file keeps the original on its own host instead: its first Save of a session links it to
  `.<name>.sopen-backup-<UTC>` beside it (a hard link, never over anything) before the overwrite, says so
  before and after — it is the same inode, so other links or open handles can still change it — and is off
  where the server has no hard links (07 §5.3). Save As writes a new local file by "Save to…"'s rules, never
  over anything (07 §7). Media exports always write a new sibling
  file and never use a replacing rename to publish it. The binary dialog's "Save to…" fills a file in a private
  staging folder and publishes it complete by a hard link, never over anything (spec §6a). The browser's rename,
  copy, move and make folder never replace an existing name; a remote rename is SFTP's plain rename, never
  `posix-rename@openssh.com` (`.prd/04-file-keys.md` §5).
- **Edit features come only from owner messages.** The viewers' edits are the list above (owner, 2026-11-11;
  hex overwrite added by spec §1 message 9; image clipboard copy / cut / paste by message 12; media cut / copy /
  paste, audio Mono → Stereo and voice / music separation, `sopen stems`, by message 13), and the viewers keep
  exactly those. The browser's own edits come from spec §1 messages 13–16: rename, copy, move, make folder and
  delete (message 15, `.prd/04-file-keys.md`) and the git panel's actions on a repository, every history rewrite
  or destructive one backed up first (message 14, `.prd/03-git-panel.md`). Anything beyond them waits for an
  owner message.
- **ffmpeg/ffprobe are external programs**, resolved at runtime (`SOPEN_FFMPEG` / `SOPEN_FFPROBE`, then PATH).
  Do not link libav*. The one linked engine is sherpa-onnx (with ONNX Runtime), statically, for `sopen stems`;
  its licences and those of what it brings are `THIRD-PARTY-NOTICES`, which must say what the binary links.
  The separation model is never in the repo or the release assets (the formula's resource; spec §7a).
  ffmpeg versions differ in how they *decode* AAC trims a file carries: ffmpeg 9 applies an MP4 edit list's end
  and Matroska's CodecDelay, ffmpeg 8 (Ubuntu 26.04's, the CI Linux runner's) applies neither — both write the
  same trims. The render tests therefore hold an AAC render to a plain encode of the same sample count by the
  same ffmpeg (`pcm_as_plain` in `src/cut/edit_tests.rs`) and read what the file declares without a decoder (an
  MP4's `stts` and edit list must say exactly the timeline's samples; Matroska declares only its priming, as
  CodecDelay, so there the end may run on by less than one AAC frame), and read frames with `-fps_mode passthrough`.
- Tests must not need ssh or a network: the remote layer sits behind a trait with a local-filesystem
  implementation used by tests. SFTP itself is tested against this machine's own OpenSSH `sftp-server`, run
  as a child over pipes (`SftpRemote::local_server()`, `src/remote/sftp.rs`; `local_server_args` passes it
  flags such as `-P copy-data`, `local_server_with` puts a proxy between); a stream's transport against the
  in-order `Fake` with a simulated link (`src/remote/ranged.rs`); the byte server against a plain
  `TcpStream` client. `SOPEN_TEST_HIDE_SFTP_SERVER=1` makes the server look absent — the receipt that every
  test needing it fails under `SOPEN_REQUIRE_SFTP_SERVER`. Tests that need a real host or volume are
  `#[ignore]`d and run by hand only: `tests/sftp_live.rs` (`SOPEN_TEST_SFTP_HOST=<ssh alias>`,
  `SOPEN_TEST_SFTP_DIR`, default `/tmp/sopen-receipts`), the hex window's live Save of a file above 256 MiB
  (`SOPEN_TEST_SFTP_HOST`, `SOPEN_TEST_SFTP_FILE`; `src/gui/hex/tests.rs`), the media window's live Save
  (the same two and `SOPEN_TEST_CLIPS=<START-END[,START-END]>`; `src/gui/media/live_tests.rs`), the directory
  tree's D-2 times on this machine's real home (`SOPEN_TEST_TREE_FOLDER=<a folder two levels below the home>`;
  `src/gui/browser/tests/s5.rs`, `d2_on_the_real_home`; it only reads), the read
  benchmark (`SOPEN_BENCH_FILE=<a large file>`, `src/remote/tests.rs`) and a real volume's copy and move
  (`SOPEN_TEST_VOLUME=<folder>`, `src/fileops/tests.rs`).
- Public repo hygiene: no machine names, IP addresses, home-directory paths or session scratch paths in
  commits, receipts or docs. Commit identity `Z <z@2lab.ai>`.
- **One look, one place.** Every window wears the PC Tools (PC Shell 5.5 / 6) skin from
  `src/gui/chrome.rs`: colours, the font, frames, the title and F-key rows, dialogs and the syntax
  theme. Windows never pick a colour or a font size themselves. The VGA font in `assets/fonts/` is
  CC BY-SA 4.0, not MIT: its licence file and the README credit stay with it.

## Where the code is

`src/lib.rs`'s doc comment maps the top-level modules; each module's own `//!` header is its contract
summary. Viewing a remote file as a stream and editing it (06, 07) live in:

| Module | What it is |
|---|---|
| `remote::ranged` (`src/remote/ranged.rs`, SFTP half `src/remote/sftp/ranged.rs`) | `Ranged`: one open handle on one connection, reads at explicit offsets sent in queue order; the pipelined whole-file read; the tests' in-order `Fake` |
| `stream` (`src/stream/`) | `Source`, one remote file open for viewing: the 64 KiB block cache (`cache.rs`), what is sent next (`prefetch.rs`), validation by `fstat` fences, change detection and the viewers' `Reader` (`source.rs`), the `Bytes` every viewer reads through, a local `File` too (`mod.rs`), the loopback byte server ffmpeg reads through (`http.rs`) |
| `gui::streamed` (`src/gui/streamed.rs`) | how a route turns the open's connection into a `Source`, the placeholder `Origin`, `bytes_fetched`, a preview's connection given back |
| `remote::patch` (`src/remote/patch.rs`, `sftp/patch.rs`, `local/patch.rs`) | a hex Save without moving the file: server copy, patch, rename; the streamed rewrite; Write in place; the undo file |
| `journal` (`src/journal.rs`) | the edit journal of the hex, image and media windows, the save intent and its reconciliation |
| `gui::save_as` (`src/gui/save_as.rs`) | the Save As modal and writer the three windows share |
| `gui::{hex,image,media}::file` | each window's write side: Save, Save copy, Save As's producer, recovery |
| `gui::image::probe` | an image's header walk, the memory its decode needs, the thumbnail, partial pictures |

## Receipts

Windows are verified with test hooks — environment variables, deliberately not in the README.
A scripted run ignores keyboard and mouse input (the owner may be typing elsewhere) and never
prompts on close.

| Hook | Effect |
|---|---|
| `SOPEN_STATE='rect=x,y,w,h;t=3.5;in=2;out=6;op=crop\|pixelize;block=16'` | Initial state in source pixels and seconds; `op` (images only, comma-separated, applied in order) uses `rect`. The hex window takes `at=OFFSET;patch=OFFSET:HEXBYTES[,OFFSET:HEXBYTES]` (offsets `0x…` or decimal): the cursor, and bytes overwritten in order, each an undo step. The file browser takes `open=NAME`: that entry of the folder is opened, as Enter would, once it is listed (with `SOPEN_SCREENSHOT`, the viewer is shot); and `at=OFFSET`: the first text file it opens — the `open=NAME` one, or else the first opened by hand — starts with the row that byte is in at the top (a place deep in a long line); later text files open at their top; `mode=dual` (the browser opens in dual mode; default `preview`) and `bar=NAME` (the active panel's bar goes on NAME once listed, without opening it — with `SOPEN_SCREENSHOT`, its preview is shot). The hex window, a streamed text file, a streamed image and a streamed sound take `loading=1`: ready to be shot at the first frame with content from the stream — the hex rows still loading as they are; the text at its first rows, pending rows and highlighting as they are (the browser hands it to the text viewers it opens, as it hands `at=`); the image at its first picture (the Exif thumbnail, else a picture of the bytes so far, else the size line where the format gives none); the sound at its first peaks, the rest of its waveform still being read — instead of once the screen's bytes are in, the image is whole or the waveform is. Every window takes `help=1` (F1 is pressed once the state is on screen: the key help is shot). The image, hex and media windows take `close=ask`: closing is asked for on the first pass, so an edit the state made brings up the unsaved-changes prompt (its receipt). The image window takes `restore=1`: once the rows of the journals an earlier session left are on screen, Restore on the first row is pressed, as a click would, where that row offers it (a crash recovery's receipt). The media window takes `clips=START-END[,START-END]` (its edit list: source seconds in output order) and `stereo=1` (Mono → Stereo, mono audio only). |
| `SOPEN_SCREENSHOT=<png>` | Open the window, render the state, write a screenshot of the window, exit. Prints `screenshot <png> <w>x<h> title="<host:/abs>" focused=<bool> …`. `SOPEN_WATCHDOG=<s>` (default 30) ends a stalled run. |
| `SOPEN_AUTOSAVE=1` | Images, the hex window and media (an edit from `clips=` / `stereo=1`): apply the state and Save without a window (backup, conflict-checked overwrite); exit 0 saved, 3 refused. `SOPEN_AUTOSAVE_DELAY_MS=<ms>` waits before the write so a second process can change the remote (the conflict receipt). |
| `SOPEN_CHOICE=hex\|save\|cancel` | Answers the binary-file dialog: `hex` opens the file at once in the window, which becomes the hex window, as Enter would — a remote file as a stream, with nothing fetched first (with `SOPEN_SCREENSHOT`, the hex window is shot after the switch; with `SOPEN_AUTOSAVE`, no window: the state is applied and saved — over the stream, as a hex Save does: the file is copied on its host and patched, never downloaded to a local copy; a file of at most 256 MiB is still read whole twice, for its content checks: the baseline pass over the file as opened and the read-back of the server's copy); `save` downloads to `SOPEN_SAVE_TO` without a window and prints `saved <path>` (a taken path is exit 1, naming the next free name); `cancel` downloads nothing. Without it, `SOPEN_SCREENSHOT` shoots the dialog and nothing is downloaded, and `SOPEN_AUTOSAVE` on a binary file is an error (nothing would answer the dialog). |
| `SOPEN_KEYS='F5,Enter'` | The file browser: keys played as the owner's own, one each time the window has settled (nothing listing, opening or being previewed, the display committed; a file operation waiting at its name-in-the-way question counts as settled), so `SOPEN_SCREENSHOT` shoots what they lead to — a dialog, the rename field, a report. Steps are separated by commas: a key by egui's name (`F2`, `Enter`, `Esc`, `Down`, `Space`, `Tab`, a letter), with `Shift+`, `Alt+` or `Cmd+` / `Ctrl+` before it, or `text:…` for typed text (`text:+,text:*.txt,Enter` selects by a pattern). `F10` opens the directory tree; the window has settled once its rows are on screen and the folders that open by themselves have opened (05 §5) — the listings further away and the deep sizes go on, so the shot shows sizes still growing (`F10,Down,Right` opens a row; `help=1` then shoots the tree's key help). An unknown key is an error before the window opens. A limit of the hook: a step is played only once the window has settled, so a key cannot be pressed in the middle of a running operation (an `Esc` after `F5,Enter` waits until the copy has ended or asks about a name) — stopping one mid-way is proven by the gated tests in `src/gui/browser/tests/s1.rs`, not by this hook. The image window of its own takes it too, played the same way once the window has settled (after `SOPEN_STATE`, whose `rect=` a played `C` crops): `SOPEN_STATE='rect=100,60,320,200' SOPEN_KEYS='C,F7,Cmd+A,text:<folder>/shot.edited.png,Enter'` crops and saves as a new local file; `C,Cmd+S` crops and saves over the file. Cmd/Ctrl+C, X and V arrive as clipboard events, never as keys, so they cannot be played. A local setting, not forwarded by `--target`. |
| `SOPEN_LIST=1` | A folder: print what the file browser lists (the path, a row per entry — Name, Ext, Size, Date, Time — and the `N Listed = N bytes` foot) instead of opening the window. A scripted save or export (`SOPEN_AUTOSAVE`, `SOPEN_AUTOEXPORT`) on a folder is an error. |
| `SOPEN_SAVE_TO=<path>` | Where the dialog's "Save to…" saves (the path field's first value; a folder gets the file's name), instead of the download folder — no native dialog. |
| `SOPEN_SAVE_AS=<folder>` | The hex and media windows: their edits are saved as `<folder>/<stem>.edited<ext>` (the next free `-N` when taken; media: the extension its render has) — without a window under `SOPEN_AUTOSAVE` (progress lines on stderr, `saved <path>` on stdout; exit 0 saved, 3 not), else through the Save As modal once the state is on screen (`.prd/07-streaming-edit.md` §7). |
| `SOPEN_SAVE_AS_HOLD_MS=<ms>` | With `SOPEN_SAVE_AS`: the save holds at its first progress at or past 30 % for that long (media: of the render), so `SOPEN_SCREENSHOT` shoots the modal mid-transfer. |
| `SOPEN_SAVE_HOLD_MS=<ms>` | `SOPEN_CHOICE=save`: the save holds at its first progress report for that long, or until it is cancelled — so a signal reaches it mid-way (Ctrl-C and SIGTERM cancel a save without a window: nothing is left, exit 130 / 143). `<ms>:stalled` holds for the whole time whatever comes, as a transfer whose remote has stalled: the first signal is not seen, and the same signal again ends sopen. |
| `SOPEN_AUTOEXPORT=1` | Media: apply the state and Export without a window; prints the new sibling's path. |
| `SOPEN_EXPORT_HOLD_MS=<ms>` | Media window: start an Export on open and hold it, at its first progress report at or past 30 %, for that long, so `SOPEN_SCREENSHOT` captures the window mid-export; when the window closes sopen waits for the export and prints its result. |
| `SOPEN_STREAM_STATS=1` | Every stream (a remote file a window reads, `.prd/06-streaming.md` §11) prints one line on stderr at each event — `stream <open\|first\|idle\|input\|ready\|close> t=<ms> <host:/abs> fetched= demand= prefetch= bulk= delivered= http_sent= sniff= requests= max_in_flight= runs= jumps= cuts=` (`input` adds ` place=0x…`; `t` counts from the open), every counter cumulative — and the byte bounds of 06 §12 are read from those lines. Only the value `1` turns it on. A local file makes no stream and prints nothing. |
| `SOPEN_STREAM_THROTTLE=<ms>,<bytes/s>` | Every read of every stream is delayed by `<ms>` and paced to `<bytes/s>`, order kept, as the tests' `Fake` does, so a screenshot catches rows still loading or a partial picture. Anything else (a rate of 0, no comma) is ignored. |
| `SOPEN_WINDOW_SIZE=<w>x<h>` | Open the window at that size in points (layout receipts on a narrow window). |
| `SOPEN_RENDERER=wgpu\|glow` | Force a renderer (default: wgpu, glow when wgpu does not start; `WGPU_BACKEND=dx12` on macOS forces that fallback). |
| `SOPEN_CODE=<path>` | Use this VS Code CLI for text and for the file browser's F4 (a folder itself opens in the browser); an empty value means VS Code is absent. A local setting, not forwarded by `--target`. |
| `SOPEN_ZED=<path>` | Use this Zed CLI instead of searching PATH and its usual install paths; an empty value means Zed is absent. A local setting, not forwarded by `--target`. |
| `SOPEN_CACHE_DIR=<dir>`, `SOPEN_STATE_DIR=<dir>` | The cache (fetched copies, `.edits` with the journals, `.backup`, `.exports`, …) and the state folder (ssh control sockets, `usage.jsonl`) instead of the platform's; an empty value is ignored. Tests point both at a temp dir (`tests/cli.rs`). Local settings, not forwarded by `--target`. |
| `VSCODE_EXTENSIONS=<dir>` | VS Code's own extensions-dir variable; when non-empty sopen looks there (instead of `~/.vscode/extensions`) for Remote - SSH before opening a remote path in VS Code. Tests point it at a temp dir. Not forwarded by `--target`. |
| `SOPEN_RAISE=0\|wait` | KDE Plasma (spec §3, msg 11): `0` leaves the editor's window where KWin put it (tests/cli.rs sets it, so a test run in a KDE session never moves a real window); `wait` brings it forward in sopen's own process, about 3 s, instead of a detached `sopen --raise-window`, so a test sees every D-Bus call before sopen exits. HOST's own setting: not forwarded by `--target`. |
| `SOPEN_EDITOR_WAIT=<seconds>` | How long sopen waits for the editor CLI (VS Code, Zed) to exit before it counts as handed off — default 3, at most 120 (anything else: 3); the wait ends as soon as the CLI exits. tests/cli.rs sets 60 where a stand-in CLI fails, so a loaded machine still sees the failure. Never unbounded. A local setting, not forwarded by `--target`. |
| `sopen --raise-window code\|zed --raise-caption=<name>` | Hidden, internal: the detached child. It loads one KWin script for the opened `<name>` and runs it once, through `busctl --user` (else `qdbus6`/`qdbus`) on PATH, then unloads it ~3 s later. It prints nothing and always exits 0. Tests run it with a stand-in `busctl` first on PATH. The script's behaviour is tested under node against a stand-in `workspace` with signals (`tests/kwin-script.mjs`). |
| `SOPEN_BROWSER=<command>` | `sopen URL`: run this command with the address as its one argument instead of the default browser's opener (`open` on macOS, `xdg-open` on Linux); an empty value is ignored. Waited for as an editor CLI is (`SOPEN_EDITOR_WAIT`). The tests point it at a stand-in that records its argument, so no test opens a real browser. Forwarded by `--target`: it acts where the address opens. |
| `SOPEN_LISTENERS=<file>` | The tunnel's HOST half (`--hold-until-eof`): the listening-socket table its port check reads — `/proc/net/tcp{,6}` rows or `netstat -anv -p tcp` lines — instead of this machine's own (`/proc` on Linux, `/usr/sbin/netstat` on macOS); an empty value is ignored, a file that cannot be read fails the check (refused). The tests point it at a table of their own, so what happens to listen on the test machine never matters. Forwarded by `--target`: the check runs where the address opens. |
| `sopen --here --hold-until-eof URL` | Hidden, internal: HOST's half of a tunnel (spec §4a). Checks that nothing listens on the address's port beyond loopback here (sshd's `GatewayPorts yes` binds a forward to every interface) and refuses otherwise (exit 1, the browser not opened); opens the address; prints `sopen-tunnel-open` (`url::TUNNEL_OPEN`: the fixed cue the other side waits for, nothing of the address); then reads its input to the end and exits 0 — the other side's sopen ending, however it ends, closes the session. Needs `--here`; only for an address. |
| `SOPEN_TARGET_BIN=<path>` | `--target` runs this sopen binary on HOST (a build not installed there yet) instead of the one on HOST's PATH or in its Homebrew / cargo install paths. A `--target` run carries this and the hooks above, and `SOPEN_LOG`, to HOST; settings naming local paths stay here. |
| `SOPEN_CONFIG=<file>` | The config file to use instead of `$XDG_CONFIG_HOME/sopen/config.toml` / `~/.config/sopen/config.toml` (tests point it at a temp file, so the owner's default target never forwards a test run). |
| `sopen --selftest-audio <file>` | Hidden: play the file on the real output device at gain 0 for ~2 s; prints `callbacks=N underruns=M device=<name> … decoder=ffmpeg\|wav` (a WAV is read in-process where there is no ffmpeg). |

`SOPEN_SCREENSHOT` on a folder shoots the file browser (its list and, in preview mode, the preview of the entry `bar=NAME` names; with `SOPEN_STATE=open=NAME` that entry's viewer). A screenshot is a receipt only after someone has looked at it. Live receipts write only under a
remote `/tmp/sopen-receipts/` directory created for the purpose.

## Distribution (llmux shape)

- `ci.yml` — every PR and main push: the gate on `macos-latest` and `ubuntu-latest`. `preview.yml` and
  `release.yml` call it first and publish nothing unless it is green.
- Every apt install in the workflows (runners and the runtime-library container) goes through
  `.github/scripts/apt-install.sh`: the Ubuntu archive and security sources point at a mirror list (apt's mirror
  method falls back to the next mirror), with retries and a request timeout; its test is
  `.github/scripts/tests/apt-install.test.sh` (no apt, no network), run by `ci.yml` on Linux.
- `preview.yml` — every main push: gate, then macOS arm64/x86_64 and Linux x86_64/aarch64 binaries (Linux built
  on `ubuntu-22.04` / `ubuntu-22.04-arm` for an older glibc floor), a `preview-<YYYY-MM-DD-HHMM>-<sha12>`
  prerelease (the binaries and `THIRD-PARTY-NOTICES`), and the tap's `Formula/sopen-preview.rb`.
- `release.yml` — `v*` tag equal to the Cargo version (tagging is the owner's call): gate, the same four
  binaries, a stable release, then the tap's `Formula/sopen.rb`.
- Tap bump = `.github/scripts/bump-tap.sh` (regression test `.github/scripts/tests/bump-tap.test.sh`): renders
  `Formula/<name>.rb` from this repo's `packaging/homebrew/<name>.rb.tmpl` with the sha256s of the published
  assets (the four binaries and `THIRD-PARTY-NOTICES`) and pushes only that file to `2lab-ai/homebrew-tap`.
  The tap holds no templates. The separation model is a `resource` with its own fixed url and sha256 in the
  templates, staged into the formula's `pkgshare`.
  - With the repo secret `TAP_DISPATCH_TOKEN`: the workflow pushes the formula; a failed bump fails the job.
  - Without it: the bump job only prints a notice and the tap is not touched. The dispatcher then pushes the
    rendered formula to the tap by hand: download the release assets into `release/`, and from a checkout of
    the released commit run `FORMULA=<sopen-preview|sopen> TAG=<tag> [BUILD_ID=<build id>] GH_TOKEN=<token
    that can push the tap> .github/scripts/bump-tap.sh`.
- Install: `brew install 2lab-ai/tap/sopen-preview`. The formula depends on `ffmpeg`, installs the separation
  model into `pkgshare` and `THIRD-PARTY-NOTICES` into the keg.
