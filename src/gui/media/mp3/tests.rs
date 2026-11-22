//! W7's two groups of mp3 fixtures (06 §7.4). Reference fixtures: what sopen reads is what
//! ffmpeg itself decides, its `is_cbr` (seen from outside: with `+fastseek`, ffmpeg warns that a
//! seek "may be imprecise" exactly when the file is not constant-bitrate by its `Info` tag).
//! Fallback fixtures: an exact seek, even where ffmpeg recovers.

use super::*;
use crate::ffmpeg::Tools;
use std::path::{Path, PathBuf};
use std::process::Command;

fn tools_or_skip() -> Option<Tools> {
    match crate::ffmpeg::resolve() {
        Ok(t) => Some(t),
        Err(e) if std::env::var_os("SOPEN_REQUIRE_FFMPEG").is_some() => {
            panic!("SOPEN_REQUIRE_FFMPEG is set but {e}")
        }
        Err(e) => {
            eprintln!("skipping the mp3 tag test: {e}");
            None
        }
    }
}

/// 4 s of noise as mp3, with `args` for the encoder and muxer.
fn encode(tools: &Tools, dir: &Path, name: &str, args: &[&str]) -> PathBuf {
    let path = dir.join(name);
    let status = Command::new(&tools.ffmpeg)
        .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-y"])
        .args([
            "-f",
            "lavfi",
            "-i",
            "anoisesrc=seed=3:amplitude=0.3",
            "-t",
            "4",
        ])
        .args(["-c:a", "libmp3lame"])
        .args(args)
        .arg(&path)
        .status()
        .unwrap();
    assert!(status.success(), "encoding {name}");
    path
}

/// ffmpeg's own answer: a fast seek it does not call imprecise.
fn ffmpeg_is_cbr(tools: &Tools, path: &Path) -> bool {
    let out = Command::new(&tools.ffmpeg)
        .args(["-hide_banner", "-nostdin", "-loglevel", "warning"])
        .args(["-fflags", "+fastseek", "-ss", "2", "-i"])
        .arg(path)
        .args(["-t", "0.1", "-f", "null", "-"])
        .output()
        .unwrap();
    let said = String::from_utf8_lossy(&out.stderr);
    // An answer only from a seek that worked.
    assert!(
        out.status.success(),
        "ffmpeg's seek in {}: {said}",
        path.display()
    );
    !said.contains("may be imprecise")
}

fn sopen_says(path: &Path) -> bool {
    info_tag(&mut std::fs::File::open(path).unwrap())
}

/// An ID3v2 tag of `version` holding `size` bytes of padding.
fn tag(version: u8, size: u32, flags: u8) -> Vec<u8> {
    let mut t = b"ID3".to_vec();
    t.extend([version, 0, flags]);
    t.extend((0..4).rev().map(|k| ((size >> (7 * k)) & 0x7f) as u8));
    t.resize(10 + size as usize, 0);
    t
}

/// Where the first frame of an ffmpeg-written file starts: after its one ID3v2 tag.
fn first_frame(bytes: &[u8]) -> usize {
    if &bytes[..3] != b"ID3" {
        return 0;
    }
    let size = bytes[6..10]
        .iter()
        .fold(0usize, |n, b| (n << 7) | *b as usize);
    10 + size
}

fn write(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, bytes).unwrap();
    path
}

#[test]
fn reference_fixtures_agree_with_ffmpeg() {
    let Some(tools) = tools_or_skip() else { return };
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let cbr = ["-b:a", "128k"];
    let info = encode(&tools, dir, "info.mp3", &cbr);
    let bare = encode(
        &tools,
        dir,
        "bare.mp3",
        &["-b:a", "128k", "-id3v2_version", "0"],
    );
    let bare_bytes = std::fs::read(&bare).unwrap();
    let info_bytes = std::fs::read(&info).unwrap();
    let at = first_frame(&info_bytes);
    assert!(
        at > 0 && &info_bytes[..4] == b"ID3\x04",
        "ffmpeg writes an ID3v2.4 tag"
    );
    let mut fixtures: Vec<(&str, PathBuf)> = vec![
        ("Info", info.clone()),
        ("Xing", encode(&tools, dir, "xing.mp3", &["-q:a", "4"])),
        (
            "no tag",
            encode(
                &tools,
                dir,
                "notag.mp3",
                &["-b:a", "128k", "-write_xing", "0"],
            ),
        ),
        (
            "MPEG-1 mono",
            encode(&tools, dir, "m1mono.mp3", &["-b:a", "64k", "-ac", "1"]),
        ),
        (
            "MPEG-2 stereo",
            encode(
                &tools,
                dir,
                "m2st.mp3",
                &["-b:a", "64k", "-ar", "22050", "-ac", "2"],
            ),
        ),
        (
            "MPEG-2 mono",
            encode(
                &tools,
                dir,
                "m2mono.mp3",
                &["-b:a", "32k", "-ar", "22050", "-ac", "1"],
            ),
        ),
    ];
    // A version-4 tag with a footer: the flag set and the footer after the tag.
    let mut footer = info_bytes[..at].to_vec();
    footer[5] |= 0x10;
    footer.extend(b"3DI");
    footer.extend(&info_bytes[3..10]);
    footer[at + 5] |= 0x10;
    footer.extend(&info_bytes[at..]);
    fixtures.push(("v4 tag with a footer", write(dir, "footer.mp3", &footer)));
    // A tag of several MB (as cover art makes), and two tags in a row.
    let mut big = tag(4, 3 << 20, 0);
    big.extend(&bare_bytes);
    fixtures.push(("a tag of 3 MiB", write(dir, "big.mp3", &big)));
    let mut two = tag(3, 300, 0);
    two.extend(tag(4, 500, 0));
    two.extend(&bare_bytes);
    fixtures.push(("two tags", write(dir, "two.mp3", &two)));
    // The first frame marked CRC-protected: its tag is still read 4 + 32 bytes in.
    let mut crc = info_bytes.clone();
    crc[at + 1] &= 0xFE;
    fixtures.push(("a CRC-protected frame", write(dir, "crc.mp3", &crc)));
    let mut seen = Vec::new();
    for (what, path) in &fixtures {
        let (ffmpeg, sopen) = (ffmpeg_is_cbr(&tools, path), sopen_says(path));
        seen.push(format!("{what}: ffmpeg {ffmpeg}, sopen {sopen}"));
        assert_eq!(sopen, ffmpeg, "{what}: ffmpeg says is_cbr={ffmpeg}");
    }
    eprintln!("mp3 reference fixtures: {}", seen.join("; "));
    // Both answers are among them.
    assert!(sopen_says(&info) && !sopen_says(&fixtures[1].1));
}

