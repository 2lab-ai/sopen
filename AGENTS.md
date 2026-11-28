# AGENTS.md — sopen

Single source of truth for anyone (human or agent) changing this repo. `CLAUDE.md` only imports this file.

## What sopen is

`sopen` is the command; xtools is the multi-function tool it opens — the file browser, the viewers, the image,
media and hex windows, the git panel, the directory tree and the dialogs (owner message 22, `.prd/01-spec.md`).

`sopen [user@]host:path` opens a remote file or folder as if it were local. A folder opens in the xtools file
browser (two panels, Total Commander-like; in preview mode, the default, the side opposite the active list previews
the file under the bar, and F9 switches to two lists; Enter shows a file in the viewer for its kind in the same
window — a read-only text viewer with syntax highlighting, or the image, media or hex window below; F2 renames, F5
copies, F6 moves and F7 makes a folder, never over anything; F8 deletes after asking — on this machine to the
Trash where there is one, on a remote permanently (SFTP has no Trash); F4 hands the entry to the editor; F10
shows the directory tree in place of the active list, the other side frozen until it closes; in a folder inside a git work tree, the bar on a
folder makes the other side the repository's git panel — the graph, details, diffs and files at a
revision opened read-only, find, filters, Cmd/Ctrl+G pins it, and every Git Graph action from its menu
(F10) and F-keys, each confirmed in a dialog that names its backup refs and command lines, run on a
worker under the repository's write lock, undone by Cmd/Ctrl+Z, and free history editing — a
commit's quick edits, the planner (Cmd/Ctrl+E), an edit stop's Amend and Split, each one run of
`git rebase -i` with sopen's own todo — running the host's own git, `.prd/03-git-panel.md`).
Text goes to VS Code when it
is installed (remote ones only with its Remote - SSH extension), otherwise Zed's remote mode; images, video and
audio open in a small xtools window with exactly these edits: rectangle crop, rectangle pixelize and clipboard
copy / cut / paste (images: cut leaves the background colour; paste lays a picture — stretched, fitted or 1:1 — or
text into the rectangle), rectangle crop, time cut and cut / copy / paste on the timeline (video), time cut,
cut / copy / paste and Mono → Stereo (audio), and — as a command, `sopen stems` — voice / music separation into
two new files (spec §7a). Any other file (a zip, any binary) opens a dialog before
anything is downloaded: the xtools hex window (the default; byte overwrite only) or "Save to…" a chosen path;
`--hex` opens any file in the hex window. It opens on the machine it was typed on, or — with `--target HOST`,
`SOPEN_TARGET` or `target` in sopen's config — on HOST's screen, by running HOST's installed sopen with `--here`
(and `--gui` when asked, again without it on an older HOST) over ssh: HOST
is a machine the owner uses sopen on, not a file's remote, which still needs only sshd (spec §4a). An `http://` or
`https://` address as SOURCE opens in an xtools window on the machine that shows things — `--browser`, no display or
no web engine: its default browser (`.prd/09-web-view.md` §7, `src/gui/web/address.rs`); on HOST a loopback one is
served from this machine through an ssh tunnel of its own that lives exactly as long as this sopen, or the window there
(spec §4a, `src/url.rs`, `forward::tunnel`). On macOS,
while an xtools window is in front, the top row is meant to send F1–F12 without fn (the key half is the owner's
receipt K-F3, still open): `fkeys` in sopen's config —
`"function"` (the default) or `"system"` (never touched), set by `sopen config fkeys function|system` — and
sopen sets the global `HIDFKeyMode` to 1 and puts back what it found once none of its windows is in front,
even when it dies (lock, state file, watchdog child; `.prd/04-file-keys.md` §6, `src/fnkeys/`); a run a hook
drives (`SOPEN_SCREENSHOT`, `SOPEN_CELLS`, `SOPEN_AUTOSAVE`, `SOPEN_AUTOEXPORT`, `SOPEN_LIST`, `SOPEN_KEYS`) and the tests never
touch it, and Linux has no such switch. A remote
file that one of the xtools windows shows is read as a stream, never downloaded first, whatever its size
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
`SOPEN_REQUIRE_NODE=1`, `SOPEN_REQUIRE_SFTP_SERVER=1` and, from S2, `SOPEN_REQUIRE_GIT=1` (git is on the
runners; the git tests skip without it locally), so there they fail instead. On macOS CI also sets
`SOPEN_REQUIRE_WEBKIT=1`: the page view's window tests run there, and skip locally without it. The
separation tests (`sopen stems`) need the model: they skip unless `SOPEN_STEMS_MODEL` names it; CI downloads it (cached, its
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
  copy, move and make folder never replace an existing name: a remote file is renamed by `hardlink@` and
  `REMOVE`, a remote folder (and a copy's publish where the server has no `hardlink@`, after a rename probe
  once per session) by SFTP's plain rename on a second `sftp` channel of the same master — never
  `posix-rename@openssh.com`. Delete always asks and says where things go: to this machine's Trash, or
  permanently — on a remote, with Shift, or where there is no Trash (`.prd/04-file-keys.md` §5).
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
  and Matroska's CodecDelay, ffmpeg 8 (Ubuntu 26.04's, the CI Linux runner's) applies neither — and in how they
  write them: 8 and 9.0 cut an MP4's last AAC sample short in `stts`, 9.1 keeps whole frames there (only the edit
  list trims the end) and adds Matroska's DiscardPadding. The render tests therefore hold an AAC render to a plain
  encode of the same sample count by the same ffmpeg (`pcm_as_plain` in `src/cut/edit_tests.rs`) and read what the
  file declares without a decoder (an MP4's edit list must say the timeline's samples to its timescale and `stts`
  hold them — exactly where its last sample is cut short, within one frame where all are whole; Matroska from 8
  and 9.0 declares only its priming, as CodecDelay, so there the end may run on by less than one AAC frame), and
  read frames with `-fps_mode passthrough`. ffmpeg 9.1's mp3 muxer writes 529 samples too little encoder delay
  into the LAME tag below 32 kHz (47 for LAME's 576): every `.mp3` sopen writes — Save, Export, `sopen cut`,
  `sopen stems` — is mended before it is published (`src/ffmpeg/mp3.rs`).
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
  (`SOPEN_TEST_VOLUME=<folder>`, `src/fileops/tests.rs`), the plain rename and its probe over a real ssh master
  (`SOPEN_TEST_SFTP_HOST`, `SOPEN_TEST_SFTP_DIR`; `src/remote/sftp/rename.rs`,
  `live_the_plain_rename_over_a_real_master`), the git panel's stop of a remote fetch, 03 G-R3's live
  receipt (`SOPEN_TEST_GIT_HOST=<ssh destination>`, `SOPEN_TEST_GIT_DIR=/tmp/<a new folder>`;
  `src/gui/browser/tests/s3.rs`, `live_g_r3_esc_stops_a_remote_fetch`; it makes and removes that
  folder), the editor buffer's two measurements, each alone in a release build (`cargo test --release
  --lib -- --ignored --exact <path>`, `--exact` taking the full path; `src/edit/buffer/tests.rs`:
  `edit::buffer::tests::eb1_100_mib_within_1_ms_and_1_1x`, 12 EB-1's 100 MB timing and memory, and
  `edit::buffer::tests::eb5_a_200_mb_delete_adds_only_its_pieces`, EB-5's 200 MB delete; each
  prints `1 passed`), the editor's kept highlighting states over 16 MiB of Rust (12 EH-3, its own
  binary for its counting allocator: `cargo test --release --test kept_states_heap -- --ignored
  --nocapture`; recorded 18.73 MiB against its 32 MiB bound — every run asserts the same rate over
  2 MiB) and the system Trash (`src/remote/local/trash.rs`,
  `the_system_trash_takes_a_file_and_a_link`; it puts two entries in the Trash and prints where). A test build
  never reaches the owner's Trash: `LocalFs`'s Trash is a stand-in of the test process's own.
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

The git panel (03) lives in:

| Module | What it is |
|---|---|
| `git::runner` (`src/git/runner.rs`) | `GitRunner`: this machine's git or a remote's over the ssh master, 03 §3's environment, limits, stop (a host's stops in one stop session at a time), the command log |
| `git::{detect,parse,history,graph,model}` | detection and its cache, the parsers, every read's words (`history::argv`), the log stream (`LogStream`, `RunnerGit` the runner behind it), the lane layout, the shared types |
| `git::{actions,backup}` | every action of 03 §7 as a pure plan (`plan`, its reads beyond the refresh `needs` / `inspect`), the executor (`execute`), the in-progress probe and the banner; backup groups, manifests, restore and undo (03 §8) |
| `git::rebase` | the history edits of 03 §7.8 as `actions` plans: the quick edits, the planner's model, ops and checks, its todo and run, an edit stop's Split and its guarded Continue (`plan`, `needs`, `open`, `open_needs`) |
| `gui::browser::git_panel` | the panel's view: graph, details pane, find, filters, its keys (`GitAction`s out); S3's screens — the menu, the action dialogs and the result (`dialogs`), the Backups list, the command log, the banner (`in_progress`) — and what the F-keys act on; S4's — the planner pane and the quick edits' dialogs (`planner`), the commit menu's history row (`menu::HistoryMenu`, offered as the loaded graph says: `GitView::eligibility`), an edit stop's Amend and Split |
| `gui::browser::git_host` (`src/gui/browser/git_host.rs`) | the integration: the window's git host, detection of the active folder, a panel per repository (3 kept), loads (the in-progress probe among them), pages, refresh, details, diffs and files at a revision (`.revisions`), `SOPEN_LIST=git` (`git_listing`); S3's actions — a dialog's prefill and reads, the run on a worker under the repository's write lock, the refresh after it, `GitEvent::WorkTreeChanged`, the window's undo, the Backups list, the log, a running write waited for when the window closes; S4's history edits — the range a quick edit or the planner opens on, their runs on the same worker and lock, the split kept per window |

## Receipts

Windows are verified with test hooks — environment variables, deliberately not in the README.
A scripted run ignores keyboard and mouse input (the owner may be typing elsewhere) and never
prompts on close.

| Hook | Effect |
|---|---|
| `SOPEN_STATE='rect=x,y,w,h;t=3.5;in=2;out=6;op=crop\|pixelize;block=16'` | Initial state in source pixels and seconds; `op` (images only, comma-separated, applied in order) uses `rect`. The hex window takes `at=OFFSET;patch=OFFSET:HEXBYTES[,OFFSET:HEXBYTES]` (offsets `0x…` or decimal): the cursor, and bytes overwritten in order, each an undo step. The file browser takes `open=NAME`: that entry of the folder is opened, as Enter would, once it is listed (with `SOPEN_SCREENSHOT`, the viewer is shot); and `at=OFFSET`: the first text file it opens — the `open=NAME` one, or else the first opened by hand — starts with the row that byte is in at the top (a place deep in a long line); later text files open at their top; `enc=NAME` (an encoding of 08 §7.1 by its label, any case): the first text file opened, as `at=`, reads its lines in it; `view=1|2|3`: the first viewer opened switches to that view once it is on screen, as the plain digit does (08 §8: 2 is Hex for every file); `mode=dual` (the browser opens in dual mode; default `preview`) and `bar=NAME` (the active panel's bar goes on NAME once listed, without opening it — with `SOPEN_SCREENSHOT`, its preview is shot); `git=pin` (once the folder's detection answers, its repository's git panel is pinned to the other side and has the keyboard), with `select=REV` (the bar on that revision, any git accepts, once its page is loaded — more pages are read until it is), `label=N` (the label cursor on the row's Nth label) and `details=1` (its details pane opens): `SOPEN_STATE='git=pin;select=v1.0;details=1'` shoots a commit's details; `dialog=ID` (an action id of 03 §7: `reset`, `tag`, `commit`, …, and §7.8's history edits `reword`, `drop`, `move-up`, `split`, `plan`, …; any other word is refused before the window opens) opens that action's dialog, once the rest is done, on the object under the bar — else on the repository, the uncommitted row or its files, whichever offers it; a history edit on the bar's commit, else HEAD's, where its menu offers it (`plan` opens the planner) — and the shot waits for the dialog's reads, so `SOPEN_STATE='git=pin;select=v1.0;dialog=reset'` shoots a confirmation with its backups line, and `SOPEN_STATE='git=pin;select=HEAD~5;dialog=plan' SOPEN_KEYS='Down,text:s'` the planner with a squash row. The hex window, a streamed text file, a streamed image and a streamed sound take `loading=1`: ready to be shot at the first frame with content from the stream — the hex rows still loading as they are; the text at its first rows, pending rows and highlighting as they are (the browser hands it to the text viewers it opens, as it hands `at=`); the image at its first picture (the Exif thumbnail, else a picture of the bytes so far, else the size line where the format gives none); the sound at its first peaks, the rest of its waveform still being read — instead of once the screen's bytes are in, the image is whole or the waveform is. Every window takes `help=1` (F1 is pressed once the state is on screen: the key help is shot). The image, hex and media windows take `close=ask`: closing is asked for on the first pass, so an edit the state made brings up the unsaved-changes prompt (its receipt). The image window takes `restore=1`: once the rows of the journals an earlier session left are on screen, Restore on the first row is pressed, as a click would, where that row offers it (a crash recovery's receipt). The media window takes `clips=START-END[,START-END]` (its edit list: source seconds in output order) and `stereo=1` (Mono → Stereo, mono audio only). |
| `sopen --tui SOURCE` | The file browser in this terminal (`.prd/10-tui.md`, X1b; `src/gui/term.rs`): a folder, or a file's folder with `open=` its name (`SOPEN_STATE`'s, unless it names another); input and output not a terminal is exit 2. `SOPEN_STATE` and `SOPEN_KEYS` act as in the window; `SOPEN_SCREENSHOT` shoots the window a switch opens (F12, Alt+G or the title row's `[GUI]`), which then closes and the terminal comes back — a scripted run, so the macOS F-key switch is not touched; `SOPEN_AUTOSAVE`, `SOPEN_AUTOEXPORT` and `SOPEN_LIST` are not read. F1's key help, the text viewer (its bar, rows, Go to field and scroll bar; a text preview's rows), the hex view (`2` in a viewer: rows, Go to field, buttons, status row) and the preview side (its frame titled with the file, its `reading … %`, `…` and failure lines; a text or hex preview inside it, a page file's as its source) are cells in the terminal as in the window — an image's, a video's or a sound's preview side and the info card's are one hand-off box, from the side's first pass where its name tells (a picture, a video or a sound by extension); where it does not — a missing or unreadable file's card, a link to another kind, a `.ts` video — the side changes once, when its preview is ready; the directory tree (F10: its frame, rows, info line, bar and scroll bar, X4-tree) and the git panel (its frame, heads, rows with their lanes and labels, bar, scroll bar, rule and paging line, X4-graph; its details pane, and its banner with the steps' buttons, X4-pane; the planner's pane — its rows, bar, scroll bar, note, buttons and keys' hint, X4-planner; Find's field with its `Aa` toggle and the history search's dialog, X4-find; its command log, the log's entry shown whole and its Backups list, X4-lists; its menu, F10, Shift+F10 or a right click, and its filters, Cmd/Ctrl+K, X4-menu; every action's dialog — its fields, choices, plan and buttons — a run's result (and a read's failure, a tag's details) and "Stop git?", X4-dialogs and X4-dialogs2) are cells too; a pane egui still draws (the other viewers) is a hand-off box, and a dialog egui draws (a quick edit's and the planner's own) is one with its own line, Esc closes it. Ends 0 on Ctrl+Q, 128 + n on SIGTERM, SIGHUP or SIGINT and 129 when the terminal itself goes away (its input hangs up, a signal or not), the terminal restored either way; what is written to the standard error meanwhile is kept and printed once the terminal is let go. A copy above 256 KiB is not sent through the terminal (OSC 52): the status row says so. Its receipt is a private tmux server (`tmux -L <name> -f /dev/null new -d -x 100 -y 30 'SOPEN_CONFIG=<an empty file> sopen --tui --here <folder>'`, then `send-keys` and `capture-pane -p -e`; `display -p '#{alternate_on} #{mouse_any_flag} #{cursor_flag}'` after the exit says the terminal is restored). `COLORTERM=truecolor` or `24bit` sends RGB, anything else the 256-colour table (10 §5). F12 opens a real window on this machine: never on a Linux receipt host. With a target (`--target`, `SOPEN_TARGET`, the config's) F12 is the remote switch instead (10 §7.3, `src/gui/term/remote.rs`): HOST's half `--here --gui --state-stdin -- FOLDER` over ssh, the terminal kept raw and dimmed until its cue, Ctrl+C ending HOST's input; `SOPEN_SCREENSHOT` and the other hooks that script a run stay here then (`forward::SWITCH_LEFT_HERE`). Its tests run against a stand-in ssh that runs the forward's script with a stand-in HOST sopen (`SOPEN_TARGET_BIN`), never a real window. |
| `sopen --gui SOURCE` | The window on the machine SOURCE opens on (`.prd/10-tui.md` §11 row 2, T35; X2-gui): here, today's routes; with no display here (`gui::display_available`: Linux without `DISPLAY` / `WAYLAND_DISPLAY`) `no display here — sopen config target <host>, or --target, opens the window on another machine`, exit 1, before anything is read. Never beside `--app` or `--tui` (exit 2). A `--target` run passes it to HOST's sopen (`--here --gui [--hex] -- SOURCE`) when given, and until XF only then (`AUTO_GUI` in `src/main.rs`, off: an automatic one would turn HOST's no-display fallbacks into exit 1, 10 §11 row 6); XF turns on `auto_gui` — for a window run, `--hex` or a kind this side sees opens in a window (a local SOURCE by its stat and first bytes, a remote one by its name alone, `seen_kind`), never a run the hooks keep without a window. `--app` and an address never get it. HOST's exit 2 with clap's `error: unexpected argument '--gui' found` as the first line it wrote on stderr (colours, blank lines aside; plain under `ssh -T`, the terminal's under `-t`, where stdout and stderr are one stream) and nothing on its stdout — clap's refusal before anything ran — runs the same forward once more without it and says `sopen: sopen on HOST is older and does not know --gui; running it there again without it`; any other exit 2 is HOST's answer as it is (`forward::run`, `watched`). HOST's output passes on byte by byte as it is read, only watched (`forward::Scan`). The tests play an older HOST with the fake ssh's stand-in sopen (`FAKE_SOPEN_UNKNOWN=--gui`, `tests/cli.rs`). |
| `SOPEN_SCREENSHOT=<png>` | Open the window, render the state, write a screenshot of the window, exit. Prints `screenshot <png> <w>x<h> title="<host:/abs>" focused=<bool> …`. `SOPEN_WATCHDOG=<s>` (default 30) ends a stalled run. |
| `SOPEN_CELLS=<path>` | The screen as text, from the window and from `sopen --tui` alike (`.prd/10-tui.md` §15; `src/gui/term.rs`): once it has settled as `SOPEN_SCREENSHOT` waits for (`SOPEN_KEYS` played, nothing on its way), its grid is written to `<path>`, `cells <path> <cols>x<rows>` printed (the terminal's once it is restored) and the run ends, exit 0 — a scripted run in both (the terminal's or the window's input ignored, but Ctrl+C in the terminal, which ends it as SIGINT, exit 130; the macOS F-key switch untouched); `SOPEN_STATE`'s `help=1` presses F1 in both. One format for both, so the receipts of one screen are the same bytes and TS-1 is a `diff` of two files: `sopen cells <cols>x<rows> caret <x>,<y>` (or `caret none`), a line per row (a wide glyph once, every blank kept), `attributes <n>`, a line per row of `n`-character keys, one per cell, then `<key> <fg rrggbb> <bg rrggbb>` per key. The window's grid is the whole cells it holds (`SOPEN_WINDOW_SIZE=<cols×8>x<rows×16>` matches a `<cols>`×`<rows>` terminal), its cells laid as the terminal lays them: a pane egui draws is the terminal's hand-off box there, titled as in the terminal. The title row from the mode button's place to its end — the button (`[GUI]` in the terminal, none in a window started directly) and the clock after it — is blank in both, so two runs agree in any minute; take them on a grid wide enough that neither title row cuts its path. With the directory tree open (`SOPEN_KEYS=F10`) the screen settles only once the tree has no request on its way — no listing (of the screen's rows and of a screen above and below, whose sums an open folder shows), no filesystem figure, no scan — so the receipt holds final sizes and no moving mark; a request reported not answering is left out — locally it never answers, remotely it may not answer before `SOPEN_WATCHDOG` — and its row is taken saying `not answering`; `SOPEN_WATCHDOG` ends a run that still does not settle (exit 1). Phase 3 sizes every closed row on screen with no budget — so a tree receipt is rooted at a small fixture: `HOME=<fixture>` with the folder inside it (05 §2), `SOPEN_STATE_DIR` and `SOPEN_CACHE_DIR` outside it (a run writes `usage.jsonl` under the home's `.local/state` otherwise, a hidden folder more for the next run), and `SOPEN_STATE='bar=<a file>'` so the other side is its preview rather than a list whose free bytes move. In an ambiguous-wide terminal — at start the TUI writes `─` and reads the cursor report, with the kitty query, in one round trip (TS-6; no answer within two seconds is a narrow terminal without the kitty protocol) — §10's stand-ins are printed for sopen's chrome only (`+ - = \|` frames, `~` for the cut); content of ambiguous width (a name's `é`, `♥`, `a─b`) is sent as written, in two cells, its run moving into the blanks after its words, so no row is broken: the receipt is the grid before any of it. `SOPEN_WATCHDOG` ends a screen that does not settle, or a window that does not close after its receipt (exit 1). A local path, not forwarded by `--target`. |
| `SOPEN_AUTOSAVE=1` | Images, the hex window and media (an edit from `clips=` / `stereo=1`): apply the state and Save without a window (backup, conflict-checked overwrite); exit 0 saved, 3 refused. `SOPEN_AUTOSAVE_DELAY_MS=<ms>` waits before the write so a second process can change the remote (the conflict receipt). |
| `SOPEN_CHOICE=hex\|save\|cancel` | Answers the binary-file dialog: `hex` opens the file at once in the window, which becomes the hex window, as Enter would — a remote file as a stream, with nothing fetched first (with `SOPEN_SCREENSHOT`, the hex window is shot after the switch; with `SOPEN_AUTOSAVE`, no window: the state is applied and saved — over the stream, as a hex Save does: the file is copied on its host and patched, never downloaded to a local copy; a file of at most 256 MiB is still read whole twice, for its content checks: the baseline pass over the file as opened and the read-back of the server's copy); `save` downloads to `SOPEN_SAVE_TO` without a window and prints `saved <path>` (a taken path is exit 1, naming the next free name); `cancel` downloads nothing. Without it, `SOPEN_SCREENSHOT` shoots the dialog and nothing is downloaded, and `SOPEN_AUTOSAVE` on a binary file is an error (nothing would answer the dialog). |
| `SOPEN_KEYS='F5,Enter'` | The file browser: keys played as the owner's own, one each time the window has settled (nothing listing, opening or being previewed, the display committed; a file operation waiting at its name-in-the-way question counts as settled, a rename running from its field does not, nor a delete dialog still checking the Trash), so `SOPEN_SCREENSHOT` shoots what they lead to — a dialog, the rename field, a report. Steps are separated by commas: a key by egui's name (`F2`, `Enter`, `Esc`, `Down`, `Space`, `Tab`, a letter), with `Shift+`, `Alt+` or `Cmd+` / `Ctrl+` before it, or `text:…` for typed text (`text:+,text:*.txt,Enter` selects by a pattern), or `typed:1` for a key that types one character as the keyboard sends it — the key and its text in one pass (08 §12): in a viewer `typed:2` chooses the hex view and types nothing, where `text:2` alone would be typed. `F10` opens the directory tree; the window has settled once its rows are on screen and the folders that open by themselves have opened (05 §5) — the listings further away and the deep sizes go on, so the shot shows sizes still growing (`F10,Down,Right` opens a row; `help=1` then shoots the tree's key help) — a `SOPEN_CELLS` run waits for them (its row). An unknown key is an error before the window opens. A limit of the hook: a step is played only once the window has settled, so a key cannot be pressed in the middle of a running operation (an `Esc` after `F5,Enter` waits until the copy has ended or asks about a name) — stopping one mid-way is proven by the gated tests in `src/gui/browser/tests/s1.rs`, not by this hook. The image window of its own takes it too, played the same way once the window has settled (after `SOPEN_STATE`, whose `rect=` a played `C` crops): `SOPEN_STATE='rect=100,60,320,200' SOPEN_KEYS='C,F7,Cmd+A,text:<folder>/shot.edited.png,Enter'` crops and saves as a new local file; `C,Cmd+S` crops and saves over the file. Cmd/Ctrl+C, X and V arrive as clipboard events, never as keys, so they cannot be played. A local setting, not forwarded by `--target`. |
| `SOPEN_LIST=1` | A folder: print what the file browser lists (the path, a row per entry — Name, Ext, Size, Date, Time — and the `N Listed = N bytes` foot) instead of opening the window. A scripted save or export (`SOPEN_AUTOSAVE`, `SOPEN_AUTOEXPORT`) on a folder is an error. |
| `SOPEN_LIST=git` | A folder in a work tree (local or remote): print its git panel's first page instead of opening the window — the title line (the top level, with ` (worktree of …)`, ` (submodule of …)`, ` (shallow)`), one line per row (lanes in ASCII: `*` a commit, `@` the checked-out one, `=` a stash, `o` the uncommitted row, `\|` `/` `\` lines; the labels in brackets; subject, author, date, hash), then the paging line. Any other folder: the `SOPEN_LIST=1` listing and one `git panel: …` line saying why (bare, not a work tree, not a repository, no git). |
| `SOPEN_GIT=<path>` | This machine's git for the git panel instead of `git` on PATH (03 §2.4); a path that names nothing is "no git". A local setting, not forwarded by `--target`. |
| `SOPEN_REQUIRE_GIT=1` | Tests that need git fail instead of skipping when it is missing (CI sets it). |
| `SOPEN_REQUIRE_TMUX=1` | The terminal backend's tmux test (`gui::term`, `.prd/10-tui.md` §15's second emulator: the backend's bytes in a private tmux server, `capture-pane -e` against the window's cells) fails instead of skipping when tmux is missing (CI does not set it yet). |
| `SOPEN_REQUIRE_WEBKIT=1` | The page view's engine tests run (`.prd/09-web-view.md` §11; macOS: `src/gui/web/mac/tests.rs`): each opens a WebKit window — borderless, off every screen, in a child process that never becomes the active app — and a failure fails; without it they skip with a note, since WebKit is always on a Mac and the window is what the variable asks for. macOS CI sets it. Linux (`src/gui/web/gtk/tests.rs`): WebKitGTK offscreen on its GTK thread, never a mapped window, but GTK connects to the display it is given — so only under a private compositor (`env -i`, a private runtime folder and bus, `kwin_wayland --virtual`, or `xvfb-run`), never the session's; there a missing WebKitGTK, display or sandbox fails. Without it a test build starts no GTK at all. Linux CI does not set it. |
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
| `SOPEN_WEB=off` | The page view (`.prd/09-web-view.md` §4): the process has no web engine, on macOS and Linux alike, and its reason is `turned off (SOPEN_WEB=off)` — a page file shows its source after `no web engine here (turned off (SOPEN_WEB=off)): showing the source`, an address goes to the system browser — as on a Linux without WebKitGTK; for the fallback tests and receipts. Read once, at the process's first page view; only the value `off` acts, anything else is ignored. An address goes to the system browser after `sopen: no web engine here (turned off (SOPEN_WEB=off)); opening <shown> in the system browser`, no window opened (09 §4, §7). Forwarded by `--target`: it acts where the page or the address shows. |
| `SOPEN_CODE=<path>` | Use this VS Code CLI for text and for the file browser's F4 (a folder itself opens in the browser); an empty value means VS Code is absent. A local setting, not forwarded by `--target`. |
| `SOPEN_ZED=<path>` | Use this Zed CLI instead of searching PATH and its usual install paths; an empty value means Zed is absent. A local setting, not forwarded by `--target`. |
| `SOPEN_CACHE_DIR=<dir>`, `SOPEN_STATE_DIR=<dir>` | The cache (fetched copies, `.edits` with the journals, `.backup`, `.exports`, …) and the state folder (ssh control sockets, `usage.jsonl`) instead of the platform's; an empty value is ignored. Tests point both at a temp dir (`tests/cli.rs`). Local settings, not forwarded by `--target`. |
| `VSCODE_EXTENSIONS=<dir>` | VS Code's own extensions-dir variable; when non-empty sopen looks there (instead of `~/.vscode/extensions`) for Remote - SSH before opening a remote path in VS Code. Tests point it at a temp dir. Not forwarded by `--target`. |
| `SOPEN_RAISE=0\|wait` | KDE Plasma (spec §3, msg 11): `0` leaves the editor's window where KWin put it (tests/cli.rs sets it, so a test run in a KDE session never moves a real window); `wait` brings it forward in sopen's own process, about 3 s, instead of a detached `sopen --raise-window`, so a test sees every D-Bus call before sopen exits. HOST's own setting: not forwarded by `--target`. |
| `SOPEN_EDITOR_WAIT=<seconds>` | How long sopen waits for the editor CLI (VS Code, Zed) to exit before it counts as handed off — default 3, at most 120 (anything else: 3); the wait ends as soon as the CLI exits. tests/cli.rs sets 60 where a stand-in CLI fails, so a loaded machine still sees the failure. Never unbounded. A local setting, not forwarded by `--target`. |
| `sopen --raise-window code\|zed --raise-caption=<name>` | Hidden, internal: the detached child. It loads one KWin script for the opened `<name>` and runs it once, through `busctl --user` (else `qdbus6`/`qdbus`) on PATH, then unloads it ~3 s later. It prints nothing and always exits 0. Tests run it with a stand-in `busctl` first on PATH. The script's behaviour is tested under node against a stand-in `workspace` with signals (`tests/kwin-script.mjs`). |
| `SOPEN_BROWSER=<command>` | The system browser's opener for an address — `sopen --browser URL`; `sopen URL` where there is no display or no web engine (`.prd/09-web-view.md` §4); a link clicked out of the address window's origin (09 §7): run this command with the address as its one argument instead of the default browser's opener (`open` on macOS, `xdg-open` on Linux); an empty value is ignored. Waited for as an editor CLI is (`SOPEN_EDITOR_WAIT`; from the window, on a thread of its own). `sopen URL` itself opens the address window (`src/gui/web/address.rs`): `SOPEN_SCREENSHOT` shoots it once the page's main frame finished or failed, the page composed in (`screenshot … title="xtools — <shown>"`). The tests point it at a stand-in that records its argument, so no test opens a real browser; every `sopen URL` test passes `--browser`, or sets `SOPEN_WEB=off` (with a `DISPLAY` name, so Linux takes §4's fallback and not the no-display route), so no test opens a window. Forwarded by `--target`: it acts where the address opens. |
| `SOPEN_LISTENERS=<file>` | The tunnel's HOST half (`--hold-until-eof`): the listening-socket table its port check reads — `/proc/net/tcp{,6}` rows or `netstat -anv -p tcp` lines — instead of this machine's own (`/proc` on Linux, `/usr/sbin/netstat` on macOS, both listing every user's sockets); an empty value is ignored, a file that cannot be read or shows no listener on the port fails the check (refused). The tests point it at a table of their own, so what happens to listen on the test machine never matters; the fake ssh adds the forward's loopback row, as sshd opens the listener. Forwarded by `--target`: the check runs where the address opens. |
| `sopen --browser URL` | `sopen URL` in the system browser, as before 09, on every route (`.prd/09-web-view.md` §7): here, through a tunnel and passed to HOST as it is — without it an address opens in the xtools address window (`src/gui/web/address.rs`). With a file SOURCE it is a usage error (exit 2), as `--hold-until-eof`. Passed on to HOST's sopen on both forwarded routes (`--here --hold-until-eof --browser -- URL`, `--here --browser -- URL`); an older HOST sopen does not know it and exits 2 with its own line. Every `sopen URL` test in `tests/cli.rs` passes it or sets `SOPEN_WEB=off` (with a `DISPLAY` name), with the `SOPEN_BROWSER` stand-in. |
| `sopen --here --hold-until-eof URL` | Hidden, internal: HOST's half of a tunnel (spec §4a). Checks for positive evidence that the address's port is private here — at least one listener on it, every one on `127.0.0.1`, `::1` or `::ffff:127.0.0.1` (sshd's `GatewayPorts yes` binds a forward to every interface) — and refuses otherwise, "no listener seen" included (exit 1, the browser not opened); opens the address in its window — `sopen-tunnel-window` (`url::TUNNEL_WINDOW`) once the view is made, then `sopen-tunnel-open` (`url::TUNNEL_OPEN`: the fixed cue the other side waits for, nothing of the address) once its load is asked for — reads its input to its end on a thread, and closes the window and exits 0 there (no further cue) — the other side's sopen ending, however it ends, closes the session; closing the window here prints `sopen-tunnel-closed` (`url::TUNNEL_CLOSED`) and exits 0. With `--browser`, no display or no web engine it opens the system browser, prints `sopen-tunnel-open` alone and reads its input to the end. Needs `--here`; only for an address. Cues are written ignoring write errors (`url::write_cue`, never `println!`): a closed output does not stop it; the other side writes its own lines the same way. The other side also reads the window cues of 09 §7, matched exactly — only the line's end (`\n`, `\r\n`) taken off, so a trailing space or tab makes a plain line (any other line is HOST's plain output, passed on): after `sopen-tunnel-window` (`url::TUNNEL_WINDOW`) its open line says that closing the window there, or Ctrl-C here, closes the tunnel; on `sopen-tunnel-closed` (`url::TUNNEL_CLOSED`), before ssh's exit or with it, it closes the tunnel at once, says `<address>: its window on HOST was closed; the tunnel is closed` and exits 0. The tests play such a HOST with the fake ssh's stand-in sopen (no test opens a window); the real HOST half runs over the fake ssh on the fallback route (`SOPEN_WEB=off`). |
| `sopen --here --gui --state-stdin -- FOLDER` | Hidden, internal: HOST's half of the switch from `sopen --tui` (`.prd/10-tui.md` §7.3, X2-host; `src/gui/switch_host.rs`). The first line of its input is the switch's state (10 §7.4, one JSON line); FOLDER (any folder SOURCE names) opens at it in a switched window, `[TUI]` on its title row. F12, `[TUI]`, the close button and Cmd/Ctrl+Q and +W return, unsaved edits asking the window's own close prompt first (Cancel keeps it); once what still writes has ended it prints `sopen-xtools-state <the state's JSON>` (`switch_host::STATE_CUE`) and exits 0 (3 after a refused save). A newer `v` (`the sender sent switch state v<n>; this sopen reads up to v<m> — update one of them`), no line, one that does not read, another host's state, a file or an address: one line, exit 2, before any window; no display: §7.1's line, exit 1. The rest of the input is read to its end, heard drawn or hidden: with nothing to lose the window closes, no cue; else `sopen-xtools-kept` (`KEPT_CUE`) and the window stays, no `[TUI]`, until it is closed there; the session is let go either way. Its stdout and stderr reach the session through relays that drain once it is gone, so no write fails on a dead session. Receipt: `(printf '%s\n' "$STATE"; sleep 40) \| SOPEN_CONFIG=<an empty file> SOPEN_KEYS='Down,Down,F12' sopen --here --gui --state-stdin -- <folder>` — the last stdout line is the cue with the keys' change, exit 0 (a played F12 is the switch); with `SOPEN_CELLS` instead of the keys, the grid at the state, then the cue; without the `sleep` the window closes at once, no output, exit 0. |
| `sopen channel [stable\|preview]`, `sopen update` | The release channel and the update, through brew (`.prd/01-spec.md` §4b, `src/channel.rs`). `sopen channel` prints `channel: <stable\|preview> (via brew)` — `(via build marker (brew unavailable))` or `(via build marker (not installed by brew))` otherwise; a `dev` build has no channel (exit 1). A switch ends `switched to the <c> channel: <old formula> <old version> → <new formula> <new version>` and the windows line; the stable channel before its first release is `the stable channel has no release yet (…); staying on preview`, exit 1, nothing installed. `sopen update` ends `<formula> <before> → <after>` and the windows line, or `<formula> is up to date (<version>)`. A failed step's error ends with what is installed, whose `bin/sopen` is and, unless it already is so, `to make bin/sopen the <c> one: brew unlink <formula> && brew link --overwrite <formula>`; a `bin/sopen` that cannot be read is never unlinked (the switch stops after its install step). A build brew did not install refuses both, `this sopen was not installed by brew (…); install it with: brew install 2lab-ai/tap/sopen-preview`, exit 1, and runs no brew — the receipt of a branch build (`target/debug/sopen update`). The brew paths are tested only against the scripted `FakeHost` (`src/channel/tests.rs`: a model of brew's kegs, link records and `bin/sopen`, with a command failing before or after its effect, or exiting 0 having done nothing) and, in `tests/cli.rs`, a recording stand-in `brew` first on PATH: never a real install, uninstall or upgrade from a test. |
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
  model into `pkgshare` and `THIRD-PARTY-NOTICES` into the keg. `sopen channel stable|preview` switches between
  the two formulas and `sopen update` upgrades the one installed (spec §4b); a switch to stable is refused until
  the tap has `Formula/sopen.rb`.
