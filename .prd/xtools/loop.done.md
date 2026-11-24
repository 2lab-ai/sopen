# xtools — done (append-only)

Loop: ./loop.md · Each closed row or round appends one block at the bottom; nothing above it ever
changes. Rows here are snapshots, frozen when they moved. A later pass is a `re-verified` line in a
loop.md round section; a regression reopens as a new loop.md row with Source `regression:<ID>`.

## R0 (part 1) — VR-M closed 2026-11-24

- Round 0 is still open (XR-T, VR-W, XR-E, X-SPEC); this block moves its first closed row. Result:
  the trace and the draft below. Commit: b6cdc8e (PR #95).

### Closed rows

| ID | Status | What | Who |
|---|---|---|---|
| VR-M | ✅ done 2026-11-24 | Trace of today's Enter viewers, their keys and the text model's encoding assumptions: `../research/viewer-modes.md`; draft of 08's mode and encoding sections: `../research/viewer-modes-08-draft.md` (model separated from screen; its work units provisional until XR-T). The orchestrator re-read six of its citations in the code (kind.rs 157-165 and 238-240: UTF-16 never reaches text; text.rs 1612: lines found by the byte 0x0A; hex.rs 3650-3659: typed text read, not consumed; image.rs 3267: `1` = 100 %; testkit.rs 240-252: `Driver::key` sends no typed text) — all as cited. | research unit |
