//! Routing (spec §3): directory by stat, image/video/audio by extension, text by extension or a
//! sniff of the first 8 KiB (UTF-8 with no NUL, or UTF-16/32 by 08 D18), else "other". `.ts` is
//! both TypeScript and MPEG-TS: text by the sniff, else video when it looks like a transport
//! stream (ffprobe has the last word once the file is local).

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Directory,
    Text,
    Image,
    Video,
    Audio,
    Other,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Directory => "dir",
            Kind::Text => "text",
            Kind::Image => "image",
            Kind::Video => "video",
            Kind::Audio => "audio",
            Kind::Other => "other",
        }
    }
}

/// Bytes read for the text sniff.
pub const SNIFF_BYTES: usize = 8 * 1024;

const IMAGE: &[&str] = &["png", "jpg", "jpeg", "webp", "bmp", "gif", "tif", "tiff"];
const VIDEO: &[&str] = &["mp4", "mov", "m4v", "mkv", "webm", "avi"];
const AUDIO: &[&str] = &["mp3", "wav", "flac", "m4a", "aac", "ogg", "opus"];
/// Known text without sniffing (a sniff still catches anything missing here).
const TEXT: &[&str] = &[
    "txt",
    "md",
    "markdown",
    "rst",
    "adoc",
    "log",
    "csv",
    "tsv",
    "json",
    "jsonl",
    "ndjson",
    "yaml",
    "yml",
    "toml",
    "ini",
    "cfg",
    "conf",
    "env",
    "xml",
    "html",
    "htm",
    "css",
    "scss",
    "sass",
    "less",
    "svg",
    "js",
    "mjs",
    "cjs",
    "jsx",
    "tsx",
    "vue",
    "svelte",
    "rs",
    "go",
    "py",
    "rb",
    "php",
    "pl",
    "lua",
    "java",
    "kt",
    "kts",
    "scala",
    "swift",
    "m",
    "mm",
    "c",
    "h",
    "cc",
    "cpp",
    "cxx",
    "hpp",
    "hh",
    "cs",
    "fs",
    "zig",
    "nim",
    "ex",
    "exs",
    "erl",
    "hs",
    "ml",
    "clj",
    "el",
    "vim",
    "sh",
    "bash",
    "zsh",
    "fish",
    "ps1",
    "bat",
    "sql",
    "graphql",
    "proto",
    "tf",
    "hcl",
    "nix",
    "dockerfile",
    "mk",
    "cmake",
    "gradle",
    "properties",
    "lock",
    "diff",
    "patch",
    "tex",
    "bib",
    "srt",
    "vtt",
];

/// Lower-cased extension of a file name (`None` for no dot or a leading-dot-only name).
pub fn extension(name: &str) -> Option<String> {
    let base = name.rsplit('/').next().unwrap_or(name);
    match base.rfind('.') {
        Some(i) if i > 0 && i + 1 < base.len() => Some(base[i + 1..].to_ascii_lowercase()),
        _ => None,
    }
}

/// The kind an extension alone decides, or `None` when the content must be sniffed.
pub fn by_extension(name: &str) -> Option<Kind> {
    let ext = extension(name)?;
    let ext = ext.as_str();
    if IMAGE.contains(&ext) {
        Some(Kind::Image)
    } else if VIDEO.contains(&ext) {
        Some(Kind::Video)
    } else if AUDIO.contains(&ext) {
        Some(Kind::Audio)
    } else if TEXT.contains(&ext) {
        Some(Kind::Text)
    } else {
        None
    }
}

/// The sniff's UTF-8 test: no NUL and valid UTF-8. `truncated` = the buffer stops before the end
/// of the file, so a multi-byte character cut at the boundary does not count against it. A
/// page's `charset=utf-8` is decided by this (09 §4), never by [`looks_like_text`], which takes
/// UTF-16 and UTF-32 too.
pub fn utf8_text(head: &[u8], truncated: bool) -> bool {
    !head.contains(&0)
        && match std::str::from_utf8(head) {
            Ok(_) => true,
            Err(e) => truncated && e.error_len().is_none(),
        }
}

