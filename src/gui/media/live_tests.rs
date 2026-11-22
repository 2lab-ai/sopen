//! Live receipts of ED6 over a real ssh session (ignored: they need a host). Each prints what it
//! saw; the assertions are the contract's. `SOPEN_TEST_SFTP_HOST=<ssh alias>`,
//! `SOPEN_TEST_SFTP_FILE=<abs path of a video there, in a folder made for receipts>`,
//! `SOPEN_TEST_CLIPS=<START-END[,START-END]>` (source seconds), `--ignored --nocapture`. The
//! inode lines read the file through this machine's own file system, so they are printed only
//! where the host is this machine.

use super::*;
use crate::gui::testkit::{command, Driver};
use crate::remote::{RemoteFs, SftpRemote};
use eframe::egui::Modifiers;
use std::os::unix::fs::MetadataExt;

struct Live {
    host: String,
    abs: String,
    clips: String,
    connect: Box<dyn Fn() -> Result<SftpRemote> + Send + Sync>,
}

fn live() -> Option<Live> {
    let (Ok(host), Ok(abs), Ok(clips)) = (
        std::env::var("SOPEN_TEST_SFTP_HOST"),
        std::env::var("SOPEN_TEST_SFTP_FILE"),
        std::env::var("SOPEN_TEST_CLIPS"),
    ) else {
        eprintln!("skipping: SOPEN_TEST_SFTP_HOST, SOPEN_TEST_SFTP_FILE or SOPEN_TEST_CLIPS unset");
        return None;
    };
    let spec = crate::target::RemoteSpec {
        user: None,
        host: host.clone(),
        path: abs.clone(),
    };
    let state = crate::cache::dirs().unwrap().state;
    Some(Live {
        host,
        abs,
        clips,
        connect: Box::new(move || SftpRemote::connect(&spec, &state)),
    })
}

/// The window on the file read as a stream over its own session, its `Origin` a second one.
fn streamed_window(
    live: &Live,
    cache: &Path,
) -> (MediaWindow, Driver, std::sync::Weak<ffmpeg::Served>) {
    let view: Box<dyn RemoteFs + Send> = Box::new((live.connect)().unwrap());
    let transport = view.into_ranged(&live.abs).map_err(|r| r.error).unwrap();
    let at = transport.opened();
    let source = Source::open(
        transport,
        Box::new(|| anyhow::bail!("no second connection here")),
        Arc::new(|| {}),
    )
    .unwrap();
    let shown = format!("{}:{}", live.host, live.abs);
    let (_, name) = crate::remote::split_parent(&live.abs);
    let media = Media::stream(source, name, &shown).unwrap();
    let served = Arc::downgrade(media.input.served().unwrap());
    let fs: Box<dyn RemoteFs + Send> = Box::new((live.connect)().unwrap());
    let origin = Origin::new(fs, None, Some(live.host.clone()), live.abs.clone(), at);
    let mut win = MediaWindow::new(
        media,
        MediaFile::new(origin, cache),
        crate::cache::exports_root(cache),
        Marks::default(),
        Selection::default(),
        Vec::new(),
    );
    win.sink = Sink::Null;
    win.unplanned = true;
    let mut drv = Driver::new(900.0, 520.0);
    drv.until(&mut win, 120, |w| {
        w.planning.is_none()
            && !w.unplanned
            && w.recovery_started
            && w.recovering.is_none()
            && (w.audio_only() || w.frame_ready())
    });
    (win, drv, served)
}

fn apply_clips(win: &mut MediaWindow, clips: &str) {
    let hooks = Hooks {
        state: crate::gui::State::parse(&format!("clips={clips}")).unwrap(),
        ..Hooks::default()
    };
    let mut timeline = Timeline::new(win.media.rate(), win.media.ticks());
    apply_state(&mut timeline, &win.media, &hooks).unwrap();
    win.timeline = timeline;
}

fn backups(dir: &str, name: &str) -> Vec<String> {
    let prefix = format!(".{name}.sopen-backup-");
    let mut found: Vec<String> = std::fs::read_dir(dir)
        .map(|d| {
            d.flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| n.starts_with(&prefix))
                .collect()
        })
        .unwrap_or_default();
    found.sort();
    found
}

