---
title: sopen — web pages in xtools
type: spec
tags: [sopen, spec, xtools, web-view]
---

# 09 — Web pages in xtools: the OS web engine inside the window

Status: planned (2026-11-24; review round 1 applied the same day)

Answers [`xtools/ssot.md`](xtools/ssot.md) item 5 (owner message 18), inside item 2 (message 23: xtools
is a TUI first; a page, like an image, is GUI-only content) and item 4 (message 20: views 1/2/3 in the
Enter viewer). Measurements and prototypes: [`research/web-view.md`](research/web-view.md), cited
*web* §n. "08" is [`08-viewer.md`](08-viewer.md): its model units VM-5 (the view set) and VM-6 (the
view keys) and its GUI binding VM-7. "10" is the screen spec being written from XR-T
([`research/tui-gui.md`](research/tui-gui.md), cited *XR-T* §n). Code citations are at e7df2f0 (in
`main`). **[추정]** = inferred here, not measured; **[미측정]** = the research names it as not
measured; **[decision]** = this document's reversible choice.

Amends, when it lands: 01 A10a and §4a, the CLI help (main.rs:39, :42-44), README's `sopen URL`
paragraph, AGENTS.md's `SOPEN_BROWSER` and `--hold-until-eof` rows plus new rows for `--browser`
(WV-E1), `SOPEN_WEB` (WV-0) and `SOPEN_REQUIRE_WEBKIT` (WV-A); 02 §4.2–§4.3 and 06 §6's channel table
(WV-D); 08 §3's HTML/HTM/XML row, D7, D9 and D30 (WV-F).

## 1. Scope

- **In**: `.html`, `.htm` and stylesheet-carrying `.xml` shown as a page in the xtools browser —
  preview pane and Enter viewer, local or remote (§5, §6); `sopen http(s)://…` as an xtools window
  wherever sopen opens an address (§7); the component (§2), its scheme (§3), the fallbacks (§4).
- **Not in**: Windows (01 §9); a full browser — tabs, history and back/forward, bookmarks, downloads,
  accounts, logins (HTTP auth, client certificates), devtools, printing (ssot "Not in scope");
  Chromium, CEF, any bundled engine, WPE (ssot item 5; *web* §1.2); the standalone `sopen
  host:/x.html` — ssot item 5 names xtools, so it stays 01 A2's editor route until the editor spec
  (message 21) decides; `.xhtml`, `.svg`, `.mhtml` as page files (they stay what 01 §3 makes them);
  the TUI's binding (§8).

## 2. The component

### 2.1 One component, two engines

`src/gui/web/mod.rs` holds `Web`, the one thing a screen hosts to show a page, and the `Engine` trait
its two backends implement. Everything above the trait is engine-independent and runs on the testkit
with a recording fake engine (the testkit has no renderer, testkit.rs:1-3). Under message 23's
architecture (*XR-T* §0) the component is a **pixel region** the GUI backend hosts at a cell
rectangle (*XR-T* §6 placed a native subview at a cell rect on macOS; on Linux the page is a texture
region, as a picture is). WV-D binds it as a pixel region of 10's GUI backend when 10 has landed, on
today's egui windows (the same rect, given by egui) when it has not; WV-F binds it per 10 either way.

- `Web::new(content, settings)`, `content` = `Page { server }` (§3) or `Address { origin }` (§7, no
  scheme); `ui(ui, rect)` each frame tells the engine only what changed (rect, scale, visibility);
  `load`, `reload`, `set_scripts` (reloads), `set_preview`, `set_shown`, `snapshot`, `close`. Events
  back: main frame loading / finished / failed, the document on screen, a refused navigation or
  request (with the count of outside requests blocked), the web process ended.
- The engine is picked at the process's first `Web::new`: WKWebView on macOS, WebKitGTK on Linux when
  it loads — else `Web::new` returns the reason and the host shows §4. `SOPEN_WEB=off` forces that
  reason (§4, WV-0).
- **The shell's part** (`gui/mod.rs`, WV-0, sequenced with 08's VM-7): the window's NSView handle
  for the engine (through `Shell`), the after-frame placement and hide check (§2.2), the teardown of
  every view before the window goes, and the macOS key monitor's install and removal.

### 2.2 macOS: WKWebView in the window

- **Crate**: `objc2-web-kit =0.3.2`, macOS only, the objc2 family already pinned (Cargo.toml:118-127):
  one lock entry, no new duplicate (*web* §3); WebKit.framework linked, start cost within noise (§0).
- **Placement**: a subview of winit's NSView (eframe hands over the window handle, *web* §1.1); the
  view is flipped, so the egui rect in points is the NSRect; `setFrame` only on change (175–398 µs).
  Teardown — `removeFromSuperview`, release, the monitor with the last view — 0.37–3.54 ms, when the
  view closes and for every view before its window goes.
- **Over egui**: a subview sits above the GPU layer; egui cannot draw over it (*web* §1.1, `hitTest`).
  **The decision is made in `Shell` after the frame** — after the window's pass and the shell's
  after-pass painters, F1's help among them (a painter-only Tooltip-order layer drawn in
  `help::end_pass`, help.rs:261-270, called from the shell's `on_end_pass`, gui/mod.rs:802-803). A
  view is hidden (`setHidden`) for that frame when any egui layer above the page's own intersects its
  rect — an `egui::Modal` (every sopen dialog, chrome.rs:515-523), a popup or menu, a tooltip, that
  help — **or when the frame did not place it** (its host did not draw it); it is shown again on the
  first frame placed with none. Meanwhile the rect shows the panel's background. Decided from egui's
  layer order and rects, never from a list of dialogs **[decision; the layer check 추정]**.
- **Keys**: §2.4, through a local `NSEvent` key monitor (KeyDown) installed with the first view and
  removed with the last (*web* §1.4, measured against a no-monitor control).
- **HiDPI**: AppKit points are egui points and WebKit renders at the backing scale itself (`ppp 2.00`
  measured); a move between displays of different scale is **[추정]**.
