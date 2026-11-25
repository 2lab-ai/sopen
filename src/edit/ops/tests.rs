//! The editor's commands (12 §14 EB-3, EB-4, EB-5's grouping, EB-6–EB-8; ED-ops's carried
//! hazards), and random runs held to a model: documents generated as tokens (characters, line
//! ends, undecodable bytes) whose offsets, columns and decoded cells the model knows from the
//! tokens alone, never from the editor's decode. Fixtures are generated (12 §15).

use super::*;
use crate::edit::arm::{BlockOp, Command, Motion};
use crate::encoding::Encoding::*;

const FFFD: char = char::REPLACEMENT_CHARACTER;

fn open(bytes: &[u8], encoding: Encoding) -> Editor {
    Editor::open(Original::new(bytes.to_vec(), encoding), Doubt::default())
}

fn open_with(bytes: &[u8], encoding: Encoding, doubt: Doubt) -> Editor {
    Editor::open(Original::new(bytes.to_vec(), encoding), doubt)
}

fn bytes_of(ed: &Editor) -> Vec<u8> {
    let mut out = Vec::new();
    ed.buffer().write_to(&mut out).unwrap();
    out
}

fn enc(encoding: Encoding, text: &str) -> Vec<u8> {
    encoding.encode(text).unwrap()
}

#[track_caller]
fn refused(outcome: Outcome) -> String {
    match outcome {
        Some(Said::Refused(line)) => line,
        other => panic!("not refused: {other:?}"),
    }
}

/// The caret at byte `at` (a click there), the block kept.
fn at(ed: &mut Editor, at: u64) {
    ed.place(at, false);
}

/// The block from `from` to `to` (a drag), the caret at `to`.
fn select(ed: &mut Editor, from: u64, to: u64) {
    ed.place(from, false);
    ed.place(to, true);
}

// ---------------------------------------------------------------------------------------------
// EB-3: BOM, line endings, tabs, a doubtful encoding.

#[test]
fn eb3_the_bom_stays_out_of_reach() {
    for e in [Utf8, Utf16Le, Utf32Be] {
        let mut doc = e.bom().to_vec();
        doc.extend(enc(e, "a\r\nb\r\n"));
        let bom = e.bom().len() as u64;
        let mut ed = open(&doc, e);
        assert_eq!(
            ed.caret(),
            bom,
            "{e:?}: the caret's first place is after the BOM"
        );
        ed.move_caret(Motion::Right, false);
        ed.move_caret(Motion::FileStart, false);
        assert_eq!(ed.caret(), bom);
        assert_eq!(ed.delete_left(), None);
        assert_eq!(
            bytes_of(&ed),
            doc,
            "{e:?}: Ctrl+Home, Backspace: no byte changes"
        );
        ed.move_caret(Motion::Left, false);
        assert_eq!(ed.caret(), bom);
        ed.select_all();
        assert_eq!(
            ed.shown(),
            Some(bom..doc.len() as u64),
            "select all starts after it"
        );
        assert_eq!(ed.delete_right(), None);
        assert_eq!(bytes_of(&ed), e.bom(), "{e:?}: only the BOM is left");
    }
}

#[test]
fn eb3_enter_writes_the_ending_of_the_line_it_splits() {
    let enter_at = |doc: &str, e: Encoding, place: &dyn Fn(&mut Editor)| {
        let mut ed = open(&enc(e, doc), e);
        place(&mut ed);
        assert_eq!(ed.enter(), None);
        bytes_of(&ed)
    };
    let end_of_line = |line: usize| {
        move |ed: &mut Editor| {
            ed.move_caret(Motion::FileStart, false);
            for _ in 0..line {
                ed.move_caret(Motion::Down, false);
            }
            ed.move_caret(Motion::End, false);
        }
    };
    // A CRLF line gets CR LF, an LF line LF; the last line, with none, the file's first.
    assert_eq!(
        enter_at("a\r\nb\nc", Utf8, &end_of_line(0)),
        b"a\r\n\r\nb\nc"
    );
    assert_eq!(enter_at("a\r\nb\nc", Utf8, &end_of_line(1)), b"a\r\nb\n\nc");
    assert_eq!(
        enter_at("a\r\nb\nc", Utf8, &end_of_line(2)),
        b"a\r\nb\nc\r\n"
    );
    assert_eq!(enter_at("a\nb", Utf8, &end_of_line(1)), b"a\nb\n");
    // No ending in the file: LF.
    assert_eq!(enter_at("ab", Utf8, &end_of_line(0)), b"ab\n");
    // UTF-16LE: `0D 00 0A 00`.
    assert_eq!(
        enter_at("a\r\nb", Utf16Le, &end_of_line(0)),
        enc(Utf16Le, "a\r\n\r\nb")
    );
    // No final newline is added: typing on a last line with none adds none.
    let mut ed = open(b"a\nb", Utf8);
    ed.move_caret(Motion::FileEnd, false);
    assert_eq!(ed.type_text("x"), None);
    assert_eq!(bytes_of(&ed), b"a\nbx");
    // Indent on: the new line is indented as the line above; off: not.
    for (indent, want) in [(true, &b"  x\n  "[..]), (false, b"  x\n")] {
        let mut ed = open(b"  x", Utf8);
        ed.set_indent(indent);
        ed.move_caret(Motion::End, false);
        assert_eq!(ed.enter(), None);
        assert_eq!(bytes_of(&ed), want, "indent {indent}");
        assert_eq!(ed.caret(), want.len() as u64);
    }
    // Ctrl+N: the line's ending, the caret stays.
    let mut ed = open(b"ab\r\n", Utf8);
    at(&mut ed, 1);
    assert_eq!(ed.insert_line(), None);
    assert_eq!(bytes_of(&ed), b"a\r\nb\r\n");
    assert_eq!(ed.caret(), 1);
}

#[test]
fn eb3_tab_is_a_tab_where_the_file_indents_with_tabs_else_spaces_to_the_next_4() {
    let mut ed = open(b"\tfoo\n\tbar\nx", Utf8);
    ed.move_caret(Motion::FileEnd, false);
    assert_eq!(ed.tab(), None);
    assert_eq!(bytes_of(&ed), b"\tfoo\n\tbar\nx\t");
    let mut ed = open(b"    foo\nx", Utf8);
    ed.move_caret(Motion::FileEnd, false);
    assert_eq!(ed.tab(), None);
    assert_eq!(bytes_of(&ed), b"    foo\nx   ", "column 1 to 4");
    // A wide character takes two columns.
    let mut ed = open(&enc(Utf16Le, "한"), Utf16Le);
    ed.move_caret(Motion::End, false);
    assert_eq!(ed.tab(), None);
    assert_eq!(bytes_of(&ed), enc(Utf16Le, "한  "));
    // Shift+Tab unindents the line: a tab, or up to 4 spaces.
    let mut ed = open(b"      a\n\tb", Utf8);
    assert_eq!(ed.unindent(), None);
    assert_eq!(bytes_of(&ed), b"  a\n\tb");
    ed.move_caret(Motion::Down, false);
    assert_eq!(ed.unindent(), None);
    assert_eq!(bytes_of(&ed), b"  a\nb");
}

#[test]
fn eb3_a_character_latin_1_cannot_hold_is_refused() {
    let mut ed = open(b"x", Latin1);
    ed.move_caret(Motion::End, false);
    assert_eq!(
        refused(ed.type_text("한")),
        "한 cannot be written in Latin-1 — nothing was typed"
    );
    assert_eq!(bytes_of(&ed), b"x");
    assert_eq!(
        refused(ed.paste("aé한b", None)),
        "한 cannot be written in Latin-1 — nothing was pasted"
    );
    assert_eq!(bytes_of(&ed), b"x", "refused whole");
    assert_eq!(ed.paste("aéb", None), None);
    assert_eq!(bytes_of(&ed), b"xa\xE9b");
}

