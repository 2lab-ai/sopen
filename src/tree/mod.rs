//! The directory tree's model (05): what F10 shows in place of the active panel's list — the root
//! rule, which folders are shown, listed, sized and opened by themselves, each row's info line,
//! the background scans and the watchdog every local call runs behind. Its reads go through
//! [`crate::remote::treefs::TreeFs`]; the browser draws it (`gui::browser::tree`, S5-B).
//!
//! # API (for the integration, S5-C)
//!
//! Nothing here blocks: every read is a request to the tree's [`TreeFs`] worker, and the model
//! takes the answers when it is polled. Per frame: [`Tree::poll`], then [`Tree::rows`].
//!
//! - **Open**: [`Tree::open`]`(fs, `[`Open`]`)` — `fs` is [`LocalFs::tree`] for the local disk, or
//!   [`SftpRemote::into_tree`] on the tree's own connection (`Connect`, 02 §6); `Open` names the
//!   host (the cache key; empty for the local disk), the active panel's folder as the panel names
//!   it (resolved here, 05 §2), the remote account for `~<user>`, and [`Params`] (`e` = the rows
//!   the tree shows). [`Tree::set_wake`] gives the worker the window's repaint.
//! - **Each frame**: [`Tree::poll`] applies what came (`true`: something changed). Until
//!   [`Tree::ready`] (phase 1, 05 §6.3) there are no rows; [`Tree::error`] says why there never
//!   will be (the folder cannot be resolved). Until [`Tree::built`] rows may still open by
//!   themselves (05 §5). [`Tree::title`] is the frame's `host:/abs`.
//! - **Rows**: [`Tree::rows`] → [`Row`] — `id` (a [`RowId`], stable while the tree is open: keep
//!   the bar by it, rows are inserted above it as listings come), `depth`, `name` (`~` or `/` for
//!   the root, `a/b/c` for a compact chain, `· 3 hidden folders`), `kind` ([`RowKind`]), `glyph`
//!   ([`Glyph`]: `▼` `►` none `→`), `open`, `auto` (opened by 05 §5), `dimmed` (hidden folders),
//!   `current` (the panel's folder — the bar starts on [`Tree::current`], its row in the middle),
//!   `path` (absolute, on the host) and `info` ([`Info`]: `dirs`, `files`, `size`, `fs_files`;
//!   [`Info::columns`] gives the three right-aligned texts). [`Tree::row_of`] finds an id's row.
//! - **The view**: [`Tree::view`]`(top, height, bar)` after every scroll or bar move — phase 2
//!   lists the rows on screen and one screen around, phase 3 sizes the closed rows on screen,
//!   nearest the bar first; a row off screen for more than a second stops its scan.
//! - **Keys**: → [`Tree::expand`] (a link: nothing; the hidden row: one dimmed row each), ←
//!   [`Tree::collapse`] (on a closed row: returns its parent's row for the bar), Space
//!   [`Tree::size_now`], Cmd/Ctrl+R [`Tree::reread`], Enter [`Tree::enter`] (`Some(path)`: list
//!   it in the panel, 02 §7 `TreeOutcome::GoTo`; `None` for a link: its resolved target comes
//!   from [`Tree::entered`] after a poll). [`Tree::owner_busy`] / [`Tree::stop_owner`]: Esc stops
//!   an owner's whole listing (05 §6.3) before it closes the tree.
//! - **Status row**: [`Tree::status`] (the watchdog's cap: `4 calls not answering; …`).
//! - **Close**: [`Tree::close`] (or drop) stops every scan and listing and never waits.
//!
//! [`LocalFs::tree`]: crate::remote::LocalFs::tree
//! [`SftpRemote::into_tree`]: crate::remote::SftpRemote::into_tree

mod cache;
mod expand;
mod info;
mod root;
mod rules;
mod scan;
#[cfg(test)]
mod tests;
pub(crate) mod watchdog;

pub use expand::Params;
pub use info::{human, Count, Info, Size, Tag, SCANNING_MARK};

use crate::remote::treefs::{
    Ask, Child, Kind, Listing, Mnt, Resolved, ScanEnd, Target, Totals, TreeFs,
};
use crate::remote::Wake;
use rules::{Class, MountMap, Rule};
use std::collections::HashMap;
use std::time::Instant;

/// What F10 opens.
#[derive(Debug, Clone)]
pub struct Open {
    /// The host as the cache knows it and the title names it (`user@host`, the remote's label;
    /// for the local disk anything, empty included). Whether the tree is local is the worker's
    /// to say ([`TreeFs::is_local`]), never this name's.
    pub host: String,
    /// The active panel's folder as the panel names it (joined names; may run through a link).
    pub folder: String,
    /// A remote's account, for `expand-path` of `~<user>` (the user `ssh -G` names).
    pub user: Option<String>,
    pub params: Params,
}