/// Signatures of binary formats, each at its offset, whose fields and samples the NUL pattern
/// could take for UTF-16 / UTF-32 text (a WAV of quiet 16-bit samples is all ASCII units): a
/// file that starts with one is that binary, and the pattern is not asked about it (D18). The
/// short ones that text can start with, `MZ` and `ID3`, are [`pe_executable`] and [`id3_tag`].
const SIGNATURES: &[(usize, &[u8])] = &[
    // RIFF (WAV, AVI, WebP), FLAC, Ogg.
    (0, b"RIFF"),
    (0, b"fLaC"),
    (0, b"OggS"),
    // PNG, GIF, JPEG, PDF, ZIP (and what is a zip: docx, jar, …), SQLite.
    (0, b"\x89PNG"),
    (0, b"GIF8"),
    (0, b"\xFF\xD8\xFF"),
    (0, b"%PDF"),
    (0, b"PK\x03\x04"),
    (0, b"SQLite format 3\0"),
    // Executables: ELF, Mach-O (32 and 64 bits, either order) and a universal binary.
    (0, b"\x7FELF"),
    (0, b"\xFE\xED\xFA\xCE"),
    (0, b"\xFE\xED\xFA\xCF"),
    (0, b"\xCE\xFA\xED\xFE"),
    (0, b"\xCF\xFA\xED\xFE"),
    (0, b"\xCA\xFE\xBA\xBE"),
    // Fonts: TrueType, OpenType (CFF), WOFF, WOFF2.
    (0, b"\x00\x01\x00\x00"),
    (0, b"OTTO"),
    (0, b"wOFF"),
    (0, b"wOF2"),
    // A Finder `.DS_Store`, OLE (old Office, MSI), a Windows registry hive.
    (0, b"\x00\x00\x00\x01Bud1"),
    (0, b"\xD0\xCF\x11\xE0"),
    (0, b"regf"),
    // ISO base media (MP4, MOV, M4A, HEIC, …): the first box is `ftyp`.
    (4, b"ftyp"),
];

/// A DOS / PE executable: `MZ`, which UTF-16LE text starts with too (U+5A4D), so only with the
/// PE header where `e_lfanew` (the u32 at 0x3C) points, inside the head, or the DOS stub's
/// message.
fn pe_executable(head: &[u8]) -> bool {
    const STUB: &[u8] = b"This program cannot be run in DOS mode";
    if !head.starts_with(b"MZ") || head.len() < 64 {
        return false;
    }
    let at = u32::from_le_bytes([head[0x3C], head[0x3D], head[0x3E], head[0x3F]]) as usize;
    let pe = at.checked_add(4).and_then(|end| head.get(at..end)) == Some(b"PE\0\0");
    pe || head.windows(STUB.len()).any(|w| w == STUB)
}

/// An MP3's ID3v2 tag: `ID3`, which UTF-16LE text starts with too (U+4449), so only with its
/// version 2–4 and the flags' low four bits clear.
fn id3_tag(head: &[u8]) -> bool {
    head.len() >= 6 && head.starts_with(b"ID3") && (2..=4).contains(&head[3]) && head[5] & 0x0F == 0
}

/// Whether `head` starts as a binary format of [`SIGNATURES`] does, or is a PE or an ID3 tag.
fn signed_binary(head: &[u8]) -> bool {
    SIGNATURES
        .iter()
        .any(|&(at, sig)| head.get(at..at + sig.len()) == Some(sig))
        || pe_executable(head)
        || id3_tag(head)
}

/// [`utf8_text`], or UTF-16 / UTF-32 by the text view's default (08 D18: §7.2's step 1, a BOM,
/// or step 2, the NUL pattern) — [`crate::encoding::pick`] itself, so a file routed to text this
/// way is decoded by the encoding that routed it (D27). The pattern's guess is not taken for a
/// file with a binary format's signature ([`SIGNATURES`]); a BOM decides before any (D37). The
/// default's other answers do not route: a CP949 guess (step 3) or a UTF-8 BOM before bytes that
/// are not UTF-8 stay "other".
pub fn looks_like_text(head: &[u8], truncated: bool) -> bool {
    use crate::encoding::Encoding::{Utf16Be, Utf16Le, Utf32Be, Utf32Le};
    if utf8_text(head, truncated) {
        return true;
    }
    let picked = crate::encoding::pick(head, truncated);
    let wide = matches!(picked.encoding, Utf16Le | Utf16Be | Utf32Le | Utf32Be);
    wide && !(picked.guessed && signed_binary(head))
}

/// Extensions shared by text and video (`.ts`: TypeScript or MPEG-TS); the content decides.
const TEXT_OR_VIDEO: &[&str] = &["ts"];

/// `.ts` and friends: video when ffprobe sees a video stream, else whatever the sniff says.
pub fn maybe_video(name: &str) -> bool {
    extension(name).is_some_and(|e| TEXT_OR_VIDEO.contains(&e.as_str()))
}

/// MPEG transport stream: a 0x47 sync byte every 188 bytes.
fn looks_like_transport_stream(head: &[u8]) -> bool {
    !head.is_empty() && head.iter().step_by(188).take(4).all(|&b| b == 0x47)
}

