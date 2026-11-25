//! The page server's gates (09 §3.1, X-W1's changes 1, 2, 3's server half and 5; §10 WV-S1,
//! WV-S3, WV-S4). A view's [`Table`] is driven on the test's thread, as an engine's glue would;
//! the server runs on its own worker over a connection that logs every call it is asked
//! ([`Spy`]) — `LocalFs` on a temp folder (real `realpath`, links and stats), the `Fake` (short
//! replies, a file cut under a read) and, where it is installed, OpenSSH's `sftp-server`. They
//! assert answers, their order and the calls asked, never wall-clock time; every wait is bounded.

use super::*;
use crate::remote::{
    fake, Asked, Change, Clock, Completion, CreateError, Entry, FakeFile, Like, Link, LocalFs,
    Ranged, Refused, SftpRemote, Stat, Ticket, Uploaded, Wake,
};
use crate::scheme::{Call, Head, Host, Mode, Read, Table, SCHEME, WINDOW};
use std::collections::{HashMap, HashSet};
use std::sync::{Condvar, Mutex};
use std::time::Instant;

const KIB: usize = 1024;
const MIB: usize = 1024 * 1024;
/// Only catches a hang.
const HANG: Duration = Duration::from_secs(30);

// ---------------------------------------------------------------------------------------------
// The spying connection.

/// What the connection was asked, in order — `realpath P`, `stat P`, `open P` (`into_ranged`:
/// open and fstat), `reopen P`, `read P OFF LEN` — and the calls held or failed on purpose.
#[derive(Clone, Default)]
struct Ops(Arc<(Mutex<Spied>, Condvar)>);

#[derive(Default)]
struct Spied {
    lines: Vec<String>,
    /// A call whose line starts with one of these waits there until it is released.
    held: Vec<String>,
    /// A read whose line starts with one of these gets an error for its reply (once).
    faults: Vec<String>,
}

impl Ops {
    fn note(&self, line: String) {
        let (lock, cv) = &*self.0;
        let mut s = lock.lock().unwrap();
        s.lines.push(line.clone());
        cv.notify_all();
        let deadline = Instant::now() + HANG;
        while s.held.iter().any(|h| line.starts_with(h.as_str())) {
            let left = deadline.saturating_duration_since(Instant::now());
            assert!(!left.is_zero(), "`{line}` held past the test's end");
            s = cv.wait_timeout(s, left).unwrap().0;
        }
    }

    fn hold(&self, prefix: &str) {
        self.0 .0.lock().unwrap().held.push(prefix.to_string());
    }

    fn release(&self, prefix: &str) {
        let (lock, cv) = &*self.0;
        lock.lock().unwrap().held.retain(|h| h != prefix);
        cv.notify_all();
    }

    fn fault(&self, prefix: &str) {
        self.0 .0.lock().unwrap().faults.push(prefix.to_string());
    }

    fn faulted(&self, line: &str) -> bool {
        let mut s = self.0 .0.lock().unwrap();
        match s.faults.iter().position(|f| line.starts_with(f.as_str())) {
            Some(i) => {
                s.faults.remove(i);
                true
            }
            None => false,
        }
    }

    fn lines(&self) -> Vec<String> {
        self.0 .0.lock().unwrap().lines.clone()
    }

    /// Waits until a call whose line starts with `prefix` was asked (it may be held there).
    fn wait_for(&self, prefix: &str) {
        let (lock, cv) = &*self.0;
        let deadline = Instant::now() + HANG;
        let mut s = lock.lock().unwrap();
        while !s.lines.iter().any(|l| l.starts_with(prefix)) {
            let left = deadline.saturating_duration_since(Instant::now());
            assert!(!left.is_zero(), "`{prefix}` never asked: {:?}", s.lines);
            s = cv.wait_timeout(s, left).unwrap().0;
        }
    }

    /// Lines starting with `prefix`.
    fn count(&self, prefix: &str) -> usize {
        self.lines()
            .iter()
            .filter(|l| l.starts_with(prefix))
            .count()
    }

    /// Bytes asked by the reads of `path`.
    fn read_bytes(&self, path: &str) -> u64 {
        let head = format!("read {path} ");
        self.lines()
            .iter()
            .filter_map(|l| l.strip_prefix(head.as_str()))
            .map(|rest| rest.rsplit(' ').next().unwrap().parse::<u64>().unwrap())
            .sum()
    }
}

/// A connection that notes every call on its [`Ops`] and hands every call on.
struct Spy {
    inner: Box<dyn RemoteFs + Send>,
    ops: Ops,
}

impl Spy {
    fn boxed(inner: Box<dyn RemoteFs + Send>, ops: &Ops) -> Box<dyn RemoteFs + Send> {
        Box::new(Spy {
            inner,
            ops: ops.clone(),
        })
    }
}

impl RemoteFs for Spy {
    fn stat_opt(&mut self, path: &str) -> anyhow::Result<Option<Stat>> {
        self.ops.note(format!("stat {path}"));
        self.inner.stat_opt(path)
    }
    fn lstat_opt(&mut self, path: &str) -> anyhow::Result<Option<Stat>> {
        self.ops.note(format!("lstat {path}"));
        self.inner.lstat_opt(path)
    }
    fn realpath(&mut self, path: &str) -> anyhow::Result<String> {
        self.ops.note(format!("realpath {path}"));
        self.inner.realpath(path)
    }
    fn read_head(&mut self, path: &str, max: usize) -> anyhow::Result<Vec<u8>> {
        self.ops.note(format!("read_head {path}"));
        self.inner.read_head(path, max)
    }
    fn list_dir(&mut self, path: &str) -> anyhow::Result<Vec<String>> {
        self.ops.note(format!("list {path}"));
        self.inner.list_dir(path)
    }
    fn list_entries(&mut self, path: &str) -> anyhow::Result<Vec<Entry>> {
        self.ops.note(format!("list {path}"));
        self.inner.list_entries(path)
    }
    fn read_to(
        &mut self,
        path: &str,
        out: &mut dyn std::io::Write,
        progress: &mut dyn FnMut(u64),
    ) -> anyhow::Result<u64> {
        self.ops.note(format!("read_to {path}"));
        self.inner.read_to(path, out, progress)
    }
    fn create_from(
        &mut self,
        local: &std::path::Path,
        path: &str,
        like: Option<Like>,
    ) -> std::result::Result<Uploaded, CreateError> {
        self.ops.note(format!("create {path}"));
        self.inner.create_from(local, path, like)
    }
    fn set_mode(&mut self, path: &str, mode: u32) -> anyhow::Result<()> {
        self.ops.note(format!("chmod {path}"));
        self.inner.set_mode(path, mode)
    }
    fn rename_over(&mut self, from: &str, to: &str) -> anyhow::Result<()> {
        self.ops.note(format!("rename {from}"));
        self.inner.rename_over(from, to)
    }
    fn link_new(&mut self, from: &str, to: &str) -> anyhow::Result<()> {
        self.ops.note(format!("link {from}"));
        self.inner.link_new(from, to)
    }
    fn remove(&mut self, path: &str) -> anyhow::Result<()> {
        self.ops.note(format!("remove {path}"));
        self.inner.remove(path)
    }
    fn into_ranged(self: Box<Self>, path: &str) -> std::result::Result<Box<dyn Ranged>, Refused> {
        self.ops.note(format!("open {path}"));
        let Spy { inner, ops } = *self;
        match inner.into_ranged(path) {
            Ok(file) => Ok(SpyFile::boxed(file, ops)),
            Err(Refused { conn, error }) => Err(Refused {
                conn: Spy::boxed(conn, &ops),
                error,
            }),
        }
    }
}

/// A [`Spy`]'s open file: notes its reads and reopens, and fails a read [`Ops::fault`] names.
struct SpyFile {
    inner: Box<dyn Ranged>,
    ops: Ops,
    bad: HashSet<Ticket>,
}

impl SpyFile {
    fn boxed(inner: Box<dyn Ranged>, ops: Ops) -> Box<dyn Ranged> {
        Box::new(SpyFile {
            inner,
            ops,
            bad: HashSet::new(),
        })
    }
}

