//! SFTP's plain `SSH_FXP_RENAME` (04 §5.6): the rename of a remote folder, and the publish of a
//! copy's temp where the server has no `hardlink@openssh.com`. OpenSSH's server refuses it onto
//! an existing name for a file or a folder (E2, 02 §8.2). `openssh-sftp-client` keeps the plain
//! request private — its `Fs::rename` sends `posix-rename@openssh.com`, which replaces — so it goes
//! on a second `sftp` subsystem of the same ssh master, through `openssh-sftp-client-lowlevel`'s
//! `WriteEnd::send_rename_request` (E2's route B). A link is never sent through it: there a link
//! replaced a dangling link, so a link keeps S1's path, made again at the new name.
//!
//! [`Plain`] is that channel, on a runtime of its own; it also keeps the session's rename probe
//! verdict. A copy's publish without `hardlink@` probes first, once per session, in its own folder
//! `.sopen-probe-<16 hex>` of the target folder: two files made exclusively, the first plain-renamed
//! onto the second, both read back, then removed while they are still what was made. Only a
//! status 4 with both files intact lets the session publish by the plain rename.
//!
//! **Where the channel comes from.** `SftpRemote` has no field for it yet:
//! [`SftpRemote::with_plain`] is the one place that lends it, and in a build without tests it has
//! none — a remote folder's rename stays refused, as before S6, and a copy to a server without
//! `hardlink@` too. Tests put one on their thread ([`SftpRemote::seam_plain`]). [`Plain::over`] is
//! what `SftpRemote::connect` is to open once the struct carries the channel.

use super::{answered, within, SftpRemote, META_TIMEOUT};
use crate::remote::ops::{taken, NoRename, Renamed, REMOTE_FOLDER_RENAME};
use crate::remote::{join, random_hex, unknown, Created, RemoteFs};
use anyhow::{anyhow, Result};
use bytes::Bytes;
use openssh_sftp_client::error::{Error as SftpErr, SftpErrorKind};
use openssh_sftp_client_lowlevel as lowlevel;
use std::borrow::Cow;
use std::num::NonZeroUsize;
use std::path::Path;
use std::pin::Pin;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};
use tokio::runtime::Runtime;

/// Why a copy is refused where the probe found that the server's plain rename replaces (04 §5.6).
pub const REPLACES: &str = "this server's plain rename replaces an existing name, so sopen \
    cannot publish a copy here without the risk of writing over one";

/// The requests the low-level client has written, until they are flushed to the subsystem.
#[derive(Default)]
struct Outbox(std::sync::Mutex<Vec<Bytes>>);

impl lowlevel::Queue for Outbox {
    fn push(&self, bytes: Bytes) {
        self.0.lock().unwrap_or_else(|p| p.into_inner()).push(bytes);
    }

    fn extend(&self, header: Bytes, body: &[&[Bytes]]) {
        let mut queue = self.0.lock().unwrap_or_else(|p| p.into_inner());
        queue.push(header);
        for part in body {
            queue.extend(part.iter().cloned());
        }
    }
}

type Buffer = Vec<u8>;
type Reader = Pin<Box<dyn AsyncRead + Send>>;
type Writer = Pin<Box<dyn AsyncWrite + Send>>;
type WriteEnd = lowlevel::WriteEnd<Buffer, Outbox, ()>;
type ReadEnd = lowlevel::ReadEnd<Reader, Buffer, Outbox, ()>;

/// What the session's probe found (04 §5.6), kept for the session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Verdict {
    /// Status 4 and both files intact: the plain rename refuses an existing name.
    Refuses,
    /// An OK reply, or a file changed or gone: it replaces.
    Replaces,
}

/// What a plain rename got.
#[derive(Debug)]
pub(super) enum Said {
    Done,
    /// The server's status reply.
    Refused(SftpErr),
    /// No reply it could read (a timeout, a closed channel): the outcome is unknown, and the
    /// channel is not used again.
    NoAnswer(String),
    /// Not sent: the channel was already unusable (why). Nothing changed.
    Unusable(String),
}

/// The second `sftp` channel of a session's master, for the plain rename only.
pub(crate) struct Plain {
    stdin: Writer,
    read: ReadEnd,
    write: WriteEnd,
    /// What keeps the subsystem alive: its `ssh` child, or a test's server.
    _keep: Box<dyn std::any::Any + Send>,
    verdict: Option<Verdict>,
    /// Why the channel cannot be trusted with another request.
    broken: Option<String>,
    /// Last: the pipes above were made on it.
    rt: Runtime,
}

