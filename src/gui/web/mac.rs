//! macOS's engine (09 §2.2, WV-A): a WKWebView over the window, a subview of winit's NSView
//! that follows the page's rect, hidden while egui paints above it ([`super::after_pass`]).
//! Everything here runs on the main thread, where AppKit and WebKit live: the engine is made
//! there ([`engine`] says why not elsewhere) and every [`View`] call comes there.
//!
//! - **A view** ([`Page`]): its own `WKWebsiteDataStore.nonPersistentDataStore` — nothing kept,
//!   no cookie, cache or storage shared with another view (09 §2.5; separate `WKProcessPool`s have
//!   had no effect since macOS 12, R12, so none is made); no script message handler, no UI
//!   delegate, not inspectable, no file URL; scripts as each navigation's preferences say.
//! - **A file view** registers `sopen-page` for itself (§3.1) and is served through the scheme
//!   [`Table`] in push mode: WebKit's start and stop go to the table; what the page server sends
//!   comes back on the main queue (the wake), and only there is a task called — the head, the
//!   pieces, the end — in the table's order and never after its stop: a stopped id is gone from
//!   the table (the tombstone), and a stop may come before the head, mid-body or after the last
//!   byte (X-W1: macOS throws on any call after a stop). The task is retained from its start to
//!   its end or stop, never longer.
//! - **Its content rule list** (§3.3): everything blocked but `sopen-page:`, `data:` and `blob:`,
//!   compiled once per process into a temp folder of its own ([`rules_dir`]), removed at exit; one
//!   left by a sopen that was killed is swept at the next start. Nothing loads in a file view
//!   before the list is on its controller (a load waits), and a list that does not compile fails
//!   the view: nothing is ever loaded uncontained.
//! - **Navigation** (§3.4, §7): a file view's main frame stays in its scheme; a link out — or any
//!   other navigation out — is cancelled and said ([`Event::Refused`]); subframes go by the rules.
//!   An address window's main frame stays in its origin. A response the engine cannot show is
//!   cancelled, never downloaded.
//! - **Keys** (§2.4): a local key monitor, on while a view is in a window, hands the keys that are
//!   always sopen's ([`super::route`]) back to winit's view; Esc only takes the keyboard back,
//!   and its repeats and release are swallowed with it. A preview's view never keeps the keyboard,
//!   and its shield — a transparent view of sopen's own over the page — takes every click and
//!   scroll and does nothing with them.
//! - **Receipts**: `takeSnapshot`, drawn into pixels at the window's scale.

#[cfg(test)]
mod tests;

use super::{
    route, Engine, Event, Events, Keyboard, Opened, Opening, Parent, Placement, Route, View,
};
use crate::scheme::{Call, Host, Mode, RequestId, Table};
use block2::RcBlock;
use eframe::egui;
use objc2::rc::{autoreleasepool, Retained};
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{
    available, define_class, msg_send, sel, AllocAnyThread, DefinedClass, MainThreadMarker,
    MainThreadOnly, Message,
};
use objc2_app_kit::{
    NSBitmapImageRep, NSDeviceRGBColorSpace, NSEvent, NSEventMask, NSEventModifierFlags,
    NSEventType, NSGraphicsContext, NSImage, NSView, NSWindow,
};
use objc2_foundation::{
    NSBundle, NSData, NSDictionary, NSError, NSHTTPURLResponse, NSLocalizedDescriptionKey,
    NSMutableDictionary, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString,
    NSURLErrorDomain, NSURLRequest, NSURL,
};
use objc2_web_kit::{
    WKContentRuleList, WKContentRuleListStore, WKNavigation, WKNavigationAction,
    WKNavigationActionPolicy, WKNavigationDelegate, WKNavigationResponse,
    WKNavigationResponsePolicy, WKNavigationType, WKURLSchemeHandler, WKURLSchemeTask,
    WKUserContentController, WKWebView, WKWebViewConfiguration, WKWebpagePreferences,
    WKWebsiteDataStore,
};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::ptr::NonNull;
use std::rc::{Rc, Weak};
use std::sync::OnceLock;

/// The content rule list of every file view (§3.3), the same JSON as WebKitGTK's: every URL
/// blocked, then the page's own scheme and the in-process `data:` and `blob:` let through.
const RULES: &str = r#"[
{"trigger":{"url-filter":".*"},"action":{"type":"block"}},
{"trigger":{"url-filter":"^sopen-page:"},"action":{"type":"ignore-previous-rules"}},
{"trigger":{"url-filter":"^data:"},"action":{"type":"ignore-previous-rules"}},
{"trigger":{"url-filter":"^blob:"},"action":{"type":"ignore-previous-rules"}}
]"#;

/// The rule list's name in its store.
const RULES_ID: &str = "sopen-file-view";

/// The temp folder of a process's rule list is this and its pid ([`rules_dir`]).
const RULES_PREFIX: &str = "sopen-web-rules-";

/// The channel a file view's scripts would not have contained (§3.3, R1): macOS has no setting
/// that turns WebRTC off, so F2 is off in file views here.
const SCRIPTS_UNCONTAINED: &str = "WebRTC";

type Task = ProtocolObject<dyn WKURLSchemeTask>;

extern "C" {
    static _dispatch_main_q: c_void;
    fn dispatch_async_f(
        queue: *const c_void,
        context: *mut c_void,
        work: extern "C" fn(*mut c_void),
    );
}

/// The process's engine, made on the main thread; why not, elsewhere.
pub(super) fn engine() -> Result<Rc<dyn Engine>, String> {
    let mtm = MainThreadMarker::new().ok_or("WebKit runs on the main thread only")?;
    Ok(Rc::new(WebKit::start(mtm)))
}

/// What a request of a file view's table sends a task, once made (tests: the glue's record).
#[cfg(test)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Glue {
    /// The rule list went on the view's controller.
    Ruled,
    /// WebKit started a task.
    Start(RequestId),
    /// WebKit stopped a task.
    Stop(RequestId),
    /// A call made on a task: `head`, `data`, `done`, `fail`.
    Call(RequestId, &'static str),
}

/// Where the rule list stands: compiled once per process, on the first engine.
enum Rules {
    Unstarted,
    /// Compiling: the views waiting for it.
    Compiling(Vec<Weak<Inner>>),
    Ready(Retained<WKContentRuleList>),
    Failed(String),
}

