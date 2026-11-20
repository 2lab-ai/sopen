//! What Enter on a file does before the viewer shows, on a worker: a connection of the viewer's
//! own, the kind (spec §3: extension, then a sniff), the file made local exactly as the
//! standalone routes do it (fetched into the cache, claimed and recorded —
//! [`cache::fetch_claimed_cancellable`]; a local file read in place, an image copied), then
//! whatever is slow: decoding the image, probing the media, indexing the text.

use super::Connect;
use crate::cache::{self, Claim, Fetched};
use crate::gui::hex::{self, HexFile};
use crate::gui::image::{Doc, ImageFile};
use crate::gui::media::Media;
use crate::gui::text::{self, TextDoc};
use crate::gui::{Origin, Reconnect};
use crate::kind::{self, Kind};
use crate::remote::{RemoteFs, Stat};
use anyhow::{bail, Context as _, Result};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

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
    let mut fetch = |fs: &mut Box<dyn RemoteFs + Send>| -> Result<Fetched> {
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
