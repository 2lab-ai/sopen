use super::*;

fn strs(args: &[OsString]) -> Vec<String> {
    args.iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect()
}

/// One piece, audio: the encoder per container and per source codec (spec §7), every track or
/// the first for a container that holds one; MPEG-TS sought 10 s early; refusals.
#[test]
fn edit_args_pick_the_encoder_and_seek_ts_early() {
    let one = |dst: &str, codec: Option<&str>, tracks: usize| -> String {
        let spec = EditSpec {
            spans: vec![Span::of(1.0, 2.0)],
            fps: None,
            stereo: false,
            crop: None,
            tracks: vec![Track::default(); tracks],
        };
        let args = edit_args(
            &[&Input::path(Path::new("/c/s"))],
            Path::new(dst),
            &spec,
            MediaKind::Audio,
            &ExportPlan::basic(codec),
            Encode::Final,
        )
        .unwrap();
        strs(&args.outputs).join(" ")
    };
    for (dst, codec) in [
        ("a.mp3", "-c:a libmp3lame -q:a 2"),
        ("a.wav", "-c:a pcm_s16le"),
        ("a.flac", "-c:a flac"),
        ("a.m4a", "-c:a aac -b:a 192k"),
        ("a.aac", "-c:a aac -b:a 192k"),
        ("a.ogg", "-c:a libvorbis"),
        ("a.opus", "-c:a libopus"),
    ] {
        assert!(
            one(dst, None, 1).ends_with(&format!("{codec} {dst}")),
            "{dst}"
        );
    }
    for (codec, dst, want) in [
        ("aac", "a.mp4", "-c:a aac -b:a 192k"),
        ("opus", "a.webm", "-c:a libopus"),
        ("vorbis", "a.ogg", "-c:a libvorbis"),
        ("mp3", "a.mkv", "-c:a libmp3lame -q:a 2"),
        ("flac", "a.flac", "-c:a flac"),
        ("pcm_s24le", "a.wav", "-c:a pcm_s24le"),
        ("pcm_f32le", "a.wav", "-c:a pcm_f32le"),
        ("ac3", "a.mp4", "-c:a aac -b:a 192k"),
    ] {
        assert!(
            one(dst, Some(codec), 1).ends_with(&format!("{want} {dst}")),
            "{codec}"
        );
    }
    assert!(one("a.mkv", None, 2).starts_with("-map [ca0] -map [ca1] "));
    // MPEG-TS has no index: an input is sought 10 s early and trimmed after decoding.
    let spec = EditSpec {
        spans: vec![Span::of(12.5, 3.0)],
        fps: Some(25.0),
        stereo: false,
        crop: None,
        tracks: Vec::new(),
    }
    .budget();
    let ts = edit_args(
        &[&Input::path(Path::new("/c/rec.ts"))],
        Path::new("/o/r.ts"),
        &spec,
        MediaKind::Video,
        &ExportPlan::basic(None),
        Encode::Final,
    )
    .unwrap();
    assert_eq!(
        strs(&ts.inputs).join(" "),
        "-ss 2.480000 -t 14.020000 -i /c/rec.ts"
    );
    assert!(ts
        .graph
        .starts_with("[0:v:0]trim=start=10.000000,setpts=PTS-10.020000/TB,"));
    // Refusals: nothing to render, a crop on audio, no audio track, an unknown container.
    let audio = |spans: Vec<Span>, crop: Option<Rect>, tracks: usize, dst: &str| {
        let spec = EditSpec {
            spans,
            fps: None,
            stereo: false,
            crop,
            tracks: vec![Track::default(); tracks],
        };
        edit_args(
            &[&Input::path(Path::new("/c/s"))],
            Path::new(dst),
            &spec,
            MediaKind::Audio,
            &ExportPlan::basic(None),
            Encode::Final,
        )
    };
    let crop = Some(Rect {
        x: 0,
        y: 0,
        w: 2,
        h: 2,
    });
    assert!(audio(vec![], None, 1, "a.mp3").is_err());
    assert!(audio(vec![Span::of(1.0, 0.0)], None, 1, "a.mp3").is_err());
    assert!(audio(vec![Span::of(1.0, 1.0)], crop, 1, "a.mp3").is_err());
    assert!(audio(vec![Span::of(1.0, 1.0)], None, 0, "a.mp3").is_err());
    assert!(audio(vec![Span::of(1.0, 1.0)], None, 1, "a.xyz").is_err());
}

