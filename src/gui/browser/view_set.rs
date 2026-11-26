//! The viewer's views in today's windows (`.prd/08-viewer.md` §8, VM-7): the egui binding of the
//! view set ([`crate::views`]) and the view keys ([`crate::view_keys`]).
//!
//! The viewer opens on its file's view 1 (or Hex, D10) as before — in a terminal a page file on
//! its source, 3, nothing of its page made (10 T21, VM-8); 1, 2 and 3 — taken before the
//! viewer's pass by the shell's input hook (`gui::route_view_keys`), or F9 then one of them, or a
//! click on the title row's strip — choose a view by §3's table. A view is made the first time it
//! is chosen, on a worker, by the same preparation the opening ran ([`prepare::prepare_view`]);
//! one switched away from is kept hidden ([`Window::set_shown`] `false`) and comes back as it was;
//! one holding unsaved edits asks its own close prompt first, and a running Save makes the switch
//! wait. The hex view opens viewing (D35). F8 (or E) in Text and Raw opens the list of encodings,
//! a dialog of cells ([`super::ops::dialog`]).
//!
//! A Save that landed is told by the window's own count of them ([`Window::saves_landed`]),
//! checked before anything else in a pass: whatever the window did after it (closed, kept its
//! notes on screen), the hidden views are stale (D25).
//!
//! A remote file's views share what its opening kept (08 §6, VM-7b): its stream, beside the
//! views, which every view made later reads through (D13: nothing read again), and one save
//! session for their `Origin`s, given back when the view that used it is hidden or closed
//! (D14) — once the operation on it is over, asked again on later passes until then — so a
//! viewer holds 06 §6's "1; 2 from its first Save". Where the stream stands (its epoch) tells a
//! reload — a view's Cmd/Ctrl+R, a Save's own — looked at before every input the set gets, so
//! no hidden view is shown from before it (D25's reload half). A Save that did not reload the
//! stream (an image's, a sound's) leaves it behind the file, so the next view made reloads it
//! first. A video none of whose frames decode gives way to Hex with the reason, as an image
//! does (D10).
//!
//! A viewer handed over by the preview (02 §4.4) hands its stream (none for an image, read
//! whole) and its save session to the set as well ([`OpenedAs::with`]).

use super::prepare::{self, Opened, Over, Prepared, SaveSlot, Shared};
use super::{Browser, Closing, Connect, Viewer};
use crate::encoding::Encoding;
use crate::gui::web::Unavailable;
use crate::gui::{chrome, help, Embedded, Outcome, Placeholder, Status, ViewKeySlot, Window};
use crate::kind::Kind;
use crate::remote::Stat;
use crate::stream::{ReadError, Reader, ReaderKind, State};
use crate::view_keys::{self, Command, Ending, ViewKeys};
use crate::views::{
    Effect, Hold, Input, Now, Opening, PageClass, Reach, Screen, Slot, Ticket, View, ViewSet, Why,
};
use eframe::egui::{self, Key, Modifiers};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Arc, Mutex, PoisonError};
use std::task::Poll;
use std::thread::JoinHandle;

/// What the opening keeps to make a view later (08 §6): where the bytes are and how to reach
/// them, and how the viewer titles them.
pub(super) struct Means {
    /// The bytes on screen: for a revision or a diff the `.revisions` copy (D13), never the
    /// working file F4 opens.
    pub path: String,
    pub name: String,
    pub title: String,
    pub connect: Connect,
    pub label: Option<String>,
    /// A revision or a diff: read where it is, every view read-only.
    pub in_place: bool,
}

/// One opened file's views in the viewer.
pub(super) struct Views {
    pub(super) set: ViewSet,
    /// The view keys, shared with the shell's input hook ([`crate::gui::set_view_keys`]).
    slot: Arc<Mutex<ViewKeySlot>>,
    /// The view whose window is on screen ([`Viewer::embedded`]); `None`: a blank stands there
    /// while the next one is made.
    shown: Option<View>,
    /// The views kept hidden (D1).
    hidden: Vec<Kept>,
    making: Option<Making>,
    means: Means,
    picker: Option<Picker>,
    /// The view on screen's close prompt is up for a switch (`Effect::AskClose`).
    asked: bool,
    /// The view on screen's count of Saves that wrote the file, as last told to the set
    /// ([`Window::saves_landed`]): one that moved is a landed Save (D25), whatever the window
    /// did after it.
    saves_seen: u64,
    /// An encoding was chosen from F8's list: the set is told the one the text view reads in,
    /// once it does (D21).
    chose: bool,
    /// Tests: views made on a worker so far.
    #[cfg(test)]
    pub(super) makes: usize,
    /// Tests: run in the pass right after the reload receipt, before the pass's view keys (a
    /// reload that lands between the two).
    #[cfg(test)]
    pub(super) between: Option<Box<dyn FnMut()>>,
    /// `SOPEN_STATE` `view=N`: chosen once the first view is on screen and ready.
    wanted: Option<Slot>,
    /// The folder the browser was opened on, for a page view made later (09 §3.1's root).
    opened_on: Option<String>,
    /// The page view could not be made (09 §4): the first row of the text view made next.
    pub(super) source_line: Option<String>,
    /// A remote file's stream, kept beside the views (D13): what every view made later reads
    /// through. `None` for a local file, and until a remote one has one (a page file's views
    /// before Hex or Text, a handed-over image's before its second view).
    stream: Option<KeptStream>,
    /// A remote file's save session, which every view's `Origin` connects through and a hidden
    /// view gives back (D14). `None` for a local file.
    save: Option<SaveSlot>,
    /// A give-back asked for (the view that saved hidden or closed), as the session's requests
    /// stood then; refused for now while an operation holds the session, it is asked again on
    /// a later pass ([`SaveSlot::give_back`]).
    giving: Option<prepare::Asked>,
    /// The view a switch chose that waits for what the view on screen runs (D2).
    waited: Option<Slot>,
}

/// The stream the views of a remote file share, and where it stood (its epoch, 06 §5) when
/// the set was last told of it and when the view on screen's Save began.
struct KeptStream {
    stream: prepare::Stream,
    /// The version the set was last told of: a reload past it — the view on screen's
    /// Cmd/Ctrl+R, its Save's own — makes the hidden views stale (D25).
    told: Epoch,
    /// The version when the view on screen's Save began (moved along while none runs): a
    /// reload past it when the Save lands is the Save's own (the hex view's), which left the
    /// stream on the file saved.
    at_save: Epoch,
    /// The file changed since the stream last read it: a Save landed that did not reload the
    /// stream (the image's, the media window's, which read no more from it). The next view
    /// made reloads it first (D25: made again, it shows the file as it is now). A file another
    /// writer changed is not this: the stream says `Changed`, the views say it, and Cmd/Ctrl+R
    /// is the owner's.
    behind: bool,
}

