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

/// 06 §11's `bytes_fetched`: the bytes every `Source` of the run received from its remote.
pub fn bytes_fetched() -> u64 {
    FETCHED.load(Ordering::Relaxed)
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

/// The transport of a stream a route opened: counts what it receives, and gives its connection
/// back to its [`Lease`] when the stream closes it.
struct Routed {
    inner: Box<dyn Ranged>,
    lease: Option<Lease>,
}

impl Routed {
    fn wrap(inner: Box<dyn Ranged>, lease: Option<Lease>) -> Box<dyn Ranged> {
        Box::new(Self { inner, lease })
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
            FETCHED.fetch_add(data.len() as u64, Ordering::Relaxed);
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
        let lease = self.lease;
        Ok(Self::wrap(self.inner.reopen(path)?, lease))
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
}
