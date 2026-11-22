//! How a route opens a remote file as a stream (06 §2, §13): the connection the open already
//! made — the one that resolved the path, stat'ed it and sniffed its kind — becomes the
//! [`Source`]'s transport, so viewing costs that one session and nothing is fetched before the
//! viewer shows. The viewer's `Origin` is then a placeholder that connects on its first use
//! ([`Origin::placeholder`](super::Origin::placeholder)).
//!
//! The transport is wrapped once here ([`Routed`]): what it receives is counted for the usage
//! line's `bytes_fetched` (06 §11), and a preview's is given back to its window when the stream
//! closes without having become a viewer ([`Lease`]), so the window keeps its one preview
//! session (06 §13).

use super::browser::Connect;
use crate::remote::{Completion, Ranged, Refused, RemoteFs, Stat, Ticket, Wake};
use crate::stream::{Opening, Reopen, Source, Waker};
use anyhow::anyhow;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The bytes every stream of this run has received.
static FETCHED: AtomicU64 = AtomicU64::new(0);

/// 06 §11's `bytes_fetched`: the bytes every `Source` of the run received from its remote —
/// every byte of every read reply that arrived, whether or not the `Source` still wanted it
/// (a reply to a read asked before a reload arrives and is counted; one that never arrives
/// is not).
pub fn bytes_fetched() -> u64 {
    FETCHED.load(Ordering::Relaxed)
}

/// Where a transport counts what it receives: the run's count, or a test's own.
#[derive(Clone)]
enum Count {
    Run,
    #[cfg(test)]
    Own(Arc<AtomicU64>),
}

impl Count {
    fn add(&self, n: u64) {
        match self {
            Count::Run => FETCHED.fetch_add(n, Ordering::Relaxed),
            #[cfg(test)]
            Count::Own(count) => count.fetch_add(n, Ordering::Relaxed),
        };
    }
}

/// Where a preview's stream gives its connection back when it closes: the window's preview
/// session, lent to the stream while the preview shows (06 §13). [`Lease::keep`] ends the
/// lending — the hand-over: the session is the viewer's from then on.
#[derive(Clone, Default)]
pub struct Lease(Arc<Mutex<Option<Back>>>);

/// Where a connection is sent back.
type Back = Sender<Box<dyn RemoteFs + Send>>;

impl Lease {
    pub fn new(back: Sender<Box<dyn RemoteFs + Send>>) -> Self {
        Self(Arc::new(Mutex::new(Some(back))))
    }

    /// The stream keeps its connection for good (it closes it when it closes).
    pub fn keep(&self) {
        *self.0.lock().unwrap_or_else(|p| p.into_inner()) = None;
    }

    fn take(&self) -> Option<Back> {
        self.0.lock().unwrap_or_else(|p| p.into_inner()).take()
    }
}

/// A stream a route opened, and the handle's stat when it was (the `Origin`'s `at`).
pub struct Opened {
    pub source: Source,
    pub at: Stat,
}

/// A stream that was not opened, with the connection it was to run on: the caller is as it
/// was (06 §7.5). `conn` is `None` only when the stream's own thread could not be started,
/// which took the connection with it.
pub struct NotOpened {
    pub conn: Option<Box<dyn RemoteFs + Send>>,
    pub error: anyhow::Error,
}

/// Opens the file at `abs` as a stream on `conn`, the connection the open already made (06 §2).
/// `dial` opens another one: what a retry after a failure runs. `lease`: a preview's, see
/// [`Lease`]. A failure — the file cannot be opened, or its handle `fstat`ed — gives the
/// connection back unchanged ([`NotOpened`]).
pub fn open(
    conn: Box<dyn RemoteFs + Send>,
    abs: &str,
    opening: Opening,
    dial: Option<Connect>,
    wake: Waker,
    lease: Option<Lease>,
) -> Result<Opened, NotOpened> {
    let transport = match conn.into_ranged(abs) {
        Ok(transport) => transport,
        Err(Refused { conn, error }) => {
            return Err(NotOpened {
                conn: Some(conn),
                error,
            })
        }
    };
    let at = transport.opened();
    let path = abs.to_string();
    let reopen: Reopen = Box::new(move || {
        let Some(dial) = &dial else {
            return Err(anyhow!("there is no way to connect again"));
        };
        // A connection of the stream's own from here on: never the window's preview session.
        match dial()?.into_ranged(&path) {
            Ok(transport) => Ok(Routed::wrap(transport, None)),
            Err(refused) => Err(refused.error),
        }
    });
    match Source::open_as(Routed::wrap(transport, lease), reopen, wake, opening) {
        Ok(source) => Ok(Opened { source, at }),
        Err(error) => Err(NotOpened { conn: None, error }),
    }
}