/// A reader of the stream's that reads nothing: it tells whether the stream was reloaded since
/// it last moved (a read of nothing reads nothing, 06 §5).
struct Epoch(Reader);

impl Epoch {
    fn new(source: &crate::stream::Source) -> Self {
        Epoch(source.reader(ReaderKind::Interactive))
    }

    /// The stream was reloaded since: this moves to the version open now.
    fn moved(&mut self) -> bool {
        match self.0.try_read(0, 0) {
            Poll::Ready(Err(ReadError::Reloaded)) => {
                self.0.renew();
                true
            }
            _ => false,
        }
    }
}

impl KeptStream {
    fn new(stream: prepare::Stream) -> Self {
        KeptStream {
            told: Epoch::new(&stream.source),
            at_save: Epoch::new(&stream.source),
            stream,
            behind: false,
        }
    }

    /// A reload the set was not told of yet.
    fn reloaded(&mut self) -> bool {
        let moved = self.told.moved();
        self.caught_up(moved);
        moved
    }

    /// A reload was seen (`moved`): the stream is on the file again — if it reads it. A reload
    /// that failed (the file gone meanwhile) leaves it behind, so the next make reloads.
    fn caught_up(&mut self, moved: bool) {
        let live = matches!(self.stream.source.state(), State::Live | State::Replaced);
        self.behind &= !(moved && live);
    }
}

/// A hidden view: its window, the cached copy it holds, its give-way.
struct Kept {
    view: View,
    embedded: Embedded,
    hold: Option<crate::cache::Claim>,
    switch: Option<crate::gui::image::HexSwitch>,
}

/// A view being made on a worker (`Effect::Make`).
struct Making {
    ticket: Ticket,
    view: View,
    at: Option<u64>,
    notice: Option<String>,
    encoding: Option<Encoding>,
    /// It reloads the kept stream first ([`KeptStream::behind`]).
    reload: bool,
    cancel: Arc<AtomicBool>,
    rx: Receiver<anyhow::Result<Opened>>,
    worker: Option<JoinHandle<()>>,
}

/// F8's list (08 §7.5): every encoding of §7.1, the current one marked, the default §7.2 picked
/// named; ↑ ↓ and Enter choose, Esc leaves the encoding as it was.
struct Picker {
    current: Encoding,
    pick: Option<crate::encoding::Pick>,
    /// The row with the bar (and egui's focus, so Enter or Space chooses it): the current one
    /// as the list opens.
    at: usize,
}

/// How a viewer's views treat a page file (09 §5–§6).
pub(super) struct PageOpen {
    /// Where the page goes among its views: the preparation's one decision, from the file
    /// opened (its resolved name and head), never the name the list shows.
    pub class: PageClass,
    /// The opening made the page (`Pages::Page`), so the set opens on it. Else the source: in a
    /// terminal, for a page file by the name the list shows, with the page still one of its
    /// views (10 T21: `Screen::TERMINAL`); otherwise without it (D30's source, 09 §4).
    pub shown: bool,
    /// The folder the browser was opened on (§3.1's root); `None` where nothing of the page was
    /// made — the page view finds it then as the browser does ([`Browser::start_making`]).
    pub opened_on: Option<String>,
}

/// The views of the file the viewer opened on and the means to make the others; the view keys
/// handed to the shell's input hook from the next pass on. `wanted`: `view=N`. A page file's
/// views as a window without the page view has them (09 §4).
#[cfg_attr(not(test), allow(dead_code))] // The browser asks [`open_paged`].
pub(super) fn open(
    ctx: &egui::Context,
    opened: &impl Opens,
    means: Means,
    wanted: Option<u8>,
) -> Views {
    let page = PageOpen {
        class: PageClass::Plain,
        shown: false,
        opened_on: None,
    };
    open_paged(ctx, opened, means, wanted, page)
}

/// [`open`], a page file's views as `page` says: 1 Page · 2 Hex · 3 Text for HTML, HTM and
/// styled XML, 1 Text · 2 Hex · 3 Page for other XML, where the window shows pages (09 §6).
/// What a remote opening kept for its views ([`Opens::shared`]) is kept beside them (08 §6).
pub(super) fn open_paged(
    ctx: &egui::Context,
    opened: &impl Opens,
    means: Means,
    wanted: Option<u8>,
    page: PageOpen,
) -> Views {
    let shared = opened.shared();
    let opened = opened.opened_as();
    let reach = if means.label.is_some() {
        Reach::Stream
    } else {
        Reach::Local
    };
    // A remote viewer's `Origin`s of the views made from now on save through one session
    // (D14): the opening's, or one of the set's own (a viewer handed over brings none).
    let save = means.label.as_ref().map(|_| {
        let save = shared.save.clone();
        save.unwrap_or_else(|| SaveSlot::new(Placeholder::YES))
    });
    let stream = shared.stream.filter(|_| means.label.is_some());
    // A viewer shows a file: a folder is never opened (§3), so none is here.
    let kind = match opened.kind {
        Kind::Directory => Kind::Other,
        kind => kind,
    };
    let mut opening = Opening::new(kind, means.path.clone(), opened.stat, reach);
    opening.page = page.class;
    opening.read_only = means.in_place;
    opening.undecodable = opened.undecodable.clone();
    let terminal = crate::gui::in_terminal(ctx);
    // The screen the opening made its first view for — the backend's own, since a preview is
    // handed over only in the backend it was made for (`preview::Host::fits`): the page where a
    // window made it; in a terminal, which draws none, the source with the window's slots (10
    // T21: `Pages::here` made nothing of the page) where a window would show the page — a page
    // file by the name the list shows (09 §5), not a link to one under another name; else the
    // source alone (09 §4).
    let screen = match (page.class, page.shown, terminal) {
        (PageClass::Plain, ..) => Screen::NO_ENGINE,
        (_, true, _) => Screen::GUI,
        (_, false, true) if crate::kind::page_file(&means.name) => Screen::TERMINAL,
        (_, false, _) => Screen::NO_ENGINE,
    };
    let set = ViewSet::open(opening, screen).expect("a file has views");
    // A terminal's input sends its releases made (`term_input::RELEASES`), a window's its own.
    let releases = !terminal || crate::gui::term_input::RELEASES;
    let down = ctx.input(|i| i.keys_down.iter().copied().collect::<Vec<_>>());
    let slot = Arc::new(Mutex::new(ViewKeySlot {
        state: view_keys::State::Free,
        keys: ViewKeys::new(releases, down),
        commands: Vec::new(),
    }));
    crate::gui::set_view_keys(ctx, Some(slot.clone()));
    let shown = set.on_screen();
    Views {
        set,
        slot,
        shown,
        hidden: Vec::new(),
        making: None,
        means,
        picker: None,
        asked: false,
        saves_seen: 0,
        chose: false,
        #[cfg(test)]
        makes: 0,
        #[cfg(test)]
        between: None,
        wanted: wanted.and_then(Slot::from_number),
        opened_on: page.opened_on,
        source_line: None,
        stream: stream.map(KeptStream::new),
        save,
        giving: None,
        waited: None,
    }
}

