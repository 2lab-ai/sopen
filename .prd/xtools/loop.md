# xtools — loop (open items only)

Status: round 0 (research), opened 2026-11-24. Target: [`ssot.md`](ssot.md). The contracts will be
numbered documents written after the research below (08 onward); design decisions go there, not
here.

## Round 0 — research

| ID | Status | What | Who |
|---|---|---|---|
| XR-T | 🔴 open | **Gates XE and XM.** The architecture of message 23 with a running prototype: how xtools becomes a TUI first and a GUI that is the same screen 1:1 (one cell-grid UI with a terminal and a GUI backend, or another route), how much of today's GUI is already cell-aligned and what moves, the TUI→GUI switch (same process with a display; across ssh to the target), terminal graphics (kitty, iTerm2, sixel; which terminals the owner uses support which), video in a terminal, and the order of a migration. Output: `../research/tui-gui.md`. | research unit |
| VR-W | 🟡 running | The OS's web engine in xtools on macOS and Linux Wayland, loaded at runtime (sopen must still start without it), remote pages and their resources, a page's security, XML in each engine. Output: `../research/web-view.md`. | research unit |
| VR-M | ✅ done 2026-11-24 | Trace of today's Enter viewers, their keys and the text model's encoding assumptions: `../research/viewer-modes.md`; draft of 08's mode and encoding sections: `../research/viewer-modes-08-draft.md` (model separated from screen; its work units provisional until XR-T). The orchestrator re-read six of its citations in the code (kind.rs 157-165 and 238-240: UTF-16 never reaches text; text.rs 1612: lines found by the byte 0x0A; hex.rs 3650-3659: typed text read, not consumed; image.rs 3267: `1` = 100 %; testkit.rs 240-252: `Driver::key` sends no typed text) — all as cited. | research unit |
| XR-E | 🔴 open | The editor of message 21 after XR-T: the PC Tools / Turbo C IDE editor's look and keys from primary references, the F4 layout, the left panel's tabs, the text buffer for local and remote files, saving through 07's safe paths, VS Code / Zed kept reachable. | research unit |
| X-SPEC | 🔴 open | The numbered documents for items 1–5, reviewed by two engines before any code; work units with disjoint files. | orchestrator |

## Build facts

- Gate, per WU and before every merge: `just check` with `SOPEN_REQUIRE_FFMPEG=1
  SOPEN_REQUIRE_SFTP_SERVER=1` (AGENTS.md), plus the Linux gate's two clippy runs (browser loop,
  2026-11-24).
- Size on 2026-11-24 (`wc -l`): `src/gui` 136,614 lines of 245,970 in `src`, tests included.
