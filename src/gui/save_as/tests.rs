//! Save As (07 §7): the writer over real folders — with producers that write, stall, cancel,
//! refuse, die, or take the chosen name meanwhile — the modal driven by real keys and clicks
//! through its `show`, the keys that ask for it, and the F-key rows that list it.

use super::*;
use crate::gui::chrome::{self, FKey};
use crate::gui::testkit::Driver;
use crate::gui::Window;
use eframe::egui::{Key, Modifiers};
use std::io::Write as _;
use std::os::unix::fs::PermissionsExt as _;
use std::sync::atomic::AtomicUsize;
use std::sync::Mutex;

const BODY: &[u8] = b"\x7fELF and then the edited bytes, as a window would make them\x00\x01\x02";

/// What a test does in the middle of a fill (after the first half is written).
type Mid = Box<dyn FnMut(&Ctl<'_>, &mut File) + Send>;

/// A window's producer: writes [`BODY`] in two halves, with progress after each, and runs `mid`
/// between them. Counts its fills and checks; `refuse` is its check's answer; `panics` kills
/// the worker in the fill; `heedless` never looks at the cancel.
struct Fake {
    body: Vec<u8>,
    fills: Arc<AtomicUsize>,
    checks: Arc<AtomicUsize>,
    mid: Option<Mid>,
    refuse: Option<String>,
    panics: bool,
    heedless: bool,
}

impl Fake {
    fn new() -> Self {
        Self {
            body: BODY.to_vec(),
            fills: Arc::default(),
            checks: Arc::default(),
            mid: None,
            refuse: None,
            panics: false,
            heedless: false,
        }
    }

    fn mid(mut self, mid: impl FnMut(&Ctl<'_>, &mut File) + Send + 'static) -> Self {
        self.mid = Some(Box::new(mid));
        self
    }
}

impl Producer for Fake {
    fn fill(&mut self, part: &mut File, ctl: &Ctl<'_>) -> Result<u64> {
        self.fills.fetch_add(1, Ordering::SeqCst);
        assert_eq!(
            part.metadata()?.len(),
            0,
            "every fill starts on an empty part"
        );
        if self.panics {
            panic!("the producer died mid-fill (a test)");
        }
        let half = self.body.len() / 2;
        part.write_all(&self.body[..half])?;
        ctl.progress(half as u64);
        if let Some(mid) = &mut self.mid {
            mid(ctl, part);
        }
        if !self.heedless && ctl.cancelled() {
            anyhow::bail!("stopped: the save was cancelled");
        }
        part.write_all(&self.body[half..])?;
        ctl.progress(self.body.len() as u64);
        Ok(self.body.len() as u64)
    }

    fn check(&mut self, written: u64) -> std::result::Result<(), String> {
        self.checks.fetch_add(1, Ordering::SeqCst);
        assert_eq!(written, self.body.len() as u64);
        match &self.refuse {
            Some(why) => Err(why.clone()),
            None => Ok(()),
        }
    }
}

/// The names in `dir` (sorted); any staging folder sopen made shows as `.sopen-…`.
fn names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// The private staging folder a running save has in `dir`, if any.
fn staging_in(dir: &Path) -> Option<PathBuf> {
    names(dir)
        .into_iter()
        .find(|n| n.starts_with(".sopen-") && n.len() == ".sopen-".len() + 16)
        .map(|n| dir.join(n))
}

/// [`write`] with a decision of the test's own and the progress it reported.
fn write_now(dest: &Path, size: Size, producer: &mut dyn Producer) -> (Outcome, Vec<u64>) {
    let decision = Decision::new();
    let seen = Mutex::new(Vec::new());
    let report = |done: u64| seen.lock().unwrap().push(done);
    let ctl = Ctl::new(&decision, &report);
    let outcome = write(dest, size, producer, &ctl);
    (outcome, seen.into_inner().unwrap())
}

fn exact() -> Size {
    Size::Exact(BODY.len() as u64)
}

// ---------------------------------------------------------------------------------------------
// The words.

/// The field's first value is `<download folder>/<stem>.edited<ext>`; when that is taken, the
/// next free `-N` (binary::suggest's rule, with its note).
#[test]
fn the_field_starts_at_the_edited_name_or_the_next_free_one() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    assert_eq!(
        first_path(dir, "model.bin"),
        (dir.join("model.edited.bin"), None)
    );
    assert_eq!(first_path(dir, "README").0, dir.join("README.edited"));
    std::fs::write(dir.join("model.edited.bin"), b"an earlier Save As").unwrap();
    let (path, note) = first_path(dir, "model.bin");
    assert_eq!(path, dir.join("model.edited-1.bin"));
    let note = note.expect("the note says why");
    assert!(note.contains("model.edited.bin is already in"), "{note}");
    assert!(note.ends_with("suggesting model.edited-1.bin"), "{note}");
    std::fs::write(dir.join("model.edited-1.bin"), b"").unwrap();
    assert_eq!(
        first_path(dir, "model.bin").0,
        dir.join("model.edited-2.bin")
    );
}

/// The size, and the time it takes at the rate the viewer is getting (07 §7's example: 24 GB at
/// 3.6 MB/s); only the size when no rate is known; an estimate says so.
#[test]
fn the_expected_time_is_at_the_viewers_rate() {
    let gb24 = Size::Exact(24_000_000_000);
    assert_eq!(
        expected_line(gb24, Some(3.6e6)),
        "24.0 GB — about 1 h 50 min at 3.6 MB/s"
    );
    assert_eq!(expected_line(gb24, None), "24.0 GB");
    assert_eq!(
        expected_line(gb24, Some(0.0)),
        "24.0 GB",
        "no rate measured"
    );
    assert_eq!(
        expected_line(Size::Exact(90_000_000), Some(1.5e6)),
        "90.0 MB — about 1 min at 1.5 MB/s"
    );
    assert_eq!(
        expected_line(Size::Exact(64), Some(1e6)),
        "64 B — about 1 s at 1.0 MB/s"
    );
    assert_eq!(
        expected_line(Size::About(24_000_000_000), Some(3.6e6)),
        "24.0 GB (estimated) — about 1 h 50 min at 3.6 MB/s"
    );
    assert_eq!(
        expected_line(Size::Exact(0), Some(3.6e6)),
        "0 B",
        "nothing to move"
    );
}

/// Progress is the bytes and the time left: at the transfer's own rate once it has run a
/// second, at the viewer's rate before that, and without a time when neither is known.
#[test]
fn progress_says_the_bytes_and_the_time_left() {
    let gb24 = Size::Exact(24_000_000_000);
    let done = 8_900_000_000;
    // 8.9 GB in 2472 s is 3.6 MB/s: 15.1 GB left is about 1 h 10 min.
    assert_eq!(
        progress_line(done, gb24, Duration::from_secs(2472), Some(1e9)),
        "37 % (8.9 of 24.0 GB) — about 1 h 10 min left"
    );
    // Under a second in: the viewer's rate.
    assert_eq!(
        progress_line(done, gb24, Duration::from_millis(500), Some(3.6e6)),
        "37 % (8.9 of 24.0 GB) — about 1 h 10 min left"
    );
    assert_eq!(
        progress_line(done, gb24, Duration::from_millis(500), None),
        "37 % (8.9 of 24.0 GB)"
    );
    assert_eq!(
        progress_line(32, Size::Exact(64), Duration::ZERO, Some(1e6)),
        "50 % (32 of 64 B) — about 1 s left"
    );
    // An estimate that the render runs past: what is written, no percentage past 100.
    assert_eq!(
        progress_line(
            25_000_000_000,
            Size::About(24_000_000_000),
            Duration::ZERO,
            None
        ),
        "25.0 GB written"
    );
}

/// The room check, exactly: less free space than the output refuses; as much is enough; a
/// folder that reports none is not refused here (the save says what is wrong with it).
#[test]
fn the_room_check_refuses_only_less_than_the_output() {
    let dir = Path::new("/somewhere");
    assert_eq!(
        room(Size::Exact(100), Some(99), dir),
        Some(Outcome::NoRoom {
            dir: dir.to_path_buf(),
            need: 100,
            free: 99
        })
    );
    assert_eq!(room(Size::Exact(100), Some(100), dir), None);
    assert_eq!(room(Size::Exact(100), None, dir), None);
    assert!(
        room(Size::About(100), Some(99), dir).is_some(),
        "an estimate too"
    );
}

// ---------------------------------------------------------------------------------------------
// The writer.

/// A Save As writes the producer's bytes, with its progress, and publishes them complete under
/// the chosen name with the mode a new file there gets; no staging folder is left.
#[test]
fn a_save_as_publishes_the_producers_bytes_complete() {
    let tmp = tempfile::tempdir().unwrap();
    let dest = tmp.path().join("model.edited.bin");
    let mut fake = Fake::new();
    let (fills, checks) = (fake.fills.clone(), fake.checks.clone());
    // Mid-fill nothing is under the name yet.
    let (d2, dir2) = (dest.clone(), tmp.path().to_path_buf());
    fake = fake.mid(move |_, _| {
        assert!(!d2.exists(), "nothing under the name before it is complete");
        assert!(
            staging_in(&dir2).is_some(),
            "the fill writes in the staging folder"
        );
    });
    let (outcome, progress) = write_now(&dest, exact(), &mut fake);
    assert_eq!(
        outcome,
        Outcome::Saved {
            path: dest.clone(),
            bytes: BODY.len() as u64,
            notes: Vec::new()
        }
    );
    assert_eq!(std::fs::read(&dest).unwrap(), BODY);
    assert_eq!(names(tmp.path()), ["model.edited.bin"]);
    assert_eq!(progress, [BODY.len() as u64 / 2, BODY.len() as u64]);
    assert_eq!(
        (fills.load(Ordering::SeqCst), checks.load(Ordering::SeqCst)),
        (1, 1)
    );
    let mode = std::fs::metadata(&dest).unwrap().permissions().mode() & 0o777;
    assert_ne!(mode & 0o600, 0, "the owner can read and write it: {mode:o}");
    assert_eq!(outcome.line(), format!("saved {}", binary::tilde(&dest)));
}

/// Too little room: refused before anything is made — no staging folder, no fill — by the
/// folder's real free space (no disk has an exbibyte free), naming both numbers.
#[test]
fn too_little_room_refuses_before_anything_is_written() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("keep.txt"), b"a neighbour").unwrap();
    let dest = tmp.path().join("huge.edited.bin");
    let mut fake = Fake::new();
    let fills = fake.fills.clone();
    let (outcome, progress) = write_now(&dest, Size::Exact(1 << 60), &mut fake);
    match &outcome {
        Outcome::NoRoom { dir, need, free } => {
            assert_eq!(dir, tmp.path());
            assert_eq!(*need, 1 << 60);
            assert!(*free < 1 << 60);
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(fills.load(Ordering::SeqCst), 0, "nothing was filled");
    assert!(progress.is_empty());
    assert_eq!(names(tmp.path()), ["keep.txt"], "nothing was made");
    let line = outcome.line();
    let needs = format!(
        "needs {} free in {}, has ",
        crate::human(1 << 60),
        binary::tilde(tmp.path())
    );
    assert!(line.starts_with(&needs), "{line}");
    assert!(line.ends_with("; nothing was written"), "{line}");
}

/// A name already taken when the save starts is refused with the next free `-N`, before any
/// transfer; what is there is untouched.
#[test]
fn a_taken_name_is_refused_with_the_next_free_one_and_untouched() {
    let tmp = tempfile::tempdir().unwrap();
    let dest = tmp.path().join("model.edited.bin");
    std::fs::write(&dest, b"the owner's earlier file").unwrap();
    let mut fake = Fake::new();
    let fills = fake.fills.clone();
    let (outcome, _) = write_now(&dest, exact(), &mut fake);
    assert_eq!(
        outcome,
        Outcome::Taken {
            path: dest.clone(),
            next: tmp.path().join("model.edited-1.bin")
        }
    );
    assert_eq!(fills.load(Ordering::SeqCst), 0);
    assert_eq!(std::fs::read(&dest).unwrap(), b"the owner's earlier file");
    assert_eq!(names(tmp.path()), ["model.edited.bin"]);
    let line = outcome.line();
    assert!(line.contains("exists — not replaced"), "{line}");
    assert!(line.ends_with("model.edited-1.bin"), "{line}");
}

/// The publish never overwrites: a name taken between the start and the publish (another
/// program writes it mid-transfer) is refused with the next free name, never replaced, and the
/// staging folder is gone.
#[test]
fn a_name_taken_during_the_transfer_is_never_overwritten() {
    let tmp = tempfile::tempdir().unwrap();
    let dest = tmp.path().join("model.edited.bin");
    let d2 = dest.clone();
    let mut fake = Fake::new().mid(move |_, _| {
        std::fs::write(&d2, b"someone else's, just now").unwrap();
    });
    let (outcome, _) = write_now(&dest, exact(), &mut fake);
    assert_eq!(
        outcome,
        Outcome::Taken {
            path: dest.clone(),
            next: tmp.path().join("model.edited-1.bin")
        }
    );
    assert_eq!(std::fs::read(&dest).unwrap(), b"someone else's, just now");
    assert_eq!(names(tmp.path()), ["model.edited.bin"], "no staging left");
}

/// A neighbour file and another sopen's staging folder (an older one, without a lock), with
/// what they hold.
fn neighbours(dir: &Path) -> PathBuf {
    std::fs::write(dir.join("keep.txt"), b"a neighbour").unwrap();
    let other = dir.join(".sopen-0123456789abcdef");
    std::fs::create_dir(&other).unwrap();
    std::fs::write(other.join("part"), b"another save's").unwrap();
    other
}

/// Cancelled mid-transfer: nothing is saved, and only this save's staging folder is removed —
/// a neighbour and another save's folder stay as they were.
#[test]
fn a_cancel_mid_transfer_removes_only_its_own_staging() {
    let tmp = tempfile::tempdir().unwrap();
    let other = neighbours(tmp.path());
    let dest = tmp.path().join("model.edited.bin");
    let dir2 = tmp.path().to_path_buf();
    let mine = Arc::new(Mutex::new(None));
    let mine2 = mine.clone();
    let decision = Arc::new(Decision::new());
    let stop = decision.clone();
    let mut fake = Fake::new().mid(move |_, _| {
        // Mid-way: this save's own folder is there, beside the other one; then Esc.
        let own: Vec<String> = names(&dir2)
            .into_iter()
            .filter(|n| n.starts_with(".sopen-") && n != ".sopen-0123456789abcdef")
            .collect();
        assert_eq!(own.len(), 1, "{own:?}");
        *mine2.lock().unwrap() = Some(dir2.join(&own[0]));
        assert!(stop.cancel(), "the cancel wins mid-transfer");
    });
    let report = |_: u64| {};
    let ctl = Ctl::new(&decision, &report);
    let outcome = write(&dest, exact(), &mut fake, &ctl);
    assert_eq!(outcome, Outcome::Cancelled { notes: Vec::new() });
    assert_eq!(outcome.line(), binary::SAVE_CANCELLED);
    let mine = mine.lock().unwrap().clone().expect("the fill ran");
    assert!(!mine.exists(), "its staging folder is gone");
    assert_eq!(
        names(tmp.path()),
        [".sopen-0123456789abcdef", "keep.txt"],
        "only its own"
    );
    assert_eq!(
        std::fs::read(other.join("part")).unwrap(),
        b"another save's"
    );
    assert_eq!(
        std::fs::read(tmp.path().join("keep.txt")).unwrap(),
        b"a neighbour"
    );
}

/// A producer that never looks at the cancel and returns its bytes anyway: the cancel still
/// wins — nothing is published, the check is not asked, nothing is left.
#[test]
fn a_cancel_wins_over_a_fill_that_ignores_it() {
    let tmp = tempfile::tempdir().unwrap();
    let dest = tmp.path().join("model.edited.bin");
    let decision = Arc::new(Decision::new());
    let stop = decision.clone();
    let mut fake = Fake::new().mid(move |_, _| {
        stop.cancel();
    });
    fake.heedless = true;
    let checks = fake.checks.clone();
    let report = |_: u64| {};
    let outcome = write(&dest, exact(), &mut fake, &Ctl::new(&decision, &report));
    assert_eq!(outcome, Outcome::Cancelled { notes: Vec::new() });
    assert_eq!(checks.load(Ordering::SeqCst), 0, "no check after a cancel");
    assert!(names(tmp.path()).is_empty(), "{:?}", names(tmp.path()));
}

/// A cancel that comes after the fill, while the check runs: the publish is still the
/// cancel's to stop — nothing gets the name, nothing is left.
#[test]
fn a_cancel_during_the_check_publishes_nothing() {
    struct CancelsInCheck(Arc<Decision>);
    impl Producer for CancelsInCheck {
        fn fill(&mut self, part: &mut File, ctl: &Ctl<'_>) -> Result<u64> {
            part.write_all(BODY)?;
            ctl.progress(BODY.len() as u64);
            Ok(BODY.len() as u64)
        }
        fn check(&mut self, _: u64) -> std::result::Result<(), String> {
            assert!(self.0.cancel(), "the cancel wins before the publish");
            Ok(())
        }
    }
    let tmp = tempfile::tempdir().unwrap();
    let dest = tmp.path().join("model.edited.bin");
    let decision = Arc::new(Decision::new());
    let mut producer = CancelsInCheck(decision.clone());
    let report = |_: u64| {};
    let outcome = write(&dest, exact(), &mut producer, &Ctl::new(&decision, &report));
    assert_eq!(outcome, Outcome::Cancelled { notes: Vec::new() });
    assert!(names(tmp.path()).is_empty(), "{:?}", names(tmp.path()));
}

/// The after-transfer check refuses (the source changed meanwhile): nothing is saved, `part`
/// and the staging folder are gone, and the reason is said.
#[test]
fn a_refusing_check_leaves_nothing_and_says_why() {
    let tmp = tempfile::tempdir().unwrap();
    let dest = tmp.path().join("model.edited.bin");
    let mut fake = Fake::new();
    fake.refuse = Some("host:/data/model.bin changed while it was read".to_string());
    let (outcome, _) = write_now(&dest, exact(), &mut fake);
    assert_eq!(
        outcome,
        Outcome::Refused {
            why: "host:/data/model.bin changed while it was read".to_string(),
            notes: Vec::new()
        }
    );
    assert!(names(tmp.path()).is_empty(), "{:?}", names(tmp.path()));
    assert_eq!(
        outcome.line(),
        "host:/data/model.bin changed while it was read; nothing was saved"
    );
}

/// A fill that fails leaves nothing and says why.
#[test]
fn a_failing_fill_leaves_nothing_and_says_why() {
    struct Broken;
    impl Producer for Broken {
        fn fill(&mut self, part: &mut File, _: &Ctl<'_>) -> Result<u64> {
            part.write_all(b"half of it")?;
            anyhow::bail!("the connection to host dropped")
        }
    }
    let tmp = tempfile::tempdir().unwrap();
    let dest = tmp.path().join("model.edited.bin");
    let (outcome, _) = write_now(&dest, exact(), &mut Broken);
    match &outcome {
        Outcome::Failed(msg) => assert!(msg.contains("the connection to host dropped"), "{msg}"),
        other => panic!("{other:?}"),
    }
    assert!(names(tmp.path()).is_empty(), "{:?}", names(tmp.path()));
}

/// When the staging folder cannot be removed after a cancel (something put a file in it), the
/// cancel says so — never silently left.
#[test]
fn a_staging_folder_that_cannot_be_removed_after_a_cancel_is_said() {
    let tmp = tempfile::tempdir().unwrap();
    let dest = tmp.path().join("model.edited.bin");
    let decision = Arc::new(Decision::new());
    let stop = decision.clone();
    let dir2 = tmp.path().to_path_buf();
    let mut fake = Fake::new().mid(move |_, _| {
        let staging = staging_in(&dir2).unwrap();
        std::fs::write(staging.join("stray"), b"not sopen's").unwrap();
        stop.cancel();
    });
    let report = |_: u64| {};
    let outcome = write(&dest, exact(), &mut fake, &Ctl::new(&decision, &report));
    match &outcome {
        Outcome::Cancelled { notes } => {
            assert_eq!(notes.len(), 1, "{notes:?}");
            assert!(notes[0].contains("could not be removed"), "{notes:?}");
        }
        other => panic!("{other:?}"),
    }
    assert!(!dest.exists());
}

/// The staging lock records the size only when it is exact: an estimate that the render runs
/// short of would let a later save sweep a complete output as a dead partial.
#[test]
fn the_lock_records_an_exact_size_only() {
    for (size, want) in [
        (exact(), format!("{}\n", BODY.len())),
        (Size::About(BODY.len() as u64 * 2), String::new()),
    ] {
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("out.bin");
        let dir2 = tmp.path().to_path_buf();
        let lock = Arc::new(Mutex::new(None));
        let lock2 = lock.clone();
        let mut fake = Fake::new().mid(move |_, _| {
            let staging = staging_in(&dir2).unwrap();
            *lock2.lock().unwrap() = Some(std::fs::read_to_string(staging.join("lock")).unwrap());
        });
        let (outcome, _) = write_now(&dest, size, &mut fake);
        assert!(matches!(outcome, Outcome::Saved { .. }), "{outcome:?}");
        assert_eq!(
            lock.lock().unwrap().as_deref(),
            Some(want.as_str()),
            "{size:?}"
        );
    }
}

/// Bytes in the plain files under `dir`, recursively: what a small disk holding only `dir` has
/// used.
fn used(dir: &Path) -> u64 {
    std::fs::read_dir(dir)
        .unwrap()
        .map(|e| {
            let path = e.unwrap().path();
            let meta = std::fs::symlink_metadata(&path).unwrap();
            if meta.is_dir() {
                used(&path)
            } else {
                meta.len()
            }
        })
        .sum()
}

/// What a killed save leaves never causes a "no room" refusal: the folder's dead partials are
/// swept (`binary::sweep_dead`, as `save_new` does) before its free space is measured. On a
/// 160-byte disk holding a killed save's 100-byte `part` (its lock recording 1000 bytes, nothing
/// holding it), the Save As does not fit until the partial goes; it goes, and the save lands.
#[test]
fn a_dead_partial_is_swept_before_the_room_is_measured() {
    const DISK: u64 = 160;
    let tmp = tempfile::tempdir().unwrap();
    let dead = tmp.path().join(".sopen-00000000deadbeef");
    std::fs::create_dir(&dead).unwrap();
    std::fs::set_permissions(&dead, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::write(dead.join("lock"), b"1000\n").unwrap();
    std::fs::write(dead.join("part"), [0u8; 100]).unwrap();
    let free_now = DISK - used(tmp.path());
    assert!(
        free_now < BODY.len() as u64,
        "with the partial there is no room: {free_now} free"
    );
    let mut small_disk = |dir: &Path| Some(DISK.saturating_sub(used(dir)));
    let decision = Decision::new();
    let report = |_: u64| {};
    let dest = tmp.path().join("model.edited.bin");
    let mut fake = Fake::new();
    let outcome = write_with(
        &dest,
        exact(),
        &mut fake,
        &Ctl::new(&decision, &report),
        &mut small_disk,
    );
    assert!(matches!(outcome, Outcome::Saved { .. }), "{outcome:?}");
    assert!(!dead.exists(), "the dead partial was swept");
    assert_eq!(names(tmp.path()), ["model.edited.bin"]);
    assert_eq!(std::fs::read(&dest).unwrap(), BODY);
}

/// The room check measures a folder whose name is not UTF-8 too (by the name's bytes): no disk
/// has an exbibyte free, so the save is refused there before anything is made — never skipped.
/// macOS (APFS) refuses to make such a name at all (`EILSEQ`): there the same runs on the
/// nearest name it takes, and the name that is not UTF-8 is Linux's case (CI).
#[test]
fn a_folder_whose_name_is_not_utf8_is_measured_too() {
    use std::os::unix::ffi::OsStrExt as _;
    let tmp = tempfile::tempdir().unwrap();
    let mut dir = tmp.path().join(std::ffi::OsStr::from_bytes(b"room-\xff"));
    match std::fs::create_dir(&dir) {
        Ok(()) => assert!(dir.to_str().is_none(), "the name is not UTF-8"),
        Err(e) if e.raw_os_error() == Some(libc::EILSEQ) => {
            assert_eq!(std::env::consts::OS, "macos", "only APFS refuses it: {e}");
            dir = tmp.path().join("room-\u{fffd}");
            std::fs::create_dir(&dir).unwrap();
        }
        Err(e) => panic!("cannot make {}: {e}", dir.display()),
    }
    let mut fake = Fake::new();
    let fills = fake.fills.clone();
    let (outcome, _) = write_now(&dir.join("huge.bin"), Size::Exact(1 << 60), &mut fake);
    assert!(
        matches!(&outcome, Outcome::NoRoom { dir: d, .. } if *d == dir),
        "{outcome:?}"
    );
    assert_eq!(fills.load(Ordering::SeqCst), 0, "nothing was filled");
    assert!(names(&dir).is_empty(), "{:?}", names(&dir));
}

// ---------------------------------------------------------------------------------------------
// The modal.

/// A window hosting the modal, as the hex, image and media windows will (ED4–ED6): it shows it
/// while it is open and keeps how it ended.
struct Host {
    modal: Option<SaveAs>,
    ended: Vec<Ended>,
}

impl Window for Host {
    fn ui(&mut self, ui: &mut egui::Ui) {
        if let Some(modal) = &mut self.modal {
            if let Some(ended) = modal.show(ui.ctx()) {
                self.ended.push(ended);
                self.modal = None;
            }
        }
    }

    fn ready(&self) -> bool {
        true
    }
}

impl Host {
    fn busy(&self) -> bool {
        self.modal.as_ref().is_some_and(SaveAs::busy)
    }

    fn modal(&self) -> &SaveAs {
        self.modal.as_ref().expect("the modal is open")
    }
}

/// The system's save dialog, answering `path`.
struct Answers(Option<PathBuf>);

impl binary::Chooser for Answers {
    fn choose(&mut self, _suggested: &Path) -> std::sync::mpsc::Receiver<Option<PathBuf>> {
        let (tx, rx) = std::sync::mpsc::channel();
        let _ = tx.send(self.0.clone());
        rx
    }
}

struct Fx {
    drv: Driver,
    host: Host,
    _tmp: tempfile::TempDir,
    dir: PathBuf,
    said: Arc<Mutex<Vec<String>>>,
}

fn modal_with(fake: Fake, size: Size, rate: Option<f64>, chooser: Answers) -> Fx {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().to_path_buf();
    let said = Arc::new(Mutex::new(Vec::new()));
    let said2 = said.clone();
    let job = Job {
        name: "model.bin".to_string(),
        size,
        rate,
        producer: Box::new(fake),
    };
    let modal = SaveAs::new(job, &dir, Box::new(chooser))
        .printing(move |line| said2.lock().unwrap().push(line.to_string()));
    let mut fx = Fx {
        drv: Driver::new(900.0, 600.0),
        host: Host {
            modal: Some(modal),
            ended: Vec::new(),
        },
        _tmp: tmp,
        dir,
        said,
    };
    fx.drv.idle(&mut fx.host, 3);
    fx
}

fn modal(fake: Fake) -> Fx {
    modal_with(fake, exact(), Some(1e6), Answers(None))
}

impl Fx {
    fn key(&mut self, key: Key) {
        self.drv.key(&mut self.host, key, Modifiers::NONE);
    }

    fn landed(&mut self) {
        self.drv.until(&mut self.host, 10, |h| !h.busy());
        self.drv.idle(&mut self.host, 2);
    }

    fn says(&self, text: &str) -> bool {
        self.drv.find_text(text).is_some() || self.drv.text_starting_with(text).is_some()
    }
}

/// The modal is a PC Shell box titled Save As: the path field prefilled with the edited name in
/// the folder, Browse…, and the size with the time expected at the viewer's rate.
#[test]
fn the_modal_shows_the_path_the_size_and_the_expected_time() {
    let fx = modal_with(
        Fake::new(),
        Size::Exact(24_000_000_000),
        Some(3.6e6),
        Answers(None),
    );
    assert!(fx.says("Save As"), "the title");
    assert_eq!(
        fx.host.modal().path,
        binary::tilde(&fx.dir.join("model.edited.bin"))
    );
    // In the field (a temp folder's path is wider than the field: it scrolls, so it is drawn
    // in part).
    let path = binary::tilde(&fx.dir.join("model.edited.bin"));
    assert!(
        fx.drv.galleys().iter().any(|(_, g)| g.text() == path),
        "in the field"
    );
    assert!(fx.says("Browse…"));
    assert!(fx.says("24.0 GB — about 1 h 50 min at 3.6 MB/s"));
    assert!(fx.host.ended.is_empty());
}

/// Enter saves; the window gets `saved <path>` and the same line goes to stdout; the modal is
/// done.
#[test]
fn enter_saves_and_the_result_line_is_also_on_stdout() {
    let mut fx = modal(Fake::new());
    fx.key(Key::Enter);
    fx.landed();
    let dest = fx.dir.join("model.edited.bin");
    assert_eq!(std::fs::read(&dest).unwrap(), BODY);
    assert!(fx.host.modal.is_none(), "the modal is done");
    match fx.host.ended.as_slice() {
        [Ended::Saved { path, line, notes }] => {
            assert_eq!(path, &dest);
            assert_eq!(line, &format!("saved {}", binary::tilde(&dest)));
            assert!(notes.is_empty());
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(
        fx.said.lock().unwrap().as_slice(),
        [format!("saved {}", dest.display())]
    );
}

/// A fill that waits mid-way until the test lets it go — or until the save is cancelled.
fn held() -> (Fake, std::sync::mpsc::Sender<()>) {
    let (go, wait) = std::sync::mpsc::channel::<()>();
    let wait = Mutex::new(wait);
    let fake = Fake::new().mid(move |ctl, _| {
        let until = Instant::now() + Duration::from_secs(10);
        while !ctl.cancelled() && Instant::now() < until {
            if wait.lock().unwrap().try_recv().is_ok() {
                return;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    });
    (fake, go)
}

/// While it writes, the modal shows the bytes and the time left and where it writes; Esc stops
/// it: only its staging folder is removed, it says nothing was saved, and the same producer
/// saves again from the start.
#[test]
fn esc_mid_transfer_cancels_and_the_same_producer_saves_again() {
    let (fake, go) = held();
    let fills = fake.fills.clone();
    let mut fx = modal(fake);
    let other = neighbours(&fx.dir);
    fx.key(Key::Enter);
    // The bytes and a time left (its words, at a rate, are `progress_line`'s test: here the
    // clock is the machine's).
    let half = BODY.len() / 2;
    let want = format!("50 % ({half} of {} B) — about ", BODY.len());
    fx.drv.until(&mut fx.host, 10, |h| {
        h.modal
            .as_ref()
            .and_then(SaveAs::progress_text)
            .is_some_and(|t| t.starts_with(&want) && t.ends_with(" left"))
    });
    fx.drv.idle(&mut fx.host, 1);
    let shown = fx.drv.text_starting_with(&want).expect("on screen");
    assert!(shown.ends_with(" left"), "{shown}");
    assert!(fx.says("saving to "), "where");
    assert!(fx.host.busy());
    fx.key(Key::Escape);
    fx.landed();
    assert!(fx.host.ended.is_empty(), "the modal stays");
    assert!(fx.says(binary::SAVE_CANCELLED));
    assert_eq!(names(&fx.dir), [".sopen-0123456789abcdef", "keep.txt"]);
    assert_eq!(
        std::fs::read(other.join("part")).unwrap(),
        b"another save's"
    );
    // Again: the fill starts over on a new part and lands.
    fx.key(Key::Enter);
    go.send(()).unwrap();
    fx.landed();
    assert_eq!(fills.load(Ordering::SeqCst), 2);
    assert_eq!(
        std::fs::read(fx.dir.join("model.edited.bin")).unwrap(),
        BODY
    );
    assert!(matches!(fx.host.ended.as_slice(), [Ended::Saved { .. }]));
}

/// The Cancel button stops a running save as Esc does.
#[test]
fn the_cancel_button_stops_a_running_save() {
    let (fake, _go) = held();
    let mut fx = modal(fake);
    fx.key(Key::Enter);
    fx.drv.until(&mut fx.host, 10, |h| {
        h.modal
            .as_ref()
            .is_some_and(|m| m.progress_text().is_some())
    });
    fx.drv.idle(&mut fx.host, 1);
    fx.drv.click_text(&mut fx.host, "Cancel");
    fx.landed();
    assert!(fx.says(binary::SAVE_CANCELLED));
    assert!(names(&fx.dir).is_empty(), "{:?}", names(&fx.dir));
    assert!(fx.host.ended.is_empty());
}

/// A name another program took mid-transfer: refused, never replaced, and the field now holds
/// the next free name, so Save again writes there.
#[test]
fn a_name_taken_meanwhile_puts_the_next_free_name_in_the_field() {
    let tmp_dest = Arc::new(Mutex::new(None::<PathBuf>));
    let d2 = tmp_dest.clone();
    let mut once = true;
    let fake = Fake::new().mid(move |_, _| {
        if std::mem::take(&mut once) {
            let dest = d2.lock().unwrap().clone().unwrap();
            std::fs::write(dest, b"someone else's").unwrap();
        }
    });
    let mut fx = modal(fake);
    let dest = fx.dir.join("model.edited.bin");
    *tmp_dest.lock().unwrap() = Some(dest.clone());
    fx.key(Key::Enter);
    fx.landed();
    let next = fx.dir.join("model.edited-1.bin");
    assert_eq!(fx.host.modal().path, binary::tilde(&next));
    assert!(
        fx.says("model.edited.bin exists — not replaced"),
        "the refusal"
    );
    assert_eq!(std::fs::read(&dest).unwrap(), b"someone else's");
    fx.key(Key::Enter);
    fx.landed();
    assert_eq!(std::fs::read(&next).unwrap(), BODY);
    assert_eq!(std::fs::read(&dest).unwrap(), b"someone else's");
}

/// Too little room, in the modal: refused at once, nothing made, the numbers said.
#[test]
fn no_room_is_refused_in_the_modal_before_anything_is_written() {
    let fake = Fake::new();
    let fills = fake.fills.clone();
    let mut fx = modal_with(fake, Size::Exact(1 << 60), None, Answers(None));
    fx.key(Key::Enter);
    fx.landed();
    assert_eq!(fills.load(Ordering::SeqCst), 0);
    assert!(names(&fx.dir).is_empty());
    assert!(
        fx.drv.text_starting_with("needs ").is_some(),
        "the refusal names the room"
    );
    assert!(fx.host.ended.is_empty());
}

/// Esc while asking, or the Cancel button, closes the modal and writes nothing.
#[test]
fn esc_or_cancel_while_asking_closes_without_writing() {
    for by_button in [false, true] {
        let fake = Fake::new();
        let fills = fake.fills.clone();
        let mut fx = modal(fake);
        if by_button {
            fx.drv.click_text(&mut fx.host, "Cancel");
        } else {
            fx.key(Key::Escape);
        }
        assert!(
            matches!(fx.host.ended.as_slice(), [Ended::Closed]),
            "{by_button}"
        );
        assert_eq!(fills.load(Ordering::SeqCst), 0);
        assert!(names(&fx.dir).is_empty());
    }
}

/// Browse… saves where the system's save dialog says; without an answer the field still works.
#[test]
fn browse_saves_where_the_system_dialog_says() {
    let tmp = tempfile::tempdir().unwrap();
    let chosen = tmp.path().join("chosen.bin");
    let mut fx = modal_with(Fake::new(), exact(), None, Answers(Some(chosen.clone())));
    fx.drv.click_text(&mut fx.host, "Browse…");
    fx.landed();
    assert_eq!(std::fs::read(&chosen).unwrap(), BODY);
    assert!(matches!(fx.host.ended.as_slice(), [Ended::Saved { path, .. }] if *path == chosen));

    let mut fx = modal_with(Fake::new(), exact(), None, Answers(None));
    fx.drv.click_text(&mut fx.host, "Browse…");
    fx.drv.idle(&mut fx.host, 2);
    assert!(fx.says(binary::BROWSE_NONE));
    assert!(fx.host.ended.is_empty());
}

/// A folder typed in the field gets the edited name inside it.
#[test]
fn a_folder_in_the_field_gets_the_edited_name() {
    let mut fx = modal(Fake::new());
    let inner = fx.dir.join("inner");
    std::fs::create_dir(&inner).unwrap();
    fx.host.modal.as_mut().unwrap().path = binary::tilde(&inner);
    fx.key(Key::Enter);
    fx.landed();
    assert_eq!(std::fs::read(inner.join("model.edited.bin")).unwrap(), BODY);
}

/// A worker that dies is a visible failure, not a spinner: the reason is said, Save is off,
/// nothing is left, and Esc closes the modal.
#[test]
fn a_dead_worker_is_a_visible_failure() {
    let mut fake = Fake::new();
    fake.panics = true;
    let mut fx = modal(fake);
    fx.key(Key::Enter);
    fx.landed();
    assert!(
        fx.drv
            .text_starting_with("the save stopped without a result")
            .is_some(),
        "the failure is said"
    );
    assert!(!fx.host.modal().can_save(), "nothing to save with any more");
    assert!(names(&fx.dir).is_empty(), "{:?}", names(&fx.dir));
    fx.key(Key::Escape);
    assert!(matches!(fx.host.ended.as_slice(), [Ended::Closed]));
}

/// Dropping the modal mid-transfer (its window went away) cancels the save and waits for it:
/// nothing is left.
#[test]
fn dropping_the_modal_mid_transfer_cancels_and_leaves_nothing() {
    let (fake, _go) = held();
    let mut fx = modal(fake);
    fx.key(Key::Enter);
    fx.drv.until(&mut fx.host, 10, |h| {
        h.modal
            .as_ref()
            .is_some_and(|m| m.progress_text().is_some())
    });
    fx.host.modal = None;
    assert!(names(&fx.dir).is_empty(), "{:?}", names(&fx.dir));
    assert!(fx.said.lock().unwrap().is_empty(), "nothing said saved");
}

// ---------------------------------------------------------------------------------------------
// The keys.

/// A window's keys: what it asks Save As of first, then its Cmd/Ctrl+S checks (egui's, which
/// ignore an extra Alt), then a text field that may have the typing.
#[derive(Default)]
struct Keys {
    save_as: usize,
    save_copy: usize,
    save: usize,
    field: Option<String>,
}

impl Window for Keys {
    fn ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        self.save_as += usize::from(pressed(&ctx));
        if crate::gui::command_key(&ctx, Key::S, true) {
            self.save_copy += 1;
        } else if crate::gui::command_key(&ctx, Key::S, false) {
            self.save += 1;
        }
        if let Some(text) = &mut self.field {
            let id = egui::Id::unique("keys-field");
            ui.add(chrome::field(text).id(id));
            ctx.memory_mut(|m| m.request_focus(id));
        }
    }

    fn ready(&self) -> bool {
        true
    }
}

/// Alt+Cmd/Ctrl+S and F7 ask for Save As, and are taken before the window's Cmd/Ctrl+S checks
/// see them; Cmd/Ctrl+S, Shift+Cmd/Ctrl+S, Shift+F7 and Alt+F7 do not; F7 does nothing while a
/// text field has the typing, and the chord still works then.
#[test]
fn f7_and_alt_command_s_ask_for_save_as() {
    let mut drv = Driver::new(600.0, 300.0);
    let alt_cmd = Modifiers::ALT | Modifiers::COMMAND;
    let mut win = Keys::default();
    drv.idle(&mut win, 1);
    drv.key(&mut win, Key::F7, Modifiers::NONE);
    assert_eq!((win.save_as, win.save, win.save_copy), (1, 0, 0), "F7");
    drv.key(&mut win, Key::S, alt_cmd);
    assert_eq!(
        (win.save_as, win.save, win.save_copy),
        (2, 0, 0),
        "Alt+Cmd/Ctrl+S"
    );
    drv.key(&mut win, Key::S, Modifiers::COMMAND);
    assert_eq!(
        (win.save_as, win.save, win.save_copy),
        (2, 1, 0),
        "Cmd/Ctrl+S is Save"
    );
    drv.key(&mut win, Key::S, Modifiers::COMMAND | Modifiers::SHIFT);
    assert_eq!(
        (win.save_as, win.save, win.save_copy),
        (2, 1, 1),
        "Save copy"
    );
    drv.key(&mut win, Key::S, alt_cmd | Modifiers::SHIFT);
    assert_eq!(win.save_as, 2, "Alt+Shift+Cmd/Ctrl+S is not Save As");
    drv.key(&mut win, Key::F7, Modifiers::SHIFT);
    drv.key(&mut win, Key::F7, Modifiers::ALT);
    assert_eq!(win.save_as, 2, "F7 with a modifier is another key");
    // A text field has the typing: F7 is not for it, the chord is.
    let mut win = Keys {
        field: Some("~/Downloads".to_string()),
        ..Keys::default()
    };
    drv.idle(&mut win, 2);
    drv.key(&mut win, Key::F7, Modifiers::NONE);
    assert_eq!(win.save_as, 0, "F7 in a field");
    drv.key(&mut win, Key::S, alt_cmd);
    assert_eq!(win.save_as, 1, "the chord in a field");
}

/// The key the editing windows list for Save As once each wires the modal (ED4–ED6) is F7
/// "SaveAs" — six characters at most, as every row label — and no text viewer row has it (the
/// text viewer is read-only), nor the browser's own rows.
#[test]
fn the_save_as_key_is_f7_save_as() {
    assert_eq!(
        chrome::SAVE_AS_KEY,
        FKey {
            n: 7,
            label: "SaveAs"
        }
    );
    assert_eq!(chrome::fkey(chrome::SAVE_AS_KEY.n), Some(Key::F7));
    assert!(chrome::SAVE_AS_KEY.label.chars().count() <= 6);
    for (name, row) in [
        ("text, wrapped", chrome::TEXT_WRAPPED_KEYS),
        ("text, unwrapped", chrome::TEXT_UNWRAPPED_KEYS),
        ("the browser's viewer row", chrome::VIEWER_KEYS),
        ("the list", chrome::LIST_KEYS),
    ] {
        assert!(row.iter().all(|k| k.n != 7), "{name}: {row:?}");
    }
}
