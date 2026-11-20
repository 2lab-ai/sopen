---
title: sopen streaming view — research, measured spike, design
type: research
tags: [sopen, streaming, view, sftp, ffmpeg, measurement]
---

# Viewing a remote file without downloading it (2026-11-20)

Owner, verbatim (message 17, [`../streaming/ssot.md`](../streaming/ssot.md)): "그리고 파일뷰어 이미지 뷰어 영상 모두
지금 병신처럼 전체를 다 받아서 처리하는데 이거 모두 스트리밍 처리해줘. 예를들어 24기가 짜리 모델 파일을 시팔 24기가 다 받아서
보여주는데 말이 안되잖아. … 그냥 파일 뷰어면 보여줄수 있을만큼만 먼저 받고 먼저 화면 보여주고 그 다음에 시퀀셜로 볼경우에
몇개의 뷰를 미리 로딩해서 앞뒤로 먼저 로딩해서 보여주고 (전체 다운로드 X) 멀리 이동하면 이거는 시간이 좀 걸리는데 그렇게
처리해줘. 예를들어 유투브에서 동영상 스트리밍한다고 생각하고 리서치해서 파일도 바이너리 파일도 음악, 영상 모두 그렇게 처리해줘."

Editing a streamed file is [`streaming-edit.md`](streaming-edit.md); this file is viewing.

## 0. Summary

**What happens today.** Every viewer of a remote file waits for a whole download (§1). Today's download keeps one
256 KiB SFTP request in flight, which is 2.5 MB/s on the measured link. That is ~49 s for a 123 MB video and
~2.7 h for a 24 GB file before anything shows.

**What was measured** (§2) against the Linux laptop over sopen's own ssh master. The design rests on these:

- **SFTP is a fine random-access byte source.**
  - A new channel plus open plus the first 64 KiB takes ~0.2 s (0.49 s cold).
  - A random 64 KiB read anywhere in a 4 GiB or 24 GiB file is p50 38 ms.
  - Reaching the link's ceiling takes 256 KiB–1 MiB **in flight**. Request size does not matter at equal bytes in
    flight.
  - The client is not the limit (2.1–2.8 GB/s locally).
- **SFTP cannot cancel, and the server answers in order.**
  - A read queued behind 16 MiB of prefetch on the same channel waits ~2 s.
  - Aborting the prefetch does not help on that channel.
  - *Design (§4.4):* cap the prefetch in **bytes in flight**, `clamp(2 × rate × rtt, 256 KiB, 4 MiB)`, so a far
    jump waits about two round trips plus its own.
  - Readahead starts at zero and grows only on sequential hits, as Linux readahead does. Blind readahead on open
    measurably delayed ffprobe's jump to a moov at the end.
- **An open SFTP handle keeps reading the file it opened** when another file is renamed over the path. Writes
  in place show through it. That gives a clean change model: `fstat` the handle and `stat` the name every 2 s.
- **ffmpeg reads a remote file well through a loopback HTTP range server** fed from the block cache. The
  pipe option and ffmpeg's `sftp://` are rejected: the pipe cannot seek or read a moov at the end, and `sftp://` is
  not built into either host's ffmpeg and would bypass sopen's ssh.
  - A moov-at-end 123 MB mp4 probes and shows its first frame in 0.4 s, having fetched 1.7 %.
  - A scrub to 80 % takes 0.4–1.1 s.
  - Open + 10 s of play + a far seek fetch 13–19 % of the file.
  - A 10 s export reads 12 % of it.
  - mp3 seeks need `-fflags +fastseek`: without it a seek reads the file through (82–91 %); with it, 0.14 s and 8.7 %.
- **Images.** On the test files, size and format came from the first 4 KiB and the Exif thumbnail from the first
  64 KiB. Real files can put more before the header, so the design walks markers with a byte cap (§5.3).
  Truncated JPEGs decode (a progressive one gives a whole coarse picture at 10 % of its bytes); truncated PNGs
  only through the `png` row reader. A pixel budget can refuse a huge image before any pixel data is fetched.

**The recommended architecture** (§4):

- `remote::Ranged` is an SFTP channel and handle of the viewer's own.
- `src/stream/` holds:
  - `Source`: fingerprint, state, change detection, reload;
  - `BlockCache`: 64 KiB blocks, a 64 MiB RAM LRU, no disk;
  - `Prefetcher`: per-reader windows, demand first, a byte cap;
  - `http`: one loopback server per media window, with a 128-bit token and a Host check, and ffmpeg run with
    `-protocol_whitelist http,tcp`.
- Local files keep reading in place.

**Per viewer** (§5):

- **Hex:** no fetch; Pending rows; a 1 MiB window.
- **Text:** first screen at once; a prefix line index; byte positions past it; Go-to-line on demand.
- **Image:** header, thumbnail and budget first, then a streamed full fetch with previews.
- **Media:** a URL `Input`, a progressive waveform, Export of just the cut.

Whole-file actions stream with progress and a cancel, and `read_to` becomes pipelined (§6). Bounds and the token
residual are in §7, 16 acceptance scenarios in §8, eight work units with disjoint files in §9, and four owner
points in §10.

## 1. What sopen does today

Read in the code at the branch point of `feat/streaming`. Every viewer of a remote file waits for the whole file.

| Fact | Where |
|---|---|
| The browser's Enter fetches the whole file into the cache before any viewer: images always, video/audio, text and hex when remote. | `src/gui/browser/prepare.rs:110-120`, `:123-181` |
| The standalone route does the same for the image and media windows. Only local media plays in place. | `src/main.rs:682-705`, `:351-364` |
| The binary dialog's "Open in hex editor" fetches the whole file first. | `src/gui/binary.rs:814-823`, `:844-850` |
| A fetch is `stat` → whole-file copy into `.part` → `stat` again → rename. It retries once if the file changed, then refuses. | `src/remote/mod.rs:443-515` |
| The copy is read one 256 KiB SFTP request at a time, and each request is awaited before the next is sent. So a fetch is bound by the round trip (§2.2: 2.5 MB/s on the measured link). | `src/remote/sftp.rs:31-32`, `:678-714` |
| The text viewer indexes the whole file in one pass when it opens. The marks fall every 256 lines and at every 64 KiB. Paging then reads only what is shown, through `pread` on a local `File`. | `src/gui/text.rs:46-48`, `:129-221`, `:343-432` |
| The hex window reads 4 KiB pages through a 64-page LRU. The read is a synchronous `read_exact_at` on the frame thread, and a failed page is held empty. | `src/gui/hex.rs:26-29`, `:136-181` |
| The image window decodes from a complete `&[u8]`. `ImageReader` has the image crate's default `Limits` (`max_alloc` 512 MiB). | `src/gui/image.rs:284-318` |
| `ffprobe`, the player's two `ffmpeg` processes, the scrubber and the waveform all take a local path. | `src/ffmpeg.rs:75-98`, `src/gui/playback.rs:366-377`, `:486-500`, `src/gui/media.rs:149-180`, `:190-237` |
| The waveform decodes the whole audio track and publishes nothing until the end. | `src/gui/media.rs:170-177` |
| "Save to…" already streams: `download_to` pipes `RemoteFs::read_to` into the private staging file with `(done, total)` progress. No cache copy is made. | `src/gui/binary.rs:673-695` |
| The kind sniff already reads only the head (8 KiB). | `src/kind.rs:30`, `:231` |

Spec text that message 17 supersedes, to be rewritten with the design:

- §9 lists "streaming playback before download completes" as out of scope.
- A6, §3a ("A remote file is fetched into the cache exactly as the standalone routes fetch it") and §6a ("Hex: the file is fetched into the cache as every fetch is") all fetch first.
- §3a: "A remote file is viewed from its cached copy, which does not change under the viewer and is not followed."
- §10 makes streaming wait on usage numbers ("Large-media waits … → consider streaming"). The owner has now asked for it directly.

## 2. Measurements

### 2.1 Setup

- **Client:** the Mac (Apple silicon), with the crates sopen locks: openssh 0.11.6, openssh-sftp-client 0.15.12,
  tokio 1.54.1, image 0.25.10, png 0.18.1, zune-jpeg 0.5.15. The spike crates reuse sopen's `Cargo.lock`.
- **Server:** the Linux laptop, OpenSSH 10.5p1 `sftp-server`. Test files live on its `/tmp`, which is a
  **tmpfs**, so the far side reads from RAM and only the link is measured.
- **Link:** a mesh-VPN path with a direct route. ICMP RTT min/avg/max = 12/21/40 ms. Its bandwidth drifted 2× during
  the session:
  - raw `ssh host 'head -c 64MiB file' > /dev/null` over the master took 10.8–18.1 s (3.7–6.2 MB/s);
  - an `scp` upload of 393 MB took 74 s (5.3 MB/s);
  - later runs reached 7–10 MB/s.
- **Transport:** one ssh ControlMaster, as in spec §4, with every SFTP channel attached to it. ssh compression is
  off (`ssh -G`: `compression no`).
- **Test data:** the remote test file is 4 GiB of `/dev/urandom`, so it is incompressible.
- **Reading the numbers:** they come from one link. A design constant must not be read off them. §4 makes the
  windows adaptive, and §8 bounds tests in bytes and requests, not seconds.

### 2.2 SFTP as a block source

**Server limits.** `limits@openssh.com` is offered, and the client takes from it `max_read_len = 261120`
(255 KiB) and `max_write_len = 261120`. These were read with the crate's `__ci-tests` accessors, which the product
cannot use (§4.2). A single read asking for 1 MiB returned 261120 bytes, so larger reads are
silently clipped. The server also offers fsync, posix-rename, hardlink, statvfs, copy-data and expand-path.

```
$ sftpbench limits
max_read_len=261120 max_write_len=261120 fsync=true posix_rename=true hardlink=true statvfs=true copy=true expand_path=true
one read asking 1048576 bytes returned 261120
```

