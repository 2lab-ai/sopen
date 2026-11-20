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
