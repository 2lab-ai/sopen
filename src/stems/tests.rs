use super::*;

/// A stand-in Homebrew keg: `<keg>/bin/sopen`, and the model's place `<keg>/share/<formula>/`.
fn keg(root: &Path) -> (PathBuf, PathBuf) {
    let exe = root.join("keg/bin/sopen");
    std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
    std::fs::write(&exe, "").unwrap();
    let model = root.join("keg/share").join(formula()).join(MODEL_FILE);
    (exe, model)
}

#[test]
fn the_model_is_looked_for_flag_then_env_then_next_to_the_binary() {
    let tmp = tempfile::tempdir().unwrap();
    let (exe, installed) = keg(tmp.path());
    let flag = Path::new("/m/flag.onnx");
    let env = || Some(OsString::from("/m/env.onnx"));
    let at = |flag, env, exe| locate_model(flag, env, exe, formula()).unwrap();
    assert_eq!(
        at(Some(flag), env(), Some(exe.as_path())),
        (flag.to_path_buf(), ModelFrom::Flag)
    );
    assert_eq!(
        at(None, env(), Some(exe.as_path())),
        (PathBuf::from("/m/env.onnx"), ModelFrom::Env)
    );
    // An empty variable is unset.
    assert_eq!(
        at(None, Some(OsString::new()), Some(exe.as_path())),
        (installed.clone(), ModelFrom::Installed)
    );
    assert_eq!(
        at(None, None, Some(exe.as_path())),
        (installed, ModelFrom::Installed)
    );
    let err = locate_model(None, None, None, formula()).unwrap_err();
    assert!(err.to_string().contains(MODEL_ENV), "{err}");
    // Homebrew's two formulas: the keg's own share folder.
    let (path, _) = locate_model(None, None, Some(&exe), "sopen").unwrap();
    assert!(
        path.ends_with("keg/share/sopen/UVR-MDX-NET-Voc_FT.onnx"),
        "{path:?}"
    );
}

/// Missing or not the measured file: one line that names the path it expected and how to fix it.
#[test]
fn a_missing_or_different_model_is_one_error_naming_the_path() {
    let tmp = tempfile::tempdir().unwrap();
    let (exe, installed) = keg(tmp.path());
    let err = format!("{:#}", find_model(None, None, Some(&exe)).unwrap_err());
    assert!(err.contains(&installed.display().to_string()), "{err}");
    assert!(err.contains("missing"), "{err}");
    assert!(
        err.contains(&format!("brew reinstall {}", formula())),
        "{err}"
    );
    assert!(!err.contains('\n'), "{err}");

    let elsewhere = tmp.path().join("nope.onnx");
    let err = format!(
        "{:#}",
        find_model(None, Some(elsewhere.clone().into_os_string()), Some(&exe)).unwrap_err()
    );
    assert!(err.contains(&elsewhere.display().to_string()), "{err}");
    assert!(err.contains(MODEL_ENV), "{err}");

    // Present, but another file.
    std::fs::create_dir_all(installed.parent().unwrap()).unwrap();
    std::fs::write(&installed, b"not the model").unwrap();
    let err = format!("{:#}", find_model(None, None, Some(&exe)).unwrap_err());
    assert!(err.contains(&installed.display().to_string()), "{err}");
    assert!(err.contains("is not the voice/music model"), "{err}");
    assert!(err.contains("13 bytes, want 66762795"), "{err}");
    let flagged = tmp.path().join("flag.onnx");
    std::fs::write(&flagged, b"x").unwrap();
    let err = format!("{:#}", find_model(Some(&flagged), None, None).unwrap_err());
    assert!(err.contains("--model"), "{err}");
}

/// The size can match and the content not: the SHA-256 decides.
#[test]
fn a_model_with_the_right_size_but_other_bytes_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("m.onnx");
    std::fs::write(&file, b"abc").unwrap();
    // SHA-256("abc").
    let abc = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
    verify_file(&file, 3, abc).unwrap();
    std::fs::write(&file, b"abd").unwrap();
    let err = verify_file(&file, 3, abc).unwrap_err().to_string();
    assert!(err.starts_with("SHA-256 "), "{err}");
    assert!(err.ends_with(&format!("want {abc}")), "{err}");
}

