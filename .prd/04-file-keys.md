---
title: sopen — file keys and function keys
type: spec
tags: [sopen, spec, browser, keys, file-operations]
---

# sopen — file keys and function keys (04)

Status: planned (2026-11-20)

Owner message 15 (spec §1; [`browser/ssot.md`](browser/ssot.md) items 5 and 6): F2 renames, F5 copies,
F6 moves; the rest of the default keys follow the most common file-manager convention; and the
function keys act as F1, F2, … inside sopen on a MacBook, Windows and Linux, where that is possible.
The survey, the measurements and the reasons are in [`research/keys.md`](research/keys.md); this
document fixes the contract.

Amends, when it lands: 01 §3a (the list's keys and F-key row; "the browser views" of message 12),
AGENTS.md "Rules" (file operations exist and never overwrite).

## 1. What changes

- The file list gets rename, copy, move, make folder and delete (§5), selection (§4), and the key
  map below (§3).
- Every sopen window's F-key row becomes clickable, and Cmd/Ctrl+1…0 act as F1…F10 everywhere (§6.1).
- On macOS, while a sopen window is focused, the keyboard's top row sends F1…F12 without fn (§6.2).
- The directory tree's key is F10 (Alt+F10 too); the tree itself is [`05-directory-tree.md`](05-directory-tree.md).

## 2. The F-key row of the file list

```
1Help  2Rename 3View  4Edit  5Copy  6Move  7MkDir 8Delete 9Dual  10Tree
```

| Slot | Key | Action | Changes files | Also |
|---|---|---|---|---|
| 1 | F1 | Help: a read-only screen listing the keys of where the keyboard is (list, viewer, git panel, tree) | no | — |
| 2 | F2 | Rename (§5.3) | yes | Shift+F6 |
| 3 | F3 | View (01 §3a, unchanged) | no | Enter on a file |
| 4 | F4 | Edit: VS Code / Zed (01 A2, unchanged) | no | — |
| 5 | F5 | Copy to the other panel's folder (§5.2) | yes | — |
| 6 | F6 | Move to the other panel's folder (§5.2) | yes | — |
| 7 | F7 | Make folder (§5.4) | yes | — |
| 8 | F8 | Delete (§5.5) | yes | Del; ⌘⌫ (macOS only); Shift+F8 / Shift+Del: delete without the Trash |
| 9 | F9 | Switch the browser's mode (02 §2); the label names the mode it switches **to**: `Dual` in preview mode, `Prevw` in dual mode | no | — |
| 10 | F10 | Directory tree (05) | no | Alt+F10 |

- F1, F7 and F8 are what all six surveyed Norton-family managers use; F2/F5/F6 are the owner's; F10 is
  the owner's word for the tree and Alt+F10 keeps Norton Commander's and Total Commander's tree key
  (keys.md §2, §3.1). F11 and F12 stay unused (macOS takes F11 for "show desktop").
- **The row changes with the context**, as PC Shell's does ("the functions on it will change
  depending upon what you are doing", PC Tools 6 manual p. 67, keys.md §3.3). In a viewer the slots
  keep the viewer's meanings — 3 Exit, 4 Edit, and the hex window's 5 Save, 6 Goto, 8 Asc/hx, the
  image window's 5 Save — 1 is Help everywhere, and 2, 7, 9, 10 are empty. The git panel's row is 03 §9.1's; the tree's is
  05 §7's. A mis-press of 5 or 6 in the list costs a dialog, never a write (§5.1).

## 3. Matching keys

`src/gui/browser/keys.rs` holds the browser's one table: each binding is a key, its modifiers, the
contexts where it acts (list, viewer, git panel, tree, a dialog) and its `Command` (02 §7). Rules:

1. **Exact modifiers.** A binding matches only when the pressed modifiers are exactly its own: F5 is
   not Shift+F5, Alt+F2 is not F2, Shift+F10 is not F10 — a key combination with no row does nothing
   (rule 4). egui's `consume_key` matches logically and ignores extra Shift and Alt (keys.md §3.2), so
   the table does not use it for its keys: it reads the frame's key events, compares the modifiers
   exactly, and **consumes the events it matches** (removes them from the frame's input, as
   `consume_key` does), so no widget or later pass acts on the same key.
2. **Characters come from typed text.** `+`, `-` and `*` need Shift on a US layout and sit elsewhere
   on others, so they are read from the frame's typed text (`Event::Text`), not as keys; so are the
   jump letters.
3. **Cmd/Ctrl** is egui's `COMMAND` (⌘ on macOS, Ctrl elsewhere); one binding serves both. A
   macOS-only key (⌘⌫) uses `mac_cmd`.
4. **One table, and text fields own their typing**: a key acts in the contexts its row names and
   nowhere else; a key with no row in a context does nothing there. While a field has the keyboard
   (rename, a dialog's path, the hex window's Go to, find), no plain F-key, Cmd/Ctrl+digit or list key
   acts (01 §3a's rule, extended to every key of this table). A preview never takes keys (02 §4.4).
5. **Who writes the table**: S1-A (02 §8) writes every context's rows up front, from 02 §3.2, 03 §9, this
   section and 05 §1; a later work unit changes a row only through a declared hand-over (02 §8).

### 3.1 The list's other keys

| Key | Action | Convention (keys.md §3.2) |
|---|---|---|
| Tab, Shift+Tab | the other side (02 §3.2) | all commanders |
| Enter, Backspace | open / up (01 §3a); Cmd/Ctrl+PgUp also goes up | TC, mc, Far, DC, Krusader |
| Space, Ins | select / unselect the bar's entry and step down | Ins: NC, TC, mc, Far, DC, Krusader; Space: TC, DC, Krusader (a MacBook has no Ins) |
| `+` · `-` · `*` | select a group… · unselect a group… · invert the selection | Gray +/−/\* (keypad and main row look alike in egui, so these three stop being jump letters) |
| Cmd/Ctrl+A | select all | TC, DC, Krusader, Dolphin, Explorer, Finder |
| Cmd/Ctrl+R | re-read the panel (and drop its folder's cached git detection, 03 §2.2) | TC, mc, Far, DC, Krusader |
| Cmd/Ctrl+U | swap the panels | NC, TC, mc, Far, DC, Krusader |
| Cmd/Ctrl+← / → | the other panel to this panel's folder (← shows it in the left panel, → in the right) | TC, DC |
| a typed letter | jump to the next name starting with it (01 §3a; not `+ - *`, not a space) | type-ahead |
| Esc | stop a running fetch or file operation; else unpin the git panel (02 §3.2) | — |
| Cmd/Ctrl+1…9, 0 | F1…F10 (§6.1) | mc's Esc+digit (Esc is taken in sopen) |

- **Checked against what exists**: no Cmd/Ctrl+A, R, U, ←/→ or Cmd/Ctrl+digit is bound in any
  sopen window today; plain Space is the media window's play key, but that window has its own pass
  (keys.md §3.2).
- **Not now** (no such feature, or the measured desktops take the key): sort keys (Ctrl+F3…F6: KDE
  switches desktops with Ctrl+F1–F4), show hidden (sopen always shows dot files, dimmed), properties,
  find files, pack, a terminal, copy path, a file clipboard (Cmd/Ctrl+C/X/V).

## 4. Selection

- Selected entries draw their text yellow, as PC Shell draws them; `..` is never selected.
- `+` and `-` open a one-line dialog with a pattern (`*` and `?` wildcards, any case; default `*`) and
  select or unselect the matching entries; `*` inverts files and folders alike.
- The foot shows, before the counts, `N selected = X bytes` (the files' bytes; folders count as
  entries) while anything is selected.
- Copy, Move and Delete act on the selection when there is one, else on the bar's entry. Rename and
  Make folder never use the selection.
- A re-read keeps the selection of names still there; entering another folder clears it.

## 5. File operations

### 5.1 Rules for all of them

- **Never overwrite.** No operation replaces or writes through anything that exists at its target —
  a file, a folder, a link — locally or on a remote (AGENTS.md "Never overwrite silently", 01 §5).
  A name in the way stops that item and asks (§5.2).
- **Asked first**, the conventional way: every operation opens its dialog; Enter goes, Esc cancels.
- **Nothing half-made under a final name.** A copied file is complete before it gets its name (§5.6);
  a server that cannot publish that way is refused for copy and move, with the reason. A local volume
  that can publish nothing atomically — neither an exclusive rename nor links (exFAT; sopen probes
  each target folder once) — gets the copy written straight into its final name, created
  exclusively (still never replacing), and the report says so. There a partial file is left under
  the final name only when sopen is interrupted (killed, a power cut); a copy that sopen stops (Esc)
  or sees fail is removed while that name is still the file it wrote (its inode, by its
  descriptor), and otherwise left and named; a move keeps its source until the copy is read back
  and made durable. **After a kill, that partial file is unmarked**: it sits under the final name
  with no `.sopen-` mark (unlike a staged temp), so the next operation counts it as a name in the
  way and asks Skip / Rename like for any other file; nothing in the listing says it is partial —
  a size or content that looks wrong is the only sign.
- **Names sopen puts aside**, in the target folder (the name part shortened to keep within 255
  bytes); one left by a sopen that was killed (`kill -9`, a power cut) stays under it:
  - `.<name>.sopen-<16 hex>` — a copy being staged: never taken for a copy; removing it is safe.
  - `.<name>.sopen-renaming-<16 hex>` — the middle name of a two-step rename (§5.3): it holds the
    **original**; rename it back to its name, never remove it.
  - `.sopen-probe-<16 hex>` — sopen's publish probe (an empty file); removing it is safe. sopen
    removes its own only while they are still its own, and names any it leaves.
- **Durable before a source goes**: each copied file is flushed through its own descriptor (`fsync`).
  A moved item is copied and read back (§5.2) and its sources then wait, with those of the items
  before it, in a **batch** — instead of one barrier per item, which made 500 selected small files
  cost 10.8 ms each (5.4 s against 0.13 s for `mv`). The batch's barrier flushes every target
  folder that received an entry, deepest first (`fsync` of the folder: a file's own `fsync` does not
  make its new name durable), then on macOS the drive (one `F_FULLFSYNC`; a plain `fsync` only where
  the volume says it does not support it), and only then are the batch's sources removed. A batch
  is flushed at 64 items; 1 s after its first item (looked at between items and between a folder's
  entries — one large file delays it); before any name-in-the-way question (no checked item waits
  on an answer with both copies in place); on Esc (the barrier first, then the sources of the items
  already checked go, and nothing new starts); and at the end, where one more barrier over
  everything the operation wrote runs after the last removal. A kill at any moment therefore finds
  each moved item either still whole at its source or copied, read back and durable. **A folder
  `fsync` that is refused is not durability, whatever the error** (`EINVAL`, `ENOTSUP` and
  `EOPNOTSUPP` included): the barrier fails, the batch's sources stay, and the report says the copy
  exists but this filesystem cannot confirm it is on disk. A copy has no source to keep: its end
  barrier's failure is a note.
- **Connections of its own** per operation, so the panel's listing goes on meanwhile: one when
  source and target are on one host, one per host otherwise. They come from a per-host connector
  of the file operations' own (`browser::OpsConnect`: this machine's file layer, or SFTP to the
  host named in the target over its ssh master) — not the viewers' `Connect`, which opens only
  the window's own host and has no file-operation primitives (`remote::ops::FileOps`).
- **Progress and stopping**: an operation on more than one item, or on a file over 8 MiB, shows
  `copying 3/12 · name 45% · 12 MB / 30 MB · Esc stops` on the status row. A same-host copy runs the
  `copy-data` request in chunks of 8 MiB (`copy_to(n)`, not one `copy_all_to`), so progress moves and
  Esc acts between chunks. **The unit is the file**: a file is either published whole under its final
  name (committed) or not at all. Esc stops after removing the unfinished file's temp; committed files
  stay, also inside a folder being copied, and the report lists the folder as partial.
- **Reported whole**: when an operation ends — done, stopped or failed — a dialog lists what was
  done, skipped, renamed, failed (with the reason) and not reached; a fully successful operation on
  one item just says so on the status row.
- **After**: both panels re-read (bar kept on the same name), and `BrowserEvent::FilesChanged`
  tells a git panel of those folders to refresh (02 §7).
- **Links** are copied and moved as links (`readlink`, then `Fs::symlink(target, link)` — the library
  already writes the request in OpenSSH's order, so sopen never swaps the two), never followed. Fifos,
  sockets and devices are skipped and listed.
- **A source that changes during its copy** (size or mtime differs after the read) fails that item,
  named; its partial copy is removed.
- **Before removing a source** (Move), sopen checks it again: same type, size and mtime as when it
  was read, and locally the same device and inode. A freed inode's number can be given to the next
  file at once (ext4 does), so a number alone proves nothing: **the source's descriptor is held open
  from the moment it was read until its removal** — on the entry itself, never what a link points at
  (`O_PATH|O_NOFOLLOW` on Linux; a read-only `O_NOFOLLOW|O_NONBLOCK` open, `O_SYMLINK` for a link,
  on macOS) — and the name is compared with `fstat` of that descriptor. The descriptor is taken only
  of the entry whose identity was read, in full (device, inode and ctime, compared when the name is
  looked at again and once more on the opened descriptor): an entry put under the name in between,
  on the same inode number or not, is not held as if it were the one read, and the source is then
  kept with "another file has its name now". Only a regular file or a link is ever opened for this:
  opening a device, a fifo or a socket can have effects of its own. An operation holds at most 64
  such descriptors at once — a bound on what it holds itself, not a promise that the process never
  runs out of descriptors. Past that, or where none can be had (too many open files, an entry the
  account cannot open, another type of entry), the device, inode and status-change time (ctime) are
  compared instead. That comparison is best effort — as fine as the filesystem's timestamps and as
  true as its clock — and errs to keeping: unlinking one hard link of a file changes the ctime of
  the others, so a hard-linked sibling is then kept with both copies said to exist — never lost.
  **The copy is looked at again too.** What it is known by is noted **before its read-back
  begins** — its type, size and mtime, and locally the device, inode and ctime its own descriptor
  reports (best effort: bytes written into it without changing its size, within one tick of the
  filesystem's clock — or over SFTP within the same second — are not seen) — and never renewed: the name must carry exactly that when the read-back and the
  source's check are over (else "its copy at <path> changed while it was checked; the source
  stays") and again right before its source goes (a batch puts that as much as 1 s later). A link
  that was made is the copy only while it is a symlink with the text that was copied; a file put in
  its place keeps the source. A copy that is gone or replaced keeps
  its source: "its copy at <path> changed before the source was removed; the source stays". The
  same holds for a
  copy's own temp (the descriptor it was written through stays open through the read-back and any
  cleanup) and for the old name of a link-and-unlink rename (§5.6). What is compared is what the
  descriptor reports at that moment, never a number noted earlier: on FAT and exFAT an empty file's
  inode number changes with its name (*verified, macOS, 2026-11-20*: `fstat` of the descriptor
  followed the file through a rename; a file with data kept its number). Anything else keeps the
  source and reports both copies. Over SFTP there is no inode and mtime has whole seconds, so a
  writer that rewrites a file to the same size within the same second is not seen; this is the same
  best-effort limit as 01 §5's.
- **Metadata**: a copy keeps the file's permission bits and times, and a folder's bits and times are
  set after its contents (so a read-only folder can still be filled). A failure to set metadata is a
  note in the report, not a failure of the copy; a read-only target folder fails the item with the
  server's reason.
- **Names** must be valid: not empty, no `/`, not `.` or `..`, at most 255 bytes. Entries whose name
  is not UTF-8 are never acted on (01 §3a).

### 5.2 Copy (F5) and Move (F6)

- The dialog: what (the bar's name, or `N items`), and a **target** field prefilled with the other
  panel's own folder in sopen's command-line form — `host:/abs` for a remote, a plain path for this
  machine (01 A1) — even when the other side shows a preview or the git panel (02 §7 `OpsTarget`;
  the dialog names it). The target may be edited to any folder on any host sopen can reach: a
  relative path is relative to the source folder; a missing folder is an error (none is created).
- **A name that exists at the target** stops that item with a dialog: **Skip**, **Rename** (the next
  free `<stem>-N<ext>` prefilled, editable), **Cancel** (the rest is not done; done items stay), and
  "the same for the rest" (off). Overwrite is never offered.
- Copying a folder copies everything inside it (the walk does not cross into a link). Copying into
  itself or below itself is refused before anything happens.
- **Is the target the source, or inside it?** Decided on resolved paths (`realpath` of both), and
  locally also by device and inode of the target and its ancestors, so a link or an alias cannot lead
  a folder into itself. Refused before anything happens. Over two connections (another host's path,
  or two sessions to one machine) it is refused when the resolved paths nest and both report one
  filesystem id (`statvfs`), or either reports none: sopen cannot tell whether both sides are the
  same machine, so the dialog asks for a folder outside the source (one id on two machines can only
  refuse a safe copy, never let one through). A folder is also walked before anything is made, so a
  copy never copies what it made itself.
- **Is a name taken?** Decided by the filesystem — an exclusive create or a link failing with "exists"
  — never by comparing strings: two names that look different may be one entry (case-insensitive or
  Unicode-normalising filesystems) and two that look alike may be two.
- **Move** is a rename where one is possible (same host and filesystem, §5.6) — for a remote folder
  on the same host only from slice S6; before S6, F6 of a remote folder moves it by copy, check and
  remove, with the rules below. Otherwise it is a copy, then a check, then the removal of the source.
  **The check reads the published copy back from the target**, in full, on every path and at any
  size, and compares its SHA-256 with that of the bytes read from the source (where the host copied
  them itself — `copy-data`, `copy_file_range` — the source is read again); a remote target so costs
  a move a second transfer, accepted for data safety: a **remote folder** moved by copy (every
  remote folder until S6) downloads the source as well as the copy, about twice its size, where
  `copy-data` alone moved nothing through this machine. A copy that differs goes and its source stays;
  Esc while checking stops with both copies there. **No source is removed before its whole item is
  copied and checked**: for a selected folder, every file below it first, then the source removed
  bottom-up.
  A stop or a failure during the copy removes no source at all (committed copies stay and the report
  says the target folder is partial). **Esc and the batch**: Esc flushes the batch — its barrier
  runs, and then the removals of that flushed batch run to their end; **Esc during those removals
  does not stop them**, because every item in the batch was already copied, read back and made
  durable by that barrier (each source and each copy is still checked again right before the
  removal, and a folder's sources go bottom-up). **An item that is not in a flushed batch keeps its
  source**: the one Esc interrupted (its copy, if published, is reported with both copies in place)
  and every item not reached. A source that cannot be removed after its copy stays, and the report
  says both copies exist.

### 5.3 Rename (F2, Shift+F6)

- Inline, in the bar's row: the name becomes a field (the whole name selected, the extension after
  it); Enter renames, Esc cancels. A name that exists is an error on the status row and nothing
  changes; the field stays for another try.
- **A change of case only** (`readme` → `README`) or of Unicode form, on a filesystem that treats both
  as one name: the target "exists" because it is the source itself. Locally that is proven by the same
  device and inode (on a case-insensitive filesystem a link to the other spelling fails with "exists"
  and both spellings stat to one inode, *verified, APFS*). Remotely attributes prove nothing; the proof
  is the folder's listing: only when a fresh listing holds the source's spelling and no entry spelled
  like the new name is the "exists" taken to be the source itself; otherwise it is another entry and an
  error. sopen then renames in two steps through a free name in the same folder (`.<name>.sopen-renaming-<16 hex>`, §5.1), each step never
  replacing. **When the second step fails**, sopen renames the temp back to the original name, again
  never replacing; when that fails too, the report names the exact path the file now has (or, when
  a step got no answer it could confirm, the two paths it may have).
- **Remote folders**: renaming one needs SFTP's plain rename, which the library sopen uses cannot send
  today (§5.6); until slice S6 (02 §8) brings it, F2 on a remote folder says so and does nothing.

### 5.4 Make folder (F7)

- A one-line dialog asks the name; Enter creates it in the active panel's folder and puts the bar on
  it. A name that exists is an error. One level: a name with `/` is refused.

### 5.5 Delete (F8, Del, ⌘⌫; Shift+F8, Shift+Del)

- Always asks: `Delete <name>?` or `Delete N items (F files, D folders)?`, a folder's line adding
  `and everything inside it`.
- **Remote**: SFTP has no Trash, so the dialog says `permanent — no Trash on <host>`. Removal is
  bottom-up (`REMOVE` for files and links, `RMDIR` for folders once empty).
- **Local**: to the system Trash where there is one — macOS through `NSFileManager trashItemAtURL`
  (loaded at runtime, nothing linked, as the save dialog's AppKit is: 01 §6a); Linux through the
  freedesktop Trash (the home's `Trash` for the home's filesystem, `$topdir/.Trash-$uid` for others,
  each with its `.trashinfo`). Where neither works the dialog says `permanent — no Trash here`
  before anything is removed.
- **Shift+F8 / Shift+Del** delete locally without the Trash; the dialog says `permanent` in the
  error colour.
- A link is removed as a link, never its target.

### 5.6 Primitives

| Operation | Remote (SFTP over the window's ssh master) | Local |
|---|---|---|
| rename, move within a filesystem: a file | **`hardlink@openssh.com` to the new name, then `REMOVE` of the old** (both already used by sopen's writes): the link fails when the name exists, so nothing is replaced; a stop between the two, or an old name that cannot be removed, leaves both names on the same file: the item fails, "both copies exist", with the reason (the same outcome as a moved copy whose source stays). The old name is checked again right before its removal; another entry found there is left. On Linux with `fs.protected_hardlinks=1` the link fails with a permission error for a file the account does not own; the report gives that reason (and the file stays where it was). Never `posix-rename@openssh.com` (POSIX `rename()`, which replaces; the library's `Fs::rename` prefers it whenever the server offers it) | `renameat2(RENAME_NOREPLACE)` on Linux, `renamex_np(RENAME_EXCL)` on macOS; where the filesystem refuses those, `link` + `unlink` through the old name's folder (the old entry's descriptor held open meanwhile and compared with `fstat` before the unlink: its inode cannot be reused; an entry found changed when that descriptor is taken — another device, inode or ctime — is not linked at all: the rename fails, nothing changed; where no descriptor can be had, the name is looked at once more before the link, and before the unlink it must be the entry the new name is — device, inode, kept alive by the new hard link, and ctime; for a link its device, inode and ctime as first read); with neither (exFAT), no rename: a move copies, checks and removes |
| rename, move within a filesystem: a folder | **plain `SSH_FXP_RENAME`**, which draft-ietf-secsh-filexfer-02 §6.5 makes an error when the new name exists and OpenSSH's server refuses so (*verified, OpenSSH's `sftp-server`, 2026-11-20*: a plain rename onto an existing file failed and left both; the posix-rename extension replaced it). The library keeps the plain request private, so slice S6 sends it on a second, low-level channel (`openssh-sftp-client-lowlevel`'s rename request; its experiment E2 decides between that and an auxiliary channel without the extension). Until then: F2 on a remote folder is refused with the reason, and F6 of a remote folder moves it by copy, check and remove (§5.2) | as for a file |
| copy within one host | `copy-data` extension, in 8 MiB chunks, into a temp `.<name>.sopen-<random>` created `CREAT\|EXCL` in the target folder, then mode and times set through the handle, then published by `hardlink@openssh.com` (fails if the name exists) and the temp removed. Without `copy-data`: read and write through this machine, same publish. **Without `hardlink@`** (not a recent OpenSSH server): copy and move are refused until S6, whose plain rename then publishes the temp instead — never a write straight into the final name | temp file created exclusively (`O_CREAT\|O_EXCL\|O_NOFOLLOW`) and filled, given its mode and times and flushed through that descriptor — `copy_file_range` where available (Linux), else read and write; never `clonefile` (a clone is made by name, and another process of the account can put an unrelated file at that name before it is opened) — published by the exclusive rename, else `link` (fails if the name exists); on a volume with neither (exFAT), written in place (§5.1) |
| copy between hosts, or between this machine and a host | streamed through this machine (fetch side as 01 §5's fetch, write side as above) | the same, one side local |
| make folder | `MKDIR` (fails if it exists) | `mkdir` |
| delete | `REMOVE`, `RMDIR`, bottom-up | Trash (§5.5), else `unlink` / `rmdir` bottom-up |

- Nothing is run on the remote: no `cp`, `mv` or `rm` over ssh (AGENTS.md: file operations are SFTP).
- These are new traits in new files (`remote::ops`, 02 §5): `RemoteFs` and its write algorithm stay
  as they are.

## 6. Function keys without fn

### 6.1 Everywhere: the clickable row and Cmd/Ctrl+digits

- **Every** sopen window's F-key row is clickable: a click on a slot does what its key does there; a
  slot with an empty label does nothing (today `chrome::fkey_row` only paints).
- **Cmd/Ctrl+1…9, 0 act as F1…F10** in every window — the digit printed on the row. They follow the
  F-key's rules (§3: not while a text field has the typing). Ctrl+digits are free on the measured KDE
  Plasma; Meta+digits are not (keys.md §1).
- Those two always work, whatever the keyboard sends for its top row. Both are turned into that
  F-key's key event **once**, in the shell's raw-input hook (`gui/mod.rs`), before any window's pass,
  so every window and the browser's key table see a plain F-key (the same exact-modifier rule then
  applies). F1's key help is part of the same work unit (S1-A, 02 §8).

### 6.2 macOS: the top row sends F-keys while sopen is in front

- **Mechanism** (keys.md §4.1): the HID system parameter `HIDFKeyMode` — 0: the top row sends media
  keys without fn (the Mac's default, measured), 1: it sends F1…F12 without fn. sopen opens
  `IOService:/IOResources/IOHIDSystem` with `IOServiceOpen(…, kIOHIDParamConnectType)` and calls
  `IOHIDSetCFTypeParameter(connect, "HIDFKeyMode", 1)`; it reads the value from the registry property
  `HIDParameters["HIDFKeyMode"]`. IOKit is loaded at runtime; no entitlement, Accessibility grant or
  root is needed on that path per Apple's IOHIDFamily source (keys.md §4.1 item 2).
- **Measured** (E1, 02 §8.2; a Mac, macOS 26.7.1, arm64): run unprivileged, `IOServiceOpen` and
  `IOHIDSetCFTypeParameter` return 0, and no entitlement is held. The value lands in IOHIDSystem's
  `HIDParameters` and in every event service's `HIDEventServiceProperties` (a count that drifts as
  services come and go: never hard-coded). IOKit forwards it (`IOHIDSetHIDParameterToEventSystem`)
  to the HID event system in WindowServer, where the built-in keyboard's `IOHIDKeyboardFilter`
  applies it (`_fnKeyMode`): setter → registry → keyboard filter.
- **Readback**: sopen's own reads use the registry's `HIDParameters["HIDFKeyMode"]`. A receipt
  reads the consumer too: the keyboard filters' `FnKeyMode`, per keyboard service, in
  `hidutil dump services` (`hidutil dump services -f text | awk '/IOClass = /{c=$3} /FnKeyMode = /{print c, $0}'`).
  `hidutil property --get HIDFKeyMode`, `IOHIDEventSystemClientCopyProperty` and
  `com.apple.keyboard.fnState` do not follow this switch (they stayed unset at 0 and at 1): never
  use them as readbacks.
- **When**: a sopen process **acquires** the switch when one of its windows gains focus, and
  **releases** it when none of its windows has focus any more, when it quits, before the Mac sleeps
  and when the session is switched away; it acquires again on wake or session return if one of its
  windows still has focus. Acquire and release are §6.3's protocol, so several windows and several
  sopen processes hand focus to each other in any order of events.
- **It is global while sopen is in front**: every app sees F-keys then, and brightness and volume
  need fn. Keyboards whose Fn layer is in their own firmware are not affected at all.
- **Default on.** Config key `fkeys` in sopen's config file (01 §4a): `"function"` (default) or
  `"system"` (sopen never touches the setting). `sopen config fkeys function|system` sets it as
  `sopen config target` sets the target.
- **Never in a scripted run or a test**: `SOPEN_SCREENSHOT`, `SOPEN_AUTOSAVE`, `SOPEN_AUTOEXPORT`,
  `SOPEN_LIST` runs and `cargo test` never switch it (tests use a fake behind a trait).
- **Half proven on macOS 26.7.1**: E1 (02 §8.2) proved the setter-to-filter half above; Fluor's
  users report mixed results on recent macOS (keys.md §5 items 4–5). The key half — a plain F5
  reaching sopen — is the owner's receipt (§8, K-F3), and it decides: if F5 without fn does not
  reach sopen there, the default becomes `"system"` and this section records the result.

### 6.3 Restoring it, even when sopen dies

- **Two files** in sopen's state folder: `fkeys.lock`, a lock file that is created once and never
  replaced or removed, and `fkeys.json` (0600, replaced atomically), holding the value to restore and
  the processes holding the switch (pid and start time, so a reused pid is not taken for a live
  holder). Every step below runs **holding an exclusive lock on `fkeys.lock`**, the read and the
  change of the HID value included.
- **Acquire**: drop holders that no longer run. Then:
  - **holders remain**: the value 1 is sopen's own — add this process and stop;
  - **no holder remains but a state file exists** (its last holder died and its watchdog has not run
    yet): the state is sopen's, not the owner's — keep the recorded original value, make this process
    the only holder, then **set the HID value to 1 and read it back**. The dead holder may have died
    after writing the state but before setting the value (it is then still 0) or after (it is 1);
    either way the takeover ends with 1. The late watchdog later finds a live holder and restores
    nothing;
  - **no state**: read the HID value — **1** is the owner's own choice: record nothing and touch
    nothing; **0**: write the state (original 0, this process), then set 1 and read it back.
  - **A write that fails or does not read back 1 is not an acquisition**: this process is removed from
    the holders again; with none left, the recorded original is set back and the state removed; the
    status row says the F-key switch did not take effect, and §6.1's fallback stays.
- **Release**: remove this process from the holders; if none remain, set the recorded original value,
  **read it back**, and remove `fkeys.json` only when the read equals the original (the lock file
  stays). A release that fails — the lock, the read, the write, or a read-back that differs — keeps
  the state and is tried again (below).
- **A state is trusted only as sopen writes it**: sopen records an original of 0 and nothing else, so
  a `fkeys.json` whose original is not 0 is read as original 0 with no holders — restoring ends at 0
  and the file is removed; a damaged file can never make sopen write 1.
- **Watchdog**: an acquire first needs a ready watchdog: a small child of sopen's own binary (a hidden
  subcommand, as `--raise-window` is) holding the read end of a pipe whose write end only sopen holds.
  The child reads the switch once, starts its reader, and prints one ready line; sopen waits for it at
  most 2 s. A child that ends or stays silent is killed and reaped, and there is **no acquisition**
  (the status row says why). Before each later acquire sopen checks the child still runs and starts a
  new one, with the same wait, if it does not. When the pipe reaches end of file — sopen exited,
  crashed or was killed — the child releases for that process (above). It keeps trying until the
  original reads back, a live holder has taken over, or no state is left — 50 ms, doubling to 2 s,
  for at most 60 s in total — and then exits, leaving any state for the next start. It does nothing
  else and opens no window. A release that fails inside a running sopen is retried the same way.
- **At start**: sopen runs the holder clean-up under the lock; with no holder left and a state file
  present, the value is restored. **Except across a reboot**: the state records the boot
  (`kern.bootsessionuuid` on macOS, `/proc/sys/kernel/random/boot_id` on Linux), and a state left
  by another boot is removed without touching the value. The switch writes no preference (E1, 02
  §8.2), and login applies the owner's own setting again *(estimate: a reboot test has not been
  run)*; restoring a pre-reboot original would then overwrite a setting the owner made since. If a
  stuck 1 did survive a reboot, the next acquire reads it as the owner's own and nothing restores
  it — the risk taken. Without a readable boot id, the plain rule applies.
- **Known gap**: overlays such as Mission Control can leave sopen "active", so F-key mode stays on
  over them (keys.md §4.1 item 3).

### 6.4 Linux and Windows

- **Linux: no per-app mechanism** for an unprivileged app — every Fn-lock control found is root-only,
  hardware-specific and global, and a Wayland client cannot remap keys (keys.md §4.2). sopen changes
  nothing there; §6.1 is the answer. Keyboards that send F-codes reach sopen as F-keys today; the
  README says that a laptop keyboard's own Fn lock (often Fn+Esc) is the way to get them there.
- **Windows: no OS API**, only vendor tools (keys.md §4.3); Windows is out of sopen's scope (01 §9).

## 7. Decisions

| Decision | Why |
|---|---|
| F7 MkDir and F8 Delete are in | message 15 asks for "the most common" keys; all six surveyed commanders agree (keys.md §2.1) |
| F9 = the mode switch, its label naming the target mode | sopen has neither Menu nor Terminal, F9's conventions; ssot item 3 asks for one key shown in the row |
| Delete goes to the Trash locally, permanently on a remote, and says which | SFTP has no Trash; the dialog must never leave the owner guessing |
| Copy into a staged temp, then publish without replacing | 01 §6a's "Save to…" rule; a copy is never seen half-made under its name |
| The target field takes `host:path` | sopen's own command-line form (01 A1); it makes copying between hosts and to this machine possible from one window |
| The macOS switch is on by default, behind a crash-safe restore and the owner's receipt | the owner asked for F-keys without fn; a global setting must come back even if sopen dies |

## 8. Acceptance

Run locally and on a remote folder under `/tmp/sopen-receipts/`; window receipts as AGENTS.md says.

- K-R1. The list's F-key row reads `1Help 2Rename 3View 4Edit 5Copy 6Move 7MkDir 8Delete 9Dual 10Tree`
  in preview mode and `9Prevw` in dual mode (screenshot); in the hex window it reads `1Help 3Exit
  4Edit 5Save 6Goto 8Asc/hx`.
- K-R2. Each slot clicked, and each Cmd/Ctrl+digit, does what its key does (test: every slot of every
  context); with a rename field open, F5, Cmd/Ctrl+5 and `+` do nothing.
- K-R3. Shift+F6 renames and F6 moves; Shift+F8 deletes permanently and F8 asks for the Trash; Alt+F10
  opens the tree; Shift+F5, Alt+F2 and Shift+F10 do nothing; `+` typed with Shift on a US layout and
  without it on a German one both open the select-group dialog (test: exact modifiers, typed text).
- K-S1. Space ×3 selects three entries (yellow, foot `3 selected = …`); `+ *.rs` selects by pattern;
  `*` inverts; Cmd/Ctrl+A selects all; Cmd/Ctrl+R keeps the selection.
- K-C1. F5 on a file → the dialog with the other panel's folder → Enter → the copy is there with the
  same bytes, mode and mtime; the source is unchanged.
- K-C2. F5 where the name exists → Skip leaves both as they were; Rename makes `<stem>-1<ext>`; Cancel
  stops; at no time is the existing file's content or inode changed (test: inode and SHA-256 before
  and after).
- K-C3. F5 of a folder holding files, a subfolder, a link and a fifo → the copy has the files, the
  subfolder and the link (as a link); the report lists the fifo as skipped.
  Through a local `sftp-server` too: the copied link's target (`readlink`) equals the source link's,
  and the link sits at the new name (the request carries target and link in OpenSSH's order).
- K-C4. F5 of a 200 MiB file, Esc at 40 % → no file under the target name and no temp left; the status
  says stopped; a same-host remote copy shows progress in 8 MiB steps and stops between them. A server
  without `hardlink@openssh.com` (a stand-in that hides it) → copy refused with the reason.
- K-C5. Copy between this machine and a remote, and between two remotes, by editing the target to
  `host:/tmp/sopen-receipts/x` and to a local path → the bytes arrive (SHA-256 equal).
- K-M1. F6 within one filesystem, by object and slice (request log of a local `sftp-server`; never a
  `posix-rename@openssh.com` request):
  - a local file or folder → renamed in place (the inode is kept);
  - a remote regular file (S1) → `hardlink@openssh.com` to the new name, then `REMOVE`; a file the
    account does not own under `fs.protected_hardlinks=1` → the permission reason, the file unmoved;
  - a remote folder before S6 → copied, checked, then removed (the file-unit rules of §5.1: a stop
    after one committed file removes no source); from S6 → one plain `SSH_FXP_RENAME`.
- K-M2. F6 to another filesystem or host → copy, check, remove; a source made unremovable → both
  copies exist and the report says so. F6 of a folder of three files, stopped after the first file was
  published → the target folder holds that one file, no temp, and **no source file was removed**; the
  same with a failure on the second file. A source rewritten between its copy and its removal → kept,
  both listed.
- K-N1. F2, type a name that exists, Enter → an error, nothing renamed; a free name → renamed, the bar
  on it; `readme` → `README` on a case-insensitive filesystem (macOS default) → renamed; the same with
  the second step made to fail (a fault-injecting file layer) → the file is back under `readme`, or the
  report names the temp name it has; remotely, a case-only rename where the listing also holds a
  `README` entry → an error; a remote
  rename through `hardlink@` + `REMOVE`, never `posix-rename@openssh.com` (request log of a local
  `sftp-server`); F2 on a remote folder before S6 → the reason, nothing changed.
- K-E1. Edge cases, each locally and through a local `sftp-server`: copying a folder into itself
  through a link or an alias to it → refused; two names equal only after Unicode normalisation on a
  normalising filesystem → the second is "exists" (decided by the exclusive create); a read-only
  source folder → copied, its bits restored on the copy after its contents; a read-only target folder
  → that item fails with the reason; a server refusing to set times → copied, a note in the report.
- K-K1. F7 `new` → the folder exists, the bar on it; F7 `new` again → an error.
- K-D1. F8 on a remote folder → the dialog says `permanent — no Trash on <host>`; Enter removes it
  and everything inside it. F8 on a local file → it is in the system Trash (macOS: in `~/.Trash`;
  Linux: in the Trash's `files` with a `.trashinfo`).
- K-F1. A test with a fake HID switch: focus gained → the fake reads 0 and is set to 1; focus lost →
  0; two sopen processes, focus moving A → B with B's acquire before or after A's release → the
  original 0 is recorded once and restored when the last lets go; the owner's own 1 is never changed
  and never recorded; sleep and wake release and reacquire.
- K-F6. Stale state, at both crash points, each with the dead holder's watchdog held back by the test:
  (a) the holder dies after writing the state but before setting the value (state with original 0,
  value still 0); (b) it dies after setting the value (value 1). In both, a new process acquires → it
  keeps the recorded original 0, sets 1 and reads 1 back; the delayed watchdog then runs → it restores
  nothing while the new holder lives; the new holder releases → 0. With the fake set to refuse the
  write (or to read back 0) → the new process is not a holder, the state is gone, the value is 0, and
  the status row says the switch did not take effect.
- K-F5. Contention: two processes acquire and release 1 000 times each, at random, against the fake →
  the lock file is never replaced or removed, the fake ends at its original value, and no run sees a
  state file with a holder that does not run.
- K-F2. The watchdog: a process holding the switch is killed with `SIGKILL` → within 1 s the fake is
  back at 0 and the state file is gone (test with the fake behind the same trait).
- K-F3. **Owner-run receipt** (macOS 26.7.1, the built-in keyboard, the default setting): open a
  folder in sopen, press F5 without fn → the Copy dialog opens; switch to another app → the top row's
  F5 is dictation again; `kill -9` sopen while it is in front → `ioreg -c IOHIDSystem -r -d 1 | grep
  HIDFKeyMode` shows 0 again within a second. E1 proved the setter-to-filter half on 26.7.1
  (02 §8.2); K-F3 proves the key half: a plain F5 opens the Copy dialog. Until this receipt
  exists, that half of §6.2 is unproven.
- K-F4. `sopen config fkeys system` → the config file holds `fkeys = "system"` and sopen never opens
  IOHIDSystem (test: the fake is never called).
