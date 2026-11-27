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
//!   [`SftpRemote::into_tree_renewing`] on the tree's own connection (`Connect`, 02 §6), given a
//!   closure that opens a fresh one: the worker renews its connection once 32 handles may have
//!   been left open on the server ([`SftpRemote::into_tree`] never renews); `Open` names the
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
//!   an owner's whole listing or Space's scan (05 §6.3) before it closes the tree.
//! - **Status row**: [`Tree::status`] (the watchdog's cap: `4 calls not answering; …`).
//! - **Close**: [`Tree::close`] (or drop) stops every scan and listing and never waits.
//!
//! [`LocalFs::tree`]: crate::remote::LocalFs::tree
//! [`SftpRemote::into_tree`]: crate::remote::SftpRemote::into_tree
//! [`SftpRemote::into_tree_renewing`]: crate::remote::SftpRemote::into_tree_renewing

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
    /// Of `bytes`, those of the files with more than one link.
    multi_bytes: u64,
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
                // A link whose target was not read is drawn as a link to a folder (05 §4 R1).
                Kind::Link(Target::Dir | Target::Unread) => {
                    d.dirs += 1;
                    d.others += 1;
                }
                Kind::File => {
                    d.files += 1;
                    d.others += 1;
                    let (first, multi) = match c.id {
                        Some(id) if c.nlink > 1 => {
                            d.multi.push(id);
                            (seen.insert(id), true)
                        }
                        _ => (true, false),
                    };
                    if first {
                        d.bytes += c.size;
                        if multi {
                            d.multi_bytes += c.size;
                        }
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
    /// Its id while the tree is open, never given to another folder ([`RowId`]).
    id: u32,
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
    /// The `statvfs` has not answered for 2 s (it may still).
    fs_late: bool,
    deep: Deep,
    /// Off screen since then, with a scan running.
    left: Option<Instant>,
}

impl Node {
    fn new(path: String, name: String, parent: Option<usize>) -> Self {
        Node {
            id: 0,
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
            fs_late: false,
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
    /// Folder nodes by slot; a slot a refresh freed is used again (`free`).
    nodes: Vec<Node>,
    free: Vec<usize>,
    /// Node id → slot, for the ids of [`RowId`].
    ids: HashMap<u32, usize>,
    next_node: u32,
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
            free: Vec::new(),
            ids: HashMap::new(),
            next_node: 1,
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

    /// A request of the tree is on its way — a listing (of the screen's rows, of a screen above
    /// and below them, of the path), a filesystem's figure, a scan — other than those reported not
    /// answering: while none is, nothing the tree shows changes until the owner acts or another
    /// row comes on screen (a `SOPEN_CELLS` receipt waits for that, X4-tree). One reported not
    /// answering is left out: locally it never answers (its call is abandoned, 05 §6.2, and no
    /// longer asked: [`Tree::apply`]); remotely it may not answer before `SOPEN_WATCHDOG` (it
    /// waits out its request timeout, 30 s) — its row says `not answering` meanwhile. Phase 1's
    /// requests count until they answer.
    pub fn in_flight(&self) -> bool {
        self.asks.values().any(|&what| match what {
            For::List(i) => !self.nodes[i].not_answering,
            For::Statvfs(i) => !self.nodes[i].fs_late,
            For::Scan(i) => !self.nodes[i].deep.not_answering,
            For::Folder | For::Home | For::Mounts | For::MacFence(_) | For::Target => true,
        })
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
                Some(RowId::Folder(self.nodes[end].id))
            }
            _ => None,
        })
    }

    /// Puts `node` in a free slot (or a new one) under a new id.
    fn add_node(&mut self, mut node: Node) -> usize {
        node.id = self.next_node;
        self.next_node += 1;
        let id = node.id;
        let slot = match self.free.pop() {
            Some(slot) => {
                self.nodes[slot] = node;
                slot
            }
            None => {
                self.nodes.push(node);
                self.nodes.len() - 1
            }
        };
        self.ids.insert(id, slot);
        slot
    }

    /// Frees `i` and every folder below it (a refresh no longer holds them): their requests
    /// are cancelled, their ids retired, their slots given to the next new folders.
    fn free_node(&mut self, i: usize) {
        let mut gone = vec![i];
        let mut at = 0;
        while at < gone.len() {
            gone.extend(self.nodes[gone[at]].children.iter().copied());
            at += 1;
        }
        let asks: Vec<u64> = self
            .asks
            .iter()
            .filter(|(_, what)| match what {
                For::List(k) | For::Statvfs(k) | For::Scan(k) => gone.contains(k),
                _ => false,
            })
            .map(|(id, _)| *id)
            .collect();
        for id in asks {
            self.cancel(id);
        }
        for k in gone {
            let id = self.nodes[k].id;
            self.ids.remove(&id);
            self.nodes[k] = Node::new(String::new(), String::new(), None);
            self.free.push(k);
        }
    }

    /// The index of `id`'s row in [`Tree::rows`].
    pub fn row_of(&self, id: RowId) -> Option<usize> {
        self.drawn().iter().position(|d| self.id_of(d) == id)
    }

    fn id_of(&self, d: &Drawn) -> RowId {
        match *d {
            Drawn::Folder { end, .. } => RowId::Folder(self.nodes[end].id),
            Drawn::Hidden { parent, .. } => RowId::Hidden(self.nodes[parent].id),
        }
    }

    /// The node a folder row stands for (its chain's last folder).
    fn node_of(&self, id: RowId) -> Option<usize> {
        match id {
            RowId::Folder(n) => self.ids.get(&n).copied(),
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
                id: RowId::Hidden(self.nodes[parent].id),
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
                    id: RowId::Folder(self.nodes[end].id),
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

    /// Why `i` is never opened by itself, and what of it is never read without the owner
    /// (05 §4). The root has none. On the path the rules hold as well — a path folder is opened
    /// by rule 1 all the same, and is listed only where its rule allows (05 §6.3) — but for R3
    /// ("not on the path") and H1 ("the path overrides it"). A path folder below an R4–R6 mount
    /// on the path is held back with it: a lookup there is what those rules avoid.
    fn rule(&self, i: usize) -> Option<Rule> {
        if i == self.root {
            return None;
        }
        let n = &self.nodes[i];
        if n.link {
            return Some(Rule::Link);
        }
        match self.mount_class(i) {
            Some(Class::Disk) if n.on_path => {}
            Some(class) => return Some(Rule::Mount(class)),
            None => {}
        }
        if let Some(class) = self.held_below(i) {
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
        if n.hidden && !n.on_path {
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

    /// The class of an R4–R6 mount a path folder lies below (not the root's own: a home on a
    /// network filesystem is the tree's root, and listed).
    fn held_below(&self, i: usize) -> Option<Class> {
        if !self.nodes[i].on_path {
            return None;
        }
        let mut at = self.nodes[i].parent;
        while let Some(p) = at {
            if p == self.root {
                return None;
            }
            if let Some(c @ (Class::Sys | Class::Net | Class::Auto)) = self.mount_class(p) {
                return Some(c);
            }
            at = self.nodes[p].parent;
        }
        None
    }

    /// What is mounted at `i` (R3–R7), on the path too: the table (or a remote's names), the
    /// listing's own mount status, or a remote's `f_fsid` fence (05 §4.1). Never the root's.
    fn mount_class(&self, i: usize) -> Option<Class> {
        let n = &self.nodes[i];
        if i == self.root {
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
        let disk = rule == Some(Rule::Mount(Class::Disk));
        if disk {
            info.fs_files = n.fs.map(|f| f.used_files);
        }
        // A late `statvfs` shows wherever it is needed: an R3 row's figure, or any row's fence
        // on a remote without a table (05 §4.1).
        let fences = self.mounts.as_ref().is_some_and(|m| m.fences);
        let fs_late =
            (disk || fences) && n.fs_late && n.deep.below.is_none() && n.deep.running.is_none();
        info.size = if n.not_answering || n.deep.not_answering || fs_late {
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
        let (bytes, _, more) = self.total(i);
        Size::Bytes {
            bytes,
            more,
            cached: n.deep.cached && n.deep.running.is_none(),
            scanning: n.deep.running.is_some(),
        }
    }

    /// Bytes in and below `i` as far as known — of them, those of files with more than one
    /// link — and whether more may be below. A closed folder: its own files plus its scan (one
    /// set of inodes: the scan was seeded with the folder's own). An open one: its own files
    /// plus its subfolders' totals (mounts below are another filesystem, links are not
    /// followed) — where a file hard-linked into two of them would be counted twice, so the
    /// files with one link are summed (each has one name, so one place) and of the multi-linked
    /// only the largest set is taken: a lower bound that never exceeds the true size, with `+`
    /// when it may be short (05 §6.1).
    fn total(&self, i: usize) -> (u64, u64, bool) {
        let n = &self.nodes[i];
        let Some(d) = &n.digest else {
            return (0, 0, true);
        };
        let more = !d.complete;
        if d.real_dirs == 0 {
            return (d.bytes, d.multi_bytes, more);
        }
        if let Some(below) = n.deep.below {
            return (
                d.bytes + below.bytes,
                d.multi_bytes + below.multi,
                more || !n.deep.complete || below.unread > 0 || below.partial > 0,
            );
        }
        if !n.open {
            return (d.bytes, d.multi_bytes, true);
        }
        let mut single = d.bytes - d.multi_bytes;
        let mut sets = vec![d.multi_bytes];
        let mut more = more;
        for &c in &n.children {
            if self.nodes[c].link || self.mount_class(c).is_some() {
                continue;
            }
            let (b, m, mo) = self.total(c);
            single += b - m;
            sets.push(m);
            more |= mo;
        }
        let multi = sets.iter().copied().max().unwrap_or(0);
        let shared = sets.iter().filter(|&&m| m > 0).count() > 1;
        // Subfolders that are not rows (names that are not UTF-8) are never counted.
        let rows = n.children.iter().filter(|&&c| !self.nodes[c].link).count() as u64;
        (single + multi, multi, more || shared || rows < d.real_dirs)
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
                let Some(n) = self.ids.get(&p).map(|&slot| &mut self.nodes[slot]) else {
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
                    Some(RowId::Folder(tree.nodes[end].id))
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
        // An automatic scan of the row gives way: Space's lists every folder whole (D-6). What
        // it reached stays shown until Space's passes it.
        if let (false, Some(id)) = (self.nodes[i].deep.wanted, self.nodes[i].deep.running) {
            self.stop_scan(i, id);
            self.nodes[i].deep.frontier = None;
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
        self.nodes[i].fs_late = false;
        self.nodes[i].fs = None;
        self.list_with(i, 0, true);
        self.pump();
    }

    /// An owner's whole listing — or Space's scan, which lists every folder whole — is running
    /// (Esc stops it before it closes the tree).
    pub fn owner_busy(&self) -> bool {
        self.nodes
            .iter()
            .any(|n| n.asked.is_some_and(|a| a.owner) || n.deep.wanted && n.deep.running.is_some())
    }

    /// Esc during an owner's whole listing or Space's scan: stops it; the row keeps what it
    /// showed (a scan's figure stays, `+`, and is not started again by itself).
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
        for i in 0..self.nodes.len() {
            let deep = &self.nodes[i].deep;
            if let (true, Some(id)) = (deep.wanted, deep.running) {
                self.stop_scan(i, id);
                let deep = &mut self.nodes[i].deep;
                deep.wanted = false;
                deep.done = true;
            }
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

    /// The node slots held (tests: a refresh frees what it detached).
    #[cfg(test)]
    pub(crate) fn slots(&self) -> usize {
        self.nodes.len()
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
            .filter(|c| {
                !c.lossy && matches!(c.kind, Kind::Dir | Kind::Link(Target::Dir | Target::Unread))
            })
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
                    let k = self.add_node(Node::new(path, c.name.clone(), Some(i)));
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
        let mut gone = Vec::new();
        for &k in old.values() {
            if kids.contains(&k) {
                continue;
            }
            if self.nodes[k].on_path {
                kids.push(k);
            } else {
                gone.push(k);
            }
        }
        self.nodes[i].children = kids;
        // A folder no longer there: its requests stop and its slot is used again.
        for k in gone {
            self.free_node(k);
        }
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
        // A local call reported not answering has been abandoned: its late answer is dropped
        // (`watchdog`), so the report is its last. A remote one goes on waiting for its answer.
        let last = !matches!(
            reply.got,
            Got::NotAnswering | Got::Scan(crate::remote::treefs::Scanned { end: None, .. })
        ) || (self.local && matches!(reply.got, Got::NotAnswering));
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
                let n = &mut self.nodes[i];
                n.fs = r.ok();
                n.fs_done = true;
                n.fs_late = false;
                // Space waited for the fence (05 §4.1).
                if n.deep.wanted {
                    self.size_wanted();
                }
            }
            (For::Statvfs(i), Got::NotAnswering) => self.nodes[i].fs_late = true,
            (For::MacFence(k), Got::Statvfs(r)) => {
                self.mac_fsids[k as usize] = r.ok().map(|f| f.fsid);
            }
            (For::Scan(i), Got::Scan(s)) => {
                let n = &mut self.nodes[i];
                if n.deep.running != Some(reply.id) {
                    return false;
                }
                n.deep.not_answering = false;
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
                // Locally the call is abandoned and the scan has stopped; remotely it goes on
                // waiting (the row says so until the next figure).
                if self.local {
                    n.deep.done = true;
                    n.deep.running = None;
                    self.cancel(reply.id);
                }
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
