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
