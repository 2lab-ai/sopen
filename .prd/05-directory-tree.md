---
title: sopen — directory tree
type: spec
tags: [sopen, spec, browser, directory-tree]
---

# sopen — directory tree (05)

Status: planned (2026-11-20)

Owner message 16 (spec §1; [`browser/ssot.md`](browser/ssot.md) item 7): F10 shows a directory tree
rooted at the home when the current folder is under it, else at `/`; device folders are never opened
by themselves; a folder with few subfolders opens by itself and others stay closed, but every closed
folder shows at once how many folders and files it holds and how big it is. The research, its
prototype and every measurement are in [`research/directory-tree.md`](research/directory-tree.md)
(its §2 draft is the base of this document). Measurements are of 2026-11-20 on an Apple M5 Max with
macOS 26.7.1 (APFS) and a btrfs Linux laptop; SFTP between them over a 12–38 ms link.

Amends, when it lands: 01 §3a (the tree is a new content of a side; hidden folders are grouped in it,
unlike the list), 01 §9 (the directory tree is no longer out of scope).

## 1. Opening and leaving

- **F10** in a list (and **Alt+F10**, Norton and Total Commander's tree key; 04 §2) shows the tree of
  the active panel's host **in place of the active panel's list** (02 §3.1: the active side's content
  becomes Tree; the opposite side keeps what it showed, frozen, until the tree closes). The frame's
  title is the root's `host:/abs`.
- The tree lists **folders only** (and links to folders); files are summed up on each row's info line.
- The bar starts on the active panel's folder, its row scrolled to the middle of the tree.
- **Enter** on a row lists that folder in the active panel and closes the tree (02 §7
  `TreeOutcome::GoTo`); on a compact chain (§3), the chain's last folder; on a link, its target,
  resolved. **Esc** or **F10** closes the tree with the panel as it was.
- **→** opens the row's folder (listing it if it never was), **←** closes it (on a closed row: the bar
  to its parent); ↑ ↓ PgUp PgDn Home End move the bar; a typed letter jumps to the next row starting
  with it, as in the list; **Space** sizes the row now (§6.3); Cmd/Ctrl+R lists the bar's row again.
