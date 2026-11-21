# browser — loop (open items only)

Status: round 1 in progress (2026-11-21). Target: [`ssot.md`](ssot.md). Contracts: `../02-architecture.md` (§8 the
slices and their order), `../03-git-panel.md`, `../04-file-keys.md`, `../05-directory-tree.md`. Closed rows move to
`loop.done.md` in the commit that closes their round.

## Round 1 — slice S1 (preview mode, F9, F2/F5/F6, F-key row, F1 help)

S1-0 and S1-A to S1-E are merged, with PR #14 and PR #15 (rows in [`loop.done.md`](loop.done.md)). What is left of
S1 is S1-M.
One slice at a time (02 §8.1). Worktree = `.worktrees/<branch with / as ->`.

| ID | Status | Branch | Scope | Owner files (02 §5) | After |
|---|---|---|---|---|---|
| S1-M | 🔴 ready (S1-E merged) | `feat/browser-s1-media-preview` | the media preview: video frame, audio waveform, their budgets and hand-over; reruns P1, P5, P6, P12 for media | `src/gui/media.rs` and tests, the preview host's media row | S1-E, G-WAVE |

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

- 2026-11-21: S1-E merged (PR #29, 0120d40) and is deployed as preview 0120d40813a0. S1-M is ready to start, and so
  are the streaming units that waited on S1: W4 and W5a are in progress, W6 and ED3 are ready
  ([`../streaming/loop.md`](../streaming/loop.md)).
- E3 is closed (2026-11-21): its result is in 02 §8.2 and its row in [`loop.done.md`](loop.done.md). S1-M reruns
  it for media previews.
