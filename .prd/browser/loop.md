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

The read-only git panel (02 §8.1 S2; contract `../03-git-panel.md`). S2-0 first, then S2-A to S2-E in parallel on
disjoint files, then S2-F. E4's result is in 02 §8.2 and its row in [`loop.done.md`](loop.done.md); S2-0 to S2-E
are merged and their rows are there too.

| ID | Status | Branch | Scope | Owner files (02 §5) | After |
|---|---|---|---|---|---|
| S2-F | 🟡 in progress | `feat/browser-s2-integration` | O6, the pin, refresh, `SOPEN_LIST=git` and the S2 receipt keys; slice 2 added to `keys.rs`'s `LANDED_SLICES` (the git panel reads the set; `LANDED: u8` gone), Cmd/Ctrl+K the filters; the production `LogStream` pipe (E5: its stop reads while it waits) and one stop session per host; S2's scenarios (02 §8.1's table), receipts, AGENTS.md, README, 01 | `src/gui/browser.rs`, `src/gui/browser/{view,keys}.rs`, `src/gui/browser/tests/s2.rs`, `src/gui/browser/git_panel/mod.rs` (the set, the Cmd/Ctrl+K hook, two readers), `src/gui/{mod,help,binary}.rs`, `src/gui/chrome/tests.rs`, `src/git/{history,runner}.rs`, `src/main.rs`, `tests/cli.rs`, `src/forward.rs`, `AGENTS.md`, `README.md`, `.prd/01-spec.md`, `.prd/03-git-panel.md` (one line) | S2-A, S2-B, S2-C, S2-D, S2-E |

## Round 2 — slice S3 (git actions, staging and commit, backups, restore and undo, the banner, the log)

Every action of 03 §7.1–§7.7 with 03 §8's safety (02 §8.1 S3; contract `../03-git-panel.md`). E5 is closed
(2026-11-23): its result is in 02 §8.2, 03 §3.5 and §4.5, its row in [`loop.done.md`](loop.done.md). S3-0 first,
then S3-A and S3-B in parallel on disjoint files; those three need S2-0–S2-E merged and E5, not S2-F. S3-C waits for
S2-F too: two integration WUs never run together (02 §8.1 "Schedule").

| ID | Status | Branch | Scope | Owner files (02 §5) | After |
|---|---|---|---|---|---|
| S3-0 | 🟡 in progress | `feat/browser-s3-scaffold` | `mod` lines and stubs for S3's files (the git tests' and `git_panel/tests_s3.rs` too); the shared types of `git::actions` (`ActionId`, `Action`, `Repo`, `Plan`, `Step`, `Confirmation`, `RemoteChange`, `Outcome`, `Banner`) and `git::backup` (`Take`, `Manifest`, `Record`, `RestoreEntry`, `Undo`); `plan`, `backup::write` and `backup::list` as `todo!()`; no `keys.rs` row (S2-F is changing that file): the rows S3 needs from 03 §9 are listed for S3-C; E5's result in 02 §8.2, 03 §3.5 and §4.5; one stop session per host (02 §6, 03 §3.5), `git_panel/mod.rs`'s S3 owner (02 §5), the banner's keys and the Backups list's (03 §9.2), G-R3's live receipt to S3-C (03 §11) | `src/git/{mod,actions,backup}.rs`, `src/git/tests/{actions,backup}.rs`, `src/gui/browser/git_panel/mod.rs` (mod lines), the stubs; `.prd/02-architecture.md`, `.prd/03-git-panel.md`, `.prd/browser/loop*.md` | S2-0–S2-E, E5 |
| S3-A | 🔴 open | `feat/browser-s3-actions` | every action of 03 §7.1–§7.7 as a pure plan — steps, confirmation (§8.1), backups (§8.2), §8.4's lease, files in the way (§8.6) — and its executor; the §8.5 in-progress probe `Snippet` and its parser into `model::InProgress`, and the banner; backups, manifests, the files snapshot and restore snippets, restore and undo (§8.2–§8.3); G-R5, G-A1–G-A9, G-S1–G-S3, G-S5–G-S8 through `LocalGit` and `ShellGit` | `src/git/{actions,backup}.rs`, `src/git/tests/{actions,backup}.rs` | S3-0 |
| S3-B | 🔴 open | `feat/browser-s3-dialogs` | the action menu (F10, Shift+F10, a right click), each action's dialog and confirmation (§8.1), the result dialog with each step's outcome (§3.8), the Backups list (§8.2), the command log (§3.7), the banner (§8.5), in the PC Shell look | `src/gui/browser/git_panel/{menu,dialogs,backups,log_view,in_progress}.rs`, `src/gui/browser/git_panel/tests_s3.rs` | S3-0 |
| S3-C | 🔴 open | `feat/browser-s3-integration` | wiring S3-A and S3-B: actions on a worker under the repository's write lock (02 §6), the refresh after (03 §5), `GitEvent::WorkTreeChanged`, the window's undo; S3-A's in-progress probe in the load and every refresh (02 §6.1, 03 §5); S3's key rows landed, and the rows 03 §9 needs added or changed — the banner's Cmd/Ctrl+Shift+C, S, A, B (03 §9.2) among them (hand-over); `SOPEN_STATE dialog=` (02 §9); S3's scenarios (02 §8.1's table: G-R5, G-A1–G-A9, G-S1–G-S8, G-K1, G-K2, G-W1 banner, backups, log), receipts; G-R3's live receipt on sopen's own binary against a real ssh host under zsh, bash and fish login shells, the master killed before Esc included (03 §11); AGENTS.md | `src/gui/browser.rs`, `src/gui/browser/{view,tests}.rs`, `src/gui/browser/git_panel/mod.rs` (hand-over), `src/gui/browser/keys.rs` and `src/gui/help.rs` (hand-over), `src/gui/mod.rs` (receipt keys), `src/main.rs`, `tests/cli.rs`, `AGENTS.md`, `README.md` | S3-A, S3-B, S2-F |

## Round 2 — slice S4 (history edits: the quick edits and the planner)

The history edits of 03 §7.8 with §8's safety (02 §8.1 S4; contract `../03-git-panel.md`). S4-0 first, then S4-A
and S4-B in parallel on disjoint files; those three need only S3's merged logic and screens (S3-A, S3-B). S4-C
waits for S3-C too: two integration WUs never run together (02 §8.1 "Schedule").

| ID | Status | Branch | Scope | Owner files (02 §5) | After |
|---|---|---|---|---|---|
| S4-0 | 🟡 in progress | `feat/browser-s4-scaffold` | `mod` lines and stubs for S4's files (`git::rebase`, its tests, `git_panel/planner.rs`, `git_panel/tests_s4.rs`); the shared types of `git::rebase` (`EditId`, `Quick`, `Edit`, `Model`, `Row`, `Op`, `Problem`, `Run`, `TodoLine`, `Exec`, `Base`, `BranchMove`, `Split`) over S3's `Plan`, `Refused`, `Repo` and `Need`; `plan`, `needs`, `open` and `open_needs` as `todo!()`; nothing added to `git::actions`; no `keys.rs` row: the rows S4 needs from 03 §9 are listed for S4-C; 02 §5's S4 rows and hand-overs, 02 §8.1's schedule | `src/git/{mod,rebase}.rs`, `src/git/tests/rebase.rs`, `src/gui/browser/git_panel/mod.rs` (mod lines), the stubs; `.prd/02-architecture.md`, `.prd/browser/loop.md` | S3-A, S3-B |
| S4-A | 🔴 open | `feat/browser-s4-rebase` | every history edit of 03 §7.8 as a pure plan over S3's `Plan`: the quick edits of §7.8.1 (the HEAD forms of `reword` and `author` as amends; `fold`'s commit and its `fixup` line, 03's estimate settled by a test), the planner's model, ops and checks (§7.8.2), its todo with the message and author lines byte for byte, `--update-refs` with the branches it moves or leaves (another worktree's), the backups (§8.2), the files in the way (§8.6), the edit stop's Split and the Continue refused while a recorded path differs (§7.8.1 `split`, §8.5); G-H1–G-H5 through `LocalGit` and `ShellGit` | `src/git/rebase.rs`, `src/git/tests/rebase.rs`; `src/git/actions.rs` (hand-over: `pub(crate)` on the helpers a rewrite shares, such as the §8.6 guard, nothing else) | S4-0 |
| S4-B | 🔴 open | `feat/browser-s4-planner` | the planner pane over the git panel (03 §7.8.2: verb, hash, subject, author, date, the `A` mark; its keys, typed letters setting verbs and never starting Find; Esc asks when something changed); the quick edits' dialogs and their confirmations (§7.8.1, §8.1), in `planner.rs`, since S3-B's `ActionDialog` takes an `Action`; the commit menu's §7.8 history row (`menu.rs`) and an edit stop's Amend and Split on the banner (`in_progress.rs`, §8.5), as new items and return types beside S3-B's, not new variants of `MenuEnd` or `Pressed`, which S3-C's wiring may match on; `Model::apply`, `changed`, `problems` and `rebase::plan` through seams of its own, so its tests run on fakes before S4-A lands; in the PC Shell look | `src/gui/browser/git_panel/{planner,menu,in_progress}.rs`, `src/gui/browser/git_panel/tests_s4.rs` | S4-0 |
| S4-C | 🔴 open | `feat/browser-s4-integration` | wiring S4-A and S4-B: the §7.8 menu row and the banner's Amend and Split that S4-B adds to `menu.rs` and `in_progress.rs`; quick edits and planner runs on a worker under the repository's write lock (`rebase::needs` and `open_needs` through `actions::inspect`, the plan through `actions::execute`), the refresh after (03 §5), `GitEvent::WorkTreeChanged`; the split kept per window, its Continue through `rebase::Edit::ContinueSplit`; F2 and F8 on a commit row as `reword` and `drop` (03 §9.1), the planner taking its keys before the git context's rows, S4's key rows landed (S4-C adds 4 to `keys.rs`'s `LANDED_SLICES`; hand-over); `SOPEN_STATE dialog=` for §7.8's ids (`EditId::parse`, 02 §9); S4's scenarios (02 §8.1's table: G-H1–G-H5, G-W1 planner), receipts | `src/gui/browser.rs`, `src/gui/browser/{view,tests}.rs`, `src/gui/browser/tests/s4.rs`, `src/gui/browser/git_panel/mod.rs` (hand-over), `src/gui/browser/keys.rs` and `src/gui/help.rs` (hand-over), `src/gui/mod.rs` (receipt keys), `src/main.rs`, `tests/cli.rs`, `AGENTS.md`, `README.md` | S4-A, S4-B, S3-C |

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
