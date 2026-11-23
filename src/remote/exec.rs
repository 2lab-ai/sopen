//! A command over sopen's ssh master for a host (03 §3.2): `exec_args(dest, state_dir)`, the
//! master's `-S <ControlPath>` with `-o ControlMaster=no -o BatchMode=yes` and the keepalive
//! options of [`super::connection_args`], so the command attaches to the running master, never
//! becomes one and never prompts; the whole `ssh` argument list of a command
//! ([`command_args`]) and of the master's check ([`check_args`]).
//!
//! The options come from [`super::connection_args`] (sopen's one list of them, read from
//! `ssh -G` through `sftp.rs`'s `ssh_config`) with the master's own two taken out: ssh keeps the
//! first value it is given for an option, so `-o ControlMaster=no` after its
//! `-o ControlMaster=auto` would be ignored.

use anyhow::Result;
use std::ffi::{OsStr, OsString};
use std::path::Path;

/// Options for the command run only, as `--target` passes them (`forward.rs`): a `RemoteCommand`
/// in `~/.ssh/config` would make ssh refuse a command line, and ssh's own notices stay off the
/// command's stderr (errors still print).
const RUN_OPTIONS: [&str; 4] = ["-o", "RemoteCommand=none", "-o", "LogLevel=ERROR"];

/// What a command adds in place of the master's options: it attaches to the running master and
/// never becomes one, and never asks for anything.
const ATTACH: [&str; 4] = ["-o", "ControlMaster=no", "-o", "BatchMode=yes"];

/// Options only a master has, or that [`ATTACH`] sets: left out of a command's arguments.
const MASTER_ONLY: [&str; 3] = ["ControlMaster", "ControlPersist", "BatchMode"];

/// `-S <ControlPath> -o ControlMaster=no -o BatchMode=yes` and the rest of
/// [`super::connection_args`] for `dest` (the keepalives, and `ConnectTimeout` unless the
/// user's config sets one). Runs `ssh -G` once.
pub fn exec_args(dest: &str, state_dir: &Path) -> Result<Vec<OsString>> {
    Ok(exec_args_from(super::connection_args(dest, state_dir)?))
}

/// [`exec_args`] from [`super::connection_args`]'s list: `-S <path>` first, then
/// `-o ControlMaster=no -o BatchMode=yes`, then every other option in its order, without the
/// master's own (`ControlMaster`, `ControlPersist`) or a `BatchMode`.
pub fn exec_args_from(connection: Vec<OsString>) -> Vec<OsString> {
    let mut socket = Vec::new();
    let mut rest = Vec::new();
    let mut words = connection.into_iter();
    while let Some(word) = words.next() {
        match word.to_str() {
            Some("-S") => {
                socket.push(word);
                socket.extend(words.next());
            }
            Some("-o") => {
                let Some(value) = words.next() else {
                    rest.push(word);
                    continue;
                };
                let key = value.to_string_lossy();
                let key = key.split('=').next().unwrap_or_default();
                if !MASTER_ONLY.iter().any(|k| k.eq_ignore_ascii_case(key)) {
                    rest.extend([word, value]);
                }
            }
            _ => rest.push(word),
        }
    }
    socket
        .into_iter()
        .chain(ATTACH.iter().map(OsString::from))
        .chain(rest)
        .collect()
}

/// `ssh <exec args> -T -o RemoteCommand=none -o LogLevel=ERROR <dest> -- <words>` (03 §3.2):
/// the arguments after `ssh`. ssh joins `words` with spaces into the one line the remote's login
/// shell reads.
pub fn command_args(exec: &[OsString], dest: &str, words: Vec<OsString>) -> Vec<OsString> {
    let mut args = exec.to_vec();
    args.push("-T".into());
    args.extend(RUN_OPTIONS.iter().map(OsString::from));
    args.push(dest.into());
    args.push("--".into());
    args.extend(words);
    args
}

/// `ssh -S <ControlPath> -O check <dest>` (03 §3.2): the arguments after `ssh` of the check
/// that the master is there, the `-S` pair taken from `exec` (an [`exec_args`] list). Exit 0: it
/// is; 255 and `Control socket connect(…)` when there is none, at once, without the network.
pub fn check_args(exec: &[OsString], dest: &str) -> Vec<OsString> {
    let socket = exec
        .iter()
        .position(|w| w == OsStr::new("-S"))
        .and_then(|i| exec.get(i..i + 2))
        .unwrap_or_default();
    let mut args = socket.to_vec();
    args.extend(["-O", "check", dest].map(OsString::from));
    args
}
