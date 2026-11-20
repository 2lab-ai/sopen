---
title: sopen F10 directory tree — research, measured prototype, spec draft
type: research
tags: [sopen, file-browser, directory-tree, sftp, measurement]
---

# sopen F10 directory tree (2026-11-20)

Owner, verbatim: "f10인가 누르면 디렉토리 트리뷰 / 이거 잘 만들어야함. / 1. 예를들어 디폴트로는 루트가 아니라
홈디렉토리 기준임. 현재 cwd가 홈디렉토리 밑에있으면 홈디렉토리가 루트로 트리로 보여줘야함. 만약 홈디렉토리가 아니면 그때
"/" 기준으로 보여줘야하고. / 2. 디바이스 폴더는 펼쳐주면 안됨. 즉, 펼쳐주는 폴더랑 안펼쳐주는 폴더를 구분할수 있어야함.
예를들어 어떤 폴더 안에 디렉토리가 5개(예)미만이면 디폴트로 펼쳐서 보여주고 아니면 폴더를 닫은채 보여줘야함. (대신에 안에
어떤 느낌인지 폴더랑 파일이 몇개 있고 용량이 얼마 되는지 예측할수 있는 정보는 즉시 보여줘야함) / 이거 리서치해서 영리하게
디렉토리 트리 보여주도록 해줘."

## 0. Answer in ten lines

1. **Root.** Home when the folder is the home or below it, else `/` — Dolphin's Folders panel does exactly this
   (§1). Locally the test is identity, not string prefix: walk up the folder's ancestors comparing `(st_dev, st_ino)`
   with the home's, so macOS's `/System/Volumes/Data/Users/<u>/…` alias and bind mounts count as "under home".
   Remotely: home = `expand-path("~<user>")`, else `realpath(".")`; folder re-resolved with `realpath` at F10.
2. **Never auto-expanded** (rules): links, unreadable folders, every mount point, macOS `nobrowse` volumes and autofs
   triggers, folders holding `CACHEDIR.TAG`, git/hg/svn internals, Python venvs. **Never auto-expanded**
   (heuristics): hidden folders (dot names, macOS `UF_HIDDEN`), `node_modules` and a short list, and macOS
   `~/Library` (a scan there blocked in `open(2)` for 22 minutes, cause unproven, §6). Mount points come from the mount
   table (macOS `getmntinfo`, Linux `/proc/self/mountinfo` — over SFTP too), **not** from `st_dev` alone: on this Mac
   `/`, `/Users` and the `/System/Volumes/Data` mount all report the same `st_dev`.
3. **Never listed or sized unopened**: kernel/virtual filesystems (`proc`, `sysfs`, `devfs`, …), network/FUSE mounts,
   autofs, links, unreadable folders, macOS `~/Library`. A disk mount point (and a macOS `nobrowse` volume) shows its
   filesystem's used bytes and file count at once from one `statvfs`. Every local filesystem call runs on a pool
   thread behind a 2 s watchdog — that, not any path rule, is the protection: a blocked `open(2)` cannot be cancelled.
4. **Auto-expand**: a folder opens by itself when it has 1–4 shown subfolders (N = 5), sits directly below the root,
   a path folder or the current folder (so opened rows reach at most 2 levels below them: D = 2), and the rows it
   adds fit a budget of one tree pane (E ≈ 36 rows), nearest to the current folder's row first. Single-child
   chains draw as one row `a/b/c` (free). The root and the path are always open; the current folder itself follows
   the N rule.
5. **Info line**, per folder row, from one listing of that folder: `12 d  340 f  1.2G+` — subfolders, files, and
   the bytes of its direct files (`+` = a lower bound). A background scan of the rows on screen grows that number and
   drops the `+` when the whole folder (one filesystem, links not followed, hard links once) is counted.
6. **No extrapolated size — a departure from the brief's "~1.2 GB".** Both estimators tried (breadth-first
   extrapolation, Knuth random probes) missed the exact size by more than 2× on about half of the big folders (Mac:
   11 and 14 of 27 within 2×; Linux: 9 and 7 of 16, §4.4). The lower bound is no better as a *prediction* (6 of 27 /
   10 of 16 within 2×; `Games` showed `589M+` for 269 GB) but it is never wrong, and it grows: locally the exact size
   arrives fast — 4–8 threads scan a million entries in 5–8 s on this Mac (§4.1), the Linux home's 0.8 M in under 1 s
   warm — so `+` is a short-lived state there; remotely it is the honest state after the budget.
7. **Cost**: phase 1 (root + path) is 1 listing per level; phase 2 lists only rows within a screen of the
   viewport (≤ ~110), the ≤ E rows the heuristic opens, and one per compact-chain link (157 listings on this Mac's
   home). Locally that is under a frame warm (median 0.013 ms per folder) and ~1.6 ms per folder on first touch; over SFTP
   36 folders with 16 listings in flight took 133 ms (RTT ~12 ms). Deep sizes: Linux warm ~0.9 M entries/s, this Mac
   ~43 k entries/s per thread; SFTP ~270 folders/s (a 60 k-folder home ≈ 4 min) — so remote deep sizes stop at a
   budget and say `+`.
8. **Remote plan**: a dedicated connection for the tree, up to 16 listings in flight, the mount table read once from
   `/proc/self/mountinfo` (Linux) or `statvfs@openssh.com` `f_fsid` per folder (others), nothing ever run on the
   remote, nothing cached on disk.

Sections: §1 what the good tools do · §2 spec draft (testable, repo-ready wording) · §3 data flow and remote plan ·
§4 measurements · §5 decisions and departures · §6 residual risks · §7 prototype · §8 sources.

## 1. What good tree tools do

