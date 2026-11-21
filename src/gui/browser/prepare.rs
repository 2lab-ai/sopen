//! What Enter on a file does before the viewer shows, on a worker: a connection of the viewer's
//! own and the kind (spec §3: extension, then a sniff). A remote file the hex window shows,
//! and remote text of at most [`TEXT_STREAM_MAX`], then open as a stream on that connection
//! (06 §7.1, §7.2): nothing is fetched. Anything else is made local exactly as the standalone
//! routes do it (fetched into the cache, claimed and recorded —
//! [`cache::fetch_claimed_cancellable`]; a local file read in place, an image copied), then
//! whatever is slow: decoding the image, probing the media, indexing the text.
//!
//! And its preview form ([`preview`], 02 §4.2–§4.4): the same viewers, prepared on the window's
//! one preview connection, within the budgets of 02 §4.3 (an info card when one is hit), reading
//! without claiming the cache — a remote file into a private copy under `.preview`, a local one in
//! place — and told they are previews; the hand-over ([`Preview::hand_over`]) makes the viewer a
//! viewer again and publishes the copy into the cache by a hard link.

use super::Connect;
use crate::cache::{self, Claim, Fetched, PreviewName, PreviewSpace};
use crate::ffmpeg::Input;
use crate::gui::hex::{self, HexFile};
use crate::gui::image::{self, Doc, ImageFile};
use crate::gui::media::{self, Media};
use crate::gui::streamed::{self, Lease};
use crate::gui::text::{self, TextDoc};
use crate::gui::{chrome, Embedded, Origin, Outcome, Placeholder, Reconnect, Window};
use crate::kind::{self, Kind};
use crate::remote::{RemoteFs, Stat};
use crate::stream::{Opening, Source, Waker};
use anyhow::{anyhow, bail, Context as _, Result};
use eframe::egui;
use sha2::Digest as _;
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

/// A file ready for its viewer. `claim` holds the cached copy for as long as the viewer shows
/// it (a hex file holds its own).
pub enum Prepared {
    Image {
        file: ImageFile,
        doc: Doc,
        notices: Vec<String>,
        claim: Option<Claim>,
    },
    Media {
        origin: Origin,
        media: Box<Media>,
        notices: Vec<String>,
        claim: Option<Claim>,
    },
    Hex {
        file: HexFile,
        notices: Vec<String>,
    },
    Text {
        doc: TextDoc,
        notices: Vec<String>,
        claim: Option<Claim>,
    },
}

/// Where the file is and how to reach it.
pub struct Request<'a> {
    pub connect: &'a Connect,
    /// `[user@]host`; `None` on this machine.
    pub label: Option<&'a str>,
    pub abs: &'a str,
    pub cache: &'a Path,
    /// Repaints the window: what a stream opened here wakes (06 §5).
    pub wake: &'a Waker,
    /// When the open began — the Enter (06 §11's `open`).
    pub began: Instant,
}

impl Request<'_> {
    fn show(&self) -> String {
        match self.label {
            Some(label) => format!("{label}:{}", self.abs),
            None => self.abs.to_string(),
        }
    }

    /// The viewer's connection, opened again if the session dies while it shows the file.
    fn reconnect(&self) -> Option<Reconnect> {
        self.label.map(|_| {
            let connect = self.connect.clone();
            Box::new(move || connect()) as Reconnect
        })
    }

    fn origin(&self, fs: Box<dyn RemoteFs + Send>, at: Stat) -> Origin {
        Origin::new(
            fs,
            self.reconnect(),
            self.label.map(str::to_string),
            self.abs.to_string(),
            at,
        )
    }

    /// The remote file as a stream on `fs`, the connection that resolved, stat'ed and sniffed
    /// it (06 §2): nothing is fetched, and `fs` is the stream's from here on. Its `Origin` is a
    /// placeholder that connects on its first use and until then takes the server as able to
    /// replace a file. A failure is the error: never a quiet whole fetch instead.
    fn stream(
        &self,
        label: &str,
        fs: Box<dyn RemoteFs + Send>,
        sniff: u64,
        wake: Waker,
    ) -> Result<(Source, Origin)> {
        let opening = Opening {
            label: self.show(),
            began: self.began,
            sniff,
        };
        let dial = Some(self.connect.clone());
        let opened =
            streamed::open(fs, self.abs, opening, dial, wake, None).map_err(|not| not.error)?;
        let origin = Origin::placeholder(
            self.reconnect(),
            label.to_string(),
            self.abs.to_string(),
            opened.at,
            Placeholder::YES,
        );
        Ok((opened.source, origin))
    }
}

/// Remote text of at most this size is read through a stream (06 §7.2 W5a: its index pass
/// then covers the whole file, so line numbers, End and highlighting are as today). A larger
/// one keeps the whole fetch until W5b: a capped index would show its first 16 MiB as the
/// whole file.
pub const TEXT_STREAM_MAX: u64 = text::HIGHLIGHT_MAX;

/// Whether a remote text file of `size` bytes is read through a stream.
pub fn text_streams(size: u64) -> bool {
    size <= TEXT_STREAM_MAX
}