- **F-key row** (04 §2's rules): `1Help` and `10Tree` (it closes the tree); the other slots are empty.
- `T-open`: F10 shows the tree on the active side; Enter on a row sets the panel to that folder; Esc
  restores the panel unchanged; Alt+F10 does the same as F10.

## 2. The root (owner item 1)

- Paths are resolved first: the folder by `realpath` when F10 is pressed (a panel's path is built by
  joining names and may run through a link, `panel.rs` `path_of`), the home once per host — locally
  `$HOME` resolved; remotely `expand-path@openssh.com` of `~<user>` (the account's home even when
  `sftp-server -d` moved the start folder), else SFTP `realpath(".")`.
- **The root is the home when the folder is the home or inside it, otherwise `/`.**
  - **Locally "inside" is identity**: the folder, or one of its ancestors, has the home's
    `(st_dev, st_ino)`. The folder is then shown under the home: `/System/Volumes/Data/Users/u/a` →
    `~/a`. A string test fails there (`realpath` keeps that alias, measured).
  - **Remotely** (SFTP has no inode) it is a component-wise prefix of the resolved paths: home
    `/home/ab` does not contain `/home/abc/x`.
  - A home of `/` gives `/`.
- The root's row reads `~` for the home, `/` otherwise.

`T-root`, with home `H`:

| Folder | Root |
|---|---|
| `H` | `H` |
| `H/a/b` | `H` |
| `H/../<home name>/a` (unresolved) | `H` |
| a link inside `H` pointing outside `H`, entered from the panel | `/` (resolved first) |
| `/tmp` on macOS (→ `/private/tmp`) | `/` |
| `/System/Volumes/Data` + `H` + `/a` on macOS | `H` (identity), the bar on `H/a` |
| `/Volumes/<boot volume>` + `H` on macOS (a link to `/`) | `H` (resolved first) |
| home `/home/ab`, folder `/home/abc/x` | `/` |
| a remote whose home is `/` (chrooted SFTP) | `/` |

## 3. Rows

- A folder's children: folders and links to folders, by name regardless of case, as in the list.
- **Hidden folders** — a name starting with `.`, or the macOS hidden flag `UF_HIDDEN` (`~/Library`,
  `/usr`, …) — are drawn as **one dimmed row** per folder, `· N hidden folders`, after its other
  children; → or Enter opens it into one dimmed row each. A hidden folder on the path to the current
  folder is always its own row. (The list shows dot entries one by one, dimmed, 01 §3a; the tree
  groups them: on the measured Mac home 59 of 103 folders are hidden, and grouped the home's top level
  is 45 rows instead of 103.)
- **Compact chains**: a folder whose only content is one shown subfolder (no files, no links, no
  special files, no hidden subfolders) is drawn on one row with it, `a/b/c`, as VS Code's compact
  folders are. A chain costs no row budget (§5).
- **A link** to a folder is a row marked `<LINK>` that never opens in place (no loops, no duplicates).
- Glyphs: `▼` open, `►` closed with subfolders, none for a folder with no subfolders, `→` a link —
  code page 437 characters the VGA font has; colours from `chrome::tree` (AGENTS.md "One look").
- `T-rows`: a folder with `.git`, `.github`, `src` draws `src` and `· 2 hidden folders`; `a` holding
  only `b` holding only `c` draws `a/b/c`; a link `l → ..` draws `<LINK>` and → does nothing.

## 4. Which folders open by themselves (owner item 2)

Every folder row has three separate answers, as broot's special paths do: **shown** (§3), **listed**
(read for its info line and children), **sized** (read below it for the deep size).

**Rules** — deterministic; such a folder is never opened by §5:

| # | Folder | Listed unopened | Sized | Detected locally (macOS / Linux) | Detected remotely |
|---|---|---|---|---|---|
| R1 | a link to a folder | no | no | `lstat` / `d_type` | READDIR attributes |
| R2 | unreadable (open fails: EACCES, EPERM — macOS privacy refusals too) | — | — | the open's error | OPENDIR's status |
| R3 | a mount point of a disk filesystem (`apfs`, `hfs`, `btrfs`, `ext4`, `xfs`, `zfs`, `vfat`, `exfat`, `ntfs`, `tmpfs`, …) — not the root, not on the path | yes | yes (stops at mounts below it) | the mount table: `getmntinfo(3)` `f_mntonname` / `/proc/self/mountinfo` field 5; cross-checked by `ATTR_DIR_MOUNTSTATUS` / `STATX_ATTR_MOUNT_ROOT` | Linux: `/proc/self/mountinfo`, read once per connection over SFTP; others: §4.1 |
| R4 | a kernel or virtual filesystem: `proc`, `sysfs`, `devtmpfs`, `devfs`, `devpts`, `cgroup`, `cgroup2`, `securityfs`, `debugfs`, `tracefs`, `bpf`, `pstore`, `efivarfs`, `configfs`, `fusectl`, `mqueue`, `hugetlbfs`, `binfmt_misc`, `nsfs`, `selinuxfs`, `fuse.portal` | no | no | the mount table's type | Linux: mountinfo; others: by name (`/dev`, `/proc`) |
| R5 | a network or FUSE-remote filesystem: `nfs`, `nfs4`, `cifs`, `smb3`, `smbfs`, `afpfs`, `webdav`, `9p`, `ceph`, `glusterfs`, `fuse.sshfs`, `fuse.rclone`, other `fuse.*` | no | no | type | Linux: mountinfo |
| R6 | an automounter trigger (`autofs`; macOS `DIR_MNTSTATUS_TRIGGER`, e.g. `/System/Volumes/Data/home`, `/Network/Servers`): a lookup can mount a share | no (not even stat'ed below) | no | type / mount status | Linux: mountinfo; macOS remotes: by name (`/home`, `/net`, `/Network`, `/System/Volumes/Data/home`) |
| R7 | a macOS `nobrowse` volume (`MNT_DONTBROWSE`: `/System/Volumes/Data`, `VM`, `Preboot`, `Update`, …): the Data volume holds everything under `/` a second time | no | no | `f_flags & MNT_DONTBROWSE` | macOS remotes: `/System/Volumes/*` by name |
| R8 | holds a `CACHEDIR.TAG` whose first 43 bytes are `Signature: 8a477f597d28d172789f06886806bc55` (cargo's `target/`, `.pytest_cache`, …) | yes | yes | its listing + a 43-byte read | the same, over SFTP |
| R9 | version-control internals: a folder holding `HEAD`, `objects` and `refs` (a `.git`), or named `.hg` / `.svn` | yes | yes | its listing / name | the same |
| R10 | a Python environment: holds `pyvenv.cfg` | yes | yes | its listing | the same |

- **Mount points come from the mount table, never from `st_dev` alone**: on the measured Mac `/`,
  `/Users` and the `/System/Volumes/Data` mount all report the same `st_dev`, and btrfs subvolumes
  change `st_dev` without being mounts.

**Heuristics** — never opened by §5, but listed and sized like any folder (except H3):

| # | Folder | Why a heuristic |
|---|---|---|
| H1 | hidden (§3) | grouped anyway; the path overrides it |
| H2 | named `node_modules`, `__pycache__`, `DerivedData`, `Pods`, `.gradle`, `.next`, `.tox` | generated and usually huge, with no tag; a name can be wrong |
| H3 | macOS `~/Library` and `~/.Trash`: not listed or sized until the owner opens it; sized only on Space | a scan below `~/Library` blocked in `open(2)` for 22 minutes; the cause (a privacy consent wait, or a stalled file provider such as `~/Library/Mobile Documents`) is unproven, and such folders can live elsewhere too — so this is a path guess, and the watchdog (§6.2) is what protects the tree |

`T-norules`: each row of both tables, built in a temp folder (mounts and macOS volumes through a fake
mount table behind the same trait as the real one), stays closed after F10; R4–R7 rows issue no
listing (counted by the fake).

### 4.1 Mount boundaries on a remote without `/proc/self/mountinfo`

SFTP has no `st_dev`. Before a remote folder is opened by §5 or sized, one `statvfs@openssh.com`
(when the server offers it) gives its `f_fsid`; a folder whose `f_fsid` differs from its parent's is a
mount point (R3) — except on a macOS remote, where `/` (the sealed system volume) and the Data volume
have different `f_fsid`s although the firmlinked folders (`/Users`, `/Applications`, `/Library`,
`/private`, `/opt`, `/usr/local`, `/Volumes`, `/cores`) are ordinary folders: there a folder is a
mount point only when its `f_fsid` is neither `/`'s nor `/System/Volumes/Data`'s, and R6/R7 go by
the names above (a `statvfs` on a trigger could mount it). A server without `statvfs@openssh.com`
gets the name rules only. A macOS remote is recognised by `realpath("/tmp")` = `/private/tmp` with
`/System/Volumes/Data` present.

## 5. Opening by itself

Parameters: **N = 5** (the owner's number), **D = 2**, **E = the rows the tree shows** (about 36 at the
default window size).

1. **Always open**: the root, and every **strict ancestor** of the current folder below the root (the
   path, not the current folder itself). The root is open even when it is the current folder.
2. **Anchors**: the root, every strict ancestor, and the current folder.
3. **Candidates**: a drawn folder that no rule or heuristic of §4 keeps closed, that has been listed
   **completely** (not stopped at §6.3's listing budget), has **1 to N − 1 shown subfolders** (hidden ones do not count), and is fewer than D drawn levels
   below its nearest anchor. A compact chain is always drawn, regardless of D.
4. **Order and budget**: repeatedly take the candidate nearest an anchor (fewest levels), then nearest
   the current folder's row, and open it if the rows it adds (its shown subfolders, plus one
   `· N hidden` row if it has hidden ones) still fit **E rows added in total** by this rule; otherwise
   it stays closed and the next is tried. Rows opened by rule 1 do not count against E.
5. **The current folder** follows N like any other candidate (its contents are in the panel already);
   nearest to itself, it is tried first — unless it is the root, which rule 1 opens. (`/private/tmp` with 1 368 subfolders stays closed: the tree
   was 13 rows with N applied, 1 412 with it always open.)
6. Opening is decided once, when F10 builds the tree; what the owner opens or closes stays so until
   the tree closes. A folder listed later (by →) is not opened by this rule.

- Measured: on the Mac home with the current folder two levels down, N = 5, D = 2, E = 36 drew 163
  rows (127 by rule 1, 36 by this rule; 28 candidates refused by the budget); the Linux home drew 62
  rows with none refused (research §4.5).
- `T-auto` (a temp tree, home = root, current folder `p/q`): the root and `p` are open; `p/q` with 3
  shown subfolders is open, with 5 it is closed; F10 at the home itself with 30 subfolders → the root
  is open showing all 30, and none of them opens by itself beyond E; a sibling of `p` with 2 subfolders is open and
  its children are not (D = 2); with E = 4 and six 2-subfolder siblings only the two nearest the bar
  open; a folder with 1 shown and 2 hidden subfolders opens to 2 rows; a folder with only hidden
  subfolders stays closed.

## 6. The info line and what is read

### 6.1 The info line

Each folder row ends with three right-aligned columns, from the folder's own listing:

```
► src                        19 d     40 f    9.8M+
▼ assets                      2 d      4 f    1.1M+
    sounds                    0 d      2 f     79K
→ Desktop                                    <LINK>
► proc                                        <SYS>
► home                        2 d      0 f  =87G fs
► Private                                          ?
► media                       3 d      0 f  not answering
```

- `d`: subfolders (links to folders included); `f`: files (regular files and links to files; special
  files are not counted, as the list's foot does not count them).
- **The size** is the bytes of the regular files in the folder and — as the background scan (§6.3)
  reaches them — below it: on the same filesystem, links not followed, a hard-linked file once (by
  device and inode, locally). **`+` means more may be below** (a lower bound); no `+` means the whole
  folder was counted. A folder with no subfolders is exact from its listing. Sizes are in the list's
  units, the smallest unit that fits, never cut.
- **This lower bound is the owner's "how big it is, shown at once"** — on purpose, not an
  extrapolated guess. The research tried both estimators that cost little (a breadth-first sample
  scaled up, and Knuth's random probes) and both missed the exact size by more than 2× on about half
  of the big folders (Mac 11–14 of 27, Linux 7–9 of 16 within 2×; the worst, 269 GB estimated at
  5.8 GB): big trees keep their bytes in a few deep branches no cheap probe reaches. The lower bound
  is never wrong, it grows as the scan runs, and locally the exact size usually arrives within seconds
  (a million entries in 5–8 s with 4–8 threads).
- A size from a scan finished earlier in this sopen process and not run again since shows as `~9.8M`
  (cached, not re-checked: a folder's mtime does not change when something deep inside it does).
- **Tags** in place of the size: `<LINK>` (R1), `<SYS>` (R4), `<NET>` (R5), `<AUTO>` (R6), `<SYSVOL>`
  (R7), `?` unreadable (R2), `not answering` (§6.2).
- **A mount point** of a disk filesystem (R3) shows at once the filesystem mounted there (a macOS
  `nobrowse` volume, R7, shows its `<SYSVOL>` tag instead, as D-4 asks): `=87G fs` = `(f_blocks − f_bfree) × f_frsize` from one `statvfs`
  (`statvfs@openssh.com` remotely), and its used file count `(f_files − f_ffree)` on the status row
  while the bar is on it. `=` and `fs` say it is the filesystem's figure (on APFS per volume; a btrfs
  subvolume reports the pool). A deep scan of an R3 row, when it runs, replaces it with the folder's
  own size. Never for R4–R6 (meaningless, or a call that can hang or mount).
- `T-info`: a temp folder with 2 subfolders and 3 files of 10, 20 and 30 bytes plus a hard link to
  the first shows `2 d 4 f 60B+` from the listing, then `60B` once scanned with nothing below; a deep
  folder shows a growing `+` size that ends without `+`; scrolled away and back it shows the size
  reached so far.

### 6.2 The watchdog (a hard rule)

- **Every local filesystem call of the tree** — listing, `lstat`, `statvfs`, the `CACHEDIR.TAG` read,
  every call of a deep scan — **runs on a pool thread behind a 2 s watchdog**, never on the frame and
  never on a thread that other rows wait behind.
- A call that has not returned in **2 s** is **abandoned, never awaited**: its row shows
  `not answering`, its thread is written off (a blocked `open(2)` cannot be cancelled, only left
  behind) and replaced. The tree keeps drawing and its keys keep working; closing the tree or the
  window never waits for an abandoned thread.
- The research saw an `open(2)` below `~/Library` block for **22 minutes** at 0 % CPU. A privacy
  consent wait or a stalled file provider are the candidates; neither is proven, and a hung network
  mount or a sleeping disk do the same. H3 avoids the one place seen; the watchdog is the protection.
- **Abandoned calls are counted for the whole process**, not per tree session: at most **4**
  outstanding at once. A slot comes back only when its call actually returns (late; its result is
  dropped). At the cap, no local tree call is started by any tree in the process — opening the tree
  again does not reset it — and the status row says why (`4 calls not answering; the tree waits for
  one to return`); listings already shown stay.
- **Remote** requests are futures on the tree's connection: they never block a thread and are
  dropped when not needed. A row whose listing has not answered in 2 s shows `not answering` too,
  and keeps waiting up to the 30 s request timeout (01 §5), then shows `?`.
- `T-watchdog`: a fake filesystem whose listing of one folder never returns → that row shows
  `not answering` after 2 s; the frame keeps painting; Esc closes the tree at once; the fake's thread
  is left blocked and the test ends without joining it. Then open and close the tree ten times on
  folders whose listings never return → at most 4 calls are ever outstanding, the fifth is never
  started, and the status row says so; release one fake call → it returns, its slot comes back and the
  next listing runs.

### 6.3 What is read, when

- **Phase 1** (before the tree's first frame): the mount table (locally one call; remotely once per
  connection), the home (once per host), the root and each folder on the path — one listing each.
- **Phase 2** (streamed, viewport first): each drawn row **on screen or within one screen above or
  below** is listed once for its info line and subfolder count; §5 runs on those listings, so it
  opens nothing it has not read. Rows further away are listed when they come near. No row is listed
  twice in one tree session.
- **Every automatic listing is bounded** — phases 1 and 2 as well as phase 3: it stops at **10 000
  entries or 2 s**, whichever comes first (locally inside the watchdog's call; remotely between
  READDIR replies, the futures dropped). A listing stopped early:
  - shows its counts as lower bounds — `9 d+  9991 f+  1.2M+` — and its subfolder status as unknown
    (the row keeps `►`), so it **never qualifies for opening by itself** (§5) and is never sized
    automatically;
  - on the path (phase 1): the subfolders found so far are drawn, and the path's next folder is drawn
    even when the listing stopped before reaching it (its name is known from the path);
  - **→** or **Space** on the row lists it whole, as an action of the owner's (no entry or time
    budget; Esc stops it), and the row then shows exact counts.
- **Phase 3** (background): the deep size of each closed, sizable row on screen, nearest the bar
  first, breadth first, one filesystem. Locally a pool of **4** workers with no budget per row
  (macOS `getattrlistbulk`, Linux `getdents64` + `statx` with `AT_STATX_DONT_SYNC`, asking only for
  type, size, inode, device and link count). Remotely a row's automatic scan stops at the first of
  **300 folder listings, 200 000 entries or 10 s** (one READDIR reply can carry any number of entries,
  so listings alone do not bound the work), and the row keeps its `+`; **Space** on it resumes it
  without the budget (and sizes an H3 row).
- **A row being scanned** shows its growing figure with a moving mark after it (`9.8M+ ·`), so a `+`
  that is still growing reads differently from a `+` that stopped at its budget (`9.8M+`).
- A row that leaves the screen for more than a second, a closed folder, a closed tree and a closed
  window stop their scans. Finished and partial results stay in a **per-process cache** keyed by host
  and resolved path (in memory only).
- **Nothing is written anywhere**: no tree file, no cache on disk; nothing is run on a remote (SFTP
  requests only).
- `T-lazy`: with a fake filesystem counting listings, F10 on a 1 000-folder root lists at most the
  root + the path + three screens of rows + E + one per compact-chain link; a folder never drawn is
  never listed; closing the tree mid-scan stops the workers (the fake sees no listing after the
  close).

### 6.4 Remote

| Need | Request(s) | Measured |
|---|---|---|
| home | `expand-path@openssh.com` `~<user>` (the user from `ssh -G`), else `realpath(".")` | 11.5 ms (1 round trip) |
| the folder | `realpath(folder)` | 1 round trip |
| mount table (Linux) | read `/proc/self/mountinfo` to its end (its size reads 0) | 3.9 KB, 31 mounts, 67–98 ms |
| a mount boundary elsewhere | `statvfs@openssh.com` per folder before opening or sizing it | 12–51 ms each |
| a listing | OPENDIR + READDIR … + CLOSE, plus a `stat` per link | 56–61 ms median, one at a time |
| a screen of rows | 36 listings, 16 in flight | 133 ms |
| deep size | breadth-first listings, 16 in flight | ~270 folders/s |

- **One connection of its own** for the tree (`Connect`, 02 §6), so a deep scan never queues behind
  the panel's listing or a viewer's fetch; at most **16** requests in flight, fewer when
  `limits@openssh.com` reports fewer `max-open-handles`.
- A Linux remote gets the same R3–R7 answers as local Linux from its `/proc/self/mountinfo` (the
  sftp-server's mount namespace is the login's); other remotes use §4.1.
- Over SFTP there is no inode or link count, so a remote size counts a hard-linked file once per name.
- Never: a command over ssh, parsing READDIR's `longname`, a stat per name.

## 7. Where it lives

| Part | Path (02 §5; slice S5: S5-A model, S5-B UI) |
|---|---|
| root rule, rules R1–R10 / H1–H3, expansion, info line, scans, watchdog pool, the per-process cache | `src/tree/*.rs` |
| `TreeFs`: bulk listing with device, inode, flags and mount status; mount table; `statvfs`; `expand-path`. It is a **worker with a request channel and a result channel**, not a `&mut self` call per request: remotely it owns the tree's SFTP connection and its runtime (`SftpRemote` runs one `block_on` per call today; the worker clones the cheap `Fs` handle and keeps up to 16 requests in flight), streams each result as it arrives, reports `not answering` after 2 s and drops a cancelled request's future; locally it is the watchdog pool of §6.2 | `src/remote/treefs.rs`, with `src/remote/local/treefs.rs` and `src/remote/sftp/treefs.rs` |
| the tree as a side's content, its keys and F-key row | `src/gui/browser/tree.rs` |
| the tree's look | `src/gui/chrome/tree.rs` |

## 8. Acceptance

Receipts: window screenshots someone looked at (AGENTS.md); remote runs on a host whose home holds a
fixture under `/tmp/sopen-receipts/` (and, for the root rule, the real home, read only).

- D-1. `T-open`, `T-root`, `T-rows`, `T-norules`, `T-auto`, `T-info`, `T-watchdog`, `T-lazy` pass
  (CI, both systems).
- D-2. Locally, F10 in a folder two levels below the home → the tree's first frame (phase 1) within
  **50 ms** warm (measured: 3 listings in 0.7–3.0 ms) and every info line of the screen within
  **250 ms** (measured: 157 listings in 6.7 ms warm, 114 ms on a first run); the root reads `~`; the
  bar is on the current folder in the middle; a screenshot shows the rows of §6.1's kinds.
- D-3. Remotely (a 12–40 ms link), F10 → the first frame within **1 s** (home, `realpath`, the mount
  table, the path's listings), and the screen's info lines within **1 s** more (measured: a screen of
  36 rows in 133 ms with 16 in flight).
- D-4. F10 in `/tmp` on macOS → root `/`; `/dev`, `/System/Volumes/*` and the automounted `home` show
  `<SYS>`, `<SYSVOL>`, `<AUTO>` and are never listed (the request log has no listing of them); a
  mounted disk image under `/Volumes` shows `=… fs`.
- D-5. A remote Linux `/` → `proc`, `sys`, `dev` show `<SYS>` without a listing; `/home` and `/boot`
  show `=… fs` from `statvfs@openssh.com`.
- D-6. A local folder with a 1 GB tree below a closed row → the row shows `…+ ·` at once and the
  exact size without `+` within seconds; a remote row with more than 300 subfolders below stops with
  `+`, Space resumes it, and it ends without `+`; a remote folder of 300 000 files in one folder stops
  at the entry budget, not after reading them all. A remote folder of 300 000 files **on the path**
  (and one on screen) → its first listing stops at 10 000 entries or 2 s: counts with `+`, the row not
  opened by itself, the path's next folder still drawn; → lists it whole, and Esc stops that.
- D-7. Open `~/Library` with → on macOS → it is listed then, not before; Space sizes it; any call
  that blocks shows `not answering` after 2 s while the tree stays usable.
- D-8. Esc during a deep scan → the tree closes at once and no listing is issued afterwards (local:
  the pool's counter; remote: the request log of a local `sftp-server`).