/// A runtime of the channel's own: its pipes are made and driven on it, and nowhere else.
fn runtime() -> Result<Runtime> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| anyhow!("cannot start the async runtime: {e}"))
}

/// Writes what the client queued to the subsystem.
async fn flush(write: &WriteEnd, stdin: &mut Writer) -> std::io::Result<()> {
    let queued = std::mem::take(&mut *write.queue().0.lock().unwrap_or_else(|p| p.into_inner()));
    for bytes in queued {
        stdin.write_all(&bytes).await?;
    }
    stdin.flush().await
}

impl Plain {
    /// The client on a subsystem's pipes, made on `rt`: the version handshake.
    fn start(
        rt: Runtime,
        stdin: Writer,
        stdout: Reader,
        keep: Box<dyn std::any::Any + Send>,
    ) -> Result<Self> {
        let write = lowlevel::connect::<Buffer, _, ()>(Outbox::default(), ())
            .map_err(|e| anyhow!("cannot start the plain-rename channel: {e}"))?;
        let buffer = NonZeroUsize::new(4096).expect("not zero");
        let read = ReadEnd::new(stdout, buffer, (*write).clone());
        let mut plain = Plain {
            stdin,
            read,
            write,
            _keep: keep,
            verdict: None,
            broken: None,
            rt,
        };
        let Plain {
            rt,
            write,
            read,
            stdin,
            ..
        } = &mut plain;
        let hello = rt.block_on(within(META_TIMEOUT, async {
            flush(write, stdin).await.map_err(|e| anyhow!("{e}"))?;
            read.receive_server_hello()
                .await
                .map_err(|e| anyhow!("{e}"))
        }));
        match hello {
            Ok(Ok(_extensions)) => Ok(plain),
            Ok(Err(e)) => Err(anyhow!("cannot start the plain-rename channel: {e:#}")),
            Err(()) => Err(anyhow!(
                "cannot start the plain-rename channel: no reply in {}s",
                META_TIMEOUT.as_secs()
            )),
        }
    }

    /// A second `sftp` subsystem over `session`, the master the session's own channel runs on
    /// (E2's route B). What `ssh` says on its stderr goes with a failed start.
    #[expect(
        dead_code,
        reason = "SftpRemote::connect opens it once SftpRemote has a field for the channel"
    )]
    pub(crate) fn over(session: std::sync::Arc<openssh::Session>) -> Result<Self> {
        let rt = runtime()?;
        let spawned = rt.block_on(within(META_TIMEOUT, async {
            openssh::Session::to_subsystem(session, "sftp")
                .stdin(openssh::Stdio::piped())
                .stdout(openssh::Stdio::piped())
                .stderr(openssh::Stdio::piped())
                .spawn()
                .await
        }));
        let mut child = match spawned {
            Ok(Ok(child)) => child,
            Ok(Err(e)) => return Err(anyhow!("cannot open a second sftp channel: {e}")),
            Err(()) => {
                return Err(anyhow!(
                    "cannot open a second sftp channel: no reply in {}s",
                    META_TIMEOUT.as_secs()
                ))
            }
        };
        let listening = child.stderr().take().and_then(super::listen);
        let (Some(stdin), Some(stdout)) = (child.stdin().take(), child.stdout().take()) else {
            return Err(anyhow!("the second sftp channel has no pipes"));
        };
        Plain::start(rt, Box::pin(stdin), Box::pin(stdout), Box::new(child)).map_err(|e| {
            match &listening {
                Some(listening) => super::with_said(e, &listening.said),
                None => e,
            }
        })
    }

    /// One plain `SSH_FXP_RENAME` of `from` to `to`.
    pub(super) fn rename(&mut self, from: &str, to: &str) -> Said {
        if let Some(why) = &self.broken {
            return Said::Unusable(format!(
                "the plain-rename channel is unusable since an earlier request ({why})"
            ));
        }
        let Plain {
            rt,
            write,
            read,
            stdin,
            ..
        } = self;
        let got = rt.block_on(within(META_TIMEOUT, async {
            let id = write.create_response_id();
            let awaiting = write.send_rename_request(
                id,
                Cow::Borrowed(Path::new(from)),
                Cow::Borrowed(Path::new(to)),
            )?;
            flush(write, stdin).await?;
            read.read_in_one_packet().await?;
            awaiting.wait().await.map(|_| ())
        }));
        let why = match got {
            Ok(Ok(())) => return Said::Done,
            Ok(Err(e)) if answered(&e) => return Said::Refused(e),
            Ok(Err(e)) => format!("the request was sent, its reply lost ({e})"),
            Err(()) => format!(
                "the request was sent, no reply in {}s",
                META_TIMEOUT.as_secs()
            ),
        };
        self.broken = Some(why.clone());
        Said::NoAnswer(why)
    }
}

