# xtools — fixed target (TUI and GUI, editor, view modes, web pages)

Status: in-progress (opened 2026-11-24)

This file fixes the target of the xtools workstream, which covers owner messages 18 and 20–23. It
is not reinterpreted during the loop; a change of scope comes only from a new owner message, quoted
here verbatim. (Message 19, the F-keys, belongs to the browser workstream: `../browser/ssot.md`.)

## Owner messages (verbatim)

Message 18 (2026-11-24):

> 1. sopen 주소를 치면 터널만 하는게 아니라 타겟에서 sopen으로 프리뷰 보여줘 안에 크롬이든 뭐든 브라우저 내장해서 보여줘
> 2. 그리고 sopen xtools에서 .html이든 htm이든 xml이든 이면 해당 파일을 소스가 아니라 웹페이지 형태로 보여줘야함
> 이거는 좀 리서치하고 크롬을 빌드해서 넣는게 아니고 그냥 os별 디폴트 내장 브라우저 뭔가 쓸수 있을것 같은데 그걸로 보여줄수 있도록해줘

Message 20 (2026-11-24; its first line quotes a progress note sopen's driver had sent):

> >  sopen <URL>, and show .html/.htm/.xml files rendered as pages in the browser
>
> 프리뷰가 아니라 엔터 눌렀을때 표시되는 곳에서는 현재 파일을 어떤식으로 보여줄지 정할 수 있도록해줘
>
> 디폴트로 뷰어 모드인거고
>
> 이미지면 디폴트로 이미지로 보여주는데 모드를 헥스 모드 선택할수 있게 해줘. (모든 파일은 헥스 모드로 선택할 수 있음.)
> html이나 json등은 그냥 raw text 모드랑 hex mode모두 선택할수 있어야함.
> (이 뷰어상태에서 디폴트로 1이 디폴트 뷰, 2가 hex 뷰, 3이 기타 뷰로해줘. 그리고 텍스트 모드일경우에는 ascii, utf8, utf16, ucs2, ucs4 등 인코딩을 고를수 있어야함.)

Message 21 (2026-11-24):

> 그리고 에디터도 디폴트로 로컬 에디터에 git graph쓸수 있도록 해줘 이거 좀 생각해서 지금 이 90년대 취향으로 pctools/터보 c 에디터 감성으로 간단한 텍스트 에디터 + git graph로 깃작업할수 있는 에디터로 만들어줘. 신택스 하일라이트랑 디렉토리 뷰어가 있어야함. 아니 프리뷰를 보다가 f4를 누르면 디렉토리 패널 자체를 왼쪽으로 밀고 프리뷰 패널이 그대로 에디터가 되도록해주곻 왼쪽 디렉토리 뷰에 탭을 추가해서 디렉토리/git뷰를 전환할 수 있도록해줘. 이거 좀 생각해서 리서치하고 구현해줘

Message 22 (2026-11-24):

> 그리고 전체 프로젝트는 디폴트는 sopen이고 sopen은 cli고 디폴트 다기능 툴 이름을 xtools로 이름 지어줘 이 컨셉으로 간다

Message 23 (2026-11-24):

> 그리고 이 xtools는 터미널 tui가 디폴트고 (이미지나 영상 빼고 다 실제 tui랑 1:1 매칭되고 잇는 ui임
> 그 tui상태에서 버튼 누르면 tui가 그대로 gui로 변하게 해줘 gui에서는 이미지나 영상등을 출력해줄수 있고 그 이미지 출력할수 있는 터미널 모드도 있지 않나? 거기서는 네이티브 터미널에서 그대로 이미지와 영상 출력할 수 있게 해주고 이것도 리서치해서 어떻게 하는게 가장 좋을지 고민하고 작업 해줘

Message 25 (2026-11-27; it belongs to the CLI, 01 A18 — recorded here because it came in this drive):

> 그리고 llmux 처럼 sopen channel stable|preview 고를수 있고 sopen update 할수 있도록해줘

Message 26 (2026-11-27):

> sopen 으로 url 넣으면 디폴트로 target의 xtools에서 브라우징해줘 지금은 아무것도 안되고 있음

## Fixed scope

1. **Names (message 22).** The project and its command stay **sopen**: the CLI. The multi-function
   tool it opens (the file browser, the viewers, the editor, the git panel) is **xtools**, and that
   is the name the owner sees for it: window and screen titles, the help screen, the README.
2. **TUI first, GUI the same screen (message 23).**
   - xtools runs in the terminal as a TUI by default.
   - The GUI shows the same screen 1:1: everything except images and video (and web pages, item 5)
     is the same cells in both.
   - A button in the TUI turns the running TUI into the GUI, keeping where the owner is.
   - The GUI shows images and video. A terminal that can draw images (the research names which:
     kitty's graphics protocol, iTerm2's inline images, sixel, …) shows images and video in the
     TUI itself; any other terminal shows what a text screen can.
   - How this is built is decided by research with a prototype (XR-T in `loop.md`), not guessed.
3. **The built-in editor (message 21).**
   - xtools has its own simple text editor in the 1990s PC Tools / Turbo C IDE feel, with syntax
     highlighting, a directory view and git work through the git panel (03).
   - **F4 while previewing** pushes the directory panel to the left and turns the preview panel, in
     place, into the editor on the same file.
   - The left panel gets **tabs** that switch it between the directory view and the git view.
4. **View modes in the Enter viewer (message 20).** The window Enter opens lets the owner choose how
   the current file is shown. Choosing a mode happens there, not in the preview pane.
   - It opens in the file's **default view** (the viewer mode): an image as an image, a web page
     as a page, text as text.
   - **1** = the default view, **2** = the hex view, **3** = the other view.
   - **Every** file can be shown in hex (2).
   - An image's default is the image; hex is selectable.
   - HTML, JSON and similar files can be shown as raw text (3) and as hex (2).
   - In the text view, the encoding can be chosen: at least ASCII, UTF-8, UTF-16, UCS-2 and UCS-4
     (byte orders included where they apply).
5. **Web pages (message 18).**
   - `sopen http://…` or `https://…`, run on a remote, still reaches the target machine through the
     tunnel #82 built; the target opens the page in xtools with an embedded web engine instead of
     its system browser. Closing that window ends the tunnel, as ending sopen does today.
   - A `.html`, `.htm` or `.xml` file is shown as a web page, not as its source, local or remote.
   - The page is drawn by the engine each OS already has: WKWebView on macOS, WebKitGTK on Linux.
     sopen never builds or ships Chromium. The research note (`../research/web-view.md`, from VR-W)
     decides how it is embedded.
   - Message 26 makes this the default on every route: `sopen URL` opens an xtools window with the
     address on the machine that shows things (here, HOST through the tunnel, or HOST as written);
     `--browser` keeps the system browser (09 §7). Its "nothing happens" half was a forward bug:
     a KDE HOST's `xdg-open` ran a missing `kfmclient` and reported success, because the forward
     did not import `KDE_SESSION_VERSION` (#160).

Readings the workstream made on 2026-11-24 (reversible; the owner may change them):
- "xtools" in message 18 is the same name message 22 gives the tool.
- Message 21's "에디터도 디폴트로 로컬 에디터" makes xtools's own editor the default where sopen
  handed text to VS Code or Zed (01 A2, owner message 10). The spec keeps VS Code / Zed reachable
  and says how; it also decides whether `sopen host:/file.txt` from the command line opens the
  editor or the viewer.
- A web page, like an image or video, is GUI-only content (or drawn where a terminal can show
  pictures); the TUI shows its source otherwise.
- The preview pane keeps showing a file's default view; whether a web page runs live in the preview
  pane (it would run whenever the bar moves over it) is decided in the spec after the research.
- What "3, the other view" is for a file whose default view is already text is decided in the spec.
- Item 2 decides the architecture of items 3 and 4: they are not built as GUI-only screens before
  XR-T's decision.

## Not in scope

Unless a new owner message asks for them:
- Windows (sopen does not build there; `01-spec.md` §9);
- a full browser: tabs, history, bookmarks, downloads, accounts;
- a full IDE: projects, build, debugger, language servers.
