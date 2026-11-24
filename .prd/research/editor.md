---
title: sopen — xtools's built-in editor (XR-E research)
type: research
tags: [sopen, xtools, editor, research, measurement]
---

# The built-in editor: Turbo C / PC Tools feel, F4 in place, Dir and Git tabs (XR-E)

2026-11-25. Research unit XR-E of [`../xtools/loop.md`](../xtools/loop.md), for item 3 of
[`../xtools/ssot.md`](../xtools/ssot.md) (owner message 21). No product code changed: sopen was read
at main 77b0beb; every `file:line` below is that commit. The prototypes ran in a session scratch
folder, called **the prototype folder** below (`xr-e/`: `proto-buffer/`, `hl/`, `mockups/`, a
scratch clone of 77b0beb for the key replay); receipts are named by their path there.

Owner message 21 (2026-11-24), verbatim:

> 그리고 에디터도 디폴트로 로컬 에디터에 git graph쓸수 있도록 해줘 이거 좀 생각해서 지금 이 90년대 취향으로 pctools/터보 c 에디터 감성으로 간단한 텍스트 에디터 + git graph로 깃작업할수 있는 에디터로 만들어줘. 신택스 하일라이트랑 디렉토리 뷰어가 있어야함. 아니 프리뷰를 보다가 f4를 누르면 디렉토리 패널 자체를 왼쪽으로 밀고 프리뷰 패널이 그대로 에디터가 되도록해주곻 왼쪽 디렉토리 뷰에 탭을 추가해서 디렉토리/git뷰를 전환할 수 있도록해줘. 이거 좀 생각해서 리서치하고 구현해줘

Evidence tags: **[M]** measured here (command and receipt in the prototype folder) · **[S]** read in
sopen's source or specs (`file:line`), or in a published document (URL, page) · **[P]** a period
manual or screenshot (URL, page or leaf). Every quote was found again in the item's saved OCR text
or read off its page image by this unit's reference pass; the Turbo C++ frame figure, the Commons,
"About" and PC Tools 6 captures and the Notepads F-key row were viewed again for this note ·
**[추정]** inference or memory, not checked.

## 0. Recommended design (answers first)

- **The look is the one xtools already wears, plus Turbo's window** (§1.3):
  - The two programs the owner named share a skin: PC Tools 6 Desktop's menu bar is black on cyan
    with a red program block and the clock, and its Notepads F-key row is
    `1Help 2Index 3Exit 4Load 5Save 6Find 7Spell 8 9Swap 10Menu`. Both are xtools's title and F-key
    rows today [P, S].
  - The editor adds Turbo C++ 3.0's edit window: a double frame, `═[■]═` close box, the name
    centred, `═1═[↕]═` zoom box, `* line:col` bottom left [P]. The title row carries the menu
    names (`File Edit Search Block Git Options`, F10).
  - The marked block is Turbo's blue on cyan. The status row carries `Insert Indent` and Turbo C
    2.0's Find prompt.
  - Text keeps the viewer's syntax colours, so F4 changes no colour.