#[test]
fn export_args_table() {
    let src = Path::new("/c/talk.mp4");
    let crop = Some(Rect {
        x: 10,
        y: 20,
        w: 321,
        h: 181,
    });
    let video = strs(
        &export_args(
            src,
            Path::new("/o/talk.cut-1-3.mp4"),
            1.0,
            3.0,
            crop,
            MediaKind::Video,
            &ExportPlan::basic(None),
        )
        .unwrap(),
    );
    assert_eq!(
        video.join(" "),
        "-ss 1.000 -to 3.000 -i /c/talk.mp4 -map 0:v:0 -map 0:a? -vf crop=320:180:10:20 \
         -c:v libx264 -preset veryfast -crf 18 -pix_fmt yuv420p -c:a aac -b:a 192k \
         -movflags +faststart /o/talk.cut-1-3.mp4"
    );
    let mkv = strs(
        &export_args(
            src,
            Path::new("/o/a.mkv"),
            0.5,
            2.25,
            None,
            MediaKind::Video,
            &ExportPlan::basic(None),
        )
        .unwrap(),
    );
    assert_eq!(
        mkv.join(" "),
        "-ss 0.500 -to 2.250 -i /c/talk.mp4 -map 0:v:0 -map 0:a? -c:v libx264 -preset veryfast \
         -crf 18 -pix_fmt yuv420p -c:a aac -b:a 192k /o/a.mkv"
    );

    let audio = [
        ("a.mp3", "-c:a libmp3lame -q:a 2"),
        ("a.wav", "-c:a pcm_s16le"),
        ("a.flac", "-c:a flac"),
        ("a.m4a", "-c:a aac -b:a 192k"),
        ("a.aac", "-c:a aac -b:a 192k"),
        ("a.ogg", "-c:a libvorbis"),
        ("a.opus", "-c:a libopus"),
    ];
    for (dst, codec) in audio {
        // Containers that hold one audio stream get the first; the others get them all.
        let map = if matches!(dst, "a.mp3" | "a.wav" | "a.flac" | "a.aac") {
            "0:a:0"
        } else {
            "0:a"
        };
        let got = strs(
            &export_args(
                Path::new("/c/s"),
                Path::new(dst),
                1.0,
                3.0,
                None,
                MediaKind::Audio,
                &ExportPlan::basic(None),
            )
            .unwrap(),
        );
        assert_eq!(
            got.join(" "),
            format!("-ss 1.000 -to 3.000 -i /c/s -map {map} -vn {codec} {dst}"),
            "{dst}"
        );
    }
    // MPEG-TS: coarse input seek 10 s early, exact trim after decoding.
    let ts = Path::new("/c/rec.ts");
    let late = strs(
        &export_args(
            ts,
            Path::new("/o/r.ts"),
            12.5,
            15.5,
            None,
            MediaKind::Video,
            &ExportPlan::basic(None),
        )
        .unwrap(),
    );
    assert_eq!(
        late[..14].join(" "),
        "-ss 2.500 -i /c/rec.ts -ss 10.000 -to 13.000 -map 0:v:0 -map 0:a? -c:v libx264"
    );
    let early = strs(
        &export_args(
            ts,
            Path::new("/o/r.ts"),
            1.0,
            3.0,
            None,
            MediaKind::Video,
            &ExportPlan::basic(None),
        )
        .unwrap(),
    );
    assert_eq!(
        early[..12].join(" "),
        "-i /c/rec.ts -ss 1.000 -to 3.000 -map 0:v:0 -map 0:a? -c:v libx264"
    );

    let audio = MediaKind::Audio;
    let none = ExportPlan::basic(None);
    assert!(export_args(src, Path::new("a.xyz"), 1.0, 3.0, None, audio, &none).is_err());
    assert!(export_args(src, Path::new("a.mp3"), 1.0, 3.0, crop, audio, &none).is_err());
    let video = MediaKind::Video;
    assert!(export_args(src, Path::new("a.mp4"), 3.0, 3.0, None, video, &none).is_err());

    // The source codec decides the encoder (spec §7), whatever the container.
    let by_codec = [
        ("aac", "a.mp4", "-c:a aac -b:a 192k"),
        ("opus", "a.webm", "-c:a libopus"),
        ("vorbis", "a.ogg", "-c:a libvorbis"),
        ("mp3", "a.mkv", "-c:a libmp3lame -q:a 2"),
        ("flac", "a.flac", "-c:a flac"),
        ("pcm_s24le", "a.wav", "-c:a pcm_s24le"),
        ("pcm_f32le", "a.wav", "-c:a pcm_f32le"),
        ("ac3", "a.mp4", "-c:a aac -b:a 192k"),
    ];
    for (codec, dst, want) in by_codec {
        let map = if matches!(dst, "a.flac" | "a.wav") {
            "0:a:0"
        } else {
            "0:a"
        };
        let got = strs(
            &export_args(
                src,
                Path::new(dst),
                1.0,
                3.0,
                None,
                audio,
                &ExportPlan::basic(Some(codec)),
            )
            .unwrap(),
        );
        assert_eq!(
            got.join(" "),
            format!("-ss 1.000 -to 3.000 -i /c/talk.mp4 -map {map} -vn {want} {dst}"),
            "{codec}"
        );
    }
}

#[test]
fn output_container() {
    let (video, audio) = (MediaKind::Video, MediaKind::Audio);
    assert_eq!(output_ext("WEBM", video, None), "mp4");
    assert_eq!(output_ext("mkv", video, Some("opus")), "mkv");
    assert_eq!(output_ext("webm", audio, None), "webm");
    assert_eq!(output_ext("MP3", audio, None), "mp3");
    // Audio-only files in video containers keep the container when it carries the codec…
    assert_eq!(output_ext("mp4", audio, Some("aac")), "mp4");
    assert_eq!(output_ext("webm", audio, Some("opus")), "webm");
    assert_eq!(output_ext("mkv", audio, Some("pcm_s24le")), "mkv");
    assert_eq!(output_ext("wav", audio, Some("pcm_s24le")), "wav");
    // …and move to the codec's own container when it does not.
    assert_eq!(output_ext("avi", audio, Some("aac")), "m4a");
    assert_eq!(output_ext("mov", audio, Some("opus")), "opus");
    assert_eq!(output_ext("mp4", audio, Some("vorbis")), "ogg");
    assert_eq!(output_ext("wav", audio, Some("mp3")), "mp3");
    assert_eq!(output_ext("mp4", audio, Some("pcm_s16le")), "wav");
    assert_eq!(output_ext("mp4", audio, Some("pcm_s16be")), "mkv");
    // Unknown codec: the container's usual audio codec, or m4a.
    assert_eq!(output_ext("mp4", audio, Some("ac3")), "mp4");
    assert_eq!(output_ext("xyz", audio, None), "m4a");
}

#[test]
fn crop_parse_and_fit() {
    let r: Rect = "10, 20,321,181".parse().unwrap();
    assert_eq!(
        r,
        Rect {
            x: 10,
            y: 20,
            w: 321,
            h: 181
        }
    );
    assert!("1,2,3".parse::<Rect>().is_err());
    assert!("1,2,3,-4".parse::<Rect>().is_err());
    assert_eq!(
        fit_crop(r, 640, 360).unwrap(),
        Rect {
            x: 10,
            y: 20,
            w: 320,
            h: 180
        }
    );
    assert!(fit_crop(r, 320, 360).is_err());
    assert!(fit_crop("0,0,1,1".parse().unwrap(), 640, 360).is_err());
}

#[test]
fn encoders() {
    let listing = "Encoders:\n V..... = Video\n ------\n V....D libx264              H.264\n \
                   A....D aac                  AAC\n A..X.D vorbis               Vorbis\n";
    let set = parse_encoders(listing);
    assert!(set.contains("libx264") && set.contains("aac") && set.contains("vorbis"));
    assert!(!set.contains("="));

    let spec = EditSpec {
        spans: vec![Span::of(1.0, 1.0)],
        fps: None,
        stereo: false,
        crop: None,
        tracks: vec![Track::default()],
    };
    let args = edit_args(
        &[&Input::path(Path::new("s.ogg"))],
        Path::new("d.ogg"),
        &spec,
        MediaKind::Audio,
        &ExportPlan::basic(Some("vorbis")),
        Encode::Final,
    )
    .unwrap()
    .outputs;
    let adapted = strs(&adapt_to_encoders(args.clone(), &set).unwrap());
    assert!(
        adapted.join(" ").contains("-c:a vorbis -strict -2 d.ogg"),
        "{adapted:?}"
    );
    let none = HashSet::new();
    let err = adapt_to_encoders(args, &none).unwrap_err();
    assert!(format!("{err}").contains("libvorbis"), "{err}");
}

