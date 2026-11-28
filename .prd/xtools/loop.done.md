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

## R1 (part 8) — the hex window's questions, the preview side in cells and HOST's half of the switch, closed 2026-11-26

Three units merged and shipped one by one. The Linux host was back for #149's prerelease; from then on each prerelease is installed on the Mac and the Linux host.

- **X3-questions** (#149): the hex window's questions in cells — the close prompt, the first Save above 256 MiB, Write in place and Save As (one cell modal shared by the hex, image and media windows) — and the rows below its grid, with Tab into their buttons. Raising the close prompt starts its journal write, and the window is not ready until it lands.
- **X3-preview** (#151): the browser's preview side in cells — its frame titled with the file, the read's progress, `…` and why there is no preview. A text or hex preview paints its own cells inside it; a picture, a video, a sound, the info card and a page in a window keep egui's frame until X6, each one hand-off box in a terminal. The frame does not change while a file is prepared: a ready preview keeps its own, and before that the file's last ready preview or its name decides.
- **X2-host** (#150): `sopen --here --gui --state-stdin -- FOLDER`, HOST's half of the switch — the state from the first line of input, the window at that state, `sopen-xtools-state <json>` on F12, `[TUI]`, the close button or Cmd/Ctrl+Q and +W; exit 2 before any window for a state it cannot take. When the input ends with nothing to lose the window closes; otherwise HOST says `sopen-xtools-kept` and keeps the window. HOST's fds 1 and 2 go through relays, so nothing sopen writes sees EPIPE. §7.3's [추정] measured: a command run by `ssh -T` gets no SIGHUP when the local ssh is killed.
- **A merge of main broke three tests without a conflict.** #149 drew the close prompt in cells, so X2-host's `switch_host` tests found no `Cancel` to click; they now let the window settle first (`click_prompt`). The module gate runs again after every merge of main, before the push.
- **Receipts:** the hex view's Save As and close prompt, the same cells in the window and `--tui`; a Hangul `.txt`, a `.png` and a binary under the bar, the window's `SOPEN_CELLS` and the TUI's identical below the head line; `--state-stdin`'s F12 state cue, the kept cue with the window open at 25 s, and exit 0 with its output dead from the start.
- **Reviews.** Two engines per unit, then one fix-check; the orchestrator verified the last fixes at their lines. CI-only failures were timing tests outside the units (F-timing), passed on a rerun of the failed job.
- **Prereleases:**
  - preview-2026-11-26-1158-9f716b30e3a3 (the ledger, #148; the Mac only, the Linux host offline);
  - preview-2026-11-26-1346-9a6f00a05959 (X3-questions);
  - preview-2026-11-26-1552-2ac751ea2847 (X3-preview);
  - preview-2026-11-26-1726-01751df2b2d0 (X2-host, built after one cancelled run).

### Closed rows

| ID | Status | What | Who |
|---|---|---|---|
| X3-questions | ✅ done 2026-11-26 | 10 §17's X3: the hex window's questions and rows, and Save As, in cells (#149) | coder |
| X3-preview | ✅ done 2026-11-26 | 10 §17's X3: the preview side's frame and progress line in cells (#151) | coder |
| X2-host | ✅ done 2026-11-26 | 10 §17's X2: HOST's half, `--state-stdin` and its two cues (#150) | coder |

## R1 (part 9) — the switch across machines, and the editor's screen and dialogs, closed 2026-11-27

Three units merged and shipped one by one, each prerelease installed on the Mac and the Linux host. With X2-switch, **X2 ships**: F12 in `sopen --tui` with a target opens the window on HOST, and its close brings the TUI back with HOST's state.

- **X2-switch** (#154): the TUI's half of 10 §7.3.
  - **The run:** HOST's half goes over the forward's ssh (`-T`, `BatchMode=yes`, `SSH_ASKPASS_REQUIRE=never`) with the state on its input. Meanwhile the terminal is dimmed under §7.3's line with only Ctrl+C read.
  - **Coming back:** HOST's cue gives its state to a fresh browser; when HOST keeps its window, our ssh client ends at once; each outcome is its line. What HOST says is bounded and matched exactly.
  - **Before the switch:** it waits for every running write, including one a closed viewer left. Unsaved edits ask (Save, Save As, Discard, Cancel); Discard drops them only once HOST's state is back. An open git dialog or plan asks too.
  - **HOST's side:** it now hands back the viewer after its close prompt is answered, and the windows' Discard is one path (`Window::discard`).
- **ED-screen** (#153): 12 §4 in cells.
  - The frame, gutter, syntect rows, caret, block, scroll bar and the three rows.
  - One highlighter for the viewer and the editor through `LineSource`. The editor's worker reads a snapshot; a one-slot mailbox; no reply painted on a newer document; kept states every 64 lines.
  - EH-3: 18.73 MiB over a 16 MiB Rust file; its 16 MiB run is a hand run.
  - Release frame cost: 0.34 ms.
- **ED-dialogs** (#155): the editor's dialogs in cells.
  - **E6:** every prompt opens on Cancel; Enter presses the focused button; letters answer nothing.
  - **The boxes:** the close prompt, the conflict dialog, Ctrl/⌘+R's question, the encoding question and the F10 menus. Find, Replace's question and Go to line are on the status row.
  - **The exit protocol and E24** (no edits while a Save runs).
  - **The binding contract** for ED-layout: inputs to `press()` until a dialog is up, then `dialogs_ui` reads only the rest.
- **Receipts:**
  - **X2-switch, live across machines** (the TUI on the Linux host in a pty, the window on the Mac):
    - Branch builds: F12 at 4.07 s, the dimmed screen at 4.60 s; a folder made in HOST's window by F7 was listed in the TUI at 8.73 s; a second run shot HOST's window.
    - The installed builds after the release: the dimmed screen 0.5 s after F12, back with HOST's folder at 2.1 s.
  - **ED-dialogs:** the close prompt and the File menu over Hangul text at 80×25 are the same cell by cell in the window and the terminal.
- **Reviews.**
  - Two engines per unit (GPT by area, Claude whole), then one fix-check; the orchestrator verified the last fixes at their lines.
  - The editor units' reviewers found the multi-event-per-frame class: the kit's key helpers add idle frames that had hidden it. Briefs for UI units now ask for two events in one `drv.frame`.
- **CI:** CI-only failures were timing tests outside the units (F-timing), passed on a rerun of the failed job. #153's Preview needed three runs (one cancelled for runner capacity, one on F-timing's macOS web view test).
- **Prereleases:**
  - preview-2026-11-26-1849-258c75bf71fd (the ledger, #152);
  - preview-2026-11-26-2121-bd0433d29ce1 (ED-screen);
  - preview-2026-11-26-2308-51f7d3e6373d (X2-switch);
  - preview-2026-11-27-0001-405242b80cf8 (ED-dialogs).

### Closed rows

| ID | Status | What | Who |
|---|---|---|---|
| X2-switch | ✅ done 2026-11-26 | 10 §17's X2: the TUI's half over ssh, §7.3's outcomes, the question before switching; ships X2 (#154) | coder |
| ED-screen | ✅ done 2026-11-26 | 12 §16: §4 in cells, the shared highlighter, EH-1–EH-3 (#153) | coder |
| ED-dialogs | ✅ done 2026-11-27 | 12 §16: the editor's dialogs in cells with E6, ES-5 (#155) | coder |

## R1 (part 10) — the X4 cut and its first units, the address in xtools, channels, closed 2026-11-27

Eight PRs merged and shipped one by one, each prerelease installed on the Mac and, while it answered, the Linux host. Owner messages 25 and 26 came in this round; with WV-B and WV-E2, `sopen URL` opens the address in an xtools window by default.

- **The X4 cut** (#157): 10 §17's X4 in 14 units, with X4's rule for shared files (each unit's tests in its own `git_panel/tests_x4_<unit>.rs`, created by X4-graph; in a shared file a unit edits only the lines its row names, the second to merge rebases); X6, X7 and XF cut first.
- **X4-lanes** (#158): the git graph's lane glyphs in code page 437, one row a commit.
- **The URL fix** (#160): `sopen URL` with a KDE Plasma 6 HOST printed "opened … in the browser" and nothing opened. The forward imported `XDG_CURRENT_DESKTOP=KDE` but not `KDE_SESSION_VERSION`, so `xdg-open` ran the missing `kfmclient` and its exit-code shim reported success. The forward now imports `KDE_SESSION_VERSION`, `KDE_FULL_SESSION`, `XDG_SESSION_DESKTOP` and `DESKTOP_SESSION`. Post-release receipt through the real forward with a recording browser on the Linux host: the address and `KDE_SESSION_VERSION=6` reached it.
- **X4-kit** (#159): the dialog parts the git panel's dialogs need — choices (`(•)`/`[X]`, marked, not coloured), a multi-row message field (cut by cells, rows reused only for the very same text), a list with a bar, several fields and check boxes; the kit takes its own keys before egui's once-per-pass click.
- **X4-tree** (#161): the F10 tree in cells in both backends; a `SOPEN_CELLS` run waits until no tree request is in flight and blanks the title row's clock with the mode button; the switch-state test's flake fixed.
- **Channels** (#162, owner message 25): `sopen channel [stable|preview]` and `sopen update`, as llmux does — the channel is what brew installed; a switch installs the target first, relinks only if needed and retires the other formula last, so a failure never leaves no sopen on PATH; stable is refused before anything changes until the tap has its formula (the first `v*` release is the owner's).
- **WV-B** (#163): the Linux web engine — WebKitGTK loaded at runtime, one GTK thread for the process, offscreen views copied into egui textures, the sandbox probed and read back, a file view's main frame held to its scheme (every `about:` refused: a top-level `about:srcdoc` replaced the page, measured). Linux has no blocked-request count and shows no IME preedit (09 §2.3, §3.3).
- **WV-E2** (#164, owner message 26): `sopen URL` opens an xtools window with the address on every route; `--browser` keeps the system browser; no engine or no display falls back after a line.
- **Receipts:**
  - WV-B on the Linux host under a private headless KWin: the preview and the Enter viewer of an HTML file (CSS, a local image, a blocked outside image, Hangul, scripts off); 69 engine tests; quitting with a page open 5 times; the owner's session unchanged before and after every run.
  - WV-E2 on the Mac with the installed build: `sopen --here http://127.0.0.1:<port>/` → `opened … in xtools`, the page in the window with its script run and Hangul shown.
  - Channels on the Mac with real brew: `channel: preview (via brew)`; `sopen update` → up to date; `sopen channel stable` → the no-release line, exit 1, nothing changed.
- **Reviews:** two engines per unit, then one fix-check; the orchestrator verified the last fixes at their lines. The ~5,000-line WV-B split its reviewers by focus (GPT on the security contract, Claude on FFI and lifecycle). Found and fixed: a channel switch that could leave no `bin/sopen`, a GTK thread that could start twice, a file view that could show an `about:` document, a sandbox never read back, a receipt rule that could pass a changing screen, cues said after an early input end.
- **CI:** #162's first macOS run failed a stderr test that assumed two lines arrive in one read (macOS wrote them apart): reading and forwarding are now separate threads.
- **The Linux host** stopped answering ssh after #161's install; #162–#164 are installed on the Mac only, and WV-E2's Linux end-to-end receipt waits (F-wv-rest).
- **Prereleases:**
  - preview-2026-11-27-0138-dc7a9c97cc2b (the X4 cut);
  - preview-2026-11-27-0252-5be0be979975 (X4-lanes);
  - preview-2026-11-27-0359-4c7d3e73989d (the URL fix);
  - preview-2026-11-27-0514-d1cb4712b1aa (X4-kit);
  - preview-2026-11-27-0543-90f19943c845 (X4-tree);
  - preview-2026-11-27-0651-5f5819b8d386 (channels);
  - preview-2026-11-27-0805-4f752f12d09f (WV-B);
  - preview-2026-11-27-0837-17032859f5fd (WV-E2).

### Closed rows

| ID | Status | What | Who |
|---|---|---|---|
| X4-cut | ✅ done 2026-11-27 | 10 §17: X4 in 14 units, X4's rule for shared files, X6/X7/XF's first cut (#157) | orchestrator |
| X4-lanes | ✅ done 2026-11-27 | 10 §17: the graph's lane glyphs in code page 437 (#158) | coder |
| F-url-kde | ✅ done 2026-11-27 | The forward imports the desktop's KDE variables, so `sopen URL` opens on a KDE HOST (#160) | coder |
| X4-kit | ✅ done 2026-11-27 | 10 §17: choices, the message field, the list, several fields and check boxes in the dialog kit (#159) | coder |
| X4-tree | ✅ done 2026-11-27 | 10 §17: the F10 tree in cells; the cells receipt waits for the tree (#161) | coder |
| CH-update | ✅ done 2026-11-27 | Owner message 25: `sopen channel [stable|preview]` and `sopen update` (01 A18, §4b; #162) | coder |
| WV-B | ✅ done 2026-11-27 | 09 §12: the Linux web engine (#163) | coder |
| WV-E2 | ✅ done 2026-11-27 | 09 §12, owner message 26: the address window by default on every route (#164) | coder |

## R1 (part 11) — the git panel in cells, closed 2026-11-28

Thirteen PRs merged and shipped one by one, each prerelease installed on the Mac and the Linux host. With them every part of the git panel is drawn in cells in the window and in `sopen --tui`, except the planner's own dialogs (X4-planner2).

- **X4-graph** (#166): the panel's frame and graph in cells, and every X4 unit's test file.
- **X4-planner** (#168): the history planner's pane — rows, bar, scroll bar, note, buttons and keys' hint.
- **X4-pane** (#170): the details pane and the in-progress banner; a click on a step's button is the step's key.
- **X4-dialogs** (#171): the action dialog through the kit. What runs is what the screen showed: the TUI skips a pass while input waits, so every guard keeps the painting of the last pass that reached the screen (`cells::OnScreen`); a second move of the keyboard in one pass drops the Enters and Spaces after it.
- **X4-find** (#173): Find's field with its `Aa` toggle and the history search.
- **X4-lists** (#174): the command log, its entry shown whole, and the Backups list; a restore runs only from a list that was on screen.
- **X4-menu** (#175): the menu (F10, Shift+F10, a right click) and the filters (Ctrl+K). One git dialog at a time across panels side by side: the keyboard follows the open dialog, and an integration's dialog waits in its channel.
- **X4-dialogs2** (#177): every other action's form, the result dialog and "Stop git?" in cells; egui's action dialog is deleted. "Stop git?" asks about one run by its id and its Yes stops that run alone.
- **X4-quick** (#178): the quick edits' dialogs — Reword, Squash, Author and date, the plan's confirmation. `OnScreen` now tags each painting with its frame and its context, so a dialog not drawn in the last printed frame counts as unseen; a click or a keyboard move ends an input method's composition.
- **The Linux address window** (#167): its failures say their line once.
- **CI** (#169): the build jobs' limit is 60 minutes; macOS runners took 8 to 21 minutes for the same suite in one day, and #168's check was cancelled four times at 30.
- **Test fixes:** the hung-up terminal test no longer shares its pty with other tests' children (#172); a stand-in program written by a test is started through the busy retry, and the busy tests wait for a refusal (#176).
- **Receipts** (Mac, `--here`, an empty `SOPEN_CONFIG`, a private tmux): the window's `SOPEN_CELLS` and `sopen --tui`'s are byte-identical for each X4 unit's screens, on fixtures with Hangul names and messages:
  - the graph at 160×40 and a 90×30 side; the planner at 160×40;
  - the pane at 160×40 and 100×30, the uncommitted row and an edit stop's banner;
  - F7's Commit typed in Hangul, a restore's dialog, Fetch's and Edit a remote's forms; Merge's and Push's dialogs and a refused Check out's result;
  - Find and the search typed in Hangul;
  - the Backups list at three sizes (the log and its entry only in the in-test harness: their run times differ between two processes);
  - F10 on a merge, the repository's menu, also at an edit stop, and the filters;
  - F2 Reword, F8 Drop, a Hangul Reword, Author and date, and Squash.
- **Reviews:** two engines per unit (GPT and Claude), then one fix-check; the orchestrator verified the last fixes at their lines. Found and fixed, among others: a guard that trusted a painting from before a gap; a pass that copied a whole message (63 MB in ten idle passes for a 1 MiB Amend message, now none); a hidden box that blanked real cells; a stop question that outlived its write, then one matched by its words; an input method's composition left open by a click; a run's end dropped while a dialog waited (`try_recv` in a match whose arm waited).
- **Merging:** after X4-find merged, X4-lists' head merged cleanly with main but did not build; the merge script now refuses a head that does not contain main.
- **Prereleases:**
  - preview-2026-11-27-1026-f32f8056e6cc (part 10's ledger);
  - preview-2026-11-27-1426-9364048679ec (X4-graph);
  - preview-2026-11-27-1822-70714d3f0f79 (the Linux address window);
  - preview-2026-11-27-2041-4aa73975f1d6 (CI 60 minutes);
  - preview-2026-11-27-2141-b2c764b692db (X4-planner);
  - preview-2026-11-27-2226-90f8850f6963 (X4-pane);
  - preview-2026-11-27-2300-7eebce300b46 (X4-dialogs);
  - preview-2026-11-27-2331-1330e4bcbee0 (the hung-up terminal test);
  - preview-2026-11-28-0003-2393cd66cefd (X4-find);
  - preview-2026-11-28-0034-dcd6860b39c3 (the busy-stub fix);
  - preview-2026-11-28-0110-c12cde9f06fe (X4-lists);
  - preview-2026-11-28-0158-acfe74614909 (X4-menu);
  - preview-2026-11-28-0259-5f96aead6154 (X4-dialogs2);
  - preview-2026-11-28-0403-ee259ab2146f (X4-quick).

### Closed rows

| ID | Status | What | Who |
|---|---|---|---|
| X4-graph | ✅ done 2026-11-27 | 10 §17: the git panel's frame and graph in cells (#166) | coder |
| F-wv-linux | ✅ done 2026-11-27 | The Linux address window's failures say their line once (#167) | coder |
| F-ci-limit | ✅ done 2026-11-27 | The build jobs' limit is 60 minutes (#169) | orchestrator |
| X4-planner | ✅ done 2026-11-27 | 10 §17: the history planner's pane in cells (#168) | coder |
| X4-pane | ✅ done 2026-11-27 | 10 §17: the details pane and the banner in cells (#170) | coder |
| X4-dialogs | ✅ done 2026-11-27 | 10 §17: the action dialog in cells; what runs is what the screen showed (#171) | coder |
| F-hungup | ✅ done 2026-11-27 | The hung-up terminal test's pty is close-on-exec (#172) | coder |
| X4-find | ✅ done 2026-11-28 | 10 §17: Find and the history search in cells (#173) | coder |
| F-txtbsy | ✅ done 2026-11-28 | A just-written stand-in starts through the busy retry; busy tests wait for a refusal (#176) | coder |
| X4-lists | ✅ done 2026-11-28 | 10 §17: the command log and the Backups list in cells (#174) | coder |
| X4-menu | ✅ done 2026-11-28 | 10 §17: the menu and the filters in cells, one git dialog at a time (#175) | coder |
| X4-dialogs2 | ✅ done 2026-11-28 | 10 §17: every git dialog in cells, egui's action dialog gone (#177) | coder |
| X4-quick | ✅ done 2026-11-28 | 10 §17: the quick edits' dialogs in cells; `OnScreen` by frame and context (#178) | coder |

## R1 (part 12) — X4 closed: the planner's dialogs, the switch state's v2, and the git panel's dialog fixes, 2026-11-28

Eight PRs (#179 to #186) merged and shipped one by one, each prerelease installed on the Mac and the Linux host. With X4-planner2 no dialog of the git panel is egui's. With X4-state, F12 carries the tree and the git panel across, so X4 is whole except for X4-state2 (a history edit's own dialog's text, v3).

- **The ledger of part 11** (#179).
- **F-docs-cells** (#180): `docs/tui.md`'s cells lists are one line a part, and AGENTS.md points to them.
- **F-git-outcome** (#181): a result up stays until it is closed. Later results wait behind it, at most 8; past them, the next result up says how many were not shown. A result closes only once the screen showed it.
- **F-git-listq** (#182): a git dialog's run is never dropped under the tree or the list's question.
  - The tree counts as another dialog, and what a dialog returns is carried out.
  - The integration's dialogs wait for the list's question and the tree.
  - A pass in which any dialog slot of a shown panel changed during the git pass drops its keys.
  - Find's `Aa` takes no click while another dialog holds the keyboard.
- **X4-planner2** (#183): the planner's own dialogs in cells (a row's message, the author and dates, Esc's question, the run's confirmation), reading the pass's keys in order. A covered dialog counts as unseen, and the planner's ids are its own. The egui form parts and `dialogs::width` are deleted.
- **F-git-drafted** (#184): the host keys a dialog's reads by its serial and revision and borrows the draft. Ten idle passes with a 1 MiB message copy 0 bytes (31 MB before).
- **F-git-covered** (#185): a git dialog under another dialog is not on screen.
  - The F12 question counts as another dialog.
  - `cells::set_covered`: a covered pass records no painting.
  - The filters, the search and the log's entry leave their keys to a dialog drawn over them.
  - No covered dialog takes the keyboard.
- **X4-state** (#186): the switch state's v2.
  - **What crosses:**
    - the tree's bar and its folders;
    - each git panel's bar (looked up on HOST, then paged to), marks, details, filters and Find;
    - an action's dialog, the filters' dialog and the history search, with what is typed;
    - the planner's plan, which the question says goes.
  - **How the line is held:**
    - every list is bounded as it is parsed, and the line is held to 64 MiB both ways;
    - every commit id is looked up before use;
    - Archive's destination never crosses;
    - HOST fits its state to the line before sending it back, and says what it left out.
  - **Version:** v1 is still read, and v3 is refused.
- **Receipts:**
  - **X4-planner2.** On the Mac (`--here`, an empty `SOPEN_CONFIG`, a private tmux, 100×30), the window's `SOPEN_CELLS` and `sopen --tui`'s are byte-identical for the run's confirmation, a reword's message (also with Hangul typed), Author and date, and Esc's question.
  - **X4-state, the F12 receipt.** From `sopen --tui` through a stand-in ssh, with HOST's half reading the repository over SFTP and remote git:
    - the bar on a commit, two marks and Find typed into came back with the same rows and colours;
    - a plan with a squash row: the question came first, HOST showed the plan, and it came back as a change;
    - the fixture repository was unchanged.
  - **X4-state, the round trips.** In-process round trips of every listed state, among them a Hangul CRLF Commit message kept byte for byte.
- **Reviews:** two engines a unit (GPT and Claude), then one fix-check; I verified the last fixes at their lines. Found and fixed, among others:
  - a success that cleared a failure's dialog unread;
  - a dialog that closed itself and had its run dropped under the tree;
  - the list's Enter closing a run's end never on screen;
  - a covered planner question closing on [Enter, Enter] and losing the plan;
  - a host clone of the whole planner model every pass;
  - the filters taking Enter from a result drawn over them;
  - text typed into a dialog over the filters, lost;
  - a hidden panel's plan said to go and then dropped;
  - a state check stricter than the UI, which lost HOST's state on the way back;
  - an unbounded line;
  - Archive's path accepted on receipt.
- **Merging:**
  - Branches that met in a file were merged into each other before their turn, so each landed as a tree already gated: F-git-outcome into F-git-listq, F-git-listq into X4-planner2, and F-git-covered into X4-state.
  - F-git-covered's test of a dialog under F12's question moved onto a restore's dialog and a quick edit's, because under v2 a run's reopened confirmation switches without asking.
- **Prereleases:**
  - preview-2026-11-28-0456-ebc67fbfec47 (part 11's ledger);
  - preview-2026-11-28-0556-38c25a960bbf (F-docs-cells);
  - preview-2026-11-28-0631-fdc553856350 (F-git-outcome);
  - preview-2026-11-28-0734-d3e6196af5f1 (F-git-listq);
  - preview-2026-11-28-0813-4d3742248770 (X4-planner2);
  - preview-2026-11-28-0856-4a0035a54070 (F-git-drafted);
  - preview-2026-11-28-0949-7a5e0f75d84c (F-git-covered);
  - preview-2026-11-28-1037-a540d1f09ea6 (X4-state).

### Closed rows

| ID | Status | What | Who |
|---|---|---|---|
| F-docs-cells | ✅ done 2026-11-28 | The TUI's cells lists one line a part (#180) | orchestrator |
| F-git-outcome | ✅ done 2026-11-28 | A result up stays until it is closed; later ones wait (#181) | coder |
| F-git-listq | ✅ done 2026-11-28 | A git dialog's run is never dropped under the tree or a question (#182) | coder |
| X4-planner2 | ✅ done 2026-11-28 | 10 §17: the planner's own dialogs in cells (#183) | coder |
| F-git-drafted | ✅ done 2026-11-28 | The host checks a dialog's reads by its revision, not a copy (#184) | coder |
| F-git-covered | ✅ done 2026-11-28 | A git dialog under another dialog is not on screen (#185) | coder |
| X4-state | ✅ done 2026-11-28 | 10 §17, §7.4: the switch state's v2 carries the tree and the git panel (#186) | coder |