- **Keys: a WordStar / Turbo set that survives a terminal** (§1.4, measured through crossterm and
  sopen's translator):
  - kept: Ctrl+K blocks (armed, as 08's F9 arms), Ctrl+Y, Ctrl+T, Ctrl+N, Ctrl+L, Alt+Bksp undo,
    Shift-selection and Ctrl+Ins / Shift+Del / Shift+Ins, Ins, F2 Save, F6 next window, F10 menu;
  - dropped: the Ctrl+Q prefix and Ctrl+W, which are sopen's close (`closing()`), and the WordStar
    diamond and Ctrl+C / X / V / S / Z / F / A / R / G, which are sopen's Cmd/Ctrl meanings.
    Ctrl = Cmd in a terminal and on Linux [M].
  - Row: `1Help 2Save 3Exit 4View 5Save 6Panel 7SaveAs 8 9View 10Menu`.
- **F4 layout** (§2):
  - F4 on a text file — in the list, the preview or the Enter viewer — narrows the directory panel
    into a column at the left edge (30 % of the width, 20–40 cells: 24 of 80). The editor takes the rest, the
    preview's place, on the same file and place.
  - The column has two tabs, **Dir** (the same panel) and **Git** (03's git panel of the file's
    repository). Tab switches them; F6 moves the keyboard between column and editor; Esc / F3 goes
    back.
  - **Shift+F4 is today's F4** (VS Code / Zed) everywhere. The layout is written as rule 1a of 02
    §3.1 and drawn at 80×25 and 160×40 (§2.3).
- **The buffer: a piece table over the file's bytes** (§3):
  - lossless in every encoding: 7 of 7 fixtures byte-identical outside the edit, where decoding
    into a rope changed 3 of 7 [M];
  - memory 1.0× the file against ropey's 2.25× [M];
  - edits and lookups well inside a frame even after 20,000 edits [M].
  - Encoding: the file's own, kept; a new encode side in `encoding.rs`; a character the encoding
    cannot hold is refused. BOM and line endings are kept.
  - Up to 256 MiB, read whole on F4 (for a remote file, with progress).
- **Highlighting: syntect, the viewer's own** (§4): 75 syntaxes, 0 bytes added. A keystroke costs
  3–13 ms re-parsed on the existing worker [M]. tree-sitter is 40× faster per keystroke, but costs
  1.2–2.9 MB per language and a second colour mapping [M].
- **Saving** (§5):
  - The image window's route: backup, stage, then `write_atomic_with` (one code path for local and
    SFTP: exclusive temp, owner and mode, fsync, check, `posix-rename`).
  - Conflicts are refused with the edit kept; there is a `Text` journal; the close prompt is the
    hex window's four choices (`hex.rs:4234-4291`).
  - `FilesChanged` after a Save refreshes the Dir and Git tabs.
- **Git** (§6): nothing new. The Git tab is 03's panel (stage with Space, F7 Commit…, diffs), and the
  editor's Git menu acts on the edited file.
- **Routing** (§7.3):
  - `sopen host:/file.txt` opens xtools's editor where a folder would open xtools.
  - Pipes and scripts keep today's VS Code / Zed route.
  - Over 256 MiB, F4 says why and offers Shift+F4.
- **Units** (§8):
  - model units start now: ED-buf, ED-enc, ED-ops, ED-save;
  - screen units wait: ED-screen for X3 and XW-list; ED-layout and ED-git for XW-grid, XW-list and
    X4; ED-dialogs for XW-dialogs; ED-route for XW-term.
- **Irreversible owner decisions: none.**
  - Every choice here is a key, a layout or a default the owner can veto.
  - The ones most worth a look are F2 = Save beside F5 = Save (Turbo's and PC Tools' both), the
    dropped Ctrl+Q prefix, and pipes keeping VS Code / Zed.

## 1. Look and keys from primary references (question 1)

### 1.1 The references read

Each quote below was found in the item's OCR text, or read off its page image ("img"). Page images
are at `https://archive.org/download/<id>/page/n<leaf>.jpg`. The screenshots are modern emulator
captures. Their colours were tallied per cell and mapped to the 16 VGA colours: they show what the
programs drew, not documented defaults.

| Tag | Source |
|---|---|
| TC2-UG | Borland, *Turbo C User's Guide* 2.0 (1988), https://archive.org/details/bitsavers_borlandturer2.01988_23162264 |
| TC2-RG | Borland, *Turbo C Reference Guide* 2.0 (1988), https://archive.org/details/bitsavers_borlandturuideVer2.01988_19310204 |
| TC3-UG | Borland, *Turbo C++ User's Guide* 3.0 (1992), https://archive.org/details/bitsavers_borlandturide1992_32222686 |
| TC1-UG | Borland, *Turbo C++ User's Guide* 1.0 (1990), only where TC3-UG is silent, https://archive.org/details/bitsavers_borlandtur1990_17138131 |
| PCT55-D | Central Point, *PC Tools 5.5 Desktop Manager* (1989), https://archive.org/details/pc-tools-v-5.5 |
| PCT6-D | Central Point, *PC Tools Deluxe 6.0 Desktop Manager* (1990), https://archive.org/details/central-point-pc-tools-deluxe-version-6.0-desktop-manager |
| PCT6-S | Central Point, *PC Tools Deluxe 6 Data Recovery & DOS Shell* (1990), https://archive.org/details/central-point-pc-tools-deluxe-version-6-data-recovery-dos-shell |
| X-TC2 | Turbo C 2.01: https://archive.org/download/msdos_borland_turbo_c_2.01/screenshot_01.jpg, `screenshot_02.jpg`; https://archive.org/download/tc201_202505/tc_001.png |
| X-TC3 | Turbo C++ (the 3.0 layout): https://commons.wikimedia.org/wiki/File:Turbo_CPP_Compiler.jpg; https://winworldpc.com/res/img/screenshots/3x-64181992530a605dfd1d88607613ccda-Turbo%20CPP%203.0%20-%20Debug.png; https://winworldpc.com/res/img/screenshots/3x-79273d6b68a02608429ca95a942b2223-Turbo%20CPP%203.0%20-%20About.png (its About box reads "Version 3.0 … 1990, 1992") |
| X-PCT | https://winworldpc.com/res/img/screenshots/6x-cd98ccf69deea38a74e295b25323d169-PC%20Tools%206%20-%20Desktop.png; https://winworldpc.com/res/img/screenshots/5x-e82f9b2d88a07444696317580a471ea8-PC%20Tools%205.5%20-%20Desktop.png. Their bottom bars belong to a Database window, not to Notepads |

### 1.2 What the screens are

**Turbo C 2.0** [P]:

- **Layout**: "it consists of four parts: the main menu, the Edit window, the Message window, and
  the Quick Reference (Quick-Ref) Line" (TC2-UG p. 80, leaf 99).
- **Menu bar**: `File Edit Run Compile Project Options Debug Break/watch`. Edit is a command, not a
  menu (TC2-UG p. 88, Fig. 5.3; p. 89: "Edit simply takes you into the Editor").
- **Frame**: the active window "has double lines at the top of it and its name is highlighted"
  (TC2-UG p. 89, leaf 108).
- **Status line**: it sits in the edit window's top bar — "Line Col Insert Indent Tab Fill Unindent
  * X:FILENAME.EXT" (TC2-RG p. 410, leaf 433). Its `*` marks a file modified and not saved (p. 411).
  A mode's word disappears when the mode is off (p. 418).
- **Quick-Ref line**: `F1-Help F5-Zoom F6-Switch F7-Trace F8-Step F9-Make F10-Menu` (TC2-UG p. 89).
- **Lines**: "the TC Editor has no wordwrap … The maximum line width is 248 characters"
  (TC2-UG p. 91).
- **Keys**: they are WordStar's — "If you are familiar with … MicroPro's WordStar program, you
  already know how to use the Turbo C editor" (TC2-RG p. 409).
- **Find** is a prompt on that status line, not a dialog: "the status line is cleared, and the
  editor prompts you for a search string". Options are letters: B, L, n, U, W (TC2-RG p. 418).
- **Undo** restores one line only (p. 420). There is no clipboard: "clipboard" has 0 hits in TC2-UG
  and TC2-RG.

**Turbo C++ 3.0** [P]:

- **Layout**: "the menu bar at the top, the window area in the middle, and the status line at the
  bottom of the screen" (TC3-UG p. 23, leaf 41).
- **Menus**: `≡ File Edit Search Run Compile Debug Project Options Window Help` (p. 36, leaf 54, img).
- **Edit window**: "the one with the double-lined border … always has a close box, a zoom box, and
  scroll bars" (p. 27). Fig. 2.1 (p. 28, leaf 46, img; read again for this note) draws its top
  border as `[■]═══ Window Title ═══ 3 =[↑]`, with scroll bars on the right and bottom edges.
  - "A edit window also displays the current line and column numbers in the lower left corner. If
    you've modified your file, an asterisk ( * ) appears to the left" (p. 28).
  - "The first nine windows you open … have a window number in the upper right border" (p. 29).
- **Status line**: `F1 Help F2 Save F3 Open F7 Trace F8 Step F9 Make F10 Menu` (p. 31, Fig. 2.2,
  img). The Commons capture, read again for this note, shows `F1 Help F2 Save F3 Open Alt-F9 Compile
  F9 Make F10 Menu`, `1:1` in the bottom border, `[■]` and `1=[↕]` in the top one.
- **Two key sets**: "The editor has two command sets: CUA and Alternate" (p. 629). With the default
  ("Native"), "Turbo C++ for DOS uses the Alternate command set automatically" (p. 27).
- **Undo**: Alt+Bkspc Undo and Alt+Shft+Bksp Redo (p. 26, Table 2.3, img).
- **Clipboard**: Ctrl+Ins, Shift+Del, Shift+Ins, Ctrl+Del in both sets (p. 631, img). Shift + cursor
  keys select (p. 631).
- **Colours**: settable, but none printed: "Options | Environment | Colors | Edit Window" lists
  Marked Text, Normal Text, Comment, Reserved, Identifier, Symbol, String, Integer, … (p. 37).

**PC Tools Desktop Notepads** [P]:

- "Notepads is the PC Tools Desktop word processor"; a file holds "about 60,000 characters"
  (PCT55-D p. 41).
- **Screen (6.0)**: a menu bar `PCTOOLS Desktop File Edit Search Controls Window` with the clock at
  its right end; `Notepad` centred in the top border; `Line: 1  Col: 1 … SAMPLE.TXT INS` on the
  first row; and the F-key row `1Help 2Index 3Exit 4Load 5Save 6Find 7Spell 8 9Swap 10Menu`
  (PCT6-D p. 48, leaf 56, img).
- **Overtype**: "When INS does not appear in the upper-right corner of the window, you are in
  overtype mode" (PCT55-D p. 42).
- **Keys**: PC keys, not WordStar's. Ctrl+←/→ move by word; Ctrl+Home/End go to the file's start
  and end; Shift + cursor keys mark a block; Shift+Del cuts, Shift+Ins pastes (PCT55-D p. 47–50,
  img).
- **Menus**: "Press Alt or F10 to highlight the command names on the horizontal menu bar"
  (PCT55-D p. 21).
- **Colours**: the 6.0 capture (X-PCT) shows "PCTOOLS V6" white on red and the menu bar black on
  cyan with the clock at its right end. Its bottom bar is the Database window's (`1Help 2Index 3Exit
  4First 5Prev …`), not Notepads', but it is the same bar: digits on black, labels black on cyan.
  Notepads' own labels are the manual page's above. **That is xtools's title row and F-key row
  already** (`chrome.rs:1-9`, `:714-761`, `:808`) [S].
- **PC Shell's own File Editor**: PC Shell 6 has one, but its manual recommends Notepads instead,
  "since it is a much more robust editor" (PCT6-S p. 206, leaf 207).

**Colours seen in the captures** [P, observed]:

- **Turbo C 2.01**: the menu bar light grey with black text and red hot letters, the edit window
  blue, source text yellow (X-TC2).
- **Turbo C++ 3.0**:
  - menu and status bars light grey, with red hot letters and red keys;
  - the edit window blue, its frame, title and `1:1` white;
  - `[■]` and `[↕]` light green, the scroll bars cyan on blue;
  - syntax colours (X-TC3 Debug): reserved words white, identifiers light green, symbols yellow,
    integers light grey, comments cyan;
  - **the marked block blue on cyan** (X-TC3 About).

### 1.3 The look xtools takes

The two programs the owner named share one look with what xtools already wears. It is PC Shell's
look (`chrome.rs:1-9`) [S]:

- the title row is PC Tools Desktop's menu bar;
- the F-key row is Notepads' message bar;
- the editor's body is Turbo's blue window.

So the editor adds Turbo's window furniture and keeps the skin. Every item is cells (10 §4) and
carries no modifiers (T37):

| Part | From | In cells |
|---|---|---|
| Title row | PC Tools Desktop's menu bar (PCT6-D p. 48); TC3-UG's menu bar | ` xtools ` red block, then the editor's menu names `File  Edit  Search  Block  Git  Options` black on cyan, a menu's letter red. Then 08 §8's view strip (`1 Text·2 Hex·3 Raw`) and the clock. F10, or a click on a name, opens a menu as a cells dialog with the DOS shadow (10 §6); a letter chooses within it. No Alt+letter (§1.4). |
| The edit window | TC3-UG Fig. 2.1 (p. 28); the Commons capture | double frame while it has the keyboard, single when the column has it (PC Tools and Turbo agree: PCT55-D p. 42, TC2-UG p. 89). Top border: `═[■]═`, the name centred, `═1═[↕]═`. Bottom border: `* 13:27`, the modified mark and line:column. `[■]` and `[↕]` light green. A click on `[■]` = F3; a click on `[↕]` hides the column (Turbo's zoom). |
| Text | the skin's `syntax_theme` (§4.2) on blue; Turbo 2's yellow text and Turbo 3's colours are noted, not taken | the same colours as the preview and the viewer, so F4 changes no colour |
| Gutter | sopen's viewer (01 A11's line numbers) | 5 cells, dim. Turbo had none, and the preview has one; "그대로" keeps it |
| Marked block | Turbo C++ 3.0's blue on cyan (X-TC3 About, observed) | fg BLUE, bg CYAN. It is not the bar's light red, so a block never reads as the list's bar |
| Caret | TC2-RG p. 409 ("a flashing underbar") | the terminal's cursor in the TUI; in the window, two scan lines (insert) or a whole cell (overwrite) [추정 choice] |
| Status row (row 23) | TC2-RG p. 410's words; Notepads' `INS` | `Insert Indent` (a word absent = the mode off, as TC2-RG p. 418 and PCT55-D p. 42 do), the syntax, the encoding, the line ending, the git letter, then 07's `unsaved — kept`. Find, Go to and the Ctrl+K arming (`^K`) use this row as fields and prompts, as Turbo C 2.0's Find did (TC2-RG p. 418) and as sopen's Go to does (08 §4's Field state) |
| F-key row | Notepads' message bar; TC3-UG Fig. 2.2 | `1Help 2Save 3Exit 4View 5Save 6Panel 7SaveAs 8 9View 10Menu` (§1.4) |

### 1.4 Keys: measured through the terminal, then decided

**Measured** [M]. The candidate keys were sent through crossterm 0.29's real parser in a pty, then
through sopen's translator and the shell's F-key hook. Each was sent as xterm's legacy bytes and as
the kitty protocol's flag 1, using the bytes `term_input/tests.rs`'s own encoder writes. The test is
`xre_editor_keys`, an ignored test in the scratch clone; its receipts are `keys-table.tsv`,
`keys-summary.txt` and `keys-raw.tsv`.

- **Decoded alike in both encodings**: every key below except those listed under "lost under
  legacy". That includes:
  - Ctrl+K, L, N, O, P, T, Y;
  - every plain F-key and Shift/Alt+F2…F10 except Shift/Alt+F3;
  - Shift/Ctrl with the arrows, Home, End, PgUp and PgDn;
  - Ctrl/Shift with Ins and Del;
  - Alt+Backspace.
- **Lost under legacy**:
  - Shift+F3 and Alt+F3 arrive as nothing (read as cursor reports);
  - Ctrl+Shift+letter loses Shift: Ctrl+Shift+Z arrives as Ctrl+Z, Ctrl+Shift+S as Ctrl+S;
  - Shift+Alt+Backspace arrives as Alt+Backspace;
  - Ctrl+Backspace arrives as Ctrl+H;
  - Ctrl+I and Ctrl+M arrive as Tab and Enter.
- **Never keys**:
  - Ctrl+C and Ctrl+X arrive as `Copy` and `Cut`; Ctrl+V arrives as nothing, in both encodings
    (`term_input.rs`'s rules, 10 §4).
  - Ctrl+Q and Ctrl+W arrive as Cmd/Ctrl+Q and +W, which `closing()` turns into a close request
    (`gui/mod.rs:1176-1185`).
- **Raw control bytes**: Ctrl+] (0x1D) decodes as Ctrl+5, and the shell's F-key hook
  (`fkey_events`, `gui/mod.rs:817`) makes it **F5**. Ctrl+\ (0x1C) becomes Ctrl+4, and Ctrl+/ (0x1F)
  becomes Ctrl+7, which the hook makes F7.
- **In every backend Ctrl is egui's `command`** (T26, `ctrl+cmd+` in every decoded row). So in the
  TUI and in a Linux window a WordStar Ctrl+letter **is** sopen's Cmd/Ctrl+letter. Only a Mac window
  tells ⌃ from ⌘. One key set must therefore hold in all three.

**The rule**:

- A key stays only if it decodes as itself under legacy encoding, the hook and `closing()` do not
  take it, and it means the same in the window and the TUI.
- Where it collides with sopen's Cmd/Ctrl meaning (Save, Undo, Find, Go to, Select all, Re-read,
  the clipboard, close) or with an F-slot (07 §10, 08 §7.5, 08 §4), sopen's meaning wins, except for
  F2 Save and F6, which take Turbo's.
- What is dropped stays reachable through the F10 menu, which holds every command (T17's pattern).

| Key | Turbo / WordStar (TC3-UG p. 629–631, TC2-RG p. 412–413, img) | Measured | sopen today | **The editor** |
|---|---|---|---|---|
| arrows, Home, End, PgUp, PgDn; Ctrl+← / → | move; word left/right (both TC++ sets, Notepads) | alike | — | the same |
| Ctrl+Home / End, Ctrl+PgUp / PgDn | window top/bottom and file top/bottom (TC++ Alternate); file start/end (Notepads, TC++ CUA) | alike | Cmd/Ctrl+PgUp = up a folder (list only) | **file start / end, both pairs** |
| Shift + those, Shift+Ctrl+← / → | extend the block (TC++) | alike | — | the same |
| Ins · Del · Backspace · Enter · Tab | insert mode · delete · delete left · new line · tab | alike | — | the same; Enter autoindents, as Turbo C 2.0 opens with `Indent` on its status line (TC2-UG p. 80, Fig. 5.1: `Line 1 Col 1 Insert Indent Tab Fill Unindent`) |
| Shift+Tab | delete character to the left (TC3-UG Table A.1, p. 629–631, both sets) | alike | the other side (list) | unindent the line or block [추정 choice: Turbo's "Backspace unindents" option, TC1-UG p. 77, made Shift+Tab's job] |
| Ctrl+Y · Ctrl+T · Ctrl+N | delete line · delete word · insert line (both sets) | alike | unbound | **the same** |
| Ctrl+K then B K C V Y H I U L T S | block begin, end, copy, move, delete, hide, indent, unindent; mark line, mark word; save (TC2-RG p. 413: "Save and edit Ctrl-K S or F2") | Ctrl+K alike; the second key arrives as a letter | Cmd/Ctrl+K = git filters (git panel only) | **the same, armed as 08's F9 arms** (D4): `^K` on the status row; the next key decides, with or without Ctrl; Esc or any other key disarms and is dropped; no timeout (D33) |
| Ctrl+Ins · Shift+Del · Shift+Ins · Ctrl+Del | copy · cut · paste · clear (both sets) | alike | — | the same; Shift+Ins brings nothing in a terminal, where the terminal's own paste arrives as `Paste` (10 §4) |
| Ctrl/⌘+C · X · V | copy · line down · insert toggle (Alternate) | `Copy`, `Cut`, nothing | the clipboard (egui-winit) | **the clipboard**; WordStar's meanings dropped |
| Ctrl/⌘+S | char left (Alternate) | alike | **Save** (07 §10) | Save |
| Ctrl/⌘+Z · Alt+Bksp | scroll down · Undo (both sets) | alike | Undo (03 §9.2) | **Undo, both** |
| Ctrl/⌘+Shift+Z · Shift+Alt+Bksp | — · Redo (both sets) | **legacy: arrive as Undo** | — | Redo under kitty and in the window; under legacy the F10 menu's Redo (an undo instead is undoable) |
| Ctrl/⌘+F | word right (Alternate) | alike | Find (git panel) | **Find**: a field on the status row, as Turbo C 2.0's Find was; Tab in it moves to `Replace:` (empty = find only); options as Turbo's letters — U ignore case, W whole words, B backward, N replace without asking (TC2-RG p. 418–419; its G, which p. 419 refers to but p. 418 does not list, is the whole file [추정]) |
| Ctrl+L | search again (Alternate) | alike | command log (git panel only) | the same |
| Ctrl/⌘+G | delete char (Alternate) | alike | Go to (text and hex viewers) | **Go to line**, the viewer's field |
| Ctrl/⌘+A · Ctrl/⌘+R | word left · page up (Alternate) | alike | select all (04) · re-read (04, 03, the text viewer) | **select all · read the file again** (asks first when unsaved) |
| Ctrl+Q prefix (Q F, Q A, Q Y, Q [ ], Q n, …) | find, replace, delete to line end, pair match, markers | Ctrl+Q **= close** | Cmd/Ctrl+Q closes (`closing()`) | **not taken**: find and replace above; delete to line end, matching bracket and block begin/end in the menu; markers dropped |
| Ctrl+W | scroll up (both sets) | **= close** | close | not taken |
| Ctrl+E / D / X / S, Ctrl+R / C, Ctrl+V, Ctrl+H, Ctrl+P, Ctrl+O prefix | the diamond, pages, insert toggle, backspace, a literal, option toggles | partly clipboard or Save | as above | not taken: the cursor keys, Ins, Backspace and the Options menu cover them |
| Alt+[ / Alt+] (CUA pair match) | match the bracket | legacy ESC `[` starts a CSI | — | not taken (menu) |
| F1 | Help | alike | Help | Help |
| F2 | **Save** (TC2-UG p. 84; TC3-UG Fig. 2.2) | alike | (text view: wrap, 01 A11) | **Save** |
| F3 | Open (Turbo) / **Exit** (PC Tools 6, "Works the same as the ESC key", PCT6-D p. 22) | alike | Exit (viewers) | **Exit** (asks first when unsaved) |
| F4 · Shift+F4 | (debug: go to cursor) | alike | VS Code / Zed (`keys.rs:456, 496, 503`) | **back to viewing** · VS Code / Zed |
| F5 | Zoom (Turbo) / **Save** (Notepads 6) | alike | Save (07 §10) | **Save** (zoom: a click on `[↕]` or the menu) |
| F6 | **Next window** (Turbo) / Find (Notepads) | alike | Move (list), Rebase (git panel), Goto (hex) | **Panel**: the keyboard between the editor and the column, from either side (§2.2; Tab types a tab) |
| F7 · Alt+Ctrl/⌘+S · Shift+Ctrl/⌘+S | Trace (debug) | alike; Shift lost under legacy | Save As · Save copy (07 §6–§7) | Save As · Save copy (under legacy Shift+Ctrl+S arrives as **Save**: the menu's Save copy instead) |
| F8 | Step (debug) | alike | Encode (08 §7.5) | empty: the encoding is fixed while editing (§3.3) |
| F9 | Make | alike | arms the view keys (08 §4) | arms (08), ending editing first (§7.1) |
| F10 | **Menu** (Turbo, PC Shell, Notepads) | alike | Tree (list), Menu (git panel) | **Menu** |
| Alt+X, Alt+F3, Alt+letter menus, Alt+digits | exit, close, menus, windows | Alt+F3 lost under legacy; Alt+letter types a character in a Mac window (10 §9) | Alt+digits are F-keys and Alt+G is the switch, in the TUI (T7, T33) | not taken: F3, F10 then a letter |
| Esc | closes menus and dialogs | alike | leaves a viewer | closes the innermost menu, field, prompt or `^K`; else leaves the editor (asks first when unsaved) |
| F12 | — | alike | the switch (10 T4) | the switch |

- **Ctrl+]** is a trap, now measured: in a legacy terminal it is F5, Save. The editor does not bind
  it, and the help lists it under "keys a legacy terminal turns into F-keys". This is not new: today
  it already saves a hex edit [M + `fkey_events`]. → Settled by [`../12-editor.md`](../12-editor.md)
  §5.3 (E5): while the editor has the keyboard, no digit becomes F2 or F5.
- **In a Mac window**, ⌃+letter arrives without `command`. The editor's WordStar rows (Ctrl+K, Y, T,
  N, L) also match ⌃ there, so a Turbo hand works on a Mac keyboard. Every other row is Cmd/Ctrl, as
  10 T27 writes it [추정 choice: reversible].

## 2. The F4 layout and the left panel's tabs (question 2)

### 2.1 What the sentence asks, on today's screen

- Today the browser is two halves: `panels_ui` cuts the central area at `floor(width / 2)` and draws
  each side's content — List, Tree, Git or Preview — in its half (`browser.rs:2420-2433`) [S]. In
  preview mode one side holds the active list and the other the preview of the file under its bar
  (02 §3.1 O5) [S]. XW-grid will cut the same halves in cells, `grid.rect(side)` (10 §17) [S].
- "프리뷰를 보다가 f4를 누르면 디렉토리 패널 자체를 왼쪽으로 밀고 프리뷰 패널이 그대로 에디터가
  되도록": the owner is in preview mode, the list on one side and the preview on the other. F4
  narrows the **directory panel itself** into a column at the left edge, and the **preview panel
  becomes the editor where it is**, on the same file, its rows unchanged until the first key.
- "왼쪽 디렉토리 뷰에 탭을 추가해서 디렉토리/git뷰를 전환": that left column gets two tabs,
  **Dir** (the same panel: its folder, its bar, now on the edited file) and **Git** (the git panel
  of the file's repository, 03).
- "디렉토리 뷰어가 있어야함" is that left column; "git graph로 깃작업" is its Git tab, which is 03's
  panel and its actions, not a second git UI (§6).

### 2.2 As rules of 02 §3.1

The editor is one more input of `view::resolve` (02 §3.1's table) and one more rule, between rule 1
(a viewer covers both sides) and rule 2 (the pin):

| Input | Meaning |
|---|---|
| `editor` | `None`, or `{ file, tab: Dir \| Git, focus: Text \| Side }` — the editor is open on `file` |

- **Rule 1a — the editor is open**: the window's central area is **the side column** (left) and
  **the editor** (the rest). The side column shows the active panel's List (tab Dir) or `Git(top)`
  for the file's repository (tab Git). The sides' own states are kept (as `git_wide` keeps them,
  02 §3.1) and come back when the editor closes; the pin, the mode, `tree_snapshot` and the other
  panel are untouched.
- **The column's width** (reversible): 30 % of the columns, at least 20 and at most 40 cells — 24 of
  80, 40 of 160 (48 capped) [M `mockups/`]. The editor gets the rest; its frame, gutter (5 cells) and
  scroll bar leave 49 text columns at 80, 113 at 160 [M].
- **Where F4 comes from** (all the same layout; Esc or F3 goes back to where F4 was pressed):

| Pressed | Opens | Back (Esc, F3) |
|---|---|---|
| F4 in a list, bar on a text file, preview mode, preview on the right | the editor where the preview was; the list narrows to the column on its own side, the left | the list and the preview as they were |
| the same, preview on the left (the active list is the right one) | the list moves to the left column, the editor takes the right: "왼쪽 디렉토리 뷰" is on the left | as they were |
| F4 in dual mode (no preview) | the active list becomes the column, the editor replaces the other list | both lists |
| F4 before the bar has rested 150 ms (no preview committed yet) | the editor on the bar's file, read as Enter reads it (`browser.rs:1772-1774`: "what the preview host has of it is taken over, else it is made ready from the start") | the list and whatever the display commits |
| F4 in the Enter viewer (Text or Raw view) | the viewer gives way to the column and the editor, the view's place kept (§7.1) | the viewer, at the editor's place |
| F4 in the Enter viewer of a revision or a diff | the editor on the **working file** — today's F4 target (`Viewer.abs`, 08 §6; `browser.rs:2369-2372`) [S], not the revision | the viewer |
| F4 with the pinned git side showing | as above; tab Git shows the pinned repository when the file is inside it | the pin as it was |
| F4 on a folder | nothing opens; status row `F4 edits a file — Shift+F4 opens the folder in VS Code / Zed` | — |
| F4 on a binary file, image or media | nothing opens; status row `<name> is not text — Enter opens its viewer (2 = Hex edits bytes), Shift+F4 opens VS Code / Zed` | — |
| F4 on a text file above 256 MiB (§3.5) | nothing opens; the status row names the size, the limit and Shift+F4 | — |
| Shift+F4 anywhere F4 is | VS Code / Zed, exactly today's F4 (01 A2, `browser.rs:1760-1770`) | — |

- **Focus**: F6 moves the keyboard between the editor and the column, both ways (Turbo's F6 "next
  window", §1). Tab types a tab in the editor, so it cannot move focus there. In the column the
  list's keys (04 §3) or the git panel's (03 §9) act, as they do on a side today, with these
  exceptions:
  - **F6 wins in the column too**, so the keyboard always has the same way back. The list's F6
    Move and F5 Copy target "the other panel's folder" (02 §7 `OpsTarget`), and no other panel is
    shown here, so both step aside. F5 is empty in the column; Copy and Move are the browser's,
    after Esc.
  - The git panel's F6 Rebase stays in its F10 menu, which holds every action (03 §7, §9.1).
  - Enter on a text file in the Dir tab opens it in the editor, after 07 §10's prompt when the
    current file has unsaved edits. Enter on a folder enters it in the column.
- **Tabs**: the column's top border holds the tab line, `[Dir]─Git─` or `─Dir─[Git]` (the chosen
  tab in the bar's colours). **Tab** in the column switches it, a click on a name too. That is 02
  §3.2's Tab already: from a List into the Git beside it, from Git back to the list (03 §9.2) [S] —
  here both live in one column. Not Alt+D / Alt+G: Alt+letter types a character in a Mac window and
  Alt+G is the TUI's switch (10 §9, T33) [S]; not Ctrl/⌘+digits, which are F1…F10 (04 §6.1) [S].
  Ctrl/⌘+Shift+G stays 03's git-wide key (03 §9.2).
- **The F-key row** is the editor's while the editor has the keyboard. While the column has it, the
  row is the list's (slot 5 empty, slot 6 `Edit`) or the git panel's (slot 6 `Edit`) (04 §2: "the
  row changes with the context").

### 2.3 Mock-ups in cells

Generated by `mockups/mock.py` (each row exactly the grid's width, a Hangul name in its two cells,
cut names never splitting one) [M]. Characters only; the colours are §1.3's: the title row black on
cyan with the red ` xtools ` block; the column's frame single and light grey while the editor has
the keyboard; the editor's frame double and white, `[■]` and `[↕]` light green, its body blue with
the skin's syntax colours; `>` marks the bar's row (the bar itself is a colour: black while the
column does not have the keyboard, 02's idle bar); the F-key row's digits on black, labels black on
cyan. Rows at 80×25: title 1 · panels 22 · status 1 · F-keys 1 — no row is added for a menu bar or a
hint line: the title row holds Turbo's menu names and the F-key row is the hint line.

**80×25, preview mode, before F4** (the list active on the left, the preview on the right):

```
 xtools  ~/proj/sopen/src/gui                                            │12:34 
╔════════ ~/proj/sopen/src/gui ════════╗┌───────────── editor.rs ──────────────┐
║Name              Ext             Size║│   1 //! The editor (XR-E): a piece t…│
║ ..                               <UP>║│   2 //! file's bytes, saved through …│
║ browser                         <DIR>║│   3                                  │
║ cells                           <DIR>║│   4 use crate::encoding::Encoding;   │
║ text                            <DIR>║│   5                                  │
║ cells            rs            21,404║│   6 pub struct Piece {               │
║ chrome           rs            38,112║│   7     src: Src,                    │
║>editor           rs            12,288║│   8     start: u64,                  │
║ 한글 메모        txt            1,024║│   9     len: u64,                    │
║ text             rs           201,377║│  10 }                                │
║ mod              rs           130,950║│  11                                  │
║                                      ║│  12 impl Doc {                       │
║                                      ║│  13     pub fn insert(&mut self, at:…│
║                                      ║│  14         let bytes = self.enc.enc…│
║                                      ║│  15         self.pieces.split_at(at,…│
║                                      ║│  16     }                            │
║                                      ║│  17 }                                │
║                                      ║│                                      │
╟──────────────────────────────────────╢│                                      │
║10 files, 3 folders                   ║│                                      │
╚══════════════════════════════════════╝└──────────────────────────────────────┘
 editor.rs  12,288 bytes  F4 edits it here                                      
1Help   2Rename 3View   4Edit   5Copy   6Move   7MkDir  8Delete 9Dual   10Tree  
```

**80×25, after F4: the column on the Dir tab, the editor where the preview was** (24 + 56 cells; 49
text columns):

```
 xtools  File  Edit  Search  Block  Git  Options      1 Text·2 Hex·3 Raw │12:34 
┌[Dir]─Git─────────────┐╔═[■]═════════════════ editor.rs ════════════════1═[↕]═╗
│ ..                   │║   1 //! The editor (XR-E): a piece table over the    ▲
│ browser              │║   2 //! file's bytes, saved through write_atomic.    ░
│ cells                │║   3                                                  ■
│ text                 │║   4 use crate::encoding::Encoding;                   ░
│ cells.rs             │║   5                                                  ░
│ chrome.rs            │║   6 pub struct Piece {                               ░
│>editor.rs            │║   7     src: Src,                                    ░
│ 한글 메모.txt        │║   8     start: u64,                                  ░
│ text.rs              │║   9     len: u64,                                    ░
│ mod.rs               │║  10 }                                                ░
│                      │║  11                                                  ░
│                      │║  12 impl Doc {                                       ░
│                      │║  13     pub fn insert(&mut self, at: u64, s: &str) { ░
│                      │║  14         let bytes = self.enc.encode(s)?;         ░
│                      │║  15         self.pieces.split_at(at, bytes);         ░
│                      │║  16     }                                            ░
│                      │║  17 }                                                ░
│                      │║                                                      ░
├──────────────────────┤║                                                      ░
│10 files              │║                                                      ▼
└──────────────────────┘╚═* 13:27 ═════════════════════════════════════════════╝
 Insert Indent   Rust · UTF-8 · LF · M   unsaved — kept                         
1Help   2Save   3Exit   4View   5Save   6Panel  7SaveAs 8       9View   10Menu  
```

**80×25, the Git tab** (Tab in the column; 03's panel, narrow):

```
 xtools  File  Edit  Search  Block  Git  Options      1 Text·2 Hex·3 Raw │12:34 
┌─Dir─[Git]────────────┐╔═[■]═════════════════ editor.rs ════════════════1═[↕]═╗
│● Uncommitted (2)     │║   1 //! The editor (XR-E): a piece table over the    ▲
│  M editor.rs         │║   2 //! file's bytes, saved through write_atomic.    ░
│  ? 한글 메모.txt     │║   3                                                  ■
│● a1b2c3d editor: undo│║   4 use crate::encoding::Encoding;                   ░
│● 77b0beb Merge #107  │║   5                                                  ░
││● fe8aa3b Merge main │║   6 pub struct Piece {                               ░
│●╯ ada755e Merge #106 │║   7     src: Src,                                    ░
│● 3c1d2e0 help: keys  │║   8     start: u64,                                  ░
│                      │║   9     len: u64,                                    ░
│                      │║  10 }                                                ░
│                      │║  11                                                  ░
│                      │║  12 impl Doc {                                       ░
│                      │║  13     pub fn insert(&mut self, at: u64, s: &str) { ░
│                      │║  14         let bytes = self.enc.encode(s)?;         ░
│                      │║  15         self.pieces.split_at(at, bytes);         ░
│                      │║  16     }                                            ░
│                      │║  17 }                                                ░
│                      │║                                                      ░
├──────────────────────┤║                                                      ░
│main ↑0 ↓0            │║                                                      ▼
└──────────────────────┘╚═* 13:27 ═════════════════════════════════════════════╝
 Insert Indent   Rust · UTF-8 · LF · M   unsaved — kept                         
1Help   2Save   3Exit   4View   5Save   6Panel  7SaveAs 8       9View   10Menu  
```

**80×25, F10 then F: the File menu** (a cells dialog over the screen; its DOS shadow, two cells right
and one down, keeps the glyphs beneath in dark grey on black, 10 §6):

```
 xtools  File  Edit  Search  Block  Git  Options      1 Text·2 Hex·3 Raw │12:34 
┌[Dir]─G┌─────────────────────────┐═══════════ editor.rs ════════════════1═[↕]═╗
│ ..    │ Save                 F2 │he editor (XR-E): a piece table over the    ▲
│ browse│ Save as...           F7 │ile's bytes, saved through write_atomic.    ░
│ cells │ Save copy               │                                            ■
│ text  │ Reload           Ctrl+R │rate::encoding::Encoding;                   ░
│ cells.│ VS Code / Zed  Shift+F4 │                                            ░
│ chrome│ Exit                 F3 │truct Piece {                               ░
│>editor└─────────────────────────┘rc: Src,                                    ░
│ 한글 메모.txt        │║   8     start: u64,                                  ░
│ text.rs              │║   9     len: u64,                                    ░
│ mod.rs               │║  10 }                                                ░
│                      │║  11                                                  ░
│                      │║  12 impl Doc {                                       ░
│                      │║  13     pub fn insert(&mut self, at: u64, s: &str) { ░
│                      │║  14         let bytes = self.enc.encode(s)?;         ░
│                      │║  15         self.pieces.split_at(at, bytes);         ░
│                      │║  16     }                                            ░
│                      │║  17 }                                                ░
│                      │║                                                      ░
├──────────────────────┤║                                                      ░
│10 files              │║                                                      ▼
└──────────────────────┘╚═* 13:27 ═════════════════════════════════════════════╝
 File  ↑↓ choose · Enter runs · Esc closes · a letter: S A C R V X              
1Help   2Save   3Exit   4View   5Save   6Panel  7SaveAs 8       9View   10Menu  
```

**160×40, the Git tab** (40 + 120 cells; 113 text columns; the Dir tab is the same column with the
folder's rows):

```
 xtools  File  Edit  Search  Block  Git  Options                                                                                      1 Text·2 Hex·3 Raw │12:34 
┌─Dir─[Git]────────────────────────────┐╔═[■]═════════════════════════════════════════════════ editor.rs ════════════════════════════════════════════════1═[↕]═╗
│● Uncommitted (2)                     │║   1 //! The editor (XR-E): a piece table over the                                                                    ▲
│  M editor.rs                         │║   2 //! file's bytes, saved through write_atomic.                                                                    ░
│  ? 한글 메모.txt                     │║   3                                                                                                                  ■
│● a1b2c3d editor: undo                │║   4 use crate::encoding::Encoding;                                                                                   ░
│● 77b0beb Merge #107                  │║   5                                                                                                                  ░
││● fe8aa3b Merge main                 │║   6 pub struct Piece {                                                                                               ░
│●╯ ada755e Merge #106                 │║   7     src: Src,                                                                                                    ░
│● 3c1d2e0 help: keys                  │║   8     start: u64,                                                                                                  ░
│                                      │║   9     len: u64,                                                                                                    ░
│                                      │║  10 }                                                                                                                ░
│                                      │║  11                                                                                                                  ░
│                                      │║  12 impl Doc {                                                                                                       ░
│                                      │║  13     pub fn insert(&mut self, at: u64, s: &str) {                                                                 ░
│                                      │║  14         let bytes = self.enc.encode(s)?;                                                                         ░
│                                      │║  15         self.pieces.split_at(at, bytes);                                                                         ░
│                                      │║  16     }                                                                                                            ░
│                                      │║  17 }                                                                                                                ░
│                                      │║                                                                                                                      ░
│                                      │║                                                                                                                      ░
│                                      │║                                                                                                                      ░
│                                      │║                                                                                                                      ░
│                                      │║                                                                                                                      ░
│                                      │║                                                                                                                      ░
│                                      │║                                                                                                                      ░
│                                      │║                                                                                                                      ░
│                                      │║                                                                                                                      ░
│                                      │║                                                                                                                      ░
│                                      │║                                                                                                                      ░
│                                      │║                                                                                                                      ░
│                                      │║                                                                                                                      ░
│                                      │║                                                                                                                      ░
│                                      │║                                                                                                                      ░
│                                      │║                                                                                                                      ░
├──────────────────────────────────────┤║                                                                                                                      ░
│main ↑0 ↓0                            │║                                                                                                                      ▼
└──────────────────────────────────────┘╚═* 13:27 ═════════════════════════════════════════════════════════════════════════════════════════════════════════════╝
 Insert Indent   Rust · UTF-8 · LF · M   unsaved — kept                                                                                                         
1Help           2Save           3Exit           4View           5Save           6Panel          7SaveAs         8               9View           10Menu          
```

## 3. The text buffer (question 3)

### 3.1 Candidates and what was measured

The prototype (`proto-buffer/src/main.rs`) builds each candidate over the same files, **one process
per variant** so the peak RSS is that variant's: `1mb.rs` (1,048,484 bytes, 25,305 lines) and
`100mb.rs` (104,857,584 bytes, 2,544,493 lines), both sopen's own `text.rs` + `hex.rs` repeated.
Workloads: open; a typing run (1,000 characters at one place, the middle); 10,000 scattered inserts
of two bytes plus 10,000 scattered one-byte deletes; 20,000 line ↔ offset lookups; serialising the
whole document to a file (what Save stages). macOS 26.7.1, arm64, release build. Receipt:
`buffer-run1.txt` [M].

| | ropey 1.6.1 (UTF-8 rope, B-tree) | piece table over the file's bytes (prototype: pieces in a `Vec`, linear scan) | `String` (egui `TextEdit`'s buffer) |
|---|---|---|---|
| open 1 MB / 100 MB | 0.5 / 36.9 ms (UTF-8 check + build) | 0.2 / 16.2 ms (LF count) | 0.0 / 3.8 ms (UTF-8 check) |
| typing, per character, 1 MB / 100 MB | 0.08 / 0.09 µs | 0.09 / 7.93 µs ¹ | 9.33 / 1,094.59 µs |
| scattered edit, per edit, 1 MB / 100 MB | 0.19 / 0.73 µs | 16.15 / 23.99 µs with 29,444 / 30,000 pieces ² | 9.80 / 1,149.64 µs (1,000 / 100 edits) |
| line ↔ offset, per lookup, 1 MB / 100 MB | 0.12 / 0.48 µs | 5.98 / 8.38 µs with ~30,000 pieces ² | 66 / 7,812 µs (no index: a scan) |
| serialise for Save, 1 MB / 100 MB | 1.0 / 92.9 ms (116,130 chunks) | 1.4 / 97.9 ms | 0.3 / 9.9 ms |
| peak RSS, 1 MB / 100 MB | 5.1 / **225.2 MiB** | 3.8 / **102.8 MiB** | 2.8 / 101.8 MiB |
| lossless (§3.2) | no: decodes, so bytes that are not valid in the encoding change | **yes**: bytes not edited are never decoded | no (UTF-8 only) |

¹ The first character splits the one original piece and the prototype recounts LFs in its left
half (50 MB, 8 ms once); a real table keeps the original's line starts (as VS Code's piece tree
keeps `lineStarts` per buffer [S: the blog below]) and splits by a binary search. ² The prototype's
lookup is a linear scan of the pieces, so these numbers are the prototype's, not the design's; even
so, after 20,000 scattered edits one edit costs 24 µs and one lookup 8 µs — a frame is 16,000 µs.

- **egui's `TextEdit`: killed.** It is an egui widget, and 10 freezes new egui screens: the editor
  is written in cells (10 §2, §12) [S]; egui's text layout is what moves Hangul off the grid (10 §4)
  [S]; a terminal cannot show it. Its buffer, a `String`, costs 1.09 ms per typed character at the
  middle of 100 MB and has no line index (a line lookup scans: 7.8 ms) [M].
- **xi-rope: killed.** Its last release is 0.3.0 of 2019-06-29 (crates.io API) [S], and its project
  says "The xi-editor project is currently discontinued"
  (https://github.com/xi-editor/xi-editor, README) [S]. It is a UTF-8 rope, so not lossless either.
- **ropey: the fastest, but it holds text, not bytes.** Every operation is O(log n) and measured
  under a microsecond [M]; it is maintained (1.6.1 stable, 2.0 in beta, crates.io) [S]. It needs
  valid UTF-8, so every file is decoded at open and encoded at Save — and that is where it loses
  bytes (§3.2) — and it held 2.25× the file in memory [M].
- **A piece table over the file's bytes: recommended.** The original bytes are immutable and never
  decoded for storage; typed text is encoded into the file's encoding and appended to an add
  buffer; the document is the list of pieces. It is lossless by construction (§3.2), its memory is
  the file plus what was typed (1.0×) [M], opening costs the LF count the viewer's index pass
  already makes (text.rs's module docs) [S], and undo is a list of piece edits. VS Code moved to the
  same structure (a piece table in a balanced tree, "piece tree") for speed and memory:
  https://code.visualstudio.com/blogs/2018/03/23/text-buffer-reimplementation [S]. The first cut
  keeps pieces in a `Vec` (measured above: within a frame by three orders of magnitude) and the
  original's line starts; a tree is only for a measured need (Delete → simplify).

### 3.2 Lossless: what each candidate saves outside the edit

One edit per fixture ("Z" inserted after the first line), saved by each candidate, the result
compared byte for byte with the original plus the edit (`proto-buffer lossless`) [M]:

| Fixture | Piece table | Decode → rope → encode |
|---|---|---|
| UTF-8 with an invalid byte `FF` | identical | **differs**: `FF` became `EF BF BD` (U+FFFD) |
| UTF-8, CRLF, no final LF | identical | identical |
| UTF-8 with a BOM | identical | identical |
| UTF-16LE with a BOM, Hangul, CRLF | identical | identical |
| UTF-16LE with a lone surrogate `D800` | identical | **differs**: the surrogate became U+FFFD |
| CP949 (Hangul, a UHC syllable `똠`) | identical | identical |
| CP949 with a lead byte `C8` followed by LF | identical | **differs**: 8 bytes became 15 — encoding_rs wrote the U+FFFD as the numeric character reference `&#65533;` |

- So a decoding buffer must refuse to edit such a file, or change bytes the owner never touched;
  the byte piece table edits it and keeps every other byte.
- **encoding_rs cannot write UTF-16**: `UTF_16LE.encode("A한")` returned `41 ED 95 9C` — UTF-8 — and
  `UTF_16LE.output_encoding()` is UTF-8 [M], as its documentation says of the UTF-16 encodings. So
  the encode side is sopen's own for UTF-16, UCS-2 and UTF-32 (std's `encode_utf16`, `u32`), as 08
  §7.4 already has std decode them [S]; encoding_rs encodes CP949 only.

### 3.3 Encodings: edit and save in the file's encoding

- **The model is 08's.** `src/encoding.rs` (VM-1, on main) has one decoder per encoding over byte
  slices, the LF and CR units, the BOM and the four character-start rules; it has **no encode side**
  (`encoding.rs:1-12`, its `pub fn`s) [S]. The editor adds `Encoding::encode(&str) -> Result<Vec<u8>,
  char>`: UTF-8 as is; UTF-16LE/BE by `encode_utf16`; UCS-2 refusing characters above U+FFFF;
  UTF-32 by `u32`; ASCII refusing above U+007F; Latin-1 above U+00FF; CP437 by the inverse of
  `chrome::cp437`; CP949 by encoding_rs's EUC-KR encoder, refusing what it cannot map (its
  `unmappable` flag, which the fixture above raised [M]) instead of writing `&#…;`.
- **A character the file's encoding cannot hold is refused**, never replaced: nothing is typed or
  pasted, and the status row says `한 cannot be written in Latin-1 — nothing was typed`. A paste is
  refused whole, naming its first such character.
- **The encoding is fixed while editing**: an edited file keeps its encoding (ssot reading, 08
  §7). F8 (Encode) works in the view before F4; in the editor its slot is empty. Saving in another
  encoding is not offered (not asked for).
- **Bytes kept**: the BOM (the caret's first place is after it, so no edit reaches it; 08 §7.2's
  skip rule); every line's own ending — a new line typed takes the ending of the line it splits,
  else the file's first, else LF; no final newline is added or removed; a tab is typed as a tab
  where the file's indented lines start with one, else as spaces to the next multiple of 4 (the
  viewer's `TAB = 4`, `text.rs:152`) [S].
- **Moving by characters** uses the decoders: → and ← step one `Cell` (`encoding.rs` `decode`), so a
  caret is always on a character start in every encoding, and a line is found by the encoding's LF
  unit (08 §7.3), never by the byte 0x0A [S].

### 3.4 Undo

- One stack of edits — `(offset, removed bytes, inserted bytes)` — grouped as Turbo C++'s "Group
  Undo" groups them (§1): a run of typing, one Backspace run, one paste (a bracketed paste in a
  terminal is one `Paste` event, 10 §4 [S]), one block operation. Undo and redo restore the caret.
