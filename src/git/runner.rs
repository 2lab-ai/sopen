//! Running git (03 §3): `GitRunner` and its transports — `LocalGit` (this machine: an argument
//! vector, a session of its own), `SshGit` (a remote, over sopen's ssh master with
//! [`crate::remote::exec`]'s arguments, every word quoted as `forward::quote` quotes it, after
//! the fixed `<locate>` script) and `ShellGit` (tests: `SshGit`'s words through a local `sh -c`)
//! — with 03 §3.3's environment, §3.4's classes and limits, stopping (§3.5), progress (§3.6) and
//! the command log (§3.7); at most 3 commands in flight per host (02 §6). S2-A writes it.
//!
//! S2-0 scaffold: the file, so S2-A edits only its own.
