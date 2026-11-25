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
- On KDE Plasma's Wayland session, while an xtools GUI window has the keyboard, the top row sends
  F1…F12 and every other KDE shortcut is handed back to KDE (§6.4).
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
  image window's 5 Save — 1 is Help everywhere, and 2, 7, 9, 10 are empty. With VM-7 (08 §8): 9 View
  in every view, 8 Encode in Text and Raw. The git panel's row is 03 §9.1's; the tree's is
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
   acts (01 §3a's rule, extended to every key of this table) — but F9 in the browser's viewer, which
   types nothing: it closes the field, as Esc would, and arms the view keys (08 D29, VM-7). A preview
   never takes keys (02 §4.4).
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
  - `.sopen-probe-<16 hex>` — sopen's publish probe (an empty file; on a remote without
    `hardlink@`, a folder holding the rename probe's two small files, §5.6); removing it is safe. sopen
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
- **Move** is a rename where one is possible (same host and filesystem, §5.6) — a remote folder on
  the same host through the plain SFTP rename (a session without that channel moves it by copy, check
  and remove, with the rules below). Otherwise it is a copy, then a check, then the removal of the
  source.
  **The check reads the published copy back from the target**, in full, on every path and at any
  size, and compares its SHA-256 with that of the bytes read from the source (where the host copied
  them itself — `copy-data`, `copy_file_range` — the source is read again); a remote target so costs
  a move a second transfer, accepted for data safety: a **remote folder** moved by copy (across
  hosts, or with no rename channel) downloads the source as well as the copy, about twice its size, where
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
- **Remote folders** are renamed by one SFTP plain rename (§5.6, the folder row), on a second `sftp`
  channel of the operation's own connection, opened the first time a folder rename needs it: a name
  that exists is an error on the status row with both left, as for a file. Where that channel cannot
  be opened (the server refuses another session), F2 says why and nothing changes.

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
  before anything is removed. The Linux Trash is written only through its own folders, each opened
  from the one above with `O_NOFOLLOW` and checked as this account's, made private (0700) if group
  or others could enter it; the `.trashinfo` is created and the item moved relative to those held
  folders, never replacing (`renameat2` with `RENAME_NOREPLACE`; where the filesystem has no
  exclusive rename, the freedesktop spec's own way — the exclusive `.trashinfo` reserves the name,
  the name is checked free in the held `files` folder, then a plain `renameat`, which moves whatever
  is at the source and deletes nothing). A top folder that its group or others can write without the sticky bit has no Trash
  (another account could swap `.Trash-$uid`): the dialog says `permanent` and asks. A name longer
  than 200 bytes gets a shorter name inside the Trash; `Path=` keeps the whole name.
- **Shift+F8 / Shift+Del** delete locally without the Trash; the dialog says `permanent` in the
  error colour.
- A link is removed as a link, never its target. Esc stops before the next request; a request
  already sent completes.
- **Locally the walk never leaves the selection**: it goes through open folder descriptors, as
  `rm` and `fts` do — each folder opened from its parent with `O_DIRECTORY|O_NOFOLLOW` and only
  while its device and inode are those just looked at, each entry looked at and removed relative
  to it (`fstatat`, `unlinkat`). A folder swapped for a link, or for another folder, mid-delete is
  left, named, with whatever is in it. **The accepted local residual:** a folder is removed by name
  (`unlinkat(…, AT_REMOVEDIR)`; POSIX has no removal by descriptor, and `rm` has the same window), so
  a folder swapped in between its last check and that call is removed instead — and only if it is
  empty, since `rmdir` never removes a folder with anything in it. The most it can cost is an empty
  folder.
- **Remotely, the accepted residual.** SFTP has no request relative to a folder: every `REMOVE`,
  `RMDIR` and listing names a path the server resolves again when it handles it. sopen takes the
  selected folder's canonical path (`REALPATH`) before it looks at it, and right before each
  request checks the entry with `LSTAT` and the folder the request goes through with `REALPATH`,
  which must still resolve where the walk listed it; otherwise the entry is left and named. Two
  things remain. (1) *One round trip*: between the reply to that check and the server's handling of
  the request, another process can swap a folder on the path for a link to a folder elsewhere, and
  the request then acts on the entry of the same name in the link's target. The window is inside
  the server, and the client cannot close it. (2) *The check is the server's `REALPATH`*: it works
  where `REALPATH` resolves links, as OpenSSH's does (`realpath(3)`; tested against OpenSSH's
  `sftp-server` only); against a server whose `REALPATH` does not, the check passes without seeing
  a swap. A folder swapped for another real folder under the same name resolves the same and is not
  detected; its entries are removed, but they lie at the selected path.

### 5.6 Primitives

