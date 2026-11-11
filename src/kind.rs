//! Routing (spec §3): directory by stat, image/video/audio by extension, text by extension or a
//! sniff of the first 8 KiB (no NUL, valid UTF-8), everything else "other".

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
    "ts",
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

/// No NUL and valid UTF-8. `truncated` = the buffer stops before the end of the file, so a
/// multi-byte character cut at the boundary does not count against it.
pub fn looks_like_text(head: &[u8], truncated: bool) -> bool {
    if head.contains(&0) {
        return false;
    }
    match std::str::from_utf8(head) {
        Ok(_) => true,
        Err(e) => truncated && e.error_len().is_none(),
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extension_table() {
        let cases = [
            ("shot.PNG", Some(Kind::Image)),
            ("/x/y/photo.jpeg", Some(Kind::Image)),
            ("talk.mkv", Some(Kind::Video)),
            ("a.mp3", Some(Kind::Audio)),
            ("voice.opus", Some(Kind::Audio)),
            ("main.rs", Some(Kind::Text)),
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
        assert!(!looks_like_text(b"\xff\xfe\x41", false));
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
}
