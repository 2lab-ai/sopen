//! [`RemoteFs`] over SFTP (spec §4). Transport is the system `ssh` binary, so `~/.ssh/config`
//! (aliases, ProxyJump, agent, keys) applies exactly as it does to `ssh <alias>`. One multiplexed
//! master per host: `ControlMaster=auto`, `ControlPath=<state>/cm-%C`, `ControlPersist=60s`;
//! a later run within the minute reuses it. Nothing runs on the remote but sshd's sftp subsystem.
//!
//! Nothing waits forever: the master sends keepalives (a dead link fails in ~30 s), every
//! metadata request has [`META_TIMEOUT`], and a transfer fails after [`STALL_TIMEOUT`] without
//! progress. A request that changes the remote and times out is reported as
//! [`OutcomeUnknown`](super::OutcomeUnknown), never as "nothing written".

use super::{unknown, CreateError, Created, Hashing, Like, RemoteFs, Stat, Uploaded};
use crate::target::RemoteSpec;
use anyhow::{anyhow, bail, Context, Result};
use bytes::BytesMut;
use openssh_sftp_client::error::{Error as SftpErr, SftpErrorKind};
use openssh_sftp_client::fs::Fs;
use openssh_sftp_client::metadata::{MetaData, MetaDataBuilder, Permissions};
use openssh_sftp_client::{Sftp, SftpOptions};
use std::ffi::OsString;
use std::future::Future;
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

/// One metadata request (stat, open, rename, setstat, …).
pub const META_TIMEOUT: Duration = Duration::from_secs(30);
/// A transfer that makes no progress for this long fails.
pub const STALL_TIMEOUT: Duration = Duration::from_secs(60);

pub struct SftpRemote {
    rt: Runtime,
    sftp: Option<Sftp>,
    fs: Option<Fs>,
}

/// `Err` = the request got no reply within `limit`.
async fn within<F: Future>(limit: Duration, fut: F) -> std::result::Result<F::Output, ()> {
    tokio::time::timeout(limit, fut).await.map_err(|_| ())
}

fn timed_out(what: &str, limit: Duration) -> anyhow::Error {
    anyhow!("{what}: no reply from the server in {}s", limit.as_secs())
}

