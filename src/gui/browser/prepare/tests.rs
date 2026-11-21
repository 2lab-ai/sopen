//! The preview's preparation (02 §4.3–§4.4) against a real SFTP server (this machine's own
//! `sftp-server` over pipes, posing as host `box`) and the local disk: it reads without claiming
//! the cache, keeps to its budgets — an info card when one is hit, before the bytes past it are
//! read — and uses the window's one preview connection. Remote text and what the hex window
//! shows read through a stream on that connection (06 §13 W8a): nothing is written to disk,
//! a cancelled preview gives the connection back, and the hand-over moves the stream to the
//! viewer. A remote image is still read into a private copy, published at the hand-over by a
//! hard link; such a viewer's own session opens on its first remote call. Enter's own
//! preparation is at the end.

use super::*;
use crate::cache::PreviewSpace;
use crate::gui::testkit::{command, Driver};
use crate::gui::Window;
use crate::remote::SftpRemote;
use eframe::egui::{self, Key};
use std::collections::BTreeMap;
use std::os::unix::fs::MetadataExt;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// What a test runs after a request: its name and path.
type After = Box<dyn FnMut(&str, &str) + Send>;

/// What crossed the connections a test opened. `content` counts the bytes a reader accepted;
/// `responses` is what the server sent, one entry per SFTP read response as it reached the reader
/// (`read_to`'s sink writes each response once, whole) — also bytes the reader then refused —
/// with the range of the file it holds, decoded from its bytes ([`stamped`] files say where every
/// byte is: never inferred from what came before). `ops` logs every request made, in order.
#[derive(Default)]
struct Counts {
    content: AtomicU64,
    heads: AtomicU64,
    reads: Mutex<Vec<(String, u64)>>,
    responses: Mutex<Vec<(String, Option<u64>, u64)>>,
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
    /// Called on the first read response, before the reader gets it (tests: hold the read).
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

