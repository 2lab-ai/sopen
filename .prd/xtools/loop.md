# xtools — loop (open items only)

Status: round 0 (research), opened 2026-11-24. Target: [`ssot.md`](ssot.md). The contracts will be
numbered documents written after the research below (08 onward); design decisions go there, not
here.

## Round 0 — research

| ID | Status | What | Who |
|---|---|---|---|
| XR-E | 🔴 open | The editor of message 21 after XR-T: the PC Tools / Turbo C IDE editor's look and keys from primary references, the F4 layout, the left panel's tabs, the text buffer for local and remote files, saving through 07's safe paths, VS Code / Zed kept reachable. | research unit |
| X-SPEC | 🔴 open | The numbered documents for items 1–5, reviewed by two engines before any code; work units with disjoint files. | orchestrator |

Closed so far: VR-M (the viewers' trace and the draft of 08's view and encoding sections) —
[`loop.done.md`](loop.done.md) R0 part 1; VR-W (the web view) — R0 part 2; XR-T (TUI/GUI architecture) — R0 part 3.

## Build facts

- Gate, per WU and before every merge: `just check` with `SOPEN_REQUIRE_FFMPEG=1
  SOPEN_REQUIRE_SFTP_SERVER=1` (AGENTS.md), plus the Linux gate's two clippy runs (browser loop,
  2026-11-24).
- Size on 2026-11-24 (`wc -l`): `src/gui` 136,614 lines of 245,970 in `src`, tests included.
