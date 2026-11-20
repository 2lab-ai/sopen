//! The journal's tests. Durability is checked by stopping the process — or cutting the power —
//! before every step a journal takes, on [`MemFs`]: an in-memory filesystem that keeps what was
//! made durable (file data after its fsync, a directory's entries after the directory's fsync)
//! apart from what was only written. The real disk's modes, links and locks are checked on
//! [`Disk`]; Restore's checks and the reconciliation of a cut-off Save against [`FakeRemote`].

use super::*;
use std::collections::{BTreeSet, HashMap};
use std::path::Component;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::UNIX_EPOCH;

const HOST: &str = "box";
const PATH: &str = "/data/model.bin";
const TEMP: &str = "/data/.model.bin.sopen-0123abcd";
const MEM_CACHE: &str = "/cache";

fn at(secs: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(secs)
}

// ---------------------------------------------------------------------------------------------
// MemFs: durable and volatile state kept apart.

#[derive(Clone, Debug)]
enum Inode {
    File {
        data: Vec<u8>,
        /// What a power loss leaves: the data as of its last fsync (`None`: never synced).
        durable: Option<Vec<u8>>,
    },
    Dir {
        entries: BTreeMap<String, usize>,
        /// The entries as of the directory's last fsync.
        durable: BTreeMap<String, usize>,
    },
}

#[derive(Default)]
struct MemState {
    nodes: Vec<Inode>,
    locks: BTreeSet<usize>,
}

#[derive(Clone)]
struct MemFs(Arc<Mutex<MemState>>);

fn no(kind: io::ErrorKind, path: &Path) -> io::Error {
    io::Error::new(kind, path.display().to_string())
}

fn names(path: &Path) -> Vec<String> {
    path.components()
        .filter_map(|c| match c {
            Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect()
}

impl MemState {
    fn lookup(&self, path: &Path) -> io::Result<usize> {
        let mut id = 0;
        for name in names(path) {
            match &self.nodes[id] {
                Inode::Dir { entries, .. } => {
                    id = *entries
                        .get(&name)
                        .ok_or_else(|| no(io::ErrorKind::NotFound, path))?
                }
                Inode::File { .. } => return Err(no(io::ErrorKind::NotADirectory, path)),
            }
        }
        Ok(id)
    }

    fn parent(&self, path: &Path) -> io::Result<(usize, String)> {
        let name = path
            .file_name()
            .ok_or_else(|| no(io::ErrorKind::InvalidInput, path))?
            .to_string_lossy()
            .into_owned();
        let dir = self.lookup(path.parent().unwrap_or(Path::new("/")))?;
        match &self.nodes[dir] {
            Inode::Dir { .. } => Ok((dir, name)),
            Inode::File { .. } => Err(no(io::ErrorKind::NotADirectory, path)),
        }
    }

    fn entries(&mut self, dir: usize) -> &mut BTreeMap<String, usize> {
        match &mut self.nodes[dir] {
            Inode::Dir { entries, .. } => entries,
            Inode::File { .. } => unreachable!("checked to be a directory"),
        }
    }

    fn add(&mut self, path: &Path, inode: Inode) -> io::Result<usize> {
        let (dir, name) = self.parent(path)?;
        if self.entries(dir).contains_key(&name) {
            return Err(no(io::ErrorKind::AlreadyExists, path));
        }
        self.nodes.push(inode);
        let id = self.nodes.len() - 1;
        self.entries(dir).insert(name, id);
        Ok(id)
    }
}

struct MemLock(Arc<Mutex<MemState>>, usize);

impl Drop for MemLock {
    fn drop(&mut self) {
        self.0.lock().unwrap().locks.remove(&self.1);
    }
}

impl MemFs {
    fn new() -> MemFs {
        let root = Inode::Dir {
            entries: BTreeMap::new(),
            durable: BTreeMap::new(),
        };
        MemFs(Arc::new(Mutex::new(MemState {
            nodes: vec![root],
            locks: BTreeSet::new(),
        })))
    }

    /// A filesystem whose cache root exists and is durable.
    fn with_cache() -> MemFs {
        let fs = MemFs::new();
        fs.create_dir(Path::new(MEM_CACHE)).unwrap();
        fs.sync_dir(Path::new("/")).unwrap();
        fs
    }

    /// The process was killed: everything it did stays, its locks are gone.
    fn killed(&self) -> MemFs {
        let state = self.0.lock().unwrap();
        MemFs(Arc::new(Mutex::new(MemState {
            nodes: state.nodes.clone(),
            locks: BTreeSet::new(),
        })))
    }

    /// The power was cut. A file keeps only the data of its last fsync (a file never synced is
    /// torn: half of it). With `durable_entries`, a directory keeps only the entries of its last
    /// fsync, so what was created, renamed or removed since is undone; without, every rename
    /// reached the disk but the data behind it may not have (the reordering a journaling
    /// filesystem allows).
    fn power_cut(&self, durable_entries: bool) -> MemFs {
        fn copy(
            old: &MemState,
            id: usize,
            durable_entries: bool,
            new: &mut MemState,
            seen: &mut HashMap<usize, usize>,
        ) -> usize {
            if let Some(&done) = seen.get(&id) {
                return done;
            }
            let slot = new.nodes.len();
            new.nodes.push(Inode::File {
                data: Vec::new(),
                durable: None,
            });
            seen.insert(id, slot);
            let inode = match &old.nodes[id] {
                Inode::File { data, durable } => {
                    let kept = durable
                        .clone()
                        .unwrap_or_else(|| data[..data.len() / 2].to_vec());
                    Inode::File {
                        data: kept.clone(),
                        durable: Some(kept),
                    }
                }
                Inode::Dir { entries, durable } => {
                    let from = if durable_entries { durable } else { entries };
                    let mut out = BTreeMap::new();
                    for (name, &child) in from {
                        out.insert(name.clone(), copy(old, child, durable_entries, new, seen));
                    }
                    Inode::Dir {
                        entries: out.clone(),
                        durable: out,
                    }
                }
            };
            new.nodes[slot] = inode;
            slot
        }
        let old = self.0.lock().unwrap();
        let mut new = MemState::default();
        copy(&old, 0, durable_entries, &mut new, &mut HashMap::new());
        MemFs(Arc::new(Mutex::new(new)))
    }

    fn exists(&self, path: &Path) -> bool {
        self.0.lock().unwrap().lookup(path).is_ok()
    }
}

impl Fs for MemFs {
    fn create_dir(&self, path: &Path) -> io::Result<()> {
        let dir = Inode::Dir {
            entries: BTreeMap::new(),
            durable: BTreeMap::new(),
        };
        self.0.lock().unwrap().add(path, dir).map(|_| ())
    }

    fn write_new(&self, path: &Path, bytes: &[u8]) -> io::Result<()> {
        let file = Inode::File {
            data: bytes.to_vec(),
            durable: None,
        };
        self.0.lock().unwrap().add(path, file).map(|_| ())
    }

    fn sync_file(&self, path: &Path) -> io::Result<()> {
        let mut state = self.0.lock().unwrap();
        let id = state.lookup(path)?;
        match &mut state.nodes[id] {
            Inode::File { data, durable } => {
                *durable = Some(data.clone());
                Ok(())
            }
            Inode::Dir { .. } => Err(no(io::ErrorKind::IsADirectory, path)),
        }
    }

    fn sync_dir(&self, path: &Path) -> io::Result<()> {
        let mut state = self.0.lock().unwrap();
        let id = state.lookup(path)?;
        match &mut state.nodes[id] {
            Inode::Dir { entries, durable } => {
                *durable = entries.clone();
                Ok(())
            }
            Inode::File { .. } => Err(no(io::ErrorKind::NotADirectory, path)),
        }
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        let mut state = self.0.lock().unwrap();
        let (src_dir, src) = state.parent(from)?;
        let (dst_dir, dst) = state.parent(to)?;
        let id = *state
            .entries(src_dir)
            .get(&src)
            .ok_or_else(|| no(io::ErrorKind::NotFound, from))?;
        if let Some(&there) = state.entries(dst_dir).get(&dst) {
            let moving_dir = matches!(state.nodes[id], Inode::Dir { .. });
            match &state.nodes[there] {
                Inode::Dir { entries, .. } if !moving_dir || !entries.is_empty() => {
                    return Err(no(io::ErrorKind::DirectoryNotEmpty, to));
                }
                Inode::File { .. } if moving_dir => {
                    return Err(no(io::ErrorKind::NotADirectory, to));
                }
                _ => {}
            }
        }
        state.entries(src_dir).remove(&src);
        state.entries(dst_dir).insert(dst, id);
        Ok(())
    }

    fn remove_file(&self, path: &Path) -> io::Result<()> {
        let mut state = self.0.lock().unwrap();
        let (dir, name) = state.parent(path)?;
        let id = *state
            .entries(dir)
            .get(&name)
            .ok_or_else(|| no(io::ErrorKind::NotFound, path))?;
        if matches!(state.nodes[id], Inode::Dir { .. }) {
            return Err(no(io::ErrorKind::IsADirectory, path));
        }
        state.entries(dir).remove(&name);
        Ok(())
    }

    fn remove_dir(&self, path: &Path) -> io::Result<()> {
        let mut state = self.0.lock().unwrap();
        let (dir, name) = state.parent(path)?;
        let id = *state
            .entries(dir)
            .get(&name)
            .ok_or_else(|| no(io::ErrorKind::NotFound, path))?;
        match &state.nodes[id] {
            Inode::Dir { entries, .. } if entries.is_empty() => {}
            Inode::Dir { .. } => return Err(no(io::ErrorKind::DirectoryNotEmpty, path)),
            Inode::File { .. } => return Err(no(io::ErrorKind::NotADirectory, path)),
        }
        state.entries(dir).remove(&name);
        Ok(())
    }

    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        let state = self.0.lock().unwrap();
        match &state.nodes[state.lookup(path)?] {
            Inode::File { data, .. } => Ok(data.clone()),
            Inode::Dir { .. } => Err(no(io::ErrorKind::IsADirectory, path)),
        }
    }

    fn list(&self, dir: &Path) -> io::Result<Vec<String>> {
        let state = self.0.lock().unwrap();
        match &state.nodes[state.lookup(dir)?] {
            Inode::Dir { entries, .. } => Ok(entries.keys().cloned().collect()),
            Inode::File { .. } => Err(no(io::ErrorKind::NotADirectory, dir)),
        }
    }

    fn node(&self, path: &Path, _follow: bool) -> io::Result<Option<Node>> {
        let state = self.0.lock().unwrap();
        match state.lookup(path) {
            Ok(id) => Ok(Some(match state.nodes[id] {
                Inode::Dir { .. } => Node::Dir,
                Inode::File { .. } => Node::File,
            })),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    fn lock(&self, path: &Path, create: bool) -> io::Result<Option<Held>> {
        let mut state = self.0.lock().unwrap();
        let id = if create {
            let file = Inode::File {
                data: Vec::new(),
                durable: None,
            };
            state.add(path, file)?
        } else {
            state.lookup(path)?
        };
        if !state.locks.insert(id) {
            return Ok(None);
        }
        Ok(Some(Held::new(MemLock(self.0.clone(), id))))
    }
}

// ---------------------------------------------------------------------------------------------
// Seams around any Fs: a process that dies before its k-th step, and one that never syncs.

fn killed() -> io::Error {
    io::Error::other("killed (test)")
}

/// Runs `left` steps (writes, fsyncs, renames, removes, locks), then dies: that step and every
/// call after it fails, as if the process had been killed right before it.
struct Dying {
    inner: Arc<dyn Fs>,
    left: AtomicUsize,
    dead: AtomicBool,
    log: Mutex<Vec<String>>,
}

impl Dying {
    fn new(inner: Arc<dyn Fs>, left: usize) -> Arc<Dying> {
        Arc::new(Dying {
            inner,
            left: AtomicUsize::new(left),
            dead: AtomicBool::new(false),
            log: Mutex::new(Vec::new()),
        })
    }

    fn step(&self, what: &str, path: &Path) -> io::Result<()> {
        self.alive()?;
        if self.left.load(Ordering::SeqCst) == 0 {
            self.dead.store(true, Ordering::SeqCst);
            return Err(killed());
        }
        self.left.fetch_sub(1, Ordering::SeqCst);
        self.log
            .lock()
            .unwrap()
            .push(format!("{what} {}", path.display()));
        Ok(())
    }

    fn alive(&self) -> io::Result<()> {
        if self.dead.load(Ordering::SeqCst) {
            return Err(killed());
        }
        Ok(())
    }

    fn ops(&self) -> Vec<String> {
        self.log.lock().unwrap().clone()
    }
}

impl Fs for Dying {
    fn create_dir(&self, path: &Path) -> io::Result<()> {
        self.step("mkdir", path)?;
        self.inner.create_dir(path)
    }
    fn write_new(&self, path: &Path, bytes: &[u8]) -> io::Result<()> {
        self.step("write", path)?;
        self.inner.write_new(path, bytes)
    }
    fn sync_file(&self, path: &Path) -> io::Result<()> {
        self.step("fsync", path)?;
        self.inner.sync_file(path)
    }
    fn sync_dir(&self, path: &Path) -> io::Result<()> {
        self.step("fsyncdir", path)?;
        self.inner.sync_dir(path)
    }
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        self.step("rename", from)?;
        self.inner.rename(from, to)
    }
    fn remove_file(&self, path: &Path) -> io::Result<()> {
        self.step("rm", path)?;
        self.inner.remove_file(path)
    }
    fn remove_dir(&self, path: &Path) -> io::Result<()> {
        self.step("rmdir", path)?;
        self.inner.remove_dir(path)
    }
    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        self.alive()?;
        self.inner.read(path)
    }
    fn list(&self, dir: &Path) -> io::Result<Vec<String>> {
        self.alive()?;
        self.inner.list(dir)
    }
    fn node(&self, path: &Path, follow: bool) -> io::Result<Option<Node>> {
        self.alive()?;
        self.inner.node(path, follow)
    }
    fn lock(&self, path: &Path, create: bool) -> io::Result<Option<Held>> {
        self.step("lock", path)?;
        self.inner.lock(path, create)
    }
}

/// The negative control: every fsync answers Ok and does nothing.
struct NoSync(Arc<dyn Fs>);

impl Fs for NoSync {
    fn create_dir(&self, path: &Path) -> io::Result<()> {
        self.0.create_dir(path)
    }
    fn write_new(&self, path: &Path, bytes: &[u8]) -> io::Result<()> {
        self.0.write_new(path, bytes)
    }
    fn sync_file(&self, _path: &Path) -> io::Result<()> {
        Ok(())
    }
    fn sync_dir(&self, _path: &Path) -> io::Result<()> {
        Ok(())
    }
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        self.0.rename(from, to)
    }
    fn remove_file(&self, path: &Path) -> io::Result<()> {
        self.0.remove_file(path)
    }
    fn remove_dir(&self, path: &Path) -> io::Result<()> {
        self.0.remove_dir(path)
    }
    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        self.0.read(path)
    }
    fn list(&self, dir: &Path) -> io::Result<Vec<String>> {
        self.0.list(dir)
    }
    fn node(&self, path: &Path, follow: bool) -> io::Result<Option<Node>> {
        self.0.node(path, follow)
    }
    fn lock(&self, path: &Path, create: bool) -> io::Result<Option<Held>> {
        self.0.lock(path, create)
    }
}

