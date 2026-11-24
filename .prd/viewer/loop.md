# viewer — loop (open items only)

Status: round 0 (research), opened 2026-11-24. Target: [`ssot.md`](ssot.md). The contract will be
`../08-viewer.md`, written after the research below; design decisions go there, not here.

## Open

| ID | Status | What | Who |
|---|---|---|---|
| VR-W | 🟡 running | Research with a running prototype: the OS's web engine inside sopen on macOS and on Linux Wayland, loading it at runtime (sopen must still start without it), remote pages and their resources, the security of a page (scoped root, navigation, no bridge), what each engine shows for XML. Output: `../research/web-view.md`. | research unit |
| VR-M | 🔴 open | Trace of today's Enter viewers (which window a kind opens, each window's keys, where 1/2/3 collide — the hex editor types digits) and of the text model's encoding assumptions; draft of 08's mode and encoding sections. | research unit |
| V-SPEC | 🔴 open | `../08-viewer.md`: modes 1/2/3, encodings, the web view, the address window (#82's contract changes), work units. Reviewed by two engines before any code. | orchestrator |

## Build facts

- Gate, per WU and before every merge: `just check` with `SOPEN_REQUIRE_FFMPEG=1
  SOPEN_REQUIRE_SFTP_SERVER=1` (AGENTS.md), plus the Linux gate's two clippy runs (browser loop,
  2026-11-24).
