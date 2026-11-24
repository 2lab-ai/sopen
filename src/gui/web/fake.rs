//! The test kit's engine (09 §11): it renders nothing and records every call the component makes,
//! per view, in order. A file view's requests go through a real [`scheme::Table`] (push, as
//! WKWebView takes them), so a test asks as WebKit would ([`FakeEngine::request`]) and sees what
//! the glue would call ([`FakeEngine::pump`]).

use super::{Engine, Events, Opened, Opening, Parent, Placement, View};
use crate::scheme::{self, Call, Host, RequestId, Table};
use eframe::egui;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

/// A call the fake got: [`Engine::open`] and every [`View`] method.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Did {
    Open {
        page: bool,
        preview: bool,
        scripts: bool,
    },
    Attach(Parent),
    Place(egui::Rect, f32),
    Hidden(bool),
    Paint(egui::Rect),
    Input(egui::Event),
    Load(String),
    Reload,
    Scripts(bool),
    Preview(bool),
    Shown(bool),
    Snapshot,
    TakeKeyboard,
    Close,
}

type Done = Box<dyn FnOnce(Option<egui::ColorImage>)>;

/// One view the fake opened.
struct Opened1 {
    /// What [`View::page_has_keyboard`] says (macOS reads AppKit's first responder).
    keyboard: Rc<Cell<bool>>,
    /// A file view's requests; `None` for an address, or once the view closed.
    table: Rc<RefCell<Option<Table>>>,
    host: Option<Host>,
    events: Events,
    /// Wakes the table gave (the outbox has something).
    woke: Arc<AtomicUsize>,
    /// Snapshots asked for and not answered yet.
    pending: Rc<RefCell<Vec<Done>>>,
}

#[derive(Default)]
struct Shared {
    placement: Cell<Option<Placement>>,
    version: RefCell<String>,
    /// [`Engine::uncontained`]: scripts off, scripts on.
    uncontained: RefCell<[Option<String>; 2]>,
    /// [`Engine::open`] fails with this.
    refuse: RefCell<Option<String>>,
    log: RefCell<Vec<(usize, Did)>>,
    monitor: RefCell<Vec<bool>>,
    views: RefCell<Vec<Opened1>>,
    /// What a snapshot answers with; `None`: kept until [`FakeEngine::answer`].
    picture: RefCell<Option<egui::ColorImage>>,
}

/// The recording engine. Clones share everything.
#[derive(Clone, Default)]
pub(crate) struct FakeEngine(Rc<Shared>);

impl FakeEngine {
    /// An engine that places its views as `placement` says, version `2.52.6`, every channel
    /// contained, answering a snapshot at once with a 1×1 picture.
    pub(crate) fn new(placement: Placement) -> Self {
        let fake = FakeEngine::default();
        fake.0.placement.set(Some(placement));
        *fake.0.version.borrow_mut() = "2.52.6".to_string();
        *fake.0.picture.borrow_mut() = Some(egui::ColorImage::filled(
            [1, 1],
            egui::Color32::from_rgb(1, 2, 3),
        ));
        fake
    }

    /// macOS's kind: a native view.
    pub(crate) fn native() -> Self {
        Self::new(Placement::Native)
    }

    /// Linux's kind: a texture.
    pub(crate) fn texture() -> Self {
        Self::new(Placement::Texture)
    }

    /// This thread's engine from now on (before its first `Web::new`).
    pub(crate) fn install(&self) -> &Self {
        super::use_engine(Ok(Rc::new(self.clone())));
        self
    }

    pub(crate) fn set_version(&self, version: &str) {
        *self.0.version.borrow_mut() = version.to_string();
    }

    /// The channel not contained with scripts off (`false`) or on (`true`).
    pub(crate) fn set_uncontained(&self, scripts: bool, channel: Option<&str>) {
        self.0.uncontained.borrow_mut()[usize::from(scripts)] = channel.map(str::to_string);
    }