impl Drop for Plain {
    fn drop(&mut self) {
        // The subsystem ends when its input does.
        let Plain { rt, stdin, .. } = self;
        let _ = rt.block_on(within(std::time::Duration::from_secs(5), stdin.shutdown()));
    }
}

#[cfg(test)]
thread_local! {
    /// The session's plain-rename channel, while a test lends one to this thread.
    static PLAIN: std::cell::RefCell<Option<Plain>> = const { std::cell::RefCell::new(None) };
}

/// The channel, out of where it is kept, for one use.
fn take_plain() -> Option<Plain> {
    #[cfg(test)]
    return PLAIN.with(|p| p.borrow_mut().take());
    #[cfg(not(test))]
    None
}

/// The channel back where it is kept.
fn put_plain(plain: Plain) {
    #[cfg(test)]
    PLAIN.with(|p| *p.borrow_mut() = Some(plain));
    #[cfg(not(test))]
    drop(plain);
}

/// Why a copy to a server without `hardlink@` is refused where the session has no plain-rename
/// channel.
pub const NO_PUBLISH: &str = "this server does not offer hardlink@openssh.com, and this \
    connection has no channel for SFTP's plain rename, so sopen cannot put a copy under its name \
    without the risk of replacing anything there";

impl SftpRemote {
    /// The session's plain-rename channel, lent to `f` with the session itself: `None` where the
    /// session has none (see the module's doc).
    fn with_plain<R>(&mut self, f: impl FnOnce(&mut Self, &mut Plain) -> R) -> Option<R> {
        let mut plain = take_plain()?;
        let got = f(self, &mut plain);
        put_plain(plain);
        Some(got)
    }

    /// Whether the session has a plain-rename channel.
    pub(super) fn has_plain(&self) -> bool {
        #[cfg(test)]
        return PLAIN.with(|p| p.borrow().is_some());
        #[cfg(not(test))]
        false
    }

    /// A folder renamed by one plain `SSH_FXP_RENAME` (04 §5.6, the folder row), never replacing:
    /// [`taken`] only when `lstat` then finds the new name (status 4 is the generic failure);
    /// [`NoRename`] where the session has no channel, or the two names may be on two
    /// filesystems (a move then copies, checks and removes).
    pub(super) fn rename_folder(&mut self, from: &str, to: &str) -> Result<Renamed> {
        let Some(said) = self.with_plain(|_, plain| plain.rename(from, to)) else {
            return Err(anyhow!(NoRename(REMOTE_FOLDER_RENAME.to_string())));
        };
        let e = match said {
            Said::Done => return Ok(Renamed::Moved),
            Said::NoAnswer(why) => {
                return Err(unknown(format!(
                    "rename {from} to {to}: {why} — it may or may not have happened"
                )))
            }
            Said::Unusable(why) => {
                return Err(anyhow!(
                    "cannot rename {from} to {to}: {why}; nothing was changed"
                ))
            }
            Said::Refused(e) => e,
        };
        if matches!(self.lstat_opt(to), Ok(Some(_))) {
            return Err(taken(to));
        }
        if matches!(e, SftpErr::SftpError(SftpErrorKind::PermDenied, _))
            || self.same_filesystem(from, to) == Some(true)
        {
            return Err(anyhow!(
                "cannot rename {from} to {to}: {e}; nothing was changed"
            ));
        }
        Err(anyhow!(NoRename(format!(
            "cannot rename {from} to {to} ({e}): it may be another filesystem"
        ))))
    }

