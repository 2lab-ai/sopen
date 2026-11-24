# F1…F12 inside sopen on KDE Plasma Wayland — prototype + measurements

2026-11-24. Research unit for the owner's message of the same day ("xtools에서는 … 항상 f1키는 f1키로
동작하도록 … os는 건드리지 말고"). Reverses `.prd/04-file-keys.md` §6.4 and `.prd/research/keys.md`
§4.2 ("no per-app mechanism on Linux"): that conclusion looked only at Fn-lock switches; it missed
the compositor's own per-surface opt-out.

Evidence tags: **[M]** measured here (log path given) · **[S]** read in source (`file:line`) ·
**[추정]** inference, not verified. Log paths are under `logs/` next to this note (copied from
`/tmp/sopen-research-fkeys/logs/` on the Linux laptop). Source checkouts are under `src-dl/` (KWin tag v6.7.5 =
the installed `kwin 6.7.5-1.1`; Linux v7.2 vanilla — the laptop runs 7.2.6 (a CachyOS build), so kernel lines are
[추정] for that build).

## 0. Verdict

**Feasible with side effects (GUI mode).** While a sopen window has keyboard focus it can hold a
`zwp_keyboard_shortcuts_inhibitor_v1`; KWin 6.7.5 grants it with no prompt, stops running its global
shortcuts for that window, and the brightness key then arrives at sopen as an ordinary key, which sopen
turns into `F1`. Nothing outside sopen changes, and the effect ends the moment focus leaves the window
or the process dies. The price: **every** KDE shortcut is off while sopen is focused (Alt+Tab, Meta,
Meta+L, Print, volume, Alt+F4, Ctrl+F1–F4 …) unless sopen hands them back — and handing back over
kglobalaccel's D-Bus works for at least Alt+Tab (§3.3).
**TUI mode: not feasible on Linux** (§5). Two GUI routes both work (§3.1). The per-position key table must come from the owner's receipt
(§6) — never from recall.

Measurement boundary: everything **[M]** ran in a private, headless `kwin_wayland --virtual` with keys
injected through KWin's fake-input device and a stand-in kglobalaccel component registered for
`Monitor Brightness Down`; the real screen backlight, the real PowerDevil and the real keyboards were
not exercised. "The real brightness will not change" is **[추정]** until receipt §6.

## 1. Isolation of the private compositor (before any launch)

`proto/scripts/run.sh` + `inner.sh`: `env -i` (DISPLAY, WAYLAND_DISPLAY, DBUS_SESSION_BUS_ADDRESS
cleared), `HOME`/`XDG_*` = `/tmp/sopen-research-fkeys/home/…`, `XDG_RUNTIME_DIR=/tmp/sopen-research-fkeys/run`
(0700), `dbus-run-session` (own bus), `--socket fkeys-0`, never `--replace`, `--no-lockscreen`. The
driver refuses to start if the bus is `/run/user/1000/bus` or DISPLAY/WAYLAND_DISPLAY is set
(`inner.sh` top); each run's environment is saved as `logs/<exp>/env.txt`. The virtual backend gets a
no-op session (`kwin src/main_wayland.cpp:552`), so no logind seat or VT. The probe binary is built
without winit's X11 backend (`proto/probe/Cargo.toml`), so it cannot fall back to an X display. Every
run ends by killing only its own PIDs (`driver.log` last line); `ps` after each batch showed none left.
The private KWin wrote its config to `/tmp/sopen-research-fkeys/home/.config/` (kglobalshortcutsrc,
kwinrc, kwinoutputconfig.json); `find ~/.config -newer /tmp/sopen-research-fkeys/STAMP-start` for
kwin*/kglobal*/kscreenlocker*/powerdevil*/kxkb* found nothing — the owner's KDE config was not touched.
This isolation was in place from the first launch (E0, 18:06), before the dispatcher's isolation note.

## 2. Q1 — does KWin 6.7 grant the inhibitor, and does a bound key reach the client?