| Tool | What it shows / expands | How it gets counts and sizes | Boundaries, links, caching |
|---|---|---|---|
| **Dolphin** Folders panel (KDE) | Root = home when the current URL is in the home, else `/` ("Limit to Home Folder"; review D7477: "limit the displayed folders in the folder panel (F7) to the tree below the user's home directory if the current URL is inside the home directory", default changed to enabled). Expands only the parents of the current URL (`expandParentDirectories`). Hidden folders: a toggle. [1][2] | Lazy: a level is read when it is opened (KDirModel). | — |
| **Total Commander** directory tree (Alt+F10) | "By default, Total Commander only reads the top level directories of the tree, like Windows Explorer." The old full tree is an option ("Always load complete directory tree"). [3] | Full tree = one scan of the drive, saved as `treeinfo.wc` in the drive root (if writable) or `%LOCALAPPDATA%\GHISLER\`; "you must rescan the tree by pressing F2 or CTRL+R". [3] | A persisted cache that goes stale until rescanned. |
| **Far Manager** tree panel | Whole-drive folder tree. [4] | "Far stores folder tree information in the file named tree3.far at root folder of each drive" (read-only drives: `Tree.Cache` next to Far.exe); setting "Minimum number of folders": "The minimal number of folders on the disk for which folder tree cache file tree3.far will be created"; Ctrl+R / F2 rescan. [4] | Same persisted-cache model as TC. |
| **broot** | Trims the tree to fit the screen ("fitting into your screen even when there are millions of files … a parallel trimming of child directories to give you a balanced view"); trimmed levels end in "X unlisted". Hidden and gitignored files hidden at start. [5][6] | "The size of directories is computed in background … results are cached so you may move around and come back to find the sizes already here." Deep file counts are a toggle (`toggle_counts`). [5][7] | `special_paths`: per path glob, `show` / `list` / `sum` each `default`/`always`/`never`, e.g. `"/media": { list: "never" sum: "never" }` — "exclude some path because it's on a slow device or non relevant". [8] |
| **VS Code** Explorer | `explorer.compactFolders` (on by default since 1.41): single-child folder chains render as one row, "useful for Java package structures". `files.exclude` hides `.git` etc. by default. [9][10] | Lazy per folder. | — |
| **tree(1)** | `--filelimit #`: "Do not descend directories that contain more than # entries." `-L level`, `-d` folders only. [11] | `--du` accumulates sizes (full walk). | `-x`: "Stay on the current file-system only. Ala find -xdev." `-l` follows links but "Symbolic links that will result in recursion are avoided when detected." [11] |
| **ncdu** | Whole tree, sorted by size. | One full scan (`-t` threads); export/import JSON (`-o`/`-f`). | `-x`: "only count files and directories on the same filesystem"; `--exclude-kernfs` skips binfmt, bpf, cgroup, cgroup2, debug, devpts, proc, pstore, security, selinux, sys, trace; `--exclude-caches`: folders with `CACHEDIR.TAG` "are still displayed, but their contents will not be scanned or counted"; hard links flagged `H` ("Same file was already counted"); "Directory hard links and firmlinks (MacOS) are not supported." [12] |
| **gdu** | Whole tree. | Parallel ("intended primarily for SSD disks"), `--sequential` "intended for rotating HDDs"; results can be stored in a database and reread. [13] | `--ignore-dirs` default `[/proc,/dev,/sys,/run]`; `-x` "Do not cross filesystem boundaries"; "Hard links are counted only once." [13] |
| **dust** | "a slightly-less-than-the-terminal-height number of the biggest subdirectories or files" — a screen budget. [14] | Walker threads default to CPU count: "a higher count can speed up the walk by overlapping more concurrent stat calls." [14] | `-x` "Only show directories on the same filesystem"; "Dust will not count hard links multiple times". [14] |
| **dua** | Interactive / aggregate. | "It's parallel by default and will max out your SSD". APFS clone dedupe is opt-in "because collecting the additional metadata reduces traversal performance." [15] | — |
| **lsd** | `--tree` exists (alias example `lsd --tree`); its README documents nothing about depth or size rules. [16] | — | — |
| **macOS Finder / Spotlight** | Finder hides `UF_HIDDEN` items (`~/Library`, `/usr`, …). | Spotlight has no folder size: `kMDItemFSSize` is `(null)` on folders, and `kMDItemFSNodeCount` disagreed with the real entry counts here (4 vs a much larger folder; 77 vs 145) — §4.1. Not usable. | `MNT_DONTBROWSE`: "file system is not appropriate path to user data" (sys/mount.h). [17] |

What the kernels offer for cheap counts:

- **macOS `getattrlistbulk(2)`** returns, per directory entry, the type, flags, `st_dev`, inode, link count, file size
  and — for a child folder — `ATTR_DIR_ENTRYCOUNT` (its entry count) and `ATTR_DIR_MOUNTSTATUS`
  (`DIR_MNTSTATUS_MNTPOINT`, `DIR_MNTSTATUS_TRIGGER`), in batches, with no stat per name. Caveats from the man page:
  "ATTR_DIR_ENTRYCOUNT is usually expensive to calculate on non-HFS [Plus] volumes" (it was free on APFS here: §4.1);
  a mount point's attributes "will be those from the (underlying) file system"; a firmlink has `SF_FIRMLINK` and its
  bulk attributes are the firmlink's, not its target's. [17]
- **Linux `getdents64`** gives `d_type`, so folder/file counts need no stat — but "only some filesystems (among them:
  Btrfs, ext2, ext3, and ext4) have full support … All applications must properly handle a return of DT_UNKNOWN."
  Sizes need `statx`/`lstat` per file; `statx` reports `STATX_ATTR_MOUNT_ROOT` ("The file is the root of a mount",
  since 5.8) and `stx_mnt_id`. [18][19] Directory `st_nlink − 2` = subfolder count on ext4/xfs, but btrfs reports 1
  (measured, §4.2).
- **SFTP v3** attributes are size, uid/gid, permissions, atime/mtime — no `st_dev`, no inode, no link count. The
  `longname` of READDIR has the link count in OpenSSH, but "clients SHOULD NOT attempt to parse the longname field".
  OpenSSH's `sftp-server` sends "up to 100 entries in one message". `statvfs@openssh.com` returns `f_fsid` and
  `f_flag`; `expand-path@openssh.com` expands `~`, `~/…`, `~user/…`; `limits@openssh.com` gives `max-open-handles`.
  [20][21][22][23]

What sopen takes from them: Dolphin's root rule; TC's and Explorer's lazy levels (no whole-disk scan, no persisted
tree file); broot's three separate decisions per path (show / list / sum) and background sizes that survive moving
around; VS Code's compact chains; `tree --filelimit`'s "do not descend big folders"; ncdu/gdu/dust's `-x`, kernfs
skip, `CACHEDIR.TAG` and hard-links-once; dust's screen-sized budget.

## 2. Spec draft — `§3b The directory tree` (wording ready for `.prd/01-spec.md`)

No host names, paths of this machine or measurements in this section; each rule has a test.

### 2.1 Opening

- **F10** in the list (and Alt+F10: Total Commander's directory tree, Far's "find folder" tree) shows the tree of the
  active panel's host in place of the two panels; the bottom row gains `10 Tree`. Esc or F10 again leaves it with
  the panels as they were. Enter on a tree row puts the active panel on that folder and leaves the tree.
- The bar starts on the active panel's folder, its row scrolled to the middle of the tree pane.
- The tree lists **folders only** (and links to folders); files are summarised on each row's info line.
- ←/→ close / open the row's folder (→ on a closed folder lists it if it was never listed), ↑/↓/PgUp/PgDn/Home/End
  move the bar, a typed letter jumps as in the list.
- `T-open`: F10 shows the tree; Enter on a row sets the panel to that folder; Esc restores the panels unchanged.

### 2.2 Root (owner item 1)