/// Makes the file at `req.abs` ready for its viewer. `progress` gets a fetch's `(done, total)`;
/// `cancel` stops it ([`crate::remote::Cancelled`]). The path is resolved first, as the CLI
/// resolves its argument: a link is opened (and saved) as the file it points at (spec §5).
pub fn prepare(
    req: &Request,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<Prepared> {
    prepare_with(req, cancel, progress, None)
}

/// [`prepare`] for Enter on a preview's info card (02 §4.4): when the card's private copy is
/// complete and the file is unchanged since (size, mtime), that copy is published into the cache
/// ([`cache::publish_claimed`]: a hard link, claimed and recorded) instead of read again;
/// otherwise the file is fetched as Enter fetches it.
pub fn prepare_reusing(
    req: &Request,
    reuse: Reuse,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<Prepared> {
    prepare_with(req, cancel, progress, Some(reuse))
}

fn prepare_with(
    req: &Request,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(u64, u64),
    reuse: Option<Reuse>,
) -> Result<Prepared> {
    let mut fs = (req.connect)()?;
    let abs = fs.realpath(req.abs)?;
    let req = &Request { abs: &abs, ..*req };
    let stat = fs.stat(req.abs)?;
    if stat.is_dir {
        bail!("{} is a folder", req.show());
    }
    if !stat.is_file {
        bail!(
            "{} is not a regular file; sopen does not open it",
            req.show()
        );
    }
    let (kind, sniff) = kind::detect_sniffed(fs.as_mut(), req.abs, &stat)?;
    let host = req.label.unwrap_or(cache::LOCAL_HOST).to_string();
    let mut reuse = reuse.filter(|r| r.abs == req.abs && unchanged(&r.copy.at, &stat));
    let mut fetch = |fs: &mut Box<dyn RemoteFs + Send>| -> Result<Fetched> {
        if let Some(r) = reuse.take() {
            let published = cache::publish_claimed(
                r.copy.name.path(),
                &host,
                req.abs,
                req.cache,
                r.copy.at.sha256.as_ref(),
            );
            // The copy is the cached one now; its preview name goes with `r`.
            match published {
                Ok((claim, notices)) => {
                    return Ok(Fetched {
                        claim,
                        at: r.copy.at,
                        notices,
                    });
                }
                Err(e) => crate::debug!("the preview copy was not published: {e:#}"),
            }
        }
        let (fetched, _ms) = cache::fetch_claimed_cancellable(
            fs.as_mut(),
            &host,
            req.abs,
            req.cache,
            progress,
            cancel,
        )?;
        Ok(fetched)
    };
    let local = req.label.is_none();
    match kind {
        Kind::Image => {
            // Always a copy, as the standalone route makes it: Save needs the bytes as opened.
            let fetched = fetch(&mut fs)?;
            let bytes = std::fs::read(&fetched.claim.local)
                .with_context(|| format!("cannot read {}", fetched.claim.local.display()))?;
            let doc = Doc::decode(&bytes)?;
            let file = ImageFile::new(req.origin(fs, fetched.at), bytes, req.cache);
            Ok(Prepared::Image {
                file,
                doc,
                notices: fetched.notices,
                claim: Some(fetched.claim),
            })
        }
        Kind::Video | Kind::Audio => {
            let fetched = if local { None } else { Some(fetch(&mut fs)?) };
            let (path, at) = match &fetched {
                Some(f) => (f.claim.local.clone(), f.at),
                None => (PathBuf::from(req.abs), stat),
            };
            if kind::refine_local(&path, req.abs, kind) == Kind::Other {
                // A `.ts` ffprobe cannot read as media: the hex view, on the copy already here.
                return hex_of(req, fs, stat, fetched);
            }
            let media = Media::probe(&path)?;
            let (notices, claim) = match fetched {
                Some(f) => (f.notices, Some(f.claim)),
                None => (Vec::new(), None),
            };
            Ok(Prepared::Media {
                origin: req.origin(fs, at),
                media: Box::new(media),
                notices,
                claim,
            })
        }
        Kind::Text => {
            if let Some(label) = req.label.filter(|_| text_streams(stat.size)) {
                text::warm();
                let (source, _origin) = req.stream(label, fs, sniff, req.wake.clone())?;
                // The handle decides: a file that grew past the limit since its stat is
                // fetched, on a connection of its own (this one is the stream's, closed here).
                if text_streams(source.size()) {
                    return Ok(Prepared::Text {
                        doc: TextDoc::streamed(source),
                        notices: Vec::new(),
                        claim: None,
                    });
                }
                drop(source);
                fs = (req.connect)()?;
            }
            let fetched = if local { None } else { Some(fetch(&mut fs)?) };
            let path = match &fetched {
                Some(f) => f.claim.local.clone(),
                None => PathBuf::from(req.abs),
            };
            text::warm();
            let doc = TextDoc::open_cancellable(&path, cancel)?;
            let (notices, claim) = match fetched {
                Some(f) => (f.notices, Some(f.claim)),
                None => (Vec::new(), None),
            };
            Ok(Prepared::Text {
                doc,
                notices,
                claim,
            })
        }
        Kind::Other | Kind::Directory => match req.label {
            // 06 §7.1: no fetch, whatever the size.
            Some(label) => {
                let repaint = hex::Repaint::default();
                let (source, origin) = req.stream(label, fs, sniff, repaint.waker())?;
                Ok(Prepared::Hex {
                    file: HexFile::streamed(origin, source, repaint, req.cache),
                    notices: Vec::new(),
                })
            }
            None => hex_of(req, fs, stat, None),
        },
    }
}

/// The hex view of the file: the copy fetched, or a local file read in place (as the binary
/// route's hex window opens it).
fn hex_of(
    req: &Request,
    fs: Box<dyn RemoteFs + Send>,
    stat: Stat,
    fetched: Option<Fetched>,
) -> Result<Prepared> {
    let (path, at, notices, claim) = match fetched {
        Some(f) => (f.claim.local.clone(), f.at, f.notices, Some(f.claim)),
        None => (PathBuf::from(req.abs), stat, Vec::new(), None),
    };
    let base = hex::Base::open(&path)?;
    Ok(Prepared::Hex {
        file: HexFile::new(req.origin(fs, at), base, claim, req.cache),
        notices,
    })
}

/// Whether a remote file of this kind and size is viewed through a stream (06 §13 W8a): what
/// the hex window shows, whatever its size, and text of at most [`TEXT_STREAM_MAX`]. An image
/// and a medium are still read whole.
fn streams(kind: Kind, size: u64) -> bool {
    match kind {
        Kind::Other | Kind::Directory => true,
        Kind::Text => text_streams(size),
        Kind::Image | Kind::Video | Kind::Audio => false,
    }
}

/// The file has not changed since `then` as far as a stat tells (size and modification time).
fn unchanged(then: &Stat, now: &Stat) -> bool {
    now.is_file && now.size == then.size && now.mtime == then.mtime
}

// ---------------------------------------------------------------------------------------------
// The preview (02 §4).

/// What a preview may cost (02 §4.3). Hitting one stops the preparation and shows the info card.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Bytes read from a remote: decided from the stat, before anything is read.
    pub read: u64,
    /// An image's encoded bytes, and its pixels (from its header, before it is decoded).
    pub image_bytes: u64,
    pub pixels: u64,
    /// The bytes of a text file its index reads; the preview shows their lines.
    pub text: u64,
    /// Preparation after the read (text, an image, hex rows).
    pub time: Duration,
    /// A video's or a sound's `ffprobe`, and a video's first frame: each its own time.
    pub probe: Duration,
    pub frame: Duration,
    /// A sound's waveform: how much of the sound it reads, and its time.
    pub sound: Duration,
    pub waveform: Duration,
}

impl Limits {
    /// 02 §4.3: 32 MiB read, images 64 MiB and 64 megapixels (256 MiB of RGBA), text indexed to
    /// 4 MiB, 5 s; a video's `ffprobe` and first frame 5 s each; a sound's `ffprobe` 5 s, then
    /// the waveform of its first 10 minutes in 10 s.
    pub const PREVIEW: Limits = Limits {
        read: 32 << 20,
        image_bytes: 64 << 20,
        pixels: 64 << 20,
        text: 4 << 20,
        time: Duration::from_secs(5),
        probe: Duration::from_secs(5),
        frame: Duration::from_secs(5),
        sound: Duration::from_secs(600),
        waveform: Duration::from_secs(10),
    };
}

/// A preview's budget, shared by its preparation and the window's preview host. Lifted — Enter
/// or F3 on the file while it is still being prepared (02 §4.4) — the preparation goes on under
/// the viewer's rules from then on: no limit, no deadline.
pub struct Budget {
    limits: Limits,
    lifted: AtomicBool,
    /// When the read ended: the deadline counts from there.
    read_ended: Mutex<Option<Instant>>,
    /// The time (tests move it).
    clock: Box<dyn Fn() -> Instant + Send + Sync>,
    /// The ffprobe or ffmpeg a video's or a sound's preparation runs right now: whoever stops
    /// the preparation kills it here ([`Budget::stop_children`]).
    children: media::preview::Children,
    /// Tests: the ffmpeg and ffprobe a preparation runs (a stand-in that stalls), instead of
    /// the installed ones.
    #[cfg(test)]
    tools: Mutex<Option<crate::ffmpeg::Tools>>,
    /// Tests: called after every chunk of a local read with the bytes read so far (Enter at a
    /// chosen point of it).
    #[cfg(test)]
    after_chunk: Mutex<Option<AfterChunk>>,
}

/// Tests: what runs after every chunk of a local read ([`Budget::after_chunk`]).
#[cfg(test)]
type AfterChunk = Box<dyn FnMut(u64) + Send>;

impl std::fmt::Debug for Budget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Budget")
            .field("limits", &self.limits)
            .field("lifted", &self.lifted())
            .finish_non_exhaustive()
    }
}

