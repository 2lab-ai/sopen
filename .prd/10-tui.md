---
title: sopen — xtools in the terminal first, the window the same screen
type: spec
tags: [sopen, spec, xtools, tui]
---

# 10 — xtools: the terminal UI first, the window the same screen

Status: planned (2026-11-24). Nothing here is built; the work units wait for a spike in the real
repo (§17).

Answers item 2 of [`xtools/ssot.md`](xtools/ssot.md) (owner message 23). Items 1, 3, 4 and 5 (the
name, the editor, the viewer's views, web pages) land on this architecture: their screens are written
in cells as this document says. Measurements and the prototype: [`research/tui-gui.md`](research/tui-gui.md)
(XR-T), cited *tui* §n, its code citations at 255a52f. **[M]** measured there · **[S]** read in source
· **[추정]** inferred, not measured · **[owner]** only the owner's terminals or hands can answer (the
probe of *tui* §13). Decisions are T1–T24 (§13): T1–T10 the orchestrator's, T11 on this document's;
all reversible.

Owner message 23 (2026-11-24), verbatim:

> 그리고 이 xtools는 터미널 tui가 디폴트고 (이미지나 영상 빼고 다 실제 tui랑 1:1 매칭되고 잇는 ui임
> 그 tui상태에서 버튼 누르면 tui가 그대로 gui로 변하게 해줘 gui에서는 이미지나 영상등을 출력해줄수 있고 그 이미지 출력할수 있는 터미널 모드도 있지 않나? 거기서는 네이티브 터미널에서 그대로 이미지와 영상 출력할 수 있게 해주고 이것도 리서치해서 어떻게 하는게 가장 좋을지 고민하고 작업 해줘

What it amends in 01, 04, 08, 09, the README and AGENTS.md, and when: §12.

## 1. The contract

- **One screen, two backends** (T1): every screen is one grid of cells (character, fg, bg); the
  terminal prints it, the window paints the same cells with the VGA font and hosts **pixel regions**
  (pictures, video frames, web views) at cell rectangles. **1:1** (§5): at one grid size every cell
  outside a region is the same in both, the caret too; the one difference is the mode label.
- **The switch** (§7, T4): F12 or the title row's `[GUI]`/`[TUI]` moves the running xtools between
  terminal and window, keeping where the owner is — in one process, or through 01 §4a's forward. The
  terminal is left as found (T19).
- **Pictures in a terminal** (§8, T8): kitty, iTerm2 or sixel where answered, else half blocks; video
  plays only on a local screen, elsewhere the first frame and F12.
- **The default flips late** (§11, T2). The owner's words are "TUI가 디폴트", and this document gets
  there in steps, not with the first slice: until the flip `sopen <folder>` is today's window and
  `sopen --tui <folder>` the TUI; the flip comes once Enter on every kind is useful in the TUI.

## 2. Scope

- **In**: the core and both backends, the switch, terminal pictures and video, keys and mouse in a
  terminal, wide characters, the window's look, the flip and what it changes, the migration order.
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

## 4. The architecture (T1)

- **The core** draws every screen into a ratatui `Buffer` (symbol, fg, bg, modifiers per cell); the
  prototype's `core.rs` (928 lines: list, F-key row, dialog, state, keys) is the shape [M]. Cell API:
  text, fill, frame (`╔═╗║` / `┌─┐│`, joints `╟ ╢` / `├ ┤`), scroll bar (`↑ ░ ↓`, one-cell thumb),
  F-key row, dialog with its shadow, field, button; widths by unicode-width (§10); colours the skin's
  palette as RGB, never an ANSI index (T15). A screen declares its regions (a cell rect and `Picture`,
  `Video` or `Web`). Layout is a function of the grid size only: one size, one set of cells.
- **The terminal backend**: ratatui 0.30 + crossterm 0.29, as the prototype [M] — alternate screen,
  raw mode, SGR mouse reporting so the row, the mode button and rows click [M] (the terminal's own
  selection then needs Shift+drag in most terminals [추정]); the kitty keyboard protocol (flag 1)
  pushed only after the terminal answers `CSI ? u`; truecolor when `COLORTERM` says `truecolor` or
  `24bit` (kitty's environment, herdr-core's help string [M *tui* §7.1]), else the 256-colour cube; the caret is the
  terminal's cursor; only changed cells are sent. **No font is loaded on the TUI's path.**
  **Leaving** (T19) — exit, panic (a hook), SIGTERM, SIGHUP, before a switch: pictures deleted,
  keyboard flags popped, mouse reporting off, the alternate screen left, the cursor shown.
- **The window backend**, in today's eframe window: each cell at (col × 8, row × 16) pt — its
  background, then its glyph at its own cell (the VGA font, then the system fallbacks), never through
  egui's text layout, which is what moves Hangul off the grid (*tui* §1.3, §5). The grid is
  floor(w/8) × floor(h/16), the remainder in the background; the caret two scan lines at the cell's
  foot. A window opened by a switch has the terminal's grid size (T16).
- **Regions in the window**: a picture is a texture at the rect (the image exactly inside its frame,
  the other 1,800 cells passing the pixel check [M *tui* §6]); a video frame a texture uploaded each
  frame (320×180 at 93 fps [M]); a web view a WKWebView subview at the rect on macOS (09 §2.2; the
  rect measured with an NSBox [M], the visible check not made), WebKitGTK offscreen into a texture on
  Linux (09 §2.3). Cells paint over a texture; a native view is hidden while a cell layer overlaps
  it, as 09 hides it under egui layers. Selections inside a region (the image's rectangle, the media
  crop) stay pixel-exact in the window, cell units in the TUI (*tui* §1.2; design). In a terminal: §8.
- **Input: one neutral input** (T10). egui's input types stay the input of both. The terminal backend
  turns crossterm events into `egui::Event`s and runs a headless `egui::Context` pass, so the shell's
  raw-input hook (`gui/mod.rs`) and the key table (`browser/keys.rs`) see them as from the window;
  key, focus, memory and repaint code stay, drawing and the 45 font measurements (→ unicode-width)
  change (*tui* §11 [M counts]). The headless pass is measured; **the translation is not prototyped**
  [추정], the spike's item (§17). A named key → `Event::Key` with modifiers; a printable character
  without Ctrl/Alt → `Event::Text` alone (a terminal sends no key event or physical key, 08 §9, so
  08's view keys match the character); Ctrl/Alt + a character → `Event::Key` with modifiers, no text;
  mouse → pointer events at the cell's centre; wheel by lines.
- **Screens not yet moved** keep working in the window and are hand-off boxes in the TUI (T17).
  *tui* option C's projector (egui shapes laid onto cells) stays only as a lint over them: off-grid
  counts may not grow past *tui* §1.3's rerun; a screen leaves it in the step that moves it (T1).

## 5. The screen contract: what 1:1 means

- **Compared per cell**, at one grid size, outside regions (masked, asserted as rects): character,
  fg RGB, bg RGB, the caret's cell; a wide character's second cell carries the first's colours, as
  terminals paint it. Excepted: the mode label; the clock is fixed by a test hook. **How**: the
  backend's bytes parsed by the vt100 crate and in a private tmux server (`capture-pane -e`), against
  the window's paint run headless with its fonts and its shapes laid back onto cells (what it
  painted, not the buffer it was given); the real window by its screenshot, cell by cell.
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
| Shadows | translucent black over anything | DOS shadow: the cells' glyphs dark grey on black, 2×1 |
| Dialog margin; panel inset; status bar | 2 × 1.5 cells; 2 pt; `CELL.y + 4` tall | 1 cell; none; one row |
| Frames; the rule above the foot | strokes; an `hline` at mid-cell | box glyphs; `─` with joints |
| Text fields; buttons and choices | egui field, 2-pt caret; framed widgets | black cells, a cell caret; `[ Make ]`, the bar colour when focused |
| Git lanes (design) | 1-pt segments, dashed when uncommitted | `│ ─ ╭ ╮ ╰ ╯ ┼` as `git log --graph` |
| Timeline, waveform (design) | segments per pixel column | `▁▂▃▄▅▆▇█` per cell, two rows for L/R |
| Progress, spinner | egui widgets | `█▓░` cells; a one-cell ASCII spinner |

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

### 7.2 In one process [M *tui* §8]

- F12 or the click leaves the terminal (§4), prints `xtools is in a window now — close it, or press
  F12 there, to come back here.`, runs the window on the main thread with the same state and comes
  back with the state it hands back; unsaved edits cross unsaved (one process holds them). Measured:
  361 ms to the first window's first frame, 60 ms the second (eframe keeps winit's loop per thread);
  a folder made in the window and a dialog's typed text, caret and error came back, twice.