/// A row's identity while the tree is open.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RowId {
    /// A folder's row (on a compact chain, its last folder).
    Folder(u32),
    /// The `· N hidden folders` row of a folder.
    Hidden(u32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowKind {
    Folder,
    /// A link to a folder: marked `<LINK>`, never opens in place.
    Link,
    /// `· N hidden folders`.
    Hidden(usize),
}

/// The glyph before a row's name (05 §3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Glyph {
    /// `▼`.
    Open,
    /// `►`: closed with subfolders, or not known to have none.
    Closed,
    /// No glyph: a folder with no subfolders (and the hidden row).
    None,
    /// `→`.
    Link,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub id: RowId,
    pub depth: usize,
    pub name: String,
    pub kind: RowKind,
    pub glyph: Glyph,
    pub open: bool,
    /// Opened by 05 §5 (not by the owner, not by rule 1).
    pub auto: bool,
    pub dimmed: bool,
    /// The active panel's folder.
    pub current: bool,
    pub path: String,
    pub info: Info,
}

/// What became of a folder's listing.
#[derive(Debug, Clone, Default)]
struct Digest {
    /// Subfolders, links to folders included.
    dirs: u64,
    /// Regular files and links to files.
    files: u64,
    /// The regular files' bytes, a hard-linked file once.
    bytes: u64,
    complete: bool,
    /// Real subfolders (not links): what a scan enters.
    real_dirs: u64,
    /// Anything but folders: files, links, special files.
    others: u64,
    cachedir: bool,
    git: bool,
    venv: bool,
    /// Inodes of the files with more than one link (counted once).
    multi: Vec<(u64, u64)>,
}

impl Digest {
    fn of(listing: &Listing) -> Self {
        let mut d = Digest {
            complete: listing.complete,
            cachedir: listing.cachedir,
            ..Digest::default()
        };
        let mut seen = std::collections::HashSet::new();
        let has = |n: &str| listing.children.iter().any(|c| c.name == n);
        d.git = has("HEAD") && has("objects") && has("refs");
        d.venv = listing
            .children
            .iter()
            .any(|c| c.name == "pyvenv.cfg" && c.kind == Kind::File);
        for c in &listing.children {
            match c.kind {
                Kind::Dir => {
                    d.dirs += 1;
                    d.real_dirs += 1;
                }
                Kind::Link(Target::Dir) => {
                    d.dirs += 1;
                    d.others += 1;
                }
                Kind::File => {
                    d.files += 1;
                    d.others += 1;
                    let first = match c.id {
                        Some(id) if c.nlink > 1 => {
                            d.multi.push(id);
                            seen.insert(id)
                        }
                        _ => true,
                    };
                    if first {
                        d.bytes += c.size;
                    }
                }
                Kind::Link(Target::File) => {
                    d.files += 1;
                    d.others += 1;
                }
                Kind::Link(Target::Other) | Kind::Other => d.others += 1,
            }
        }
        d
    }
}

/// A folder's deep size (05 §6.3 phase 3).
#[derive(Debug, Clone, Default)]
struct Deep {
    /// What was counted below the folder (its own files are the digest's).
    below: Option<Totals>,
    /// Everything below was counted.
    complete: bool,
    /// From the process cache, not run again in this tree.
    cached: bool,
    /// A scan runs: its request id.
    running: Option<u64>,
    /// This tree has scanned it to its end (or to its budget): not again by itself.
    done: bool,
    /// A remote scan stopped at its budget: where Space resumes it.
    frontier: Option<Vec<String>>,
    not_answering: bool,
    /// The owner asked (Space): no budget, and H3 too.
    wanted: bool,
}

/// A request for a folder's listing in flight.
#[derive(Debug, Clone, Copy)]
struct Asked {
    id: u64,
    /// The owner's whole listing (→, Space, Cmd/Ctrl+R), not an automatic one.
    owner: bool,
}

#[derive(Debug, Clone)]
struct Node {
    path: String,
    name: String,
    parent: Option<usize>,
    /// Subfolder rows (folders and links to folders), by name regardless of case.
    children: Vec<usize>,
    link: bool,
    /// A dot name or the macOS hidden flag.
    hidden: bool,
    /// The root, a strict ancestor of the current folder, or the current folder.
    on_path: bool,
    /// The mount status the parent's listing gave.
    mnt: Mnt,
    digest: Option<Digest>,
    asked: Option<Asked>,
    /// Its listing failed (R2).
    failed: bool,
    not_answering: bool,
    /// Listed at least once in this tree (no row is listed twice by itself).
    listed_once: bool,
    open: bool,
    auto: bool,
    /// The owner opened or closed it: 05 §5 leaves it alone.
    touched: bool,
    /// 05 §5 tried it and the budget refused it.
    refused: bool,
    hidden_open: bool,
    fs: Option<crate::remote::treefs::FsStat>,
    fs_asked: bool,
    /// The `statvfs` has answered (or failed).
    fs_done: bool,
    deep: Deep,
    /// Off screen since then, with a scan running.
    left: Option<Instant>,
}