impl Budget {
    pub fn new(limits: Limits) -> Arc<Self> {
        Self::with_clock(limits, Box::new(Instant::now))
    }

    /// [`Budget::new`] on a clock of the caller's (tests: a deadline reached in the middle of a
    /// preparation, a read that takes long).
    pub(crate) fn with_clock(
        limits: Limits,
        clock: Box<dyn Fn() -> Instant + Send + Sync>,
    ) -> Arc<Self> {
        Arc::new(Self {
            limits,
            lifted: AtomicBool::new(false),
            read_ended: Mutex::new(None),
            clock,
            children: Default::default(),
            #[cfg(test)]
            tools: Mutex::new(None),
            #[cfg(test)]
            after_chunk: Mutex::new(None),
        })
    }

    /// A preview's budget, 02 §4.3's.
    pub fn preview() -> Arc<Self> {
        Self::new(Limits::PREVIEW)
    }

    /// No limit and no deadline from now on (the preparation becomes the viewer's opening).
    pub fn lift(&self) {
        self.lifted.store(true, Ordering::SeqCst);
    }

    pub fn lifted(&self) -> bool {
        self.lifted.load(Ordering::SeqCst)
    }

    /// `value` while the budget holds; `None` once lifted.
    fn limit(&self, value: u64) -> Option<u64> {
        (!self.lifted()).then_some(value)
    }

    /// Stops the ffprobe or ffmpeg the preparation runs, if one runs now, and lets none start:
    /// called with its stop flag set, by whoever does not wait for its worker (the window
    /// closing). Only the signal is sent here — it is called on a frame; the worker reaps.
    pub fn stop_children(&self) {
        self.children.stop();
    }

    /// The ffmpeg and ffprobe a preparation runs.
    fn tools(&self) -> Result<crate::ffmpeg::Tools> {
        #[cfg(test)]
        if let Some(tools) = self.tools.lock().unwrap().clone() {
            return Ok(tools);
        }
        crate::ffmpeg::resolve()
    }

    /// Tests: the preparation runs `tools` instead of the installed ffmpeg and ffprobe.
    #[cfg(test)]
    pub(crate) fn with_tools(self: Arc<Self>, tools: crate::ffmpeg::Tools) -> Arc<Self> {
        *self.tools.lock().unwrap() = Some(tools);
        self
    }

    fn read_done(&self) {
        *self.read_ended.lock().unwrap_or_else(|p| p.into_inner()) = Some((self.clock)());
    }

    /// The preparation may go on: not cancelled, and not past its deadline.
    fn check(&self, cancel: &AtomicBool) -> Result<()> {
        if cancel.load(Ordering::Relaxed) {
            return Err(anyhow!(crate::remote::Cancelled));
        }
        let ended = *self.read_ended.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(at) = ended.filter(|_| !self.lifted()) {
            if (self.clock)().saturating_duration_since(at) >= self.limits.time {
                return Err(anyhow!(OverBudget(format!(
                    "the preview took over {} s: Enter opens it",
                    self.limits.time.as_secs()
                ))));
            }
        }
        Ok(())
    }
}

/// A video's or a sound's preparation, watched ([`media::preview::Watch`]): stopped by the
/// preview's cancel, and — while the budget holds — by the time of the step it is in (02 §4.3:
/// `ffprobe`, the first frame and the waveform each have their own, in place of the one time
/// the other kinds have).
struct MediaWatch<'a> {
    budget: &'a Budget,
    cancel: &'a AtomicBool,
    step: Mutex<Option<(media::preview::Step, Instant)>>,
}

impl MediaWatch<'_> {
    /// Cancelled or not: no step's time is looked at.
    fn cancelled(&self) -> Result<()> {
        if self.cancel.load(Ordering::Relaxed) {
            return Err(anyhow!(crate::remote::Cancelled));
        }
        Ok(())
    }
}

/// `2 s`, `0.3 s`: a step's time on the info card.
fn secs_text(time: Duration) -> String {
    if time.subsec_nanos() == 0 {
        format!("{} s", time.as_secs())
    } else {
        format!("{:.1} s", time.as_secs_f64())
    }
}

impl media::preview::Watch for MediaWatch<'_> {
    fn begin(&self, step: media::preview::Step) {
        *self.step.lock().unwrap_or_else(|p| p.into_inner()) = Some((step, (self.budget.clock)()));
    }

    fn check(&self) -> Result<()> {
        use media::preview::Step;
        let step = *self.step.lock().unwrap_or_else(|p| p.into_inner());
        let now = (self.budget.clock)();
        self.cancelled()?;
        let Some((step, since)) = step.filter(|_| !self.budget.lifted()) else {
            return Ok(());
        };
        let limits = &self.budget.limits;
        let (what, time) = match step {
            Step::Probe => ("ffprobe", limits.probe),
            Step::Frame => ("the first frame", limits.frame),
            Step::Wave => ("the waveform", limits.waveform),
            Step::Encoder => ("the encoder check", limits.probe),
        };
        if now.saturating_duration_since(since) >= time {
            return Err(over(format!(
                "{what} took over {}: Enter opens it",
                secs_text(time)
            )));
        }
        Ok(())
    }

    fn sound(&self) -> Option<f64> {
        (!self.budget.lifted()).then_some(self.budget.limits.sound.as_secs_f64())
    }
}

/// A preview stopped at its budget, or at what it does not show: the info card's last line.
#[derive(Debug)]
struct OverBudget(String);

