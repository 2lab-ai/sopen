//! The media window's writes: Save renders the edit list and writes it back over the original
//! by the same rule as image and hex Save (spec §5: a local backup of the original first, then
//! the conflict-checked overwrite, never silent), and Export publishes a new sibling.

use super::{Media, CANNOT_REPLACE};
use crate::ffmpeg::EditSpec;
use crate::gui::Origin;
use crate::remote::WriteOutcome;
use anyhow::{bail, Context as _, Result};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::time::SystemTime;

/// The opened media file: where it lives and the local places sopen writes to.
pub struct MediaFile {
    pub origin: Origin,
    cache_root: PathBuf,
    /// Staged renders and refused ones: `<cache>/.edits/<host>/<abs>` siblings.
    edits: PathBuf,
    /// Set by the first Save's backup; later saves of the session keep it.
    backup: Option<PathBuf>,
    /// What the last Save of a remote file wrote, kept here: the window plays it (the cached
    /// copy still holds what was fetched). Removed when the window goes or saves again.
    rendered: Option<(PathBuf, crate::cache::JobLock)>,
    /// A file opened in place has no hash from a fetch: its content as opened is hashed on a
    /// worker from the start, the baseline the Save checks compare with (§5).
    baseline: Option<Arc<Baseline>>,
}

/// The hash of a local file as it was opened, made on a worker (`None` in it: over the hash
/// limit — size and modification time only, as §5 does above it).
#[derive(Default)]
struct Baseline {
    result: Mutex<Option<Result<Option<[u8; 32]>, String>>>,
    ready: Condvar,
}

/// Hashes `path` if it is still the file `at` describes — size and modification time read
/// through the handle before and after — up to the hash limit.
fn hash_as_opened(path: &Path, at: &crate::remote::Stat) -> Result<Option<[u8; 32]>, String> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let same = |m: &std::fs::Metadata| m.len() == at.size && m.modified().ok() == Some(at.mtime);
    let mut file = std::fs::File::open(path).map_err(|e| format!("cannot read it ({e})"))?;
    let before = file
        .metadata()
        .map_err(|e| format!("cannot read it ({e})"))?;
    if !same(&before) {
        return Err("changed since it was opened (before its content was read)".to_string());
    }
    if before.len() > crate::remote::HASH_CHECK_MAX {
        return Ok(None);
    }
    let mut hash = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        match file.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => hash.update(&buf[..n]),
            Err(e) => return Err(format!("cannot read it ({e})")),
        }
    }
    let after = file
        .metadata()
        .map_err(|e| format!("cannot read it ({e})"))?;
    if !same(&after) {
        return Err("changed since it was opened (while its content was read)".to_string());
    }
    Ok(Some(hash.finalize().into()))
}

/// Result of one Save.
pub enum Saved {
    /// `notes`: what the owner must still hear (the remote changed right after); `not_kept`:
    /// what the saved file does not carry; `media`: the saved file as probed (the window plays
    /// it from now on), or why it could not be read back.
    Written {
        remote: String,
        backup: PathBuf,
        notes: Vec<String>,
        not_kept: Vec<String>,
        media: Box<Result<Media, String>>,
    },
    /// The file changed since it was opened; nothing was written. `message` names where the
    /// render was kept.
    Refused { message: String },
}

impl MediaFile {
    pub fn new(origin: Origin, cache_root: &Path) -> Self {
        let edits = crate::cache::edits_path(cache_root, origin.host(), &origin.abs);
        Self {
            origin,
            cache_root: cache_root.to_path_buf(),
            edits,
            backup: None,
            rendered: None,
            baseline: None,
        }
    }

    /// For a file opened in place (no hash from a fetch): its content as opened is hashed on a
    /// worker from now on — what Save compares the file with, before rendering and before the
    /// overwrite.
    pub fn hash_in_background(&mut self) {
        if !self.origin.is_local() || self.origin.at.sha256.is_some() || self.baseline.is_some() {
            return;
        }
        let baseline = Arc::new(Baseline::default());
        let (slot, path, at) = (
            baseline.clone(),
            PathBuf::from(&self.origin.abs),
            self.origin.at,
        );
        std::thread::spawn(move || {
            let result = hash_as_opened(&path, &at);
            *slot.result.lock().unwrap() = Some(result);
            slot.ready.notify_all();
        });
        self.baseline = Some(baseline);
    }