impl Ranged for SpyFile {
    fn path(&self) -> &str {
        self.inner.path()
    }
    fn opened(&self) -> Stat {
        self.inner.opened()
    }
    fn read(&mut self, off: u64, len: u32) -> Ticket {
        let line = format!("read {} {off} {len}", self.inner.path());
        let bad = self.ops.faulted(&line);
        self.ops.note(line);
        let ticket = self.inner.read(off, len);
        if bad {
            self.bad.insert(ticket);
        }
        ticket
    }
    fn fstat(&mut self) -> Ticket {
        self.ops.note(format!("fstat {}", self.inner.path()));
        self.inner.fstat()
    }
    fn stat_path(&mut self) -> Ticket {
        self.ops.note(format!("stat_path {}", self.inner.path()));
        self.inner.stat_path()
    }
    fn pending(&self) -> usize {
        self.inner.pending()
    }
    fn next(&mut self, timeout: Option<Duration>) -> Option<Completion> {
        let c = self.inner.next(timeout)?;
        if self.bad.remove(&c.ticket) {
            return Some(Completion {
                reply: Err(anyhow::anyhow!("an injected read error")),
                ..c
            });
        }
        Some(c)
    }
    fn waker(&self) -> Wake {
        self.inner.waker()
    }
    fn now(&self) -> Instant {
        self.inner.now()
    }
    fn reopen(self: Box<Self>, path: &str) -> std::result::Result<Box<dyn Ranged>, Refused> {
        self.ops.note(format!("reopen {path}"));
        let SpyFile { inner, ops, .. } = *self;
        match inner.reopen(path) {
            Ok(file) => Ok(SpyFile::boxed(file, ops)),
            Err(Refused { conn, error }) => Err(Refused {
                conn: Spy::boxed(conn, &ops),
                error,
            }),
        }
    }
    fn into_connection(self: Box<Self>) -> Box<dyn RemoteFs + Send> {
        let SpyFile { inner, ops, .. } = *self;
        Spy::boxed(inner.into_connection(), &ops)
    }
    fn close(self: Box<Self>) {
        self.inner.close()
    }
}

// ---------------------------------------------------------------------------------------------
// A page's folder.

/// A temp folder holding the root `site/` and, beside it, `outside/` with a secret.
struct Site {
    _tmp: tempfile::TempDir,
    root: String,
    outside: String,
}

fn site() -> Site {
    let tmp = tempfile::tempdir().unwrap();
    // The root is a resolved path (§3.1): on macOS the temp folder is behind `/var` → `/private`.
    let base = std::fs::canonicalize(tmp.path()).unwrap();
    let root = base.join("site");
    let outside = base.join("outside");
    std::fs::create_dir(&root).unwrap();
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(outside.join("secret.txt"), b"the owner's secret").unwrap();
    std::fs::write(outside.join("secret.html"), b"<p>outside</p>").unwrap();
    Site {
        root: root.to_str().unwrap().to_string(),
        outside: outside.to_str().unwrap().to_string(),
        _tmp: tmp,
    }
}

impl Site {
    /// The absolute path of `rel` under the root.
    fn at(&self, rel: &str) -> String {
        format!("{}/{rel}", self.root)
    }

    fn put(&self, rel: &str, bytes: &[u8]) {
        let path = std::path::PathBuf::from(self.at(rel));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }

    /// Another file renamed over `rel` (open handles keep reading the one they opened).
    fn replace(&self, rel: &str, bytes: &[u8]) {
        let tmp = self.at(&format!("{rel}.new"));
        std::fs::write(&tmp, bytes).unwrap();
        std::fs::rename(tmp, self.at(rel)).unwrap();
    }

    fn dir(&self, rel: &str) {
        std::fs::create_dir_all(self.at(rel)).unwrap();
    }

    fn link(&self, rel: &str, target: &str) {
        std::os::unix::fs::symlink(target, self.at(rel)).unwrap();
    }
}

/// `n` bytes of text: lines of ASCII, so a text type's charset is `utf-8` and every place is
/// checkable (`text(n)[i]` is the same byte in any read).
fn text(n: usize) -> Vec<u8> {
    (0..n)
        .map(|i| match i % 64 {
            63 => b'\n',
            k => b'a' + (k as u8 % 26),
        })
        .collect()
}

/// `n` bytes no read placed elsewhere could match: the offset's bytes, a word at a time.
fn binary(n: usize) -> Vec<u8> {
    crate::remote::pattern(0, n)
}

// ---------------------------------------------------------------------------------------------
// The engine's side.

/// What one request got, as the engine saw it.
#[derive(Debug, Default)]
struct Got {
    head: Option<Head>,
    body: Vec<u8>,
    end: Option<std::result::Result<(), String>>,
    /// The calls, in order: `head`, `data`, `done`, `fail`.
    calls: Vec<&'static str>,
}

impl Got {
    fn code(&self) -> u16 {
        self.head.as_ref().map_or(0, Head::code)
    }

    fn mime(&self) -> &str {
        self.head.as_ref().map_or("", Head::mime)
    }

    fn header(&self, name: &str) -> Option<String> {
        let head = self.head.as_ref()?;
        let fields = head.headers();
        fields.into_iter().find(|(n, _)| *n == name).map(|(_, v)| v)
    }

    fn ok(&self) -> bool {
        self.end == Some(Ok(()))
    }

    fn failed(&self) -> bool {
        matches!(self.end, Some(Err(_)))
    }
}

/// One page view: its table on this thread, the server behind it, what came back.
struct View {
    table: Option<Table>,
    host: Host,
    mode: Mode,
    woke: Arc<(Mutex<bool>, Condvar)>,
    control: Control,
    ops: Ops,
    got: HashMap<RequestId, Got>,
    /// Every call as it came: `(request, call)`.
    events: Vec<(RequestId, &'static str)>,
}

/// A view over the site through `LocalFs`, spied, a push engine.
fn view(site: &Site, options: Options) -> View {
    let ops = Ops::default();
    let conn = Spy::boxed(Box::new(LocalFs::new()), &ops);
    view_on(conn, &site.root, options, Mode::Push, ops)
}

fn view_on(
    conn: Box<dyn RemoteFs + Send>,
    root: &str,
    options: Options,
    mode: Mode,
    ops: Ops,
) -> View {
    let (page, control) = serve(conn, root, options);
    table_view(Box::new(page), control, mode, ops)
}

fn table_view(server: Box<dyn scheme::Server>, control: Control, mode: Mode, ops: Ops) -> View {
    let host = Host::new();
    let woke = Arc::new((Mutex::new(false), Condvar::new()));
    let flag = woke.clone();
    let table = Table::new(host.clone(), server, mode, move || {
        let (lock, cv) = &*flag;
        *lock.lock().unwrap() = true;
        cv.notify_all();
    });
    View {
        table: Some(table),
        host,
        mode,
        woke,
        control,
        ops,
        got: HashMap::new(),
        events: Vec::new(),
    }
}

impl View {
    fn table(&mut self) -> &mut Table {
        self.table.as_mut().expect("the view is open")
    }

    /// The engine asks for `path` (under the root, not encoded) with `range`.
    fn get(&mut self, path: &str, range: Option<&str>) -> RequestId {
        let url = self.host.url(path);
        self.table().start(&url, range.map(str::to_string))
    }

    /// The engine asks for a path exactly as written (`raw`: from its `/`, percent-encoded).
    fn get_raw(&mut self, raw: &str) -> RequestId {
        let url = format!("{SCHEME}://{}{raw}", self.host.as_str());
        self.table().start(&url, None)
    }

    fn stop(&mut self, id: RequestId) {
        self.table().stop(id);
    }

    /// The next call the glue would make, waiting for one (bounded).
    fn call(&mut self) -> (RequestId, Call) {
        let deadline = Instant::now() + HANG;
        loop {
            if let Some(c) = self.table().next_call() {
                return c;
            }
            let (lock, cv) = &*self.woke;
            let mut woke = lock.lock().unwrap();
            while !*woke {
                let left = deadline.saturating_duration_since(Instant::now());
                assert!(!left.is_zero(), "no call within {HANG:?}");
                woke = cv.wait_timeout(woke, left).unwrap().0;
            }
            *woke = false;
        }
    }