**Yes, both. [M]+[S]**
- **Grant, no prompt.** `inhibit_shortcuts` creates the inhibitor and calls `setActive(true)` at once,
  with no permission check (`src/wayland/keyboard_shortcuts_inhibit_v1.cpp:139-156`). Nothing else ever
  calls `setActive` (grep: only `:155`), so `inactive` is never sent. A second inhibitor for the same
  surface+seat is a protocol error `already_inhibited` (`:146-149`).
  Measured: `inhibit.active (compositor sent 'active')` within the same millisecond as the request
  (`logs/e1/probe-A2.log`).
- **Offered to every client.** The manager is not on KWin's restricted list
  (`src/wayland_server.cpp:138-144`: window management, fake input, screencast, activation feedback,
  lockscreen overlay), and unrestricted interfaces return before any check (`:168-170`). The private
  session's `KWIN_WAYLAND_NO_PERMISSION_CHECKS=1` is read only after that return (`:172`), so it
  opened fake input for the injector and did **not** affect the inhibitor result.
- **Where it applies.** `WaylandServer::isKeyboardShortcutsInhibited` asks only: does the **focused
  keyboard surface** have an active inhibitor (`src/wayland_server.cpp:818-833`). So it is scoped to
  focus by construction.
- **What it skips.** `GlobalShortcutFilter::keyboardKey` calls kglobalaccel only when not inhibited,
  for press/repeat and for release (`src/input.cpp:1076-1087`). kglobalaccel lives inside KWin
  (`src/globalshortcuts.cpp:99-104`, `:189-221`, `src/plugins/kglobalaccel/kglobalaccel_plugin.cpp:27-55`)
  and also decides modifier-only shortcuts such as a bare Meta (measured: E1 row "Meta tap" below). Also skipped: the a11y keyboard
  monitor (screen-reader key grabs, `src/keyboard_input.cpp:286-291`).