    /// The hash of the file as opened is made (tests wait for it before changing the file).
    #[cfg(test)]
    pub fn baseline_ready(&self) -> bool {
        self.baseline
            .as_ref()
            .is_none_or(|b| b.result.lock().unwrap().is_some())
    }

    /// As if hashing the file as opened had failed for `why` (tests: a failure that leaves the
    /// file's size and time as they were, such as a read error).
    #[cfg(test)]
    pub fn fail_baseline(&mut self, why: &str) {
        let baseline = Baseline::default();
        *baseline.result.lock().unwrap() = Some(Err(why.to_string()));
        self.baseline = Some(Arc::new(baseline));
    }

    /// The staged render a saved remote file plays from, if any (tests).
    #[cfg(test)]
    pub fn rendered_path(&self) -> Option<&Path> {
        self.rendered.as_ref().map(|(p, _)| p.as_path())
    }

    /// Waits for the hash of the file as opened and makes it the baseline; why not, when the
    /// file is no longer the one opened (or cannot be read). A failure stays: every later Save
    /// is refused the same way until the file is opened again — none falls back to comparing
    /// size and time alone.
    fn take_baseline(&mut self) -> Option<String> {
        let baseline = self.baseline.clone()?;
        let mut slot = baseline.result.lock().unwrap();
        while slot.is_none() {
            slot = baseline.ready.wait(slot).unwrap();
        }
        match slot.as_ref().expect("checked") {
            Ok(hash) => {
                self.origin.at.sha256 = *hash;
                drop(slot);
                self.baseline = None;
                None
            }
            Err(why) => Some(why.clone()),
        }
    }

    /// The local backup of the original — the file the window plays (`local`) as opened —
    /// written once per session before the first overwrite: copied whole (a media file is not
    /// held in memory), into a new owner-only file.
    fn ensure_backup(&mut self, local: &Path) -> Result<PathBuf> {
        if let Some(path) = &self.backup {
            return Ok(path.clone());
        }
        let first = crate::cache::backup_path(
            &self.cache_root,
            self.origin.host(),
            &self.origin.abs,
            SystemTime::now(),
        );
        let (path, mut file) = crate::cache::create_unique(|attempt| {
            if attempt == 0 {
                return first.clone();
            }
            let mut name = first.file_name().unwrap_or_default().to_os_string();
            name.push(format!("-{}", &crate::remote::random_hex()[..8]));
            first.with_file_name(name)
        })?;
        let copied = std::fs::File::open(local)
            .and_then(|mut src| std::io::copy(&mut src, &mut file))
            .and_then(|_| file.sync_all());
        if let Err(e) = copied {
            let _ = std::fs::remove_file(&path);
            return Err(e).with_context(|| format!("cannot write the backup {}", path.display()));
        }
        println!("backup of {} at {}", self.origin.display(), path.display());
        self.backup = Some(path.clone());
        Ok(path)
    }

