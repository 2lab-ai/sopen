# xtools — loop (open items only)

Status: round 1 (code), opened 2026-11-25; round 0 (research and specs) is closed. Target:
[`ssot.md`](ssot.md). Contracts: [`../08-viewer.md`](../08-viewer.md) (view modes and encodings),
[`../09-web-view.md`](../09-web-view.md) (web pages), [`../10-tui.md`](../10-tui.md) (TUI first, the
GUI the same cells), [`../12-editor.md`](../12-editor.md) (the built-in editor). Design decisions go there, not here; each unit's row in those documents' work
unit tables says what it owns and what it waits for.

## Round 0 — research

Closed: VR-M, VR-W, XR-T, XR-E and X-SPEC — [`loop.done.md`](loop.done.md) R0 parts 1–3 and R1 part 3.

## Round 1 — code

Open units, in each document's order. Closed units are in [`loop.done.md`](loop.done.md) R1.

| ID | Status | What | Who |
|---|---|---|---|
| X2-host | 🟡 running | 10 §17's X2: HOST's half, `--state-stdin`, the `sopen-xtools-state` and `sopen-xtools-kept` cues; branch `feat/x2-host`. §7.3's [추정] measured 2026-11-26: a command run by `ssh -T` kept running after the local ssh was killed, no SIGHUP | coder |
| X2-switch | 🔴 open | 10 §17's X2: the TUI's half over ssh, §7.3's outcomes, the question before switching; ships X2 (after X2-host) | coder |
| X3-questions | 🟡 running | 10 §17's X3: the hex window's questions and rows, and Save As, in cells; branch `feat/x3-questions` | coder |
| X3-preview | 🟡 running | 10 §17's X3: the preview side's frame and progress line in cells; branch `feat/x3-preview` | coder |
| VM-D0 | 🔴 open | 08 §13: 04 §2 and K-R1 brought to the shipped code | orchestrator |
| WV-rest | 🔴 open | 09 §12's WV-C2, WV-B, WV-E2 and WV-F, in its order (X-W1, WV-0, WV-E1, WV-C and WV-A are closed) | coder |
| ED-units | 🔴 open | 12 §16's units after ED-buf, ED-enc, ED-ops and ED-save (closed): ED-screen, ED-dialogs, ED-layout (ships X5), ED-git, ED-route, after their 10 units | coder |

## Follow-ups found on the way (not units yet)