- Paths are resolved first: the folder by `realpath` at F10 (a panel path built by joining names may run through a
  link), the home once per host: locally `$HOME` resolved; remotely `expand-path@openssh.com` of `~<user>` (the
  account's home even when `sftp-server -d` changed the start directory), else SFTP `realpath(".")`, the start
  directory, which is the home by default.
- **The root is the home when the folder is the home or inside it, otherwise `/`.** Locally "inside" is identity:
  some ancestor `A` of the folder (or the folder) has the home's `(st_dev, st_ino)`; the folder is then shown and
  reached in the tree as the home followed by the folder's path below `A` (`/System/Volumes/Data/Users/u/a` → `~/a`).
  Remotely (no inode in SFTP) it is a component-wise prefix of the resolved paths. A home of `/` gives `/`.
- The root's row reads `~` for the home (with the full path in the frame title), `/` otherwise.
- Tests (`T-root-*`), with home `H`:

| Folder | Root |
|---|---|
| `H` | `H` |
| `H/a/b` | `H` |
| `H/../<home name>/a` (unresolved) | `H` |
| a link inside `H` pointing outside `H`, entered from the panel | `/` (resolved first) |
| `/tmp` on macOS (`→ /private/tmp`) | `/` |
| `/System/Volumes/Data` + `H` + `/a` on macOS (same directory as `H/a`) | `H` (identity), the bar on `H/a` |
| `/Volumes/<boot volume name>` + `H` on macOS (a link to `/`) | `H` (resolved first) |
| `H` = `/home/ab`, folder `/home/abc/x` | `/` (component-wise) |
| remote home `/` (chrooted SFTP) | `/` |

### 2.3 Rows

- Children of a folder: folders and links to folders, by name regardless of case, as in the list.
- **Hidden folders** — a name starting with `.`, or the macOS hidden flag (`UF_HIDDEN`, which Finder honours:
  `~/Library`, `/usr`, `/bin`, …) — are drawn as one dimmed row per folder, `· N hidden folders`, after its other
  children; → / Enter opens that row into one dimmed row each. A hidden folder on the path to the current folder is
  always drawn as its own row.
- **Compact chains**: a folder whose only content is one shown subfolder (no files, no links, no special files, no
  hidden subfolders) is drawn on one row with it, `a/b/c`, as VS Code's `explorer.compactFolders` does. Enter on
  such a row goes to the last folder of the chain.
- A link to a folder is a row marked `<LINK>` and never opens in place (no loops, no duplicates); Enter goes to its
  target, resolved.
- `T-rows`: a folder with `.git`, `.github`, `src` draws `src` and `· 2 hidden folders`; `a` holding only `b`
  holding only `c` draws `a/b/c`; a link `l → ..` draws `<LINK>` and → does nothing.

### 2.4 Which folders open by themselves (owner item 2)

Every folder row has three separate answers, as broot's `special_paths` have: **shown** (§2.3), **listed** (read
for its info line and children), **sized** (read below it for the deep size).

**Rules** — deterministic, never opened by the heuristic:

| # | Folder | Listed unopened | Sized | Detected locally (macOS / Linux) | Detected remotely |
|---|---|---|---|---|---|
| R1 | a link (to a folder) | no | no | `lstat` / `d_type` | READDIR attrs |
| R2 | unreadable (open fails: EACCES, EPERM — also macOS privacy refusals) | — | — | the open error | the OPENDIR status |
| R3 | a mount point of a disk filesystem (`apfs`, `hfs`, `btrfs`, `ext4`, `xfs`, `zfs`, `vfat`, `exfat`, `ntfs`, `tmpfs`, …) — not the root, not on the path | yes | yes (stops at mounts below it) | mount table: `getmntinfo(3)` `f_mntonname` / `/proc/self/mountinfo` field 5; cross-checked by `ATTR_DIR_MOUNTSTATUS` / `STATX_ATTR_MOUNT_ROOT` | Linux: `/proc/self/mountinfo` read once over SFTP; other systems: §2.4.1 |
| R4 | a kernel / virtual filesystem: `proc`, `sysfs`, `devtmpfs`, `devfs`, `devpts`, `cgroup`, `cgroup2`, `securityfs`, `debugfs`, `tracefs`, `bpf`, `pstore`, `efivarfs`, `configfs`, `fusectl`, `mqueue`, `hugetlbfs`, `binfmt_misc`, `nsfs`, `selinuxfs`, `fuse.portal` | no | no | fstype from the mount table | Linux: mountinfo; others: by name (`/dev`, `/proc`) |
| R5 | a network or FUSE-remote filesystem: `nfs`, `nfs4`, `cifs`, `smb3`, `smbfs`, `afpfs`, `webdav`, `9p`, `ceph`, `glusterfs`, `fuse.sshfs`, `fuse.rclone`, other `fuse.*` | no | no | fstype | Linux: mountinfo |
| R6 | an automounter trigger (`autofs`; macOS `DIR_MNTSTATUS_TRIGGER`, e.g. `/System/Volumes/Data/home`, `/Network/Servers`) — a lookup can mount a share | no (not even stat'ed below) | no | fstype / mount status | Linux: mountinfo; macOS remote: by name (`/home`, `/net`, `/Network`, `/System/Volumes/Data/home`) |
| R7 | a macOS `nobrowse` volume (`MNT_DONTBROWSE`: `/System/Volumes/Data`, `VM`, `Preboot`, `Update`, …) — the Data volume holds everything under `/` a second time | no | no | `f_flags & MNT_DONTBROWSE` | macOS remote: `/System/Volumes/*` by name |
| R8 | holds a `CACHEDIR.TAG` whose first 43 bytes are `Signature: 8a477f597d28d172789f06886806bc55` (cargo `target/`, `.pytest_cache`, `.ruff_cache`, …) | yes | yes | the folder's own listing + a 43-byte read | same, over SFTP |
| R9 | version-control internals: a folder holding `HEAD`, `objects` and `refs` (a `.git`), or named `.hg` / `.svn` | yes | yes | its listing / name | same |
| R10 | a Python environment: holds `pyvenv.cfg` | yes | yes | its listing | same |

**Heuristics** — never opened by the heuristic, but listed and sized like any folder:

| # | Folder | Why a heuristic |
|---|---|---|
| H1 | hidden (§2.3) | grouped anyway; the path overrides |
| H2 | named `node_modules`, `__pycache__`, `DerivedData`, `Pods`, `.gradle`, `.next`, `.tox` | generated, usually huge, no tag; a name can be wrong |
| H3 | macOS `~/Library` (and `~/.Trash`): not listed or sized until the user opens it; sized only on Space | a scan below it has been seen to block in `open(2)` for many minutes; the cause — a privacy consent wait for another app's data or a stalled file provider (`~/Library/Mobile Documents`) — is unproven, so this is a path guess, and provider-backed folders can live elsewhere too: the 2 s watchdog (§2.7) is what actually protects the tree |

`T-norules`: each row of the tables above, built in a temp folder (mounts and macOS volumes by a fake mount table
behind the same trait as `LocalFs`), stays closed after F10; R4–R7 rows issue no listing (counted by the fake).

#### 2.4.1 Mount boundaries on a remote without `/proc/self/mountinfo`

SFTP has no `st_dev`. Before a remote folder is opened by the heuristic or sized, one `statvfs@openssh.com`
(when the server offers it) gives its `f_fsid`; a folder whose `f_fsid` differs from its parent's is a mount point
(R3) — except on macOS, where `/` (the sealed system volume) and the Data volume have different `f_fsid` although
the firmlinked folders (`/Users`, `/Applications`, `/Library`, `/private`, `/opt`, `/usr/local`, `/Volumes`,
`/cores`) are ordinary folders: there a folder is a mount point only when its `f_fsid` is neither `/`'s nor
`/System/Volumes/Data`'s, and R6/R7 go by the names above (a `statvfs` on a trigger could mount it). A server
without `statvfs@openssh.com` gets the name rules only. A macOS remote is recognised by `realpath("/tmp")` =
`/private/tmp` with `/System/Volumes/Data` present.

### 2.5 Auto-expansion

Parameters (defaults): **N = 5**, **D = 2**, **E = the number of rows the tree pane shows** (about 36 at the default
window size).

1. **By rule, always open**: the root and every folder on the path to the current folder.
2. **Anchors** for D: the root, every folder on the path, and the current folder.
3. **Candidates**: a drawn folder that no rule or heuristic of §2.4 closes, that has been listed, has between 1 and
   N − 1 **shown** subfolders (hidden ones do not count — opening a folder only to show `· 3 hidden folders` is
   useless) and is fewer than D drawn levels below its nearest anchor. A compact chain (§2.3) is always drawn, at
   no cost and regardless of D.
4. **Order and budget**: repeatedly take the candidate nearest an anchor (fewest levels), then nearest the current
   folder's row; open it if the rows it adds (its shown subfolders, plus one `· N hidden` row if it has hidden
   ones) still fit E rows added in total by this heuristic; otherwise it stays closed and the next is tried. The
   rows that rule 1 opens do not count against E.
