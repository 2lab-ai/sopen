//! Live SFTP checks of the conflict-safe write path (spec §5) against a real sshd. Ignored by
//! default — tests never need ssh. Run them with an ssh alias whose remote has the directory:
//!
//! ```sh
//! SOPEN_TEST_SFTP_HOST=<alias> SOPEN_TEST_SFTP_DIR=/tmp/sopen-receipts \
//!   cargo test --test sftp_live -- --ignored --nocapture
//! ```

use sopen::remote::{join, random_hex, RemoteFs, SftpRemote, WriteOutcome};
use sopen::target::RemoteSpec;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

struct Live {
    spec: RemoteSpec,
    state: PathBuf,
    dir: String,
}

fn live() -> Option<Live> {
    let Ok(host) = std::env::var("SOPEN_TEST_SFTP_HOST") else {
        eprintln!("skipping: SOPEN_TEST_SFTP_HOST is not set");
        return None;
    };
    let dir = std::env::var("SOPEN_TEST_SFTP_DIR").unwrap_or_else(|_| "/tmp/sopen-receipts".into());
    Some(Live {
        spec: RemoteSpec {
            user: None,
            host,
            path: dir.clone(),
        },
        state: sopen::cache::dirs().unwrap().state,
        dir,
    })
}

impl Live {
    fn connect(&self) -> SftpRemote {
        SftpRemote::connect(&self.spec, &self.state).unwrap()
    }
}

/// `.<name>.sopen-<16 hex>` temps in `dir` — none may survive a write.
fn temps(fs: &mut SftpRemote, dir: &str) -> Vec<String> {
    fs.list_dir(dir)
        .unwrap()
        .into_iter()
        .filter(|n| {
            n.rsplit_once(".sopen-")
                .is_some_and(|(_, t)| t.len() == 16 && t.chars().all(|c| c.is_ascii_hexdigit()))
        })
        .collect()
}

fn read(fs: &mut SftpRemote, path: &str) -> String {
    let mut out = Vec::new();
    fs.read_to(path, &mut out, &mut |_| {}).unwrap();
    String::from_utf8(out).unwrap()
}

fn local_file(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, body).unwrap();
    path
}

#[test]
#[ignore = "needs SOPEN_TEST_SFTP_HOST=<ssh alias> and an existing SOPEN_TEST_SFTP_DIR"]
fn sftp_write_atomic_refuses_a_changed_remote() {
    let Some(live) = live() else { return };
    let local = tempfile::tempdir().unwrap();
    let v1 = local_file(local.path(), "v1", "version one\n");

    // Writer A creates the file (a new sibling, never over an existing one) and fetches it.
    let mut a = live.connect();
    let dir = a.realpath(&live.dir).unwrap();
    let name = a
        .write_new_sibling(&v1, &dir, &format!("conflict-{}.txt", random_hex()))
        .unwrap();
    let target = join(&dir, &name);
    a.set_mode(&target, 0o640).unwrap();
    let copy = local.path().join("copy");
    let at = a.fetch(&target, &copy, &mut |_, _| {}).unwrap();
    println!("fetched {target}: size={} mode={:o}", at.size, at.mode);

    // Writer B (its own SFTP channel) changes the remote file.
    let mut b = live.connect();
    let v2 = local_file(local.path(), "v2", "version two, from another writer\n");
    let b_at = b.stat(&target).unwrap();
    let b_out = b.write_atomic(&v2, &target, b_at.fingerprint()).unwrap();
    assert!(matches!(b_out, WriteOutcome::Written { .. }), "{b_out:?}");
    println!("second writer replaced {target}");

    // A's stale save is refused and the remote keeps B's content.
    std::fs::write(&copy, "A's edit made on the stale copy\n").unwrap();
    let outcome = a.write_atomic(&copy, &target, at.fingerprint()).unwrap();
    println!("stale write_atomic -> {outcome:?}");
    let WriteOutcome::Conflict { now: Some(now), .. } = outcome else {
        panic!("expected Conflict, got {outcome:?}")
    };
    assert_eq!(now.size, 33);
    assert_eq!(read(&mut a, &target), "version two, from another writer\n");

    // With a fresh baseline the same save goes through, keeping the permission bits and owner.
    let fresh = a.stat(&target).unwrap();
    let saved = a.write_atomic(&copy, &target, fresh.fingerprint()).unwrap();
    let WriteOutcome::Written { now, notes } = saved else {
        panic!("expected Written, got {saved:?}")
    };
    assert!(notes.is_empty(), "{notes:?}");
    assert_eq!(now.mode, 0o640);
    assert_eq!((now.uid, now.gid), (fresh.uid, fresh.gid));
    println!("fresh write_atomic -> Written, mode {:o}", now.mode);

    let left = temps(&mut a, &dir);
    assert!(left.is_empty(), "temp files left: {left:?}");
    println!("no temp files left in {dir}");
    a.remove(&target).unwrap();
}