#[test]
fn probe_json() {
    let json = r#"{"streams":[
        {"codec_type":"video","width":640,"height":360,"disposition":{"attached_pic":0},
         "avg_frame_rate":"30000/1001","r_frame_rate":"30/1"},
        {"codec_type":"audio","codec_name":"aac"}],"format":{"duration":"4.000000"}}"#;
    assert_eq!(
        parse_probe(json).unwrap(),
        Probe {
            duration: Some(4.0),
            video: Some((640, 360)),
            rotation: 0,
            display: Some((640, 360)),
            fps: Some(30000.0 / 1001.0),
            video_index: 0,
            video_duration: None,
            video_start: None,
            pixel_aspect: None,
            channel_layout: None,
            pix_fmt: None,
            bits: None,
            color: Vec::new(),
            has_audio: true,
            audio_codec: Some("aac".to_string()),
            channels: None,
            sample_rate: None,
            tracks: vec![Track::default()],
            chapters: 0,
            audio_streams: 1,
            dropped: Vec::new(),
        }
    );
    let unknown_avg = r#"{"streams":[{"codec_type":"video","width":2,"height":2,
        "avg_frame_rate":"0/0","r_frame_rate":"25/1"}],"format":{}}"#;
    assert_eq!(parse_probe(unknown_avg).unwrap().fps, Some(25.0));
    let cover = r#"{"streams":[{"codec_type":"audio","codec_name":"mp3"},
        {"codec_type":"video","width":500,"height":500,"disposition":{"attached_pic":1}}],
        "format":{"duration":"2.5"}}"#;
    let cover = parse_probe(cover).unwrap();
    assert_eq!(cover.video, None);
    assert_eq!(cover.display, None);
    assert_eq!(cover.audio_codec.as_deref(), Some("mp3"));
}

#[test]
fn probe_rotation() {
    // Display matrix (what ffmpeg 9 writes and reports), either sign.
    let matrix = |deg: &str| {
        format!(
            r#"{{"streams":[{{"codec_type":"video","width":640,"height":360,
                "side_data_list":[{{"side_data_type":"Display Matrix","rotation":{deg}}}]}}]}}"#
        )
    };
    for (deg, turns, display) in [
        ("90", 90, (360, 640)),
        ("-90", 270, (360, 640)),
        ("180", 180, (640, 360)),
        ("-180", 180, (640, 360)),
        ("270", 270, (360, 640)),
        ("0", 0, (640, 360)),
    ] {
        let probe = parse_probe(&matrix(deg)).unwrap();
        assert_eq!(probe.rotation, turns, "{deg}");
        assert_eq!(probe.video, Some((640, 360)), "{deg}: coded size stays");
        assert_eq!(probe.display, Some(display), "{deg}");
    }
    // The legacy tag (older muxers / ffprobe).
    let tagged = r#"{"streams":[{"codec_type":"video","width":1920,"height":1080,
        "tags":{"rotate":"90"}}]}"#;
    assert_eq!(parse_probe(tagged).unwrap().display, Some((1080, 1920)));
}

#[test]
fn times() {
    let ok = [
        ("3", 3.0),
        ("90", 90.0),
        ("1.25", 1.25),
        ("01:30", 90.0),
        ("1:02:03", 3723.0),
        ("0:00:01.5", 1.5),
    ];
    for (s, want) in ok {
        assert_eq!(parse_time(s).unwrap(), want, "{s}");
    }
    for s in ["", "-1", "1:75", "1:60:00", "a", "1::2", "1:2:3:4", "1e3"] {
        assert!(parse_time(s).is_err(), "{s}");
    }
    assert_eq!(name_time(1.0), "1");
    assert_eq!(name_time(1.5), "1.5");
    assert_eq!(name_time(62.25), "62.25");
    assert_eq!(cut_name("t", 1.0, 3.0, "mp3"), "t.cut-1-3.mp3");
}

#[test]
fn probe_stream_inventory() {
    // Cover art first (it still counts in `0:v:N`), then the picture, two audio tracks, a
    // subtitle and a data stream.
    let json = r#"{"streams":[
        {"codec_type":"video","width":500,"height":500,"disposition":{"attached_pic":1}},
        {"codec_type":"video","width":1920,"height":1080,"pix_fmt":"yuv420p10le",
         "bits_per_raw_sample":"10","color_primaries":"bt2020","color_transfer":"smpte2084",
         "color_space":"bt2020nc","color_range":"unknown","disposition":{"attached_pic":0}},
        {"codec_type":"audio","codec_name":"aac"},
        {"codec_type":"audio","codec_name":"ac3"},
        {"codec_type":"subtitle","codec_name":"mov_text"},
        {"codec_type":"data","codec_name":"bin_data"}],"format":{"duration":"9"}}"#;
    let p = parse_probe(json).unwrap();
    assert_eq!(p.video, Some((1920, 1080)));
    assert_eq!(p.video_index, 1);
    assert_eq!(p.pix_fmt.as_deref(), Some("yuv420p10le"));
    assert_eq!(p.bits, Some(10));
    let color: Vec<(&str, &str)> = p
        .color
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    assert_eq!(
        color,
        [
            ("-color_primaries", "bt2020"),
            ("-color_trc", "smpte2084"),
            ("-colorspace", "bt2020nc")
        ]
    );
    assert_eq!(
        (p.audio_streams, p.audio_codec.as_deref()),
        (2, Some("aac"))
    );
    assert_eq!(p.dropped, ["cover art", "subtitle", "data"]);
}

#[test]
fn export_plan_notes() {
    let probe = parse_probe(
        r#"{"streams":[
        {"codec_type":"video","width":64,"height":64,"pix_fmt":"yuva444p12le"},
        {"codec_type":"audio","codec_name":"aac"},{"codec_type":"audio","codec_name":"aac"},
        {"codec_type":"subtitle"},{"codec_type":"subtitle"},{"codec_type":"attachment"}],
        "format":{"duration":"9"}}"#,
    )
    .unwrap();
    assert!(high_depth(&probe));
    let with_10bit: HashSet<String> = ["yuv420p", "yuv420p10le"].map(String::from).into();
    let plan = ExportPlan::new(&probe, MediaKind::Video, "mkv", Some(&with_10bit));
    assert_eq!(plan.pix_fmt, "yuv420p10le");
    assert_eq!(
        plan.notes,
        [
            "not kept: 2 subtitle streams, 1 attachment stream",
            "exported as 10-bit (source is 12-bit)",
            "alpha is dropped (H.264 has no alpha)"
        ]
    );
    let eight: HashSet<String> = ["yuv420p".to_string()].into();
    let plan = ExportPlan::new(&probe, MediaKind::Video, "mkv", Some(&eight));
    assert_eq!(plan.pix_fmt, "yuv420p");
    assert!(plan
        .notes
        .iter()
        .any(|n| n.starts_with("exported as 8-bit")));
    // An audio export into a one-track container names the tracks it leaves out.
    let audio = parse_probe(
        r#"{"streams":[{"codec_type":"audio","codec_name":"mp3"},
        {"codec_type":"audio","codec_name":"mp3"},
        {"codec_type":"video","width":9,"height":9,"disposition":{"attached_pic":1}}],
        "format":{"duration":"9"}}"#,
    )
    .unwrap();
    let plan = ExportPlan::new(&audio, MediaKind::Audio, "mp3", None);
    assert_eq!(
        plan.notes,
        ["not kept: cover art, 1 more audio stream (.mp3 holds one)"]
    );
    // An ordinary 8-bit file: nothing to say.
    let plain = parse_probe(
        r#"{"streams":[{"codec_type":"video","width":9,"height":9,"pix_fmt":"yuv420p"},
        {"codec_type":"audio","codec_name":"aac"}],"format":{"duration":"9"}}"#,
    )
    .unwrap();
    let plan = ExportPlan::new(&plain, MediaKind::Video, "mp4", None);
    assert!(plan.notes.is_empty(), "{:?}", plan.notes);
}