    pub(crate) fn refuse_open(&self, why: Option<&str>) {
        *self.0.refuse.borrow_mut() = why.map(str::to_string);
    }

    /// Snapshots answer with `picture` at once, or (`None`) wait for [`FakeEngine::answer`].
    pub(crate) fn set_picture(&self, picture: Option<egui::ColorImage>) {
        *self.0.picture.borrow_mut() = picture;
    }

    /// Answers view `n`'s waiting snapshots with `picture`.
    pub(crate) fn answer(&self, n: usize, picture: Option<egui::ColorImage>) {
        let pending = std::mem::take(&mut *self.0.views.borrow()[n].pending.borrow_mut());
        for done in pending {
            done(picture.clone());
        }
    }

    /// Views opened so far.
    pub(crate) fn views(&self) -> usize {
        self.0.views.borrow().len()
    }

    /// Every call view `n` got, in order.
    pub(crate) fn calls(&self, n: usize) -> Vec<Did> {
        let log = self.0.log.borrow();
        log.iter()
            .filter(|(v, _)| *v == n)
            .map(|(_, d)| d.clone())
            .collect()
    }

    /// View `n`'s calls since the last [`FakeEngine::take`] of it.
    pub(crate) fn take(&self, n: usize) -> Vec<Did> {
        let mut log = self.0.log.borrow_mut();
        let (mine, rest) = std::mem::take(&mut *log)
            .into_iter()
            .partition(|(v, _)| *v == n);
        *log = rest;
        mine.into_iter().map(|(_, d): (usize, Did)| d).collect()
    }

    /// The key monitor's switches, in order.
    pub(crate) fn monitor(&self) -> Vec<bool> {
        self.0.monitor.borrow().clone()
    }

    /// The page in view `n` has the keyboard (macOS: WebKit became first responder) or not.
    pub(crate) fn set_page_keyboard(&self, n: usize, has: bool) {
        self.0.views.borrow()[n].keyboard.set(has);
    }

    /// Where view `n` says its events, as the engine.
    pub(crate) fn events(&self, n: usize) -> Events {
        self.0.views.borrow()[n].events.clone()
    }

    /// View `n`'s scheme host (a file view).
    pub(crate) fn host(&self, n: usize) -> Host {
        self.0.views.borrow()[n].host.clone().expect("a file view")
    }

    /// WebKit asks view `n` for `url` (a file view).
    pub(crate) fn request(&self, n: usize, url: &str, range: Option<&str>) -> RequestId {
        self.with_table(n, |t| t.start(url, range.map(str::to_string)))
    }

    /// WebKit stops request `id` of view `n`.
    pub(crate) fn stop(&self, n: usize, id: RequestId) {
        self.with_table(n, |t| t.stop(id));
    }

    /// What view `n`'s glue would call now, in order: the table drained.
    pub(crate) fn pump(&self, n: usize) -> Vec<(RequestId, Call)> {
        let table = self.0.views.borrow()[n].table.clone();
        let mut out = vec![];
        loop {
            let next = table.borrow_mut().as_mut().and_then(Table::next_call);
            match next {
                Some(call) => out.push(call),
                None => return out,
            }
        }
    }

    /// The wakes view `n`'s table gave.
    pub(crate) fn woke(&self, n: usize) -> usize {
        self.0.views.borrow()[n].woke.load(Ordering::SeqCst)
    }

    /// Whether view `n` still has its requests' table (it goes when the view closes).
    pub(crate) fn has_table(&self, n: usize) -> bool {
        self.0.views.borrow()[n].table.borrow().is_some()
    }

    fn with_table<R>(&self, n: usize, f: impl FnOnce(&mut Table) -> R) -> R {
        let table = self.0.views.borrow()[n].table.clone();
        let mut table = table.borrow_mut();
        f(table.as_mut().expect("a file view that is open"))
    }

    fn record(&self, n: usize, did: Did) {
        self.0.log.borrow_mut().push((n, did));
    }
}