/// What the standalone route (`sopen host:/file`) opens a remote image, video or sound as (06
/// §13 W8b), on the connection that resolved, stat'ed and sniffed it: a stream, with nothing
/// fetched.
pub enum Standalone {
    /// The image window over `source`; `switch` is where it gives way to the hex window, in the
    /// same window (Hex view, a file that does not decode: 06 §7.3).
    Image {
        origin: super::Origin,
        source: Source,
        switch: super::image::HexSwitch,
    },
    /// The media window over `source` (06 §7.4).
    Media {
        origin: super::Origin,
        source: Source,
    },
    /// A `.ts` ffprobe does not read as media through its `play` URL (06 §7.4): any other file,
    /// for the binary dialog. Its stream is closed: the dialog opens the file from the start.
    /// `size`: the file's, as its handle said.
    NotMedia { size: u64 },
}

/// Opens the remote `kind` (an image, a video or a sound) at `abs` on `label`'s host as a
/// stream on `conn` ([`open`]) for its window of its own: its `Origin` a placeholder that
/// connects through `dial` on its first use (a Save), answering yes until then (06 §2). A `.ts`
/// is probed through its `play` URL first. `sniff`: the bytes the kind sniff read of it. Every
/// step that can fail comes before the stream is handed on; a failure is said in 06 §6's words.
pub fn standalone(
    conn: Box<dyn RemoteFs + Send>,
    label: &str,
    abs: &str,
    kind: crate::kind::Kind,
    sniff: u64,
    dial: Connect,
) -> anyhow::Result<Standalone> {
    let shown = format!("{label}:{abs}");
    let repaint = super::hex::Repaint::default();
    let opening = Opening {
        label: shown.clone(),
        began: Instant::now(),
        sniff,
    };
    let opened = open(
        conn,
        abs,
        opening,
        Some(dial.clone()),
        repaint.waker(),
        None,
    )
    .map_err(|not| anyhow!(crate::stream::explain(&not.error)))?;
    let origin = super::Origin::placeholder(
        Some(Box::new(move || dial()) as super::Reconnect),
        label.to_string(),
        abs.to_string(),
        opened.at,
        super::Placeholder::YES,
    );
    let source = opened.source;
    if kind == crate::kind::Kind::Image {
        let switch = super::image::HexSwitch::new(repaint);
        return Ok(Standalone::Image {
            origin,
            source,
            switch,
        });
    }
    let (_, name) = crate::remote::split_parent(abs);
    if crate::kind::maybe_video(name) {
        // The `.ts` refine over the URL (06 §7.4).
        // No window yet, so nothing to stop it with.
        let never = std::sync::atomic::AtomicBool::new(false);
        let input = super::media::served_input(source.clone(), name, &shown, &never)?;
        if crate::kind::refine_input(&input, name, kind) == crate::kind::Kind::Other {
            return Ok(Standalone::NotMedia {
                size: opened.at.size,
            });
        }
    }
    Ok(Standalone::Media { origin, source })
}

/// The transport of a stream a route opened: counts what it receives, and gives its connection
/// back to its [`Lease`] when the stream closes it.
struct Routed {
    inner: Box<dyn Ranged>,
    lease: Option<Lease>,
    count: Count,
}

impl Routed {
    fn wrap(inner: Box<dyn Ranged>, lease: Option<Lease>) -> Box<dyn Ranged> {
        Self::counting(inner, lease, Count::Run)
    }

    fn counting(inner: Box<dyn Ranged>, lease: Option<Lease>, count: Count) -> Box<dyn Ranged> {
        Box::new(Self {
            inner,
            lease,
            count,
        })
    }
}

