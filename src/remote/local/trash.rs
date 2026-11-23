//! The local Trash (04 §5.5): F8 on this machine moves an entry to the system Trash where there is
//! one — macOS through `NSFileManager trashItemAtURL` (loaded at runtime, nothing linked, as the
//! save dialog's AppKit is), Linux through the freedesktop Trash (the home's `Trash` for the home's
//! filesystem, `$topdir/.Trash-$uid` for others, each with its `.trashinfo`).
//!
//! The freedesktop Trash is compiled on every unix — its tests run on macOS too, against folders
//! of their own — and is the system's where the platform is not macOS. Tests never reach the
//! owner's Trash: in a test build [`system`] is a stand-in (process-wide, so an engine on a worker
//! thread reaches it too); the real one runs only in an `#[ignore]`d test, by hand.

use crate::remote::ops::Trash;
use crate::remote::suffixed;
use anyhow::{bail, Context, Result};
use std::ffi::{CStr, CString};
use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

/// The Trash F8 uses on this machine.
pub(crate) fn system() -> &'static dyn Trash {
    #[cfg(not(test))]
    return &Native;
    #[cfg(test)]
    &stand_in::StandIn
}

/// The system's Trash: macOS's, else this account's freedesktop Trash.
pub(crate) struct Native;

impl Trash for Native {
    fn available(&self, dir: &str) -> std::result::Result<(), String> {
        #[cfg(target_os = "macos")]
        return mac::available(dir);
        #[cfg(not(target_os = "macos"))]
        Freedesktop::of_this_account()
            .map_err(|e| format!("{e:#}"))?
            .available(Path::new(dir))
    }

    fn put(&self, path: &str) -> Result<String> {
        #[cfg(target_os = "macos")]
        return mac::put(path);
        #[cfg(not(target_os = "macos"))]
        Freedesktop::of_this_account()?
            .put(Path::new(path))
            .map(|p| p.to_string_lossy().into_owned())
    }
}

/// The highest `-N` tried for a free name in the Trash.
const MAX_SUFFIX: u32 = 10_000;

/// The freedesktop Trash (the XDG Trash specification) of one account: the home Trash for
/// entries on the filesystem that holds it, `$topdir/.Trash-$uid` on any other — `topdir` being
/// that filesystem's top folder. An entry goes under `files/` with the same name as its
/// `info/<name>.trashinfo` (`Path=`, `DeletionDate=`), the info file created exclusively first,
/// the entry then moved by an exclusive rename: nothing in the Trash is ever replaced.
///
/// The Trash's folders are checked through descriptors held open — each opened without
/// following a link, this account's own, nobody else's way in (`fchmod` 0700 where group or
/// others had any) — and the info file and the move go **relative to those descriptors**
/// (`openat`, `renameat2` / `renameatx_np`, else `linkat` + `unlinkat`): a folder swapped for a
/// link or another folder after the check is never written into. A top folder that others can
/// write without the sticky bit is no place for a Trash: there is none there (the delete dialog
/// then says permanent and asks, 04 §5.5).
#[cfg_attr(target_os = "macos", allow(dead_code))] // macOS has its own
pub(crate) struct Freedesktop {
    /// `$XDG_DATA_HOME/Trash` (`~/.local/share/Trash` by default).
    home: PathBuf,
    uid: u32,
    /// The top folder of the filesystem holding a folder (its mount point).
    topdir: TopDir,
}

/// How the top folder of a folder's filesystem is found.
type TopDir = Box<dyn Fn(&Path) -> Result<PathBuf> + Send + Sync>;

/// The longest name an entry gets in the Trash (bytes): with a `-N` and `.trashinfo` after it, its
/// info file's name still fits in one component (255 bytes). `Path=` keeps the whole name.
const TRASH_NAME_MAX: usize = 200;

/// A Trash's folders, held open.
struct Held {
    /// The Trash folder itself, for messages and what `put` answers.
    trash: PathBuf,
    files: std::fs::File,
    info: std::fs::File,
}

#[cfg(test)]
thread_local! {
    /// Right after the Trash's folders are checked and held, before anything goes in: the
    /// Trash folder's path.
    static HELD: std::cell::RefCell<Option<crate::remote::ops::SeamHook>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg_attr(target_os = "macos", allow(dead_code))] // macOS has its own
impl Freedesktop {
    /// This account's, from the environment.
    fn of_this_account() -> Result<Self> {
        let data = match std::env::var_os("XDG_DATA_HOME") {
            Some(d) if Path::new(&d).is_absolute() => PathBuf::from(d),
            _ => {
                let home = std::env::var_os("HOME").context("HOME is not set")?;
                PathBuf::from(home).join(".local/share")
            }
        };
        Ok(Self {
            home: data.join("Trash"),
            // SAFETY: geteuid has no preconditions and cannot fail.
            uid: unsafe { libc::geteuid() },
            topdir: Box::new(mount_point),
        })
    }

    /// The device of the home Trash, or of its nearest folder that exists.
    fn home_dev(&self) -> Result<u64> {
        let mut at = self.home.as_path();
        loop {
            match std::fs::metadata(at) {
                Ok(m) => return Ok(m.dev()),
                Err(_) => {
                    at = at
                        .parent()
                        .with_context(|| format!("no folder of {} exists", self.home.display()))?
                }
            }
        }
    }

    /// The Trash for an entry of the folder `dir`, and the top folder its `Path=` is relative to
    /// (`None`: the home Trash, an absolute `Path=`).
    fn trash_for(&self, dir: &Path) -> Result<(PathBuf, Option<PathBuf>)> {
        let dev = std::fs::metadata(dir)
            .with_context(|| format!("cannot stat {}", dir.display()))?
            .dev();
        if dev == self.home_dev()? {
            return Ok((self.home.clone(), None));
        }
        let top = (self.topdir)(dir)?;
        Ok((top.join(format!(".Trash-{}", self.uid)), Some(top)))
    }

