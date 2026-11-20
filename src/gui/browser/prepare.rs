//! What Enter on a file does before the viewer shows, on a worker: a connection of the viewer's
//! own, the kind (spec §3: extension, then a sniff), the file made local exactly as the
//! standalone routes do it (fetched into the cache, claimed and recorded —
//! [`cache::fetch_claimed_cancellable`]; a local file read in place, an image copied), then
//! whatever is slow: decoding the image, probing the media, indexing the text.
//!
//! And its preview form ([`preview`], 02 §4.2–§4.4): the same viewers, prepared on the window's
//! one preview connection, within the budgets of 02 §4.3 (an info card when one is hit), reading
//! without claiming the cache — a remote file into a private copy under `.preview`, a local one in
//! place — and told they are previews; the hand-over ([`Preview::hand_over`]) makes the viewer a
//! viewer again and publishes the copy into the cache by a hard link.

// The preview form is for the preview host (02 §4, S1-E), which is not wired up yet: until then
// only the tests call it. S1-E removes this allowance.
#![cfg_attr(not(test), allow(dead_code))]

use super::Connect;
use crate::cache::{self, Claim, Fetched, PreviewName, PreviewSpace};
use crate::gui::hex::{self, HexFile};
use crate::gui::image::{self, Doc, ImageFile};
use crate::gui::media::Media;
use crate::gui::text::{self, TextDoc};
use crate::gui::{chrome, Embedded, Origin, Outcome, Reconnect, Window};
use crate::kind::{self, Kind};
use crate::remote::{RemoteFs, Stat};
use anyhow::{anyhow, bail, Context as _, Result};
use eframe::egui;
use sha2::Digest as _;
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
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
        media: Media,
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
    let req = &Request {
        connect: req.connect,
        label: req.label,
        abs: &abs,
        cache: req.cache,
    };
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
    let kind = kind::detect(fs.as_mut(), req.abs, &stat)?;
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
                    })
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
                media,
                notices,
                claim,
            })
        }
        Kind::Text => {
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
        Kind::Other | Kind::Directory => {
            let fetched = if local { None } else { Some(fetch(&mut fs)?) };
            hex_of(req, fs, stat, fetched)
        }
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
    /// Preparation after the read.
    pub time: Duration,
}