impl Ranged for Routed {
    fn path(&self) -> &str {
        self.inner.path()
    }

    fn opened(&self) -> Stat {
        self.inner.opened()
    }

    fn read(&mut self, off: u64, len: u32) -> Ticket {
        self.inner.read(off, len)
    }

    fn fstat(&mut self) -> Ticket {
        self.inner.fstat()
    }

    fn stat_path(&mut self) -> Ticket {
        self.inner.stat_path()
    }

    fn pending(&self) -> usize {
        self.inner.pending()
    }

    fn next(&mut self, timeout: Option<Duration>) -> Option<Completion> {
        let c = self.inner.next(timeout)?;
        if let Some(data) = c.data() {
            self.count.add(data.len() as u64);
        }
        Some(c)
    }

    fn waker(&self) -> Wake {
        self.inner.waker()
    }

    fn now(&self) -> Instant {
        self.inner.now()
    }

    fn reopen(self: Box<Self>, path: &str) -> Result<Box<dyn Ranged>, Refused> {
        let (lease, count) = (self.lease, self.count);
        Ok(Self::counting(self.inner.reopen(path)?, lease, count))
    }

    fn into_connection(self: Box<Self>) -> Box<dyn RemoteFs + Send> {
        self.inner.into_connection()
    }

    /// The stream is done with it: a preview's goes back to its window (the handle closed, the
    /// session kept), any other is closed.
    fn close(self: Box<Self>) {
        match self.lease.as_ref().and_then(Lease::take) {
            // Nobody waiting any more (the window closed): the connection is dropped there.
            Some(back) => drop(back.send(self.inner.into_connection())),
            None => self.inner.close(),
        }
    }
}

#[cfg(test)]
mod tests {
    //! The route's transport on the local disk posing as a remote (`LocalFs` streams in tests,
    //! 06 §2): what it counts, and where its connection goes when the stream closes.

    use super::*;
    use crate::remote::LocalFs;
    use crate::stream::{ReaderKind, BLOCK};
    use std::sync::mpsc;
    use std::time::Instant;

    fn file(len: usize) -> (tempfile::TempDir, String) {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("blob.bin");
        std::fs::write(&path, vec![0x5A; len]).unwrap();
        let abs = path.to_str().unwrap().to_string();
        (tmp, abs)
    }

    fn opened(abs: &str, lease: Option<Lease>) -> Opened {
        let opening = Opening {
            label: format!("box:{abs}"),
            began: Instant::now(),
            sniff: 0,
        };
        let dial: Connect = Arc::new(|| Ok(Box::new(LocalFs::new()) as Box<dyn RemoteFs + Send>));
        let wake: Waker = Arc::new(|| {});
        open(
            Box::new(LocalFs::new()),
            abs,
            opening,
            Some(dial),
            wake,
            lease,
        )
        .ok()
        .expect("the stream opens")
    }

    /// 06 §11's `bytes_fetched` is what the routes' streams received: reading a block through
    /// one adds what its `Source` counts as fetched. (The count is the run's, shared by every
    /// test of this process: only its growth is looked at.)
    #[test]
    fn what_a_routed_stream_receives_is_counted_for_the_usage_line() {
        let (_tmp, abs) = file(3 * BLOCK as usize);
        let before = bytes_fetched();
        let stream = opened(&abs, None);
        assert_eq!(stream.at.size, 3 * BLOCK);
        let mut reader = stream.source.reader(ReaderKind::Interactive);
        let mut buf = vec![0u8; 16];
        let cancel = std::sync::atomic::AtomicBool::new(false);
        assert_eq!(reader.read_at(BLOCK, &mut buf, &cancel).unwrap(), 16);
        assert_eq!(buf, [0x5A; 16]);
        let fetched = stream.source.fetched().total;
        assert!(fetched >= BLOCK, "{fetched}");
        assert!(
            bytes_fetched() - before >= fetched,
            "{} counted for {fetched} fetched",
            bytes_fetched() - before
        );
    }