    fn available(&self, dir: &Path) -> std::result::Result<(), String> {
        let (trash, top) = self.trash_for(dir).map_err(|e| format!("{e:#}"))?;
        let Some(top) = top else {
            return Ok(());
        };
        let top_dir = open_dir(&top).map_err(|e| format!("cannot open {}: {e}", top.display()))?;
        safe_top(&top_dir, &top).map_err(|e| format!("{e:#}"))?;
        match std::fs::symlink_metadata(&trash) {
            Ok(m) if m.is_dir() && m.uid() == self.uid => Ok(()),
            Ok(_) => Err(format!(
                "{} is not this account's own folder, so there is no Trash there",
                trash.display()
            )),
            Err(_) if writable(&top) => Ok(()),
            Err(_) => Err(format!(
                "{} has no Trash of this account, and one cannot be made there",
                top.display()
            )),
        }
    }

    /// Moves `path` into its Trash; where it is now.
    fn put(&self, path: &Path) -> Result<PathBuf> {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .with_context(|| format!("{} has no name sopen can use", path.display()))?;
        let dir = match path.parent() {
            Some(p) if !p.as_os_str().is_empty() => p,
            _ => Path::new("."),
        };
        let (trash, top) = self.trash_for(dir)?;
        let held = self.ready(&trash, top.as_deref())?;
        #[cfg(test)]
        crate::remote::ops::fire_seam(&HELD, &trash.to_string_lossy());
        // The entry, through the folder that holds it.
        let source = open_dir(dir).with_context(|| format!("cannot open {}", dir.display()))?;
        let source_name = c_name(name)?;
        let original = match &top {
            None => std::path::absolute(path)?,
            Some(top) => {
                let abs = std::fs::canonicalize(dir)?.join(name);
                abs.strip_prefix(top).map(Path::to_path_buf).unwrap_or(abs)
            }
        };
        let record = format!(
            "[Trash Info]\nPath={}\nDeletionDate={}\n",
            escaped(original.as_os_str().as_bytes()),
            deletion_date()
        );
        let base = trash_name(name);
        for n in 0..MAX_SUFFIX {
            let candidate = if n == 0 {
                base.clone()
            } else {
                suffixed(&base, n)
            };
            let note = c_name(&format!("{candidate}.trashinfo"))?;
            let mut file = match create_at(&held.info, &note) {
                Ok(file) => file,
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => {
                    return Err(e).with_context(|| {
                        format!(
                            "cannot write {}/info/{candidate}.trashinfo",
                            trash.display()
                        )
                    })
                }
            };
            let wrote = file
                .write_all(record.as_bytes())
                .and_then(|()| file.sync_all());
            let to = c_name(&candidate)?;
            let moved = wrote.and_then(|()| move_excl(&source, &source_name, &held.files, &to));
            match moved {
                Ok(()) => return Ok(held.trash.join("files").join(&candidate)),
                Err(e) => {
                    // SAFETY: an open descriptor and a NUL-terminated name.
                    unsafe { libc::unlinkat(held.info.as_raw_fd(), note.as_ptr(), 0) };
                    if e.kind() == std::io::ErrorKind::AlreadyExists {
                        // A `files/` entry without its info (left by another program).
                        continue;
                    }
                    return Err(e).with_context(|| {
                        format!(
                            "cannot move {} into the Trash {}",
                            path.display(),
                            trash.display()
                        )
                    });
                }
            }
        }
        bail!("no free name for {name} in {}/files", trash.display())
    }

    /// The Trash's folder and its `files` and `info`, made where missing (0700), each opened
    /// without following a link and checked through its descriptor ([`own_dir_at`]). The home
    /// Trash's parent is the account's data folder, found by its path; a top folder's must be
    /// safe for a Trash ([`safe_top`]).
    fn ready(&self, trash: &Path, top: Option<&Path>) -> Result<Held> {
        let parent = match top {
            Some(top) => top.to_path_buf(),
            None => {
                let data = trash.parent().context("the home Trash has no folder")?;
                std::fs::DirBuilder::new()
                    .recursive(true)
                    .mode(0o700)
                    .create(data)
                    .with_context(|| format!("cannot make {}", data.display()))?;
                data.to_path_buf()
            }
        };
        let parent_dir =
            open_dir(&parent).with_context(|| format!("cannot open {}", parent.display()))?;
        if top.is_some() {
            safe_top(&parent_dir, &parent)?;
        }
        let trash_name = trash
            .file_name()
            .context("a Trash folder without a name")?
            .to_str()
            .context("a Trash folder whose name is not UTF-8")?;
        let bin = own_dir_at(&parent_dir, trash_name, trash, self.uid)?;
        let files = own_dir_at(&bin, "files", &trash.join("files"), self.uid)?;
        let info = own_dir_at(&bin, "info", &trash.join("info"), self.uid)?;
        Ok(Held {
            trash: trash.to_path_buf(),
            files,
            info,
        })
    }
}

/// `name` as a C string.
#[cfg_attr(target_os = "macos", allow(dead_code))]
fn c_name(name: &str) -> Result<CString> {
    CString::new(name).map_err(|_| anyhow::anyhow!("{name}: a name with a NUL byte"))
}

/// The name an entry gets in the Trash: its own, or — longer than [`TRASH_NAME_MAX`] — cut there
/// at a character, its extension kept.
#[cfg_attr(target_os = "macos", allow(dead_code))]
fn trash_name(name: &str) -> String {
    if name.len() <= TRASH_NAME_MAX {
        return name.to_string();
    }
    let ext = match name.rfind('.') {
        Some(at) if at > 0 && name.len() - at <= 16 => &name[at..],
        _ => "",
    };
    let mut keep = TRASH_NAME_MAX - ext.len();
    while !name.is_char_boundary(keep) {
        keep -= 1;
    }
    format!("{}{ext}", &name[..keep])
}

