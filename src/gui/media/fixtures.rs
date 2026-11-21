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

    fn let_go(&self) -> bool {
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

/// Runs `body` on a thread of its own and fails when it has not ended in `secs`: a test of
/// something that must stop cannot hang the run.
pub(crate) fn bounded(secs: u64, body: impl FnOnce() + Send + 'static) {
    let (tx, rx) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        body();
        let _ = tx.send(());
    });
    match rx.recv_timeout(Duration::from_secs(secs)) {
        Ok(()) => worker.join().unwrap(),
        // The body panicked: its own message.
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            std::panic::resume_unwind(worker.join().unwrap_err())
        }
        Err(mpsc::RecvTimeoutError::Timeout) => panic!("not over in {secs} s"),
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
