# streaming — loop

Status: open (2026-11-20; ledger brought up to date 2026-11-22). Target: [`ssot.md`](ssot.md). Contract: [`../06-streaming.md`](../06-streaming.md)
(viewing) and [`../07-streaming-edit.md`](../07-streaming-edit.md) (editing). Design decisions live
in those documents, not here.

## Round 1 — work units and experiments

Marks: 🔴 ready to start · 🚪 waiting on what "After" names · 🟡 in progress · ✅ merged.

Gate for every unit: `SOPEN_REQUIRE_FFMPEG=1 SOPEN_REQUIRE_SFTP_SERVER=1 just check`, plus the
scenarios named. Edit units are ED1–ED6. Plain E-numbers are 07's acceptance scenarios, and the
browser's experiments are its own E1–E5 (02 §8.2).

| ID | Status | Branch | Worktree | Scope | Owns | After | Gate |
|---|---|---|---|---|---|---|---|
| W7 | 🟡 | `feat/stream-media` | `.worktrees/feat-stream-media` | 06 §7.4: URL inputs with their own options and reader kinds, progressive waveform, Export cancel, cut runs failed | `src/gui/media.rs`, `src/gui/media/*`, `src/gui/playback.rs`, `src/ffmpeg.rs`, `src/cut.rs` | — (S1-M merged: PR #36, bdb6166; W3 merged: PR #31, 5f4e6b3) | V16, V18, V19, V22 |
| ED6 | 🚪 | `feat/stream-edit-media` | `.worktrees/feat-stream-edit-media` | 07 §5.3, §9: Save from a URL, baseline pass, cut renders failed, hard-link backup and disclosure, Source closed after Save, journal | `src/gui/media/file.rs`, `src/gui/media.rs`, `src/gui/media/edit_tests.rs` | W7, ED2, ED3; before W8b | E15, E16, E20 |
| W8b | 🚪 | `feat/stream-routing-b` | `.worktrees/feat-stream-routing-b` | 06 §7.3, §13: remote text over 16 MiB through a Source, the image → hex switch, the image and media routes and previews through a Source with 02 §4.3–§4.4 and P6's image part, the `.ts` refine over the URL, the `.preview` root | `src/gui/browser/prepare.rs`, `src/gui/browser/preview.rs`, `src/gui/browser.rs` (hex switch), `src/gui/browser/tests.rs`, `src/gui/browser/prepare/tests.rs`, `src/cache.rs` (`.preview` root), `src/main.rs`, `src/kind.rs`, `.prd/02-architecture.md` (§4.3–§4.4) | W8a, W5b, W6, W7, ED6; S1-M merged | V15's Hex view; live V2–V3 with `SOPEN_STREAM_STATS` |
| ED4 | 🟡 | `feat/stream-edit-hex` | `.worktrees/feat-stream-edit-hex` | 07 §3–§6, §10: hex Save, Save As, journal, Restore checks, reconciliation, rows, confirmation, prompt, `Origin` refused at Save | `src/gui/hex/file.rs`, `src/gui/hex.rs`, `src/gui/hex/tests.rs` | — (W4, W8a, ED1, ED2 merged; ED3 merged: PR #44, 485c240) | E1, E2, E10–E13, E16–E18, E21; live E1 on a 24 GB file |
| ED5 | 🟡 | `feat/stream-edit-image` | `.worktrees/feat-stream-edit-image` | 07 §8: image journal, Save As | `src/gui/image/file.rs`, `src/gui/image.rs`, `src/gui/image/robustness_tests.rs` | W6, ED2, ED3 | E14, E16, E18 (image cases, no edits during Save) |
| D1 | 🚪 | `feat/stream-docs` | `.worktrees/feat-stream-docs` | 06 §14, 07 §13: 01, AGENTS.md and README to the shipped contract | `.prd/01-spec.md`, `AGENTS.md`, `README.md`, `.prd/06-streaming.md`, `.prd/07-streaming-edit.md` | every unit above | review |

## Merge order, both workstreams

The "After" column is authoritative; this is the order it gives. As of 2026-11-22, steps 1–5 and 7
are done; W7 and ED5 are left of step 6.

1. PR #15 (media edit) and PR #16 (stems): both merged on 2026-11-20. PR #16 shared only
   `Cargo.toml` with W1 (other sections).
2. This docs branch, rebased on `main` (S1-0 is merged), with the 02 §8 sentence below.
3. In parallel with the browser's S1-A…D: W1 and ED2; then W2, then W3 (new files).
4. ED1 after W1 and S1-C.
5. Once S1 has merged, in parallel: S1-M, W4, W5a, W6 and ED3.
6. W7 after S1-M and W3; W5b; ED5.
7. W8a after W4 and W5a, not waiting for step 6: hex and text of at most 16 MiB, with the 02
   §4.3–§4.5 edits. S1-M (PR #36) shares `prepare.rs` and `preview.rs`: the second to merge rebases.
8. ED6, then W8b. W8b also takes `browser.rs` for the hex switch.
9. ED4 (after W8a), then the browser's S2-E (after W4–W7 and ED4–ED6, or the trait default only), then D1.

**02 §8 (added in this docs PR):** at the end of "Hand-overs between slices": "S2-E changes the viewer files (`text`, `image`, `hex`, `media`) only after the
streaming units W4–W7 and ED4–ED6 have merged, or it touches only the `Window` trait's default
(`.prd/06-streaming.md` §13)."

Serial chains: W4 → ED4; W6 → ED5; PR #15 → S1-M → W7 → ED6; W5a → W5b; W1 → ED1; S1-C → ED1
(`src/remote/sftp/ops.rs`); W8a → W8b (`browser/prepare.rs`, `browser/preview.rs`).

## Build facts

- `main` 3a426b6, 2026-11-20, the Mac (Apple silicon, cargo 1.99.1, fresh target directory):
  `SOPEN_REQUIRE_FFMPEG=1 SOPEN_REQUIRE_SFTP_SERVER=1 just check` exit 0 in 82 s; fmt and clippy clean;
  lib 460, main 13, `tests/cli.rs` 27 passed; `tests/sftp_live.rs` 4 ignored. Not re-run after PR #14,
  PR #17 and S1-0 (d970a3f): this branch changes only `.prd/`.
- Checked 2026-11-20 with throwaway clients, results cited where used (06 §2, §4, §5, §7.4, §9; 07 §5.3,
  §12): OpenSSH 10.3p1 `sftp-server` (read limit and full replies, EOF cases, reply order, `fstat` after
  writes and rename-over, `copy-data`, hard link and old-handle write); ffmpeg 9.0.2 (request forms,
  exit 0 on a cut-short body).

## Next start point

2026-11-22: the browser's S1-M (PR #36, bdb6166) merged on 2026-11-22, and so did W8a (PR #37,
09cafaf), W6 (PR #39, b75c392) and W5b (PR #40, bd49904), with three fixes that are not work units:
PR #38 (d7f9000, from W8a's live probe), PR #41 (622e843, a media decoder race) and PR #42
(7544dd8, highlighting on long word runs). ED3 (PR #44, 485c240) merged on 2026-11-22 too. The W
rows and ED3's are the last two blocks of [`loop.done.md`](loop.done.md). No experiment is left
open: X1–X5 are done.

- In progress since 2026-11-22, in parallel: W7 (media over URLs) and ED4 (hex edit). W7 takes
  X1's rules with it (06 §7.4: the cut account around every render run; §12 V16's render gates)
  and settles the mp3 question in 06's Tensions.
- Then ED5 (after ED3) and ED6 (after W7 and ED3); then W8b (after W7 and ED6); then D1.
