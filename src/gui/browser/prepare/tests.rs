//! The preview's preparation (02 §4.3–§4.4) against a real SFTP server (this machine's own
//! `sftp-server` over pipes, posing as host `box`) and the local disk: it reads without claiming
//! the cache, keeps to its budgets — an info card when one is hit, before the bytes past it are
//! read — uses the window's one preview connection, and hands over by publishing its copy with a
//! hard link; the viewer's own session opens on its first remote call.

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
/// (`read_to`'s sink writes each response once, whole), with its range in the file — the read
/// boundary, also for bytes the reader then refused. `ops` logs every request made, in order.
#[derive(Default)]
struct Counts {
    content: AtomicU64,
    heads: AtomicU64,
    reads: Mutex<Vec<(String, u64)>>,
    responses: Mutex<Vec<(String, u64, u64)>>,
    ops: Mutex<Vec<String>>,
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
    fn responses(&self, path: &str) -> Vec<(u64, u64)> {
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
    inner: Box<dyn RemoteFs + Send>,
    counts: Arc<Counts>,
    dead: bool,
}

impl Counting {
    fn new(inner: Box<dyn RemoteFs + Send>, counts: &Arc<Counts>) -> Self {
        let live = counts.live.fetch_add(1, Ordering::SeqCst) + 1;
        counts.peak.fetch_max(live, Ordering::SeqCst);
        Self {
            inner,
            counts: counts.clone(),
            dead: false,
        }
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

impl Drop for Counting {
    fn drop(&mut self) {
        self.counts.live.fetch_sub(1, Ordering::SeqCst);
    }
}

/// What a reader gets: each write is one SFTP read response, recorded with its range at the
/// boundary, held at the gate the first time, then handed on.
struct CountingWriter<'a> {
    out: &'a mut dyn std::io::Write,
    counts: &'a Counts,
    path: &'a str,
    at: u64,
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
            self.at,
            buf.len() as u64,
        ));
        self.at += buf.len() as u64;
        let gate = self.counts.gate.lock().unwrap().take();
        if let Some(gate) = gate {
            gate();
        }
        let mut rest = buf;
        while !rest.is_empty() {
            let n = self.write(rest)?;
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
        let got = self.inner.stat_opt(p);
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
                None => self.inner.lstat_opt(p),
            };
        self.counts.op("lstat", p);
        got
    }
    fn realpath(&mut self, p: &str) -> Result<String> {
        self.alive()?;
        let got = self.inner.realpath(p);
        self.counts.op("realpath", p);
        got
    }
    fn read_head(&mut self, p: &str, max: usize) -> Result<Vec<u8>> {
        self.alive()?;
        let head = self.inner.read_head(p, max)?;
        self.counts
            .heads
            .fetch_add(head.len() as u64, Ordering::Relaxed);
        self.counts.op("read_head", p);
        Ok(head)
    }
    fn list_dir(&mut self, p: &str) -> Result<Vec<String>> {
        self.alive()?;
        self.inner.list_dir(p)
    }
    fn list_entries(&mut self, p: &str) -> Result<Vec<crate::remote::Entry>> {
        self.alive()?;
        self.inner.list_entries(p)
    }
    fn read_to(
        &mut self,
        p: &str,
        out: &mut dyn std::io::Write,
        progress: &mut dyn FnMut(u64),
    ) -> Result<u64> {
        self.alive()?;
        let counts = self.counts.clone();
        let mut counted = CountingWriter {
            out,
            counts: &counts,
            path: p,
            at: 0,
            n: 0,
        };
        let result = self.inner.read_to(p, &mut counted, progress);
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
        self.inner.create_from(local, p, like)
    }
    fn set_mode(&mut self, p: &str, mode: u32) -> Result<()> {
        self.inner.set_mode(p, mode)
    }
    fn rename_over(&mut self, from: &str, to: &str) -> Result<()> {
        self.inner.rename_over(from, to)
    }
    fn link_new(&mut self, from: &str, to: &str) -> Result<()> {
        self.inner.link_new(from, to)
    }
    fn can_link(&self) -> bool {
        self.inner.can_link()
    }
    fn can_replace(&self) -> bool {
        self.inner.can_replace()
    }
    fn remove(&mut self, p: &str) -> Result<()> {
        self.inner.remove(p)
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

/// No claim, no record, no lock, no session copy, nothing moved aside: a remote preview reads the
/// file into a private copy of the window's (02 §4.4) — also with an earlier session's copy of the
/// same file in the cache, holding an edit it never saved — and dropping it removes the copy.
/// Only a hand-over claims: then the edit is moved aside, and said so, as Enter's fetch does.
#[test]
fn a_remote_preview_reads_without_claiming_the_cache() {
    let Some(mut fx) = remote() else { return };
    let abs = fx.file("notes.txt", b"what the remote holds\n");
    // An earlier session's copy: recorded, then edited and never saved.
    crate::cache::seed_copy(&fx.cache, "box", &abs, b"fetched", b"an unsaved edit");
    let before = fx.cache_files();
    let p = fx.preview(&abs);
    assert!(p.card().is_none(), "{}", card_why(&p));
    assert_eq!(fx.cache_files(), before, "the cache is as it was");
    let copies = fx.preview_copies();
    assert_eq!(copies.len(), 1, "{copies:?}");
    assert_eq!(
        std::fs::read(&copies[0]).unwrap(),
        b"what the remote holds\n"
    );
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(&copies[0]).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
    let mut p = p;
    let mut drv = Driver::new(500.0, 300.0);
    shown(&mut drv, &mut Host(&mut p.embedded.window));
    assert!(drv.find_text("what the remote holds").is_some());
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
    assert_eq!(std::fs::read(&cached).unwrap(), b"what the remote holds\n");
}

/// P3: a remote file over the read budget shows the info card at once and nothing of it is read
/// — not even the sniff's head (the gate is the stat).
#[test]
fn a_remote_file_over_the_read_budget_is_a_card_with_nothing_read() {
    let Some(mut fx) = remote() else { return };
    let abs = fx.file("big", &vec![7u8; 200 << 10]);
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
    assert!(fx.preview_copies().is_empty());
}

/// P3: a remote file that grows past the budget between the stat and the read is cut at the
/// budget (plus at most one chunk) and shows the card; its partial copy is gone.
#[test]
fn a_remote_read_stops_at_the_budget() {
    let Some(mut fx) = remote() else { return };
    let abs = fx.file("growing.txt", &vec![b'a'; 64 << 10]);
    let limits = Limits {
        read: 100 << 10,
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
    };
    let mut grown = false;
    let p = preview(req, &budget, &AtomicBool::new(false), &mut |_, _| {
        if !grown {
            // Grown while being read (the stat said 64 KiB).
            std::fs::write(&path, vec![b'b'; 8 << 20]).unwrap();
            grown = true;
        }
    })
    .unwrap()
    .show();
    assert_eq!(card_why(&p), "over the preview limit: Enter opens it");
    // At the read boundary: what the server sent, response by response, read once, in order —
    // and no further than the response that crossed the budget (the SFTP reader asks for the
    // next response only after the last one was taken: nothing else is in flight).
    let responses = fx.counts.responses(&abs);
    assert_ranges_once(&responses);
    let sent: u64 = responses.iter().map(|r| r.1).sum();
    assert!(
        sent <= (100 << 10) + SFTP_RESPONSE,
        "the server sent {sent} bytes"
    );
    assert_eq!(fx.counts.reads.lock().unwrap().len(), 1, "one read");
    // What the copy took: never past the budget.
    let taken = fx.counts.content.load(Ordering::Relaxed);
    assert!(taken <= 100 << 10, "the copy took {taken} bytes");
    assert!(fx.preview_copies().is_empty(), "{:?}", fx.preview_copies());
}

/// The most one SFTP read response carries: the reader's request size (`sftp.rs` `CHUNK`).
const SFTP_RESPONSE: u64 = 256 << 10;

/// Read responses that cover the file from its start, each once, in order.
fn assert_ranges_once(responses: &[(u64, u64)]) {
    let mut at = 0;
    for &(offset, n) in responses {
        assert_eq!(offset, at, "a range read twice or skipped: {responses:?}");
        at += n;
    }
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
    // Before the hand-over the preview holds nothing: another session gets the shared copy.
    let other = crate::cache::claim(&fx.cache, "box", &abs).unwrap();
    assert!(other.note.is_none(), "not held by the preview");
    drop(other);
    let mut handed = p.hand_over().ok().expect("a viewer");
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
    let names: Vec<String> = (0..20)
        .map(|i| {
            fx.file(
                &format!("f{i:02}.txt"),
                &vec![b'a' + (i % 26) as u8; 300 << 10],
            )
        })
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

/// Lifted (Enter while the preview is still being prepared), the preparation goes on under the
/// viewer's rules: past the read budget, past the text budget, past the deadline.
#[test]
fn a_lifted_budget_lets_the_preparation_go_on() {
    let Some(mut fx) = remote() else { return };
    let body: String = (0..100_000).map(|i| format!("{i:08} line\n")).collect();
    let abs = fx.file("long.txt", b"short at first\n");
    // Under the read budget at the stat; then the long file it becomes is read.
    let (path, long) = (abs.clone(), body.clone());
    let mut armed = true;
    *fx.counts.after.lock().unwrap() = Some(Box::new(move |op, p| {
        if op == "stat" && p == path && std::mem::take(&mut armed) {
            std::fs::write(&path, &long).unwrap();
        }
    }));
    let budget = Budget::new(Limits {
        read: 64 << 10,
        text: 64 << 10,
        time: Duration::from_secs(5),
        ..Limits::PREVIEW
    });
    // Enter comes while the read is under way: after its first response has arrived.
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
    // The same read went on past the budget, from where it was: one read, every range once.
    let responses = fx.counts.responses(&abs);
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
    let mut handed = p.hand_over().ok().unwrap();
    let mut drv = Driver::new(600.0, 400.0);
    let mut win = Host(&mut handed.embedded.window);
    shown(&mut drv, &mut win);
    drv.key(&mut win, Key::End, command(false));
    drv.idle(&mut win, 2);
    assert!(
        drv.text_starting_with("00099999 line").is_some(),
        "the whole file"
    );
    assert_eq!(fx.counts.content.load(Ordering::Relaxed), body.len() as u64);
}

/// P12 (remote text): a 6 MiB remote file is fetched once, into the preview's copy, which the
/// preview indexes to 4 MiB; handed over, the copy is the cached one (claimed by its hard link)
/// and the index goes on through the copy's name from 4 MiB — the remote is not read again, no
/// byte is indexed twice — and End shows the last line.
#[test]
fn a_handed_over_remote_text_resumes_through_its_copy() {
    let Some(mut fx) = remote() else { return };
    let body: String = (0..6 * 16_384)
        .map(|i| format!("{i:08} {}\n", "r".repeat(54)))
        .collect();
    let abs = fx.file("six.log", body.as_bytes());
    let p = fx.preview(&abs);
    assert!(p.card().is_none(), "{}", card_why(&p));
    let copy = fx.preview_copies().remove(0);
    let indexed = |path: &Path| {
        crate::gui::text::index_reads(path)
            .iter()
            .map(|r| r.1)
            .sum::<u64>()
    };
    assert_eq!(
        indexed(&copy),
        4 << 20,
        "the preview indexed 4 MiB of its copy"
    );
    let mut handed = p.hand_over().ok().expect("a viewer");
    let hold = handed.hold.take().expect("claimed");
    assert_eq!(
        std::fs::metadata(&hold.local).unwrap().ino(),
        std::fs::metadata(&copy).unwrap().ino()
    );
    let mut drv = Driver::new(600.0, 400.0);
    let mut win = Host(&mut handed.embedded.window);
    drv.until(&mut win, 20, |_| indexed(&copy) >= 6 << 20);
    shown(&mut drv, &mut win);
    drv.key(&mut win, Key::End, command(false));
    drv.idle(&mut win, 2);
    assert!(drv
        .text_starting_with(&format!("{:08} ", 6 * 16_384 - 1))
        .is_some());
    let mut at = 0;
    for (offset, n) in crate::gui::text::index_reads(&copy) {
        assert_eq!(offset, at, "in order, nothing twice");
        at += n;
    }
    assert_eq!(at, 6 << 20);
    assert_eq!(
        *fx.counts.reads.lock().unwrap(),
        vec![(abs.clone(), 6 << 20)],
        "one fetch"
    );
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

/// P3 (local image): the encoded budget is checked again on the bytes themselves — an image
/// grown in place, or replaced by a bigger one, after the stat that checked its size is read
/// only to its budget and shows the card; lifted (Enter), the same file is read on.
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
    for replace in [false, true] {
        let abs = fx.file(&format!("photo-{replace}.png"), &small);
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
        let p = fx.preview_limited(&abs, limits);
        assert_eq!(card_why(&p), OVER_LIMIT, "replaced: {replace}");
        *fx.counts.after.lock().unwrap() = None;
        let budget = Budget::new(limits);
        budget.lift();
        let p = fx
            .preview_with(&abs, &budget, &AtomicBool::new(false))
            .unwrap();
        assert!(p.card().is_none(), "lifted: {}", card_why(&p));
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
/// sniff, the read's own stat, the read — no later request is made. On a dead session, a cancel
/// raised inside the failed lstat leaves it there: no probe, no new connection.
#[test]
fn a_cancel_inside_any_step_starts_no_later_one() {
    let Some(probe) = remote() else { return };
    drop(probe);
    let steps = [
        "connect ",
        "lstat {link}",
        "stat {link}",
        "realpath {link}",
        "stat {data}",
        "read_head {data}",
        "stat {data}",
        "read_to {data}",
    ];
    for k in 0..steps.len() {
        let mut fx = remote().unwrap();
        let data = fx.file("data", b"plain text with no extension\n");
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
            "step {k}: {:?}",
            result.as_ref().err()
        );
        assert_eq!(fx.counts.ops(), expected, "cancelled inside step {k}");
        assert!(fx.preview_copies().is_empty());
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
    let abs = fx.file("big.txt", &vec![b'c'; 4 << 20]);
    let (reached_tx, reached) = std::sync::mpsc::channel::<()>();
    let (go, go_rx) = std::sync::mpsc::channel::<()>();
    *fx.counts.gate.lock().unwrap() = Some(Box::new(move || {
        reached_tx.send(()).unwrap();
        go_rx.recv().unwrap();
    }));
    let cancel = AtomicBool::new(false);
    let budget = Budget::preview();
    let result = std::thread::scope(|scope| {
        let worker = scope.spawn(|| fx.preview_with(&abs, &budget, &cancel).map(|_| ()));
        reached
            .recv_timeout(Duration::from_secs(10))
            .expect("the read began");
        cancel.store(true, Ordering::SeqCst);
        go.send(()).unwrap();
        worker.join().unwrap()
    });
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
    let abs = fx.file("slow.txt", &vec![b's'; 600 << 10]);
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