/// The windows cover the track exactly, in order, each core once; each window is its core
/// with `context` on both sides, the file's own frames where it has them and silence past its
/// ends; the last core is what is left.
#[test]
fn pieces_cover_the_input_exactly_with_context_on_both_sides() {
    for (piece, context) in [(4, 1), (30, 1), (3, 5), (1, 0)] {
        let layout = Layout { piece, context };
        for total in 0..70u64 {
            let spans = layout.spans(total);
            assert_eq!(
                spans.len() as u64,
                layout.count(total),
                "{layout:?} {total}"
            );
            let mut next = 0;
            for (k, s) in spans.iter().enumerate() {
                assert_eq!(s.core.start, next, "{layout:?} {total} #{k}");
                assert!(s.core.end > s.core.start);
                let last = k + 1 == spans.len();
                let len = s.core.end - s.core.start;
                assert_eq!(
                    len,
                    if last {
                        total - k as u64 * piece
                    } else {
                        piece
                    }
                );
                assert_eq!(s.window_len(), len + 2 * context);
                // Real frames on each side, as many as the file has, silence for the rest.
                assert_eq!(s.read.start, s.core.start.saturating_sub(context));
                assert_eq!(s.read.end, (s.core.end + context).min(total));
                assert_eq!(s.lead, context - (s.core.start - s.read.start));
                assert_eq!(s.tail, context - (s.read.end - s.core.end));
                next = s.core.end;
            }
            assert_eq!(next, total, "{layout:?}");
        }
    }
    // The spike's numbers: 30 s pieces, 1 s of context, at 44.1 kHz; a 65 s track is
    // 30 + 30 + 5, the last window 7 s.
    let layout = Layout::standard(44_100);
    assert_eq!((layout.piece, layout.context), (1_323_000, 44_100));
    let spans = layout.spans(65 * 44_100);
    assert_eq!(spans.len(), 3);
    assert_eq!(spans[0].lead, 44_100);
    assert_eq!(spans[0].read, 0..31 * 44_100);
    assert_eq!(spans[1].read, 29 * 44_100..61 * 44_100);
    assert_eq!(spans[2].core, 60 * 44_100..65 * 44_100);
    assert_eq!((spans[2].read.end, spans[2].tail), (65 * 44_100, 44_100));
    assert_eq!(spans[2].window_len(), 7 * 44_100);
}

/// A window is decided by what is known to be there: before the end of the file is read,
/// `end` = how far the next window reads, and the span is the same as with the whole length.
#[test]
fn a_span_needs_only_the_audio_up_to_its_own_end() {
    let layout = Layout {
        piece: 10,
        context: 2,
    };
    let whole = layout.spans(100);
    for (k, s) in whole.iter().enumerate() {
        let k = k as u64;
        let known = (k + 1) * 10 + 2;
        assert_eq!(layout.span(k, known.min(100)).as_ref(), Some(s));
    }
    assert_eq!(layout.span(10, 100), None);
}

#[test]
fn progress_lines_round_trip_and_nothing_else_parses() {
    let p = PieceProgress {
        done: 3,
        total: 8,
        elapsed_s: 61,
        remaining_s: Some(102),
    };
    assert_eq!(p.line(), "stems: piece 3/8, 61s elapsed, 102s remaining");
    assert_eq!(PieceProgress::parse(&p.line()), Some(p));
    let start = PieceProgress {
        done: 0,
        total: 8,
        elapsed_s: 0,
        remaining_s: None,
    };
    assert_eq!(start.line(), "stems: piece 0/8, 0s elapsed");
    assert_eq!(PieceProgress::parse(&start.line()), Some(start));
    assert_eq!(start.fraction(), 0.0);
    // As a terminal shows it (`\r`, then the line), with spaces around.
    assert_eq!(PieceProgress::parse(&format!("  {}\r", p.line())), Some(p));
    for other in [
        "sopen: stems works on audio and video files",
        READY_LINE,
        "stems: piece 9/8, 1s elapsed",
        "stems: piece 1/0, 1s elapsed",
        "stems: piece 1/2",
        "stems: piece 1/2, 1s elapsed, 2s remaining, extra",
        "stems: piece a/2, 1s elapsed",
        "stems: piece 1/2, -1s elapsed",
    ] {
        assert_eq!(PieceProgress::parse(other), None, "{other}");
    }
}

