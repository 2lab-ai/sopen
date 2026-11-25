//! The viewer's views (`.prd/08-viewer.md` §3–§6): which views an opened file has in slots 1, 2
//! and 3, which one is on screen and which are kept hidden, what the opening keeps for all of
//! them, and what choosing a view does. Screen-free (08 D31): no window and no I/O. A binding —
//! today's egui windows (VM-7), a terminal (VM-8) — opens a [`ViewSet`] once the file's kind is
//! known, reports what happened as an [`Input`] together with what the view on screen holds now
//! ([`Now`]), and carries out the [`Effect`]s it gets back, in their order.
//!
//! The rules, in short: a view is made the first time it is chosen; a clean view switched away
//! from is kept hidden and comes back as it was (D1); a view holding unsaved edits asks 07 §10's
//! close prompt and closes once a Save lands or Discard is chosen (D2); a running Save makes the
//! switch wait; a hidden view gives back its save connection (D14); a Save or a reload that lands
//! makes every hidden view stale (D25); Text, Raw and Hex share one byte place (D11).

use crate::kind::Kind;
use crate::remote::Stat;
use std::fmt;

/// One way of showing a file (08 §2). Raw is the Text view with highlighting off (D8): one
/// document, index, encoding and place.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum View {
    Image,
    Video,
    Audio,
    /// 09's page view (D9, D40).
    Page,
    Text,
    Raw,
    Hex,
}

impl View {
    /// The view's name on the strip (08 §8): `1 Image · 2 Hex`.
    pub fn name(self) -> &'static str {
        match self {
            View::Image => "Image",
            View::Video => "Video",
            View::Audio => "Audio",
            View::Page => "Page",
            View::Text => "Text",
            View::Raw => "Raw",
            View::Hex => "Hex",
        }
    }

    /// Text, Raw and Hex: the views with a byte place (08 §2).
    pub fn is_bytes(self) -> bool {
        matches!(self, View::Text | View::Raw | View::Hex)
    }

    /// Image, Video and Audio: the views a file that does not decode cannot show (D10).
    pub fn is_media(self) -> bool {
        matches!(self, View::Image | View::Video | View::Audio)
    }

    /// The views that save, and so hold a save connection once they have saved (06 §6): the
    /// image, media and hex windows. Text, Raw and Page are read-only.
    pub fn saves(self) -> bool {
        matches!(self, View::Image | View::Video | View::Audio | View::Hex)
    }

    fn doc(self) -> Doc {
        match self {
            View::Image => Doc::Image,
            View::Video => Doc::Video,
            View::Audio => Doc::Audio,
            View::Page => Doc::Page,
            View::Text | View::Raw => Doc::Text,
            View::Hex => Doc::Hex,
        }
    }
}

/// What is made, kept and closed: a view, but Text and Raw are one (D8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Doc {
    Image,
    Video,
    Audio,
    Page,
    Text,
    Hex,
}

impl Doc {
    const ALL: [Doc; 6] = [
        Doc::Image,
        Doc::Video,
        Doc::Audio,
        Doc::Page,
        Doc::Text,
        Doc::Hex,
    ];

    /// The view that names it in [`Effect::Close`]: the text document is `Text`.
    fn view(self) -> View {
        match self {
            Doc::Image => View::Image,
            Doc::Video => View::Video,
            Doc::Audio => View::Audio,
            Doc::Page => View::Page,
            Doc::Text => View::Text,
            Doc::Hex => View::Hex,
        }
    }
}

/// A view slot: the plain digits 1, 2, 3, or F9 then one of them (08 §4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Slot {
    One,
    Two,
    Three,
}

impl Slot {
    pub const ALL: [Slot; 3] = [Slot::One, Slot::Two, Slot::Three];

    /// 1, 2 or 3.
    pub fn number(self) -> u8 {
        self.index() as u8 + 1
    }