impl SftpRemote {
    /// Attaches to (or starts) the ssh master for `spec` and opens an SFTP channel over it.
    pub fn connect(spec: &RemoteSpec, state_dir: &Path) -> Result<Self> {
        let dest = spec.destination();
        let config = ssh_config(&dest, state_dir)?;
        ensure_master(&dest, &config)?;
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .context("cannot start the async runtime")?;
        // `resume` attaches to our master and never tears it down on drop; `from_clonable_session`
        // closes only the sftp channel (`from_session` would `-O exit` the shared master).
        let session = openssh::Session::resume(config.control_path.into_boxed_path(), None);
        let started = rt.block_on(within(
            META_TIMEOUT,
            Sftp::from_clonable_session(Arc::new(session), SftpOptions::default()),
        ));
        let sftp = match started {
            Ok(r) => r.map_err(|e| anyhow!("cannot start sftp on {}: {e}", spec.label()))?,
            Err(()) => {
                return Err(timed_out(
                    &format!("starting sftp on {}", spec.label()),
                    META_TIMEOUT,
                ))
            }
        };
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

    fn stat_by(&mut self, path: &str, follow: bool) -> Result<Option<Stat>> {
        let (rt, _, fs) = self.parts();
        let got = if follow {
            rt.block_on(within(META_TIMEOUT, fs.metadata(path)))
        } else {
            rt.block_on(within(META_TIMEOUT, fs.symlink_metadata(path)))
        };
        match got.map_err(|()| timed_out(&format!("stat {path}"), META_TIMEOUT))? {
            Ok(meta) => Ok(Some(stat_of(&meta))),
            Err(e) if is_not_found(&e) => Ok(None),
            Err(e) => Err(anyhow!("cannot stat {path}: {e}")),
        }
    }
}

/// Did the server answer? Only an SFTP status reply (`SSH_FX_FAILURE`, `PERMISSION_DENIED`,
/// `NO_SUCH_FILE`, …) proves a request did not happen; an extension the server does not offer is
/// refused before anything is sent. Every other error — connection closed, EOF, broken pipe, a
/// garbled or unexpected reply — came after the request may have reached the server.
fn answered(e: &SftpErr) -> bool {
    matches!(e, SftpErr::SftpError(..) | SftpErr::UnsupportedExtension(_))
}

/// A failed request that changes the remote: refused by the server, or outcome unknown.
fn failed_change(what: &str, e: SftpErr) -> anyhow::Error {
    if answered(&e) {
        anyhow!("cannot {what}: {e}")
    } else {
        unknown(format!(
            "{what}: request sent, reply lost ({e}) — it may or may not have happened"
        ))
    }
}

/// The result of a request that changes the remote: a timeout or a lost reply is
/// [`OutcomeUnknown`](super::OutcomeUnknown), never "nothing happened".
fn changed(what: &str, got: std::result::Result<Result<(), SftpErr>, ()>) -> Result<()> {
    match got {
        Ok(r) => r.map_err(|e| failed_change(what, e)),
        Err(()) => Err(unknown(format!(
            "{what}: request sent, no reply in {}s — it may or may not have happened",
            META_TIMEOUT.as_secs()
        ))),
    }
}

/// A failed exclusive open: refused by the server = not created; a lost reply = unknown.
fn failed_create(path: &str, e: SftpErr) -> CreateError {
    let created = if answered(&e) {
        Created::No
    } else {
        Created::Unknown
    };
    CreateError::new(created, failed_change(&format!("create {path}"), e))
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

/// What `ssh -G` says about the destination that sopen needs.
#[derive(Debug, PartialEq, Eq)]
struct SshConfig {
    control_path: PathBuf,
    /// The user's config sets ConnectTimeout; sopen then leaves it alone.
    connect_timeout_set: bool,
}

/// `ControlPath=<state>/cm-%C` expanded by ssh itself (`ssh -G` prints the expanded value).
fn ssh_config(dest: &str, state_dir: &Path) -> Result<SshConfig> {
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
    let config = parse_ssh_g(&String::from_utf8_lossy(&out.stdout))?;
    if config.control_path.as_os_str().len() > MAX_SOCKET_PATH {
        bail!(
            "ssh control socket path is too long ({} bytes): set SOPEN_STATE_DIR to a shorter directory",
            config.control_path.as_os_str().len()
        );
    }
    Ok(config)
}

fn parse_ssh_g(stdout: &str) -> Result<SshConfig> {
    let value = |key: &str| {
        stdout
            .lines()
            .find_map(|l| l.strip_prefix(key)?.strip_prefix(' ').map(str::trim))
    };
    let control_path = value("controlpath")
        .map(PathBuf::from)
        .context("ssh -G printed no controlpath")?;
    let connect_timeout_set = value("connecttimeout").is_some_and(|v| v != "none");
    Ok(SshConfig {
        control_path,
        connect_timeout_set,
    })
}

/// The state dir holds ssh control sockets, so it must be this account's own private
/// directory. Created 0700 (with missing parents) when absent. An existing one is opened
/// without following a symlink and checked through that descriptor — a directory owned by this
/// account — then made 0700 through the same descriptor if group or others had access; never
/// chmodded by path. Anything else is refused with one error.
fn create_private_dir(dir: &Path) -> Result<()> {
    create_private_dir_with(dir, &mut || {})
}

/// [`create_private_dir`], with `between` called between the path check and the open (tests:
/// the entry swapped meanwhile).
fn create_private_dir_with(dir: &Path, between: &mut dyn FnMut()) -> Result<()> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
    let refuse = |why: String| {
        anyhow!(
            "{} {why}; sopen keeps its ssh control sockets there — remove it or set \
             SOPEN_STATE_DIR",
            dir.display()
        )
    };
    match std::fs::symlink_metadata(dir) {
        Ok(m) if m.file_type().is_symlink() => return Err(refuse("is a symlink".into())),
        Ok(m) if !m.is_dir() => return Err(refuse("is not a directory".into())),
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(dir)
                .with_context(|| format!("cannot create {}", dir.display()))?;
        }
        Err(e) => return Err(e).with_context(|| format!("cannot stat {}", dir.display())),
    }
    between();
    let opened = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY)
        .open(dir);
    let handle = match opened {
        Ok(h) => h,
        // Swapped since the check above (a symlink fails the open with ELOOP, or with ENOTDIR
        // on systems that check O_DIRECTORY first): say which, from what is there now.
        Err(e) if matches!(e.raw_os_error(), Some(libc::ELOOP | libc::ENOTDIR)) => {
            let link = std::fs::symlink_metadata(dir).is_ok_and(|m| m.file_type().is_symlink());
            let why = if link {
                "is a symlink"
            } else {
                "is not a directory"
            };
            return Err(refuse(why.into()));
        }
        Err(e) => return Err(e).with_context(|| format!("cannot open {}", dir.display())),
    };
    let meta = handle.metadata()?;
    // SAFETY: geteuid has no preconditions and cannot fail.
    let me = unsafe { libc::geteuid() };
    if meta.uid() != me {
        return Err(refuse(format!(
            "is owned by uid {}, not this account (uid {me})",
            meta.uid()
        )));
    }
    if meta.mode() & 0o077 != 0 {
        handle
            .set_permissions(std::fs::Permissions::from_mode(0o700))
            .with_context(|| format!("cannot make {} private", dir.display()))?;
    }
    Ok(())
}