#[test]
fn stems_are_named_after_the_source() {
    assert_eq!(
        stem_names("talk.final.mp3", "mp3"),
        ("talk.final.voice.mp3".into(), "talk.final.music.mp3".into())
    );
    assert_eq!(
        stem_names("clip.mp4", "m4a"),
        ("clip.voice.m4a".into(), "clip.music.m4a".into())
    );
    assert_eq!(media_kind_of("x.MOV").unwrap(), Kind::Video);
    assert_eq!(media_kind_of("x.flac").unwrap(), Kind::Audio);
    let err = media_kind_of("notes.txt").unwrap_err().to_string();
    assert_eq!(
        err,
        "stems works on audio and video files; notes.txt is neither"
    );
}

/// The model, from `SOPEN_STEMS_MODEL` (CI downloads it), or `None` with the reason printed.
/// `SOPEN_REQUIRE_STEMS=1` (CI) turns a missing model into a failure instead of a skip.
#[cfg(feature = "stems")]
pub(crate) fn model_or_skip() -> Option<PathBuf> {
    let env = std::env::var_os(MODEL_ENV).filter(|v| !v.is_empty());
    let found = match env {
        Some(env) => find_model(None, Some(env), None),
        None => Err(anyhow::anyhow!("{MODEL_ENV} is not set")),
    };
    match found {
        Ok(path) => Some(path),
        Err(e) if std::env::var_os("SOPEN_REQUIRE_STEMS").is_some() => {
            panic!("SOPEN_REQUIRE_STEMS is set but {e:#}")
        }
        Err(e) => {
            eprintln!("skipping the separation test: {e:#}");
            None
        }
    }
}

