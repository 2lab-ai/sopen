//! The preview's preparation (02 §4.3–§4.4) against a real SFTP server (this machine's own
//! `sftp-server` over pipes, posing as host `box`) and the local disk: it reads without claiming
//! the cache, keeps to its budgets — an info card when one is hit, before the bytes past it are
//! read — and uses the window's one preview connection. Every remote file reads through a stream
//! on that connection (06 §13 W8a, W8b): nothing is written to disk, a cancelled preview gives
//! the connection back, and the hand-over moves the stream to the viewer — an image's stream,
//! read whole, gives it back at once, and such a viewer's own session opens on its first remote
//! call. Enter's own preparation is at the end.

use super::*;
use crate::gui::testkit::{command, Driver};
use crate::gui::Window;
use crate::remote::SftpRemote;
use eframe::egui::{self, Key};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// What a test runs after a request: its name and path.
type After = Box<dyn FnMut(&str, &str) + Send>;

/// What crossed the connections a test opened. `content` counts the bytes a whole read's reader
/// accepted; `ops` logs every request made, in order.
#[derive(Default)]
struct Counts {
    content: AtomicU64,
    heads: AtomicU64,
    reads: Mutex<Vec<(String, u64)>>,
    ops: Mutex<Vec<String>>,
    /// What streams asked of the connections that became their transport: `(path, offset,
    /// length)` of every read, in order.
    ranged: Mutex<Vec<(String, u64, u32)>>,
    /// Bytes those reads' replies carried.
    streamed: AtomicU64,
    /// How long a stream's transport takes to give its session back (a slow shutdown).
    return_delay: Mutex<Duration>,
    /// Connections alive now, and the most at once.
    live: AtomicUsize,
    peak: AtomicUsize,
    /// Called after every request, with its name and path (tests: a cancel, a change, a gate).
    after: Mutex<Option<After>>,
    /// Every lstat fails with this, the session staying healthy (a path error).
    lstat_error: Mutex<Option<std::io::ErrorKind>>,
    /// Called on the first read response — a whole read's, or a stream's reply with bytes —
    /// before the reader gets it (tests: hold the read).
    gate: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}

impl Counts {
    fn op(&self, op: &str, path: &str) {
        self.ops.lock().unwrap().push(format!("{op} {path}"));
        if let Some(after) = self.after.lock().unwrap().as_mut() {
            after(op, path);
        }
    }

    fn ops(&self) -> Vec<String> {
        self.ops.lock().unwrap().clone()
    }
}

/// A connection that counts what is read through it. `dead`: its session is gone (every request
/// fails).
struct Counting {
    /// `None` only once the session has gone to a stream ([`CountingRanged`] has it then).
    inner: Option<Box<dyn RemoteFs + Send>>,
    counts: Arc<Counts>,
    dead: bool,
    /// The session, counted alive: handed on when it becomes a stream's transport.
    alive: Option<Alive>,
}

/// One session counted alive, from its connect until whoever has it last lets go — the
/// connection, the transport it became, the connection that gave back: the count never dips
/// in between.
struct Alive(Arc<Counts>);

impl Alive {
    fn new(counts: &Arc<Counts>) -> Self {
        let live = counts.live.fetch_add(1, Ordering::SeqCst) + 1;
        counts.peak.fetch_max(live, Ordering::SeqCst);
        Self(counts.clone())
    }
}

impl Drop for Alive {
    fn drop(&mut self) {
        self.0.live.fetch_sub(1, Ordering::SeqCst);
    }
}

impl Counting {
    fn new(inner: Box<dyn RemoteFs + Send>, counts: &Arc<Counts>) -> Self {
        Self::carried(inner, counts, Alive::new(counts))
    }

    /// A session already counted, as a connection again.
    fn carried(inner: Box<dyn RemoteFs + Send>, counts: &Arc<Counts>, alive: Alive) -> Self {
        Self {
            inner: Some(inner),
            counts: counts.clone(),
            dead: false,
            alive: Some(alive),
        }
    }

    fn inner(&mut self) -> &mut (dyn RemoteFs + Send) {
        self.inner.as_mut().expect("a live session").as_mut()
    }

    /// A connection whose session is gone.
    fn dead(mut self) -> Self {
        self.dead = true;
        self
    }

    fn alive(&self) -> Result<()> {
        if self.dead {
            bail!("the session was lost");
        }
        Ok(())
    }

    /// `op` on a dead session: logged (it was asked), then failed.
    fn dead_op(&self, op: &str, p: &str) -> Result<()> {
        if self.dead {
            self.counts.op(op, p);
        }
        self.alive()
    }
}

/// What a reader gets: each write is one SFTP read response, held at the gate the first time,
/// then handed on.
struct CountingWriter<'a> {
    out: &'a mut dyn std::io::Write,
    counts: &'a Counts,
    n: u64,
}

impl std::io::Write for CountingWriter<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = self.out.write(buf)?;
        self.n += n as u64;
        Ok(n)
    }
    fn write_all(&mut self, buf: &[u8]) -> std::io::Result<()> {
        let gate = self.counts.gate.lock().unwrap().take();
        if let Some(gate) = gate {
            gate();
        }
        let mut rest = buf;
        while !rest.is_empty() {
            let n = self.write(rest)?;
            if n == 0 {
                return Err(std::io::ErrorKind::WriteZero.into());
            }
            rest = &rest[n..];
        }
        Ok(())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.out.flush()
    }
}

impl RemoteFs for Counting {
    fn stat_opt(&mut self, p: &str) -> Result<Option<Stat>> {
        self.dead_op("stat", p)?;
        let got = self.inner().stat_opt(p);
        self.counts.op("stat", p);
        got
    }
    fn lstat_opt(&mut self, p: &str) -> Result<Option<Stat>> {
        self.dead_op("lstat", p)?;
        let injected = *self.counts.lstat_error.lock().unwrap();
        let got =
            match injected {
                Some(kind) => Err(anyhow::Error::new(std::io::Error::from(kind))
                    .context(format!("cannot stat {p}"))),
                None => self.inner().lstat_opt(p),
            };
        self.counts.op("lstat", p);
        got
    }
    fn realpath(&mut self, p: &str) -> Result<String> {
        self.alive()?;
        let got = self.inner().realpath(p);
        self.counts.op("realpath", p);
        got
    }
    fn read_head(&mut self, p: &str, max: usize) -> Result<Vec<u8>> {
        self.alive()?;
        let head = self.inner().read_head(p, max)?;
        self.counts
            .heads
            .fetch_add(head.len() as u64, Ordering::Relaxed);
        self.counts.op("read_head", p);
        Ok(head)
    }
    fn list_dir(&mut self, p: &str) -> Result<Vec<String>> {
        self.alive()?;
        self.inner().list_dir(p)
    }
    fn list_entries(&mut self, p: &str) -> Result<Vec<crate::remote::Entry>> {
        self.alive()?;
        self.inner().list_entries(p)
    }
    fn read_to(
        &mut self,
        p: &str,
        out: &mut dyn std::io::Write,
        progress: &mut dyn FnMut(u64),
    ) -> Result<u64> {
        self.alive()?;
        // A route that reads the owner's 24 GB whole fails here, at once, not after an hour.
        if self.inner().stat(p)?.size > 1 << 30 {
            self.counts.op("read_to", p);
            bail!("{p} is read whole");
        }
        let counts = self.counts.clone();
        let mut counted = CountingWriter {
            out,
            counts: &counts,
            n: 0,
        };
        let result = self.inner().read_to(p, &mut counted, progress);
        let n = counted.n;
        self.counts.content.fetch_add(n, Ordering::Relaxed);
        self.counts.reads.lock().unwrap().push((p.to_string(), n));
        self.counts.op("read_to", p);
        result
    }
    fn create_from(
        &mut self,
        local: &Path,
        p: &str,
        like: Option<crate::remote::Like>,
    ) -> std::result::Result<crate::remote::Uploaded, crate::remote::CreateError> {
        self.inner().create_from(local, p, like)
    }
    fn set_mode(&mut self, p: &str, mode: u32) -> Result<()> {
        self.inner().set_mode(p, mode)
    }
    fn rename_over(&mut self, from: &str, to: &str) -> Result<()> {
        self.inner().rename_over(from, to)
    }
    fn link_new(&mut self, from: &str, to: &str) -> Result<()> {
        self.inner().link_new(from, to)
    }
    fn can_link(&self) -> bool {
        self.inner.as_ref().is_some_and(|i| i.can_link())
    }
    fn can_replace(&self) -> bool {
        self.inner.as_ref().is_some_and(|i| i.can_replace())
    }
    fn remove(&mut self, p: &str) -> Result<()> {
        self.inner().remove(p)
    }
    /// The session becomes a stream's transport (06 §2) and stays counted: it is the same one,
    /// and it is counted again when the stream gives it back.
    fn into_ranged(
        mut self: Box<Self>,
        path: &str,
    ) -> std::result::Result<Box<dyn crate::remote::Ranged>, crate::remote::Refused> {
        let counts = self.counts.clone();
        let inner = self.inner.take().expect("a live session");
        let alive = self.alive.take().expect("a live session");
        if self.dead {
            return Err(crate::remote::Refused {
                conn: Box::new(Counting::carried(inner, &counts, alive).dead()),
                error: anyhow!("the session was lost"),
            });
        }
        drop(self);
        counts.op("into_ranged", path);
        match inner.into_ranged(path) {
            Ok(ranged) => Ok(CountingRanged::wrap(ranged, &counts, alive)),
            Err(refused) => Err(crate::remote::Refused {
                conn: Box::new(Counting::carried(refused.conn, &counts, alive)),
                error: refused.error,
            }),
        }
    }
}

/// A stream's transport that was a counted connection: still one live session, its reads
/// logged ([`Counts::ranged`]) and their bytes counted.
struct CountingRanged {
    inner: Option<Box<dyn crate::remote::Ranged>>,
    counts: Arc<Counts>,
    alive: Option<Alive>,
}

impl CountingRanged {
    fn wrap(
        inner: Box<dyn crate::remote::Ranged>,
        counts: &Arc<Counts>,
        alive: Alive,
    ) -> Box<dyn crate::remote::Ranged> {
        Box::new(Self {
            inner: Some(inner),
            counts: counts.clone(),
            alive: Some(alive),
        })
    }

    fn inner(&self) -> &dyn crate::remote::Ranged {
        self.inner.as_ref().expect("a live transport").as_ref()
    }

    fn inner_mut(&mut self) -> &mut Box<dyn crate::remote::Ranged> {
        self.inner.as_mut().expect("a live transport")
    }
}

impl crate::remote::Ranged for CountingRanged {
    fn path(&self) -> &str {
        self.inner().path()
    }
    fn opened(&self) -> Stat {
        self.inner().opened()
    }
    fn read(&mut self, off: u64, len: u32) -> crate::remote::Ticket {
        let path = self.path().to_string();
        self.counts.ranged.lock().unwrap().push((path, off, len));
        self.inner_mut().read(off, len)
    }
    fn fstat(&mut self) -> crate::remote::Ticket {
        self.inner_mut().fstat()
    }
    fn stat_path(&mut self) -> crate::remote::Ticket {
        self.inner_mut().stat_path()
    }
    fn pending(&self) -> usize {
        self.inner().pending()
    }
    fn next(&mut self, timeout: Option<Duration>) -> Option<crate::remote::Completion> {
        let c = self.inner_mut().next(timeout)?;
        let n = c.data().map_or(0, |b| b.len() as u64);
        self.counts.streamed.fetch_add(n, Ordering::Relaxed);
        // The gate, at the first reply that carries bytes (a stream's read held, as a whole
        // read's first response is).
        if n > 0 {
            let gate = self.counts.gate.lock().unwrap().take();
            if let Some(gate) = gate {
                gate();
            }
        }
        Some(c)
    }
    fn waker(&self) -> crate::remote::Wake {
        self.inner().waker()
    }
    fn now(&self) -> std::time::Instant {
        self.inner().now()
    }
    fn reopen(
        mut self: Box<Self>,
        path: &str,
    ) -> std::result::Result<Box<dyn crate::remote::Ranged>, crate::remote::Refused> {
        let counts = self.counts.clone();
        let inner = self.inner.take().expect("a live transport");
        let alive = self.alive.take().expect("a live transport");
        drop(self);
        counts.op("reopen", path);
        match inner.reopen(path) {
            Ok(ranged) => Ok(CountingRanged::wrap(ranged, &counts, alive)),
            Err(refused) => Err(crate::remote::Refused {
                conn: Box::new(Counting::carried(refused.conn, &counts, alive)),
                error: refused.error,
            }),
        }
    }
    /// The session given back: after the shutdown's delay, alive throughout.
    fn into_connection(mut self: Box<Self>) -> Box<dyn RemoteFs + Send> {
        let (counts, path) = (self.counts.clone(), self.path().to_string());
        let delay = *counts.return_delay.lock().unwrap();
        if !delay.is_zero() {
            std::thread::sleep(delay);
        }
        // The same session, counted once throughout.
        let inner = self.inner.take().expect("a live transport");
        let alive = self.alive.take().expect("a live transport");
        drop(self);
        counts.op("into_connection", &path);
        Box::new(Counting::carried(inner.into_connection(), &counts, alive))
    }
    fn close(mut self: Box<Self>) {
        let path = self.path().to_string();
        self.counts.op("close", &path);
        if let Some(inner) = self.inner.take() {
            inner.close();
        }
    }
}

/// A folder of files on "host `box`" (this machine's SFTP server), a cache, and a `Connect` that
/// counts the sessions it opens and what is read over them.
struct Fx {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    cache: PathBuf,
    connect: Connect,
    sessions: Arc<AtomicUsize>,
    counts: Arc<Counts>,
    /// The window's preview connection (opened by the first remote preview).
    conn: Option<Box<dyn RemoteFs + Send>>,
    label: Option<&'static str>,
    /// What a stream opened here wakes: nothing (the test kit's passes look by themselves).
    wake: Waker,
    /// Where the preview connection comes back from while a preview's stream has it.
    back: Back,
}

/// A session to "host `box`" (this machine's SFTP server), or the local disk.
fn inner_for(label: Option<&str>) -> Box<dyn RemoteFs + Send> {
    match label {
        Some(_) => Box::new(SftpRemote::local_server().expect("sftp-server")),
        None => Box::new(crate::remote::LocalFs::new()),
    }
}

/// `None` (the test skips) where OpenSSH's `sftp-server` is not installed — a failure instead
/// with `SOPEN_REQUIRE_SFTP_SERVER` (CI).
fn remote() -> Option<Fx> {
    SftpRemote::local_server()?;
    Some(fixture(Some("box")))
}

