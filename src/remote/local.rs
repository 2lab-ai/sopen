//! [`RemoteFs`] on the local disk: local targets, and the implementation every test runs against.

use super::{CreateError, Created, Hashing, RemoteFs, Stat, Uploaded};
use anyhow::{anyhow, Context, Result};
use std::fs;
use std::io::{ErrorKind, Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;

#[derive(Debug, Default)]
pub struct LocalFs;

impl LocalFs {
    pub fn new() -> Self {
        Self
    }
}

pub(crate) fn stat_of(meta: &fs::Metadata) -> Stat {
    Stat {
        is_dir: meta.is_dir(),
        is_file: meta.is_file(),
        is_symlink: meta.file_type().is_symlink(),
        size: meta.len(),
        mtime: meta.modified().unwrap_or(std::time::UNIX_EPOCH),
        mode: meta.permissions().mode() & 0o7777,
        uid: Some(meta.uid()),
        gid: Some(meta.gid()),
        missing: None,
        sha256: None,
    }
}

fn stat_with(path: &str, meta: std::io::Result<fs::Metadata>) -> Result<Option<Stat>> {
    match meta {
        Ok(meta) => Ok(Some(stat_of(&meta))),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("cannot stat {path}")),
    }
}

impl RemoteFs for LocalFs {
    fn stat_opt(&mut self, path: &str) -> Result<Option<Stat>> {
        stat_with(path, fs::metadata(path))
    }

    fn lstat_opt(&mut self, path: &str) -> Result<Option<Stat>> {
        stat_with(path, fs::symlink_metadata(path))
    }

    fn realpath(&mut self, path: &str) -> Result<String> {
        let expanded = match path.strip_prefix('~') {
            Some(rest) if rest.is_empty() || rest.starts_with('/') => {
                let home = std::env::var("HOME").context("HOME is not set")?;
                format!("{home}{rest}")
            }
            _ if path.is_empty() => ".".to_string(),
            _ => path.to_string(),
        };
        let abs = fs::canonicalize(&expanded)
            .with_context(|| format!("{path}: no such file or directory"))?;
        abs.into_os_string()
            .into_string()
            .map_err(|p| anyhow!("{}: path is not valid UTF-8", p.to_string_lossy()))
    }

    fn read_head(&mut self, path: &str, max: usize) -> Result<Vec<u8>> {
        let file = fs::File::open(path).with_context(|| format!("cannot open {path}"))?;
        let mut buf = Vec::with_capacity(max);
        file.take(max as u64)
            .read_to_end(&mut buf)
            .with_context(|| format!("cannot read {path}"))?;
        Ok(buf)
    }

    fn list_dir(&mut self, path: &str) -> Result<Vec<String>> {
        let mut names = Vec::new();
        for entry in fs::read_dir(path).with_context(|| format!("cannot list {path}"))? {
            names.push(entry?.file_name().to_string_lossy().into_owned());
        }
        names.sort();
        Ok(names)
    }

    fn read_to(
        &mut self,
        path: &str,
        out: &mut dyn Write,
        progress: &mut dyn FnMut(u64),
    ) -> Result<u64> {
        let mut file = fs::File::open(path).with_context(|| format!("cannot open {path}"))?;
        let mut buf = vec![0u8; 1 << 20];
        let mut done = 0u64;
        loop {
            let n = file
                .read(&mut buf)
                .with_context(|| format!("cannot read {path}"))?;
            if n == 0 {
                return Ok(done);
            }
            out.write_all(&buf[..n])?;
            done += n as u64;
            progress(done);
        }
    }

    fn create_from(
        &mut self,
        local: &Path,
        path: &str,
        mode: Option<u32>,
    ) -> std::result::Result<Uploaded, CreateError> {
        let mut src = fs::File::open(local)
            .with_context(|| format!("cannot open {}", local.display()))
            .map_err(|e| CreateError::new(Created::No, e))?;
        // A brand-new file gets 0666 minus the umask, as `cp` would; a temp that stands in for an
        // existing file starts private and then takes that file's bits before any content lands.
        let dst = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(if mode.is_some() { 0o600 } else { 0o666 })
            .open(path)
            .with_context(|| format!("cannot create {path}"))
            .map_err(|e| CreateError::new(Created::No, e))?;
        let mut fill = || -> Result<Uploaded> {
            if let Some(mode) = mode {
                dst.set_permissions(fs::Permissions::from_mode(mode))
                    .with_context(|| format!("cannot set mode on {path}"))?;
            }
            let mut out = Hashing::new(&dst);
            std::io::copy(&mut src, &mut out).with_context(|| format!("cannot write {path}"))?;
            let written = out.finish();
            dst.sync_all()
                .with_context(|| format!("cannot sync {path}"))?;
            Ok(written)
        };
        fill().map_err(|e| CreateError::new(Created::Yes, e))
    }

    fn set_mode(&mut self, path: &str, mode: u32) -> Result<()> {
        fs::set_permissions(path, fs::Permissions::from_mode(mode))
            .with_context(|| format!("cannot set mode on {path}"))
    }

    fn set_owner(&mut self, path: &str, uid: u32, gid: u32) -> Result<()> {
        std::os::unix::fs::lchown(path, Some(uid), Some(gid))
            .with_context(|| format!("cannot set owner {uid}:{gid} on {path}"))
    }

    fn rename_over(&mut self, from: &str, to: &str) -> Result<()> {
        fs::rename(from, to).with_context(|| format!("cannot rename {from} to {to}"))
    }

    fn link_new(&mut self, from: &str, to: &str) -> Result<()> {
        fs::hard_link(from, to).with_context(|| format!("cannot link {from} to {to}"))
    }

    fn remove(&mut self, path: &str) -> Result<()> {
        fs::remove_file(path).with_context(|| format!("cannot remove {path}"))
    }
}