    /// The slot a digit names, if it is 1, 2 or 3.
    pub fn from_number(n: u8) -> Option<Slot> {
        match n {
            1 => Some(Slot::One),
            2 => Some(Slot::Two),
            3 => Some(Slot::Three),
            _ => None,
        }
    }

    fn index(self) -> usize {
        match self {
            Slot::One => 0,
            Slot::Two => 1,
            Slot::Three => 2,
        }
    }
}

/// Where 09's page view goes among a text file's views (09 §5–§6, 08 D40). 09 serves only
/// `.html`, `.htm` and `.xml` as pages: `.rss`, `.svg` and every other name are [`PageClass::Plain`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageClass {
    /// Not a page file.
    Plain,
    /// `.html`, `.htm`, and an `.xml` whose first 8 KiB holds `<?xml-stylesheet` before the
    /// first element: 1 Page · 2 Hex · 3 Text.
    PageFirst,
    /// Any other `.xml`, an RSS feed among them: 1 Text · 2 Hex · 3 Page.
    PageThird,
}

impl PageClass {
    /// The class of a file named `name`; `styled` says whether an `.xml` one's first 8 KiB holds
    /// `<?xml-stylesheet` before the first element — 09's `kind::page_default` reads that (WV-D).
    pub fn of(name: &str, styled: bool) -> PageClass {
        match crate::kind::extension(name).as_deref() {
            Some("html" | "htm") => PageClass::PageFirst,
            Some("xml") if styled => PageClass::PageFirst,
            Some("xml") => PageClass::PageThird,
            _ => PageClass::Plain,
        }
    }
}

/// What the screen the set is shown on can draw (08 §9; 09 §4, §8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Screen {
    /// Images and video: false in a terminal that draws no pictures. Then view 1 of an image or
    /// a video says why and the file opens on Hex (§9).
    pub pictures: bool,
    /// 09's page view: a web engine is here and the page view has landed. Without it HTML, HTM
    /// and XML are 1 Text · 2 Hex · 3 Raw (D30; 09 §4's fallback, 09 §8's terminal).
    pub pages: bool,
}

impl Screen {
    /// Today's egui windows with a web engine that contains a file view (09's page view, WV-D).
    pub const GUI: Screen = Screen {
        pictures: true,
        pages: true,
    };
    /// A window where file pages show their source (09 §4: no engine here, or one that does not
    /// contain a file view): HTML, HTM and XML are 1 Text · 2 Hex · 3 Raw (D30).
    pub const NO_ENGINE: Screen = Screen {
        pictures: true,
        pages: false,
    };
    /// A terminal that draws no pictures.
    pub const TERMINAL: Screen = Screen {
        pictures: false,
        pages: false,
    };
}

/// How the views reach the file's bytes (08 §6, D13).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reach {
    /// A local file, or a revision's or a diff's `.revisions` copy: read in place.
    Local,
    /// A remote file: the opening's one stream, kept beside the views, serves every view made
    /// later, so making a view reads nothing again.
    Stream,
    /// A handed-over image preview, which has no stream (`prepare.rs:887-890`): the first view
    /// made after it opens one ([`Effect::OpenStream`]), kept from then on — one channel, and the
    /// image window holds none.
    NoStream,
}

/// Why view 1 cannot show the file, so the set opens on 2 with this as its first row, and 1
/// says it on the status row (D10, §9).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Why {
    /// The decoder's words: an image that does not decode, a video none of whose frames decode.
    Undecodable(String),
    /// Empty media (0 bytes).
    Empty,
    /// The screen draws no pictures (a terminal, §9).
    NoPictures,
}

impl fmt::Display for Why {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Why::Undecodable(e) => write!(f, "{e}; the file is shown as bytes"),
            Why::Empty => f.write_str("the file is empty (0 bytes); it is shown as bytes"),
            Why::NoPictures => {
                f.write_str("this screen draws no pictures; the file is shown as bytes")
            }
        }
    }
}