#[test]
fn pixel_formats() {
    let help =
        "Encoder libx264 [libx264 H.264 / AVC]:\n    General capabilities: delay threads\n    \
                Supported pixel formats: yuv420p yuvj420p yuv422p yuv420p10le gray10le\n";
    let set = parse_pix_fmts(help);
    assert!(set.contains("yuv420p10le") && set.contains("gray10le") && set.len() == 5);
    assert!(parse_pix_fmts("no such line").is_empty());
    for alpha in [
        "yuva420p",
        "rgba",
        "argb",
        "bgra",
        "abgr",
        "gbrap",
        "gbrap10le",
        "ya8",
        "rgba64le",
    ] {
        assert!(has_alpha(alpha), "{alpha}");
    }
    for plain in [
        "yuv420p",
        "gbrp",
        "rgb24",
        "nv12",
        "gray",
        "yuv444p10le",
        "pal8",
    ] {
        assert!(!has_alpha(plain), "{plain}");
    }
    let depth = |fmt: &str| {
        high_depth(&Probe {
            pix_fmt: Some(fmt.to_string()),
            ..parse_probe(r#"{"streams":[]}"#).unwrap()
        })
    };
    for deep in [
        "yuv420p10le",
        "p010le",
        "yuv444p12le",
        "gray16be",
        "rgb48le",
        "gbrp10le",
    ] {
        assert!(depth(deep), "{deep}");
    }
    for eight in [
        "yuv420p", "nv12", "rgb24", "yuvj420p", "bgra", "gray", "yuv422p",
    ] {
        assert!(!depth(eight), "{eight}");
    }
}

/// Fewer bits out than in is named: 12/16-bit → 10-bit, deeper than 8 → 8-bit where libx264
/// has no 10-bit; a 10-bit source exported as 10-bit says nothing.
#[test]
fn depth_reduction_notes() {
    let video = |pix_fmt: &str, bits: Option<&str>| {
        let bits = bits.map_or(String::new(), |b| {
            format!(r#","bits_per_raw_sample":"{b}""#)
        });
        parse_probe(&format!(
            r#"{{"streams":[{{"codec_type":"video","width":8,"height":8,"pix_fmt":"{pix_fmt}"{bits}}}]}}"#
        ))
        .unwrap()
    };
    let ten: HashSet<String> = ["yuv420p", "yuv420p10le"].map(String::from).into();
    let eight: HashSet<String> = ["yuv420p".to_string()].into();
    let notes = |probe: &Probe, formats: &HashSet<String>| {
        ExportPlan::new(probe, MediaKind::Video, "mkv", Some(formats)).notes
    };
    let p12 = video("yuv422p12le", Some("12"));
    assert_eq!(notes(&p12, &ten), ["exported as 10-bit (source is 12-bit)"]);
    let p16 = video("rgb48le", None);
    assert_eq!(source_bits(&p16), Some(16));
    assert_eq!(notes(&p16, &ten), ["exported as 10-bit (source is 16-bit)"]);
    assert_eq!(
        notes(&p12, &eight),
        ["exported as 8-bit (source is 12-bit; this ffmpeg's libx264 has no 10-bit)"]
    );
    let p10 = video("yuv420p10le", Some("10"));
    assert!(notes(&p10, &ten).is_empty());
    assert_eq!(
        notes(&p10, &eight),
        ["exported as 8-bit (source is 10-bit; this ffmpeg's libx264 has no 10-bit)"]
    );
    assert!(notes(&video("yuv420p", Some("8")), &eight).is_empty());
    for (fmt, bits) in [
        ("p210le", 10),
        ("gray9be", 9),
        ("yuv444p14le", 14),
        ("gbrpf32le", 32),
    ] {
        assert_eq!(source_bits(&video(fmt, None)), Some(bits), "{fmt}");
    }
}

/// Every audio track's channels and rate, and the chapter count, come from ffprobe; the first
/// track's are also the file's.
#[test]
fn probe_reads_each_audio_track_and_the_chapters() {
    let json = r#"{"streams":[
        {"codec_type":"audio","codec_name":"aac","channels":1,"sample_rate":"48000"},
        {"codec_type":"audio","codec_name":"aac","channels":2,"sample_rate":"44100"}],
        "chapters":[{"id":0},{"id":1}],"format":{"duration":"2.0"}}"#;
    let p = parse_probe(json).unwrap();
    assert_eq!((p.channels, p.sample_rate), (Some(1), Some(48_000)));
    assert_eq!(
        p.tracks,
        [
            Track {
                channels: Some(1),
                rate: Some(48_000),
                format: None
            },
            Track {
                channels: Some(2),
                rate: Some(44_100),
                format: None
            }
        ]
    );
    assert_eq!(p.chapters, 2);
}

fn span(file: usize, start: f64, len: f64) -> Span {
    Span {
        file,
        ..Span::of(start, len)
    }
}

fn edit(spans: Vec<Span>, tracks: Vec<Track>) -> EditSpec {
    EditSpec {
        spans,
        fps: Some(25.0),
        stereo: false,
        crop: None,
        tracks,
    }
}

fn track(channels: u32, rate: u32, format: &str) -> Track {
    Track {
        channels: Some(channels),
        rate: Some(rate),
        format: Some(format.to_string()),
    }
}

