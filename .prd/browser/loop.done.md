# browser — done (append-only)

Loop: [`loop.md`](loop.md). Each round close appends one block at the bottom; nothing above it ever
changes. Rows here are snapshots taken when they were moved.

## R1 (part) — closed 2026-11-21

- measured G-WRAP ✅ G-WAVE ✅ S1-0 ✅ S1-A ✅ S1-B ✅ S1-C ✅ S1-D ✅ · result: both gates and five of
  S1's work units merged; S1-E (PR #29) is in review and S1-M waits on it · closed rows: G-WRAP,
  G-WAVE, S1-0, S1-A, S1-B, S1-C, S1-D · PRs: #14, #15, #18, #20, #21, #22, #23

### Closed rows

| ID | Status | Branch | Scope | Owner files (02 §5) | After |
|---|---|---|---|---|---|
| G-WRAP | ✅ merged: PR #14 (dd7dce4), 2026-11-20 | — | the text-wrap branch (PR #14) merged: S1-B's text preview (and P1) builds on its wrap mode; message 13 arrives with it | — | — |
| G-WAVE | ✅ merged: PR #15 (f1359ac), 2026-11-20 | — | PR #15 merged: it rewrites `media.rs`; S1-B touches `media.rs` only after it, and only for the `set_preview` default; S1-M follows it | — | — |
| S1-0 | ✅ merged: PR #18 (d970a3f), 2026-11-20 | `feat/browser-s1-scaffold` | `mod` lines and stubs for S1's files; `Window::set_preview` on the trait (default: nothing); `Start` carries the ssh destination and the state dir; touches no viewer file | `src/lib.rs`, `src/main.rs`, `src/gui/mod.rs` (mod lines, the trait method), `src/gui/browser.rs` (`mod keys; ops; view; preview;`, `Start`), `src/remote/{mod,sftp,local}.rs` (mod lines), stubs | E3 |
| S1-A | ✅ merged: PR #21 (221f58e), 2026-11-20 | `feat/browser-s1-keys` | the key table with every context's rows (04 §3, 03 §9, 05 §1), exact modifiers that consume their events, typed-text `+ - *`; Cmd/Ctrl+digit → F-key in the raw-input hook; clickable F-key row; F1 help; `mode=` and `bar=` state keys | `src/gui/browser/keys.rs`, `src/gui/mod.rs`, `src/gui/chrome.rs`, `src/gui/chrome/tests.rs`, `src/gui/help.rs` | S1-0 |
| S1-B | ✅ merged: PR #22 (7470a8a), 2026-11-20 | `feat/browser-s1-preview-viewers` | `Window::set_preview`: inactive frame, no bars, disabled input, no audio, no following; the budgets of 02 §4.3; the preview read without claiming, the placeholder connection, the hard-link claim on hand-over, resuming work past the budget; the `.preview` cache root | `src/gui/{text,image,hex}.rs` and tests, `src/gui/media.rs` (after G-WAVE, the default only), `src/gui/browser/prepare.rs`, `src/cache.rs` | S1-0, G-WRAP |
| S1-C | ✅ merged: PR #23 (ccd3ef3), 2026-11-21 | `feat/browser-s1-fileops` | rename, copy, move of files and folders, local and remote (`hardlink@` + `REMOVE` for files, a remote folder's move by copy, check and remove, chunked `copy-data`, staged temps, `Fs::symlink(target, link)`), never overwriting (04 §5) | `src/fileops/*.rs`, `src/remote/ops.rs`, `src/remote/{sftp,local}/ops.rs` | S1-0 |
| S1-D | ✅ merged: PR #20 (3aca81d), 2026-11-20 | `feat/browser-s1-ops-ui` | selection (Space, Ins, `+ - *`, Cmd/Ctrl+A), the Copy/Move dialog, inline rename, name-in-the-way dialog, progress, the report (04 §4–§5); drawing helpers for the rename field and selection colours, called by S1-E's `panel_ui` | `src/gui/browser/ops.rs`, `src/gui/browser/panel.rs` | S1-0 |

### Round section

