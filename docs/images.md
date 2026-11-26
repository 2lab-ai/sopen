# Images

```sh
sopen myserver:/tmp/shot.png       # image → view, rectangle crop / pixelize, cut / copy / paste, save back
```

The window shows the image fitted; the mouse wheel or a pinch zooms, a trackpad scroll or a
right-drag pans (`F` fits again, `A` is 100 % — `1` too in a window of its own; in the browser's
viewer `1` is view 1). Drag a rectangle (handles adjust it, dragging
inside moves it), then:

| Key | |
|---|---|
| `C` | crop to the rectangle |
| `P` | pixelize the rectangle (block size slider, 16 px by default) |
| `⌘C` (Ctrl+C on Linux) | copy the rectangle — or, with none, the whole image — to the clipboard as a picture |
| `⌘X` | cut: copy the rectangle, then fill it with the background colour; the rest of the picture is untouched |
| `⌘V` | paste into the rectangle: a picture as the toolbar's **Paste** choice says (**Stretch**, the default, to the rectangle's size; **Fit** keeps its aspect, centred, the rest in the background colour; **1:1** unscaled at the rectangle's top-left, cut off at its edges), text in the **Text** colour, wrapped to the rectangle's width and as large as fits. With no rectangle a picture goes 1:1 at the top-left, text at the top-left in letters a tenth of the image's height, and what was pasted becomes the rectangle |
| `Esc` | clear the rectangle |
| `⌘Z` / `⇧⌘Z` (Ctrl on Linux) | undo / redo |
| `⌘S` or `F5` | Save: the original is first backed up locally (path printed), then overwritten in place in the same format (PNG, WebP, BMP, TIFF lossless; JPEG quality 95). A server without `posix-rename@openssh.com` cannot replace a file safely: Save is off there, Save copy works. While it runs nothing is edited; `Esc` stops it before the original is replaced |
| `⇧⌘S` | Save copy: `<name>.edited.<ext>` next to the original |
| `⌥⌘S` or `F7` | Save As: the edited image to a new file on this machine (never over an existing one) |

The second toolbar row holds the **Paste** choice and two colours: **Background** (white by
default; on an image with alpha, **transparent** too) and **Text** (black). A pasted picture's
own transparency blends over what is there. When the clipboard holds both text and a picture,
text wins (that is what the window is handed); Korean text — also as Finder copies a file name,
decomposed into jamo — is drawn as syllables with an installed font that has Hangul; without one
Hangul shows as boxes and the status line says Hangul may not render. Every cut and paste is one
undo step, saved by Save or Save copy like the other edits; a key held until it repeats counts
once, a second press pastes again. Pictures go through the clipboard as 8-bit RGBA, so a 16-bit
image's copy is 8-bit.

A remote image shows its size (and its Exif thumbnail) at once, then fills in as it is read —
JPEG and PNG as a partial picture. An image whose decoding needs more than 512 MiB of memory (or
a file over 512 MiB) first says how much it needs and what this machine has, and waits:
**Continue** or **Close** — and for a large decode **Hex view** too (`2` in the browser, `H`), its
bytes in the hex editor.
A file that does not decode opens in the hex editor, saying why.

Animated GIF/PNG/WebP and multi-page TIFF open read-only (first frame / page; `⌘C` still copies).
Closing with unsaved changes asks first. Save keeps the ICC profile and Exif where the format
holds them; anything it cannot keep (XMP, text chunks, IPTC, comments) is named in the window when
the image opens and again in the result line.
