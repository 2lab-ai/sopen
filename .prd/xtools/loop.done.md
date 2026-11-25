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

## R0 (part 2) — VR-W closed 2026-11-24

- Result: `../research/web-view.md`. Both platforms show the OS's own engine inside a rectangle of
  sopen's window, with no switch of the app's backend. macOS uses a WKWebView subview, plus a key
  monitor that gives Esc, the F-keys and Cmd+W/Q back, and the view is hidden under popups. Linux
  Wayland, where no child view is possible (wry accepts only X11 handles), uses WebKitGTK 4.1
  loaded at runtime and drawn offscreen into an egui texture. sopen still starts without it (the
  prototype's `ldd`: libc, libm, libgcc_s only). Remote pages load through a custom scheme.
  Plain XML and RSS render as run-on text in both engines; XSLT-styled XML renders.
- The orchestrator read the receipts (Linux `egui1-2-clicked.png`: CSS and a relative image
  applied, script not run, checkbox clicked; `egui-missing-unavailable.png`: the fallback) and
  wry 0.57.3 `src/webkitgtk/mod.rs:150-153` (only `RawWindowHandle::Xlib`). All as reported.
  Not measured: a real scaled display, real pointer and Korean IME input on Linux, and video on
  Linux (the laptop has no `gst-plugins-good`).

### Closed rows

| ID | Status | What | Who |
|---|---|---|---|
| VR-W | ✅ done 2026-11-24 | The OS's web engine in xtools on macOS and Linux Wayland, loaded at runtime (sopen must still start without it), remote pages and their resources, a page's security, XML in each engine. Output: `../research/web-view.md`. | research unit |

## R0 (part 3) — XR-T closed 2026-11-24

- Result: `../research/tui-gui.md`. Recommended architecture: **(A) one cell-grid UI core with two
  backends**, a terminal backend and a GUI backend that paints the same cells (the VGA font, glyph by
  glyph per cell) and hosts pixel regions at cell rectangles (images, video, native views).
  - Killed: (B) a separate TUI kept 1:1 with today's egui GUI by tests (the GUI must move onto the grid
    first anyway), and (D) the window as a terminal emulator (no native web view; kitty pictures over
    ssh at 6–8 fps).
  - (C) egui's shapes projected onto cells stays as a lint for screens not yet moved: the F7 dialog had
    158 glyphs off the grid, the hex window 1,337.
  - Measured: the 1:1 proof, the TUI→GUI switch in one process and across machines (the state sent over
    ssh, 1.1 s end to end), and terminal-picture rates.
  - The migration order starts with the file list in cells (`--tui`).
- The orchestrator recomputed the 1:1 proof from the dumps. The file list plus F7 with Hangul typed
  covers 3,000 cells. Under vt100, tmux and Linux, against the GUI backend's painted cells:
  - 0 background differences;
  - 0 foreground differences on painted glyphs;
  - 1 character difference, the `[GUI]`/`[TUI]` mode button, by design.
- It also read the prototype's GUI screenshot: PC Shell's look, with Hangul names on whole cells.
- XR-T also found that the owner's laptop terminals are kitty and herdr-app, not Konsole. The
  orchestrator confirmed this with `ps` (see the Tension added to `../research/fkeys-linux.md` §5).

### Closed rows

| ID | Status | What | Who |
|---|---|---|---|
| XR-T | ✅ done 2026-11-24 | The architecture of message 23 with a running prototype: TUI first and a GUI that is the same screen 1:1, the gap in today's GUI, the TUI→GUI switch, terminal graphics, video in a terminal, the migration order. Output: `../research/tui-gui.md`. | research unit |

## R1 (part 1) — the first code units, closed 2026-11-25