| ID | Status | What | Who |
|---|---|---|---|
| F-mp3vbr | 🔴 open | A 22 kHz VBR mp3 cut comes out misaligned; found while fixing ffmpeg 9.1 (#101), recorded, not fixed | coder |
| F-numpad | 🔴 open | The Ctrl+numpad digit alias reads the physical key first (`src/gui/mod.rs`, `fkey_events`) | coder |
| F-keynames | 🔴 open | Key names outside help still mix forms: "Cmd/Ctrl+R" literals, "⌘/Ctrl" hovers, README line 95; 01, 02 and 03 write `Cmd/Ctrl+Z` | coder |
| F-ci-macos | 🔴 open | A merge to main runs `ci.yml` twice on macOS: CI's own push run and Preview's `gate` (`preview.yml` calls `ci.yml`). Dropping CI's push trigger for main, so that Preview's gate is the one run, would halve the macOS checks per merge while runners are short | orchestrator |
| F-timing | 🔴 open | Wall-clock bounds that fail under a loaded full run and pass alone, seen 2026-11-25: `gui::image::robustness_tests::malformed_headers_never_panic_the_header_read` (one interlaced-PNG case took 2.86 s against `CASE_LIMIT` 2 s, Linux load ~12; alone 5/5), `s3::g_r5_a_waiting_hook…` (no hook pid within 10 s) and `git::tests::runner::a_record_or_an_output_past_its_bound_is_stopped` (each once at load 33–45; alone 5/5 and 3/3), and `s5::d2_the_first_frame_and_the_screens_info_lines_arrive_in_time` (first frame 618 ms in ED-buf's Linux full run; alone 3/3). Bound the work, not the wall clock, as #101–#103 did; `fnkeys::tests::r3_a_dead_watchdog_is_replaced_before_the_next_acquisition` (macOS runner, #139: acquired with a dead watchdog; the replacement became ready before the next focus). Seen 2026-11-26: `s5::d2…` again and `gui::term::tests::a_terminal_that_hung_up_is_an_error_at_once` (X2-gui's Linux full run, load ~13; alone 3/3 each), and `gui::image::stream_tests::continue_reads_the_file_and_the_window_becomes_the_image_window` (#146's ubuntu CI; the rerun passed) | coder |
| F-cell-select | 🔴 open | Text drawn in cells (the list since #119, the dialogs since XW-dialogs) cannot be selected or copied; main's egui labels could (`selectable_labels`), e.g. a report's failure reason or the clash question's path. A cell selection with Cmd/Ctrl+C, the same in the window and the terminal (where the terminal's own selection works only with mouse reporting off) | coder |
| F-list-perf | 🔴 open | The list in cells costs 0.71 ms a frame in release against main's 0.23 ms (2,000 entries, 1000×700 at 2×; #119): one galley per glyph. One galley per run of one-cell glyphs measured 0.47 ms; it needs 10 §4's "its glyph at its own cell, never through egui's text layout" amended, `cells.rs` paint and its tests, and the kit's `glyphs`/`placed` | coder |
| F-handoff-cover | 🔴 open | A hand-off box partly under a cell dialog or the help is narrowed to the uncovered strip (`│F12 sh…│`, XW-help's receipt); show it whole or not at all | coder |
| F-tty-spin | 🔴 open | A hang-up in the middle of a split escape sequence, or during the first entry's keyboard query, can still spin inside crossterm 0.29's mio reader (XW-term's review); crossterm's `use-dev-tty` feature returns on an empty read | coder |
| F-git-kill | 🔴 open | A local `git version` child ended by SIGKILL, once in 20 TUI runs while the test gate ran in parallel, 0 in 20 idle (XW-receipts' receipts): not a stop sopen asked for (`Ending::Signal`, `git/history.rs`); the sender is unknown — memory pressure or a process-group kill by number | coder |
| F-edsave-rest | 🔴 open | ED-save fix-check notes: when the swap-back cannot be recorded the record is cleared (keep it, as for `Kept` with nothing moved, so the next Save moves the other version to its visible name); `move_new`'s link-then-unlink fallback has no test | coder |
| F-content-chrome | 🔴 open | 10's Tensions: the printer treats a text file's `…` and box lines not between letters as chrome and substitutes them in an ambiguous-wide terminal (T38: content must not change); mark viewer cells as content | coder |

## Build facts

- Gate, per WU and before every merge: `just check` with `SOPEN_REQUIRE_FFMPEG=1
  SOPEN_REQUIRE_SFTP_SERVER=1` (AGENTS.md), plus the Linux gate's two clippy runs (browser loop,
  2026-11-24).
- Size on 2026-11-24 (`wc -l`): `src/gui` 136,614 lines of 245,970 in `src`, tests included.
- Timing tests (v16, v22, s2's parked streams, s6's k_d1) are load-sensitive. On 2026-11-24 three of
  them needed test-only fixes (#101, #102, #103): ffmpeg 9.1 renders a short clip before its first
  progress report, and a dropped stream gives its slot back on a thread of its own. Keep at most
  three compiling units on one machine.
- macOS arm64 hosted runners are short. On 2026-11-24 and 2026-11-25 Preview's macOS build jobs
  were cancelled after 15 minutes of waiting, with GitHub's annotation "The job was not acquired by
  Runner of type hosted even after multiple attempts" and the notice "Due to capacity constraints,
  jobs targeting macOS arm64 runners may experience longer queue times". A rerun of just the failed
  jobs passed each time. Every merge to main starts two macOS checks (CI's push run and Preview's
  gate) plus two macOS builds, about 26 minutes each for the checks; merge, let the Preview
  finish, then push the next PRs.
- unicode-width is pinned to `=0.2.2` (#104's review fix): ratatui-core 0.1.2's `cell_width` adds 1
  per halfwidth sound mark, which 0.2.3 already counts. Lift the pin with a ratatui-core that drops
  its compensation.
