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
  - **VM-4** (#121): CP949 / EUC-KR through `encoding_rs`.
    - Character starts are proven by a sync byte, a line start within REACH, or a lazy phase checkpoint; with none, rows show ░, never a wrong glyph.
    - The guess (D39 (a)–(e)) is a Korean-first prior; real Chinese prose is stopped by (b)/(d). An ASCII-share criterion was tried and deleted.
    - encoding_rs and its WHATWG data are in THIRD-PARTY-NOTICES §6a; the stripped binary grows by 166 KB.
- **Web (09 §12).**
  - **WV-E1** (#117): `--browser`.
  - **WV-0** (#118): the component core and the scheme contract, which is head, exact-length body or fail; a stop at any point; head-less stops matched by URL and Range.
- **XW-dialogs** (#125): the list's dialogs and the F2 field in cells.
  - Fields keep egui's editor, laid out on cells, so clicks, IME and scrolling follow the cells.
  - A dialog stays between the title and status rows, at most 76 columns wide, with a black-cell shadow.
- **WV-C** (#124): the page server.
  - Paths: steps refuse `..` with no filesystem call, then realpath and a root check; a folder without its `/` is sent to `NAME/`.
  - Freshness: one checked handle per view plus a block cache; a fault forgets the file.
  - Range per RFC 9110; the charset comes from WHATWG's prescan or `pick`.
  - The budget counts bytes read; the local reader is FIFO-safe. Pipelining is WV-C2.
- **ED-0 + ED-buf** (#122): the editor's piece table.
  - The original file is shared and never copied; add blocks never move; the line index is LF counts per 4 KiB.
  - A CR LF pair is never split; undo is stored as pieces.
  - `restore` rebuilds a document for the journal; a piece from another buffer is `Stale`.
  - EB-1's memory bound counts what the file and buffer add.
- **Reviews.** Two engines per unit, then one fix-check; the orchestrator verified the last fixes at their lines.
- **Prereleases, installed on both hosts:**
  - preview-2026-11-24-2256-c9e2949510d2 (WV-E1);
  - preview-2026-11-25-0010-d7b90ce8cd82 (XW-list, WV-0);
  - preview-2026-11-25-0038-ddafaca53216 (VM-3);
  - preview-2026-11-25-0216-602a9e7157db (VM-4);
  - preview-2026-11-25-0243-380ec0c77e4b (ED-buf);
  - preview-2026-11-25-0340-f58623c45457 (XW-dialogs, WV-C).

### Closed rows

| ID | Status | What | Who |
|---|---|---|---|
| XW-kit | ✅ done 2026-11-25 | 10 §17: the test kit's text queries also answer from cell layers (#108) | coder |
| XW-shell | ✅ done 2026-11-25 | 10 §17: route B, F12 / Alt+G / Alt+digits, the start-up failure path (#110) | coder |
| XW-grid | ✅ done 2026-11-25 | 10 §17: the panes and the viewer at grid rectangles (#113) | coder |
| XW-list | ✅ done 2026-11-25 | 10 §17: the list, title, status and F-key rows in cells; ships X1a (#119) | coder |
| VM-2 | ✅ done 2026-11-25 | 08 §13: the text document per encoding, the bar's encoding, `SOPEN_STATE enc=` (#114) | coder |
| VM-3 | ✅ done 2026-11-25 | 08 §13: UTF-16/32 sniffed as text; corroborated signatures refuse the guess; the bar's encoding never cut (#120) | coder |
| VM-4 | ✅ done 2026-11-25 | 08 §13: CP949 / EUC-KR through `encoding_rs`: decoder, sync bytes, grid cut, lazy phase checkpoints, the default's step 3 (#121) | coder |
| WV-E1 | ✅ done 2026-11-25 | 09 §12: the CLI half of the address window, `--browser` (#117) | coder |
| WV-0 | ✅ done 2026-11-25 | 09 §12: the component core, the scheme contract, the shell's part (#118) | coder |
| XW-dialogs | ✅ done 2026-11-25 | 10 §17: the list's dialogs and the F2 field in cells (#125) | coder |
| WV-C | ✅ done 2026-11-25 | 09 §12: the page server (#124) | coder |
| ED-buf | ✅ done 2026-11-25 | 12 §16: ED-0 (folded in) and ED-buf, the piece table (#122) | coder |
| F-k_d1 | ✅ done 2026-11-25 | s6's k_d1: the Trash check runs on a worker; the test waits for it (#111) | coder |

## R1 (part 5) — the macOS web engine, encode, the views and the TUI, closed 2026-11-25

- **WV-A** (#126): the macOS engine — WKWebView in the window, a non-persistent data store per view, the rule list in a private store, link previews off, download menu items dropped; the close containment fix and Objective-C exceptions caught.
- **ED-enc** (#127): `Encoding::encode`, refusing a character the encoding cannot hold with the character, its offset and the encoding (`Refused`).
- **VM-7** (#128) answers owner message 20 in the window.
  - Keys 1/2/3 and 9 View switch the views; a view with unsaved edits asks first, and a running Save is waited for.
  - Hex for every file opens viewing (D35); F8 picks the text encoding in cells.
  - Each window counts its landed Saves, read before any switch; a hidden media view stops polling and parks its waveform.
  - What VM-7 did not build is VM-7b (08 §13).
- **XW-term** (#129) ships **X1b**: `sopen --tui`.
  - egui's cell layers painted to the terminal, diffed; 24-bit colour or the 256-colour table; kitty keyboard flag 1, mouse, OSC 52 (capped at 256 KiB).
  - The terminal is restored on close, panic and signals (exit 128+n); a real hang-up ends the run (it spun at about 92 % CPU before the review); an idle TUI makes no passes.
  - F12 or the `[GUI]`/`[TUI]` button switches in process, keeping dialogs, focus and textures; the button works over a modal.
- **Reviews.** Two engines per unit, then one fix-check; the orchestrator verified the last fixes at their lines. VM-7's fix-check found a refusal lost when the viewer leaves; XW-term's found the mode button's press surviving the switch.
- **Prereleases, installed on both hosts:**
  - preview-2026-11-25-0551-b01e688ce90b (WV-A);
  - preview-2026-11-25-0623-cb69ae806e10 (ED-enc);
  - preview-2026-11-25-0708-d472fa49f50e (VM-7);
  - preview-2026-11-25-0746-6a86f31b0541 (XW-term).

### Closed rows

| ID | Status | What | Who |
|---|---|---|---|
| WV-A | ✅ done 2026-11-25 | 09 §12: the macOS engine (#126) | coder |
| ED-enc | ✅ done 2026-11-25 | 12 §16: `Encoding::encode` and its refusal (#127) | coder |
| VM-7 | ✅ done 2026-11-25 | 08 §13: the egui binding of the view keys and the views (1/2/3, 9 View, F8 encodings) (#128) | coder |
| XW-term | ✅ done 2026-11-25 | 10 §17: the terminal backend and `sopen --tui`; ships X1b (#129) | coder |

## R1 (part 6) — pages, the help in cells, the editor's model, shared streams and receipts, closed 2026-11-25

- **WV-D** (#131) answers owner message 18: `.html`/`.htm` and styled `.xml` render as pages in the preview and the viewer (scripts off, outside requests blocked and counted, the folder as root); Page·Hex·Text; the source with the reason when no engine. #135 made its hand-over tests deterministic.
- **XW-help2** (#132): F1's help drawn in cells in both backends, shown in the terminal, one `⌘` face in every window.
- **ED-ops** (#133) and **ED-save** (#136): the editor's commands over the buffer (bounded work on one huge line) and its save path (§9.1's steps through `replace_guarded`: the local exchange, the SFTP link guard, reconcile by identity; the SFTP window before the rename stated as a limit).
- **VM-7b** (#137): one remote file's views share its stream and one save session, given back only once a hidden view's work ended; a viewer back within 06 §6.
- **XW-receipts** (#138): `SOPEN_CELLS` writes either backend's screen as text; TS-6 — an ambiguous-wide terminal gets §10's chrome stand-ins in one start-up round trip.
- **X3-hex** (#139): the hex viewer in cells in both backends — in the TUI the hex view where a hand-off box stood; the CP437 column's stand-ins through §10's table; the title row keeps the mode button's width (TS-1).
- **Docs:** the ledger (#130); X2 and X3 cut into units (#134).
- **The Linux host** was offline from about 19:50 to 22:40 KST (11-25); PRs' ubuntu CI stood in as the Linux gate and prereleases went to the Mac only until it returned (preview-2026-11-25-1338-d8733a5ee00a installed on both).
- **Reviews.** Two engines per unit, then one fix-check; the orchestrator verified the last fixes at their lines. Two CI-only failures were test races (a free-space-dependent oracle; a Save read after it landed), fixed in the tests.
- **Prereleases:**
  - preview-2026-11-25-0939-e59eda3fc46a (WV-D);
  - preview-2026-11-25-1008-623d6fc33266 (XW-help2);
  - preview-2026-11-25-1056-8e1391e16ad0 (ED-ops);
  - preview-2026-11-25-1148-c14204ebfe3e (ED-save);
  - preview-2026-11-25-1338-d8733a5ee00a (XW-receipts);
  - preview-2026-11-25-1406-ca5fee3974e1 (VM-7b);
  - preview-2026-11-25-1751-d98f95eb71eb (X3-hex, built at #140's commit after two cancelled macOS builds).

### Closed rows

| ID | Status | What | Who |
|---|---|---|---|
| WV-D | ✅ done 2026-11-25 | 09 §12: the page view in the browser (#131, #135) | coder |
| XW-help2 | ✅ done 2026-11-25 | F1's help in cells in both backends (#132) | coder |
| ED-ops | ✅ done 2026-11-25 | 12 §16: the editor's commands over the buffer (#133) | coder |
| ED-save | ✅ done 2026-11-25 | 12 §16: the editor's save path without a screen (#136) | coder |
| VM-7b | ✅ done 2026-11-25 | 08 §13: views of one remote file share its stream and one save session (#137) | coder |
| XW-receipts | ✅ done 2026-11-25 | 10 §17's X3: `SOPEN_CELLS` and TS-6 (#138) | coder |
| X3-hex | ✅ done 2026-11-26 | 10 §17's X3: the hex viewer in cells (#139) | coder |

## R1 (part 7) — the binary dialog, the text viewer, `--gui`, the switch's state and the views in a terminal, closed 2026-11-26

Five units merged and shipped one by one, each prerelease installed on the Mac and the Linux host.

- **X3-binary** (#143): the binary dialog in cells — the file's line, `[ Hex ]` and `[ Save to… ]`, the path field, the progress and the clash note; the window fitted to its rows.
- **X3-text** (#145): the text viewer and its preview in cells in both backends, wrapped and scrolled sideways by cells; in the TUI the text view where a hand-off box stood. CP437 stand-ins apply to chrome and control-character glyphs only: content keeps its letters (TS-1, TS-4, TS-6).
- **X2-gui** (#144): `--gui` — the window here, or with no display §7.1's line and exit 1 before anything is read; refused beside `--app` and `--tui`. A forward passes it only when the owner gave it (the automatic rule waits behind `AUTO_GUI` for XF); an older HOST that refuses it at parse time gets the run once more without it. HOST's bytes pass as they are read: a prefix on its lines held a terminal query once (the fix-check's find), so there is none.
- **X2-state** (#146): the switch's state, one JSON line, `"v": 1`, taken from one browser and given to a new one; checked as untrusted input wherever it is read or given; paths rewritten out and back; a dialog reopened only on its side, folder and entries, the pass that reopens it dropping its input. Nothing sends it yet: X2-host and X2-switch will.
- **VM-8** (#147): 08's views in the terminal — the window's slots with pages not drawn, so `index.html` opens on its source (3) in the TUI and on its page (1) in the window; a preview made under one backend is made again after a switch; the view keys through the translator.
- **Receipts** on the installed builds: the hex view and the text viewer in `--tui` (Hangul in two cells); the binary dialog's cells with a Hangul file name; `--gui --tui` exits 2; `--gui` without a display exits 1 with its line on the Linux host; `index.html` in `--tui` opens on its source, the bar reading `1 Page · 2 Hex · 3 Text`.
- **Reviews.** Two engines per unit, then one fix-check; the orchestrator verified the last fixes at their lines. CI-only failures were timing tests outside the units (F-timing), passed on a rerun of the failed job.
- **Prereleases:**
  - preview-2026-11-26-0456-3adfa7be05c1 (X3-binary);
  - preview-2026-11-26-0646-ca4b291f9d8a (X3-text);
  - preview-2026-11-26-0852-fa141481f257 (X2-gui);
  - preview-2026-11-26-0954-8e0c880d8956 (X2-state);
  - preview-2026-11-26-1027-3b186f4fca4b (VM-8).

### Closed rows

| ID | Status | What | Who |
|---|---|---|---|
| X3-binary | ✅ done 2026-11-26 | 10 §17's X3: the binary dialog in cells (#143) | coder |
| X3-text | ✅ done 2026-11-26 | 10 §17's X3: the text viewer and its preview in cells (#145) | coder |
| X2-gui | ✅ done 2026-11-26 | 10 §17's X2: `--gui` and its forward, with an older HOST's retry (#144) | coder |
| X2-state | ✅ done 2026-11-26 | 10 §17's X2: the switch's state, `"v": 1` (#146) | coder |
| VM-8 | ✅ done 2026-11-26 | 10 §17's X3 (08 §13's row): 08's views in the terminal (#147) | coder |