impl std::fmt::Display for OverBudget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for OverBudget {}

const OVER_LIMIT: &str = "over the preview limit: Enter opens it";

fn over(why: impl Into<String>) -> anyhow::Error {
    anyhow!(OverBudget(why.into()))
}

/// The card's line for why the preparation stopped: the reason itself, first.
fn why(e: &anyhow::Error) -> String {
    if let Some(OverBudget(why)) = e.chain().find_map(|c| c.downcast_ref::<OverBudget>()) {
        return why.clone();
    }
    if let Some(unreadable) = unreadable(e) {
        return unreadable.0.clone();
    }
    format!("{e:#}")
}

fn unreadable(e: &anyhow::Error) -> Option<&media::preview::Unreadable> {
    e.chain().find_map(|c| c.downcast_ref())
}

/// Under the reason, what could not be done to which file — the file named once, as its owner
/// knows it (`host:/abs`; a remote one was read from a private copy, which is never named).
fn what(e: &anyhow::Error, host: &str, abs: &str) -> Option<String> {
    unreadable(e)?;
    let file = if host == cache::LOCAL_HOST {
        abs.to_string()
    } else {
        format!("{host}:{abs}")
    };
    Some(format!("ffprobe cannot read {file}"))
}

/// The file a preview shows, and where its copies go.
pub struct PreviewRequest<'a> {
    /// The window's one preview connection (02 §4.4): opened by `connect` at the first preview,
    /// and again when its session died; previews are prepared one at a time on it.
    pub conn: &'a mut Option<Box<dyn RemoteFs + Send>>,
    /// Opens a connection to the folder's host (the preview's, and the viewer's own after the
    /// hand-over).
    pub connect: &'a Connect,
    /// `[user@]host`; `None` on this machine.
    pub label: Option<&'a str>,
    /// The file, as the list names it (a link is previewed as what it points at).
    pub abs: &'a str,
    pub cache: &'a Path,
    /// The window's preview copies.
    pub space: &'a PreviewSpace,
    /// Where the preview connection comes back from while a preview's stream has it.
    pub back: &'a Back,
    /// Repaints the window: what a stream opened here wakes (06 §5).
    pub wake: &'a Waker,
    /// When the bar came to rest on the file (06 §11's `open`).
    pub began: Instant,
}

/// The window's preview connection while a preview's stream has it (06 §13): the stream reads
/// on it — it is its transport — and gives it back when it closes without having become a
/// viewer ([`streamed::Lease`]). The next preparation takes it from here, so the window keeps
/// its one preview session; a stream handed over keeps it for good, and nothing comes back.
pub type Back = Mutex<Option<Receiver<Box<dyn RemoteFs + Send>>>>;

/// How long a preparation waits for the preview connection to come back from the stream
/// before it. The stream gives it back as soon as it closes (its handle's close is written,
/// not waited for: 06 §2); past this, the preparation opens a session of its own and the
/// late one is closed when it comes.
const BACK_WAIT: Duration = Duration::from_secs(2);

/// The preview connection a closed stream gave back, if one had it.
fn returned(back: &Back, cancel: &AtomicBool) -> Result<Option<Box<dyn RemoteFs + Send>>> {
    let Some(rx) = back.lock().unwrap_or_else(|p| p.into_inner()).take() else {
        return Ok(None);
    };
    let started = Instant::now();
    loop {
        match rx.recv_timeout(Duration::from_millis(20)) {
            Ok(conn) => return Ok(Some(conn)),
            // Handed over (the viewer has it), or its stream's thread is gone.
            Err(RecvTimeoutError::Disconnected) => return Ok(None),
            Err(RecvTimeoutError::Timeout) if cancel.load(Ordering::Relaxed) => {
                // Still on its way: the next preparation waits for it.
                *back.lock().unwrap_or_else(|p| p.into_inner()) = Some(rx);
                return Err(anyhow!(crate::remote::Cancelled));
            }
            Err(RecvTimeoutError::Timeout) if started.elapsed() >= BACK_WAIT => return Ok(None),
            Err(RecvTimeoutError::Timeout) => {}
        }
    }
}

/// A link, on the info card: what it points at, or nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Link {
    To(String),
    Broken,
}

/// The info card (02 §4.2): what is known of a file that is not previewed, and why not.
#[derive(Debug, Clone)]
pub struct Card {
    /// As the list names it.
    pub name: String,
    pub kind: Option<Kind>,
    pub size: Option<u64>,
    pub mtime: Option<SystemTime>,
    pub link: Option<Link>,
    /// The last line: `over the preview limit: Enter opens it`, `image is W×H; Enter opens it`,
    /// the error.
    pub why: String,
    /// Under it, what could not be done to which file (`ffprobe cannot read host:/abs`): the
    /// part that gives way when the side is small — the reason above it is never cut.
    pub what: Option<String>,
}

/// A preview's private copy of a remote file, complete: its name (the copy goes when it does)
/// and the stat it was read at, with the SHA-256 of what was read.
struct Copy {
    name: PreviewName,
    at: Stat,
}

/// What a preview's viewer is made of, on the worker that prepared it.
enum Parts {
    Text {
        doc: TextDoc,
        title: String,
        name: String,
        live: bool,
    },
    Image {
        file: ImageFile,
        doc: Doc,
        shown: Option<::image::RgbaImage>,
    },
    Hex {
        file: HexFile,
    },
    /// Remote text read through a stream (06 §7.2): its index grows on the window's frames,
    /// as far as `index` while it is a preview.
    TextStream {
        source: Source,
        title: String,
        name: String,
        index: u64,
    },
    Media {
        origin: Origin,
        made: Box<media::preview::Prepared>,
    },
    Card(Card),
}

/// A preview made ready on a worker (it crosses to the window: [`Previewed::show`] makes its
/// viewer there).
pub struct Previewed {
    parts: Parts,
    copy: Option<Copy>,
    /// A streamed preview's hold on the window's preview connection.
    lease: Option<Lease>,
    host: String,
    /// The file, resolved (a link's target).
    abs: String,
    cache: PathBuf,
}

impl Previewed {
    /// The info card, when the file is not previewed.
    #[cfg_attr(not(test), allow(dead_code))] // Only the tests use it.
    pub fn card(&self) -> Option<&Card> {
        match &self.parts {
            Parts::Card(card) => Some(card),
            _ => None,
        }
    }

