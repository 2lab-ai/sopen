//! [`PatchFs`] over SFTP (07 §4): files by handle on the session a Save runs on (the viewer's
//! `Origin`, 06 §6). Every request is one `block_on` of the session's runtime and has a timeout;
//! one that changes the remote and gets no reply is an
//! [`OutcomeUnknown`](crate::remote::OutcomeUnknown).
//!
//! - A read or a write carries its own offset, through a clone of the handle: the crate's
//!   cursor moves by the length asked, not by what came back (06 §2).
//! - The server copy is `ops.rs`'s `copy-data` loop, paced by time. Each copy request has
//!   [`META_TIMEOUT`], not the transfer's stall bound: a request sized to a second that takes
//!   thirty is a server not to wait on (07 §4.2 step 5).

use super::ops::{copy_data, create_temp};
use super::{
    answered, failed_change, perms, poll_replies, read_at, stat_of, timed_out, within, SftpRemote,
    META_TIMEOUT, STALL_TIMEOUT,
};
use crate::remote::patch::{CopyCtl, Each, Handle, NoFsync, OpenFor, PatchFs};
use crate::remote::ranged::Pipeline;
use crate::remote::{unknown, CreateError, Stat};
use anyhow::{anyhow, Context, Result};
use bytes::BytesMut;
use openssh_sftp_client::error::Error as SftpErr;
use openssh_sftp_client::file::File;
use openssh_sftp_client::metadata::MetaDataBuilder;
use std::pin::Pin;
use std::time::Duration;

/// The result of a request that changes the remote, sent with `limit` to be answered.
fn sent(
    what: &str,
    limit: Duration,
    got: std::result::Result<std::result::Result<(), SftpErr>, ()>,
) -> Result<()> {
    match got {
        Ok(r) => r.map_err(|e| failed_change(what, e)),
        Err(()) => Err(unknown(format!(
            "{what}: request sent, no reply in {}s — it may or may not have happened",
            limit.as_secs()
        ))),
    }
}

/// Writes `bytes` at `offset` through a clone of the handle, split at the server's write limit.
async fn write_at(
    file: &File,
    offset: u64,
    bytes: &[u8],
) -> std::result::Result<std::result::Result<(), SftpErr>, ()> {
    use tokio::io::AsyncSeek;
    let mut file = file.clone();
    if let Err(e) = Pin::new(&mut file).start_seek(std::io::SeekFrom::Start(offset)) {
        return Ok(Err(e.into()));
    }
    within(STALL_TIMEOUT, file.write_all(bytes)).await
}

impl PatchFs for SftpRemote {
    fn can_copy(&self) -> bool {
        self.sftp.as_ref().is_some_and(|s| s.support_copy())
    }

    fn open(&mut self, path: &str, how: OpenFor) -> Result<Handle> {
        let (rt, sftp, _) = self.parts();
        // Reading always: the library sends `fstat` only on a readable handle.
        let mut options = sftp.options();
        options.read(true).write(how == OpenFor::Patch);
        let file = rt
            .block_on(within(META_TIMEOUT, options.open(path)))
            .map_err(|()| timed_out(&format!("open {path}"), META_TIMEOUT))?
            .map_err(|e| anyhow!("cannot open {path}: {e}"))?;
        Ok(Handle::new(path, file))
    }

    fn create_temp(&mut self, path: &str) -> std::result::Result<Handle, CreateError> {
        let (rt, sftp, _) = self.parts();
        let file = rt.block_on(create_temp(sftp, path))?;
        Ok(Handle::new(path, file))
    }

    fn fstat(&mut self, file: &mut Handle) -> Result<Stat> {
        let (rt, _, _) = self.parts();
        let path = file.path().to_string();
        let file = file.get::<File>()?;
        let meta = rt
            .block_on(within(META_TIMEOUT, file.metadata()))
            .map_err(|()| timed_out(&format!("stat {path}"), META_TIMEOUT))?
            .map_err(|e| anyhow!("cannot stat {path}: {e}"))?;
        Ok(stat_of(&meta))
    }

    fn read_at(&mut self, file: &mut Handle, offset: u64, len: usize) -> Result<Vec<u8>> {
        let (rt, _, _) = self.parts();
        let path = file.path().to_string();
        let file = file.get::<File>()?;
        let mut out = Vec::with_capacity(len);
        // A short reply is followed by a read of the rest, at its own offset.
        while out.len() < len {
            let want = (len - out.len()).min(u32::MAX as usize) as u32;
            let reply = read_at(file, offset + out.len() as u64, want);
            let got = rt
                .block_on(within(STALL_TIMEOUT, reply))
                .map_err(|()| timed_out(&format!("read {path}"), STALL_TIMEOUT))?
                .map_err(|e| anyhow!("cannot read {path}: {e}"))?;
            match got {
                Some(bytes) if !bytes.is_empty() => out.extend_from_slice(&bytes),
                _ => break,
            }
        }
        Ok(out)
    }

