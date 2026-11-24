# browser — loop (open items only)

Status: round 2 merged (2026-11-24; S4-C, PR #92, was its last unit) — every slice S1–S7 is on main and every
WU row is in [`loop.done.md`](loop.done.md). Target: [`ssot.md`](ssot.md). Contracts: `../02-architecture.md`
(§8 the slices and their order), `../03-git-panel.md`, `../04-file-keys.md`, `../05-directory-tree.md`.

## Open — receipts only (no WU)

| ID | Status | What | Who |
|---|---|---|---|
| K-F3 | 🚪 owner | 04 §8: a plain F5 on the Mac's built-in keyboard opens the Copy dialog with sopen in front; another app gets dictation back; `kill -9` of sopen restores the key mode within a second. Until it exists, the key half of 04 §6.2 is unproven. | the owner, at the Mac |
| G-R3-bash | 🔴 open | 03 §11: the live Esc-stop of a remote fetch under a bash login shell (zsh and fish are measured). | needs a host account whose login shell is bash |

## Round 3 — message 19 (opened 2026-11-24)

| ID | Status | What | Who |
|---|---|---|---|
| K-L2 | 🚪 owner | The owner-assisted receipt K-L1 built (`../research/fkeys-linux.md` §6): in a terminal of the real Plasma session the owner runs the receipt script, presses Esc, F1…F12 without and with Fn, Alt+Tab and Meta, then Fn+Esc and the F1 position again (does it now send F1?), and answers whether brightness or anything outside the window changed. It gives the top row's real codes (this account cannot read input devices) and confirms that brightness stays put on the real screen. Batched with K-F3. | the owner, at the Linux laptop |
| KL-W | 🔴 open | 04 §9's units, in its order: KL-W1 the key decision and shared types, KL-W5 the headless driver, KL-W2 the hand-back and KL-W3 the inhibitor side by side, KL-W4 the shell after 10's XW-shell (route B), KL-W6 the docs, KL-W7 the measured table after K-L2. The switch ships off until K-L2's table exists. | coder |

Closed in round 3 so far: K-L1 (the feasibility gate: feasible in GUI mode, with the side effects
above handed back) — [`loop.done.md`](loop.done.md), R3 part 1; K-S4 (04 §6.4 and §7 rewritten,
reviewed, merged as #100) — R3 part 2.

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
