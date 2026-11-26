# Video and audio

```sh
sopen myserver:/tmp/talk.mp4       # video → play, cut / copy / paste, crop → save back or a new file
sopen myserver:/tmp/a.mp3          # audio → play, cut / copy / paste, mono → stereo → save back or a new file
```

Video plays in the window with sound; audio shows its waveform, a stereo file in two lanes (L and
R). Both are edited on the timeline: cut, copy and paste stretches of it, as the image window
edits a rectangle. Editing only changes the list of pieces, so playback and the paused frame show
the edit at once; ffmpeg renders it only when you Save (over the original, after keeping it — see
below) or Export (a new file next to it). A remote file plays without being downloaded: ffmpeg
reads it through sopen, which fetches only what it asks for, and the waveform of a long sound fills
in as it is read. If no audio device can be opened the video plays silently and says so.

| Key | |
|---|---|
| click on the timeline | move the playhead (and drop the selection) |
| drag on the timeline | select a stretch (the playhead follows, so you see the frame there) |
| `I` / `O` | the selection starts / ends at the playhead (to the end / from the start without one) |
| `←` / `→` | move the playhead 1 s (`⇧` 5 s; `⌥`/Alt one frame on video, 10 ms on audio) |
| `Space` | play / pause the edited timeline |
| `⌘C` (Ctrl+C on Linux) | copy the selection (to this window only, not the system clipboard) |
| `⌘X` | cut: the selection goes, the gap closes, and it can be pasted |
| `Delete` / `Backspace` | remove the selection without keeping it |
| `⌘V` | paste what was copied or cut at the playhead, or over the selection |
| `M` | audio, mono file: Mono → Stereo — left and right are both the one channel (`M` again: mono) |
| `Esc` | clear the crop rectangle, then the selection |
| `⌘Z` / `⇧⌘Z` | undo / redo |
| `⌘S` or `F5` | Save: the edit is rendered (video: H.264 + AAC; audio: its own codec) and written over the original in the same format — refused, nothing rendered, if the file changed since it was opened. The original is kept first: a local file as a local backup (path printed); a remote one on its own server, as a hidden hard link beside it (`.<name>.sopen-backup-<time>`, named in the result — not a copy: a program that still has the original open can change it; delete it when you no longer need it). Then the window plays the saved file. `Esc` stops a Save before the original is replaced |
| `⌥⌘S` or `F7` | Save As: render the edit into a new file on this machine (never over an existing one) |
| `E` | Export In–Out of the timeline (all of it without a selection) and the crop as `<stem>.cut-<in>-<out>.<ext>` next to the original, and show its remote path; **Cancel** (or `Esc`) stops it and publishes nothing |

On video, drag a rectangle on the picture to crop what Export writes (Save writes the edit only,
never a crop over the original). Cuts land on frame times, so
the sound never drifts from the picture. Save and Export keep the picture and every audio track;
what they cannot keep (subtitles, data streams, cover art, chapters, alpha, more than 8 bits where
this ffmpeg's libx264 has no 10-bit) is named in the window before you save or export and again in
the result line. Closing with unsaved edits asks first. A save or export whose ffmpeg makes no
progress for two minutes is stopped with an error; if sopen itself is killed during one, its ffmpeg
stops with it and the half-made file is removed the next time a media window or `sopen cut` starts.
