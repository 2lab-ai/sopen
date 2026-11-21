---
title: sopen — viewing a remote file as a stream
type: spec
tags: [sopen, spec, streaming]
---

# 06 — Viewing a remote file as a stream

Status: planned (2026-11-20). This is the contract the streaming work units (§13) build. Until each
lands, [`01-spec.md`](01-spec.md) describes what ships; §14 lists the 01 text each unit replaces.

Editing a streamed file is [`07-streaming-edit.md`](07-streaming-edit.md). The measurements behind
both documents are in [`research/streaming-view.md`](research/streaming-view.md) (cited as *view*
§n) and [`research/streaming-edit.md`](research/streaming-edit.md) (*edit* §n). The target is fixed
in [`streaming/ssot.md`](streaming/ssot.md). Code citations name functions. Where a line number is
given, it is at `main` 3a426b6, and the branches that land first will move it (§13).

**Reference rig.** Every time and rate quoted here comes from one link: a remote on a mesh VPN,
ICMP RTT 12–40 ms, 2–10 MB/s, OpenSSH 10.5p1 `sftp-server` reading from tmpfs (*view* §2.1). Byte
bounds are exact on any link. Time bounds are for that rig.

## Owner message 17 (verbatim, 2026-11-20)

> 그리고 파일뷰어 이미지 뷰어 영상 모두 지금 병신처럼 전체를 다 받아서 처리하는데 이거 모두 스트리밍 처리해줘. 예를들어 24기가 짜리 모델 파일을 시팔 24기가 다 받아서 보여주는데 말이 안되잖아. 모두 스트리밍 처리해줘 이건 충분히 리서치해서 해줘
>
> 예를들어 그냥 파일 뷰어면 보여줄수 있을만큼만 먼저 받고 먼저 화면 보여주고 그 다음에 시퀀셜로 볼경우에 몇개의 뷰를 미리 로딩해서 앞뒤로 먼저 로딩해서 보여주고 (전체 다운로드 X) 멀리 이동하면 이거는 시간이 좀 걸리는데 그렇게 처리해줘. 예를들어 유투브에서 동영상 스트리밍한다고 생각하고 리서치해서 파일도 바이너리 파일도 음악, 영상 모두 그렇게 처리해줘.
>
> 그리고 그냥 뷰어가 아니라 만약 편집할 경우는 다른데 이것은 별도로 리서치해서 처리해줘. 편집할경우는 그 편집된 부분은 전부 현재 로컬에 해당 내역이 모두 메모리 혹은 파일로 임시 저장해야하는거고 원하면 save as 로 로컬에 저장할 수 있게 해줘. (디폴트는 원격 편집 저장임)

The first two paragraphs are this document. The third is 07.

## 1. The contract

