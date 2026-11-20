//! What a session's request pipe has taken (06 §2). The crate writes requests from its own flush
//! task and says nothing of when they reach the pipe, so the pipe it is handed is [`Counted`]:
//! the writes and flushes that complete are recorded in [`Writes`], framed as SFTP packets. The
//! transport waits on it when it hands a session back: the handle's `CLOSE` has been written
//! once a whole `CLOSE` packet has gone into the pipe and a flush has completed after it.

use std::io::IoSlice;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use tokio::io::AsyncWrite;
use tokio::sync::Notify;

/// `SSH_FXP_CLOSE`.
const CLOSE: u8 = 4;

/// A session's request pipe, as far as it has taken bytes.
#[derive(Default)]
pub(crate) struct Writes {
    framing: Mutex<Framing>,
    changed: Notify,
    /// Tests: while closed, the pipe takes nothing (a pipe the far end has stopped draining).
    #[cfg(test)]
    pub gate: Gate,
}

/// The packets written so far, followed byte by byte through their lengths.
#[derive(Default)]
struct Framing {
    /// The current packet's 4-byte length and its type, as far as seen.
    head: [u8; 5],
    have: usize,
    /// Bytes of the current packet still to come after its head.
    left: u64,
    /// Whole `CLOSE` packets written.
    closes: u64,
    /// Flushes completed.
    flushes: u64,
    /// `flushes` when the last whole `CLOSE` was written.
    flushes_at_close: u64,
}

impl Framing {
    fn wrote(&mut self, mut bytes: &[u8]) {
        while !bytes.is_empty() {
            if self.left > 0 {
                let n = self.left.min(bytes.len() as u64);
                self.left -= n;
                bytes = &bytes[n as usize..];
                if self.left == 0 {
                    self.ended();
                }
                continue;
            }
            self.head[self.have] = bytes[0];
            self.have += 1;
            bytes = &bytes[1..];
            if self.have == self.head.len() {
                let len = u32::from_be_bytes(self.head[..4].try_into().unwrap());
                // The length counts the type byte, already seen.
                self.left = (len as u64).saturating_sub(1);
                if self.left == 0 {
                    self.ended();
                }
            }
        }
    }

    /// The current packet has been written whole.
    fn ended(&mut self) {
        if self.head[4] == CLOSE {
            self.closes += 1;
            self.flushes_at_close = self.flushes;
        }
        self.have = 0;
    }
}

impl Writes {
    /// Whole `CLOSE` packets written so far.
    pub(crate) fn closes(&self) -> u64 {
        self.framing.lock().unwrap().closes
    }

    /// Resolves once a `CLOSE` after the first `before` has been written whole and a flush has
    /// completed after it.
    pub(crate) async fn closed_after(&self, before: u64) {
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            {
                let f = self.framing.lock().unwrap();
                if f.closes > before && f.flushes > f.flushes_at_close {
                    return;
                }
            }
            changed.await;
        }
    }

    fn wrote(&self, bufs: &[IoSlice<'_>], mut n: usize) {
        let mut f = self.framing.lock().unwrap();
        for buf in bufs {
            let take = n.min(buf.len());
            f.wrote(&buf[..take]);
            n -= take;
            if n == 0 {
                break;
            }
        }
        drop(f);
        self.changed.notify_waiters();
    }

    fn flushed(&self) {
        self.framing.lock().unwrap().flushes += 1;
        self.changed.notify_waiters();
    }
}

/// A test's gate on the pipe.
#[cfg(test)]
#[derive(Default)]
pub(crate) struct Gate {
    closed: std::sync::atomic::AtomicBool,
    waiting: Mutex<Vec<std::task::Waker>>,
}

#[cfg(test)]
impl Gate {
    pub(crate) fn close(&self) {
        self.closed.store(true, std::sync::atomic::Ordering::SeqCst);
    }