    /// A staged copy published by the plain rename (04 §5.6, the copy row without `hardlink@`):
    /// only once this session's probe found that the server refuses an existing name; [`taken`]
    /// when the refusal finds the name there.
    pub(super) fn publish_plain(&mut self, temp: &str, target: &str) -> Result<Renamed> {
        let got = self.with_plain(|_, plain| match plain.verdict {
            Some(Verdict::Refuses) => Ok(plain.rename(temp, target)),
            Some(Verdict::Replaces) => Err(anyhow!(REPLACES)),
            None => Err(anyhow!(
                "the server's rename was not probed on this connection, so {temp} was not \
                 published"
            )),
        });
        let said = got.unwrap_or_else(|| Err(anyhow!(NO_PUBLISH)))?;
        match said {
            Said::Done => Ok(Renamed::Moved),
            Said::Refused(e) => match self.lstat_opt(target) {
                Ok(Some(_)) => Err(taken(target)),
                _ => Err(anyhow!("cannot rename {temp} to {target}: {e}")),
            },
            Said::NoAnswer(why) => Err(unknown(format!(
                "rename {temp} to {target}: {why} — it may or may not have happened"
            ))),
            Said::Unusable(why) => Err(anyhow!(
                "cannot rename {temp} to {target}: {why}; it was not published"
            )),
        }
    }

    /// Before the first copy of this session is staged in `dir` (04 §5.6): the rename probe,
    /// once per session. `Ok` (with an entry the probe left, named) when the plain rename
    /// refuses an existing name; `Err` when it replaces (kept for the session) or when the probe
    /// could not run (not kept: the next copy probes again).
    pub(super) fn probe(&mut self, dir: &str) -> Result<Vec<String>> {
        let got = self.with_plain(|session, plain| match plain.verdict {
            Some(Verdict::Refuses) => Ok(Vec::new()),
            Some(Verdict::Replaces) => Err(anyhow!(REPLACES)),
            None => {
                let (found, left) = session.run_probe(plain, dir);
                let left_said = || match left.is_empty() {
                    true => String::new(),
                    false => format!("; {}", left.join("; ")),
                };
                match found {
                    Ok(verdict) => {
                        plain.verdict = Some(verdict);
                        match verdict {
                            Verdict::Refuses => Ok(left),
                            Verdict::Replaces => Err(anyhow!("{REPLACES}{}", left_said())),
                        }
                    }
                    Err(why) => Err(anyhow!(
                        "sopen's rename probe could not run in {dir} ({why}): without \
                         hardlink@openssh.com a copy is published by the server's plain rename \
                         only once the probe has found that it refuses an existing name{}",
                        left_said()
                    )),
                }
            }
        });
        got.unwrap_or_else(|| Err(anyhow!(NO_PUBLISH)))
    }

    /// The probe itself: what it found (`Err`: it could not run), and every entry of its own it
    /// could not remove, named.
    fn run_probe(
        &mut self,
        plain: &mut Plain,
        dir: &str,
    ) -> (std::result::Result<Verdict, String>, Vec<String>) {
        let folder = join(dir, &format!(".sopen-probe-{}", random_hex()));
        // `MKDIR` with the attributes 0700: only this account can put anything in it, so the
        // cleanup's check of each file by its type and size is enough. A server that made it
        // writable by others (one that ignores the attributes) gets no probe.
        if let Err(e) = crate::remote::ops::FileOps::make_dir(self, &folder) {
            return (Err(format!("{e:#}")), Vec::new());
        }
        match self.lstat_opt(&folder) {
            Ok(Some(st)) if st.is_dir && !st.is_symlink && st.mode & 0o022 == 0 => {}
            looked => {
                let why = match looked {
                    Err(e) => format!("{e:#}"),
                    Ok(_) => format!(
                        "its folder {folder} was not made private (0700), so other accounts \
                         could put files in it"
                    ),
                };
                return (Err(why), self.probe_cleanup(&folder, &[]));
            }
        }
        let tag = random_hex();
        let files = [
            (
                join(&folder, "a"),
                format!("sopen rename probe, first: {tag}\n"),
            ),
            (
                join(&folder, "b"),
                format!("sopen rename probe, the second: {tag}\n"),
            ),
        ];
        // Every file the probe may have made, from the moment its create may have happened —
        // whatever came of its write and its close.
        let mut made = Vec::new();
        let mut found = Ok(());
        for (path, bytes) in &files {
            if let Err(e) = self.probe_file(path, bytes.as_bytes(), &mut made) {
                found = Err(format!("{e:#}"));
                break;
            }
        }
        let found = found.and_then(|()| {
            let e = match plain.rename(&files[0].0, &files[1].0) {
                // It said it renamed one file onto the other: it replaces.
                Said::Done => return Ok(Verdict::Replaces),
                Said::NoAnswer(why) | Said::Unusable(why) => return Err(why),
                Said::Refused(e) => e,
            };
            // Refused: is each file still what was made? One changed or gone was replaced.
            let mut intact = true;
            for (path, bytes) in &files {
                match self.probe_read(path) {
                    Ok(Some(now)) => intact &= now == bytes.as_bytes(),
                    Ok(None) => intact = false,
                    Err(e) => return Err(format!("its read-back failed: {e:#}")),
                }
            }
            match e {
                _ if !intact => Ok(Verdict::Replaces),
                SftpErr::SftpError(SftpErrorKind::Failure, _) => Ok(Verdict::Refuses),
                e => Err(format!("the server refused its rename otherwise: {e}")),
            }
        });
        let left = self.probe_cleanup(&folder, &made);
        (found, left)
    }