- **Preview input**: AppKit delivers mouse events to a subview directly, past egui's withheld frame
  events (preview.rs:2-4). In a preview a **shield** — a transparent NSView of sopen's own over the
  same frame, whose `hitTest:` returns itself and whose mouse and scroll handlers do nothing — covers
  the page, and the preview's view never becomes first responder (sopen never makes it one; one found
  first responder is moved back to winit's view) **[decision; 추정: not prototyped]**. The shield goes
  at the hand-over (§6).
- **Receipts**: eframe's read-back has an empty rect where the page is (*web* §1.1). The screenshot
  hook (`screenshot_tick`, gui/mod.rs:1030) first takes every shown view's `takeSnapshot` (10–36 ms),
  then requests the read-back, and draws each snapshot at its view's frame before writing the PNG
  **[decision; 추정: the research composed outside the app]**.
- **ATS**: App Transport Security may refuse plain `http` to a named host from WKWebView **[추정]**;
  IP addresses and `localhost` are exempt by Apple's rules **[추정]**. WV-A measures `http://` to an
  IP, to `localhost` and to a dotted name. sopen never weakens ATS (no Info.plist exception); a
  refused load says so with a line naming `--browser` **[decision]**.

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
  (double-lined: the skin's rule for a side with the keyboard, 02 §4.2). On macOS the state is read
  from AppKit's first responder each frame, never tracked from clicks; on Linux it is sopen's own. A
  view opens in the first; a click in the page moves to the second (macOS: WebKit becomes first
  responder **[추정: the research drove `makeFirstResponder` in place of a click]**; Linux: a
  focus-in, then forwarding); a click elsewhere in the window moves back **[추정]**.
- **Always sopen's**: Esc, F1–F12, Cmd+W, Cmd+Q (Ctrl+W, Ctrl+Q on Linux) — measured on macOS: without
  the monitor the page swallows them, Cmd+W and Cmd+Q included, past the close prompt; with it they
  reach egui (*web* §1.4) — and Cmd/Ctrl+1…0, 04 §6.1's F-key aliases **[decision; 추정: not in the
  measured set]**. macOS: the monitor hands them to winit's view (`makeFirstResponder`). Linux: never
  forwarded.
- **Esc while the page has the keyboard only takes it back** — the monitor consumes it **[decision;
  추정: the prototype passed it on]**; a second Esc leaves the viewer (or closes the address window,
  §7). The other always-sopen keys take the keyboard back and act.