    /// Save (spec §5, §7): can this server replace a file in one step (refused before anything
    /// is written if not) → can the file keep its format → the local backup of the original
    /// (printed) → render the edit with ffmpeg into a staged file → the conflict-checked
    /// overwrite. A local file then plays from its path again; a remote one from the staged
    /// render, kept for the window. A refusal or a failure keeps the render and names it.
    pub fn save(
        &mut self,
        media: &Media,
        spec: &EditSpec,
        work_root: &Path,
        progress: &mut dyn FnMut(f64),
    ) -> Result<Saved> {
        if !self.origin.can_replace() {
            bail!(
                "{} was not touched: {CANNOT_REPLACE}",
                self.origin.display()
            );
        }
        if let Some(why) = media.save_block() {
            bail!("{} was not touched: {why}", self.origin.display());
        }
        let Some((kind, _, plan)) = media.plan.clone() else {
            bail!(
                "{} was not touched: not a file sopen can render",
                self.origin.display()
            );
        };
        // A render can take minutes, and a file opened in place is what it reads: a file that
        // changed since it was opened is refused first, by the overwrite's own rule — content
        // too, against the bytes as opened (§5) — and the overwrite checks again after.
        if let Some(why) = self.take_baseline() {
            return Ok(Saved::Refused {
                message: format!(
                    "{} {why}; not saved — nothing was rendered, and your edits are still in \
                     the window",
                    self.origin.display()
                ),
            });
        }
        let at = self.origin.at;
        let abs = self.origin.abs.clone();
        let changed = crate::remote::changed_since(self.origin.fs()?, &abs, &at.fingerprint())?;
        if let Some(reason) = changed {
            return Ok(Saved::Refused {
                message: format!(
                    "{} changed since it was opened ({reason}); not saved — nothing was \
                     rendered, and your edits are still in the window",
                    self.origin.display()
                ),
            });
        }
        // The backup is the original's bytes: today the local file or cached copy every run
        // reads; a streamed input (a URL) needs its bytes from the remote instead.
        let Some(local) = media.input.local() else {
            bail!(
                "{} was not touched: there is no local copy to back up first",
                self.origin.display()
            );
        };
        let backup = self.ensure_backup(local)?;
        let work = work_root.join(crate::remote::random_hex());
        // Both held while the render runs and is written back: a sopen killed meanwhile leaves
        // them to the next start's sweep ([`crate::cache::sweep_stale_jobs`]), never forever.
        let _work_job = crate::cache::JobLock::take(&work)?;
        crate::cache::private_dir(&work)?;
        let staged = match crate::gui::create_keep(&self.edits, "render") {
            Ok((staged, _)) => staged,
            Err(e) => {
                let _ = std::fs::remove_dir_all(&work);
                return Err(e);
            }
        };
        let mut render_job = match crate::cache::JobLock::take(&staged) {
            Ok(job) => Some(job),
            Err(e) => {
                let _ = std::fs::remove_dir_all(&work);
                let _ = std::fs::remove_file(&staged);
                return Err(e);
            }
        };
        let rendered = crate::cut::render_edit(
            &media.tools,
            &media.input,
            &staged,
            spec,
            kind,
            &plan,
            &work,
            progress,
        );
        let _ = std::fs::remove_dir_all(&work);
        if let Err(e) = rendered {
            let _ = std::fs::remove_file(&staged);
            return Err(e.context(format!(
                "{} was not touched (the original is backed up at {})",
                self.origin.display(),
                backup.display()
            )));
        }
        let not_kept = crate::cut::edit_notes(&plan, &media.probe, spec);
        let result = (|| -> Result<Saved> {
            let at = self.origin.at;
            let abs = self.origin.abs.clone();
            match self
                .origin
                .fs()?
                .write_atomic(&staged, &abs, at.fingerprint())?
            {
                WriteOutcome::Written { now, notes } => {
                    self.origin.at = now;
                    let playing = if self.origin.is_local() {
                        let _ = std::fs::remove_file(&staged);
                        PathBuf::from(&abs)
                    } else {
                        // Kept as the playing copy, with its lock: a sopen killed later still
                        // leaves it to the next start's sweep.
                        let job = render_job.take().expect("held until here");
                        if let Some((old, _)) = self.rendered.replace((staged.clone(), job)) {
                            let _ = std::fs::remove_file(old);
                        }
                        staged.clone()
                    };
                    let (_, name) = self.origin.parent();
                    let input = crate::ffmpeg::Input::path(&playing);
                    let media =
                        Box::new(Media::probe_as(&input, &name).map_err(|e| format!("{e:#}")));
                    Ok(Saved::Written {
                        remote: self.origin.display(),
                        backup: backup.clone(),
                        notes,
                        not_kept: not_kept.clone(),
                        media,
                    })
                }
                WriteOutcome::Conflict { reason, notes, .. } => {
                    // A name of its own first, so the move cannot replace anyone's file.
                    let (kept, _) = crate::gui::create_keep(&self.edits, "conflict")?;
                    if let Err(e) = std::fs::rename(&staged, &kept) {
                        let _ = std::fs::remove_file(&kept);
                        return Err(e).with_context(|| {
                            format!("cannot keep the rendered edit at {}", kept.display())
                        });
                    }
                    let mut message = format!(
                        "{} changed since it was opened ({reason}); not saved — your edit is at \
                         {}",
                        self.origin.display(),
                        kept.display()
                    );
                    for note in notes {
                        message.push_str(&format!("; {note}"));
                    }
                    Ok(Saved::Refused { message })
                }
            }
        })();
        result.map_err(|e| crate::gui::image::failed(&e, &staged))
    }
}

impl Drop for MediaFile {
    fn drop(&mut self) {
        // The file first, then its lock (dropped with it).
        if let Some((path, _job)) = self.rendered.take() {
            let _ = std::fs::remove_file(path);
        }
    }
}
