---
title: sopen — xtools's built-in editor
type: spec
tags: [sopen, spec, xtools, editor]
---

# 12 — xtools's editor: Turbo C in the preview's place, Dir and Git beside it

Status: planned (2026-11-25; review round 1 applied the same day). Nothing here is built. The model
units (§16: ED-0, ED-buf, ED-enc, ED-ops, ED-save) have no screen and can start now; the screen units
wait for 10's steps.

Answers item 3 of [`xtools/ssot.md`](xtools/ssot.md) (owner message 21), written in cells on 10's
architecture ([`10-tui.md`](10-tui.md): one grid, two backends; the editor is its step X5, 10 §16). The
evidence is XR-E's note, [`research/editor.md`](research/editor.md), cited *editor* §n: its period
manuals and screenshots, prototypes and receipts are there. Code citations are at 77b0beb, this
branch's base and the note's commit. **[M]** measured in the note · **[S]** read in source · **[추정]**
inferred, not measured · **[owner]** only the owner's terminals or hands can answer it.

Decisions are E1–E40 (§12), all reversible.
- The orchestrator's of 2026-11-25 accept the note's recommendations and fix its keys, limit,
  routing, encoding and conflict rules (E1–E4, E11–E13, E18, E21, E22, E26, E27, E29). Review round
  1 added three more (E31–E33).
- At the orchestrator's request this document settles the key hazards the note measured (E5–E9) and
  the note's four open points (E14, E15, E19, E20).
- The rest are this document's; E34–E40 come from review round 1.

Owner message 21 (2026-11-24), verbatim:

> 그리고 에디터도 디폴트로 로컬 에디터에 git graph쓸수 있도록 해줘 이거 좀 생각해서 지금 이 90년대 취향으로 pctools/터보 c 에디터 감성으로 간단한 텍스트 에디터 + git graph로 깃작업할수 있는 에디터로 만들어줘. 신택스 하일라이트랑 디렉토리 뷰어가 있어야함. 아니 프리뷰를 보다가 f4를 누르면 디렉토리 패널 자체를 왼쪽으로 밀고 프리뷰 패널이 그대로 에디터가 되도록해주곻 왼쪽 디렉토리 뷰에 탭을 추가해서 디렉토리/git뷰를 전환할 수 있도록해줘. 이거 좀 생각해서 리서치하고 구현해줘

What it amends in 01, 02, 03, 04, 07, 08, 10, the README and AGENTS.md, by which unit: §13.

## 1. The contract

- **F4 on a text file opens xtools's editor in place** (E2). The directory panel narrows into a
  column at the left edge (30 % of the width, 20–40 cells), and the editor takes the preview's place,
  on the same file, place and colours.
  - The column has two tabs: **Dir** (the panel) and **Git** (03's git panel of the file's
    repository). Tab switches them.
  - F6 moves the keyboard between column and editor; Esc or F3 goes back to where F4 was pressed.
- **Shift+F4 is today's F4** (VS Code / Zed, 01 A2) wherever F4 is (E3).
- **The look** is xtools's skin with Turbo C++ 3.0's edit window (E1, §4). Every part is cells, the
  same in both backends (10 §5).
- **The keys** are a Turbo / WordStar set that holds in a terminal (E4, §5). **A Save starts only from
  a key the owner means as Save** (E5, E31, §5.3): F2 or F5 pressed as itself or clicked on the row;
  Ctrl/⌘+S in the window, and in a terminal once Shift is known to arrive (else Ctrl+S, then F2);
  Ctrl+K S; the File menu; a prompt's Save button. Never from an F-key made out of a digit.
- **The buffer is a piece table over the file's own bytes** (E11, §7). Bytes not edited are never
  decoded or rewritten. The file keeps its encoding, BOM and line endings; a character the encoding
  cannot hold is refused, never replaced. Files up to 256 MiB (E13).
- **Highlighting is the viewer's syntect** (E18, §8), so F4 changes no colour.
- **Save is the image window's route, guarded** (E21, E32, §9): backup, stage, then a replace that a
  version changed in the meantime cannot slip through unseen — an atomic exchange checked before it
  stands for a local file; for a remote one today's check before the rename, and a hard-link check
  of what the rename replaced.
  - A change found before anything is replaced is never written over: the owner chooses Reload,
    Overwrite or Save As (E22, E23). A change that reached a remote file only in the moment before
    the rename is kept beside it as `<name>.sopen-replaced-<stamp>`, and the result names it.
  - Leaving with unsaved edits asks 07 §10's four choices (E26), each prompt opening on Cancel (E6).
- **Git work is 03's panel** in the Git tab, plus a Git menu on the edited file (E27, §10).
- **`sopen file.txt`** in a terminal opens xtools with the editor, where a folder would open xtools,
  and ends when the editor closes, so `EDITOR=sopen` works (E29, E33). Pipes and scripts keep
  VS Code / Zed (§11).

## 2. Scope

- **In**: the editor screen in cells (§4); its keys in the window and in a terminal under both key
  encodings (§5); the F4 layout and the column's tabs (§6); the buffer, encodings and undo (§7);
  highlighting (§8); Save, conflicts, the journal and the close prompt (§9); git from the editor
  (§10); routing (§11).
- **Not in**:
  - a full IDE: projects, build, debugger, language servers (ssot "Not in scope");
  - blame, an inline diff gutter, a merge editor (*editor* §6);
  - files above 256 MiB. Editing through the stream needs a content check of its own, designed
    only if asked (*editor* §3.5);
  - a new file (Save As writes one; there is no New), and saving in another encoding;
  - word wrap while editing (E16);
  - markers and the WordStar Ctrl+Q and Ctrl+O prefixes (E4);
  - Windows (01 §9).

## 3. Words

**The editor**: the Text view in its editing state (*editor* §7.1), drawn as Turbo's edit window.
**The column**: the left column of the F4 layout; its **tabs**, Dir and Git. **The original**: the
file's bytes as read at F4, never changed. **The add buffer**: the bytes typed or pasted, encoded,
appended. **A piece**: a range of one of the two. **The baseline**: what a Save checks the file
against — its size, mtime and SHA-256, and for a local file its device and inode. **A digit alias**:
an F-key the shell makes from Cmd/Ctrl+digit (04 §6.1) or, in the TUI, from Alt+digit (10 T7). **Shift
known**: Shift is known to reach xtools together with Ctrl/⌘ — always in the window; in a terminal
once a Ctrl/⌘+Shift chord on a letter key has arrived with Shift since the terminal's last focus
report or resize (E31).

## 4. The look (E1)

*editor* §1.3 sources every row. *editor* §2.3 has the mock-ups in cells: 80×25 before and after F4,
the Git tab, the File menu, and 160×40.

| Part | In cells |
|---|---|
| Title row | ` xtools ` red block, then `File  Edit  Search  Block  Git  Options` black on cyan, each name's letter red (PC Tools Desktop's menu bar, Turbo C++'s menus); then 08 §8's strip (`1 Text·2 Hex·3 Raw`) and the clock |
| Edit window | double frame while it has the keyboard, single while the column has it. Top border `═[■]═`, the name centred, `═1═[↕]═`; bottom border `* 13:27` (modified mark, line:column). `[■]` and `[↕]` light green |
| Text | the skin's `syntax_theme` on blue: the preview's and the viewer's colours |
| Gutter | 5 cells, dim line numbers, as the preview has |
| Block | fg BLUE on bg CYAN (Turbo C++ 3.0's marked text), never the list's light-red bar |
| Caret | the terminal's cursor in the TUI; in the window two scan lines (insert) or a whole cell (overwrite) [추정 choice] |
| Status row | `Insert Indent` (a word absent = that mode off), the syntax, the encoding with 08's `(BOM)` / `(guessed)`, the line ending, the git letter (§10), 07's `unsaved — kept`. Find, Go to, `^K` and `^S` use this row as fields and prompts, as Turbo C 2.0's Find did |
| F-key row | `1Help 2Save 3Exit 4View 5Save 6Panel 7SaveAs 8 9View 10Menu` |

- **Rows at 80×25**: title 1 · panels 22 · status 1 · F-keys 1. No menu bar or hint row is added: the
  title row holds the menu names and the F-key row is the hint line.
- **Widths** [M *editor* §2.3]: at 80 columns the column takes 24 cells and the editor 56, of which 49
  are text (the frame, the gutter and the scroll bar take the rest). At 160 columns: 40, 120 and 113.
- **Text is not wrapped** while editing (E16), as in Turbo C's editor (TC2-UG p. 91): one row per
  line, scrolled sideways to keep the caret in view. The viewer's wrap (01 A11) returns when editing
  ends.
- **The menus** open with F10 or a click on a name, as cells dialogs with the DOS shadow (10 §6). A
  letter chooses an item. Every command is in a menu, so a key a terminal cannot send loses nothing
  (10 T17's pattern).
  - **File**: Save F2 · Save as… F7 · Save copy · Reload Ctrl/⌘+R · VS Code / Zed Shift+F4 · Exit F3.
  - **Edit**: Undo · Redo · Cut · Copy · Paste (the window only) · Select all · Delete line · Delete
    word · Insert line · Delete to line end.
  - **Search**: Find… · Replace… · Find again · Go to line… · Matching bracket.
  - **Block**: Begin · End · Copy · Move · Delete · Hide · Indent · Unindent · Mark line · Mark word.
  - **Git**: Stage this file · Unstage · Commit… · Diff against the index · Show in Git (§10).
  - **Options**: Insert / Overwrite · Autoindent · Column (hides or shows the column, as `[↕]` does).

## 5. Keys

### 5.1 The table (E4)

*editor* §1.4 sent each key through crossterm 0.29's parser and sopen's translator, in both
encodings, and cites Turbo's and PC Tools' meaning of each. This table is the result.
"Ctrl/⌘" is egui's `command`: Ctrl in a terminal and on Linux, ⌘ in a Mac window (10 T26).

| Key | The editor |
|---|---|
| ← → ↑ ↓ · Home End · PgUp PgDn · Ctrl+← / → | a character (a CR LF pair is one, E35), line start / end, a page; a word |
| Ctrl+Home / End · Ctrl+PgUp / PgDn | the file's start (after a BOM, §7.2) / end |
| Shift + any of those | extend the block |
| Ins | Insert ↔ Overwrite (Overwrite never replaces a line ending, E35) |
| Enter | a new line, indented as the line above while `Indent` is on |
| Tab · Shift+Tab | a tab or spaces (§7.2) · unindent the line or the block |
| Del · Backspace | delete right · left; with a block shown, delete the block |
| Ctrl+Y · Ctrl+T · Ctrl+N | delete the line · the word right · insert a line |
| Ctrl+K, then B K C V Y H I U L T S | block begin · end · copy · move · delete · hide · indent · unindent · mark line · mark word · Save (§5.4) |
| Ctrl+Ins · Shift+Del · Shift+Ins · Ctrl+Del | copy · cut · paste · delete the block |
| Ctrl/⌘+C · X · V | copy · cut · paste (in a terminal: §5.3) |
| Ctrl/⌘+S · Shift+Ctrl/⌘+S · Alt+Ctrl/⌘+S | Save (in a terminal until Shift is known it arms, §5.4) · Save copy (07 §6) · Save As… (07 §7) |
| Ctrl/⌘+Z, Alt+Backspace · Ctrl/⌘+Shift+Z, Shift+Alt+Backspace | Undo · Redo |
| Ctrl/⌘+F · Ctrl+L | Find (Tab in it moves to `Replace:`), options U W B N G as Turbo C 2.0's letters (TC2-RG p. 418–419) · find again |
| Ctrl/⌘+G | Go to line (the viewer's field) |
| Ctrl/⌘+A · Ctrl/⌘+R | select all · read the file again (asks first, §9.6) |
| F1 | Help: the editor's keys, §5.3's terminal lines among them |
| F2 · F5 | Save |
| F3 · Esc | leave the editor (§6.4); Esc first closes the innermost menu, field, prompt or arming |
| F4 · Shift+F4 | the Enter viewer on this file, at the caret (§6.4) · VS Code / Zed (E3) |
| F6 | the keyboard to the column (showing it first when hidden), and back |
| F7 | Save As… |
| F8 | empty: the encoding is fixed while editing (§7.2) |
| F9 | arms 08's view keys; a view chosen leaves the editor (§6.4) |
| F10 | Menu |
| F12 · Alt+G in a terminal | the switch (10 §7) |
| Ctrl/⌘+Q · Ctrl/⌘+W | sopen's close (§5.3) |
| a click · a drag · the wheel | the caret · a block · scrolling. `[■]` = F3; `[↕]` = the column off or on; a click in the column moves the keyboard there; a tab's name switches to it; the F-key row's slots act |

- **Not taken** (E4): the WordStar Ctrl+Q prefix and Ctrl+W (sopen's close), the cursor diamond and
  Ctrl+E / S / D / X / R / C / V / H / P, the Ctrl+O prefix, Alt+[ / Alt+], Alt+letter menus, Alt+X,
  Alt+F3. The cursor keys, Ins, Backspace and the menus cover them (*editor* §1.4).
- **In a Mac window** ⌃+letter arrives without `command`. The WordStar rows (Ctrl+K, Y, T, N, L)
  match ⌃ there too, so a Turbo hand works on a Mac keyboard; every other row is Ctrl/⌘ [추정 choice,
  *editor* §1.4].
- **The rows are 04's one table** (`src/gui/browser/keys.rs`, 04 §3). They go in a new context,
  `Editor`, beside `List`, `Viewer`, `Git`, `Tree` and `Dialog` (`keys.rs:70-81`). Modifiers match
  exactly (04 §3 rule 1), and `no_duplicate_bindings` (`keys.rs:995`) covers the new rows.

### 5.2 What a terminal does to these keys

Measured [M *editor* §1.4, its `keys-table.tsv`]: every key of §5.1 decodes alike under xterm's legacy
encoding and under the kitty protocol, except the following. §5.3 settles each.

- **Shift is lost with Ctrl under legacy encoding.** Ctrl+Shift+S arrives as Ctrl+S and Ctrl+Shift+Z
  as Ctrl+Z; Shift+Alt+Backspace arrives as Alt+Backspace. Pressing either twice, or holding it, sends
  the same byte again, so nothing after it can tell the two apart.
- **Some control bytes become digits.** Ctrl+] (0x1D) decodes as Ctrl+5, Ctrl+\ (0x1C) as Ctrl+4
  and Ctrl+/ (0x1F) as Ctrl+7 [M]; Ctrl+^ (0x1E) as Ctrl+6 is the same rule, which 10 §9 measured
  as "Ctrl+4…7 only" [M spike]. The shell's hook then turns each Ctrl+digit into an F-key
  (`fkey_events`, `gui/mod.rs:817-896`, the alias at `:874`; `key_aliases` once XW-shell has renamed
  it), so Ctrl+] is F5.
- **Ctrl+Q and Ctrl+W are a close.** They arrive as Cmd/Ctrl+Q and +W, which `closing()` turns into a
  close request (`gui/mod.rs:1180-1187`).
- **Ctrl+V gives nothing in a terminal.** The translator drops it (`term_input.rs:236`), and the
  terminal's own paste arrives as one bracketed `Paste`, with its CRLFs and lone CRs turned into LFs
  (`term_input.rs:18-20`).
- **Esc then a digit read in one input is Alt+digit**, which the TUI makes an F-key (10 T7). 10 §9
  lists this as a legacy residual [추정 10 §9].
- **Other legacy losses**: Ctrl+Backspace arrives as Ctrl+H; Ctrl+I and Ctrl+M arrive as Tab and
  Enter; Shift+F3 and Alt+F3 arrive as nothing.

### 5.3 The hazards, settled (E5–E9, E31, E34)

**The rule.** Saving is the only write to the file that a key starts, and it starts from these and
nothing else:

1. F2 or F5 pressed as a key, or `2Save` / `5Save` clicked on the F-key row — also while Ctrl+K
   waits. While `^S` waits only F2 or a click on its Save confirms, and F5 cancels (E31, E34).
2. Ctrl/⌘+S in the window, and in a terminal once Shift is known (§3); before that, Ctrl+S then F2
   (§5.4, E31).
3. Ctrl+K, then S: WordStar's "Ctrl-K S or F2" (TC2-RG p. 413, *editor* §1.4).
4. The File menu's Save; the Save button of the close prompt (§9.6); Overwrite in the conflict dialog
   (§9.2). These are buttons, which no letter and no first Enter presses (E6).

Save copy (Shift+Ctrl/⌘+S once Shift is known, C after `^S`, or the menu) and Save As… (F7,
Alt+Ctrl/⌘+S, or the menu) never write over the file (07 §6, §7).

| Pressed | Arrives as: legacy · kitty · window | The editor | Decision |
|---|---|---|---|
| Ctrl+] (also Ctrl+\, Ctrl+^, Ctrl+/) | F5 (F4, F6, F7) by the hook · itself · itself | **No digit becomes F2 or F5 while the editor has the keyboard**: Ctrl+] arrives as Ctrl+5, which the editor answers with `this key arrived as Ctrl+5 (Ctrl+] in some terminals) and saved nothing — F2, F5, Ctrl+K S, a click on 2Save, or F10 F S saves`. A Mac terminal's top row may need fn for F2 and F5, so the line names three keys that need none. F4, F6 and F7 from a digit act: back to viewing (asks first when unsaved), the column, the Save As dialog. None of those writes | E5 |
| Cmd/Ctrl+2, +5; Alt+2, +5 (TUI); Esc then 2 or 5 in one read | the digit, not F2 / F5 | as above: no Save, the line | E5 |
| Ctrl+Shift+S | Ctrl+S · itself · itself | in a terminal until Shift is known, Ctrl+S **arms** `^S`: only F2 or the prompt's Save confirms, C saves a copy, any other key cancels untyped (§5.4) | E31 |
| Ctrl+Shift+Z, Shift+Alt+Backspace | Undo · Redo · Redo | under legacy, Undo; Redo is the Edit menu (F10 E R). The first Ctrl+Z of a terminal session before Shift is known says `Undo — this terminal may send Ctrl+Shift+Z as Ctrl+Z; Redo: F10 E R` | E7 |
| Ctrl+Q, Ctrl+W | a close, in every backend | sopen's close. With unsaved edits the editor refuses it and shows the close prompt, on Cancel (§9.6) | E8 |
| Ctrl+V, Shift+Ins | the key with no `Paste` (E9) · the same · paste | in a terminal the status row says `the terminal's paste: ⌘V / Ctrl+Shift+V`; that paste arrives as one `Paste`, one undo step (§7.2) | E9 |
| a key with the Korean input source on | jamo text, or nothing while the input method composes | §5.5 | E10 |

