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
([`research/tui-gui.md`](research/tui-gui.md), cited *XR-T* §n). Code citations are at e7df2f0, except
`main.rs`, cited at the branch base 05752c4 (one line later there). **[추정]** = inferred here, not
measured; **[미측정]** = the research names it as not measured; **[decision]** = this document's
reversible choice.

Amends, when it lands: 01 A10a and §4a, the CLI help (main.rs:40, :43-45), README's `sopen URL`
paragraph, AGENTS.md's `SOPEN_BROWSER` and `--hold-until-eof` rows plus new rows for `--browser`
(WV-E1), `SOPEN_WEB` (WV-0) and `SOPEN_REQUIRE_WEBKIT` (WV-A); 02 §4.2–§4.3 and 06 §6's channel table
(WV-D); 08 D30 once the page view lands (WV-F) — 08 already carries §6's table as its D40.

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
- **ATS**: App Transport Security is not enforced for sopen's binary, an unbundled executable with no
  Info.plist — measured by WV-A on macOS 26.7.1 (`wv_a_ats_and_plain_http` in
  `src/gui/web/mac/tests.rs`, an executable of the same kind): plain `http` to `127.0.0.1`
  loaded in 101–150 ms, to `localhost` in 57–63 ms, to `sopen.localhost` (a dotted name the system
  resolves to loopback) in 57–64 ms, and `http://sopen-ats.invalid/` failed at the lookup
  (`NSURLErrorDomain -1003`), not at ATS (`-1022`, which comes before any lookup); a one-off probe
  outside the tests loaded `http://example.com/` in 485 ms. So no load is refused for being plain
  `http`. sopen never adds an ATS exception; were it ever shipped in a bundle with an Info.plist, ATS
  would apply and this is measured again **[decision]**.

### 2.3 Linux: WebKitGTK offscreen, loaded at runtime

- **Loading**: `dlopen("libwebkit2gtk-4.1.so.0")` at the first view; the ~120 GTK, GDK, GLib, GIO,
  cairo, libsoup and WebKit entry points resolve by `dlsym` on that one handle (*web* §2;
  `src/gui/web/gtk/api.rs`). Nothing is linked, as for sopen's other runtime libraries
  (fnkeys/macos.rs, remote/local/trash.rs): `ldd` does not change (WV-B: the release binary's `ldd`
  names libstdc++, libm, libasound, libgcc_s, libc only; CI checks it). A missing library fails in
  0.1–0.2 ms → §4 (the windows' thread does nothing more before the first view). The sandbox is
  checked by the GTK thread's one-time start, before GTK or WebKit, never on the windows' thread,
  and a failure is the start's (the queued views told, every later one §4): WebKit's
  `WEBKIT_DISABLE_SANDBOX_THIS_IS_DANGEROUS` set to anything but `0` (WebKit then runs its web
  processes unsandboxed whatever the context's setting reads), a machine without the tools its
  sandbox runs each web process under (`/usr/bin/bwrap`, `/usr/bin/xdg-dbus-proxy`, WebKit's
  compiled-in paths), or one where `bwrap` cannot make the sandbox — `bwrap --unshare-all` runs
  `/bin/sh -c :` (5.5–6.3 ms in the release receipts), given up after 2 s. WV-B measured WebKit
  itself aborting the whole process at its first web process when unprivileged user namespaces
  were denied (`g_error` "Failed to fully launch dbus-proxy", SIGABRT); with the start's check the
  same run gets `no web engine here (WebKitGTK's web process sandbox cannot start here (…))`
  instead **[decision]**.
- **Thread**: one GTK thread per process, **spawned at most once**, by the first view; the windows'
  thread never waits for it — views opened while it starts are queued and told (`Failed`, §4's line)
  should it not start: `gtk_init_check` false, no seat, GLib's default main context another
  thread's, or nothing within 20 s; every later view then gets that reason synchronously (§4's
  fallback). Those checks come before WebKit's first use, which binds WebKit to the thread; a start
  the watchdog gave up on that later passes them has bound WebKit to a thread that then returns —
  `Failed` is final, nothing is ever sent to it and no view is made in the process. There it
  blocks in `g_main_context_iteration`, the default context acquired for good (so no
  `g_main_context_invoke` from another thread runs inline), woken by `g_main_context_wakeup`. GTK is
  not let set the process's locale (`gtk_disable_setlocale` first). **It is never joined**: WebKit
  binds its main thread to the first thread that uses it, and a view made on a second GTK thread
  after the first ended aborted the process (WV-B: SIGABRT inside
  `webkit_web_context_new_ephemeral`, gdb backtrace); so when the last view closes the thread stays,
  parked in that wait, for the process. Measured working beside winit's Wayland connection (*web*
  §1.4), though GTK off the main thread is not GTK's documented model — a risk.
- **Drawing**: a never-mapped `GtkOffscreenWindow`; on `damage-event` its surface is mapped
  (`cairo_surface_map_to_image`, BGRA premultiplied) into an egui texture painted at the rect, clipped,
  one device pixel a pixel — at most one copy per pass of the GTK loop. WV-B, release build, private
  compositor, 1×: copy 0.39–0.61 ms at 720×880, 1.07–1.48 ms at 1480×1200 (*web* §1.3 had
  0.34–1.3 ms; `TextureHandle::set` itself under 0.01 ms, the GPU upload is the renderer's; a 2×
  release timing was not taken). A view says its load
  finished once the picture after the load's end is copied (or 150 ms later), so a screenshot taken
  then holds the page. Acceleration `NEVER` (policy 2; the default aborts offscreen), so GPU-heavy
  pages run at software speed. One path for X11 and Wayland: nothing is embedded (Wayland has no
  cross-client child; wry takes only Xlib, *web* §1.2).
- **HiDPI**: an offscreen window ignores `GDK_SCALE` (measured). The view is rect × scale in device
  pixels with `webkit_web_view_set_zoom_level(scale)`; scale = winit's `pixels_per_point` (divided by
  GTK's own factor should it ever exceed 1); pointer positions are scaled alike, over the same factor;
  a side never exceeds the renderer's largest texture (egui's `max_texture_side`). Measured at 2× on a
  headless compositor only (WV-B: a 200×120-point view is a 400×240 picture, its layout 1×'s); a
  fractional scale is rendered at exactly rect × scale with that zoom, never at the ceiling and
  resampled **[decision; not seen on a real scaled output]**.
- **Input**: pointer moves, buttons, smooth scroll and keys as synthesized GdkEvents (§2.4: a key that
  types text goes by its text event, once; the others by keyval) — measured (WV-B): a click toggled a
  checkbox, "abc" typed into an `<input>`, Hangul typed there both as egui's text and as an input
  method's committed text (GDK's Unicode keyvals), a wheel scrolled, Tab and Enter reached a link. A
  key sent in the same instant as a click can reach the page before it: WebKit sends a view's mouse
  events one at a time, each after the last was answered, and its keys at once (measured with a
  synthesized move-click-type; a hand is slower). **Korean input**: while the page has the keyboard
  the window asks for its input method (`Web::ui`, as a text field does: winit allows an IME only
  then), so the owner's input method composes and its committed text is typed into the page; **the
  composition (preedit) is shown nowhere** — WebKitGTK gets only committed text, the view has no
  input method context of its own (none of an IME daemon's windows can open from the offscreen
  page), so a syllable appears when the input method commits it. Committed text measured with
  synthesized egui events only; the owner's real input method is WV-5's. Not covered
  **[미측정]**: the cursor over links, drag and drop, selection and copy to the system clipboard,
  accessibility.
- **Nothing maps a window**: the context menu, a `<select>`'s popup, the file and colour choosers,
  script dialogs, printing, HTTP authentication, tooltips and full screen are refused, each a
  window of GTK's own outside the texture **[decision]** — so a `<select>` does not open by mouse.