/// The fallback fixtures, and an `Info` frame cut short before or after its marker, each built
/// from one Info file.
fn fallback_cases(tools: &Tools, dir: &Path) -> (PathBuf, Vec<(&'static str, Vec<u8>)>) {
    let info = encode(tools, dir, "info.mp3", &["-b:a", "128k"]);
    let bytes = std::fs::read(&info).unwrap();
    let at = first_frame(&bytes);
    let mut cases: Vec<(&str, Vec<u8>)> = Vec::new();
    let mut bad = bytes.clone();
    bad[7] |= 0x80;
    cases.push(("an invalid synchsafe byte", bad));
    let mut v2 = bytes.clone();
    v2[3] = 2;
    cases.push(("an unsupported tag version", v2));
    let mut junk = bytes[..at].to_vec();
    junk.extend([0u8; 100]);
    junk.extend(&bytes[at..]);
    cases.push(("junk before the first frame", junk));
    // MPEG-1 stereo: the marker is 4 + 32 bytes in, its 4 bytes after.
    cases.push(("a file cut before the marker", bytes[..at + 20].to_vec()));
    cases.push((
        "a file cut right after the marker",
        bytes[..at + 40].to_vec(),
    ));
    cases.push(("a file cut later in the frame", bytes[..at + 200].to_vec()));
    (info, cases)
}

/// What sopen reads of the fallback fixtures: none of them is an `Info` file.
#[test]
fn fallback_fixtures_are_not_read_as_info() {
    let Some(tools) = tools_or_skip() else { return };
    let tmp = tempfile::tempdir().unwrap();
    let (info, cases) = fallback_cases(&tools, tmp.path());
    assert!(sopen_says(&info), "the base file is Info");
    for (what, case) in cases {
        let path = write(tmp.path(), "case.mp3", &case);
        assert!(!sopen_says(&path), "{what}: an exact seek");
    }
}

/// The fallback fixtures seek exactly: read through a stream on the local `sftp-server` as a
/// window opens one (`seeks_fast`, which `Media::stream` takes), their `play` input has no
/// `+fastseek` — the Info file they are made from does.
#[test]
fn fallback_fixtures_seek_exactly() {
    use crate::ffmpeg::Served;
    use crate::remote::{RemoteFs, SftpRemote};
    use crate::stream::http::{Kind, Server};
    use crate::stream::Source;
    let Some(tools) = tools_or_skip() else { return };
    let tmp = tempfile::tempdir().unwrap();
    let (info, mut cases) = fallback_cases(&tools, tmp.path());
    cases.insert(0, ("the Info file", std::fs::read(&info).unwrap()));
    for (k, (what, case)) in cases.into_iter().enumerate() {
        let path = write(tmp.path(), &format!("case-{k}.mp3"), &case);
        let Some(fs) = SftpRemote::local_server() else {
            return;
        };
        let transport = Box::new(fs).into_ranged(path.to_str().unwrap()).unwrap();
        let no_reopen: crate::stream::Reopen = Box::new(|| anyhow::bail!("no second connection"));
        let source = Source::open(transport, no_reopen, std::sync::Arc::new(|| {})).unwrap();
        let fast = crate::gui::media::seeks_fast(&source, "case.mp3");
        let server = Server::start(source.clone()).unwrap();
        let served = Served::new(server, source, "case.mp3", "case.mp3", fast);
        let options = served.input(Kind::Play).options().to_vec();
        let fastseek = options.iter().any(|o| o == "+fastseek");
        assert_eq!(fastseek, k == 0, "{what}: {options:?}");
    }
}
