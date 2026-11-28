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
| VM-D0 | 🔴 open | 08 §13: 04 §2 and K-R1 brought to the shipped code | orchestrator |
| WV-rest | 🔴 open | 09 §12's WV-C2 and WV-F, in its order (X-W1, WV-0, WV-E1, WV-C, WV-A, WV-B and WV-E2 are closed) | coder |
| X-units | 🔴 open | 10 §17's units after X1–X3 and X4 (every X4 unit closed: X4-kit, X4-lanes, X4-tree, X4-graph, X4-pane, X4-planner, X4-dialogs, X4-find, X4-lists, X4-menu, X4-dialogs2, X4-quick, X4-planner2, X4-state): X4-state2 (the switch state's v3: what is typed in a history edit's own dialog — a quick edit's reword, squash or author, the planner's message and author dialogs — crosses with the switch; X4-state parked it as its slice G, and the switch question says it is dropped until then; whichever of it and X5 merges second takes the next `v`); X5 (= 12's ED-layout); X6-preview, X6-regions, X6-image, X6-media, X6-timeline; X7-pick, X7-draw, X7-video; XF | coder |
| ED-units | 🔴 open | 12 §16's units after ED-buf, ED-enc, ED-ops, ED-save, ED-screen and ED-dialogs (closed): ED-layout (ships X5), ED-git, ED-route, after their 10 units. ED-layout inherits from ED-screen and ED-dialogs: the binding contract (each pass gives its inputs to `EditView::press` until `asking()`, removes what it gave, then calls `dialogs_ui`; 12's ED-layout row); `text::warm()` before `EditView::new`; an IME commit delivered as `Press::Text`; paint through `EditView::paint`; no `io::Read` adapter over the text document's Bulk reader yet; `leave(exit)` at every §6.4 exit and `set_file` each pass; the menu's Copy and Cut come back done, for the clipboard only | coder |

## Follow-ups found on the way (not units yet)

