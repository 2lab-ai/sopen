# browser — loop (open items only)

Status: round 1 merged (2026-11-22; S1-M, PR #36, was its last unit). Target: [`ssot.md`](ssot.md). Contracts: `../02-architecture.md` (§8 the
slices and their order), `../03-git-panel.md`, `../04-file-keys.md`, `../05-directory-tree.md`. Closed rows move to
`loop.done.md` in the commit that closes their round.

## Round 2 — slice S5 (the directory tree: F10, Alt+F10)

All of S5 is merged — S5-0 (PR #63), S5-B (#66), S5-A (#71), S5-C (#77), 2026-11-23 — and its rows are in
[`loop.done.md`](loop.done.md). Contract: 05.

## Build facts

- Gate, per WU and before every merge: `just check` (`cargo fmt --check`, `cargo clippy --all-targets --locked
  -- -D warnings`, `cargo test --locked`) with `SOPEN_REQUIRE_FFMPEG=1`; from S2 also `SOPEN_REQUIRE_GIT=1`.
  Taken from AGENTS.md; not run in round 1's planning.
- Receipts: window screenshots someone looked at (AGENTS.md "Receipts"); remote receipts under
  `/tmp/sopen-receipts/` on the remote.
- After S1: slices run at the same time once their "Needs first" is met — S2 → S3 → S4 in that order, S7 after
  E1; only work on a shared file runs one at a time (a scaffold's `mod` lines, the `keys.rs` and `gui/mod.rs` rows,
  each slice's integration WU in `browser.rs`, `main.rs` and `view.rs`), and two integration WUs never at once
  (02 §8.1).

## Next

- 2026-11-21: S1-E merged (PR #29, 0120d40) and is deployed as preview 0120d40813a0. S1-M and the streaming units
  that waited on S1 were then ready to start: S1-M (PR #36) and W6 merged on 2026-11-22, W4 and
  W5a on 2026-11-21, and ED3 is in progress ([`../streaming/loop.md`](../streaming/loop.md)).
- 2026-11-22: S1-M merged (PR #36, bdb6166): the media preview, its budgets and hand-over, with E3's media rerun
  written into 02 §8.2. Its row is in [`loop.done.md`](loop.done.md) (R1 part 3).
- E3 is closed (2026-11-21): its result is in 02 §8.2 and its row in [`loop.done.md`](loop.done.md). S1-M reruns
  it for media previews.

## Round 2 — slice S2

All of S2 is merged — S2-0 (PR #64), S2-C (#67), S2-E (#68), S2-B (#69), S2-A (#70), S2-D (#72), S2-F (#80),
2026-11-23 — and its rows are in [`loop.done.md`](loop.done.md). Contract: 03.

## Round 2 — slice S3 (git actions, staging and commit, backups, restore and undo, the banner, the log)

Every action of 03 §7.1–§7.7 with 03 §8's safety (02 §8.1 S3; contract `../03-git-panel.md`). E5 is closed
(2026-11-23): its result is in 02 §8.2, 03 §3.5 and §4.5, its row in [`loop.done.md`](loop.done.md). S3-0 first,
then S3-A and S3-B in parallel on disjoint files; those three need S2-0–S2-E merged and E5, not S2-F. S3-C waits for
S2-F too: two integration WUs never run together (02 §8.1 "Schedule"). S3-0 (PR #79), S3-B (#81) and S3-A (#83)
are merged (2026-11-23), their rows in [`loop.done.md`](loop.done.md); S3-C wires them.

| ID | Status | Branch | Scope | Owner files (02 §5) | After |
|---|---|---|---|---|---|
| S3-C | 🟡 in progress | `feat/browser-s3-integration` | wiring S3-A and S3-B: actions on a worker under the repository's write lock (02 §6), the refresh after (03 §5), `GitEvent::WorkTreeChanged`, the window's undo; S3-A's in-progress probe in the load and every refresh (02 §6.1, 03 §5); S3's key rows landed, and the rows 03 §9 needs added or changed — the banner's Cmd/Ctrl+Shift+C, S, A, B (03 §9.2) among them (hand-over); `SOPEN_STATE dialog=` (02 §9); S3's scenarios (02 §8.1's table: G-R5, G-A1–G-A9, G-S1–G-S8, G-K1, G-K2, G-W1 banner, backups, log), receipts; G-R3's live receipt on sopen's own binary against a real ssh host under zsh, bash and fish login shells, the master killed before Esc included (03 §11); AGENTS.md | `src/gui/browser.rs`, `src/gui/browser/{view,tests}.rs`, `src/gui/browser/git_panel/mod.rs` (hand-over), `src/gui/browser/keys.rs` and `src/gui/help.rs` (hand-over), `src/gui/mod.rs` (receipt keys), `src/main.rs`, `tests/cli.rs`, `AGENTS.md`, `README.md` | S3-A, S3-B, S2-F |

## Round 2 — slice S7 (the macOS F-key switch)

E1 is closed (2026-11-23): its result is in 02 §8.2 and its row in [`loop.done.md`](loop.done.md). S7-A (PR #65)
and S7-B (#74) are merged (2026-11-23), their rows in `loop.done.md`. **Still open: the owner's receipt K-F3**
(04 §8): a plain F5 on the built-in keyboard opens the Copy dialog, another app gets dictation back, and
`kill -9` of sopen in front restores the key mode within a second. Until it exists, the key half of 04 §6.2 is
unproven.

## Round 2 — slice S6 (F7 make folder, F8 delete, remote folder rename, copy publish without `hardlink@`)

E2 is closed (2026-11-23): route B, its result in 02 §8.2 and 04 §5.6, its row in [`loop.done.md`](loop.done.md).
S6-0 (PR #73), S6-B (#75) and S6-A (#78) are merged (2026-11-23), their rows in `loop.done.md`; S6-C wires them.

| ID | Status | Branch | Scope | Owner files (02 §5) | After |
|---|---|---|---|---|---|
| S6-C | 🟡 in progress | `feat/browser-s6-integration` | wiring S6-A and S6-B: F7, F8, Del, ⌘⌫, Shift+F8, Shift+Del into the dialogs and the engine; F2 and F6 of a remote folder through the plain rename; S6's key rows landed (`keys.rs` lands rows by one threshold, `slice <= LANDED`, so S6 before S2–S5 needs a change there, declared); S6's scenarios (02 §8.1's table: 04 K-R1 slots 7, 8, K-R3 Shift+F8, K-K1, K-D1, K-N1 remote folders, K-C4 without `hardlink@`), receipts; AGENTS.md, README, 01 §3a | `src/gui/browser.rs`, `src/gui/browser/{view,tests}.rs`, `src/gui/browser/keys.rs` (hand-over), `src/gui/mod.rs` (receipt keys), `src/main.rs`, `tests/cli.rs`, `AGENTS.md`, `README.md`, `.prd/01-spec.md` | S6-A, S6-B |