    /// 06 §11, exactly, on a count of the test's own (not the run's, which every test of the
    /// process shares): what the route's transport counts is what the transport under it
    /// delivered — byte for byte, by the fixture's meter below it — through a reload asked
    /// while a read's reply was still on its way, and the reads after it. The `Source`'s own
    /// `fetched` can be less (it drops a reply of the version before unread), never more.
    #[test]
    fn the_count_is_exactly_what_arrived_through_a_reload_with_a_reply_outstanding() {
        use crate::gui::testkit::Meter;
        use crate::remote::{fake, FakeFile, Link};
        use crate::stream::{Settings, Source};
        use std::task::Poll;

        let (transport, _control) = fake("/m.bin", FakeFile::pattern(4 << 20), Link::gate());
        let meter = Meter::new(u64::MAX, 0);
        let own = Arc::new(AtomicU64::new(0));
        let routed = Routed::counting(meter.transport(transport), None, Count::Own(own.clone()));
        let opening = Opening {
            label: "box:/m.bin".into(),
            began: Instant::now(),
            sniff: 0,
        };
        let reopen: Reopen = Box::new(|| Err(anyhow!("no second connection in this test")));
        let (source, mut engine) =
            Source::build(routed, reopen, Arc::new(|| {}), opening, Settings::plain());
        let settle = |engine: &mut crate::stream::Engine| {
            for _ in 0..10_000 {
                engine.pump();
                if engine.transport().map_or(0, |r| r.pending()) == 0 {
                    return;
                }
                engine.step(None);
            }
            panic!("the stream did not settle");
        };
        let counted = || own.load(Ordering::Relaxed);
        let arrived = || meter.stream_bytes.load(Ordering::SeqCst);
        // A first block, read whole.
        let mut reader = source.reader(ReaderKind::Interactive);
        assert!(reader.try_read(0, 16).is_pending());
        settle(&mut engine);
        assert!(matches!(reader.try_read(0, 16), Poll::Ready(Ok(_))));
        assert_eq!((counted(), arrived()), (BLOCK, BLOCK));
        // A read is sent; the reload is asked with its reply outstanding.
        assert!(reader.try_read(2 << 20, 16).is_pending());
        engine.pump();
        assert!(
            engine.transport().is_some_and(|r| r.pending() > 0),
            "a reply is on its way"
        );
        source.request_reload();
        settle(&mut engine);
        assert_eq!(counted(), arrived(), "through the reload");
        // The version after it, read on.
        reader.renew();
        assert!(reader.try_read(3 << 20, 16).is_pending());
        settle(&mut engine);
        assert!(matches!(reader.try_read(3 << 20, 16), Poll::Ready(Ok(_))));
        assert_eq!(counted(), arrived(), "and after it");
        assert!(counted() >= 2 * BLOCK, "{}", counted());
        assert!(source.fetched().total <= counted());
        assert_eq!(meter.returned.lock().unwrap().len(), 1, "one re-open");
    }

    /// 06 §13: a preview's stream gives its connection back when it closes — the session is
    /// kept, usable at once — and keeps it for good once the lease was kept (the hand-over):
    /// then nothing comes back, and the receiver sees that at once, without a wait.
    #[test]
    fn a_leased_connection_comes_back_unless_the_lease_was_kept() {
        let (_tmp, abs) = file(BLOCK as usize);
        let (tx, rx) = mpsc::channel();
        let stream = opened(&abs, Some(Lease::new(tx)));
        drop(stream);
        let mut conn = rx
            .recv_timeout(Duration::from_secs(10))
            .expect("the connection came back");
        assert_eq!(conn.stat(&abs).unwrap().size, BLOCK);

        let (tx, rx) = mpsc::channel();
        let lease = Lease::new(tx);
        let stream = opened(&abs, Some(lease.clone()));
        lease.keep();
        assert!(
            matches!(rx.try_recv(), Err(mpsc::TryRecvError::Disconnected)),
            "nothing will come back, and that is known at once"
        );
        drop(stream);
        assert!(rx.recv_timeout(Duration::from_secs(1)).is_err());
    }

