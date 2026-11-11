//! Live SFTP check of the conflict-safe write path (spec §5) against a real sshd. Ignored by
//! default — tests never need ssh. Run it with an ssh alias whose remote has the directory:
//!
//! ```sh
//! SOPEN_TEST_SFTP_HOST=<alias> SOPEN_TEST_SFTP_DIR=/tmp/sopen-receipts \
//!   cargo test --test sftp_live -- --ignored --nocapture
//! ```

use sopen::remote::{join, RemoteFs, SftpRemote, WriteOutcome};
use sopen::target::RemoteSpec;

#[test]
#[ignore = "needs SOPEN_TEST_SFTP_HOST=<ssh alias> and an existing SOPEN_TEST_SFTP_DIR"]
fn sftp_write_atomic_refuses_a_changed_remote() {
    let Ok(host) = std::env::var("SOPEN_TEST_SFTP_HOST") else {
        eprintln!("skipping: SOPEN_TEST_SFTP_HOST is not set");
        return;
    };
    let dir = std::env::var("SOPEN_TEST_SFTP_DIR").unwrap_or_else(|_| "/tmp/sopen-receipts".into());
    let state = sopen::cache::dirs().unwrap().state;
    let spec = RemoteSpec {
        user: None,
        host,
        path: dir.clone(),
    };
    let local = tempfile::tempdir().unwrap();
    let v1 = local.path().join("v1");
    std::fs::write(&v1, "version one\n").unwrap();

    // Writer A creates the file (a new sibling, never over an existing one) and fetches it.
    let mut a = SftpRemote::connect(&spec, &state).unwrap();
    let dir = a.realpath(&dir).unwrap();
    let name = a.write_new_sibling(&v1, &dir, "conflict.txt").unwrap();
    let target = join(&dir, &name);
    a.set_mode(&target, 0o640).unwrap();
    let copy = local.path().join("copy");
    let at = a.fetch(&target, &copy, &mut |_, _| {}).unwrap();
    println!("fetched {target}: size={} mode={:o}", at.size, at.mode);

    // Writer B (its own SFTP channel) changes the remote file.
    let mut b = SftpRemote::connect(&spec, &state).unwrap();
    let v2 = local.path().join("v2");
    std::fs::write(&v2, "version two, from another writer\n").unwrap();
    let b_at = b.stat(&target).unwrap();
    let b_out = b.write_atomic(&v2, &target, b_at.fingerprint()).unwrap();
    assert!(matches!(b_out, WriteOutcome::Written { .. }), "{b_out:?}");
    println!("second writer replaced {target}");

    // A's stale save is refused and the remote keeps B's content.
    std::fs::write(&copy, "A's edit made on the stale copy\n").unwrap();
    let outcome = a.write_atomic(&copy, &target, at.fingerprint()).unwrap();
    println!("stale write_atomic -> {outcome:?}");
    let WriteOutcome::Conflict { now: Some(now) } = outcome else {
        panic!("expected Conflict, got {outcome:?}")
    };
    assert_eq!(now.size, 33);
    let check = local.path().join("check");
    a.fetch(&target, &check, &mut |_, _| {}).unwrap();
    assert_eq!(
        std::fs::read_to_string(&check).unwrap(),
        "version two, from another writer\n"
    );

    // With a fresh baseline the same save goes through, keeping the permission bits.
    let fresh = a.stat(&target).unwrap();
    let saved = a.write_atomic(&copy, &target, fresh.fingerprint()).unwrap();
    let WriteOutcome::Written { now } = saved else {
        panic!("expected Written, got {saved:?}")
    };
    assert_eq!(now.mode, 0o640);
    println!("fresh write_atomic -> Written, mode {:o}", now.mode);

    // Exports never overwrite: a second sibling with the same wanted name gets `-N`.
    let again = a.write_new_sibling(&v1, &dir, &name).unwrap();
    assert_ne!(again, name);
    println!("write_new_sibling({name}) again -> {again}");

    let leftovers: Vec<String> = a
        .list_dir(&dir)
        .unwrap()
        .into_iter()
        .filter(|n| n.contains(".sopen-"))
        .collect();
    assert!(leftovers.is_empty(), "temp files left: {leftovers:?}");
    a.remove(&target).unwrap();
    a.remove(&join(&dir, &again)).unwrap();
}