- **What still fires (filters before GlobalShortcut, `src/input.h:356-382`):** VT switch
  Ctrl+Alt+F1…F12 (`src/input.cpp:271-290`), the lock screen once locked (`LockScreenFilter`), screen
  edges, drag-and-drop, the window selector, an already open Alt+Tab switcher (`TabBoxInputFilter::keyboardKey`, `:1861-1865`), and the power
  key (handled before the inhibit check, `:1057-1075`). **No escape chord exists in KWin** (no other
  caller of the inhibitor; contrast GNOME's Super+Escape, §4).
- **The key path is the same for real keyboards and the injector:** both are `InputDevice`s whose
  `keyChanged` goes to `KeyboardInputRedirection::processKey` (`src/input.cpp:3240`); XF86MonBrightnessDown
  becomes `Qt::Key_MonBrightnessDown` (`src/xkb.cpp:294`), the key PowerDevil binds.
- **Measured control vs treatment** (`logs/e1/driver.log`, shortcut signals in `logs/e1/shortcuts.log`):

| injected (evdev) | A without inhibitor (P1) | A with inhibitor (P2) |
|---|---|---|
| KEY_BRIGHTNESSDOWN 224 | shortcut fired (0→1), window got nothing | **no shortcut (4→4)**, window got `evdev=224` → synth `F1` → `consume_key` matched |
| Meta tap (modifier-only shortcut) | fired (1→2) | not fired (4→4), window got SuperLeft |
| Alt+Tab | switcher took focus (`wl.leave`) | window kept focus, got Alt + Tab |
| Alt+F4 | window closed by KWin | window alive, got Alt+F4 (`consume_key(NONE,F4)` matched with Alt held) |
| plain F1 | reached the window | reached the window |

## 3. Q2 + Q3 — reading the key under eframe 0.37, and what else changes

### 3.1 The routes (prototype `proto/probe/src/main.rs`; both measured)
- **The inhibitor always needs a guest connection**: winit 0.30.13 has no keyboard-shortcuts-inhibit
  support (keys.md §4.2). `CreationContext` implements `HasDisplayHandle`/`HasWindowHandle`
  (`eframe-0.37.0/src/epi.rs:101-117`) → `wl_display*` + the window's `wl_surface*` →
  `Backend::from_foreign_display` (`wayland-backend-0.3.17/src/sys/mod.rs:102`, guest mode) → bind
  `zwp_keyboard_shortcuts_inhibit_manager_v1` + `wl_seat` → `inhibit_shortcuts(surface, seat)`.
- **Why egui alone cannot see it:** winit maps XF86MonBrightnessDown to `NamedKey::BrightnessDown`
  (`winit-0.30.13/src/platform_impl/linux/common/xkb/keymap.rs:710`) with physical
  `Unidentified(Xkb(224))` (`:261` commented out, `:286` fallback), and egui-winit's `key_from_named_key` ends in
  `_ => None` (`egui-winit-0.37.0/src/lib.rs:1670`); volume keys do map (`:1648-1650`, measured:
  `egui.key native AudioVolumeDown`, `logs/e5/probe-W.log`). `App` has no raw-event method
  (`epi.rs:152-282`).
- **Route A — second `wl_keyboard` on the guest connection** (`--route wl`, default; E1-E4). Every
  client `wl_keyboard` gets the focused surface's keys, so it sees what winit sees, by evdev code, on its
  own thread → channel → `raw_input_hook` pushes `egui::Event::Key{F1…}` → the app's exact-modifier
  `consume_key` matches (`logs/e1/probe-A2.log`: `synth F1 down from evdev 224` → `app.action F1`).
  Keeps sopen's `run_native` call as it is.
- **Route B — eframe's external event loop** (`--route winit`; E5). eframe 0.37 *does* expose winit's
  events this way: `eframe::create_native` returns an `EframeWinitApplication` that implements winit's
  `ApplicationHandler` (`eframe-0.37.0/src/lib.rs:306-381`, `native/run.rs:540-560`); sopen runs its
  own `EventLoop` and wraps the handler, seeing each `WindowEvent::KeyboardInput` before eframe. Measured
  with the inhibitor: `winit.key scancode=Some(224) physical=Unidentified(Xkb(0x00E0))
  logical=Named(BrightnessDown)` → synth F1 → `app.action F1`, shortcut not fired (0→0); control
  without inhibitor: nothing reached the window, shortcut fired (0→1) (`logs/e5/`). The guest
  connection then only holds the inhibitor (no second keyboard, no key thread needed).
- **Decision for the first cut: route A** — it does not touch sopen's run path (the wgpu→glow
  fallback and `event_loop_builder` hook in `src/gui/mod.rs:1300-1340` assume `run_native`). Route B is
  the cleaner runtime (one key source, winit's synthetic-key flag) if sopen later moves to
  `create_native`. Reversible either way.
- Same crate versions as sopen 2f7758a (`Cargo.lock`); the only new feature is wayland-protocols
  `unstable`, which sctk already enables (`smithay-client-toolkit-0.19.2/Cargo.toml:163-170`).

### 3.2 Held only while focused; crash
- **Window-lifetime inhibitor is enough [M]**: with A2 holding its inhibitor and B focused, the
  brightness shortcut fired (4→5); back on A2 it did not (5→5) (`logs/e1/driver.log` P2).
- **Per-focus mode also works [M]**: create on `wl_keyboard.enter`, destroy on `leave` (P3: 6→6 on
  A3, 6→7 on B, 7→7 on A3 again; `logs/e1/probe-A3.log`). Not needed on KWin; harmless.
- **Crash/kill**: client death destroys its resources; the inhibitor's `destroy_resource` removes it
  from the table (`keyboard_shortcuts_inhibit_v1.cpp:83-89`) [S]. After `kill -9` of A2 the next
  press fired the shortcut (5→6) [M — focus had moved to B, so this shows recovery, not the table removal itself].
- **Design rules found [S]+[M]:**
  - KWin keys inhibitors by raw `SurfaceInterface*` and removes the entry only when the **inhibitor**
    goes (`:58, :146, :170`). Destroy the inhibitor **before** the window's surface (prototype:
    `on_exit`, `logs/e1/probe-A4.log`), or a dangling entry outlives the surface.
  - The guest thread outlives `on_exit`: `wl.leave surface=<anonymous>@0` arrives after `probe: exit`
    (`logs/e1/probe-A4.log`); the null surface parsed without a panic. sopen must stop that thread
    before eframe drops the `wl_display`.
  - KWin sends `leave` when the focused surface is destroyed (`src/wayland/keyboard.cpp:176-178`).
  - Our `wl_seat` is bound at v7, so no `repeated` key state; F-key actions don't repeat anyway.