// ---------------------------------------------------------------------------------------------
// Snapshots the scenarios publish.

fn fp(size: u64, secs: u64, sha: Option<Sha>) -> Fingerprint {
    Fingerprint {
        size,
        mtime: at(secs),
        sha256: sha,
    }
}

fn hex_snap(i: u64) -> Snapshot {
    Snapshot {
        baseline: fp(1_000_000, 1_794_389_076, None),
        seen: Seen::pages((1..=i).map(|p| (p * PAGE, sha256(&p.to_be_bytes())))),
        edits: (1..=i)
            .map(|p| format!("0x{:X}: {:02X} was 00\n", p * PAGE + 4, p))
            .collect(),
        pastes: Vec::new(),
    }
}

fn picture(i: u8) -> Paste {
    Paste::new(vec![0x89, b'P', b'N', b'G', i, i, i, i])
}

fn image_snap(pastes: &[u8]) -> Snapshot {
    let pastes: Vec<Paste> = pastes.iter().map(|&i| picture(i)).collect();
    Snapshot {
        baseline: fp(40_000, 1_794_389_076, Some(sha256(b"original"))),
        seen: Seen::whole(sha256(b"original")),
        edits: pastes
            .iter()
            .map(|p| format!("paste 0,0,4,4,fit,{}\n", crate::cache::hex(&p.sha256)))
            .collect(),
        pastes,
    }
}

fn sorted_pastes(snap: &Snapshot) -> Vec<Sha> {
    let set: BTreeSet<Sha> = snap.pastes.iter().map(|p| p.sha256).collect();
    set.into_iter().collect()
}

fn file_stat(size: u64, secs: u64) -> FileStat {
    FileStat {
        size,
        mtime: at(secs),
        uid: Some(501),
        gid: Some(20),
        mode: 0o644,
    }
}

// ---------------------------------------------------------------------------------------------
// The crash matrix.

/// What a scenario was told had become durable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ack {
    Gen(u64),
    /// A save phase: 0 `naming` / `writing`, 1 `created`, 2 `staged`, 3 `renaming`.
    Phase(u8),
    /// The save ended with its outcome recorded (its `save` file gone).
    SaveEnded,
    /// The journal's removal started: from here on it may be gone.
    Removing,
    Removed,
}

type Steps = fn(&Arc<dyn Fs>, &Path, &mut Vec<Ack>) -> Result<()>;
type Check = fn(&[Ack], Option<&Recovered>, &Arc<dyn Fs>) -> std::result::Result<(), String>;

struct Scenario {
    name: &'static str,
    run: Steps,
    /// What each generation number holds when published.
    gens: BTreeMap<u64, Snapshot>,
    /// The scenario's own rule over what recovery found (after the shared rules held).
    check: Check,
}

struct Recovered {
    gen: Generation,
    save: SaveFile,
    dir: PathBuf,
}

fn rank(phase: &Phase) -> u8 {
    match phase {
        Phase::Naming { .. } | Phase::Writing { .. } => 0,
        Phase::Created { .. } => 1,
        Phase::Staged { .. } => 2,
        Phase::Renaming { .. } => 3,
    }
}

fn last_gen(acks: &[Ack]) -> Option<u64> {
    acks.iter()
        .filter_map(|a| match a {
            Ack::Gen(n) => Some(*n),
            _ => None,
        })
        .max()
}

fn last_phase(acks: &[Ack]) -> Option<u8> {
    acks.iter()
        .filter_map(|a| match a {
            Ack::Phase(p) => Some(*p),
            _ => None,
        })
        .max()
}

/// The shared rules: at most one journal; nothing after it was removed; a generation is whole —
/// exactly what was published under its number, its pastes intact — and never older than the
/// last one reported kept; a save record names a whole generation and is never behind the last
/// phase reported recorded.
fn recover(
    fs: &Arc<dyn Fs>,
    cache: &Path,
    acks: &[Ack],
    gens: &BTreeMap<u64, Snapshot>,
) -> std::result::Result<Option<Recovered>, String> {
    let found = orphans(fs, cache, HOST, PATH).map_err(|e| format!("listing failed: {e:#}"))?;
    let removed = acks.contains(&Ack::Removed);
    let removing = acks.contains(&Ack::Removing);
    let kept = last_gen(acks);
    if found.len() > 1 {
        return Err(format!("{} journals found", found.len()));
    }
    let Some(orphan) = found.into_iter().next() else {
        return match kept {
            Some(n) if !removing => Err(format!("gen-{n} was reported kept, then lost")),
            _ => Ok(None),
        };
    };
    if removed {
        return Err("a removed journal came back".into());
    }
    let gen = match &orphan.found {
        Found::Edits(gen) => gen.clone(),
        Found::Unusable {
            generation: None, ..
        } if kept.is_none() => return Ok(None),
        Found::Unusable { why, .. } => return Err(format!("unusable: {why}")),
    };
    let n = gen.at.n;
    let want = gens
        .get(&n)
        .ok_or_else(|| format!("gen-{n} was never published"))?;
    if gen.head.baseline != want.baseline
        || gen.seen != want.seen
        || gen.edits != want.edits
        || gen.pastes != sorted_pastes(want)
    {
        return Err(format!("gen-{n} is not what was published as gen-{n}"));
    }
    if let Some(k) = kept {
        if n < k {
            return Err(format!(
                "gen-{n} restored although gen-{k} was reported kept"
            ));
        }
    }
    for sha in &gen.pastes {
        let png = orphan
            .read_paste(sha)
            .map_err(|e| format!("paste of gen-{n}: {e:#}"))?;
        if sha256(&png) != *sha {
            return Err(format!("paste of gen-{n} is torn"));
        }
    }
    if let SaveFile::Unreadable(why) = &orphan.save {
        return Err(format!("a torn save record: {why}"));
    }
    if let SaveFile::Intent(intent) = &orphan.save {
        if let Some(p) = last_phase(acks) {
            if !acks.contains(&Ack::SaveEnded) && rank(&intent.phase) < p {
                return Err(format!(
                    "save record at phase {} although phase {p} was reported",
                    rank(&intent.phase)
                ));
            }
        }
        let bytes = fs
            .read(&orphan.dir().join(format!("gen-{}", intent.saved.n)))
            .map_err(|e| format!("the saved gen-{}: {e}", intent.saved.n))?;
        let saved = parse_generation(&bytes).map_err(|e| format!("the saved generation: {e:#}"))?;
        if saved.at != intent.saved {
            return Err("the save record names another generation".into());
        }
        for sha in &saved.pastes {
            orphan
                .read_paste(sha)
                .map_err(|e| format!("a paste of the saved generation: {e:#}"))?;
        }
    }
    let dir = orphan.dir().to_path_buf();
    Ok(Some(Recovered {
        gen,
        save: orphan.save.clone(),
        dir,
    }))
}

/// Runs `scenario` once whole to count its steps, then once per step k on a fresh filesystem
/// that dies before step k, and checks recovery after a kill and after both kinds of power cut.
/// Returns every rule broken.
fn mem_matrix(scenario: &Scenario, wrap: fn(Arc<dyn Fs>) -> Arc<dyn Fs>) -> Vec<String> {
    let cache = Path::new(MEM_CACHE);
    let all = {
        let mem = MemFs::with_cache();
        let dying = Dying::new(wrap(Arc::new(mem)), usize::MAX);
        let fs: Arc<dyn Fs> = dying.clone();
        (scenario.run)(&fs, cache, &mut Vec::new())
            .unwrap_or_else(|e| panic!("{}: the scenario fails whole: {e:#}", scenario.name));
        dying.ops()
    };
    let mut broken = Vec::new();
    for k in 0..=all.len() {
        let mem = MemFs::with_cache();
        let dying = Dying::new(wrap(Arc::new(mem.clone())), k);
        let fs: Arc<dyn Fs> = dying.clone();
        let mut acks = Vec::new();
        let ran = (scenario.run)(&fs, cache, &mut acks);
        if k == all.len() {
            if let Err(e) = ran {
                broken.push(format!("{}: whole run failed: {e:#}", scenario.name));
            }
        }
        let step = all.get(k).map_or("the end", String::as_str);
        for (how, after) in [
            ("kill", mem.killed()),
            ("power cut", mem.power_cut(true)),
            ("power cut, renames on disk", mem.power_cut(false)),
        ] {
            let after: Arc<dyn Fs> = Arc::new(after);
            let outcome = recover(&after, cache, &acks, &scenario.gens)
                .and_then(|found| (scenario.check)(&acks, found.as_ref(), &after));
            if let Err(why) = outcome {
                broken.push(format!(
                    "{}: {how} before `{step}` (step {k}, acks {acks:?}): {why}",
                    scenario.name
                ));
            }
        }
    }
    broken
}

fn no_extra(_: &[Ack], _: Option<&Recovered>, _: &Arc<dyn Fs>) -> std::result::Result<(), String> {
    Ok(())
}

fn publishes() -> Scenario {
    fn run(fs: &Arc<dyn Fs>, cache: &Path, acks: &mut Vec<Ack>) -> Result<()> {
        let mut j = Journal::create(fs.clone(), cache, HOST, PATH, Kind::Hex)?;
        for i in 1..=3 {
            let r = j.publish(&hex_snap(i))?;
            acks.push(Ack::Gen(r.n));
        }
        Ok(())
    }
    Scenario {
        name: "hex: creation and three generations",
        run,
        gens: (1..=3).map(|i| (i, hex_snap(i))).collect(),
        check: no_extra,
    }
}

fn pastes() -> Scenario {
    fn run(fs: &Arc<dyn Fs>, cache: &Path, acks: &mut Vec<Ack>) -> Result<()> {
        let mut j = Journal::create(fs.clone(), cache, HOST, PATH, Kind::Image)?;
        for snap in [image_snap(&[1]), image_snap(&[1, 2]), image_snap(&[2])] {
            let r = j.publish(&snap)?;
            acks.push(Ack::Gen(r.n));
        }
        Ok(())
    }
    Scenario {
        name: "image: pastes added, then one dropped",
        run,
        gens: [
            (1, image_snap(&[1])),
            (2, image_snap(&[1, 2])),
            (3, image_snap(&[2])),
        ]
        .into(),
        check: no_extra,
    }
}

fn hex_post(staged: &FileStat) -> PostEvidence {
    PostEvidence::hex(
        staged,
        [(PAGE, sha256(b"new page"))].into(),
        Some(sha256(b"new whole")),
    )
    .unwrap()
}

/// The remaining edits after a Save (the rebase): a new baseline and evidence.
fn rebased() -> Snapshot {
    Snapshot {
        baseline: fp(1_000_000, 1_794_390_000, Some(sha256(b"new whole"))),
        seen: Seen::pages([(9 * PAGE, sha256(b"page 9"))]),
        edits: "0x9004: 09 was 00\n".into(),
        pastes: Vec::new(),
    }
}

fn save_phases(j: &mut Journal, acks: &mut Vec<Ack>) -> Result<()> {
    let created = file_stat(0, 1_794_389_500);
    j.save_created(created)?;
    acks.push(Ack::Phase(1));
    let staged = file_stat(1_000_000, 1_794_389_600);
    j.save_staged(staged, hex_post(&staged))?;
    acks.push(Ack::Phase(2));
    j.save_renaming()?;
    acks.push(Ack::Phase(3));
    Ok(())
}

fn temp_plan() -> Plan {
    Plan::Temp {
        route: Route::ServerCopy,
        target: PATH.into(),
        temp: TEMP.into(),
    }
}

fn save_rebased() -> Scenario {
    fn run(fs: &Arc<dyn Fs>, cache: &Path, acks: &mut Vec<Ack>) -> Result<()> {
        let mut j = Journal::create(fs.clone(), cache, HOST, PATH, Kind::Hex)?;
        acks.push(Ack::Gen(j.publish(&hex_snap(1))?.n));
        // The edits saved differ from gen-1: the Save's start writes gen-2 first.
        j.begin_save(&hex_snap(2), temp_plan())?;
        acks.push(Ack::Gen(2));
        acks.push(Ack::Phase(0));
        save_phases(&mut j, acks)?;
        let j = j.save_landed(Some(&rebased()))?.expect("edits remain");
        acks.push(Ack::Gen(3));
        acks.push(Ack::SaveEnded);
        drop(j);
        Ok(())
    }
    fn check(
        acks: &[Ack],
        found: Option<&Recovered>,
        _: &Arc<dyn Fs>,
    ) -> std::result::Result<(), String> {
        let Some(r) = found else { return Ok(()) };
        let begun = acks.contains(&Ack::Phase(0));
        match (&r.save, r.gen.at.n) {
            // The rebase is CURRENT before the save record goes.
            (SaveFile::None, 2) if begun => Err("gen-2 offered again with no save record".into()),
            (SaveFile::None, _) => Ok(()),
            (SaveFile::Intent(i), _) if i.saved.n != 2 => Err("the save names another gen".into()),
            (SaveFile::Intent(_), _) if acks.contains(&Ack::SaveEnded) => {
                Err("the save record outlived the end of the save".into())
            }
            _ => Ok(()),
        }
    }
    let mut gens: BTreeMap<u64, Snapshot> = [(1, hex_snap(1)), (2, hex_snap(2))].into();
    gens.insert(3, rebased());
    Scenario {
        name: "hex save: four phases, landed, rebased",
        run,
        gens,
        check,
    }
}

