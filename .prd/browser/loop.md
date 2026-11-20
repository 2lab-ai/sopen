# browser — loop (open items only)

Status: round 1 planned (2026-11-20). Target: [`ssot.md`](ssot.md). Contracts: `../02-architecture.md` (§8 the
slices and their order), `../03-git-panel.md`, `../04-file-keys.md`, `../05-directory-tree.md`. Closed rows move to
`loop.done.md` in the commit that closes their round.

## Round 1 — slice S1 (preview mode, F9, F2/F5/F6, F-key row, F1 help)

Before it: E3 has passed. S1-0 alone first; then S1-A to S1-D in parallel (S1-B also waits for PR #14); then S1-E.
One slice at a time (02 §8.1). Worktree = `.worktrees/<branch with / as ->`.

| ID | Status | Branch | Scope | Owner files (02 §5) | After |
|---|---|---|---|---|---|
| G-WRAP | 🚪 waiting | — | the text-wrap branch (PR #14) merged: S1-B's text preview (and P1) builds on its wrap mode; message 13 arrives with it | — | — |
| G-WAVE | 🚪 waiting | — | PR #15 merged: it rewrites `media.rs`; S1-B touches `media.rs` only after it, and only for the `set_preview` default; S1-M follows it | — | — |
| E3 | 🔴 open | `exp/browser-e3-preview` | 02 §8.2 E3: viewers embedded at half width, disabled, events withheld; a 50-file sweep counting ffmpeg processes, cache growth, clicks and drags | scratch only | — |
| S1-0 | 🔴 open | `feat/browser-s1-scaffold` | `mod` lines and stubs for S1's files; `Window::set_preview` on the trait (default: nothing); `Start` carries the ssh destination and the state dir; touches no viewer file | `src/lib.rs`, `src/main.rs`, `src/gui/mod.rs` (mod lines, the trait method), `src/gui/browser.rs` (`mod keys; ops; view; preview;`, `Start`), `src/remote/{mod,sftp,local}.rs` (mod lines), stubs | E3 |
| S1-A | 🔴 open | `feat/browser-s1-keys` | the key table with every context's rows (04 §3, 03 §9, 05 §1), exact modifiers that consume their events, typed-text `+ - *`; Cmd/Ctrl+digit → F-key in the raw-input hook; clickable F-key row; F1 help; `mode=` and `bar=` state keys | `src/gui/browser/keys.rs`, `src/gui/mod.rs`, `src/gui/chrome.rs`, `src/gui/chrome/tests.rs`, `src/gui/help.rs` | S1-0 |
| S1-B | 🔴 open | `feat/browser-s1-preview-viewers` | `Window::set_preview`: inactive frame, no bars, disabled input, no audio, no following; the budgets of 02 §4.3; the preview read without claiming, the placeholder connection, the hard-link claim on hand-over, resuming work past the budget; the `.preview` cache root | `src/gui/{text,image,hex}.rs` and tests, `src/gui/media.rs` (after G-WAVE, the default only), `src/gui/browser/prepare.rs`, `src/cache.rs` | S1-0, G-WRAP |
| S1-C | 🔴 open | `feat/browser-s1-fileops` | rename, copy, move of files and folders, local and remote (`hardlink@` + `REMOVE` for files, a remote folder's move by copy, check and remove, chunked `copy-data`, staged temps, `Fs::symlink(target, link)`), never overwriting (04 §5) | `src/fileops/*.rs`, `src/remote/ops.rs`, `src/remote/{sftp,local}/ops.rs` | S1-0 |
| S1-D | 🔴 open | `feat/browser-s1-ops-ui` | selection (Space, Ins, `+ - *`, Cmd/Ctrl+A), the Copy/Move dialog, inline rename, name-in-the-way dialog, progress, the report (04 §4–§5); drawing helpers for the rename field and selection colours, called by S1-E's `panel_ui` | `src/gui/browser/ops.rs`, `src/gui/browser/panel.rs` | S1-0 |
| S1-E | 🔴 open | `feat/browser-s1-integration` | modes, `view::resolve` (O2, O5, O7) with the committed display, the preview host and its lifecycle (02 §3–§4), per-feature job state, wiring of S1-A to S1-D (`panel_ui` calls S1-D's helpers); S1's scenarios (02 §8.1's table), receipts, AGENTS.md and 01 §3a for S1 | `src/gui/browser.rs`, `src/gui/browser/{view,preview,tests}.rs`, `tests/cli.rs`, `AGENTS.md`, `README.md`, `.prd/01-spec.md` | S1-A, S1-B, S1-C, S1-D |
| S1-M | 🔴 open | `feat/browser-s1-media-preview` | the media preview: video frame, audio waveform, their budgets and hand-over; reruns P1, P5, P6, P12 for media | `src/gui/media.rs` and tests, the preview host's media row | S1-E, G-WAVE |

## Experiments for later slices (open; each runs before its slice)

| ID | Status | Before | What (02 §8.2) |
|---|---|---|---|
| E4 | 🔴 open | S2 | five git reads over the ssh master, timed |
| E5 | 🔴 open | S3 | remote process-group stop under zsh, fish and bash login shells; an SFTP listing on the same master while a parked `git log` stream is open |
| E2 | 🔴 open | S6 | plain SFTP rename: `openssh-sftp-client-lowlevel` vs an auxiliary channel, against a local `sftp-server -e -l INFO` |
| E1 | 🔴 open | S7 | `IOHIDSetCFTypeParameter("HIDFKeyMode", 1)` unprivileged, read back with `ioreg`, restored |

## Build facts

- Gate, per WU and before every merge: `just check` (`cargo fmt --check`, `cargo clippy --all-targets --locked
  -- -D warnings`, `cargo test --locked`) with `SOPEN_REQUIRE_FFMPEG=1`; from S2 also `SOPEN_REQUIRE_GIT=1`.
  Taken from AGENTS.md; not run in round 1's planning.
- Receipts: window screenshots someone looked at (AGENTS.md "Receipts"); remote receipts under
  `/tmp/sopen-receipts/` on the remote.
- After S1: one slice at a time, S2 → S3 → S4 → S5 → S6 → S7; S1-M between slices once PR #15 has merged; S7 after
  E1 (02 §8.1).

## Next

- Run E3; open S1-0; S1-B waits for G-WRAP.