5. **The current folder** is a candidate like the others (its contents are in the panel already); being nearest to
   itself, it is the first one tried.
6. Opening is decided once, when F10 builds the tree; folders the user opens or closes stay so until the tree is
   left. A folder listed later (by →) is not opened by the heuristic.

`T-auto` (temp tree, home = root, current folder `p/q`): `p` and `p/q`'s ancestors are open; `p/q` with 3 shown
subfolders is open, with 5 it is closed; a sibling of `p` with 2 subfolders is open and its children are not
(D = 2); with E = 4 and six 2-subfolder siblings only the two nearest the bar open; a folder with 1 shown and 2
hidden subfolders opens to 2 rows; a folder with only hidden subfolders stays closed.

### 2.6 The info line

Each folder row ends with three right-aligned columns, from the folder's own listing:

```
▸ src                        19 d     40 f    9.8M+
▾ assets                      2 d      4 f    1.1M+
    sounds                    0 d      2 f     79K
▸ Games                       3 d      0 f    589M+
→ Desktop                                    <LINK>
▸ proc                                       <SYS>
▸ home                        2 d      0 f  =87G fs
▸ .Trash                                         ?
```

- `d`: subfolders (links to folders included); `f`: files (regular files and links to files; special files are not
  counted, as the list's foot does not count them).
- The size is the bytes of the regular files in the folder and, as the background scan (§3) reaches them, below it,
  on the same filesystem, links not followed, a hard-linked file once (by device and inode where they are known).
  `+` means more may be below it (a lower bound); no `+` means the whole folder was counted. Sizes in the list's
  units, the smallest unit that fits, never cut.
- A size from a scan finished earlier in this sopen process and not run again since is shown as `~9.8M` (cached,
  not re-verified: a folder's modification time does not change when something deep inside it does).
- Tags in place of the size: `<LINK>` (R1), `<SYS>` (R4), `<NET>` (R5), `<AUTO>` (R6), `<SYSVOL>` (R7), `?`
  unreadable (R2).
- A mount point of a disk filesystem (R3) and a macOS `nobrowse` volume (R7) show, at once, the filesystem mounted
  there: `=87G fs` = `(f_blocks − f_bfree) × f_frsize` from one `statvfs` (`statvfs@openssh.com` remotely), and its
  used file count `(f_files − f_ffree)` on the status row while the bar is on it. The `=` and `fs` say it is the
  filesystem's figure, not the folder's: on APFS it is per volume; on btrfs a subvolume reports the whole pool. A
  deep scan of an R3 row, when it runs, replaces it with the folder's own size. Never for R4–R6 (meaningless
  or, for NFS and autofs, a call that can hang or mount).
- `T-info`: a temp folder with 2 subfolders, 3 files of 10, 20 and 30 bytes and a hard link to the first shows
  `2 d 4 f 60B+` from the listing, then `60B` (or more) once scanned with nothing below; a deep folder shows a
  growing `+` size that ends without `+`; scrolling it away and back shows the size reached so far.

### 2.7 What is read, when

- **Phase 1** (before the first frame of the tree): the mount table (local: one call; remote: once per connection),
  the home (once per host), the root and each folder on the path — one listing each.
- **Phase 2** (streamed, viewport first): each drawn folder row in or near the viewport (the rows on screen plus one
  screen above and below) is listed once for its info line and its subfolder count; the heuristic of §2.5 runs on
  those listings, so it opens nothing it has not read. Rows far off screen are listed when they come near the
  viewport. No row is listed twice in one F10 session.
- **Phase 3** (background): the deep size of each closed, sizable row in the viewport, nearest the bar first,
  breadth first, one filesystem. Budgets: locally none per row but a pool of 4 workers; remotely 300 folder listings
  per row, then the row stops with `+` (→ Space on the row resumes it without the budget). A local listing that has
  not returned in 2 s marks its row `…` (still waiting) and its worker is written off: a blocked `open(2)` cannot be
  interrupted (one has been seen to block for many minutes), so the pool replaces it, at most 4 written off per F10 session,
  after which phase 3 stops for the session with the reason on the status row. A row that leaves the
  viewport for more than a second, a closed folder, a left tree and a closed window stop their scans; finished and
  partial results stay in a per-process cache keyed by host and resolved path.
- Nothing is written anywhere: no tree file, no cache on disk (TC's `treeinfo.wc` and Far's `tree3.far` exist
  because they scan the whole drive; sopen reads only what is drawn).
- `T-lazy`: with a fake filesystem counting listings, F10 on a 1 000-folder root lists at most root + path +
  3 screens of rows + E + one per compact-chain link; a folder never drawn is never listed; closing the
  tree mid-scan stops the workers (the fake sees no listing after the close).

## 3. Data flow and the remote plan

### 3.1 Pipeline (both sides)

```
F10 ──► resolve folder (realpath) ─► home (cached per host) ─► root rule
     ──► mount table (cached per connection)
     ──► phase 1: list root, each path folder ──────────────► first frame (rows by rule, info lines of the path)
     ──► phase 2: list viewport ±1 screen rows (≤ ~110) + opened ──► info lines stream in; heuristic opens ≤ E rows
     ──► phase 3: deep-size closed viewport rows, nearest first ─► sizes grow, `+` drops when complete
scroll ─► rows entering the window are listed (phase 2) and sized (phase 3); rows leaving it > 1 s are cancelled
Esc ───► every job's stop flag set; workers end at their next listing; results stay in the process cache
```

- **Worker shape** (the browser's rule "nothing slow runs on a frame"): one tree worker thread owns the listing
  connection and a priority queue (phase 1 > phase 2 by row distance to the bar > phase 3 by row distance); results
  come back over the channel the browser already uses for listings. Locally phase 3 runs on a pool of 4 threads
  (macOS `getattrlistbulk`, Linux `getdents64` + `statx` with `AT_STATX_DONT_SYNC`, asking only for type, size,
  inode, device and link count); a row's scan is a queue of folders shared by the pool.
  Every local filesystem call of the tree (phases 1–3) is made on a pool thread watched by a 2 s timer, never on
  the frame and never on a thread other rows wait behind: a call can block without bound (a privacy consent wait, a
  hung network or FUSE mount, a sleeping disk) and cannot be cancelled, only abandoned.
- **Cache** (in memory, per process, keyed by `(host, resolved path)`): `{listing digest: d, f, direct bytes, subfolder
  names; folder mtime; deep: bytes, entries, complete?, finished-at}`. A listing is reused within the F10 session; a
  later F10 re-lists (one listing, as the panel re-lists on entering) and keeps a finished deep size as `~` until its
  scan runs again.
- **Cancellation**: each job carries the generation of the row's visibility; the worker checks the job's stop flag
  before each listing (and, remotely, drops the futures of in-flight requests: the replies are read and ignored,
  handles closed). Nothing is left behind because nothing is written.
- **Hard links**: locally a file with link count > 1 is counted once per `(st_dev, st_ino)` within a row's scan;
  over SFTP there is no inode or link count, so a remote size counts a hard-linked file once per name (the spec says
  so; the screen does not).

### 3.2 Remote (SFTP, nothing run on the remote, nothing written)

| Need | Request(s) | Measured (§4.3) |
|---|---|---|
| home | `expand-path@openssh.com` `~<user>` (user from `ssh -G`), else `realpath(".")` | 11.5 ms (1 RTT) |
| folder resolved | `realpath(folder)` | 1 RTT |
| mount table (Linux) | open + read + close of `/proc/self/mountinfo` (size 0 in its attrs; read to EOF) | 3.9 KB, 31 mounts, 67–98 ms |
| mount boundary (no `/proc`) | `statvfs@openssh.com` per folder before opening/sizing it | 12–51 ms each |
| a listing | OPENDIR + READDIR × ⌈entries/100⌉ + 1 + CLOSE, plus one `stat` per link | median 56–61 ms sequential |
| viewport | 36 listings, 16 in flight | 133 ms |
| deep size | breadth-first listings, 16–32 in flight | ~270 folders/s, 3.5–11 k entries/s |

- **One connection of its own** for the tree (`Connect`, as each viewer has), so a long deep scan never queues
  behind the panel's listing or a viewer's fetch; ≤ 16 requests in flight by default (64 was faster — 106 ms vs
  285 ms for 71 folders — but `limits@openssh.com` `max-open-handles`, when offered, caps it, and other servers may
  allow fewer handles).
- **Deep sizes over SFTP are budgeted**: at ~270 folders/s a 60 k-folder home takes ~4 minutes, so a remote row
  stops after 300 listings (~1–2 s with 16 in flight) with `+`; Space resumes that one row unbudgeted.
- **Linux remotes** get the same R3–R7 answers as local Linux from `/proc/self/mountinfo` (that file describes the
  sftp-server process's mount namespace, which is the login's). **macOS/BSD remotes** use §2.4.1.
- Free per listing: each entry's type, size and mtime (so `d`, `f` and direct bytes cost nothing beyond the
  listing); `CACHEDIR.TAG` costs one 43-byte read only when the name is present.
- Never: running a command over `ssh` (spec §4: no remote shell parsing), parsing `longname`, stat-ing every name.

## 4. Measurements (2026-11-20, read-only)

Machines: this Mac — Apple M5 Max, macOS 26.7.1, APFS, warm page cache unless "first touch". The Linux laptop —
CachyOS, kernel 7.2.6, x86_64, 32 threads, btrfs on NVMe; read over `ssh -o BatchMode=yes` with `find`, `du`,
`stat`, `findmnt`, `ls` (nothing installed or written there) and over SFTP by the prototype (read-only requests).
Raw outputs: `tree-proto/data/` (§7).

### 4.1 This Mac

**Home, top two levels** (`level2`, 5 warm runs each, median): 103 folders (home + its 102 readable subfolders;
`~/.Trash` refused, EPERM), 1 449 entries.

| Method | Total for all 103 folders |
|---|---|
| A `read_dir` + `d_type` (counts only) | 1.3 ms |
| B `read_dir` + `lstat` per name (= `LocalFs::list_entries` today) | 2.5 ms (per folder median 0.013 ms, p90 0.043 ms, max 0.197 ms) |
| C `getattrlistbulk` (counts + sizes + flags + mount status) | 1.5 ms |
| C+ the same with `ATTR_DIR_ENTRYCOUNT` | 1.5 ms — free on APFS despite the man page's warning |
| B, first touch in that run | 165.8 ms |

`ATTR_DIR_ENTRYCOUNT` from the home's bulk listing equalled a `read_dir` count for all 102 child folders
(`entrycount` mode) — one listing of a parent gives every child's entry count.

**The tree** (`tree $HOME $HOME/2lab.ai/sopen --n 5 --d 2 --rows 36`): 163 rows = 127 by rule (the home row, its 44 shown
folders and `· 59 hidden folders`, and the ~80 shown folders of the path's parent `2lab.ai`) + 36 opened by the heuristic (33 folders opened,
including compact chains). Phase 1: 3 listings, 0.7–3.0 ms. Phase 2 (viewport-bounded): 157 listings (≤ 109 rows
near the bar + 36 opened + chain links), 6.7 ms warm, 114 ms on a first run. Around the current folder:

```
    ▾ sopen                                          8 d      8 f   162K+     path
        assets/fonts                                 0 d      2 f     89K
        packaging/homebrew                           0 d      2 f    2.2K
      ▾ src                                          3 d     14 f   300K+
          ffmpeg                                     0 d      2 f     20K
        ▸ gui                                        7 d     12 f    998K
          remote                                     0 d      4 f    135K
        tests                                        0 d      3 f     80K
      · 4 hidden folders
    ▸ soul                                           7 d      1 f   155M+
    ▸ Teum                                          21 d     28 f    338M
```

**Root `/` with the folder `/private/tmp`** (13 rows; `/private/tmp` has 1 368 subfolders, so N keeps it closed —
with the current folder always open the same tree was 1 412 rows):

```
▾ /                                                 15 d      1 f     0B+
  ▸ Applications                                    25 d      2 f   8.0K+
  ▸ Library                                         65 d      1 f     0B+
  ▾ private                                          4 d      0 f     0B+     hidden-flag path
    ▸ etc                                           21 d     53 f   789K+
      tftpboot                                       0 d      0 f      0B
    ▸ tmp                                         1368 d    407 f   123M+     path
    ▸ var                                           32 d      0 f     0B+
  ▸ System                                           7 d      1 f     0B+
  ▾ Users                                            2 d      1 f     0B+
    ▸ Shared                                         5 d      2 f     0B+
    ▸ <user>                                    103 d     60 f    57M+
  · 14 hidden folders
```
Phase 1 (3 listings, `/private/tmp` with 1 775 entries) took 70 ms and 187 ms in two runs.

**Mount boundaries under `/`, depth 1–2** (`mounts`; 130 folders): `getmntinfo` lists 14 mounts — `/` (apfs, sealed,
ro), `/dev` (devfs), nine `nobrowse` APFS volumes (`/System/Volumes/{Data,VM,Preboot,Update,xarts,iSCPreboot,
Hardware}`, two CoreSimulator images, one cryptex), `/System/Volumes/Data/home` (autofs, `map auto_home`), one
mounted disk image under `/Volumes`.

| Detector | Fired at depth 1–2 |
|---|---|
| `st_dev` differs from the parent | `/dev`, the `/Volumes` image — 2 |
| `ATTR_DIR_MOUNTSTATUS` mount point | the same 2 |
| in the mount table | the same 2 (the other 12 are deeper) |
| `SF_FIRMLINK` (an ordinary folder for the tree) | `/Library`, `/private`, `/Users`, `/Applications`, `/opt`, `/Volumes`, `/cores`, `/usr/local` — 8 |

`stat`: `/`, `/Users`, `~`, `/System/Volumes/Data`, `/private/tmp` all have `st_dev` 16777233; `/System/Volumes/VM`
16777234; the image 16777245. `statvfs` `f_fsid`: `/` 0x1000013, `/Users` = `/System/Volumes/Data` 0x1000011,
`VM` 0x1000012, `Preboot` 0x1000010, `/dev` 0x1ef44bf9. So `st_dev` misses the Data volume mount (which holds all of
`/` again), while `f_fsid` and the mount table see it.

**Root rule** (`roots`): `/tmp` → `/private/tmp` → `/`; `/Volumes/Macintosh HD/Users/<u>` → `/Users/<u>` → home;
`/System/Volumes/Data/Users/<u>/2lab.ai` stays that path under `realpath` (string rule: `/`), and the identity walk
finds the home's `(st_dev, st_ino)` at `/System/Volumes/Data/Users/<u>` → home; `H/../<u>/2lab.ai` → home; home
`/home/ab` vs `/home/abc/x` → `/`.

**Deep sizes.** Exact, one thread, `getattrlistbulk`: 9 510 733 entries in 214.8 s over 83 home folders (~44 k
entries/s; `Games` 1 039 382 entries / 127 588 folders / 269 GB in 24.2 s; `iq.io` 1 032 878 entries in 30.7 s).
Shared-queue pool on `Games`: 1 thread 28.5 s, 4 threads 8.3 s, 8 threads 5.0 s (36 k → 125 k → 209 k entries/s).
Budgeted at 20 000 entries / 50 ms per row, the tree's 125 closed rows sized in 4.1–4.3 s on one thread: 79
complete, 46 stopped with `+` (`Games`: `589M+`–`789M+` of 269 GB). An unbudgeted scan of `~/Library` blocked in
`open(2)` for 22 minutes (0 % CPU) and was killed (§6).

**Spotlight**: `mdls` on `~/2lab.ai/sopen` and `~/Downloads`: `kMDItemFSSize = (null)`, `kMDItemPhysicalSize =
(null)`, `kMDItemFSNodeCount` 4 and 77 (a `read_dir` of `~/Downloads` gives 145); `mdfind -count -onlyin` 0 for both.

### 4.2 The Linux laptop (local commands over ssh, warm cache)

- Home: 96 entries at the top, 404 folders and 721 entries to depth 2, **801 636 entries / 59 959 folders /
  276 GB** in all (`du -sxb`: 276 454 802 001 bytes).
- Times (bash `time`): readdir of the top two levels 1 ms, with `lstat` 2 ms; every name in the home (`find -xdev`,
  `d_type`) 0.337 s; with `lstat` 0.902 s; `du -sxb ~` 0.831 s.
- 32 mounts (`findmnt`): btrfs subvolumes `/`, `/home`, `/root`, `/srv`, `/var/{cache,log,tmp}`; `/boot` vfat;
  tmpfs `/tmp`, `/run`, `/dev/shm`, `/run/user/1000`; and `devtmpfs devpts sysfs securityfs cgroup2 pstore efivarfs
  bpf proc autofs tracefs mqueue hugetlbfs configfs fusectl debugfs binfmt_misc fuse.portal`. None below the home;
  no `st_dev` change below the home to depth 4.
- btrfs folders report `st_nlink` 1 (`/`, `/home`, the home), so `nlink − 2` gives nothing; folder `st_size` is a
  name-length sum (home: 1 994).
- `stat -f` ids: `/` d01d010bf907ab77, `/home` = home d01d010bf907ab76, `/srv` …74, `/var/cache` …73, `/tmp`,
  `/proc`, `/sys`, `/dev`, `/run`, `/boot` all distinct — every subvolume and mount has its own fsid.
- In the home: 10 `CACHEDIR.TAG`, 45 `node_modules`, 41 `.git` folders (on this Mac, 27 `CACHEDIR.TAG` within 4
  levels of `~/2lab.ai`, mostly cargo `target/`).

**The tree** on a capture of that home (`find ~ -printf …`, replayed by the prototype; structure only):
`tree /home/<u> /home/<u>/2lab.ai/herdr` → 62 rows = 34 by rule + 28 opened (18 folders), 61 listings. Root `/`
with the folder `/tmp` → 24 rows:

```
▾ /                                                 16 d      0 f     0B+
  → bin                                                                       link
    boot                                             0 d      0 f      0B     mount:local
  ▸ dev                                                                       mount:pseudo
  ▸ etc                                            104 d    113 f   887K+
  ▸ home                                             2 d      0 f     0B+     mount:local
  ▾ opt                                              3 d      0 f     0B+
      microsoft/msedge                               0 d      0 f      0B
    ▸ openai-codex                                   3 d      1 f   246B+
    ▸ wine-cachyos                                   3 d      0 f     0B+
  ▸ proc                                                                      mount:pseudo
  ▸ run                                             34 d      2 f   773B+     mount:local
  ▸ sys                                                                       mount:pseudo
  ▸ tmp                                             28 d     13 f   150K+     mount:local path
  ▸ usr                                              8 d      0 f     0B+
  ▸ var                                             13 d      1 f   255B+
  · 1 hidden folders
```
(`/boot`, `/root` show 0 because the capture could not read them; depth-4 folders of the `/` capture are empty.)

### 4.3 SFTP, this Mac → the laptop (tailnet, realpath RTT 12–38 ms across two runs)

| Request | Result |
|---|---|
| ssh master + sftp channel | 200–366 ms + 104–120 ms (sopen's `ControlPersist=60s` amortises it) |
| `realpath(".")` | `/home/<u>`, 12.5–37.6 ms each |
| `expand-path` `~` and `~<u>` | `/home/<u>`, 11.5 ms each |
| read `/proc/self/mountinfo` | 3 882 bytes, 31 mounts, classified as on the laptop; 67–98 ms |
| `statvfs@openssh.com` × 10 | `f_fsid` distinct for `/` (…ab77), `/home` = home (…ab76), `/srv` (…ab74), `/tmp`, `/proc` (0x18), `/sys`, `/dev`, `/run`, `/boot`; `f_flag` 0x2 (nosuid) on the kernel ones; 11.8–59.3 ms each |
| list the home (96 entries, + a `stat` per link) | 234–319 ms |
| top two levels, one listing at a time | 71 folders / 628 entries: 4 958–6 031 ms (median 56–61 ms per folder, max 363–642 ms) |
| same, 4 / 16 / 64 listings in flight | 861–1 768 ms / 285–575 ms / 106–307 ms |
| a viewport: 36 folders, 16 in flight | 566 entries, 133 ms |
| deep breadth-first from the home, 1 in flight | 19 folders/s, 242 entries/s |
| … 16 in flight, 20 000 entries | 283 folders/s, 3 507 entries/s |
| … 32 in flight, 15 s cap | 268 folders/s, 11 134 entries/s (288 621 entries, 67 GB, 8 399 folders still queued) |
| `longname` | `drwxr-xr-x    1 <u> <u>  2698 Nov 20 10:48 .config` — link count 1 on btrfs (and not to be parsed) |

### 4.4 Size estimates vs exact

Per home folder bigger than the budget; "within 2×" = estimate / exact in [0.5, 2].

| Estimator | Mac (folders) | Linux capture (folders) |
|---|---|---|
| breadth-first, 2 000 entries: lower bound | 6 of 27 | 10 of 16 |
| breadth-first, 2 000 entries: + unlisted folders × mean folder size | 11 of 27 | 9 of 16 |
| breadth-first, 50 000 entries: lower bound / extrapolated | 10 / 10 of 16 | 1 / 1 of 4 |
| Knuth random probes, 64 probes (2 240 / 1 077 listings) | 14 of 27 | 7 of 16 |
| Knuth random probes, 256 probes (5 106 / 2 313 listings) | 12 of 27 | 8 of 16 |

Worst misses: `Games` 269 GB estimated 5.8 GB (Knuth 64), and 589–789 MB as the lower bound the tree reached in its
20 000-entry / 50 ms budget; `~/.claude` 49 GB → 166 MB; Linux `.local` 97 GB → 0.8–2.8 GB. Big trees hide their bytes in a few deep branches that neither
a shallow walk nor a few random probes reach. Hence `+` (honest lower bound) and exact, nothing in between.

### 4.5 Choosing N, D, E (`sweep`, hidden folders grouped)

Rows drawn / folders opened (incl. root, path, chains) / listings / candidates refused by the budget:

| N | D | E | Mac home, folder `2lab.ai/sopen` | Linux home, folder `2lab.ai/herdr` |
|---|---|---|---|---|
| 3 | 2 | 36 | 163 / 45 / 164 / 5 | 44 / 12 / 42 / 0 |
| 4 | 2 | 36 | 163 / 37 / 159 / 16 | 57 / 18 / 56 / 0 |
| **5** | **2** | **36** | **163 / 35 / 160 / 28** | **62 / 20 / 61 / 0** |
| 5 | 2 | 24 | 151 / 23 / 107 / 20 | 57 / 18 / 56 / 2 |
| 5 | 2 | 60 | 187 / 49 / 201 / 25 | 62 / 20 / 61 / 0 |
| 5 | 3 | 36 | 163 / 35 / 160 / 37 | 70 / 26 / 71 / 6 |
| 5 | 1 | 36 | 127 / 19 / 127 / 0 | 34 / 6 / 33 / 0 |
| 6 | 2 | 36 | 163 / 32 / 161 / 45 | 62 / 20 / 61 / 0 |
| 8 | 2 | 36 | 163 / 29 / 160 / 61 | 62 / 20 / 61 / 0 |
| 5 | 2 | 36, hidden shown one row each | 223 / 32 / 164 / 21 | 98 / 18 / 101 / 0 |

N = 5 (the owner's number) with D = 2 and E = one pane fills the Mac's pane around the current folder (the budget,
not N, is what stops it: 28 candidates refused) and leaves the smaller Linux home with room to spare. D = 3 changes
nothing on the Mac (budget-bound) and adds 8 rows on Linux. D = 1 opens nothing but chains (the root's and path
folders' children are already at level 1).

## 5. Decisions (one line each, with the reason)

| Decision | Reason |
|---|---|
| Root by **identity** locally, string prefix remotely | `realpath` keeps `/System/Volumes/Data/Users/<u>/…` as is, so a string test says `/`; `(st_dev, st_ino)` of an ancestor finds the home there (measured, §4.1). SFTP has no inode. |
| Remote home = `expand-path("~<user>")`, fallback `realpath(".")` | In OpenSSH's `sftp-server.c` both `"~"` and `"."` resolve against the start directory (`-d` changes it); `~user` goes through `tilde_expand`, the account's home. |
| `realpath` the panel folder at F10 | `panel.rs:128` builds entered paths by joining names, so a panel path can run through a link. |
| Mount points from the **mount table**, not from `st_dev` | On this Mac `/`, `/Users` and `/System/Volumes/Data` share `st_dev` 16777233, so a `st_dev` test misses the Data volume (which holds all of `/` again); btrfs subvolumes change `st_dev` without being mounts. |
| Budget **E = extra rows** (rows added by the heuristic), not "total rows under a screen" (the brief's wording) | The rows the rules open are already over a screen: 127 on this Mac's home (44 shown home folders + ~80 in the path's parent, hidden ones grouped), so a total-rows budget would open nothing anywhere. |
| N does not apply to the root, the path, or chains | The owner's rule is about folders the tree opens *by itself*; the path is where the user is, and a chain adds no row. |
| The current folder follows N (not always open) | Its contents are already in the panel; `/private/tmp` here has 1 368 subfolders and would bury the tree (1 412 rows measured with it open, 13 with N applied). |
| D is counted from the nearest **anchor** (root, path folder, current folder) | Context next to the current folder is what the tree is for; counted from the root, D = 2 would never open anything below a deep current folder. |
| **Hidden folders grouped** into `· N hidden folders` (departs from §3a's "shown, dimmed" in the panels) | On this Mac's home 59 of 103 folders are hidden (58 dot folders + `~/Library`, `UF_HIDDEN`); grouped, the home's top level is 45 rows instead of 103. Finder, broot and Dolphin hide them by default; the group row keeps them one key away. Sweep columns for both in §4.5. |
| Viewport-bounded listing (phase 2) | The prototype first listed every drawn row (195 listings on this Mac's home); a path folder like `/private/tmp` would add 1 368. Bounded to ±1.5 screens, and with hidden children of opened folders left unlisted, it is 157. |
| `+` lower bound and exact only; `~` only for a cached earlier result — departs from the brief's "~1.2 GB" | No estimator was within 2× on more than about half of the big folders (§4.4); the lower bound predicts worse (6 of 27 on the Mac) but is never wrong, and locally exact arrives in seconds with 4–8 threads. |
| Mount points show their filesystem's used size (`=87G fs`) at once | The owner asked for immediate "what is inside" on exactly these folders; one `statvfs` gives it (12–51 ms remotely); marked as the filesystem's because btrfs subvolumes report the pool. |
| `~/Library` rule is a heuristic (H3), the watchdog is the rule | The 22-minute block's cause is unproven (consent wait or file provider); a path rule cannot cover provider folders elsewhere. |
| Folders with no subfolders show an exact size from the listing | Nothing is below them; the listing counted every file. |
| No tree file on disk | TC's `treeinfo.wc` / Far's `tree3.far` cache whole-drive scans that sopen does not do; spec §9 keeps sopen small; a stale tree is worse than a 0.1 s listing. |
| F10 (+ Alt+F10) | The owner's key; Alt+F10 opens Total Commander's directory tree and Far's "find folder" tree (Far: "Perform find folder command Alt+F10"; its tree panel is Ctrl+T). |
| Remote deep size budget 300 listings per row | ~270 folders/s measured; 300 listings ≈ 1–2 s per row with 16 in flight, so a screen of rows settles in well under a minute; Space lifts it per row. |

## 6. Residual risks (what could still be wrong after this spec)

- **A blocked scan under `~/Library`.** The prototype's unbudgeted size scan of `~/Library` (the `estimate` mode)
  sat in `open(2)` for 22 minutes at 0 % CPU (`sample`: `__open`) and was killed; the path was not identified (that
  needs root tools). Two candidate causes, neither proven: a macOS privacy consent wait for another app's data
  (`~/Library/Containers/…`), or a stalled file provider (`~/Library/Mobile Documents` exists, and
  `fileproviderctl dump` itself hung for over 35 s at the same time). The unified log of the last hour has no `tccd`
  entry naming the prototype (`log show`, compact; such entries may be redacted), so a consent dialog is unlikely but
  not excluded. Hence H3 and, above all, the 2 s watchdog.
  Listing `~/Desktop`, `~/Documents`, `~/Downloads`, removable or network volumes can likewise ask for consent when
  sopen is not covered by its terminal's grant (e.g. started by `--target` over ssh); from this terminal they listed
  fine and only `~/.Trash` refused (EPERM). Proof needed: one `--target` run on a Mac whose sshd has no Full Disk
  Access, F10 at home.
- **Cold cache** was not measured: local timings are warm or "first touch" (the first listing in this run, after the
  machine had been used). A cold-disk number needs `purge` (root) on macOS or `drop_caches` on Linux.
- **ext4 / xfs / zfs and nested btrfs subvolumes** were not observed: the laptop is btrfs with no subvolume below the
  home (no `st_dev` change to depth 4) and no mount below it. The rules do not depend on them (mount table, not
  `st_dev`), but `T-norules` should include a bind mount and a nested subvolume in CI if a Linux runner allows it.
- **Remote macOS** (§2.4.1) is designed from local `statvfs` measurements of this Mac (`f_fsid` of `/` differs from
  the Data volume's) and OpenSSH's protocol text, not from an SFTP session to a Mac.
- **Huge single folders on the path** (a 1 368-folder `/private/tmp`) are listed in phase 1 by rule: 70 ms locally
  here; remotely ~14 READDIR replies. A path folder with 10⁵ subfolders would still be listed whole.
- **Hard links over SFTP** are counted per name (no inode), so a remote size can exceed the disk usage.

## 7. Prototype

`<scratchpad>`
— a throwaway Rust crate on sopen's own `Cargo.lock` (libc, openssh, openssh-sftp-client, tokio; built
`--offline`), read-only everywhere. Not product code.

| File | What |
|---|---|
| `src/fsx.rs` | three local listing methods (A `read_dir` + `d_type`; B `read_dir` + `lstat` per name, what `LocalFs` does; C `getattrlistbulk`), `getmntinfo` table, `/proc/self/mountinfo` and `findmnt` parsers, the mount classifier (R3–R7) |
| `src/model.rs` | the `Lister` trait (local disk / a `find -printf` capture), root rule, info line, rules R1–R10 / H1–H2 (H3 is not coded: `~/Library` is caught by H1's hidden flag; there is no watchdog — the `estimate` run that blocked shows why the product needs one), the auto-expansion of §2.5 with the viewport bound, the deep scan (a budget-hit size prints as a lower bound `+`), renderer |
| `src/remote.rs` | the SFTP measurements of §4.3 (realpath, expand-path, mountinfo read, statvfs `f_fsid`, sequential / concurrent listings, budgeted deep breadth-first) |
| `src/main.rs` | modes: `roots`, `mounts`, `level2`, `tree`, `sweep`, `estimate`, `knuth`, `entrycount`, `par`, `sftp` |
| `data/` | every output quoted in §4 (`level2-mac-home.txt`, `mounts-mac.txt`, `tree-*.txt`, `sweep-*.txt`, `estimate-*.txt`, `knuth-*.txt`, `sftp-cachy*.txt`, `cachy-*.txt`) and the laptop captures (`cachy-home.bin`, `cachy-root3.bin`, `cachy-findmnt.txt`) |

Re-run: `cargo build --release --offline` with `CARGO_TARGET_DIR=…/tree-proto/target`, then e.g.
`target/release/tree-proto tree "$HOME" "$PWD" --n 5 --d 2 --rows 36 --deep-entries 20000 --deep-ms 50`;
`--capture data/cachy-home.bin --findmnt data/cachy-findmnt.txt` replays the laptop's home (structure only — times
from a capture are not disk times). The `sftp` mode starts its own ssh master at a short relative ControlPath
(`ctl/cm`; the ~104-byte socket-path limit, which sopen's `MAX_SOCKET_PATH` also guards) and exits it after.

## 8. Sources

1. Dolphin `src/panels/folders/folderspanel.cpp` (`loadTree`, `expandParentDirectories`, hidden toggle) —
   https://github.com/KDE/dolphin/blob/master/src/panels/folders/folderspanel.cpp
2. KDE review D7477 "Limit folder panel to home directory if inside home" (default changed to enabled) —
   https://phabricator.kde.org/D7477
3. Total Commander help, "Show directory tree" — https://tcmd.madsenworld.dk/onlinehelp/totalcmd.chm/show_tree.htm
4. Far Manager help source `far/FarEng.hlf.m4` (tree panel, `tree3.far`, Tree settings "Minimum number of folders",
   "Perform find folder command Alt+F10", "Toggle tree panel Ctrl+T") —
   https://github.com/FarGroup/FarManager/blob/master/far/FarEng.hlf.m4
5. broot, Tree view — https://dystroy.org/broot/tree_view/
6. broot issue #106 "Truncated listing for current directory" ("X unlisted") — https://github.com/Canop/broot/issues/106
7. broot usage (`toggle_counts`) — https://dystroy.org/broot/documentation/usage/
8. broot configuration, special paths — https://dystroy.org/broot/conf_file/
9. VS Code 1.41 release notes, compact folders — https://code.visualstudio.com/updates/v1_41
10. VS Code user interface docs (`files.exclude` hides `.git` by default) —
    https://code.visualstudio.com/docs/getstarted/userinterface
11. tree(1) — https://manpages.debian.org/testing/tree/tree.1.en.html
12. ncdu manual — https://dev.yorhel.nl/ncdu/man
13. gdu README — https://github.com/dundee/gdu
14. dust README — https://github.com/bootandy/dust
15. dua README — https://github.com/Byron/dua-cli
16. lsd README — https://github.com/lsd-rs/lsd
17. macOS 26.7.1 `man 2 getattrlist`, `man 2 getattrlistbulk`; SDK `sys/mount.h` (`MNT_DONTBROWSE` "file system is
    not appropriate path to user data"), `sys/stat.h` (`UF_HIDDEN`, `SF_FIRMLINK`) — local man pages and headers
    (online copy of the bulk page: https://developer.apple.com/library/archive/documentation/System/Conceptual/ManPages_iPhoneOS/man2/getattrlistbulk.2.html)
18. getdents(2) — https://man7.org/linux/man-pages/man2/getdents.2.html
19. statx(2) — https://man7.org/linux/man-pages/man2/statx.2.html
20. SFTP v3, draft-ietf-secsh-filexfer-02 (§5 attributes, §6.11 REALPATH, §7 longname "SHOULD NOT … parse") —
    https://datatracker.ietf.org/doc/html/draft-ietf-secsh-filexfer-02
21. OpenSSH `PROTOCOL` (statvfs@ `f_fsid`/`f_flag`, expand-path@, limits@) —
    https://github.com/openssh/openssh-portable/blob/master/PROTOCOL
22. OpenSSH `sftp-server.c` (100 entries per READDIR reply; `process_extended_expand`: `~` and `~/` against the cwd,
    `~user` via `tilde_expand`; `-d` start directory `chdir`) and `sftp-common.c` (`ls_file` link count) —
    https://github.com/openssh/openssh-portable/blob/master/sftp-server.c ,
    https://github.com/openssh/openssh-portable/blob/master/sftp-common.c
23. sftp-server(8) `-d`: "The default is to use the user's home directory." — https://man.openbsd.org/sftp-server.8
24. Cache Directory Tagging Specification (`CACHEDIR.TAG`, 43-byte signature) — https://bford.info/cachedir/
25. D. E. Knuth, "Estimating the efficiency of backtrack programs", Mathematics of Computation 29 (1975) 121–136 —
    the random-probe tree-size estimator tried in §4.4.
26. sopen: `AGENTS.md`, `.prd/01-spec.md` §3a/§4, `src/remote/mod.rs` (`RemoteFs`, `Entry`, `entries_from`),
    `src/remote/sftp.rs` (`read_dir`, `realpath`, `free_space`), `src/remote/local.rs:87` (`list_entries`: `read_dir`
    + `lstat` per name), `src/gui/browser/panel.rs:128` (entered path = join of names).