fn save_retired() -> Scenario {
    fn run(fs: &Arc<dyn Fs>, cache: &Path, acks: &mut Vec<Ack>) -> Result<()> {
        let mut j = Journal::create(fs.clone(), cache, HOST, PATH, Kind::Hex)?;
        acks.push(Ack::Gen(j.publish(&hex_snap(1))?.n));
        // Nothing changed since gen-1: the Save saves gen-1 itself.
        j.begin_save(&hex_snap(1), temp_plan())?;
        acks.push(Ack::Phase(0));
        save_phases(&mut j, acks)?;
        acks.push(Ack::Removing);
        assert!(j.save_landed(None)?.is_none());
        acks.push(Ack::Removed);
        Ok(())
    }
    fn check(
        acks: &[Ack],
        found: Option<&Recovered>,
        _: &Arc<dyn Fs>,
    ) -> std::result::Result<(), String> {
        let Some(r) = found else { return Ok(()) };
        if r.gen.at.n != 1 {
            return Err(format!(
                "gen-{} found; only gen-1 was published",
                r.gen.at.n
            ));
        }
        if acks.contains(&Ack::Phase(0)) && r.save == SaveFile::None {
            return Err("saved edits offered again with no save record".into());
        }
        Ok(())
    }
    Scenario {
        name: "hex save: landed, journal retired",
        run,
        gens: [(1, hex_snap(1))].into(),
        check,
    }
}

fn in_place_retired() -> Scenario {
    fn run(fs: &Arc<dyn Fs>, cache: &Path, acks: &mut Vec<Ack>) -> Result<()> {
        let mut j = Journal::create(fs.clone(), cache, HOST, PATH, Kind::Hex)?;
        acks.push(Ack::Gen(j.publish(&hex_snap(1))?.n));
        let plan = Plan::InPlace {
            target: PATH.into(),
            undo: PathBuf::from("/cache/.backup/box/data/model.bin.20261120T114200Z.undo.txt"),
            runs: vec![Run {
                offset: PAGE + 4,
                old: vec![0],
                new: vec![1],
            }],
        };
        j.begin_save(&hex_snap(1), plan)?;
        acks.push(Ack::Phase(0));
        acks.push(Ack::Removing);
        assert!(j.save_landed(None)?.is_none());
        acks.push(Ack::Removed);
        Ok(())
    }
    Scenario {
        name: "hex write in place: landed, journal retired",
        run,
        gens: [(1, hex_snap(1))].into(),
        check: save_retired().check,
    }
}

fn unknown_then_edits() -> Scenario {
    fn run(fs: &Arc<dyn Fs>, cache: &Path, acks: &mut Vec<Ack>) -> Result<()> {
        let mut j = Journal::create(fs.clone(), cache, HOST, PATH, Kind::Image)?;
        acks.push(Ack::Gen(j.publish(&image_snap(&[1]))?.n));
        j.begin_save(
            &image_snap(&[1, 2]),
            Plan::Temp {
                route: Route::Upload,
                target: PATH.into(),
                temp: TEMP.into(),
            },
        )?;
        acks.push(Ack::Gen(2));
        acks.push(Ack::Phase(0));
        j.save_unknown();
        // Editing resumes; gen-2, named by the save, and its pastes must stay.
        acks.push(Ack::Gen(j.publish(&image_snap(&[3]))?.n));
        acks.push(Ack::Gen(j.publish(&image_snap(&[4]))?.n));
        Ok(())
    }
    fn check(
        acks: &[Ack],
        found: Option<&Recovered>,
        fs: &Arc<dyn Fs>,
    ) -> std::result::Result<(), String> {
        let Some(r) = found else { return Ok(()) };
        if acks.contains(&Ack::Phase(0)) {
            let SaveFile::Intent(i) = &r.save else {
                return Err("the save record of an unknown outcome is gone".into());
            };
            if i.saved.n != 2 {
                return Err("the save names another gen".into());
            }
            // `recover` already read gen-2 and its pastes through the record.
            let _ = fs.read(&r.dir.join("gen-2")).map_err(|e| e.to_string())?;
        }
        Ok(())
    }
    Scenario {
        name: "image save: unknown outcome, then more edits",
        run,
        gens: [
            (1, image_snap(&[1])),
            (2, image_snap(&[1, 2])),
            (3, image_snap(&[3])),
            (4, image_snap(&[4])),
        ]
        .into(),
        check,
    }
}

fn discarded() -> Scenario {
    fn run(fs: &Arc<dyn Fs>, cache: &Path, acks: &mut Vec<Ack>) -> Result<()> {
        let mut j = Journal::create(fs.clone(), cache, HOST, PATH, Kind::Image)?;
        acks.push(Ack::Gen(j.publish(&image_snap(&[1]))?.n));
        acks.push(Ack::Gen(j.publish(&image_snap(&[1, 2]))?.n));
        acks.push(Ack::Removing);
        j.remove()?;
        acks.push(Ack::Removed);
        Ok(())
    }
    Scenario {
        name: "image: discarded",
        run,
        gens: [(1, image_snap(&[1])), (2, image_snap(&[1, 2]))].into(),
        check: no_extra,
    }
}

fn plain(fs: Arc<dyn Fs>) -> Arc<dyn Fs> {
    fs
}

fn unsynced(fs: Arc<dyn Fs>) -> Arc<dyn Fs> {
    Arc::new(NoSync(fs))
}

fn assert_no_breaks(scenario: Scenario) {
    let broken = mem_matrix(&scenario, plain);
    assert!(broken.is_empty(), "{}", broken.join("\n"));
}

/// E10, the journal's creation and each step of a generation's publication: a kill or a power cut
/// before any step leaves no journal, a journal with no generation (named, ignored), or generation
/// n − 1 or n whole — never one older than the last reported kept.
#[test]
fn a_crash_at_any_step_of_creation_or_publication_recovers_a_whole_generation() {
    assert_no_breaks(publishes());
}

/// E10: a generation naming pastes is published only after its pastes are durable; a crash
/// anywhere never recovers a generation whose paste is missing or torn, and a dropped paste is
/// removed only once no kept generation names it.
#[test]
fn a_crash_while_pastes_are_published_never_loses_a_named_paste() {
    assert_no_breaks(pastes());
}

/// E10 during a rebase, E18's phases: after a Save landed, the rebase is one new generation
/// (new baseline, new evidence, remaining edits) made current before the save record goes. A
/// crash recovers the old edits with the old evidence and the save record, or the new ones —
/// never a mix, never the saved edits with no save record.
#[test]
fn a_crash_during_a_save_or_its_rebase_never_mixes_generations() {
    assert_no_breaks(save_rebased());
}

/// A landed Save retires the journal: a crash before the retirement is durable recovers gen-1
/// with its save record (reconciled again), after it nothing.
#[test]
fn a_crash_while_retiring_never_offers_saved_edits_without_their_record() {
    assert_no_breaks(save_retired());
    assert_no_breaks(in_place_retired());
}

/// After an unknown outcome editing resumes, and the generation the save names stays — with its
/// pastes — through later generations until the save is reconciled.
#[test]
fn the_saved_generation_outlives_later_generations_until_reconciled() {
    assert_no_breaks(unknown_then_edits());
}

#[test]
fn a_crash_while_discarding_never_brings_back_part_of_a_journal() {
    assert_no_breaks(discarded());
}

/// The matrix is worth something only if it sees a missing fsync: with every fsync a no-op,
/// a power cut loses generations reported kept.
#[test]
fn the_crash_matrix_catches_missing_fsyncs() {
    for scenario in [publishes(), pastes(), save_rebased()] {
        let broken = mem_matrix(&scenario, unsynced);
        assert!(
            broken.iter().any(|b| b.contains("power cut")),
            "{}: no power cut broke a rule without fsyncs",
            scenario.name
        );
        assert!(
            !broken.iter().any(|b| b.contains(": kill before")),
            "{}: a kill needs no fsync:\n{}",
            scenario.name,
            broken.join("\n")
        );
    }
}

/// The same kills on the real disk (fewer: every fsync there is a real one).
#[test]
fn a_kill_at_any_step_on_the_real_disk_recovers_a_whole_generation() {
    fn short(fs: &Arc<dyn Fs>, cache: &Path, acks: &mut Vec<Ack>) -> Result<()> {
        let mut j = Journal::create(fs.clone(), cache, HOST, PATH, Kind::Image)?;
        acks.push(Ack::Gen(j.publish(&image_snap(&[1]))?.n));
        acks.push(Ack::Gen(j.publish(&image_snap(&[2]))?.n));
        acks.push(Ack::Removing);
        j.remove()?;
        acks.push(Ack::Removed);
        Ok(())
    }
    let gens: BTreeMap<u64, Snapshot> = [(1, image_snap(&[1])), (2, image_snap(&[2]))].into();
    let all = {
        let tmp = tempfile::tempdir().unwrap();
        let dying = Dying::new(disk(), usize::MAX);
        let fs: Arc<dyn Fs> = dying.clone();
        short(&fs, tmp.path(), &mut Vec::new()).unwrap();
        dying.ops().len()
    };
    assert!(all > 20, "{all} steps");
    for k in 0..=all {
        let tmp = tempfile::tempdir().unwrap();
        let dying = Dying::new(disk(), k);
        let fs: Arc<dyn Fs> = dying.clone();
        let mut acks = Vec::new();
        let ran = short(&fs, tmp.path(), &mut acks);
        // A kill while a retired journal is emptied is not an error: it is gone already.
        assert!(k < all || ran.is_ok(), "the whole run: {:?}", ran.err());
        let found = recover(&disk(), tmp.path(), &acks, &gens);
        assert!(found.is_ok(), "kill before step {k}: {:?}", found.err());
    }
}

// ---------------------------------------------------------------------------------------------
// The order of the steps, as the spec states it.

fn logged(fs: &MemFs) -> (Arc<Dying>, Arc<dyn Fs>) {
    let dying = Dying::new(Arc::new(fs.clone()), usize::MAX);
    let as_fs: Arc<dyn Fs> = dying.clone();
    (dying, as_fs)
}

/// §3: after each `mkdir`, the parent is fsynced before anything else happens there, for every
/// directory the creation made; then the lock.
#[test]
fn creation_fsyncs_the_parent_of_every_directory_it_makes() {
    let mem = MemFs::with_cache();
    let (log, fs) = logged(&mem);
    let j = Journal::create(fs, Path::new(MEM_CACHE), HOST, PATH, Kind::Hex).unwrap();
    let ops = log.ops();
    let dir = j.dir().to_path_buf();
    let mut want = Vec::new();
    for d in [
        "/cache/.edits",
        "/cache/.edits/box",
        "/cache/.edits/box/data",
    ] {
        let d = Path::new(d);
        want.push(format!("mkdir {}", d.display()));
        want.push(format!("fsyncdir {}", d.parent().unwrap().display()));
    }
    want.push(format!("mkdir {}", dir.display()));
    want.push("fsyncdir /cache/.edits/box/data".to_string());
    want.push(format!("lock {}", dir.join("lock").display()));
    assert_eq!(ops, want);
    let name = dir.file_name().unwrap().to_string_lossy().into_owned();
    let rest = name.strip_prefix("model.bin.journal-").expect(&name);
    let (stamp, hex) = rest.split_once('-').expect(&name);
    assert_eq!(stamp.len(), 16, "{name}");
    assert!(
        stamp.ends_with('Z') && stamp.as_bytes()[8] == b'T',
        "{name}"
    );
    assert!(
        hex.len() == 8 && hex.bytes().all(|b| b.is_ascii_hexdigit()),
        "{name}"
    );
    // Nothing is created again for the next journal of the same file but the journal itself.
    let (log, fs) = logged(&mem);
    let two = Journal::create(fs, Path::new(MEM_CACHE), HOST, PATH, Kind::Hex).unwrap();
    assert_eq!(log.ops()[0], format!("mkdir {}", two.dir().display()));
}

/// §3 steps 1–3: each new paste written, fsynced, renamed, the directory fsynced; then the
/// generation; then `CURRENT` — the directory fsynced after each rename.
#[test]
fn a_generation_is_published_in_the_spec_order() {
    let mem = MemFs::with_cache();
    let fs: Arc<dyn Fs> = Arc::new(mem.clone());
    let mut j = Journal::create(fs, Path::new(MEM_CACHE), HOST, PATH, Kind::Image).unwrap();
    let (log, fs) = logged(&mem);
    j.fs = fs;
    let snap = image_snap(&[7]);
    j.publish(&snap).unwrap();
    let d = j.dir().display().to_string();
    let paste = format!("paste-{}.png", crate::cache::hex(&snap.pastes[0].sha256));
    let want: Vec<String> = [
        format!("write {d}/{paste}.tmp"),
        format!("fsync {d}/{paste}.tmp"),
        format!("rename {d}/{paste}.tmp"),
        format!("fsyncdir {d}"),
        format!("write {d}/gen-1.tmp"),
        format!("fsync {d}/gen-1.tmp"),
        format!("rename {d}/gen-1.tmp"),
        format!("fsyncdir {d}"),
        format!("write {d}/CURRENT.tmp"),
        format!("fsync {d}/CURRENT.tmp"),
        format!("rename {d}/CURRENT.tmp"),
        format!("fsyncdir {d}"),
    ]
    .into();
    assert_eq!(log.ops(), want);
    // A paste already kept is not written again.
    let (log, fs) = logged(&mem);
    j.fs = fs;
    j.publish(&image_snap(&[7])).unwrap();
    assert!(
        !log.ops().iter().any(|op| op.contains("paste-")),
        "{:?}",
        log.ops()
    );
}

