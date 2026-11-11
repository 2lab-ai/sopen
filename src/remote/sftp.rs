//! [`RemoteFs`] over SFTP (spec §4). Transport is the system `ssh` binary, so `~/.ssh/config`
//! (aliases, ProxyJump, agent, keys) applies exactly as it does to `ssh <alias>`. One multiplexed
//! master per host: `ControlMaster=auto`, `ControlPath=<state>/cm-%C`, `ControlPersist=60s`;
//! a later run within the minute reuses it. Nothing runs on the remote but sshd's sftp subsystem.

use super::{RemoteFs, Stat};
use crate::target::RemoteSpec;
use anyhow::{anyhow, bail, Context, Result};
use bytes::BytesMut;
use openssh_sftp_client::error::{Error as SftpErr, SftpErrorKind};
use openssh_sftp_client::fs::Fs;
use openssh_sftp_client::metadata::{MetaData, Permissions};
use openssh_sftp_client::{Sftp, SftpOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;
use tokio::runtime::Runtime;

/// macOS `sun_path` holds 104 bytes; ssh binds the master at `<ControlPath>.<16 random>` first.
const MAX_SOCKET_PATH: usize = 103 - 17;

/// Bytes per read request / local read when uploading.
const CHUNK: usize = 256 * 1024;

pub struct SftpRemote {
    rt: Runtime,
    sftp: Option<Sftp>,
    fs: Option<Fs>,
}

impl SftpRemote {
    /// Attaches to (or starts) the ssh master for `spec` and opens an SFTP channel over it.
    pub fn connect(spec: &RemoteSpec, state_dir: &Path) -> Result<Self> {
        let dest = spec.destination();
        let ctl = control_path(&dest, state_dir)?;
        ensure_master(&dest, &ctl)?;
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .context("cannot start the async runtime")?;
        // `resume` attaches to our master and never tears it down on drop; `from_clonable_session`
        // closes only the sftp channel (`from_session` would `-O exit` the shared master).
        let session = openssh::Session::resume(ctl.into_boxed_path(), None);
        let sftp = rt
            .block_on(Sftp::from_clonable_session(
                Arc::new(session),
                SftpOptions::default(),
            ))
            .map_err(|e| anyhow!("cannot start sftp on {}: {e}", spec.label()))?;
        crate::debug!(
            "sftp {}: fsync={} posix-rename={} hardlink={} expand-path={}",
            spec.label(),
            sftp.support_fsync(),
            sftp.support_posix_rename(),
            sftp.support_hardlink(),
            sftp.support_expand_path()
        );
        let fs = sftp.fs();
        Ok(Self {
            rt,
            sftp: Some(sftp),
            fs: Some(fs),
        })
    }

    fn parts(&mut self) -> (&Runtime, &Sftp, &mut Fs) {
        let Self { rt, sftp, fs } = self;
        (
            rt,
            sftp.as_ref().expect("open session"),
            fs.as_mut().expect("open session"),
        )
    }
}

impl Drop for SftpRemote {
    fn drop(&mut self) {
        drop(self.fs.take());
        if let Some(sftp) = self.sftp.take() {
            let _ = self.rt.block_on(async {
                tokio::time::timeout(Duration::from_secs(5), sftp.close()).await
            });
        }
    }
}

/// `ControlPath=<state>/cm-%C` expanded by ssh itself (`ssh -G` prints the expanded value).
fn control_path(dest: &str, state_dir: &Path) -> Result<PathBuf> {
    create_private_dir(state_dir)?;
    let pattern = state_dir.join("cm-%C");
    let out = Command::new("ssh")
        .arg("-G")
        .arg("-S")
        .arg(&pattern)
        .arg(dest)
        .stdin(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .context("cannot run ssh (is OpenSSH installed?)")?;
    if !out.status.success() {
        bail!(
            "ssh cannot read the config for {dest}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    let path = stdout
        .lines()
        .find_map(|l| l.strip_prefix("controlpath "))
        .map(|p| PathBuf::from(p.trim()))
        .context("ssh -G printed no controlpath")?;
    if path.as_os_str().len() > MAX_SOCKET_PATH {
        bail!(
            "ssh control socket path is too long ({} bytes): set SOPEN_STATE_DIR to a shorter directory",
            path.as_os_str().len()
        );
    }
    Ok(path)
}

fn create_private_dir(dir: &Path) -> Result<()> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .with_context(|| format!("cannot create {}", dir.display()))?;
    let mode = std::fs::metadata(dir)?.permissions().mode();
    if mode & 0o077 != 0 {
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
            .with_context(|| format!("cannot make {} private", dir.display()))?;
    }
    Ok(())
}

fn ensure_master(dest: &str, ctl: &Path) -> Result<()> {
    let alive = Command::new("ssh")
        .arg("-S")
        .arg(ctl)
        .args(["-O", "check", dest])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    if alive {
        crate::debug!("ssh master for {dest} is running; reusing it");
        return Ok(());
    }
    let mut cmd = Command::new("ssh");
    cmd.arg("-S")
        .arg(ctl)
        .args([
            "-o",
            "ControlMaster=auto",
            "-o",
            "ControlPersist=60s",
            "-f",
            "-N",
            dest,
        ])
        // Prompts (passphrase, host key) go through the terminal as they do for plain ssh.
        .stdout(Stdio::null());
    crate::debug!(
        "{}",
        crate::render_command(
            cmd.get_program(),
            &cmd.get_args().map(|a| a.to_os_string()).collect::<Vec<_>>()
        )
    );
    let status = cmd.status().context("cannot run ssh")?;
    if !status.success() {
        bail!("ssh could not connect to {dest} ({status})");
    }
    Ok(())
}

fn is_not_found(e: &SftpErr) -> bool {
    matches!(e, SftpErr::SftpError(SftpErrorKind::NoSuchFile, _))
}

fn stat_of(meta: &MetaData) -> Stat {
    Stat {
        is_dir: meta.file_type().is_some_and(|t| t.is_dir()),
        size: meta.len().unwrap_or(0),
        mtime: meta
            .modified()
            .map(|t| t.as_system_time())
            .unwrap_or(std::time::UNIX_EPOCH),
        mode: meta.permissions().map(|p| p.as_raw().bits()).unwrap_or(0) & 0o7777,
    }
}

fn perms(mode: u32) -> Permissions {
    Permissions::from((mode & 0o7777) as u16)
}

impl RemoteFs for SftpRemote {
    fn stat_opt(&mut self, path: &str) -> Result<Option<Stat>> {
        let (rt, _, fs) = self.parts();
        match rt.block_on(fs.metadata(path)) {
            Ok(meta) => Ok(Some(stat_of(&meta))),
            Err(e) if is_not_found(&e) => Ok(None),
            Err(e) => Err(anyhow!("cannot stat {path}: {e}")),
        }
    }

    fn realpath(&mut self, path: &str) -> Result<String> {
        let (rt, sftp, fs) = self.parts();
        let mut query = if path.is_empty() { "." } else { path };
        // Servers without expand-path@openssh.com: `~` and `~/…` are home-relative by hand.
        if !sftp.support_expand_path() {
            if query == "~" {
                query = ".";
            } else if let Some(rest) = query.strip_prefix("~/") {
                query = if rest.is_empty() { "." } else { rest };
            }
        }
        let resolved = rt
            .block_on(fs.canonicalize(query))
            .map_err(|e| match is_not_found(&e) {
                true => anyhow!("{path}: no such file or directory"),
                false => anyhow!("cannot resolve {path}: {e}"),
            })?;
        resolved
            .into_os_string()
            .into_string()
            .map_err(|p| anyhow!("{}: path is not valid UTF-8", p.to_string_lossy()))
    }

    fn read_head(&mut self, path: &str, max: usize) -> Result<Vec<u8>> {
        let (rt, sftp, _) = self.parts();
        rt.block_on(async {
            let mut file = sftp
                .open(path)
                .await
                .map_err(|e| anyhow!("cannot open {path}: {e}"))?;
            let mut buf = Vec::with_capacity(max);
            while buf.len() < max {
                let want = (max - buf.len()) as u32;
                match file
                    .read(want, BytesMut::with_capacity(want as usize))
                    .await
                    .map_err(|e| anyhow!("cannot read {path}: {e}"))?
                {
                    Some(chunk) => buf.extend_from_slice(&chunk),
                    None => break,
                }
            }
            let _ = file.close().await;
            Ok(buf)
        })
    }

    fn list_dir(&mut self, path: &str) -> Result<Vec<String>> {
        use futures_core::Stream;
        let (rt, _, fs) = self.parts();
        rt.block_on(async {
            let dir = fs
                .open_dir(path)
                .await
                .map_err(|e| anyhow!("cannot list {path}: {e}"))?;
            let mut entries = Box::pin(dir.read_dir());
            let mut names = Vec::new();
            while let Some(entry) = std::future::poll_fn(|cx| entries.as_mut().poll_next(cx)).await
            {
                let entry = entry.map_err(|e| anyhow!("cannot list {path}: {e}"))?;
                let name = entry.filename().to_string_lossy().into_owned();
                if name != "." && name != ".." {
                    names.push(name);
                }
            }
            names.sort();
            Ok(names)
        })
    }

    fn read_to(
        &mut self,
        path: &str,
        out: &mut dyn Write,
        progress: &mut dyn FnMut(u64),
    ) -> Result<u64> {
        let (rt, sftp, _) = self.parts();
        rt.block_on(async {
            let mut file = sftp
                .open(path)
                .await
                .map_err(|e| anyhow!("cannot open {path}: {e}"))?;
            // The client caps each request at the server's read limit.
            let chunk = CHUNK as u32;
            let mut done = 0u64;
            while let Some(bytes) = file
                .read(chunk, BytesMut::with_capacity(chunk as usize))
                .await
                .map_err(|e| anyhow!("cannot read {path}: {e}"))?
            {
                out.write_all(&bytes)?;
                done += bytes.len() as u64;
                progress(done);
            }
            let _ = file.close().await;
            Ok(done)
        })
    }

    fn create_from(&mut self, local: &Path, path: &str, mode: Option<u32>) -> Result<()> {
        let (rt, sftp, _) = self.parts();
        let mut src = std::fs::File::open(local)
            .with_context(|| format!("cannot open {}", local.display()))?;
        rt.block_on(async {
            let mut file = sftp
                .options()
                .write(true)
                .create_new(true)
                .open(path)
                .await
                .map_err(|e| anyhow!("cannot create {path}: {e}"))?;
            if let Some(mode) = mode {
                file.set_permissions(perms(mode))
                    .await
                    .map_err(|e| anyhow!("cannot set mode on {path}: {e}"))?;
            }
            // write_all splits at the server's write limit.
            let mut buf = vec![0u8; CHUNK];
            loop {
                let n = src
                    .read(&mut buf)
                    .with_context(|| format!("cannot read {}", local.display()))?;
                if n == 0 {
                    break;
                }
                file.write_all(&buf[..n])
                    .await
                    .map_err(|e| anyhow!("cannot write {path}: {e}"))?;
            }
            if sftp.support_fsync() {
                file.sync_all()
                    .await
                    .map_err(|e| anyhow!("cannot fsync {path}: {e}"))?;
            }
            file.close()
                .await
                .map_err(|e| anyhow!("cannot close {path}: {e}"))
        })
    }

    fn set_mode(&mut self, path: &str, mode: u32) -> Result<()> {
        let (rt, _, fs) = self.parts();
        rt.block_on(fs.set_permissions(path, perms(mode)))
            .map_err(|e| anyhow!("cannot set mode on {path}: {e}"))
    }

    fn rename_over(&mut self, from: &str, to: &str) -> Result<()> {
        // Fs::rename sends posix-rename@openssh.com when advertised (atomic replace), plain
        // SSH_FXP_RENAME otherwise — which refuses an existing target rather than replacing it.
        let (rt, _, fs) = self.parts();
        rt.block_on(fs.rename(from, to))
            .map_err(|e| anyhow!("cannot rename {from} to {to}: {e}"))
    }

    fn link_new(&mut self, from: &str, to: &str) -> Result<()> {
        let (rt, _, fs) = self.parts();
        rt.block_on(fs.hard_link(from, to))
            .map_err(|e| anyhow!("cannot link {from} to {to}: {e}"))
    }

    fn remove(&mut self, path: &str) -> Result<()> {
        let (rt, _, fs) = self.parts();
        rt.block_on(fs.remove_file(path))
            .map_err(|e| anyhow!("cannot remove {path}: {e}"))
    }
}
