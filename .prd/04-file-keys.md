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
- On KDE Plasma's Wayland session, while an xtools window has the keyboard, the top row sends F1…F12 and
  every other KDE shortcut is handed back to KDE (§6.4); this ships off until the top row's codes are
  measured (K-L2).
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

On Linux no keyboard setting can be switched for one app: every Fn-lock control found is root-only,
hardware-specific and global (keys.md §4.2). What exists instead is the compositor's own per-window
opt-out. KDE Plasma's Wayland compositor (KWin) runs its global shortcuts before any app sees a key.
While the focused surface holds a **keyboard-shortcuts inhibitor** (`zwp_keyboard_shortcuts_inhibitor_v1`),
KWin skips them, and a media key reaches the window as a key (K-L1, `research/fkeys-linux.md`).
sopen uses that and changes no setting. It also hands every other shortcut back to KDE, so inside
an xtools window only the top row changes. This reverses keys.md §4.2's "fallback only", which
looked at Fn-lock switches and missed the compositor.

Evidence marks in this section: *measured* = K-L1's private, headless KWin 6.7.5 session with keys
injected through KWin's fake input (`research/fkeys-linux.md` §1–§3, experiments E1–E5). The real
screen backlight, the real keyboards and the owner's session were not exercised: they are K-L2
and K-L8 (§8). [추정] = inferred and not checked. [가설] = a hypothesis that an item of §8 settles.

#### 6.4.1 Where it applies

sopen holds an inhibitor only when **all** of these hold, checked in this order. The first three
cost no I/O:

1. `fkeys` is `"function"`, which it is when the key is absent (§6.2). The run is not a scripted
   run and not a test build (the same rule as the macOS switch: `src/fnkeys/mod.rs:99-105`,
   `:518-520`).
2. The top-row table (§6.4.3) has at least one row.
3. The window is a GUI xtools window on Wayland: eframe's creation context gives a Wayland display
   and window handle (`eframe-0.37.0/src/epi.rs:101-117`). An X11 window (XWayland, or a Plasma X11
   session) has no such protocol and gets nothing.
4. The compositor offers `zwp_keyboard_shortcuts_inhibit_manager_v1`. KWin offers it to every
   client and grants it without a prompt (`research/fkeys-linux.md` §2: KWin
   `src/wayland/keyboard_shortcuts_inhibit_v1.cpp:139-156`, `src/wayland_server.cpp:138-144,
   168-170`). *Measured*: `active` came within the same millisecond as the request.
5. The hand-back (§6.4.4) can work. The session bus's `org.kde.kglobalaccel` has the same owner as
   `org.kde.KWin`, that is, KWin's own kglobalaccel, the one whose shortcuts KWin skips (study §2:
   kglobalaccel lives inside KWin). That both names share one owner on Plasma 6.7 is [추정]; K-L6
   checks it. The first snapshot of the shortcuts (§6.4.4) has also been read.

If any check fails, nothing happens. sopen binds no inhibit manager, opens no second keyboard and
holds no inhibitor. §6.1's clickable row and Cmd/Ctrl+digits work as everywhere. On GNOME, on sway
and on any other compositor, check 5 fails, so sopen never sends a request there (§6.4.7).

#### 6.4.2 The inhibitor and its lifetime

- **One inhibitor per xtools window, for the window's whole life.** KWin applies it only while that
  window's surface has the keyboard. It looks only at whether the *focused* keyboard surface holds an
  active inhibitor (study §2, KWin `src/wayland_server.cpp:818-833`).
  - *Measured*: with the window holding its inhibitor, a second window focused → the brightness
    shortcut fired; focus back on the xtools window → it did not (E1, P2).
  - Creating the inhibitor at each focus-in and destroying it at each focus-out also works
    (*measured*, E1, P3), but KWin does not need it, and sopen does not do it.
- **The connection.** The inhibitor is bound through a second `wayland-client` connection object in
  guest mode on **winit's own** `wl_display` (`Backend::from_foreign_display`,
  `wayland-backend-0.3.17/src/sys/mod.rs:102`). It is fed the display pointer and the window's
  `wl_surface` from eframe's creation context.
  - This is the same Wayland client as winit, which is what lets it name the window's surface. A
    separate socket would be another client, and that client cannot name another client's surface.
  - The connection has its own event queue. It binds the manager and `wl_seat` (version 7), then
    `inhibit_shortcuts(surface, seat)`. A second inhibitor for one surface and seat is a protocol
    error (study §2), so sopen makes exactly one.
  - winit has no support for the protocol (keys.md §4.2), so this is the only way in.
- **Where it is built, and by whom.** The app-creation closure that `run_native` runs
  (`src/gui/mod.rs:1320-1327`) checks 1–3 of §6.4.1 and **only starts two threads**. It never waits
  on the compositor or on the bus. The two threads are:
  - the **key thread** (§6.4.3), handed the display and surface pointers;
  - the **hand-back thread** (§6.4.4).

  The key thread makes the guest connection and binds the manager and the seat; that is check 4.
  The hand-back thread runs check 5 and reads the first snapshot, then says *ready*. The key thread
  sends `inhibit_shortcuts` only once both hold. Until then the window works as without this
  section.

  The `run_native` call and the wgpu→glow fallback stay as they are (`src/gui/mod.rs:1300-1341`). A
  renderer that fails before it creates the app never starts the threads; the renderer that starts
  starts its own.
- **The window's end, in this order.** This runs in `App::on_exit`, which eframe calls while the
  window still exists (`eframe-0.37.0/src/native/glow_integration.rs:508-520`,
  `wgpu_integration.rs:593-607`: `on_exit` runs before the painter and the window go). `Drop` runs
  the same steps as a backstop.
  The window's thread tells the key thread to stop, through its stop pipe. On its way out the key
  thread does steps 1–3, and the window's thread then does step 4:
  1. **The inhibitor is destroyed, and the request flushed.** KWin keys its table by the raw surface
     and removes an entry only when the inhibitor goes (study §3.2: `:58, :146, :170`). If the
     surface went first, a dangling entry would outlive it.
  2. The second keyboard is released (`wl_keyboard.release`).
  3. The guest connection is dropped, and the thread ends.
  4. The window's thread joins the key thread.

  The key thread waits only in `poll` on its stop pipe and the display (§6.4.3), so the join is
  bounded by one dispatch [추정]. All four steps finish before eframe drops the display. wayland-backend requires the `wl_display`
  to outlive every guest backend (`sys/mod.rs:99-101`). The study's prototype saw its key thread
  still reading after the window's exit (study §3.2).
- **When sopen dies**, the inhibitor goes with it. It is a resource of the client, and KWin destroys
  the inhibitor when the client goes (study §3.2, `:83-89`).
  - *Measured*: after `kill -9`, the next press fired the shortcut again. Focus had already moved to
    another window, so this shows recovery, not the removal itself (study §3.2).
  - Nothing is written anywhere, so there is nothing to restore: no state file, no lock, no watchdog.
    The macOS protocol of §6.3 has no Linux counterpart because nothing global changes.

#### 6.4.3 The top row