/// §3: `kept` only after the directory fsync that published the generation holding the latest
/// edit. A failure there is `NOT kept on disk`; the next generation that lands says `kept`.
#[test]
fn kept_is_said_only_after_the_directory_fsync() {
    let mem = MemFs::with_cache();
    let fs: Arc<dyn Fs> = Arc::new(mem.clone());
    let mut j = Journal::create(fs, Path::new(MEM_CACHE), HOST, PATH, Kind::Hex).unwrap();
    let (count, fs) = logged(&mem);
    j.fs = fs;
    j.publish(&hex_snap(1)).unwrap();
    let steps = count.ops();
    let last_sync = steps
        .iter()
        .rposition(|op| op.starts_with("fsyncdir"))
        .unwrap();
    assert!(steps[last_sync - 1].contains("CURRENT.tmp"), "{steps:?}");

    let t0 = Instant::now();
    let mut keep = Schedule::default();
    keep.changed(1, t0);
    assert_eq!(keep.status(), Keep::Keeping);
    let rev = keep.taking();
    // Dies right at the fsync of the directory after CURRENT's rename.
    let dying = Dying::new(Arc::new(mem.clone()), last_sync);
    j.fs = dying;
    match j.publish(&hex_snap(2)) {
        Ok(_) => panic!("published without the directory fsync"),
        Err(e) => keep.failed(format!("{e:#}")),
    }
    assert!(
        matches!(keep.status(), Keep::NotKept(ref why) if why.contains("killed")),
        "{:?}",
        keep.status()
    );
    let line = keep.status().line("~/j");
    assert!(line.starts_with("unsaved — NOT kept on disk ("), "{line}");
    // CURRENT already names gen-2 on disk. The owner undoes edit 2: back to gen-1's content,
    // which must be written again (not skipped as what is current), or a crash restores the
    // undone edit.
    j.fs = Arc::new(mem.clone());
    let r = j.publish(&hex_snap(1)).unwrap();
    assert_eq!(r.n, 3, "a number once tried is never used again");
    keep.kept(rev);
    assert_eq!(keep.status(), Keep::Kept);
    assert_eq!(keep.status().line("~/j"), "unsaved — kept at ~/j");
    drop(j);
    let after: Arc<dyn Fs> = Arc::new(mem.killed());
    let found = listed(&after);
    let g = edits_of(&found[0]);
    assert_eq!((g.at.n, &g.edits), (3, &hex_snap(1).edits));
}

/// A generation whose publication failed may still be what `CURRENT` names on disk: nothing
/// removes it until a later one is current, and the next Save writes its edits afresh.
#[test]
fn a_failed_publication_is_never_cleaned_away_or_saved_as_current() {
    let (mem, _, mut j) = mem_journal(Kind::Hex);
    j.publish(&hex_snap(1)).unwrap();
    j.begin_save(&hex_snap(1), temp_plan()).unwrap();
    j.save_unknown();
    // gen-2 published up to CURRENT's rename; its directory fsync never answers.
    j.fs = Dying::new(Arc::new(mem.clone()), 7);
    assert!(j.publish(&hex_snap(2)).is_err());
    j.fs = Arc::new(mem.clone());
    j.clear_save().unwrap();
    let dir = j.dir().to_path_buf();
    assert!(
        mem.exists(&dir.join("gen-2")),
        "the generation CURRENT names stays"
    );
    drop(j);
    let fs: Arc<dyn Fs> = Arc::new(mem.killed());
    let found = listed(&fs);
    assert_eq!(edits_of(&found[0]).at.n, 2);
    assert!(found[0].notes.is_empty(), "{:?}", found[0].notes);
    drop(found);
    // The same failure, then a Save of gen-1's content: its intent names a new generation.
    let (mem, _, mut j) = mem_journal(Kind::Hex);
    j.publish(&hex_snap(1)).unwrap();
    j.fs = Dying::new(Arc::new(mem.clone()), 7);
    assert!(j.publish(&hex_snap(2)).is_err());
    j.fs = Arc::new(mem.clone());
    j.begin_save(&hex_snap(1), temp_plan()).unwrap();
    let saved = j.pending_save().unwrap().saved;
    assert_eq!(saved.n, 3);
    assert_eq!(Some(saved), j.current());
    drop(j);
    // An image generation with a new paste, failed the same way: the paste it names stays too.
    let (mem, _, mut j) = mem_journal(Kind::Image);
    j.publish(&image_snap(&[1])).unwrap();
    j.begin_save(&image_snap(&[1]), upload_plan()).unwrap();
    j.save_unknown();
    // The paste (4 steps), gen-2 (4), CURRENT's write, fsync and rename: then it dies.
    j.fs = Dying::new(Arc::new(mem.clone()), 11);
    assert!(j.publish(&image_snap(&[2])).is_err());
    let dir = j.dir().to_path_buf();
    let on_disk = parse_current(&mem.read(&dir.join("CURRENT")).unwrap()).unwrap();
    assert_eq!(on_disk.n, 2, "CURRENT names gen-2 on disk");
    j.fs = Arc::new(mem.clone());
    j.clear_save().unwrap();
    drop(j);
    let fs: Arc<dyn Fs> = Arc::new(mem.killed());
    let found = listed(&fs);
    let g = edits_of(&found[0]);
    assert_eq!(g.pastes, vec![picture(2).sha256]);
    assert_eq!(
        found[0].read_paste(&picture(2).sha256).unwrap(),
        &*picture(2).png
    );
}

/// §3: the first change after the last generation schedules the next at most 250 ms later,
/// holding every change up to then (at most four a second); a generation taken at once (focus
/// loss, close, a Save's start) takes every change too.
#[test]
fn the_schedule_holds_changes_for_at_most_250_ms() {
    let t0 = Instant::now();
    let mut s = Schedule::default();
    assert_eq!(s.status(), Keep::Clean);
    assert_eq!(s.status().line("~/j"), "");
    assert_eq!(s.due(), None);
    s.changed(1, t0);
    assert_eq!(s.due(), Some(t0 + DELAY));
    s.changed(2, t0 + Duration::from_millis(200));
    assert_eq!(
        s.due(),
        Some(t0 + DELAY),
        "a later change does not push it out"
    );
    assert!(!s.is_due(t0 + Duration::from_millis(249)));
    assert!(s.is_due(t0 + DELAY));
    assert_eq!(s.taking(), 2);
    assert_eq!(s.due(), None);
    assert_eq!(s.status(), Keep::Keeping);
    assert_eq!(s.status().line("~/j"), "unsaved — keeping…");
    // A change while that generation is written schedules the next one.
    let t1 = t0 + Duration::from_millis(260);
    s.changed(3, t1);
    assert_eq!(s.due(), Some(t1 + DELAY));
    s.kept(2);
    assert_eq!(s.status(), Keep::Keeping, "rev 3 is not kept yet");
    // Focus loss: taken at once.
    assert_eq!(s.taking(), 3);
    s.kept(3);
    assert_eq!(s.status(), Keep::Kept);
    // Four changes a second at most, however fast the typing.
    let mut s = Schedule::default();
    let mut taken = 0;
    for ms in 0..1000u64 {
        let now = t0 + Duration::from_millis(ms);
        s.changed(ms + 1, now);
        if s.is_due(now) {
            s.taking();
            taken += 1;
        }
    }
    assert!(taken <= 4, "{taken} generations in a second");
}

// ---------------------------------------------------------------------------------------------
// Recovery: CURRENT, fallbacks, pastes, the lock.

fn mem_journal(kind: Kind) -> (MemFs, Arc<dyn Fs>, Journal) {
    let mem = MemFs::with_cache();
    let fs: Arc<dyn Fs> = Arc::new(mem.clone());
    let j = Journal::create(fs.clone(), Path::new(MEM_CACHE), HOST, PATH, kind).unwrap();
    (mem, fs, j)
}

fn listed(fs: &Arc<dyn Fs>) -> Vec<Orphan> {
    orphans(fs, Path::new(MEM_CACHE), HOST, PATH).unwrap()
}

fn edits_of(o: &Orphan) -> &Generation {
    match &o.found {
        Found::Edits(g) => g,
        other => panic!("not restorable: {other:?}"),
    }
}

/// §3: a `CURRENT` that is missing, or names a generation that is missing or fails its hash,
/// falls back to the newest generation whose hash verifies, with a note.
#[test]
fn a_bad_current_falls_back_to_the_newest_whole_generation() {
    // gen-2 current, gen-3 written and renamed but CURRENT not moved (a kill before CURRENT.tmp).
    let setup = || {
        // Where publishing gen-3 writes CURRENT.tmp, counted on a journal of its own.
        let stop = {
            let (mem, _, mut j) = mem_journal(Kind::Hex);
            j.publish(&hex_snap(1)).unwrap();
            j.publish(&hex_snap(2)).unwrap();
            let (count, logfs) = logged(&mem);
            j.fs = logfs;
            j.publish(&hex_snap(3)).unwrap();
            let ops = count.ops();
            ops.iter()
                .position(|op| op.contains("CURRENT.tmp"))
                .unwrap()
        };
        let (mem, fs, mut j) = mem_journal(Kind::Hex);
        j.publish(&hex_snap(1)).unwrap();
        j.publish(&hex_snap(2)).unwrap();
        j.fs = Dying::new(Arc::new(mem.clone()), stop);
        assert!(j.publish(&hex_snap(3)).is_err());
        let dir = j.dir().to_path_buf();
        drop(j);
        (mem, fs, dir)
    };

    let (_, fs, _) = setup();
    let found = listed(&fs);
    assert_eq!(edits_of(&found[0]).at.n, 2, "CURRENT is followed");
    assert!(found[0].notes.is_empty(), "{:?}", found[0].notes);
    drop(found);

    let (mem, fs, dir) = setup();
    mem.remove_file(&dir.join("CURRENT")).unwrap();
    let found = listed(&fs);
    assert_eq!(edits_of(&found[0]).at.n, 3, "the newest whole one");
    assert!(
        found[0].notes.iter().any(|n| n.contains("CURRENT")),
        "{:?}",
        found[0].notes
    );
    drop(found);

    // CURRENT names gen-2, whose bytes no longer match: the newest that verifies is gen-3.
    let (mem, fs, dir) = setup();
    let mut bytes = mem.read(&dir.join("gen-2")).unwrap();
    bytes[40] ^= 1;
    mem.remove_file(&dir.join("gen-2")).unwrap();
    mem.write_new(&dir.join("gen-2"), &bytes).unwrap();
    let found = listed(&fs);
    assert_eq!(edits_of(&found[0]).at.n, 3);
    assert!(
        found[0].notes.iter().any(|n| n.contains("gen-2")),
        "{:?}",
        found[0].notes
    );
    drop(found);

    // gen-3 torn as well: gen-1 is long gone (removed after gen-2), so nothing verifies.
    let (mem, fs, dir) = setup();
    for gen in ["gen-2", "gen-3"] {
        let bytes = mem.read(&dir.join(gen)).unwrap();
        mem.remove_file(&dir.join(gen)).unwrap();
        mem.write_new(&dir.join(gen), &bytes[..bytes.len() - 1])
            .unwrap();
    }
    let found = listed(&fs);
    match &found[0].found {
        Found::Unusable { why, generation } => {
            assert!(generation.is_none());
            assert!(why.contains("no generation"), "{why}");
        }
        other => panic!("{other:?}"),
    }
    assert!(mem.exists(&dir), "kept and named, never removed");
}

/// §3, E10: a generation naming a paste that is missing or fails its hash is never applied; the
/// journal is kept and named, with the reason.
#[test]
fn a_missing_or_torn_paste_is_never_applied() {
    for damage in ["missing", "torn"] {
        let (mem, fs, mut j) = mem_journal(Kind::Image);
        let snap = image_snap(&[5, 6]);
        j.publish(&snap).unwrap();
        let dir = j.dir().to_path_buf();
        drop(j);
        let name = format!("paste-{}.png", crate::cache::hex(&snap.pastes[1].sha256));
        mem.remove_file(&dir.join(&name)).unwrap();
        if damage == "torn" {
            mem.write_new(&dir.join(&name), b"\x89PNG half").unwrap();
        }
        let found = listed(&fs);
        assert_eq!(found.len(), 1);
        match &found[0].found {
            Found::Unusable {
                why,
                generation: Some(g),
            } => {
                assert!(why.contains(&name), "{damage}: {why}");
                assert!(why.contains(damage), "{damage}: {why}");
                assert_eq!(g.at.n, 1);
            }
            other => panic!("{damage}: applied or lost: {other:?}"),
        }
        assert!(found[0].read_paste(&snap.pastes[1].sha256).is_err());
        drop(found);
        assert!(mem.exists(&dir), "{damage}: the journal is kept");
    }
}

/// A journal found under a file's journal path that belongs to another host or path (two names
/// that map to one cache path) is not offered for this one.
#[test]
fn a_journal_of_another_file_is_not_offered() {
    let mem = MemFs::with_cache();
    let fs: Arc<dyn Fs> = Arc::new(mem);
    let mut j = Journal::create(fs.clone(), Path::new(MEM_CACHE), "a/b", PATH, Kind::Hex).unwrap();
    j.publish(&hex_snap(1)).unwrap();
    drop(j);
    assert!(orphans(&fs, Path::new(MEM_CACHE), "a_b", PATH)
        .unwrap()
        .is_empty());
    assert_eq!(
        orphans(&fs, Path::new(MEM_CACHE), "a/b", PATH)
            .unwrap()
            .len(),
        1
    );
}