#[test]
fn eb3_a_doubtful_encoding_asks_once_on_cancel() {
    // A BOM-less UTF-16LE file, guessed.
    let doc = enc(Utf16Le, "ab");
    let mut ed = open_with(
        &doc,
        Utf16Le,
        Doubt {
            guessed: true,
            undecodable: 0,
        },
    );
    assert_eq!(
        ed.opening_line("notes.txt").as_deref(),
        Some("read as UTF-16LE (guessed)")
    );
    ed.move_caret(Motion::FileEnd, false);
    assert_eq!(ed.type_text("x"), None, "ASCII: no question");
    let typed = enc(Utf16Le, "abx");
    assert_eq!(bytes_of(&ed), typed);
    let asked = Some(Said::Asked(
        "Type 한 as UTF-16LE? The encoding was guessed".to_string(),
    ));
    assert_eq!(ed.type_text("한"), asked);
    assert_eq!(bytes_of(&ed), typed, "nothing typed while it asks");
    assert!(ed.question().is_some());
    // Enter on the question is Cancel (its focus): nothing typed.
    assert_eq!(ed.answer(Answer::Cancel), None);
    assert_eq!(bytes_of(&ed), typed);
    assert!(ed.question().is_none());
    assert_eq!(ed.type_text("한"), asked, "asked again after Cancel");
    assert_eq!(ed.answer(Answer::Type), None);
    assert_eq!(bytes_of(&ed), enc(Utf16Le, "abx한"));
    assert_eq!(ed.type_text("글"), None, "no second question for the file");
    assert_eq!(bytes_of(&ed), enc(Utf16Le, "abx한글"));
    // A paste asks too.
    let mut ed = open_with(
        &doc,
        Utf16Le,
        Doubt {
            guessed: true,
            undecodable: 0,
        },
    );
    assert!(matches!(ed.paste("x한", None), Some(Said::Asked(_))));
    assert_eq!(ed.answer(Answer::Type), None);
    assert_eq!(bytes_of(&ed), enc(Utf16Le, "x한ab"));

    // UTF-8 holding FF: counted at F4, the line, and the question.
    let doc = b"a\xFFb\xFF\xEF\xBF\xBD";
    let doubt = Doubt::of(&Original::new(doc.to_vec(), Utf8), false);
    assert_eq!(doubt.undecodable, 2, "a real U+FFFD is no undecodable byte");
    let mut ed = open_with(doc, Utf8, doubt);
    assert_eq!(
        ed.opening_line("editor.rs").as_deref(),
        Some("editor.rs holds 2 bytes UTF-8 cannot decode — they stay as they are")
    );
    assert_eq!(
        ed.type_text("한"),
        Some(Said::Asked(
            "Type 한 as UTF-8? The file holds bytes UTF-8 cannot decode".to_string()
        ))
    );
    let clean = open(b"abc", Utf8);
    assert_eq!(clean.opening_line("x"), None);
}

// ---------------------------------------------------------------------------------------------
// EB-4: the caret.

#[test]
fn eb4_the_caret_steps_one_decoded_character() {
    let steps = |ed: &mut Editor, motion: Motion, n: usize| -> Vec<u64> {
        (0..n)
            .map(|_| {
                ed.move_caret(motion, false);
                ed.caret()
            })
            .collect()
    };
    // UTF-16LE: a surrogate pair is 4 bytes; UTF-8's 한 is 3.
    let mut ed = open(&enc(Utf16Le, "a😀b"), Utf16Le);
    assert_eq!(steps(&mut ed, Motion::Right, 4), [2, 6, 8, 8]);
    assert_eq!(steps(&mut ed, Motion::Left, 4), [6, 2, 0, 0]);
    let mut ed = open(&enc(Utf8, "a한b"), Utf8);
    assert_eq!(steps(&mut ed, Motion::Right, 3), [1, 4, 5]);
    let mut ed = open(&enc(Utf32Le, "a한"), Utf32Le);
    assert_eq!(steps(&mut ed, Motion::Right, 2), [4, 8]);
    // 갊 is `0a ac` in UTF-16LE: no line ends there.
    for e in [Utf16Le, Utf16Be] {
        let doc = enc(e, "x갊y\n갊갊 z");
        let mut ed = open(&doc, e);
        assert_eq!(ed.buffer().lines(), 2, "{e:?}");
        ed.move_caret(Motion::End, false);
        assert_eq!(ed.caret(), 6, "{e:?}: End stops before the LF unit");
        ed.move_caret(Motion::Home, false);
        assert_eq!(ed.caret(), 0);
        ed.move_caret(Motion::WordRight, false);
        assert_eq!(
            ed.caret(),
            8,
            "{e:?}: x갊y is one word; the next starts the next line"
        );
        ed.move_caret(Motion::WordRight, false);
        assert_eq!(ed.caret(), 14, "{e:?}: past 갊갊 and the space");
        ed.move_caret(Motion::WordLeft, false);
        assert_eq!(ed.caret(), 8);
        ed.move_caret(Motion::WordLeft, false);
        assert_eq!(ed.caret(), 0);
        ed.move_caret(Motion::Down, false);
        assert_eq!(ed.caret(), 8);
        ed.move_caret(Motion::End, false);
        assert_eq!(ed.caret(), doc.len() as u64);
    }
    // CP949 (VM-4): two bytes a Hangul syllable.
    let doc = enc(Cp949, "한글 ab\n가");
    let mut ed = open(&doc, Cp949);
    assert_eq!(steps(&mut ed, Motion::Right, 4), [2, 4, 5, 6]);
    ed.move_caret(Motion::Home, false);
    ed.move_caret(Motion::WordRight, false);
    assert_eq!(ed.caret(), 5);
    ed.move_caret(Motion::End, false);
    assert_eq!(ed.caret(), 7);
    ed.move_caret(Motion::Right, false);
    assert_eq!(ed.caret(), 8, "past the LF");
    ed.move_caret(Motion::Right, false);
    assert_eq!(ed.caret(), 10);
    // Up keeps the column in cells: 가 (2 columns) under 한글 lands after 한.
    ed.move_caret(Motion::Up, false);
    assert_eq!(ed.caret(), 2);
}

// ---------------------------------------------------------------------------------------------
// EB-5's grouping: the steps the commands make, each restoring the caret and the block.

#[test]
fn eb5_a_word_a_backspace_run_a_paste_and_a_block_move_are_four_steps() {
    let mut ed = open(b"hello world\n", Utf8);
    ed.move_caret(Motion::FileEnd, false);
    let s0 = ed.sel();
    assert_eq!(ed.type_text("a"), None);
    assert_eq!(ed.type_text("b"), None);
    let s1 = ed.sel();
    assert_eq!(ed.delete_left(), None);
    assert_eq!(ed.delete_left(), None);
    let s2 = ed.sel();
    assert_eq!(ed.paste("xy", None), None);
    select(&mut ed, 12, 14);
    ed.move_caret(Motion::FileStart, false);
    let s3 = ed.sel();
    assert_eq!(ed.block(BlockOp::Move), None);
    assert_eq!(bytes_of(&ed), b"xyhello world\n");
    assert_eq!(ed.buffer().steps(), 4);
    for (want, bytes) in [
        (s3, &b"hello world\nxy"[..]),
        (s2, b"hello world\n"),
        (s1, b"hello world\nab"),
        (s0, b"hello world\n"),
    ] {
        assert!(ed.undo());
        assert_eq!(ed.sel(), want);
        assert_eq!(bytes_of(&ed), bytes);
    }
    assert!(!ed.undo());
    for _ in 0..4 {
        assert!(ed.redo());
    }
    assert_eq!(bytes_of(&ed), b"xyhello world\n");
    assert_eq!(ed.shown(), Some(0..2), "the moved block at its new place");
}