    /// One call made, as an engine would make it, and what it says checked against the order.
    fn step(&mut self) -> (RequestId, &'static str) {
        let (id, call) = self.call();
        let kind = match &call {
            Call::Head(_) => "head",
            Call::Data(_) => "data",
            Call::Done => "done",
            Call::Readable => "readable",
            Call::Fail(_) => "fail",
        };
        let got = self.got.entry(id).or_default();
        assert!(got.end.is_none(), "{id:?}: {kind} after its end");
        match call {
            Call::Head(head) => {
                assert!(got.head.is_none(), "{id:?}: a second head");
                got.head = Some(head);
            }
            Call::Data(bytes) => {
                assert!(got.head.is_some(), "{id:?}: data before its head");
                assert!(bytes.len() <= WINDOW, "{id:?}: a piece over the window");
                got.body.extend(bytes);
            }
            Call::Done => {
                let head = got.head.as_ref().expect("done before the head");
                assert_eq!(got.body.len() as u64, head.length(), "{id:?}: done short");
                got.end = Some(Ok(()));
            }
            Call::Fail(why) => got.end = Some(Err(why)),
            Call::Readable => {}
        }
        if kind != "readable" {
            got.calls.push(kind);
            self.events.push((id, kind));
        }
        if kind == "readable" || (kind == "head" && self.mode == Mode::Pull) {
            self.pull(id);
        }
        (id, kind)
    }

    /// A pull engine reads what is there, 8 KiB a read, as WebKitGTK does.
    fn pull(&mut self, id: RequestId) {
        loop {
            let read = self.table().read(id, 8 * KIB);
            let got = self.got.entry(id).or_default();
            match read {
                Read::Bytes(bytes) => got.body.extend(bytes),
                Read::Pending | Read::Gone => return,
                Read::End => {
                    let head = got.head.as_ref().expect("an end before the head");
                    assert_eq!(got.body.len() as u64, head.length(), "{id:?}: end short");
                    got.end = Some(Ok(()));
                    got.calls.push("done");
                    self.events.push((id, "done"));
                    return;
                }
                Read::Failed(why) => {
                    got.end = Some(Err(why));
                    got.calls.push("fail");
                    self.events.push((id, "fail"));
                    return;
                }
            }
        }
    }

    /// Calls made until `id` has ended; what it got.
    fn answer(&mut self, id: RequestId) -> &Got {
        while self.got.get(&id).is_none_or(|g| g.end.is_none()) {
            self.step();
        }
        &self.got[&id]
    }

    /// Calls made until `id`'s head came.
    fn head(&mut self, id: RequestId) -> Head {
        while self.got.get(&id).is_none_or(|g| g.head.is_none()) {
            let (got, kind) = self.step();
            assert!(
                !(got == id && kind == "fail"),
                "{id:?} failed: {:?}",
                self.got[&id].end
            );
        }
        self.got[&id].head.clone().unwrap()
    }

    /// Calls made until `id` got at least `n` bytes of its body.
    fn body_at_least(&mut self, id: RequestId, n: usize) {
        while self.got.get(&id).is_none_or(|g| g.body.len() < n) {
            self.step();
        }
    }

    /// Every request before this one has been handled by the worker: a request for nothing,
    /// asked now and answered (its 404) after them.
    fn settle(&mut self) {
        let id = self.get("__settle__/nothing", None);
        let got = self.answer(id);
        assert_eq!(got.code(), 404);
    }

    /// The index in [`View::events`] of `id`'s `kind`.
    fn at(&self, id: RequestId, kind: &str) -> usize {
        self.events
            .iter()
            .position(|e| *e == (id, kind))
            .unwrap_or_else(|| panic!("{id:?} had no {kind}: {:?}", self.events))
    }
}

/// `GET path` on a fresh view, its answer.
fn fetch(view: &mut View, path: &str, range: Option<&str>) -> Got {
    let id = view.get(path, range);
    view.answer(id);
    view.got.remove(&id).unwrap()
}

// ---------------------------------------------------------------------------------------------
// §3.1's steps, every new request.

/// Steps 1–4: a new request is `realpath`, `stat`, the open (`open` + `fstat`) and then its
/// reads — X-W1's 4 RTT before the head, the first byte at 5.
#[test]
fn step_a_new_request_is_realpath_stat_open_then_its_reads() {
    let s = site();
    let page = b"<!doctype html><title>t</title><p>hello</p>\n".to_vec();
    s.put("a.html", &page);
    let mut v = view(&s, Options::viewer());
    let got = fetch(&mut v, "a.html", None);
    assert!(got.ok(), "{:?}", got.end);
    assert_eq!(got.code(), 200);
    assert_eq!(got.mime(), "text/html; charset=utf-8");
    assert_eq!(got.body, page);
    assert_eq!(got.header("Content-Length"), Some(page.len().to_string()));
    let a = s.at("a.html");
    assert_eq!(
        v.ops.lines(),
        [
            format!("realpath {a}"),
            format!("stat {a}"),
            format!("open {a}"),
            format!("read {a} 0 {}", page.len()),
        ]
    );
}

/// Step 3: a folder selects its `index.html`, which goes through steps 2 and 3 again before it
/// opens — X-W1's 6 RTT; the root itself is a folder too.
#[test]
fn step_a_folder_serves_its_index_after_the_same_checks() {
    let s = site();
    s.put("sub/index.html", b"<p>sub</p>");
    s.put("index.html", b"<p>top</p>");
    let mut v = view(&s, Options::viewer());
    let got = fetch(&mut v, "sub/", None);
    assert!(got.ok(), "{:?}", got.end);
    assert_eq!(got.body, b"<p>sub</p>");
    assert_eq!(got.mime(), "text/html; charset=utf-8");
    let (sub, index) = (s.at("sub"), s.at("sub/index.html"));
    assert_eq!(
        v.ops.lines()[..5],
        [
            format!("realpath {sub}"),
            format!("stat {sub}"),
            format!("realpath {index}"),
            format!("stat {index}"),
            format!("open {index}"),
        ]
    );
    let top = fetch(&mut v, "", None);
    assert_eq!(top.body, b"<p>top</p>");
    // A folder with no index: nothing to serve.
    s.dir("empty");
    assert_eq!(fetch(&mut v, "empty/", None).code(), 404);
}

/// Step 3: a folder asked without its `/` is not served its index at that address — the index's
/// relative links would resolve one level up — but a page sending the engine to `NAME/` (the
/// scheme has no redirect status), the name as the request had it; with the `/` it is the index,
/// whose relative links resolve inside the folder. Checked as a folder already, the same.
#[test]
fn step_a_folder_without_its_slash_sends_the_engine_to_it() {
    let s = site();
    // What a wrong base (the root) would reach instead.
    s.put("style.css", b"p{color:red}");
    s.put("sub/style.css", b"p{color:blue}");
    let index = b"<link rel=stylesheet href=\"style.css\"><p>sub</p>";
    s.put("sub/index.html", index);
    s.put("my dir/index.html", b"<p>spaced</p>");
    let mut v = view(&s, Options::viewer());
    let stub = fetch(&mut v, "sub", None);
    assert!(stub.ok(), "{:?}", stub.end);
    assert_eq!(stub.code(), 200);
    assert_eq!(stub.mime(), "text/html; charset=utf-8");
    let want = "<!doctype html><meta http-equiv=\"refresh\" content=\"0;url=sub/\">\
                <a href=\"sub/\">sub/</a>";
    assert_eq!(String::from_utf8_lossy(&stub.body), want);
    // The engine follows it, against the address it asked (`/sub`).
    let folder = relative("/sub", "sub/");
    assert_eq!(folder, "/sub/");
    assert_eq!(fetch(&mut v, &folder, None).body, index);
    let css = relative(&folder, "style.css");
    assert_eq!(css, "/sub/style.css");
    assert_eq!(fetch(&mut v, &css, None).body, b"p{color:blue}");
    // `sub` again, its folder checked by now: still the page, never the index.
    assert_eq!(fetch(&mut v, "sub", None).body, stub.body);
    let spaced = fetch(&mut v, "my dir", None);
    assert!(
        String::from_utf8_lossy(&spaced.body).contains("url=my%20dir/\""),
        "{}",
        String::from_utf8_lossy(&spaced.body)
    );
    assert_eq!(fetch(&mut v, "my dir/", None).body, b"<p>spaced</p>");
    // Nothing in a name ends the attribute or makes `NAME:` a scheme.
    let page = String::from_utf8(folder_page("/a/x\"y'<z>:w&")).unwrap();
    assert!(page.contains("url=x%22y%27%3Cz%3E%3Aw%26/\""), "{page}");
}

/// `rel` resolved against the path `base`, as a URL parser does for a plain relative path.
fn relative(base: &str, rel: &str) -> String {
    format!("{}{rel}", &base[..=base.rfind('/').unwrap()])
}

/// Step 3 (R2): an `index.html` that links out of the root is 403, nothing opened.
#[test]
fn step_an_index_that_links_out_of_the_root_is_403() {
    let s = site();
    s.dir("sub");
    s.link("sub/index.html", &format!("{}/secret.html", s.outside));
    let mut v = view(&s, Options::viewer());
    let got = fetch(&mut v, "sub/", None);
    assert_eq!(got.code(), 403);
    assert_eq!(got.calls, ["head", "done"]);
    assert!(got.body.is_empty());
    assert_eq!(v.control.refused(), 1);
    assert_eq!(v.ops.count("open "), 0, "{:?}", v.ops.lines());
}

/// Step 2: a link counts by what it points at — out of the root is 403 and counted on the row,
/// with no stat or open of what is outside; a link inside the root to inside it is served.
#[test]
fn step_a_link_out_of_the_root_is_403_and_counted() {
    let s = site();
    s.put("in.css", b"p{}");
    s.link("same.css", &s.at("in.css"));
    s.link("out.txt", &format!("{}/secret.txt", s.outside));
    s.link("outdir", &s.outside);
    let mut v = view(&s, Options::viewer());
    let out = fetch(&mut v, "out.txt", None);
    assert_eq!(out.code(), 403);
    assert_eq!(out.calls, ["head", "done"]);
    let deep = fetch(&mut v, "outdir/secret.txt", None);
    assert_eq!(deep.code(), 403);
    assert_eq!(v.control.refused(), 2);
    assert_eq!(v.ops.count("stat "), 0, "{:?}", v.ops.lines());
    assert_eq!(v.ops.count("open "), 0);
    let same = fetch(&mut v, "same.css", None);
    assert!(same.ok());
    assert_eq!(same.body, b"p{}");
    assert_eq!(same.mime(), "text/css; charset=utf-8");
}

/// Step 1: `..` in any spelling — plain, `%2e%2e`, mixed case, an encoded `/` — is 403 and
/// counted, and nothing is asked of the connection: not even a `realpath` of what is outside.
/// A NUL is refused the same way. WV-S1.
#[test]
fn step_dot_dot_in_every_spelling_and_a_nul_are_403_with_nothing_asked() {
    let s = site();
    s.put("a/b.html", b"x");
    let lines = Arc::new(Mutex::new(Vec::<String>::new()));
    let sink = lines.clone();
    let options = Options {
        log: Some(Arc::new(move |l: &str| {
            sink.lock().unwrap().push(l.to_string())
        })),
        ..Options::viewer()
    };
    let mut v = view(&s, options);
    let raws = [
        "/../outside/secret.txt",
        "/%2e%2e/outside/secret.txt",
        "/%2E%2e/outside/secret.txt",
        "/.%2E/outside/secret.txt",
        "/..%2foutside/secret.txt",
        "/%2e%2e%2f%2e%2e%2fetc/hosts",
        "/a/%2e%2e/%2e%2e/outside/secret.txt",
        "/a/b.html%00.png",
        "/%00",
    ];
    for raw in raws {
        let id = v.get_raw(raw);
        let got = v.answer(id);
        assert_eq!(got.code(), 403, "{raw}");
        assert_eq!(got.calls, ["head", "done"], "{raw}");
    }
    assert_eq!(v.control.refused(), raws.len() as u64);
    assert!(v.ops.lines().is_empty(), "{:?}", v.ops.lines());
    // The server's own request log says so, one line each (WV-S1: "403 or never asked").
    let logged = lines.lock().unwrap().clone();
    assert_eq!(
        logged.iter().filter(|l| l.contains(" 403")).count(),
        raws.len(),
        "{logged:?}"
    );
    // `.` and empty components are nothing; a `..` that stays inside is still refused.
    let fine = {
        let id = v.get_raw("/a/./b.html");
        v.answer(id).code()
    };
    assert_eq!(fine, 200);
    let id = v.get_raw("/a/../a/b.html");
    assert_eq!(v.answer(id).code(), 403);
}

/// Step 3: a name that is not there, a name no path can hold (not UTF-8), and something that is
/// neither a regular file nor a folder are 404 — a head, then its end.
#[test]
fn step_missing_and_not_regular_are_404() {
    let s = site();
    let fifo = std::ffi::CString::new(s.at("pipe")).unwrap();
    // SAFETY: a NUL-terminated path; mkfifo only makes the node.
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o644) }, 0);
    let mut v = view(&s, Options::viewer());
    for path in ["nope.html", "pipe", "no/such/dir/x.css"] {
        let got = fetch(&mut v, path, None);
        assert_eq!(got.code(), 404, "{path}");
        assert_eq!(got.calls, ["head", "done"], "{path}");
    }
    let id = v.get_raw("/%ff%fe.html");
    assert_eq!(v.answer(id).code(), 404);
    assert_eq!(v.control.refused(), 0);
    assert_eq!(v.ops.count("open "), 0);
}