/// The folder `path` opened (following a link: it is found by its path).
#[cfg_attr(target_os = "macos", allow(dead_code))]
fn open_dir(path: &Path) -> std::io::Result<std::fs::File> {
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_CLOEXEC)
        .open(path)
}

/// A top folder its group or others can write without the sticky bit is no place for a Trash:
/// another account could swap its `.Trash-$uid` (the freedesktop specification's own rule for
/// `$topdir/.Trash`). A group-writable one counts: its group's members are other accounts.
#[cfg_attr(target_os = "macos", allow(dead_code))]
#[allow(clippy::unnecessary_cast)] // `S_ISVTX` is `u16` on macOS, `u32` on Linux
fn safe_top(top: &std::fs::File, shown: &Path) -> Result<()> {
    let mode = top.metadata()?.mode();
    if mode & 0o022 != 0 && mode & libc::S_ISVTX as u32 == 0 {
        bail!(
            "{} can be written by other accounts and has no sticky bit, so there is no safe \
             place for a Trash there",
            shown.display()
        );
    }
    Ok(())
}

/// The folder `name` of the open folder `parent` — made (0700) where missing, opened without
/// following a link, and checked through that descriptor: a folder of the account `uid`, made
/// 0700 (`fchmod`) where group or others had any way in.
#[cfg_attr(target_os = "macos", allow(dead_code))]
fn own_dir_at(parent: &std::fs::File, name: &str, shown: &Path, uid: u32) -> Result<std::fs::File> {
    use std::os::fd::FromRawFd;
    use std::os::unix::fs::PermissionsExt;
    let c = c_name(name)?;
    // SAFETY: an open descriptor and a NUL-terminated name.
    let made = unsafe { libc::mkdirat(parent.as_raw_fd(), c.as_ptr(), 0o700) };
    if made != 0 {
        let e = std::io::Error::last_os_error();
        if e.kind() != std::io::ErrorKind::AlreadyExists {
            return Err(e).with_context(|| format!("cannot make {}", shown.display()));
        }
    }
    let not_own = || {
        anyhow::anyhow!(
            "{} is not this account's own folder, so nothing is put there",
            shown.display()
        )
    };
    // SAFETY: an open descriptor and a NUL-terminated name.
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            c.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        let e = std::io::Error::last_os_error();
        if matches!(e.raw_os_error(), Some(libc::ELOOP | libc::ENOTDIR)) {
            return Err(not_own());
        }
        return Err(e).with_context(|| format!("cannot open {}", shown.display()));
    }
    // SAFETY: a descriptor just opened, owned by nothing else.
    let dir = unsafe { std::fs::File::from_raw_fd(fd) };
    let m = dir.metadata()?;
    if !m.is_dir() || m.uid() != uid {
        return Err(not_own());
    }
    if m.mode() & 0o077 != 0 {
        dir.set_permissions(std::fs::Permissions::from_mode(0o700))
            .with_context(|| format!("cannot make {} private", shown.display()))?;
    }
    Ok(dir)
}