/// What the opening keeps for every view of the file (08 §6). The binding keeps the handles
/// beside it: the stream ([`Reach::Stream`]) and the connection of [`Opening::path`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Opening {
    /// The kind, refined: a `.ts` as `kind::classify` and `kind::refine_after_fetch` leave it
    /// (`kind.rs:167-215`).
    pub kind: Kind,
    /// Where the page view goes, for a text file (ignored for any other kind).
    pub page: PageClass,
    /// The stat at open.
    pub stat: Stat,
    /// A file at a revision or a diff (03 §6.2): every view read-only.
    pub read_only: bool,
    /// The path of the bytes on screen: for a revision or a diff the `.revisions` copy, never the
    /// working file F4 opens (`browser.rs:1659-1664`, D13).
    pub path: String,
    pub reach: Reach,
    /// The sniffed head (at most `kind::SNIFF_BYTES`), so a view made later reads it no more.
    pub head: Vec<u8>,
    /// View 1 found at open that it cannot decode the file (a local image, `prepare.rs:240-247`):
    /// the decoder's words. A view that finds out once on screen says so by [`Input::Failed`].
    pub undecodable: Option<String>,
}

impl Opening {
    /// An opening of a file that is not a page, not read-only, with no head kept and nothing
    /// found undecodable; set the fields for the rest.
    pub fn new(kind: Kind, path: impl Into<String>, stat: Stat, reach: Reach) -> Opening {
        Opening {
            kind,
            page: PageClass::Plain,
            stat,
            read_only: false,
            path: path.into(),
            reach,
            head: Vec::new(),
            undecodable: None,
        }
    }
}

/// What the view on screen holds (07 §10's dirty state, by content).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hold {
    /// Nothing unsaved, no Save running.
    Clean,
    /// Unsaved edits.
    Unsaved,
    /// A Save runs.
    Saving,
}

/// What the view on screen says as an input comes: what it holds and, for a byte view, its
/// place (Text, Raw: the first byte of the top row; Hex: the cursor's byte). With no view on
/// screen it is not read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Now {
    pub hold: Hold,
    pub place: Option<u64>,
}

impl Now {
    pub const CLEAN: Now = Now {
        hold: Hold::Clean,
        place: None,
    };

    pub fn holding(hold: Hold) -> Now {
        Now { hold, place: None }
    }

    /// The same, the byte view on screen at `place`.
    pub fn at(self, place: u64) -> Now {
        Now {
            place: Some(place),
            ..self
        }
    }
}

/// Names one [`Effect::Make`]: its [`Input::Made`] or [`Input::MakeFailed`] echoes it, so the
/// answer of a make that was abandoned is told apart and ignored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ticket(u64);

/// What happened, as the binding reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Input {
    /// A view key chose the slot (08 §4; VM-6 decides which presses are view keys).
    Choose(Slot),
    /// Esc or F3 left the viewer (§5: "Leaving asks the view on screen"). A view key and a leave
    /// in the same input are reported as the leave alone, or the leave after it: either way no
    /// view is made ("Leave wins", §4). While a view is being made, Esc alone is
    /// [`Input::CancelMake`].
    Leave,
    /// The view [`Effect::Make`] asked for is made.
    Made(Ticket),
    /// It could not be made: the words for the status row. An Image, Video or Audio view that
    /// could not be made is one that cannot show the file (D10): 1 says why from then on, and
    /// with nothing on screen Hex shows the file, the words its first row.
    MakeFailed(Ticket, String),
    /// Esc while a view is being made: the make stops and the view on screen stays (§5). While
    /// [`ViewSet::making`] is `Some`, an Esc the binding gets from VM-6 as `Command::Leave` is
    /// this; F3, or an Esc in the same input as a view key, is [`Input::Leave`].
    CancelMake,
    /// The close prompt was cancelled. Every binding reports it: until it does, the set counts
    /// the prompt as up ([`ViewSet::asking`]).
    Cancelled,
    /// The view on screen agreed to close after the prompt: its Save or Save As landed, or
    /// Discard was chosen (07 §10). A landed Save's [`Input::SaveEnded`] comes first. Ignored
    /// unless the prompt is up: a switch chosen during the prompt's Save waits for that Save
    /// instead, so whichever of the two comes first, the view is kept.
    Closed,
    /// A Save of the view on screen ended; `landed` = it wrote the file. Not Save As or Save
    /// copy, which leave the file as it was.
    SaveEnded { landed: bool },
    /// The view on screen read the file again after it changed (06 §5).
    Reloaded,
    /// The media view on screen found that it cannot show the file after all (a video none of
    /// whose frames decode, a remote image that does not decode): it gives way to Hex (D10).
    Failed(Why),
    /// The text document now decodes by this encoding (VM-1's name): the opening's one encoding,
    /// for Text, Raw and a text document made later (D21).
    Encoding(String),
}

