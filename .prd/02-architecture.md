---
title: sopen — browser workstream architecture
type: architecture
tags: [sopen, architecture, browser, git-panel, preview]
---

# sopen — browser workstream architecture (02)

Status: planned (2026-11-20)

This document covers only the browser workstream: owner messages 14–16 (spec §1), fixed in
[`browser/ssot.md`](browser/ssot.md). It sets the browser's two modes, what each side of the window
shows and when, the preview, the module map, the threading, the hooks between the features, and the
work units. The features themselves are specified in:

| Doc | Owns |
|---|---|
| 02 (this) | modes, the side state machine, the preview (its whole contract), module boundaries, threading, hooks, work units, test hooks |
| [`03-git-panel.md`](03-git-panel.md) | the git panel: detection, running git, the graph, details, every action, safety, git-context keys |
| [`04-file-keys.md`](04-file-keys.md) | the key map of the file list, F-keys without fn, rename / copy / move / make folder / delete |
| [`05-directory-tree.md`](05-directory-tree.md) | the directory tree (F10) |

[`01-spec.md`](01-spec.md) §3a stays the contract of the browser as it ships today. Each of 02–05 says
which part of 01 it amends; that part of 01 is rewritten when the doc's work lands.

## 1. Words used here

- **Side**: the left or the right half of the browser's window. Each side owns a directory **panel**
  (`Panel` in `src/gui/browser/panel.rs`: its folder, its entries, its bar). A panel keeps its state
  while its side shows something else.
- **Active side**: the side that has the keyboard (its frame is the double line, 01 §3a).
- **Opposite side**: the other one.
- **Content** of a side: one of **List** (its panel), **Tree** (05), **Git** (the git panel, 03),
  **Preview** (§4). A **viewer** opened by Enter or F3 covers both sides, as today (01 §3a).
- **Repo of a folder**: the work tree the folder is inside, found by 03 §2's detection.

## 2. Browser modes

- **Preview mode** is the default: every browser window opens in it. **Dual-directory mode** is the
  browser as it shipped with message 12: two lists.
- **F9** switches between them, from a list or from the git panel (04 owns the key map). The F-key
  row's slot 9 names the mode F9 switches **to**: `Dual` in preview mode, `Prevw` in dual mode.
- **F9 with the keyboard in an automatic git panel** (O6, not pinned): the keyboard first goes back to
  the list it came from (the **origin list**, §3.1), then the mode switches; in dual mode both sides
  then show lists. **F9 with the keyboard in a pinned git panel** switches the mode and leaves the
  keyboard and the pin where they are: the pin outranks the mode (§3.1 rule 2).
- The mode belongs to the window and is not saved: the next window opens in preview mode again.
- The ssot records this reading of message 14 as the workstream's decision (ssot item 3); the owner
  may change it.

## 3. What each side shows

### 3.1 State and rules

`src/gui/browser/view.rs` computes what both sides show (`view::resolve`) from this state, and nothing
else; the browser asks it whenever an input changes. The only content it remembers is `shown`, the
**committed display** below, which is itself an input.

| Input | Meaning |
|---|---|
| `mode` | preview or dual (§2) |
| `focus` | which side has the keyboard, and whether it is in that side's List, Tree or Git |
| `origin` | the side whose List the keyboard last left for a Git (set by Tab into Git; cleared when the keyboard returns to a List) |
| `bar` | the active List's bar: its entry's kind and path |
| `folder` | the active List's folder, and its detection result (03 §2) |
| `pin` | `None`, or `{ side, repo }`: the side the git panel is pinned to and the repo's top level |
| `tree_snapshot` | `None`, or the opposite side's content captured when the tree opened (O4) |
| `git_wide` | the git panel takes both sides (Cmd/Ctrl+Shift+G, 03 §9.2): then it is the only content shown, with the keyboard; the sides' states are kept and come back when it is turned off |
| `rest` | how long the bar has rested since it last moved within the same folder |
| `shown` | the **committed display** of each side: the content the side shows now, with its identity (a Preview's file and its prepared viewer or preparation, a Git's repo, a List). It is what the side keeps while the bar has not rested 150 ms; see "Committing" below |
| `viewer` | whether a viewer covers both sides |

1. **A viewer is open** (Enter or F3 on a file): it covers both sides (01 §3a, unchanged).
2. **The pinned side** (`pin.side`) shows Git for `pin.repo`, whatever has the keyboard and whatever
   the mode. The pin belongs to a side, not to the keyboard.
3. **The side with the keyboard** shows what the keyboard is in: its List; its Tree while the tree is
   open on it (05); or Git (a pinned one, or an automatic one entered by Tab).
4. **The other side**, when rule 2 does not decide it, in this order:

| # | Condition | Shows |
|---|---|---|
| O2 | dual mode | its own List |
| O3 | the keyboard is in Git (pinned or automatic) | its own List — the origin list stays where it was |
| O4 | the keyboard is in a Tree | `tree_snapshot`, unchanged until the tree closes |
| O5 | the active list's bar is on a file (a regular file or a link to one) | Preview of that file (§4) |
| O6 | the bar is on a folder, a link to a folder, `..`, a special entry, or the list is empty — and the active folder is inside a git work tree | Git, for the repo of the active folder |
| O7 | otherwise (not in a work tree; detection still running; git missing) | its own List |

- **Committing.** `resolve` first computes each side's **target** from the rules above. A side whose
  target differs from `shown` only because the bar moved within the same folder (O5 and O6, the bar
  having rested less than 150 ms) keeps `shown`; at the 150 ms deadline, `shown` := target. Every other
  change commits **at once**, without the deadline: the window opening (the initial `shown` is the
  first target), a focus change (Tab, a pin taking the keyboard), a mode change (F9), a folder change
  (entering a folder, going up, a re-read that changes the folder), a detection answer for the active
  folder, pin and unpin, the tree opening or closing, `git_wide` on or off, and a viewer closing.
- **Committing and the preview's lifecycle.** When the bar leaves a previewed file, its **preparation**
  (if still running) is cancelled at once, freeing the single preparation slot (§4.4), while the side
  keeps showing what the preview last drew — a finished preview stays as it is, a running one freezes
  on its last progress frame with `…` — until the deadline commits the new target and the old preview
  is dropped. So two histories that reach the same bar and rest (the bar on file X after a preview of
  W, or after a git panel) keep their different displays until the deadline, then show the same
  thing.
- **The pin is rule 2, not a row of the table.** While a List has the keyboard, the pinned side is the
  other side and shows Git; after Tab the keyboard is in that pinned Git and the other side is the
  origin list (O3). So the list is never lost and Git never shows on both sides.
- **`tree_snapshot`** is taken when the tree opens: the other side's content as it was, with its
  identity — a Preview's file path and its preparation (kept alive, not cancelled), a Git's repo, a
  List. It is a `resolve` input until the tree closes, then dropped, and the rules apply again.
