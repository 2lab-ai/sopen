# Showing a page with the OS's own web engine, inside sopen (2026-11-24)

Owner, verbatim (2026-11-24):

> 1. sopen 주소를 치면 터널만 하는게 아니라 타겟에서 sopen으로 프리뷰 보여줘 안에 크롬이든 뭐든 브라우저 내장해서 보여줘
> 2. 그리고 sopen xtools에서 .html이든 htm이든 xml이든 이면 해당 파일을 소스가 아니라 웹페이지 형태로 보여줘야함
> 이거는 좀 리서치하고 크롬을 빌드해서 넣는게 아니고 그냥 os별 디폴트 내장 브라우저 뭔가 쓸수 있을것 같은데 그걸로 보여줄수 있도록해줘

Engines: WKWebView (macOS, WebKit.framework) and WebKitGTK (Linux, `libwebkit2gtk-4.1`). No Chromium, CEF or
bundled engine. Base: sopen `main` = 2f7758a (eframe 0.37.0, winit 0.30.13, objc2 `=0.6.5`, raw-window-handle 0.6.2).
Every number below was measured today unless it says **[추정]** (inferred, not measured) or **[미측정]** (not measured).
Prototype sources and receipts are listed in §9.

## 0. Summary

**Both platforms can show the OS engine inside a rectangle of sopen's eframe window, following the egui layout,
with no whole-app backend switch.** The mechanisms differ:

| | macOS | Linux (KDE Plasma 6 Wayland, and X11 alike) |
|---|---|---|
| How | `WKWebView` added as a subview of winit's `NSView`; `setFrame` from the egui rect when it changes | WebKitGTK in a `GtkOffscreenWindow` (never mapped) on a GTK thread; pixels copied on damage into an egui texture; egui input forwarded as synthesized `GdkEvent`s |
| Linked? | WebKit.framework linked (present on every Mac; start cost within noise: median 6.1–6.9 ms either way) | **Nothing linked**: `dlopen("libwebkit2gtk-4.1.so.0")`; binary `ldd` = libgcc_s, libm, libc |
| Not installed | n/a | dlopen fails in 0.1 ms, the window shows the fallback; exit 0 |
| New crates | `objc2-web-kit 0.3.2` only (+1 lock entry, 0 new duplicate versions) | none (`libc` already in sopen) |
| Keyboard | page takes first responder and swallows Esc, F-keys, **Cmd+W and Cmd+Q**; a local `NSEvent` key monitor hands them back to sopen (measured, with a no-monitor control) | sopen never loses the keyboard; it forwards only the keys it chooses (typing into an `<input>` and Page_Down measured) |
| HiDPI | AppKit points = egui points (flipped view); WebKit renders at backing scale | view sized `scale×` in device pixels + page zoom `scale`, scale taken from winit (offscreen windows ignore `GDK_SCALE` and the output scale) — crisp at 2× (simulated via egui zoom), same layout as 1× |
| Over egui? | the native view covers egui popups in its rect (`hitTest` inside an egui popup → `WKWebView`) | it *is* egui content: popups draw over it |
| Renderers | wgpu and glow both measured | wgpu and glow both measured |
| Cost | `setFrame` 175–398 µs; teardown 0.3–3.5 ms | copy 0.4–1.3 ms/damaged frame (2× 1428×1158: 1.0–1.3 ms), upload 0.2–0.5 ms, input→new frame p50 4–16 ms (max 25–57 ms) in a 60 Hz wheel burst |

**Remote pages load through a custom URL scheme** (`sopen-preview://…`) served by sopen — relative CSS, images,
links and (on macOS) `<video>` with Range requests all measured working on both engines. No socket, no port, no
token: nothing outside the process can reach it, unlike the loopback byte server (§4).

**XML** (§5): both engines show plain XML and RSS as run-on text (no tree viewer, JS on or off); XML with an
`<?xml-stylesheet type="text/xsl"?>` PI is transformed and rendered. So "XML as a page" is only meaningful for
XSLT-styled XML.