    /// 06 §7.5: a file that cannot be opened as a stream gives the connection back unchanged.
    #[test]
    fn a_failed_open_gives_the_connection_back() {
        let (tmp, _abs) = file(16);
        let missing = tmp.path().join("nothing-here");
        let opening = Opening {
            label: "box:/nothing-here".into(),
            began: Instant::now(),
            sniff: 0,
        };
        let wake: Waker = Arc::new(|| {});
        let not = open(
            Box::new(LocalFs::new()),
            missing.to_str().unwrap(),
            opening,
            None,
            wake,
            None,
        )
        .err()
        .expect("no such file");
        let mut conn = not.conn.expect("the connection is given back");
        assert!(conn.stat(tmp.path().to_str().unwrap()).unwrap().is_dir);
    }

    // -----------------------------------------------------------------------------------------
    // The standalone route (`sopen host:/file`, 06 §13 W8b), by the fixture's own count
    // ([`Meter`]: below the `Source`, with budgets that end the I/O the moment a route asks
    // for more). The local disk poses as host `box`.

    use crate::gui::testkit::{Driver, Meter};
    use crate::gui::Window;
    use crate::kind::Kind;
    use eframe::egui;

    /// A window around an `Embedded`'s, for the test kit.
    struct Host<'a>(&'a mut Box<dyn Window>);

    impl Window for Host<'_> {
        fn ui(&mut self, ui: &mut egui::Ui) {
            self.0.ui(ui)
        }
        fn ready(&self) -> bool {
            self.0.ready()
        }
    }

    /// A metered connection to "box" (the local disk) and a `dial` that meters what it opens.
    fn metered(stream_budget: u64) -> (Arc<Meter>, Box<dyn RemoteFs + Send>, Connect) {
        let meter = Meter::new(stream_budget, 0);
        let conn = meter.connection(Box::new(LocalFs::new()));
        let dial = meter.connect(|| Ok(Box::new(LocalFs::new()) as Box<dyn RemoteFs + Send>));
        (meter, conn, dial)
    }

    /// Some text the last pass painted holds `part`.
    fn shows(drv: &Driver, part: &str) -> bool {
        drv.texts().iter().any(|(_, _, t)| t.contains(part))
    }

    /// A PNG header saying `w`×`h`, and one empty row of data.
    fn png_claiming(w: u32, h: u32) -> Vec<u8> {
        fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
            out.extend_from_slice(&(data.len() as u32).to_be_bytes());
            let typed = [kind.as_slice(), data].concat();
            let mut crc = 0xFFFF_FFFFu32;
            for &b in &typed {
                crc ^= u32::from(b);
                for _ in 0..8 {
                    crc = if crc & 1 != 0 {
                        0xEDB8_8320 ^ (crc >> 1)
                    } else {
                        crc >> 1
                    };
                }
            }
            out.extend_from_slice(&typed);
            out.extend_from_slice(&(!crc).to_be_bytes());
        }
        let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
        let mut ihdr = w.to_be_bytes().to_vec();
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

    /// The standalone route on a 2 GiB remote image (sparse, a PNG header at its front): a
    /// stream on the open's own connection, its window at the memory line (with Hex view)
    /// having read its header's block — nothing read whole, no second connection.
    #[test]
    fn the_standalone_route_streams_a_remote_image() {
        let tmp = tempfile::tempdir().unwrap();
        let abs = tmp.path().join("huge.png");
        std::fs::write(&abs, png_claiming(100_000, 100_000)).unwrap();
        std::fs::File::options()
            .write(true)
            .open(&abs)
            .unwrap()
            .set_len(2 << 30)
            .unwrap();
        let abs = abs.to_str().unwrap().to_string();
        let (meter, conn, dial) = metered(BLOCK);
        let opened = standalone(conn, "box", &abs, Kind::Image, 0, dial).unwrap();
        let Standalone::Image {
            origin,
            source,
            switch,
        } = opened
        else {
            panic!("not the image window")
        };
        let cache = tmp.path().join("cache");
        let mut embedded =
            crate::gui::image::embed_stream(origin, source, &cache, Vec::new(), Some(switch))
                .unwrap();
        let mut drv = Driver::new(900.0, 600.0);
        let deadline = Instant::now() + Duration::from_secs(60);
        while !shows(&drv, "Hex view (H)") {
            assert!(Instant::now() < deadline, "no memory line");
            drv.idle(&mut Host(&mut embedded.window), 1);
        }
        assert_eq!(meter.totals().2, 0, "nothing read whole");
        assert_eq!(meter.reads_of(&abs), vec![(0, BLOCK as u32)], "its header");
        assert!(!meter.tripped());
        assert_eq!(
            meter.connects.load(Ordering::SeqCst),
            0,
            "no second connection"
        );
    }

    /// The standalone route on a 2 GiB remote video (sparse, its `moov` at the front): the
    /// media window shows its first frame having read a sixteenth of the file at most —
    /// nothing read whole, no second connection.
    #[test]
    fn the_standalone_route_streams_a_remote_video() {
        let Some(tools) = crate::gui::media::fixtures::tools() else {
            return;
        };
        let tmp = tempfile::tempdir().unwrap();
        let (front, abs) = (tmp.path().join("front.mp4"), tmp.path().join("big.mp4"));
        crate::gui::media::fixtures::clip(&tools, &front, (160, 120));
        let args = [
            "-i",
            front.to_str().unwrap(),
            "-c",
            "copy",
            "-movflags",
            "+faststart",
        ];
        crate::gui::media::fixtures::make(&tools, &args, &abs);
        let have = std::fs::metadata(&abs).unwrap().len();
        let size: u64 = 2 << 30;
        let mut free = 1u32.to_be_bytes().to_vec();
        free.extend_from_slice(b"free");
        free.extend_from_slice(&(size - have).to_be_bytes());
        {
            use std::io::Write as _;
            let mut file = std::fs::File::options().append(true).open(&abs).unwrap();
            file.write_all(&free).unwrap();
            file.set_len(size).unwrap();
        }
        let abs = abs.to_str().unwrap().to_string();
        let (meter, conn, dial) = metered(size / 16);
        let opened = standalone(conn, "box", &abs, Kind::Video, 0, dial).unwrap();
        let Standalone::Media { origin, source } = opened else {
            panic!("not the media window")
        };
        let shown = origin.display();
        let media = crate::gui::media::Media::stream(source, "big.mp4", &shown).unwrap();
        let cache = tmp.path().join("cache");
        let mut embedded = crate::gui::media::embed_streamed(origin, media, &cache, Vec::new());
        let mut drv = Driver::new(800.0, 600.0);
        drv.until(&mut Host(&mut embedded.window), 60, |w| w.ready());
        drv.idle(&mut Host(&mut embedded.window), 2);
        assert_eq!(drv.pictures().len(), 1, "its first frame");
        assert_eq!(meter.totals().2, 0, "nothing read whole");
        assert!(!meter.tripped(), "{} bytes", meter.totals().0);
        assert_eq!(
            meter.connects.load(Ordering::SeqCst),
            0,
            "no second connection"
        );
    }

    /// The standalone route on a remote `.ts` that looks like a transport stream but that
    /// ffprobe does not read as media: probed through its `play` URL (06 §7.4) — nothing read
    /// whole, no byte read twice — and it is not a medium (the binary dialog opens it).
    #[test]
    fn the_standalone_route_refines_a_ts_over_its_url() {
        if crate::gui::media::fixtures::tools().is_none() {
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let abs = tmp.path().join("clip.ts");
        let mut body = vec![0u8; 188 * 64];
        for packet in body.chunks_mut(188) {
            packet[0] = 0x47;
        }
        std::fs::write(&abs, &body).unwrap();
        let abs = abs.to_str().unwrap().to_string();
        let (meter, conn, dial) = metered(body.len() as u64);
        let opened = standalone(conn, "box", &abs, Kind::Video, 0, dial).unwrap();
        let Standalone::NotMedia { size } = opened else {
            panic!("taken for a medium")
        };
        assert_eq!(size, body.len() as u64);
        assert_eq!(meter.totals().2, 0, "nothing read whole");
        assert!(!meter.tripped(), "no byte asked twice");
        assert_eq!(meter.connects.load(Ordering::SeqCst), 0);
    }
}
