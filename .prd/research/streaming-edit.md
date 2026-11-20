---
title: sopen streaming edit — research, measured spike, design
type: research
tags: [sopen, streaming, edit, sftp, measurement]
---

# Editing a streamed file (2026-11-20)

Owner, verbatim (message 17, item 2 of [`../streaming/ssot.md`](../streaming/ssot.md)): "그리고 그냥 뷰어가 아니라
만약 편집할 경우는 다른데 이것은 별도로 리서치해서 처리해줘. 편집할경우는 그 편집된 부분은 전부 현재 로컬에 해당 내역이 모두
메모리 혹은 파일로 임시 저장해야하는거고 원하면 save as 로 로컬에 저장할 수 있게 해줘. (디폴트는 원격 편집 저장임)"

Fixed scope it answers (ssot item 2): every edit lives locally until saved; Save writes back to the remote by
default with §5's conflict check and backup; Save As writes the edited result to a local path; a small edit to a
huge remote file is not saved by re-uploading or re-downloading the whole file when a cheaper safe path exists.
Viewing (block cache, prefetch, the loopback HTTP range server for ffmpeg) is
[`streaming-view.md`](streaming-view.md)'s; this file assumes its `read_at(offset, len)` byte source and builds
editing on it.

## 0. Answer

1. **Edits never touch the file until Save, and they survive a crash.** Each editor keeps its edits as a small
   state: hex = the patch map (overwrite only, size never changes); image = the decoded image plus an op list;
   media = the edit list of source ranges. The state lives in memory, and a snapshot is written to an owner-only
   **journal** directory `<cache>/.edits/<host>/<abs>.journal-<stamp>-<rand>/` within 250 ms of every change. A
   journal whose session is gone (crash, kill) is offered on the next open: Restore / Later / Discard. Today a crash
   loses every unsaved hex and image edit (§1).
2. **Save (remote, the default) of a small edit to a huge file = a server-side copy, then a patch, then a rename.**
   The steps: `copy-data` (OpenSSH ≥ 9.0) into the temp sibling, write the changed bytes into the temp, then
   `posix-rename` over the target. That is §5's atomic replace, without moving the file over the network. Measured
   on a 24 GB file with a 4-byte patch: **6.3 s, 8 bytes of file content on the wire** (the spike read back the 4 edited bytes; the design reads back the 4 KiB page: ≤ 4 KiB + the patch). A full rewrite through the
   client moves 48 GB; at the measured 1.39 MB/s each way that is **≈ 4.8 h** (extrapolated from a measured 64 MiB
   run). In-place writes take 0.22 s but are not atomic, so they run only when the owner explicitly chooses them.
3. **The conflict check stays §5's, re-run right before the rename.** That is `(size, mtime)` at open = before the
   copy = after the copy = right before the rename. Under streaming no whole-file hash exists for a big file, so a
   new check takes its place: **the 4 KiB pages under each edit are compared with the bytes the editor showed**.
   Three refusals were measured: a stale baseline, a writer during the copy, and a same-size, same-mtime change of the
   edited bytes. In every one the target was untouched and no temp was left.
4. **Backup without copying 24 GB.** A hex edit only overwrites, so the original = the saved file with the replaced
   bytes written back. sopen keeps those bytes in a local **inverse patch** (a few hundred bytes for this edit). It
   was measured exact: reverting restored the original SHA-256 twice. Image Save keeps today's whole local backup, as
   the image is local anyway. A media render replaces the whole file, so its backup needs a decision; the recommended
   default is a remote hard link of the original (zero copy), named in the result (§7).
5. **Save As (local)** writes `<stem>.edited<ext>` (by default in the download folder) through the existing §6a
   machinery (`binary::save_new`: private staging folder, hard-link publish, never over anything). It streams the
   unchanged ranges from the remote, applies the edits in flight, shows progress and can be cancelled. 24 GB takes
   ≈ 1.1 h at 6.0 MB/s (32 reads in flight) and ≈ 3 h at sopen's current 1 read in flight. The window says that
   before starting.
6. **Media**: ffmpeg reads the remote source through the viewer's loopback range server (`feat/media-edit`'s `Input`
   already takes a URL). It renders into a local temp under `<cache>/.exports`. From there: Save uploads it through
   §5, Export through `write_new_sibling`, and Save As renders straight into the §6a staging file. The render's
   output is new bytes, so an upload of the output size cannot be avoided.
7. **Image**: decoding needs every byte, and **sopen has no decode-size limit today**. Its path
   (`into_decoder` → `DynamicImage::from_decoder`) skips the image crate's 512 MiB reservation, which only
   `ImageReader::decode()` makes. Measured with sopen's exact calls: 14000² RGB decodes, 588 MB buffer, 616 MB RSS.
   Proposed: sopen enforces **512 MiB decoded**, computed from the dimensions in the first 64 KiB, before any
   download. Above it the window says "too large to edit here" (view only). Within it, a file over 64 MiB asks first,
   with its size and estimated time; a smaller one downloads with progress, and Esc cancels.
8. **UI**: the status bar shows `unsaved — kept at <journal>`. Save (Cmd/Ctrl+S, F5) = remote. Save copy
   (Shift+Cmd/Ctrl+S) = a remote sibling, also by server copy. **Save As… (Alt+Cmd/Ctrl+S, F7)** = local. The close
   prompt becomes Save / Save As… / Discard / Cancel, and Discard deletes the kept edits.

Sections: §1 today · §2 measurements · §3 why copy-data, from the source · §4 design · §5 acceptance · §6 risks ·
§7 rule changes that need the spec/owner · §8 work units · §9 reproduce + sources.

## 1. What sopen does today (read in the code, `feat/streaming` at its branch point)

Every editor works on a complete local copy, and every Save writes a complete new file through the client.