    /// One of the probe's files: created exclusively (`CREAT|EXCL`), `bytes` written, closed.
    /// It goes into `made` as soon as its create may have happened (made, or no answer), apart
    /// from whether its write and its close then succeed.
    fn probe_file(&mut self, path: &str, bytes: &[u8], made: &mut Vec<Made>) -> Result<()> {
        let (rt, sftp, _) = self.parts();
        rt.block_on(async {
            let mut file = match super::ops::create_temp(sftp, path).await {
                Ok(file) => file,
                Err(e) => {
                    if e.created != Created::No {
                        made.push(Made::new(path, bytes));
                    }
                    return Err(e.error);
                }
            };
            made.push(Made::new(path, bytes));
            let wrote = within(META_TIMEOUT, file.write_all(bytes)).await;
            if matches!(wrote, Ok(Ok(_))) {
                if let Some(last) = made.last_mut() {
                    last.written = true;
                }
            }
            let closed = within(META_TIMEOUT, file.close()).await;
            match (wrote, closed) {
                (Ok(Ok(_)), Ok(Ok(()))) => Ok(()),
                (Ok(Err(e)), _) | (_, Ok(Err(e))) => Err(anyhow!("cannot write {path}: {e}")),
                (Err(()), _) | (_, Err(())) => Err(anyhow!(
                    "writing {path} got no reply in {}s",
                    META_TIMEOUT.as_secs()
                )),
            }
        })
    }

    /// What one of the probe's files holds now; `None` when it is gone or is no regular file.
    fn probe_read(&mut self, path: &str) -> Result<Option<Vec<u8>>> {
        match self.lstat_opt(path)? {
            Some(st) if st.is_file && !st.is_symlink => Ok(Some(self.read_head(path, 4096)?)),
            _ => Ok(None),
        }
    }

    /// The probe's files removed — each only while it is still the one made: a regular file of
    /// the size written (or, where the write did not finish, no larger) — then its folder; what
    /// stays is named, safe to remove (04 §5.1). A file gone is nothing left.
    fn probe_cleanup(&mut self, folder: &str, made: &[Made]) -> Vec<String> {
        let mut left = Vec::new();
        for Made {
            path,
            size,
            written,
        } in made
        {
            let still = match self.lstat_opt(path) {
                Ok(None) => continue,
                Ok(Some(st)) => {
                    st.is_file
                        && !st.is_symlink
                        && (st.size == *size || (!written && st.size < *size))
                }
                Err(_) => false,
            };
            if !still {
                left.push(format!(
                    "sopen's rename probe left {path} (it is not the file it made any more)"
                ));
                continue;
            }
            if let Err(e) = self.remove(path) {
                left.push(format!(
                    "sopen's rename probe left {path} ({e:#}); removing it is safe"
                ));
            }
        }
        let folder_still =
            matches!(self.lstat_opt(folder), Ok(Some(st)) if st.is_dir && !st.is_symlink);
        if !folder_still {
            left.push(format!(
                "sopen's rename probe folder {folder} is not the folder it made any more; it \
                 was left"
            ));
        } else if let Err(e) = crate::remote::ops::FileOps::remove_dir(self, folder) {
            left.push(format!(
                "sopen's rename probe left its folder {folder} ({e:#}); removing it is safe"
            ));
        }
        left
    }
}