/// Overwrite keystrokes are one run, and an Overwrite between a lone CR and an LF keeps both
/// where they were (the buffer's CR LF guard runs on every call of an edit).
#[test]
fn overwrite_is_one_step_and_never_moves_past_a_line_end() {
    let mut ed = open(b"abc\n", Utf8);
    ed.toggle_insert();
    assert!(ed.overwrite());
    for c in ["x", "y", "z"] {
        assert_eq!(ed.type_text(c), None);
    }
    assert_eq!(bytes_of(&ed), b"xyz\n");
    assert_eq!(ed.buffer().steps(), 1);
    // At the line's end Overwrite inserts (E35).
    assert_eq!(ed.type_text("w"), None);
    assert_eq!(bytes_of(&ed), b"xyzw\n");
    assert!(ed.undo());
    assert_eq!(bytes_of(&ed), b"abc\n");
    let mut ed = open(b"\x0Dx\x0A", Utf8);
    ed.toggle_insert();
    ed.move_caret(Motion::Right, false);
    assert_eq!(ed.caret(), 1, "a lone CR is a character");
    assert_eq!(ed.type_text("y"), None);
    assert_eq!(bytes_of(&ed), b"\x0Dy\x0A");
    assert_eq!(ed.caret(), 2);
}

// ---------------------------------------------------------------------------------------------
// EB-6: blocks and the clipboard.

#[test]
fn eb6_a_block_over_an_undecodable_byte_moves_and_pastes_unchanged() {
    let doc = b"a\xFFb\ncd";
    // Ctrl+K V: the byte unchanged at its new place.
    let mut ed = open(doc, Utf8);
    select(&mut ed, 1, 2);
    ed.move_caret(Motion::FileEnd, false);
    assert_eq!(ed.block(BlockOp::Move), None);
    assert_eq!(bytes_of(&ed), b"ab\ncd\xFF");
    assert_eq!(ed.shown(), Some(5..6));
    // Ctrl+K C copies it.
    let mut ed = open(doc, Utf8);
    select(&mut ed, 1, 2);
    ed.move_caret(Motion::FileEnd, false);
    assert_eq!(ed.block(BlockOp::Copy), None);
    assert_eq!(bytes_of(&ed), b"a\xFFb\ncd\xFF");
    assert_eq!(ed.shown(), Some(6..7), "the copy is the block");
    // Cut, then pasted elsewhere: the FF byte there, not EF BF BD.
    let mut ed = open(doc, Utf8);
    select(&mut ed, 1, 2);
    let (clip, said) = ed.cut();
    assert_eq!(said, None);
    let clip = clip.expect("a block was cut");
    assert_eq!(clip.text(), "\u{FFFD}");
    assert_eq!(bytes_of(&ed), b"ab\ncd");
    ed.move_caret(Motion::FileEnd, false);
    assert_eq!(ed.paste(clip.text(), Some(&clip)), None);
    assert_eq!(bytes_of(&ed), b"ab\ncd\xFF");
    // Any other paste holding U+FFFD: refused whole, with the line.
    let line =
        "the paste holds �, which stood for bytes that could not be decoded — nothing was pasted";
    assert_eq!(refused(ed.paste("x\u{FFFD}", Some(&clip))), line);
    assert_eq!(bytes_of(&ed), b"ab\ncd\xFF");
    // The clip in another editor of the same encoding: its pieces are stale there, so its
    // bytes; in another encoding, its text — refused, as it holds U+FFFD.
    let mut other = open(b"z", Utf8);
    other.move_caret(Motion::FileEnd, false);
    assert_eq!(other.paste(clip.text(), Some(&clip)), None);
    assert_eq!(bytes_of(&other), b"z\xFF");
    let mut wide = open(&enc(Utf16Le, "z"), Utf16Le);
    assert_eq!(refused(wide.paste(clip.text(), Some(&clip))), line);
    // A clip of plain text pastes in another encoding as its text.
    let mut ed = open(b"a\r\nb", Utf8);
    ed.select_all();
    let clip = ed.copy().expect("a block");
    assert_eq!(clip.text(), "a\r\nb");
    let mut wide = open(&enc(Utf16Le, "z\n"), Utf16Le);
    assert_eq!(wide.paste(clip.text(), Some(&clip)), None);
    assert_eq!(
        bytes_of(&wide),
        enc(Utf16Le, "a\nbz\n"),
        "line breaks as the line's ending"
    );
}

#[test]
fn eb6_each_block_command() {
    // B and K set the markers.
    let mut ed = open(b"abcdef", Utf8);
    at(&mut ed, 1);
    ed.block(BlockOp::Begin);
    at(&mut ed, 4);
    ed.block(BlockOp::End);
    assert_eq!(ed.shown(), Some(1..4));
    // H hides it: Del is a character again; H again shows it.
    ed.block(BlockOp::Hide);
    assert_eq!(ed.shown(), None);
    assert_eq!(ed.delete_right(), None);
    assert_eq!(bytes_of(&ed), b"abcdf");
    ed.block(BlockOp::Hide);
    assert_eq!(ed.shown(), Some(1..4));
    // Y deletes it.
    assert_eq!(ed.block(BlockOp::Delete), None);
    assert_eq!(bytes_of(&ed), b"af");
    assert_eq!(ed.shown(), None);
    // L marks the line, its ending with it; T the word at the caret.
    let mut ed = open(b"foo bar\r\nbaz", Utf8);
    at(&mut ed, 5);
    ed.block(BlockOp::MarkLine);
    assert_eq!(ed.shown(), Some(0..9));
    ed.block(BlockOp::MarkWord);
    assert_eq!(ed.shown(), Some(4..7));
    // I and U indent and unindent the block's lines (spaces here: no line starts with a tab).
    let mut ed = open(b"a\nb\n\nc", Utf8);
    ed.select_all();
    assert_eq!(ed.block(BlockOp::Indent), None);
    assert_eq!(bytes_of(&ed), b"    a\n    b\n\n    c");
    assert_eq!(ed.shown(), Some(0..18), "the block covers its lines");
    assert_eq!(ed.block(BlockOp::Unindent), None);
    assert_eq!(bytes_of(&ed), b"a\nb\n\nc");
    assert_eq!(ed.buffer().steps(), 2, "one step each");
    // Del and Backspace with a block shown delete it; Ctrl+Del too.
    for delete in [
        Editor::delete_right,
        Editor::delete_left,
        Editor::delete_block,
    ] {
        let mut ed = open(b"abcd", Utf8);
        select(&mut ed, 1, 3);
        assert_eq!(delete(&mut ed), None);
        assert_eq!(bytes_of(&ed), b"ad");
        assert_eq!(ed.caret(), 1);
    }
    // Ctrl+K V into the block itself does nothing.
    let mut ed = open(b"abcd", Utf8);
    select(&mut ed, 1, 3);
    at(&mut ed, 2);
    assert_eq!(ed.block(BlockOp::Move), None);
    assert_eq!(bytes_of(&ed), b"abcd");
    // Typing does not replace a shown block (Turbo's persistent blocks).
    assert_eq!(ed.type_text("x"), None);
    assert_eq!(bytes_of(&ed), b"abxcd");
    assert_eq!(ed.shown(), Some(1..4));
}