/// What the binding does, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    /// The view on screen goes hidden: it draws nothing, takes no input, reads nothing for a
    /// screen, polls nothing and plays nothing, and keeps what it holds — zoom and rectangle,
    /// playhead, In/Out and selection, wrap, the typing column (§5, D1). Raw hides the text
    /// document as Text does.
    Hide(View),
    /// The view just hidden gives its save connection back — to the placeholder that connects on
    /// next use — so a viewer stays at "1; 2 from its first Save" (D14). Nothing when it holds
    /// none.
    ReleaseSaveConnection(View),
    /// The view comes on screen as it was kept. `at`: the place followed the owner from another
    /// byte view (D11) — Hex puts its cursor on that byte with its row at the top, Text and Raw
    /// jump there by §7.3's rule, as `at=` does; `None`: its own place. Showing Raw over Text or
    /// Text over Raw only switches the document's highlighting. `viewing`: a Hex view comes up
    /// viewing — typed text is not read until Enter or Insert (D35); false for any other view.
    Show {
        view: View,
        at: Option<u64>,
        viewing: bool,
    },
    /// Open one stream for the file and keep it beside the views (§6: a handed-over image
    /// preview has none). Always right before a [`Effect::Make`].
    OpenStream,
    /// Make the view on a worker from what the opening kept, with progress and Esc on the status
    /// row; the view on screen, if any, stays until [`Input::Made`] says it is made. `at`: a
    /// byte view's place (its own, 0 the first time, or the one that followed the owner).
    /// `notice`: the made view's first row (D10's reason). `encoding`: for Text and Raw, the
    /// opening's one encoding once the text document has said it — the document made decodes
    /// by it (D21); `None`: §7.2's default, and always for the other views. `viewing`: as for
    /// [`Effect::Show`].
    Make {
        ticket: Ticket,
        view: View,
        at: Option<u64>,
        notice: Option<String>,
        encoding: Option<String>,
        viewing: bool,
    },
    /// Stop making the view of that ticket; whatever it answers later is ignored.
    Abandon(Ticket),
    /// Close the view (the text document for `Text`): the one on screen once its prompt was
    /// answered or once it failed, or a hidden one gone stale (D25). It is made again when next
    /// chosen, at its place.
    Close(View),
    /// Show 07 §10's close prompt for the view on screen: `Save the changes to host:/abs before
    /// closing?` → Save · Save As… · Discard · Cancel (D2). Its answer comes back as
    /// [`Input::Closed`], [`Input::SaveEnded`] or [`Input::Cancelled`].
    AskClose,
    /// A Save of the view on screen runs: say `saving…`; the switch waits for its
    /// [`Input::SaveEnded`] (D2).
    WaitForSave,
    /// Say this on the status row.
    Say(String),
    /// The viewer closes, every view of the set with it — the one on screen and the hidden ones.
    Leave,
}

/// What a switch goes on to once the view on screen lets it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Then {
    Show(View),
    Leave,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pending {
    Idle,
    /// A view is being made ([`Effect::Make`]); `noticed`: with D10's reason as its first row.
    Making {
        ticket: Ticket,
        view: View,
        noticed: bool,
    },
    /// The close prompt is up for the view on screen.
    Asking(Then),
    /// A Save of the view on screen runs.
    Waiting(Then),
}