/// A file the probe may have made (its ledger).
struct Made {
    path: String,
    /// The bytes it is to hold.
    size: u64,
    /// They were all written.
    written: bool,
}

impl Made {
    fn new(path: &str, bytes: &[u8]) -> Self {
        Self {
            path: path.to_string(),
            size: bytes.len() as u64,
            written: false,
        }
    }
}

/// The tests' plain-rename channel: to a new local `sftp-server` over pipes, as
/// `SftpRemote::local_server_run` starts the session's own.
/// What the tests' tap does to the plain channel's renames.
#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tap {
    /// No tap: the server itself.
    Off,
    /// The server's reply to a rename becomes "done" (status 0), whatever it was.
    AnswerDone,
    /// A rename goes to the server as `posix-rename@openssh.com`, which replaces: the server
    /// really writes over the name, as one whose plain rename replaces would.
    Replace,
}

#[cfg(test)]
impl Plain {
    fn local_server(args: &[&str], log: Option<&Path>, tap: Tap) -> Option<Self> {
        let hidden = std::env::var_os("SOPEN_TEST_HIDE_SFTP_SERVER").is_some();
        let found = super::SFTP_SERVERS
            .iter()
            .find(|p| !hidden && Path::new(p).exists());
        let Some(server) = found else {
            let missing = format!("no sftp-server on this machine ({:?})", super::SFTP_SERVERS);
            if std::env::var_os("SOPEN_REQUIRE_SFTP_SERVER").is_some() {
                panic!("SOPEN_REQUIRE_SFTP_SERVER is set but {missing}");
            }
            eprintln!("skipping: {missing}");
            return None;
        };
        let rt = runtime().unwrap();
        let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
        let log = log.map(Path::to_path_buf);
        let server = server.to_string();
        let spawn = move || {
            let mut cmd = tokio::process::Command::new(&server);
            cmd.stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .kill_on_drop(true);
            if let Some(log) = &log {
                cmd.args(["-e", "-l", "INFO"])
                    .stderr(std::process::Stdio::from(
                        std::fs::File::create(log).unwrap(),
                    ));
            }
            cmd.args(&args);
            cmd.spawn().unwrap()
        };
        if tap == Tap::Off {
            let mut child = {
                let _on = rt.enter();
                spawn()
            };
            let (stdin, stdout) = (child.stdin.take().unwrap(), child.stdout.take().unwrap());
            return Some(
                Plain::start(rt, Box::pin(stdin), Box::pin(stdout), Box::new(child)).unwrap(),
            );
        }
        // The tap and the server on a thread and runtime of their own, as a remote is.
        let (to_tap, from_sopen) = tokio::io::duplex(1 << 16);
        let (to_sopen, from_tap) = tokio::io::duplex(1 << 16);
        let thread = std::thread::spawn(move || {
            let rt = runtime().unwrap();
            rt.block_on(async move {
                let mut child = spawn();
                let (stdin, stdout) = (child.stdin.take().unwrap(), child.stdout.take().unwrap());
                let renames = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
                let requests = tokio::spawn(tap_requests(from_sopen, stdin, renames.clone(), tap));
                tap_replies(stdout, to_sopen, renames, tap).await;
                let _ = requests.await;
                drop(child);
            })
        });
        let (stdin, stdout) = {
            let _on = rt.enter();
            (to_tap, from_tap)
        };
        Some(
            Plain::start(
                rt,
                Box::pin(stdin),
                Box::pin(stdout),
                Box::new(Tapped(Some(thread))),
            )
            .unwrap(),
        )
    }
}

/// The tap's thread, ended with the channel (its input closes first).
#[cfg(test)]
struct Tapped(Option<std::thread::JoinHandle<()>>);

#[cfg(test)]
impl Drop for Tapped {
    fn drop(&mut self) {
        if let Some(thread) = self.0.take() {
            let _ = thread.join();
        }
    }
}

/// `SSH_FXP_RENAME`.
#[cfg(test)]
const RENAME: u8 = 18;