fn local() -> Fx {
    fixture(None)
}

fn fixture(label: Option<&'static str>) -> Fx {
    let tmp = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(tmp.path()).unwrap();
    let root = base.join("files");
    std::fs::create_dir(&root).unwrap();
    let cache = base.join("cache");
    let sessions = Arc::new(AtomicUsize::new(0));
    let counts = Arc::new(Counts::default());
    let (n, c) = (sessions.clone(), counts.clone());
    let connect: Connect = Arc::new(move || {
        n.fetch_add(1, Ordering::SeqCst);
        c.op("connect", "");
        Ok(Box::new(Counting::new(inner_for(label), &c)) as Box<dyn RemoteFs + Send>)
    });
    Fx {
        _tmp: tmp,
        root,
        cache,
        connect,
        sessions,
        counts,
        conn: None,
        label,
        wake: Arc::new(|| {}),
        back: Back::default(),
    }
}

impl Fx {
    fn file(&self, name: &str, bytes: &[u8]) -> String {
        let path = self.root.join(name);
        std::fs::write(&path, bytes).unwrap();
        path.to_str().unwrap().to_string()
    }

    fn preview_with(&mut self, abs: &str, budget: &Budget, cancel: &AtomicBool) -> Result<Preview> {
        let req = PreviewRequest {
            conn: &mut self.conn,
            connect: &self.connect,
            label: self.label,
            abs,
            cache: &self.cache,
            back: &self.back,
            wake: &self.wake,
            began: std::time::Instant::now(),
        };
        preview(req, budget, cancel, &mut |_, _| {}).map(Previewed::show)
    }

    fn preview_limited(&mut self, abs: &str, limits: Limits) -> Preview {
        let budget = Budget::new(limits);
        self.preview_with(abs, &budget, &AtomicBool::new(false))
            .expect("not cancelled")
    }

    fn preview(&mut self, abs: &str) -> Preview {
        self.preview_limited(abs, Limits::PREVIEW)
    }

    /// [`Fx::preview_limited`] on a clock that never moves: no step runs over its time, however
    /// loaded the machine (a test about bytes, not about the budget's times).
    fn preview_unhurried(&mut self, abs: &str, limits: Limits) -> Preview {
        let at = std::time::Instant::now();
        let budget = Budget::with_clock(limits, Box::new(move || at));
        self.preview_with(abs, &budget, &AtomicBool::new(false))
            .expect("not cancelled")
    }

    fn request<'a>(&'a self, abs: &'a str) -> Request<'a> {
        Request {
            connect: &self.connect,
            label: self.label,
            abs,
            cache: &self.cache,
            wake: &self.wake,
            began: std::time::Instant::now(),
        }
    }

    /// Every file under the cache, with its content: what a preview must not change (it writes
    /// nothing to disk).
    fn cache_files(&self) -> BTreeMap<PathBuf, Vec<u8>> {
        fn walk(dir: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
            let Ok(entries) = std::fs::read_dir(dir) else {
                return;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk(&path, out);
                } else {
                    out.insert(path.clone(), std::fs::read(&path).unwrap_or_default());
                }
            }
        }
        let mut out = BTreeMap::new();
        walk(&self.cache, &mut out);
        out
    }
}

/// A window around an [`Embedded`]'s (what a preview or a hand-over gives), for the test kit.
struct Host<'a>(&'a mut Box<dyn Window>);

impl Window for Host<'_> {
    fn ui(&mut self, ui: &mut egui::Ui) {
        self.0.ui(ui)
    }
    fn ready(&self) -> bool {
        self.0.ready()
    }
    fn close_requested(&mut self) -> bool {
        self.0.close_requested()
    }
}

/// Passes until the window is ready, then two more; what it drew is then in `drv`.
fn shown(drv: &mut Driver, win: &mut Host<'_>) {
    drv.until(win, 10, |w| w.ready());
    drv.idle(win, 2);
}

fn card_why(p: &Preview) -> String {
    p.card().map(|c| c.why.clone()).unwrap_or_default()
}

fn png(w: u32, h: u32) -> Vec<u8> {
    let img = ::image::DynamicImage::ImageRgb8(::image::RgbImage::from_fn(w, h, |x, y| {
        ::image::Rgb([x as u8, y as u8, 90])
    }));
    crate::gui::image::encode(&img, ::image::ImageFormat::Png, None, None)
        .unwrap()
        .bytes
}

/// A PNG that says it is `w`×`h` and holds a single empty pixel row: its header is real, its
/// image data is not (no decoder gets that far in a preview).
fn png_header(w: u32, h: u32) -> Vec<u8> {
    fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        let mut crc_input = kind.to_vec();
        crc_input.extend_from_slice(data);
        out.extend_from_slice(&crc_input);
        let mut crc = 0xFFFF_FFFFu32;
        for &b in &crc_input {
            crc ^= u32::from(b);
            for _ in 0..8 {
                crc = if crc & 1 != 0 {
                    0xEDB8_8320 ^ (crc >> 1)
                } else {
                    crc >> 1
                };
            }
        }
        out.extend_from_slice(&(!crc).to_be_bytes());
    }
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&w.to_be_bytes());
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(
        &mut out,
        b"IDAT",
        &[0x78, 0x9c, 0x03, 0x00, 0x00, 0x00, 0x00, 0x01],
    );
    chunk(&mut out, b"IEND", &[]);
    out
}

/// A preview is prepared on a worker and handed to the window: what the worker makes crosses
/// threads.
#[test]
fn a_prepared_preview_crosses_to_the_window() {
    fn crosses<T: Send + 'static>() {}
    crosses::<Previewed>();
    crosses::<Arc<Budget>>();
    crosses::<Prepared>();
}

/// A remote preview never touches the cache: an image reads through a stream on the preview
/// connection — also with an earlier session's copy of the same file in the cache, holding an
/// edit it never saved — and writes nothing to disk; nor does the hand-over (a stream claims
/// nothing: that copy and its edit stay as they are, and nothing is said of them).
#[test]
fn a_remote_preview_reads_without_claiming_the_cache() {
    let Some(mut fx) = remote() else { return };
    let bytes = png(64, 48);
    let abs = fx.file("photo.png", &bytes);
    // An earlier session's copy: recorded, then edited and never saved.
    crate::cache::seed_copy(&fx.cache, "box", &abs, b"fetched", b"an unsaved edit");
    let before = fx.cache_files();
    let mut p = fx.preview(&abs);
    assert!(p.card().is_none(), "{}", card_why(&p));
    assert_eq!(fx.cache_files(), before, "the cache is as it was");
    let mut drv = Driver::new(500.0, 300.0);
    shown(&mut drv, &mut Host(&mut p.embedded.window));
    assert_eq!(drv.pictures().len(), 1, "the picture is drawn");
    drop(p);
    assert_eq!(fx.cache_files(), before);
    let handed = fx.preview(&abs).hand_over().ok().expect("a viewer");
    assert!(handed.hold.is_none(), "a stream claims nothing");
    assert!(handed.notices.is_empty(), "{:?}", handed.notices);
    assert_eq!(
        fx.cache_files(),
        before,
        "nothing claimed, nothing moved aside"
    );
    assert_eq!(whole_reads(&fx), Vec::<String>::new());
}

/// A streamed remote preview (text, 06 §13) writes nothing at all: nothing anywhere in
/// the cache — an earlier session's copy there, holding an edit it never saved, stays exactly
/// as it is, through the hand-over too (a stream claims nothing).
#[test]
fn w8a_a_streamed_preview_writes_nothing_to_disk() {
    let Some(mut fx) = remote() else { return };
    let abs = fx.file("notes.txt", b"what the remote holds\n");
    crate::cache::seed_copy(&fx.cache, "box", &abs, b"fetched", b"an unsaved edit");
    let before = fx.cache_files();
    let mut p = fx.preview(&abs);
    assert!(p.card().is_none(), "{}", card_why(&p));
    let mut drv = Driver::new(500.0, 300.0);
    shown(&mut drv, &mut Host(&mut p.embedded.window));
    assert!(drv.find_text("what the remote holds").is_some());
    assert_eq!(fx.cache_files(), before, "the cache is as it was");
    assert_eq!(whole_reads(&fx), Vec::<String>::new());
    let mut handed = p.hand_over().ok().expect("a viewer");
    assert!(handed.hold.is_none(), "a stream claims nothing");
    assert!(handed.notices.is_empty(), "{:?}", handed.notices);
    shown(&mut drv, &mut Host(&mut handed.embedded.window));
    assert!(drv.find_text("what the remote holds").is_some());
    assert_eq!(fx.cache_files(), before, "nothing is published");
}

/// P3: a remote file over the read budget that is read whole to be previewed (an image) shows
/// the info card at once and nothing of it is read — not even the sniff's head (its name says
/// its kind, and the gate is the stat).
#[test]
fn a_remote_file_over_the_read_budget_is_a_card_with_nothing_read() {
    let Some(mut fx) = remote() else { return };
    let abs = fx.file("big.png", &vec![7u8; 200 << 10]);
    let limits = Limits {
        read: 100 << 10,
        ..Limits::PREVIEW
    };
    let p = fx.preview_limited(&abs, limits);
    assert_eq!(card_why(&p), "over the preview limit: Enter opens it");
    assert_eq!(fx.counts.content.load(Ordering::Relaxed), 0, "nothing read");
    assert_eq!(
        fx.counts.heads.load(Ordering::Relaxed),
        0,
        "not even a head"
    );
    assert!(fx.counts.ranged.lock().unwrap().is_empty());
}

/// 02 §4.3 with streaming: the read budget no longer gates a file by its size when it is
/// previewed through a stream. A remote binary far over it — the owner's 24 GB, sparse —
/// previews its first rows: the sniff (its name does not say its kind), then one block.
#[test]
fn w8a_a_large_remote_binary_previews_its_first_rows() {
    const SIZE: u64 = 24_000_000_000;
    let Some(mut fx) = remote() else { return };
    let abs = fx.file("model.safetensors", b"");
    std::fs::File::options()
        .write(true)
        .open(&abs)
        .unwrap()
        .set_len(SIZE)
        .unwrap();
    let mut p = fx.preview(&abs);
    assert!(p.card().is_none(), "{}", card_why(&p));
    let mut drv = Driver::new(600.0, 400.0);
    shown(&mut drv, &mut Host(&mut p.embedded.window));
    // (A preview's rows are as wide as its side takes: here eight bytes.)
    assert!(
        drv.text_starting_with("00 00 00 00 00 00 00 00").is_some(),
        "its first rows"
    );
    let block = crate::stream::BLOCK;
    assert_eq!(
        *fx.counts.ranged.lock().unwrap(),
        vec![(abs.clone(), 0, block as u32)],
        "one block"
    );
    assert!(fx.counts.heads.load(Ordering::Relaxed) <= 8 << 10);
    assert_eq!(whole_reads(&fx), Vec::<String>::new());
    assert!(fx.cache_files().is_empty());
    // Remote text over 16 MiB streams too (W5b): past the read budget it previews, its name
    // saying its kind — nothing sniffed, nothing read whole.
    let log = fx.file("huge.log", b"");
    grow(&log, 40 << 20);
    drop(p);
    let heads = fx.counts.heads.load(Ordering::Relaxed);
    let p = fx.preview(&log);
    assert!(p.card().is_none(), "{}", card_why(&p));
    assert_eq!(fx.counts.heads.load(Ordering::Relaxed), heads);
    assert_eq!(whole_reads(&fx), Vec::<String>::new());
    assert_eq!(fx.sessions.load(Ordering::SeqCst), 1, "one preview session");
}

/// P3 through a stream: the handle's size decides too. An image that grew past the read budget
/// between the stat that decided it fits and its open is the card, nothing of it read through
/// the stream; with Enter meanwhile (the budget lifted) the whole larger file is read, every
/// range once, and shows.
#[test]
fn an_image_grown_past_the_read_budget_before_its_open_is_its_card() {
    let Some(mut fx) = remote() else { return };
    let small = png(64, 48);
    let big = noisy_png(300, 200);
    let limits = Limits {
        read: small.len() as u64 + 100,
        ..Limits::PREVIEW
    };
    assert!(big.len() as u64 > 2 * limits.read);
    for lift in [false, true] {
        let abs = fx.file(&format!("grows-{lift}.png"), &small);
        let budget = Budget::new(limits);
        let (path, bigger, enter) = (abs.clone(), big.clone(), budget.clone());
        let mut armed = true;
        *fx.counts.after.lock().unwrap() = Some(Box::new(move |op, p| {
            // After the stat that decided it fits.
            if op == "stat" && p == path && std::mem::take(&mut armed) {
                std::fs::write(&path, &bigger).unwrap();
                if lift {
                    enter.lift();
                }
            }
        }));
        let p = fx
            .preview_with(&abs, &budget, &AtomicBool::new(false))
            .unwrap();
        let reads = stream_reads(&fx, &abs);
        if lift {
            assert!(p.card().is_none(), "{}", card_why(&p));
            assert!(assert_read_once(&reads) >= big.len() as u64, "{reads:?}");
        } else {
            assert_eq!(card_why(&p), OVER_LIMIT);
            assert!(reads.is_empty(), "nothing of it read: {reads:?}");
        }
    }
    assert_eq!(whole_reads(&fx), Vec::<String>::new());
}

/// The most one SFTP read response carries: what the whole-file reader asks per request
/// (`remote::ranged::PIPE_REQUEST`, 06 §2), which OpenSSH's `sftp-server` answers in full.
const SFTP_RESPONSE: u64 = crate::remote::PIPE_REQUEST as u64;

/// A file whose bytes say where they are: 16-byte records `@<offset, 14 hex digits>\n`, so any
/// run of its bytes holding one whole record tells its own offset (it is text, too).
fn stamped(len: u64) -> Vec<u8> {
    let mut out = Vec::with_capacity(len as usize + 16);
    let mut at = 0u64;
    while (out.len() as u64) < len {
        out.extend_from_slice(format!("@{at:014x}\n").as_bytes());
        at += 16;
    }
    out.truncate(len as usize);
    out
}