### 3.3 What else changes while sopen is focused, and the hand-back
- Off while focused [S] (all go through kglobalaccel): Alt+Tab/Meta+Tab, Meta (launcher), Meta+L
  (lock), Print/Spectacle, volume, mic mute, screen and keyboard brightness, Ctrl+F1–F4, Meta+F5–F10,
  Alt+F1–F4, Meta+W, Meta+D, layout switch Meta+Alt+K (owner's bindings:
  `~/.config/kglobalshortcutsrc` lines 17-24, 29, 45, 58, 60, 69, 112, 122, 242). They arrive in sopen
  as keys instead, so sopen's own key table now sees Alt+F4, Ctrl+F1 etc.
- Still on: power key, Ctrl+Alt+F-n VT switch, pointer, touchpad gestures, screen edges.
- Kernel-handled positions never become keys at all (inhibitor irrelevant) [S, 추정 for the CachyOS build]:
  keyboard backlight on the N-KEY/folio path (`hid-asus.c:369-375`; `asus-wmi.c:4623-4637`), the fan
  key (`hid-asus.c:416-420`), Fn+Esc Fn-lock on asus-wmi (`asus-wmi.c:4639-4643`). Screen brightness is
  **not** done in the kernel here: `/sys/class/backlight` has only `amdgpu_bl1`, so acpi_video's own
  switch returns early (`acpi_video.c:1414`) and asus-wmi only handles it for vendor backlight
  (`asus-wmi.c:4614-4618`).
- **Hand-back [M]**: kglobalaccel answers "which action is bound to this chord" with the deprecated
  `action(int qtKey)` (`Alt+Tab` → `kwin / Walk Through Windows`, `logs/e3/driver.log`);
  my `globalShortcutsByKey` call failed only because I sent signature `(ai)i` — the method takes
  `(ai)(i)` (`/usr/share/dbus-1/interfaces/kf6_org.kde.KGlobalAccel.xml`); not retried. Then
  `/component/kwin invokeShortcut("Walk Through Windows")` from the session bus switched focus to B
  (`logs/e3/probe-B.log` `wl.enter` at the call). E4 (Alt physically held): see §3.4. zbus 5.19 is
  already in sopen's tree (via accesskit_unix, `Cargo.lock`), so hand-back adds no crate.

### 3.4 E4 — hand-back with Alt physically held [M]
One injector process (so Alt stays down; KWin releases a fake device's keys when its client goes,
`src/backends/fakeinput/fakeinputbackend.cpp:101-109`): Alt down and Tab reach inhibited A; while Alt
is still down, `invokeShortcut("Walk Through Windows")` → A gets `wl.leave` at the call (the switcher
takes the keyboard); Alt released 1.6 s later → B gets `wl.enter` 9 ms after the release
(`logs/e4/inject.log`, `probe-A.log`, `probe-B.log`). So a handed-back Alt+Tab behaves like the native
one. Further Tabs while the switcher is open go to `TabBoxInputFilter`, which runs before the
GlobalShortcut filter and is not inhibit-gated (`src/input.cpp:1840, 1861-1865`) [S, not measured].
- The design that follows: sopen keeps a short list of chords it never wants (Alt+Tab, Meta+Tab,
  Meta, Meta+L, Print, Alt+F4 …) or simply every chord whose `action(qtKey)` lookup is non-empty and is
  not in its own key table, and invokes the bound action. [가설] for modifier-only Meta (its trigger is
  "released without another key", which sopen would have to reproduce) and for the lock screen (not run
  here: `--no-lockscreen`).

## 4. Q4 — other per-app routes (no OS setting changed)
- **None found besides the inhibitor.** Near miss: kglobalaccel's D-Bus `blockGlobalShortcuts(true)`
  deactivates every shortcut (`kglobalacceld v6.7.5 src/kglobalacceld.cpp:286-294`), but it is
  session-wide state (every app loses shortcuts, not just sopen) and nothing restores it if sopen dies —
  the macOS-style global switch, rejected while the per-surface inhibitor exists.
