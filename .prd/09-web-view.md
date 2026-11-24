---
title: sopen — web pages in xtools
type: spec
tags: [sopen, spec, xtools, web-view]
---

# 09 — Web pages in xtools: the OS web engine inside the window

Status: planned (2026-11-24)

Answers [`xtools/ssot.md`](xtools/ssot.md) item 5 (owner message 18), inside item 2 (message 23: xtools
is a TUI first; a page, like an image, is GUI-only content) and item 4 (message 20: views 1/2/3 in the
Enter viewer). Measurements and prototypes: [`research/web-view.md`](research/web-view.md), cited
*web* §n. "08" is [`08-viewer.md`](08-viewer.md) (planned; c0a4aad on `docs/xtools-specs`), whose
model units VM-5 (the view set) and VM-6 (the view keys) and GUI binding VM-7 (after XR-T) this
document plugs into. Code citations are at e7df2f0 (in `main`). **[추정]** = inferred here, not
measured; **[미측정]** = the research names it as not measured; **[decision]** = this document's
reversible choice.

Amends, when it lands: 01 A10a and §4a, the CLI help (main.rs:39, :42-44), README's `sopen URL`
paragraph, AGENTS.md's `SOPEN_BROWSER` and `--hold-until-eof` rows plus new rows for `--browser` and
`SOPEN_WEB` (WV-E) and `SOPEN_REQUIRE_WEBKIT` (WV-A); 02 §4.2–§4.3 and 06 §6's channel table (WV-D);
08 §3's HTML/HTM/XML row and D30 (WV-F; Tensions).

## 1. Scope

- **In**: `.html`, `.htm` and stylesheet-carrying `.xml` shown as a page in the xtools browser —
  preview pane and Enter viewer, local or remote (§5, §6); `sopen http(s)://…` as an xtools window
  wherever sopen opens an address (§7); the component (§2), its scheme (§3), the fallbacks (§4).
- **Not in**: Windows (01 §9); a full browser — tabs, history and back/forward, bookmarks, downloads,
  accounts, logins (HTTP auth, client certificates), devtools, printing (ssot "Not in scope");
  Chromium, CEF, any bundled engine, WPE (ssot item 5; *web* §1.2); the standalone `sopen
  host:/x.html` — ssot item 5 names xtools, so it stays 01 A2's editor route until the editor spec
  (message 21) decides; `.xhtml`, `.svg`, `.mhtml` (they stay what 01 §3 makes them); the TUI's
  binding (§8).

## 2. The component

### 2.1 One component, two engines

`src/gui/web/mod.rs` holds `Web`, the one thing a window hosts to show a page, and the `Engine` trait
its two backends implement. Everything above the trait is engine-independent and runs on the testkit
with a recording fake engine (the testkit has no renderer, testkit.rs:1-3).

- `Web::new(content, settings)`, `content` = `Page { server }` (§3) or `Address { origin }` (§7, no
  scheme); `ui(ui, rect)` each frame tells the engine only what changed (rect, scale, visibility);
  `load`, `reload`, `set_scripts` (reloads), `set_preview`, `set_shown`, `snapshot`, `close`. Events
  back: main frame loading / finished / failed, the document on screen, a refused navigation or
  request, the page took the keyboard.
- The engine is picked at the process's first `Web::new`: WKWebView on macOS, WebKitGTK on Linux when
  it loads — else `Web::new` returns the reason and the host shows §4.

### 2.2 macOS: WKWebView in the window

- **Crate**: `objc2-web-kit =0.3.2`, macOS only, the objc2 family already pinned (Cargo.toml:118-127):
  one lock entry, no new duplicate (*web* §3); WebKit.framework linked, start cost within noise (§0).
- **Placement**: a subview of winit's NSView (eframe hands over the window handle, *web* §1.1); the
  view is flipped, so the egui rect in points is the NSRect; `setFrame` only on change (175–398 µs).
  Teardown — `removeFromSuperview`, release, the monitor with the last view — 0.37–3.54 ms, when the
  view closes and for every view before its window goes.
- **Over egui**: a subview sits above the GPU layer; egui cannot draw over it (*web* §1.1, `hitTest`).
  Rule: on a frame where any egui layer above the page's own intersects the page's rect — an
  `egui::Modal` (every sopen dialog, chrome.rs:515-523), a popup or menu, a tooltip, F1's help (a
  Tooltip-order layer, help.rs:269) — the view is hidden (`setHidden`), and shown again on the first
  frame with none; meanwhile the rect shows the panel's background. Decided from egui's layer order
  and rects each frame, never from a list of dialogs **[decision; the layer check 추정]**.
- **Keys**: §2.4, through a local `NSEvent` key monitor (KeyDown) installed with the first view and
  removed with the last (*web* §1.4, measured against a no-monitor control).
- **HiDPI**: AppKit points are egui points and WebKit renders at the backing scale itself (`ppp 2.00`
  measured); a move between displays of different scale is **[추정]**.