**How it is enforced: two rules for keys, one for buttons.** Each needs no screen to test.

- **The hook (E5)**: while the editor has the keyboard, the shell's hook makes no F2 or F5 out of a
  digit. It decides at each press, so the rule is per event:
  - the press stays the digit, with its modifiers, and types nothing (Ctrl, Alt or ⌘ is held);
  - a digit already held as F2 or F5 when the editor took the keyboard — the hook keeps a held
    digit's meaning through its repeats (`gui/mod.rs:769-780`) — has its further repeats dropped;
  - a real F5 or a row click in the same read is untouched; every other digit alias (F1, F3, F4,
    F6–F10) is made as today.
  - The editor tells the hook each pass, through the context, as a field's focus tells it today
    (`typing`, `gui/mod.rs:788-789`). F4 and 0x1D in one read therefore reach the list, where the
    alias is F5 Copy's dialog (04 §5.2), not a Save; EK-3 plays it.
  - XW-shell's Alt+digit aliases (10 §17) follow the same rule.
  - Why not a mark on the alias: the hook's per-pass data (`chrome::take_clicked`'s way,
    `gui/mod.rs:790`) cannot tell two F5s of one read apart, and the event has no free field —
    `physical_key` is what 08 §8's view keys read first. Why not "no aliases in the editor": the
    hook's `typing` flag also drops row clicks (`gui/mod.rs:892`), and the TUI's Alt+6 and Alt+0 are
    the column's and the menu's F-keys where the top row sends media keys (10 §9).
- **`edit::arm::decide`** (ED-ops, `src/edit/arm.rs`) maps the armed state, the press, Shift known and
  the backend to an editor command. Every Save a key starts comes out of it (E31, E34); its table
  test is EK-1.
- **The focus rule** (E6, §9.6): every prompt that holds a Save or a Discard opens on Cancel; letters
  answer nothing.

### 5.4 Arming: Ctrl+K, and Ctrl+S in a terminal

- **Ctrl+K arms** as 08's F9 arms (08 D4, D33). The status row shows `^K`. The next key decides,
  with or without Ctrl; Esc or any other key disarms and is dropped; there is no timeout.