- Rejected as OS config changes: KWin window rules ("Ignore
  global shortcuts"), kglobalshortcutsrc edits, Fn-lock module params/sysfs, keyd/kanata/uinput remappers
  (need `/dev/uinput`/`input` group; the owner's `open-au05 daemon` + `open-au05 virtual input` show such
  access exists for that device, but using it is a system setup, not per-app).
- **GNOME [S, docs only]**: mutter asks the shell, which shows "Allow inhibiting shortcuts" (Allow /
  Deny) and remembers the answer per app ID in the permission store (`gnome-shell
  js/ui/inhibitShortcutsDialog.js:13-14, 70-104, 121-148`; window-backed apps without an app ID are
  asked every time, `:47-49`). Escape: `restore-shortcuts`, default `<Super>Escape`
  (`mutter data/org.gnome.mutter.wayland.gschema.xml.in:58-61`).
- **sway [S]**: `seat … shortcuts_inhibitor enable|disable|…`, honoured by default
  (`sway/sway-input.5.scd:302-312`). wlroots-based others: per compositor [추정].
- X11 sessions: no such protocol; out of scope (the laptop is Wayland).

## 5. TUI mode (xtools in a terminal) — sources only
- **Linux/Konsole: the top row's media keys never reach the TUI.** KWin consumes bound keys before
  Konsole (`input.cpp:1076-1082`); Konsole holds no inhibitor or keyboard grab (no match for
  keyboardGrab / shortcuts-inhibit in Konsole `src/` at tag v26.08.1 = the installed `konsole 26.08.1`,
  nor in QtWayland v6.11.2 `src/` = the installed `qt6-wayland 6.11.2`; Konsole lines below are v26.08.1). A TUI process cannot hold one for its
  terminal: the inhibitor names a `wl_surface` of the requesting client, and over ssh there is no
  Wayland at all. A real F1 does arrive: Konsole's default keytab sends `\EOP` (`data/keyboard-layouts/
  default.keytab:167`); with the kitty keyboard protocol (Konsole's `src/KittyKeyMap.cpp`) F1 is `CSI P`-family
  (`KittyKeyMap.cpp:33`). Brightness has **no** code in
  the kitty protocol (`keyboard-protocol.rst:601-652` lists media and volume, not brightness); Konsole's
  keytab has no media entries (0 matches).
- **Konsole's own F-key shortcuts (v26.08.1 defaults):** it frees F1 (help) and F10 (menu → Shift+F10)
  for the terminal (`src/MainWindow.cpp:254-263`) but keeps **F11 = Full Screen**
  (`:493-494`); its ShortcutOverride list does not include F11 (`TerminalDisplay.cpp:3044-3088`), so a
  plain F11 never reaches a TUI in default Konsole [S]. sopen's row uses F1-F10, so this bites only
  if slot 11 is ever used.
- **So in TUI mode the top row is F-keys only where the keyboard itself sends F-codes** (Fn held, or a
  keyboard Fn-lock), plus the clickable row / digit aliases. Konsole binds Alt+1…0 to "Switch to Tab"
  (`src/ViewManager.cpp:389, 392`); Ctrl+digits are only distinguishable with the kitty protocol's
  disambiguate flag (`keyboard-protocol.rst:342-350`).
- **macOS TUI** (this Mac: Ghostty — `TERM_PROGRAM=ghostty` — and Terminal.app running; herdr in
  between): the HIDFKeyMode switch (04 §6.2) is process-agnostic, so a *local* TUI sopen could flip it
  while its terminal has focus, using focus reports (DECSET 1004; Konsole implements it at
  `Vt102Emulation.cpp:2300-2301`, Ghostty/Terminal.app [추정]). Over ssh the remote TUI cannot reach the
  Mac's HID setting. [가설] — not prototyped.
- Tension: Konsole's kitty media codes are shifted from the spec (VolumeDown 57436 vs spec
  LOWER_VOLUME 57438, VolumeUp 57438 vs 57439, Mute 57439 vs 57440: `KittyKeyMap.cpp:105-107` vs
  `keyboard-protocol.rst:651-652`). Irrelevant for brightness; matters only if a TUI keyed on volume.

## 6. Owner-assisted receipt (the Linux laptop, real session; changes no setting)
Files: `proto/scripts/receipt.sh` (= `the Linux laptop:/tmp/sopen-research-fkeys/proto/scripts/receipt.sh`),
probe binary `/tmp/sopen-research-fkeys/target/debug/fkeys-probe`. Steps for the owner, at the laptop:
1. Attach the keyboard to test (the folio = the laptop's own top row; repeat later for others).
2. In Konsole inside the Plasma session: `bash /tmp/sopen-research-fkeys/proto/scripts/receipt.sh folio`
   (if it says the binary is missing: `bash /tmp/sopen-research-fkeys/build.sh`, about a minute).
3. Step 1 CONTROL window (no inhibitor): press the key printed F1 → watch the screen → Next; the key
   printed F2 → Next; answer y/n in the terminal.
4. Step 2 INHIBIT window: press exactly the key each prompt names — Esc, F1…F12 without Fn, F1…F12 with
   Fn held, Alt+Tab, Meta — it advances by itself ~1 s after a key; "Next" if nothing happens, "Back"
   to redo. Answer the two y/n questions.
5. Send back the printed summary (or the saved `/tmp/sopen-research-fkeys/receipt-folio-*.log`).

What it settles: (a) the position → evdev-code table, without and with Fn (sopen's map); (b) the
inhibitor is granted in the real session (`inhibitor granted … yes`); (c) control: the backlight sysfs
value changes **and** the owner saw it — if sysfs does not move, the "no change" in step 2 is read from
the owner's answer only; (d) step 2: no backlight change, and Alt+Tab/Meta land in the window; (e) which
positions never arrive (kernel-handled, §3.3). The window logs every backlight change from a 50 ms poll,
independent of keys (a key KWin eats never reaches the window).
Dry run in the private KWin (injector as the fingers, a pretend media-first keyboard): the script,
the per-prompt grouping, the Next click and the summary all work (`logs/e2/receipt-stdout.txt`,
`logs/e2/receipt-virtualtest-20261124-182807.log`; signals 0→1 in control, 1→1 through all of step 2,
`logs/e2/driver.log`).

## 8. What sopen would add (GUI mode, Linux Wayland) — for the spec, not built
- Only when the display is Wayland and the compositor offers the manager; else today's fallback (04 §6.1).
- `fkeys = "function"` (default) holds one inhibitor for the window's life; `"system"` holds none —
  the same config key as macOS (04 §6.2).
- The table position → code comes from §6, keyed by evdev code; codes outside it are ignored.
- Hand-back of KDE chords over kglobalaccel (zbus, already in the tree), §3.3-3.4.
- Destroy the inhibitor, then stop the key thread, before eframe drops the window and display (§3.2).
- 04 §6.4's "Linux: no per-app mechanism" becomes: none at the keyboard/kernel level; on Wayland the
  compositor's shortcuts inhibitor is one (KWin: granted silently; GNOME: asks once per app).

## 7. Q5 macOS, Q6 Windows (short)
- **macOS: no route without the temporary switch that also avoids a TCC grant.** An NSEvent *local*
  monitor sees only events dispatched through `-[NSApplication sendEvent:]` to the app
  (`AppKit NSEvent.h:542-547`); brightness/volume/Mission Control on the built-in keyboard are vendor or
  consumer usages acted on by the system (keys.md §1 `FnFunctionUsageMap`), and winit's macOS backend has
  no `SystemDefined` handling (0 files match). Suppressing the system action needs an **active
  CGEventTap** on `NX_SYSDEFINED` (FunctionFlip `FFHelperApp.m:196-197`). The SDK header names the
  Accessibility requirement only for key up/down events (`CoreGraphics CGEvent.h:272-278`); FunctionFlip
  still demands Accessibility before tapping (`FFHelperApp.m:158-178`). Whether an active NX_SYSDEFINED
  tap works on macOS 26 without Accessibility or Input Monitoring: [추정 — not tested, no keys pressed
  here]. Any TCC grant would land on the terminal, since sopen is a bare binary (keys.md §4.1 item 2). Whether brightness even appears as NX_SYSDEFINED on Apple-silicon built-in
  keyboards: [추정], not checked. Keep 04 §6.2.
- **Windows**: sopen does not build there (01 §9); not researched.
