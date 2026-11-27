---
title: sopen — xtools in the terminal first, the window the same screen
type: spec
tags: [sopen, spec, xtools, tui]
---

# 10 — xtools: the terminal UI first, the window the same screen

Status: building (2026-11-27; planned 2026-11-24, review round 1 applied). X1a (the window's list
in cells, #119), X1b (`sopen --tui`, #129), X3 (#139, #143, #145, #149, #151) and X2 (#154) have
shipped; X4 is cut into units, and X6, X7 and XF have a first cut (§17).

Answers item 2 of [`xtools/ssot.md`](xtools/ssot.md) (owner message 23). Items 1, 3, 4 and 5 (the
name, the editor, the viewer's views, web pages) land on this architecture: their screens are written
in cells as this document says. Measurements and the prototype: [`research/tui-gui.md`](research/tui-gui.md)
(XR-T), cited *tui* §n, its code citations at 255a52f; sopen code cited at this branch's `src/`.
**[M]** measured there · **[S]** read in source · **[추정]** inferred, not measured · **[owner]** only
the owner's terminals or hands can answer (the probe of *tui* §13) · **[M spike]** measured in the
real repo by the spike on branch `spike/tui-host` (§17). Decisions are T1–T40 (§13): T1–T10, T12,
T24 and T25–T40 are the orchestrator's (T12 and T24 rewritten in review round 1; T25, T27, T30 and
T32 extended in round 2), the rest this document's; all reversible.

Owner message 23 (2026-11-24), verbatim:

> 그리고 이 xtools는 터미널 tui가 디폴트고 (이미지나 영상 빼고 다 실제 tui랑 1:1 매칭되고 잇는 ui임
> 그 tui상태에서 버튼 누르면 tui가 그대로 gui로 변하게 해줘 gui에서는 이미지나 영상등을 출력해줄수 있고 그 이미지 출력할수 있는 터미널 모드도 있지 않나? 거기서는 네이티브 터미널에서 그대로 이미지와 영상 출력할 수 있게 해주고 이것도 리서치해서 어떻게 하는게 가장 좋을지 고민하고 작업 해줘

What it amends in 01, 04, 08, 09, the README and AGENTS.md, and when: §12.

## 1. The contract

- **One screen, two backends** (T1): every screen is one grid of cells (character, fg, bg); the
  terminal prints it, the window paints the same cells with the VGA font and hosts **pixel regions**
  (pictures, video frames, web views) at cell rectangles. **1:1** (§5): at one grid size, under
  truecolor, every cell outside a region is the same in both, the caret too; the one difference is
  the mode label.
- **The switch** (§7, T4): F12, Alt+G in a terminal, or the title row's `[GUI]`/`[TUI]` moves the
  running xtools between terminal and window, keeping where the owner is — in one process, or
  through 01 §4a's forward. The terminal is left as found (T19, T28).
- **Pictures in a terminal** (§8, T8): kitty, iTerm2 or sixel where answered, else half blocks; video
  plays only on a local screen, elsewhere the first frame and F12.
- **The default flips late** (§11, T2, T24). The owner's words are "TUI가 디폴트", and this document
  gets there in steps, not with the first slice: until the flip `sopen <folder>` is today's window
  and `sopen --tui <folder>` the TUI; the flip lands after the terminal draws pictures itself (X7).

## 2. Scope

- **In**: the core and both backends, the switch, terminal pictures and video, keys and mouse in a
  terminal, wide characters and colours, the window's look, routing and the flip, the migration order.
- **Not in**: Windows (01 §9); the window as a terminal emulator of the TUI's bytes (*tui* option D,
  killed: no protocol carries a native web view, and pictures over ssh came at 6–8 fps); F-keys on a
  laptop's top row inside a terminal (T9); the editor's design (XR-E, written in cells after X4,
  §16); a target that cannot reach this machine (the origin workstream, message 24,
  [`origin/ssot.md`](origin/ssot.md) — its channel would carry the same state, *origin* §6);
  unsaved edits carried across machines (T4); a page snapshot drawn as a terminal picture (T21).

## 3. Words

**Cell**: 8×16 pt in the window. **Grid**: columns × rows. **Wide**: 2 cells. **Region**: a cell rect
of pixels. **Core**: state, keys → commands, the screen into the grid. **Destination**: where the
switch opens the window (§7.1). **Hand-off box**: the TUI's stand-in for a content not in cells (T17).
**Remote screen**: T12's.

## 4. The architecture (T1)

- **The core** (`src/gui/cells.rs`) draws every screen into a `ratatui_core::Buffer` (ratatui-core
  0.1.2: the buffer only, not ratatui), as the spike did in the real repo (branch `spike/tui-host`:
  grid, painter, layer record, cell API and read-back, 451 lines [M spike]). Cell API: text, fill,
  frame (`╔═╗║` / `┌─┐│`, joints `╟ ╢` / `├ ┤`), scroll bar (`↑ ░ ↓`, one-cell thumb), F-key row,
  dialog with its shadow, field, button; widths by unicode-width (§10); colours the skin's 16 as RGB
  (T15, §5). **The cell API sets no modifiers** (no bold, underline, reverse or blink): PC Shell's
  look is colour only, and the caret is the cursor (T37). A screen declares its regions (a cell rect
  and `Picture`, `Video` or `Web`). Layout is a function of the grid size only.
- **The terminal backend** (`src/gui/term.rs`): crossterm 0.29 printing the core's layers —
  alternate screen, raw mode, SGR mouse reporting so the row, the mode button and rows click [M]
  (the terminal's own selection then needs Shift+drag in most terminals [추정]); the kitty keyboard
  protocol (flag 1) pushed by default whenever the terminal answers `CSI ? u`; platform output —
  egui's `CopyText` as OSC 52, the terminal's paste as bracketed `Paste` (Ctrl+V brings nothing in a
  terminal), the terminal cursor at the focused field's caret so the terminal draws a Hangul
  composition there (no preedit reaches a terminal) — seams the spike found, not built there;
  truecolor when `COLORTERM` says `truecolor` or `24bit` (kitty's environment, herdr-core's help
  string [M *tui* §7.1]), else §5's 256-colour table; the caret is the terminal's cursor; only
  changed cells are sent. **No font is loaded on the TUI's path.** Re-entering after a switch
  flushes the terminal's pending input: keys typed meanwhile are dropped, never acted on (T28).
- **The window backend** owns winit's handler (route B, T34): it builds the app with
  `eframe::create_native` and keeps one `EventLoop` per process for every window, each window run by
  `run_app_on_demand` (04 §6.4.3); 04's KL-W4 adds only its key wrapper. XR-T measured the switch
  with `run_native`, so X1b measures it again (TW-1). Each cell is painted at (col × 8, row × 16) pt
  — its background, then its glyph at its own cell (the VGA font, then the system fallbacks), never
  through egui's text layout, which is what moves Hangul off the grid (*tui* §1.3, §5). A wide
  glyph's second cell keeps the glyph's colours, which ratatui-core resets: before the spike's fix
  (15333c7) the bar's second cells of `사`/`진` were `0000aa`, after it every bar cell `ff5555` [M
  spike]. The grid is floor(w/8) × floor(h/16), the remainder in the background; the caret two scan
  lines at the cell's foot. A window opened by a switch has the terminal's grid size (T16).
- **Regions in the window**: a picture is a texture at the rect (the image exactly inside its frame,
  the other 1,800 cells passing the pixel check [M *tui* §6]); a video frame a texture uploaded each
  frame (320×180 at 93 fps [M]); a web view a WKWebView subview at the rect on macOS (09 §2.2; the
  rect measured with an NSBox [M], the visible check not made), WebKitGTK offscreen into a texture on
  Linux (09 §2.3). Cells paint over a texture; a native view is hidden while a cell layer overlaps
  it, as 09 hides it under egui layers. Selections inside a region stay pixel-exact in the window,
  cell units in the TUI (*tui* §1.2; design). In a terminal: §8.
- **Input: one neutral input** (T10). The terminal backend turns crossterm events into
  `egui::Event`s and runs a headless `egui::Context` pass, so the `FKeyInput` plugin's input hook
  (`gui/mod.rs`) and the key table (`browser/keys.rs`) see them as from the window; key, focus,
  memory and repaint code stay, drawing and the 45 font measurements (→ unicode-width) change (*tui*
  §11 [M counts]). **The translator** (`src/gui/term_input.rs`, 231 lines) was built and measured by
  the spike, through crossterm's real parser in a pty, on macOS [M spike]: under the kitty protocol
  101/101 key-table rows, 26/26 ⌘ variants and 20/20 F-key aliases reach the GUI's command; under
  legacy encoding 90/101 and 4/10 aliases, every loss already in crossterm's decode (§9). Its rules,
  egui-winit 0.37's event shape:
  - a named key → `Event::Key` with modifiers; Cmd/Ctrl+X and +C → `Cut` / `Copy`, then X's or C's
    release; a bracketed paste → `Paste`, then V's release, as a window's Cmd/Ctrl+V gives; every
    press is followed at once by a made release, and releases the terminal reports are dropped
    (`term_input::RELEASES`, as built in #106: the shell's digit aliases and egui's repeat marking
    need a release, and kitty reports none for text keys under flag 1);
  - **Ctrl → egui's `command` on every OS** (T26): Ctrl+x arrives with `ctrl` and `command` set, as
    egui reports Ctrl on Linux, so every Cmd/Ctrl row matches (`Mods::matches` needs `m.command`,
    `keys.rs:290-301`; a Mac's ⌃ alone would match none; the spike's negative control with egui-winit's
    macOS rule lost Ctrl+A [M spike]). ⌘, passed only under the kitty protocol, arrives as `mac_cmd` +
    `command` on macOS and as nothing elsewhere; ⌘-only rows (⌘⌫) are reachable only that way, Del
    and F8 are their routes;
  - **every printable ASCII character egui names a key for** (letters, digits, Space and the
    punctuation keys) → `Event::Key` **and** `Event::Text`, as egui-winit sends it (T25): the key
    table matches key triggers only on `Event::Key` (`keys.rs:602-617`; Space at `:486, 526, 565`,
    whitespace never a text trigger, `:1029-1031`), and the viewers match letters as keys —
    `W` (`text.rs:3979`), `C P F 1` (`image.rs:3258-3268`), `H` (`image.rs:4829`), through
    `plain_key`'s `consume_key` (`mod.rs:1880-1882`); no key-table row matches a plain letter, and an
    egui field inserts from `Text`. Other ASCII (`# $ % & ( ) * < > @ ^ _ ~ "`) and all non-ASCII
    text — a Hangul commit arrives as characters — → `Event::Text` alone (without it the letter
    jumps fail: the spike's negative control); with Ctrl or Alt held, the key and no text; mouse →
    pointer events at the cell's centre; wheel by lines; focus reports → `WindowFocused`.
- **Screens not yet moved** keep working in the window and are hand-off boxes in the TUI (T17). The
  spike hosted all four of today's panes (preview, Enter viewer, git panel, tree) and the F2 field and
  F7 modal at cell rects of a cell-grid window with 0 lines changed inside them: clipping, Tab, click,
  F-key and Esc routing and resize hold, with a failing negative control [M spike]. In a terminal they
  show nothing (they are egui shapes), which is why the TUI draws hand-off boxes and X1b moves the
  list's dialogs. The grid gives the odd column to the right side, so a hosted pane is 504 pt wide
  where the old half was 496 [M spike].
  *tui* option C's projector (egui shapes laid onto cells) stays only as a lint over them: off-grid
  counts may not grow past *tui* §1.3's rerun; a screen leaves it in the step that moves it (T1).

## 5. The screen contract: what 1:1 means

- **Compared per cell**, at one grid size, **under truecolor**, outside regions (masked, asserted as
  rects): character, fg RGB, bg RGB, the caret's cell, and the modifiers, which must be none in both
  (T37); a wide character's second cell carries the first's colours, as terminals paint it. Excepted:
  the mode label; the clock is fixed by a test hook. **How**: the backend's bytes parsed by the vt100
  crate and in a private tmux server (`capture-pane -e`), against the window's paint run headless
  with its fonts and its shapes laid back onto cells (what it painted, not the buffer it was given);
  the real window by its screenshot, cell by cell.
- **In a 256-colour terminal** the 16 skin colours go through one fixed table, the nearest by RGB
  distance among the indexes 16–255 (0–15 are never sent: themes repaint them), and the oracle
  compares through it (T37): BLACK 16 · BLUE 19 · GREEN 34 · CYAN 37 · RED 124 · MAGENTA 127 ·
  BROWN 130 · LIGHT_GRAY 248 · DARK_GRAY 240 · LIGHT_BLUE 63 · LIGHT_GREEN 83 · LIGHT_CYAN 87 ·
  LIGHT_RED 203 · LIGHT_MAGENTA 207 · YELLOW 227 · WHITE 231 (`chrome.rs:32-47`'s RGB, computed).
- **In an ambiguous-wide terminal** the TUI substitutes §10's table and the oracle maps through it.
- **Measured** [M *tui* §2] at 100×30 (Korean names, an emoji, `Down Down F7`, `새 폴더 2026` typed):
  vt100 (Mac) = the core's buffer exactly; vt100 (Mac), tmux 3.7b and vt100 (Linux) against the
  window's shapes → 1 character (the label), 0 fg, 0 bg, the caret equal. The orchestrator recomputed
  it from the dumps: 3,000 cells, 0 bg and 0 fg differences on painted glyphs, 1 character. The
  window's screenshot: backgrounds 3,000/3,000, glyph foregrounds 1,602/1,603 — the miss is U+1F3B5, a
  thin anti-aliased colour glyph with < 2 pixels of the exact colour; so the pixel check reads
  foregrounds only on glyph cells with ≥ 2 ink pixels, never on colour glyphs (T18).
- **What the proof covers**: the list, the F-key row, one dialog, three region kinds, the switch.
  Git lanes as glyphs, selections in cells, timeline and waveform as block glyphs and the editor are
  designs [추정 *tui* §0]. On Linux no window was opened (headless shapes only, the glow renderer
  only). Each step of §16 brings its screens under TS-4.

## 6. The look the grid forces on the window (T5)

The window changes to match the cells (*tui* §1.2 has the call-site counts):

| Item | Today | On the grid |
|---|---|---|
| Spacing between controls | 4 pt (half a cell) | whole cells only |
| Shadows; translucent overlays | translucent black; the timeline span (`chrome.rs` `span`) | DOS shadow: the cells' glyphs dark grey on black, 2×1; a solid skin colour |
| Dialog margin; panel inset; status bar | 2 × 1.5 cells; 2 pt; `CELL.y + 4` tall | 1 cell; none; one row |
| Frames; the rule above the foot | strokes; an `hline` at mid-cell | box glyphs; `─` with joints |
| Text fields; buttons and choices | egui field, 2-pt caret; framed widgets | black cells, a cell caret; buttons `[ Make ]`, choices `(•) word` (one of several) and `[X] word` (any of several, as a check box); the bar colour when focused |
| Git lanes (design) | 1-pt segments, a lane change one diagonal across half a row, dashed when uncommitted | code page 437's `│ ─ ┐ ┘ ├ ┤ ┬ ┴ ┼`, one row a commit, a lane change along its node's row; the uncommitted row's line `:` a cell (03 §4.3–§4.4, X4-lanes) |
| Timeline, waveform (design) | segments per pixel column | `▁▂▃▄▅▆▇█` per cell, two rows for L/R |
| Progress, spinner | egui widgets | `█▓░` cells; a one-cell ASCII spinner |

As the list's dialogs were built (XW-dialogs, 2026-11-25):

- **The shadow is black cells** in the window, not the cells' glyphs in dark grey: outside the
  tests the window keeps no record of the cells under a dialog (`cells.rs`'s recording is off),
  so it cannot draw them again. The terminal owns its grid and could; it does not (§17, XW-term):
  a per-cell 1:1 (§5, TS-1) needs both backends to draw the same shadow, so the dark-grey glyphs
  come to both at once, when the window keeps the cells under a dialog.
- **Buttons** are `[ X ]`, two blank cells between two of them (each its own text), the bar's
  colours when focused; nothing changes under the pointer (egui's buttons were inverted there).
- **The field's caret** shows while the field has the keyboard, also when the window is not in
  front (a terminal shows its cursor so too); egui drew none then.
- **A dialog's width** is its widest text's, its nominal width at least, at most 76 cells inside
  (or its nominal width, if more) and the window less 8 cells; a longer text wraps by cells, a
  path anywhere. It stands between the title row and the status row; where they leave too little
  room, its texts lose their last rows, and what still does not fit is cut there, never drawn
  over them.
- **Dialog and list text cannot be selected** (egui's labels could) until cell selection lands.
- **A dialog's scroll bar** (the report's): ↑ and ↓ a row, a click on the track a page, the DOS
  way (egui's scroll area jumped there), the thumb dragged.

## 7. The switch (T4)

### 7.1 Where the window opens (T3, T11)

- **The destination is resolved as 01 §4a resolves a run** — `--here`, `--target HOST`,
  `SOPEN_TARGET`, the config's `target`, else this machine — read when the owner switches, not at
  start, so a config fixed meanwhile applies. A HOST destination is the remote switch (§7.3, since
  X2-switch; before it the status row said `switching to HOST comes with the remote switch; --here
  switches on this machine`).
- **This machine** with a display (`gui::display_available`: DISPLAY or WAYLAND_DISPLAY on Linux,
  macOS always) → one process (§7.2). Without one, the status row says `no display here — sopen config
  target <host>, or --target, opens the window on another machine` and the TUI stays.
  **HOST** → across machines (§7.3).
- **The owner's setup** (*tui* §7.1, §9): xtools in a herdr pane on the Mac, shown on the Linux
  laptop, `target = <the Linux laptop>`. F12 opens the window on the laptop, where the owner looks —
  "a display is here → in-process" alone would open it on a Mac screen nobody looks at.

### 7.2 In one process [M *tui* §8, with `run_native`]

- F12 or the click **leaves the terminal** (T28: pictures deleted, keyboard flags popped, mouse
  reporting off, the alternate screen left, the cursor shown), prints `xtools is in a window now —
  close it, or press F12 there, to come back here.`, runs the window on the main thread on the
  process's event loop with the same state, and comes back with the state it hands back, the input
  flushed (§4); unsaved edits cross unsaved (one process holds them). Measured: 361 ms to the first
  window's first frame, 60 ms the second; a folder made in the window and a dialog's typed text,
  caret and error came back, twice.
- **A window that fails to start** (no backend, a renderer or window error, after the terminal was
  left) brings the TUI back with its state unchanged and one line naming the reason and what to do:
  `the window did not open: <reason> — check the display, or set a target` (T31).
- In a **switched window** — one opened by a switch, here or on HOST through `--state-stdin` —
  F12, `[TUI]` or closing it returns, even with no terminal on HOST's side. Cmd/Ctrl+Q and +W are
  that close too — the shell already treats them as the close button (`closing()` in
  `gui/mod.rs`) — so they return with the state (on HOST, with the cue) (M1). A window
  started directly quits on close, as today; it shows `[TUI]` only when its process has a terminal
  on input and output (F12 then takes it), else neither works there.

### 7.3 Across machines (T29, T30, T35)

- **HOST's half**, through 01 §4a's forward (its master, options, bare `exec` and quoting, HOST's
  PATH and desktop environment, the macOS float rule), with `-T`, `-o BatchMode=yes` and
  `SSH_ASKPASS_REQUIRE=never` (the TUI holds the terminal, so ssh may not ask anything): `sopen
  --here --gui --state-stdin -- <the active folder, rewritten as §4a rewrites SOURCE>`. The state is
  one JSON line on its input, which then stays open; HOST's stderr goes to the status row, its last
  line kept. When the window hands back (§7.2), HOST prints `sopen-xtools-state <json>` (a fixed cue,
  as `sopen-tunnel-open`) and exits. **Measured** [M *tui* §8], laptop TUI (no display) → Mac window:
  4,885 bytes, first frame 192 ms, a folder made there listed back, 1.1 s end to end. **The owner's
  direction (Mac TUI → laptop window) is not measured** [owner, TW-3].
- **Meanwhile** this side keeps raw mode and the alternate screen (T28): pictures deleted, the screen
  dimmed under `xtools is in a window on HOST — close it or press F12 there; Ctrl+C ends it here`,
  only Ctrl+C read.
- **Coming back with unsaved edits** (T30): F12 or a close in HOST's window (Cmd/Ctrl+Q and +W
  included) first asks that window's close prompt there — **Save** · **Save As…** · **Discard** ·
  **Cancel** (07 §10, `07-streaming-edit.md:801`); Cancel keeps the window; a running Save or any
  writing job (§7.4) makes the return wait.
- **HOST's input ending** — Ctrl+C here, or the link dropping — closes HOST's window by 09 §7's
  end-of-input rule **only when it has nothing to lose**. With unsaved edits it **stays open** (01
  A11: never dropped unasked): HOST's sopen prints `sopen-xtools-kept` if its output still works,
  stops reading and writing the session, and keeps the window until the owner closes it there.
  A session without a pty sends its command no SIGHUP when it dies: the command sees only its
  input ending [M 2026-11-26: a command run by `ssh -T` with a SIGHUP trap kept running after the
  local ssh client was killed with SIGKILL — the trap never fired, and it finished its 15 s sleep
  and wrote its end file]. This side restores its
  state from before the switch with `the window on HOST stays open: it has unsaved edits` (after the
  cue) or `lost the link to HOST; a window there with unsaved edits stays open` (without it).
- **Outcomes** (status row; the TUI stays and lists again; a cue's state is used whatever the code,
  otherwise the state from before the switch):

| HOST's end | Line |
|---|---|
| 0 with the cue | none (back with HOST's state) |
| 0 without the cue | `sopen on HOST ended without handing the screen back` |
| 1 | `sopen on HOST failed: <its last stderr line>` |
| 2 | `sopen on HOST is too old for the switch, or a usage error: <line>` |
| 3 | `the window on HOST ended with a refused save: <line>` |
| 128 + n | `sopen on HOST ended by signal n` |
| 127 | `sopen is not installed on HOST (brew install 2lab-ai/tap/sopen-preview)` |
| 255 | `could not reach HOST: <ssh's line> — run ssh HOST once in a shell to answer its questions` |
| no exit code (ssh killed by a signal) | `ssh to HOST ended without an exit code (<status>)`, as `forward.rs:1141` says it |

- **Paths** go out rewritten as §4a's SOURCE (local → `<this machine>:<abs>`, HOST's own → local) and
  come back with `<this machine>:<abs>` local again. HOST reads this machine's files by its own ssh
  (§4a); a HOST that cannot is the origin workstream's.
- **Every forwarded window run passes `--gui`** (T35): the switch's from X2, `--target`'s from XF —
  after the flip a forward with `-t` would otherwise start a TUI inside ssh on HOST. Until XF a
  `--target` run passes it only when the owner gave it (decided 2026-11-26): an automatic one would
  turn HOST's fallbacks without a display (folder → editor, a file fetched and its path printed)
  into exit 1, against §11 row 6, and before the flip HOST already opens a window for a window kind
  when it can. **XF turns on the forward's automatic `--gui`** (`AUTO_GUI` in `src/main.rs`, X2-gui):
  for a window run, when the kind the sender sees needs a window; a text file, a kind the sender
  cannot see, or a run the hooks keep without a window (`SOPEN_LIST`, `SOPEN_AUTOSAVE`, …) goes
  without it. An older HOST that refuses `--gui` at parse time — exit 2, clap's line the first it
  wrote on stderr, nothing on stdout — gets the run again without it (decided 2026-11-25, Tensions;
  X2-gui).

### 7.4 What the state carries (T32); unsaved edits (T4)

- Versioned JSON (`"v": 1`; the prototype's fields [M *tui* §8] plus the viewer's): both panels'
  folders, bars, selections and scroll, the active side, the mode, each side's content (tree open, git
  pin), an open dialog with its typed text, caret and error; the open viewer — file, 08's view,
  place, encoding, wrap; the source host. No caches, streams or jobs: the receiver opens them again.
  A reader accepts its own `v` and every older one (a field the older state lacks takes its
  default); a newer `"v"` is refused with `HOST sent switch state v<n>; this sopen reads up to v<m> —
  update one of them`, and the TUI stays. **Each step that moves a screen adds that screen's fields and bumps
  `v`**: the tree's bar and open rows (X4); the git panel's bar, marks, details and filters, and the
  planner's unsaved plan, which asks first as unsaved edits do (X4). Unknown fields are ignored.
- **Every running job that writes** — a Save, a file operation, a git fetch, push or rebase, an
  export, `stems` — makes the switch wait (`waiting for <it>…`), as a running Save makes 08's view
  switch wait. Reads are dropped and started again on the other side.
- **Unsaved edits across machines**: 07's journal is per machine (07 §3) and does not cross in the
  first cut. The switch first asks `Save the changes to host:/abs before switching to HOST?` →
  **Save** or **Save As…** (switch once it lands) · **Discard** (switch with the file as stored) ·
  **Cancel** (stay), 07 §10's four choices.

## 8. Pictures, video and pages in a terminal (T8)

- **Which protocol**, at start: (1) queries — kitty `a=q`, DA1's sixel attribute `4`, XTVERSION and
  iTerm2's `ReportCellSize`; (2) `TERM` / `TERM_PROGRAM`; (3) else half blocks (two pixels a cell).
  `SOPEN_PICTURES=kitty|iterm2|sixel|blocks` (environment, or `pictures` in the config) overrides
  the pick (T20).
- **An answer is not proof of drawing.** herdr's libghostty-vt parses kitty frames [M strings], so an
  `a=q` OK there proves parsing only; whether herdr-app draws is [owner, TP-2]. herdr-core sets
  `TERM_PROGRAM=ghostty`, so the environment cannot tell herdr from Ghostty: until the probe answers,
  a TUI with `HC_TERMINAL_ID` in its environment (seen in the laptop's herdr panes [M], on the Mac
  [추정]) uses half blocks (T20). A TUI reached by ssh from inside a herdr pane has no such variable.
- **Placement**: scaled by the reported cell size (else 8×16 px [추정]); deleted under a cell layer.
- **Local fast path**: kitty by temp file (94 fps vs 59 in-band [M]); over ssh it showed nothing, the
  file being on the other machine [M], so it is used only when a query sent by that medium is answered
  OK [추정: kitty's query by medium, not tried]; else in-band, zlib.
- **A remote screen** (T12): `SSH_CONNECTION` or `SSH_TTY` is set; else the screen is local.
  `SOPEN_SCREEN=local|remote` (environment, or `screen` in the config) overrides it, and the TUI's
  help and status row show the value in force and where it came from. The owner's herdr path is a
  TUI running locally on the Mac whose pane may be viewed from the laptop: no ssh variable sees that,
  so there `screen = "remote"` is the owner's setting [owner: whether herdr panes are viewed from
  elsewhere; the probe's DSR round trip per terminal is recorded for information only].
- **Video**: a local screen plays (640×360 in WezTerm [M *tui* §7.4]: the kitty protocol 59 fps,
  sixel 41, half blocks 221; the kitty terminal and herdr unmeasured); a remote one shows the first
  frame and `F12 plays it in the window` — over ssh the kitty protocol gave 6–8 fps, sixel 11, and the
  link varied 5× in half an hour. **Sound** comes out of the machine sopen runs on, so on a remote
  screen the TUI plays none; the waveform and F12 instead (T22).
- **Views in a terminal** keep 08 §3's slots in both backends, so the strip is 1:1 and the state's
  `view` means the same after F12. An image opens on view 1 (a terminal picture, half blocks at
  worst); a video on view 1 (it plays, or its first frame). A page: HTML, HTM and styled XML open on
  their source slot, 3, with view 1 a one-row hand-off (`the page is drawn in the window — F12`);
  plain XML and RSS open on 1, which is already their source (09 §5). No snapshot: the engine runs
  inside a window (WKWebView needs one; WebKitGTK's offscreen path a display [추정]) (T21).

## 9. Keys in a terminal (T7, T9, T25, T26, T33)

| Route | Legacy encoding | kitty protocol |
|---|---|---|
| F1–F12, Shift/Alt/Ctrl+F5–F10, Shift+Del, Ctrl+PgUp, Ctrl+←/→, Shift+Tab | decoded [M] | decoded [M] |
| The clickable F-key row and mode button | mouse reporting [M] | the same |
| **Alt+1…0 → F1…F10** (T7); **Alt+G → the switch** (T33) | `ESC <char>`, read by crossterm as Alt+char [M] | `CSI …;3u` [추정] |
| Ctrl+1…0 → F1…F10 (04 §6.1) | Ctrl+4…7 only; Ctrl+1/9/0 → plain digits, Ctrl+2 → Ctrl+Space, Ctrl+3 → Esc, Ctrl+8 → Backspace [M spike] | decoded, 10/10 [M spike] |
| Ctrl+letter (= Cmd/Ctrl, T26) | Ctrl+I = Tab, Ctrl+M = Enter [M]; Ctrl+J is sent as LF and decoded as Ctrl+J, so it reaches `ToHead` [M spike] | distinct [M] |
| Cmd+… (macOS) | never sent | `CSI …;9u` when the terminal passes ⌘ [M decode]; ⌘+digits are tab keys there [추정] |
| Shift+F3; Ctrl+Shift+letter (git) | read as a cursor report; Shift lost, so the Ctrl chord arrives [M] | distinct [M] |
| A printable ASCII key (Space, letters, digits; T25) | the character → `Key` + `Text` | the same |
| 08's view keys | `1`/`2`/`3` → `Key` + `Text`, matched by the window's own binding (08 §8: physical key first, else the logical key; the translator gives no physical key); F9 then a digit | the same |
| Esc | a lone `ESC`, by timing | `CSI 27u` |

- **Alt+digits and Alt+G** are taken once, by the `FKeyInput` plugin's input hook, **in the TUI only**
  (in the window Option+digit and Option+G type characters on macOS). Alt+digits never act while a
  text field has the typing (04 §6.1); Alt+G does (it types nothing). Cmd/Ctrl+digits stay where they
  arrive (T7). On a Mac keyboard Alt+char arrives only where the terminal sends Option as Alt
  [추정]; herdr-app's own bindings are unknown [owner, TK-2]. Legacy residual: Esc then a digit or `g`
  within the escape timeout reads as an Alt chord [추정].
- **Alt+G** (T33): `G` for GUI; under legacy it arrives as `ESC g`, which crossterm decodes as
  Alt+G [M for `ESC x`]; the key table binds no Alt+letter (its Alt rows are F-keys, and Cmd+Alt+G is
  another chord under exact matching); with no text, it never reaches a letter jump.
- **Legacy encoding loses 17 bindings** (the spike's 118 plays of every key-table row and alias, all
  lost in crossterm's own decode, i.e. in what the terminal sent — none in the egui layer [M spike]):
  ⌘⌫ (1: a legacy terminal cannot send ⌘); Shift+F3 (1: `CSI 1;2R` reads as a cursor report); 9
  Ctrl+Shift+letter git rows (Shift lost: +F, +R, +G, +B fire Find, Reread, Pin, Backups, **+M
  arrives as Enter → Details**, +C, +S, +A, +P fire nothing); 6 Ctrl+digit aliases (Ctrl+1, 9, 0 →
  letter jumps; Ctrl+2 → nothing; Ctrl+3 → Esc → Stop; Ctrl+8 → Backspace → Up). Their routes: the
  Ctrl+Shift actions and Shift+F3's file at revision are in the F10 menu of what is under the bar,
  which already holds every action (03 §7) (T17); the F-keys by Alt+digits, the row, or real F-keys;
  ⌘⌫ by Del or F8. The kitty protocol, pushed by default, loses none. Raw mode clears ISIG and IXON,
  so Ctrl+C, Ctrl+Z, Ctrl+S and Ctrl+Q arrive as keys [추정: cfmakeraw].
- **F12 = the switch** (T4), taken by the `FKeyInput` plugin in every context, fields included (it
  types nothing), never a key-table row (`no_duplicate_bindings` forbids F12 rows and stays).
  Whether kitty, herdr-app and herdr pass F12 is [owner, TK-2]; the button always works.
- **The help in both backends is one text** (T27): every alias named with where it works —
  `Ctrl/⌘+1…0 · Alt+1…0 in a terminal`, `F12 · Alt+G in a terminal` — and every command row written
  `Ctrl/⌘+R`, `Ctrl/⌘+Shift+F`, with one line saying `⌘ applies in a Mac window; Ctrl everywhere
  else`. It replaces the machine's own name (`keys::command_name()`) in `Mods::prefix`
  (`keys.rs:309-351`), help's `aliases()` and `describe` (`help.rs:139-146`).
- **No per-app F-key switch in a terminal** (T9): sopen owns no surface there; the macOS HID switch
  (04 §6.2) and the Wayland shortcuts inhibitor (04 §6.4) are the window's; kitty's own keyboard grab
  is the owner's terminal setting, a later study. In the TUI the F-keys that always work are real
  F-keys, the row and Alt+digits.

## 10. Wide characters and ambiguous width (T6, T38)

- **Terminal**: unicode-width — Hangul, CJK, emoji (`한 글 漢`, U+1F600, U+1F44D U+1F3FD) 2 cells;
  `─ … ░ A é` 1 [M *tui* §5].
- **Window**: each glyph painted at its first cell. Hangul advances 13.84 pt on the Mac (Apple SD
  Gothic Neo), 14.72 pt on Linux (WenQuanYi Zen Hei) and sits in its 16-pt slot with 1–2 pt to
  spare; emoji 16.00 pt on the Mac, but Noto Color Emoji on Linux is 19.92 pt, 3.92 over two cells —
  clipped to them. (egui's own layout puts 7 Hangul in 12.1 cells where the terminal gives 14.)
- **Ambiguous width** (T18, T38): at start the TUI writes `─` and reads the cursor report. If it took
  2 cells (an "ambiguous = wide" setting, possible under the laptop's `ko_KR.UTF-8` [추정]), **every
  ambiguous-width glyph sopen draws from its own tables** is substituted in the TUI, the window keeps
  CP437, and the oracle maps through the table. Width A by python3's `unicodedata` covers the box
  glyphs, `▀ ▄ █ ▌ ▒ ▓ ■ ▲ ▼ ○`, the arrows, `… · • ≡ ↔ ↕` and the CP437 table's Greek, maths and
  Latin-1 letters (`chrome.rs`); `░ ► ◄ ◙ »` are not ambiguous and stay:

| Glyphs (width A) | In the TUI |
|---|---|
| box corners and joints (`┌┐└┘├┤┬┴┼╔╗╚╝╟╢╤╧╞╡╭╮╰╯` and the rest of U+2500–U+257F) | `+` |
| `─`, `═`; `│`, `║` | `-`, `=`; `\|` |
| block and shade elements (`▀ ▄ █ ▌ ▒ ▓`, the timeline's `▁…▇`); `■` | `#` |
| `↑ ↓ ← → ↔ ↕ ▲ ▼` | `^ v < > - \| ^ v` |
| `…` (a cut name) | `~` |
| `·`, and every other width-A glyph of the CP437 table (the hex ASCII column, C0 controls) | `.` |

- File content (names, text lines) is not substituted: there the TUI lays an ambiguous character in
  2 cells, so nothing overlaps, and the oracle compares such rows as text.
- **As built** (XW-receipts, 2026-11-25): the printer cannot tell a table's glyph from content, so
  it substitutes only what is sopen's chrome — frames and blocks (a box line between two glyphs of
  a name, `a─b`, is the name's), scroll bars' arrows, the cut's `…`. Every other width-A glyph,
  sopen's own text included (the hint row's `·`, a status line's arrows, `■`), is drawn as written
  in 2 cells, its run moved into the blanks after its words; the CP437 table's last row is the
  viewers' to substitute at the source (X3-text, X3-hex).
- **Cutting a name** (01 §3a) never splits a wide character: it ends a cell earlier, with "…".

## 11. Routing, until the flip, and the flip (T2, T3, T24, T36)

- **Until the flip**, `sopen <folder>` is today's window, unchanged; `sopen --tui <folder>` runs
  xtools in this terminal with F12 to the window (§7). A content not in cells yet is a hand-off box
  (T17): a frame at its place with its title, what it is, and a clickable `F12 shows it in the window`.
- **The gate** (T2): the TUI becomes the default only when Enter on every kind does something useful
  in it — text and hex in cells (X3); image, media and page by terminal graphics where the terminal
  can, else a one-key hand-off. **T24 places the flip after X6 and X7** (and so after X3 and X4):
  message 23 asks for pictures and video in the terminal itself, so at the flip they are drawn there,
  not handed off.
- **One routing table** (T36): first match wins, in the rows' order — the explicit mode (rows 1–3),
  the kinds routed elsewhere (4–5), input and output terminals (6), a folder (7), the destination
  (8), the display (9–10). `--hex` makes the kind hex, then rows 4–10 apply.

| # | When | Opens |
|---|---|---|
| 1 | `--app` | the named app, as today (01 A6) |
| 2 | `--gui` | the window on the destination (HOST: forwarded with `--gui`); here with no display: `no display here`, exit 1 |
| 3 | `--tui` | the TUI here; input or output not a terminal: usage error, exit 2 |
| 4 | an address (`http(s)://`) | 09 §7 and §4, unchanged |
| 5 | text (a file) | 01 A2's editor route, until XR-E decides |
| 6 | input or output not a terminal | today's routes exactly: `while read` loops (01 §4a), `SOPEN_LIST`, `SOPEN_AUTOSAVE`, `SOPEN_AUTOEXPORT`, a forward without a terminal |
| 7 | a folder | the TUI here, a target set or not (before XF: today's window, on the destination) |
| 8 | destination HOST | HOST's window, forwarded with `--gui` |
| 9 | this machine with a display | the window, as today |
| 10 | this machine, no display | the TUI here, the file in its viewer: an image as a terminal picture, a binary as the binary dialog in cells, then hex (before XF: fetched and its path printed) |

- An image on a terminal with no display and no target is row 10: the TUI shows it. **`sopen --tui
  <file>`** (row 3) opens the TUI on the file's folder, the bar on the file and its viewer open;
  Esc returns to the list (T39). After the flip `target` is where the window opens on a switch and
  `--here` keeps it on this machine (T3).

## 12. Contracts that flip

| Text | Today | Becomes | With |
|---|---|---|---|
| 01 A11, §3a's first line | a folder opens in a window | the TUI in this terminal (§11 row 7) | XF |
| 01 §3a "Without a display"; A6, §3, §6a (no display) | the folder to the editor; a file fetched, its path printed | the TUI, where input and output are terminals (row 10) | XF |
| 01 A9, A10, §4a | `target` = where SOURCE opens | the switch's destination (X2, for `--tui`); for `sopen <folder>`, where the window opens on a switch; GUI kinds as today (XF) | X2, XF |
| 01 §4a, the forward | HOST runs `sopen --here …` | every forwarded window run passes `--gui` (`--target`'s when the owner gives it from X2, by kind from XF); the switch's half `--gui --state-stdin` with `BatchMode=yes`, the `sopen-xtools-state` and `sopen-xtools-kept` cues | X2 |
| 01 §4a, the config | `target`, `fkeys` | plus `screen` (T12) and `pictures` (T20) | X7 |
| 01 A14 | every window | the HID switch and the inhibitor are the window's; the TUI has the row, Alt+digits, real F-keys | X1b |
| 01 §3a, the skin | translucent shadows and overlays, half-cell paddings | §6's look | each step, for its screens |
| 01 §3a, 04 §3 rule 4 | no plain F-key acts while a field has the typing | except F12 and Alt+G, which type nothing (as 08 D29 excepts F9) | X1b |
| 04 §3 rule 3 | Cmd on macOS, Ctrl elsewhere | in the TUI Ctrl on every OS, ⌘ only under the kitty protocol (T26) | X1b |
| 04 §2 | "F11 and F12 stay unused" | F12 = the switch, the `FKeyInput` plugin's (not a table row) | X1b |
| 04 §6.1 | Cmd/Ctrl+1…0 | plus Alt+1…0 in the TUI; Cmd/Ctrl+1…0 in the TUI under the kitty protocol | X1b |
| 04 §10, KL-W4 | "if X1 already owns the winit handler…"; KL-W4's run is `create_native` | X1b owns `create_native` and the one event loop (T34); KL-W4 adds only its wrapper | X1b |
| `src/gui/help.rs` (`aliases()`, `describe`, `:139-146`); `keys.rs` `Mods::prefix` (`:309-351`) | this machine's command key (`Cmd+R` on macOS, `Ctrl+R` elsewhere) | `Ctrl/⌘+R` on every row, one line on where ⌘ applies, each alias with where it works (T27) | X1a |
| 08 §8–§9, VM-7, VM-8 | provisional; "if XR-T keeps egui windows" | VM-7's screen half is cells already (the strip, F8's list: #128); VM-8 keeps 08 §3's slots in the terminal in X3 (T21); VM-7's input half (the window's key and text, `Driver::press`) stays for the window; §9's "Alt+digit is none either" gains "except as the TUI's F-key aliases"; its "opens on Hex" becomes §8's: an image and a video open on 1 | X3 |
| 09 §8 | "in a terminal a page file shows its source (the source is view 1 there)" (09 §8's first paragraph before VM-8) | 08's slots kept: HTML and styled XML open on 3, plain XML and RSS on 1; F12; no snapshot | X3 |
| 09 WV-D, WV-E chrome | egui | cells if X1a has landed, else egui moved in X6 (T23) | X1a, X6 |
| README | line 9 (folder → window); "Requirements" (a desktop session); "Open on another machine's screen" | `--tui`, F12, Alt+G (X1b); the switch to a target (X2); folder → TUI, target = the window's place, a section "In the terminal" (XF) | X1b, X2, XF |
| AGENTS.md "Receipts" | — | rows for `--tui`, `--gui`, `SOPEN_CELLS` (a local path, not forwarded), `SOPEN_REQUIRE_TMUX`, `SOPEN_SCREEN`, `SOPEN_PICTURES` | X1b (`--tui`, `SOPEN_REQUIRE_TMUX`), X2 (`--gui`), X3 (`SOPEN_CELLS`), X7 |

**Frozen meanwhile** (*tui* §12): new egui screens — 08's screens and the editor are written in cells
— and egui widget work on screens about to move; bug fixes continue. 09's chrome follows T23, so
WV-D still ships without waiting (09 §12).

## 13. Decisions

| # | Decision | Why |
|---|---|---|
| T1 | Architecture A: one cell-grid core; a terminal and a window backend paint the same cells and the window hosts pixel regions. C's projector stays only as a lint for screens not yet moved | 0 bg / 0 fg differences on painted glyphs, 1 character by design (*tui* §2); B = the same grid work + a second renderer; C left the F7 dialog 158 glyphs and the hex window 1,337 off grid; D carries no native view |
| T2 | The default flip is its own late step: the first slices ship `sopen --tui` opt-in; the TUI becomes the default only when Enter on every kind does something useful in it (text/hex in cells; image/media/page by terminal graphics where the terminal can, else a one-key hand-off). Until then `sopen <folder>` keeps today's window | "TUI가 디폴트" is the owner's; a default TUI whose viewers show nothing would be worse than today's window |
| T3 | After the flip: `sopen <folder>` runs the TUI in this terminal; `target` is where the window opens on a switch; `--here` keeps its meaning for the window; the no-display path becomes the TUI | the owner's setup (a Mac TUI shown on the laptop, target = the laptop) needs the TUI here and the window there (*tui* §9) |
| T4 | Switch: F12 and a clickable `[GUI]`/`[TUI]` in the title row (mouse reporting); in one process the same state continues in the window and back; across machines the state goes to the target's sopen, which opens the window; unsaved edits ask to save first, no journal transfer in the first cut | both measured (*tui* §8: 361/60 ms; 4,885 bytes, 1.1 s); 07's journal is per machine |
| T5 | The grid's look: no half-cell spacing, DOS shadows, bracketed buttons; the window changes to match the cells | 1:1 cannot hold otherwise (*tui* §1.2, §3) |
| T6 | Wide characters painted per cell in the window: Hangul 2 cells, emoji 2 cells clipped to them | egui's layout drifts Hangul (13.84 / 14.72 pt); the Linux emoji overflows by 3.92 pt (*tui* §5) |
| T7 | Legacy-terminal aliases: Alt+1…0 = F1…F10 in the TUI, alongside Cmd/Ctrl+digits where they arrive; the kitty keyboard protocol requested when the terminal supports it | under legacy encoding the Ctrl+digit aliases are lost or become other keys (*tui* §4) |
| T8 | Terminal pictures: kitty / iTerm2 / sixel where supported (detected by query), half blocks otherwise; video plays locally, on a remote screen shows the first frame and offers the GUI; whether herdr passes pictures is the owner probe's | 6–8 fps over ssh is not playback (*tui* §7.4); herdr's drawing is unmeasured |
| T9 | F-keys in the TUI: no per-app switch exists in a terminal; kitty's keyboard grab is the owner's terminal setting, a later study | sopen owns no surface in a terminal (*tui* §10) |
| T10 | egui's input types stay the neutral input of both backends; tests query cells (text and attributes), not pixels | the key table, focus and repaint code and the tests' input side survive (*tui* §11) |
| T11 | The switch's destination: 01 §4a's order first, the display second | in the owner's setup "display here → in-process" opens on a screen nobody looks at |
| T12 | A remote screen = `SSH_CONNECTION` or `SSH_TTY` set, else local; `SOPEN_SCREEN` / `screen` overrides; the value in force is shown in help and the status row (replaces round 0's 5 ms round trip) | a timing threshold is a guess; the owner's herdr path is the owner's setting; the round trip is kept as probe information |
| T13 | The TUI only where input and output are terminals; Ctrl+C during a remote switch returns with the state from before | scripts and hooks keep today's paths; the owner always has a way back |
| T14 | `--gui` after the flip; GUI kinds on the command line keep opening the window on the destination; closing a switched window returns to the TUI | *tui* §9's proposal; today's routes stay reachable; one close does not lose the terminal |
| T15 | Colours as RGB (truecolor), else §5's 256-colour table; never the 16 ANSI indexes | a terminal theme repaints the ANSI indexes, breaking 1:1 |
| T16 | A window opened by a switch has the terminal's grid size | 1:1 is defined at one grid size |
| T17 | Contents not in cells yet are hand-off boxes in the TUI; under legacy encoding Shift+F3 and the Ctrl+Shift git actions are reached through the F10 menu of what is under the bar | each step ships alone (*tui* §12); the menu already holds every action (03 §7) |
| T18 | The pixel check reads foregrounds on glyph cells with ≥ 2 ink pixels, never colour glyphs; an ambiguous-width probe at start | the U+1F3B5 miss (*tui* §2); "ambiguous = wide" would break every frame |
| T19 | The terminal is left as found when xtools ends: exit, panic, SIGTERM, SIGHUP | a TUI dead in raw mode leaves the shell unusable |
| T20 | `HC_TERMINAL_ID` set → half blocks until the probe answers; `SOPEN_PICTURES` / `pictures` overrides the pick | herdr parses kitty frames but may not draw them; its `TERM_PROGRAM` says ghostty |
| T21 | 08's slots in both backends: an image and a video open on 1; HTML and styled XML on 3 with view 1 a hand-off row; plain XML and RSS on 1; no page snapshot | the state's `view` must mean the same in both backends; the engine runs inside a window (09 §2) |
| T22 | No sound on a remote screen (T12); the waveform and F12 | sound comes out of the machine sopen runs on |
| T23 | 09's page viewer and address window chrome: cells if X1a has landed, else egui moved in X6 | 09 §12 ships WV-D without waiting; the page itself is a region either way |
| T24 | XF lands after X6 and X7 (and so after X3 and X4) | message 23 asks for pictures and video in the terminal itself; a flip with them handed off falls short of it |
| T25 | Every printable ASCII character egui names a key for becomes `Event::Key` + `Event::Text` in the TUI, as egui-winit sends it; other ASCII and non-ASCII text (Hangul) stay `Text` alone (widened in round 2) | the key table matches key triggers only on `Event::Key` (`keys.rs:602-617, 1029-1031`); the viewers match letters as keys (`text.rs:3979`, `image.rs:3258-3268, 4829`, `mod.rs:1880-1882`); no table row matches a plain letter, and fields insert from `Text` |
| T26 | Ctrl = egui's `command` in the TUI on every OS; ⌘ only under the kitty protocol | ⌘ never reaches a terminal in legacy encoding; with a Mac's ⌃ as `ctrl` alone every Cmd row would be unreachable (`keys.rs:290-301`) |
| T27 | The help in both backends is one text: every command row as `Ctrl/⌘+…` (`Mods::prefix`, `describe`), one line on where ⌘ applies, every alias with where it works; it lists the rows this machine can press (`keys::rows`), so ⌘⌫ is absent off macOS (#107's review) | the help screen is 1:1 content; per-machine key names would differ, and a row that cannot be pressed would mislead |
| T28 | The in-process switch leaves the terminal (alternate screen and modes off); the remote switch keeps raw mode and the alternate screen, dims, deletes pictures and reads Ctrl+C; re-entry flushes pending input. At start, input before the first screen is drawn is dropped too: the probe's one round trip (§10's `─` and cursor report, the kitty query, DA1) reads keys typed meanwhile with the answers, and they are not replayed (XW-receipts' review, 2026-11-25) | the window owns the terminal's machine in one case and not in the other; keys typed meanwhile were meant for the window; at start nothing is on screen to type into, and a key read among the answers cannot be told from one |
| T29 | The remote switch's ssh runs with `BatchMode=yes` and `SSH_ASKPASS_REQUIRE=never`, HOST's stderr on the status row, one line per outcome | ssh cannot ask a question while the TUI holds raw mode; each end gets its own answer |
| T30 | Returning with unsaved edits asks HOST's close prompt there; a writing job makes it wait; input ending with unsaved edits keeps HOST's window open (01 A11 over 09 §7's end-of-input close, which applies only to a window with nothing to lose); Cmd/Ctrl+Q and +W are that window's close and return with the cue, as in one process (round 2); `--state-stdin` makes HOST's window a switched window | an edit is never dropped unasked; the shell already treats Q and W as the close (`gui/mod.rs:1174-1185`); the owner can still return from a window with no terminal |
| T31 | A local window that fails to start brings the TUI back unchanged, with an actionable line | the terminal was already left; the owner must not lose the screen |
| T32 | A reader accepts its own and every older state version; a newer one is refused with a line; each step that moves a screen adds its fields and bumps `v`; every running writing job makes the switch wait | the two ends update at different times; skew must say so; a writing job cannot be carried |
| T33 | Alt+G is the switch's keyboard alias in the TUI | it arrives under legacy encoding as `ESC g`; the table binds no Alt+letter, and with no text it never reaches a letter jump |
| T34 | Route B: the window backend owns winit's handler through `eframe::create_native`, one event loop per process; X1b measures the switch again | 04 §6.4.3 already needs it; XR-T measured `run_native` |
| T35 | Every forwarded window run passes `--gui`, from X2: the switch's always, `--target`'s when asked — and from XF also when the kind the sender sees needs a window (2026-11-26); an older HOST that refuses it at parse time gets the run again without it (2026-11-25) | after the flip a forward with `-t` would start a TUI inside ssh on HOST |
| T36 | One routing table (§11), first match in the rows' order: explicit mode, kinds routed elsewhere, terminals, a folder, destination, display | overlapping rules gave no answer for, e.g., an image on a terminal with no display and no target |
| T37 | 1:1 is defined under truecolor; a 256-colour terminal goes through one fixed table; the cell API has no modifiers | the oracle needs one answer per colour; modifiers would be a second, unchecked channel |
| T38 | In an ambiguous-wide terminal every ambiguous glyph from sopen's tables is substituted (§10); file content is laid in 2 cells | frames and cut names must hold; content must not change |
| T39 | `sopen --tui <file>` opens the TUI on its folder, the bar on it, its viewer open | `--tui` must have one meaning for every SOURCE |
| T40 | XR-T's X1 splits into X1a (the grid in the window, no TUI) and X1b (the terminal, the list's dialogs, OSC 52, the cursor at a field, kitty flag 1 by default); the translator is built in parallel from the start; tests move by one kit change; ratatui-core (the buffer only), not ratatui | the spike: panes host unchanged at cell rects, so X1a ships with no TUI; the dialogs are invisible only in a terminal; the kit re-point fixed the list's text queries at once; the buffer is all the core needs |

## 14. Acceptance

"Do X → observe Y"; the step that brings each item in brackets. **[owner]** = the owner's terminals
or hands (*tui* §13's probe, extended by these items).

**The screen**
- TS-1 [X1b]. The list, the F-key row and the F7 dialog with `새 폴더 2026` typed, the fixture with
  Korean names and U+1F3B5, 100×30, and again with F1's help open: cell dumps of (a) the terminal bytes
  through vt100, (b) a private tmux server's `capture-pane -e`, (c) the window's shapes laid onto cells
  → 0 character, foreground, background and modifier differences but the mode label; the caret equal;
  (d) the same bytes with `COLORTERM` unset → equal through §5's 256-colour table. macOS and Linux CI.
- TS-2 [X1b, owner]. The same in kitty and a herdr-app pane on the Linux laptop and a herdr pane on the
  Mac: kitty by its remote-control text dump [추정: not tried], herdr by a screenshot, cell by cell.
- TS-3 [X1a]. The window's screenshot: every background and every ≥ 2-ink-pixel glyph's foreground =
  the dump; regions masked, colour glyphs excepted (T18). TS-5 [X1b]: resized to 80×24 and 160×50,
  each dump = the window's at that size. TS-7 [X1a]: the projector lint stays within *tui* §1.3.
- TS-4 [each step]. Every moved screen passes TS-1 (a) and (c) — X3: text and hex viewers, a UTF-16
  Hangul file in view 1, the encoding list open; X4: lanes, the tree; X6: the chrome around regions.
- TS-6 [X3]. A pty emulator advancing ambiguous glyphs by 2, the list holding a name cut with "…" and
  a name with `é` → every §10 glyph substituted (`~` for the cut), no row broken, the oracle equal
  through the table; [owner] the probe's advances in each owner terminal. TC-1 [X1b]: `한글 문서
  초안.txt` takes the same cells in both (its 6 Hangul in 12); an emoji 2 cells, no ink outside them.
- TS-8 [X1a]. In the window, the list and title in cells and today's panes hosted at cell rects:
  the spike's three `q1_*` tests (clip and resize; the keyboard between the grid and egui fields,
  dialogs and the viewer; Tab and clicks to and from the git panel) pass and their negative control
  fails; the bar on a Hangul name is `ff5555` in every cell; the tree gets the clip check the spike
  lacked [M spike for the rest].

**The switch**
- TW-1 [X1b]. One process, a display, route B: F12 from the list with the F7 dialog open and typed into
  → the window's dump = the TUI's but the label; a folder made; F12 → the TUI lists it, the dialog as
  left; twice per run, first-frame times recorded; keys typed while the window was up do nothing back
  in the TUI. `[GUI]` and Alt+G = F12; closing returns; a window with no terminal shows no `[TUI]`. No
  display and no target → the status line; a HOST destination before X2 → its line.
- TW-2 [X2]. A stand-in ssh running HOST's half locally: state in, cue out, paths rewritten both ways,
  `BatchMode=yes` on its command line and `SSH_ASKPASS_REQUIRE=never` in its environment, HOST's
  stderr on the status row; Ctrl+C → the state from before; each of §7.3's outcomes (0 without the
  cue, 1, 2, 3, 128 + n, 127, 255, no exit code) → its line, the TUI stays;
  an unknown `"v"` → its line; a running copy → `waiting for…`, then the switch.
- TW-3 [X2, owner]. The owner's direction: the TUI in a herdr pane on the Mac shown on the laptop,
  `target = <the Linux laptop>`; F12 → the window there, same cells; a folder made; close → the Mac's
  TUI lists it; timed.
- TW-4 [X2]. An unsaved hex edit, F12 to HOST → the save question; Cancel stays; Save → switched after
  it lands. On HOST: edit, F12 → HOST's close prompt (Save · Save As… · Discard · Cancel); Cancel → the
  window stays; Save → back with the state; the same with Cmd/Ctrl+Q and +W → back with the cue.
  Edit, then Ctrl+C here, and again with the stand-in's link cut → HOST's window stays with its edit,
  this side restored with the kept or the lost-link line. In one process the edit crosses unsaved and
  saves from the window.
- TW-5 [X1b]. An injected window start-up failure → the TUI back with its state unchanged and the line.

**Pictures, keys, routing, leaving**
- TP-1 [X7]. The pick against scripted answers: kitty → kitty; DA1 with `4` → sixel; none → half
  blocks; `HC_TERMINAL_ID` → half blocks; `SOPEN_PICTURES=sixel` → sixel. TP-2 [X7, owner]: the
  probe's `--show` squares in kitty, herdr-app and herdr on the Mac — which drew; TP-1's table follows.
- TP-3 [X7]. A video in a pty → plays; with `SSH_CONNECTION` set, or `SOPEN_SCREEN=remote` → the first
  frame, the F12 line, no sound; `SOPEN_SCREEN=local` with `SSH_TTY` set → plays; help shows the value
  and its source. TP-4 [X6]: a region = its cell rect × (8, 16) pt; a dialog over a macOS web view
  hides it (`hitTest` at its centre is not the view). TP-5 [X3]: `index.html` opens on 3, `feed.xml`
  on 1, an image on 1 in a half-block terminal.
- TK-1 [X1b]. *tui* §4's table as a test (legacy and kitty bytes → commands): Alt+digits → F-keys and
  Alt+G → the switch under legacy, Ctrl+digits under kitty; Space selects in the list, marks in the git
  panel and sizes in the tree, in both encodings; Ctrl+R rereads on a macOS build; Ctrl+J under legacy
  → `ToHead`; the 17 legacy losses of §9 exactly, none under kitty; the Ctrl+Shift actions in the F10
  menu; the kitty flags pushed only after `CSI ? u` is answered. The spike measured the decode half on
  macOS (101/101 and 90/101 rows, the 17 losses, Ctrl+J → `ToHead`, and 21 browser steps with
  identical states under both encodings) [M spike]; the unit runs it on Linux too.
- TK-2 [X1b, owner]. In kitty, herdr-app and herdr on the Mac: a click on `[GUI]` and on the F-key row
  acts; which of F12, Alt+G, Alt+1…0 and Ctrl+1…0 arrive; the DSR round trip (information only);
  whether the herdr panes are viewed from another machine. TK-3 [X3, X6]: in the TUI `1` → view 1;
  F9 then `2` → Hex; in an editable hex view `1` is typed; `W` in the text viewer wraps and unwraps
  (X3); `P`, `F` and `H` act in the image viewer (X6); a Hangul name typed in the F7 field arrives
  whole, as `Text` alone.
- TF-1 [XF]. Each row of §11's table, in order: `--app`, `--gui` here without a display (exit 1),
  `--tui` with output to a pipe (exit 2), an address, a text file, a pipe (`SOPEN_LIST` prints, a
  `while read` loop is not eaten), a folder with and without a target, an image with a target, with a
  display, and **on a terminal with no display and no target** → the TUI's picture.
- TL-1 [X1b; the remote switch X2]. Exit, a test-hook panic, SIGTERM, SIGHUP and the in-process switch → the alternate screen
  left, modes reset, kitty flags popped, pictures deleted; the remote switch → still in raw mode and
  the alternate screen, pictures deleted, only Ctrl+C read (bytes checked).

## 15. Test plan

- **Tests query cells, by one test-kit change** (T10, unit XW-kit): the kit's text queries (`find_text`,
  `click_text`, `texts`, `text_starting_with`, `colors_of`, `rows_of`) answer from cells with the same
  signatures, plus a column/range query and runs that stop at frame glyphs. It lands once, before any
  screen moves; a later step rewrites only its screens' geometry asserts. Measured on the list and
  title [M spike]: with the list in cells and the kit untouched, 14 of 226 browser tests failed; the
  kit re-point fixed 9 and answered 17 list-text query sites in 11 tests with the same values; 6 sites
  were edited by hand (3 `framed` → `framed_right`, 2 geometry tests); 0 behaviour failures, 0 vacuous
  passes logged; the other 2,220 lib tests were untouched. For dialogs, the F-key and status rows it
  is [추정] (they add focus and button clicks). Kit cautions: a cell match is a substring of a row,
  looser than galley equality, so absent-text asserts weaken; cells keep no draw-call identity, hence
  the column query. The ≈75 pixel queries (`pictures`, `rects`, `segments`, `galleys`, `strokes`)
  become region rects in their screens' steps [M counts *tui* §11]. The Driver's input side stays.
- **Two harness tests per screen**: terminal bytes through vt100 = the buffer; the window's shapes
  laid onto cells = the buffer (§5), with §5's 256-colour and §10's substitution tables as oracle
  maps. **tmux** is CI's second emulator (`SOPEN_REQUIRE_TMUX=1`, as the other `SOPEN_REQUIRE_*`).
  **A terminal driver** plays legacy and kitty bytes into a pty (*tui* §4's `keyaxis`, the spike's
  key-row player), sending a typed character alone as a terminal does; the translator makes the
  events. **The remote switch** runs against a stand-in ssh (AGENTS.md: no ssh or network in tests),
  as 01 §4a's forward is tested today.
- **Receipts**: `SOPEN_CELLS=<path>` writes the grid (a row per line plus an attribute table) once
  `SOPEN_KEYS` have settled, then ends — both backends, one format for TS-1, TS-2 and the owner's
  receipts. It names a local path, so like §4a's other path settings it is not forwarded; a receipt
  on HOST sets it there. `SOPEN_SCREENSHOT` stays the window's.
- **Gate**: the loop's (`just check`, `SOPEN_REQUIRE_FFMPEG=1 SOPEN_REQUIRE_SFTP_SERVER=1`, the Linux
  clippy runs); a unit that changes a screen brings a cell dump and a screenshot someone looked at.

## 16. Migration order

*tui* §12 adjusted by T2 and T24 and by the spike: XR-T's X2 bundled "TUI by default" with the
cross-machine switch, so the switch stays early and the flip is its own last step, XF; XR-T's X1
splits in two (T40). Each step ships alone (prerelease) and none waits for a later one; tests move
once, by the kit (§15).

1. **X1a — the grid in the window, no TUI**: the cell core (`src/gui/cells.rs`) → the test kit's
   text queries answer from cells → the grid with today's egui panes hosted at cell rects → the
   list, title, status and F-key rows in cells; the neutral help text. Everything else is today's
   egui, at a cell rect. The spike measured this part: ≈0.94k product lines (cells 436, list and title
   216, `browser.rs` 53, the translator 231) plus 183 in the kit [M spike].
2. **X1b — the terminal**: the terminal backend (`src/gui/term.rs`: loop, layers, kitty flag 1 by
   default, mouse, OSC 52, the cursor at a field for Hangul composition, leaving and flushing), the
   list's dialogs in cells (F2, F5–F8, groups: egui and invisible in a terminal, fine in the window,
   so they gate X1b only), route B with F12 and Alt+G, `sopen --tui`. The translator
   (`src/gui/term_input.rs`) is built in parallel from the start. Viewers, tree and git panel are
   hand-off boxes in the TUI.
3. **X2 — the switch across machines** (HOST's half, the state and cues, `--gui` on every forward).
4. **X3 — the text and hex viewers, their previews, the hex window's questions and the binary
   dialog in cells**: 08's views in the terminal (VM-8; its encodings are cells since VM-7); the
   receipts `SOPEN_CELLS` and TS-6 (XW-receipts).
5. **X4 — the tree and the git panel in cells**: lanes as glyphs, details, dialogs, the planner.
6. **X5 — the editor (XR-E)**, cell-native; its left-panel directory/git tabs need X4.
7. **X6 — the image and media chrome in cells; pictures, video and the web view as regions.**
8. **X7 — terminal pictures and video**: the pick, the fast path, in-band frames, `screen` and
   `pictures`.
9. **XF — the flip** (T2, T3, T24), after X6 and X7: §11's rows 7 and 10 and §12's XF rows change,
   and XF turns on the forward's automatic `--gui` (`AUTO_GUI`, X2-gui; §7.3).

## 17. Work units

Sized by the spike in the real repo (branch `spike/tui-host`, 402fde1…15333c7, never merged; all three
seams PASS: egui panes hosted at cell rects, crossterm → egui as the neutral input, test breakage
0.9–2.2 % beyond the kit re-point on the list and title). Sizes are product lines; **[M]** = the spike's
`git diff --numstat`, **[추정]** = not built there. Each unit owns its files; a file shared by two units
passes from one to the next in the order given. The gate is §15's.

| ID | Unit | Owns | Size | After | Ships alone as |
|---|---|---|---|---|---|
| XW-0 | Scaffold: ratatui-core 0.1.2, crossterm 0.29, unicode-width 0.2 (the lock adds 20 packages, bumps none [M]); the `mod` lines | `Cargo.toml`, `Cargo.lock`, `src/gui/mod.rs` (`mod` lines only) | ≈10 | — | no change |
| XW-cells | Cell core: grid (`Grid::over`, `rect`), painter (background runs, a glyph per cell, the wide-glyph second-cell fix), layer record, cell API, read-back | `src/gui/cells.rs` (new) | ≈450 (451 [M]) | XW-0 | no change: nothing draws through it yet |
| XW-kit | Test kit: text queries answer from cells; a column/range query; runs stop at frame glyphs | `src/gui/testkit.rs` | ≈190 (+190 [M], 13 of them the spike's flip log, dropped) | XW-cells | no change: every test passes as before |
| XW-help | Neutral key names (T27): `Mods::prefix`, help's `aliases()` and `describe` | `src/gui/help.rs`; `src/gui/browser/keys.rs` (`prefix` and its tests only) | ≈60 [추정] | XW-0 | the window's help reads `Ctrl/⌘+…` |
| XW-grid | Grid and hosting: `panels_ui` splits the central area into cells (the odd column to the right), today's panes at `grid.rect(side)`, the Enter viewer in a scope at `grid.rect(all)`; 3 one-call test fixes (`framed` → `framed_right`) | `src/gui/browser.rs` (`panels_ui`, the viewer scope); `src/gui/browser/tests.rs`, `tests/s1.rs` (those calls) | ≈50 (≈46 [M]) | XW-kit | the window as today, panes 504 pt wide |
| XW-list | The list, title, status and F-key rows in cells; `panel_ui` deleted; 2 geometry tests rewritten | `src/gui/browser/list_cells.rs` (new); `src/gui/browser.rs` (after XW-grid); `src/gui/chrome.rs` (the rows' drawing); `src/gui/browser/tests.rs` (the 2 tests) | ≈220 (list and title 216 [M]; status and F-key rows [추정]) | XW-grid | **X1a**: the window's list in cells |
| XW-input | The translator (§4's rules) and its replays: the key-row table under legacy and kitty, the three test sequences and Hangul | `src/gui/term_input.rs` (new), `src/gui/term_input/tests.rs` (new) | 231 [M] + tests | XW-0; parallel to XW-cells–XW-list | no change: no caller yet |
| XW-shell | Route B: `create_native`, one event loop per process, `run_app_on_demand` per window; `FKeyInput`'s F12, Alt+digits and Alt+G; the start-up failure path (T31). Then `src/gui/mod.rs` passes to 04's KL-W4 | `src/gui/mod.rs` | [추정] | XW-0 | the window as today, its run re-measured (TW-1's times) |
| XW-dialogs | The list's dialogs in cells: the F2 field, F5–F8 modals, the group dialogs | `src/gui/browser/ops.rs`, `src/gui/browser/ops/`, `src/gui/browser/panel.rs` | [추정: where the spike puts X1's cost] | XW-list | the window's dialogs in cells |
| XW-term | The terminal backend (from the spike's 272-line runner): loop, layer drawing, kitty flag 1 by default, mouse, OSC 52, the cursor at a field, resize, leaving and flushing, the in-process switch; `--tui`; README and AGENTS rows. Carried from XW-dialogs: a dialog's shadow stays black cells here too, as the window draws it (§6, 1:1 for TS-1) — §6's dark-grey glyphs come to both backends together | `src/gui/term.rs` (new); `src/main.rs` (`--tui`); `README.md`, `AGENTS.md` | ≈300 [추정] | XW-list, XW-input, XW-shell, XW-dialogs | **X1b**: `sopen --tui` |

- **Order**: XW-0 → XW-cells → XW-kit → XW-grid → XW-list, with XW-help, XW-input and XW-shell
  in parallel from XW-0; XW-dialogs after XW-list; XW-term last. X1a ships with XW-list (and
  XW-help); X1b with XW-term.

X2 and X3 were cut on 2026-11-25, once X1b had merged (#129), from the code each unit touches.
Their sizes are product lines, all [추정], against what X1 cost **[M]** (the merges'
`git diff --numstat`). A unit that moves a screen to cells is sized at 1.4× the lines of today's
code it replaces **[S]**: XW-list's ratio (+384 product lines against −272, #119), with XW-dialogs's
dialog engine (774 lines, #125) reused, not built again. A unit of new code (X2's, XW-receipts,
VM-8) is sized from the nearest code that does the same [S]; XW-term came to 1,495 lines against
≈300 estimated (#129), so read those as floors. Tests came to 0.5–1.7 times the product lines
(#119, #125, #128, #129). X2 and X3 wait for no unit of each other and may run side by side; where
they meet in a file (`src/gui/term.rs`, `src/gui/mod.rs`, `src/gui/text.rs`, `AGENTS.md`),
whichever merges second rebases, as 08 §13's units do. A unit's few lines in `src/gui/mod.rs`
follow the same rule with 04's KL-W4, which holds that file after XW-shell.

**X2 — the switch across machines.**

| ID | Unit | Owns | Size | After | Ships alone as |
|---|---|---|---|---|---|
| X2-gui | `--gui` (§11 row 2, T35, as decided 2026-11-25 in Tensions): the flag — the window on the destination: here today's window, or with no display §7.1's `no display here …` and exit 1. Until XF a forward passes it only when the owner gave `--gui` (decided 2026-11-26: an automatic one would turn HOST's fallbacks without a display into exit 1, against §11 row 6); the automatic rule is in code behind `AUTO_GUI`, which XF turns on: for a window run, the file's kind needs a window (a folder, an image, video or audio, a binary, `--hex`), as the sender sees the kind — a run the hooks keep without a window (`SOPEN_LIST`, `SOPEN_AUTOSAVE`, `SOPEN_AUTOEXPORT`, a binary's `SOPEN_CHOICE=save` or `cancel`) is no window run (2026-11-26): a text file is forwarded without it and keeps the editor (row 5; `main.rs:726`); where the sender cannot see the kind before forwarding (it forwards before any stat today: `main.rs:567-585`), it forwards without it and HOST picks by kind. The switch's forward always passes it (X2-switch); never `--app` (row 1) nor an address (row 4: `main.rs:633`, the tunnel). HOST's exit 2 with clap's unknown-argument line for `--gui` as the first line it wrote on stderr (colours and blank lines aside) and nothing on stdout — clap's rejection at parse time, nothing run (2026-11-26) → the sender runs the forward once more without it and says on stderr that HOST's sopen is older, so a plain `--target` never breaks on an old HOST; HOST's output passes as it is read, only watched (2026-11-26). Its sentences in 01 §4a, the README and AGENTS.md | `src/main.rs` (`Cli`, `run_source`'s forward); `src/forward.rs` (`remote_words`, the retry, their tests); `README.md`, `AGENTS.md` (`--gui`) | ≈150 | — | `sopen --gui --target HOST DIR` runs `sopen --here --gui -- DIR` there, `sopen --target HOST DIR` and a text file `sopen --here -- …` as today (DIR with `--gui` from XF); an older HOST still opens it, with a line on stderr; `sopen --gui` with no display exits 1 with its line |
| X2-state | The switch's state, `"v": 1` (§7.4, T32): both panels' folders, bars, selections and scroll, the active side, the mode, each side's content (the tree open, a git pin), an open dialog with its typed text, caret and error, the open viewer (file, 08's view, place, encoding, wrap), the source host — taken from one `Browser` and given to a new one, as `SOPEN_STATE`'s `open=`, `at=` and `enc=` are handed on today (`browser.rs:794-805`); one JSON line; its own and every older `v` read, a newer one refused with §7.4's line; paths rewritten out and back (§7.3), as `forward::source_for` rewrites SOURCE (`forward.rs:205`) | `src/gui/browser/switch_state.rs`, `src/gui/browser/switch_state/tests.rs` (new); `src/gui/browser.rs` (taking and giving the state only); `src/gui/browser/ops.rs` (the open dialog's part only); `src/gui/mod.rs` and `src/gui/text.rs` (a `Window` getter for wrap only) | ≈500 | — | no change: nothing sends it yet (a round trip in its tests) |
| X2-host | HOST's half (§7.2, §7.3, T30): `--state-stdin` (hidden, with `--here --gui`) reads the state's line from its input and opens the browser at it in a switched window — F12, `[TUI]`, the close button, Cmd/Ctrl+Q and +W return; unsaved edits first ask that window's close prompt (Cancel keeps the window); a writing job makes the return wait — then prints `sopen-xtools-state <json>` and exits 0. The input ending closes the window only when it has nothing to lose (09 §7's rule, not built yet: the input watched as `--hold-until-eof` watches it, `main.rs:655`); else `sopen-xtools-kept`, the session no longer read or written, the window kept until it is closed there — checking §7.3's [추정] that a session without a pty sends its command no SIGHUP. The cues are fixed lines, as `sopen-tunnel-open` is (`url.rs:288`). Its sentences in 01 §4a. As built (`feat/x2-host`, with `Browser::would_lose` in `src/gui/browser.rs` and its tests in `src/gui/browser/tests/x2host.rs`, the module's line and `Window::logic` in `src/gui/mod.rs`, `chrome::clear_mode_button`): FOLDER is any folder SOURCE names, here or over SFTP — the switch sends one on its own machine as `<that machine>:/abs` (§7.3) and `give_state` takes only a state of the window's host, so a FOLDER on HOST alone could not serve the owner's direction; a file or an address, a state HOST cannot read, of a newer `v` or of another host, are exit 2 before any window (`switch_host::read_state`, `NotGiven`); no display is exit 1 before the state is read (an address is refused before that). The state is taken just before the browser's own close, so a viewer that close drops is in it; after a close prompt's answer the browser has dropped the viewer already, and the state came back at the list, the bar on its file — since X2-switch the state taken when the prompt came up is kept for the close its Save or Discard brings, so the viewer comes back in it (its Cancel, or a Save that does not land, drops that state: `Browser::viewer_stays`). A played F12 (`SOPEN_KEYS`) is the switch, as the owner's own. A writing job does not hold the window: it is waited for after it, as every browser window waits (`Leftover::finish`; a file operation's name-in-the-way question answered Cancel, `ops.rs:529`), and the cue comes after it; a refused save is the cue and exit 3, as any window's. The session can die while something still writes, before or after the return (review round 1: a write to its dead pipe panicked and cut short what still wrote), so the run's fds 1 and 2 are pipes relayed by a thread each to the session's streams, which the relay keeps aside and alone writes to (`relay_stdio`): a relay whose write fails drains, so nothing in sopen sees EPIPE. A relay is asked (fix-check: the kept cue could reach `/dev/null` before the relay wrote it) and answers once it has passed on all that is in its pipe now (children may hold a write end: no waiting for its end): letting the session go asks it to, then to close its stream (`Ask::LetGo`, at most 2 s, on the window's thread, once), and `main`'s end asks it to (`Ask::Flush`, at most 30 s, `CUE_WAIT`), so the state's cue reaches a peer slowed by its link; a dead session answers at once; a peer that keeps the session open and stops reading holds sopen's end 30 s, as it held its writes before the relays. `main` ends with `end_of_run` (a guard held from its first line, so a panic's message goes too; `gui::give_up` calls it before its exit): the flush, then fds 1 and 2 on `/dev/null` and the relays given at most 2 s to end. A child that inherited fd 2 and outlives sopen (the F-key watchdog under `SOPEN_LOG=debug`) is left with a pipe nobody reads; `debug!` drops a line it cannot write (`src/lib.rs`), so the watchdog's retries go on. At the input's end the session is let go in both cases (the relays close their streams, the input on `/dev/null`); the window is from then on one started directly — no `[TUI]`, F12 nothing — closed there through its prompt, no cue. The input's end is heard in the shell's `logic` (`Window::logic`), which eframe runs before each pass and alone while the window is hidden, so a minimised window closes or is kept as a shown one. A run a hook drives (`SOPEN_CELLS`, `SOPEN_SCREENSHOT`) is never asked about its close: it hands back the state of its last pass and leaves the input's end to its hook. §7.3's [추정] is measured: no SIGHUP without a pty (2026-11-26) | `src/main.rs` (`--state-stdin`; after X2-gui); `src/gui/switch_host.rs`, `src/gui/switch_host/tests.rs` (new) | ≈350 | X2-gui, X2-state | `sopen --here --gui --state-stdin -- FOLDER` with a state line on its input opens the window at that state and prints the cue when it closes: TW-4's HOST half |
| X2-switch | The TUI's half (§7.1's HOST, §7.3, T28, T29): `Place::Host`, a status line today (`term.rs:1368-1373`), runs HOST's half over 01 §4a's forward (its master, options, bare `exec` and quoting) with `-T`, `-o BatchMode=yes` and `SSH_ASKPASS_REQUIRE=never` (as the ssh exec layer and git's runner set them: `remote/exec.rs:23`, `git/runner.rs:140-141`), the state out on its input and the cue read from its output. Meanwhile raw mode and the alternate screen stay, the screen dimmed under §7.3's line, only Ctrl+C read; HOST's last stderr line is on the status row; each of §7.3's outcomes is its line, with the state from before unless a cue came. Before it switches: `waiting for <it>…` while a writing job runs, and with unsaved edits `Save the changes to host:/abs before switching to HOST?` in a cell dialog, 07 §10's four choices. TW-2 against a stand-in ssh; TW-4's TUI half; TL-1's remote-switch half. Its sentences in 01 A9, A10 and §4a and the README's "Open on another machine's screen". As built (`feat/x2-switch`; `src/gui/term/remote.rs`, `forward::Switch`): HOST's half runs as `ssh <the master's options> -T -o RemoteCommand=none -o LogLevel=ERROR -o BatchMode=yes HOST -- exec [env …] sh -c <the forward's script> sopen --gui --state-stdin -- FOLDER`, `SSH_ASKPASS_REQUIRE=never` in ssh's environment, never through `forward::run` (its exit 2 is §7.3's line, no retry). FOLDER is the rewritten state's active folder, so FOLDER and the state cannot disagree. The paths go out by `forward::source_for` and come back by the one pair of labels the folders went by — the terminal browser's and the one HOST's browser has for them — so the state comes back with the label `give_state` compares (`box:/x` with the target `me@box` goes out as `/x` and comes back as `box:/x`; `me@localhost:/x` as `me@<this machine>:/x` and back); a place on any other machine (a Copy / Move target) comes back by `forward::source_back`: HOST's own `/abs` is `HOST:/abs`, a loopback place there (`localhost:/p`, `127.0.0.1:/p`, `[::1]:/p`, `me@localhost:/p`) HOST too — `[its user, else HOST's]@HOST:/p`, as `source_for` names this machine's loopback the other way — and `<this machine>:/abs` `/abs`. Of `FORWARDED_ENV`'s 22 hooks the switch leaves here the 12 that script HOST's run or make it one without a window (`forward::SWITCH_LEFT_HERE`, a reason each: `SOPEN_STATE` — HOST's browser applies it on top of the given state —, `SOPEN_SCREENSHOT`, `SOPEN_AUTOSAVE` and its delay, `SOPEN_AUTOEXPORT`, `SOPEN_EXPORT_HOLD_MS`, `SOPEN_CHOICE`, `SOPEN_SAVE_TO`, `SOPEN_SAVE_HOLD_MS`, `SOPEN_SAVE_AS` and its hold, `SOPEN_LIST`) and carries the other 10; `SOPEN_WINDOW_SIZE` is the terminal's grid in points when the owner set none (T16, as the in-process switch's). Meanwhile the screen is printed whole once more, after one pass with no input (the question answered), every cell in the skin's shadow colours (dark grey on black), §7.3's line in a box over it, HOST's last stderr line on the status row (what HOST says is bounded: its standard output read for its exact cues alone — a line's `\n` and `\r` taken off each on its own, so a CRLF cue reads — other lines dropped, and a line past its bound dropped whole, never read cut: 64 MiB for a state's cue, which then says `HOST sent its state too long to read (over 64 MiB): the state from before`, 1 MiB for any other; of its standard error only the last line, cut at 4 KiB — never a queue); nothing of the terminal is left (no `LEAVE`, raw mode kept), and no protocol picture is drawn before X7, so none is deleted; a resize lays the browser out again under the dimming; any key but Ctrl+C is read and dropped. Ctrl+C closes ssh's input and the box says so; HOST's kept cue ends this run's ssh client at once (HOST let the session go, but the forward script's shell holds it open while the kept window lives); a second Ctrl+C kills this run's ssh client (its pid; never `ssh -O exit` on the master), else ssh's keepalives (about 30 s) end a dead link — X2-switch's decision, reversible. After Ctrl+C, an end with neither cue that is ssh's (255) or a signal's (no exit code) is the lost-link line; any other end is §7.3's table (a code it lacks: `sopen on HOST ended with status N: <line>`). A cue's state, read and rewritten back, goes to a browser made again from the terminal's place (`Destination::fresh`: `run_tui`'s target connected again, `browser_for`), `Browser::new` without `SOPEN_STATE` (so `run_tui`'s `open=` does not reopen its viewer over HOST's state), which takes the old one's place; what the old one still holds is settled at once (a viewer's claim on its file), and each browser the switch made is finished after the run, as `open_on` finishes the first. Before it goes (`Browser::before_switch`, `switch_due`, heard by the loop after every pass): `waiting for <it>…` — `the save of <file>`, `the file operation`, `the git action`, `would_lose`'s job branches as one predicate (`Browser::writing_here`), and a viewer closed while its save or export still writes (`the save or export of <file>`, its window in `Leftover`) — then the switch is asked for again and decided anew (unsaved edits made meanwhile still ask: only the question's answer lets them go). The question is one cell dialog, titled `Unsaved changes` as the viewers' close prompts are (or `Switch to HOST`), drawn at the pass's end over everything, its text holding the keyboard first as the delete dialog's does; the pass's keys, text, pastes, copies and cuts are taken out at its start and handed to it, and the keyboard the pass behind took (a hex grid, a git dialog's field) is given back, so nothing behind it acts; while it is up — from the moment it comes up, before any pass (`view_set::views_question`) — the view keys are a question's (`view_set::views_after`): no view digit and no hex view's Esc acts under it, and Esc is its Cancel. Unsaved edits: Save and Save As… press the viewer's own F5 and F7 as the owner's keys (the browser's key player) and the switch goes once the viewer is clean — still unsaved, no question up: `not switched to HOST: <file> still has unsaved changes` (a media window's Save As stops so too: its edits are unsaved until the file itself is saved; kept as decided); Discard lets it go with the file as stored, the viewer's edits held through the run and dropped by `Window::discard` — the hex, image and media windows' own Discard (the journal they recorded removed, nothing broader) — only once HOST's state has come back and been given (the old browser's viewer, as it goes), so the file opened here again offers nothing to restore; a HOST that never hands its state back (255, 127, 2, no exit code) leaves them here, the line ending `the edits to <file> are kept here` (coordinator's decision, X2-switch's review); Cancel stays. Two questions in a row (unsaved edits, then a git plan) keep both answers; a wait for a write forgets them, so after it everything is decided anew. The git panel's open dialog (`GitView::dialog_up`) or the planner's plan (`planner_model`), which the state does not carry yet (Tensions): `<what> is not carried to HOST: switching drops it.` — Switch · Cancel. Without a cue's state, after any end, the terminal lists again (both panels re-read, each bar where it was), as a browser given HOST's state does. A known limit: a link that drops without Ctrl+C is §7.3's 255 line, since nothing HOST says tells a window that was up from a HOST never reached (decided). X2-host's note checked and fixed (TW-4's HOST half, `tw4_on_host_cancel_keeps_the_viewer_and_save_returns`): Cancel kept the window with the viewer on screen and usable and F12 asked again, but after Save or Discard the browser's close had dropped the viewer before `switch_host::Switched::close_requested` took the state; HOST now keeps the state taken when the prompt came up and hands it back on the close the answer brings, the viewer in it (file, view, place), and drops it once the viewer stays (Cancel, a Save that does not land: `Browser::viewer_stays`) | `src/gui/term.rs` (the HOST arm of `to_window`); `src/gui/term/remote.rs`, `src/gui/term/remote/tests.rs` (new); `src/forward.rs` (the switch's words and its ssh run; after X2-gui); `src/gui/browser.rs` (the question and the wait only; after X2-state); `README.md` | ≈600 (the tunnel's run with its cues is 320 lines, `forward.rs:576-895` [S]) | X2-host | **X2**: F12 in `sopen --tui` with a target opens the window on HOST, and its close brings the TUI back with HOST's state; TW-3 is the owner's |

- **Order**: X2-gui and X2-state from the start, side by side (disjoint files); X2-host after both
  (`src/main.rs` passes to it from X2-gui); X2-switch last (`src/forward.rs` passes to it from
  X2-gui, `src/gui/browser.rs` from X2-state). X2 ships with X2-switch; TW-3 is the owner's receipt
  after it.

**X3 — the text and hex viewers, their previews, the hex window's questions and the binary dialog in
cells; 08's views in the terminal.** 08's screen half is cells already: VM-7 drew the views' strip
on the title row (`chrome.rs:805-832`) and F8's list of encodings as a cell dialog
(`view_set.rs:963-980`, #128). Each screen unit brings §6's look to its screens (§12's skin row).

| ID | Unit | Owns | Size | After | Ships alone as |
|---|---|---|---|---|---|
| XW-receipts | §15's receipts, which XW-term left out by decision: `SOPEN_CELLS=<path>` writes the grid — a row per line and an attribute table — once `SOPEN_KEYS` have settled, then ends, in both backends (the window's layers laid as `term::compose` lays them); a local path, so not forwarded. TS-6: at start the TUI writes `─` and reads the cursor report; two cells → §10's table, substituted as it prints, the oracle mapping through it. As built (`feat/xw-receipts`): the mode button is blank in both receipts (§5's one difference), so TS-1 is a diff of two files; the window takes its receipt from a whole pass, F1's help (painted at the pass's end) included; a receipt run reads no input in either backend and never switches the F-key mode; the start's probe is one round trip — `─`, the cursor report, the kitty query and DA1 — every answer read with the DA1 parsed; width A is python3's `unicodedata`'s — sopen's chrome (frames, scroll bars, the cut) gets §10's stand-ins and content of width A (a name's `é`, `♥`, `a─b`) is sent as written in two cells, the rest of its run moved whole into the blanks after its words (a run with none is cut with `~`), and a click maps back through that move; a receipt run ends on Ctrl+C (130), presses F1 for `help=1`, and its window is watched until the runner returns | `src/gui/term.rs` (the probe, the substitution, the dump); `src/gui/term/tests.rs`; `src/gui/mod.rs` (`Hooks`' `SOPEN_CELLS` and the window's dump only); `src/fnkeys/mod.rs` (`SCRIPTED`); `AGENTS.md` (its row, the hook-driven runs) | ≈300; built +966 −84 [M] (after its review) | XW-help2 (it rewrites ≈200 lines of `term.rs`) | `SOPEN_CELLS=<path>` writes either backend's screen as text, the receipt of every X3 screen; an ambiguous-wide terminal draws `+ - \|` frames, no row broken |
| X3-text | The text viewer in cells (§4, §6, T6, T38): the bar (the stats, the encoding whole, the title cut from the left); the rows — line numbers, highlighting a colour per cell, placeholder marks and C1's `·` dim (08 D28, D32), a right-to-left run reversed into the cells' order, which `directed_job` (`text.rs:6048`) asks egui to shape today [추정: Arabic loses its joined forms, as in a terminal]; wrap and the sideways scroll by cells (unicode-width in `wrap.rs`'s measure, no egui layout, no `glyph_width`: `text.rs:5681`); the scroll bar on the frame; the status row, Go to a cell field (`ops::dialog`'s); the notes; a preview's rows and its `preview of the first …` row. TK-3's `W` wraps and unwraps through the translator; TS-4 with a UTF-16 Hangul file in view 1, and with F8's list open. Carried from XW-receipts: CP437 letters reach the printer as content (it cannot tell them from a name), so §10's last-row substitution for an ambiguous-wide terminal is done at the source, where the viewer knows a cell is a CP437 glyph. | `src/gui/text.rs` (its screen, `bar` to `ui`: `text.rs:5392-6155`); `src/gui/text/wrap.rs` (the measure and the row cut: `wrap.rs:98-463`); `src/gui/text/tests.rs`, `src/gui/text/preview_tests.rs`, `src/gui/text/stream_tests.rs` (geometry asserts only) | ≈1,100 (it replaces 763 lines of screen and wrap's 51-line `Measure` [S]) | XW-receipts, VM-7b (its text-window hook) | Enter on a text file: the viewer in cells in the window, and in the TUI where a hand-off box stood; its preview's rows are cells, framed in the TUI once X3-preview lands |
| X3-hex | The hex viewer in cells: the Go to row (a cell field), the grid — offsets, the hex and ASCII columns, the cursor in its pane, changed bytes, rows the file does not reach, bytes still loading — the status row, the preview's rows; `own_keys`' F-key row in cells for a window on its own (`--hex`; the image and media windows get it too). Carried from #108's review: point the hex tests' `drawn()` (`hex/tests.rs:3612-3615`) at `texts()` once hex paints cells (`galleys()` then holds one-character glyphs), and note that `rects()` will list cell backgrounds and the caret. TS-4 with the hex view of a Hangul file. Carried from XW-receipts: CP437 letters reach the printer as content (it cannot tell them from a name), so §10's last-row substitution for an ambiguous-wide terminal is done at the source, where the viewer knows a cell is a CP437 glyph. | `src/gui/hex.rs` (`toolbar`, `status_bar`, `grid`, `ui`: `hex.rs:3730-3878, 3918-4268, 4528-4641`); `src/gui/hex/tests.rs`, `src/gui/hex/preview_tests.rs`; `src/gui/mod.rs` (`own_keys` only) | ≈850 (it replaces ≈615 lines [S]) | XW-receipts | view 2 of any file (1 of a binary) and `sopen --hex FILE` in cells; in the TUI the hex view where a hand-off box stood (its preview as X3-text's) |
| X3-questions | The hex window's questions and rows in cells, through `ops::dialog`: the close prompt (07 §10's Save · Save As… · Discard · Cancel), Save, Write in place; the rows below the grid (the notes, the keep row, the offer, left and recovery rows); Save As's modal, which the image and media windows share | `src/gui/hex.rs` (`recovery_rows`, `keep_row`, `close_prompt` to `offer_row`: `hex.rs:3270-3352, 3545-3572, 4269-4497`; after X3-hex); `src/gui/hex/tests.rs` (after X3-hex); `src/gui/save_as.rs` (`show`, `save_as.rs:652-756`), `src/gui/save_as/tests.rs` | ≈600 (it replaces ≈420 lines [S]) | X3-hex | the hex window's questions and every Save As in cells, in the window and in the TUI, where they were a hand-off box with its line |
| X3-preview | The preview side in cells: its frame titled with the file, and its `reading … %`, `…` and failure lines (`Host::ui`, `say`: `preview.rs:383-468`), for a preview that paints cells (text, hex) — a page file's too, which in a terminal previews as its source (VM-8: `Pages::here`), so the TUI never shows a page's hand-off box inside a preview frame; an image or media preview leaves its side to the hand-off box until X6, so the TUI never shows a box inside a frame. Until it lands, the egui frame's one-cell ring around a text or hex preview's cells is hand-off strips in the TUI (F-handoff-cover's symptom). As built (`feat/x3-preview`): `Host::ui` is given the side's cells of the panels' grid and the cells a dialog of the list's covers there (`Ops::covered`, as the list's side asks it); a preview whose window paints cells (`Window::paints_cells`, `gui/mod.rs`: the text and hex views answer it, a page file's source in a terminal among them) gets the side's frame in cells — single-lined, the path cut from the left as the list's title is — and paints its own cells inside it; the read's progress, `…` and why there is none are cells at the frame's top, wrapped by cells (`dialog::wrap`), and the cells under a dialog or the key help are blanked first. An image, a video, a sound, the info card (`prepare.rs`'s, egui's) and a page in a window keep egui's frame, their lines egui's too — one hand-off box in a terminal — until X6. The frame does not change while a file is prepared: a ready preview's own; before it is ready, the frame of the file's last ready preview (a refresh prepares it again), else its name's — egui's for a picture, a video or a sound by extension and, where pages show, a page file whose page is its default view by its name alone (`.html`, `.htm`: `kind::page_default`), cells otherwise. Where the name does not tell, the side changes frames once, when its first preview is ready: a name framed in cells whose preview is the info card (its time budget hit; a file missing, unreadable, special or a broken link), a link whose target is of another kind than its name, a `.ts` that is a video, a styled `.xml` (its page). Built +154 −31 product lines [M] (`preview.rs` +131 −27). Release frame cost of the side with a text preview, alone at 1000×700: 0.38 ms against 0.31 (the browser's frame 0.51 against 0.43; +238 shapes, the frame's glyphs: F-list-perf) | `src/gui/browser/preview.rs` | ≈120 (it replaces 86 lines [S]) | X3-text, X3-hex, 09's WV-D (+64 lines in `preview.rs`) | in the TUI a text or hex file under the bar previews in cells in its frame, its progress line too |
| X3-binary | The binary dialog in cells, through `ops::dialog`: the file's line, `[ Hex ]` and `[ Save to… ]`, the path field, the progress, the clash note; the window fitted to its rows | `src/gui/binary.rs` (`dialog`, `fit`, `ui`: `binary.rs:2164-2433`); `src/gui/binary/tests.rs` (geometry asserts only) | ≈380 (it replaces ≈270 lines [S]) | XW-receipts | `sopen file.zip`'s dialog in cells, in its window; a terminal reaches it only from XF (§11 row 10): its one caller is `main.rs:990-1023` |
| VM-8 | 08's views in the terminal (08 §9, T21; 08 §13's row): the view set opened for its backend (`open_paged`, `view_set.rs:281`, opened `Screen::GUI` or `Screen::NO_ENGINE` in both before it), keeping 08 §3's slots in both. Carried from the cut: VM-8 makes `Screen::TERMINAL` follow T21 — today it opens an image or a video on Hex, 08 §9's rule (`views.rs:186-190, 481-496`). The slots: an image and a video on 1, HTML, HTM and styled XML on 3 where 1 is Page (view 1 the hand-off box until X6 draws §8's one-row hand-off), plain XML and RSS on 1. The view keys through the translator, already given made releases (`view_set.rs:136`: `term_input::RELEASES`, which `view_keys.rs`'s module doc names): TK-3's `1` → view 1, F9 then `2` → Hex, `1` typed in an editing hex view; TP-5. 08 §8–§9's and 09 §8's sentences (§12). As built (`feat/vm8`, with `src/gui/browser/prepare.rs`): `Screen::TERMINAL` is the window's slots with pages not drawn, so `Screen::first_slot` is 3 for a page file there, and `Why::NoPictures` is gone (no screen opens a picture on Hex); in a terminal `Pages::here` opens a page file as its source and the preview previews its source, nothing of the page made (no server, no connection: a remote page opens on one), the class still found; the set opens with the terminal's screen, on 3, and `1` makes the page view (a hand-off box), `view=` as in the window. A page file's preview is handed over only in the backend it was made for: a switch makes it again, and a preparation Enter took over across one is opened again (`preview::Host::fits`), so a hand-over opens as a fresh open there; a page's preview in a terminal is its source's (X3-preview's row) | `src/views.rs`, `src/views/tests.rs`; `src/gui/browser/view_set.rs` (the screen at open); `src/gui/browser/tests/vm8.rs` (new) | ≈80 | VM-7b, 09's WV-D (both change `view_set.rs`; WV-D changes `views.rs`'s `Screen`) | in the TUI `index.html` opens on its source (3), where the window opens its page (1); an image opens on 1 in both; the view keys as in the window |

- **Order**: XW-receipts from the start (after XW-help2, which also makes `ops::dialog` reachable
  from the viewers), and VM-8 once VM-7b and WV-D have merged; X3-text, X3-hex and X3-binary after
  XW-receipts, side by side (disjoint files); X3-questions after X3-hex (`src/gui/hex.rs` and its
  tests pass to it); X3-preview after X3-text and X3-hex. `src/gui/browser/ops/dialog.rs`, the
  engine, is the one file they share: a unit that needs a part it lacks adds it there, and whichever
  of X3-text, X3-hex and X3-binary merges second rebases. Each ships alone; X3 is whole when the
  last of them has merged. X3 does not wait for F-list-perf: each screen unit records its release
  frame cost as #119 did (the list's 0.71 ms), and F-list-perf's paint (`cells.rs`) lowers them all
  at once.

X4 was cut on 2026-11-27, once X3 (#139, #143, #145, #149, #151) and X2 (#154) had merged, from
the code each unit touches. Sizes are product lines, all [추정], by X3's rule: a unit that moves a
screen is 1.4× the lines of today's code it replaces **[S]**, counted as the spans of the
functions that draw (the ones that only read keys left out); new code is sized from the nearest
code that does the same [S]. X2's and X3's merges say how far to trust that (`git diff --numstat`
over `src`, test files apart) **[M]**: the screens came in at or under the rule (X3-text +822 −605
against ≈1,100, X3-hex +518 −289 against ≈850, X3-binary +340 −216 against ≈380), a unit of
dialogs over it (X3-questions +1,170 −390 against ≈600), new code at two to three times its
estimate (X2-state +1,015 against ≈500, X2-switch +1,811 against ≈600). So a screen unit's size is
an estimate, and a dialog unit's or new code's is a floor; those units are cut well under 600.
Tests came to 0.9–1.7 times the product lines in the same merges. XR-T put the whole step at
≈6–9k lines changed (*tui* §12).

**X4-graph hands every other part its cells**, so the units after it own only their own files.
`draw` (`git_panel/mod.rs:2635-2933` when cut; as built `Layout` and `draw`, `mod.rs:2715-3033`) lays out the panel's cells: the banner's rows, the heads'
row (or Find's field), the rows, the details pane under them, the body that the log, the Backups
list or the planner take, the rule and the foot. It gives each part its cells and the panels'
grid, and every dialog the same grid. The panels' grid is the area between the title and status
rows (`browser.rs:2673`), where the list's dialogs stand too (`:3241-3246`, `:3261`), and some of
the panel's dialogs open from inside a part's draw (the log's entry, `log_view.rs:285-289`; the
planner's, `planner.rs:1605-1607`). A part or dialog still egui's draws at `grid.rect(cells)`
until its unit moves it. Each one says, from its own file, whether it paints cells and what of the
grid it covers (no cells, and nothing, while it is egui's); its unit changes those answers, and
nothing in `mod.rs`. The panel blanks the cells its own dialogs cover, and those under the list's
dialogs and the help (`Ops::covered`, `help::covered`), which `git_ui` hands it as the list's side
takes them (`list_cells.rs:185-192`) — so it does not miss the list's dialogs, as `text.rs`'s own
`covered()` does (F-text-covered). The other side's list, preview or tree blanks what the panel's
dialogs cover (`cells.rs:20-24`). Until its unit moves it, each dialog of the panel — the menu, an
action's dialog, the result, the stop question, the filters, the history search, a quick edit's,
the planner's, the log's entry — is an egui modal, which the TUI shows as a hand-off box where it
stands, its keys still reaching it and Esc closing it (`term.rs:592-633`). Each screen unit's
receipt is §15's: the window's `SOPEN_CELLS` equals the TUI's on that screen (TS-4), and the unit
records its release frame cost alone at 1000×700 against main's, as #119 and X3-preview did.
Today's hooks reach every screen the rows name (AGENTS.md "Receipts", 02 §9): `SOPEN_STATE`'s
`git=pin`, `select=`, `details=1`, `dialog=` (an action's id, `plan` the planner) and `help=1`;
`SOPEN_KEYS`' F10 (the tree, or in the panel its menu), `Cmd+F`, `Cmd+K`, `Cmd+L`, `Cmd+B` and
`text:`. Both runs read one fixture, made once: a backup's stamp is its time. From X4-tree on, the
browser's title-row clock is blank in both receipts, with the mode button. What still changes
between two runs — the log's times and durations — is checked in TS-4's harness, both backends
against one buffer in one test (§15). What egui's labels in the git panel's dialogs let one select
today (the result's stderr, the plan's command lines, the log's entry) cannot be selected in cells
until F-cell-select, as §6 says of the list's dialogs.

**X4 — the tree and the git panel in cells: lanes as glyphs, details, dialogs, the planner.**

| ID | Unit | Owns | Size | After | Ships alone as |
|---|---|---|---|---|---|
| X4-tree | The tree in cells (05 §1, §3, §6.1; §4, §6, T6, T38). Its frame is titled with the root. Each row is a cell tall: `▼`, `►` or `→`, the name two cells deeper a level and cut by cells, and the `· N hidden folders` row dim. At the right are the info line's three right-aligned columns with their tags and the scan's moving mark, one cell. Then the bar and the scroll bar on the border. Clicks, double clicks and the wheel go by cells, as the list's do (`list_cells.rs`). The cells a list dialog or the help covers are blanked first, as `Host::ui` does (`preview.rs:510-567`), and those a pinned git panel's dialog covers (a pinned panel stands beside the tree, `view.rs:152-154`): the second of X4-tree and X4-graph to merge adds X4-graph's `git_covered` to `tree_ui`'s blanking. `chrome::tree`'s look stays. In an ambiguous-wide terminal `▼ → · •` take §10's stand-ins at the source (T38): the printer cannot tell them from a folder's name (§10, as built). `►` and `∙` are narrow (python3's `unicodedata`) and stay. In a `SOPEN_CELLS` run `TreeSide::settled` (`browser.rs:643-655`) also waits until no row is scanning, so the receipt holds final sizes and no moving mark. `SOPEN_WATCHDOG` caps that wait: a screen that does not settle ends the run, exit 1 (AGENTS.md). Locally phase 3 scans every closed row on screen with no budget (`tree/scan.rs:13-14`), and a tree rooted at `/` holds `/usr` and the rest, so a receipt's tree is rooted at a small fixture: `HOME` is the fixture, and the root is the home (05 §2). Other hooked runs (`SOPEN_SCREENSHOT` among them) settle as today, the shot showing sizes still growing (AGENTS.md's `SOPEN_KEYS`). `SOPEN_CELLS` blanks the browser's title-row clock in both backends, with the mode button. The button stands right before the clock (`chrome.rs:946-968`), so `mode_cells` gives one rect from the button's place — kept blank in a window started directly — to the row's end, and `cells_text` keeps its signature (`term.rs:1185`; its callers `term.rs:1274, 1929`, `term/tests.rs:3017`, `text/tests/screen.rs:755`). The editor draws a clock of its own (`edit.rs:1625-1632`), which this leaves as it is. TS-4's tree. In `q1_panes_on_cell_rects_clip_there_and_follow_a_resize`, "the tree is egui's" (`tests/x1a.rs:216`) becomes the tree's cells. As built (`feat/x4-tree`): `tree::draw` takes the side's cells of the panels' grid and the cells a dialog of the list's covers there (`Ops::covered`, as `tree_ui` hands it; X4-graph's `git_covered` joins it there, whichever merges second), and blanks those and the key help's first. The stand-ins go through `term::narrow`, so §10's table stays in one place: `▼` `v`, `→` `>`, the hidden row's `·` and the mark's `·` and `•` `.` (`•` is CP437's, §10's last row); `►` and `∙` stay. The wheel's point deltas go by `CELL.y`, as the list's. In a `SOPEN_CELLS` run (`Browser::whole_sizes`, set in `open_on` from the hook, so the TUI, the window and HOST's switched window alike) the tree settles once it has no request on its way (`Tree::in_flight`: a listing, a filesystem's figure, a scan), not only once no row is being scanned: phase 3 starts a row's scan only once its listing has landed (`tree/scan.rs` `scannable`), phase 2 lists a screen above and below the screen too, and an open folder never scanned shows the sum of its subfolders' listings, so a listing below the last row still changes a size on screen (review round 1). A request reported not answering is left out — locally it never answers (its call is abandoned, and the model takes the report as its last), remotely it may not answer before `SOPEN_WATCHDOG` (it waits out its 30 s request timeout) — and its row is taken saying `not answering`; a row the rules never list unasked (05 §4 H3) asks nothing and is taken as it is; `SOPEN_WATCHDOG` caps the wait, exit 1 (AGENTS.md). The receipt's mask is one rect from the mode button's place — the clock's, where the row has no room for the button — to the row's end, 15 cells with the button: `chrome::mode_clock_rect`, kept beside the button's own rect (`mode_button_rect`, still the click's alone, so a click on the clock never switches). The tree's tests read cells by column and row (in cells `▼ src` is one run, so the kit's exact text queries cannot find a name after its glyph). Release frame cost of the tree alone at 1000×700 (300 rows, Hangul names, one in seven being scanned): 0.39–0.45 ms against main's 0.05–0.07 (one shape per glyph: F-list-perf). A known figure of the model, not this unit's: an open folder holding a compact chain reads `+` (the chain's head is neither open nor scanned, `tree/mod.rs:919-921`) | `src/gui/browser/tree.rs` (`draw`, `tree.rs:520-672`); `src/gui/browser/tree/tests.rs` (geometry asserts: `:515`, `:529`); `src/gui/browser.rs` (`tree_ui`, `browser.rs:2360-2383`, its line in `panels_ui`, `:2693`, and `TreeSide::settled`, `:643-655`); `src/gui/browser/tests/x1a.rs` (`:216`); `src/gui/term.rs` (`mode_cells`, `term.rs:1277-1280`); `src/gui/chrome.rs` (the button's place, `mode_button`, `chrome.rs:946-968`, beside `mode_button_rect`, `:938`); `AGENTS.md` (`SOPEN_CELLS`' "take the two in one minute" and its tree, and "the tree" in `sopen --tui`'s list of hand-off boxes, `AGENTS.md:200`); `docs/tui.md` ("the directory tree" among the boxes, `docs/tui.md:37-39`) | ≈250 (it replaces 149 lines [S]; the receipt's two changes ≈40 [추정]) | — | F10 in the TUI: the tree in cells where a hand-off box stood, and in the window too. With `HOME` the fixture, the window's `SOPEN_CELLS` equals the TUI's with the tree open on it: Hangul folder names, a hidden-folders row, a compact chain and a link, its sizes all counted. A `<SYS>` row stands only under `/` (05 §2, D-4), so the tree's tests prove it with a fake mount table (05 `T-norules`), and TS-4's harness a row being scanned, its mark moving. The tree's release frame cost |
| X4-lanes | Lanes as glyphs (§6, 03 §4.3–§4.4). Pure: it draws nothing. From `git::graph`'s node segments and the visible rows' pass-throughs (`Graph::pass_throughs`) it gives each Graph-column cell's glyph and colour. A lane's first cell holds its node or its line, its second the line of a lane change. For an ambiguous-wide terminal it gives §10's stand-in (`\| - +`, `#` for `■`, `.` for `≡` and `○`; `◙` and `»` are narrow). 03 §4.3's row stream already decides one row a commit, with no `git log --graph` rows between (`03-git-panel.md:369-379`; each segment joins two adjacent rows, `:416-417`). **Decisions to take in the unit**: the corners — §6 names `╭ ╮ ╰ ╯`, which are not in code page 437 and so not in the VGA font (`chrome.rs:17`), while 03 §4.4 and `chrome/git.rs:9` take every glyph from CP437, which gives `┌ ┐ └ ┘` (Tensions); the uncommitted row's dashed line to HEAD (03 §4.4), since CP437 has no dashed glyph; whether `├ ┤ ┬ ┴` stand where a lane change meets a pass-through (§6 names only `│ ─ ┼` beside the corners); which lane's colour a cell takes where two lines meet. Table tests: each of 03 §4.3's graphs (a line, a branch and its merge, an octopus, two tips sharing a parent, a criss-cross, a parent not loaded, `--first-parent`, the uncommitted row with HEAD far down, a root in the middle, the stash rows) written out as rows of glyphs | `src/gui/browser/git_panel/lanes.rs`, `src/gui/browser/git_panel/lanes/tests.rs` (new); `src/gui/chrome/git.rs` (the lane glyphs); `src/gui/browser/git_panel/mod.rs` (its `mod` line only); the sentences it decides: 03 §4.3's half-row diagonal, 03 §4.4's dashed line and its CP437 sentence, this document's §6 lane row | ≈200 (new code; `lanes_of`, 81 lines [S], is what it replaces in X4-graph; a floor) | — | no change: nothing draws through it yet |
| X4-kit | The dialog parts the git panel's dialogs need, in `ops::dialog`, the one engine every cell dialog shares (its `Part`s had none of these before it; they are `ops/dialog.rs:85-145` now). A row of choices, one of several or any of several, wrapped by cells: 03's `one_of`, `pick_name`, `pick_names`, `pick_optional` and `parent` (`dialogs.rs:1010-1149`), all `chrome::choice` today (`chrome.rs:521`). A message field of several rows, where Shift+Enter is a new line and Enter is the dialog's (`dialogs.rs:1064-1080`, the planner's `message_field`), its text laid at cells as `field_cells` lays one row (`ops/dialog.rs:1903-1941`). A list with a bar, its rows' keys at the right, scrolled with the bar (the menu, the filters' branches). Several fields and check boxes in one dialog, Tab between them (`Inputs` held one of each before it; `ops/dialog.rs:699-708` now, `by_id` holding the rest). **Decided in the unit** (X4-kit, reversible): a chosen choice reads as its mark, never as a colour — one of several `(•) word` against `( ) word`, the DOS look (`•` is code page 437's 7), any of several `[X] word` as the check box — since the bar's colours are the focus's (§6), a colour would make the focused choice look chosen, and `SOPEN_CELLS`' text rows say which is chosen; `•` is of ambiguous width, so an ambiguous-wide terminal gets §10's `.` there, at the source (T38). The message field shows the rows its caller gives (`Part::Message`'s `rows`: 4 for an action's message and 6 for the planner's, as today, `dialogs.rs:1072`, `planner.rs:267`), which give way with the rows that scroll on a short grid; its text wraps by cells at a blank, in a cell fewer than the field so the caret after a full row stays on it; past its rows it scrolls a row at a time to keep the caret's row in view whenever the caret moves or the text changes, and the wheel over it and ↑ ░ ↓ on the box's right border scroll it without moving the caret, as the report's rows (§6); a message opens at its top, its caret at its start, as an editor opens a file (egui's field put the caret at the end) — a dialog opened again under the same id too: a message field not laid in the pass before starts afresh, nothing to undo from before (the git dialogs reuse their fields' ids across actions). As built (`feat/x4-kit`): `Part::Choices`, `Part::Message` and `Part::List` (with `Row`), each control's state by its part's id in `Inputs::by_id` (`Input`: a text, a check box, one of several, any of several, a list's `Bar`); `Inputs::field` and `check` still serve the first field and check box `by_id` does not name, so every caller is as it was. Each choice is a control of its own and the list one, Tab going through them in the parts' order (egui's arrows move the keyboard among the choices). The kit's own keys — Space and Enter on a choice; a list's ↑ ↓ PgUp PgDn (a page its rows less one, as the tree's) Home End, Space and Enter — count once a press, in the pass's order, and go to the control that had the keyboard before the pass's first Tab, or to the one that Tab gives it to, a Space's text taken with it; Tab itself is egui's (two in one pass move it once). The list's bar is in the bar's colours whether or not the list has the keyboard; a click on a row, or Space or Enter, chooses the bar's row (`Shown::chosen`, the rows in order). The message field never types a plain Enter: the dialog takes it with exactly no modifiers (`keys::take`'s `Mods::None`), since egui's `consume_key(Modifiers::NONE, …)` would take Shift+Enter too. A pass that began on a field or a message field is the exception to the presses after a Tab: that field read the whole pass, a Space after its Tab too, before the Tab moved the keyboard on, so the control the Tab reaches takes no press of that pass (`[Tab, Space]` from a field types the blank and turns or chooses nothing); a field keeps every event it reads, as the list's dialogs' fields do. Check boxes named in `by_id` take the kit's presses as a choice does, so Space then Tab in one pass turns the one that had the keyboard (egui's click went to the next control); the first check box `Inputs::check` holds keeps egui's click, as the list's dialogs have it. A pass's cost does not grow with what a part holds: the message field keeps its text cut into rows (each row's bytes and where its drawn glyphs end, no glyphs) and the galley egui's engine reads, known by the text itself — kept and compared byte for byte, never by a hash two texts can share — and its room, so a pass that changes nothing cuts only the cells it shows (blanks hung past the field are never walked) and lays no glyph, and a change cuts and lays the text once, in work linear in its length (the galley made from egui's galley of one blank, its glyphs with no vertices: the engine paints nothing, a selection only adds its rectangle); a list's rows are `Rows`, shared and measured once when made (`Rows::new`), so a pass copies and measures none of them and draws only those it shows. Limits kept: a pass compares the text with the kept one byte for byte about twice (the engine's galley and the cells), and a selection makes egui copy the rows it spans on each pass it shows (its galley is shared with the kept one); egui moves the keyboard once a pass, by its first Tab or arrow (`[Tab, ↓]` from a choice loses the Tab); a message field not laid for a pass starts again at its top; choices do not give way on a short grid; a closed dialog's last message galley stays in egui's memory until a field of its id is laid again (about 70 bytes a character); egui's own field copies its text into its undo record on each pass it has the keyboard | `src/gui/browser/ops/dialog.rs`; `src/gui/browser/ops/tests/x4kit.rs` (new) and its `mod` line in `src/gui/browser/ops.rs`'s tests | ≈300 (new code; the nearest, the field's editor and `field_cells`, is 72 lines and the scrolling part's input 47 [S]; a floor) | — | no change: nothing uses the parts yet |
| X4-graph | The git panel's frame and graph in cells (03 §4.2–§4.4; §4, §6, T6, T37, T38). The frame is titled with the top level (`[filtered]`, a pin's repository). Then the column heads and the rows: the lanes through X4-lanes; the labels in 03 §4.4's order and colours, with `+N` where they stop; the subject and Author cut by cells (`cells::fit_start`, not `chrome::fit_start`'s egui measure); Date and Commit. Narrowing follows `Columns::new`, which already counts in cells (`graph_view.rs:91-140`). The bar; marked rows yellow; the label cursor with its label's colours swapped (03 §4.4's "inverted": no modifier, T37). The scroll bar on the border, the rule, and the foot: the paging line, Find's `k of n`, the ahead and behind at the right when they fit. A history search's flat list is the same rows without lanes. Clicks, double clicks, the right click (the translator's `Secondary`, `term_input.rs:185`), the wheel and the paging line's click go by cells. `git_wide` takes `grid.all()`. It hands the other parts their cells and every dialog the panels' grid, and blanks what they, the list's dialogs and the help cover (the paragraph above). `Browser::git_covered` (new, in `git_host.rs`) says what the panels' dialogs cover, for the list, the preview and the tree to blank. It moves Find's hint (`FIND_HINT`, `mod.rs:2575-2576`, read at `:841`) into `find.rs`, for X4-find. In an ambiguous-wide terminal `↔ ↑ ↓` take §10's stand-ins at the source, as the nodes and lanes do through X4-lanes; labels, subjects and names are content (T38). **Decision to take in the unit**: while an open part answers that it does not paint cells (Find's field, the banner, the details pane, the log, the Backups list, the planner, until their units), either the panel keeps egui's frame whole — one hand-off box in a terminal, X3-preview's choice for an image's preview — or it leaves that part's cells to `term::compose`, which makes a box of every cell no layer covers (`term.rs:635-663`), so a box stands inside the frame. Under the first, X4-graph keeps egui's path — `graph_view`'s egui `draw` and `lanes_of`, and `draw`'s egui layout and foot — and X4-state, which comes after every part's unit, deletes it; under the second, X4-graph deletes it. TS-4's lanes. **Decided (X4-graph, 2026-11-27, reversible): the second option** — an open part that does not paint cells has its cells left to `term::compose`, a box inside the frame, and egui's path (`graph_view`'s egui `draw`, `Cells` and `lanes_of`, `draw`'s egui layout and foot) is deleted here, so X4-state deletes none of it. Why: the graph, the panel's content, stays cells whatever part is open, where the first option turned the whole panel into one box each time Enter opened the details pane; and one drawing path is tested, not two kept in step. Its cost until each part's unit: a box needs three rows for its frame and the hand-off line (`hand_off`, `term.rs:817`: two rows show a frame alone, one a blank row), so in a terminal Find's field is a blank row and a one-row banner over it a frame with nothing inside; the status row (`FIND_HINT`, the banner's keys) and the foot (`k of n`) speak for them meanwhile. As built (`feat/x4-graph`): `draw` lays the parts out in cells (`Layout`, `git_panel/mod.rs:2715`; no font measured) and writes the frame, heads, rows, scroll bar, rules and foot into one buffer of the side's cells, painted once. Each part's draw takes the panels' grid, its cells and that buffer, and says from its own file whether it paints cells: `PAINTS_CELLS`, `false` in `details.rs`, `in_progress.rs`, `find.rs` (whose `field` returns its caret for the paint), `log_view.rs`, `backups.rs` and `planner.rs`. Each dialog takes the grid in its `ui` and answers `covers(grid)`, nothing while egui draws it: `HistoryMenu`, `filters::Dialog`, `ActionDialog`, `ResultDialog`, `SearchDialog`, `QuickDialog`, `PlannerView` (its own dialogs), `LogView` (its entry), and `stop_covers` beside `stop_question`. `GitView::covered` is their union; the panel blanks it, `under` (`Ops::covered`, from `git_ui`) and the help's; `Browser::git_covered` hands it to the list, the preview and the tree (a test-only `test_covers` proves the blanking until a dialog answers). A part egui draws has its cells reset in the buffer (no glyph, no colour): the details pane with the border cell right of them, where its egui scroll bar stands (`details.rs:522-525`), so the window still shows it; the log, the Backups list and the planner, which draw their scroll bar in their own last column, Find's field and the banner without, so the frame's `║` stays beside them and the banner over Find's row is one box (review round 1: the border was the log's, the Backups list's and the planner's too, so the window lost its `║` there). A panel too short for its parts (`Layout::of` `None`) still runs, at no cells, the parts that hold the keyboard — the planner, the log (its entry shown whole), Find's field and its bar — so no key or typing goes nowhere (review round 1), in a child `Ui` clipped to no cells, so nothing of them is painted or takes a click past the panel's (round 2: the Backups list's buttons stood on the frame's title row and Find's `Aa` left of the side); the Backups list reads no key of its own (`keys` → `run_list`) and is not drawn there. The pane's box is titled with the first text egui draws in its top line (`Commit`). A double click is the browser's `second_click` (`browser.rs:1017`), as the list's and the tree's: egui alone missed a double click right after a click on another row (this unit's test). `Label::joined` holds where a label's own `↔` stands, so an ambiguous-wide terminal gets `-` there and a `↔` in a name stays — only the shown start of a label is built, the joiner substituted in it, so a pass costs the room, not the name (review round 1); the foot's arrows are `^ v`. `git_ui` hands a panel the other panel's covers too (`git_covered(grid, Some(i))`, never its own, which it blanks as its pass leaves them), for two repositories' panels side by side. The wheel goes by `CELL.y`. Receipts (this Mac, `--here`, `SOPEN_CONFIG` an empty file, a private tmux): the fixture (a merge, a stash, the uncommitted row, local, checked-out, remote, joined, annotated and lightweight tag and stash labels, a Hangul author; a copy of it detached for `HEAD`) gives the same bytes from the window and the TUI over both sides (160×40, Cmd/Ctrl+Shift+G), with the details pane's box (160×40), and on a 90×30 grid, whose side drops Author and Date; beside the list (160×40, attached and detached) the one differing row is the list's foot, its free bytes moving between the two runs. TS-4's harness (`tests_x4_graph.rs`, `ts4`): the terminal's receipt and the window's of the graph — its lanes, labels and a Hangul author — are the same bytes at 100×30 and at 44×20, where Author and Date are dropped. Release frame cost of the panel alone at 1000×700 (3,000 commits in three to four lanes, labels, Hangul subjects and authors; `--no-default-features`, the same build of main and of the unit): 0.47–0.50 ms against main's 0.049–0.069 (one shape per glyph: F-list-perf) | `src/gui/browser/git_panel/mod.rs` (`draw`, `mod.rs:2635-2933`; the calls in `modals`, `:2054-2143`); `src/gui/browser/git_panel/graph_view.rs` (`Cells`, `draw`, `lanes_of`: `graph_view.rs:380-695` when cut; as built `Geometry` to `draw`, `graph_view.rs:362-621`, `lanes_of` gone); `git_panel/tests_x4_graph.rs` (its new tests), and the other units' `tests_x4_*.rs`, a module doc each, with every `mod` line (X4's rule, below); in `git_panel/tests.rs`, `the_graph_draws_s2cs_segments` and the bound on painted rows (`tests.rs:407-501`); the stand-in hosts' calls of the parts, `Host::ui` in `git_panel/tests.rs` (`:26-39`), `tests_s3.rs` (`:315-370`) and `tests_s4.rs` (`:384-443`), and those hosts and fixtures made `pub(super)` for the units' own files (`tests.rs:17, 46`; `tests_s3.rs:278, 377, 437-450`; `tests_s4.rs:345, 450, 544-564`); `src/gui/browser/git_host.rs` (`git_ui`, `git_host.rs:2316-2343` when cut; as built `git_ui` and `git_covered`, `git_host.rs:2320-2381`); `src/gui/browser.rs` (`panels_ui`'s `git_ui` calls, `browser.rs:2677, 2694` when cut, as built `:2695, 2714`, and the covers it hands the preview, `:2685`, as built `:2703-2704`; as built in `tree_ui`, `:2379-2381`; in `tree_ui` the one call of `git_covered`, if it merges after X4-tree); `src/gui/browser/list_cells.rs` (the covers the list blanks, `list_cells.rs:185-192`); in `details.rs`, `in_progress.rs`, `find.rs` (with Find's hint, moved from `mod.rs:2575-2576`), `log_view.rs`, `backups.rs`, `planner.rs`, `menu.rs`, `filters.rs` and `dialogs.rs`, the new signatures and their answers' stubs only; `src/gui/browser/tests/x1a.rs` ("the git panel is egui's", `:399`); "the git panel" in `sopen --tui`'s list of hand-off boxes (`AGENTS.md:200`) and in `docs/tui.md:37-39` | ≈550 added (1.4× the ≈355 lines [S] its cells take the place of: graph_view's `draw` and `put`, 215, and in `mod.rs` the ≈140 of `draw`'s 294 that lay it out and paint the foot [추정]; plus the parts' and dialogs' seams [추정]). Under its decision's first option it deletes none of them; X4-state does | X4-lanes | in the TUI the git panel — automatic (O6), pinned, and over both sides — in cells where a hand-off box stood, and in the window too. The window's `SOPEN_CELLS` equals the TUI's on a fixture with a merge, a stash, the uncommitted row, labels of every kind and a Hangul author, and again on a side narrow enough to drop Author and Date (03 §4.2). The panel's release frame cost |
| X4-pane | The details pane and the banner in cells (03 §6.1, §8.5). The pane stands under the graph, which keeps 8 rows at least, with the rule between. It holds the summary (`Commit`, `Parents`, `Author`, the dates, `In`), then the files — the change letter, the path, `+adds -dels`, `√`, the groups, an untracked folder's `►` row — then the message wrapped by cells. Tabs go to every eighth cell and control characters are escaped, as today, and the wrap is `dialog::wrap`'s by unicode-width, not egui's layout (`details.rs:342-371, 523-553`). Then the pane's bar and scroll bar. The banner's rows are in the warning colour, its steps `[ Continue ]`-style buttons (at an edit stop `[ Amend ]` and `[ Split ]` first), a click the step's key (`in_progress.rs:124-160, 334-387`). The buttons lose their hover, which named each step's key (`in_progress.rs:141, 365`); the status row names the keys while the banner shows (`mod.rs:843-859`), so nothing is lost. In an ambiguous-wide terminal `√` takes `.` at the source; paths and messages are content (T38). As built (`feat/x4-pane`): `details::draw` and the banner's `draw` and `draw_stop` write into the panel's buffer and answer `PAINTS_CELLS` true, so the panel leaves neither to egui and a terminal shows no box for them (`with_border` no longer applies). A field's name stands at the pane's first cell and its value at the fourteenth; a file's mark at the first, its letter at the third, its path at the fifth cut from its start (`cells::fit_left`, a path keeps its end) and `+adds -dels` ending at the pane's last cell, a blank before it; every text cut by cells. The pane's scroll bar is on the frame's right border beside its rows, as the graph's; its wheel goes by `CELL.y`, its double click is the browser's `second_click`, the pair keyed by the pane's subject and what the line shows — a file by its group and path, so a refresh that moves the files between two clicks makes the second a click of its own (review round 1); no panel in it, since egui counts no double click across more than 6 points (`max_click_dist`). The pane's lines and the banner's buttons have ids salted with their part's first column: two repositories' panels draw into one `ui` side by side, and with one id egui kept the right panel's rects for both — the left banner's buttons dead, a click on the right `[ Abort ]` Abort in both repositories (review round 1); the column alone, as the two sides always differ in it (`cells::halves`), while the pane's row moves when the banner's rows change (`Layout::of`) and egui gives a release to the id it pressed (`interaction.rs:118-123, 174-181`), so a row in the id lost a click across that change (fix check). Both sense clicks alone (`Sense::CLICK`): egui 0.37's `Sense::click()` is focusable, and a focused widget's Space or Enter is a click (`context.rs:1758-1764`), so Tab then Space would have pressed a step. The banner's first row is one more such rect under its buttons, so a click on the state or on the blank between two buttons presses none in any host (egui's hit test gives a click a few points off a widget to it when nothing is under the pointer, `hit_test.rs:220-245`; the browser's background had caught it until then). **Decided in the unit** (reversible): the viewed mark `√` keeps its light green on the bar, every other cell of the row the bar's text colour, as the graph's lanes and labels keep their colours — a mark is not text, and it stays a text of its own in `SOPEN_CELLS` (`browser/tests/s2.rs`'s `painted("√")`); a rename's `→` is the pane's own joiner, so an ambiguous-wide terminal gets §10's `>` there at the source, as X4-graph's `↔`, a `→` inside a path staying; the banner's `·` stays as written: its line mixes the state's words with a commit's subject (content), and §10 as built draws sopen's own text's `·` in two cells (the hint row's); a tab goes to the next eighth cell as the row shows it, a grapheme at a time — a Hangul glyph two cells, an emoji's sequence (`👩‍💻`, `❤️`) the two the buffer lays it in, an escaped control character its escape's length — where egui's path counted characters (review round 1: a count by characters put `👩‍💻\tx`'s `x` at the sixth cell); the message wraps by `dialog::wrap`'s rule in one walk (`details::wrap_cells`, a table test holding the two equal), since `dialog::wrap` measures what is left at each row: a one-line message of 1.5 MiB took more than 90 s in a debug build (03 §6.1 shows up to 1 MiB); a value is escaped and measured only up to its room (`fitted`), so `In` naming thousands of refs costs a pass its room; the banner's buttons are in the dialog's colours (black on cyan), as the hex view's buttons outside a dialog, and never take the keyboard (their keys are Cmd/Ctrl+Shift's); a blank of the panel's back stands between two and before the first — §6's two are for a dialog's buttons, on the dialog's own colour, where one blank would join them into one text; the hex view's rows of buttons outside a dialog keep two (`below_ui`, `x += w + 2`), which this departs from: with two an edit stop's four buttons (45 cells) leave its state 14 cells on a 1000-point window's side (61 inside), `stopped to ed…`, with one (42) they leave 18, `stopped to edit d…` — its commit id cut either way, where egui's narrower buttons (a word and two cells, one apart) left room for the whole id; the id stays on HEAD's row in the graph; the buttons come first, as egui's did: each that fits whole is laid from the right, the state cut before them, and only one that does not fit at all goes, from the left (Amend first). The stand-in hosts in `tests_s3.rs` and `tests_s4.rs` now paint their buffer (one line each, the line X4-lists and X4-planner need too). Beyond the row, as the flip required: `tests_x4_graph.rs`'s `a_part_egui_draws_is_a_box_inside_the_frame` (no box where the pane is; under the banner Find's row alone) and the buttons' texts in `browser/tests/s3.rs:641` and `browser/tests/s4.rs:555, 1503` (`[ Continue ]`, `[ Amend ]`). Receipts (this Mac, `--here`, `SOPEN_CONFIG` an empty file, a private tmux): X4-graph's fixture kind with the merge's message in Hangul and a renamed file with a Hangul name, `SOPEN_STATE='git=pin;select=HEAD;details=1'`, gives the same bytes from the window and the TUI over both sides (Cmd/Ctrl+Shift+G) at 160×40 and 100×30, and with the pane on the uncommitted row (`git=pin;details=1`: its `Not staged` and `Untracked` groups); a copy stopped at an `edit` of `git rebase -i` gives the same bytes with the banner over both sides at 160×40; beside the list (160×40) the one differing row is the list's foot, its free bytes moving. TS-4's harness (`tests_x4_pane.rs`, `ts4`): the pane on the merge at 100×40 and 60×30 and the banner at an edit stop at 100×30, the same bytes in both backends. Release frame cost of the panel alone at 1000×700 with the pane open (a merge of 300 files with Hangul paths and a Hangul message of 200 lines over the fixture's graph; `--no-default-features`, the same bench in a build of main and of the unit): 0.405–0.422 ms against main's 0.235–0.248 ms, whose pane is egui's (one shape per glyph: F-list-perf); a build of each in its own target folder, since cargo gives both the same unit and a shared folder takes the second for fresh | `src/gui/browser/git_panel/details.rs` (`wrap`, `draw`, `wrap_line`: `details.rs:342-553`); `src/gui/browser/git_panel/in_progress.rs` (`draw`, `draw_stop`); `git_panel/tests_x4_pane.rs`; in `git_panel/tests.rs` (`g_t1_enter_shows_the_summary_and_files`, `a_long_message_line_wraps_in_the_pane`, `message_tabs_stop_at_every_eighth_cell`, …), `tests_s3.rs` (`the_banner_shows_its_state_and_its_keys_and_buttons_proceed`) and `tests_s4.rs` (`an_edit_stop_offers_amend_and_split`); "the banner" and "the details pane" in `sopen --tui`'s lists of cells and hand-off boxes (`AGENTS.md:201`, `docs/tui.md:39-42`; X4-planner edits the same two lines for the planner) | ≈385 (it replaces 275 lines [S]) | X4-graph | Enter on a commit, a stash or the uncommitted row: the details pane in cells in both backends. A rebase stopped at a conflict: the banner and its buttons in cells. The window's `SOPEN_CELLS` equals the TUI's with the pane open on a merge commit with a Hangul message and a renamed file, and with the banner at an edit stop. Its release frame cost |
| X4-find | Find and the history search in cells (03 §4.7). Find's field stands in the heads' row: `ops::dialog`'s field at its cells, the `Aa` toggle after it, the bar jumping as one types (`find.rs:154-208`). The history search's dialog goes through the kit: message, author, text added or removed, path, since and until, the case toggle (`find.rs:260-335`). The search's flat list is X4-graph's. **Decision to take in the unit**: where the `Aa` toggle's meaning is said, since its hover (`case-sensitive`, `find.rs:204`) has no place in cells (§6): in the toggle's own label, or in Find's hint, which X4-graph has moved into `find.rs` | `src/gui/browser/git_panel/find.rs` (Find's hint too); `git_panel/tests_x4_find.rs`; in `git_panel/tests.rs` (`g_f2_typing_merge_finds_it`, `find_matches_labels_authors_and_hashes`, `history_search_shows_a_flat_list`, …) | ≈160 (it replaces 112 lines [S]; the dialog's half a floor) | X4-graph, X4-kit (several fields in one dialog) | a letter typed in the graph: Find's field in cells. Cmd/Ctrl+Shift+F: the search dialog in cells in both backends. The window's `SOPEN_CELLS` equals the TUI's after `Cmd+F` and a Hangul word typed, and with the dialog filled |
| X4-lists | The command log and the Backups list in cells, in the panel's body (03 §3.7, §8.2). The log: its header, an entry a row with the bar, the scroll bar, and the entry shown whole — every field, stderr whole and scrollable — as an `ops::dialog` dialog, whose texts, rows that scroll and button it has already (`log_view.rs:139-290`). The Backups list: its header, its groups and refs with the bar, the scroll bar, `[ Restore ]`, `[ Show ]` and `[ Delete ]`, and the keys' hint at its foot (`backups.rs:214-364`) | `src/gui/browser/git_panel/log_view.rs` (`whole_ui`, `draw`); `src/gui/browser/git_panel/backups.rs` (`draw`); `git_panel/tests_x4_lists.rs`; in `tests_s3.rs` (`the_command_log_lists_newest_first_and_enter_shows_an_entry_whole`, `the_backups_list_shows_groups_and_its_keys_and_buttons_act`) | ≈410 (it replaces 294 lines [S]) | X4-graph | Cmd/Ctrl+L and Cmd/Ctrl+B: the log, an entry shown whole, and the Backups list in cells in both backends. The window's `SOPEN_CELLS` equals the TUI's with the Backups list open; the log's entries carry each run's times, so its 1:1 is TS-4's harness. The release frame cost of the panel with the log |
| X4-menu | The menu and the filters in cells (03 §4.6, §7, §9). F10, Shift+F10 and the right click open the object's menu as an `ops::dialog` dialog with X4-kit's list: its items with their keys at the right, a commit's history row, the Repository item, the bar, scrolling with it (`menu.rs:565-637, 816-910`). Its width follows §6's rule for a dialog, the kit's, in place of today's 24 cells to 80 % of the window (`menu.rs:590-598`). Cmd/Ctrl+K's filters go through the kit's choices and list: branches with type-to-filter, remotes, tags, stashes, first parent, order, reflog (`filters.rs:272-373`) | `src/gui/browser/git_panel/menu.rs` (`Menu::ui`, `HistoryMenu::ui`); `src/gui/browser/git_panel/filters.rs` (`Dialog::ui`); `git_panel/tests_x4_menu.rs`; in `git_panel/tests.rs` (`g_f1_each_filter_changes_the_rows`, `the_branch_chooser_draws_only_its_visible_rows`), `tests_s3.rs` (`the_menu_draws_in_the_dialog_look_and_its_keys_choose`) and `tests_s4.rs` (`the_commit_menu_draws_its_history_row_and_gives_the_choice`) | ≈370 (it replaces 262 lines [S]; a floor) | X4-graph, X4-kit | F10 in the git panel: the menu in cells in both backends — where a legacy terminal reaches the Ctrl+Shift actions it loses (§9, T17). Cmd/Ctrl+K: the filters in cells. The window's `SOPEN_CELLS` equals the TUI's with each open |
| X4-dialogs | The action's dialog in cells, through the kit (03 §7, §8.1, §8.3). It starts with the line of what is lost, then the options, then the plan in a part that scrolls: Changes, Backed up first, Runs, the notes, a remote overwrite's warning. Then `[ Run ]` (off until the plan is in and nothing refuses it), `[ Cancel ]`, Rebase's `[ Plan the commits first… ]`, and `Enter runs · Esc cancels` (`dialogs.rs:809-962`). The forms here are 03 §7.6–§7.7's: the uncommitted row's and its files' actions, the repository's, its remotes', its name and email (`form_ui`'s arms from `Commit`, `dialogs.rs:1452-1580`), and a restore's entries with `moved since` and `replace` (`:1230-1251`, `:1581`). It also takes every dialog without options, whatever its section: those `has_options` names (`dialogs.rs:540-556`: Check out of a branch, Stage, Unstage, Delete on the remote, Drop of a stash, Discard, the conflict steps, Delete backup, the banner's steps) and the confirmations built elsewhere (`ActionDialog::confirm`, `:705-718`: the banner's steps, Delete backup, Fetch all). Every other action of 03 §7.1–§7.5 keeps today's egui dialog until X4-dialogs2. **Decision to take in the unit**: how many of the plan's rows stand before it scrolls (today 60 % of the window's height, `dialogs.rs:837`; §6 cuts a dialog's texts to the room between the title and status rows). **Decided (X4-dialogs, 2026-11-28, reversible):** at most three fifths of the panels' grid's rows and eight at least — egui's 60 % of the window and its eight rows, in whole cells — and fewer where the grid leaves less, the kit's rows that scroll giving way first, so the box never stands past the title or the status row. As built (`feat/x4-dialogs`): `ActionDialog::in_cells` is every dialog without options and Commit, Amend, Stash, Reset the changes, Remove untracked, Fetch's form, Fetch, Prune and Delete a remote, Add and Edit a remote, Name and email, Remove name and email, and a restore; `form_ui` keeps the other arms, egui's. The box is 72 cells inside, egui's width, widened by §6's rule to its widest text (76 at most): the lost line, a blank row, the options — the fields, a message field of four rows, an option on or off a `[X] word` of a row of any of several (two in one row where egui had them side by side), a mode, a remote or a scope a row of one of several — a blank row, the plan in the kit's rows that scroll (each heading in the weak colour, its lines two cells in and wrapped there, a long command line going on two cells in), a blank row, `[ Run ]  [ Cancel ]` (and Rebase's `[ Plan the commits first… ]`, for X4-dialogs2's form) and the hint in the weak colour. A restore's entry is a row of its own, `[X] <subject> → <value then>` with an occupied path's `[ ] replace` beside it, its `moved since (now …)` and the occupied path's rule four cells in under it; in an ambiguous-wide terminal the entry's `→`, sopen's own between a name and a value, is §10's `>` at the source (T38, as the panel's `↔` in a label), while the plan's lines — the planner's text, names and paths in it — and the hint's `·` are drawn as written (two cells, the run moving into the blanks after it, as the list's dialogs' hints). The plan's part is `plan_rows`, `plan_wants` and `plan_part`, for X4-quick's confirmation too. The keyboard opens on the form's first text field (Commit's or Stash's message, a remote's or the user's Name), or on nothing, and that field takes it again when it shows anew (Amend's message once "edit it" is chosen) or while nothing has it. Enter runs (03 §8.1, the hint's words; E6's opening on Cancel is the editor's rule for a prompt that holds a Save or a Discard), but each Enter of a pass goes to what has the keyboard at that point — before the pass's first Tab what had it then, after it what the Tab gave it to — so Enter on a focused button is its press, and an Enter after a Shift+Tab or an arrow that moves the keyboard in the same pass (egui moves it on the next pass, or at the pass's end) is dropped, the owner pressing it again on what now shows the keyboard — a Space after one too, with its text, which the button or choice the keyboard leaves would take. Only what the screen showed runs (round 1, fix-check): each pass keeps what it painted — the draft, the plan's rows, whether Run was on — and a Run or an Enter runs only when what would run at the end of its pass is what the pass before painted, the frame the owner saw; so an option or a typed character in that pass or the one before, a refresh's plan or reads landing in it, and the dialog's first two passes (the first laid out unseen) run nothing, the hint then saying `the plan changed — Enter runs it · Esc cancels` while Run is on; the next Enter runs what shows. What runs is compared with the painting of the last pass that reached the screen — the terminal prints no pass while input waits, and says so after each (`cells::set_presented`; a window shows every pass) — kept in `cells::OnScreen` (its `start` at a pass's start gives what the screen shows, `painted` records the pass's painting), so a pass the terminal never printed is never what runs, and one that changed nothing (a click's press, whose release comes in the same read) leaves the click or Enter after it to run what still shows (reviews of X4-find and X4-lists). After a second move of the keyboard in a pass (a Tab, a Shift+Tab or an arrow that moves it: egui moves it once a pass, its last direction) every later Enter of the pass is dropped, and every later Space unless the pass began on a text field (review of X4-find). A Space after a Shift+Tab or an arrow is dropped only where the pass began on a control that is not a text field: a field types it, and the kit already keeps the control the keyboard reaches from pressing it (review of X4-find). A box the grid holds less of than all (its choices, fields and buttons never give way: a restore of many entries) says first, in the error colour, `the window is too short for all of it: make it taller to run it`, and its Run and Enter are off until it fits (`Layout::fits`, new in `ops/dialog.rs`); Esc still cancels. Esc, `[ Cancel ]` or a click outside cancels, and a pass that both cancels and runs cancels. The buttons are named to the kit (`Input::Button`, new in `ops/dialog.rs`), so Space on them counts once a press in the pass's order as a choice's: `[Tab, Space]` from a field types the blank and presses nothing, `[Space, Tab]` on Run runs and never presses the Cancel the Tab reached (F-kit-tab, for these dialogs; the list's dialogs' buttons and first check box keep egui's click). `covers` is the kit layout's box and shadow, from the dialog as it stands before its pass. Limits kept: a restore's entries do not scroll yet (the kit's choices do not give way), so one of more entries than the grid holds runs only on a taller window — for the owner: a `files` restore has an entry per path, so one of more untracked paths than about the grid's rows less twelve cannot run in that window until the entries scroll; a pass whose refresh changes the plan draws the new box over what the panel blanked for the old one, that pass only. Receipts (this Mac, `--here`, `SOPEN_CONFIG` an empty file, a private tmux, 100×30): the fixture (`기능` checked out, the remotes `origin` and `백업`, a staged Hangul file, and a backup group written once by deleting the merged `이전`) gives the same bytes from the window and the TUI with F7's Commit dialog typed into in Hangul, with the restore's dialog (Cmd/Ctrl+B, then Cmd/Ctrl+Z on its group), and with Fetch's form and Edit a remote's open | `src/gui/browser/git_panel/dialogs.rs` (`Dialog::ui`, `plan_ui`, `lines`, `restore_ui`, and `form_ui`'s §7.6–§7.7 arms); `git_panel/tests_x4_dialogs.rs`; in `tests_s3.rs` (`the_dialog_shows_every_part_of_the_confirmation`, `every_option_change_plans_again`, `a_restore_never_writes_over_anything_unless_chosen`, `shift_enter_is_a_new_line_in_a_message_and_enter_runs`, the masked-URL tests) | ≈410 (it replaces 290 lines [S]; a floor) | X4-graph, X4-kit | F7 on the uncommitted row: the Commit dialog in cells in both backends, its message typed in Hangul; Fetch's, a remote's and a restore's dialogs too. The window's `SOPEN_CELLS` equals the TUI's with the Commit dialog typed into, and with a restore's dialog. Still egui's in this release, a failed action's result dialog and the planner's message dialog in `sopen --tui`: each a hand-off box where it stands, its keys reaching it, Esc closing it |
| X4-dialogs2 | The other actions' dialogs in cells. 03 §7.1–§7.5's forms: a commit row's (tag, branch, move, checkout, pick, revert, merge, rebase, reset, archive), a local, a remote and a tag label's, a stash row's (`form_ui`'s arms up to `StashBranch`, `dialogs.rs:1253-1451`). The result dialog, each step and the error scrollable (`:1686-1735`). The stop question (`mod.rs:2497-2521`). Then the egui dialog and the forms' egui parts (`dialogs.rs:1007-1206`) go. `dialogs::width` goes as X4's rule says (below) | `src/gui/browser/git_panel/dialogs.rs` (after X4-dialogs); `src/gui/browser/git_panel/mod.rs` (`stop_question` only); `git_panel/tests_x4_dialogs2.rs`; in `tests_s3.rs` (`the_result_dialog_lists_each_step_and_the_error`, `the_result_dialog_masks_credentials`, `move_tag_keeps_the_chosen_tags_own_message`, …) | ≈380 (it replaces 271 lines [S]; a floor), less the ≈200 egui lines it removes | X4-dialogs | every action's dialog, its result and the stop question in cells in both backends: in the TUI no action dialog is a hand-off box. The window's `SOPEN_CELLS` equals the TUI's with Merge's and Push's dialogs, and with the result of a step that fails, writes no backup and changes nothing — Check out of a branch, refused over a local change to a file it changes — since a result after a backup names its stamp (`dialogs.rs:1726-1728`) |
| X4-planner | The planner's pane in cells, over the panel's body (03 §7.8.2): its rows newest at the top — verb, hash, subject, author, date, the `A` mark — the bar, the scroll bar, the note in the error colour, and the keys' hint at its foot (`hint`, `paint`, `draw`: `planner.rs:1339-1616`). Its keys stay as they are (`keys`, `:1014-1077`), read a press at a time in the pass's order (as built, below). **Decided in the unit** (X4-planner, reversible): the columns stand two blanks apart — the verb (6 cells: `reword`, `squash`), the `A` mark, the hash (8), the subject, and at the right end the author and the date (16 each, left out where the subject would keep fewer than 10 cells) — where egui's stood one apart, since one blank joins two columns into one text (a `SOPEN_CELLS` row read by eye, the test kit's runs), so the mark and the hash read as texts of their own; the scroll bar stands on the border beside the rows, as the graph's, the tree's and the list's, where egui's took the pane's last column; `[ Run… ]` and `[ Close ]` are black on cyan, as egui's buttons were on the panel's body (`chrome::visuals`' inactive widget), the bar's colours when one has the focus (§6), two blanks apart and the hint two blanks after them, a button drawn and clicked only where it fits whole; in an ambiguous-wide terminal the hint's own `↑↓` and `·` take §10's `^v` and `.` at the source (T38) — unlike the graph's foot, whose `·` stays as written, the hint is cut to its row and leaves the printer no blank to move a glyph laid in two cells into — while subjects and names are content. As built (`feat/x4-planner`): `paint` writes the pane into the panel's buffer at the listing's cells, `PAINTS_CELLS` is `true`, and nothing in `mod.rs` changed but the `with_border` comment; texts are cut by `cells::fit_start`, control characters shown; a click on a row's cells puts the bar there and the wheel over the rows goes by `CELL.y`, as the graph's; in a panel too short for its layout the pane runs at no cells and paints nothing, its keys still working. The keys are the same keys, now read one press at a time in the pass's order (`PaneKey`): before, each key counted once a pass and the typed letters came after every arrow (`keys` at 9364048), so two ↓ in one pass moved the bar one row (this unit's test) and `r` typed before ↓ set the verb of the row below (read from that code); a key that opens a dialog of the pane's — Enter on a reword or squash row, `A`, Cmd/Ctrl+Enter — ends them for the pass, and every event after it in the pass, keys and text in their order (the rest of a text that key was typed in too), stays for that dialog, which `draw` shows in the same pass: it takes them as if they were typed once it was open, so Enter and a message typed in one pass, or in two, put the message into the message field, as the code at 9364048 had it (read from that code), and `a` then `x` put the `x` at the end of the author's name (review round 1); Enter, `x` and Enter in one pass keep the message with the `x`. A dialog of the pane's opens with each field's caret at the end of its text (`carets_at_the_end`): egui keeps a field's caret by its id, one for every row's dialog, so the caret left in one row's message stood in the middle of the next row's (review round 2). Left as it was until X4-planner2's dialogs, through the kit, take presses in the pass's order: the egui dialog takes its own Enter and Esc before its field reads the pass's text, so text typed after them in the same pass is read as typed before — Enter, Enter and `d` keep the message with the `d`; Enter, Esc and `d` keep nothing and drop the `d`; `a`, Enter and `d` set the author `Ann Leed` (this unit's tests pin each). The planner's own dialogs are still egui's: a hand-off box where each stands in a terminal (X4-planner2). Receipts (this Mac, `--here`, `SOPEN_CONFIG` an empty file, a private tmux): on a fixture of seven commits with a Hangul subject and Hangul and Latin authors, `SOPEN_STATE='git=pin;select=HEAD~6;dialog=plan'` and `SOPEN_KEYS='Cmd+Shift+G,text:d,Down,text:r,Down,text:e,Down,text:s,Down,text:f'` (every verb) give the same bytes from the window and the TUI at 160×40; TS-4's harness (`tests_x4_planner.rs`, `ts4`): the same bytes at 100×30 and at 50×20, where the author and the date are dropped, with the `A` mark. Release frame cost of the planner alone at 1000×700 (300 rows, Hangul subjects and authors, a verb of each kind; `--no-default-features`, the same bench in a build of main and of the unit, run in turn): 0.47–0.49 ms against main's 0.18–0.20 (one shape per glyph: F-list-perf) | `src/gui/browser/git_panel/planner.rs` (`hint`, `paint`, `draw`; as built also `keys` and `PaneKey`); `git_panel/tests_x4_planner.rs`; in `tests_s4.rs` (`the_planner_shows_each_rows_verb_hash_subject_author_date_and_mark`, `the_bar_and_the_rows_move_with_their_keys`, `typed_letters_set_verbs_and_never_start_find`, and as built the stand-in host's paint of its buffer, `tests_s4.rs:428-430`, X4-pane's line word for word); in `tests_x4_graph.rs`, the planner's case of `the_log_backups_and_planner_leave_the_frames_right_border` (`tests_x4_graph.rs:1044-1051`); the comment of `with_border` (`git_panel/mod.rs:2766-2767`); the planner among the cells of `sopen --tui` (`AGENTS.md:201`) and of `docs/tui.md:38-45` | ≈370 (it replaces ≈265 lines [S]) | X4-graph | Cmd/Ctrl+E: the planner in cells in both backends. The window's `SOPEN_CELLS` equals the TUI's with a plan holding every verb and a Hangul subject. Its release frame cost |
| X4-quick | The quick edits' dialogs in cells, through the kit (03 §7.8.1): reword's message, squash's two messages joined, author's name, email and dates with the committer date's choice; the plan's confirmation as X4-dialogs shows an action's (`QuickDialog::ui`, `planner.rs:664-722`). The egui form parts it used (`:245-428`) stay: the planner's own dialogs still use them (`inner_ui`; `run_buttons` at `:1300`), and X4-planner2 deletes them. **Decided in the unit** (X4-quick, 2026-11-28, reversible): the message field shows six rows, as egui's did (an action's shows four), so squash's two messages show their subjects and bodies at once; it opens with its caret at its start and its first rows shown, the kit's rule for every message field (`ops::dialog::message`, as an editor opens a file), where egui's stood at the end — a reword most often changes the subject, its first line (the planner's own message dialog still opens at its end, `carets_at_the_end`: a tension X4-planner2 settles as it moves that dialog to the kit); each one-line field (author's) opens with its caret at its end, as egui puts a new field's; and every field opens afresh, never where an earlier quick edit's dialog left its caret under the same ids (egui kept it there, the class X4-planner's review round 2 found in the planner's dialogs). As built (`feat/x4-quick`): `QuickDialog::ui` is X4-dialogs' box through the kit for every quick edit and every confirmation `QuickDialog::confirm` builds (an edit stop's Split, a split's Continue): the lost line, the options — `Message` and its field, or `Name`, `Email`, `Author date` and `Committer date (•) keep  ( ) now  ( ) same as the author date` — the plan in the kit's rows that scroll (`plan_rows`, `plan_wants`, `plan_part`), `[ Run ]  [ Cancel ]` and the hint; `covers` is its box and shadow. The first field has the keyboard as it opens; each Enter of a pass goes to what has the keyboard at that point (on `[ Cancel ]` it cancels), one egui cannot follow is dropped, the buttons take the kit's presses, and only what the screen showed runs — the edit, its title, its lost line, its plan's rows and whether Run was on, as the last pass that reached the screen painted them (`cells::OnScreen`) — else the hint says `the plan changed — Enter runs it · Esc cancels`; a box the grid holds less of than all says so first and does not run. `take_enters` is a copy of `dialogs.rs`'s, private there while X4-dialogs2 holds that file: whichever of the two merges second makes that one `pub(super)` and deletes the copy. Limit kept, as X4-dialogs': a pass whose refresh changes the plan draws the new box over what the panel blanked for the old one, that pass only. Receipts (this Mac, `--here`, `SOPEN_CONFIG` an empty file, a private tmux, 100×30; a fixture of four commits, one with a Hangul subject and body by a Hangul author): the same bytes from the window and the TUI with F2's Reword and F8's Drop on a commit, with Reword's message replaced by a Hangul one (`dialog=reword`, `SOPEN_KEYS='Cmd+A,text:…'`), and with the history row's Author and date and Squash (`dialog=author`, `dialog=squash`) | `src/gui/browser/git_panel/planner.rs` (after X4-planner); `git_panel/tests_x4_quick.rs`; in `tests_s4.rs` (`every_quick_edit_opens_its_dialog_with_its_plan`, `the_shown_confirmation_is_the_edit_enter_returns`, `a_refused_quick_edit_never_runs`, `messages_are_kept_byte_for_byte`) | ≈235 (1.4× the 167 lines [S] of `QuickDialog::ui` and the parts it uses, none deleted here; a floor) | X4-planner, X4-dialogs (its plan's part) | F2 and F8 on a commit (Reword, Drop) and the menu's history row: their dialogs in cells in both backends. The window's `SOPEN_CELLS` equals the TUI's with Reword typed into |
| X4-planner2 | The planner's own dialogs in cells (03 §7.8.2): a row's message, the author and dates, the run's confirmation with `--autostash`, `--update-refs` and the checks that fail, and Esc's question when something changed (`inner_ui`, `planner.rs:1079-1324`). Then the egui form parts go (`line_field` to `type_into`, in `planner.rs:245-428`) | `src/gui/browser/git_panel/planner.rs` (after X4-quick); `git_panel/tests_x4_planner2.rs`; in `tests_s4.rs` (`enter_edits_the_message_of_a_reword_or_squash_row`, `a_sets_the_author_and_dates_and_marks_the_row`, `esc_closes_and_asks_only_when_something_changed`, `enter_on_a_focused_button_is_its_click`, `the_run_shows_its_confirmation_and_runs_only_what_it_shows`) | ≈345 (it replaces 245 lines [S]; a floor), less the ≈110 lines of egui form parts it deletes | X4-quick | Enter on a reword row, `A`, Cmd/Ctrl+Enter and Esc in the planner: their dialogs in cells in both backends. The window's `SOPEN_CELLS` equals the TUI's with the run's confirmation open |
| X4-state | The switch state's `"v": 2` (§7.4, T32; Tensions, §7.4's entry). It adds the tree's bar and the rows the owner opened or closed, and the git panel's bar (by its commit, found on HOST as `select=` finds one, reading pages until it comes: `git_host.rs:2290-2300`), marks, details pane (its subject, whether it has the keyboard) and filters. It adds an open git dialog with what is typed in it — an action's, a quick edit's, the planner's, the filters' (choices not yet applied and the branches' type-to-filter text, `filters.rs:189-205`), the history search's (its six fields and the case, `SearchQuery`, `find.rs:211-228`) — Find's open field and its text, and the planner's unsaved plan. The menu, the result, the log's entry and the stop question hold nothing typed, and are dropped without asking. v1 is still read, the new fields at their defaults. Round-trip tests take each field out, rewrite its paths as §7.3 does, and give it back. `Browser::git_dropped`'s question (`browser.rs:2980-2991`): an open git dialog no longer asks, as the list's dialogs do not; the planner's plan still asks first, as §7.4 says unsaved edits do, and its line says the plan goes with the switch, in place of `… is not carried to HOST: switching drops it` (`browser.rs:3123`). Under X4-graph's first option it then deletes the panel's egui path (X4-graph's row). The texts that say a git dialog or the plan does not cross (`docs/tui.md:30-31`, `README.md:157`) say what v2 does | `src/gui/browser/switch_state.rs` (`VERSION`, `switch_state.rs:41`, and the fields), `src/gui/browser/switch_state/tests.rs`; `src/gui/browser/git_panel/state.rs` (new: the panel's part, taken and given) and its `mod` line in `git_panel/mod.rs`; `src/gui/browser.rs` (taking and giving the tree's part, `git_dropped` and its question only); `src/gui/browser/tests/x4state.rs` (new: the receipt's round trips) and its `mod` line in `src/gui/browser/tests.rs`; `src/gui/browser/tests/x2switch.rs` (the git panel's tests, `x2switch.rs:15-90, 118-175`); a getter and a setter each in `src/tree/mod.rs` (the folders the owner opened or closed, by path, beside `expand` and `collapse`, `tree/mod.rs:983-1044`), `dialogs.rs`, `filters.rs`, `find.rs` and `planner.rs`; under X4-graph's first option `graph_view.rs` and `mod.rs` (egui's path); `docs/tui.md:30-31`, `README.md:157` | ≈500 (X2-state's v1 came to +1,015 against ≈500 [M]: a floor), less what it deletes | the other X4 units (it reads and sets what their files hold) | **X4**: F12 to HOST from the TUI with the tree open and a folder opened by hand, or with the git panel's bar on a commit, two rows marked, the details pane open, a filter set, Find typed into, the Commit dialog, the filters or the history search typed into, or a plan with a squash row: HOST's window shows each as it was, and its close brings it back (TW-2's stand-in ssh). With the plan, the question comes first (`Ask::Git`, `browser.rs:2903-2904`): it says the plan goes with the switch, and Switch goes. A v1 state is read; a v3 is refused with §7.4's line |

- **Order**: X4-tree, X4-lanes and X4-kit from the start, side by side. X4-graph after X4-lanes.
  After X4-graph, side by side: X4-pane, X4-lists and X4-planner, and — once X4-kit has merged too
  — X4-find, X4-menu and X4-dialogs. `dialogs.rs` passes from X4-dialogs to X4-dialogs2, and
  `planner.rs` from X4-planner to X4-quick (which also waits for X4-dialogs's plan part) and then
  to X4-planner2. X4-state comes last, after all of them. Each ships alone; X4 is whole when
  X4-state has merged. As with X3, X4 does not wait for F-list-perf.
- **X4's rule for shared files** (decided 2026-11-27, reversible). Units side by side meet in a
  few files: `git_panel/tests.rs`, `tests_s3.rs` and `tests_s4.rs`, which are split by slice, not
  by screen (S2's graph, Find, details and filters; S3's menu, dialogs, Backups list, log and
  banner; S4's history row, quick edits, planner and edit stop); `browser/tests/s2.rs` to `s5.rs`,
  which drive the same screens by their texts and a focus id (`s4.rs:1047-1048`); `browser.rs` and
  `tests/x1a.rs` (X4-tree, X4-graph); and `AGENTS.md` and `docs/tui.md` (X4-tree, X4-graph,
  X4-state). Each git-panel screen unit's new tests — X4-graph's to X4-planner2's — go in a file
  of its own, `git_panel/tests_x4_<unit>.rs`; X4-graph creates all of these, a module doc each,
  with their `mod` lines, so no later unit touches a `mod` line there. X4-tree (`tree/tests.rs`),
  X4-lanes (`lanes/tests.rs`), X4-kit (`ops/tests/x4kit.rs`) and X4-state
  (`browser/tests/x4state.rs`) keep their own. In the shared files a unit edits only the tests or
  lines its row names, and the second to merge rebases, as X2 and X3 did where they met in a file.
  After X4-kit, `ops/dialog.rs` follows X3's rule for it: a unit that lacks a part adds it there,
  and the second to merge rebases. `dialogs::width` (`dialogs.rs:668-674`) has callers in five
  units: `dialogs.rs:836` (X4-dialogs), `:1691` (X4-dialogs2), `log_view.rs:145` (X4-lists),
  `planner.rs:683` (X4-quick) and `planner.rs:1108, 1165, 1228, 1275` (X4-planner2). Unused, it
  fails clippy's `dead_code` under `-D warnings` (`justfile:6`), so whichever of X4-lists,
  X4-dialogs2 and X4-planner2 merges last deletes it, a line this rule lets it edit.
- **X5** is 12 §16's ED-units. Of X4, ED-layout needs X4-graph: the Git tab is the git panel in
  cells at 24–40 cells (EL-4's 1:1, EG-1's lanes). It also takes `switch_state.rs` from X4-state,
  X4's last unit, to bump `v` again for the editor's fields; that file's order alone puts it after
  X4 as a whole (were the two to run side by side, whichever merged second would take the next
  `v`). By then the Git tab's details pane and dialogs (F3, EG-1's Space and F7) are cells too.
  ED-screen and ED-dialogs wait for no X4 unit. The tree is not in the column: the column's F10 is
  empty (12 §6.3).

X6, X7 and XF have a first cut (2026-11-27), to be cut again from their code once X4 has merged.
Sizes are [추정]: 1.4× the drawing functions' spans where a screen moves [S, a first count],
else from XR-T's estimates (*tui* §12: X6 ≈5–8k lines changed, X7 ≈1–2k).

**X6 — the image and media chrome in cells; pictures, video and the web view as regions (first
cut).**

| ID | Unit | Owns | Size | After | Ships alone as |
|---|---|---|---|---|---|
| X6-regions | §4's regions. A screen declares a cell rect and `Picture`, `Video` or `Web`. The window paints a texture there (a picture once, a video's frame each pass) or keeps the native web view at it (09 §2.2), hidden while a cell layer overlaps it, as 09 hides it under egui layers. Until X7 the TUI shows a region as a hand-off box, and a page's view 1 as §8's one-row hand-off (`the page is drawn in the window — F12`: 09 §8's "until X6", VM-8). TP-4: a region is its cell rect × (8, 16) pt, and a dialog over a macOS web view hides it (`hitTest` at its centre is not the view). 09's WV-F binds the page to its region | `src/gui/cells.rs` (the regions); `src/gui/term.rs` (a region's stand-in); `src/gui/mod.rs` (the window's painting of regions) | ≈300 | — | no change in the window. In the TUI a page's view 1 reads the one-row hand-off |
| X6-image | The image window's chrome in cells: the toolbar, the status row, the loading screen, the picture a `Picture` region; selection rectangles pixel-exact in the window and in cell units in the TUI (§4); TK-3's `P`, `F` and `H` through the translator; TS-4 with a picture open (`toolbar`, `status_bar`, `navigate`, `central`, `ui`, `loading`: in `image.rs:3290-5004`) | `src/gui/image.rs` (those); its tests (geometry asserts) | ≈600 (≈430 lines replaced) | X6-regions | an image's window with its chrome in cells in both backends; in the TUI the picture is a box until X7 |
| X6-image2 | The image window's rows and questions in cells, through `ops::dialog`: the keep, recovery, asked and paste rows, and the close prompt with 07 §10's four choices, as X3-questions moved the hex window's (`keep`, `recovery_rows`, `asked_row`, `paste_row`, `keep_line`, `close_prompt`: in `image.rs:2268-3730`) | `src/gui/image.rs` (after X6-image); its tests | ≈370 (≈265 lines replaced; a floor) | X6-image | an image's rows and close prompt in cells in both backends |
| X6-media | The media window's frame and transport in cells, a video's frame a `Video` region (`ui`, `transport`: `media.rs:4255-4456, 4810-4992`) | `src/gui/media.rs` (those); its tests | ≈540 (≈385 lines replaced) | X6-regions | a video's or a sound's window: its frame and transport in cells; in the TUI the video is a box until X7 |
| X6-timeline | The timeline and the waveform as block glyphs, `▁▂▃▄▅▆▇█` a cell, two rows for L and R, with selections in cells (§6) (`timeline`, `paint_wave`: `media.rs:4078-4253, 833-865`) | `src/gui/media.rs` (after X6-media); `src/gui/media/wave.rs`, `src/gui/media/timeline.rs` | ≈290 (≈210 lines replaced) | X6-media | the timeline and the waveform in cells in both backends |
| X6-media2 | The media window's info, picture and close prompt, and the recovery rows, in cells through `ops::dialog` (`info`, `picture`, `close_prompt`, `recovery_ui`: in `media.rs:3507-4639`) | `src/gui/media.rs` (after X6-timeline); its tests | ≈410 (≈295 lines replaced; a floor) | X6-timeline | a media window with no egui left |
| X6-preview | The preview side of an image, a video, a sound, a page and the info card in cells: X3-preview's cell frame for them too, the picture or the video a region, the info card's rows in cells (`prepare.rs:2121-2162`; media's `preview_ui`, `media.rs:4710-4764`). Only the region is a box inside the frame in the TUI | `src/gui/browser/preview.rs`; `src/gui/browser/prepare.rs` (the info card); `src/gui/media.rs` (`preview_ui`, after X6-media2) | ≈200 | X6-regions, X6-image, X6-media2 | in the TUI every preview's frame is cells |

**X7 — terminal pictures and video (first cut).**

| ID | Unit | Owns | Size | After | Ships alone as |
|---|---|---|---|---|---|
| X7-pick | §8's pick at start: kitty's `a=q`, DA1's `4`, XTVERSION and iTerm2's `ReportCellSize`, in XW-receipts's one round trip; else `TERM` and `TERM_PROGRAM`; `HC_TERMINAL_ID` gives half blocks (T20); `SOPEN_PICTURES` or `pictures` overrides it. The screen (T12): `SSH_CONNECTION` or `SSH_TTY`, overridden by `SOPEN_SCREEN` or `screen`, shown in help and on the status row with where it came from. 01 §4a's config sentences and AGENTS.md's rows (§12). TP-1, and TP-3's screen half | `src/gui/term.rs` (the probe); `src/gui/term/pictures.rs` (new: the pick); `src/config.rs` (`screen`, `pictures`); `src/gui/help.rs` (the line); `AGENTS.md` | ≈350 | X6-regions | no picture yet; help names the protocol and the screen in force |
| X7-draw | A `Picture` region drawn in the terminal: kitty (by temp file on a local screen when a query by that medium is answered, else in-band with zlib), iTerm2, sixel, half blocks; scaled by the reported cell size, else 8×16 px; deleted under a cell layer, on leaving and before a switch (T28, TL-1). **Decision to take in the unit**: XR-T's prototype encoders or a crate (*tui* §12 names ratatui-image) | `src/gui/term/pictures.rs`; `src/gui/term.rs` (placing and deleting); `Cargo.toml`, `Cargo.lock` if a crate | ≈600 | X7-pick, X6-image | an image on view 1 drawn in kitty, iTerm2, a sixel terminal, and as half blocks (TP-5) |
| X7-video | Video on a local screen at the protocol's rate (T8); on a remote screen the first frame and `F12 plays it in the window`, with no sound (T22). TP-3 | `src/gui/term/pictures.rs`; `src/gui/media.rs` (the frame handed to its region) | ≈300 | X7-draw, X6-media | a video plays in a local terminal; over ssh, its first frame and the F12 line |

**XF — the flip (first cut).**

| ID | Unit | Owns | Size | After | Ships alone as |
|---|---|---|---|---|---|
| XF | §11's rows 7 and 10 flip: a folder opens the TUI here; with no display the TUI opens with the file's viewer (an image as a terminal picture; a binary as the binary dialog in cells, then hex). The forward's automatic `--gui` turns on (`AUTO_GUI`, §7.3, T35). §12's XF rows (01 A11, §3a, A6, §3, §6a, A9, A10, §4a; the README's "In the terminal"; AGENTS.md). TF-1 | `src/main.rs` (the routing, `AUTO_GUI`); `tests/cli.rs`; `.prd/01-spec.md`, `README.md`, `AGENTS.md` | ≈250 | X6's and X7's units (T24); 12's ED-route, which it rebases onto | `sopen <folder>` opens the TUI in this terminal; F12 opens the window on the target |

- **X6, X7 and XF** are cut again from their code once X4 has merged. Until then their rows are a
  first cut: X6-regions first; X6-image, X6-media and X7-pick after it; the rest as their "after"
  columns say.

## Tensions

- ***tui* §12 X2 vs T2.** XR-T made "TUI by default" step 2, with the cross-machine switch; T2 makes
  the flip a late step behind a gate. §16 splits it: the switch keeps its place, the flip is XF.
- **T2's gate vs T24.** The gate alone could hold after X3, with pictures handed off; T24 waits for
  X6 and X7 so that the flip delivers message 23's pictures in the terminal.
- **08 §9 and *tui* §14.7 vs T7.** 08 says "Alt+digit is none either" (Option composes `¡`; Meta
  sends ESC, then the digit), but XR-T measured crossterm reading `ESC digit` as Alt+digit — a route
  on Linux terminals, [추정] on a Mac keyboard; §12 amends 08 §9. XR-T proposed Alt+digits "in place
  of" Ctrl/Cmd+digits; T7 keeps both.
- **04 §2, 04 §3 rules 3–4 and 01 §3a vs T4, T26, T33.** F12 was to stay unused, the key table's test
  forbids F12 rows, no plain F-key acts while a field has the typing, and Cmd is ⌘ on macOS; F12 and
  Alt+G are the plugin's and act in fields, and the TUI reads Ctrl as Cmd on every OS. Amended with X1b.
- **08 §9 vs 09 §8 on a page's slots.** 08 §9 opens HTML on Text with view 1 saying why; 09 §8 says
  "the source is view 1 there". T21 keeps 08's slots (Text = 3), so a window on 3 lands on 3. Both
  read T21's since VM-8.
- **09 §7's end-of-input close vs 01 A11.** A window with unsaved edits would close when its tunnel's
  or switch's input ends; T30 keeps it open, and 09 §7's rule applies to a window with nothing to lose.
- **09 §12 vs the freeze.** *tui* §12 freezes new egui screens; 09 ships WV-D without waiting. T23:
  its chrome is cells if X1a has landed, else egui moved in X6.
- **T8's "over ssh" vs the owner's herdr path**, which has no ssh environment: T12 makes it the
  owner's `screen = "remote"`; the probe says whether herdr panes are viewed from elsewhere.
  *tui* §14.5 (video across machines) is settled by T8.
- **T38 vs the printer's guess at chrome** (found reviewing X3-text, 2026-11-25). On the merged
  base `term::chrome` takes a text file's `…`, and box lines not between letters (`├──`), for
  sopen's own chrome and substitutes them in an ambiguous-wide terminal, though T38 says file
  content does not change. Follow-up: the viewer marks its cells as content, so the printer
  never substitutes them.
- ***tui* §5's ambiguous list.** It names `░` as ambiguous; python3's `unicodedata` gives it N, so §10
  keeps it and substitutes the rest.
- **§17's X3-binary row vs 01 §6a on the hex answer's label** (found building X3-binary). The row
  names the buttons `[ Hex ]` and `[ Save to… ]`; 01 §6a, the README and the dialog's tests name
  the default "Open in hex editor". Kept as built: `[ Open in hex editor ]  [ Save to… ]  [ Cancel
  ]` (48 cells, inside the smallest window's 56); the row's `[ Hex ]` is shorthand.
- **T35 vs a HOST with an older sopen, and vs a text file** (found cutting X2). `--gui` on every
  forward would break a plain `sopen --target HOST FILE` on a HOST whose sopen predates it (exit 2,
  clap's line; `--browser` has that reach by design, `forward.rs:387-389`, but is asked for), and
  §11 matching row 2 before row 5 would take a forwarded text file off 01 A2's editor
  (`main.rs:726`); the sender forwards before it knows the kind (`main.rs:567-585`). **Decided
  2026-11-25:** a forward passes `--gui` only when the owner gave it or the kind the sender sees
  needs a window; a text file is forwarded without it and keeps the editor; a kind the sender
  cannot see is forwarded without it and HOST picks by kind; HOST's exit 2 for an unknown `--gui`
  makes the sender retry once without it and say on stderr that HOST's sopen is older (X2-gui).
  **Decided 2026-11-26:** until XF the forward adds `--gui` only when the owner gave it — before
  the flip there is nothing for the automatic one to win, and it would turn HOST's no-display
  fallbacks into exit 1 (§11 row 6); XF turns it on (`AUTO_GUI`). The retry needs parse-time
  evidence: clap's line the first on HOST's stderr, nothing on its stdout.
  T35 still guards a HOST past the flip for every window kind the sender sees.
- **§7.4's "an open dialog with its typed text" vs the git panel's dialogs** (found reviewing
  X2-state, 2026-11-26). v1 carries the list's dialogs (04 §4–§5: the select-group, Copy / Move,
  rename, make-folder and delete ones) and nothing of the git panel's: a commit message, a tag's or
  a branch's name typed in an action's dialog (03 §7), the planner's unsaved plan, is carried by
  nobody yet — the git panel's screen moves in X4, which adds those fields and bumps `v` (§7.4).
  Until then a switch with one open drops its text; X2-switch's question before the switch is
  where to say so — since X2-switch it asks first, naming what is dropped (Switch · Cancel).
  X4-state is that unit (§17).
- **§6's lane glyphs vs 03 §4.3–§4.4** (found cutting X4, 2026-11-27). §6 draws lanes as
  `│ ─ ╭ ╮ ╰ ╯ ┼` "as `git log --graph`". 03 §4.4 and `chrome/git.rs:9` take every glyph from code
  page 437, which the VGA font holds (`chrome.rs:17`), and `╭ ╮ ╰ ╯` are not in it; CP437 has
  `┌ ┐ └ ┘`. 03 §4.3 paints a lane change as one diagonal across half a row, and 03 §4.4 the
  uncommitted row's line to HEAD dashed; a cell holds neither, and CP437 has no dashed line.
  `git log --graph` draws a lane change on a row of its own, where 03 §4.3's row stream keeps one
  row a commit. One row a commit stands; X4-lanes decides the corners and the dashed line.
  **Decided (X4-lanes, 2026-11-27):** CP437's square corners for the rounded ones (the VGA font
  has no others; the layout draws only `┐` and `┘`, joins coming from the right); a lane change runs
  along its node's row, so no cell holds a half-row diagonal; `├ ┤ ┬ ┴` where lines join (without
  them an octopus's or a criss-cross's shared row reads as lines ending) and `┼` where one crosses;
  the uncommitted row's line `:` a cell, dim (CP437 has it, and it stays one cell, apart from
  `|`, in an ambiguous-wide terminal); and a cell where lines meet takes the colour of the piece
  drawn last — in egui's order (lines passing by, then lane changes, then the node), lane changes
  among themselves longest first, as egui's diagonals share no stretch — so a line's colour changes
  where it turns; a join a longer lane change goes on past is told from a crossing by colour alone,
  which two lanes share when their occupancies began a multiple of five apart in turn (03 §4.3's
  colour index), adjacent lanes too — about one such join in five (03 §4.3–§4.4, §6).
- **03 §10.2's frame-title hover vs §6** (found cutting X4, 2026-11-27). 03 §10.2 put the host's
  git version "on the frame title's hover"; a frame in cells has no hover (§6: nothing changes
  under the pointer), and the panel drew none (no hover on its frame in `git_panel/`). 03 §10.2
  now names the log alone (03 §3.7).