- **In a terminal, Ctrl+S arms `^S` until Shift is known** (E31). A legacy terminal sends Ctrl+Shift+S
  as Ctrl+S [M], and a second press of either sends the same byte, so a doubled Ctrl+S proves
  nothing; so does a letter, which may be the first of a word typed without looking. The status row
  says `Save over <name>? F2 or a click on [Save] saves · C saves a copy · any other key cancels`.
  - Only an unambiguous key confirms the overwrite: **F2** pressed as itself (never one made from a
    digit, E5; a click on the row's `2Save` is the same key) or a click on the prompt's `[Save]`.
  - **C** runs Save copy, which writes a new file and never over this one.
  - **Every other key cancels and is not typed**: letters (S among them), Enter, Space, F5, a second
    Ctrl+S. Each cancel says `not saved — F2, Ctrl+K S or a click saves`: a Mac terminal's top row may
    need fn for F2, and Ctrl+K S and the click need none.
  - **Shift known**: once the shell's hook has seen a Ctrl/⌘+Shift chord on a letter key arrive with
    Shift, Ctrl+S saves at once and Shift+Ctrl+S saves a copy. The translator passes Shift whenever
    the terminal sends it (`term_input.rs:134-145`), which the kitty protocol does. Only letter keys
    count: a legacy terminal sends Shift with Ctrl on the arrows too (`CSI 1;6A`) [추정, xterm's
    encoding].
  - **It is forgotten at every focus report and every resize** of the terminal (`WindowFocused`, which
    the translator makes of the terminal's focus events, and the pass's screen size changing). A
    tmux or herdr client that reattaches sends both, and the client now attached may drop Shift, so
    the `^S` prompt comes back until a chord proves Shift again.
  - In the window Shift always arrives, so Ctrl/⌘+S saves at once.
- **Precedence while armed** (E34): while `^K` waits, F2 or F5 (never one made from a digit, E5) and a
  click on `2Save` or `5Save` save, and disarm. While `^S` waits, only its own two confirmations above
  save. Every other key follows its arming's rule.
- Both armings are states of `edit::arm::decide`.

### 5.5 The Korean input source (E10)

- A terminal gives no physical key (10 §9). With the Korean input source on, the letter after Ctrl+K,
  `^S`'s C, a menu's letter and Find's option letters arrive as jamo — or not at all while the input
  method holds a composition.
- **In the window**: while an arming, a menu or the options field waits for a letter, the editor
  turns the input method off and matches the physical key, as 08 §8 matches view keys.
  - The editor does this by not setting `PlatformOutput::ime`: egui-winit 0.37 allows the input
    method only while it is set (`src/lib.rs:1257-1273` of that crate).
- **In the TUI**: a 2-set (두벌식) jamo matches the letter on its key: ㄴ = S, ㅊ = C, ㅕ = U, and so
  on.
  - Whether a terminal's input method sends a lone jamo at once, or holds it until the next key, is
    [owner, EO-2].
  - If it holds it, the `^K` and `^S` prompts add `under the Korean input source the letter arrives
    with the next key`.

## 6. The F4 layout (E2, E3, E39, E40)

### 6.1 Rule 1a of 02 §3.1

`view::resolve` (`src/gui/browser/view.rs:163`; the rules are in `target`, `:112-158`) gains one input
and one rule. The rule sits right after rule 1 (a viewer covers both sides, `:113`) and before
`git_wide` and rule 2 (the pin).

| Input | Meaning |
|---|---|
| `editor` | `None`, or `{ file, tab: Dir \| Git, focus: Text \| Column, column: shown \| hidden, wide: bool }` |

- **Rule 1a — the editor is open**: the central area is the column (left) and the editor (the rest).
  - The column shows the active panel's List (tab Dir) or `Git(top)` for the file's repository (tab
    Git). With `column: hidden` the editor takes the whole area; with `wide` (E40) the Git panel does.
  - The sides' own states, the pin, the mode, `tree_snapshot` and the other panel are kept, as
    `git_wide` keeps them (02 §3.1). They come back when the editor closes.
- **`git_wide` comes after it** (E40): F4 saves `git_wide` and clears it; leaving the editor restores
  it. Inside the editor, Ctrl/⌘+Shift+G in the Git tab toggles `editor.wide` — the git panel over
  the whole area, the editor kept underneath — which rule 1a reads.
- **The column's width**: 30 % of the columns, at least 20 cells and at most 40 — 24 of 80, 40 of 160
  [M *editor* §2.3].
  - Today `panels_ui` cuts the area at `floor(width / 2)` (`browser.rs:2420-2433`), and XW-grid will
    make that cut in cells (10 §17). Rule 1a is one more cut.

### 6.2 Where F4 comes from

*editor* §2.2's table, adopted, with two rows added. The last column is where Esc or F3 goes back to.

| Pressed | Opens | Esc or F3 |
|---|---|---|
| F4 in a list, the bar on a text file, preview mode, the preview on the right | the editor where the preview was; the list narrows to the column at the left | the list and the preview as they were |
| the same, the preview on the left (the right list is active) | the list moves to the left column and the editor takes the right: the column is always on the left ("왼쪽 디렉토리 뷰") | as they were |
| F4 in dual mode | the active list becomes the column; the editor replaces the other list | both lists |
| F4 before the bar has rested 150 ms (02 §3.1) | the editor on the bar's file, read as Enter reads it (`browser.rs:1772-1774`) | what the display commits |
| F4 in the Enter viewer, Text or Raw view | the column and the editor, at the viewer's place | the viewer, at the editor's place |
| F4 in the Enter viewer's Hex view | the editor at the hex cursor's byte; with unsaved hex edits, 07 §10's prompt first (§9.6), as F9 then 1 would ask (08 D2) | the viewer, on its Text view |
| F4 in the Enter viewer of a revision or a diff | the editor on the working file: today's F4 target, `Viewer.abs` (08 §6) | the viewer |
| F4 with the git panel pinned, or over both sides (`git_wide`) | as above; the Git tab shows that repository when the file is inside it (E40) | the pin, or `git_wide`, as it was |
| F4 on the git panel's uncommitted row, on a file | the editor on that file (03 §9.1 slot 4) | the git panel |
| F4 on a folder, or in the git panel on anything but a file | VS Code / Zed, as today (01 A11): the built-in editor edits files | — |
| F4 on a binary, an image or media | nothing. The status row says `<name> is not text — Enter opens its viewer (2 = Hex edits bytes), Shift+F4 opens VS Code / Zed` | — |
| F4 on a text file above 256 MiB | nothing. The status row names the size, the limit and Shift+F4 | — |
| Shift+F4 anywhere F4 is | VS Code / Zed: today's F4 (`browser.rs:1758-1770`) | — |

### 6.3 The column (E39)

- **Tabs**: `[Dir]─Git─` or `─Dir─[Git]` in its top border, the chosen tab in the bar's colours. Tab or
  a click switches them: this is 02 §3.2's Tab from a List into Git and back (03 §9.2), inside one
  column.
  - Not Alt+D or Alt+G: Alt+letter types a character in a Mac window, and Alt+G is the TUI's switch
    (10 T33).
  - Not Ctrl/⌘+digits: those are F-keys (04 §6.1). In the column they act as in the list; E5's rule
    holds only while the editor has the keyboard.
- **Every key**, by tab. "Asks" is §9.6's prompt, when the edited file has unsaved edits.

| Key | Dir tab | Git tab |
|---|---|---|
| F1 | the list's help, with the column's lines | the git panel's help, likewise |
| F2 | Rename; on the edited file refused: `editor.rs is open in the editor — leave it to rename it` | Reword (03 §9.1) |
| F3, Enter | a text file: edit it here (asks); a folder: enter it; any other file: leave the editor (asks) for its viewer | details (03 §6.1) |
| F4 · Shift+F4 | a text file: edit it here (asks); a folder: VS Code / Zed · VS Code / Zed | a file row: edit it here (asks); elsewhere VS Code / Zed · VS Code / Zed |
| F5 | empty: Copy goes to the other panel's folder (02 §7 `OpsTarget`), which is not shown | Pick (03) |
| F6 | the keyboard to the editor | the keyboard to the editor; Rebase stays in the F10 menu (03 §7) |
| F7 | Make folder | Branch, or Commit… on the uncommitted row (03) |
| F8, Del, Shift+F8, Shift+Del | Delete (with Shift: without the Trash, 04 §5.5); a selection holding the edited file opens on Cancel, Enter cancels, and the hint names it (§9.4) | 03's Delete; discarding the edited file changes it under the editor (§9.4) |
| F9 | empty: the mode belongs to the layout underneath and comes back with it | empty, likewise |
| F10 | empty: the tree belongs to a side, after the editor | Menu (03) |
| Tab, Shift+Tab | the Git tab | the Dir tab |
| Esc | stops a running fetch or file operation; else the keyboard to the editor | 03 §9.2's innermost first; then the keyboard to the editor |
| Ctrl/⌘+G | refused: `pin the git panel from the browser — the Git tab is here` | the same |
| Ctrl/⌘+Shift+G | — | `editor.wide` (E40) |
| Ctrl/⌘+U, Ctrl/⌘+← / → | empty: there is no other panel | — |
| ↑ ↓ PgUp PgDn Home End, Backspace, Space, Ins, `+ - *`, letters, Ctrl/⌘+A, Ctrl/⌘+R | as in the list (04 §3.1) | as in the git panel (03 §9.2) |

- **The F-key row** is the editor's while the editor has the keyboard. While the column has it, the
  row is the list's (slot 5 empty, slot 6 `Edit`, 9 and 10 empty) or the git panel's (slot 6 `Edit`,
  9 empty), as 04 §2's row changes with the context.
- **Outside a work tree** the Git tab says `not in a git work tree` in its body; Tab still switches.
- **Zoom**: a click on `[↕]`, or Options → Column, hides the column, and the editor takes the whole
  area. F6, or a click on `[↕]` again, shows it.

### 6.4 Leaving

- **Esc or F3** leaves the editor for where F4 was pressed (§6.2's last column).
- **F4** leaves it for the Enter viewer on this file, Text view, at the caret (08 §5's place rule). So
  F4 moves between viewing and editing a file.
- **Shift+F4** leaves it and then opens the file in VS Code / Zed (E3). One editor at a time, so the
  two never meet at the conflict check.
- **F9 then a view key** leaves it for the Enter viewer on that view (*editor* §7.1; 08 §5 D2).
- **Every exit asks first when there are unsaved edits** (§9.6): Esc, F3, F4, Shift+F4, F9 then
  another view, F3 / F4 / Enter on another file in the column, Ctrl/⌘+R, the window's close
  (Ctrl/⌘+Q and +W among them) and the switch (10 §7.4, T30).
- **Started as the editor** (E33, §11), Esc and F3 end xtools once the editor has closed.

## 7. The buffer (E11–E17, E35–E37)

### 7.1 A piece table over the file's bytes (E11)

- **The model**: the original is immutable and never decoded for storage. Typed and pasted text is
  encoded in the file's encoding (§7.2) and appended to the add buffer. The document is the list of
  pieces.
- **Lossless by construction**: over *editor* §3.2's seven fixtures the table kept every byte outside
  the edit, where decode → rope → encode changed 3 of the 7 [M].
- **Memory**: the file plus what was typed. 102.8 MiB peak for a 100 MB file, against ropey's
  225.2 MiB [M *editor* §3.1].
- **The first cut** keeps pieces in a `Vec`, with the original's line starts, and splits by binary
  search. Lines are found by the encoding's LF unit (`Encoding::find_lf`, `encoding.rs:326`).
  - The prototype's linear scan already stayed well within a frame after 20,000 edits on 100 MB:
    24 µs an edit and 8 µs a lookup, where a frame is 16,000 µs [M *editor* §3.1].
  - A balanced tree waits for a measured need.
- **Serialising** for Save writes the pieces in order: 97.9 ms for 100 MB [M].
- **Killed** (*editor* §3.1):
  - egui's `TextEdit`: not cells, and 1.09 ms a character at 100 MB;
  - xi-rope: discontinued;
  - ropey: it holds text, not bytes, so it is lossy, and it takes 2.25× the file.

### 7.2 Encoding, BOM, line endings, tabs, the clipboard (E12, E35–E37)

- **The encoding** is the one on screen at F4, the opening's one encoding (08 D21), with its
  `(BOM)` / `(guessed)` label on the status row.
  - It is fixed while editing: F8 is empty, and saving in another encoding is not offered.
- **The encode side** (ED-enc, `Encoding::encode`) turns typed and pasted text into bytes:
  - UTF-8 as it is;
  - UTF-16LE / BE by `encode_utf16`; UCS-2 refuses characters above U+FFFF;
  - UTF-32 by `u32`;
  - ASCII refuses characters above U+007F, Latin-1 above U+00FF;
  - CP437: ASCII below 0x80, the inverse of `chrome::cp437` (`chrome.rs:640`) above;
  - CP949: encoding_rs's EUC-KR encoder, after VM-4 adds encoding_rs (08 §13). What its
    `unmappable` flag reports is refused, never written as `&#…;`.
  - encoding_rs cannot write UTF-16: `UTF_16LE.encode("A한")` gave UTF-8 `41 ED 95 9C` [M *editor*
    §3.2]. So every Unicode encoding's encoder is sopen's own, as 08 §7.4 already has std decode them.
- **A character the encoding cannot hold is refused**, never replaced (*editor* §3.3).
  - Nothing is typed, and the status row says `한 cannot be written in Latin-1 — nothing was typed`.
  - A paste that holds one is refused whole, naming its first such character.
- **An encoding that may be wrong asks once** (E37). F4 on a file whose encoding is `(guessed)`
  (08 §7.2 steps 2–3), or whose bytes the encoding cannot all decode, says so on the status row:
  `read as UTF-16LE (guessed)`, or `editor.rs holds 12 bytes UTF-8 cannot decode — they stay as they
  are`. One decode pass on the read's worker counts them.
  - The first non-ASCII character typed or pasted then asks: `Type 한 as UTF-16LE? The encoding was
    guessed` — **Type** · **Cancel**, the focus on Cancel. Cancel types nothing; Type is not asked
    again for this file.
  - This holds for every encoding the default can guess, so it does not wait for VM-4's CP949.
- **The BOM stays.** The caret's first place is after it (08 §7.2's skip rule), so no edit reaches it.
  Select all starts after it.
- **Line endings**: every line keeps its own.
  - A new line — Enter, or a line break in a paste — takes the ending of the line it splits; failing
    that, the file's first ending; failing that, LF.
  - No final newline is added or removed.
  - A line ends at the encoding's LF unit (08 §7.3), never at a byte 0x0A inside another unit.
- **A CR LF pair is one line end, in every encoding** (E35): the CR unit right before the LF unit
  (08 §7.3) and that LF unit.
  - The caret never stands between them. Backspace at a line's start and Delete at a line's end take
    both.
  - Only a boundary strictly between the CR and the LF is moved, past the LF. A boundary before the
    CR stays where it is: in `a` CR LF `b`, a block of `a` alone, cut, leaves CR LF `b`.
  - Overwrite never replaces a line ending: typing at a line's end inserts.
  - Enter in a CR LF line writes CR LF.
  - A lone CR is no line end in sopen (08 §7.3: only LF and CR LF end a line). It is a character,
    drawn as its CP437 glyph (08 §7.1). A file of CR-only lines is one line, and Enter there writes
    the file's first line ending, LF when it has none.
- **Tabs**: Tab types a tab where the file's indented lines start with one, else spaces to the next
  multiple of 4 (the viewer's `TAB`, `text.rs:152`).
- **Moving by characters** steps one decoded `Cell` (`Encoding::decode`, `encoding.rs:381`), so the
  caret is always on a character start, in every encoding.
- **Bytes the encoding cannot decode** (a UTF-8 `FF`, a lone surrogate) show as U+FFFD and stay as
  they are unless edited. Ctrl+K C and V copy and move the block's pieces, so such bytes move
  unchanged.
- **The clipboard** (E36): Copy and Cut put the decoded text on the clipboard, and keep the block's
  pieces beside it, the last copy only, in memory.
  - A paste whose text equals that text pastes the pieces: the same bytes, undecodable ones too.
  - Any other paste holding U+FFFD is refused whole, as an unencodable character is: `the paste holds
    �, which stood for bytes that could not be decoded — nothing was pasted`.

### 7.3 Undo (E17)

- **One stack of edits.** Each records the offset and the removed and inserted bytes, held as pieces
  of the two buffers, so even a large delete copies nothing.
- **Grouped** as Turbo C++'s Group Undo (*editor* §3.4): a run of typing, a Backspace run, one
  paste, one block operation. Undo and Redo restore the caret and the block.
- **At most 100,000 steps**, 07 §11's bound for hex. Past it the oldest go.
- **The buffers never change identity within a session.** A Save does not rebase the piece list onto
  the saved bytes (§9.3), so an undo or redo across a Save refers to the same original and add buffer
  as before it, and needs no copy.
  - Undoing past a Save makes the document unsaved again, as an image's undo does (07 §2).
  - Memory: the original (at most 256 MiB) plus everything typed or pasted in the session, held until
    the editor closes. Nothing more is retained after a Save.
- **A re-read starts over**: Reload, and E30's re-read of a clean editor (§9.4), make a new original
  and clear the stack. The status row says `undo history cleared — the file was read again`.

### 7.4 Reading the file (E13–E15)

- **Whole, at F4**: the original is held in memory.
  - The limit is 256 MiB, `HASH_CHECK_MAX` (`remote/mod.rs:34`); up to it every Save checks the
    whole content.
  - Above it F4 opens nothing and says why (§6.2), and `sopen` on such a file opens the viewer
    (§11).
- **A local file** is read from disk, with its SHA-256 taken on the way and its device and inode
  recorded for E32. Its text document reads it in place and holds no stream (`local_parts`,
  `prepare.rs:1235`).
- **A remote file** (E14) is read by a `Bulk` reader (`stream/mod.rs:92-93`; `Source::reader`,
  `stream/source.rs:1337`) on the opening's `Source`.
  - Remote text of every size opens as a stream: `Parts::TextStream { source, … }`
    (`prepare.rs:1122-1130`). That is the one stream the view set keeps for every view (08 D13), and
    the text document holds it (`text.rs:222-225`).
  - So the read runs on the viewer's `Ranged` channel, and the count stays 06 §6's "1; 2 from its
    first Save". The editor's Save opens the `Origin` at its first use and gives it back when the
    editor closes, as a hidden view gives its own back (08 D14).
  - A Bulk reader's blocks go to the cache's cold end and do not touch the cache's order
    (`stream/cache.rs:105, 122-123`; `source.rs:836`), so the viewer's own window stays cached.
  - Progress and Esc: the status row says `reading for the editor — 37 %`; Esc goes back to viewing
    and changes nothing.
  - At 07's rates (1.39–3.6 MB/s, *editor* §3.5): 1 MB takes 0.3–0.7 s, 16 MiB 4.7–12 s, 100 MB
    29–75 s.
- **The preview's blocks survive F4** (E15).
  - F4 on a previewed file takes the preview over as Enter does (`Host::take`, `preview.rs:326-351`).
    A running preparation is taken over with its budget lifted (`Taken::Running`, `:342`).
  - The hand-over moves the stream "with its cache, its readers and its session: no byte is read
    twice" (`prepare.rs:886-890`).
  - The Bulk read is served from the cache wherever a block is already there (`serve`,
    `source.rs:831-842`) [S].
  - So the ≤ 4 MiB a text preview read (02 §4.3) is not fetched again: [추정] end to end until ES-6's
    `fetched=` receipt.
- **Memory**: the file plus what was typed, plus the stream's block cache of at most 64 MiB
  (`cache.rs:13`) while the stream is open (§9.3 closes it after a Save).
- **The baseline**: the opening's stat and the SHA-256 of the bytes read. A file that changed after
  the opening's stat fails Save's checks (§9.1, §9.2); it is never written over silently.

## 8. Highlighting (E18, E19)

- **One highlighter** for the preview, the viewer and the editor: syntect as sopen links it
  (`Cargo.toml:90`), with the viewer's syntax set and `chrome::syntax_theme` (`chrome.rs:544`).
  75 syntaxes, 0 bytes added [M *editor* §4.1].
- **On the viewer's worker** (`MAX_WORKERS = 3`, `text.rs:2795`), because one syntect call cannot be
  interrupted.
  - A keystroke re-parses from the kept state before the edited line to the page's end: 3.1–3.3 ms
    mid-block, 12.8–13.3 ms at a block's last line [M *editor* §4.1].
  - The frame draws the edited line in its old colours until the new runs arrive.
  - The worker then walks on to the next kept state. If the states differ, the kept states after it
    are dropped and made again as pages are asked for (`Highlight::page`, `text.rs:2742-2789`). The
    states were equal after mid-block, block-end and `/* … */` edits [M].
- **Kept states** (E19): every 64 lines instead of every 256 (the viewer's `BLOCK_LINES`,
  `text.rs:115`).
  - So a keystroke parses at most 64 lines plus a page: about 5 ms at 40 µs a line [M rate].
  - The memory per state is not measured. [추정]: under 1 KiB at source nesting depths, so about
    7 MiB over the 16 MiB that highlighting covers (`HIGHLIGHT_MAX`, `text.rs:123`) at 40-byte lines.
  - **EH-3** (ED-screen) asserts the kept states' heap over a 16 MiB Rust file is at most 32 MiB at
    64 lines. If it is not, the unit ships the spacing at 256 — one constant — and asserts it there.
- **It stops where the viewer's stops**: a line over `LINE_MAX` (`text.rs:120`), anything past
  `HIGHLIGHT_MAX`, long runs shortened (`Shortened`, `text.rs:2392`).
- **Killed** (*editor* §4.2):
  - tree-sitter: 40× faster a keystroke, but +1.2–2.9 MB for Rust alone and a second colour mapping;
  - a hand lexer: one language.

## 9. Saving (E21–E26, E30, E32, E38)

### 9.1 The route (E21, E24, E32)

**Today's primitive is a check, then a rename.** `RemoteFs::write_atomic_with` (`remote/mod.rs:619`)
is one code path for SFTP and for `LocalFs` (`remote/local.rs:62, 220, 232`) [S *editor* §5.1]:

- it refuses without `posix-rename`, and checks the target's stat (`check_target`,
  `remote/mod.rs:634`, `:820-850`: a regular file, not a link, the same size and mtime);
- it uploads a temp `.<name>.sopen-<random>`, created exclusively in the same folder with the target's
  owner and mode;
- it re-reads the target's content up to 256 MiB and checks its stat again (`prepare_swap`,
  `:890`; the re-read at `:938-958`), then renames the temp over it (`:693`);
- after the rename it checks that the target is the object it staged (`:706-728`). That is about the
  new file: the version the rename replaced is gone by then.

So a change between the last check and the rename is replaced unseen. 07 already calls the checks
"best effort … not a compare-and-swap" (07 §1).

**The editor's Save uses a guarded replace** (E32): `RemoteFs::replace_guarded`, a new trait method
beside `write_atomic_with` (ED-save). The hex and image windows keep theirs.

- **Every phase is in the save intent first.** 07 §3.1 writes each phase of a Save to the journal's
  `save` file, durably, before the request that moves the Save to it (`07-streaming-edit.md:213-218`);
  today's phases are `write_atomic_with`'s steps (`Step`, `remote/mod.rs:83-104`: named, written,
  staged, renamed, removed, left). The guarded replace adds `exchanging` and `swapped back` (local),
  `linked`, `committed`, `verified` and `kept` (remote), each naming the paths it holds, so a Save cut
  off anywhere is reconciled from the intent on the next open or reconnect, never repeated blindly
  and never by removing a file it has not checked.
- **A local file — a conditional commit.**
  1. The intent records `exchanging`: the temp's and the target's paths, and both devices and inodes.
  2. The staged temp is exchanged with the target in one call: `renamex_np(…, RENAME_SWAP)` on macOS,
     `renameat2(…, RENAME_EXCHANGE)` on Linux, through libc (already a dependency, `Cargo.toml:63`).
  3. The file now at the temp's name is the version displaced. It must be the version read: the
     baseline's device and inode, size, mtime and SHA-256. Then it is removed, the folder fsynced and
     the intent closed.
  4. Otherwise the intent records `swapped back`, and the two are exchanged back in one call, so the
     other writer's version is where it was; the conflict dialog opens (§9.2).
  - **A swap-back that fails** leaves sopen's version in place and the other one at the temp's name.
    That one is renamed to the visible `<name>.sopen-replaced-<stamp>` beside the file — never
    removed — and the result says `replaced; the other version is kept at <path>` (or the temp's own
    path when that rename fails too). The editor stays open with its text.
  - **A crash between the exchange and the check** is reconciled on the next open from the intent:
    the file at the temp's name is hashed against the baseline. Equal → the Save had landed; the temp
    is removed. Different → it is the other writer's version: renamed to
    `<name>.sopen-replaced-<stamp>` and named. Never "remove it" unchecked.
  - Residual: a writer that still holds the old file open and writes into it after the check writes
    into the file being removed, as with any replace by rename. A reader by path sees sopen's content
    between the exchange and an exchange back.
  - **Where exchange is not offered**, `write_atomic_with` as today, and the status row says once per
    filesystem `this filesystem cannot exchange files: a change in the last moment before the save
    is not caught`. That is the call failing with `ENOSYS` (an older kernel, or a seccomp filter) or
    `EINVAL` / `ENOTSUP` / `EOPNOTSUPP` (a filesystem without it [추정: NFS, SMB/CIFS, FAT and exFAT,
    most FUSE mounts]).
- **A remote file — today's check before the rename, and a link check after it** (orchestrator,
  review round 1). SFTP has no exchange. Where the server offers `hardlink@openssh.com` (`can_link`,
  `remote/mod.rs:469`; `link_new`, `sftp.rs:896`):
  1. the stat check as today, then the upload;
  2. the intent records `linked` and the link's path, then the target is hard-linked to
     `.<name>.sopen-prev-<random>`: the version about to be replaced;
  3. **today's check before the rename** — the content re-read and the stat check (`prepare_swap`,
     `remote/mod.rs:890`; the re-read at `:938-958`). A change found here replaces nothing: the link
     and the temp are removed (the link is a second name of the file still in place), and §9.2's
     dialog opens with Reload · Overwrite · Save As… · Cancel;
  4. the temp is renamed over the target (`:693`), and the intent records `committed`;
  5. **the link check after it**: the link is read whole (up to 256 MiB) and hashed against step 3's
     hash. Equal → the intent records `verified` and the link is removed. Different → the change came
     in the window between step 3 and the rename: the link is renamed to
     `<name>.sopen-replaced-<stamp>`, the intent records `kept`, and the result says `saved, but
     editor.rs changed in the moment before — that version is kept at <path>`. Never silent.
  - **When something in it fails**, nothing unverified is ever deleted:
    - a `link` refused for this file (`EPERM`, `EMLINK`, `EDQUOT`) → today's route, with the line
      below;
    - step 5's read failing → `saved, but what it replaced could not be checked — kept at <link>`;
    - the rename of the link failing → the line names the link's own path;
    - the link's removal failing after a good check → `saved; remove <link> by hand` — and the next
      reconcile removes it.
  - **Reconcile on reconnect**: `linked` without `committed` → not committed; the link is hashed
    against the baseline. `committed` without `verified` or `kept` → committed, the check pending:
    step 5 runs then. Either way a link equal to the baseline is removed, and any other is renamed
    `<name>.sopen-replaced-<stamp>` and named.
  - Residual: a writer that replaces the file by its own rename between steps 2 and 4 is replaced
    unseen; one that writes in place there is caught, at step 3 or at step 5.
  - **Without `hardlink@`**: `write_atomic_with` as today, and the status row says once per server
    `this server cannot link files: a change in the last moment before the save is not caught`.

