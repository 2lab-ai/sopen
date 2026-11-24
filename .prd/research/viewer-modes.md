# VR-M trace — the Enter viewers today (2026-11-24)

Read at `docs/viewer-ssot` = 767a8a8, read only (the workstream folder `.prd/viewer/` has since been renamed `.prd/xtools/`, same ssot.md).
Every `file:line` is at that commit. `[추정]` = not read in code or measured.

## 0. The happy path being traced

"Enter on a file in the browser's list puts one viewer for that file on screen, in the browser's own
window, and that viewer's keys act on it."

## 1. Enter → which window

### 1.1 The key and the dispatch

| Hop | Where | What |
|---|---|---|
| 1 | `src/gui/browser/keys.rs:475` | row `(List, Enter, Plain) → Command::Open`; F3 is `Command::View` (`:462`) |
| 2 | `src/gui/browser.rs:2187-2197` | `list_keys` looks the list's rows up only while no viewer, tree or git focus is up |
| 3 | `src/gui/browser.rs:1102-1103` | `Open → enter(ctx, false)`, `View → enter(ctx, true)` |
| 4 | `src/gui/browser.rs:1775-1833` | `enter`: a folder is listed; a file takes what the preview host has (`preview.take`, `:1803`): a ready preview is handed over (`:1806`), a card or nothing starts a fresh open (`:1807`, `:1821`), a running preparation becomes the opening (`:1808-1820`). Fifos/sockets/devices and dangling links are refused on the status row (`:1824-1831`) |
| 5 | `src/gui/browser.rs:996-1065` | `start_open_with`: `prepare::prepare` (or `prepare_in_place` for a revision/diff, `:1043-1047`) on a worker thread; the result comes back through `Opening` |
| 6 | `src/gui/browser.rs:1482-1541` | `poll_opening`: the result → `opened` (`:1526`); a preview's preparation that ended → `hand_over` (`:1531-1534`) |
| 7 | `src/gui/browser.rs:1546-1665` | `opened`: one `Prepared` variant → one `embed` (table 1.2) → `view(...)` |
| 8 | `src/gui/browser.rs:1696-1722` | `view`: the browser holds **one** `Viewer { embedded, hold, abs, switch, read_only }` (`:547-559`); `debug_assert!(self.viewer.is_none(), "a viewer was replaced unasked")` (`:1713`) |
| 9 | `src/gui/browser.rs:2802-2806` | while `viewer` is `Some`, the central panel draws `viewer_ui`, never the panels |

### 1.2 Kind → `Prepared` → window

The kind: `kind::detect_sniffed` (`src/kind.rs:229-244`): a folder by stat; then the extension table
(`:32-127`); only when the extension does not decide, a sniff of the first 8 KiB (`SNIFF_BYTES`,
`:30`) through `looks_like_text` = no NUL and valid UTF-8 (`:157-165`). `.ts` is text by the sniff, else
video when it looks like a transport stream (`:167-194`).