impl Node {
    fn new(path: String, name: String, parent: Option<usize>) -> Self {
        Node {
            hidden: name.starts_with('.'),
            path,
            name,
            parent,
            children: Vec::new(),
            link: false,
            on_path: false,
            mnt: Mnt::No,
            digest: None,
            asked: None,
            failed: false,
            not_answering: false,
            listed_once: false,
            open: false,
            auto: false,
            touched: false,
            refused: false,
            hidden_open: false,
            fs: None,
            fs_asked: false,
            fs_done: false,
            deep: Deep::default(),
            left: None,
        }
    }
}

/// What a request in flight was for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum For {
    Folder,
    Home,
    Mounts,
    List(usize),
    Statvfs(usize),
    /// `statvfs` of `/` (`0`) or `/System/Volumes/Data` (`1`) on a macOS remote (05 §4.1).
    MacFence(u8),
    Scan(usize),
    /// Enter on a link: its target resolved.
    Target,
}

/// Where the tree is in its first steps.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Stage {
    /// The folder, the home and the mount table are asked for.
    Resolving,
    /// Phase 1: the root and the path are being listed.
    Path,
    /// 05 §5 at this level, once the rows it reads are listed.
    Building(usize),
    Done,
    Failed(String),
}

/// One drawn row, before it is described.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Drawn {
    /// A folder's row: its first node and (on a compact chain) its last.
    Folder {
        head: usize,
        end: usize,
        depth: usize,
        dimmed: bool,
    },
    Hidden {
        parent: usize,
        count: usize,
        depth: usize,
    },
}

/// The tree of one F10 session.
pub struct Tree {
    fs: Box<dyn TreeFs>,
    host: String,
    user: Option<String>,
    params: Params,
    folder_in: String,
    stage: Stage,
    nodes: Vec<Node>,
    root: usize,
    current: usize,
    root_is_home: bool,
    home: Option<String>,
    folder: Option<Resolved>,
    home_got: Option<Option<Resolved>>,
    mounts: Option<MountMap>,
    /// `f_fsid` of `/` and `/System/Volumes/Data` on a macOS remote without a table.
    mac_fsids: [Option<u64>; 2],
    asks: HashMap<u64, For>,
    next_id: u64,
    /// Rows 05 §5 has added so far (its budget E).
    auto_added: usize,
    view: Option<View>,
    /// The bar's row as the view last named it.
    bar: Option<RowId>,
    entered: Option<String>,
    closed: bool,
    local: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct View {
    top: usize,
    height: usize,
}

impl Tree {
    /// Starts the tree on `fs`: the folder, the home (once per host) and the mount table are
    /// asked for at once; nothing is waited for here.
    pub fn open(fs: Box<dyn TreeFs>, open: Open) -> Tree {
        let mut tree = Tree {
            local: fs.is_local(),
            fs,
            host: open.host,
            user: open.user,
            params: open.params,
            folder_in: open.folder,
            stage: Stage::Resolving,
            nodes: Vec::new(),
            root: 0,
            current: 0,
            root_is_home: false,
            home: None,
            folder: None,
            home_got: None,
            mounts: None,
            mac_fsids: [None, None],
            asks: HashMap::new(),
            next_id: 1,
            auto_added: 0,
            view: None,
            bar: None,
            entered: None,
            closed: false,
        };
        let folder = tree.folder_in.clone();
        tree.ask(For::Folder, 0, Ask::Resolve(folder));
        match cache::home(&tree.host) {
            Some(home) => tree.home_got = Some(home),
            None => {
                let user = tree.user.clone();
                tree.ask(For::Home, 0, Ask::Home(user));
            }
        }
        tree.ask(For::Mounts, 0, Ask::Mounts);
        tree
    }

    /// The worker calls `wake` after every answer (the window's repaint).
    pub fn set_wake(&mut self, wake: Wake) {
        self.fs.set_wake(wake);
    }

    fn ask(&mut self, what: For, prio: u64, ask: Ask) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.asks.insert(id, what);
        self.fs.send(id, prio, ask);
        id
    }

    fn cancel(&mut self, id: u64) {
        if self.asks.remove(&id).is_some() {
            self.fs.cancel(id);
        }
    }

    /// Phase 1 is done: the rows exist.
    pub fn ready(&self) -> bool {
        matches!(self.stage, Stage::Building(_) | Stage::Done)
    }

    /// 05 §5 has run to its end: no row opens by itself any more.
    pub fn built(&self) -> bool {
        self.stage == Stage::Done
    }

    /// Why the tree has no rows and never will.
    pub fn error(&self) -> Option<&str> {
        match &self.stage {
            Stage::Failed(why) => Some(why),
            _ => None,
        }
    }

