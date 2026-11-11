//! `sopen cut` (spec §7): export `[in, out]` of a fetched media file (optionally cropped) and
//! publish it next to the original as `<stem>.cut-<in>-<out>.<ext>` — never over an existing file.
//! The same function serves remote targets (fetched copy → SFTP) and local ones (LocalFs).

use crate::ffmpeg::{self, MediaKind, Rect, Tools};
use crate::kind::{self, Kind};
use crate::remote::{random_hex, RemoteFs};
use anyhow::{bail, Context, Result};
use std::path::Path;

#[derive(Debug, Clone, Copy)]
pub struct CutRequest {
    pub in_s: f64,
    pub out_s: f64,
    pub crop: Option<Rect>,
}

/// The kind `sopen cut` accepts, by extension, before anything is fetched (`.ts` counts as
/// video; ffprobe decides after the fetch).
pub fn media_kind_of(name: &str) -> Result<Kind> {
    match kind::by_extension(name) {
        Some(k @ (Kind::Video | Kind::Audio)) => Ok(k),
        None if kind::maybe_video(name) => Ok(Kind::Video),
        _ => bail!("cut works on audio and video files; {name} is neither"),
    }
}

/// Splits `name` into stem and extension (`a.b.mp3` → `a.b`, `mp3`).
fn stem_ext(name: &str) -> (&str, &str) {
    match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i + 1..]),
        _ => (name, ""),
    }
}