- Bounded as 07 §11 bounds the hex window's: at most 100,000 steps; past it the oldest go.
- Undo survives a Save (the stack is in memory); undoing past the Save makes the document unsaved
  again, as an image's undo past a Save does (07 §2) [S]. The journal holds only the edits since the
  last Save (§5.3).

### 3.5 Large files, local and remote

- **The original is held in memory**, read whole when the editor opens: a local file from disk; a
  remote file on a worker, with progress and Esc (`reading for the editor — 37 %`). Which channel
  that read takes — the viewer's `Ranged` or its `Origin`, within 06 §6's per-host count and 08
  D14's "1; 2 from its first Save" — is [추정], left to X-SPEC (→ settled by 12 §7.4, E14: a `Bulk`
  reader on the opening's `Source`, the `Ranged` channel). That read gives the baseline's SHA-256, as the image window's whole read gives its own
  (01 §5) [S], so Save's content check covers the whole file (§5).
- **The limit is 256 MiB** (`HASH_CHECK_MAX`, `remote/mod.rs:34`) [S]: up to it every Save checks
  the whole content; memory is the file plus the typed text (1.0×, [M] above). Above it F4 opens
  nothing and says why (§2.2).
- **What a remote read costs** at 07's measured rates (1.39 MB/s, *edit* §2.3; 3.6 MB/s, 07 §4.1's
  example) [S]: 1 MB 0.3–0.7 s; 16 MiB 4.7–12 s; 100 MB 29–75 s. A text preview's stream has
  already read up to its 4 MiB budget (02 §4.3) [S]; whether those blocks are still in the block
  cache when F4 is pressed is [추정]. → Settled by 12 §7.4 (E15): the hand-over keeps the stream's
  cache; receipt ES-6.