    /// The root's `host:/abs` (the frame's title); `None` before the root is known.
    pub fn title(&self) -> Option<String> {
        let root = &self.nodes.get(self.root)?.path;
        Some(if self.host.is_empty() {
            root.clone()
        } else {
            format!("{}:{root}", self.host)
        })
    }

    /// The current folder's row: where the bar starts.
    pub fn current(&self) -> Option<RowId> {
        self.ready().then(|| self.row_id_of(self.current)).flatten()
    }

    /// The status row's words, when the tree has something to say there.
    pub fn status(&self) -> Option<String> {
        if let Some(held) = self.fs.held() {
            return Some(held);
        }
        self.error().map(str::to_string)
    }

    fn row_id_of(&self, node: usize) -> Option<RowId> {
        self.drawn().into_iter().find_map(|d| match d {
            Drawn::Folder { head, end, .. } if self.chain_holds(head, end, node) => {
                Some(RowId::Folder(end as u32))
            }
            _ => None,
        })
    }

    /// The index of `id`'s row in [`Tree::rows`].
    pub fn row_of(&self, id: RowId) -> Option<usize> {
        self.drawn().iter().position(|d| self.id_of(d) == id)
    }

    fn id_of(&self, d: &Drawn) -> RowId {
        match *d {
            Drawn::Folder { end, .. } => RowId::Folder(end as u32),
            Drawn::Hidden { parent, .. } => RowId::Hidden(parent as u32),
        }
    }

    /// The node a folder row stands for (its chain's last folder).
    fn node_of(&self, id: RowId) -> Option<usize> {
        match id {
            RowId::Folder(n) => self.nodes.get(n as usize).map(|_| n as usize),
            RowId::Hidden(_) => None,
        }
    }

    /// The next folder of a compact chain at `at`: `at` holds one shown subfolder and nothing
    /// else (no files, links, special files or hidden subfolders), listed completely, is not on
    /// the path (the root, the folders down to the current one and the current one each keep a
    /// row of their own), and no rule keeps it closed (05 §3).
    fn chain_next(&self, at: usize) -> Option<usize> {
        let n = &self.nodes[at];
        let d = n.digest.as_ref()?;
        if n.on_path || n.link || !d.complete || self.rule(at).is_some() {
            return None;
        }
        if d.dirs != 1 || d.real_dirs != 1 || d.others != 0 || n.children.len() != 1 {
            return None;
        }
        let only = n.children[0];
        (!self.grouped(only)).then_some(only)
    }

    /// A hidden folder drawn in its folder's `· N hidden` row (one on the path never is).
    fn grouped(&self, i: usize) -> bool {
        self.nodes[i].hidden && !self.nodes[i].on_path
    }

    /// The drawn rows, top to bottom.
    fn drawn(&self) -> Vec<Drawn> {
        let mut out = Vec::new();
        if self.ready() {
            self.draw(self.root, 0, false, &mut out);
        }
        out
    }

    fn draw(&self, head: usize, depth: usize, dimmed: bool, out: &mut Vec<Drawn>) {
        let mut end = head;
        while let Some(next) = self.chain_next(end) {
            end = next;
        }
        out.push(Drawn::Folder {
            head,
            end,
            depth,
            dimmed,
        });
        let n = &self.nodes[end];
        if !n.open || n.link {
            return;
        }
        let mut hidden = Vec::new();
        for &c in &n.children {
            if self.grouped(c) {
                hidden.push(c);
            } else {
                self.draw(c, depth + 1, false, out);
            }
        }
        if hidden.is_empty() {
            return;
        }
        if n.hidden_open {
            for c in hidden {
                self.draw(c, depth + 1, true, out);
            }
        } else {
            out.push(Drawn::Hidden {
                parent: end,
                count: hidden.len(),
                depth: depth + 1,
            });
        }
    }

    /// The rows as the UI draws them.
    pub fn rows(&self) -> Vec<Row> {
        self.drawn().iter().map(|d| self.describe(d)).collect()
    }

    fn describe(&self, d: &Drawn) -> Row {
        match *d {
            Drawn::Hidden {
                parent,
                count,
                depth,
            } => Row {
                id: RowId::Hidden(parent as u32),
                depth,
                name: format!(
                    "· {count} hidden folder{}",
                    if count == 1 { "" } else { "s" }
                ),
                kind: RowKind::Hidden(count),
                glyph: Glyph::None,
                open: false,
                auto: false,
                dimmed: true,
                current: false,
                path: self.nodes[parent].path.clone(),
                info: Info::default(),
            },
            Drawn::Folder {
                head,
                end,
                depth,
                dimmed,
            } => {
                let mut names = Vec::new();
                let mut at = head;
                loop {
                    names.push(if at == self.root {
                        if self.root_is_home { "~" } else { "/" }.to_string()
                    } else {
                        self.nodes[at].name.clone()
                    });
                    if at == end {
                        break;
                    }
                    at = self.chain_next(at).unwrap_or(end);
                }
                let n = &self.nodes[end];
                let glyph = if n.link {
                    Glyph::Link
                } else if n.open {
                    Glyph::Open
                } else if n.digest.as_ref().is_some_and(|d| d.complete && d.dirs == 0) {
                    Glyph::None
                } else {
                    Glyph::Closed
                };
                Row {
                    id: RowId::Folder(end as u32),
                    depth,
                    name: names.join("/"),
                    kind: if n.link {
                        RowKind::Link
                    } else {
                        RowKind::Folder
                    },
                    glyph,
                    open: n.open && !n.link,
                    auto: n.auto,
                    dimmed,
                    current: end == self.current,
                    path: n.path.clone(),
                    info: self.info(end),
                }
            }
        }
    }