/// What the view set needs of an opening besides its means: the kind, the stat, why view 1
/// cannot show the file.
pub(super) struct OpenedAs {
    pub kind: Kind,
    pub stat: Stat,
    pub undecodable: Option<String>,
}

/// What [`open_paged`] reads of an opening: what the set needs, and what the views of a remote
/// file share from it (08 §6) — nothing, for an [`OpenedAs`] alone (a viewer handed over by
/// the preview, 02 §4.4).
pub(super) trait Opens {
    fn opened_as(&self) -> &OpenedAs;
    fn shared(&self) -> Shared {
        Shared::default()
    }
}

impl Opens for OpenedAs {
    fn opened_as(&self) -> &OpenedAs {
        self
    }
}

/// A preparation's opening: what the set needs, and what its views share ([`Opened::shared`]).
pub(super) struct OpenedShared {
    opened: OpenedAs,
    shared: Shared,
}

impl Opens for OpenedShared {
    fn opened_as(&self) -> &OpenedAs {
        &self.opened
    }

    fn shared(&self) -> Shared {
        self.shared.clone()
    }
}

impl OpenedAs {
    /// With what its views share: a preview's, handed over ([`prepare::HandedOver::shared`]).
    pub fn with(self, shared: Shared) -> OpenedShared {
        OpenedShared {
            opened: self,
            shared,
        }
    }

    /// From a preparation's [`Opened`], with what its views share.
    pub fn of(opened: &Opened) -> OpenedShared {
        OpenedShared {
            opened: OpenedAs {
                kind: opened.kind,
                stat: opened.stat,
                undecodable: opened.undecodable.clone(),
            },
            shared: opened.shared.clone(),
        }
    }
}

/// How a text window made from a preparation opens: the scripted run's `at=`, `loading=1` and
/// `enc=` (the first viewer only), and the rows its first lines say.
#[derive(Default)]
pub(super) struct TextOpen {
    pub at: Option<u64>,
    pub loading: bool,
    pub enc: Option<Encoding>,
}

/// A window made from a preparation, with the cached copy it holds and its give-way.
pub(super) struct Made {
    pub embedded: Embedded,
    pub hold: Option<crate::cache::Claim>,
    pub switch: Option<crate::gui::image::HexSwitch>,
}

/// The window that shows `prepared`, titled `title` (`name` picks a text file's syntax);
/// `notice`: the first row of a hex window (D10's reason).
pub(super) fn embed(
    prepared: Prepared,
    cache: &Path,
    title: String,
    name: &str,
    text: TextOpen,
    notice: Option<String>,
) -> anyhow::Result<Made> {
    use crate::gui::{hex, image, media, text as textview};
    let mut switch = None;
    let (embedded, hold) = match prepared {
        Prepared::Image {
            file,
            doc,
            notices,
            claim,
        } => (image::embed(file, doc, notices), claim),
        Prepared::ImageAsking {
            file,
            path,
            notices,
            claim,
        } => {
            // 06 §7.3: the memory line, with Hex view on the file read in place.
            let gives_way = image::HexSwitch::local(path);
            let embedded = image::embed_asking(file, notices, Some(gives_way.clone()))?;
            switch = Some(gives_way);
            (embedded, claim)
        }
        Prepared::ImageStream {
            origin,
            source,
            repaint,
            notices,
        } => {
            // 06 §7.3: the window reads on a worker of its own, and gives way to the hex window
            // on the same stream (Hex view, a file that does not decode).
            let gives_way = image::HexSwitch::new(repaint);
            let made =
                image::embed_stream(origin, source, cache, notices, Some(gives_way.clone()))?;
            switch = Some(gives_way);
            (made, None)
        }
        Prepared::Media {
            origin,
            media,
            notices,
            claim,
        } => (media::embed(origin, *media, cache, notices), claim),
        // Probed on the worker: nothing slow is left for this frame (06 §7.4).
        Prepared::MediaStream {
            origin,
            media,
            notices,
        } => (media::embed_streamed(origin, *media, cache, notices), None),
        Prepared::Hex { file, notices } => {
            let notices = notice.into_iter().chain(notices).collect();
            (hex::embed(file, notices), None)
        }
        Prepared::Text {
            doc,
            notices,
            claim,
        } => {
            // A file read in place (no fetched copy, no stream) is followed at its end.
            let live = claim.is_none() && !doc.is_stream();
            let TextOpen { at, loading, enc } = text;
            // The page view that could not be made says why first (09 §4).
            let notices = notice.into_iter().chain(notices).collect();
            (
                textview::embed_scripted(doc, title, name, notices, live, at, loading, enc),
                claim,
            )
        }
        // 09 §6: its view is made on the window's next pass.
        Prepared::Page { page } => (crate::gui::web::page::embed(page), None),
    };
    Ok(Made {
        embedded,
        hold,
        switch,
    })
}

/// A window with nothing in it: what stands on screen while the next view is made after the one
/// that was there closed (its prompt answered, or it failed).
struct Blank;

impl Window for Blank {
    fn ui(&mut self, _ui: &mut egui::Ui) {}
    fn ready(&self) -> bool {
        true
    }
}

fn blank() -> Embedded {
    Embedded {
        window: Box::new(Blank),
        settle: Box::new(|_| Some(Outcome::default())),
    }
}

/// What the set says (`Effect::Say`), on the status row: a refused session in the error colour,
/// as the opening says it (06 §6).
fn said(words: String) -> Status {
    if crate::stream::session_refused(&words) {
        Status::error(words)
    } else {
        Status::info(words)
    }
}

/// Text and Raw are one document (D8): one window.
fn same_doc(a: View, b: View) -> bool {
    let doc = |v: View| match v {
        View::Raw => View::Text,
        other => other,
    };
    doc(a) == doc(b)
}

/// The kind a view is prepared as ([`prepare::prepare_view`]; the page view is
/// [`prepare::prepare_page_view`]'s).
fn kind_of(view: View) -> Kind {
    match view {
        View::Image => Kind::Image,
        View::Video => Kind::Video,
        View::Audio => Kind::Audio,
        View::Hex => Kind::Other,
        View::Text | View::Raw | View::Page => Kind::Text,
    }
}

