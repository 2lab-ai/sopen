# streaming — loop

Status: done (2026-11-23). Target: [`ssot.md`](ssot.md). Contract, shipped: [`../06-streaming.md`](../06-streaming.md)
(viewing) and [`../07-streaming-edit.md`](../07-streaming-edit.md) (editing). Design decisions live
in those documents, not here.

Every work unit (W1–W8b, ED1–ED6, D1) and every experiment (X1–X5) has merged or is done; their
rows, the merge order loop.md gave and the build facts of round 1 are in
[`loop.done.md`](loop.done.md), the last block R1 (part 8). Nothing is open here.

## Next starting point

- A new owner message on streaming or editing reopens this loop: quote it in `ssot.md`, then add a
  round here with new IDs (a regression of a closed unit opens as a new ID with
  `regression:<ID>` in its row).
- The browser workstream's S2-E (02 §8) no longer waits on this one: the viewer units it waited
  for (W4–W7, ED4–ED6) have merged.
- Gate for any change to the streamed viewers or editors, as for every unit:
  `SOPEN_REQUIRE_FFMPEG=1 SOPEN_REQUIRE_SFTP_SERVER=1 just check`, plus the 06 §12 and 07 §12
  scenarios it touches.