/// Where in a [`stamped`] file the bytes `buf` are, from the bytes themselves — `None` when they
/// are not a run of one (or hold no whole record).
fn source_of(buf: &[u8]) -> Option<u64> {
    let first = (0..buf.len().min(16)).find(|&i| {
        buf.get(i) == Some(&b'@')
            && buf.get(i + 15) == Some(&b'\n')
            && buf[i + 1..i + 15].iter().all(u8::is_ascii_hexdigit)
    })?;
    let record = std::str::from_utf8(&buf[first + 1..first + 15]).ok()?;
    let start = u64::from_str_radix(record, 16)
        .ok()?
        .checked_sub(first as u64)?;
    // The whole response must be that run of the file, byte for byte.
    let whole = stamped(start + buf.len() as u64);
    (whole[start as usize..] == *buf).then_some(start)
}

/// The responses' source ranges cover the file from its start, each once, in order: no range
/// sent twice, none skipped. `Err` names the first that is not.
fn ranges_once(responses: &[(Option<u64>, u64)]) -> std::result::Result<(), String> {
    let mut at = 0;
    for (i, &(source, n)) in responses.iter().enumerate() {
        match source {
            Some(source) if source == at => at += n,
            Some(source) => return Err(format!("response {i} holds {source}.., expected {at}..")),
            None => return Err(format!("response {i} is not a run of the file")),
        }
    }
    Ok(())
}

/// The range oracle reads the ranges from the bytes, not from their count: a response sent twice,
/// or one skipped, is seen though the lengths add up the same.
#[test]
fn the_range_oracle_sees_a_range_sent_twice_or_skipped() {
    let file = stamped(1000);
    let resp = |from: usize, to: usize| (source_of(&file[from..to]), (to - from) as u64);
    assert_eq!(source_of(&file[37..300]), Some(37));
    assert_eq!(source_of(b"not stamped at all, not at all"), None);
    assert!(ranges_once(&[resp(0, 300), resp(300, 700), resp(700, 1000)]).is_ok());
    let twice = [resp(0, 300), resp(0, 300), resp(600, 1000)];
    assert!(ranges_once(&twice).is_err(), "sent twice");
    let skipped = [resp(0, 300), resp(400, 700), resp(700, 1000)];
    assert!(ranges_once(&skipped).is_err(), "skipped");
    let mut forged = file[300..700].to_vec();
    forged[200] = b'#';
    assert_eq!(source_of(&forged), None, "a byte not the file's");
}

/// P3 (local): an image whose header says 30000×30000 shows `image is 30000×30000; Enter opens
/// it` without being decoded (it could not be: its data is one empty row); an image over the
/// encoded budget, a preparation past its deadline, a video or a sound that is not one show
/// their cards. The card
/// names the file, its kind, size (exact and in units) and date; the list's status row is not
/// the card's to change.
#[test]
fn budgets_hit_show_the_info_card() {
    let mut fx = local();
    let header = png_header(30_000, 30_000);
    let huge = fx.file("huge.png", &header);
    let mut p = fx.preview(&huge);
    assert_eq!(card_why(&p), "image is 30000×30000; Enter opens it");
    let mut drv = Driver::new(500.0, 400.0);
    shown(&mut drv, &mut Host(&mut p.embedded.window));
    let size = header.len() as u64;
    let size = format!(
        "{} bytes ({})",
        crate::gui::hex::grouped(size),
        crate::human(size)
    );
    for text in [
        "huge.png",
        "image",
        "image is 30000×30000; Enter opens it",
        &size,
    ] {
        assert!(drv.find_text(text).is_some(), "{text:?} not on the card");
    }

    let photo = fx.file("photo.png", &png(64, 48));
    let small_image = Limits {
        image_bytes: 100,
        ..Limits::PREVIEW
    };
    assert_eq!(
        card_why(&fx.preview_limited(&photo, small_image)),
        "over the preview limit: Enter opens it"
    );
    let few_pixels = Limits {
        pixels: 1000,
        ..Limits::PREVIEW
    };
    assert_eq!(
        card_why(&fx.preview_limited(&photo, few_pixels)),
        "image is 64×48; Enter opens it"
    );
    let text = fx.file("notes.txt", b"one\ntwo\n");
    let no_time = Limits {
        time: Duration::ZERO,
        ..Limits::PREVIEW
    };
    for abs in [&text, &photo] {
        assert_eq!(
            card_why(&fx.preview_limited(abs, no_time)),
            "the preview took over 0 s: Enter opens it"
        );
    }
    // A video or a sound ffprobe cannot read is its card: the reason first, as ffprobe gives
    // it without the path, then what could not be done to which file — named once.
    if media_tools().is_some() {
        for (name, kind) in [("clip.mp4", "video"), ("song.mp3", "audio")] {
            let abs = fx.file(name, b"not really media");
            let p = fx.preview(&abs);
            assert_eq!(card_why(&p), "Invalid data found when processing input");
            let card = p.card().unwrap();
            assert_eq!(
                card.what.as_deref(),
                Some(format!("ffprobe cannot read {abs}").as_str())
            );
            assert_eq!(card.kind.map(Kind::as_str), Some(kind));
        }
    }
    let gone = fx.root.join("vanished.txt");
    let p = fx.preview(gone.to_str().unwrap());
    assert!(card_why(&p).contains("no such file"), "{}", card_why(&p));
}

/// A link is previewed as the file it points at, its card saying `<LINK>` and the target; a
/// broken link is a card saying `<BROKEN>`.
#[test]
fn links_preview_their_target_and_a_broken_one_is_a_card() {
    let mut fx = local();
    let target = fx.file("real.txt", b"through the link\n");
    let link = fx.root.join("link.txt");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    let mut p = fx.preview(link.to_str().unwrap());
    assert!(p.card().is_none(), "{}", card_why(&p));
    let mut drv = Driver::new(500.0, 300.0);
    shown(&mut drv, &mut Host(&mut p.embedded.window));
    assert!(drv.find_text("through the link").is_some());
    let broken = fx.root.join("broken.txt");
    std::os::unix::fs::symlink(fx.root.join("nowhere"), &broken).unwrap();
    let mut p = fx.preview(broken.to_str().unwrap());
    assert_eq!(p.card().expect("a card").link, Some(Link::Broken));
    shown(&mut drv, &mut Host(&mut p.embedded.window));
    assert!(drv.text_starting_with("<BROKEN>").is_some());
    let big = fx.root.join("big.mp3");
    std::fs::write(&big, b"x").unwrap();
    let to_big = fx.root.join("to-big.mp3");
    std::os::unix::fs::symlink(&big, &to_big).unwrap();
    let p = fx.preview(to_big.to_str().unwrap());
    assert_eq!(
        p.card().unwrap().link,
        Some(Link::To(big.to_str().unwrap().to_string()))
    );
}

/// P12 (local text): a 6 MiB file previews its first 4 MiB (said on its last row, the rest not
/// read); handed over, the viewer indexes the rest from 4 MiB — each byte read by the index once —
/// and End shows the last line. A local file is read in place: nothing in the cache, no claim.
#[test]
fn a_handed_over_local_text_reads_past_its_budget_once() {
    let mut fx = local();
    let body: String = (0..6 * 16_384)
        .map(|i| format!("{i:08} {}\n", "z".repeat(54)))
        .collect();
    assert_eq!(body.len(), 6 << 20);
    let abs = fx.file("six.log", body.as_bytes());
    // The 5 s deadline is not what this is about (its own tests move a clock): a loaded machine
    // indexing 4 MiB must not make it the card.
    let unhurried = Limits {
        time: Duration::from_secs(600),
        ..Limits::PREVIEW
    };
    let p = fx.preview_limited(&abs, unhurried);
    assert!(p.card().is_none(), "{}", card_why(&p));
    let path = PathBuf::from(&abs);
    assert_eq!(
        crate::gui::text::index_reads(&path)
            .iter()
            .map(|r| r.1)
            .sum::<u64>(),
        4 << 20,
        "the preview indexed 4 MiB"
    );
    let mut p = p;
    let mut drv = Driver::new(600.0, 400.0);
    shown(&mut drv, &mut Host(&mut p.embedded.window));
    assert!(drv.find_text("preview of the first 4 MiB").is_some());
    let mut handed = p.hand_over().ok().expect("a viewer");
    assert!(handed.hold.is_none(), "a local file is not claimed");
    let mut win = Host(&mut handed.embedded.window);
    drv.until(&mut win, 20, |_| {
        crate::gui::text::index_reads(&path)
            .iter()
            .map(|r| r.1)
            .sum::<u64>()
            >= 6 << 20
    });
    shown(&mut drv, &mut win);
    drv.key(&mut win, Key::End, command(false));
    drv.idle(&mut win, 2);
    assert!(drv
        .text_starting_with(&format!("{:08} ", 6 * 16_384 - 1))
        .is_some());
    let mut at = 0;
    for (offset, n) in crate::gui::text::index_reads(&path) {
        assert_eq!(offset, at, "read in order, nothing twice");
        at += n;
    }
    assert_eq!(at, 6 << 20);
    assert!(fx.cache_files().is_empty(), "{:?}", fx.cache_files().keys());
}

/// P6 / P12 (remote image): handed over, the viewer is the window the preview made — nothing
/// claimed, nothing in the cache, the file read once through the preview's stream (which gave
/// the session back as soon as it had read it) and decoded once — and the viewer's own session
/// opens on its first remote call (here its Save copy).
#[test]
fn a_handed_over_remote_image_reconnects_on_first_use() {
    let Some(mut fx) = remote() else { return };
    let bytes = png(64, 48);
    let abs = fx.file("photo.png", &bytes);
    let decoded = crate::gui::image::decodes();
    let p = fx.preview(&abs);
    assert!(p.card().is_none(), "{}", card_why(&p));
    assert_eq!(
        fx.sessions.load(Ordering::SeqCst),
        1,
        "the preview connection"
    );
    assert!(
        fx.conn.is_some(),
        "the stream gave the preview session back"
    );
    let mut handed = p.hand_over().ok().expect("a viewer");
    assert!(handed.hold.is_none(), "a stream claims nothing");
    assert!(handed.notices.is_empty(), "{:?}", handed.notices);
    assert!(fx.cache_files().is_empty(), "{:?}", fx.cache_files().keys());
    let mut drv = Driver::new(800.0, 600.0);
    let mut win = Host(&mut handed.embedded.window);
    shown(&mut drv, &mut win);
    assert!(
        drv.find_text("Save copy").is_some(),
        "the image window's toolbar"
    );
    drv.key(&mut win, Key::S, command(true));
    let sibling = fx.root.join("photo.edited.png");
    drv.until(&mut win, 20, |_| sibling.exists());
    assert_eq!(
        fx.sessions.load(Ordering::SeqCst),
        2,
        "its own session, on first use"
    );
    let crate::gui::Embedded { window, mut settle } = handed.embedded;
    drop(window);
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    while settle(true).is_none() {
        assert!(
            std::time::Instant::now() < deadline,
            "the viewer never settled"
        );
    }
    assert_eq!(
        crate::gui::image::decodes(),
        decoded + 1,
        "decoded once: for the preview, never again"
    );
    assert_eq!(
        fx.counts.streamed.load(Ordering::Relaxed),
        bytes.len() as u64,
        "read once"
    );
    assert_eq!(whole_reads(&fx), Vec::<String>::new());
}

/// P4: twenty quick moves over images — each preview cancelled at once, mid-read, or after it
/// is ready and dropped — use the window's one preview connection (each stream gives it back),
/// never two at once, and leave nothing on disk.
#[test]
fn twenty_quick_moves_use_one_connection_and_leave_nothing() {
    let Some(mut fx) = remote() else { return };
    // Images: read whole through their stream (text and binaries read what they show: their
    // twenty moves are `w8a_twenty_quick_moves_over_streams_use_one_session`).
    let picture = noisy_png(320, 300);
    assert!(
        picture.len() as u64 > 2 * SFTP_RESPONSE,
        "several responses"
    );
    let names: Vec<String> = (0..20)
        .map(|i| fx.file(&format!("f{i:02}.png"), &picture))
        .collect();
    for (i, abs) in names.iter().enumerate() {
        let cancel = AtomicBool::new(i % 3 == 0);
        let budget = Budget::preview();
        let req = PreviewRequest {
            conn: &mut fx.conn,
            connect: &fx.connect,
            label: fx.label,
            abs,
            cache: &fx.cache,
            back: &fx.back,
            wake: &fx.wake,
            began: std::time::Instant::now(),
        };
        let result = preview(req, &budget, &cancel, &mut |_, _| {
            if i % 3 == 1 {
                cancel.store(true, Ordering::Relaxed);
            }
        });
        match i % 3 {
            0 | 1 => assert!(
                result.as_ref().is_err_and(crate::remote::is_cancelled),
                "move {i}: {:?}",
                result.as_ref().err()
            ),
            _ => assert!(result.is_ok_and(|p| p.card().is_none())),
        }
    }
    assert_eq!(
        fx.sessions.load(Ordering::SeqCst),
        1,
        "one preview connection"
    );
    assert_eq!(
        fx.counts.peak.load(Ordering::SeqCst),
        1,
        "never two at once"
    );
    assert_eq!(whole_reads(&fx), Vec::<String>::new());
    assert!(fx.cache_files().is_empty(), "{:?}", fx.cache_files().keys());
}

/// The window's preview connection is opened again when its session died, once.
#[test]
fn a_dead_preview_connection_is_opened_again() {
    let Some(mut fx) = remote() else { return };
    let abs = fx.file("a.txt", b"alive\n");
    fx.conn = Some(Box::new(Placeholder {
        can_replace: true,
        can_link: true,
    }));
    let p = fx.preview(&abs);
    assert!(p.card().is_none(), "{}", card_why(&p));
    assert_eq!(fx.sessions.load(Ordering::SeqCst), 1);
}

/// The placeholder a previewed viewer's `Origin` holds fails every call, so the first remote
/// call reconnects (`Origin::fs`) and goes through the new session; it says what the preview
/// connection's server can do meanwhile (the image window reads it when it opens).
#[test]
fn the_placeholder_connection_reconnects_on_first_use() {
    let Some(fx) = remote() else { return };
    let abs = fx.file("a.txt", b"x");
    let conn = (fx.connect)().unwrap();
    let connect = fx.connect.clone();
    let mut origin = Origin::placeholder(
        Some(Box::new(move || connect()) as Reconnect),
        "box".to_string(),
        abs.clone(),
        Stat {
            is_dir: false,
            is_file: true,
            is_symlink: false,
            size: 1,
            mtime: std::time::SystemTime::UNIX_EPOCH,
            mode: 0o644,
            uid: None,
            gid: None,
            missing: None,
            sha256: None,
        },
        Placeholder {
            can_replace: conn.can_replace(),
            can_link: conn.can_link(),
        },
    );
    let opened = fx.sessions.load(Ordering::SeqCst);
    assert!(origin.can_replace(), "as the preview connection's server");
    let stat = origin.fs().unwrap().stat(&abs).unwrap();
    assert_eq!(stat.size, 1);
    assert_eq!(
        fx.sessions.load(Ordering::SeqCst),
        opened + 1,
        "reconnected once"
    );
    origin.fs().unwrap().stat(&abs).unwrap();
    assert_eq!(fx.sessions.load(Ordering::SeqCst), opened + 1, "and kept");
}