thread_local! {
    static RULE_LIST: RefCell<Rules> = const { RefCell::new(Rules::Unstarted) };
    /// Every view not closed, by its key (the wake carries the key, never a pointer).
    static PAGES: RefCell<HashMap<u64, Weak<Inner>>> = RefCell::new(HashMap::new());
    static NEXT_KEY: Cell<u64> = const { Cell::new(0) };
    /// The local key monitor, while on.
    static MONITOR: RefCell<Option<Retained<AnyObject>>> = const { RefCell::new(None) };
    /// The Esc that took the keyboard back is down: its repeats and release are its own.
    static ESC_HELD: Cell<bool> = const { Cell::new(false) };
}

/// The rule list's folder, once made: removed at exit.
static RULES_DIR: OnceLock<PathBuf> = OnceLock::new();

/// WKWebView: the engine of every view of the process.
pub(super) struct WebKit {
    mtm: MainThreadMarker,
    version: String,
}

impl WebKit {
    /// The engine: a folder left by a killed sopen swept, the rule list compiling.
    pub(super) fn start(mtm: MainThreadMarker) -> Self {
        sweep(&std::env::temp_dir());
        compile(mtm);
        WebKit {
            mtm,
            version: webkit_version(),
        }
    }

    /// A new view (see [`Engine::open`]).
    pub(super) fn open_page(&self, opening: Opening) -> Result<Page, String> {
        self.open_view(opening, true)
    }

    /// A file view without its rule list: the tests' control only.
    #[cfg(test)]
    pub(super) fn open_unruled(&self, opening: Opening) -> Result<Page, String> {
        self.open_view(opening, false)
    }

    fn open_view(&self, opening: Opening, ruled: bool) -> Result<Page, String> {
        let mtm = self.mtm;
        let rules = match &opening.content {
            Opened::Page { .. } if ruled => RULE_LIST.with(|r| match &*r.borrow() {
                Rules::Failed(why) => Err(format!("the content rule list did not compile: {why}")),
                Rules::Ready(list) => Ok(Some(list.clone())),
                Rules::Unstarted | Rules::Compiling(_) => Ok(None),
            })?,
            _ => None,
        };
        let key = NEXT_KEY.with(|n| {
            n.set(n.get() + 1);
            n.get()
        });
        let inner = autoreleasepool(|_| Inner::new(mtm, key, opening, ruled));
        if let Some(list) = rules {
            inner.rules_ready(&list);
        } else if inner.needs_rules {
            RULE_LIST.with(|r| {
                if let Rules::Compiling(waiting) = &mut *r.borrow_mut() {
                    waiting.push(Rc::downgrade(&inner));
                }
            });
        }
        PAGES.with(|p| p.borrow_mut().insert(key, Rc::downgrade(&inner)));
        Ok(Page(inner))
    }
}

impl Engine for WebKit {
    fn placement(&self) -> Placement {
        Placement::Native
    }

    fn version(&self) -> String {
        self.version.clone()
    }

    fn uncontained(&self, scripts: bool) -> Option<String> {
        scripts.then(|| SCRIPTS_UNCONTAINED.to_string())
    }

    fn open(&self, opening: Opening) -> Result<Box<dyn View>, String> {
        self.open_page(opening)
            .map(|page| Box::new(page) as Box<dyn View>)
    }

    fn monitor(&self, on: bool) {
        monitor(on);
    }
}

/// WebKit.framework's bundle version (`CFBundleVersion`, e.g. `21624.5.1.11.3`).
fn webkit_version() -> String {
    let bundle = NSBundle::bundleWithIdentifier(&NSString::from_str("com.apple.WebKit"));
    bundle
        .and_then(|b| b.objectForInfoDictionaryKey(&NSString::from_str("CFBundleVersion")))
        .and_then(|v| v.downcast::<NSString>().ok())
        .map_or_else(|| "unknown".to_string(), |v| v.to_string())
}

/// This process's rule-list folder in `temp`.
fn rules_dir(temp: &Path) -> PathBuf {
    temp.join(format!("{RULES_PREFIX}{}", std::process::id()))
}