| Fact | Where |
|---|---|
| A fetch downloads the whole file into the cache (`.part` file, renamed), records `(size, mtime)` and the SHA-256 of the fetched bytes. That hash is §5's content baseline. | `src/remote/mod.rs:443-515`, `src/cache.rs:327-369` |
| Reads go one 256 KiB SFTP request at a time, each awaited before the next is sent. | `src/remote/sftp.rs:31-32`, `:678-714` |
| **Hex** edits are a sparse in-memory map `offset → byte wanted` (absolute values) plus an undo deque of `(at, before, after)` steps, capped at 100 000. The cached copy is never written. | `src/gui/hex.rs:201-239`, `:31` |
| Hex is overwrite-only: "Editing only overwrites; the file's size never changes". §9 puts "hex insert / delete or anything that changes a file's size" out of scope, and AGENTS.md's edit list says "byte overwrite only". **Size-changing edits stay out of scope for streaming edit.** | spec `01-spec.md:498`, `:583`; `AGENTS.md:15`, `:45-48` |
| Hex Save makes one pass over every byte of the file as opened. The pass writes a full local backup on the session's first Save, plus a full staged copy with the patches applied (both under the cache), then `write_atomic` uploads the whole staged file. | `src/gui/hex.rs:861-937`, `:1089-1159` |
| `write_atomic` checks `lstat` (size, mtime), creates `.<name>.sopen-<random>` exclusively with the target's owner and mode, uploads, and fsyncs. Up to 256 MiB it also re-reads the **whole target** to compare its hash. It then re-checks and calls `posix-rename@openssh.com`. | `src/remote/mod.rs:21-23`, `:524-622`, `:745-809`; `src/remote/sftp.rs:526-613` |
| **Image** keeps the whole encoded original in memory (`Arc<Vec<u8>>`) and the decoded `DynamicImage`. Its undo stack is whole images, capped at 32 steps or 1 GiB. Save writes a full local backup, encodes, stages, then calls `write_atomic`. | `src/gui/image.rs:32-33`, `:38-58`, `:877-1000` |
| Image decode is `ImageReader::new` → `into_decoder()` → `DynamicImage::from_decoder()`. The default `Limits` (`max_alloc` 512 MiB) reach the decoder through `set_limits`, which for PNG and JPEG checks only width and height (unset by default). The 512 MiB reservation `limits.reserve(total_bytes)` happens only in `ImageReader::decode()`, which sopen does not call, and `from_decoder` allocates the whole buffer. **So no decoded-size limit applies today** (measured, §2.8). | `src/gui/image.rs:287-318`; image 0.25.10 `src/io/image_reader_type.rs:219-223`, `:311-322`, `src/codecs/png.rs:264-272`, `src/codecs/jpeg/decoder.rs:168-174`, `src/io/free_functions.rs:305-317` |
| **Media** (main) only exports: `ffmpeg` reads the local cached copy, and the render is published as a new sibling (`write_new_sibling`), never replacing. | `src/cut.rs:73-118`, `src/gui/media.rs:266-296` |
| **Media** (`feat/media-edit`, in flight) adds an edit list: timeline `Clip`s, rendered as `EditSpec { spans: [Span { file, start, len, frames }] … }`. Its Save copies the whole local original as a backup, renders with ffmpeg, then calls `write_atomic`. Its ffmpeg input type `Input` already accepts a URL (`local()` is `None` for `…://`). | that branch: `src/gui/media/timeline.rs:30-35`, `src/ffmpeg.rs:49-76`, `:776-806`, `src/gui/media/file.rs:55-85`, `:93-200`, `src/cut.rs:119-170` |
| `set_aside_unsaved` protects only edits written **into the cached copy** (by `--app`, or the terminal editor). Before a fetch it compares the copy's hash with its record and moves a differing copy aside. Hex and image edits are never in the copy, only in window memory. | `src/cache.rs:200-221`, `:327-369` |
| So **a crash, a kill or a power loss loses every unsaved hex and image edit today.** The close prompt (Save / Discard / Cancel; Save copy where Save is off) guards a normal close only. | `src/gui/hex.rs:2305-2341`, `src/gui/image.rs:1901-1934` |
| Keys: Cmd/Ctrl+S and F5 = Save; **Shift+Cmd/Ctrl+S = Save copy** (a new remote sibling `<stem>.edited<ext>`). The F-key rows show F5 Save (hex also has F6 Goto and F8 Asc/hx). | `src/gui/hex.rs:1862-1874`, `src/gui/image.rs:1614-1621`, `src/gui/chrome.rs:100-119`; spec `01-spec.md:523-525` |

Cost of today's hex Save of a 4-byte patch to a 24 GB remote file:

- Opening already downloaded 24 GB.
- The first Save writes 48 GB on the local disk (the backup and the stage).
- It then uploads 24 GB.

That is 48 GB over the network in all, the same as strategy (a) in §2.3, plus 48 GB of local disk. At the link
measured in §2 (2.2 MB/s each way), each 24 GB leg takes about 3 h.

## 2. Measurements (2026-11-20)

### 2.1 Setup

| | |
|---|---|
| Client | The Mac (macOS, 128 GiB RAM), running a throwaway spike crate. The crate uses sopen's own client stack: `openssh` 0.11.6, `openssh-sftp-client` 0.15.12 and 256 KiB requests, each awaited, as `src/remote/sftp.rs` does. The server's `limits@openssh.com` clips each read to 261 120 bytes (streaming-view.md §2.2), hence 1029 requests per 256 MiB below. The crate is not in the repo; its request sequences are reproduced in §9. |
| Remote | The Linux laptop: Linux 7.2, OpenSSH 10.5p1 `sftp-server`, reached with `ssh -o BatchMode=yes <linux-laptop>` over an overlay VPN. Test files only under `/tmp/sopen-stream-spike/edit/`, which is **tmpfs**; the directory and everything in it was deleted afterwards. |
| Link | `ping -c 10`: rtt min/avg/max 31.7 / 54.1 / 78.2 ms during these runs. The same link measured 12/21/40 ms and 3.7–10 MB/s at another hour (streaming-view.md §2.1). **Every link-bound time here scales with the link**; the bytes moved do not. |
| Second server | The Mac's own `/usr/libexec/sftp-server` (OpenSSH 10.3p1) on pipes, as `SftpRemote::local_server()` does in tests (`src/remote/sftp.rs:732-768`), over a file on APFS. |

Extensions offered (`spike … ext`; on the Linux laptop also `sftp -vvv`):

```
linux-laptop  copy-data=true posix-rename=true fsync=true hardlink=true statvfs=true
              (sftp -vvv also lists lsetstat, limits, expand-path, users-groups-by-id)
the Mac       copy-data=true posix-rename=true fsync=true hardlink=true statvfs=true
```

### 2.2 The link: what moving bytes through the client costs

```
down 256 MiB, 1 request in flight   down bytes=268435456 par=1  requests=1029 ms=121155 MB/s=2.2
down 256 MiB, 8 in flight           down bytes=268435456 par=8  requests=1032 ms=94426  MB/s=2.8
down 256 MiB, 32 in flight          down bytes=268435456 par=32 requests=1056 ms=44401  MB/s=6.0
up    64 MiB, 1 writer              up bytes=67108864 par=1 write_ms=30208 fsync_ms=12 total_ms=30238 MB/s=2.2
up    64 MiB, 8 writers             up bytes=67108864 par=8 write_ms=17955 fsync_ms=22 total_ms=17998 MB/s=3.7
```

With one request in flight (sopen today), throughput is set by the round trip: 256 KiB every ~119 ms ≈ 2 RTT.
More requests in flight help (×2.7 for downloads at 32), which matters for Save As and renders (§4.3–4.4).

### 2.3 Strategy (a): full rewrite through the client

The same §5 sequence, but every byte goes through the client. It reads the target sequentially, writes the temp
with the patch applied in flight, fsyncs, re-checks, then renames. The 64 MiB run was measured; 24 GB is
extrapolated.

```
fullrewrite size=67108864 bytes_down=67108864 bytes_up=67108864 transfer_ms=48401 fsync_ms=18 total_ms=48650 MB/s(each way)=1.39 renamed=true
$ od -An -tx1 -j 33554433 -N 4 mid.bin      →  de ad be ef
```

For 24 GB the bytes moved are exact: 24 GB down and 24 GB up. The time is extrapolated:
24·10⁹ / 1.39·10⁶ B/s ≈ 17 300 s ≈ **4.8 h**. The best overlapped case with the pipelined rates above is still
max(24·10⁹/6.0·10⁶, 24·10⁹/3.7·10⁶) ≈ 6 500 s ≈ **1.8 h**. At the 10 MB/s the link reached at another hour it
would still be ≈ 40 min each way. Today's hex Save (§1) moves the same 48 GB, as the fetch at open plus the upload,
and adds 48 GB of local disk writes.

### 2.4 Strategy (b): `copy-data` on the server, then patch, then posix-rename — 24 GB, 4-byte patch