- While the page has the keyboard every other key is its own: typing, Tab (form fields), Cmd/Ctrl+C/V/A
  (copy from the page; *web* §1.4's decisions). macOS IME: WKWebView takes input methods as first
  responder **[추정: Korean not measured]**; Linux: §2.3's gap.

### 2.5 Lifecycle, processes and bounds

- At most two live views per browser window — the preview's and the viewer's (the first becomes the
  second at Enter, §6) — plus, after WV-F, one hidden page view per opened file in 08's set. One per
  address window.
- **Processes** **[추정: the model is WebKit's, not measured here]**. macOS: each WKWebView renders in
  a WebContent XPC process; sopen gives every view its own configuration and process pool, so a view's
  WebContent process is its own; networking runs in WebKit's Networking process. Linux: WebKitGTK runs
  a web process (and a network process) per `WebKitWebContext`; every view has its own ephemeral
  context, so its processes are its own. **Contract**: after a view closes, after the address half's
  input reaches EOF (§7), and after sopen itself dies (SIGKILL included), no web or network process
  of that view survives and nothing more reaches the network — WV-L1, measured by WV-A and WV-B.
- **No stored data**: `WKWebsiteDataStore.nonPersistentDataStore`; on Linux the ephemeral context.
  The content rule lists (§3.3) are compiled into a **per-process temp folder** of sopen's own
  (`WKContentRuleListStore storeWithURL:`, `webkit_user_content_filter_store_new(path)`), never the
  default store under the home; it is removed when the process ends, and one left by a killed sopen
  is swept at the next start **[decision]**. WV-S5 checks that nothing else is written.

## 3. The page scheme and its security

### 3.1 What the scheme serves (`stream::page`)

- **URLs** `sopen-page://<view>/<path>`, `<view>` = 128 random bits per view, so no two views share an
  origin and each handler serves its own only **[decision]**. Registered per view —
  `setURLSchemeHandler:forURLScheme:` on its configuration, `webkit_web_context_register_uri_scheme`
  on its context. No loopback port: nothing outside the process can reach it (*web* §4). The contract
  between the engines and the server — request, range, response pieces, stop — is a trait WV-0 owns
  (`src/scheme.rs`), so WV-C and WV-0 are built side by side.
- **The root**: the folder the browser was opened on (`browser::Start.abs`, browser.rs:90-113) when the
  file's resolved path is inside it on the same host, else the file's own folder; resolved by
  `realpath` on that host. The page's URL is the file's path relative to the root.
- **A new request** — the steps run on the page's connection as a `RemoteFs`, never while it is a
  `Ranged` (a `Ranged` resolves and stats nothing but its own file):
  1. percent-decode, refuse a NUL; join under the root;
  2. the connection back from its last file (`into_connection`); `realpath`, so a link counts by what
     it points at; served only when the result is the root or below — else 403, counted on the row;
  3. `stat`: a regular file goes to step 4. A folder selects its `index.html`, and **that final path
     goes through step 2 and this step again** — `realpath`, root containment, regular file — before
     anything opens (an `index.html` that links out of the root is 403) **[decision]**. Anything
     else is 404;
  4. `into_ranged(resolved)` on the checked path: the handle's `fstat` is the request's
     fingerprint (06 §5).
  That is ~4 RTT (realpath, stat, open, fstat): 48–160 ms at the reference rig's 12–40 ms
  **[추정; X-W1 measures]**.
- **Served in turn** **[decision]**: open requests take turns, one piece (≤ 1 MiB, ≤ 4 MiB in flight)
  each, so a long response (a large image, an open-ended `bytes=0-`) never holds back the CSS behind
  it. Consecutive turns on one file (a `<video>`'s ranges) keep its handle. A later turn of a checked
  request on another file is `reopen(resolved)` — `into_connection().into_ranged(path)`
  (sftp/ranged.rs:392-393), ~2 RTT, no new checks — and the fingerprint is compared at every reopen:
  a file whose size or mtime moved fails that request (a network error to the engine), never padded.
- **Transport**: 06's `remote::Ranged`, each piece handed to the engine as it arrives
  (`didReceiveData:`, the request's input stream on Linux), wrapped in `stream::Throttled`
  (stream/mod.rs:263) under `SOPEN_STREAM_THROTTLE`, as every stream is. A local page goes the same
  way over `LocalFs` (`into_ranged`, local.rs:251), in place (06 §1). A short reply or an early end
  fails the request.
- **Range** `bytes=a-b` → 206 with `Content-Range` and `Accept-Ranges: bytes`; unsatisfiable → 416;
  else 200 with `Content-Length` (measured on both engines, `<video>` seeking on macOS, *web* §4).
- **Type** by extension, a fixed table (html/htm `text/html`, xml `application/xml`, xsl/xslt
  `text/xsl`, and css, js, json, svg, png, jpeg, gif, webp, woff/woff2, ttf/otf, mp4/webm, mp3/ogg/wav,
  pdf); anything else `application/octet-stream`. A text type gets `; charset=utf-8` only when its
  first 8 KiB is valid UTF-8 — a character cut at the 8 KiB edge accepted, as `kind::looks_like_text`
  does (kind.rs:157-165) — and names no charset itself (`<meta charset`, `http-equiv`, `encoding=`);
  else none, and the engine decides as a browser does **[decision; that the engine honours the header
  추정]**. The server marks every XML-family response (xml, svg, xsl) whose first 8 KiB holds
  `<?xml-stylesheet` as **styled** (§3.4 reads the mark).
- **Cancel**: the engine's stop (`webView:stopURLSchemeTask:`; the request's cancellable on Linux)
  ends the request after the piece in flight; nothing is sent to a stopped task (WebKit throws on
  that **[추정]**); closing the view stops all.
- **Channels** (06 §6) **[decision]**: a shown remote page view holds **one** SFTP channel, as a text
  viewer does — in the preview the window's preview session, given back when the bar moves on (06 §13
  item 3, `streamed::Lease`); in the viewer the connection that opened the file (prepare.rs:188-203),
  or the preview's, carried over. A page view hidden in 08's set (WV-F) gives its session back, as 08
  D14 does a save connection, and connects again at its next request. WV-D adds the row to 06 §6.
- **Preview budget** **[decision]**: a preview's page is served at most 32 MiB in all (02 §4.3's bound
  for an image read whole); later requests fail. **The hand-over lifts it**, as Enter lifts a
  preparation's budget (preview.rs:50); a view whose budget refused anything reloads once at the
  hand-over, so the viewer shows the whole page. The viewer has no byte budget.

### 3.2 Engine settings, every view

No JS↔native bridge (no `WKScriptMessageHandler`, no script message handler); no UI delegate
(`window.open` makes nothing, JS dialogs show nothing); devtools off (`inspectable = false`,
`enable-developer-extras = FALSE`); no file URL ever loaded (Linux: `allow-file-access-from-file-urls`
and `allow-universal-access-from-file-urls` off); no downloads (a top-level response the engine cannot
show is cancelled, with a row line); WebKitGTK's web-process sandbox on; WebRTC off by setting where
the engine has one (Linux `enable-webrtc = FALSE`); DNS prefetching off where it has one (Linux
`enable-dns-prefetching = FALSE` **[추정: it may be a no-op in 2.52]**).

### 3.3 Scripts and outside requests

- **Every file view — preview and Enter viewer, scripts on or off — loads nothing outside the
  scheme** (`sopen-page:`, plus the in-process `data:` and `blob:`). One content rule list does it,
  the same JSON on both engines (`WKContentRuleListStore`, `WebKitUserContentFilterStore`, *web*
  §4.1-10): block every URL, then let those three through. Why scripts off is not enough: XSLT runs
  with scripts off, and a stylesheet's `document()` can read files of the root and write them into an
  outside `<img>` URL — at the top level, inside an HTML `<iframe>`, or as an SVG carrying the
  stylesheet instruction — and in the preview without any click (*web* §5: both engines run XSLT). The
  view's row says `N outside requests blocked`; outside CSS, images and fonts therefore do not load,
  and a page that needs them looks unstyled. The lists were prototyped only as `^https?:` blocks
  **[미측정]**; WV-S2 goes through the channels.
- **Scripts are off by default**: `WKWebpagePreferences.allowsContentJavaScript = false`;
  `enable-javascript-markup = FALSE` (*web* §4.1-5).
- **F2** turns scripts on for this view and off again, reloading the document on screen. Its label
  names what it does (`JS on` / `JS off`); the top bar says `scripts off`, or `scripts ON` in the warn
  colour. A new view starts off; the preview is always off (F2 does nothing there).
- **Scripts-on channels fail closed.** A channel that is not a URL the rule list sees — WebRTC's STUN
  and TURN over UDP, DNS (a `<link rel=dns-prefetch>` or any name a script makes the engine resolve),
  anything else WV-S2 finds — is either turned off by a setting that WV-S2 shows works, or **file-view
  scripts are unavailable on that backend**: F2 there says `scripts are not available here: <channel>
  is not contained` and does nothing. **Until WV-S2 has passed on a backend, F2 is off on it.** How
  each is measured (WV-S2): WebRTC by a UDP listener on loopback named as the STUN and TURN server;
  DNS by a logging resolver as the engine's only resolver (Linux: a private network namespace whose
  resolver is that logger; macOS: a per-domain resolver for a test domain pointing at it), names under
  that domain; an HTTP server that saw nothing is never the evidence for DNS or UDP.

### 3.4 Links and navigation