/// A 10 s track separated in 4 s pieces (4 + 4 + 2: the last one shorter) with 1 s of
/// context: both stems are as long as the source, and voice + music give the source back
/// sample for sample — which they only do when every window's kept part is put back exactly
/// where it was taken from (the engine's music is the window minus its voice).
#[cfg(feature = "stems")]
#[test]
fn a_separated_track_is_its_two_stems_sample_for_sample() {
    use crate::ffmpeg;
    use crate::remote::LocalFs;
    let Some(model) = model_or_skip() else { return };
    let tools = match ffmpeg::resolve() {
        Ok(t) => t,
        Err(e) if std::env::var_os("SOPEN_REQUIRE_FFMPEG").is_some() => panic!("{e:#}"),
        Err(e) => return eprintln!("skipping the separation test: {e:#}"),
    };
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("mix.wav");
    // A tone over noise, stereo, 32-bit float so the check is not lost to rounding.
    let status = std::process::Command::new(&tools.ffmpeg)
        .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-y"])
        .args([
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=220:duration=10:sample_rate=44100",
        ])
        .args(["-f", "lavfi", "-i", "anoisesrc=d=10:c=pink:a=0.1:r=44100"])
        .args([
            "-filter_complex",
            "[0][1]amix=inputs=2,aformat=channel_layouts=stereo",
        ])
        .args(["-c:a", "pcm_f32le"])
        .arg(&src)
        .status()
        .unwrap();
    assert!(status.success());
    let work = tmp.path().join("work");
    crate::cache::private_dir(&work).unwrap();
    let mut seen = Vec::new();
    let mut asked = 0;
    let published = separate_and_publish(
        &mut LocalFs::new(),
        &tools,
        &model,
        &src,
        tmp.path().to_str().unwrap(),
        "mix.wav",
        &Options {
            piece_s: 4.0,
            context_s: 1.0,
            // CI's runners have 3 (macOS) and 4 (Linux) cores.
            threads: 4,
        },
        &work,
        &mut || {
            asked += 1;
            Ok(())
        },
        &mut |p| seen.push(*p),
    )
    .unwrap();
    assert_eq!(asked, 1, "asked once before publishing");
    assert_eq!(
        (published.voice.as_str(), published.music.as_str()),
        ("mix.voice.wav", "mix.music.wav")
    );
    assert!(published.notes.is_empty(), "{:?}", published.notes);
    assert_eq!(
        seen.iter().map(|p| (p.done, p.total)).collect::<Vec<_>>(),
        [(0, 3), (1, 3), (2, 3), (3, 3)]
    );
    assert_eq!(seen[3].remaining_s, Some(0));
    assert_eq!(
        std::fs::read_dir(&work).unwrap().count(),
        0,
        "the local encodes are removed"
    );
    let decode = |name: &str| -> Vec<f32> {
        let out = std::process::Command::new(&tools.ffmpeg)
            .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-i"])
            .arg(tmp.path().join(name))
            .args(["-f", "f32le", "-c:a", "pcm_f32le", "-"])
            .output()
            .unwrap();
        assert!(out.status.success());
        out.stdout
            .as_chunks::<4>()
            .0
            .iter()
            .map(|b| f32::from_le_bytes(*b))
            .collect()
    };
    let (mix, voice, music) = (
        decode("mix.wav"),
        decode("mix.voice.wav"),
        decode("mix.music.wav"),
    );
    assert_eq!(mix.len(), 2 * 441_000);
    assert_eq!(voice.len(), mix.len());
    assert_eq!(music.len(), mix.len());
    let worst = mix
        .iter()
        .zip(voice.iter().zip(&music))
        .map(|(m, (v, u))| (m - (v + u)).abs())
        .fold(0.0f32, f32::max);
    assert!(
        worst < 1e-5,
        "voice + music is off the source by up to {worst}"
    );
    let energy = |x: &[f32]| x.iter().map(|s| s * s).sum::<f32>();
    assert!(
        energy(&music) > 0.0 && energy(&voice) < energy(&mix),
        "a split happened"
    );
}

/// A 22.05 kHz mp3 separates into two mp3 stems that decode exactly as long as it does — on
/// ffmpeg 9.1 too, whose LAME tag below 32 kHz says 529 samples too little encoder delay
/// ([`crate::ffmpeg::mp3`]): unmended, each stem would decode 529 samples of priming first.
#[cfg(feature = "stems")]
#[test]
fn a_22_khz_mp3_separates_into_stems_as_long_as_it() {
    use crate::ffmpeg;
    use crate::remote::LocalFs;
    let Some(model) = model_or_skip() else { return };
    let tools = match ffmpeg::resolve() {
        Ok(t) => t,
        Err(e) if std::env::var_os("SOPEN_REQUIRE_FFMPEG").is_some() => panic!("{e:#}"),
        Err(e) => return eprintln!("skipping the separation test: {e:#}"),
    };
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("talk.mp3");
    let status = std::process::Command::new(&tools.ffmpeg)
        .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-y"])
        .args([
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=220:duration=6:sample_rate=22050",
        ])
        .args(["-c:a", "libmp3lame", "-b:a", "64k"])
        .arg(&src)
        .status()
        .unwrap();
    assert!(status.success());
    let work = tmp.path().join("work");
    crate::cache::private_dir(&work).unwrap();
    let published = separate_and_publish(
        &mut LocalFs::new(),
        &tools,
        &model,
        &src,
        tmp.path().to_str().unwrap(),
        "talk.mp3",
        &Options {
            piece_s: 4.0,
            context_s: 1.0,
            threads: 4,
        },
        &work,
        &mut || Ok(()),
        &mut |_| {},
    )
    .unwrap();
    let samples = |name: &str| -> usize {
        let out = std::process::Command::new(&tools.ffmpeg)
            .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-i"])
            .arg(tmp.path().join(name))
            .args(["-ac", "1", "-f", "s16le", "-"])
            .output()
            .unwrap();
        assert!(out.status.success());
        out.stdout.len() / 2
    };
    let source = samples("talk.mp3");
    for stem in [&published.voice, &published.music] {
        assert!(stem.ends_with(".mp3"), "{stem}");
        assert_eq!(samples(stem), source, "{stem}");
    }
}

