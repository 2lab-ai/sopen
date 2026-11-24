# xtools as a TUI first and the same screen as a GUI — prototype and measurements (XR-T)

2026-11-24. Research for owner message 23 (`.prd/xtools/ssot.md` item 2). No product code changed:
sopen was read at `docs/viewer-ssot` 255a52f (copied with `git archive` into the prototype folder;
every `file:line` below is that commit) and the copy was built only in my own folder. Nothing was
changed on either machine; no window was opened on the Linux laptop's display; no owner terminal or
pty was written to (their `/proc/<pid>/environ` and `ps` were read).

Evidence tags: **[M]** measured here — receipt path given (paths are relative to the prototype
folder `xtools-tui-research/`, a session scratch folder on this Mac;
on the Linux laptop `/tmp/sopen-research-tui/`) · **[S]** read in source or docs (`file:line` / URL)
· **[추정]** inference, not verified.

## 0. Answers first

- **Recommended architecture: (A) one cell-grid UI core with two backends.** xtools draws every
  screen into a grid of cells (character + foreground + background; ratatui's `Buffer`); the terminal
  backend prints it (ratatui + crossterm), the GUI backend paints the same cells with the VGA font in
  the eframe window and hosts pixel regions (picture, video frame, web view) at cell rectangles.
  The prototype's file list + F-key row + F7 dialog, typed into in a terminal and in the window, give
  **0 foreground and 0 background differences in 3,000 cells, and exactly 1 character difference —
  the mode button's label (`[GUI]` in the terminal, `[TUI]` in the window), by design** — across
  three terminal emulators (vt100 crate, tmux 3.7b, vt100 on Linux) and the window (painted shapes
  and its own screenshot) [M §2]. Input stays egui's in both backends (crossterm events become
  `egui::Event`s for a headless context), so the key table, focus/memory code and the tests' input
  side survive; only drawing and ≈75 pixel-level test queries are rewritten (§11) [추정: the
  crossterm→egui translation is not prototyped; a headless egui pass is measured].
- **(B) keep egui + a separate TUI kept 1:1 by tests: killed (dominated).** A cell-by-cell test needs
  the GUI on the grid first, and today's dialogs, hex toolbar and image window are not, even with the
  panel insets removed [M §1.3]. B = the same grid work + a second renderer for every screen [추정 cost].
- **(C) egui stays the only UI, the TUI is egui's shapes projected onto cells: viable as a bridge,
  not as the architecture.** Run on today's screens with the two panel insets removed and the
  window's Hangul font: the list, text viewer and tree land on the grid (0 glyphs off in y), but a
  Korean name lands 4 glyphs on occupied cells (egui lays Hangul at 13.84 pt, the terminal at 16),
  and the F7 dialog (158 glyphs off in y, field/buttons as line debris), the hex window (1,337 off in
  y) and the image window stay off [M §1.3]. Those need the same grid rewrite as A; what C keeps is
  ~200 drawing calls. It also puts an egui pass with font loading into every TUI start (first
  headless pass 194 ms on the Mac, 89 ms on Linux [M]) and keeps parity only as strong as a lint's
  coverage. **Keep its projector as the lint** for whatever egui still paints during the migration.
- **(D) the window as a built-in terminal emulator of the TUI's byte stream: killed for the GUI
  role.** No terminal protocol carries a native view, so the web view (message 18) would need a side
  channel anyway [추정], and pictures ride the pty — kitty frames over ssh measured 6–8 fps at 640×360
  [M §7.4] where a window reads the file itself.
- **Scope of the proof:** A's kill tests ran on the list, the F-key row, one dialog, three region
  kinds and the switch. The git lanes as glyphs, the image/media selection in cells and the editor
  are designs, not measured [추정].
- **Switch:** in one process the TUI opened the window and came back twice (first window 361 ms to
  first frame, second 60 ms; eframe keeps winit's event loop per thread) with the state — a folder
  made in the window, a dialog's typed text and its error — round-tripping [M §8]. Across machines,
  a TUI on the Linux laptop with no display handed 4,885 bytes of state over ssh to the Mac, the
  window opened there, made a folder, closed, and the Linux TUI showed it [M §8].
- **The owner's terminals [M §7.1]:** Linux laptop — **kitty 0.48.2** and **herdr-app** (a GPUI app
  with a vendored libghostty-vt, showing herdr-core terminals); Mac — **herdr-core** terminals
  (`TERM_PROGRAM=ghostty` is set by herdr-core, Ghostty.app is not installed) viewed from elsewhere
  through `herdr-core connect` under sshd. The live `sopen .` on the Mac forwards to the laptop
  (Mac config `target = <the Linux laptop>`). No tmux/zellij/screen in any owner shell's chain; **herdr sits
  between xtools and every owner terminal but kitty**. Whether herdr shows kitty images is unknown —
  the owner probe (§13) settles it.