- **O5 and O6 wait for the bar to rest 150 ms** (see Committing), so a sweep over files and folders
  neither prepares previews nor shows and hides git panels; a git panel hidden and shown again within 30 s keeps its
  log stream, and a refresh younger than 10 s is not repeated (03 §4.5, §5).
- O6 uses the **active folder's** repo, never the repo of the folder under the bar (ssot item 3:
  "the current folder is inside a git work tree"). A subfolder that is itself another repo shows the
  current folder's repo until the user enters it.
- Bare repositories and folders inside a `.git` directory are not inside a work tree: O7 (03 §2.3).
- While detection runs for a newly entered folder, the other side shows O7; when detection answers,
  the rules apply again. Detection is cached per folder (03 §2.2), so this happens once per folder per
  window.
- One git panel exists per repo top level for the window's lifetime (at most 3 kept; the least
  recently shown is dropped, its running write first waited for). Moving between folders of the same
  repo keeps the same panel, its scroll, bar, filters and loaded pages.

### 3.2 Keys that move between contents

The key map is 04's (§3 there); these are the keys whose effect is defined by the rules above.

| Key | From | Effect |
|---|---|---|
| Tab / Shift+Tab | a List | The keyboard goes to the other side: into Git when that side shows Git (pinned or O6; `origin` = the list left), else into that side's own List (a Preview gives way to the panel underneath, and the side left then follows the new bar). |
| Tab / Shift+Tab | Git | The keyboard goes back to the origin list. |
| F9 | a List, Git | Switches the mode (§2). |
| Cmd/Ctrl+G | a List | Pins the git panel of the active folder's repo to the other side (`pin.side`). In a folder outside a work tree: the status row says so and nothing changes. Again: unpins. |
| Cmd/Ctrl+G | Git | Pins this git panel where it is; again: unpins (as Esc does when nothing is open). |
| Esc | a List | Stops a running fetch or file operation (01 §3a, 04 §5.1); otherwise, when a pin exists, unpins it. |
| Esc | Git | Closes the innermost open thing of the git panel (03 §9.2); when nothing is open and it is pinned, unpins it. |
| F10, Alt+F10 | a List | Opens the tree on the side with the keyboard (05). |
| Enter, F3 | a List, on a file | Opens the viewer over both sides (01 §3a); the preview of that file is handed over (§4.4). |

- **Unpinning from inside Git** moves the keyboard to the origin list first (or, without one, to the
  other side's List), then the rules apply: the side that was pinned shows a Preview, an automatic Git
  or its List as O2–O7 say — its List when the active folder is outside a work tree.
- **Navigating with a pin**: the active list may go anywhere; the pinned panel keeps `pin.repo` and
  says so in its frame title, also when the active folder is outside that repo.
- Cmd/Ctrl+G is new. It collides with nothing in the list (keys.md §3.2 "what exists"); the hex
  viewer's Cmd/Ctrl+G (Go to) only acts while the hex viewer is open, when the list's keys do not.
- Every key here follows 04's matching rules: modifier variants before plain keys, Cmd/Ctrl+digit
  aliases for F-keys, and no plain F-key or Cmd/Ctrl letter acts while a text field has the typing.

### 3.3 State diagram (the other side, preview mode, nothing pinned)

```
                bar moves onto a file                 bar moves onto a folder / ..
   ┌──────────┐ ───────────────────────► ┌─────────┐ ───────────────────────────► ┌───────────────┐
   │ own List │                          │ Preview │                              │ Git or List   │
   │  (O7)    │ ◄─────────────────────── │  (O5)   │ ◄─────────────────────────── │ (O6 / O7, by  │
   └──────────┘  folder not in a repo    └─────────┘     bar moves onto a file    │  detection)   │
        ▲                                                                         └───────────────┘
        │ F9 (dual) · Tab into it · tree opens (snapshot) · pin (→ Git) ── leave with F9 / Tab / tree closes / unpin
```

## 4. The preview (ssot item 3; this section is its whole contract)

### 4.1 When

- Only in preview mode, only while a List has the keyboard, only for rule O5 (the bar on a file).
- The preview starts when the bar has rested on the file for **150 ms**; a key or click that moves
  the bar sooner starts nothing for that file. Holding ↓ through a folder therefore prepares nothing.

### 4.2 What

The preview uses the viewers the browser already embeds (`text::embed`, `image::embed`,
`media::embed`, `hex::embed`), chosen exactly as Enter chooses (01 §3: extension, then the sniff),
prepared by the same code as Enter (`prepare::prepare`) in its preview form (§4.3), drawn inside the
other side's frame (the file's `host:/abs` centred in its top border, the frame single-lined because
the side does not have the keyboard). The viewer is told it is a preview (`Window::set_preview(true)`)
and then draws **as inactive and without its own bars** (no toolbar, status row or F-key row; its own
frame single-lined), and the text viewer does not follow a growing file.

| Kind | Shows |
|---|---|
| text | the first page, **wrapped** at the side's width, with highlighting as the text viewer does it |
| image | the image fitted into the side |
| video | one frame: the frame at 0 s (the media window's first frame), fitted in the picture's own shape (pixels that are not square are shown as wide as they are), with a line under it saying its size, frame rate and duration |
| audio | the waveform, with a line under it saying its channels, sample rate and whole duration; of a sound longer than 10 minutes, the waveform of the first 10 minutes **drawn over the side's whole width**, the line ending `preview of the first 10 minutes`. A sound in more than two channels shows its first two, the lanes named as the channels are (`FL`, `FR`), the line saying `6 channels · first 2 shown` |
| anything else | the hex view of the first rows |

- **Wrapped text** needs the text viewer's wrap mode, which the text-wrap branch adds (PR #14,
  `feat/text-wrap`). **It lands before slice S1** (§8), and P1 and the browser workstream's final
  acceptance require it; S1-B builds the preview's text on the wrap mode, never on a stop-gap.