/// Keys as a source sends them — a character key, then its text unless Ctrl/⌘ is held
/// (`term_input.rs`, egui-winit) — through [`Keys`] and [`Editor::apply`]: what an arming takes
/// is never typed (EB-6, EK-1), what it does not is typed once.
#[test]
fn eb6_the_keys_reach_the_commands_and_an_arming_types_nothing() {
    use crate::edit::arm::{Backend, Keys, Press};
    use eframe::egui::{Key, Modifiers};
    let ctrl = Modifiers {
        ctrl: true,
        command: true,
        ..Modifiers::NONE
    };
    let none = Modifiers::NONE;
    for backend in [Backend::Terminal, Backend::Window] {
        let mut ed = open(b"abcd", Utf8);
        let keys = std::cell::RefCell::new(Keys::new(backend));
        let mut clip = None;
        let mut send = |ed: &mut Editor, key: Key, m: Modifiers, text: Option<&str>| {
            let command = keys.borrow_mut().press(&Press::key(key, m));
            ed.apply(&command, &mut clip);
            if let Some(t) = text.filter(|_| !m.command) {
                let command = keys.borrow_mut().press(&Press::Text(t.to_string()));
                ed.apply(&command, &mut clip);
            }
        };
        send(&mut ed, Key::ArrowRight, none, None);
        send(&mut ed, Key::K, ctrl, None);
        send(&mut ed, Key::B, none, Some("b"));
        send(&mut ed, Key::End, none, None);
        send(&mut ed, Key::K, ctrl, None);
        send(&mut ed, Key::K, none, Some("k"));
        assert_eq!(ed.shown(), Some(1..4), "{backend:?}");
        assert_eq!(bytes_of(&ed), b"abcd", "{backend:?}: b and k not typed");
        send(&mut ed, Key::K, ctrl, None);
        send(&mut ed, Key::Y, ctrl, None);
        assert_eq!(bytes_of(&ed), b"a");
        // Ctrl+K then x: disarmed, nothing typed; then q types once.
        send(&mut ed, Key::K, ctrl, None);
        send(&mut ed, Key::X, none, Some("x"));
        assert_eq!(bytes_of(&ed), b"a", "{backend:?}: x not typed");
        send(&mut ed, Key::Q, none, Some("q"));
        assert_eq!(bytes_of(&ed), b"aq", "{backend:?}");
        // A terminal's ^S, then a word starting with s: cancelled, and none of it typed.
        if backend == Backend::Terminal {
            send(&mut ed, Key::S, ctrl, None);
            assert_eq!(keys.borrow().armed(), crate::edit::arm::Armed::Save);
            send(&mut ed, Key::S, none, Some("s"));
            assert_eq!(bytes_of(&ed), b"aq", "s not typed");
            send(&mut ed, Key::A, none, Some("a"));
            assert_eq!(bytes_of(&ed), b"aqa", "the next key types again");
        }
    }
}

/// Ctrl+N after a lone CR makes a CR LF pair: the caret is not left between them (E35), so
/// what is typed next lands where the caret shows.
#[test]
fn a_line_break_after_a_lone_cr_leaves_the_caret_past_the_pair() {
    let mut ed = open(b"a\rb", Utf8);
    at(&mut ed, 2);
    assert_eq!(ed.insert_line(), None);
    assert_eq!(bytes_of(&ed), b"a\r\nb");
    assert_eq!(ed.caret(), 3);
    assert_eq!(ed.type_text("x"), None);
    assert_eq!(bytes_of(&ed), b"a\r\nxb");
    assert_eq!(ed.caret(), 4);
    // A block that starts with an LF, copied or moved to right after a lone CR: the caret
    // and the block's begin past the pair.
    for op in [BlockOp::Copy, BlockOp::Move] {
        let mut ed = open(b"x\ry\nz", Utf8);
        select(&mut ed, 3, 5);
        at(&mut ed, 2);
        assert_eq!(ed.block(op), None, "{op:?}");
        let caret = ed.caret();
        let doc = bytes_of(&ed);
        assert!(
            !(caret >= 1
                && doc[caret as usize - 1] == b'\r'
                && doc.get(caret as usize) == Some(&b'\n')),
            "{op:?}: the caret between CR and LF in {doc:?} at {caret}"
        );
        let block = ed.shown().expect("a block");
        let b = block.start as usize;
        assert!(
            !(b >= 1 && doc[b - 1] == b'\r' && doc.get(b) == Some(&b'\n')),
            "{op:?}: the block's begin between CR and LF"
        );
    }
}

// ---------------------------------------------------------------------------------------------
// EB-7: Find and Replace.

fn search(needle: &str, letters: &str) -> Search {
    Search {
        needle: needle.to_string(),
        options: Options::parse(letters),
    }
}

#[test]
fn eb7_find_with_turbos_options() {
    let doc = b"Hello hello HELLO\ncat concat cat";
    let mut ed = open(doc, Utf8);
    assert_eq!(ed.find(&search("hello", ""), false), Some(6..11));
    assert_eq!(ed.caret(), 11, "the caret after the match");
    assert_eq!(ed.shown(), Some(6..11), "the match is the block");
    assert_eq!(
        ed.find(&search("hello", ""), true),
        None,
        "case matters without U"
    );
    // U: ignore case.
    let mut ed = open(doc, Utf8);
    let u = search("HELLO", "u");
    assert_eq!(ed.find(&u, false), Some(0..5));
    assert_eq!(ed.find(&u, true), Some(6..11));
    assert_eq!(ed.find(&u, true), Some(12..17));
    // W: whole words.
    let mut ed = open(doc, Utf8);
    let w = search("cat", "w");
    assert_eq!(ed.find(&w, false), Some(18..21));
    assert_eq!(ed.find(&w, true), Some(29..32), "concat is no whole word");
    let mut ed = open(doc, Utf8);
    at(&mut ed, 21);
    assert_eq!(ed.find(&search("cat", ""), false), Some(25..28));
    // B: backward from the caret.
    let mut ed = open(doc, Utf8);
    ed.move_caret(Motion::FileEnd, false);
    let b = search("cat", "b");
    assert_eq!(ed.find(&b, false), Some(29..32));
    assert_eq!(ed.caret(), 29, "backward: the caret at the match's start");
    assert_eq!(ed.find(&b, true), Some(25..28));
    assert_eq!(ed.find(&b, true), Some(18..21));
    assert_eq!(ed.find(&b, true), None);
    // G: the whole file, whatever the caret; Find again goes on from the caret.
    let mut ed = open(doc, Utf8);
    ed.move_caret(Motion::FileEnd, false);
    assert_eq!(ed.find(&search("Hello", ""), false), None);
    let g = search("hello", "gu");
    assert_eq!(ed.find(&g, false), Some(0..5));
    assert_eq!(ed.find(&g, true), Some(6..11));
    // N with G: replace every one, without asking, one undo step.
    let mut ed = open(doc, Utf8);
    let (n, said) = ed.replace_all(&search("cat", "gn"), "dog");
    assert_eq!((n, said), (3, None));
    assert_eq!(bytes_of(&ed), b"Hello hello HELLO\ndog condog dog");
    assert_eq!(ed.buffer().steps(), 1);
    // The letters as Turbo's, in any case, and as 2-set jamo (§5.5).
    let all = Options {
        ignore_case: true,
        whole_words: true,
        backward: true,
        no_ask: true,
        global: true,
    };
    assert_eq!(Options::parse("UWBNG"), all);
    assert_eq!(Options::parse("uwbng"), all);
    assert_eq!(Options::parse("ㅕㅈㅠㅜㅎ"), all);
}

#[test]
fn eb7_a_needle_across_a_piece_boundary_and_a_line_end_is_found() {
    let mut ed = open(b"abcd", Utf8);
    at(&mut ed, 2);
    ed.type_text("XY");
    at(&mut ed, 0);
    assert_eq!(ed.find(&search("bXYc", ""), false), Some(1..5));
    let mut ed = open(b"ab\r\ncd\nef", Utf8);
    assert_eq!(ed.find(&search("b\ncd", ""), false), Some(1..6));
    at(&mut ed, 0);
    assert_eq!(ed.find(&search("b\r\ncd", ""), false), Some(1..6));
    at(&mut ed, 0);
    assert_eq!(ed.find(&search("d\ne", ""), false), Some(5..8));
    // In UTF-16LE, by decoded characters: 갊's `0a` byte is no line end.
    let mut ed = open(&enc(Utf16Le, "a갊b\nc"), Utf16Le);
    assert_eq!(ed.find(&search("갊b\nc", ""), false), Some(2..10));
}