- **Video in a terminal:** fine locally (WezTerm, 640×360: kitty 59 fps, kitty via temp file 94 fps,
  sixel 41, half blocks 221) and poor over ssh (kitty 6–8 fps, sixel 11, half blocks 23); the
  Mac→laptop link measured 1.25 MB/s in one run and 6.0–6.6 MB/s in two later runs [M §7.3–7.4]. A
  terminal gets decoded frames (0.9 MB each as kitty) where a window gets the compressed file, so
  video across machines stays a window job [추정 from those numbers].
- **Keys:** the table works in a TUI under the kitty keyboard protocol; under legacy encoding
  Ctrl/Cmd+digits (the F-key aliases), Ctrl+Shift+letters, Shift+F3 and every ⌘ binding are lost or
  turn into other keys (Ctrl+3 arrives as Esc, Ctrl+8 as Backspace) [M §4].
- **Owner decisions** are listed in §14.

## 1. Today's GUI against the cell grid

### 1.1 What is already a grid [S]
`chrome.rs:1-19` (PC Shell skin), `CELL = 8×16` (`chrome.rs:53`), the F-key row's whole-cell slots
(`chrome.rs:789-800`), the scroll bar's ↑ ░ ↓ cells and one-cell thumb (`chrome.rs:967-996`), CP437
glyphs for the tree (`chrome/tree.rs:10-11` `▼ ►`) and the git nodes (`chrome/git.rs` `■ ◙ ≡ ○ » ► ↔`),
the list's columns in characters (`browser.rs:2452-2462`).

