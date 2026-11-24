---
title: sopen — the viewer's views and text encodings
type: spec
tags: [sopen, spec, xtools, viewer, encoding]
---

# 08 — The viewer: views 1 / 2 / 3 and text encodings

Status: planned (2026-11-24). §2–§7 are the contract: the model of which views a file has, which key
means what, what a switch keeps, and how text is decoded over byte ranges; its units (§13) are ready
to build. §8 binds the model to today's egui windows and is **provisional** until XR-T decides the
screen; §9 says what a terminal binding needs from the model.

The target is item 4 of [`xtools/ssot.md`](xtools/ssot.md) (owner message 20), shaped by its item 2
(message 23): xtools becomes a terminal UI first, with a GUI that is the same screen, built as XR-T's
research decides ([`xtools/loop.md`](xtools/loop.md)). Owner message 20, verbatim:

> 프리뷰가 아니라 엔터 눌렀을때 표시되는 곳에서는 현재 파일을 어떤식으로 보여줄지 정할 수 있도록해줘
>
> 디폴트로 뷰어 모드인거고
>
> 이미지면 디폴트로 이미지로 보여주는데 모드를 헥스 모드 선택할수 있게 해줘. (모든 파일은 헥스 모드로 선택할 수 있음.)
> html이나 json등은 그냥 raw text 모드랑 hex mode모두 선택할수 있어야함.
> (이 뷰어상태에서 디폴트로 1이 디폴트 뷰, 2가 hex 뷰, 3이 기타 뷰로해줘. 그리고 텍스트 모드일경우에는 ascii, utf8, utf16, ucs2, ucs4 등 인코딩을 고를수 있어야함.)

Message 23's first line, verbatim:

