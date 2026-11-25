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
into the contract. Code citations are at 767a8a8, the trace's commit. Since then only fd26122 (the name
xtools) has touched `src/`: `chrome.rs` past line 225 moves down one line. Decisions keep the draft's numbers (D1–D26); D27 on are new. `[추정]` marks what was
inferred, not read in code or measured. What it amends in 01, 04, 06, 07, the README and AGENTS.md is §14.

## 1. The contract

- **Enter (or F3) opens a file's default view, view 1.** In the viewer **1** is the default view, **2**
  the hex view, **3** the other view. Every file has a hex view.
- **The hex view opens viewing** in the browser's viewer, where 1, 2, 3 are view keys too; Enter or
  Insert starts editing it. Where digits are typed (a hex view being edited, a status-row field),
  **F9 then 1, 2 or 3** chooses; F9-then-digit works in every view, so it is the route a terminal can
  always send.
- **A view switched away from stays alive, hidden**, and comes back as it was; nothing is read again.
  Leaving a view with unsaved edits asks 07 §10's close prompt; a running Save makes the switch wait.
- **The text views decode** ASCII, UTF-8, UTF-16LE/BE, UCS-2LE/BE, UTF-32LE/BE (= UCS-4), Latin-1,
  CP437 and CP949 / EUC-KR. The default comes from the file's first bytes; F8 (or `E`) opens the list.
- **Every rule of 06 §7.2 holds in every encoding** (the first screen from one block, the 16 MiB index,
  jumps by byte position past it). A line ends at the encoding's LF unit, never at a `0x0A` byte inside
  another character, and a ranged view never shows a character the whole-file decode does not have.
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
| an image that does not decode, a video none of whose frames decode, empty media (0 bytes) | opens on 2, its first row the reason; 1 says the reason on the status row and stays on 2 (D10) | Hex | none |
| text by extension or sniff (source, JSON, YAML, CSV, SVG, …); a UTF-16/32 file with no known extension (D18, §7.2 steps 1–2) | Text, highlighted by syntax | Hex | Raw |
| HTML, HTM; XML whose first 8 KiB holds `<?xml-stylesheet` before the first element (09 §5) | Page (09) | Hex | Text, the source highlighted |
| other XML, RSS among it | Text, the source highlighted | Hex | Page (09) |
| any other file (binary) | Hex (the same view as 2) | Hex | Text, decoded by §7.2's default |
| an empty file that is not media | as its kind | Hex | as its kind |
| a file at a revision, a diff (03 §6.2) | as its kind, read-only | Hex, read-only | as its kind, read-only |
| a `.ts` | as its refined kind (`kind::classify`, `kind::refine_after_fetch`) | Hex | as that kind |
| a folder, fifo, socket, device, dangling link, a name that is not UTF-8 | not opened, as today (`browser.rs:1780-1783, 1824-1831`) | — | — |

- **none**: the key does nothing and the strip shows no 3 (D7).
- **HTML, HTM and XML follow 09** (D40); until 09's page view lands, all of them are 1 Text, 2 Hex,
  3 Raw (D30).
- **Raw is the Text view with highlighting off** (D8): one document, index, encoding and place. JSON's
  "raw text" in message 20 is Raw.
- The image's memory line (06 §7.3) is view 1's first state and is **Free** (§4): 2 is its Hex view, with
  `H` kept as an alias (D12); its own keys are Enter and Esc.

## 4. The view keys

- **1, 2, 3** choose the view in that slot; the view on screen chosen again does nothing.
- **A view key is the digit pressed with no modifier** (D3). Shift, Alt and Cmd/Ctrl with a digit are not
  view keys; Cmd/Ctrl+1…0 stay F1…F10 (04 §6.1). One press, one action: a press that chooses a view is
  not also typed, and a press that is typed never chooses a view.
- **Each view declares one state**, as of its last pass:

| State | When | A plain digit | F9 |
|---|---|---|---|
| Free | any view in no other state — a hex view while viewing among them | chooses the view | arms |
| DigitsAreData | a hex view while editing (D35), also while a reload or a Save holds typing back (`hex.rs:3664-3671`) | is typed as a nibble or an ASCII byte (`hex.rs:1002-1025`); held back, it says why — as today | ends editing and arms |
| Field | a status-row field has the typing: Go to (hex, text), the hex window's read-only path field | goes into the field (04 §3 rule 4) | closes the field, as Esc would, and arms (D29) |
| Question | the close prompt, Save As, a confirmation, the encoding list | belongs to the question | does nothing |

- **The hex view in the browser's viewer opens viewing** (D35). Today the grid always holds the focus
  (`hex.rs:4013-4018`) and every typed character goes to it (`hex.rs:3650-3660` → `type_char`,
  `1002-1018`), so 1→2→1 on an image would write a byte. Viewing, typed text is not read. **Enter or
  Insert** starts editing (not F4, which is VS Code / Zed: `keys.rs:503`); the status row says
  `editing — Esc ends`. **Esc** stops a running Save first, as today (`hex.rs:3549-3554`), else ends
  editing, and a second Esc leaves; **F9** ends editing and arms. Edits stay unsaved until saved in
  either mode. A read-only hex view (a revision, `hex.rs:2270-2275`) never edits: Enter says so. A hex
  window on its own (`--hex`, the binary dialog) keeps today's typing.
- **Armed** (D4): the next key decides — 1, 2 or 3 chooses that view; Esc or F9 disarms; any other key
  disarms and is dropped, never typed. No timeout (D33).