/// Requests to the server: the ids of the renames noted; with [`Tap::Replace`] each rename sent
/// on as `posix-rename@openssh.com` (`SSH_FXP_EXTENDED`: type, id, the extension's name, then
/// the rename's two paths unchanged).
#[cfg(test)]
async fn tap_requests<R, W>(
    mut from: R,
    mut to: W,
    renames: std::sync::Arc<std::sync::Mutex<Vec<u32>>>,
    tap: Tap,
) where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    const EXTENDED: u8 = 200;
    while let Some(mut body) = super::packet(&mut from).await {
        if body.first() == Some(&RENAME) {
            renames.lock().unwrap().push(super::be32(&body[1..]));
            if tap == Tap::Replace {
                let ext = b"posix-rename@openssh.com";
                let mut replacing = vec![EXTENDED];
                replacing.extend(&body[1..5]);
                replacing.extend((ext.len() as u32).to_be_bytes());
                replacing.extend(ext);
                replacing.extend(&body[5..]);
                body = replacing;
            }
        }
        if super::send_packet(&mut to, &body).await.is_none() {
            return;
        }
    }
}

/// Replies to sopen: with [`Tap::AnswerDone`] the reply to a rename is "done" (status 0),
/// whatever the server said — a server whose plain rename replaces would answer so.
#[cfg(test)]
async fn tap_replies<R, W>(
    mut from: R,
    mut to: W,
    renames: std::sync::Arc<std::sync::Mutex<Vec<u32>>>,
    tap: Tap,
) where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    const VERSION: u8 = 2;
    const STATUS: u8 = 101;
    while let Some(mut body) = super::packet(&mut from).await {
        if body[0] != VERSION && tap == Tap::AnswerDone {
            let id = super::be32(&body[1..]);
            let mut renames = renames.lock().unwrap();
            if let Some(at) = renames.iter().position(|r| *r == id) {
                renames.remove(at);
                // STATUS: type, id, code 0 (SSH_FX_OK), message, language.
                body = vec![STATUS];
                body.extend(id.to_be_bytes());
                body.extend(0u32.to_be_bytes());
                body.extend(0u32.to_be_bytes());
                body.extend(0u32.to_be_bytes());
            }
        }
        if super::send_packet(&mut to, &body).await.is_none() {
            return;
        }
    }
}

#[cfg(test)]
impl SftpRemote {
    /// Test seam on this thread: until the guard drops, this thread's sessions have a
    /// plain-rename channel — to a new local `sftp-server` with `args`, its request log
    /// (`-e -l INFO`) at `log`; with `answer_ok`, behind a tap that answers every
    /// `SSH_FXP_RENAME` as done (status 0) whatever the server said, as a server whose plain
    /// rename replaces would. `None` where there is no `sftp-server` (as `local_server`).
    pub(crate) fn seam_plain(
        args: &[&str],
        log: Option<&Path>,
        answer_ok: bool,
    ) -> Option<crate::remote::ops::SeamGuard> {
        let tap = if answer_ok { Tap::AnswerDone } else { Tap::Off };
        Self::seam_plain_with(Plain::local_server(args, log, tap)?)
    }

    /// [`SftpRemote::seam_plain`] behind a tap that sends every rename on as
    /// `posix-rename@openssh.com`: the server really replaces the name, as one whose plain
    /// rename replaces would.
    pub(crate) fn seam_plain_replacing(
        args: &[&str],
        log: Option<&Path>,
    ) -> Option<crate::remote::ops::SeamGuard> {
        Self::seam_plain_with(Plain::local_server(args, log, Tap::Replace)?)
    }

