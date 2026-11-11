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
            None,
        )
        .unwrap(),
    );
    assert_eq!(
        video.join(" "),
        "-ss 1.000 -to 3.000 -i /c/talk.mp4 -vf crop=320:180:10:20 -c:v libx264 -preset veryfast \
         -crf 18 -pix_fmt yuv420p -c:a aac -b:a 192k -movflags +faststart /o/talk.cut-1-3.mp4"
    );
    let mkv = strs(
        &export_args(
            src,
            Path::new("/o/a.mkv"),
            0.5,
            2.25,
            None,
            MediaKind::Video,
            None,
        )
        .unwrap(),
    );
    assert_eq!(
        mkv.join(" "),
        "-ss 0.500 -to 2.250 -i /c/talk.mp4 -c:v libx264 -preset veryfast -crf 18 -pix_fmt yuv420p \
         -c:a aac -b:a 192k /o/a.mkv"
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
        let got = strs(
            &export_args(
                Path::new("/c/s"),
                Path::new(dst),
                1.0,
                3.0,
                None,
                MediaKind::Audio,
                None,
            )
            .unwrap(),
        );
        assert_eq!(
            got.join(" "),
            format!("-ss 1.000 -to 3.000 -i /c/s -vn {codec} {dst}"),
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
            None,
        )
        .unwrap(),
    );
    assert_eq!(
        late[..10].join(" "),
        "-ss 2.500 -i /c/rec.ts -ss 10.000 -to 13.000 -c:v libx264"
    );
    let early = strs(
        &export_args(
            ts,
            Path::new("/o/r.ts"),
            1.0,
            3.0,
            None,
            MediaKind::Video,
            None,
        )
        .unwrap(),
    );
    assert_eq!(
        early[..8].join(" "),
        "-i /c/rec.ts -ss 1.000 -to 3.000 -c:v libx264"
    );

    let audio = MediaKind::Audio;
    assert!(export_args(src, Path::new("a.xyz"), 1.0, 3.0, None, audio, None).is_err());
    assert!(export_args(src, Path::new("a.mp3"), 1.0, 3.0, crop, audio, None).is_err());
    let video = MediaKind::Video;
    assert!(export_args(src, Path::new("a.mp4"), 3.0, 3.0, None, video, None).is_err());

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
        let got =
            strs(&export_args(src, Path::new(dst), 1.0, 3.0, None, audio, Some(codec)).unwrap());
        assert_eq!(
            got.join(" "),
            format!("-ss 1.000 -to 3.000 -i /c/talk.mp4 -vn {want} {dst}"),
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
        Some("vorbis"),
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
        {"codec_type":"video","width":640,"height":360,"disposition":{"attached_pic":0}},
        {"codec_type":"audio","codec_name":"aac"}],"format":{"duration":"4.000000"}}"#;
    assert_eq!(
        parse_probe(json).unwrap(),
        Probe {
            duration: Some(4.0),
            video: Some((640, 360)),
            rotation: 0,
            display: Some((640, 360)),
            has_audio: true,
            audio_codec: Some("aac".to_string()),
        }
    );
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