/// One grid over the whole output: no rounding builds up, the total is the rounded whole, and
/// a piece shorter than a step gets none rather than being stretched to one.
#[test]
fn grid_counts_do_not_accumulate_rounding() {
    // 1.5 frames each at 25 fps: 1, 2, 1, 2, … — not 2, 2, 2 (stretched) or 1, 1, 1 (lost).
    let lens = vec![1.5 / 25.0; 8];
    assert_eq!(grid_counts(&lens, 25.0), [2, 1, 2, 1, 2, 1, 2, 1]);
    assert_eq!(grid_counts(&lens, 25.0).iter().sum::<u64>(), 12);
    // A frame at 30000/1001 fps is 1601.6 samples at 48 kHz: a hundred one-frame pieces are
    // exactly round(100 × 1601.6) samples, each 1601 or 1602.
    let fps = 30000.0 / 1001.0;
    let counts = grid_counts(&vec![1.0 / fps; 100], 48_000.0);
    assert_eq!(counts.iter().sum::<u64>(), 160_160);
    assert!(
        counts.iter().all(|n| *n == 1601 || *n == 1602),
        "{counts:?}"
    );
    // A play position that leaves a third of a frame of its first piece: no frame for it.
    assert_eq!(grid_counts(&[0.3 / 25.0, 1.0], 25.0), [0, 25]);
    assert_eq!(grid_counts(&[0.7 / 25.0, 1.0], 25.0), [1, 25]);
}

/// The render's graph for a cut-and-paste of a video with one audio track: pieces in source
/// order share an input (one seek, `split`), the piece out of order gets its own; each stream
/// of a piece is cut at the piece's start and moved by that same amount (its offsets kept,
/// a late start filled), video on the frame grid; each is counted out in frames and samples;
/// picture and sound are joined by a `concat` each; chapters are dropped; the crop is applied
/// once, after `concat`.
#[test]
fn edit_args_share_inputs_keep_offsets_and_count_frames_and_samples() {
    // Output: [1, 2) then [0, 1) then [2, 4) of the source.
    let mut spec = edit(
        vec![span(0, 1.0, 1.0), span(0, 0.0, 1.0), span(0, 2.0, 2.0)],
        vec![track(2, 48_000, "fltp")],
    )
    .budget();
    spec.crop = Some(Rect {
        x: 2,
        y: 4,
        w: 31,
        h: 17,
    });
    let args = edit_args(
        &[&Input::path(Path::new("/c/v.mp4"))],
        Path::new("/o/v.mp4"),
        &spec,
        MediaKind::Video,
        &ExportPlan::basic(None),
        Encode::Final,
    )
    .unwrap();
    assert_eq!(
        strs(&args.inputs).join(" "),
        "-ss 0.780000 -t 2.220000 -i /c/v.mp4 -t 5.000000 -i /c/v.mp4",
        "[1,2) alone; [0,1) and [2,4) share the second input (0..4 s, no seek at 0)"
    );
    let graph: Vec<&str> = args.graph.split(';').collect();
    let v = |n: u32| {
        format!(
            ",fps=fps=25/1:start_time=0,trim=end_frame={n},tpad=stop={n}:stop_mode=clone,\
             trim=end_frame={n},setpts=PTS-STARTPTS"
        )
    };
    let a = |n: u32| {
        format!(
            ",aresample=first_pts=0,atrim=end_sample={n},apad=whole_len={n},\
             asetpts=PTS-STARTPTS"
        )
    };
    assert_eq!(
        graph,
        [
            format!(
                "[0:v:0]trim=start=0.200000,setpts=PTS-0.220000/TB{}[v0]",
                v(25)
            ),
            format!(
                "[0:a:0]atrim=start=0.220000,asetpts=PTS-0.220000/TB{}[a0_0]",
                a(48000)
            ),
            "[1:v:0]split=2[g1v0][g1v1]".to_string(),
            format!(
                "[g1v0]trim=start=0.000000,setpts=PTS-0.000000/TB{}[v1]",
                v(25)
            ),
            format!(
                "[g1v1]trim=start=1.980000,setpts=PTS-2.000000/TB{}[v2]",
                v(50)
            ),
            "[1:a:0]asplit=2[g1a0_0][g1a0_1]".to_string(),
            format!(
                "[g1a0_0]atrim=start=0.000000,asetpts=PTS-0.000000/TB{}[a1_0]",
                a(48000)
            ),
            format!(
                "[g1a0_1]atrim=start=2.000000,asetpts=PTS-2.000000/TB{}[a2_0]",
                a(96000)
            ),
            "[v0][v1][v2]concat=n=3:v=1:a=0,setpts=N/(25/1)/TB[cv]".to_string(),
            "[a0_0][a1_0][a2_0]concat=n=3:v=0:a=1[ca0]".to_string(),
            "[cv]crop=30:16:2:4[vo]".to_string(),
        ]
    );
    assert_eq!(
        strs(&args.outputs).join(" "),
        "-map [vo] -map [ca0] -map_chapters -1 -map_metadata:s:a:0 0:s:a:0 -c:v libx264 \
         -preset veryfast -crf 18 -pix_fmt yuv420p -c:a aac -b:a 192k -movflags +faststart \
         /o/v.mp4"
    );
}

/// Each audio track is counted at its own rate; Mono → Stereo pans only the mono tracks; an
/// audio file keeps its codec; an intermediate keeps each track in its own sample format.
#[test]
fn edit_args_count_each_track_at_its_rate_and_pan_only_mono_tracks() {
    let tracks = vec![track(1, 48_000, "s32"), track(2, 44_100, "fltp")];
    let mut spec = edit(vec![span(0, 0.5, 0.25)], tracks).budget();
    spec.fps = None;
    spec.stereo = true;
    let args = edit_args(
        &[&Input::path(Path::new("/c/a.mka"))],
        Path::new("/o/a.mka"),
        &spec,
        MediaKind::Audio,
        &ExportPlan::basic(Some("flac")),
        Encode::Final,
    )
    .unwrap();
    for n in ["12000", "11025"] {
        let count = format!("atrim=end_sample={n},");
        assert!(args.graph.contains(&count), "{}", args.graph);
    }
    assert!(args.graph.contains("[ca0]pan=stereo|c0=c0|c1=c0[so0]"));
    assert!(
        !args.graph.contains("[ca1]pan"),
        "a stereo track is left alone"
    );
    assert_eq!(
        strs(&args.outputs).join(" "),
        "-map [so0] -map [ca1] -map_chapters -1 -map_metadata:s:a:0 0:s:a:0 \
         -map_metadata:s:a:1 0:s:a:1 -c:a flac /o/a.mka"
    );
    // The intermediate pass keeps every channel as it is and does not crop or pan.
    let lossless = edit_args(
        &[&Input::path(Path::new("/c/a.mka"))],
        Path::new("/w/p.mkv"),
        &spec,
        MediaKind::Audio,
        &ExportPlan::basic(Some("flac")),
        Encode::Lossless,
    )
    .unwrap();
    assert!(!lossless.graph.contains("pan="));
    assert_eq!(
        strs(&lossless.outputs).join(" "),
        "-map [ca0] -map [ca1] -map_chapters -1 -map_metadata:s:a:0 0:s:a:0 \
         -map_metadata:s:a:1 0:s:a:1 -c:a:0 pcm_s32le -c:a:1 pcm_f32le -f matroska /w/p.mkv"
    );
}