- Merge dates are GitHub's, in UTC. S1-A and S1-D merged before S1-B; S1-C merged last, on
  2026-11-21.
- E3 is not in this block: 02 §8.2 records no result for it, so its row stays in `loop.md`.

## R1 (part 2) — closed 2026-11-21

- measured S1-E ✅ E3 ✅ · result: S1's integration merged and deployed (preview 0120d40813a0); the
  preview sweep run on that preview and written into 02 §8.2; S1-M reruns it for media previews ·
  closed rows: S1-E, E3 · PRs: #29, and the PR that carries this block

### Closed rows

| ID | Status | Branch | Scope | Owner files (02 §5) | After |
|---|---|---|---|---|---|
| S1-E | ✅ merged: PR #29 (0120d40), 2026-11-21; deployed as preview 0120d40813a0 | `feat/browser-s1-integration` | modes, `view::resolve` (O2, O5, O7) with the committed display, the preview host and its lifecycle (02 §3–§4), per-feature job state, wiring of S1-A to S1-D (`panel_ui` calls S1-D's helpers); S1's scenarios (02 §8.1's table), receipts, AGENTS.md and 01 §3a for S1 | `src/gui/browser.rs`, `src/gui/browser/{view,preview,tests}.rs`, `tests/cli.rs`, `AGENTS.md`, `README.md`, `.prd/01-spec.md` | S1-A, S1-B, S1-C, S1-D |
| E3 | ✅ done: 2026-11-21, result in 02 §8.2 | `exp/browser-e3-preview` | 02 §8.2 E3: viewers embedded at half width, disabled, events withheld; a 50-file sweep counting ffmpeg processes, cache growth, clicks and drags | scratch only | — |

### Round section