- **Above 256 MiB**, or a remote file without the patience for a whole read: a later unit could edit
  through the piece table over the stream (pieces of the original read through 06's `Source`) and
  save by `copy-data` from the source's ranges into the temp at their new offsets — 07 §4.2's server
  copy with offsets that move. That is **not designed here** [추정]: 07 §4.2 step 8 requires the
  temp's size to equal the size as opened and §4.5's content check reads back an unchanged copy,
  neither of which holds for a text edit, so it needs its own check.

## 4. Syntax highlighting (question 4)

### 4.1 Measured

The prototypes (`hl/`) run over `rust10k.rs` (sopen's `text.rs` + `hex.rs`: 10,007 lines,
412,368 bytes). syntect with sopen's exact features (`default-features = false`,
`default-syntaxes`, `regex-fancy`; `Cargo.toml:90`) and set (`load_defaults_nonewlines`,
`text.rs:2019-2022`), and a theme of `chrome.rs:557-594`'s scope rules; tree-sitter 0.25.10 with
tree-sitter-rust 0.24.2 and tree-sitter-highlight 0.25.10; a hand lexer (keywords, strings,
numbers, line and block comments; 63 lines). Sizes: each program stripped, minus a baseline that
reads a file and counts its lines (360,336 bytes), macOS arm64, release. Receipts: `hl-run1.txt`,
`hl-syn-run2.txt`, `sizes.txt`, `syntect-syntaxes.txt` [M].