/// D10's words for a reason the hex window's first row already says as bytes.
fn reason(notice: &str) -> String {
    notice
        .strip_suffix("; the file is shown as bytes")
        .unwrap_or(notice)
        .to_string()
}

/// `key` pressed with no modifier in this pass's input, taken with the text it typed.
fn take_plain(ctx: &egui::Context, key: Key) -> bool {
    ctx.input_mut(|i| {
        let at = i.events.iter().position(|e| {
            matches!(e, egui::Event::Key { key: k, pressed: true, modifiers, .. }
                if *k == key && modifiers.is_none())
        });
        let Some(at) = at else {
            return false;
        };
        i.events.remove(at);
        if matches!(i.events.get(at), Some(egui::Event::Text(_))) {
            i.events.remove(at);
        }
        true
    })
}

fn picker_row_id(i: usize) -> egui::Id {
    egui::Id::unique("sopen-encoding").with(i)
}

impl Views {
    /// The slot the file's source is in (09 §4: where the page view gives way to, 1 or 3).
    pub(super) fn source_slot(&self) -> Option<Slot> {
        Slot::ALL
            .into_iter()
            .find(|&s| self.set.slot(s) == Some(View::Text))
    }

    /// The strip on the title row (08 §8).
    pub(super) fn strip(&self) -> chrome::Strip {
        let slots = self.set.slots().map(|v| v.map(View::name));
        let armed = self
            .slot
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .keys
            .armed();
        chrome::Strip {
            slots,
            on: self.set.on_screen().map(View::name),
            armed,
        }
    }

    /// Tests: a give-back is asked for and not done yet.
    #[cfg(test)]
    pub(super) fn giving(&self) -> bool {
        self.giving.is_some()
    }

    /// Tests: the stream the views share is behind the file ([`KeptStream::behind`]).
    #[cfg(test)]
    pub(super) fn behind(&self) -> bool {
        self.stream.as_ref().is_some_and(|k| k.behind)
    }

    /// Tests: the stream the views share.
    #[cfg(test)]
    pub(super) fn kept_source(&self) -> Option<crate::stream::Source> {
        self.stream.as_ref().map(|k| k.stream.source.clone())
    }

    /// A diff or a file at a revision (03 §6.3): every view read-only.
    pub(super) fn read_only(&self) -> bool {
        self.means.in_place
    }

    /// Nothing is on its way: no view being made, `view=` chosen.
    pub(super) fn settled(&self) -> bool {
        self.making.is_none() && self.wanted.is_none()
    }

    /// The status row's line while the views have one: a view being made, the list of
    /// encodings.
    pub(super) fn status(&self) -> Option<Status> {
        if let Some(making) = &self.making {
            return Some(Status::info(format!(
                "making the {} view… · Esc stops",
                making.view.name()
            )));
        }
        self.picker.as_ref().map(|p| {
            Status::info(format!(
                "↑ ↓ an encoding · Enter reads the file in it · Esc keeps {}",
                p.current.label()
            ))
        })
    }

    /// Everything the views hold goes with the viewer: the hidden windows (what they may still
    /// write settles after them), a view being made, the view keys.
    pub(super) fn close(self, closing: &mut Vec<Closing>, workers: &mut Vec<JoinHandle<()>>) {
        for kept in self.hidden {
            closing.push(Closing {
                settle: kept.embedded.settle,
                hold: kept.hold,
            });
        }
        if let Some(mut making) = self.making {
            making.cancel.store(true, Ordering::Relaxed);
            workers.extend(making.worker.take());
        }
    }
}

impl Browser {
    /// Early in the window's pass, before the title row (its strip says what this pass did):
    /// the strip's click and the view keys' commands, the view being made, the prompt or the
    /// Save a switch waits for, `view=`.
    pub(super) fn views_before(&mut self, ctx: &egui::Context) {
        // First a Save that landed since, and a reload: no switch below may show a view they
        // made stale.
        self.save_receipt(ctx);
        self.reload_receipt(ctx);
        if let Some(viewer) = self.viewer.as_mut() {
            // While no Save runs, where the stream stands is where the next one begins.
            let saving =
                viewer.views.shown.is_some() && viewer.embedded.window.hold() == Hold::Saving;
            if let Some(kept) = viewer.views.stream.as_mut().filter(|_| !saving) {
                kept.at_save.moved();
            }
            give_back(&mut viewer.views, ctx);
        }
        #[cfg(test)]
        if let Some(between) = self.viewer.as_mut().and_then(|v| v.views.between.as_mut()) {
            between();
        }
        if let Some(n) = chrome::take_strip_click(ctx) {
            if let Some(viewer) = self.viewer.as_mut() {
                viewer
                    .views
                    .slot
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .keys
                    .disarm();
            }
            if let Some(slot) = Slot::from_number(n) {
                self.view_input(ctx, Input::Choose(slot));
            }
        }
        let commands = match self.viewer.as_ref() {
            Some(viewer) => std::mem::take(
                &mut viewer
                    .views
                    .slot
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .commands,
            ),
            None => return,
        };
        for command in commands {
            self.view_command(ctx, command);
        }
        self.poll_making(ctx);
        self.poll_saves(ctx);
        // `view=N`, once the view the viewer opened on is on screen and ready.
        let wanted = self.viewer.as_mut().and_then(|v| {
            let ready = v.views.making.is_none() && v.embedded.window.ready();
            ready.then(|| v.views.wanted.take()).flatten()
        });
        if let Some(slot) = wanted {
            self.view_input(ctx, Input::Choose(slot));
        }
    }

    /// After the view's pass: what the view declares to the view keys for the next input.
    pub(super) fn views_after(&mut self, ctx: &egui::Context) {
        // The question before a switch to another machine (10 §7.4) is a question too: no view
        // digit and no hex view's Esc acts under it.
        let switch_asking = self.switch_asking();
        let Some(viewer) = self.viewer.as_mut() else {
            return;
        };
        let views = &mut viewer.views;
        let asking = views.picker.is_some() || help::is_open(ctx) || views.set.asking();
        let state = if asking || switch_asking {
            view_keys::State::Question
        } else if ctx.text_edit_focused() {
            view_keys::State::Field
        } else if views.shown.is_none() {
            view_keys::State::Free
        } else {
            viewer.embedded.window.view_state()
        };
        views
            .slot
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .state = state;
        // The encoding the text view reads in now, after a choice from F8's list: the opening's
        // one encoding from then on (D21), for Text, Raw and a text view made again.
        let text = matches!(views.shown, Some(View::Text | View::Raw));
        let reads = viewer
            .embedded
            .window
            .encoding()
            .map(|(enc, _)| enc.label());
        if let Some(label) = reads.filter(|_| text && views.chose) {
            if views.set.encoding() != Some(label) {
                self.view_input(ctx, Input::Encoding(label.to_string()));
            }
        }
    }

