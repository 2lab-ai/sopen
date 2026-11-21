//! What the media preview's tests share (here, in the preview's preparation and in the browser):
//! real media made by ffmpeg, stand-in programs that are one process each and wait where a test
//! holds them, a clock a test moves, and a bound on anything that could block.

pub(crate) use super::child::log::{
    most_at_once, naming, none_left, started_under, still_processes, still_running,
};
use crate::ffmpeg::Tools;
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

/// ffmpeg and ffprobe, or `None` (the test skips) where they are not installed — a failure
/// instead with `SOPEN_REQUIRE_FFMPEG` (CI).
pub(crate) fn tools() -> Option<Tools> {
    match crate::ffmpeg::resolve() {
        Ok(tools) => Some(tools),
        Err(e) if std::env::var_os("SOPEN_REQUIRE_FFMPEG").is_some() => {
            panic!("SOPEN_REQUIRE_FFMPEG is set but {e}")
        }
        Err(e) => {
            eprintln!("skipping media preview test: {e}");
            None
        }
    }
}

pub(crate) fn make(tools: &Tools, args: &[&str], dst: &Path) {
    let ok = std::process::Command::new(&tools.ffmpeg)
        .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-y"])
        .args(args)
        .arg(dst)
        .status()
        .unwrap()
        .success();
    assert!(ok, "fixture {}", dst.display());
}

/// 2 s of `w`×`h` at 25 fps with a sound track.
pub(crate) fn clip(tools: &Tools, dst: &Path, (w, h): (u32, u32)) {
    let picture = format!("testsrc2=size={w}x{h}:rate=25:duration=2");
    make(
        tools,
        &[
            "-f",
            "lavfi",
            "-i",
            &picture,
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000:duration=2",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
        ],
        dst,
    );
}

/// `secs` of a 440 Hz tone, mono, 8 kHz (a WAV: its samples are where its time says).
pub(crate) fn tone(tools: &Tools, dst: &Path, secs: u32) {
    let source = format!("sine=frequency=440:sample_rate=8000:duration={secs}");
    make(tools, &["-f", "lavfi", "-i", &source, "-ac", "1"], dst);
}

/// `secs` of a 440 Hz tone, mono, 8 kHz, quiet up to `cut` seconds and loud from there: what
/// is painted of it past `cut` cannot be mistaken for what is painted before.
pub(crate) fn quiet_then_loud(tools: &Tools, dst: &Path, secs: u32, cut: u32) {
    let source = format!("sine=frequency=440:sample_rate=8000:duration={secs}");
    let volume = format!("volume='if(lt(t,{cut}),0.05,7)':eval=frame");
    make(
        tools,
        &["-f", "lavfi", "-i", &source, "-af", &volume, "-ac", "1"],
        dst,
    );
}

/// An MP4 whose index is whole and whose pictures are cut off: ffprobe reads it as a video, and
/// no frame of it decodes.
pub(crate) fn index_only(tools: &Tools, dst: &Path) {
    let whole = dst.with_extension("whole.mp4");
    make(
        tools,
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=160x120:rate=25:duration=2",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-movflags",
            "+faststart",
        ],
        &whole,
    );
    let bytes = std::fs::read(&whole).unwrap();
    let mdat = bytes
        .windows(4)
        .position(|w| w == b"mdat")
        .expect("the pictures");
    assert!(
        bytes.windows(4).position(|w| w == b"moov").unwrap() < mdat,
        "the index comes first"
    );
    std::fs::write(dst, &bytes[..mdat + 4 + 64]).unwrap();
    std::fs::remove_file(whole).unwrap();
}

/// 1 s of 720×576 whose pixels are 16:11 wide: shown, it is 1047×576.
pub(crate) fn wide_pixels(tools: &Tools, dst: &Path) {
    make(
        tools,
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=720x576:rate=25:duration=1",
            "-vf",
            "setsar=16/11",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
        ],
        dst,
    );
}

/// 3 s of sound in six channels (5.1): a tone in every channel but the second (front right),
/// which is silent — a mix of them all would not be.
pub(crate) fn six_channels(tools: &Tools, dst: &Path) {
    make(
        tools,
        &[
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=8000:duration=3",
            "-af",
            "pan=5.1|c0=c0|c1=0*c0|c2=c0|c3=c0|c4=c0|c5=c0",
        ],
        dst,
    );
}