| | syntect (as sopen links it) | tree-sitter | hand lexer |
|---|---|---|---|
| languages | **75** syntaxes in the bundled set | 1 per grammar crate | 1 |
| size in sopen | **+0 bytes: already linked** for the text viewer (`Cargo.toml:90`) [S] | +1,191,168 (runtime + Rust grammar, parse only); +2,859,024 with tree-sitter-highlight and its query; +446,208 for one more grammar (Python) | +16,960 |
| size standalone | +2,201,248 | — | — |
| whole file, cold | 438–455 ms (40–46 µs a line) | parse 16.2–18.9 ms; tree-sitter-highlight's whole pass 30 ms | 1.48 ms |
| a keystroke, mid-block (line 5128; the state kept every 256 lines) | 3.1–3.3 ms to the end of a 60-line page (68 lines parsed); 10.7 ms on to the next mark | incremental reparse 0.33 ms + page captures 0.12–0.14 ms | — |
| a keystroke at a block's last line (5375) | 12.8–13.3 ms (315 lines parsed) | — | — |
| an unclosed `/*` typed at line 1 | 0.14 ms to the page's end; every later state differs (whole file then: 6.3 ms, all comment) | reparse 2.0 ms | — |
| convergence | the state at the next mark compared with the kept one: **equal** after a mid-block edit, after a block-end edit, and after `/*` … `*/` closed two lines later (`(ParseState, HighlightState)` is `PartialEq`, compiled) | the tree's changed ranges | — |

