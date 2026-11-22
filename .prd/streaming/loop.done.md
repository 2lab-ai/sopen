# streaming — done (append-only)

Loop: [`loop.md`](loop.md). Each round close appends one block at the bottom; nothing above it ever
changes. Rows here are snapshots taken when they were moved.

## R1 (part) — closed 2026-11-21

- measured W1 ✅ ED2 ✅ X2 ✅ X4 ✅ X5 ✅ · result: the SFTP read transport and the edit journal merged
  and deployed (preview 4a282e4e7f2d on both hosts); three experiments written into 06 and 07 ·
  closed rows: W1, ED2, X2, X4, X5 · PRs: #24, #25, #26

### Closed rows

| ID | Status | Branch | Worktree | Scope | Owns | After | Gate |
|---|---|---|---|---|---|---|---|
| X2 | ✅ | — (scratch) | — | Server `copy-data` on real disks: 4 GiB on the Linux laptop's disk and on APFS, cold cache, 64 MiB requests, fsync every 2 GiB; the 6.3 s figure was tmpfs (07 §4.2). About 30 min. | none | before ED1 | Done (X2): 4 GiB cold copy-data 2.2 GB/s laptop disk, 4.5 GB/s Mac APFS; 24 GiB 16–17 s / 5–14 s; numbers + fsync guard in 07 §1, §4.2 (PR #26) |
| X4 | ✅ | — (scratch) | — | The JPEG walk to SOF and dimensions read from it, on a JPEG cut right after SOF and on about 20 camera-style JPEGs with large MPF/XMP segments (06 §7.3). About 30 min. | none | before W6 | Done (X4): walk-to-SOF right on 894 real + 40 built JPEGs; ≤ 42,662 bytes to SOF on real files; rules in 06 §7.3 (PR #26) |
| X5 | ✅ | — (scratch) | — | mp3 `+fastseek` precision: a VBR `lavfi` sine whose frequency steps every second; seek to 480 s with and without the flag and compare the decoded second (06 §7.4). About 1 hour. | none | before W7 | Done (X5): +fastseek CBR within 0.026 s, Xing VBR up to 4.3 s (10 min) / 21.8 s (1 h), no-header VBR up to 161 s → fastseek only for LAME `Info` (CBR), 06 §7.4 (PR #26) |
| W1 | ✅ | `feat/stream-transport` | `.worktrees/feat-stream-transport` | 06 §2: `Ranged` with `reopen` / `into_connection`, `into_ranged` giving the connection back, explicit offsets, short replies and early ends, in-order sending, the validation `fstat`, pipelined `read_to`, the clipping proxy | `src/remote/ranged.rs`, `src/remote/sftp/ranged.rs`, `src/remote/sftp.rs`, `src/remote/mod.rs`, `src/remote/local.rs`, `src/remote/tests.rs`, `Cargo.toml` / `Cargo.lock` (dev-features, if any) | — (rebase if PR #16 lands first: other `Cargo.toml` sections) | Merged PR #24 (592fe06), deployed preview 4a282e4e7f2d; cachy gate 754+14+43; live sftp_live 4/4 |
| ED2 | ✅ | `feat/stream-edit-journal` | `.worktrees/feat-stream-edit-journal` | 07 §3–§3.1: journal, durable generations, evidence, save intent and reconciliation | `src/journal.rs`, `src/journal/tests.rs`, `src/lib.rs` | — | Merged PR #25 (4a282e4), deployed preview 4a282e4e7f2d; cachy gate 775+14+43 |

### Round section

- W1 (PR #24): 5 review rounds (6 gpt6 area reviews, then the transport reviewer); pipelined
  `read_to` 2.1 GB/s against 0.5 GB/s for the old loop on a local `sftp-server`; the old reader's
  silent short read on a clipping server is fixed. The hand-back waits for the CLOSE to be written
  (observed on the pipe, at most 200 ms). Merging it into S1-B's tests moved one test's "full-size
  response" from 261,120 bytes to the 64 KiB request.
- ED2 (PR #25): 4 review rounds (8 gpt6 area reviews in round 1); the journal creates nothing above
  the cache root and fails closed below it; one intent validator for writer and reader.
- X2, X4, X5: scratch experiments; their numbers and the decisions taken from them are in the
  numbered documents, not here.

## R1 (part 2) — closed 2026-11-21

- measured W2 ✅ ED1 ✅ W3 ✅ X3 ✅ · result: the stream core, the edit patch path and the byte
  server merged; X3's measurements written into 06 §7.2 and §10 · closed rows: X3, W2, W3, ED1 ·
  PRs: #28, #30, #31, and the docs PR that carries this block

### Closed rows

| ID | Status | Branch | Worktree | Scope | Owns | After | Gate |
|---|---|---|---|---|---|---|---|
| X3 | ✅ | — (scratch) | — | A fake slow `Bytes` returning Pending, carried like `Stale`; count frames to the first screen; then jump into a 1 GB single-line file (06 §7.2). About half a day. | none | before W5a | Done (X3), 2026-11-21, on 41956b9 with a scratch patch: with any latency the first screen is the 4th pass, one round trip plus one frame after open; a jump into a 1 GiB line with no newline is one round trip of at most 3 blocks and read at most 49,741 bytes; five defects for W5a; text in 06 §7.2 and §10 (this docs PR) |
| W2 | ✅ | `feat/stream-core` | `.worktrees/feat-stream-core` | 06 §3–§6: `Source`, cache and floors, admission, validation and generations, change, MaxSessions, the stats events | `src/stream/{mod,cache,prefetch,source}.rs`, `src/stream/tests.rs`, `src/lib.rs` | W1 | Merged PR #28 (41956b9), 2026-11-21 |
| W3 | ✅ | `feat/stream-http` | `.worktrees/feat-stream-http` | 06 §9: the byte server, reader-kind paths, range table, limits, shutdown, cut account | `src/stream/http.rs`, `src/stream/http/tests.rs` | W2 | Merged PR #31 (5f4e6b3), 2026-11-21 |
| ED1 | ✅ | `feat/stream-edit-patch` | `.worktrees/feat-stream-edit-patch` | 07 §4–§5.1: `PatchFs`, S1-C's `copy-data` loop at about 1 s per request, fresh-read rewrite, write in place, undo file, baseline page hashes, step hooks | `src/remote/patch.rs`, `src/remote/patch/tests.rs`, `src/remote/sftp/patch.rs`, `src/remote/local/patch.rs`, `src/remote/sftp/ops.rs` (chunk size), `mod` lines in `src/remote/{mod,sftp,local}.rs` | W1; S1-C merged; X2 | Merged PR #30 (adb036e), 2026-11-21 |

### Round section

- W2 (PR #28, merged 2026-11-21, 41956b9), ED1 (PR #30, merged 2026-11-21, adb036e) and W3 (PR #31,
  merged 2026-11-21, 5f4e6b3).
- X3: a scratch experiment on 41956b9. Its numbers, the rules taken from them and the five defects
  W5a must fix are in 06 §7.2 and §10, not here.

## R1 (part 3) — closed 2026-11-21

- measured X1 ✅ · result: the editor's real render and playback runs measured through the byte
  server; the server's limit raised to 64 and its 60 s rule kept to `bulk`; the numbers and W7's
  rules written into 06 §7.4, §9, §10 and §12 · closed rows: X1 · PRs: the PR that carries this
  block

### Closed rows

| ID | Status | Branch | Worktree | Scope | Owns | After | Gate |
|---|---|---|---|---|---|---|---|
| X1 | ✅ | — (scratch) | — | Spike: PR #15's real `edit_args` (10 spans, up to 24 inputs) and playback command lines against the real `stream::http::Server` (W3, merged) with its 64 KiB blocks, 64 MiB cache and the cap; log bytes fetched against bytes sent (06 §7.4). About half a day. | none | before W7 (PR #15 and W3 are merged) | Done (X1), 2026-11-21, on 5f4e6b3 with a scratch harness, ffmpeg 9.0.2: at 32 connections 3 of 9 24-input mp4 renders had an input refused and ffmpeg exited 0 on a short output, at 64 none of 19; the 60 s rule cut a waiting render input at 66 s and playback's next piece at 65 s; an mp4 or wav input fetched at most 1.18 MiB beyond what was sent and no block twice; a 144 MB mp3 with 10 inputs fetched 304 MB to keep 4 MB; text in 06 §7.4, §9, §10, §12 V16 and Tensions (this PR) |

### Round section

- X1: a scratch experiment on 5f4e6b3, with the limit, the cap and the 60 s rule each changed in
  the scratch copy for an A/B run. Its numbers and the rules taken from them are in 06 §7.4, §9 and
  §12 V16, not here. The code change it led to (`MAX_CONNECTIONS` 64, `Stalled` on `bulk` only)
  is in `src/stream/http.rs`, in the same PR.
- One question it left open is in 06's Tensions, for W7: an mp3 render reads the file once per
  input.

## R1 (part 4) — closed 2026-11-21

- merged W4 ✅ W5a ✅ · result: the hex window and the text viewer read through `Bytes`, tested
  over a Source; no route opened one yet (W8a does) · closed rows: W4, W5a · PRs: #35 (64b8443),
  #34 (7f3d414)

### Closed rows

| ID | Status | Branch | Worktree | Scope | Owns | After | Gate |
|---|---|---|---|---|---|---|---|
| W4 | ✅ | `feat/stream-hex` | `.worktrees/feat-stream-hex` | 06 §7.1: hex over `Bytes`, Pending rows, patches keep their byte, interim Save | `src/gui/hex.rs`, `src/gui/hex/file.rs`, `src/gui/hex/tests.rs`, `src/gui/mod.rs` (`State`'s `loading` field) | — (browser S1 merged: S1-E, PR #29, 0120d40) | Merged PR #35 (64b8443), 2026-11-21 |
| W5a | ✅ | `feat/stream-text-reads` | `.worktrees/feat-stream-text-reads` | 06 §7.2 W5a: reads through `Bytes`, Pending like `Stale`, first screen first, 16 MiB pass for a Source continuing S1-B's (a local file keeps its whole-file pass), highlighting over the prefix, the five defects X3 found | `src/gui/text.rs`, `src/gui/text/tests.rs` | — (browser S1 merged: S1-E, PR #29, 0120d40) | Merged PR #34 (7f3d414), 2026-11-21 |

### Round section

- W4 (PR #35, merged 2026-11-21, 64b8443) and W5a (PR #34, merged 2026-11-21, 7f3d414).
- W8 was split on 2026-11-21 into W8a and W8b (06 §13): W8a needs only these two.

## R1 (part 5) — closed 2026-11-22

- merged W8a ✅ W6 ✅ W5b ✅ · result: remote hex of any size opens as a Source from `--hex`, the
  dialog, the browser's Enter and its preview, and remote text of at most 16 MiB from the browser's
  Enter and preview; the image viewer reads the header first and the text viewer reads past 16 MiB
  by byte position, but no route opens either one over a Source yet (W8b does) · closed rows: W8a,
  W6, W5b · PRs: #37 (09cafaf), #39 (b75c392), #40 (bd49904)

### Closed rows

| ID | Status | Branch | Worktree | Scope | Owns | After | Gate |
|---|---|---|---|---|---|---|---|
| W8a | ✅ | `feat/stream-routing-a` | `.worktrees/feat-stream-routing-a` | 06 §7.1, §7.2, §7.5, §11, §13: hex of any size opens a Source on the browser's Enter, the standalone dialog and `--hex`; remote text of at most 16 MiB on the browser's Enter and preview only (standalone text stays with the editor; larger text and local files keep today's route); a placeholder `Origin`, the preview change for text and hex with 02 §4.3–§4.5 and S1's P3/P4/P6/P12, `Window::set_shown`, one preview-index constant, Save to… cancel, hooks forwarded, `bytes_fetched`, ssh's stderr on a failed start | `src/gui/streamed.rs` (new), `src/gui/browser/prepare.rs`, `src/gui/browser/prepare/tests.rs`, `src/gui/browser/preview.rs`, `src/gui/browser.rs`, `src/gui/browser/tests.rs`, `src/gui/browser/tests/s1.rs`, `src/gui/binary.rs`, `src/gui/binary/tests.rs`, `src/main.rs`, `src/kind.rs`, `src/usage.rs`, `src/forward.rs`, `src/gui/mod.rs` (`Origin`, `Window`), `src/gui/hex.rs` and `src/gui/text.rs` (`set_shown`, the preview index) with their stream tests, `src/remote/sftp.rs` (`start_sftp`'s stderr), `tests/cli.rs`, `.prd/02-architecture.md` (§4.3–§4.5) | — (W4 and W5a merged: PR #35, PR #34; S1 merged) | Merged PR #37 (09cafaf), 2026-11-22 |
| W6 | ✅ | `feat/stream-image` | `.worktrees/feat-stream-image` | 06 §7.3: header walk (SOF read directly), memory line, reserved buffer, streamed read, partial pictures | `src/gui/image.rs`, `src/gui/image/probe.rs`, `src/gui/image/robustness_tests.rs`, `Cargo.toml` / `Cargo.lock` (`png`) | — (browser S1 merged: S1-E, PR #29, 0120d40) | Merged PR #39 (b75c392), 2026-11-22 |
| W5b | ✅ | `feat/stream-text-large` | `.worktrees/feat-stream-text-large` | 06 §7.2 W5b: prefix index, byte-offset keys (with wrap), unknown line starts, Go to, index bound | `src/gui/text.rs`, `src/gui/text/wrap.rs`, `src/gui/text/tests.rs` | W5a | Merged PR #40 (bd49904), 2026-11-22 |

### Round section

- W8a (PR #37, merged 2026-11-22, 09cafaf; preview 09cafaf4da9f), W6 (PR #39, merged 2026-11-22,
  b75c392; preview b75c39212ccf) and W5b (PR #40, merged 2026-11-22, bd49904; preview bd49904eec9f).
- PR #38 (merged 2026-11-22, d7f9000) is not a work unit: it fixes what a live probe of W8a's
  preview found. A cancelled windowless Save to… leaves nothing behind, Go to keeps its typing in an
  unfocused window, and errors name a remote file as `host:/abs`.
- PR #41 (merged 2026-11-22, 622e843) is not a work unit: a race in the media window's paused-frame
  decoder, found as a one-off Linux test failure. A paused frame is never replaced by an older
  decode.
- PR #42 (merged 2026-11-22, 7544dd8) is not a work unit: highlighting is no longer quadratic on
  long runs of word characters.
- Dates in this block are the merge commits' dates in +0900. In UTC (GitHub's times), all six
  merges fell on 2026-11-21.

## R1 (part 6) — closed 2026-11-22

- merged ED3 ✅ · result: the shared Save As modal and its writer (`gui::save_as`) and F7
  "SaveAs" (`chrome::SAVE_AS_KEY`), wired by no window yet: each of ED4–ED6 adds F7 to its row
  and help line when it wires the modal · closed rows: ED3 · PRs: #44 (485c240)

### Closed rows

| ID | Status | Branch | Worktree | Scope | Owns | After | Gate |
|---|---|---|---|---|---|---|---|
| ED3 | ✅ | `feat/stream-edit-save-as` | `.worktrees/feat-stream-edit-save-as` | 07 §7: Save As modal, F7 in three rows | `src/gui/save_as.rs`, `src/gui/save_as/tests.rs`, `src/gui/mod.rs`, `src/gui/chrome.rs` (F7 entries) | — (browser S1 merged: S1-E, PR #29, 0120d40; S1-A owns `chrome.rs`'s body) | Merged PR #44 (485c240), 2026-11-22 |

### Round section

- ED3 (PR #44, merged 2026-11-22, 485c240), after one review round (Enter on a focused button
  is that button's, the path field holds the keyboard).
- The date in this block is the merge commit's date in +0900.
