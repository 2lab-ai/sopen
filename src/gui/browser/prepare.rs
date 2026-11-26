//! What Enter on a file does before the viewer shows, on a worker: a connection of the viewer's
//! own and the kind (spec §3: extension, then a sniff). A remote file then opens as a stream on
//! that connection, whatever its kind and size (06 §7, §13): nothing is fetched, and the viewer
//! reads what it shows — the hex window and text their screens, an image its header first and
//! then the whole file into memory (its decode needs every byte), a video or a sound what
//! ffmpeg asks of the byte server. A local file is read in place (an image copied into the
//! cache, as the standalone route makes it: Save needs the bytes as opened), then whatever is
//! slow: decoding the image, probing the media, indexing the text.
//!
//! And its preview form ([`preview`], 02 §4.2–§4.4): the same viewers, prepared on the window's
//! one preview connection, within the budgets of 02 §4.3 (an info card when one is hit), reading
//! without claiming the cache — a remote file through a stream on that connection, a local one
//! in place — and told they are previews; the hand-over ([`Preview::hand_over`]) makes the viewer
//! a viewer again. Nothing a preview reads is written to disk.
//!
//! A remote file's views share what its opening kept ([`Shared`], 08 §6): a view made later
//! reads through the opening's stream — no connection, no stat, no sniff of its own — and every
//! `Origin` of them connects through one save session that a hidden view gives back (D13, D14).

mod shared;

pub use shared::{Asked, GiveBack, SaveSlot, Shared, Stream};

use super::Connect;
use crate::cache::{self, Claim};
use crate::ffmpeg::Input;
use crate::gui::hex::{self, HexFile};
use crate::gui::image::{self, Doc, ImageFile};
use crate::gui::media::{self, Media};
use crate::gui::streamed::{self, Lease};
use crate::gui::text::{self, TextDoc};
use crate::gui::web::page::{self, PreviewSession, Served};
use crate::gui::{chrome, Embedded, Origin, Outcome, Placeholder, Reconnect, Window};
use crate::kind::{self, Kind};
use crate::remote::{RemoteFs, Stat};
use crate::stream::{Opening, ReadError, ReaderKind, Source, Waker};
use crate::views::PageClass;
use anyhow::{anyhow, bail, Context as _, Result};
use eframe::egui;
use sha2::Digest as _;
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
    /// A local image whose decode must be asked about first (06 §7.3: the memory line applies
    /// to local images too): its window asks, decodes on a worker of its own, and gives way to
    /// the hex window on the file at `path`, read in place (Hex view, a file that does not
    /// decode).
    ImageAsking {
        file: ImageFile,
        path: PathBuf,
        notices: Vec<String>,
        claim: Option<Claim>,
    },
    /// A remote image read through `source` (06 §7.3): its window is made on the frame — it
    /// reads on a worker of its own — and gives way to the hex window on the same stream
    /// (Hex view, a file that does not decode); `repaint` is what the stream wakes.
    ImageStream {
        origin: Origin,
        source: Source,
        repaint: hex::Repaint,
        notices: Vec<String>,
    },
    Media {
        origin: Origin,
        media: Box<Media>,
        notices: Vec<String>,
        claim: Option<Claim>,
    },
    /// A remote video or sound read through a stream (06 §7.4), probed here: every run on it
    /// reads a URL of its byte server.
    MediaStream {
        origin: Origin,
        media: Box<Media>,
        notices: Vec<String>,
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
    /// A page file as its page (09 §6): its server running from its root, on this machine in
    /// place, a remote one over the connection that opened the file; its view is made on the
    /// frame.
    Page {
        page: Served,
    },
}

/// How a page file opens (09 §4–§6), as the window that asks decided it on its frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pages {
    /// As its page, when the page is its default view (`kind::page_default`): its root found
    /// from `opened_on`, the folder the browser was opened on, when that is on the file's host
    /// (§3.1).
    Page { opened_on: Option<String> },
    /// As its source, `line` its first row where the page would have been its view 1 (§4:
    /// `no web engine here (…): showing the source`); `None`: no line.
    Source(Option<String>),
}

impl Pages {
    /// How a file named `name` opens in `ctx`'s window, asked on its frame (the engine is the
    /// windows' thread's): a page file as its page where pages show here (09 §4), else as its
    /// source with the line saying why — `failed`, a view the engine refused before, says it
    /// for the rest of the window. Any other name opens as before 09, no engine asked. In a
    /// terminal, which draws no page (10 T21), a page file opens as its source with no line and
    /// nothing of the page is made; its page is still one of its views (the class the
    /// preparation finds), made when it is chosen.
    pub fn here(
        ctx: &egui::Context,
        name: &str,
        opened_on: Option<String>,
        failed: Option<&crate::gui::web::Unavailable>,
    ) -> Pages {
        if !kind::page_file(name) || crate::gui::in_terminal(ctx) {
            return Pages::Source(None);
        }
        let here = match failed {
            Some(why) => Err(why.clone()),
            None => page::pages_here(),
        };
        match here {
            Ok(()) => Pages::Page { opened_on },
            Err(why) => Pages::Source(Some(page::source_line(&why))),
        }
    }

    /// Whether page files show their page.
    pub fn shown(&self) -> bool {
        matches!(self, Pages::Page { .. })
    }
}

/// What a preparation makes: the opening's first view, a view of a kind (08 §5), or the page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Want {
    Opening,
    Kind(Kind),
    Page,
}

/// Where the page goes among the views of the file at `abs` — its resolved path — (09 §5–§6, the
/// one page decision): by its name, and for an `.xml` by its first 8 KiB, read on `fs` (a local
/// file in place) — at most that much more than the opening read before 09.
fn page_class_on(fs: &mut dyn RemoteFs, abs: &str) -> Result<PageClass> {
    let name = crate::remote::split_parent(abs).1;
    if !kind::page_file(name) {
        return Ok(PageClass::Plain);
    }
    let head = if kind::extension(name).as_deref() == Some("xml") {
        fs.read_head(abs, kind::SNIFF_BYTES)?
    } else {
        Vec::new()
    };
    Ok(PageClass::of(name, kind::page_default(name, &head)))
}