The remote file was 24 000 000 000 bytes from `/dev/urandom`, original SHA-256 `a67cd114…ce7b12`. Shown bytes at
offset 12 345 678 901: `da 94 45 a7`; the patch was `de ad be ef`. The copy used 256 MiB per request, with probes
(a `stat` on a second channel every 200 ms, and one `stat` on the save's own channel 300 ms in):

```
statvfs free=26778021888 need=24000000000
probe other-channel stats=25 worst_ms=80.4
probe same-channel stat issued_at=300ms waited_ms=46.7
copysave size=24000000000 chunk=268435456 copy_requests=90 copy_ms=5778 server_MB/s=4154 worst_request_ms=168 setup_ms=94 fsync_ms=18 rename_ms=49.1 total_ms=6300
copysave payload: file_bytes_over_wire=4 (patch) + 4 (range read-back) backup(old)=da9445a7 new=deadbeef renamed=true after=(24000000000,1795142144)
$ od -An -tx1 -j 12345678901 -N 4 model.bin; stat -c "size=%s mtime=%Y mode=%a" model.bin; ls -a
 de ad be ef
size=24000000000 mtime=1795142144 mode=644
.  ..  mid.bin  model.bin  small.bin                      ← no temp left
```

The sparse backup was then written back in place, and the result hashed:

```
inplace size=24000000000 off=12345678901 backup(old)=deadbeef new=da9445a7 write_ms=32.7 fsync_ms=24.1 total_ms=255.2 bytes_up=4 bytes_down=4 …
$ sha256sum model.bin  →  a67cd11442364e070a355a81bb40de75500bcce4ff5299ce43054ac7a0027b12   (= the original)
```

This shows two things. The server-side copy is byte-exact, and the 4 bytes read back before patching restore the
original completely.

One request for the whole file (`copy_all_to`), temp removed instead of renamed:

```
probe other-channel stats=15 worst_ms=60.2
probe same-channel stat issued_at=300ms waited_ms=3140.3
copysave size=24000000000 chunk=0 copy_requests=1 copy_ms=3443 server_MB/s=6970 worst_request_ms=3443 …
```

A copy request holds its channel until it is done: a `stat` on the same channel waited 3.1 s. A second channel to
the same server was unaffected (worst 60–80 ms ≈ one RTT). So a save runs on a channel of its own, in bounded
requests.

**These copy rates are memory speed, not disk speed.** `/tmp` on the Linux laptop is tmpfs. The Mac runs below
copied a file that had just been written, so it sat in the page cache (fsync 1–2 ms). A disk-backed rate was not
measured. §4.2 bounds every request by time for that reason.

```
the Mac, 4 GiB file, 1 GiB requests   copysave size=4294967296 chunk=1073741824 copy_requests=4 copy_ms=1176 server_MB/s=3654 worst_request_ms=339 fsync_ms=1 …
                                      probe other-channel stats=6 worst_ms=0.1 · probe same-channel stat issued_at=300ms waited_ms=303.7
the Mac, 4 GiB file, one request      copysave size=4294967296 chunk=0 copy_requests=1 copy_ms=1138 server_MB/s=3775 …
```

### 2.5 Strategy (c): in-place ranged write

The spike opened the target read-write (no truncate), read the old bytes (the sparse backup), wrote the new ones and
fsynced:

```
inplace size=24000000000 off=12345678901 backup(old)=da9445a7 new=deadbeef write_ms=59.3 fsync_ms=15.0 total_ms=219.7 bytes_up=4 bytes_down=4 after=(24000000000,1795142118)
revert:  … backup(old)=deadbeef new=da9445a7 write_ms=11.4 fsync_ms=15.0 total_ms=125.3 …
$ sha256sum model.bin  →  a67cd114…ce7b12   (= the original)
```

This is the cheapest strategy, but it gives up atomicity (§4.2, table).

### 2.6 Strategy (d): `cp --reflink` on the host

This was run only for comparison: it needs a remote shell, which AGENTS.md rules out ("file operations are SFTP"):

```
$ cp --reflink=always model.bin .r1   →  cp: failed to clone '.r1' from 'model.bin': Operation not supported   (tmpfs)
$ cp --reflink=auto   model.bin .r2   →  exit=0 ms=3103 (a full copy; size 24000000000)
```

**Reflink was not measured.** The test files had to stay in `/tmp`, which is tmpfs and cannot clone; the Linux
laptop's other filesystems are btrfs, which can. Even where it can, sftp-server's `copy-data` does not clone (§3).
On the SFTP route the filesystem axis therefore reduces to **free space for one temp of the file's size** and the
**server disk's speed**.

### 2.7 Conflicts (each refused; the target untouched; no temp left)

```
C1 baseline from an earlier open:          REFUSED step=lstat-before-anything reason=changed-since-open open=(67108864,1795142007) now=(67108864,1795142067) created=nothing   spike_exit=3
C2 another writer changes byte 0 after
   copy request 3 (of 90):                 REFUSED step=recheck-before-rename reason=changed source_now=(24000000000,1795142188) target_now=(24000000000,1795142188) open=(24000000000,1795142156) temp=removed
                                           $ od … model.bin → da 94 45 a7 ; ls -a → no temp
C3 a writer overwrites the edited bytes
   and puts the mtime back:                mutated off=12345678901 bytes=11223344 before=(24000000000,1795142188) after=(24000000000,1795142188)
   (b)                                     REFUSED step=edited-range reason=bytes-differ shown=da9445a7 now=11223344 temp=removed
   (c)                                     REFUSED step=edited-range reason=bytes-differ shown=da9445a7 now=11223344
```

C3 is the case a `(size, mtime)` check cannot see; only the edited-range check caught it. A same-size change
elsewhere with the mtime put back is not caught by any of (a)–(c) under streaming (§6).

### 2.8 Image decode size (the Mac; image 0.25.10 with sopen's features)

**sopen's own calls** (`src/gui/image.rs:288-318`): `ImageReader::new` → `with_guessed_format` → `into_decoder` →
`icc_profile` / `exif_metadata` / `orientation` → `DynamicImage::from_decoder`:

```
11000x11000.png  (RGBA)  file_bytes=14275684 header_dims=Some((11000, 11000)) decode=ok 11000x11000 Rgba8 buffer_bytes=484000000 decode_ms=532   max RSS 500662272
12000x12000.png  (RGBA)  file_bytes=15195639 header_dims=Some((12000, 12000)) decode=ok 12000x12000 Rgba8 buffer_bytes=576000000 decode_ms=663   max RSS 593608704
13000x13000.jpg          file_bytes=10413965 header_dims=Some((13000, 13000)) decode=ok 13000x13000 Rgb8  buffer_bytes=507000000 decode_ms=201   max RSS 531464192
14000x14000.png  (RGB)   file_bytes=18333398 header_dims=Some((14000, 14000)) decode=ok 14000x14000 Rgb8  buffer_bytes=588000000 decode_ms=920   max RSS 608681984
14000x14000.jpg          file_bytes=11881412 header_dims=Some((14000, 14000)) decode=ok 14000x14000 Rgb8  buffer_bytes=588000000 decode_ms=351   max RSS 615907328
```

**For contrast, `ImageReader::decode()`** on the same files (a first probe used it by mistake): 11000² and 13000²
decode; 12000² RGBA, 14000² PNG and 14000² JPEG fail at once with `Memory limit exceeded` (`decode_ms=0`). That
function reserves `total_bytes` against `max_alloc` (image `src/io/image_reader_type.rs:311-322`); sopen's path
does not. streaming-view.md §2.4's "Memory limit exceeded" for a 100000² header also used `image::decode`.

`header_dims` comes from the first 64 KiB only, so a size budget can be checked before any pixel data arrives. The
test images are synthetic and compress well; photographs of the same size are larger files. WebP, BMP, TIFF and GIF
were not run.

## 3. Why `copy-data`, from the source

- **What it is.** It is one SFTP request, "copy `len` bytes from handle A at offset X to handle B at offset Y". In
  OpenSSH 10.5p1 (`sftp-server.c` v1.156, tag `V_10_5_P1`) it is registered at `:164` and advertised at `:725`; the
  handler is `:1603-1712`. The handler reads and writes in a loop through a 64 KiB stack buffer (`:1605`, `:1684`,
  `:1696`) and sends one status reply at the end. It refuses the same handle, the same path or the same inode for
  source and destination. **No `copy_file_range`, no reflink**: the server's disk does a full copy.
- **Since when.** It is absent from `V_8_9_P1` (`sftp-server.c` v1.139) and present from `V_9_0_P1` (v1.140,
  2022-03-31). Servers on OpenSSH < 9.0 do not offer it. [estimate] That includes several long-term-support
  distributions still in use; their versions were not checked here.
- **Client.** `openssh-sftp-client` 0.15.12, the version sopen already uses, has `Sftp::support_copy()`
  (`src/sftp.rs:437-442`) and `File::copy_to(dst, n)` / `File::copy_all_to(dst)` (`src/file/mod.rs:650-741`). Each
  side's offset is set with `AsyncSeek::start_seek` (`:749-760`, a local offset carried in the request). Positional
  writes work the same way (`write` at `self.offset`, `:434-466`), so patching ranges of the temp needs no new
  dependency.
- **One request at a time per channel.** The server's main loop handles one request at a time
  (`sftp_server_main` → `process()`, `:2053-2123`). So a long copy request holds its channel. Measured in §2.4: the
  save's own channel waited 3.1 s, while a second channel saw no delay.

## 4. Design

### 4.0 What this assumes from streaming viewing

[`streaming-view.md`](streaming-view.md) was in progress when this was written: its measurements (§1–§3) were in;
its design sections (§4–§10) were still empty. What its measurements already fix, and this design uses:

- Reads are clipped to 261 120 bytes; throughput depends only on the bytes in flight, saturating the link from
  about 256 KiB in flight.
- `sftp-server` answers in order, so a second channel keeps demand reads fast, and **SFTP has no cancel**: a dropped
  request still runs on the server.
- `Sftp::close()` waits for every live `File`.
- An open handle keeps reading the file it opened across a rename-over, while an in-place change shows through the
  handle (`fstat(handle)` sees it).
- Channels per host are a budget: `MaxSessions` defaults to 10.

What this design needs from it, still assumptions until its §4 lands:

1. `read_at(off, len)` over a source opened as `(host, abs, size, mtime)`. It reads through a bounded block cache and
   **fails with a distinct "changed" error** once the source's `(size, mtime)` differs from the open baseline. That
   is checked at least once per batch of blocks, so a render or a Save As never mixes two versions of the file.
2. Whole-file streams (Save As, a render, an image fetch) keep ≥ 256 KiB in flight on a channel of their own.
3. The loopback HTTP range server gives ffmpeg a URL per source (`Input` on `feat/media-edit` takes it). A response
   fails, rather than serving bytes, when assumption 1 trips.
4. A Save takes one more channel from the per-host budget, for its duration. §3: a copy request holds its channel.
   The save drops every `File` before closing that channel.
5. A file the viewer read whole (small files) still gets today's whole-content SHA-256. §4.2 uses it when it exists.

### 4.1 The edit journal (every editor)

**Memory** holds the working state, exactly what the windows hold today:

| Editor | State in memory | Bound | Size-changing? |
|---|---|---|---|
| Hex | `patches: BTreeMap<u64, u8>` (absolute bytes) + undo `(at, before, after)` | 100 000 undo steps (`hex.rs:31`); a step is one typed nibble or character, so patch sets are human-sized | No — overwrite only (§9); stays out of scope |
| Image | the decoded image + undo of whole images | decode ≤ 512 MiB (§2.8), undo ≤ 32 steps / 1 GiB (`image.rs:32-33`). The encoded original moves from `Arc<Vec<u8>>` (`image.rs:877-887`) to the local fetched file, read when needed (backup, metadata, encode) | Crop changes it; it is a re-encode anyway |
| Media (`feat/media-edit`) | timeline `Clip`s + Mono→Stereo + crop + In/Out | 512 undo steps (`timeline.rs` `HISTORY`), each a short clip list | Render = new file |

**On disk.** Each editing session gets one journal directory, created on the first edit (not at open). It is
owner-only (0700, files 0600, created exclusively — `cache::create_unique`'s rules):

```
<cache>/.edits/<host>/<abs path>.journal-<UTC stamp>-<8 hex>/
  head     kind (hex | image | media), host, abs path, size and mtime at open, sopen version, created
  seen     hex: per edited 4 KiB page, its offset and the SHA-256 of the bytes the editor showed (read when the
           page was first edited); image/media: the SHA-256 of the whole fetched file when known
  state    the current edit state, replaced atomically: write state.tmp-<rand>, fsync, rename over state
           hex:   the .hexpatch text sopen already writes (hex.rs:1029-1057: header, one "0xOFF: HEX" line per run),
                  plus the bytes shown before each run ("0xOFF: NEW was OLD")
           image: one line per op — crop x,y,w,h · pixelize x,y,w,h,block · cut x,y,w,h,fill · paste x,y,w,h,mode,
                  paste-N.png · text x,y,w,h,<escaped text> — replayed in order on the decoded original
           media: the clip list in ticks, stereo, crop, In/Out
  paste-N.png   image only: each pasted picture, written once when pasted
  lock     flock held while the session lives (what cache::Claim does with .meta/lock)
```

- **When it is written.** Within 250 ms of a change (debounced), at focus loss, and before any Save starts. A typical
  hex `state` is a few hundred bytes, so an fsync per snapshot is cheap.
- **Why `.edits`.** It is outside the mirrored tree, so no fetch ever writes it or moves it aside.
  `set_aside_unsaved` (`cache.rs:200-221`) keeps its job for edits made into the cached copy (`--app`, the terminal
  editor).
- **Removed when** the window closes with nothing unsaved, meaning the state is written to the remote, a Save copy
  or a Save As. Also on Discard.
- **Kept when** the window closes any other way (crash, kill, power loss). Then the lock is free.

**Crash recovery.** On open, sopen looks for `…/<abs path>.journal-*` directories whose lock it can take, of the
same kind as the window. For each one, the disclosure rows say: `unsaved edits from 2026-11-20 11:42 (4 bytes
changed) kept at <path> — Restore (R) · Later · Discard`.

- **Restore.** sopen re-stats the remote. If `(size, mtime)` equals the journal's `head`, it applies the edits. If
  the remote changed, it re-reads only the `seen` pages (ranged reads) and compares them. If they match, it applies
  the edits with a note ("the file changed elsewhere since; the bytes under your edits did not"). If they differ,
  nothing is applied and the journal stays, its `state` readable as text.
- **Later.** Leaves it.
- **Discard.** Removes it.
- **Another kind.** A journal of another kind (the same file opened as hex earlier) is named in one row, never
  applied.

### 4.2 Save to the remote (the default)

**Route by what the server offers.** Decided before anything is written:

| Server | Free space in the target's folder (`statvfs`) | Hex Save | Hex Save copy |
|---|---|---|---|
| `posix-rename` + `copy-data` | ≥ size + 1 GiB (or no `statvfs`) | **(b)** server copy → patch → rename | (b) without rename, published by hard link as `<stem>.edited<ext>` (`write_new_sibling`'s rule) |
| `posix-rename`, no `copy-data` | ≥ size + 1 GiB | size ≤ 256 MiB (`HASH_CHECK_MAX`): **(a)** streamed (unchanged ranges from the block cache, else `read_at`). Larger: **refused before any transfer**, with the cost stated: "rewriting 24 GB through this machine moves 48 GB (≈ N h at the current rate)". The window then offers Write in place… (c) and Save As… | (a) to a new sibling, same size rule |
| `posix-rename` | < size + 1 GiB | No atomic route exists (a temp of the file's size does not fit): refused with "needs 24.0 GB free in <dir>, has 3.1 GB". Offers Write in place… and Save As… | refused the same way |
| no `posix-rename` | — | Save off, as today (`CANNOT_REPLACE`) | as above |

**(b), step by step.** Each step is measured in §2.4 and §2.7. The save runs on its own SFTP channel and every
request has a timeout:

1. `lstat` the target: a regular file whose `(size, mtime)` equals the open baseline, with all attributes present.
   Otherwise refuse, having created nothing (C1).
2. Run `statvfs` on the folder and refuse early when there is no room.
3. Open the source for reading. `fstat` the handle against the baseline; the copy reads from this handle, so a
   rename-over after this point is caught at step 9.
4. Create `.<name>.sopen-<random>` exclusively. Through its handle: the owner, then the mode without set-id
   (`create_from`'s rule, `remote/sftp.rs:556-576`).
5. `copy-data` source → temp in requests sized by time:
   - **Request size.** Start at 64 MiB, then size each next request so it takes about 1 s at the last request's rate
     (clamped to 16 MiB–1 GiB). A slow disk then never turns one request into a 30 s `META_TIMEOUT`, which would be
     `OutcomeUnknown`.
   - **Periodic fsync.** Every 2 GiB copied, send `fsync@openssh.com` on the temp, so the final fsync never meets a
     whole file of dirty pages. sopen's fsync limit is the 60 s `STALL_TIMEOUT` (`remote/sftp.rs:599-603`). The
     Linux laptop caps dirty data at `vm.dirty_bytes` = 256 MiB, but the kernel default is 20 % of RAM.
   - **Progress and cancel.** Progress is bytes copied / size. Cancel is checked between requests: the temp is
     removed and the target is untouched. SFTP has no cancel, so a request already sent finishes on the server
     first. Time-sized requests bound that wait to about 1 s.
6. **Edited-page check.** Read each edited 4 KiB page back from the temp copy and compare its SHA-256 with `seen`. A
   difference is refused and the temp removed (C3).
7. **Backup.** The bytes each run replaces, from those same reads, go into the local inverse patch
   `<cache>/.backup/<host>/<abs>.<stamp>.undo.txt` (0600; header = source, size and mtime as opened, the
   `.hexpatch` line format; then `0xOFF: OLD` per run). It is fsynced **before** the temp is patched, and its path is
   printed (today's "backup of … at …").
   - **One undo file per session, holding the file as opened.** That matches today's backup, which is written once
     per session (`hex.rs:1096-1106`).
   - **Later Saves add to it.** Each later Save in the session adds the offsets it changes for the first time.
     Offsets already in the file keep the bytes recorded first, so the file is rewritten atomically (temp + rename)
     and never overwritten in place.
   - **What writing it back restores.** Writing the whole undo file back over the saved file gives the file as
     opened, whatever number of Saves came between.
8. Write each run at its offset (positional writes). Restore the full mode, `fsync@openssh.com`, and `fstat` the
   temp: size = size as opened, and owner and mode as wanted (`prepare_swap`'s checks, `remote/mod.rs:745-787`).
9. **Re-check immediately before the replace.** `fstat` the source handle and `lstat` the target, both against the
   baseline; any difference is refused and the temp removed (C2). If the whole-content SHA-256 exists (assumption
   5) and the file is ≤ 256 MiB, compare it too, by reading the temp. That keeps §5 for small files.
10. `posix-rename` the temp over the target, then `lstat` after (today's "saved, but the remote changed right after"
    note). The next baseline = the temp's stat from step 8. The journal's `seen` hashes are re-taken from the bytes
    now saved.

The failure rules are §5's, unchanged. A refused or failed Save keeps the journal and names it. A request with no
reply is `OutcomeUnknown`: "check <path>", never "nothing was written". A temp is removed only when this save
created it.

**(a) streamed** follows the same steps, except steps 3–6 become "stream every byte through the client": `read_at`
for ranges the block cache does not hold, the patch applied in flight, written into the temp. It is used only where
(b) is unavailable, under the size rule above.

**(c) Write in place…** happens only on an explicit dialog, never as a fallback:

> Write the 4 changed bytes into host:/abs in place? **This is not atomic.** If the connection drops while writing,
> the file may hold only some of the changes. Programs reading it meanwhile see the changes as they land, and every
> hard link to it changes too. The original bytes are kept at <backup path>.

Sequence: steps 1–3 (open read-write, no truncate) → read each edited page from the target, compare with `seen`
(C3), write the inverse patch and fsync it → write the runs → `fsync` → `lstat` (the new mtime becomes the
baseline). A write or fsync with no reply is `OutcomeUnknown` **on the target itself**: "<file> may hold some of
your changes — check it; the original bytes are at <backup>".

| | (a) streamed rewrite | (b) server copy + patch | (c) in place | (d) `cp --reflink` over ssh |
|---|---|---|---|---|
| File bytes over the network, 24 GB + 4-byte patch | 48 GB (24 down + 24 up) | 4 + 4 KiB read-back per edited page | 4 + 4 | 4 (+ a shell) |
| Time measured here | 64 MiB: 48.6 s → 24 GB ≈ 4.8 h (extrapolated) | **6.3 s** (tmpfs; disk unmeasured) | 0.22 s | 3.1 s full copy (tmpfs; reflink unmeasured) |
| Server disk written | 24 GB | 24 GB (a read/write loop, §3) | 4 B | 0 with reflink, else 24 GB |
| Needs free space on the server | size | size | none | 0 with reflink, else size |
| Atomic for readers and crashes | yes (rename) | yes (rename) | **no**: torn on connection loss, multi-run patches land one by one | yes, if run as copy + rename |
| Keeps the inode (open handles, `mmap` users, hard links, xattrs/ACLs) | no — new inode | no — new inode | yes — a running process mapping the file sees the bytes change under it | no |
| Rules | §5 as today | §5 as today | new: needs the owner (§7) | breaks "file operations are SFTP" and needs GNU `cp` (macOS `cp` clones with `-c`; BusyBox cannot) — rejected |

**What replaces §5's hash baseline.** Today it is the SHA-256 of the whole fetch, re-checked against the whole
target up to 256 MiB (`remote/mod.rs:788-804`). Under streaming nobody fetches a big file whole, so:

- **≤ 256 MiB and read whole by the viewer:** as today.
- **Otherwise:** `(size, mtime)` at four points (open, before the copy, after the copy, before the rename) plus the
  edited pages against what the editor showed. The result line says so: "checked size, modification time and the
  8 KiB under your edits, not the whole content". This is stronger than today above 256 MiB, which checks size and
  mtime only.
- **A same-size change elsewhere with the mtime put back** is not detected. With (a), (b) and (c) under streaming it
  is **kept in the saved file**: the patch lands on the file as it is now, where today's whole-copy rewrite would
  overwrite it. This is a semantic change; see §7.

**Backups by editor.**

| Editor | Backup | Why |
|---|---|---|
| Hex | the local inverse patch (step 7) | The edit only overwrites, so original = saved file + inverse patch. Measured exact: SHA-256 restored twice (§2.4, §2.5). It costs bytes, not gigabytes. |
| Image | today's whole local backup (`image.rs:909-936`) | The whole file is local anyway (decoding needs it), and it is within the decode budget. |
| Media Save (`feat/media-edit`) | **owner decision (§7).** Recommended: `hardlink@openssh.com` from the original to `.<name>.sopen-backup-<stamp>` in the same folder right before the rename. That is zero bytes copied, and the original inode survives under that name; the result line names it and its size ("the original is kept on <host> as … (24.0 GB); delete it when no longer needed"). Without hard links, a local full backup streamed with progress (asked first above 1 GiB), or Save off and Export offered. | A render replaces every byte, so no inverse patch exists. A local backup of a remote 24 GB video is a 24 GB download. |

### 4.3 Save As (local)

- **Where.** A modal with one path field, prefilled with `<download folder>/<stem>.edited<ext>`. The next free
  `-N` name is shown when that one is taken (`binary::suggest`, `binary::next_free`, `binary.rs:152-237`). It also
  has Browse… (the system save dialog, `binary::NativeChooser`), and it shows the size and the estimated time at the
  rate the viewer is getting.
- **How.**
  1. Refuse early when the chosen folder has less free space than the file.
  2. Write through `binary::save_new(dest, fill)` (`binary.rs:533-541`): the private staging folder `.sopen-<16 hex>`,
     `part` created exclusively, a hard-link publish that fails if anything is at the name — never over anything,
     never through a link. `binary.rs` is used, not changed.
  3. `fill` streams the source in large ranges with several reads in flight, reusing blocks the cache holds. It
     applies the edits in flight and hashes as it goes.
     - Hex: the journal's patches.
     - Image: the encoder's output.
     - Media: ffmpeg rendering into `part` with an explicit `-f <muxer>`, since `part` has no extension.
  4. After the transfer, the source's `(size, mtime)` must still equal the open baseline, and the edited pages
     streamed must equal `seen`. Otherwise it is not saved and `part` is removed (§6a: "a download whose source
     changes meanwhile is not saved").
- **Progress, cancel, result.** Progress shows bytes and the ETA. Esc or Cancel stops it, and only sopen's own
  staging folder is removed. The result line is `saved <local path>`, also printed on stdout.
- **Cost.** 24 GB = 24 GB downloaded: ≈ 1.1 h at 6.0 MB/s (32 in flight) and ≈ 3 h at 2.2 MB/s (§2.2). The modal
  says so before Enter.
- **What it counts as.** A completed Save As writes the state somewhere, so it counts for the close prompt as Save
  copy does (`HexDoc::mark_copied`, `hex.rs:538`).

### 4.4 Media: edit list rendered from a remote source

**Design only — nothing in this subsection was measured.** It depends on the range server that streaming-view.md
§4 is still writing, and on `feat/media-edit` merging.

- **Input.** ffmpeg reads `http://127.0.0.1:<port>/<token>/<name>` from the viewer's range server, through
  `feat/media-edit`'s `Input` (`ffmpeg.rs:49-76`), which already carries a URL. [estimate] The render reads what
  the spans need, plus the container index and the 0.2 s preroll (`EDIT_PREROLL_S`, `ffmpeg.rs:893`). How many
  bytes ffmpeg actually requests per span over HTTP is the view work's measurement to make; a container whose index
  sits at the end (`moov` last) costs one more far read.
- **Output.** A local temp under `<cache>/.exports/<random>/` (`cache::exports_root`, 0700). Before starting, sopen
  checks local free space against an estimate (bitrate × output length).
- **Save.** Run media-edit's pre-render check (`media/file.rs:115-140`), render, then `write_atomic` (§5, unchanged)
  with the backup decided in §7. The upload is the output's size; that cannot be avoided, since the content is new.
- **Export.** `write_new_sibling`, unchanged.
- **Save As.** Render straight into the §6a `part`.
- **Source changes mid-render.** The range server fails the response (assumption 1); ffmpeg fails; nothing is
  uploaded or published, the journal stays and the window says why.

### 4.5 Image: what may be downloaded whole

- **Budget (new; sopen has none today, §1 and §2.8).** Decoded bytes = width × height × bytes per pixel of the
  colour type the decoder will produce. That is computable from the first 64 KiB (measured, §2.8) and checked by
  sopen itself before the download, not left to the image crate.
  - **Proposed limit: 512 MiB.** That is the crate's own default figure. It keeps an editing session within about
    2 GiB: the image, the next image during a crop or pixelize, and the existing 1 GiB undo cap (`image.rs:32-33`).
    Today a 14000² RGB image decodes to 588 MB and is edited, using 616 MB RSS before any undo, so this limit stops
    something that works now. [estimate] It is reversible: one constant.
  - **Above the limit,** before any download: `too large to edit here: 14000×14000 needs 588 MB to decode (limit
    512 MiB) — view only`.
  - **The limit's value is the owner's call** (§7 item 8). It trades memory against what can be edited.
- **Within the budget and not yet local:**
  - **File ≤ 64 MiB:** the first edit gesture starts the whole fetch, with the status bar saying `editing needs the
    whole image — downloading 14.3 MB (37 %) — Esc cancels`. Edits apply once it is decoded. At the measured 2.2–6.0
    MB/s, 64 MiB takes 11–30 s.
  - **File > 64 MiB:** one confirm first: `Editing needs the whole file: 312 MB, about 2 min at 2.6 MB/s — Download
    (Enter) · Cancel (Esc)`.
  - **Either way:** the fetch goes into the claimed cached copy (§5's fetch rules), and that copy is kept (pinned
    against the view cache's eviction) while the image has unsaved edits.

### 4.6 UI contract (hex, image, media windows)

| Element | Behaviour |
|---|---|
| Dirty state | Today's content-based `unsaved` (hex: `HexDoc::unsaved`, `hex.rs:521-536`; image: `rev` vs `saved_rev`/`copied_rev`). Once the journal holds the state: `unsaved — kept at <journal dir>` (a `~`-shortened path, with the whole path on hover). If the snapshot cannot be written: `unsaved — NOT kept on disk (<why>)` in the error colour; the close prompt still guards it. |
| Save | Cmd/Ctrl+S and F5 (unchanged), to the remote by the §4.2 route. Progress `saving — copying on <host> 37 % (8.9 of 24.0 GB)` with Cancel. Result: `saved host:/abs · backup <path>` plus notes. |
| Save copy | Shift+Cmd/Ctrl+S (unchanged): a remote new sibling, also by server copy when offered. |
| **Save As…** | **Alt+Cmd/Ctrl+S, and F7 "SaveAs" in the F-key row** (hex today: F5, F6, F8; image and media: F5 — `chrome.rs:100-125`). No viewer row uses F7 in any current worktree. The file-list row proposal gives F7 to MkDir (`feat/git-panel` `.prd/research/keys.md` §0), and the viewer rows already reuse 5/6/8 with other meanings, as PC Shell's bar does. Opens the §4.3 modal. |
| Write in place… | Not on a key. Offered only in the refusal rows when §4.2 has no atomic route, behind the §4.2 dialog. |
| Close / leave (window close, Esc out of a browser viewer) | `Save the changes to host:/abs before closing?` → **Save** · **Save As…** · **Discard** · **Cancel**. Save copy replaces Save where Save is off, as today. Save As… opens the modal; closing follows only a save that landed. Discard's hover says it deletes the kept edits at <path>. Today's rules stay: closing during a save waits; a save that did not land keeps the window. |
| Recovery | §4.1 rows on open: Restore (R) · Later · Discard. |

## 5. Acceptance scenarios

Every remote scenario runs against an SFTP server with real `copy-data`. In tests that is `SftpRemote::local_server()`
(`remote/sftp.rs:732-768`; CI already requires `sftp-server` via `SOPEN_REQUIRE_SFTP_SERVER`). For the "no
copy-data" rows it is a `LocalFs` or SFTP wrapper that reports the extension absent. The live receipt is a file under
a remote `/tmp/sopen-receipts/`.

1. **Small patch, huge file.** Open a remote 24 GB file in the hex window, overwrite 4 bytes at 0x2DFDC1C35 and press
   Save.
   - File bytes sent and received ≤ 4 KiB per edited page plus the patch. Nothing else of the file is downloaded or
     uploaded: count the client's read and write payloads.
   - Local disk written ≤ 64 KiB: journal, inverse patch.
   - No copy request lasts longer than 5 s.
   - Afterwards: the remote holds the new bytes; writing the inverse patch back gives the original SHA-256; no
     `.sopen-` temp is left; the window says `saved … · backup <path>`; the journal is gone once nothing is unsaved.
2. **Viewer keeps working during a save.** While scenario 1 copies, scroll the hex grid. Every viewer read is
   answered within RTT + 250 ms; the save is on its own channel.
3. **Changed since open.** Change the remote's mtime before Save. Refused at the first `lstat`: nothing created on
   the remote, the edits still in the window, the journal path named.
4. **Writer during the copy.** Write one byte of the source after the third copy request. Refused at the re-check
   before the rename, the temp removed, the target byte-identical to before.
5. **Same size, mtime put back, edited bytes changed.** Refused at the edited-page check: `shown … now …`.
6. **No room.** `statvfs` reports less than size + 1 GiB. Save is refused before any request that writes, naming
   the needed and the free bytes and offering Write in place… and Save As….
7. **No copy-data, big file.** With a server without `copy-data` and a 300 MiB file, Save is refused before any
   transfer, stating the (a) cost. Write in place… requires the dialog's explicit button; after it, the remote holds
   the patch and the inverse patch exists locally.
8. **No copy-data, small file.** With a 1 MiB file, Save runs (a). The result matches scenario 1's content checks,
   and the upload is ≤ 1 MiB plus protocol overhead.
9. **Crash.** `kill -9` sopen 1 s after typing an edit, then open the same file again. The window offers Restore;
   Restore shows the same changed bytes marked; the remote is unchanged. Discard removes the journal directory.
10. **Save As, 24 GB.** Save As to a free local path. The file appears only when complete, with size 24 000 000 000
    and SHA-256 = the remote's with the patch applied. A taken name is refused with the next free `-N`. Esc
    mid-transfer leaves nothing behind in the chosen folder (no `part`, no staging folder), and the edits stay
    unsaved.
11. **Save As, source changed mid-transfer.** Not saved, staging removed, the reason shown.
12. **Image budget.** Open a remote 14000×14000 JPEG; today it decodes and is editable (§2.8). Before any byte past
    the first 64 KiB is fetched, the window says `too large to edit here … 588 MB … limit 512 MiB`. Viewing behaves
    as streaming-view says.
13. **Image within budget.** Open a 14 MB 11000×11000 PNG and make the first edit gesture. It shows download
    progress, and Esc cancels with no copy kept. Completed, the edit applies, Save runs as today, and the whole local
    backup is printed.
14. **Media render from remote.** Edit a remote video (cut, reorder) and Save. The render reads through the range
    server, and the upload equals the output size. When the source changes mid-render, nothing is uploaded and the
    journal is kept.
15. **Close prompt.** With unsaved edits, close: the prompt shows Save · Save As… · Discard · Cancel (Save copy in
    Save's place where Save is off). Each choice behaves as §4.6 says.
16. **Memory.** Editing (scenario 1) a 24 GB file keeps the process's resident memory within the viewer's budget +
    16 MiB.

## 6. Risks

| Risk | What happens | Mitigation |
|---|---|---|
| Disk-speed `copy-data` unmeasured (§2.4) | A slow disk makes the copy minutes long, and a fixed-size request could pass the 30 s `META_TIMEOUT` → `OutcomeUnknown` | Requests sized by time (~1 s), progress, cancel; the time is shown, not assumed |
| Dirty-page pile-up | The server buffers a whole 24 GB copy as dirty pages (kernel default 20 % of RAM), so the final fsync exceeds 60 s | fsync every 2 GiB copied (§4.2 step 5) |
| A server copy writes the full size every Save | 24 GB written to the server's disk per save (SSD wear, I/O load on that host) | Said in the progress line; (c) exists by explicit choice; repeated saves are the owner's call |
| Sparse files | sftp-server's read/write loop fills holes, so the temp allocates the full apparent size | `statvfs` checks the apparent size (conservative); [estimate] model files are rarely sparse, VM images are |
| Old servers (OpenSSH < 9.0) | No `copy-data`: big-file Save becomes the explicit (c) or Save As | The cost stated in the refusal; (a) still covers ≤ 256 MiB |
| Same-size change elsewhere with the mtime put back | Not detected; kept in the saved file (§4.2) | The result line says what was checked; §7 asks the owner to accept it |
| mtime in whole seconds (SFTP v3) | A same-size write within the open's second slips past `(size, mtime)` | The edited-page check covers the edited neighbourhood only |
| Rename replaces the inode | Other hard links keep the old bytes; xattrs/ACLs are not copied. That is today's §5 behaviour, now applied to huge files too | Unchanged; (c) keeps the inode, by choice |
| A running process maps the file (e.g. a model server) | (b) leaves its mapping on the old inode (safe); (c) changes bytes under it | One more reason (c) is never automatic |
| Inverse patch needs a tool to apply | The backup is complete but is not a file a `cp` restores | §7: owner decides on a restore path |
| A lost reply mid-(c) | The target itself may be partly patched | `OutcomeUnknown` naming the target and the backup |
| Journal disk use | Image pastes are full pictures | Bounded by the decode budget per paste; removed with the journal |
| No decode-size limit today | A large image's decode allocates its whole buffer: 588 MB measured for 14000², and more for bigger headers. The 512 MiB budget is sopen's own check from the header, never the crate's | §4.5; the check runs before the download |
| Save channel exhausts the per-host budget | `MaxSessions` (default 10) per master is shared by viewers, prefetch and saves | One save channel per Save, closed after; counted in the viewing unit's channel budget |
| Viewer block cache serves stale bytes | The editor shows bytes the remote no longer has | `seen` is checked at every Save and Restore; assumption 1 makes reads fail on change |

## 7. Rule changes that need the spec or the owner

Implementing §4 changes these sentences of AGENTS.md and the spec. None of them is done here.

1. **Hex backup is an inverse patch, not a copy.** AGENTS.md: "Image and hex Save keep a local backup of the
   original first"; spec §5: "Image Save … copying the fetched original to the local backup". For hex, the local
   backup becomes the bytes the save replaced (complete for overwrite-only edits, measured). Restoring it needs
   sopen: either a hidden `sopen --apply <undo file> host:/abs` running the (b) route, or the hex window opening an
   undo file. That is an edit feature, so it is the **owner's call** (AGENTS.md "No new edit features without the
   owner").
2. **Media Save's backup of a remote original:** remote hard link (recommended, zero copy, leaves a file on the
   remote until deleted) vs a local full download vs Save off. This leaves a file on the remote, so it is the
   **owner's call**.
3. **§5's content baseline under streaming.** "Same content hash up to 256 MiB" holds only where the viewer read
   the whole file. Otherwise it is `(size, mtime)` four times plus the edited pages, and a same-size, same-mtime
   change elsewhere is kept rather than refused (§4.2). Spec §5 and AGENTS.md "Never overwrite silently" wording.
4. **Write in place (c)** is a new write mode. It is not atomic, it runs only from an explicit dialog, and its
   `OutcomeUnknown` lands on the target. Spec §5 addition, **owner's call**.
5. **Save As (local)** is new in the image, hex and media windows: owner message 17 asks for it. New key
   Alt+Cmd/Ctrl+S and F7; Shift+Cmd/Ctrl+S stays Save copy. Spec §6/§6a/§7 key lists.
6. **Close prompt** gains Save As…; Discard now also deletes the journal. Spec §6a "Closing with unsaved edits asks:
   Save (Save copy …) / Discard / Cancel".
7. **Journal directories** become a new kind of owner-only file under `<cache>/.edits` (spec §5's list of local
   copies and modes).
8. **Image whole-fetch on edit**, with the 64 MiB confirm, stated in spec §6. Also a **new decode budget** — 512 MiB
   recommended. It refuses images that are editable today (a 14000² RGB decodes to 588 MB), so its value is the
   **owner's call**.

## 8. Work units (disjoint file ownership)

Two efforts already touch the same files:

- **Streaming viewing** adds `read_at` to `src/remote/{mod,sftp,local}.rs`, and reworks the hex `Pages`, the image
  load and the media input.
- **`feat/media-edit`** owns `src/gui/media/*`, `src/cut.rs` and `src/ffmpeg.rs`.

So every unit below owns new files, and edits a shared file only where stated.

| # | Unit | Owns (new unless marked) | Touches elsewhere | Depends on | Gate / receipt |
|---|---|---|---|---|---|
| E1 | Remote patch primitives and the (b)/(a)/(c) algorithms; the inverse-patch writer | `src/remote/patch.rs`, `src/remote/patch/tests.rs`, `src/remote/sftp/patch.rs` (child module: SFTP handles, dedicated channel, `copy_to`, positional writes, fstat, fsync, statvfs), `src/remote/local/patch.rs` (LocalFs: `copy_file_range` / clone + `pwrite`) | One `mod patch;` line each in `remote/mod.rs`, `remote/sftp.rs`, `remote/local.rs`. One provided method `fn patcher(&mut self) -> Option<&mut dyn PatchFs> { None }` on `RemoteFs`, handed to the viewing unit that owns that trait | — | `just check`; scenarios 1, 3–8 against `local_server()` and LocalFs; live 24 GB receipt on a remote `/tmp/sopen-receipts/` |
| E2 | Journal | `src/journal.rs`, `src/journal/tests.rs` | `mod journal;` in `src/lib.rs` | — | `just check`; scenario 9's restore logic as unit tests (lock, orphan listing, atomic snapshot, kinds) |
| E3 | Hex write side moved out of the window file | `src/gui/hex/file.rs` — a **pure move** of `hex.rs:615-1191` (`Ident`, `Base`, `HexFile`, `Saved`, `Pass`, …), landed **before** the viewing unit edits `hex.rs` | `hex.rs`: the move only | — | `just check` with no behaviour change (existing hex tests) |
| E4 | Hex Save via E1, Save As via `binary::save_new`, journal wiring | `src/gui/hex/file.rs` (after E3), `src/gui/save_as.rs` (the shared Save As modal: path field, Browse via `binary::NativeChooser`, ETA, progress, cancel) | `mod save_as;` in `src/gui/mod.rs` | E1, E2, E3, viewing's `read_at` | scenarios 1, 10, 11 |
| E5 | Hex window UI: keys, F7, prompt, status `kept at`, recovery rows; the F7 "SaveAs" slot in **all three** key rows | `src/gui/hex.rs` (window part), `src/gui/chrome.rs` (`HEX_KEYS`, `IMAGE_KEYS`, `MEDIA_KEYS`: F7 only) | — | E4, and **after** the viewing unit's `hex.rs` change (same file: sequenced, not parallel) | `SOPEN_SCREENSHOT` receipts of the prompt and the recovery rows (`close=ask`); scenario 15 |
| E6 | Image: write side moved to `src/gui/image/file.rs` (pure move of the `ImageFile` write side, `image.rs:829-1020`, first), then the budget check from the header, the whole fetch on edit, the op-log journal, Save As | `src/gui/image/file.rs`, image UI bits in `image.rs` sequenced after viewing | — | E2, E4's `save_as.rs`, viewing's image load | scenarios 12, 13, 15 |
| E7 | Media: remote-source Save/Save As/Export through `Input` URLs; backup per §7 item 2 | `src/gui/media/file.rs` (on `feat/media-edit`, after it merges) | — | media-edit merged, viewing's range server, owner decision §7.2 | scenario 14 |
| E8 | Spec and AGENTS text for §7 | `.prd/01-spec.md` §5, §6, §6a, §7, §9; `AGENTS.md` rules | — | owner decisions §7.1, 7.2, 7.4 | review |

Order: E1, E2 and E3 in parallel, then E4, then E5 and E6, then E7 (gated by media-edit) and E8 (gated by the
owner).

## 9. Reproduce, and sources

The spike is a single Rust file over `openssh` 0.11.6 + `openssh-sftp-client` 0.15.12 + `sha2`, with a second
binary over `image` 0.25.10. Its sequences, with each request awaited unless stated:

- `copysave P OFF HEX CHUNK [base=SIZE:MTIME expect=HEX probe mutate_at=K keep]` = §4.2 steps 1–10, with CHUNK
  fixed (0 = one `copy_all_to`).
  - `probe`: a second `Sftp` over the same session `stat`s every 200 ms, and the save's channel `stat`s once at
    300 ms.
  - `mutate_at=K`: a third channel writes one byte at offset 0 after copy request K.
  - `keep`: removes the temp instead of renaming.
- `inplace P OFF HEX [base=… expect=…]` = §4.2 (c).
- `fullrewrite P OFF HEX` = (a), sequential 256 KiB.
- `down P N` / `up P BYTES N` = N handles over disjoint ranges, 256 KiB requests.
- `mutate P OFF HEX keep-mtime` = another writer that restores the mtime (`setstat` times).
- Test data: `head -c 24000000000 /dev/urandom > /tmp/sopen-stream-spike/edit/model.bin` on the remote;
  `sha256sum`, `od -An -tx1 -j OFF -N 4` and `stat -c` for checks; images made with `ffmpeg -f lavfi -i
  testsrc2=s=WxH` (`sips` for the 14000² JPEG).

Sources:

- OpenSSH `sftp-server.c` at `V_10_5_P1` (v1.156, 2026-07-01): `copy-data` `:164`, `:725`, `:1603-1712`; main loop
  `:2053-2123`. At `V_9_0_P1` (v1.140) `copy-data` is present; at `V_8_9_P1` (v1.139) it is absent.
  `https://github.com/openssh/openssh-portable/blob/V_10_5_P1/sftp-server.c`
- `openssh-sftp-client` 0.15.12 (crates.io): `src/sftp.rs:437-442` `support_copy`; `src/file/mod.rs:403-466` read
  and write at an offset; `:650-741` `copy_to` / `copy_all_to`.
- `image` 0.25.10 (crates.io):
  - `src/io/limits.rs:36-57`: `max_alloc` 512 MiB default.
  - `src/io/image_reader_type.rs`: `:89-93` (`ImageReader::new` takes the default limits), `:219-223`
    (`into_decoder` only calls `set_limits`), `:311-322` (`decode` reserves `total_bytes` first).
  - `src/codecs/png.rs:264-272` and `src/codecs/jpeg/decoder.rs:168-174`: `set_limits` checks dimensions only.
  - `src/io/free_functions.rs:305-317`: `from_decoder` allocates the whole buffer.
- `image` probe: the second spike binary makes sopen's exact calls (`src/gui/image.rs:288-318`). Its first version
  called `decode()`, which gave the misleading failures quoted in §2.8.
- sopen at `feat/streaming`'s branch point: the files and lines cited in §1 and §4. `feat/media-edit`:
  `src/gui/media/file.rs:93-140`, `src/ffmpeg.rs:49-76`, `:776-806`, `:893`, `src/gui/media/timeline.rs:30-45`,
  `src/cut.rs:119-170`.