- S1-E (PR #29, merge commit 0120d40, 2026-11-21): deployed as preview 0120d40813a0 the same day.
  S1-M is the one unit of S1 left.
- E3: 50 files over SFTP in preview mode, the bar moved down 49 times through the key hook; 13.4 s
  for the 50 settled previews, 0 ffmpeg or ffprobe processes, the preview cache at most 196 KB and
  0 bytes after exit. Clicks and drags were not part of the run: S1-E's P5 tests cover input
  isolation.

## R1 (part 3) — closed 2026-11-23

- measured S1-M ✅ · result: S1's last unit merged, so all of S1 is merged; round 1 is closed ·
  closed rows: S1-M · PRs: #36, and the PR that carries this block

### Closed rows

| ID | Status | Branch | Scope | Owner files (02 §5) | After |
|---|---|---|---|---|---|
| S1-M | ✅ merged: PR #36 (bdb6166), 2026-11-22 | `feat/browser-s1-media-preview` | the media preview: video frame, audio waveform, their budgets and hand-over; reruns P1, P5, P6, P12 for media | `src/gui/media.rs` and tests, the preview host's media row | S1-E, G-WAVE |

### Round section

- As it was in `loop.md`, under "Round 1 — slice S1 (preview mode, F9, F2/F5/F6, F-key row, F1
  help)": "S1-0 and S1-A to S1-E are merged, with PR #14 and PR #15 (rows in `loop.done.md`). S1-M
  merged on 2026-11-22 (PR #36, bdb6166), so all of S1 is merged; its row below moves to
  `loop.done.md` in a later change. One slice at a time (02 §8.1). Worktree = `.worktrees/<branch
  with / as ->`."
- S1-M (PR #36, merge commit bdb6166): the media preview, its budgets and hand-over, with E3's
  media rerun written into 02 §8.2. The row's date is the merge commit's in +0900 (2026-11-21 in
  UTC).
- "One slice at a time" no longer holds from round 2: slices run at the same time once their
  "Needs first" is met, and only work on a shared file runs one at a time (02 §8.1 "Schedule").

## R2 (part) — E4, closed 2026-11-23

- measured E4 ✅ · result: five git reads timed over an ssh master to a remote Linux host and
  locally, on main bcd6910, and written into 02 §8.2; nothing in 03's rules changes · closed rows:
  E4 · PRs: the PR that carries this block (S2-0's)

### Closed rows

| ID | Status | Before | What (02 §8.2) |
|---|---|---|---|
| E4 | ✅ done: 2026-11-23, result in 02 §8.2 | S2 | five git reads over the ssh master, timed |

### Round section

- E4: on one remote Linux host the five small reads took 65–86 ms end to end (median), near the
  `true` floor of 65.7 ms (the account's fish config adds about 17 ms per command there); the
  100 000-commit fixture's first page took 341 ms
  remote without a commit-graph and 117 ms with one. The 150 ms rest, at most 3 in flight per host
  and the 30 s hidden stream stay. A parked stream and more than 3 in flight were not tested: E5
  and G-P3.

## E1 — closed 2026-11-23

- measured E1 ✅ · result: `HIDFKeyMode` set to 1 and back to 0 by an unprivileged program, the
  setter → registry → keyboard filter chain and the readbacks written into 02 §8.2, 04 §6.2 and
  keys.md §4.1; S7 can start · closed rows: E1 · PRs: the PR that carries this block

### Closed rows

| ID | Status | Before | What (02 §8.2) |
|---|---|---|---|
| E1 | ✅ done: 2026-11-23, result in 02 §8.2 | S7 | `IOHIDSetCFTypeParameter("HIDFKeyMode", 1)` unprivileged, read back with `ioreg`, restored |

### Round section

- E1: a Mac (macOS 26.7.1, arm64), the built-in keyboard; two windows of value 1, about 550 ms
  each, after rehearsals at 0; every call returned 0 with no entitlement; the registry and the
  keyboard filters' `FnKeyMode` followed the set, `hidutil property` and `fnState` did not. The
  F5 key itself is the owner's receipt K-F3 (04 §8).

## E2 — closed 2026-11-23

- measured E2 ✅ · result: of four routes against a Mac's own OpenSSH `sftp-server`, only the
  plain `SSH_FXP_RENAME` refused every existing name for a file or folder source; route B (the
  low-level crate on a second `sftp` channel) chosen and written into 02 §8.2 and 04 §5.6; S6 can
  start · closed rows: E2 · PRs: the PR that carries this block (S6-0's)

### Closed rows

| ID | Status | Before | What (02 §8.2) |
|---|---|---|---|
| E2 | ✅ done: 2026-11-23, result in 02 §8.2 | S6 | plain SFTP rename: `openssh-sftp-client-lowlevel` vs an auxiliary channel, against a local `sftp-server -e -l INFO` |

### Round section

- E2: OpenSSH 10.3p1 on APFS, 14 cases on each of four routes. `Fs::rename` sends
  `posix-rename@openssh.com` and replaced 6 of 11 existing targets; the plain rename (routes B, C
  and D) replaced 1, a link onto a dangling link, so a link is never sent through it. Route B
  over C: same bytes, but C turns silently into the replacing route if the crate's fallback or
  the version-reply filter changes. From the server's source, not measured: a folder rename is
  `stat` then `rename()`, so an empty folder made in between is replaced. Without `hardlink@`
  measured on OpenSSH only (`-P hardlink`); on other servers sopen first probes, once per session,
  whether a plain rename refuses an existing target, and refuses the publish if it does not
  (04 §5.6).

## E5 — closed 2026-11-23

- measured E5 ✅ · result: the runner's remote stop worked under fish, and under bash and zsh
  (`-c`, `-l -c`) emulated from a fish account, 10 of 10; a parked `git log` stream blocked
  neither SFTP nor a second command on the same master; written into 02 §8.2 and 03 §3.5, §4.5;
  S3 can start · closed rows: E5 · PRs: the PR that carries this block (S3-0's)

### Closed rows

| ID | Status | Before | What (02 §8.2) |
|---|---|---|---|
| E5 | ✅ done: 2026-11-23, result in 02 §8.2 | S3 | remote process-group stop under zsh, fish and bash login shells; an SFTP listing on the same master while a parked `git log` stream is open |

### Round section

- E5: one remote Linux host (OpenSSH 10.5p1, git 2.55.0, account shell fish); a harness built the
  runner's words from main a562e1e and redid its stop step by step, sopen's binary not run. The
  printed pid led its group and session every time; TERM emptied a plain group in 0.14–0.74 s; a
  child that ignored TERM was gone after the runner's KILL, the stop 5.3–5.6 s. git parked in
  `anon_pipe_write` about 2.2 MB ahead of what was read; SFTP listings (334–378 ms) and a second
  command (84–101 ms) ran as fast as with nothing parked. A stop that drained the output took
  0.59 s, one that did not 5.17 s: hence 03 §3.5's new clause. `MaxSessions` is the default 10
  (an estimate); git's worst case in sopen is 6 by reading the code. Not tested: a channel that
  ends while its group survives, the `stop sent` paths, bash or zsh as the real account shell,
  more than 3 sessions at once.

## R2 (part) — S2-0 to S2-E, closed 2026-11-23

- merged S2-0 (PR #64), S2-C (#67), S2-E (#68), S2-B (#69), S2-A (#70), S2-D (#72) ✅ · result: the git panel's
  parts — scaffold, runner, read model, lane layout, panel UI, the viewers' read-only flag and `.revisions` root — on
  main; S2-F wires them · closed rows: S2-0, S2-A, S2-B, S2-C, S2-D, S2-E · PRs: S2-F's (this block)

### Closed rows

| ID | Status | Branch | Scope | Owner files (02 §5) | After |
|---|---|---|---|---|---|
| S2-0 | ✅ done: merged (#64) | `feat/browser-s2-scaffold` | `mod` lines and stubs for S2's files (the git tests' `mod` lines too); the shared types `Oid`, `Detection`, `CommitRow`, `RefLabel`, `StashRow`, `StatusEntry`, `InProgress`, `Backup`; `pub(crate)` on `sftp.rs`'s `ssh_config` and its `SshConfig`; no dependency, no `keys.rs` row; E4's result in 02 §8.2 | `src/lib.rs`, `src/git/{mod,model}.rs`, `src/git/tests/model.rs`, `src/remote/{mod,sftp}.rs`, `src/gui/browser.rs` (`mod git_panel;`), `src/gui/chrome.rs` (`pub mod git;`), stubs | S1, E4 |
| S2-A | ✅ done: merged (#70) | `feat/browser-s2-runner` | `GitRunner`; `LocalGit`, `SshGit`, `ShellGit`; quoting, `<locate>`, limits, stop, progress, the command log; `exec_args(dest, state_dir)`; CI's `SOPEN_REQUIRE_GIT=1` (03 §3) | `src/git/runner.rs`, `src/remote/exec.rs`, `.github/workflows/ci.yml`, `src/git/tests/runner.rs` | S2-0 |
| S2-B | ✅ done: merged (#69) | `feat/browser-s2-read-model` | detection and its cache, the parsers, the log stream and its pages (03 §2, §4.1, §4.5); the shared types (adds only). S2-B adds: a path or name that is not UTF-8 (the model's paths are `String`; `remote::Entry` carries `lossy`), the HEAD read's result (03 §4.1), the status read's ahead/behind (`# branch.ab`), a copy (`C`) status letter | `src/git/{model,detect,parse,history}.rs`, `src/git/tests/{model,detect,parse,history}.rs` | S2-0 |
| S2-C | ✅ done: merged (#67) | `feat/browser-s2-lanes` | the lane layout, pure and deterministic, with its table tests and properties (03 §4.3, G-G2) | `src/git/graph.rs`, `src/git/tests/graph.rs` | S2-0 |
| S2-D | ✅ done: merged (#72) | `feat/browser-s2-panel-ui` | the read-only panel: the graph, the details pane, find, filters, and its look (03 §4.2–§4.7, §6) | `src/gui/browser/git_panel/{mod,graph_view,details,find,filters}.rs`, `src/gui/browser/git_panel/tests*.rs`, `src/gui/chrome/git.rs` | S2-0 |
| S2-E | ✅ done: merged (#68) | `feat/browser-s2-read-only` | the viewers' `read_only` flag (Save and Save copy off, the reason on hover) and the `.revisions` cache root (03 §6.2–§6.3) | `src/gui/{text,image,hex,media}.rs` and their tests (or only the `Window` trait's default, 02 §8.1), `src/cache.rs` | S2-0 |

### Round section

- S2-0 to S2-E each passed its module gate and review rounds; their hand-overs to S2-F (the production
  `LogStream` pipe, the runner's stop for a waiting page, Cmd/Ctrl+K, `set_remotes`, the read-only flag before a
  window's first pass, the `.revisions` root in 01 §5, `SOPEN_REQUIRE_GIT` in AGENTS.md) are S2-F's row.

## R2 (part) — S5, S7, S6-0 to S6-B, closed 2026-11-23

- merged S5-0 (PR #63), S5-B (#66), S5-A (#71), S5-C (#77), S7-A (#65), S7-B (#74), S6-0 (#73), S6-B (#75),
  S6-A (#78) ✅ · result: all of S5 (the directory tree) and all of S7 (the macOS F-key switch) on main, S7's
  owner receipt K-F3 still open; S6's scaffold, engine and dialogs on main, which S6-C wires · closed rows:
  S5-0, S5-A, S5-B, S5-C, S7-A, S7-B, S6-0, S6-A, S6-B · PRs: S6-C's (this block)

### Closed rows

| ID | Status | Branch | Scope | Owner files (02 §5) | After |
|---|---|---|---|---|---|
| S5-0 | ✅ merged: PR #63 (3fb9ca1), 2026-11-23 | `feat/browser-s5-scaffold` | `mod` lines and stubs for S5's files: `src/tree/` (one module per part of 05 §7), the `TreeFs` trait and its SFTP and local impls (no methods yet), `gui::browser::tree` with `TreeOutcome::GoTo`, `gui::chrome::tree`; the F10 and Alt+F10 rows of `keys.rs` checked against 05 §1 and 04 §2 (unchanged); 02 §8.1's schedule | `src/lib.rs`, `src/remote/{mod,sftp,local}.rs`, `src/gui/browser.rs`, `src/gui/chrome.rs` (mod lines), the stubs; `.prd/02-architecture.md`, `.prd/browser/loop*.md` | S1 |
| S5-A | ✅ merged: PR #71 (fbb8106), 2026-11-23 | `feat/browser-s5-tree-model` | the tree's model: the root (05 §2), rows (§3), R1–R10 and H1–H3 (§4), opening by itself (§5), the info line, the watchdog, phases 1–3 and the per-process cache (§6); `TreeFs` as a worker with channels, local and over SFTP (§6.4, §7); `T-root`, `T-rows`, `T-norules`, `T-auto`, `T-info`, `T-watchdog`, `T-lazy` | `src/tree/*.rs`, `src/remote/treefs.rs`, `src/remote/{sftp,local}/treefs.rs` | S5-0 |
| S5-B | ✅ merged: PR #66 (926536b), 2026-11-23 | `feat/browser-s5-tree-ui` | the tree as a side's content: its rows, the bar, the tree context's keys, the F-key row `1Help 10Tree` (05 §1); glyphs and colours (§3, §6.1) | `src/gui/browser/tree.rs`, `src/gui/chrome/tree.rs` | S5-0 |
| S5-C | ✅ merged: PR #77 (3b47c5c), 2026-11-23 | `feat/browser-s5-integration` | `Content::Tree` and O4's `tree_snapshot` (02 §3.1); the tree's own connection; `TreeOutcome::GoTo` into the panel; S5's key rows landed (S5-C adds 5 to `keys.rs`'s `LANDED_SLICES`, the set of landed slices; `LANDED`, the run landed in order from S1, stays 1 for the git panel's own key reading until S2 lands); S5's scenarios (02 §8.1's table: 02 P11, 05 D-1–D-8, 04 K-R1 slot 10, K-R3 Alt+F10), receipts; 01 §3a and §9 as 05 amends them | `src/gui/browser.rs`, `src/gui/browser/{view,tests}.rs`, `src/gui/browser/tests/s5.rs`, `src/gui/browser/tree.rs` (the model's rows mapped), `src/gui/browser/keys.rs` and `src/gui/help.rs` (hand-over), `src/gui/mod.rs` (receipt keys), `src/main.rs`, `src/lib.rs` (the module map), `tests/cli.rs`, `AGENTS.md`, `README.md`, `.prd/01-spec.md` | S5-A, S5-B |
| S7-A | ✅ merged: PR #65 (7923c53), 2026-11-23 | `feat/browser-s7-fnkeys` | the switch (`HIDFKeyMode` through IOKit, loaded at runtime), its lock, state and watchdog, the `fkeys` config key and `sopen config fkeys`; K-F1, K-F2, K-F4, K-F5, K-F6 against a fake (04 §6) | `src/fnkeys/{mod,macos,tests}.rs`, `src/config.rs`; in `src/main.rs` the hidden watchdog flags and `config fkeys` | E1 |
| S7-B | ✅ merged: PR #74 (a5bb554), 2026-11-23; the owner's receipt K-F3 (04 §8) still open | `feat/browser-s7-focus` | focus, sleep and session events into the switch, the status row's line when it did not take effect, integration; `fkeys` in AGENTS.md, README and 01 §4a; then the owner's receipt K-F3 | `src/gui/mod.rs`, `src/gui/browser.rs`, `tests/cli.rs`, docs | S7-A |
| S6-0 | ✅ merged: PR #73 (6b708b0), 2026-11-23 | `feat/browser-s6-scaffold` | `openssh-sftp-client-lowlevel = "=0.7.4"` as a direct dependency (already locked); `mod` lines and empty stubs for S6-A's new files: `remote::sftp::rename`, `remote::local::trash`, `fileops::{make_dir,delete}`; no `keys.rs` row, no behaviour change; E2's result in 02 §8.2 and 04 §5.6 (K-C4, K-N1) | `Cargo.toml`, `Cargo.lock`, `src/remote/{sftp,local}.rs`, `src/fileops/mod.rs` (mod lines), the stubs; `.prd/02-architecture.md`, `.prd/04-file-keys.md`, `.prd/browser/loop*.md` | S1, E2 |
| S6-A | ✅ merged: PR #78 (6db6ebd), 2026-11-23 | `feat/browser-s6-engine` | F7 and F8 as actions over both file layers (04 §5.4, §5.5): `MKDIR` / `mkdir`; bottom-up `REMOVE` and `RMDIR`, links as links; the local Trash (macOS `trashItemAtURL` loaded at runtime, the freedesktop Trash); the plain-rename channel (route B) for a remote folder's rename and move and for a copy's publish without `hardlink@`, links never through it (04 §5.6); K-K1, K-D1, K-M1 and K-N1 for remote folders, K-C4 without `hardlink@` (its `-P hardlink` test, `src/fileops/tests.rs`, now expects the publish) | `src/fileops/*.rs`, `src/remote/ops.rs`, `src/remote/{sftp,local}/ops.rs`, `src/remote/sftp/rename.rs`, `src/remote/local/trash.rs` | S6-0 |
| S6-B | ✅ merged: PR #75 (882a5a0), 2026-11-23 | `feat/browser-s6-dialogs` | the make-folder dialog (one line, a name with `/` refused) and the delete dialog (`Delete <name>?` / `Delete N items (F files, D folders)?`, `and everything inside it`, `permanent — no Trash on <host>`, Shift+F8's `permanent` in the error colour) (04 §5.4, §5.5). A new `ops::Job` variant needs an arm in `browser.rs`'s engine match (S6-C's file): declared, or left to S6-C | `src/gui/browser/ops.rs`, `src/gui/browser/panel.rs` | S6-0 |

### Round section

- Merge commits and their dates are main's (+0900; the same day in UTC). S5-B (#66) merged before S5-A (#71),
  and S6-B (#75) before S6-A (#78): the rows keep their IDs' order.
- S7-B's code is merged; its last step, the owner's receipt K-F3 (04 §8: a plain F5 opens the Copy dialog on
  the built-in keyboard, and `kill -9` restores the key mode), is not done, and loop.md's S7 section keeps it
  open.
- The rows are as they were in loop.md except their Status. S6-C's row stays there (in progress), and so do
  S2-F and S3's rows.