    /// Why `i` is never opened by itself (05 §4); `None` for the root and the path, which the
    /// rules do not apply to.
    fn rule(&self, i: usize) -> Option<Rule> {
        let n = &self.nodes[i];
        if n.on_path {
            return None;
        }
        if n.link {
            return Some(Rule::Link);
        }
        if let Some(class) = self.class(i) {
            return Some(Rule::Mount(class));
        }
        if n.failed {
            return Some(Rule::Unreadable);
        }
        if let Some(d) = &n.digest {
            if d.cachedir {
                return Some(Rule::CacheDir);
            }
            if d.git {
                return Some(Rule::Vcs);
            }
            if d.venv {
                return Some(Rule::Venv);
            }
        }
        if rules::vcs_name(&n.name) {
            return Some(Rule::Vcs);
        }
        if self.private(i) {
            return Some(Rule::Private);
        }
        if n.hidden {
            return Some(Rule::Hidden);
        }
        if rules::GENERATED.contains(&n.name.as_str()) {
            return Some(Rule::Generated);
        }
        None
    }

    /// H3: `~/Library` and `~/.Trash` on a macOS host.
    fn private(&self, i: usize) -> bool {
        let (Some(home), Some(m)) = (&self.home, &self.mounts) else {
            return false;
        };
        m.macos
            && self.nodes[i]
                .path
                .strip_prefix(home.trim_end_matches('/'))
                .is_some_and(|rest| rest == "/Library" || rest == "/.Trash")
    }

    /// What is mounted at `i` (R3–R7): the table (or a remote's names), the listing's own mount
    /// status, or a remote's `f_fsid` fence (05 §4.1).
    fn class(&self, i: usize) -> Option<Class> {
        let n = &self.nodes[i];
        if i == self.root || n.on_path {
            return None;
        }
        let m = self.mounts.as_ref()?;
        if let Some(class) = m.class(&n.path) {
            return Some(class);
        }
        match n.mnt {
            Mnt::Trigger => return Some(Class::Auto),
            Mnt::Point => return Some(Class::Disk),
            Mnt::No => {}
        }
        if m.fences {
            let parent = n.parent.and_then(|p| self.nodes[p].fs);
            if let (Some(own), Some(parent)) = (n.fs, parent) {
                let firm = m.macos && self.mac_fsids.contains(&Some(own.fsid));
                if own.fsid != parent.fsid && !firm {
                    return Some(Class::Disk);
                }
            }
        }
        None
    }

    /// The info line of `i`'s row.
    fn info(&self, i: usize) -> Info {
        let n = &self.nodes[i];
        let rule = self.rule(i);
        let mut info = Info::default();
        if let Some(d) = &n.digest {
            info.dirs = Some(Count {
                n: d.dirs,
                more: !d.complete,
            });
            info.files = Some(Count {
                n: d.files,
                more: !d.complete,
            });
        }
        let class = self.class(i);
        if class == Some(Class::Disk) {
            info.fs_files = n.fs.map(|f| f.used_files);
        }
        info.size = if n.not_answering || n.deep.not_answering {
            Size::Tag(Tag::NotAnswering)
        } else {
            match rule {
                Some(Rule::Link) => Size::Tag(Tag::Link),
                Some(Rule::Mount(Class::Sys)) => Size::Tag(Tag::Sys),
                Some(Rule::Mount(Class::Net)) => Size::Tag(Tag::Net),
                Some(Rule::Mount(Class::Auto)) => Size::Tag(Tag::Auto),
                Some(Rule::Mount(Class::SysVol)) => Size::Tag(Tag::SysVol),
                Some(Rule::Unreadable) => Size::Tag(Tag::Unreadable),
                Some(Rule::Mount(Class::Disk))
                    if n.deep.below.is_none() && n.deep.running.is_none() =>
                {
                    match n.fs {
                        Some(f) => Size::Fs {
                            used_bytes: f.used_bytes,
                        },
                        None => self.bytes(i),
                    }
                }
                _ if n.failed => Size::Tag(Tag::Unreadable),
                _ => self.bytes(i),
            }
        };
        info
    }