**Kill criteria: not hit.** KDE Wayland does not need a backend switch (the offscreen route needs no window-system
embedding), and WebKitGTK works without a load-time link.

## 1. Embedding inside sopen's window

### 1.1 macOS — WKWebView as a subview (prototype `mac-proto/`)

- eframe hands the window handle to the app (`eframe-0.37.0/src/epi.rs:750-755`, `impl HasWindowHandle for Frame`);
  `RawWindowHandle::AppKit.ns_view` is winit's view.
- winit's view is flipped (`winit-0.30.13/src/platform_impl/macos/view.rs:159-162`, `isFlipped` → `true`), so the
  egui rect in points is the `NSRect` directly; wry does the same (`wry-0.57.3/src/wkwebview/mod.rs:1101-1118`).
- Measured (wgpu renderer, the sopen default, and again with glow — same results, `receipts/mac/run-glow.log`;
  `ppp 2.00` on this Mac; the prototype turns the default menu off as sopen does, `src/gui/mod.rs` `event_loop_hook`):
  - WKWebView created + `addSubview` + `loadRequest`: 128–206 ms (first in the process); `didFinishNavigation` ~790 ms
    after creation for the fixture page.
  - Resize: the view followed `ViewportCommand::InnerSize` (frame 714×578.5 pt → 914×678.5 pt,
    `receipts/mac/mac-3-resized.composed.png`); `setFrame` 175–398 µs, called only when the rect changes.
  - Teardown in `on_exit`: `removeMonitor` + `removeFromSuperview` + release: 0.37–3.54 ms.
  - **Z-order:** a subview sits above the window's GPU layer. `hitTest` at the centre of an egui popup drawn over the
    web rect returned `NSKVONotifying_WKWebView` (`receipts/mac/run-monitor.log`). Consequence: **egui menus, dialogs and tooltips cannot
    draw over the page**; sopen must hide (`setHidden`) or shrink the web view while one is open
    (`receipts/mac/mac-4-pair.png`: left = eframe read-back with the popup, right = composite).
  - **sopen's screenshot hook will not see the page**: eframe's read-back of its own surface has an empty rect where
    the page is (`receipts/mac/mac-4-egui-overlay.egui.png`). Receipts need WebKit's `takeSnapshot` (10–36 ms).