    /// The preview, on the window's thread: its viewer, told it is a preview
    /// ([`Window::set_preview`]), or the info card.
    pub fn show(self) -> Preview {
        let cache = &self.cache;
        let (embedded, card) = match self.parts {
            Parts::Text {
                doc,
                title,
                name,
                live,
            } => (text::embed(doc, title, &name, Vec::new(), live, None), None),
            Parts::Image { file, doc, shown } => (image::embed_preview(file, doc, shown), None),
            Parts::Hex { file } => (hex::embed(file, Vec::new()), None),
            // The one preview-index constant is this route's (02 §4.3): the viewer is told.
            Parts::TextStream {
                source,
                title,
                name,
                index,
            } => (
                text::embed_source(source, title, &name, Vec::new(), None, Some(index)),
                None,
            ),
            Parts::Media { origin, made } => (media::embed_preview(origin, *made, cache), None),
            Parts::Card(card) => (card_window(card.clone()), Some(card)),
        };
        let mut embedded = embedded;
        embedded.window.set_preview(true);
        Preview {
            embedded,
            card,
            shown: true,
            copy: self.copy,
            lease: self.lease,
            host: self.host,
            abs: self.abs,
            cache: self.cache,
        }
    }
}

/// A preview on screen: the viewer told it is one, or the info card. Dropped, its preparation's
/// copy goes with it.
pub struct Preview {
    pub embedded: Embedded,
    card: Option<Card>,
    /// Whether its window was last told it is on screen ([`Preview::set_shown`]).
    shown: bool,
    copy: Option<Copy>,
    /// A streamed preview's hold on the window's preview connection: dropped as a preview,
    /// its stream gives the connection back; handed over, the viewer keeps it.
    lease: Option<Lease>,
    host: String,
    abs: String,
    cache: PathBuf,
}

/// A preview handed over (Enter or F3 on it): the viewer, a viewer again, and the cached copy it
/// holds until it is gone.
pub struct HandedOver {
    /// The live window: its budgets lifted, its bars and input back, the work it stopped at its
    /// budget going on from there. Its `settle` holds the preview copy's name (the viewer reads
    /// the file by it) until the viewer has settled.
    pub embedded: Embedded,
    /// The cached copy, claimed and recorded (a remote file): held as Enter's claim is.
    pub hold: Option<Claim>,
    /// What the claim said (an earlier copy's unsaved edits moved aside, a copy of its own),
    /// printed already, for the status row.
    pub notices: Vec<String>,
}

/// A card's complete private copy, for Enter on it ([`prepare_reusing`]).
pub struct Reuse {
    abs: String,
    copy: Copy,
}

impl Preview {
    /// Tests: a preview around a window of the test's own (the preview host's input isolation,
    /// shown on a window that would react).
    #[cfg(test)]
    pub(crate) fn standing_in(embedded: Embedded) -> Self {
        Self {
            embedded,
            card: None,
            shown: true,
            copy: None,
            lease: None,
            host: cache::LOCAL_HOST.to_string(),
            abs: String::new(),
            cache: PathBuf::new(),
        }
    }

    /// The info card, when the file is not previewed.
    #[cfg_attr(not(test), allow(dead_code))] // Only the tests use it.
    pub fn card(&self) -> Option<&Card> {
        self.card.as_ref()
    }

    /// The window is drawn this pass, or held without being drawn (the browser shows a viewer
    /// over the panels): told to the window when it changes ([`Window::set_shown`]), so a
    /// streamed preview nobody sees does not poll its file (06 §5).
    pub fn set_shown(&mut self, shown: bool) {
        if std::mem::replace(&mut self.shown, shown) != shown {
            self.embedded.window.set_shown(shown);
        }
    }

    /// The hand-over (02 §4.4): the live window becomes the viewer (`set_preview(false)`).
    /// - A streamed preview (remote text and hex, 06 §13) moves its stream to the viewer, with
    ///   its cache, its readers and its session: no byte is read twice, nothing is published
    ///   into the cache, and the window's next remote preview opens a new preview session.
    /// - A remote file read into a private copy (an image) has that copy published into the
    ///   cache by a hard link — the same inode, nothing read again — then claimed and recorded
    ///   as Enter's fetch would be; the copy's own name stays until the viewer is gone.
    /// - A local file was read in place and stays so.
    ///
    /// An info card is not handed over: it comes back (Enter opens the file:
    /// [`Preview::into_reuse`]).
    pub fn hand_over(mut self) -> std::result::Result<HandedOver, Box<Preview>> {
        if self.card.is_some() {
            return Err(Box::new(self));
        }
        self.embedded.window.set_preview(false);
        // The viewer is on screen, whatever the preview was; its stream's session is its own.
        self.set_shown(true);
        if let Some(lease) = self.lease.take() {
            lease.keep();
        }
        let (mut hold, mut notices) = (None, Vec::new());
        if let Some(copy) = self.copy.take() {
            let published = cache::publish_claimed(
                copy.name.path(),
                &self.host,
                &self.abs,
                &self.cache,
                copy.at.sha256.as_ref(),
            );
            match published {
                Ok((claim, said)) => (hold, notices) = (Some(claim), said),
                Err(e) => {
                    let line = format!("not kept in sopen's cache: {e:#}");
                    eprintln!("sopen: {line}");
                    notices.push(line);
                }
            }
            let mut name = Some(copy.name);
            let mut settle = self.embedded.settle;
            self.embedded.settle = Box::new(move |wait| {
                let settled = settle(wait);
                if settled.is_some() {
                    drop(name.take());
                }
                settled
            });
        }
        Ok(HandedOver {
            embedded: self.embedded,
            hold,
            notices,
        })
    }

    /// The private copy a card's preparation completed, for Enter on the card
    /// ([`prepare_reusing`]); `None` when nothing was read (a local file, or a stop before the
    /// read).
    pub fn into_reuse(mut self) -> Option<Reuse> {
        let copy = self.copy.take()?;
        Some(Reuse {
            abs: std::mem::take(&mut self.abs),
            copy,
        })
    }
}

/// What a preview's preparation has learnt so far: what its card shows if it stops.
struct Known {
    /// As the list names it.
    name: String,
    abs: String,
    stat: Option<Stat>,
    kind: Option<Kind>,
    link: Option<Link>,
    copy: Option<Copy>,
    lease: Option<Lease>,
}