/// E15, E20 over ssh: Save from the stream's URL, the backup link (its inode and size the
/// original's), the result line, the Source and byte server closed after it lands; a second
/// Save makes no new link.
#[test]
#[ignore = "live: needs SOPEN_TEST_SFTP_HOST, SOPEN_TEST_SFTP_FILE and SOPEN_TEST_CLIPS"]
fn live_ed6_a_streamed_save_over_ssh() {
    let Some(live) = live() else { return };
    let (dir, name) = crate::remote::split_parent(&live.abs);
    let before = std::fs::metadata(&live.abs).ok();
    let cache = crate::cache::dirs().unwrap().cache;
    let (mut win, mut drv, served) = streamed_window(&live, &cache);
    apply_clips(&mut win, &live.clips);
    drv.idle(&mut win, 2);
    drv.key(&mut win, Key::S, command(false));
    println!("while rendering: {}", win.status.text);
    drv.until(&mut win, 600, |w| w.save.is_none());
    drv.idle(&mut win, 2);
    println!("status: {}", win.status.text);
    for n in &win.notices {
        println!("row: {n}");
    }
    assert!(win.status.text.starts_with("saved "), "{}", win.status.text);
    let links = backups(dir, name);
    println!("links beside it: {links:?}");
    assert_eq!(links.len(), 1);
    if let (Some(before), Ok(link)) = (
        &before,
        std::fs::metadata(crate::remote::join(dir, &links[0])),
    ) {
        println!(
            "original before: inode {} size {}; link: inode {} size {}",
            before.ino(),
            before.len(),
            link.ino(),
            link.len()
        );
        assert_eq!((before.ino(), before.len()), (link.ino(), link.len()));
    }
    drv.until(&mut win, 30, |_| served.upgrade().is_none());
    println!(
        "byte server and Source closed: {}; now playing {}",
        served.upgrade().is_none(),
        win.media.input.show()
    );
    // A second Save of the session: no new link.
    let length = win.timeline.len();
    let view = win.view();
    win.timeline.set_clips(
        vec![Clip {
            start: 0,
            end: length / 2,
        }],
        view,
    );
    drv.idle(&mut win, 2);
    drv.key(&mut win, Key::F5, Modifiers::NONE);
    drv.until(&mut win, 600, |w| w.save.is_none());
    drv.idle(&mut win, 2);
    println!("second save: {}", win.status.text);
    assert!(win.status.text.starts_with("saved "), "{}", win.status.text);
    let again = backups(dir, name);
    println!("links beside it after the second save: {again:?}");
    assert_eq!(again, links);
}

/// E15 over ssh: the file changes in place while the render reads it — nothing is linked or
/// uploaded, the journal keeps the edits, and the window says why.
#[test]
#[ignore = "live: needs SOPEN_TEST_SFTP_HOST, SOPEN_TEST_SFTP_FILE and SOPEN_TEST_CLIPS"]
fn live_ed6_a_change_mid_render_publishes_nothing() {
    let Some(live) = live() else { return };
    let (dir, name) = crate::remote::split_parent(&live.abs);
    let cache = crate::cache::dirs().unwrap().cache;
    let (mut win, mut drv, _served) = streamed_window(&live, &cache);
    apply_clips(&mut win, &live.clips);
    drv.idle(&mut win, 2);
    let listed = |dir: &str| -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    };
    let names = listed(dir);
    drv.key(&mut win, Key::S, command(false));
    drv.until(&mut win, 600, |w| {
        w.save.is_none()
            || w.save.as_ref().is_some_and(|s| {
                s.progress.load(SeqCst) >= 100
                    && matches!(*s.stage.lock().unwrap(), Some(file::Stage::Rendering(_)))
            })
    });
    assert!(
        win.save.is_some(),
        "the render ended first: {}",
        win.status.text
    );
    use std::os::unix::fs::FileExt;
    let meta = std::fs::metadata(&live.abs).unwrap();
    let f = std::fs::OpenOptions::new()
        .write(true)
        .open(&live.abs)
        .unwrap();
    f.write_all_at(&[0xEE; 4096], meta.len() / 2).unwrap();
    f.set_modified(meta.modified().unwrap() + Duration::from_secs(10))
        .unwrap();
    drop(f);
    let changed = journal::sha256(&std::fs::read(&live.abs).unwrap());
    println!("changed the file in place at 10 % of the render");
    drv.until(&mut win, 600, |w| w.save.is_none());
    drv.until(&mut win, 20, |w| w.schedule.status() == Keep::Kept);
    println!("status: {}", win.status.text);
    for n in &win.notices {
        println!("row: {n}");
    }
    assert_eq!(win.status.text, "save failed");
    assert!(win
        .notices
        .iter()
        .any(|n| n.contains("changed on the remote")));
    assert_eq!(listed(dir), names, "nothing published beside it");
    assert!(backups(dir, name).is_empty());
    assert_eq!(journal::sha256(&std::fs::read(&live.abs).unwrap()), changed);
    let kept = win.kept_line().0;
    println!("journal: {kept}");
    assert!(kept.starts_with("unsaved — kept at "), "{kept}");
    assert!(win.unsaved() && win.editable(), "the edits stay, editable");
}

/// For the recovery rows' screenshot (07 §3, E16): edits kept in a journal of the file, the
/// window then ended as a kill ends it — the journal stays for the next open.
#[test]
#[ignore = "live: needs SOPEN_TEST_SFTP_HOST, SOPEN_TEST_SFTP_FILE and SOPEN_TEST_CLIPS"]
fn live_ed6_leave_a_journal() {
    let Some(live) = live() else { return };
    let cache = crate::cache::dirs().unwrap().cache;
    let (mut win, mut drv, _served) = streamed_window(&live, &cache);
    apply_clips(&mut win, &live.clips);
    drv.until(&mut win, 60, |w| {
        w.journal_text.1 && w.schedule.status() == Keep::Kept
    });
    println!("{}", win.kept_line().0);
    win.crash();
}
