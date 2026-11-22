//! [`RemoteFs::write_atomic_with`]: the steps it tells, in order, on every path — landed,
//! refused, failed, stopped by the caller — with no request beyond [`RemoteFs::write_atomic`]'s,
//! and the cleanup a failure gets whenever the caller stops it.

use super::*;
use std::sync::{Arc, Mutex};

/// The local disk, every request of the trait noted (the queries `can_link`, `can_replace` and
/// `hash_check_limit` send nothing); a create, a rename or a remove can be made to fail.
#[derive(Default)]
struct Counted {
    inner: LocalFs,
    requests: Arc<Mutex<Vec<String>>>,
    fail_create_after_bytes: bool,
    rename_unknown: bool,
    fail_rename: bool,
    fail_remove: bool,
}

impl Counted {
    fn note(&self, what: &str) {
        self.requests.lock().unwrap().push(what.to_string());
    }
}

impl RemoteFs for Counted {
    fn stat_opt(&mut self, path: &str) -> Result<Option<Stat>> {
        self.note(&format!("stat {path}"));
        self.inner.stat_opt(path)
    }
    fn lstat_opt(&mut self, path: &str) -> Result<Option<Stat>> {
        self.note(&format!("lstat {path}"));
        self.inner.lstat_opt(path)
    }
    fn realpath(&mut self, path: &str) -> Result<String> {
        self.note(&format!("realpath {path}"));
        self.inner.realpath(path)
    }
    fn read_head(&mut self, path: &str, max: usize) -> Result<Vec<u8>> {
        self.note(&format!("read-head {path}"));
        self.inner.read_head(path, max)
    }
    fn list_dir(&mut self, path: &str) -> Result<Vec<String>> {
        self.note(&format!("list {path}"));
        self.inner.list_dir(path)
    }
    fn list_entries(&mut self, path: &str) -> Result<Vec<Entry>> {
        self.note(&format!("list-entries {path}"));
        self.inner.list_entries(path)
    }
    fn free_space(&mut self, path: &str) -> Option<u64> {
        self.note(&format!("free-space {path}"));
        self.inner.free_space(path)
    }
    fn patcher(&mut self) -> Option<&mut dyn patch::PatchFs> {
        // Handles stay off (the requests above are the whole trace); asking is noted.
        self.note("patcher");
        None
    }
    fn read_to(
        &mut self,
        path: &str,
        out: &mut dyn Write,
        progress: &mut dyn FnMut(u64),
    ) -> Result<u64> {
        self.note(&format!("read {path}"));
        self.inner.read_to(path, out, progress)
    }
    fn create_from(
        &mut self,
        local: &Path,
        path: &str,
        like: Option<Like>,
    ) -> std::result::Result<Uploaded, CreateError> {
        self.note(&format!("create {path}"));
        let up = self.inner.create_from(local, path, like)?;
        if self.fail_create_after_bytes {
            return Err(CreateError::new(
                Created::Yes,
                anyhow!("simulated connection loss after upload"),
            ));
        }
        Ok(up)
    }
    fn set_mode(&mut self, path: &str, mode: u32) -> Result<()> {
        self.note(&format!("set-mode {path}"));
        self.inner.set_mode(path, mode)
    }
    fn rename_over(&mut self, from: &str, to: &str) -> Result<()> {
        self.note(&format!("rename {from} {to}"));
        if self.rename_unknown {
            return Err(unknown("simulated: rename sent, no reply"));
        }
        if self.fail_rename {
            bail!("simulated: the rename was refused");
        }
        self.inner.rename_over(from, to)
    }
    fn link_new(&mut self, from: &str, to: &str) -> Result<()> {
        self.note(&format!("link {from} {to}"));
        self.inner.link_new(from, to)
    }
    fn remove(&mut self, path: &str) -> Result<()> {
        self.note(&format!("remove {path}"));
        if self.fail_remove {
            bail!("simulated: permission denied");
        }
        self.inner.remove(path)
    }
}

/// A target, a local file with the new content, and the fingerprint the target was read at.
struct Rig {
    _tmp: tempfile::TempDir,
    target: String,
    local: std::path::PathBuf,
    at: Fingerprint,
}