| Operation | Remote (SFTP over the window's ssh master) | Local |
|---|---|---|
| rename, move within a filesystem: a file | **`hardlink@openssh.com` to the new name, then `REMOVE` of the old** (both already used by sopen's writes): the link fails when the name exists, so nothing is replaced; a stop between the two, or an old name that cannot be removed, leaves both names on the same file: the item fails, "both copies exist", with the reason (the same outcome as a moved copy whose source stays). The old name is checked again right before its removal; another entry found there is left. On Linux with `fs.protected_hardlinks=1` the link fails with a permission error for a file the account does not own; the report gives that reason (and the file stays where it was). Never `posix-rename@openssh.com` (POSIX `rename()`, which replaces; the library's `Fs::rename` prefers it whenever the server offers it) | `renameat2(RENAME_NOREPLACE)` on Linux, `renamex_np(RENAME_EXCL)` on macOS; where the filesystem refuses those, `link` + `unlink` through the old name's folder (the old entry's descriptor held open meanwhile and compared with `fstat` before the unlink: its inode cannot be reused; an entry found changed when that descriptor is taken — another device, inode or ctime — is not linked at all: the rename fails, nothing changed; where no descriptor can be had, the name is looked at once more before the link, and before the unlink it must be the entry the new name is — device, inode, kept alive by the new hard link, and ctime; for a link its device, inode and ctime as first read); with neither (exFAT), no rename: a move copies, checks and removes |
| rename, move within a filesystem: a folder | **plain `SSH_FXP_RENAME`**, which draft-ietf-secsh-filexfer-02 §6.5 makes an error when the new name exists and OpenSSH's server refuses so (*verified, OpenSSH's `sftp-server`, 2026-11-20*: a plain rename onto an existing file failed and left both; the posix-rename extension replaced it; E2, 2026-11-23, refused a folder onto a non-empty folder, an empty folder, a file, a dangling link and a link to a folder, both left: 02 §8.2). The library keeps the plain request private, so sopen sends it on a second `sftp` channel of the same master, through `openssh-sftp-client-lowlevel`'s `WriteEnd::send_rename_request` (E2's route B); the channel is opened the first time the operation's connection needs it and kept for that connection, and an open that failed is kept with its reason — F2 says it, F6 moves by copy, check and remove instead (§5.2). A failure is a name taken only when `lstat` then finds the new name (status 4 is the generic failure). A non-empty folder, a file or a link (dangling or to a folder) already at the new name is refused, both left (measured, E2). **The accepted residual:** OpenSSH renames a folder as `stat(newpath)`, then `rename()` (`sftp-server.c:1308-1309`; *from the source, not measured*), a check and then the act, and between the two only an **empty folder** made at the new name can be replaced: `rename()` never lets a folder replace a non-empty folder (`ENOTEMPTY`) or a file or link (`ENOTDIR`), so no file data can be lost. It is accepted because the window is inside the server, where sopen cannot close it from the client, and the most it can cost is an empty folder made in that instant. **A link is never sent through the plain rename** — E2 saw it replace a dangling link — so a link, one to a folder included (`lstat` shows a link), is made again at the new name, as for a file. F2 and F6 of a remote folder within one host are each one plain rename (F6 to another filesystem or host copies, checks and removes, §5.2) | as for a file |
| copy within one host | `copy-data` extension, in 8 MiB chunks, into a temp `.<name>.sopen-<random>` created `CREAT\|EXCL` in the target folder, then mode and times set through the handle, then published by `hardlink@openssh.com` (fails if the name exists) and the temp removed. Without `copy-data`: read and write through this machine, same publish. **Without `hardlink@`** (not a recent OpenSSH server): the temp is published by the plain rename (the folder row's route B, on the same second channel) instead — never a write straight into the final name; where that channel cannot be opened, the copy is refused with the reason before anything is staged. That it refuses an existing name is measured on OpenSSH only (E2, `sftp-server -P hardlink`: the plain rename of a regular file is `link()` + `unlink()` there); another server's is its own (draft-ietf-secsh-filexfer-02 §6.5 makes it an error), and sopen cannot tell which server it talks to (SFTP's version reply lists extensions, not the server). **So the server's rename is probed first, and a server that replaces is refused — never written over.** Once per SFTP session (the operation's own connection, §5.1), before its first such publish — before that copy's temp is staged, so a refusal copies nothing — sopen makes its own folder `.sopen-probe-<16 hex>` (`MKDIR`, failing if it exists) in the target folder, creates two files in it exclusively (`CREAT\|EXCL`), each with bytes of its own, plain-renames the first onto the second, and reads both back. **Refused** = the reply is status 4 (`SSH_FX_FAILURE`) and both files are there with their own bytes: the session's publishes then use the plain rename, whose refusal finds a name in the way, which asks as with `hardlink@` (§5.2). **Replaced** = the reply is OK, or after any reply either file changed or is gone: that publish, and every later one of the session, is refused with "this server's plain rename replaces an existing name, so sopen cannot publish a copy here without the risk of writing over one", and nothing is written. *Cleanup:* the two files (`REMOVE`) and the folder (`RMDIR`), each only while it is still the one sopen made (type and size as made); one that cannot be removed is left, named in the report — removing it is safe (§5.1). *Failure modes:* the folder or a file cannot be made, a request is refused for another reason, times out or has an unknown outcome, or a read-back fails — the probe could not run: that publish is refused with that reason, nothing is written, nothing is recorded (the session's next such publish probes again). The probe proves that the server refuses a name already there, not that its rename is atomic: a server that checks, then renames — OpenSSH does so for a regular file on a filesystem without hard links (`sftp-server.c:1290-1292`; *from the source, not measured*) — can still replace an entry made between its check and its rename | temp file created exclusively (`O_CREAT\|O_EXCL\|O_NOFOLLOW`) and filled, given its mode and times and flushed through that descriptor — `copy_file_range` where available (Linux), else read and write; never `clonefile` (a clone is made by name, and another process of the account can put an unrelated file at that name before it is opened) — published by the exclusive rename, else `link` (fails if the name exists); on a volume with neither (exFAT), written in place (§5.1) |
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
- **When**: a sopen process **acquires** the switch when one of its windows is in front — focused,
  and neither minimised nor fully covered — and **releases** it when none of its windows has been in
  front for 250 ms (focus moving from one of its windows to another can pass through a moment with
  neither), at once when its window closes, when it quits, before the Mac sleeps
  (`NSWorkspaceWillSleepNotification`) and when the session is switched away
  (`NSWorkspaceSessionDidResignActiveNotification`); it acquires again on wake
  (`NSWorkspaceDidWakeNotification`) or session return (`NSWorkspaceSessionDidBecomeActiveNotification`)
  if one of its windows is in front by its windows' latest word (never by the focus it held before it
  let go), and not while the Mac sleeps or the session is away, whatever its windows say. The four
  are observed on `NSWorkspace.sharedWorkspace.notificationCenter`, once per process, registered once
  the switch is built (not with `"system"`, not in a scripted run); eframe and winit pass on none of
  them. Acquire and release are §6.3's protocol, so several windows and several sopen processes hand
  focus to each other in any order of events; they run on a thread of their own — which also reads
  the config and builds the switch — never on a window's or AppKit's main thread (the lock can be held
  by a stopped sopen). The windows and the notifications only send to it, except the will-sleep
  notification: the Mac may suspend once its observers return, so it waits for the release, at most
  1 s, then returns either way. A covered window (a locked screen, a screen
  saver) is not in front where macOS reports it covered *(estimate: not measured)*. Should a wake
  come without its notification, it is seen afterwards: the wall clock has run ahead of the monotonic
  one, which stops while the Mac sleeps; the switch is then let go and acquired again, read back anew,
  if a window is still in front.
- **It is global while sopen is in front**: every app sees F-keys then, and brightness and volume
  need fn. Keyboards whose Fn layer is in their own firmware are not affected at all.
- **Default on.** Config key `fkeys` in sopen's config file (01 §4a): `"function"` (default) or
  `"system"` (sopen never touches the setting). `sopen config fkeys function|system` sets it as
  `sopen config target` sets the target.
- **Never in a scripted run or a test**: `SOPEN_SCREENSHOT`, `SOPEN_AUTOSAVE`, `SOPEN_AUTOEXPORT`,
  `SOPEN_LIST` and `SOPEN_KEYS` runs (any non-empty value) and `cargo test` never switch it (tests
  use a fake behind a trait).
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

On Linux no keyboard setting can be switched for one app. Every Fn-lock control found is root-only,
hardware-specific and global (keys.md §4.2). KDE Plasma's Wayland compositor (KWin) has a per-window
opt-out instead: while the focused surface holds a keyboard-shortcuts inhibitor
(`zwp_keyboard_shortcuts_inhibitor_v1`), KWin skips its global shortcuts, and a media key reaches
the window as a key (K-L1, `research/fkeys-linux.md`). sopen uses it, changes no setting, and hands
every other shortcut back to KDE, so inside an xtools GUI window only the top row changes.

Evidence: *measured* = K-L1's private headless KWin 6.7.5 with injected keys (study E1–E5); [추정] =
not checked; [가설] = an item of §8 settles it.

#### 6.4.1 Where it applies

sopen asks for an inhibitor only when every check below passes. The checks run in this order, and
the first Wayland request is made only after the last one:

1. **Before the window.** `run_window_sized` (`src/gui/mod.rs:1252`) checks three things before it
   opens the window:
   - the run is not a scripted run or a test build (`src/fnkeys/mod.rs:99-105`, `:518-520`);
   - `SOPEN_FKEYS_TABLE`, if set, parses — a malformed value is an error;
   - the top-row table (§6.4.3) has a row.
2. **In the app-creation closure.** The window is a Wayland window: eframe's creation context gives
   a Wayland display and window handle (`eframe-0.37.0/src/epi.rs:101-117`). An X11 or XWayland
   window gets nothing. The closure then starts the hand-back thread and the inhibitor thread, and
   waits for neither.
3. **The config.** `fkeys` is `"function"`; an absent key counts as `"function"`. The hand-back
   thread reads it first, as the macOS feed does (`config::load(&config::path()?)`,
   `src/gui/mod.rs:1756`). `"system"`, or a config that cannot be read, ends the thread, and nothing
   else starts (the rule of `src/gui/mod.rs:1487-1493`). `"system"` therefore never binds anything
   and never calls the bus. `sopen config fkeys function|system` sets the key, as on macOS (§6.2).
4. **KWin's own kglobalaccel.** The session bus's `org.kde.kglobalaccel` and `org.kde.KWin` must
   have one owner. kglobalaccel lives inside KWin (study §2), so this is the kglobalaccel whose
   shortcuts KWin skips [추정 that the two names share one owner; K-L6]. The first snapshot (§6.4.4)
   is read. The thread then says *ready*.
5. **The manager.** Only after *ready* does the inhibitor thread bind
   `zwp_keyboard_shortcuts_inhibit_manager_v1` and the seat. KWin offers the manager to every client
   and grants an inhibitor without a prompt (study §2; *measured*).

When any check fails, nothing happens. §6.1's clickable row and Cmd/Ctrl+digits work as they do
everywhere.

#### 6.4.2 The inhibitor

- **Bound on a guest queue of winit's own `wl_display`** (`Backend::from_foreign_display`,
  `wayland-backend-0.3.17/src/sys/mod.rs:102`). This is the same Wayland client as winit, which is
  what lets it name the window's `wl_surface`. winit has no support for the protocol.
- **Held only while the window has the keyboard and its snapshot is fresh.**
  - At focus-out it is destroyed.
  - At focus-in the snapshot is read again, and the inhibitor is requested once the read is done.
  - While a refresh is pending there is no inhibitor, and KDE has every key.
  - KWin applies an inhibitor only to the focused surface anyway (study §2), and creating one at
    each focus-in was *measured* to behave the same as holding one (E1 P3).
- **One owner and one guard.**
  - The inhibitor thread owns the guest queue. It waits in `poll` on the display's fd and a stop
    pipe (`EventQueue::prepare_read`, `wayland-client-0.31.15/src/event_queue.rs:456`), never in
    `blocking_dispatch`.
  - The inhibitor lives in a guard that sends `destroy` and flushes on every exit path of that
    thread, unwinding included.
  - The window holds a handle to the same inhibitor. It sends `destroy` when the owning thread
    disappears (its channel closes).
  - **One rule for creating it.** The inhibitor thread sends `inhibit_shortcuts` only when all
    hold: the window has the keyboard; this focus-in's snapshot is read; nothing has failed since
    this focus-in; no window call is past 500 ms; and it holds none. Every other path only
    destroys. A second inhibitor for the surface is `already_inhibited`
    (`wayland-protocols-0.32.13/…/keyboard-shortcuts-inhibit-unstable-v1.xml:61,71-73`), and any
    protocol error ends the whole connection, winit's window included (`wayland.xml:73`).
  - The guest backend never outlives winit's display (`wayland-backend-0.3.17/src/sys/mod.rs:98-101`);
    it holds because the one event loop, and its display, live for the whole process.
- **The window's end.** In `App::on_exit` the window sends `destroy` unconditionally, then stops and
  joins the thread. eframe calls `on_exit` while the window still exists
  (`eframe-0.37.0/src/native/glow_integration.rs:508-520`, `wgpu_integration.rs:593-607`).
  The inhibitor goes before the surface. KWin keys its table by the raw surface and removes an
  entry only when the inhibitor goes (study §3.2).
- **When sopen dies,** KWin destroys the client's inhibitor (study §3.2). After `kill -9` the next
  press fired the shortcut again (*measured*, with the focus already elsewhere). Nothing global was
  changed, so there is nothing to restore.

#### 6.4.3 The top row

- **One key source: winit's own events, before eframe.** The GUI shell owns the winit handler:
  - It builds the app with `eframe::create_native` (`eframe-0.37.0/src/lib.rs:376`) and wraps the
    handler it returns.
  - One `EventLoop` per process, kept for every window and for the glow retry. winit refuses a
    second one (`winit-0.30.13/src/event_loop.rs:119`); each window uses `run_app_on_demand`
    (`platform/run_on_demand.rs:70`).
  - `create_native` returns no `Result`: a renderer's error stays inside eframe
    (`native/run.rs:96, 177-191, 541`), and `run_app_on_demand` returns `Ok(())`. The retry's
    trigger is the shell still in its slot when the loop returns — wgpu fails at `set_window`
    (`wgpu_integration.rs:272`), before the app creator (`:325-345`). Still in its slot after glow
    too → `cannot open a window: neither wgpu nor glow started`, exit 1. A run whose app was created
    ends as a close.
  - The shell builds its one `EventLoop<eframe::UserEvent>` itself, on every OS, and applies
    `event_loop_hook()` to that builder: `create_native` never runs `NativeOptions::event_loop_builder`
    (`run.rs:54-56`), and on macOS the default menu's Quit would skip the unsaved-changes prompt
    (`src/gui/mod.rs:1216-1224`).
  - The wrapper sees each `WindowEvent::KeyboardInput` and `ModifiersChanged` before eframe. A key
    event's code is its evdev code (`PhysicalKeyExtScancode::to_scancode`). E5 *measured* the
    brightness key as `scancode=Some(224)`, `logical=Named(BrightnessDown)`, which egui alone never
    sees (`egui-winit-0.37.0/src/lib.rs:1670`).
  - Modifiers come from winit's xkb state, Meta included. egui-winit drops Meta on Linux
    (`lib.rs:554-564`).
- **The table** maps an evdev code to a slot 1…12, in `src/fnkeys/top_row.rs`.
  - Its rows come from the owner's receipt K-L2 only, never from recall; a position that sends a
    chord is no row and stays KDE's.
  - **On Linux the switch ships OFF until K-L2's measured table exists.** Until the table has a
    row, check 1 fails.
  - One table serves every keyboard on the seat. A key carries no device [추정], so another
    keyboard's key that sends a table code is that F-key too.
  - `SOPEN_FKEYS_TABLE=<code>:<slot>[,…]` replaces the table for one run, for receipts only.
- **A table code's key event is rewritten into F*n*.**
  - Its `physical_key` and `logical_key` become F*n*'s and its `text` is dropped; winit's modifiers
    are kept. eframe then sees an F-key, and §3's rules apply: exact modifiers, and no plain F-key
    while a field has the typing.
  - Its repeats and its release are rewritten the same way.
  - The chord goes where that F-key chord goes (§6.4.4).
  - The table code's own media chord is never looked up.

#### 6.4.4 Every other key goes back to KDE

While the inhibitor holds, KWin runs none of its global shortcuts for the window (study §2), so
Alt+Tab, Meta, Meta+L, Print, volume, Ctrl+F1–F4, Alt+F4 and the rest arrive as keys (*measured*
for brightness, Meta, Alt+Tab and Alt+F4, E1).

- **The rule.**
  - A press whose chord a KDE global shortcut owns never reaches eframe: the wrapper drops it, with
    its repeats and its release.
  - Instead, the hand-back thread invokes the owner, `/component/<c>`
    `org.kde.kglobalaccel.Component.invokeShortcut("<action>")`. This is *measured* for Alt+Tab, also
    with Alt held: focus moved on Alt's release, as with the native shortcut (E3, E4).
  - Every other key does what it does today.
  - An F-key chord made from the top row is the F-key's chord. Ctrl + the key printed F1 is KDE's
    desktop 1 when KDE owns Ctrl+F1, sopen's when sopen binds it, and nothing otherwise.
- **The snapshot.**
  - It holds every active component's shortcuts with their keys: `org.kde.KGlobalAccel.allComponents`,
    then `allShortcutInfos` and `isActive` per component. These names are from memory [추정], and
    KL-W2 reads the installed interface first. Only `action(int)` and `invokeShortcut` were
    measured (E3).
  - It is read before every inhibitor (§6.4.2).
  - A chord is winit's logical key, or for a key with no logical key its code's key, plus winit's
    modifiers. It is turned into kglobalaccel's key code (Qt's key plus modifiers, the form E3 passed
    to `action(int)`) by one table in `src/fnkeys/handback.rs`.