/// Prepares the preview of the file at `req.abs` (02 §4.2–§4.4), on the window's preview
/// connection, within `budget`: stops are checked between every step, at every read and every
/// MiB of the text index (`cancel`, the deadline). Nothing is claimed in the cache: a remote
/// file is read into a private copy (`<cache>/.preview/<window>/<n>`, 0600), a local one read in
/// place; a video's frame and a sound's waveform come from ffprobe and ffmpeg runs the same
/// stops reach ([`media::preview`]). A file that cannot be previewed — unreadable, vanished, a
/// budget hit — is the info card. `Err` only when cancelled ([`crate::remote::Cancelled`]).
pub fn preview(
    req: PreviewRequest,
    budget: &Budget,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<Previewed> {
    let (_, name) = crate::remote::split_parent(req.abs);
    let mut known = Known {
        name: name.to_string(),
        abs: req.abs.to_string(),
        stat: None,
        kind: None,
        link: None,
        copy: None,
        lease: None,
    };
    let host = req.label.unwrap_or(cache::LOCAL_HOST).to_string();
    let cache = req.cache.to_path_buf();
    match preview_parts(req, budget, cancel, progress, &mut known) {
        Ok(parts) => Ok(Previewed {
            parts,
            copy: known.copy,
            lease: known.lease,
            host,
            abs: known.abs,
            cache,
        }),
        Err(e) if crate::remote::is_cancelled(&e) => Err(e),
        Err(e) => {
            let card = Card {
                name: known.name,
                kind: known.kind,
                size: known.stat.map(|s| s.size),
                mtime: known.stat.map(|s| s.mtime),
                link: known.link,
                why: why(&e),
                what: what(&e, &host, &known.abs),
            };
            Ok(Previewed {
                parts: Parts::Card(card),
                copy: known.copy,
                lease: None,
                host,
                abs: known.abs,
                cache,
            })
        }
    }
}

fn preview_parts(
    req: PreviewRequest,
    budget: &Budget,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(u64, u64),
    known: &mut Known,
) -> Result<Parts> {
    let PreviewRequest {
        conn,
        connect,
        label,
        abs,
        cache,
        space,
        back,
        wake,
        began,
    } = req;
    // Cancel is looked at between every two requests: none starts once it is set.
    budget.check(cancel)?;
    // The stream of the preview before had the connection: it comes back as that one closes.
    if conn.is_none() {
        *conn = returned(back, cancel)?;
    }
    let opened_now = conn.is_none();
    if opened_now {
        *conn = Some(connect()?);
        budget.check(cancel)?;
    }
    let lstat = match conn.as_mut().expect("opened above").lstat_opt(abs) {
        Ok(lstat) => lstat,
        Err(e) => {
            budget.check(cancel)?;
            // Its session may have died since the last preview. Only then — a path-free request
            // fails too — once more, on a new one, the old one closed first: never two at once.
            // A path error (permission denied) is the file's.
            let fs = conn.as_mut().expect("opened above");
            if opened_now || fs.stat_opt("/").is_ok() {
                return Err(e);
            }
            budget.check(cancel)?;
            drop(conn.take());
            *conn = Some(connect()?);
            budget.check(cancel)?;
            conn.as_mut().expect("opened above").lstat_opt(abs)?
        }
    };
    let fs = conn.as_mut().expect("opened above");
    let Some(lstat) = lstat else {
        bail!("{abs}: no such file");
    };
    known.stat = Some(lstat);
    budget.check(cancel)?;
    if lstat.is_symlink && fs.stat_opt(abs)?.is_none() {
        known.link = Some(Link::Broken);
        bail!("the link points at nothing");
    }
    budget.check(cancel)?;
    // As Enter opens it: a link as the file it points at.
    let real = fs.realpath(abs)?;
    if lstat.is_symlink {
        known.link = Some(Link::To(real.clone()));
    }
    known.abs = real.clone();
    budget.check(cancel)?;
    let stat = fs.stat(&real)?;
    known.stat = Some(stat);
    if stat.is_dir {
        bail!("a folder");
    }
    if !stat.is_file {
        bail!("not a regular file; sopen does not open it");
    }
    let remote = label.is_some();
    let limits = budget.limits;
    // A remote file over the read budget is previewed only through a stream, which reads the
    // ranges it shows and no more (02 §4.3). Whether it is one is decided from the stat and
    // the name where the name says the kind — then nothing is read, the sniff's head included,
    // and the card says the kind its name gives it.
    let over_read = remote && budget.limit(limits.read).is_some_and(|max| stat.size > max);
    let name_says = kind::by_extension(crate::remote::split_parent(&real).1);
    if over_read && name_says.is_some_and(|kind| !streams(kind, stat.size)) {
        known.kind = name_says;
        return Err(over(OVER_LIMIT));
    }
    budget.check(cancel)?;
    let (kind, sniff) = kind::detect_sniffed(fs.as_mut(), &real, &stat)?;
    known.kind = Some(kind);
    let streamed = remote && streams(kind, stat.size);
    if over_read && !streamed {
        return Err(over(OVER_LIMIT));
    }
    if kind == Kind::Image
        && budget
            .limit(limits.image_bytes)
            .is_some_and(|max| stat.size > max)
    {
        return Err(over(OVER_LIMIT));
    }
    budget.check(cancel)?;
    if let Some(label) = label.filter(|_| streamed) {
        // 06 §13: the preview reads through a stream on the window's one preview session,
        // which becomes that stream's transport. Nothing is written to disk.
        let conn_taken = conn.take().expect("opened above");
        let (tx, rx) = std::sync::mpsc::channel();
        *back.lock().unwrap_or_else(|p| p.into_inner()) = Some(rx);
        let lease = Lease::new(tx);
        let opening = Opening {
            label: show(Some(label), &real),
            began,
            sniff,
        };
        let repaint = hex::Repaint::default();
        let waker = match kind {
            Kind::Text => wake.clone(),
            _ => repaint.waker(),
        };
        let dial = Some(connect.clone());
        let opened =
            match streamed::open(conn_taken, &real, opening, dial, waker, Some(lease.clone())) {
                Ok(opened) => opened,
                Err(not) => {
                    // The preview connection is the window's again, as it was.
                    lease.keep();
                    back.lock().unwrap_or_else(|p| p.into_inner()).take();
                    *conn = not.conn;
                    return Err(not.error);
                }
            };
        known.lease = Some(lease);
        budget.read_done();
        let source = opened.source;
        let parts = match kind {
            // The handle decides too: text that grew past the limit since its stat would be
            // shown short by a capped index (06 §7.2), so it is the card.
            Kind::Text if !text_streams(source.size()) => return Err(over(OVER_LIMIT)),
            Kind::Text => {
                text::warm();
                Parts::TextStream {
                    source,
                    title: show(Some(label), &real),
                    name: known.name.clone(),
                    index: limits.text,
                }
            }
            _ => {
                let connect = connect.clone();
                let origin = Origin::placeholder(
                    Some(Box::new(move || connect()) as Reconnect),
                    label.to_string(),
                    real.clone(),
                    opened.at,
                    Placeholder::YES,
                );
                Parts::Hex {
                    file: HexFile::streamed(origin, source, repaint, cache),
                }
            }
        };
        // Once more before handing the preview over: a cancel meanwhile drops it here (its
        // stream closes, and the connection comes back).
        budget.check(cancel)?;
        return Ok(parts);
    }
    if remote {
        known.copy = Some(read_copy(
            fs.as_mut(),
            &real,
            crate::remote::split_parent(&real).1,
            space,
            budget,
            cancel,
            progress,
        )?);
    }
    budget.read_done();
    budget.check(cancel)?;
    let (path, at) = match &known.copy {
        Some(copy) => (copy.name.path().to_path_buf(), copy.at),
        None => (PathBuf::from(&real), stat),
    };
    let origin = |at: Stat| -> Result<Origin> {
        Ok(match label {
            Some(label) => placeholder_origin(connect, label, fs.as_ref(), real.clone(), at),
            None => Origin::new(connect()?, None, None, real.clone(), at),
        })
    };
    let check = || budget.check(cancel);
    if matches!(kind, Kind::Video | Kind::Audio) {
        // Each of its steps has a time of its own (02 §4.3), and every ffprobe and ffmpeg it
        // runs is stopped — killed and reaped — the moment the preview is left or the step is
        // over its time.
        let watch = MediaWatch {
            budget,
            cancel,
            step: Mutex::new(None),
        };
        let tools = budget.tools()?;
        let input = Input::path(&path);
        // The file's own name (a link's target's): its extension says how it is rendered.
        let name = crate::remote::split_parent(&real).1;
        let found = media::preview::prepare(&tools, &input, name, kind, &budget.children, &watch)?;
        let parts = match found {
            media::preview::Found::Media(made) => {
                if made.media.video.is_none() {
                    known.kind = Some(Kind::Audio);
                }
                Parts::Media {
                    origin: origin(at)?,
                    made,
                }
            }
            // A `.ts` ffprobe cannot read as media: the hex view, as Enter shows it.
            media::preview::Found::NotMedia => Parts::Hex {
                file: HexFile::new(origin(at)?, hex::Base::open(&path)?, None, cache),
            },
        };
        // Once more before handing the preview over: a cancel meanwhile drops it here.
        watch.cancelled()?;
        return Ok(parts);
    }
    let parts = match kind {
        Kind::Image => {
            // The stat said it fits; the bytes decide (the file can have grown since).
            let bytes = read_bounded(&path, budget, &check)?;
            let (w, h) = image::dimensions(&bytes)?;
            if budget
                .limit(limits.pixels)
                .is_some_and(|max| u64::from(w) * u64::from(h) > max)
            {
                return Err(over(format!("image is {w}×{h}; Enter opens it")));
            }
            let doc = Doc::decode_checked(&bytes, &check)?;
            let shown = image::fitted(&doc);
            check()?;
            // A local file read in place: Save checks it against the bytes read, as against a
            // fetch's.
            let at = match at.sha256 {
                Some(_) => at,
                None => Stat {
                    sha256: Some(sha2::Sha256::digest(&bytes).into()),
                    ..at
                },
            };
            let file = ImageFile::new(origin(at)?, bytes, cache);
            Parts::Image { file, doc, shown }
        }
        Kind::Text => {
            text::warm();
            let doc = TextDoc::open_capped(&path, &|| budget.limit(limits.text), &check)?;
            Parts::Text {
                doc,
                title: show(label, &real),
                name: known.name.clone(),
                live: !remote,
            }
        }
        Kind::Other | Kind::Directory => {
            let base = hex::Base::open(&path)?;
            Parts::Hex {
                file: HexFile::new(origin(at)?, base, None, cache),
            }
        }
        Kind::Video | Kind::Audio => unreachable!("made above"),
    };
    // Once more before handing the preview over: a cancel meanwhile drops it here.
    check()?;
    Ok(parts)
}

/// `host:/abs`, or the local path.
fn show(label: Option<&str>, abs: &str) -> String {
    match label {
        Some(label) => format!("{label}:{abs}"),
        None => abs.to_string(),
    }
}

/// The local image at `path`, read whole through a handle of its own — while the budget holds
/// (looked at before every read: Enter lifts it in the middle), no further than one byte past its
/// encoded limit: that byte read and it is over the limit (the file grew since its size was
/// checked, or another took its name). Each read asks for at most what is left of the limit and
/// one byte. `check` after every read.
fn read_bounded(path: &Path, budget: &Budget, check: &dyn Fn() -> Result<()>) -> Result<Vec<u8>> {
    use std::io::Read as _;
    let mut file =
        std::fs::File::open(path).with_context(|| format!("cannot open {}", path.display()))?;
    let mut bytes = Vec::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let limit = budget.limit(budget.limits.image_bytes);
        let want = match limit {
            Some(max) => (max.saturating_sub(bytes.len() as u64) + 1).min(buf.len() as u64),
            None => buf.len() as u64,
        } as usize;
        let n = file
            .read(&mut buf[..want])
            .with_context(|| format!("cannot read {}", path.display()))?;
        if n == 0 {
            break;
        }
        #[cfg(test)]
        log_local_read(path, bytes.len() as u64, n as u64);
        bytes.extend_from_slice(&buf[..n]);
        #[cfg(test)]
        if let Some(hook) = budget.after_chunk.lock().unwrap().as_mut() {
            hook(bytes.len() as u64);
        }
        let limit = budget.limit(budget.limits.image_bytes);
        if limit.is_some_and(|max| bytes.len() as u64 > max) {
            return Err(over(OVER_LIMIT));
        }
        check()?;
    }
    check()?;
    Ok(bytes)
}