/// The middle one of `heights` (0 for none).
pub(crate) fn median(mut heights: Vec<f32>) -> f32 {
    heights.sort_by(f32::total_cmp);
    heights.get(heights.len() / 2).copied().unwrap_or(0.0)
}

/// 3 s of 160×120 above 8 bits (10-bit ProRes, an encoder every ffmpeg has): a video whose
/// Save and Export plan asks ffmpeg what its H.264 encoder takes.
pub(crate) fn deep_clip(tools: &Tools, dst: &Path) {
    make(
        tools,
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=160x120:rate=25:duration=3",
            "-c:v",
            "prores_ks",
            "-pix_fmt",
            "yuv422p10le",
        ],
        dst,
    );
}

/// A program standing in for ffmpeg or ffprobe: a shell script, `body` after the `-version`
/// answer (asked once here, so a start that finds the new file busy is retried before the
/// test's own). Bodies use shell builtins and `exec` only: the stand-in is one process, and its
/// command line names `dir` for as long as it has not become another program.
pub(crate) fn stand_in(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    let script = format!("#!/bin/sh\n[ \"$1\" = -version ] && exit 0\n{body}\n");
    std::fs::write(&path, script).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    crate::testutil::run_fresh(std::process::Command::new(&path).arg("-version"));
    path
}

/// Sound without end, from one process (the shell's own `printf`).
pub(crate) const NEVER_ENDS: &str =
    "while :; do printf '\\000\\000\\000\\000\\000\\000\\000\\000'; done";

/// Where a stand-in waits until the test lets it go: a named pipe it reads a line from (the
/// shell's own `read`: no second process). Never let go, it waits until it is killed. Dropped,
/// whatever still waits there is let go, so a test that failed leaves nothing waiting.
#[derive(Clone)]
pub(crate) struct Hold(Arc<PathBuf>);

impl Hold {
    pub(crate) fn new(dir: &Path, name: &str) -> Self {
        let path = dir.join(name);
        let c = std::ffi::CString::new(path.to_str().unwrap()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0, "mkfifo");
        Self(Arc::new(path))
    }

    /// The shell line that waits here.
    pub(crate) fn line(&self) -> String {
        format!("read _ < '{}'", self.0.display())
    }

    /// A stand-in that waits here and then fails: one that never answers.
    pub(crate) fn stalls(&self) -> String {
        format!("{}\nexit 1", self.line())
    }

    /// A stand-in that waits here and then is `program`.
    pub(crate) fn then(&self, program: &Path) -> String {
        format!("{}\nexec '{}' \"$@\"", self.line(), program.display())
    }