- **Sandbox and network**: each view's ephemeral context runs its web process in WebKit's bubblewrap
  sandbox (off by default in the 4.1 API: turned on before any process of the context starts, and
  read back — a context that reads off makes no view, loads nothing and says `no web engine here
  (WebKit's web process sandbox did not turn on)`). WV-B measured, under the private compositor:
  the view's process tree is its network process, four `bwrap`s, `xdg-dbus-proxy` and the web
  process, whose parents are `bwrap`, `bwrap`, sopen and whose `/proc/<pid>/status` says
  `NSpid: <pid> 2` — a PID namespace of its own. A file view's context sends every
  connection to a dead proxy, `http://127.0.0.1:0` — nothing can listen on port 0: a click, or
  Enter, on a link out made WebKit open a bare TCP connection to the link's host before the
  navigation policy refused it (measured with the filter alone: one, no request), which no
  content filter sees; with the proxy, none. Each layer holds a page's outside requests alone
  (WV-B, an outside `<img>`, stylesheet, frame, preconnect and prefetch: the filter alone 0
  connections, the proxy alone 0, neither 2 — so there is no direct fallback past the proxy). A
  meta refresh and a form made none either way (the policy's). The address window has no proxy.
- The page is egui content: dialogs draw over it, nothing hides, and `SOPEN_SCREENSHOT`'s read-back
  contains it. Teardown: `gtk_widget_destroy` and drain, 0.26–1.11 ms (WV-B); the view's network
  process, web process, `bwrap`s and `xdg-dbus-proxy` gone 24 ms after its close, and after
  `kill -9` of sopen within 200 ms (WV-L1, WV-B; the polling page of WV-L1 not run).

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
- **Esc while the page has the keyboard takes it back; otherwise it leaves the viewer** (or closes
  the address window, §7). The monitor consumes the first **[decision; 추정: the prototype passed it
  on]**. The other always-sopen keys take the keyboard back and act.
- While the page has the keyboard every other key is its own: typing, Tab (form fields), Cmd/Ctrl+C/V/A
  (copy from the page; *web* §1.4's decisions). macOS IME: WKWebView takes input methods as first
  responder **[추정: Korean not measured]**; Linux: §2.3's gap.

### 2.5 Lifecycle, processes and bounds

- At most two live views per browser window — the preview's and the viewer's (the first becomes the
  second at Enter, §6) — plus, after WV-F, one hidden page view per opened file in 08's set. One per
  address window.
- **Processes** **[추정: the model is WebKit's, not measured here]**. macOS: each WKWebView renders in
  a WebContent XPC process — "each web view is given its own web content process until an
  implementation-defined process limit is reached" — and separate `WKProcessPool` instances have had
  no effect since macOS 12 (the SDK's `WKProcessPool.h:30-37`), so sopen does not try to make one per
  view; networking runs in WebKit's Networking process. Linux: WebKitGTK runs
  a web process (and a network process) per `WebKitWebContext`; every view has its own ephemeral
  context, so its processes are its own. **Contract**: after a view closes, its web content process
  ends (the macOS Networking process may stay for other views) and nothing more of that view reaches
  the network; after the address half's input reaches EOF (§7), and after sopen itself dies (SIGKILL
  included), no web or network process of sopen's survives — WV-L1, measured by WV-A and WV-B.
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
  1. percent-decode; `..` in any spelling (`..`, `%2e%2e`, `.%2E`, `..%2f` — an encoded `/`
     splits too) and a NUL are 403, counted on the row, with **no filesystem call**, so nothing
     outside the root is even resolved; a path that is not UTF-8 names nothing (404); `.` and empty
     components are nothing; join under the root (`stream::page::join`) **[decision]**;
  2. the connection back from its last file (`into_connection`); `realpath`, so a link counts by what
     it points at; served only when the result is the root or below (the root-prefix check,
     `stream::page::inside`) — else 403, counted on the row; a path that does not resolve is 404
     when `stat_opt` finds nothing there, else the request fails;
  3. `stat`: a regular file goes to step 4. A folder asked with its `/` selects its `index.html`,
     and **that final path goes through step 2 and this step again** — `realpath`, root
     containment, regular file — before anything opens (an `index.html` that links out of the root
     is 403) **[decision]**. A folder asked without its `/` is not served its index at that address
     (its relative links would resolve one level up) but a 200 `text/html; charset=utf-8` page
     sending the engine to `NAME/` — a meta refresh and a link, NAME the last segment as the request
     had it (`stream::page::folder_page`) — as the scheme has no redirect status; the query does not
     reach the server (`scheme::Host::path_of` drops it), so the link carries none **[decision]**.
     Anything else is 404;
  4. `into_ranged(resolved)` on the checked path: the handle's `fstat` is the request's
     fingerprint (06 §5).
  That is ~4 RTT (realpath, stat, open, fstat): 48–160 ms at the reference rig's 12–40 ms
  **[추정; X-W1 measures]**.
- **Served in turn** **[decision]**: open requests take turns, one piece each — at most the scheme's
  `WINDOW` (1 MiB) in flight, the request's credit (`scheme::Body::room`), read as 64 KiB reads sent
  together — so a long response (a large image, an open-ended `bytes=0-`) never holds back the CSS
  behind it. Consecutive turns on one file (a `<video>`'s ranges) keep its handle. A later turn of a
  checked request on another file is `reopen(resolved)` — `into_connection().into_ranged(path)`
  (sftp/ranged.rs:392-393), ~2 RTT, no new checks — and the fingerprint is compared at every reopen:
  a file whose size or mtime moved fails a request under way (a network error to the engine), never
  padded. A new request has sent nothing, so a checked file that moved — or will not open, being
  gone — is forgotten and the request runs its steps afresh (served as the file is now, or 404);
  a new whole-file request on a checked path (a reload) is always a reopen, before anything is
  answered from its check — the kept handle and the block cache are for ranges (change 1); and a
  read that fails or ends early forgets the file, its charset and its kept blocks **[decision]**.
- **Transport**: 06's `remote::Ranged`, each piece handed to the engine as it arrives
  (`didReceiveData:`, the request's input stream on Linux), wrapped in `stream::Throttled`
  (stream/mod.rs:263) under `SOPEN_STREAM_THROTTLE`, as every stream is. A local page goes the same
  way, in place (06 §1), over `stream::page::serve_local`'s own reader — `LocalFs::into_ranged` is
  test-only. A short reply's rest is read at its own offset (`remote::Ranged`'s contract: a server
  may clip its replies); an empty reply inside the file is an early end and fails the request, as
  does a body short of its head's length **[decision]**.
- **Range** `bytes=a-b` → 206 with `Content-Range` and `Accept-Ranges: bytes`; unsatisfiable → 416;
  else 200 with `Content-Length` (measured on both engines, `<video>` seeking on macOS, *web* §4).
- **Type** by the extension of the name asked (`index.html` for a folder), a fixed table
  (`stream::page::mime`: html/htm `text/html`, xml `application/xml`, xsl/xslt `text/xsl`, and css,
  js/mjs, json, svg, png, jpg/jpeg, gif, webp, woff/woff2, ttf/otf, mp4/webm, mp3/ogg/wav, pdf);
  anything else `application/octet-stream`. A text type that names no charset itself — by its own
  rule, only a label of a real encoding (the Encoding Standard's) counting: HTML by WHATWG's
  prescan of its first 1024 bytes (comments and other markup skipped; a `<meta>`'s `charset`, or
  `http-equiv=content-type` with a `content` naming one), XML by its declaration's `encoding` only
  at the very start (after a BOM), CSS by `@charset "…";` only as its first bytes
  (`stream::page::charset`) — gets the one `crate::encoding::pick` gives for its first 8 KiB
  (`kind::SNIFF_BYTES`) — the text view's default (08 §7.2), so the page and its source agree:
  `; charset=utf-8` for UTF-8 by its BOM, or by step 4 when the sample is valid UTF-8 — a character
  cut at the 8 KiB edge accepted: `kind::utf8_text`, never `kind::looks_like_text` (UTF-16/32 too)
  (step 4 is UTF-8 "valid or not": bytes that are not get none); `utf-16le` / `utf-16be` for UTF-16 by
  its BOM or the NUL pattern (a BOM decides before the header anyway: the Encoding Standard's
  *decode*); `utf-32le` / `utf-32be` for UTF-32, though the Encoding Standard defines no UTF-32
  label, so WebKit is expected to ignore it and not render the page as UTF-32 text **[추정: not
  measured]** — 08's text view reads it; anything else (CP949, step 3's guess) none, and the
  engine decides as a browser does **[decision; that the engine honours the header 추정]**.
- **Cancel**: the engine's stop (`webView:stopURLSchemeTask:`; the request's cancellable on Linux):
  the server ends a stopped request where the stop finds it — in its steps, after the step in
  progress (its file never opened); waiting between turns, at once (nothing more is read for it); in
  a piece, after the 64 KiB read in flight: nothing more of it reaches the engine and no further
  read is sent, and the piece's other reads, already sent (at most one window), are waited for and
  dropped, so the handle is clean for the next request **[decision]**. Nothing is sent to a stopped
  task (WebKit throws on that, X-W1); closing the view stops all.
- **Channels** (06 §6) **[decision]**: a shown remote page view holds **one** SFTP channel, as a text
  viewer does — in the preview the window's preview session, given back when the bar moves on (06 §13
  item 3, `streamed::Lease`); in the viewer the connection that opened the file (prepare.rs:188-203),
  or the preview's, carried over. A page view hidden in 08's set (WV-F) gives its session back, as 08
  D14 does a save connection, and connects again at its next request. WV-D adds the row to 06 §6.
- **Preview budget** **[decision]**: a preview's page reads at most 32 MiB in all (02 §4.3's bound
  for an image read whole), counted as bytes read — a small range's block and a charset's sample
  included: each request reserves what it will read before its head, one that would pass the bound
  fails before its head (`stream::page::Control::cut`), and a stopped request gives back what it
  never read. **The hand-over lifts it** (`Control::lift`), as Enter lifts a
  preparation's budget (preview.rs:50); a view whose budget refused anything reloads once at the
  hand-over, so the viewer shows the whole page. The viewer has no byte budget.

#### X-W1 result (2026-11-25)

A prototype outside the repo ran this section's server — the steps, Range, turns of one piece,
cancel; one worker thread and one connection per view — over a real-time throttled fake of one SFTP
channel (a reply one round trip after its request, bytes at 2 MB/s behind the reply before:
`Throttled`'s timing, stream/mod.rs:263-291, at 50 ms RTT). Engines: WKWebView in an eframe window
(Rust, objc2-web-kit 0.3.2; WebKit 21624.5.1.11.3, macOS 26.7.1) on the Mac; WebKitGTK 2.52.6,
dlopen'ed, offscreen on its own GTK thread, on the Linux laptop under a private headless compositor
(own runtime folder, bus and socket; the live session unchanged before and after every run). 56 Mac
runs, 46 Linux runs; a range is three runs unless it says one. The RTT counts follow sopen's calls at
cd521ca — `realpath` (sftp.rs:787-810) and `stat` (sftp.rs:354-369) one each, `open_ranged` = open
then fstat (sftp/ranged.rs:158-194), `reopen` = `into_connection().into_ranged` with the CLOSE not
waited for (sftp/ranged.rs:392-400) — so they hold by construction; the wall times are measured
through the engines. The fake's sleeps overshoot on the Mac: 4 RTT took 207–239 ms there, 200.4–208.4
ms on Linux.

- **A handler answering from a worker thread — held, both.** Mac: `webView:startURLSchemeTask:`
  (main) retains the task and sends `(id, path, Range)` to the worker; the worker's messages come
  back by `dispatch_async_f` on the main queue, and only there call `didReceiveResponse:`,
  `didReceiveData:`, `didFinish`, through one function that checks the thread and a per-task
  tombstone and wraps the call in an exception catch: 0 off-main calls and 0 exceptions in every run.
  Linux: the scheme callback (GTK thread) refs the request and sends the same; once the head is known
  the GTK thread calls `webkit_uri_scheme_request_finish_with_response` with a
  `WebKitURISchemeResponse` (`_new(stream, length)`, `_set_status`, `_set_content_type`,
  `_set_http_headers` with a libsoup `SoupMessageHeaders`; the soup symbols resolve by `dlsym` on the
  WebKit handle) over a `GInputStream` subclass (`g_type_register_static_simple`, class and instance
  sizes asserted against `g_type_query`) whose `read_async` keeps a pending `GTask` until a piece
  comes; pieces cross by `g_idle_add_full`; every vfunc and completion ran on the GTK thread: 0
  off-thread in every run.
- **Push and pull.** The Mac is push: each piece goes to WebKit as it arrives, no flow control. Linux
  is pull: WebKit asks 8 KiB per `read_async`, one at a time, then closes with the synchronous
  `close_fn` and drops the stream, on the GTK thread; the glue's buffer peaked at one 64 KiB read
  because WebKit drained at once. A reader that stops pulling would let it grow **[미측정]**.
- **The head comes first, on both**: `didReceiveResponse:` precedes any data; WebKitGTK takes status,
  length and headers in the response object at finish. A request's first message is therefore its
  complete head (status, type, length, `Content-Range`); 403, 404 and 416 are a head with no body. The
  prototype did not sniff: it sent text types with no charset, so its head went when the steps ended.
  In sopen the head of a text type waits for the first read when the charset rule above needs its
  first 8 KiB (they come in that first 64 KiB read): it goes at 5 RTT together with the first bytes,
  not at 4 — the first byte stays at 5 RTT, so the page loses nothing; every other head goes when the
  steps end **[추정: derived from the measured RTTs]**.
- **Stop mid-response — held, both.** Mac: navigating away gave `stopURLSchemeTask` on the main thread
  9 ms after `loadRequest` (one run; 2 MiB of 20 MiB delivered); later pieces were dropped at the
  tombstone. Negative control (one run): one `didReceiveData:` after stop raised
  `NSInternalInconsistencyException` "This task has already been stopped" — "WebKit throws" above is
  measured now. Linux: the page's `WebKitWebResource` emitted `failed` (code 302, "Load request
  cancelled"), then the pending read's `GCancellable` fired, both on the GTK thread, for the main
  resource and for an image alike; the glue completes that read with `G_IO_ERROR_CANCELLED` from an
  idle (never inside the emission), and WebKit closes and finalizes the stream. The worker sees a
  stop after the 64 KiB read in flight (16 ms Linux, 0.4–71 ms Mac, one run each), or after the steps
  in progress (150–168 ms when a 20-image page was left on Linux); 0 calls after stop.
- **Stop before the head.** Mac: `stopURLSchemeTask` comes before any response — 19 of 19 head-less
  images when a 20-image page was left at +600 ms; late heads are dropped. Linux: nothing on the
  request fires — WebKitGTK 2.52 has no cancel signal on `WebKitURISchemeRequest`, and a read's
  cancellable exists only once reading starts. A late `finish_with_response` on the dead request is
  accepted and WebKit then keeps pulling its body at the link's rate — 5,373,952 B of 20 MiB by the
  end of each run — the next page's file waiting 674–676 ms instead of 183–186 ms and teardown taking
  442–454 ms instead of ~1 ms. The view's `resource-load-started` → `WebKitWebResource::failed` is the
  signal: 0.6 ms after the new load for the main resource (one run), and for 20 of 20 head-less
  images within 1.1–2.2 ms; acting on it, the glue drops the head, gives its ref back and never
  finishes — no warning, nothing read. Whether a media element's requests emit it is **[미측정]**.
- **Stop after the last byte.** Mac `<video>`: WebKit stopped 0–59 range tasks per run after their
  last byte but before `didFinish` came in the next main-queue block; the tombstone dropped those
  `didFinish`es, 0 exceptions.
- **Turns — held, both.** A CSS asked while a 6 MiB HTML streams ended 740–741 ms (Linux) / 764–772 ms
  (Mac) after it was asked, the HTML 4.05 s / 4.14–4.18 s after its own; FIFO control: the CSS 3.66 s /
  3.74–3.75 s. On Linux the page background from that CSS showed 2.34–2.35 s into the run while the
  HTML streamed until 5.26 s (FIFO: 5.70–5.72 s). A CSS behind a 4.3 MB image: 1.03 s / 1.08 s, FIFO
  2.88 s / 2.97–3.00 s. The CSS waits for the piece in flight (1 MiB at 2 MB/s = 0.52 s) plus its own
  5 RTT. A new request's first turn here is its steps and its first piece, on the handle just opened
  (else two more reopens).
- **A run of ranges on one handle — held only with change 1.** Read literally, every Range request is
  a new request: it leaves the `Ranged` and runs the four steps. Mac `<video>`: WebKit reads the MP4
  with 133 requests one after another, 131 of them tiny ranges (`bytes=0-1`, `0-7`, `32-39`, …; 2–784
  B each) and 2 without a Range. Re-checked,
  each costs 264–289 ms, and the video did not play within 60 s (0 of 3). Kept handle, each costs
  50–59 ms (the read): it played at 7.7–8.2 s in 2 of 4 runs and stalled in the other 2 (not within
  60 s). Kept handle plus a block cache (the request read as its aligned 256 KiB block, kept per view,
  later ranges inside it answered from memory): 130 of 133 ranges from memory, `playing` 428–477 ms
  after the page started; a zero-latency fake: 88 ms (one run). Ten scripted XHR ranges: Linux
  716–719 ms kept vs 2.52–2.54 s re-checked; Mac 0.77–1.01 s vs 2.88–3.13 s. Linux media made no
  request at all, `<video>` nor Ogg Vorbis `<audio>` (its demuxer present) — the laptop lacks
  gst-plugins-good **[추정: the cause]** — so Linux Range is measured with XHR; "measured on both
  engines" above had held for the Mac only (the research's Linux handler answered whole files).
  Linux Range needs `webkit_uri_scheme_request_get_http_headers` +
  `soup_message_headers_get_one("Range")`; the 206 goes as the response object's status and headers.
- **RTT.** A new request: realpath, stat, open, fstat = 4 RTT before the head (200.4–208.4 ms Linux),
  its first bytes 1 more — 5 RTT to the first byte, 60–200 ms at the reference rig's 12–40 ms, not
  48–160. A folder adds the index's realpath and stat: 6 RTT (one run: 300.7 ms Linux, 338.6 ms
  Mac). `reopen`: 2 RTT. **A page of R requests — the HTML and its subresources — costs R × 5 RTT,
  strictly in series on one handle.** The fixture was the HTML, one stylesheet and N images, R = N + 2
  (no favicon or redirect was requested): N = 1, 5, 20 → 757–761, 1766–1771, 5522–5536 ms Linux (Mac
  845–1091, 1945–2436, 6160–7621 ms); at 12–40 ms, 1.3–4.4 s for R = 22 **[추정: scaled]**.
- **The main-thread rule** (§12's X-W1 brief, *web* §4: "back on the main thread"). On macOS 26.7.1
  WebKit accepted task calls from the worker thread — whole requests answered off-main, 0.02–0.9 ms a
  call, no exception, pages loaded (negative control, two runs). Main-only stays sopen's rule for
  another reason: the tombstone and `stopURLSchemeTask` must share a thread for "no call after stop"
  to hold; off-main, a call can cross a stop.

**Changes this section takes [decision]:**
1. **A path checked once in a view is not checked again**, and small ranges come from a block cache:
   the view keeps each checked file's resolved path and fingerprint; a range on the file whose
   handle is open is served from that handle with no steps, on another file it is a `reopen` with the
   fingerprint compare (2 RTT) — a whole file asked again is always a reopen (§3.1 "Served in
   turn"); a range inside one aligned block (256 KiB here) reads that block, and later ranges inside
   a kept block are answered from memory, for the view's life and within its budget. Built as the
   page server's own cache (`stream::page`, not 06's): blocks keyed by the file's fingerprint, 16 MiB
   a view at most, the least recently used going first; one version of a file per view
   **[decision]**. Without the first a `<video>` costs 4 RTT per range; without the second it plays
   late or not at all.
2. A new request's first turn is its steps and its first piece.
3. **Stop, per engine**: Mac `stopURLSchemeTask` — before the head, mid-body, or after the last byte
   before `didFinish`; Linux the read's `GCancellable` mid-body and, before the head, the view's
   `WebKitWebResource::failed` matched to the request by URI (the request has no signal of its own).
   The glue keeps a tombstone per request and drops late messages; the server ends a stopped request
   as §3.1's Cancel says: after the step in progress, at once between turns, or after the 64 KiB
   read in flight (the piece's reads already sent drained, not delivered).
4. **The scheme trait (WV-0)**: in `start(id, path, range)`, `stop(id)`; out a complete `head`, then
   `data`*, then `done` or `fail` (a sniffed text type's head waits for its first read, above), each
   delivered by the engine glue on the engine's thread (Mac main queue, Linux GTK context by idle).
   The Linux glue owns a per-request buffer that WebKit pulls in 8 KiB reads and bounds it by credit
   to the server **[추정: the bound not built]**.
5. **Open for WV-C**: a page of R small files (the HTML and its subresources) costs R × 5 RTT on one
   handle. Pipelining the realpath and stat of every queued new request, and several handles open on
   the channel (open, fstat, read pipelined), would bring a page to ~5 RTT plus transfer **[추정: not
   built]** — it needs a batched `RemoteFs` call and a several-handle `Ranged`. A turn makes the
   request behind it wait one piece (0.52 s at 2 MB/s); pieces sized by link time (~100 ms) would cut
   that **[추정]**. WV-C built the second: while another request waits, a piece is what the link
   carries in `stream::page::LINK_TIME` at its rate measured from the last pieces, in whole 64 KiB
   reads; the first is WV-C2's (§12) **[decision]**.

Per unit: WV-C takes 1, 2, 3's server half and 5's piece sizing; WV-C2 takes 5's pipelining; WV-0
takes 4; WV-A retains each task, calls it only from the main queue behind the tombstone, and expects
the after-the-last-byte stop; WV-B builds the `GInputStream` subclass with its layout asserted at
start, completes reads outside the cancel emission, never holds its own state across a call into
GLib or WebKit (a completion can re-enter `read_async`) **[추정: designed around, not provoked]**,
watches `resource-load-started`/`failed` for the stop before the head, and reads Range from the
request's headers.

### 3.2 Engine settings, every view

No JS↔native bridge (no `WKScriptMessageHandler`, no script message handler); no UI delegate
(`window.open` makes nothing, JS dialogs show nothing); devtools off (`inspectable = false`,
`enable-developer-extras = FALSE`); no file URL ever loaded (Linux: `allow-file-access-from-file-urls`
and `allow-universal-access-from-file-urls` off); no downloads (a top-level response the engine cannot
show is cancelled, with a row line; on macOS a 403/404 head with nothing to show is shown empty,
never downloaded); WebKitGTK's web-process sandbox on; WebRTC off by setting where the engine has
one (Linux `enable-webrtc = FALSE`); DNS prefetching off where it has one (Linux
`enable-dns-prefetching = FALSE` **[추정: it may be a no-op in 2.52]**).

- **The context menu** (macOS, every view): WebKit's own, without its Download Linked File,
  Download Image, Download Media and Open Link / Image / Media / Frame in New Window items — the
  view's `WKWebView` subclass drops them by WebKit's identifiers (`strip_menu`, `DROPPED_MENU_ITEMS`
  in `src/gui/web/mac.rs`; the strings checked against WebKit's `_WKMenuItemIdentifier…`
  constants). Measured by WV-A (`wv_a_the_context_menu_offers_no_download_and_no_new_window`):
  Download Linked File on an outside link of a file view fetched it from the outside server,
  past the rule list and the navigation policy; with the items gone, nothing reaches it. Open Link,
  Copy Link, Copy Image, Look Up and Share stay: Open Link is a navigation, refused as a clicked
  link out (§3.4, 0 connections measured). Linux: no context menu at all — WebKitGTK's would be a
  window of GTK's own outside the page's texture (§2.3, "Nothing maps a window") **[decision]**.
- **No link preview** (macOS, every view): `allowsLinkPreview = false`. A force click on a link would
  otherwise load it in a WebKit preview of its own, with the default configuration — no rule list,
  the persistent store, past the navigation policy **[추정: the preview's configuration, from
  WebKit's documented default; not measured — a force click cannot be synthesized]**.

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
  **[미측정]**; WV-S2 goes through the channels. **On Linux the count is unavailable**, never 0:
  WebKitGTK says nothing of a load its filter blocks — no `resource-load-started` for it (measured,
  WV-B), and no public word like macOS's — so `Blocked` is never said there and the row shows no
  count at all, not "0 blocked"; the blocking is the filter's (and the dead proxy's, §2.3) all the
  same.
- **Scripts off has non-URL channels too.** An XSLT can build host names from a file it read into
  `<link rel=dns-prefetch|preconnect>` or an `<a href>`: a DNS lookup, or a TCP connect with no
  request, would carry them. The rule list is what contains them, and WV-S2's logging resolver and
  TCP listener measure it (how, below). On macOS WV-A measured the connects for what a page makes:
  `<link rel=preconnect|dns-prefetch|prefetch>` to an outside server opened 0 connections with the
  list and 1 without it (`wv_a_a_request_outside_the_scheme_is_blocked`); a lookup there is
  **[미측정]**. A load sopen itself asks of the view (`loadRequest`) is not contained that way:
  WebKit opened a TCP connection to an outside address before its navigation policy cancelled the
  load (measured, WV-A), so the macOS engine refuses an address outside the view's scope before
  WebKit sees it, with the refusal said as any navigation out (`Inner::load` in
  `src/gui/web/mac.rs`) **[decision]**. **If that scripts-off part of WV-S2 fails on a backend, file
  pages on that backend show their source**, with §4's line (`the page view is not contained here
  (<channel>): showing the source`), until a measurement shows containment.
- **Scripts are off by default**: `WKWebpagePreferences.allowsContentJavaScript = false`;
  `enable-javascript-markup = FALSE` (*web* §4.1-5). On macOS the engine itself holds a file view's
  scripts off — opened with them asked on, or asked on later — whatever its caller says, and logs
  it under `SOPEN_LOG=debug`: WebRTC is not contained there (R1, Tensions), so no caller's
  mistake can turn them on (measured, `wv_a_scripts_off_means_a_script_does_not_run`; an address
  window's scripts run, the control) **[decision]**.
- **F2** turns scripts on for this view and off again, reloading the document on screen. Its label
  names what it does (`JS on` / `JS off`); the top bar says `scripts off`, or `scripts ON` in the warn
  colour. A new view starts off; the preview is always off (F2 does nothing there).
- **Scripts-on channels fail closed.** A channel that is not a URL the rule list sees — WebRTC's STUN
  and TURN over UDP, DNS (a `<link rel=dns-prefetch>` or any name a script makes the engine resolve),
  anything else WV-S2 finds — is either turned off by a setting that WV-S2 shows works, or **file-view
  scripts are unavailable on that backend**: F2 there says `scripts are not available here: <channel>
  is not contained` and does nothing. **F2 turns scripts on only on an engine version WV-S2 passed
  on**: the version is read when the engine loads (WebKitGTK's `webkit_get_major_version` and its
  minor and micro; on macOS WebKit.framework's bundle version) and compared with the list of versions
  that passed, kept in the code; any other version — a new one included — has F2 off, saying `scripts
  are not available with this WebKit (<version>): not yet checked` **[decision]**.
- **How each channel is measured** (WV-S2): WebRTC by a UDP listener on loopback named as the STUN
  and TURN server; TCP by a listener that logs every accepted connection, a request or not (a
  preconnect opens one without a request); DNS by a logging resolver as the engine's only resolver
  (Linux: a private network namespace whose resolver is that logger; macOS: a per-domain resolver for
  a test domain pointing at it), names under that domain. An HTTP server that saw no request is never
  the evidence for DNS, a bare connection or UDP.

### 3.4 Links and navigation

- A link inside the scheme (the view's own scheme host) navigates within the root, in the view.
- **A file view never sends a link out.** A clicked link whose target is outside the scheme — http,
  https, `data:`, any scheme — is not followed: the row shows the target as `url::redacted` shows an
  address, with `link not followed from a file view — F5 copies it`. **F5** (`5Link` on the F-key row
  while there is one) copies the whole URL to the clipboard and the row says `copied — it may carry
  what the page read`; opening it is then the owner's own act **[decision]**. No test of the page
  decides this — not scripts, not whether the document is styled XML (the stylesheet instruction can
  sit past any byte limit, or in UTF-16, and a styled page can build a `data:` or `srcdoc` frame
  whose link would be clicked).
- Every other navigation out (a redirect, a meta refresh, `location=`, a form, a script's `a.click()`)
  is cancelled; the row says where it went, redacted the same way. On Linux WebKitGTK's policy
  decision does not say which frame a navigation is for (WV-B: not even its request's
  `Sec-Fetch-Dest`), so in a file view a frame's navigation out (an outside `<iframe src>`) is
  refused and said as the main frame's is, where macOS leaves it to the rule list **[decision]**.
  A frame's `data:` or `blob:` document goes unless clicked; the main frame going there is told
  apart as the view's main resource starts and is stopped and said, the page staying on screen
  (WV-B measured: a meta refresh to `data:`, a script's `location` to `data:` and to a `blob:`).
  **`about:` fails closed**: an `about:` page loads no resource, so a main frame going there cannot
  be told apart — measured: a meta refresh to `about:blank`, and a meta refresh or a script's
  `location` to `about:srcdoc` (`#x` and `?x` too), reached the screen — so every frame's `about:`
  navigation is refused, `about:srcdoc` included. **Frame limit**: a file view never shows an
  `<iframe srcdoc>` (WebKitGTK asks for one as `about:srcdoc` with scripts on, as `about:blank`
  with them off) **[decision]**. Measured held as well, each page staying on screen at its own
  address with nothing outside reached: a form submitted to `data:` (held at its main resource),
  `window.open` of a `data:` page and `window.open('', '_self')` (nothing navigates),
  `history.back()` after a navigation within the root (back within it). A script's `javascript:`
  URL replaces the page's content with its result in place — the page's own script, as
  `document.write` would, the document still the scheme's at its address. A scheme response cannot
  redirect: the page server's heads are 200, 206, 403, 404 and 416 only. **Behind all of it**, the
  commit is checked (`gtk::Home`): a main-frame document committed outside the view's scope is
  never copied to the screen — the last picture of the view's own stays — and is stopped: the view's
  page loaded back the first time per load sopen asked for (the view's load, a user's reload), only
  stopped and said after that, so a page that leaves at every load cannot loop; Cmd/Ctrl+R then
  loads the view's own page, not the outside one. No input of the tests reached it; with the
  `about:` refusal taken away by a test hook, a page whose script leaves at every load was sent home
  once and then stopped, its red on screen throughout (and once more after a reload).
- **Only through the view's policy.** WebKit's ways of following a link outside the view's own
  navigation policy are closed on macOS: the link preview (off, §3.2) and the context menu's Open
  … in New Window (gone, §3.2); its Open Link is a click like any other (refused out of the
  scheme, `clicked`, F5 copies it).
- No back or forward (non-scope): a link followed inside the root is left with Esc; Enter opens the
  file again.

### 3.5 Threat model: the page is an untrusted file

| The page reaches | Scripts off | Scripts on |
|---|---|---|
| `sopen-page:` under the root, `data:`, `blob:` | shown (`<img>`, `<iframe>`, links, XSLT `document()`) | shown, and readable by script |
| files outside the root, `file:` | no (`realpath` checks, the index's included; no file URLs) | no |
| anything outside the scheme, by any element, stylesheet or XSLT | no (blocked; counted on the row) | no |
| DNS, bare TCP connects, WebRTC, other non-URL channels | XSLT runs with scripts off: the rule list contains it; WV-S2's resolver and listener measure it, else the source shows (§3.3) | contained per §3.3, else scripts unavailable |
| navigation out | none: a link out is shown and can be copied, never followed | none |
| native code, devtools, storage, downloads | none | none |

Nothing outside the root is ever served, nothing outside the scheme is ever loaded, and no link is
ever sent out by a file view, so what a page can read it can only show. The residual is the owner's
own act: copying a link the row showed (F5) and opening it themselves.

## 4. Fallbacks

- macOS always has WebKit. Linux without `libwebkit2gtk-4.1.so.0`, or with one that fails to load or
  start (a missing symbol, `gtk_init_check` false), or without its sandbox's `bwrap` and
  `xdg-dbus-proxy` (§2.3): sopen starts and every other window works.
  - A page view → the file's highlighted source (the text viewer, as before 09, on 1 Text · 2 Hex ·
    3 Raw), its notice area — the line under its rows where a viewer says what it has to — saying
    `no web engine here (<reason>): showing the source`. A preview shows the source with no line
    (02 §4.2: a preview has no rows); an engine that refuses a view it was asked for gives way the
    same way where it stands — the viewer to its source view with that line, the preview to its
    source at once with the line on the status row — and the window opens page files as their
    source from then on (WV-D).
  - An address → the system browser (`url::open`), as `--browser`, after one stderr line `sopen: no
    web engine here (<reason>); opening <shown> in the system browser`.
- A backend whose scripts-off containment WV-S2 did not show (§3.3): page views show the source the
  same way, the reason naming the channel; the address window is not affected.
- No display (Linux, `display_available`, gui/mod.rs:1206): an address goes to the system browser as
  today. The reason is found once per process; the hook `SOPEN_WEB=off` (WV-0) makes it "turned off
  (SOPEN_WEB=off)" on both systems, for fallback tests and receipts **[decision]**.

## 5. Which files are pages

- `.html`, `.htm`: the page is the default view.
- `.xml`: the page is the default when its first 8 KiB holds `<?xml-stylesheet` before the first
  element; plain XML and RSS default to the highlighted source — both engines show them as run-on text
  (*web* §5) — with the page as view 3 (§6).
  - **The 8 KiB**: today an `.xml` is decided by extension with nothing read (`xml` in `kind.rs`'s
    `TEXT` table, which `kind::detect_sniffed` checks before any read). The page decision reads
    `kind::SNIFF_BYTES` on the opening's connection
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
  requests blocked, and a link not followed (§3.4). F-key row `1Help 2JS on 3Exit 4Edit`, plus `5Link`
  while a not-followed link is on the row (F4 = 01 A2's editor on the file, as now;
  keys.rs:499-503). Cmd/Ctrl+R reloads; Esc leaves (§2.4).
- **The views in the viewer** are 08's keys (§4, D3–D4) on VM-7's binding, landed before WV-D: 1, 2
  and 3 by the table below. Which row a file takes is decided once, by the preparation, from the
  file opened — its resolved name and its head — never from the name the list shows (a link
  `x.html` to plain XML is `1 Text · 2 Hex · 3 Page`; one to text has no page); the preview hands
  its decision over with its view. What WV-F adds is the hidden page view's session given back
  and the pixel region per 10.
- **The views, settled here** (they supersede 08's D7 for these files; 08 already carries this table
  as its D40, and WV-F retires 08's D30 when the page view lands):

| Files | 1 | 2 | 3 |
|---|---|---|---|
| `.html`, `.htm`, styled XML | Page | Hex | the source, highlighted |
| plain XML, RSS | the source, highlighted | Hex | Page |

  A hidden page view shows nothing, takes no input and plays nothing (media paused **[추정 API]**).
- **Under message 23** (*XR-T* §0): the page view is a pixel region of the GUI backend at its cell
  rectangle (10); in the TUI the same view shows the file's source (§8). WV-D needs nothing from 08
  or 10 to ship (§2.1); WV-F binds it per 08 and 10.

## 7. The address window, and #82's contract

**Today (#82).** `open_url` (main.rs:598-612) opens the system browser (`url::open`, url.rs:316) and
prints `opened … in the browser`; as HOST's half (`--hold-until-eof`, main.rs:70-73) it checks the
port is private, opens the browser, prints `sopen-tunnel-open` (url.rs:288) and holds until its input
ends. The remote side prints `… is open on HOST through a tunnel — Ctrl-C closes it`
(forward.rs:649-668). When HOST's command ends first, the remote exits with HOST's code if ssh exits
within 2 s, else 1 with `… ended while ssh stayed for the browser's connections` (forward.rs:689-718,
01 §4a).

**From 09, on each route to an address:**
1. **Here** (no HOST, or HOST is this machine; main.rs:582): an xtools window with the address view;
   `opened <shown> in xtools` once it is up; sopen returns when it is closed (exit 0). A change: today
   the command returns at once.
2. **A tunnel** (loopback with HOST; main.rs:585-587). HOST's half checks the port as today and opens
   the window. **Only once the view's engine has started** (`Web::new` succeeded) it prints the fixed
   cue `sopen-tunnel-window` (`url::TUNNEL_WINDOW`) — an engine that fails takes §4's system-browser
   fallback with no window cue — then `sopen-tunnel-open` once the load has started. A thread reads
   its input; at EOF the window closes and HOST exits 0. **Closing the window** prints
   `sopen-tunnel-closed` (`url::TUNNEL_CLOSED`), tears the view down (its processes end, §2.5) and
   exits 0. Cues carry nothing of the address and are written ignoring write errors — never
   `println!`, which panics on a closed stdout (today's cue, main.rs:607) **[decision]**.
   - The remote side prints `<shown> is open on HOST through a tunnel — closing its window there, or
     Ctrl-C here, closes it` **only after it saw `sopen-tunnel-window`**; else today's `… — Ctrl-C
     closes it` (a HOST on §4's fallback, or an older HOST). The fallback-port forms alike.
   - **The closed cue counts whether it comes before or after ssh exits**: on ssh's exit the remote
     reads it only after `ended()` has drained the stdout relay (forward.rs:689-700), not before. On
     it the remote closes the tunnel at once (its input closed, the guard ends ssh: forward.rs:534-544),
     prints `<shown>: its window on HOST was closed; the tunnel is closed` and exits 0. Any other end
     of HOST's sopen keeps today's rule above.
3. **Forwarded as written** (not loopback; main.rs:590): HOST's sopen opens the window as in 1; the
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
  apply here. **Linux** (WV-B, measured against two loopback servers): frames of another origin
  load; a click out is refused at the policy and asks the other server nothing, though WebKit
  opens a bare connection to it first (no proxy here; measured: one connection, no request); the
  main frame is told apart as the view's main resource starts (the policy cannot tell it, and the
  view's address changes only at the commit), so a script's `location=` out is stopped there and
  said, and a 302 out at the main resource's redirected request — neither document ever on screen,
  but their requests can reach the other server first (measured: the redirect's `GET` in every
  run, the script's in one run of two) **[decision]**.
- **Failures**, each a line in the bar: the main page fails to load (the dev server is not up yet) →
  `cannot load <shown>: <reason> — Cmd/Ctrl+R tries again`; a TLS certificate the system does not
  trust → never accepted, `the certificate of <host> is not trusted — --browser opens it in the system
  browser`; HTTP auth or a client certificate → refused, naming `--browser`; the web process ends →
  `the page's process ended — Cmd/Ctrl+R reloads`. Plain `http` is not refused on macOS (§2.2: ATS
  does not apply to sopen's binary), so there is no ATS line.
- **`--browser`** (new, public): today's behaviour exactly, every route. With a file SOURCE a usage
  error (exit 2), as `--hold-until-eof` (main.rs:521-525). Passed to HOST on both forwarded routes
  (`tunnel_words`, forward.rs:361-364, and the run's words); an older HOST sopen does not know it and
  exits 2 with its own line.
- **Version skew**: cues are matched exactly (forward.rs:634). An older remote prints
  `sopen-tunnel-window` and `sopen-tunnel-closed` as plain lines and ends by today's rules; an older
  HOST sends neither, so the remote prints today's line. Both ends are meant to be one sopen
  (`SOPEN_TARGET_BIN`, the tap).
- **Tests** **[decision]**: tests/cli.rs's `sopen URL` tests use a `SOPEN_BROWSER` stand-in
  (cli.rs:3255-3262) and pass `--browser` from WV-E1, keeping every assertion; AGENTS.md's
  `SOPEN_BROWSER` row then serves `--browser`, the fallbacks and the address window's outside links
  (a file view sends none, §3.4).

**As built (WV-E2, 2026-11-27)** — `src/gui/web/address.rs`; each a **[decision]**:
- **Where the way is decided** (`address::way`): `--browser` and no display take the system browser
  before the engine is asked for; otherwise the engine is picked **before the window opens**
  (`address::engine_here`), so a process without one (Linux until WV-B, `SOPEN_WEB=off`) takes §4's
  fallback with no window flashing up. HOST's side decides the same on every route. A view the engine
  still refuses at the window's first pass closes the window and takes the same fallback, no cue
  said. Under `--hold-until-eof` the port check stays first.
- **The cues**: `sopen-tunnel-window` right after `Web::new` made the view; `sopen-tunnel-open` right
  after its load is asked for — not on the engine's `Loading`, which a load refused before WebKit
  sees it never sends, leaving the other side without its open line. `sopen-tunnel-closed` only after
  the owner closed a view that was made, once the window is gone (the view torn down first) — never
  after the input's end (the other side is gone; the window closes for it), never after a refused
  view. An input that ends before the first pass makes no view at all and says nothing (review round
  1). A scripted run (`SOPEN_SCREENSHOT`, `SOPEN_CELLS`) ends by its hook and ignores the input's
  end; its hook's close is a close here, so it says the closed cue. Route 1 and HOST's half of route 3 print `opened <shown> in xtools` once the view is made; all
  of it, and §4's stderr line, written ignoring errors.
- **The screen**: the title row is the program's block and `<shown>` (the window's title `xtools —
  <shown>`); under it the page's frame `Web Page`, single- or double-lined by §2.4, whose first row is
  the bar — the document on screen (`url::redacted`), and at its right `scripts ON` (the warn colour)
  or `scripts off`, `loading…` — and whose second row, when there is one, is the line; the page at
  the rest of the frame's cells; the F-key row `1Help 2JS off 3Exit` (`2JS on` while off).
- **Links out**: only an address `url::parse` takes (`http`, `https`, written plainly) goes to the
  system browser, on a thread of its own (`url::open` waits for the opener), one at a time — another
  click out while one is opening (route 3 opens any site with scripts on, and a script's `a.click()`
  may count as a click) says `<target>: not opened — a link is still opening in the system browser`
  (review round 1); any other scheme, or an
  address sopen refuses, is said — `<target>: not opened — only an http:// or https:// link goes to
  the system browser` — and never opened, since the system's opener starts an application for a
  `file:` link. The line says `<target>: opened in the system browser` (or the opener's error); any
  other navigation out `<target>: a navigation away from <origin>, refused`.
- **Again before anything was on screen**: Cmd/Ctrl+R and F2 load the address again while no
  document has been on screen (the first load failed: a dev server not up yet) — WebKit's reload has
  nothing to reload then; after one, they reload.
- **The failure lines** are told apart by the engine's error as macOS says it (`… (NSURLErrorDomain
  <code>)`): -1201…-1204 the certificate line, -1205 and -1206 the client-certificate line, -1012 and
  -1013 and a 401 or 407 with nothing to show the login line; anything else `cannot load …`.
  **Open**: a 401 whose body the engine shows is shown as the page — `Event` carries no status, so
  the login line needs the engine to say it (WV-A's file).
- **WebKitGTK's failures** (fix/wv-linux-failures, measured under a private compositor): both
  engines end a failure with `(<domain> <code>)`, and `address::failure` reads Linux's too — GLib's
  `g-tls-error-quark` 2 (a bad certificate: a self-signed server gave `Unacceptable TLS certificate
  (g-tls-error-quark 2)`) the certificate line, 5 (a certificate required) the client-certificate
  line; an authentication request, which WebKitGTK asks through its `authenticate` signal and the
  engine answers with none, is said with WebKit's scheme as the code (`… (WebKitAuthenticationScheme
  <n>)`, `gtk::asks`): 7 and 9 (measured: a TLS server asking for a client certificate is 7, its own
  certificate ignored for the measurement) the client-certificate line, 8 the certificate line, any
  other (measured: HTTP Basic is 2) the login line; anything else (`Could not connect to 127.0.0.1:
  Connection refused (g-io-error-quark 39)`) `cannot load …`. **A restricted port**: WebKitGTK says
  nothing of one — it commits `about:blank` and fails nothing (measured on all 83 of the Fetch
  standard's bad ports, 0 and 9 and 6000 among them; 2, 8, 80, 443, 3000, 8080 and others connect) —
  and the commit's check sent that view home again and again (1,691 times in 2 s). Now a load sopen
  asked for that shows a document outside its scope before reaching its page has failed: stopped,
  never sent home, said once — `port <n> is one WebKit never connects to (a restricted port)`, or
  `WebKit showed <address> instead of the page` for any other port — so the window says `cannot load
  <shown>: port 9 is one WebKit never connects to (a restricted port) — Cmd/Ctrl+R tries again`; after
  that load's own failure a document outside is only stopped; a navigation away from a page on
  screen is sent home once per load sopen asked for (the view's load, an address again, a user's
  reload), then only stopped and said; a failure after the page was reached never silences the
  check, and an outside document is never copied to the screen (`gtk::Home`, round 1). An
  authentication request's line is not said over by WebKit's own failure of the same load (none
  followed one in the measured cases, HTTP Basic and a client certificate).
- **Open**: Cmd/Ctrl+R while the page holds the keyboard on macOS reaches the page, not sopen: R is
  not among §2.4's always-sopen keys (`route`, `mod.rs`). Esc takes the keyboard back first.

## 8. The TUI

Message 23 makes xtools one cell-grid core with a terminal and a GUI backend (*XR-T* §0; 10). A page,
like an image or a video, is GUI-only content: the GUI backend hosts the page view as a pixel region
at its cell rectangle. In a terminal a page file keeps 08's slots and opens on its source (10 T21,
08 §9): HTML, HTM and styled XML on 3, view 1 the page; plain XML and RSS on 1, already their source
(§5). A picture-capable terminal (kitty, iTerm2, sixel) shows no snapshot of the page: the engine
runs inside a window (§2). The terminal form of every key here is 10's. `sopen URL` with only a
terminal: §4's no-display rule.

**The page view in a terminal** (WV-D, VM-8) is a hand-off box (10 §11) — its frame and rows are
cells; no engine is asked in a terminal — until 10's X6 draws its one-row hand-off (`the page is
drawn in the window — F12`). In a terminal the opening and the preview make the source and nothing
of the page (`Pages::here`: no server, no connection for it), so the viewer opens on 3; a page
file's preview made before a switch is made again, so a hand-over opens as a fresh open does in
either backend; `1` makes the page view, its server connecting on its own, and F12's window makes the view there; a window that goes takes its view
down and the next one makes it again (a remote page on the connection its server gave back, else
connected again).

## 9. Decisions

1–9 are the orchestrator's first round (2026-11-24), with 4 as review round 1 revised it and 5 as the
fix-check replaced it; R1–R9 are review round 1's and R10–R12 the fix-check's (the coordinator,
2026-11-24); 10–14 are this document's choices that
change a decision or a shipped contract (the smaller ones are marked **[decision]** where they are
made). All reversible.

| # | Decision | Why |
|---|---|---|
| 1 | One component, two engines: macOS WKWebView subview following the egui rect, hidden under egui layers, key monitor; Linux WebKitGTK 4.1 by dlopen, offscreen into an egui texture, acceleration off, HiDPI by size × scale + zoom, one path for X11 and Wayland | the only routes measured to work inside the window; Wayland has no child views, wry takes only Xlib handles (*web* §1.2) |
| 2 | Without WebKitGTK sopen still starts: a page falls back to its source with a line, an address to the system browser | an optional library must not break sopen (ci.yml:146-164) |
| 3 | Pages load through an in-process scheme fed by the stream layer, never a port; root = the opened folder when the file is in it, else its folder; nothing outside; no `file:` | no local process can reach it; generated docs reference parents (*web* §4, §4.1-1) |
| 4 | Scripts off by default, F2 per view; **every file view, scripts on or off, loads nothing outside the scheme** (revised: the first round allowed outside http(s) subresources with scripts off); no bridge, devtools, storage | XSLT runs without scripts and its `document()` can put root files into an outside URL — top level, in an iframe, in an SVG, in the preview without a click |
| 5 | **A file view never sends a link out**: a clicked link outside the scheme is not followed; the row shows it redacted, F5 copies it; links inside the scheme navigate within the root (replaced: the first round sent http(s) to the system browser, review round 1 did so only with scripts off outside styled XML) | the styled test leaks — a stylesheet instruction past 8 KiB or in UTF-16 escapes it, and a styled page can build a `data:`/`srcdoc` frame whose link is clicked; deleting the rule closes every path at once |
| 6 | Styled `.xml` defaults to the page, plain XML/RSS to the source (page as view 3); `.html`/`.htm` to the page | engines show plain XML as run-on text (*web* §5) |
| 7 | The preview shows the page, scripts off, after the rest, by 02 §4; views and scripts are chosen in the Enter viewer | message 20: modes in the viewer, not the preview |
| 8 | `sopen URL` → an xtools window on the target, limited to its origin; closing it ends the target's half and the tunnel; `--browser` keeps the old route | message 18 item 1 |
| 9 | TUI: a page is GUI-only; the source shows; the binding is 10's | message 23 (ssot item 2) |
| R1 | Scripts-on channels fail closed: off by a setting shown to work, or file-view scripts unavailable on that backend; F2 off until WV-S2 passes there | an HTTP server seeing nothing is no evidence for DNS or UDP |
| R2 | The index file gets the same resolution, containment and regular-file checks | an `index.html` that links out of the root would otherwise be served |
| R3 | A view's web and network processes end with it, with the address half's EOF and with sopen's death | a page left running keeps fetching after the owner closed it |
| R4 | Views: HTML/HTM and styled XML 1 Page · 2 Hex · 3 source; plain XML/RSS 1 source · 2 Hex · 3 Page (supersedes 08 D7 here; 08's D40) | decision 6's "the page as another view"; one table, no "unsettled" |
| R5 | `sopen-tunnel-window` before `sopen-tunnel-open`, sent only once `Web::new` succeeded; the window line only after it | the remote cannot otherwise know whether HOST fell back to the system browser or is older |
| R6 | The closed cue counts before or after ssh's exit, read after `ended()`; cues written ignoring errors | a fast ssh exit would race the relay; `println!` panics on a closed stdout |
| R7 | The page server resolves and checks on the `RemoteFs`, then `into_ranged`; later turns `reopen`; fingerprint compared at every reopen; `Throttled` under the hook | a `Ranged` resolves and stats nothing but its own file (sftp/ranged.rs:392-393) |
| R8 | The hand-over lifts the preview budget; a cut view reloads once | as Enter lifts a preparation's budget (preview.rs:50); the viewer shows the whole page |
| R9 | The address window: Esc takes the keyboard back, else closes it; redacted URLs everywhere; a line for each failure; never accept an untrusted certificate | a dev server not up yet, a bad certificate and a crash are its common states; addresses carry secrets (url.rs:14-15) |
| R10 | Scripts-off non-URL channels (DNS, bare TCP connects built by an XSLT) are measured by WV-S2's resolver and listener; a backend that fails shows file pages as source until contained | XSLT runs with scripts off; zero HTTP requests says nothing about a lookup or a connect |
| R11 | F2 turns scripts on only on an engine version WV-S2 passed on, read at load | containment shown on one WebKit says nothing about the next |
| R12 | WV-L1's close case checks the view's WebContent process and no further network activity, not one process pool per view | separate `WKProcessPool`s have no effect since macOS 12 (SDK `WKProcessPool.h:37`) |
| 10 | Scripts **on** by default in the address window | decision 4's reason (the root) does not apply: no root, the owner's own server; a dev server's page is usually a script app **[추정]** |
| 11 | Requests served in turns of one piece each | a long response must not hold back the CSS behind it |
| 12 | Esc with the page holding the keyboard takes it back, otherwise it leaves; Cmd/Ctrl+1…0 reserved too | one press, one action (08 §4); on macOS 04 §6.1's aliases would be lost |
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
  the root → followed; an outside https link in an HTML page → not followed, the row shows it
  redacted, F5 copies the whole URL; the Korean input source in a field → composed Hangul.
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
- **WV-L1** (WV-A, WV-B) Note the processes a view starts (the process list before and after it
  opens), and let its page poll (scripts on, a `setInterval` fetch to its own origin). Within 2 s of
  each event:
  - a file view closed → its web content process is gone, and the page server's own request log
    shows nothing after the close (the macOS Networking process may stay for other views);
  - the address window closed, and the address half's input ended (EOF) → its web content process is
    gone and the test HTTP server behind the address logs nothing more;
  - `kill -9` of sopen with both open → no web or network process of sopen's is left, and neither
    log shows anything more.
- **WV-S1** Fixtures with `../`, `%2e%2e`, a link inside the root pointing out, `sub/index.html` a link
  pointing out of the root (requested as `sub/`), an `/etc/hostname` iframe, a `file:` iframe → none
  served (server log: 403 or never asked); locally and through `local_server()`.
- **WV-S2** Outside channels, against a test HTTP server on 127.0.0.1:P whose listener logs every
  accepted TCP connection, a UDP listener, and a logging resolver (§3.3):
  - scripts off: `<img>`, CSS `url()`, `@font-face`, `<link rel=stylesheet|prefetch|preconnect|
    dns-prefetch>` to the server and to names under the test domain; styled XML whose XSLT reads a
    root file with `document()` and writes it into an outside `<img>` URL — at the top level, inside an
    HTML `<iframe>`, and as an SVG carrying the stylesheet instruction in an iframe; an XSLT that
    builds host names under the test domain from a file it read into `<a href>` and `<link
    rel=dns-prefetch|preconnect>`; the same in the preview → **zero** HTTP requests, **zero** accepted
    TCP connections, **zero** resolver lookups for the test domain; the row counts the blocked ones.
    A backend that fails any of these shows file pages as source (R10);
  - links out, clicked (owner, or `hitTest` + a synthesized click where it reaches) → **not
    followed**, the row shows the URL, the `SOPEN_BROWSER` stand-in never called: from an HTML page;
    from an XML whose `<?xml-stylesheet` sits past the first 8 KiB; from a UTF-16 styled XML; from a
    `data:` frame and a `srcdoc` frame a styled XML built;
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
  xtools`. The page holding the keyboard (macOS: `makeFirstResponder`; Linux: a forwarded click) → Esc
  takes it back, the window stays; Esc again → it closes, exit 0. A window where sopen holds the
  keyboard → one Esc closes it. Nothing listening on P → the cannot-load line; Cmd/Ctrl+R after
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
  the window there closes. ATS: WV-A recorded the Mac's plain `http://` loads (§2.2) — none refused,
  so the address loads on the Mac too.

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
| WV-C | **Page server**: root, request steps (index included), turns, `reopen`, fingerprint, Range, type and charset tables, cancel, budget and its lift | `src/stream/page.rs`, `src/stream/page/tests.rs` (new); `src/stream/mod.rs` (the `mod` line) | X-W1; WV-0's `src/scheme.rs` (the trait) |
| WV-C2 | **Pipelined steps** (X-W1 change 5): batched realpath/stat in `RemoteFs` and a `Ranged` with several open handles, in `src/remote/`, so a page of R files would cost ~5 RTT plus transfer **[추정]**. WV-C built the link-time piece sizing and pins today's R × (realpath, stat, open) cost (`change5_a_page_of_r_files_costs_r_times_the_steps`) | `src/remote/` (the batched call, the several-handle `Ranged`); `src/stream/page.rs` (their use) | WV-C |
| WV-0 | **Component core**: `Web`, `Engine`, the scheme trait, placement, the after-frame hide check, key states, scripts and F2's availability, `SOPEN_WEB`, `FakeEngine`, the compose call; the shell's part (§2.1) | `src/scheme.rs`, `src/gui/web/mod.rs`, `src/gui/web/fake.rs`, `src/gui/web/tests.rs` (new); `src/lib.rs` (the `mod` line); `src/gui/mod.rs` (`mod web`, `Shell`'s handle, after-frame check, teardown, monitor install, the compose call); AGENTS.md's `SOPEN_WEB` row | X-W1 (the trait first, so WV-C builds beside it) |
| WV-A | **macOS engine**: WKWebView, a non-persistent data store per view (no per-view process pool: §2.5, R12), monitor, shield, `takeSnapshot`, scheme glue, rule list in the temp store, ATS measurement | `src/gui/web/mac.rs`, `src/gui/web/mac/tests.rs` (new); `src/gui/web/mod.rs` (the `mod mac` line and `platform()`); `Cargo.toml`, `Cargo.lock` (objc2-web-kit; objc2-app-kit features); `.github/workflows/ci.yml` (one macOS-only line: `SOPEN_REQUIRE_WEBKIT=1`); AGENTS.md's `SOPEN_REQUIRE_WEBKIT` row | WV-0, X-W1 |
| WV-B | **Linux engine**: loader, GTK thread, context per view, offscreen copy, input, HiDPI, scheme glue, rules, settings | `src/gui/web/gtk.rs`, `src/gui/web/gtk/api.rs`, `src/gui/web/gtk/tests.rs` (new); `.github/workflows/ci.yml` (the Linux steps) | WV-0, X-W1 |
| WV-D | **Page view in the browser**: `page_default`, `Prepared::Page`, preview, viewer, root, F2, the rows, fallback | `src/kind.rs`; `src/gui/web/page.rs` (new); `src/gui/web/mod.rs` (`mod` line); `src/gui/browser/prepare.rs`; `src/gui/browser/preview.rs`; `src/gui/browser.rs`; `src/gui/browser/keys.rs`; `src/gui/chrome.rs` (`PAGE_KEYS`); `src/gui/browser/tests/page.rs` (new); `.prd/02-architecture.md` §4.2–§4.3; `.prd/06-streaming.md` §6 | WV-C, WV-0; each platform's receipts after WV-A / WV-B |
| WV-E1 | **The CLI half**: `--browser`, the three cues and their writes, the remote's lines, the closed-cue race, version skew — all on the fake ssh; HOST still opens the system browser | `src/main.rs`, `src/url.rs`, `src/forward.rs`, `tests/cli.rs`; AGENTS.md's `--browser` and `--hold-until-eof` rows | — (starts at once) |
| WV-E2 | **The address view**: the window, its screen, navigation, failures, scripts on, HOST's half opening it | `src/gui/web/address.rs` (new); `src/gui/web/mod.rs` (`mod` line); `src/main.rs` (`open_url`'s window route); `.prd/01-spec.md` (A10a, §4a); `README.md`; AGENTS.md's `SOPEN_BROWSER` row | WV-E1; WV-0, WV-A, WV-B |
| WV-F | **Page in 08's view set** (= 08's VM-9): §6's table, hidden page views paused and their session given back, the pixel region per 10 | `src/gui/web/page.rs`; with 08 VM-7's binding files (`src/gui/browser.rs`, `src/gui/browser/keys.rs`); 08 D30 retired (08 §3 and D40 already match §6) | WV-D; 08's VM-5, VM-6, VM-7; 10 |

**Order**: X-W1 → WV-0 (trait first) ∥ WV-C → WV-A ∥ WV-B → WV-D ∥ WV-E2 → WV-F; WV-E1 from the
start. **Only WV-F waits on 08's model** (VM-5 the view set, VM-6 the keys, VM-7 the binding) and on
10: WV-D shipped with VM-7's binding already landed, so a page file's views 1, 2 and 3 are §6's table
from WV-D on (§6, "The views in the viewer"); WV-F adds the hidden page view's session given back
and the GUI backend's pixel regions. **Shared files are sequenced**:
`src/gui/web/mod.rs` gets one `mod` line each from WV-A (with `platform()`), WV-D and WV-E2;
`src/lib.rs` one from WV-0 beside
08's VM-1/5/6 lines; `src/gui/mod.rs` (WV-0) is shared with 08's VM-2 and VM-7 and the browser
workstream's input hook; `browser.rs`, `prepare.rs`, `keys.rs` with 08's VM-7 and the F4 editor, one
at a time; `main.rs` WV-E1 → WV-E2; `ci.yml` by WV-A (the macOS line) and WV-B (the Linux steps),
different sections, the second to merge rebases; `AGENTS.md` by WV-0, WV-A, WV-E1, WV-E2, one row
each; only WV-A touches `Cargo.toml`/`Cargo.lock`. A conflict goes to the dispatcher, never settled by
dropping the other side (06 §13). A unit that shows something on screen brings a screenshot someone
looked at (06 §13).

## Tensions

- **08 D30 vs WV-D.** D30 shows HTML/HTM/XML as 1 Text, 3 Raw until 09's page view lands; WV-D
  landed it with VM-7's keys already in the GUI, so §6's table holds from WV-D on (§6, "The views
  in the viewer") and D30 stays only where file pages show their source (§4: no engine here, or
  one not contained; `Screen::NO_ENGINE`). (08's D40 already carries §6's table.)
- ***web* §8-6 and decision 4.** The research defaulted to blocking outside subresources; the first
  round allowed them with scripts off; the review round (decision 4 revised) blocks them in every file
  view, for XSLT's sake. Outside CSS and fonts therefore never style a file page.
- ***web* §1.4 vs decision 12.** The prototype's monitor passed Esc on after taking the keyboard back;
  09 consumes it — one Esc returns the keyboard, a second leaves.
- **Change 1's kept handle vs a changed file.** During a run of ranges on one open handle, a file
  rewritten in place goes unseen until the next reopen (a file renamed over it keeps reading the old
  one, as 06 §5's "replaced"), and ranges answered from kept blocks never see a change for the view's
  life; a whole file asked again is a reopen, so a reload does see it. No reconnect: a connection that
  stops replying fails the view's later requests until WV-F's reconnect (§3.1 "Channels") covers it.
- **01 A10a vs §7 route 1.** A10a: `sopen URL` opened the default browser and exited 0; from 09 it lasts
  until its window closes. Resolved: WV-E2 rewrote A10a (and §4a) to the window, `--browser` keeping
  the old route.
- **libtest vs AppKit's main thread (WV-A).** AppKit and WebKit run only on a process's main thread,
  and libtest runs every test on a thread of its own. The macOS window tests (§11) therefore run
  each scenario in a child process — the test binary again — whose main thread a Mach-O initializer
  (`CHILD_MAIN` in `src/gui/web/mac/tests.rs`, test builds only) takes over before libtest's `main`;
  its window is borderless and off every screen, and the child never becomes the active app.
- **R1 on macOS.** WKWebView has no setting that turns WebRTC off, so the macOS engine names it as
  not contained with scripts on (`uncontained(true)` in `src/gui/web/mac.rs`) and F2 stays off in
  file views there, whatever WV-S2 finds for the other channels; the address window keeps its
  scripts (decision 10).
- **The research's joined GTK thread vs WebKit's main thread (WV-B).** *web* §1.3 joined the GTK
  thread at the prototype's exit, and §2.3 once said "joined when the last window closes"; WebKit
  binds its main thread to the first thread that uses it, so a GTK thread restarted for a later view
  aborts the process (measured). The thread lives for the process instead, parked in its wait.
- **The row's count of outside requests blocked, macOS vs Linux (WV-B).** §3.3's count is macOS's
  (a private delegate word of WebKit's); WebKitGTK gives none, so a Linux file view's row never
  says it. The requests are blocked on both.
- **§11's Linux CI vs WV-B's `ci.yml`.** §11 plans WebKitGTK installed on the Linux runner and
  the engine tests under `xvfb-run`; WV-B's CI does neither: the engine builds with nothing
  installed (nothing linked), the display-free tests run, the engine tests skip without
  `SOPEN_REQUIRE_WEBKIT=1`, and the release binary's `ldd` is checked to name no WebKit, GTK, GLib,
  libsoup or cairo. The engine tests ran under a private kwin on the Linux laptop. An Ubuntu
  runner's user namespaces (AppArmor) decide whether WebKit's sandbox starts there — unmeasured.
- **The link-preconnect channel (WV-B).** §3.3 names a bare TCP connect as a channel the rule list
  contains; on Linux a click or Enter on a link out opened one before the policy refused the
  navigation, outside the filter's reach. The dead proxy of §2.3 contains it (measured); whether
  macOS's clicked link opens one was measured by WV-A as 0 connections (§3.2).