- **Nothing is downloaded before it shows.** A remote file opened in any built-in viewer — the
  hex window (and the binary dialog's "Open in hex editor", and `--hex`), the browser's text viewer,
  the image window, the media window — is read through a stream (§5). The viewer fetches what
  its first screen needs and shows it.
- **Then a buffer, like a video player's.** Around what is on screen the viewer keeps one screen
  before and one after (the margin, §4). Sequential movement grows a readahead window up to a bound
  (§4). Blocks already read stay in a RAM cache behind it (§3). The whole file is never fetched for
  viewing.
- **A far jump takes a moment, and says so.** The block at the new place is fetched on demand,
  ahead of any prefetch. While it is missing the viewer shows that place as loading (§7), never as
  empty or as an error.
- **Whole-file actions stream with progress and a cancel** (§8): decoding an image, Save to…, the
  waveform, an index pass the owner asked for, and the edit actions of 07. The cancel is ssot's
  fixed scope, the workstream's reading of message 17, not its words ([`streaming/ssot.md`](streaming/ssot.md)).
- **Viewing writes nothing to disk.** No cache copy is made for viewing a remote file (§10).
- **Local files keep reading in place** as today (01 §3a). No stream is created for them (§12 V17).
- **Text sent to VS Code or Zed is unchanged.** Message 17 is about sopen's own viewers; the
  editors read remote files their own way (01 A2).

## 2. The byte transport: `remote::Ranged`

- **The connection becomes the transport.** `RemoteFs::into_ranged(self, path)` takes the connection
  the open already made (the one that resolved the path, stat'ed it and sniffed its kind), opens the
  file on it once, `fstat`s the handle and returns a `Ranged`. It runs on a thread of its own with a
  current-thread runtime. `RemoteFs` gets it as a provided method whose default says "not
  supported", so the wrappers in `src/testutil.rs` and `main.rs`'s tests need no change.
  - **A failure gives the connection back.** The error carries the unchanged connection, so the
    caller is as it was: the binary dialog keeps its connection for a retry or "Save to…" (§7.5).
  - **And `Ranged` can give it back.** `into_connection()` closes the handle and returns the
    connection without waiting on the server. What is queued and not yet sent is dropped. The
    handle's `CLOSE` is written before `into_connection` returns, unless the local pipe stays
    blocked for 200 ms; then it goes with the session's next use. "Written" is observed, not
    assumed: the session's request pipe counts the packets it takes, and the wait ends once a
    whole `CLOSE` has gone in and a flush has completed after it. Its reply is read by the
    session's next use, or at `Sftp::close`. `reopen(path)` closes the handle the same way and
    opens `path` again on the same connection. A cancelled preview uses them to keep the window's
    preview session (§13).
  - The viewer's `Origin` then holds a **placeholder** connection that connects on first use, which
    is 02 §4.4's rule for a previewed window. Save, Save copy, Export and a reconnect are what use it.
  - So viewing costs one SFTP session (§6).
- **Every read carries its own offset.** A read of `len` bytes at `off` goes out with that offset
  (`start_seek(Start(off))` on a `File` clone), never through a shared cursor. Each read is clipped
  to the fingerprint's size (§5).
- **A short reply** is followed by a read of the rest, at its own offset.
  - **The end comes early** when the follow-up gets `SSH_FX_EOF`, or data of zero bytes, before the
    clipped end. The file is then shorter than when it was opened. The block is not used, and the
    Source becomes `Changed` (§5).
  - **Verified** against OpenSSH 10.3p1's `sftp-server` (scratch, 2026-11-20):
    - a read straddling the end returns the rest;
    - a read at or past the end gets `SSH_FX_EOF`;
    - after a truncate in the middle of a block, the rest of that block gets `SSH_FX_EOF`.
  - **This also fixes a latent bug.** `openssh-sftp-client` 0.15.12's `File::read` advances its
    cursor by the length **asked**, not the length returned (`file/mod.rs:403-428`).
    `SftpRemote::read_with` relies on that cursor. So against a server that answers a read mid-file
    with fewer bytes than asked, it skips the rest of that request, and it delivers fewer bytes
    than the file has.
    - `fetch`'s size check then refuses: it retries once and fails with "kept changing while it
      was fetched".
    - "Save to…" refuses as "changed while it was downloaded".
    - `write_atomic`'s re-read hashes the wrong bytes and refuses the save as a content change.
    - The kind sniff reads a gap.

    It refuses rather than corrupts.
  - **It does not fire against OpenSSH.** OpenSSH's server answers every read within its limit in
    full: 200 of 200 mid-file reads of 20 480–261 120 bytes did (verified, scratch). The crate never
    asks for more than that limit. So it fires against other servers only.
- **Server read limit.** OpenSSH 10.3p1 offers `limits@openssh.com` with a read limit of 261 120
  bytes. A read asking 1 MiB mid-file returned 261 120 bytes (verified, scratch; *view* §2.2 found
  the same on 10.5p1). The product cannot read the limit: `Sftp::max_read_len` sits behind the
  crate's `__ci-tests` feature, and without the extension the crate assumes 20 480 bytes. So nothing
  assumes 261 120. A block is asked for whole, and short replies are followed.
- **Interface:**
  - `size()`;
  - `fstat()` (the handle) and `stat_path()` (the name);
  - `read(off, len)` returns a ticket, and its completion carries `Result<Bytes>` and the time it
    took;
  - `reopen(path)`, `into_connection()` and `close()`.
- **Requests leave in the order they were queued.** `openssh-sftp-client` writes a request when its
  future is first polled, so a request built in one task can overtake reads spawned in others
  [inferred from the crate's code]. The transport therefore sends every request from one task, in
  queue order.
  - A validation `fstat` (§5) is queued after the reads it validates, and is written after them.
  - Test (V5): on the in-order `Fake`, every validation `fstat` arrives after each read it
    validates.
- **Close.** `close()` aborts every task and drops every `File` clone. Then `Sftp::close()` runs,
  bounded to 5 s, on the transport's thread. A live clone makes `Sftp::close()` hang (measured: not
  returned after 10 s, *view* §2.2). No window ever waits for it.
- **Implementations:**
  - SFTP;
  - local, over a `File` (tests only; a local file never gets a stream);
  - a `Fake` for tests. It answers **in order**, as `sftp-server` does. Latency and bandwidth are set
    per test. It can clip each reply to a given length, inject errors, change the file in place, or
    replace it. It counts every request and the bytes sent ahead of each one.
  - **A clipping proxy for tests.** The `Fake` cannot sit under today's `SftpRemote::read_with`, and
    OpenSSH never clips. So V5 runs a real SFTP session — `Sftp::new_with_auxiliary` over pipes, as
    `local_server()` does — through a proxy to the local `sftp-server` that cuts every
    `SSH_FXP_DATA` payload to 20 000 bytes. It fails on 3a426b6's `read_with` and passes on the new
    reads. W1 owns `Cargo.toml` for any tokio dev-features the proxy needs.
- **Pipelined whole reads.** `SftpRemote::read_to` is rewritten on the same rule: explicit offsets,
  in-order delivery to `out`, and up to **4 MiB in flight**. The window starts at one request and
  grows by one per reply, as OpenSSH's `sftp` does (*view* §3). Cancel is checked between chunks, as
  `Stoppable` does today. This serves every whole-file user of the `Origin` connection: `fetch`
  (and so `--app`, the terminal editor and the no-display fallback), "Save to…" (`download_to`), and
  `write_atomic`'s re-read of the target up to 256 MiB. With one request in flight today's rate is
  2.2–2.5 MB/s on the reference rig, and 256 KiB–4 MiB in flight reached the link's ceiling, 1.5–4×
  that (*view* §2.2, *edit* §2.2).

## 3. The block cache: `stream::BlockCache`

- **Blocks of 64 KiB**, one request each. At equal bytes in flight the request size does not change
  throughput (*view* §2.2). A random 64 KiB read costs p50 38 ms on an idle channel, and one block
  holds a hex screen 136 times over.
- **64 MiB of RAM per stream** (1024 blocks). The least recently used block goes first.
- **Protected from eviction:** the blocks inside any live reader's `[cursor − behind, cursor + ahead]`
  (§4). The 64 MiB is a hard bound, and a window's protection yields to it.
  - When the readers' protected windows together would pass 48 MiB, every reader's `ahead` and
    `behind` are scaled down to an equal share of that 48 MiB, but never below a floor of 256 KiB
    ahead and 64 KiB behind.
  - **A block a reader is waiting for is held by that reader, not by the cache.** A reader's
    wait is one span of at most 16 MiB (one read). The reader keeps its own reference to each
    block of the span that is there: those the cache has when the wait begins, and each one that
    arrives and is validated after. A read is served from what the reader holds first, then from
    the cache.
  - So a wait never depends on what the cache keeps, and the cache evicts freely: by window,
    then by age. Nothing limits which waits are served; priority is §4's class order alone.
  - The references are dropped when the read completes, when the reader moves, is renewed or is
    dropped, and when the Source fails, changes or reloads. Nothing held is served after a
    change or a reload.
  - **Memory:** the cache's 64 MiB, plus at most 16 MiB for each reader with a wait outstanding.
    A held block that is also in the cache is one buffer, counted once.
- **Bulk readers insert at the cold end.** Their blocks are the next to go, so a pass over the whole
  file (an index pass, the waveform, Save As) does not evict what the screen or playback needs.
- **No disk spill in v1.** The measured patterns fit in 64 MiB: open + 10 s of play + a far seek
  fetched 13–23 MB (*view* §2.3). A refetch after eviction costs 0.2–0.6 s, which the owner accepts
  for a far jump ("멀리 이동하면 이거는 시간이 좀 걸리는데"). A spill file would bring a sparse 0600
  file, a bitmap and cleanup rules under 01 §5. Revisit only if the usage log's `bytes_fetched` (§11)
  shows sessions fetching the same bytes twice.

## 4. Prefetch: `stream::Prefetcher`

**Readers.** A reader is one consumer with a position: a viewer's screen, one HTTP connection of
ffmpeg's, or a worker (an index pass, the waveform, Save As). Each reader has its own readahead
state, on Linux's on-demand rule (*view* §3):

- The window is **0** when the reader opens and after a jump. Blind readahead at open measurably
  delayed ffprobe's jump to a `moov` at the end (*view* §2.3).
- A **sequential hit** (the next access starts at or inside the last window) sets the window to
  `max(1 block, 2 × window)`, up to the reader's `ahead`.
- A **jump** (anything else) resets the window to 0. Only what was asked is fetched.

| Reader | `ahead` max | `behind` kept |
|---|---|---|
| Interactive (hex, text) | 1 MiB | 256 KiB |
| Media (one HTTP connection) | 10 s at the probed bit rate, clamped to 2–32 MiB (Chromium's rule, *view* §3) | 2 s at that rate, at least 1 MiB |
| Bulk (index pass, waveform, image read, Save As) | 1 MiB | none (cold insertion) |

These are maximums. Readers sharing one cache get less when their windows would not fit in it
(§3).

**The margin.** An Interactive reader asks for the screen it shows and, right after, for one screen
before and one after it. The margin is foreground prefetch (below). That is the owner's
"앞뒤로 먼저 로딩" at a jump, and it makes PgUp and PgDn from any place immediate. At hex and text
sizes the margin almost always lies inside the screen's own block. When it crosses into another,
that block is the only extra cost.

**Why there is a cap.** Requests already sent cannot be recalled: SFTP has no cancel, and
`sftp-server` answers in order. Both were verified with a scratch client on OpenSSH 10.3p1: eight
reads and then an `fstat` were answered in request order. *View* §2.2 measured the cost: a read
queued behind 16 MiB waited about 2 s. So a request waits behind every byte already sent on its
channel, and the Source bounds those bytes.

**Admission: one cap for everything sent.**

- **The cap:** `cap = clamp(2 × rate × rtt_min, 256 KiB, 4 MiB)`.
  - `rate` is the delivered byte rate, an EWMA over the last second of completions. Until there is
    one, the cap is 256 KiB.
  - `rtt_min` is the fastest small reply seen: a status reply, or a one-block read. It starts at
    the first reply, not at the open's `fstat`, which `Ranged` does not time. The cap is 256 KiB
    until then.
- **Reads are admitted against the cap.** A read goes out only if `in_flight + its size ≤ cap`. The
  one exception is a read on an idle channel (`in_flight` = 0), which always goes, so that a read
  larger than the cap still makes progress.
- **Status requests are not reads.** A validation fence and a poll each count about 100 bytes of
  reply against `in_flight`, but the cap never holds them back. At most one fence per batch and one
  poll pair are ever outstanding, so together they add well under a block.
- **A read batch travels with its fence.** A batch (the reads admitted together for one reader,
  §5) is sent as one unit: its reads, then at once its validation `fstat`.
  - The fence has its batch's class and is never queued apart from it. No queued prefetch or bulk
    can get between a demand read and its fence.
  - A **remainder read** after a short reply (§2) is a batch of its own, with its own fence sent
    right behind it.
- **The poll** (§5) is queued every 2 s. It goes out ahead of queued prefetch and bulk and behind
  queued demand, because it guards what the screen shows.
- **The queue.** Reads not yet sent wait in one queue, by class, oldest first within a class:
  1. **foreground demand**: a block an Interactive or Media reader is waiting for, or that a frame
     shows as loading;
  2. **foreground prefetch**: those readers' margin and readahead;
  3. **bulk**: what a Bulk reader asks for, its readahead included. A Bulk reader's waiting block is
     not demand. Within the class a waiting block goes before readahead, whatever their ages: a
     pass is stalled on the one. Bulk goes out only while nothing of classes 1–2 is queued. The
     priority is strict, so bulk progresses whenever the foreground windows are full or idle, and
     not while a foreground reader keeps moving. A foreground reader's readahead stops at its
     `ahead`.
- **A newer demand from the same reader replaces its older one** that has not been sent yet. So
  repeated jumps queue only the latest place.
- **The bound.** A foreground demand waits behind at most `cap` bytes already sent, plus the older
  queued demand of other readers (one block each). So it waits `(cap + one block per other waiting
  reader) / rate`, then its own transfer.
  - With one viewer, that is `cap / rate`: `2 × rtt_min` while the clamp does not bind, about 0.5 s
    at the 256 KiB floor on a 0.5 MB/s link, and `4 MiB / rate` at the ceiling.
  - On the reference rig (4–10 MB/s, `rtt_min` 18–20 ms) the cap is 256–400 KiB. About 256 KiB in
    flight already reaches that link's ceiling, and at 510 KiB in flight a demand read waited p50
    51–89 ms (*view* §2.2).
- **When the cap falls** while requests are out (the rate dropped), nothing is recalled. Nothing more
  is sent until `in_flight + block` fits the new cap.
- **One streaming channel per viewer.** The cap is the far-seek bound; no second channel is opened
  for prefetch (§6).

**Cancel on jump.** A jump drops that reader's queued prefetch at once. What was already sent, at most
the cap, still arrives and is cached; it is useful if the reader comes back.

## 5. `stream::Source`: fingerprint, change, errors

A **Source** is one remote file opened for viewing: one `Ranged`, its cache and its prefetcher.
Cloning one is cheap; the last drop closes it.

**Fingerprint.** At open sopen runs `stat(path)`, opens the handle and `fstat`s it. The handle's
`(size, mtime)` is the fingerprint: 01 §5's `Fingerprint`, without a hash.

**What the handle shows.** Measured in *view* §2.2, and verified again with a scratch client on
OpenSSH 10.3p1 over APFS:

- It keeps reading the file it opened when another file is renamed over the path, as an editor's
  save does. `fstat(handle)` and `stat(path)` then disagree (22 bytes against 3).
- It shows writes made in place. An in-place write a second later moved `fstat(handle)`'s mtime,
  and a truncate changed its size.
- A same-size write in the same second as the last one left `(size, mtime)` unchanged, since SFTP v3
  carries whole seconds.

**Every read is validated after it arrives.** The transport follows every batch of reads with an
`fstat(handle)` on the same channel: the batch's **fence**, sent with it (§4). A block becomes
visible, to the cache and the screen, only when every part of it has been followed by a fence that
returned the fingerprint. The server answers in order (verified, §4), so a fence describes the file
after the reads before it.

- A block read across a write made in place is therefore never shown next to blocks of the old
  version: the write moves the mtime, the `fstat` after the block sees it, and the block is
  discarded.
- **The one exception is SFTP's own limit:** a same-size write in the same whole second as the
  file's last change (01 §5's limit). Viewing is best effort there, and blocks of two versions can
  mix until a change is seen.
- **A block read in parts** (a short reply, then its remainder, §2) becomes visible only after the
  remainder's own fence, and only if every part's fence returned the fingerprint. A part whose
  fence failed gives the whole fetch up, its remainder included, even one already sent. A write between the first reply and the remainder moves the mtime that
  fence sees, so the whole block is discarded and the Source turns `Changed`.
- **The cost** is one `fstat` request (about 50 bytes of reply) per batch. On the fast path, where
  every reply is full, it is sent with the reads and adds no round trip: the fence's reply follows
  the block's data. A short reply costs the remainder's round trip, and its fence rides with it.

**Polling** covers what validation cannot see: a file renamed over the path (`Replaced`), and a
change while nothing is being read. sopen sends `fstat(handle)` and `stat(path)` every 2 s while the
viewer is on screen, scheduled as §4 says: ahead of queued prefetch and bulk, behind queued demand,
never held back by the cap. A viewer that is hidden, such as a browser viewer behind the list, does
not poll.

`Replaced` is found by comparing `stat(path)` with the fingerprint. A file of the same size and the
same whole-second mtime renamed over the path is therefore not seen: SFTP v3 carries no inode. It
is the limit below, for the poll.

**A detected change invalidates what is in flight.** Every request carries the Source's generation,
which a change or a reload increments. A completion of an older generation is dropped unread, so
nothing sent before the change can refill the cleared cache. The attempt id of a block's fetch
does the same within a generation: a reply whose fetch was given up (its fence failed) is
dropped before it is read, so its error, its emptiness or its bytes cannot touch the fetch that
replaced it.

| State | Meaning | What the viewer does |
|---|---|---|
| `Live` | unchanged | normal |
| `Replaced` | the path names another file; the handle's file is unchanged | keeps showing the version opened, consistently; status row: `the remote file was replaced; showing the version opened — Cmd/Ctrl+R reloads` |
| `Changed` | the handle's size or mtime moved | **drops the whole cache** (blocks from before and after must not mix), keeps the screen it has and reads no more; `the file changed on the remote — Cmd/Ctrl+R reloads` |
| `Failed(reason)` | a read got an SFTP status error, or the channel closed | the screen stays; the reason is on the status row; the next key, click or scroll retries, at most once per second |
| `Closed` | the viewer is gone | — |

- **Reload** (Cmd/Ctrl+R in every viewer) re-opens the path, takes a new fingerprint and drops the
  cache. Reloads run in the order asked. Two plain ones not yet started are one; a reload after a
  save (below) is never merged away. The text viewer indexes again, the media window probes again, and the hex window keeps its
  offset. The key is Cmd/Ctrl+R and not R alone, because in the hex window's ASCII column R types a
  byte.
- **Retry** reconnects through the viewer's existing `Reconnect` (`src/gui/mod.rs:34`), re-opens and
  `fstat`s. The same `(size, mtime)` gives `Live` and the cache is kept; anything else is `Changed`.
  SFTP v3 attributes carry no inode, so after a reconnect only `(size, mtime)` can be compared.
- **A close during a retry or a reload.** One not yet at the network opens nothing. One already
  there is an operation under way: the close does not wait for it, and when it returns, what it
  opened is closed at once, unused. So nothing is requested after the close, except the open of a
  reconnect already under way, which is closed on return unused.
- **sopen's own writes are not changes.** When one of sopen's own writes to the file lands (07's
  Save, or §7.1's interim Save), the window reloads its Source at once. The new fingerprint must
  equal the write's own baseline: the staged temp's stat (01 §5), or for 07's write in place the
  target's `lstat` right after it.
  - Equal: `Live` on the saved file. The cache is dropped, since its blocks are of the old file.
  - Different: `Changed`, with 01 §5's note `saved, but the remote changed right after`, and the
    next Save from that baseline is refused (01 §5).
- **Limit, as in 01 §5.** mtime has whole seconds over SFTP. A same-size rewrite within the same
  second cannot be seen without reading every byte, and is not (verified, above).

**Timeouts.** A demand block pending over 1 s shows `loading 0x… (N KiB)`, and over 10 s
`the remote is slow`. The transport fails a request that has no reply 60 s after it was sent (W1),
and that failure makes the Source `Failed`. This is the one bound: the Source has no stall timer of
its own. A failed prefetch is never shown; it is only retried as demand.

**Interface** (`src/stream/mod.rs`; every viewer builds on it, and 07 reads through it):

```rust
/// One remote file opened for viewing. Cheap to clone (Arc); the last drop closes it.
pub struct Source { /* … */ }
impl Source {
    pub fn open(transport: Box<dyn remote::Ranged>, reopen: Reopen, wake: Waker) -> Result<Source>;
    pub fn size(&self) -> u64;                   // the handle's size at open
    pub fn opened(&self) -> remote::Fingerprint; // (size, mtime) at open; sha256 None
    pub fn state(&self) -> State;                // Live | Replaced | Changed | Failed(String) | Closed
    pub fn reader(&self, kind: ReaderKind) -> Reader;
    pub fn fetched(&self) -> Fetched;            // bytes received, refetches counted: total, and by
                                                 // demand / prefetch / bulk
    pub fn reload(&self) -> Result<()>;          // re-open the path, new fingerprint, cache dropped
}
pub enum ReaderKind { Interactive, Media { bytes_per_s: u64 }, Bulk }
pub struct Reader { /* a position with its own readahead window (§4) */ }
impl Reader {
    /// Frames: the bytes when every block is cached, else Pending (fetch queued; `wake` on arrival).
    pub fn try_read(&mut self, off: u64, len: usize) -> Poll<Result<bytes::Bytes>>;
    /// Moves the reader to the version open now, at no position.
    pub fn renew(&mut self);
    /// Workers and HTTP connections: blocks. Err(Cancelled) once `cancel` is set,
    /// Err(Changed) when the source changed, Err(Reloaded) when it was reloaded,
    /// Err(reason) when it failed.
    pub fn read_at(&mut self, off: u64, buf: &mut [u8], cancel: &AtomicBool) -> Result<usize>;
}
/// What a viewer reads: implemented by `std::fs::File` (local, in place; always Ready) and `Reader`.
pub trait Bytes { /* len, try_read, read_at — as above */ }
```

**A reader reads one version.** After any reload, every reader made before it gets
`ReadError::Reloaded` for every read, cached or not, until `renew()`. So a worker never continues
into the new file. A viewer renews its readers after a reload. The byte server treats `Reloaded`
as it treats `Changed`: it cuts the response (§9).

`Waker` is egui's `request_repaint` for the viewer's context. `Reopen` opens a new connection on the
viewer's `Reconnect` and makes it a `Ranged`.

## 6. Channels per host

Every SFTP channel sopen opens to a host is a session on that host's one ssh master (01 §4), and
sshd allows `MaxSessions` per connection, 10 by default (`sshd_config(5)`, *view* §3). Every sopen
window on that host shares that one budget.

- **A remote viewer that only views holds one channel**: its `Ranged`, which carries every read
  (§2). Prefetch never opens another.
- **Its `Origin` connects on first use** (02 §4.4's placeholder): Save, Save copy, Export, or a
  reconnect. From then on the viewer holds two channels. A Save never opens a third (07 §11).
  - The two stay apart because a long `copy-data` request holds its channel; a `stat` behind one
    waited 3.1 s (*edit* §2.4). The viewer's reads, on its `Ranged`, are not held up by it.
- **The media window holds one Source.** All of ffmpeg's HTTP connections read through its single
  `Ranged`.

| Holder | Channels while it lives |
|---|---|
| a standalone viewer (hex, image, media) | 1; 2 from its first Save, Save copy or Export |
| the browser's listing | 1 |
| a browser viewer | 1; 2 from its first Save, Save copy or Export |
| the browser's preview session (02 §4.4, one per window) | 1 |
| the directory tree (browser 05) | 1 |
| a file operation (browser 04) | 1 each |
| git commands (browser 03) | up to 3 exec sessions |

These are 02 §4.4's counts: listing 1, viewer 1, preview 1, tree 1, file operation 1, git ≤ 3, which
is 8, and 9 while a git stop runs. Streaming adds one only while a viewer has saved or exported.
One more sopen window on the same host can therefore reach the limit.

**When sshd refuses a session**, the refusal can come at two moments. Each is said in words, and
nothing else is affected:

> the remote refused another SSH session (sshd allows MaxSessions per connection, 10 by default,
> shared by every sopen window on this host) — close a window there and try again

1. **At open**, when the open's own connection is made. Nothing is open yet. The browser's status
   row shows the line. A route that was not opening a viewer stays as it was; for the binary
   dialog, that means ready for a retry or "Save to…".
2. **At the `Origin`'s first use**: a Save, a Save copy or an Export, possibly with unsaved edits.
   The action fails with the line, and the edits stay in the window and in the journal (07 §4.2).
   Nothing on the remote was touched, because no request was sent. The next Save tries to connect
   again.

**Not verified yet.** `stream::session_refused` matches ssh's words for a refused session, but
`start_sftp` sends ssh's stderr to null (`src/remote/sftp.rs:81`), so those words never reach the
error today. W8 and ED4 must capture that stderr, a change to W1's file, before V10 can pass.

## 7. Per viewer

### 7.1 Hex (the binary dialog, the browser's Enter, `--hex`)

The owner's 24 GB model file lands here: `.safetensors`, `.gguf` and `.bin` are kind "other" (01 §3).

- **No fetch.** "Open in hex editor", Enter in the browser and `--hex` open a Source and the window
  at once. The kind sniff (01 §3: the first 8 KiB) is unchanged. It runs on the open's connection
  before that connection becomes the `Ranged` (§2), and it is counted apart (`sniff`, §11).
- **Reads.** `Pages` keeps its 4 KiB pages and its 64-page LRU, now read through `Bytes`.
  - A page that has not arrived is **Pending**, not empty. Its rows show `··` in the skin's dim
    colour, the status row says `loading 0x…`, and the frame repaints when it arrives.
  - A page that failed is the status row's `Failed` text and is retried. Today a failed read is held
    as an empty page (`Pages::byte`) and shows as nothing.
- **Prefetch.** One Interactive reader at the screen, with its margin (§4). Scrolling down grows the
  readahead to 1 MiB.
- **Jumps.** Cmd/Ctrl+G, Cmd/Ctrl+Home and Cmd/Ctrl+End are jumps. So is a scroll-bar drag, which
  fetches only where the thumb rests: no demand is sent for a position the thumb passes, only after
  it has rested 100 ms or is released. Demand queued for a position passed, and not yet sent, is
  dropped.
- **Edits keep the byte they replaced.** Each patch holds the byte the file had there, read when
  the edit was typed. Its page is on screen, so it is present.
  - So `changed`, `differ`, `changed_count`, `dirty` and `unsaved` compare a patch with its own
    replaced byte and never read the file.
  - This fixes a bug in today's code. Those functions read the file's byte for every patch, through
    a synchronous `read_exact_at` on the frame thread, and count a byte that cannot be read as
    changed. Over a stream that would block a frame or misreport.
  - A byte whose page is Pending cannot be typed over. The keystroke does nothing, and the status
    row says `0x… is still loading`.
  - A `SOPEN_STATE` patch waits for its bytes.
- **Save until 07 lands (interim).** Save and Save copy of a streamed file first fetch the whole
  file into the cache exactly as today's fetch does: claimed, recorded, pipelined (§2), with
  progress on the status row and Esc to stop. Today's Save then runs on that copy.
  - The fetch must equal `Source::opened()`. A different `(size, mtime)` is refused as a change.
  - Its SHA-256 is 01 §5's baseline, as today.
  - 07 replaces this with its Save, which fetches nothing. It is the only whole fetch the hex window
    ever makes.

### 7.2 Text (the browser's text viewer)

Today the text viewer keys everything by line number, and every read blocks:
- `TextDoc`'s marks and line count;
- `read_lines` and the highlight worker's own `File`;
- `TextWindow`'s top line;
- wrap's `Pos { line, off }`, its `Row` and its memos (PR #14, merged: `src/gui/text/wrap.rs`).

One pass indexes the whole file at open. So the work is split in two units (§13): W5a makes reads
streamable for files of any size it already handles, and W5b removes the line-number keying for
large files.

**W5a — reads through `Bytes`, first screen first.**

- **Reads go through `Bytes`** (§5). A read that is not cached returns `Pending`, which travels up
  the way today's `Stale` does: the page is not replaced, the frame asks again when the block
  arrives, and nothing blocks a frame. The highlight worker reads through a `Reader` of its own on
  the same Source, never a second `File`.
- **The first screen from byte 0 comes first.** The view shows lines from the first block while the
  index grows, and the background index pass starts only once that screen is up.
- **The background pass**, a Bulk reader, indexes at most the first **16 MiB** (`HIGHLIGHT_MAX`). On
  the rig that is 2–8 s.
  - It continues S1-B's preview pass, which stops at 4 MiB and resumes from there on hand-over (02
    §4.4).
  - A file of at most 16 MiB is then indexed whole. Line numbers, the line count and highlighting
    work as today.
- **Highlighting runs over the indexed prefix as it grows.** A page whose lines all lie in the
  prefix is highlighted, with state carried from byte 0; a page past it shows plain until the prefix
  reaches it.
  - This is what 02 §4.2's preview already does under its 4 MiB cap, so a 4–16 MiB remote text file
    previews highlighted too.
  - Files over 16 MiB stay plain past the first 16 MiB, as today's rule says for the whole file.
- **Change.** A remote file follows §5. `Changed` freezes the page and offers Cmd/Ctrl+R. `Replaced`
  keeps showing the version opened. A remote file is still not followed like `tail -f`. Local files
  keep `TextDoc::now`'s rules (01 §3a).
- W5a alone never reaches users for remote text: W8, which routes remote text through a Source,
  waits for W5b (§13). Until W8, remote text is still fetched whole, as today.

**W5b — large files by byte position.**

- **A prefix index.** The marks are today's: one every 256 lines, and one at the first line start
  that is at least 64 KiB past the last mark. They are kept in a prefix index that only grows from
  byte 0.
  - The Bulk pass gives the first 16 MiB.
  - Beyond that, the index grows from blocks the screen and its readahead bring in, while they are
    contiguous with the prefix.
- **Position.** The place on screen is a byte offset of a line start, plus its line number when the
  prefix reaches it. `TextWindow`'s top, wrap's `Pos` and `Row` and its memos are keyed by byte
  offset.
  - The gutter shows line numbers inside the prefix. Past it, the gutter shows the line's byte
    offset (`0x4B0000000`) in the skin's dim colour.
  - The status row says `lines known to 3.1 MB of 24 GB`.
- **The scroll bar** is by bytes while the line count is unknown, and by lines once the whole file
  is indexed.
- **A jump** (the scroll bar, End, Go to) reads the block at the target and looks back for the line
  that holds it.
  - It reads at most **64 KiB back** (`SPAN`). That bounds the read, not the line: SPAN only says
    where marks fall, and a line can be any length. A line whose start is not found within those
    64 KiB has an **unknown start**.
  - **Wrapped** (the default since PR #14): the rows of a line with an unknown start begin on wrap's
    restart grid, at the absolute 16 KiB offset at or before the target. The status row says
    `line start not known`.
  - **Unwrapped:** that line shows as one row from the same 16 KiB grid point, cut at 4 096 bytes
    as any long line is. The gutter shows its byte offset with `…` before it.
    - PgUp and ↑ move back one grid point (16 KiB) at a time, until a newline is found and the line
      start becomes known.
    - PgDn and ↓ move to the next line start after the target.
  - With a known start, PgUp reads the block before and splits it at newlines, and a partial first
    line is joined when its own block arrives.
- **Go to: Cmd/Ctrl+G** opens a field in the status row, as in the hex window. It takes a line
  number, or `0x…` for a byte offset.
  - A line inside the prefix goes there at once.
  - A line past the prefix runs the Bulk index pass from the last mark to that line, with progress
    (`indexing… 1.2 GB of 24 GB`). Esc stops it: nothing more is sent, and only what was already in
    flight (≤ the cap) arrives.
  - It is the only thing that reads a large file through, and only when asked.
- **Index memory is bounded.** Line marks come at most one per 256 lines. 64 KiB marks are at least
  64 KiB apart, so they cost at most 16 bytes per 64 KiB of the file. When the index would pass
  1 Mi marks (16 MiB), it drops every other line mark and doubles the line spacing; a 64 KiB mark is
  never dropped. So it stays within 16 MiB up to 64 GiB of indexed text, and 16 bytes per 64 KiB
  beyond.
  - Applying this to local files too is **the workstream's decision**, beyond message 17's words.
    Today a local file of empty lines gets a mark every 256 bytes: 1.5 GB of index for 24 GB.

### 7.3 Image

- **The header first, by walking, with a 1 MiB cap.** The header is found by reading, not from a
  fixed prefix:
  - **JPEG:** walk the marker segments (each at most 64 KiB), keeping the Exif APP1 for its
    thumbnail. The width, height and component count are read **straight from the SOF segment** by
    sopen's own walk, with no decoder.
    - image 0.25.10's `JpegDecoder::new` calls `read_to_end` and then `decode_headers`.
    - zune-jpeg reads markers up to SOS and fails at the end of its input. So bytes that stop at
      the SOF cannot be handed to it, and the stream never can (it would pull the whole file).
    - The walk reads 64 KiB first and then 64 KiB at a time, up to the 1 MiB cap. Measured (X4,
      2026-11-21): the SOF ends within 42,662 bytes in all 894 JPEGs shipped with macOS and its
      apps on one Mac (median 2,942), and within 19,170 in the five camera files among them, so
      the first read is normally the only one. A built phone-style file (Exif with a 60 KB
      thumbnail, XMP, extended XMP, ICC, MPF) needs 303,964 bytes; a file with more than about
      1 MB of metadata ahead of its frame passes the cap.
    - A plain search for the first SOF code is wrong: it gives a wrong size on all five camera
      files (the Exif thumbnail's on four, for example 160×106, and 0×3361 on the fifth). The walk
      skips whole segments by their lengths.
    - The rules of the walk:
      - The file must start `FF D8`. Otherwise the format is guessed from content, as today.
      - A marker is `FF`, any further `FF` bytes (fill), then the code. A segment is skipped by its
        2-byte length and nothing inside it is read. That is what keeps the Exif thumbnail's own
        SOF and a second image after the first EOI (MPF) out of the answer.
      - The SOF is the first code in C0–CF other than C4, C8 and CC. Its whole segment is read
        (at most 775 bytes) and accepted only if its length is 8 + 3 × components, the width and
        height are not 0, and it has 1 to 4 components.
      - The walk gives up, with no header, on anything else before the SOF: a byte that is not
        `FF` where a marker must be, `FF 00`, a marker with no length (01, D0–D7), a second SOI,
        EOI, SOS, a length below 2, or a height of 0 (a height set later by DNL is not supported).
        It never searches forward for a marker.
      - No header is not an error. The window shows the size line and streams the file, and the
        decoder decides. The same holds when the file ends before the SOF does.
    - The Exif kept for the thumbnail is the first APP1 that starts `Exif\0\0`. Its orientation
      (5–8 swap the sides) gives the upright `W×H` to show.
    - The memory a decode needs is width × height × 1 for one component and × 3 for three or
      four (the decoder turns CMYK into RGB). Two components were not seen in X4; they count as
      × 3, the upper bound.
    - A header found does not promise a decode: a file cut after its SOF, or one with a DAC
      segment, still fails in the decoder.
    - A SOF that zune-jpeg cannot decode (any type but C0, C1, C2, or a precision other than 8:
      arithmetic, 12-bit, lossless) is reported at once with its type and size, and the file is
      not streamed for a decode. Hex view stays offered once W8 brings it.
  - **PNG, GIF, BMP, WebP:** the header is at the start (PNG's IHDR at byte 16; WebP's
    VP8/VP8L/VP8X chunk first). `ImageReader::into_dimensions` runs on the bytes up to it.
  - **TIFF:** follow the first-IFD offset, one more read wherever it points.
  - Past 1 MiB without a header, the window shows the size line and streams the file as below.
  - With a header, the window shows the Exif thumbnail scaled up (else the dimensions alone) with
    `W×H, N MB, loading N %`.
- **The memory a decode needs, before any pixel data.** The header gives width × height × the bytes
  per pixel of the colour type the decoder produces. Up to **512 MiB** the window goes on.
  - Above that it shows the memory needed and the machine's physical memory:

    > decoding this image needs 37.3 GB of memory (100000×100000); this machine has 16 GB —
    > Continue (Enter) · Hex view (H) · Close (Esc)

  - **Continue proceeds.** sopen does not refuse an image for its size. Until the owner continues,
    nothing past the header is fetched. A need above the physical memory is said in the error
    colour, and Continue is still offered.
  - This is one of **the workstream's decisions told to the owner** ([`streaming/ssot.md`](streaming/ssot.md)).
    It applies to local images too, which goes beyond streaming.
  - **The buffer is reserved before decoding:** `Vec::try_reserve_exact` for the decoded size, then
    `ImageDecoder::read_image` into it, in place of `DynamicImage::from_decoder`, which allocates
    with no check.
    - An allocation the system refuses is then an error line, not an abort.
    - On systems that overcommit memory (Linux by default, macOS) a refusal is rare: the
      allocation succeeds, and the cost shows later as memory pressure. The memory line is the real
      guard.
  - Today no limit applies at all: sopen's path skips the image crate's 512 MiB reservation, and a
    14000² RGB image decodes to 588 MB (*edit* §2.8).
- **Hex view, and the hex view for a file that does not decode**, are new. Today a decode error is a
  status-row error in the browser (`prepare`, then `browser.rs`). The standalone route opens the hex
  window only for `--hex`, the binary dialog and a `.ts` file (`main.rs`).
  - Switching an open image to the hex window needs `browser.rs` and `main.rs`, so **W8** brings
    Hex view and the fallback.
  - Until then, Hex view is not offered, and a decode failure keeps today's error row.
- **Then the whole file streams** into memory through a Bulk reader, with progress and Esc. Esc stops
  the reader and keeps nothing. The read hashes as it goes, so 01 §5's SHA-256 baseline comes out
  of it, as today's fetch gives it. No cache copy is made.
- **A partial picture while it streams**, for the formats whose partial decode was measured
  (*view* §2.4, image 0.25.10, png 0.18.1, zune-jpeg 0.5.15). Each preview is a decode of the prefix
  so far, at about 25, 50 and 75 % of the bytes. It runs on a worker, one at a time, and a mark
  reached while one runs is skipped.

  | Format | Measured on a cut file | What the window shows while it streams |
  |---|---|---|
  | progressive JPEG | every row filled at 10 % (a whole coarse picture); 57 ms at 24 Mpx | the whole frame, coarse, getting finer |
  | baseline JPEG | top 10 % of rows at 10 %, 52 % at 50 % | the rows so far, top down |
  | plain PNG | `png` row reader: 203 of 2000 rows at 10 %, 1015 at 50 % (`image::decode` fails) | the rows so far, top down, through the row reader |
  | interlaced PNG | `png` row reader through Adam7 pass 4 of 7 at 10 % (a whole picture at ¼ resolution) | each completed pass, whole frame |
  | WebP, TIFF, GIF, BMP | not measured | the thumbnail or the size line until complete. A measurement may add them later; nothing is promised for them here. |

  - A preview decode needs the whole buffer, so it runs only within the budget or after Continue.
  - The plain-PNG row reader is the `png` crate's own API. `png` is only a transitive dependency
    today, so W6 adds it to `Cargo.toml` at the version the lock already has (0.18.1).
- **Once complete**, the decoded image, its edits and Save are today's: the full bytes are in memory
  (07 §8).

### 7.4 Audio and video

Built on PR #15 (`feat/media-edit`), whose `ffmpeg::Input` is a path or a URL and is handed unchanged
to every run on the file: the probe, playback, the paused frame, the waveform, Save and Export.

- **A remote file's `Input` is a URL on the window's byte server.** The URL is
  `http://127.0.0.1:<port>/<token>/<kind>/<name>` (§9), where `kind` says which reader serves it:
  - `play`: a Media reader, for playback, the paused frame and the probe;
  - `bulk`: a Bulk reader, for the waveform;
  - `render`: a Bulk reader, for Export, a 07 render and Save As.

  The file name stays last, so ffmpeg still sees its extension. A local file's `Input` is its path,
  as today.
- **The `.ts` refine** (`kind::refine_local`) probes the `play` URL.
- **`Input` carries its own input options.** Each `Input` holds the options that must precede its
  `-i`, and emits them there. So every remote input, each group input of PR #15's `edit_args`
  included, gets `-protocol_whitelist http,tcp`. mp3 `play` inputs with a LAME `Info` tag (below) also get
  `-fflags +fastseek`.
  - Without it an mp3 seek reads the file up to the target: the share of the file before it (6–8 %
    at 30 s, 81–84 % at 480 s of a 600 s file; 82–91 % in *view* §2.3). With it, a target past
    about the first 400 kB costs ffmpeg 65–115 kB (190–246 kB sent by the server, the rest being
    read-ahead), wherever it is; a target nearer the start is read through (221–393 kB).
  - Save and Export keep exact seeking: 01 §7 promises exact cuts.
  - **Measured precision** (X5, 2026-11-21, ffmpeg 9.0.2, 600 s test files, 60 seeks each; without
    the flag every seek landed within 2 ms):
    - CBR: within one mp3 frame (0.026 s).
    - VBR with a Xing header: from 4.3 s early to 1.1 s late (median 0.9 s). The error grows with
      the file's length: in a 1-hour file, 21.8 s early to 9.0 s late (median 3.9 s). ffmpeg seeks
      on a grid of 1/256 of the file's size.
    - VBR with no header: up to 161 s off. ffmpeg also misjudges the length (437.8 s for a 600 s
      file, with or without the flag), and a seek past that length reads through anyway (83–102 %
      of the file).
    - The flag saves bytes only on a server with ranges, which the byte server is (§9).
  - **So `+fastseek` is used only where its error is one mp3 frame (26 ms at 44.1 kHz), the
    tolerance accepted: an mp3 whose first frame carries a LAME `Info` tag (constant bitrate).**
    Every other mp3 seeks exactly. **This is the workstream's decision:** the media window places
    cuts at the play position, so a play position seconds off would put the user's cuts in the
    wrong place. The cost is that a far seek in a long VBR mp3 first reads the file up to the
    target; blocks already fetched are served from the cache while they stay in it (§3), and the
    window shows that read's progress.
  - **Finding the tag.** sopen reads it itself, through the `play` reader:
    - **ID3v2 tags:** while the bytes at the current offset start `ID3` with major version 3 or 4,
      skip that tag: 10 bytes of header plus the size in header bytes 6–9, a synchsafe integer
      (each byte below 0x80, 7 bits each), plus 10 more when a version-4 tag sets its footer flag
      (bit 4 of byte 5). Any other version or a size byte of 0x80 or more is an invalid tag, and
      the seek is exact. A large tag (cover art) costs one more ranged read.
    - **The first frame** must start right after the tags (ffmpeg resyncs over junk; sopen does
      not, and junk means an exact seek): an 11-bit sync, layer III, a valid bitrate and
      sample-rate index. The tag is read where ffmpeg's mp3 demuxer reads it, 4 + 32 bytes in
      (MPEG-1 stereo), 4 + 17 (MPEG-1 mono, MPEG-2 and 2.5 stereo) or 4 + 9 (MPEG-2 and 2.5 mono),
      with no adjustment for a CRC (`xing_offtbl` in libavformat/mp3dec.c). So on a file whose
      first frame follows valid tags directly, sopen and ffmpeg agree on which files are `Info`.
    - `Info` there means constant bitrate (ffmpeg's `is_cbr`). `Xing`, anything else, a frame that
      does not validate, or a file that ends first means an exact seek. Every misreading therefore
      costs bytes, never precision.
    - W7's fixtures, in two groups:
      - **Reference fixtures**, whose expected answer is what ffmpeg reports (its `is_cbr`), not
        what this text says: a version-4 tag with a footer, a tag of several MB, two tags in a row,
        MPEG-1 and MPEG-2 in mono and stereo, a CRC-protected frame, `Info`, `Xing`, and no tag.
      - **Fallback fixtures**, whose expected answer is an exact seek even where ffmpeg recovers:
        an invalid synchsafe byte, an unsupported tag version, junk before the first frame, and a
        file cut inside the frame.
      - X5 measured only mono 44.1 kHz files with small tags.
- **ffmpeg's reconnect options stay off.** The server ends a connection only on an error the window
  already shows from its Source, so a reconnect would only repeat it.
- **ffmpeg's exit status cannot be trusted for a cut-short input** (verified with ffmpeg 9.0.2,
  scratch). The server cut every response short at 300 kB. ffmpeg logged `Stream ends prematurely`
  and `partial file`, yet **exited 0**, and wrote a 3.2 s output from a 20 s source.
  - So the byte server keeps the account: it knows every response it ended before its
    `Content-Length` for a reason of its own (a read error, a `Changed` source).
  - A run whose input had any such response is failed, whatever ffmpeg's status: an Export, a 07
    render or Save As publishes nothing, and a probe or a frame shows the Source's state.
  - A connection ffmpeg itself closed early (it does so on every seek, verified) is not counted.
- **The waveform** needs every audio sample, so it stays a whole read:
  - it is decoded here, never on the remote (AGENTS.md: the remote needs only sshd);
  - it publishes its peaks as they fill, every ~250 ms, and the timeline draws what is known with a
    dim band for the rest;
  - it reads through its `bulk` URL, so it is a Bulk reader: it never evicts playback's blocks, and
    it yields to playback's demand (§4);
  - it runs only for audio-only files, as today.
- **Many inputs at once.** A PR #15 render opens up to 24 inputs (`ffmpeg::MAX_INPUTS`), each one
  `render` connection on the one in-order channel, sharing the 48 MiB of protected windows (§3).
  - Each reader keeps a floor of 256 KiB ahead and 64 KiB behind, whatever the share. So 32 readers
    need at most 10 MiB.
  - How such a render behaves against the cap is unmeasured. X1 (loop.md) measures PR #15's real
    `edit_args` and playback commands against a spike server with this block size, cache and cap
    before W7.
- **Export** reads only what the cut needs (*view* §2.3: 12 % of a 2-minute video for 10 s) and
  publishes as today (01 §7: a render under `<cache>/.exports`, then a new sibling, never over
  anything). Its progress is ffmpeg's time-based progress.
- **Export can be cancelled.** This is **the workstream's decision**, written into ssot's fixed scope
  (item 1: "streams with progress and can be cancelled"). It is not in message 17's own words. Today
  an export cannot be cancelled: a close only waits. The export's progress row gets **Cancel**, and
  Esc does the same while it runs.
  - **During the render:** ffmpeg is killed, and the local render and its folder are removed.
    Nothing is uploaded.
  - **During the upload:** the upload stops, and its remote temp is removed (01 §5: only a temp this
    export created). Nothing is linked under any name.
  - **After the publishing link has landed** it is too late. The result says the sibling was
    published, and names it.
  - Closing the window keeps today's rules (the first close waits, the second closes and sopen still
    waits and prints the result). Only Cancel cancels: an export nobody cancelled is finished.

### 7.5 The binary dialog

- **"Open in hex editor"** opens a Source and the hex window at once (§7.1). Every step that can fail
  (the channel, the open, the `fstat`) comes before the connection is handed over. After a failure
  the dialog is as it was.
- **"Save to…"** keeps 01 §6a's publishing rules (a private staging folder, `part`, a hard-link
  publish, never over anything). Its read is pipelined (§2).
  - **New: Esc and closing the window cancel it.** The transfer stops between chunks, `part` and the
    private folder are removed, nothing is published, and the terminal says `nothing was saved`.
  - This replaces 01 §6a's "during a transfer Enter and Esc do nothing and closing waits for it".
    It is **the workstream's decision**, from ssot's fixed scope item 1 ("can be cancelled"), not
    message 17's own words.
- **`--hex SOURCE`** opens a Source. **Without a display** the file is still fetched and its path
  printed (01 A6), now pipelined.

## 8. Whole-file actions

Viewing never needs the whole file. These actions do, and each streams with progress and a cancel.
None blocks a window before it shows.

| Action | Reads | Progress | Cancel |
|---|---|---|---|
| Image decode (§7.3) | the whole file, Bulk reader, into memory | bytes / size, with previews | Esc, or closing: the reader stops, nothing is kept |
| Interim hex Save (§7.1, until 07) | the whole file into the cache, as today | the status row | Esc (`fetch_claimed_cancellable`) |
| "Save to…" (§7.5) | the whole file into the private staging file | the dialog's bytes / size | **new:** Esc and closing |
| Waveform (§7.4) | every byte of an audio-only file, Bulk reader | the waveform fills in | closing kills the decoder and drops its reader |
| Text Go to past the prefix (§7.2) | from the last mark to the line | `indexing… X of Y` | Esc |
| Media Export (§7.4) | only what the cut needs, through the URL | ffmpeg's time | **new:** Cancel and Esc, at every stage (§7.4) |
| 07's Save, Save copy, Save As, media render | 07 §4–§9 | 07 | 07 |

Left as whole fetches, with the fetch now pipelined (§2):

- **`sopen cut`** with a remote source. It runs in the terminal, has no viewer, and reads what it
  keeps under 01 §7's rules. Ctrl+C ends it as today.
- **`sopen stems`** (`feat/stems`, its 01 §7a). It owns its cancel and publication. A cancel kills
  the child's process group, removes the work folder and publishes nothing. The child publishes only
  after the parent's `publish`, which is sent under the same lock Cancel takes, so a cancel either
  comes first (nothing reaches the remote) or after the go-ahead (both stems published). Its window
  button is "a later change" there. Streaming adds nothing to it except the pipelined fetch.
- **`--app`, the terminal editor (01 §3) and the no-display fallback (01 A6).** Each hands a local
  file to a program that reads it whole.

## 9. Robustness and security

**The loopback byte server (`stream::http`)**

- **One per media window**, started with the window and dropped with it. It is bound to
  `127.0.0.1:0`.
- **Three paths, one per reader kind.** It serves only `/<token>/<kind>/<file name>`, with `kind` =
  `play`, `bulk` or `render` (§7.4). The kind decides the connection's reader. The token is 128 bits from the OS
  (`getentropy` through the `libc` crate), not `random_hex()`, which is not secret-grade.
- **What its clients send** (verified with ffmpeg and ffprobe 9.0.2, scratch). Across a probe of a
  `moov`-at-end mp4, a one-frame seek, a cut and an mp3 `+fastseek` seek:
  - every request was a `GET` with one open range `bytes=N-`;
  - the host was always `Host: 127.0.0.1:<port>`;
  - the other headers were `Accept`, `Connection`, `Icy-MetaData` and `User-Agent`;
  - to seek, ffmpeg closed the connection and opened a new one.

  The contract below is that, plus a deliberate answer for everything else.
- **One request per connection.** The server reads one request head and answers it with
  `Connection: close`, then closes. Anything the client sends after the head is never read as a
  request.
- **The head.**
  - The request line and headers together are at most **8 KiB**. A longer head gets 431, and the
    connection closes.
  - The whole head must arrive within **5 s** of the accept, else the connection closes unanswered.
  - Header names are matched case-insensitively. `Icy-MetaData` and every header not named here are
    ignored.
- **Accepted:** `GET` or `HEAD`, one of the three paths, and **exactly one**
  `Host: 127.0.0.1:<port>`.
  - A page in a browser cannot reach the server by DNS rebinding (the fix of Transmission's
    CVE-2018-5702).
  - Any other method gets 405.
  - A wrong token gets 404 with no body, after the same work as a right one.
  - A missing, wrong or repeated `Host` gets 403.
  - A request with a body (`Content-Length` other than 0, or any `Transfer-Encoding`) gets 400.
- **Ranges.** Let `size` be the fingerprint's size.

  | Request | Response |
  |---|---|
  | no `Range` | 200, `Content-Length: size`, the whole file |
  | `bytes=N-` with `N < size` | 206, `Content-Range: bytes N-(size−1)/size`, `Content-Length: size−N` |
  | `bytes=N-M` with `N ≤ M` and `N < size` | 206 for `N`…`min(M, size−1)`: clamped to the end, `Content-Length` the clamped count |
  | `bytes=N-` or `bytes=N-M` with `N ≥ size` (every range of an empty file) | 416, `Content-Range: bytes */size` |
  | suffix `bytes=-K`, several ranges, `N > M`, a number past 2^63, any other unit or syntax | 416, `Content-Range: bytes */size`. Rejected on purpose: ffmpeg never sends them |
  | `HEAD`, with or without `Range` | the same status and headers as the `GET`, no body |

  Every 200 and 206 carries `Accept-Ranges: bytes` and `Connection: close`.
- **A response already started that cannot finish.** A read error or a `Changed` source mid-body
  closes the connection short of its `Content-Length`. No byte of a newer version is ever sent:
  blocks are validated (§5). The server records the cut, and the run that used it is failed (§7.4).
- **Deadlines.** A response whose client takes no bytes for **60 s** (01 §5's no-progress bound) is
  closed. The Source's own reads are bounded by §5's timeouts.
- **Connections.** Each connection is one Media reader, served by blocking `read_at` on a std thread.
  TCP backpressure from ffmpeg paces it.
  - At most **32** are served at once, and a 33rd gets 503.
  - PR #15 needs up to 31 at once: a render opens at most 24 inputs (`ffmpeg::MAX_INPUTS`), playback
    runs two pieces at a boundary (video and audio each, 4), and the paused frame, the waveform and
    a probe take one each.
- **Shutdown.** Dropping the server closes the listener, then shuts down every accepted socket
  (`shutdown` of both directions) and sets every connection's cancel flag. A thread blocked in
  `read_at` or a socket write then returns at once. The window never joins those threads.
- **Errors.** A read error or a `Changed` source ends the connection. ffmpeg sees EOF, and the window
  shows the Source's own state; it never parses ffmpeg's stderr for it.
- **ffmpeg's inputs.** Every remote ffmpeg and ffprobe input has `-protocol_whitelist http,tcp`. The
  default whitelist already refuses `file:` for a URL input. The explicit one also drops `udp`, `rtp`,
  `https` and `tls` (*view* §2.3).
- **Peer check on Linux: the client's end.** The accepted socket is sopen's own, so its uid proves
  nothing. sopen checks the **client's** socket instead.
  - The server's end is `127.0.0.1:P` (local) and `127.0.0.1:Q` (peer). The client's socket is the
    `/proc/net/tcp` entry whose local address is `127.0.0.1:Q` and whose remote address is
    `127.0.0.1:P`: the same tuple, reversed.
  - That entry's `uid` column must equal sopen's uid.
  - No such entry, or more than one, is a rejection: the connection is closed unanswered, as for any
    other uid.
  - The format (hex addresses with the IPv4 address in host byte order, the port big-endian, and
    `uid` as the eighth column) is the kernel's `/proc/net/tcp` layout. It is not run here (the Mac
    has no `/proc`); the parser's unit test pins it with fixture lines.
  - Tests:
    - the parser on fixtures, for the reversed-tuple match, an absent entry and two matches;
    - on Linux CI, a client run as another uid (`sudo -n -u nobody`, available on the hosted
      runners) is closed unanswered, and a client of sopen's own uid is served. Skipped with a note
      where `sudo -n` cannot run.
- **Residual on macOS, documented.** ffmpeg takes the URL as an argument, and ffmpeg 9.0.2 rejects
  `-/headers`. On macOS, `ps` shows other accounts' full command lines (*view* §2.3). So another local
  account on the same Mac can read the token while a media window is open, and with it read the
  viewed file through the server. Processes of the same user are trusted, as 01 §6a already says.
  This stays named in 01 until a macOS peer check (`proc_pidfdinfo` on the ffmpeg children) is
  spiked.

**Mid-stream errors** (§5):

- An SFTP status error marks the block failed. The viewer's status row names it, the screen stays,
  and the next interaction retries.
- A lost channel shows at once when the connection closes. A silent link shows when the master's
  keepalive (~30 s) or the request timeout fires. It is retried through `Reconnect`.
- A failed prefetch is never shown.

**Closing.**

- The viewer's drop closes its Source. Prefetch stops at once, and what is in flight (at most the
  cap) arrives and is dropped.
- Every `File` clone is dropped before `Sftp::close()`, which runs bounded to 5 s on the transport's
  thread (§2).
- The media window kills its ffmpeg children and drops its server.
- **No window close waits on the network.** The process exits within 5 s of the last window closing,
  unless a write is still running (01 §3a: a running save is waited for).

## 10. Bounds

| Resource | Bound |
|---|---|
| RAM per Source | 64 MiB of blocks + at most 16 MiB per reader with a wait outstanding (§3) + at most 4 MiB in flight |
| RAM, the hex window | + its page cache, 64 × 4 KiB |
| RAM, the text viewer | + its index, at most 16 MiB up to 64 GiB of indexed text (§7.2) |
| RAM, the image window | the encoded bytes + the decoded pixels, as today; above 512 MiB decoded only after Continue (§7.3) |
| Disk for viewing | **0 bytes**: nothing is created or grows under the cache directory |
| Disk for whole-file actions | as today: the interim hex Save's cache copy (§7.1), "Save to…"'s staging file |
| SFTP channels | 1 per remote viewer; 2 from its first Save, Save copy or Export (§6) |
| Bytes before a hex first screen | the blocks covering the screen and its margin: one 64 KiB block at offset 0 for any window under 1 300 rows, at most two elsewhere |
| Bytes before a text first screen | the blocks covering the bytes its rows read and their margin: one block for a file of short lines at offset 0; unwrapped, at most 30 rows × 4 KiB + the margin; the background index pass comes after it |
| A demand's wait behind what was sent | `(cap + one block per other waiting reader) / rate`, cap ≤ 4 MiB (§4) |
| The byte server | 32 connections, an 8 KiB request head, 5 s to send it, 60 s without progress (§9) |

## 11. Usage log and receipt hooks

- **Usage line.** 01 §8's line gains `bytes_fetched`: the bytes every Source of the run received. A
  streamed open keeps `bytes` (the file's size) and has `fetch_ms` 0.
- **New receipt hooks** (environment variables, as AGENTS.md's "Receipts"; a `--target` run carries
  them to HOST as it carries the others, listed in `src/forward.rs`):

| Hook | Effect |
|---|---|
| `SOPEN_STREAM_STATS=1` | One line on stderr at each event, per Source (format below). Every live byte bound of §12 is read from these lines. |
| `SOPEN_STREAM_THROTTLE=<ms>,<bytes/s>` | Every `Ranged` read is delayed by `<ms>` and paced to `<bytes/s>`, as the `Fake` does in tests, so a screenshot can catch rows still loading or a partial picture. |
| `SOPEN_STATE=…;loading=1` | Hex, text and image: a scripted `SOPEN_SCREENSHOT` is taken at the first frame drawn with content from the stream: rows, or a thumbnail or partial picture. Pending rows are shot as they are. Without it, a streamed viewer is ready, and shot, once the screen's bytes have arrived or failed. |

**The stats line:**

```
stream <event> t=<ms> <host:/abs> fetched=<b> demand=<b> prefetch=<b> bulk=<b> delivered=<b>
       http_sent=<b> sniff=<b> requests=<n> max_in_flight=<b> runs=<n> jumps=<n> cuts=<n>
```

- **The events**, each with `t` in ms since the open began:
  - `open`: the open began (the Enter, the dialog's button, or `--hex`);
  - `first`: the first frame drawn with content from the stream;
  - `idle`: 2 s after `first` with no input in between;
  - `input`: a key, click or drag that moves the view to a place not yet shown (a jump, End, Go to,
    a scrub or a seek), with that place;
  - `ready`: the first frame showing content for the last `input`'s place: rows, a video frame, or
    the first PCM given to the audio output;
  - `close`: the Source closed.
- **Every counter is cumulative.**
  - `fetched` is what arrived from the remote, split into `demand`, `prefetch` and `bulk` (§4).
  - `delivered` is the union of byte ranges readers actually consumed.
  - `http_sent` is the union of byte ranges the byte server sent to ffmpeg.
  - `sniff` is the kind sniff's bytes, read on the open's connection before it becomes the
    `Ranged`, counted per run.
  - `runs` is the number of sequential runs (§4).
  - `jumps` is the number of jumps.
  - `cuts` is the number of responses the server ended early (§9).

## 12. Acceptance

**Two kinds of check.**

- **Gates** are deterministic and run in CI and every unit's `just check`. They are byte, request and
  ordering bounds, on `remote::Ranged`'s `Fake` (in order, 20 ms latency, 5 MB/s unless a scenario
  says otherwise), on `local_server()`'s `sftp-server` child, and on a std `TcpStream` client for
  the byte server. A gate never asserts a wall-clock time, except as a generous timeout that only
  catches a hang.
- **Qualification** is the live receipt on the reference rig: the Mac (Apple silicon), the Linux
  laptop's OpenSSH `sftp-server` on tmpfs, the mesh-VPN link of the header. It runs with
  `SOPEN_STREAM_STATS=1`, with the ssh master already up (the dialog or the browser opened over it),
  and with the file not read in the previous 60 s by this process.
  - **Opening bounds** run from `open` to `first`: for example, `first − open ≤ 1.0 s`.
  - **Interaction bounds** run from the `input` that started them to its `ready`: for example, a jump's
    `ready − input ≤ 1.0 s`.
  - The bytes come from the same lines.
  - Each live receipt runs on a file under a remote `/tmp/sopen-receipts/`.

**The owner's scenario.**

- **V1. 24 GB, first screen.** `sopen host:/tmp/sopen-receipts/model.safetensors` on a 24 000 000 000
  byte file, then Enter on the dialog.
  - Gate (`Fake`): at the first frame with rows, `fetched ≤ 64 KiB`. The window's ~30 rows (480
    bytes) and their margin lie in block 0, and readahead is 0 at open (§4).
  - Gate: with no input for 2 s after that, nothing more is sent.
  - Gate: `sniff ≤ 8 KiB`, so ≤ 72 KiB of the file crosses the network in all.
  - Qualification: `first − open ≤ 1.0 s`; at `first` and `idle`, `fetched ≤ 64 KiB` and
    `sniff ≤ 8 KiB`. Measured on the rig: a new channel plus open, `fstat` and the first 64 KiB took
    0.19–0.25 s warm and 0.49 s cold (*view* §2.2).
  - Both: nothing is created under the cache directory.
- **V2. 24 GB, scrolling stays bounded.** Hold PgDn for 30 s, drag the scroll bar to the middle and
  back to the top, then hold PgUp for 10 s.
  - Gate (`Fake`, the same key and drag script):
    `fetched ≤ delivered + runs × 1 MiB + jumps × (128 KiB + cap)`.
  - Qualification: the same inequality on the `close` line, and resident memory at most **80 MiB**
    above the window's at `first` (64 MiB of blocks, 4 MiB in flight, the page cache and slack).
  - Both: the cache directory is unchanged.
- **V3. 24 GB, far jump.** Cmd/Ctrl+G `0x500000000` (20 GiB).
  - Gate: the jump's demand and margin add at most 128 KiB to `fetched`.
  - Gate: the bytes delivered for the old place after the jump are ≤ the cap.
  - Qualification: `ready − input ≤ 1.0 s` for the jump (measured 20–94 ms per 64 KiB at 20 and
    24 GiB).

**Transport and core** (gates unless marked).

- **V4. Admission.** On the `Fake`, every request carries the bytes sent ahead of it.
  - One Interactive reader streaming: the bytes ahead of any demand are ≤ the cap.
  - Two Interactive readers and one Bulk reader on one Source: the bytes ahead of a demand are ≤ the
    cap + one block per other reader with demand queued. No bulk request is sent while foreground
    demand is queued.
  - Ten jumps within 100 ms: nothing queues. Every demand sent was the reader's latest place when
    it went, and a place passed while the cap was full is never sent
    (`v4_ten_jumps_on_an_idle_link_never_queue_a_passed_place`: on the gate link from an idle
    start, 9 of the 10 places find room and go). While the cap is full, only the last place is sent
    (`v4_ten_jumps_send_only_the_last_place`).
  - **Fences with the cap full:** prefetch fills the cap and more prefetch is queued. A demand batch
    is admitted when room frees. Its fence is sent right behind its read, ahead of the queued
    prefetch, and the block is visible once that fence returns. It does not wait for the queued
    prefetch to be sent or answered.
  - A poll due while prefetch and bulk are queued goes out ahead of them.
  - The `Fake` drops from 5 MB/s to 0.5 MB/s mid-stream: after the next rate update, nothing is sent
    until `in_flight + block` fits the lowered cap.
- **V5. Explicit offsets, short replies, request order.**
  - Through the clipping proxy (§2), which cuts every `SSH_FXP_DATA` to 20 000 bytes, `read_to`, the
    kind sniff and a Source's reads of a 4 MiB file give bytes whose SHA-256 equals the file's. The
    same test fails on 3a426b6's `read_with`.
  - On the in-order `Fake`, every validation `fstat` arrives after each read it validates (§2).
  - A reply short of the block, followed by `SSH_FX_EOF`: the block is not used, and the Source is
    `Changed`.
  - On `local_server()`, a truncate between the two halves of a block does the same.
- **V6. Pipelined whole reads.**
  - Gate: on the `Fake`, `read_to` of 64 MiB finishes in at most ⅔ of the simulated time that one
    request in flight takes.
  - Qualification: "Save to…" of a 1 GB file runs at least 1.5× today's rate, measured in the same
    minute.
- **V7. Remote change while viewing.**
  - Rewrite the file in place right **after a successful poll**, then cause a cache miss. The block
    read across the change is never shown; the Source turns `Changed`; no block of the new version
    enters the cache. Completions of the old generation arriving after the change are dropped.
  - A clipped read whose file changes in place between the first reply and its remainder: the
    block is never shown, and the Source turns `Changed`.
  - Rename another file over it: the viewer keeps showing the version opened and says it was
    replaced.
  - Cmd/Ctrl+R reloads both.
  - After one of sopen's own saves, no change is reported.
  - The one documented gap: a same-size write within the same second as the file's mtime is not
    seen (V7 pins that the `Fake` shows it as unchanged, so the limit stays stated, not hidden).
- **V8. Close during prefetch.** Close a window while 4 MiB of prefetch is in flight. The window goes
  at once, and on the `Fake` nothing is requested after the close, except the open of a reconnect
  already under way, which is closed on return unused (§5). Qualification: the process exits
  within 5 s.
- **V9. Lost connection.** Kill the transport while scrolling (the `Fake`'s channel closes; live, the
  ssh master is killed).
  - The status row says so on the next read, and the screen stays.
  - A key once the transport is back continues from the cache, with the same fingerprint.
- **V10. MaxSessions, both moments** (a `Connect` that fails as sshd's refusal does):
  - **At open** (W8's gate, since the open's own connection is the routes'): the line shows, and the
    dialog keeps its connection for a retry.
  - **At the `Origin`'s first use** (ED4's gate, 07 E21): with unsaved hex edits, Save fails with the
    line, the edits stay in the window and the journal, nothing is created on the remote, and a Save
    once the refusal ends lands.

**Viewers.**

- **V11. Hex edits over a stream.** On the `Fake`:
  - `changed`, `changed_count`, `dirty` and `unsaved` with 1 000 patches send no request;
  - a key on a Pending byte changes nothing;
  - a `SOPEN_STATE` patch lands once its bytes arrive.
- **V12. Text, 4 GiB remote log** (lines under 200 bytes). Gates on the `Fake`:
  - at `first`, `fetched ≤ 64 KiB`, because the background pass has not started;
  - the background pass then reads at most the first 16 MiB (`bulk ≤ 16 MiB`);
  - End adds at most 128 KiB of demand and prefetch, the gutter shows byte offsets, and the status
    row says `lines known to …`;
  - PgUp from there adds at most 64 KiB of demand per page;
  - Go to line 10 000 000 shows progress, and after Esc only what was in flight (≤ the cap) arrives;
  - the index stays within 16 MiB on a 4 GiB file of empty lines.

  Qualification: `first − open ≤ 1.0 s`, and End's `ready − input ≤ 1.0 s`.
- **V13. Text, small file.** A remote file of at most 16 MiB shows its first screen before its index
  finishes. Once indexed, it has the line count and highlighting it has today.
- **V14. Image, partial picture.**
  - Gate (`Fake`): a 6000×4000 progressive JPEG (16.6 MB) shows its size and Exif thumbnail having
    fetched ≤ the bytes up to its SOF + one block; a whole-frame preview exists by 25 % fetched; Esc
    stops the read and keeps nothing.
  - Gate: a 3000×2000 plain PNG shows ≥ 10 % of its rows by 25 % fetched.
  - Gate: a baseline JPEG shows its top rows by 25 % fetched.
  - Receipt: screenshots, with `SOPEN_STREAM_THROTTLE` and `loading=1`, of the progressive JPEG's
    coarse frame and of the plain PNG's top rows, each taken before the read completes.
- **V15. Image header limits.**
  - A JPEG with a 64 KB Exif APP1 and an ICC segment before its SOF shows its size and thumbnail
    having fetched ≤ 1 MiB.
  - A PNG whose header claims 100000×100000 shows the memory line with Continue and Close, having
    fetched ≤ 64 KiB, and Continue proceeds (W6). Once W8 lands, the line also offers Hex view,
    which switches to the hex window, and a file that does not decode opens in the hex window; until
    then it keeps today's error row (W8's gate).
- **V16. Video, `moov` at the end** (123 MB, remote; gates on `local_server()` with a `lavfi`
  fixture of the same shape).
  - Gate: at every step, `fetched ≤ http_sent + Σ live readers' ahead + cap`.
  - Qualification: `first − open ≤ 1.5 s` for the probe and first frame (measured 0.30 + 0.10 s),
    and `ready − input ≤ 1.5 s` for a far scrub to 80 % (measured 0.41–1.06 s across three
    containers). Measured with no readahead:
    2.08 MB fetched at the first frame, and 19.1 MB after 10 s of play and one far seek.
- **V17. Local files are untouched.** A local file opens in every viewer with no Source created
  (`SOPEN_STREAM_STATS` prints no line) and reads in place, as today.
- **V18. mp3, 600 s.**
  - Gate: a seek to 480 s, between the seek and its first PCM, adds ≤ 1 MiB + `ahead` + the cap to
    `fetched` (measured 0.53 MB with `+fastseek`, 18.35 MB without).
  - Gate: Export of 470–480 s has no `+fastseek`, and its output is 10.000 s long.
  - Qualification: the seek's `ready − input ≤ 0.5 s`.
- **V19. Waveform** of a remote 24 MB mp3.
  - Gate: the overview completes with `fetched ≤ 1.05 ×` the file.
  - Gate: while it runs, no bulk request is sent while a playback demand is queued.
  - Qualification: the first peaks show within 1 s of `open`, and a seek while it runs has
    `ready − input ≤ 0.5 s`.
- **V20. The byte server** (std `TcpStream` client, every row of §9's tables):
  - **Ranges:**
    - `bytes=N-` and `bytes=N-M` give exactly the clamped count, with the right `Content-Range`;
    - `bytes=N-M` with `M ≥ size` is clamped to `size − 1`;
    - `N ≥ size` gives 416 with `bytes */size`, and so does every range of an empty file;
    - suffix, several ranges, `N > M` and a number past 2^63 each give 416;
    - `HEAD` gives the `GET`'s headers and no body, with and without a range.
  - **Requests:**
    - no token, or a kind other than `play`, `bulk` and `render`, gets 404 and 0 bytes;
    - a `bulk` connection is served as a Bulk reader: none of its requests goes out while a `play`
      demand is queued;
    - a missing, wrong or repeated `Host` gets 403;
    - `POST` gets 405;
    - a body or `Transfer-Encoding` gets 400;
    - a head of 9 KiB gets 431;
    - a head trickled over 6 s is closed unanswered;
    - a second request on the same connection is never answered;
    - a 33rd connection gets 503.
  - **Lifetime:**
    - a client that stops reading is closed after 60 s (simulated time);
    - after the window closes, the port refuses connections, and a thread blocked in a write
      returns;
    - a source that changes during a response already started cuts it short, sends no byte of the
      new version, and counts a cut;
    - on Linux, the peer check finds the client's end by the reversed tuple; a client of another uid
      is closed unanswered, and an absent or doubled entry is rejected (§9's tests);
    - every ffmpeg and ffprobe run on a remote input has `-protocol_whitelist http,tcp`.
- **V21. "Save to…" of a 24 GB file.** Esc cancels it and leaves no file and no staging folder, and so
  does closing the window (gate on `local_server()` with a throttled read). Qualification: it runs
  at the pipelined rate (V6).
- **V22. Export cancel and failure** (gates on `local_server()` and a `lavfi` fixture):
  - **During the render:** Cancel kills ffmpeg, leaves no file under `<cache>/.exports`, and creates
    nothing on the remote.
  - **During the upload:** Cancel removes the remote temp and links nothing.
  - **After the link:** a Cancel arriving later reports the published sibling.
  - **An existing `<stem>.cut-…` name:** the export publishes `-2`, never over it.
  - **A response cut short during the render** (the source changed): nothing is published, though
    ffmpeg exits 0 (§7.4).

## 13. Work units

Each unit is one agent, one branch and one worktree (`.worktrees/<branch with dashes>`). The live
table, with status, is [`streaming/loop.md`](streaming/loop.md). A unit's gate is AGENTS.md's
`just check`, run with `SOPEN_REQUIRE_FFMPEG=1 SOPEN_REQUIRE_SFTP_SERVER=1`, as CI runs it. A unit
that shows something on screen also brings a screenshot receipt someone looked at.

| ID | Unit | Owns | After |
|---|---|---|---|
| W1 | **Transport.** `Ranged` (with `reopen` and `into_connection`) and `into_ranged`, which gives the connection back on failure; the SFTP, local and `Fake` implementations; short replies and early ends; one task sending in queue order; the validation `fstat`; bounded close; `read_with` / `read_to` with explicit offsets and up to 4 MiB in flight (fixes the latent cursor bug, §2); the clipping proxy for V5 | `src/remote/ranged.rs` (new: trait, local impl, `Fake`), `src/remote/sftp/ranged.rs` (new, child module), `src/remote/sftp.rs` (`read_with`, `read_to`, the `mod` line), `src/remote/mod.rs` (`into_ranged`, the `mod` line), `src/remote/local.rs` (`into_ranged`), `src/remote/tests.rs`; `Cargo.toml` and `Cargo.lock` (tokio dev-features for the proxy, if any) | — (if PR #16 lands first, W1 rebases: they share only `Cargo.toml`, in different sections) |
| W2 | **Stream core.** `Source`, `BlockCache` with its floors, `Prefetcher` with §4's admission, `Reader`, `Bytes`, validation and generations, polling, reload, the MaxSessions line; the counters and events of `SOPEN_STREAM_STATS`, and `SOPEN_STREAM_THROTTLE` (§11) | `src/stream/{mod,cache,prefetch,source}.rs`, `src/stream/tests.rs` (all new); `src/lib.rs` (`pub mod stream`) | W1 |
| W3 | **Byte server.** §9 whole: token, the three reader-kind paths, head limits and deadlines, the Host rules, the range table, one request per connection, the 32-connection bound, shutdown, the cut account, the Linux peer check | `src/stream/http.rs`, `src/stream/http/tests.rs` (new) | W2 |
| W4 | **Hex streams.** `Pages` over `Bytes`, Pending and Failed rows, the drag debounce, patches keeping their byte (fixes the hex bug, §7.1), the interim Save. Its first commit is a pure move of the write side (`Ident` … `HexFile::save_copy`) into `src/gui/hex/file.rs`. Adds `SOPEN_STATE`'s `loading=1` (§11) | `src/gui/hex.rs`, `src/gui/hex/file.rs` (new), `src/gui/hex/tests.rs`; `src/gui/mod.rs` (`State`'s `loading` field only) | W2; browser S1 merged |
| W5a | **Text reads through `Bytes`.** Pending travels like `Stale`; the first screen first; the background pass capped at 16 MiB, continuing S1-B's 4 MiB preview pass; highlighting over the indexed prefix (§7.2) | `src/gui/text.rs`, `src/gui/text/tests.rs` | W2; S1 merged; X3 |
| W5b | **Large text files by byte position.** The prefix index, byte-offset keys in `TextWindow` and wrap, unknown line starts, Go to, the index memory bound (§7.2) | `src/gui/text.rs`, `src/gui/text/wrap.rs`, `src/gui/text/tests.rs` | W5a |
| W6 | **Image streams.** Header walk with the SOF read directly, thumbnail, the memory line with Continue, the reserved buffer, the streamed read, partial pictures for the four measured formats (plain PNG included) | `src/gui/image.rs`, `src/gui/image/probe.rs` (new), `src/gui/image/robustness_tests.rs`; `Cargo.toml` and `Cargo.lock` (the direct `png` dependency) | W2; S1 merged; X4 |
| W7 | **Media streams.** URL `Input`s with their own input options and reader kinds, the progressive waveform on a `bulk` connection, Export through the URL and its Cancel, runs failed on a cut response | `src/gui/media.rs`, `src/gui/media/*`, `src/gui/playback.rs`, `src/ffmpeg.rs`, `src/cut.rs` | W3; S1, PR #15 and S1-M merged; X1, X5 |
| W8 | **Routing.** The browser's Enter, the standalone route, the dialog and `--hex` open a Source instead of fetching, with the placeholder `Origin`; the image → hex switch (§7.3); the preview change below, with its 02 §4.3–§4.4 text and S1's tests it changes; the `.ts` refine over the URL; "Save to…" cancel; `bytes_fetched`; forwarding §11's hooks | `src/gui/browser/prepare.rs`, `src/gui/browser/preview.rs` (the hand-over only), `src/gui/browser.rs` (the hex switch), `src/gui/browser/tests.rs` (P6 and P12's remote parts), `src/cache.rs` (the `.preview` root, if unused), `src/gui/binary.rs`, `src/gui/binary/tests.rs`, `src/main.rs`, `src/kind.rs`, `src/usage.rs`, `src/forward.rs`, `src/gui/mod.rs` (`Origin`), `tests/cli.rs`, `.prd/02-architecture.md` (§4.3–§4.4) | W4, W5a, W5b, W6, W7 and ED6; S1 and S1-M merged |

07 §14 has the edit units (ED1–ED6) and the docs unit (D1), which rewrites 01, AGENTS.md and the
README once the code has landed. The experiments X1–X5 and the merge order across both workstreams
are in [`streaming/loop.md`](streaming/loop.md).

**Order** (loop.md has the full merge order):
1. W1 → W2 → W3, alongside S1-A…D.
2. Once S1 has merged: W4, W5a and W6.
3. Once W3, PR #15 and S1-M have merged: W7. W5b follows W5a.
4. ED6 (media edit on streams), then W8, which wires the routes. W8 waits for W5b too, so a remote
   text file never ships with W5a's 16 MiB stop-gap.

Why ED6 comes before W8: PR #15's Save refuses a URL input (`media/file.rs`), and its background hash
skips remote files. If W8 routed remote media through a Source first, remote media Save would break
until ED6.

- W4–W7 land with remote files still fetched by today's routes. Each viewer reads `Bytes`, which a
  local `File` implements, and is tested over a Source on the `Fake` and on `local_server()`. W8
  switches the routes to open a Source. So `main` ships a working build after every merge.
- `src/lib.rs` gets one `mod` line each from W2 and ED2, which may run in parallel; whichever merges
  second rebases.
- `src/gui/mod.rs` is edited by W4 (`State`), then W8 (`Origin`). ED3's `mod` line rebases onto
  whichever has landed.
- `Cargo.toml` and `Cargo.lock` are edited by W1 (dev-features, if any), then W6 (`png`), in order.

**Serial chains across 06 and 07.** These files are needed by a view unit and then an edit unit, so
each is owned by one unit at a time, in this order:

- `src/gui/hex.rs`, `src/gui/hex/*`: W4 → ED4.
- `src/gui/image.rs`, `src/gui/image/*`: W6 → ED5.
- `src/gui/media.rs`, `src/gui/media/*`: PR #15 → S1-M (browser) → W7 → ED6.
- `src/gui/text.rs`, `src/gui/text/*`: W5a → W5b.
- `src/remote/{mod,sftp,local}.rs`: W1 → ED1 (`mod` lines only). `src/remote/sftp/ops.rs`: S1-C → ED1
  (the shared `copy-data` loop, 07 §4.2).

**Files shared with the browser workstream** (02 §8's slices S1–S7, one at a time; S1-0 is merged)
are not disjoint, and are sequenced instead.

- **Its S1** lands first and owns `src/gui/browser/prepare.rs`, `src/cache.rs` and the viewers'
  preview changes (`set_preview`, S1-B). W4–W8 rebase onto it. S1-M takes `media.rs` after PR #15,
  before W7.
- **S2-E** gives the viewers a `read_only` flag (02 §5). It comes after W4–W7 and ED4–ED6, or touches
  only the `Window` trait's default. This docs PR adds that sentence to 02 §8 when it is rebased
  onto `main` (the exact text is in loop.md), because S2 starts right after S1.
- **PR #16** (stems) touches `Cargo.toml`, `main.rs`, `ffmpeg.rs`, `cut.rs`, `lib.rs`, `usage.rs` and
  `tests/cli.rs`. It shares only `Cargo.toml` with W1, in a different section, so neither waits: the
  one that merges second rebases. Its remote sources go through `fetch_to_cache`, which W1 only
  speeds up.
- Later browser slices also touch `src/lib.rs` and `src/remote/{mod,sftp,local}.rs` (`mod` lines),
  `src/gui/mod.rs`, `src/gui/chrome.rs`, `src/main.rs`, `src/gui/browser.rs`, `AGENTS.md`,
  `README.md` and `.prd/01-spec.md`. Whichever lands second rebases. A conflict on any of these goes
  to the dispatcher, never resolved by dropping the other side.
- **The preview** (02 §4, merged with PR #17) stays the browser's contract. W8 changes three things
  in it, and writes them into 02 §4.3–§4.4 in the same PR, with S1's tests it changes: P6's "claimed
  by a hard link (same inode)" and P12's "remotely, one fetch". 02 §4.3 already foresees the first:
  "with streaming (another workstream), only the byte ranges the preview shows, never more than
  32 MiB".
  1. **The read.** Today a remote preview reads into a private copy, `<cache>/.preview/<window>/<n>`.
     After W8 it reads through a Source on the window's one preview session, which becomes that
     Source's `Ranged`.
     - The 32 MiB budget becomes a cap on the preview's `fetched`, no longer a gate on the file's
       size. So a large remote file previews its first rows, and a large remote video previews its
       first frame (02 §4.3: "Lifting that limit belongs to the streaming workstream").
     - The other budgets are unchanged: 4 MiB of text index, 64 Mpx of image, 5 s and 10 minutes.
     - Nothing is written to disk. A local file is still read in place.
  2. **The hand-over.** Enter on a previewed remote file moves the Source to the viewer, with its
     cache, its readers and its session.
     - No byte is read twice. P6 and P12 then assert the reads' ranges and `fetched`, not a hard
       link or a single fetch.
     - Nothing is published into the cache, so 02 §4.4's hard-link publish no longer happens for a
       streamed file.
     - The window opens a new preview session at its next remote preview. There is still at most one
       preview session at a time; the one handed over now counts as the viewer's.
     - 02 §4.4's placeholder `Origin` stays exactly as it is, and 06 §2 makes it every streamed
       viewer's rule.
     - The `.preview` root is removed if nothing else uses it.
  3. **A cancelled preview** (the bar moves on) calls `reopen(path)` on its `Ranged` for the next
     file, or `into_connection()` when the next preview is not remote. So the window keeps its one
     preview session.
     - Its in-flight bytes (≤ the cap) delay the next preview by at most `cap / rate`.
     - 02's rule of one preparation at a time is unchanged.

**Tests stay off the network** (AGENTS.md): the `Fake`, `LocalFs` and the `local_server()` child carry
every test.

## 14. 01 text this replaces

Each unit's PR names the sentences it makes untrue. D1 (07 §14) rewrites them in 01 as the shipped
contract.

- **A6 and §6a.** "Enter = Open in hex editor (the default: fetched to the cache, then sopen's hex
  window)" and "Hex: the file is fetched into the cache as every fetch is …" (W8). And "during a
  transfer Enter and Esc do nothing and closing waits for it" (W8, §7.5).
- **§3a, Enter on a file.** "A remote file is fetched into the cache exactly as the standalone routes
  fetch it …" (W8). "A remote file is viewed from its cached copy, which does not change under the
  viewer and is not followed" (W5a, §7.2). "Opening indexes the lines in one pass" for remote files
  (W5a, W5b).
- **§6a, the hex window.** "whether a byte is changed is a comparison with the file" becomes a
  comparison with the byte the patch replaced (W4).
- **§7.** Media reads its `Input` as a URL for remote files (W7).
- **§8.** `bytes_fetched` (W8).
- **§7.** Export gets Cancel, and a run whose input was cut short publishes nothing (W7, §7.4).
- **§9 and §10.** Already edited with these documents: §9 no longer lists streaming playback as out
  of scope, and §10 no longer waits on usage to consider streaming.
- **02 §4.3 and §4.4** (the browser's document): the preview's read, hand-over and cancel, as §13
  says. W8 writes them there, in its own PR.
- **02 §8**: one sentence, added by this docs PR at its rebase (loop.md): S2-E comes after the
  viewer units, or touches only the trait default (§13).
