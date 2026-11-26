---
title: sopen — xtools in the terminal first, the window the same screen
type: spec
tags: [sopen, spec, xtools, tui]
---

# 10 — xtools: the terminal UI first, the window the same screen

Status: building (2026-11-25; planned 2026-11-24, review round 1 applied). X1a (the window's list
in cells, #119) and X1b (`sopen --tui`, #129) have shipped; X2 and X3 are cut into units (§17).

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
| Text fields; buttons and choices | egui field, 2-pt caret; framed widgets | black cells, a cell caret; `[ Make ]`, the bar colour when focused |
| Git lanes (design) | 1-pt segments, dashed when uncommitted | `│ ─ ╭ ╮ ╰ ╯ ┼` as `git log --graph` |
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
  start, so a config fixed meanwhile applies. Until X2, a HOST destination says on the status row
  `switching to HOST comes with the remote switch; --here switches on this machine`.
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
  stops reading and writing the session, and keeps the window until the owner closes it there
  [추정: a session without a pty sends its command no SIGHUP; X2 checks it]. This side restores its
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
| X2-host | HOST's half (§7.2, §7.3, T30): `--state-stdin` (hidden, with `--here --gui`) reads the state's line from its input and opens the browser at it in a switched window — F12, `[TUI]`, the close button, Cmd/Ctrl+Q and +W return; unsaved edits first ask that window's close prompt (Cancel keeps the window); a writing job makes the return wait — then prints `sopen-xtools-state <json>` and exits 0. The input ending closes the window only when it has nothing to lose (09 §7's rule, not built yet: the input watched as `--hold-until-eof` watches it, `main.rs:655`); else `sopen-xtools-kept`, the session no longer read or written, the window kept until it is closed there — checking §7.3's [추정] that a session without a pty sends its command no SIGHUP. The cues are fixed lines, as `sopen-tunnel-open` is (`url.rs:288`). Its sentences in 01 §4a | `src/main.rs` (`--state-stdin`; after X2-gui); `src/gui/switch_host.rs`, `src/gui/switch_host/tests.rs` (new) | ≈350 | X2-gui, X2-state | `sopen --here --gui --state-stdin -- FOLDER` with a state line on its input opens the window at that state and prints the cue when it closes: TW-4's HOST half |
| X2-switch | The TUI's half (§7.1's HOST, §7.3, T28, T29): `Place::Host`, a status line today (`term.rs:1368-1373`), runs HOST's half over 01 §4a's forward (its master, options, bare `exec` and quoting) with `-T`, `-o BatchMode=yes` and `SSH_ASKPASS_REQUIRE=never` (as the ssh exec layer and git's runner set them: `remote/exec.rs:23`, `git/runner.rs:140-141`), the state out on its input and the cue read from its output. Meanwhile raw mode and the alternate screen stay, the screen dimmed under §7.3's line, only Ctrl+C read; HOST's last stderr line is on the status row; each of §7.3's outcomes is its line, with the state from before unless a cue came. Before it switches: `waiting for <it>…` while a writing job runs, and with unsaved edits `Save the changes to host:/abs before switching to HOST?` in a cell dialog, 07 §10's four choices. TW-2 against a stand-in ssh; TW-4's TUI half; TL-1's remote-switch half. Its sentences in 01 A9, A10 and §4a and the README's "Open on another machine's screen" | `src/gui/term.rs` (the HOST arm of `to_window`); `src/gui/term/remote.rs`, `src/gui/term/remote/tests.rs` (new); `src/forward.rs` (the switch's words and its ssh run; after X2-gui); `src/gui/browser.rs` (the question and the wait only; after X2-state); `README.md` | ≈600 (the tunnel's run with its cues is 320 lines, `forward.rs:576-895` [S]) | X2-host | **X2**: F12 in `sopen --tui` with a target opens the window on HOST, and its close brings the TUI back with HOST's state; TW-3 is the owner's |

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
| X3-preview | The preview side in cells: its frame titled with the file, and its `reading … %`, `…` and failure lines (`Host::ui`, `say`: `preview.rs:383-468`), for a preview that paints cells (text, hex) — a page file's too, which in a terminal previews as its source (VM-8: `Pages::here`), so the TUI never shows a page's hand-off box inside a preview frame; an image or media preview leaves its side to the hand-off box until X6, so the TUI never shows a box inside a frame. Until it lands, the egui frame's one-cell ring around a text or hex preview's cells is hand-off strips in the TUI (F-handoff-cover's symptom). As built (`feat/x3-preview`): `Host::ui` is given the side's cells of the panels' grid and the cells a dialog of the list's covers there (`Ops::covered`, as the list's side asks it); a preview whose window paints cells (`Window::paints_cells`, `gui/mod.rs`: the text and hex views answer it, a page file's source in a terminal among them) gets the side's frame in cells — single-lined, the path cut from the left as the list's title is — and paints its own cells inside it; the read's progress, `…` and why there is none are cells at the frame's top, wrapped by cells (`dialog::wrap`), and the cells under a dialog or the key help are blanked first. An image, a video, a sound, the info card (`prepare.rs`'s, egui's) and a page in a window keep egui's frame, their lines egui's too — one hand-off box in a terminal — until X6. The frame does not change while a file is prepared: a ready preview's own; before it is ready, the frame of the file's last ready preview (a refresh prepares it again), else its name's — egui's for a picture, a video or a sound by extension and a page file whose page shows here, cells otherwise. A name that does not tell (a picture with no extension, a file over the preview's limit that becomes the info card, a plain `.xml` where pages show) changes frames once, when its first preview is ready. Built +150 −31 product lines [M] (`preview.rs` +127 −27). Release frame cost of the side with a text preview, alone at 1000×700: 0.38 ms against 0.31 (the browser's frame 0.51 against 0.43; +238 shapes, the frame's glyphs: F-list-perf) | `src/gui/browser/preview.rs` | ≈120 (it replaces 86 lines [S]) | X3-text, X3-hex, 09's WV-D (+64 lines in `preview.rs`) | in the TUI a text or hex file under the bar previews in cells in its frame, its progress line too |
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
- **Steps X4–XF** are cut into units when X3 has merged.

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
  where to say so.