    /// The view on screen asked to close in its pass: when its prompt was up for a switch, its
    /// answer is the switch's (`true`: taken here); else it is the viewer's leaving (`false`).
    pub(super) fn views_asked(&mut self, ctx: &egui::Context) -> bool {
        let Some(viewer) = self.viewer.as_mut() else {
            return false;
        };
        // D10, found once on screen: a video none of whose frames decode asked to close with
        // why — it gives way to Hex, the reason its first row, and 1 says it from then on. Only
        // over a live stream, as for the image (06 §7.3): a stream that changed or failed is
        // what failed, and an image's failure is a stream's (one that does not decode gives
        // way by its switch) — said as before, back in the list.
        let video = viewer.views.shown == Some(View::Video);
        let live = viewer
            .views
            .stream
            .as_ref()
            .is_none_or(|k| matches!(k.stream.source.state(), State::Live | State::Replaced));
        if let Some(why) = viewer.embedded.window.failure().filter(|_| video && live) {
            viewer.views.asked = false;
            self.view_input(ctx, Input::Failed(Why::Undecodable(why)));
            return true;
        }
        if !viewer.views.asked {
            return false;
        }
        if viewer.embedded.window.close_requested() {
            viewer.views.asked = false;
            // The prompt's Save that landed comes first (`views::Input::Closed`).
            self.save_receipt(ctx);
            self.view_input(ctx, Input::Closed);
        }
        true
    }

    /// The view on screen's Saves that wrote the file since the set was last told: each is
    /// `SaveEnded { landed: true }` (D25: the hidden views are stale; a switch that waited goes
    /// on).
    fn save_receipt(&mut self, ctx: &egui::Context) {
        let Some(viewer) = self.viewer.as_mut() else {
            return;
        };
        if viewer.views.shown.is_none() {
            return;
        }
        let landed = viewer.embedded.window.saves_landed();
        if std::mem::replace(&mut viewer.views.saves_seen, landed) != landed {
            // A Save that reloaded the stream it reads (the hex view's, 06 §5) left it on the
            // file saved; any other left it behind the file.
            if let Some(kept) = viewer.views.stream.as_mut() {
                if !kept.at_save.moved() {
                    kept.behind = true;
                }
            }
            // The set goes on with a switch that waited for it.
            viewer.views.waited = None;
            self.view_input(ctx, Input::SaveEnded { landed: true });
        }
    }

    /// The stream the views share was reloaded since the set last looked — by the view on
    /// screen's Cmd/Ctrl+R (06 §5), or by its Save's own reload — so the hidden views read a
    /// version before it: stale (D25's reload half), made again when next shown. Looked at
    /// before every input the set gets, so no view is shown from before a reload that landed
    /// since the pass began. Not while a view being made reloads the stream itself
    /// ([`KeptStream::behind`]): that one is taken with it.
    fn reload_receipt(&mut self, ctx: &egui::Context) {
        let Some(viewer) = self.viewer.as_mut() else {
            return;
        };
        let views = &mut viewer.views;
        if views.making.as_ref().is_some_and(|m| m.reload) {
            return;
        }
        if views.stream.as_mut().is_some_and(KeptStream::reloaded) {
            self.view_input(ctx, Input::Reloaded);
        }
    }

    /// The image on screen gave way to the hex window (06 §7.3): the memory line's Hex view is
    /// view 2 chosen, a file that does not decode (`failed`, its reason the first notice) is
    /// D10's — either way the hex window it left is the Hex view, made.
    pub(super) fn views_given_way(
        &mut self,
        ctx: &egui::Context,
        file: crate::gui::hex::HexFile,
        notices: Vec<String>,
        failed: bool,
    ) {
        let input = match notices.first().filter(|_| failed) {
            Some(why) => Input::Failed(Why::Undecodable(reason(why))),
            None => Input::Choose(Slot::Two),
        };
        let made = Made {
            embedded: crate::gui::hex::embed(file, notices),
            hold: None,
            switch: None,
        };
        let chosen = matches!(input, Input::Choose(_));
        let Some(viewer) = self.viewer.as_mut() else {
            return;
        };
        let now = views_now(viewer);
        let effects = viewer.views.set.apply(input, now);
        self.view_effects(ctx, effects, Some(made));
        if chosen {
            // The image window that gave its file to the hex window is spent: hidden, it is
            // closed as a stale view is, and made again when 1 is chosen (D25's rule).
            self.view_input(ctx, Input::Reloaded);
        }
    }

    /// The page view on screen could not be made (09 §4): the file's source is chosen in its
    /// place — view 3 of a page, view 1 of plain XML — its first row saying why.
    pub(super) fn views_page_failed(&mut self, ctx: &egui::Context, why: &Unavailable) {
        let Some(viewer) = self.viewer.as_mut() else {
            return;
        };
        let Some(slot) = viewer.views.source_slot() else {
            return;
        };
        viewer.views.source_line = Some(crate::gui::web::page::source_line(why));
        self.view_input(ctx, Input::Choose(slot));
    }

