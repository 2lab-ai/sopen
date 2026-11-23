# browser — loop (open items only)

Status: round 2 merged (2026-11-24; S4-C, PR #92, was its last unit) — every slice S1–S7 is on main and every
WU row is in [`loop.done.md`](loop.done.md). Target: [`ssot.md`](ssot.md). Contracts: `../02-architecture.md`
(§8 the slices and their order), `../03-git-panel.md`, `../04-file-keys.md`, `../05-directory-tree.md`.

## Open — receipts only (no WU)

| ID | Status | What | Who |
|---|---|---|---|
| K-F3 | 🚪 owner | 04 §8: a plain F5 on the Mac's built-in keyboard opens the Copy dialog with sopen in front; another app gets dictation back; `kill -9` of sopen restores the key mode within a second. Until it exists, the key half of 04 §6.2 is unproven. | the owner, at the Mac |
| G-R3-bash | 🔴 open | 03 §11: the live Esc-stop of a remote fetch under a bash login shell (zsh and fish are measured). | needs a host account whose login shell is bash |

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
- Clippy differs by toolchain: on 2026-11-24 the Mac's clippy passed S4-C while the Linux gate's (brew 1.96 and
  stable 1.100) refused `nonminimal_bool` — the Linux gate's two clippy runs are part of the merge gate.