- Focus (§1.4) and teardown measured. Real mouse clicks were not measured: clicks posted to the app's own queue did not
  reach the page while the window was not key — and `screencapture -l` is denied here ("could not create image from
  window", no Screen Recording permission), so macOS receipts are composites of WebKit's snapshot over eframe's
  read-back at the measured frame (`mac-proto/compose.sh`). A hands-on look at the real window is the owner's.

### 1.2 Linux Wayland — the window system rules out a child view

- **wry's `build_as_child` is X11-only in code, not just in docs**: `wry-0.57.3/src/webkitgtk/mod.rs:150-153`
  returns `Error::UnsupportedWindowHandle` for any handle but `RawWindowHandle::Xlib`; the X11 path creates an X child
  window (`XCreateSimpleWindow`, `:212`) and wraps it with `gdk_x11_window_foreign_new_for_display` (`:233`), and
  needs GDK itself on X11 (`:157-158`). Docs: `lib.rs:77-78`, `:1558-1559`.
- **Wayland has no cross-client child or position:** `xdg_toplevel`'s requests are destroy, set_parent, set_title,
  set_app_id, show_window_menu, move (interactive), resize, set_max/min_size, (un)set_maximized/fullscreen,
  set_minimized — no position (`wayland-protocols-0.32.13/…/xdg-shell.xml`, listed on the Linux laptop). A subsurface needs a
  `wl_surface` of the same connection; winit and GDK each open their own `wl_display` connection **[추정 from the
  protocol model, not tested]**. GtkSocket/GtkPlug (XEmbed) are X11-only.
- So the measured options on KDE Wayland are:

| Option | What it costs | Measured |
|---|---|---|
| **(c) offscreen → egui texture** (recommended) | software rendering; input fidelity is sopen's job (§1.3) | yes — prototype `linux-proto/`, receipts below |
| (a) helper top-level window/process | a separate window the compositor places anywhere; cannot track sopen's layout; focus/stacking are KWin's | **not proven**: a mapped `GtkWindow` on the private kwin never finished loading in 20 s (html+css served, then `WebKitWebProcess … GLib-GObject-CRITICAL: invalid (NULL) pointer instance`), with accel default and NEVER; cause not investigated (headless compositor artifact possible) |
| (b) sopen under XWayland + X11 reparent | whole-app backend switch (every sopen window goes X11: fractional-scale blur on KDE **[추정]**, IME via XIM) and wry links WebKitGTK at load time (§2) | not prototyped: (c) works without it |
| WPE WebKit (designed for offscreen) | not packaged on the Linux laptop (`pacman -Q` shows no wpe/libwpe) | n/a |

### 1.3 Linux offscreen route — measured (prototype `linux-proto/`, eframe 0.37 wgpu, private headless kwin Wayland)

- Path: `gtk_offscreen_window_new` → `webkit_web_view` inside → on `damage-event`,
  `gtk_offscreen_window_get_surface` + `cairo_surface_map_to_image` (BGRA premultiplied) → `ColorImage::from_rgba_premultiplied`
  → `TextureHandle::set`; painted at the egui rect, clipped. Resizes follow the rect (`Cmd::Resize`).
- **Hardware acceleration must be `NEVER`**: with the default policy the process aborts ("GDK is not able to create a GL
  context: The current backend does not support OpenGL") once the page loads, in an offscreen window. With `NEVER`
  (`webkit_settings_set_hardware_acceleration_policy(…, 2)`; the enum is ON_DEMAND=0, ALWAYS=1, NEVER=2,
  `/usr/include/webkitgtk-4.1/webkit/WebKitSettings.h:69-73`) it renders; `WEBKIT_DISABLE_DMABUF_RENDERER=1` alone also did.
- Timings (Rust prototype, WebKitGTK 2.52.6):
  - `dlopen` 21.7–26.5 ms; GTK up on its thread ~190 ms after process start; first page bytes served ~265 ms.
  - Copy per damaged frame: 0.34–0.77 ms (≈714×590 at 1×), 1.0 ms (1428×1158 at 2×), 1.2–3.0 ms (1614×1198 at 1×);
    texture upload 0.15–0.48 ms. (`gtk_offscreen_window_get_pixbuf` in the C probe: 2.3 ms at 900×700;
    `webkit_web_view_get_snapshot`: 8–10 ms.)
  - Input → next copied frame during a 60-event wheel burst at 60 Hz (3 runs per size): p50 4.0–16 ms at
    1614×1198 @1×, 4.1–11.1 ms at 1428×1158 @2×; max 24.6–56.7 ms. 160–189 texture uploads in ~9.4 s; process CPU
    1.08–1.38 s user over 9.4 s wall with the burst, 0.4–0.6 s without (`receipts/linux/run-egui.log`).
  - Teardown: `gtk_widget_destroy` + drain 0.4–1.0 ms; GTK thread joined in 0.5–1.2 ms.
- Input: forwarded click on a `<label for>` toggled the checkbox, forwarded smooth scroll scrolled
  (`receipts/linux/egui1-12.png`, `egui1-34.png`, `hd2-small.png`), resize re-laid-out the page (`egui1-4-resized.png`).
  Keys: click into an `<input>` + keyvals `a b c` → "abc" typed (with or without a synthesized focus-in; with it the
  focus ring shows), click on the body + `Page_Down` scrolled (`receipts/linux/keys-typed.png`, `k3.png`,
  `run-keys.log`). Six consecutive loads in one view (html, XSLT xml, html…) each rendered (`reloads.png`).
  glow renderer: same results (`run-keys.log`, `glow-top.png`).
- **HiDPI:** a `GtkOffscreenWindow` rendered a 714×579 surface for a 714×579 view with `GDK_SCALE=2` (measured), i.e.
  it ignores the scale. Fix measured: view size × scale in device pixels and `webkit_web_view_set_zoom_level(scale)`
  — same CSS layout, text at device resolution (`receipts/linux/hidpi-before-after.png`: top = 1× surface drawn 1:1,
  bottom = zoom route; `hd2-checkbox-1to1.png`), clicks mapped (logical 100,461 → device 200,922). Fractional scales
  (1.25/1.5) would render at `ceil` and resample **[추정]**. The 2× run is egui `zoom_factor 2` on the private kwin:
  `kwin_wayland --virtual --scale 2` reached neither winit (ppp stayed 1.00) nor GTK (`gtk_widget_get_scale_factor` = 1),
  so a real scaled output was not seen; sopen must take the scale from winit (`pixels_per_point`), and the code divides by
  GTK's own factor if it is ever > 1. Real-display HiDPI is part of the owner's hands-on receipt.
- Not covered by forwarding today **[미측정]**: IME/Korean text into page fields (synthesized key events carry no
  preedit), cursor shape over links, drag and drop, right-click menu, page text selection + copy, accessibility,
  GPU-heavy pages (WebGL, big CSS animations) at software speed.

### 1.4 Event loop, focus

- **macOS:** WebKit runs on the main thread inside AppKit's loop, which winit already drives — no integration.
  Keyboard: once the page is first responder it consumes keys. A local monitor
  (`NSEvent::addLocalMonitorForEventsMatchingMask_handler(KeyDown)`) sees every key before dispatch; for Esc (53) and
  F1–F12 it calls `makeFirstResponder(winit view)` and passes the event on. Measured: page focused → Esc → focus back,
  egui saw `Escape`; page focused → F3 → focus back, egui saw `F3`. **Control without the monitor: the page kept focus
  and egui saw nothing** (`receipts/mac/run-monitor.log`, `run-no-monitor.log`). **Cmd+W / Cmd+Q** with the page
  focused: without routing, egui saw nothing (sopen's close prompt and Cmd+Q handling would be bypassed —
  `run-cmd-nomonitor.log`); with the monitor routing Cmd+W (13) / Cmd+Q (12) too, egui saw `Cmd+W` and Cmd+Q closed
  the window through `on_exit` (`run-cmd.log`). Cmd+C/V/A stay the page's (copy from the page) **[decision,
  reversible]**. Tab stays the page's (form fields) **[decision, reversible]**. The focus switch itself was driven by
  `makeFirstResponder(WKWebView)` standing in for a user's click.
- **Linux:** GTK lives on its own thread (`gtk_init_check` there, measured working beside winit's Wayland connection);
  the thread blocks in `g_main_context_iteration(NULL, TRUE)` and is woken by `g_main_context_wakeup` when sopen sends
  a command; repaint of the egui window is requested from the GTK thread after each copy. sopen keeps the keyboard:
  Esc closed the prototype window, text was forwarded as keyvals; nothing reaches the page unless sopen forwards it.

## 2. Start-up dependency (Linux)

- **dlopen works and needs no helper binary.** All 60-odd entry points (GTK, GDK, GLib, cairo, WebKit) resolve with
  `dlsym` on the single WebKit handle (its dependencies are searched): `linux-proto/src/gtk.rs` (`api!` table). The
  prototype's `ldd`: `libgcc_s.so.1, libm.so.6, libc.so.6, ld-linux` only. With the soname overridden to a missing one:
  `dlopen … cannot open shared object file`, 0.1 ms, the window shows "unavailable", exit 0
  (`receipts/linux/missing-crop.png`). sopen's existing precedent: `src/fnkeys/macos.rs:96-111`,
  `src/remote/local/trash.rs:559-603` (dlopen, nothing linked).
- **The linked route (wry/webkit2gtk-rs) breaks two gates:**
  - CI's "Runtime libraries (Linux)" step (`.github/workflows/ci.yml:146-164`) runs the release binary in a clean
    Ubuntu container with only `libasound2t64`; a wry build has `NEEDED libwebkit2gtk-4.1.so.0, libgtk-3.so.0,
    libsoup-3.0.so.0, libjavascriptcoregtk-4.1.so.0` (measured `readelf -d` on the Linux laptop) → that step would fail, and so
    would every Linux machine without WebKitGTK. Building it on Ubuntu CI would also need the `-dev` packages
    (`libwebkit2gtk-4.1-dev` and its gtk3/soup3/jsc dev deps **[추정 package names]**).
  - the Linux laptop gate build: `javascriptcore-rs-sys` build script fails — "Package javascriptcoregtk-4.1 was not found in the
    pkg-config search path". Cause: linuxbrew's `pkg-config` searches only linuxbrew dirs
    (`pc_path = /home/linuxbrew/.linuxbrew/lib/pkgconfig:…`); the `.pc` files exist in `/usr/lib/pkgconfig`
    (`webkit2gtk-4.1.pc`, `gtk+-3.0.pc`, `javascriptcoregtk-4.1.pc`, `libsoup-3.0.pc`). With
    `PKG_CONFIG=/usr/bin/pkg-config` the wry probe built in 38 s.
- The dlopen route changes neither CI nor the Linux laptop build.

## 3. Dependency tree (sopen's Cargo.toml + Cargo.lock copied, dependency added)

| Added | Lock entries added | New duplicate versions vs sopen baseline (`cargo tree -d -e normal --depth 0`) |
|---|---|---|
| `objc2-web-kit =0.3.2` (macOS target) | 1 | macOS: none · Linux: none |
| `wry 0.57.3` | 101 (incl. windows-only crates) | macOS: none (shares objc2 0.6.5, objc2-{foundation,app-kit,core-foundation} 0.3.2, block2 0.6.2, raw-window-handle 0.6.2) · Linux: bitflags 1.3.2/2.13.2, heck 0.4.1/0.5.0, proc-macro-crate 1.3.1/2.0.2/3.5.0, syn 1.0.109/2, toml_edit 0.19.15/0.20.2/0.25.19, winnow 0.5.40/1.0.5, serde_spanned 0.6.9/1.1.3, toml_datetime |
| dlopen (Linux) | 0 | — |

wry on Linux brings gtk-rs **0.18** (GTK3 bindings) and 13 `-sys` crates resolved at build time through `system-deps`
(atk, cairo, gdk-pixbuf, gdk, gdkx11, gio, glib, gobject, gtk, javascriptcore, pango, soup3, webkit2gtk). No
raw-window-handle conflict anywhere (single 0.6.2). Files: `deptree*/`.

## 4. Remote HTML: loading relative resources

- **Custom scheme (chosen).** macOS `WKURLSchemeHandler` registered on the configuration
  (`setURLSchemeHandler_forURLScheme`); Linux `webkit_web_context_register_uri_scheme`. The handler maps the URL path
  under one root, refuses anything that resolves outside it, answers with MIME type and Range (206 +
  `Content-Range`). Measured on both: `index.html` → `css/style.css`, `img/pic.png`, `img/big.png` (4.3 MB),
  `sub/page2.html` → `../css/style.css` all served through the handler. macOS `<video>`: WebKit sent ~40 small Range
  requests (`bytes=2809-2812` …) then played and seeked (frame at 3.52 s after `currentTime=3`,
  `receipts/mac/xml-mac-js.png` bottom). Linux `<video>`: no source request at all, `networkState=3`, over the scheme
  *and* over loopback HTTP — the Linux laptop lacks `gst-plugins-good` (WebKitGTK's optional "media decoding" dep; `qtdemux`,
  `autoaudiosink`: "No such element"), an environment gap, not the scheme.
- **Loopback byte server (W3) compared:** it would need a path-mapping mode for a subtree, a per-view token in the URL
  and the Host check (as `src/stream/http.rs` does for ffmpeg), and it is reachable by any local process during the
  view; the page gets an `http://127.0.0.1:port` origin. The scheme needs none of that and behaved identically on both
  engines. W3 stays for ffmpeg.
- **Feeding it from the stream layer [미측정 — the load-bearing unbuilt part]:** the prototypes read local files synchronously. In sopen the handler
  must not block: start the read on the stream layer (small files: one SFTP read; large media: `stream::Source` /
  block cache by range) and complete the task later — `didReceiveResponse/didReceiveData/didFinish` back on the main
  thread (macOS), `webkit_uri_scheme_request_finish*` on the GTK thread (Linux); `webView:stopURLSchemeTask:` /
  request cancellation must cancel the read.

### 4.1 Security design (a page is untrusted content)

1. **Scoped root.** URLs are `sopen-preview://root/<path>`; the handler resolves the path (SFTP `realpath` remotely,
   so symlinks count) and serves only inside the root; `..` escapes are refused. Measured: WebKit itself normalizes
   `sopen-preview://root/%2e%2e/outside.html` to `/outside.html` before the handler sees it (→ 404, page not shown), so
   the prototype's own `starts_with(root)` check was not exercised by that URL; a symlink inside the root pointing out
   is the case that check exists for [미측정].
   Root default: the folder the browser was opened on (the scope the owner opened), not the file's own folder —
   generated docs reference parents (`../css`, rustdoc `../../static.files`) **[decision, reversible; owner may narrow]**.
2. **No per-view secret needed** with the scheme (in-process only). If loopback HTTP were used: 128-bit token + Host
   check, as W3.
3. **Navigation policy.** Allowed: the scheme under the root (and, for item 1, the tunnelled origin only). Everything
   else is cancelled; a *user-activated link* (`WKNavigationTypeLinkActivated` / `WEBKIT_NAVIGATION_TYPE_LINK_CLICKED`)
   to http(s) is handed to the target's system browser (`url::open`), never a meta-refresh or script navigation.
   Measured: a top-level `loadRequest`/`load_uri` of `https://example.com/` → "nav refused" by the policy delegate; a
   `file:///etc/hostname` `<iframe>` was never requested (Linux resource log) / rendered empty (macOS). Not measured:
   a user's click on an off-root link (§1.1 — real clicks were not driven on macOS) **[미측정]**.
4. **No file:// access**: `allow-file-access-from-file-urls` / `allow-universal-access-from-file-urls` off (Linux);
   macOS pages never come from file URLs.
5. **JavaScript off by default** (`WKWebpagePreferences.allowsContentJavaScript = false`;
   `enable-javascript-markup = FALSE`). Measured both: "script did NOT run" off, runs on.
6. **No JS↔native bridge:** no `WKScriptMessageHandler`, no `register_script_message_handler`, no UI delegate
   (`window.open` creates nothing).
7. **Devtools off:** `setInspectable(false)`; `enable-developer-extras = FALSE`.
8. **No persistence:** `WKWebsiteDataStore.nonPersistentDataStore`; `webkit_web_context_new_ephemeral`.
9. **No downloads:** a response WebKit cannot show is cancelled (`canShowMIMEType` / `is_mime_type_supported`).
10. **Outside requests from subresources [미측정]:** an `<img src="https://…">` in a remote HTML file would still be
    fetched (tracking, IP leak). Block all `http(s):` subresources with a content rule list — the same JSON on both
    (`WKContentRuleListStore`, `WebKitUserContentFilterStore`): `[{"trigger":{"url-filter":"^https?:"},"action":{"type":"block"}}]`.
11. WebKitGTK's own bubblewrap sandbox for the web process stays on (never disabled in the prototype).

## 5. XML — what each engine renders (measured, JS off and on)

| File | macOS WKWebView | Linux WebKitGTK 2.52.6 |
|---|---|---|
| plain XML, no stylesheet (`application/xml`, `text/xml`) | text nodes run together: "Plain XML, no stylesheetA. Writer12.50 …" — no tree viewer | same |
| XML + `<?xml-stylesheet type="text/xsl" href="style.xsl"?>` | XSLT applied: styled HTML table | same (the `.xsl` fetched through the scheme) |
| RSS 2.0 (`application/rss+xml` or `application/xml`) | run-on text | same |

Receipts: `receipts/mac/xml-mac.png`, `xml-mac-js.png`, `receipts/linux/xml-linux.png`, `xml-linux-js.png`.
XSLT is on its way out of browsers: Chrome stops it on Stable in Chrome 158 (2026-11-17) and removes it fully in
2027 ([Chrome for Developers](https://developer.chrome.com/docs/web-platform/deprecating-xslt)); WebKit has signalled
intent, no date found ([LWN](https://lwn.net/Articles/1034560/)) — a risk for the styled-XML case.

## 6. Item 1 — the address in the embedded view

Today (#82): on the target, `open_url(url, hold=true)` checks the port is private, opens the system browser, prints
`sopen-tunnel-open`, then reads stdin to its end (`src/main.rs:597-611`); the remote side ends when the target's
session ends (`src/forward.rs:592-594` `SESSION_GRACE` 2 s). With the embedded view (measured: macOS loaded
`http://127.0.0.1:18767/sub/page2.html` with navigation limited to that origin; Linux the same over loopback HTTP):

- The target's sopen runs a window instead of exiting after spawning a browser; a thread keeps reading stdin and
  closes the window on EOF (the old "hold"). **New:** closing the window ends the target's sopen → its session ends →
  the remote side closes the tunnel within `SESSION_GRACE` — already handled there. The remote's line "— Ctrl-C closes
  it" (`forward.rs` ~651) should also say the window closes it.
- The port-privacy check, the display discovery on the target (`forward.rs:306-319`) and the system-browser path stay;
  the system browser remains the fallback (Linux without WebKitGTK) and the target of off-origin link clicks.
- Navigation: the tunnelled origin only; anything else → system browser on click (§4.1-3).

## 7. Recommended architecture

- **One module, `gui::web` (a "page" viewer kind), two backends behind one trait:** `place(rect)`, `hide/show`,
  `load(url)`, `close`, `snapshot`. The browser (xtools) preview pane and the Enter viewer host it for `.html/.htm`
  (and XSLT-styled XML, §8-4); `sopen http://…` on the target hosts it full-window.
- **macOS:** `objc2-web-kit =0.3.2` (macOS target only, same objc2 family as the pins), `WKWebView` subview of winit's
  view, frame from the egui rect each frame (set only on change), hidden while an egui popup/dialog is open, local key
  monitor for Esc, F-keys, Cmd+W, Cmd+Q, scheme handler fed by the stream layer, settings of §4.1.
- **Linux:** WebKitGTK 4.1 dlopen'ed on first use. Same API surface on X11 sessions, so one Linux path for both; on
  X11 a native child (wry-style reparent) would also be possible, but two Linux paths are not worth it **[decision,
  reversible]**. Details: (a `fnkeys`/`trash`-style runtime loader), one GTK thread for all
  views, `GtkOffscreenWindow` + software rendering + damage-driven copy into the egui texture, page zoom for HiDPI,
  input forwarding (pointer, wheel, keys sopen does not own; text as keyvals). Missing library → the source view (files)
  or the system browser (addresses), with one line saying why.
- **Receipts:** Linux receipts work headless (eframe's read-back contains the page); macOS receipts need
  `takeSnapshot` composited at the view's frame.

## 8. Open risks and the owner's decisions

Risks: Linux software rendering and forwarded input are not a native browser (IME, cursor, DnD, selection/copy,
heavy pages — §1.3) **[미측정]**; distros may drop `webkit2gtk-4.1` for GTK4 `webkitgtk-6.0`, which has no
`GtkOffscreenWindow` (a GTK4 snapshot path would be needed) **[추정]**; `<video>` on Linux needs `gst-plugins-good`;
XSLT removal (§5); the async stream-fed scheme handler is unbuilt (§4); real-display HiDPI/fractional scaling and real
clicks were not seen (macOS: no Screen Recording permission; Linux: headless only); the helper-window route (a) did
not load on the headless compositor and stays unproven.

Owner decisions (each reversible, default in brackets):
1. Linux route: offscreen-in-window [recommended] vs helper window vs XWayland whole app (§1.2).
2. Page root: the folder the browser opened [default] vs the file's own folder (§4.1-1).
3. JavaScript: off [default], or a per-view toggle key.
4. XML: render only XML carrying an `xml-stylesheet` PI (sniffed in the first 8 KiB, `kind::SNIFF_BYTES`) and keep the
   highlighted source for plain XML/RSS [default — the engines show those as run-on text], or render all XML anyway.
5. Off-root link click: system browser on the target [default] vs refuse.
6. Outside subresources (`https:` images/CSS in a remote HTML): blocked [default] vs allowed.
7. `sopen http://…`: embedded view by default with the system browser as fallback [default] vs keep the system
   browser and add a flag.

## 9. Files (prototypes and receipts)

Prototypes were kept outside the repo: a scratch folder on the Mac and `/tmp/sopen-research-webview/` on the Linux laptop.

- `mac-proto/` — eframe + WKWebView prototype (`src/main.rs`), `compose.sh`; run:
  `PROTO_SCRIPT=1 target/release/webview-proto-mac fixtures/site` (`PROTO_NO_MONITOR=1` control, `PROTO_PAGES=…`, `PROTO_JS=1`).
- `linux-proto/` — eframe + dlopen'ed WebKitGTK offscreen prototype (`src/gtk.rs`, `src/main.rs`);
  `PROTO_SCALE`/`PROTO_ZOOM`, `PROTO_WEBKIT_SONAME` (missing-library test).
- `linux/offscreen.c` — C probe (render switches, snapshot timings, XML, video); `linux/run-in-kwin.sh` + `inner.sh` —
  the private compositor harness.
- `wry-link/` — wry link-time probe; `deptree/`, `deptree-wry/`, `deptree-webkit/` — `cargo tree -d` inputs/outputs;
  `mac-linkcost/` — WebKit.framework start-cost probe.
- `fixtures/site/` — HTML with relative CSS/images/video/links/file iframe, XML/XSL/RSS.
- Logs: `receipts/mac/run-monitor.log`, `run-no-monitor.log`, `receipts/linux/run-egui.log`.
- More logs: `receipts/mac/run-cmd.log`, `run-cmd-nomonitor.log`, `run-glow.log`, `receipts/linux/run-keys.log`.
- `receipts/linux/` — `egui1-*.png`, `hd2-*.png`, `hidpi-before-after.png`, `click-before-after.png`,
  `xml-linux*.png`, `media-*.png`, `missing-crop.png`; `receipts/mac/` — `mac-*.composed.png`, `mac-4-pair.png`,
  `xml-mac*.png`, `pg-*` (traversal, external, http).

**Isolation of the Linux runs** (the owner's live Plasma session was never touched): every compositor run went
through `run-in-kwin.sh`: `env -i` (no inherited DISPLAY, WAYLAND_DISPLAY, DBUS_SESSION_BUS_ADDRESS),
`XDG_RUNTIME_DIR=/tmp/sopen-research-webview/rt` (0700), private `XDG_CONFIG/CACHE/STATE/DATA_HOME`,
`dbus-run-session` (its own bus), `kwin_wayland --virtual --socket wl-sopen-research --no-lockscreen
--no-global-shortcuts --no-kactivities`, never `--replace`; `inner.sh` refuses to start if the runtime dir is not the
private one or a display variable leaked; kwin stopped by its own PID. Verified before/after: the child saw only the
private bus (`/tmp/dbus-…`, names `org.kde.kglobalaccel`, `org.freedesktop.a11y.Manager`) and private socket; the live
session (loginctl session 4, `Type=wayland Desktop=KDE State=active`), the owner's `kwin_wayland` PID 1958, and
`/run/user/1000/{bus,wayland-0}` (mtime 16:23–16:24) were unchanged; no private kwin left running.