    /// One command of the view keys (08 §4).
    fn view_command(&mut self, ctx: &egui::Context, command: Command) {
        let Some(viewer) = self.viewer.as_mut() else {
            return;
        };
        match command {
            Command::Choose(n) => {
                if let Some(slot) = Slot::from_number(n) {
                    self.view_input(ctx, Input::Choose(slot));
                }
            }
            Command::Leave => {
                // Esc while a view is made stops making it; the view on screen stays (§5).
                let esc = ctx.input(|i| i.key_pressed(Key::Escape));
                if viewer.views.making.is_some() && esc {
                    ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape));
                    self.view_input(ctx, Input::CancelMake);
                }
            }
            Command::Arm(Ending::Field) => {
                // The field closes, as Esc would (D29): it loses the typing, and the hex
                // window's and the text view's Go to close when they lose it.
                ctx.memory_mut(|m| {
                    if let Some(id) = m.focused() {
                        m.surrender_focus(id);
                    }
                });
            }
            Command::Arm(Ending::Editing)
            | Command::StartEditing
            | Command::EndEditing
            | Command::ReadOnly => viewer.embedded.window.view_key(command),
            Command::Arm(Ending::Nothing) | Command::Disarm => {}
            Command::Pass | Command::Drop | Command::DropText => {}
        }
    }

    /// `input` to the view set, given what the view on screen holds now; its effects carried
    /// out.
    fn view_input(&mut self, ctx: &egui::Context, input: Input) {
        // A reload that landed since the pass's receipt (D25): told first — but for a make's
        // answer, whose window is kept already: the next input tells it, the view then on
        // screen reading the version open now (06 §5).
        if !matches!(
            input,
            Input::Reloaded | Input::Made(_) | Input::MakeFailed(..)
        ) {
            self.reload_receipt(ctx);
        }
        let Some(viewer) = self.viewer.as_mut() else {
            return;
        };
        let now = views_now(viewer);
        let chose = match input {
            Input::Choose(slot) => Some(slot),
            _ => None,
        };
        let effects = viewer.views.set.apply(input, now);
        // A switch that waits for what the view on screen runs (D2): asked again if that ends
        // without a Save that landed ([`Browser::poll_saves`]).
        if effects.contains(&Effect::WaitForSave) {
            viewer.views.waited = chose;
        }
        self.view_effects(ctx, effects, None);
    }

    /// Carries out `effects` in order (`ready`: a window made already, for the Hex view the
    /// set asks to make — the image's give-way). An effect that answers at once (a prompt not
    /// needed, a window made already) is the set's next input.
    fn view_effects(&mut self, ctx: &egui::Context, effects: Vec<Effect>, ready: Option<Made>) {
        let mut ready = ready;
        let mut next = None;
        // What this batch said: a Leave after it keeps it (below).
        let mut words: Option<Status> = None;
        for effect in effects {
            let Some(viewer) = self.viewer.as_mut() else {
                return;
            };
            match effect {
                Effect::Hide(view) => {
                    let old = std::mem::replace(&mut viewer.embedded, blank());
                    let mut kept = Kept {
                        view,
                        embedded: old,
                        hold: viewer.hold.take(),
                        switch: viewer.switch.take(),
                    };
                    kept.embedded.window.set_shown(false);
                    viewer.views.hidden.push(kept);
                    viewer.views.shown = None;
                }
                Effect::Show { view, at, viewing } => {
                    let views = &mut viewer.views;
                    if !views.shown.is_some_and(|s| same_doc(s, view)) {
                        let Some(i) = views.hidden.iter().position(|k| same_doc(k.view, view))
                        else {
                            continue;
                        };
                        let kept = views.hidden.remove(i);
                        // A blank, or a window that closed: nothing of it is left to settle.
                        let gone = std::mem::replace(&mut viewer.embedded, kept.embedded);
                        drop(gone);
                        viewer.hold = kept.hold;
                        viewer.switch = kept.switch;
                        viewer.embedded.window.set_shown(true);
                    }
                    let window = &mut viewer.embedded.window;
                    if matches!(view, View::Text | View::Raw) {
                        window.set_raw(view == View::Raw);
                    }
                    if viewing {
                        window.set_in_viewer(true);
                    }
                    if let Some(at) = at {
                        window.go_to_byte(at);
                    }
                    viewer.views.shown = Some(view);
                    viewer.views.saves_seen = viewer.embedded.window.saves_landed();
                    ctx.memory_mut(|m| m.surrender_focus(super::list_id()));
                    // What the switch said (`saving…`) is over; a reason for it follows.
                    self.status = Status::default();
                }
                // D14: the view just hidden gives its save session back; its `Origin` connects
                // again on its next use.
                // Only for a window that holds nothing (D2: the switch waited for its Save, Save
                // copy, Save As or Export): its operation, publish and cleanup included, ended.
                Effect::ReleaseSaveConnection(view) => {
                    let views = &mut viewer.views;
                    let kept = views
                        .hidden
                        .iter_mut()
                        .rev()
                        .find(|k| same_doc(k.view, view));
                    let clean = kept.is_none_or(|k| k.embedded.window.hold() == Hold::Clean);
                    debug_assert!(clean, "a give-back for {view:?}, which holds something");
                    if clean {
                        ask_give_back(views, ctx);
                    }
                }
                // A view made with no stream kept opens one, and the set keeps it from then on
                // (`poll_making`): nothing to do ahead of the make.
                Effect::OpenStream => {}
                Effect::Make {
                    ticket,
                    view,
                    at,
                    notice,
                    encoding,
                    viewing: _,
                } => {
                    if let Some(made) = ready.take().filter(|_| view == View::Hex) {
                        // The image's give-way left the hex window: made already.
                        let read_only = viewer.views.read_only();
                        viewer.views.hidden.push(kept(view, made, read_only));
                        next = Some(Input::Made(ticket));
                        continue;
                    }
                    let encoding = encoding.as_deref().and_then(Encoding::from_label);
                    let making = self.start_making(ctx, ticket, view, at, notice, encoding);
                    if let Some(viewer) = self.viewer.as_mut() {
                        viewer.views.making = Some(making);
                    }
                }
                Effect::Abandon(ticket) => {
                    let views = &mut viewer.views;
                    if views.making.as_ref().is_some_and(|m| m.ticket == ticket) {
                        let mut making = views.making.take().expect("being made");
                        making.cancel.store(true, Ordering::Relaxed);
                        let worker = making.worker.take();
                        self.leftover.borrow_mut().workers.extend(worker);
                    }
                }
                Effect::Close(view) => {
                    let closed = if viewer.views.shown.is_some_and(|s| same_doc(s, view)) {
                        // The view on screen closed (its prompt answered, D2; it failed): one
                        // that saves gives the session back, as a hidden one does (D14).
                        if view.saves() && !viewer.views.read_only() {
                            ask_give_back(&mut viewer.views, ctx);
                        }
                        viewer.views.shown = None;
                        let old = std::mem::replace(&mut viewer.embedded, blank());
                        viewer.switch = None;
                        Some((old, viewer.hold.take()))
                    } else {
                        let views = &mut viewer.views;
                        let at = views.hidden.iter().position(|k| same_doc(k.view, view));
                        at.map(|i| {
                            let kept = views.hidden.remove(i);
                            (kept.embedded, kept.hold)
                        })
                    };
                    if let Some((embedded, hold)) = closed {
                        self.leftover.borrow_mut().closing.push(Closing {
                            settle: embedded.settle,
                            hold,
                        });
                    }
                }
                Effect::AskClose => {
                    viewer.views.asked = true;
                    if viewer.embedded.window.close_requested() {
                        viewer.views.asked = false;
                        next = Some(Input::Closed);
                    }
                }
                Effect::WaitForSave => self.status = Status::info("saving…"),
                Effect::Say(said_now) => {
                    self.status = said(said_now);
                    words = Some(self.status.clone());
                }
                Effect::Leave => {
                    // What this batch said is said on the list (a make refused after the view on
                    // screen closed, 06 §6): leaving would put the list's own line over it.
                    self.drop_viewer();
                    self.after_viewer(ctx);
                    if let Some(words) = words {
                        self.status = words;
                    }
                    return;
                }
            }
        }
        ctx.request_repaint();
        if let Some(input) = next {
            let Some(viewer) = self.viewer.as_mut() else {
                return;
            };
            let now = views_now(viewer);
            let effects = viewer.views.set.apply(input, now);
            self.view_effects(ctx, effects, ready);
        }
    }

    /// The view `view` made on a worker from what the opening kept, as the opening prepared it.
    fn start_making(
        &mut self,
        ctx: &egui::Context,
        ticket: Ticket,
        view: View,
        at: Option<u64>,
        notice: Option<String>,
        encoding: Option<Encoding>,
    ) -> Making {
        #[cfg(test)]
        if let Some(viewer) = self.viewer.as_mut() {
            viewer.views.makes += 1;
        }
        let views = &mut self.viewer.as_mut().expect("a viewer").views;
        // The text view made after a page view that could not be (09 §4) says why first.
        let notice = match view {
            View::Text | View::Raw => notice.or_else(|| views.source_line.take()),
            _ => notice,
        };
        // A page file opened with nothing of its page made (a terminal, 10 T21) was handed no
        // folder: its root is found from the one the browser was opened on, when that is on the
        // file's host, as the browser finds it for an opening.
        let opened_on = views
            .opened_on
            .clone()
            .or_else(|| (views.means.label == self.label).then(|| self.opened_on.clone()));
        let means = &views.means;
        let (connect, label, path, in_place) = (
            means.connect.clone(),
            means.label.clone(),
            means.path.clone(),
            means.in_place,
        );
        // What the views share (08 §6): the view reads through the kept stream, reloaded first
        // when it is behind the file, and saves through the views' session.
        let reload = views.stream.as_ref().is_some_and(|k| k.behind);
        let shared = Shared {
            stream: views.stream.as_ref().map(|k| k.stream.clone()),
            save: views.save.clone(),
        };
        let cache = self.cache.clone();
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel();
        let (stop, waker) = (cancel.clone(), ctx.clone());
        let began = std::time::Instant::now();
        let worker = std::thread::spawn(move || {
            let repaint = waker.clone();
            let wake: crate::stream::Waker = Arc::new(move || repaint.request_repaint());
            let req = prepare::Request {
                connect: &connect,
                label: label.as_deref(),
                abs: &path,
                cache: &cache,
                wake: &wake,
                began,
            };
            let result = match view {
                // 09 §6: the page, from its root, on its own connection.
                View::Page => {
                    prepare::prepare_page_view(&req, in_place, opened_on.as_deref(), &stop)
                }
                _ => {
                    let over = Over {
                        shared: &shared,
                        reload,
                    };
                    let kind = kind_of(view);
                    prepare::prepare_view(&req, in_place, kind, over, &stop, &mut |_, _| {
                        waker.request_repaint()
                    })
                }
            };
            let _ = tx.send(result);
            waker.request_repaint();
        });
        Making {
            ticket,
            view,
            at,
            notice,
            encoding,
            reload: reload && view != View::Page,
            cancel,
            rx,
            worker: Some(worker),
        }
    }

    /// The view being made, once its worker answers: made (kept hidden until the set shows it)
    /// or not.
    fn poll_making(&mut self, ctx: &egui::Context) {
        let Some(viewer) = self.viewer.as_mut() else {
            return;
        };
        let Some(making) = viewer.views.making.as_mut() else {
            return;
        };
        let result = match making.rx.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => Err(anyhow::anyhow!("stopped without a result")),
        };
        let mut making = viewer.views.making.take().expect("being made");
        if let Some(worker) = making.worker.take() {
            let _ = worker.join();
        }
        let (ticket, view) = (making.ticket, making.view);
        if making.reload {
            // The make's own reload of the kept stream (`KeptStream::behind`): the set moves past
            // it — it is no view's reload. Only a reload seen puts the stream on the file again:
            // a make that failed before it (the file gone meanwhile) leaves it behind, and the
            // next make reloads.
            if let Some(kept) = viewer.views.stream.as_mut() {
                let seen = kept.told.moved();
                kept.at_save.moved();
                kept.caught_up(seen);
            }
        }
        let made = result.and_then(|opened| {
            // The first remote view made where none was kept (a page file's Hex or Text, a
            // handed-over image's): its stream is the views' from now on (D13).
            if viewer.views.stream.is_none() && viewer.views.means.label.is_some() {
                viewer.views.stream = opened.shared.stream.clone().map(KeptStream::new);
            }
            // A picture view whose file does not decode is not made (D10).
            if let (true, Some(why)) = (view.is_media(), &opened.undecodable) {
                anyhow::bail!("{why}");
            }
            let means = &viewer.views.means;
            let text = TextOpen {
                at: None,
                loading: false,
                enc: making.encoding,
            };
            embed(
                opened.prepared,
                &self.cache,
                means.title.clone(),
                &means.name,
                text,
                making.notice.take(),
            )
        });
        let input = match made {
            Ok(made) => {
                let read_only = viewer.views.read_only();
                let mut kept = kept(view, made, read_only);
                if let Some(at) = making.at.filter(|&at| at > 0) {
                    kept.embedded.window.go_to_byte(at);
                }
                viewer.views.hidden.push(kept);
                // Made hidden, the view told the stream it shares with the view on screen that
                // nobody sees it (one flag, 08 §8): the view on screen says it is seen again.
                // (A page view reads no stream; told again, it would say its failure again.)
                if viewer.views.shown.is_some_and(|v| v != View::Page) {
                    viewer.embedded.window.set_shown(true);
                }
                Input::Made(ticket)
            }
            Err(e) => {
                let words = crate::stream::explain(&e);
                if crate::stream::session_refused(&words) && view.is_media() {
                    // A refused session is no reason the file cannot be shown (D10): the make
                    // stops, the view on screen stays, and the refusal is said (06 §6).
                    self.view_input(ctx, Input::CancelMake);
                    self.status = Status::error(words);
                    return;
                }
                Input::MakeFailed(ticket, words)
            }
        };
        self.view_input(ctx, input);
    }

    /// A switch waiting for the view on screen (D2): its Save that ended without landing (one
    /// that landed is [`Browser::save_receipt`]'s), its prompt answered without closing.
    fn poll_saves(&mut self, ctx: &egui::Context) {
        let Some(viewer) = self.viewer.as_mut() else {
            return;
        };
        if viewer.views.shown.is_none() {
            return;
        }
        let (waiting, asking) = (viewer.views.set.waiting(), viewer.views.asked);
        if !(waiting || asking) {
            return;
        }
        let hold = viewer.embedded.window.hold();
        if waiting && hold != Hold::Saving {
            // The switch's `saving…` is over with no Save that landed: an Export or a Save copy
            // (they leave the file as it was), or a Save that did not. The set lets the view
            // stay (07 §10), and the switch is asked again: clean, it goes ahead; with edits
            // still unsaved, it asks the close prompt (D2).
            self.status = Status::default();
            let again = viewer.views.waited.take();
            self.view_input(ctx, Input::SaveEnded { landed: false });
            if let Some(slot) = again {
                self.view_input(ctx, Input::Choose(slot));
            }
            return;
        }
        let question = viewer.embedded.window.view_state() == view_keys::State::Question;
        if asking && hold != Hold::Saving && !question {
            // The prompt was answered and the window stays: Cancel, a Save from it that did not
            // land, or one that landed and kept the window open (its notes) — told already.
            viewer.views.asked = false;
            self.view_input(ctx, Input::Cancelled);
        }
    }

    /// Before the view's pass, over its `area`: F8 (or E) in Text and Raw opens the list of
    /// encodings; the list, while it is up, as a dialog of cells over the viewer, which keeps
    /// the keyboard from the view (08 §7.5).
    pub(super) fn picker_ui(&mut self, ctx: &egui::Context, area: egui::Rect) {
        let Some(viewer) = self.viewer.as_mut() else {
            return;
        };
        let views = &mut viewer.views;
        let text = matches!(views.shown, Some(View::Text | View::Raw));
        if views.picker.is_none() && text && !ctx.text_edit_focused() && !views.set.asking() {
            let asked = take_plain(ctx, Key::F8) || take_plain(ctx, Key::E);
            if let Some((current, pick)) = viewer.embedded.window.encoding().filter(|_| asked) {
                let at = Encoding::ALL.iter().position(|e| *e == current);
                views.picker = Some(Picker {
                    current,
                    pick,
                    at: at.unwrap_or(0),
                });
            }
        }
        let Some(picker) = views.picker.as_mut() else {
            return;
        };
        let all = Encoding::ALL;
        // The bar's row has egui's focus, so Enter or Space chooses it. ↑ and ↓ move it a row,
        // as this list's keys — those played into the pass too (`SOPEN_KEYS`), which egui's
        // own arrow moves (read before the pass) never see; Tab or a click moves it as egui does.
        if let Some(i) = (0..all.len()).find(|&i| ctx.memory(|m| m.has_focus(picker_row_id(i)))) {
            picker.at = i;
        }
        if take_plain(ctx, Key::ArrowDown) {
            picker.at = (picker.at + 1).min(all.len() - 1);
        }
        if take_plain(ctx, Key::ArrowUp) {
            picker.at = picker.at.saturating_sub(1);
        }
        ctx.memory_mut(|m| {
            m.move_focus(egui::FocusDirection::None);
            m.request_focus(picker_row_id(picker.at));
        });
        let labels: Vec<String> = all
            .iter()
            .map(|e| {
                let mark = if *e == picker.current { "•" } else { " " };
                let default = match picker.pick {
                    Some(p) if p.encoding == *e && p.guessed => " — the default, guessed",
                    Some(p) if p.encoding == *e => " — the default",
                    _ => "",
                };
                format!("{mark} {}{default}", e.label())
            })
            .collect();
        let widest = labels
            .iter()
            .map(|l| crate::gui::cells::width(l))
            .max()
            .unwrap_or(0);
        use super::ops::dialog::{self, Button, Part};
        let mut parts = vec![Part::Text(
            "The file's lines are read again in the one chosen.".to_string(),
            dialog::Ink::Weak,
        )];
        parts.extend(labels.iter().enumerate().map(|(i, l)| {
            Part::Buttons(vec![Button::new(
                &format!("{l:<widest$}"),
                picker_row_id(i),
            )])
        }));
        let grid = crate::gui::cells::Grid::over(area);
        let layout = dialog::layout(grid, "Encoding", 34, &parts);
        let shown = dialog::show(
            ctx,
            egui::Id::unique("sopen-encoding-list"),
            &layout,
            dialog::Inputs::default(),
        );
        let chosen = (0..all.len()).find(|&i| shown.clicked(picker_row_id(i)));
        let close = shown.modal.should_close();
        // The list keeps the keyboard: nothing of this pass reaches the view or the viewer's
        // keys behind it.
        ctx.input_mut(|i| {
            i.events
                .retain(|e| !matches!(e, egui::Event::Key { .. } | egui::Event::Text(_)))
        });
        if let Some(i) = chosen {
            views.picker = None;
            let enc = all[i];
            viewer.embedded.window.set_encoding(enc);
            // The set hears of it once the text view reads in it (`views_after`): a rebuild Esc
            // stops keeps the encoding it had (D22).
            views.chose = true;
        } else if close {
            views.picker = None;
        }
    }
}