/// The connection options of sopen's multiplexed master for `dest`, for an ssh that runs a
/// command there (`--target`, spec §4a): `-S <ControlPath>` and [`master_options`]. With
/// `ControlMaster=auto` that ssh attaches to a running master or becomes one.
pub fn connection_args(dest: &str, state_dir: &Path) -> Result<Vec<OsString>> {
    Ok(connection_args_for(&ssh_config(dest, state_dir)?))
}

fn connection_args_for(config: &SshConfig) -> Vec<OsString> {
    let mut args = vec![
        OsString::from("-S"),
        config.control_path.clone().into_os_string(),
    ];
    args.extend(master_options(config).into_iter().map(OsString::from));
    args
}

/// Options for a new master: multiplexing, keepalives (a dead link drops the master after
/// ~30 s instead of hanging every request), and a connect bound unless the config has one.
/// Command-line `-o` wins over `~/.ssh/config`.
fn master_options(config: &SshConfig) -> Vec<&'static str> {
    let mut opts = vec![
        "-o",
        "ControlMaster=auto",
        "-o",
        "ControlPersist=60s",
        "-o",
        "ServerAliveInterval=10",
        "-o",
        "ServerAliveCountMax=3",
    ];
    if !config.connect_timeout_set {
        opts.extend(["-o", "ConnectTimeout=15"]);
    }
    opts
}