/// Exports the cut of local file `src` (originally `src_name` in `dir` on `fs`), uploads it as a
/// new sibling and returns the final file name. `work_root` holds the local export (removed
/// afterwards); `progress` gets 0.0..=1.0 of the encode.
#[allow(clippy::too_many_arguments)]
pub fn cut_and_publish(
    fs: &mut dyn RemoteFs,
    tools: &Tools,
    src: &Path,
    dir: &str,
    src_name: &str,
    req: &CutRequest,
    work_root: &Path,
    progress: &mut dyn FnMut(f64),
) -> Result<String> {
    let by_ext = media_kind_of(src_name)?;
    let probe = ffmpeg::probe(tools, src)?;
    let media = match kind::refine_with_probe(by_ext, probe.video.is_some()) {
        Kind::Video => MediaKind::Video,
        _ if probe.has_audio => MediaKind::Audio,
        _ => bail!("{src_name} has no audio or video stream"),
    };
    if let Some(duration) = probe.duration {
        if req.in_s >= duration {
            bail!("IN {}s is past the end ({duration:.3}s)", req.in_s);
        }
        // ffprobe durations of compressed audio are estimates to a frame or so.
        if req.out_s > duration + 0.05 {
            bail!("OUT {}s is past the end ({duration:.3}s)", req.out_s);
        }
    }
    // ffmpeg rotates frames (display matrix) before the crop filter: crop in displayed pixels.
    let crop = match (req.crop, probe.display, media) {
        (Some(_), _, MediaKind::Audio) => bail!("--crop needs a video stream; {src_name} has none"),
        (Some(r), Some((w, h)), MediaKind::Video) => Some(ffmpeg::fit_crop(r, w, h)?),
        _ => None,
    };
    let audio_codec = probe.audio_codec.as_deref();
    let (stem, ext) = stem_ext(src_name);
    let out_ext = ffmpeg::output_ext(ext, media, audio_codec);
    let name = ffmpeg::cut_name(stem, req.in_s, req.out_s, &out_ext);

    let work = work_root.join(random_hex());
    std::fs::create_dir_all(&work).with_context(|| format!("cannot create {}", work.display()))?;
    let result = (|| -> Result<String> {
        let dst = work.join(&name);
        let args = ffmpeg::export_args(src, &dst, req.in_s, req.out_s, crop, media, audio_codec)?;
        let args = ffmpeg::adapt_to_encoders(args, &ffmpeg::available_encoders(tools)?)?;
        ffmpeg::run_export(tools, &args, req.out_s - req.in_s, progress)?;
        fs.write_new_sibling(&dst, dir, &name)
    })();
    let _ = std::fs::remove_dir_all(&work);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remote::LocalFs;
    use std::process::Command;

    /// ffmpeg + ffprobe, or `None` with the reason printed. `SOPEN_REQUIRE_FFMPEG=1` (CI) turns
    /// a missing ffmpeg into a failure instead of a skip.
    fn tools_or_skip() -> Option<Tools> {
        match ffmpeg::resolve() {
            Ok(t) => Some(t),
            Err(e) if std::env::var_os("SOPEN_REQUIRE_FFMPEG").is_some() => {
                panic!("SOPEN_REQUIRE_FFMPEG is set but {e}")
            }
            Err(e) => {
                eprintln!("skipping ffmpeg end-to-end test: {e}");
                None
            }
        }
    }

    fn generate(tools: &Tools, dst: &Path, inputs: &[&str], codec: &[&str]) {
        let mut cmd = Command::new(&tools.ffmpeg);
        cmd.args(["-hide_banner", "-loglevel", "error", "-nostdin", "-y"]);
        for input in inputs {
            cmd.args(["-f", "lavfi", "-i", input]);
        }
        let status = cmd.args(codec).arg(dst).status().unwrap();
        assert!(status.success(), "generating {}", dst.display());
    }

    fn cut_one(
        tools: &Tools,
        dir: &Path,
        name: &str,
        crop: Option<Rect>,
    ) -> (String, ffmpeg::Probe) {
        let mut fs = LocalFs::new();
        let src = dir.join(name);
        let req = CutRequest {
            in_s: 1.0,
            out_s: 3.0,
            crop,
        };
        let mut last = 0.0;
        let out = cut_and_publish(
            &mut fs,
            tools,
            &src,
            dir.to_str().unwrap(),
            name,
            &req,
            &dir.join("work"),
            &mut |f| last = f,
        )
        .unwrap();
        assert_eq!(last, 1.0, "progress reaches the end");
        let probe = ffmpeg::probe(tools, &dir.join(&out)).unwrap();
        let d = probe.duration.unwrap();
        assert!((d - 2.0).abs() <= 0.1, "{out}: duration {d}");
        (out, probe)
    }

    const SINE: &str = "sine=frequency=440:duration=4";

    #[test]
    fn video_cut_with_crop() {
        let Some(tools) = tools_or_skip() else { return };
        let tmp = tempfile::tempdir().unwrap();
        generate(
            &tools,
            &tmp.path().join("v.mp4"),
            &["testsrc2=size=640x360:rate=30:duration=4", SINE],
            &[
                "-c:v",
                "libx264",
                "-pix_fmt",
                "yuv420p",
                "-c:a",
                "aac",
                "-shortest",
            ],
        );
        let crop = Rect {
            x: 100,
            y: 50,
            w: 320,
            h: 180,
        };
        let (name, probe) = cut_one(&tools, tmp.path(), "v.mp4", Some(crop));
        assert_eq!(name, "v.cut-1-3.mp4");
        assert_eq!(probe.video, Some((320, 180)));
        assert!(probe.has_audio);
        assert!(!tmp.path().join("work").read_dir().unwrap().any(|_| true));
    }

    #[test]
    fn mp3_cut_never_overwrites() {
        let Some(tools) = tools_or_skip() else { return };
        let tmp = tempfile::tempdir().unwrap();
        generate(
            &tools,
            &tmp.path().join("t.mp3"),
            &[SINE],
            &["-c:a", "libmp3lame"],
        );
        let (first, probe) = cut_one(&tools, tmp.path(), "t.mp3", None);
        assert_eq!(first, "t.cut-1-3.mp3");
        assert_eq!(probe.video, None);
        let (second, _) = cut_one(&tools, tmp.path(), "t.mp3", None);
        assert_eq!(second, "t.cut-1-3-1.mp3");
    }

    #[test]
    fn wav_cut() {
        let Some(tools) = tools_or_skip() else { return };
        let tmp = tempfile::tempdir().unwrap();
        generate(&tools, &tmp.path().join("a.wav"), &[SINE], &[]);
        let (name, probe) = cut_one(&tools, tmp.path(), "a.wav", None);
        assert_eq!(name, "a.cut-1-3.wav");
        assert_eq!(probe.audio_codec.as_deref(), Some("pcm_s16le"));
    }

    /// Audio-only files in video containers, and PCM depth: the source codec is kept (spec §7).
    #[test]
    fn audio_only_sources_keep_codec_and_container() {
        let Some(tools) = tools_or_skip() else { return };
        let tmp = tempfile::tempdir().unwrap();
        let cases = [
            ("a.mp4", "aac", "a.cut-1-3.mp4", "aac"),
            ("b.webm", "libopus", "b.cut-1-3.webm", "opus"),
            ("c.wav", "pcm_s24le", "c.cut-1-3.wav", "pcm_s24le"),
        ];
        for (src, encoder, want_name, want_codec) in cases {
            generate(&tools, &tmp.path().join(src), &[SINE], &["-c:a", encoder]);
            let (name, probe) = cut_one(&tools, tmp.path(), src, None);
            assert_eq!(name, want_name);
            assert_eq!(probe.video, None, "{name}");
            assert_eq!(probe.audio_codec.as_deref(), Some(want_codec), "{name}");
        }
    }

    /// A phone video stored 640x360 with a 90° display matrix shows as 360x640; `--crop` is in
    /// the displayed pixels because ffmpeg rotates before the crop filter.
    #[test]
    fn crop_is_checked_against_the_rotated_frame() {
        let Some(tools) = tools_or_skip() else { return };
        let tmp = tempfile::tempdir().unwrap();
        let plain = tmp.path().join("plain.mp4");
        generate(
            &tools,
            &plain,
            &["testsrc2=size=640x360:rate=30:duration=4"],
            &["-c:v", "libx264", "-pix_fmt", "yuv420p"],
        );
        // `-display_rotation` on a stream copy writes the display matrix (ffmpeg ≥ 6.1).
        let status = Command::new(&tools.ffmpeg)
            .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-y"])
            .args(["-display_rotation", "90", "-i"])
            .arg(&plain)
            .args(["-c", "copy"])
            .arg(tmp.path().join("r.mp4"))
            .status()
            .unwrap();
        assert!(status.success());
        let src = ffmpeg::probe(&tools, &tmp.path().join("r.mp4")).unwrap();
        assert_eq!(src.video, Some((640, 360)));
        assert_eq!(src.rotation, 90);
        assert_eq!(src.display, Some((360, 640)));

        let crop = Rect {
            x: 10,
            y: 20,
            w: 300,
            h: 600,
        };
        let (_, out) = cut_one(&tools, tmp.path(), "r.mp4", Some(crop));
        assert_eq!(out.display, Some((300, 600)));
        let too_wide = Rect {
            x: 0,
            y: 0,
            w: 400,
            h: 100,
        };
        let err = cut_and_publish(
            &mut LocalFs::new(),
            &tools,
            &tmp.path().join("r.mp4"),
            tmp.path().to_str().unwrap(),
            "r.mp4",
            &CutRequest {
                in_s: 1.0,
                out_s: 2.0,
                crop: Some(too_wide),
            },
            &tmp.path().join("work"),
            &mut |_| {},
        )
        .unwrap_err();
        assert!(err.to_string().contains("360x640"), "{err}");
    }

    /// `.ts` is MPEG-TS here (TypeScript is text and never reaches cut).
    #[test]
    fn transport_stream_cut() {
        let Some(tools) = tools_or_skip() else { return };
        let tmp = tempfile::tempdir().unwrap();
        generate(
            &tools,
            &tmp.path().join("t.ts"),
            &["testsrc2=size=320x240:rate=30:duration=4", SINE],
            &[
                "-c:v",
                "libx264",
                "-pix_fmt",
                "yuv420p",
                "-c:a",
                "aac",
                "-shortest",
            ],
        );
        let head = std::fs::read(tmp.path().join("t.ts")).unwrap();
        assert_eq!(kind::classify("t.ts", || head, 1 << 20), Kind::Video);
        let (name, probe) = cut_one(&tools, tmp.path(), "t.ts", None);
        assert_eq!(name, "t.cut-1-3.ts");
        assert_eq!(probe.video, Some((320, 240)));
    }

    #[test]
    fn refuses_bad_requests() {
        let Some(tools) = tools_or_skip() else { return };
        let tmp = tempfile::tempdir().unwrap();
        generate(&tools, &tmp.path().join("a.wav"), &[SINE], &[]);
        let mut fs = LocalFs::new();
        let dir = tmp.path().to_str().unwrap();
        let mut run = |req: CutRequest| {
            cut_and_publish(
                &mut fs,
                &tools,
                &tmp.path().join("a.wav"),
                dir,
                "a.wav",
                &req,
                &tmp.path().join("work"),
                &mut |_| {},
            )
            .unwrap_err()
            .to_string()
        };
        let past = run(CutRequest {
            in_s: 1.0,
            out_s: 9.0,
            crop: None,
        });
        assert!(past.contains("past the end"), "{past}");
        let crop = run(CutRequest {
            in_s: 1.0,
            out_s: 2.0,
            crop: Some(Rect {
                x: 0,
                y: 0,
                w: 2,
                h: 2,
            }),
        });
        assert!(crop.contains("needs a video stream"), "{crop}");
        assert!(media_kind_of("notes.txt").is_err());
        assert_eq!(media_kind_of("x.MOV").unwrap(), Kind::Video);
        assert_eq!(media_kind_of("x.ts").unwrap(), Kind::Video);
    }
}