/// The page of the file at `req.abs` (09 §3.1): its root resolved on `fs`, its server started —
/// in place on this machine, over `fs` for a remote file (the viewer's one channel).
fn page_prepared(
    req: &Request,
    mut fs: Box<dyn RemoteFs + Send>,
    opened_on: Option<&str>,
) -> Result<Prepared> {
    let root = crate::stream::page::root(fs.as_mut(), opened_on, req.abs)?;
    let page = match req.label {
        None => Served::local(root, false),
        Some(label) => Served::remote(fs, root, label, req.connect.clone(), None),
    };
    Ok(Prepared::Page { page })
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
    /// it (06 §2): nothing is fetched, and `fs` is the stream's from here on — kept beside the
    /// views ([`Stream`]). Its `Origin` connects on its first use, through the views' `save`
    /// session (D14), and until then takes the server as able to replace a file. A failure is
    /// the error: never a quiet whole fetch instead.
    fn stream(
        &self,
        label: &str,
        fs: Box<dyn RemoteFs + Send>,
        sniff: u64,
        wake: Waker,
        save: &SaveSlot,
    ) -> Result<(Source, Origin, Stream)> {
        let opening = Opening {
            label: self.show(),
            began: self.began,
            sniff,
        };
        let dial = Some(self.connect.clone());
        let opened =
            streamed::open(fs, self.abs, opening, dial, wake, None).map_err(|not| not.error)?;
        let stream = Stream {
            source: opened.source.clone(),
            abs: self.abs.to_string(),
            at: opened.at,
        };
        let origin = self.saving_origin(label, save, opened.at);
        Ok((opened.source, origin, stream))
    }

    /// A remote view's `Origin`, at `at`: it connects on its first use through `save`, the
    /// session its views share and a hidden one gives back (D14).
    fn saving_origin(&self, label: &str, save: &SaveSlot, at: Stat) -> Origin {
        let (fs, reconnect) = save.origin_fs(self.connect.clone());
        Origin::new(
            fs,
            Some(reconnect),
            Some(label.to_string()),
            self.abs.to_string(),
            at,
        )
    }
}