### 1.2 What is not [S]
| Item | Where | Today | On the grid |
|---|---|---|---|
| Half-cell item spacing | `chrome.rs:449` | 4 pt between controls | whole cells |
| Dialog margin | `chrome.rs:494-501` | 2 cells × 1.5 cells | 1 cell |
| Shadows | `chrome.rs:414-415`, `:501` | translucent black (α 170) over anything | the cells' glyphs in dark grey on black (DOS shadow) |
| Panel inset | `browser.rs:2426` `.shrink(2.0)` | panels 2 pt inside their half | none |
| Status bar | `chrome.rs:774-780` | `CELL.y + 4` tall, 2-pt margin | one row |
| Frames | `chrome.rs:885-897` | strokes (a double line = two outlines 2 pt apart) | `╔═╗║` / `┌─┐│` glyphs |
| Rule above the foot | `chrome.rs:1066-1070` | an `hline` at mid-cell | `─` with `╟ ╢` / `├ ┤` joints |
| Text fields | `chrome.rs:468` `TextEdit` | egui field, 2-pt caret (`chrome.rs:421`) | black cells, caret = a cell (terminal cursor / 2 scan lines) |
| Buttons, choices | `chrome.rs:474-476`, egui `Button` | framed widgets with 2-pt padding | `[ Make ]` cells, the bar colour when focused |
| Labels / layouts | egui `Label`, `horizontal`, `Grid` | proportional flow | text put at cells |
| Git lanes | `git_panel/graph_view.rs:50-52`, `:634-636` | 1-pt line segments at mid-cell, dashed for uncommitted | lane glyphs `│ ─ ╭ ╮ ╰ ╯ ┼` (as `git log --graph`/tig) [추정 design] |
| Hex window | `hex.rs` | text on rows, offset by panel insets | already text; only the insets move (§1.3: 45 of 1,497 glyphs off in x) |
| Pictures, video frames | `image.rs`, `media.rs` textures | pixels | pixel regions at a cell rect (§6) |
| Selection rectangles (image/media) | `select.rs`, `PALETTE.select*` (`chrome.rs:290-293`) | pixel-exact rect + handles + shaded outside | GUI: stays pixel-exact *inside* the region; TUI: in cell units over the picture |
| Timeline / waveform | `media/timeline.rs`, `media/wave.rs` | line segments per pixel column | block glyphs `▁▂▃▄▅▆▇█` per cell (2 rows for L/R) [추정 design] |
| Progress, spinner | `ProgressBar` ×2, spinner strokes | widgets | `█▓░` bar cells, a `|/-\` cell |
| Wide characters | `fonts.rs` system fallback | Hangul at 13.84 / 14.72 pt advance | 2 cells, glyph placed at the first (§5) |

Call sites per item (non-test `src/gui`, `tools/gaps.sh` → `receipts/gaps.txt`) [M]: add_space /
item spacing 85 · sub-cell margins 9 · shadows 5 · TextEdit 26 · buttons/choices 138 · labels 130 ·
layouts 72 · ScrollArea 9 · scroll bars 11 · line strokes 30 · textures 12 · progress/spinners 8 ·
modals 28 · painter.text/galley 57 · rect_filled 27 · fit_left/fit_start 21 · egui panels 35 ·
pointer/drag/hover 87 · wheel 37. Size of what draws them, non-test lines (`receipts/gui-areas.txt`)
[M]: browser list/ops/keys/chrome/help 9,629 · tree 831 · git panel 13,986 · text viewer 8,486 · hex
+ binary dialog 9,473 · image 8,441 · media + playback 10,963 · shell/save-as/streamed/fonts 3,050 —
64,859 in all (of `src/gui`'s 136,614; tests are the rest).

### 1.3 Today's screens projected onto cells [M]
`project.rs` (prototype) copied into my sopen copy as `src/gui/cellproj.rs`, one accessor added to the
test kit, and one test (`src/gui/browser/tests/xrt.rs`) driving the real browser at 800×480 pt =
100×30 cells: `receipts/option-c/{1-list,2-f7-dialog,3-text-viewer,4-hex,5-image,6-tree}.txt`.

| Screen | glyphs | off x | off y | rects off grid | line segments | translucent |
|---|---|---|---|---|---|---|
| list | 587 | 382 | 447 | 10/27 | 14 | 0 |
| F7 dialog | 745 | 405 | 605 | 16/34 | 35 | 1 |
| text viewer | 317 | 21 | 204 | 4/24 | 8 | 0 |
| hex | 1,497 | 45 | 1,370 | 13/32 | 12 | 3 |
| image | 247 | 70 | 130 | 20/40 | 20 | 5 |
| tree | 558 | 379 | 455 | 10/28 | 16 | 0 |

The list and the hex rows still read correctly after rounding (the offsets are the 2-pt insets);
the dialog's `name` field and buttons become `┌───┬…`, `name새─폴더┼…`, `┬───┐┬─────┐` debris
(`2-f7-dialog.txt` rows 15–19). (The test kit has no system font, so Hangul here is not the window's.)

**Discriminating rerun** (`sopen-copy-c2/`: `.shrink(2.0)` removed at `browser.rs:2416,2426`, the
status bar's `+ 4.0` height and 2-pt margin removed at `chrome.rs:776,779`, and eframe's system font
provider added to the test kit's context — `receipts/option-c-fixed/`):

| Screen | glyphs | off x | off y | collisions | rects off | segments off centre |
|---|---|---|---|---|---|---|
| list | 905 | 42 | **0** | **4** (Hangul) | 0 | 0 |
| F7 dialog | 1,063 | 65 | 158 | 4 | 6 | 14 |
| text viewer | 318 | 21 | 0 | 0 | 1 | 0 |
| hex | 1,497 | 45 | 1,337 | 0 | 13 | 8 |
| image | 247 | 70 | 97 | 0 | 20 | 9 |
| tree | 579 | 1 | 0 | 0 | 2 | 0 |

`한글 문서 초안.txt` is laid at x = 7, 21, 43, 57 pt (`1-list.txt` examples): 13.84-pt Hangul
advances, so two glyphs share cells; a terminal needs 10 cells where egui uses 8, and cuts the name
elsewhere. The remaining off-grid glyphs are the dialog's modal placement and widgets, the hex and
image windows' egui toolbars. (The projector draws a double-line corner as `+`: its own limit.)

## 2. The 1:1 proof (Q2) [M]

Prototype `proto/` (3,268 lines): `core.rs` (state, keys, the screen into a `Buffer`), `term.rs`
(terminal backend), `gui.rs` (window backend), `dump.rs` (cell dump + diff), `project.rs`
(egui shapes → cells), bins `xtui`, `vtdump`, `celldiff`, `gfxbench`, `gfxprobe`, `fontprobe`.
Screen: 100×30, fixture with Korean names and an emoji, keys `Down Down F7` and typed `새 폴더 2026`.

- **Terminal:** `xtui` in a pty of my own (`tools/ptyrun.py`, legacy key encoding), its bytes parsed
  by the vt100 crate (`vtdump`; a wide glyph's second cell takes the first's colours, as terminals
  paint it); independently inside a private tmux server (`tools/tmux-dump.sh`, `capture-pane -e`).
- **GUI:** the window's paint run headless in a plain egui context with the window's fonts, its
  shapes projected back onto cells (glyph positions, rect fills, the caret rect) — not the buffer
  it was given; and the real window's screenshot checked cell by cell (`gui::pixel_check`).

| A | B | char | fg | bg | caret |
|---|---|---|---|---|---|
| vt100 (Mac) | core buffer | 0 | 0 | 0 | equal (37,13) |
| vt100 (Mac) | GUI shapes (Mac) | **1** `(86,0) G≠T` | 0 | 0 | equal |
| tmux 3.7b | GUI shapes | 1 (same) | 0 | 0 | equal |
| tmux 3.7b | vt100 | 0 | 0 | 0 | equal |
| vt100 (Linux) | GUI shapes (Linux) | 1 (same) | 0 | 0 | equal |
| GUI shapes (Linux) | GUI shapes (Mac) | 0 | 0 | 0 | equal |

On Linux no window was opened (the laptop's GUI side is the headless paint, shapes only); only the
glow renderer was exercised (sopen's default tries wgpu first). Receipts: `receipts/onetoone/*.dump`,
`receipts/linux/*.dump`. Window screenshot
`receipts/onetoone/gui-f7.png` (1600×960 px, 2 px per point): backgrounds 3000/3000 cells, glyph
foregrounds 1602/1603 — the one miss is 🎵, whose thin anti-aliased glyph has fewer than 2 pixels of
the exact colour. The window's ui pass: p50 0.51 ms (first frame 331 ms with font loading).

## 3. The gap, inventoried (Q3)
§1.2 is the inventory with counts. What each becomes is in its last column. Nothing on the list is
impossible on a grid; three things change the look visibly and are the owner's taste (§14): shadows
(translucent → DOS dark-grey cells), widget frames (egui buttons/fields → bracketed cells / black
bars), and the half-cell paddings (gone).

## 4. Input axis (Q4) [M]
`tools/keyaxis.py` played each binding's bytes into `xtui` (crossterm 0.29) in my own pty and read
what it decoded (`receipts/keys/keyaxis.txt`):

| Binding (keys.rs table / shell rule) | legacy encoding | kitty protocol (flag 1) |
|---|---|---|
| F1–F12, Shift/Alt/Ctrl+F5–F10, Shift+Del, Ctrl+PgUp, Ctrl+←/→, Shift+Tab | decoded | decoded |
| **Shift+F3** (git: file at revision) | `CSI 1;2R` → **nothing** (read as a cursor report) | `CSI 13;2~` → Shift+F3 |
| **Ctrl+1…0 → F1…F10** (shell rule, 04 §6.1) | Ctrl+1/0 → plain `1`/`0`; **Ctrl+3 → Esc**; **Ctrl+8 → Backspace**; Ctrl+7 → Ctrl+7 | all Ctrl+digit |
| **Cmd+digits, Cmd+Z/E/…, ⌘⌫** (macOS) | never sent (no ⌘ in legacy) | `CSI …;9u` → super+key — when the terminal passes ⌘ through |
| **Ctrl+Shift+letter** (git: fetch all, wide, continue, …) | = Ctrl+letter (shift lost) | distinct |
| Ctrl+I / Ctrl+M | = Tab / Enter | distinct |
| Esc vs Alt+x | lone `ESC` → Esc, `ESC x` → Alt+x (timing) | `CSI 27u` / `CSI 120;3u` |
| Hangul typed | `Char('한')` | same |

Terminals and the protocol [S]: kitty's spec lists kitty, alacritty, foot, ghostty, iTerm2, rio,
WezTerm, xterm.js, Windows Terminal, Warp (sw.kovidgoyal.net/kitty/keyboard-protocol). WezTerm
20240203 answered no `CSI ? u` by default [M `receipts/probe/wezterm-report.txt`]; libghostty-vt in
herdr parses kitty keyboard pushes (strings `InvalidKittyKeyboardFlags`, `pushKittyKeyboard`) [M
strings]. **Bindings that need a terminal alternative:** every ⌘ binding (Terminal.app, iTerm2,
WezTerm, kitty and ghostty bind ⌘+digits to tabs [추정 defaults]); Ctrl/Cmd+digit F-key aliases
under legacy encoding; Ctrl+Shift+letters under legacy; Shift+F3 under legacy. Proposal: in the
TUI the F-key row is clickable (mouse reporting, done in the prototype), Ctrl+1…0 stay the aliases
under the kitty protocol, **Alt+1…0** is added for legacy terminals where it arrives [추정: on the
laptop's keyboard; on a Mac keyboard Option+digit types `¡™£…` unless the terminal treats Option as
Alt, and herdr-app's own bindings are unknown — the probe cannot see bindings], and the Ctrl+Shift
git actions get entries in the existing F10 menu (keys.rs `C::Menu`).

## 5. Wide characters (Q5) [M]
`fontprobe` (`receipts/wide/fontprobe-{mac,linux}.txt`), egui 16 pt with the VGA font first and
eframe's system font provider:

| | unicode-width (terminal cells) | Mac advance (face) | Linux advance (face) |
|---|---|---|---|
| `한 글 漢` | 2 | 13.84 pt (Apple SD Gothic Neo) | 14.72 pt (WenQuanYi Zen Hei; `漢`, `あ` 16.0) |
| `😀 🎵 👍🏽` | 2 | 16.00 (Apple Color Emoji) | 16.68 / **19.92** (DejaVu Sans / Noto Color Emoji) |
| `─ … ░ A é` | 1 | 8.00 | 8.00 |

Row height stays 16 pt; Hangul ink is 14×14–15 px (Mac) and up to 15×16 px (Linux). So **in egui's
own text layout a Korean filename drifts**: 7 Hangul characters take 96.9 pt on the Mac = 12.1 cells
where the terminal gives 14. The prototype's GUI paints each glyph at its own cell, so the grid holds
in both OSes (§2) — the Hangul glyph sits in its 16-pt slot with 1–2 pt to spare; the Linux emoji
overflows its 2 cells by 3.92 pt (clip it to the slot, or scale fallback faces to their cell span —
[추정] one FontTweak). Terminal-side risk: `─ ░ … ↑` and the box glyphs are East Asian *ambiguous*
width; a terminal set to "ambiguous = wide" under a CJK locale (the laptop's `LANG=ko_KR.UTF-8`)
would break every frame [추정]; the probe measures it (WezTerm: all 1 [M]).

## 6. Pixel regions in the GUI backend (Q6) [M]
- **Picture at a cell rect:** F9 (preview) on `사진 2026-11-24.png` → region cells (51,2,48,25) →
  points (408,32)–(792,432); `receipts/regions/image-region.png` shows the picture exactly inside the
  frame; the other 1,800 cells pass the pixel check.
- **Video at a cell rect:** a 320×180 texture re-uploaded every frame: 300 frames in 3.21 s = 93 fps,
  ui pass p50 0.47 ms.
- **Native child view at a cell rect (macOS):** an `NSBox` subview of winit's NSView placed at the
  region of `index.html`: `frame (408,32 384x400)` = the cell rect in points, kept on every frame
  (`receipts/regions/` run log; the GL screenshot does not include subviews, so the visible check is
  [미측정]). VR-W measured the same mechanism with `WKWebView` (`.prd/research/web-view.md` §0, on
  main e7df2f0); on Linux its web view is an egui texture from an offscreen WebKitGTK — i.e. a texture
  region, the same as a picture.

## 7. Terminal graphics (Q7)

### 7.1 The owner's terminals [M]
- **Linux laptop:** `kitty` pid 2793 (`/usr/bin/kitty`, 0.48.2) with fish on pts/0 —
  `TERM=xterm-kitty KITTY_WINDOW_ID=1 COLORTERM=truecolor`; **herdr-app** pid 13114
  (`~/.local/share/herdr-app/herdr-app`: GPUI on wgpu, `crates/ghostty-sys`, "Ghostty (libghostty-vt)"
  in its THIRD-PARTY-NOTICES) whose child `herdr-core connect` views herdr-core terminals pts/1–3 —
  `TERM=xterm-256color TERM_PROGRAM=ghostty HC_TERMINAL_ID=t1…t3`. No tmux, zellij or screen (`tmux`
  not installed; not in `ps`). `LANG=ko_KR.UTF-8`, KDE Plasma Wayland.
- **Mac:** 55 shells are children of `herdr-core hold` (pid 60385); herdr-core's binary sets
  `TERM=xterm-256color, COLORTERM=truecolor and TERM_PROGRAM=ghostty` (its own help string) and
  embeds libghostty-vt; **Ghostty.app is not installed** (Spotlight: none). The Mac's herdr is viewed
  by `herdr-core connect` (pid 81862) whose parent is `sshd-session: <owner>@notty` — someone
  attached over ssh [추정: the laptop's herdr-app; its ssh config has an alias for this Mac]. Terminal.app has
  one session (ttys062); WezTerm is installed, not running; two tmux servers run `fx` sessions, not in
  any shell's chain. The live `sopen .` (ttys030) runs `ssh … <the Linux laptop> -- exec 'sh' …`: Mac config
  `target = <the Linux laptop>`; the laptop has no target.
- **Tension:** K-L1's note (`.prd/research/fkeys-linux.md` §5, branch docs/xtools-specs a902135)
  reads this Mac's terminal as "Ghostty" and the laptop's as Konsole. Measured: `TERM_PROGRAM=ghostty`
  comes from herdr-core, and the laptop's terminals are kitty and herdr-app, not Konsole.

### 7.2 Protocols [S + M]
| | kitty graphics | iTerm2 OSC 1337 | sixel | half blocks |
|---|---|---|---|---|
| kitty | yes (reference) | — | no | yes |
| herdr-core / herdr-app (libghostty-vt) | **parsed** (strings `kitty_gfx`, shm/file transmission, "kitty graphics protocol error"); drawn by herdr-app? **[미측정 → probe]** | — | no parser (no `sixel` string) | yes |
| WezTerm 20240203 | yes with `enable_kitty_graphics` (a=q answered OK) [M] | yes (`ReportCellSize`) [M] | yes (DA1 `4`) [M] | yes |
| ghostty / iTerm2 / foot / Konsole / tmux | ghostty kitty; iTerm2 its own; foot sixel; Konsole sixel buggy (ratatui-image table, github.com/benjajaja/ratatui-image); tmux: passthrough only with `allow-passthrough` [추정] | | | |

Crates: `ratatui-image` 12.0.0-rc.3 (2026-10-18, needs ratatui ^0.30.1 — the prototype's ratatui
0.30.2) picks by env, then terminal queries, then half blocks [S crates.io, README]. The prototype
implements kitty and half blocks itself (`term.rs`) to measure; `gfx.rs` has all four encoders.

### 7.3 Bytes per frame (offline, `receipts/gfx/gfxbench-mac.txt`) [M]
640×360 (80×22 cells): kitty raw RGB 924 KB · zlib 582 KB (smooth) / 887 KB (camera-like noise) ·
PNG 287–862 KB · temp-file 296 B (local only) · iTerm2 PNG ≈ PNG · sixel (252-colour) 52–321 KB ·
half blocks 64 KB. 1280×720: kitty raw 3.7 MB, PNG 1.0–3.4 MB, sixel 0.14–1.1 MB (encode 10–62 ms),
half blocks 0.27 MB.

### 7.4 Frame rate in a real terminal (`gfxprobe`, 640×360 camera-like frames, a DSR after every frame) [M]
| WezTerm on the Mac | local | over ssh (probe on the laptop) |
|---|---|---|
| kitty raw | 59 fps, rtt p50 16.5 ms | 6.0 fps, p50 144 ms |
| kitty zlib / PNG | 51 / 52 fps | 7.8 / 8.0 fps |
| kitty temp file (`t=t`) | 94 fps | **nothing shown** (the file is on the other machine; 15 files left unread) |
| iTerm2 | 68 fps | 8.1 fps |
| sixel | 41 fps | 10.8 fps |
| half blocks | 221 fps | 23.4 fps |

`receipts/gfx/wez-local.txt`, `wez-ssh.txt`. A DSR answer says the terminal parsed the frame, not
that it painted it [추정 bound]. Link (`receipts/gfx/ssh-throughput.txt`): RTT 10.8–23.4 ms over
Tailscale; Mac→laptop 1.25 MB/s (100 MB, 19:00), then 6.01 and 6.56 MB/s (30 MB, 19:28);
laptop→Mac 4.3, 8.66, 5.74 MB/s — the link varies 5× within half an hour. On the owner's real path
(TUI on the Mac in herdr, shown on the laptop) every picture byte crosses the Mac→laptop direction:
a 640×360 camera frame is 0.9 MB as kitty → 1.4–7 fps [추정 from the numbers].
**What a non-graphics terminal shows:** half blocks (two pixels a cell, 80×44 "pixels" for a 640×360
region) — `term.rs` `half_blocks`, used by every terminal-side receipt above.

## 8. The switch (Q8) [M]
- **Same process:** F12 or a click on the title row's `[GUI]` leaves raw mode and the alternate
  screen, prints one line ("xtools is in a window now — close it, or press F12 there, to come back
  here."), runs the window on the main thread with the state, and re-enters the TUI with the state
  the window returns (`term.rs` `to_gui`). Two round trips in one process: first window 361 ms to
  first frame, second 60 ms (eframe's thread-local event loop, `eframe-0.37.0/src/native/run.rs:67-71`
  [S]); final state `touched_by = [tui, gui, tui, gui]`, the folder made in the window listed in the
  TUI, the second window's open dialog (`gui폴더`, caret 5, error "exists") carried back
  (`receipts/switch/`). The terminal cannot repaint while the window runs on the main thread; a
  mirror would need the TUI drawn from another thread [추정].
- **Across ssh:** `XTUI_TARGET=<this Mac>` — the laptop's TUI (no DISPLAY/WAYLAND_DISPLAY) sent the
  state as JSON on the input of `ssh -T <this Mac> -- exec '<xtui>' '--gui' '--state-stdin' …`
  (every word single-quoted, as sopen's forward does); the Mac's window showed it (first frame
  192 ms), made `맥에서-만듦`, closed; the state came back on its output (`XTUI-STATE <json>`), the
  laptop's TUI listed the new folder; 1.1 s end to end (`receipts/linux/x.keylog`, `x.after.vt.dump`).
- **What is handed over:** folders of both panels, the bar, the selection, mode, an open dialog with
  its typed text and caret (all in the prototype's 4,885-byte state). For sopen add: the open file
  and the viewer's position/mode/encoding (08), and the source host so the target opens
  `<this machine>:<path>` as §4a already rewrites. **Unsaved edits:** 07's journal is per machine
  (`<cache>/.edits/<host>/…`, `07-streaming-edit.md` §3); the switch writes it (07 already writes it
  on focus loss) and either carries it with the state or asks to save first — owner decision (§14).
- **Back from the window:** F12 or closing it returns to the TUI on the same machine; across ssh the
  window's exit carries the state home.

## 9. Contracts that flip (Q9, `.prd/01-spec.md`)
- **A11 / §3a** — `sopen <folder>` opens xtools **in the terminal it was typed in** (TUI) by default,
  local or remote folder; the window is the switch (or `--gui`).
- **A9 / A10 / §4a** — today `target` sends every open to the target's screen. With the TUI default,
  `sopen <folder>` stays in this terminal; **the target becomes where the window opens when the owner
  switches** (and where `sopen <file>` of a GUI-only kind — image, video, web page — opens directly)
  [proposal]. The owner's measured setup (sopen typed in a Mac herdr terminal shown on the laptop,
  `target = <the Linux laptop>`) is exactly this case: the TUI runs on the Mac, the window must open on the
  laptop, not on the Mac's screen that nobody looks at.
- **`--here` / `--target`** keep their meaning for the window (and the files that need one).
- **No display (A6, §3, §4a)** — today: fetch and print the path. With a TUI there is always a
  screen: the TUI opens; the switch to the window goes to the target, else says why there is none.
- **A2 / message 21** — text opens in xtools's editor (XR-E) in the TUI; VS Code / Zed stay F4.
- **A14 (F-keys)** — the macOS HID switch and the Wayland inhibitor apply to the window only (§10).

## 10. Message 19 in a TUI (Q10, sources only)
In a TUI xtools owns no window, so the macOS per-window `HIDFKeyMode` switch (04 §6.2) and K-L1's
shortcuts inhibitor (a `wl_surface` of the requesting client, `fkeys-linux.md` §5) do not apply; what
reaches the TUI is whatever the terminal sends after the compositor and the terminal have taken
their shortcuts. On the laptop KWin consumes bound brightness keys before any client (as K-L1 read
it: `fkeys-linux.md` §5, KWin `input.cpp:1076-1082`), and the kitty protocol has no brightness code (its functional-key
table lists media and volume only, sw.kovidgoyal.net/kitty/keyboard-protocol). The owner's kitty can
take the keyboard itself: its `grab_keyboard` action and `--grab-keyboard` flag hold the same
`zwp_keyboard_shortcuts_inhibitor_v1` (`/usr/lib/kitty/kitty/simple_cli_definitions.py:325-331`,
`boss.py:3738-3744`, strings of `glfw-wayland.so`), after which the brightness key reaches kitty —
whether kitty then sends it as F1 needs a `map` in the owner's kitty.conf [추정]; herdr-app has no
inhibitor string [M]. A local macOS TUI could flip `HIDFKeyMode` on the terminal's focus reports
(DECSET 1004; WezTerm reports it [M]) — over ssh or herdr it cannot reach the Mac's keyboard. So in
TUI mode the reliable F-keys are the clickable row and the Alt+digit aliases (§4).

## 11. Tests (Q11)
Today: 1,305 `#[test]` in `src/gui`, 1,157 of them in files driving windows through the egui test
kit, and ≈1,158 lines querying painted shapes (`find_text`, `colors_of`, `rects`, `segments`, …)
[M grep; my added test excluded]. Under each option:
- **A:** keep **egui's input side as the neutral input** in both backends — the terminal backend
  turns crossterm events into `egui::Event`s and runs a headless `egui::Context` pass (the headless pass is measured — the
  prototype's `headless_dump`; the translation is not prototyped [추정]), so the key table (`keys.rs:589-595` takes
  `egui::InputState` / `Vec<egui::Event>`), focus, memory and repaint calls stay. Today's controller
  code uses egui for: 372 lines of `Key`/`Modifiers`/`Event`, 118 repaint requests, 102 `ctx.input`,
  40 focus/memory, 45 font measurements, 17 `ctx.data`, 13 viewport commands, 9 textures
  (`receipts/ctx-uses.txt` [M]); only the 45 font measurements (→ unicode-width) and the drawing
  change. Tests: the input side (the Driver's key/pointer events) stays; of the shape queries
  (`receipts/test-queries.txt` [M]) the text ones — `find_text` 506, `click_text` 138+21, `texts` 252,
  `text_starting_with` 92+29, `colors_of` 52+12, `rows_of` 8 — re-implement over the cell grid with
  the same signatures; the pixel ones — `pictures` 35, `rects` 16, `segments` 12, `galleys` 9,
  `strokes` 3 — are rewritten (≈75). Two harness tests keep the backends honest for every screen:
  terminal bytes through vt100 = the buffer; the window's shapes projected = the buffer (§2). Pixel
  regions are masked and asserted as rects. A screen's tests change in the step that moves it.
- **B:** both suites twice + a cross-diff; the GUI side cannot pass a cell diff until it is on the grid.
- **C:** today's suite stays; a projection lint must pass for every screen; nothing forces screens no
  test visits.

## 12. Migration (Q12)
Each step ships alone (prerelease) and nothing in it waits for a later one. Sizes are [추정], scaled
from the prototype (list + F-key row + dialog + title/status in `core.rs` 928 lines incl. state and
keys; terminal backend 345; GUI cell painter + regions 591; dump/projection/diff 800). Every step
also moves its screens' tests (§11: text queries re-pointed to the grid, pixel queries rewritten) —
the sizes below are product code only; test churn is of the same order [추정].

1. **X1 — cell core + both backends + the list (first useful slice).** `xtools` cell API (put/fill/
   frame/scroll bar/F-key row/dialog/field/button), terminal backend (ratatui + crossterm, kitty
   keyboard when offered, mouse), GUI cell painter in today's window, the dump/diff harness; the
   browser's list, title, status, F-key row and the list's dialogs (F2/F5/F6/F7/F8, groups) drawn in
   cells **in both**; `sopen --tui <folder>` and F12 → today's window in-process. Viewers, tree, git
   panel stay egui inside the window and are F12-only from the TUI. ≈ +6–8k / −2–3k lines.
2. **X2 — TUI by default + the cross-machine switch** (state hand-over through the §4a forward,
   window on the target, back on close), the 01 rows of §9 rewritten. ≈ +1–1.5k.
3. **X3 — text + hex viewers in cells** (08's views 1/2/3 and encodings land here, cell-native).
   ≈ text 2–3k, hex 2–3k changed (hex is nearly a grid already, §1.3).
4. **X4 — tree + git panel in cells** (lanes as glyphs, details, dialogs, planner). ≈ 6–9k changed —
   the largest; its egui dialogs keep working in the window until then.
5. **X5 — the editor (XR-E)** written cell-native from the start (F4 layout; the left panel's
   directory/git tabs need X4's git view in cells, hence after it).
6. **X6 — image + media windows' chrome in cells, pictures/video/web view as regions**; selection
   rectangles pixel-exact in the window, cell units in the TUI; timeline/waveform as block glyphs.
   ≈ 5–8k changed.
7. **X7 — terminal pictures and video**: protocol pick (query → env → half blocks; ratatui-image or
   the prototype's encoders), local kitty temp-file/shm fast path, in-band capped rate over ssh.
   ≈ 1–2k.
**Frozen meanwhile:** new egui screens (08's screens, the editor) — they are written in cells; egui
widget work on screens about to move. Bug fixes continue.

## 13. Owner-run probe (only the owner's terminals can answer)
`xtools-term-probe.py` (prototype folder; copy on the laptop at `/tmp/sopen-research-tui/xtools-term-probe.py`).
Read-only: it writes no file and changes no setting; it asks the terminal it runs in standard
questions (XTVERSION, DA1/sixel, kitty keyboard, kitty graphics, cell size, iTerm2, focus reports,
synchronized output) and measures how many cells `A ─ … ░ 한 😀 👍🏽` advance; `--show` also draws a
kitty, a sixel and a half-block square. Steps, as the owner, in each terminal:
1. kitty on the laptop: `python3 /tmp/sopen-research-tui/xtools-term-probe.py --show`
2. a herdr-app pane on the laptop (a laptop shell): the same command.
3. a herdr pane of the Mac: `python3 <prototype folder>/xtools-term-probe.py --show`
Paste each report and say which squares showed. Under herdr an `a=q` "OK" only proves that
libghostty-vt parsed the query; whether the picture reaches the screen is the owner's eyes on the
`--show` squares. Validated in WezTerm here (`receipts/probe/wezterm-report.txt`) and the `--show`
path in my own pty (`receipts/probe/show.bytes`: the three squares sent, a key ends it, pictures
deleted and the alternate screen left on exit).

## 14. Owner decisions
1. Where `sopen <folder>` opens with a target set: in this terminal (proposed) or the window on the
   target (today).
2. The switch's key and button: F12 + the title row's `[GUI]`/`[TUI]` (proposed).
3. The look changes the grid forces on the window: DOS cell shadows, bracketed buttons, black field
   bars, no half-cell paddings (compare `receipts/onetoone/gui-f7.png` with today's dialog).
4. Unsaved edits on a cross-machine switch: carry the journal, or save first.
5. Video in a terminal across machines: a low-rate preview (half blocks / sixel) or always the window.
6. Top-row F-keys in the TUI on the laptop: whether to set kitty's `grab_keyboard` (all KDE
   shortcuts pass to kitty while it is focused) and map the brightness keys to F-keys in kitty.conf.
7. The F-key aliases in the TUI: Alt+1…0 (proposed) in place of Ctrl/Cmd+1…0.

## 15. Receipts
`proto/` (sources), `target-proto/release/` (Mac binaries), `/tmp/sopen-research-tui/target/release/`
(Linux binaries), `receipts/onetoone/`, `receipts/linux/`, `receipts/switch/`, `receipts/regions/`,
`receipts/option-c/`, `receipts/keys/`, `receipts/gfx/`, `receipts/wide/`, `receipts/probe/`,
`receipts/gaps.txt`, `receipts/gui-areas.txt`, `tools/` (ptyrun, tmux-dump, keyaxis, gaps, count),
`sopen-copy/` (255a52f + `cellproj.rs`, `tests/xrt.rs`, one test-kit accessor), `sopen-copy-c2/` (the same + insets removed + the test kit's Hangul font), `receipts/option-c-fixed/`, `receipts/ctx-uses.txt`, `receipts/test-queries.txt`.
