//! A command over sopen's ssh master for a host (03 §3.2): `exec_args(dest, state_dir)`, the
//! master's `-S <ControlPath>` with `-o ControlMaster=no -o BatchMode=yes` and the keepalive
//! options of [`super::connection_args`], so the command attaches to the running master, never
//! becomes one and never prompts. Read from `sftp.rs`'s `ssh_config`. S2-A writes it.
//!
//! S2-0 scaffold: the file, and `ssh_config` with what it returns made `pub(crate)` for it.