/// The intermediate PCM per sample format: the format's own width, integer or float.
#[test]
fn intermediates_hold_each_sample_format_exactly() {
    for (format, want) in [
        ("u8", "pcm_s16le"),
        ("s16", "pcm_s16le"),
        ("s16p", "pcm_s16le"),
        ("s32", "pcm_s32le"),
        ("s32p", "pcm_s32le"),
        ("s64", "pcm_s64le"),
        ("flt", "pcm_f32le"),
        ("fltp", "pcm_f32le"),
        ("dbl", "pcm_f64le"),
        ("dblp", "pcm_f64le"),
    ] {
        assert_eq!(track(2, 48_000, format).lossless_pcm(), want, "{format}");
    }
    assert_eq!(Track::default().lossless_pcm(), "pcm_f32le");
}

/// More separate stretches than one render may open: lossless passes of at most `max` inputs
/// each, then a final pass over their files in order whose pieces are those files whole, their
/// frames and samples the sums of the grid's counts — what one pass would count out.
#[test]
fn edit_passes_split_a_long_out_of_order_edit() {
    let max = 4;
    let n = max * 2 + 3;
    // Every piece out of order (each needs an input of its own), 0.41 s: 10.25 frames.
    let spans: Vec<Span> = (0..n)
        .map(|k| span(0, (n - k) as f64 * 20.0, 0.41))
        .collect();
    let spec = edit(spans.clone(), vec![track(2, 44_100, "s16")]);
    assert_eq!(spec.groups().len(), n);
    let src = Input::path(Path::new("/c/src.mp4"));
    let passes = edit_passes(&spec, &src, max).unwrap();
    assert_eq!(passes.len(), 4, "three lossless passes and the final one");
    let budgeted = spec.clone().budget();
    let first: Vec<Span> = passes[..3].iter().flat_map(|p| p.spans.clone()).collect();
    assert_eq!(
        first, budgeted.spans,
        "every piece once, in order, as budgeted"
    );
    assert!(passes[..3].iter().all(|p| p.groups().len() <= max));
    let last = &passes[3];
    assert_eq!(
        last.spans.iter().map(|s| s.file).collect::<Vec<_>>(),
        [1, 2, 3]
    );
    assert!(last.spans.iter().all(|s| s.start == 0.0));
    let frames: Option<u64> = last.spans.iter().map(|s| s.frames).sum();
    let samples: u64 = last.spans.iter().map(|s| s.samples[0]).sum();
    // 11 × 0.41 s = 4.51 s: 112.75 → 113 frames, 198 891 samples — the one-pass totals.
    assert_eq!(frames, Some(113));
    assert_eq!(samples, 198_891);
    assert_eq!(
        budgeted.spans.iter().map(|s| s.samples[0]).sum::<u64>(),
        samples
    );
    assert!((last.length() - spec.length()).abs() < 1e-9);
    // One that fits is one pass (budgeted).
    let small = edit(spans[..3].to_vec(), Vec::new());
    assert_eq!(edit_passes(&small, &src, max).unwrap(), [small.budget()]);
}

/// The stall rule on explicit instants: progress lines that repeat the same output position
/// (ffmpeg 9 prints one every half second while stuck) do not reset it; an advance does; the
/// time the caller's own callback takes does not count.
#[test]
fn watchdog_counts_only_time_without_an_advance() {
    use std::time::{Duration, Instant};
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let mut dog = Watchdog::new(Duration::from_millis(1000), t0);
    assert!(
        !dog.line("frame=0", at(10)),
        "a first frame=0 is not a step"
    );
    assert!(dog.line("out_time_us=40000", at(20)));
    for k in 1..=9 {
        assert!(!dog.line("out_time_us=40000", at(20 + k * 100)), "repeated");
        assert!(!dog.line("progress=continue", at(20 + k * 100)));
    }
    assert!(!dog.stalled(at(1000)), "980 ms without an advance");
    assert!(dog.stalled(at(1030)), "past the limit");
    // An advance resets it.
    let mut dog = Watchdog::new(Duration::from_millis(1000), t0);
    dog.line("out_time_us=1", at(900));
    assert!(dog.line("total_size=4096", at(950)));
    assert!(!dog.stalled(at(1900)));
    // The callback slept 5 s (the export-hold hook): not ffmpeg's stall.
    let mut dog = Watchdog::new(Duration::from_millis(1000), t0);
    dog.line("out_time_us=5", at(100));
    dog.resume(at(5100));
    assert!(!dog.stalled(at(5600)));
    assert!(dog.stalled(at(6200)));
}

#[test]
fn ffmpeg_major_versions() {
    assert_eq!(
        parse_major("ffmpeg version 9.0.2 Copyright (c) 2000-2026"),
        Some(9)
    );
    assert_eq!(parse_major("ffmpeg version n7.1 Copyright"), Some(7));
    assert_eq!(
        parse_major("ffmpeg version 6.1.1-3ubuntu5 Copyright"),
        Some(6)
    );
    assert_eq!(
        parse_major("ffmpeg version N-118000-gabc Copyright"),
        Some(u32::MAX)
    );
    assert_eq!(parse_major("nothing here"), None);
}

/// The one input value: a path or a URL, its extension from the last segment (a query left
/// out); only a path is local.
#[test]
fn inputs_are_paths_or_urls() {
    let p = Input::path(Path::new("/c/Talk.MP4"));
    assert_eq!(
        (p.ext(), p.local()),
        ("mp4".to_string(), Some(Path::new("/c/Talk.MP4")))
    );
    let u = Input::path(Path::new("http://127.0.0.1:9/tok/dir/clip.ts?range=1#x"));
    assert_eq!((u.ext(), u.local()), ("ts".to_string(), None));
    assert_eq!(Input::path(Path::new("/c/README")).ext(), "");
    // A local name keeps its `?` and `#`: they are not a query.
    assert_eq!(Input::path(Path::new("/c/clip#1.ts")).ext(), "ts");
    assert_eq!(Input::path(Path::new("/c/what?.MP4")).ext(), "mp4");
}

#[test]
fn stall_messages() {
    use std::time::Duration;
    assert_eq!(
        stall_message(Duration::from_secs(120), Some("4500000")),
        "ffmpeg made no progress for 120 s (stuck at 4.50 s of output) and was stopped"
    );
    assert_eq!(
        stall_message(Duration::from_millis(300), None),
        "ffmpeg made no progress for 0.3 s and was stopped"
    );
}

#[test]
fn frame_rates_are_given_as_fractions() {
    assert_eq!(rate_arg(30000.0 / 1001.0), "30000/1001");
    assert_eq!(rate_arg(25.0), "25/1");
    assert_eq!(rate_arg(125.0 / 11.0), "125/11");
    assert_eq!(rate_arg(24000.0 / 1001.0), "24000/1001");
    assert_eq!(rate_arg(std::f64::consts::PI), "3.141592654");
}

