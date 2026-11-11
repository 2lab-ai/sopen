use super::*;

fn strs(args: &[OsString]) -> Vec<String> {
    args.iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect()
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

    let args = export_args(
        Path::new("s.ogg"),
        Path::new("d.ogg"),
        1.0,
        2.0,
        None,
        MediaKind::Audio,
        &ExportPlan::basic(Some("vorbis")),
    )
    .unwrap();
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
            pix_fmt: None,
            bits: None,
            color: Vec::new(),
            has_audio: true,
            audio_codec: Some("aac".to_string()),
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