/// W2: a rewrite with the same size inside the same second has the same `(size, mtime)` over
/// SFTP v3 (whole seconds); only the content hash catches it.
#[test]
#[ignore = "needs SOPEN_TEST_SFTP_HOST=<ssh alias> and an existing SOPEN_TEST_SFTP_DIR"]
fn sftp_same_size_same_second_rewrite_is_a_conflict() {
    let Some(live) = live() else { return };
    let local = tempfile::tempdir().unwrap();
    let mut a = live.connect();
    let mut b = live.connect();
    let dir = a.realpath(&live.dir).unwrap();
    let base = local_file(local.path(), "base", "aaaa\n");
    let other = local_file(local.path(), "other", "bbbb\n");
    let copy = local.path().join("copy");

    let mut tries = 0;
    let (target, at) = loop {
        tries += 1;
        assert!(tries <= 20, "never landed two writes in the same second");
        let name = a
            .write_new_sibling(&base, &dir, &format!("w2-{}.txt", random_hex()))
            .unwrap();
        let target = join(&dir, &name);
        let at = a.fetch(&target, &copy, &mut |_, _| {}).unwrap();
        let b_at = b.stat(&target).unwrap();
        let WriteOutcome::Written { now, .. } =
            b.write_atomic(&other, &target, b_at.fingerprint()).unwrap()
        else {
            panic!("second writer refused")
        };
        if now.mtime == at.mtime && now.size == at.size {
            break (target, at);
        }
        a.remove(&target).unwrap();
    };
    println!("same size and second after {tries} attempt(s): {target}");

    std::fs::write(&copy, "mine\n").unwrap();
    let outcome = a.write_atomic(&copy, &target, at.fingerprint()).unwrap();
    println!("write_atomic over the same-second rewrite -> {outcome:?}");
    let WriteOutcome::Conflict { reason, notes, .. } = outcome else {
        panic!("expected Conflict, got {outcome:?}")
    };
    assert_eq!(
        reason,
        "its content changed (same size and modification time)"
    );
    assert!(notes.is_empty(), "{notes:?}");
    assert_eq!(read(&mut a, &target), "bbbb\n");
    let left = temps(&mut a, &dir);
    assert!(left.is_empty(), "temp files left: {left:?}");
    println!("remote kept the other writer's bytes; no temp files left");
    a.remove(&target).unwrap();
}

/// W1: publishing a sibling never replaces anything, also with two connections racing for the
/// same name.
#[test]
#[ignore = "needs SOPEN_TEST_SFTP_HOST=<ssh alias> and an existing SOPEN_TEST_SFTP_DIR"]
fn sftp_sibling_publish_never_replaces() {
    const EACH: usize = 5;
    let Some(live) = live() else { return };
    let local = tempfile::tempdir().unwrap();
    let mut a = live.connect();
    let dir = a.realpath(&live.dir).unwrap();
    let wanted = format!("w1-{}.cut-1-3.mp4", random_hex());
    let theirs = local_file(local.path(), "theirs", "already here\n");
    assert_eq!(a.write_new_sibling(&theirs, &dir, &wanted).unwrap(), wanted);

    let handles: Vec<_> = (0..2)
        .map(|t| {
            let (spec, state, dir, wanted) = (
                live.spec.clone(),
                live.state.clone(),
                dir.clone(),
                wanted.clone(),
            );
            let local = local.path().to_path_buf();
            std::thread::spawn(move || {
                let mut fs = SftpRemote::connect(&spec, &state).unwrap();
                (0..EACH)
                    .map(|i| {
                        let body = format!("publisher {t} export {i}\n");
                        let src = local_file(&local, &format!("src-{t}-{i}"), &body);
                        (fs.write_new_sibling(&src, &dir, &wanted).unwrap(), body)
                    })
                    .collect::<Vec<_>>()
            })
        })
        .collect();
    let all: Vec<(String, String)> = handles
        .into_iter()
        .flat_map(|h| h.join().unwrap())
        .collect();
    let names: HashSet<&String> = all.iter().map(|(n, _)| n).collect();
    assert_eq!(names.len(), 2 * EACH, "a name was published twice: {all:?}");
    assert!(!names.contains(&wanted));
    for (name, body) in &all {
        assert_eq!(&read(&mut a, &join(&dir, name)), body, "{name}");
    }
    assert_eq!(read(&mut a, &join(&dir, &wanted)), "already here\n");
    println!(
        "2 connections x {EACH} publishes of {wanted}: {} distinct names, original intact",
        names.len()
    );
    let left = temps(&mut a, &dir);
    assert!(left.is_empty(), "temp files left: {left:?}");
    println!("no temp files left in {dir}");
    for name in names {
        a.remove(&join(&dir, name)).unwrap();
    }
    a.remove(&join(&dir, &wanted)).unwrap();
}