**Time to first byte** is a new SFTP channel over the running master, then `open` + `fstat` + the first
64 KiB. Six runs; run 0 was the first channel after a pause:

```
ttfb run=0 channel_ms=352.2 open_ms=375.3 fstat_ms=403.0 first64k_ms=485.7
ttfb run=1 channel_ms=120.4 open_ms=135.6 fstat_ms=180.8 first64k_ms=203.9
ttfb run=2 channel_ms=122.4 open_ms=157.4 fstat_ms=169.2 first64k_ms=191.7
ttfb run=3 channel_ms=118.9 open_ms=154.0 fstat_ms=206.7 first64k_ms=250.1
ttfb run=4 channel_ms=106.1 open_ms=143.7 fstat_ms=165.8 first64k_ms=218.8
ttfb run=5 channel_ms=136.9 open_ms=151.0 fstat_ms=166.8 first64k_ms=200.6
```

So **first byte ≈ 0.2 s** once the master is up (0.49 s cold). About 0.12 s of it is starting the channel, which
a viewer already pays today: each viewer gets its own connection, spec §3a.

**A random 64 KiB read** anywhere in the 4 GiB file, on an idle channel, 40 reads:
p50 38 ms, p95 73 ms, max 94 ms, min 18 ms. The first three reads were checked byte-for-byte: their SHA-256
equals `tail -c +N | head -c 65536 | sha256sum` run on the laptop.

**Sequential throughput.** Requests were kept in flight with `File::clone` + `start_seek` + `read`. The crate
documents that "Cloning [`File`] … shares the same underlying file handle … while reads, writes and seeks can be
performed independently" (`file/mod.rs`, [docs.rs 0.15.12](https://docs.rs/openssh-sftp-client/0.15.12/src/openssh_sftp_client/file/mod.rs.html)).
Each cell ran for 3 s, three repetitions, grouped by **bytes in flight**:

| In flight | Configurations | MB/s median | range | n |
|---|---|---|---|---|
| 32 KiB | 32K×1 | 0.5 | 0.5–1.1 | 3 |
| 64 KiB | 32K×2, 64K×1 | 1.2 | 0.8–2.3 | 6 |
| 128 KiB | 128K×1, 32K×4, 64K×2 | 2.3 | 1.3–4.2 | 9 |
| 255 KiB | 255K×1 (today's fetch) | 2.5 | 2.0–4.8 | 3 |
| 256 KiB | 128K×2, 32K×8, 64K×4 | 3.8 | 1.3–7.6 | 9 |
| 512 KiB | 128K×4, 32K×16, 64K×8 | 3.4 | 1.8–8.9 | 9 |
| 1 MiB | 128K×8, 32K×32, 64K×16 | 3.7 | 2.3–9.5 | 9 |
| 2 MiB | 128K×16, 32K×64, 64K×32 | 4.2 | 2.6–9.2 | 9 |
| 4 MiB | 128K×32, 64K×64 | 5.0 | 3.1–9.2 | 6 |
| 8 MiB | 128K×64 | 3.6 | 2.5–8.4 | 3 |

Below about 256 KiB in flight the reader is bound by the round trip: 32 KiB per round trip is 0.5 MB/s. From
about 256 KiB the medians (3.4–5.0 MB/s) sit inside the raw `ssh cat` range (3.7–6.2 MB/s). More in flight buys
nothing measurable. These are single cells taken while the link drifted (see below), so read the trend, not the
digits.

The matrix runs request sizes in a fixed order, and its 255K rows came last in each repetition, when the link was
slower. To check whether request size matters, a second run alternated three request sizes at the same ~1 MiB in
flight, 2 s each, six rounds. Medians:

| 64K×16 | 128K×8 | 255K×4 |
|---|---|---|
| 8.3 MB/s (7.4–10.0) | 9.1 MB/s (5.3–10.3) | 8.7 MB/s (6.6–9.7) |

**Request size does not matter at equal bytes in flight.** Only the bytes in flight do.

**The client is not the bottleneck.** Against OpenSSH's own `sftp-server` run locally over pipes (no network,
sparse local file): 2.1–2.8 GB/s at ≥ 2 MiB in flight, and a random 64 KiB read p50 0.1 ms.

**In flight vs. a demand read.** This decides the prefetch design. A background prefetch keeps *k* × 255 KiB
reads in flight while a viewer does random 64 KiB demand reads, 15 reads each, demand latency in ms. Two
settings:

- *same channel*: the prefetch shares the demand reads' SFTP channel;
- *second channel*: the prefetch runs on its own SFTP channel, which is its own `sftp-server` over the same master.

| prefetch in flight | same channel p50 / p95 / max | second channel p50 / p95 / max |
|---|---|---|
| 0 | 33–54 / 83–97 / 84–155 | 45 / 98 / 120 |
| 510 KiB | 51–89 / 95–107 / 98–152 | 54 / 91 / 96 |
| 1 MiB | 95–171 / 120–210 / 137–219 | 95 / 103 / 110 |
| 2 MiB | 298–362 / 443–487 / 499–502 | 177 / 279 / 336 |
| 4 MiB | 439–639 / 475–811 / 544–897 | 186 / 238 / 270 |
| 8 MiB | 1014–1038 / 1816–2770 / 2152–2999 | 173 / 275 / 285 |
| 16 MiB | 1648–2152 / 1921–4046 / 1995–7604 | 167 / 188 / 208 |

(Two runs of the same-channel column, given as low–high.)

- **Same channel:** the demand read waits behind everything already queued. `sftp-server` answers in order
  (source below), so latency ≈ bytes queued ÷ link rate: 16 MiB queued costs 2 s.
- **Second channel:** demand latency levels off at ~170–190 ms whatever the prefetch depth. Why it levels off there
  is a [hypothesis], given below.

**SFTP has no cancel.** A 16 MiB prefetch (64 × 255 KiB) was started, and after 300 ms its tasks were aborted
(the futures dropped). Then one demand read was timed. 4 trials each:

```
cancel mode=same        p50_ms=1815 max_ms=2066 all=[2066, 1732, 1815, 1462]
cancel mode=other-drop  p50_ms=186  max_ms=334  all=[171, 172, 186, 334]
cancel mode=other-close p50_ms=191  max_ms=199  all=[199, 169, 165, 191]
```

Dropping a read's future does not stop the server: the bytes still come, and a demand read on the same
channel waits for them. Either of two measures keeps a far seek fast:

- prefetch on a channel of its own;
- a bound on how much is queued.

At 1 MiB in flight the paired run gave the same demand latency on both: 94.5 ms (same channel) vs 95.1 ms (second
channel), p50. The first same-channel run gave 171 ms. That much in flight is already the link's ceiling. §4.4 takes the bound, which costs no extra channel (`MaxSessions`, §3). Closing the prefetch
channel buys nothing over just dropping its tasks (191 vs 186 ms).

**Closing waits for every live handle.** `Sftp::close()` with no file handle alive returned in 67 ms. With
one `File` still alive it had not returned after 10 s. A viewer must therefore abort its prefetch tasks, drop every
`File` clone, and only then close, bounded and off the UI thread.

```
closetest handle_alive=false close_returned=true after_ms=67
closetest handle_alive=true close_returned=false after_ms=10004
```

**An open handle reads the file it opened.** Measured on the laptop's tmpfs (POSIX rename and open-file
semantics):

```
before: handle reads "OLD-CONTENT-0123456789"
after rename-over: handle reads "OLD-CONTENT-0123456789"; fstat(handle) size=22 …; stat(path) size=3 …
after append in place: new handle reads "NEWX"; fstat size=4
after truncate in place: new handle reads ""; fstat size=0
```

- **Replaced:** another file renamed over the path, the way an editor saves. The handle keeps reading the old
  file, consistently, so `fstat(handle)` and `stat(path)` disagree.
- **Changed in place:** an append or a truncate shows through the handle, and `fstat(handle)` sees the new size.

That gives the change model of §4.5.

**Bounds still hold at 24 GB.** A 24 GiB sparse file was made with `truncate -s 24G` and a marker written at
offset 0. Three fresh channels each ran: `open` + `fstat` 148–161 ms, then 64 KiB reads at 0, 20 GiB and 24 GiB −
64 KiB, each 20–74 ms. Each run took 217–285 ms in all. Nothing about a read depends on where it is in the file.

**Why the link varied.** During this session another spike used the same laptop (a directory of its own appeared
in the test folder at 11:44). The 2× drift in single-cell timings therefore has no known cause. The alternated
`contrast` run and the ~10× same-channel vs second-channel gap are differential, so the drift does not explain them.

**Two server behaviours, from source:**

- `sftp-server` handles requests one at a time in arrival order. Its main loop calls `process()` once per
  iteration, and only "if we can fit the results into the output buffer, otherwise stop processing input and let
  the output queue drain" (OpenSSH `sftp-server.c` V_10_6_P2, L2053–L2124). That is why a demand read on a shared
  channel waits for everything queued before it.
- Why the second channel levels off at ~170–190 ms is **[hypothesis]**: the other channel's replies already sent
  fill the ssh connection's buffers ahead of the demand reply. This was not measured further.

### 2.3 Audio and video

**Options for getting a remote file to ffmpeg:**

- **(a) Loopback HTTP range server.** A server inside sopen on `127.0.0.1`, with a random token in the path,
  serving byte ranges from the SFTP block source (spike `httprange`).
