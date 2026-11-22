---
title: sopen — editing a streamed file
type: spec
tags: [sopen, spec, streaming, edit]
---

# 07 — Editing a streamed file

Status: in progress (2026-11-22: ED1, ED2 and ED3 merged; ED4, the hex window, in review). This
is the contract the edit units (§14) build. Until each lands, [`01-spec.md`](01-spec.md)
describes what ships.

Viewing is [`06-streaming.md`](06-streaming.md). This document reads through 06's `Source`, its
`Bytes` and its range server. Owner message 17 is quoted whole in 06's header; this is its third
paragraph, verbatim:

> 그리고 그냥 뷰어가 아니라 만약 편집할 경우는 다른데 이것은 별도로 리서치해서 처리해줘. 편집할경우는 그 편집된 부분은 전부 현재 로컬에 해당 내역이 모두 메모리 혹은 파일로 임시 저장해야하는거고 원하면 save as 로 로컬에 저장할 수 있게 해줘. (디폴트는 원격 편집 저장임)

The measurements are in [`research/streaming-edit.md`](research/streaming-edit.md) (cited as *edit*
§n). Times quoted are from 06's reference rig. Code citations name functions, at `main` 3a426b6.

## 1. The contract

- **Every edit lives here until it is saved**: in memory, and in a journal on the local disk (§3).
  - **Recovery reaches the last durable generation** (§3). A generation is scheduled at most
    250 ms after a change, and writing it takes its own time on top. The status bar shows that lag
    (`keeping…`, then `kept`), so a crash loses only edits the window had not yet shown as kept.
  - **Focus loss, closing and the start of a Save** write the journal at once, so nothing is lost
    across them. Focus loss and closing wait for it; a Save's worker writes it before anything
    else, and the window goes on drawing meanwhile.
  - The journal is offered back on the next open: Restore, Later or Discard.
  - Today a crash loses every unsaved hex and image edit (*edit* §1).
- **Save writes to the remote file. It is the default** (Cmd/Ctrl+S, F5). It keeps 01 §5: a conflict
  check before anything is written and again right before the replace, a backup, and the atomic
  rename (§4, §5).
  - The checks are best effort, as 01 §5 already says; they are not a compare-and-swap.
  - §4.5 states exactly which concurrent writes a Save can lose, and the owner is told before the
    first large Save.
- **A small edit to a huge file moves bytes, not the file.** Hex Save copies the file on the server
  (`copy-data`) into the temp, patches the changed ranges, and renames. Measured on a 24 GB remote
  file with a 4-byte patch: **6.3 s** with the file in memory (tmpfs), of which the copy was
  3.4 s, with 8 bytes of file content on the wire. On a real disk the copy alone takes about 17 s on
  the Linux laptop and 5–14 s on the Mac (X2, §4.2). Today the same Save
  costs 48 GB over the network and 48 GB of local disk: about 4.8 h at the measured 1.39 MB/s each
  way (*edit* §2.3–§2.4).
- **Save As writes the edited result to a local path** (Alt+Cmd/Ctrl+S, F7), streaming the unchanged
  ranges from the remote and applying the edits on the way. It never writes over anything (§7).
- **The edit list is unchanged.** Hex overwrites bytes and never changes the size. Image edits are
  crop, pixelize and clipboard copy / cut / paste. Media edits are PR #15's. AGENTS.md "No new edit
  features without the owner" stands: this document changes where edits live and how they are
  written, not what can be edited. The text viewer stays read-only.

## 2. What each editor keeps