- Merged on 2026-11-24 and 2026-11-25. Each unit had one review round, and every must-fix was
  fixed before merge. CI passed on macOS and Linux for each merge commit's PR head.
  - Model units of 08: VM-6 the view keys (#97), VM-5 the view set (#98), VM-1 the encoding core
    (#99).
  - TUI floor of 10: XW-0 the crates (#104), XW-cells the cell core (#105), XW-input the terminal
    translator (#106), XW-help one help text (#107).
- Review fixes that changed a contract:
  - **#104.** unicode-width is pinned to 0.2.2. ratatui-core's halfwidth sound-mark compensation
    double-counts on 0.2.3, so `ﾊﾟ` took 3 cells against a terminal's 2.
  - **#106.** A terminal paste gives V's release after `Paste`, as a window's Cmd/Ctrl+V does.
  - **#107.** Help lists only the rows this machine can press. Key names stay neutral (`Ctrl/⌘+…`).
- Prerelease with VM-1, VM-5 and VM-6 and the specs: `preview-2026-11-24-1548-e974df9fc18e`,
  installed on both machines. The TUI floor ships with XW-kit's prerelease.

### Closed rows

| ID | Status | What | Who |
|---|---|---|---|
| VM-1 | ✅ done 2026-11-25 | 08 §13: the encoding core (#99) | coder |
| VM-5 | ✅ done 2026-11-24 | 08 §13: the view set (#98) | coder |
| VM-6 | ✅ done 2026-11-24 | 08 §13: the view keys (#97) | coder |
| XW-0 | ✅ done 2026-11-25 | 10 §17: ratatui-core, crossterm, unicode-width; the empty modules (#104) | coder |
| XW-cells | ✅ done 2026-11-25 | 10 §17: the cell core shared by the window and the terminal (#105) | coder |
| XW-input | ✅ done 2026-11-25 | 10 §17: crossterm events become egui events, as the window sends them (#106) | coder |
| XW-help | ✅ done 2026-11-25 | 10 §17: one help text on every machine, `Ctrl/⌘+…` (#107) | coder |

## R1 (part 2) — X-W1 closed 2026-11-25

- **Result.** `../09-web-view.md` §3.1, in the block "X-W1 result (2026-11-25)". Prototypes ran outside the repo on both engines: macOS WKWebView through objc2-web-kit, and Linux WebKitGTK 2.52.6 offscreen in an isolated headless session. Both used a real-time fake link at 50 ms RTT and 2 MB/s.
- **Held on both engines:**
  - a handler answering from a worker thread;
  - late pieces taken on the engine's thread;
  - no call after a stop, which macOS throws on;
  - turns that keep a later CSS request ahead of a long response.
- **A run of ranges on one handle held only with a spec change.** Media needs one checked handle per path and view, plus a 256 KiB block cache. A video played in 428–477 ms; re-checking every range never played within 60 s.
- **RTT counts.** About 4 and about 2 RTT, as §3.1 said. The first byte takes 5 RTT, and a page with N resources costs (N+2)×5 RTT in series.
- **What the units inherit (written in §3.1):**
  - WV-C: per-view handles, the block cache, checks plus the first piece in one turn. Batching is still to decide.
  - WV-A: the tombstone, and the stop that can come after the last byte.
  - WV-B: a GInputStream subclass, and a request stopped before its head. Linux sends no cancel then; the only signal is `resource-load-started`/`failed`.

### Closed rows

| ID | Status | What | Who |
|---|---|---|---|
| X-W1 | ✅ done 2026-11-25 | 09 §12: the stream-fed page handler prototype on both engines, before WV-C and WV-0 | research unit |

## R1 (part 3) — XR-E and X-SPEC closed 2026-11-25

- **XR-E.** Research in `../research/editor.md`, from primary references:
  - **Look:** PC Tools 6 Desktop / Notepads, with Turbo C++ 3.0's edit window.
  - **Buffer:** a piece table over the file's own bytes, measured against ropey and against a String.
  - **Highlighting:** syntect, already linked (0 bytes added), against tree-sitter.
  - **Keys:** played through the terminal translator.
  - **Layout:** the F4 layout drawn as cell mock-ups.
- **X-SPEC.** Item 3, the editor, is written as `../12-editor.md` (E1–E40).
- **Review.** Two engines.
  - **Round 1 was FIX.** 6 GPT findings + 12 Claude must-fixes. Orchestrator decisions:
    - E31: in a terminal, `^S` is confirmed only by a real F2 or a click.
    - E32: the guarded replace — a local exchange-and-verify, and a remote check before the rename plus a link check after it.
    - E33: `sopen file.txt` ends with the editor.
  - **The fix-check was FIX.** 4 + 7 gaps in the new mechanisms' failure states: a failed swap-back, an interrupted remote check, journal generations and durability, the CRLF boundary, the column's delete dialog, "Shift known" expiring, and the prompt lines.
  - **The orchestrator checked those fixes at their lines.**
- **Items 2, 4 and 5 were written earlier** (10, 08, 09; #100). With this, every item of `ssot.md` has its contract.

### Closed rows

| ID | Status | What | Who |
|---|---|---|---|
| XR-E | ✅ done 2026-11-25 | The editor of message 21: the PC Tools / Turbo C IDE editor's look and keys from primary references, the F4 layout, the left panel's tabs, the text buffer for local and remote files, saving through 07's safe paths, VS Code / Zed kept reachable. Result: `../research/editor.md`. | research unit |
| X-SPEC | ✅ done 2026-11-25 | The numbered document for item 3 (the editor), after XR-E, reviewed by two engines before any code. Items 2, 4 and 5 are written: 10, 08 and 09, merged 2026-11-24 (#100); item 1 shipped as #96. | orchestrator |

## R1 (part 4) — the window on cells, encodings and the web core, closed 2026-11-25

- **The window on cells (10 §17).**
  - **XW-kit** (#108): the test kit's text queries answer from cell layers too.
  - **XW-shell** (#110): route B (`create_native`, one event loop per process), F12 / Alt+G / Alt+digits, and the start-up failure path.
  - **XW-grid** (#113): the panes and the viewer sit at grid rectangles.
  - **XW-list** (#119) ships **X1a**.
    - The list, title, status and F-key rows are drawn in cells; Hangul takes two cells.
    - The kit's exact queries answer only from a whole cell text. Eleven s3/s4 lookups that had passed on the status row now hit their labels.
    - Release frame cost: 0.71 ms against 0.23 ms (F-list-perf).
- **Encodings (08 §13).**
  - **VM-2** (#114): the text document per encoding.
  - **VM-3** (#120): UTF-16/32 files with no known extension route to text.
    - Corroborated binary signatures refuse the pattern's guess. MZ needs a PE header or the DOS stub; ID3 needs its version and flags.
    - The text bar's encoding is never cut.
    - 09's page charset follows `pick`.
- **Web (09 §12).**
  - **WV-E1** (#117): `--browser`.
  - **WV-0** (#118): the component core and the scheme contract, which is head, exact-length body or fail; a stop at any point; head-less stops matched by URL and Range.
- **Reviews.** Two engines per unit, then one fix-check; the orchestrator verified the last fixes at their lines.
- **Prereleases, installed on both hosts:**
  - preview-2026-11-24-2256-c9e2949510d2 (WV-E1);
  - preview-2026-11-25-0010-d7b90ce8cd82 (XW-list, WV-0);
  - the VM-3 one, after #120.

### Closed rows

| ID | Status | What | Who |
|---|---|---|---|
| XW-kit | ✅ done 2026-11-25 | 10 §17: the test kit's text queries also answer from cell layers (#108) | coder |
| XW-shell | ✅ done 2026-11-25 | 10 §17: route B, F12 / Alt+G / Alt+digits, the start-up failure path (#110) | coder |
| XW-grid | ✅ done 2026-11-25 | 10 §17: the panes and the viewer at grid rectangles (#113) | coder |
| XW-list | ✅ done 2026-11-25 | 10 §17: the list, title, status and F-key rows in cells; ships X1a (#119) | coder |
| VM-2 | ✅ done 2026-11-25 | 08 §13: the text document per encoding, the bar's encoding, `SOPEN_STATE enc=` (#114) | coder |
| VM-3 | ✅ done 2026-11-25 | 08 §13: UTF-16/32 sniffed as text; corroborated signatures refuse the guess; the bar's encoding never cut (#120) | coder |
| WV-E1 | ✅ done 2026-11-25 | 09 §12: the CLI half of the address window, `--browser` (#117) | coder |
| WV-0 | ✅ done 2026-11-25 | 09 §12: the component core, the scheme contract, the shell's part (#118) | coder |
| F-k_d1 | ✅ done 2026-11-25 | s6's k_d1: the Trash check runs on a worker; the test waits for it (#111) | coder |