- **Preview input**: AppKit delivers mouse events to a subview directly, past egui's withheld frame
  events (preview.rs:2-4). In a preview a **shield** — a transparent NSView of sopen's own over the
  same frame, whose `hitTest:` returns itself and whose mouse and scroll handlers do nothing — covers
  the page **[decision; 추정: not prototyped]**. It goes at the hand-over (§6).
- **Receipts**: eframe's read-back has an empty rect where the page is (*web* §1.1). Before the
  screenshot hook (`screenshot_tick`, gui/mod.rs:1030) writes its PNG, it draws every shown view's
  `takeSnapshot` (10–36 ms) at the view's frame **[decision; 추정: the research composed outside the
  app]**.

### 2.3 Linux: WebKitGTK offscreen, loaded at runtime

- **Loading**: `dlopen("libwebkit2gtk-4.1.so.0")` at the first view; the ~60 GTK, GDK, GLib, cairo and
  WebKit entry points resolve by `dlsym` on that one handle (*web* §2). Nothing is linked, as for
  sopen's other runtime libraries (fnkeys/macos.rs:96-111, remote/local/trash.rs:559-603): `ldd`
  does not change. A missing library fails in ~0.1 ms → §4.
- **Thread**: one GTK thread per process, from the first view: `gtk_init_check` there, blocking in
  `g_main_context_iteration`, woken by `g_main_context_wakeup`; joined (0.5–1.2 ms) when the last
  window closes. Measured working beside winit's Wayland connection (*web* §1.4), though GTK off the
  main thread is not GTK's documented model — a risk.
- **Drawing**: a never-mapped `GtkOffscreenWindow`; on `damage-event` its surface is mapped
  (`cairo_surface_map_to_image`, BGRA premultiplied) into an egui texture painted at the rect, clipped:
  copy 0.34–1.3 ms per damaged frame, upload 0.15–0.48 ms (*web* §1.3). Acceleration `NEVER` (policy
  2; the default aborts offscreen), so GPU-heavy pages run at software speed. One path for X11 and
  Wayland: nothing is embedded (Wayland has no cross-client child; wry takes only Xlib, *web* §1.2).
- **HiDPI**: an offscreen window ignores `GDK_SCALE` (measured). The view is rect × scale in device
  pixels with `webkit_web_view_set_zoom_level(scale)`; scale = winit's `pixels_per_point` (divided by
  GTK's own factor should it ever exceed 1); pointer positions are scaled alike. Measured at 2× via
  egui's zoom on a headless compositor only; fractional scales render at the ceiling and are
  resampled **[추정]**.
- **Input**: pointer moves, buttons, smooth scroll and keys (keyvals, §2.4) as synthesized GdkEvents —
  measured: a click toggled a checkbox, "abc" typed into an `<input>`, Page_Down, a scroll (*web*
  §1.3). Not covered **[미측정]**: IME preedit (Korean composition), the cursor over links, drag and
  drop, the context menu, selection and copy, accessibility.
- The page is egui content: dialogs draw over it, nothing hides, and `SOPEN_SCREENSHOT`'s read-back
  contains it. Teardown: `gtk_widget_destroy` and drain, 0.4–1.0 ms.

### 2.4 Keys and focus (both engines)