/// The picture stream's own length: its `duration`, or Matroska's `DURATION` tag.
#[test]
fn probe_reads_the_picture_stream_s_own_length() {
    let mp4 = r#"{"streams":[{"codec_type":"video","width":2,"height":2,"duration":"4.000000",
        "avg_frame_rate":"25/1"},{"codec_type":"audio"}],"format":{"duration":"5.0"}}"#;
    assert_eq!(parse_probe(mp4).unwrap().video_duration, Some(4.0));
    let mkv = r#"{"streams":[{"codec_type":"video","width":2,"height":2,
        "tags":{"DURATION":"00:00:04.040000000"}}],"format":{"duration":"5.0"}}"#;
    assert_eq!(parse_probe(mkv).unwrap().video_duration, Some(4.04));
}

/// Where the picture's frames sit in the frame period: a transport stream whose sound starts
/// 21.3 ms earlier (AAC priming) has its frames 21.3 ms into each 40 ms period.
#[test]
fn frame_origin_is_the_picture_s_offset_within_a_frame() {
    let ts = r#"{"streams":[{"codec_type":"video","width":2,"height":2,"start_time":"1.621333",
        "avg_frame_rate":"25/1"},{"codec_type":"audio","start_time":"1.378667"}],
        "format":{"start_time":"1.378667","duration":"4.0"}}"#;
    let p = parse_probe(ts).unwrap();
    assert!((p.video_start.unwrap() - 0.242666).abs() < 1e-6);
    assert!(
        (p.frame_origin() - 0.002666).abs() < 1e-6,
        "{}",
        p.frame_origin()
    );
    let plain = r#"{"streams":[{"codec_type":"video","width":2,"height":2,"start_time":"0.000000",
        "avg_frame_rate":"25/1"}],"format":{"start_time":"0.000000"}}"#;
    assert_eq!(parse_probe(plain).unwrap().frame_origin(), 0.0);
    let whole = r#"{"streams":[{"codec_type":"video","width":2,"height":2,"start_time":"0.2",
        "avg_frame_rate":"25/1"}],"format":{"start_time":"0"}}"#;
    assert!(
        parse_probe(whole).unwrap().frame_origin() < 1e-6,
        "0.2 s is five periods"
    );
}

/// A child's error output is read to its end — 3 MB of empty lines, then a 3 MB line that never
/// ends, then its last line — and only a bounded tail is kept: the endless line cut to its first
/// bytes at a character's edge and marked, the last line whole; the child, never blocked on its
/// pipe, exits.
#[test]
fn an_error_tail_drains_everything_and_keeps_only_a_bounded_end() {
    let mut child = Command::new("sh")
        .arg("-c")
        .arg(
            "head -c 3000000 /dev/zero | tr '\\0' '\\n' >&2; \
             head -c 3000000 /dev/zero | tr '\\0' x >&2; printf '\\né the end' >&2",
        )
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let tail = ErrorTail::spawn(child.stderr.take().unwrap());
    assert!(child.wait().unwrap().success(), "it ran to its end");
    let tail = tail.join().unwrap();
    assert!(tail.kept() <= 2048, "{} bytes kept", tail.kept());
    assert_eq!(tail.last_line(), "é the end");
    // A last line that never ends: its first bytes, marked.
    let mut bytes = b"first\n".to_vec();
    bytes.extend([b'x'; 3000]);
    let last = ErrorTail::read(std::io::Cursor::new(bytes)).last_line();
    assert_eq!(last.len(), 2048 + '…'.len_utf8());
    assert!(last.ends_with("xx…"), "{last}");
    // A line cut inside a character (two bytes each, then one): at the edge before it.
    let mut bytes = "é".repeat(2048).into_bytes();
    bytes.insert(0, b'!');
    let last = ErrorTail::read(std::io::Cursor::new(bytes)).last_line();
    assert!(last.starts_with("!éé") && last.ends_with("éé…"), "{last}");
    assert_eq!(last.len(), 2047 + '…'.len_utf8());
    assert!(!last.contains('\u{FFFD}'));
}

/// [`whole_path_as`], the one place another program's text is rewritten: the copy's path
/// becomes the name the owner knows only where it stands as a whole path — after the start,
/// white space, a quote or `(`, and before the end, `:`, a quote, `)` or `,`.
#[test]
fn another_programs_text_names_the_file_only_where_the_whole_path_stands() {
    let (copy, known) = ("/c/box/data/a.mp4", "box:/data/a.mp4");
    let as_known = |said: &str| whole_path_as(said, copy, known);
    for (said, want) in [
        ("/c/box/data/a.mp4", "box:/data/a.mp4"),
        (
            "/c/box/data/a.mp4: Invalid data found",
            "box:/data/a.mp4: Invalid data found",
        ),
        (
            "Error opening input file /c/box/data/a.mp4.",
            "Error opening input file /c/box/data/a.mp4.",
        ),
        (
            "moov atom not found in '/c/box/data/a.mp4', giving up",
            "moov atom not found in 'box:/data/a.mp4', giving up",
        ),
        (
            "cannot open \"/c/box/data/a.mp4\" (/c/box/data/a.mp4)",
            "cannot open \"box:/data/a.mp4\" (box:/data/a.mp4)",
        ),
        // Twice in one line: both.
        (
            "/c/box/data/a.mp4: bad header, /c/box/data/a.mp4: giving up",
            "box:/data/a.mp4: bad header, box:/data/a.mp4: giving up",
        ),
        // Twice with nothing between them is not two whole paths.
        (
            "/c/box/data/a.mp4/c/box/data/a.mp4",
            "/c/box/data/a.mp4/c/box/data/a.mp4",
        ),
    ] {
        assert_eq!(as_known(said), want, "{said}");
    }
    // A different, longer path: more components before it, or more of a name after it.
    for other in [
        "cannot read /backup/c/box/data/a.mp4: gone",
        "cannot read ../c/box/data/a.mp4: gone",
        "cannot read /c/box/data/a.mp4+1: gone",
        "cannot read /c/box/data/a.mp4@2: gone",
        "cannot read /c/box/data/a.mp4 2: gone",
        "cannot read /c/box/data/a.mp4.sopen-backup-1: gone",
        "cannot read /c/box/data/a.mp4-1: gone",
        "cannot read /c/box/data/a.mp4/part: gone",
        "cannot read /c/box/data/a.mp4~: gone",
    ] {
        assert_eq!(as_known(other), other);
    }
    // Nothing in either name is a pattern: they are text.
    let (copy, known) = (
        "/c/b.x/[a]+(1)*$^?/a|b{2}.mp4",
        "b.x:/[a]+(1)*$^?/$0 \\1 a|b{2}.mp4",
    );
    assert_eq!(
        whole_path_as(&format!("{copy}: Invalid data, see '{copy}'"), copy, known),
        format!("{known}: Invalid data, see '{known}'")
    );
    assert_eq!(
        whole_path_as("/c/bXx/[a]+(1)*$^?/a|b{2}.mp4: no", copy, known),
        "/c/bXx/[a]+(1)*$^?/a|b{2}.mp4: no",
        "the dot is a dot"
    );
    // The same name, or no path: as printed.
    assert_eq!(whole_path_as("/x: no", "/x", "/x"), "/x: no");
    assert_eq!(whole_path_as("/x: no", "", "box:/x"), "/x: no");
}