/// The file `name` created in the open folder `dir`: exclusively, not through a link, 0600.
#[cfg_attr(target_os = "macos", allow(dead_code))]
fn create_at(dir: &std::fs::File, name: &CStr) -> std::io::Result<std::fs::File> {
    use std::os::fd::FromRawFd;
    // SAFETY: an open descriptor and a NUL-terminated name.
    let fd = unsafe {
        libc::openat(
            dir.as_raw_fd(),
            name.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0o600 as libc::c_uint,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: a descriptor just opened, owned by nothing else.
    Ok(unsafe { std::fs::File::from_raw_fd(fd) })
}

/// The entry `from` of the open folder `from_dir` moved to `to` in the open folder `to_dir`,
/// never over anything there: the exclusive rename; where the filesystem has none, a hard link
/// (fails on an existing name) and then the old name's removal — not for a folder, which then
/// stays where it is.
#[cfg_attr(target_os = "macos", allow(dead_code))]
fn move_excl(
    from_dir: &std::fs::File,
    from: &CStr,
    to_dir: &std::fs::File,
    to: &CStr,
) -> std::io::Result<()> {
    let (f, t) = (from_dir.as_raw_fd(), to_dir.as_raw_fd());
    #[cfg(target_os = "macos")]
    // SAFETY: open descriptors and NUL-terminated names.
    let r = unsafe { libc::renameatx_np(f, from.as_ptr(), t, to.as_ptr(), libc::RENAME_EXCL) };
    #[cfg(target_os = "linux")]
    // SAFETY: open descriptors and NUL-terminated names.
    let r = unsafe { libc::renameat2(f, from.as_ptr(), t, to.as_ptr(), libc::RENAME_NOREPLACE) };
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    let r = {
        let _ = (f, t);
        -2
    };
    if r == 0 {
        return Ok(());
    }
    let e = if r == -2 {
        std::io::Error::from_raw_os_error(libc::ENOTSUP)
    } else {
        std::io::Error::last_os_error()
    };
    let unsupported = matches!(
        e.raw_os_error(),
        Some(c) if [libc::EINVAL, libc::ENOSYS, libc::ENOTSUP, libc::EOPNOTSUPP].contains(&c)
    );
    if !unsupported {
        return Err(e);
    }
    // SAFETY: an all-zero stat is a valid value to be overwritten; an open descriptor, a
    // NUL-terminated name and a valid place for the result.
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: as above.
    if unsafe { libc::fstatat(f, from.as_ptr(), &mut st, libc::AT_SYMLINK_NOFOLLOW) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    if st.st_mode & libc::S_IFMT == libc::S_IFDIR {
        return Err(std::io::Error::other(
            "this filesystem cannot move a folder without the risk of replacing a name",
        ));
    }
    // SAFETY: open descriptors and NUL-terminated names; flags 0 links a link itself.
    if unsafe { libc::linkat(f, from.as_ptr(), t, to.as_ptr(), 0) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: an open descriptor and a NUL-terminated name.
    if unsafe { libc::unlinkat(f, from.as_ptr(), 0) } != 0 {
        let e = std::io::Error::last_os_error();
        // Both names are one entry: the one just made in the Trash goes again.
        // SAFETY: as above.
        unsafe { libc::unlinkat(t, to.as_ptr(), 0) };
        return Err(e);
    }
    Ok(())
}

/// Whether this account can make an entry in the folder `path`.
#[cfg_attr(target_os = "macos", allow(dead_code))]
fn writable(path: &Path) -> bool {
    let Ok(c) = CString::new(path.as_os_str().as_bytes()) else {
        return false;
    };
    // SAFETY: a NUL-terminated path.
    unsafe { libc::access(c.as_ptr(), libc::W_OK | libc::X_OK) == 0 }
}

/// [`super::ops::rename_excl`] by paths (the tests' stand-in Trash).
#[cfg(test)]
fn rename_excl(from: &Path, to: &Path) -> std::io::Result<()> {
    let c = |p: &Path| {
        CString::new(p.as_os_str().as_bytes())
            .map_err(|_| std::io::Error::from_raw_os_error(libc::EINVAL))
    };
    super::ops::rename_excl(&c(from)?, &c(to)?)
}

/// The top folder of the filesystem holding the folder `dir`: the highest folder above it on
/// the same device.
#[cfg_attr(target_os = "macos", allow(dead_code))]
fn mount_point(dir: &Path) -> Result<PathBuf> {
    let mut at = std::fs::canonicalize(dir)?;
    let dev = std::fs::metadata(&at)?.dev();
    while let Some(up) = at.parent() {
        if std::fs::metadata(up)?.dev() != dev {
            break;
        }
        at = up.to_path_buf();
    }
    Ok(at)
}

/// `Path=`'s escaping (RFC 2396, as the specification asks): every byte but the unreserved ones
/// and `/` as `%XX`.
#[cfg_attr(target_os = "macos", allow(dead_code))]
fn escaped(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len());
    for &b in bytes {
        if b.is_ascii_alphanumeric() || b"-_.~/".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// `DeletionDate=`: now, local time, `YYYY-MM-DDThh:mm:ss`.
#[cfg_attr(target_os = "macos", allow(dead_code))]
fn deletion_date() -> String {
    // SAFETY: time(NULL) has no preconditions; an all-zero tm is a valid value to be overwritten;
    // localtime_r writes only into it.
    unsafe {
        let now = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&now, &mut tm).is_null() {
            return "1970-01-01T00:00:00".to_string();
        }
        format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
            tm.tm_year + 1900,
            tm.tm_mon + 1,
            tm.tm_mday,
            tm.tm_hour,
            tm.tm_min,
            tm.tm_sec
        )
    }
}

/// macOS: `-[NSFileManager trashItemAtURL:resultingItemURL:error:]` through the Objective-C
/// runtime, `libobjc` and Foundation loaded with `dlopen` (nothing linked). Every call runs in an
/// autorelease pool of its own.
#[cfg(target_os = "macos")]
mod mac {
    use anyhow::{anyhow, Result};
    use std::ffi::{c_char, c_void, CStr, CString};
    use std::sync::OnceLock;

    type Id = *mut c_void;
    type Sel = *mut c_void;

    const OBJC: &CStr = c"/usr/lib/libobjc.A.dylib";
    const FOUNDATION: &CStr = c"/System/Library/Frameworks/Foundation.framework/Foundation";
    /// `NSTrashDirectory`, `NSUserDomainMask`.
    const TRASH_DIRECTORY: usize = 102;
    const USER_DOMAIN: usize = 1;

    struct Api {
        get_class: unsafe extern "C" fn(*const c_char) -> Id,
        sel: unsafe extern "C" fn(*const c_char) -> Sel,
        /// `objc_msgSend`, cast at each call to that method's prototype.
        send: *mut c_void,
        push: unsafe extern "C" fn() -> *mut c_void,
        pop: unsafe extern "C" fn(*mut c_void),
    }

    // SAFETY: function pointers into system libraries that stay loaded; usable from any thread.
    unsafe impl Send for Api {}
    unsafe impl Sync for Api {}

    /// Opens `path`; the handle is never closed (the libraries stay loaded).
    fn open(path: &CStr) -> std::result::Result<*mut c_void, String> {
        // SAFETY: `path` is a C string naming a system library.
        let handle = unsafe { libc::dlopen(path.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
        if handle.is_null() {
            return Err(format!("cannot load {}", path.to_string_lossy()));
        }
        Ok(handle)
    }

    /// The symbol `name` of `handle`, as `F`.
    ///
    /// SAFETY: the caller names `F` as the SDK declares the symbol.
    unsafe fn symbol<F: Copy>(handle: *mut c_void, name: &CStr) -> std::result::Result<F, String> {
        // SAFETY: `handle` came from dlopen, `name` is a C string.
        let found = unsafe { libc::dlsym(handle, name.as_ptr()) };
        if found.is_null() {
            return Err(format!("libobjc has no {}", name.to_string_lossy()));
        }
        // SAFETY: a function pointer the size of a data pointer; the caller vouches for its type.
        Ok(unsafe { std::mem::transmute_copy::<*mut c_void, F>(&found) })
    }

    fn api() -> std::result::Result<&'static Api, String> {
        static API: OnceLock<std::result::Result<Api, String>> = OnceLock::new();
        API.get_or_init(|| {
            let objc = open(OBJC)?;
            // Loaded for its classes (NSFileManager, NSURL, NSString), found by name below.
            open(FOUNDATION)?;
            // SAFETY: each type is the runtime's prototype of the symbol it names.
            unsafe {
                Ok(Api {
                    get_class: symbol(objc, c"objc_getClass")?,
                    sel: symbol(objc, c"sel_registerName")?,
                    send: symbol::<*mut c_void>(objc, c"objc_msgSend")?,
                    push: symbol(objc, c"objc_autoreleasePoolPush")?,
                    pop: symbol(objc, c"objc_autoreleasePoolPop")?,
                })
            }
        })
        .as_ref()
        .map_err(Clone::clone)
    }

    /// An autorelease pool, popped when dropped.
    struct Pool<'a>(&'a Api, *mut c_void);

    impl Drop for Pool<'_> {
        fn drop(&mut self) {
            // SAFETY: the token this pool's push returned, popped once.
            unsafe { (self.0.pop)(self.1) }
        }
    }

    impl Api {
        fn class(&self, name: &CStr) -> Result<Id> {
            // SAFETY: a C string.
            let class = unsafe { (self.get_class)(name.as_ptr()) };
            if class.is_null() {
                return Err(anyhow!("no class {}", name.to_string_lossy()));
            }
            Ok(class)
        }

        fn sel(&self, name: &CStr) -> Sel {
            // SAFETY: a C string; sel_registerName never fails.
            unsafe { (self.sel)(name.as_ptr()) }
        }

        /// `objc_msgSend` as the prototype `F`.
        ///
        /// SAFETY: `F` is the method's prototype, receiver and selector first.
        unsafe fn send<F: Copy>(&self) -> F {
            // SAFETY: as the caller vouches.
            unsafe { std::mem::transmute_copy::<*mut c_void, F>(&self.send) }
        }

        /// An NSString's UTF-8 text (`None` for nil).
        fn text(&self, string: Id) -> Option<String> {
            if string.is_null() {
                return None;
            }
            // SAFETY: -[NSString UTF8String] → const char *.
            let utf8: unsafe extern "C" fn(Id, Sel) -> *const c_char = unsafe { self.send() };
            // SAFETY: a live NSString and its selector.
            let c = unsafe { utf8(string, self.sel(c"UTF8String")) };
            // SAFETY: a NUL-terminated string the pool keeps alive.
            (!c.is_null()).then(|| unsafe { CStr::from_ptr(c) }.to_string_lossy().into_owned())
        }

        /// An NSError's description.
        fn error(&self, error: Id) -> String {
            if error.is_null() {
                return "no reason given".to_string();
            }
            // SAFETY: -[NSError localizedDescription] → NSString *.
            let describe: unsafe extern "C" fn(Id, Sel) -> Id = unsafe { self.send() };
            // SAFETY: a live NSError and its selector.
            let said = unsafe { describe(error, self.sel(c"localizedDescription")) };
            self.text(said)
                .unwrap_or_else(|| "no reason given".to_string())
        }

        /// `[NSURL fileURLWithPath:]` of `path`.
        fn url(&self, path: &str) -> Result<Id> {
            let c = CString::new(path).map_err(|_| anyhow!("{path}: a path with a NUL byte"))?;
            // SAFETY: +[NSString stringWithUTF8String:] → NSString *.
            let string: unsafe extern "C" fn(Id, Sel, *const c_char) -> Id = unsafe { self.send() };
            // SAFETY: the class, its selector and a C string.
            let ns = unsafe {
                string(
                    self.class(c"NSString")?,
                    self.sel(c"stringWithUTF8String:"),
                    c.as_ptr(),
                )
            };
            if ns.is_null() {
                return Err(anyhow!("{path} is not a name Foundation takes"));
            }
            // SAFETY: +[NSURL fileURLWithPath:] → NSURL *.
            let file_url: unsafe extern "C" fn(Id, Sel, Id) -> Id = unsafe { self.send() };
            // SAFETY: the class, its selector and an NSString.
            let url = unsafe { file_url(self.class(c"NSURL")?, self.sel(c"fileURLWithPath:"), ns) };
            if url.is_null() {
                return Err(anyhow!("no file URL for {path}"));
            }
            Ok(url)
        }

        fn manager(&self) -> Result<Id> {
            // SAFETY: +[NSFileManager defaultManager] → NSFileManager *.
            let default: unsafe extern "C" fn(Id, Sel) -> Id = unsafe { self.send() };
            // SAFETY: the class and its selector.
            let fm = unsafe { default(self.class(c"NSFileManager")?, self.sel(c"defaultManager")) };
            if fm.is_null() {
                return Err(anyhow!("no NSFileManager"));
            }
            Ok(fm)
        }
    }

    pub(super) fn available(dir: &str) -> std::result::Result<(), String> {
        let api = api()?;
        // SAFETY: a pool pushed here, popped by its guard.
        let _pool = Pool(api, unsafe { (api.push)() });
        let found = (|| -> Result<()> {
            let fm = api.manager()?;
            let url = api.url(dir)?;
            // SAFETY: -[NSFileManager URLForDirectory:inDomain:appropriateForURL:create:error:]
            // → NSURL *; NSUInteger, NSUInteger, NSURL *, BOOL, NSError **.
            let trash_for: unsafe extern "C" fn(Id, Sel, usize, usize, Id, i8, *mut Id) -> Id =
                unsafe { api.send() };
            let mut error: Id = std::ptr::null_mut();
            // SAFETY: live objects, their selector, and a place for the error.
            let trash = unsafe {
                trash_for(
                    fm,
                    api.sel(c"URLForDirectory:inDomain:appropriateForURL:create:error:"),
                    TRASH_DIRECTORY,
                    USER_DOMAIN,
                    url,
                    0,
                    &mut error,
                )
            };
            if trash.is_null() {
                return Err(anyhow!("no Trash for {dir}: {}", api.error(error)));
            }
            Ok(())
        })();
        found.map_err(|e| format!("{e:#}"))
    }

    pub(super) fn put(path: &str) -> Result<String> {
        let api = api().map_err(|e| anyhow!(e))?;
        // SAFETY: a pool pushed here, popped by its guard.
        let _pool = Pool(api, unsafe { (api.push)() });
        let fm = api.manager()?;
        let url = api.url(path)?;
        // SAFETY: -[NSFileManager trashItemAtURL:resultingItemURL:error:] → BOOL; NSURL *,
        // NSURL **, NSError **.
        let trash_item: unsafe extern "C" fn(Id, Sel, Id, *mut Id, *mut Id) -> i8 =
            unsafe { api.send() };
        let (mut moved, mut error): (Id, Id) = (std::ptr::null_mut(), std::ptr::null_mut());
        // SAFETY: live objects, their selector, and places for the results.
        let ok = unsafe {
            trash_item(
                fm,
                api.sel(c"trashItemAtURL:resultingItemURL:error:"),
                url,
                &mut moved,
                &mut error,
            )
        };
        if ok == 0 {
            return Err(anyhow!("{}", api.error(error)));
        }
        if moved.is_null() {
            return Ok("the Trash".to_string());
        }
        // SAFETY: -[NSURL path] → NSString *.
        let path_of: unsafe extern "C" fn(Id, Sel) -> Id = unsafe { api.send() };
        // SAFETY: a live NSURL and its selector.
        let at = unsafe { path_of(moved, api.sel(c"path")) };
        Ok(api.text(at).unwrap_or_else(|| "the Trash".to_string()))
    }
}

/// The tests' Trash: entries go into a folder of the test process's own in the system's temp
/// folder (removed when the process exits), each under a name of its own, and where each went is
/// kept for the test to look at.
#[cfg(test)]
mod stand_in {
    use super::rename_excl;
    use crate::remote::ops::Trash;
    use anyhow::{Context, Result};
    use std::path::PathBuf;
    use std::sync::Mutex;

    pub(super) struct StandIn;

    /// `(from, to)` of every entry the stand-in took.
    pub(super) static TAKEN: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());

    /// The tests' Trash folder, this process's own.
    fn bin() -> PathBuf {
        std::env::temp_dir().join(format!("sopen-tests-trash-{}", std::process::id()))
    }

    /// Removes the tests' Trash folder when the test process exits.
    extern "C" fn remove_bin() {
        let _ = std::fs::remove_dir_all(bin());
    }

    impl Trash for StandIn {
        fn available(&self, _dir: &str) -> std::result::Result<(), String> {
            Ok(())
        }

        fn put(&self, path: &str) -> Result<String> {
            static CLEANUP: std::sync::Once = std::sync::Once::new();
            // SAFETY: `remove_bin` is a plain function that touches only the tests' folder.
            CLEANUP.call_once(|| unsafe {
                libc::atexit(remove_bin);
            });
            let bin = bin();
            std::fs::create_dir_all(&bin)?;
            let name = std::path::Path::new(path)
                .file_name()
                .context("no name")?
                .to_string_lossy()
                .into_owned();
            let to = bin.join(format!("{}-{name}", crate::remote::random_hex()));
            rename_excl(std::path::Path::new(path), &to)
                .with_context(|| format!("cannot move {path} into the tests' Trash"))?;
            let to = to.to_string_lossy().into_owned();
            TAKEN
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .push((path.to_string(), to.clone()));
            Ok(to)
        }
    }
}

#[cfg(test)]
impl super::LocalFs {
    /// Where the tests' Trash put `from` (`None`: it did not).
    pub(crate) fn trashed_by_tests(from: &str) -> Option<String> {
        let taken = stand_in::TAKEN.lock().unwrap_or_else(|p| p.into_inner());
        taken
            .iter()
            .rev()
            .find(|(f, _)| f == from)
            .map(|(_, to)| to.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A home Trash and a top folder of a test's own.
    fn bins() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        let (data, items) = (root.join("data"), root.join("items"));
        std::fs::create_dir(&data).unwrap();
        std::fs::create_dir(&items).unwrap();
        (tmp, data, items)
    }

    fn info_of(trash: &Path, name: &str) -> String {
        std::fs::read_to_string(trash.join("info").join(format!("{name}.trashinfo"))).unwrap()
    }

    /// The freedesktop home Trash: a file, a folder and a link go under `files/` with their
    /// `.trashinfo` (the absolute, escaped `Path=` and a `DeletionDate=`); a second entry of the
    /// same name gets the next free name — nothing in the Trash is replaced; a link goes as a link.
    #[test]
    fn the_freedesktop_home_trash_takes_files_folders_and_links() {
        let (_t, data, items) = bins();
        // SAFETY: geteuid has no preconditions.
        let uid = unsafe { libc::geteuid() };
        let fd = Freedesktop {
            home: data.join("Trash"),
            uid,
            topdir: Box::new(|_| panic!("the home Trash's filesystem")),
        };
        assert_eq!(fd.available(&items), Ok(()));
        let trash = data.join("Trash");
        std::fs::write(items.join("a b%.txt"), "first").unwrap();
        let to = fd.put(&items.join("a b%.txt")).unwrap();
        assert_eq!(to, trash.join("files/a b%.txt"));
        assert_eq!(std::fs::read(&to).unwrap(), b"first");
        let info = info_of(&trash, "a b%.txt");
        let want = format!(
            "Path={}\n",
            escaped(items.join("a b%.txt").as_os_str().as_bytes())
        );
        assert!(
            info.starts_with("[Trash Info]\n") && info.contains(&want),
            "{info}"
        );
        assert!(info.contains("a%20b%25.txt"), "{info}");
        assert!(info.contains("\nDeletionDate=2"), "{info}");
        std::fs::write(items.join("a b%.txt"), "second").unwrap();
        let to2 = fd.put(&items.join("a b%.txt")).unwrap();
        assert_eq!(to2, trash.join("files/a b%-1.txt"));
        assert_eq!(std::fs::read(&to).unwrap(), b"first", "never replaced");
        assert!(trash.join("info/a b%-1.txt.trashinfo").exists());
        std::fs::create_dir(items.join("d")).unwrap();
        std::fs::write(items.join("d/x"), "x").unwrap();
        let d = fd.put(&items.join("d")).unwrap();
        assert_eq!(std::fs::read(d.join("x")).unwrap(), b"x");
        std::os::unix::fs::symlink("d", items.join("l")).unwrap();
        let l = fd.put(&items.join("l")).unwrap();
        assert_eq!(std::fs::read_link(&l).unwrap(), PathBuf::from("d"));
        assert_eq!(std::fs::read_dir(&items).unwrap().count(), 0);
        // A `files/` entry without its info is not replaced either.
        std::fs::write(trash.join("files/orphan"), "theirs").unwrap();
        std::fs::write(items.join("orphan"), "mine").unwrap();
        let o = fd.put(&items.join("orphan")).unwrap();
        assert_eq!(o, trash.join("files/orphan-1"));
        assert_eq!(
            std::fs::read(trash.join("files/orphan")).unwrap(),
            b"theirs"
        );
        assert!(!trash.join("info/orphan.trashinfo").exists());
    }

    /// An entry on another filesystem than the home Trash goes to `$topdir/.Trash-$uid`, made
    /// 0700 where missing, its `Path=` relative to the top folder; a `.Trash-$uid` that is a link
    /// is refused, nothing moved.
    #[test]
    fn the_freedesktop_top_trash_is_the_accounts_own_folder() {
        let (_t, data, items) = bins();
        let top = items.parent().unwrap().to_path_buf();
        // SAFETY: geteuid has no preconditions.
        let uid = unsafe { libc::geteuid() };
        let at = top.clone();
        let fd = Freedesktop {
            // A home Trash on another device (`/dev`'s): the test's items are not on its
            // filesystem.
            home: PathBuf::from("/dev/null/sopen-tests-trash"),
            uid,
            topdir: Box::new(move |_| Ok(at.clone())),
        };
        assert_ne!(
            fd.home_dev().unwrap(),
            std::fs::metadata(&items).unwrap().dev()
        );
        std::fs::create_dir(items.join("sub")).unwrap();
        std::fs::write(items.join("sub/f"), "f").unwrap();
        assert_eq!(fd.available(&items.join("sub")), Ok(()));
        let to = fd.put(&items.join("sub/f")).unwrap();
        let bin = top.join(format!(".Trash-{uid}"));
        assert_eq!(to, bin.join("files/f"));
        assert_eq!(std::fs::read(&to).unwrap(), b"f");
        assert_eq!(
            std::fs::symlink_metadata(&bin).unwrap().mode() & 0o7777,
            0o700
        );
        assert!(info_of(&bin, "f").contains("\nPath=items/sub/f\n"));
        std::fs::remove_dir_all(&bin).unwrap();
        std::os::unix::fs::symlink(&data, &bin).unwrap();
        std::fs::write(items.join("g"), "g").unwrap();
        let e = fd.put(&items.join("g")).unwrap_err();
        assert!(
            format!("{e:#}").contains("not this account's own folder"),
            "{e:#}"
        );
        assert_eq!(std::fs::read(items.join("g")).unwrap(), b"g");
        assert!(fd.available(&items).is_err());
        assert_eq!(
            std::fs::read_dir(&data).unwrap().count(),
            0,
            "nothing through the link"
        );
    }

    /// The Trash's folders are swapped after they were checked and held — the top folder's
    /// `.Trash-$uid` and the home Trash alike moved away and a link to another folder put in their
    /// place: the info file and the entry go into the folders that were checked (through their
    /// descriptors), nothing through the link.
    #[test]
    fn a_trash_folder_swapped_after_its_check_is_never_written_through() {
        for on_top in [true, false] {
            let (_t, data, items) = bins();
            let top = items.parent().unwrap().to_path_buf();
            let elsewhere = top.join("elsewhere");
            std::fs::create_dir(&elsewhere).unwrap();
            // SAFETY: geteuid has no preconditions.
            let uid = unsafe { libc::geteuid() };
            let at = top.clone();
            let fd = Freedesktop {
                home: match on_top {
                    true => PathBuf::from("/dev/null/sopen-tests-trash"),
                    false => data.join("Trash"),
                },
                uid,
                topdir: Box::new(move |_| Ok(at.clone())),
            };
            let bin = match on_top {
                true => top.join(format!(".Trash-{uid}")),
                false => data.join("Trash"),
            };
            let moved = top.join("moved-trash");
            let (from, link_to) = (bin.clone(), elsewhere.clone());
            let swapped = moved.clone();
            let _seam = crate::remote::ops::set_seam(&HELD, move |_| {
                std::fs::rename(&from, &swapped).unwrap();
                std::os::unix::fs::symlink(&link_to, &from).unwrap();
            });
            std::fs::write(items.join("f"), "f").unwrap();
            fd.put(&items.join("f")).unwrap();
            assert_eq!(
                std::fs::read_dir(&elsewhere).unwrap().count(),
                0,
                "on_top={on_top}: nothing through the link"
            );
            assert_eq!(std::fs::read(moved.join("files/f")).unwrap(), b"f");
            assert!(info_of(&moved, "f").starts_with("[Trash Info]\n"));
            assert!(!items.join("f").exists());
        }
    }

    /// A top folder that other accounts can write, without the sticky bit, has no Trash: `put`
    /// and `available` refuse, nothing moved; with the sticky bit it has one.
    #[test]
    fn a_top_folder_others_can_write_without_the_sticky_bit_has_no_trash() {
        use std::os::unix::fs::PermissionsExt;
        let (_t, _data, items) = bins();
        let top = items.parent().unwrap().to_path_buf();
        // SAFETY: geteuid has no preconditions.
        let uid = unsafe { libc::geteuid() };
        let at = top.clone();
        let fd = Freedesktop {
            home: PathBuf::from("/dev/null/sopen-tests-trash"),
            uid,
            topdir: Box::new(move |_| Ok(at.clone())),
        };
        let chmod = |mode| std::fs::set_permissions(&top, std::fs::Permissions::from_mode(mode));
        std::fs::write(items.join("f"), "f").unwrap();
        chmod(0o777).unwrap();
        let refused = fd.put(&items.join("f"));
        let available = fd.available(&items);
        chmod(0o1777).unwrap();
        let e = refused.unwrap_err();
        assert!(format!("{e:#}").contains("sticky"), "{e:#}");
        assert!(
            matches!(&available, Err(why) if why.contains("sticky")),
            "{available:?}"
        );
        assert_eq!(std::fs::read(items.join("f")).unwrap(), b"f");
        assert!(!top.join(format!(".Trash-{uid}")).exists());
        assert_eq!(fd.available(&items), Ok(()));
        fd.put(&items.join("f")).unwrap();
        chmod(0o700).unwrap();
    }

    /// An existing Trash whose folders others can read or enter is made private (0700, through
    /// its descriptor) before anything goes in.
    #[test]
    fn an_existing_trash_others_can_enter_is_made_private() {
        use std::os::unix::fs::PermissionsExt;
        let (_t, _data, items) = bins();
        let top = items.parent().unwrap().to_path_buf();
        // SAFETY: geteuid has no preconditions.
        let uid = unsafe { libc::geteuid() };
        let bin = top.join(format!(".Trash-{uid}"));
        for folder in [&bin, &bin.join("files"), &bin.join("info")] {
            std::fs::create_dir(folder).unwrap();
            std::fs::set_permissions(folder, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let at = top.clone();
        let fd = Freedesktop {
            home: PathBuf::from("/dev/null/sopen-tests-trash"),
            uid,
            topdir: Box::new(move |_| Ok(at.clone())),
        };
        std::fs::write(items.join("f"), "f").unwrap();
        fd.put(&items.join("f")).unwrap();
        for folder in [&bin, &bin.join("files"), &bin.join("info")] {
            let mode = std::fs::symlink_metadata(folder).unwrap().mode() & 0o7777;
            assert_eq!(mode, 0o700, "{}", folder.display());
        }
    }

    /// A name of 255 bytes: its info file's name would not fit one component, so it gets a
    /// shorter name in the Trash, its extension kept; `Path=` keeps the whole name.
    #[test]
    fn a_name_of_255_bytes_gets_a_shorter_trash_name() {
        let (_t, data, items) = bins();
        // SAFETY: geteuid has no preconditions.
        let uid = unsafe { libc::geteuid() };
        let fd = Freedesktop {
            home: data.join("Trash"),
            uid,
            topdir: Box::new(|_| panic!("the home Trash's filesystem")),
        };
        let long = format!("{}.txt", "n".repeat(251));
        assert_eq!(long.len(), 255);
        std::fs::write(items.join(&long), "long").unwrap();
        let to = fd.put(&items.join(&long)).unwrap();
        let short = to.file_name().unwrap().to_str().unwrap().to_string();
        assert!(
            short.len() <= TRASH_NAME_MAX && short.ends_with(".txt"),
            "{short}"
        );
        assert_eq!(std::fs::read(&to).unwrap(), b"long");
        let info = info_of(&data.join("Trash"), &short);
        assert!(info.contains(&format!("{long}\n")), "{info}");
        // A second one of the same name gets the next free name, still short enough.
        std::fs::write(items.join(&long), "again").unwrap();
        let to2 = fd.put(&items.join(&long)).unwrap();
        assert_ne!(to2, to);
        assert_eq!(std::fs::read(&to).unwrap(), b"long");
    }

    /// macOS: Foundation, loaded at runtime, says where the Trash for a folder is — only asked
    /// (`create: NO`), nothing put there: the runtime calls of `mac` up to the move itself.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_macos_trash_is_found_through_foundation() {
        let tmp = tempfile::tempdir().unwrap();
        // Whether this account has a Trash is the host's; that the runtime, its classes and
        // the call answered is the code's.
        match Native.available(tmp.path().to_str().unwrap()) {
            Ok(()) => {}
            Err(why) => assert!(why.starts_with("no Trash for"), "{why}"),
        }
    }

    /// The real Trash of this machine (macOS: `~/.Trash`; Linux: the home's freedesktop Trash):
    /// a file goes there with its bytes, and a link goes as a link — its target stays. By hand
    /// only, as it puts both into the owner's Trash. macOS may refuse this process a look inside
    /// `~/.Trash` ("Operation not permitted"): then the receipt is the path it answered and the
    /// entries gone from their folder.
    #[test]
    #[ignore = "puts a file and a link into this account's real Trash"]
    fn the_system_trash_takes_a_file_and_a_link() {
        let tmp = tempfile::tempdir().unwrap();
        let tag = crate::remote::random_hex();
        let f = tmp.path().join(format!("sopen-trash-receipt-{tag}.txt"));
        let l = tmp.path().join(format!("sopen-trash-receipt-{tag}.link"));
        std::fs::write(&f, "sopen's Trash receipt").unwrap();
        std::os::unix::fs::symlink(&f, &l).unwrap();
        let dir = tmp.path().to_str().unwrap();
        Native.available(dir).unwrap();
        let link_at = Native.put(l.to_str().unwrap()).unwrap();
        eprintln!("the link is in the Trash at {link_at}");
        assert!(std::fs::symlink_metadata(&l).is_err(), "the link went");
        assert_eq!(
            std::fs::read(&f).unwrap(),
            b"sopen's Trash receipt",
            "its target stayed"
        );
        match std::fs::read_link(&link_at) {
            Ok(target) => assert_eq!(target, f),
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                eprintln!("the Trash cannot be looked into from here: {e}")
            }
            Err(e) => panic!("{link_at}: {e}"),
        }
        let file_at = Native.put(f.to_str().unwrap()).unwrap();
        eprintln!("the file is in the Trash at {file_at}");
        assert!(!f.exists(), "the file went");
        match std::fs::read(&file_at) {
            Ok(bytes) => assert_eq!(bytes, b"sopen's Trash receipt"),
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                eprintln!("the Trash cannot be looked into from here: {e}")
            }
            Err(e) => panic!("{file_at}: {e}"),
        }
    }
}