#[test]
fn eb7_replace_in_utf16le_writes_the_encoded_replacement() {
    let mut ed = open(&enc(Utf16Le, "a cat b cat"), Utf16Le);
    let found = ed.find(&search("cat", ""), false).expect("found");
    assert_eq!(found, 4..10);
    assert_eq!(ed.replace_at(found, "개"), None);
    assert_eq!(bytes_of(&ed), enc(Utf16Le, "a 개 b cat"));
    assert_eq!(ed.caret(), 6, "after the replacement");
    let (n, said) = ed.replace_all(&search("cat", "n"), "😀");
    assert_eq!((n, said), (1, None));
    assert_eq!(bytes_of(&ed), enc(Utf16Le, "a 개 b 😀"));
    // A replacement the encoding cannot hold is refused, nothing replaced.
    let mut ed = open(b"a cat", Latin1);
    let found = ed.find(&search("cat", ""), false).expect("found");
    assert_eq!(
        refused(ed.replace_at(found, "한")),
        "한 cannot be written in Latin-1 — nothing was replaced"
    );
    assert_eq!(bytes_of(&ed), b"a cat");
}

// ---------------------------------------------------------------------------------------------
// EB-8: a CR LF pair is one (E35).

#[test]
fn eb8_cr_lf_is_one_in_utf8_and_utf16le() {
    for e in [Utf8, Utf16Le] {
        let u = e.unit() as u64;
        let doc = enc(e, "a\r\nb");
        let fresh = || open(&doc, e);
        let mut ed = fresh();
        ed.move_caret(Motion::Right, false);
        assert_eq!(ed.caret(), u);
        ed.move_caret(Motion::Right, false);
        assert_eq!(ed.caret(), 3 * u, "{e:?}: → never stops between CR and LF");
        ed.move_caret(Motion::Left, false);
        assert_eq!(ed.caret(), u, "{e:?}: ← neither");
        // Backspace at a line's start, Delete at a line's end: both.
        let mut ed = fresh();
        at(&mut ed, 3 * u);
        assert_eq!(ed.delete_left(), None);
        assert_eq!(bytes_of(&ed), enc(e, "ab"));
        let mut ed = fresh();
        at(&mut ed, u);
        assert_eq!(ed.delete_right(), None);
        assert_eq!(bytes_of(&ed), enc(e, "ab"));
        // Shift+← from a line's start selects both.
        let mut ed = fresh();
        at(&mut ed, 3 * u);
        ed.move_caret(Motion::Left, true);
        assert_eq!(ed.shown(), Some(u..3 * u));
        // Overwrite at a line's end inserts.
        let mut ed = fresh();
        ed.toggle_insert();
        at(&mut ed, u);
        assert_eq!(ed.type_text("x"), None);
        assert_eq!(bytes_of(&ed), enc(e, "ax\r\nb"));
        // Enter in a CR LF line: CR LF.
        let mut ed = fresh();
        at(&mut ed, u);
        assert_eq!(ed.enter(), None);
        assert_eq!(bytes_of(&ed), enc(e, "a\r\n\r\nb"));
        // A click between the CR and the LF is a place after the LF.
        let mut ed = fresh();
        at(&mut ed, 2 * u);
        assert_eq!(ed.caret(), 3 * u);
        // A CR-only file is one line, and Enter writes LF.
        let cr = enc(e, "a\rb\rc");
        let mut ed = open(&cr, e);
        assert_eq!(ed.buffer().lines(), 1);
        ed.move_caret(Motion::End, false);
        assert_eq!(ed.caret(), cr.len() as u64);
        assert_eq!(ed.enter(), None);
        assert_eq!(bytes_of(&ed), enc(e, "a\rb\rc\n"));
    }
    // `61 0D 0A 62`: a block of `a` alone, cut, leaves CR LF `b`.
    let doc = b"\x61\x0D\x0A\x62";
    let mut ed = open(doc, Utf8);
    ed.move_caret(Motion::Right, true);
    assert_eq!(ed.shown(), Some(0..1));
    assert_eq!(ed.cut().1, None);
    assert_eq!(bytes_of(&ed), b"\x0D\x0A\x62");
    // Its end placed between the CR and the LF — by a click, or by Shift+→ stepping there —
    // is after the LF: a cut leaves `62`.
    let mut ed = open(doc, Utf8);
    select(&mut ed, 0, 2);
    assert_eq!(ed.shown(), Some(0..3));
    assert_eq!(ed.cut().1, None);
    assert_eq!(bytes_of(&ed), b"\x62");
    let mut ed = open(doc, Utf8);
    ed.move_caret(Motion::Right, true);
    ed.move_caret(Motion::Right, true);
    assert_eq!(ed.shown(), Some(0..3));
    assert_eq!(ed.cut().1, None);
    assert_eq!(bytes_of(&ed), b"\x62");
}

// ---------------------------------------------------------------------------------------------
// ED-ops's carried hazards (12 §16).

/// Whole units: a 5-byte UTF-16LE file takes nothing past its last whole unit (ED-buf's
/// counterexample: `b` LF at byte 5 would count 3 lines where the encoding sees 2).
#[test]
fn a_5_byte_utf16le_file_takes_nothing_past_its_last_whole_unit() {
    let doc = [0x61, 0x00, 0x62, 0x00, 0x63];
    let mut ed = open(&doc, Utf16Le);
    ed.move_caret(Motion::FileEnd, false);
    assert_eq!(ed.caret(), 5);
    let line =
        "the file ends in part of a UTF-16LE unit, so nothing goes after it — nothing was typed";
    assert_eq!(refused(ed.type_text("b\n")), line);
    assert_eq!(refused(ed.enter()), line, "Enter neither");
    assert!(matches!(ed.paste("b", None), Some(Said::Refused(_))));
    assert_eq!(bytes_of(&ed), doc);
    assert_eq!(ed.buffer().lines(), 1);
    // Before the partial unit, a whole unit goes in.
    ed.move_caret(Motion::Left, false);
    assert_eq!(ed.caret(), 4);
    assert_eq!(ed.type_text("x"), None);
    assert_eq!(bytes_of(&ed), [0x61, 0x00, 0x62, 0x00, 0x78, 0x00, 0x63]);
    // The partial unit, moved inside the text: refused.
    let mut ed = open(&doc, Utf16Le);
    select(&mut ed, 4, 5);
    ed.move_caret(Motion::FileStart, false);
    assert!(matches!(ed.block(BlockOp::Move), Some(Said::Refused(_))));
    assert!(matches!(ed.block(BlockOp::Copy), Some(Said::Refused(_))));
    assert_eq!(bytes_of(&ed), doc);
}

/// A CP949 lead with no trail is a character of its own; text typed after it must not become
/// its trail: `c` typed between `8C` and LF is not 똠.
#[test]
fn a_cp949_lead_with_no_trail_takes_no_typed_trail() {
    let mut ed = open(&[0x8C, 0x0A], Cp949);
    ed.move_caret(Motion::Right, false);
    assert_eq!(ed.caret(), 1, "the lead alone is one character");
    assert_eq!(Cp949.phase(&[0x8C]), Some(true));
    let line = refused(ed.type_text("c"));
    assert!(line.ends_with("— nothing was typed"), "{line}");
    assert_eq!(bytes_of(&ed), [0x8C, 0x0A], "not 8C 63 0A");
    assert!(matches!(ed.paste("한", None), Some(Said::Refused(_))));
    // A byte that is no trail stands on its own: typed.
    assert_eq!(ed.type_text("1"), None);
    assert_eq!(bytes_of(&ed), [0x8C, 0x31, 0x0A]);
    let mut ed = open(&[0x8C, 0x0A], Cp949);
    ed.move_caret(Motion::End, false);
    assert_eq!(ed.enter(), None);
    assert_eq!(bytes_of(&ed), [0x8C, 0x0A, 0x0A]);
}