// ---------------------------------------------------------------------------------------------
// X-W1 change 1: a path checked once is not checked again; small ranges from a block cache.

/// WV-S3: `bytes=2809-2812` on a 4 MiB file → 206, 4 bytes, its `Content-Range`; 40 ranges on
/// the file → one handle, one `realpath` and `stat`, and only the blocks they fall in are read.
#[test]
fn change1_forty_ranges_on_one_file_use_one_handle_and_the_block_cache() {
    let s = site();
    let size = 4 * MIB;
    let bytes = binary(size);
    s.put("v.mp4", &bytes);
    let mut v = view(&s, Options::viewer());
    let one = fetch(&mut v, "v.mp4", Some("bytes=2809-2812"));
    assert_eq!(one.code(), 206);
    assert_eq!(one.body, &bytes[2809..2813]);
    assert_eq!(one.mime(), "video/mp4");
    assert_eq!(
        one.header("Content-Range"),
        Some(format!("bytes 2809-2812/{size}"))
    );
    assert_eq!(one.header("Accept-Ranges"), Some("bytes".to_string()));
    let mut blocks = HashSet::from([0u64]);
    for i in 0..40u64 {
        // Inside one 64-byte run, so inside one block.
        let first = (i * 997 * 1024) % (size as u64 - 64);
        let first = first - first % 64;
        let last = first + (i % 7) * 8;
        blocks.insert(first / CACHE_BLOCK);
        let got = fetch(&mut v, "v.mp4", Some(&format!("bytes={first}-{last}")));
        assert!(got.ok(), "{i}: {:?}", got.end);
        assert_eq!(got.body, &bytes[first as usize..=last as usize], "{i}");
    }
    let path = s.at("v.mp4");
    assert_eq!(v.ops.count("realpath "), 1, "{:?}", v.ops.lines());
    assert_eq!(v.ops.count("stat "), 1);
    assert_eq!(v.ops.count("open "), 1);
    assert_eq!(v.ops.count("reopen "), 0);
    assert_eq!(v.ops.read_bytes(&path), blocks.len() as u64 * CACHE_BLOCK);
}

/// Change 1: a checked request on another file than the open handle's is a `reopen` — open and
/// fstat, no `realpath`, no `stat`.
#[test]
fn change1_a_checked_file_on_another_handle_is_a_reopen_with_no_checks() {
    let s = site();
    let (a, b) = (text(300 * KIB), text(400 * KIB));
    s.put("a.html", &a);
    s.put("b.css", &b);
    let mut v = view(&s, Options::viewer());
    assert_eq!(fetch(&mut v, "a.html", None).body, a);
    assert_eq!(fetch(&mut v, "b.css", None).body, b);
    let before = v.ops.lines().len();
    assert_eq!(fetch(&mut v, "a.html", None).body, a);
    let after = &v.ops.lines()[before..];
    let path = s.at("a.html");
    assert_eq!(after[0], format!("reopen {path}"));
    assert!(after[1..]
        .iter()
        .all(|l| l.starts_with(&format!("read {path} "))));
}

/// Change 1 and §3.1's fingerprint: a file rewritten between two turns fails that request at
/// the reopen — never padded, never mixed — and the next request (Cmd/Ctrl+R) checks it again
/// and serves the new file. WV-S3.
#[test]
fn fingerprint_a_file_rewritten_between_turns_fails_at_the_reopen() {
    let s = site();
    let a = binary(3 * MIB);
    s.put("a.bin", &a);
    s.put("b.bin", &binary(300 * KIB));
    let mut v = view(&s, Options::viewer());
    let first = v.get("a.bin", None);
    v.head(first);
    // Not taken by the engine: the first piece fills the window and the request waits.
    let mut grown = a.clone();
    grown.extend_from_slice(b"more");
    s.replace("a.bin", &grown);
    let other = v.get("b.bin", None);
    assert!(v.answer(other).ok());
    let got = v.answer(first);
    assert!(got.failed(), "{:?}", got.end);
    assert!(
        got.end
            .as_ref()
            .unwrap()
            .as_ref()
            .unwrap_err()
            .contains("changed"),
        "{:?}",
        got.end
    );
    assert!(got.body.len() < a.len());
    let checks = v.ops.count("realpath ");
    let again = fetch(&mut v, "a.bin", None);
    assert!(again.ok());
    assert_eq!(again.body, grown);
    assert_eq!(v.ops.count("realpath "), checks + 1, "checked again");
}