**The steps**, after the image window's Save (`image/file.rs:150-260`):

1. Refuse when the server cannot replace a file in one step (`CANNOT_REPLACE`), offering Save copy.
2. Reconcile a Save of this session that was cut off (07 §3.1).
3. The session's first Save backs up the original as opened to `<cache>/.backup/<host>/<abs>.<stamp>`
   and prints its path (01 §5): a hard link of the journal's `original` (§9.5) where `<cache>` is
   one volume, else a copy.
4. Stage the pieces in order into `<cache>/.edits/<host>/<abs>` (0600, created exclusively), hashing
   the new content on the way.
5. `replace_guarded`, the journal recording each step (07 §3.1).
6. Once it has landed: §9.3.

- **While it runs**: progress and Cancel, as the image window's `Saving` shows them.
  - A remote Save moves the file three times: the upload, today's re-read before the rename and the
    link check after it — 1 MB in 0.9–2.1 s, 100 MB in 87–225 s at 07's rates [S *editor* §5.1,
    tripled].
  - A streamed hex Save's server copy (07 §4.2) does not fit here: its temp must keep the size as
    opened, and a text edit changes the size.
- **No edits while a Save runs** (E24), as in 07 §10's hex and image windows. The status row says
  `saving — editing resumes when the save is done`; the view still scrolls. The baseline after it is
  the bytes staged.
- **The result**: `saved host:/abs · backup <path>` (07 §10).

### 9.2 A file changed since it was read (E22, E23)

- **At Save, before anything is replaced**: a check finds that the size, the mtime or (up to 256 MiB)
  the content changed, that the file is gone, or that it is now a link or not a regular file
  (`WriteOutcome::Conflict`, `remote/mod.rs:115-119`; a remote file's re-read before the rename,
  §9.1 step 3; a local file's exchange back, §9.1 step 4).
  - Nothing is written over the file. The staged copy is kept under `.edits` and named. The edits
    stay in the editor and in the journal.
  - A dialog (ED-dialogs) says what changed, e.g. `editor.rs changed since it was read (size 12,288 →
    12,301)`, and offers four choices, the focus on Cancel (E6):
    - **Reload**: discard the edits and read the file as it is now, at the caret's line. It asks
      `Discard your changes and read editor.rs again?` first; the journal goes.
    - **Overwrite**: only while the file is still a regular file (below).
    - **Save As…**: 07 §7's local path, never over anything.
    - **Cancel**: keep editing; the next Save checks again.
- **After the rename, for a remote file**: a change that came only in the window between the last
  check and the rename is found by the link check (§9.1 step 5). The file then holds the edit; the
  other version is kept at `<name>.sopen-replaced-<stamp>` and the result line names it. No dialog
  opens, since there is nothing left to choose; the kept copy is the other writer's text.
- **Overwrite** (E23) is a second guarded replace, never a way around the checks.
  - The file is read again as it is now (with progress, up to 256 MiB).
  - That version is written to the backup folder beside the session's first backup, and its path is
    printed. The version Overwrite replaces is never lost.
  - Its stat, SHA-256 and (local) device and inode become the baseline, so a change between that read
    and the replace is caught again (E32).
  - A file that is gone, a link or not a regular file gets no Overwrite: there is no version to back
    up, and `check_target` replaces only a regular file. Save As… and the menu's Save copy remain.
- **Two editors on one file** — two xtools windows, or the editor and VS Code — meet here: the second
  Save finds the first one's version (ES-7).

### 9.3 After a Save lands (E38)

- **The piece list is kept as it is.** The saved version is the document as shown; the bytes staged,
  their stat and (local) device and inode are the next baseline, and the undo stack gets a mark
  (§7.3).
- **The opening's stream is closed**: it reads a file that has been replaced. 07 does the same for
  the media window after a remote Save, so that it never shows `Replaced` for its own save
  (`07-streaming-edit.md:779-782`). Its connection goes back to the window for reuse.
- **Every view of the file is then stale** (08 D25) and is made again when shown, over a new opening
  of the saved file: the Text view that F4 or Esc returns to, the Hex view, and the side's preview
  once the editor has closed. For a remote file that reads the first screen again, not the whole
  file. `Replaced` never shows.
- **`FilesChanged`** (`browser.rs:121`) is sent after the baseline is updated, so the Dir tab
  re-lists, the Git tab soft-refreshes (02 §7, 03 §5), and E30's stat finds nothing to do.

### 9.4 sopen's own changes, and a deleted file (E30)

- **A changed folder makes the editor look, not act.** A `FilesChanged` naming the file's folder, or a
  `GitEvent::WorkTreeChanged` naming its repository (`git_host.rs:2358`: a checkout, a discard), makes
  the editor stat the file — locally by `lstat`, remotely on the browser's listing channel (06 §6).
  - It acts only when the stat differs from the baseline (size, mtime and, locally, device and inode).
    The editor's own Save updates the baseline first, and another operation in the folder leaves the
    file's stat alone, so neither re-reads or warns.
  - A clean editor reads the file again at its place (§7.3's undo note). One with unsaved edits reads
    nothing and says `editor.rs changed on disk (checkout) — Save will ask`.
- **A deleted file never closes the editor.** The buffer stays, and the status row says `editor.rs was
  deleted on disk — Save copy or F7 keeps your text`. Save then opens §9.2's dialog without Overwrite.
- **A delete in the column whose selection holds the edited file** — F8, Del, Shift+F8 or Shift+Del
  (Shift+Del is the list's delete without the Trash there, the editor's Cut only in the editor) —
  opens 04 §5.5's dialog with the focus on Cancel, under the focus rule (E6): Enter presses Cancel.
  Its hint changes to `editor.rs is open in the editor — Enter cancels; Tab, then Enter, deletes`.
- **Another program's change is found at Save**: sopen has no file watcher (03 §5, 01 §9).

### 9.5 The journal (E25)

- **A fourth kind**: `journal::Kind` (`journal.rs:233-237`: `Hex`, `Image`, `Media`) gains `Text`.
  - `head` is 07 §3's (kind `text`, the baseline); `seen` is the read's whole SHA-256; `edits` is
    the piece list.
  - Generations are written as 07 §3 writes them: at most 250 ms after a change, at focus loss, at
    close, before a Save.
- **It is self-contained.** At the first edit the journal writes `original`: the bytes as read, once,
  on a worker, fsynced, its SHA-256 equal to `seen`. Beside it, the add buffer in immutable chunks —
  `add-<n>` holds the bytes appended since the previous chunk, written, fsynced and named once, as
  07 §3 keeps each pasted picture in a file of its own. A generation names the pieces and how much of
  the add buffer they use, so a 50 MB paste is written once, not four times a second.
- **Nothing is "kept" before `original` is durable.** No generation is published until `original`
  and the folder are fsynced; until then the status row says 07's `keeping…`, never `kept`.
  - With no room for it (`ENOSPC`, or less free space than the file's size), no journal is published
    and the row says `unsaved — NOT kept on disk (no room for a copy of the file)` in the error colour,
    as 07 §3 says when a generation cannot be written (`07-streaming-edit.md:138`). The close prompt
    still guards the edits.
- **A re-read starts a new backing generation.** Every re-read that lands — Reload, or E30's re-read
  of a clean editor (§9.4) — makes a new original, `k + 1`. It is written as `original.tmp`, fsynced
  and renamed over `original`, and the folder is fsynced, before any generation names `k + 1`. Every
  generation names the `k` of the original its pieces were made on, and a generation is published
  only once that original is durable; the old original's file is gone with the rename, so no piece
  can be applied to a buffer it was not made on.
- **Restore rebuilds the buffer from the journal alone** and never applies pieces to the file. 07 §3's
  Restore checks the file's content and applies nothing to a file that changed
  (`07-streaming-edit.md:170-191`); the text journal goes further.
  - The file as it is now is then compared with the journal's baseline. The same → the edits are
    unsaved edits against it, as 07 §3 has them.
  - Changed or gone → the rebuilt buffer opens with `editor.rs changed since these edits were made —
    restored from the journal; Save will ask`, and Save opens §9.2's dialog.
  - Cost: the file's size on the local disk, once per editing session (at most 256 MiB). The
    recovery row (07 §3's Restore · Later · Discard) shows the journal's size beside its date.
  - A journal nobody restores stays, whatever its age, until Restore, Later or Discard is chosen on
    that file's next F4: Later keeps it, and nothing else removes it.
- **One journal per editing session** (07 §3). Its baseline follows each Save. It is removed when the
  editor closes with nothing unsaved, or on Discard.
- **Undo past a Save stays kept on disk**, because the pieces keep their buffers across a Save (§7.3).
  The image window's journal cannot replay that (`07-streaming-edit.md:732-736`); this one can.

### 9.6 Leaving with unsaved edits (E26, E6)

- **The prompt is 07 §10's**: `Save the changes to host:/abs before closing?`, with **Save** (Save
  copy where Save is off) · **Save As…** · **Discard** (its hover names the kept edits it deletes) ·
  **Cancel**. It is the hex window's (`hex.rs:4234-4291`), drawn in cells by ED-dialogs after
  XW-dialogs.
- **The focus rule** (E6), for this prompt, §9.2's dialog, Ctrl/⌘+R's question and §7.2's encoding
  question: each opens with the focus on **Cancel**. Esc is Cancel; ← →, Tab and Shift+Tab move the
  focus; Enter presses the focused button; a letter answers nothing.
  - The keys that open them — Ctrl+Q, Ctrl+W, Ctrl+R — are WordStar prefixes and scroll keys, so a
    Turbo hand's next key lands in the prompt. Under this rule it can neither save nor discard.
- **The path through the shell and the browser**:
  - The shell turns Cmd/Ctrl+Q and +W, and the window's close, into a close request, and cancels it
    when the window refuses (`closing()`, `gui/mod.rs:1180-1187`).
  - The browser asks an open viewer first (`close_requested`, `browser.rs:2840-2857`). The editor is
    asked there too, and refuses while it holds unsaved edits, showing the prompt.
  - A Save that lands, or Discard, asks to close again: `request_close` (`gui/mod.rs:699-710`); a
    hosted window tells its host instead (`host_pass`, `:900-907`).
  - In the TUI the same `closing()` must run each pass, and a close it grants must end xtools. That
    is a requirement on XW-term (10 §17) [추정 until XW-term lands].
- **Every exit of §6.4 asks the same four choices**; a running Save makes the exit wait (07 §10).
- **The switch** carries unsaved edits within one process, and asks first across machines (10 §7.4,
  T4, T30). Its state gains `editor: { file, encoding, caret, anchor, top, ovr, tab, focus, column,
  wide, find }` and bumps `v` (10 T32).

## 10. Git from the editor (E27)

- **The Git tab is 03's git panel** of the file's repository, the one panel the window keeps per top
  level (02 §3.1: at most 3), drawn at the column's rect. Its rows, keys, F-key row (03 §9.1), menu
  and dialogs are 03's.
- **Narrow already**: 03's graph fits its columns to the width. The Graph column takes at most a third
  of it; Author, Date and Commit are dropped, in that order, so the description keeps 16 cells
  (`graph_view.rs:5-7, 28, 102`).
  - At 24 cells that leaves the lanes and the subject. No narrow layout is built (Tensions).
  - Ctrl/⌘+Shift+G gives the panel the whole area and back, the editor kept underneath (E40).
- **Outside a work tree** the tab says `not in a git work tree` (§6.3).
- **What the editor uses of 03**, all of it there already [S *editor* §6]:
  - the uncommitted row's groups (03 §6.1);
  - Space to stage and unstage (`add -- <p>` · `restore --staged -- <p>`, 03 §7.6);
  - F7 Commit… on the uncommitted row (`commit --cleanup=verbatim -F -`), and amend;
  - Enter on a file row for its diff (03 §6.2);
  - F8 discard, backed up (03 §8.2). It changes the file under the editor, so §9.4's rule applies.
- **The editor's Git menu** acts on the edited file without leaving it: Stage this file, Unstage,
  Commit…, Diff against the index, Show in Git (the tab, with the bar on the file).
  - These are 03's `stage`, `unstage` and `commit` (`ActionId::Stage`, `Unstage`, `Commit`,
    `git/actions.rs:111-113`), run by the window's git host as the panel runs them.
- **The status letter** in the bottom border — `M`, `A`, `?`, or `S` when staged — comes from the
  uncommitted row. A Save's `FilesChanged` refreshes it (03 §5's soft refresh).

## 11. Routing and VS Code / Zed (E3, E29, E33)

- **F4 is xtools's editor and Shift+F4 is VS Code / Zed** in the list, the viewer, the editor and the
  git panel (today all F4: `keys.rs:456, 496, 503`). 04 row 4 becomes `4 Edit: xtools's editor`.
  - Shift+F4 decodes as itself in both encodings [M *editor* §1.4].
  - F4 on a folder, and in the git panel on anything but a file, stays VS Code / Zed (§6.2).
  - The File menu has `VS Code / Zed`.
- **`sopen host:/file.txt`**: 10 §11 row 5 waited for XR-E, and this is its answer.
  - With input and output terminals, a text file routes as its folder would (rows 7–10: today's
    window until 10's flip, the TUI after it, on the destination). It opens with the bar on the file
    and the editor open.
  - With input or output not a terminal (a `while read` loop, a launcher; row 6), it keeps 01 A2's
    route to VS Code / Zed. That route is `Kind::Text => Route::Editor` today (`main.rs:681`).
  - `--app APP` still opens a copy in any local app (`main.rs:64-66`), and `--hex` the hex editor.
  - Above 256 MiB the command line opens the viewer (view 1), with §6.2's line.
- **Started as the editor, it ends with the editor** (E33). When `sopen <text file>` or `sopen --tui
  <text file>` opened xtools with the editor, Esc or F3 out of the editor — after its prompt — ends
  xtools with status 0, so `EDITOR=sopen` works.
  - F4 (to the viewer) and opening another file from the column keep xtools running; it then ends as
    xtools always does.
  - `sopen --tui <text file>` opens the editor rather than the viewer: 10 T39 changes for text.
  - Through a forward to a HOST destination the command returns when HOST's sopen ends [추정: ER-1
    checks it with the stand-in ssh].
- **The new row 5 of 10 §11**: "text (a file), input and output terminals → as its folder (rows
  7–10), the bar on it, the editor open, xtools ending with the editor; above 256 MiB its viewer".
  Any other text falls to row 6.

