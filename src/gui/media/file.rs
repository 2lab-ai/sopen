//! The media window's writes: Save renders the edit list and writes it back over the original
//! by the same rule as image and hex Save (spec §5: a local backup of the original first, then
//! the conflict-checked overwrite, never silent), and Export publishes a new sibling.

use super::{Media, CANNOT_REPLACE};
use crate::ffmpeg::EditSpec;
use crate::gui::Origin;
use crate::remote::WriteOutcome;
use anyhow::{bail, Context as _, Result};
use std::path::{Path, PathBuf};
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
    rendered: Option<PathBuf>,
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
        // changed since it was opened is refused first (the overwrite checks again after).
        let at = self.origin.at;
        let abs = self.origin.abs.clone();
        let now = self.origin.fs()?.lstat_opt(&abs)?;
        let changed = match now {
            Some(now)
                if now.is_file
                    && !now.is_symlink
                    && now.size == at.size
                    && now.mtime == at.mtime =>
            {
                None
            }
            Some(now) => Some(crate::remote::describe_change(&at.fingerprint(), &now)),
            None => Some("it is gone".to_string()),
        };
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
        let (staged, _) = crate::gui::create_keep(&self.edits, "edited")?;
        let work = work_root.join(crate::remote::random_hex());
        crate::cache::private_dir(&work)?;
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
                        if let Some(old) = self.rendered.replace(staged.clone()) {
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
        if let Some(path) = self.rendered.take() {
            let _ = std::fs::remove_file(path);
        }
    }
}