fn ensure_master(dest: &str, config: &SshConfig) -> Result<()> {
    let ctl = &config.control_path;
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
    cmd.args(connection_args_for(config))
        .args(["-f", "-N", dest])
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

/// Attributes the server leaves out become zeros and are named in [`Stat::missing`].
fn stat_of(meta: &MetaData) -> Stat {
    let kind = meta.file_type();
    let size = meta.len();
    let mtime = meta.modified();
    let perms = meta.permissions();
    let (uid, gid) = (meta.uid(), meta.gid());
    let missing = [
        ("size", size.is_none()),
        ("mtime", mtime.is_none()),
        ("permissions", perms.is_none()),
        ("type", kind.is_none()),
        ("owner", uid.is_none() || gid.is_none()),
    ]
    .into_iter()
    .find_map(|(name, absent)| absent.then_some(name));
    Stat {
        is_dir: kind.is_some_and(|t| t.is_dir()),
        is_file: kind.is_some_and(|t| t.is_file()),
        is_symlink: kind.is_some_and(|t| t.is_symlink()),
        size: size.unwrap_or(0),
        mtime: mtime
            .map(|t| t.as_system_time())
            .unwrap_or(std::time::UNIX_EPOCH),
        mode: perms.map(|p| p.as_raw().bits()).unwrap_or(0) & 0o7777,
        uid,
        gid,
        missing,
        sha256: None,
    }
}

fn perms(mode: u32) -> Permissions {
    Permissions::from((mode & 0o7777) as u16)
}

impl RemoteFs for SftpRemote {
    fn stat_opt(&mut self, path: &str) -> Result<Option<Stat>> {
        self.stat_by(path, true)
    }

    fn lstat_opt(&mut self, path: &str) -> Result<Option<Stat>> {
        self.stat_by(path, false)
    }

    fn realpath(&mut self, path: &str) -> Result<String> {
        let expand = self.sftp.as_ref().is_some_and(|s| s.support_expand_path());
        let mut query = if path.is_empty() { "." } else { path };
        // Servers without expand-path@openssh.com: `~` and `~/…` are home-relative by hand.
        if !expand {
            if query == "~" {
                query = ".";
            } else if let Some(rest) = query.strip_prefix("~/") {
                query = if rest.is_empty() { "." } else { rest };
            }
        }
        let (rt, _, fs) = self.parts();
        let resolved = rt
            .block_on(within(META_TIMEOUT, fs.canonicalize(query)))
            .map_err(|()| timed_out(&format!("resolve {path}"), META_TIMEOUT))?
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
        let mut buf = Vec::with_capacity(max);
        let mut sink = |chunk: &[u8]| {
            buf.extend_from_slice(chunk);
            Ok(())
        };
        self.read_with(path, Some(max), &mut sink, &mut |_| {})?;
        Ok(buf)
    }

    fn list_dir(&mut self, path: &str) -> Result<Vec<String>> {
        use futures_core::Stream;
        let (rt, _, fs) = self.parts();
        let listed = rt.block_on(within(META_TIMEOUT, async {
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
        }));
        listed.map_err(|()| timed_out(&format!("list {path}"), META_TIMEOUT))?
    }

    fn read_to(
        &mut self,
        path: &str,
        out: &mut dyn Write,
        progress: &mut dyn FnMut(u64),
    ) -> Result<u64> {
        let mut sink = |chunk: &[u8]| out.write_all(chunk).map_err(anyhow::Error::from);
        self.read_with(path, None, &mut sink, progress)
    }

    fn create_from(
        &mut self,
        local: &Path,
        path: &str,
        like: Option<Like>,
    ) -> std::result::Result<Uploaded, CreateError> {
        let not_created = |e| CreateError::new(Created::No, e);
        let mut src = std::fs::File::open(local)
            .with_context(|| format!("cannot open {}", local.display()))
            .map_err(not_created)?;
        let (rt, sftp, _) = self.parts();
        rt.block_on(async {
            let opened = within(
                META_TIMEOUT,
                sftp.options().write(true).create_new(true).open(path),
            )
            .await;
            let mut file = match opened {
                Ok(Ok(file)) => file,
                Ok(Err(e)) => return Err(failed_create(path, e)),
                Err(()) => {
                    return Err(CreateError::new(
                        Created::Unknown,
                        unknown(format!(
                            "create {path}: request sent, no reply in {}s",
                            META_TIMEOUT.as_secs()
                        )),
                    ))
                }
            };
            // From here `path` is ours.
            let fill = async {
                // Handle-based metadata updates on the open file, never by path: the owner, then
                // the permission bits without set-id bits before the content, then the full mode
                // restored after the content, so set-id bits are applied last.
                if let Some(like) = like {
                    // A refused owner change is not fatal here: the caller checks the result
                    // (a mismatch refuses the save; an owner that was already right passes).
                    let owner = MetaDataBuilder::new().id((like.uid, like.gid)).create();
                    match within(META_TIMEOUT, file.set_metadata(owner)).await {
                        Ok(Ok(())) => {}
                        Ok(Err(e)) => crate::debug!("set owner on {path}: {e}"),
                        Err(()) => {
                            return Err(timed_out(&format!("set owner on {path}"), META_TIMEOUT))
                        }
                    }
                    within(META_TIMEOUT, file.set_permissions(perms(like.mode & 0o777)))
                        .await
                        .map_err(|()| timed_out(&format!("set mode on {path}"), META_TIMEOUT))?
                        .map_err(|e| anyhow!("cannot set mode on {path}: {e}"))?;
                }
                // write_all splits at the server's write limit.
                let mut out = Hashing::new(std::io::sink());
                let mut buf = vec![0u8; CHUNK];
                loop {
                    let n = src
                        .read(&mut buf)
                        .with_context(|| format!("cannot read {}", local.display()))?;
                    if n == 0 {
                        break;
                    }
                    within(STALL_TIMEOUT, file.write_all(&buf[..n]))
                        .await
                        .map_err(|()| timed_out(&format!("write {path}"), STALL_TIMEOUT))?
                        .map_err(|e| anyhow!("cannot write {path}: {e}"))?;
                    out.write_all(&buf[..n])?;
                }
                if let Some(like) = like {
                    within(META_TIMEOUT, file.set_permissions(perms(like.mode)))
                        .await
                        .map_err(|()| timed_out(&format!("set mode on {path}"), META_TIMEOUT))?
                        .map_err(|e| anyhow!("cannot set mode on {path}: {e}"))?;
                }
                if sftp.support_fsync() {
                    within(STALL_TIMEOUT, file.sync_all())
                        .await
                        .map_err(|()| timed_out(&format!("fsync {path}"), STALL_TIMEOUT))?
                        .map_err(|e| anyhow!("cannot fsync {path}: {e}"))?;
                }
                within(META_TIMEOUT, file.close())
                    .await
                    .map_err(|()| timed_out(&format!("close {path}"), META_TIMEOUT))?
                    .map_err(|e| anyhow!("cannot close {path}: {e}"))?;
                Ok(out.finish())
            };
            fill.await.map_err(|e| CreateError::new(Created::Yes, e))
        })
    }

    fn set_mode(&mut self, path: &str, mode: u32) -> Result<()> {
        let (rt, _, fs) = self.parts();
        let got = rt.block_on(within(META_TIMEOUT, fs.set_permissions(path, perms(mode))));
        changed(&format!("set mode on {path}"), got)
    }

    fn rename_over(&mut self, from: &str, to: &str) -> Result<()> {
        // Fs::rename sends posix-rename@openssh.com when advertised (atomic replace), plain
        // SSH_FXP_RENAME otherwise — which refuses an existing target rather than replacing it.
        let (rt, _, fs) = self.parts();
        let got = rt.block_on(within(META_TIMEOUT, fs.rename(from, to)));
        changed(&format!("rename {from} to {to}"), got)
    }

    fn link_new(&mut self, from: &str, to: &str) -> Result<()> {
        let (rt, _, fs) = self.parts();
        let got = rt.block_on(within(META_TIMEOUT, fs.hard_link(from, to)));
        changed(&format!("link {from} to {to}"), got)
    }

    fn can_link(&self) -> bool {
        self.sftp.as_ref().is_some_and(|s| s.support_hardlink())
    }

    fn can_replace(&self) -> bool {
        self.sftp.as_ref().is_some_and(|s| s.support_posix_rename())
    }

    fn remove(&mut self, path: &str) -> Result<()> {
        let (rt, _, fs) = self.parts();
        let got = rt.block_on(within(META_TIMEOUT, fs.remove_file(path)));
        changed(&format!("remove {path}"), got)
    }
}

impl SftpRemote {
    /// Reads `path` (up to `max` bytes) into `sink`, failing after [`STALL_TIMEOUT`] without data.
    fn read_with(
        &mut self,
        path: &str,
        max: Option<usize>,
        sink: &mut dyn FnMut(&[u8]) -> Result<()>,
        progress: &mut dyn FnMut(u64),
    ) -> Result<u64> {
        let (rt, sftp, _) = self.parts();
        rt.block_on(async {
            let mut file = within(META_TIMEOUT, sftp.open(path))
                .await
                .map_err(|()| timed_out(&format!("open {path}"), META_TIMEOUT))?
                .map_err(|e| anyhow!("cannot open {path}: {e}"))?;
            let mut done = 0u64;
            loop {
                // The client caps each request at the server's read limit.
                let want = match max {
                    Some(max) if done as usize >= max => break,
                    Some(max) => (max - done as usize).min(CHUNK),
                    None => CHUNK,
                } as u32;
                let chunk = within(
                    STALL_TIMEOUT,
                    file.read(want, BytesMut::with_capacity(want as usize)),
                )
                .await
                .map_err(|()| timed_out(&format!("read {path}"), STALL_TIMEOUT))?
                .map_err(|e| anyhow!("cannot read {path}: {e}"))?;
                let Some(bytes) = chunk else { break };
                sink(&bytes)?;
                done += bytes.len() as u64;
                progress(done);
            }
            let _ = within(Duration::from_secs(5), file.close()).await;
            Ok(done)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The state dir (ssh control sockets) is this account's own private directory: created
    /// 0700; an existing one of ours with group/other access is made 0700 through its own
    /// descriptor; a symlink or a non-directory there is refused — and nothing is changed
    /// through it.
    #[test]
    fn the_state_dir_is_ours_and_private_or_refused() {
        use std::os::unix::fs::PermissionsExt;
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        let tmp = tempfile::tempdir().unwrap();
        let fresh = tmp.path().join("new/state");
        create_private_dir(&fresh).unwrap();
        assert_eq!(mode(&fresh), 0o700);

        let open = tmp.path().join("open");
        std::fs::create_dir(&open).unwrap();
        std::fs::set_permissions(&open, std::fs::Permissions::from_mode(0o755)).unwrap();
        create_private_dir(&open).unwrap();
        assert_eq!(mode(&open), 0o700);

        let elsewhere = tmp.path().join("elsewhere");
        std::fs::create_dir(&elsewhere).unwrap();
        std::fs::set_permissions(&elsewhere, std::fs::Permissions::from_mode(0o755)).unwrap();
        let link = tmp.path().join("link");
        std::os::unix::fs::symlink(&elsewhere, &link).unwrap();
        let err = format!("{:#}", create_private_dir(&link).unwrap_err());
        assert!(err.contains("is a symlink"), "{err}");
        assert!(err.contains("SOPEN_STATE_DIR"), "{err}");
        assert_eq!(mode(&elsewhere), 0o755, "nothing changed through the link");

        let file = tmp.path().join("file");
        std::fs::write(&file, "").unwrap();
        let err = format!("{:#}", create_private_dir(&file).unwrap_err());
        assert!(err.contains("is not a directory"), "{err}");

        // A directory swapped for a symlink between the check and the open: the open does not
        // follow it, and nothing is changed through it.
        let swapped = tmp.path().join("swapped");
        std::fs::create_dir(&swapped).unwrap();
        let err = create_private_dir_with(&swapped, &mut || {
            std::fs::remove_dir(&swapped).unwrap();
            std::os::unix::fs::symlink(&elsewhere, &swapped).unwrap();
        })
        .unwrap_err();
        let err = format!("{err:#}");
        assert!(err.contains("is a symlink"), "{err}");
        assert_eq!(mode(&elsewhere), 0o755, "nothing changed through the link");

        // A directory owned by another account is refused too ("is owned by uid …"); making
        // one needs root, so that case has no test here.
    }

    #[test]
    fn ssh_g_parsing_and_master_options() {
        let out = "user me\nhostname box\ncontrolpath /s/cm-abc\nconnecttimeout none\n\
                   serveraliveinterval 0\n";
        let config = parse_ssh_g(out).unwrap();
        assert_eq!(config.control_path, PathBuf::from("/s/cm-abc"));
        assert!(!config.connect_timeout_set);
        let opts = master_options(&config).join(" ");
        assert_eq!(
            opts,
            "-o ControlMaster=auto -o ControlPersist=60s -o ServerAliveInterval=10 \
             -o ServerAliveCountMax=3 -o ConnectTimeout=15"
        );

        let set = parse_ssh_g("controlpath /s/x\nconnecttimeout 60\n").unwrap();
        assert!(set.connect_timeout_set);
        assert!(!master_options(&set).join(" ").contains("ConnectTimeout"));
        assert!(parse_ssh_g("hostname box\n").is_err());

        // `--target` runs its command over the same master, with the same options.
        let args = connection_args_for(&config);
        assert_eq!(
            args[..2],
            [OsString::from("-S"), OsString::from("/s/cm-abc")]
        );
        assert_eq!(
            args[2..],
            master_options(&config)
                .into_iter()
                .map(OsString::from)
                .collect::<Vec<_>>()[..]
        );
    }

    #[test]
    fn timeouts_fire_and_name_the_request() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let never = std::future::pending::<()>();
        let got = rt.block_on(within(Duration::from_millis(10), never));
        assert_eq!(got, Err(()));
        let ready = rt.block_on(within(Duration::from_secs(1), async { 7 }));
        assert_eq!(ready, Ok(7));
        let e = timed_out("stat /x", META_TIMEOUT);
        assert_eq!(e.to_string(), "stat /x: no reply from the server in 30s");
        assert!(super::super::is_outcome_unknown(&unknown("rename a to b")));
    }

    fn lost() -> SftpErr {
        SftpErr::IOError(std::io::Error::new(
            std::io::ErrorKind::BrokenPipe,
            "connection closed",
        ))
    }

    /// A transport failure after the request went out proves nothing about the remote.
    #[test]
    fn f2_transport_errors_after_sending_are_outcome_unknown() {
        let e = changed("rename /a to /b", Ok(Err(lost()))).unwrap_err();
        assert!(super::super::is_outcome_unknown(&e), "{e:#}");
        assert!(e.to_string().contains("reply lost"), "{e}");
        for other in [
            SftpErr::BackgroundTaskFailure(&"read task failed"),
            SftpErr::IOError(std::io::ErrorKind::UnexpectedEof.into()),
        ] {
            assert!(!answered(&other), "{other}");
        }
        let create = failed_create("/d/.a.sopen-0", lost());
        assert_eq!(create.created, Created::Unknown);
        assert!(super::super::is_outcome_unknown(&create.error));
    }

    /// Only a status reply (or an extension never sent) proves the request did not happen.
    #[test]
    fn f2_status_replies_prove_nothing_happened() {
        let refused = SftpErr::UnsupportedExtension(&"hardlink");
        assert!(answered(&refused));
        let e = changed("link /a to /b", Ok(Err(refused))).unwrap_err();
        assert!(!super::super::is_outcome_unknown(&e), "{e:#}");
        // SSH_FX_FAILURE as the server sends it (the message type is only deserializable).
        let msg: openssh_sftp_client::error::SftpErrMsg =
            serde_json::from_str(r#"["file exists", "en"]"#).unwrap();
        let status = SftpErr::SftpError(SftpErrorKind::Failure, msg);
        assert!(answered(&status));
        let create = failed_create("/d/out.mp4", status);
        assert_eq!(create.created, Created::No);
        assert!(!super::super::is_outcome_unknown(&create.error));
        assert!(create
            .error
            .to_string()
            .starts_with("cannot create /d/out.mp4"));
    }
}