/// ffprobe's refusal of a remote file's copy: sopen's own words name the file as its owner
/// knows it, directly, and ffprobe's line loses the copy's path wherever it stood whole.
#[test]
fn a_probe_refusal_of_a_copy_names_the_file() {
    let copy = Path::new("/c/box/data/a.mp4");
    assert_eq!(
        probe_refusal_shown(
            copy,
            "box:/data/a.mp4",
            "/c/box/data/a.mp4: Invalid data found"
        ),
        "ffprobe cannot read box:/data/a.mp4: Invalid data found"
    );
    assert_eq!(
        probe_refusal_shown(
            copy,
            "box:/data/a.mp4",
            "'/c/box/data/a.mp4': no such stream"
        ),
        "ffprobe cannot read box:/data/a.mp4: 'box:/data/a.mp4': no such stream"
    );
    assert_eq!(
        probe_refusal(copy, "/c/box/data/a.mp4: Invalid data found"),
        "ffprobe cannot read /c/box/data/a.mp4: Invalid data found"
    );
}

/// An mp3 render takes every piece from one input, decoded through once from the first piece's
/// place, whatever the pieces' order (06 Tensions, settled by W7); another file keeps an input
/// per run of pieces. The one input holds only while the pieces it gives before they are wanted
/// come to at most [`HOLD_MAX`] of decoded sound.
#[test]
fn an_mp3_edit_takes_its_pieces_from_one_input_while_what_it_holds_is_bounded() {
    let stereo = vec![track(2, 44_100, "fltp")];
    // Output: [300, 310) then [100, 110) then [500, 505): every piece out of order.
    let spans = vec![
        span(0, 300.0, 10.0),
        span(0, 100.0, 10.0),
        span(0, 500.0, 5.0),
    ];
    let spec = EditSpec {
        fps: None,
        ..edit(spans, stereo.clone())
    };
    let (mp3, wav) = (
        Input::path(Path::new("/c/s.mp3")),
        Input::path(Path::new("/c/s.wav")),
    );
    assert_eq!(spec.groups_from(&mp3), vec![vec![0, 1, 2]]);
    assert_eq!(spec.groups_from(&wav), spec.groups());
    assert_eq!(spec.groups().len(), 3);
    // Decoded in the file's order, [100, 110) is given first and waits for [300, 310): 10 s of
    // 44.1 kHz stereo floats.
    assert_eq!(spec.held(), 10 * 44_100 * 2 * 4);
    let args = edit_args(
        &[&mp3],
        Path::new("/c/out.mp3"),
        &spec.clone().budget(),
        MediaKind::Audio,
        &ExportPlan::basic(Some("mp3")),
        Encode::Final,
    )
    .unwrap();
    let text: Vec<String> = args
        .inputs
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    // One input, from just before its first piece in the file to past its last one.
    assert_eq!(text.iter().filter(|a| *a == "-i").count(), 1, "{text:?}");
    assert_eq!(text[..2], ["-ss", "99.800000"], "{text:?}");
    assert_eq!(text[2..4], ["-t", "406.200000"], "{text:?}");
    assert!(args.graph.contains("asplit=3"), "{}", args.graph);
    // Pieces held for longer than the bound: an input each again.
    let long = EditSpec {
        spans: vec![span(0, 2000.0, 600.0), span(0, 0.0, 600.0)],
        fps: None,
        ..edit(Vec::new(), stereo)
    };
    assert!(long.held() > HOLD_MAX);
    assert_eq!(long.groups_from(&mp3).len(), 2);
    // One piece is one input whatever the file.
    let one = EditSpec {
        spans: vec![span(0, 10.0, 5.0)],
        fps: None,
        ..edit(Vec::new(), Vec::new())
    };
    assert_eq!(one.groups_from(&mp3), one.groups());
}

/// A piece pasted right after itself, or two pieces over the same stretch, are decoded from one
/// input at once: the later one waits whole while the earlier one plays. [`EditSpec::held`]
/// counts that wait, so such an edit shares one input only below [`HOLD_MAX`].
#[test]
fn repeated_or_overlapping_mp3_pieces_share_one_input_only_below_the_bound() {
    let stereo = vec![track(2, 44_100, "fltp")];
    let mp3 = Input::path(Path::new("/c/s.mp3"));
    let spec = |ranges: &[(f64, f64)]| EditSpec {
        spans: ranges.iter().map(|&(a, b)| span(0, a, b - a)).collect(),
        fps: None,
        ..edit(Vec::new(), stereo.clone())
    };
    let per_s = 44_100 * 2 * 4;
    // Pasted after itself: the copy waits whole.
    let repeated = spec(&[(0.0, 600.0), (0.0, 600.0)]);
    assert_eq!(repeated.held(), 600 * per_s);
    assert!(repeated.held() > HOLD_MAX);
    assert_eq!(
        repeated.groups_from(&mp3),
        repeated.groups(),
        "past the bound: an input each"
    );
    assert_eq!(repeated.groups().len(), 2);
    let short = spec(&[(0.0, 60.0), (0.0, 60.0)]);
    assert_eq!(short.held(), 60 * per_s);
    assert_eq!(
        short.groups_from(&mp3),
        vec![vec![0, 1]],
        "below it: one input"
    );
    // Overlapping by 600 s: what the second has been given by the first's end waits.
    let overlap = spec(&[(0.0, 800.0), (200.0, 1000.0)]);
    assert_eq!(overlap.held(), 600 * per_s);
    assert_eq!(overlap.groups_from(&mp3).len(), 2);
    let small = spec(&[(0.0, 100.0), (50.0, 150.0)]);
    assert_eq!(small.held(), 50 * per_s);
    assert_eq!(small.groups_from(&mp3), vec![vec![0, 1]]);
    // The order the output takes them in decides: the later stretch first holds the earlier.
    let swapped = spec(&[(50.0, 150.0), (0.0, 100.0)]);
    assert_eq!(swapped.held(), 100 * per_s);
}