- **Requests.** The window only sends to the hand-back thread, and the thread invokes **once per
  press**.
  - Each request carries the focus generation, and at focus-out the unsent requests of the old
    generation are dropped, never replayed.
  - Calls run on a zbus connection built with `method_timeout(250 ms)`
    (`zbus-5.19.0/src/blocking/connection/builder.rs:300`). KWin answers in-process well under that
    [추정], and a stuck KDE gets its shortcuts back before the next press.
  - A timeout or an error is a failure (§6.4.5). A request whose outcome is unknown is never retried.
- **Meta alone** (KWin's modifier-only shortcuts; E1's "Meta tap") is handed back at Meta's release
  when no other key and no pointer button came since its press, if the snapshot holds a
  modifier-only chord for it [가설: KWin's rule may have more conditions; K-L6].

#### 6.4.5 Failing toward KDE

- **The inhibitor goes at once when there is any doubt.** Each of these is a doubt:
  - a snapshot that cannot be read;
  - a call that times out or fails;
  - the hand-back thread or the inhibitor thread ending.

  The status row then says `the F-key switch did not take effect: <reason>` (`NOT_IN_EFFECT`,
  `src/fnkeys/mod.rs:62`). The reason goes through the feed's `Said` and `publish`
  (`src/gui/mod.rs:1404-1409`, `:1702-1714`), because `Feed::pass` rewrites the window's line on
  every pass (`:1541-1551`). No new inhibitor is made until the next focus-in. A protocol error is
  not survivable: the window ends, and KWin drops the inhibitor with the client (§6.4.2).
- **A stalled window.** The window's thread marks the start and the end of each event it handles. If
  one call lasts 500 ms, the inhibitor thread destroys the inhibitor until that call returns. A
  frame that blocks for seconds then costs the top row, never Alt+Tab or Meta+L.
- **What KWin keeps for itself, inhibitor or not:** the power key, Ctrl+Alt+F1…F12, the locked
  screen, screen edges and an open Alt+Tab switcher (study §2). KWin has no escape chord. A process
  stopped whole (`SIGSTOP`) keeps its inhibitor while focused; a click on another window ends it.

#### 6.4.6 What the hand-back does not reproduce

| Case | Inside an xtools window |
|---|---|
| A chord the snapshot does not list (kglobalaccel names it otherwise, or the Qt table lacks it) | acts inside xtools, as an unbound key would. K-L6 and K-L8 check the lock (Meta+L), Alt+Tab, the launcher (Meta) and Print |
| A held handed-back chord | acts once per press. KWin repeats a held shortcut; sopen does not |
| Meta alone | the rule of §6.4.4 [가설] |
| Meta+L (lock) | handed back to its owner [추정; the private session ran `--no-lockscreen`, K-L8]. If that fails, the screen does not lock and the status row says so. The idle lock is unaffected |
| Screen-reader and accessibility key grabs | off while the window has the keyboard. KWin skips them when inhibited, and there is no action to invoke (study §2) |
| Keys the kernel handles (some backlight and fan keys, a kernel Fn lock) | never become keys; unchanged |

#### 6.4.7 Other desktops, and the terminal

- **GNOME: not requested.** Mutter shows an Allow/Deny dialog for an inhibitor (study §4). Once it
  is allowed, GNOME has no kglobalaccel to hand its shortcuts back through.
- **sway: not requested.** sway honours the inhibitor (study §4), but it has no "which binding owns
  this key" query, so its workspace and window keys would be gone.
- **X11 and XWayland: nothing.** §6.1 is the answer.
- **xtools in a terminal (10-tui.md).** sopen owns no surface there, and KWin takes a bound media
  key before the terminal unless the terminal holds an inhibitor itself:
  - kitty's `grab_keyboard` holds one [추정], and kitty would also need its own map, since the kitty
    protocol has no brightness code;
  - herdr-app holds none (`research/tui-gui.md` §10).

  That route is the terminal's setting and is not specified. The top row is F-keys there only when
  the keyboard sends them (Fn held, or its Fn lock); a real F1 reaches the TUI as its escape
  sequence (`CSI P` under the kitty protocol) [추정]. §6.1's fallbacks are 10 §9's. A macOS TUI
  taking HIDFKeyMode on focus reports is [가설] and not specified.

#### Windows

- **Windows: no OS API**, only vendor tools (keys.md §4.3). Windows is out of sopen's scope (01 §9).

## 7. Decisions

| Decision | Why |
|---|---|
| F7 MkDir and F8 Delete are in | message 15 asks for "the most common" keys; all six surveyed commanders agree (keys.md §2.1) |
| F9 = the mode switch, its label naming the target mode | sopen has neither Menu nor Terminal, F9's conventions; ssot item 3 asks for one key shown in the row |
| Delete goes to the Trash locally, permanently on a remote, and says which | SFTP has no Trash; the dialog must never leave the owner guessing |
| Copy into a staged temp, then publish without replacing | 01 §6a's "Save to…" rule; a copy is never seen half-made under its name |
| The target field takes `host:path` | sopen's own command-line form (01 A1); it makes copying between hosts and to this machine possible from one window |
| The macOS switch is on by default, behind a crash-safe restore and the owner's receipt | the owner asked for F-keys without fn; a global setting must come back even if sopen dies |
| Linux, KDE Plasma Wayland: an inhibitor per xtools GUI window, held while the window has the keyboard and its snapshot is fresh (§6.4.2) | Message 19: F1 is F1 inside xtools, the OS untouched. It is KWin's per-window opt-out, and the measured per-focus mode lets each focus-in refresh first |
| Route B: the GUI shell owns winit's handler (`create_native`) and decides each key before eframe (§6.4.3) | One key source, with xkb modifiers, Meta included. Route A paired egui's input with a second `wl_keyboard`, and that was unsound: egui sets `repeat` itself in `begin_pass` (`egui-winit lib.rs:1159`, `input_state/mod.rs:405-413`), and egui-winit drops Meta |
| Only the top row changes. Top-row F-key chords go where F-key chords go, and KDE-owned chords are handed back and never reach eframe (§6.4.3–6.4.4) | Every other key keeps today's meaning, where KWin took it before sopen |
| Hand-back: a snapshot read before each inhibitor, `invokeShortcut` once per press from its own thread, a 250 ms timeout, focus generations, no replay and no retry (§6.4.4) | The window never waits on the bus. A stale or unknown request must not act after the focus has moved |
| Fail toward KDE: any doubt, or a window call of 500 ms, drops the inhibitor, and the status row says why (§6.4.5) | A broken inhibitor costs every KDE shortcut, the lock included; a broken top row costs only F-keys |
| KWin only, gated on KWin owning kglobalaccel; never GNOME or sway (§6.4.1, §6.4.7) | Without a hand-back, an inhibitor takes every compositor shortcut. GNOME would also prompt |
| The table comes from K-L2 only, ships empty, and `SOPEN_FKEYS_TABLE` serves receipts (§6.4.3) | A guessed code would break a key. The headless receipts need a table before K-L2 |
| The same `fkeys` key as macOS, and `"system"` has no side effect (§6.4.1) | One word, one meaning |
| A terminal: sopen does nothing; a terminal's own inhibitor is not specified (§6.4.7) | sopen owns no surface there; kitty's grab is the terminal's setting |
| The Fn-lock answer to "이거 온오프를 어떻게 할수 있음?" goes in the README (§9 KL-W6) | It is how to use the hardware, not sopen's contract |

## 8. Acceptance

Run locally and on a remote folder under `/tmp/sopen-receipts/`; window receipts as AGENTS.md says.

- K-R1. The list's F-key row reads `1Help 2Rename 3View 4Edit 5Copy 6Move 7MkDir 8Delete 9Dual 10Tree`
  in preview mode and `9Prevw` in dual mode (screenshot); in the hex window it reads `1Help 3Exit
  4Edit 5Save 6Goto 7SaveAs 8Asc/hx 9View` (`9View` in every view of the browser's viewer, `8Encode`
  in Text and Raw: 08 §8, VM-7).
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
  without `hardlink@openssh.com` (a local `sftp-server -P hardlink`: `-P` takes the handler's short
  name, and `-P hardlink@openssh.com` hides nothing) → before S6, copy refused with the reason; from
  S6, the request log shows the rename probe once per session (§5.6: `MKDIR`, two exclusive
  creates, one plain `SSH_FXP_RENAME` answered with status 4, the read-back, two `REMOVE`, one
  `RMDIR`), then the temp published by one plain `SSH_FXP_RENAME`; a name in the way still asks; no
  `.sopen-probe-` entry is left. The same server behind a proxy that answers the probe's rename as
  done (`local_server_with`) → copy refused with the reason, no file written over, nothing staged.
- K-C5. Copy between this machine and a remote, and between two remotes, by editing the target to
  `host:/tmp/sopen-receipts/x` and to a local path → the bytes arrive (SHA-256 equal).
- K-M1. F6 within one filesystem, by object and slice (request log of a local `sftp-server`; never a
  `posix-rename@openssh.com` request):
  - a local file or folder → renamed in place (the inode is kept);
  - a remote regular file (S1) → `hardlink@openssh.com` to the new name, then `REMOVE`; a file the
    account does not own under `fs.protected_hardlinks=1` → the permission reason, the file unmoved;
  - a remote folder before S6 → copied, checked, then removed (the file-unit rules of §5.1: a stop
    after one committed file removes no source); from S6 → one plain `SSH_FXP_RENAME`. *Measured
    2026-11-23 (S6-C, sopen's binary on macOS 26.7.1 against a remote Linux host over ssh): F6 of a
    folder into a sibling folder → moved, its file inside.*
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
  `sftp-server`); F2 on a remote folder before S6 → the reason, nothing changed; from S6 → one plain
  `SSH_FXP_RENAME`, and onto an existing name an error with both left; F2 of a remote link onto a
  name a dangling link holds → an error, the dangling link kept, and no `SSH_FXP_RENAME` sent (a
  link is made again at the new name, never plain-renamed: E2's one replacing case). *Measured
  2026-11-23 (S6-C, the same host): F2 of a remote folder → renamed, its contents kept, the bar on
  it; onto an existing folder → `dirB exists` on the status row, both left; the plain rename over
  the real master and the session's rename probe (verdict: refuses) by the ignored live test.*
- K-E1. Edge cases, each locally and through a local `sftp-server`: copying a folder into itself
  through a link or an alias to it → refused; two names equal only after Unicode normalisation on a
  normalising filesystem → the second is "exists" (decided by the exclusive create); a read-only
  source folder → copied, its bits restored on the copy after its contents; a read-only target folder
  → that item fails with the reason; a server refusing to set times → copied, a note in the report.
- K-K1. F7 `new` → the folder exists, the bar on it; F7 `new` again → an error. *Measured
  2026-11-23 (S6-C) on the remote Linux host: made, the bar on it; again → `made exists` in the
  report.*
- K-D1. F8 on a remote folder → the dialog says `permanent — no Trash on <host>`; Enter removes it
  and everything inside it. F8 on a local file → it is in the system Trash (macOS: in `~/.Trash`;
  Linux: in the Trash's `files` with a `.trashinfo`). *Measured 2026-11-23 (S6-C): on the remote
  Linux host F8 on a folder tree said `permanent — no Trash on <host>` and removed it all; on macOS
  26.7.1 F8 on a local file put it in `~/.Trash` with its bytes. Linux's Trash not yet measured on
  Linux (its freedesktop code runs in the tests on macOS).*
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
- K-F4. `sopen config fkeys system` → the config file holds `fkeys = "system"`. On macOS, sopen never
  opens IOHIDSystem; on Linux, it never binds the inhibit manager and never calls kglobalaccel (test:
  the fakes are never called; K-L5 (a) live).

**Linux (K-L2–K-L8).** Scripted runs never switch, so these items observe other things:
- files that the keys made;
- stand-in signal counts;
- `dbus-monitor` on the private bus;
- `WAYLAND_DEBUG=1` request logs;
- `SOPEN_LOG=debug` lines;
- the owner's eyes.

Before K-L2 the table is `SOPEN_FKEYS_TABLE=224:7` (KEY_BRIGHTNESSDOWN → F7).

**The private session.** K-L5 and K-L6 are `#[ignore]`d live tests (KL-W5), run on the Linux laptop
and never in the owner's session. They keep the isolation of study §1:
- `env -i` with display and bus variables cleared, and its own HOME, XDG and runtime folders (0700);
- `dbus-run-session`;
- `kwin_wayland --virtual` on its own socket, without `--replace`, with `--no-lockscreen`;
- a refusal to start on the owner's bus or display;
- keys only through KWin's fake input;
- stand-in kglobalaccel components;
- a kill of only its own PIDs, and a check that no KDE config of the owner's is newer than the run.

- K-L2. **Owner-assisted receipt: the top row's codes.** This is the browser loop's K-L2. It runs on
  the Linux laptop, from a terminal in the real Plasma session, on the laptop's own keyboard, and
  changes no setting. The owner runs the study's receipt (§6): Esc, F1…F12 without and with Fn,
  Alt+Tab and Meta, and whether anything outside the window changed. Then one step more: **Fn+Esc,
  then the F1 position**. Does it now send KEY_F1? The answer is recorded, and the README states it.
  - The receipt gives the code each position sends, without and with Fn, and the positions that never
    arrive.
  - It shows the inhibitor granted.
  - It shows the backlight unchanged in the inhibit step, by sysfs and by the owner's eye.
  - KL-W7 writes one row per position that sent a single media code. A position that sends an
    F-code, a chord or nothing is no row.
- K-L3. **The key decision** (test, every OS). A table code's press, repeats and release become
  F*n*'s, with winit's modifiers kept and the text dropped:
  - Shift + the code → Shift+F*n*, which nothing binds.
  - Ctrl + the code → handed back when the snapshot holds Ctrl+F*n*, else to sopen.
  - The media chord is never looked up.

  For other keys:
  - A KDE-owned chord is dropped with its repeats and release, and one request is sent per press.
  - An unlisted chord goes to eframe.
  - Meta down then up alone → the modifier-only chord, once. Meta, L, Meta up → Meta+L only. Meta
    then a pointer button → nothing.
  - Focus lost while an F*n* is held → its release.

  Table input:
  - An empty table with no `SOPEN_FKEYS_TABLE` → nothing starts.
  - `224`, `224:13` or `x:1` → an error before the window.
- K-L4. **The hand-back** (test, every OS, fake bus).
  - **Qt codes.** Every chord the owner's KDE binds (keys.md §1) round-trips through the Qt code
    table.
  - **Snapshot.** A recorded `allShortcutInfos` reply parses, and inactive components are left out.
  - **Gate.** No *ready*, and nothing bound or called, when:
    - `fkeys = "system"`;
    - the config is unreadable;
    - kglobalaccel is absent;
    - kglobalaccel's owner is not KWin's;
    - the snapshot fails.
  - **Timeout.** A fake that answers after 300 ms → a failure: the inhibitor is destroyed, and the
    status row says why.
  - **Generations.** At focus-out, the old generation's unsent requests are dropped, and a timed-out
    request is not retried.
  - **Refresh.** A focus-in with its refresh pending → no inhibitor until the refresh is ready.
- K-L5. **Headless: the top row and the inhibitor.** The private session runs sopen with
  `fkeys = "function"` and the folder `a1 b1 l1 l2`, with a stand-in on `Monitor Brightness Down`.
  - (a) With `fkeys = "system"`, KEY_BRIGHTNESSDOWN → the stand-in fires. No inhibit manager is
    bound, and sopen makes no kglobalaccel call.
  - (b) With `"function"`, KEY_BRIGHTNESSDOWN → no fire. Then `made` and Enter → the folder `made`
    exists (F7's dialog).
  - (c) Another window focused → it fires; back on xtools, after the refresh → it does not.
  - (d) `kill -9` while focused → the next press fires.
  - (e) Ctrl+Q → `zwp_keyboard_shortcuts_inhibitor_v1.destroy` comes before the window's
    `wl_surface.destroy`.
- K-L6. **Headless: the hand-back** (the bar on `a1`).
  - (a) Alt down, Tab, Alt up 1 s later → one `invokeShortcut("Walk Through Windows")`, and the other
    window gets the keyboard at Alt's release.
  - (b) A stand-in on Meta+L, pressed ×20 → it fires 20 times. Then F2, `renamed`, Enter → `a1` is
    renamed, so `l` never acted inside xtools.
  - (c) Alt+F4 → one `invokeShortcut` of the action the snapshot names for it, then the window closes
    through sopen's own close path. A stand-in on Ctrl+Alt+J → it fires once, and nothing happens in
    xtools. Ctrl+7, unbound by KDE → sopen's F7: a folder is made.
  - (d) A stand-in on Meta alone → a Meta tap fires it once, and Meta+L does not.
  - (e) A stand-in on Print → it fires once. KEY_VOLUMEUP held 1 s, outside the table, with a
    stand-in → it fires once.
  - (f) **The premises, landing right after KL-W1 (KL-W5):**
    - `GetNameOwner` gives one owner for `org.kde.kglobalaccel` and `org.kde.KWin`;
    - the installed interface has `allComponents`, `allShortcutInfos`, `isActive` and
      `invokeShortcut` with the signatures KL-W2 uses;
    - the snapshot lists the stand-ins, with the lock (Meta+L), Alt+Tab, the launcher and Print under
      kglobalaccel names KL-W2's table resolves.

    If (f) fails, §6.4.1 check 4 or §6.4.4 is rewritten before KL-W2 merges.
- K-L7. **Failing toward KDE** (test, fakes, a test clock).
  - **The guard.** A panic in the inhibitor thread → its guard sends `destroy`.
  - **The window's handle.** The thread gone → the window's handle sends `destroy`.
  - **`on_exit`.** It sends `destroy` even when the thread already did, before the surface goes.
  - **A stall.** A window call of 500 ms → `destroy`. The inhibitor is asked for again once the
    call returns, under §6.4.2's one rule: a stall that ends after a failure since focus-in → no
    inhibitor; a fresh snapshot and a stall ending at the same moment → one `inhibit_shortcuts`.
  - **No waiting.** The creation closure returns at once, against a compositor and a bus that answer
    after 2 s.
  - **The status line.** The hook, then `logic`, then `ui` run in that order, and the status line
    survives `Feed::pass`.
- K-L8. **Owner-assisted: the real session, with K-L2's rows released.** In xtools, on a folder:
  - **The top row.** The key printed F7, without Fn → the MkDir dialog. The F1 key → the key help,
    and the brightness does not change.
  - **Handed back.** Alt+Tab, Meta (the launcher), Meta+L (it locks) and Print (Spectacle) each act
    as without sopen. The `SOPEN_LOG=debug` line names the action each one resolved to in the real
    snapshot.
  - **Outside xtools.** Another window focused → the F1 key changes the brightness.
  - **Death.** `kill -9` while xtools is focused → it changes the brightness again.

## 9. Work units (round 3)

The units build §6.4. Each owns its files, and another unit edits them only through a named
hand-over (§3 rule 5). The gate is `just check` with `SOPEN_REQUIRE_FFMPEG=1` and
`SOPEN_REQUIRE_GIT=1`. Pure logic compiles on every OS and is tested on the Mac too; code under
`#[cfg(target_os = "linux")]` is gated by the Linux run.

**Order:**
1. KL-W1.
2. KL-W5's driver with K-L6 (f).
3. KL-W2 and KL-W3, in parallel.
4. KL-W4, after spec 10's shell unit X1 hands over `src/gui/mod.rs`.
5. KL-W6.
6. KL-W7, after the owner's K-L2.

### KL-W1 — the key decision and the shared types

- **Owns:**
  - `src/fnkeys/top_row.rs` and `top_row_tests.rs`;
  - `src/fnkeys/mod.rs`: the docs at `:1-7` and `:514-517`, and the `mod` lines of every new module;
  - `Cargo.toml` and `Cargo.lock`.
- **Exports:**
  - `Chord` (a winit key, or a code, plus winit's modifiers);
  - `Action` (a component and an action);
  - `Generation`;
  - `Ready`;
  - `Destroy` (the window's destroy request);
  - `Failure` (the status reason);
  - the channel shapes between the window, the hand-back thread and the inhibitor thread.
- **Implements:**
  - `TOP_ROW`, empty, its doc naming K-L2;
  - the parse of `SOPEN_FKEYS_TABLE`;
  - `decide(event, scancode, modifiers, &table, lookup, &mut Held) -> Verdict`, where `Verdict` is
    one of forward, rewrite to F*n*, hand back, or drop. It covers repeats, releases, focus loss and
    the Meta-alone tracker, over winit's types; the Linux caller supplies the scancode.
- **Manifest.** Linux dependencies at the versions already in `Cargo.lock`, so the package list does
  not change:
  - `wayland-client =0.31.15`;
  - `wayland-backend =0.3.17` (`client_system`);
  - `wayland-protocols =0.32.13` (`client`, `unstable`);
  - `raw-window-handle =0.6.2`;
  - `winit =0.30.13`, on Linux too;
  - `zbus =5.19.0`, with `default-features = false` and `["async-io", "blocking-api"]`.
    `blocking-api` is the one feature the tree lacks: accesskit_unix builds zbus without it.
  - A Linux dev-dependency: `wayland-protocols-plasma =0.3.12` (`client`).
- **Tests:** K-L3.

### KL-W5 — the headless driver (right after KL-W1)

- **Owns:** `tests/fkeys_kwin_live.rs`, with `#![cfg(target_os = "linux")]`, `#[ignore]`, and
  `SOPEN_TEST_KWIN=1`.
- **Implements:**
  - the private session with §8's isolation;
  - the fake-input injector;
  - stand-in components;
  - a probe window;
  - capture of `dbus-monitor` and `WAYLAND_DEBUG`.
- **Tests:** K-L6 (f) first. K-L5 and K-L6 (a)–(e) are added once KL-W4 lands, through this unit's
  own hand-over.
- No path of a run (the private session's folders, logs) is printed into the repo.

### KL-W2 — the hand-back

- **Owns:** `src/fnkeys/handback.rs` and `handback_tests.rs`.
- **Implements:**
  - **First,** read the installed kglobalaccel interface files on the Linux laptop, and fix the
    method names and signatures of §6.4.4 from them.
  - **Pure:** the `Chord` ↔ Qt code table, the snapshot parse, and `lookup`.
  - **A `Bus` trait,** with its zbus implementation (`method_timeout(250 ms)`).
  - **The thread.** It reads the config (check 3), checks the gate (check 4), and sends `Ready`. It
    refreshes the snapshot at each focus-in. It invokes per request and per generation; it drops
    stale generations and never retries. Every failure becomes a `Failure`.
- **Tests:** K-L4.

### KL-W3 — the inhibitor

- **Owns:** `src/fnkeys/wayland.rs` and `wayland_tests.rs`, all under the Linux `cfg`.
- **Implements:**
  - **The thread:**
    - the guest queue on winit's display;
    - the manager and the seat bound only after `Ready`;
    - `inhibit` only under §6.4.2's one rule, and `destroy` at focus-out, at a `Destroy` and on a stall
      (the window's 500 ms marks);
    - the guard that destroys on every exit, unwinding included;
    - `poll` on the display's fd and a stop pipe.
  - **The window's handle:** it sends `destroy` when the thread disappears and in `end()`.
- **Tests:** K-L7: the guard, the handle and the stall, against a fake queue.

### KL-W4 — the shell (after X1)

- **Owns:** `src/gui/mod.rs`, by hand-over from spec 10's shell work (X1) once X1 has merged.
- **Implements:**
  - **The run.** `create_native`, with one `EventLoop<eframe::UserEvent>` per process, built by the
    shell with `event_loop_hook()` applied, kept for every window and for the glow retry; the retry
    fires when the shell is still in its slot as the loop returns (§6.4.3). Its handler wrapper calls
    `decide` on key, modifier, pointer-button and focus events, and marks the start and end of every
    handler callback (`new_events` included: eframe runs overdue logic passes there, `run.rs:401-407`)
    for the stall check.
  - **Before the window.** `run_window_sized` parses the table before the window opens.
  - **The creation closure** starts the threads, and the window never waits on them.
  - **`on_exit`** runs `end()`.
  - **Failures** go through `Said` and `publish`.
  - **Debug lines:** `fkeys: top row <code> → F<n>`, `fkeys: handed back <component>/<action>` and
    `fkeys: inhibitor off: <reason>`.
- **Tests:**
  - the hook, `logic` and `ui` in that order (K-L7);
  - a scripted run, `"system"` or an empty table → nothing starts (K-F4);
  - a wgpu that fails before the app → glow on the same loop; on macOS, the loop is built with the
    default menu off.

### KL-W6 — the docs

- **Owns:** `src/config.rs` (doc comments only), `README.md`, `AGENTS.md` and
  `.prd/research/keys.md`.
- **Implements:**
  - **README.** It keeps the macOS paragraph and states five facts:
    1. On KDE Plasma's Wayland session, with `"function"`, while an xtools GUI window has the
       keyboard, the top row sends F1–F12 there, and every other KDE shortcut works as before; a
       held one acts once.
    2. It is off until the laptop keyboard's top row is measured, and the release with those codes
       turns it on.
    3. On GNOME, sway and X11 nothing changes. In a terminal the top row is F-keys only when the
       keyboard sends them: with Fn held, or with its Fn lock.
    4. `"system"` leaves everything alone. The F-key row and Cmd/Ctrl+1…0 work either way.
    5. The keyboard's own Fn lock gives F-keys everywhere, with K-L2's measured answer for Fn+Esc. A
       `hid_asus fnlock_default` module line does nothing (keys.md §4.2).
  - **AGENTS.md:** the `SOPEN_FKEYS_TABLE` row, and the live test in the list of hand-run tests.
  - **`src/config.rs`:** `fkeys` documented for both systems.
- **Tests:** none.

### KL-W7 — the measured table (after K-L2)

- **Owns, by hand-over:**
  - the `TOP_ROW` rows;
  - one test checking them against K-L2's summary, which is checked in with its date and keyboard;
  - README fact 2.
- **Then:** K-L8.

## 10. Tensions

- **Route B needs the GUI shell on `create_native`, and spec 10 reworks that shell.**
  - Today `run_window_sized` calls `run_native` (`src/gui/mod.rs:1300-1341`), which keeps winit's
    events inside eframe.
  - Spec 10 (xtools TUI-first; `research/tui-gui.md`) changes the window backend in X1.
  - KL-W4 is therefore sequenced after X1, and takes `src/gui/mod.rs` from it. If X1 already owns
    the winit handler, KL-W4 only adds its wrapper; if X1 keeps `run_native`, KL-W4 makes the move
    itself.
  - KL-W1, KL-W2, KL-W3 and KL-W5 do not wait.
