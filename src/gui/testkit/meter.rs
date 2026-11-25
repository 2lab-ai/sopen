//! A metered connection for tests: what crossed it, counted by the fixture itself.

use crate::remote::{Completion, Ranged, Refused, RemoteFs, Stat, Ticket, Wake};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// What crossed the connections of one test, counted by the fixture itself — below the
/// `Source`, so a claim such as "one block before the first screen" does not rest on the
/// product's own statistics — with budgets that end the I/O the moment a route asks for more:
/// a route that reads a large file whole fails at once, not after the file.
pub struct Meter {
    /// Bytes streams may ask for; a read past it is not sent, and the meter is tripped.
    stream_budget: u64,
    /// Bytes whole reads (`read_to`) may deliver; a response past it fails the read.
    whole_budget: u64,
    /// Connections opened (the fixture's `connect` counts here).
    pub connects: AtomicUsize,
    /// Files opened as a stream, in order: `(transport id, path)`.
    pub opened: Mutex<Vec<(usize, String)>>,
    /// Every read a stream asked: `(transport id, offset, length)`.
    pub asked: Mutex<Vec<(usize, u64, u32)>>,
    /// Every poll a stream sent (an `fstat` or a stat of its path, 06 §5): by transport id.
    pub polls: Mutex<Vec<usize>>,
    /// Bytes and read responses that came back to streams.
    pub stream_bytes: AtomicU64,
    pub stream_responses: AtomicUsize,
    /// Whole reads started, and the bytes and responses they delivered.
    pub whole_reads: AtomicUsize,
    pub whole_bytes: AtomicU64,
    pub whole_responses: AtomicUsize,
    /// Bytes of the kind sniff (`read_head`).
    pub heads: AtomicU64,
    /// Transports closed for good, and those that gave their connection back: by id.
    pub closed: Mutex<Vec<usize>>,
    pub returned: Mutex<Vec<usize>>,
    /// Sessions alive now (a connection, or the stream it became), and the most at once.
    pub live: AtomicUsize,
    pub peak: AtomicUsize,
    /// How long a transport takes to give its connection back (tests: a slow shutdown).
    pub return_delay: Mutex<Duration>,
    tripped: AtomicBool,
    next: AtomicUsize,
}

impl Meter {
    pub fn new(stream_budget: u64, whole_budget: u64) -> Arc<Self> {
        Arc::new(Self {
            stream_budget,
            whole_budget,
            connects: AtomicUsize::new(0),
            opened: Mutex::default(),
            asked: Mutex::default(),
            polls: Mutex::default(),
            stream_bytes: AtomicU64::new(0),
            stream_responses: AtomicUsize::new(0),
            whole_reads: AtomicUsize::new(0),
            whole_bytes: AtomicU64::new(0),
            whole_responses: AtomicUsize::new(0),
            heads: AtomicU64::new(0),
            closed: Mutex::default(),
            returned: Mutex::default(),
            live: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
            return_delay: Mutex::new(Duration::ZERO),
            tripped: AtomicBool::new(false),
            next: AtomicUsize::new(0),
        })
    }

    /// A budget was passed: something read more than the test allows.
    pub fn tripped(&self) -> bool {
        self.tripped.load(Ordering::SeqCst)
    }

    /// The reads streams asked of `path`'s transports, as `(offset, length)`, in order.
    pub fn reads_of(&self, path: &str) -> Vec<(u64, u32)> {
        let ids: Vec<usize> = self
            .opened
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, p)| p == path)
            .map(|(id, _)| *id)
            .collect();
        let asked = self.asked.lock().unwrap();
        asked
            .iter()
            .filter(|(id, _, _)| ids.contains(id))
            .map(|&(_, off, len)| (off, len))
            .collect()
    }

    /// `(stream bytes, stream responses, whole bytes, whole reads)`.
    pub fn totals(&self) -> (u64, usize, u64, usize) {
        (
            self.stream_bytes.load(Ordering::SeqCst),
            self.stream_responses.load(Ordering::SeqCst),
            self.whole_bytes.load(Ordering::SeqCst),
            self.whole_reads.load(Ordering::SeqCst),
        )
    }

    fn alive(&self) {
        let live = self.live.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak.fetch_max(live, Ordering::SeqCst);
    }

    /// `inner` as a metered connection: one more session alive, counted until it goes.
    pub fn connection(
        self: &Arc<Self>,
        inner: Box<dyn RemoteFs + Send>,
    ) -> Box<dyn RemoteFs + Send> {
        self.alive();
        Box::new(MeteredFs {
            inner: Some(inner),
            meter: self.clone(),
        })
    }

    /// `inner`, a transport already made, metered as one more session.
    pub fn transport(self: &Arc<Self>, inner: Box<dyn Ranged>) -> Box<dyn Ranged> {
        self.alive();
        let path = inner.path().to_string();
        MeteredRanged::wrap(inner, self, &path)
    }

    /// A `connect` over `dial`: every connection it opens is metered and counted.
    pub fn connect(
        self: &Arc<Self>,
        dial: impl Fn() -> anyhow::Result<Box<dyn RemoteFs + Send>> + Send + Sync + 'static,
    ) -> crate::gui::browser::Connect {
        let meter = self.clone();
        Arc::new(move || {
            let inner = dial()?;
            meter.connects.fetch_add(1, Ordering::SeqCst);
            Ok(meter.connection(inner))
        })
    }
}