/// A delete is what the owner asked for: fragments it brings together read as they read — the
/// joined character shows, and the caret goes to its start (decision of 2026-11-25). Typed
/// text next to the same fragments is still refused.
#[test]
fn a_delete_between_undecodable_fragments_is_done_and_the_joined_character_shows() {
    let one = |e: Encoding, bytes: &[u8]| decoded(e, bytes);
    // CP949: the LF between a lead `8C` and `c` — 똠.
    let mut ed = open(&[0x8C, 0x0A, 0x63], Cp949);
    ed.move_caret(Motion::End, false);
    assert_eq!(ed.delete_right(), None);
    assert_eq!(bytes_of(&ed), [0x8C, 0x63]);
    assert_eq!(one(Cp949, &[0x8C, 0x63]), [('똠', 2)]);
    assert_eq!(ed.caret(), 0, "the caret at the joined character's start");
    ed.move_caret(Motion::Right, false);
    assert_eq!(ed.caret(), 2, "one character");
    // UTF-8: `ED` LF `95 9C`, three U+FFFD — Backspace at the second line's start: 한.
    let doc = [0xED, 0x0A, 0x95, 0x9C];
    assert_eq!(
        one(Utf8, &doc),
        [(FFFD, 1), ('\n', 1), (FFFD, 1), (FFFD, 1)]
    );
    let mut ed = open(&doc, Utf8);
    at(&mut ed, 2);
    assert_eq!(ed.delete_left(), None);
    assert_eq!(bytes_of(&ed), [0xED, 0x95, 0x9C]);
    assert_eq!(one(Utf8, &bytes_of(&ed)), [('한', 3)]);
    assert_eq!(ed.caret(), 0);
    assert!(ed.undo());
    assert_eq!(bytes_of(&ed), doc);
    // UTF-16LE: a lone high and a lone low surrogate either side of an LF: a pair.
    let doc = [0x3D, 0xD8, 0x0A, 0x00, 0x00, 0xDE];
    let mut ed = open(&doc, Utf16Le);
    ed.move_caret(Motion::End, false);
    assert_eq!(ed.delete_right(), None);
    assert_eq!(one(Utf16Le, &bytes_of(&ed)), [('😀', 4)]);
    // A block whose removal joins them, cut: done too.
    let mut ed = open(&[0x8C, 0x20, 0x63], Cp949);
    select(&mut ed, 1, 2);
    let (clip, said) = ed.cut();
    assert!(clip.is_some());
    assert_eq!(said, None);
    assert_eq!(bytes_of(&ed), [0x8C, 0x63]);
    // Typed text after the same lead is still refused (`a_cp949_lead_with_no_trail_…`).
    let mut ed = open(&[0x8C, 0x20], Cp949);
    ed.move_caret(Motion::Right, false);
    assert!(matches!(ed.type_text("c"), Some(Said::Refused(_))));
    assert_eq!(bytes_of(&ed), [0x8C, 0x20]);
}

/// U+FEFF at byte 0 of a BOM-less file is that encoding's BOM, which the next read skips.
#[test]
fn u_feff_at_byte_0_of_a_bom_less_file_is_refused() {
    for e in [Utf8, Utf16Le, Utf16Be, Ucs2Le, Ucs2Be, Utf32Le, Utf32Be] {
        let doc = enc(e, "ab");
        let mut ed = open(&doc, e);
        let line = refused(ed.type_text("\u{FEFF}"));
        assert!(line.contains("byte-order mark"), "{e:?}: {line}");
        assert!(line.ends_with("— nothing was typed"), "{line}");
        assert!(matches!(
            ed.paste("\u{FEFF}x", None),
            Some(Said::Refused(_))
        ));
        assert_eq!(bytes_of(&ed), doc, "{e:?}");
        // After the first character it is a character (a zero-width no-break space).
        ed.move_caret(Motion::Right, false);
        assert_eq!(ed.type_text("\u{FEFF}"), None, "{e:?}");
        assert_eq!(bytes_of(&ed), enc(e, "a\u{FEFF}b"));
        // A delete that brings it to byte 0 is done, and says so.
        ed.move_caret(Motion::FileStart, false);
        assert!(
            matches!(ed.delete_right(), Some(Said::Warned(l)) if l.contains("byte-order mark")),
            "{e:?}"
        );
        assert_eq!(bytes_of(&ed), enc(e, "\u{FEFF}b"));
    }
    // In a file with its BOM, nothing reaches byte 0: typed after the BOM, it is text.
    let mut doc = Utf8.bom().to_vec();
    doc.extend(b"ab");
    let mut ed = open(&doc, Utf8);
    assert_eq!(ed.type_text("\u{FEFF}"), None);
}

/// E36's check runs before `Encoding::encode`, in every encoding: a pasted U+FFFD gets E36's
/// line, never `� cannot be written in …`.
#[test]
fn a_pasted_u_fffd_is_refused_before_encoding_in_every_encoding() {
    let line =
        "the paste holds �, which stood for bytes that could not be decoded — nothing was pasted";
    for e in Encoding::ALL {
        let doc = enc(e, "a");
        let mut ed = open(&doc, e);
        assert_eq!(refused(ed.paste("x\u{FFFD}y", None)), line, "{e:?}");
        assert_eq!(bytes_of(&ed), doc);
    }
}

#[test]
fn apply_runs_the_editing_commands() {
    let mut ed = open(b"ab", Utf8);
    let mut clip = None;
    ed.apply(&Command::Move(Motion::End, false), &mut clip);
    ed.apply(&Command::Type("c".to_string()), &mut clip);
    ed.apply(&Command::SelectAll, &mut clip);
    ed.apply(&Command::Copy, &mut clip);
    assert_eq!(clip.as_ref().map(Clip::text), Some("abc"));
    ed.apply(&Command::Move(Motion::FileEnd, false), &mut clip);
    let text = clip.as_ref().map(|c| c.text().to_string()).unwrap();
    ed.apply(&Command::Paste(text), &mut clip);
    assert_eq!(bytes_of(&ed), b"abcabc");
    ed.apply(&Command::Undo { hint: false }, &mut clip);
    assert_eq!(bytes_of(&ed), b"abc");
    // Commands that are not edits are the binding's: nothing here.
    assert_eq!(ed.apply(&Command::Save, &mut clip), None);
    assert_eq!(bytes_of(&ed), b"abc");
}

// ---------------------------------------------------------------------------------------------
// Random runs against a model.

/// A small deterministic generator (splitmix64), as `buffer/tests.rs` has.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

/// One token of a generated document: its bytes, the cells it decodes as, the character the
/// caret and the words see (`'\n'` for a line end), its columns' rule.
#[derive(Debug, Clone)]
struct Tok {
    bytes: Vec<u8>,
    cells: Vec<(char, u8)>,
    ch: char,
    end: bool,
}

fn text_tok(e: Encoding, c: char) -> Tok {
    let bytes = enc(e, &c.to_string());
    Tok {
        cells: vec![(c, bytes.len() as u8)],
        bytes,
        ch: c,
        end: false,
    }
}

fn line_end(e: Encoding, crlf: bool) -> Tok {
    let mut bytes = Vec::new();
    let mut cells = Vec::new();
    let u = e.unit() as u8;
    if crlf {
        bytes.extend(e.cr());
        cells.push(('\r', u));
    }
    bytes.extend(e.lf());
    cells.push(('\n', u));
    Tok {
        bytes,
        cells,
        ch: '\n',
        end: true,
    }
}

