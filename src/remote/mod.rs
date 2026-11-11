//! The file layer. [`RemoteFs`] has two implementations with the same semantics: [`SftpRemote`]
//! (SFTP over the system `ssh`, spec §4) and [`LocalFs`] (the local disk; used for local targets and
//! by every test). The conflict-safe write algorithm (spec §5) is written once, as provided methods
//! on top of a few primitives, so the tests exercise exactly what runs against a remote.

mod local;
mod sftp;

pub use local::LocalFs;
pub use sftp::SftpRemote;

use anyhow::{bail, Context, Result};
use std::io::Write;
use std::path::Path;
use std::time::SystemTime;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stat {
    pub is_dir: bool,
    pub size: u64,
    /// SFTP v3 carries whole seconds; the local disk carries nanoseconds.
    pub mtime: SystemTime,
    /// Permission bits only (`& 0o7777`).
    pub mode: u32,
}

/// What a write is checked against: the remote `(size, mtime)` recorded at fetch time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fingerprint {
    pub size: u64,
    pub mtime: SystemTime,
}

impl Stat {
    pub fn fingerprint(&self) -> Fingerprint {
        Fingerprint {
            size: self.size,
            mtime: self.mtime,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriteOutcome {
    /// The target now holds the local content; `now` is its new stat (the next write's baseline).
    Written { now: Stat },
    /// The target changed (or vanished) since it was fetched; nothing was written.
    Conflict { now: Option<Stat> },
}

/// File operations on one host. Paths are absolute (or home-relative for SFTP) unix paths.
pub trait RemoteFs {
    /// `stat` following symlinks; `Ok(None)` when the path does not exist.
    fn stat_opt(&mut self, path: &str) -> Result<Option<Stat>>;
    /// Absolute, symlink-resolved path. Relative paths resolve against the remote home (SFTP) or
    /// the working directory (local); `~` and `~/…` expand to the home.
    fn realpath(&mut self, path: &str) -> Result<String>;
    /// Up to `max` bytes from the start of a file (for the text sniff).
    fn read_head(&mut self, path: &str, max: usize) -> Result<Vec<u8>>;
    /// Entry names of a directory, sorted (detection only — sopen never syncs directories).
    fn list_dir(&mut self, path: &str) -> Result<Vec<String>>;
    /// Streams a file's content into `out`; `progress` gets the running byte count.
    fn read_to(
        &mut self,
        path: &str,
        out: &mut dyn Write,
        progress: &mut dyn FnMut(u64),
    ) -> Result<u64>;
    /// Creates `path` exclusively (fails if it exists), sets `mode` first when given, writes the
    /// local file into it and flushes it to disk (`fsync@openssh.com` when the server offers it).
    fn create_from(&mut self, local: &Path, path: &str, mode: Option<u32>) -> Result<()>;
    fn set_mode(&mut self, path: &str, mode: u32) -> Result<()>;
    /// Renames `from` over `to`, replacing it (`posix-rename@openssh.com` when offered).
    fn rename_over(&mut self, from: &str, to: &str) -> Result<()>;
    /// Publishes `from` as `to` without replacing anything (a hard link; fails if `to` exists).
    fn link_new(&mut self, from: &str, to: &str) -> Result<()>;
    fn remove(&mut self, path: &str) -> Result<()>;

    fn stat(&mut self, path: &str) -> Result<Stat> {
        self.stat_opt(path)?
            .with_context(|| format!("{path}: no such file or directory"))
    }

    /// Downloads `remote` to `local` (written via a `.part` file, then renamed) and returns the
    /// stat it was fetched at. The stat is taken before and after the transfer; if they differ
    /// the file changed mid-transfer and the fetch is retried once, then refused.
    fn fetch(
        &mut self,
        remote: &str,
        local: &Path,
        progress: &mut dyn FnMut(u64, u64),
    ) -> Result<Stat> {
        if let Some(parent) = local.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("cannot create {}", parent.display()))?;
        }
        let part = local.with_file_name(format!(
            "{}.part-{}",
            local
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            random_hex()
        ));
        for _ in 0..2 {
            let before = self.stat(remote)?;
            if before.is_dir {
                bail!("{remote} is a directory");
            }
            let result = (|| -> Result<u64> {
                let file = std::fs::File::create(&part)
                    .with_context(|| format!("cannot create {}", part.display()))?;
                let mut out = std::io::BufWriter::with_capacity(1 << 20, file);
                let mut cb = |done: u64| progress(done, before.size);
                let n = self.read_to(remote, &mut out, &mut cb)?;
                out.flush()?;
                Ok(n)
            })();
            let n = match result {
                Ok(n) => n,
                Err(e) => {
                    let _ = std::fs::remove_file(&part);
                    return Err(e);
                }
            };
            let after = self.stat_opt(remote)?;
            if after.map(|s| s.fingerprint()) == Some(before.fingerprint()) && n == before.size {
                std::fs::rename(&part, local)
                    .with_context(|| format!("cannot move into {}", local.display()))?;
                return Ok(before);
            }
            let _ = std::fs::remove_file(&part);
        }
        bail!("{remote} kept changing while it was fetched; try again when it is quiet")
    }

    /// Replaces `target` with the local file — only if its `(size, mtime)` still equals
    /// `expected`. Upload goes to `.<name>.sopen-<random>` in the same directory (created with the
    /// target's permission bits), the target is re-checked, then the temp is renamed over it and
    /// the permission bits are verified/restored. Any failure removes the temp file.
    fn write_atomic(
        &mut self,
        local: &Path,
        target: &str,
        expected: Fingerprint,
    ) -> Result<WriteOutcome> {
        let current = match self.stat_opt(target)? {
            Some(s) if s.fingerprint() == expected => s,
            other => return Ok(WriteOutcome::Conflict { now: other }),
        };
        if current.is_dir {
            bail!("{target} is a directory");
        }
        let mode = current.mode & 0o7777;
        let (dir, name) = split_parent(target);
        let tmp = join(dir, &format!(".{name}.sopen-{}", random_hex()));
        let staged = stage_and_swap(self, local, target, &tmp, mode, expected);
        if !matches!(staged, Ok(true)) {
            let _ = self.remove(&tmp);
        }
        if !staged? {
            return Ok(WriteOutcome::Conflict {
                now: self.stat_opt(target)?,
            });
        }
        let mut now = self.stat(target)?;
        if now.mode & 0o7777 != mode {
            self.set_mode(target, mode)
                .with_context(|| format!("wrote {target} but could not restore mode {mode:o}"))?;
            now = self.stat(target)?;
        }
        Ok(WriteOutcome::Written { now })
    }

    /// Uploads the local file into `dir` as `wanted`, or `<stem>-N.<ext>` when that name is
    /// taken; never replaces an existing file. Returns the final name.
    fn write_new_sibling(&mut self, local: &Path, dir: &str, wanted: &str) -> Result<String> {
        if wanted.is_empty() || wanted.contains('/') {
            bail!("invalid file name '{wanted}'");
        }
        let tmp = join(dir, &format!(".{wanted}.sopen-{}", random_hex()));
        if let Err(e) = self.create_from(local, &tmp, None) {
            let _ = self.remove(&tmp);
            return Err(e);
        }
        let published = publish_unique(self, &tmp, dir, wanted);
        let _ = self.remove(&tmp);
        published
    }
}

/// Upload to `tmp`, re-check the target, rename over it. `Ok(false)` = the target changed.
fn stage_and_swap<F: RemoteFs + ?Sized>(
    fs: &mut F,
    local: &Path,
    target: &str,
    tmp: &str,
    mode: u32,
    expected: Fingerprint,
) -> Result<bool> {
    fs.create_from(local, tmp, Some(mode))
        .with_context(|| format!("cannot upload to {tmp}"))?;
    match fs.stat_opt(target)? {
        Some(s) if s.fingerprint() == expected => {}
        _ => return Ok(false),
    }
    fs.rename_over(tmp, target)
        .with_context(|| format!("cannot rename {tmp} over {target}"))?;
    Ok(true)
}

fn publish_unique<F: RemoteFs + ?Sized>(
    fs: &mut F,
    tmp: &str,
    dir: &str,
    wanted: &str,
) -> Result<String> {
    for n in 0..10_000u32 {
        let name = if n == 0 {
            wanted.to_string()
        } else {
            suffixed(wanted, n)
        };
        let path = join(dir, &name);
        if fs.stat_opt(&path)?.is_some() {
            continue;
        }
        match fs.link_new(tmp, &path) {
            Ok(()) => return Ok(name),
            Err(_) if fs.stat_opt(&path)?.is_some() => continue,
            Err(link_err) => {
                // No hard links on this filesystem/server: rename instead. The name was free a
                // moment ago; the window to a concurrent creator is one round trip.
                crate::debug!("link {tmp} -> {path} failed ({link_err:#}); renaming instead");
                fs.rename_over(tmp, &path)
                    .with_context(|| format!("cannot create {path}"))?;
                return Ok(name);
            }
        }
    }
    bail!("no free name for {wanted} in {dir}")
}

/// `("/a/b", "c.txt")` for `/a/b/c.txt`; `("/", "c")` for `/c`; `(".", "c")` for `c`.
pub fn split_parent(path: &str) -> (&str, &str) {
    let trimmed = path.trim_end_matches('/');
    match trimmed.rfind('/') {
        Some(0) => ("/", &trimmed[1..]),
        Some(i) => (&trimmed[..i], &trimmed[i + 1..]),
        None => (".", trimmed),
    }
}

pub fn join(dir: &str, name: &str) -> String {
    if dir.ends_with('/') {
        format!("{dir}{name}")
    } else {
        format!("{dir}/{name}")
    }
}

/// `t.cut-1-3.mp3` → `t.cut-1-3-2.mp3` for n = 2; names without an extension get `-N` appended.
pub fn suffixed(name: &str, n: u32) -> String {
    match name.rfind('.') {
        Some(i) if i > 0 => format!("{}-{n}{}", &name[..i], &name[i..]),
        _ => format!("{name}-{n}"),
    }
}

/// 16 hex chars, unique per call (per-process random keys + time + counter).
pub fn random_hex() -> String {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let mut h = RandomState::new().build_hasher();
    h.write_u128(
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    );
    h.write_u32(std::process::id());
    h.write_u64(COUNTER.fetch_add(1, Ordering::Relaxed));
    format!("{:016x}", h.finish())
}

#[cfg(test)]
mod tests;
