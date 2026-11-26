# Safety

- Every write first re-checks the remote file: still the same regular file (not a symlink), same
  size and modification time, and — up to 256 MiB — the same content. If anything changed since
  sopen opened it, nothing is written; your edits are kept and where is printed (exit 3).
- Conflict detection is best-effort: sopen cannot lock the remote file, so a write that races the
  final rename is not detected.
- Writes go to a temporary file in the same directory, get the original's owner and permission
  bits, are flushed, and are renamed over the original atomically. If the owner cannot be kept,
  nothing is written.
- A save that fails for any reason moves your edited copy aside (`<file>.conflict-<time>`) and
  prints where. Opening the file again never overwrites an unsaved edit.
- Unsaved edits in the hex, image and media windows are kept on disk as well as in memory until you
  save or discard them. After a crash the next open offers them back — **Restore**, **Later** or
  **Discard** — once the file is checked to be the one they were made on. A save that was cut off
  is checked on the next open: sopen says whether it landed and never writes it twice.
- The local copies sopen creates (the cache, image, hex and media backups, hex undo files, kept
  edits) are readable by you only. A binary file saved with "Save to…" is created new, never over an existing file.
  A second sopen session on a file that is still open in the first gets its own copy, so neither
  overwrites the other's edits.
- Exports never overwrite: if the name exists, sopen adds `-1`, `-2`, ….