fn make_rig() -> Rig {
    let tmp = tempfile::tempdir().unwrap();
    let target = tmp.path().join("shot.png");
    std::fs::write(&target, b"the original").unwrap();
    let local = tmp.path().join("new");
    std::fs::write(&local, b"the edited content").unwrap();
    let target = target.to_str().unwrap().to_string();
    let mut at = LocalFs::new().stat(&target).unwrap();
    at.sha256 = Some(Sha256::digest(b"the original").into());
    Rig {
        _tmp: tmp,
        target,
        local,
        at: at.fingerprint(),
    }
}

/// The steps a replace told, by name, with the temp each named.
fn run(
    fs: &mut Counted,
    rig: &Rig,
    mut stop: impl FnMut(&Step<'_>) -> Option<anyhow::Error>,
) -> (Result<WriteOutcome>, Vec<(&'static str, String)>) {
    let mut told = Vec::new();
    let result = fs.write_atomic_with(&rig.local, &rig.target, rig.at, &mut |step| {
        let (name, temp) = match &step {
            Step::Named { temp } => ("named", temp),
            Step::Written { temp, .. } => ("written", temp),
            Step::Staged { temp, .. } => ("staged", temp),
            Step::Renamed { temp } => ("renamed", temp),
            Step::Removed { temp } => ("removed", temp),
            Step::Left { temp } => ("left", temp),
        };
        told.push((name, temp.to_string()));
        match stop(&step) {
            Some(e) => Err(e),
            None => Ok(()),
        }
    });
    (result, told)
}

fn names(told: &[(&'static str, String)]) -> Vec<&'static str> {
    told.iter().map(|(n, _)| *n).collect()
}

/// A replace that lands tells named → written → staged → renamed, each of one temp; the
/// written and staged stats are the temp's; its requests are the pinned ones
/// ([`every_path_sends_the_pinned_requests`]).
#[test]
fn a_landed_replace_tells_every_step() {
    let rig = make_rig();
    let mut fs = Counted::default();
    let mut stats = Vec::new();
    let mut told = Vec::new();
    let result = fs.write_atomic_with(&rig.local, &rig.target, rig.at, &mut |step| {
        match &step {
            Step::Written { stat, .. } => stats.push(**stat),
            Step::Staged { stat, uploaded, .. } => {
                stats.push(**stat);
                assert_eq!(uploaded.size, b"the edited content".len() as u64);
            }
            _ => {}
        }
        told.push(format!("{step:?}"));
        Ok(())
    });
    let WriteOutcome::Written { now, .. } = result.unwrap() else {
        panic!("landed");
    };
    assert_eq!(told.len(), 4, "{told:?}");
    for (step, word) in told.iter().zip(["Named", "Written", "Staged", "Renamed"]) {
        assert!(step.starts_with(word), "{told:?}");
    }
    assert_eq!(
        trace(&fs, &rig.target),
        [
            "lstat T",
            "create t",
            "lstat t",
            "read T",
            "lstat T",
            "rename t T",
            "lstat T"
        ]
    );
    assert_eq!(stats[0], stats[1]);
    assert_eq!((now.size, now.mtime), (stats[1].size, stats[1].mtime));
    assert_eq!(std::fs::read(&rig.target).unwrap(), b"the edited content");
}

/// Stopped by the caller before the create: nothing is made, nothing is sent after the step,
/// and its error is the result.
#[test]
fn a_stop_at_named_makes_nothing() {
    let rig = make_rig();
    let mut fs = Counted::default();
    let (result, told) = run(&mut fs, &rig, |step| {
        matches!(step, Step::Named { .. }).then(|| anyhow!("not recorded"))
    });
    assert_eq!(format!("{:#}", result.unwrap_err()), "not recorded");
    assert_eq!(names(&told), ["named"]);
    assert!(!fs
        .requests
        .lock()
        .unwrap()
        .iter()
        .any(|r| r.starts_with("create ")));
    assert_eq!(std::fs::read(&rig.target).unwrap(), b"the original");
    let dir = std::path::Path::new(&rig.target).parent().unwrap();
    assert_eq!(std::fs::read_dir(dir).unwrap().count(), 2, "no temp");
}

/// Stopped after the temp was written, or once it is staged: the temp is removed — told — and
/// the target is untouched; a temp that cannot be removed is told left and named.
#[test]
fn a_stop_after_the_temp_removes_it_and_says_so() {
    for at in ["written", "staged"] {
        let rig = make_rig();
        let mut fs = Counted::default();
        let (result, told) = run(&mut fs, &rig, |step| {
            let here = matches!(
                (at, step),
                ("written", Step::Written { .. }) | ("staged", Step::Staged { .. })
            );
            here.then(|| anyhow!("stopped at {at}"))
        });
        let e = result.unwrap_err();
        assert_eq!(e.root_cause().to_string(), format!("stopped at {at}"));
        let mut want = vec!["named", "written"];
        if at == "staged" {
            want.push("staged");
        }
        want.push("removed");
        assert_eq!(names(&told), want, "{at}");
        assert!(told.iter().all(|(_, t)| *t == told[0].1), "one temp");
        assert!(!std::path::Path::new(&told[0].1).exists(), "{at}: removed");
        assert_eq!(std::fs::read(&rig.target).unwrap(), b"the original");
        assert!(!fs
            .requests
            .lock()
            .unwrap()
            .iter()
            .any(|r| r.starts_with("rename ")));
    }
    let rig = make_rig();
    let mut fs = Counted {
        fail_remove: true,
        ..Counted::default()
    };
    let (result, told) = run(&mut fs, &rig, |step| {
        matches!(step, Step::Staged { .. }).then(|| anyhow!("stopped"))
    });
    let e = format!("{:#}", result.unwrap_err());
    assert_eq!(names(&told), ["named", "written", "staged", "left"]);
    assert!(e.contains("is left behind"), "{e}");
    assert!(std::path::Path::new(&told[0].1).exists());
}

/// Failures and changes on the way tell where the temp went: a create that failed after it
/// wrote (removed), a target changed before the replace (removed, a conflict), a rename with
/// no reply (nothing after `staged`: it may have landed); and what the caller says once the
/// rename landed changes nothing.
#[test]
fn failures_tell_where_the_temp_went() {
    let rig = make_rig();
    let mut fs = Counted {
        fail_create_after_bytes: true,
        ..Counted::default()
    };
    let (result, told) = run(&mut fs, &rig, |_| None);
    assert!(result.is_err());
    assert_eq!(names(&told), ["named", "removed"]);
    assert!(!std::path::Path::new(&told[0].1).exists());

    let rig = make_rig();
    let mut fs = Counted::default();
    let target = rig.target.clone();
    let (result, told) = run(&mut fs, &rig, |step| {
        if matches!(step, Step::Written { .. }) {
            // Another writer, same size and mtime kept by nobody: the content check sees it.
            let mtime = std::fs::metadata(&target).unwrap().modified().unwrap();
            std::fs::write(&target, b"someone else").unwrap();
            let f = std::fs::File::options().write(true).open(&target).unwrap();
            f.set_modified(mtime).unwrap();
        }
        None
    });
    assert!(
        matches!(result.unwrap(), WriteOutcome::Conflict { .. }),
        "refused"
    );
    assert_eq!(names(&told), ["named", "written", "removed"]);

    let rig = make_rig();
    let mut fs = Counted {
        rename_unknown: true,
        ..Counted::default()
    };
    let (result, told) = run(&mut fs, &rig, |_| None);
    assert!(is_outcome_unknown(&result.unwrap_err()));
    assert_eq!(names(&told), ["named", "written", "staged"]);
    assert!(
        std::path::Path::new(&told[0].1).exists(),
        "kept: it may have landed"
    );

    let rig = make_rig();
    let mut fs = Counted::default();
    let (result, told) = run(&mut fs, &rig, |step| {
        matches!(step, Step::Renamed { .. }).then(|| anyhow!("could not record it"))
    });
    assert!(matches!(result.unwrap(), WriteOutcome::Written { .. }));
    assert_eq!(names(&told), ["named", "written", "staged", "renamed"]);
    assert_eq!(std::fs::read(&rig.target).unwrap(), b"the edited content");
}

/// The requests a replace sent, the target as `T`, the temp as `t`, their folder as `D`.
fn trace(fs: &Counted, target: &str) -> Vec<String> {
    let dir = target.rsplit_once('/').map(|(d, _)| d).unwrap_or_default();
    fs.requests
        .lock()
        .unwrap()
        .iter()
        .map(|r| {
            r.split(' ')
                .map(|w| {
                    if w == target {
                        "T"
                    } else if w.contains(".sopen-") {
                        "t"
                    } else if w == dir {
                        "D"
                    } else {
                        w
                    }
                })
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect()
}

/// The requests of a replace, pinned for each path through it — what `write_atomic` sent before
/// the seam was added (the same test ran on 54d921f) — and `write_atomic_with` with an observer
/// that stops nothing sends exactly these too: landed; refused at the first check; a content
/// change found after the upload (the temp removed); a create that failed after it wrote (the
/// temp removed); a rename whose reply was lost (nothing after it); a cleanup whose remove
/// failed (the temp left).
#[test]
fn every_path_sends_the_pinned_requests() {
    let success = [
        "lstat T",
        "create t",
        "lstat t",
        "read T",
        "lstat T",
        "rename t T",
        "lstat T",
    ];
    type Make = Box<dyn Fn() -> Counted>;
    type Before = Box<dyn Fn(&Rig)>;
    let cases: Vec<(&str, Make, Before, Vec<&str>)> = vec![
        (
            "landed",
            Box::new(Counted::default),
            Box::new(|_| {}),
            success.to_vec(),
        ),
        (
            "refused at the first check",
            Box::new(Counted::default),
            Box::new(|rig| std::fs::write(&rig.target, b"longer than the original").unwrap()),
            vec!["lstat T"],
        ),
        (
            "create failed after writing",
            Box::new(|| Counted {
                fail_create_after_bytes: true,
                ..Counted::default()
            }),
            Box::new(|_| {}),
            vec!["lstat T", "create t", "remove t"],
        ),
        (
            "rename reply lost",
            Box::new(|| Counted {
                rename_unknown: true,
                ..Counted::default()
            }),
            Box::new(|_| {}),
            vec![
                "lstat T",
                "create t",
                "lstat t",
                "read T",
                "lstat T",
                "rename t T",
            ],
        ),
    ];
    for (case, make, before, want) in &cases {
        for with_steps in [false, true] {
            let rig = make_rig();
            before(&rig);
            let mut fs = make();
            let _ = if with_steps {
                fs.write_atomic_with(&rig.local, &rig.target, rig.at, &mut |_| Ok(()))
            } else {
                fs.write_atomic(&rig.local, &rig.target, rig.at)
            };
            assert_eq!(
                trace(&fs, &rig.target),
                *want,
                "{case} (steps told: {with_steps})"
            );
        }
    }
    // A content change found after the upload: same size and mtime, other bytes.
    for with_steps in [false, true] {
        let rig = make_rig();
        let mut fs = Counted::default();
        let target = rig.target.clone();
        // Another writer before the replace reads it back: same size and mtime.
        let mtime = std::fs::metadata(&target).unwrap().modified().unwrap();
        std::fs::write(&target, b"the 0riginal").unwrap();
        std::fs::File::options()
            .write(true)
            .open(&target)
            .unwrap()
            .set_modified(mtime)
            .unwrap();
        let result = if with_steps {
            fs.write_atomic_with(&rig.local, &rig.target, rig.at, &mut |_| Ok(()))
        } else {
            fs.write_atomic(&rig.local, &rig.target, rig.at)
        };
        assert!(matches!(result.unwrap(), WriteOutcome::Conflict { .. }));
        assert_eq!(
            trace(&fs, &rig.target),
            ["lstat T", "create t", "lstat t", "read T", "remove t"],
            "content changed (steps told: {with_steps})"
        );
    }
    // A cleanup whose remove failed: the rename refused, the temp left.
    for with_steps in [false, true] {
        let rig = make_rig();
        let mut fs = Counted {
            fail_remove: true,
            fail_rename: true,
            ..Counted::default()
        };
        let result = if with_steps {
            fs.write_atomic_with(&rig.local, &rig.target, rig.at, &mut |_| Ok(()))
        } else {
            fs.write_atomic(&rig.local, &rig.target, rig.at)
        };
        assert!(format!("{:#}", result.unwrap_err()).contains("is left behind"));
        assert_eq!(
            trace(&fs, &rig.target),
            [
                "lstat T",
                "create t",
                "lstat t",
                "read T",
                "lstat T",
                "rename t T",
                "remove t"
            ],
            "remove failed (steps told: {with_steps})"
        );
    }
}