impl Limits {
    /// 02 §4.3: 32 MiB read, images 64 MiB and 64 megapixels (256 MiB of RGBA), text indexed to
    /// 4 MiB, 5 s.
    pub const PREVIEW: Limits = Limits {
        read: 32 << 20,
        image_bytes: 64 << 20,
        pixels: 64 << 20,
        text: 4 << 20,
        time: Duration::from_secs(5),
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
    fn with_clock(limits: Limits, clock: Box<dyn Fn() -> Instant + Send + Sync>) -> Arc<Self> {
        Arc::new(Self {
            limits,
            lifted: AtomicBool::new(false),
            read_ended: Mutex::new(None),
            clock,
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

/// The card's last line for why the preparation stopped.
fn why(e: &anyhow::Error) -> String {
    match e.chain().find_map(|c| c.downcast_ref::<OverBudget>()) {
        Some(OverBudget(why)) => why.clone(),
        None => format!("{e:#}"),
    }
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
    Card(Card),
}

/// A preview made ready on a worker (it crosses to the window: [`Previewed::show`] makes its
/// viewer there).
pub struct Previewed {
    parts: Parts,
    copy: Option<Copy>,
    host: String,
    /// The file, resolved (a link's target).
    abs: String,
    cache: PathBuf,
}

impl Previewed {
    /// The info card, when the file is not previewed.
    pub fn card(&self) -> Option<&Card> {
        match &self.parts {
            Parts::Card(card) => Some(card),
            _ => None,
        }
    }

    /// The preview, on the window's thread: its viewer, told it is a preview
    /// ([`Window::set_preview`]), or the info card.
    pub fn show(self) -> Preview {
        let (embedded, card) = match self.parts {
            Parts::Text {
                doc,
                title,
                name,
                live,
            } => (text::embed(doc, title, &name, Vec::new(), live, None), None),
            Parts::Image { file, doc, shown } => (image::embed_preview(file, doc, shown), None),
            Parts::Hex { file } => (hex::embed(file, Vec::new()), None),
            Parts::Card(card) => (card_window(card.clone()), Some(card)),
        };
        let mut embedded = embedded;
        embedded.window.set_preview(true);
        Preview {
            embedded,
            card,
            copy: self.copy,
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
    copy: Option<Copy>,
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
    /// The info card, when the file is not previewed.
    pub fn card(&self) -> Option<&Card> {
        self.card.as_ref()
    }

    /// The hand-over (02 §4.4): the live window becomes the viewer (`set_preview(false)`), and a
    /// remote file's private copy is published into the cache by a hard link — the same inode,
    /// nothing read again — then claimed and recorded as Enter's fetch would be; the copy's own
    /// name stays until the viewer is gone. A local file was read in place and stays so. An info
    /// card is not handed over: it comes back (Enter opens the file: [`Preview::into_reuse`]).
    pub fn hand_over(mut self) -> std::result::Result<HandedOver, Box<Preview>> {
        if self.card.is_some() {
            return Err(Box::new(self));
        }
        self.embedded.window.set_preview(false);
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
}

/// Prepares the preview of the file at `req.abs` (02 §4.2–§4.4), on the window's preview
/// connection, within `budget`: stops are checked between every step, at every read and every
/// MiB of the text index (`cancel`, the deadline). Nothing is claimed in the cache: a remote
/// file is read into a private copy (`<cache>/.preview/<window>/<n>`, 0600), a local one read in
/// place. A file that cannot be previewed — unreadable, vanished, a budget hit, video and audio
/// in S1 — is the info card. `Err` only when cancelled ([`crate::remote::Cancelled`]).
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
    };
    let host = req.label.unwrap_or(cache::LOCAL_HOST).to_string();
    let cache = req.cache.to_path_buf();
    match preview_parts(req, budget, cancel, progress, &mut known) {
        Ok(parts) => Ok(Previewed {
            parts,
            copy: known.copy,
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
            };
            Ok(Previewed {
                parts: Parts::Card(card),
                copy: known.copy,
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
    } = req;
    // Cancel is looked at between every two requests: none starts once it is set.
    budget.check(cancel)?;
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
    // Decided from the stat, before anything is read — the sniff's head included.
    if remote && budget.limit(limits.read).is_some_and(|max| stat.size > max) {
        return Err(over(OVER_LIMIT));
    }
    budget.check(cancel)?;
    let kind = kind::detect(fs.as_mut(), &real, &stat)?;
    known.kind = Some(kind);
    match kind {
        // Their previews come with S1-M (02 §4.2): the info card until then.
        Kind::Video | Kind::Audio => {
            return Err(over(format!(
                "no preview of {} yet: Enter opens it",
                kind.as_str()
            )))
        }
        Kind::Image
            if budget
                .limit(limits.image_bytes)
                .is_some_and(|max| stat.size > max) =>
        {
            return Err(over(OVER_LIMIT));
        }
        _ => {}
    }
    budget.check(cancel)?;
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
        Kind::Video | Kind::Audio => unreachable!("an info card above"),
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

/// The `Origin` of a previewed remote viewer (02 §4.4): a placeholder connection that fails, so
/// the viewer's first remote call — `Origin::fs`'s probe — reconnects through `connect` and the
/// viewer works on a session of its own from then on, never on the window's preview connection.
/// What the server can do is the preview connection's (the same server).
fn placeholder_origin(
    connect: &Connect,
    label: &str,
    fs: &dyn RemoteFs,
    abs: String,
    at: Stat,
) -> Origin {
    let connect = connect.clone();
    Origin::new(
        Box::new(Placeholder {
            can_replace: fs.can_replace(),
            can_link: fs.can_link(),
        }),
        Some(Box::new(move || connect()) as Reconnect),
        Some(label.to_string()),
        abs,
        at,
    )
}

/// A connection that is not there yet: every call fails (see [`placeholder_origin`]).
struct Placeholder {
    can_replace: bool,
    can_link: bool,
}

fn not_connected() -> anyhow::Error {
    anyhow!("not connected yet (a preview)")
}

impl RemoteFs for Placeholder {
    fn stat_opt(&mut self, _: &str) -> Result<Option<Stat>> {
        Err(not_connected())
    }
    fn lstat_opt(&mut self, _: &str) -> Result<Option<Stat>> {
        Err(not_connected())
    }
    fn realpath(&mut self, _: &str) -> Result<String> {
        Err(not_connected())
    }
    fn read_head(&mut self, _: &str, _: usize) -> Result<Vec<u8>> {
        Err(not_connected())
    }
    fn list_dir(&mut self, _: &str) -> Result<Vec<String>> {
        Err(not_connected())
    }
    fn list_entries(&mut self, _: &str) -> Result<Vec<crate::remote::Entry>> {
        Err(not_connected())
    }
    fn read_to(
        &mut self,
        _: &str,
        _: &mut dyn std::io::Write,
        _: &mut dyn FnMut(u64),
    ) -> Result<u64> {
        Err(not_connected())
    }
    fn create_from(
        &mut self,
        _: &Path,
        _: &str,
        _: Option<crate::remote::Like>,
    ) -> std::result::Result<crate::remote::Uploaded, crate::remote::CreateError> {
        Err(crate::remote::CreateError::new(
            crate::remote::Created::No,
            not_connected(),
        ))
    }
    fn set_mode(&mut self, _: &str, _: u32) -> Result<()> {
        Err(not_connected())
    }
    fn rename_over(&mut self, _: &str, _: &str) -> Result<()> {
        Err(not_connected())
    }
    fn link_new(&mut self, _: &str, _: &str) -> Result<()> {
        Err(not_connected())
    }
    fn can_link(&self) -> bool {
        self.can_link
    }
    fn can_replace(&self) -> bool {
        self.can_replace
    }
    fn remove(&mut self, _: &str) -> Result<()> {
        Err(not_connected())
    }
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
                let galley =
                    painter.layout(self.card.why.clone(), font, p.text, inside.width().max(1.0));
                painter.galley(egui::Pos2::new(inside.left(), y), galley, p.text);
            });
        self.painted = true;
    }

    fn ready(&self) -> bool {
        self.painted
    }
}

#[cfg(test)]
mod tests;