    /// The size column of `i` as bytes: its own files, and what is known below it.
    fn bytes(&self, i: usize) -> Size {
        let n = &self.nodes[i];
        if n.digest.is_none() {
            return Size::Unknown;
        }
        let (bytes, more) = self.total(i);
        Size::Bytes {
            bytes,
            more,
            cached: n.deep.cached && n.deep.running.is_none(),
            scanning: n.deep.running.is_some(),
        }
    }

    /// Bytes in and below `i` as far as known, and whether more may be below: a closed folder's
    /// own files plus its scan; an open one's own files plus its subfolders' totals (mounts
    /// below are another filesystem, links are not followed).
    fn total(&self, i: usize) -> (u64, bool) {
        let n = &self.nodes[i];
        let Some(d) = &n.digest else {
            return (0, true);
        };
        let mut more = !d.complete;
        let mut bytes = d.bytes;
        if d.real_dirs == 0 {
            return (bytes, more);
        }
        if let Some(below) = n.deep.below {
            return (
                bytes + below.bytes,
                more || !n.deep.complete || below.unread > 0,
            );
        }
        if !n.open {
            return (bytes, true);
        }
        for &c in &n.children {
            let child = &self.nodes[c];
            if child.link || self.class(c).is_some() {
                continue;
            }
            let (b, m) = self.total(c);
            bytes += b;
            more |= m;
        }
        // Subfolders that are not rows (names that are not UTF-8) are never counted.
        let rows = n.children.iter().filter(|&&c| !self.nodes[c].link).count() as u64;
        (bytes, more || rows < d.real_dirs)
    }

    /// The rows' node ids with their positions (the build and the phases read them).
    fn positions(&self) -> (Vec<Drawn>, Option<usize>) {
        let drawn = self.drawn();
        let at = drawn.iter().position(|d| match *d {
            Drawn::Folder { end, .. } => end == self.current,
            _ => false,
        });
        (drawn, at)
    }

    /// Tells the model what is on screen: rows `top..top + height`, the bar on `bar`.
    pub fn view(&mut self, top: usize, height: usize, bar: Option<RowId>) {
        self.view = Some(View {
            top,
            height: height.max(1),
        });
        self.set_bar(bar);
        self.pump();
    }