    /// Lets go what waits here now, if anything does (`false`: nothing waited).
    pub(crate) fn let_go(&self) -> bool {
        use std::io::Write as _;
        let pipe = std::fs::OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&*self.0);
        // No reader: nothing waits here.
        pipe.is_ok_and(|mut pipe| pipe.write_all(b"\n").is_ok())
    }

    /// Lets go what waits here (waits up to 10 s for something to wait).
    pub(crate) fn release(&self) {
        let give_up = Instant::now() + Duration::from_secs(10);
        while !self.let_go() {
            assert!(Instant::now() < give_up, "nothing came to wait at the hold");
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

impl Drop for Hold {
    fn drop(&mut self) {
        if Arc::strong_count(&self.0) == 1 {
            self.let_go();
        }
    }
}

/// A clock a test moves: it starts now and goes on only when told to.
#[derive(Clone)]
pub(crate) struct Hand(Arc<Mutex<Instant>>);

impl Hand {
    pub(crate) fn new() -> Self {
        Self(Arc::new(Mutex::new(Instant::now())))
    }

    pub(crate) fn now(&self) -> Instant {
        *self.0.lock().unwrap()
    }

    pub(crate) fn advance(&self, by: Duration) {
        *self.0.lock().unwrap() += by;
    }

    pub(crate) fn clock(&self) -> Arc<dyn Fn() -> Instant + Send + Sync> {
        let hand = self.clone();
        Arc::new(move || hand.now())
    }
}

/// What a supervised test ([`bounded`]) has started, so that a test that does not end can be
/// cleaned up after: its folders (every stand-in and every file a child reads is in one of
/// them), the holds its stand-ins wait at, and the clocks its budgets run on.
pub(crate) struct Test {
    _tmp: tempfile::TempDir,
    dirs: Mutex<Vec<PathBuf>>,
    holds: Mutex<Vec<Hold>>,
    hands: Mutex<Vec<Hand>>,
}

/// A test's folder (it lives as long as the test's supervisor).
#[derive(Clone)]
pub(crate) struct Dir(PathBuf);

impl Dir {
    pub(crate) fn path(&self) -> &Path {
        &self.0
    }
}

impl Test {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        // As a child's command line names it (a path through a link would not be found there).
        let dir = std::fs::canonicalize(tmp.path()).unwrap();
        Self {
            _tmp: tmp,
            dirs: Mutex::new(vec![dir]),
            holds: Mutex::default(),
            hands: Mutex::default(),
        }
    }

    /// The test's own folder.
    pub(crate) fn tmp(&self) -> Dir {
        Dir(self.dirs.lock().unwrap()[0].clone())
    }

    /// Another folder the test's children read from or run in (a fixture's own).
    pub(crate) fn also(&self, dir: &Path) {
        // By its name as given, and as the system resolves it.
        let resolved = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
        let mut dirs = self.dirs.lock().unwrap();
        dirs.push(dir.to_path_buf());
        dirs.push(resolved);
    }

    /// A hold in `dir`, known to the supervisor.
    pub(crate) fn hold(&self, dir: &Path, name: &str) -> Hold {
        let hold = Hold::new(dir, name);
        self.holds.lock().unwrap().push(hold.clone());
        hold
    }

    /// A clock, known to the supervisor.
    pub(crate) fn hand(&self) -> Hand {
        let hand = Hand::new();
        self.hands.lock().unwrap().push(hand.clone());
        hand
    }

    /// Ends everything the test started and left: every clock is moved a day on (whatever
    /// waits for its time is over it), every hold is let go, and every process that is one of
    /// the children logged for the test's folders, or names one of them in its command line, is
    /// killed — and the logged ones, this process's own children, reaped. Returns how many
    /// processes it had to kill. `grace`: how long it first waits for them to end by
    /// themselves (a stop only signals; the reaping is its runner's next look).
    fn clean_up(&self, grace: Duration) -> usize {
        let dirs = self.dirs.lock().unwrap().clone();
        let live = |dirs: &[PathBuf]| -> Vec<u32> {
            let mut live: Vec<u32> = Vec::new();
            for dir in dirs {
                let logged = started_under(dir);
                live.extend(logged.iter().filter(|pid| still_processes(&[**pid]) > 0));
                live.extend(super::child::log::pids_naming(dir));
            }
            live
        };
        let patient = Instant::now() + grace;
        while Instant::now() < patient && !live(&dirs).is_empty() {
            std::thread::sleep(Duration::from_millis(20));
        }
        for hand in self.hands.lock().unwrap().iter() {
            hand.advance(Duration::from_secs(86_400));
        }
        for hold in self.holds.lock().unwrap().iter() {
            hold.let_go();
        }
        // What was only waiting at a hold or for its time ends by itself now: given a moment
        // to, before anything is killed and counted (its runner reaps at its next look; a
        // stand-in let go still has to run to its end).
        let patient = Instant::now() + Duration::from_secs(2);
        while Instant::now() < patient && !live(&dirs).is_empty() {
            for hold in self.holds.lock().unwrap().iter() {
                hold.let_go();
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let mut killed = std::collections::BTreeSet::new();
        let give_up = Instant::now() + Duration::from_secs(5);
        loop {
            // Whatever is found is killed before the time is looked at: a look that took long
            // (a loaded machine) never leaves what it found.
            let live = live(&dirs);
            if live.is_empty() {
                return killed.len();
            }
            let last = Instant::now() >= give_up;
            for pid in live {
                if killed.insert(pid) && std::env::var_os("SOPEN_TEST_SHOW_KILLED").is_some() {
                    let out = std::process::Command::new("ps")
                        .args([
                            "-o",
                            "pid=,ppid=,stat=,etime=,command=",
                            "-p",
                            &pid.to_string(),
                        ])
                        .output()
                        .unwrap();
                    eprintln!("killed: {}", String::from_utf8_lossy(&out.stdout).trim());
                }
                unsafe {
                    libc::kill(pid as libc::pid_t, libc::SIGKILL);
                    // Our own child, when it is one: reaped here if its runner does not.
                    let mut status = 0;
                    libc::waitpid(pid as libc::pid_t, &mut status, libc::WNOHANG);
                }
            }
            if last {
                return killed.len();
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

/// Runs `body` on a thread of its own, with the [`Test`] it registers what it starts in, and
/// tells how it ended. A body that has not ended in `secs` is abandoned — but not what it
/// started: its clocks are moved on, its holds let go, and every process of its folders killed
/// and reaped ([`Test::clean_up`]) before this returns the failure.
///
/// **A body that this lets pass was cleaned up after, not verified.** The cleaning up lets go
/// every hold and moves every clock on, then gives what was waiting two seconds to end by
/// itself: a child the code under test left held can end there, with nothing to kill and
/// nothing counted. So the rule for every test of a child's life: it asserts, itself and before
/// its body returns, that no process is left ([`none_left`], as `export_ends` does in the
/// preview's tests) — never leaving that to this supervisor.
pub(crate) fn supervised(
    secs: u64,
    body: impl FnOnce(&Test) + Send + 'static,
) -> Result<(), String> {
    let test = Arc::new(Test::new());
    let (tx, rx) = mpsc::channel();
    let inside = test.clone();
    let worker = std::thread::spawn(move || {
        body(&inside);
        let _ = tx.send(());
    });
    let ended = match rx.recv_timeout(Duration::from_secs(secs)) {
        Ok(()) => {
            worker.join().unwrap();
            Ok(())
        }
        // The body panicked: its own message, after the cleaning up.
        Err(mpsc::RecvTimeoutError::Disconnected) => Err(worker.join().unwrap_err()),
        Err(mpsc::RecvTimeoutError::Timeout) => {
            let killed = test.clean_up(Duration::ZERO);
            return Err(format!(
                "not over in {secs} s; {killed} cleanup targets (processes it left) were killed"
            ));
        }
    };
    // A body that failed may have left something too.
    let killed = test.clean_up(Duration::from_secs(5));
    match ended {
        Ok(()) if killed == 0 => Ok(()),
        Ok(()) => Err(format!(
            "it ended and left {killed} cleanup targets (processes, killed)"
        )),
        Err(panic) => std::panic::resume_unwind(panic),
    }
}

/// [`supervised`], failing the test that calls it: a test of something that must stop cannot
/// hang the run, nor leave a process behind when it fails.
pub(crate) fn bounded(secs: u64, body: impl FnOnce(&Test) + Send + 'static) {
    if let Err(why) = supervised(secs, body) {
        panic!("{why}");
    }
}

/// Waits (at most 20 s) until `done`.
pub(crate) fn until(what: &str, mut done: impl FnMut() -> bool) {
    let give_up = Instant::now() + Duration::from_secs(20);
    while !done() {
        assert!(Instant::now() < give_up, "{what}: not in 20 s");
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// How long `call` took.
pub(crate) fn timed<T>(call: impl FnOnce() -> T) -> (T, Duration) {
    let started = Instant::now();
    let got = call();
    (got, started.elapsed())
}

/// Counts, from a thread of its own, the processes that name `dir` (every stand-in and ffmpeg a
/// test runs there), as the system lists them: the most seen at one look.
pub(crate) struct Census {
    stop: Arc<std::sync::atomic::AtomicBool>,
    looks: Option<std::thread::JoinHandle<(usize, usize)>>,
}

impl Census {
    pub(crate) fn start(dir: &Path) -> Self {
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (quit, dir) = (stop.clone(), dir.to_path_buf());
        let looks = std::thread::spawn(move || {
            let (mut most, mut looks) = (0, 0);
            while !quit.load(std::sync::atomic::Ordering::SeqCst) {
                most = most.max(naming(&dir));
                looks += 1;
            }
            (most, looks)
        });
        Self {
            stop,
            looks: Some(looks),
        }
    }

    /// The most processes seen at one look, and how many looks there were.
    pub(crate) fn end(mut self) -> (usize, usize) {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        self.looks.take().unwrap().join().unwrap()
    }
}

impl Drop for Census {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
    }
}