### 4.2 Decision: syntect, the viewer's own

- **One highlighter for the preview, the viewer and the editor**: F4 turns the preview into the
  editor "그대로", and with the same grammar and the same theme (`chrome::syntax_theme`,
  `chrome.rs:544-604`) the colours do not change on F4. A second engine would bring a second scope
  mapping beside `chrome.rs:557-594` and different colours for the same file [S].
- **Size**: syntect costs sopen nothing more; tree-sitter would add 1.2–2.9 MB for Rust alone and
  about 0.45 MB a grammar after it [M] — tens of MB for the 75 languages syntect covers [추정, from
  two grammars].
- **Speed is enough where it is spent**: the viewer already highlights on a worker, never on a
  frame (text.rs's module docs; `MAX_WORKERS = 3`, `text.rs:2795`) [S], because one syntect call
  cannot be interrupted. The editor keeps that: a keystroke re-parses from the kept state before the
  edited line to the end of the page on the worker (3–13 ms measured), the frame draws the edited
  line in its old colours until the runs arrive, and the worker then walks on to the next kept state
  to see whether the states converged; if not, the kept states after it are dropped and made again
  as pages are asked for (the viewer's `Highlight::page` already makes missing states block by block,
  `text.rs:2742-2789`) [S].
- **The editor keeps a state every 64 lines** instead of 256 [추정 choice]: a keystroke then parses
  at most 64 + 60 lines (≈ 5 ms at 40 µs a line [M rate]). Its memory per kept state is not measured.
  → Settled by 12 §8 (E19): 64 lines by default, receipt EH-3, back to 256 above 32 MiB.
- **The hand lexer: killed** — one language, and the 75 the viewer highlights would turn plain on
  F4. **tree-sitter: killed** for now — its incremental parse is 40× faster [M], but that speed is
  not needed at 5 ms a keystroke on a worker, and its size and second colour mapping are.
- **Colours to cells**: `chrome::syntax_theme` already maps scopes onto the 16 colours of
  `chrome.rs:32-47` (comments cyan, keywords yellow, strings light cyan, numbers light green, types
  white, …) [S]; the editor puts each run's colour in its cells' foreground, as X3's text view will
  (10 §16). Highlighting stops where the viewer's does: a line over `LINE_MAX` = 4096 bytes
  (`text.rs:120`), past `HIGHLIGHT_MAX` = 16 MiB (`text.rs:123`), and the viewer's shortening of
  long runs applies (`Shortened`, `text.rs:2392`) [S].

## 5. Saving (question 5)

### 5.1 The route: today's whole-file replace

- **Every Save that replaces a whole file already goes through one primitive**:
  `RemoteFs::write_atomic_with(staged, target, fingerprint, steps)` (`remote/mod.rs:604-660`) [S]:
  refuse without `posix-rename`; `check_target` — `lstat`, still a regular file, not a link, the
  same size and mtime (`remote/mod.rs:822-850`), and up to 256 MiB the same content hash; create
  `.<name>.sopen-<random>` exclusively in the same folder; give it the target's owner and mode
  through the handle; write; fsync; check again; `posix-rename` over the target; each step told to
  `steps` before the next request, for the journal's save intent (07 §3.1). `LocalFs` implements the
  same trait (`remote/local.rs:62`, its `create_from` fsyncs with `sync_all`, `:220`, and
  `rename_over`, `:232`) [S], so **a local file's atomic write is the same code**.
- **Who uses it today**: the image window's Save (`image/file.rs:150-260`: backup of the original
  as opened, stage, `write_atomic_with` with the journal's steps, the conflict kept aside), a local
  file's hex Save (`hex/file.rs:903-975`), and the terminal editor's `upload_back`
  (`main.rs:1229-1270`) [S]. A streamed hex Save is a different route (server copy, then patch) whose
  temp must keep the size as opened (07 §4.2 step 8) [S] — a text edit changes the size, so it does
  not fit.
- **The editor's Save is the image route** (07 §4.1: "Image Save replaces every byte … through
  `write_atomic`") [S]:
  1. refuse if this server cannot replace in one step (`CANNOT_REPLACE`), offering Save copy;
  2. reconcile a Save of this session left cut off (07 §3.1);
  3. the session's first Save writes the original as opened — held in memory — to the local backup
     `<cache>/.backup/<host>/<abs>.<stamp>` and prints its path (01 §5's image and hex rule);
  4. stage: the pieces written in order into `<cache>/.edits/<host>/<abs>` (0600, created
     exclusively), the whole new content hashed on the way (the post-save evidence);
  5. `write_atomic_with`, the journal recording each step;
  6. landed: the staged temp's stat and the hash are the next baseline; the piece table is rebased
     on the saved bytes (one piece); `BrowserEvent::FilesChanged { host, dirs }` is sent, so the Dir
     tab re-lists and the Git tab soft-refreshes (02 §7, 03 §5) [S].
- **Its cost**: a local file, the backup and the stage (2× its size on the local disk, once per
  session for the backup) [S rule; not measured]; a remote file, one upload of the whole file —
  1 MB 0.3–0.7 s, 100 MB 29–75 s at 07's rates [S], with progress and Cancel (image's `Saving`).
- **Keys**: F2 (Turbo, §1) and Ctrl/⌘+S and F5 (07 §10); Save As… Alt+Ctrl/⌘+S and F7; Save copy
  Shift+Ctrl/⌘+S (07 §6) [S].

### 5.2 Conflicts

- **Changed on disk** — the size, mtime or (≤ 256 MiB) content differ, the file is gone, a link or
  not a regular file: nothing is written (`WriteOutcome::Conflict`); the staged copy is moved to a
  name of its own under `.edits` and named; the edits stay in the editor and the journal. The status
  row: `editor.rs changed since it was opened (size 12,288 → 12,301); not saved — your edit is at
  <path>`, as `image/file.rs`'s message [S]. Then **Save As…** (07 §7) or **Reload** (Ctrl/⌘+R:
  asks `Discard your changes and read editor.rs again?`).