/// Makes the file at `req.abs` ready for its viewer. `progress` gets a local image's copy's
/// `(done, total)`; `cancel` stops it ([`crate::remote::Cancelled`]). The path is resolved
/// first, as the CLI resolves its argument: a link is opened (and saved) as the file it points
/// at (spec §5). A remote file opens through a stream ([`streamed_prepared`]); a local one is
/// read in place.
#[cfg_attr(not(test), allow(dead_code))] // The browser asks [`prepare_opening`].
pub fn prepare(
    req: &Request,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<Prepared> {
    prepare_opening(req, false, cancel, progress).map(|o| o.prepared)
}

/// A file made ready for the viewer with what its view set keeps of the opening (08 §6): the
/// kind, refined (a `.ts` that is not media is Other; media with no picture is audio), the stat
/// at open, and — a local image that does not decode, shown as bytes — why view 1 cannot show
/// it (D10).
pub struct Opened {
    pub prepared: Prepared,
    pub kind: Kind,
    pub stat: Stat,
    pub undecodable: Option<String>,
    /// Where the page goes among its views (09 §5–§6), decided from the file opened — its
    /// resolved name and head, never the name the list shows (the opening's only).
    pub page: PageClass,
    /// What a remote file's views share from now on (08 §6, D13, D14): the stream it was read
    /// through — the opening's, or the one a view made later opened where there was none — and
    /// the session its `Origin` saves on. Nothing for a local file.
    pub shared: Shared,
}

/// [`prepare`] with what the view set keeps ([`Opened`]). `in_place`: a file at a revision or a
/// diff (03 §6.2–§6.3, S2-F), opened read-only from the cache's `.revisions` — read where it is:
/// an image is not copied into the cache's mirror (it is never saved, and nothing of it may
/// outlive its window). A page file opens as its source, with no line.
#[cfg_attr(not(test), allow(dead_code))] // The browser asks [`prepare_paged`].
pub fn prepare_opening(
    req: &Request,
    in_place: bool,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<Opened> {
    prepare_paged(req, in_place, &Pages::Source(None), cancel, progress)
}

/// [`prepare_opening`], a page file opening as `pages` says (09 §5–§6): as its page when that
/// is its default view and the window shows pages, else as its source — its first row the
/// line `pages` gives where the page would have been view 1.
pub fn prepare_paged(
    req: &Request,
    in_place: bool,
    pages: &Pages,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<Opened> {
    let none = Shared::default();
    prepare_with(req, in_place, Want::Opening, pages, &none, cancel, progress)
}

/// What a view made later is made over (08 §6): what its views share, and whether the stream
/// must be read again first.
pub struct Over<'a> {
    pub shared: &'a Shared,
    /// The stream reads a version older than the file's — a Save landed that did not reload it
    /// (the image's, the media window's: 06 §5) — so it is reloaded before the view reads it
    /// (D25: the view shows the file as it is now).
    pub reload: bool,
}

/// One view of a file already opened (08 §5: made the first time it is chosen), as `kind` would
/// open it: `Other` its Hex view, `Text` its text (Text and Raw; view 3 of a binary), `Image`,
/// `Video` or `Audio` its view 1 made again. A local file is read in place as the opening read it
/// (a binary's text by the local whole-file pass, D23). A remote one reads through the stream its
/// views share (D13) — no connection of its own — and where they have none yet (a page file's
/// first view after its Page, a handed-over image's first) opens one on a new connection, kept
/// from then on ([`Opened::shared`]). Its `Origin` saves through their session (D14).
pub fn prepare_view(
    req: &Request,
    in_place: bool,
    kind: Kind,
    over: Over,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<Opened> {
    if let (Some(label), Some(stream)) = (req.label, &over.shared.stream) {
        let save = over.shared.save.clone();
        let save = save.unwrap_or_else(|| SaveSlot::new(Placeholder::YES));
        return kept_prepared(req, label, kind, stream, save, over.reload, cancel);
    }
    let source = Pages::Source(None);
    let want = Want::Kind(kind);
    prepare_with(req, in_place, want, &source, over.shared, cancel, progress)
}

/// A view of a remote file made on the stream its views share (08 §6, D13): no connection, no
/// stat and no sniff — the opening's are known — and nothing it shows is read again.
/// `reload`: the stream is re-opened first ([`Over::reload`]). Its `Origin` saves through
/// `save`, at the version the stream reads.
fn kept_prepared(
    req: &Request,
    label: &str,
    kind: Kind,
    stream: &Stream,
    save: SaveSlot,
    reload: bool,
    cancel: &AtomicBool,
) -> Result<Opened> {
    let stopped = || -> Result<()> {
        if cancel.load(Ordering::Relaxed) {
            bail!(crate::remote::Cancelled);
        }
        Ok(())
    };
    let req = &Request {
        abs: &stream.abs,
        ..*req
    };
    if reload {
        stream.source.reload()?;
        stopped()?;
    }
    let at = stream.at_now();
    let source = stream.source.clone();
    let origin = req.saving_origin(label, &save, at);
    let prepared = match kind {
        Kind::Text => {
            text::warm();
            Prepared::Text {
                doc: TextDoc::streamed(source),
                notices: Vec::new(),
                claim: None,
            }
        }
        // 06 §7.3: the window reads it whole through the stream — from its cache, what the
        // opening's image read — and gives way to the hex window on the same stream.
        Kind::Image => Prepared::ImageStream {
            origin,
            source,
            repaint: hex::Repaint::default(),
            notices: Vec::new(),
        },
        Kind::Video | Kind::Audio => {
            let (_, name) = crate::remote::split_parent(req.abs);
            let media = Media::stream_until(source, name, &req.show(), cancel);
            // A probe Esc stopped fails as one: that is the cancel, not the file's fault.
            stopped()?;
            Prepared::MediaStream {
                origin,
                media: Box::new(media?),
                notices: Vec::new(),
            }
        }
        Kind::Other | Kind::Directory => Prepared::Hex {
            file: HexFile::streamed(origin, source, hex::Repaint::default(), req.cache),
            notices: Vec::new(),
        },
    };
    Ok(Opened {
        prepared,
        kind,
        stat: at,
        undecodable: None,
        page: PageClass::Plain,
        shared: Shared {
            stream: Some(stream.clone()),
            save: Some(save),
        },
    })
}

/// The page view of a file already opened (09 §6: plain XML's view 3, or a page's view 1 made
/// again), its root found from `opened_on` (§3.1), on a connection of its own.
pub fn prepare_page_view(
    req: &Request,
    in_place: bool,
    opened_on: Option<&str>,
    cancel: &AtomicBool,
) -> Result<Opened> {
    let pages = Pages::Page {
        opened_on: opened_on.map(str::to_string),
    };
    let none = Shared::default();
    prepare_with(
        req,
        in_place,
        Want::Page,
        &pages,
        &none,
        cancel,
        &mut |_, _| {},
    )
}

/// `shared`: what the views share already — the save session a remote view's `Origin` uses
/// (a new one for an opening).
fn prepare_with(
    req: &Request,
    in_place: bool,
    want: Want,
    pages: &Pages,
    shared: &Shared,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<Opened> {
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
    // 09 §5: the page decision, before the view is made, from the file opened (`req.abs` is
    // resolved) — an `.xml`'s head read on this connection. A page file stays Text for the
    // views and everything else (§5).
    let class = match (want, kind) {
        (Want::Opening, Kind::Text) => page_class_on(fs.as_mut(), req.abs)?,
        _ => PageClass::Plain,
    };
    let first = class == PageClass::PageFirst;
    let (page, source_line) = match (want, pages) {
        (Want::Page, _) if !kind::page_file(crate::remote::split_parent(req.abs).1) => {
            bail!("{} is not a page file", req.show())
        }
        (Want::Page, Pages::Page { opened_on }) => (Some(opened_on), None),
        (Want::Opening, Pages::Page { opened_on }) if first => (Some(opened_on), None),
        (Want::Opening, Pages::Source(line)) if first => (None, line.clone()),
        _ => (None, None),
    };
    let kind = match want {
        Want::Kind(kind) => kind,
        Want::Opening | Want::Page => kind,
    };
    // 08 D10: empty media has nothing to probe and its byte server no range to serve — it is
    // shown as bytes, why said first, its views still a video's or a sound's.
    let empty =
        want == Want::Opening && matches!(kind, Kind::Video | Kind::Audio) && stat.size == 0;
    // A remote file's `Origin`s save through one session (D14): the views' own, or a new one.
    let save = req.label.map(|_| {
        let save = shared.save.clone();
        save.unwrap_or_else(|| SaveSlot::new(Placeholder::YES))
    });
    let mut stream = None;
    let mut undecodable = None;
    let mut prepared = match (page, req.label, &save) {
        (Some(opened_on), _, _) => page_prepared(req, fs, opened_on.as_deref())?,
        (None, Some(label), Some(save)) => {
            let kind = if empty { Kind::Other } else { kind };
            let (prepared, kept) = streamed_prepared(req, label, fs, kind, sniff, save, cancel)?;
            stream = Some(kept);
            prepared
        }
        (None, _, _) if empty => hex_of(req, fs, stat)?,
        (None, _, _) => local_prepared(
            req,
            in_place,
            fs,
            kind,
            stat,
            &mut undecodable,
            progress,
            cancel,
        )?,
    };
    if let (true, Prepared::Hex { notices, .. }) = (empty, &mut prepared) {
        notices.insert(0, crate::views::Why::Empty.to_string());
    }
    // §4: the source, saying why it is not the page.
    if let (Some(line), Prepared::Text { notices, .. }) = (source_line, &mut prepared) {
        notices.insert(0, line);
    }
    // The kind as the views see it (08 §3): what the preparation found the file to be.
    let kind = match &prepared {
        Prepared::Hex { .. } if undecodable.is_none() && !empty => Kind::Other,
        Prepared::Media { media, .. } | Prepared::MediaStream { media, .. } => {
            if media.video.is_some() {
                Kind::Video
            } else {
                Kind::Audio
            }
        }
        _ => kind,
    };
    Ok(Opened {
        prepared,
        kind,
        stat,
        undecodable,
        page: class,
        shared: Shared { stream, save },
    })
}

/// A local file of `kind` made ready, read in place (an image copied into the cache unless
/// `in_place`). A local image that does not decode is its hex window, the reason in
/// `undecodable`.
#[allow(clippy::too_many_arguments)]
fn local_prepared(
    req: &Request,
    in_place: bool,
    mut fs: Box<dyn RemoteFs + Send>,
    kind: Kind,
    stat: Stat,
    undecodable: &mut Option<String>,
    progress: &mut dyn FnMut(u64, u64),
    cancel: &AtomicBool,
) -> Result<Prepared> {
    let path = PathBuf::from(req.abs);
    match kind {
        Kind::Image => {
            // Always a copy, as the standalone route makes it: Save needs the bytes as opened —
            // but a file at a revision is read where it is (it is never saved).
            let (bytes, at, notices, claim) = if in_place {
                let bytes =
                    std::fs::read(&path).with_context(|| format!("cannot read {}", req.show()))?;
                (bytes, stat, Vec::new(), None)
            } else {
                let (fetched, _ms) = cache::fetch_claimed_cancellable(
                    fs.as_mut(),
                    cache::LOCAL_HOST,
                    req.abs,
                    req.cache,
                    progress,
                    cancel,
                )?;
                let bytes = std::fs::read(&fetched.claim.local)
                    // Read from the copy, said about the file: the copy is never named.
                    .with_context(|| format!("cannot read {}", req.show()))?;
                (bytes, fetched.at, fetched.notices, Some(fetched.claim))
            };
            let doc = match image::decode_unless_asking(&bytes, image::probe::BUDGET, true) {
                Ok(Some(doc)) => doc,
                // 06 §7.3, for a local image too: the memory line first, with Hex view.
                Ok(None) => {
                    let file = ImageFile::new(req.origin(fs, at), bytes, req.cache);
                    return Ok(Prepared::ImageAsking {
                        file,
                        path,
                        notices,
                        claim,
                    });
                }
                // 06 §7.3: a file that does not decode is shown as bytes, why said first.
                Err(e) => {
                    let base = hex::Base::open_as(Path::new(req.abs), &req.show())?;
                    *undecodable = Some(format!("{e:#}"));
                    return Ok(Prepared::Hex {
                        file: HexFile::new(req.origin(fs, stat), base, None, req.cache),
                        notices: vec![format!("{e:#}; the file is shown as bytes")],
                    });
                }
            };
            let file = ImageFile::new(req.origin(fs, at), bytes, req.cache);
            Ok(Prepared::Image {
                file,
                doc,
                notices,
                claim,
            })
        }
        Kind::Video | Kind::Audio => {
            if kind::refine_local(&path, req.abs, kind) == Kind::Other {
                // A `.ts` ffprobe cannot read as media: the hex view.
                return hex_of(req, fs, stat);
            }
            let (_, name) = crate::remote::split_parent(req.abs);
            let media = Media::probe_shown(&Input::path(&path), name, &req.show())?;
            Ok(Prepared::Media {
                origin: req.origin(fs, stat),
                media: Box::new(media),
                notices: Vec::new(),
                claim: None,
            })
        }
        Kind::Text => {
            text::warm();
            let doc = TextDoc::open_cancellable_as(&path, &req.show(), cancel)?;
            Ok(Prepared::Text {
                doc,
                notices: Vec::new(),
                claim: None,
            })
        }
        Kind::Other | Kind::Directory => hex_of(req, fs, stat),
    }
}

/// The waker of a stream a hex window may read (06 §5): the window's own repaint and the hex
/// window's ([`hex::Repaint`], bound to its context when it first draws), so a viewer that gives
/// way to the hex window on the same stream (an image, a `.ts` that is not media) is woken too.
fn hex_waker(wake: &Waker, repaint: &hex::Repaint) -> Waker {
    let (wake, hex) = (wake.clone(), repaint.waker());
    Arc::new(move || {
        wake();
        hex();
    })
}

/// A remote file, of any kind and size, opened as a stream on `fs` — the connection that
/// resolved, stat'ed and sniffed it (06 §13): nothing is fetched. Text is read from its first
/// block on (W5b's index for a file over 16 MiB); an image's window reads its header and then
/// the file, on a worker of its own (06 §7.3); a video or a sound is probed here through its
/// `play` URL (06 §7.4), and a `.ts` ffprobe cannot read as media is the hex window on the
/// same stream. The stream is kept beside the views as well ([`Stream`]); its `Origin` saves
/// through `save` (D14).
fn streamed_prepared(
    req: &Request,
    label: &str,
    fs: Box<dyn RemoteFs + Send>,
    kind: Kind,
    sniff: u64,
    save: &SaveSlot,
    cancel: &AtomicBool,
) -> Result<(Prepared, Stream)> {
    // Esc is looked at between the steps, and while a probe runs (its ffprobe is killed then).
    let stop = || cancel.load(Ordering::Relaxed);
    let stopped = || -> Result<()> {
        if stop() {
            bail!(crate::remote::Cancelled);
        }
        Ok(())
    };
    let repaint = hex::Repaint::default();
    let hex_of_stream = |origin: Origin, source: Source, repaint: hex::Repaint| Prepared::Hex {
        file: HexFile::streamed(origin, source, repaint, req.cache),
        notices: Vec::new(),
    };
    match kind {
        Kind::Text => {
            text::warm();
            let (source, _origin, kept) = req.stream(label, fs, sniff, req.wake.clone(), save)?;
            let prepared = Prepared::Text {
                doc: TextDoc::streamed(source),
                notices: Vec::new(),
                claim: None,
            };
            Ok((prepared, kept))
        }
        Kind::Image => {
            let wake = hex_waker(req.wake, &repaint);
            let (source, origin, kept) = req.stream(label, fs, sniff, wake, save)?;
            let prepared = Prepared::ImageStream {
                origin,
                source,
                repaint,
                notices: Vec::new(),
            };
            Ok((prepared, kept))
        }
        Kind::Video | Kind::Audio => {
            let wake = hex_waker(req.wake, &repaint);
            let (source, origin, kept) = req.stream(label, fs, sniff, wake, save)?;
            let (_, name) = crate::remote::split_parent(req.abs);
            if kind::maybe_video(name) {
                stopped()?;
                // The `.ts` refine over the URL (06 §7.4): ffprobe reads the `play` URL, so
                // what it reads is what the window plays next — from the stream's cache.
                let input = media::served_input(source.clone(), name, &req.show(), cancel);
                // An Esc during its setup (a stream that stalls): the cancel, not the file.
                stopped()?;
                let input = input?;
                let refined = kind::refine_input_until(&input, name, kind, &stop);
                // Stopped, the probe knows nothing of the file: its answer is not used.
                stopped()?;
                if refined == Kind::Other {
                    drop(input);
                    return Ok((hex_of_stream(origin, source, repaint), kept));
                }
            }
            stopped()?;
            let media = Media::stream_until(source, name, &req.show(), cancel);
            // A probe Esc stopped fails as one: that is the cancel, not the file's fault.
            stopped()?;
            let media = media?;
            let prepared = Prepared::MediaStream {
                origin,
                media: Box::new(media),
                notices: Vec::new(),
            };
            Ok((prepared, kept))
        }
        // 06 §7.1: no fetch, whatever the size.
        Kind::Other | Kind::Directory => {
            let (source, origin, kept) = req.stream(label, fs, sniff, repaint.waker(), save)?;
            Ok((hex_of_stream(origin, source, repaint), kept))
        }
    }
}

/// The hex view of a local file, read in place (as the binary route's hex window opens it).
fn hex_of(req: &Request, fs: Box<dyn RemoteFs + Send>, stat: Stat) -> Result<Prepared> {
    let base = hex::Base::open_as(Path::new(req.abs), &req.show())?;
    Ok(Prepared::Hex {
        file: HexFile::new(req.origin(fs, stat), base, None, req.cache),
        notices: Vec::new(),
    })
}

/// Whether a remote preview of this kind reads the file whole (02 §4.3): an image, whose decode
/// needs every byte — so the read budget gates it, decided from the stat. Every other kind reads
/// the ranges it shows.
fn read_whole(kind: Kind) -> bool {
    kind == Kind::Image
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
/// knows it (`host:/abs`; a remote one is read through a URL of its byte server, which is never
/// named).
fn what(e: &anyhow::Error, host: &str, abs: &str) -> Option<String> {
    unreadable(e)?;
    let file = if host == cache::LOCAL_HOST {
        abs.to_string()
    } else {
        format!("{host}:{abs}")
    };
    Some(format!("ffprobe cannot read {file}"))
}

/// The file a preview shows.
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
    /// A page file's page (09 §6), served from its root within the preview's budget.
    Page(Served),
    Card(Card),
}

/// A preview made ready on a worker (it crosses to the window: [`Previewed::show`] makes its
/// viewer there).
pub struct Previewed {
    parts: Parts,
    /// A streamed preview's hold on the window's preview connection.
    lease: Option<Lease>,
    cache: PathBuf,
    /// 09: see [`HandedOver::page`].
    page: PageSeen,
    /// See [`HandedOver::shared`].
    shared: Shared,
    /// The answer of [`Pages::here`] it was prepared under: a page file's preview is handed
    /// over only in a backend whose answer is the same (10 T21).
    made_under: Pages,
}

/// What a preview found of a page file (09 §5–§6), for the views of the viewer it becomes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageSeen {
    /// It was prepared with pages shown (`Pages::Page`).
    pub shown: bool,
    /// Where the page goes among its views, from the file previewed (its resolved name, its
    /// head).
    pub class: PageClass,
}

impl Default for PageSeen {
    fn default() -> Self {
        PageSeen {
            shown: false,
            class: PageClass::Plain,
        }
    }
}

impl Previewed {
    /// The answer of [`Pages::here`] it was prepared under.
    pub fn made_under(&self) -> &Pages {
        &self.made_under
    }

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
        // What the viewer it is handed over to has for views (08 §3).
        let kind = match &self.parts {
            Parts::Text { .. } | Parts::TextStream { .. } | Parts::Page(_) => Kind::Text,
            Parts::Image { .. } => Kind::Image,
            Parts::Media { made, .. } if made.media.video.is_some() => Kind::Video,
            Parts::Media { .. } => Kind::Audio,
            Parts::Hex { .. } | Parts::Card(_) => Kind::Other,
        };
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
            Parts::Page(served) => (page::embed(served), None),
            Parts::Card(card) => (card_window(card.clone()), Some(card)),
        };
        let mut embedded = embedded;
        embedded.window.set_preview(true);
        Preview {
            embedded,
            card,
            shown: true,
            lease: self.lease,
            kind,
            page: self.page,
            shared: self.shared,
            made_under: self.made_under,
        }
    }
}

/// A preview on screen: the viewer told it is one, or the info card. Dropped, its stream (if it
/// has one) gives the window's preview connection back.
pub struct Preview {
    pub embedded: Embedded,
    card: Option<Card>,
    /// Whether its window was last told it is on screen ([`Preview::set_shown`]).
    shown: bool,
    /// A streamed preview's hold on the window's preview connection: dropped as a preview,
    /// its stream gives the connection back; handed over, the viewer keeps it.
    lease: Option<Lease>,
    /// The file's kind as its viewer has views for it (08 §3).
    kind: Kind,
    page: PageSeen,
    shared: Shared,
    made_under: Pages,
}

/// A preview handed over (Enter or F3 on it): the viewer, a viewer again.
pub struct HandedOver {
    /// The live window: its budgets lifted, its bars and input back, the work it stopped at its
    /// budget going on from there.
    pub embedded: Embedded,
    /// The cached copy it holds (none: a preview claims nothing, 02 §4.4) — held as Enter's
    /// claim is.
    pub hold: Option<Claim>,
    /// What the hand-over has to say, for the status row.
    pub notices: Vec<String>,
    /// The file's kind, as the preview found it: what views the viewer has (08 §3).
    pub kind: Kind,
    /// What the preview found of a page file: whether its views have the page (09 §6).
    pub page: PageSeen,
    /// What a remote file's views share from now on (08 §6, D13, D14): the stream the preview
    /// read through and still does (none for an image, read whole), and the session its
    /// `Origin` saves on — so a view made after the hand-over reads that stream, and the
    /// handed-over view gives its save session back when it is hidden.
    pub shared: Shared,
}

impl Preview {
    /// The answer of [`Pages::here`] it was prepared under ([`Previewed::made_under`]).
    pub fn made_under(&self) -> &Pages {
        &self.made_under
    }

    /// Tests: a preview around a window of the test's own (the preview host's input isolation,
    /// shown on a window that would react).
    #[cfg(test)]
    pub(crate) fn standing_in(embedded: Embedded) -> Self {
        Self {
            embedded,
            card: None,
            shown: true,
            lease: None,
            kind: Kind::Other,
            page: PageSeen::default(),
            shared: Shared::default(),
            made_under: Pages::Source(None),
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
    /// - A preview still reading through its stream (text, hex rows, a video or a sound, 06
    ///   §13) moves that stream to the viewer, with its cache, its readers and its session: no
    ///   byte is read twice, nothing is published into the cache, and the window's next remote
    ///   preview opens a new preview session.
    /// - An image was read whole through its stream, which then closed and gave the session
    ///   back to the window: the viewer has the file in memory, decoded once, and its `Origin`
    ///   connects on its first use.
    /// - A local file was read in place and stays so.
    ///
    /// An info card is not handed over: it comes back (Enter opens the file as from the list).
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
        Ok(HandedOver {
            embedded: self.embedded,
            hold: None,
            notices: Vec::new(),
            kind: self.kind,
            page: self.page,
            shared: self.shared,
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
    lease: Option<Lease>,
    page: PageSeen,
    /// What the views of the viewer it may become share ([`HandedOver::shared`]).
    shared: Shared,
}

/// Prepares the preview of the file at `req.abs` (02 §4.2–§4.4), on the window's preview
/// connection, within `budget`: stops are checked between every step, at every read and every
/// MiB of the text index (`cancel`, the deadline). Nothing is claimed in the cache and nothing
/// is written to disk: a remote file is read through a stream on the preview connection (06
/// §13), a local one in place; a video's frame and a sound's waveform come from ffprobe and
/// ffmpeg runs the same stops reach ([`media::preview`]). A file that cannot be previewed —
/// unreadable, vanished, a budget hit — is the info card. `Err` only when cancelled
/// ([`crate::remote::Cancelled`]).
#[cfg_attr(not(test), allow(dead_code))] // The preview host asks [`preview_paged`].
pub fn preview(
    req: PreviewRequest,
    budget: &Budget,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<Previewed> {
    preview_paged(req, &Pages::Source(None), None, budget, cancel, progress)
}

/// [`preview`], a page file previewed as `pages` says (09 §6): its page — scripts off, served
/// from its root within the preview's budget (§3.1), a remote one on the window's preview
/// session (`session`, as the preview host keeps it: what `req.conn` and `req.back` are) — where
/// the page is its default view and the window shows pages; else its source.
pub fn preview_paged(
    req: PreviewRequest,
    pages: &Pages,
    session: Option<PreviewSession>,
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
        lease: None,
        page: PageSeen::default(),
        shared: Shared::default(),
    };
    let host = req.label.unwrap_or(cache::LOCAL_HOST).to_string();
    let cache = req.cache.to_path_buf();
    match preview_parts(req, pages, session, budget, cancel, progress, &mut known) {
        Ok(parts) => Ok(Previewed {
            parts,
            lease: known.lease,
            cache,
            page: known.page,
            shared: known.shared,
            made_under: pages.clone(),
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
                lease: None,
                cache,
                page: PageSeen::default(),
                shared: Shared::default(),
                made_under: pages.clone(),
            })
        }
    }
}

fn preview_parts(
    req: PreviewRequest,
    pages: &Pages,
    session: Option<PreviewSession>,
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
    let limits = budget.limits;
    // A remote file is previewed through a stream, which reads the ranges it shows and no more
    // (02 §4.3) — but an image is read whole (its decode needs every byte), so the read budget
    // gates it, decided from the stat and the name: then nothing is read, the sniff's head
    // included, and the card says the kind its name gives it.
    let over_read = label.is_some() && budget.limit(limits.read).is_some_and(|max| stat.size > max);
    let name_says = kind::by_extension(crate::remote::split_parent(&real).1);
    if over_read && name_says.is_some_and(read_whole) {
        known.kind = name_says;
        return Err(over(OVER_LIMIT));
    }
    budget.check(cancel)?;
    let (kind, sniff) = kind::detect_sniffed(fs.as_mut(), &real, &stat)?;
    known.kind = Some(kind);
    // 09 §5–§6: a page file whose default view is its page shows it, where the window shows
    // pages; the decision reads an `.xml`'s head on this connection. Where it shows its source
    // (a terminal, 10 T21) the viewer it becomes still has the page among its views; a window
    // that shows the source (§4, `Pages::Source` with its line) has no page view, so nothing more
    // is read for it.
    let page_view = !matches!(pages, Pages::Source(Some(_)));
    if page_view && kind == Kind::Text && kind::page_file(crate::remote::split_parent(&real).1) {
        budget.check(cancel)?;
        known.page.class = page_class_on(fs.as_mut(), &real)?;
    }
    if let (Kind::Text, Pages::Page { opened_on }) = (kind, pages) {
        known.page.shown = true;
        // A remote page lends the window's preview session; without it, its source.
        let lent = session.filter(|_| label.is_some());
        if known.page.class == PageClass::PageFirst && (label.is_none() || lent.is_some()) {
            budget.check(cancel)?;
            let root = crate::stream::page::root(fs.as_mut(), opened_on.as_deref(), &real)?;
            budget.check(cancel)?;
            let served = match (label, lent) {
                (Some(label), Some(session)) => {
                    // The window's one preview session is the page's while it is a preview,
                    // and comes back when its server ends (06 §13), as a stream's does.
                    let conn = conn.take().expect("opened above");
                    Served::remote(conn, root, label, connect.clone(), Some(session))
                }
                _ => Served::local(root, true),
            };
            budget.read_done();
            return Ok(Parts::Page(served));
        }
    }
    if kind == Kind::Image
        && budget
            .limit(limits.image_bytes)
            .is_some_and(|max| stat.size > max)
    {
        return Err(over(OVER_LIMIT));
    }
    budget.check(cancel)?;
    let Some(label) = label else {
        return local_parts(connect, &real, stat, kind, cache, budget, cancel, known);
    };
    // 06 §13: the preview reads through a stream on the window's one preview session, which
    // becomes that stream's transport. Nothing is written to disk.
    let answers = Placeholder {
        can_replace: fs.can_replace(),
        can_link: fs.can_link(),
    };
    let conn_taken = conn.take().expect("opened above");
    let (tx, rx) = std::sync::mpsc::channel();
    *back.lock().unwrap_or_else(|p| p.into_inner()) = Some(rx);
    let lease = Lease::new(tx);
    let shown = show(Some(label), &real);
    let opening = Opening {
        label: shown.clone(),
        began,
        sniff,
    };
    let repaint = hex::Repaint::default();
    let waker = match kind {
        Kind::Text => wake.clone(),
        _ => hex_waker(wake, &repaint),
    };
    let dial = Some(connect.clone());
    let opened = match streamed::open(conn_taken, &real, opening, dial, waker, Some(lease.clone()))
    {
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
    let source = opened.source;
    // A streamed viewer's `Origin` connects on its first use (06 §2), through the save session
    // the views of the viewer it may become share (D14); until then it answers what it has to:
    // what the preview connection's server can do where that was asked of it.
    let origin = |answers: Placeholder, at: Stat| {
        let save = SaveSlot::new(answers);
        let (fs, reconnect) = save.origin_fs(connect.clone());
        let origin = Origin::new(
            fs,
            Some(reconnect),
            Some(label.to_string()),
            real.clone(),
            at,
        );
        (origin, save)
    };
    // The stream the preview reads, which a viewer it becomes keeps beside its views (D13).
    let kept = |source: &Source| Stream {
        source: source.clone(),
        abs: real.clone(),
        at: opened.at,
    };
    let parts = match kind {
        Kind::Text => {
            budget.read_done();
            text::warm();
            known.shared.stream = Some(kept(&source));
            Parts::TextStream {
                source,
                title: shown,
                name: known.name.clone(),
                index: limits.text,
            }
        }
        Kind::Image => {
            let (bytes, sha256) = read_streamed_image(&source, budget, cancel, progress)?;
            // In memory: the stream is done, and it gives the session back to the window now
            // (the viewer's `Origin` connects on its first use, a Save).
            drop(source);
            known.lease = None;
            *conn = returned(back, cancel)?;
            budget.read_done();
            let check = || budget.check(cancel);
            let at = Stat {
                sha256: Some(sha256),
                ..opened.at
            };
            let (doc, shown) = decode_previewed(&bytes, budget, &check)?;
            // Its stream is closed (read whole): the views share only its save session.
            let (origin, save) = origin(answers, at);
            known.shared.save = Some(save);
            Parts::Image {
                file: ImageFile::new(origin, bytes, cache),
                doc,
                shown,
            }
        }
        // An empty file has nothing to probe, and its byte server no range to serve.
        Kind::Video | Kind::Audio if source.size() == 0 => bail!("the file is empty (0 bytes)"),
        Kind::Video | Kind::Audio => {
            budget.read_done();
            // Each of its steps has a time of its own (02 §4.3), and every ffprobe and ffmpeg it
            // runs is stopped — killed and reaped — the moment the preview is left or the step is
            // over its time. They read the stream through the `play` URL of a byte server of its
            // own (06 §7.4), which the viewer keeps after the hand-over.
            let watch = MediaWatch {
                budget,
                cancel,
                step: Mutex::new(None),
            };
            let tools = budget.tools()?;
            // The file's own name (a link's target's): its extension says how it is rendered.
            let name = crate::remote::split_parent(&real).1;
            let input = media::served_input(source.clone(), name, &shown, cancel)?;
            let found = media::preview::prepare(
                &tools,
                &input,
                name,
                &shown,
                kind,
                &budget.children,
                &watch,
            )?;
            let parts = match found {
                media::preview::Found::Media(made) => {
                    if made.media.video.is_none() {
                        known.kind = Some(Kind::Audio);
                    }
                    // `play` readers read ahead by the file's own rate from here (06 §4), as
                    // `Media::stream` sets it.
                    if let Some(served) = made.media.input.served() {
                        let rate = source.size() as f64 / made.media.duration.max(1e-3);
                        served.server().set_rate(rate as u64);
                    }
                    let (origin, save) = origin(Placeholder::YES, opened.at);
                    known.shared = Shared {
                        stream: Some(kept(&source)),
                        save: Some(save),
                    };
                    Parts::Media { origin, made }
                }
                // A `.ts` ffprobe cannot read as media: the hex view, as Enter shows it, on the
                // same stream.
                media::preview::Found::NotMedia => {
                    drop(input);
                    let (origin, save) = origin(Placeholder::YES, opened.at);
                    known.shared = Shared {
                        stream: Some(kept(&source)),
                        save: Some(save),
                    };
                    Parts::Hex {
                        file: HexFile::streamed(origin, source, repaint, cache),
                    }
                }
            };
            // Once more before handing the preview over: a cancel meanwhile drops it here (its
            // stream closes, and the connection comes back).
            watch.cancelled()?;
            return Ok(parts);
        }
        Kind::Other | Kind::Directory => {
            budget.read_done();
            let (origin, save) = origin(Placeholder::YES, opened.at);
            known.shared = Shared {
                stream: Some(kept(&source)),
                save: Some(save),
            };
            Parts::Hex {
                file: HexFile::streamed(origin, source, repaint, cache),
            }
        }
    };
    // Once more before handing the preview over: a cancel meanwhile drops it here (its stream
    // closes, and the connection comes back).
    budget.check(cancel)?;
    Ok(parts)
}

/// The preview of a local file, read in place: an image whole (within its budget, through a
/// handle of its own), text indexed to the text budget, the hex rows, or a video's frame and a
/// sound's waveform from ffprobe and ffmpeg runs on its path.
#[allow(clippy::too_many_arguments)]
fn local_parts(
    connect: &Connect,
    real: &str,
    stat: Stat,
    kind: Kind,
    cache: &Path,
    budget: &Budget,
    cancel: &AtomicBool,
    known: &mut Known,
) -> Result<Parts> {
    let limits = budget.limits;
    budget.read_done();
    budget.check(cancel)?;
    let path = PathBuf::from(real);
    let origin = |at: Stat| -> Result<Origin> {
        Ok(Origin::new(connect()?, None, None, real.to_string(), at))
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
        let name = crate::remote::split_parent(real).1;
        let found =
            media::preview::prepare(&tools, &input, name, real, kind, &budget.children, &watch)?;
        let parts = match found {
            media::preview::Found::Media(made) => {
                if made.media.video.is_none() {
                    known.kind = Some(Kind::Audio);
                }
                Parts::Media {
                    origin: origin(stat)?,
                    made,
                }
            }
            // A `.ts` ffprobe cannot read as media: the hex view, as Enter shows it.
            media::preview::Found::NotMedia => Parts::Hex {
                file: HexFile::new(origin(stat)?, hex::Base::open_as(&path, real)?, None, cache),
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
            let (doc, shown) = decode_previewed(&bytes, budget, &check)?;
            // A local file read in place: Save checks it against the bytes read, as against a
            // stream's.
            let at = Stat {
                sha256: Some(sha2::Sha256::digest(&bytes).into()),
                ..stat
            };
            let file = ImageFile::new(origin(at)?, bytes, cache);
            Parts::Image { file, doc, shown }
        }
        Kind::Text => {
            text::warm();
            let doc = TextDoc::open_capped_as(&path, real, &|| budget.limit(limits.text), &check)?;
            Parts::Text {
                doc,
                title: real.to_string(),
                name: known.name.clone(),
                live: true,
            }
        }
        Kind::Other | Kind::Directory => {
            let base = hex::Base::open_as(&path, real)?;
            Parts::Hex {
                file: HexFile::new(origin(stat)?, base, None, cache),
            }
        }
        Kind::Video | Kind::Audio => unreachable!("made above"),
    };
    // Once more before handing the preview over: a cancel meanwhile drops it here.
    check()?;
    Ok(parts)
}

/// An image's bytes decoded for its preview, within the pixel budget (its dimensions read from
/// the bytes before the decode), with the picture fitted to the side.
fn decode_previewed(
    bytes: &[u8],
    budget: &Budget,
    check: &dyn Fn() -> Result<()>,
) -> Result<(Doc, Option<::image::RgbaImage>)> {
    let (w, h) = image::dimensions(bytes)?;
    over_pixels(budget, w, h)?;
    let doc = Doc::decode_checked(bytes, check)?;
    let shown = image::fitted(&doc);
    check()?;
    Ok((doc, shown))
}

/// The card when an image of `w`×`h` is over the preview's pixel budget.
fn over_pixels(budget: &Budget, w: u32, h: u32) -> Result<()> {
    if budget
        .limit(budget.limits.pixels)
        .is_some_and(|max| u64::from(w) * u64::from(h) > max)
    {
        return Err(over(format!("image is {w}×{h}; Enter opens it")));
    }
    Ok(())
}

/// How much of a streamed image a read asks for at a time: the stops and the side's progress
/// are this fine.
const IMAGE_STEP: usize = 256 << 10;

/// `buf.len()` bytes at `off` through `reader` (fewer at the end of the file), with the
/// preview's stops looked at first: a cancel, the deadline. A stream that failed or changed is
/// the card, with its words.
fn read_stopping(
    reader: &mut crate::stream::Reader,
    off: u64,
    buf: &mut [u8],
    budget: &Budget,
    cancel: &AtomicBool,
) -> Result<usize> {
    budget.check(cancel)?;
    match reader.read_at(off, buf, cancel) {
        Ok(n) => Ok(n),
        Err(ReadError::Cancelled) => Err(anyhow!(crate::remote::Cancelled)),
        Err(e) => Err(anyhow!("{e}")),
    }
}

/// A remote image read whole through its stream for its preview (02 §4.3, 06 §7.3): its header
/// first, by W6's walk — a picture over the pixel budget is the card having read its header's
/// blocks, not its body — then every byte, hashed on the way (01 §5's baseline, as a fetch gives
/// it), with the stops looked at before every read and the side's progress after it. The
/// handle's size decides the read budget too: a file grown past it since its stat is the card.
/// Lifted (Enter while it is prepared), the preview's budgets go, but not the image window's own
/// question: a file or a decode the window would ask about first (06 §7.3's memory line) is the
/// card all the same, nothing past its header read, and Enter opens it in that window.
fn read_streamed_image(
    source: &Source,
    budget: &Budget,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<(Vec<u8>, [u8; 32])> {
    let size = source.size();
    // The side says the read is under way from its start (the header's blocks are its first).
    progress(0, size);
    let limits = budget.limits;
    let over_size = |max: u64| size > max;
    if budget.limit(limits.read).is_some_and(over_size)
        || budget.limit(limits.image_bytes).is_some_and(over_size)
    {
        return Err(over(OVER_LIMIT));
    }
    let probed = {
        let mut head = image::probe::Head::new(size, image::probe::HEADER_CAP, |off, buf| {
            // A reader of its own for each block, as the image window's header read has: one
            // at no position reads nothing ahead of what it is asked for (06 §4).
            let mut reader = source.reader(ReaderKind::Bulk);
            read_stopping(&mut reader, off, buf, budget, cancel)
                .map_err(|e| std::io::Error::other(format!("{e:#}")))
        });
        image::probe::probe(&mut head)
    };
    // A stop inside the walk is the walk's error: said as the stop it was.
    budget.check(cancel)?;
    let header = match probed {
        Ok(image::probe::Probe::Header(header)) => {
            over_pixels(budget, header.width, header.height)?;
            Some(header)
        }
        Ok(image::probe::Probe::Refused(why)) => bail!(why),
        Ok(image::probe::Probe::Unknown) => None,
        Err(e) => bail!(e),
    };
    let decode = header.as_ref().map(|h| (h.need, h.upright()));
    if image::probe::ask_open(size, decode, image::probe::BUDGET).is_some() {
        return Err(over(OVER_LIMIT));
    }
    let len = usize::try_from(size).context("the file is larger than this machine can address")?;
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(len).map_err(|_| {
        anyhow!(
            "not enough memory: the system refused {} for this file",
            image::probe::memory_text(size)
        )
    })?;
    let mut hasher = sha2::Sha256::new();
    let mut reader = source.reader(ReaderKind::Bulk);
    let mut step = vec![0u8; IMAGE_STEP];
    while (bytes.len() as u64) < size {
        let at = bytes.len() as u64;
        let want = IMAGE_STEP.min((size - at) as usize);
        let n = read_stopping(&mut reader, at, &mut step[..want], budget, cancel)?;
        if n == 0 {
            bail!("the file ended after {at} of its {size} bytes");
        }
        hasher.update(&step[..n]);
        bytes.extend_from_slice(&step[..n]);
        progress(bytes.len() as u64, size);
    }
    Ok((bytes, hasher.finalize().into()))
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