## 12. Decisions

Every decision is reversible; the owner may change any of them.

| # | Decision | Why |
|---|---|---|
| E1 | The look: xtools's PC Shell skin (title row = PC Tools Desktop's menu bar, F-key row = Notepads' bar) plus Turbo C++ 3.0's edit window: double frame, `[■]` and `[↕]`, `* line:col`, blue on cyan for a block. The text keeps the viewer's colours (orchestrator) | the two programs the owner named share xtools's skin already (*editor* §1.3, `chrome.rs:1-9`); "그대로" keeps the preview's colours |
| E2 | The F4 layout: the list narrows to a left column (30 %, 20–40 cells); Dir and Git tabs, switched by Tab; the editor in the preview's place; F6 between them; rule 1a of 02 §3.1 (orchestrator) | message 21's own words; `git_wide` shows a whole-area state can keep the sides' states (02 §3.1) |
| E3 | Shift+F4 = today's F4 (orchestrator). F4 on a folder or the git top level stays VS Code / Zed. In the editor F4 = the Enter viewer at the caret, and Esc / F3 = back to where F4 was pressed. Shift+F4 from the editor leaves it first. F4 from a Hex view with unsaved edits asks first | "디폴트로 로컬 에디터" with VS Code / Zed one key away. The built-in editor edits files, so a folder keeps today's route (01 A11). Two editors on one file only meet at the conflict check. One view holds edits at a time (08 D2) |
| E4 | F2 and F5 both Save (Turbo's and PC Tools'). The WordStar Ctrl+Q prefix, Ctrl+W and the cursor diamond are dropped; Ctrl+K blocks, Ctrl+Y / T / N / L and Alt+Backspace undo are kept (orchestrator) | Ctrl is sopen's command in a terminal and on Linux (10 T26), so the diamond and Ctrl+Q / W collide with Save, Undo, Cut, Copy and close; the kept keys decode alike in both encodings [M *editor* §1.4] |
| E5 | While the editor has the keyboard the hook makes no F2 or F5 from a digit, decided at each press; a digit held from before has its repeats dropped. The editor answers the digit with a line | under legacy Ctrl+] is F5 [M *editor* §1.4], and Esc-then-digit can read as Alt+digit (10 §9). Deciding per press is per event; a mark could not tell two F5s of one read apart, and the event has no free field |
| E6 | Every prompt that holds a Save or a Discard opens with the focus on Cancel; Enter presses the focused button; letters answer nothing | Ctrl+Q, Ctrl+W and Ctrl+R open prompts and are also WordStar prefixes, so the next key is often a letter or Enter |
| E7 | Under legacy, Ctrl+Shift+Z is Undo; Redo is the Edit menu, with a hint once a session | an undo writes nothing and is itself redone; no free key that survives legacy encoding is a Turbo or sopen Redo |
| E8 | Ctrl/⌘+Q and +W stay sopen's close everywhere; with unsaved edits the close is refused and the prompt shown | `closing()` takes them in every backend (`gui/mod.rs:1180-1187`); the prompt guards the edits |
| E9 | Paste is the terminal's own key in the TUI, egui's `Paste` in the window: one undo step, its line breaks written as the ending of the line they land in. In a terminal Ctrl+V and Shift+Ins arrive as keys and the editor names the terminal's paste | the translator makes the paste LF-only (`term_input.rs:18-20`); a file keeps its line endings; today Ctrl+V vanishes (`term_input.rs:236`), so the owner gets no answer |
| E10 | Korean input source: armings, menu letters and Find's options match the physical key in the window, with the input method off meanwhile, and a 2-set jamo as its key's letter in the TUI | a terminal gives no physical key (10 §9); egui-winit allows the input method only while `PlatformOutput::ime` is set |
| E11 | A piece table over the file's bytes, pieces in a `Vec` first (orchestrator) | lossless in 7 of 7 fixtures, 1.0× memory, every operation within a frame [M *editor* §3.1–§3.2] |
| E12 | The file keeps its encoding (the one on screen at F4), BOM and line endings. An encoder per encoding, sopen's own for the Unicode ones; a character the encoding cannot hold is refused (orchestrator) | 08 §7; encoding_rs writes no UTF-16 [M]; replacing a character would change text the owner typed |
| E13 | 256 MiB, read whole at F4 (orchestrator) | `HASH_CHECK_MAX`: up to it every Save checks the whole content (`remote/mod.rs:34`) |
| E14 | A remote whole read is a `Bulk` reader on the opening's `Source`: the viewer's `Ranged` channel. Save uses the `Origin`, opened at the first Save and given back on close | 06 §6's count holds (1; 2 from the first Save); a Bulk reader goes to the cache's cold end (`cache.rs:122-123`) and the view set already keeps one stream (08 D13) |
| E15 | F4 takes the preview over as Enter does, so its cached blocks serve the whole read | the hand-over moves the stream with its cache (`prepare.rs:886-890`) and `serve` reads cached blocks (`source.rs:831-842`); receipt ES-6 |
| E16 | No word wrap while editing; one row per line | Turbo C had none (TC2-UG p. 91); a caret over wrapped rows needs wrap's layout in the edit model; Options can add it later |
| E17 | Undo steps hold pieces; groups as Turbo C++'s; at most 100,000 steps. The buffers keep their identity for the session: a Save does not rebase the piece list, so undo crosses a Save with no copy; a re-read clears the stack | a 200 MB delete costs nothing; 07 §11's bound; the memory held is the original plus the session's typing, and nothing more after a Save |
| E18 | syntect, the viewer's (orchestrator) | one colour mapping, so F4 changes no colour; 0 bytes added; 3–13 ms a keystroke on a worker [M *editor* §4] |
| E19 | Kept highlight states every 64 lines within `HIGHLIGHT_MAX`; EH-3 asserts at most 32 MiB, else the spacing is 256 | at most about 5 ms a keystroke; memory bounded by the 16 MiB highlighted; [추정] until EH-3 |
| E20 | Hangul preedit: in the TUI the terminal draws the composition at the cursor, which the editor puts at the caret; in the window the editor draws the preedit over the cells at the caret, as a terminal does, and sets the IME rect there. The preedit's rect is masked in the 1:1 check | 10 §4 puts the cursor at the caret for this; an overlay keeps the two backends alike; whether each terminal draws it there is [owner, EO-2] |
| E21 | Save follows the image route: backup once a session, stage, a guarded replace with the journal's steps, `FilesChanged` (orchestrator) | one shipped route for local and SFTP (*editor* §5.1); a streamed hex Save keeps the size and so does not fit |
| E22 | A conflict at Save offers Reload · Overwrite · Save As… · Cancel; nothing is ever written silently (orchestrator) | 01 §5; the owner chooses |
| E23 | Overwrite re-reads the file, backs that version up, and replaces with it as the baseline; it is not offered for a file that is gone or not regular | the version replaced is someone's work; a third change is still caught; `check_target` replaces regular files only |
| E24 | No edits while a Save runs | 07 §10's rule; the next baseline is exactly the bytes staged |
| E25 | `journal::Kind::Text`, self-contained: the journal keeps `original` and the add buffer in immutable chunks; Restore rebuilds from them and never applies pieces to a changed file. No generation is published before `original` is fsynced (`keeping…` until then; no room → `NOT kept on disk`). Every re-read starts a new original generation `k`, and a generation names the `k` its pieces were made on. The recovery row shows the journal's size; a journal stays until Restore, Later or Discard on that file | 07 §3's Restore refuses a changed file and the edits stay unreachable; `kept` must mean durable; pieces made on one original must never be applied to another; a large paste is written once, as a pasted picture is; the cost is the file's size on disk per session |
| E26 | Every exit with unsaved edits asks 07 §10's four choices, through `closing()` and the browser's `close_requested` (orchestrator) | the shipped prompt (`hex.rs:4234-4291`); the editor lives inside the browser window |
| E27 | The Git tab is 03's panel, narrowed by its own rule; the editor's Git menu runs 03's actions on the edited file (orchestrator: 03's panel) | nothing is built twice; `graph_view.rs` already narrows (`:5-7, 28`) |
| E28 | Find matches decoded characters, in the file's encoding | Turbo's U (ignore case) and W (whole words) need characters; an encoded needle can match neither |
| E29 | `sopen file.txt` with input and output terminals → xtools with the editor, where a folder opens xtools; pipes and scripts keep VS Code / Zed; above 256 MiB, the viewer (orchestrator) | "디폴트로 로컬 에디터"; scripts and hooks keep today's paths (10 T13) |
| E30 | sopen's own changes make the editor stat the file and act only when the stat differs from the baseline; a deleted file never closes the editor | there is no file watcher (03 §5); the editor's own Save and a neighbour's copy must not look like a change; a buffer is never dropped unasked (01 A11) |
| E31 | In a terminal, until Shift is known (a Ctrl/⌘+Shift chord on a letter key has arrived with Shift since the last focus report or resize), Ctrl+S arms `^S` (`Save over <name>? F2 or a click on [Save] saves · C saves a copy · any other key cancels`): only a real F2 or a click on the prompt's Save overwrites, C saves a copy, every other key — letters, Enter, Space, F5, a second Ctrl+S — cancels, is not typed, and says `not saved — F2, Ctrl+K S or a click saves`. Shift known is forgotten at every focus report and resize (orchestrator, review round 1, revised the same day) | a legacy terminal sends Ctrl+Shift+S as Ctrl+S, and twice as the same byte twice, so doubling proves nothing, and a letter may be the start of a word typed without looking; F2 is a key no word types, and Save copy writes a new file only; once Shift has been seen the chord means what it says, until a reattached client, which may drop Shift, sends a focus report or a resize |
| E32 | The editor's Save is a guarded replace, every phase recorded in the save intent first. Local: an atomic exchange, the displaced file checked against the version read (device, inode, size, mtime, hash), exchanged back on a mismatch; a failed swap-back or a crash before the check keeps the displaced file as `<name>.sopen-replaced-<stamp>` unless its hash equals the baseline. Remote: a hard link to the version about to be replaced, today's re-read before the rename (a change there opens §9.2's dialog), and the link hashed after it (a change only in that window is kept as `<name>.sopen-replaced-<stamp>` and named); no unverified link is ever removed. Without exchange (`ENOSYS`, `EINVAL`, `ENOTSUP`) or a link (`hardlink@` missing, `EPERM`, `EMLINK`, `EDQUOT`), today's check-then-rename with the residual said once (orchestrator, review round 1; the remote order revised the same day) | today's final check then rename is not a conditional replace (`remote/mod.rs:693, 706-728`); an exchange is one, and a link is the only way to see what a rename replaced over SFTP |
| E33 | Started as the editor (`sopen <text>`, `sopen --tui <text>`), xtools ends when the editor closes (orchestrator, review round 1) | `EDITOR=sopen` must return when the edit is done; 10 T39's "one meaning for every SOURCE" then holds for text too |
| E34 | While `^K` is armed, F2, F5 (never from a digit) and a click on a Save slot save; while `^S` is, only E31's F2 or the prompt's Save; every other key follows the arming's rule | those are unambiguous; an arming must not swallow a deliberate Save, and `^S` asks about an overwrite the owner may not have meant |
| E35 | A CR LF pair is one line end in every encoding: no caret between them, Backspace / Delete take both, only a boundary strictly between CR and LF moves (past the LF; one before the CR stays), Overwrite never replaces a line ending; a lone CR is a character | a half-deleted CR LF leaves a stray CR the owner cannot see (08 §7.1 draws it as a glyph only alone); 08 §7.3 ends lines at LF and CR LF only |
| E36 | Copy and Cut keep the block's pieces; a paste equal to their text pastes the pieces; another paste holding U+FFFD is refused | undecodable bytes then move intact through the clipboard; any other U+FFFD stood for bytes that were lost when it was copied |
| E37 | A guessed encoding, or one that cannot decode every byte, is said at F4, and the first non-ASCII character asks once, focus on Cancel | typing into a wrong encoding writes bytes that read as noise later; the question costs one answer per file |
| E38 | After a Save lands the opening's stream is closed and every view is made again over the saved file; `FilesChanged` follows the new baseline | a stream over a replaced file shows `Replaced` (07 §9 avoids that for media, `07-streaming-edit.md:779-782`); the views must show what was saved |
| E39 | Every List and Git key in the column is defined (§6.3): F3 as Enter, every edit of another file through §9.6, F9 and F10 empty, Ctrl/⌘+G refused, F2 refused on the edited file, F6 showing a hidden column | the column is the list or the git panel without the other side; a key with an undefined meaning there is a hazard |
| E40 | Rule 1a comes before `git_wide`: F4 saves and clears it, leaving restores it; inside the editor the Git tab's whole-area view is `editor.wide` | one rule must decide the area; the sides' states come back as they were |

## 13. Text this amends

Each unit's PR amends the sentences it makes untrue, as 08 §13 and 10 §12 do. Each text has one
owner, the same as in §16; X5 ships with ED-layout, so every text that describes F4 or an editable
text view lands there and none before it.

| Text | Today | Becomes | Unit |
|---|---|---|---|
| 01 §9 (`01-spec.md:1249`) | "Own text editor" out of scope | xtools's editor (message 21, this document) | ED-layout |
| 01 A11; 01 A2's F4 sentence | F4 = VS Code / Zed | F4 = xtools's editor for a text file; Shift+F4 = VS Code / Zed; a folder keeps F4 = VS Code / Zed | ED-layout |
| 01 A2's CLI part, 01 §3 | text → VS Code / Zed | with terminals, xtools with the editor, ending with it (§11) | ED-route |
| 02 §3.1 | rules 1–4 | plus rule 1a before `git_wide` (§6.1) | ED-layout |
| 02 §4.4; `preview.rs:16-19` | "F4 changes nothing" for a preview | F4 hands the preview over to the editor | ED-layout |
| 03 §6.2 (`03-git-panel.md:613`), §9.1 slot 4, the editor row (`:713`) | F4 = VS Code / Zed | a file → xtools's editor; the top level → VS Code / Zed; Shift+F4 = VS Code / Zed | ED-layout |
| 04 §2 row 4; 04 §6.1 | F4 = VS Code / Zed; Cmd/Ctrl+1…0 = F1…F10 in every window | the editor; Shift+F4; no digit makes F2 or F5 while the editor has the keyboard (E5) | ED-layout, after VM-D0 and KL-W6, which own 04's other lines |
| 07 §1 (`07-streaming-edit.md:54`), §2, §3, §10 | "The text viewer stays read-only"; three editors and journal kinds; the windows table | the Text view's editing state; a Text row (size changes; guarded whole-file replace); kind `text` with `original` and `add-<n>`; the editor's conflict dialog with Overwrite | ED-layout |
| 08 §4 | four view-key states | an editing Text view is a DigitsAreData row | ED-ops |
| 08 §7.5 | F8 Encode in Text and Raw | empty while editing | ED-layout |
| 10 §11 row 5; 10 T39 for text | "01 A2's editor route, until XR-E decides"; `--tui <file>` opens the viewer | §11's row; `--tui <text file>` opens the editor and ends with it | ED-route, before XF, which rebases rows 7 and 10 onto it |
| 10 §16 X5, §17 | "the editor (XR-E)" | this document's units | the X-SPEC round, with this document |
| AGENTS.md:105; AGENTS.md's receipt rows | edit features from owner messages | plus the text editor (message 21); `SOPEN_STATE` `edit=NAME`, `tab=git`, `exit=editor`, `close=ask` for the editor (§15) | ED-layout, after KL-W6 and XW-term, which own their own rows |
| README | F4 = VS Code / Zed; `sopen file.txt` | the editor and Shift+F4 (ED-layout); `sopen file.txt` and `EDITOR=sopen` (ED-route) | ED-layout, then ED-route, after KL-W6 |

## 14. Acceptance

"Do X → observe Y". The unit that brings each item is in brackets. Model items run with no window;
key items send the key and its typed text together (08 §12) and, in a terminal, the bytes a terminal
sends (10 §15).

**The buffer and encodings**
- EB-1 [ED-buf] *editor* §3.2's seven fixtures, a `Z` inserted after line 1 and serialised → each is
  byte-identical to the original plus the edit. The fixtures: UTF-8 with `FF`; UTF-8 CRLF with no
  final LF; UTF-8 with a BOM; UTF-16LE with a BOM, Hangul and CRLF; UTF-16LE with a lone `D800`; CP949
  `똠`; CP949 lead `C8` before LF.
  - On a 100 MB file: 1,000 typed characters, 10,000 scattered inserts and 10,000 deletes, 20,000
    line ↔ offset lookups → no operation above 1 ms, and peak RSS at most 1.1× the file plus 10 MiB
    (*editor* §3.1's prototype is the floor).
- EB-2 [ED-enc] `encode("A한")` → UTF-8 `41 ED 95 9C`; UTF-16LE `41 00 5C D5`; UTF-16BE `00 41 D5 5C`;
  UTF-32LE `41 00 00 00 5C D5 00 00`.
  - U+1F600 → UTF-16LE `3D D8 00 DE`; UCS-2 refuses it.
  - ASCII refuses `한`; Latin-1 gives `E9` for `é` and refuses `한`; CP437 gives `B0` for `░`.
  - `decode(encode(s)) = s` for every encodable `s`.
  - After VM-4: CP949 `똠` gives encoding_rs's two bytes, U+1F600 is refused, and no output holds
    `&#`.
- EB-3 [ED-ops] **BOM, line endings, a doubtful encoding.**
  - In a UTF-8 CRLF file with a BOM: Ctrl+Home, then Backspace → no byte changes. Select all, Delete
    → only the BOM is left.
  - Enter writes `0D 0A` in a CRLF line and `0A` in an LF line. On a last line with no ending it
    writes the file's first ending. No Save adds a final newline. In UTF-16LE, Enter writes
    `0D 00 0A 00`.
  - Tab in a file whose indented lines start with a tab → `09`; otherwise spaces to the next
    multiple of 4.
  - In a Latin-1 file, typing `한` → nothing typed, and the status line. Pasting `aé한b` → refused
    whole, naming `한`.
  - A BOM-less UTF-16LE file (`(guessed)`), and a UTF-8 file holding `FF` → F4's status line; ASCII
    typed with no question; the first `한` → the question on Cancel; Enter → nothing typed; Type →
    typed, and no second question for the file.
- EB-4 [ED-ops] **The caret.** → crosses 4 bytes over a UTF-16LE surrogate pair and 3 over UTF-8
  `한`. 갊 in UTF-16LE (`0a ac`) breaks no line. Home, End and Ctrl+← / → behave on such lines.
  CP949 after VM-4.
- EB-5 [ED-ops] **Undo.**
  - A typed word, a Backspace run, a paste and a Ctrl+K V → four undo steps, each restoring the caret
    and the block.
  - 100,001 single steps → the first one is gone.
  - Save, then Undo → unsaved again; Redo → the saved state again, the bytes equal to the file; no
    buffer copied across the Save (the buffers' identities unchanged).
  - Deleting a 200 MB block adds only its pieces to memory.
- EB-6 [ED-ops] **Blocks and the clipboard.** Ctrl+K, then each of B K C V Y H I U L T S, with and
  without Ctrl → its command. Ctrl+K then Esc, or then `x` → disarmed, nothing typed. No timeout.
  - A block over a UTF-8 `FF`, moved by Ctrl+K V → the byte unchanged at its new place.
  - The same block cut, then pasted elsewhere → the `FF` byte there, not `EF BF BD`. A paste of other
    text holding U+FFFD → refused whole, with the line.
- EB-7 [ED-ops] Find with U, W, B, N and G → Turbo's meanings. A needle across a piece boundary, and
  one across a line end, is found. Replace in UTF-16LE writes the encoded replacement.
- EB-8 [ED-ops] **CR LF is one** (E35), in UTF-8 and in UTF-16LE: → and ← never stop between CR and LF;
  Backspace at a line's start and Delete at a line's end remove both; Shift+← from a line's start
  selects both; Overwrite at a line's end inserts; Enter in a CR LF line → CR LF. A CR-only file is one
  line, and Enter writes LF.
  - In `61 0D 0A 62` (`a` CR LF `b`): a block of `a` alone, cut → `0D 0A 62`; a block whose end is
    placed between the CR and the LF (by a click or Shift+→ stepping there) → its end after the LF,
    so a cut leaves `62`.

**Keys (§5)**
- EK-1 [ED-ops] `edit::arm::decide` as a table over (armed state, press, Shift known, backend) →
  command:
  - F2 and F5 → Save, from a key and from a row click.
  - Window: Ctrl/⌘+S → Save; Shift+Ctrl/⌘+S → Save copy.
  - Terminal, Shift not known: Ctrl+S → `^S` armed, its line exactly `Save over <name>? F2 or a click
    on [Save] saves · C saves a copy · any other key cancels`; then F2 or a click on `[Save]` → Save;
    then C → Save copy; then S, `x`, Enter, Space, Esc, F5 or Ctrl+S → cancelled, nothing saved,
    nothing typed, and the line exactly `not saved — F2, Ctrl+K S or a click saves`.
  - Terminal, Shift known: Ctrl+S → Save; Shift+Ctrl+S → Save copy. Then a `WindowFocused`, and
    separately a resize → Shift not known again: Ctrl+S → `^S` armed.
  - Window: Shift known from the start, never forgotten.
  - Ctrl+5 with the editor holding the keyboard → no Save, and the line exactly `this key arrived as
    Ctrl+5 (Ctrl+] in some terminals) and saved nothing — F2, F5, Ctrl+K S, a click on 2Save, or F10
    F S saves`.
  - `^K` armed, then F2, F5 or a row click on a Save slot → Save; `^S` armed, then F5 → cancelled
    (E34).
  - Ctrl+K then S → Save. Every other row of §5.1 → its command, and no row outside §5.3's list
    yields Save.
- EK-2 [ED-layout] **The hook, per event.** With the editor holding the keyboard:
  - Cmd/Ctrl+5 → the digit with its modifiers, no F5, no text; held (each repeat a fresh press) → no
    F5 at all;
  - Cmd/Ctrl+5 pressed in the list and still held when F4 gives the editor the keyboard → its later
    repeats dropped, no F5 reaches the editor;
  - a real F5 and `1D` in one read → exactly one F5 (the real one) and one Ctrl+5;
  - a click on slot 5 → F5; Cmd/Ctrl+4, +6, +7 → F4, F6, F7 as today;
  - Alt+5 in the TUI (after XW-shell) → the same as Cmd/Ctrl+5.
  - In the list, Cmd/Ctrl+5 → F5, as today. A field with the typing → no alias, as today.
- EK-3 [ED-layout] **No Save from a key not meant as Save.** The TUI, through the terminal driver,
  with the editor open on an edited local fixture and on an edited remote one (`sftp-server`). Each
  input gets a fresh editor, and the switch is stubbed (`ESC g` is Alt+G):
  - Replay every row of *editor* §1.4's table, every byte 0x01–0x1F, and ESC followed by each
    printable ASCII character, under legacy encoding and under the kitty protocol; in the window
    driver, every Cmd/Ctrl+digit.
  - After an input that leaves a state behind — `0B` (Ctrl+K), `11` and `17` (close), `12`
    (Ctrl+R), `13` (Ctrl+S in a terminal), `1B` (Esc) — play a second key, each of Enter, Esc, `13`
    and every printable ASCII character; after `13 13`, a third likewise.
  - After each input or sequence not on §5.3's list → the file's bytes and mtime unchanged, no backup
    made, no `save` intent in the journal, and the journal not discarded.
  - F4 and `1D` in one read (the hook reads the focus the last pass left, `gui/mod.rs:788-789`) → no
    Save.
  - Positive controls, each saving once: `ESC [15~` (F5), `ESC O Q` (F2), `13 ESC O Q` (Ctrl+S,
    F2), `0B 73` (Ctrl+K s), `0B ESC [15~` (F5 while armed), a click on `5Save`; under kitty, after a
    `CSI 122;6u` (Ctrl+Shift+Z with Shift) has made Shift known, `CSI 115;5u` (Ctrl+S).
  - `13 63`, and under kitty with Shift known `CSI 115;6u` → `<stem>.edited<ext>` beside the file,
    and the file unchanged.
  - `13` then a word starting with S (`73 61 76 65`), then `0D` (Enter), then `20` (Space) → each
    cancelled: the file's bytes and mtime unchanged, and none of those characters typed.
- EK-4 [ED-layout] **Undo, Redo and close.**
  - Legacy `1A` after a typed word → the word undone, with the hint once a session; F10 E R brings
    it back. Kitty `CSI 122;6u` → Redo.
  - Ctrl+Q and Ctrl+W (legacy `11`, `17`; kitty; ⌘Q in the window) with unsaved edits → the close
    prompt, focus on Cancel; Enter → still editing, the edit there. With a clean editor → xtools
    closes.
- EK-5 [ED-layout] **Paste.** Legacy `16` → nothing changes, and the status row says `the terminal's
  paste: ⌘V / Ctrl+Shift+V`; Shift+Ins sent as `ESC [2;2~` → the same line. A bracketed paste of
  `a`, CR LF, `b` into a CRLF file → `61 0D 0A 62`; into an LF file → `61 0A 62`; one undo step each.
  A paste over 1 MiB is one step too. A paste equal to the last Copy over an `FF` byte → the byte.
- EK-6 [ED-layout] Every §5.1 row through the driver, in both encodings and in the window → its
  command (10 TK-1's form). `no_duplicate_bindings` passes with `Context::Editor`.

**The layout**
- EL-1 [ED-layout] 80×25 and 160×40, preview mode, the bar on a text file, F4 → the cell dump's
  frame, column (24 / 40 cells), tab line, title row and F-key row as *editor* §2.3's mock-ups, with
  49 / 113 text columns. Esc → a dump equal to the one before F4.
- EL-2 [ED-layout] Each row of §6.2 → its result: the right side active; dual mode; F4 before the bar
  has rested; from the Enter viewer's Text, Raw and Hex views (with unsaved hex edits → the prompt
  first); from a revision's viewer (the working file); with a pin; with `git_wide`; a folder (the
  stand-in VS Code / Zed launcher); a binary (the line); a 300 MiB text (the line); Shift+F4 (the
  launcher's argument).
- EL-3 [ED-layout] Every row of §6.3's table, in both tabs, with unsaved edits where a row asks: F3 on
  another text file in Dir → the prompt on Cancel; Enter there → still the first file; F2 on the
  edited file → refused; F9, F10, Ctrl/⌘+U → nothing; Ctrl/⌘+G → the line; Esc → the keyboard in
  the editor. F6 and a click on `[↕]` with the column hidden → shown. Outside a work tree the Git tab
  says so.
- EL-4 [ED-layout] **1:1** (10 TS-1 (a) and (c)): the editor with the Dir tab, the Git tab, the File
  menu open and Find typed, a Hangul line in UTF-16LE → 0 differences outside the preedit's rect;
  the 256-colour table holds.
- EL-5 [ED-layout] F4 on a 6 MiB UTF-8 Rust file → every text cell's foreground the same before and
  after F4 (E18).
- EL-6 [ED-layout] `git_wide` (E40): F4 from the git panel over both sides on an uncommitted file →
  the editor, `git_wide` off; Ctrl/⌘+Shift+G in the Git tab → the panel over the whole area, the
  editor underneath; again → back; Esc out of the editor → `git_wide` on, as before F4.

**The switch**
- EW-1 [ED-layout] One process, unsaved edits: F12 from the TUI → the window with the same buffer,
  caret, block, tab and `unsaved — kept`; Save there lands; F12 back → the TUI shows the saved text,
  clean. The same from the window to the TUI.
- EW-2 [ED-layout] Across machines (10 TW-2's stand-in ssh): unsaved edits, F12 → the save question
  (10 §7.4) on Cancel; Enter → stays; Save → switched once it lands, the editor open on HOST at the
  caret.

**Saving** (local, and against `sftp-server`, as the hex and image Saves' tests run)
- ES-1 [ED-save, ED-layout] **A Save and the views after it.** Edit, Save → the file holds the
  expected bytes, with owner and mode kept. The backup is the original as opened, made at the
  session's first Save only. The next baseline is the bytes staged. No edit is taken while the Save
  runs.
  - Then F4 → the Text view shows the saved bytes; Esc out of the editor → the preview, prepared
    again, shows them; for a remote file `SOPEN_STREAM_STATS` shows the old stream closed and a new
    one reading the first screen. `Replaced` never appears on the status row.
- ES-2 [ED-save, ED-dialogs] **Conflicts and the guard.**
  - Between the read and the Save, change the file: one byte (same size, mtime set back); its size;
    delete it; replace it with a link → each: nothing written over it, the dialog's line names the
    change, the focus on Cancel. Overwrite is offered for the first two only.
  - Overwrite → the changed version is in the backup folder, then the file is written. A change made
    between Overwrite's read and its replace → caught again.
  - Reload → the file as it is now, the journal gone. Save As… → 07 §7's modal.
  - **Local** (§9.1's local steps):
    - a test hook writing the file between the checks and the exchange → the intent shows
      `exchanging`, then `swapped back`; the other version in place byte for byte; the dialog;
    - the swap-back failing (a hook) → sopen's version in place, the other at
      `<name>.sopen-replaced-<stamp>`, the line `replaced; the other version is kept at <path>`, the
      editor still open with its text;
    - the process killed between the exchange and the check (a hook), once with the file unchanged
      and once changed by the hook → on the next F4: unchanged → the temp removed, the Save landed;
      changed → the displaced file renamed `<name>.sopen-replaced-<stamp>` and named; in neither case
      a file removed unhashed;
    - a filesystem double failing the exchange with `ENOSYS`, `EINVAL`, `ENOTSUP` and `EOPNOTSUPP` →
      today's route, and its line once per filesystem.
  - **Remote**, against `sftp-server`:
    - a test hook writing the file in place before the re-read before the rename (§9.1 step 3) → no
      replace, the link and the temp removed, the target unchanged byte for byte, the dialog with
      Reload · Overwrite · Save As… · Cancel;
    - a hook writing it in place after that check and before the rename → saved, the other version
      at `<name>.sopen-replaced-<stamp>`, the line naming it, no dialog;
    - killed after `linked` and before the rename, and again after `committed` and before `verified`
      → on reconnect: a link equal to the baseline removed; one changed by a hook renamed and named;
      never one removed unhashed;
    - the link's read failing (a hook) → `saved, but what it replaced could not be checked — kept at
      <link>`, the link still there; the link's rename failing → its own path named; its removal
      failing after a good check → `saved; remove <link> by hand`, and the next reconcile removes it;
    - a `link` failing with `EPERM`, `EMLINK` and `EDQUOT` (a hook), and a server without `hardlink@`
      → today's route, and its line once.
- ES-3 [ED-save] **The journal.**
  - Kill the process while typing → the next F4 offers Restore · Later · Discard; Restore → the edits
    up to the last kept generation, byte for byte.
  - Save, Undo past it, kill, Restore → the buffer as it was before the kill, unsaved against the
    saved file.
  - Edit, kill, replace the file, F4 → Restore rebuilds from the journal (the file is not read for
    it), with the `changed since` line; Save → the conflict dialog. The same with the file deleted →
    Save offers Save As… and Save copy.
  - A 50 MB paste → one `add-<n>` file, which later generations do not rewrite.
  - A slow-disk double holding `original`'s fsync → no `gen-<n>` published and the row `keeping…`
    until it returns, then `kept`. A full-disk double (`ENOSPC`) → no journal published, the row
    `unsaved — NOT kept on disk (no room for a copy of the file)`, and the close prompt still asks.
  - Edit, Save, a clean re-read (E30), edit, kill → Restore rebuilds against the new original
    (`k = 2`), whose bytes are the re-read file's; a journal whose generation names `k = 1` after
    `original` was replaced is refused, never applied.
  - The recovery row shows the journal's size. Later → the journal stays, and it is offered again at
    the file's next F4; nothing else removes it.
- ES-4 [ED-layout] **sopen's own changes.**
  - The editor's own Save → no re-read and no `changed on disk` line. A copy of another file into the
    folder → nothing.
  - A checkout of the file from the Git tab with a clean editor → the checked-out text, the undo
    line; with unsaved edits → the status line, then Save → the conflict dialog.
  - In the Dir tab, on a selection holding the edited file: Del then Enter, and Shift+Del then Enter
    → nothing deleted, the hint `editor.rs is open in the editor — Enter cancels; Tab, then Enter,
    deletes`. F8, then Tab and Enter → deleted; the editor stays, with the `deleted on disk` line; F2
    → the dialog without Overwrite.
- ES-5 [ED-dialogs] The close prompt at each exit of §6.4. A Save that lands closes; Discard deletes
  the journal; a running Save makes the exit wait. The close prompt, the conflict dialog, Ctrl/⌘+R's
  question and the encoding question each open on Cancel: Enter → Cancel; every letter → nothing.
- ES-6 [ED-layout] **The remote read.** A remote 6 MiB text, previewed, then F4 → `SOPEN_STREAM_STATS`
  `fetched=` grows by the file's size less what the preview read, within one 64 KiB block, and the
  window's channels never pass 2. A 300 MiB remote text → F4 refused with the line, nothing read.
- ES-7 [ED-layout] **Two editors on one file**: two xtools windows edit it; the first Save lands; the
  second → the conflict dialog; Overwrite there → the first window's version in the backup folder.

**Highlighting**
- EH-1 [ED-screen] Typing at line 5,128 of a 10,007-line Rust file → the edited line in its old
  colours, then the new ones when the worker replies; the frame never waits on syntect. An unclosed
  `/*` at line 1 → the rest is comment; closing it → the old colours back.
- EH-2 [ED-screen] A line over `LINE_MAX`, and text past `HIGHLIGHT_MAX` → plain, as in the viewer.
- EH-3 [ED-screen] The kept states' heap over a 16 MiB Rust file, measured with a counting allocator
  in the test → at most 32 MiB at 64-line spacing (E19), and the measure recorded in the PR.

**Git**
- EG-1 [ED-layout, ED-git] The Git tab at 24 cells → the graph's lanes and subjects, no row
  overflowing the column. Space on the edited file's row → staged, and the border letter `S`. F7 →
  the Commit dialog. The Git menu's five entries → 03's actions on the edited file.

**Routing**
- ER-1 [ED-route] `sopen <text>` with input and output terminals → xtools with the editor on it (the
  window before 10's flip, the TUI after). Esc → xtools ends with status 0; F4, then Esc → xtools
  still running; `sopen --tui <text>` likewise. A stand-in `EDITOR=sopen` caller waits until the
  editor closes. Through a stand-in forward to HOST → the command returns when HOST's sopen ends.
  `sopen <text> < /dev/null`, and inside a `while read` loop → the stand-in VS Code launcher, as
  today. `--app` and `--hex` as today. A 300 MiB text → the viewer, with the line.

**Owner receipts** [owner]
- EO-1 The look at 80×25 and 160×40, in kitty, a herdr pane and the window: screenshots someone
  looked at (AGENTS.md).
- EO-2 The Korean input source, in kitty, a herdr-app pane, a herdr pane on the Mac, and the window:
  - `한글` typed → the composition at the caret, and the encoding's bytes in the file (`ED 95 9C EA
    B8 80` in UTF-8, `5C D5 00 AE` in UTF-16LE);
  - Ctrl+K then S, and F10 then F, S, with the input source on → saved; `^S` then C (ㅊ) → a copy;
  - whether a lone jamo arrives at once (§5.5).
- EO-3 In a legacy terminal (Terminal.app, or tmux), in kitty, in herdr-app and in a herdr pane:
  Ctrl+] in the editor saves nothing; Ctrl+S shows `^S` until a Ctrl+Shift chord has arrived with
  Shift, then saves at once — which of the four ever gets there is recorded. Then the client is
  detached and reattached (tmux `detach` and `attach`; a herdr pane closed and reopened on the
  session), from a client that drops Shift → Ctrl+S shows `^S` again.

## 15. Test plan

- **The model units** (ED-buf, ED-enc, ED-ops, ED-save) are plain unit tests.
  - The tests generate their fixtures; there are no binary fixtures (08 §12).
  - A read through the stream is compared with the whole file (08 §12's "ranged equals whole").
  - E32's guard is tested on the local disk and against `sftp-server`, with test hooks that write
    the file between a check and the replace, and a filesystem double that refuses the exchange.
- **Key tests send what the platform sends**:
  - in the window, the key with its physical key and then its typed text, in one frame (08 §12,
    `Driver::press`);
  - in a terminal, the bytes, through 10 §15's terminal driver.
  - EK-3's replay is the complement test: every input and two-key sequence not on §5.3's list asserts
    no write, each on a fresh editor, and the positive controls keep it from passing vacuously.
- **Remote saves** run against `sftp-server` (`SOPEN_REQUIRE_SFTP_SERVER=1`) and the `Ranged` Fake,
  as 07's tests do. No ssh or network in tests (AGENTS.md); EW-2 and ER-1's forward use 10's stand-in
  ssh.
- **Screens**: cell dumps through the test kit (10 §15, XW-kit) in both backends, which is 10 TS-4
  for X5; `SOPEN_CELLS` receipts.
- **Receipt hooks** (ED-layout, AGENTS.md rows):
  - `SOPEN_STATE` `edit=NAME`: the browser opens the editor on that entry, as F4 would, once listed;
  - `exit=editor`: xtools ends when that editor closes (E33's start, which ED-route's text route
    uses);
  - `tab=git`, `at=OFFSET` (the caret's byte), and `close=ask` for the editor;
  - `SOPEN_KEYS` plays the editor's keys.
- **The gate** is the loop's: `just check` with `SOPEN_REQUIRE_FFMPEG=1 SOPEN_REQUIRE_SFTP_SERVER=1`,
  and the Linux gate's two clippy runs. A unit that changes a screen brings a cell dump and a
  screenshot someone looked at.

## 16. Work units

*editor* §8's units, cut to disjoint files. Sizes are product lines [추정]; the prototype gives a
floor only for ED-buf. **This is the one ownership table**: every file and every amended text (§13)
has one owner at a time, and a file shared by two units passes from one to the next in the order
given (10 §17's rule). `mod` lines in a shared file: whichever unit merges second rebases (08 §13).
**X3** and **X4** are 10 §16's steps; their units are cut only once X1b has merged (10 §17), so this
table waits on "X3's unit" and "X4's units".

| ID | Unit | Owns | Size | After | Ships alone as |
|---|---|---|---|---|---|
| ED-0 | Scaffold: the `edit` module and its docs. Each later unit adds its own `mod` line, so the scaffold declares no file that does not exist yet | `src/edit.rs` (new: module docs only); `src/lib.rs` (`pub mod edit;` beside `pub mod encoding;`, `lib.rs:22`) | ≈15 | — | no change — **starts now** |
| ED-buf | §7.1 and §7.3: the original and the add buffer with stable identities, pieces, the original's line starts by the encoding's LF unit, insert, delete, line ↔ offset, serialise; undo steps as pieces, their groups and bound. EB-1, EB-5 | `src/edit/buffer.rs`, `src/edit/buffer/tests.rs` (new); `src/edit.rs` (its `mod buffer;` line) | ≈450 (the prototype's table: 150) | ED-0 | no change — **starts now** |
| ED-enc | §7.2's encode side: `Encoding::encode` for every encoding but CP949, as an `impl Encoding` in a child module, refusing what it cannot hold; then CP949 through encoding_rs's EUC-KR encoder. EB-2 | `src/encoding/encode.rs`, `src/encoding/encode/tests.rs` (new); `src/encoding.rs` (its `mod encode;` line only: VM-1's file, whose `cp949` slot, `encoding.rs:713`, is VM-4's, as is `encoding_rs` in `Cargo.toml`, 08 §13) | ≈150 | — (VM-1 has merged); the CP949 part after VM-4 | no change — **starts now**, CP949 later |
| ED-ops | §5's commands over the buffer: the caret by characters, lines and words; the block; the clipboard's pieces (E36); CR LF as one (E35); the Ctrl+K and `^S` armings and their precedence (`edit::arm::decide`, E31, E34); find and replace over decoded characters; autoindent, the tab and line-ending rules, paste; §5.5's jamo match; §7.2's encoding question as a state; 08's view-key state of an editing Text view. EB-3, EB-4, EB-6–EB-8, EK-1. Text: 08 §4 | `src/edit/ops.rs`, `src/edit/ops/tests.rs`, `src/edit/arm.rs`, `src/edit/arm/tests.rs` (new); `src/edit.rs` (its `mod ops;` and `mod arm;` lines); `src/view_keys.rs` (an editing Text state in the DigitsAreData row, `view_keys.rs:45-71`) and `src/view_keys/tests.rs` (its rows); `.prd/08-viewer.md` §4 | ≈800 | ED-buf, ED-enc | no change — **starts now** |
| ED-save | §7.4 and §9 without a screen. The whole read takes a local path or any byte reader (the stream's `Bulk` reader in the product, the `Ranged` Fake in tests) and records the baseline; Save by §9.1's steps; `replace_guarded` (E32) with its intent phases (`exchanging`, `swapped back`, `linked`, `committed`, `verified`, `kept`, beside `Step`, `remote/mod.rs:83-104`) and their reconcile; Save As, Save copy, the backup; the conflict outcomes and Overwrite; `journal::Kind::Text`, self-contained, with `original` (durable before any generation, replaced per re-read generation `k`), `add-<n>`, the recovery row's size and Restore's rebuild. A Save's result says what landed; ED-layout turns it into §9.3. ES-1's model half, ES-2, ES-3 | `src/edit/file.rs`, `src/edit/file/tests.rs` (new); `src/edit.rs` (its `mod file;` line); `src/journal.rs` (`Kind::Text`, `journal.rs:233-237`, and its format), `src/journal/tests.rs` (its tests); `src/remote/mod.rs` (`replace_guarded` and its default: the link guard, else `write_atomic_with`), `src/remote/local.rs` (the exchange), `src/remote/tests.rs` (their rows). No other open unit owns these files | ≈850 | ED-buf | no change — **starts now** (its tests run against `sftp-server`, as the hex and image Saves' do) |
| ED-screen | §4 in cells: frame, gutter, rows with syntect's runs through a line source that both the text document and the buffer give, caret, block, scroll bar, the status row's fields and armings, the title row's menu names, the F-key row. Also: the 64-line kept states and EH-3; the window's IME output and preedit (E10, E20); the text document handing out a `Bulk` reader of its `Source`. EH-1–EH-3 | `src/gui/edit.rs`, `src/gui/edit/tests.rs` (new); `src/gui/text.rs` (only `Highlight`'s line-source seam and the document's Bulk reader: VM-2's file, then X3's, passing here after them); `src/gui/mod.rs` (its `mod edit;` line) | ≈900 | X3's unit (the text view in cells), XW-list (the title, status and F-key rows in cells), ED-ops | no change: nothing opens it yet |
| ED-dialogs | In cells, with E6's focus rule: Find / Replace's confirmation, Go to line, the F10 menus, the close prompt (§9.6), the conflict dialog (§9.2), Ctrl/⌘+R's question and the encoding question (§7.2). ES-5 | `src/gui/edit/dialogs.rs`, `src/gui/edit/dialogs/tests.rs` (new); `src/gui/edit.rs` (its `mod dialogs;` line, after ED-screen) | ≈500 | XW-dialogs, ED-screen, ED-save | no change |
| ED-layout | §6, §5's binding and §9.3–§9.4: rule 1a before `git_wide`; the column and every key of §6.3; F4 and Shift+F4 in every context; every exit through §9.6; §9.3 after a Save; §9.4's stat and the delete dialog's line; the editor's rows under `Context::Editor`; the hook's E5 rule and Shift known (E31); the translator's Ctrl+V (E9); the switch state's `editor` fields; the editor in `close_requested`; F1's terminal lines; the receipt hooks, `exit=editor` among them. EK-2–EK-6, EL-1–EL-6, EW-1, EW-2, ES-1's view half, ES-4, ES-6, ES-7. Texts: 01 §9, A11, A2's F4 sentence; 02 §3.1, §4.4; 03 §6.2, §9.1; 04 §2 row 4, §6.1; 07 §1, §2, §3, §10; 08 §7.5; AGENTS.md; the README's F4 lines | `src/gui/browser/view.rs` (rule 1a and its table test); `src/gui/browser.rs` (`panels_ui`'s editor branch, `edit`, `close_requested`, the receipt hooks); `src/gui/browser/keys.rs` (the F4 rows `:456, 496, 503`, `Context::Editor` and its rows); `src/gui/browser/ops.rs` (the delete dialog's edited-file line); `src/gui/mod.rs` (the E5 rule and Shift known in the hook only); `src/gui/term_input.rs` and `src/gui/term_input/tests.rs` (the Ctrl+V branch, `:236`, and its rows); `src/gui/term.rs` (the switch state's `editor` fields only); `src/gui/help.rs` (the editor's terminal lines); `src/gui/browser/tests/s7.rs` (new) and `src/gui/browser/tests.rs` (its `mod` line); the texts above | ≈550 | XW-grid, XW-list, XW-dialogs (`ops.rs`), XW-shell and 04's KL-W4 (`gui/mod.rs`), VM-7's input half (`keys.rs`, `gui/mod.rs`), XW-term and X2's units (`term.rs`, the switch), X4's units (the tree and the git panel in cells), VM-D0 and KL-W6 (04, the README, AGENTS.md), ED-dialogs | **X5**: F4 edits in place |
| ED-git | §10: the editor's Git menu and the border's status letter. EG-1 | `src/gui/edit/git.rs` (new); `src/gui/edit.rs` (its `mod git;` line and the letter, after ED-dialogs); `src/gui/browser/git_host.rs` (the editor's entry to the git host only) | ≈150 | ED-layout | the Git menu works |
| ED-route | §11: the CLI's text route, started as the editor and ending with it (through ED-layout's `edit=` and `exit=editor`). ER-1. Texts: 01 §3 and A2's CLI part; 10 §11 row 5 and T39 for text; the README's `sopen file.txt` and `EDITOR=sopen` lines | `src/main.rs` (the text route, `main.rs:678-684`), `tests/cli.rs` (its rows), the texts above | ≈100 | ED-layout, XW-term (which adds `--tui` to `main.rs`), X2's units (which add the switch's flags there); before XF, which rebases `main.rs`, 10 §11 and the README onto it | `sopen file.txt` opens xtools's editor |

- **Order**:
  1. **Now**, with no screen: ED-0, then ED-buf and ED-enc (its UTF and one-byte parts) side by side,
     then ED-ops and ED-save side by side. ED-enc's CP949 part comes after VM-4.
  2. ED-screen after X3's unit, XW-list and ED-ops.
  3. ED-dialogs after XW-dialogs, ED-screen and ED-save.
  4. ED-layout after the units its row names and ED-dialogs. It ships X5 with every text that
     describes F4 and an editable text view, so F4 never ships without its prompts and 07 §1 never
     says the text viewer is read-only once it is not.
  5. ED-git after ED-layout.
  6. ED-route last, after ED-layout, XW-term and X2's units, before XF.
- **Not a unit**: editing above 256 MiB through `copy-data` (*editor* §3.5). It needs its own content
  check, and is designed only if the owner asks.

## Tensions

- **01 §9 (`01-spec.md:1249`), 07 §1 (`07-streaming-edit.md:54`) and AGENTS.md:105 vs message 21.**
  "Own text editor" is out of scope, "the text viewer stays read-only", and "edit features come only
  from owner messages". Message 21 is the owner message that last rule waits for; each text gets its
  line (§13), all with ED-layout.
- **07 §2's table vs the editor.** Hex never changes the size, and text has no row. The editor adds a
  Text row whose size changes, saved by a whole-file replace rather than 07 §4.2's patch route.
- **07 §1 ("best effort … not a compare-and-swap") and the hex and image Saves vs E32.** The editor's
  local Save becomes a conditional commit and its remote Save checks what the rename replaced; the
  hex and image windows keep today's check-then-rename. Whether they take `replace_guarded` too is
  theirs to decide.
- **04 row 4 and `keys.rs:456, 496, 503` vs "디폴트로 로컬 에디터".** F4 = VS Code / Zed in the list,
  the viewer and the git panel. F4 becomes the built-in editor and Shift+F4 the old route. 02 §4.4's
  "F4 changes nothing" for a preview (`preview.rs:16-19`) becomes "F4 hands the preview over".
- **10 §11 row 5 and T39 vs §11.** Text routes as its folder does, except where row 6 keeps today's
  routes; and `--tui <text file>` opens the editor and ends with it (E33), where T39 opens the viewer
  and returns to the list.
- **The note's two F4-on-a-folder rules.** *editor* §2.2 opens nothing for F4 on a folder; *editor*
  §7.3 keeps VS Code / Zed for the git panel's top level. E3 takes §7.3's rule for both: a folder
  keeps today's route.
- **04 §6.1 and *editor* §1.4 vs E5.** 04 §6.1 says "Cmd/Ctrl+1…0 act as F1…F10 in every window", and
  the note left Ctrl+] = F5 = Save as a line in the help ("Not introduced here"). E5 closes it,
  following the orchestrator's decision 3: while the editor has the keyboard no digit makes F2 or F5.
- **10 §9 ("Alt+digits never act while a text field has the typing") vs the editor.** The editor is
  not a field — its F-keys act — so Alt+digits act there, except that none makes F2 or F5 (E5).
- **The note's Save copy Tension vs E31.** Under legacy, Shift+Ctrl+S arrives as Ctrl+S and saved over
  the file; the note answered with the menu and a help line. E31 arms Ctrl+S in a terminal until Shift
  is known, and only F2 or the prompt's Save confirms the overwrite: a word typed after Ctrl+Shift+S
  cancels it untyped, so no letter can write over the file.
- **The same hazards outside the editor.** In a terminal the hex, image and media windows' F5 Save is
  also reachable by Ctrl+], and their Save copy arrives as Save [M *editor* §1.4]. E5's hook rule and
  E31's arming are the fix there too, but X3 and X6 decide that, not this document.
- **F2 in the Text view vs F2 in the editor.** The view's F2 toggles wrap (01 A11, "W or F2"); the
  editor's F2 is Turbo's Save. A mis-press costs a wrap toggle, or a checked Save with a backup; never
  lost text.
- **Turbo's keys vs sopen's** (*editor* §1.4): Turbo's F3 Open, F5 Zoom, F6 Next window and Alt+X
  Exit. Turbo C++'s Alternate set makes Ctrl+Home / End the window's top and bottom; Notepads and the
  CUA set make them the file's start and end, and §5.1 takes the latter.
- **"그대로" vs E16.** *editor* §2.1 has the preview's rows stay "unchanged until the first key". With
  no wrap that holds where lines fit the 49 / 113 text columns; a long line the preview wrapped shows
  cut in the editor.
- ***editor* §5.1 step 6 and §3.4 vs E17 and E25.** The note rebases the table on the saved bytes and
  keeps only the edits since the last Save. E17 keeps the piece list and its buffers across a Save, so
  undo crosses it with no copy, and E25's journal keeps the original, so undo past a Save survives a
  crash — beyond 07 §8's image rule (`07-streaming-edit.md:732-736`), at the cost of the file's size
  on disk per session.
- **07 §3's Restore vs E25.** 07 applies nothing to a file that changed, and the edits stay in the
  journal; the text journal rebuilds the buffer without the file and lets Save ask.
- ***editor* §8's ED-git vs `graph_view.rs:5-7, 28, 102`.** The note planned a narrow layout; 03's
  graph already narrows, so E27 builds none. The note's mock-ups show a short hash beside the subject
  (*editor* §2.3), but at 24 cells the rule drops Commit before the description. EG-1's dump decides.
- ***editor* §8's ED-ops ("the needle encoded, matched at unit offsets") vs Find's U and W.** Both
  need characters, so E28 matches decoded characters.
- ***editor* §5.2 (Save As or Reload after a conflict) vs decision 6.** E22 adds Overwrite, and E23
  backs up the version it replaces.
- ***editor* §7.1 ("`View::saves()` adds Text while editing") vs E14.** The editor's `Origin` is its
  own and is given back when the editor closes. An editor is never hidden, so `views.rs:58-60` stays
  as it is.
- **E36 vs a real U+FFFD.** A paste holding U+FFFD that is not the editor's own last copy is refused,
  even in UTF-8, where U+FFFD can be written; such a character is far more often a byte lost in a
  copy than one meant.
- **Highlighting a keystroke** costs 3–13 ms on a worker with syntect, against 0.5 ms with tree-sitter
  [M]. E18 keeps syntect for one colour mapping and 0 bytes, and accepts a frame of old colours on the
  edited line.
- **The Mac window's ⌃ rows** (§5.1) are a choice, not a measurement [추정]: ⌃+K there is not ⌘+K,
  which the git panel binds (03 §9.2) only while the git panel has the keyboard.