/// A connection whose reads the [`Meter`] counts.
struct MeteredFs {
    /// `None` only once the session went to a stream ([`MeteredRanged`] is the session then).
    inner: Option<Box<dyn RemoteFs + Send>>,
    meter: Arc<Meter>,
}

impl MeteredFs {
    fn fs(&mut self) -> &mut (dyn RemoteFs + Send) {
        self.inner.as_mut().expect("a live session").as_mut()
    }
}

impl Drop for MeteredFs {
    fn drop(&mut self) {
        if self.inner.is_some() {
            self.meter.live.fetch_sub(1, Ordering::SeqCst);
        }
    }
}

/// Counts each response of a whole read, and refuses the one that passes the budget.
struct MeteredWriter<'a> {
    out: &'a mut dyn std::io::Write,
    meter: &'a Meter,
}

impl std::io::Write for MeteredWriter<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.write_all(buf)?;
        Ok(buf.len())
    }

    fn write_all(&mut self, buf: &[u8]) -> std::io::Result<()> {
        let m = self.meter;
        m.whole_responses.fetch_add(1, Ordering::SeqCst);
        if m.whole_bytes.load(Ordering::SeqCst) + buf.len() as u64 > m.whole_budget {
            m.tripped.store(true, Ordering::SeqCst);
            return Err(std::io::Error::other(
                "the test's whole-read budget is spent",
            ));
        }
        self.out.write_all(buf)?;
        m.whole_bytes.fetch_add(buf.len() as u64, Ordering::SeqCst);
        Ok(())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.out.flush()
    }
}

impl RemoteFs for MeteredFs {
    fn stat_opt(&mut self, p: &str) -> anyhow::Result<Option<Stat>> {
        self.fs().stat_opt(p)
    }
    fn lstat_opt(&mut self, p: &str) -> anyhow::Result<Option<Stat>> {
        self.fs().lstat_opt(p)
    }
    fn realpath(&mut self, p: &str) -> anyhow::Result<String> {
        self.fs().realpath(p)
    }
    fn read_head(&mut self, p: &str, max: usize) -> anyhow::Result<Vec<u8>> {
        let head = self.fs().read_head(p, max)?;
        self.meter
            .heads
            .fetch_add(head.len() as u64, Ordering::SeqCst);
        Ok(head)
    }
    fn list_dir(&mut self, p: &str) -> anyhow::Result<Vec<String>> {
        self.fs().list_dir(p)
    }
    fn list_entries(&mut self, p: &str) -> anyhow::Result<Vec<crate::remote::Entry>> {
        self.fs().list_entries(p)
    }
    fn read_to(
        &mut self,
        p: &str,
        out: &mut dyn std::io::Write,
        progress: &mut dyn FnMut(u64),
    ) -> anyhow::Result<u64> {
        let meter = self.meter.clone();
        meter.whole_reads.fetch_add(1, Ordering::SeqCst);
        let mut counted = MeteredWriter { out, meter: &meter };
        self.fs().read_to(p, &mut counted, progress)
    }
    fn create_from(
        &mut self,
        local: &std::path::Path,
        p: &str,
        like: Option<crate::remote::Like>,
    ) -> Result<crate::remote::Uploaded, crate::remote::CreateError> {
        self.fs().create_from(local, p, like)
    }
    fn set_mode(&mut self, p: &str, mode: u32) -> anyhow::Result<()> {
        self.fs().set_mode(p, mode)
    }
    fn rename_over(&mut self, from: &str, to: &str) -> anyhow::Result<()> {
        self.fs().rename_over(from, to)
    }
    fn link_new(&mut self, from: &str, to: &str) -> anyhow::Result<()> {
        self.fs().link_new(from, to)
    }
    fn can_link(&self) -> bool {
        self.inner.as_ref().is_some_and(|i| i.can_link())
    }
    fn can_replace(&self) -> bool {
        self.inner.as_ref().is_some_and(|i| i.can_replace())
    }
    fn remove(&mut self, p: &str) -> anyhow::Result<()> {
        self.fs().remove(p)
    }
    /// A hex Save's requests by handle (07 §4) go to the session itself: none is a whole read.
    fn patcher(&mut self) -> Option<&mut dyn crate::remote::patch::PatchFs> {
        self.fs().patcher()
    }
    /// The session becomes a stream's transport (06 §2), metered below the `Source`.
    fn into_ranged(mut self: Box<Self>, path: &str) -> Result<Box<dyn Ranged>, Refused> {
        let meter = self.meter.clone();
        // The session lives on, as the transport or as the connection given back.
        let inner = self.inner.take().expect("a live session");
        drop(self);
        match inner.into_ranged(path) {
            Ok(ranged) => Ok(MeteredRanged::wrap(ranged, &meter, path)),
            Err(refused) => Err(Refused {
                conn: Box::new(MeteredFs {
                    inner: Some(refused.conn),
                    meter,
                }),
                error: refused.error,
            }),
        }
    }
}