/// A whole file asked again (a reload) is opened again, its fingerprint compared — the kept
/// handle and the block cache are for ranges — and a file that moved since its check is checked
/// afresh and served as it is now: replaced by a rename while the handle is on it, or rewritten
/// in place with another size.
#[test]
fn fingerprint_a_small_file_changed_between_requests_is_served_new() {
    let s = site();
    s.put("a.css", b"p{color:red}");
    let mut v = view(&s, Options::viewer());
    assert_eq!(fetch(&mut v, "a.css", None).body, b"p{color:red}");
    s.replace("a.css", b"p{color:blue;margin:0}");
    let before = v.ops.lines().len();
    let got = fetch(&mut v, "a.css", None);
    assert!(got.ok(), "{:?}", got.end);
    assert_eq!(got.body, b"p{color:blue;margin:0}");
    let a = s.at("a.css");
    assert_eq!(
        v.ops.lines()[before..before + 4],
        [
            format!("reopen {a}"),
            format!("realpath {a}"),
            format!("stat {a}"),
            format!("open {a}"),
        ]
    );
    std::fs::write(&a, b"p{}").unwrap();
    assert_eq!(fetch(&mut v, "a.css", None).body, b"p{}");
    // Unchanged, it is opened again and served with no check.
    let before = v.ops.lines().len();
    assert_eq!(fetch(&mut v, "a.css", None).body, b"p{}");
    assert_eq!(v.ops.lines()[before], format!("reopen {a}"));
    assert_eq!(v.ops.count(&format!("realpath {a}")), 3);
}

/// Two paths to one file (a folder and its `index.html`) checked at two fingerprints: the view
/// keeps one version — the older is forgotten with its blocks — so no range is ever cut from a
/// block of another version.
#[test]
fn change1_two_paths_to_one_file_keep_one_version() {
    let s = site();
    s.put("sub/index.html", b"<p>old</p>");
    let mut v = view(&s, Options::viewer());
    let old = fetch(&mut v, "sub/index.html", Some("bytes=0-1"));
    assert_eq!(old.body, b"<p");
    let new = text(20 * KIB);
    s.replace("sub/index.html", &new);
    let got = fetch(&mut v, "sub/", Some("bytes=0-9000"));
    assert!(got.ok(), "{:?}", got.end);
    assert_eq!(got.body, &new[..=9000]);
    assert_eq!(got.mime(), "text/html; charset=utf-8");
    let again = fetch(&mut v, "sub/index.html", Some("bytes=0-1"));
    assert_eq!(again.body, &new[..2]);
}

// ---------------------------------------------------------------------------------------------
// §3.1 "served in turn", X-W1 change 2.

/// A CSS asked while a 6 MiB HTML streams is answered long before the HTML ends: one piece of
/// ≤ 1 MiB a turn.
#[test]
fn turns_a_css_asked_behind_a_long_response_ends_first() {
    let s = site();
    s.put("big.html", &text(6 * MIB));
    s.put("style.css", b"body{color:red}");
    let mut v = view(&s, Options::viewer());
    let html = v.get("big.html", None);
    v.head(html);
    let css = v.get("style.css", None);
    assert!(v.answer(css).ok());
    assert!(v.answer(html).ok());
    assert!(v.at(css, "done") < v.at(html, "done"));
    assert_eq!(v.got[&html].body, text(6 * MIB));
}

/// Change 2: a new request's first turn is its steps and its first piece, together — no other
/// request's read between its `realpath` and its first read.
#[test]
fn change2_a_new_requests_first_turn_is_its_steps_and_first_piece() {
    let s = site();
    s.put("big.bin", &binary(6 * MIB));
    s.put("small.css", b"p{}");
    let mut v = view(&s, Options::viewer());
    let big = v.get("big.bin", None);
    v.head(big);
    let css = v.get("small.css", None);
    assert!(v.answer(css).ok());
    assert!(v.answer(big).ok());
    let lines = v.ops.lines();
    let path = s.at("small.css");
    let from = lines
        .iter()
        .position(|l| *l == format!("realpath {path}"))
        .unwrap();
    let to = lines
        .iter()
        .position(|l| l.starts_with(&format!("read {path} ")))
        .unwrap();
    assert!(
        lines[from..=to].iter().all(|l| l.contains(&path)),
        "{:?}",
        &lines[from..=to]
    );
}

/// The window: a push engine that takes nothing holds the server at one window of bytes read
/// for the request; taking them lets it go on to the end.
#[test]
fn credit_an_engine_that_takes_nothing_holds_the_server_at_one_window() {
    let s = site();
    let bytes = binary(8 * MIB);
    s.put("big.bin", &bytes);
    let mut v = view(&s, Options::viewer());
    let id = v.get("big.bin", None);
    v.ops.wait_for(&format!("read {} ", s.at("big.bin")));
    std::thread::sleep(Duration::from_millis(300));
    assert!(v.ops.read_bytes(&s.at("big.bin")) <= WINDOW as u64);
    let got = v.answer(id);
    assert!(got.ok());
    assert_eq!(got.body, bytes);
}

/// A pull engine (WebKitGTK) reads the body 8 KiB a read; the credit comes back as it reads.
#[test]
fn credit_a_pull_engine_reads_the_whole_body() {
    let s = site();
    let bytes = binary(3 * MIB + 5);
    s.put("big.bin", &bytes);
    let ops = Ops::default();
    let conn = Spy::boxed(Box::new(LocalFs::new()), &ops);
    let mut v = view_on(conn, &s.root, Options::viewer(), Mode::Pull, ops);
    let id = v.get("big.bin", None);
    let got = v.answer(id);
    assert!(got.ok(), "{:?}", got.end);
    assert_eq!(got.body, bytes);
}

// ---------------------------------------------------------------------------------------------
// Range (RFC 9110 §14).

#[test]
fn range_parses_as_rfc_9110_says() {
    use Span::*;
    let cases: &[(Option<&str>, u64, Span)] = &[
        (None, 100, Whole),
        (Some("bytes=0-"), 100, Part(0, 99)),
        (Some("bytes=0-0"), 100, Part(0, 0)),
        (Some("bytes=10-19"), 100, Part(10, 19)),
        (Some("bytes=50-1000"), 100, Part(50, 99)),
        (Some("bytes=-10"), 100, Part(90, 99)),
        (Some("bytes=-1000"), 100, Part(0, 99)),
        (Some("bytes=-0"), 100, Unsatisfiable),
        (Some("bytes=100-"), 100, Unsatisfiable),
        (Some("bytes=100-200"), 100, Unsatisfiable),
        (Some("bytes=99999999999999999999999-"), 100, Unsatisfiable),
        (Some("bytes=0-99999999999999999999999"), 100, Part(0, 99)),
        (Some("bytes=5-4"), 100, Whole),
        (Some("bytes=0-1,5-6"), 100, Whole),
        (Some("bytes=0-10,5-20"), 100, Whole),
        (Some("bytes=0-1, "), 100, Part(0, 1)),
        (Some("BYTES=0-1"), 100, Part(0, 1)),
        (Some("bytes= 3-4"), 100, Part(3, 4)),
        (Some("items=0-1"), 100, Whole),
        (Some("bytes=a-b"), 100, Whole),
        (Some("bytes=1 -2"), 100, Whole),
        (Some("bytes="), 100, Whole),
        (Some("bytes=-"), 100, Whole),
        (Some("bytes"), 100, Whole),
        (Some("bytes=0-"), 0, Unsatisfiable),
        (Some("bytes=-5"), 0, Whole),
    ];
    for (range, size, want) in cases {
        assert_eq!(span(*range, *size), *want, "{range:?} of {size}");
    }
}