- **A second keyboard.** The guest connection opens its own `wl_keyboard` on the seat. Every
  keyboard a client opens receives the focused surface's keys (*measured*, E1). So it sees each
  press by its evdev code, including the codes winit turns into nothing.
  - Brightness is one of those: winit maps it to `NamedKey::BrightnessDown` with an unidentified
    physical key, and egui-winit then makes no event (`winit-0.30.13/src/platform_impl/linux/common/xkb/keymap.rs:710`,
    `egui-winit-0.37.0/src/lib.rs:1670`).
  - The keyboard's own thread, the **key thread**, sends the window a *record* for each press and
    release, through a channel, and asks the window for a repaint. A record holds the code, pressed
    or released, and the modifiers in effect.
  - Records exist only between `enter` and `leave` of the window's own surface. Keys already held at
    `enter` are not presses.
  - The modifiers come from this keyboard's own `wl_keyboard.modifiers` events (§6.4.4 says why
    egui's cannot serve).
  - The key thread waits with `EventQueue::prepare_read` and `poll` on the display's fd and on a stop
    pipe (`wayland-client-0.31.15/src/event_queue.rs:456`). It never waits in `blocking_dispatch`.
    So a stop is prompt, and the read protocol that libwayland shares with winit's thread is kept:
    the thread never holds a prepared read without reading it.
- **The table.** It maps an evdev code to a slot 1…12 and lives in `src/fnkeys/top_row.rs`.
  - **Its rows come from the owner's receipt K-L2** (§8; `research/fkeys-linux.md` §6), each row
    naming that receipt, **never from recall**.
  - **On Linux the switch ships OFF until K-L2's measured table exists.** Until the table has a
    row, check 2 of §6.4.1 fails, sopen holds no
    inhibitor whatever `fkeys` says, and Linux behaves as it did before this section. The code, its
    tests and the receipts land first. The change that adds K-L2's rows is the one that turns the
    switch on, by the default (`fkeys` absent = `"function"`).
  - **One table for every keyboard on the seat.** A Wayland seat gives a client one keyboard for
    all devices, and a key carries no device [추정: read from the protocol's model, not checked
    against `wayland.xml` here]. So a dedicated volume key on another keyboard that sends a table
    code is that F-key inside xtools too.
  - A receipt variable, `SOPEN_FKEYS_TABLE=<code>:<slot>[,<code>:<slot>…]`, replaces the table for
    one run. It is for receipts (§8) and for trying another keyboard's table before its rows land.
    A malformed value is an error before the window opens.
- **What a press becomes.** It happens in the shell's raw-input hook (`Shell::raw_input_hook`,
  `src/gui/mod.rs:1109`), which eframe runs before the plugins' hooks
  (`eframe-0.37.0/src/native/epi_integration.rs:423`, then `egui-0.37.0/src/context.rs:1227`). So it
  comes before §6.1's conversion in `FKeyInput::input_hook` (`src/gui/mod.rs:785-796`).
  - A press of a table code becomes **F*n* pressed**, with the record's modifiers. Its release
    becomes F*n*'s release.
  - From there the existing rules apply: exact modifiers (§3 rule 1); no plain F-key acts while a
    field has the typing (§3 rule 4). Shift + the key printed F5 is Shift+F5, which no row binds, so
    nothing happens.
  - An F-key chord made this way goes where that F-key chord would go on a keyboard that sends it
    (§6.4.4).
  - The event that winit makes of a table code is removed, with its repeats and its release, so a
    press is only its F-key. Volume keys have such an event (`egui-winit-0.37.0/src/lib.rs:1648-1650`);
    sopen binds no media key.
  - When the window loses focus, every F-key held this way is released, as with §6.1's digits.
- **No repeats.** The second keyboard does not repeat, because a Wayland client makes its own
  repeats (study §3.2) and sopen makes none for these keys. No F-key action of sopen repeats.
  §6.1's Cmd/Ctrl+digit does repeat as its F-key; the top row does not.

#### 6.4.4 Every other key goes back to KDE

While the inhibitor holds, KWin runs none of its global shortcuts for the window (study §2, KWin
`src/input.cpp:1076-1087`). Alt+Tab, Meta, Meta+L, Print, volume, Ctrl+F1–F4, Alt+F4 and the rest
arrive as keys (*measured* for brightness, Meta, Alt+Tab and Alt+F4, E1). sopen gives each one back.

- **The rule.** A press whose chord a KDE global shortcut owns does **not act inside xtools**:
  neither its key, nor its typed text, nor its repeats or its release. Instead sopen invokes the
  owning action over the session bus: `/component/<component>`,
  `org.kde.kglobalaccel.Component.invokeShortcut("<action>")`.
  - *Measured*: Alt+Tab → `kwin` / `Walk Through Windows` switched focus to the other window (E3).
  - *Measured*: with Alt still held, the call moved the keyboard to the switcher, and focus landed
    on the other window 9 ms after Alt's release, as with the native shortcut (E4).
  - Further Tabs while the switcher is open go to KWin's switcher, which is not inhibited (study
    §3.4) [not measured].
  - So inside an xtools window every key does what it does today, except the top row. The
    exception covers KDE's chords on F-keys too: KDE's chord wins, as it does today.
- **F-key chords from the top row.** A top-row press is the F-key's chord. Ctrl + the key printed F1
  is Ctrl+F1, so it is KDE's (desktop 1) when KDE owns Ctrl+F1, sopen's when sopen's table binds it,
  and nothing otherwise. A table code's own chord, the media key itself, is never looked up.
- **Which chords KDE owns.** A *snapshot* of kglobalaccel's shortcuts answers that:
  - every component (`org.kde.KGlobalAccel.allComponents`);
  - its shortcuts, each with its keys (`org.kde.kglobalaccel.Component.allShortcutInfos`);
  - only components that say they are active (`isActive`), because today an inactive shortcut's key
    reaches the app [추정].
  
  These three methods are named from KF6's D-Bus interface as remembered. Neither K-L1 nor this
  rewrite read them [추정]. KL-W2's first step (§9) reads the installed interface files, and K-L6 calls
  them. Only `action(int)`, which is deprecated, and `invokeShortcut` were *measured* (E3). The study's
  `globalShortcutsByKey` call failed on a wrong signature and was not retried (study §3.3).
  
  The snapshot is read on the hand-back's own thread before the inhibitor is first requested, so
  the inhibitor never exists without one. It is read again at every focus-in: an edited shortcut
  counts from the next focus-in.
- **The chord of a press.**
  - **Its modifiers** (Shift, Ctrl, Alt, Meta) come from the key thread's record, from the masks of
    `wl_keyboard.modifiers`. egui cannot serve here: on Linux egui-winit drops Meta
    (`egui-winit-0.37.0/src/lib.rs:554-564`), so winit's copy of Meta+L reaches egui as a bare `L`.
    Alt is Mod1 and Meta is Mod4, as the usual keymaps say [추정]; K-L6 checks Alt+Tab and Meta+L.
  - **Its key** is the logical key of winit's event for the same press where there is one. That key
    follows the layout, as KDE's matching does [추정]. For codes egui has no key for (brightness when
    it is not in the table, keyboard backlight, mic mute, launcher keys), the key is the code's own.
  - The chord is turned into kglobalaccel's key code (Qt's key plus its modifiers, the form E3 passed
    to `action(int)`) by one table in `src/fnkeys/handback.rs`. Its Qt side comes from Qt's and
    KWin's sources at implementation time. K-L4 tests every chord the owner's KDE binds (keys.md §1),
    and K-L6 runs a sample live.