    /// The read responses for `path`, `(offset, bytes)`.
    fn responses(&self, path: &str) -> Vec<(Option<u64>, u64)> {
        let all = self.responses.lock().unwrap();
        all.iter()
            .filter(|(p, _, _)| p == path)
            .map(|&(_, at, n)| (at, n))
            .collect()
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

/// What a reader gets: each write is one SFTP read response, recorded with its range at the
/// boundary, held at the gate the first time, then handed on.
struct CountingWriter<'a> {
    out: &'a mut dyn std::io::Write,
    counts: &'a Counts,
    path: &'a str,
    n: u64,
}

impl std::io::Write for CountingWriter<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = self.out.write(buf)?;
        self.n += n as u64;
        Ok(n)
    }
    fn write_all(&mut self, buf: &[u8]) -> std::io::Result<()> {
        self.counts.responses.lock().unwrap().push((
            self.path.to_string(),
            source_of(buf),
            buf.len() as u64,
        ));
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
            path: p,
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

/// A folder of files on "host `box`" (this machine's SFTP server), a cache, the window's preview
/// space, and a `Connect` that counts the sessions it opens and what is read over them.
struct Fx {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    cache: PathBuf,
    space: PreviewSpace,
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
    let space = PreviewSpace::new(&cache).unwrap();
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
        space,
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
            space: &self.space,
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

    /// Every file under the cache outside the preview root, with its content: what a preview
    /// must not change.
    fn cache_files(&self) -> BTreeMap<PathBuf, Vec<u8>> {
        fn walk(dir: &Path, skip: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
            let Ok(entries) = std::fs::read_dir(dir) else {
                return;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.starts_with(skip) {
                    continue;
                }
                if path.is_dir() {
                    walk(&path, skip, out);
                } else {
                    out.insert(path.clone(), std::fs::read(&path).unwrap_or_default());
                }
            }
        }
        let mut out = BTreeMap::new();
        walk(&self.cache, &self.cache.join(".preview"), &mut out);
        out
    }

    /// The names in the window's preview folder.
    fn preview_copies(&self) -> Vec<PathBuf> {
        let mut names: Vec<_> = std::fs::read_dir(self.space.dir())
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .collect();
        names.sort();
        names
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
    crosses::<Reuse>();
}

/// A remote preview that reads a copy (an image) never touches the cache: it reads the file
/// into a private copy of the window's (02 §4.4) — also with an earlier session's copy of the
/// same file in the cache, holding an edit it never saved — and dropping it removes the copy.
/// Only a hand-over claims: then the edit is moved aside, and said so, as Enter's fetch does.
#[test]
fn a_remote_preview_reads_without_claiming_the_cache() {
    let Some(mut fx) = remote() else { return };
    let bytes = png(64, 48);
    let abs = fx.file("photo.png", &bytes);
    // An earlier session's copy: recorded, then edited and never saved.
    crate::cache::seed_copy(&fx.cache, "box", &abs, b"fetched", b"an unsaved edit");
    let before = fx.cache_files();
    let p = fx.preview(&abs);
    assert!(p.card().is_none(), "{}", card_why(&p));
    assert_eq!(fx.cache_files(), before, "the cache is as it was");
    let copies = fx.preview_copies();
    assert_eq!(copies.len(), 1, "{copies:?}");
    assert_eq!(std::fs::read(&copies[0]).unwrap(), bytes);
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(&copies[0]).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
    let mut p = p;
    let mut drv = Driver::new(500.0, 300.0);
    shown(&mut drv, &mut Host(&mut p.embedded.window));
    assert_eq!(drv.pictures().len(), 1, "the picture is drawn");
    drop(p);
    assert!(
        fx.preview_copies().is_empty(),
        "the copy goes with the preview"
    );
    assert_eq!(fx.cache_files(), before);
    let handed = fx.preview(&abs).hand_over().ok().expect("a viewer");
    assert_eq!(handed.notices.len(), 1, "{:?}", handed.notices);
    assert!(
        handed.notices[0].contains("unsaved edits"),
        "{:?}",
        handed.notices
    );
    let cached = crate::cache::cache_path(&fx.cache, "box", &abs);
    assert_eq!(std::fs::read(&cached).unwrap(), bytes);
}

/// A streamed remote preview (text, 06 §13) writes nothing at all: no private copy, nothing in
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
    assert!(fx.preview_copies().is_empty(), "{:?}", fx.preview_copies());
    assert_eq!(fx.cache_files(), before, "the cache is as it was");
    assert_eq!(whole_reads(&fx), Vec::<String>::new());
    let mut handed = p.hand_over().ok().expect("a viewer");
    assert!(handed.hold.is_none(), "a stream claims nothing");
    assert!(handed.notices.is_empty(), "{:?}", handed.notices);
    shown(&mut drv, &mut Host(&mut handed.embedded.window));
    assert!(drv.find_text("what the remote holds").is_some());
    assert_eq!(fx.cache_files(), before, "nothing is published");
    assert!(fx.preview_copies().is_empty());
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
    assert!(fx.preview_copies().is_empty());
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
    assert!(fx.preview_copies().is_empty());
    assert!(fx.cache_files().is_empty());
    // Remote text over 16 MiB is not streamed before W5b (06 §7.2): past the read budget it is
    // the card, decided from the stat and its name — nothing read.
    let log = fx.file("huge.log", b"");
    std::fs::File::options()
        .write(true)
        .open(&log)
        .unwrap()
        .set_len(40 << 20)
        .unwrap();
    drop(p);
    let heads = fx.counts.heads.load(Ordering::Relaxed);
    let p = fx.preview(&log);
    assert_eq!(card_why(&p), "over the preview limit: Enter opens it");
    assert_eq!(fx.counts.heads.load(Ordering::Relaxed), heads);
    assert_eq!(whole_reads(&fx), Vec::<String>::new());
    assert_eq!(fx.sessions.load(Ordering::SeqCst), 1, "one preview session");
}

/// P3: a remote file that grows past the budget while it is read is cut at the budget (plus the
/// one response that crossed it) and shows the card; its partial copy is gone.
#[test]
fn a_remote_read_stops_at_the_budget() {
    let Some(mut fx) = remote() else { return };
    // More than one response at the stat (a short read would make the SFTP client skip what
    // the file grows by: it moves its offset by what it asked for).
    // (An image by its name: the file is read into a copy, and the card comes before any
    // decoding.)
    let abs = fx.file("growing.png", &stamped(400 << 10));
    let limits = Limits {
        read: 500 << 10,
        ..Limits::PREVIEW
    };
    let budget = Budget::new(limits);
    let path = PathBuf::from(&abs);
    let req = PreviewRequest {
        conn: &mut fx.conn,
        connect: &fx.connect,
        label: fx.label,
        abs: &abs,
        cache: &fx.cache,
        space: &fx.space,
        back: &fx.back,
        wake: &fx.wake,
        began: std::time::Instant::now(),
    };
    let mut grown = false;
    let p = preview(req, &budget, &AtomicBool::new(false), &mut |_, _| {
        if !grown {
            // Grown while being read (the stat said 400 KiB).
            std::fs::write(&path, stamped(8 << 20)).unwrap();
            grown = true;
        }
    })
    .unwrap()
    .show();
    assert_eq!(card_why(&p), "over the preview limit: Enter opens it");
    // At the read boundary: what the server sent, response by response, read once, in order —
    // and taken no further than the response that crossed the budget. (The SFTP reader keeps
    // requests in flight, at most 4 MiB, 06 §2: what was already sent past that response
    // arrives and is dropped, never taken.)
    let responses = fx.counts.responses(&abs);
    assert_ranges_once(&responses);
    let sent: u64 = responses.iter().map(|r| r.1).sum();
    assert!(
        sent <= (500 << 10) + SFTP_RESPONSE,
        "the server sent {sent} bytes"
    );
    assert_eq!(fx.counts.reads.lock().unwrap().len(), 1, "one read");
    // What the copy took: never past the budget.
    let taken = fx.counts.content.load(Ordering::Relaxed);
    assert!(taken <= 500 << 10, "the copy took {taken} bytes");
    assert!(fx.preview_copies().is_empty(), "{:?}", fx.preview_copies());
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

fn assert_ranges_once(responses: &[(Option<u64>, u64)]) {
    if let Err(why) = ranges_once(responses) {
        panic!("{why}: {responses:?}");
    }
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
/// encoded budget, a preparation past its deadline, video and audio show their cards. The card
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
    for (name, kind) in [("clip.mp4", "video"), ("song.mp3", "audio")] {
        let abs = fx.file(name, b"not really media");
        let p = fx.preview(&abs);
        assert_eq!(
            card_why(&p),
            format!("no preview of {kind} yet: Enter opens it")
        );
        assert_eq!(p.card().unwrap().kind.map(Kind::as_str), Some(kind));
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
    let p = fx.preview(&abs);
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

/// P6 / P12 (remote): handed over, a remote preview's copy is published into the cache by a hard
/// link — the same inode — and only then claimed (locked) and recorded; the file was read once,
/// on the preview connection; the viewer's own session opens on its first remote call (here its
/// Save copy), and the preview name goes when the viewer does.
#[test]
fn a_handed_over_remote_preview_is_published_by_a_hard_link_and_reconnects_on_use() {
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
    let copy = fx.preview_copies().remove(0);
    let cached = crate::cache::cache_path(&fx.cache, "box", &abs);
    assert!(!cached.exists(), "nothing published before the hand-over");
    // Before the hand-over the preview holds nothing: no session has ever asked for the copy's
    // lock, so its lock file is not there. (Not shown by claiming it here: a process a parallel
    // test forks while that claim is held keeps the lock's descriptor until its exec, and the
    // hand-over's own claim then finds the copy held and takes a session copy.)
    let lock = fx
        .cache
        .join(".meta/lock")
        .join(cached.strip_prefix(&fx.cache).unwrap());
    assert!(!lock.exists(), "not claimed by the preview");
    let mut handed = p.hand_over().ok().expect("a viewer");
    assert!(lock.exists(), "claimed by the hand-over");
    assert!(handed.notices.is_empty(), "{:?}", handed.notices);
    let hold = handed.hold.take().expect("claimed");
    assert_eq!(hold.local, cached);
    let ino = |p: &Path| std::fs::metadata(p).unwrap().ino();
    assert_eq!(ino(&cached), ino(&copy), "the same inode");
    let record = fx
        .cache
        .join(".meta/record")
        .join(cached.strip_prefix(&fx.cache).unwrap());
    let sha = crate::cache::hex(&crate::app::file_hash(&cached).unwrap());
    assert_eq!(std::fs::read_to_string(record).unwrap().trim(), sha);
    assert_eq!(
        *fx.counts.reads.lock().unwrap(),
        vec![(abs.clone(), bytes.len() as u64)],
        "one read, of the whole file"
    );
    assert_eq!(fx.sessions.load(Ordering::SeqCst), 1, "no session yet");
    // Held from the hand-over on: another session gets a copy of its own.
    let other = crate::cache::claim(&fx.cache, "box", &abs).unwrap();
    assert!(other.note.is_some(), "held by the viewer");
    assert_ne!(other.local, cached);
    drop(other);
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
    assert_eq!(
        fx.counts.reads.lock().unwrap().len(),
        1,
        "nothing read again"
    );
    let crate::gui::Embedded { window, mut settle } = handed.embedded;
    drop(window);
    assert!(
        copy.exists(),
        "the viewer's name lives while it may still write"
    );
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    while settle(true).is_none() {
        assert!(
            std::time::Instant::now() < deadline,
            "the viewer never settled"
        );
    }
    drop(settle);
    assert!(!copy.exists(), "the preview name goes with the viewer");
    assert!(cached.exists(), "the cached copy stays");
    assert_eq!(
        crate::gui::image::decodes(),
        decoded + 1,
        "decoded once: for the preview, never again"
    );
    drop(hold);
    // Released with the viewer: the next session gets the shared copy again. (A process forked
    // by a parallel test can hold the lock's descriptor for a moment: asked again until then.)
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let other = crate::cache::claim(&fx.cache, "box", &abs).unwrap();
        if other.note.is_none() {
            assert_eq!(other.local, cached);
            break;
        }
        assert!(std::time::Instant::now() < deadline, "never released");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// P4: twenty quick moves — each preview cancelled at once, mid-read, or after it is ready and
/// dropped — use the window's one preview connection, leave no copy or partial copy, and leave
/// the cache's mirror and records as they were (empty).
#[test]
fn twenty_quick_moves_use_one_connection_and_leave_nothing() {
    let Some(mut fx) = remote() else { return };
    // Images: read into a private copy (text and binaries stream, 06 §13: their twenty moves
    // are `w8a_twenty_quick_moves_over_streams_use_one_session`).
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
            space: &fx.space,
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
    assert!(fx.preview_copies().is_empty(), "{:?}", fx.preview_copies());
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
    let mut origin = placeholder_origin(
        &fx.connect,
        "box",
        conn.as_ref(),
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

/// Enter on an info card whose private copy is complete and unchanged publishes that copy into
/// the cache instead of reading the file again (02 §4.4); a file changed since is fetched anew.
#[test]
fn enter_on_a_card_reuses_its_complete_unchanged_copy() {
    let Some(mut fx) = remote() else { return };
    let abs = fx.file("photo.png", &png(64, 48));
    let few_pixels = Limits {
        pixels: 1000,
        ..Limits::PREVIEW
    };
    let p = fx.preview_limited(&abs, few_pixels);
    assert_eq!(card_why(&p), "image is 64×48; Enter opens it");
    let copy = fx.preview_copies().remove(0);
    let copy_ino = std::fs::metadata(&copy).unwrap().ino();
    let reads = fx.counts.reads.lock().unwrap().len();
    let reuse = p.into_reuse().expect("a complete copy");
    let opened = prepare_reusing(
        &fx.request(&abs),
        reuse,
        &AtomicBool::new(false),
        &mut |_, _| {},
    )
    .unwrap();
    let Prepared::Image { claim, .. } = opened else {
        panic!("not an image")
    };
    let claim = claim.expect("claimed");
    assert_eq!(
        std::fs::metadata(&claim.local).unwrap().ino(),
        copy_ino,
        "published, not copied"
    );
    assert!(!copy.exists(), "its preview name is gone");
    assert_eq!(
        fx.counts.reads.lock().unwrap().len(),
        reads,
        "not read again"
    );
    drop(claim);
    // Changed since its preview: read again.
    let p = fx.preview_limited(&abs, few_pixels);
    let reuse = p.into_reuse().unwrap();
    std::fs::write(&abs, png(32, 32)).unwrap();
    let opened = prepare_reusing(
        &fx.request(&abs),
        reuse,
        &AtomicBool::new(false),
        &mut |_, _| {},
    )
    .unwrap();
    let Prepared::Image { doc, .. } = opened else {
        panic!("not an image")
    };
    assert_eq!(doc.size(), (32, 32), "the file as it is now");
}

/// Lifted (Enter while the preview is still being prepared, 02 §4.4), the preparation goes on
/// under the viewer's rules: Enter comes while the read is under way — after its first response
/// arrived — and that same read goes on to the end (one read, every range once), then the text
/// is indexed past the text budget, all of it within the preparation; no card.
#[test]
fn a_lifted_budget_lets_the_preparation_go_on() {
    let Some(mut fx) = remote() else { return };
    // Over 16 MiB: remote text that is still read into a copy (06 §7.2, until W5b).
    let body = stamped(TEXT_STREAM_MAX + (1 << 20));
    let abs = fx.file("long.txt", &body);
    let budget = Budget::new(Limits {
        text: 64 << 10,
        ..Limits::PREVIEW
    });
    let (lift, reached) = (budget.clone(), Arc::new(AtomicBool::new(false)));
    let seen = reached.clone();
    *fx.counts.gate.lock().unwrap() = Some(Box::new(move || {
        seen.store(true, Ordering::SeqCst);
        lift.lift();
    }));
    let p = fx
        .preview_with(&abs, &budget, &AtomicBool::new(false))
        .unwrap();
    assert!(reached.load(Ordering::SeqCst), "lifted mid-read");
    assert!(p.card().is_none(), "{}", card_why(&p));
    let responses = fx.counts.responses(&abs);
    assert!(responses.len() > 1, "lifted with the read under way");
    assert_ranges_once(&responses);
    assert_eq!(
        responses.iter().map(|r| r.1).sum::<u64>(),
        body.len() as u64
    );
    assert_eq!(
        fx.counts.reads.lock().unwrap().len(),
        1,
        "not started again"
    );
    let copy = fx.preview_copies().remove(0);
    let indexed: u64 = crate::gui::text::index_reads(&copy)
        .iter()
        .map(|r| r.1)
        .sum();
    assert_eq!(indexed, body.len() as u64, "indexed past the text budget");
    let mut handed = p.hand_over().ok().unwrap();
    let mut drv = Driver::new(600.0, 400.0);
    let mut win = Host(&mut handed.embedded.window);
    shown(&mut drv, &mut win);
    drv.key(&mut win, Key::End, command(false));
    drv.idle(&mut win, 2);
    let last = format!("@{:014x}", body.len() as u64 - 16);
    assert!(drv.find_text(&last).is_some(), "the whole file");
    assert_eq!(fx.counts.content.load(Ordering::Relaxed), body.len() as u64);
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
    assert!(fx.preview_copies().is_empty());
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

/// Text that grows past 16 MiB between its stat and its open is not shown through a capped
/// index (06 §7.2): the handle's size decides too. As a preview it is the card, nothing of it
/// read — and its stream gave the session back: the next preview uses it. By Enter it is
/// fetched whole, on a connection of its own.
#[test]
fn w8a_text_grown_past_16_mib_between_stat_and_open_is_not_streamed() {
    let Some(mut fx) = remote() else { return };
    let grown = TEXT_STREAM_MAX + 1;
    let grows_after_its_stat = |fx: &Fx, abs: &str| {
        let (path, mut armed) = (abs.to_string(), true);
        *fx.counts.after.lock().unwrap() = Some(Box::new(move |op, p| {
            if op == "stat" && p == path && std::mem::take(&mut armed) {
                let file = std::fs::File::options().write(true).open(&path).unwrap();
                file.set_len(grown).unwrap();
            }
        }));
    };
    let abs = fx.file("grows.txt", b"small when it was stat'ed\n");
    let other = fx.file("small.txt", b"another file\n");
    grows_after_its_stat(&fx, &abs);
    let p = fx.preview(&abs);
    assert_eq!(card_why(&p), OVER_LIMIT);
    assert!(fx.counts.ops().contains(&format!("into_ranged {abs}")));
    assert!(stream_reads(&fx, &abs).is_empty(), "nothing of it was read");
    assert_eq!(whole_reads(&fx), Vec::<String>::new());
    drop(p);
    let mut p = fx.preview(&other);
    assert!(p.card().is_none(), "{}", card_why(&p));
    let mut drv = Driver::new(500.0, 300.0);
    shown(&mut drv, &mut Host(&mut p.embedded.window));
    assert!(drv.find_text("another file").is_some());
    assert_eq!(
        fx.sessions.load(Ordering::SeqCst),
        1,
        "the rejected stream gave the preview session back"
    );
    assert_eq!(fx.counts.peak.load(Ordering::SeqCst), 1);
    assert!(fx.counts.ops().contains(&format!("into_connection {abs}")));
    drop(p);
    // Enter on such a file: the whole fetch.
    let abs = fx.file("grows-too.txt", b"small when it was stat'ed\n");
    grows_after_its_stat(&fx, &abs);
    let sessions = fx.sessions.load(Ordering::SeqCst);
    let prepared = prepare(&fx.request(&abs), &AtomicBool::new(false), &mut |_, _| {}).unwrap();
    let Prepared::Text { doc, claim, .. } = prepared else {
        panic!("not the text viewer")
    };
    assert!(!doc.is_stream(), "not through a capped index");
    assert_eq!(doc.size(), grown, "indexed whole");
    assert_eq!(
        std::fs::metadata(&claim.expect("fetched").local)
            .unwrap()
            .len(),
        grown
    );
    assert_eq!(whole_reads(&fx), vec![format!("read_to {abs}")]);
    assert_eq!(
        fx.sessions.load(Ordering::SeqCst),
        sessions + 2,
        "the stream's connection, then the fetch's own"
    );
    assert!(stream_reads(&fx, &abs).is_empty());
}

/// P12 for remote text over 16 MiB, which is still read into a copy (06 §7.2, until W5b): a
/// 17 MiB file is fetched once, into the preview's copy, which the preview indexes to 4 MiB;
/// handed over, the copy is the cached one (claimed by its hard link: the same inode) and the
/// index goes on through the copy's name from 4 MiB — the remote is not read again, no byte is
/// indexed twice — and End shows the last line.
#[test]
fn a_handed_over_remote_text_over_16_mib_resumes_through_its_copy() {
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
    assert!(stream_reads(&fx, &abs).is_empty(), "not a stream");
    let copy = fx.preview_copies().remove(0);
    let indexed = |path: &Path| {
        crate::gui::text::index_reads(path)
            .iter()
            .map(|r| r.1)
            .sum::<u64>()
    };
    assert_eq!(indexed(&copy), 4 << 20, "the preview indexed 4 MiB");
    let mut handed = p.hand_over().ok().expect("a viewer");
    let hold = handed.hold.take().expect("claimed");
    assert_eq!(
        std::fs::metadata(&hold.local).unwrap().ino(),
        std::fs::metadata(&copy).unwrap().ino(),
        "published by a hard link"
    );
    let mut drv = Driver::new(600.0, 400.0);
    let mut win = Host(&mut handed.embedded.window);
    drv.until(&mut win, 30, |_| indexed(&copy) >= size);
    shown(&mut drv, &mut win);
    drv.key(&mut win, Key::End, command(false));
    drv.idle(&mut win, 2);
    assert!(drv
        .text_starting_with(&format!("{:08} ", lines - 1))
        .is_some());
    let mut at = 0;
    for (offset, n) in crate::gui::text::index_reads(&copy) {
        assert_eq!(offset, at, "in order, nothing twice");
        at += n;
    }
    assert_eq!(at, size);
    assert_eq!(
        *fx.counts.reads.lock().unwrap(),
        vec![(abs.clone(), size)],
        "one fetch"
    );
}

/// A remote file over the read budget whose name says nothing and whose bytes are text: the
/// sniff (at most 8 KiB) says so, text over 16 MiB does not stream, and it is the card —
/// nothing more of its 33 MiB read, whole or by range.
#[test]
fn w8a_a_nameless_text_file_over_the_read_budget_is_its_card_after_the_sniff() {
    let Some(mut fx) = remote() else { return };
    let body = "a printable line of a file with no extension\n".repeat(1 << 20);
    let bytes = &body.as_bytes()[..(33 << 20)];
    let abs = fx.file("README", bytes);
    let p = fx.preview(&abs);
    assert_eq!(card_why(&p), OVER_LIMIT);
    assert_eq!(
        p.card().unwrap().kind,
        Some(Kind::Text),
        "the sniff said text"
    );
    let sniffed = fx.counts.heads.load(Ordering::Relaxed);
    assert!(sniffed > 0 && sniffed <= 8 << 10, "{sniffed}");
    assert_eq!(whole_reads(&fx), Vec::<String>::new());
    assert!(
        fx.counts.ranged.lock().unwrap().is_empty(),
        "no stream read"
    );
    assert!(!fx
        .counts
        .ops()
        .iter()
        .any(|op| op.starts_with("into_ranged")));
    assert!(fx.preview_copies().is_empty());
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
    assert!(fx.preview_copies().is_empty(), "{:?}", fx.preview_copies());
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
/// sniff, then the stream's open (text: 06 §13) or the read's own stat and the read (an image,
/// read into a copy) — no later request is made. On a dead session, a cancel raised inside the
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
    let copied = [
        "connect ",
        "lstat {link}",
        "stat {link}",
        "realpath {link}",
        "stat {data}",
        "stat {data}",
        "read_to {data}",
    ];
    for (name, steps) in [("data", &streamed), ("data.png", &copied)] {
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
            assert!(fx.preview_copies().is_empty());
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

/// P4: a cancel raised by the window while a read is in flight — after its first response has
/// arrived — ends the read at that response: the server is asked for nothing more, far less than
/// the file is sent, the result is a cancel and the partial copy is gone. (That only one worker
/// runs at a time is the preview host's to show.)
#[test]
fn a_cancel_during_a_read_stops_its_requests() {
    let Some(mut fx) = remote() else { return };
    let abs = fx.file("big.png", &stamped(4 << 20));
    let (reached_tx, reached) = std::sync::mpsc::channel::<()>();
    let (go, go_rx) = std::sync::mpsc::channel::<()>();
    // The worker waits at the gate a bounded time (a test that failed never leaves it hanging),
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
    let responses = fx.counts.responses(&abs);
    assert_eq!(
        responses.len(),
        1,
        "nothing asked after the cancel: {responses:?}"
    );
    let sent: u64 = responses.iter().map(|r| r.1).sum();
    // One response at most, a fraction of the file's 4 MiB.
    assert!(sent <= SFTP_RESPONSE, "sent {sent}");
    assert_eq!(
        fx.counts.content.load(Ordering::Relaxed),
        0,
        "the copy took nothing"
    );
    assert!(fx.preview_copies().is_empty(), "{:?}", fx.preview_copies());
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

/// The 5 s count from the end of the remote read (02 §4.3: the read shows its progress and is
/// bounded by its bytes): a read that takes a minute leaves the preparation after it its whole
/// 5 s, and the preview shows.
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
        fx.counts.responses(&abs).len() > 1,
        "the read went on after the minute"
    );
}

/// One read budget for the whole preview (02 §4.3), however many times the read starts: a file
/// that changed during a complete first read is read again only if what is left of the budget
/// holds it — here it does not, and the card shows, the server having sent no more than the
/// budget and one response.
#[test]
fn a_read_started_again_shares_one_budget() {
    let Some(mut fx) = remote() else { return };
    let body = stamped(200 << 10);
    let abs = fx.file("touched.png", &body);
    let limits = Limits {
        read: 300 << 10,
        ..Limits::PREVIEW
    };
    let path = abs.clone();
    let mut armed = true;
    *fx.counts.after.lock().unwrap() = Some(Box::new(move |op, p| {
        if op == "read_to" && p == path && std::mem::take(&mut armed) {
            // Written to as the first read ended: its time moves.
            let f = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
            let then = f.metadata().unwrap().modified().unwrap();
            f.set_modified(then - Duration::from_secs(3600)).unwrap();
        }
    }));
    let p = fx.preview_limited(&abs, limits);
    assert_eq!(card_why(&p), OVER_LIMIT);
    let sent: u64 = fx.counts.responses(&abs).iter().map(|r| r.1).sum();
    assert!(
        sent <= limits.read + SFTP_RESPONSE,
        "the server sent {sent} bytes for a {} byte budget",
        limits.read
    );
    assert!(fx.preview_copies().is_empty(), "{:?}", fx.preview_copies());
}

/// P6 (remote, the read budget itself): Enter while the read is under way lifts the read budget
/// for that same read. The file was just under the budget at the stat that decided it and grew
/// before it was read; Enter comes at the first (full-size) response, and the whole larger file
/// arrives in that one read — past the budget, every range once, not started again — and shows.
/// Without Enter the same read stops at the response that crossed the budget: the card.
#[test]
fn a_read_lifted_mid_way_goes_past_its_read_budget() {
    let Some(mut fx) = remote() else { return };
    // Over 16 MiB throughout: remote text that is still read into a copy (06 §7.2, until W5b).
    let cap = TEXT_STREAM_MAX + (1 << 20);
    let grown = stamped(cap + (1 << 20));
    for lift in [true, false] {
        let abs = fx.file(&format!("grown-{lift}.txt"), &stamped(cap - (10 << 10)));
        let (path, bigger) = (abs.clone(), grown.clone());
        let mut armed = true;
        *fx.counts.after.lock().unwrap() = Some(Box::new(move |op, p| {
            // After the stat that decided it fits.
            if op == "stat" && p == path && std::mem::take(&mut armed) {
                std::fs::write(&path, &bigger).unwrap();
            }
        }));
        let budget = Budget::new(Limits {
            read: cap,
            ..Limits::PREVIEW
        });
        if lift {
            let enter = budget.clone();
            *fx.counts.gate.lock().unwrap() = Some(Box::new(move || enter.lift()));
        }
        let reads = fx.counts.reads.lock().unwrap().len();
        let p = fx
            .preview_with(&abs, &budget, &AtomicBool::new(false))
            .unwrap();
        let responses = fx.counts.responses(&abs);
        assert_eq!(
            responses.first().map(|r| r.1),
            Some(SFTP_RESPONSE),
            "the first response is full-size — the length asked, no short read: {responses:?}"
        );
        // Enter (the gate, at the first response) came before the read crossed its budget; the
        // lifted read then crosses it.
        assert!(
            SFTP_RESPONSE < cap,
            "the first response alone is under the budget"
        );
        assert_ranges_once(&responses);
        assert_eq!(fx.counts.reads.lock().unwrap().len(), reads + 1, "one read");
        let sent: u64 = responses.iter().map(|r| r.1).sum();
        if lift {
            assert!(p.card().is_none(), "{}", card_why(&p));
            assert_eq!(sent, grown.len() as u64, "the whole larger file");
            assert!(sent > cap, "past the read budget");
            let copy = fx.preview_copies().remove(0);
            assert_eq!(std::fs::read(copy).unwrap(), grown);
        } else {
            assert_eq!(card_why(&p), OVER_LIMIT);
            assert!(sent <= cap + SFTP_RESPONSE, "the server sent {sent} bytes");
            assert!(fx.preview_copies().is_empty());
        }
    }
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

/// The browser's Enter on remote text: a file of at most 16 MiB opens through a stream (06
/// §7.2 W5a: its index is then whole, as today) — no whole read, nothing in the cache; one byte
/// more and it keeps today's route, fetched into the cache, until W5b (a capped index would
/// show the first 16 MiB as the whole file).
#[test]
fn w8a_enter_on_remote_text_streams_up_to_16_mib_and_fetches_above() {
    let Some(probe) = remote() else { return };
    drop(probe);
    let cap = crate::gui::text::HIGHLIGHT_MAX;
    for (size, streams) in [(300 << 10, true), (cap, true), (cap + 1, false)] {
        let fx = remote().unwrap();
        let mut body = b"the first line\n".to_vec();
        body.resize(size as usize - 1, b'x');
        body.push(b'\n');
        let abs = fx.file("notes.txt", &body);
        let prepared = prepare(&fx.request(&abs), &AtomicBool::new(false), &mut |_, _| {}).unwrap();
        let Prepared::Text { doc, claim, .. } = prepared else {
            panic!("{size}: not the text viewer")
        };
        assert_eq!(doc.is_stream(), streams, "{size} bytes");
        assert_eq!(claim.is_none(), streams, "{size} bytes: the cache");
        assert_eq!(
            whole_reads(&fx).len(),
            usize::from(!streams),
            "{size} bytes"
        );
        assert_eq!(fx.sessions.load(Ordering::SeqCst), 1, "{size} bytes");
        if !streams {
            continue;
        }
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
        assert_eq!(whole_reads(&fx), Vec::<String>::new());
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