- **(b) ffmpeg's own `sftp://`.** It is **not available**: `ffmpeg -protocols` on both hosts (FFmpeg 9.0.2,
  Homebrew) lists no `sftp`. It needs `--enable-libssh`
  ([configure L284 @n9.1](https://github.com/FFmpeg/FFmpeg/blob/n9.1/configure#L284)), and even then it would
  bypass §4's transport:
  - it opens its own ssh connection with libssh, one per ffmpeg process;
  - it authenticates with none / publickey / password only
    ([libssh.c L116-L160](https://github.com/FFmpeg/FFmpeg/blob/n9.1/libavformat/libssh.c#L116-L160));
  - libssh treats `ControlMaster`/`ControlPath` as "Unapplicable option"
    ([libssh config.c L1509-1510](https://gitlab.com/libssh/libssh-mirror/-/blob/libssh-0.12.2/src/config.c#L1509-1510)).
- **(c) Pipe input.** `ssh … cat | ffmpeg -i pipe:0`, with no seek (spike `pipe_run.py`).

**Commands measured.** Each step runs sopen's own command lines with the path replaced by the URL:

- `ffprobe -v error -print_format json -show_format -show_streams` (`src/ffmpeg.rs:76-98`);
- the player's video and audio decoders (`src/gui/playback.rs:366-377`, `:486-500`, decoded at 1280×720 RGBA and
  48 kHz f32);
- the scrubber's one-frame decode (`src/gui/media.rs:210-221`);
- the waveform's whole decode (`media.rs:158-166`).

**Readings.** "MB" is SFTP bytes fetched, cumulative, deduplicated by the block cache. Each cell is a single run
(n=1). The "first frame" column ran right after ffprobe, so it is warm; read **probe + first frame as the cost of
opening**. Fixtures:

- 120 s 1080p H.264 + AAC at 8.2 Mbit/s: 123 MB, as mp4 with moov at the end (`mdat@40`, `moov@123077870`), as
  faststart mp4, and as mkv (Cues at the end);
- a 600 s 320 kbit/s CBR mp3: 24 MB.

Blocks 256 KiB; readahead 0 or 8 blocks per HTTP connection.

| File, readahead | probe cold | first frame (warm) | MB after probe+frame | MB after 10 s play | far seek @96 s / 480 s | MB after far seek (% of file) | reprobe warm |
|---|---|---|---|---|---|---|---|
| mp4 moov-end, 0 | 0.30 s | 0.10 s | 2.08 | 15.45 | 0.60 s | 19.12 (15.5 %) | 0.04 s |
| mp4 moov-end, 8 | 1.25 s (0.44, 0.41 in two later runs) | 0.09 s | 5.22 | 17.28 | 0.58 s | 23.05 (18.7 %) | 0.04 s |
| mp4 faststart, 8 | 0.28 s | 0.08 s | 5.51 | 17.04 | 0.41 s | 22.54 (18.3 %) | 0.04 s |
| mkv, 0 | 0.18 s | 0.37 s | 2.04 | 12.79 | 0.64 s | 16.19 (13.2 %) | 0.04 s |
| mkv, 8 | 0.30 s | 0.21 s | 4.92 | 15.41 | 1.06 s | 20.91 (17.0 %) | 0.05 s |
| mp3, 0, default | 0.11 s | 0.03 s (PCM) | 0.79 | 1.31 | **3.07 s** | **19.66 (81.9 %)** | 0.02 s |
| mp3, 8, default | 0.21 s | 0.02 s | 3.41 | 3.41 | **1.70 s** | **21.76 (90.6 %)** | 0.02 s |
| mp3, 0, `-fflags +fastseek` | 0.15 s | 0.02 s | 0.79 | 1.57 | 0.14 s | 2.10 (8.7 %) | 0.02 s |
| mp3, 0, `-usetoc 1` | 0.11 s | 0.02 s | 0.79 | 1.57 | 0.14 s | 2.62 (10.9 %) | 0.02 s |

After the far seek: "play from there" gave its first frame in 0.11–0.14 s, and a near scrub 2 s later in
0.10–0.49 s.

**What ffmpeg asked for.** From the server's Range log:

| File | Ranges requested |
|---|---|
| moov-end mp4 | `bytes=0-` (closed after 256 KiB), then `bytes=123077870-` (the moov, 109 503 bytes), then `bytes=48-` sequential |
| mkv | the head, then the Cues at the end (1 438 bytes), then the data |
| any file, on seek | the connection is closed and a new `bytes=N-` opened |

- Every range is open-ended. ffmpeg reads as it decodes and closes the connection to seek. FFmpeg sends
  `Range: bytes=%"PRIu64"-` ([http.c L1691-L1710 @n9.1](https://github.com/FFmpeg/FFmpeg/blob/n9.1/libavformat/http.c#L1691-L1710)),
  and treats `Accept-Ranges: bytes` or a `Content-Range` as seekable (L1044-L1045, L1360-L1366).
- sopen's two players run as two processes on two connections over the same bytes. The cache fetched them once:
  each connection was sent ~14 MB, and 12–15 MB was fetched.
- 10 s of this video is ~10.3 MB of media. A 10 s play fetched 10.5–14 MB beyond the open.
- With a reader paced the way sopen's frame channel paces it (pipe backpressure, video only), 10 s fetched 15.2 MB.

**Blind readahead and the jump to the moov.** With readahead 8, the cold moov-end probe queued 2 MiB of head
prefetch in front of the moov request. It took 1.25 / 0.44 / 0.41 s in three runs, against 0.30 / 0.30 s without
readahead. That is n=3 vs n=2 on a drifting link, so it only illustrates. The rule rests on §2.2's `randload` and
`cancel` (a demand read waits for every byte queued before it): **readahead starts at zero and grows only on
sequential hits.**

**mp3 seeking reads the whole file through** unless `-fflags +fastseek` (or `-usetoc 1`) is given. In FFmpeg's
`mp3_seek` (`libavformat/mp3dec.c` n9.1, L638–L676), without `fast_seek` (and without `usetoc` with a Xing TOC)
the function ends in `return -1; // generic index code`. With `fast_seek`, a CBR file seeks to
`timestamp × filesize / duration`, and VBR logs "Using scaling to seek VBR MP3; may be imprecise."

- The precision of the seek was **not verified**.
- `+fastseek` belongs in playback and scrub only. Export keeps exact seeking: spec §7 promises exact cuts.

**Option (c), pipe**, one run each:

| File | ffprobe | first frame | far seek (`-ss 96`) |
|---|---|---|---|
| moov-end mp4 | **13.5 s, all 123 MB read** | **none**: the 123 MB was consumed before the moov was seen ("partial file") | — |
| faststart mp4 | 0.17 s, 0.6 MB | 0.20 s, 1.4 MB | **10.8 s, 99.4 MB read through** |
| mkv | 0.26 s, 0.6 MB | 0.22 s, 1.4 MB | — |
| mp3 | 0.23 s, 0.2 MB | — | — |

sopen restarts ffmpeg for every play, pause-then-play and scrub, and each restart would re-read from byte 0. **(c)
is rejected.**

**Whole-file actions over (a):**

- **Export.** sopen's export command line (`-ss 60 -to 70 -i URL`, libx264 veryfast) cut 10 s out of the moov-end
  mp4 in 1.39 s. It fetched **14.7 MB (12 % of the file)**. The result was 10.000000 s long.
- **Waveform.** The whole-file decode of the 24 MB mp3 took 2.46 s cold and fetched 24.0 MB once.

**Today, for comparison** (computed from §2.2, not timed end to end): a fetch at today's one-request-at-a-time
2.5 MB/s takes ~49 s for this 123 MB video, and ~2.7 h for a 24 GB file, before anything shows.

**Nested protocols.** A playlist that points at a local file was served as `pl.m3u8`:

- With the default whitelist, ffmpeg refused it: "Protocol 'file' not on whitelist 'http,https,tls,rtp,tcp,udp,crypto,httpproxy,data'".
- With `-protocol_whitelist http,tcp` it was refused the same way, against `'http,tcp'`.
- Served as `pl.mp4`, ffmpeg did not detect HLS at all: "Not detecting m3u8/hls with non standard extension".

So the default whitelist already keeps a URL input from opening local files. `-protocol_whitelist http,tcp` also
drops `udp`, `rtp`, `https` and `tls`.

The positive path was also run, against a minimal local Range server serving the moov-end mp4, with
`-protocol_whitelist http,tcp` as an input option:

- ffprobe read h264 + aac and a 120.000000 s duration;
- the export-shaped command (`-ss 60 -to 70 … -i URL … -c:v libx264 -y out.mp4`, a local output file) wrote a
  10.000000 s file in 1.49 s;
- a one-frame decode at 96 s produced its 3 686 400 bytes.

**Reconnect options.** ffmpeg's http `reconnect`, `reconnect_at_eof`, `reconnect_streamed` and
`reconnect_on_network_error` all default to off. `reconnect_delay_max` is 120 s (`ffmpeg -h protocol=http` on 9.0.2;
[http.c L194-L232 @n9.1](https://github.com/FFmpeg/FFmpeg/blob/n9.1/libavformat/http.c#L194-L232)). They stay off:
the server ends a connection only on a read error the window already shows from its `Source`, and a reconnect would
only repeat it.

**Keeping the token off the command line.** ffprobe 9.0.2 accepts `-/headers <file>`: a capture of its request
showed the header lines read from the file. ffmpeg 9.0.2 rejects it ("Unrecognized option '/headers'"). And on
the Mac, `ps -axo user,args` shows other users' full command lines. So a token in ffmpeg's arguments is readable
by other local accounts (§7).

### 2.4 Images

All on the Mac with the crate versions sopen locks (spike `imgpartial`). The test images:

- a 6000×4000 noisy test card as a baseline JPEG (17.6 MB) and as a progressive JPEG (16.6 MB, `jpegtran -progressive`);
- each JPEG with an Exif APP1 carrying a 160×107 thumbnail (IFD1 tags 0x0201/0x0202);
- a 3000×2000 PNG, plain (12.9 MB) and Adam7-interlaced (13.3 MB);
- a 74-byte PNG whose header claims 100000×100000.

Each file was cut at 10/25/50/75/100 % and decoded.

| Input | First 4 KiB | First 64 KiB | Cut at 10 % | 50 % | 100 % |
|---|---|---|---|---|---|
| baseline JPEG | 6000×4000 | + Exif thumbnail decodes (bytes 80…2059) | `image::decode` OK, top 10 % of rows filled, rest flat grey; 27 ms | 52 % rows; 88 ms | 188 ms |
| progressive JPEG | 6000×4000 | + Exif thumbnail decodes | OK, **every row filled** (a whole coarse picture); 57 ms | whole, finer; 212 ms | 443 ms |
| plain PNG | 3000×2000 | — | `image::decode` **fails** (`unexpected end of file`); `png` row reader gave 203 of 2000 rows, then the same error | 1015 rows | 2000 rows |
| interlaced PNG | 3000×2000 | — | `image::decode` fails; `png` gave 1039 rows through **Adam7 pass 4 of 7** (a whole picture at 1/4 resolution) | pass 6 | pass 7 |
| PNG header claiming 100000×100000 | 100000×100000 from 74 bytes | — | `image::decode` with default `Limits`: `Memory limit exceeded` at once | — | — |

What this shows:

- **Dimensions and format** came from the first 4 KiB of every image tried, a best case: these fixtures' APP1 is
  2 KB.
  - A real JPEG can put a 64 KB Exif APP1 and ICC, XMP or MPF segments before its SOF. Each segment is at most
    64 KB, because the JPEG segment length is 16-bit (ITU-T T.81 Table B.9; the Exif APP1 limit is CIPA
    DC-008-2012, Exif 2.3).
  - A TIFF's first-IFD offset may point anywhere in the file.
  - So the header is found by walking markers, not by reading a fixed prefix (§5.3).
- **WebP, TIFF, GIF and BMP partial decode was not measured.** HEIC is not an image kind sopen opens (spec §3).
- **JPEG decodes from a prefix:** image 0.25.10 runs zune-jpeg with `set_strict_mode(false)`
  (`src/codecs/jpeg/decoder.rs:34-37`). zune-jpeg fills what is missing with grey instead of failing
  (`mcu.rs:202-209`, `mcu_prog.rs:111-120`).
  - A progressive JPEG gives a whole, coarse picture at 10 % of its bytes.
  - A baseline JPEG gives the top rows.
  - `JpegDecoder::new` reads its whole input first (`read_to_end`, `decoder.rs:30-40`). So each preview is one
    decode of the prefix so far, costing 30–440 ms at 24 Mpx. Previews are paced, not run per block.
- **PNG does not decode from a prefix through `image`.** The `png` crate's row reader delivers every row it has
  before `UnexpectedEof`, and documents that "decoding can resume when more data becomes available"
  (png 0.18.1 `decoder/stream.rs:118-124`). An interlaced PNG gives full-frame passes early.
- **A pixel budget is free to enforce:** the header gives the size before any pixel data. `Limits::default()` is
  `max_alloc: Some(512 * 1024 * 1024)` (image 0.25.10 `src/io/limits.rs:48-57`).

## 3. Prior art

How players and kernels size "read ahead, keep behind". Values come from source at the tag linked. The ones the
design leans on (mpv, Chromium, Shaka, Linux, sshd) were re-checked in the downloaded source.

| System | Forward | Behind | Unit / block | Source |
|---|---|---|---|---|
| mpv demuxer cache | `max_bytes = 150 MiB`; readahead 1 s by default | `max_bytes_bw = 50 MiB` | packets | [demux.c L139-L146 @v0.42.0](https://github.com/mpv-player/mpv/blob/v0.42.0/demux/demux.c#L139-L146), [options.rst L4339-L4469](https://github.com/mpv-player/mpv/blob/v0.42.0/DOCS/man/options.rst#L4339-L4469) |
| Chromium `<video>` (MultiBuffer) | preload 10 s at the measured bitrate, clamped to 2–50 MiB; default bitrate 200 KiB/s | keep 2 s after the play position | **32 KiB blocks** (`kBlockSizeShift = 15`) | [multi_buffer_data_source.cc L29-L55](https://github.com/chromium/chromium/blob/159.0.8154.29/third_party/blink/renderer/platform/media/multi_buffer_data_source.cc#L29-L55), [.h L307-L313](https://github.com/chromium/chromium/blob/159.0.8154.29/third_party/blink/renderer/platform/media/multi_buffer_data_source.h#L307-L313), [url_index.cc L24](https://github.com/chromium/chromium/blob/159.0.8154.29/third_party/blink/renderer/platform/media/url_index.cc#L24) |
| Shaka Player (DASH/HLS) | `bufferingGoal: 10` s | `bufferBehind: 30` s | segments | [player_configuration.js L180-L193 @v5.3.5](https://github.com/shaka-project/shaka-player/blob/v5.3.5/lib/util/player_configuration.js#L180-L193) |
| dash.js (YouTube-style ABR) | `bufferTimeDefault 18`, `bufferTimeAtTopQuality 30` s | `bufferToKeep 20` s, pruned every 10 s | segments | [Settings.js L1318-L1323 @v5.3.0](https://github.com/Dash-Industry-Forum/dash.js/blob/v5.3.0/src/core/Settings.js#L1318-L1323) |
| VLC prefetch filter | 16 MiB buffer, 16 MiB reads; a seek under 16 KiB reads through | — | bytes | [prefetch.c L528-L541 @3.0.24](https://github.com/videolan/vlc/blob/3.0.24/modules/stream_filter/prefetch.c#L528-L541) |
| Linux page-cache readahead | first window = the request rounded up ×4 or ×2; then ×4 while small, ×2 up to the max (128 KiB default). A "standalone, small random read" is read as is and does "not pollute the readahead state". | — | pages | [readahead.c L385-L418, L600-L650 @v7.3](https://github.com/torvalds/linux/blob/v7.3/mm/readahead.c#L385-L418) |
| OpenSSH `sftp` client | 32 KiB × 64 requests by default; buffer raised to the server's `limits` read length. Outstanding requests start at 1 and grow by one per reply. | — | requests | [sftp-client.c L59-L63, L552-L578, L1820-L1830 @V_10_6_P2](https://github.com/openssh/openssh-portable/blob/V_10_6_P2/sftp-client.c#L552-L578) |

What carries over:

- **Forward buffers are sized by time at the stream's bitrate**, and bounded by bytes: 10 s for Chromium and
  Shaka, 18–30 s for dash.js. mpv's 150 MiB is a desktop player's "watch the whole film offline" setting.
  sopen's windows are views that close, so it follows the browser.
- **Keep a little behind** (Chromium 2 s), so a short scrub back is free.
- **Grow on sequential access, reset on a random one**, as Linux does: a far jump must not drag a big
  readahead with it.
- **Small blocks:** 32 KiB in Chromium. Here a block is a cache unit, and §2.2 shows request size does not
  matter at equal bytes in flight.

`sshd_config(5)`: "MaxSessions … the maximum number of open shell, login or subsystem (e.g. sftp) sessions
permitted per network connection … The default is 10"
([sshd_config.5 L1414-L1424](https://github.com/openssh/openssh-portable/blob/V_10_6_P2/sshd_config.5#L1414-L1424)).
Every sopen channel to a host shares one master, so channels per host are a budget (§4.2).

## 4. Design

### 4.1 Shape

```
viewer (hex · text · image · media)          ffmpeg / ffprobe (child processes)
   │ Reader::try_read (frames, non-blocking)      │ GET /<token>/<name>  Range: bytes=N-
   │ Reader::read_at  (workers, blocking)         ▼
   ▼                                        stream::http  (127.0.0.1, one per media window)
stream::Source ── BlockCache (64 KiB blocks, RAM LRU) ── Prefetcher (per-reader windows, byte cap)
   │                                                            │
   └──────────── remote::Ranged  (one SFTP channel + handle, its own thread) ◄──┘
                 local file: no Source at all — viewers keep reading the file in place
```

- A **remote** file opened for viewing gets one `Source`. That is one SFTP channel over §4's master, holding one
  open handle for the viewer's lifetime.
- Nothing is written to disk for viewing.
- A **local** file keeps today's path: `pread` on a `File`. Spec §3a already reads local files live, and the cache
  would only add copies. The viewers take a small `Bytes` trait implemented by both `File` and `stream::Reader`,
  so one viewer code path serves both (§5).

### 4.2 `remote::Ranged`: the byte transport

- **A channel of its own.** `SftpRemote::open_ranged(path)` starts a new SFTP channel over the running master.
  It runs on a thread with its own current-thread runtime, so the viewer's `Origin` connection stays free for
  Save and Export. It opens the file once and `fstat`s the handle.
- **Reads.** A read of `len` at `off` goes through a `File` clone with `start_seek(Start(off))`. It loops on short
  reads.
- **Server read limit.** The product cannot read the server's limit: `Sftp::max_read_len` sits behind the crate's
  `__ci-tests` feature. Without `limits@openssh.com` the crate assumes 20480 bytes
  (`OPENSSH_PORTABLE_DEFAULT_DOWNLOAD_BUFLEN`, lowlevel 0.7.4 `lib.rs:43`). So nothing assumes 261120: a short
  reply is followed by the rest, and the request size is learned from the first reply.
- **Every request carries its own offset**, set with `start_seek(Start(off))` on a clone. The crate's `File::read`
  advances its cursor by the length *asked* (capped at the read limit), not the length returned
  (`file/mod.rs:403-428` in 0.15.12). Today's `read_with` relies on that cursor; after a short reply it would skip
  bytes, which `fetch`'s size check then refuses.
- **The budget is bytes in flight, not requests** (§2.2). The crate pipelines whatever is outstanding. Requests
  are buffered and flushed every 0.5 ms by default (`flush_interval`), or at once when more than 100 are pending
  (`max_pending_requests`), per `options.rs` in 0.15.12. The 64-request cells of §2.2 ran on those defaults.
- **Interface:**
  - `size()`;
  - `fstat()` (the handle) and `stat_path()` (the name);
  - `read(off, len) -> ticket`, where a completion carries `Result<Bytes>` and the time it took;
  - `close()`.
- **Close.** `close()` aborts every task, drops every `File` clone, then `Sftp::close()`s under a 5 s bound, on
  the transport's thread. §2.2 measured that a live clone makes `close` hang.
- **Implementations:**
  - SFTP;
  - local, over a `File`, used by tests only (a local file never gets a `Source`);
  - a `Fake`, for tests: latency, bandwidth, a server that answers **in order** like `sftp-server`, injected
    errors, and the file changed in place or replaced.
- **Channel budget.** `MaxSessions` defaults to 10 per connection (§3), shared by every sopen process on that
  host. A viewer uses 2 channels (its `Origin` and its `Ranged`). The browser's listing uses 1.

### 4.3 `stream::BlockCache`

- **Blocks are 64 KiB**, one request each. §2.2 shows request size does not matter at equal bytes in flight. A
  64 KiB random read costs p50 38 ms idle, and a hex or text screen needs one block. (Chromium uses 32 KiB.)
- **RAM only: 64 MiB per `Source`** (1024 blocks), least recently used out first.
- **Protected from eviction:** the blocks inside any live reader's `[cursor − behind, cursor + ahead]`.
- **Bulk readers:** their blocks (waveform, index pass, export) are inserted at the cold end, so a pass over the
  whole file does not evict what playback needs. This is the `POSIX_FADV_NOREUSE` idea.
- **Process total:** at most one `Source` per open viewer, so a browser plus a standalone window stays ≤ 2 ×
  (64 + 4) MiB.
- **No disk spill in v1.** The measured patterns stay inside 64 MiB: open + 10 s + a far seek fetched 13–23 MB.
  The owner accepted that a far jump "takes a moment" ("멀리 이동하면 이거는 시간이 좀 걸리는데"). A spill file
  would add a sparse 0600 file, a bitmap, and cleanup rules under §5's privacy rules, for refetches that cost
  ~0.2–0.6 s. Revisit if the usage log (§8 of the spec) shows sessions that fetch the same bytes twice. To make that
  visible, add `bytes_fetched` to the usage line (§9).

### 4.4 `stream::Prefetcher`

**Readahead state per reader.** A reader is a viewer cursor, one HTTP connection, or a worker such as the text
index pass. The rule is Linux's on-demand readahead (§3):

- The window starts at **0** when the reader opens or jumps.
- A **sequential hit** (the next access starts at or inside the last window) sets the window to
  `max(1 block, 2 × window)`, up to the reader's `ahead`.
- A **jump** (anything else) resets it to 0 and fetches only what was asked.

| Reader | `ahead` max | `behind` kept |
|---|---|---|
| Interactive (hex, text) | 1 MiB | 256 KiB |
| Media (one HTTP connection) | 10 s at the probed bit rate, clamped to 2–32 MiB (Chromium's rule, §3) | 2 s at that rate, ≥ 1 MiB |
| Bulk (waveform, index pass, export, full image) | 1 MiB | none, cold insertion |

**Priority.** A **demand** fetch (a block a reader is waiting for, or a frame is showing as loading) goes out
before any prefetch. Requests already sent cannot be recalled: SFTP has no cancel, and the server answers in order
(§2.2). So a demand fetch waits behind the **bytes already in flight**. That is the far-seek bound.

**Bytes-in-flight cap: `cap = clamp(2 × rate × rtt_min, 256 KiB, 4 MiB)`.**

- `rate` is the delivered byte rate (EWMA over the last second of completions).
- `rtt_min` is the fastest small reply seen (the open's `fstat`, a single-block demand).
- Prefetch is issued only while `in_flight + block ≤ cap`. Demand fetches may go one cap over it.
- On the measured link (4–10 MB/s; `rtt_min` ≈ 18–20 ms, the fastest 64 KiB read) that is 256–400 KiB. §2.2
  shows ~256 KiB in flight already at the link's ceiling, and same-channel demand latency at 510 KiB in flight of
  51–89 ms p50.
- The wait a jump adds is `cap / rate ≈ 2 × rtt_min`.
- The cap is testable without a network: on the `Fake`, count the bytes sent ahead of a demand request; the
  count must stay ≤ cap.

**Cancel on jump.** A jump stops issuing prefetch for that reader at once. What is in flight (≤ cap) still
arrives and is cached, which is useful if the reader comes back.

### 4.5 `stream::Source`: state, change, errors

**Fingerprint.** At open: `stat(path)`, open the handle, `fstat(handle)`. The `(size, mtime)` of the handle is the
`Fingerprint` (spec §5's type, without a hash).

**What the handle shows** (§2.2):

- It keeps reading the file it opened even when another file is renamed over the path.
- It shows writes made in place.

**Polling.** Every 2 s while the viewer is on screen, and at most once per 2 s before a demand miss: `fstat(handle)`
and `stat(path)`. That is two small requests, queued like any other.

**States:**

| State | Meaning | What the viewer does |
|---|---|---|
| `Live` | unchanged | normal |
| `Replaced` | the path names another file; the handle's file is unchanged | keeps showing the opened version consistently; one status line: "the remote file was replaced; showing the version opened — R reloads" |
| `Changed` | the handle's size or mtime moved | **drops the whole cache** (blocks from before and after must not mix), keeps the screen it has, reads no more; "the file changed on the remote — R reloads" |
| `Failed(reason)` | a read got an SFTP status error, or the channel closed | the screen stays; the reason on the status row; the next interaction retries after ≥ 1 s |
| `Closed` | — | — |

On reload the source re-opens and takes a new fingerprint. The text viewer re-indexes, the media window re-probes,
and the hex window keeps its offset.

**Retry.** A retry reconnects through the viewer's existing `Reconnect` closure (`src/gui/mod.rs`), re-opens and
`fstat`s. Then:

- same `(size, mtime)` → `Live`, cache kept;
- otherwise → `Changed`.

SFTP v3 attributes carry no inode, so after a reconnect only `(size, mtime)` can be compared.

**Limit, as in spec §5.** mtime has whole seconds over SFTP. A same-size rewrite within the same second is not
detectable without reading every byte, and is not detected.

**Timeouts.**

- A demand block pending over 1 s shows "loading 0x… (N KiB)".
- Over 10 s it shows "the remote is slow".
- §5's 30 s request / 60 s stall bounds make it `Failed`.
- Prefetch failures are silent: they are only retried as demand.

### 4.6 `stream::http`: the byte server for ffmpeg

- **One per media window**, started when the window opens and dropped with it.
  - Bound to `127.0.0.1:0`.
  - Serves exactly one path, `/<token>/<file name>`. The token is 128 bits from the OS (`getentropy`, `libc`
    crate), not `random_hex()`, which is not secret-grade.
  - Accepts only `GET` and `HEAD`.
  - Requires `Host: 127.0.0.1:<port>` (the DNS-rebinding fix of Transmission's CVE-2018-5702,
    [PR #468](https://github.com/transmission/transmission/pull/468)).
- **Responses:** `Range: bytes=N-` and `bytes=N-M` → `206` with `Content-Range` and `Accept-Ranges: bytes`; past
  the end → `416`; no `Range` → `200`. Every response carries `Connection: close`.
- **Streaming.** Each connection is a Media reader. It streams with blocking `read_at` (std threads, at most 4
  connections; the player, scrubber and probe use ≤ 3 at once). TCP backpressure from ffmpeg paces it.
- **Errors.** A read error ends the connection. ffmpeg sees EOF and the window shows the `Source`'s own error. The
  window learns about errors from its `Source`, not from parsing ffmpeg's stderr.
- **ffmpeg input options:** every ffmpeg/ffprobe input gets `-protocol_whitelist http,tcp`, and mp3
  playback/scrub gets `-fflags +fastseek` (§2.3).
- **The token is in the path for every client.** ffprobe could read it from a file (`-/headers`), but ffmpeg cannot
  (§2.3). Once one argv carries it, hiding it from the other buys nothing. The residual is in §7.

### 4.7 The interface (`src/stream/mod.rs`)

Every remote viewer builds on this interface, and the edit research reads through it too
([`streaming-edit.md`](streaming-edit.md)):

```rust
/// One remote file opened for viewing. Cheap to clone (Arc); the last drop closes it.
pub struct Source { /* … */ }
impl Source {
    pub fn open(transport: Box<dyn remote::Ranged>, wake: Waker) -> Result<Source>;
    pub fn size(&self) -> u64;                 // the handle's size at open
    pub fn opened(&self) -> remote::Fingerprint; // (size, mtime) at open; sha256 None
    pub fn state(&self) -> State;              // Live | Replaced | Changed | Failed(String) | Closed
    pub fn reader(&self, kind: ReaderKind) -> Reader;
    pub fn fetched(&self) -> u64;              // bytes fetched so far (usage line, tests)
    pub fn reload(&self) -> Result<()>;        // re-open, new fingerprint, cache dropped
}
pub enum ReaderKind { Interactive, Media { bytes_per_s: u64 }, Bulk }
/// A cursor with its own readahead window (§4.4).
pub struct Reader { /* … */ }
impl Reader {
    /// For frames: the bytes when every block is cached, else Pending (the fetch is queued,
    /// `wake` is called on arrival). Never blocks.
    pub fn try_read(&mut self, off: u64, len: usize) -> Poll<Result<Bytes>>;
    /// For workers and HTTP connections: waits; Err(Cancelled) once `cancel` is set,
    /// Err(Changed) when the source changed, Err(reason) when it failed.
    pub fn read_at(&mut self, off: u64, buf: &mut [u8], cancel: &AtomicBool) -> Result<usize>;
}
/// What a viewer reads: implemented by `std::fs::File` (local, in place) and by `Reader`.
pub trait Bytes { fn len(&self) -> u64; fn try_read(&mut self, off: u64, len: usize) -> Poll<Result<bytes::Bytes>>; /* … */ }
```

`Waker` is egui's `request_repaint` for the viewer's context.

## 5. Per-viewer integration

### 5.1 Hex (and the binary dialog)

The hex view is what the owner's 24 GB model file hits: `.safetensors`, `.gguf` and `.bin` are kind "other".

- **No fetch.** The dialog's "Open in hex editor" (and Enter in the browser, and `--hex`) opens a `Source` and the
  window at once.
- **Reads.** `Pages` (`src/gui/hex.rs:136-181`) keeps its 4 KiB pages and 64-page LRU, but reads through `Bytes`.
  A page not yet there is **Pending**, not empty. Its rows show `··` in the skin's dim colour, the status row says
  "loading 0x…", and the frame repaints when it arrives.
  - Today a failed page is held empty and shows as nothing (`hex.rs:164-172`). Failure becomes the status row's
    `Failed` text, and the page is retried.
- **Prefetch.** One Interactive reader at the cursor's row. Scrolling down grows the window to 1 MiB ahead.
  `Cmd/Ctrl+G` and Home/End are jumps.
- **Edits.** Today `changed`, `differ` and `changed_count` (`hex.rs:244-256`, `:325-345`) read the file's byte for
  each patch synchronously, and count a byte that cannot be read as changed. With Pending pages that would
  misreport:
  - each patch keeps the **original byte it replaced**, read when the edit is typed (the page is on screen, so it
    is present);
  - a byte whose page is Pending cannot be typed over until it arrives.
- **Save.** How Save works without a whole local copy belongs to [`streaming-edit.md`](streaming-edit.md). Until
  that lands, **Save on a streamed file first fetches the whole file**, with progress and Esc. This is today's
  fetch, now pipelined (§6).
  - The fetch must match `Source::opened()`; it is refused when the source is `Changed`.
  - Its SHA-256 becomes spec §5's baseline, as today.
- **"Save to…"** already streams (`download_to`, `src/gui/binary.rs:673-695`). It gets pipelined reads (§6) and
  a cancel (§6, §10).

### 5.2 Text

Today: one indexing pass over the whole file, then `pread` paging (`src/gui/text.rs:129-221`, `:361-432`). The
lazy design:

- **First screen from byte 0 at once.** The view shows lines from the first block while the index grows.
- **A file ≤ 16 MiB** (= `HIGHLIGHT_MAX`, the highlighting bound already in the spec) is indexed whole, in the
  background, by a Bulk reader: 16 MiB ≈ 2–4 s on the measured link. Line numbers, the line count and
  highlighting then work exactly as today. Nothing changes for small files except that the first screen comes
  first.
- **A file > 16 MiB** is indexed only as far as its bytes have been read: by scrolling, by the Interactive
  reader's readahead, and by a Bulk pass capped at the first 16 MiB. The marks are today's (every 256 lines or
  64 KiB), kept in a prefix index that only grows from 0.
- **Position.** The place on screen is a **byte offset of a line start**, with the line number when the index
  reaches it (`Option<u64>`).
  - The gutter shows line numbers inside the indexed prefix, and past it the byte offset in the skin's dim colour
    (`@1.2G`, or `0x4B0000000`).
  - The status row says "lines known to 3.1 MB of 24 GB".
- **The scroll bar is by bytes** while the line count is unknown, and by lines once the whole file is indexed.
- **A jump** (the scroll bar, End, a byte offset) reads the block at the target and starts at the first line start
  after it. The lines before it are found going **backwards**: a page up reads the block before, splits at
  newlines, and joins the partial first line when the block before that arrives. That reads at most a
  `SPAN` (64 KiB) back, the bound today's index already gives a line.
- **"Go to line N"**, and End when the user asks for the line count, runs the Bulk index pass **on demand** from
  the last mark to N. It shows progress ("indexing… 1.2 GB of 24 GB") and Esc stops it. It is the only thing that
  reads a large file through, and only when asked.
- **Highlighting** stays bound to the indexed prefix of files ≤ 16 MiB. Larger files are plain, as today.
- **Wrapping** (feat/text-wrap) places the screen at `Pos { line, off }`: a line number and the byte of that line
  the top row starts at. Its rows depend on the line's start, its restart points (every 16 KiB **counted from the
  line start**) and the width. Its memos are keyed by line number.
  - Streaming keys them by the line's start offset instead. `Span` already carries `start`.
  - A jump into a line whose start lies more than `SPAN` back, with no newline found, has no known line start. Its
    rows then start on a restart grid anchored at absolute 16 KiB offsets. Those rows can differ from the ones the
    same line gets once its start is known, and the view says "line start not known".
  - Build streaming on wrap **after text-wrap merges** (§9).
- **Re-reads.** The local-file re-read rules (`TextDoc::now`, §3a) stay for local files. A remote file follows
  §4.5: `Changed` freezes the page and offers R; `Replaced` keeps showing the opened version. A remote file is
  still not followed like `tail -f` (§3a).

### 5.3 Images

- **Header first, by walking, with a 1 MiB cap.** The probe finds the header by reading, not from a fixed prefix:
  - **JPEG:** walk the marker segments (each ≤ 64 KiB) to the SOF, keeping the Exif APP1 for its thumbnail.
  - **PNG, GIF, BMP, WebP:** the header is at the start (PNG IHDR at byte 16; WebP's VP8/VP8L/VP8X chunk header
    first).
  - **TIFF:** follow the first-IFD offset, one more read wherever it points.

  Then `ImageReader::into_dimensions` runs on the bytes up to the header. It cannot be given the source itself:
  image 0.25.10's `JpegDecoder::new` calls `read_to_end` (`codecs/jpeg/decoder.rs:30-40`), which would pull the
  whole file. Past 1 MiB without a header, the window shows the size line and streams the file as below. The
  window shows the thumbnail scaled up, with "W×H, N MB, loading N %".
- **Pixel budget before any pixel data.** The header gives `W × H × channels × depth`. Above `max_alloc` (the
  image crate's 512 MiB default, which sopen already gets today) the window says "too large to decode here:
  W×H = N Mpx" and offers the hex view.
  - Measured: a 74-byte PNG claiming 100000×100000 is refused at once.
  - A file that is not a valid image of its extension goes to the hex view, as `Doc::decode` failures do now.
- **Then the whole file streams** with progress and Esc. An editable image needs every byte: decode, and Save's
  backup of the original. The read hashes as it goes, so spec §5's SHA-256 baseline comes out of it as today's
  fetch gives it.
  - Progressive previews come only where they are cheap and real (§2.4):
    - a **progressive JPEG** is decoded from its prefix at about 25 / 50 / 75 % (each decode 50–330 ms at 24 Mpx, on
      a worker);
    - an **interlaced PNG** shows its completed Adam7 passes through the `png` crate's row reader;
    - a baseline JPEG shows its top rows from the prefix.
  - Everything else (plain PNG, WebP, BMP, TIFF, GIF) shows the thumbnail or the header line until complete.
- The decoded image, its edits and Save are unchanged (the full bytes are in memory, as today).

### 5.4 Audio and video

- **`Media::probe`, `Player`, `Scrubber`, the waveform and Export take an `Input`** that is a path (local) or the
  window's server URL (remote). feat/media-edit already has such an `Input` (`src/ffmpeg.rs:49-76` on that
  branch), so build on it after it merges (§9).
- **The `.ts` refine** (`kind::refine_local`) probes the URL.
- Every remote input gets `-protocol_whitelist http,tcp`. mp3 playback/scrub gets `-fflags +fastseek`.
- **Waveform.** It needs every audio sample, so it stays a whole read, and the data says it can be bounded:
  - It is **progressive**: the decoder thread publishes partial `Peaks` every ~250 ms (today it publishes once, at
    the end: `src/gui/media.rs:170-177`). The timeline draws what is known and a dim band for the rest.
  - It reads through a **Bulk** reader on its own HTTP connection, so it never evicts playback's blocks and
    yields to demand.
  - It runs only for audio-only files, as today.
  - **Peaks computed on the remote with its `ffmpeg` are not proposed.** AGENTS.md: "Never install, upload or run a
    helper on the remote." See §10.
- **Export** reads only the cut (§2.3: 12 % of the file for 10 s of a 2 min video) and publishes as today
  (§5 of the spec). Its progress stays ffmpeg's time-based progress.

## 6. Whole-file actions

Viewing never needs the whole file. These actions do, and each streams with progress and a cancel. None blocks a
window before it shows.

**Shared fix: pipelined sequential reads.** `SftpRemote::read_with` sends one 256 KiB request and awaits it before
the next (`src/remote/sftp.rs:678-714`). That runs at 2.5 MB/s on the measured link. Keeping ≥ 256 KiB–1 MiB in
flight reaches the link's ceiling, 1.5–4× faster on the same link (§2.2). Every `read_to` user gains:

- `fetch` (the image download, the hex Save fallback);
- `download_to` ("Save to…");
- `write_atomic`'s re-read of the target up to 256 MiB.

Chunks are delivered in order. Cancel is checked between chunks, as `Stoppable` does today.

| Action | Reads | Progress | Cancel |
|---|---|---|---|
| Image decode (§5.3) | the whole file, Bulk reader, into memory | bytes / size, with previews | Esc / close: the reader stops; nothing is kept |
| Hex Save before streaming-edit lands (§5.1) | the whole file into the cache, as today | the status row (bytes) | Esc, as the browser's fetch has today (`fetch_claimed_cancellable`) |
| "Save to…" | the whole file into the private staging file, as today (§6a) | the dialog's bytes / size | **new:** Esc and closing cancel it. The staging `part` and private folder are removed, and nothing is published. Today Esc does nothing during a transfer and closing waits (§6a). Changing that is a spec change (§10). |
| Media Export | only what the cut needs, through the URL (§2.3: 14.7 MB for 10 s) | ffmpeg's time progress, as today | as today (the second close) |
| Waveform | every byte of an audio-only file, Bulk reader, low priority | the waveform fills in | closing the window kills the decoder and drops its reader |
| Stems (feat/stems) | every audio sample through the URL; the stems run decodes from ffmpeg's output | its own | the run's own cancel; the reader is dropped with it |
| "Go to line N" / line count (§5.2) | from the last mark to N | "indexing… X of Y" | Esc |

## 7. Robustness and security

**The loopback server**

- **Who can connect.** It is bound to `127.0.0.1` only, and requires its `Host` header, so a web page cannot
  reach it by DNS rebinding.
- **Path and method.** It serves one path and `GET`/`HEAD` only. A wrong token gets 404 with no body, after the
  same work as a right one.
- **Lifetime.** It lives exactly as long as its media window, and drops its listener with the window.
- **ffmpeg inputs.** Each one gets `-protocol_whitelist http,tcp`. The default whitelist already refuses `file:`
  for a URL input (§2.3), and the explicit one also drops `udp`, `rtp`, `https` and `tls`.

**Residual risk: the token is on ffmpeg's command line.** Other **local accounts** on the same machine can read it
while the window is open: `/proc/<pid>/cmdline` on Linux unless `/proc` is mounted `hidepid`, and `ps` on the Mac
(§2.3). With it they can read the viewed file through the server. Processes of the same user are trusted, as spec
§6a already says.

Two ways to close it, neither measured yet:

- **(i) Check the peer of every accepted connection.** On Linux: the socket's uid from `/proc/net/tcp{,6}`, which
  must equal sopen's. On the Mac: the local port must belong to one of the ffmpeg children sopen started, found
  with `proc_pidfdinfo`. Owner-only like the cache, but platform code.
- **(ii) Accept the residual and say so in the spec.** The machines are the owner's.

The default is (i) on Linux and (ii) on the Mac until (i) is spiked there (§10).

**Errors mid-stream** (§4.5):

- An SFTP status error marks the block failed. The viewer's status row names it, the screen stays, and the next
  interaction retries.
- A lost channel shows at once. Connection closed is immediate; a silent link is caught by the master's keepalive
  (~30 s) or the request timeout. It is retried through `Reconnect`.
- Prefetch failures are never shown.

**Cancellation on close.**

- The viewer's drop closes its `Source`. Prefetch stops at once; at most `cap` bytes still arrive and are dropped.
- Every `File` clone is dropped before `Sftp::close()`, which runs bounded to 5 s on the transport's thread.
- The media window already kills its ffmpeg children on drop.
- No window close ever waits on the network.

**Bounds.**

| Resource | Bound |
|---|---|
| RAM per `Source` | 64 MiB cache + ≤ 4 MiB in flight + the text index (16 bytes per 64 KiB of prefix read, i.e. 6 MB for a fully indexed 24 GB file) |
| Disk for viewing | **0 bytes** |
| Disk for whole-file actions | as today: cache copy for the hex-Save fallback, staging for "Save to…" |
| Image | as today (the encoded bytes plus the decoded pixels), bounded now by the pixel budget before the fetch |
| SFTP channels per viewer | 2 |

## 8. Acceptance scenarios

**The reference rig.** A remote reached through sopen's master over a link like §2's (12–40 ms RTT, 2–10 MB/s).
Byte bounds are exact. Time bounds are for that link.

**Test the byte bounds without a network**, on `remote::Ranged`'s `Fake`. It answers in order, at 20 ms latency and
5 MB/s. Every scenario's "fetched" is `Source::fetched()`.

1. **24 GB model file, first screen.** Open a remote 24 GB file (`sopen host:/…/model.safetensors`, Enter on the
   dialog).
   - The hex window shows its first rows within **1.0 s** of Enter (measured: open + `fstat` 148–161 ms, first
     64 KiB 21–74 ms).
   - At most **1 MiB** is fetched by then.
   - No file appears under the cache directory.
2. **24 GB, scrolling stays bounded.** Hold PgDn for 30 s, then drag the scroll bar to the middle and back to the
   top.
   - RSS grows by at most **80 MiB** over the window as opened.
   - Fetched ≤ bytes shown + 1 MiB per sequential run.
   - The cache directory is unchanged.
3. **24 GB, far jump.** Cmd/Ctrl+G `0x500000000` (20 GiB).
   - The rows there show within **1.0 s** (measured: 20–30 ms per 64 KiB at 20 and 24 GiB).
   - At most 1 MiB more is fetched for the jump.
   - On the `Fake`: the bytes delivered after the jump for the old place are ≤ `cap`.
4. **Demand never waits behind more than the cap.** On the `Fake`, with a reader streaming sequentially:
   - the bytes sent ahead of any demand request are ≤ `cap` ≤ 4 MiB;
   - with `rtt_min` = 20 ms and 5 MB/s, the demand completes within 2 × cap / rate + 2 × RTT.
5. **Text, 4 GiB remote log.**
   - The first screen shows within 1.0 s, with ≤ 1 MiB fetched.
   - End shows the last screen within 1.0 s, ≤ 1 MiB more, with byte offsets in the gutter and "lines known to …"
     on the status row.
   - PgUp from there shows the previous screen without reading more than 64 KiB per page.
   - "Go to line 10 000 000" shows progress and stops on Esc with nothing more fetched.
6. **Text, small file.** A remote file ≤ 16 MiB shows its first screen before its index finishes. Once indexed,
   it has the line count and highlighting it has today.

**Media bounds.** The §2.3 figures below came from a fixed readahead of 0 or 2 MiB per connection, not §4.4's
policy. So byte bounds for media are stated as **fetched ≤ bytes ffmpeg was sent (deduplicated) + each live
reader's `ahead` + cap**. The server counts what it sends and the `Source` what it fetches, both at the instant named.

7. **Video, moov at the end, 123 MB.**
   - Probe + first frame within **1.5 s** (measured 0.30 + 0.10 s).
   - At the first frame, fetched ≤ sent + ahead + cap (measured with no readahead: 2.08 MB, 1.7 %).
   - A far scrub (80 %) within **1.5 s** (measured 0.41–1.06 s across the three containers).
   - After open + 10 s of play + one far seek, the same invariant holds. Measured with no readahead: 19.1 MB of
     123 MB.
8. **mp3, 600 s.**
   - A seek to 480 s gives PCM within **0.5 s** (measured 0.14 s).
   - Between the seek and its first PCM, the seek fetches ≤ 1 MiB + ahead + cap. Measured: 0.53 MB with
     `+fastseek`, against 18.35 MB without it (both readahead settings), which reads the file through.
   - Export from 470–480 s keeps exact seeking (no `+fastseek`): its output is 10.000 s.
9. **Waveform.** For a remote 24 MB mp3:
   - the first peaks show within 1 s of opening;
   - the overview completes having fetched ≤ 1.05 × the file;
   - a seek while it runs still gives PCM within 0.5 s.
10. **Image.** A remote 6000×4000 progressive JPEG (16.6 MB):
    - size and Exif thumbnail show within **1.0 s**, with ≤ the bytes up to its SOF + one block fetched (64 KiB
      for this file; the probe's cap is 1 MiB);
    - a preview of the whole frame shows by 25 % fetched;
    - Esc stops the fetch;
    - a PNG whose header claims 100000×100000 is refused with one block (64 KiB) fetched;
    - a JPEG with a 64 KB Exif APP1 and an ICC segment before its SOF still shows its size and thumbnail, having
      fetched ≤ 1 MiB.
11. **Remote change while viewing.**
    - Rewrite the file in place: the viewer says "changed on the remote" within 2 s of its next poll and shows no
      newly fetched bytes after that.
    - Rename another file over it: the viewer keeps showing the opened version and says it was replaced.
    - R reloads both.
12. **Close during prefetch.** Close a window while a 4 MiB prefetch is in flight:
    - the window goes at once;
    - the process exits within 5 s;
    - on the `Fake`, nothing is requested after the close.
13. **Lost connection.** Kill the ssh master while scrolling:
    - the status row says so on the next read;
    - the screen stays;
    - a key after the master is back (sopen restarts it) continues from the cache, with the same fingerprint.
14. **HTTP server.**
    - A request without the token gets 404 and 0 bytes.
    - A wrong `Host` gets 403.
    - `bytes=N-M` gets exactly M−N+1 bytes; past the end gets 416.
    - After the window closes, the port refuses connections.
    - Every ffmpeg/ffprobe invocation for a remote input has `-protocol_whitelist http,tcp`.
15. **Local files are untouched.** A local file opens in every viewer with no `Source` created (a test hook
    counts them) and reads in place, as today.
16. **"Save to…" of a 24 GB file.**
    - It runs at the link's ceiling (pipelined, ≥ 1.5× today's single-request rate on the same link and minute).
    - Esc cancels it, leaving no file and no staging folder.

## 9. Work units

**Disjoint file ownership.** One owner per file; a unit that needs another's file asks its owner for an interface.

| # | Unit | Owns | Depends on | Gate it adds |
|---|---|---|---|---|
| W1 | **Transport.** Pipelined `read_with` (bytes-in-flight, short-read loop, ordered delivery), `open_ranged` + `Ranged` trait, SFTP / local / `Fake` impls, bounded close. | `src/remote/mod.rs`, `src/remote/sftp.rs`, `src/remote/local.rs`, new `src/remote/ranged.rs`, `src/remote/tests.rs`, `Cargo.toml`, `Cargo.lock` (`open_ranged` is a `RemoteFs` method with a default "not supported" body, so the wrapper impls in `src/testutil.rs` and `main.rs`'s tests need no change) | — | `local_server()` tests for `Ranged` (reads, short reads, `fstat`, rename-over keeps the old bytes, close with no live clone); scenario 16's speed-up on the `Fake` |
| W2 | **Stream core.** `Source`, `BlockCache`, `Prefetcher`, `Reader`, state/poll/reload, `Bytes` trait. | new `src/stream/mod.rs`, `cache.rs`, `prefetch.rs`, `source.rs`, `src/stream/tests.rs`; `src/lib.rs` (`pub mod stream`) | W1 | scenarios 3, 4, 11, 12, 13 on the `Fake`; cache ≤ 64 MiB under a 4 GiB random walk |
| W3 | **HTTP byte server.** Token from `getentropy`, Host check, Range/206/416, Media readers, lifetime, peer check on Linux. | new `src/stream/http.rs` | W2 | scenario 14 (std `TcpStream` client); an ffprobe-over-URL test that skips without ffmpeg (`SOPEN_REQUIRE_FFMPEG` in CI) |
| W4 | **Hex view streams.** `Pages` over `Bytes` with Pending/Failed, repaint on arrival, Interactive reader; Save falls back to a full pipelined fetch. | `src/gui/hex.rs`, `src/gui/hex/tests.rs` | W2 | scenarios 1–3; hex screenshot receipts with a Pending row |
| W5 | **Text view streams.** Prefix index, byte-anchored position, backward paging, Go-to-line pass. | `src/gui/text.rs`, `src/gui/text/*` | W2, **after feat/text-wrap merges** | scenarios 5–6 |
| W6 | **Image view streams.** Header probe, Exif thumbnail, pixel budget, progressive previews during a Bulk fetch. | `src/gui/image.rs`, new `src/gui/image/probe.rs` | W2 | scenario 10; receipts of the thumbnail and a mid-fetch preview |
| W7 | **Media streams.** `Input` = URL, input options, progressive waveform on a Bulk connection, Export via URL. | `src/gui/media.rs`, `src/gui/playback.rs`, `src/ffmpeg.rs`, `src/cut.rs` | W3, **after feat/media-edit merges** (its `Input`) | scenarios 7–9 with the fixtures generated by ffmpeg in the test (`lavfi`) |
| W8 | **Routing, dialog, spec.** Browser Enter / standalone / dialog / `--hex` open a `Source` instead of fetching; `.ts` refine over the URL; "Save to…" cancel; usage `bytes_fetched`; spec §3a/§6a/§9/§10/A6 and README rewritten. | `src/gui/browser/prepare.rs`, `src/gui/browser.rs`, `src/gui/mod.rs` (`Origin` holds the `Source`), `src/main.rs`, `src/gui/binary.rs`, `src/kind.rs`, `src/usage.rs`, `.prd/01-spec.md`, `README.md`, `AGENTS.md` | W4–W7 | scenarios 15–16; `just check` |

**Sequencing.**

- W1 → W2 → W3, W4, W5, W6, W7 in parallel → W8 last, since it wires them.
- `hex.rs` and `image.rs` are also the edit workstream's ([`streaming-edit.md`](streaming-edit.md)). Land W4 and W6
  first, then the edit units on top, or give each file one owner across both workstreams.
- `text.rs` waits for feat/text-wrap. `media.rs`, `ffmpeg.rs` and `cut.rs` wait for feat/media-edit.

**Tests stay off the network** (AGENTS.md): the `Fake`, `LocalFs` and the `sftp-server` child carry every test.

## 10. For the owner

Only scope and irreversible points. Each one has the default the work takes unless the owner says otherwise.

1. **Remote ffmpeg for waveform peaks: not done.** The owner's brief allowed peaks "computed on the remote host via
   ssh `ffmpeg` only if present". AGENTS.md says "The remote needs only sshd. Never install, upload or run a helper
   on the remote." Default: the waveform is decoded here, progressively (§5.4). Running a remote program needs the
   owner to change that rule.
2. **"Save to…" becomes cancellable.** Spec §6a says Esc does nothing during a transfer and closing waits for it.
   Message 17's scope says every whole-file transfer "can be cancelled". Default: Esc and closing cancel; nothing
   is published and the staging folder is removed.
3. **Token residual on the Mac.** Another local account can read the loopback URL's token from `ps` while a media
   window is open (§7). Default: peer check on Linux (i); on the Mac the residual stays named in the spec until the
   `proc_pidfdinfo` check is spiked.
4. **Spec text this replaces** (W8 rewrites it):
   - §9's "streaming playback before download completes" (out of scope until now);
   - §10's "consider streaming" trigger;
   - §3a's "A remote file is viewed from its cached copy";
   - A6 and §6a's "fetched to the cache, then … hex window".

## Appendix: how the numbers were taken

The spike crates and scripts were throwaway and are not in the repo. They used sopen's `Cargo.lock` (same crate
versions) and one ssh master started as `ssh -o ControlMaster=yes -o ControlPath=./cm -o ControlPersist=600 -f -N
<host>`. Every SFTP channel was `openssh::Session::resume(./cm)` + `Sftp::from_clonable_session`, as
`SftpRemote::connect` does.

**Fixtures.** On the remote's tmpfs, deleted afterwards: `head -c 4G /dev/urandom > big.bin`, and
`truncate -s 24G big24.bin` with a marker at 0. Media generated on the Mac and copied there:

```sh
ffmpeg -f lavfi -i "testsrc2=size=1920x1080:rate=30:duration=120,noise=alls=12:allf=t" \
       -f lavfi -i "sine=frequency=440:duration=120:sample_rate=48000" \
       -c:v libx264 -preset veryfast -b:v 8M -maxrate 8M -bufsize 16M -pix_fmt yuv420p -g 60 \
       -c:a aac -b:a 192k -ac 2 v_moovend.mp4
ffmpeg -i v_moovend.mp4 -c copy -movflags +faststart v_faststart.mp4
ffmpeg -i v_moovend.mp4 -c copy v.mkv
ffmpeg -f lavfi -i "sine=frequency=330:duration=600:sample_rate=44100" -ac 2 -c:a libmp3lame -b:a 320k a.mp3
```

Atom order was checked by walking the top-level boxes: `ftyp@0 free@32 mdat@40 moov@123077870` vs
`ftyp@0 moov@32 free@109535 mdat@109543`. The mkv's Cues element sits at 100 % of the file.

**SFTP (`sftpbench <cmd>`):**

| cmd | What it does |
|---|---|
| `limits` | the server's limits and extensions, plus one 1 MiB read |
| `ttfb N` | N fresh channels: open, `fstat`, first 64 KiB |
| `matrix 3 3` | sequential reads, request sizes {32K, 64K, 128K, 255K} × in flight {1…64}, 3 s per cell, 3 repetitions; requests kept in flight with spawned tasks on `File` clones |
| `contrast 6` | 64K×16, 128K×8 and 255K×4 alternated, 2 s each |
| `rand 40` | random 64 KiB reads; the first three were checked against `tail -c +N \| head -c 65536 \| sha256sum` on the remote |
| `randload` / `randload2` | 15 random 64 KiB reads, 100 ms apart, while a background loop keeps k × 255 KiB in flight on the same channel / on a second channel |
| `cancel 4 64` | 64 × 255 KiB queued, aborted after 300 ms, then one demand read; same channel, second channel kept, second channel dropped |
| `closetest` | `Sftp::close()` with no handle vs one live `File` |
| `rename <host>` | read through a handle before and after `mv new old`, an append and a truncate made with ssh |
| `at 0 21474836480 25769738240` | the 24 GiB file |
| `SFTPBENCH_LOCAL=1` | the same against `/usr/libexec/sftp-server` over pipes and a sparse local 4 GiB file |

The raw ssh ceiling was `/usr/bin/time ssh <host> 'head -c 67108864 big.bin' | wc -c`, three runs.

**Media (`httprange <path> <block KiB> <req KiB> <readahead blocks>` + `media_run.py`).**

- The server binds `127.0.0.1:0`, puts a random token in the path, caches blocks in a map, logs every request
  (Range, bytes sent, why it ended), and counts SFTP bytes fetched.
- The driver runs sopen's exact ffprobe/ffmpeg argument lists against the URL. It reads the counters only after
  they stop changing for 0.75 s, so the in-flight prefetch is counted in the step that caused it.
- "First frame" = time until 1280×720×4 bytes came out of the rawvideo pipe. "10 s play" = both decoders with
  `-re -t 10`, or a reader paced at 30 fps on the pipe.
- `FFIN` added input options (`-fflags +fastseek`, `-usetoc 1`, `-request_size 1048576`).
- `pipe_run.py` fed `ssh <host> cat file` into `ffprobe -i pipe:0` / `ffmpeg -i pipe:0` and counted the bytes fed.

**Security checks.**

- A one-shot TCP listener captured ffprobe's request with `-/headers <file>`.
- `ffmpeg -/headers` and `ffmpeg -/i` were tried.
- `ps -axo user=,args=` listed other users' processes.
- A two-line HLS playlist with a `file://` segment was served as `.mp4` and as `.m3u8`, with and without
  `-protocol_whitelist http,tcp`.

**Images (`imgpartial <dir>`).**

- Fixtures, made with ImageMagick + `jpegtran -progressive`: a 6000×4000 noisy test card as baseline and as
  progressive JPEG; 3000×2000 PNG, plain and `-interlace PNG`. A 160×107 JPEG thumbnail was put in an Exif APP1
  (IFD1 0x0201/0x0202), built by hand.
- Each file was cut at 10/25/50/75/100 %. On each cut: `ImageReader::into_dimensions` on 4 KiB and 64 KiB,
  `image::ImageReader::decode`, `zune_jpeg::JpegDecoder` (non-strict), and `png::Reader::next_interlaced_row` until
  error. "Filled rows" = the share of every 16th row that is not one flat colour.