#[derive(Debug, Clone, Copy, Default)]
struct DocState {
    /// Alive, on screen or hidden, and not stale.
    made: bool,
    /// A byte view's place (D11), kept while it is hidden or closed.
    place: u64,
}

/// One opened file's views: its slots, the view on screen, the hidden ones, and what the
/// opening keeps (08 §2: "the view set").
#[derive(Debug, Clone)]
pub struct ViewSet {
    opening: Opening,
    slots: [Option<View>; 3],
    unshown: Option<Why>,
    on_screen: Option<View>,
    docs: [DocState; 6],
    pending: Pending,
    reach: Reach,
    encoding: Option<String>,
    tickets: u64,
    left: bool,
}

/// §3's table: the views in slots 1, 2 and 3; `None` for a folder, which is not opened.
fn slots_of(kind: Kind, page: PageClass, screen: Screen) -> Option<[Option<View>; 3]> {
    use View::{Audio, Hex, Image, Page, Raw, Text, Video};
    Some(match kind {
        Kind::Directory => return None,
        Kind::Image => [Some(Image), Some(Hex), None],
        Kind::Video => [Some(Video), Some(Hex), None],
        Kind::Audio => [Some(Audio), Some(Hex), None],
        Kind::Text => match page {
            PageClass::PageFirst if screen.pages => [Some(Page), Some(Hex), Some(Text)],
            PageClass::PageThird if screen.pages => [Some(Text), Some(Hex), Some(Page)],
            _ => [Some(Text), Some(Hex), Some(Raw)],
        },
        // A binary: 1 is the same Hex view as 2, 3 its text (D7).
        Kind::Other => [Some(Hex), Some(Hex), Some(Text)],
    })
}

/// D10 and §9: why view 1 of this file cannot show it, known at open.
fn unshown_at_open(opening: &Opening, screen: Screen) -> Option<Why> {
    let pictured = matches!(opening.kind, Kind::Image | Kind::Video);
    if !(pictured || opening.kind == Kind::Audio) {
        return None;
    }
    if opening.stat.size == 0 {
        Some(Why::Empty)
    } else if let Some(e) = &opening.undecodable {
        Some(Why::Undecodable(e.clone()))
    } else if pictured && !screen.pictures {
        Some(Why::NoPictures)
    } else {
        None
    }
}

impl ViewSet {
    /// The set of a file just opened, on its first view — the one the opening makes: slot 1's,
    /// or Hex when view 1 cannot show the file (D10, §9; [`ViewSet::unshown`] is then the hex
    /// view's first row). `None` for a folder, which is listed, not opened.
    pub fn open(opening: Opening, screen: Screen) -> Option<ViewSet> {
        let slots = slots_of(opening.kind, opening.page, screen)?;
        let unshown = unshown_at_open(&opening, screen);
        let first = match unshown {
            Some(_) => View::Hex,
            None => slots[0]?,
        };
        let mut set = ViewSet {
            reach: opening.reach,
            opening,
            slots,
            unshown,
            on_screen: Some(first),
            docs: [DocState::default(); 6],
            pending: Pending::Idle,
            encoding: None,
            tickets: 0,
            left: false,
        };
        set.doc_mut(first).made = true;
        Some(set)
    }

    pub fn opening(&self) -> &Opening {
        &self.opening
    }

    /// Slots 1, 2 and 3; `None` = the key does nothing and the strip shows no such slot (D7).
    pub fn slots(&self) -> [Option<View>; 3] {
        self.slots
    }

    pub fn slot(&self, slot: Slot) -> Option<View> {
        self.slots[slot.index()]
    }

    /// The view on screen; `None` while the one that was there has closed and the next is being
    /// made, and once the viewer has left.
    pub fn on_screen(&self) -> Option<View> {
        self.on_screen
    }