#[cfg(test)]
static LOCAL_READS: Mutex<Vec<(PathBuf, u64, u64)>> = Mutex::new(Vec::new());

#[cfg(test)]
fn log_local_read(path: &Path, at: u64, n: u64) {
    LOCAL_READS
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .push((path.to_path_buf(), at, n));
}

/// Tests: every read a preview made of the local file `path`, `(offset, bytes)`, in order.
#[cfg(test)]
fn local_reads(path: &Path) -> Vec<(u64, u64)> {
    LOCAL_READS
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .iter()
        .filter(|(p, _, _)| p == path)
        .map(|&(_, at, n)| (at, n))
        .collect()
}

/// Where a preview's read writes its copy, hashing what it writes: every response the server
/// sends is looked at before it is written — a cancel, or the copy past the read budget while it
/// holds, refuses it, which ends the read at that response (nothing more is asked of the
/// server).
struct Watched<'a, W> {
    inner: W,
    written: u64,
    /// What the attempts before this one took: the read budget is the whole preview's.
    spent: u64,
    hasher: sha2::Sha256,
    cancel: &'a AtomicBool,
    budget: &'a Budget,
    past_budget: bool,
}

impl<W: std::io::Write> std::io::Write for Watched<'_, W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if self.cancel.load(Ordering::Relaxed) {
            return Err(std::io::Error::other("cancelled"));
        }
        let max = self.budget.limit(self.budget.limits.read);
        if max.is_some_and(|max| self.spent + self.written + buf.len() as u64 > max) {
            self.past_budget = true;
            return Err(std::io::Error::other("over the preview limit"));
        }
        let n = self.inner.write(buf)?;
        self.hasher.update(&buf[..n]);
        self.written += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