/// 206 with `Content-Range` and `Accept-Ranges`; 416 past the end (a head with `bytes */T`,
/// then its end); several ranges → the whole file as 200; an open-ended range streams.
#[test]
fn range_answers_206_416_and_200() {
    let s = site();
    let size = 3 * MIB;
    let bytes = binary(size);
    s.put("f.bin", &bytes);
    let mut v = view(&s, Options::viewer());
    let past = fetch(&mut v, "f.bin", Some(&format!("bytes={size}-")));
    assert_eq!(past.code(), 416);
    assert_eq!(past.calls, ["head", "done"]);
    assert_eq!(
        past.header("Content-Range"),
        Some(format!("bytes */{size}"))
    );
    assert_eq!(fetch(&mut v, "f.bin", Some("bytes=-0")).code(), 416);
    let multi = fetch(&mut v, "f.bin", Some("bytes=0-1,10-20"));
    assert_eq!(multi.code(), 200);
    assert_eq!(multi.body, bytes);
    let tail = fetch(&mut v, "f.bin", Some("bytes=1000000-"));
    assert_eq!(tail.code(), 206);
    assert_eq!(tail.body, &bytes[1_000_000..]);
    assert_eq!(
        tail.header("Content-Range"),
        Some(format!("bytes 1000000-{}/{size}", size - 1))
    );
    let suffix = fetch(&mut v, "f.bin", Some("bytes=-300"));
    assert_eq!(suffix.body, &bytes[size - 300..]);
    let across = fetch(&mut v, "f.bin", Some("bytes=262140-262150"));
    assert_eq!(across.body, &bytes[262_140..=262_150]);
    s.put("empty.bin", b"");
    let empty = fetch(&mut v, "empty.bin", None);
    assert!(empty.ok());
    assert_eq!(empty.code(), 200);
    assert_eq!(empty.calls, ["head", "done"]);
    assert_eq!(fetch(&mut v, "empty.bin", Some("bytes=0-")).code(), 416);
}

// ---------------------------------------------------------------------------------------------
// The type and charset tables.

#[test]
fn type_by_extension_from_the_fixed_table() {
    let table = [
        ("a.html", "text/html"),
        ("A.HTM", "text/html"),
        ("feed.xml", "application/xml"),
        ("s.xsl", "text/xsl"),
        ("s.xslt", "text/xsl"),
        ("s.css", "text/css"),
        ("a.js", "text/javascript"),
        ("a.mjs", "text/javascript"),
        ("d.json", "application/json"),
        ("i.svg", "image/svg+xml"),
        ("i.png", "image/png"),
        ("i.jpg", "image/jpeg"),
        ("i.jpeg", "image/jpeg"),
        ("i.gif", "image/gif"),
        ("i.webp", "image/webp"),
        ("f.woff", "font/woff"),
        ("f.woff2", "font/woff2"),
        ("f.ttf", "font/ttf"),
        ("f.otf", "font/otf"),
        ("v.mp4", "video/mp4"),
        ("v.webm", "video/webm"),
        ("s.mp3", "audio/mpeg"),
        ("s.ogg", "audio/ogg"),
        ("s.wav", "audio/wav"),
        ("d.pdf", "application/pdf"),
        ("notes.txt", "application/octet-stream"),
        ("archive.tar.gz", "application/octet-stream"),
        ("Makefile", "application/octet-stream"),
        (".html", "application/octet-stream"),
    ];
    for (name, want) in table {
        assert_eq!(mime(name), want, "{name}");
    }
}

