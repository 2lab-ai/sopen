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
            )
            .unwrap(),
        );
        assert_eq!(
            got.join(" "),
            format!("-ss 1.000 -to 3.000 -i /c/s -vn {codec} {dst}"),
            "{dst}"
        );
    }
    assert!(export_args(src, Path::new("a.xyz"), 1.0, 3.0, None, MediaKind::Audio).is_err());
    assert!(export_args(src, Path::new("a.mp3"), 1.0, 3.0, crop, MediaKind::Audio).is_err());
    assert!(export_args(src, Path::new("a.mp4"), 3.0, 3.0, None, MediaKind::Video).is_err());
}

#[test]
fn output_container() {
    assert_eq!(output_ext("WEBM", MediaKind::Video), "mp4");
    assert_eq!(output_ext("mkv", MediaKind::Video), "mkv");
    assert_eq!(output_ext("webm", MediaKind::Audio), "webm");
    assert_eq!(output_ext("MP3", MediaKind::Audio), "mp3");
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
        {"codec_type":"audio"}],"format":{"duration":"4.000000"}}"#;
    assert_eq!(
        parse_probe(json).unwrap(),
        Probe {
            duration: Some(4.0),
            video: Some((640, 360)),
            has_audio: true
        }
    );
    let cover = r#"{"streams":[{"codec_type":"audio"},
        {"codec_type":"video","width":500,"height":500,"disposition":{"attached_pic":1}}],
        "format":{"duration":"2.5"}}"#;
    assert_eq!(parse_probe(cover).unwrap().video, None);
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
