//! The image edits as the journal keeps them (07 §3 `edits`): each [`Edit`]'s line reads back
//! as the same edit and replays to the same pixels, and [`Doc::journal_edits`] follows undo,
//! redo and Save.

use super::edit_tests::{bytes_of, clipboard_picture};
use super::*;

/// Every kind of edit's journal line reads back as the same edit — a text with commas, a line
/// break and a `%`, a fractional text size, a pasted picture by its hash — and replaying the
/// lines on the decoded file gives the same pixels as the edits did.
#[test]
fn every_edit_line_reads_back_and_replays_to_the_same_pixels() {
    let original = bytes_of(ImageFormat::Png);
    let mut doc = Doc::decode(&original).unwrap();
    let picture = rgba_image(&clipboard_picture());
    let red = [200, 10, 10, 255];
    doc.paste_image(Some([4, 4, 30, 20]), &picture, clip::Fit::Fit, red)
        .unwrap();
    doc.paste_image(None, &picture, clip::Fit::Stretch, red)
        .unwrap();
    doc.paste_text(
        Some([40, 10, 60, 30]),
        "a, b\n100% 한",
        [0, 0, 0, 255],
        None,
    )
    .unwrap();
    doc.paste_text(None, "top", [9, 8, 7, 255], None).unwrap();
    doc.pixelize([0, 0, 50, 50], 7).unwrap();
    doc.cut(Some([60, 40, 20, 20]), [1, 2, 3, 0]).unwrap();
    doc.crop([2, 3, 100, 70]).unwrap();
    let ops = doc.journal_edits().unwrap().to_vec();
    assert_eq!(ops.len(), 7);
    let pngs: Vec<journal::Paste> = ops
        .iter()
        .filter_map(|op| match op {
            Edit::Paste { picture, .. } => Some(picture.paste().unwrap().clone()),
            _ => None,
        })
        .collect();
    let mut fetch = |sha: &journal::Sha| -> Result<Vec<u8>> {
        pngs.iter()
            .find(|p| p.sha256 == *sha)
            .map(|p| p.png.to_vec())
            .context("no such paste")
    };
    let lines: Vec<String> = ops.iter().map(|op| op.line().unwrap()).collect();
    assert!(lines.iter().all(|l| !l.contains('\n')), "{lines:?}");
    for (op, line) in ops.iter().zip(&lines) {
        assert_eq!(&Edit::parse(line, &mut fetch).unwrap(), op, "{line}");
    }
    assert!(lines[4].starts_with("pixelize 0,0,50,50,7"), "{}", lines[4]);
    let mut again = Doc::decode(&original).unwrap();
    assert_eq!(
        again.replay(&lines.join("\n"), &mut fetch, None).unwrap(),
        7
    );
    assert_eq!(again.img, doc.img, "the same pixels");
    // A line that names a picture the journal does not have replays nothing.
    let mut none = |_: &journal::Sha| -> Result<Vec<u8>> { bail!("missing") };
    let mut third = Doc::decode(&original).unwrap();
    assert!(third.replay(&lines.join("\n"), &mut none, None).is_err());
    assert!(!third.dirty(), "read whole before anything is applied");
    for bad in [
        "crop 1,2,3",
        "warp 1,2,3,4",
        "cut 0,0,1,1,zzzzzzzz",
        "text 0,0,1,1,00000000,px=-1,x",
    ] {
        assert!(Edit::parse(bad, &mut none).is_err(), "{bad}");
    }
}

/// The journal's operations follow undo and redo, start again from the file at a Save, and are
/// gone (`None`) for a picture no operations on the file make: undone past the Save, or edited
/// there.
#[test]
fn the_journals_operations_follow_undo_redo_and_save() {
    let mut doc = Doc::decode(&bytes_of(ImageFormat::Png)).unwrap();
    let names = |doc: &Doc| -> Option<Vec<String>> {
        doc.journal_edits()
            .map(|ops| ops.iter().map(|o| o.line().unwrap()).collect())
    };
    assert_eq!(names(&doc), Some(vec![]));
    doc.crop([0, 0, 100, 70]).unwrap();
    doc.pixelize([0, 0, 10, 10], 2).unwrap();
    doc.undo();
    assert_eq!(names(&doc), Some(vec!["crop 0,0,100,70".to_string()]));
    doc.redo();
    assert_eq!(names(&doc).unwrap().len(), 2);
    doc.undo();
    doc.pixelize([5, 5, 10, 10], 3).unwrap();
    assert_eq!(
        names(&doc),
        Some(vec![
            "crop 0,0,100,70".to_string(),
            "pixelize 5,5,10,10,3".to_string()
        ]),
        "an edit after undo replaces what was undone"
    );
    // Saved: the file holds both now.
    doc.mark_saved(doc.rev());
    assert_eq!(names(&doc), Some(vec![]));
    doc.crop([0, 0, 50, 50]).unwrap();
    assert_eq!(names(&doc), Some(vec!["crop 0,0,50,50".to_string()]));
    doc.undo();
    assert_eq!(names(&doc), Some(vec![]));
    doc.undo();
    assert_eq!(names(&doc), None, "undone past the save");
    doc.redo();
    assert_eq!(names(&doc), Some(vec![]), "back at the file");
    doc.undo();
    doc.pixelize([0, 0, 4, 4], 2).unwrap();
    assert_eq!(names(&doc), None, "an edit made past the save");
    doc.undo();
    doc.redo();
    assert_eq!(names(&doc), None);
    // The next Save starts it again.
    doc.mark_saved(doc.rev());
    doc.crop([0, 0, 10, 10]).unwrap();
    assert_eq!(names(&doc), Some(vec!["crop 0,0,10,10".to_string()]));
}

/// A journal whose later edit does not fit the picture the earlier ones leave replays nothing:
/// every edit is checked before the first is applied.
#[test]
fn a_replay_that_cannot_finish_applies_nothing() {
    let mut doc = Doc::decode(&bytes_of(ImageFormat::Png)).unwrap();
    let before = doc.img.clone();
    let mut none = |_: &journal::Sha| -> Result<Vec<u8>> { bail!("missing") };
    let edits = "pixelize 0,0,20,20,4\ncrop 0,0,30,30\npixelize 20,20,20,20,4";
    let err = doc.replay(edits, &mut none, None).unwrap_err();
    assert!(format!("{err:#}").contains("edit 3 of 3"), "{err:#}");
    assert_eq!(doc.img, before);
    assert!(!doc.dirty() && !doc.can_undo());
}
