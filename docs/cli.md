# Command line: `--app`, `sopen cut`, `sopen stems`

Three ways to work on a file without an xtools window: hand a copy to any local app, cut
audio or video, or split voice from music.

## Any other app: `--app`

```sh
sopen myserver:/srv/data.bin --app ImHex    # macOS: the app's name, as `open -a` takes it
sopen myserver:/srv/data.bin --app imhex    # Linux: the command on PATH
```

Fetches a copy, opens it in the named local app, and waits: Enter uploads your changes, `q`
discards them.

## Cut audio and video: `sopen cut`

```sh
sopen cut myserver:/tmp/talk.mp4 1:05 1:20.5 --crop 0,0,1280,720
sopen cut myserver:/tmp/a.mp3 3 9
```

`IN` and `OUT` are `SS`, `MM:SS` or `HH:MM:SS(.ms)`. The cut is re-encoded so it is exact (video:
H.264 + AAC; audio: the source's codec) and uploaded next to the original as
`<stem>.cut-<in>-<out>.<ext>`. The remote path of the result is printed.

## Split voice and music: `sopen stems`

```sh
sopen stems myserver:/tmp/talk.mp3    # → talk.voice.mp3 and talk.music.mp3 next to it
sopen stems ./clip.mp4                # a video → clip.voice.m4a and clip.music.m4a (AAC)
```

Separates the voice from the music on this machine's CPU — a 4-minute song takes about 3 minutes
on an Apple-silicon Mac, and memory peaks at about 6.5 GB however long the track is — and uploads
both as new files next to the original, never over anything (`-1`, `-2`, … when a name is taken,
the same number for both unless another program takes one of the names in between); their paths
are printed, voice first. Each keeps the original's audio codec, sample rate, channels and exact
length (a lossy codec such as AAC may decode up to one codec frame longer: its encoder pads the
last frame) — mono stays mono, more than two channels come out as stereo, downmixed without
clipping (a note says so); a video gives `.m4a` (AAC); only the first audio track is
separated. Progress goes to stderr, one line per 30-second piece:
`stems: piece 3/8, 61s elapsed, 102s remaining`.

The model (UVR-MDX-NET-Voc_FT) comes with the Homebrew formula, and sopen checks it is exactly
that file before it uses it; `--model PATH` or `SOPEN_STEMS_MODEL` names another copy of it.