    /// Why view 1 cannot show the file (D10, §9), from the opening or from [`Input::Failed`].
    pub fn unshown(&self) -> Option<&Why> {
        self.unshown.as_ref()
    }

    /// The view is alive — on screen or hidden — and not stale.
    pub fn is_made(&self, view: View) -> bool {
        self.doc(view).made
    }

    /// The views kept hidden.
    pub fn hidden(&self) -> Vec<View> {
        let shown = self.on_screen.map(View::doc);
        Doc::ALL
            .into_iter()
            .filter(|&d| self.docs[d as usize].made && Some(d) != shown)
            .map(Doc::view)
            .collect()
    }

    /// The view being made, if one is.
    pub fn making(&self) -> Option<View> {
        match self.pending {
            Pending::Making { view, .. } => Some(view),
            _ => None,
        }
    }

    /// The close prompt was asked for a switch or a leave and its answer has not come back
    /// ([`Input::Cancelled`], [`Input::Closed`], or a Save from it that did not land): the prompt
    /// is up, or the Save chosen in it runs.
    pub fn asking(&self) -> bool {
        matches!(self.pending, Pending::Asking(_))
    }

    /// A switch or a leave waits for a running Save.
    pub fn waiting(&self) -> bool {
        matches!(self.pending, Pending::Waiting(_))
    }

    /// A byte view's place, as last known (D11); `None` for the other views.
    pub fn place(&self, view: View) -> Option<u64> {
        view.is_bytes().then(|| self.doc(view).place)
    }

    /// How the views reach the bytes now: a [`Reach::NoStream`] opening becomes
    /// [`Reach::Stream`] once a view opened its stream.
    pub fn reach(&self) -> Reach {
        self.reach
    }

    pub fn read_only(&self) -> bool {
        self.opening.read_only
    }

    /// The opening's one encoding, once the text document has said it (D21).
    pub fn encoding(&self) -> Option<&str> {
        self.encoding.as_deref()
    }

    /// The viewer has left: nothing more happens.
    pub fn is_left(&self) -> bool {
        self.left
    }

    /// What `input` does, given what the view on screen holds `now`. The state moves; the
    /// effects are for the binding, in order.
    #[must_use]
    pub fn apply(&mut self, input: Input, now: Now) -> Vec<Effect> {
        let mut out = Vec::new();
        if self.left {
            return out;
        }
        match input {
            Input::Choose(slot) => self.choose(slot, now, &mut out),
            Input::Leave => {
                // Leave wins: a view being made is not made (§4).
                self.abandon(&mut out);
                self.go(Then::Leave, now, &mut out);
            }
            Input::Made(ticket) => {
                if let Pending::Making {
                    ticket: t, view, ..
                } = self.pending
                {
                    if t == ticket {
                        self.pending = Pending::Idle;
                        self.doc_mut(view).made = true;
                        // The view on screen may have been edited or started a Save meanwhile.
                        self.go(Then::Show(view), now, &mut out);
                    }
                }
            }
            Input::MakeFailed(ticket, words) => {
                if let Pending::Making {
                    ticket: t, view, ..
                } = self.pending
                {
                    if t == ticket {
                        self.pending = Pending::Idle;
                        self.make_failed(view, words, &mut out);
                    }
                }
            }
            Input::CancelMake => {
                if matches!(self.pending, Pending::Making { .. }) {
                    self.abandon(&mut out);
                    self.leave_if_blank(&mut out);
                }
            }
            Input::Cancelled => {
                if self.asking() {
                    self.pending = Pending::Idle;
                }
            }
            Input::Closed => {
                if let Pending::Asking(then) = self.pending {
                    self.carry_out(then, now.place, true, &mut out);
                }
            }
            Input::SaveEnded { landed } => self.save_ended(landed, now, &mut out),
            Input::Reloaded => self.stale(&mut out),
            Input::Failed(why) => self.failed(why, &mut out),
            Input::Encoding(name) => self.encoding = Some(name),
        }
        out
    }

