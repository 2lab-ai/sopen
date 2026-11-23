//! The local Trash (04 §5.5): F8 on this machine moves an entry to the system Trash where there is
//! one — macOS through `NSFileManager trashItemAtURL` (loaded at runtime, nothing linked, as the
//! save dialog's AppKit is), Linux through the freedesktop Trash (the home's `Trash` for the home's
//! filesystem, `$topdir/.Trash-$uid` for others, each with its `.trashinfo`). S6-A writes it.
//!
//! S6-0 scaffold: nothing yet.