- **Leave wins**: a view key and Esc or F3 in the same input leave the viewer; no view is made (the race
  PR #61 fixed for the image's give-way: *trace* §1.3).

## 5. A switch: what it keeps

- **Clean views are kept, hidden** (D1): zoom and rectangle (image); playhead, In/Out and selection
  (media); wrap (text); the typing column (hex, shown again viewing). A hidden view draws nothing, takes
  no input, reads nothing for a screen, polls nothing and plays nothing.
- **A view is made the first time it is chosen**, from what the opening kept (§6), on a worker, with
  progress and Esc on the status row; Esc stays on the view on screen.
- **Unsaved edits** (D2): choosing another view while the view on screen holds them asks 07 §10's
  `Save the changes to host:/abs before closing?` → **Save** · **Save As…** · **Discard** · **Cancel**.
  A Save or Save As that lands, or Discard, closes that view and shows the chosen one; Cancel stays; a
  running Save makes the switch wait (`saving…`). So only the view on screen holds unsaved edits, hidden
  views show the file as stored, and a file has one journal at a time (07 §3; *trace* §4).
- **The place follows the owner between byte views** (D11). Text → Hex: the cursor on the first byte of
  the top row, that row at the top. Hex → Text: the text view at the cursor's byte, by §7.3's jump rule,
  as `at=` opens it (`embed_scripted`'s `Jump::Byte`). Text ↔ Raw: one view state. From Image,
  Video, Audio or Page (no byte place) a byte view keeps its own last place, byte 0 the first time.
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
| UTF-8 | 1 byte | 1–4 bytes | each maximal bad sequence, as before VM-2 (`Encoding::decode`) |
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

One function picks it from the file's first **8 KiB**, the kind sniff's own sample (`SNIFF_BYTES`,
`kind.rs:30`), so a file routed to text by its pattern is decoded by it (D27). VM-1 builds steps 1, 2
and 4 with step 3's slot empty; VM-4 filled step 3 (`src/encoding/cp949.rs`).

1. **A BOM, the longest first** (D37): `FF FE 00 00` UTF-32LE, `00 00 FE FF` UTF-32BE, `EF BB BF`
   UTF-8, `FF FE` UTF-16LE, `FE FF` UTF-16BE — whatever the file's size. A partial last unit is one
   U+FFFD: `FF FE 00 00 41 00 00 00 42` is UTF-32LE `A` and one U+FFFD.
2. **Else the NUL pattern.** A unit is an *ASCII unit* when its value is 0x09, 0x0A, 0x0D or 0x20–0x7E
   — in UTF-16 a printable byte beside one 0x00, in UTF-32 beside three. In the order UTF-32LE,
   UTF-32BE, UTF-16LE, UTF-16BE, the first whose sample holds at least 4 whole units, at least **3/4**
   of them ASCII units, is the default.
3. **Else CP949**, when all of these hold (D39): (a) the sample is not valid UTF-8, with `kind::utf8_text`'s
   allowance (a character cut by the sample's end is no error when the file goes on); (b) it decodes as
   CP949 with no error (a lead as its last byte is none either when the file goes on); (c) it holds at
   least 4 Hangul syllables (U+AC00–U+D7A3); (d) they are at least **3/4** of its decoded non-ASCII
   characters; (e) at least **3/4** of them are KS X 1001's 2,350 (both bytes ≥ 0xA1). It is a
   Korean-first prior, shown `(guessed)` (D39, Tensions).
4. **Else UTF-8**, valid or not, as today. ASCII, UCS-2, Latin-1 and CP437 are never defaults.

- **Steps 1–2 over a corpus** (a python model, 2026-11-24; the same answers at 64 KiB): BOM-less English
  UTF-16LE/BE, UTF-32LE/BE → found; a UTF-16LE Korean-and-ASCII log (79 % ASCII units) → UTF-16LE;
  Korean UTF-16LE/UTF-32LE without a BOM, UTF-8, CP949, Latin-1, random bytes, zeros, float32, a Mach-O
  binary, a TrueType font, int16 0–1023, int32 0–4095 → no pattern. **Taken for text**: int16 0–127,
  int32 0x20–0x7E — such an extension-less binary opens as text; 2 shows it.
- **Step 3 over a corpus** (python3's `cp949` codec, which agrees with encoding_rs on all 17,048 pairs;
  each sample cut at four different 8 KiB boundaries, the same answer each time): → CP949: Korean prose in
  CP949 and in EUC-KR, C source with Korean comments, Korean news with some hanja (92 % Hangul). → not:
  Korean with a hanja word each (47 % Hangul), a file with one syllable, UHC-only syllables (25 % KS X
  1001), Korean UTF-8 (valid), BOM-less Korean UTF-16LE, GBK, GB2312, Big5, EUC-JP, Shift-JIS with kanji,
  Latin-1, Windows-1252, random bytes, a Mach-O binary, a TrueType font (all CP949 errors), hiragana-only
  EUC-JP (no syllable). **Near misses**: hiragana-only Shift-JIS decodes as 4,048 UHC syllables with no
  error and is stopped by (e) alone (0 of 4,048 in KS X 1001); a GB2312 text whose every character
  decodes in CP949 is 66–68 % Hangul, stopped by (d) alone.
- **Step 3 over public-domain excerpts** (VM-4's tests, encoded with encoding_rs; the first 8 KiB
  as CP949 decodes it — CP949 errors, and the Hangul share of its non-ASCII characters):

  | Sample | Errors (b) | Hangul (d) | Answer |
  |---|---|---|---|
  | 기미독립선언서, today's Korean (CP949) | 0 | 100 % | CP949 |
  | 훈민정음 서문, today's Korean (CP949) | 0 | 100 % | CP949 |
  | 대한민국 헌법 전문 (CP949) | 0 | 98 % | CP949 |
  | 기미독립선언서, the 1919 original, thick with hanja (CP949) | 0 | 39 % | not: (d) |
  | 훈민정음 서문, 한문 (CP949) | 0 | 0 % | not: (c), (d) |
  | 道德經 GBK / Big5 | 108 / 88 | 42 / 54 % | not: (b), (d) |
  | 道德经, simplified, GBK | 36 | 38 % | not: (b), (d) |
  | 論語 GBK / Big5 | 60 / 96 | 44 / 54 % | not: (b), (d) |
  | 出師表 GBK / Big5 | 78 / 156 | 36 / 51 % | not: (b), (d) |
  | Chinese subtitles (.srt), GBK | 0 | 42 % | not: (d) |
  | A list of long Korean names, one a line, no space (CP949) | 0 | 100 % | CP949 |
- **A BOM is skipped only under its own encoding** (D26, D37): `EF BB BF` under UTF-8; `FF FE` / `FE FF`
  under UTF-16 or UCS-2 of that order; `FF FE 00 00` / `00 00 FE FF` under UTF-32 of that order (so a file starting `FF FE 00 00`, picked as UTF-32LE, switched by F8 to UTF-16LE or UCS-2LE has its `FF FE` skipped as that encoding's own BOM and `00 00` is its first unit). Line 1
  then starts after it, so `#!/bin/sh` after a UTF-8 BOM picks the syntax (*trace* §3 T14). Under any
  other encoding its bytes are text from byte 0 (Latin-1 shows `EF BB BF` as `ï»¿`); switching back
  skips it again. Go to `0x…` counts file bytes either way. A place inside a skipped BOM is line 1's start:
  `span_at` would fall back to the last line it read (`.or(spans.last())`), so since VM-2 it looks
  for a place no earlier than line 1's start (`TextDoc::span_at`).
- **The label**: `(BOM)` while a BOM is skipped; `(guessed)` while the encoding on screen is the one step
  2 or 3 picked, also after switching back to it; nothing otherwise.
- **It runs inside the text document**, so preview and viewer agree (*trace* §1.2): in the index
  pass, on the first bytes it is fed — a stream's first block, in the pass that picks the syntax (06
  §7.2); a local file's first read — before any mark, since the marks depend on it (`Scan::feed`).
- **Routing** (D18): a file whose kind the sniff decides (no known extension, `kind::detect_sniffed`) is
  text when step 1 or 2 finds UTF-16 or UTF-32 — `kind::looks_like_text` asks `encoding::pick` itself
  (VM-3), so a UTF-16LE BOM is text where it was not. Step 2's guess is not taken for a file that
  starts with a binary format's signature (`kind::signed_binary`: RIFF, FLAC, Ogg, PNG, GIF, JPEG,
  PDF, ZIP, SQLite, ELF, Mach-O, fonts, `.DS_Store`, OLE, a registry hive, `ftyp` at 4; `MZ` and
  `ID3`, which text can start with, only with a PE header or the DOS stub, and an ID3v2 version): a
  canonical WAV of quiet 16-bit samples is all ASCII units. A BOM decides before any signature (D37). Step 3
  does not route: an extension-less CP949 file fails the sniff's UTF-8 test (`kind::utf8_text`),
  stays Other and opens in Hex; its view 3 decodes it as CP949 by step 3.

### 7.3 Lines and characters over byte ranges

- **Every offset the text model makes is a multiple of the unit**, counted from byte 0 (a skipped BOM's
  length is one).
- **A line ends at the LF unit at an aligned offset** — `0a 00` (UTF-16LE), `00 0a` (UTF-16BE),
  `0a 00 00 00` / `00 00 00 0a` (UTF-32) — **never at a 0x0A byte inside another unit**: the byte
  search before VM-2 started UTF-16LE's next line on the `00` byte, and 44 of the 11,172 Hangul
  syllables carry a 0x0A byte in UTF-16 (U+AC0A 갊 = `0a ac` LE, `ac 0a` BE: *trace* §3 T2). **A CR
  is the CR unit just before the LF unit** (UTF-16LE `\r\n` = `0d 00 0a 00`, T5). Only LF and CR LF
  end a line, as today. In UTF-8, ASCII, Latin-1, CP437 and CP949, LF and CR are the bytes, as
  today.
- **The index pass** (local and streamed) steps by units; its marks, `SPAN`, `CHUNK`, the 16 KiB grid,
  `LINE_MAX` and the 16 MiB cap stay byte bounds, all multiples of 4 (T16). VM-2 turned every
  hard-coded one-byte newline into the encoding's unit — the `b'\n'` / `b'\r'` tests of the index
  pass (`Scan::feed`, which finishes a unit a read cut in two), the line reads, the spans, the
  look-back for a line start and a line's shown text, the one-byte newline lengths, and in `wrap.rs`
  the span's unread CR tested and popped as a byte (`Wrapper::cut`, `ended`, `end_left`, `flat`) —
  through `Encoding::find_lf`, `rfind_lf`, `ends_with_cr` and `unit`; a read the line searches make
  ends on a unit boundary (`Window_::units`), so an LF unit is in it whole or not at all.
- **Four rules place a character start**, each one function per encoding in VM-1:
  1. **A grid cut** (D36) — a piece boundary at grid point g (a block edge, a GRID point of a line whose
     start is not known, `LINE_MAX`) — is one cut, used by the piece before and the piece after, in
     [g − 3, g]. UTF-8: `cut_char_start` over the bytes before g, as before VM-2 (it looks back only;
     `TextDoc::grid_cut` hands VM-1's rule the look-back). UTF-16: g − 2 when the unit at g − 2 is a
     high surrogate, else g. UTF-32, UCS-2 and the one-byte encodings: g. CP949: §7.4. VM-1 says how
     many bytes before g each rule reads (3, 2, 0; CP949's W = 256), so VM-2 supplies them without
     knowing the encoding.
  2. **A restart's start** — wrap's restart near a 16 KiB multiple only (`Wrapper::restart`), forward in
     its window: UTF-8 skips up to 3 continuation bytes, as before VM-2 (`Wrapper::restart`); UTF-16
     takes the window's first unit, and moves past it only when it is a low surrogate whose preceding
     unit is a high surrogate (D38); UTF-32, UCS-2 and the one-byte encodings take the first aligned
     offset; CP949 the window's first sync byte, or the exact start a known phase gives (§7.4).
  3. **A jump's place** — End, Go to `0x…`, `at=`, the scroll bar by bytes, the hex view's place — is
     aligned down to the unit. In UTF-16 it moves back one unit only across a valid pair: a low
     surrogate whose preceding unit is a high surrogate. A standalone low surrogate stays and shows as
     U+FFFD, as the whole-file decode shows it (D38). **The top row of a jump** is the row holding that
     place ("the row that byte is in at the top", AGENTS.md's `at=`). Rule 3 picks which row; the row's
     own start comes from the line start, a grid cut (rule 1) or a restart (rule 2), never from the jump.
  4. **A place inside a line, not wrapped** — the byte `left` that every row is shown from (Right/Left
     by `SIDEWAYS`, End's `end_left`, the sideways wheel: `TextWindow::keys`, `Wrapper::end_left`,
     `TextWindow::body`) — is aligned down to the unit, then moved to the first character start at or
     after it: UTF-8 skips up to 3 continuation bytes, as before VM-2 (`Wrapper::start_at`); UTF-16
     moves past a low surrogate whose preceding unit is a high surrogate; CP949 decodes forward from
     §7.4's proof before `left` (the line start within REACH, else a checkpoint, else the last sync
     byte within W), else shows the placeholders.
- **One decoder per encoding** gives a piece's characters and their byte lengths, in the shape of
  `encoding::Cell { ch, len }` (`wrap.rs`'s `Cell` since VM-2); the line text, the row text, the
  highlighter's input and the per-character colour count all come from it, so they cannot disagree
  (T10, T13).

### 7.4 CP949 / EUC-KR

- **One entry, CP949** (Microsoft's Unified Hangul Code, a superset of EUC-KR / KS X 1001), decoded by
  encoding_rs's `EUC_KR`, which is the WHATWG Encoding Standard's EUC-KR = windows-949. Checked
  2026-11-24: 17,048 two-byte characters in encoding_rs 0.8.35 and in python3's `cp949` codec alike,
  all 11,172 Hangul syllables among them; leads 0x81–0xFE (python3 maps 0x81–0xC8 and 0xCA–0xFD);
  trails 0x41–0x5A, 0x61–0x7A and 0x81–0xFE, the smallest 0x41 in both. A lead 0x81–0xC6 takes a letter
  as its trail (`B0 41` = 캚, the UHC extension); a lead from 0xC7 up does not (`D0 41` = U+FFFD, `A`).
- **Lines**: no byte below 0x41 is a trail, so 0x0A and 0x0D are always whole characters: line starts
  found by the byte 0x0A are character boundaries, and the index pass and line search are today's.
- **Sync bytes**: 0x00–0x40, 0x5B–0x60, 0x7B–0x7F — never a trail, never a lead. After a lead,
  encoding_rs gives the lead one U+FFFD and decodes the sync byte on its own (`euc_kr.rs`: `Malformed(1, 0)`,
  the byte unread), so a character starts at every sync byte and right after it, whatever came before.
  Not 0x80 or 0xFF: after a lead, encoding_rs takes them into one two-byte U+FFFD.
- **Checked** (a throwaway program against encoding_rs 0.8.35, 2026-11-24): over 2,000 random Korean
  texts and 2,000 random byte strings, decoding in two pieces cut at a sync byte equalled decoding whole
  at all 302,874 cuts; cut at a byte ≥ 0x81 inside Korean text, it differed at 163,747 of 404,209.
- **Never out of phase** (D28). A character start is taken only from what proves it, in this order:
  1. a known line start within REACH (64 KiB, `text.rs`'s `REACH`) before the place — the index's, or an LF
     found that far back;
  2. a **phase checkpoint** of a line longer than REACH: one bit per GRID point, whether a lead is
     pending there;
  3. the last sync byte within W = 256 bytes before the place;
  and decoding runs forward from it. Each gives the same characters from a sync byte on, so VM-4
  decodes from the nearest: the sync byte within W when there is one, else the line start, else the
  checkpoint. (The phase is a parity: a lead is pending exactly after an odd run of 0x81–0xFE,
  `cp949_tests.rs` holding it to encoding_rs's own state. So after any byte outside 0x81–0xFE a
  character starts, whatever came before; the rules do not take such a byte as a proof — only the
  sync bytes, at which a character also starts.) With none of
  these — past 16 MiB with no sync byte inside W, or a checkpoint not made yet — the bytes up to the
  first sync byte show as dim `░` placeholder marks, one per byte, never as characters. The status row says `CP949: no character boundary known here — shown
  from the next space or punctuation`.
- **Checkpoints are made lazily** (D22, D28). Switching into CP949 from UTF-8, ASCII, Latin-1 or CP437,
  by F8 or by step 3, rebuilds no index: CP949's LF byte is theirs (from UTF-16 or UTF-32 the LF unit
  changes and D22's rebuild runs). The first time a place past REACH in a line longer than REACH is
  needed, that line is read once from its known start up to the place (a Bulk read, like the index
  pass's), and every GRID point passed keeps its bit, so no stretch is read twice. The cost is at most
  the line's bytes before the place, inside the indexed prefix: ≤ 16 MiB through a stream (all 16 MiB
  take 2–8 s on 06's rig), fewer when the block cache still holds them `[추정]`; a local file is read in
  place. A line shorter than REACH needs none, since its start is within REACH. Until the read ends,
  rule 3 or the placeholders apply. VM-4 keeps the bits per line start (`text.rs`'s `Phases`: past
  64 lines the one asked for longest ago goes, never one the page on screen asked for, so a page of
  more long lines than that settles), reads them on a worker through a handle of the document's own
  (`PhaseRead`; a stream's waits for its bytes), never from a preview, and drops them — the read under
  way stopped at once — with the document (a re-read, a reload, another index) and on any change of
  encoding (they are CP949's: a read under another encoding makes none); `ready` waits for a read the
  page asked for, so a screenshot shows its rows, and the page is read again from the same place once
  the bits are in.
- **The grid cut** (§7.3 rule 1): decode forward from the line start within REACH, else the checkpoint
  at g, else the last sync byte in [g − W, g); the cut is g − 1 when a lead is pending there, else g. With neither, the placeholders
  above. "Within REACH" is a line start less than REACH before g — the bound a jump's look-back
  (`piece_holding`) has, so the cut at g is the same whichever way g is reached (D36); only a line's
  own first piece is anchored at its start, never a piece from the grid. Grid cuts are of pieces past
  the lines known, so the checkpoint there is the index's open line's (its start is the index's),
  inside the indexed prefix, read to the grid point that ends the piece holding the place (at most
  GRID past it); past the prefix no line start is known. A piece that ends at a grid point
  nothing proves stops before a lead pending there (decoded from the piece's own proof): the piece
  after begins blind, so neither shows that character, never as a U+FFFD the whole decode lacks.
  A piece that begins on a grid point finds its cut there as the piece before got it — from g alone,
  else from its line start less than REACH back, found again by the LF before it — so the piece after
  a cut the line start proves is proven too, not blind (`TextDoc::cut_at`). A line that starts
  exactly on a grid point is REACH from its first piece's end, so that first cut never has the
  line-start proof (the strict "less than"): a sync byte or the open line's checkpoint, else marks.
  A wrapped row that ends at a restart nothing proves is not decoded whole: a lead pending there goes
  to the row after it, as one mark, never a U+FFFD. **A restart's start** (rule 2) is the window's first sync byte, or the exact start a line start
  or checkpoint gives; "first sync byte in the window" is for restarts only.
- **The new dependency**: `encoding_rs = "0.8.35"`, for CP949 only (VM-4): its 17,048-pair table is data
  sopen should not write by hand, and encoding_rs is the WHATWG decoder Gecko uses (since Firefox 56, its
  README). Its one required dependency, `cfg-if`, is in `Cargo.lock` already; licence `(Apache-2.0 OR
  MIT) AND BSD-3-Clause` (its notices, the WHATWG data's among them, are THIRD-PARTY-NOTICES's
  section 6a); a stripped release toy calling it grew by 183,104 bytes (macOS arm64), and
  sopen's own stripped release binary grew by 166,136 bytes with VM-4 (42,251,648 → 42,417,784; macOS
  arm64, `cargo build --release --locked` at 1c6fa00 and with VM-4, encoding_rs and VM-4's code
  together; 2026-11-25). Not for
  the rest: std decodes UTF-8 and UTF-16, encoding_rs has no UTF-32 (its `lib.rs` docs), and its
  `iso-8859-1` label is windows-1252, not Latin-1 (measured with `Encoding::for_label`).

### 7.5 Showing and changing the encoding

- The text view says its encoding beside its syntax, `1,204 lines · 88 KB · Rust · UTF-16LE (BOM)`, with
  §7.2's `(BOM)` or `(guessed)`.
- **F8 `Encode`** (and plain `E`, as `W` is F2's) opens the list in Text and Raw (D19). It is a question
  (§4): ↑ ↓ and Enter choose; Esc leaves the encoding as it was; the choice overrides any guess.
- **Changing the encoding rebuilds the line index only when the LF unit changes** (D22). UTF-8, ASCII,
  Latin-1, CP437 and CP949 share the byte 0x0A; UTF-16LE and UCS-2LE share `0a 00`, UTF-16BE and UCS-2BE
  `00 0a`; each UTF-32 has its own. Line 1's start follows the BOM rule (§7.2) without a rebuild.
  Wrap's memos and the highlighter's states are always dropped, and the place is kept by §7.3's jump
  rule. A rebuild through a stream reads at most the first 16 MiB again (2–8 s on 06's rig); a local one
  reruns the whole-file pass with progress, and Esc keeps the old encoding. Into CP949: §7.4's lazy
  checkpoints. **View 3 of a local binary** uses the local pass, with progress and Esc (D23).

## 8. Today's screen: the egui binding (provisional, after XR-T)

- **The strip** (D6), in the title row before the clock (`browser.rs:2793`, `chrome.rs:713-761`):
  `1 Image · 2 Hex`, the view on screen highlighted (a binary's `1·2 Hex` together), warn-coloured while
  F9 is armed, each entry clickable. The F-key row adds `9View` everywhere, `8Encode` in Text and Raw.
- **The keys are taken before the viewer's pass**, in the shell's input hook, where Cmd/Ctrl+digit
  already becomes F1…F10 (`mod.rs:785-796`): a digit's key event with no modifier, physical key first
  (`mod.rs:866`), is removed with the typed text that follows it (egui-winit 0.37 pushes the key, then
  its text: *trace* §2.4) when it chooses a view; F9 likewise. A row matched after the pass would type
  and switch at once in an editing hex view, which reads typed text without consuming it
  (`hex.rs:3650-3659`), and would never see the image's `1`, which the image consumes (`image.rs:3267`).
- **The hex view's two modes** (D35) are one new state of the hex window in the browser: viewing, typed
  text never reaches `type_char`; the status row says `viewing — Enter edits` or `editing — Esc ends`.
- **Fields are `text_edit_focused`**; DigitsAreData is declared by the hex window while editing — never
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
  question of a Mac's top row aside). The binding maps its input to VM-6's keys (a digit, F9, Esc, Enter,
  any other) and reports its own fields and questions; the strip is a cell row, the encoding a bar field.
- Image, Video and Page exist only where the screen can draw them (ssot item 2); elsewhere view 1 says
  why and the file opens on Hex, or on Text for HTML/HTM/XML (ssot's readings). The image's `1`
  collision exists only where an image is drawn.

## 10. Decisions

Every decision is reversible; the owner may change any of them.

| # | Decision | Why | Alternative |
|---|---|---|---|
| D1 | Views switched away from stay alive, hidden; zoom, playhead, place kept; nothing re-read | a remote image read whole on every 1↔2 costs its size each time; today's give-way reuses the stream | close the old view on every switch (loses state, re-reads) |
| D2 | Leaving a view with unsaved edits asks 07 §10's close prompt; a running Save makes the switch wait | the shipped closing rules; one view holds edits, one journal per file (07 §3) | refuse the switch until saved |
| D3 | A view key is the plain digit, by physical key, its typed text dropped, taken before the viewer's pass — in a viewing hex view too (D35) | one press one action; the Cmd/Ctrl+digit hook's precedent (04 §6.1); Shift+digit stays a character | match the typed character (AZERTY needs Shift) |
| D4 | Where digits are typed (a hex view being edited, a status-row field), F9 then 1/2/3 switches; F9-then-digit works in every view | F9 is free in every viewer (*trace* §5), types nothing, a terminal can send it, its slot is clickable | Alt+1/2/3 (GUI only); F9 cycling 1→2→3 |
| D5 | No view key while a question is up | the question keeps its keyboard | — |
| D6 | The strip is in the title row | the F-key row's digits are F-keys and Cmd/Ctrl+digits (04 §6.1) | the view's frame border |
| D7 | 3 = Text where 1 is not text (binaries); Raw where 1 is text (JSON's "raw text"); none for image, video, audio — 3 does nothing, the strip shows no 3; HTML/HTM/XML by D40 | ssot item 4; a JPEG as text is noise | Raw showing tabs, CRs, the BOM; pretty JSON; image metadata |
| D8 | Raw = Text with highlighting off | the smallest "raw text"; no second index | — |
| D9 | The page view is 09's; which slot it takes is D40 | message 18; the engine is VR-W's | — |
| D10 | A video that won't decode and empty media open on Hex with the reason; 1 then says the reason and stays | "모든 파일은 헥스 모드로 선택할 수 있음"; today the first closes, the second is an error (*trace* §1.3) | keep today's close and error |
| D11 | Text, Raw and Hex share one byte place | both models are keyed by byte offset | each keeps its own |
| D12 | The image's 100 % moves from `1` to `A` (README updated); `H` stays an alias where it exists | `1` is view 1; `1` is documented today (`README.md:288`) | keep `1`; views by F9 only in the image |
| D13 | One stream per opened remote file, kept beside the views; a revision's views read its `.revisions` copy | nothing read twice (06 §7.3); `Viewer.abs` is the working file | a stream per view (+1 channel each) |
| D14 | A hidden view gives back its save connection | 06 §6: the window is at 8–9 of sshd's 10 | restate 06 §6 as "+1 per view that saved" |
| D15 | ASCII, UTF-8, UTF-16LE/BE, UCS-2LE/BE, UTF-32LE/BE (= UCS-4), Latin-1, CP437, CP949 / EUC-KR | message 20's list and its "등"; the owner is Korean; CP949 resynchronises (§7.4); Latin-1 and CP437 cost a table each | leave CP949 out |
| D16 | UCS-4 is one entry with UTF-32 | they decode alike for every value that is a character | — |
| D17 | Default: BOM (D37), then the NUL pattern (8 KiB, ≥ 4 units, ≥ 3/4 ASCII units), then CP949 (D39), then UTF-8 | a BOM is certain; the pattern finds ASCII-heavy UTF-16/32 and rejects all but two of §7.2's binaries | other thresholds |
| D18 | UTF-16/32 files with no known extension route to text — the CLI route too (`main.rs`'s `route`) | before VM-3 they opened in hex (`kind::looks_like_text` was UTF-8 only) | keep hex; 3 shows them |
| D19 | F8 `Encode` (and `E`) opens the list | F8 is free in the text view; `E` mirrors `W`/F2 | another key |
| D20 | One U+FFFD per invalid unit or sequence, never counted | no extra pass | count them on the bar |
| D21 | The chosen encoding lasts for this opening only | nothing is stored about files today | remember per path in the state folder |
| D22 | A change of encoding rebuilds the line index only when the LF unit changes; wrap's memos and the highlighter's states always go; into CP949, checkpoints come lazily (D28); Esc during a local rebuild keeps the old | the marks depend on the LF unit alone, and five encodings share the byte | rebuild on every change |
| D23 | View 3 of a large local binary uses the local whole-file pass with progress and Esc | today's local route (06 Tensions); a local stream is untried (`local.rs:251-260`) | read local files through a stream |
| D24 | Views exist in the browser's viewer only; windows on their own keep their keys | message 20 names the window Enter opens | — |
| D25 | A Save or reload in one view makes the hidden ones stale, made again when shown | a hex Save reloads the shared stream; a whole image keeps its own bytes | — |
| D26 | A BOM under its own encoding is not text (D37) | syntax by first line; Go to counts file bytes | — |
| D27 | The pattern's sample is the kind sniff's 8 KiB | routing and decoding see the same bytes; the corpus answers alike at 64 KiB | the first 64 KiB block |
| D28 | CP949 never shows characters out of phase: a start from a line start within REACH, a lazy per-GRID phase checkpoint (lines longer than REACH, on first need), or the last sync byte within W = 256; else dim `░` marks to the next sync byte | the sync-byte property held at all 302,874 cuts; a mark is honest, a wrong syllable is not; a switch into CP949 rebuilds nothing (D22) | decode from the offset (wrong syllables until a sync byte) |
| D29 | F9 in a status-row field closes the field and arms (amends 04 §3 rule 4, for F9 only) | F9 types nothing, so it steals no typing; the field is the view's own | Esc first, then F9 |
| D30 | Until 09's page view lands, HTML/HTM/XML are 1 Text, 3 Raw | one rule for every text file until Page exists | 1 and 3 the same view |
| D31 | The model is screen-free modules outside `src/gui/` | a terminal binding and the GUI share them | inside each window |
| D32 | A C1 control is drawn as a dim `·` | Latin-1 makes 32 of them; today they reach the screen glyph-less | U+FFFD (hides that they are valid) |
| D33 | An armed F9 lasts one key, no timeout | a timeout is a race with a slow hand | 2 s |
| D34 | The bar's encoding label and `SOPEN_STATE enc=` ship with VM-2, before XR-T | the only way to see the default pick and take receipts; one bar field, the same on any screen | wait for the binding |
| D35 | In the browser's viewer the hex view opens viewing; Enter or Insert starts editing; Esc or F9 ends it; `--hex` and the binary dialog keep today's typing | today the grid always has focus and digits write nibbles (`hex.rs:4013-4018`, `3650-3660`), so image 1→2→1 would edit a byte | DigitsAreData in every hex view (F9 only) |
| D36 | One grid-cut function per encoding, used by both pieces, in [g − 3, g]; VM-1 exposes its look-back | the text view's `grid_cut` before VM-2 looked back only; two pieces that cut apart show a character twice or lose it | a forward skip at each piece |
| D37 | BOM: the longest first, whatever the size; skipped only under its own encoding (UTF-16's for UCS-2, UTF-32's for UCS-4); a place inside it is line 1's start | `FF FE 00 00` is UTF-32LE's BOM before UTF-16LE's; under another encoding it is bytes like any other; `span_at`'s fallback would show the last line | the draft's size-dependent pick |
| D38 | UTF-16 resync looks at the preceding unit and moves only across a valid pair; a standalone low surrogate is one U+FFFD | a low surrogate pairs only with the high one right before it, so one unit of look-back equals whole-file decoding | skip every low surrogate (hides a standalone one) |
| D39 | CP949 is the default's step 3, by (a)–(e), shown `(guessed)`, F8 overriding; routing unchanged. A Korean-first prior: bytes that are Korean in CP949 and something else in another encoding are guessed CP949 (Tensions) | the owner is Korean; over §7.2's corpus it picks every Korean CP949/EUC-KR sample up to 8 % hanja and nothing else; (e) alone stops hiragana-only Shift-JIS; real Chinese prose is stopped by (b) and (d) | never a default (F8 only); an ASCII-share criterion (tried and deleted 2026-11-25: GBK `你好\n` repeated passes it, a list of long Korean names fails it) |
| D40 | HTML, HTM and styled XML: 1 Page · 2 Hex · 3 Text; other XML, RSS among it: 1 Text · 2 Hex · 3 Page (09 §5–§6); supersedes D7 for these | both engines show plain XML as run-on text (09 §5) | every XML a page (the draft's D9) |

## 11. Acceptance

Model tests run with no window, binding tests through the test kit; every key test sends the key event
**and** its typed text in one frame, and every ranged decode is compared with the whole-file decode (§12).

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
- EA-3 (VM-4). **CP949**: Korean text encoded by encoding_rs (갊 = `b0 a7`, CRLF) as a `.txt` → `CP949
  (guessed)`, the text equal to encoding_rs's whole-file decode; set to UTF-8 → U+FFFD shown, the line
  count unchanged, no index pass run; back → `CP949 (guessed)`. An extension-less CP949 file → Hex; 3 →
  Text, `CP949 (guessed)`. **Ranged equals whole** wherever characters show: (i) past 16 MiB, a 256 KiB
  line of Hangul with spaces, Go to 100 KiB past its start → exact by the last sync byte (§7.4 rule 3,
  no LF within REACH); (ii) inside the
  index, a 256 KiB line of Hangul with no sync byte (longer than REACH + GRID), Go to 100 KiB past its
  start → placeholders and the status line, then, once the line's checkpoint read ends (it reads at most
  the line's first 100 KiB), rows equal to the whole-file decode; (iii) the same line past 16 MiB →
  placeholders with the reason, never a syllable the whole-file decode lacks; (iv) a line shorter than
  REACH past 16 MiB → exact.
- EA-4 (VM-1, VM-4). Invalid input: a UTF-16 unpaired surrogate, a UCS-2 surrogate pair (two U+FFFD), a
  UTF-32 value above 10FFFF, an odd trailing byte, ASCII 0x80; CP949 `D0 41` (a lead ≥ 0xC7 before `A`)
  → U+FFFD, `A`, while `B0 41` and `81 41` are 캚 and 갂; a lead before a sync byte, `B0 20` → U+FFFD,
  space → each as §7.1 says, the next character in place; Latin-1 and CP437 over all 256 bytes.
- EA-5 (VM-1, VM-3, VM-4). Defaults over §7.2's two corpora → §7.2's answers; `FF FE 00 00 41 00 00 00
  42` → UTF-32LE `A` and one U+FFFD; an extension-less UTF-16 file → text. A UTF-8 BOM before
  `#!/bin/sh` → shell highlighting, no U+FEFF on line 1; set to Latin-1 → line 1 starts `ï»¿#!`, label
  `Latin-1`; back to UTF-8 → `UTF-8 (BOM)`, the BOM skipped again. A UTF-16LE file with a BOM set to
  UCS-2LE → `UCS-2LE (BOM)`, skipped; Go to `0x1` there → line 1 at the top. The preview pane and the
  viewer decode a UTF-16 file alike.
- EA-6 (VM-2). UTF-8 → Latin-1 → CP949 → UTF-8 runs no index pass; UTF-8 → UTF-16LE runs one; every
  change drops wrap's memos and the highlighter's states and keeps the place, aligned; line 1's start
  follows the BOM rule (EA-5); Esc during a local rebuild keeps the old encoding.
- EA-7 (VM-1, VM-2). **Cuts and resyncs equal the whole-file decode**: in UTF-8, UTF-16LE/BE and CP949, a
  character across each grid point and block edge → the pieces on both sides together give it once. In
  UTF-16, a valid pair across a grid point stays one character; a standalone low surrogate at a grid
  point and at a jump's place is one U+FFFD; a jump onto a pair's low half shows the pair whole. Not
  wrapped (rule 4): Right ×1–3, End, and a one-byte sideways wheel step, over a CP949 line that starts
  with one ASCII byte and over a UTF-16LE line holding an emoji → every row equals the whole-file decode
  from its first shown character; `left` stays a multiple of the unit.

**Views** (VM-5 and VM-6 as tables of state × input; every VA item end to end with VM-7)
- VA-1. Each row of §3 (HTML, HTM and XML by D40, and by D30 before 09), locally and through the Fake:
  Enter shows 1; 2 shows Hex; 3 shows the third view, or does nothing and the strip has no 3.
- VA-2. **Hex: viewing, editing, a switch**: an image, `2`, `1` → the image again, no byte written. In
  Hex, Enter → `editing — Esc ends`; `4`, `1` (key + text) type 0x41, no switch; F9 then `1` → the close
  prompt; Cancel → Hex, viewing, the edit kept; plain `1` → the prompt again; Discard → view 1, the
  journal gone; again with Save → view 1 once the Save lands, the file holding 0x41. Viewing, during a
  Save, `1` → `saving…`, then view 1; editing, it is held back, as today. Editing, Esc → viewing; a second Esc → the close prompt. A revision's Hex:
  Enter says it is read-only. `--hex` types at once, as today. An image crop, then `2` → the prompt.
- VA-3. F9 then `x` while editing hex → nothing typed, editing ended, no switch. Go to open: `1` goes
  into the field, F9 closes it and arms. The close prompt, Save As or the encoding list up: 1, 2, 3 and
  F9 do nothing. Cmd/Ctrl+1, +2, +3 stay F1, F2, F3. `2` and Esc in one frame → the viewer leaves.
- VA-4. Image zoom and rectangle survive 1→2→1; the media playhead survives and nothing plays while
  hidden; Text→Hex puts the cursor on the top row's first byte; Hex→Text opens at the cursor's line.
- VA-5. Remote: 1→2→1→2 on an image reads the file once (`SOPEN_STREAM_STATS` `fetched=`); a handed-over
  image preview's 2 opens one stream; channels never pass 2. A revision's 2 shows its bytes, read-only.
- VA-6. A video none of whose frames decode, and empty media → Hex with the reason; 1 says it again. F1
  lists 1, 2, 3 and F9; `keys.rs`'s `no_duplicate_bindings` passes.

**Live receipts** (AGENTS.md: screenshots someone looked at; remote ones under `/tmp/sopen-receipts/`)
- LR-1. Each §3 kind in views 1, 2 and 3, locally and remotely, through `SOPEN_STATE` `view=N`; a remote
  UTF-16 log with a BOM → text from block 0 (`first` on the stats line); a remote binary of several GB →
  3 shows text from block 0, no whole-file read.
- LR-2. The owner at the Mac: 1, 2, 3 on the US layout and with the Korean input source on; Enter, then
  F9 1 in a hex view; the same on the Linux laptop under KDE.

## 12. Test plan

- **The model units** are plain unit tests: per encoding the split property (two pieces cut at a
  character start the core reports decode as the whole does), LF search over every Hangul syllable in
  UTF-16LE/BE and UTF-32, §7.1's invalid inputs, §7.2's corpora, CP949's sync-byte property as §7.4
  measured it; the view set and the key rules as tables of state × input → command.
- **The corpus is generated by the tests**, no binary fixtures: §7.2's texts, arrays and noise, and for a
  real binary the first 8 KiB of the test's own executable.
- **Ranged equals whole.** The text document's tests run locally and through the Fake on both sides of
  the 16 MiB cap. Every ranged read — a jump, a grid piece, a restart, a lazy checkpoint — is compared
  with decoding the whole file, not only with the other scroll direction: rows that agree with each
  other can agree on a wrong phase.
- **Key tests send what the platform sends.** `testkit::Driver::key` sends only `Event::Key`, with
  `physical_key: None` (`testkit.rs:240-252`), and an `SOPEN_KEYS` step is a key or a text, never both
  (`browser.rs:2167-2180`): a test built on either passes while the real keyboard types and switches at
  once in an editing hex view (*trace* §2.3 item 1). The egui binding adds `Driver::press` (the key with
  its physical key, then its typed text, in one frame, as egui-winit 0.37 does) for every digit and
  letter, and the `SOPEN_KEYS` step `typed:1`. A terminal binding's tests send the character alone.
- **The gate** is the loop's: `just check` with `SOPEN_REQUIRE_FFMPEG=1 SOPEN_REQUIRE_SFTP_SERVER=1` and
  the Linux gate's two clippy runs; a unit that changes a screen brings a screenshot someone looked at.

## 13. Work units

Model units are ready to build; screen units wait for XR-T. Files are disjoint except the `mod` lines in
`src/lib.rs`, where whichever unit merges second rebases (06 §13's rule).

| ID | Unit | Owns | After | When |
|---|---|---|---|---|
| VM-1 | **Encoding core**: §7.1's list; §7.2's steps 1, 2 and 4 with step 3's slot empty, and the BOM rule; one decoder per encoding (characters and byte lengths); LF/CR unit search; §7.3's four rules, the grid cut's look-back W exposed; a phase slot (a no-op but for CP949) | `src/encoding.rs`, `src/encoding/tests.rs` (new); `src/lib.rs` (`mod` line) | — | ready |
| VM-2 | **Text document per encoding**: index pass, line reads, spans, look-back, grid pieces, restarts and jumps through VM-1; §7.3's newline sites; the BOM skip and `span_at`'s guard; a rebuild only when the LF unit changes; the placeholder marks (the lazy checkpoints moved to VM-4: `phase()` is `None` for every encoding until CP949); a C1 control as a dim `·` (D32); the bar's encoding and `SOPEN_STATE enc=` (D34) | `src/gui/text.rs`, `src/gui/text/wrap.rs`, `src/gui/text/tests.rs`, `src/gui/text/stream_tests.rs`; `src/gui/mod.rs` (`State`'s `enc` only); `src/gui/browser.rs` (handing `enc` on, as `at`) | VM-1 | ready |
| VM-3 | **Kind sniff**: UTF-16/32 → text (D18) | `src/kind.rs`; `src/gui/text.rs` (the bar: the stats before the title, the encoding whole) and its tests in `src/gui/text/tests.rs`, `src/gui/text/stream_tests.rs`; `src/views/tests.rs` (the D18 row) | VM-2 (before it, such a file would open misaligned) | ready |
| VM-4 | **CP949 / EUC-KR**: its decoder, sync bytes, grid cut and phase; step 3 of the pick (D39); `encoding_rs`; the binary-size measure; the lazy phase checkpoints (§7.4), the slot VM-2 left | `src/encoding/cp949.rs`, `src/encoding/cp949_tests.rs` (new); `src/encoding.rs` (the entry, the step-3 slot and the `mod` line only); `src/gui/text.rs` (the checkpoint slot only); `Cargo.toml`, `Cargo.lock` | VM-1, VM-2 | ready |
| VM-5 | **The view set**: §3's table, §5's switch rules, §6's kept means, screen-free | `src/views.rs`, `src/views/tests.rs` (new); `src/lib.rs` (`mod` line) | — | ready |
| VM-6 | **The view keys**: §4 as a function of (state, armed, input) → command, the hex view's viewing and editing included | `src/view_keys.rs`, `src/view_keys/tests.rs` (new); `src/lib.rs` (`mod` line) | — | ready |
| VM-7 | **egui binding** (§8), if XR-T keeps egui windows | `src/gui/browser.rs`, `src/gui/browser/prepare.rs`, `src/gui/browser/keys.rs`, `src/gui/mod.rs` (the input hook, `view=`), `src/gui/chrome.rs`, `src/gui/testkit.rs`, a hook each in the hex (viewing and editing, D35), image, media and text windows; README, AGENTS.md | VM-2…VM-6, XR-T | after XR-T |
| VM-7b | **What VM-7 left** (2026-11-25): D13 and D14 — the views of one remote file share its stream, kept beside the views, and a hidden view gives its save connection back, so a viewer is back within 06 §6's 1–2 (VM-7 makes every view on a session of its own and keeps it hidden: 1–4); D25's reload half (a view's Cmd/Ctrl+R reload makes the hidden views stale; VM-7 tells only Saves that landed); D10 for empty media and a video none of whose frames decode (open on Hex with the reason; VM-7 has it for an image that does not decode); D29 in the text view (F9 drops the Go to field's focus but leaves it open); VA-5 and VA-6 | `src/gui/browser/view_set.rs`, `src/gui/browser/prepare.rs`, the media and text windows' hooks | VM-7 | ready |
| VM-8 | **TUI binding** (§9) | per XR-T | VM-5, VM-6, XR-T | after XR-T |
| VM-9 | **Page view** in the set (09's WV-F) | per 09 | VM-5, 09, XR-T | after XR-T |
| VM-D0 | 04 §2 and K-R1 brought to the shipped code (§14) | `.prd/04-file-keys.md` | — | ready |

- **Order**: VM-1 → VM-2 → VM-3 and VM-4 side by side (disjoint); VM-5, VM-6 and VM-D0 from the start;
  the bindings after XR-T. Each unit's PR amends the sentences it makes untrue (§14), as 06 §14 did.
- **Why VM-2 carries a screen piece before XR-T** (D34): without the bar's label and `enc=` the model is
  reachable only by tests — the default pick cannot be seen and no receipt can be taken. Both are one
  field and one key of state, which any screen keeps; the F8 list, the strip and the keys wait.
- VM-2's few lines in `src/gui/mod.rs` (`State`'s `enc` and its parse) and `src/gui/browser.rs`
  (`enc` handed on as `at` is, and past a preview's hand-over, as `at` is), and VM-7's larger share, meet
  the browser workstream's input-hook round, the web view and the F4 editor there: sequenced; a
  conflict goes to the dispatcher. The text document stays in `src/gui/text.rs` until XR-T moves it
  (D31 covers the new modules).
- **CP949's proofs, closed by VM-4** (§7.4): VM-2 decoded a row start forward from the line start
  within REACH (`Wrapper::start_at`) but gave a grid cut only the W bytes before its grid point
  (`TextDoc::grid_cut`), and a restart only its window (`Wrapper::restart`). Now all three take the
  same proofs — a sync byte within W, the line's (or a proven piece's) start less than REACH back, a
  phase checkpoint — through `TextDoc::proof_in` and `TextDoc::grid_cut`; VM-4's few lines in
  `src/gui/text/wrap.rs` (those two calls) and its new test files under `src/gui/text/tests/` and
  `src/gui/text/stream_tests/` (each one `mod` line) are outside its row above.

## 14. Text this amends

- **04 §2, now** (VM-D0; the code is the shipped contract, 07 §14 ED3): "In a viewer the slots keep the
  viewer's meanings — 3 Exit, 4 Edit, and the hex window's 5 Save, 6 Goto, 8 Asc/hx, the image window's
  5 Save — 1 is Help everywhere, and 2, 7, 9, 10 are empty" becomes "… 3 Exit, 4 Edit; the hex
  window's 5 Save, 6 Goto, 7 SaveAs, 8 Asc/hx; the image and media windows' 5 Save, 7 SaveAs; the text
  viewer's 2 Wrap (Unwrap while wrapped). 1 is Help everywhere; a slot not named is empty"
  (`chrome.rs:140-202`; *trace* §5). With VM-7: 9 View in every view, 8 Encode in Text and Raw.
- **04 K-R1, now**: the hex row reads `1Help 3Exit 4Edit 5Save 6Goto 7SaveAs 8Asc/hx` (`9View` with VM-7).
- **01 §3** (VM-3): a UTF-16/32 file with no known extension is text, so the CLI route moves with it:
  `sopen` on such a file opens the editor (`main.rs`'s `route`, `Kind::Text => Route::Editor`) where
  before VM-3 it opened the binary dialog (`Kind::Other => Route::Binary`).
- **07 §10** (VM-7): the windows table gains "in the browser's viewer the hex view opens viewing; Enter or
  Insert starts editing, Esc or F9 ends it (08 D35)"; 07 §2's hex model is unchanged.
- With their units: 06 §7.2 per encoding, AGENTS.md `enc=` (VM-2); 01 §3a's views and D10, 04 §3 rule 4
  (D29), 06 §6 (D14), 06 §7.3 (Hex view is 2), `README.md:288` (`A` is 100 %), AGENTS.md `view=` and
  `typed:` (VM-7).

## Tensions

- **"Raw text" for JSON.** D7/D8 make 3 the unhighlighted text, since JSON's 1 is already text; a
  pretty-printed or tree view as JSON's 1 would make "raw" mean "as stored". The owner's call.
- **CP949's guess has narrow margins** (D39). A GB2312 text whose characters all decode in CP949 is
  66–68 % Hangul against the 3/4 bar, and hiragana-only Shift-JIS is stopped only by (e). Korean thick
  with hanja (47 % Hangul) is not picked: F8 picks it. VM-4's generated corpus agrees, but for one
  sample: a kana-and-kanji EUC-JP text decodes in CP949 with no error (its kanji as Hangul and hanja)
  and is stopped by (d) alone, at 36 % Hangul — not the CP949 error §7.2's corpus found.
- **The CP949 guess is a Korean-first prior** (D39, DEC 2026-11-25), shown `(guessed)`. Text made
  only of pairs that are KS X 1001 Hangul in another encoding too — GBK's `你好你好` (`C4E3 BAC3 C4E3
  BAC3`), `你好` a line however many times — is the same bytes as Korean text and is guessed CP949 by
  design: no rule over the bytes can tell them apart. Real Chinese prose is stopped by (b) and (d)
  (§7.2's excerpt table). The way out is choosing the encoding by hand (F8, VM-7's list).
- **SVG is text** (`kind.rs:62`) though it is a picture: its 1 is the highlighted source. 09 leaves
  `.svg` as 01 §3 makes it (09's scope), and the `image` crate's features in `Cargo.toml` decode no SVG.
- **F9 and message 19.** A Mac's top row sends media keys unless fn is held; message 19's outcome decides
  whether F9 is reachable without fn, in the GUI and in a terminal.
- **View 3 of a 24 GB local binary** reads the whole file before its first screen (D23), about 10 s at
  2–3 GB/s `[추정]`; a local stream would not, but is untried.
- **D18 on the CLI.** `sopen` on a UTF-16/32 file with no known extension hands it to the editor (VS
  Code, else Zed; `main.rs`'s `route`), whose handling of BOM-less UTF-16 is unverified `[추정: not
  checked]`; and §7.2's "2 shows it" for a binary the pattern takes for text (int16 0–127, int32
  0x20–0x7E) has no CLI counterpart — only `--hex` opens such a file as bytes there.
