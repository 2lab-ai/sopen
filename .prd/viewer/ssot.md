# viewer — fixed target (web pages, view modes, text encodings)

Status: in-progress (opened 2026-11-24)

This file fixes the target of the viewer workstream, which covers owner messages 18 and 20. It is
not reinterpreted during the loop; a change of scope comes only from a new owner message, quoted
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

## Fixed scope

1. **An address opens in a sopen window (message 18 item 1).** `sopen http://…` or `https://…`,
   run on a remote, still reaches the target machine through the tunnel #82 built. The target no
   longer hands the address to its system browser: it opens a sopen window that shows the page in
   an embedded web engine. Closing that window ends the tunnel, as ending sopen does today.
2. **Web pages are shown as pages (message 18 item 2).** In sopen's browser, a `.html`, `.htm` or
   `.xml` file is shown as a web page, not as its source, local or remote.
3. **The OS's own engine, nothing bundled (message 18).** The page is drawn by the engine each OS
   already has: WKWebView on macOS, WebKitGTK on Linux. sopen never builds or ships Chromium. The
   research note (`.prd/research/web-view.md`, from VR-W) decides how it is embedded.
4. **View modes in the Enter viewer (message 20).** The window Enter opens lets the owner choose how
   the current file is shown. Choosing a mode happens there, not in the preview pane.
   - It opens in the file's **default view** (the viewer mode): an image as an image, a web page
     as a page, text as text.
   - **1** = the default view, **2** = the hex view, **3** = the other view.
   - **Every** file can be shown in hex (2).
   - An image's default is the image; hex is selectable.
   - HTML, JSON and similar files can be shown as raw text (3) and as hex (2).
5. **Text encodings (message 20).** In the text view, the encoding can be chosen: at least ASCII,
   UTF-8, UTF-16, UCS-2 and UCS-4 (byte orders included where they apply).

Readings the workstream made on 2026-11-24 (reversible; the owner may change them):
- "xtools" in message 18 means sopen's two-panel browser and the viewers it opens.
- The preview pane keeps showing a file's default view; whether a web page is rendered live in the
  preview pane (where a page would run whenever the bar moves over it) is decided in the spec after
  the research.
- What "3, the other view" is for a file whose default view is already text is decided in the spec.

## Not in scope

Unless a new owner message asks for them:
- Windows (sopen does not build there; `01-spec.md` §9);
- a full browser: tabs, history, bookmarks, downloads, accounts;
- editing a page in the web view.