- In a window opened by a switch, F12, `[TUI]` or closing it returns; Cmd+Q ends sopen (T14). A
  window started directly quits on close, as today; it shows `[TUI]` only when the process has a
  terminal on input and output (F12 then takes it), else neither works there.

### 7.3 Across machines

- **HOST's half**, through 01 §4a's forward (its master, options, bare `exec` and quoting, HOST's
  PATH and desktop environment, the macOS float rule, exit codes), with `-T`: `sopen --here --gui
  --state-stdin -- <the active folder, rewritten as §4a rewrites SOURCE>`. The state is one JSON line
  on its input, which then stays open. When that window closes, or F12 is pressed there, HOST prints
  `sopen-xtools-state <json>` (a fixed cue, as `sopen-tunnel-open`), exits 0, and the TUI here
  re-enters with that state. **Measured** [M *tui* §8], laptop TUI (no display) → Mac window: 4,885
  bytes, first frame 192 ms, a folder made there listed back, 1.1 s end to end. **The owner's
  direction (Mac TUI → laptop window) is not measured** [owner, TW-3].
- **Meanwhile** the TUI stays, dimmed, under `xtools is in a window on HOST — close it or press F12
  there; Ctrl+C ends it here`. Ctrl+C closes HOST's input and so its window (09 §7's end-of-input
  rule); the TUI returns with the state from before and lists again (T13). **Failures** (status row,
  the TUI stays): 255 → `could not reach HOST`; 127 → `sopen is not installed on HOST …`; 2 (older
  than `--state-stdin`) → `sopen on HOST is too old for the switch`; no cue → the state from before.