/// The charset rule (§3.1, 08 §7.2): `crate::encoding::pick` on the first 8 KiB, UTF-8 only by
/// its BOM or `kind::utf8_text` (a character cut at the 8 KiB edge accepted), UTF-16/32 by BOM or
/// the NUL pattern, anything else none; and none when the text names its own.
#[test]
fn charset_follows_the_text_views_default() {
    let korean = "한국어 페이지\n".repeat(400);
    assert_eq!(charset(korean.as_bytes(), false), Some("utf-8"));
    // A Hangul syllable (3 bytes) cut at byte 8192, the sample's edge: accepted when the file
    // goes on (WV-S4), not when it ends there.
    let mut cut = vec![b'a'; 8190];
    cut.extend_from_slice("한".as_bytes());
    let sample = &cut[..8192];
    assert_eq!(charset(sample, true), Some("utf-8"));
    assert_eq!(charset(sample, false), None);
    // A UTF-8 BOM decides, whatever follows.
    assert_eq!(charset(b"\xEF\xBB\xBF<p>\xFF</p>", false), Some("utf-8"));
    // UTF-16 by its BOM, or by the NUL pattern.
    let wide = |s: &str, be: bool| -> Vec<u8> {
        s.encode_utf16()
            .flat_map(|u| if be { u.to_be_bytes() } else { u.to_le_bytes() })
            .collect()
    };
    let mut le = vec![0xFF, 0xFE];
    le.extend(wide("<p>안녕</p>", false));
    assert_eq!(charset(&le, false), Some("utf-16le"));
    let mut be = vec![0xFE, 0xFF];
    be.extend(wide("<p>안녕</p>", true));
    assert_eq!(charset(&be, false), Some("utf-16be"));
    assert_eq!(
        charset(&wide("<html><p>hello</p></html>", false), false),
        Some("utf-16le")
    );
    assert_eq!(
        charset(&wide("<html><p>hello</p></html>", true), false),
        Some("utf-16be")
    );
    assert_eq!(
        charset(b"\xFF\xFE\x00\x00<\x00\x00\x00", false),
        Some("utf-32le")
    );
    assert_eq!(
        charset(b"\x00\x00\xFE\xFF\x00\x00\x00<", false),
        Some("utf-32be")
    );
    // CP949 / EUC-KR bytes ("한국어"): step 3 is VM-4's, so step 4 — and they are not UTF-8.
    let euc_kr = b"<p>\xC7\xD1\xB1\xB9\xBE\xEE</p>".repeat(20);
    assert_eq!(charset(&euc_kr, false), None);
    // Binary: none.
    assert_eq!(
        charset(b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR\x80\x81", false),
        None
    );
    // The text names its own: none, the document's own rule decides.
    for named in [
        &b"<!doctype html><meta charset=\"euc-kr\"><p>a</p>"[..],
        b"<HTML><HEAD><META HTTP-EQUIV=\"Content-Type\" CONTENT=\"text/html; charset=euc-kr\">",
        b"<meta\tname=x charset=euc-kr>",
        b"<?xml version=\"1.0\" encoding=\"EUC-KR\"?><rss/>",
        b"\xEF\xBB\xBF<?xml version='1.0' encoding='utf-8'?><a/>",
        b"@charset \"euc-kr\";\np { color: red }",
    ] {
        assert_eq!(
            charset(named, false),
            None,
            "{}",
            String::from_utf8_lossy(named)
        );
    }
    // A meta tag that names no charset names nothing.
    assert_eq!(
        charset(
            b"<meta name=\"viewport\" content=\"width=device-width\">",
            false
        ),
        Some("utf-8")
    );
    assert_eq!(charset(b"", false), Some("utf-8"));
}

/// A text type's head waits for the read that brings its first 8 KiB; any other type's head
/// goes as soon as the steps end.
#[test]
fn charset_a_text_head_waits_for_its_first_read_and_others_do_not() {
    let s = site();
    s.put("big.png", &binary(2 * MIB));
    s.put("big.html", &text(2 * MIB));
    let mut v = view(&s, Options::viewer());
    let (png, html) = (s.at("big.png"), s.at("big.html"));
    v.ops.hold(&format!("read {png} "));
    v.ops.hold(&format!("read {html} "));
    let id = v.get("big.png", None);
    assert_eq!(v.head(id).mime(), "image/png");
    v.ops.release(&format!("read {png} "));
    assert!(v.answer(id).ok());
    let id = v.get("big.html", None);
    v.ops.wait_for(&format!("read {html} "));
    // The worker is held at the HTML's first read: nothing of it has come.
    assert!(v.table().next_call().is_none());
    v.ops.release(&format!("read {html} "));
    assert_eq!(v.head(id).mime(), "text/html; charset=utf-8");
    assert!(v.answer(id).ok());
    assert_eq!(v.got[&id].body, text(2 * MIB));
}

/// WV-S4's files through the server: Korean UTF-8 with no meta, a syllable cut at byte 8192, and
/// a bad byte far past the first 8 KiB (the sample is the first 8 KiB, never the whole read);
/// EUC-KR HTML naming its charset; a UTF-16 stylesheet; a picture.
#[test]
fn charset_on_the_wire_wv_s4() {
    let s = site();
    let mut korean = vec![b'a'; 8190];
    korean.extend_from_slice("한국어 문서\n".repeat(30_000).as_bytes());
    korean[20 * KIB] = 0xFF;
    s.put("ko.html", &korean);
    let mut euc = b"<!doctype html><meta charset=euc-kr>".to_vec();
    euc.extend(vec![b' '; 9000]);
    euc.extend_from_slice(b"<p>\xC7\xD1\xB1\xB9\xBE\xEE</p>");
    s.put("euc.html", &euc);
    let mut css = vec![0xFF, 0xFE];
    css.extend("p{}".encode_utf16().flat_map(u16::to_le_bytes));
    s.put("wide.css", &css);
    s.put("i.png", &binary(1000));
    let mut v = view(&s, Options::viewer());
    let ko = fetch(&mut v, "ko.html", None);
    assert_eq!(ko.mime(), "text/html; charset=utf-8");
    assert_eq!(ko.body, korean);
    assert_eq!(fetch(&mut v, "euc.html", None).mime(), "text/html");
    assert_eq!(
        fetch(&mut v, "wide.css", None).mime(),
        "text/css; charset=utf-16le"
    );
    assert_eq!(fetch(&mut v, "i.png", None).mime(), "image/png");
    // A range far into a text file still takes the file's first 8 KiB for its charset.
    let part = fetch(&mut v, "ko.html", Some("bytes=300000-300010"));
    assert_eq!(part.mime(), "text/html; charset=utf-8");
    assert_eq!(part.body, &korean[300_000..=300_010]);
}

// ---------------------------------------------------------------------------------------------
// Cancel (§3.1) and X-W1 change 3's server half.

/// A stop during the steps ends the request after the step in progress: its file is never
/// opened, and nothing of it reaches the engine.
#[test]
fn stop_during_the_steps_ends_the_request_after_the_step_in_progress() {
    let s = site();
    s.put("a.html", b"<p>a</p>");
    let mut v = view(&s, Options::viewer());
    let a = s.at("a.html");
    v.ops.hold(&format!("stat {a}"));
    let id = v.get("a.html", None);
    v.ops.wait_for(&format!("stat {a}"));
    v.stop(id);
    v.ops.release(&format!("stat {a}"));
    v.settle();
    assert!(!v.got.contains_key(&id), "{:?}", v.got.get(&id));
    assert_eq!(v.ops.count(&format!("open {a}")), 0, "{:?}", v.ops.lines());
}

/// A stop while a text head waits for its first read (Mac: `stopURLSchemeTask` before any
/// response; Linux: the resource's `failed`, `Table::stop_headless`): no head ever, and no read
/// after the piece in flight.
#[test]
fn stop_before_the_head_while_it_waits_for_the_first_read() {
    let s = site();
    s.put("big.html", &text(4 * MIB));
    let mut v = view(&s, Options::viewer());
    let html = s.at("big.html");
    v.ops.hold(&format!("read {html} "));
    let url = v.host.url("big.html");
    let id = v.get("big.html", None);
    v.ops.wait_for(&format!("read {html} "));
    assert_eq!(v.table().stop_headless(&url, None), Some(id));
    v.ops.release(&format!("read {html} "));
    v.settle();
    assert!(!v.got.contains_key(&id), "{:?}", v.got.get(&id));
    assert!(v.ops.read_bytes(&html) <= PIECE);
}

/// WV-S3: a view stops a long response mid-body: reads stop within one window more than the
/// engine took, and nothing more of it reaches the engine.
#[test]
fn stop_mid_body_reads_stop_within_one_window() {
    let s = site();
    s.put("big.bin", &binary(16 * MIB));
    let mut v = view(&s, Options::viewer());
    let id = v.get("big.bin", None);
    v.body_at_least(id, 3 * MIB / 2);
    let taken = v.got[&id].body.len() as u64;
    v.stop(id);
    let calls = v.got[&id].calls.len();
    v.settle();
    assert_eq!(v.got[&id].calls.len(), calls, "a call after the stop");
    assert!(v.got[&id].end.is_none());
    let read = v.ops.read_bytes(&s.at("big.bin"));
    assert!(read <= taken + WINDOW as u64, "{read} read, {taken} taken");
}

/// A stop after the last byte but before the end (Mac `<video>`): the end is never called, and
/// the view goes on serving.
#[test]
fn stop_after_the_last_byte_sends_nothing_more() {
    let s = site();
    s.put("a.css", b"p{color:blue}");
    let mut v = view(&s, Options::viewer());
    let id = v.get("a.css", None);
    v.head(id);
    v.body_at_least(id, 13);
    v.stop(id);
    v.settle();
    assert_eq!(v.got[&id].calls, ["head", "data"]);
    assert!(fetch(&mut v, "a.css", None).ok());
}

/// The view closes (its table drops): every request stops, the worker ends after the piece in
/// flight and gives its connection back — the preview's session (06 §13).
#[test]
fn closing_the_view_stops_all_and_gives_the_connection_back() {
    let s = site();
    s.put("a.bin", &binary(8 * MIB));
    s.put("b.bin", &binary(8 * MIB));
    let (tx, rx) = std::sync::mpsc::channel();
    let options = Options {
        give_back: Some(Box::new(move |conn| {
            let _ = tx.send(conn);
        })),
        ..Options::viewer()
    };
    let mut v = view(&s, options);
    let a = v.get("a.bin", None);
    let b = v.get("b.bin", None);
    v.body_at_least(a, MIB);
    drop(v.table.take());
    let mut conn = rx.recv_timeout(HANG).expect("the connection back");
    assert!(conn.stat_opt(&s.at("a.bin")).unwrap().is_some());
    // Each request read at most one window past what the engine took of it.
    let taken: u64 = [a, b]
        .iter()
        .map(|id| v.got.get(id).map_or(0, |g| g.body.len() as u64))
        .sum();
    let read = v.ops.read_bytes(&s.at("a.bin")) + v.ops.read_bytes(&s.at("b.bin"));
    assert!(
        read <= taken + 2 * WINDOW as u64,
        "{read} read, {taken} taken"
    );
    assert!(read < 16 * MIB as u64);
}

// ---------------------------------------------------------------------------------------------
// The preview's budget and its lift.

/// A preview reads at most its budget: a request that would read past it fails before its head
/// (counted as cut); a stopped request gives back what it did not read; the lift (the
/// hand-over) says a reload is due and serves everything.
#[test]
fn budget_fails_past_it_refunds_a_stop_and_lifts() {
    assert_eq!(Options::preview().budget, Some(32 << 20));
    assert_eq!(Options::viewer().budget, None);
    let s = site();
    s.put("a.css", &text(300 * KIB));
    s.put("big.bin", &binary(900 * KIB));
    let options = Options {
        budget: Some(MIB as u64),
        ..Options::viewer()
    };
    let mut v = view(&s, options);
    assert!(fetch(&mut v, "a.css", None).ok());
    let big = fetch(&mut v, "big.bin", None);
    assert!(big.failed());
    assert!(big.head.is_none());
    assert!(v.control.cut());
    assert!(v.control.lift(), "a reload is due");
    assert_eq!(fetch(&mut v, "big.bin", None).body, binary(900 * KIB));

    let s = site();
    s.put("long.bin", &binary(5 * MIB / 2));
    s.put("next.bin", &binary(9 * MIB / 5));
    let options = Options {
        budget: Some(3 * MIB as u64),
        ..Options::viewer()
    };
    let mut v = view(&s, options);
    let id = v.get("long.bin", None);
    v.head(id);
    v.stop(id);
    v.settle();
    let next = fetch(&mut v, "next.bin", None);
    assert!(
        next.ok(),
        "the stop gave back what it never read: {:?}",
        next.end
    );
    assert!(!v.control.cut());
}

// ---------------------------------------------------------------------------------------------
// The root.

#[test]
fn root_is_the_opened_folder_when_the_file_is_inside_it_else_the_files_folder() {
    let s = site();
    s.put("docs/api/a.html", b"x");
    s.link("linked.html", &format!("{}/secret.html", s.outside));
    let mut fs = LocalFs::new();
    let r = root(&mut fs, Some(&s.root), &s.at("docs/api/a.html")).unwrap();
    assert_eq!(
        (r.root.as_str(), r.path.as_str()),
        (s.root.as_str(), "docs/api/a.html")
    );
    let other = format!("{}/..", s.outside);
    let r = root(&mut fs, Some(&s.at("docs/api")), &s.at("docs/api/a.html")).unwrap();
    assert_eq!(r.root, s.at("docs/api"));
    assert_eq!(r.path, "a.html");
    let r = root(&mut fs, Some(&other), &s.at("docs/api/a.html")).unwrap();
    assert_eq!(
        r.root,
        std::fs::canonicalize(&other).unwrap().to_str().unwrap()
    );
    let r = root(&mut fs, Some(&s.at("docs")), &s.at("docs/api/a.html")).unwrap();
    assert_eq!(r.path, "api/a.html");
    // A link in the opened folder to a file outside it: the file's own folder.
    let r = root(&mut fs, Some(&s.root), &s.at("linked.html")).unwrap();
    assert_eq!(
        (r.root.as_str(), r.path.as_str()),
        (s.outside.as_str(), "secret.html")
    );
    let r = root(&mut fs, None, &s.at("docs/api/a.html")).unwrap();
    assert_eq!(r.root, s.at("docs/api"));
}

// ---------------------------------------------------------------------------------------------
// X-W1 change 5.

/// Change 5, pinned as it is: a page of R new files costs R × (realpath, stat, open) on one
/// handle, strictly in series — no batched `RemoteFs` call nor several-handle `Ranged` exists to
/// pipeline them.
#[test]
fn change5_a_page_of_r_files_costs_r_times_the_steps() {
    let s = site();
    let names = ["index.html", "s.css", "a.png", "b.png", "c.png"];
    for n in names {
        s.put(n, &binary(1000));
    }
    let mut v = view(&s, Options::viewer());
    for n in names {
        assert!(fetch(&mut v, n, None).ok());
    }
    let lines = v.ops.lines();
    let mut want = Vec::new();
    for n in names {
        let p = s.at(n);
        want.extend([
            format!("realpath {p}"),
            format!("stat {p}"),
            format!("open {p}"),
            format!("read {p} 0 1000"),
        ]);
    }
    assert_eq!(lines, want);
}

/// Change 5: while another request waits, a piece is what the link carries in about 100 ms
/// (whole reads, at least one), else the whole window.
#[test]
fn change5_pieces_shrink_to_link_time_while_another_request_waits() {
    assert_eq!(piece_for(None, true), PIECE);
    assert_eq!(piece_for(Some(2e6), false), PIECE);
    assert_eq!(piece_for(Some(2e6), true), 3 * READ);
    assert_eq!(piece_for(Some(1e3), true), READ);
    assert_eq!(piece_for(Some(1e12), true), PIECE);
}

/// Change 5 on a simulated link (X-W1's: 50 ms, 2 MB/s): once the link's rate is measured, the
/// piece of a long response planned while another request waits is what 100 ms carries (three
/// reads), not the whole window — the request behind it waits that long, not 0.52 s.
#[test]
fn change5_on_a_simulated_link_a_waiting_request_holds_back_only_a_short_piece() {
    let size = 20 * MIB as u64;
    let link = Link {
        latency: Duration::from_millis(50),
        rate: 2_000_000,
        clip: None,
        clock: Clock::Simulated,
    };
    let (file, control) = fake("/site/v.bin", FakeFile::pattern(size), link);
    let ops = Ops::default();
    let conn = Spy::boxed(file.into_connection(), &ops);
    let mut v = view_on(conn, "/site", Options::viewer(), Mode::Push, ops);
    let big = v.get("v.bin", Some("bytes=0-"));
    v.head(big);
    let at = 38 * CACHE_BLOCK;
    let small = v.get("v.bin", Some(&format!("bytes={at}-{}", at + 10)));
    let got = v.answer(small);
    assert!(got.ok(), "{:?}", got.end);
    assert_eq!(got.body, crate::remote::pattern(at, 11));
    let reads: Vec<(u64, u64)> = control
        .sent()
        .iter()
        .filter_map(|r| match r.asked {
            Asked::Read { off, len } => Some((off, u64::from(len))),
            _ => None,
        })
        .collect();
    let block = reads
        .iter()
        .position(|(off, _)| (at..at + CACHE_BLOCK).contains(off))
        .expect("the small request's block was read");
    // The big one's first piece was planned alone (the whole window); what it read after that,
    // before the small one's block, is at most one short piece.
    let before: u64 = reads[..block].iter().map(|(_, len)| len).sum();
    assert!(before >= PIECE, "{before}");
    assert!(
        before - PIECE <= 3 * READ,
        "{} read in between",
        before - PIECE
    );
    v.stop(big);
}

// ---------------------------------------------------------------------------------------------
// The transport.

/// A server that clips its replies: each short reply's rest is read at its own offset (the
/// `Ranged` contract), and the body is whole; a file cut under a read is an early end that fails
/// the request, never padded.
#[test]
fn transport_a_short_reply_is_followed_and_an_early_end_fails() {
    let size = 300 * KIB as u64;
    let link = Link {
        clip: Some(10_000),
        ..Link::gate()
    };
    let (file, control) = fake("/site/f.bin", FakeFile::pattern(size), link);
    let ops = Ops::default();
    let conn = Spy::boxed(file.into_connection(), &ops);
    let mut v = view_on(conn, "/site", Options::viewer(), Mode::Push, ops);
    let got = fetch(&mut v, "f.bin", None);
    assert!(got.ok(), "{:?}", got.end);
    assert_eq!(got.body, crate::remote::pattern(0, size as usize));
    control.set_link(|l| l.clip = None);
    control.change(Change::Truncate(100 * KIB as u64));
    let cut = fetch(&mut v, "f.bin", Some("bytes=0-"));
    assert!(cut.failed(), "{:?}", cut.end);
}

/// A read that fails mid-piece fails its request; the piece's other reads are drained, so the
/// next request on the same handle gets its own bytes.
#[test]
fn transport_a_read_error_mid_piece_leaves_the_handle_clean() {
    let s = site();
    let bytes = binary(2 * MIB);
    s.put("big.bin", &bytes);
    let mut v = view(&s, Options::viewer());
    v.ops.fault(&format!("read {} 131072 ", s.at("big.bin")));
    let bad = fetch(&mut v, "big.bin", None);
    assert!(bad.failed(), "{:?}", bad.end);
    let next = fetch(&mut v, "big.bin", Some("bytes=600000-1700000"));
    assert!(next.ok(), "{:?}", next.end);
    assert_eq!(next.body, &bytes[600_000..=1_700_000]);
}

/// Over SFTP (OpenSSH's `sftp-server` on this machine): the steps, the checks and the body hold.
#[test]
fn over_sftp_the_steps_and_the_checks_hold() {
    let Some(sftp) = SftpRemote::local_server() else {
        return;
    };
    let s = site();
    let page = text(700 * KIB);
    s.put("sub/index.html", &page);
    s.link("out.txt", &format!("{}/secret.txt", s.outside));
    let ops = Ops::default();
    let conn = Spy::boxed(Box::new(sftp), &ops);
    let mut v = view_on(conn, &s.root, Options::viewer(), Mode::Push, ops);
    let got = fetch(&mut v, "sub/", None);
    assert!(got.ok(), "{:?}", got.end);
    assert_eq!(got.body, page);
    assert_eq!(fetch(&mut v, "out.txt", None).code(), 403);
    let id = v.get_raw("/%2e%2e/outside/secret.txt");
    assert_eq!(v.answer(id).code(), 403);
    let part = fetch(&mut v, "sub/index.html", Some("bytes=10-19"));
    assert_eq!(part.body, &page[10..20]);
    assert_eq!(v.control.refused(), 2);
}

/// A local page is read in place (06 §1), under the throttle hook as every stream is.
#[test]
fn a_local_page_is_served_in_place() {
    let s = site();
    let page = text(1500 * KIB);
    s.put("a.html", &page);
    s.put("b.css", b"p{}");
    s.link("out.txt", &format!("{}/secret.txt", s.outside));
    let options = Options {
        throttle: Some(Throttle {
            delay: Duration::from_millis(1),
            rate: 1 << 30,
        }),
        ..Options::viewer()
    };
    let (page_server, control) = serve_local(&s.root, options);
    let mut v = table_view(Box::new(page_server), control, Mode::Push, Ops::default());
    assert_eq!(fetch(&mut v, "a.html", None).body, page);
    assert_eq!(fetch(&mut v, "b.css", None).body, b"p{}");
    assert_eq!(
        fetch(&mut v, "a.html", Some("bytes=700000-")).body,
        &page[700_000..]
    );
    assert_eq!(fetch(&mut v, "out.txt", None).code(), 403);
}