/// §3: the lock is held while the session lives — another session neither lists nor takes its
/// journal — and is free once the session ends (a crash included). A listed journal is held by
/// the listing until its row is answered.
#[test]
fn the_lock_keeps_two_sessions_apart() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = tmp.path();
    let fs = disk();
    let mut live = Journal::create(fs.clone(), cache, HOST, PATH, Kind::Hex).unwrap();
    live.publish(&hex_snap(1)).unwrap();
    assert!(
        orphans(&fs, cache, HOST, PATH).unwrap().is_empty(),
        "a live session's journal is not offered"
    );
    drop(live);
    let first = orphans(&fs, cache, HOST, PATH).unwrap();
    assert_eq!(first.len(), 1);
    assert!(
        orphans(&fs, cache, HOST, PATH).unwrap().is_empty(),
        "a journal shown in one window's row is not offered in another"
    );
    let mut adopted = first.into_iter().next().unwrap().adopt().unwrap();
    assert!(orphans(&fs, cache, HOST, PATH).unwrap().is_empty());
    let r = adopted.publish(&hex_snap(2)).unwrap();
    assert_eq!(r.n, 2);
    drop(adopted);
    let again = orphans(&fs, cache, HOST, PATH).unwrap();
    assert_eq!(edits_of(&again[0]).at.n, 2);
}

/// A journal whose lock file never got made (a kill right after the `mkdir`) is taken by the next
/// listing, which makes the lock exclusively; one created by a session at the same moment then
/// goes to a journal of its own.
#[test]
fn a_journal_without_a_lock_file_is_taken_by_the_listing() {
    let stop = {
        let mem = MemFs::with_cache();
        let (count, logfs) = logged(&mem);
        drop(Journal::create(logfs, Path::new(MEM_CACHE), HOST, PATH, Kind::Hex).unwrap());
        count.ops().len() - 1
    };
    let mem = MemFs::with_cache();
    let dying = Dying::new(Arc::new(mem.clone()), stop);
    assert!(Journal::create(dying, Path::new(MEM_CACHE), HOST, PATH, Kind::Hex).is_err());
    let fs2: Arc<dyn Fs> = Arc::new(mem.clone());
    let found = orphans(&fs2, Path::new(MEM_CACHE), HOST, PATH).unwrap();
    assert_eq!(found.len(), 1);
    assert!(matches!(
        found[0].found,
        Found::Unusable {
            generation: None,
            ..
        }
    ));
    let dir = found[0].dir().to_path_buf();
    assert!(mem.exists(&dir.join("lock")));
    found.into_iter().next().unwrap().remove().unwrap();
    assert!(!mem.exists(&dir));
}

// ---------------------------------------------------------------------------------------------
// The real disk: owner-only, never through a link, the conformance of MemFs.

/// §3: the journal directory 0700 and its files 0600, and each directory the creation made 0700
/// — under umask 022, so this does not pass only because the umask is strict.
#[test]
fn a_journal_is_owner_only() {
    if crate::testutil::rerun_under_umask_022("journal::tests::a_journal_is_owner_only") {
        return;
    }
    use std::os::unix::fs::PermissionsExt;
    let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    let tmp = tempfile::tempdir().unwrap();
    let cache = tmp.path().join("cache");
    let mut j = Journal::create(disk(), &cache, HOST, PATH, Kind::Image).unwrap();
    j.publish(&image_snap(&[1])).unwrap();
    j.begin_save(
        &image_snap(&[1]),
        Plan::Temp {
            route: Route::Upload,
            target: PATH.into(),
            temp: TEMP.into(),
        },
    )
    .unwrap();
    j.save_created(file_stat(0, 1)).unwrap();
    let dir = j.dir().to_path_buf();
    assert_eq!(mode(&dir), 0o700);
    let names: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    for want in ["CURRENT", "gen-1", "save", "lock"] {
        assert!(names.iter().any(|n| n == want), "{want} in {names:?}");
    }
    assert!(names.iter().any(|n| n.starts_with("paste-")), "{names:?}");
    for name in &names {
        assert_eq!(mode(&dir.join(name)), 0o600, "{name}");
    }
    for d in dir.ancestors().skip(1) {
        if !d.starts_with(&cache) {
            break;
        }
        assert_eq!(mode(d), 0o700, "{}", d.display());
    }
}

/// §3: never through a link. A link where a directory of the journal path should be is refused,
/// and nothing is made where it points.
#[test]
fn a_journal_is_never_made_through_a_link() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = tmp.path().join("cache");
    let elsewhere = tmp.path().join("elsewhere");
    std::fs::create_dir_all(cache.join(".edits")).unwrap();
    std::fs::create_dir(&elsewhere).unwrap();
    std::os::unix::fs::symlink(&elsewhere, cache.join(".edits").join(HOST)).unwrap();
    let made = Journal::create(disk(), &cache, HOST, PATH, Kind::Hex);
    let err = made.err().expect("refused");
    assert!(format!("{err:#}").contains("not a directory"), "{err:#}");
    assert_eq!(std::fs::read_dir(&elsewhere).unwrap().count(), 0);
    // The cache root itself may be a link (the owner's choice, e.g. /tmp on macOS).
    let real = tmp.path().join("real-cache");
    std::fs::create_dir(&real).unwrap();
    let linked = tmp.path().join("linked-cache");
    std::os::unix::fs::symlink(&real, &linked).unwrap();
    assert!(Journal::create(disk(), &linked, HOST, PATH, Kind::Hex).is_ok());
}

/// The semantics the journal relies on, the same on [`Disk`] and on [`MemFs`].
fn conformance(fs: &dyn Fs, root: &Path) {
    let d = root.join("d");
    fs.create_dir(&d).unwrap();
    assert_eq!(
        fs.create_dir(&d).unwrap_err().kind(),
        io::ErrorKind::AlreadyExists
    );
    let a = d.join("a");
    fs.write_new(&a, b"one").unwrap();
    assert_eq!(
        fs.write_new(&a, b"two").unwrap_err().kind(),
        io::ErrorKind::AlreadyExists
    );
    fs.sync_file(&a).unwrap();
    fs.sync_dir(&d).unwrap();
    assert_eq!(fs.read(&a).unwrap(), b"one");
    let b = d.join("b");
    fs.write_new(&b, b"bee").unwrap();
    fs.rename(&b, &a).unwrap();
    assert_eq!(fs.read(&a).unwrap(), b"bee", "a rename replaces a file");
    assert_eq!(fs.node(&b, false).unwrap(), None);
    assert_eq!(fs.node(&a, false).unwrap(), Some(Node::File));
    assert_eq!(fs.node(&d, false).unwrap(), Some(Node::Dir));
    assert_eq!(fs.list(&d).unwrap(), vec!["a".to_string()]);
    assert!(fs.remove_dir(&d).is_err(), "not empty");
    let held = fs.lock(&d.join("lock"), true).unwrap().expect("free");
    assert_eq!(
        fs.lock(&d.join("lock"), true).err().map(|e| e.kind()),
        Some(io::ErrorKind::AlreadyExists)
    );
    assert!(fs.lock(&d.join("lock"), false).unwrap().is_none(), "held");
    drop(held);
    assert!(fs.lock(&d.join("lock"), false).unwrap().is_some(), "freed");
    assert_eq!(
        fs.lock(&d.join("nolock"), false).err().map(|e| e.kind()),
        Some(io::ErrorKind::NotFound)
    );
    let e = d.join("e");
    fs.create_dir(&e).unwrap();
    let e2 = d.join("e2");
    fs.rename(&e, &e2).unwrap();
    assert_eq!(fs.node(&e2, false).unwrap(), Some(Node::Dir));
    fs.remove_dir(&e2).unwrap();
    fs.remove_file(&a).unwrap();
    fs.remove_file(&d.join("lock")).unwrap();
    fs.remove_dir(&d).unwrap();
    assert_eq!(fs.node(&d, false).unwrap(), None);
}

#[test]
fn memfs_behaves_as_the_disk_does() {
    let tmp = tempfile::tempdir().unwrap();
    conformance(&Disk, tmp.path());
    let mem = MemFs::with_cache();
    conformance(&mem, Path::new(MEM_CACHE));
}

// ---------------------------------------------------------------------------------------------
// The file formats.

fn sample_intents() -> Vec<SaveIntent> {
    let staged = file_stat(1_000_000, 1_794_389_600);
    let base = SaveIntent {
        saved: GenRef {
            n: 2,
            sha256: [2; 32],
        },
        kind: Kind::Hex,
        route: Route::ServerCopy,
        target: "/data/a b\nc%d".into(),
        started: at(1_794_389_000),
        baseline: fp(1_000_000, 1_794_389_076, None),
        seen: hex_snap(2).seen,
        phase: Phase::Staged {
            temp: TEMP.into(),
            created: file_stat(0, 1_794_389_500),
            staged,
            post: hex_post(&staged),
        },
    };
    let writing = SaveIntent {
        route: Route::InPlace,
        phase: Phase::Writing {
            undo: PathBuf::from("/cache/.backup/box/data/x.undo.txt"),
            runs: vec![
                Run {
                    offset: PAGE + 4,
                    old: vec![0, 0],
                    new: vec![1, 2],
                },
                Run {
                    offset: PAGE + 0x800,
                    old: vec![9],
                    new: vec![8],
                },
            ],
        },
        ..base.clone()
    };
    let media = SaveIntent {
        kind: Kind::Media,
        route: Route::Upload,
        baseline: fp(300 << 20, 1_794_389_076, None),
        seen: Seen::ends(Ends {
            first: [3; 32],
            last: [4; 32],
        }),
        phase: Phase::Renaming {
            temp: TEMP.into(),
            created: file_stat(0, 1),
            staged: file_stat(301 << 20, 2),
            post: PostEvidence::media(
                &file_stat(301 << 20, 2),
                None,
                Some(Ends {
                    first: [5; 32],
                    last: [6; 32],
                }),
            )
            .unwrap(),
        },
        ..base.clone()
    };
    let naming = SaveIntent {
        seen: Seen::Pending,
        phase: Phase::Naming { temp: TEMP.into() },
        ..base.clone()
    };
    vec![base, writing, media, naming]
}