/// Lifted (Enter while the preview is still being prepared, 02 §4.4), the preparation goes on
/// under the viewer's rules: an image over the preview's pixel budget, Enter coming as its
/// stream opens, is read and decoded all the same — the file read once, every range once — and
/// there is no card. Without Enter the same file is the card.
#[test]
fn a_lifted_budget_lets_the_preparation_go_on() {
    let Some(mut fx) = remote() else { return };
    let bytes = noisy_png(300, 200);
    let few_pixels = Limits {
        pixels: 1000,
        ..Limits::PREVIEW
    };
    let abs = fx.file("held.png", &bytes);
    assert_eq!(
        card_why(&fx.preview_limited(&abs, few_pixels)),
        "image is 300×200; Enter opens it"
    );
    let abs = fx.file("lifted.png", &bytes);
    let budget = Budget::new(few_pixels);
    let (lift, path) = (budget.clone(), abs.clone());
    *fx.counts.after.lock().unwrap() = Some(Box::new(move |op, p| {
        if op == "into_ranged" && p == path {
            lift.lift();
        }
    }));
    let p = fx
        .preview_with(&abs, &budget, &AtomicBool::new(false))
        .unwrap();
    assert!(p.card().is_none(), "{}", card_why(&p));
    let reads = stream_reads(&fx, &abs);
    assert!(assert_read_once(&reads) >= bytes.len() as u64, "{reads:?}");
    let mut handed = p.hand_over().ok().unwrap();
    let mut drv = Driver::new(600.0, 400.0);
    shown(&mut drv, &mut Host(&mut handed.embedded.window));
    assert_eq!(drv.pictures().len(), 1, "the picture");
    assert_eq!(whole_reads(&fx), Vec::<String>::new());
}

/// The byte ranges streams asked of `path`, in order, as `(offset, length)`.
fn stream_reads(fx: &Fx, path: &str) -> Vec<(u64, u64)> {
    let all = fx.counts.ranged.lock().unwrap();
    all.iter()
        .filter(|(p, _, _)| p == path)
        .map(|&(_, off, len)| (off, u64::from(len)))
        .collect()
}

/// No byte of the file was asked for twice: the ranges do not overlap. Their sum.
fn assert_read_once(reads: &[(u64, u64)]) -> u64 {
    let mut sorted = reads.to_vec();
    sorted.sort_unstable();
    let mut end = 0;
    for &(off, len) in &sorted {
        assert!(off >= end, "{off:#x} was read twice: {sorted:?}");
        end = off + len;
    }
    sorted.iter().map(|r| r.1).sum()
}

/// P12 (remote text, 06 §13): a 6 MiB remote file previews through a stream on the window's
/// preview session — its index pass stops at the preview's 4 MiB, and no more is fetched; the
/// last row says so. Handed over, the stream is the viewer's, with what it has: the index goes
/// on from 4 MiB to the end, End shows the last line, and over preview and viewer together no
/// byte range was read twice — the file crossed the connection once, on one session, and
/// nothing was written to disk.
#[test]
fn a_handed_over_remote_text_goes_on_through_its_stream() {
    let Some(mut fx) = remote() else { return };
    let body: String = (0..6 * 16_384)
        .map(|i| format!("{i:08} {}\n", "r".repeat(54)))
        .collect();
    let size = body.len() as u64;
    assert_eq!(size, 6 << 20);
    let abs = fx.file("six.log", body.as_bytes());
    let mut p = fx.preview(&abs);
    assert!(p.card().is_none(), "{}", card_why(&p));
    let mut drv = Driver::new(600.0, 400.0);
    let streamed = |fx: &Fx| fx.counts.streamed.load(Ordering::Relaxed);
    {
        let mut win = Host(&mut p.embedded.window);
        drv.until(&mut win, 30, |_| streamed(&fx) >= 4 << 20);
        shown(&mut drv, &mut win);
        drv.idle(&mut win, 10);
    }
    assert!(drv.find_text("00000000 rrrr").is_none(), "wrapped rows");
    assert!(drv.text_starting_with("00000000 ").is_some());
    assert!(drv.find_text("preview of the first 4 MiB").is_some());
    assert_eq!(
        streamed(&fx),
        4 << 20,
        "the preview fetched its 4 MiB budget and no more"
    );
    assert_eq!(assert_read_once(&stream_reads(&fx, &abs)), 4 << 20);
    let mut handed = p.hand_over().ok().expect("a viewer");
    assert!(
        handed.hold.is_none(),
        "a stream claims nothing in the cache"
    );
    let mut win = Host(&mut handed.embedded.window);
    drv.until(&mut win, 30, |_| streamed(&fx) >= size);
    shown(&mut drv, &mut win);
    drv.key(&mut win, Key::End, command(false));
    drv.until(&mut win, 20, |w| w.ready());
    drv.idle(&mut win, 2);
    assert!(drv
        .text_starting_with(&format!("{:08} ", 6 * 16_384 - 1))
        .is_some());
    assert_eq!(
        assert_read_once(&stream_reads(&fx, &abs)),
        size,
        "every byte asked for once, preview and viewer together"
    );
    assert_eq!(streamed(&fx), size, "the file crossed the connection once");
    assert_eq!(whole_reads(&fx), Vec::<String>::new());
    assert_eq!(fx.sessions.load(Ordering::SeqCst), 1, "one session");
    assert_eq!(fx.counts.peak.load(Ordering::SeqCst), 1);
    assert!(fx.cache_files().is_empty(), "{:?}", fx.cache_files().keys());
}

/// P6 (remote, 06 §13): Enter on a previewed remote binary moves the preview's stream to the
/// viewer. The block the preview read is not read again; the session is the viewer's now — its
/// `Origin` connects only on its first use — so the window's next remote preview opens a new
/// preview session, and closing the viewer gives nothing back.
#[test]
fn w8a_a_handed_over_stream_takes_its_session_and_reads_nothing_twice() {
    let Some(mut fx) = remote() else { return };
    let abs = fx.file("blob.bin", &crate::remote::pattern(0, 1 << 20));
    let other = fx.file("other.bin", &crate::remote::pattern(7, 1 << 20));
    let mut p = fx.preview(&abs);
    assert!(p.card().is_none(), "{}", card_why(&p));
    let mut drv = Driver::new(700.0, 500.0);
    shown(&mut drv, &mut Host(&mut p.embedded.window));
    let block = crate::stream::BLOCK;
    assert_eq!(stream_reads(&fx, &abs), vec![(0, block)]);
    let mut handed = p.hand_over().ok().expect("a viewer");
    assert!(handed.hold.is_none() && handed.notices.is_empty());
    {
        let mut win = Host(&mut handed.embedded.window);
        shown(&mut drv, &mut win);
        // A viewer now: its keys act. Three pages down stay inside the block it has.
        for _ in 0..3 {
            drv.key(&mut win, Key::PageDown, egui::Modifiers::NONE);
        }
        drv.until(&mut win, 20, |w| w.ready());
    }
    assert_read_once(&stream_reads(&fx, &abs));
    assert_eq!(
        stream_reads(&fx, &abs).first(),
        Some(&(0, block)),
        "the preview's block is the viewer's"
    );
    assert_eq!(
        fx.sessions.load(Ordering::SeqCst),
        1,
        "the Origin has not connected"
    );
    assert!(fx.conn.is_none(), "the session went with the stream");
    // The next preview: a new preview session, beside the viewer's.
    let p2 = fx.preview(&other);
    assert!(p2.card().is_none(), "{}", card_why(&p2));
    assert_eq!(
        fx.sessions.load(Ordering::SeqCst),
        2,
        "a new preview session"
    );
    assert_eq!(fx.counts.live.load(Ordering::SeqCst), 2);
    drop(p2);
    drop(handed);
    // By transport: the handed-over viewer's was closed for good, never given back to the
    // window; the dropped preview's was given back. (Each ends on its stream's own thread.)
    let did_in = |ops: &[String], what: &str, path: &str| ops.contains(&format!("{what} {path}"));
    wait_for("both streams to end", || {
        let ops = fx.counts.ops();
        did_in(&ops, "close", &abs) && did_in(&ops, "into_connection", &other)
    });
    wait_for("one session left", || {
        fx.counts.live.load(Ordering::SeqCst) == 1
    });
    let ops = fx.counts.ops();
    let did = |what: &str, path: &str| did_in(&ops, what, path);
    assert!(
        did("close", &abs),
        "the viewer's transport is closed: {ops:?}"
    );
    assert!(!did("into_connection", &abs), "and not returned: {ops:?}");
    assert!(did("into_connection", &other), "the preview's is: {ops:?}");
    assert!(!did("close", &other), "{ops:?}");
    assert!(fx.cache_files().is_empty());
}

/// 06 §13, the one exception to "one preview session": a dropped preview's stream whose
/// shutdown takes longer than the next preparation waits (2 s). That preparation then opens a
/// session of its own, so for that moment there are two — never more — and the late one is
/// closed when it comes, not kept: the window is back to one, and the preview after it uses
/// that one.
#[test]
fn w8a_a_shutdown_slower_than_the_wait_makes_two_sessions_and_the_late_one_is_closed() {
    let Some(mut fx) = remote() else { return };
    let a = fx.file("a.txt", b"the first\n");
    let b = fx.file("b.txt", b"the second\n");
    let mut drv = Driver::new(500.0, 300.0);
    let mut p = fx.preview(&a);
    shown(&mut drv, &mut Host(&mut p.embedded.window));
    *fx.counts.return_delay.lock().unwrap() = BACK_WAIT + Duration::from_secs(1);
    drop(p);
    let waited = std::time::Instant::now();
    let mut p = fx.preview(&b);
    assert!(waited.elapsed() >= BACK_WAIT, "it waited for the session");
    assert!(p.card().is_none(), "{}", card_why(&p));
    shown(&mut drv, &mut Host(&mut p.embedded.window));
    assert!(drv.find_text("the second").is_some());
    assert_eq!(
        fx.sessions.load(Ordering::SeqCst),
        2,
        "a session of its own"
    );
    assert_eq!(
        fx.counts.peak.load(Ordering::SeqCst),
        2,
        "two, for a moment"
    );
    wait_for("the late session to be closed", || {
        fx.counts.live.load(Ordering::SeqCst) == 1
    });
    assert!(fx.counts.ops().contains(&format!("into_connection {a}")));
    // Back to one: the next preview takes the session this one gives back.
    *fx.counts.return_delay.lock().unwrap() = Duration::ZERO;
    drop(p);
    let p = fx.preview(&a);
    assert!(p.card().is_none(), "{}", card_why(&p));
    assert_eq!(fx.sessions.load(Ordering::SeqCst), 2, "no third session");
    assert_eq!(fx.counts.peak.load(Ordering::SeqCst), 2);
    assert_eq!(fx.counts.live.load(Ordering::SeqCst), 1);
}

/// P12 for remote text over 16 MiB (W5b): a 17 MiB file previews through its stream, indexed to
/// 4 MiB; handed over, the stream is the viewer's, and End shows the last line — no byte range
/// asked twice over preview and viewer together, nothing read whole, nothing on disk, one
/// session.
#[test]
fn a_handed_over_remote_text_over_16_mib_goes_on_through_its_stream() {
    let Some(mut fx) = remote() else { return };
    let lines = 17 * 16_384;
    let body: String = (0..lines)
        .map(|i| format!("{i:08} {}\n", "r".repeat(54)))
        .collect();
    let size = body.len() as u64;
    assert_eq!(size, 17 << 20);
    let abs = fx.file("big.log", body.as_bytes());
    let p = fx.preview(&abs);
    assert!(p.card().is_none(), "{}", card_why(&p));
    let mut handed = p.hand_over().ok().expect("a viewer");
    assert!(handed.hold.is_none(), "a stream claims nothing");
    let mut drv = Driver::new(600.0, 400.0);
    let mut win = Host(&mut handed.embedded.window);
    shown(&mut drv, &mut win);
    drv.key(&mut win, Key::End, command(false));
    let last = format!("{:08} ", lines - 1);
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    while drv.text_starting_with(&last).is_none() {
        assert!(std::time::Instant::now() < deadline, "End never showed it");
        drv.idle(&mut win, 1);
    }
    assert_read_once(&stream_reads(&fx, &abs));
    assert_eq!(whole_reads(&fx), Vec::<String>::new());
    assert!(fx.cache_files().is_empty(), "{:?}", fx.cache_files().keys());
    assert_eq!(fx.sessions.load(Ordering::SeqCst), 1, "one session");
}

/// A remote file over the read budget whose name says nothing and whose bytes are text: the
/// sniff (at most 8 KiB) says so, and text of any size streams (W5b) — its preview is its first
/// page, through the stream on the preview session: nothing read whole, nothing on disk.
#[test]
fn w8a_a_nameless_text_file_over_the_read_budget_previews_after_the_sniff() {
    let Some(mut fx) = remote() else { return };
    let body = "a printable line of a file with no extension\n".repeat(1 << 20);
    let bytes = &body.as_bytes()[..(33 << 20)];
    let abs = fx.file("README", bytes);
    let mut p = fx.preview(&abs);
    assert!(p.card().is_none(), "{}", card_why(&p));
    let sniffed = fx.counts.heads.load(Ordering::Relaxed);
    assert!(sniffed > 0 && sniffed <= 8 << 10, "{sniffed}");
    let mut drv = Driver::new(600.0, 400.0);
    shown(&mut drv, &mut Host(&mut p.embedded.window));
    assert!(
        drv.text_starting_with("a printable line").is_some(),
        "its first page"
    );
    let block = crate::stream::BLOCK;
    assert_eq!(stream_reads(&fx, &abs).first(), Some(&(0, block)));
    assert_eq!(whole_reads(&fx), Vec::<String>::new());
    assert!(fx.cache_files().is_empty(), "{:?}", fx.cache_files().keys());
    assert_eq!(fx.sessions.load(Ordering::SeqCst), 1);
}