/// Bytes the encoding cannot decode, one U+FFFD that joins nothing where the generator puts it.
fn bad_tok(e: Encoding) -> Tok {
    let bytes = match e {
        Utf8 => vec![0xFF],
        Utf16Le => vec![0x00, 0xDC],
        Utf16Be => vec![0xDC, 0x00],
        Utf32Le => vec![0xFF; 4],
        Utf32Be => vec![0xFF; 4],
        // A lead with no trail: the generator puts a line end after it.
        Cp949 => vec![0x8C],
        _ => unreachable!(),
    };
    Tok {
        cells: vec![(FFFD, bytes.len() as u8)],
        bytes,
        ch: FFFD,
        end: false,
    }
}

const MODEL_ENCODINGS: [Encoding; 6] = [Utf8, Utf16Le, Utf16Be, Utf32Le, Utf32Be, Cp949];

fn generate(e: Encoding, rng: &mut Rng) -> Vec<Tok> {
    let mut chars = vec!['a', 'b', 'Z', '_', ' ', '\t', '한', '글'];
    if e != Cp949 {
        chars.push('😀');
    }
    let n = rng.below(40);
    let mut toks = Vec::new();
    while toks.len() < n {
        match rng.below(10) {
            0 => toks.push(line_end(e, false)),
            1 => toks.push(line_end(e, true)),
            2 => {
                toks.push(bad_tok(e));
                if e == Cp949 {
                    toks.push(line_end(e, rng.below(2) == 0));
                }
            }
            _ => toks.push(text_tok(e, chars[rng.below(chars.len())])),
        }
    }
    toks
}

fn doc_of(toks: &[Tok]) -> Vec<u8> {
    toks.iter().flat_map(|t| t.bytes.clone()).collect()
}

/// The model's caret: a token index; its offsets are the tokens' bytes before it.
struct Model<'a> {
    toks: &'a [Tok],
    caret: usize,
    goal: Option<u64>,
    block: Option<(usize, usize)>,
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

impl Model<'_> {
    fn offset(&self, i: usize) -> u64 {
        self.toks[..i].iter().map(|t| t.bytes.len() as u64).sum()
    }

    fn line_start(&self, i: usize) -> usize {
        self.toks[..i]
            .iter()
            .rposition(|t| t.end)
            .map_or(0, |j| j + 1)
    }

    fn line_end(&self, i: usize) -> usize {
        self.toks[i..]
            .iter()
            .position(|t| t.end)
            .map_or(self.toks.len(), |j| i + j)
    }

    fn width(c: char, col: u64) -> u64 {
        match c {
            '\t' => TAB - col % TAB,
            '한' | '글' | '😀' => 2,
            _ => 1,
        }
    }

    fn column(&self, i: usize) -> u64 {
        let mut col = 0;
        for t in &self.toks[self.line_start(i)..i] {
            col += Self::width(t.ch, col);
        }
        col
    }

    fn at_column(&self, start: usize, goal: u64) -> usize {
        let mut col = 0;
        let mut i = start;
        while i < self.toks.len() && !self.toks[i].end {
            let w = Self::width(self.toks[i].ch, col);
            if col + w > goal {
                break;
            }
            col += w;
            i += 1;
        }
        i
    }

    /// The start of the line `delta` lines away, clamped.
    fn line_away(&self, i: usize, delta: i64) -> usize {
        let mut s = self.line_start(i);
        if delta < 0 {
            for _ in 0..-delta {
                if s == 0 {
                    break;
                }
                s = self.line_start(s - 1);
            }
        } else {
            for _ in 0..delta {
                let e = self.line_end(s);
                if e == self.toks.len() {
                    break;
                }
                s = e + 1;
            }
        }
        s
    }

    fn step(&mut self, motion: Motion, extend: bool, page: u64) {
        let i = self.caret;
        let n = self.toks.len();
        let vertical = |m: &mut Model, delta: i64| {
            let column = m.column(i);
            let goal = *m.goal.get_or_insert(column);
            let s = m.line_away(i, delta);
            if s == m.line_start(i) {
                return i;
            }
            m.at_column(s, goal)
        };
        let to = match motion {
            Motion::Left => i.saturating_sub(1),
            Motion::Right => (i + 1).min(n),
            Motion::Home => self.line_start(i),
            Motion::End => self.line_end(i),
            Motion::FileStart => 0,
            Motion::FileEnd => n,
            Motion::WordRight => {
                let mut j = i;
                while j < n && is_word(self.toks[j].ch) {
                    j += 1;
                }
                while j < n && !is_word(self.toks[j].ch) {
                    j += 1;
                }
                j
            }
            Motion::WordLeft => {
                let mut j = i;
                while j > 0 && !is_word(self.toks[j - 1].ch) {
                    j -= 1;
                }
                while j > 0 && is_word(self.toks[j - 1].ch) {
                    j -= 1;
                }
                j
            }
            Motion::Up => vertical(self, -1),
            Motion::Down => vertical(self, 1),
            Motion::PageUp => vertical(self, -(page as i64)),
            Motion::PageDown => vertical(self, page as i64),
        };
        if !matches!(
            motion,
            Motion::Up | Motion::Down | Motion::PageUp | Motion::PageDown
        ) {
            self.goal = None;
        }
        if extend {
            let anchor = match self.block {
                Some((b, e)) if b < e && e == i => b,
                Some((b, e)) if b < e && b == i => e,
                _ => i,
            };
            self.block = Some((anchor.min(to), anchor.max(to)));
        }
        self.caret = to;
    }
}

const MOTIONS: [Motion; 12] = [
    Motion::Left,
    Motion::Right,
    Motion::Up,
    Motion::Down,
    Motion::Home,
    Motion::End,
    Motion::PageUp,
    Motion::PageDown,
    Motion::WordLeft,
    Motion::WordRight,
    Motion::FileStart,
    Motion::FileEnd,
];

#[test]
fn random_caret_moves_match_the_model() {
    let mut rng = Rng(0x00ED_0095);
    for round in 0..400 {
        let e = MODEL_ENCODINGS[round % MODEL_ENCODINGS.len()];
        let toks = generate(e, &mut rng);
        // The model's premise: the document decodes as its tokens say.
        assert_eq!(
            decoded(e, &doc_of(&toks)),
            cells_of(&toks),
            "{e:?} {toks:?}"
        );
        let bom = if e != Cp949 && rng.below(4) == 0 {
            e.bom().to_vec()
        } else {
            Vec::new()
        };
        let mut doc = bom.clone();
        doc.extend(doc_of(&toks));
        let mut ed = open(&doc, e);
        ed.set_page(3);
        let mut model = Model {
            toks: &toks,
            caret: 0,
            goal: None,
            block: None,
        };
        let b = bom.len() as u64;
        for step in 0..60 {
            let motion = MOTIONS[rng.below(MOTIONS.len())];
            let extend = rng.below(3) == 0;
            model.step(motion, extend, 3);
            ed.move_caret(motion, extend);
            let label = format!("{e:?} round {round} step {step} {motion:?} {extend} {toks:?}");
            assert_eq!(ed.caret(), b + model.offset(model.caret), "{label}");
            let want = model
                .block
                .filter(|(s, t)| s < t)
                .map(|(s, t)| b + model.offset(s)..b + model.offset(t));
            assert_eq!(ed.shown(), want, "{label}");
        }
    }
}

/// The decode the tokens promise for the document they would make.
fn cells_of(toks: &[Tok]) -> Vec<(char, u8)> {
    toks.iter().flat_map(|t| t.cells.clone()).collect()
}

fn decoded(e: Encoding, bytes: &[u8]) -> Vec<(char, u8)> {
    e.decode(bytes, true)
        .0
        .iter()
        .map(|c| (c.ch, c.len))
        .collect()
}