- **Media** (video and audio, work unit S1-M). Everything a media preview needs from `ffprobe` and
  `ffmpeg` — the probe, the one frame, the waveform, and for a video above 8 bits the question which
  pixel formats ffmpeg's H.264 encoder takes (how Save and Export will render it) — is run by the
  preview's worker (`media/preview.rs`), one process at a time, each one stopped by the same cancel
  as a read (§4.4). The previewed window itself runs nothing: no decoder, no playback, no hash of
  the file. A file named as a video that holds only sound is previewed as the sound it is; a `.ts`
  `ffprobe` cannot read as media shows its hex rows, as Enter does; any other file `ffprobe`
  cannot read shows the info card with ffprobe's words. The media window's own waveform decode
  stops when its window goes (the window signals; the decoder's thread reaps).
- **A file that cannot be previewed** — unreadable, vanished, a read error, a decode error, a budget
  hit (§4.3) — shows an **info card**: name, kind, size (exact and in units), date and time, `<LINK>`
  and the target when it is a link, and a last line saying why (`over the preview limit: Enter opens
  it`, `image is 30000×30000; Enter opens it`, the error). The kind is there also when nothing of
  the file was read (a remote file over the limit: by its name). The reason comes first and is
  never cut; where it is about a file a program could not read, that is said under it, the file
  named once and as its owner knows it (`ffprobe cannot read host:/abs` — never the private copy),
  and that line is what a small side cuts. What `ffprobe` or `ffmpeg` said is shown without its
  `[component @ 0x…]` tags and without the path it puts in front. The status row stays the list's.
- **A link** is previewed as the file it points at (01 §3a: a link opens as its target); a broken
  link shows the info card with `<BROKEN>`.

### 4.3 Budgets

A preview never costs more than these; hitting one stops the preparation and shows the info card (the
bar does not have to move).

| Resource | Budget |
|---|---|
| bytes a preview may read from a remote | 32 MiB (decided from the stat, before anything is read); with streaming (another workstream), only the byte ranges the preview shows, never more than 32 MiB |
| image | 64 MiB encoded, and **64 megapixels** decoded (≤ 256 MiB of RGBA), checked from the image header before decoding |
| text | the index pass stops after the first **4 MiB** of the file; the preview shows those lines and its last row says `preview of the first 4 MiB` |
| video | `ffprobe` 5 s, then the first frame 5 s; above 8 bits, then the encoder question 5 s — each step its own time |
| audio | `ffprobe` 5 s; then the waveform, which decodes at most the first 10 minutes, in 10 s |
| hex | the rows on screen (the hex window reads pages only) |
| time, text, image and hex | 5 s of preparation after the read (a read shows its progress and is bounded by the 32 MiB) |

- A remote video over 32 MiB therefore shows the info card: its frame comes from `ffmpeg` on a local
  copy today. Lifting that limit belongs to the streaming workstream, which reads byte ranges.
- **Media has a time per step, not the one 5 s**: a video may take 5 s for `ffprobe` and 5 s more
  for its frame; a sound 5 s and 10 s. A step over its time is stopped — its process killed and
  reaped — and the file shows the info card, its last line naming the step (`ffprobe took over
  5 s: Enter opens it`, `the first frame took over 5 s: …`, `the waveform took over 10 s: …`); no
  partial waveform is shown. The one exception is the encoder question: over its time it is
  stopped the same way, but the preview shows all the same, and the media window asks again at
  the hand-over, off its frames (§4.4).

### 4.4 Lifecycle: one preparation at a time, every exit defined

- **One preview connection per window**, long-lived (`Connect`, an SFTP session opened at the first
  remote preview and kept until the window closes), and **previews are serialised on it**: a new
  preparation starts only after the previous one's worker has ended (it sent its result or its thread
  finished; a cancel is acknowledged by the worker's end, not by setting the flag). Meanwhile the side
  shows `…`. `prepare`'s steps that cannot be stopped (opening, `realpath`, the stat) only delay the
  next preview; they never add a session.
- **Sessions per window** (an ssh server allows about 10 per connection: OpenSSH's default
  `MaxSessions`): the listing 1, a viewer 1, the preview 1, the tree 1, a file operation 1, git ≤ 3
  (03 §3.2) — 8, and 9 for the moment a git stop runs (03 §3.5). A cancelled preview never holds a
  second one.
- **A preview reads without claiming the cache.** A remote file is read into a private preview copy
  (`<cache>/.preview/<window>/<n>`, 0600, removed when the preview is dropped; the window's folder
  goes with the window, and `.preview` itself with the last window's folder — by `rmdir`, which only ever removes it empty); a local file is read in
  place, an image included. A preview never claims a cached copy, publishes into the mirror, moves
  earlier edits aside or creates a session copy (01 §5's machinery is for files that are opened).
- **Cancel** = the preparation's stop flag (the flag Esc sets for Enter today); the worker checks it at
  every read, between decode steps, at every MiB of the text index, and before handing over. While
  an `ffprobe` or `ffmpeg` of a media preview runs, the flag (and the step's time) is looked at
  every few milliseconds; a child starts only after a look at it, and on every way out — cancelled,
  over its time, ended, or the worker itself failing — it is killed and reaped. A child stays in
  its slot (`media/child.rs`) until it has exited, so a program that closed its output and lives
  on is still there to be stopped; a stop that comes while a child is being started is applied
  as soon as it is in its slot; and a child is never read from without a thread watching it. No
  process outlives its preview. **When sopen itself is killed** (a signal, `kill -9`): on Linux
  the kernel kills every such child with it (each sets `PR_SET_PDEATHSIG`); on macOS there is
  no such thing, and the one `ffprobe` or `ffmpeg` that ran is left to end by itself — each is
  bounded by its input (one probe, one frame, at most the file's sound), and a Ctrl+C in the
  terminal reaches it with the rest of the process group. The private preview folder such an end
  leaves is swept by the next window (its lock is free). sopen keeps no list of its children
  and handles no signal.

| Exit | What happens to the preparation (or the prepared preview) |
|---|---|
| the bar moves to another file | cancel; the next starts 150 ms after the bar rests, once this one has ended |
| the bar moves to a folder or `..` | cancel |
| F9, pin (Cmd/Ctrl+G), Tab | cancel |
| the tree opens | kept as it is, inside `tree_snapshot` (O4); cancelled when the tree closes if the rules no longer show it |
| Enter or F3 on the previewed file | **handed over** (below) |
| Enter or F3 on a file showing an info card | the preview is dropped (its preparation cancelled) and the file opens as Enter opens it in the list (01 §3a), from the start; a private preview copy that is complete and whose file is unchanged since (size, mtime) is published into the cache as below instead of read again. A video that `ffprobe` reads but none of whose frames decode fails the open too — after it was made ready: the media window, still as it was opened (nothing edited, nothing being written), gives up at its first frame, and the list is back with the reason on its status row. An open that fails, at either point, removes nothing from the cache: the copy it fetched or published is ordinary cache content, kept or replaced by the cache's own rules (01 §5) |
| Enter on another entry, F4 | cancel / kept (F4 does not change the bar) |
| the window closes | cancel; the window does not wait for a cancelled preview. Its worker reads, removes its own private copy, and for a video or a sound may hold one `ffprobe` or `ffmpeg`: the window sends that process its kill itself as it goes (`Host::cancel`), since sopen may end before the worker has looked at its stop flag. That call is on a frame, so it only signals — it waits neither for the process nor for the worker; the worker reaps |

- **The hand-over** (Enter or F3 on a previewed file). The live preview window becomes the viewer:
  - `Window::set_preview(false)`: the preview's budgets are lifted from that moment, its frame and bars
    are drawn as a viewer's, input reaches it.
  - **Work the preview stopped at its budget continues where it stopped** — the text index resumes at
    the offset where the 4 MiB pass stopped and keeps its marks; the audio waveform keeps its first 10
    minutes and decodes on from there (one more `ffmpeg`, started at 10:00), and a video's frame is
    the one the preview decoded, not decoded again. For the text index the guarantee is exactly this: during a
    continuation that succeeds, each byte is indexed once; the bytes before the cut are also read
    twice more to verify them (hashed before and after the continuation); a mismatch re-indexes
    the whole file.
  - **What a media preview left undone starts now**: the file as opened is hashed for Save's check
    (a local file; a remote copy's hash came with its read), and the cache is swept of dead Save
    and Export jobs. How Save and Export render the file came with the preview; only where the
    encoder question ran out of time is it asked again — on a thread of its own, Save and Export
    unavailable and the status row saying `checking the encoder…` until the answer lands (§6:
    nothing slow on a frame). A media window opened by itself (`sopen file.mov`) asks the same
    way. That question is the window's own task: it has 5 s, and it is stopped — its `ffmpeg`
    killed — when the window goes. Without an answer in its time, Save and Export are planned
    for 8-bit `yuv420p` (which every H.264 encoder takes), become available, and say so:
    `encoder check timed out; Export converts to 8-bit` — and nothing else about the encoder,
    which never answered.
  - **A preparation still running** becomes the viewer's opening task: its read or decode continues
    under the viewer's rules (no preview budget, no 5 s deadline), its progress moves to the status row,
    Esc stops it as Enter's does.
  - **The cache**: a remote file's private preview copy is **published into the cache by a hard link**
    and claimed and recorded as Enter's fetch would be (01 §5); the viewer keeps the path it opened
    (the text viewer, the hex window and the media window each re-check or reuse their opened path),
    and the preview name is removed when the viewer closes. A local file was read in place and stays
    so.
  - **The connection**: a previewed window's `Origin` holds a placeholder connection that fails; the
    viewer's first remote call fails its stat, and `Origin::fs()` reconnects (`gui/mod.rs`), so the
    viewer opens its own session on first use and the preview connection stays the window's one
    preview session (§4.4's budget).
- **A previewed viewer gets no input.** The requirement: no key, click, drag or wheel changes a
  previewed viewer; it never holds unsaved changes; dropping it never asks anything or waits for a
  write; it never takes a key the list needs (so 02 §3.2's keys hold while a preview shows). The
  means: the browser draws the preview inside a child UI that is **disabled** (`ui.disable()`, with
  `disabled_alpha` 1.0 so it is not greyed), and withholds the frame's keyboard, pointer and wheel
  events during its pass. Withholding alone is not enough: egui computes clicks from the pointer
  state at the start of the frame, and the media window's Play button is clickable.
- **A previewed medium never plays** and opens no audio output.
- **F4** opens the file in the editor as from the list (01 §3a).

### 4.5 Acceptance (preview)

Run each in a local folder and in a remote folder (`sopen host:/tmp/sopen-receipts/preview`), the
receipt being a window screenshot (AGENTS.md "Receipts") someone looked at, plus the tests named.
P1 and the final acceptance run after the text-wrap branch has landed (§4.2).

- P1. Open a folder holding `a.rs`, `b.png`, `c.mp4`, `d.mp3`, `e.zip`, a subfolder → the window opens
  in preview mode, the bar on `..`; F-key slot 9 reads `Dual`. Move the bar to each file and wait →
  the other side shows, in turn: wrapped highlighted text, the fitted image, a video frame with its
  duration, the waveform, the hex rows. (Test: `view::resolve` table, every row of §3.1 with every
  input of its state table.)
- P2. Press ↓ quickly through ten files → nothing is fetched for the files passed (test: a counting
  `RemoteFs` sees one preparation, for the file the bar rested on).
- P3. Put the bar on a 100 MiB remote file → the info card `over the preview limit`; nothing is
  read. Put it on a local 30000×30000 PNG, a local 2 GB log, a two-hour MP3 and a video whose probe
  hangs (a stand-in `ffprobe` that sleeps) and leave the bar there → each shows its card or its bounded
  preview (`first 4 MiB`, `first 10 minutes`) within its budget, and no worker is left running.
- P4. Put the bar on a 30 MiB remote file and move away at 40 % → its requests stop, nothing is
  published under any name in the cache (the private preview copy is removed), and the next preview
  starts only after the first worker ended (test with a slow fake worker: peak live preview workers =
  1, peak preview SFTP sessions = 1, across 20 quick moves). Sweep the bar over 50 files of every kind
  → no ffmpeg process outlives its preview, and the cache's mirror and session folders are unchanged.
- P5. For each kind (text, image, video, audio, hex), previewed: real clicks (the media window's
  Play button included), drags, the wheel, `C`, `P`, Space, Cmd/Ctrl+V and typing over it → nothing
  changes in the viewer (its dirty state stays clean), no audio output is opened (a stand-in output
  counts opens), the list's keys still act, and moving the bar away asks nothing. The preview draws as
  inactive and without bars (screenshot).
- P6. Press Enter on a previewed remote image → the image window opens at once; no byte range is read
  twice and nothing is decoded twice (test: the reads' ranges, one decode), and the cached copy is
  claimed and recorded only now, by a hard link to the preview copy (same inode); the viewer's Save
  reconnects on its first remote call. Press Enter while the preview still reads → the same read
  continues as the viewer's opening.
- P12. A local text file of 6 MiB and a remote one of 20 MiB: the preview shows the first page and
  `preview of the first 4 MiB`; Enter → the viewer; End → the file's last line. On a continuation
  that succeeds each byte is indexed once, and the 4 MiB before the cut are read twice more only to
  verify them (§4.4) (test: the index's reads see every offset once, the verification reads are
  counted apart; remotely, one fetch). Enter on a 100 MiB remote text
  file's info card → the ordinary open, with its fetch progress on the status row. An audio file of
  20 minutes (after S1-M): Enter → the waveform's first 10 minutes stay, the rest is decoded from 10:00.
- P13. Two histories reach the bar on file X within 150 ms — one from a preview of file W, one from the
  git panel on `..` — → each side keeps its own display (W's preview; the git panel) until the
  deadline, then both show X's preview; Tab, F9 or entering a folder during the 150 ms commit at once
  (test: `view::resolve` with `shown`).
- P7. With nothing pinned, press F9 → both sides show lists, slot 9 reads `Prevw`; F9 again → preview
  mode. With a pin, F9 → the pinned side keeps Git, the other side is a List.
- P8. In a git work tree, put the bar on `..` → the other side shows the git panel; on a file → its
  preview; Cmd/Ctrl+G → the git panel stays while the bar moves over files; Esc → the preview again.
- P9. List → Cmd/Ctrl+G → Tab → the keyboard is in Git, the other side still shows the origin list (not
  Git); Tab → back in that list, the bar where it was; Tab, Esc → unpinned, the keyboard in the origin
  list. Enter a folder outside the repo, pin there is refused; pin inside, then go to a folder outside →
  the pinned panel stays, titled with its repo; Tab into it, Esc → the pinned side shows that
  folder's other-side content by O2–O7 (its List).
- P10. Tab into an automatic git panel (O6), press F9 → both sides show lists and the keyboard is in
  the origin list.
- P11. With a preview showing, open the tree (F10) and move through it → the other side keeps the
  same preview (no new preparation); close the tree → the rules apply again.

## 5. Module map

New code goes into new files. Each path has one owner at a time: the work unit named here, in the
slice named here (§8); a file that exists today changes owner only from one slice to the next.

| Path | Responsibility | Owner |
|---|---|---|
| `src/gui/browser/keys.rs` | the browser's one key table — every context's rows, written up front from 02 §3.2, 03 §9, 04 §2–§3 and 05 §1 — `Command`, exact modifier matching, typed-text characters (04 §3) | S1-A; later slices change rows only through a declared hand-over |
| `src/gui/mod.rs` | `Window::set_preview(bool)` on the trait, with a default that does nothing (S1-0, so S1-B and S1-A never share the file); Cmd/Ctrl+digit → F-key, once, in the shell's raw-input hook; clicks on the F-key row; `State::parse` and `Hooks` for the receipt keys (§9); focus events (S7) | S1-0, then S1-A, then each slice's integration WU for its receipt keys |
| `src/gui/chrome.rs` (body), `src/gui/chrome/tests.rs`, `src/gui/help.rs` | the clickable F-key row; F1's key help screen in every window | S1-A |
| `src/gui/{text,image,hex,media}.rs` and their `tests.rs`, `src/gui/browser/prepare.rs`, `src/cache.rs` | `Window::set_preview(bool)` (inactive frame, no bars, no input, no audio, no following, §4.3's budgets); the preview's read without claiming, the placeholder connection, the hard-link claim on hand-over; the `.preview` cache root | S1-B (`media.rs` only after PR #15, and only the `set_preview` default; then S1-M); in S2 the `read_only` flag and the `.revisions` root (S2-E) |
| `src/fileops/*.rs`, `src/remote/ops.rs`, `src/remote/sftp/ops.rs`, `src/remote/local/ops.rs` | S1-C: rename, copy and move of files and folders, with the primitives they need — `MKDIR` for a copied folder, `REMOVE` and `RMDIR` for a moved one's sources; S6-A: F7 and F8 as actions, the local Trash, the plain-rename channel for remote folders: plans and runs over both file layers, never overwriting (04 §5) | S1-C, S6-A |
| `src/gui/browser/ops.rs`, `src/gui/browser/panel.rs` | selection, the file-operation dialogs, progress and reports; **drawing helpers** for the inline rename field and the selection colours, which `browser.rs`'s `panel_ui` (S1-E) calls | S1-D, S6-B |
| `src/gui/browser/view.rs`, `src/gui/browser/preview.rs` | modes and `view::resolve` (§3) with its table test; the preview host (§4) | S1-E (O6 and the pin: S2-F) |
| `src/gui/browser.rs`, `src/gui/browser/tests.rs`, `src/main.rs` | wiring and end-to-end window tests; `Start` gains the ssh destination (`RemoteSpec::destination`, not the bracketed label) and the state dir; the per-feature job state (§6) | S1-0 first (`browser.rs`: the `mod keys; ops; view; preview;` lines next to today's `panel` and `prepare`, and `Start`; `main.rs`: building `Start`), then one integration WU per slice (S1-E, S2-F, S3-C, S4-C, S5-C, S6-C, S7-B) |
| `src/git/mod.rs`, `src/git/model.rs` | `mod` lines; the shared types `Oid`, `Detection`, `CommitRow`, `RefLabel`, `StashRow`, `StatusEntry`, `InProgress`, `Backup` (03 names their fields) | S2-0 writes, S2-B owns (adds only; a rename goes through the dispatcher) |
| `src/git/runner.rs`, `src/remote/exec.rs`, `.github/workflows/ci.yml` | `GitRunner`; `LocalGit`, `SshGit`, `ShellGit` (tests); quoting, limits, stop, progress, the command log; `exec_args(dest, state_dir)`; CI's `SOPEN_REQUIRE_GIT=1` | S2-A (`pub(crate)` on `sftp.rs`'s `ssh_config`: S2-0) |
| `tests/cli.rs`, `src/forward.rs` | end-to-end CLI tests of each slice's hooks; forwarding (nothing new: `SOPEN_STATE` and `SOPEN_LIST` already travel, `SOPEN_GIT` stays local) | each slice's integration WU |
| `src/git/{detect,parse,history}.rs` | detection and its cache; parsers; the log stream | S2-B |
| `src/git/graph.rs` | the lane layout (pure, deterministic) | S2-C |
| `src/gui/browser/git_panel/{mod,graph_view,details,find,filters}.rs`, `src/gui/chrome/git.rs` | the read-only git panel and its look | S2-D |
| `src/git/{actions,backup}.rs` | every action of 03 §7 as a pure plan (steps, confirmation text, backups); backups, manifests, restores | S3-A |
| `src/gui/browser/git_panel/{menu,dialogs,backups,log_view,in_progress}.rs` | the action menu, dialogs, the backups list, the command log, the banner | S3-B |
| `src/git/rebase.rs`, `src/gui/browser/git_panel/planner.rs` | the history planner | S4-A, S4-B |
| `src/tree/*.rs`, `src/remote/treefs.rs`, `src/remote/{sftp,local}/treefs.rs` | the tree's model and `TreeFs` (a worker with channels, 05 §7) | S5-A |
| `src/gui/browser/tree.rs`, `src/gui/chrome/tree.rs` | the tree's side content, keys and look | S5-B |
| `src/fnkeys/{mod,macos}.rs`, `src/config.rs` | the macOS F-key switch, its lock, state and watchdog, the `fkeys` config key (04 §6) | S7-A |
| `src/git/tests/*.rs`, `src/gui/browser/git_panel/tests*.rs` | each git WU's tests in a file of its own | the WU they test |

- The remote layer grows **new traits in new files** (`remote::ops::FileOps`, `remote::treefs::TreeFs`)
  implemented for `SftpRemote` and `LocalFs` in submodules of `sftp.rs` and `local.rs`. A submodule sees
  its parent's private fields, so `sftp.rs`, `local.rs` and `RemoteFs` stay as they are; the slice's
  scaffold step adds the `mod` lines.
- Git never goes through `RemoteFs`: it runs as a command (03 §3).

## 6. Threading and data flow

- **Nothing slow runs on a frame** (01 §3a's rule, kept). The media window's one such run — asking
  ffmpeg what its H.264 encoder takes, for a video above 8 bits — is made by the preview's worker,
  or on a thread of the window's own (§4.4). Every git command, listing, preview
  preparation, file operation and tree scan runs on a worker thread; results come back over an
  `mpsc` channel and the worker calls `ctx.request_repaint()` (the browser's existing pattern,
  `browser.rs` `list` / `start_open`).
- **One job state per feature.** Today the browser has one task slot and its list ignores keys while
  any task runs. The integration of S1 replaces it with a state per feature — listing, opening a
  viewer, preview, file operation, tree, git — each with its own rule for which keys it holds back
  (a running listing still holds the list's keys; a preview or a git read holds none).
- **Stale results are dropped**: each job carries the generation of what asked for it (a git panel's
  load generation, a preview's bar position, a tree row's visibility); a result for an older
  generation is discarded unread.
- **Sessions per window** (OpenSSH's default `MaxSessions` is 10 per connection): the listing 1, a
  viewer 1, the preview 1 (§4.4), the tree 1 (05), a file operation 1 (04), git ≤ 3 — 8 in all, 9
  while a git stop runs.
- **Git, per host**: one `GitHost` (runner, git version, at most 3 commands in flight; a stop has a
  session of its own, 03 §3.5). **Per repo**: one `GitRepo` (detection, the log stream, a **write
  lock**: one repository-changing command at a time; reads go on alongside).
- **Stopping**: reads and network commands stop on Esc; writes on Esc after 10 s, with a question
  (03 §3.4). Closing the window during a write waits for it and prints its result, as a viewer's
  running save is waited for (`Leftover` in `browser.rs`, 01 §3a).
- **The tree's remote reads** go through `TreeFs`, a worker owning its connection and runtime, not
  through `SftpRemote`'s one `block_on` per call (05 §7).

### 6.1 Trace: entering a folder inside a remote repo, preview mode

1. Enter on a folder → `Browser::list` (worker) → `Panel::fill` (existing).
2. `view::resolve`: bar on `..`, detection for this folder unknown → O7 (own List); the browser asks
   `git::detect::detect(host, folder)` on a worker.
3. `detect` → `GitRunner::run(Read, ["rev-parse", …])` → `SshGit`: `ssh <exec_args> host -- exec 'env'
   … 'sh' '-c' '<locate git>' 'sopen-git' '-C' '<folder>' 'rev-parse' …` → stdout lines →
   `parse::detection` → `Detection::WorkTree { top, git_dir, common_dir, … }` → cached by
   `(host, folder)`.
4. Result → `view::resolve` → O6: Git for `top` → a `GitPanel` for `top` is made (or the cached one
   is reused) → `GitPanel::load`.
5. `load` (a new generation), in parallel (≤ 3): refs + HEAD, stash list, status, in-progress probe;
   then `history::start(roots)` — the object ids those reads returned, on stdin (03 §4.1) — → the log
   stream → `history::page(500)` → `parse::commit_rows`.
6. `graph::layout(rows, synthetic rows for uncommitted and stashes)` → lanes per row.
7. Frame: `git_panel::graph_view` paints the visible rows only (chrome/git.rs look); the status row
   names the last command (`git log … · 500 rows · 0.4 s`).

### 6.2 Trace: drop a commit from the graph

1. F8 on a commit row (03 §9.1) → `actions::plan(DropCommit { oid })` → a plan: backup step + planner
   run (`rebase -i` with the todo on stdin) + refresh; the confirmation text names the commit, the
   branch, the backup ref and the exact command lines.
2. Run (Enter) → takes the repo's write lock → `backup::write` (`update-ref --stdin`, one transaction)
   → if it fails, nothing else runs → the rebase → exit 0 or a conflict stop.
3. Done → a `GitEvent::WorkTreeChanged { top }` → the browser re-lists every panel whose folder is
   inside `top` (bar kept on the same name) → the git panel reloads (03 §5) → the status
   row: `dropped abc1234 · backup refs/sopen-backup/… · Cmd/Ctrl+Z undoes`.
4. A conflict stop → the in-progress banner (03 §8.5); the repo stays as git left it.

## 7. Hooks between the features

Each slice's scaffold step writes the hooks it needs as stubs; later slices fill them. A key whose
feature has not landed has no row in the deployed table and its F-key slot shows an empty label, as
01 §3a's row lists only the keys that work (so S1's list row reads `1Help 2Rename 3View 4Edit 5Copy
6Move 9Dual`).

| Hook | Where | Used by |
|---|---|---|
| `keys::Command` and `keys::lookup(input, context) -> Option<Command>` | `browser/keys.rs` | every key of 02–05; one table, exact modifiers (04 §3) |
| `Browser::run(Command)` | `browser.rs` | dispatches to `ops`, `tree`, `git_panel`, `view` |
| `view::resolve(&Inputs) -> Resolved { show: [Content; 2], pending: Option<Duration> }` | `browser/view.rs` | the browser's drawing and focus: `show` is the new committed display; `pending` is how long a side still keeps what it shows because the bar has not rested (the browser asks again then) |
| `Window::set_preview(bool)` | `gui/mod.rs` trait, each viewer | a preview, and its hand-over to a full viewer (§4.4) |
| `BrowserEvent::FilesChanged { host, dirs }` | `browser.rs` | sent by file operations (04) and git writes (03); panels showing one of `dirs` re-list, a git panel whose top level holds one soft-refreshes |
| `GitEvent::WorkTreeChanged { host, top }` | `git_panel` → `browser.rs` | after a git write that can change files (checkout, reset, merge, rebase, stash, clean, discard) |
| `BrowserRequest::OpenViewer { file, title, read_only }` | `git_panel` → `browser.rs` | diffs and files at a revision open in the viewers, read-only (03 §6) |
| `TreeOutcome::GoTo(path)` | `tree` → `browser.rs` | Enter on a tree row lists that folder in the active panel (05) |
| `OpsTarget` | `ops` | F5/F6 copy and move to the other panel's own folder, even when the other side shows Git or a Preview — the dialog names it (04 §5.2) |
| `Content::Tree` | `view.rs` | the tree replaces the active side's List; `tree_snapshot` keeps the other side (O4) |

## 8. Slices and work units

The workstream ships in **seven slices, in this order**. Each slice is merged and deployed on its own
(the preview channel, AGENTS.md "Distribution"), with its receipts, before the next starts; so the
owner sees each part as it lands, and each slice's WUs integrate into `browser.rs` directly instead
of waiting for one integration at the end. Within a slice: a small **scaffold** step first (its `mod`
lines, stubs, dependencies — one serial change, so `--locked` stays green), then its WUs in parallel
on disjoint files, then its **integration** WU, the slice's one owner of `browser.rs`. The experiment
a slice depends on runs before it (§8.2).

### 8.1 The slices

| Slice | What the owner gets | WUs (owner files: §5) | Needs first |
|---|---|---|---|
| **S1** | Preview mode (text, image, hex; video and audio came with S1-M), F9 and rules O2 / O5 / O7, F2 rename, F5 copy, F6 move — local, and remote: files through `hardlink@` + `REMOVE`, folders by copy, check and remove (04 §5.6) — selection, the clickable F-key row with Cmd/Ctrl+digit aliases, F1 key help | S1-0 scaffold (touches no viewer file) · S1-A keys, aliases, row, help · S1-B viewer preview flags, preview read path, `.preview` cache root · S1-C file-operations engine · S1-D selection and file-operation dialogs, with drawing helpers for the inline rename field and selection colours · S1-E view, preview host, integration (calls S1-D's helpers in `panel_ui`) | E3; for S1-B (and P1) the text-wrap branch (PR #14) merged; S1-B changes `media.rs` only after PR #15 has merged, and then only for the `set_preview` default |
| **S1-M** | The media preview: video frame and audio waveform previews, their budgets and hand-over (§4.2–§4.4) | S1-M (one WU: `media.rs`, its tests, the preview host's media row) | S1; PR #15 merged |
| **S2** | The read-only git panel: detection, the log stream, the graph, details, diffs, files at a revision, find and filters, pin, refresh, `SOPEN_LIST=git` | S2-0 scaffold · S2-A runner · S2-B read model · S2-C lane layout · S2-D panel UI · S2-E viewers' read-only flag, `.revisions` root · S2-F O6, pin, integration | S1; E4 |
| **S3** | Every Git Graph action, staging and commit, backups, restore and undo, the banner, the command log | S3-0 · S3-A actions and backups (logic) · S3-B menu, dialogs, backups list, log, banner · S3-C integration | S2; E5 |
| **S4** | Free history editing: quick edits and the planner | S4-0 · S4-A planner logic · S4-B planner UI · S4-C integration | S3 |
| **S5** | The directory tree (F10, Alt+F10) | S5-0 · S5-A tree model, `TreeFs` worker · S5-B tree UI · S5-C integration | S1 |
| **S6** | F7 make folder and F8 delete as actions (Trash locally), remote folder rename and copy publish without `hardlink@` through a low-level plain-rename channel (S1 already uses `MKDIR`, `REMOVE` and `RMDIR` inside copy and move) | S6-0 (adds `openssh-sftp-client-lowlevel` if E2 picks it) · S6-A engine · S6-B dialogs · S6-C integration | S1; E2 |
| **S7** | The macOS function-key switch | S7-A switch, lock, state, watchdog, config · S7-B focus events, integration | S1; E1; then the owner's receipt K-F3 (04 §8) |

- **Acceptance per slice** — a slice is accepted on its own scenarios only:

| Slice | Scenarios |
|---|---|
| S1 | 02 P1–P7, P12 (text), P13 (P1 and P5: text, image and hex in full; video and audio with S1-M), 04 K-R1 (S1's row), K-R2, K-R3 (Shift+F6, F6, exact modifiers), K-S1, K-C1–K-C5, K-M1 and K-M2 (S1 rows), K-N1 (files; remote folders after S6), K-E1 |
| S1-M | 02 P1, P5, P6 and P12 for video and audio; P3's media parts (a stalled `ffprobe`, a two-hour audio file: each within its budget, no worker left) and P4's (no ffmpeg process outlives its preview across a 50-file sweep, a cancelled waveform decode stops) |
| S2 | 02 P8–P10; 03 G-D1, G-D2, G-R1–G-R4, G-R6, G-G1, G-G2, G-P1–G-P4, G-F1, G-F2, G-T1–G-T5, G-W1 (graph, details, diff) |
| S3 | 03 G-R5, G-A1–G-A9, G-S1–G-S8, G-K1, G-K2, G-W1 (banner, backups, log) |
| S4 | 03 G-H1–G-H5, G-W1 (planner) |
| S5 | 02 P11; 05 D-1–D-8; 04 K-R1 (slot 10), K-R3 (Alt+F10) |
| S6 | 04 K-R1 (slots 7, 8), K-R3 (Shift+F8), K-K1, K-D1, K-N1 (remote folders), K-C4 (a server without `hardlink@`) |
| S7 | 04 K-F1–K-F6 |

- **Schedule: one slice at a time.** Slices never run concurrently, so no two slices ever work on a
  shared file at once. The order is S1 → S2 → S3 → S4 → S5 → S6 → S7, constrained by the "Needs
  first" column: S1-M runs as soon as PR #15 has merged, between two slices; S7 waits for E1 and runs
  last. The streaming workstream (another branch) rebases onto S1, which touches `prepare.rs` and the
  four `embed` signatures first.
- **Hand-overs between slices** are the only shared files: `keys.rs` (S1-A writes every row; a later
  slice changes a row in its scaffold step, declared there), `gui/mod.rs` (S1-A, then each
  integration WU for its receipt keys: `State::parse` rejects unknown keys and `Hooks` flags accept
  only `1` today), `browser.rs`, `main.rs`, `view.rs`, the viewers (S1-B → S2-E). S2-E changes the
  viewer files (`text`, `image`, `hex`, `media`) only after the streaming units W4–W7 and ED4–ED6
  have merged, or it touches only the `Window` trait's default (`.prd/06-streaming.md` §13).
- Each WU's gate is AGENTS.md's (`just check`, with `SOPEN_REQUIRE_FFMPEG=1` and, from S2,
  `SOPEN_REQUIRE_GIT=1`); a WU that shows something on screen brings a screenshot receipt (§9). The
  live plan — branches, worktrees, status — is in [`browser/loop.md`](browser/loop.md).

### 8.2 Experiments before their slice

| ID | Before | What it checks |
|---|---|---|
| E1 | S7 | `IOHIDSetCFTypeParameter("HIDFKeyMode", 1)` from a tiny unprivileged program: read back with `ioreg`, then restore (the F5 key itself is the owner's receipt K-F3) |
| E2 | S6 | plain rename through `openssh-sftp-client-lowlevel` vs an auxiliary channel without the posix-rename extension, against a local `sftp-server -e -l INFO` |
| E3 | S1 | preview isolation and cost: each viewer embedded at half width, disabled, events withheld; a 50-file sweep counting ffmpeg processes, cache growth, and the effect of clicks and drags |
| E4 | S2 | five git reads over the ssh master, timed |
| E5 | S3 | remote stop: `exec sh -c 'echo $$ >&2; exec sleep 300'`, then `kill -TERM -- -<pid>`, under zsh, fish and bash login shells; an SFTP listing on the same master working while a parked `git log` stream is open |

**E3's result** (measured 2026-11-21 on preview 0120d40813a0):

- **Run:** 50 files (10 txt, 10 rs, 8 png, 7 jpg, 5 mp4, 3 wav, 2 mp3, 5 bin; 3 MB in total) on a
  remote host over SFTP, in preview mode, with the bar moved down 49 times through the browser's
  key hook (`SOPEN_KEYS`). Each step waits for the preview to settle.
- **Time:** 13.4 s for the 50 settled previews.
- **ffmpeg:** 0 ffmpeg or ffprobe processes spawned by sopen (video and audio showed a file card
  before S1-M).
- **Cache:** the preview cache peaked at 196 KB and held 0 bytes after exit.
- **Input isolation:** not part of this run. S1-E's P5 tests cover it: the host withholds the
  frame's input, and a stand-in window that reacts to everything stays untouched.

**E3's media rerun** (S1-M, measured 2026-11-21 on the branch's dev build):

- **Run:** 50 files (10 txt, 10 rs, 8 png, 7 jpg, 5 mp4, 3 wav, 2 mp3 of 15 minutes, 5 bin; 19 MB
  in total), the bar moved down 50 times through `SOPEN_KEYS`, once in a local folder and once
  over ssh to the same machine (`localhost:`), not another host.
- **Time:** 13.7 s local, 12.4 s over ssh.
- **ffmpeg:** 20 processes per sweep — 10 `ffprobe` and 10 `ffmpeg`, one probe and one frame or
  waveform for each of the 10 media files — counted by stand-in tools that log and `exec` the
  real ones. At most 1 at a time; 0 alive after the window closed.
- **Cache:** the preview cache peaked at 0 bytes locally (files are read in place) and at 7.2 MB
  over ssh (one mp3's private copy); 0 bytes after exit in both; nothing outside `.preview`.

## 9. Receipts and test hooks

Added to AGENTS.md "Receipts" by the slice that brings them (the hooks travel with `--target` as
`SOPEN_STATE` and `SOPEN_LIST` already do):

| Hook | Effect | Slice |
|---|---|---|
| `SOPEN_STATE='mode=dual'` | The browser opens in dual mode (default preview). | S1 |
| `SOPEN_STATE='bar=NAME'` | The active panel's bar goes on NAME once listed, without opening it (a preview receipt). | S1 |
| `SOPEN_KEYS='F5,Enter'` | The browser plays these keys as the owner's, one each time the window has settled, so a screenshot shows what they lead to (a dialog, the rename field, a report); `text:…` types text. A local setting: **not** forwarded by `--target`. It waits for the window to settle, so it cannot press a key in the middle of a running operation. | S1 |
| `SOPEN_STATE='git=pin'` | The git panel is pinned once the folder's detection answers. | S2 |
| `SOPEN_STATE='git=pin;select=REV;details=1;dialog=ACTION;label=N'` | With the pin: the bar on REV (any revision git accepts) once it is loaded, its details open, the label cursor on the row's Nth label, and ACTION's dialog open (03 §7's action ids) — a confirmation's receipt. | S2 (`dialog=` S3) |
| `SOPEN_LIST=git` | A folder in a work tree: print the git panel's first page as text — one line per row: lane glyphs in ASCII, labels, subject, author, date, hash — then the paging line, instead of opening a window (local and remote). | S2 |
| `SOPEN_REQUIRE_GIT=1` | Tests that need `git` fail instead of skipping when it is missing (CI sets it, as for ffmpeg). | S2 |
| `SOPEN_GIT=<path>` | The local git binary to use (else `git` on PATH). A local setting, not forwarded by `--target`. | S2 |

- The remote git path is tested without ssh (AGENTS.md: tests need no ssh or network): `ShellGit`
  runs the exact words `SshGit` would hand to ssh through a local `sh -c`, so quoting, the locate
  script, the pid marker and stdin are exercised as a remote shell reads them.
- Live receipts on a remote use only repositories created under the remote `/tmp/sopen-receipts/`
  for the purpose (AGENTS.md "Receipts").
- UI rounds compare against the PC Shell look sopen already wears (01 §3a, `chrome.rs`); Git Graph's
  visuals are not a reference (03 §1).
- **For the owner to see at S1–S2's deploy** (kept as the ssot reads it, open to change): dual mode
  never shows git by itself; in preview mode the bar on a file previews it and the bar on a folder in
  a repository shows git.

## 10. Decisions made here

| Decision | Why |
|---|---|
| The other side's content is computed (`view::resolve`), never stored | One rule table to test; no stale side after a mode switch, a pin or a detection answer. |
| O6 follows the active folder's repo, not the folder under the bar | ssot item 3's words; a bar sweep over sibling repos would otherwise reload panels on every key. |
| The pin belongs to a side; while Git has the keyboard, the origin list stays on the other side | Git never shows twice and the list is never lost; the git panel has its own details pane (03 §6). |
| O4: the other side is a snapshot while the tree is open | Moving through the tree would otherwise reload previews and git panels on every key; the snapshot is a resolver input, so the rule stays a pure function. |
| O5 and O6 after 150 ms of rest; a git stream survives 30 s hidden; no refresh younger than 10 s | A sweep over files and folders in a repository would otherwise restart git reads, remote sessions included, on every key. |
| Tab from a List always reaches the other side; a Preview gives way to its panel, and the side left becomes the preview of the new bar | ssot item 3: the panel opposite the active one follows the bar, whichever side is active; the two-panel Tab habit keeps working, and F9 (dual mode) keeps both lists in view. |
| Pin = Cmd/Ctrl+G, unpin = Esc (or Cmd/Ctrl+G again) | ssot item 3 asks for one pin key and Esc; G = git; free in the list (keys.md §3.2). |
| One serialised preview connection per window, the budgets of §4.3, reading without claiming the cache | A bar sweep must not fetch, add sessions or fill the cache; a preview must not cost more than a glance (a 32 MiB read is a few seconds on a slow link; 64 megapixels is 256 MiB of RGBA). |
| Previews are drawn disabled, with their events withheld, and switched to full viewers in place (`set_preview(false)`) | Withheld events alone still let egui's pointer state click; a prepared viewer cannot be cloned, so the live window is what is handed over. |
| At most 3 git commands in flight per host | sshd's `MaxSessions` (OpenSSH default 10) is shared with the SFTP sessions of the listing, a viewer, the preview, the tree and a file operation. |
| Seven slices, each merged and deployed on its own | The owner sees each part as it lands; each slice's integration is small; nothing waits for one big integration. |
| New remote primitives as new traits in submodules | Keeps `sftp.rs`, `local.rs` and `RemoteFs` out of the parallel WUs (§8). |
