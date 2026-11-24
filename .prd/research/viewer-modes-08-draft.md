# 08 — the viewer: views 1 / 2 / 3 and text encodings (draft sections)

Status: draft (2026-11-24) — the mode and encoding sections of a future `08-viewer.md`, for the
orchestrator. Not the contract yet. The evidence is the VR-M trace (`viewer-modes.md`, cited as
*trace* §n); code citations are at `docs/viewer-ssot` 767a8a8.

**How this draft is cut.** xtools' screen is open (a terminal UI by default, the GUI drawing the same cell
screen, is being researched). So §A is the **model** — what views a file has, which key means what, what a
switch keeps, how text is decoded from byte ranges — and holds whatever screen xtools gets. §B binds the
model to today's egui windows; §C says what a terminal changes. The work units of §F are provisional and
are not cut along today's egui files.

Owner message 20 (the workstream's `ssot.md`, verbatim lines):

> 프리뷰가 아니라 엔터 눌렀을때 표시되는 곳에서는 현재 파일을 어떤식으로 보여줄지 정할 수 있도록해줘
>
> 디폴트로 뷰어 모드인거고
>
> 이미지면 디폴트로 이미지로 보여주는데 모드를 헥스 모드 선택할수 있게 해줘. (모든 파일은 헥스 모드로 선택할 수 있음.)
> html이나 json등은 그냥 raw text 모드랑 hex mode모두 선택할수 있어야함.
> (이 뷰어상태에서 디폴트로 1이 디폴트 뷰, 2가 hex 뷰, 3이 기타 뷰로해줘. 그리고 텍스트 모드일경우에는 ascii, utf8, utf16, ucs2, ucs4 등 인코딩을 고를수 있어야함.)

Amends, when it lands: 01 §3 (a UTF-16/32 file is text, D18) and §3a (Enter opens the file's views; the
memory line's Hex view key; a default view that cannot show the file gives way to hex, D10); 04 §2 (the
viewer's slots: F8 Encode in text, F9 View — and 04's stale viewer slots, Tensions); 04 §3.1 (the
viewer's keys); 06 §6 (a viewer's channels, D14); 06 §7.2 (decoding per encoding, §A5); 06 §7.3 (Hex view
is view 2); README (the image's 100 % key, D12); AGENTS.md's receipt table (`SOPEN_STATE` `view=`, `enc=`).

## A. The model

### A1. Words

- **The viewer**: what Enter (or F3) on a file opens — not the preview pane, which always shows the
  default view and takes no keys (02 §4.2).
- **A view**: one way of showing the opened file — Image, Video, Audio, Page, Text, Raw, Hex.
- **The views of a file**: three numbered slots, 1 (the default view), 2 (Hex), 3 (the other view). A slot
  may be empty for a kind (`—`).
- **The view set**: the views of one opened file that exist at the moment — the one on screen and the
  ones kept hidden (§A4).
- **The place**: where a byte view is in the file — one byte offset (06 §7.2 already keys the text view by
  byte offset; the hex view's cursor is a byte offset).

### A2. The views of a file

| File | 1 (default) | 2 | 3 | Notes |
|---|---|---|---|---|
| image (png, jpg, webp, bmp, gif, tif) | Image | Hex | — | D7. The memory line (06 §7.3) is view 1's first state; its Hex view is 2 |
| image that does not decode | (the reason) | Hex — opens here, the reason first | — | as today (*trace* §1.3); 1 says the reason and stays on 2 |
| video | Video | Hex | — | |
| audio | Audio | Hex | — | |
| video none of whose frames decode; empty media (0 bytes) | (the reason) | Hex — opens here, the reason first | — | **changes**: today the first closes with the reason on the status row and the second is an error (*trace* §1.3). D10: every file can be shown in hex |
| `.ts` | as its kind (text, video, or hex when ffprobe cannot read it) | Hex | per kind | unchanged routing (`kind.rs:167-194`) |
| plain text, source code, JSON, YAML, CSV, SVG, … (text by extension or sniff) | Text — highlighted by syntax | Hex | Raw — the same text, not highlighted | D7, D8 |
| HTML, HTM, XML | Page (message 18) | Hex | Text — the source, highlighted | D9: until the page view lands, 1 is Text and 1 and 3 are the same view |
| a UTF-16 or UTF-32 text file of any extension | Text, decoded by its BOM or pattern | Hex | Raw | **changes**: today an extension-less one opens in hex (`kind.rs:157-165, 331`), D18 |
| any other file (binary) | Hex | Hex | Text — decoded by §A5's default | 1 and 2 are the same view. Text shows a binary's strings and its UTF-16 text |
| an empty file (0 bytes), not media | as its kind | Hex (empty: `hex/tests.rs:1314`) | as its kind | |
| a file at a revision, a diff (03 §6.2) | as its kind, read-only | Hex, read-only | as its kind, read-only | every view is told read-only before its first pass (03 §6.3) |
| a folder, a fifo/socket/device, a dangling link, a name that is not UTF-8 | not opened, as today | — | — | `browser.rs:1780-1783, 1824-1831` |

- A slot that is `—` for the kind does nothing when chosen; the status row says so (`an image has no third
  view`). An empty slot is shown as such (§B1).
- Raw is the Text view with highlighting off — one view state of the text document, not a second document
  (D8): it shares the index, the encoding and the place.
- The preview pane is unchanged: it shows view 1, as today. Enter on a previewed file hands the preview
  over as view 1 (02 §4.4).

### A3. The view keys

- **1, 2, 3** choose the view in that slot. Choosing the view on screen does nothing.
- **Where they act**: in the viewer, whatever view is on screen — except in three states the view
  declares, in which a digit is not a view key:
  1. **Digits are data**: the hex view, unless it is read-only (a file at a revision,
     `hex.rs:2270-2275`). There a digit is typed as a nibble or an ASCII byte, as today
     (`hex.rs:1002-1025`) — also while a reload or a Save holds typing back (`hex.rs:3664-3671`): the
     digit then says why it was not typed, as today, and never switches the view. The state does not
     flicker with the hex view's transient states.
  2. **A field has the typing**: Go to (hex, text), the hex window's read-only path field, Save As's path,
     the encoding list's filter if it has one. The field owns its digits (04 §3 rule 4).
  3. **A question is up**: the close prompt, Save As, a confirmation, the encoding list. The question has
     the keyboard; no view key acts.
- **The fallback, where digits are data: F9, then 1, 2 or 3** (D4). F9 is labelled `View`; it arms the
  view keys for one key: the next 1, 2 or 3 chooses that view, Esc or F9 again disarms, any other key
  disarms and is dropped (it is never typed into the hex view). F9 works in every view, so the same two
  keys always work; plain digits are the short form wherever digits are not data.
- **One press, one action.** A press that chooses a view is not also typed, and a press that is typed
  never chooses a view.
- **What a view key is**: the digit pressed with no modifier. Shift+digit, Alt+digit and Cmd/Ctrl+digit are
  not view keys (Cmd/Ctrl+1…0 stay F1…F10, 04 §6.1). §B2 and §C say how each screen tells a digit from
  typed text.
- **A switch and a leave in the same input** (a view key and Esc together): the leave wins; no view is
  made. (PR #61's race, for the image's give-way: *trace* §1.3.)

### A4. A switch: what it keeps, what it does with edits

- **Clean views are kept** (D1). The view switched away from is hidden, not closed: switching back shows it
  as it was — zoom and rectangle (image), playhead, In/Out and selection (media), wrap (text), the typing
  column (hex). A hidden view draws nothing, takes no input, reads nothing for a screen and polls nothing
  (06 §5's "a viewer that is hidden does not poll"), and plays nothing.
- **A view is made the first time it is chosen**, from the means the opening kept (§A6): a remote file's
  open stream when there is one, else the same preparation as an opening — on a worker, with its progress
  and Esc on the status row.
- **Unsaved edits are never carried across or dropped unasked** (D2). Choosing another view while the view
  on screen holds unsaved edits asks what closing asks (07 §10): `Save the changes to host:/abs before
  closing?` → **Save** · **Save As…** · **Discard** · **Cancel**. Save or Save As that lands, or Discard,
  then closes that view and shows the chosen one; Cancel stays. A view whose Save is running makes the
  switch wait for it, as closing does (`saving…`, then the switch). So at most one view of a file ever
  holds unsaved edits — the one on screen — and every hidden view shows the file as it is stored.
- **The place follows the owner between byte views** (D11): Text, Raw and Hex share one place.
  - Text → Hex: the hex cursor goes to the first byte of the top row on screen, that row at the top.
  - Hex → Text: the text view opens (or moves) to the cursor's byte, aligned down to the encoding's code
    unit (§A5), as `at=` opens it today (`text.rs:5244-5246`).
  - Text ↔ Raw: one view state; nothing moves.
  - Image, Video, Audio and Page have no byte place: switching to a byte view from them keeps that byte
    view's own last place (byte 0 the first time); switching back to them keeps their own state.
- **Selections stay where they were made.** Only the image (its rectangle) and the media view (its
  selection and marks) have one (*trace* §4.1); they stay in their hidden view. Text and hex have none.
- **Leaving** (Esc, F3) asks the view on screen as today; hidden views are clean and close with it.
- **A reload** (Cmd/Ctrl+R) acts in the view on screen. So does a Save's own reload: a hex Save
  reloads its stream once it lands (`source.reload_saved`, `hex.rs:2636`, `hex/file.rs:621-623`), which,
  on the shared stream, moves the bytes under every hidden view — and a whole image holds its own copy of
  the bytes in memory (`image.rs:4725`), which no reload reaches. So **a Save or a reload that lands in
  one view makes every hidden view stale**, and a stale view is made again when it is next shown, at the
  place it had (D25). A change made elsewhere is what each view already handles (06 §5's states).
- **One opened file, one encoding**: the encoding chosen in Text or Raw is the file's for this opening
  (D21); a Text view made later (view 3 of a binary) starts with it.

### A5. Text encodings

**The list** (D15, D16). Offered in the text views (Text and Raw):

| Name shown | Code unit | A character | Invalid input, each shown as one U+FFFD |
|---|---|---|---|
| ASCII | 1 byte | one byte 0x00–0x7F | every byte 0x80–0xFF |
| UTF-8 | 1 byte | 1–4 bytes | as today: each maximal bad sequence (`from_utf8_lossy`, `wrap.rs:47-80`) |
| UTF-16LE, UTF-16BE | 2 bytes | one unit, or a surrogate pair | an unpaired surrogate (one per unit) |
| UCS-2LE, UCS-2BE | 2 bytes | one unit; no pairs | every surrogate unit (D800–DFFF), one per unit |
| UTF-32LE, UTF-32BE (UCS-4) | 4 bytes | one unit | a value above 10FFFF or a surrogate, one per unit |
| Latin-1 (ISO-8859-1) | 1 byte | one byte = U+0000–U+00FF | none |
| CP437 | 1 byte | one byte, the skin's own table (`chrome::cp437`, `chrome.rs:639-647`) | none |

- A file whose size is not a multiple of the unit ends in one U+FFFD for the partial unit.
- **The admission rule**: an encoding is offered when any code-unit-aligned offset can be decoded from with
  at most one unit of look-back (UTF-8: three bytes, as today). That is what lets a text view start at a
  block edge, a 16 KiB grid point past the 16 MiB index (06 §7.2, `text.rs:77-92`) or a jump's byte. The
  list above passes it. It leaves out the legacy multi-byte code pages (CP949/EUC-KR, Shift-JIS, GB18030,
  Big5): their lead and trail byte ranges overlap, so a character start is found only by decoding from
  the line's start. LF is never a trail byte in any of them (python3's codecs, every BMP character:
  smallest trail byte 0x41 CP949, 0xA1 EUC-KR, 0x40 Shift-JIS, 0x30 GB18030, 0x40 Big5), so lines are
  found, but past the 16 MiB index a line's start may not be known (06 §7.2). It leaves out the stateful
  ones (ISO-2022, UTF-7: the shift state runs from the file's start). Each would also need a crate sopen
  does not have (no encoding crate is in `Cargo.lock`). The owner's "등" is read through this rule; a new
  owner message can add one — CP949 could be offered on lines whose start is known (D15).
- Control characters are drawn as today (C0 and DEL as their CP437 glyphs, `chrome.rs:650-655`).

**The default** (D17), picked from the file's first bytes before its first screen — for a remote file when
its first block arrives, in the pass that picks the syntax (06 §7.2); for a local file before its index
pass (the marks depend on it):

1. A BOM: `EF BB BF` UTF-8; `FF FE 00 00` UTF-32LE when the size is a multiple of 4, else UTF-16LE;
   `00 00 FE FF` UTF-32BE; `FF FE` UTF-16LE; `FE FF` UTF-16BE.
2. Else a pattern in the first block (at most 64 KiB): UTF-16LE when at least 3/4 of the odd-offset bytes
   are 0 and fewer than 1/16 of the even-offset ones are; UTF-16BE mirrored; UTF-32LE/BE likewise on
   4-byte groups. Such a file is mostly ASCII in UTF-16 (a Windows log, a `.reg` export); Korean UTF-16
   without a BOM does not match and opens as UTF-8, where the owner picks.
3. Else UTF-8 — valid or not, as today. ASCII is never the default (all ASCII is UTF-8).

- **The BOM is not text** (D26): line 1 starts after it, so the syntax is found by the first line
  (`#!/bin/sh` after a UTF-8 BOM) and Go to `0x…` still counts file bytes.
- **Routing follows** (D18): a file whose kind the sniff decides (no known extension, `kind.rs:238-243`)
  is text when step 1 or 2 finds UTF-16 or UTF-32, not "other". A file with a text extension is text
  already; its encoding is picked as above.

**How it is shown and changed** — the view states it; the screen draws it (§B1):
- The text view says its encoding beside its syntax: `1,204 lines · 88 KB · Rust · UTF-16LE (BOM)`; `(BOM)`
  when step 1 picked it, `(guessed)` for step 2, nothing for UTF-8 by step 3 or the owner's choice.
- **F8 `Encode`** (and plain `E`, as `W` is F2's) opens the list of §A5's encodings, the current one
  marked; ↑ ↓ and Enter choose, Esc leaves it as it was (D19). While it is open it has the keyboard.

**Decoding over byte ranges** — every rule of 06 §7.2 stays; what changes is the unit:
- **Every offset the text model makes is a multiple of the code unit**, counted from byte 0 (a BOM's
  length is a multiple of its unit). A jump (End, Go to `0x…`, `at=`, the scroll bar by bytes, the hex
  view's place) is aligned down; in UTF-16, a place on a low surrogate moves back one unit.
- **A line ends at the LF code unit at an aligned offset**, and a CR is the unit just before it — never a
  `0x0A` byte inside another unit. (Today a byte search: in UTF-16LE the next line would start on the
  `00` byte, and 44 of the 11,172 Hangul syllables, U+AC0A 갊 among them, carry a `0x0A` byte: *trace* §3
  T2.) Only LF and CR LF end a line, as today; NEL, U+2028 and a lone CR do not.
- **The index pass** (local and streamed) steps by units; its marks, `SPAN`, `CHUNK`, the 16 KiB grid,
  `LINE_MAX` and the 16 MiB cap stay byte bounds (all multiples of 4: *trace* §3 T16).
- **A piece that ends inside a character** (a block, a grid point, `LINE_MAX`): UTF-8 as today
  (`cut_char_start`); UTF-16: a high surrogate as the last unit is left for the next piece; every other
  encoding has no multi-unit character.
- **One decoder per encoding** gives a piece's characters and their byte lengths (the shape of
  `wrap::Cell { ch, len }`, `wrap.rs:40-45`); the line text, the row text, the highlighter's input and the
  per-character colour count all come from it, so they cannot disagree (*trace* §3 T10, T13).
- **Changing the encoding builds the index again from byte 0** (the marks depend on the LF unit) and drops
  what depends on the old one: wrap's memos and the highlighter's states. Through a stream: the background
  pass again over at most the first 16 MiB (2–8 s on the rig, 06 §7.2); past them the byte-position path
  needs no index. A local file: the whole-file pass again, with progress, Esc keeping the old encoding
  (D22). The page keeps its byte place, aligned.
- A Text view (3) of a local file that is not text uses today's local route: the whole-file pass before
  its first screen, with progress and Esc (D23).

### A6. What the opening keeps to make the other views

- The kind, the sniffed head, the file's stat at open, read-only or not, and **the path of the bytes on
  screen** — for a revision or a diff, the `.revisions` copy and its local connection, not `Viewer.abs`,
  which is the working file F4 opens (`browser.rs:1659-1664`, *trace* §1.2) (D13).
- For a remote file, **one stream for every view** (D13): the opening's `Source` is kept beside the views,
  so making a view reads nothing again (as today's give-way does, 06 §7.3). The image view drops its own
  handle once the picture is whole (`image.rs:4717`); the kept handle keeps the stream for the others.
- Where no stream was kept (a handed-over image preview, whose stream closed: `prepare.rs:887-890`), a view
  that needs one opens it on a new connection, as an opening does.

## B. The screen today (egui)

### B1. Where the views are shown

- **The view strip goes in the browser's title row**, right-aligned before the clock, drawn by the browser
  (which owns the row: `browser.rs:2793`, `chrome.rs:713-761`): `1 Image · 2 Hex · 3 —`. The view on screen
  in the highlight bar's colours (white on light red), an empty slot dim, and while F9 is armed the strip
  in the warn colour. Each entry is clickable, as the F-key row's slots are (04 §6.1). The place is cut
  from the left first, as today. (D6)
- **Not the F-key row**: its digits are F-keys, Cmd/Ctrl+digits and clickable slots already (04 §6.1); `1`
  there is Help and `3` is Exit.
- The F-key row adds `9View` in every view and `8Encode` in Text and Raw.
- The text view's top bar adds the encoding (§A5).
- A view whose default cannot show the file (§A2) says why in its first row, as today's
  `"…; the file is shown as bytes"` (`prepare.rs:245`).

### B2. How the keys are taken

- The browser takes the view keys **before the viewer's pass** (where the shell's Cmd/Ctrl+digit hook
  already works, `mod.rs:785-796`), not from what the pass leaves: a key row matched after the pass
  double-acts in hex, which reads typed text without consuming it (`hex.rs:3650-3659`), and never sees the
  image's consumed `1` (`image.rs:3267`) (*trace* §2.3, §6).
- The view declares its state as of the last pass (`Free`, `DigitsAreData`, `Held`); a field's typing is
  `text_edit_focused` — **not** `egui_wants_keyboard_input`, which is true whenever the hex grid has focus
  (egui 0.37 `context.rs:3485-3496`, *trace* §2.4).
- A digit is the key event with no modifier, the **physical** key first, as the F-key aliases read it
  (`mod.rs:866`); the typed text that follows it (egui-winit pushes the key, then its text:
  `lib.rs:1128-1180`) is dropped with it when it chooses a view. In `DigitsAreData` the browser removes
  the key event and leaves the text for the hex view.
- The viewer's rows in the key table (`keys.rs:500-503`) gain `1`, `2`, `3` and `F9` — so F1's help lists
  them and the one-table rule holds (04 §3 rule 5) — but the after-pass lookup never sees the digits (they
  are taken before).
- **The image's 100 % key becomes `A`** (`F` fits, `A` actual pixels; D12) in the browser; the standalone
  image window keeps `1` too, since it has no views (D24). README's line changes.
- **The memory line** says `Continue (Enter) · Hex view (2) · Close (Esc)` in the browser; `H` stays its
  alias (D12).

### B3. Hidden views

- Hidden = not drawn and `set_shown(false)`; shown again = `set_shown(true)`. A stream has one visibility
  flag for all its handles (`source.rs:1411-1415`): the browser hides the old view **before** it shows the
  new one.
- `set_shown(false)` must pause the media view's playback: today the media window has no `set_shown` and
  plays on its own thread (`media.rs:1724, 2259, 2306`; *trace* §4).
- A view is hidden only when it holds no unsaved edit and no running write (§A4), so nothing it would do in
  its pass (journal ticks, a Save's polling: `hex.rs:4595-4597`) is pending while it is hidden.
- **Channels** (06 §6): one stream for the set keeps the viewer's 1 `Ranged`. Each view has its own
  `Origin`, which connects at its first Save. A view hidden after a Save gives its `Origin`'s session back
  (back to the placeholder that connects on next use), so a viewer stays at "1; 2 from its first Save"
  (D14).

## C. A terminal screen (if the screen research picks one)

The model (§A) holds; the key reading changes (*trace* §7, all `[추정]` until measured):
- A terminal sends the character `1`, no key event and no physical key: a view key is the character with
  no modifier. An AZERTY owner needs Shift there, as for typing a 1 anywhere.
- Cmd/Ctrl+digits (04 §6.1) do not survive a terminal: Cmd never arrives, and in the legacy encoding
  Ctrl+3 is ESC (it would leave the viewer) and Ctrl+1 can arrive as a plain `1` (it would switch the view).
  The F-key aliases need the kitty keyboard protocol or modifyOtherKeys there, or a different alias.
- Alt+digit is not a usable fallback in a terminal (Option composes on macOS; with Meta it is ESC then the
  digit). That is why §A3's fallback is F9-then-digit: an F-key is an escape sequence in every terminal —
  with message 19's open question of a Mac's top row.
- An image or a video view exists only where the screen can draw one; on a text-only terminal their slot
  1 shows the reason and the file opens in Hex (as §A2's "cannot show" rows).

## D. Decisions

| # | Decision | Why | [decision] for the owner? |
|---|---|---|---|
| D1 | Clean views are kept hidden; switching back is instant and keeps zoom, playhead, wrap, cursor | a remote image re-read whole on every 1↔2 would cost its size each time; today's give-way already reuses the stream | [decision] — alternative: close the old view on every switch (simpler, loses view state, re-reads) |
| D2 | Switching away from unsaved edits asks 07 §10's prompt; a running Save makes the switch wait | reuses the shipped closing rules; only one view ever holds edits, so no view shows bytes another has unsaved, and one journal per file at a time (07 §3) | [decision] — alternatives: refuse the switch until saved; or keep the dirty view hidden and refuse edits in the others |
| D3 | A view key is the digit with no modifier, physical key first in the GUI, its typed text dropped | one press one action; the same reading as Cmd/Ctrl+digit (04 §6.1); Shift+digit stays a character | [decision] — alternative: match the typed character (04 §3 rule 2; AZERTY needs Shift) |
| D4 | Where digits are data (the editable hex view), plain digits type; **F9 then 1/2/3** chooses a view, in every view | F9 is free in every viewer (*trace* §5), types no text in either screen, is sendable by a terminal, and its slot is clickable; Alt+digit types text in the GUI and is not sendable reliably by a terminal (*trace* §2.4, §7) | [decision] — alternatives: Alt+1/2/3 (GUI only, its text stripped before the pass); F9 cycling 1→2→3 |
| D5 | No view key acts while a field has the typing or a question is up | 04 §3 rule 4; the prompt must keep its keyboard | — |
| D6 | The view strip is in the title row, not the F-key row | the F-key row's digits are F-keys and Cmd/Ctrl+digits (04 §6.1) | [decision] — alternative: in the view's own frame border |
| D7 | The third view: Text where 1 is not text (HTML/XML source, binary); Raw where 1 is text; none for image, video, audio | ssot item 4 ("HTML, JSON … raw text (3)"); a JPEG as text is noise | [decision] — richer options: Raw that also shows tabs, CRs and the BOM; pretty-printed JSON; an image's metadata as view 3 |
| D8 | Raw is the Text view with highlighting off (one document, one place, one encoding) | the smallest thing that is "raw text"; no second index | — |
| D9 | HTML/HTM/XML: 1 = Page once the web view lands; until then 1 = Text | message 18; the engine is VR-W's | — |
| D10 | A default view that cannot show the file gives way to Hex with the reason — also a video none of whose frames decode, and empty media | "모든 파일은 헥스 모드로 선택할 수 있음" | [decision] (behaviour change) |
| D11 | Text, Raw and Hex share one byte place; image, media and page keep their own | the place is a byte offset in both models already (06 §7.2, `hex.rs:2055`) | — |
| D12 | The image's 100 % key becomes `A` in the browser (standalone keeps `1`); the memory line names `2`, `H` stays an alias | `1` is view 1 in the viewer; README documents `1` today (`README.md:288`) | [decision] (a documented key moves) |
| D13 | One stream per opened remote file, kept beside the views; a revision's views read its `.revisions` copy | nothing read twice (06 §7.3's rule); `Viewer.abs` is the working file | — |
| D14 | A hidden view gives its `Origin`'s session back | 06 §6: the window is at 8–9 of sshd's 10 | [decision] — alternative: restate 06 §6 as "+1 per view that saved" |
| D15 | Encodings: ASCII, UTF-8, UTF-16LE/BE, UCS-2LE/BE, UTF-32LE/BE, Latin-1, CP437 — by the admission rule of §A5 | message 20's list, plus two one-byte tables that cost nothing (CP437 is the skin's) | [decision] — Latin-1 and CP437 are additions; CP949/EUC-KR left out by the rule (could be offered on indexed lines only, with a new crate) |
| D16 | UCS-4 is one entry with UTF-32 | they decode alike for every value that is a character | — |
| D17 | Default: BOM, then the NUL pattern, then UTF-8 | a BOM is certain; the pattern catches ASCII-heavy UTF-16; anything else is today's behaviour | [decision] — the pattern's thresholds |
| D18 | A UTF-16/32 file without a known extension routes to Text | today it opens in hex (`kind.rs:157-165`, its test `:331` changes) | [decision] (behaviour change) |
| D19 | F8 `Encode` (and `E`) open the encoding list in Text and Raw | F8 is free in the text view; `E` mirrors `W`/F2 | [decision] — the key |
| D20 | Invalid input is one U+FFFD per invalid unit or sequence, as today; nothing counts them | no extra pass over the file | — |
| D21 | The encoding is per opening, not remembered for the file | nothing is stored about files today | [decision] — alternative: remember per path in the state folder |
| D22 | Changing the encoding builds the index again; Esc during a local pass keeps the old one | the marks depend on the LF unit | — |
| D23 | Text view (3) of a local non-text file uses the local whole-file pass, with progress and Esc | today's local route; a local stream (`LocalFs` is a `Ranged`, `local.rs:251-260`) is untested here | [decision] — alternative: read local files through a stream too |
| D24 | Views exist in the browser's viewer only; standalone windows keep their keys | message 20 names the window Enter opens | — |
| D25 | A Save or reload in one view makes the hidden views stale; each is made again when next shown | a hex Save reloads the shared stream (`hex.rs:2636`); a whole image keeps its own bytes (`image.rs:4725`) | — |
| D26 | The BOM is not text; line 1 starts after it | syntax by first line; Go to counts file bytes | — |

## E. Acceptance

Tests run on the test kit's real windows (`src/gui/testkit.rs`) through the browser fixture
(`src/gui/browser/tests.rs:102-155`; VM-4 and VM-5's rules also as plain unit tests, with no window). **Every key test sends what the platform sends**: the key event with
its physical key **and** its typed text in one frame (`Driver::frame` under `as_shell()`), because
`Driver::key` sends no text (`testkit.rs:240-252`) and would pass while the real keyboard double-acts
(*trace* §2.4).

**Views**
- V-1. For each row of §A2 (local and through the stream fake): Enter opens view 1; 2 shows Hex; 3 shows
  the third view or says it has none; the strip names the view on screen.
- V-2. Hex of an editable file: `1` (key + text) types one nibble, no switch; F9 then `1` shows view 1 and
  types nothing; F9 then `x` types nothing and switches nothing; in the ASCII column `1` types 0x31.
- V-3. A Go to field open (hex, text): `1` goes into the field; F9 does nothing.
- V-4. Cmd/Ctrl+1, +2, +3 in every view are still F1, F2, F3 (help; wrap in the text views; exit), and
  switch no view. F9 is taken from what the view's pass leaves (no view takes F9 today: *trace* §2.2);
  the digit after it is taken before the pass, as any view key.
- V-5. The close prompt, Save As or the encoding list up: 1/2/3 and F9 do nothing.
- V-6. `2` and Esc in one frame: the viewer leaves; no view is made.
- V-7. Image: zoom and rectangle survive 1→2→1; media: the playhead survives and nothing plays while
  hidden; text→hex puts the cursor on the top row's first byte; hex→text opens at the cursor's line.
- V-8. A dirty hex view, `1`: the prompt; Cancel keeps the edits and the view; Discard shows view 1 and
  removes the journal; Save shows view 1 after the save lands. A running Save: `1` waits, then switches.
- V-9. Remote: 1→2→1→2 on an image reads the file once (`SOPEN_STREAM_STATS` `fetched=`); a handed-over
  image preview's `2` opens one stream.
- V-10. A revision: `2` shows the revision's bytes, read-only; never the working file.
- V-11. A video none of whose frames decode, and empty media: opens in Hex with the reason (D10).
- V-12. F1 in the viewer lists 1, 2, 3 and F9; `keys.rs`'s `no_duplicate_bindings` still passes.

**Encodings** (the text document's tests, local and through the stream fake, at both sides of the 16 MiB
index cap)
- T-1. UTF-16LE and BE with a BOM: line count, numbers, CR LF, a line of Hangul with U+AC0A not broken;
  UTF-32LE/BE likewise.
- T-2. Invalid input: a UTF-16 unpaired surrogate, a UCS-2 surrogate pair (two U+FFFD), a UTF-32 value
  above 10FFFF, an odd trailing byte, ASCII 0x80: each one U+FFFD; Latin-1 and CP437 over all 256 bytes.
- T-3. Past 16 MiB: a jump to an odd offset in UTF-16 aligns down; a grid piece on a low surrogate starts
  one unit earlier; rows reached from any restart point equal those reached from the line start (the
  existing wrap invariant, per encoding).
- T-4. Defaults: BOM, the pattern (`(guessed)`), Korean UTF-16LE without a BOM opens as UTF-8, invalid
  UTF-8 stays UTF-8; `FF FE 00 00` with a size not a multiple of 4 is UTF-16LE.
- T-5. The kind sniff: an extension-less UTF-16 file is text (the test at `kind.rs:331` changes).
- T-6. Changing the encoding keeps the byte place (aligned), re-indexes, highlights again; Esc during a
  local pass keeps the old encoding.
- T-7. A UTF-8 BOM before `#!/bin/sh`: shell highlighting; line 1 has no BOM character.
- T-8. The preview pane and the viewer decode a UTF-16 file alike.

**Live receipts** (AGENTS.md: screenshots someone looked at; remote ones under `/tmp/sopen-receipts/`)
- R-1. Screenshots of each §A2 kind in views 1, 2 and 3, locally and on a remote, with the strip — through
  new `SOPEN_STATE` keys `view=N` and `enc=NAME` (VM-6; `State::parse` rejects unknown keys today, 02 §8.1).
- R-2. Owner at the Mac: 1/2/3 on the US layout and with the Korean input source on; F9 then 1 in a hex
  view; the same on the Linux laptop under KDE (Alt+digits are unbound there, `research/keys.md:93-94`).
- R-3. A remote UTF-16 log (BOM) opens as text, first screen from block 0 (`first` on the stats line); its
  line count once indexed.
- R-4. A remote binary of several GB: `3` shows text from block 0, no whole-file read.

## F. Work units (provisional)

The screen is open, so these are cut by model component, not by today's egui files. Shared files are
sequenced, not disjoint (07 §14's rule).

| ID | Unit | Owns (model) | After | Notes |
|---|---|---|---|---|
| VM-1 | **Encoding core**: the encoding list, the default pick (BOM, pattern), one decoder per encoding (characters + byte lengths), unit-aligned LF/CR search, character start / cut rules, alignment of a place | a new module beside the text document; its tests | — | pure functions over byte slices; no screen |
| VM-2 | **Text document per encoding**: the index pass, line reads, spans, look-back, grid pieces and jumps through VM-1; the BOM not text; changing the encoding rebuilds the index | the text document (today `TextDoc`, `text.rs`) and its tests | VM-1 | the line/row decode that `wrap.rs` does today moves to VM-1's decoder |
| VM-3 | **Kind sniff**: UTF-16/32 → text | `kind.rs` | VM-2 | without VM-2 such a file would open misaligned |
| VM-4 | **The view set**: §A2's table, §A4's switch rules (keep clean, ask on edits, wait on a Save, the place mapping, reload rule), §A6's kept means — as a screen-free model with unit tests | a new module | — | the screen asks it "what does 2 do now" |
| VM-5 | **The view keys**: §A3 as a function from abstract key input + the view's state to a view command (plain digit, F9 armed, fields, questions, the leave-wins rule) | a new module | — | each screen binds its own input to it (§B2, §C) |
| VM-6 | **GUI binding** (only if the GUI stays egui): the strip, F9/F8 rows, taking keys before the pass, hidden views, the stream kept, the image's `A`, the memory line's `2`, media pause on hide, `Origin` release, `SOPEN_STATE view= enc=` | today: `browser.rs`, `browser/prepare.rs`, `browser/keys.rs`, `gui/mod.rs`, `chrome.rs`, and one hook each in the hex, image, media and text windows | VM-4, VM-5; VM-2 for the encoding list | split per viewer once the screen is fixed; shares `gui/mod.rs` with the browser workstream's round 3 (the input hook) and `browser.rs`/`prepare.rs` with the web view and the F4 editor |
| VM-7 | **TUI binding** | per the screen research | VM-4, VM-5, the screen decision | |
| VM-8 | **Page view** (1 for HTML/HTM/XML) | per VR-W | VM-4 and **the web-engine research (VR-W)** | the only unit that waits on the web engine |
| VM-D | **Docs**: 08 itself and the amendments listed at the top | `.prd/`, README, AGENTS.md | all | |

Order: VM-1 → VM-2 → VM-3; VM-4 and VM-5 beside them (disjoint); then the binding(s); VM-8 when VR-W lands;
VM-D last.

## Tensions

- **04 §2 and K-R1 vs the code.** 04 says a viewer's slots "2, 7, 9, 10 are empty" and the hex row reads
  `1Help 3Exit 4Edit 5Save 6Goto 8Asc/hx` (`04-file-keys.md:52-53, 493-494`); the code has F7 SaveAs in
  hex, image and media (ED3, `chrome.rs:146-168`) and F2 Wrap in text (`chrome.rs:190-201`). The code is
  the shipped contract (07 §14 ED3); 04 is stale. Unsettled until 08 rewrites 04 §2's viewer sentence.
- **"Raw text" for a JSON file.** The ssot reads JSON's raw text as view 3; JSON's view 1 is already text.
  D7/D8 make 3 the unhighlighted text; a pretty-printed or tree view as JSON's 1 would make "raw" mean
  "as stored" instead. Unsettled — the owner's call.
- **`1` = 100 % in the image** (README) vs `1` = view 1. D12 moves the image key in the browser.
- **The F-key fallback and message 19.** F9 is the fallback in a terminal too, but a Mac's top row sends
  media keys there unless fn is held; message 19's outcome decides whether F9 is reachable without fn.