Local files (`src/gui/browser/prepare.rs:205-281`) and remote files (`streamed_prepared`,
`:301-377`, every remote kind opens as a `Source` on the open's own connection):

| Kind | Local | Remote | `opened` embeds (`browser.rs`) | Window (frame title) |
|---|---|---|---|---|
| image | copied into the cache, then `decode_unless_asking` (`prepare.rs:207-256`): `Image`, or `ImageAsking` (the memory line), or **`Hex` with the reason** when it does not decode (`:240-247`) | `ImageStream` (`:332-340`): the window reads the header, then the whole file, on its own worker | `image::embed` / `embed_asking` with `HexSwitch::local` / `embed_stream` with `HexSwitch::new` (`:1572-1623`) | "Image Edit" (`image.rs:58`) |
| video, audio | `refine_local`; a `.ts` that is not media → hex (`prepare.rs:257-270`) | `.ts` refined over the play URL; not media → hex on the same stream (`:341-370`) | `media::embed` / `embed_streamed` (`:1624-1641`) | "Video Edit" / "Audio Edit" (`media.rs:101-102`) |
| text | `TextDoc::open_cancellable_as` — the whole-file index pass before the first screen (`prepare.rs:271-279`) | `TextDoc::streamed(source)` (`:323-331`) | `text::embed_scripted(doc, title, name, notices, live, at, loading)` (`:1643-1656`) | "Text Viewer" (`text.rs:3123`) |
| other (binary), dir | `hex_of`: `hex::Base::open_as` in place (`prepare.rs:280`, `:380-386`) | `HexFile::streamed` (`:372-375`) | `hex::embed` (`:1642`) | "File Edit" (`hex.rs:71`) |

- The binary dialog (01 A6, `src/gui/binary.rs:1-9`) is the **CLI** route's; the browser's Enter goes
  straight to hex (01-spec §3a: "the binary dialog's 'ask first' is not needed here").
- `.html`, `.htm`, `.xml`, `.json`, `.svg` are text by extension (`kind.rs:45, 55-57, 62`): today they open in
  the text viewer, highlighted by extension (`text.rs:2134-2139`).
- **A UTF-16 or UTF-32 file opens in hex** unless its extension is in the text table: its NUL bytes fail
  `looks_like_text` (`kind.rs:158`), and the test pins a UTF-16LE BOM as not text
  (`kind.rs:331`: `assert!(!looks_like_text(b"\xff\xfe\x41", false))`).
- A revision or a diff (03 §6.2) opens through the same `opened`, read-only, from the cache's
  `.revisions` over a `LocalFs` connect (`src/gui/browser/git_host.rs:2111-2128`; `browser.rs:1705-1707`).
  **`Viewer.abs` is then not the file on screen**: `opened` swaps it for `ReadOnly.edit`, the working
  file's path that F4 opens (`browser.rs:1659-1664`; `git_host.rs:3551-3556`). Anything that re-opens
  "the viewer's file" from `Viewer.abs` would show the working file instead of the revision — the
  revision's own path and its `LocalFs` connect exist only in `Opening` (`browser.rs:1004-1064`).
- The preview builds the same documents: local text through `TextDoc::open_capped_as`
  (`prepare.rs:1301-1303`), remote text through a stream (`prepare.rs:1122`). Whatever picks a text
  file's encoding must live in `TextDoc`, or the preview and the viewer disagree.

### 1.3 Today's fallbacks and the one switch that exists

| Case | What happens | Where |
|---|---|---|
| local image that does not decode | opens in **hex**, `"<why>; the file is shown as bytes"` as the first row | `prepare.rs:240-247` |
| remote image that does not decode | the image window **gives way** to hex on the same stream | `image.rs:4592-4633` (`give_way`), `browser.rs:2324-2337` |
| image whose decode needs the memory question | the memory line `Continue (Enter) · Hex view (H) · Close (Esc)` | `image.rs:4807-4831`, 06 §7.3 (`06-streaming.md:865-879`) |
| H on the memory line | `give_way(None)`; the browser drops the image viewer and embeds `hex::embed(file, notices)` in its place, read-only kept for a revision | `image.rs:4829-4831, 4908-4910`; `browser.rs:2328-2336` |
| `.ts` that is not media | hex | `prepare.rs:258-261`, `:355-358` |
| video none of whose frames decode | the window asks to close; the browser drops it and says the reason on the status row (no hex) | `mod.rs:674-679` (`failure`), `browser.rs:2338-2348` |
| empty media | an error, nothing opens | `media.rs:187` |
| unreadable / refused session | status row error, the list as it was | `browser.rs:1556-1564` |

- **The only switch between windows today is image → hex**, and only while the image is not whole
  (`Streaming`, `image.rs:4467-4505`: `switch` lives on the loading window). Once the image is whole
  there is no hex key. The give-way **drops** the image window (`browser.rs:2331` `drop_viewer`); that is
  safe only because no edit can exist before the image is whole.
- PR #61 (merge `f5f1242`, commit `8c70f8f`, "Esc closes a hex viewer on its second frame; no switch
  after Esc") touched only `browser.rs` and its tests (`git show 8c70f8f --stat`). Its two races, from
  `git show 8c70f8f -- src/gui/browser.rs`: (1) the hex grid takes focus on its first frame and egui
  applies the focus-lock filter only from the second, so an Esc then looked like a field's own Esc —
  fixed by `focused_before = ctx.text_edit_focused()` (only a text field owns an Esc); (2) H and Esc in
  one frame made the image give way **and** ask to close — fixed by `.filter(|_| !asked)`
  (`browser.rs:2329`). The H key itself came with W8b: `git log -S'Key::H' -- src/gui/image.rs` →
  `fe5c0b6 feat(stream): W8b — every remote open goes through a stream` (06 §13, `06-streaming.md:1844`).
  Lesson for view keys: a switch and a close in the same pass is a known race; so is a focus test
  that counts the hex grid as a field.
- The give-way reuses the stream: `HexFile::streamed(origin, source, repaint, cache)` (`image.rs:4621-4624`),
  "nothing read again" (06 §7.3).

### 1.4 Where the preview pane's content is built (for the later F4-editor unit)

| Hop | Where | What |
|---|---|---|
| 1 | `browser.rs:2286-2310` | each pass: the display's file (the bar's file once it has rested) and the bar → `self.preview.sync(ctx, display, bar)`; nothing new is prepared while an opening, a viewer or the git keyboard is up (`:2305-2308`) |
| 2 | `browser/preview.rs:211` `sync` → `:263` `start` | one preparation at a time, on a worker |
| 3 | `browser/preview.rs:297` → `browser/prepare.rs:934` `prepare::preview` | the same kinds as Enter, within 02 §4.3's budgets, on the window's one preview connection; an info card when a budget is hit |
| 4 | `browser/prepare.rs:799-830` `Previewed::show` | the window per kind: `text::embed` / `text::embed_source(…, Some(index))`, `image::embed_preview`, `hex::embed`, `media::embed_preview`, `card_window`; then `set_preview(true)` (`:824`) |
| 5 | `browser.rs:2427-2428` | `Some(view::Content::Preview(_)) => self.preview.ui(ui, rect)` |
| 6 | `browser/preview.rs:383-400` `ui` | the frame titled with the path (`chrome::panel_frame`), the preview drawn with the pass's input withheld (`i.events.clear()`, pointer and scroll zeroed) — no key reaches it (02 §4.4) |