    fn choose(&mut self, slot: Slot, now: Now, out: &mut Vec<Effect>) {
        // none: the key does nothing (D7).
        let Some(target) = self.slot(slot) else {
            return;
        };
        // D10, §9: view 1 says why it cannot show the file, and the set stays where it is.
        if let (Slot::One, Some(why)) = (slot, &self.unshown) {
            out.push(Effect::Say(why.to_string()));
            return;
        }
        // The view on screen chosen again does nothing (§4), whatever waits.
        if self.on_screen == Some(target) {
            return;
        }
        if let Pending::Making { view, .. } = &mut self.pending {
            if view.doc() == target.doc() {
                // Being made already; Text or Raw is only which way it comes up.
                *view = target;
                return;
            }
        }
        // Another view: it replaces whatever was under way.
        self.abandon(out);
        self.go(Then::Show(target), now, out);
    }

    /// The switch or leave goes on as far as the view on screen lets it (D2).
    fn go(&mut self, then: Then, now: Now, out: &mut Vec<Effect>) {
        if self.on_screen.is_some() {
            match now.hold {
                Hold::Clean => {}
                Hold::Unsaved => {
                    self.pending = Pending::Asking(then);
                    out.push(Effect::AskClose);
                    return;
                }
                Hold::Saving => {
                    self.pending = Pending::Waiting(then);
                    out.push(Effect::WaitForSave);
                    return;
                }
            }
        }
        self.carry_out(then, now.place, false, out);
    }

    /// The view on screen lets it: it is hidden, or — `closing`, after its prompt — closed.
    fn carry_out(&mut self, then: Then, place: Option<u64>, closing: bool, out: &mut Vec<Effect>) {
        self.pending = Pending::Idle;
        let old = self.on_screen;
        if let (Some(old), Some(place)) = (old, place) {
            if old.is_bytes() {
                self.doc_mut(old).place = place;
            }
        }
        let target = match then {
            Then::Show(target) => target,
            Then::Leave => {
                // Hidden views close with the one on screen (§5).
                self.left = true;
                self.on_screen = None;
                out.push(Effect::Leave);
                return;
            }
        };
        // D11: between byte views the place follows the owner; from any other view a byte
        // view keeps its own.
        let follow = old
            .filter(|o| o.is_bytes() && target.is_bytes())
            .map(|o| self.doc(o).place);
        if let Some(place) = follow {
            self.doc_mut(target).place = place;
        }
        if let Some(old) = old {
            if closing {
                out.push(Effect::Close(old));
                self.doc_mut(old).made = false;
                self.on_screen = None;
            } else if old.doc() == target.doc() {
                // Text ↔ Raw: one view state (D8, D11).
                self.show(target, None, out);
                return;
            }
        }
        if self.doc(target).made {
            if let Some(old) = self.on_screen {
                self.hide(old, out);
            }
            self.show(target, follow, out);
        } else {
            self.make(target, None, out);
        }
    }

    fn hide(&mut self, view: View, out: &mut Vec<Effect>) {
        out.push(Effect::Hide(view));
        // D14; a revision's views never save, so they hold no save connection.
        if view.saves() && !self.opening.read_only {
            out.push(Effect::ReleaseSaveConnection(view));
        }
    }

    fn show(&mut self, view: View, at: Option<u64>, out: &mut Vec<Effect>) {
        self.on_screen = Some(view);
        out.push(Effect::Show {
            view,
            at,
            viewing: view == View::Hex,
        });
    }

    fn make(&mut self, view: View, notice: Option<String>, out: &mut Vec<Effect>) {
        if self.reach == Reach::NoStream {
            out.push(Effect::OpenStream);
            self.reach = Reach::Stream;
        }
        let ticket = Ticket(self.tickets);
        self.tickets += 1;
        self.pending = Pending::Making {
            ticket,
            view,
            noticed: notice.is_some(),
        };
        out.push(Effect::Make {
            ticket,
            view,
            at: self.place(view),
            notice,
            encoding: self.encoding.clone().filter(|_| view.doc() == Doc::Text),
            viewing: view == View::Hex,
        });
    }