| Editor | In memory | In the journal | Size changes? |
|---|---|---|---|
| Hex | the patches: offset → (the byte replaced, the byte wanted), from 06 §7.1; the undo steps (at most 100 000) | the patch list, and a hash of each edited 4 KiB page as shown | never |
| Image | the decoded image and its undo of whole images (at most 32 steps or 1 GiB), as today | the operations in order, and each pasted picture | crop, as today |
| Media (PR #15) | the timeline's clips, Mono → Stereo, the crop, In/Out | the same, in ticks | a render is a new file |

## 3. The journal

The journal and §3.1's save reconciliation are **the workstream's reading** of message 17's "메모리
혹은 파일로 임시 저장": kept in a file, so that edits survive a crash. The message does not say how.

- **Where.** One directory per editing session, created at its first edit (not at open):
  `<cache>/.edits/<host>/<abs path>.journal-<UTC stamp>-<8 hex>/`. It is owner-only: the directory
  0700, its files 0600, created exclusively, never through a link (`cache::create_unique`'s rules).
  It sits under `.edits`, outside the mirrored tree, so no fetch ever writes it or moves it aside.
  - **Its creation is made durable** before its first generation: after the `mkdir`, and after each
    parent the same call created, the parent directory is fsynced.
- **What it holds:**
  - **`gen-<n>`**: one immutable file per generation (`n` counts up from 1). It holds three things
    together, with its own SHA-256 on its last line:
    - **`head`**: the kind (`hex`, `image`, `media`), the host, the absolute path, the sopen version,
      the time created, and the **baseline** the edits are against: size, mtime, and the whole
      file's SHA-256 when known.
    - **`seen`**: the evidence Restore and Save check (Recovery below, §4.5).
      - Hex: each edited 4 KiB page's offset and the SHA-256 of its bytes as shown, taken when the
        page was first edited.
      - Image: the whole file's SHA-256, from the streamed read.
      - Media: the whole file's SHA-256 at most 256 MiB, from the baseline pass; above that, the
        SHA-256 of the first and last 1 MiB.
      - `pending` until it is known.
    - **`edits`**:
      - Hex: the `.hexpatch` text sopen already writes (`HexFile::list_text`), with the byte each run
        replaced (`0xOFF: NEW was OLD`).
      - Image: one line per operation, replayed in order on the decoded original: `crop x,y,w,h`,
        `pixelize x,y,w,h,block`, `cut x,y,w,h,fill`, `paste x,y,w,h,mode,<sha256>`,
        `text x,y,w,h,<escaped text>`.
      - Media: the clip list in ticks, stereo, the crop, In/Out.
  - **`CURRENT`**: the name of the current generation and its SHA-256, replaced atomically.
  - **`paste-<sha256>.png`** (image): each pasted picture, named by its own hash and never changed.
  - **`save`**: the save intent, present only while a Save runs or after one was cut off (§3.1).
  - **`lock`**: an advisory lock held while the session lives (what `cache::Claim` does with
    `.meta/lock`).
- **When a generation is written.** The first change after the last generation schedules the next
  one at most 250 ms later, holding every change up to then. So continuous typing schedules at most
  four a second.
  - It is also written at once, and waited for, at focus loss and at close (any path out of the
    window, Discard excepted). Before any Save sends anything, the Save's own worker writes it: a
    generation still on its way is waited for there first — never on the frame, so a slow or
    stalled disk under the journal does not stop the window; edits are refused meanwhile, as for
    the whole Save, and Esc stops a streamed Save still waiting there, nothing sent — and none
    begun before the Save writes after it starts.
  - A hex generation is a few hundred bytes.
- **How a generation is made durable**, one at a time:
  1. Each new paste: written to `paste-<sha256>.png.tmp`, fsynced, renamed to its own new name, and
     the directory fsynced.
  2. `gen-<n>.tmp` written and fsynced, renamed to `gen-<n>` (a new name, never replaced), and the
     directory fsynced.
  3. `CURRENT.tmp` naming `gen-<n>` and its hash, fsynced, renamed over `CURRENT`, and the directory
     fsynced.
  4. Generations older than `n`, and pastes no kept generation names, are removed. The one exception
     is the generation a `save` names (§3.1), which stays until that save is reconciled.

  `head`, `seen` and `edits` change only together, as one new generation; `CURRENT` moves from one
  whole generation to the next. So edits and their evidence always come from the same generation.
  That includes the rebase after a Save, which writes one new generation with the new baseline, the
  new evidence and the edits that remain.
  - A crash at any step leaves `CURRENT` naming the previous generation or the new one, each whole.
  - On recovery, a `CURRENT` that is missing, or names a generation that is missing or fails its
    hash, falls back to the newest generation whose hash verifies, with a note.
  - A generation that names a paste that is missing or fails its hash is not applied: the journal is
    kept and named, with the reason.
- **The recovery point is the last durable generation.** A generation is scheduled at most 250 ms
  after a change. Writing it (pastes, fsyncs) takes its own time on top, which the status bar shows:
  - `unsaved — keeping…` until step 3's directory fsync for the generation holding the latest edit
    is done;
  - then `unsaved — kept at <journal>` (the path `~`-shortened, whole on hover);
  - `unsaved — NOT kept on disk (<why>)` in the error colour when a generation cannot be written. The
    close prompt still guards the edits.
  - The hex window puts this line at the status bar's right end, after what the window last said
    (which is cut with "…" first). Where the bar has no room for the path in half its width, the
    line is `unsaved — kept`, the path whole on hover either way.
- **When it is removed.** When the window closes with nothing unsaved: the edits are in the remote
  file, in a Save copy, or in a Save As. Discard also removes it.
- **When it is kept.** On any other ending (a crash, a kill, a power loss). The lock is then free.
  - A scripted run (`SOPEN_SCREENSHOT`, `SOPEN_AUTOSAVE`) keeps nothing when it ends: its edits
    are its `SOPEN_STATE`'s, not the owner's, and would otherwise come back as a recovery row. The
    one exception is a Save whose request got no reply: its record is kept, and its place said,
    since it is what tells later whether that Save landed.

**Recovery.** On open, sopen looks for `<abs path>.journal-*` directories of the same host and path
whose lock it can take. A lock found taken is tried again for up to 500 ms before the journal counts
as another live session's: the lock belongs to the open file, and a child that another thread is
starting (ssh, ffmpeg) holds a copy of every open descriptor from its fork until its exec, so the
lock of a journal this process has just let go can look taken for that moment. The viewers' open
path does the retrying (ED4–ED6); `orphans` itself tries once. The hex window lists them at its
first pass as a viewer, in the background — never as a browser preview, which must not hold
another session's journals — and tries a taken lock again every 50 ms for up to 500 ms. It waits
only for this file's own journals: a folder named exactly `<file>.journal-<UTC stamp>-<8 hex>`
whose generation, when it can be read, is of this host and path — not another file's whose name
starts the same, one being removed, or one written for another path that maps to the same place. Its
`Origin` connects for the listing only when there is a journal to check (reconcile, Restore's
reads): that is its first use (06 §2). Each one gets a row:

> unsaved edits from 2026-11-20 11:42 (4 bytes changed) kept at ~/…/model.bin.journal-… — [Restore] [Later] [Discard]

The row's actions are buttons, not plain-letter keys, because in the hex window a letter types a
byte.

- **Restore checks the file's content, never only its metadata.** A same-size write with the mtime
  put back is part of the threat (*edit* §2.7 C3).
  - **Hex.** sopen reads the `seen` pages (ranged reads) and compares their hashes, **also when
    `(size, mtime)` equals `head`**. Each patch's replaced byte is then the byte the page holds now.
    - All match: the edits are applied. If the metadata differed, the note says `the file changed
      elsewhere since; the bytes under your edits did not`.
    - Any differs: nothing is applied, and the journal stays, its generation readable as text.
  - **Image.** The image is read whole to show it anyway (06 §7.3). Its SHA-256 must equal `seen`'s,
    and then the operations are replayed. A different hash applies nothing and keeps the journal.
  - **Media at most 256 MiB.** A baseline pass (§4.5) hashes the file, and that hash must equal
    `seen`'s.
  - **Media above 256 MiB.** `seen` holds the SHA-256 of the first and last 1 MiB (a container's
    header and index sit there) with `(size, mtime)`. All must match. The note says `checked size,
    modification time and the first and last MiB, not the whole content`.
  - **Missing evidence.** A crash before `seen` was written, such as during the baseline pass, leaves
    `seen: pending`. Restore then asks:

    > these edits were made before the file could be checked — [Restore anyway] [Later] [Discard]

    Nothing is applied without that answer.
  - **Restored edits are unsaved edits against the file as now opened.** The window's baseline is
    the current open's fingerprint, its `seen` pages are taken again, and a later Save runs every
    check of §4 against that baseline.
- **Restore in a window that has edits of its own** is refused (`undo or save them first`): one
  window keeps one journal. The row stays, its journal untouched; once the window's own edits
  are undone or saved, Restore goes ahead.
- **Restored bytes land as their pages arrive** (over a stream, as a typed byte needs its page).
  Until all are in, the journal Restore took over is left exactly as it is — no generation holds a
  part of them, closing asks, and a window that goes anyway leaves it for the next open — and no
  Save starts. The status row says `putting back N bytes…`, then `restored N bytes`.
- **A journal that holds no edit** (its edits undone before the end) is removed when it is listed:
  there is nothing to offer — unless it still holds a question about an interrupted save's file
  (§3.1): then its row says `an interrupted save's record is kept at <journal>` and asks it.
- **Later** leaves the journal for the next open.
- **Discard** removes it.
- A journal of **another kind** (the same file opened as hex earlier, now as an image) is named in
  one row and never applied.

### 3.1 Save intent: linking a remote commit to the journal

A Save can be cut off before its outcome is recorded: sopen killed, the reply lost. So every Save,
of every kind and route, records what it is doing in the journal's `save` file before it sends
anything that changes the remote. Each phase is written durably, as `CURRENT` is (§3, step 3),
before the request that moves the Save to it.

**What the intent always holds:**
- the **saved generation**: the name and hash of `gen-<m>`, the immutable generation whose edits this
  Save writes. It is kept until the Save is reconciled. **No edit happens while a Save runs**: hex
  and image, like PR #15's media window, take no edits and no undo or redo until it ends (§10). So
  `gen-<m>` stays the journal's last generation for the whole Save;
- the kind, the route, and the target;
- the **pre-save evidence**: `gen-<m>`'s baseline and `seen`;
- the **post-save evidence**, once it is known (below).

**Phases by route:**

| Route | Phases, in order |
|---|---|
| A temp, then a rename: hex server copy (§4.2), hex streamed rewrite (§4.3), image and media upload (`write_atomic`, §8, §9) | `naming` (the temp's name, before the exclusive create) → `created` (after the create's reply: the temp's stat at creation; a hex Save records it again once the temp has its owner and mode, before it is filled) → `staged` (after the last write, the fsync and the check right before the replace, §4.2 step 9: the temp's stat and the post-save evidence) → `renaming` (just before the rename, so `staged` and `renaming` are recorded back to back) → removed once the outcome is recorded |
| Write in place (§4.4) | `writing` (before the first write: each run's offset, old bytes and new bytes; the undo file is already durable) → removed once the fsync's reply and the `lstat` are recorded |

**Post-save evidence by kind.** There are two kinds of evidence, and they are kept apart:
- **Reconciliation evidence** decides on a reopen whether a cut-off Save landed. It may be cheap to
  check.
- **The next baseline** is what the window's next Save, or a later Restore, compares against: §4.5 and
  §3's Recovery need it whole where they check whole.

| Kind | Reconciliation evidence (did it land?) | Next baseline and Restore evidence |
|---|---|---|
| hex, temp routes | the target's `lstat` equals the staged temp's stat; each edited page's new SHA-256; at most 256 MiB, the new whole SHA-256 (computed while reading, §4.2 step 6) | the staged stat; the edited pages' new hashes; at most 256 MiB, the new whole SHA-256 |
| image | the target's `lstat` equals the staged temp's stat; at most 256 MiB, the output's whole SHA-256; above that, its size and the SHA-256 of its first and last 1 MiB | the staged stat and the encoded output's **whole SHA-256, at every size**. The output is a local file sopen made, so its hash is always known, and image Restore checks a whole hash (§3) |
| media | the target's `lstat` equals the staged temp's stat; at most 256 MiB, the output's whole SHA-256; above that, its size and the SHA-256 of its first and last 1 MiB | the staged stat; at most 256 MiB, the whole SHA-256; above that, the first and last 1 MiB, as media Restore checks (§3) |
| write in place | each run's bytes as recorded in `writing`, old and new, read back run by run (below) | the target's new `(size, mtime)`; the edited pages' new hashes |

**On the next open,** a journal whose `save` file is still there is reconciled before Restore is
offered:

- **Landed** (the target matches the reconciliation evidence). No edit happened while the Save ran,
  for any kind, so every edit in the journal is `gen-<m>`'s and is now in the file. Nothing is left
  to rebase. The journal is retired, and the row says `the save at 11:42 landed (found on reopen)`.
  For hex, the undo file was written before the patch (§5.1) and stays.
  - In a live session the same holds at once: the window's next baseline is the "next baseline"
    column above, and the journal goes when nothing is unsaved.
  - **A Save of this session that got no reply** leaves its record. The next Save reconciles it from
    fresh reads before it sends anything. Landed: `… landed (found now): nothing was written
    again`, and the window takes the bytes it wrote as the file's. Not landed: its provable temp is
    removed, the record ends, and the Save goes on. Anything else: that Save is refused with the
    line, naming what to check.
- **Not landed** (the target still matches the pre-save evidence). The edits stay, offered by
  Restore. A temp is removed only under the ownership rule below.
- **Write in place, classified run by run.** Each run recorded in `writing` is read back by its own
  offset and length, and compared with its recorded old and new bytes. A run is judged by its own
  bytes, so two runs that share one 4 KiB page are told apart.
  - Every run new: **landed**, as above.
  - Every run old: **not landed**.
  - Some runs new and some old: **in part**. Nothing is removed or written. The row says `<file>
    holds some of your changes`, names the undo file, and offers Load as edits (§5.1) and Restore.
  - Any run neither old nor new: **unknown**, below.
- **Unknown:** anything else. Nothing is removed, and the row names the target and the temp (`a save
  was cut off; check host:/abs and host:/…/.model.bin.sopen-…`). Restore stays available under §3's
  checks.
  - **Restore of a journal whose Save is unknown or in part** takes the edits over and ends the
    record: the row has named what to check, and the edits are the window's from then on.
- **Not landed, with a temp of uncertain ownership:** the record stays: it holds the question
  about the temp (below), which the journal's row asks under its own line with **Remove it** and
  **Keep it**. Either answer ends the record — Remove it once the file is gone, Keep it leaving
  the file where it is, never asked about again. **Later** leaves the question for the next open.
  - **Remove it removes only the file the question was about.** The question holds the file's
    attributes as they were when it was asked (its `lstat`: a regular file, size, modification
    time, owner and mode). Before removing, the record must still name that temp, and it is
    reconciled again from fresh reads: a temp it proves the Save's is removed as a reconcile
    removes one. A question about a temp the record proved the Save's (its removal failed)
    removes it only while that proof — the recorded stat and, from `staged` on, the contents'
    evidence — still holds. Otherwise the file there now must have every attribute the question
    saw. Anything else — another file under the name, a file that cannot be looked at, a
    question that could not look at its file — removes nothing and keeps the question and the
    record: `<host>:<path> is not the file the question was about (…) — not removed`. A file of
    uncertain ownership put there since with every attribute equal is not told apart (SFTP has
    no inode). The removals sopen makes by itself (a temp provably the Save's, at open or at the
    next Save's start) are the reconcile's own, from its fresh `lstat` against the recorded stat
    and the contents' evidence.
  The row's Restore and Discard wait for the answer, since both would end the record. (A temp of
  uncertain ownership beside a Save that *landed* has no record to keep the question: it is asked
  on that open only.)
  - **The same in a live window.** A Save of this session cut off that way is reconciled by the
    next Save's start: nothing is sent, the record stays, and the window asks the question in
    its own row with Remove it and Keep it. Save waits for the answer. A window closed with the
    question open keeps the record for the next open — without the edits, when they were
    discarded or nothing is unsaved; one closed while an answer is on its way waits for it
    first.
- **Not landed, with a temp provably the Save's that cannot be removed** (the server refuses the
  removal): the record stays too, and the same question is asked — `the interrupted save's
  temporary file <host>:<path> could not be removed (<why>) — remove it?`. Remove it tries again
  (a failure keeps the question); Keep it ends the record. The next open, or the next Save, first
  tries the removal again by itself, since the temp is provably the Save's.

**Owning a temp.** Only the exclusive create's reply proves that sopen made a temp: `SSH_FXP_HANDLE`.
- An exclusive create on a name that already exists gets `SSH_FX_FAILURE` and leaves that file
  untouched (verified, scratch, OpenSSH 10.3p1). So in phase `naming` the temp's name is recorded,
  but it is never assumed to be sopen's. A crash there leaves it named in the row, never removed.
- SFTP v3 attributes carry no inode (verified: the server returned size, owner, permissions and
  times, nothing more). So a temp's identity is checked by what can be compared.
  - **On reconcile**, a temp is removed only if the phase reached `created`, its `lstat` equals the
    recorded stat exactly (a regular file of that size, mtime, owner and mode), and, from `staged`
    on, its post-save evidence matches.
    - **Anything else is kept, named, and offered to the user for removal**: `a file at the
      interrupted save's temporary path <path> (N bytes); sopen could not verify it is its own
      partial copy, so it was not removed — remove it?`. That is a temp a Save was cut off
      filling, one whose mode had just been set, and one already given a set-id target's full
      mode — or another file that took the name since. The words claim no ownership, and the
      prompt the hex window shows (ED4) carries the same uncertainty.
    - **Why not less.** A random name, an owner, a mode and a size below the target's do not
      prove a file is sopen's: any number of files share them, and after the process has died
      another file can take the name. Removing on those would delete someone else's file.
    - **Best effort still.** SFTP carries no inode, so a file equal to the recorded stat in every
      attribute, put under the name since, is not told apart (in `created`, an empty one).
    - **What `created` records.** The temp's stat at creation, and again once the temp has its
      owner and mode (§4.2 step 4). So a Save cut off between that and its first copy request
      leaves a temp that still matches, and it is removed.
  - **During a live Save**, sopen compares `fstat(handle)` with `lstat(path)` right before removing
    its own temp; a difference keeps it and names it. A swap of the name between that check and the
    remove cannot be seen, since SFTP has no remove by handle. That is a stated residual.
- A temp whose ownership is uncertain is always kept and named, never removed.

## 4. Save to the remote (the default)

### 4.1 The route, decided before anything is written

| The server offers | Free space in the target's folder (`statvfs`) | Hex Save | Hex Save copy |
|---|---|---|---|
| `posix-rename` and `copy-data` | at least size + 1 GiB, or no `statvfs` | **server copy** (§4.2) | a server copy published as `<stem>.edited<ext>` by hard link (`write_new_sibling`'s rule, never over anything) |
| `posix-rename`, no `copy-data` | at least size + 1 GiB | up to 256 MiB: **streamed rewrite** (§4.3). Above: **refused before any transfer**, with the cost stated (below) | the same, as a new sibling |
| `posix-rename` | less than size + 1 GiB | refused: no atomic route fits (below) | refused the same way |
| no `posix-rename` | — | Save is off, as today (`CANNOT_REPLACE`) | as above |

- **The refusal above 256 MiB without `copy-data`** names the cost: `rewriting 24.0 GB through this
  machine moves 48.0 GB (about 3 h 40 min at the current 3.6 MB/s); this server has no copy-data
  (OpenSSH before 9.0)`. Its row offers **Write in place…** (§4.4) and **Save As…** (§7).
- **The refusal for room** names both numbers: `needs 24.0 GB free in /data, has 3.1 GB`. It offers
  the same two.
- **Image Save** replaces every byte with a re-encode. So it uploads the encoded file through
  `write_atomic`, as today, with the content check of §4.5 (§8).
- **Media Save** renders a new file and uploads it the same way (§9).
- **A local file keeps 01 §6a's Save**: one pass writing the full local backup and the staged copy,
  `write_atomic`, and the re-bind. Message 17 is about streamed files, and a local file has no
  Source to reload. The journal (§3), Save As (§7) and the close prompt (§10) apply to it as to a
  remote file.
  - `LocalFs::patcher()` exists for the tests and for parity with SFTP (ED1's algorithms run on
    both). ED4 never routes a local file through it.

### 4.2 Server copy, then patch, then rename

The save runs on the viewer's `Origin` channel (06 §6), and every request has a timeout. Each step is
measured in *edit* §2.4 and §2.7.

**Every read a Save makes comes from the remote, fresh, never from the view's block cache.** The cache
serves the screen, and a cached block may predate a change that §4.5 must see. This applies to the
check, the undo file, the streamed rewrite and Save As.

1. **Check the target.** `lstat` it: a regular file whose `(size, mtime)` equals the baseline, with
   every attribute present. Otherwise refuse, having created nothing.
2. **Check the room.** `statvfs` the folder, and refuse early when there is no room.
3. **Open the source.** Open it for reading and `fstat` the handle against the baseline. The copy
   reads from this handle, so a rename-over after this point is caught at step 9.
4. **Create the temp.** The journal's `save` file first records phase `naming`, with the temp's name
   and the saved generation (§3.1). Then sopen creates `.<name>.sopen-<random>` exclusively. On the
   create's reply it records phase `created` and the temp's stat. Through the handle it gives the
   temp the target's owner, then its mode without set-id bits (`create_from`'s rule). It then
   `fstat`s the temp and records that stat in `created`, before the first copy request: a Save
   cut off before that request leaves a temp that equals the record and is removed on reopen. Once
   the copy has started the temp no longer equals it, and is kept and named (§3.1).
5. **Copy on the server.** `copy-data` from the source to the temp, in requests sized by time. There
   is **one `copy-data` loop** in sopen: the browser's S1-C builds it in `src/remote/sftp/ops.rs`
   for copying files, and ED1 gives it a pace. File copies keep their 8 MiB steps through the same
   loop (04 §5.1: progress and Esc per step). Only a Save is paced by time, in requests of about
   1 s of copying:
   - The first request is 64 MiB. Each next one is sized to take about 1 s at the last one's rate,
     clamped to 16 MiB–1 GiB. A slow server disk then never turns one request into a 30 s
     `META_TIMEOUT`, which would be an unknown outcome.
   - Measured (X2, 2026-11-21: 4 GiB and 24 GiB files of random bytes, source evicted from the page
     cache): the Linux laptop (btrfs on NVMe) copies at 2.2 GB/s for 4 GiB and 1.5–1.6 GB/s across
     24 GiB; the Mac (APFS, internal SSD) at 1.8–5.4 GB/s, usually about 5. With the source cached,
     3.7 and 5.4 GB/s. A 64 MiB request takes 18–70 ms on the laptop (one network round trip
     included) and usually 11–19 ms on the Mac (at most 294 ms); every measured run of the rule
     reached the 1 GiB cap on its second request (their first requests took 16–25 ms); at the slow
     end of the 64 MiB range (70 ms) the rule sizes the second request at 914 MiB. A 1 GiB request
     typically takes 0.4–0.5 s on the laptop and 0.2–0.3 s on the Mac; the worst seen was 1.73 s on the laptop, when its disk slowed to about 0.6 GiB/s (the
     rule then cut the next requests to 370–950 MiB), and 1.07 s on the Mac. Two or four requests outstanding gain only the network round trip,
     which 1 GiB requests already amortise, so the loop keeps one outstanding. A slow disk was not
     measured; the 16 MiB floor and the time-sized rule are for it.
   - Every 2 GiB copied, sopen sends `fsync@openssh.com` on the temp, so the final fsync never meets
     a whole file of dirty pages. sopen's fsync bound is the 60 s `STALL_TIMEOUT`. Measured (X2):
     10–46 ms per fsync on the laptop's disk, up to 0.41 s late in a 24 GiB copy; 1–7 ms on the Mac,
     where `fsync` does not ask the drive to flush.
   - A server whose dirty-page limit is large can answer requests at memory speed and leave the disk
     time to the fsync. So the fsync's time counts in the rate the next request is sized from, and
     sopen also sends an fsync after 10 s of copying since the last one, whichever of 2 GiB or 10 s
     comes first. This guard is the workstream's decision; X2 did not measure such a server.
   - Progress is `saving — copying on <host> 37 % (8.9 of 24.0 GB)`. Cancel is checked between
     requests; the temp is then removed and the target is untouched. A request already sent finishes
     on the server first (SFTP has no cancel), and time-sized requests bound that wait to about 1 s.
6. **Check the content** (§4.5). Read back what the check needs from the temp, which is now the
   target as it was at the copy, and compare. A difference is refused and the temp is removed. At
   most 256 MiB, the same read also hashes the bytes with the patches applied: that is the
   post-save evidence for §3.1.
7. **Write the backup** (§5.1). The bytes each run will replace, from those same reads, go into the
   local undo file. It is fsynced **before** the temp is patched, and its path is printed, as
   today's `backup of … at …` is. (In the streamed rewrite the pass has already written the
   patched bytes into the temp; §4.3 says when the undo file is durable there.)
8. **Patch.** Write each run at its offset (positional writes). Restore the full mode, send
   `fsync@openssh.com`, and `fstat` the temp. Its size must equal the size as opened, and its owner
   and mode must be as wanted (`prepare_swap`'s checks).
   - **A server without `fsync@openssh.com`** cannot flush. A Save never reports a flush it did not
     make: it goes ahead, and says so before it starts and in its result — `this server cannot
     flush to disk; the save is not guaranteed to survive a power cut on <host>`. The copy's
     flushes on the way (step 5) are then not sent either.
9. **Check again, right before the replace.** `fstat` the source handle and `lstat` the target, both
   against the baseline. Any difference is refused and the temp is removed. The journal's `save`
   file then records phase `staged` (the temp's stat from step 8, and the post-save evidence), then
   `renaming` (§3.1).
10. **Replace.** `posix-rename` the temp over the target, then `lstat` it (today's `saved, but the
    remote changed right after`). The next baseline is the temp's stat from step 8. The journal is
    rebased as one new generation: the new baseline, the new `seen` taken from the bytes now saved,
    and the patches that remain. Then the `save` file is removed.
11. **Reload the view.** The window reloads its Source as 06 §5 says for sopen's own writes. Each
    patch's replaced byte becomes the byte saved there, so the saved patches drop out without
    reading anything.

**Failures are 01 §5's, unchanged.** A refused or failed Save keeps the journal and names it. A
request with no reply is an unknown outcome: "check <path>", never "nothing was written". A temp is
removed only when this save created it.

**The `Origin` may not be connected yet.** A viewer's `Origin` connects at its first use (06 §2), and
that can be this Save. If sshd refuses the session (`MaxSessions`, 06 §6):
- the Save fails before step 1, with 06 §6's line;
- the edits stay in the window and the journal, and nothing is sent to the remote;
- the next Save tries to connect again.

### 4.3 Streamed rewrite (no `copy-data`, at most 256 MiB)

The same steps, except that steps 3–6 become one pass through this machine.

- The pass reads the whole target fresh, on the `Origin`, through the handle step 3 opened, with up
  to 4 MiB in flight (06 §2). It never uses the view's cache: a cached block could hide a same-size,
  same-mtime change.
- It applies the patches on the way, writes the temp, and hashes what it read.
- That hash must equal the baseline (§4.5) before the rename.
- **Step 7 here.** The pass writes the patched bytes into the temp as it reads, so the undo file
  cannot come before them. It is made durable after the pass and before the temp's fsync, the
  check before the replace and the rename: before anything can replace the target. In the server
  copy it is durable before the patch.
- The upload is at most the file's size.

### 4.4 Write in place… (only from an explicit dialog)

Write in place is one of **the workstream's decisions told to the owner**
([`streaming/ssot.md`](streaming/ssot.md)). It is offered only in the refusal rows of §4.1, never as a
fallback and never on a key:

> Write the 4 changed bytes into host:/abs in place? **This is not atomic.** If the connection drops
> while writing, the file may hold only some of the changes. Programs reading it meanwhile see the
> changes as they land, and every hard link to it changes too. The original bytes are kept at
> <undo file>. — Write in place · Cancel

The sequence:

1. Steps 1–3 of §4.2, opening the target read-write without truncating it.
2. Read each edited page from the target and check it (§4.5).
3. Write the undo file and fsync it.
4. Record the `save` intent in phase `writing` (§3.1): each run's offset, old bytes and new bytes,
   and the saved generation.
5. Write the runs, then fsync.
6. `lstat` the target. Its new `(size, mtime)` is the baseline. The journal is rebased as one new
   generation, and the `save` file is removed.

A write or fsync with no reply is an unknown outcome **on the target itself**:
`<file> may hold some of your changes — check it; the original bytes are at <undo file>`.

### 4.5 The content check, and what it cannot do

- **Files of at most 256 MiB keep 01 §5's whole-content hash.** The baseline is the SHA-256 of the
  file as opened.
  - The image window has it from its streamed read (06 §7.3).
  - The hex and media windows take it at the first edit, by a **baseline pass**: a Bulk reader hashes
    the whole file in the background, at most 256 MiB, with `checking the file for Save — 37 %` on
    the status row. A Save before the pass ends waits for it.
    - In the hex window the reader is on the window's own stream, not the `Origin`, so the pass
      neither connects the `Origin` nor holds what a Save needs. Its blocks are the stream's,
      cached ones included: a change the cache hides is still refused, by the whole-content check
      of the copy read back (§4.2 step 6).
    - A pass that failed (the file changed since it was opened) is not started again by the
      window; a Save, Save copy or Save As that needs it asks once more. A reload's version gets a
      pass of its own. Esc stops a Save that waits for the pass, as it stops one copying.
  - The check reads the whole temp back fresh (or, in §4.3, hashes the pass) and compares. So a
    change made before the copy, anywhere, is refused, as today.
  - **The baseline must agree with what the window showed.** The pass also keeps an 8-byte hash
    (truncated SHA-256) of every 4 KiB page: at most 512 KiB for 256 MiB.
    - Every edited page's `seen` must equal that page's hash in the pass, whether the page was first
      edited before the pass or after it.
    - A mismatch means the file changed under a page the window showed, after it was shown, with
      size and mtime unchanged. The baseline is then not accepted: the status row says `the file
      changed under your edits — Cmd/Ctrl+R reloads`, and Save is refused.
    - Without this, the pass could read the other writer's bytes, and Save would compare them with
      themselves.
- **Every Save, whatever the size, also reads each edited page fresh and compares it with `seen`.**
  For files of at most 256 MiB that check runs alongside the whole-content hash.
- **Larger files are checked by size, mtime and the pages under the edits** (the page check above).
  Today a file above 256 MiB is checked by size and mtime only.
  - Size and mtime are checked four times: at open, before the copy, after the copy, before the
    rename.
  - Each edited 4 KiB page is read back from the temp and compared with `seen`, which also catches a
    same-size write with the mtime put back over the edited bytes (*edit* §2.7 C3).
  - The result line says what was checked: `checked size, modification time and the 8 KiB under
    your edits, not the whole content`.
- **These checks are best effort. They are not a compare-and-swap.** SFTP has no lock and no
  conditional rename. What a Save keeps of another writer's change depends on when it was made:

  | Another writer's change | Kept in the saved file? |
  |---|---|
  | made before the copy read that region, **captured by the copy**, size and mtime put back, outside the edited pages (file above 256 MiB) | **yes**: the patch lands on the file as copied, where today's whole rewrite would have overwritten it |
  | made before the copy, anywhere (file at most 256 MiB) | no Save: the whole-content check refuses |
  | made to a region **after the copy read it**, with the mtime put back | **no**: the temp holds the bytes from before, and the rename replaces the writer's change, unseen |
  | made **between the last check (step 9) and the rename**, even with a normal mtime | **no**: the rename replaces it, unseen (01 §5: "a write racing the final rename is not detected") |

- **The owner sees this before it matters.** The first Save of a session on a file above 256 MiB
  asks once (Save only: a Save copy writes no file anyone else reads, and Write in place has its own
  dialog):

  > Save to host:/abs (24.0 GB)? sopen checks its size, modification time and the pages under your
  > edits. A program writing to the file while it saves can lose that write. — Save (Enter) · Cancel

  Later Saves in the session do not ask. The result line keeps the "checked …" sentence.

## 5. Backups

### 5.1 Hex: a local undo file

- A hex edit only overwrites, so writing the replaced bytes back over the saved file reverts this
  session's overwrites. sopen keeps those bytes: `<cache>/.backup/<host>/<abs path>.<UTC stamp>.undo.txt` (0600, a
  new file). It has a header naming the source and its size and mtime as opened, then one
  `0xOFF: OLD` line per run. Writing it back was measured exact: the original SHA-256 came back,
  twice (*edit* §2.4–§2.5).
- **What it reverts, and what it does not.** It reverts the offsets this session overwrote, and
  only those. Where another writer's change was kept by a Save (§4.5's table, above 256 MiB), the
  undo file leaves that change as it is. So under concurrency it reverts sopen's overwrites; it does
  not rebuild the file as it was when opened.
- **One undo file per session, holding the file as opened.** Each later Save in the session adds the
  offsets it changes for the first time. An offset already in the file keeps the byte recorded
  first. The file is rewritten atomically (a temp, then a rename), never in place.
- **Restoring is done in sopen** (Load as edits: one of **the workstream's decisions told to the
  owner**, [`streaming/ssot.md`](streaming/ssot.md)). The hex window opened on a file lists that
  file's undo files in its recovery rows (§3), newest first:

  > bytes replaced by the save at 2026-11-20 11:42 (4 bytes) kept at ~/…/model.bin.….undo.txt — [Load as edits] [Later]

  **Load as edits** (a button, as in §3) makes those bytes the window's patches: unsaved edits, marked as any are. Save
  then writes them by §4, with every check. Nothing writes an undo file back without a window and a
  Save.

### 5.2 Image: the whole local backup, as today

The encoded original is in memory (06 §7.3), and Save writes it to `<cache>/.backup/…` before the
first overwrite, as today (`ImageFile::ensure_backup`).

### 5.3 Media: a hidden hard link on the remote

The remote hard link is one of **the workstream's decisions told to the owner**
([`streaming/ssot.md`](streaming/ssot.md)).

- A media Save replaces every byte, so no undo file exists for it, and a local backup of a remote
  24 GB video would be a 24 GB download.
- **The order.** The first Save of the session renders and stages its output. It then links the
  remote original to `.<name>.sopen-backup-<UTC stamp>` in the same folder, through the existing
  `RemoteFs::link_new` (`hardlink@openssh.com`, never over anything). Only then does it call
  `write_atomic`, unchanged.
  - Nothing is copied, and the original's inode survives under that name. A link changes neither
    the target's size nor its mtime, so `write_atomic`'s checks still see the file as opened.
  - **The link is the original's data, not a copy of it.** It is the same inode. So a program that
    still has the original open for writing, or another hard link to it, can change it after the
    Save. Verified (scratch, OpenSSH 10.3p1 on APFS): after the link and a `posix-rename` over the
    path, a write through a handle opened earlier changed the backup name's bytes.
  - It is not replaced by a server-side copy, which would cost the file's size on the server's disk
    at every first Save; the hard link is the decided backup.
  - Later Saves of the session make no link: the first one already keeps the file as opened.
- **When the Save does not land:**
  - Refused or failed: the link is removed. Only that name is removed, and a link that cannot be
    removed is named.
  - An unknown outcome (01 §5): the link is kept and named.
- **The owner is told, before and after.**
  - Before: while the first Save renders, its status line says `the original will be kept on <host>
    as a hard link — still changeable through other links or programs that have it open`.
  - After, when it lands, the result line names the link and its size: `the original is kept on
    <host> as /data/.talk.mp4.sopen-backup-20261120T114200Z (24.0 GB, a hard link: other links or
    open handles can still change it); delete it when no longer needed`.
- **Where the server has no hard links**, media Save of a remote file is off, with that reason.
  Save As and Export are offered.
- **A local media file** keeps PR #15's local backup copy.

## 6. Save copy (a remote sibling)

Shift+Cmd/Ctrl+S, unchanged in meaning: a new file `<stem>.edited<ext>` next to the original,
published by hard link from a temp, never over anything. Hex makes it by the server copy (§4.2 steps
1–8, without the rename), or by the streamed rewrite where there is no `copy-data`, under the same
size rule. Image and media upload their output, as today.

## 7. Save As (local)

- **Keys.** Alt+Cmd/Ctrl+S, and **F7 "SaveAs"** in the hex, image and media windows' F-key rows
  (`chrome::SAVE_AS_KEY`). The text viewer is read-only and gets no Save As.
  - The key and the chord are the windows' to add as ED4, ED5 and ED6 wire Save As, each in its
    own change. That change adds F7 to its row and its help line, and calls `save_as::pressed`
    before its Cmd/Ctrl+S checks. F7 stays unused in every viewer row until then, so no slot
    names a key that does nothing.
  - The chord is Alt with Cmd/Ctrl and nothing else; F7 is F7 alone, and not while a text field
    has the typing. egui's own shortcut check ignores an extra Alt, so a window that has not
    wired Save As takes Alt+Cmd/Ctrl+S as its Cmd/Ctrl+S.
  - The hex window's grid holds the keyboard, which egui counts as a widget wanting the keys, so
    `save_as::pressed` leaves F7 there: the hex window takes F7 alone itself, as it takes F5, and
    not while the Go to field has the typing. Its toolbar has a Save As… button too.
- **The modal** (`gui::save_as`, ED3; each window opens it from its Save path, ED4–ED6). One path
  field, prefilled with `<download folder>/<stem>.edited<ext>`; the next free `-N` name when that
  one is taken (`binary::suggest`, `binary::next_free`). A name already in a counted series —
  `<stem>-N<ext>` beside its `<stem><ext>` — counts on in that series: `model.edited-1.bin` taken
  gives `model.edited-2.bin`, never `model.edited-1-1.bin`; a number of the name's own
  (`report-2024.pdf`, nothing at `report.pdf`) stays. It looks at `-1` to `-9999`; with every one
  of them taken it suggests nothing (the field keeps the name, and says `every name from
  model.edited-1.bin to model.edited-9999.bin is taken in <folder>`), and a Save to a taken name
  there is refused with those words. A Save copy's sibling names start from
  `<stem>.edited<ext>` the same way (`remote::candidate`). A folder typed in the field gets that
  name inside it. Browse… opens the system's save dialog (`binary::NativeChooser`). It shows the size and
  the time expected at the rate the viewer is getting: `24.0 GB — about 1 h 50 min at 3.6 MB/s`. A
  media render's size is its estimate, shown as `24.0 GB (estimated) — …`. With no measured rate (a
  local file) it shows the size alone.
  - The rate is the stream's own measure, `Source::rate`: the admission's EWMA of read replies over
    about the last second (06 §4), `None` until a reply has come. §4.1's cost line is said at it
    too.
- **How it writes:**
  1. It refuses early when the chosen folder has less free space than the output (its estimate, for
     a render), by the folder's `statvfs`: `needs 24.0 GB free in ~/Downloads, has 3.1 GB; nothing
     was written`.
     - Nothing is made before this check.
     - The dead partials that killed saves left in the folder are removed first (`sweep_dead`, as
       `save_new` does), so they never count against the room.
     - The folder is measured by its path's bytes, a name that is not UTF-8 included.
     - A folder whose free space cannot be read is not refused here; the save says what is wrong
       with it.
  2. It writes through `binary::save_new` unchanged (`save_new_deciding`, the same routine with the
     cancel's decision): a private staging folder, `part` created exclusively, and a hard-link
     publish that fails if anything is at the name. It never writes over anything, nor through a
     link.
     - A name taken at the start, or by another program during the transfer, is refused, and the
       field then holds the next free `-N`. The transferred `part` was removed with the staging
       folder, so Save again transfers again.
     - The staging lock records the output's size for `sweep_dead` only when it is exact (hex, an
       encoded image), never a render's estimate.
  3. The fill applies the edits on the way.
     - Hex: it reads the source fresh on the `Origin` (never the view's cache, §4.2) through a handle
       opened at the start, with up to 4 MiB in flight, and applies the patches.
     - Image: the encoder's output.
     - Media: ffmpeg rendering into `part`, with an explicit `-f <muxer>` since `part` has no
       extension.
  4. After the transfer (hex) the handle's `(size, mtime)` must still equal the baseline. The edited
     pages streamed must equal `seen`. At most 256 MiB, the hash of the source bytes read must equal
     the baseline. Otherwise nothing is saved and `part` is removed, with the reason (01 §6a: "a
     download whose source changes meanwhile is not saved").
     - For media, a response cut short during the render fails it (06 §7.4: ffmpeg exits 0 even
       then).
     - The fill (step 3) and this check are the window's producer (`save_as::Producer`). A refusal
       says its reason, then `; nothing was saved`.
- **Progress, cancel, result.** Progress shows the bytes and the time left: `37 % (8.9 of 24.0 GB)
  — about 1 h 10 min left`, at the transfer's own rate after its first second and at the viewer's
  rate before that. Esc or Cancel stops it, and only sopen's own staging folder is removed. The
  result line is `saved <local path>`, also on stdout.
  - After a cancel the modal stays open and says `cancelled; nothing was saved`. Save again starts
    a new fill from the beginning. Esc with no save running closes the modal.
  - A cancel that comes once the complete file is getting its name is refused (`binary::Decision`):
    the file is saved, and said to be saved.
- **What it counts as.** A completed Save As puts the edits somewhere, so for the close prompt it
  counts as Save copy does (`HexDoc::mark_copied`), and the journal goes when nothing else is
  unsaved.

## 8. Image

- **Editing needs no new fetch.** Viewing already holds the whole file and the decoded image
  (06 §7.3), including after Continue on a large one. No editing-specific budget or confirm exists.
- **Save** is today's: the local backup (§5.2), encode, stage, then `write_atomic`'s conflict-checked
  overwrite. The content check is the whole hash taken by the streamed read (§4.5).
- **The journal** records the operations and the pasted pictures (§3). A restore replays them on the
  decoded original.
- **Save As** writes the encoder's output (§7).

## 9. Media (PR #15)

- **The render reads the remote source through the window's range server** (06 §7.4): only what its
  spans need, plus the container index and the 0.2 s preroll. It renders into a local temp under
  `<cache>/.exports/<random>/`, after checking local free space against bitrate × output length.
- **Save** runs PR #15's pre-render check and renders. It then makes the hard-link backup (§5.3,
  first Save only) and writes over the original by `write_atomic` (01 §5, unchanged). The upload is
  the output's size; the content is new, so that cannot be avoided.
- **What ED6 changes in PR #15.**
  - PR #15's Save refuses an `Input` that is a URL (`media/file.rs`), and its background hash skips
    remote files. ED6 lets Save take the URL. The baseline for a remote file of at most 256 MiB
    comes from §4.5's baseline pass, through the `bulk` URL; above that, from the first and last
    1 MiB (§3).
  - ED6 lands **before** W8b, which routes remote media through a Source, so remote media Save never breaks between them (06 §13).
- **After a remote Save lands**, PR #15 plays the staged render, which now equals the remote file.
  ED6 keeps that, and **closes the window's Source and its byte server**. The window's `Input`
  becomes the staged render's path, so the window never shows `Replaced` for its own save. The next
  Save's baseline is `write_atomic`'s staged stat, as today.
- **Export** is unchanged: `write_new_sibling`.
- **Save As** renders straight into the staging `part` (§7).
- **A source that changes mid-render** gets its response cut short by the range server (06 §9).
  ffmpeg may still exit 0 with a truncated output (verified, 06 §7.4), so the server's cut account
  decides: the render is failed, nothing is uploaded or published, the journal stays, and the
  window says why.

## 10. The windows

| Element | Behaviour |
|---|---|
| Dirty state | Today's, by content (hex: `HexDoc::unsaved`; image: `rev` against the saved and copied revisions). The status bar adds `kept at <journal>` (§3). |
| Save | Cmd/Ctrl+S and F5, to the remote by §4. Progress and Cancel while it runs. Result: `saved host:/abs · backup <path>`, plus notes. |
| While a Save runs | The hex and image windows take no edits and no undo or redo, as PR #15's media window already does (`MediaWindow::editable` is false then). The status row says `saving — editing resumes when the save is done` (hex: once an edit was tried; before that, the Save's progress). The view still scrolls, zooms and plays. Esc stops a hex Save between its copy requests (§4.2 step 5). |
| Save copy | Shift+Cmd/Ctrl+S, a remote sibling (§6). |
| Save As… | Alt+Cmd/Ctrl+S and F7 (§7). |
| Write in place… | Only in §4.1's refusal rows, behind §4.4's dialog. |
| Closing with unsaved edits (closing the window, Esc out of a browser viewer, F3) | `Save the changes to host:/abs before closing?` → **Save** · **Save As…** · **Discard** · **Cancel**. Save copy stands in for Save where Save is off, as today. Save As… opens the modal, and the window closes only after a save that landed. Discard's hover says it deletes the kept edits at <path>. Today's rules stay: closing during a save waits; a save that did not land keeps the window. |
| Recovery | §3's rows on open, and §5.1's undo-file rows in the hex window: the newest three, the rest counted in a line. A row's buttons are disabled while a Save runs. |

## 11. Robustness and bounds

- **No extra channel.** A save runs on the viewer's `Origin` channel (06 §6). While a copy request
  holds it, the viewer's reads go on over its `Ranged`.
- **Every copy request is about 1 s** (§4.2 step 5), so no request can reach the 30 s request
  timeout on a server that copies at least 16 MiB/s.
- **Memory.** Editing a 24 GB file in the hex window adds at most 16 MiB to the viewer's 06 §10
  budget: the patches and undo steps are at most 100 000 entries.
  - The hex window holds that bound: past 100 000 changed offsets a byte at a new offset is
    refused, and the status row says `too many changes for one save — save first` (a byte already
    edited can still change; Restore or Load as edits stops there and says how many bytes were not
    put in). A Save that lands makes room: what it wrote is the file's, and the evidence of
    offsets nothing names any more is let go.
  - Undo and redo hold it too. A step that would add an offset past the bound is refused with
    the same line and stays on its stack; nothing is lost. A byte put back to what the file holds
    there, with its page in memory, is no entry at all, so undoing an edit frees its slot. Where
    the page is not in memory (dropped from a stream's cache), the entry stays: it is what shows
    that byte without reading the file again.
  - What the edits keep beside the entries — each edited offset's original byte, each edited
    page's evidence — goes once nothing names its offset any more (no entry, no undo or redo
    step): it stays within twice what the entries and steps name.
- **Local disk.** A hex Save of a remote file writes only the journal and the undo file, in
  proportion to the edit, never to the file: at most 16 KiB + 16 bytes per changed byte + 128 bytes
  per edited 4 KiB page (two generations at a time). For E1's 4-byte edit that is under
  64 KiB. An image paste keeps its picture in the journal until the journal goes.
- **Server disk.** A server copy writes the file's full size on the server at every Save. The
  progress line says it is copying on the host; it is the cost of the atomic rename.
- **The undo file needs sopen to apply** (§5.1). It is complete, but it is not a file `cp` restores.
- **Sparse files.** `sftp-server`'s copy fills holes, so the temp takes the full apparent size. The
  room check uses the apparent size.

## 12. Acceptance

Every remote scenario runs against an SFTP server with real `copy-data`: in tests `local_server()`
(CI sets `SOPEN_REQUIRE_SFTP_SERVER`; OpenSSH 10.3p1's server was verified to offer `copy-data`,
`hardlink@openssh.com`, `posix-rename@openssh.com` and `fsync@openssh.com`, to copy byte-exact, and
to refuse a copy within one handle). For the "no copy-data" rows, a wrapper reports the extension
absent. Bytes are counted at the client's read and write payloads. Every save step has a test hook
that can stop the save there, or lose the step's reply, as 01 §5's tests already do for timeouts.
Gates and qualification are as in 06 §12. The live receipt is a file under a remote
`/tmp/sopen-receipts/`.

Two receipt hooks of the hex window, read in `gui/hex.rs` (in AGENTS.md's table):
`SOPEN_SAVE_AS=<folder>` saves the edits as `<folder>/<stem>.edited<ext>` — without a window
under `SOPEN_AUTOSAVE` (progress on stderr, `saved <path>` on stdout), else through the Save As
modal once the state is on screen; `SOPEN_SAVE_AS_HOLD_MS=<ms>` holds that fill at its first
progress at or past 30 % for that long, so `SOPEN_SCREENSHOT` shows it under way.

- **E1. A 4-byte patch to a 24 GB remote file.** Open a 24 000 000 000 byte file in the hex window,
  overwrite 4 bytes at 0x2DFDC1C35, press Save.
  - File bytes moved by the Save's own requests: **≤ 4 096 read + 4 written** (one 4 KiB page read
    back for the page check, and the patch). Nothing else of the file is read or written by the Save.
    The page itself was read once, when the window showed it.
  - After the Save the view reloads (§4.2 step 11) and fetches the blocks on screen again: ≤ 128 KiB.
  - No copy request takes longer than 5 s.
  - Local disk written: at most 64 KiB (the journal and the undo file; §11's bound for 4 bytes).
  - Afterwards the remote holds the new bytes, and writing the undo file back gives the original
    SHA-256. No `.sopen-` temp is left. The window says `saved … · backup <undo file>`, and the
    journal is gone once nothing is unsaved.
- **E2. The view keeps working during a save.** While E1 copies, scroll the grid. Every read is
  answered within RTT + 250 ms.
  - The unit test holds the copy and requires each uncached read to complete while it is held,
    on both servers (a handshake; no clock but a deadlock watchdog). The time is the live run's:
    `live_e2_reads_while_a_real_save_copies` (ignored; `SOPEN_TEST_SFTP_HOST=<alias>
    SOPEN_TEST_SFTP_FILE=<abs path of a file above 256 MiB>`, `--ignored --nocapture`) saves for
    real and prints each far read's time, frames included.
- **E3. Changed since open.** Change the remote's mtime, then Save.
  - It is refused at the first `lstat`, and nothing is created on the remote.
  - The edits stay in the window, and the journal path is named.
- **E4. A writer during the copy.** Write one byte of the source after the third copy request.
  - It is refused at the check before the replace, and the temp is removed.
  - The writer's byte is in the target, and no byte of sopen's patch is: the target is the file as
    the writer left it.
- **E5. Same size, mtime put back, edited bytes changed** (a file above 256 MiB). It is refused at the
  page check (`shown … now …`), the temp is removed, and the original is untouched.
- **E6. Same size, mtime put back, a byte changed far from the edits** (a file of at most 256 MiB). It
  is refused by the whole-content check, and the original is untouched.
  - **E6b.** The same on the no-copy-data route, with the changed block **already in the view's
    cache** from before the change: still refused. The streamed rewrite read the target fresh.
  - **E6c.** A change **before the baseline pass**, to the bytes being edited:
    1. page A is shown;
    2. another writer changes a byte of it, with size and mtime put back;
    3. the owner edits the still-shown page;
    4. the baseline pass then reads the changed byte.

    Save is refused with the original untouched, by one of two checks. Run it both with the page
    edited before the pass and edited after it.
    - Where the pass reads the changed byte (the page is not in the view's cache any more, or the
      pass reads the remote fresh, as ED1's `baseline_pass` does), the edited page's `seen` does
      not match the pass's page hash: the window says `the file changed under your edits —
      Cmd/Ctrl+R reloads`.
    - The hex window's pass is a Bulk reader on its own stream (§4.5), which serves a page still
      in its cache from the cache. Then the pass agrees with `seen`, and the change is caught by
      the copy's whole-content check (§4.2 step 6): `its content changed (same size and
      modification time)`.
- **E7. No room.** `statvfs` reports less than size + 1 GiB. Save is refused before any request that
  writes, naming the needed and the free bytes, and offering Write in place… and Save As….
- **E8. No copy-data, big file.** With no `copy-data` and a 300 MiB file, Save is refused before any
  transfer, with the cost stated. Write in place… needs the dialog's button; after it the remote holds
  the patch and the undo file exists.
- **E9. No copy-data, small file.** With no `copy-data` and a 1 MiB file, Save runs the streamed
  rewrite. The content checks are E1's, and the upload is at most 1 MiB plus protocol overhead.
- **E10. Crash.**
  - `kill -9` sopen 1 s after typing an edit, then open the same file again. The window offers
    Restore, and Restore shows the same changed bytes, marked. The remote is unchanged. Discard
    removes the journal directory.
  - `kill -9` 100 ms after a second edit: Restore gives the last durable generation, so the first
    edit at least.
  - **A kill at every step of §3**, each by a test hook:
    - **the journal's creation:** after the `mkdir`, before the parent's fsync, and before the first
      `CURRENT`. The reopen finds no journal, or one with no generation that it names and ignores;
      never a torn one;
    - **each step of a generation's publication:** a paste written; `gen-<n>` written or renamed;
      `CURRENT.tmp` written or renamed. The reopen restores generation `n − 1` or `n`, each whole;
    - **during a rebase:** the new generation written, `CURRENT` not yet moved. The reopen restores
      the old edits with the old baseline and evidence, never the old edits with new evidence or the
      reverse.
  - A generation that names a missing or torn paste, or whose own hash fails, is not applied. The
    journal is kept and named.
  - The status bar says `kept` only after the directory fsync that published the generation holding
    the last edit.
- **E10b. Restore checks content.**
  - Hex: put back the mtime after changing an edited page's bytes, then Restore. Nothing is applied,
    although `(size, mtime)` matches `head`.
  - Image: change a byte with the mtime put back. Nothing is replayed.
  - A journal with `seen: pending` asks before it applies anything.
- **E11. Undo file.**
  - Load as edits on E1's undo file shows the 4 original bytes as edits. Save writes them, and the
    remote's SHA-256 is the original's.
  - With an unrelated change kept by a Save (above 256 MiB, §4.5's first row), Load as edits and
    Save revert only sopen's 4 bytes. The unrelated change stays.
- **E12. Save As, 24 GB.** Save As to a free local path.
  - The file appears only when complete, with 24 000 000 000 bytes and the SHA-256 of the remote with
    the patch applied.
  - A taken name is refused with the next free `-N`.
  - Esc mid-transfer leaves nothing in the chosen folder (no `part`, no staging folder), and the edits
    stay unsaved.
- **E13. Save As, source changed mid-transfer.** Nothing is saved, the staging folder is removed, and
  the reason is shown.
- **E14. Image.** A remote 14000×14000 JPEG shows 06's memory line. After Continue, crop and Save.
  - The local backup is printed.
  - The upload is the encoded output.
  - The journal replays the crop after a kill.
- **E15. Media render from a remote source.** Cut and reorder a remote video, then Save.
  - The render reads through the range server, and the upload equals the output's size.
  - The result line names the hard-link backup and its size.
  - Without `hardlink@openssh.com`, Save is off with the reason.
  - A source changed mid-render uploads nothing, and the journal is kept.
  - After the Save lands, the window plays the staged render, its Source and byte server are closed,
    and no `Replaced` line appears.
  - A remote file of at most 256 MiB has its baseline from the baseline pass (§4.5).
- **E16. Close prompt.** With unsaved edits, close: Save · Save As… · Discard · Cancel (Save copy in
  Save's place where Save is off). Each behaves as §10 says. Receipt: `SOPEN_STATE=…;close=ask`
  screenshots of the prompt and of the recovery rows.
- **E17. Memory.** During E1 the process's resident memory stays within 06's V2 bound + 16 MiB.
- **E18. A cut-off Save is reconciled** (§3.1), by a stop or a lost reply at every phase, for each
  kind and route.
  - **Hex, server copy:**
    - stopped in `naming`: the temp, if any, is named and kept, never removed;
    - stopped in `created` or `staged`: on reopen the temp, whose stat (and evidence) match, is
      removed, and the edits are offered;
    - killed mid-copy (`created`, the temp at some size up to the target's): on reopen the temp is
      kept and named as a file sopen could not verify is its own partial copy, and its removal is
      offered to the user; killed after the temp's owner and mode were recorded and before the first copy
      request, it still equals the record and is removed;
    - **another file under the temp's name** after the kill (same owner and mode, smaller than
      the target, other bytes and mtime): kept, and its bytes are untouched;
    - the rename landed but its reply was lost: on reopen, "landed (found on reopen)", the journal
      retired, nothing offered twice.
  - **Image crop Save:** landed but the reply was lost. On reopen the output's hash identifies the
    landing, and the journal is retired.
  - **Image whose encoded output exceeds 256 MiB:** landed but the reply was lost. Reconciliation
    decides by the stat and the first and last 1 MiB. The window's next baseline is still the
    output's whole SHA-256. After a restart that follows the reconcile, opening the image and making
    a new edit gives a Restore and a Save that check that whole hash.
  - **Media timeline Save,** including an output above 256 MiB (checked by size and first and last
    1 MiB): landed but the reply was lost. The journal is retired. Not landed: the edits are offered.
  - **Write in place, stopped after k of n runs:** "holds some of your changes", the undo file named,
    nothing removed or written.
    - The same with **two runs in one 4 KiB page**, stopped after the first: each run is judged by
      its own recorded bytes, so the result is "in part", not unknown.
  - **No edits while a Save runs.** During a hex Save and during an image Save, typing, paste, cut,
    crop, pixelize, undo and redo do nothing, and the status row says
    `saving — editing resumes when the save is done`. A kill during that Save and a reopen then
    reconcile `gen-<m>` alone, with no later generation.
  - **An exclusive-create collision:** a file already has the temp's name. The create fails, the
    Save fails, and that file is never removed or changed.
  - **The recorded temp replaced** by another file of the same size before the reopen: its mtime or
    content differs, so it is kept and named.
  - **The target matches neither evidence** (another writer after the rename): unknown, both paths
    named, nothing removed.
- **E19. The residual windows are as stated** (§4.5's table, a file above 256 MiB):
  - a writer changes a region after the copy read it and puts the mtime back: the Save lands, and
    the writer's change is not in the result;
  - a writer changes the target between step 9 and the rename: the same.

  These two pin the documented loss, so the confirmation's words stay true. The first Save of the
  session asks once before any of it.
- **E21. The `Origin` refused at Save** (a `Connect` failing as sshd's `MaxSessions` refusal does).
  With unsaved hex edits:
  - Save fails with 06 §6's line;
  - the edits stay in the window and the journal, and nothing is sent to the remote;
  - a Save after the refusal ends lands.
- **E20. The media backup is a link.** After E15's Save, a write through a handle on the original
  opened before the Save changes the backup's bytes. The result line said so.

## 13. 01 and AGENTS.md text this changes (written by D1)

- **AGENTS.md "Never overwrite silently".** The content hash stays up to 256 MiB. Above that, the
  check is size, mtime and the pages under the edits (§4.5), with the best-effort table and the
  one-time confirmation. The hex backup is the undo file (§5.1). A remote media Save keeps a remote
  hard link, which can still change (§5.3).
- **01 §5.** The server-copy route and its four checks (§4.2). The streamed rewrite with its 256 MiB
  rule (§4.3). Write in place, with its unknown outcome on the target (§4.4). The kept same-size,
  same-mtime change above 256 MiB, and the writes a Save can lose (§4.5). The undo file, the hard-link
  backup, and journals as a new owner-only kind of file under `<cache>/.edits`, with their durability
  rules and the save intent (§3, §3.1).
- **01 §6a and §6.** Today hex takes edits during a Save ("an edit or an undo in that frame counts").
  Now hex and image take none until the Save ends (§10).
- **01 §6, §6a, §7.** Save As and its keys (§7). The close prompt's Save As… (§10). The recovery rows
  (§3, §5.1). F7 in the three F-key rows.
- **Browser workstream, for its owner.** Its key survey says "F2, F7 and F10 do nothing inside a
  viewer" (`.prd/research/keys.md`, merged with PR #17). F7 is Save As in the three editing viewers once
  ED3 lands, which that line must say. Its 04 gives F7 to MkDir in the list only, so the two do not
  collide.

## 14. Work units

Same rules as 06 §13: one agent, one branch, one worktree each; the gate is
`SOPEN_REQUIRE_FFMPEG=1 SOPEN_REQUIRE_SFTP_SERVER=1 just check`. The live table is
[`streaming/loop.md`](streaming/loop.md).

The edit units are ED1–ED6. Plain E-numbers are this document's acceptance scenarios (§12), and the
browser's experiments are its own E1–E5 (02 §8.2).

| ID | Unit | Owns | After |
|---|---|---|---|
| ED1 | **Patch primitives and the save algorithms.** A `PatchFs` trait (open by handle, positional writes, `fstat`, `fsync`, `statvfs`); the one `copy-data` loop, S1-C's, extended to about 1 s per request; §4.2–§4.5 as window-free functions, reading fresh; the undo-file writer; a hook at every step to stop there or lose the reply (E18) | `src/remote/patch.rs`, `src/remote/patch/tests.rs`, `src/remote/sftp/patch.rs` (SFTP), `src/remote/local/patch.rs` (`LocalFs`: a plain copy and `pwrite`, the trait's local half for tests) — all new; one `mod` line each in `src/remote/{mod,sftp,local}.rs`; `src/remote/sftp/ops.rs` (the copy loop's chunk size); a provided method `RemoteFs::patcher()` returning `None` | W1; S1-C merged; X2 |
| ED2 | **Journal.** Its durable creation, immutable generations published through `CURRENT` (§3), the 250 ms scheduling, the lock, listing a file's orphans, kinds, `seen` evidence, the `save` intent for every kind and route, temp ownership, and reconciliation (§3.1) | `src/journal.rs`, `src/journal/tests.rs` (new); `src/lib.rs` (`mod journal`) | — |
| ED3 | **Save As modal and F7.** The shared modal (path, Browse, the expected time, progress, cancel) over `binary::save_new`; F7 "SaveAs" in the three F-key rows | `src/gui/save_as.rs`, `src/gui/save_as/tests.rs` (new); `src/gui/mod.rs` (`mod save_as`); `src/gui/chrome.rs` (the F7 entries of the hex, image and media key rows only) | browser S1 merged (S1-A owns `chrome.rs`'s body) |
| ED4 | **Hex edit.** Save by ED1, no edits or undo while it runs, Save copy, Save As by ED3, the journal, Restore's checks, reconciliation on open, recovery and undo-file rows, the one-time confirmation, the prompt, the `Origin` refused at Save (E21); removes 06 §7.1's interim Save | `src/gui/hex/file.rs`, `src/gui/hex.rs`, `src/gui/hex/tests.rs` | W4, W8a, ED1, ED2, ED3 |
| ED5 | **Image edit.** First a pure move of the write side (`ImageFile`) into `src/gui/image/file.rs`; then the operation journal, the whole-hash next baseline at every size, no edits or undo while a Save runs, and Save As | `src/gui/image/file.rs` (new), `src/gui/image.rs`, `src/gui/image/robustness_tests.rs` | W6, ED2, ED3 |
| ED6 | **Media edit on streams.** Save taking a URL `Input`; the baseline pass for remote files; Save, Save As and Export through the URL, failed on a cut response; the hard-link backup through the existing `link_new` (no remote-layer change) and its disclosure; closing the Source after a remote Save; the journal and its evidence | `src/gui/media/file.rs`, `src/gui/media.rs`, `src/gui/media/edit_tests.rs` | W7, ED2, ED3; **before W8b** |
| D1 | **Docs.** 01 rewritten to the shipped contract (06 §14, §13 above); AGENTS.md rules and the 06 §11 hooks; README; 06 and 07 marked shipped | `.prd/01-spec.md`, `AGENTS.md`, `README.md`, `.prd/06-streaming.md`, `.prd/07-streaming-edit.md` | every unit of 06 and 07 |

**Order.**
1. ED2 now.
2. ED1 after W1 and S1-C.
3. ED3 once S1 has merged.
4. ED5 after W6.
5. ED6 after W7, then W8b.
6. ED4 after W8a.
7. D1 last.

loop.md has the merge order across both workstreams.

**Serial chains** (06 §13):
- hex W4 → ED4;
- image W6 → ED5;
- media PR #15 → S1-M → W7 → ED6;
- remote W1 → ED1, and `src/remote/sftp/ops.rs` S1-C → ED1.

`src/gui/chrome.rs`, `src/gui/mod.rs`, `src/lib.rs`, `AGENTS.md`, `README.md`, `.prd/01-spec.md` and
`.prd/02-architecture.md` are shared with the browser workstream and are sequenced, not disjoint.