/// The rule-list folders in `temp` of processes that are gone are removed; a live one's stays.
fn sweep(temp: &Path) {
    let Ok(entries) = std::fs::read_dir(temp) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(pid) = name
            .to_str()
            .and_then(|n| n.strip_prefix(RULES_PREFIX))
            .and_then(|p| p.parse::<i32>().ok())
        else {
            continue;
        };
        if pid > 0 && !alive(pid) {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

/// Whether process `pid` exists (another user's counts).
fn alive(pid: i32) -> bool {
    // SAFETY: signal 0 checks the process without sending anything.
    let found = unsafe { libc::kill(pid, 0) } == 0;
    found || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

extern "C" fn remove_rules_dir() {
    if let Some(dir) = RULES_DIR.get() {
        let _ = std::fs::remove_dir_all(dir);
    }
}

/// The rule list compiles into this process's folder, once; the views waiting get it when it
/// is ready, on the main thread.
fn compile(mtm: MainThreadMarker) {
    let started = RULE_LIST.with(|r| {
        let mut r = r.borrow_mut();
        if !matches!(*r, Rules::Unstarted) {
            return false;
        }
        *r = Rules::Compiling(vec![]);
        true
    });
    if !started {
        return;
    }
    let dir = rules_dir(&std::env::temp_dir());
    if let Err(e) = std::fs::create_dir_all(&dir) {
        finish_compile(Err(format!("{}: {e}", dir.display())));
        return;
    }
    if RULES_DIR.set(dir.clone()).is_ok() {
        // SAFETY: a plain function, run once at exit.
        unsafe { libc::atexit(remove_rules_dir) };
    }
    let url = NSURL::fileURLWithPath_isDirectory(&NSString::from_str(&dir.to_string_lossy()), true);
    // SAFETY: the URL names a folder of sopen's own.
    let Some(store) = (unsafe { WKContentRuleListStore::storeWithURL(Some(&url), mtm) }) else {
        finish_compile(Err("no rule-list store".to_string()));
        return;
    };
    let done = RcBlock::new(|list: *mut WKContentRuleList, error: *mut NSError| {
        // SAFETY: WebKit hands a list or an error, each valid for the call.
        let list = unsafe { Retained::retain(list) };
        let outcome = list.ok_or_else(|| {
            // SAFETY: as above.
            unsafe { error.as_ref() }.map_or_else(
                || "unknown error".to_string(),
                |e| e.localizedDescription().to_string(),
            )
        });
        finish_compile(outcome);
    });
    // SAFETY: the identifier, the JSON and the block are valid; the block runs on the main thread.
    unsafe {
        store.compileContentRuleListForIdentifier_encodedContentRuleList_completionHandler(
            Some(&NSString::from_str(RULES_ID)),
            Some(&NSString::from_str(RULES)),
            Some(&done),
        );
    }
}

fn finish_compile(outcome: Result<Retained<WKContentRuleList>, String>) {
    let waiting = RULE_LIST.with(|r| {
        let next = match &outcome {
            Ok(list) => Rules::Ready(list.clone()),
            Err(why) => Rules::Failed(why.clone()),
        };
        match std::mem::replace(&mut *r.borrow_mut(), next) {
            Rules::Compiling(waiting) => waiting,
            _ => vec![],
        }
    });
    for inner in waiting.iter().filter_map(Weak::upgrade) {
        match &outcome {
            Ok(list) => inner.rules_ready(list),
            Err(why) => inner.rules_failed(why),
        }
    }
}

/// What a view keeps to: its scheme host (a file view) or its origin (an address window).
enum Scope {
    Page(Host),
    Address(Option<Origin>),
}

/// `scheme://host:port`, lower-cased, the port explicit.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Origin {
    scheme: String,
    host: String,
    port: Option<u16>,
}

/// `url`'s origin; `None` for one without an authority.
fn origin(url: &str) -> Option<Origin> {
    let (scheme, rest) = url.split_once("://")?;
    let scheme = scheme.to_ascii_lowercase();
    let authority = &rest[..rest.find(['/', '?', '#']).unwrap_or(rest.len())];
    let hostport = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let (host, port) = match hostport.strip_prefix('[') {
        Some(v6) => {
            let (host, after) = v6.split_once(']')?;
            (format!("[{host}]"), after.strip_prefix(':'))
        }
        None => match hostport.rsplit_once(':') {
            Some((host, port)) => (host.to_string(), Some(port)),
            None => (hostport.to_string(), None),
        },
    };
    let port = match port.filter(|p| !p.is_empty()) {
        Some(p) => Some(p.parse().ok()?),
        None => match scheme.as_str() {
            "http" => Some(80),
            "https" => Some(443),
            _ => None,
        },
    };
    Some(Origin {
        scheme,
        host: host.to_ascii_lowercase(),
        port,
    })
}

impl Scope {
    /// Whether a navigation of the main frame (`main`) or a subframe to `url` stays.
    fn keeps(&self, url: &str, main: bool) -> bool {
        match self {
            Scope::Page(host) => host.path_of(url).is_some() || (!main && in_process(url)),
            Scope::Address(own) => !main || (own.is_some() && origin(url) == *own),
        }
    }

    /// The document on screen: a file view's path under its root, an address as it is.
    fn document(&self, url: &str) -> String {
        match self {
            Scope::Page(host) => host
                .path_of(url)
                .map_or_else(|| url.to_string(), |p| decoded(p.trim_start_matches('/'))),
            Scope::Address(_) => url.to_string(),
        }
    }
}

/// `data:`, `blob:` and `about:` — what a subframe of a file view may show besides the scheme.
fn in_process(url: &str) -> bool {
    let scheme = url.split_once(':').map_or("", |(s, _)| s);
    ["data", "blob", "about"]
        .iter()
        .any(|s| scheme.eq_ignore_ascii_case(s))
}

/// `path` percent-decoded (bytes that are not UTF-8 shown as U+FFFD).
fn decoded(path: &str) -> String {
    let bytes = path.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = |b: u8| char::from(b).to_digit(16);
        match (
            bytes[i],
            bytes.get(i + 1).copied().and_then(hex),
            bytes.get(i + 2).copied().and_then(hex),
        ) {
            (b'%', Some(hi), Some(lo)) => {
                out.push((hi * 16 + lo) as u8);
                i += 3;
            }
            (b, _, _) => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// One view's state, on the main thread. Its WebKit objects go at [`Inner::close`].
pub(super) struct Inner {
    key: u64,
    scope: Scope,
    events: Events,
    web: RefCell<Option<Retained<WKWebView>>>,
    controller: Retained<WKUserContentController>,
    nav: RefCell<Option<Retained<Navigation>>>,
    _handler: Option<Retained<Scheme>>,
    parent: RefCell<Option<Retained<NSView>>>,
    shield: RefCell<Option<Retained<Shield>>>,
    scripts: Cell<bool>,
    preview: Cell<bool>,
    hidden: Cell<bool>,
    shown: Cell<bool>,
    /// The window's scale as last placed.
    scale: Cell<f64>,
    /// A file view's requests (gone once closed) and the tasks WebKit gave for them.
    table: RefCell<Option<Table>>,
    tasks: RefCell<HashMap<RequestId, Retained<Task>>>,
    /// A file view loads nothing until its rule list is on its controller.
    needs_rules: bool,
    ruled: Cell<bool>,
    /// The load waiting for the rule list.
    pending: RefCell<Option<String>>,
    blocked: Cell<u32>,
    closed: Cell<bool>,
    #[cfg(test)]
    log: RefCell<Vec<Glue>>,
}

/// A wake of view `key`'s table: a drain on the main queue, from any thread.
fn wake(key: u64) {
    extern "C" fn drain(context: *mut c_void) {
        let key = context as u64;
        let inner = PAGES.with(|p| p.borrow().get(&key).and_then(Weak::upgrade));
        if let Some(inner) = inner {
            inner.drain();
        }
    }
    // SAFETY: the main queue lives for the process; the context is the key, not a pointer.
    unsafe {
        dispatch_async_f(
            std::ptr::addr_of!(_dispatch_main_q),
            key as *mut c_void,
            drain,
        );
    }
}

impl Inner {
    fn new(mtm: MainThreadMarker, key: u64, opening: Opening, ruled: bool) -> Rc<Inner> {
        let (scope, table) = match opening.content {
            Opened::Page { host, server } => {
                let table = Table::new(host.clone(), server, Mode::Push, move || wake(key));
                (Scope::Page(host), Some(table))
            }
            Opened::Address { origin: address } => (Scope::Address(origin(&address)), None),
        };
        let needs_rules = table.is_some() && ruled;
        // SAFETY (this function's WebKit calls): main-thread objects made and configured on it.
        let controller = unsafe { WKUserContentController::new(mtm) };
        Rc::new_cyclic(|me: &Weak<Inner>| {
            let config = unsafe { WKWebViewConfiguration::new(mtm) };
            let handler = table.as_ref().map(|_| {
                let handler = Scheme::alloc(mtm).set_ivars(me.clone());
                let handler: Retained<Scheme> = unsafe { msg_send![super(handler), init] };
                handler
            });
            unsafe {
                config.setWebsiteDataStore(&WKWebsiteDataStore::nonPersistentDataStore(mtm));
                config.setUserContentController(&controller);
                config
                    .defaultWebpagePreferences()
                    .setAllowsContentJavaScript(opening.scripts);
                if let Some(handler) = &handler {
                    config.setURLSchemeHandler_forURLScheme(
                        Some(ProtocolObject::from_ref(&**handler)),
                        &NSString::from_str(crate::scheme::SCHEME),
                    );
                }
            }
            let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(0.0, 0.0));
            let web = unsafe {
                WKWebView::initWithFrame_configuration(WKWebView::alloc(mtm), frame, &config)
            };
            let nav = Navigation::alloc(mtm).set_ivars(me.clone());
            let nav: Retained<Navigation> = unsafe { msg_send![super(nav), init] };
            unsafe { web.setNavigationDelegate(Some(ProtocolObject::from_ref(&*nav))) };
            if available!(macos = 13.3) {
                unsafe { web.setInspectable(false) };
            }
            web.setHidden(true);
            let inner = Inner {
                key,
                scope,
                events: opening.events,
                web: RefCell::new(Some(web)),
                controller: controller.clone(),
                nav: RefCell::new(Some(nav)),
                _handler: handler,
                parent: RefCell::default(),
                shield: RefCell::default(),
                scripts: Cell::new(opening.scripts),
                preview: Cell::new(false),
                hidden: Cell::new(true),
                shown: Cell::new(true),
                scale: Cell::new(1.0),
                table: RefCell::new(table),
                tasks: RefCell::default(),
                needs_rules,
                ruled: Cell::new(false),
                pending: RefCell::default(),
                blocked: Cell::new(0),
                closed: Cell::new(false),
                #[cfg(test)]
                log: RefCell::default(),
            };
            if opening.preview {
                inner.set_preview(true);
            }
            inner
        })
    }

    #[cfg(test)]
    fn note(&self, glue: Glue) {
        self.log.borrow_mut().push(glue);
    }

    fn web(&self) -> Option<Retained<WKWebView>> {
        self.web.borrow().clone()
    }

    fn parent(&self) -> Option<Retained<NSView>> {
        self.parent.borrow().clone()
    }

    fn shield(&self) -> Option<Retained<Shield>> {
        self.shield.borrow().clone()
    }

    fn rules_ready(&self, list: &WKContentRuleList) {
        if self.closed.get() || self.ruled.get() {
            return;
        }
        // SAFETY: the view's own controller, on the main thread.
        unsafe { self.controller.addContentRuleList(list) };
        self.ruled.set(true);
        #[cfg(test)]
        self.note(Glue::Ruled);
        let pending = self.pending.borrow_mut().take();
        if let Some(url) = pending {
            self.load_now(&url);
        }
    }

    fn rules_failed(&self, why: &str) {
        self.pending.borrow_mut().take();
        let line = format!("the content rule list did not compile: {why}");
        self.events.send(Event::Failed(line));
    }

    /// A load asked of the view: refused outside its scope before WebKit sees it — WebKit
    /// connects to a loaded address's host before the navigation policy is asked (measured: a
    /// bare TCP connect for a load the policy cancelled).
    fn load(&self, url: &str) {
        if !self.scope.keeps(url, true) {
            self.events.send(Event::Refused {
                target: url.to_string(),
                clicked: false,
            });
            return;
        }
        if self.needs_rules && !self.ruled.get() {
            *self.pending.borrow_mut() = Some(url.to_string());
        } else {
            self.load_now(url);
        }
    }

    fn load_now(&self, url: &str) {
        let Some(web) = self.web() else {
            return;
        };
        match NSURL::URLWithString(&NSString::from_str(url)) {
            // SAFETY: a request for a URL; loading never calls back synchronously into a borrow.
            Some(address) => autoreleasepool(|_| unsafe {
                web.loadRequest(&NSURLRequest::requestWithURL(&address));
            }),
            None => self
                .events
                .send(Event::Failed(format!("not an address: {url}"))),
        }
    }

    /// Whether the first responder of the view's window is the page (or inside it).
    fn has_keyboard(&self) -> bool {
        let Some(web) = self.web() else {
            return false;
        };
        web.window()
            .is_some_and(|window| responder_in(&window, &web))
    }

    /// winit's view becomes first responder again, if the page had the keyboard.
    fn take_keyboard(&self) {
        let (Some(web), Some(parent)) = (self.web(), self.parent()) else {
            return;
        };
        if let Some(window) = web.window() {
            if responder_in(&window, &web) {
                window.makeFirstResponder(Some(&parent));
            }
        }
    }

    /// Hidden or shown as the frame and the host say.
    fn apply_hidden(&self) {
        let hidden = self.hidden.get() || !self.shown.get();
        if let Some(web) = self.web() {
            web.setHidden(hidden);
        }
        if let Some(shield) = self.shield() {
            shield.setHidden(hidden);
        }
    }

    fn set_preview(&self, on: bool) {
        self.preview.set(on);
        let mtm = MainThreadMarker::from(&*self.controller);
        if on {
            self.take_keyboard();
            if self.shield.borrow().is_some() {
                return;
            }
            let frame = self.web().map_or(NSRect::ZERO, |w| w.frame());
            let shield: Retained<Shield> =
                unsafe { msg_send![Shield::alloc(mtm), initWithFrame: frame] };
            shield.setHidden(self.hidden.get() || !self.shown.get());
            if let Some(parent) = self.parent() {
                parent.addSubview(&shield);
            }
            *self.shield.borrow_mut() = Some(shield);
        } else if let Some(shield) = self.shield.borrow_mut().take() {
            shield.removeFromSuperview();
        }
    }

    /// The calls the server's messages make, in order, on the main thread: through the table's
    /// order and tombstone, no borrow held while WebKit runs (a call can stop or start a task).
    fn drain(&self) {
        loop {
            let next = match self.table.borrow_mut().as_mut() {
                Some(table) => table.next_call(),
                None => return,
            };
            let Some((id, call)) = next else {
                return;
            };
            let task = self.tasks.borrow().get(&id).cloned();
            let Some(task) = task else {
                continue;
            };
            let ended = matches!(call, Call::Done | Call::Fail(_));
            #[cfg(test)]
            self.note(Glue::Call(
                id,
                match &call {
                    Call::Head(_) => "head",
                    Call::Data(_) => "data",
                    Call::Done => "done",
                    Call::Fail(_) => "fail",
                    Call::Readable => "readable",
                },
            ));
            autoreleasepool(|_| make(&task, call));
            if ended {
                self.tasks.borrow_mut().remove(&id);
            }
        }
    }

    /// WebKit starts a task.
    fn start(&self, task: &Task) {
        if self.closed.get() {
            return;
        }
        // SAFETY: the task's request, valid while WebKit holds the task.
        let request = unsafe { task.request() };
        let url = request
            .URL()
            .and_then(|u| u.absoluteString())
            .map(|u| u.to_string())
            .unwrap_or_default();
        let range = request
            .valueForHTTPHeaderField(&NSString::from_str("Range"))
            .map(|r| r.to_string());
        let id = match self.table.borrow_mut().as_mut() {
            Some(table) => table.start(&url, range),
            None => return,
        };
        // No drain can come before this: the answers arrive on a later turn of the main queue.
        self.tasks.borrow_mut().insert(id, task.retain());
        #[cfg(test)]
        self.note(Glue::Start(id));
    }

    /// WebKit stops a task: nothing more is called on it.
    fn stop(&self, task: &Task) {
        let id = self
            .tasks
            .borrow()
            .iter()
            .find(|(_, t)| std::ptr::eq(Retained::as_ptr(t), task))
            .map(|(id, _)| *id);
        let Some(id) = id else {
            return;
        };
        #[cfg(test)]
        self.note(Glue::Stop(id));
        self.tasks.borrow_mut().remove(&id);
        if let Some(table) = self.table.borrow_mut().as_mut() {
            table.stop(id);
        }
    }

    /// Gone: its requests stopped, out of its window, every WebKit object of it released — the
    /// cells emptied first, then WebKit told (which may call back here).
    fn close(&self) {
        if self.closed.replace(true) {
            return;
        }
        PAGES.with(|p| p.borrow_mut().remove(&self.key));
        let table = self.table.borrow_mut().take();
        let tasks = std::mem::take(&mut *self.tasks.borrow_mut());
        let web = self.web.borrow_mut().take();
        let nav = self.nav.borrow_mut().take();
        let shield = self.shield.borrow_mut().take();
        let parent = self.parent.borrow_mut().take();
        self.pending.borrow_mut().take();
        // The server is told every request stopped; the tasks are let go without a call.
        drop(table);
        autoreleasepool(|_| {
            if let Some(web) = &web {
                if let (Some(window), Some(parent)) = (web.window(), &parent) {
                    if responder_in(&window, web) {
                        window.makeFirstResponder(Some(parent));
                    }
                }
                // SAFETY: the view's own delegate slot and load, on the main thread.
                unsafe {
                    web.setNavigationDelegate(None);
                    web.stopLoading();
                }
                web.removeFromSuperview();
            }
            if let Some(shield) = &shield {
                shield.removeFromSuperview();
            }
            drop(tasks);
            drop(web);
            drop(nav);
            drop(shield);
        });
    }
}

/// One call on a task.
fn make(task: &Task, call: Call) {
    // SAFETY (each call): on the main thread, on a task WebKit has not stopped — the table
    // returns calls only for live requests, and a stop removes the request before returning.
    match call {
        Call::Head(head) => {
            let url = unsafe { task.request() }.URL();
            let fields: Retained<NSMutableDictionary<NSString, NSString>> =
                NSMutableDictionary::new();
            for (name, value) in head.headers() {
                fields.insert(&*NSString::from_str(name), &*NSString::from_str(&value));
            }
            let response = url.and_then(|url| {
                NSHTTPURLResponse::initWithURL_statusCode_HTTPVersion_headerFields(
                    NSHTTPURLResponse::alloc(),
                    &url,
                    head.code() as isize,
                    Some(&NSString::from_str("HTTP/1.1")),
                    Some(&fields),
                )
            });
            match response {
                Some(response) => unsafe { task.didReceiveResponse(&response) },
                None => unsafe { task.didFailWithError(&error("no response for the head")) },
            }
        }
        Call::Data(bytes) => unsafe { task.didReceiveData(&NSData::with_bytes(&bytes)) },
        Call::Done => unsafe { task.didFinish() },
        Call::Fail(why) => unsafe { task.didFailWithError(&error(&why)) },
        // Pull engines only (WebKitGTK).
        Call::Readable => {}
    }
}

/// A network error saying `why`.
fn error(why: &str) -> Retained<NSError> {
    let why = NSString::from_str(why);
    let info = NSDictionary::from_slices(
        // SAFETY: a constant of Foundation's.
        &[unsafe { NSLocalizedDescriptionKey }],
        &[&*why as &AnyObject],
    );
    // SAFETY: the domain is Foundation's constant; the dictionary holds a string.
    unsafe { NSError::errorWithDomain_code_userInfo(NSURLErrorDomain, -1, Some(&info)) }
}

/// Whether `window`'s first responder is `view` or inside it.
fn responder_in(window: &NSWindow, view: &NSView) -> bool {
    window
        .firstResponder()
        .and_then(|r| r.downcast::<NSView>().ok())
        .is_some_and(|r| r.isDescendantOf(view))
}

/// The view of the page: the [`View`] the component holds.
pub(super) struct Page(Rc<Inner>);

impl View for Page {
    fn attach(&mut self, parent: Parent) {
        let Parent::AppKit(view) = parent else {
            return;
        };
        // SAFETY: the window's NSView, alive while the window is (the shell's handle).
        let Some(view) = (unsafe { Retained::retain(view.as_ptr().cast::<NSView>()) }) else {
            return;
        };
        if let Some(web) = self.0.web() {
            view.addSubview(&web);
        }
        if let Some(shield) = self.0.shield() {
            view.addSubview(&shield);
        }
        *self.0.parent.borrow_mut() = Some(view);
    }

    fn place(&mut self, rect: egui::Rect, scale: f32) {
        // winit's view is flipped: the rect in points is the frame.
        let frame = NSRect::new(
            NSPoint::new(f64::from(rect.min.x), f64::from(rect.min.y)),
            NSSize::new(f64::from(rect.width()), f64::from(rect.height())),
        );
        self.0.scale.set(f64::from(scale));
        if let Some(web) = self.0.web() {
            web.setFrame(frame);
        }
        if let Some(shield) = self.0.shield() {
            shield.setFrame(frame);
        }
    }

    fn set_hidden(&mut self, hidden: bool) {
        self.0.hidden.set(hidden);
        self.0.apply_hidden();
    }

    fn page_has_keyboard(&self) -> bool {
        self.0.has_keyboard()
    }

    fn take_keyboard(&mut self) {
        self.0.take_keyboard();
    }

    fn load(&mut self, url: &str) {
        self.0.load(url);
    }

    fn reload(&mut self) {
        if self.0.pending.borrow().is_some() {
            return;
        }
        if let Some(web) = self.0.web() {
            // SAFETY: on the main thread.
            autoreleasepool(|_| unsafe {
                web.reload();
            });
        }
    }

    fn set_scripts(&mut self, on: bool) {
        self.0.scripts.set(on);
    }

    fn set_preview(&mut self, on: bool) {
        self.0.set_preview(on);
    }

    fn set_shown(&mut self, shown: bool) {
        if self.0.shown.replace(shown) == shown {
            return;
        }
        self.0.apply_hidden();
        if !shown {
            self.0.take_keyboard();
        }
        if let Some(web) = self.0.web() {
            if available!(macos = 12.0) {
                let done = RcBlock::new(|| {});
                // SAFETY: on the main thread; suspending and resuming come in pairs.
                unsafe { web.setAllMediaPlaybackSuspended_completionHandler(!shown, Some(&done)) };
            }
        }
    }

    fn snapshot(&mut self, done: Box<dyn FnOnce(Option<egui::ColorImage>)>) {
        let Some(web) = self.0.web() else {
            done(None);
            return;
        };
        let scale = web
            .window()
            .map_or(self.0.scale.get(), |w| w.backingScaleFactor());
        let done = RefCell::new(Some(done));
        let block = RcBlock::new(move |image: *mut NSImage, _error: *mut NSError| {
            // SAFETY: WebKit hands an image or nil, valid for the call.
            let picture = unsafe { image.as_ref() }.and_then(|image| pixels(image, scale));
            if let Some(done) = done.borrow_mut().take() {
                done(picture);
            }
        });
        // SAFETY: the default configuration (the view's bounds); the block runs on the main thread.
        unsafe { web.takeSnapshotWithConfiguration_completionHandler(None, &block) };
    }

    fn close(&mut self) {
        self.0.close();
    }
}

impl Drop for Page {
    fn drop(&mut self) {
        self.0.close();
    }
}

#[cfg(test)]
impl Page {
    pub(super) fn inner(&self) -> &Inner {
        &self.0
    }
}

/// `image` drawn into RGBA pixels at `scale` pixels a point.
fn pixels(image: &NSImage, scale: f64) -> Option<egui::ColorImage> {
    let size = image.size();
    let (w, h) = (
        (size.width * scale).round() as isize,
        (size.height * scale).round() as isize,
    );
    if w <= 0 || h <= 0 {
        return None;
    }
    // SAFETY: a bitmap of sopen's own, 8-bit RGBA, drawn into on the main thread and read back
    // within its size.
    unsafe {
        let rep = NSBitmapImageRep::initWithBitmapDataPlanes_pixelsWide_pixelsHigh_bitsPerSample_samplesPerPixel_hasAlpha_isPlanar_colorSpaceName_bytesPerRow_bitsPerPixel(
            NSBitmapImageRep::alloc(),
            std::ptr::null_mut(),
            w,
            h,
            8,
            4,
            true,
            false,
            NSDeviceRGBColorSpace,
            w * 4,
            32,
        )?;
        let context = NSGraphicsContext::graphicsContextWithBitmapImageRep(&rep)?;
        NSGraphicsContext::saveGraphicsState_class();
        NSGraphicsContext::setCurrentContext(Some(&context));
        image.drawInRect(NSRect::new(
            NSPoint::new(0.0, 0.0),
            NSSize::new(w as f64, h as f64),
        ));
        context.flushGraphics();
        NSGraphicsContext::restoreGraphicsState_class();
        let data = rep.bitmapData();
        if data.is_null() {
            return None;
        }
        let (w, h) = (w as usize, h as usize);
        let rgba = std::slice::from_raw_parts(data, w * h * 4);
        Some(egui::ColorImage::from_rgba_premultiplied([w, h], rgba))
    }
}

/// The key monitor on (once) or off.
fn monitor(on: bool) {
    if !on {
        if let Some(monitor) = MONITOR.with(|m| m.borrow_mut().take()) {
            // SAFETY: the monitor this function installed.
            unsafe { NSEvent::removeMonitor(&monitor) };
        }
        ESC_HELD.with(|held| held.set(false));
        return;
    }
    if MONITOR.with(|m| m.borrow().is_some()) {
        return;
    }
    let handler = RcBlock::new(|event: NonNull<NSEvent>| -> *mut NSEvent {
        // SAFETY: AppKit hands an event valid for the call.
        if on_key(unsafe { event.as_ref() }) {
            event.as_ptr()
        } else {
            std::ptr::null_mut()
        }
    });
    // SAFETY: a local monitor of key events, its block returning the event or nil.
    let installed = unsafe {
        NSEvent::addLocalMonitorForEventsMatchingMask_handler(
            NSEventMask::KeyDown | NSEventMask::KeyUp,
            &handler,
        )
    };
    MONITOR.with(|m| *m.borrow_mut() = installed);
}

/// The key monitor's word on a key event: `true` passes it on, `false` consumes it (§2.4).
fn on_key(event: &NSEvent) -> bool {
    let down = event.r#type() == NSEventType::KeyDown;
    let code = event.keyCode();
    if code == KEY_ESCAPE && ESC_HELD.with(Cell::get) {
        if !down || event.isARepeat() {
            ESC_HELD.with(|held| held.set(down));
            return false;
        }
        // A new press: the release was missed.
        ESC_HELD.with(|held| held.set(false));
    }
    if !down {
        return true;
    }
    let Some(mtm) = MainThreadMarker::new() else {
        return true;
    };
    let Some(window) = event.window(mtm) else {
        return true;
    };
    let Some(page) = page_with_keyboard(&window) else {
        return true;
    };
    if page.preview.get() {
        // A preview's view never keeps the keyboard: the key is sopen's.
        page.take_keyboard();
        return true;
    }
    let chars = event
        .charactersIgnoringModifiers()
        .map(|c| c.to_string())
        .unwrap_or_default();
    let Some(key) = key_of(code, &chars) else {
        return true;
    };
    match route(Keyboard::Page, key, modifiers_of(event.modifierFlags())) {
        Route::Page | Route::Sopen => true,
        Route::TakeBack => {
            page.take_keyboard();
            ESC_HELD.with(|held| held.set(true));
            false
        }
        Route::TakeBackAndAct => {
            page.take_keyboard();
            true
        }
    }
}

/// The view whose page has `window`'s keyboard.
fn page_with_keyboard(window: &NSWindow) -> Option<Rc<Inner>> {
    let pages: Vec<Rc<Inner>> =
        PAGES.with(|p| p.borrow().values().filter_map(Weak::upgrade).collect());
    pages.into_iter().find(|page| {
        page.web().is_some_and(|web| {
            web.window().is_some_and(|w| std::ptr::eq(&*w, window)) && responder_in(window, &web)
        })
    })
}

/// `kVK_Escape`.
const KEY_ESCAPE: u16 = 53;

/// The keys [`route`] tells apart, from the key code (F-keys, Esc, the digit row) or the
/// characters (W and Q, wherever the layout puts them); any other key is the page's anyway.
fn key_of(code: u16, chars: &str) -> Option<egui::Key> {
    use egui::Key::*;
    let by_code = match code {
        KEY_ESCAPE => Some(Escape),
        122 => Some(F1),
        120 => Some(F2),
        99 => Some(F3),
        118 => Some(F4),
        96 => Some(F5),
        97 => Some(F6),
        98 => Some(F7),
        100 => Some(F8),
        101 => Some(F9),
        109 => Some(F10),
        103 => Some(F11),
        111 => Some(F12),
        18 => Some(Num1),
        19 => Some(Num2),
        20 => Some(Num3),
        21 => Some(Num4),
        23 => Some(Num5),
        22 => Some(Num6),
        26 => Some(Num7),
        28 => Some(Num8),
        25 => Some(Num9),
        29 => Some(Num0),
        _ => None,
    };
    by_code.or(match chars.to_ascii_lowercase().as_str() {
        "w" => Some(W),
        "q" => Some(Q),
        _ => None,
    })
}

/// The modifiers as egui-winit reports them on a Mac: ⌘ is `mac_cmd` and `command`.
fn modifiers_of(flags: NSEventModifierFlags) -> egui::Modifiers {
    let cmd = flags.contains(NSEventModifierFlags::Command);
    egui::Modifiers {
        alt: flags.contains(NSEventModifierFlags::Option),
        ctrl: flags.contains(NSEventModifierFlags::Control),
        shift: flags.contains(NSEventModifierFlags::Shift),
        mac_cmd: cmd,
        command: cmd,
    }
}

define_class!(
    /// A file view's `sopen-page` handler: each task to the view's table.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = Weak<Inner>]
    struct Scheme;

    unsafe impl NSObjectProtocol for Scheme {}

    unsafe impl WKURLSchemeHandler for Scheme {
        #[unsafe(method(webView:startURLSchemeTask:))]
        fn start_task(&self, _web: &WKWebView, task: &Task) {
            if let Some(inner) = self.ivars().upgrade() {
                inner.start(task);
            }
        }

        #[unsafe(method(webView:stopURLSchemeTask:))]
        fn stop_task(&self, _web: &WKWebView, task: &Task) {
            if let Some(inner) = self.ivars().upgrade() {
                inner.stop(task);
            }
        }
    }
);

define_class!(
    /// A view's navigation delegate: the policy of §3.4 and §7, and its events.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = Weak<Inner>]
    struct Navigation;

    unsafe impl NSObjectProtocol for Navigation {}

    unsafe impl WKNavigationDelegate for Navigation {
        #[unsafe(method(webView:decidePolicyForNavigationAction:preferences:decisionHandler:))]
        fn decide_action(
            &self,
            _web: &WKWebView,
            action: &WKNavigationAction,
            preferences: &WKWebpagePreferences,
            handler: &block2::DynBlock<dyn Fn(WKNavigationActionPolicy, NonNull<WKWebpagePreferences>)>,
        ) {
            let preferences_ptr = NonNull::from(preferences);
            let Some(inner) = self.ivars().upgrade() else {
                handler.call((WKNavigationActionPolicy::Cancel, preferences_ptr));
                return;
            };
            // SAFETY: the action's fields, on the main thread.
            let (url, frame, clicked) = unsafe {
                let url = action
                    .request()
                    .URL()
                    .and_then(|u| u.absoluteString())
                    .map(|u| u.to_string())
                    .unwrap_or_default();
                let clicked = action.navigationType() == WKNavigationType::LinkActivated;
                (url, action.targetFrame(), clicked)
            };
            // No target frame: a new window, which this view never opens.
            let main = frame.as_ref().is_none_or(|f| unsafe { f.isMainFrame() });
            let kept = inner.scope.keeps(&url, main);
            // A subframe of a file view goes by its rule list; only a click out is the row's.
            let refused = !kept && (main || clicked);
            if !refused && frame.is_some() {
                // SAFETY: the preferences WebKit gave, for this navigation.
                unsafe { preferences.setAllowsContentJavaScript(inner.scripts.get()) };
                handler.call((WKNavigationActionPolicy::Allow, preferences_ptr));
                return;
            }
            handler.call((WKNavigationActionPolicy::Cancel, preferences_ptr));
            if refused {
                inner.events.send(Event::Refused {
                    target: url,
                    clicked,
                });
            } else if kept {
                // A link to a new window inside the scope opens in the view itself.
                inner.load_now(&url);
            }
        }

        #[unsafe(method(webView:decidePolicyForNavigationResponse:decisionHandler:))]
        fn decide_response(
            &self,
            _web: &WKWebView,
            response: &WKNavigationResponse,
            handler: &block2::DynBlock<dyn Fn(WKNavigationResponsePolicy)>,
        ) {
            // SAFETY: the response's fields, on the main thread.
            let (shows, main, mime, status) = unsafe {
                let main = response.isForMainFrame();
                let shows = response.canShowMIMEType();
                let response = response.response();
                let mime = response.MIMEType().map(|m| m.to_string());
                let status = response
                    .downcast_ref::<NSHTTPURLResponse>()
                    .map(|r| r.statusCode());
                (shows, main, mime.filter(|m| !m.is_empty()), status)
            };
            let error = status.filter(|s| *s >= 400);
            // An error with nothing to show (a 403 or 404 head) is no download: shown empty.
            let allow = shows || error.is_some();
            handler.call((if allow {
                WKNavigationResponsePolicy::Allow
            } else {
                WKNavigationResponsePolicy::Cancel
            },));
            let line = match (shows, error) {
                (true, _) => return,
                (false, Some(code)) => format!("the page answered {code}"),
                (false, None) => match mime {
                    Some(mime) => format!("cannot show {mime}"),
                    None => "cannot show a response without a type".to_string(),
                },
            };
            if let Some(inner) = self.ivars().upgrade().filter(|_| main) {
                inner.events.send(Event::Failed(line));
            }
        }

        #[unsafe(method(webView:didStartProvisionalNavigation:))]
        fn started(&self, _web: &WKWebView, _navigation: Option<&WKNavigation>) {
            self.say(Event::Loading);
        }

        #[unsafe(method(webView:didCommitNavigation:))]
        fn committed(&self, web: &WKWebView, _navigation: Option<&WKNavigation>) {
            if let Some(inner) = self.ivars().upgrade() {
                // SAFETY: the view's URL, on the main thread.
                let url = unsafe { web.URL() }
                    .and_then(|u| u.absoluteString())
                    .map(|u| u.to_string())
                    .unwrap_or_default();
                inner.events.send(Event::Document(inner.scope.document(&url)));
            }
        }

        #[unsafe(method(webView:didFinishNavigation:))]
        fn finished(&self, _web: &WKWebView, _navigation: Option<&WKNavigation>) {
            self.say(Event::Finished);
        }

        #[unsafe(method(webView:didFailProvisionalNavigation:withError:))]
        fn failed_early(&self, _web: &WKWebView, _navigation: Option<&WKNavigation>, error: &NSError) {
            self.failed(error);
        }

        #[unsafe(method(webView:didFailNavigation:withError:))]
        fn failed_late(&self, _web: &WKWebView, _navigation: Option<&WKNavigation>, error: &NSError) {
            self.failed(error);
        }

        #[unsafe(method(webViewWebContentProcessDidTerminate:))]
        fn process_ended(&self, _web: &WKWebView) {
            self.say(Event::ProcessEnded);
        }
    }

    impl Navigation {
        /// WebKit's word (its delegate's private method, called when it responds) that a
        /// content rule acted on `url`: a blocked load counts on the row (§3.3). Private, so
        /// read defensively: any argument may be nil, and an action that does not answer
        /// `blockedLoad` counts nothing. Should WebKit stop calling it, no `Blocked` event is
        /// ever said — the blocking itself is the rule list's and does not depend on it.
        #[unsafe(method(_webView:contentRuleListWithIdentifier:performedAction:forURL:))]
        fn rule_acted(
            &self,
            _web: Option<&WKWebView>,
            _list: Option<&NSString>,
            action: Option<&AnyObject>,
            _url: Option<&NSURL>,
        ) {
            // SAFETY: a selector asked of an object only when it answers to it.
            let blocked = action.is_some_and(|action| unsafe {
                let answers: bool = msg_send![action, respondsToSelector: sel!(blockedLoad)];
                answers && msg_send![action, blockedLoad]
            });
            if let Some(inner) = self.ivars().upgrade().filter(|_| blocked) {
                inner.blocked.set(inner.blocked.get() + 1);
                inner.events.send(Event::Blocked(inner.blocked.get()));
            }
        }
    }
);

impl Navigation {
    fn say(&self, event: Event) {
        if let Some(inner) = self.ivars().upgrade() {
            inner.events.send(event);
        }
    }

    /// A load failed: said, unless it was cancelled — a new load, or the policy's own cancel.
    fn failed(&self, error: &NSError) {
        let (domain, code) = (error.domain().to_string(), error.code());
        let cancelled = (domain == "NSURLErrorDomain" && code == -999)
            || (domain == "WebKitErrorDomain" && code == 102);
        if !cancelled {
            let why = error.localizedDescription();
            self.say(Event::Failed(format!("{why} ({domain} {code})")));
        }
    }
}

define_class!(
    /// A preview's shield (§2.2): a transparent view over the page that is hit instead of it
    /// and does nothing with a click or a scroll.
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    struct Shield;

    impl Shield {
        #[unsafe(method(acceptsFirstResponder))]
        fn accepts_first_responder(&self) -> bool {
            false
        }

        #[unsafe(method(acceptsFirstMouse:))]
        fn accepts_first_mouse(&self, _event: Option<&NSEvent>) -> bool {
            false
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, _event: &NSEvent) {}

        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, _event: &NSEvent) {}

        #[unsafe(method(mouseDragged:))]
        fn mouse_dragged(&self, _event: &NSEvent) {}

        #[unsafe(method(rightMouseDown:))]
        fn right_mouse_down(&self, _event: &NSEvent) {}

        #[unsafe(method(rightMouseUp:))]
        fn right_mouse_up(&self, _event: &NSEvent) {}

        #[unsafe(method(rightMouseDragged:))]
        fn right_mouse_dragged(&self, _event: &NSEvent) {}

        #[unsafe(method(otherMouseDown:))]
        fn other_mouse_down(&self, _event: &NSEvent) {}

        #[unsafe(method(otherMouseUp:))]
        fn other_mouse_up(&self, _event: &NSEvent) {}

        #[unsafe(method(otherMouseDragged:))]
        fn other_mouse_dragged(&self, _event: &NSEvent) {}

        #[unsafe(method(scrollWheel:))]
        fn scroll_wheel(&self, _event: &NSEvent) {}

        #[unsafe(method(magnifyWithEvent:))]
        fn magnify(&self, _event: &NSEvent) {}
    }
);