    fn read_through(
        &mut self,
        src: &mut Handle,
        size: u64,
        dst: Option<&mut Handle>,
        each: &mut Each<'_>,
    ) -> Result<u64> {
        let (rt, _, _) = self.parts();
        let path = src.path().to_string();
        let file = src.get::<File>()?;
        let (to, out) = match dst {
            Some(dst) => (dst.path().to_string(), Some(&*dst.get::<File>()?)),
            None => (String::new(), None),
        };
        rt.block_on(async {
            // `read_with`'s pipeline, on the open handle: explicit offsets, in-order delivery,
            // up to 4 MiB asked ahead, clipped to `size`.
            let mut flight = Vec::new();
            let mut plan = Pipeline::new(Some(size));
            let mut done = 0u64;
            loop {
                while let Some((off, len)) = plan.send() {
                    flight.push((off, read_at(file, off, len)));
                }
                if plan.done() {
                    return Ok(done);
                }
                let replies = within(
                    STALL_TIMEOUT,
                    std::future::poll_fn(|cx| poll_replies(&mut flight, cx)),
                )
                .await
                .map_err(|()| timed_out(&format!("read {path}"), STALL_TIMEOUT))?;
                for (off, reply) in replies {
                    let got = reply.map_err(|e| anyhow!("cannot read {path}: {e}"))?;
                    plan.reply(off, got.map(BytesMut::freeze))
                        .with_context(|| format!("cannot read {path}"))?;
                }
                while let Some(bytes) = plan.take() {
                    if let (Some(write), Some(out)) = (each(done, &bytes)?, out) {
                        sent(
                            &format!("write {to}"),
                            STALL_TIMEOUT,
                            write_at(out, done, &write).await,
                        )?;
                    }
                    done += bytes.len() as u64;
                }
            }
        })
    }

    fn write_at(&mut self, file: &mut Handle, offset: u64, bytes: &[u8]) -> Result<()> {
        let (rt, _, _) = self.parts();
        let path = file.path().to_string();
        let file = file.get::<File>()?;
        let got = rt.block_on(write_at(file, offset, bytes));
        sent(&format!("write {path}"), STALL_TIMEOUT, got)
    }

    fn set_owner(&mut self, file: &mut Handle, uid: u32, gid: u32) -> Result<()> {
        let (rt, _, _) = self.parts();
        let path = file.path().to_string();
        let file = file.get::<File>()?;
        let owner = MetaDataBuilder::new().id((uid, gid)).create();
        let got = rt.block_on(within(META_TIMEOUT, file.set_metadata(owner)));
        sent(&format!("set owner on {path}"), META_TIMEOUT, got)
    }

    fn set_handle_mode(&mut self, file: &mut Handle, mode: u32) -> Result<()> {
        let (rt, _, _) = self.parts();
        let path = file.path().to_string();
        let file = file.get::<File>()?;
        let got = rt.block_on(within(META_TIMEOUT, file.set_permissions(perms(mode))));
        sent(&format!("set mode on {path}"), META_TIMEOUT, got)
    }

    fn can_fsync(&self) -> bool {
        self.sftp.as_ref().is_some_and(|s| s.support_fsync())
    }

    fn fsync(&mut self, file: &mut Handle) -> Result<()> {
        let (rt, sftp, _) = self.parts();
        if !sftp.support_fsync() {
            // Nothing is sent, and nothing is claimed: the Save says so in its result.
            return Err(anyhow!(NoFsync));
        }
        let path = file.path().to_string();
        let file = file.get::<File>()?;
        let got = rt.block_on(within(STALL_TIMEOUT, file.sync_all()));
        sent(&format!("fsync {path}"), STALL_TIMEOUT, got)
    }

    fn copy(
        &mut self,
        src: &mut Handle,
        dst: &mut Handle,
        size: u64,
        ctl: &mut CopyCtl<'_>,
    ) -> Result<()> {
        let (rt, sftp, _) = self.parts();
        let to = dst.path().to_string();
        let (src, dst) = (src.get::<File>()?, dst.get::<File>()?);
        let fsync = sftp.support_fsync();
        let copied = rt.block_on(copy_data(src, dst, size, &to, META_TIMEOUT, fsync, ctl))?;
        if !copied {
            return Err(anyhow!(
                "this server offers copy-data but refused to copy into {to}"
            ));
        }
        Ok(())
    }

    fn close(&mut self, file: Handle) {
        let (rt, _, _) = self.parts();
        let path = file.path().to_string();
        if let Some(file) = file.into_inner::<File>() {
            match rt.block_on(within(Duration::from_secs(5), file.close())) {
                Ok(Ok(())) => {}
                Ok(Err(e)) if answered(&e) => crate::debug!("close {path}: {e}"),
                _ => crate::debug!("close {path}: no reply"),
            }
        }
    }
}