#[test]
fn random_block_operations_match_the_model() {
    let mut rng = Rng(0xB10C);
    let mut refusals = 0;
    for round in 0..600 {
        let e = MODEL_ENCODINGS[round % MODEL_ENCODINGS.len()];
        let toks = generate(e, &mut rng);
        let n = toks.len();
        if n == 0 {
            continue;
        }
        let doc = doc_of(&toks);
        let off = |i: usize| -> u64 { toks[..i].iter().map(|t| t.bytes.len() as u64).sum() };
        let (mut i, mut j) = (rng.below(n + 1), rng.below(n + 1));
        if i > j {
            std::mem::swap(&mut i, &mut j);
        }
        if i == j {
            continue;
        }
        let k = rng.below(n + 1);
        let mut ed = open(&doc, e);
        select(&mut ed, off(i), off(j));
        assert_eq!(ed.shown(), Some(off(i)..off(j)));
        at(&mut ed, off(k));
        let block: Vec<Tok> = toks[i..j].to_vec();
        let op = rng.below(4);
        let (said, want): (Outcome, Vec<Tok>) = match op {
            // Ctrl+K C at k.
            0 => {
                let mut want = toks[..k].to_vec();
                want.extend(block.clone());
                want.extend(toks[k..].to_vec());
                (ed.block(BlockOp::Copy), want)
            }
            // Ctrl+K V to k.
            1 => {
                let want = if (i..=j).contains(&k) {
                    toks.clone()
                } else if k < i {
                    [&toks[..k], &block, &toks[k..i], &toks[j..]].concat()
                } else {
                    [&toks[..i], &toks[j..k], &block, &toks[k..]].concat()
                };
                (ed.block(BlockOp::Move), want)
            }
            // Ctrl+K Y.
            2 => (ed.block(BlockOp::Delete), [&toks[..i], &toks[j..]].concat()),
            // Cut, then paste at k (where k is after the cut).
            _ => {
                let (clip, said) = ed.cut();
                if said.is_some() {
                    (said, [&toks[..i], &toks[j..]].concat())
                } else {
                    let clip = clip.expect("cut");
                    let rest = [&toks[..i], &toks[j..]].concat();
                    let kk = if k <= i {
                        k
                    } else if k >= j {
                        k - (j - i)
                    } else {
                        i
                    };
                    let kk_off: u64 = rest[..kk].iter().map(|t| t.bytes.len() as u64).sum();
                    assert_eq!(bytes_of(&ed), doc_of(&rest), "round {round}: the cut");
                    at(&mut ed, kk_off);
                    if ed.caret() != kk_off {
                        // The cut joined the fragments around it into one character, so that
                        // place is no character start any more: the caret went to its start.
                        assert!(ed.caret() < kk_off, "round {round}");
                        continue;
                    }
                    let mut want = rest[..kk].to_vec();
                    want.extend(block.clone());
                    want.extend(rest[kk..].to_vec());
                    // A paste is refused exactly when a character would cross its seams: when
                    // the document's decode is not its three parts' decodes end to end.
                    let parts = [
                        decoded(e, &doc_of(&rest[..kk])),
                        decoded(e, &doc_of(&block)),
                        decoded(e, &doc_of(&rest[kk..])),
                    ]
                    .concat();
                    let joins = decoded(e, &doc_of(&want)) != parts;
                    let said = ed.paste(clip.text(), Some(&clip));
                    let label = format!("{e:?} round {round} paste at {kk}: {want:?}");
                    if matches!(said, Some(Said::Refused(_))) {
                        assert!(joins, "{label}: refused, yet nothing would join");
                        assert_eq!(bytes_of(&ed), doc_of(&rest), "{label}");
                        refusals += 1;
                        continue;
                    }
                    assert!(!joins, "{label}: pasted, yet it joined the bytes beside it");
                    (said, want)
                }
            }
        };
        let label = format!("{e:?} round {round} op {op} {i}..{j} at {k}: {toks:?}");
        let want_bytes = doc_of(&want);
        // The block commands bring the file's own bytes together: never refused, whatever they
        // then read as (decision of 2026-11-25).
        assert!(!matches!(said, Some(Said::Refused(_))), "{label}: {said:?}");
        assert_eq!(bytes_of(&ed), want_bytes, "{label}");
        assert_eq!(
            ed.caret(),
            ed.doc().place(ed.caret()),
            "{label}: the caret on a character start"
        );
        // Undo gives back the document as it was before the operation; redo, after it.
        let steps = ed.buffer().steps();
        for _ in 0..steps {
            assert!(ed.undo(), "{label}");
        }
        assert_eq!(bytes_of(&ed), doc, "{label}: undone");
        for _ in 0..steps {
            assert!(ed.redo(), "{label}");
        }
        assert_eq!(bytes_of(&ed), want_bytes, "{label}: redone");
    }
    assert!(refusals > 0, "the runs met a join");
}

/// Lines longer than the look-back window and the decode chunk: ← and → step where the tokens
/// say along the whole line (a start found by the encoding's rules, or from the line's start in
/// CP949 Hangul with no sync byte), a needle across a chunk's end is found forward and back,
/// and the read's undecodable count crosses chunks.
#[test]
fn long_lines_step_find_and_count_across_windows_and_chunks() {
    let mut rng = Rng(0x4C4F_4E47);
    for e in MODEL_ENCODINGS {
        let mut chars = vec!['a', ' ', '한'];
        if e != Cp949 {
            chars.push('😀');
        }
        let mut toks: Vec<Tok> = (0..3000)
            .map(|_| text_tok(e, chars[rng.below(chars.len())]))
            .collect();
        if e == Cp949 {
            // A run of Hangul longer than CP949's look-back W: no sync byte proves a start.
            toks.extend((0..400).map(|_| text_tok(e, '한')));
        }
        let doc = doc_of(&toks);
        let mut ed = open(&doc, e);
        let mut off = 0;
        for (i, t) in toks.iter().enumerate() {
            off += t.bytes.len() as u64;
            ed.move_caret(Motion::Right, false);
            assert_eq!(ed.caret(), off, "{e:?} → {i}");
        }
        for (i, t) in toks.iter().enumerate().rev() {
            off -= t.bytes.len() as u64;
            ed.move_caret(Motion::Left, false);
            assert_eq!(ed.caret(), off, "{e:?} ← {i}");
        }
        // Backspace from the end takes one character's bytes.
        ed.move_caret(Motion::End, false);
        assert_eq!(ed.delete_left(), None);
        assert_eq!(
            bytes_of(&ed),
            doc[..doc.len() - toks.last().unwrap().bytes.len()]
        );
    }
    // Find: a line past two decode chunks, the needle across the first chunk's end.
    let mut line = "ab ".repeat(CHUNK as usize / 3 + 10);
    let at = CHUNK as usize - 2;
    line.replace_range(at..at + 4, "XYZW");
    line.push_str(" tail");
    for e in [Utf8, Utf16Le] {
        let doc = enc(e, &line);
        let u = e.unit() as u64;
        let mut ed = open(&doc, e);
        let want = at as u64 * u..(at as u64 + 4) * u;
        assert_eq!(
            ed.find(&search("XYZW", ""), false),
            Some(want.clone()),
            "{e:?}"
        );
        ed.move_caret(Motion::FileEnd, false);
        assert_eq!(
            ed.find(&search("xyzw", "bu"), false),
            Some(want),
            "{e:?} back"
        );
        let before = ed.find(&search("ab", "b"), true).expect("an ab before it");
        assert!(before.end <= at as u64 * u, "{e:?}: {before:?}");
    }
    // The read's count: undecodable bytes on both sides of a chunk's end, a character across it.
    let mut doc = vec![b'a'; CHUNK as usize - 1];
    doc.extend("한".as_bytes());
    doc.push(0xFF);
    doc.extend(vec![b'b'; 100]);
    doc.push(0xFF);
    let doubt = Doubt::of(&Original::new(doc, Utf8), false);
    assert_eq!(doubt.undecodable, 2);
}