- **Two states**: sopen has the keyboard (the page's frame single-lined) or the page has it
  (double-lined: the skin's rule for a side with the keyboard, 02 §4.2). A view opens in the first;
  a click in the page moves to the second (macOS: WebKit becomes first responder **[추정: the
  research drove `makeFirstResponder` in place of a click]**; Linux: a focus-in, then forwarding); a
  click elsewhere in the window moves back **[추정]**.
- **Always sopen's**: Esc, F1–F12, Cmd+W, Cmd+Q (Ctrl+W, Ctrl+Q on Linux) — measured on macOS: without
  the monitor the page swallows them, Cmd+W and Cmd+Q included, past the close prompt; with it they
  reach egui (*web* §1.4) — and Cmd/Ctrl+1…0, 04 §6.1's F-key aliases **[decision; 추정: not in the
  measured set]**. macOS: the monitor hands them to winit's view (`makeFirstResponder`). Linux: never
  forwarded.
- **Esc while the page has the keyboard only takes it back** — the monitor consumes it **[decision;
  추정: the prototype passed it on]**; a second Esc leaves the viewer, as everywhere. The other
  always-sopen keys take the keyboard back and act.
- While the page has the keyboard every other key is its own: typing, Tab (form fields), Cmd/Ctrl+C/V/A
  (copy from the page; *web* §1.4's decisions). macOS IME: WKWebView takes input methods as first
  responder **[추정: Korean not measured]**; Linux: §2.3's gap.

### 2.5 Lifecycle and bounds

- At most two live views per browser window — the preview's and the viewer's (the first becomes the
  second at Enter, §6) — plus, after WV-F, one hidden page view per opened file in 08's set. One per
  address window. WebKit runs one web process per view; it ends with its view **[추정 timing]**.
- No stored data: `WKWebsiteDataStore.nonPersistentDataStore`; on Linux one ephemeral
  `WebKitWebContext` per view (`webkit_web_context_new_ephemeral`). WV-S5 checks.

## 3. The page scheme and its security

### 3.1 What the scheme serves (`stream::page`)

- **URLs** `sopen-page://<view>/<path>`, `<view>` = 128 random bits per view, so no two views share an
  origin and each handler serves its own only **[decision]**. Registered per view —
  `setURLSchemeHandler:forURLScheme:` on its configuration, `webkit_web_context_register_uri_scheme`
  on its context. No loopback port: nothing outside the process can reach it (*web* §4).
- **The root**: the folder the browser was opened on (`browser::Start.abs`, browser.rs:90-113) when the
  file's resolved path is inside it on the same host, else the file's own folder; resolved by
  `realpath` on that host. The page's URL is the file's path relative to the root.
- **A request**: (1) percent-decode, refuse a NUL; (2) join under the root; (3) `realpath`, so a link
  counts by what it points at; (4) served only when that is the root or below — else 403, counted on
  the view's row; (5) `stat`: a regular file is served, a folder serves its `index.html` if any
  **[decision]**, anything else is 404. Steps 3–4 also catch a `..` WebKit did not already normalize
  (*web* §4.1-1) and a link pointing out.
- **The bytes** come through 06's transport: the page's connection becomes a `remote::Ranged` of the
  file (`into_ranged`; `reopen(path)` for another file, ranged.rs:132-136), read at explicit offsets,
  ≤ 1 MiB per piece and ≤ 4 MiB in flight, each piece handed to the engine as it arrives
  (`didReceiveData:`, the request's input stream on Linux). A local page reads through `LocalFs`'s
  `Ranged` (local.rs:251), in place (06 §1). Validation: the handle's `fstat` at open is the
  fingerprint (06 §5); a short reply or an early end fails that request (a network error to the
  engine), never padded.
- **Served in turn** **[decision; cost 추정, X-W1 measures]**: open requests take turns, one piece
  each, so a long response (a large image, an open-ended `bytes=0-`) never holds back the CSS behind
  it. Consecutive turns on one file (a `<video>`'s ranges) keep its handle; a turn on another file
  reopens it (~2 RTT: 24–80 ms at the reference rig's 12–40 ms).
- **Range** `bytes=a-b` → 206 with `Content-Range` and `Accept-Ranges: bytes`; unsatisfiable → 416;
  else 200 with `Content-Length` (measured on both engines, `<video>` seeking on macOS, *web* §4).
- **Type** by extension, a fixed table (html/htm `text/html`, xml `application/xml`, xsl/xslt
  `text/xsl`, and css, js, json, svg, png, jpeg, gif, webp, woff/woff2, ttf/otf, mp4/webm, mp3/ogg/wav,
  pdf); anything else `application/octet-stream`. A text type gets `; charset=utf-8` only when its
  first 8 KiB is valid UTF-8 and names no charset itself (`<meta charset`, `http-equiv`,
  `encoding=`); else none, and the engine decides as a browser does **[decision; that the engine
  honours the header 추정]**.
- **Cancel**: the engine's stop (`webView:stopURLSchemeTask:`; the request's cancellable on Linux)
  ends the request after the piece in flight; nothing is sent to a stopped task (WebKit throws on
  that **[추정]**); closing the view stops all.
- **Channels** (06 §6) **[decision]**: a shown remote page view holds **one** SFTP channel, as a text
  viewer does — in the preview the window's preview session, given back when the bar moves on (06 §13
  item 3, `streamed::Lease`); in the viewer the connection that opened the file (prepare.rs:188-203),
  or the preview's, carried over. A page view hidden in 08's set (WV-F) gives its session back, as 08
  D14 does a save connection, and connects again at its next request. WV-D adds the row to 06 §6.
- **Preview budget** **[decision]**: a preview's page is served at most 32 MiB in all (02 §4.3's bound
  for an image read whole); later requests fail and the page shows what came. The viewer has none.

### 3.2 Engine settings, every view

No JS↔native bridge (no `WKScriptMessageHandler`, no script message handler); no UI delegate
(`window.open` makes nothing, JS dialogs show nothing); devtools off (`inspectable = false`,
`enable-developer-extras = FALSE`); no file URL ever loaded (Linux: `allow-file-access-from-file-urls`
and `allow-universal-access-from-file-urls` off); no downloads (a top-level response the engine cannot
show is cancelled, with a row line); WebKitGTK's web-process sandbox on; WebRTC off where the engine
has a switch (Linux `enable-webrtc = FALSE`).

### 3.3 Scripts

- **Off by default**: `WKWebpagePreferences.allowsContentJavaScript = false`;
  `enable-javascript-markup = FALSE` (*web* §4.1-5).
- **F2** turns scripts on for this view and off again, reloading the document on screen. Its label
  names what it does (`JS on` / `JS off`); the top bar says `scripts off`, or `scripts ON` in the
  warn colour. A new view starts off; the preview is always off (F2 does nothing there).
- **Requests outside the scheme** go through one content rule list, the same JSON on both engines
  (`WKContentRuleListStore`, `WebKitUserContentFilterStore`, *web* §4.1-10), per §3.5's table: with
  scripts on, a script that read the root's files has no URL to send them to. The lists were
  prototyped only as `^https?:` blocks **[미측정]**; WV-S2 goes through the channels.

### 3.4 Links and navigation

- Top-level navigation within the view's own scheme host is followed in the view.
- A clicked link out (`WKNavigationTypeLinkActivated`, `WEBKIT_NAVIGATION_TYPE_LINK_CLICKED`): with
  scripts off, http/https → the target's system browser (`url::open`, url.rs:316), any other scheme
  refused with a line. **With scripts on it is refused** (`scripts are on: links out of the page are
  not followed — F2 turns them off`): a script can make a link and click it (`a.click()`
  **[추정: read as link-activated]**), and its URL would carry what it read.
- Every other navigation out (a redirect, a meta refresh, `location=`, a form) is cancelled; the row
  says where it went, shown as `url::redacted` shows an address.
- No back or forward (non-scope): a link followed inside the root is left with Esc; Enter opens the
  file again.

### 3.5 Threat model: the page is an untrusted file

| The page reaches | Scripts off | Scripts on |
|---|---|---|
| `sopen-page:` under the root, `data:`, `blob:` | shown (`<img>`, `<iframe>`, links) | shown, and readable by script |
| files outside the root, `file:` | no (`realpath` check; no file URLs) | no |
| `http:` / `https:` subresources (CSS, images, fonts) | allowed, so pages look right — residual below | blocked |
| any other scheme (`ws:`, `ftp:`, …) | blocked | blocked |
| navigation out | the owner's click only, to the system browser | none |
| native code, devtools, storage, downloads | none | none |

Accepted residuals. **Scripts off** (decision 4, against *web* §8-6's default of blocking): outside
requests tell the page's author it was opened, when, and the target's address, and a GET reaches any
host the target can reach — its loopback and LAN included (an `<img>` CSRF), as in a browser.
**Scripts on**: macOS has no public WebRTC switch, and a STUN request (UDP) is no URL the rules see
**[추정]**; a script-made `<link rel=dns-prefetch>` may resolve a name built from what it read
**[추정]**. WV-S2 measures both; a leak it finds is named on the `scripts ON` row before WV-D merges.

## 4. Fallbacks

- macOS always has WebKit. Linux without `libwebkit2gtk-4.1.so.0`, or with one that fails to load or
  start (a missing symbol, `gtk_init_check` false): sopen starts and every other window works.
  - A page view → the file's highlighted source (the text viewer, as before 09), first row `no web
    engine here (<reason>): showing the source`.
  - An address → the system browser (`url::open`), as `--browser`, after one stderr line `sopen: no
    web engine here (<reason>); opening <shown> in the system browser`.
- No display (Linux, `display_available`, gui/mod.rs:1206): an address goes to the system browser as
  today. The reason is found once per process; the hook `SOPEN_WEB=off` makes it "turned off
  (SOPEN_WEB=off)" on both systems, for fallback tests and receipts **[decision]**.

## 5. Which files are pages

- `.html`, `.htm`: the page is the default view.
- `.xml`: the page is the default when its first 8 KiB holds `<?xml-stylesheet` before the first
  element; plain XML and RSS default to the highlighted source — both engines show them as run-on text
  (*web* §5) — with the page as another view (§6).
  - **The 8 KiB**: today an `.xml` is decided by extension with nothing read (kind.rs:55,
    :238-240). The page decision reads `kind::SNIFF_BYTES` (kind.rs:30) on the opening's connection
    before the view is made (a local file in place): at most 8 KiB more than today.
  - Only `type="text/xsl"` was measured; `text/css` rendering styled is **[추정]**. XSLT is leaving
    browsers (Chrome 158 stops it on Stable, *web* §5); if WebKit drops it the page shows what the
    engine shows.
- `kind::Kind` is unchanged **[decision]**: these files stay `Text` for the usage log, routing, and VS
  Code / Zed. The decision is a function beside it, `kind::page_default(name, head)`.

## 6. Preview and the Enter viewer

- **The preview pane** shows a page file's default view — the page — in the side's frame, as 02 §4.2
  shows any viewer: prepared after the bar has rested 150 ms (02 §4.1; `view::COMMIT`, view.rs:11),
  one preparation at a time on the window's one preview session (preview.rs:13-15). The worker
  resolves, stats, finds the kind and §5's head; the view is made on the frame. Scripts always off;
  nothing reaches the page (macOS: the shield, §2.2; Linux: nothing forwarded); §3.1's budget. Every
  exit of 02 §4.4 holds: the bar leaving closes the view, stops its requests, gives the session back;
  Enter or F3 hands the view over in place, no reload — the shield goes, §2.4 applies, the scroll stays.
- **The Enter viewer** opens a page file in the page view, filling the viewer's area. Top bar: the
  document on screen (`host:/abs`), `scripts off|ON`, loading, the count of refused requests. F-key
  row `1Help 2JS on 3Exit 4Edit` (F4 = 01 A2's editor on the file, as now; keys.rs:499-503).
  Cmd/Ctrl+R reloads; Esc leaves (§2.4).
- **Until WV-F, the gap**: in xtools a page file shows only as the page; its source and hex are
  reached through F4 (the editor), not inside sopen. The view keys are 08's (§4, D3–D4) and reach the
  GUI with its binding VM-7, after XR-T; binding digits here first would pre-empt them.
- **After 08 (WV-F, which is 08's VM-9)**: the page is one view of 08's set (08 §3). `.html`, `.htm`,
  styled XML: 1 Page · 2 Hex · 3 Text. Plain XML, RSS: 1 Text · 2 Hex · 3 Page. A hidden page view
  shows nothing, takes no input and plays nothing (media paused **[추정 API]**). WV-D needs nothing
  from 08 to ship.

## 7. The address window, and #82's contract

**Today (#82).** `open_url` (main.rs:597-611) opens the system browser (`url::open`, url.rs:316) and
prints `opened … in the browser`; as HOST's half (`--hold-until-eof`, main.rs:69-72) it checks the
port is private, opens the browser, prints `sopen-tunnel-open` (url.rs:288) and holds until its input
ends. The remote side prints `… is open on HOST through a tunnel — Ctrl-C closes it`
(forward.rs:649-668). When HOST's command ends first, the remote exits with HOST's code if ssh exits
within 2 s, else 1 with `… ended while ssh stayed for the browser's connections` (forward.rs:689-718,
01 §4a).

**From 09, on each route to an address:**
1. **Here** (no HOST, or HOST is this machine; main.rs:581): an xtools window with the address view,
   titled with the shown address; `opened <shown> in xtools` once it is up; sopen returns when it is
   closed (exit 0). A change: today the command returns at once.
2. **A tunnel** (loopback with HOST; main.rs:584-586). HOST's half checks the port as today, opens
   the window, prints `sopen-tunnel-open` once the load has started, and a thread reads its input — at
   EOF the window closes and HOST exits 0. **Closing the window** prints the new cue
   `sopen-tunnel-closed` (`url::TUNNEL_CLOSED`: fixed, nothing of the address), tears the view down
   (its connections close **[추정]**) and exits 0. On that cue the remote side closes the tunnel at
   once (its input closed, the guard ends ssh: forward.rs:534-544), prints `<shown>: its window on
   HOST was closed; the tunnel is closed` and exits 0. Any other end of HOST's sopen keeps today's
   rule above. The open line becomes `<shown> is open on HOST through a tunnel — closing its window
   there, or Ctrl-C here, closes it` (its fallback-port form alike).
3. **Forwarded as written** (not loopback; main.rs:589): HOST's sopen opens the window as in 1; the
   remote's run lasts until it closes, as any window opened with `--target`.

- **The view**: top-level navigation within the address's origin (scheme, host, the port it got); a
  clicked link elsewhere → HOST's system browser (a script's `a.click()` may count as a click here
  **[추정]** — the page is the owner's own server); any other navigation out refused with a line;
  subframes and subresources load as in a browser; **scripts on by default**, F2 toggles; no
  `sopen-page` scheme (nothing on disk reachable); §3.2's settings. A site asking for HTTP auth or a
  client certificate is refused with a line naming `--browser`.
- **`--browser`** (new, public): today's behaviour exactly, every route. With a file SOURCE a usage
  error (exit 2), as `--hold-until-eof` (main.rs:520-524). Passed to HOST on both forwarded routes
  (`tunnel_words`, forward.rs:361-364, and the run's words); an older HOST sopen does not know it and
  exits 2 with its own line.
- **Version skew**: cues are matched exactly (forward.rs:634); an older remote prints
  `sopen-tunnel-closed` as a plain line and ends by today's rules; an older HOST never sends it and
  opens its system browser. Both ends are meant to be one sopen (`SOPEN_TARGET_BIN`, the tap).
- **Tests** **[decision]**: tests/cli.rs's `sopen URL` tests use a `SOPEN_BROWSER` stand-in
  (cli.rs:3255-3262) and pass `--browser` from WV-E, keeping every assertion; AGENTS.md's
  `SOPEN_BROWSER` row then serves `--browser`, the fallbacks and outside links.

## 8. The TUI

xtools is a terminal UI by default (ssot item 2); its screen is being researched (XR-T). A page, like
an image or a video, is GUI-only content: in a terminal a page file shows its source (Text is view 1
there), and the GUI form of the same screen shows the page. Whether a picture-capable terminal (kitty,
iTerm2, sixel) shows a snapshot of the page, and the terminal form of every key here, are the TUI
spec's. `sopen URL` with only a terminal: §4's no-display rule.

## 9. Decisions

1–9 are the orchestrator's (2026-11-24); 10–14 are the choices here that change one of them or a
shipped contract (the smaller ones are marked **[decision]** where they are made). All reversible.

| # | Decision | Why |
|---|---|---|
| 1 | One component, two engines: macOS WKWebView subview following the egui rect, hidden under egui layers, key monitor; Linux WebKitGTK 4.1 by dlopen, offscreen into an egui texture, acceleration off, HiDPI by size × scale + zoom, one path for X11 and Wayland | the only routes measured to work inside the window; Wayland has no child views, wry takes only Xlib handles (*web* §1.2) |
| 2 | Without WebKitGTK sopen still starts: a page falls back to its source with a line, an address to the system browser | an optional library must not break sopen (ci.yml:146-164) |
| 3 | Pages load through an in-process scheme fed by the stream layer, never a port; root = the opened folder when the file is in it, else its folder; nothing outside; no `file:` | no local process can reach it; generated docs reference parents (*web* §4, §4.1-1) |
| 4 | Scripts off by default, F2 per view; scripts on → everything outside the scheme blocked; off → outside http(s) subresources allowed; no bridge, devtools, storage | pages look right without scripts; with them, root files cannot leave |
| 5 | A clicked link out: http(s) → system browser, other schemes refused | the view is not a browser (ssot non-scope) |
| 6 | Styled `.xml` defaults to the page, plain XML/RSS to the source (page as another view); `.html`/`.htm` to the page | engines show plain XML as run-on text (*web* §5) |
| 7 | The preview shows the page, scripts off, after the rest, by 02 §4; views and scripts are chosen in the Enter viewer | message 20: modes in the viewer, not the preview |
| 8 | `sopen URL` → an xtools window on the target, limited to its origin; closing it ends the target's half and the tunnel; `--browser` keeps the old route | message 18 item 1 |
| 9 | TUI: a page is GUI-only; the source shows; the binding is the TUI spec's | message 23 (ssot item 2) |
| 10 | Scripts **on** by default in the address window | decision 4's reason (the root) does not apply: no root, the owner's own server; a dev server's page is usually a script app **[추정]** |
| 11 | With scripts on, links out are refused (narrows 5) | closes the `a.click()` path out of decision 4's promise |
| 12 | Esc with the page holding the keyboard only takes it back; Cmd/Ctrl+1…0 reserved too | one press, one action (08 §4); on macOS 04 §6.1's aliases would be lost |
| 13 | `sopen-tunnel-closed` and the remote's exit 0; local `sopen URL` returns when its window closes | a window closed is the normal end (decision 8); today that path is HOST's code or an error |
| 14 | The standalone `sopen x.html` is unchanged | ssot item 5 names xtools; the editor spec owns the standalone route |

## 10. Acceptance

"Do X → observe Y". **[owner]** = needs the owner's hands. On macOS posted clicks do not reach the
page (*web* §1.1), so automated runs drive `makeFirstResponder` and `hitTest`; real clicks are [owner].

- **WV-1** Resize a viewer showing a page → its frame equals the egui rect in points, `setFrame` once
  per change (fake engine). **WV-2** The close prompt and F1 help over a page → macOS: `hitTest` at the
  dialog's centre is not the WKWebView, hidden for those frames, back after Esc; Linux: drawn over it.
- **WV-3** Page holding the keyboard (macOS: `makeFirstResponder`; Linux: a forwarded click), keyvals
  `abc` → in the `<input>`; Esc → keyboard back, viewer open; Esc → it closes. With the page holding
  the keyboard: F3 → leaves; Cmd/Ctrl+1 → help; Cmd+W → the close path; Cmd+Q → the quit path. macOS
  with the monitor off → none reach sopen.
- **WV-4 [owner, macOS, real clicks]** A click into a field, then typing → typed there; a link inside
  the root → followed; an outside https link with scripts off → the system browser; the Korean input
  source in a field → composed Hangul.
- **WV-5 [owner, the Linux laptop, a real scaled output at 2×, 1.25×, 1.5×]** → crisp text at device
  resolution, 1×'s layout, a checkbox click toggles. Korean into a page field: record the result.
- **WV-6** Linux, a headless private compositor (*web* §9's isolation) → a screenshot holds the page's
  pixels; copy ≤ 1.5 ms per damaged frame at 2×; teardown ≤ 2 ms; the release binary's `ldd` unchanged.
- **WV-7** The Linux loader with a missing soname → its reason in < 1 ms. `SOPEN_WEB=off`: Enter on
  `index.html` → the text viewer with the line; `sopen --here http://127.0.0.1:P/` → the stand-in once,
  the stderr line, exit 0. The clean container still passes `sopen --version`.
- **WV-8** Preview → the page ≥ 150 ms after the bar rests, never sooner (fake clock); a bar moving
  within 150 ms makes no view; moving later closes it, stops its requests, and the window keeps one
  preview session; F2 does nothing; Enter → the same view, no second load (requests counted). macOS:
  `hitTest` at the preview's centre is the shield, after the hand-over the page. **[owner, real
  clicks]** a click and a wheel on the preview change nothing (scroll offset, a click counter); the
  same after Enter act (the control).
- **WV-S1** Fixtures with `../`, `%2e%2e`, a link inside the root pointing out, an `/etc/hostname`
  iframe, a `file:` iframe → none served (server log: 403 or never asked); locally and through
  `local_server()`.
- **WV-S2** A hostile page aims `<img>`, fetch, XHR, WebSocket, EventSource, `sendBeacon`, a form POST,
  `location=`, `window.open`, `a.click()`, CSS `url()`, `@font-face`, `<link rel=prefetch|preconnect|
  dns-prefetch>` at a test server on 127.0.0.1:P → scripts on: the server logs **zero** requests and
  the `SOPEN_BROWSER` stand-in is never called; scripts off: only the `<img>`, CSS and font requests
  arrive. A WebRTC STUN server and DNS prefetch: recorded where the platform lets a test see them.
- **WV-S3** `bytes=2809-2812` on a 4 MiB file → 206, 4 bytes, its `Content-Range`; past the end → 416;
  40 ranges on one file → one handle (the Fake's counters); a CSS request behind an open `bytes=0-` on
  a 200 MiB file → answered before that response ends. The 200 MiB file under
  `SOPEN_STREAM_THROTTLE`, its view closed at 10 % → reads stop within ≤ 1 MiB more, no call reaches a
  stopped task. A file rewritten while its page loads → that request fails; Cmd/Ctrl+R shows the new.
- **WV-S4** Korean UTF-8 HTML with no meta charset, and EUC-KR HTML with `<meta charset=euc-kr>` →
  Hangul renders in both.
- **WV-S5** Close a page that set a cookie and localStorage (scripts on) → no website data of sopen on
  disk (WV-A and WV-B name the folders checked); no Inspect item.
- **WV-S6** A remote page in the viewer → one SFTP session (the master's count, as 06 §6 verified); in
  the preview → none beyond the window's preview session.
- **WV-P1** Enter: `.html`, `.htm` → page (local, remote); `feed.xml` → source; `styled.xml` → page,
  its `.xsl` through the scheme; an instruction past 8 KiB → source.
- **WV-P2** A rustdoc-shaped remote tree opened from its top → styled pages; the same file from a
  browser opened inside its subfolder → root = that folder, the upward CSS 403 and counted on the row.
- **WV-A1** `sopen --here http://127.0.0.1:P/` → the window shows the page (screenshot), `opened … in
  xtools`; closing it → exit 0.
- **WV-A2** A stand-in ssh whose HOST half prints the open cue, then the closed cue, while ssh stays →
  the remote exits 0 within 1 s with the closed line; HOST killed instead → today's rule.
- **WV-A3** `--browser` on all three routes → today's output byte for byte; with a file → exit 2. In
  the view: a clicked link to another origin → the stand-in records it; a redirect to another origin →
  refused with a line; a script writes into the page (scripts on by default).
- **WV-A4 [owner]** From the Mac, `sopen --target <the Linux laptop> http://127.0.0.1:<dev server>/` →
  the page in xtools there; closing it → the Mac's sopen prints the line, exit 0; Ctrl-C on the Mac →
  the window there closes.

## 11. Test plan

- **Unit level, no engine**: `stream::page` (WV-C) on the `Fake` `Ranged`, `LocalFs` and
  `local_server()` (off the network, AGENTS.md); `gui::web` (WV-0) and the browser (WV-D) on the
  testkit with a recording `FakeEngine`, 02 §4.5's rows gaining a page; the CLI (WV-E) with the
  stand-in browser and the fake ssh.
- **Engines**: window tests run only with `SOPEN_REQUIRE_WEBKIT=1` (as the other `SOPEN_REQUIRE_*`).
  macOS CI sets it (scheme fixtures with `takeSnapshot` pixels at known places, the monitor driven by
  synthesized NSEvents with its no-monitor control as *web* §1.4, `hitTest` under a Modal and the
  shield). Linux CI **[decision]** installs WebKitGTK's runtime package for these tests only and runs
  them under `xvfb-run` **[추정: the research ran a private kwin on Wayland; X11 takes the same
  path]**; a developer uses *web* §9's private kwin (`env -i`, private runtime folder and bus,
  `--virtual`, never the live session). The clean container (ci.yml:146-164) adds only WV-7's check.
- **Receipts**: `SOPEN_SCREENSHOT` of a page in the preview and the viewer, macOS (composed) and Linux,
  remote ones under `/tmp/sopen-receipts/` (AGENTS.md); a page is ready to shoot once its main frame
  finished or failed and no request is in flight.

## 12. Work units

**X-W1 first** (before WV-C merges and before A's and B's scheme glue): the stream-fed handler is the
load-bearing part the research left unbuilt (*web* §4). A prototype outside the repo shows, on both
engines, a handler answering from a worker thread over a throttled Fake; late pieces accepted on the
engine's thread (macOS main, GTK); a stop mid-response leaving no call on the task; a run of ranges on
one handle; a long-lived response not starving the requests behind it. Its result goes into §3.1.

| ID | Unit | Owns | After |
|---|---|---|---|
| WV-C | **Page server**: root, request steps, turns, Range, type and charset tables, cancel, budget | `src/stream/page.rs`, `src/stream/page/tests.rs` (new); `src/stream/mod.rs` (the `mod` line) | X-W1 |
| WV-0 | **Component core**: `Web`, `Engine`, placement, hiding under layers, key states, scripts, `FakeEngine`, the compose call | `src/gui/web/mod.rs`, `src/gui/web/fake.rs`, `src/gui/web/tests.rs` (new); `src/gui/mod.rs` (`mod web`, the compose call in `screenshot_tick`) | WV-C (its types) |
| WV-A | **macOS engine**: WKWebView, monitor, shield, `takeSnapshot`, scheme glue, content rules | `src/gui/web/mac.rs`, `src/gui/web/mac/tests.rs` (new); `Cargo.toml`, `Cargo.lock` (objc2-web-kit; objc2-app-kit features); AGENTS.md's `SOPEN_REQUIRE_WEBKIT` row | WV-0, X-W1 |
| WV-B | **Linux engine**: loader, GTK thread, offscreen copy, input, HiDPI, scheme glue, rules | `src/gui/web/gtk.rs`, `src/gui/web/gtk/api.rs`, `src/gui/web/gtk/tests.rs` (new); `.github/workflows/ci.yml` | WV-0, X-W1 |
| WV-D | **Page view in the browser**: `page_default`, `Prepared::Page`, preview, viewer, root, F2, fallback row | `src/kind.rs`; `src/gui/web/page.rs` (new); `src/gui/web/mod.rs` (`mod` line); `src/gui/browser/prepare.rs`; `src/gui/browser/preview.rs`; `src/gui/browser.rs`; `src/gui/browser/keys.rs`; `src/gui/chrome.rs` (`PAGE_KEYS`); `src/gui/browser/tests/page.rs` (new); `.prd/02-architecture.md` §4.2–§4.3; `.prd/06-streaming.md` §6 | WV-C, WV-0; each platform's receipts after WV-A / WV-B |
| WV-E | **Address window**: three routes, `--browser`, the closed cue, the lines, the fallback | `src/gui/web/address.rs` (new); `src/gui/web/mod.rs` (`mod` line); `src/main.rs`; `src/url.rs`; `src/forward.rs`; `tests/cli.rs`; `.prd/01-spec.md` (A10a, §4a); `README.md`; `AGENTS.md` (its rows of the header) | WV-0; WV-A, WV-B |
| WV-F | **Page in 08's view set** (= 08's VM-9): §6's rows, hidden page views paused and their session given back | `src/gui/web/page.rs`; with 08 VM-7's binding files (`src/gui/browser.rs`, `src/gui/browser/keys.rs`); 08 §3's row and D30 | WV-D; 08's VM-5, VM-6, VM-7 (after XR-T) |

**Order**: X-W1 → WV-C → WV-0 → WV-A ∥ WV-B → WV-D ∥ WV-E → WV-F. **Only WV-F waits on 08's model**
(VM-5 the view set, VM-6 the keys, VM-7 the binding). WV-D ships before them with the page as the
default view and Esc back (§6 names the gap), and WV-F plugs it into the set. **Shared files are
sequenced**: `src/gui/web/mod.rs` gets one `mod` line each from WV-D and WV-E (the second to merge
rebases); `src/gui/mod.rs` is shared with 08's VM-2 and VM-7 and the browser workstream's input hook;
`browser.rs`, `prepare.rs`, `keys.rs` with 08's VM-7 and the F4 editor, one at a time;
`AGENTS.md` by WV-A then WV-E; in 09 only WV-A touches `Cargo.toml`/`Cargo.lock` and only WV-B
`ci.yml`. A conflict goes to the dispatcher, never settled by dropping the other side (06 §13). A unit
that shows something on screen brings a screenshot someone looked at (06 §13).

## Tensions

- **08 §3 and D9 vs decision 6.** 08's HTML, HTM, XML row makes every XML a page once 09 lands; 09
  §5 narrows it to styled XML. The row becomes §6's two rows when WV-F lands.
- **08 D7 vs plain XML's view 3.** 08 gives Raw as 3 where 1 is text; 09 gives plain XML Page there
  (decision 6, "the page as another view"), so plain XML has no Raw. Unsettled: 08 owns the table.
- **08 D30 vs WV-D before WV-F.** D30 shows HTML as 1 Text, 3 Raw "before 09"; WV-D makes the page
  view 1 before 08's keys reach the GUI, so the in-sopen source waits for WV-F (§6's gap).
- **Decision 5 vs decision 4.** A clicked link out goes to the system browser (5), but with scripts on
  a script can click a link it made, and the URL carries what it read — breaking 4's promise. 09
  refuses links out while scripts are on (decision 11).
- ***web* §8-6 vs decision 4.** The research defaulted to blocking outside subresources; decision 4
  allows them with scripts off. 09 follows decision 4; §3.5 names the residual.
- ***web* §1.4 vs decision 12.** The prototype's monitor passed Esc on after taking the keyboard back;
  09 consumes it — one Esc returns the keyboard, a second leaves.
- **01 A10a vs §7 route 1.** A10a: `sopen URL` opens the default browser and exits 0; from 09 it lasts
  until its window closes. WV-E rewrites A10a.
