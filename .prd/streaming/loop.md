# streaming — loop

Status: open (2026-11-20; ledger brought up to date 2026-11-21). Target: [`ssot.md`](ssot.md). Contract: [`../06-streaming.md`](../06-streaming.md)
(viewing) and [`../07-streaming-edit.md`](../07-streaming-edit.md) (editing). Design decisions live
in those documents, not here.

## Round 1 — work units and experiments

Marks: 🔴 ready to start · 🚪 waiting on what "After" names · 🟡 in progress · ✅ merged.

Gate for every unit: `SOPEN_REQUIRE_FFMPEG=1 SOPEN_REQUIRE_SFTP_SERVER=1 just check`, plus the
scenarios named. Edit units are ED1–ED6. Plain E-numbers are 07's acceptance scenarios, and the
browser's experiments are its own E1–E5 (02 §8.2).

| ID | Status | Branch | Worktree | Scope | Owns | After | Gate |
|---|---|---|---|---|---|---|---|
| W4 | 🟡 | `feat/stream-hex` | `.worktrees/feat-stream-hex` | 06 §7.1: hex over `Bytes`, Pending rows, patches keep their byte, interim Save | `src/gui/hex.rs`, `src/gui/hex/file.rs`, `src/gui/hex/tests.rs`, `src/gui/mod.rs` (`State`'s `loading` field) | — (browser S1 merged: S1-E, PR #29, 0120d40) | V1–V3, V11; screenshot with `loading=1` |
| W5a | 🟡 | `feat/stream-text-reads` | `.worktrees/feat-stream-text-reads` | 06 §7.2 W5a: reads through `Bytes`, Pending like `Stale`, first screen first, 16 MiB pass for a Source continuing S1-B's (a local file keeps its whole-file pass), highlighting over the prefix, the five defects X3 found | `src/gui/text.rs`, `src/gui/text/tests.rs` | — (browser S1 merged: S1-E, PR #29, 0120d40) | V13, V13a |
| W5b | 🚪 | `feat/stream-text-large` | `.worktrees/feat-stream-text-large` | 06 §7.2 W5b: prefix index, byte-offset keys (with wrap), unknown line starts, Go to, index bound | `src/gui/text.rs`, `src/gui/text/wrap.rs`, `src/gui/text/tests.rs` | W5a | V12 |
| W6 | 🔴 | `feat/stream-image` | `.worktrees/feat-stream-image` | 06 §7.3: header walk (SOF read directly), memory line, reserved buffer, streamed read, partial pictures | `src/gui/image.rs`, `src/gui/image/probe.rs`, `src/gui/image/robustness_tests.rs`, `Cargo.toml` / `Cargo.lock` (`png`) | — (browser S1 merged: S1-E, PR #29, 0120d40) | V14, V15; screenshots of a partial progressive JPEG and a partial plain PNG |
| W7 | 🚪 | `feat/stream-media` | `.worktrees/feat-stream-media` | 06 §7.4: URL inputs with their own options and reader kinds, progressive waveform, Export cancel, cut runs failed | `src/gui/media.rs`, `src/gui/media/*`, `src/gui/playback.rs`, `src/ffmpeg.rs`, `src/cut.rs` | S1-M merged (ready on the browser side: S1-E merged) | V16, V18, V19, V22 |
| ED6 | 🚪 | `feat/stream-edit-media` | `.worktrees/feat-stream-edit-media` | 07 §5.3, §9: Save from a URL, baseline pass, cut renders failed, hard-link backup and disclosure, Source closed after Save, journal | `src/gui/media/file.rs`, `src/gui/media.rs`, `src/gui/media/edit_tests.rs` | W7, ED2, ED3; before W8 | E15, E16, E20 |
| W8 | 🚪 | `feat/stream-routing` | `.worktrees/feat-stream-routing` | 06 §7.3, §7.5, §11, §13: routes open a Source with a placeholder `Origin`, the image → hex switch, the preview change with 02 §4.3–§4.4 and S1's P6/P12, Save to… cancel, hooks, `bytes_fetched` | `src/gui/browser/prepare.rs`, `src/gui/browser/preview.rs` (hand-over), `src/gui/browser.rs` (hex switch), `src/gui/browser/tests.rs` (P6, P12), `src/cache.rs` (`.preview` root), `src/gui/binary.rs`, `src/gui/binary/tests.rs`, `src/main.rs`, `src/kind.rs`, `src/usage.rs`, `src/forward.rs`, `src/gui/mod.rs` (`Origin`), `tests/cli.rs`, `.prd/02-architecture.md` (§4.3–§4.4) | W4, W5a, W5b, W6, W7, ED6; S1 and S1-M merged | V10 (at open), V17, V21; live V1–V3 with `SOPEN_STREAM_STATS` |
| ED3 | 🔴 | `feat/stream-edit-save-as` | `.worktrees/feat-stream-edit-save-as` | 07 §7: Save As modal, F7 in three rows | `src/gui/save_as.rs`, `src/gui/save_as/tests.rs`, `src/gui/mod.rs`, `src/gui/chrome.rs` (F7 entries) | — (browser S1 merged: S1-E, PR #29, 0120d40; S1-A owns `chrome.rs`'s body) | — |
| ED4 | 🚪 | `feat/stream-edit-hex` | `.worktrees/feat-stream-edit-hex` | 07 §3–§6, §10: hex Save, Save As, journal, Restore checks, reconciliation, rows, confirmation, prompt, `Origin` refused at Save | `src/gui/hex/file.rs`, `src/gui/hex.rs`, `src/gui/hex/tests.rs` | W4, W8, ED1, ED2, ED3 | E1, E2, E10–E13, E16–E18, E21; live E1 on a 24 GB file |
| ED5 | 🚪 | `feat/stream-edit-image` | `.worktrees/feat-stream-edit-image` | 07 §8: image journal, Save As | `src/gui/image/file.rs`, `src/gui/image.rs`, `src/gui/image/robustness_tests.rs` | W6, ED2, ED3 | E14, E16, E18 (image cases, no edits during Save) |
| D1 | 🚪 | `feat/stream-docs` | `.worktrees/feat-stream-docs` | 06 §14, 07 §13: 01, AGENTS.md and README to the shipped contract | `.prd/01-spec.md`, `AGENTS.md`, `README.md`, `.prd/06-streaming.md`, `.prd/07-streaming-edit.md` | every unit above | review |

## Merge order, both workstreams

The "After" column is authoritative; this is the order it gives. Steps 1–4 are done as of
2026-11-21; step 5 is under way: S1 merged with S1-E (PR #29, 0120d40) on 2026-11-21.

1. PR #15 (media edit) and PR #16 (stems): both merged on 2026-11-20. PR #16 shared only
   `Cargo.toml` with W1 (other sections).
2. This docs branch, rebased on `main` (S1-0 is merged), with the 02 §8 sentence below.
3. In parallel with the browser's S1-A…D: W1 and ED2; then W2, then W3 (new files).
4. ED1 after W1 and S1-C.
5. Once S1 has merged, in parallel: S1-M, W4, W5a, W6 and ED3.
6. W7 after S1-M and W3; W5b; ED5.
7. ED6, then W8. W8 also takes `browser.rs` for the hex switch, and `browser/tests.rs` together with
   the 02 §4.3–§4.4 edits.
8. ED4, then the browser's S2-E (after W4–W7 and ED4–ED6, or the trait default only), then D1.

**02 §8 (added in this docs PR):** at the end of "Hand-overs between slices": "S2-E changes the viewer files (`text`, `image`, `hex`, `media`) only after the
streaming units W4–W7 and ED4–ED6 have merged, or it touches only the `Window` trait's default
(`.prd/06-streaming.md` §13)."

Serial chains: W4 → ED4; W6 → ED5; PR #15 → S1-M → W7 → ED6; W5a → W5b; W1 → ED1; S1-C → ED1
(`src/remote/sftp/ops.rs`).

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

2026-11-21: W2 (PR #28), ED1 (PR #30) and W3 (PR #31, 5f4e6b3) merged on 2026-11-21; X3 done on
2026-11-21 (results in 06 §7.2 and §10). Their rows are in [`loop.done.md`](loop.done.md), below the
block for W1, ED2, X2, X4 and X5. X1 done on 2026-11-21 (results in 06 §7.4, §9 and §12 V16; one
open question in 06's Tensions); its row is the last block of `loop.done.md`.

- No experiment is left open: X1–X5 are done.
- Browser S1 merged on 2026-11-21 (S1-E, PR #29, 0120d40; preview 0120d40813a0), so nothing here
  waits on it any more.
  - In progress: W4 (`feat/stream-hex`, `.worktrees/feat-stream-hex`) and W5a
    (`feat/stream-text-reads`, `.worktrees/feat-stream-text-reads`). W5a takes X3's five defects
    with it (06 §7.2).
  - Ready: W6 and ED3.
- W7 waits on S1-M, which is ready on the browser side. It takes X1's rules with it (06 §7.4: the cut account
  around every render run; §12 V16's render gates) and settles the mp3 question in 06's Tensions.