/// A window made for `view`, kept hidden until the set shows it: told it is a view of the
/// viewer (the hex view opens viewing, D35), read-only for a revision, Raw for Raw.
fn kept(view: View, made: Made, read_only: bool) -> Kept {
    let mut embedded = made.embedded;
    if read_only {
        embedded.window.set_read_only(true);
    }
    embedded.window.set_in_viewer(true);
    if view == View::Raw {
        embedded.window.set_raw(true);
    }
    embedded.window.set_shown(false);
    Kept {
        view,
        embedded,
        hold: made.hold,
        switch: made.switch,
    }
}

/// D14: the view that used the save session is hidden or closed — the session goes back, now
/// or (an operation holds it) on a later pass ([`give_back`]).
fn ask_give_back(views: &mut Views, ctx: &egui::Context) {
    if let Some(save) = &views.save {
        views.giving = Some(save.ask());
        give_back(views, ctx);
    }
}

/// A give-back asked for, asked again: done, or the session is the view on screen's now (it
/// used it since), or — an operation holds it — again on a later pass.
fn give_back(views: &mut Views, ctx: &egui::Context) {
    let (Some(save), Some(asked)) = (&views.save, views.giving) else {
        return;
    };
    match save.give_back(asked) {
        prepare::GiveBack::NotNow => ctx.request_repaint_after(GIVE_BACK_AGAIN),
        prepare::GiveBack::Done | prepare::GiveBack::InUse => views.giving = None,
    }
}

/// How soon a give-back an operation held off is asked again.
const GIVE_BACK_AGAIN: std::time::Duration = std::time::Duration::from_millis(200);

/// What the view on screen holds now (`views::Now`): its edits or Save, its byte place.
fn views_now(viewer: &mut Viewer) -> Now {
    if viewer.views.shown.is_none() {
        return Now::CLEAN;
    }
    let window = &mut viewer.embedded.window;
    Now {
        hold: window.hold(),
        place: window.byte_place(),
    }
}