- **Paths** go out rewritten as §4a's SOURCE (local → `<this machine>:<abs>`, HOST's own → local) and
  come back with `<this machine>:<abs>` local again. HOST reads this machine's files by its own ssh
  (§4a); a HOST that cannot is the origin workstream's.

### 7.4 What the state carries; unsaved edits (T4)

- Versioned JSON (`"v": 1`; the prototype's fields [M *tui* §8] plus the viewer's): both panels'
  folders, bars, selections and scroll, the active side, the mode, each side's content (tree open, git
  pin), an open dialog with its typed text, caret and error; the open viewer — file, 08's view,
  place, encoding, wrap; the source host. No caches, streams or jobs: the receiver opens them again.
  A running file operation or Save makes the switch wait (`waiting for <it>…`), as a running Save
  makes 08's view switch wait (T13). Unknown fields are ignored; an unknown dialog is dropped, named.
- **Unsaved edits across machines**: 07's journal is per machine (07 §3) and does not cross in the
  first cut. The switch first asks `Save the changes to host:/abs before switching to HOST?` →
  **Save** (switch once it lands) · **Discard** (switch with the file as stored) · **Cancel** (stay).

## 8. Pictures, video and pages in a terminal (T8)

- **Which protocol**, at start: (1) queries — kitty `a=q`, DA1's sixel attribute `4`, XTVERSION and
  iTerm2's `ReportCellSize`; (2) `TERM` / `TERM_PROGRAM`; (3) else half blocks (two pixels a cell).
- **An answer is not proof of drawing.** herdr's libghostty-vt parses kitty frames [M strings], so an
  `a=q` OK there proves parsing only; whether herdr-app draws is [owner, TP-2]. herdr-core sets
  `TERM_PROGRAM=ghostty`, so the environment cannot tell herdr from Ghostty: until the probe answers,
  a TUI with `HC_TERMINAL_ID` in its environment (seen in the laptop's herdr panes [M], on the Mac
  [추정]) uses half blocks (T20). A TUI reached by ssh from inside a herdr pane has no such variable.
