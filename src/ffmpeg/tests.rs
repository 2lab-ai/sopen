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
                rate: Some(48_000)
            },
            Track {
                channels: Some(2),
                rate: Some(44_100)
            }
        ]
    );
    assert_eq!(p.chapters, 2);
}

fn span(file: usize, start: f64, len: f64, frames: Option<u64>) -> Span {
    Span {
        file,
        start,
        len,
        frames,
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

/// The render's graph for a cut-and-paste of a video with one audio track: pieces in source
/// order share an input (one seek, `split`), the piece out of order gets its own; each piece
/// is cut half a frame early and counted out in frames and samples; `concat` joins them in
/// output order; chapters are dropped; the crop is applied once, after `concat`.
#[test]
fn edit_args_share_inputs_and_count_frames_and_samples() {
    let track = Track {
        channels: Some(2),
        rate: Some(48_000),
    };
    // Output: [1, 2) then [0, 1) then [2, 4) of the source.
    let mut spec = edit(
        vec![
            span(0, 1.0, 1.0, Some(25)),
            span(0, 0.0, 1.0, Some(25)),
            span(0, 2.0, 2.0, Some(50)),
        ],
        vec![track],
    );
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
    assert_eq!(
        graph,
        [
            "[0:v:0]trim=start=0.200000,setpts=PTS-STARTPTS,fps=25.000000,trim=end_frame=25,\
             tpad=stop=25:stop_mode=clone,trim=end_frame=25,setpts=PTS-STARTPTS[v0]",
            "[0:a:0]atrim=start=0.220000,asetpts=PTS-STARTPTS,atrim=end_sample=48000,\
             apad=whole_len=48000,asetpts=PTS-STARTPTS[a0_0]",
            "[1:v:0]split=2[g1v0][g1v1]",
            "[g1v0]trim=start=0.000000,setpts=PTS-STARTPTS,fps=25.000000,trim=end_frame=25,\
             tpad=stop=25:stop_mode=clone,trim=end_frame=25,setpts=PTS-STARTPTS[v1]",
            "[g1v1]trim=start=1.980000,setpts=PTS-STARTPTS,fps=25.000000,trim=end_frame=50,\
             tpad=stop=50:stop_mode=clone,trim=end_frame=50,setpts=PTS-STARTPTS[v2]",
            "[1:a:0]asplit=2[g1a0_0][g1a0_1]",
            "[g1a0_0]atrim=start=0.000000,asetpts=PTS-STARTPTS,atrim=end_sample=48000,\
             apad=whole_len=48000,asetpts=PTS-STARTPTS[a1_0]",
            "[g1a0_1]atrim=start=2.000000,asetpts=PTS-STARTPTS,atrim=end_sample=96000,\
             apad=whole_len=96000,asetpts=PTS-STARTPTS[a2_0]",
            "[v0][a0_0][v1][a1_0][v2][a2_0]concat=n=3:v=1:a=1[cv][ca0]",
            "[cv]crop=30:16:2:4[vo]",
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
/// audio file keeps its codec.
#[test]
fn edit_args_count_each_track_at_its_rate_and_pan_only_mono_tracks() {
    let tracks = vec![
        Track {
            channels: Some(1),
            rate: Some(48_000),
        },
        Track {
            channels: Some(2),
            rate: Some(44_100),
        },
    ];
    let mut spec = edit(vec![span(0, 0.5, 0.25, None)], tracks);
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
    assert!(
        args.graph.contains("atrim=end_sample=12000,"),
        "{}",
        args.graph
    );
    assert!(
        args.graph.contains("atrim=end_sample=11025,"),
        "{}",
        args.graph
    );
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
         -map_metadata:s:a:1 0:s:a:1 -c:a pcm_f32le -f matroska /w/p.mkv"
    );
}

/// More separate stretches than one render may open: lossless passes of at most `MAX_INPUTS`
/// inputs each, then a final pass over their files in order whose pieces are those files whole
/// (their frames counted). Nothing is lost or reordered on the way.
#[test]
fn edit_passes_split_a_long_out_of_order_edit() {
    let n = MAX_INPUTS * 2 + 3;
    // Every piece out of order: each needs an input of its own.
    let spans: Vec<Span> = (0..n)
        .map(|k| span(0, (n - k) as f64 * 20.0, 0.4, Some(10)))
        .collect();
    let spec = edit(spans.clone(), Vec::new());
    assert_eq!(spec.groups().len(), n);
    let passes = edit_passes(&spec).unwrap();
    assert_eq!(passes.len(), 4, "three lossless passes and the final one");
    let first: Vec<Span> = passes[..3].iter().flat_map(|p| p.spans.clone()).collect();
    assert_eq!(first, spans, "every piece once, in order");
    assert!(passes[..3].iter().all(|p| p.groups().len() <= MAX_INPUTS));
    let last = &passes[3];
    assert_eq!(
        last.spans.iter().map(|s| s.file).collect::<Vec<_>>(),
        [1, 2, 3]
    );
    assert!(last.spans.iter().all(|s| s.start == 0.0));
    assert_eq!(
        last.spans.iter().map(|s| s.frames).collect::<Vec<_>>(),
        [
            Some(10 * MAX_INPUTS as u64),
            Some(10 * MAX_INPUTS as u64),
            Some(30)
        ]
    );
    assert!((last.length() - spec.length()).abs() < 1e-9);
    // One that fits is one pass.
    let small = edit(spans[..3].to_vec(), Vec::new());
    assert_eq!(edit_passes(&small).unwrap(), [small]);
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
    let u = Input(OsString::from(
        "http://127.0.0.1:9/tok/dir/clip.ts?range=1#x",
    ));
    assert_eq!((u.ext(), u.local()), ("ts".to_string(), None));
    assert_eq!(Input::path(Path::new("/c/README")).ext(), "");
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
