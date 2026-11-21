# streaming — done (append-only)

Loop: [`loop.md`](loop.md). Each round close appends one block at the bottom; nothing above it ever
changes. Rows here are snapshots taken when they were moved.

## R1 (part) — closed 2026-11-21

- measured W1 ✅ ED2 ✅ X2 ✅ X4 ✅ X5 ✅ · result: the SFTP read transport and the edit journal merged
  and deployed (preview 4a282e4e7f2d on both hosts); three experiments written into 06 and 07 ·
  closed rows: W1, ED2, X2, X4, X5 · PRs: #24, #25, #27

### Closed rows

| ID | Status | Branch | Worktree | Scope | Owns | After | Gate |
|---|---|---|---|---|---|---|---|
| X2 | ✅ | — (scratch) | — | Server `copy-data` on real disks: 4 GiB on the Linux laptop's disk and on APFS, cold cache, 64 MiB requests, fsync every 2 GiB; the 6.3 s figure was tmpfs (07 §4.2). About 30 min. | none | before ED1 | Done (X2): 4 GiB cold copy-data 2.2 GB/s laptop disk, 4.5 GB/s Mac APFS; 24 GiB 16–17 s / 5–14 s; numbers + fsync guard in 07 §1, §4.2 (PR #27) |
| X4 | ✅ | — (scratch) | — | The JPEG walk to SOF and dimensions read from it, on a JPEG cut right after SOF and on about 20 camera-style JPEGs with large MPF/XMP segments (06 §7.3). About 30 min. | none | before W6 | Done (X4): walk-to-SOF right on 894 real + 40 built JPEGs; ≤ 42,662 bytes to SOF on real files; rules in 06 §7.3 (PR #27) |
| X5 | ✅ | — (scratch) | — | mp3 `+fastseek` precision: a VBR `lavfi` sine whose frequency steps every second; seek to 480 s with and without the flag and compare the decoded second (06 §7.4). About 1 hour. | none | before W7 | Done (X5): +fastseek CBR within 0.026 s, Xing VBR up to 4.3 s (10 min) / 21.8 s (1 h), no-header VBR up to 161 s → fastseek only for LAME `Info` (CBR), 06 §7.4 (PR #27) |
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