- Top-level navigation within the view's own scheme host is followed in the view.
- A clicked link out (`WKNavigationTypeLinkActivated`, `WEBKIT_NAVIGATION_TYPE_LINK_CLICKED`): http/https
  → the target's system browser (`url::open`, url.rs:316) **only while scripts are off and neither the
  top document nor the frame the link is in is styled** (§3.1's mark). Otherwise it is refused with a
  line — with scripts on, `scripts are on: links out of the page are not followed — F2 turns them
  off`; in styled XML, `links out of a styled XML page are not followed`. A script can make a link and
  click it (`a.click()` **[추정: read as link-activated]**), and an XSLT can write what it read into
  a link's URL; either click would carry it out. Any other scheme is refused with a line.
- Every other navigation out (a redirect, a meta refresh, `location=`, a form) is cancelled; the row
  says where it went, shown as `url::redacted` shows an address.
- No back or forward (non-scope): a link followed inside the root is left with Esc; Enter opens the
  file again.

### 3.5 Threat model: the page is an untrusted file

| The page reaches | Scripts off | Scripts on |
|---|---|---|
| `sopen-page:` under the root, `data:`, `blob:` | shown (`<img>`, `<iframe>`, links, XSLT `document()`) | shown, and readable by script |
| files outside the root, `file:` | no (`realpath` checks, the index's included; no file URLs) | no |
| anything outside the scheme, by any element, stylesheet or XSLT | no (blocked; counted on the row) | no |
| DNS, WebRTC, other non-URL channels | none known (no scripts) | contained per §3.3, else scripts unavailable |
| navigation out | the owner's click only, to the system browser, never from styled XML | none |
| native code, devtools, storage, downloads | none | none |

Nothing outside the root is ever served, and nothing outside the scheme is ever loaded by a file view,
so what a page can read it can only show. The residual is the owner's own click on a link out of an
unstyled page with scripts off: its URL is what the file's author wrote, carrying nothing the page
read.

## 4. Fallbacks

- macOS always has WebKit. Linux without `libwebkit2gtk-4.1.so.0`, or with one that fails to load or
  start (a missing symbol, `gtk_init_check` false): sopen starts and every other window works.
  - A page view → the file's highlighted source (the text viewer, as before 09), first row `no web
    engine here (<reason>): showing the source`.
  - An address → the system browser (`url::open`), as `--browser`, after one stderr line `sopen: no
    web engine here (<reason>); opening <shown> in the system browser`.
- No display (Linux, `display_available`, gui/mod.rs:1206): an address goes to the system browser as
  today. The reason is found once per process; the hook `SOPEN_WEB=off` (WV-0) makes it "turned off
  (SOPEN_WEB=off)" on both systems, for fallback tests and receipts **[decision]**.

## 5. Which files are pages

- `.html`, `.htm`: the page is the default view.
- `.xml`: the page is the default when its first 8 KiB holds `<?xml-stylesheet` before the first
  element; plain XML and RSS default to the highlighted source — both engines show them as run-on text
  (*web* §5) — with the page as view 3 (§6).
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
  nothing outside the scheme (§3.3); nothing reaches the page (macOS: the shield, §2.2; Linux: nothing
  forwarded); §3.1's budget. Every exit of 02 §4.4 holds: the bar leaving closes the view, stops its
  requests, gives the session back; Enter or F3 hands the view over in place — the shield goes, the
  budget lifts (a view the budget cut reloads once, §3.1), §2.4 applies, the scroll stays.
- **The Enter viewer** opens a page file in the page view, filling the viewer's area. Top bar: the
  document on screen (`host:/abs`), `scripts off|ON`, loading, the counts of refused and of outside
  requests blocked. F-key row `1Help 2JS on 3Exit 4Edit` (F4 = 01 A2's editor on the file, as now;
  keys.rs:499-503). Cmd/Ctrl+R reloads; Esc leaves (§2.4).
- **Until WV-F, the gap**: in xtools a page file shows only as the page; its source and hex are
  reached through F4 (the editor), not inside sopen. The view keys are 08's (§4, D3–D4) and reach the
  GUI with its binding VM-7; binding digits here first would pre-empt them.
- **The views, settled here** (they supersede 08's D7 for these files; WV-F amends 08 §3, D7, D9,
  D30):

| Files | 1 | 2 | 3 |
|---|---|---|---|
| `.html`, `.htm`, styled XML | Page | Hex | the source, highlighted |
| plain XML, RSS | the source, highlighted | Hex | Page |

  A hidden page view shows nothing, takes no input and plays nothing (media paused **[추정 API]**).
- **Under message 23** (*XR-T* §0): the page view is a pixel region of the GUI backend at its cell
  rectangle (10); in the TUI the same view shows the file's source (§8). WV-D needs nothing from 08
  or 10 to ship (§2.1); WV-F binds it per 08 and 10.

## 7. The address window, and #82's contract

**Today (#82).** `open_url` (main.rs:597-611) opens the system browser (`url::open`, url.rs:316) and
prints `opened … in the browser`; as HOST's half (`--hold-until-eof`, main.rs:69-72) it checks the
port is private, opens the browser, prints `sopen-tunnel-open` (url.rs:288) and holds until its input
ends. The remote side prints `… is open on HOST through a tunnel — Ctrl-C closes it`
(forward.rs:649-668). When HOST's command ends first, the remote exits with HOST's code if ssh exits
within 2 s, else 1 with `… ended while ssh stayed for the browser's connections` (forward.rs:689-718,
01 §4a).

**From 09, on each route to an address:**
1. **Here** (no HOST, or HOST is this machine; main.rs:581): an xtools window with the address view;
   `opened <shown> in xtools` once it is up; sopen returns when it is closed (exit 0). A change: today
   the command returns at once.
2. **A tunnel** (loopback with HOST; main.rs:584-586). HOST's half checks the port as today and opens
   the window; then it prints the fixed cue `sopen-tunnel-window` (`url::TUNNEL_WINDOW`), then
   `sopen-tunnel-open` once the load has started. A thread reads its input; at EOF the window closes
   and HOST exits 0. **Closing the window** prints `sopen-tunnel-closed` (`url::TUNNEL_CLOSED`),
   tears the view down (its processes end, §2.5) and exits 0. Cues carry nothing of the address and
   are written ignoring write errors — never `println!`, which panics on a closed stdout (today's
   cue, main.rs:606) **[decision]**.
   - The remote side prints `<shown> is open on HOST through a tunnel — closing its window there, or
     Ctrl-C here, closes it` **only after it saw `sopen-tunnel-window`**; else today's `… — Ctrl-C
     closes it` (a HOST on §4's fallback, or an older HOST). The fallback-port forms alike.
   - **The closed cue counts whether it comes before or after ssh exits**: on ssh's exit the remote
     reads it only after `ended()` has drained the stdout relay (forward.rs:689-700), not before. On
     it the remote closes the tunnel at once (its input closed, the guard ends ssh: forward.rs:534-544),
     prints `<shown>: its window on HOST was closed; the tunnel is closed` and exits 0. Any other end
     of HOST's sopen keeps today's rule above.
3. **Forwarded as written** (not loopback; main.rs:589): HOST's sopen opens the window as in 1; the
   remote's run lasts until it closes, as any window opened with `--target`.

**The address window's screen.**
- Title and top bar: `xtools — <shown>`; the bar adds loading, `scripts ON|off`, and any line below.
  **Every URL sopen shows** — title, bar, lines, stderr — goes through `url::redacted`. F-key row
  `1Help 2JS off 3Exit` (F3 closes the window, as a window of its own does: `chrome::EXIT_KEY`).
- Esc: the first takes the keyboard back from the page (§2.4), the second closes the window — which,
  as HOST's half, ends the tunnel (route 2). Cmd/Ctrl+R reloads.
- **Navigation**: top-level navigation within the address's origin (scheme, host, the port it got); a
  clicked link elsewhere → HOST's system browser (a script's `a.click()` may count as a click here
  **[추정]** — the page is the owner's own server); any other navigation out refused with a line;
  subframes and subresources load as in a browser; **scripts on by default**, F2 toggles; no
  `sopen-page` scheme (nothing on disk reachable); §3.2's settings. §3.3's file-view rules do not
  apply here.
- **Failures**, each a line in the bar: the main page fails to load (the dev server is not up yet) →
  `cannot load <shown>: <reason> — Cmd/Ctrl+R tries again`; a TLS certificate the system does not
  trust → never accepted, `the certificate of <host> is not trusted — --browser opens it in the system
  browser`; HTTP auth or a client certificate → refused, naming `--browser`; the web process ends →
  `the page's process ended — Cmd/Ctrl+R reloads`; ATS refusing plain http (§2.2) → naming
  `--browser`.
- **`--browser`** (new, public): today's behaviour exactly, every route. With a file SOURCE a usage
  error (exit 2), as `--hold-until-eof` (main.rs:520-524). Passed to HOST on both forwarded routes
  (`tunnel_words`, forward.rs:361-364, and the run's words); an older HOST sopen does not know it and
  exits 2 with its own line.
- **Version skew**: cues are matched exactly (forward.rs:634). An older remote prints
  `sopen-tunnel-window` and `sopen-tunnel-closed` as plain lines and ends by today's rules; an older
  HOST sends neither, so the remote prints today's line. Both ends are meant to be one sopen
  (`SOPEN_TARGET_BIN`, the tap).
- **Tests** **[decision]**: tests/cli.rs's `sopen URL` tests use a `SOPEN_BROWSER` stand-in
  (cli.rs:3255-3262) and pass `--browser` from WV-E1, keeping every assertion; AGENTS.md's
  `SOPEN_BROWSER` row then serves `--browser`, the fallbacks and outside links.

## 8. The TUI

Message 23 makes xtools one cell-grid core with a terminal and a GUI backend (*XR-T* §0; 10). A page,
like an image or a video, is GUI-only content: the GUI backend hosts the page view as a pixel region
at its cell rectangle, and in a terminal a page file shows its source (the source is view 1 there).
Whether a picture-capable terminal (kitty, iTerm2, sixel) shows a snapshot of the page, and the
terminal form of every key here, are 10's. `sopen URL` with only a terminal: §4's no-display rule.

## 9. Decisions

1–9 are the orchestrator's first round (2026-11-24), with 4 and 5 as the review round revised them;
R1–R9 are the review round's (the coordinator, 2026-11-24); 10–14 are this document's choices that
change a decision or a shipped contract (the smaller ones are marked **[decision]** where they are
made). All reversible.

| # | Decision | Why |
|---|---|---|
| 1 | One component, two engines: macOS WKWebView subview following the egui rect, hidden under egui layers, key monitor; Linux WebKitGTK 4.1 by dlopen, offscreen into an egui texture, acceleration off, HiDPI by size × scale + zoom, one path for X11 and Wayland | the only routes measured to work inside the window; Wayland has no child views, wry takes only Xlib handles (*web* §1.2) |
| 2 | Without WebKitGTK sopen still starts: a page falls back to its source with a line, an address to the system browser | an optional library must not break sopen (ci.yml:146-164) |
| 3 | Pages load through an in-process scheme fed by the stream layer, never a port; root = the opened folder when the file is in it, else its folder; nothing outside; no `file:` | no local process can reach it; generated docs reference parents (*web* §4, §4.1-1) |
| 4 | Scripts off by default, F2 per view; **every file view, scripts on or off, loads nothing outside the scheme** (revised: the first round allowed outside http(s) subresources with scripts off); no bridge, devtools, storage | XSLT runs without scripts and its `document()` can put root files into an outside URL — top level, in an iframe, in an SVG, in the preview without a click |
| 5 | A clicked link out: http(s) → system browser only while scripts are off and the page (top document and the link's frame) is not styled XML; otherwise refused (revised) | a script or an XSLT can write what it read into a link's URL |
| 6 | Styled `.xml` defaults to the page, plain XML/RSS to the source (page as view 3); `.html`/`.htm` to the page | engines show plain XML as run-on text (*web* §5) |
| 7 | The preview shows the page, scripts off, after the rest, by 02 §4; views and scripts are chosen in the Enter viewer | message 20: modes in the viewer, not the preview |
| 8 | `sopen URL` → an xtools window on the target, limited to its origin; closing it ends the target's half and the tunnel; `--browser` keeps the old route | message 18 item 1 |
| 9 | TUI: a page is GUI-only; the source shows; the binding is 10's | message 23 (ssot item 2) |
| R1 | Scripts-on channels fail closed: off by a setting shown to work, or file-view scripts unavailable on that backend; F2 off until WV-S2 passes there | an HTTP server seeing nothing is no evidence for DNS or UDP |
| R2 | The index file gets the same resolution, containment and regular-file checks | an `index.html` that links out of the root would otherwise be served |
| R3 | A view's web and network processes end with it, with the address half's EOF and with sopen's death | a page left running keeps fetching after the owner closed it |
| R4 | Views: HTML/HTM and styled XML 1 Page · 2 Hex · 3 source; plain XML/RSS 1 source · 2 Hex · 3 Page (supersedes 08 D7 here) | decision 6's "the page as another view"; one table, no "unsettled" |
| R5 | `sopen-tunnel-window` before `sopen-tunnel-open`; the window line only after it | the remote cannot otherwise know whether HOST fell back to the system browser or is older |
| R6 | The closed cue counts before or after ssh's exit, read after `ended()`; cues written ignoring errors | a fast ssh exit would race the relay; `println!` panics on a closed stdout |
| R7 | The page server resolves and checks on the `RemoteFs`, then `into_ranged`; later turns `reopen`; fingerprint compared at every reopen; `Throttled` under the hook | a `Ranged` resolves and stats nothing but its own file (sftp/ranged.rs:392-393) |
| R8 | The hand-over lifts the preview budget; a cut view reloads once | as Enter lifts a preparation's budget (preview.rs:50); the viewer shows the whole page |
| R9 | The address window: Esc twice closes it; redacted URLs everywhere; a line for each failure; never accept an untrusted certificate | a dev server not up yet, a bad certificate and a crash are its common states; addresses carry secrets (url.rs:14-15) |
| 10 | Scripts **on** by default in the address window | decision 4's reason (the root) does not apply: no root, the owner's own server; a dev server's page is usually a script app **[추정]** |
| 11 | Requests served in turns of one piece each | a long response must not hold back the CSS behind it |
| 12 | Esc with the page holding the keyboard only takes it back; Cmd/Ctrl+1…0 reserved too | one press, one action (08 §4); on macOS 04 §6.1's aliases would be lost |
| 13 | `sopen-tunnel-closed` and the remote's exit 0; local `sopen URL` returns when its window closes | a window closed is the normal end (decision 8); today that path is HOST's code or an error |
| 14 | The standalone `sopen x.html` is unchanged | ssot item 5 names xtools; the editor spec owns the standalone route |

## 10. Acceptance

"Do X → observe Y". **[owner]** = needs the owner's hands. On macOS posted clicks do not reach the
page (*web* §1.1), so automated runs drive `makeFirstResponder` and `hitTest`; real clicks are [owner].

- **WV-1** Resize a viewer showing a page → its frame equals the egui rect in points, `setFrame` once
  per change (fake engine). **WV-2** The close prompt and F1 help over a page → macOS: `hitTest` at the
  dialog's centre is not the WKWebView, hidden for those frames, back after Esc; a frame that does not
  place the view (its viewer covered) → hidden; Linux: drawn over it.
- **WV-3** Page holding the keyboard (macOS: `makeFirstResponder`, the state read back from the first
  responder; Linux: a forwarded click), keyvals `abc` → in the `<input>`; Esc → keyboard back, viewer
  open; Esc → it closes. With the page holding the keyboard: F3 → leaves; Cmd/Ctrl+1 → help; Cmd+W →
  the close path; Cmd+Q → the quit path. macOS with the monitor off → none reach sopen.
- **WV-4 [owner, macOS, real clicks]** A click into a field, then typing → typed there; a link inside
  the root → followed; an outside https link in an HTML page with scripts off → the system browser;
  the Korean input source in a field → composed Hangul.
- **WV-5 [owner, the Linux laptop, a real scaled output at 2×, 1.25×, 1.5×]** → crisp text at device
  resolution, 1×'s layout, a checkbox click toggles. Korean into a page field: record the result.
- **WV-6** Linux, a headless private compositor (*web* §9's isolation) → a screenshot holds the page's
  pixels; copy ≤ 1.5 ms per damaged frame at 2×; teardown ≤ 2 ms; the release binary's `ldd` unchanged.
- **WV-7** The Linux loader with a missing soname → its reason in < 1 ms. `SOPEN_WEB=off`: Enter on
  `index.html` → the text viewer with the line; `sopen --here http://127.0.0.1:P/` → the stand-in once,
  the stderr line, exit 0. The clean container still passes `sopen --version`.
- **WV-8** Preview → the page ≥ 150 ms after the bar rests, never sooner (fake clock); a bar moving
  within 150 ms makes no view; moving later closes it, stops its requests, and the window keeps one
  preview session; F2 does nothing; Enter on a page within the budget → the same view, no second load
  (requests counted); on a page the budget cut → one reload, then every file served. macOS: `hitTest`
  at the preview's centre is the shield, after the hand-over the page; the preview's view is never
  first responder. **[owner, real clicks]** a click and a wheel on the preview change nothing (scroll
  offset, a click counter); the same after Enter act (the control).
- **WV-L1** (WV-A, WV-B) Note the web and network processes a view starts (the process list before
  and after it opens). Close the view; end the address half's input (EOF); `kill -9` sopen with a
  view open → each time, within 2 s, none of those processes is left (process list), and a page that
  polls a test server (scripts on, `setInterval` fetch to its own origin) sends nothing more (the
  server's log).
- **WV-S1** Fixtures with `../`, `%2e%2e`, a link inside the root pointing out, `sub/index.html` a link
  pointing out of the root (requested as `sub/`), an `/etc/hostname` iframe, a `file:` iframe → none
  served (server log: 403 or never asked); locally and through `local_server()`.
- **WV-S2** Outside channels, against a test HTTP server on 127.0.0.1:P, a UDP listener and a logging
  resolver (§3.3):
  - scripts off: `<img>`, CSS `url()`, `@font-face`, `<link rel=stylesheet|prefetch|preconnect|
    dns-prefetch>` to the server and to names under the test domain; styled XML whose XSLT reads a
    root file with `document()` and writes it into an outside `<img>` URL — at the top level, inside an
    HTML `<iframe>`, and as an SVG carrying the stylesheet instruction in an iframe; the same in the
    preview → **zero** HTTP requests, zero resolver queries for the test domain; the row counts the
    blocked ones; a link out of the styled page clicked (owner, or `hitTest` + a synthesized click
    where it reaches) → refused, the `SOPEN_BROWSER` stand-in never called;
  - scripts on: fetch, XHR, WebSocket, EventSource, `sendBeacon`, a form POST, `location=`,
    `window.open`, `a.click()` → zero HTTP requests, the stand-in never called; `RTCPeerConnection`
    with the listener as STUN and TURN server → zero datagrams; names built by script under the test
    domain → zero queries. A backend that fails any of these has F2 off (R1).
- **WV-S3** `bytes=2809-2812` on a 4 MiB file → 206, 4 bytes, its `Content-Range`; past the end → 416;
  40 ranges on one file → one handle (the Fake's counters); a CSS request behind an open `bytes=0-` on
  a 200 MiB file → answered before that response ends; a new request → realpath, stat, open and fstat
  in that order, a later turn on it → one reopen (request log). The 200 MiB file under
  `SOPEN_STREAM_THROTTLE`, its view closed at 10 % → reads stop within ≤ 1 MiB more, no call reaches a
  stopped task. A file rewritten between two turns → that request fails at the reopen; Cmd/Ctrl+R
  shows the new file.
- **WV-S4** Korean UTF-8 HTML with no meta charset (a Hangul syllable cut at byte 8 192), and EUC-KR
  HTML with `<meta charset=euc-kr>` → Hangul renders in both.
- **WV-S5** Close a page that set a cookie and localStorage (scripts on) → no website data of sopen on
  disk (WV-A and WV-B name the folders checked) and the rule-list temp folder gone; no Inspect item.
- **WV-S6** A remote page in the viewer → one SFTP session (the master's count, as 06 §6 verified); in
  the preview → none beyond the window's preview session.
- **WV-P1** Enter: `.html`, `.htm` → page (local, remote); `feed.xml` → source; `styled.xml` → page,
  its `.xsl` through the scheme; an instruction past 8 KiB → source.
- **WV-P2** A rustdoc-shaped remote tree opened from its top → styled pages; the same file from a
  browser opened inside its subfolder → root = that folder, the upward CSS 403 and counted on the row.
- **WV-A1** `sopen --here http://127.0.0.1:P/` → the window shows the page (screenshot), `opened … in
  xtools`; Esc, Esc → it closes, exit 0. Nothing listening on P → the cannot-load line; Cmd/Ctrl+R after
  the server starts → the page. A self-signed `https` server → the certificate line, nothing loaded.
  A URL with a password and a query → no shown line or title carries either.
- **WV-A2** A stand-in ssh whose HOST half prints the window cue, the open cue, then the closed cue:
  while ssh stays → the remote exits 0 within 1 s with the closed line; with ssh exiting at once after
  the cue → the same. Without the window cue → today's open line. HOST killed instead → today's rule.
  HOST's stdout closed early → HOST does not panic, exits 0.
- **WV-A3** `--browser` on all three routes → today's output byte for byte; with a file → exit 2. In
  the view: a clicked link to another origin → the stand-in records it; a redirect to another origin →
  refused with a line; a script writes into the page (scripts on by default).
- **WV-A4 [owner]** From the Mac, `sopen --target <the Linux laptop> http://127.0.0.1:<dev server>/` →
  the page in xtools there; closing it → the Mac's sopen prints the line, exit 0; Ctrl-C on the Mac →
  the window there closes. ATS: WV-A's three `http://` loads → recorded, and the spec's line holds.

## 11. Test plan

- **Unit level, no engine**: `stream::page` (WV-C) on the `Fake` `Ranged`, `LocalFs` and
  `local_server()` (off the network, AGENTS.md); `gui::web` (WV-0) and the browser (WV-D) on the
  testkit with a recording `FakeEngine`, 02 §4.5's rows gaining a page; the CLI (WV-E1) with the
  stand-in browser and the fake ssh.
- **Engines**: window tests run only with `SOPEN_REQUIRE_WEBKIT=1` (as the other `SOPEN_REQUIRE_*`).
  macOS CI sets it (scheme fixtures with `takeSnapshot` pixels at known places, the monitor driven by
  synthesized NSEvents with its no-monitor control as *web* §1.4, `hitTest` under a Modal and the
  shield, WV-L1, WV-S2). Linux CI **[decision]** installs WebKitGTK's runtime package for these tests
  only and runs them under `xvfb-run` **[추정: the research ran a private kwin on Wayland; X11 takes
  the same path]**, WV-S2's DNS part in a private network namespace; a developer uses *web* §9's
  private kwin (`env -i`, private runtime folder and bus, `--virtual`, never the live session). The
  clean container (ci.yml:146-164) adds only WV-7's check.
- **Receipts**: `SOPEN_SCREENSHOT` of a page in the preview and the viewer, macOS (composed) and Linux,
  remote ones under `/tmp/sopen-receipts/` (AGENTS.md); a page is ready to shoot once its main frame
  finished or failed and no request is in flight.

## 12. Work units

**X-W1 first** (before WV-C and WV-0 merge, and before A's and B's scheme glue): the stream-fed
handler is the load-bearing part the research left unbuilt (*web* §4). A prototype outside the repo
shows, on both engines, a handler answering from a worker thread over a throttled Fake; late pieces
accepted on the engine's thread (macOS main, GTK); a stop mid-response leaving no call on the task;
turns of one piece keeping a later CSS request ahead of a long response; a run of ranges on one
handle; §3.1's ~4 and ~2 RTT. Its result goes into §3.1.

| ID | Unit | Owns | After |
|---|---|---|---|
| WV-C | **Page server**: root, request steps (index included), turns, `reopen`, fingerprint, Range, type and charset tables, the styled mark, cancel, budget and its lift | `src/stream/page.rs`, `src/stream/page/tests.rs` (new); `src/stream/mod.rs` (the `mod` line) | X-W1; WV-0's `src/scheme.rs` (the trait) |
| WV-0 | **Component core**: `Web`, `Engine`, the scheme trait, placement, the after-frame hide check, key states, scripts and F2's availability, `SOPEN_WEB`, `FakeEngine`, the compose call; the shell's part (§2.1) | `src/scheme.rs`, `src/gui/web/mod.rs`, `src/gui/web/fake.rs`, `src/gui/web/tests.rs` (new); `src/lib.rs` (the `mod` line); `src/gui/mod.rs` (`mod web`, `Shell`'s handle, after-frame check, teardown, monitor install, the compose call); AGENTS.md's `SOPEN_WEB` row | X-W1 (the trait first, so WV-C builds beside it) |
| WV-A | **macOS engine**: WKWebView, process pool per view, monitor, shield, `takeSnapshot`, scheme glue, rule list in the temp store, ATS measurement | `src/gui/web/mac.rs`, `src/gui/web/mac/tests.rs` (new); `Cargo.toml`, `Cargo.lock` (objc2-web-kit; objc2-app-kit features); `.github/workflows/ci.yml` (one macOS-only line: `SOPEN_REQUIRE_WEBKIT=1`); AGENTS.md's `SOPEN_REQUIRE_WEBKIT` row | WV-0, X-W1 |
| WV-B | **Linux engine**: loader, GTK thread, context per view, offscreen copy, input, HiDPI, scheme glue, rules, settings | `src/gui/web/gtk.rs`, `src/gui/web/gtk/api.rs`, `src/gui/web/gtk/tests.rs` (new); `.github/workflows/ci.yml` (the Linux steps) | WV-0, X-W1 |
| WV-D | **Page view in the browser**: `page_default`, `Prepared::Page`, preview, viewer, root, F2, the rows, fallback | `src/kind.rs`; `src/gui/web/page.rs` (new); `src/gui/web/mod.rs` (`mod` line); `src/gui/browser/prepare.rs`; `src/gui/browser/preview.rs`; `src/gui/browser.rs`; `src/gui/browser/keys.rs`; `src/gui/chrome.rs` (`PAGE_KEYS`); `src/gui/browser/tests/page.rs` (new); `.prd/02-architecture.md` §4.2–§4.3; `.prd/06-streaming.md` §6 | WV-C, WV-0; each platform's receipts after WV-A / WV-B |
| WV-E1 | **The CLI half**: `--browser`, the three cues and their writes, the remote's lines, the closed-cue race, version skew — all on the fake ssh; HOST still opens the system browser | `src/main.rs`, `src/url.rs`, `src/forward.rs`, `tests/cli.rs`; AGENTS.md's `--browser` and `--hold-until-eof` rows | — (starts at once) |
| WV-E2 | **The address view**: the window, its screen, navigation, failures, scripts on, HOST's half opening it | `src/gui/web/address.rs` (new); `src/gui/web/mod.rs` (`mod` line); `src/main.rs` (`open_url`'s window route); `.prd/01-spec.md` (A10a, §4a); `README.md`; AGENTS.md's `SOPEN_BROWSER` row | WV-E1; WV-0, WV-A, WV-B |
| WV-F | **Page in 08's view set** (= 08's VM-9): §6's table, hidden page views paused and their session given back, the pixel region per 10 | `src/gui/web/page.rs`; with 08 VM-7's binding files (`src/gui/browser.rs`, `src/gui/browser/keys.rs`); 08 §3, D7, D9, D30 | WV-D; 08's VM-5, VM-6, VM-7; 10 |

**Order**: X-W1 → WV-0 (trait first) ∥ WV-C → WV-A ∥ WV-B → WV-D ∥ WV-E2 → WV-F; WV-E1 from the
start. **Only WV-F waits on 08's model** (VM-5 the view set, VM-6 the keys, VM-7 the binding) and on
10: WV-D ships before them with the page as the default view and Esc back (§6 names the gap), and
WV-F plugs it into the set and the GUI backend's pixel regions. **Shared files are sequenced**:
`src/gui/web/mod.rs` gets one `mod` line each from WV-D and WV-E2; `src/lib.rs` one from WV-0 beside
08's VM-1/5/6 lines; `src/gui/mod.rs` (WV-0) is shared with 08's VM-2 and VM-7 and the browser
workstream's input hook; `browser.rs`, `prepare.rs`, `keys.rs` with 08's VM-7 and the F4 editor, one
at a time; `main.rs` WV-E1 → WV-E2; `ci.yml` by WV-A (the macOS line) and WV-B (the Linux steps),
different sections, the second to merge rebases; `AGENTS.md` by WV-0, WV-A, WV-E1, WV-E2, one row
each; only WV-A touches `Cargo.toml`/`Cargo.lock`. A conflict goes to the dispatcher, never settled by
dropping the other side (06 §13). A unit that shows something on screen brings a screenshot someone
looked at (06 §13).

## Tensions

- **08 §3, D7, D9, D30 vs R4.** 08's HTML, HTM, XML row makes every XML a page once 09 lands, gives
  Raw as view 3 where 1 is text (D7), and shows HTML as 1 Text, 3 Raw before 09 (D30). §6's table
  settles these files (plain XML's 3 is the Page, so it has no Raw); WV-F amends 08 to match.
- **08 D30 vs WV-D before WV-F.** WV-D makes the page view 1 before 08's keys reach the GUI, so the
  in-sopen source waits for WV-F (§6's gap).
- ***web* §8-6 and decision 4.** The research defaulted to blocking outside subresources; the first
  round allowed them with scripts off; the review round (decision 4 revised) blocks them in every file
  view, for XSLT's sake. Outside CSS and fonts therefore never style a file page.
- ***web* §1.4 vs decision 12.** The prototype's monitor passed Esc on after taking the keyboard back;
  09 consumes it — one Esc returns the keyboard, a second leaves.
- **01 A10a vs §7 route 1.** A10a: `sopen URL` opens the default browser and exits 0; from 09 it lasts
  until its window closes. WV-E2 rewrites A10a.