### 1.5 The preview hand-over

`preview::take` (`src/gui/browser/preview.rs:326-353`) → `Preview::hand_over` (`prepare.rs:897-913`):
`set_preview(false)`, `set_shown(true)`, the lease kept. The handed-over window is the viewer. A
handed-over text, hex or media preview moves its stream into the viewer; the browser keeps no clone.

**A whole remote image has no stream, however it was opened.** `Streaming::finish` drops it once the
bytes are in: `self.source = None; self.switch = None;` (`image.rs:4717-4718`); the image window then
holds the bytes in memory (`ImageFile::new(origin, loaded.bytes, …)`, `:4725`) and an `Origin` that
connects on first use. The hand-over says the same for a previewed image (`prepare.rs:887-890`). So
after the decode, hex of a remote image cannot be "the same stream": it is a new stream (one channel,
the viewer's channel count stays 1 because the image window holds none), or a hex document over the
image's bytes in memory.

## 2. Every key each window handles (where plain 1, 2, 3 land)

### 2.1 The order of a pass with a viewer up

| Step | Where | Effect on keys |
|---|---|---|
| a | `src/gui/mod.rs:785-796` (`FKeyInput::input_hook`, before any window's pass) | Cmd/Ctrl + digit (by **physical** key first, `:866`) becomes F1…F10, its typed text dropped (`:844-848`, `:879-882`); a plain digit is left alone (`Digit::Plain`, `:875`); "not while a text field has the typing", judged from **the focus the last pass left** (`:787`) |
| b | `browser.rs:2767-2777` | `play` (`SOPEN_KEYS`), git Esc, polls; `list_keys` does nothing while a viewer is up (`:2189`); `git_focused` is `None` while a viewer is up (`git_host.rs:1111-1114`) |
| c | `browser.rs:2793-2794` | title row and F-key row drawn (§5) — before the viewer's pass |
| d | `browser.rs:2323` | **the viewer's pass** (`host_pass`) — the window reads and consumes its own keys |
| e | `browser.rs:2352-2363` | while a text field has the typing (`text_edit_focused`), every key event but Esc is withheld from the table |
| f | `browser.rs:2364-2380` | the browser's `Context::Viewer` rows on **what the viewer left**: F1 Help, F3 and Esc CloseViewer, F4 Edit (`keys.rs:500-503`) |

So a key row in the browser's table for `1`/`2`/`3` would only see what the window did not consume.

### 2.2 Per window (plain digits marked)

| Window | Keys it takes (all from code) | Digits |
|---|---|---|
| hex (`hex.rs:3545-3698`, `:4518-4547`; nothing past Esc-stops-a-Save while `locked() \|\| dialog_up()`, `:3555-3557`) | Esc stops a streamed Save (`:3549-3554`); Save As (`save_as::pressed` = Alt+Cmd/Ctrl+S, `save_as.rs:371-380`) and F7 (`:3561`); Cmd/Ctrl+R reload, stream only (`:3567`); Cmd/Ctrl+S, Shift+Cmd/Ctrl+S (`:3570-3574`); F5 (`:3580`); Cmd/Ctrl+Z / Shift+Cmd/Ctrl+Z (`:3588-3600`); Cmd/Ctrl+Home/End, arrows, PgUp/PgDn, Home/End (`:3607-3641`); Tab and F8 switch the typing column (`:3643-3649`); Cmd/Ctrl+G and F6 open Go to (`:4520-4526`); the prompt's Enter/Esc (`:4303-4304`), Esc (`:4338`) | **every typed character** is read from `Event::Text` **without being consumed** (`ctx.input`, not `input_mut`, `:3650-3659`): in the hex column a hex digit types a nibble (`type_char`, `:1002-1019`), in the ASCII column any of `' '..='~'` types a byte (`:1020-1025`), anything else in the ASCII column says `'<c>' is not printable ASCII` (`:3690-3694`). Off only at a revision (`no_edit_here`, `:2270-2275`), while reloading or saving (`:3664-3671`), while Go to or the read-only path field owns the frame (`goto_owns`, `:4541`, `:4546-4547`), in a preview (`:4497-4503`) |
| image (`image.rs:3205-3270`; nothing while the prompt or Save As is up, `:3207-3209`; its plain letters and `1` go through `plain_key`, which is off whenever **any** widget has focus — `egui_wants_keyboard_input` is `memory.focused().is_some()`, egui 0.37.0 `context.rs:3485-3487` — while `text_edit_focused` is true only for a text edit, `:3490-3496`) | Esc stops a Save / drops a queued one / clears the rectangle (`:3220-3263`); Save As (`:3237`); Cmd/Ctrl+Z, Shift+Cmd/Ctrl+Z, Cmd/Ctrl+S, Shift+Cmd/Ctrl+S (`:3240-3249`); F5 (`:3252`); `C` crop, `P` pixelize, `F` fit (`:3255-3266`); clipboard events (`:2828-2885`); the block-size slider (`:3305`); memory line Enter, `H`, Esc (`:4826-4836`) | **`1` = actual pixels**, consumed by `plain_key(ctx, Key::Num1)` (`:3267-3269`); `plain_key` is `consume_key(Modifiers::NONE, …)` unless egui wants the keyboard (`mod.rs:1880-1882`). The image preview test lists `Num1` as a key a preview ignores (`image/preview_tests.rs:92`) |
| media (`media.rs:3905-3983`; nothing while the prompt or Save As is up, `:3908-3910`; no text field: `grep DragValue\|Slider\|TextEdit src/gui/media*` finds none outside tests) | Space, `I`, `O`, `E`, Cmd/Ctrl+Z / Shift, Cmd/Ctrl+S, F5, Delete/Backspace, `M`, Esc chain, ←/→ with Shift/Alt steps, clipboard V/X (`:3827-3900`), Save As (`:3912`) | none |
| text (`text.rs:3949-4026`, `:5094-5103`) | Esc stops Go to's pass; Cmd/Ctrl+R (stream); `W` and F2 wrap; ↑↓ PgUp PgDn; Cmd/Ctrl+Home/End; Home/End; ←/→ unwrapped; Cmd/Ctrl+G opens Go to in the status row (`:5097-5102`), whose Enter goes there (`:4569-4574`) | none outside the Go to field |
| image memory line (`Streaming`, `image.rs:4824-4837`) | Enter Continue, `H` Hex view, Esc | none |
| Save As modal (`save_as.rs:659-689`) | its path field, Enter, Esc | typed into the field |
| the browser's viewer rows (`keys.rs:500-503`) | F1, F3, Esc, F4 | none; F2, F5–F10 have no browser row in the viewer |

### 2.3 Collisions with plain `1`/`2`/`3`

1. **hex grid** — a plain digit is data (a nibble or an ASCII byte); its `Event::Text` is not consumed,
   and its `Event::Key` is not consumed either (nothing in `hex.rs` takes `Num0..Num9`). A table row run
   after the pass (`browser.rs:2364`) would see the leftover key **and** the hex window would already have
   typed it: one press, two actions.
2. **image** — `1` is actual pixels and is consumed (`image.rs:3267`): a row after the pass never sees it.
   It is a documented, shipped key: README "`F` fits again, `1` is 100 %" (`README.md:288`); the image
   preview test lists it (`image/preview_tests.rs:92`). 01-spec and the key help do not name it
   (`grep` of `01-spec.md`, `help.rs`).
3. **text fields** — hex Go to (`hex.rs:3708-3718`), text Go to (`text.rs:4563-4568`), hex read-only path
   (`hex.rs:3528`), Save As path (`save_as.rs:689`), the image block size's value box (`image.rs:3305`,
   `[추정]` egui's slider value turns into a text field when edited; `image.rs:3250-3252` treats "the block
   size" as a field). Digits belong to the field there (04 §3 rule 4).
4. **Cmd/Ctrl+1, +2, +3** are F1, F2, F3 (04 §6.1; `mod.rs:815-894`; `keys.rs:1011-1025` forbids any row
   binding Cmd+digit). So the view keys cannot be Cmd/Ctrl+digits.
5. **The list context** binds every non-space typed character as a jump letter (`keys.rs:497`,
   `:640-650`) — inactive while a viewer is up (`browser.rs:2189`).
6. Media and text windows take no digit: no collision there.

### 2.4 Event shapes that decide the fallback

- egui-winit 0.37.0 (`Cargo.lock:1177-1178`) pushes `Event::Key { key: logical.or(physical), physical_key,
  modifiers }` (egui-winit 0.37.0 `src/lib.rs:1128-1158`, read from the cargo registry copy) and then `Event::Text(text)` for printable text **unless ctrl/command/mac_cmd is held** (`:1164-1180`). So
  Shift+digit and Alt+digit still deliver typed text, which the hex ASCII column would read
  (`hex.rs:3690-3694`). macOS Option+1 typing `¡` is `[추정]` (layout knowledge, not measured here).
- The measured KDE leaves Ctrl+digits and **Alt+digits** unbound; Meta+digits are bound
  (`.prd/research/keys.md:93-94`). Esc+digit is unusable: Esc stops a fetch and leaves a viewer
  (`keys.md:264`).
- **Two focus predicates are in use, and they differ in the hex window.** The shell hook (`mod.rs:787`),
  the browser's withholding (`browser.rs:2354`) and PR #61's Esc rule use `text_edit_focused`; `plain_key`
  and `fkey_alone` (`mod.rs:1880-1888`) use `egui_wants_keyboard_input`, which is true while the hex
  grid holds focus (`grid_id`, `hex.rs:1921-1923`). A view-key rule must name `text_edit_focused` (fields)
  plus a window-declared "digits are data here" — not `egui_wants_keyboard_input`, which would switch the
  keys off in every hex view.
- `testkit::Driver::key` sends only `Event::Key`, `physical_key: None` (`src/gui/testkit.rs:240-252`);
  `SOPEN_KEYS` plays either a key or a `text:` step, never both together (`browser.rs:2167-2180`). A test or
  receipt that does not send the Key+Text pair misses collision 1.

## 3. The text model's encoding assumptions

The text viewer is UTF-8 and LF throughout. Every site:

| # | Site | Assumption | What UTF-16/32 needs there |
|---|---|---|---|
| T1 | `kind.rs:157-165` | text = no NUL + valid UTF-8 | BOM'd UTF-16/32 (and a BOM-less pattern) must be allowed to be text, or such files never reach the text viewer |
| T2 | `text.rs:1604-1628` `Scan::feed` (the index pass, local and streamed) | a line ends at **any byte** `0x0A`; the next line starts at `i + 1` | match the LF **code unit** at an aligned offset. In UTF-16LE `\n` is `0a 00` (command output: `'\n'.encode('utf-16-le').hex() = 0a00`): today's next-line start lands on the `00` byte — an odd offset — and every later line decodes one byte off. 44 of the 11,172 Hangul syllables contain a `0x0A` byte in UTF-16LE (e.g. U+AC0A 갊 = `0a ac`, measured with python3), each a false line break. In UTF-16BE `\n` is `00 0a`: the line before keeps a stray `00`, and every unit with a `0x0A` byte breaks a line too — U+xx0A (갊 is `ac 0a` in BE, same output) and U+0Axx |
| T3 | `text.rs:733-747` (`adopt`), `:1757` | `last_byte == b'\n'`, `line_len` in bytes | last unit == LF unit |
| T4 | `text.rs:1068-1139` `read_lines`, `:1115` | `position(b'\n')`, `pos += i + 1` | unit search, aligned steps |
| T5 | `text.rs:1148-1228` `spans`, `:1197-1203` | LF byte search; CR = the byte before it is `b'\r'` | in UTF-16LE `\r\n` = `0d 00 0a 00`: the byte before `0a` is `00`, so CR is never found; CR is the unit before the LF unit |
| T6 | `text.rs:1296-1311` `newline_before` | `rposition(b'\n')`, reads backwards from any byte | backwards by aligned units |
| T7 | `text.rs:1315-1319` `grid_cut`; `:1993-2002` `cut_char_start` | a character cut at a piece end is found by UTF-8 lead/continuation bytes, up to 3 back | UTF-16: a high surrogate as the last unit is cut; a grid point on a low surrogate belongs to the pair before. UTF-32 / UCS-2 / one-byte encodings: no multi-unit character |
| T8 | `text.rs:1328-1369` `scan_span`, `:1331` `(start + 3) / GRID` | up to 3 bytes of a UTF-8 character before a grid point | per encoding: 2 bytes (a surrogate pair's first unit) or 0 |
| T9 | `text.rs:2006-2016` `shown` | trailing `b'\r'` popped as a byte; `String::from_utf8_lossy` | decode per encoding; CR unit |
| T10 | `src/gui/text/wrap.rs:47-80` `decode` | `from_utf8`, `Cell { ch, len: len_utf8 }` — the one place a row's characters and their byte lengths come from | the same `Cell` per encoding (len 1, 2 or 4) — the shape already fits |
| T11 | `wrap.rs:615-621, 648-653, 870-879, 1025-1032` | skip UTF-8 continuation bytes (`b & 0xC0 == 0x80`, at most 3) to reach a character start | the encoding's "character start at or after" |
| T12 | `wrap.rs:32-37` `RESTART` 16 KiB, `RESTART_BEHIND` 64 B, `RESTART_AHEAD` 128 B | byte windows sized for UTF-8 | still bytes; 128 B is 32 UTF-32 characters (enough for a cluster `[추정]`) |
| T13 | `wrap.rs:954-965` `chars_before` | highlight colours are per character, counted by `decode` | consistent as long as `decode` and the highlighter read the same decoded text |
| T14 | `text.rs:2144-2156` `syntax_of` | first line text from `read_lines` | decoded; a BOM must not be the first character (a UTF-8 BOM today is `U+FEFF` before `#!` — `[추정]` the shebang match then fails) |
| T15 | `text.rs:2684-2700` highlight `run(line: &str)` | syntect on decoded `&str` | unchanged once lines are decoded |
| T16 | `text.rs:113-150` constants | `SPAN` 64 KiB, `CHUNK` 16 KiB, `GRID` 16 KiB, `LINE_MAX` 4096, `HIGHLIGHT_MAX` 16 MiB, `PASS_READ` 1 MiB (`:1600`) | all multiples of 4: reads that start aligned stay aligned |
| T17 | `text.rs:5230-5250` `embed_scripted(… at …)`, `Jump::Byte(at)`; Go to `0x…` (`text.rs:4566`) | any byte offset | align down to the unit (and off a low surrogate) |
| T18 | `text.rs:4428-4455` the bar | `N lines · size · syntax` | room for the encoding |

The 16 MiB vs larger split (06 §7.2): ≤ 16 MiB through a Source is indexed whole by the background pass
(`text.rs:434-445` `TextDoc::streamed`; `06-streaming.md:603-613`); larger streams are read by byte position past the
indexed prefix (`text.rs:77-92` module doc; `piece_holding` `:1374`, `bring` `:1501`); a local file keeps
its whole-file pass before the first screen (`06-streaming.md:614`; `prepare.rs:271-279`). All three paths go
through T2–T11, so the encoding change is one change for all three.

Already in the tree: `chrome::cp437` (`src/gui/chrome.rs:639-647`, over `CP437_LOW`/`CP437_HIGH`,
`:621-625`: the full code page 437) and `shown_char` (`:650-655`), which draws C0 controls and DEL as
their CP437 glyphs. No encoding crate is in `Cargo.lock` (`grep '^name = "encoding' Cargo.lock` →
nothing); UTF-16/32 decoding is in `std`.

**Where a default encoding can be picked.** A file whose extension is in the text table is never
sniffed (`kind.rs:238-240`: `by_extension` returns before `read_head`), so a BOM test in `kind.rs`
reaches only extension-less files. The text model reads the first block before the first screen on
every path (06 §7.2 "the first screen from byte 0 comes first", `06-streaming.md:579-580`; a local file
reads it in its index pass), and the syntax is already picked there ("by first line when the first
block arrives", `06-streaming.md:569-570`). The encoding pick belongs at the same point, and for a
local file before its index pass (the marks depend on it, T2).

## 4. Edit state when a window closes or is replaced

| Window | Close asks | Journal on close | Where |
|---|---|---|---|
| hex | `close_requested`: nothing unsaved → closes; a Save running → closes after it; else the prompt (Save · Save As… · Discard · Cancel, 07 §10) | `Drop` → `settle_journal` (written once more and kept, or removed when nothing is unsaved / Discard) | `hex.rs:4672-4715`, `:1884-1902`, `:2855-` |
| image | same rule; a queued or running save closes after it | `Drop`: Discard pending → removed when written; a scripted run keeps nothing | `image.rs:3839-3880`, `:2101-2121` |
| media | same; a failed open still as opened goes without asking | `Drop` → `settle_journal` | `media.rs:5010-5050`, `:4722-4743`, `:3061-` |
| text | nothing to ask (read-only) | none | `text.rs` has no `close_requested` override |

- The browser asks before it drops: `leave` → `close_requested` (`browser.rs:1726-1736`); the window shows
  its own prompt and asks again through `request_close` (`mod.rs:697-708`, `browser.rs:2338-2350`).
  `drop_viewer` hands the window's `settle` to `leftover` so a write still running is waited for
  (`browser.rs:1739-1748`).
- **Nothing replaces a dirty window today**: `view` asserts no viewer is replaced unasked (`browser.rs:1713`);
  the image give-way can only happen before the image is whole (§1.3).
- **Journals are per editing session and per kind.** Kind = hex, image, media (`journal.rs:231-237`). A
  journal's lock is `File::try_lock` on an open of its own (`journal.rs:203-224`). The listing skips a
  journal whose lock is taken — `Ok(None) => continue`, "a live session's" (`journal.rs:1596-1600`) — and
  the viewers' open path retries a taken lock for up to 500 ms first (`LOCK_RETRY`, `src/gui/hex/file.rs:1811, 1843`; 07:151-158). So a second view of the
  same file in the same process does not see the first view's live journal: it waits up to 500 ms, then
  lists nothing of it. Only a dead session's journal of another kind gets the "named, never applied" row
  (`journal.rs:2001-2002`, `image.rs:1929`; 07:210-211). "One window keeps one journal" (07:193-195).
  The hex window lists at its first pass as a viewer, never as a preview or at a revision
  (`hex.rs:2961-2977`).
- **Journal ticks and save polling run only inside `ui()`**: hex writes a generation on focus loss in its
  pass (`hex.rs:4507-4510`) and polls its job and journal at the end of its pass (`:4595-4597`). A window
  that is held but not drawn runs neither. `set_shown(false)` today only stops the stream's polling
  (`hex.rs:4665-4670`, `text.rs:5158-5163`).
- **The media window does not override `set_shown`** (`grep -n "fn set_shown"` finds it in `hex.rs`,
  `text.rs` and `image.rs:4982`, not in `media.rs`), and playback runs on a `Player` of its own
  (`media.rs:1724`, `play`/`pause` `:2259`, `:2306`): a media window held without being drawn keeps
  playing `[추정: the player thread does not depend on ui(); not run]`.
- **`LocalFs` can be a `Ranged`** (`src/remote/local.rs:251-260` → `ranged::local(path)`), so a local file
  can be read through a `Source` too `[추정: no route does so today; not run]`.
- **A Source is shared state across clones**: one visibility flag (`src/stream/source.rs:1411-1415`), one
  reload (`:1396`), one change state (06 §5, `06-streaming.md:266-330`). A Save's rename replaces the file
  under every other reader of it (07 §4.2).
- **Channels** (06 §6, `06-streaming.md:401-424`): a browser viewer holds 1 channel (its `Ranged`), 2 from
  its first Save, Save copy, Save As or Export (its `Origin` connects on first use); the window's total is
  8–9 of sshd's default 10, and "one more sopen window on the same host can therefore reach the limit".
  Each window owns its own `Origin` (`prepare.rs:118-126`, `:147-153`), so several live views of one file
  count one `Origin` each once they save — and a second stream per view one more `Ranged`.
- **An empty file opens in hex** (`hex/tests.rs:1314` `an_empty_file_opens`); empty media is an error
  (`media.rs:187`).

### 4.1 Position, selection and zoom per window

| Window | Position | Selection | Zoom / other view state | Entry point that sets a position |
|---|---|---|---|---|
| text | a byte offset: the top line's start and the byte of it the top row starts at (06 §7.2; `text.rs:20-22`) | none (`grep -i 'selection\|clipboard' text.rs` finds only doc words) | wrap on/off (`text.rs:3979-3987`) | `Jump::Byte(u64)` (`text.rs:3101-3111`), used by `at=` (`text.rs:5244-5246`) |
| hex | the cursor's byte offset and column (`Cursor { at, pane, low }`) | none (the only "selection" in `hex.rs` is egui's visuals colour, `:4138-4141`) | the typing column | `place(at, pane)` (`hex.rs:2055-2062`); `SOPEN_STATE at=` |
| image | — | the rectangle (`sel`, `image.rs:3261`) | `View { scale, center }` (`image.rs:1836-1842`): `F` fit, `1` actual pixels | none |
| media | the playhead `marks.t` (`media.rs:3973-3981`) | `sel` and the marks (`media.rs:3954-3958`) | In/Out, clips | `SOPEN_STATE t=` (AGENTS.md:183) |

## 5. The rows a viewer is drawn with

| Row | Who draws | Where | Content today |
|---|---|---|---|
| title row (cyan, top) | the browser | `browser.rs:2793` `chrome::title_panel(ui, …, &self.title)`; `chrome.rs:713-761` | ` sopen ` red block, the place (fitted from the left, hover shows it whole), `│` clock at the right end |
| F-key row (bottom) | the browser | `browser.rs:2780-2794`: `merged(keys::row(Context::Viewer, mode), window.keys())` | 1 Help, 3 Exit, 4 Edit + the window's own: hex 5 Save 6 Goto 7 SaveAs 8 Asc/hx (`chrome.rs:154-168`); image 5 Save 7 SaveAs (`:172-178`); media 5 Save 7 SaveAs (`:182-188`); text 2 Wrap/Unwrap (`:192-201`). Drawn **before** the viewer's pass, so it is the previous pass's `keys()` |
| a hosted window's own F-key row | nobody | `mod.rs:723-727` returns at once while hosted | — |
| the window's frame | the window | `chrome::frame(painter, rect, TITLE, active)`: `text.rs:4660`, `hex.rs:3964`, `image.rs:3607, 4856`, `media.rs:4468, 4669, 4898` | the window's name in the top border |
| text top bar | text window | `text.rs:4428-4455` | path · `N lines · size · syntax` (or `lines known to …`) |
| status rows | each window; the browser's own below | `text.rs:4503-`, `hex.rs:3885`, `image.rs:3474`; `browser.rs:2796-2798` | place, messages |

- **The F-key row's digits already mean three things**: F-n, Cmd/Ctrl+n, and a click on slot n
  (04 §6.1; `chrome.rs:802-806`, `mod.rs:757-760`). Labels `1 2 3` for the view keys there would read as
  F1 (Help), F2, F3 (Exit).
- **Tension (docs vs code):** 04 §2 says a viewer's slots "2, 7, 9, 10 are empty" (`04-file-keys.md:52-53`)
  and K-R1 says the hex row reads `1Help 3Exit 4Edit 5Save 6Goto 8Asc/hx` (`:493-494`); the code has F7
  SaveAs in hex/image/media (`chrome.rs:146-168`, ED3) and F2 Wrap in text (`chrome.rs:190-201`). 04 is
  stale on those slots. Free in every viewer today: 9 and 10; in text also 5–8.

## 6. Next breaks past the first fix (look-ahead)

1. Putting view rows in the browser's table, matched after the pass, double-acts in hex (§2.3 item 1)
   and never fires in the image (item 2). The match has to come before the window's pass, or the windows
   have to consume what they type.
2. A switch made by dropping the old window (today's give-way) prompts or loses edits; a switch that keeps
   the old window hidden stops its journal ticks and save polling (§4).
3. A second view that opens its own stream adds a channel to a budget already at 8–9 of 10; one that shares a stream shares its
   visibility and reload (§4).
4. A view switch on a handed-over image has no stream to share (§1.4).
5. Choosing UTF-16 in the text view without T2–T11 shows a misaligned file after the first line; with them
   but without T1 a UTF-16 file never reaches the text view by default.
6. Collision tests built on `Driver::key` or `SOPEN_KEYS` pass while the real keyboard double-acts (§2.4).

## 7. The same collisions in a terminal (scope note, 2026-11-24)

xtools may become a terminal UI with the GUI drawing the same cell screen (coordinator, 2026-11-24). Nothing
below was measured; it is terminal convention, marked `[추정]` throughout, and each line is a receipt to take
before a key is fixed.

| Collision (§2.3) | In the egui window (traced) | In a terminal `[추정]` |
|---|---|---|
| hex grid: a digit is data | Key + Text pair; the window reads the Text (`hex.rs:3650-3659`) | the same: the terminal sends the character `1`; there is no separate key event and no physical key, so "the key printed 1" cannot be told from the character `1` (an AZERTY owner needs Shift, as for typing a 1 anywhere) |
| Cmd/Ctrl+1/2/3 = F1/F2/F3 (04 §6.1) | turned into F-keys in the shell hook (`mod.rs:815-894`) | Cmd never reaches a terminal program (the terminal app takes it). In the legacy encoding Ctrl+2 is NUL, **Ctrl+3 is ESC** (would leave the viewer), Ctrl+4…8 are FS GS RS US DEL, and Ctrl+1, Ctrl+9, Ctrl+0 send the plain digit or nothing — so Ctrl+1 would read as a plain `1`, a view switch. Only with the kitty keyboard protocol or xterm's modifyOtherKeys are they distinct |
| Alt+digit as a fallback | Key with Alt + its typed text (egui-winit `lib.rs:1164-1180`) | macOS terminals compose by default (Option+1 types `¡`) unless Option-as-Meta is set; with Meta it arrives as ESC then `1`, the same bytes as Esc followed by 1 (only an escape timeout tells them apart) |
| F-keys (a non-text fallback) | free in every viewer: slots 9, 10 (§5) | sent as escape sequences in every terminal; on a Mac the top row sends media keys unless fn is held or the terminal maps them (message 19's subject) |
| image `1` = actual pixels | consumed by the image window (`image.rs:3267`) | an image is shown only where the screen can draw one (GUI, graphics-capable terminals): the collision exists only there |
| text fields own digits | `text_edit_focused` (§2.4) | the same rule, on the TUI's own fields |
| IME | egui-winit filters keys the IME processed (`lib.rs:663-706`) | the terminal composes; committed text arrives as characters |