    pub(crate) fn open(&self) {
        self.closed
            .store(false, std::sync::atomic::Ordering::SeqCst);
        for w in self.waiting.lock().unwrap().drain(..) {
            w.wake();
        }
    }

    fn poll_open(&self, cx: &mut Context<'_>) -> Poll<()> {
        use std::sync::atomic::Ordering::SeqCst;
        if !self.closed.load(SeqCst) {
            return Poll::Ready(());
        }
        self.waiting.lock().unwrap().push(cx.waker().clone());
        // Opened between the check and the registration: go on.
        match self.closed.load(SeqCst) {
            true => Poll::Pending,
            false => Poll::Ready(()),
        }
    }
}

/// A session's request pipe, recording into [`Writes`] what it takes.
pub(crate) struct Counted<W> {
    inner: W,
    writes: Arc<Writes>,
}

impl<W> Counted<W> {
    pub(crate) fn new(inner: W, writes: Arc<Writes>) -> Self {
        Self { inner, writes }
    }
}

impl<W: AsyncWrite + Unpin> AsyncWrite for Counted<W> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        #[cfg(test)]
        std::task::ready!(self.writes.gate.poll_open(cx));
        let got = Pin::new(&mut self.inner).poll_write(cx, buf);
        if let Poll::Ready(Ok(n)) = got {
            self.writes.wrote(&[IoSlice::new(buf)], n);
        }
        got
    }

    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[IoSlice<'_>],
    ) -> Poll<std::io::Result<usize>> {
        #[cfg(test)]
        std::task::ready!(self.writes.gate.poll_open(cx));
        let got = Pin::new(&mut self.inner).poll_write_vectored(cx, bufs);
        if let Poll::Ready(Ok(n)) = got {
            self.writes.wrote(bufs, n);
        }
        got
    }

    fn is_write_vectored(&self) -> bool {
        self.inner.is_write_vectored()
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        let got = Pin::new(&mut self.inner).poll_flush(cx);
        if let Poll::Ready(Ok(())) = got {
            self.writes.flushed();
        }
        got
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn packet(kind: u8, body: usize) -> Vec<u8> {
        let mut p = ((body + 1) as u32).to_be_bytes().to_vec();
        p.push(kind);
        p.extend(std::iter::repeat_n(7u8, body));
        p
    }

    /// The framing counts a `CLOSE` only once it has been written whole, however the writes cut
    /// the packets, and only a flush after it completes the wait.
    #[test]
    fn a_close_counts_once_written_whole_and_flushed() {
        let w = Writes::default();
        let mut stream = packet(5, 300); // a READ
        stream.extend(packet(CLOSE, 8));
        stream.extend(packet(8, 8)); // an FSTAT
        let close_end = 4 + 1 + 300 + 4 + 1 + 8;
        // Cut anywhere: one byte, then the rest in odd pieces.
        for cut in [1, 3, 5, 6, 304, 305, 309, close_end - 1, close_end] {
            let w = Writes::default();
            w.wrote(&[IoSlice::new(&stream[..cut])], cut);
            assert_eq!(w.closes(), (cut >= close_end) as u64, "cut at {cut}");
            let rest = &stream[cut..];
            w.wrote(
                &[IoSlice::new(&rest[..2]), IoSlice::new(&rest[2..])],
                rest.len(),
            );
            assert_eq!(w.closes(), 1, "cut at {cut}");
        }
        w.wrote(&[IoSlice::new(&stream)], close_end);
        assert_eq!(w.closes(), 1);
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .start_paused(true)
            .build()
            .unwrap();
        // Paused time: a wait that cannot end passes its second at once.
        let done = |w: &Writes| {
            let wait = async {
                tokio::time::timeout(std::time::Duration::from_secs(1), w.closed_after(0)).await
            };
            rt.block_on(wait).is_ok()
        };
        assert!(!done(&w), "written, not yet flushed");
        w.flushed();
        assert!(done(&w), "written and flushed");
    }
}