| ID | Status | What | Who |
|---|---|---|---|
| F-mp3vbr | 🔴 open | A 22 kHz VBR mp3 cut comes out misaligned; found while fixing ffmpeg 9.1 (#101), recorded, not fixed | coder |
| F-numpad | 🔴 open | The Ctrl+numpad digit alias reads the physical key first (`src/gui/mod.rs`, `fkey_events`) | coder |
| F-keynames | 🔴 open | Key names outside help still mix forms: "Cmd/Ctrl+R" literals, "⌘/Ctrl" hovers, README line 95; 01, 02 and 03 write `Cmd/Ctrl+Z` | coder |
| F-ci-macos | 🔴 open | A merge to main runs `ci.yml` twice on macOS: CI's own push run and Preview's `gate` (`preview.yml` calls `ci.yml`). Dropping CI's push trigger for main, so that Preview's gate is the one run, would halve the macOS checks per merge while runners are short | orchestrator |
| F-timing | 🔴 open | Wall-clock bounds that fail under a loaded full run and pass alone, seen 2026-11-25: `gui::image::robustness_tests::malformed_headers_never_panic_the_header_read` (one interlaced-PNG case took 2.86 s against `CASE_LIMIT` 2 s, Linux load ~12; alone 5/5), `s3::g_r5_a_waiting_hook…` (no hook pid within 10 s) and `git::tests::runner::a_record_or_an_output_past_its_bound_is_stopped` (each once at load 33–45; alone 5/5 and 3/3), and `s5::d2_the_first_frame_and_the_screens_info_lines_arrive_in_time` (first frame 618 ms in ED-buf's Linux full run; alone 3/3). Bound the work, not the wall clock, as #101–#103 did; `fnkeys::tests::r3_a_dead_watchdog_is_replaced_before_the_next_acquisition` (macOS runner, #139: acquired with a dead watchdog; the replacement became ready before the next focus). Seen 2026-11-26: `s5::d2…` again and `gui::term::tests::a_terminal_that_hung_up_is_an_error_at_once` (X2-gui's Linux full run, load ~13; alone 3/3 each), `gui::image::stream_tests::continue_reads_the_file_and_the_window_becomes_the_image_window` (#146's ubuntu CI; the rerun passed), `gui::term::tests::ts1_with_f1s_help_open_the_terminal_shows_the_windows_cells` at its tmux comparison (`term/tests.rs:969`; #148's ubuntu CI, the rerun passed; once in X3-questions' Mac run, alone and in two reruns it passed), and `a_terminal_that_hung_up_is_an_error_at_once` again on the Mac (1.011 s against its 1 s, X3-binary's run beside receipt windows and a build). Seen 2026-11-27: `gui::hex::tests::stream::questions::r1_nothing_to_write_is_not_due_for_ever` (#152's macOS Preview gate, "a generation was due": the journal's tick ran before the assertion; the rerun passed); `gui::browser::switch_state::tests::the_tree_and_a_git_pin_cross` (X2-switch's Linux full run: the tree's running size, `246B+` against `1.3G+`, differs between the two screens it compares; alone 6/6); `gui::web::mac::tests::wv_a_the_context_menu_offers_no_download_and_no_new_window` (#153's macOS Preview gate; the rerun passed); and `a_terminal_that_hung_up_is_an_error_at_once` twice more (1.0085 s on the Mac at load 8–9, 1.0057 s on Linux). Later on 2026-11-27: `a_terminal_that_hung_up_is_an_error_at_once` again (X4-tree's Linux module runs 1 of 3, its Mac gate at 1.006 s, WV-B's Linux full run at 2.008 s, load ~16); `the_tree_and_a_git_pin_cross` is fixed (#161 roots its trees at its repository). On 2026-11-27 and 11-28: `a_terminal_that_hung_up_is_an_error_at_once` is fixed (#172: the test's pty descriptors were inherited by other tests' children, which kept the terminal open; they are close-on-exec now); a stand-in program started just after it was written failed with ETXTBSY on Linux CI (`s3::a_failing_read_leaves_the_plan_refused`, main after #170), fixed in #176 (every such start goes through `testutil::run_fresh`); `s5::d2…` once more (X4-find's last Linux module run; alone it passed); macOS runners took 8 to 21 minutes for the same suite in one day, and #168's check was cancelled 4 times at the 30-minute limit (#169 raised it to 60) Seen 2026-11-28: `switch_host::tests::tw2_ctrl_c_ends_hosts_input…` past its 11.5 s bound once in X4-state's baseline run on the Mac (passed on every run after). | coder |
| F-cell-select | 🔴 open | Text drawn in cells (the list since #119, the dialogs since XW-dialogs) cannot be selected or copied; main's egui labels could (`selectable_labels`), e.g. a report's failure reason or the clash question's path. A cell selection with Cmd/Ctrl+C, the same in the window and the terminal (where the terminal's own selection works only with mouse reporting off) | coder |
| F-list-perf | 🔴 open | The list in cells costs 0.71 ms a frame in release against main's 0.23 ms (2,000 entries, 1000×700 at 2×; #119): one galley per glyph. One galley per run of one-cell glyphs measured 0.47 ms; it needs 10 §4's "its glyph at its own cell, never through egui's text layout" amended, `cells.rs` paint and its tests, and the kit's `glyphs`/`placed` | coder |
| F-handoff-cover | 🔴 open | A hand-off box partly under a cell dialog or the help is narrowed to the uncovered strip (`│F12 sh…│`, XW-help's receipt); show it whole or not at all. X3-preview (#151) removed the one place it showed in every preview, the egui frame's one-cell ring of strips around a text or hex preview; the narrowing itself is `term.rs`'s, unchanged | coder |
| F-tty-spin | 🔴 open | A hang-up in the middle of a split escape sequence, or during the first entry's keyboard query, can still spin inside crossterm 0.29's mio reader (XW-term's review); crossterm's `use-dev-tty` feature returns on an empty read | coder |
| F-git-kill | 🔴 open | A local `git version` child ended by SIGKILL, once in 20 TUI runs while the test gate ran in parallel, 0 in 20 idle (XW-receipts' receipts): not a stop sopen asked for (`Ending::Signal`, `git/history.rs`); the sender is unknown — memory pressure or a process-group kill by number | coder |
| F-edsave-rest | 🔴 open | ED-save fix-check notes: when the swap-back cannot be recorded the record is cleared (keep it, as for `Kept` with nothing moved, so the next Save moves the other version to its visible name); `move_new`'s link-then-unlink fallback has no test | coder |
| F-content-chrome | 🔴 open | 10's Tensions: the printer treats a text file's `…` and box lines not between letters as chrome and substitutes them in an ambiguous-wide terminal (T38: content must not change); mark viewer cells as content | coder |
| F-text-covered | 🔴 open | `text.rs`'s own `covered()` does not see the list's dialogs, which are drawn after the panels (`text.rs:6379-6400` at #151; X3-preview's coder, the same before it); F8's list and the help are the dialogs it sees. Whether a wide glyph half under a list dialog's edge shows has not been checked | coder |
| F-ed-rest | 🔴 open | ED-screen's deferred review items: §8's keep-the-tail-of-states (an edit truncates every kept state after it; colours stay right); text that Enter moves off the edited line shows plain until the worker answers; per-key clones of the piece list (O(pieces)); `Typed` keeps a second copy of the add buffer, against E17's "the original plus the session's typing, and nothing more"; scroll-bar dragging and clicks on menu names | coder |
| F-ed-frame | 🔴 open | ED-dialogs' window-only limits (several keys within one frame): keys typed after a dialog closes in the same frame are dropped; an open box reads ← → and Tab out of order within one frame; E7's once-per-session undo hint is spent by an undo refused during a Save | coder |
| F-kit-focus | 🔴 open | `ops::dialog::show` drops a focus requested in its first frame (egui's sizing pass ignores it). ED-dialogs works around it in `dialogs.rs` by giving the box its size before the first show; the kit's other dialogs (the hex close prompt among them) share it — fix in `dialog.rs` | coder |
| F-wv-rest | 🔴 open | The web views' residuals: the address window shows no login line for a 401 whose body the engine shows (`Event` carries no status; `mac.rs`), Cmd/Ctrl+R goes to the page while it has the keyboard on macOS (R is not in `route()`'s always-sopen keys), WebKitGTK's error codes are not yet sorted into §7's failure lines; `Scope`/`origin`, the filter-folder helpers and `RULES` are duplicated between `mac.rs` and `gtk.rs`; CI never runs the Linux engine tests (`SOPEN_REQUIRE_WEBKIT=1` needs a compositor; 09's Tensions); the address window's Linux end-to-end receipt (the window on WebKitGTK under a private compositor, a plain address and a loopback tunnel) waits for the Linux host | coder |
| F-kit-tab | 🔴 open | Older than X4-kit (its fix-check): buttons and the first check box still take egui's keyboard click when a Tab reaches them in the same pass — [Space, Tab] on the name-exists dialog's "the same for the rest" also clicks Skip; [Tab, Space] from the Copy dialog's field types a blank and clicks Copy. X4-dialogs' message field with buttons will do the same | coder |
| F-x2-rest | 🔴 open | X2-switch's residuals: the git question tested only by click in a window driver; a destination changed to this machine during a wait could fire the in-process switch when the job ends; Esc on a waited-for copy lets the switch go; a resize during the wait and `term::open`'s finishing of the made browsers are not driven on a real terminal; a link that drops without Ctrl+C gets the 255 line (nothing HOST says tells a window that was up from one never reached) | coder |
| F-git-ids | 🔴 open | egui ids not keyed by the panel: the planner's `Sense::click` buttons, the graph's rows and its foot take ids that two git panels side by side share (the X4 reviews' rule: ids per panel, stateful fields keyed by panel identity) | coder |
| F-restore-scroll | 🔴 open | A restore's entries do not scroll (the kit's choices do not give way): a `files` restore with more untracked paths than about the grid's rows less twelve runs only in a taller window (X4-dialogs' kept limit) | coder |
| F-click-slop | 🔴 open | egui's 5-point click slop (`style.rs`) reaches across cells: a click on the details pane's border or scroll bar puts the bar on that line's row, and a gap of one cell between two buttons presses the one registered last (X4-pane's review). sopen never sets `interact_radius` | coder |
| F-dialog-wrap | 🔴 open | `ops::dialog::wrap` is quadratic in a long line's length (X4-dialogs2's review); the result dialog now wraps once a width, and its kept rows are copied each layout (bounded by the 64 KiB of kept stderr). A part that borrows shared rows and walks only the visible slice removes both | coder |
| F-menu-file | 🔴 open | Shift+F3's file at a revision from the file rows' menu (10 T17's F10 route under legacy encoding), cut from X4-menu; the repository's menu has every other Ctrl+Shift action | coder |
| F-menu-own | 🔴 open | A menu choice that opens a quick edit or an action dialog draws it in the same pass with nothing blanked under it, for that pass: the panel's own covers are taken before `modals` runs (X4-dialogs2's review of X4-menu) | coder |
| F-lost-line | 🔴 open | On a short, narrow grid an action's or a quick edit's lost line can be cut to `…` while Run is on (X4-dialogs, X4-quick) | coder |
| F-write-stub | 🔴 open | Tests write stand-in programs from the test process and warm each one through `testutil::run_fresh`; a `write_stub` that places them through a child (`install -m 755`) would leave the test process no write descriptor and the per-site warm-ups could go (#176's follow-up) | coder |

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
- After merging main into a branch, run the branch's module gate again before the push, not only a
  build: on 2026-11-26 #149 drew the hex close prompt in cells, and three of X2-host's tests broke
  with no conflict in the merge.
- A head that does not contain today's main is not mergeable, however green its checks: on 2026-11-28 X4-lists' head merged
  cleanly with main yet did not build (`GitView::run` took a ctx, and X4-find's tests called it without). The merge script
  refuses such a head; merge main in, run the gate, push.
- Every merge to main runs CI's own push run, and a rerun of an older main run cancels the newer one (CI's concurrency is per
  ref): rerun only the latest.
- unicode-width is pinned to `=0.2.2` (#104's review fix): ratatui-core 0.1.2's `cell_width` adds 1
  per halfwidth sound mark, which 0.2.3 already counts. Lift the pin with a ratatui-core that drops
  its compensation.
