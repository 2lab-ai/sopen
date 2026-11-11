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

/// The kind `sopen cut` accepts, by extension, before anything is fetched.
pub fn media_kind_of(name: &str) -> Result<Kind> {
    match kind::by_extension(name) {
        Some(k @ (Kind::Video | Kind::Audio)) => Ok(k),
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
    let crop = match (req.crop, probe.video, media) {
        (Some(_), _, MediaKind::Audio) => bail!("--crop needs a video stream; {src_name} has none"),
        (Some(r), Some((w, h)), MediaKind::Video) => Some(ffmpeg::fit_crop(r, w, h)?),
        _ => None,
    };
    let (stem, ext) = stem_ext(src_name);
    let name = ffmpeg::cut_name(stem, req.in_s, req.out_s, &ffmpeg::output_ext(ext, media));

    let work = work_root.join(random_hex());
    std::fs::create_dir_all(&work).with_context(|| format!("cannot create {}", work.display()))?;
    let result = (|| -> Result<String> {
        let dst = work.join(&name);
        let args = ffmpeg::export_args(src, &dst, req.in_s, req.out_s, crop, media)?;
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
        let (name, _) = cut_one(&tools, tmp.path(), "a.wav", None);
        assert_eq!(name, "a.cut-1-3.wav");
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
    }
}