/// Full routing for a non-directory: extension first, then the sniff (`head` = first bytes,
/// `size` = file size, to know whether `head` is the whole file).
pub fn classify(name: &str, head: impl FnOnce() -> Vec<u8>, size: u64) -> Kind {
    if let Some(kind) = by_extension(name) {
        return kind;
    }
    let head = head();
    if looks_like_text(&head, (head.len() as u64) < size) {
        Kind::Text
    } else if maybe_video(name) && looks_like_transport_stream(&head) {
        Kind::Video
    } else {
        Kind::Other
    }
}

/// A "video" file whose probe shows no video stream is audio (spec §3).
pub fn refine_with_probe(kind: Kind, has_video_stream: bool) -> Kind {
    match kind {
        Kind::Video if !has_video_stream => Kind::Audio,
        k => k,
    }
}

/// The kind once the file is local: `probe` = `Some((has_video, has_audio))` from ffprobe, or
/// `None` when ffprobe could not read it. A `.ts` with neither stream is not media.
pub fn refine_after_fetch(name: &str, kind: Kind, probe: Option<(bool, bool)>) -> Kind {
    if kind != Kind::Video {
        return kind;
    }
    match probe {
        Some((false, false)) | None if maybe_video(name) => Kind::Other,
        Some((has_video, _)) => refine_with_probe(kind, has_video),
        None => kind,
    }
}

/// The kind of the file or folder at `abs` (`stat` is its stat): the folder by its stat, then
/// the extension, then a sniff of its first bytes (one read).
pub fn detect(
    fs: &mut dyn crate::remote::RemoteFs,
    abs: &str,
    stat: &crate::remote::Stat,
) -> anyhow::Result<Kind> {
    detect_sniffed(fs, abs, stat).map(|(kind, _)| kind)
}

/// [`detect`], with the bytes its sniff read (0 when the stat or the extension decided): what
/// a stream's stats line counts apart as `sniff` (06 §11).
pub fn detect_sniffed(
    fs: &mut dyn crate::remote::RemoteFs,
    abs: &str,
    stat: &crate::remote::Stat,
) -> anyhow::Result<(Kind, u64)> {
    if stat.is_dir {
        return Ok((Kind::Directory, 0));
    }
    let (_, name) = crate::remote::split_parent(abs);
    if let Some(kind) = by_extension(name) {
        return Ok((kind, 0));
    }
    let head = fs.read_head(abs, SNIFF_BYTES)?;
    let sniffed = head.len() as u64;
    Ok((classify(name, || head, stat.size), sniffed))
}

/// The kind once the file is local at `local` (`name` = its name): video by extension but with
/// no video stream (per ffprobe) is audio; a `.ts` ffprobe cannot read as media is not media.
/// Without ffprobe, video stays video.
pub fn refine_local(local: &std::path::Path, name: &str, kind: Kind) -> Kind {
    refine_input(&crate::ffmpeg::Input::path(local), name, kind)
}

/// [`refine_local`] for any input ffprobe reads: a local path, or a remote file's `play` URL on
/// its window's byte server (06 §7.4: the `.ts` refine probes the URL, nothing is fetched first).
/// The probe has the URL's deadline ([`crate::ffmpeg::probe_input`]).
pub fn refine_input(input: &crate::ffmpeg::Input, name: &str, kind: Kind) -> Kind {
    refine_input_until(input, name, kind, &|| false)
}

/// [`refine_input`] that its caller can stop: said before ffprobe is started, none is; said
/// while it runs, it is killed and reaped at its next look ([`crate::ffmpeg::probe_timed`]).
/// Either way nothing is known of the file — whoever stopped it does not use the answer.
pub fn refine_input_until(
    input: &crate::ffmpeg::Input,
    name: &str,
    kind: Kind,
    stop: &dyn Fn() -> bool,
) -> Kind {
    if kind != Kind::Video {
        return kind;
    }
    refine_input_with(crate::ffmpeg::resolve(), input, name, kind, stop)
}

