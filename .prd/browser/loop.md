# browser — loop (open items only)

Status: round 1 merged (2026-11-22; S1-M, PR #36, was its last unit). Target: [`ssot.md`](ssot.md). Contracts: `../02-architecture.md` (§8 the
slices and their order), `../03-git-panel.md`, `../04-file-keys.md`, `../05-directory-tree.md`. Closed rows move to
`loop.done.md` in the commit that closes their round.

## Round 2 — slice S5 (the directory tree: F10, Alt+F10)

Round 1 (S1) is closed: its rows are in [`loop.done.md`](loop.done.md). S5 needs only S1, so it runs alongside
other slices (02 §8.1 "Schedule"). S5-0 first, the one serial change; then S5-A and S5-B in parallel on disjoint
files; then S5-C, the slice's integration. Contract: 05. Worktree = `.worktrees/<branch with / as ->`.

| ID | Status | Branch | Scope | Owner files (02 §5) | After |
|---|---|---|---|---|---|
| S5-0 | 🟡 in progress | `feat/browser-s5-scaffold` | `mod` lines and stubs for S5's files: `src/tree/` (one module per part of 05 §7), the `TreeFs` trait and its SFTP and local impls (no methods yet), `gui::browser::tree` with `TreeOutcome::GoTo`, `gui::chrome::tree`; the F10 and Alt+F10 rows of `keys.rs` checked against 05 §1 and 04 §2 (unchanged); 02 §8.1's schedule | `src/lib.rs`, `src/remote/{mod,sftp,local}.rs`, `src/gui/browser.rs`, `src/gui/chrome.rs` (mod lines), the stubs; `.prd/02-architecture.md`, `.prd/browser/loop*.md` | S1 |
| S5-A | 🔴 open | `feat/browser-s5-tree-model` | the tree's model: the root (05 §2), rows (§3), R1–R10 and H1–H3 (§4), opening by itself (§5), the info line, the watchdog, phases 1–3 and the per-process cache (§6); `TreeFs` as a worker with channels, local and over SFTP (§6.4, §7); `T-root`, `T-rows`, `T-norules`, `T-auto`, `T-info`, `T-watchdog`, `T-lazy` | `src/tree/*.rs`, `src/remote/treefs.rs`, `src/remote/{sftp,local}/treefs.rs` | S5-0 |
| S5-B | 🔴 open | `feat/browser-s5-tree-ui` | the tree as a side's content: its rows, the bar, the tree context's keys, the F-key row `1Help 10Tree` (05 §1); glyphs and colours (§3, §6.1) | `src/gui/browser/tree.rs`, `src/gui/chrome/tree.rs` | S5-0 |
| S5-C | 🔴 open | `feat/browser-s5-integration` | `Content::Tree` and O4's `tree_snapshot` (02 §3.1); the tree's own connection; `TreeOutcome::GoTo` into the panel; S5's key rows landed (`keys.rs` lands rows by one threshold, `slice <= LANDED`, so S5 before S2–S4 needs a change there, declared); S5's scenarios (02 §8.1's table: 02 P11, 05 D-1–D-8, 04 K-R1 slot 10, K-R3 Alt+F10), receipts; 01 §3a and §9 as 05 amends them | `src/gui/browser.rs`, `src/gui/browser/{view,tests}.rs`, `src/gui/browser/keys.rs` (hand-over), `src/gui/mod.rs` (receipt keys), `src/main.rs`, `src/lib.rs` (the module map), `tests/cli.rs`, `AGENTS.md`, `README.md`, `.prd/01-spec.md` | S5-A, S5-B |

## Experiments for later slices (open; each runs before its slice)

| ID | Status | Before | What (02 §8.2) |
|---|---|---|---|
| E5 | 🔴 open | S3 | remote process-group stop under zsh, fish and bash login shells; an SFTP listing on the same master while a parked `git log` stream is open |
| E2 | 🔴 open | S6 | plain SFTP rename: `openssh-sftp-client-lowlevel` vs an auxiliary channel, against a local `sftp-server -e -l INFO` |

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

The read-only git panel (02 §8.1 S2; contract `../03-git-panel.md`). S2-0 first, then S2-A to S2-E in parallel on
disjoint files, then S2-F. E4's result is in 02 §8.2 and its row in [`loop.done.md`](loop.done.md).

| ID | Status | Branch | Scope | Owner files (02 §5) | After |
|---|---|---|---|---|---|
| S2-0 | 🟡 in progress | `feat/browser-s2-scaffold` | `mod` lines and stubs for S2's files (the git tests' `mod` lines too); the shared types `Oid`, `Detection`, `CommitRow`, `RefLabel`, `StashRow`, `StatusEntry`, `InProgress`, `Backup`; `pub(crate)` on `sftp.rs`'s `ssh_config` and its `SshConfig`; no dependency, no `keys.rs` row; E4's result in 02 §8.2 | `src/lib.rs`, `src/git/{mod,model}.rs`, `src/git/tests/model.rs`, `src/remote/{mod,sftp}.rs`, `src/gui/browser.rs` (`mod git_panel;`), `src/gui/chrome.rs` (`pub mod git;`), stubs | S1, E4 |
| S2-A | 🔴 open | `feat/browser-s2-runner` | `GitRunner`; `LocalGit`, `SshGit`, `ShellGit`; quoting, `<locate>`, limits, stop, progress, the command log; `exec_args(dest, state_dir)`; CI's `SOPEN_REQUIRE_GIT=1` (03 §3) | `src/git/runner.rs`, `src/remote/exec.rs`, `.github/workflows/ci.yml`, `src/git/tests/runner.rs` | S2-0 |
| S2-B | 🔴 open | `feat/browser-s2-read-model` | detection and its cache, the parsers, the log stream and its pages (03 §2, §4.1, §4.5); the shared types (adds only). S2-B adds: a path or name that is not UTF-8 (the model's paths are `String`; `remote::Entry` carries `lossy`), the HEAD read's result (03 §4.1), the status read's ahead/behind (`# branch.ab`), a copy (`C`) status letter | `src/git/{model,detect,parse,history}.rs`, `src/git/tests/{model,detect,parse,history}.rs` | S2-0 |
| S2-C | 🔴 open | `feat/browser-s2-lanes` | the lane layout, pure and deterministic, with its table tests and properties (03 §4.3, G-G2) | `src/git/graph.rs`, `src/git/tests/graph.rs` | S2-0 |
| S2-D | 🔴 open | `feat/browser-s2-panel-ui` | the read-only panel: the graph, the details pane, find, filters, and its look (03 §4.2–§4.7, §6) | `src/gui/browser/git_panel/{mod,graph_view,details,find,filters}.rs`, `src/gui/browser/git_panel/tests*.rs`, `src/gui/chrome/git.rs` | S2-0 |
| S2-E | 🔴 open | `feat/browser-s2-read-only` | the viewers' `read_only` flag (Save and Save copy off, the reason on hover) and the `.revisions` cache root (03 §6.2–§6.3) | `src/gui/{text,image,hex,media}.rs` and their tests (or only the `Window` trait's default, 02 §8.1), `src/cache.rs` | S2-0 |
| S2-F | 🔴 open | `feat/browser-s2-integration` | O6, the pin, refresh, `SOPEN_LIST=git` and the S2 receipt keys; `keys.rs`'s `LANDED` raised to 2; S2's scenarios (02 §8.1's table), receipts, AGENTS.md | `src/gui/browser.rs`, `src/gui/browser/{view,preview,tests}.rs`, `src/gui/browser/keys.rs` (`LANDED`), `src/gui/mod.rs` (receipt keys), `src/main.rs`, `tests/cli.rs`, `src/forward.rs`, `AGENTS.md`, `README.md`, `.prd/01-spec.md` | S2-A, S2-B, S2-C, S2-D, S2-E |

## Round 2 — slice S7 (the macOS F-key switch)

E1 is closed (2026-11-23): its result is in 02 §8.2 and its row in [`loop.done.md`](loop.done.md).

| ID | Status | Branch | Scope | Owner files (02 §5) | After |
|---|---|---|---|---|---|
| S7-A | 🟡 in progress | `feat/browser-s7-fnkeys` | the switch (`HIDFKeyMode` through IOKit, loaded at runtime), its lock, state and watchdog, the `fkeys` config key and `sopen config fkeys`; K-F1, K-F2, K-F4, K-F5, K-F6 against a fake (04 §6) | `src/fnkeys/{mod,macos,tests}.rs`, `src/config.rs`; in `src/main.rs` the hidden watchdog flags and `config fkeys` | E1 |
| S7-B | 🔴 open | — | focus, sleep and session events into the switch, the status row's line when it did not take effect, integration; `fkeys` in AGENTS.md, README and 01 §4a; then the owner's receipt K-F3 | `src/gui/mod.rs`, `src/gui/browser.rs`, `tests/cli.rs`, docs | S7-A |