/// Every proper prefix of a generation, of `CURRENT` and of a save record — a torn write — is
/// refused, and so is any one byte changed; the whole file reads back as written.
#[test]
fn torn_or_changed_journal_files_are_refused() {
    let (mem, _, mut j) = mem_journal(Kind::Image);
    j.publish(&image_snap(&[1, 2])).unwrap();
    let dir = j.dir().to_path_buf();
    let gen = mem.read(&dir.join("gen-1")).unwrap();
    let current = mem.read(&dir.join("CURRENT")).unwrap();
    let parsed = parse_generation(&gen).unwrap();
    assert_eq!(parsed.text().as_bytes(), gen.as_slice(), "readable as text");
    assert_eq!(parse_current(&current).unwrap(), parsed.at);
    for cut in 0..gen.len() {
        assert!(parse_generation(&gen[..cut]).is_err(), "gen cut at {cut}");
    }
    for cut in 0..current.len() {
        assert!(
            parse_current(&current[..cut]).is_err(),
            "CURRENT cut at {cut}"
        );
    }
    for i in 0..gen.len() {
        let mut changed = gen.clone();
        changed[i] ^= 0x20;
        assert!(parse_generation(&changed).is_err(), "gen byte {i} changed");
    }
    for intent in sample_intents() {
        let text = intent.text();
        assert_eq!(parse_save(text.as_bytes()).unwrap(), intent, "{text}");
        for cut in 0..text.len() {
            assert!(
                parse_save(&text.as_bytes()[..cut]).is_err(),
                "save cut at {cut}"
            );
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The save intent's life in a journal.

fn upload_plan() -> Plan {
    Plan::Temp {
        route: Route::Upload,
        target: PATH.into(),
        temp: TEMP.into(),
    }
}

/// §3.1, §10: no edit while a Save runs — no new generation until it ends; the phases go in
/// order; the route must fit the kind.
#[test]
fn a_running_save_takes_no_generation_and_its_phases_go_in_order() {
    let (_, _, mut j) = mem_journal(Kind::Hex);
    assert!(
        j.begin_save(&hex_snap(1), upload_plan()).is_err(),
        "upload is not a hex route"
    );
    j.begin_save(&hex_snap(1), temp_plan()).unwrap();
    assert_eq!(
        j.current().unwrap().n,
        1,
        "the Save's start wrote the journal"
    );
    let err = j.publish(&hex_snap(2)).unwrap_err();
    assert!(format!("{err:#}").contains("save"), "{err:#}");
    assert!(j.save_renaming().is_err(), "renaming before staged");
    let staged = file_stat(1_000_000, 2);
    assert!(
        j.save_staged(staged, hex_post(&staged)).is_err(),
        "staged before created"
    );
    j.save_created(file_stat(0, 1)).unwrap();
    assert!(
        j.save_staged(staged, PostEvidence::image(&staged, [1; 32], None).unwrap())
            .is_err(),
        "image evidence for a hex save"
    );
    j.save_staged(staged, hex_post(&staged)).unwrap();
    j.save_renaming().unwrap();
    assert!(
        j.begin_save(&hex_snap(1), temp_plan()).is_err(),
        "one save at a time"
    );
    j.save_not_landed().unwrap();
    assert!(j.pending_save().is_none());
    j.publish(&hex_snap(2)).unwrap();
    // An image save of an image journal; a write in place only for hex.
    let (_, _, mut i) = mem_journal(Kind::Image);
    let in_place = Plan::InPlace {
        target: PATH.into(),
        undo: PathBuf::from("/u"),
        runs: vec![Run {
            offset: 0,
            old: vec![0],
            new: vec![1],
        }],
    };
    assert!(i.begin_save(&image_snap(&[1]), in_place).is_err());
    assert!(i.begin_save(&image_snap(&[1]), temp_plan()).is_err());
    i.begin_save(&image_snap(&[1]), upload_plan()).unwrap();
}

/// §3.1 landed live: with nothing left unsaved the journal goes; with edits left, one new
/// generation holds them against the next baseline and the saved generation goes.
#[test]
fn a_landed_save_retires_or_rebases_the_journal() {
    let (mem, _, mut j) = mem_journal(Kind::Hex);
    j.begin_save(&hex_snap(1), temp_plan()).unwrap();
    let dir = j.dir().to_path_buf();
    // Landed only from `renaming` (or `writing`): the reply to a rename never sent proves nothing.
    let err = j.save_landed(None).err().expect("landed from naming");
    assert!(format!("{err:#}").contains("renaming"), "{err:#}");
    assert!(mem.exists(&dir), "the journal stays when landed is refused");

    let (mem, _, mut j) = mem_journal(Kind::Hex);
    j.begin_save(&hex_snap(1), temp_plan()).unwrap();
    save_phases(&mut j, &mut Vec::new()).unwrap();
    let dir = j.dir().to_path_buf();
    assert!(j.save_landed(None).unwrap().is_none());
    assert!(!mem.exists(&dir));
    assert!(
        !mem.list(dir.parent().unwrap())
            .unwrap()
            .iter()
            .any(|n| n.contains(".journal-")),
        "nothing left of it"
    );

    let (mem, _, mut j) = mem_journal(Kind::Hex);
    j.begin_save(&hex_snap(1), temp_plan()).unwrap();
    save_phases(&mut j, &mut Vec::new()).unwrap();
    let dir = j.dir().to_path_buf();
    let j = j.save_landed(Some(&rebased())).unwrap().unwrap();
    assert_eq!(j.current().unwrap().n, 2);
    assert!(j.pending_save().is_none());
    let names = mem.list(&dir).unwrap();
    assert!(!names.contains(&"save".to_string()), "{names:?}");
    assert!(!names.contains(&"gen-1".to_string()), "{names:?}");
    drop(j);
    let fs: Arc<dyn Fs> = Arc::new(mem);
    let found = listed(&fs);
    let g = edits_of(&found[0]);
    assert_eq!(g.head.baseline, rebased().baseline);
    assert_eq!(g.seen, rebased().seen);
}

/// §3.1 on reopen: landed with the saved generation current retires the journal; landed with a
/// later generation (edits made after an unknown outcome) keeps it, the save record cleared, so
/// those edits are offered under Restore's checks; an unreadable record is an unknown outcome.
#[test]
fn reopen_settles_a_cut_off_save() {
    let (mem, fs, mut j) = mem_journal(Kind::Hex);
    j.begin_save(&hex_snap(1), temp_plan()).unwrap();
    save_phases(&mut j, &mut Vec::new()).unwrap();
    let dir = j.dir().to_path_buf();
    drop(j);
    let o = listed(&fs).into_iter().next().unwrap();
    assert!(matches!(o.save, SaveFile::Intent(_)));
    assert!(o.landed().unwrap().is_none());
    assert!(!mem.exists(&dir));

    let (mem, fs, mut j) = mem_journal(Kind::Hex);
    j.begin_save(&hex_snap(1), temp_plan()).unwrap();
    save_phases(&mut j, &mut Vec::new()).unwrap();
    j.save_unknown();
    j.publish(&hex_snap(2)).unwrap();
    let dir = j.dir().to_path_buf();
    drop(j);
    let o = listed(&fs).into_iter().next().unwrap();
    let o = o.landed().unwrap().expect("later edits stay");
    assert_eq!(o.save, SaveFile::None);
    assert_eq!(edits_of(&o).at.n, 2);
    assert!(o
        .notes
        .iter()
        .any(|n| n.contains("landed") && n.contains("later")));
    drop(o);
    assert!(!mem.list(&dir).unwrap().contains(&"save".to_string()));

    // A record that cannot be read: unknown, named; Restore only after it is cleared.
    let (mem, fs, mut j) = mem_journal(Kind::Hex);
    j.begin_save(&hex_snap(1), temp_plan()).unwrap();
    let dir = j.dir().to_path_buf();
    drop(j);
    let bytes = mem.read(&dir.join("save")).unwrap();
    mem.remove_file(&dir.join("save")).unwrap();
    mem.write_new(&dir.join("save"), &bytes[..bytes.len() / 2])
        .unwrap();
    let mut o = listed(&fs).into_iter().next().unwrap();
    assert!(matches!(o.save, SaveFile::Unreadable(_)));
    let r = o.reconcile(&mut FakeRemote::default()).unwrap();
    assert_eq!(r.outcome, Outcome::Unknown);
    assert!(r.line.contains("cannot be read"), "{}", r.line);
    o.clear_save().unwrap();
    assert!(o.adopt().is_ok());
}

// ---------------------------------------------------------------------------------------------
// A remote for Restore and reconciliation: files held as bytes over zeros, so a 300 MiB file
// costs nothing.

#[derive(Clone, Debug)]
struct FakeFile {
    size: u64,
    mtime: SystemTime,
    uid: u32,
    gid: u32,
    mode: u32,
    data: Vec<u8>,
    patch: BTreeMap<u64, u8>,
}

impl FakeFile {
    fn new(content: &[u8], secs: u64) -> FakeFile {
        FakeFile {
            size: content.len() as u64,
            mtime: at(secs),
            uid: 501,
            gid: 20,
            mode: 0o644,
            data: content.to_vec(),
            patch: BTreeMap::new(),
        }
    }

    /// A file of `size` bytes, zeros but where `patch`ed.
    fn big(size: u64, secs: u64) -> FakeFile {
        FakeFile {
            size,
            ..FakeFile::new(&[], secs)
        }
    }

    fn read(&self, offset: u64, len: usize) -> Vec<u8> {
        let end = (offset + len as u64).min(self.size);
        if offset >= end {
            return Vec::new();
        }
        let mut out = vec![0; (end - offset) as usize];
        let held = self.data.len() as u64;
        if offset < held {
            let to = end.min(held);
            out[..(to - offset) as usize].copy_from_slice(&self.data[offset as usize..to as usize]);
        }
        for (&i, &b) in self.patch.range(offset..end) {
            out[(i - offset) as usize] = b;
        }
        out
    }

    fn set(&mut self, offset: u64, bytes: &[u8]) {
        for (i, b) in bytes.iter().enumerate() {
            self.patch.insert(offset + i as u64, *b);
        }
    }

    fn stat(&self) -> Stat {
        Stat {
            is_dir: false,
            is_file: true,
            is_symlink: false,
            size: self.size,
            mtime: self.mtime,
            mode: self.mode,
            uid: Some(self.uid),
            gid: Some(self.gid),
            missing: None,
            sha256: None,
        }
    }

    fn file_stat(&self) -> FileStat {
        FileStat::of(&self.stat())
    }

    fn page(&self, offset: u64) -> Sha {
        sha256(&self.read(offset, PAGE as usize))
    }

    fn whole(&self) -> Sha {
        sha256(&self.read(0, self.size as usize))
    }

    fn ends(&self) -> Ends {
        let n = ENDS.min(self.size) as usize;
        Ends {
            first: sha256(&self.read(0, n)),
            last: sha256(&self.read(self.size - n as u64, n)),
        }
    }
}

#[derive(Default)]
struct FakeRemote {
    files: BTreeMap<String, FakeFile>,
    wholes: usize,
    broken: bool,
}

impl FakeRemote {
    fn with(files: &[(&str, &FakeFile)]) -> FakeRemote {
        FakeRemote {
            files: files
                .iter()
                .map(|(p, f)| (p.to_string(), (*f).clone()))
                .collect(),
            ..FakeRemote::default()
        }
    }
}

impl Inspect for FakeRemote {
    fn lstat(&mut self, path: &str) -> Result<Option<Stat>> {
        if self.broken {
            bail!("connection lost");
        }
        Ok(self.files.get(path).map(FakeFile::stat))
    }

    fn read_at(&mut self, path: &str, offset: u64, len: usize) -> Result<Vec<u8>> {
        if self.broken {
            bail!("connection lost");
        }
        let file = self
            .files
            .get(path)
            .ok_or_else(|| anyhow::anyhow!("no {path}"))?;
        Ok(file.read(offset, len))
    }

    fn whole_sha256(&mut self, path: &str) -> Result<Sha> {
        self.wholes += 1;
        let file = self
            .files
            .get(path)
            .ok_or_else(|| anyhow::anyhow!("no {path}"))?;
        assert!(
            file.size <= crate::remote::HASH_CHECK_MAX,
            "the whole of a {} byte file was hashed",
            file.size
        );
        Ok(file.whole())
    }
}

/// A 12 KiB original, and the same with 4 bytes changed in its second page.
fn hex_files() -> (FakeFile, FakeFile) {
    let orig: Vec<u8> = (0..3 * PAGE).map(|i| (i % 251) as u8).collect();
    let mut new = orig.clone();
    new[PAGE as usize + 4..PAGE as usize + 8].copy_from_slice(b"EDIT");
    (FakeFile::new(&orig, 1_794_389_076), {
        let mut f = FakeFile::new(&new, 1_794_389_600);
        f.mode = 0o640;
        f
    })
}

fn generation(kind: Kind, baseline: Fingerprint, seen: Seen) -> Generation {
    Generation {
        at: GenRef {
            n: 1,
            sha256: [1; 32],
        },
        head: Head {
            kind,
            host: HOST.into(),
            path: PATH.into(),
            version: crate::version_text(),
            created: at(1_794_389_100),
            baseline,
        },
        seen,
        edits: String::new(),
        pastes: Vec::new(),
    }
}

fn baseline_of(f: &FakeFile, sha: Option<Sha>) -> Fingerprint {
    Fingerprint {
        size: f.size,
        mtime: f.mtime,
        sha256: sha,
    }
}

// ---------------------------------------------------------------------------------------------
// Restore (E10b).

/// Hex: the `seen` pages are read and compared even when `(size, mtime)` equals `head`; a page
/// changed with the mtime put back applies nothing; metadata changed around unchanged pages
/// applies with the note.
#[test]
fn hex_restore_reads_the_pages_even_when_the_metadata_matches() {
    let (orig, _) = hex_files();
    let seen = Seen::pages([(PAGE, orig.page(PAGE)), (2 * PAGE, orig.page(2 * PAGE))]);
    let g = generation(Kind::Hex, baseline_of(&orig, None), seen);
    let mut same = FakeRemote::with(&[(PATH, &orig)]);
    assert_eq!(
        restore_check(&g, Kind::Hex, &mut same),
        Restore::Apply { notes: Vec::new() }
    );
    let mut changed = orig.clone();
    changed.set(PAGE + 100, b"x");
    let mut remote = FakeRemote::with(&[(PATH, &changed)]);
    match restore_check(&g, Kind::Hex, &mut remote) {
        Restore::Refuse { why } => assert!(why.contains("0x1000"), "{why}"),
        other => panic!("applied over a changed page: {other:?}"),
    }
    let mut elsewhere = orig.clone();
    elsewhere.set(10, b"y");
    elsewhere.mtime = at(1_794_390_000);
    let mut remote = FakeRemote::with(&[(PATH, &elsewhere)]);
    assert_eq!(
        restore_check(&g, Kind::Hex, &mut remote),
        Restore::Apply {
            notes: vec![NOTE_PAGES.to_string()]
        }
    );
    let mut short = FakeFile::new(&orig.read(0, PAGE as usize), 1_794_389_076);
    short.mtime = orig.mtime;
    let mut remote = FakeRemote::with(&[(PATH, &short)]);
    assert!(matches!(
        restore_check(&g, Kind::Hex, &mut remote),
        Restore::Refuse { .. }
    ));
    let mut gone = FakeRemote::default();
    assert!(matches!(
        restore_check(&g, Kind::Hex, &mut gone),
        Restore::Refuse { .. }
    ));
}

/// Image: the whole SHA-256 must equal `seen`'s; a byte changed with the mtime put back replays
/// nothing; a size that differs is refused without hashing.
#[test]
fn image_restore_compares_the_whole_hash() {
    let orig = FakeFile::new(b"a small encoded image", 1_794_389_076);
    let g = generation(
        Kind::Image,
        baseline_of(&orig, Some(orig.whole())),
        Seen::whole(orig.whole()),
    );
    let mut remote = FakeRemote::with(&[(PATH, &orig)]);
    assert_eq!(
        restore_check(&g, Kind::Image, &mut remote),
        Restore::Apply { notes: Vec::new() }
    );
    let mut changed = orig.clone();
    changed.set(3, b"X");
    let mut remote = FakeRemote::with(&[(PATH, &changed)]);
    assert!(matches!(
        restore_check(&g, Kind::Image, &mut remote),
        Restore::Refuse { .. }
    ));
    let longer = FakeFile::new(b"a small encoded image!", 1_794_389_076);
    let mut remote = FakeRemote::with(&[(PATH, &longer)]);
    assert!(matches!(
        restore_check(&g, Kind::Image, &mut remote),
        Restore::Refuse { .. }
    ));
    assert_eq!(remote.wholes, 0, "a different size needs no hash");
}

/// Media at most 256 MiB: the whole hash. Above: size, mtime and the first and last MiB, with
/// the note saying so.
#[test]
fn media_restore_checks_whole_or_ends_by_size() {
    let small = FakeFile::new(b"a short clip", 1_794_389_076);
    let g = generation(
        Kind::Media,
        baseline_of(&small, Some(small.whole())),
        Seen::whole(small.whole()),
    );
    let mut remote = FakeRemote::with(&[(PATH, &small)]);
    assert_eq!(
        restore_check(&g, Kind::Media, &mut remote),
        Restore::Apply { notes: Vec::new() }
    );
    let mut big = FakeFile::big(300 << 20, 1_794_389_076);
    big.set(0, b"ftyp");
    big.set(big.size - 8, b"moovtail");
    let g = generation(Kind::Media, baseline_of(&big, None), Seen::ends(big.ends()));
    let mut remote = FakeRemote::with(&[(PATH, &big)]);
    assert_eq!(
        restore_check(&g, Kind::Media, &mut remote),
        Restore::Apply {
            notes: vec![NOTE_ENDS.to_string()]
        }
    );
    assert_eq!(remote.wholes, 0);
    let mut touched = big.clone();
    touched.mtime = at(1_794_390_000);
    let mut remote = FakeRemote::with(&[(PATH, &touched)]);
    assert!(matches!(
        restore_check(&g, Kind::Media, &mut remote),
        Restore::Refuse { .. }
    ));
    let mut tail = big.clone();
    tail.set(big.size - 1, b"!");
    let mut remote = FakeRemote::with(&[(PATH, &tail)]);
    assert!(matches!(
        restore_check(&g, Kind::Media, &mut remote),
        Restore::Refuse { .. }
    ));
}

/// Missing evidence asks before anything is applied (every kind); a journal of another kind is
/// named, never applied; a check that cannot run applies nothing.
#[test]
fn restore_asks_without_evidence_and_never_crosses_kinds() {
    let f = FakeFile::new(b"bytes", 1);
    for kind in [Kind::Hex, Kind::Image, Kind::Media] {
        let g = generation(kind, baseline_of(&f, None), Seen::Pending);
        let mut remote = FakeRemote::with(&[(PATH, &f)]);
        assert_eq!(restore_check(&g, kind, &mut remote), Restore::Ask);
    }
    let g = generation(
        Kind::Hex,
        baseline_of(&f, None),
        Seen::pages([(0, f.page(0))]),
    );
    let mut remote = FakeRemote::with(&[(PATH, &f)]);
    assert_eq!(
        restore_check(&g, Kind::Image, &mut remote),
        Restore::OtherKind(Kind::Hex)
    );
    remote.broken = true;
    assert!(matches!(
        restore_check(&g, Kind::Hex, &mut remote),
        Restore::Unchecked { .. }
    ));
}

/// The provided reads: a page past the end is none, the last page is short, the whole hash and
/// the ends come from `read_at` alone.
#[test]
fn inspect_provides_pages_whole_and_ends_from_reads() {
    struct Plain(FakeFile);
    impl Inspect for Plain {
        fn lstat(&mut self, _: &str) -> Result<Option<Stat>> {
            Ok(Some(self.0.stat()))
        }
        fn read_at(&mut self, _: &str, offset: u64, len: usize) -> Result<Vec<u8>> {
            Ok(self.0.read(offset, len))
        }
    }
    let content: Vec<u8> = (0..(5 << 20) + 123).map(|i| (i % 7) as u8).collect();
    let f = FakeFile::new(&content, 1);
    let mut p = Plain(f.clone());
    assert_eq!(p.page_sha256(PATH, 0).unwrap(), Some(f.page(0)));
    let last = (f.size / PAGE) * PAGE;
    assert_eq!(
        p.page_sha256(PATH, last).unwrap(),
        Some(sha256(&content[last as usize..]))
    );
    assert_eq!(p.page_sha256(PATH, f.size + PAGE).unwrap(), None);
    assert_eq!(p.whole_sha256(PATH).unwrap(), sha256(&content));
    assert_eq!(p.ends_sha256(PATH, f.size).unwrap(), Ends::of(&content));
}

// ---------------------------------------------------------------------------------------------
// Reconciliation (E18).

fn intent(kind: Kind, route: Route, baseline: Fingerprint, seen: Seen, phase: Phase) -> SaveIntent {
    SaveIntent {
        saved: GenRef {
            n: 2,
            sha256: [2; 32],
        },
        kind,
        route,
        target: PATH.into(),
        started: at(1_794_389_000),
        baseline,
        seen,
        phase,
    }
}

struct HexCase {
    orig: FakeFile,
    new: FakeFile,
    created: FileStat,
    post: PostEvidence,
}

fn hex_case() -> HexCase {
    let (orig, new) = hex_files();
    let post = PostEvidence::hex(
        &new.file_stat(),
        [(PAGE, new.page(PAGE))].into(),
        Some(new.whole()),
    )
    .unwrap();
    HexCase {
        created: FileStat {
            size: 0,
            ..new.file_stat()
        },
        orig,
        new,
        post,
    }
}

impl HexCase {
    fn intent(&self, route: Route, phase: Phase) -> SaveIntent {
        intent(
            Kind::Hex,
            route,
            baseline_of(&self.orig, Some(self.orig.whole())),
            Seen::pages([(PAGE, self.orig.page(PAGE))]),
            phase,
        )
    }
    fn staged(&self) -> Phase {
        Phase::Staged {
            temp: TEMP.into(),
            created: self.created,
            staged: self.new.file_stat(),
            post: self.post.clone(),
        }
    }
    fn renaming(&self) -> Phase {
        Phase::Renaming {
            temp: TEMP.into(),
            created: self.created,
            staged: self.new.file_stat(),
            post: self.post.clone(),
        }
    }
    fn created_temp(&self) -> FakeFile {
        FakeFile {
            size: 0,
            data: Vec::new(),
            ..self.new.clone()
        }
    }
}

/// Stopped in `naming`: the temp, if any, is named and kept, never removed — the exclusive create
/// may have failed on a file someone else made (the collision), so the name proves nothing.
#[test]
fn naming_never_removes_the_temp() {
    let c = hex_case();
    let i = c.intent(Route::ServerCopy, Phase::Naming { temp: TEMP.into() });
    let theirs = FakeFile::new(b"someone else's file", 5);
    let mut remote = FakeRemote::with(&[(PATH, &c.orig), (TEMP, &theirs)]);
    let r = reconcile(&i, HOST, &mut remote);
    assert_eq!(r.outcome, Outcome::NotLanded);
    match &r.temp {
        Temp::Keep { path, .. } => assert_eq!(path, TEMP),
        other => panic!("{other:?}"),
    }
    assert!(r.line.contains(TEMP), "{}", r.line);
    let mut remote = FakeRemote::with(&[(PATH, &c.orig)]);
    let r = reconcile(&i, HOST, &mut remote);
    assert_eq!(r.outcome, Outcome::NotLanded);
    assert_eq!(r.temp, Temp::Gone(TEMP.into()));
}

/// Stopped in `created` or `staged` (both temp routes): the temp whose stat — and from `staged`
/// on, evidence — match is removed, and the edits are offered. A temp that moved on since its
/// recorded stat is kept and named.
#[test]
fn created_or_staged_removes_only_a_temp_that_still_matches() {
    let c = hex_case();
    for route in [Route::ServerCopy, Route::StreamedRewrite] {
        let created = c.intent(
            route,
            Phase::Created {
                temp: TEMP.into(),
                created: c.created,
            },
        );
        let mut remote = FakeRemote::with(&[(PATH, &c.orig), (TEMP, &c.created_temp())]);
        let r = reconcile(&created, HOST, &mut remote);
        assert_eq!(r.outcome, Outcome::NotLanded, "{route:?}");
        assert_eq!(r.temp, Temp::Remove(TEMP.into()), "{route:?}");
        // Cut off mid-copy: the temp grew since its creation was recorded.
        let mut grown = c.created_temp();
        grown.size = 4096;
        grown.mtime = at(1_794_389_550);
        let mut remote = FakeRemote::with(&[(PATH, &c.orig), (TEMP, &grown)]);
        let r = reconcile(&created, HOST, &mut remote);
        assert!(
            matches!(r.temp, Temp::Keep { .. }),
            "{route:?} {:?}",
            r.temp
        );

        for phase in [c.staged(), c.renaming()] {
            let staged = c.intent(route, phase);
            let mut remote = FakeRemote::with(&[(PATH, &c.orig), (TEMP, &c.new)]);
            let r = reconcile(&staged, HOST, &mut remote);
            assert_eq!(r.outcome, Outcome::NotLanded, "{route:?}");
            assert_eq!(r.temp, Temp::Remove(TEMP.into()), "{route:?}");
            assert!(r.next.is_none());
        }
    }
}

/// The recorded temp replaced by another file of the same size before the reopen: its mtime or
/// its content differs, so it is kept and named.
#[test]
fn a_same_size_replacement_of_the_temp_is_kept() {
    let c = hex_case();
    let i = c.intent(Route::ServerCopy, c.staged());
    let mut other_mtime = c.new.clone();
    other_mtime.mtime = at(1_794_389_999);
    let mut other_content = c.new.clone();
    other_content.set(PAGE + 5, b"Z");
    for (what, temp) in [("mtime", other_mtime), ("content", other_content)] {
        let mut remote = FakeRemote::with(&[(PATH, &c.orig), (TEMP, &temp)]);
        let r = reconcile(&i, HOST, &mut remote);
        assert_eq!(r.outcome, Outcome::NotLanded, "{what}");
        match &r.temp {
            Temp::Keep { path, .. } => assert_eq!(path, TEMP, "{what}"),
            other => panic!("{what}: {other:?}"),
        }
        assert!(r.line.contains(TEMP), "{what}: {}", r.line);
    }
}

/// The rename landed but its reply was lost: found on reopen by the target's stat and
/// evidence; the next baseline is the post-save one.
#[test]
fn a_rename_that_landed_with_its_reply_lost_is_found() {
    let c = hex_case();
    for route in [Route::ServerCopy, Route::StreamedRewrite] {
        let i = c.intent(route, c.renaming());
        let mut remote = FakeRemote::with(&[(PATH, &c.new)]);
        let r = reconcile(&i, HOST, &mut remote);
        assert_eq!(r.outcome, Outcome::Landed, "{route:?}: {}", r.line);
        assert_eq!(r.temp, Temp::Gone(TEMP.into()));
        assert_eq!(
            r.next,
            Some((c.post.next_baseline, c.post.next_seen.clone()))
        );
        assert!(r.line.contains("landed (found on reopen)"), "{}", r.line);
    }
}

/// The target matches neither evidence (another writer after the rename): unknown, both paths
/// named, nothing removed — not even a temp that matches.
#[test]
fn a_target_that_matches_neither_is_unknown_and_nothing_is_removed() {
    let c = hex_case();
    let i = c.intent(Route::ServerCopy, c.renaming());
    let mut theirs = c.new.clone();
    theirs.set(PAGE + 4, b"THEM");
    theirs.mtime = at(1_794_390_500);
    let mut remote = FakeRemote::with(&[(PATH, &theirs), (TEMP, &c.new)]);
    let r = reconcile(&i, HOST, &mut remote);
    assert_eq!(r.outcome, Outcome::Unknown);
    assert!(matches!(r.temp, Temp::Keep { .. }), "{:?}", r.temp);
    assert!(
        r.line.contains(&format!("{HOST}:{PATH}")) && r.line.contains(&format!("{HOST}:{TEMP}")),
        "{}",
        r.line
    );
    // A remote that cannot be read is unknown too.
    let mut remote = FakeRemote::with(&[(PATH, &c.orig), (TEMP, &c.new)]);
    remote.broken = true;
    let r = reconcile(&i, HOST, &mut remote);
    assert_eq!(r.outcome, Outcome::Unknown);
    assert!(matches!(r.temp, Temp::Keep { .. }));
}

/// Image: landed with the reply lost, identified by the output's hash at most 256 MiB, and above
/// it by the stat and the first and last MiB; the next baseline is the whole hash either way.
#[test]
fn an_image_save_that_landed_is_found_at_every_size() {
    let orig = FakeFile::new(b"the original jpeg", 1_794_389_076);
    let out = FakeFile::new(b"the cropped jpeg", 1_794_389_600);
    let post = PostEvidence::image(&out.file_stat(), out.whole(), None).unwrap();
    let i = intent(
        Kind::Image,
        Route::Upload,
        baseline_of(&orig, Some(orig.whole())),
        Seen::whole(orig.whole()),
        Phase::Renaming {
            temp: TEMP.into(),
            created: FileStat {
                size: 0,
                ..out.file_stat()
            },
            staged: out.file_stat(),
            post: post.clone(),
        },
    );
    let mut remote = FakeRemote::with(&[(PATH, &out)]);
    let r = reconcile(&i, HOST, &mut remote);
    assert_eq!(r.outcome, Outcome::Landed);
    assert_eq!(remote.wholes, 1, "decided by the output's hash");
    let mut remote = FakeRemote::with(&[(PATH, &orig), (TEMP, &out)]);
    let r = reconcile(&i, HOST, &mut remote);
    assert_eq!(r.outcome, Outcome::NotLanded);
    assert_eq!(r.temp, Temp::Remove(TEMP.into()));

    let mut big = FakeFile::big(300 << 20, 1_794_389_600);
    big.set(0, b"\xff\xd8\xff");
    big.set(big.size - 2, b"\xff\xd9");
    let whole = [0x77; 32]; // the encoder's output hash, known locally at every size
    let post = PostEvidence::image(&big.file_stat(), whole, Some(big.ends())).unwrap();
    assert!(
        PostEvidence::image(&big.file_stat(), whole, None).is_err(),
        "above 256 MiB the ends are needed"
    );
    assert_eq!(post.next_baseline.sha256, Some(whole));
    assert_eq!(post.next_seen, Seen::whole(whole));
    let i = intent(
        Kind::Image,
        Route::Upload,
        baseline_of(&orig, Some(orig.whole())),
        Seen::whole(orig.whole()),
        Phase::Renaming {
            temp: TEMP.into(),
            created: FileStat {
                size: 0,
                ..big.file_stat()
            },
            staged: big.file_stat(),
            post: post.clone(),
        },
    );
    let mut remote = FakeRemote::with(&[(PATH, &big)]);
    let r = reconcile(&i, HOST, &mut remote);
    assert_eq!(r.outcome, Outcome::Landed, "{}", r.line);
    assert_eq!(remote.wholes, 0, "no whole read above 256 MiB");
    assert_eq!(r.next, Some((post.next_baseline, Seen::whole(whole))));
    let mut wrong_tail = big.clone();
    wrong_tail.set(big.size - 1, b"\x00");
    let mut remote = FakeRemote::with(&[(PATH, &wrong_tail)]);
    assert_eq!(reconcile(&i, HOST, &mut remote).outcome, Outcome::Unknown);
}

/// Media, below and above 256 MiB: landed with the reply lost, and not landed with the temp
/// removed.
#[test]
fn a_media_save_is_reconciled_below_and_above_256_mib() {
    for size in [4096u64, 300 << 20] {
        let mut orig = FakeFile::big(size, 1_794_389_076);
        orig.set(0, b"orig");
        let mut out = FakeFile::big(size - 1000, 1_794_389_600);
        out.set(0, b"cut!");
        let (seen, post) = if size <= crate::remote::HASH_CHECK_MAX {
            (
                Seen::whole(orig.whole()),
                PostEvidence::media(&out.file_stat(), Some(out.whole()), None).unwrap(),
            )
        } else {
            (
                Seen::ends(orig.ends()),
                PostEvidence::media(&out.file_stat(), None, Some(out.ends())).unwrap(),
            )
        };
        let i = intent(
            Kind::Media,
            Route::Upload,
            baseline_of(&orig, None),
            seen,
            Phase::Renaming {
                temp: TEMP.into(),
                created: FileStat {
                    size: 0,
                    ..out.file_stat()
                },
                staged: out.file_stat(),
                post,
            },
        );
        let mut remote = FakeRemote::with(&[(PATH, &out)]);
        assert_eq!(
            reconcile(&i, HOST, &mut remote).outcome,
            Outcome::Landed,
            "{size}"
        );
        let mut remote = FakeRemote::with(&[(PATH, &orig), (TEMP, &out)]);
        let r = reconcile(&i, HOST, &mut remote);
        assert_eq!(r.outcome, Outcome::NotLanded, "{size}: {}", r.line);
        assert_eq!(r.temp, Temp::Remove(TEMP.into()), "{size}");
    }
}

/// Write in place, run by run: each run read back by its own offset and length and judged by its
/// own recorded bytes — two runs in one 4 KiB page are told apart.
#[test]
fn write_in_place_is_judged_run_by_run() {
    let (orig, _) = hex_files();
    let runs = vec![
        Run {
            offset: PAGE + 4,
            old: orig.read(PAGE + 4, 4),
            new: b"EDIT".to_vec(),
        },
        Run {
            offset: PAGE + 0x800,
            old: orig.read(PAGE + 0x800, 2),
            new: b"HI".to_vec(),
        },
        Run {
            offset: 2 * PAGE + 16,
            old: orig.read(2 * PAGE + 16, 1),
            new: b"!".to_vec(),
        },
    ];
    let undo = PathBuf::from("/cache/.backup/box/data/model.bin.20261120T114200Z.undo.txt");
    let i = intent(
        Kind::Hex,
        Route::InPlace,
        baseline_of(&orig, None),
        Seen::pages([(PAGE, orig.page(PAGE)), (2 * PAGE, orig.page(2 * PAGE))]),
        Phase::Writing {
            undo: undo.clone(),
            runs: runs.clone(),
        },
    );
    let written = |k: usize| {
        let mut f = orig.clone();
        for run in &runs[..k] {
            f.set(run.offset, &run.new);
        }
        f.mtime = at(1_794_389_700);
        f
    };
    let judge = |f: &FakeFile| reconcile(&i, HOST, &mut FakeRemote::with(&[(PATH, f)]));
    assert_eq!(judge(&orig).outcome, Outcome::NotLanded);
    assert_eq!(judge(&written(3)).outcome, Outcome::Landed);
    for k in [1, 2] {
        let r = judge(&written(k));
        assert_eq!(r.outcome, Outcome::InPart, "stopped after {k} of 3");
        assert!(r.line.contains("holds some of your changes"), "{}", r.line);
        assert!(r.line.contains(&undo.display().to_string()), "{}", r.line);
        assert_eq!(r.temp, Temp::None);
    }
    // Runs 1 and 2 share page 0x1000: after the first alone the page matches neither its old
    // nor its new hash, yet each run is judged by its own bytes.
    let one = written(1);
    assert_ne!(one.page(PAGE), orig.page(PAGE));
    assert_eq!(judge(&one).outcome, Outcome::InPart);
    // A run that is neither old nor new: unknown, the undo file named.
    let mut torn = written(1);
    torn.set(PAGE + 0x800, b"H?");
    let r = judge(&torn);
    assert_eq!(r.outcome, Outcome::Unknown);
    assert!(
        r.line.contains("may hold some of your changes"),
        "{}",
        r.line
    );
    assert!(r.line.contains(&undo.display().to_string()), "{}", r.line);
    // Cut short before the last run: unknown.
    let mut cut = written(2);
    cut.size = 2 * PAGE + 10;
    assert_eq!(judge(&cut).outcome, Outcome::Unknown);
}

/// The post-save evidence per kind (§3.1's table) keeps reconciliation evidence and the next
/// baseline apart, and refuses shapes the table does not allow.
#[test]
fn post_save_evidence_follows_the_table() {
    let small = file_stat(1000, 2);
    let big = file_stat(300 << 20, 2);
    let pages: BTreeMap<u64, Sha> = [(0, [1; 32])].into();
    let hex = PostEvidence::hex(&small, pages.clone(), Some([2; 32])).unwrap();
    assert_eq!(hex.check.pages, pages);
    assert_eq!(hex.check.whole, Some([2; 32]));
    assert_eq!(hex.next_baseline, fp(1000, 2, Some([2; 32])));
    assert_eq!(hex.next_seen, Seen::pages(pages.clone()));
    assert!(PostEvidence::hex(&small, pages.clone(), None).is_err());
    assert!(PostEvidence::hex(&big, pages.clone(), Some([2; 32])).is_err());
    let hex_big = PostEvidence::hex(&big, pages.clone(), None).unwrap();
    assert_eq!(hex_big.next_baseline.sha256, None);
    assert!(PostEvidence::hex(&small, BTreeMap::new(), Some([2; 32])).is_err());
    let ends = Ends {
        first: [3; 32],
        last: [4; 32],
    };
    let image = PostEvidence::image(&small, [5; 32], None).unwrap();
    assert_eq!(image.check.whole, Some([5; 32]));
    assert_eq!(image.check.ends, None);
    let media = PostEvidence::media(&big, None, Some(ends)).unwrap();
    assert_eq!(media.check.ends, Some(ends));
    assert_eq!(media.next_seen, Seen::ends(ends));
    assert_eq!(media.next_baseline.sha256, None);
    assert!(PostEvidence::media(&big, Some([1; 32]), None).is_err());
    assert!(PostEvidence::media(&small, None, Some(ends)).is_err());
}

/// E18 for every kind and size, route and phase, against every state of the target: the
/// original (not landed: a temp proven sopen's is removed, one from `naming` kept), the output
/// (landed once `staged` was recorded, unknown before), another writer's file (unknown, nothing
/// removed). No file above 256 MiB is ever hashed whole: hex then decides by the stat and the
/// pages, image and media by the stat and the first and last MiB.
#[test]
fn every_kind_route_and_phase_is_reconciled() {
    struct Case {
        name: &'static str,
        kind: Kind,
        routes: &'static [Route],
        orig: FakeFile,
        out: FakeFile,
        seen: Seen,
        post: PostEvidence,
    }
    let big = 300u64 << 20;
    let edited = |f: &FakeFile, secs: u64| {
        let mut out = f.clone();
        out.set(PAGE + 4, b"EDIT");
        out.mtime = at(secs);
        out.mode = 0o640;
        out
    };
    let (hex_small, _) = hex_files();
    let hex_small_out = edited(&hex_small, 1_794_389_600);
    let mut hex_big = FakeFile::big(big, 1_794_389_076);
    hex_big.set(PAGE + 4, b"orig");
    let hex_big_out = edited(&hex_big, 1_794_389_600);
    let image_orig = FakeFile::new(b"the original image", 1_794_389_076);
    let image_out = FakeFile::new(b"the cropped image", 1_794_389_600);
    let mut image_big_out = FakeFile::big(big + 7, 1_794_389_600);
    image_big_out.set(0, b"\x89PNG");
    image_big_out.set(big, b"IEND");
    let mut media_orig = FakeFile::big(4096, 1_794_389_076);
    media_orig.set(0, b"orig");
    let mut media_out = FakeFile::big(3096, 1_794_389_600);
    media_out.set(0, b"cut!");
    let mut media_big = FakeFile::big(big + 4096, 1_794_389_076);
    media_big.set(0, b"ftyp");
    let mut media_big_out = FakeFile::big(big + 1000, 1_794_389_600);
    media_big_out.set(big + 999, b"!");
    let hex_routes: &[Route] = &[Route::ServerCopy, Route::StreamedRewrite];
    let cases = vec![
        Case {
            name: "hex at most 256 MiB",
            kind: Kind::Hex,
            routes: hex_routes,
            seen: Seen::pages([(PAGE, hex_small.page(PAGE))]),
            post: PostEvidence::hex(
                &hex_small_out.file_stat(),
                [(PAGE, hex_small_out.page(PAGE))].into(),
                Some(hex_small_out.whole()),
            )
            .unwrap(),
            orig: hex_small,
            out: hex_small_out,
        },
        Case {
            name: "hex above 256 MiB",
            kind: Kind::Hex,
            routes: hex_routes,
            seen: Seen::pages([(PAGE, hex_big.page(PAGE))]),
            post: PostEvidence::hex(
                &hex_big_out.file_stat(),
                [(PAGE, hex_big_out.page(PAGE))].into(),
                None,
            )
            .unwrap(),
            orig: hex_big,
            out: hex_big_out,
        },
        Case {
            name: "image output at most 256 MiB",
            kind: Kind::Image,
            routes: &[Route::Upload],
            seen: Seen::whole(image_orig.whole()),
            post: PostEvidence::image(&image_out.file_stat(), image_out.whole(), None).unwrap(),
            orig: image_orig.clone(),
            out: image_out,
        },
        Case {
            name: "image output above 256 MiB",
            kind: Kind::Image,
            routes: &[Route::Upload],
            seen: Seen::whole(image_orig.whole()),
            post: PostEvidence::image(
                &image_big_out.file_stat(),
                [0x77; 32],
                Some(image_big_out.ends()),
            )
            .unwrap(),
            orig: image_orig,
            out: image_big_out,
        },
        Case {
            name: "media at most 256 MiB",
            kind: Kind::Media,
            routes: &[Route::Upload],
            seen: Seen::whole(media_orig.whole()),
            post: PostEvidence::media(&media_out.file_stat(), Some(media_out.whole()), None)
                .unwrap(),
            orig: media_orig,
            out: media_out,
        },
        Case {
            name: "media above 256 MiB",
            kind: Kind::Media,
            routes: &[Route::Upload],
            seen: Seen::ends(media_big.ends()),
            post: PostEvidence::media(&media_big_out.file_stat(), None, Some(media_big_out.ends()))
                .unwrap(),
            orig: media_big,
            out: media_big_out,
        },
    ];
    #[derive(Debug, Clone, Copy, PartialEq)]
    enum Target {
        Original,
        Output,
        Neither,
    }
    let mut seen_cases = 0;
    for case in &cases {
        let created = FileStat {
            size: 0,
            ..case.out.file_stat()
        };
        let created_temp = FakeFile {
            size: 0,
            data: Vec::new(),
            patch: BTreeMap::new(),
            ..case.out.clone()
        };
        let mut theirs = case.out.clone();
        theirs.set(1, b"THEIRS");
        theirs.mtime = at(1_794_390_500);
        let staged = case.out.file_stat();
        let phases = [
            Phase::Naming { temp: TEMP.into() },
            Phase::Created {
                temp: TEMP.into(),
                created,
            },
            Phase::Staged {
                temp: TEMP.into(),
                created,
                staged,
                post: case.post.clone(),
            },
            Phase::Renaming {
                temp: TEMP.into(),
                created,
                staged,
                post: case.post.clone(),
            },
        ];
        for &route in case.routes {
            for phase in &phases {
                for target in [Target::Original, Target::Output, Target::Neither] {
                    let what = format!("{} {route:?} {} {target:?}", case.name, phase.word());
                    let i = intent(
                        case.kind,
                        route,
                        baseline_of(&case.orig, None),
                        case.seen.clone(),
                        phase.clone(),
                    );
                    // The temp as the phase left it; once renamed it is gone.
                    let temp = match phase {
                        _ if target == Target::Output => None,
                        Phase::Naming { .. } => Some(theirs.clone()),
                        Phase::Created { .. } => Some(created_temp.clone()),
                        _ => Some(case.out.clone()),
                    };
                    let at_target = match target {
                        Target::Original => &case.orig,
                        Target::Output => &case.out,
                        Target::Neither => &theirs,
                    };
                    let mut remote = FakeRemote::with(&[(PATH, at_target)]);
                    if let Some(t) = temp {
                        remote.files.insert(TEMP.into(), t);
                    }
                    let r = reconcile(&i, HOST, &mut remote);
                    let reached_staged = phase.staged().is_some();
                    let (outcome, verdict) = match target {
                        Target::Original => (
                            Outcome::NotLanded,
                            match phase {
                                Phase::Naming { .. } => "keep",
                                _ => "remove",
                            },
                        ),
                        Target::Output if reached_staged => (Outcome::Landed, "gone"),
                        Target::Output => (Outcome::Unknown, "gone"),
                        Target::Neither => (Outcome::Unknown, "keep"),
                    };
                    assert_eq!(r.outcome, outcome, "{what}: {}", r.line);
                    let got = match &r.temp {
                        Temp::Keep { .. } => "keep",
                        Temp::Remove(p) | Temp::Gone(p) if p != TEMP => "wrong path",
                        Temp::Remove(_) => "remove",
                        Temp::Gone(_) => "gone",
                        Temp::None => "none",
                    };
                    assert_eq!(got, verdict, "{what}: {:?}", r.temp);
                    if verdict == "keep" {
                        assert!(r.line.contains(TEMP), "{what}: {}", r.line);
                    }
                    let next = (outcome == Outcome::Landed)
                        .then(|| (case.post.next_baseline, case.post.next_seen.clone()));
                    assert_eq!(r.next, next, "{what}");
                    seen_cases += 1;
                }
            }
        }
    }
    assert_eq!(seen_cases, (2 * 2 + 4) * 4 * 3);
}