> 그리고 이 xtools는 터미널 tui가 디폴트고 (이미지나 영상 빼고 다 실제 tui랑 1:1 매칭되고 잇는 ui임

The evidence is the VR-M trace, [`research/viewer-modes.md`](research/viewer-modes.md) (cited as
*trace* §n); this document turns its draft, [`research/viewer-modes-08-draft.md`](research/viewer-modes-08-draft.md),
into the contract. Code citations are at 767a8a8, the trace's commit; `src/` has not changed since
(`git diff --stat 767a8a8 e7df2f0 -- src` is empty). Decisions keep the draft's numbers (D1–D26);
D27 on are new. `[추정]` marks what was inferred, not read in code or measured. What it amends in 01,
04, 06, the README and AGENTS.md, and when, is §14.

## 1. The contract

- **Enter (or F3) opens a file's default view, view 1.** In the viewer **1** is the default view, **2**
  the hex view, **3** the other view. Every file has a hex view.
- **Where digits are typed** (the editable hex grid, a status-row field), **F9 then 1, 2 or 3** chooses;
  F9-then-digit works in every view, so it is also the route a terminal can always send.
- **A view switched away from stays alive, hidden**, and comes back as it was; nothing is read again.
  Leaving a view with unsaved edits asks 07 §10's close prompt; a running Save makes the switch wait.
- **The text views decode** ASCII, UTF-8, UTF-16LE/BE, UCS-2LE/BE, UTF-32LE/BE (= UCS-4), Latin-1,
  CP437 and CP949 / EUC-KR. The default comes from the file's first bytes; F8 (or `E`) opens the list.
- **Every rule of 06 §7.2 holds in every encoding** (the first screen from one block, the 16 MiB index,
  jumps by byte position past it). A line ends at the encoding's LF unit, never at a `0x0A` byte inside
  another character.
- **The preview pane is unchanged**: view 1, no keys (02 §4.4).

## 2. Words

- **The viewer**: what Enter or F3 on a file opens. **A view**: one way of showing it — Image, Video,
  Audio, Page, Text, Raw, Hex. **Slots**: 1, 2, 3; a slot can be *none* for a kind. **The view set**: the
  views of one opened file that exist now, on screen or hidden. **The place**: a byte view's byte offset
  (the text view's key since 06 §7.2's W5b; the hex cursor, `hex.rs:2055-2062`). **Byte views**: Text,
  Raw, Hex.

## 3. The views of a file

| File | 1 | 2 | 3 |
|---|---|---|---|
| image (png, jpg, webp, bmp, gif, tif) | Image | Hex | none |
| video, audio | Video, Audio | Hex | none |
| an image that does not decode, a video none of whose frames decode, empty media (0 bytes) | opens on 2, its first row the reason (D10) | Hex | none |
| text by extension or sniff (source, JSON, YAML, CSV, SVG, …); a UTF-16/32 file of any extension (D18) | Text, highlighted by syntax | Hex | Raw |
| HTML, HTM, XML | Page (09) — until 09 lands, Text | Hex | Text, the source highlighted — until then Raw (D30) |
| any other file (binary) | Hex (the same view as 2) | Hex | Text, decoded by §7.2's default |
| an empty file that is not media | as its kind | Hex | as its kind |
| a file at a revision, a diff (03 §6.2) | as its kind, read-only | Hex, read-only | as its kind, read-only |
| a `.ts` | as its refined kind (`kind.rs:167-194`) | Hex | as that kind |
| a folder, fifo, socket, device, dangling link, a name that is not UTF-8 | not opened, as today (`browser.rs:1780-1783, 1824-1831`) | — | — |

- **none**: the key does nothing and the strip shows no 3 (D7).
- **Raw is the Text view with highlighting off** (D8): one document, index, encoding and place. JSON's
  "raw text" in message 20 is Raw.
- The image's memory line (06 §7.3) is view 1's first state; its Hex view is 2, `H` kept as an alias (D12).

## 4. The view keys

- **1, 2, 3** choose the view in that slot; the view on screen chosen again does nothing.
- **A view key is the digit pressed with no modifier** (D3). Shift, Alt and Cmd/Ctrl with a digit are not
  view keys; Cmd/Ctrl+1…0 stay F1…F10 (04 §6.1). One press, one action: a press that chooses a view is
  not also typed, and a press that is typed never chooses a view.
- **Each view declares one state**, as of its last pass:

| State | When | A plain digit | F9 |
|---|---|---|---|
| Free | any view in no other state | chooses the view | arms |
| DigitsAreData | the hex view unless read-only (`hex.rs:2270-2275`), also while a reload or a Save holds typing back (`hex.rs:3664-3671`) | is typed as a nibble or an ASCII byte (`hex.rs:1002-1025`); held back, it says why — as today | arms |
| Field | a status-row field has the typing: Go to (hex, text), the hex window's read-only path field | goes into the field (04 §3 rule 4) | closes the field, as Esc would, and arms (D29) |
| Question | the close prompt, Save As, a confirmation, the encoding list | belongs to the question | does nothing |

- **Armed** (D4): the next key decides — 1, 2 or 3 chooses that view; Esc or F9 disarms; any other key
  disarms and is dropped, never typed. No timeout (D33).
- **Leave wins**: a view key and Esc or F3 in the same input leave the viewer; no view is made (the race
  PR #61 fixed for the image's give-way: *trace* §1.3).

## 5. A switch: what it keeps

- **Clean views are kept, hidden** (D1): zoom and rectangle (image); playhead, In/Out and selection
  (media); wrap (text); the typing column (hex). A hidden view draws nothing, takes no input, reads
  nothing for a screen, polls nothing and plays nothing.
- **A view is made the first time it is chosen**, from what the opening kept (§6), on a worker, with
  progress and Esc on the status row; Esc stays on the view on screen.
- **Unsaved edits** (D2): choosing another view while the view on screen holds them asks 07 §10's
  `Save the changes to host:/abs before closing?` → **Save** · **Save As…** · **Discard** · **Cancel**.
  A Save or Save As that lands, or Discard, closes that view and shows the chosen one; Cancel stays; a
  running Save makes the switch wait (`saving…`). So only the view on screen holds unsaved edits, hidden
  views show the file as stored, and a file has one journal at a time (07 §3; *trace* §4).
- **The place follows the owner between byte views** (D11). Text → Hex: the cursor on the first byte of
  the top row, that row at the top. Hex → Text: the text view at the cursor's byte, aligned down (§7.3),
  as `at=` opens it (`text.rs:5244-5246`). Text ↔ Raw: one view state. From Image, Video, Audio or Page
  (no byte place) a byte view keeps its own last place, byte 0 the first time.
- **Selections stay in their view** (the image's rectangle, the media view's marks; text and hex have
  none: *trace* §4.1). **Leaving** (Esc, F3) asks the view on screen; hidden views close with it.
- **A Save or a reload that lands in one view makes every hidden view stale** (D25): a hex Save reloads
  the shared stream (`hex.rs:2636`); a whole image holds its own bytes (`image.rs:4725`). A stale view is
  made again when next shown, at its place.
- **One opening, one encoding** (D21), shared by Text and Raw and by a Text view made later.
- **Channels** (D14): the set holds one stream (§6), so the viewer keeps 06 §6's one `Ranged`; a view
  that is hidden gives its `Origin`'s session back (to the placeholder that connects on next use), so a
  viewer stays at "1; 2 from its first Save".

## 6. What the opening keeps

- The kind, the sniffed head, the stat at open, read-only or not, and **the path of the bytes on
  screen**: for a revision or a diff the `.revisions` copy and its local connection — not `Viewer.abs`,
  the working file F4 opens (`browser.rs:1659-1664`; *trace* §1.2) (D13).
- **One stream for every view of a remote file** (D13): the opening's `Source` is kept beside the views,
  so making a view reads nothing again, as today's give-way does (`image.rs:4621-4624`). The image view
  drops its own handle once the picture is whole (`image.rs:4717-4718`); the kept one serves the others.
- A handed-over image preview has no stream (`prepare.rs:887-890`): a view that needs one opens a new
  one — one channel, and the image window holds none, so the count stays 1.

## 7. Text encodings

### 7.1 The list

| Shown as | Unit | A character | Each shown as one U+FFFD |
|---|---|---|---|
| ASCII | 1 byte | 0x00–0x7F | every byte 0x80–0xFF |
| UTF-8 | 1 byte | 1–4 bytes | each maximal bad sequence, as today (`wrap.rs:47-80`) |
| UTF-16LE, UTF-16BE | 2 bytes | a unit, or a surrogate pair | an unpaired surrogate, per unit |
| UCS-2LE, UCS-2BE | 2 bytes | a unit; no pairs | every surrogate unit (D800–DFFF) |
| UTF-32LE, UTF-32BE (UCS-4) | 4 bytes | a unit | a value above 10FFFF or a surrogate, per unit |
| Latin-1 (ISO-8859-1) | 1 byte | byte n = U+00nn | none |
| CP437 | 1 byte | 0x00–0x7F as ASCII, 0x80–0xFF by the skin's table (`chrome.rs:639-647`) | none |
| CP949 / EUC-KR | 1 byte | 1 or 2 bytes (§7.4) | as encoding_rs's EUC-KR decoder says (§7.4) |

- **Invalid input never shifts alignment**: after a U+FFFD the next unit is decoded at the next aligned
  offset; a partial last unit is one U+FFFD. Nothing counts them (D20).
- Control characters are drawn as today, C0 and DEL as their CP437 glyphs (`chrome.rs:650-655`); a C1
  control (U+0080–U+009F, which Latin-1 makes of 0x80–0x9F) as a dim `·` (D32).
- **The admission rule**: a line end is found without decoding and a character start from a bounded
  window. Every entry passes, CP949 by its sync bytes (§7.4); ISO-2022-* and UTF-7 fail (shift state from
  byte 0); Shift-JIS, Big5, GB18030 may pass `[추정: not checked]` and wait until asked.

### 7.2 The default

One function (VM-1) picks it from the file's first **8 KiB**, the kind sniff's own sample
(`SNIFF_BYTES`, `kind.rs:30`), so a file routed to text by its pattern is decoded by it (D27):

1. **A BOM**: `EF BB BF` UTF-8; `FF FE 00 00` UTF-32LE when the size is a multiple of 4, else UTF-16LE;
   `00 00 FE FF` UTF-32BE; `FF FE` UTF-16LE; `FE FF` UTF-16BE.
2. **Else the NUL pattern.** A unit is an *ASCII unit* when its value is 0x09, 0x0A, 0x0D or 0x20–0x7E
   — in UTF-16 a printable byte beside one 0x00, in UTF-32 beside three. In the order UTF-32LE,
   UTF-32BE, UTF-16LE, UTF-16BE, the first whose sample holds at least 4 whole units, at least **3/4**
   of them ASCII units, is the default.
3. **Else UTF-8**, valid or not, as today. ASCII, UCS-2, Latin-1, CP437 and CP949 are never defaults.

- **The rule over a corpus** (a python model of it, 2026-11-24; the same answers at 64 KiB): BOM-less
  English UTF-16LE/BE, UTF-32LE/BE → found; a UTF-16LE Korean-and-ASCII log (79 % ASCII units) →
  UTF-16LE; Korean UTF-16LE/UTF-32LE without a BOM → UTF-8 (F8 picks it); UTF-8, CP949, Latin-1, random
  bytes, zeros, float32, a Mach-O binary, a TrueType font, int16 0–1023, int32 0–4095 → UTF-8. **Taken
  for text**: int16 0–127, int32 0x20–0x7E — such an extension-less binary opens as text; 2 shows it.
- **The BOM is not text** (D26): line 1 starts after it, so `#!/bin/sh` after a UTF-8 BOM picks the
  syntax (*trace* §3 T14), and Go to `0x…` still counts file bytes.
- **It runs inside the text document**, so preview and viewer agree (*trace* §1.2): for a stream in the
  pass that picks the syntax (06 §7.2); for a local file before its index pass, whose marks depend on it.
- **Routing** (D18): a file whose kind the sniff decides (no known extension, `kind.rs:238-243`) is text
  when step 1 or 2 finds UTF-16 or UTF-32; `kind.rs:331`'s assertion that a UTF-16LE BOM is not text
  inverts.

### 7.3 Lines and characters over byte ranges

- **Every offset the text model makes is a multiple of the unit**, counted from byte 0 (a BOM's length is
  one). A jump — End, Go to `0x…`, `at=`, the scroll bar by bytes, the hex view's place — is aligned
  down; in UTF-16 a place on a low surrogate moves back one unit.
- **A line ends at the LF unit at an aligned offset** — `0a 00` (UTF-16LE), `00 0a` (UTF-16BE),
  `0a 00 00 00` / `00 00 00 0a` (UTF-32) — **never at a 0x0A byte inside another unit**: today's byte
  search starts UTF-16LE's next line on the `00` byte, and 44 of the 11,172 Hangul syllables carry a
  0x0A byte in UTF-16 (U+AC0A 갊 = `0a ac` LE, `ac 0a` BE: *trace* §3 T2). **A CR is the CR unit just
  before the LF unit** (UTF-16LE `\r\n` = `0d 00 0a 00`, T5). Only LF and CR LF end a line, as today. In
  UTF-8, ASCII, Latin-1, CP437 and CP949, LF and CR are the bytes, as today.
- **The index pass** (local and streamed) steps by units; its marks, `SPAN`, `CHUNK`, the 16 KiB grid,
  `LINE_MAX` and the 16 MiB cap stay byte bounds, all multiples of 4 (T16).
- **A piece that ends inside a character** (a block, a grid point, `LINE_MAX`): UTF-8 as today
  (`cut_char_start`); UTF-16 leaves a high surrogate in the last unit for the next piece, CP949 a lead in
  the last byte; the others have no multi-unit character.
- **A window's first character start**, wherever decoding begins without having decoded up to it (a
  restart near a 16 KiB multiple, `wrap.rs:596-656`; a grid point past the index; a jump): UTF-8 skips
  up to 3 continuation bytes, as today; UTF-16 skips one low surrogate; UTF-32, UCS-2 and the one-byte
  encodings start at the aligned offset; CP949 at the window's first sync byte (§7.4).
- **One decoder per encoding** gives a piece's characters and their byte lengths, in the shape of
  `wrap::Cell { ch, len }` (`wrap.rs:40-45`); the line text, the row text, the highlighter's input and the
  per-character colour count all come from it, so they cannot disagree (T10, T13).

### 7.4 CP949 / EUC-KR

- **One entry, CP949** (Microsoft's Unified Hangul Code, a superset of EUC-KR / KS X 1001), decoded by
  encoding_rs's `EUC_KR`, which is the WHATWG Encoding Standard's EUC-KR = windows-949. Checked
  2026-11-24: 17,048 two-byte characters in encoding_rs 0.8.35 and in python3's `cp949` codec alike,
  all 11,172 Hangul syllables among them; leads 0x81–0xFE (python3 maps 0x81–0xC8 and 0xCA–0xFD);
  trails 0x41–0x5A, 0x61–0x7A and 0x81–0xFE, the smallest 0x41 in both.
- **Lines**: no byte below 0x41 is a trail, so 0x0A and 0x0D are always whole characters: line starts
  found by the byte 0x0A are character boundaries, and the index pass and line search are today's.
- **Sync bytes**: 0x00–0x40, 0x5B–0x60, 0x7B–0x7F — never a trail, never a lead. After a lead,
  encoding_rs gives the lead one U+FFFD and decodes the sync byte on its own (`euc_kr.rs`: `Malformed(1, 0)`,
  the byte unread), so a character starts at every sync byte and right after it, whatever came before.
  Not 0x80 or 0xFF: after a lead, encoding_rs takes them into one two-byte U+FFFD.
- **Checked** (a throwaway program against encoding_rs 0.8.35, 2026-11-24): over 2,000 random Korean
  texts and 2,000 random byte strings, decoding in two pieces cut at a sync byte equalled decoding whole
  at all 302,874 cuts; cut at a byte ≥ 0x81 inside Korean text, it differed at 163,747 of 404,209.
- **A far jump into a long line resynchronises at the next sync byte**, a window's first character
  start. Wrap's restart rule stays (a word start after a space, else a cluster start, from 64 bytes before
  to 128 after a 16 KiB multiple: `wrap.rs:35-37, 596-656`): a space is a sync byte, so a word start is
  exact. A place past the index is reached through those restart points too `[추정: the unindexed-jump
  path was not read for this; VM-4's EA-3 decides]`.
- **The accepted residual** (D28): a window with no sync byte — 192 bytes with no space, digit,
  punctuation or line end, some 96 Hangul syllables in a row — restarts at the offset itself, as a
  one-byte encoding would. The row before it ends there, a lead left at offset − 1 as one U+FFFD, so rows
  stay a function of the bytes `[추정: the wrap invariant is asserted by EA-3, not yet run]`; up to the
  next sync byte characters may be read one byte out. In random bytes 76 of 256 values are sync bytes,
  so such a window comes about once in 10^29 (arithmetic, not measured).
- **The new dependency**: `encoding_rs = "0.8.35"`, for CP949 only (VM-4): its 17,048-pair table is data
  sopen should not write by hand, and encoding_rs is the WHATWG decoder Gecko uses (since Firefox 56, its
  README). Its one required dependency, `cfg-if`, is in `Cargo.lock` already; licence `(Apache-2.0 OR
  MIT) AND BSD-3-Clause`; a stripped release toy calling it grew by 183,104 bytes (macOS arm64). Not for
  the rest: std decodes UTF-8 and UTF-16, encoding_rs has no UTF-32 (its `lib.rs` docs), and its
  `iso-8859-1` label is windows-1252, not Latin-1 (measured with `Encoding::for_label`).

### 7.5 Showing and changing the encoding

- The text view says its encoding beside its syntax: `1,204 lines · 88 KB · Rust · UTF-16LE (BOM)` —
  `(BOM)` for step 1, `(guessed)` for step 2, nothing for step 3 or the owner's choice.
- **F8 `Encode`** (and plain `E`, as `W` is F2's) opens the list in Text and Raw (D19). It is a question
  (§4): ↑ ↓ and Enter choose; Esc leaves the encoding as it was.
- **Changing the encoding builds the index again from byte 0** and drops what depended on the old one
  (wrap's memos, the highlighter's states); the place is kept, aligned down (D22). Through a stream the
  background pass reads at most the first 16 MiB again (2–8 s on 06's rig); past them the byte-position
  path needs no index. A local file runs its whole-file pass again with progress; Esc keeps the old
  encoding. **View 3 of a local binary** uses that local pass too, with progress and Esc (D23).

## 8. Today's screen: the egui binding (provisional, after XR-T)

- **The strip** (D6), in the title row before the clock (`browser.rs:2793`, `chrome.rs:713-761`):
  `1 Image · 2 Hex`, the view on screen highlighted (a binary's `1·2 Hex` together), warn-coloured while
  F9 is armed, each entry clickable. The F-key row adds `9View` everywhere, `8Encode` in Text and Raw.
- **The keys are taken before the viewer's pass**, in the shell's input hook, where Cmd/Ctrl+digit
  already becomes F1…F10 (`mod.rs:785-796`): a digit's key event with no modifier, physical key first
  (`mod.rs:866`), is removed with the typed text that follows it (egui-winit 0.37 pushes the key, then
  its text: *trace* §2.4) when it chooses a view; F9 likewise. A row matched after the pass would type
  and switch at once in hex, which reads typed text without consuming it (`hex.rs:3650-3659`), and
  would never see the image's `1`, which the image consumes (`image.rs:3267`).
- **Fields are `text_edit_focused`**; DigitsAreData is declared by the hex window — never
  `egui_wants_keyboard_input`, true whenever the hex grid has focus (*trace* §2.4). The key table's viewer
  rows (`keys.rs:500-503`) gain 1, 2, 3 and F9, so F1 lists them (04 §3 rule 4: one table).
- **The image's 100 % key becomes `A`** (`F` fits, `A` actual pixels); on its own the image window keeps
  `1` too, having no views (D12, D24). **The memory line** reads `Continue (Enter) · Hex view (2) · Close
  (Esc)`, `H` its alias.
- **Hidden** = not drawn and `set_shown(false)`; a stream's one visibility flag (`source.rs:1411-1415`)
  means the old view is hidden before the new one is shown. The media window has no `set_shown` and plays
  on its own thread (`media.rs:1724, 2259, 2306`): its `set_shown(false)` pauses. A video none of whose
  frames decode, and empty media, give way to Hex as a remote image does today (`image.rs:4592-4633`).
- Receipt hooks: `SOPEN_STATE` `view=N` and `enc=NAME` (VM-2); `SOPEN_KEYS` `typed:1` (§12).

## 9. What a terminal binding needs from the model

All `[추정]` (terminal convention, *trace* §7), each a receipt to take before a key is fixed:
- A terminal sends the character `1` — no key event, no physical key. A view key is then the character
  1, 2 or 3 with no modifier; an AZERTY owner needs Shift, as for any 1.
- Cmd never reaches a terminal program; in the legacy encoding Ctrl+3 is ESC (it would leave the viewer)
  and Ctrl+1 can arrive as a plain `1` (it would switch the view). 04 §6.1's Cmd/Ctrl+digit aliases are
  no route there without the kitty keyboard protocol or modifyOtherKeys. Alt+digit is none either
  (Option composes `¡` on macOS; Meta sends ESC, then the digit).
- **F9-then-digit is the route**: an F-key is an escape sequence in every terminal (message 19's open
  question of a Mac's top row aside). The binding maps its input to VM-6's keys (a digit, F9, Esc, any
  other) and reports its own fields and questions; the strip is a cell row, the encoding a bar field.
- Image, Video and Page exist only where the screen can draw them (ssot item 2); elsewhere view 1 says
  why and the file opens on Hex, or on Text for HTML/HTM/XML (ssot's readings). The image's `1`
  collision exists only where an image is drawn.

## 10. Decisions

Every decision is reversible; the owner may change any of them.

| # | Decision | Why | Alternative |
|---|---|---|---|
| D1 | Views switched away from stay alive, hidden; zoom, playhead, place kept; nothing re-read | a remote image read whole on every 1↔2 costs its size each time; today's give-way reuses the stream | close the old view on every switch (loses state, re-reads) |
| D2 | Leaving a view with unsaved edits asks 07 §10's close prompt; a running Save makes the switch wait | the shipped closing rules; one view holds edits, one journal per file (07 §3) | refuse the switch until saved |
| D3 | A view key is the plain digit, by physical key, its typed text dropped, taken before the viewer's pass | one press one action; the Cmd/Ctrl+digit hook's precedent (04 §6.1); Shift+digit stays a character | match the typed character (AZERTY needs Shift) |
| D4 | Where digits are typed, F9 then 1/2/3 switches; F9-then-digit works in every view | F9 is free in every viewer (*trace* §5), types nothing, a terminal can send it, its slot is clickable | Alt+1/2/3 (GUI only); F9 cycling 1→2→3 |
| D5 | No view key while a question is up | the question keeps its keyboard | — |
| D6 | The strip is in the title row | the F-key row's digits are F-keys and Cmd/Ctrl+digits (04 §6.1) | the view's frame border |
| D7 | 3 = Text where 1 is not text (HTML/XML source, binaries); Raw where 1 is text (JSON's "raw text"); none for image, video, audio — 3 does nothing, the strip shows no 3 | ssot item 4; a JPEG as text is noise | Raw showing tabs, CRs, the BOM; pretty JSON; image metadata |
| D8 | Raw = Text with highlighting off | the smallest "raw text"; no second index | — |
| D9 | HTML/HTM/XML: 1 = Page once 09 lands | message 18; the engine is VR-W's | — |
| D10 | A video that won't decode and empty media open on Hex with the reason | "모든 파일은 헥스 모드로 선택할 수 있음"; today the first closes, the second is an error (*trace* §1.3) | keep today's close and error |
| D11 | Text, Raw and Hex share one byte place | both models are keyed by byte offset | each keeps its own |
| D12 | The image's 100 % moves from `1` to `A` (README updated); `H` stays an alias where it exists | `1` is view 1; `1` is documented today (`README.md:288`) | keep `1`; views by F9 only in the image |
| D13 | One stream per opened remote file, kept beside the views; a revision's views read its `.revisions` copy | nothing read twice (06 §7.3); `Viewer.abs` is the working file | a stream per view (+1 channel each) |
| D14 | A hidden view gives back its save connection | 06 §6: the window is at 8–9 of sshd's 10 | restate 06 §6 as "+1 per view that saved" |
| D15 | ASCII, UTF-8, UTF-16LE/BE, UCS-2LE/BE, UTF-32LE/BE (= UCS-4), Latin-1, CP437, CP949 / EUC-KR | message 20's list and its "등"; the owner is Korean; CP949 resynchronises (§7.4); Latin-1 and CP437 cost a table each | leave CP949 out |
| D16 | UCS-4 is one entry with UTF-32 | they decode alike for every value that is a character | — |
| D17 | Default: BOM, then the NUL pattern (8 KiB, ≥ 4 units, ≥ 3/4 ASCII units), then UTF-8 | a BOM is certain; the pattern finds ASCII-heavy UTF-16/32 and rejects all but two of §7.2's binaries | other thresholds; a CP949 guess (Tensions) |
| D18 | UTF-16/32 files with no known extension route to text | today they open in hex (`kind.rs:157-165, 331`) | keep hex; 3 shows them |
| D19 | F8 `Encode` (and `E`) opens the list | F8 is free in the text view; `E` mirrors `W`/F2 | another key |
| D20 | One U+FFFD per invalid unit or sequence, never counted | no extra pass | count them on the bar |
| D21 | The chosen encoding lasts for this opening only | nothing is stored about files today | remember per path in the state folder |
| D22 | Changing the encoding rebuilds the index; Esc during a local pass keeps the old | the marks depend on the LF unit | — |
| D23 | View 3 of a large local binary uses the local whole-file pass with progress and Esc | today's local route (06 Tensions); a local stream is untried (`local.rs:251-260`) | read local files through a stream |
| D24 | Views exist in the browser's viewer only; windows on their own keep their keys | message 20 names the window Enter opens | — |
| D25 | A Save or reload in one view makes the hidden ones stale, made again when shown | a hex Save reloads the shared stream; a whole image keeps its own bytes | — |
| D26 | The BOM is not text | syntax by first line; Go to counts file bytes | — |
| D27 | The pattern's sample is the kind sniff's 8 KiB | routing and decoding see the same bytes; the corpus answers alike at 64 KiB | the first 64 KiB block |
| D28 | CP949 resynchronises at sync bytes; a 192-byte window with none starts at the offset | the property held at all 302,874 cuts; the residual needs ~96 syllables with no space | decode from the line start (unbounded past the index) |
| D29 | F9 in a status-row field closes the field and arms (amends 04 §3 rule 4, for F9 only) | F9 types nothing, so it steals no typing; the field is the view's own | Esc first, then F9 |
| D30 | HTML/HTM/XML before 09: 1 Text, 3 Raw | one rule for every text file until Page exists | 1 and 3 the same view |
| D31 | The model is screen-free modules outside `src/gui/` | a terminal binding and the GUI share them | inside each window |
| D32 | A C1 control is drawn as a dim `·` | Latin-1 makes 32 of them; today they reach the screen glyph-less | U+FFFD (hides that they are valid) |
| D33 | An armed F9 lasts one key, no timeout | a timeout is a race with a slow hand | 2 s |
| D34 | The bar's encoding label and `SOPEN_STATE enc=` ship with VM-2, before XR-T | the only way to see the default pick and take receipts; one bar field, the same on any screen | wait for the binding |

## 11. Acceptance

Model tests run with no window, binding tests through the test kit; every key test sends the key event
**and** its typed text in one frame (§12).

**Encodings** (the text document, locally and through the stream Fake, each on both sides of the 16 MiB
index cap; an encoding is set by `enc=` or the document's own call until VM-7 brings F8)
- EA-1 (VM-2). A UTF-16LE file with a BOM, 3,000 CRLF lines of Hangul and ASCII, every 7th holding U+AC0A 갊
  (`0a ac`) → 3,000 lines; every line starts at an even offset; 갊 is inside its line, no break; no line
  shows a CR or a NUL; the bar says `UTF-16LE (BOM)`. The same text in UTF-16BE (`ac 0a`), UTF-32LE and
  UTF-32BE → the same lines.
- EA-2 (VM-2). **The 24 GB jump**: the Fake's 24,000,000,000-byte file (`FakeFile::pattern`, the stream
  tests' `MODEL`) as a text document set to UTF-32LE (`enc=`) → Go to `0x300000003` → the row holding
  `0x300000000` is at the top, every row starts at a multiple of 4, its text equals the core's UTF-32LE
  decode of the pattern's bytes; `fetched` grows by what a UTF-8 jump to that offset fetches, no more.
  VM-7's variant reaches it through 3 and F8.
- EA-3 (VM-4). **CP949**: Korean text encoded by encoding_rs (갊 = `b0 a7`, CRLF), opened with no
  `enc=` → UTF-8, bar `UTF-8`, U+FFFD shown; set to CP949 / EUC-KR → the text equals encoding_rs's
  decode of the whole file, the line count unchanged. *Resync*: past 16 MiB, a 64 KiB line of Hangul
  with spaces, Go to its middle → the rows equal that line decoded from its start, from the first sync
  byte on. *Residual*: a 400-byte run with no sync byte across a 16 KiB multiple → rows reached from
  above and from below are the same.
- EA-4 (VM-1, VM-4). Invalid input: a UTF-16 unpaired surrogate, a UCS-2 surrogate pair (two U+FFFD), a UTF-32 value
  above 10FFFF, an odd trailing byte, ASCII 0x80, a CP949 lead before `A` (U+FFFD, then `A`) → as §7.1
  says, the next character in place; Latin-1 and CP437 over all 256 bytes.
- EA-5 (VM-1, VM-3). Defaults over §7.2's corpus → §7.2's answers; `FF FE 00 00` at a size not a
  multiple of 4 → UTF-16LE; an extension-less UTF-16 file → text. A UTF-8 BOM before `#!/bin/sh` →
  shell highlighting, no U+FEFF on line 1. The preview pane and the viewer decode a UTF-16 file alike.
- EA-6 (VM-2). A change of encoding keeps the place, aligned; Esc during a local pass keeps the old one.

**Views** (VM-5 and VM-6 as tables of state × input; every VA item end to end with VM-7)
- VA-1. Each row of §3, locally and through the Fake: Enter shows 1; 2 shows Hex; 3 shows the third view,
  or does nothing and the strip has no 3.
- VA-2. **A hex edit, then a switch**: in an editable hex view `4`, `1` in the hex column (key + text)
  type 0x41, no switch; F9 then `1` → the close prompt; Cancel → Hex with the edit; F9 `1`, Discard →
  view 1, the journal gone; F9 `1`, Save → view 1 once the Save lands, the file holding 0x41. During a
  Save, F9 `1` → `saving…`, then view 1. An image crop, then plain `2` → the same prompt.
- VA-3. F9 then `x` in hex → nothing typed, no switch. Go to open: `1` goes into the field, F9 closes it
  and arms. The close prompt, Save As or the encoding list up: 1, 2, 3 and F9 do nothing. Cmd/Ctrl+1,
  +2, +3 stay F1, F2, F3 and switch nothing. `2` and Esc in one frame → the viewer leaves, no view made.
- VA-4. Image zoom and rectangle survive 1→2→1; the media playhead survives and nothing plays while
  hidden; Text→Hex puts the cursor on the top row's first byte; Hex→Text opens at the cursor's line.
- VA-5. Remote: 1→2→1→2 on an image reads the file once (`SOPEN_STREAM_STATS` `fetched=`); a handed-over
  image preview's 2 opens one stream; channels never pass 2. A revision's 2 shows its bytes, read-only.
- VA-6. A video none of whose frames decode, and empty media → Hex with the reason. F1 lists 1, 2, 3 and
  F9; `keys.rs`'s `no_duplicate_bindings` passes.

**Live receipts** (AGENTS.md: screenshots someone looked at; remote ones under `/tmp/sopen-receipts/`)
- LR-1. Each §3 kind in views 1, 2 and 3, locally and remotely, through `SOPEN_STATE` `view=N`; a remote
  UTF-16 log with a BOM → text from block 0 (`first` on the stats line); a remote binary of several GB →
  3 shows text from block 0, no whole-file read.
- LR-2. The owner at the Mac: 1, 2, 3 on the US layout and with the Korean input source on; F9 then 1 in
  a hex view; the same on the Linux laptop under KDE.

## 12. Test plan

- **The model units** are plain unit tests: per encoding the split property (two pieces cut at a
  character start the core reports decode as the whole does), LF search over every Hangul syllable in
  UTF-16LE/BE and UTF-32, §7.1's invalid inputs, §7.2's corpus, CP949's sync-byte property as §7.4
  measured it; the view set and the key rules as tables of state × input → command.
- **The corpus is generated by the tests**, no binary fixtures: §7.2's texts, arrays and noise, and for a
  real binary the first 8 KiB of the test's own executable.
- **The text document's tests** run locally and through the Fake on both sides of the 16 MiB cap, with
  the wrap invariant per encoding: rows reached from any restart point equal those from the line start.
- **Key tests send what the platform sends.** `testkit::Driver::key` sends only `Event::Key`, with
  `physical_key: None` (`testkit.rs:240-252`), and an `SOPEN_KEYS` step is a key or a text, never both
  (`browser.rs:2167-2180`): a test built on either passes while the real keyboard types and switches at
  once in hex (*trace* §2.3 item 1). The egui binding adds `Driver::press` (the key with its physical
  key, then its typed text, in one frame, as egui-winit 0.37 does) for every digit and letter, and the
  `SOPEN_KEYS` step `typed:1`. A terminal binding's tests send the character alone, as a terminal does.
- **The gate** is the loop's: `just check` with `SOPEN_REQUIRE_FFMPEG=1 SOPEN_REQUIRE_SFTP_SERVER=1` and
  the Linux gate's two clippy runs; a unit that changes a screen brings a screenshot someone looked at.

## 13. Work units

Model units are ready to build; screen units wait for XR-T. Files are disjoint except the `mod` lines in
`src/lib.rs`, where whichever unit merges second rebases (06 §13's rule).

| ID | Unit | Owns | After | When |
|---|---|---|---|---|
| VM-1 | **Encoding core**: §7.1's list, §7.2's pick, one decoder per encoding (characters and byte lengths), LF/CR unit search, a window's first character start, the piece-cut rule, place alignment — with a slot CP949 fills (no 1-unit look-back assumed) | `src/encoding.rs`, `src/encoding/tests.rs` (new); `src/lib.rs` (`mod` line) | — | ready |
| VM-2 | **Text document per encoding**: index pass, line reads, spans, look-back, grid pieces, restarts and jumps through VM-1; the BOM not text; a change of encoding rebuilds the index; the bar's encoding and `SOPEN_STATE enc=` (D34) | `src/gui/text.rs`, `src/gui/text/wrap.rs`, `src/gui/text/tests.rs`, `src/gui/text/stream_tests.rs`; `src/gui/mod.rs` (`State`'s `enc` only); `src/gui/browser.rs` (handing `enc` on, as `at`) | VM-1 | ready |
| VM-3 | **Kind sniff**: UTF-16/32 → text (D18) | `src/kind.rs` | VM-2 (before it, such a file would open misaligned) | ready |
| VM-4 | **CP949 / EUC-KR**: its decoder and sync-byte rule; `encoding_rs`; the binary-size measure | `src/encoding/cp949.rs`, `src/encoding/cp949_tests.rs` (new); `src/encoding.rs` (the entry and `mod` line only); `Cargo.toml`, `Cargo.lock` | VM-1, VM-2 | ready |
| VM-5 | **The view set**: §3's table, §5's switch rules, §6's kept means, screen-free | `src/views.rs`, `src/views/tests.rs` (new); `src/lib.rs` (`mod` line) | — | ready |
| VM-6 | **The view keys**: §4 as a function of (state, armed, input) → command | `src/view_keys.rs`, `src/view_keys/tests.rs` (new); `src/lib.rs` (`mod` line) | — | ready |
| VM-7 | **egui binding** (§8), if XR-T keeps egui windows | `src/gui/browser.rs`, `src/gui/browser/prepare.rs`, `src/gui/browser/keys.rs`, `src/gui/mod.rs` (the input hook, `view=`), `src/gui/chrome.rs`, `src/gui/testkit.rs`, a hook each in the hex, image, media and text windows; README, AGENTS.md | VM-2…VM-6, XR-T | after XR-T |
| VM-8 | **TUI binding** (§9) | per XR-T | VM-5, VM-6, XR-T | after XR-T |
| VM-9 | **Page view**, 1 for HTML/HTM/XML | per 09 | VM-5, 09, XR-T | after XR-T |
| VM-D0 | 04 §2 and K-R1 brought to the shipped code (§14) | `.prd/04-file-keys.md` | — | ready |

- **Order**: VM-1 → VM-2 → VM-3 and VM-4 side by side (disjoint); VM-5, VM-6 and VM-D0 from the start;
  the bindings after XR-T. Each unit's PR amends the sentences it makes untrue (§14), as 06 §14 did.
- **Why VM-2 carries a screen piece before XR-T** (D34): without the bar's label and `enc=` the model is
  reachable only by tests — the default pick cannot be seen and no receipt can be taken. Both are one
  field and one key of state, which any screen keeps; the F8 list, the strip and the keys wait.
- VM-2's one line in each of `src/gui/mod.rs` and `src/gui/browser.rs`, and VM-7's larger share, meet
  the browser workstream's input-hook round, the web view and the F4 editor there: sequenced; a
  conflict goes to the dispatcher. The text document stays in `src/gui/text.rs` until XR-T moves it
  (D31 covers the new modules).

## 14. Text this amends

- **04 §2, now** (VM-D0; the code is the shipped contract, 07 §14 ED3): "In a viewer the slots keep the
  viewer's meanings — 3 Exit, 4 Edit, and the hex window's 5 Save, 6 Goto, 8 Asc/hx, the image window's
  5 Save — 1 is Help everywhere, and 2, 7, 9, 10 are empty" becomes "… 3 Exit, 4 Edit; the hex
  window's 5 Save, 6 Goto, 7 SaveAs, 8 Asc/hx; the image and media windows' 5 Save, 7 SaveAs; the text
  viewer's 2 Wrap (Unwrap while wrapped). 1 is Help everywhere; a slot not named is empty"
  (`chrome.rs:140-202`; *trace* §5). With VM-7: 9 View in every view, 8 Encode in Text and Raw.
- **04 K-R1, now**: the hex row reads `1Help 3Exit 4Edit 5Save 6Goto 7SaveAs 8Asc/hx` (`9View` with VM-7).
- With their units: 06 §7.2 per encoding, AGENTS.md `enc=` (VM-2); 01 §3, a UTF-16/32 file is text
  (VM-3); 01 §3a's views and D10, 04 §3 rule 4 (D29), 06 §6 (D14), 06 §7.3 (Hex view is 2),
  `README.md:288` (`A` is 100 %), AGENTS.md `view=` and `typed:` (VM-7).

## Tensions

- **"Raw text" for JSON.** D7/D8 make 3 the unhighlighted text, since JSON's 1 is already text; a
  pretty-printed or tree view as JSON's 1 would make "raw" mean "as stored". The owner's call.
- **CP949 is never the default** (D17): a CP949 file opens as UTF-8 full of U+FFFD until F8. A step
  "invalid UTF-8 that decodes as CP949 with no error" would pick it, at the risk of guessing on Latin-1
  files `[추정: not measured]`.
- **F9 and message 19.** A Mac's top row sends media keys unless fn is held; message 19's outcome decides
  whether F9 is reachable without fn, in the GUI and in a terminal.
- **View 3 of a 24 GB local binary** reads the whole file before its first screen (D23), about 10 s at
  2–3 GB/s `[추정]`; a local stream would not, but is untried.