impl Engine for FakeEngine {
    fn placement(&self) -> Placement {
        self.0.placement.get().unwrap_or(Placement::Native)
    }

    fn version(&self) -> String {
        self.0.version.borrow().clone()
    }

    fn uncontained(&self, scripts: bool) -> Option<String> {
        self.0.uncontained.borrow()[usize::from(scripts)].clone()
    }

    fn open(&self, opening: Opening) -> Result<Box<dyn View>, String> {
        if let Some(why) = self.0.refuse.borrow().clone() {
            return Err(why);
        }
        let n = self.views();
        let woke = Arc::new(AtomicUsize::new(0));
        let (page, table, host) = match opening.content {
            Opened::Page { host, server } => {
                let count = woke.clone();
                let table = Table::new(host.clone(), server, scheme::Mode::Push, move || {
                    count.fetch_add(1, Ordering::SeqCst);
                });
                (true, Some(table), Some(host))
            }
            Opened::Address { .. } => (false, None, None),
        };
        self.record(
            n,
            Did::Open {
                page,
                preview: opening.preview,
                scripts: opening.scripts,
            },
        );
        let keyboard = Rc::new(Cell::new(false));
        let pending = Rc::new(RefCell::new(Vec::new()));
        let table = Rc::new(RefCell::new(table));
        self.0.views.borrow_mut().push(Opened1 {
            keyboard: keyboard.clone(),
            table: table.clone(),
            host,
            events: opening.events,
            woke,
            pending: pending.clone(),
        });
        Ok(Box::new(FakeView {
            n,
            fake: self.clone(),
            keyboard,
            table,
            pending,
        }))
    }

    fn monitor(&self, on: bool) {
        self.0.monitor.borrow_mut().push(on);
    }
}

struct FakeView {
    n: usize,
    fake: FakeEngine,
    keyboard: Rc<Cell<bool>>,
    table: Rc<RefCell<Option<Table>>>,
    pending: Rc<RefCell<Vec<Done>>>,
}

impl View for FakeView {
    fn attach(&mut self, parent: Parent) {
        self.fake.record(self.n, Did::Attach(parent));
    }

    fn place(&mut self, rect: egui::Rect, scale: f32) {
        self.fake.record(self.n, Did::Place(rect, scale));
    }

    fn set_hidden(&mut self, hidden: bool) {
        self.fake.record(self.n, Did::Hidden(hidden));
    }

    fn paint(&mut self, _painter: &egui::Painter, rect: egui::Rect) {
        self.fake.record(self.n, Did::Paint(rect));
    }

    fn input(&mut self, event: &egui::Event) {
        self.fake.record(self.n, Did::Input(event.clone()));
    }

    fn page_has_keyboard(&self) -> bool {
        self.keyboard.get()
    }

    fn take_keyboard(&mut self) {
        self.fake.record(self.n, Did::TakeKeyboard);
        self.keyboard.set(false);
    }

    fn load(&mut self, url: &str) {
        self.fake.record(self.n, Did::Load(url.to_string()));
    }

    fn reload(&mut self) {
        self.fake.record(self.n, Did::Reload);
    }

    fn set_scripts(&mut self, on: bool) {
        self.fake.record(self.n, Did::Scripts(on));
    }

    fn set_preview(&mut self, on: bool) {
        self.fake.record(self.n, Did::Preview(on));
    }

    fn set_shown(&mut self, shown: bool) {
        self.fake.record(self.n, Did::Shown(shown));
    }

    fn snapshot(&mut self, done: Box<dyn FnOnce(Option<egui::ColorImage>)>) {
        self.fake.record(self.n, Did::Snapshot);
        let picture = self.fake.0.picture.borrow().clone();
        match picture {
            Some(picture) => done(Some(picture)),
            None => self.pending.borrow_mut().push(done),
        }
    }

    fn close(&mut self) {
        self.fake.record(self.n, Did::Close);
        // The view's requests stop with it.
        self.table.borrow_mut().take();
    }
}