    fn seam_plain_with(plain: Plain) -> Option<crate::remote::ops::SeamGuard> {
        PLAIN.with(|p| *p.borrow_mut() = Some(plain));
        Some(crate::remote::ops::SeamGuard::new(|| {
            let plain = PLAIN.with(|p| p.borrow_mut().take());
            drop(plain);
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The plain rename on its own channel against OpenSSH's `sftp-server` (E2's route B): a file
    /// and a folder onto free names are renamed; onto an existing name — a file, a folder — the
    /// server answers status 4 and both are left.
    #[test]
    fn the_plain_rename_renames_onto_a_free_name_and_refuses_a_taken_one() {
        let Some(mut plain) = Plain::local_server(&[], None, Tap::Off) else {
            return;
        };
        let tmp = tempfile::tempdir().unwrap();
        let at = |n: &str| tmp.path().join(n).to_str().unwrap().to_string();
        std::fs::write(at("f"), "f").unwrap();
        std::fs::write(at("g"), "g").unwrap();
        std::fs::create_dir(at("d")).unwrap();
        std::fs::create_dir(at("e")).unwrap();
        assert!(matches!(plain.rename(&at("f"), &at("f2")), Said::Done));
        assert!(matches!(plain.rename(&at("d"), &at("d2")), Said::Done));
        for (from, to) in [("f2", "g"), ("d2", "e"), ("d2", "g")] {
            let said = plain.rename(&at(from), &at(to));
            assert!(
                matches!(
                    said,
                    Said::Refused(SftpErr::SftpError(SftpErrorKind::Failure, _))
                ),
                "{from} onto {to}: {said:?}"
            );
        }
        assert_eq!(std::fs::read(at("f2")).unwrap(), b"f");
        assert_eq!(std::fs::read(at("g")).unwrap(), b"g");
        assert!(Path::new(&at("d2")).is_dir() && Path::new(&at("e")).is_dir());
    }

    /// The probe on OpenSSH's server finds that its plain rename refuses, and leaves nothing;
    /// behind the tap that answers a rename as done it finds that it replaces.
    #[test]
    fn the_probe_tells_a_refusing_server_from_a_replacing_one() {
        for (tap, want) in [
            (Tap::Off, Verdict::Refuses),
            (Tap::AnswerDone, Verdict::Replaces),
        ] {
            let Some(mut session) = SftpRemote::local_server() else {
                return;
            };
            let Some(mut plain) = Plain::local_server(&[], None, tap) else {
                return;
            };
            let tmp = tempfile::tempdir().unwrap();
            let dir = tmp.path().to_str().unwrap();
            let (found, left) = session.run_probe(&mut plain, dir);
            assert_eq!(found, Ok(want), "{tap:?}");
            assert_eq!(left, Vec::<String>::new());
            assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 0);
        }
    }

    /// A server whose rename really replaces (the tap sends it as `posix-rename@openssh.com`):
    /// the first file is gone and the second holds its bytes. The probe finds that it replaces;
    /// its cleanup removes nothing that is not as it made it — the second file now holds the
    /// first's bytes, so it stays, and so does the folder holding it — and names both.
    #[test]
    fn the_probe_finds_a_server_that_really_replaces_and_names_what_it_left() {
        let Some(mut session) = SftpRemote::local_server() else {
            return;
        };
        let Some(mut plain) = Plain::local_server(&[], None, Tap::Replace) else {
            return;
        };
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_str().unwrap();
        let (found, left) = session.run_probe(&mut plain, dir);
        assert_eq!(found, Ok(Verdict::Replaces));
        let folders: Vec<_> = std::fs::read_dir(tmp.path())
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        let [folder] = &folders[..] else {
            panic!("{folders:?}")
        };
        let inside: Vec<_> = std::fs::read_dir(folder)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        assert_eq!(inside, ["b"]);
        let b = std::fs::read_to_string(folder.join("b")).unwrap();
        assert!(b.starts_with("sopen rename probe, first: "), "{b}");
        assert_eq!(left.len(), 2, "{left:?}");
        assert!(
            left[0].contains("/b (it is not the file it made"),
            "{left:?}"
        );
        assert!(left[1].contains("left its folder"), "{left:?}");
    }

    /// A probe file whose CLOSE fails after its bytes were written stays in the probe's ledger:
    /// the probe could not run, and its cleanup removes that file and the folder all the same —
    /// nothing is left.
    #[test]
    fn a_probe_file_whose_close_fails_is_still_cleaned_up() {
        let proxy = super::super::Proxy::default();
        proxy.rules.lock().unwrap().fail = Some(super::super::Wired::CLOSE);
        let Some(mut session) = SftpRemote::local_server_with(Some(proxy)) else {
            return;
        };
        let Some(mut plain) = Plain::local_server(&[], None, Tap::Off) else {
            return;
        };
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_str().unwrap();
        let (found, left) = session.run_probe(&mut plain, dir);
        assert!(
            matches!(&found, Err(why) if why.contains("failed by the test proxy")),
            "{found:?}"
        );
        assert_eq!(left, Vec::<String>::new());
        assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 0);
    }
}