/// Waits (at most 10 s) for what a stream's own thread does after its viewer is gone.
fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !done() {
        assert!(std::time::Instant::now() < deadline, "never: {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// P4 over streams (06 §13): a cancelled preview gives the window's preview connection back, so
/// twenty quick moves over remote text and binaries — each cancelled at once, or ready and then
/// dropped — use one session, never two at a time, and leave nothing on disk.
#[test]
fn w8a_twenty_quick_moves_over_streams_use_one_session() {
    let Some(mut fx) = remote() else { return };
    let names: Vec<String> = (0..20)
        .map(|i| {
            let ext = if i % 2 == 0 { "txt" } else { "bin" };
            let body = if i % 2 == 0 {
                vec![b'a' + (i % 26) as u8; 300 << 10]
            } else {
                crate::remote::pattern(i as u64, 300 << 10)
            };
            fx.file(&format!("f{i:02}.{ext}"), &body)
        })
        .collect();
    let mut drv = Driver::new(600.0, 400.0);
    for (i, abs) in names.iter().enumerate() {
        let cancel = AtomicBool::new(i % 3 == 0);
        let result = fx.preview_with(abs, &Budget::preview(), &cancel);
        match i % 3 {
            0 => assert!(
                result.as_ref().is_err_and(crate::remote::is_cancelled),
                "move {i}: {:?}",
                result.as_ref().err()
            ),
            // Ready, shown, then the bar moves on: dropped with its stream open.
            1 => {
                let mut p = result.expect("a preview");
                assert!(p.card().is_none(), "move {i}: {}", card_why(&p));
                shown(&mut drv, &mut Host(&mut p.embedded.window));
            }
            // Ready and dropped before it was ever drawn.
            _ => assert!(result.is_ok_and(|p| p.card().is_none()), "move {i}"),
        }
    }
    assert_eq!(
        fx.sessions.load(Ordering::SeqCst),
        1,
        "one preview session: each cancelled stream gave it back"
    );
    assert_eq!(
        fx.counts.peak.load(Ordering::SeqCst),
        1,
        "never two at once"
    );
    assert_eq!(whole_reads(&fx), Vec::<String>::new());
    assert!(fx.cache_files().is_empty(), "{:?}", fx.cache_files().keys());
}

/// A PNG of `w`×`h` that does not compress (its bytes are noise): about 3 bytes a pixel.
fn noisy_png(w: u32, h: u32) -> Vec<u8> {
    let mut seed = 0x2545_f491_u32;
    let img = ::image::DynamicImage::ImageRgb8(::image::RgbImage::from_fn(w, h, |_, _| {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        let [a, b, c, _] = seed.to_le_bytes();
        ::image::Rgb([a, b, c])
    }));
    crate::gui::image::encode(&img, ::image::ImageFormat::Png, None, None)
        .unwrap()
        .bytes
}

/// P3 (local image): the encoded budget is checked again on the bytes themselves, as they are
/// read — an image grown in place, or replaced by a bigger one, after the stat that checked its
/// size is read no further than one byte past its budget, and shows the card; Enter (the budget
/// lifted) after the first chunk lets that read go on to the end, and the image shows.
#[test]
fn a_local_image_grown_after_its_stat_is_read_only_to_its_budget() {
    let mut fx = local();
    let small = png(64, 48);
    let big = noisy_png(300, 200);
    let limits = Limits {
        image_bytes: small.len() as u64 + 100,
        ..Limits::PREVIEW
    };
    assert!(big.len() as u64 > 2 * limits.image_bytes);
    let read = |abs: &str| crate::gui::browser::prepare::local_reads(Path::new(abs));
    for (replace, lift) in [(false, false), (true, false), (false, true), (true, true)] {
        let abs = fx.file(&format!("photo-{replace}-{lift}.png"), &small);
        let (path, bigger) = (abs.clone(), big.clone());
        let mut armed = true;
        *fx.counts.after.lock().unwrap() = Some(Box::new(move |op, p| {
            if op == "stat" && p == path && std::mem::take(&mut armed) {
                if replace {
                    let tmp = format!("{path}.new");
                    std::fs::write(&tmp, &bigger).unwrap();
                    std::fs::rename(&tmp, &path).unwrap();
                } else {
                    std::fs::write(&path, &bigger).unwrap();
                }
            }
        }));
        let budget = Budget::new(limits);
        if lift {
            let enter = budget.clone();
            *budget.after_chunk.lock().unwrap() = Some(Box::new(move |_| enter.lift()));
        }
        let p = fx
            .preview_with(&abs, &budget, &AtomicBool::new(false))
            .unwrap();
        let reads = read(&abs);
        let total: u64 = reads.iter().map(|r| r.1).sum();
        let mut at = 0;
        for &(offset, n) in &reads {
            assert_eq!(offset, at, "read in order, once: {reads:?}");
            at += n;
        }
        if lift {
            assert!(p.card().is_none(), "lifted mid-read: {}", card_why(&p));
            assert!(reads.len() > 1, "lifted after the first chunk: {reads:?}");
            assert_eq!(total, big.len() as u64, "the whole image");
        } else {
            assert_eq!(card_why(&p), OVER_LIMIT, "replaced: {replace}");
            assert!(
                total <= limits.image_bytes + 1,
                "read {total} bytes of a {} byte budget",
                limits.image_bytes
            );
        }
    }
}

/// A path error on a healthy session (permission denied) is the file's card, never a reconnect:
/// the window keeps its one preview connection.
#[test]
fn a_path_error_on_a_healthy_session_is_a_card_not_a_reconnect() {
    let Some(mut fx) = remote() else { return };
    let first = fx.file("first.txt", b"first\n");
    let locked = fx.file("locked.txt", b"locked\n");
    assert!(
        fx.preview(&first).card().is_none(),
        "the connection is opened"
    );
    *fx.counts.lstat_error.lock().unwrap() = Some(std::io::ErrorKind::PermissionDenied);
    let p = fx.preview(&locked);
    assert!(
        card_why(&p).contains("cannot stat"),
        "the error is the card: {}",
        card_why(&p)
    );
    assert_eq!(fx.sessions.load(Ordering::SeqCst), 1, "no reconnect");
    assert_eq!(
        fx.counts.peak.load(Ordering::SeqCst),
        1,
        "one session at a time"
    );
}

/// A preview connection whose session died is dropped before the next one is opened: never two
/// at once.
#[test]
fn a_dead_session_is_dropped_before_the_next_one_opens() {
    let Some(mut fx) = remote() else { return };
    let abs = fx.file("a.txt", b"alive\n");
    fx.conn = Some(Box::new(
        Counting::new(inner_for(fx.label), &fx.counts).dead(),
    ));
    let p = fx.preview(&abs);
    assert!(p.card().is_none(), "{}", card_why(&p));
    assert_eq!(fx.sessions.load(Ordering::SeqCst), 1, "opened again, once");
    assert_eq!(
        fx.counts.peak.load(Ordering::SeqCst),
        1,
        "never two at once"
    );
    assert_eq!(fx.counts.live.load(Ordering::SeqCst), 1);
}

/// Cancel is looked at between every two requests of a preparation: raised inside any one of
/// them — opening the connection, the lstat, the link target's stat, realpath, the stat, the
/// sniff (a name that says nothing), then the stream's open (06 §13; an image's too, its name
/// saying its kind) — no later request is made, and nothing is read through the stream. On a dead session, a cancel raised inside the
/// failed lstat leaves it there: no probe, no new connection.
#[test]
fn a_cancel_inside_any_step_starts_no_later_one() {
    let Some(probe) = remote() else { return };
    drop(probe);
    let streamed = [
        "connect ",
        "lstat {link}",
        "stat {link}",
        "realpath {link}",
        "stat {data}",
        "read_head {data}",
        "into_ranged {data}",
    ];
    // Its name says its kind: no sniff.
    let image = [
        "connect ",
        "lstat {link}",
        "stat {link}",
        "realpath {link}",
        "stat {data}",
        "into_ranged {data}",
    ];
    for (name, steps) in [("data", &streamed[..]), ("data.png", &image[..])] {
        for k in 0..steps.len() {
            let mut fx = remote().unwrap();
            let data = fx.file(name, b"plain text with no extension\n");
            let link = fx.root.join("link");
            std::os::unix::fs::symlink(&data, &link).unwrap();
            let link = link.to_str().unwrap().to_string();
            let expected: Vec<String> = steps[..=k]
                .iter()
                .map(|s| s.replace("{link}", &link).replace("{data}", &data))
                .collect();
            let cancel = Arc::new(AtomicBool::new(false));
            let (raise, seen) = (cancel.clone(), Arc::new(AtomicUsize::new(0)));
            *fx.counts.after.lock().unwrap() = Some(Box::new(move |_, _| {
                if seen.fetch_add(1, Ordering::SeqCst) == k {
                    raise.store(true, Ordering::SeqCst);
                }
            }));
            let result = fx.preview_with(&link, &Budget::preview(), &cancel);
            assert!(
                result.as_ref().is_err_and(crate::remote::is_cancelled),
                "{name} step {k}: {:?}",
                result.as_ref().err()
            );
            assert_eq!(requests(&fx), expected, "{name}: cancelled inside step {k}");
            assert!(
                fx.counts.ranged.lock().unwrap().is_empty(),
                "{name} step {k}: nothing was read through the stream"
            );
        }
    }
    // A dead session: the cancel raised inside its failed lstat ends it.
    let mut fx = remote().unwrap();
    let data = fx.file("data", b"x\n");
    fx.conn = Some(Box::new(
        Counting::new(inner_for(fx.label), &fx.counts).dead(),
    ));
    let cancel = Arc::new(AtomicBool::new(false));
    let raise = cancel.clone();
    *fx.counts.after.lock().unwrap() = Some(Box::new(move |_, _| {
        raise.store(true, Ordering::SeqCst);
    }));
    let result = fx.preview_with(&data, &Budget::preview(), &cancel);
    assert!(result.as_ref().is_err_and(crate::remote::is_cancelled));
    assert_eq!(fx.counts.ops(), vec![format!("lstat {data}")]);
    assert_eq!(fx.sessions.load(Ordering::SeqCst), 0, "no new connection");
}

/// P4: a cancel raised by the window while an image's read is in flight — once its stream's
/// first reply has arrived — ends the read there: no later range is asked, a fraction of the
/// file crosses, the result is a cancel, and nothing is on disk. (That only one worker runs at a
/// time is the preview host's to show.)
#[test]
fn a_cancel_during_a_read_stops_its_requests() {
    let Some(mut fx) = remote() else { return };
    let bytes = noisy_png(1200, 1000);
    let abs = fx.file("big.png", &bytes);
    let (reached_tx, reached) = std::sync::mpsc::channel::<()>();
    let (go, go_rx) = std::sync::mpsc::channel::<()>();
    // The stream waits at the gate a bounded time (a test that failed never leaves it hanging),
    // and is let go before anything here can panic.
    *fx.counts.gate.lock().unwrap() = Some(Box::new(move || {
        let _ = reached_tx.send(());
        let _ = go_rx.recv_timeout(Duration::from_secs(20));
    }));
    let cancel = AtomicBool::new(false);
    let budget = Budget::preview();
    let (began, result) = std::thread::scope(|scope| {
        let worker = scope.spawn(|| fx.preview_with(&abs, &budget, &cancel).map(|_| ()));
        let began = reached.recv_timeout(Duration::from_secs(10)).is_ok();
        cancel.store(true, Ordering::SeqCst);
        drop(go);
        (began, worker.join())
    });
    assert!(began, "the read never began");
    let result = result.expect("the worker did not panic");
    assert!(
        result.as_ref().is_err_and(crate::remote::is_cancelled),
        "{:?}",
        result.err()
    );
    let block = crate::stream::BLOCK;
    assert_eq!(
        stream_reads(&fx, &abs),
        vec![(0, block)],
        "nothing asked after the cancel"
    );
    assert!(fx.counts.streamed.load(Ordering::Relaxed) <= block);
    assert!(bytes.len() as u64 > 20 * block, "a fraction of the file");
    assert_eq!(whole_reads(&fx), Vec::<String>::new());
    assert!(fx.cache_files().is_empty(), "{:?}", fx.cache_files().keys());
}

/// A clock for a budget that the test moves.
fn moved_clock() -> (
    Arc<Mutex<std::time::Instant>>,
    Box<dyn Fn() -> std::time::Instant + Send + Sync>,
) {
    let now = Arc::new(Mutex::new(std::time::Instant::now()));
    let read = now.clone();
    (now, Box::new(move || *read.lock().unwrap()))
}

/// The 5 s deadline (02 §4.3) runs out in the middle of a preparation, after the read: a local
/// text whose index is under way when the clock passes 5 s stops there — part of it indexed,
/// not all — and shows the card. The clock moves as the preparation looks at it, not by sleeping.
#[test]
fn the_deadline_stops_a_preparation_in_the_middle() {
    let mut fx = local();
    let abs = fx.file("three.log", &vec![b'x'; 3 << 20]);
    let (now, clock) = moved_clock();
    let looked = Arc::new(AtomicUsize::new(0));
    let (n, moved) = (looked.clone(), now.clone());
    let clock: Box<dyn Fn() -> std::time::Instant + Send + Sync> = Box::new(move || {
        // From the fourth look on (the read's end, the check after it, the index starting, its
        // first MiB), 6 s have passed.
        if n.fetch_add(1, Ordering::SeqCst) == 3 {
            *moved.lock().unwrap() += Duration::from_secs(6);
        }
        clock()
    });
    let budget = Budget::with_clock(Limits::PREVIEW, clock);
    let p = fx
        .preview_with(&abs, &budget, &AtomicBool::new(false))
        .unwrap();
    assert_eq!(card_why(&p), "the preview took over 5 s: Enter opens it");
    let indexed: u64 = crate::gui::text::index_reads(Path::new(&abs))
        .iter()
        .map(|r| r.1)
        .sum();
    assert!(
        indexed > 0 && indexed < 3 << 20,
        "stopped in the middle: {indexed} bytes indexed"
    );
    drop(now);
}

/// The 5 s count from the end of the remote read (02 §4.3: an image's read through its stream
/// shows its progress and is bounded by its bytes): a read that takes a minute leaves the
/// preparation after it its whole 5 s, and the preview shows.
#[test]
fn the_deadline_starts_after_the_read() {
    let Some(mut fx) = remote() else { return };
    let abs = fx.file("slow.png", &noisy_png(500, 400));
    let (now, clock) = moved_clock();
    let budget = Budget::with_clock(Limits::PREVIEW, clock);
    let moved = now.clone();
    *fx.counts.gate.lock().unwrap() = Some(Box::new(move || {
        *moved.lock().unwrap() += Duration::from_secs(60);
    }));
    let p = fx
        .preview_with(&abs, &budget, &AtomicBool::new(false))
        .unwrap();
    assert!(p.card().is_none(), "{}", card_why(&p));
    assert!(
        stream_reads(&fx, &abs).len() > 1,
        "the read went on after the minute"
    );
}

// ---------------------------------------------------------------------------------------------
// 06 §13 W8a: Enter opens a stream.

/// What the connections were asked, in order, without how a stream let go of its transport
/// (`close`, `into_connection`: its own thread's, whenever it ends).
fn requests(fx: &Fx) -> Vec<String> {
    let ops = fx.counts.ops();
    ops.into_iter()
        .filter(|op| !op.starts_with("close ") && !op.starts_with("into_connection "))
        .collect()
}

/// Every full read (`read_to`) the connections were asked for.
fn whole_reads(fx: &Fx) -> Vec<String> {
    let ops = fx.counts.ops();
    ops.into_iter()
        .filter(|op| op.starts_with("read_to "))
        .collect()
}

/// 06 §12 V1 at the browser's Enter: a remote binary of the owner's size (24 GB, here sparse)
/// opens through a stream on the open's own connection. Nothing of it is read whole or reaches
/// the cache, no second connection is made — whether the server can replace a file is not asked
/// at open (06 §2) — the sniff is at most 8 KiB, and the first screen costs one block.
#[test]
fn w8a_enter_on_a_remote_binary_opens_a_stream_and_fetches_one_block() {
    const SIZE: u64 = 24_000_000_000;
    let Some(fx) = remote() else { return };
    let abs = fx.file("model.safetensors", b"");
    std::fs::File::options()
        .write(true)
        .open(&abs)
        .unwrap()
        .set_len(SIZE)
        .unwrap();
    let prepared = prepare(&fx.request(&abs), &AtomicBool::new(false), &mut |_, _| {}).unwrap();
    let Prepared::Hex { file, notices } = prepared else {
        panic!("not the hex window")
    };
    let stream = file
        .stream()
        .expect("a remote file is viewed through a stream");
    assert_eq!(stream.size(), SIZE);
    assert_eq!(whole_reads(&fx), Vec::<String>::new(), "no whole read");
    assert!(file.can_replace(), "taken as yes until the first Save");
    assert_eq!(
        fx.sessions.load(Ordering::SeqCst),
        1,
        "the open's connection is the stream's, and the Origin has not connected"
    );
    let mut embedded = hex::embed(file, notices);
    let mut drv = Driver::new(760.0, 560.0);
    shown(&mut drv, &mut Host(&mut embedded.window));
    assert!(
        drv.find_text("00 00 00 00 00 00 00 00  00 00 00 00 00 00 00 00")
            .is_some(),
        "the first rows"
    );
    let block = crate::stream::BLOCK;
    assert_eq!(
        *fx.counts.ranged.lock().unwrap(),
        vec![(abs.clone(), 0, block as u32)],
        "one block before the first screen"
    );
    let fetched = stream.fetched().total;
    assert_eq!(fetched, block);
    assert_eq!(
        fx.counts.streamed.load(Ordering::Relaxed),
        fetched,
        "what crossed the connection is what the stream counts"
    );
    assert!(
        fx.counts.heads.load(Ordering::Relaxed) <= 8 << 10,
        "the sniff"
    );
    assert_eq!(fx.sessions.load(Ordering::SeqCst), 1);
    assert_eq!(fx.counts.peak.load(Ordering::SeqCst), 1, "one channel");
    assert!(fx.cache_files().is_empty(), "{:?}", fx.cache_files().keys());
}

/// The browser's Enter on remote text: a file of any size opens through a stream (06 §7.2; W5b
/// for one over 16 MiB) — no whole read, nothing in the cache, the open's one session, and its
/// first screen's block first.
#[test]
fn w8a_enter_on_remote_text_streams_at_any_size() {
    let Some(probe) = remote() else { return };
    drop(probe);
    let cap = crate::gui::text::HIGHLIGHT_MAX;
    for size in [300 << 10, cap, cap + 1] {
        let fx = remote().unwrap();
        let mut body = b"the first line\n".to_vec();
        body.resize(size as usize - 1, b'x');
        body.push(b'\n');
        let abs = fx.file("notes.txt", &body);
        let prepared = prepare(&fx.request(&abs), &AtomicBool::new(false), &mut |_, _| {}).unwrap();
        let Prepared::Text { doc, claim, .. } = prepared else {
            panic!("{size}: not the text viewer")
        };
        assert!(doc.is_stream(), "{size} bytes");
        assert!(claim.is_none(), "{size} bytes: the cache");
        assert_eq!(fx.sessions.load(Ordering::SeqCst), 1, "{size} bytes");
        assert_eq!(doc.total(), size);
        let mut embedded = text::embed(
            doc,
            format!("box:{abs}"),
            "notes.txt",
            Vec::new(),
            false,
            None,
        );
        let mut drv = Driver::new(600.0, 400.0);
        shown(&mut drv, &mut Host(&mut embedded.window));
        assert!(drv.find_text("the first line").is_some(), "{size} bytes");
        let first = fx.counts.ranged.lock().unwrap().first().cloned();
        assert_eq!(
            first,
            Some((abs.clone(), 0, crate::stream::BLOCK as u32)),
            "{size} bytes: the first screen's block comes first"
        );
        assert_eq!(whole_reads(&fx), Vec::<String>::new(), "{size} bytes");
        assert!(fx.cache_files().is_empty(), "{:?}", fx.cache_files().keys());
    }
}

/// 06 §12 V17: local files are untouched — text and binary are read in place, with no stream
/// and nothing asked of a transport.
#[test]
fn w8a_v17_local_files_get_no_stream() {
    let fx = local();
    let text = fx.file("notes.txt", b"plain\n");
    let bin = fx.file("blob.bin", &[0u8, 159, 146, 150, 0, 1, 2, 3]);
    let prepared = prepare(&fx.request(&text), &AtomicBool::new(false), &mut |_, _| {}).unwrap();
    let Prepared::Text { doc, claim, .. } = prepared else {
        panic!("not the text viewer")
    };
    assert!(!doc.is_stream() && claim.is_none(), "read in place");
    let prepared = prepare(&fx.request(&bin), &AtomicBool::new(false), &mut |_, _| {}).unwrap();
    let Prepared::Hex { file, .. } = prepared else {
        panic!("not the hex window")
    };
    assert!(file.stream().is_none(), "read in place");
    assert!(fx.counts.ranged.lock().unwrap().is_empty());
    assert!(!fx
        .counts
        .ops()
        .iter()
        .any(|op| op.starts_with("into_ranged")));
    assert!(fx.cache_files().is_empty());
}

/// "Any size" includes none: an empty remote file — named as text, or with a name that says
/// nothing — opens through a stream and its viewer comes ready (nothing to wait for), by Enter
/// and as a preview.
#[test]
fn w8a_an_empty_remote_file_opens_and_comes_ready() {
    let Some(mut fx) = remote() else { return };
    let mut drv = Driver::new(600.0, 400.0);
    for name in ["empty.txt", "empty"] {
        let abs = fx.file(name, b"");
        let prepared = prepare(&fx.request(&abs), &AtomicBool::new(false), &mut |_, _| {}).unwrap();
        let mut embedded = match prepared {
            Prepared::Text { doc, .. } => {
                assert!(doc.is_stream(), "{name}");
                text::embed(doc, format!("box:{abs}"), name, Vec::new(), false, None)
            }
            Prepared::Hex { file, notices } => {
                assert!(file.stream().is_some(), "{name}");
                hex::embed(file, notices)
            }
            _ => panic!("{name}: neither text nor hex"),
        };
        shown(&mut drv, &mut Host(&mut embedded.window));
        assert!(embedded.window.ready(), "{name}: Enter");
        drop(embedded);
        let mut p = fx.preview(&abs);
        assert!(p.card().is_none(), "{name}: {}", card_why(&p));
        shown(&mut drv, &mut Host(&mut p.embedded.window));
        assert!(p.embedded.window.ready(), "{name}: the preview");
    }
    assert_eq!(whole_reads(&fx), Vec::<String>::new());
}

// ---------------------------------------------------------------------------------------------
// 06 §13 W8b: the rest of the routes through a stream — remote text over 16 MiB, images and
// media, by Enter and as previews.

/// `len` bytes of text in lines, each saying its number (`line 00000000 …`).
fn text_lines(len: usize) -> Vec<u8> {
    let mut body = Vec::with_capacity(len + 64);
    let mut n = 0u64;
    while body.len() < len {
        body.extend_from_slice(format!("line {n:08} of a long remote log\n").as_bytes());
        n += 1;
    }
    body.truncate(len - 1);
    body.push(b'\n');
    body
}

/// The file at `abs` made `len` bytes long without writing them (sparse: zeros past its end).
fn grow(abs: &str, len: u64) {
    std::fs::File::options()
        .write(true)
        .open(abs)
        .unwrap()
        .set_len(len)
        .unwrap();
}

/// What a 2 GiB video's first frame may cost here, at most: a sixteenth of the file — a bound in
/// bytes, never a time, that a route fetching the file whole cannot stay under. It is not the
/// engine's own figure: the byte server's rate is the file's (2 GiB in 2 s), so a `play` reader
/// may read ahead its most (32 MiB, 06 §4's table), and the probe and the frame open a few such
/// readers; this leaves room for several of them. W8b's live receipts measured 3–5 MB.
const FAR_SHORT: u64 = (2 << 30) / 16;

/// A 2 s 160×120 clip with its `moov` at the front, followed by a top-level `free` box (64-bit
/// size) that makes the file `len` bytes long, sparse: a valid video of any size that costs no
/// disk.
fn padded_clip(tools: &Tools, dst: &Path, len: u64) {
    let front = dst.with_extension("front.mp4");
    clip(tools, &front, (160, 120));
    make_media(
        tools,
        &[
            "-i",
            front.to_str().unwrap(),
            "-c",
            "copy",
            "-movflags",
            "+faststart",
        ],
        dst,
    );
    std::fs::remove_file(&front).unwrap();
    let have = std::fs::metadata(dst).unwrap().len();
    let mut box_head = 1u32.to_be_bytes().to_vec();
    box_head.extend_from_slice(b"free");
    box_head.extend_from_slice(&(len - have).to_be_bytes());
    use std::io::Write as _;
    let mut file = std::fs::File::options().append(true).open(dst).unwrap();
    file.write_all(&box_head).unwrap();
    file.set_len(len).unwrap();
}

/// W8b at the browser's Enter: remote text over 16 MiB (W5b's large text) opens through a stream
/// as text of any size does — its first block first, nothing read whole, nothing in the cache,
/// the open's one session — and End shows its last line, read from the end, not after the rest.
#[test]
fn w8b_enter_on_remote_text_over_16_mib_streams() {
    let Some(fx) = remote() else { return };
    // Over W5a's 16 MiB, by a margin.
    let size = 17 << 20;
    let body = text_lines(size);
    // The last whole line (the body ends with its newline).
    let tail = &body[body.len() - 200..body.len() - 1];
    let from = tail.iter().rposition(|&b| b == b'\n').unwrap() + 1;
    let last = String::from_utf8_lossy(&tail[from..]).to_string();
    let abs = fx.file("big.log", &body);
    drop(body);
    let prepared = prepare(&fx.request(&abs), &AtomicBool::new(false), &mut |_, _| {});
    assert_eq!(whole_reads(&fx), Vec::<String>::new(), "nothing read whole");
    let Prepared::Text { doc, claim, .. } = prepared.unwrap() else {
        panic!("not the text viewer")
    };
    assert!(doc.is_stream(), "read through a stream");
    assert!(claim.is_none());
    assert_eq!(doc.total(), size as u64);
    assert_eq!(
        fx.sessions.load(Ordering::SeqCst),
        1,
        "the open's one session"
    );
    let mut embedded = text::embed(
        doc,
        format!("box:{abs}"),
        "big.log",
        Vec::new(),
        false,
        None,
    );
    let mut drv = Driver::new(700.0, 400.0);
    let mut win = Host(&mut embedded.window);
    shown(&mut drv, &mut win);
    assert!(
        drv.text_starting_with("line 00000000").is_some(),
        "the first screen"
    );
    let block = crate::stream::BLOCK;
    assert_eq!(
        fx.counts.ranged.lock().unwrap().first().cloned(),
        Some((abs.clone(), 0, block as u32)),
        "the first screen's block first"
    );
    drv.key(&mut win, Key::End, command(false));
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    while drv.text_starting_with(&last).is_none() {
        assert!(
            std::time::Instant::now() < deadline,
            "End never showed {last}"
        );
        drv.idle(&mut win, 1);
    }
    assert_eq!(whole_reads(&fx), Vec::<String>::new());
    assert!(fx.cache_files().is_empty(), "{:?}", fx.cache_files().keys());
}

/// W8b as a preview: remote text over 16 MiB previews through a stream (no longer the card,
/// nor read whole): its first page, nothing read whole, nothing on disk.
#[test]
fn w8b_a_remote_text_over_16_mib_previews_through_its_stream() {
    let Some(mut fx) = remote() else { return };
    let abs = fx.file("big.log", &text_lines(17 << 20));
    let mut p = fx.preview(&abs);
    assert!(p.card().is_none(), "{}", card_why(&p));
    let mut drv = Driver::new(600.0, 400.0);
    shown(&mut drv, &mut Host(&mut p.embedded.window));
    assert!(
        drv.text_starting_with("line 00000000").is_some(),
        "its first page"
    );
    assert_eq!(whole_reads(&fx), Vec::<String>::new());
    assert!(fx.cache_files().is_empty(), "{:?}", fx.cache_files().keys());
}

/// W8b at the browser's Enter: a remote image opens through a stream on the open's connection
/// — a 2 GiB one (sparse, a PNG header at its front) included: nothing of it is read whole,
/// nothing reaches the cache, one session. Its window shows the memory line (with Hex view)
/// having read its header's block, and nothing more until it is answered.
#[test]
fn w8b_enter_on_a_remote_image_reads_nothing_whole() {
    let Some(fx) = remote() else { return };
    let abs = fx.file("huge.png", &png_header(100_000, 100_000));
    grow(&abs, 2 << 30);
    let prepared = prepare(&fx.request(&abs), &AtomicBool::new(false), &mut |_, _| {});
    assert_eq!(whole_reads(&fx), Vec::<String>::new(), "nothing read whole");
    assert!(prepared.is_ok(), "{:#}", prepared.as_ref().err().unwrap());
    assert!(fx.cache_files().is_empty(), "{:?}", fx.cache_files().keys());
    assert_eq!(
        fx.sessions.load(Ordering::SeqCst),
        1,
        "the open's one session"
    );
    let Ok(Prepared::ImageStream {
        origin,
        source,
        repaint,
        notices,
    }) = prepared
    else {
        panic!("not an image through its stream")
    };
    assert_eq!(source.size(), 2 << 30);
    let switch = image::HexSwitch::new(repaint);
    let mut embedded =
        image::embed_stream(origin, source, &fx.cache, notices, Some(switch)).expect("the window");
    let mut drv = Driver::new(900.0, 600.0);
    let mut win = Host(&mut embedded.window);
    shown(&mut drv, &mut win);
    drv.idle(&mut win, 5);
    let line = drv
        .text_starting_with("reading and decoding this image needs")
        .expect("the memory line");
    assert!(line.contains("Hex view (H)"), "{line}");
    let block = crate::stream::BLOCK;
    assert_eq!(
        *fx.counts.ranged.lock().unwrap(),
        vec![(abs.clone(), 0, block as u32)],
        "its header's block, and nothing until the line is answered"
    );
}

/// W8b as a preview: a remote image previews through a stream on the window's preview session
/// — read once, nothing read whole, nothing in the cache — and the picture is
/// drawn.
#[test]
fn w8b_a_remote_image_previews_through_a_stream() {
    let Some(mut fx) = remote() else { return };
    let bytes = png(64, 48);
    let abs = fx.file("photo.png", &bytes);
    let mut p = fx.preview(&abs);
    assert!(p.card().is_none(), "{}", card_why(&p));
    let mut drv = Driver::new(500.0, 300.0);
    shown(&mut drv, &mut Host(&mut p.embedded.window));
    assert_eq!(drv.pictures().len(), 1, "the picture is drawn");
    assert_eq!(whole_reads(&fx), Vec::<String>::new(), "nothing read whole");
    assert!(fx.cache_files().is_empty(), "{:?}", fx.cache_files().keys());
}

/// W8b, 02 §4.3's pixel budget through a stream: a remote image whose header says it is over
/// 64 megapixels is the card having read its header's block — not its 20 MiB body.
#[test]
fn w8b_a_remote_image_over_the_pixel_budget_is_its_card_after_its_header() {
    let Some(mut fx) = remote() else { return };
    let abs = fx.file("wide.png", &png_header(30_000, 30_000));
    grow(&abs, 20 << 20);
    let p = fx.preview(&abs);
    assert_eq!(card_why(&p), "image is 30000×30000; Enter opens it");
    assert_eq!(whole_reads(&fx), Vec::<String>::new(), "nothing read whole");
    let block = crate::stream::BLOCK;
    assert_eq!(
        *fx.counts.ranged.lock().unwrap(),
        vec![(abs.clone(), 0, block as u32)],
        "the header's block"
    );
}

/// Lifted (Enter while it is prepared), an image's preview keeps the image window's own question
/// (06 §7.3's memory line): a remote image whose header asks for more than the window decodes
/// without asking is the card having read its header's block — never read whole into memory
/// unasked — and Enter opens it in the window that asks.
#[test]
fn w8b_a_lifted_image_preview_still_asks_before_it_reads_a_huge_image() {
    let Some(mut fx) = remote() else { return };
    let abs = fx.file("huge.png", &png_header(100_000, 100_000));
    grow(&abs, 40 << 20);
    let budget = Budget::preview();
    let (lift, path) = (budget.clone(), abs.clone());
    // Enter right after its stat: the read budget, which would have made it the card from the
    // stat, is lifted.
    *fx.counts.after.lock().unwrap() = Some(Box::new(move |op, p| {
        if op == "stat" && p == path {
            lift.lift();
        }
    }));
    let p = fx
        .preview_with(&abs, &budget, &AtomicBool::new(false))
        .unwrap();
    assert!(budget.lifted());
    assert!(
        fx.counts.ops().contains(&format!("into_ranged {abs}")),
        "it went on to its stream"
    );
    assert_eq!(card_why(&p), OVER_LIMIT);
    let block = crate::stream::BLOCK;
    assert_eq!(
        stream_reads(&fx, &abs),
        vec![(0, block)],
        "its header's block, nothing more"
    );
    assert_eq!(whole_reads(&fx), Vec::<String>::new());
}

/// 06 §7.3 for a local image: one that does not decode opens in the hex window, why said first
/// in its rows — not the error row of before.
#[test]
fn w8b_a_local_image_that_does_not_decode_opens_as_bytes() {
    let fx = local();
    let mut broken = b"\x89PNG\r\n\x1a\n".to_vec();
    broken.extend((0..200u32).map(|i| (i * 7) as u8));
    let abs = fx.file("broken.png", &broken);
    let prepared = prepare(&fx.request(&abs), &AtomicBool::new(false), &mut |_, _| {}).unwrap();
    let Prepared::Hex { file, notices } = prepared else {
        panic!("not the hex window")
    };
    assert!(file.stream().is_none(), "read in place");
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert!(
        notices[0].contains("cannot decode") || notices[0].contains("Format error"),
        "{notices:?}"
    );
}

/// P6 (image) through a stream: Enter on a previewed remote image — the preview is the viewer:
/// nothing claimed or published into the cache, the file crossed once, decoded once.
#[test]
fn w8b_a_handed_over_remote_image_is_the_viewer_with_nothing_cached() {
    let Some(mut fx) = remote() else { return };
    let bytes = png(64, 48);
    let abs = fx.file("photo.png", &bytes);
    let decodes = crate::gui::image::decodes();
    let p = fx.preview(&abs);
    assert!(p.card().is_none(), "{}", card_why(&p));
    let mut handed = p.hand_over().ok().expect("a viewer");
    assert!(handed.hold.is_none(), "a stream claims nothing");
    assert!(fx.cache_files().is_empty(), "{:?}", fx.cache_files().keys());
    let mut drv = Driver::new(700.0, 500.0);
    shown(&mut drv, &mut Host(&mut handed.embedded.window));
    assert_eq!(drv.pictures().len(), 1, "the picture");
    assert_eq!(crate::gui::image::decodes() - decodes, 1, "decoded once");
    assert_eq!(whole_reads(&fx), Vec::<String>::new());
    assert_eq!(
        fx.counts.streamed.load(Ordering::Relaxed),
        bytes.len() as u64,
        "the file crossed once"
    );
}

/// W8b at the browser's Enter: a remote video — 2 GiB, sparse, its `moov` at the front — opens
/// through a stream: nothing read whole, nothing in the cache.
#[test]
fn w8b_enter_on_remote_media_reads_nothing_whole() {
    let Some(tools) = media_tools() else { return };
    let Some(fx) = remote() else { return };
    let video = fx.root.join("big.mp4");
    padded_clip(&tools, &video, 2 << 30);
    let abs = video.to_str().unwrap().to_string();
    let prepared = prepare(&fx.request(&abs), &AtomicBool::new(false), &mut |_, _| {});
    assert_eq!(whole_reads(&fx), Vec::<String>::new(), "nothing read whole");
    assert!(prepared.is_ok(), "{:#}", prepared.as_ref().err().unwrap());
    assert!(fx.cache_files().is_empty(), "{:?}", fx.cache_files().keys());
    let Ok(Prepared::MediaStream {
        origin,
        media,
        notices,
    }) = prepared
    else {
        panic!("not a medium through its stream")
    };
    assert_eq!(media.input.kind(), Some(crate::stream::http::Kind::Play));
    let mut embedded = media::embed_streamed(origin, *media, &fx.cache, notices);
    let mut drv = Driver::new(800.0, 600.0);
    {
        let mut win = Host(&mut embedded.window);
        shown(&mut drv, &mut win);
        assert_eq!(drv.pictures().len(), 1, "its first frame");
    }
    let crossed = fx.counts.streamed.load(Ordering::Relaxed);
    assert!(crossed <= FAR_SHORT, "{crossed} bytes of a 2 GiB file");
    assert_eq!(whole_reads(&fx), Vec::<String>::new());
    assert_eq!(
        fx.sessions.load(Ordering::SeqCst),
        1,
        "the open's one session"
    );
    drop(embedded);
}

/// W8b as a preview: a remote video far over the read budget (2 GiB) previews its first frame
/// through a stream (02 §4.3: the 32 MiB no longer gates it), nothing read whole.
#[test]
fn w8b_a_remote_video_over_the_read_budget_previews_its_first_frame() {
    let Some(tools) = media_tools() else { return };
    let Some(mut fx) = remote() else { return };
    let video = fx.root.join("big.mp4");
    padded_clip(&tools, &video, 2 << 30);
    let mut p = fx.preview_unhurried(video.to_str().unwrap(), Limits::PREVIEW);
    assert!(p.card().is_none(), "{}", card_why(&p));
    let mut drv = Driver::new(500.0, 400.0);
    shown(&mut drv, &mut Host(&mut p.embedded.window));
    assert_eq!(drv.pictures().len(), 1, "the frame");
    assert_eq!(whole_reads(&fx), Vec::<String>::new());
    assert!(fx.cache_files().is_empty(), "{:?}", fx.cache_files().keys());
}

/// W8b, 02 §4.4: a remote video's and a remote sound's previews, dropped (the bar moves on),
/// give the preview session back — every hold on their stream (the byte server, its
/// connections, the probe's input) goes with them — and the next preview takes it: one session
/// throughout, never two at once.
#[test]
fn w8b_a_dropped_media_preview_gives_its_session_back() {
    let Some(tools) = media_tools() else { return };
    let Some(mut fx) = remote() else { return };
    let video = fx.root.join("clip.mp4");
    clip(&tools, &video, (160, 120));
    let sound = fx.root.join("tone.wav");
    tone(&tools, &sound, 3);
    let text = fx.file("after.txt", b"the next preview\n");
    for media in [&video, &sound] {
        let abs = media.to_str().unwrap().to_string();
        let mut p = fx.preview_unhurried(&abs, Limits::PREVIEW);
        assert!(p.card().is_none(), "{}", card_why(&p));
        let mut drv = Driver::new(500.0, 400.0);
        shown(&mut drv, &mut Host(&mut p.embedded.window));
        drop(p);
        let given = format!("into_connection {abs}");
        wait_for(&given, || fx.counts.ops().contains(&given));
    }
    let mut p = fx.preview(&text);
    let mut drv = Driver::new(500.0, 400.0);
    shown(&mut drv, &mut Host(&mut p.embedded.window));
    assert!(drv.find_text("the next preview").is_some());
    assert_eq!(fx.sessions.load(Ordering::SeqCst), 1, "one preview session");
    assert_eq!(
        fx.counts.peak.load(Ordering::SeqCst),
        1,
        "never two at once"
    );
    assert_eq!(whole_reads(&fx), Vec::<String>::new());
}

/// W8b, the `.ts` refine over the URL (06 §7.4): a remote `.ts` whose head looks like a
/// transport stream but which ffprobe does not read as media opens in the hex window on the
/// same stream — nothing read whole first.
#[test]
fn w8b_a_remote_ts_that_is_not_media_is_hex_on_its_stream() {
    let Some(_tools) = media_tools() else { return };
    let Some(fx) = remote() else { return };
    let mut body = vec![0u8; 188 * 64];
    for packet in body.chunks_mut(188) {
        packet[0] = 0x47;
    }
    let abs = fx.file("clip.ts", &body);
    let prepared = prepare(&fx.request(&abs), &AtomicBool::new(false), &mut |_, _| {}).unwrap();
    assert_eq!(whole_reads(&fx), Vec::<String>::new(), "nothing read whole");
    let Prepared::Hex { file, .. } = prepared else {
        panic!("not the hex window")
    };
    assert!(
        file.stream().is_some(),
        "on the stream it was probed through"
    );
    assert!(fx.cache_files().is_empty(), "{:?}", fx.cache_files().keys());
}

// ---------------------------------------------------------------------------------------------
// Slice S1-M: the media preview (02 §4.2–§4.4; P1, P3, P4, P6 and P12 for video and audio).
// Time is a clock the test moves, a program that stalls is a stand-in held at a named pipe, and
// processes are counted as the system lists them (`crate::gui::media::fixtures`).

use crate::ffmpeg::{spawns, Tools};
use crate::gui::media::fixtures::{
    bounded, clip, make as make_media, none_left, stand_in, started_under, still_running, tone,
    tools as media_tools, until, Hand,
};
use crate::gui::media::wave;

/// The folder a fixture's files, cache and stand-ins are all under.
fn base(fx: &Fx) -> PathBuf {
    fx.cache.parent().unwrap().to_path_buf()
}

fn assert_none_left(dir: &Path) {
    if let Err(left) = none_left(dir) {
        panic!("{left}");
    }
}

/// A budget of 02 §4.3's limits on `hand`, running `tools`.
fn budget_on(hand: &Hand, tools: Tools) -> Arc<Budget> {
    let hand = hand.clone();
    Budget::with_clock(Limits::PREVIEW, Box::new(move || hand.now())).with_tools(tools)
}

/// P1 (video and audio): a local video previews as its first frame with its size and duration,
/// a sound as its waveform with its duration — read in place, nothing in the cache — and a
/// "video" that holds only sound is previewed as the sound it is.
#[test]
fn a_video_previews_its_frame_and_a_sound_its_waveform() {
    let Some(tools) = media_tools() else { return };
    let mut fx = local();
    let video = fx.root.join("clip.mp4");
    clip(&tools, &video, (160, 120));
    let mut p = fx.preview(video.to_str().unwrap());
    assert!(p.card().is_none(), "{}", card_why(&p));
    let mut drv = Driver::new(500.0, 400.0);
    shown(&mut drv, &mut Host(&mut p.embedded.window));
    assert_eq!(drv.pictures().len(), 1, "the frame");
    let said = drv.text_starting_with("160×120").expect("its size");
    assert!(said.contains("0:02.00"), "{said}");
    assert!(drv.find_text("Play").is_none(), "no bars");
    let runs = spawns::of(&video);
    assert_eq!((runs.probe, runs.preview), (1, 1), "one probe, one frame");

    let sound = fx.root.join("tone.wav");
    tone(&tools, &sound, 3);
    let mut p = fx.preview(sound.to_str().unwrap());
    assert!(p.card().is_none(), "{}", card_why(&p));
    shown(&mut drv, &mut Host(&mut p.embedded.window));
    let said = drv.text_starting_with("audio · mono").expect("the line");
    assert!(said.contains("0:03.00"), "{said}");
    assert_eq!(wave::starts(&sound), [(0.0, None)]);
    let wave_colour = chrome::PALETTE.wave;
    let tall = drv
        .segments()
        .iter()
        .filter(|([a, b], c)| *c == wave_colour && (a.y - b.y).abs() > 20.0)
        .count();
    assert!(tall > 300, "the waveform, painted: {tall} columns");

    // Named as a video, holding only sound: the sound's preview.
    let named = fx.root.join("only-sound.mp4");
    make_media(
        &tools,
        &[
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000:duration=2",
            "-c:a",
            "aac",
        ],
        &named,
    );
    let mut p = fx.preview(named.to_str().unwrap());
    assert!(p.card().is_none(), "{}", card_why(&p));
    shown(&mut drv, &mut Host(&mut p.embedded.window));
    assert!(drv.text_starting_with("audio · ").is_some());
    assert_none_left(&base(&fx));
    assert!(fx.cache_files().is_empty(), "{:?}", fx.cache_files().keys());
}

/// P6 (video) through a stream: handed over, a remote video's stream is the media window's —
/// its frame the one the preview decoded, the session gone with it (not given back), nothing
/// claimed or on disk — and of a 2 GiB file (its `moov` at the front) far less than the file
/// crossed ([`FAR_SHORT`]).
#[test]
fn a_handed_over_remote_video_keeps_its_stream_and_its_frame() {
    let Some(tools) = media_tools() else { return };
    let Some(mut fx) = remote() else { return };
    let video = fx.root.join("clip.mp4");
    padded_clip(&tools, &video, 2 << 30);
    let abs = video.to_str().unwrap().to_string();
    let p = fx.preview_unhurried(&abs, Limits::PREVIEW);
    assert!(p.card().is_none(), "{}", card_why(&p));
    let crossed = fx.counts.streamed.load(Ordering::Relaxed);
    assert!(crossed <= FAR_SHORT, "{crossed} bytes for the first frame");
    let mut handed = p.hand_over().ok().expect("a viewer");
    assert!(handed.hold.is_none(), "a stream claims nothing");
    let mut drv = Driver::new(800.0, 600.0);
    {
        let mut win = Host(&mut handed.embedded.window);
        shown(&mut drv, &mut win);
        assert!(drv.find_text("Play").is_some(), "the media window's bars");
        assert_eq!(drv.pictures().len(), 1, "the frame");
        drv.idle(&mut win, 3);
    }
    assert_eq!(whole_reads(&fx), Vec::<String>::new());
    assert_eq!(fx.sessions.load(Ordering::SeqCst), 1, "no second session");
    assert!(
        !fx.counts
            .ops()
            .iter()
            .any(|op| op.starts_with("into_connection ")),
        "the stream kept its session"
    );
    let crate::gui::Embedded { window, mut settle } = handed.embedded;
    drop(window);
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    while settle(true).is_none() {
        assert!(std::time::Instant::now() < deadline, "never settled");
    }
    drop(settle);
    assert!(fx.cache_files().is_empty(), "{:?}", fx.cache_files().keys());
    assert_none_left(&base(&fx));
}

/// P12 (audio) through a stream: a remote sound longer than a preview reads (here 2 s of 6)
/// shows that much of its waveform; handed over, the viewer decodes on from there through the
/// same stream — no byte range asked twice over preview and viewer together, nothing on disk.
#[test]
fn a_handed_over_remote_sound_decodes_on_through_its_stream() {
    let Some(tools) = media_tools() else { return };
    let Some(mut fx) = remote() else { return };
    let sound = fx.root.join("tone.wav");
    tone(&tools, &sound, 6);
    let abs = sound.to_str().unwrap().to_string();
    let size = std::fs::metadata(&sound).unwrap().len();
    let limits = Limits {
        sound: Duration::from_secs(2),
        ..Limits::PREVIEW
    };
    let mut p = fx.preview_unhurried(&abs, limits);
    assert!(p.card().is_none(), "{}", card_why(&p));
    let mut drv = Driver::new(500.0, 400.0);
    shown(&mut drv, &mut Host(&mut p.embedded.window));
    let said = drv.text_starting_with("audio · mono").expect("the line");
    assert!(
        said.contains("0:06.00") && said.contains("preview of the first 2 s"),
        "{said}"
    );
    let mut handed = p.hand_over().ok().expect("a viewer");
    let mut drv = Driver::new(800.0, 500.0);
    {
        let mut win = Host(&mut handed.embedded.window);
        drv.until(&mut win, 20, |w| w.ready());
        drv.idle(&mut win, 2);
        assert!(drv.find_text("Play").is_some(), "the media window's bars");
        let deadline = std::time::Instant::now() + Duration::from_secs(60);
        while fx.counts.streamed.load(Ordering::Relaxed) < size {
            assert!(
                std::time::Instant::now() < deadline,
                "never read to its end"
            );
            drv.idle(&mut win, 1);
        }
    }
    assert!(assert_read_once(&stream_reads(&fx, &abs)) >= size);
    assert_eq!(whole_reads(&fx), Vec::<String>::new());
    drop(handed);
    assert!(fx.cache_files().is_empty(), "{:?}", fx.cache_files().keys());
    assert_none_left(&base(&fx));
}

/// P3: a video whose probe hangs (a stand-in `ffprobe` that never answers) goes on while its
/// time is not over and shows its card when it is — said there, the kind known — and the
/// stand-in is killed and reaped. The time is the test's clock: nothing is waited for.
#[test]
fn a_stalled_ffprobe_is_a_card_when_its_time_is_over_and_no_process_is_left() {
    bounded(180, |t| {
        let mut fx = local();
        t.also(&base(&fx));
        let hold = t.hold(&base(&fx), "hold");
        let stalls = stand_in(&base(&fx), "stalls", &hold.stalls());
        let tools = Tools {
            ffmpeg: stalls.clone(),
            ffprobe: stalls,
        };
        let abs = fx.file("hangs.mp4", b"whatever is in it");
        let hand = t.hand();
        let budget = budget_on(&hand, tools);
        let root = fx.root.clone();
        let p = std::thread::scope(|scope| {
            // The clock, moved from a thread of its own once the probe runs.
            let clock = scope.spawn(|| {
                until("the probe starts", || !started_under(&root).is_empty());
                hand.advance(Duration::from_millis(4900));
                std::thread::sleep(Duration::from_millis(100));
                let running = still_running(&started_under(&root));
                hand.advance(Duration::from_millis(100));
                running
            });
            let p = fx.preview_with(&abs, &budget, &AtomicBool::new(false));
            assert_eq!(clock.join().unwrap(), 1, "4.9 s of its 5 s: it went on");
            p.unwrap()
        });
        assert_eq!(card_why(&p), "ffprobe took over 5 s: Enter opens it");
        assert_eq!(p.card().unwrap().kind, Some(Kind::Video));
        assert_eq!(started_under(&fx.root).len(), 1, "the probe ran");
        assert_none_left(&base(&fx));
    });
}

/// The real clock: a stalled probe held to 300 ms is its card about then.
#[test]
fn on_the_real_clock_a_stalled_ffprobe_is_a_card_in_time() {
    bounded(180, |t| {
        let mut fx = local();
        t.also(&base(&fx));
        let hold = t.hold(&base(&fx), "hold");
        let stalls = stand_in(&base(&fx), "stalls", &hold.stalls());
        let tools = Tools {
            ffmpeg: stalls.clone(),
            ffprobe: stalls,
        };
        let abs = fx.file("hangs.mp4", b"whatever is in it");
        let short = Limits {
            probe: Duration::from_millis(300),
            ..Limits::PREVIEW
        };
        let budget = Budget::new(short).with_tools(tools);
        let started = std::time::Instant::now();
        let p = fx
            .preview_with(&abs, &budget, &AtomicBool::new(false))
            .unwrap();
        assert_eq!(card_why(&p), "ffprobe took over 0.3 s: Enter opens it");
        assert!(started.elapsed() < Duration::from_secs(5));
        assert_none_left(&base(&fx));
    });
}

/// P4: the bar leaves a remote video while its first frame is being decoded through its stream
/// (an ffmpeg that never gives one): the preparation ends cancelled, its ffmpeg is killed and
/// reaped, and nothing is on disk.
#[test]
fn a_cancel_during_the_first_frame_stops_its_ffmpeg() {
    let Some(real) = media_tools() else { return };
    if SftpRemote::local_server().is_none() {
        return;
    }
    bounded(180, move |t| {
        let mut fx = remote().unwrap();
        t.also(&base(&fx));
        let video = fx.root.join("clip.mp4");
        clip(&real, &video, (160, 120));
        let abs = video.to_str().unwrap().to_string();
        let hold = t.hold(&base(&fx), "hold");
        // The stand-in says it runs (a shell builtin: still one process) before it waits: a run
        // on a URL names no file a count could find.
        let decoding = base(&fx).join("decoding");
        let body = format!(": > '{}'\n{}", decoding.display(), hold.stalls());
        let tools = Tools {
            ffmpeg: stand_in(&base(&fx), "stalls", &body),
            ffprobe: real.ffprobe,
        };
        let budget = budget_on(&t.hand(), tools);
        let cancel = AtomicBool::new(false);
        let result = std::thread::scope(|scope| {
            // The bar, on a thread of its own: it leaves once the frame's decode runs (the
            // probe, then the decode).
            let bar = scope.spawn(|| {
                until("the decode starts", || decoding.exists());
                cancel.store(true, Ordering::SeqCst);
            });
            let result = fx.preview_with(&abs, &budget, &cancel);
            bar.join().unwrap();
            result
        });
        assert!(
            result.as_ref().is_err_and(crate::remote::is_cancelled),
            "{:?}",
            result.as_ref().err()
        );
        assert_none_left(&base(&fx));
        assert!(fx.cache_files().is_empty());
    });
}

/// 02 §4.4: Enter while a video is still being probed lifts the budget — the same preparation
/// goes on past the probe's time (the test's clock says 10 s), as the viewer's opening, until
/// the probe answers; a budget that still holds stops the same probe at its 5 s.
#[test]
fn a_lifted_budget_lets_a_probe_go_on_past_its_time() {
    let Some(real) = media_tools() else { return };
    for lifted in [false, true] {
        let real = real.clone();
        bounded(180, move |t| {
            let mut fx = local();
            t.also(&base(&fx));
            t.also(&base(&fx));
            let video = fx.root.join("clip.mp4");
            clip(&real, &video, (160, 120));
            let hold = t.hold(&base(&fx), "hold");
            let tools = Tools {
                ffmpeg: real.ffmpeg,
                ffprobe: stand_in(&base(&fx), "held-ffprobe", &hold.then(&real.ffprobe)),
            };
            let abs = video.to_str().unwrap().to_string();
            let hand = t.hand();
            let budget = budget_on(&hand, tools);
            if lifted {
                budget.lift();
            }
            let root = fx.root.clone();
            let p = std::thread::scope(|scope| {
                let clock = scope.spawn(|| {
                    until("the probe starts", || !started_under(&root).is_empty());
                    hand.advance(Duration::from_secs(10));
                    std::thread::sleep(Duration::from_millis(100));
                    let running = still_running(&started_under(&root));
                    if lifted {
                        hold.release();
                    }
                    running
                });
                let p = fx.preview_with(&abs, &budget, &AtomicBool::new(false));
                let running = clock.join().unwrap();
                assert_eq!(running, usize::from(lifted), "10 s on the clock");
                p.unwrap()
            });
            if lifted {
                assert!(p.card().is_none(), "{}", card_why(&p));
                assert!(p.hand_over().is_ok(), "the viewer");
            } else {
                assert_eq!(card_why(&p), "ffprobe took over 5 s: Enter opens it");
            }
            assert_none_left(&base(&fx));
        });
    }
}

/// A card in a small side (a 480×320 window's): the reason is whole inside the side — it comes
/// before what it is about, and that part gives way first.
#[test]
fn a_cards_reason_is_never_cut_in_a_small_side() {
    if media_tools().is_none() {
        return;
    }
    let mut fx = local();
    let deep = fx
        .root
        .join("a folder with a long name/and another one inside it");
    std::fs::create_dir_all(&deep).unwrap();
    let file = deep.join("a clip with a long name.mp4");
    std::fs::write(&file, b"not really media").unwrap();
    let mut p = fx.preview(file.to_str().unwrap());
    let why = card_why(&p);
    assert_eq!(why, "Invalid data found when processing input");
    let mut drv = Driver::new(236.0, 230.0);
    shown(&mut drv, &mut Host(&mut p.embedded.window));
    assert!(
        drv.find_text(&why).is_some(),
        "the reason, whole, inside the side"
    );
    // In a side with room, what it is about is there too, under it.
    let mut drv = Driver::new(500.0, 420.0);
    shown(&mut drv, &mut Host(&mut p.embedded.window));
    let reason = drv.find_text(&why).expect("the reason");
    let about = drv
        .texts_starting_with("ffprobe cannot read ")
        .into_iter()
        .next()
        .expect("what it is about");
    assert!(about.0.top() >= reason.bottom() - 1.0, "under the reason");
    assert!(
        about.1.ends_with("a clip with a long name.mp4"),
        "{}",
        about.1
    );
}

/// A remote file's card names the file as the owner knows it — `host:/abs` — never the URL its
/// byte server reads it through; and a remote image over the read limit, of which nothing is
/// read, still says its kind (by its name).
#[test]
fn a_remote_card_names_the_remote_file_and_says_its_kind() {
    let Some(mut fx) = remote() else { return };
    if media_tools().is_some() {
        let abs = fx.file("broken.mp4", b"not really media");
        let p = fx.preview(&abs);
        let card = p.card().expect("a card");
        assert_eq!(card.why, "Invalid data found when processing input");
        assert_eq!(
            card.what.as_deref(),
            Some(format!("ffprobe cannot read box:{abs}").as_str())
        );
    }
    let big = fx.file("big.png", &vec![7u8; 200 << 10]);
    let limits = Limits {
        read: 100 << 10,
        ..Limits::PREVIEW
    };
    let p = fx.preview_limited(&big, limits);
    assert_eq!(card_why(&p), "over the preview limit: Enter opens it");
    assert_eq!(p.card().unwrap().kind, Some(Kind::Image), "by its name");
    assert_eq!(fx.counts.heads.load(Ordering::Relaxed), 0, "nothing read");
    let mut p = p;
    let mut drv = Driver::new(500.0, 400.0);
    shown(&mut drv, &mut Host(&mut p.embedded.window));
    assert!(drv.find_text("Kind").is_some(), "the card's Kind row");
}