/// A stream's transport, metered: its reads and what came back, and how it ended.
struct MeteredRanged {
    inner: Option<Box<dyn Ranged>>,
    meter: Arc<Meter>,
    id: usize,
}

impl MeteredRanged {
    /// The session was alive already (as the connection): it is not counted again.
    fn wrap(inner: Box<dyn Ranged>, meter: &Arc<Meter>, path: &str) -> Box<dyn Ranged> {
        let id = meter.next.fetch_add(1, Ordering::SeqCst);
        meter.opened.lock().unwrap().push((id, path.to_string()));
        Box::new(Self {
            inner: Some(inner),
            meter: meter.clone(),
            id,
        })
    }

    fn inner(&self) -> &dyn Ranged {
        self.inner.as_ref().expect("a live transport").as_ref()
    }

    fn inner_mut(&mut self) -> &mut Box<dyn Ranged> {
        self.inner.as_mut().expect("a live transport")
    }

    /// The connection back, after the shutdown's delay — the session alive throughout.
    fn back(&mut self) -> Box<dyn RemoteFs + Send> {
        let delay = *self.meter.return_delay.lock().unwrap();
        if !delay.is_zero() {
            std::thread::sleep(delay);
        }
        let inner = self.inner.take().expect("a live transport");
        Box::new(MeteredFs {
            inner: Some(inner.into_connection()),
            meter: self.meter.clone(),
        })
    }
}

impl Drop for MeteredRanged {
    fn drop(&mut self) {
        if self.inner.is_some() {
            self.meter.live.fetch_sub(1, Ordering::SeqCst);
        }
    }
}

impl Ranged for MeteredRanged {
    fn path(&self) -> &str {
        self.inner().path()
    }
    fn opened(&self) -> Stat {
        self.inner().opened()
    }
    fn read(&mut self, off: u64, len: u32) -> Ticket {
        let asked: u64 = {
            let mut asked = self.meter.asked.lock().unwrap();
            asked.push((self.id, off, len));
            asked.iter().map(|a| u64::from(a.2)).sum()
        };
        if asked > self.meter.stream_budget {
            // Nothing more crosses: the read is sent empty, and the test fails on the trip.
            self.meter.tripped.store(true, Ordering::SeqCst);
            return self.inner_mut().read(off, 0);
        }
        self.inner_mut().read(off, len)
    }
    fn fstat(&mut self) -> Ticket {
        self.meter.polls.lock().unwrap().push(self.id);
        self.inner_mut().fstat()
    }
    fn stat_path(&mut self) -> Ticket {
        self.meter.polls.lock().unwrap().push(self.id);
        self.inner_mut().stat_path()
    }
    fn pending(&self) -> usize {
        self.inner().pending()
    }
    fn next(&mut self, timeout: Option<Duration>) -> Option<Completion> {
        let c = self.inner_mut().next(timeout)?;
        if let Some(data) = c.data() {
            self.meter
                .stream_bytes
                .fetch_add(data.len() as u64, Ordering::SeqCst);
            self.meter.stream_responses.fetch_add(1, Ordering::SeqCst);
        }
        Some(c)
    }
    fn waker(&self) -> Wake {
        self.inner().waker()
    }
    fn now(&self) -> Instant {
        self.inner().now()
    }
    fn reopen(mut self: Box<Self>, path: &str) -> Result<Box<dyn Ranged>, Refused> {
        self.meter.returned.lock().unwrap().push(self.id);
        self.back().into_ranged(path)
    }
    fn into_connection(mut self: Box<Self>) -> Box<dyn RemoteFs + Send> {
        self.meter.returned.lock().unwrap().push(self.id);
        self.back()
    }
    fn close(mut self: Box<Self>) {
        self.meter.closed.lock().unwrap().push(self.id);
        if let Some(inner) = self.inner.take() {
            inner.close();
            self.meter.live.fetch_sub(1, Ordering::SeqCst);
        }
    }
}
