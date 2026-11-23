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
| S5-C | 🔴 open | `feat/browser-s5-integration` | `Content::Tree` and O4's `tree_snapshot` (02 §3.1); the tree's own connection; `TreeOutcome::GoTo` into the panel; S5's key rows landed (S5-C adds 5 to `keys.rs`'s `LANDED_SLICES`, the set of landed slices; `LANDED`, the run landed in order from S1, stays 1 for the git panel's own key reading until S2 lands); S5's scenarios (02 §8.1's table: 02 P11, 05 D-1–D-8, 04 K-R1 slot 10, K-R3 Alt+F10), receipts; 01 §3a and §9 as 05 amends them | `src/gui/browser.rs`, `src/gui/browser/{view,tests}.rs`, `src/gui/browser/tests/s5.rs`, `src/gui/browser/tree.rs` (the model's rows mapped), `src/gui/browser/keys.rs` and `src/gui/help.rs` (hand-over), `src/gui/mod.rs` (receipt keys), `src/main.rs`, `src/lib.rs` (the module map), `tests/cli.rs`, `AGENTS.md`, `README.md`, `.prd/01-spec.md` | S5-A, S5-B |

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

## Round 2 — slice S7 (the macOS F-key switch)

E1 is closed (2026-11-23): its result is in 02 §8.2 and its row in [`loop.done.md`](loop.done.md).

| ID | Status | Branch | Scope | Owner files (02 §5) | After |
|---|---|---|---|---|---|
| S7-A | 🟡 in progress | `feat/browser-s7-fnkeys` | the switch (`HIDFKeyMode` through IOKit, loaded at runtime), its lock, state and watchdog, the `fkeys` config key and `sopen config fkeys`; K-F1, K-F2, K-F4, K-F5, K-F6 against a fake (04 §6) | `src/fnkeys/{mod,macos,tests}.rs`, `src/config.rs`; in `src/main.rs` the hidden watchdog flags and `config fkeys` | E1 |
| S7-B | 🟡 in progress: code done; the owner's K-F3 open | `feat/browser-s7-focus` | focus, sleep and session events into the switch, the status row's line when it did not take effect, integration; `fkeys` in AGENTS.md, README and 01 §4a; then the owner's receipt K-F3 | `src/gui/mod.rs`, `src/gui/browser.rs`, `tests/cli.rs`, docs | S7-A |

## Round 2 — slice S6 (F7 make folder, F8 delete, remote folder rename, copy publish without `hardlink@`)

E2 is closed (2026-11-23): route B, its result in 02 §8.2 and 04 §5.6, its row in [`loop.done.md`](loop.done.md).

| ID | Status | Branch | Scope | Owner files (02 §5) | After |
|---|---|---|---|---|---|
| S6-0 | 🟡 in progress | `feat/browser-s6-scaffold` | `openssh-sftp-client-lowlevel = "=0.7.4"` as a direct dependency (already locked); `mod` lines and empty stubs for S6-A's new files: `remote::sftp::rename`, `remote::local::trash`, `fileops::{make_dir,delete}`; no `keys.rs` row, no behaviour change; E2's result in 02 §8.2 and 04 §5.6 (K-C4, K-N1) | `Cargo.toml`, `Cargo.lock`, `src/remote/{sftp,local}.rs`, `src/fileops/mod.rs` (mod lines), the stubs; `.prd/02-architecture.md`, `.prd/04-file-keys.md`, `.prd/browser/loop*.md` | S1, E2 |
| S6-A | 🔴 open | `feat/browser-s6-engine` | F7 and F8 as actions over both file layers (04 §5.4, §5.5): `MKDIR` / `mkdir`; bottom-up `REMOVE` and `RMDIR`, links as links; the local Trash (macOS `trashItemAtURL` loaded at runtime, the freedesktop Trash); the plain-rename channel (route B) for a remote folder's rename and move and for a copy's publish without `hardlink@`, links never through it (04 §5.6); K-K1, K-D1, K-M1 and K-N1 for remote folders, K-C4 without `hardlink@` (its `-P hardlink` test, `src/fileops/tests.rs`, now expects the publish) | `src/fileops/*.rs`, `src/remote/ops.rs`, `src/remote/{sftp,local}/ops.rs`, `src/remote/sftp/rename.rs`, `src/remote/local/trash.rs` | S6-0 |
| S6-B | 🔴 open | `feat/browser-s6-dialogs` | the make-folder dialog (one line, a name with `/` refused) and the delete dialog (`Delete <name>?` / `Delete N items (F files, D folders)?`, `and everything inside it`, `permanent — no Trash on <host>`, Shift+F8's `permanent` in the error colour) (04 §5.4, §5.5). A new `ops::Job` variant needs an arm in `browser.rs`'s engine match (S6-C's file): declared, or left to S6-C | `src/gui/browser/ops.rs`, `src/gui/browser/panel.rs` | S6-0 |
| S6-C | 🔴 open | `feat/browser-s6-integration` | wiring S6-A and S6-B: F7, F8, Del, ⌘⌫, Shift+F8, Shift+Del into the dialogs and the engine; F2 and F6 of a remote folder through the plain rename; S6's key rows landed (`keys.rs` lands rows by one threshold, `slice <= LANDED`, so S6 before S2–S5 needs a change there, declared); S6's scenarios (02 §8.1's table: 04 K-R1 slots 7, 8, K-R3 Shift+F8, K-K1, K-D1, K-N1 remote folders, K-C4 without `hardlink@`), receipts; AGENTS.md, README, 01 §3a | `src/gui/browser.rs`, `src/gui/browser/{view,tests}.rs`, `src/gui/browser/keys.rs` (hand-over), `src/gui/mod.rs` (receipt keys), `src/main.rs`, `tests/cli.rs`, `AGENTS.md`, `README.md`, `.prd/01-spec.md` | S6-A, S6-B |
