//! SFTP's plain `SSH_FXP_RENAME` (04 §5.6): the rename of a remote folder, and the publish of a
//! copy's temp where the server has no `hardlink@openssh.com`. OpenSSH's server refuses it onto
//! an existing name for a file or a folder (E2, 02 §8.2). `openssh-sftp-client` keeps the plain
//! request private — its `Fs::rename` sends `posix-rename@openssh.com`, which replaces — so it goes
//! on a second `sftp` subsystem of the same ssh master, through `openssh-sftp-client-lowlevel`'s
//! `WriteEnd::send_rename_request` (E2's route B). A link is never sent through it: there a link
//! replaced a dangling link, so a link keeps S1's path, made again at the new name. S6-A writes it.
//!
//! S6-0 scaffold: nothing yet.
