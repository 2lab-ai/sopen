# xtools — loop (open items only)

Status: round 1 (code), opened 2026-11-25; round 0's research is closed except XR-E. Target:
[`ssot.md`](ssot.md). Contracts: [`../08-viewer.md`](../08-viewer.md) (view modes and encodings),
[`../09-web-view.md`](../09-web-view.md) (web pages), [`../10-tui.md`](../10-tui.md) (TUI first, the
GUI the same cells). Design decisions go there, not here; each unit's row in those documents' work
unit tables says what it owns and what it waits for.

## Round 0 — research (what is left)

| ID | Status | What | Who |
|---|---|---|---|
| XR-E | 🟡 running | The editor of message 21: the PC Tools / Turbo C IDE editor's look and keys from primary references, the F4 layout, the left panel's tabs, the text buffer for local and remote files, saving through 07's safe paths, VS Code / Zed kept reachable. Result: `../research/editor.md`. | research unit |
| X-SPEC | 🔴 open | The numbered document for item 3 (the editor), after XR-E, reviewed by two engines before any code. Items 2, 4 and 5 are written: 10, 08 and 09, merged 2026-11-24 (#100); item 1 shipped as #96. | orchestrator |

Closed so far: VR-M, VR-W, XR-T — [`loop.done.md`](loop.done.md) R0 parts 1–3.

## Round 1 — code

Open units, in each document's order. Closed units are in [`loop.done.md`](loop.done.md) R1.

| ID | Status | What | Who |
|---|---|---|---|
| XW-kit | 🟡 merging | 10 §17: the test kit's text queries also answer from cell layers (#108) | coder |
| XW-shell | 🟡 running | 10 §17: route B (`create_native`, one event loop per process), F12 / Alt+G / Alt+digits, the start-up failure path; branch `feat/xw-shell` | coder |
| XW-grid | 🔴 open | 10 §17: `panels_ui` on the cell grid, the panes and the viewer at grid rectangles; after XW-kit | coder |
| XW-list | 🔴 open | 10 §17: the list, title, status and F-key rows in cells (ships X1a); after XW-grid. Carried from #108's review: re-measure whether a cell substring can win a lookup over a later exact egui label (the kit merges both in painting order; the spike looked at cells only when the shapes found nothing) | coder |
| XW-dialogs | 🔴 open | 10 §17: the list's dialogs in cells; after XW-list | coder |
| XW-term | 🔴 open | 10 §17: the terminal backend and `sopen --tui` (ships X1b); after XW-list, XW-input, XW-shell and XW-dialogs | coder |
| VM-2 | 🟡 running | 08 §13: the text document per encoding, the bar's encoding and `SOPEN_STATE enc=`; branch `feat/vm2-text-doc` | coder |
| VM-3 | 🔴 open | 08 §13: UTF-16/32 sniffed as text; after VM-2 | coder |
| VM-4 | 🔴 open | 08 §13: CP949 / EUC-KR through `encoding_rs`; after VM-2 | coder |
| VM-7 | 🔴 open | 08 §13: the egui binding of the view keys and the views; after XW-list (it shares `browser.rs`) | coder |
| VM-8 | 🔴 open | 08 §13: the terminal binding. Carried: `src/view_keys.rs`'s module doc now names `term_input::RELEASES` (the translator makes every release); VM-8 builds on that | coder |
| VM-D0 | 🔴 open | 08 §13: 04 §2 and K-R1 brought to the shipped code | orchestrator |
| X-W1 | 🔴 open | 09 §12: the stream-fed page handler prototype on both engines, before WV-C and WV-0 | research unit |
| WV-E1 | 🔴 open | 09 §12: the CLI half of the address window (starts at once) | coder |
| WV-rest | 🔴 open | 09 §12's WV-0, WV-C, WV-A, WV-B, WV-D, WV-E2 and WV-F, in its order after X-W1 | coder |
| X3-hex | 🔴 open | 10 §16's hex step. Carried from #108's review: point the hex tests' `drawn()` at `texts()` once hex paints cells (`galleys()` then holds one-character glyphs), and note that `rects()` will list cell backgrounds and the caret | coder |

## Follow-ups found on the way (not units yet)

| ID | Status | What | Who |
|---|---|---|---|
| F-mp3vbr | 🔴 open | A 22 kHz VBR mp3 cut comes out misaligned; found while fixing ffmpeg 9.1 (#101), recorded, not fixed | coder |
| F-numpad | 🔴 open | The Ctrl+numpad digit alias reads the physical key first (`src/gui/mod.rs`, `fkey_events`) | coder |
| F-keynames | 🔴 open | Key names outside help still mix forms: "Cmd/Ctrl+R" literals, "⌘/Ctrl" hovers, README line 95; 01, 02 and 03 write `Cmd/Ctrl+Z` | coder |
| F-k_d1 | 🔴 open | `gui::browser::tests::s6::k_d1_f8_here_goes_to_the_trash_and_shift_f8_does_not` failed once in a loaded full run; it passes alone 3/3 | coder |

## Build facts

- Gate, per WU and before every merge: `just check` with `SOPEN_REQUIRE_FFMPEG=1
  SOPEN_REQUIRE_SFTP_SERVER=1` (AGENTS.md), plus the Linux gate's two clippy runs (browser loop,
  2026-11-24).
- Size on 2026-11-24 (`wc -l`): `src/gui` 136,614 lines of 245,970 in `src`, tests included.
- Timing tests (v16, v22, s2's parked streams, s6's k_d1) are load-sensitive. On 2026-11-24 three of
  them needed test-only fixes (#101, #102, #103): ffmpeg 9.1 renders a short clip before its first
  progress report, and a dropped stream gives its slot back on a thread of its own. Keep at most
  three compiling units on one machine.
- The Preview workflow's macOS jobs share runners with every PR's macOS check (about 26 minutes
  each). On 2026-11-24 two macOS build jobs never got a runner and were cancelled after exactly 15
  minutes while PR checks held the runners; where the 15-minute cutoff comes from is not known. A
  rerun of just those jobs passed. Merge, let the Preview finish, then push the next PRs.
- unicode-width is pinned to `=0.2.2` (#104's review fix): ratatui-core 0.1.2's `cell_width` adds 1
  per halfwidth sound mark, which 0.2.3 already counts. Lift the pin with a ratatui-core that drops
  its compensation.