/// A file with no audio stream is refused by the probe, before any model is loaded (so no
/// model is needed here) and before anything is written.
#[cfg(feature = "stems")]
#[test]
fn a_file_without_audio_is_refused_before_the_model_loads() {
    use crate::ffmpeg;
    use crate::remote::LocalFs;
    let tools = match ffmpeg::resolve() {
        Ok(t) => t,
        Err(e) if std::env::var_os("SOPEN_REQUIRE_FFMPEG").is_some() => panic!("{e:#}"),
        Err(e) => return eprintln!("skipping: {e:#}"),
    };
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("silent.mp4");
    let status = std::process::Command::new(&tools.ffmpeg)
        .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-y"])
        .args([
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=160x120:rate=25:duration=2",
        ])
        .args(["-c:v", "libx264", "-pix_fmt", "yuv420p"])
        .arg(&src)
        .status()
        .unwrap();
    assert!(status.success());
    let work = tmp.path().join("work");
    crate::cache::private_dir(&work).unwrap();
    let err = separate_and_publish(
        &mut LocalFs::new(),
        &tools,
        &tmp.path().join("no-model-needed.onnx"),
        &src,
        tmp.path().to_str().unwrap(),
        "silent.mp4",
        &Options::default(),
        &work,
        &mut || panic!("nothing to publish"),
        &mut |_| panic!("no progress before the probe passes"),
    )
    .unwrap_err();
    assert_eq!(
        err.to_string(),
        "silent.mp4 has no audio stream to separate"
    );
    assert_eq!(std::fs::read_dir(&work).unwrap().count(), 0);
    assert_eq!(
        std::fs::read_dir(tmp.path()).unwrap().count(),
        2,
        "silent.mp4 and work only"
    );
}

/// The pair carries one number: the first free for both names, whatever is taken (a file, a
/// folder, a dangling link) under either.
#[test]
fn the_pair_gets_the_first_number_free_for_both() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let d = dir.to_str().unwrap();
    let mut fs = crate::remote::LocalFs::new();
    let pick =
        |fs: &mut crate::remote::LocalFs| free_pair(fs, d, "a.voice.wav", "a.music.wav").unwrap();
    assert_eq!(pick(&mut fs), ("a.voice.wav".into(), "a.music.wav".into()));
    std::fs::write(dir.join("a.music.wav"), "").unwrap();
    assert_eq!(
        pick(&mut fs),
        ("a.voice-1.wav".into(), "a.music-1.wav".into())
    );
    std::fs::write(dir.join("a.voice-1.wav"), "").unwrap();
    std::fs::create_dir(dir.join("a.music-2.wav")).unwrap();
    std::os::unix::fs::symlink(dir.join("nowhere"), dir.join("a.voice-3.wav")).unwrap();
    assert_eq!(
        pick(&mut fs),
        ("a.voice-4.wav".into(), "a.music-4.wav".into())
    );
}

/// The piece loop (spec §7a) with a deterministic stand-in for the engine — no model needed.
#[cfg(feature = "stems")]
mod piece_loop {
    use super::super::engine::Separated;
    use super::super::pipeline::{run_pieces, Separate};
    use super::*;
    use anyhow::Result;
    use std::io::Read;
    use std::time::Instant;

    /// A stand-in engine whose voice at each sample is the mean of the window's samples on both
    /// sides of it (silence past the window's edges) — so it depends on context — and whose
    /// music is the window minus that. It keeps every window it was given.
    #[derive(Default)]
    struct Neighbours {
        windows: Vec<[Vec<f32>; 2]>,
    }