/// Reads the remote file at `real` (named `name`) into a new private copy of the window's (02
/// §4.4: 0600, removed with its name on every way out), as a fetch reads it — the stat before and
/// after, read again once when they differ — with cancel looked at between every two requests
/// and at every response. While it holds, the read budget is one for the whole preview: a read
/// started again must fit in what the first left of it (else the card), and every response is
/// checked against it — a file that grew past its stat is read no further than the response that
/// crossed it; lifted meanwhile (Enter), the same read goes on to the end.
fn read_copy(
    fs: &mut dyn RemoteFs,
    real: &str,
    name: &str,
    space: &PreviewSpace,
    budget: &Budget,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<Copy> {
    use std::io::Write as _;
    let copy = space.name(name);
    let mut spent = 0u64;
    for attempt in 0..2 {
        budget.check(cancel)?;
        let before = fs.stat(real)?;
        budget.check(cancel)?;
        // A read started again must fit in what the first left of the budget. The first read's
        // size was decided at the preview's own stat; a file grown since is stopped by its
        // responses (below) — or, Enter coming meanwhile, read on whole.
        let left = budget
            .limit(budget.limits.read)
            .map(|max| max.saturating_sub(spent));
        if attempt > 0 && left.is_some_and(|left| before.size > left) {
            return Err(over(OVER_LIMIT));
        }
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(attempt == 0)
            .truncate(attempt > 0)
            .mode(0o600)
            .open(copy.path())
            .with_context(|| format!("cannot create {}", copy.path().display()))?;
        let mut out = Watched {
            inner: std::io::BufWriter::with_capacity(1 << 20, file),
            written: 0,
            spent,
            hasher: sha2::Sha256::new(),
            cancel,
            budget,
            past_budget: false,
        };
        let read = fs
            .read_to(real, &mut out, &mut |done| progress(done, before.size))
            .and_then(|n| out.flush().map(|()| n).map_err(anyhow::Error::from));
        budget.check(cancel)?;
        if out.past_budget {
            return Err(over(OVER_LIMIT));
        }
        spent += out.written;
        read?;
        let (size, sha256) = (out.written, out.hasher.finalize().into());
        let after = fs.stat_opt(real)?;
        let whole = before.missing == Some("size") || size == before.size;
        if whole && after.is_some_and(|a| unchanged(&before, &a)) {
            return Ok(Copy {
                name: copy,
                at: Stat {
                    sha256: Some(sha256),
                    ..before
                },
            });
        }
    }
    bail!("{real} kept changing while it was read; try again when it is quiet")
}

/// The `Origin` of a previewed remote viewer that read a copy (02 §4.4): a placeholder
/// connection ([`Origin::placeholder`]), so the viewer's first remote call reconnects through
/// `connect` and the viewer works on a session of its own from then on, never on the window's
/// preview connection. What the server can do is the preview connection's (the same server).
fn placeholder_origin(
    connect: &Connect,
    label: &str,
    fs: &dyn RemoteFs,
    abs: String,
    at: Stat,
) -> Origin {
    let connect = connect.clone();
    Origin::placeholder(
        Some(Box::new(move || connect()) as Reconnect),
        label.to_string(),
        abs,
        at,
        Placeholder {
            can_replace: fs.can_replace(),
            can_link: fs.can_link(),
        },
    )
}

// ---------------------------------------------------------------------------------------------
// The info card's window.

/// The info card's frame title.
const CARD_TITLE: &str = "File Info";

/// The info card drawn in the preview's side: its rows (name, kind, size exact and in units,
/// date and time, the link), then why it is a card. Nothing in it takes input.
fn card_window(card: Card) -> Embedded {
    Embedded {
        window: Box::new(CardWindow {
            card,
            painted: false,
        }),
        settle: Box::new(|_| Some(Outcome::default())),
    }
}

struct CardWindow {
    card: Card,
    painted: bool,
}

impl CardWindow {
    fn rows(&self) -> Vec<(&'static str, String)> {
        let c = &self.card;
        let mut rows = vec![("Name", c.name.clone())];
        rows.extend(c.kind.map(|k| ("Kind", k.as_str().to_string())));
        rows.extend(c.size.map(|n| {
            (
                "Size",
                format!("{} bytes ({})", hex::grouped(n), crate::human(n)),
            )
        }));
        let when = c.mtime.and_then(|t| {
            let secs = match t.duration_since(std::time::UNIX_EPOCH) {
                Ok(d) => d.as_secs() as i64,
                Err(e) => -(e.duration().as_secs() as i64),
            };
            chrome::local_tm(secs)
        });
        rows.extend(when.map(|tm| {
            (
                "Date",
                format!(
                    "{:04}-{:02}-{:02} {:02}:{:02}",
                    tm.tm_year + 1900,
                    tm.tm_mon + 1,
                    tm.tm_mday,
                    tm.tm_hour,
                    tm.tm_min
                ),
            )
        }));
        rows.extend(c.link.as_ref().map(|link| {
            (
                "Link",
                match link {
                    Link::To(target) => format!("<LINK> {target}"),
                    Link::Broken => "<BROKEN>".to_string(),
                },
            )
        }));
        rows
    }
}

impl Window for CardWindow {
    fn ui(&mut self, ui: &mut egui::Ui) {
        let rows = self.rows();
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ui, |ui| {
                let outer = ui.max_rect();
                let inside = chrome::frame(&ui.painter_at(outer), outer, CARD_TITLE, false);
                let painter = ui.painter_at(inside);
                let p = chrome::Palette::of(ui.visuals());
                let font = chrome::mono(ui);
                let row_h = ui.ctx().fonts_mut(|f| f.row_height(&font)).ceil();
                let label_w = 6.0 * chrome::CELL.x;
                let room = (inside.width() - label_w).max(chrome::CELL.x);
                let mut y = inside.top();
                for (label, value) in rows {
                    painter.text(
                        egui::Pos2::new(inside.left(), y),
                        egui::Align2::LEFT_TOP,
                        label,
                        font.clone(),
                        p.dim,
                    );
                    let galley = painter.layout(value, font.clone(), p.text, room);
                    let h = galley.size().y.max(row_h);
                    painter.galley(egui::Pos2::new(inside.left() + label_w, y), galley, p.text);
                    y += h;
                }
                y += row_h;
                // The reason first, whole; what it is about under it — that is what the
                // side's edge cuts when there is no room for both.
                let wide = inside.width().max(1.0);
                let galley = painter.layout(self.card.why.clone(), font.clone(), p.text, wide);
                let h = galley.size().y.max(row_h);
                painter.galley(egui::Pos2::new(inside.left(), y), galley, p.text);
                if let Some(what) = &self.card.what {
                    y += h;
                    let galley = painter.layout(what.clone(), font, p.dim, wide);
                    painter.galley(egui::Pos2::new(inside.left(), y), galley, p.dim);
                }
            });
        self.painted = true;
    }

    fn ready(&self) -> bool {
        self.painted
    }
}

#[cfg(test)]
mod tests;