    /// Takes every answer that has come and applies it; `true` when anything changed.
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        while let Some(reply) = self.fs.next(std::time::Duration::ZERO) {
            changed |= self.apply(reply);
        }
        changed |= self.pump();
        changed
    }

    /// Waits up to `limit` for the next answer and applies it (tests).
    #[cfg(test)]
    pub(crate) fn poll_wait(&mut self, limit: std::time::Duration) -> bool {
        match self.fs.next(limit) {
            Some(reply) => {
                let changed = self.apply(reply);
                self.poll() || changed
            }
            None => self.poll(),
        }
    }

    /// → on a row: opens it (listing it whole if it never was, or only in part); on the hidden
    /// row, one dimmed row each. `false`: nothing to open (a link, an open row).
    pub fn expand(&mut self, id: RowId) -> bool {
        match id {
            RowId::Hidden(p) => {
                let Some(n) = self.nodes.get_mut(p as usize) else {
                    return false;
                };
                let was = n.hidden_open;
                n.hidden_open = true;
                !was
            }
            RowId::Folder(_) => {
                let Some(i) = self.node_of(id) else {
                    return false;
                };
                if self.nodes[i].link || self.nodes[i].open {
                    return false;
                }
                self.nodes[i].open = true;
                self.nodes[i].touched = true;
                if !self.nodes[i].digest.as_ref().is_some_and(|d| d.complete) {
                    self.list_with(i, 0, true);
                }
                self.pump();
                true
            }
        }
    }

    /// ← on a row: closes it; on a closed row (or a link, or the hidden row) the row to move the
    /// bar to — its parent's.
    pub fn collapse(&mut self, id: RowId) -> Option<RowId> {
        let parent_row = |tree: &Tree, node: usize| {
            let drawn = tree.drawn();
            // The row whose chain holds `node`'s parent.
            let parent = tree.nodes[node].parent?;
            drawn.iter().find_map(|d| match *d {
                Drawn::Folder { head, end, .. } if tree.chain_holds(head, end, parent) => {
                    Some(RowId::Folder(end as u32))
                }
                _ => None,
            })
        };
        match id {
            RowId::Hidden(p) => Some(RowId::Folder(p)),
            RowId::Folder(_) => {
                let i = self.node_of(id)?;
                if self.nodes[i].open && !self.nodes[i].link {
                    self.nodes[i].open = false;
                    self.nodes[i].touched = true;
                    self.pump();
                    return None;
                }
                let head = match self.drawn().into_iter().find(|d| self.id_of(d) == id) {
                    Some(Drawn::Folder { head, .. }) => head,
                    _ => i,
                };
                parent_row(self, head)
            }
        }
    }

    /// Whether the chain `head..=end` holds `node`.
    fn chain_holds(&self, head: usize, end: usize, node: usize) -> bool {
        let mut at = head;
        loop {
            if at == node {
                return true;
            }
            if at == end {
                return false;
            }
            match self.chain_next(at) {
                Some(next) => at = next,
                None => return false,
            }
        }
    }

    /// Space: sizes the row now — listed whole first when its listing stopped early, then
    /// scanned without a budget (an H3 row too; never R1, R2, R4–R7).
    pub fn size_now(&mut self, id: RowId) {
        let Some(i) = self.node_of(id) else {
            return;
        };
        if self.rule(i).is_some_and(|r| !r.sizable_on_space()) {
            return;
        }
        self.nodes[i].deep.wanted = true;
        self.nodes[i].deep.done = false;
        self.nodes[i].deep.not_answering = false;
        if !self.nodes[i].digest.as_ref().is_some_and(|d| d.complete) {
            self.list_with(i, 0, true);
        }
        self.size_wanted();
        self.pump();
    }

    /// Cmd/Ctrl+R: lists the row again (whole) and sizes it afresh.
    pub fn reread(&mut self, id: RowId) {
        let Some(i) = self.node_of(id) else {
            return;
        };
        if self.nodes[i].link {
            return;
        }
        if let Some(scan) = self.nodes[i].deep.running.take() {
            self.cancel(scan);
        }
        self.nodes[i].deep = Deep::default();
        self.nodes[i].fs_asked = false;
        self.nodes[i].fs_done = false;
        self.nodes[i].fs = None;
        self.list_with(i, 0, true);
        self.pump();
    }

    /// An owner's whole listing is running (Esc stops it before it closes the tree).
    pub fn owner_busy(&self) -> bool {
        self.nodes.iter().any(|n| n.asked.is_some_and(|a| a.owner))
    }

    /// Esc during an owner's whole listing: stops it; the row keeps what it showed.
    pub fn stop_owner(&mut self) {
        let owned: Vec<(usize, u64)> = self
            .nodes
            .iter()
            .enumerate()
            .filter_map(|(i, n)| n.asked.filter(|a| a.owner).map(|a| (i, a.id)))
            .collect();
        for (i, id) in owned {
            self.nodes[i].asked = None;
            self.cancel(id);
        }
    }

    /// Enter on a row: `Some(path)` — the folder to list in the panel (a chain's last); a link's
    /// target is resolved first and comes from [`Tree::entered`]; the hidden row opens.
    pub fn enter(&mut self, id: RowId) -> Option<String> {
        if let RowId::Hidden(_) = id {
            self.expand(id);
            return None;
        }
        let i = self.node_of(id)?;
        if self.nodes[i].link {
            let path = self.nodes[i].path.clone();
            self.ask(For::Target, 0, Ask::Resolve(path));
            return None;
        }
        Some(self.nodes[i].path.clone())
    }

    /// A link's target, resolved, once it has come (taken once).
    pub fn entered(&mut self) -> Option<String> {
        self.entered.take()
    }

    /// Closes the tree: every listing and scan stops, nothing is waited for.
    pub fn close(&mut self) {
        if self.closed {
            return;
        }
        self.closed = true;
        for i in 0..self.nodes.len() {
            if self.nodes[i].deep.running.is_some() {
                self.store_partial(i);
            }
        }
        self.asks.clear();
        self.fs.close();
    }

    /// Children of `i` from its listing: folders and links to folders, by name regardless of
    /// case; nodes that exist (the path, folders opened before a re-read) are kept.
    fn set_children(&mut self, i: usize, listing: &Listing) {
        let mut rows: Vec<&Child> = listing
            .children
            .iter()
            .filter(|c| !c.lossy && matches!(c.kind, Kind::Dir | Kind::Link(Target::Dir)))
            .collect();
        rows.sort_by(|a, b| {
            a.name
                .to_lowercase()
                .cmp(&b.name.to_lowercase())
                .then_with(|| a.name.cmp(&b.name))
        });
        let old: HashMap<String, usize> = self.nodes[i]
            .children
            .iter()
            .map(|&c| (self.nodes[c].name.clone(), c))
            .collect();
        let mut kids = Vec::with_capacity(rows.len());
        for c in rows {
            let k = match old.get(&c.name) {
                Some(&k) => k,
                None => {
                    let path = crate::remote::join(&self.nodes[i].path, &c.name);
                    self.nodes.push(Node::new(path, c.name.clone(), Some(i)));
                    let k = self.nodes.len() - 1;
                    self.cached_size(k);
                    k
                }
            };
            let n = &mut self.nodes[k];
            n.link = matches!(c.kind, Kind::Link(_));
            n.hidden = c.name.starts_with('.') || c.hidden;
            n.mnt = c.mnt;
            kids.push(k);
        }
        // A path folder the listing did not reach (it stopped early) is drawn all the same.
        for &k in old.values() {
            if self.nodes[k].on_path && !kids.contains(&k) {
                kids.push(k);
            }
        }
        self.nodes[i].children = kids;
    }

    /// Keeps a running scan's figure in the process cache as a partial result.
    fn store_partial(&mut self, i: usize) {
        let n = &self.nodes[i];
        if let Some(below) = n.deep.below {
            cache::store(&self.host, &n.path, below, false);
        }
    }

    /// Applies one answer.
    fn apply(&mut self, reply: crate::remote::treefs::Reply) -> bool {
        use crate::remote::treefs::Got;
        let Some(&what) = self.asks.get(&reply.id) else {
            return false;
        };
        let last = !matches!(
            reply.got,
            Got::NotAnswering | Got::Scan(crate::remote::treefs::Scanned { end: None, .. })
        );
        if last {
            self.asks.remove(&reply.id);
        }
        match (what, reply.got) {
            (For::Folder, Got::Resolved(r)) => match r {
                Ok(r) => self.folder = Some(r),
                Err(e) => self.stage = Stage::Failed(e),
            },
            // A local call that does not answer is abandoned and never answers: phase 1 goes on
            // without it. A remote one keeps waiting for its answer or its timeout.
            (For::Folder, Got::NotAnswering) if self.local => {
                self.stage = Stage::Failed(format!("{}: not answering", self.folder_in));
            }
            (For::Folder, Got::NotAnswering) => {}
            (For::Home, Got::Home(r)) => {
                // Only a home that was read is kept for the host: a failure is asked again by
                // the next tree.
                let home = r.ok();
                if let Some(h) = &home {
                    cache::set_home(&self.host, Some(h.clone()));
                }
                self.home_got = Some(home);
            }
            (For::Home, Got::NotAnswering) if self.local => self.home_got = Some(None),
            (For::Home, Got::NotAnswering) => {}
            (For::Mounts, Got::NotAnswering) if self.local => {
                let none = crate::remote::treefs::Mounts {
                    table: Some(Vec::new()),
                    macos: cfg!(target_os = "macos"),
                    statvfs: true,
                };
                self.mounts = Some(MountMap::new(&none, false));
            }
            (For::Mounts, Got::Mounts(m)) => self.mounts = Some(MountMap::new(&m, !self.local)),
            (For::Target, Got::Resolved(Ok(r))) => self.entered = Some(r.path),
            (For::List(i), Got::Listed(r)) => {
                let n = &mut self.nodes[i];
                if n.asked.is_some_and(|a| a.id == reply.id) {
                    n.asked = None;
                }
                n.not_answering = false;
                match r {
                    Ok(listing) => {
                        n.failed = false;
                        n.digest = Some(Digest::of(&listing));
                        self.set_children(i, &listing);
                        if self.nodes[i].deep.wanted {
                            self.size_wanted();
                        }
                    }
                    // Unreadable (R2): what an earlier listing showed stays, and so does the path
                    // through it.
                    Err(_) => n.failed = true,
                }
            }
            (For::List(i), Got::NotAnswering) => self.nodes[i].not_answering = true,
            (For::Statvfs(i), Got::Statvfs(r)) => {
                self.nodes[i].fs = r.ok();
                self.nodes[i].fs_done = true;
            }
            (For::Statvfs(i), Got::NotAnswering) => self.nodes[i].fs_done = true,
            (For::MacFence(k), Got::Statvfs(r)) => {
                self.mac_fsids[k as usize] = r.ok().map(|f| f.fsid);
            }
            (For::Scan(i), Got::Scan(s)) => {
                let n = &mut self.nodes[i];
                if n.deep.running != Some(reply.id) {
                    return false;
                }
                // A scan started afresh shows what an earlier one reached until it passes it.
                let behind =
                    s.end.is_none() && n.deep.below.is_some_and(|b| b.bytes > s.totals.bytes);
                if !behind {
                    n.deep.below = Some(s.totals);
                    n.deep.complete = false;
                }
                n.deep.cached = false;
                if let Some(end) = s.end {
                    n.deep.below = Some(s.totals);
                    n.deep.running = None;
                    n.deep.done = true;
                    n.deep.complete = end == ScanEnd::Complete;
                    n.deep.frontier = match end {
                        ScanEnd::Budget(f) => Some(f),
                        ScanEnd::Complete => None,
                    };
                    let (path, complete) = (n.path.clone(), n.deep.complete);
                    cache::store(&self.host, &path, s.totals, complete);
                }
            }
            (For::Scan(i), Got::NotAnswering) => {
                let n = &mut self.nodes[i];
                if n.deep.running != Some(reply.id) {
                    return false;
                }
                n.deep.not_answering = true;
                n.deep.done = true;
                n.deep.running = None;
                self.cancel(reply.id);
            }
            _ => {}
        }
        true
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        self.close();
    }
}