    impl Separate for Neighbours {
        fn separate(&mut self, left: &[f32], right: &[f32]) -> Result<Separated> {
            let voice = |w: &[f32]| -> Vec<f32> {
                (0..w.len())
                    .map(|i| {
                        let before = if i > 0 { w[i - 1] } else { 0.0 };
                        let after = w.get(i + 1).copied().unwrap_or(0.0);
                        0.5 * (before + after)
                    })
                    .collect()
            };
            let (vl, vr) = (voice(left), voice(right));
            let music = |w: &[f32], v: &[f32]| -> Vec<f32> {
                w.iter().zip(v).map(|(a, b)| a - b).collect()
            };
            let separated = Separated {
                music: [music(left, &vl), music(right, &vr)],
                voice: [vl, vr],
            };
            self.windows.push([left.to_vec(), right.to_vec()]);
            Ok(separated)
        }
    }

    /// Reads at most 13 bytes at a time: frames arrive cut anywhere.
    struct Ragged(std::io::Cursor<Vec<u8>>);
    impl Read for Ragged {
        fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
            let n = out.len().min(13);
            self.0.read(&mut out[..n])
        }
    }

    /// Through the real piece loop with a stand-in that depends on context: every window is the
    /// file's own frames around its piece (silence only past the file's ends), and every kept
    /// sample is the stand-in's answer for that very sample with its real neighbours — at the
    /// joins between pieces too; the last piece is the shorter rest.
    #[test]
    fn the_piece_loop_gives_each_window_its_real_context_and_keeps_the_right_samples() {
        let n = 30usize;
        let x = |c: usize, t: usize| -> f32 {
            if c == 0 {
                (t as f32 * 0.37).sin() + 0.01 * t as f32
            } else {
                (t as f32 * 0.91).cos() - 0.02 * t as f32
            }
        };
        let bytes: Vec<u8> = (0..n)
            .flat_map(|t| [x(0, t), x(1, t)])
            .flat_map(f32::to_le_bytes)
            .collect();
        let layout = Layout {
            piece: 7,
            context: 3,
        };
        // ffprobe's estimate (5), and one too low (1): the total is never below what is done,
        // and exact once the input ends (it is read one window ahead, so at piece 4).
        let mut engine = Neighbours::default();
        let (mut voice, mut music) = (Vec::new(), Vec::new());
        for (estimate, want) in [
            (5, [(0, 5), (1, 5), (2, 5), (3, 5), (4, 5), (5, 5)]),
            (1, [(0, 1), (1, 1), (2, 2), (3, 3), (4, 5), (5, 5)]),
        ] {
            engine = Neighbours::default();
            voice.clear();
            music.clear();
            let mut seen = Vec::new();
            let done = run_pieces(
                &mut engine,
                layout,
                Ragged(std::io::Cursor::new(bytes.clone())),
                estimate,
                Instant::now(),
                &mut |v, m| {
                    voice.extend_from_slice(v);
                    music.extend_from_slice(m);
                    Ok(())
                },
                &mut |p| seen.push((p.done, p.total)),
            )
            .unwrap();
            assert_eq!(done, 5, "7 + 7 + 7 + 7 + 2");
            assert_eq!(seen, want, "estimate {estimate}");
        }
        // The windows: the piece and 3 frames on each side, the file's own where it has them.
        let at = |c: usize, t: isize| -> f32 {
            if t < 0 || t >= n as isize {
                0.0
            } else {
                x(c, t as usize)
            }
        };
        assert_eq!(engine.windows.len(), 5);
        for (k, w) in engine.windows.iter().enumerate() {
            let start = (k * 7) as isize - 3;
            let core = if k == 4 { 2 } else { 7 };
            for (c, channel) in w.iter().enumerate() {
                let want: Vec<f32> = (0..core as isize + 6).map(|i| at(c, start + i)).collect();
                assert_eq!(channel, &want, "window {k} channel {c}");
            }
        }
        // The kept samples: each one the stand-in's answer with its real neighbours.
        let decode = |b: &[u8]| -> Vec<f32> {
            b.as_chunks::<4>()
                .0
                .iter()
                .map(|q| f32::from_le_bytes(*q))
                .collect()
        };
        let (voice, music) = (decode(&voice), decode(&music));
        assert_eq!(voice.len(), 2 * n);
        assert_eq!(music.len(), 2 * n);
        for t in 0..n as isize {
            for c in 0..2 {
                let v = 0.5 * (at(c, t - 1) + at(c, t + 1));
                let i = 2 * t as usize + c;
                assert_eq!(voice[i], v, "voice at {t}/{c}");
                assert_eq!(music[i], at(c, t) - v, "music at {t}/{c}");
            }
        }
    }
}

