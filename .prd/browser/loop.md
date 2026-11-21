# browser — loop (open items only)

Status: round 1 in progress (2026-11-21). Target: [`ssot.md`](ssot.md). Contracts: `../02-architecture.md` (§8 the
slices and their order), `../03-git-panel.md`, `../04-file-keys.md`, `../05-directory-tree.md`. Closed rows move to
`loop.done.md` in the commit that closes their round.

## Round 1 — slice S1 (preview mode, F9, F2/F5/F6, F-key row, F1 help)

S1-0 and S1-A to S1-D are merged, with PR #14 and PR #15 (rows in [`loop.done.md`](loop.done.md)). What is left of
S1 is S1-E, then S1-M.
One slice at a time (02 §8.1). Worktree = `.worktrees/<branch with / as ->`.

| ID | Status | Branch | Scope | Owner files (02 §5) | After |
|---|---|---|---|---|---|
| E3 | 🔴 open | `exp/browser-e3-preview` | 02 §8.2 E3: viewers embedded at half width, disabled, events withheld; a 50-file sweep counting ffmpeg processes, cache growth, clicks and drags | scratch only | — |
| S1-E | 🟡 in progress: PR #29 in review | `feat/browser-s1-integration` | modes, `view::resolve` (O2, O5, O7) with the committed display, the preview host and its lifecycle (02 §3–§4), per-feature job state, wiring of S1-A to S1-D (`panel_ui` calls S1-D's helpers); S1's scenarios (02 §8.1's table), receipts, AGENTS.md and 01 §3a for S1 | `src/gui/browser.rs`, `src/gui/browser/{view,preview,tests}.rs`, `tests/cli.rs`, `AGENTS.md`, `README.md`, `.prd/01-spec.md` | S1-A, S1-B, S1-C, S1-D |
| S1-M | 🚪 waiting on S1-E | `feat/browser-s1-media-preview` | the media preview: video frame, audio waveform, their budgets and hand-over; reruns P1, P5, P6, P12 for media | `src/gui/media.rs` and tests, the preview host's media row | S1-E, G-WAVE |

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

- 2026-11-21: S1-E is in review (PR #29). When it merges, S1-M starts, and so do the streaming units W4, W5a, W6
  and ED3 ([`../streaming/loop.md`](../streaming/loop.md)).
- E3's row stays open until its result is written into 02 §8.2: S1-B's commit (d514124) says it was shaped by E3's
  measurements, but 02 records none.