- **Placement**: scaled by the reported cell size (else 8×16 px [추정]); deleted under a cell layer.
- **Local fast path**: kitty by temp file (94 fps vs 59 in-band [M]); over ssh it showed nothing, the
  file being on the other machine [M], so it is used only when a query sent by that medium is answered
  OK [추정: kitty's query by medium, not tried]; else in-band, zlib.
- **A remote screen** (T12): `SSH_CONNECTION` or `SSH_TTY` set, or a start-up round trip (one DSR)
  ≥ 5 ms [추정 threshold]. The owner's real path — a TUI running locally under herdr on the Mac, its
  bytes crossing to the laptop inside herdr's own connection — has no ssh environment; the round trip
  finds it. Its value in each owner terminal is [owner].
- **Video**: a local screen plays (640×360 in WezTerm [M *tui* §7.4]: the kitty protocol 59 fps,
  sixel 41, half blocks 221; the kitty terminal and herdr unmeasured); a remote one shows the first
  frame and `F12 plays it in the window` — over ssh the kitty protocol gave 6–8 fps, sixel 11, and the
  link varied 5× in half an hour. **Sound** comes out of the machine sopen runs on, so on a remote
  screen the TUI plays none; the waveform and F12 instead (T22).
- **A page** keeps 08 §3's slots in both backends (1 Page · 2 Hex · 3 Text), so the strip is 1:1 and
  the state's `view` means the same after F12. In the TUI view 1 is a one-row hand-off (`the page is
  drawn in the window — F12`) and the file opens on 3, its source, as 08 §9 says. No snapshot: the
  engine runs inside a window (WKWebView needs one; WebKitGTK's offscreen path a display [추정]) (T21).

## 9. Keys in a terminal (T7, T9)

| Route | Legacy encoding | kitty protocol |
|---|---|---|
| F1–F12, Shift/Alt/Ctrl+F5–F10, Shift+Del, Ctrl+PgUp, Ctrl+←/→, Shift+Tab | decoded [M] | decoded [M] |
| The clickable F-key row and mode button | mouse reporting [M] | the same |
| **Alt+1…0 → F1…F10** (T7) | `ESC <digit>`, read by crossterm as Alt+digit [M] | `CSI …;3u` [추정] |
| Ctrl+1…0 → F1…F10 (04 §6.1) | lost: Ctrl+3 → Esc, Ctrl+8 → Backspace, Ctrl+1/0 → digits [M] | decoded [M] |
| Cmd+… (macOS) | never sent | `CSI …;9u` when the terminal passes ⌘ [M decode]; ⌘+digits are tab keys there [추정] |
| Shift+F3 (git: file at revision); Ctrl+Shift+letter (git) | read as a cursor report; Shift lost [M] | distinct [M] |
| 08's view keys | the character `1`/`2`/`3`; F9 then a digit | the same |
| Esc | a lone `ESC`, by timing | `CSI 27u` |

- **Alt+digits** become that F-key once, in the shell's raw-input hook, **in the TUI only** (in the
  window Option+digit stays typed text on macOS), never while a text field has the typing (04 §6.1);
  Cmd/Ctrl+digits stay where they arrive (T7). On a Mac keyboard Alt+digit arrives only where the
  terminal sends Option as Alt [추정]; herdr-app's own bindings are unknown [owner, TK-2]. Legacy
  residual: Esc then a digit within the escape timeout reads as Alt+digit [추정].
- **Legacy gaps**: Shift+F3 and the Ctrl+Shift git actions get entries in the git panel's F10 menu
  (`keys.rs` `C::Menu`, *tui* §4) (T17).
- **F12 = the switch** (T4), taken by the shell's raw-input hook in every context, fields included
  (it types nothing), never a key-table row (`no_duplicate_bindings` forbids F12 rows and stays).
  Whether kitty, herdr-app and herdr pass F12 is [owner, TK-2]; the button always works.
- **No per-app F-key switch in a terminal** (T9): sopen owns no surface there; the macOS HID switch
  (04 §6.2) and the Wayland shortcuts inhibitor (*fkeys-linux* §8) are the window's; over ssh or
  herdr no TUI reaches the Mac's keyboard; kitty's own keyboard grab is the owner's terminal setting,
  a later study. In the TUI the F-keys that always work are real F-keys, the row and Alt+digits.

## 10. Wide characters (T6)

- **Terminal**: unicode-width — Hangul, CJK, emoji (`한 글 漢`, U+1F600, U+1F44D U+1F3FD) 2 cells;
  `─ … ░ A é` and every East Asian *ambiguous* glyph 1 [M *tui* §5].
- **Window**: each glyph painted at its first cell. Hangul advances 13.84 pt on the Mac (Apple SD
  Gothic Neo), 14.72 pt on Linux (WenQuanYi Zen Hei) and sits in its 16-pt slot with 1–2 pt to
  spare; emoji 16.00 pt on the Mac, but Noto Color Emoji on Linux is 19.92 pt, 3.92 over two cells —
  clipped to them. (egui's own layout puts 7 Hangul in 12.1 cells where the terminal gives 14.)
- **Ambiguous width** (T18): at start the TUI writes `─` and reads the cursor report; 2 cells (an
  "ambiguous = wide" setting, possible under the laptop's `ko_KR.UTF-8` [추정]) → frames, scroll bar
  and arrows in ASCII (`+ - | # < >`) for that run. The probe measures each owner terminal [owner].
- **Cutting a name** (01 §3a) never splits a wide character: it ends a cell earlier, with "…".

## 11. Until the flip, and the flip (T2, T3)

- **Until the flip**, `sopen <folder>` is today's window, unchanged; `sopen --tui <folder>` runs
  xtools in this terminal with F12 to the window (§7). A content not in cells yet is a hand-off box
  (T17): a frame at its place with its title, what it is, and a clickable `F12 shows it in the window`.
- **The gate** (T2): the TUI becomes the default only when Enter on every kind does something useful
  in it — text and hex in cells (X3); image, media and page by terminal graphics where the terminal
  can (X7), else a one-key hand-off to the window (since X1; across machines since X2).
- **The tree and the git panel are not Enter kinds** but are 1:1 content (message 23 excepts only
  images and video), and preview mode shows git opposite a repository folder by default (01 A12):
  before X4 they are hand-off boxes in the TUI, so the flip lands after X4 (T24; Tensions).
- **After the flip** (T3): `sopen <folder>` runs the TUI in this terminal — a local or remote folder,
  a target set or not; `target` (config or `--target`) is where the window opens on a switch; `--here`
  keeps the window on this machine; `sopen --gui <folder>` opens the window directly (T14). With no
  display the TUI opens where today a folder goes to the editor and an image, media, binary or
  `--hex` file is fetched and its path printed. Images, video, audio and pages named on the command line keep opening
  the window on the destination (T14); text stays 01 A2's route until the editor spec (XR-E) decides.
- **Only where input and output are both terminals** (T13); otherwise today's paths, unchanged — `while
  read f; do sopen "$f"; done < list` (01 §4a), `SOPEN_LIST`, `SOPEN_AUTOSAVE`, `SOPEN_AUTOEXPORT`, a
  forward with no terminal. `sopen URL` keeps 09 §4's no-display rule.

## 12. Contracts that flip

| Text | Today | Becomes | With |
|---|---|---|---|
| 01 A11, §3a's first line | a folder opens in a window | the TUI in this terminal | XF |
| 01 §3a "Without a display" | the folder goes to the editor | the TUI, where input and output are terminals | XF |
| 01 A6, §3, §6a (no display) | fetched, its path printed | the TUI's view of its kind (hex for a binary and `--hex`), where input and output are terminals | XF |
| 01 A9, A10, §4a | `target` = where SOURCE opens | the switch's destination (X2, for `--tui`); for `sopen <folder>`, where the window opens on a switch; GUI kinds as today (XF) | X2, XF |
| 01 §4a | — | HOST's half `--gui --state-stdin`, the `sopen-xtools-state` cue | X2 |
| 01 A14 | every window | the HID switch and the inhibitor are the window's; the TUI has the row, Alt+digits, real F-keys | X1 |
| 01 §3a, the skin | translucent 2×1 shadows, half-cell paddings | §6's look | each step, for its screens |
| 04 §2 | "F11 and F12 stay unused" | F12 = the switch, the shell's (not a table row) | X1 |
| 01 §3a, 04 §3 rule 4 | a plain F-key never acts while a text field has the typing | except F12, which types nothing; the field's text crosses with the state (as 08 D29 excepts F9) | X1 |
| 04 §6.1 | Cmd/Ctrl+1…0 | plus Alt+1…0 in the TUI; Cmd/Ctrl+1…0 in the TUI under the kitty protocol | X1 |
| 08 §8–§9, VM-7, VM-8 | provisional; "if XR-T keeps egui windows" | it does not: VM-8 (cells) replaces VM-7 in X3; §9's "Alt+digit is none either" gains "except as the TUI's F-key aliases, where `ESC digit` arrives" | X3 |
| 09 §8 | left to the TUI spec; "Text is view 1 there" | §8 here: 08's slots kept, view 1 a hand-off, the file opens on 3 (Text); F12; no snapshot | X3 |
| 09 WV-D, WV-E chrome | egui | written against the cell API if X1 has landed, else in egui and moved in X6 (T23; the page itself is a region) | X1, X6 |
| README | line 9 (folder → window); "Requirements" (a desktop session); "Open on another machine's screen" | `--tui`, F12 (X1); the switch to a target (X2); folder → TUI, target = the window's place, a section "In the terminal" (XF) | X1, X2, XF |
| AGENTS.md "Receipts" | — | rows for `--tui`, `--gui`, `SOPEN_CELLS`, `SOPEN_REQUIRE_TMUX` | X1 |

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
| T8 | Terminal pictures: kitty / iTerm2 / sixel where supported (detected by query), half blocks otherwise; video plays locally, over ssh shows the first frame and offers the GUI; whether herdr passes pictures is the owner probe's | 6–8 fps over ssh is not playback (*tui* §7.4); herdr's drawing is unmeasured |
| T9 | F-keys in the TUI: no per-app switch exists in a terminal; kitty's keyboard grab is the owner's terminal setting, a later study | sopen owns no surface in a terminal (*tui* §10) |
| T10 | egui's input types stay the neutral input of both backends; tests query cells (text and attributes), not pixels | the key table, focus and repaint code and the tests' input side survive (*tui* §11) |
| T11 | The switch's destination: 01 §4a's order first, the display second | in the owner's setup "display here → in-process" opens on a screen nobody looks at |
| T12 | T8's "over ssh" = a remote screen: ssh environment, or a start-up round trip ≥ 5 ms [추정 threshold] | on the owner's herdr path the TUI has no ssh environment (*tui* §7.1) |
| T13 | The TUI only where input and output are terminals; a switch waits for a running Save or file operation; Ctrl+C during a remote switch returns with the state from before | scripts and hooks keep today's paths; a job cannot be carried; the owner always has a way back |
| T14 | `--gui` after the flip; GUI kinds on the command line keep opening the window on the destination; closing a switched-to window returns to the TUI, Cmd+Q ends sopen | *tui* §9's proposal; today's routes stay reachable; one close does not lose the terminal |
| T15 | Colours as RGB (truecolor), else the 256-colour cube; never the 16 ANSI indexes | a terminal theme repaints the ANSI indexes, breaking 1:1 |
| T16 | A window opened by a switch has the terminal's grid size | 1:1 is defined at one grid size |
| T17 | Contents not in cells yet are hand-off boxes in the TUI; Shift+F3 and the Ctrl+Shift git actions get F10 menu entries | each step ships alone (*tui* §12); legacy encoding loses those keys |
| T18 | The pixel check reads foregrounds on glyph cells with ≥ 2 ink pixels, never colour glyphs; an ambiguous-width probe at start switches frames to ASCII if `─` takes 2 cells | the U+1F3B5 miss (*tui* §2); "ambiguous = wide" would break every frame |
| T19 | The terminal is left as found on exit, panic, SIGTERM, SIGHUP and before a switch | a TUI dead in raw mode leaves the shell unusable |
| T20 | `HC_TERMINAL_ID` set → half blocks until the probe answers | herdr parses kitty frames but may not draw them; its `TERM_PROGRAM` says ghostty |
| T21 | A page keeps 08's slots in the TUI: view 1 a hand-off row, the file opens on 3 (its source); F12 to the page; no snapshot | the state's `view` must mean the same in both backends; the engine runs inside a window (09 §2); 09 §8 left the choice here |
| T22 | No sound on a remote screen; the waveform and F12 | sound comes out of the machine sopen runs on |
| T23 | 09's page viewer and address window chrome: cells if X1 has landed, else egui moved in X6 | 09 §12 ships WV-D without waiting; the page itself is a region either way |
| T24 | XF lands after X3 and X4 (the gate's earliest point is after X3) | preview mode shows git opposite a repository folder by default (01 A12); a hand-off box there falls short of today's window |

## 14. Acceptance

"Do X → observe Y"; the step that brings each item in brackets. **[owner]** = the owner's terminals
or hands (*tui* §13's probe, extended by these items).

**The screen**
- TS-1 [X1]. The list, the F-key row and the F7 dialog with `새 폴더 2026` typed, the fixture with
  Korean names and U+1F3B5, 100×30: cell dumps of (a) the terminal bytes through vt100, (b) a private
  tmux server's `capture-pane -e`, (c) the window's shapes laid onto cells → 0 character, foreground
  and background differences but the mode label; the caret equal. macOS and Linux CI.
- TS-2 [X1, owner]. The same in kitty and a herdr-app pane on the Linux laptop and a herdr pane on the
  Mac: kitty by its remote-control text dump [추정: not tried], herdr by a screenshot, cell by cell.
- TS-3 [X1]. The window's screenshot: every background and every ≥ 2-ink-pixel glyph's foreground =
  the dump; regions masked, colour glyphs excepted (T18). TS-5 [X1]: resized to 80×24 and 160×50,
  each dump = the window's at that size. TS-7 [X1]: the projector lint stays within *tui* §1.3.
- TS-4 [each step]. Every moved screen passes TS-1 (a) and (c) — X3: text and hex viewers, a UTF-16
  Hangul file in view 1, the encoding list open; X4: lanes, the tree; X6: the chrome around regions.
- TS-6 [X1]. A pty emulator advancing `─` by 2 → ASCII frames, no row broken; [owner] the probe's
  advances in each owner terminal. TC-1 [X1]: `한글 문서 초안.txt` takes the same cells in both (its 6
  Hangul in 12); an emoji 2 cells, no ink outside them in the Linux screenshot.

**The switch**
- TW-1 [X1]. One process, a display: F12 from the list with the F7 dialog open and typed into → the
  window's dump = the TUI's but the label; a folder made; F12 → the TUI lists it, the dialog as left;
  twice per run, first-frame times recorded. A click on `[GUI]` = F12; closing returns, Cmd+Q ends
  sopen; a window with no terminal shows no `[TUI]`. No display and no target → the status line.
- TW-2 [X2]. A stand-in ssh running HOST's half locally: state in, cue out, paths rewritten both ways;
  Ctrl+C → the state from before; exits 255, 127, 2 → the three lines, the TUI stays.
- TW-3 [X2, owner]. The owner's direction: the TUI in a herdr pane on the Mac shown on the laptop,
  `target = <the Linux laptop>`; F12 → the window there, same cells; a folder made; close → the Mac's
  TUI lists it; timed.
- TW-4 [X2]. An unsaved hex edit, F12 to HOST → the save question; Cancel stays; Save → switched
  after it lands. In one process the edit crosses unsaved and saves from the window.

**Pictures, keys, the flip, leaving**
- TP-1 [X7]. The pick against scripted answers: kitty → kitty; DA1 with `4` → sixel; none → half
  blocks; `HC_TERMINAL_ID` → half blocks. TP-2 [X7, owner]: the probe's `--show` squares in kitty,
  herdr-app and herdr on the Mac — which drew; TP-1's table follows.
- TP-3 [X7]. A video in a pty answering at once → plays; answers held 20 ms, or `SSH_CONNECTION` set →
  the first frame, the F12 line, no sound. TP-4 [X6]: a region = its cell rect × (8, 16) pt; a dialog
  over a macOS web view hides it (`hitTest` at its centre is not the view).
- TK-1 [X1]. *tui* §4's table as a test (legacy and kitty bytes → commands): Alt+digits → F-keys under
  legacy, Ctrl+digits under kitty, Shift+F3 and the Ctrl+Shift actions in the F10 menu; the kitty
  flags pushed only after `CSI ? u` is answered. TK-2 [X1, owner]: in kitty, herdr-app and herdr on the
  Mac, which of F12, Alt+1…0, Ctrl+1…0 arrive. TK-3 [X3]: the character `1` with no key event → view
  1; F9 then `2` → Hex; in an editable hex view `1` is typed.
- TF-1 [XF]. `sopen <folder>` in a terminal → the TUI, `target` set or not; `--gui` → the window;
  output not a terminal → today's run (`SOPEN_LIST` prints, a `while read` loop is not eaten); no
  display → the TUI; Enter on every kind of 08 §3 → cells, a picture or a hand-off. TL-1 [X1]: exit, a
  test-hook panic, SIGTERM, SIGHUP, a switch → modes reset, kitty flags popped, pictures deleted.

## 15. Test plan

- **Tests query cells** (T10): `find_text`, `click_text`, `texts`, `text_starting_with`,
  `colors_of`, `rows_of` are re-implemented over the grid with the same signatures; the ≈75 pixel
  queries (`pictures`, `rects`, `segments`, `galleys`, `strokes`) are rewritten as region rects [M
  counts *tui* §11]. The Driver's input side stays. A screen's tests change in the step that moves it.
- **Two harness tests per screen**: terminal bytes through vt100 = the buffer; the window's shapes
  laid onto cells = the buffer (§5). **tmux** is CI's second emulator (`SOPEN_REQUIRE_TMUX=1`, as the
  other `SOPEN_REQUIRE_*`). **A terminal driver** plays legacy and kitty bytes into a pty (*tui* §4's
  `keyaxis`), the character alone for typed text (08 §12). **The remote switch** runs against a
  stand-in ssh (AGENTS.md: no ssh or network in tests), as 01 §4a's forward is tested today.
- **Receipts**: `SOPEN_CELLS=<path>` writes the grid (a row per line plus an attribute table) once
  `SOPEN_KEYS` have settled, then ends — both backends, one format for TS-1, TS-2 and the owner's
  receipts. `SOPEN_SCREENSHOT` stays the window's.
- **Gate**: the loop's (`just check`, `SOPEN_REQUIRE_FFMPEG=1 SOPEN_REQUIRE_SFTP_SERVER=1`, the Linux
  clippy runs); a unit that changes a screen brings a cell dump and a screenshot someone looked at.

## 16. Migration order

*tui* §12 adjusted by T2: XR-T's X2 bundled "TUI by default" with the cross-machine switch; the
switch stays early and the flip is its own step, XF. Each step ships alone (prerelease), none waits
for a later one, and each moves its screens' tests (§15).

1. **X1 — the core, both backends, the list**: the cell API, the terminal backend, the window's cell
   painter, the dump/diff harness, `SOPEN_CELLS`; the list, title, status, F-key row and the list's
   dialogs (F2, F5–F8, groups) in cells **in both**; `sopen --tui`; F12 → the window in one process.
   Viewers, tree and git panel stay egui in the window, hand-off boxes in the TUI.
2. **X2 — the switch across machines** (HOST's half, the state and cue). No default flip.
3. **X3 — the text and hex viewers and their previews in cells**: 08's views and encodings (VM-8).
4. **X4 — the tree and the git panel in cells**: lanes as glyphs, details, dialogs, the planner.
5. **X5 — the editor (XR-E)**, cell-native; its left-panel directory/git tabs need X4.
6. **X6 — the image and media chrome in cells; pictures, video and the web view as regions.**
7. **X7 — terminal pictures and video**: the pick, the fast path, in-band frames, the remote rule.
8. **XF — the flip** (T2, T3) is not positional: it lands once its gate holds, after X3 at the
   earliest (the hand-off exists since X1/X2), and by T24 after X4. §12's XF rows change here.

## 17. Work units

Sized after the spike in the real repo: hosting legacy egui panes in a cell-grid window, the
crossterm→egui translation, test breakage.

| ID | Unit | Owns | After |
|---|---|---|---|
| — | (placeholder — filled from the spike's results) | — | — |

## Tensions

- ***tui* §12 X2 vs T2.** XR-T made "TUI by default" step 2, with the cross-machine switch; T2 makes
  the flip a late step behind a gate. §16 splits it: the switch keeps its place, the flip is XF.
- **XF before or after X4.** T2's gate names Enter kinds only and can hold after X3; T24 waits for X4
  (the git side, §11) [추정: most owner folders are repositories]. The owner may move it earlier.
- **08 §9 vs 09 §8 on a page's slots.** 08 §9 opens HTML on Text with view 1 saying why; 09 §8 says
  "Text is view 1 there". T21 keeps 08's slots (Text = 3), so a window on view 3 lands on view 3.
- **09 §12 vs the freeze.** *tui* §12 freezes new egui screens; 09 ships WV-D without waiting. T23:
  its chrome is cells if X1 has landed, else egui moved in X6.
- **08 §9 and *tui* §14.7 vs T7.** 08 says "Alt+digit is none either" (Option composes `¡`; Meta
  sends ESC, then the digit), but XR-T measured crossterm reading `ESC digit` as Alt+digit — a route
  on Linux terminals, [추정] on a Mac keyboard; §12 amends 08 §9. XR-T proposed Alt+digits "in place
  of" Ctrl/Cmd+digits; T7 keeps both.
- **04 §2 and 01 §3a vs T4.** F12 was to stay unused, the key table's test forbids F12 rows, and no
  plain F-key acts while a field has the typing; F12 becomes the switch in the shell's hook (the
  table and its test stay) and acts in fields too. Both are amended with X1 (§12).
- **T8's "over ssh" vs the owner's herdr path**, which has no ssh environment: T12 adds the measured
  round trip (5 ms [추정]). *tui* §14.5 (video across machines) is settled by T8.