/// [`refine_input_until`] of a video with the tools given (tests: a stand-in `ffprobe`).
pub(crate) fn refine_input_with(
    tools: anyhow::Result<crate::ffmpeg::Tools>,
    input: &crate::ffmpeg::Input,
    name: &str,
    kind: Kind,
    stop: &dyn Fn() -> bool,
) -> Kind {
    let shown = input.show();
    let (limit, now) = (crate::ffmpeg::PROBE_LIMIT, std::time::Instant::now);
    let probed =
        tools.and_then(|t| crate::ffmpeg::probe_timed(&t, input, &shown, limit, &now, stop));
    let probe = match probed {
        Ok(probe) => Some((probe.video.is_some(), probe.has_audio)),
        Err(e) => {
            crate::debug!("not probed: {}", input.said(&format!("{e:#}")));
            None
        }
    };
    refine_after_fetch(name, kind, probe)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encoding::{pick, Encoding};

    #[test]
    fn extension_table() {
        let cases = [
            ("shot.PNG", Some(Kind::Image)),
            ("/x/y/photo.jpeg", Some(Kind::Image)),
            ("talk.mkv", Some(Kind::Video)),
            ("a.mp3", Some(Kind::Audio)),
            ("voice.opus", Some(Kind::Audio)),
            ("main.rs", Some(Kind::Text)),
            ("clip.ts", None),
            ("app.tsx", Some(Kind::Text)),
            ("app.log", Some(Kind::Text)),
            ("Makefile", None),
            (".bashrc", None),
            ("archive.tar.gz", None),
            ("trailing.", None),
        ];
        for (name, want) in cases {
            assert_eq!(by_extension(name), want, "{name}");
        }
        assert_eq!(extension("a.TAR.GZ").as_deref(), Some("gz"));
        assert_eq!(extension("dir.d/file"), None);
    }

    #[test]
    fn sniff() {
        assert!(looks_like_text(b"#!/bin/sh\necho hi\n", false));
        assert!(looks_like_text(b"", false));
        assert!(looks_like_text("한글 텍스트".as_bytes(), false));
        assert!(!looks_like_text(b"ELF\x00\x01", false));
        // "한" is 3 bytes; cut after 2 at the sniff boundary.
        let cut = &"a한".as_bytes()[..3];
        assert!(looks_like_text(cut, true));
        assert!(!looks_like_text(cut, false));
    }

    #[test]
    fn classify_routes() {
        let never = || -> Vec<u8> { panic!("extension decides, no sniff") };
        assert_eq!(classify("a.png", never, 10), Kind::Image);
        assert_eq!(classify("a.rs", never, 10), Kind::Text);
        assert_eq!(
            classify("Makefile", || b"all:\n\ttrue\n".to_vec(), 11),
            Kind::Text
        );
        assert_eq!(classify("blob.bin", || vec![0x7f, 0, 1, 2], 4), Kind::Other);
        assert_eq!(refine_with_probe(Kind::Video, false), Kind::Audio);
        assert_eq!(refine_with_probe(Kind::Video, true), Kind::Video);
        assert_eq!(refine_with_probe(Kind::Image, false), Kind::Image);
    }

    #[test]
    fn ts_is_typescript_or_transport_stream() {
        let typescript = b"export const x: number = 1;\n".to_vec();
        assert_eq!(classify("a.ts", || typescript, 28), Kind::Text);
        let mut mpegts = vec![0u8; 188 * 4];
        for packet in mpegts.chunks_mut(188) {
            packet[0] = 0x47;
            packet[1] = 0x40;
        }
        assert_eq!(classify("clip.ts", || mpegts.clone(), 1 << 20), Kind::Video);
        assert_eq!(classify("clip.TS", || mpegts.clone(), 1 << 20), Kind::Video);
        assert_eq!(
            classify("blob.ts", || vec![0x00, 0x01, 0x02], 3),
            Kind::Other
        );
        // A transport-stream sync pattern under another extension is still just "other".
        assert_eq!(classify("clip.bin", || mpegts, 1 << 20), Kind::Other);

        let ts = |probe| refine_after_fetch("clip.ts", Kind::Video, probe);
        assert_eq!(ts(Some((true, true))), Kind::Video);
        assert_eq!(ts(Some((false, true))), Kind::Audio);
        assert_eq!(ts(Some((false, false))), Kind::Other);
        assert_eq!(ts(None), Kind::Other);
        assert_eq!(refine_after_fetch("a.mp4", Kind::Video, None), Kind::Video);
        assert_eq!(
            refine_after_fetch("a.mp4", Kind::Video, Some((false, true))),
            Kind::Audio
        );
        assert_eq!(refine_after_fetch("a.ts", Kind::Text, None), Kind::Text);
    }

    // 08 D18: UTF-16 / UTF-32 with no known extension is text, by §7.2's steps 1–2.

    /// `text` in a UTF-16 or UTF-32 encoding, no BOM.
    fn wide(e: Encoding, text: &str) -> Vec<u8> {
        let units: Vec<u32> = match e {
            Encoding::Utf16Le | Encoding::Utf16Be => text.encode_utf16().map(u32::from).collect(),
            _ => text.chars().map(u32::from).collect(),
        };
        units
            .iter()
            .flat_map(|&u| match e {
                Encoding::Utf16Le => (u as u16).to_le_bytes().to_vec(),
                Encoding::Utf16Be => (u as u16).to_be_bytes().to_vec(),
                Encoding::Utf32Le => u.to_le_bytes().to_vec(),
                _ => u.to_be_bytes().to_vec(),
            })
            .collect()
    }

    /// `text` in `e` after its BOM.
    fn with_bom(e: Encoding, text: &str) -> Vec<u8> {
        let mut out = e.bom().to_vec();
        out.extend(wide(e, text));
        out
    }

    const WIDE: [Encoding; 4] = [
        Encoding::Utf16Le,
        Encoding::Utf16Be,
        Encoding::Utf32Le,
        Encoding::Utf32Be,
    ];

    /// §7.2's Korean-and-ASCII log: 79 % ASCII units in UTF-16LE.
    fn korean_log() -> String {
        (0..400)
            .map(|i| {
                format!(
                    "2026-11-24 10:{:02}:{:02} INFO user={i:04} 사용자가 로그인 성공함\r\n",
                    i / 60 % 60,
                    i % 60
                )
            })
            .collect()
    }

    /// The kind as the sniff reads it: the first [`SNIFF_BYTES`] of a file of `bytes`.
    fn sniffed(name: &str, bytes: &[u8]) -> Kind {
        let head = bytes[..bytes.len().min(SNIFF_BYTES)].to_vec();
        classify(name, || head, bytes.len() as u64)
    }

    #[test]
    fn d18_utf16_and_utf32_with_no_known_extension_are_text() {
        let english = "The quick brown fox jumps over the lazy dog, again and again.\n".repeat(200);
        let korean = "안녕하세요 오늘은 날씨가 좋습니다. 내일 다시 만나요.\n".repeat(200);
        for e in WIDE {
            for name in ["notes.dat", "NOTES", "x.bin"] {
                // Step 1: a BOM, whatever follows.
                let bytes = with_bom(e, &korean);
                assert_eq!(sniffed(name, &bytes), Kind::Text, "{name} {e:?} with a BOM");
                assert_eq!(pick(&bytes, true).encoding, e, "{e:?}: the viewer agrees");
                // Step 2: no BOM, ASCII-heavy.
                let bytes = wide(e, &english);
                assert_eq!(sniffed(name, &bytes), Kind::Text, "{name} {e:?}, no BOM");
                assert_eq!(pick(&bytes, true).encoding, e, "{e:?}: the viewer agrees");
            }
        }
        let log = wide(Encoding::Utf16Le, &korean_log());
        assert_eq!(sniffed("notes.dat", &log), Kind::Text);
        // `kind.rs:331`'s old assertion inverts: a UTF-16LE BOM is text, whatever the size (D37).
        assert!(looks_like_text(b"\xff\xfe\x41", false));
        assert_eq!(classify("x.dat", || vec![0xFF, 0xFE], 2), Kind::Text);
        assert_eq!(classify("x.dat", || vec![0xFE, 0xFF], 2), Kind::Text);
        assert_eq!(
            classify("x.dat", || vec![0x00, 0x00, 0xFE, 0xFF], 4),
            Kind::Text
        );
        // At least 4 whole units: three ASCII units are not enough, four are.
        let three = wide(Encoding::Utf16Le, "abc");
        assert_eq!(classify("x.dat", || three, 6), Kind::Other);
        let four = wide(Encoding::Utf16Le, "abcd");
        assert_eq!(classify("x.dat", || four, 8), Kind::Text);
        let four = wide(Encoding::Utf32Be, "abcd");
        assert_eq!(classify("x.dat", || four, 16), Kind::Text);
        // The default's other answers do not route: a UTF-8 BOM before bytes that are not UTF-8.
        let bad = vec![0xEF, 0xBB, 0xBF, b'a', 0xFF, 0x00];
        assert_eq!(pick(&bad, false).encoding, Encoding::Utf8);
        assert_eq!(classify("x.dat", || bad, 6), Kind::Other);
    }

    #[test]
    fn d18_a_truncated_sniff_is_still_text() {
        let log = wide(Encoding::Utf16Le, &korean_log());
        assert!(log.len() > 2 * SNIFF_BYTES);
        let size = log.len() as u64;
        // The first 8 KiB of a longer file; an odd head ends in half a unit.
        for cut in [SNIFF_BYTES, SNIFF_BYTES - 1, 4096 + 1] {
            let head = log[..cut].to_vec();
            assert_eq!(classify("notes.dat", || head, size), Kind::Text, "{cut}");
        }
        // A pair cut in two by the head's end: 40 times 20 bytes, then 8 ASCII units and the
        // high half.
        let emoji = wide(Encoding::Utf16Le, &"abcdefgh😀".repeat(1000));
        let head = emoji[..40 * 20 + 16 + 2].to_vec();
        assert_eq!(
            head[head.len() - 2..],
            [0x3D, 0xD8],
            "a high surrogate last"
        );
        assert_eq!(
            classify("notes.dat", || head, emoji.len() as u64),
            Kind::Text
        );
        // UTF-32 cut inside a unit.
        let utf32 = wide(Encoding::Utf32Le, &"wide text\n".repeat(1000));
        let head = utf32[..SNIFF_BYTES - 2].to_vec();
        assert_eq!(
            classify("notes.dat", || head, utf32.len() as u64),
            Kind::Text
        );
        // The pattern's sample is the 8 KiB (D27): binary after it does not count — the whole
        // buffer given, so the sample's own cut (`encoding::pick`) is what is held to it.
        let mut file = wide(Encoding::Utf16Le, &"wide text\n".repeat(1000));
        file.extend(std::iter::repeat_n(0xFF, 64 * 1024));
        let size = file.len() as u64;
        assert_eq!(classify("notes.dat", || file, size), Kind::Text);
    }

    /// `[0x40, 0x00]` 4,074 times as the samples of a canonical 8 kHz 16-bit mono PCM WAV: 8 KiB
    /// in all, its samples UTF-16LE `@`.
    fn quiet_wav() -> Vec<u8> {
        let data: Vec<u8> = [0x40, 0x00].repeat(4074);
        let mut out = b"RIFF".to_vec();
        out.extend((36 + data.len() as u32).to_le_bytes());
        out.extend(b"WAVEfmt ");
        out.extend(16u32.to_le_bytes());
        out.extend(1u16.to_le_bytes()); // PCM
        out.extend(1u16.to_le_bytes()); // mono
        out.extend(8000u32.to_le_bytes());
        out.extend(16_000u32.to_le_bytes()); // bytes per second
        out.extend(2u16.to_le_bytes()); // block align
        out.extend(16u16.to_le_bytes()); // bits per sample
        out.extend(b"data");
        out.extend((data.len() as u32).to_le_bytes());
        out.extend(data);
        out
    }

    /// A binary format's signature is checked before the NUL pattern, so its fields and samples
    /// are never taken for UTF-16 / UTF-32 text; a BOM still decides first (D37).
    #[test]
    fn d18_a_known_binary_signature_is_not_wide_text() {
        let wav = quiet_wav();
        assert_eq!(wav.len(), SNIFF_BYTES);
        let alone = pick(&wav, false).encoding;
        assert_eq!(alone, Encoding::Utf16Le, "the pattern alone");
        assert_eq!(sniffed("blob.dat", &wav), Kind::Other);
        assert_eq!(sniffed("blob.wav", &wav), Kind::Audio);
        let log = wide(Encoding::Utf16Le, &korean_log());
        assert_eq!(sniffed("notes.dat", &log), Kind::Text);
        // Each signature before samples the pattern would take for text.
        let samples: Vec<u8> = [0x40, 0x00].repeat(2000);
        let signatures: [&[u8]; 24] = [
            b"RIFF",
            b"fLaC",
            b"OggS",
            b"\x89PNG",
            b"GIF8",
            b"\xFF\xD8\xFF",
            b"%PDF",
            b"PK\x03\x04",
            b"SQLite format 3\0",
            b"\x7FELF",
            b"\xFE\xED\xFA\xCE",
            b"\xFE\xED\xFA\xCF",
            b"\xCE\xFA\xED\xFE",
            b"\xCF\xFA\xED\xFE",
            b"\xCA\xFE\xBA\xBE",
            b"\x00\x01\x00\x00",
            b"OTTO",
            b"wOFF",
            b"wOF2",
            b"\x00\x00\x00\x01Bud1",
            b"\xD0\xCF\x11\xE0",
            b"regf",
            b"\x00\x00\x00\x18ftyp",
            b"\x00\x00\x00\x20ftyp",
        ];
        for sig in signatures {
            let mut file = sig.to_vec();
            file.extend(&samples);
            let alone = pick(&file, false).encoding;
            assert!(
                WIDE.contains(&alone),
                "{sig:02x?}: the pattern alone takes it"
            );
            assert_eq!(sniffed("blob.dat", &file), Kind::Other, "{sig:02x?}");
        }
        // A BOM decides before any signature: `FF FE`, `A`, then `ftyp` where an ISO media file
        // has it (offset 4), is UTF-16LE text.
        let mut bom = vec![0xFF, 0xFE, 0x41, 0x00];
        bom.extend(b"ftyp");
        bom.extend(&samples);
        assert!(signed_binary(&bom));
        assert_eq!(sniffed("blob.dat", &bom), Kind::Text);
    }

    /// The two short signatures, `MZ` and `ID3`, are the start of UTF-16LE text too (U+5A4D,
    /// U+4449): they refuse the pattern's guess only with what corroborates them — a PE header
    /// where `e_lfanew` points, or the DOS stub's message; an ID3v2 version and clear flags.
    #[test]
    fn d18_mz_and_id3_refuse_the_guess_only_when_corroborated() {
        let samples: Vec<u8> = [0x40, 0x00].repeat(2000);
        let pattern = |file: &[u8]| WIDE.contains(&pick(file, false).encoding);
        // Text: U+5A4D then `abc` (4 units, 3 ASCII), U+4449 then `3abc`, and `MZ` before
        // ASCII units with neither a PE header nor the stub.
        let texts: [&[u8]; 2] = [
            b"\x4D\x5A\x61\x00\x62\x00\x63\x00",
            b"\x49\x44\x33\x00\x61\x00\x62\x00\x63\x00",
        ];
        for file in texts {
            assert!(pattern(file), "{file:02x?}");
            assert_eq!(sniffed("notes.dat", file), Kind::Text, "{file:02x?}");
        }
        let mut mz = b"MZ".to_vec();
        mz.extend(&samples);
        assert_eq!(sniffed("notes.dat", &mz), Kind::Text);
        // A PE: `e_lfanew` at 0x3C points to `PE\0\0` inside the head.
        let mut pe = b"MZ".to_vec();
        pe.extend([0x40, 0x00].repeat(29));
        pe.extend(0x80u32.to_le_bytes());
        pe.extend([0x40, 0x00].repeat(32));
        assert_eq!(pe.len(), 0x80);
        pe.extend(b"PE\0\0");
        pe.extend(&samples);
        // The DOS stub's message, its PE header past the head.
        let mut stub = b"MZ".to_vec();
        stub.extend([0x40, 0x00].repeat(29));
        stub.extend(0x10_0000u32.to_le_bytes());
        stub.extend(b"This program cannot be run in DOS mode");
        stub.extend(&samples);
        // An MP3's ID3v2.3 tag: version 3, revision 0, flags 0, a syncsafe size.
        let mut id3 = b"ID3\x03\x00\x00\x00\x00\x10\x00".to_vec();
        id3.extend(&samples);
        for (name, file) in [("PE", pe), ("DOS stub", stub), ("ID3v2.3", id3)] {
            assert!(pattern(&file), "{name}: the pattern alone takes it");
            assert_eq!(sniffed("blob.dat", &file), Kind::Other, "{name}");
        }
        // Too short for `e_lfanew`: text.
        let mut short = b"MZ".to_vec();
        short.extend([0x41, 0x00].repeat(29));
        assert!(short.len() < 64 && pattern(&short));
        assert_eq!(sniffed("notes.dat", &short), Kind::Text);
    }

    /// The sniff before D18 (`looks_like_text` at 1c6fa00), the oracle of its UTF-8 half: no NUL
    /// and valid UTF-8, a character cut by the end of a truncated head allowed.
    fn utf8_before_d18(head: &[u8], truncated: bool) -> bool {
        !head.contains(&0)
            && match std::str::from_utf8(head) {
                Ok(_) => true,
                Err(e) => truncated && e.error_len().is_none(),
            }
    }

    /// A small deterministic generator (splitmix64), as `encoding/tests.rs` has.
    fn noise(seed: u64, n: usize) -> Vec<u8> {
        let mut s = seed;
        (0..n)
            .map(|_| {
                s = s.wrapping_add(0x9E37_79B9_7F4A_7C15);
                let mut z = s;
                z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
                z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
                (z ^ (z >> 31)) as u8
            })
            .collect()
    }

    /// §7.2's corpus for steps 1–2, routed (EA-5): text where the text view's default finds
    /// UTF-16 or UTF-32 or the file is UTF-8, else other — CP949 and Latin-1 included (step 3 does
    /// not route) and every binary but the two the pattern takes for text.
    #[test]
    fn d18_the_corpus_routes_as_section_7_2_says() {
        use Encoding::*;
        let english = "The quick brown fox jumps over the lazy dog, again and again.\n".repeat(200);
        let korean = "안녕하세요 오늘은 날씨가 좋습니다. 내일 다시 만나요.\n".repeat(200);
        let rnd = noise(0x0857_0003, 8192);
        let cp949: Vec<u8> = (0..4096)
            .flat_map(|i| match i % 5 {
                0 => vec![b' '],
                _ => vec![0xB0 + rnd[2 * i] % 0x19, 0xA1 + rnd[2 * i + 1] % 0x5E],
            })
            .collect();
        let latin1: Vec<u8> = "Café crème, déjà vu: naïve façade à la française. "
            .repeat(200)
            .chars()
            .map(|c| c as u8)
            .collect();
        let float32: Vec<u8> = (0..2048)
            .flat_map(|i| (i as f32 * 0.01).sin().to_le_bytes())
            .collect();
        let int16 =
            |top: u16| -> Vec<u8> { (0..4096u16).flat_map(|i| (i % top).to_le_bytes()).collect() };
        let int32 = |lo: u32, top: u32| -> Vec<u8> {
            (0..2048u32)
                .flat_map(|i| (lo + i % (top - lo)).to_le_bytes())
                .collect()
        };
        let exe = std::fs::read(std::env::current_exe().unwrap()).unwrap();
        let font: &[u8] = include_bytes!("../assets/fonts/PxPlus_IBM_VGA_8x16.ttf");

        let (text, other) = (Kind::Text, Kind::Other);
        let corpus: Vec<(&str, Vec<u8>, Kind)> = vec![
            ("English UTF-16LE", wide(Utf16Le, &english), text),
            ("English UTF-16BE", wide(Utf16Be, &english), text),
            ("English UTF-32LE", wide(Utf32Le, &english), text),
            ("English UTF-32BE", wide(Utf32Be, &english), text),
            ("a Korean-and-ASCII log", wide(Utf16Le, &korean_log()), text),
            ("Korean UTF-16LE", wide(Utf16Le, &korean), other),
            ("Korean UTF-32LE", wide(Utf32Le, &korean), other),
            ("English UTF-8", english.clone().into_bytes(), text),
            ("Korean UTF-8", korean.clone().into_bytes(), text),
            ("CP949", cp949, other),
            ("Latin-1", latin1, other),
            ("random bytes", rnd, other),
            ("zeros", vec![0; 8192], other),
            ("float32", float32, other),
            ("this test's executable", exe, other),
            ("a TrueType font", font.to_vec(), other),
            ("int16 0–1023", int16(1024), other),
            ("int32 0–4095", int32(0, 4096), other),
            // Taken for text: such an extension-less binary opens as text; 2 shows it.
            ("int16 0–127", int16(128), text),
            ("int32 0x20–0x7E", int32(0x20, 0x7F), text),
        ];
        for (name, bytes, want) in corpus {
            let got = sniffed("blob.dat", &bytes);
            assert_eq!(got, want, "{name}");
            // The sniff and the text view's default agree: what the UTF-8 test does not take is
            // text exactly when the default is UTF-16 or UTF-32, unless a known signature refuses
            // it (no sample here has a BOM).
            let head = &bytes[..bytes.len().min(SNIFF_BYTES)];
            let truncated = bytes.len() > SNIFF_BYTES;
            if !utf8_before_d18(head, truncated) {
                let wide = WIDE.contains(&pick(head, truncated).encoding) && !signed_binary(head);
                assert_eq!(got == Kind::Text, wide, "{name}");
            }
        }
    }

    #[test]
    fn d18_known_extensions_are_unchanged() {
        let never = || -> Vec<u8> { panic!("extension decides, no sniff") };
        for name in ["a.png", "a.mp4", "a.mp3", "a.txt", "a.rs"] {
            assert_eq!(classify(name, never, 1 << 20), by_extension(name).unwrap());
        }
        // `.ts` is sniffed: UTF-16 TypeScript is text, a transport stream still video.
        let typescript = with_bom(Encoding::Utf16Le, "export const x: number = 1;\n");
        assert_eq!(classify("a.ts", || typescript, 58), Kind::Text);
        let mut mpegts = vec![0u8; 188 * 4];
        for packet in mpegts.chunks_mut(188) {
            packet[0] = 0x47;
            packet[1] = 0x40;
        }
        assert_eq!(classify("clip.ts", || mpegts.clone(), 1 << 20), Kind::Video);
        assert_eq!(classify("clip.dat", || mpegts, 1 << 20), Kind::Other);
    }

    /// The route `main.rs` and the browser take (`detect_sniffed`), on a file on disk.
    #[test]
    fn d18_detect_reads_8_kib_and_routes_utf16_to_text() {
        use crate::remote::RemoteFs as _;
        let tmp = tempfile::tempdir().unwrap();
        let mut fs = crate::remote::LocalFs::new();
        let files = [
            (
                "notes.dat",
                wide(Encoding::Utf16Le, &korean_log()),
                Kind::Text,
            ),
            (
                "bom.dat",
                with_bom(Encoding::Utf16Be, "안녕하세요\n"),
                Kind::Text,
            ),
            (
                "korean.dat",
                wide(Encoding::Utf16Le, &"안녕하세요 오늘은\n".repeat(600)),
                Kind::Other,
            ),
        ];
        for (name, bytes, want) in files {
            let path = tmp.path().join(name);
            std::fs::write(&path, &bytes).unwrap();
            let abs = path.to_str().unwrap();
            let stat = fs.stat(abs).unwrap();
            let read = bytes.len().min(SNIFF_BYTES) as u64;
            assert_eq!(detect_sniffed(&mut fs, abs, &stat).unwrap(), (want, read));
        }
    }
}