- **Pairing the two copies of a press.** winit's event and the key thread's record of the same
  press arrive on two queues, and nothing fixes their order [추정]. This rule is route A's cost
  and goes if the GUI shell moves to route B (§10). Only a **press** pairs: a winit
  key event with `pressed` and not `repeat`, with a record of a press of the same physical key.
  - **What waits.**
    - A press's repeats (winit makes them; the second keyboard does not), its release and its typed
      text follow that press's verdict. They are decided at the press and kept until the key is up,
      as §6.1's digits are (`src/gui/mod.rs:767-778`).
    - An event with no physical key, and text with no key before it (an input method's commit, a
      paste), never waits.
    - Holding an ordinary key therefore never counts as a stall.
  - **winit's copy first.** The press, with whatever follows it, is not acted on until its record is
    in. It waits in the hook (kept for the next pass, with a repaint asked for), at most **50 ms**.
    Past that, the key thread counts as stalled (§6.4.5), and the press goes on as it is.
  - **The record first.**
    - A record whose code has no egui physical key is decided at once from the code. These are
      modifiers, brightness, keyboard backlight and the like, for which winit makes no egui event.
    - Any other record waits, at most 50 ms, for its winit copy, so that it is decided with the
      copy's logical key.
    - Past 50 ms without its copy, it is decided with its physical key as the key [추정 that this is
      rare: winit dispatches on the window's thread before each pass].
    - A record decided as handed back drops its winit copy, with what follows it, whenever that copy
      comes.
  - **The physical key of a code** is winit's `PhysicalKey::from_scancode`, which on Linux takes the
    evdev code as it is (`winit-0.30.13/src/platform/scancode.rs:27`,
    `platform_impl/linux/common/xkb/keymap.rs:33`), plus a copy of egui-winit's private
    `key_from_key_code` (`egui-winit-0.37.0/src/lib.rs:1677`). A test pins egui-winit's version, so an
    upgrade re-checks the copy. `from_scancode` means something else on macOS, so the pairing logic
    takes the map as a parameter and its tests inject one.
- **The window's thread never calls the bus.** The window only sends to the hand-back's thread. The
  thread invokes the action **once per press**: a held chord acts once.
- **Meta alone.** This covers KWin's modifier-only shortcuts; E1's "Meta tap" fired without the
  inhibitor and not with it.
  - Meta alone is handed back at Meta's release when no other key and no pointer button was pressed
    since Meta's press, and the snapshot holds a modifier-only chord for Meta.
  - [가설]: KWin's own rule may have more conditions (study §3.4). K-L6 measures it.

#### 6.4.5 Failing toward KDE

- **What fails, and what sopen does.** Each of these destroys the inhibitor at once:
  - the hand-back fails: a snapshot cannot be read, an invocation fails, or its thread ends;
  - the key thread fails: its thread ends, or a record is more than 50 ms late (§6.4.4).

  KDE's shortcuts are back, and the top row is media keys again. The status row says
  `the F-key switch did not take effect: <reason>` (`NOT_IN_EFFECT`, `src/fnkeys/mod.rs:62`). The
  inhibitor is not requested again in that window.
- **A stalled window.** Suppose the window's thread has not taken a record 500 ms after a press. The
  key thread then destroys the inhibitor itself, because it owns the guest queue. It requests the
  inhibitor again once the window takes records again. So a window busy for seconds costs the top
  row for that time, never Alt+Tab or Meta+L.
- **What KWin keeps for itself, inhibitor or not.** These checks come before the inhibitor check
  (study §2):
  - the power key;
  - Ctrl+Alt+F1…F12 (VT switch);
  - the locked screen;
  - screen edges and pointer gestures;
  - an Alt+Tab switcher that is already open.
- **No escape chord.** KWin has none (study §2). A process stopped whole (`SIGSTOP`) keeps its
  inhibitor while its window has the keyboard. A click on another window, or a VT switch, ends it.

#### 6.4.6 What the hand-back does not reproduce

| Case | What happens inside an xtools window | Evidence |
|---|---|---|
| A held handed-back chord (volume on a code outside the table, a held Meta+arrow) | acts once per press; KWin's own repeat of a held shortcut (study §2, `input.cpp:1076-1087` press/repeat) does not happen | decided; the second keyboard has no repeat (study §3.2); K-L6 measures one invocation per press |
| Meta alone | handed back on release by the rule of §6.4.4 | [가설]; K-L6 |
| Meta+L (lock) | handed back like any chord, to the shortcut's owner (ksmserver's `Lock Session` [추정]). If that fails, the screen does not lock, and the status row says so (§6.4.5). The idle lock is not a shortcut and is unaffected | [추정]: the private session ran with `--no-lockscreen`; K-L8 is the owner's check |
| Print (Spectacle) | handed back to Spectacle's component | [추정]: not run; K-L6 runs a stand-in, K-L8 the real one |
| Screen-reader and accessibility key grabs | off while the window has the keyboard. KWin skips its a11y keyboard monitor when inhibited (study §2, `keyboard_input.cpp:286-291`), and it has no kglobalaccel action to invoke | [S] from the study |
| A modifier remapped in KDE's keyboard settings (Caps Lock as Ctrl) | counts as the modifier its real-mod mask says | [추정]; not checked |
| Kernel-handled positions (some keyboards' backlight and fan keys, a kernel Fn lock) | never become keys at all; unchanged, neither F-keys nor handed back | study §3.3 [S, 추정 for the kernel build] |

#### 6.4.7 Other desktops

- **GNOME: not requested.** Mutter asks the shell, which shows an "Allow inhibiting shortcuts"
  dialog (Allow / Deny). It remembers the answer per app id, and asks every time for a window
  without one (study §4, `gnome-shell js/ui/inhibitShortcutsDialog.js`; sources only). Two reasons
  not to ask:
  - a dialog the first time an xtools window opens is itself a change the owner did not ask for;
  - GNOME has no kglobalaccel to hand its shortcuts back to, so once allowed, every GNOME shortcut
    but Super+Escape (mutter's `restore-shortcuts`) would be gone while the window has the keyboard.
- **sway and other wlroots compositors: not requested.** sway honours the inhibitor by default
  (study §4, `sway-input.5.scd:302-312`; sources only). It has no "which binding owns this key" query
  to hand its bindings back through, so an inhibitor would take the owner's workspace and window
  keys while xtools has the keyboard.
- **X11 sessions and XWayland windows**: there is no such protocol. §6.1 is the answer.
- Each of these gets §6.1's clickable row and Cmd/Ctrl+digits, and the keyboard's own Fn lock (the
  README says how).

#### 6.4.8 TUI mode

xtools runs as a TUI by default (`xtools/ssot.md` item 2). In a terminal, **sopen owns no surface**:
an inhibitor names a `wl_surface` of the client that asks for it, and over ssh there is no Wayland
at all (study §5; `research/tui-gui.md` §10). Neither §6.2's switch nor this section's inhibitor
applies to a TUI. KWin takes a globally bound media key before the terminal sees it (study §5, KWin
`input.cpp:1076-1082`), **unless the terminal itself holds a shortcuts inhibitor**.

- **The terminal's own inhibitor: not specified.** Whether a terminal holds one is the terminal's
  setting, not sopen's, and this is left to a later study or to the owner's terminal setting
  (`tui-gui.md` §14 item 6). It is not impossible:
  - **kitty**, one of the laptop's terminals, has a `grab_keyboard` action and a `--grab-keyboard`
    flag. XR-T reads them as holding the same inhibitor (`tui-gui.md` §10, from kitty's installed
    sources) [추정 until measured].
  - While kitty holds it, every KDE shortcut goes to kitty while kitty is focused. sopen hands
    nothing back there: the inhibitor is kitty's.
  - The kitty keyboard protocol has no brightness code (study §5). Even a grabbing kitty would need
    its own key mapping (a `map` in its config) to send F-keys for the top row [추정].
  - **herdr-app**, the other terminal, holds no inhibitor (`tui-gui.md` §10 *measured*: no inhibitor
    string in it).
- **Without it, the top row is F-keys only when the keyboard sends F-codes**: with Fn held, or with
  the keyboard's own Fn lock. A real F1 does reach a TUI, as its escape sequence (the kitty keyboard
  protocol's `CSI P` form among them, study §5).
- **§6.1's fallbacks, where a terminal can carry them**:
  - The clickable row works where the terminal reports mouse clicks.
  - The digit aliases are the TUI spec's (XR-T proposes Alt+1…0, `tui-gui.md` §14 item 7). Ctrl+digits
    can be told apart only with the kitty keyboard protocol's disambiguate flag (study §5).
  - On macOS terminals, Cmd+digits switch the terminal's tabs and never reach the TUI [추정].
- **A macOS terminal is not specified here.** The HIDFKeyMode switch of §6.2 is process-agnostic. A
  *local* TUI could take it while its terminal has focus, by the terminal's focus reports (DECSET
  1004) [가설; not prototyped, study §5]. Over ssh a remote TUI cannot reach the Mac's setting. This
  is a later study; until it exists, a TUI never touches HIDFKeyMode.

#### 6.4.9 The config key

The key is the same as on macOS (§6.2), with the same `sopen config fkeys function|system`.

- `fkeys = "function"`, or the key absent: on KDE Plasma's Wayland session, the inhibitor plus the
  hand-back, for each xtools window, when §6.4.1 holds. Elsewhere on Linux, nothing.
- `fkeys = "system"`: sopen never binds the inhibit manager, never opens a second keyboard and never
  calls kglobalaccel.
- How to switch a keyboard's own Fn lock is the README's, not this contract's.

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
| On KDE Plasma Wayland, each xtools window holds one `zwp_keyboard_shortcuts_inhibitor_v1` for its whole life; nothing global is changed | Message 19 asks for F1 as F1 inside xtools without touching the OS. The inhibitor is the compositor's per-window opt-out: KWin grants it without a prompt, applies it only while that surface has the keyboard, and drops it when the client dies (K-L1: *measured* in a private KWin 6.7.5). Per-focus create/destroy was measured to behave the same, so it would only add code |
| Bound through a guest `wayland-client` connection on winit's own `wl_display`; at the window's end the inhibitor is destroyed first, then the keyboard is released and the key thread joined, all before eframe drops the display | winit has no inhibitor support. Only the same Wayland client can name the window's surface, and a separate socket would be another client. KWin keys inhibitors by raw surface, so a surface destroyed first leaves a dangling entry (study §3.2). wayland-backend requires the display to outlive the guest (`sys/mod.rs:99-101`) |
| Route A for the first cut: a second `wl_keyboard` on the guest connection feeds records to `Shell::raw_input_hook`; `run_native` and the wgpu→glow fallback stay as they are | Route A keeps sopen's run path (`src/gui/mod.rs:1300-1341`). Route B (eframe's external event loop, one key source) is cleaner and both were measured (study §3.1). Route A's cost is the pairing rule below; moving to route B would delete it (§10) |
| Only the top row changes. A press of a table code is F*n* with the modifiers held, and every chord a KDE global shortcut owns is handed back and does not act inside xtools: not its key, its text, its repeats or its release | "f1키가 실제 F1키 역활 … os는 건드리지 말고" (message 19): the top row is the one change. Every other key keeps today's meaning, where KWin took the key before sopen. Handing back is *measured* for Alt+Tab, also with Alt held (E3, E4) |
| An F-key chord made from the top row goes where that F-key chord goes: KDE's when KDE owns it (Ctrl+F1 → desktop 1), else sopen's table, else nothing. A table code's media chord is never looked up **(K-S4 addition)** | "The top row is F1…F12" means the chords of a keyboard that sends F-keys. Looking the media chord up as well would make one press two keys |
| Hand-back = a snapshot of kglobalaccel's active shortcuts (`allComponents`, `allShortcutInfos`, `isActive`) read on a thread of its own before the first inhibitor and again at each focus-in, and `invokeShortcut` sent from that thread, once per press **(K-S4 addition: the snapshot and its refresh)** | A key must be decided in the pass it arrives in, and the window's thread never waits on the bus (the same rule as the macOS switch's thread, §6.2). The study measured only the deprecated `action(int)` and `invokeShortcut`. Its per-chord `globalShortcutsByKey` call failed on a signature error and was not retried. So the snapshot methods are [추정] until K-L6 |
| A press's modifiers come from the second keyboard's `wl_keyboard.modifiers`; its key comes from winit's logical key where there is one, else from the code **(K-S4 addition)** | egui-winit drops Meta on Linux (`lib.rs:554-564`), so egui cannot tell Meta+L from L. winit's logical key follows the layout as KDE's matching does [추정] |
| Pairing. Only a non-repeat press pairs with its record, and its repeats, release and text follow its verdict. Whichever copy comes first waits for the other, up to 50 ms. Records with no egui physical key, and text with no key (IME, paste), never wait. A handed-back record drops its winit copy when that copy comes **(K-S4 addition)** | The two copies of a press arrive on two queues in no fixed order [추정]. Without the wait, a lagging record lets Meta+L act inside xtools as the jump letter `l`. winit repeats and the second keyboard does not, so pairing repeats would read every held key as a stall. 50 ms is far above the key thread's normal lag and is still not felt in typing [추정] |
| The creation closure only starts the key thread and the hand-back thread. The key thread binds and sends the inhibitor request once the hand-back says ready **(K-S4 addition)** | The window's thread never waits on the compositor or the bus, the same rule as the macOS switch's thread (§6.2), and the inhibitor never exists without a snapshot |
| Fail toward KDE. A hand-back error, a key-thread error or a record 50 ms late destroys the inhibitor for the rest of the window, with the status row saying why. A window that takes no record for 500 ms after a press has its inhibitor destroyed by the key thread until it takes records again **(K-S4 addition)** | A broken inhibitor costs every KDE shortcut, the lock (Meta+L) included. A broken top row costs only F-keys, and §6.1 still works |
| Inhibit only on KWin with KWin's own kglobalaccel: `org.kde.kglobalaccel` and `org.kde.KWin` have one owner. Not on GNOME, not on sway | Without a hand-back, an inhibitor takes every compositor shortcut. GNOME would also show an Allow/Deny dialog the first time (study §4). sway honours the inhibitor but has no query to hand bindings back through. Checking that both names have one owner keeps a stray kglobalacceld on another desktop from passing as KDE's [추정 that they share one owner on Plasma 6.7; K-L6] |
| The top-row table comes from K-L2 only, never from recall. Until it has a row, the Linux switch is off whatever `fkeys` says; the change that adds the rows turns it on by the default | A guessed code either leaves brightness a media key or turns a real key into an F-key. Gating on a non-empty table makes "off until measured" a property of the code rather than of the release notes. This reconciles the study's "`function` is the default" (§8) with "off until K-L2" |
| One table for all keyboards of the seat | Wayland gives a client one keyboard per seat, and a key carries no device [추정]. A dedicated volume key that sends a table code becomes that F-key inside xtools |
| `SOPEN_FKEYS_TABLE=<code>:<slot>,…` replaces the table for one run, for receipts only **(K-S4 addition)** | The headless receipts (K-L5, K-L6) need a table before K-L2 exists, and another keyboard's table can be tried before its rows land. Scripted-run hooks cannot be used because they never switch (`src/fnkeys/mod.rs:99-105`) |
| The top row's F-keys do not repeat | The second keyboard makes no repeats (seat v7, study §3.2), and no F-key action of sopen repeats. §6.1's Cmd/Ctrl+digits keep their repeats |
| The same `fkeys` key as macOS: `"function"` (absent) / `"system"`; `"system"` never binds the inhibit manager, opens no keyboard and calls no bus | One word for one meaning on both systems (§6.2's precedent). `"system"` must be free of side effects, prompts included |
| TUI mode: sopen holds no inhibitor (it owns no surface). Without a terminal that holds one itself, the top row is F-keys only where the keyboard sends them, plus §6.1 where the terminal can carry it. A terminal's own inhibitor (kitty's `grab_keyboard`) and a macOS TUI taking HIDFKeyMode on terminal focus reports are both not specified | An inhibitor names a surface of the client that asks, and KWin takes bound keys before the terminal unless the terminal holds one (study §5; `tui-gui.md` §10). kitty's grab is the terminal's setting, not sopen's, and it is [추정 until measured]. Even with it, the kitty protocol has no brightness code, so kitty would need its own map. herdr-app has none. The macOS idea is [가설] and was not prototyped. Both are left to a later study or to the owner's terminal setting (`tui-gui.md` §14 item 6) |
| The answer to "이거 온오프를 어떻게 할수 있음?" (the keyboard's own Fn lock) is in the README, not here | It is how to use the hardware, not part of sopen's contract. The README also says that a `hid_asus fnlock_default` module line does nothing, which was measured |

Rows marked **(K-S4 addition)** are mechanisms that neither the feasibility study (K-L1,
`research/fkeys-linux.md`) nor the decisions taken before this rewrite name: this section's own,
each with its reason, reversible like the rest.

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
- K-F4. `sopen config fkeys system` → the config file holds `fkeys = "system"`.
  - On macOS, sopen never opens IOHIDSystem (test: the fake is never called).
  - On Linux, sopen never binds `zwp_keyboard_shortcuts_inhibit_manager_v1`, never opens a second
    keyboard and never calls the session bus for kglobalaccel. The test checks that the Wayland and
    bus fakes are never called; K-L5 (a) checks the same live.

**The Linux items (K-L2 to K-L8).**

**What each kind of item observes.** Scripted runs never switch (`src/fnkeys/mod.rs:99-105`), so
no Linux item can use `SOPEN_SCREENSHOT`, `SOPEN_KEYS` or another scripted-run hook. Each item
below names its observable instead:
- a file or folder the keys made;
- the stand-in shortcut's signal count;
- the private bus seen through `dbus-monitor`;
- the client's request log under `WAYLAND_DEBUG=1`;
- sopen's `SOPEN_LOG=debug` lines;
- the owner's eyes.

**Where the table comes from before K-L2.** It comes from the receipt variable
`SOPEN_FKEYS_TABLE` (§6.4.3), never from a guessed built-in row.

**The private session (K-L5, K-L6).** These are `#[ignore]`d live tests, run by hand on the Linux
laptop and never in the owner's Plasma session. Each restates the isolation of
`research/fkeys-linux.md` §1:
- `env -i`, with `DISPLAY`, `WAYLAND_DISPLAY` and `DBUS_SESSION_BUS_ADDRESS` cleared;
- its own `HOME` and `XDG_*` folders, and its own runtime folder at 0700;
- `dbus-run-session`, so the session has its own bus;
- `kwin_wayland --virtual` on a socket of its own, never `--replace`, with `--no-lockscreen`;
- the driver refuses to start when the bus is the owner's or a display is set, and it saves each
  run's environment;
- keys go in only through KWin's fake input, and stand-in kglobalaccel components are registered on
  the private bus (study E1);
- at the end, the driver kills only its own processes and checks that no `kwin*`, `kglobal*`,
  `kscreenlocker*`, `powerdevil*` or `kxkb*` file of the owner's config is newer than the run's
  start.

- K-L2. **Owner-assisted receipt, the top row's codes** (the browser loop's K-L2; the Linux
  laptop, run from a terminal in the real Plasma session, any terminal; the laptop's own keyboard;
  no setting is changed). The owner runs the study's receipt (`research/fkeys-linux.md` §6): Esc,
  F1…F12 without and with Fn, Alt+Tab and Meta, and answers whether brightness or anything outside
  the window changed. It settles:
  - the code each top-row position sends without Fn and with Fn, and the positions that never
    arrive (kernel-handled);
  - that the inhibitor is granted in the real session;
  - that the backlight stays put in its inhibit step, by the sysfs value and by the owner's answer,
    while Alt+Tab and Meta land in the window.

  Its printed summary is what KL-W7 (§9) writes into `src/fnkeys/top_row.rs`: one row (code →
  slot) per position that sent a media code, each naming the receipt's date and keyboard. A
  position that sent an F-code, or nothing, is no row. Until this receipt exists, the Linux switch
  ships OFF (§6.4.3).
- K-L3. **The frame's keys** (test, on every OS: pure logic over records and egui events).
  - **Table codes.**
    - A table code's press and release each become one F*n* press and one release.
    - The record's modifiers are kept exactly: Shift + the code is Shift+F*n*, which no row binds,
      so nothing acts.
    - winit's volume event for a table code is removed, with its repeats and its release.
    - A code outside the table is left as it is.
    - A key held at `enter` is not pressed.
    - Focus lost while an F*n* is held → that F*n* is released.
    - With a text field focused, the F*n* arrives, but no plain F-key row acts (§3 rule 4).
    - With the table empty and no `SOPEN_FKEYS_TABLE`, no inhibitor is asked for, even with
      `fkeys = "function"`.
    - A malformed `SOPEN_FKEYS_TABLE` (`224`, `224:13`, `x:1`) is an error before the window opens.
  - **F-key chords.**
    - Ctrl + a table code → Ctrl+F*n*, handed back when the snapshot holds Ctrl+F*n*, else given to
      sopen's table.
    - A table code's own media chord is never looked up.
  - **Pairing.**
    - **winit first.** A winit press that comes before its record is held until the record arrives,
      then acted on or dropped. Its text and repeats are held with it.
    - **Record first.**
      - A Meta+L record whose winit `L` has not come yet is handed back. The later `L`, its text, its
        repeats and its release are dropped.
      - A record of a code with a physical key whose winit copy is 50 ms late is decided with its
        physical key.
    - **No pairing needed.** A record of a code with no physical key (brightness, a modifier) is
      decided at once.
    - **A stall.** A winit press whose record is 50 ms late goes on as it is, and the stall is
      reported (K-L7).
    - **Holding is not a stall.** An ordinary key held 1 s gives winit repeats with no records. The
      repeats act as they do today, and the inhibitor stays.
    - **Never held.** Text with no key before it (an input method's commit, a paste) and an event with
      no physical key are passed on in the same pass.
  - **Meta alone.**
    - Meta down, Meta up, with nothing between → the modifier-only chord is handed back once.
    - Meta down, L, Meta up → only Meta+L; the modifier-only chord is not handed back.
    - Meta down, a pointer button, Meta up → nothing.
  - **Version pin.** The copied egui-winit code table is pinned: the test fails when `Cargo.lock`'s
    egui-winit is not 0.37.0.
- K-L4. **The hand-back's tables and gate** (test, on every OS, against fakes of the bus).
  - **Qt key codes.** Every chord the owner's KDE binds (keys.md §1) goes to kglobalaccel's key code
    and back: Ctrl+F1…F4, Ctrl+F7/F9/F10/F12, Alt+F1…F4, Meta+F5…F10, Meta+0…9, Alt+Tab,
    Meta+Tab, Meta+L, Meta+W, Meta+D, Meta+Alt+K, Print, volume ±/mute, mic mute, screen and
    keyboard brightness ±.
  - **Snapshot.** A recorded `allShortcutInfos` reply is parsed into the snapshot. An inactive
    component's chords are left out.
  - **Gate.** Any of these → no inhibitor asked for, and nothing invoked:
    - `org.kde.kglobalaccel` absent;
    - `org.kde.kglobalaccel` and `org.kde.KWin` with different owners;
    - the snapshot failing.
  - **Refresh.** A focus-in rereads the snapshot. A chord added between two focus-ins counts from
    the second.
- K-L5. **Headless: the top row and the inhibitor's lifetime** (the private session above). sopen
  runs inside it with `fkeys = "function"` in the private config and `SOPEN_FKEYS_TABLE=224:7`
  (KEY_BRIGHTNESSDOWN → slot 7). The folder holds `a1`, `b1`, `l1` and `l2`, and a stand-in component
  is bound to `Monitor Brightness Down`.
  - (a) **Control.** With `fkeys = "system"`, KEY_BRIGHTNESSDOWN → the stand-in fires, and:
    - `WAYLAND_DEBUG` shows no bind of the inhibit manager;
    - `dbus-monitor` shows no call from sopen to kglobalaccel.
  - (b) **Treatment.** KEY_BRIGHTNESSDOWN → the stand-in does not fire. Then the text `made` and Enter
    → a folder `made` exists: the brightness code opened F7's MkDir dialog.
  - (c) **Focus.** A second window focused → KEY_BRIGHTNESSDOWN fires the stand-in. Back on xtools →
    it does not.
  - (d) **Death.** `kill -9` of sopen while its window has the keyboard → the next KEY_BRIGHTNESSDOWN
    fires the stand-in, wherever KWin puts the focus.
  - (e) **Teardown.** sopen closed with Ctrl+Q:
    - in `WAYLAND_DEBUG`, `zwp_keyboard_shortcuts_inhibitor_v1.destroy` comes before the window's
      `wl_surface.destroy`, and `wl_keyboard.release` comes before the display disconnects;
    - no guest-queue event is logged after sopen's last debug line, unlike the study's prototype
      (study §3.2).
- K-L6. **Headless: the hand-back** (the same session and table, xtools focused, with the bar on
  `a1`).
  - (a) **Alt+Tab.** Alt down, Tab, Alt released 1 s later, from one injector process:
    - `dbus-monitor` shows one `invokeShortcut("Walk Through Windows")`;
    - the second window gets the keyboard at Alt's release (as E4).
  - (b) **Meta+letter.** A stand-in is bound to Meta+L.
    - Meta+L ×20 → the stand-in fires 20 times.
    - Then F2 (`KEY_F2`), the text `renamed`, Enter → it is `a1` that is renamed. The bar never
      moved, so the jump letter `l` never acted inside xtools. This is the pairing's discriminating
      check.
  - (c) **Alt+F4 and sopen's own chords.**
    - Alt+F4 → one `invokeShortcut` of the action that the snapshot names for Alt+F4 in the `kwin`
      component, then sopen's window closes through its own close path, and sopen exits 0.
    - A stand-in bound to Ctrl+Alt+J → fires once, and nothing happens in xtools.
    - Ctrl+7, which KDE does not bind → sopen's own F7 (§6.1): a folder made by the dialog exists.
  - (d) **Meta alone.** A stand-in is bound to Meta alone.
    - A Meta tap → it fires once.
    - Meta+L → it does not fire.
  - (e) **Print and repeat.**
    - A stand-in bound to Print → fires once.
    - KEY_VOLUMEUP, not in this run's table, held 1 s with a stand-in bound to it → it fires once:
      the stated difference (§6.4.6).
  - (f) **The gate's premise.** In this session, `GetNameOwner` gives one owner for
    `org.kde.kglobalaccel` and for `org.kde.KWin`. The snapshot lists the stand-ins with their keys,
    and Alt and Meta are read from Mod1 and Mod4. If (f) fails, §6.4.1 check 5 and §6.4.4's masks are
    rewritten before KL-W4 merges (§9: KL-W2's gate, KL-W3's masks).
- K-L7. **Failing toward KDE** (test, against fakes of the guest queue, the bus and a test clock).
  - A failing invocation → the inhibitor is destroyed, and the status row reads
    `the F-key switch did not take effect: <reason>`. Later presses go to KDE: nothing is handed back
    and nothing is turned into an F-key.
  - The key thread ending → the same.
  - A record 50 ms late → the same.
  - A window that takes no record for 500 ms after a press → the key thread destroys the inhibitor
    itself. The window taking records again → it is asked for again, once.
  - The teardown order of K-L5 (e) holds against the fakes, also when the key thread is blocked in
    its `poll` at the time of the stop.
  - The creation closure returns without waiting, against fakes of the compositor and the bus that
    answer only after 2 s. No inhibitor is asked for until the hand-back says ready.
- K-L8. **Owner-assisted, the real session, after K-L2's rows land** (the Linux laptop, a sopen
  release with the table, the default config, the laptop's own keyboard). In xtools, on a folder:
  - **The top row.**
    - The key printed F7, without Fn → the MkDir dialog.
    - The key printed F1 → the key help, and the screen brightness does not change: the owner looks,
      and the study's backlight poll logs the value.
  - **Handed back.**
    - Alt+Tab → switches windows.
    - Meta → the launcher.
    - Meta+L → the screen locks (the owner unlocks).
    - Print → Spectacle.
  - **Outside xtools.** A click on another window → the same F1 key changes the brightness.
  - **Death.** `kill -9` of sopen while xtools has the keyboard → that key changes the brightness
    again.
  - Until this item exists, §6.4's "the brightness does not change" holds only in the private session
    (K-L5).

No item covers TUI mode. The TUI's own spec (XR-T) carries its key receipts; §6.4.8 is the contract
those receipts check.

## 9. Work units (round 3)

The units that build §6.4 (Linux).

Each unit owns its files: no other unit edits them, except through a **declared hand-over**. A
hand-over is the named lines and nothing else, in the sense of §3 rule 5.

The gate for every unit and every merge is `just check`, with `SOPEN_REQUIRE_FFMPEG=1` and
`SOPEN_REQUIRE_GIT=1` (browser loop, "Build facts"):
- units marked **both** run it on macOS and on Linux;
- units marked **Linux** compile their new code only on Linux (`#[cfg(target_os = "linux")]`), so
  their gate has to be the Linux run.

Pure logic is kept out of the `cfg`, so its tests also run on the Mac.

The order is KL-W1, then KL-W2 and KL-W3 (in parallel), then KL-W4, KL-W5 and KL-W6. KL-W4 waits
for spec 10's decision on the GUI shell (§10). KL-W7 comes last, after the owner's K-L2.

### KL-W1 — the frame's keys, the manifest, the module docs (gate: both)

**Owns:**
- `src/fnkeys/top_row.rs` (new);
- `src/fnkeys/top_row_tests.rs` (new);
- `src/fnkeys/mod.rs`;
- `Cargo.toml` and `Cargo.lock` (for every unit below).

**Builds:**
- `TOP_ROW: &[(u16, u8)]`, empty. Its doc says the rows come from K-L2 only, and each row names
  the receipt.
- `table()`: `SOPEN_FKEYS_TABLE` parsed (an error before the window opens), else `TOP_ROW`.
- The `Record` type: the code, pressed or released, and the modifiers Shift, Ctrl, Alt and Meta.
- The `Chord` type: an egui key, an evdev code, or a modifier alone, plus the modifiers.
- The code → egui physical key map, in two parts:
  - a copy of egui-winit 0.37.0's private `key_from_key_code`, which is OS-independent and is
    tested on every OS;
  - on Linux, winit's `PhysicalKey::from_scancode`, which takes the evdev code as it is there
    (`platform_impl/linux/common/xkb/keymap.rs:33`) and means another code on macOS.
- `frame(...)`. Its inputs are:
  - the pass's egui events and the new records;
  - the table;
  - the code → physical key map, passed in, so the tests on macOS inject one;
  - a lookup closure (`&Chord -> Option<Action>`), so this unit does not depend on KL-W2;
  - a `Held` state.

  It does:
  - the top-row translation;
  - the removal of winit's own events for table codes;
  - the pairing of a non-repeat press with its record, whichever copy comes first, up to 50 ms.
    The press's repeats, release and text follow its verdict; text with no key, and events with no
    physical key, never wait;
  - the drop of handed-back presses with what follows them;
  - the Meta-alone tracker;
  - focus-loss releases.

  It returns the rewritten events, the hand-backs to send, and a stall flag.

**`src/fnkeys/mod.rs`:**
- Rewrite the doc at `:1-7` ("Linux and Windows have no such switch") and at `:514-517` ("any OS
  but macOS (§6.4)"). The HID switch stays macOS-only, and Linux's is §6.4's inhibitor in its own
  modules.
- Add `pub mod top_row;`.
- `platform_switch` (`:560-568`) stays as it is: the HID switch is still none off macOS.

**`Cargo.toml`.** Add Linux target dependencies at the exact versions already in `Cargo.lock`, with
no new crate. Check it with a diff of the lock's package list before and after:
- `wayland-client = "=0.31.15"`;
- `wayland-backend = { version = "=0.3.17", features = ["client_system"] }`;
- `wayland-protocols = { version = "=0.32.13", features = ["client", "unstable"] }`;
- `raw-window-handle = "=0.6.2"`;
- `winit = { version = "=0.30.13", default-features = false }` on Linux too, for `from_scancode`.
  winit is already a macOS dependency.
- `zbus = { version = "=5.19.0", default-features = false, features = ["async-io", "blocking-api"] }`.
  `blocking-api` is the one feature that nothing in the tree enables today: `accesskit_unix` builds
  zbus with `async-io` only (`accesskit_unix-0.24.1/Cargo.toml:100-103`). `zbus::block_on` is
  `#[doc(hidden)]` and is not used.
- Under `[target.'cfg(target_os = "linux")'.dev-dependencies]`,
  `wayland-protocols-plasma = { version = "=0.3.12", features = ["client"] }` (KWin's fake input, for
  KL-W5).

Each addition carries a comment in the manifest's existing style ("Already in the tree (…), at the
same version").

**Tests:** K-L3, all on every OS, against hand-made records and egui events, with a test clock for
the 50 ms hold.

### KL-W2 — the hand-back to KDE (gate: both; the bus half is Linux)

**Owns:** `src/fnkeys/handback.rs` (new) and `src/fnkeys/handback_tests.rs` (new). **Hand-over:**
one line `pub mod handback;` in `src/fnkeys/mod.rs`.

**First step.** On the Linux laptop, read the installed kglobalaccel D-Bus interface files: the
`org.kde.KGlobalAccel` interface and the component interface, under the system's
`dbus-1/interfaces`. Record in the module doc the signatures of `allComponents`,
`allShortcutInfos`, `isActive` and `invokeShortcut`. These names come from memory in §6.4.4 and
are [추정]; if the files disagree, §6.4.4 is corrected in the same PR.

**Builds:**
- **Pure, on every OS:**
  - the table between a `Chord` and kglobalaccel's key code (Qt's key plus modifiers), its Qt side
    taken from Qt's and KWin's sources, cited;
  - the parse of `allShortcutInfos` into a snapshot that keeps active components only;
  - `lookup(&Chord) -> Option<(component, action)>`.
- **A `Bus` trait:** name owners, `allComponents`, `allShortcutInfos`, `isActive` and
  `invokeShortcut`.
- **Its zbus implementation**, on Linux only, through `zbus::blocking` on the session bus.
- **The hand-back thread.** It reads the gate and the first snapshot before it says "ready". It
  rereads the snapshot at each focus-in it is told of. It invokes once for each request it is sent,
  from a queue the window only sends to. Each failure goes back to the window as one word.

**Tests:**
- K-L4, every OS, against a fake `Bus`;
- K-L7's hand-back half: a failing invoke, the thread ending.

### KL-W3 — the Wayland side (gate: Linux)

**Owns:** `src/fnkeys/wayland.rs` (new, `#[cfg(target_os = "linux")]`) and
`src/fnkeys/wayland_tests.rs` (new). **Hand-over:** one line
`#[cfg(target_os = "linux")] pub mod wayland;` in `src/fnkeys/mod.rs`.

**Builds:**
- **`start(display, surface, ctx, ready)`.** It starts the key thread and returns at once. The key
  thread then:
  - makes the guest connection from the window's raw handles (`Backend::from_foreign_display` on the
    Wayland display pointer, with the surface from the window handle), on its own event queue;
  - binds the registry, the inhibit manager and `wl_seat` (v7);
  - sends `inhibit_shortcuts` only once the hand-back's `ready` has come.

  The window's thread never waits on the compositor.
- **`inhibit()` and `uninhibit()`**, on the key thread.
- **The second `wl_keyboard`.**
  - It keeps records only between `enter` and `leave` of the window's surface; keys held at `enter`
    are not presses.
  - Modifiers come from the `modifiers` event's masks, with Alt = Mod1 and Meta = Mod4.
  - The keymap fd is closed unread.
- **The key thread.**
  - It waits with `EventQueue::prepare_read`, then `poll` on the display fd and a stop pipe, then
    `read`; a stop drops the guard. It never uses `blocking_dispatch`.
  - It sends records into a channel and asks the window's `egui::Context` for a repaint.
  - **Liveness:** if no record is taken 500 ms after a press, it destroys the inhibitor itself, and
    asks for it again once records are taken.
- **`end()`**, in §6.4.2's order. It writes to the stop pipe. On its way out the key thread
  destroys the inhibitor and flushes, releases the keyboard and drops the connection. The caller
  then joins the thread. Drop calls `end()` too.
- **A socket-free core.** The record and liveness state machine is split from the socket, so it
  runs against a fake queue.

**Tests:** K-L7's key-thread half (liveness, the 50 ms late record, a stop during `poll`), and the
enter/leave and modifier-mask handling, all against the fake queue.

### KL-W4 — the shell (gate: both; after spec 10's shell decision, §10)

**Owns:** `src/gui/mod.rs`.

**Builds:**
- **In the app-creation closure (`:1320-1327`)**, the Linux keys of the window. The closure checks
  §6.4.1 checks 1–3 (config, table, scripted run and test build, then the Wayland handles), with no
  I/O. It then only starts KL-W2's hand-back thread and KL-W3's key thread, passing the hand-back's
  `ready` to the key thread. It never waits on the compositor or the bus.

  `run_native` and the wgpu→glow fallback (`:1300-1341`) do not change. The macOS `Feed`
  (`:1364-1767`) is untouched.
- **`Shell` gets a `linux_keys` field.**
- **`Shell::raw_input_hook` (`:1109-1127`)** drains the records and runs `top_row::frame`. It sends
  the hand-backs, and tells the hand-back thread of a focus-in. It writes a failure into the existing
  `FkeysLine` slot (`:1716-1730`), so that `fkeys_status` shows it as it shows the macOS line. It runs
  before `FKeyInput::input_hook` (`:785-796`), which is unchanged.
- **`App::on_exit`** runs KL-W3's `end()`; `Drop` stays as a backstop next to the macOS feed's
  release (`:1085-1093`).
- **Debug lines** (`SOPEN_LOG=debug`), which K-L5 and K-L6 read:
  - `fkeys: top row <code> → F<n>`;
  - `fkeys: handed back <component>/<action>`;
  - `fkeys: inhibitor off: <reason>`.

**Tests:**
- The hook runs before the plugin: a table code becomes a plain F*n* that the `FKeyInput` rules
  then see.
- A scripted run, `fkeys = "system"` or an empty table → nothing is built, and the fakes are never
  called (K-F4's Linux clause).
- On the window's end, `end()` runs before the shell is dropped.

### KL-W5 — the headless live receipts (gate: both; the tests are `#[ignore]`d and run by hand on Linux)

**Owns:** `tests/fkeys_kwin_live.rs` (new). It starts with `#![cfg(target_os = "linux")]`, its tests
are `#[ignore]`d, and it runs only with `SOPEN_TEST_KWIN=1`.

**Builds:**
- **The private session driver**, with the isolation of `research/fkeys-linux.md` §1 restated in
  the test: `env -i`, its own HOME, XDG and runtime folders, `dbus-run-session`,
  `kwin_wayland --virtual` on its own socket, no `--replace`, `--no-lockscreen`, a refusal on the
  owner's bus or display, kill own PIDs only, and the no-owner-config-touched check.
- **A fake-input injector** (`org_kde_kwin_fake_input` from `wayland-protocols-plasma`).
- **Stand-in kglobalaccel components**, registered over the private bus.
- **A second probe window** that logs its `enter` and `leave`.
- **`dbus-monitor` capture** of the private bus.

It runs sopen's built binary under `WAYLAND_DEBUG=1` and `SOPEN_LOG=debug`, with
`SOPEN_FKEYS_TABLE=224:7`. No path of the run is printed into the repo.

**Tests:** K-L5 (a)–(e) and K-L6 (a)–(f). AGENTS.md's list of hand-run live tests names it (KL-W6).

### KL-W6 — config, README, AGENTS and docs (gate: both; after KL-W4 is on main)

**Owns:** `src/config.rs` (doc comments only), `README.md`, `AGENTS.md` and `.prd/research/keys.md`.

**`src/config.rs`:**
- `:1-8`, `:22` and `:28-37`: `fkeys` is no longer "the macOS switch". The doc now says what it does
  on each system: on Linux, the inhibitor on KDE Plasma Wayland (04 §6.4).
- No behaviour change: absent stays `Function`, and the test `kf4_fkeys_is_set_as_the_target_is`
  stands as it is.

**`README.md`:** lines `:76-77`, the comments of the `sopen config fkeys` lines, become
"macOS and KDE Plasma". Lines `:88-96` are replaced with the text under "README wording" below.

**`AGENTS.md`:**
- a row for `SOPEN_FKEYS_TABLE` in the variables table (next to `SOPEN_KEYS`), as a receipt
  variable: a local setting, not forwarded by `--target`;
- `tests/fkeys_kwin_live.rs` in the list of `#[ignore]`d hand-run tests.

**`.prd/research/keys.md` §4.2:** one line under "Recommended for Linux": reversed by
`fkeys-linux.md` (2026-11-24); the contract is 04 §6.4.

**Tests:** none new. `just check` covers the doc build.

#### README wording (replaces `README.md:88-96`; no host names)

> `fkeys` decides what the keyboard's top row does inside xtools.
>
> **macOS:** with `"function"` (the default), while an xtools window is in front sopen switches
> the Mac's top row to F1–F12 without fn — so F5 should copy — and brightness and volume need fn;
> once no xtools window is in front (or the Mac sleeps, or you switch user), the Mac's own setting
> is back, even if sopen crashed. The switch is measured to reach the keyboard driver; that a plain
> F5 then reaches xtools is not yet checked on a real keyboard (`.prd/04-file-keys.md` §6.2, K-F3).
> The setting is global, so other apps see F-keys while xtools is in front.
>
> **Linux (KDE Plasma, Wayland session):** with `"function"`, while an xtools window has the
> keyboard, KDE passes its keys to the window, and sopen turns the top row's media keys into
> F1–F12. Every other KDE shortcut — Alt+Tab, Meta, Meta+L, Print, … — is handed back to KDE and
> works as before, except that a held shortcut acts once. Nothing in the system changes, and it
> ends when the window loses the keyboard or sopen exits. **It is not on yet:** it needs the
> measured codes of the laptop keyboard's top row, and the release that adds them turns it on.
> Until then nothing changes. GNOME, sway and X11 sessions: nothing changes.
>
> `"system"` leaves everything alone on both. The F-key row's slots and Cmd/Ctrl+1…0 (F1–F10)
> work either way.
>
> **To make the top row send F1–F12 everywhere**, not only in xtools, use the keyboard's own Fn
> lock: on many laptops Fn+Esc. A kernel module option is no shortcut: the line
> `options hid_asus fnlock_default=0` in `/etc/modprobe.d/` does nothing. `hid_asus` has no such
> parameter, and the kernel logs "unknown parameter … ignored" for it. `fnlock_default` belongs to
> `asus_wmi` and is read-only once that module is loaded. Whether it reaches a detachable keyboard
> is not checked (`.prd/research/keys.md` §4.2).

### KL-W7 — the measured table (gate: both; only after the owner's K-L2)

**Owns, by hand-over:**
- the `TOP_ROW` rows in `src/fnkeys/top_row.rs`, from KL-W1;
- one test in `src/fnkeys/top_row_tests.rs` that checks the rows against K-L2's printed summary,
  checked in as test data with its date and keyboard;
- the README sentences "It is not on yet … Until then nothing changes.", from KL-W6, which become
  "On the laptop's own keyboard it is on by default; another keyboard sends F-keys already, or needs
  its own measured rows."

**Then:** the owner runs K-L8 on the release. This change turns the Linux switch on by default.

## 10. Tensions

- **Route A's pairing exists only because eframe's `run_native` hides winit's events.** With
  `run_native`, sopen sees keys only as egui events, and egui-winit drops Meta on Linux
  (`egui-winit-0.37.0/src/lib.rs:554-564`). So route A reads a second `wl_keyboard`, and the two
  copies of a press have to be paired within 50 ms (§6.4.4).
  - Spec 10 (xtools TUI-first, being written; research `research/tui-gui.md`) reworks the GUI shell.
    If its GUI backend owns the winit handler (route B, `eframe::create_native`, `research/fkeys-linux.md`
    §3.1), each `WindowEvent::KeyboardInput` is seen before eframe, with winit's modifiers, Meta
    included, and one source per key. KL-W4 then moves to route B, and the pairing rule, the second
    keyboard and the 50 ms stall are deleted from §6.4.
  - KL-W4 is therefore sequenced after spec 10's shell decision (§9). Most of the work serves both
    routes and does not wait:
    - KL-W2's hand-back;
    - KL-W1's table, its translation and its chord lookup;
    - KL-W3's guest connection and its inhibitor.
  - Two parts are route A's and go with it: KL-W1's pairing and KL-W3's second keyboard and key
    records.
- **On Linux the switch ships OFF until K-L2's measured table exists.** `fkeys` absent still means
  `"function"`, as the study's §8 has it, but an empty table holds no inhibitor (§6.4.3). The
  default turns on in the change that adds K-L2's rows (KL-W7), not by a config change.