/// LocalFs, but another program takes `name` in the folder the moment the first publish starts
/// (after the names were chosen).
struct TakenMidway {
    fs: crate::remote::LocalFs,
    take: Option<PathBuf>,
}

impl crate::remote::RemoteFs for TakenMidway {
    fn stat_opt(&mut self, path: &str) -> Result<Option<crate::remote::Stat>> {
        self.fs.stat_opt(path)
    }
    fn lstat_opt(&mut self, path: &str) -> Result<Option<crate::remote::Stat>> {
        self.fs.lstat_opt(path)
    }
    fn realpath(&mut self, path: &str) -> Result<String> {
        self.fs.realpath(path)
    }
    fn read_head(&mut self, path: &str, max: usize) -> Result<Vec<u8>> {
        self.fs.read_head(path, max)
    }
    fn list_dir(&mut self, path: &str) -> Result<Vec<String>> {
        self.fs.list_dir(path)
    }
    fn list_entries(&mut self, path: &str) -> Result<Vec<crate::remote::Entry>> {
        self.fs.list_entries(path)
    }
    fn read_to(
        &mut self,
        path: &str,
        out: &mut dyn std::io::Write,
        progress: &mut dyn FnMut(u64),
    ) -> Result<u64> {
        self.fs.read_to(path, out, progress)
    }
    fn create_from(
        &mut self,
        local: &Path,
        path: &str,
        like: Option<crate::remote::Like>,
    ) -> std::result::Result<crate::remote::Uploaded, crate::remote::CreateError> {
        if let Some(name) = self.take.take() {
            std::fs::write(name, "another program's").unwrap();
        }
        self.fs.create_from(local, path, like)
    }
    fn set_mode(&mut self, path: &str, mode: u32) -> Result<()> {
        self.fs.set_mode(path, mode)
    }
    fn rename_over(&mut self, from: &str, to: &str) -> Result<()> {
        self.fs.rename_over(from, to)
    }
    fn link_new(&mut self, from: &str, to: &str) -> Result<()> {
        self.fs.link_new(from, to)
    }
    fn remove(&mut self, path: &str) -> Result<()> {
        self.fs.remove(path)
    }
}

/// The voice's chosen name is taken by another program after the names were chosen: nothing is
/// overwritten, the voice goes to the next free name, the music keeps the chosen one, and the
/// pair returned is what was really published.
#[test]
fn a_name_taken_after_the_choice_is_never_overwritten_and_the_real_pair_is_reported() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("out");
    std::fs::create_dir(&dir).unwrap();
    let (voice_file, music_file) = (tmp.path().join("v.wav"), tmp.path().join("m.wav"));
    std::fs::write(&voice_file, "voice").unwrap();
    std::fs::write(&music_file, "music").unwrap();
    let mut fs = TakenMidway {
        fs: crate::remote::LocalFs::new(),
        take: Some(dir.join("a.voice.wav")),
    };
    let got = publish_pair(
        &mut fs,
        (&voice_file, "a.voice.wav"),
        (&music_file, "a.music.wav"),
        dir.to_str().unwrap(),
    )
    .unwrap();
    assert_eq!(got, ("a.voice-1.wav".into(), "a.music.wav".into()));
    let read = |name: &str| std::fs::read_to_string(dir.join(name)).unwrap();
    assert_eq!(read("a.voice.wav"), "another program's");
    assert_eq!(read("a.voice-1.wav"), "voice");
    assert_eq!(read("a.music.wav"), "music");
}