- **Changed while the editor is clean** (the Git tab's checkout or discard, `GitEvent::WorkTreeChanged`,
  02 §7; a file operation's `FilesChanged`): the editor reads the file again at its place. With
  unsaved edits it reads nothing and the status row says `editor.rs changed on disk (checkout) —
  Save will refuse; Ctrl/⌘+R reads it again`. There is no file watcher (03 §5, 01 §9) [S], so a
  change by another program is found at Save, as everywhere in sopen.
- The checks are 01 §5's best effort, not a lock [S].

### 5.3 The journal and the prompt on close

- **The journal** (07 §3) gets a fourth kind: `journal::Kind` is `Hex | Image | Media` today
  (`journal.rs:233-237`) [S]; `Text` adds edits = the piece list against the baseline plus the add
  buffer's bytes, and `seen` = the whole SHA-256 from the read at open. Generations as 07 §3 writes
  them: ≤ 250 ms after a change, at focus loss, at close, before a Save; Restore / Later / Discard on
  the next open.
- **The prompt on close is the shell's and the hex window's** (the brief's "the shell already
  prompts"): the shell turns Cmd/Ctrl+Q and +W into a close request and cancels it when the window
  refuses (`closing()`, `gui/mod.rs:1176-1185`); `Window::close_requested` is where a window refuses
  (`gui/mod.rs:637-642`); the hex window then shows 07 §10's `Save the changes to <host:/abs> before
  closing?` — **Save** (Save copy where Save is off) · **Save As…** · **Discard** (its hover names the
  kept edits it deletes) · **Cancel** (`hex.rs:4234-4291`, refusing at `hex.rs:4672`) [S]. The editor
  asks the same four at every exit: Esc, F3, F4 back to the view, Enter on another file in the Dir
  tab, F9 then another view (08 §5 D2), the window's close, and the switch (10 §7.4, T30).
- That dialog is egui today; in cells it is XW-dialogs' (10 §17) [S].

## 6. Git work from the editor (question 6)

- **The Git tab is 03's git panel**, the one panel the window keeps for the repository's top level
  (02 §3.1: one git panel per top level, at most 3), drawn in the column's rect. Its rows, keys,
  F-key row (03 §9.1), menu and dialogs are 03's; nothing is built twice.
- **What the editor needs of it**, all of it in 03 already [S]:
  - the file's status — the uncommitted row's `Staged`, `Not staged`, `Untracked` groups (03 §6.1)
    — shown as one letter in the editor's bottom border (`M`, `A`, `?`, `S` for staged), and the
    uncommitted row first in the tab;
  - **stage / unstage** — Space on a file of the uncommitted row (03 §7.6: `add -- <p>` ·
    `restore --staged -- <p>`);
  - **commit** — F7 Commit… on the uncommitted row (03 §7.6: `commit --cleanup=verbatim -F -`, the
    message dialog), amend;
  - **the diff of the file** — Enter on its row opens the diff in the text viewer (03 §6.2);
  - **discard** (F8, backed up, 03 §8.2), which changes the file under the editor: §5.2's rule.
- **From the editor itself**, the F10 menu's Git entries act on the edited file without leaving it:
  Stage this file, Unstage, Commit… (03 §7.6's dialog), Diff against the index, Show in Git (the tab
  with the bar on the file). A Save sends `FilesChanged`, so the tab's status follows (03 §5's soft
  refresh) [S].
- **The narrow tab** (24 cells at 80 columns, 40 at 160) shows 03 §4.2's columns cut to fit — the
  lanes as glyphs (10 §6: `│ ─ ╭ ╮ ╰ ╯ ┼`), the short hash, the subject — and 03's Ctrl/⌘+Shift+G
  (git wide, 03 §9.2) gives it the whole central area and back, the editor kept underneath.
- **Not new**: no blame, no inline diff gutter, no merge editor (01 §9 and ssot "Not in scope": no
  IDE) [S].

## 7. How it fits the specs it lands on

### 7.1 In 08: the editing state of the Text view

- The editor is **the Text view, editing** — as 08's hex view is viewing or editing (D35,
  `view_keys.rs:33-42`) [S]. One document, encoding and place: F4 from the Text view's place opens
  the editor at that place; F4 or Esc from the editor returns to viewing at the caret. Raw shares
  the document (08 D8), so F4 from Raw edits too.
- Its view-key state: **TypingIsData** — like `DigitsAreData` (`view_keys.rs:60-70`): plain digits
  are typed; F9 ends editing and arms (asking 07 §10's prompt first, 08 §5 D2, if the next key
  chooses another view). `View::saves()` (`views.rs:58-60`, "Text, Raw and Page are read-only") adds
  Text while editing [S].
- 08's Page view (HTML) opens on 3 = Text (10 T21); F4 there edits the source.

### 7.2 In 10: cells, keys, the switch

- **Cells**: every part is §1's cell API; no modifiers (T37), so the selection is a colour (§1),
  Ins/Ovr is a word on the status row, and the caret is the terminal's cursor in the TUI (T37) and
  two scan lines in the window — an overwrite caret a whole cell [추정 choice].
- **Hangul**: in the TUI the terminal draws the composition at the cursor and commits arrive as
  `Text` (10 §4) [S]; in the window egui's IME preedit is drawn by the editor at the caret in the
  bar's colours, and committed on `Ime::Commit`. The preedit is the one place where TUI and window
  differ during typing (the TUI draws none itself) [추정]. → Settled by 12 E20 (masked in the 1:1
  check) and E10 (the Korean input source); owner receipt EO-2.
- **The switch** (T4, T32): the state gains `editor: { file, encoding, caret, anchor, top, ovr,
  tab, focus, find }` and bumps `v`; in one process the unsaved edits cross as they are; across
  machines 10 §7.4's question comes first (journals are per machine) [S].
- **Clipboard**: Copy and Cut go out as OSC 52 in the TUI; paste is the terminal's bracketed paste
  (Ctrl/⌘+V brings nothing in a terminal, 10 §4 [S], measured: Ctrl+V decodes to nothing in both
  encodings [M `keys-table.tsv`]).

### 7.3 Routing and VS Code / Zed

- **F4 = the built-in editor** (04 row 4 becomes `4 Edit: xtools's editor`), **Shift+F4 = VS Code /
  Zed** as F4 is today, in the list, the viewer, the editor and the git panel (03 §9.1's slot 4 keeps
  the built-in editor for a file and VS Code / Zed for the top level, a folder). Shift+F4 decodes as
  itself under both encodings [M]. The F10 menu has `Open in VS Code / Zed`.
- **`sopen host:/file.txt`** (10 §11 row 5, left to XR-E): a text file routes as its folder routes
  (rows 7–10: today's window until the flip, the TUI after it) and opens with the bar on the file
  and the editor open — the default editor is xtools's, as message 21 says. Row 6 is unchanged: with
  input or output not a terminal (a `while read` loop, a launcher), text keeps today's A2 route to
  VS Code / Zed. `--app APP` keeps opening a copy in any local app (`main.rs:64-66`) [S].
- The text above 256 MiB on the command line: the viewer (view 1), with the status row's line.

## 8. Work units (question 7)

Two kinds, as 08 split VM-1…VM-6 from VM-7/VM-8: **model** units have no screen and can start now
(they need nothing of 10's steps); **screen** units are cells and wait for the steps of 10 §16 they
draw on. Sizes are product lines [추정], except where the prototype gives a floor. Each unit owns its
files; a file shared by two units passes from one to the next in the order given (10 §17's rule).

| ID | Unit | Owns | Size | After | Ships alone as |
|---|---|---|---|---|---|
| ED-0 | Scaffold: the `edit` module and its `mod` lines | `src/lib.rs` (`mod` line), `src/edit.rs` (new, docs only) | ≈10 | — | no change |
| ED-buf | The piece table over bytes (§3.1): pieces, the original's line starts by the encoding's LF unit (08 §7.3), insert, delete, line ↔ offset, serialise; undo groups (§3.4); §3.2's fixtures as tests | `src/edit/buffer.rs`, its tests | ≈450 (the prototype's table is 150) | ED-0 | no change |
| ED-enc | The encode side (§3.3): `Encoding::encode` for every encoding, refusing what it cannot hold; CP949 by encoding_rs's EUC-KR encoder | `src/encoding/encode.rs`, its tests (new); `src/encoding.rs` (its `mod` line only — VM-1's file, whose CP949 entry is VM-4's, 08 §13) | ≈150 | ED-0; its CP949 part after VM-4 (which adds encoding_rs) | no change |
| ED-ops | Editing over the buffer: the caret by characters, lines and columns, words, selection, the Ctrl+K block set and its arming (a function of armed state and press, as `view_keys::decide` is for F9), find and replace (the needle encoded, matched at unit offsets), autoindent, the tab and line-ending rules — operations only: their keys are rows of 04's one table (ED-layout) | `src/edit/ops.rs`, `src/edit/arm.rs`, their tests | ≈700 | ED-buf, ED-enc | no change |
| ED-save | The file (§3.5, §5): the whole read at open (local; a remote one on its `Origin`'s worker, with progress and Esc), the baseline, Save by the image route, Save As, Save copy, the backup, `journal::Kind::Text` and its recovery rows; a Save's result says what landed, and ED-layout turns it into `FilesChanged` | `src/edit/file.rs` (new); `src/journal.rs` (`Kind::Text` and its edits format; no other unit of xtools owns it) | ≈600 | ED-buf | no change (its tests run against `sftp-server` as the hex and image Saves' do) |
| ED-screen | The editor in cells (§1.3): frame, gutter, rows with syntect's runs from the viewer's `Highlight` through a line source both `TextDoc` and the buffer give, caret, selection, scroll bars, the status row's fields, the title row's menu names, the F-key row; Hangul preedit in the window | `src/gui/edit.rs` (new); `src/gui/text.rs` (the line-source seam of `Highlight` only; the file is VM-2's, then X3's, and passes here after them) | ≈900 | **X3** (the text view in cells), **XW-list** (the title, status and F-key rows in cells), ED-ops | no change: nothing opens it yet |
| ED-layout | The browser (§2): `view::resolve`'s rule 1a, the column (Dir: the list narrow; Git: the git panel at the column's rect), F4 and Shift+F4 in every context, F6, Tab in the column, Enter on a file in Dir, every exit through 07 §10's prompt, `FilesChanged` after a Save; the editor's key rows (§1.4) in 04's one table under `Context::Editor` | `src/gui/browser/view.rs`; `src/gui/browser.rs` (`panels_ui`'s editor branch, `edit`); `src/gui/browser/keys.rs` (the F4 rows and `Context::Editor`'s rows) — `browser.rs` passes here after XW-grid, XW-list and VM-2/VM-7's lines; `keys.rs` after XW-help and VM-7 | ≈400 | **XW-grid**, **XW-list** (the list in cells, which the Dir tab draws narrow), **X4** (the tree and the git panel in cells), ED-screen, ED-save | **X5**: F4 edits in place |
| ED-dialogs | Find / Replace, Go to line, the close prompt, the F10 menu — in cells | `src/gui/edit/dialogs.rs` (new) | ≈400 | **XW-dialogs** (10 §17), ED-screen | with ED-layout |
| ED-git | The Git tab's narrow columns (§6) and the editor's Git menu (stage, unstage, commit…, diff, show in Git), the status letter in the frame | `src/gui/browser/git_panel/` (the narrow layout only); `src/gui/edit.rs` (after ED-screen) | ≈200 | X4, ED-layout | the Git tab works |
| ED-route | `sopen host:/file.txt` → the editor (§7.3); 01 A2/A11/§9, 04 row 4, 07 §1/§2, 10 §11 row 5 and AGENTS.md's edit line amended; README | `src/main.rs` (the text route), `README.md`, `AGENTS.md` — after XW-term, which adds `--tui` there | ≈80 | ED-layout, XW-term | `sopen file.txt` opens xtools's editor |

- **Order**: ED-0 → ED-buf ∥ ED-enc → ED-ops ∥ ED-save, all now (no screen, no file another unit
  holds but `journal.rs`, which no xtools unit owns); ED-screen after X3 and XW-list; ED-layout after
  XW-grid, XW-list, X4 and ED-screen; ED-dialogs after XW-dialogs; ED-git after ED-layout; ED-route
  last, after XW-term.
- **Waits on XW-list and XW-dialogs** (the brief's question): ED-screen and ED-layout wait on XW-list
  (the title, status and F-key rows and the list in cells); ED-dialogs waits on XW-dialogs (Find's
  field is a status-row field, but Replace's confirmation, Go to, the close prompt and the F10 menu
  are dialogs).
- **Not a unit here**: editing above 256 MiB through `copy-data` (§3.5) — it needs its own content
  check; designed only if the owner asks for it.
- **Gate**: the loop's (`just check` with `SOPEN_REQUIRE_FFMPEG=1 SOPEN_REQUIRE_SFTP_SERVER=1`, the
  Linux clippy runs, `../xtools/loop.md`); each screen unit brings a cell dump (10 §15's
  `SOPEN_CELLS`) and a screenshot someone looked at.

## Tensions

- **01 §9 ("Own text editor" out of scope, `01-spec.md:1249`), 07 §1 ("The text viewer stays
  read-only", `07-streaming-edit.md:54`) and AGENTS.md ("Edit features come only from owner
  messages", `AGENTS.md:105`) vs message 21.** Message 21 is the owner message that rule waits for;
  each text gets a line naming it when the spec lands.
- **07 §2's table** (hex never changes the size; text has no row) vs the editor: a `Text` row whose
  size changes, saved by the whole-file replace (§5.1), not by 07 §4.2's patch route.
- **04 row 4 and `keys.rs:456, 496, 503`** (F4 = VS Code / Zed in the list, the viewer and the git
  panel) vs message 21's "디폴트로 로컬 에디터": F4 becomes the built-in editor, Shift+F4 the old
  route; 02 §4.4's "F4 changes nothing" for a preview (`preview.rs:16-19`) becomes "F4 hands the
  preview over to the editor".
- **10 §11 row 5** ("01 A2's editor route, until XR-E decides") vs §7.3: text routes as its folder
  does, except where row 6 keeps today's routes.
- **Turbo's keys vs sopen's** (§1.4): F2 Save, F3 Open, F5 Zoom, F6 Next window, Alt+X Exit and the
  WordStar Ctrl+Q / Ctrl+S / Ctrl+X / Ctrl+C / Ctrl+Z / Ctrl+W keys collide with sopen's F-key row
  (04 §2, 07 §10, 08 §7.5) or its Cmd/Ctrl meanings (Save, Undo, Cut, Copy, close). The table in
  §1.4 decides each; where Turbo and sopen disagree on an F-key, sopen's row wins except F2 Save and
  F6 next window.
- **F2 in the Text view vs F2 in the editor**: the view's F2 toggles wrap (01 A11, "W or F2"); the
  editor's F2 is Turbo's Save. One view, two states, two meanings — a mis-press costs a wrap toggle
  or a conflict-checked Save with a backup, never lost text. Kept for Turbo's F2; the owner may veto.
- **07 §6's Save copy key under legacy encoding**: Shift+Ctrl+S arrives as Ctrl+S [M], so in a
  legacy terminal "Save copy" saves over the file (conflict-checked, backed up). This is true of
  the hex window too; the editor's menu offers Save copy, and the help says so. → 12 E31: in a
  terminal, until Shift is known to arrive with Ctrl, the editor's Ctrl+S arms `^S` instead.
- **Ctrl+] in a legacy terminal is F5** [M]: 0x1D decodes as Ctrl+5, which the F-key hook makes F5,
  Save in the editor and the hex window. Not introduced here; listed in the help. → 12 E5: while the
  editor has the keyboard, no digit becomes F2 or F5.
- **Turbo C++ 3.0's Ctrl+Home/End** (window top/bottom in the Alternate set) **vs Notepads and TC++'s
  CUA set** (file start/end): §1.4 takes file start/end for both pairs.
- **Highlighting a keystroke** costs 3–13 ms on a worker with syntect against 0.5 ms with
  tree-sitter [M]; §4.2 keeps syntect for one colour mapping and 0 bytes, and accepts a frame of old
  colours on the edited line.

## Sources

sopen (main 77b0beb): `.prd/01-spec.md` §5, §9, A2, A11; `02-architecture.md` §2–§4, §7; `03-git-panel.md`
§5, §6, §7.6, §9; `04-file-keys.md` §2–§3, §6.1; `06-streaming.md` §7.2; `07-streaming-edit.md` §1–§4,
§10–§11; `08-viewer.md` §1–§7; `10-tui.md` §1–§17; `xtools/ssot.md`, `xtools/loop.md`; `research/keys.md`
(PC Shell 6's F-key bar, p. 67); `AGENTS.md:105`. Code as cited inline.

Published documents:
- VS Code's text buffer (a piece table in a tree): https://code.visualstudio.com/blogs/2018/03/23/text-buffer-reimplementation
- xi-editor, "currently discontinued": https://github.com/xi-editor/xi-editor (README)
- crates.io API: https://crates.io/api/v1/crates/xi-rope (0.3.0, 2019-06-29), https://crates.io/api/v1/crates/ropey (1.6.1 stable, 2.0.0-beta.1)
- encoding_rs `output_encoding`: "This is UTF-8 for UTF-16BE, UTF-16LE, and replacement":
  https://docs.rs/encoding_rs/0.8.35/encoding_rs/struct.Encoding.html#method.output_encoding

Period manuals and screenshots: §1.1's table. Not found there: any default colour value of either
Borland IDE (TCINST names three preset sets, TC2-RG p. 573; TC3-UG names the items only, p. 37);
Turbo C++ 3.0's insert/overwrite indicator; a Turbo C++ 3.1 manual; PC Tools 7's Desktop manual
(lending only on archive.org). Two manuals disagree with their own screens: TC3-UG Fig. 2.2's status
line has `F7 Trace F8 Step` where the Commons capture has `Alt-F9 Compile`; TC3-UG Table A.1 prints
"Move to previous position" as Ctrl+P, which is also "Insert control character" (TC 2.0: Ctrl-Q P).

Prototype folder receipts: `proto-buffer/` (`buffer-run1.txt`, `proto-buffer lossless`), `hl/`
(`hl-run1.txt`, `hl-syn-run2.txt`, `sizes.txt`, `syntect-syntaxes.txt`), `keys-table.tsv`,
`keys-summary.txt` and `keys-raw.tsv` (the scratch clone's `gui::term_input::tests::xre_editor_keys`
and `xre_raw_control_bytes`, ignored tests appended there and never in the repo), `mockups/mock.py`
and its `*.txt`, `refs/` (the manuals' OCR text and page images read).