    fn make_failed(&mut self, view: View, words: String, out: &mut Vec<Effect>) {
        if !view.is_media() {
            out.push(Effect::Say(words));
            self.leave_if_blank(out);
            return;
        }
        // D10: the media view cannot show the file, so 1 says why rather than trying again.
        let why = Why::Undecodable(words);
        if self.on_screen.is_none() {
            // Every file has a hex view (§1).
            self.give_way_to_hex(why, out);
        } else {
            out.push(Effect::Say(why.to_string()));
            self.unshown = Some(why);
        }
    }

    fn abandon(&mut self, out: &mut Vec<Effect>) {
        if let Pending::Making { ticket, .. } = self.pending {
            out.push(Effect::Abandon(ticket));
            self.pending = Pending::Idle;
        }
    }

    /// A make that ended with nothing on screen — the view that was there closed after its
    /// prompt, or failed — leaves the viewer: there is no view to stay on. (A media view's
    /// failed make gives way to Hex instead: `make_failed`.)
    fn leave_if_blank(&mut self, out: &mut Vec<Effect>) {
        if self.on_screen.is_none() {
            self.left = true;
            out.push(Effect::Leave);
        }
    }

    fn save_ended(&mut self, landed: bool, now: Now, out: &mut Vec<Effect>) {
        if landed {
            self.stale(out);
        }
        match self.pending {
            // A switch that waited on this Save goes on; the view, clean now, is kept (D1).
            Pending::Waiting(then) if landed => {
                self.pending = Pending::Idle;
                self.go(then, now, out);
            }
            // A Save that did not land keeps the view, as closing does (07 §10). After the
            // prompt's Save that landed, the view's own close comes as `Closed`.
            Pending::Waiting(_) | Pending::Asking(_) if !landed => self.pending = Pending::Idle,
            _ => {}
        }
    }

    /// D25: the file changed under every hidden view; each is closed, and made again when next
    /// shown, at its place. A view being made read it before the change: it is made again.
    fn stale(&mut self, out: &mut Vec<Effect>) {
        let shown = self.on_screen.map(View::doc);
        for doc in Doc::ALL {
            if self.docs[doc as usize].made && Some(doc) != shown {
                self.docs[doc as usize].made = false;
                out.push(Effect::Close(doc.view()));
            }
        }
        if let Pending::Making { view, noticed, .. } = self.pending {
            self.abandon(out);
            let notice = noticed
                .then(|| self.unshown.as_ref().map(ToString::to_string))
                .flatten();
            self.make(view, notice, out);
        }
    }

    /// D10, found once on screen: the media view closes and the file is shown on Hex, its first
    /// row the reason; 1 says it from now on.
    fn failed(&mut self, why: Why, out: &mut Vec<Effect>) {
        let Some(view) = self.on_screen.filter(|v| v.is_media()) else {
            return;
        };
        out.push(Effect::Close(view));
        self.doc_mut(view).made = false;
        self.on_screen = None;
        self.give_way_to_hex(why, out);
    }

    /// D10 with nothing on screen: from now on 1 says `why`, and Hex shows the file, `why` its
    /// first row. Whatever was under way goes; a Hex view being made is made again with it.
    fn give_way_to_hex(&mut self, why: Why, out: &mut Vec<Effect>) {
        let notice = why.to_string();
        self.unshown = Some(why);
        self.abandon(out);
        self.pending = Pending::Idle;
        if self.doc(View::Hex).made {
            self.show(View::Hex, None, out);
            out.push(Effect::Say(notice));
        } else {
            self.make(View::Hex, Some(notice), out);
        }
    }

    fn doc(&self, view: View) -> &DocState {
        &self.docs[view.doc() as usize]
    }

    fn doc_mut(&mut self, view: View) -> &mut DocState {
        &mut self.docs[view.doc() as usize]
    }
}

#[cfg(test)]
mod tests;
