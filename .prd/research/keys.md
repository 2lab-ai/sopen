# sopen keys research: file-manager shortcut survey, proposed key map, F1–F12 without fn

2026-11-20. Research only; no product code changed and nothing changed on either machine (all
commands below are reads). Evidence: one location per claim; `[추정]` = inference, not verified.

## 0. Answers first

- **Proposed default row (file list):** `1 Help · 2 Rename · 3 View · 4 Edit · 5 Copy · 6 Move · 7 MkDir · 8 Delete · 9 (empty) · 10 Tree`.
  F2/F5/F6 are the owner's. F1, F7 and F8 are what all six surveyed Norton-family commanders
  use (NC, Total Commander, mc, Far, Double Commander, Krusader). F10 = Tree is the owner's word.
  No surveyed manager puts the tree on plain F10, but sopen has no menu and no quit action for
  that slot, and NC/TC's tree key Alt+F10 stays as an alias. Details are in §3.
- **Viewer rows stay as they are.** In a viewer, slots 5, 6 and 8 mean Save, Goto and Asc/hx;
  in the list they mean Copy, Move and Delete. PC Shell's own manual says the bar changes with
  the context and the viewer. Every key that changes a file asks first, so a mis-press costs a
  dialog, not data (§3.4).
- **"F1…F12 always act as F-keys inside sopen":**
  - **macOS: possible but global, and not yet proven on this machine.** sopen would set the
    HID system's `HIDFKeyMode` to 1 while its window is focused and put the saved value back on
    blur, quit, sleep or crash. Fluor's source does exactly this through
    `IOHIDSetCFTypeParameter`, and Apple's IOHIDFamily source shows no entitlement or root check
    on that path. Whether it still takes effect on macOS 26.7.1 is untested here: Fluor's issue
    reports are mixed.
  - **Linux: no per-app mechanism for an unprivileged app.** Every Fn-lock control found is
    root-only and tied to particular hardware, and none of them is wired for the Z13's keyboards.
  - **Windows: no OS API found.** It can only be done vendor by vendor (ASUS: ATK ACPI device
    `0x00100023`). Windows is also out of sopen's scope (spec §9).
  - **Fallback that works everywhere, recommended in every case:** make the F-key row clickable
    (PC Shell's was), and accept Ctrl+1…Ctrl+0 on Linux / ⌘1…⌘0 on macOS (egui's `COMMAND`
    modifier) as F1…F10. That matches the digits printed on the row.

## 1. Read-only measurements (quoted)

### This Mac
```
$ sw_vers; sysctl -n hw.model
ProductVersion: 26.7.1   BuildVersion: 25G241   Mac17,7
$ defaults read -g com.apple.keyboard.fnState
The domain/default pair of (kCFPreferencesAnyApplication, com.apple.keyboard.fnState) does not exist   (exit 1)
$ ioreg -c IOHIDSystem -r -d 1 | grep HIDFKeyMode      → … "HIDParameters" = {… ,"HIDFKeyMode"=0}
$ ioreg -l -w0 | grep -o -E '"HIDFKeyMode"=[0-9]+' | sort | uniq -c   →   236 "HIDFKeyMode"=0
$ hidutil property --get HIDFKeyMode                                       → (null)
$ hidutil property --matching '{"PrimaryUsagePage":1,"PrimaryUsage":6}' --get HIDFKeyMode
  RegistryID 100000fdf  HIDFKeyMode (null)
$ hidutil list --matching keyboard   → only "Apple Internal Keyboard / Trackpad" (AppleHIDKeyboardEventDriverV2, Built-In 1)
$ ioreg -r -c AppleHIDKeyboardEventDriverV2 -d 1 | grep FnFunctionUsageMap
  "FnFunctionUsageMap" = "0x0007003a,0x00ff0005,0x0007003b,0x00ff0004,0x0007003c,0xff010010,0x0007003d,0x000c0221,
   0x0007003e,0x000c00cf,0x0007003f,0x0001009b,0x00070040,0x000c00b4,0x00070041,0x000c00cd,0x00070042,0x000c00b3,
   0x00070043,0x000c00e2,0x00070044,0x000c00ea,0x00070045,0x000c00e9"
```
Decoded: pairs are (F-key usage, what it sends without fn). F4→Consumer AC Search 0x221 (Spotlight),
F5→Voice Command 0xCF (Dictation), F6→Generic Desktop 0x9B Do Not Disturb, F7→Rewind 0xB4,
F8→Play/Pause 0xCD, F9→Fast Forward 0xB3, F10→Mute 0xE2, F11→Volume− 0xEA, F12→Volume+ 0xE9 (names
from the SDK's `IOKit/hid/IOHIDUsageTables.h` lines 165, 1144–1184, 1327). F1/F2 → Apple vendor page
0xFF usages 5/4 and F3 → 0xFF01 usage 0x10: brightness down/up and Mission Control `[추정: from the
key-cap legends; these vendor usages are not in the public headers]`.
`com.apple.keyboard.fnState` is not documented by Apple; it is absent here. The value the keyboard
filter actually uses is `HIDFKeyMode`=0, which per IOHIDFamily means media-first (§4.1).

### The Linux laptop (the Linux laptop, over `ssh -o BatchMode=yes`, reads only)
```
uname: Linux <host> 7.2.6-1-cachyos … x86_64     DMI: ROG Flow Z13 GZ302EA_GZ302EA
session 3: Type=wayland Desktop=KDE Remote=no
$ ls -l /sys/devices/platform/asus-nb-wmi/   → boot_sound charge_mode cpufv driver_override hwmon input leds
  modalias nv_temp_target panel_od platform-profile power ppt_* rfkill throttle_thermal_policy uevent   (no fn_lock)
$ cat /sys/devices/platform/asus-nb-wmi/fn_lock      → No such file or directory
$ cat /sys/module/hid_apple/parameters/fnmode        → No such file or directory   (hid_apple not loaded)
$ cat /sys/module/asus_wmi/parameters/fnlock_default → Y
$ ls /sys/module/hid_asus/parameters/                → No such file or directory   (no params)
$ lsmod | grep -i -E 'asus|hid'  → asus_nb_wmi, asus_wmi, asus_armoury, hid_asus, hid_multitouch, i2c_hid(_acpi), mac_hid, hid_sensor_*, uhid
$ ls /sys/class/firmware-attributes/asus-armoury/attributes/ → boot_sound charge_mode panel_overdrive pending_reboot ppt_pl1_spl ppt_pl2_sppt ppt_pl3_fppt
$ lsusb | grep 0b05     → Bus 003 Device 003: ID 0b05:18c6 ASUSTek Computer, Inc. N-KEY Device
```
Keyboard-capable input devices, decoded from `/sys/class/input/event*/device/capabilities/key`:

| event | device | vendor:product | F1–F12? | media keys |
|---|---|---|---|---|
| event2 | AT Translated Set 2 keyboard | 0001:0001 | yes | mute, vol± |
| event4 | ASUSTeK N-KEY Device | 0b05:18c6 | **no** | mute, vol±, brightness±, KEY_FN_ESC |
| event5 | Asus WMI hotkeys | — | no | mute, vol±, brightness± |
| event20/24 | Lofree Flow2 (external) | 388d:0002 | yes | consumer page: mute, vol±, brightness± |
| event29 | AU05 keyboard (external) | fff1:00dd | yes | consumer page: mute, vol±, brightness± |

`0b05:18c6` is `USB_DEVICE_ID_ASUSTEK_ROG_Z13_LIGHTBAR` in `drivers/hid/hid-ids.h` (v7.2 line 230). The
Z13 folio keyboard `0x1a30` (`ROG_Z13_FOLIO`, line 229) was **not enumerated** at measurement time, so
the folio's own Fn behaviour could not be read today. The owner was probably typing on the
external keyboards `[추정]`.

KDE global shortcuts that use F-keys (`~/.config/kglobalshortcutsrc`, non-`none` lines):
Ctrl+F1…F4 switch desktop 1–4 · Ctrl+F7/F9/F10 present windows · Ctrl+F12 show desktop · Alt+F1
launcher · Alt+F2 KRunner · Alt+F3 window menu · Alt+F4 close · Meta+F5…F10. **Plain F1–F12,
Shift+F-keys and Alt+F10 are not bound.** Volume Mute/Up/Down, mic mute, and screen and keyboard
brightness are bound globally (lines 17–24, 224–230). Meta+0…9 are bound (task manager, zoom);
Ctrl+digits and Alt+digits are not.

## 2. Part A — survey of default keys

Sources (one per manager, primary; details in §7): **NC** = Symantec *Norton Commander 4.0 User's
Guide* (1993), key-bar table pp.11–12, NCD tree p.20, sort p.39. **PCS** = Central Point *PC Tools
Deluxe 6 Data Recovery – DOS Shell* manual (1990): menu list p.66, default F-key bar p.67 (read from
the page image), Short Cut Keys line p.73 (page image); 5.5 row from the *PC Tools v5.5* manual OCR.
**TC** = `KEYBOARD.TXT` shipped inside the Total Commander 11.58 installer (`tcmd115801x64.exe`).
**mc** = `misc/mc.default.keymap` + `src/filemanager/filemanager.c` (master). **Far** =
`far/FarEng.hlf.m4` (master), sections FuncCmd / PanelCmd / DeleteFile / FastFind. **DC** =
doublecmd.github.io/doc/en/shortcuts.html. **Kru** = Krusader handbook `keyboard-commands.docbook`.
**Dol** = Dolphin handbook `index.docbook` + `src/dolphinmainwindow.cpp`; KDE standard shortcuts from
`kconfig/src/gui/kstandardshortcut.cpp`. **Exp** = Microsoft "Keyboard shortcuts in Windows". **Fin**
= Apple 102650 "Mac keyboard shortcuts" + Apple "Rename files…" guide. **C1** = Electronic Team
Commander One help (Preferences; "F1-F12 keys don't work").
Legend: `—` none / menu only; `n/d` = not in the source read; `(2nd)` = only secondary sources.

### 2.1 Plain F-key rows

| Key | NC 4 | PC Shell 6 (5.5) | TC | mc | Far | DC | Krusader | Dolphin | Explorer | Finder |
|---|---|---|---|---|---|---|---|---|---|---|
| F1 | Help | Help (fixed) | Help | Help | Help | Help | Help | Help (KDE std) | n/d | — |
| F2 | User menu | QView (Index) | Re-read | User menu | User menu | **Rename** | **Rename** | **Rename** | **Rename** | — |
| F3 | View | Exit (fixed) | View (Lister) | View | View | View | View | Split view | Search | — |
| F4 | Edit | Unsel (Unsel) | Edit | Edit | Edit | Edit | Edit | Terminal panel | Address bar | — |
| F5 | **Copy** | **Copy** (Info) | **Copy** | **Copy** | **Copy** | **Copy** | **Copy** | Reload | Refresh | — |
| F6 | Ren/Move | Disply (Display) | Ren/Move | **Move** | Ren/Move | Ren/Move | **Move** | Edit location | Cycle panes | — |
| F7 | MkDir | Locate (Switch) | MkDir | MkDir | MkDir | MkDir | MkDir | Folders panel (tree) | n/d | — |
| F8 | Delete | Zoom (List) | Delete | Delete | Delete | Delete | Delete | n/d | n/d | — |
| F9 | Menu | Select (Select) | Menu (src side) | Menu | Menu | Terminal | Terminal | Places panel | n/d | — |
| F10 | Quit | Menu (fixed) | Left menu | Quit | Quit | n/d | Quit | Main menu | Menu bar | — |

**Only Krusader's defaults match all three of the owner's fixed keys** (F2 Rename, F5 Copy, F6 Move
as its own action); DC has F2 Rename + F5 Copy but F6 is "Rename or move". mc's F6 is Move (it also
renames). PC Shell 6 has 5 Copy; its F1/F3/F10 are "PC Shell system keys which cannot be changed",
the rest user-definable (p.67). PC Shell 6's **Short Cut Keys line** (advanced mode, a single letter,
no modifier, p.73; highlighted letters read from the full-resolution JP2 page): C Copy · M Move ·
T Delete · R Rename · V View · X HexEdit · I Find · P Print · L Locate · E FileEdit · U Undelete ·
Z Zoom.

### 2.2 Actions × managers — Norton family

| Action | NC 4 | PC Shell 6 | TC | mc | Far | DC |
|---|---|---|---|---|---|---|
| Help | F1 | F1 | F1 | F1 | F1 | F1 |
| User menu | F2 | — | — | F2 | F2 | — |
| Rename | F6 | R (line) / File menu | Shift+F6 (here), F6 | F6 | Shift+F6, F6 | F2, Shift+F6 |
| View | F3 | V; F2 QView | F3 | F3 | F3 | F3 |
| Edit | F4 | E (FileEdit) | F4 | F4 | F4 | F4 |
| Copy | F5 | F5, C | F5 (Shift+F5 same dir) | F5 | F5 (Shift+F5 one) | F5 (Shift+F5 same dir) |
| Move | F6 | M | F6 | F6 | F6 | F6 |
| Make folder | F7 | Disk › Directory Maint › Add | F7 | F7 | F7 | F7 |
| Delete | F8 | T (line) / File menu | F8, Del (trash per config) | F8 | F8 (trash per config), Shift+F8 one | F8, Del (trash) |
| Delete permanently | (DOS: always) | Clean File (wipe) | Shift+F8, Shift+Del | (always) | Shift+Del always; Alt+Del wipe | Shift+F8, Shift+Del; Alt+Del wipe |
| Menu | F9 | F10 | F9 / F10 | F9 | F9 | n/d |
| Quit | F10 | F3 Exit | Alt+F4 | F10 | F10 | Alt+F4, Alt+X |
| Select / unselect | Ins; Gray+ / Gray− group | F9 Select, F4 Unsel; Enter toggles (5.5) | Ins, Space; Num+ / Num− / Num* | Ins, Ctrl-t; KP+ / KP− / KP* | Ins; Gray+ / Gray− / Gray* | Ins, Space; Num+ / Num− / Num* |
| Select all | n/d | n/d | Ctrl+A, Ctrl+Num+ | n/d | Shift+Gray+ | Ctrl+A, Ctrl+Num+ |
| Re-read | Left/Right menu › Re-read (key n/d) | Options › Re-Read the Tree | Ctrl+R, F2 | Ctrl-r | Ctrl+R | Ctrl+R |
| Swap panels | Ctrl+U | — | Ctrl+U | Ctrl-u | Ctrl+U | Ctrl+U |
| Same folder in other panel | n/d | — | Ctrl+←/→ | Alt-i (sync), Alt-o (dir under bar) | n/d | Ctrl+←/→, Alt+Z |
| Up / back / forward | Enter on `..` | tree window | Backspace, Ctrl+PgUp / Alt+← / Alt+→ | Ctrl-PgUp / Alt-y / Alt-u | Ctrl+PgUp / Alt+F12 history | Backspace, Ctrl+PgUp / n/d |
| Quick search | Alt+letters | n/d | AltGr+ or Ctrl+Alt+letters | Ctrl-s, Alt-s | Alt+letters | Ctrl+S (or letters, option) |
| Compare folders | Commands menu | File › Compare File; Disk › Compare Disk | Shift+F2 (file lists) | Ctrl-x d | Commands menu | Synchronize dirs (no key) |
| Sort by column | Ctrl+F3 name, F4 ext, F5 date, F6 size, F7 unsorted | Special › Directory Sort | Ctrl+F3…F7 | menu (keys unbound) | Ctrl+F3…F11, Ctrl+F12 menu | Ctrl+F3…F6 |
| Show hidden | config | n/d | config | Alt-. | Ctrl+H | Ctrl+. |
| Properties / attributes | n/d | File › Attribute Change | Alt+Enter | Ctrl-x c / o / e | Ctrl+A; F4 on a folder | Alt+Enter |
| Multi-rename | — | — | Ctrl+M | — | — (plugin) | Ctrl+M |
| Pack / unpack | Alt+F5 / Alt+F6 | — | Alt+F5 / Alt+F6, Alt+F9 | — (VFS) | Shift+F1 / Shift+F2 | Alt+F5 / Alt+F9 |
| Find files | Alt+F7 | F7 Locate; I Find | Alt+F7 | Alt-? | Alt+F7 | Alt+F7 |
| New file | Shift+F4 | — | Shift+F4 | Shift-F4 | Shift+F4 | Shift+F4 |
| Change drive / location | Alt+F1 / Alt+F2 | Ctrl+drive letter | Alt+F1 / Alt+F2 | Alt-c, Ctrl-\\ | Alt+F1 / Alt+F2 | Alt+F1 / Alt+F2 |
| Terminal here | Ctrl+O (panels off) | DOS command line | command line | Ctrl-o | Ctrl+O | F9 |
| Copy path | Ctrl+Enter (name → cmd line) | — | Ctrl+P (→ cmd line) | Alt-a (→ cmd line) | Ctrl+Shift+[ ; Alt+Shift+Ins (full names → clipboard) | Ctrl+Shift+C (full names → clipboard), Ctrl+P |
| Clipboard files copy / cut / paste | — | — | Ctrl+C / Ctrl+X / Ctrl+V | — | Ctrl+Shift+C / Ctrl+Shift+X / n/d | Ctrl+C / Ctrl+X / Ctrl+V |
| **Directory tree** | **Alt+F10** NCD tree pop-up; panel "Tree" format via Left/Right menu | **Tree window on by default** (left of the file list); Options › Tree Window toggles; Tree/Files Switch | **Ctrl+F8** tree in panel; Ctrl+Shift+F8 separate tree; **Alt+F10** tree dialog | Left/Right menu "Tree", Command › "Directory tree" — **no key** | **Ctrl+T** tree panel; Alt+F10 find folder (tree) | **Ctrl+Shift+F8** tree view panel |

### 2.3 Actions × managers — GUI and Mac

| Action | Krusader | Dolphin | Explorer (Win 11) | Finder | Commander One |
|---|---|---|---|---|---|
| Help | F1 | (KDE help) | F1 | Help menu | n/d |
| User menu | Alt+` | — | — | — | n/d |
| Rename | F2 | F2 | F2 | Return | n/d |
| View | F3 | Enter opens; F3 = split view | Enter | Space (Quick Look), ⌘Y | n/d |
| Edit | F4 | — | — | — | n/d |
| Copy | F5 (Shift+F5 queued) | Ctrl+C / Ctrl+V; Shift+F5 to other view | Ctrl+C / Ctrl+V | ⌘C / ⌘V; ⌘D duplicate | F5 |
| Move | F6 | Ctrl+X / Ctrl+V; Shift+F6 to other view | Ctrl+X / Ctrl+V | ⌘C then ⌥⌘V | F6 |
| Make folder | F7 | Ctrl+Shift+N (KDE std `CreateFolder`) | Ctrl+Shift+N | ⇧⌘N | n/d |
| Delete | F8, Delete | Del (trash) | Delete, Ctrl+D (Recycle Bin) | ⌘⌫ (Trash) | F8, Delete (trash, if enabled) |
| Delete permanently | Ctrl+Delete ("alternate delete") | Shift+Del | Shift+Delete | ⌥⌘⌫ (2nd) | Shift+F8 |
| Menu | n/d | F10 (KDE std `OpenMainMenu`) | F10 | Ctrl+F2 (menu-bar focus) | n/d |
| Quit | F10, Ctrl+Q | Ctrl+Q | Alt+F4 | — | n/d |
| Select / unselect | Ins, Space; Ctrl++ / Ctrl+− group | Space | n/d | ⌘-click | Num+ |
| Select all / invert | Ctrl+A, Alt++ / Alt+− / Alt+* | Ctrl+A / Ctrl+Shift+A | Ctrl+A | ⌘A | ⌘Num+ |
| Re-read | Ctrl+R | F5 | F5 | — | n/d |
| Swap panels | Ctrl+U | — | — | — | n/d |
| Same folder in other panel | Ctrl+= (other panel's folder); Alt+Shift+O sync | Ctrl+F3 focuses the other view | — | — | n/d |
| Up / back / forward | Backspace, Ctrl+PgUp / Ctrl+J jump back | Alt+↑ / Alt+← / Alt+→ | Alt+↑ / Alt+←, Backspace / Alt+→ | ⌘↑ / ⌘[ / ⌘] | n/d |
| Quick search | type (search/select/filter); Ctrl+F bar | type-ahead; Ctrl+I filter | type-ahead | type-ahead | n/d |
| Compare folders | Alt+Shift+C | — | — | — | n/d |
| Sort by column | Meta+1…4 | View menu | column header | View › Sort By | n/d |
| Show hidden | Alt+. | Alt+. (KDE std also Ctrl+H) | View menu | ⇧⌘. (2nd) | n/d |
| Properties | Alt+Enter | Alt+Return | Alt+Enter | ⌘I | n/d |
| Multi-rename | Shift+F2 (KRename) | n/d | n/d | n/d | n/d |
| Pack / unpack | Alt+Shift+P / Alt+Shift+U | context menu | context menu | File › Compress | n/d |
| Find files | Ctrl+S | Ctrl+F, Ctrl+Shift+F | Ctrl+E, Ctrl+F, F3 | ⌘F | n/d |
| New file | Shift+F4 | Create New menu | — | — | n/d |
| Change location | Ctrl+L; Ctrl+M media | F6, Ctrl+L | Ctrl+L, Alt+D, F4 | ⇧⌘G; ⌘K server | n/d |
| Terminal here | F9; Ctrl+Alt+E embedded | Shift+F4; Shift+Alt+F4 here; F4 panel | — | — | n/d |
| Copy path | n/d | Ctrl+Alt+C | n/d (Ctrl+Shift+C is 2nd) | ⌥⌘C (2nd) | n/d |
| Clipboard files | n/d | Ctrl+C / Ctrl+X / Ctrl+V | Ctrl+C / Ctrl+X / Ctrl+V | ⌘C / ⌘V, ⌥⌘V move | n/d |
| **Directory tree** | Alt+↓ = sidebar (tree is one of its views `[추정]`) | **F7 Folders panel** | navigation pane (Ctrl+Shift+E expands to current folder; Num Lock+* / + / − expand/collapse) | none: list view → / ← open/close a folder, ⌥-click opens all; ⌥⌘S sidebar | n/d |
| F1–F12 without fn | — | — | — | — | **"Ignore system settings for F1-F12"** (Preferences › Hotkeys); needs Accessibility |

### 2.4 Where the directory tree lives (the owner's "f10인가")

| Manager | Tree key | What it is |
|---|---|---|
| NC 4 | **Alt+F10** | NCD tree pop-up over the panels (its own bar: F2 Rescan, F6 Rename, F7 Mkdir, F8 Delete, F10 Quit) |
| PC Shell 5.5/6 | none: **on by default** | The default screen is "one Tree and File List Window"; Options › Tree Window toggles it |
| Total Commander | **Ctrl+F8** / Ctrl+Shift+F8 / **Alt+F10** | Tree in the panel / separate tree / tree dialog |
| Far | **Ctrl+T** (Alt+F10 = find folder) | Tree panel |
| mc | none (menu) | Left/Right › Tree; Command › Directory tree |
| Double Commander | Ctrl+Shift+F8 | Tree view panel |
| Krusader | Alt+↓ | Sidebar |
| Dolphin | **F7** | Folders panel |
| Explorer / Finder | none | Navigation pane / sidebar; list-view disclosure |

So the owner most likely remembers NC/TC's **Alt+F10**. Plain F10 is Quit (NC, mc, Far, Krusader),
Menu (PC Shell, TC, Dolphin, Explorer) or not listed (DC) everywhere surveyed; no manager uses it for the tree.
(Finder's column is `—` because a MacBook's top row is media keys by default and Finder binds no plain F-keys in Apple 102650.)

## 3. Proposed default key map for sopen

Rule used: a key goes where most of the surveyed managers put it, unless (a) the owner fixed it,
(b) one of the two real machines grabs it globally (§1), or (c) sopen has no such action. Ctrl+letter
shortcuts are written `Cmd/Ctrl+X`: egui's `Modifiers::COMMAND` is Ctrl on Linux and ⌘ on macOS, so
one binding serves both. Today's code consumes F-keys with `Modifiers::NONE` (`src/gui/browser.rs:604`).

### 3.1 The file list: the bottom row (plain keys, all shown)

| Slot | Proposed | Why | File-changing? |
|---|---|---|---|
| 1 | **Help**: a read-only screen listing the keys | F1 = Help in all six commanders, PC Shell (fixed key) and KDE std. sopen has no help yet, and the alternates below are worth listing | no |
| 2 | **Rename** (owner) | DC, Krusader, Dolphin, Explorer, KDE std `RenameFile`=F2. Alias **Shift+F6** (TC, Far, DC "rename here") | **yes** |
| 3 | View (exists) | all six commanders | no |
| 4 | Edit (exists: VS Code / Zed) | all six commanders | no |
| 5 | **Copy** (owner) → the other panel's folder | NC, PC Shell 6, TC, mc, Far, DC, Krusader | **yes** |
| 6 | **Move** (owner) → the other panel's folder | mc and Krusader exactly; NC, TC, Far and DC "Ren/Move" | **yes** |
| 7 | **MkDir** | 6/6 commanders. **Owner call**: not in the owner's message (scope, §5) | **yes** |
| 8 | **Delete**. Aliases: **Del** (on a MacBook that is Fn+Delete, Apple 102650) and, macOS only, **⌘⌫** (Finder's "Move the selected item to the Trash", Apple 102650). **Shift+Del / Shift+F8** = permanent where a Trash exists | 6/6 commanders; Del in TC, DC, Krusader, Dolphin, Explorer; Shift+Del in TC, Far, DC, Dolphin, Explorer. **Owner call** (§5) | **yes** |
| 9 | (empty) | Convention is Menu (NC, TC, mc, Far) or Terminal (DC, Krusader); sopen has neither. sopen's row already draws empty slots for keys that do nothing (`chrome.rs:697-699`) | — |
| 10 | **Tree** (owner); alias **Alt+F10** | The owner said F10. Its usual meanings, Quit and Menu, have no counterpart in sopen: it closes with ⌘W or the window's close button and has no menu bar. Tree is the one new list function that needs a key on the visible row. Alt+F10 keeps NC/TC muscle memory and is free on the measured KDE (§1). In a viewer, slot 10 is empty | no |

### 3.2 The file list: other keys (not on the row)

| Action | Proposed | Convention behind it | Note |
|---|---|---|---|
| Switch panel | Tab / Shift+Tab (exists) | all commanders | — |
| Open / up | Enter, Backspace (exist); add **Ctrl+PgUp** as an alias for up | TC, mc, Far, DC, Krusader | — |
| Select / unselect | **Space** (primary) and **Ins** toggle the bar entry and step down | Ins in NC, TC, mc, Far, DC, Krusader; Space in TC, DC, Krusader | Space is primary because a MacBook has no Ins key. Space is unused in the list today: the typed-letter jump drops whitespace (`browser.rs:649`) |
| Select group / unselect group / invert | **+ / − / \*** (keypad or main row) | Gray +/−/\* in NC, TC, mc, Far, DC | egui 0.37 drops key location (`egui-winit lib.rs:1101`, `location: _`), so keypad and main row look the same, and neither laptop has a keypad. These three characters stop being jump characters |
| Select all | **Cmd/Ctrl+A** | TC, DC, Krusader, Dolphin, Explorer, Finder | — |
| Re-read the panel | **Cmd/Ctrl+R** | TC, mc, Far, DC, Krusader | F5 (the GUI refresh key) is Copy here |
| Swap panels | **Cmd/Ctrl+U** | NC, TC, mc, Far, DC, Krusader | — |
| Same folder in the other panel | **Cmd/Ctrl+← / →** | TC, DC (Ctrl+←/→) | On macOS, ⌃←/⌃→ switch Spaces by default `[추정: macOS Mission Control default, not read from an Apple page]`, so ⌘ is used there; egui's COMMAND does that by itself |
| Quick jump | typed letter (exists) | type-ahead in Krusader, Dolphin, Explorer, Finder | NC/Far's Alt+letters would collide with macOS ⌥ characters |
| Leave / cancel | Esc (exists) | — | — |
| F-key aliases | **Cmd/Ctrl+1…9, 0 = F1…F10** | mc turns Esc+digit into F-keys, Esc 0 = F10 (`lib/tty/key.c:1849`, `:106`) | Ctrl+digits are free on the measured KDE (Meta+digits are not, §1). Esc+digit is not usable in sopen, because Esc stops a fetch and leaves a viewer |
| Click on a row slot | the slot's action | PC Shell 6 manual: commands run "by pressing the highlighted key(s) or positioning the mouse cursor on any portion of the command … and clicking" | `fkey_row` only paints today (`src/gui/chrome.rs:700-732`) |

**Checked against sopen's existing keys** (`grep` of `src/gui`, tests excluded):
- **What exists:**
  - Cmd/Ctrl+Q and Cmd/Ctrl+W close the window (`gui/mod.rs:670`).
  - hex: Cmd/Ctrl+S and Shift+Cmd/Ctrl+S, Cmd/Ctrl+Z and Shift+Cmd/Ctrl+Z, Cmd/Ctrl+G (`hex.rs:1862-1877, 2362`).
  - image: Cmd/Ctrl+Z, Cmd/Ctrl+S and their Shift forms (`image.rs:1609-1616`), plus plain letters
    and the digit 1 through `plain_key` (`image.rs:1624, 1636`).
  - media: plain Space and O, Shift+arrows (`media.rs:720-740`).
  - list: Shift+Tab (`browser.rs:615`).
- **What that means for the proposal:**
  - No Cmd/Ctrl+A, R, U, ←/→ or any Cmd/Ctrl+digit is bound anywhere, so the proposals collide
    with nothing. Plain Space is the media window's play key, but that window has its own key pass.
  - The digit aliases should also act in viewers (⌘5 = Save in hex/image) and follow the same rule
    as the F-key they stand for: "a plain F-key never acts while a text field has the typing" (spec §3a).
- **egui matching caveat.** `consume_key` matches with `Modifiers::matches_logically`, which
  **ignores extra Shift and Alt** (`egui input_state/mod.rs:743-755`, `data/input/modifiers.rs:211-220`).
  Today's `consume_key(Modifiers::NONE, Key::F6)` therefore also fires on Shift+F6. Shift+F6,
  Shift+F8, Shift+Del and Alt+F10 must be checked **before** the plain F6, F8, Del and F10.
- **macOS keys.** Backspace (the MacBook key printed "delete") stays "go up" (`browser.rs:656`),
  which is harmless. ⌘⌫ is made macOS-only (`mac_cmd`, not COMMAND) because Ctrl+Backspace means
  "jump to root" in Krusader.

**Not proposed now** (convention noted; sopen has no such feature or the keys collide):
- **Sort**, Ctrl+F3…F6 (NC, TC, Far, DC). Ctrl+F3/F4 are KDE "switch to desktop 3/4" here (§1),
  and the spec sorts by name only (§3a).
- **Show hidden.** Convention is Alt+. (mc, Krusader, Dolphin), Ctrl+H (Far) or ⇧⌘. (Finder); the
  spec always shows dot files, dimmed.
- **Properties** (Alt+Enter), **find** (Alt+F7), **pack** (Alt+F5/F6), **terminal** (F9 / Ctrl+O),
  **copy path** (DC Ctrl+Shift+C, Dolphin Ctrl+Alt+C).
- **File clipboard** Cmd/Ctrl+C/X/V (TC, DC, Dolphin, Explorer, Finder; Finder moves with ⌥⌘V). If it
  is ever added: egui-winit turns Cmd/Ctrl+C/X/V into `Event::Copy/Cut/Paste` before any key event
  (`egui-winit lib.rs:1129-1140`), so the list has to read those events, not keys.

### 3.3 Viewers (unchanged) and slot reuse

| Window | Row today (spec §3a) | Same slot in the list | Is the reuse fine? |
|---|---|---|---|
| text | 3 Exit · 4 Edit | — | yes |
| hex | 3 Exit · 4 Edit · **5 Save** · **6 Goto** · **8 Asc/hx** | 5 Copy · 6 Move · 8 Delete | yes. PC Shell itself does this: "the functions on it will change depending upon what you are doing … when using the viewers, you will have different commands depending upon which viewer you are using" (PC Tools 6 manual p.67). sopen's hex keys copy PC Shell's hex editor: "press F8 or click on Asc/Hx on the Message Bar" (same manual). |
| image | 3 Exit · 4 Edit · **5 Save** | 5 Copy | yes |
| media | 3 Exit · 4 Edit | — | yes |

Why a mis-press is cheap:
- Both F5s write only after a check (hex/image Save re-checks the remote and keeps a backup; list
  Copy opens a dialog first).
- F8 Asc/hx only moves the typing.
- F8 Delete always asks first.
- F6 Goto only opens a field.

The one hazard is muscle memory. Someone who pressed F5 = Save in hex, then Esc, then F5 again
gets the Copy dialog, not a save. That is why Copy and Move must start with a dialog, never act
at once. F2, F7 and F10 do nothing inside a viewer.

The viewers' own F-keys already have non-F aliases (spec §3a): hex 5 = ⌘S, 6 = ⌘G, 8 = Tab; 3 Exit =
Esc / ⌘W. So **the fn problem is mainly the list's**: Rename, Copy, Move, MkDir, Delete and Tree have
no key other than their F-key. The digit aliases and the clickable row fix exactly that.

### 3.4 File-changing actions: conventional confirmation and sopen's rule

| Action | Convention | Proposed for sopen (AGENTS.md "Never overwrite silently") |
|---|---|---|
| F5 Copy | Dialog with the target path prefilled (the other panel); per-file question when the name exists: Overwrite / Skip / Rename / All (NC, TC, Far) | Dialog with the target prefilled; Enter = go, Esc = cancel. An existing name at the target stops for Skip / Rename / Cancel, and is **never overwritten** |
| F6 Move | Same dialog as copy | Same. The source is removed only after its copy is verified (cross-device and cross-host) |
| F2 Rename | Inline edit of the name (DC, Krusader, Dolphin, Explorer, Finder Return) or Far's Shift+F6 dialog | Inline edit in the bar row: Enter commits, Esc cancels. A name that exists → error, nothing changes |
| F7 MkDir | Name prompt | Name prompt; an existing name → error |
| F8 Delete | Always asks "Delete N files?" (NC, TC, mc, Far); Trash by configuration (TC, DC, Far, Krusader); Shift+Del permanent (TC, Far, DC, Dolphin, Explorer) | Always ask. Local: Trash; Shift+Del or Shift+F8 permanent, with stronger wording. **Remote: SFTP has no Trash**, so delete is permanent and the dialog must say so |

### 3.5 Remote (SFTP over the existing ssh session): which primitive per operation (notes only)

| Op | Primitive | Evidence / caveat |
|---|---|---|
| Rename / move within one remote FS | **plain `SSH_FXP_RENAME`** | OpenSSH `sftp-server.c:1261-1316`: for a regular file it does `link(old,new)` then `unlink(old)`, so it **never replaces** an existing target; for other types it renames only if `stat(new)` fails. **Do not use `posix-rename@openssh.com`**: it is POSIX `rename()`, which replaces the target (OpenSSH `PROTOCOL` §4.3). The crate in use, `openssh-sftp-client` 0.15.12, `Fs::rename`, picks posix-rename whenever the server offers it and documents "replacing the original file if to already exists" (`src/fs/mod.rs:210-235`). sopen would need the low-level plain rename request. |
| Move across file systems / hosts | Copy, verify, then `SSH_FXP_REMOVE` | Plain rename fails with EXDEV on a cross-device target (`sftp-server.c`: link → EXDEV → stat+rename → rename fails) |
| Copy within one remote host | **`copy-data`** extension (server-side; the data never crosses the network). Open the source for read; create the target with CREAT\|EXCL (no overwrite); `File::copy_all_to`; then setstat for mode and times | `PROTOCOL` §4.10; `sftp-server.c:164,725` advertise it; the crate implements it (`src/file/mod.rs:701-745`). Older servers: stream through the client (read + write). |
| Copy local ↔ remote, or host A ↔ host B | SFTP read and write through the local machine (sopen's existing fetch path); publish under a temp name by **`hardlink@openssh.com`** (never over anything, as spec §6a's "Save to…") or by plain rename | `PROTOCOL` §4.5. There is no direct host-to-host path. |
| Make folder | `SSH_FXP_MKDIR` (fails if it exists) | `sftp-server.c:1196` |
| Delete | `SSH_FXP_REMOVE` (files, links); `SSH_FXP_RMDIR` (empty folders); recursive = walk bottom-up | `sftp-server.c:1179,1217`. No remote Trash. A freedesktop Trash under the remote `~/.local/share/Trash` could be done with SFTP rename alone, but that is extra scope. |
| Symlinks while copying | `readlink` + `SSH_FXP_SYMLINK` | OpenSSH reverses SYMLINK's arguments (`PROTOCOL` §4.1) |
| `ssh host cp -a …` | faster, keeps all attributes | **Conflicts with AGENTS.md** "file operations are SFTP over that session" (it runs a remote command). `copy-data` covers the file content without it. Owner call. |
| Local side | no-overwrite rename (`renameat2(RENAME_NOREPLACE)` on Linux, `renamex_np(RENAME_EXCL)` on macOS), clone/copy_file_range for copy, OS Trash | `[추정: standard OS APIs, not read today]` |

## 4. Part B — "F1…F12 always act as F-keys inside sopen"

### 4.1 macOS (MacBook; measured: macOS 26.7.1, Mac17,7, internal keyboard only)

1. **Can an app switch it only while focused?** Not per app. There is one global switch, and an
   app can flip it on focus and flip it back on blur. That is what Fluor does.
   - **The switch** is the HID system parameter `HIDFKeyMode` (`kIOHIDFKeyModeKey`,
     `IOKit/hidsystem/IOHIDParameter.h:121`).
   - **How Fluor sets it** (`Fluor/Misc/FKeyManager.swift:57-106`): open
     `IOService:/IOResources/IOHIDSystem` with `IOServiceOpen(…, kIOHIDParamConnectType /*=1*/)`,
     then call `IOHIDSetCFTypeParameter(connect, "HIDFKeyMode", 0|1)`. It reads the value back from
     the registry property `HIDParameters["HIDFKeyMode"]`.
   - **SDK status:** `IOHIDSetCFTypeParameter` carries no deprecation mark in the macOS 27.2 SDK
     (`IOHIDLib.h:113`), while `IOHIDSetParameter` is marked deprecated since 10.12 (`:105`).
   - **Who reads it:** the keyboard filter stores the value (`IOHIDKeyboardFilter.mm:988-995`) and
     remaps with `if (FnKeyDown == !!_fnKeyMode) key = FnFunctionUsageMap[key]` (`:1680`). Mode 0
     (measured here) turns the F-keys into the media usages of the measured `FnFunctionUsageMap`
     when fn is **not** held. Mode 1 makes them F1–F12 without fn and media keys with fn.
   - **Doing it per app:** set 1 when a sopen window gains focus (winit
     `WindowEvent::Focused(true)`) and restore the saved value on `Focused(false)`, on quit, before
     sleep and on session switch. Fluor restores on `willSleepNotification` and
     `sessionDidResignActiveNotification`, and on quit (`BehaviorController.swift:60-63, 80-92, 165-178`).
   - **The hop in between, partly read.** `IOHIDSystem::setParamProperties` pushes the parameters
     to every event service through `setSystemProperties`. But `createFilteredParamPropertiesForService`
     skips any key the service already has in its own registry: "properties generated via the global
     IOHIDSystem::setParamProperties are defaults" (`IOHIDSystem.cpp:3809-3846`). The keyboard
     services here do carry their own `HIDFKeyMode` (236 registry entries, §1), so a global set may
     not reach them. That would explain Fluor's mixed reports `[추정]`. The last step, from the
     kernel service to hidd's user-space `IOHIDKeyboardFilter`, is unread.
   - **A second candidate setter:** the public `IOHIDEventSystemClientSetProperty(client,
     "HIDFKeyMode", …)` (`IOHIDEventSystemClient.h:61-75`). Whether the keyboard filter honours that
     route is not shown. `hidutil` returns `(null)` for this key at both levels while the registry
     says 0 (§1).
2. **Privileges:**
   - **No root or entitlement in the source path.** `IOHIDSystem::newUserClientGated` hands out a
     param connect with no entitlement check (`IOHIDSystem.cpp:2923-2928`), and
     `IOHIDParamUserClient::setProperties` strips only `kIOHIDUseKeyswitchKey` for non-admins
     (`IOHIDUserClient.cpp:357-367`).
   - **No Accessibility needed for the switch itself.** Fluor asks for Accessibility only for its
     "switch on fn press" feature (`AppDelegate.swift:43-47`, `adaptToAccessibilityTrust`). Its
     entitlements file holds only `disable-library-validation`; it is not sandboxed.
   - **Why that matters for sopen:** sopen is a bare Homebrew binary, not a `.app` (spec §9). A
     TCC-gated approach would attach the grant to whichever terminal launched it (`[추정]`), and to
     nothing at all for a `--target` run started over ssh. The IOKit route needs no grant.
   - **Commander One** offers the same result ("Ignore system settings for F1-F12") but **requires
     Accessibility**. That points to an event-tap mechanism instead (`[추정]`). FunctionFlip, for
     example, uses a `CGEventTapCreate(kCGSessionEventTap, …, NX_SYSDEFINED | keyDown/Up)`
     (`FFHelperApp.m:196-197`).
   - **Effect on 26.7.1 is unverified.** Fluor's reports are mixed: #92 has a 15.5 user with "the
     mode doesn't actually change" and a 14.7.6 user for whom it works; #99 is a Tahoe-beta launch
     failure.
3. **Side effects and safe restore:**
   - **Global while focused.** Every app sees F-keys while sopen is in front; brightness and volume
     then need fn.
   - **Only some keyboards.** It affects only keyboards whose Fn is done by Apple's filter. Keyboards
     with firmware Fn (Keychron, Logitech) are not affected (Fluor #93).
   - **Restore points:** focus loss, window close, quit, `NSWorkspace.willSleep`, session resign.
   - **Crash or SIGKILL.** Nothing restores the value, and the Mac stays in F-key mode. Mitigation:
     write the original value to a state file before the first change, and spawn a small watchdog
     child that blocks on a pipe from sopen. When the pipe hits EOF (sopen died), the child writes the
     value back. Then restore again at the next sopen start if the state file says so.
   - **Don't clobber the user's own change.** Re-read the value at every focus gain, so a change made
     in System Settings while sopen was in front is not overwritten.
   - **Known gap (Fluor #97):** Mission Control and similar overlays may leave the app "active", so
     F-key mode stays on over them.
   - **Persistence:** IOHIDSystem saves its parameters (`IOHIDSystem.cpp:3522`). Whether a stuck value
     survives a reboot is unverified; login normally re-applies the user preference `[추정]`.
4. **What sopen gets without it** (mode 0, measured):
   - **Plain F1–F12:** brightness, Mission Control, Spotlight, dictation, DND, media and volume
     events, all handled by the system. winit 0.30.13's macOS backend has no handler for
     `NSSystemDefined` media events (no match in `platform_impl/macos`), so **egui sees nothing**.
   - **With fn held:** a normal F-key `keyDown`. winit maps virtual keycode 0x60 to `KeyCode::F5`
     and then `NamedKey::F5` (`platform_impl/macos/event.rs:578, 201`), and egui gets `Key::F5`.
     egui's `matches_logically` looks only at alt, shift and cmd/ctrl (`modifiers.rs:211-220`;
     winit's macOS modifier state has no fn, `event.rs:312-329`), so
     `consume_key(Modifiers::NONE, Key::F5)` matches. So fn+F5 should already work today
     `[from source; not pressed]`.
   - **Global F-keys:** F11 / fn+F11 is "Show the desktop" and Control-F2 focuses the menu bar
     (Apple 102650), so slot 11 and up stay unused.
5. **Fallback that works everywhere:** the clickable row plus ⌘1…⌘0 (§3.2), or the owner's
   global setting "Use F1, F2, etc. keys as standard function keys" (System Settings › Keyboard ›
   Keyboard Shortcuts › Function Keys, Apple 102439).

**Recommended for macOS:**
- Ship the fallback, the clickable row and ⌘digits, always.
- Add the `HIDFKeyMode` focus switch behind a config key (`fkeys = "function"`, on by default
  once a receipt shows it works on 26.7). It must have the watchdog restore.
- **Receipts to get first, both run by the owner** (each is reversible, but each changes a global
  setting on the owner's Mac):
  - **(a) Zero code.** Flip System Settings' "Use F1, F2, etc. keys as standard function keys" once,
    then re-read `defaults read -g com.apple.keyboard.fnState`, `ioreg … HIDFKeyMode` and
    `hidutil property --get HIDFKeyMode`, and flip it back. That settles which readback is real and
    what `fnState` means (Tension 4). It does **not** prove that an app can set the value.
  - **(b) One-off build.** Set mode 1 the Fluor way, read `ioreg` back, check that a plain F5
    reaches egui as `Key::F5`, and restore. Until (b) passes on 26.7, the macOS switch is
    **unproven**.

### 4.2 Linux (KDE Plasma 6 Wayland on the ROG Flow Z13; measured kernel 7.2.6-cachyos)

1. **Can an app switch it only while focused?** Not as an unprivileged app. Every mechanism found
   is global, root-only and tied to particular hardware:
   - **asus-wmi.** Fn-lock lives in WMI device `ASUS_WMI_DEVID_FNLOCK` (0x00100023). It exists only
     if the BIOS reports it present and not disabled (`asus_wmi_has_fnlock_key`). The kernel itself
     toggles it on notify 0x4e (Fn+Esc) and starts it from `fnlock_default` (`module_param(…, 0444)`:
     read-only at runtime; measured `Y`) (`drivers/platform/x86/asus-wmi.c` v7.2:54-55, 4574-4584,
     5222).
     - **No sysfs attribute.** asus-wmi exposes no control for it, and the asus-wmi ABI document has
       no fn entry. **The brief's `/sys/devices/platform/asus-nb-wmi/fn_lock` is not an asus-wmi
       ABI.** `fn_lock` is ideapad-laptop's (`/sys/bus/platform/devices/VPC2004:*/fn_lock`,
       `Documentation/ABI/testing/sysfs-platform-ideapad-laptop:30`). It does not exist here (§1).
   - **hid-asus.** It does Fn-lock only for `QUIRK_HID_FN_LOCK` devices: in v7.2 only
     `NKEY_KEYBOARD2 0x19b6` (lines 1495-1496; added for the ProArt P16 in commit f631011e36b8). It
     toggles on `KEY_FN_ESC` by sending feature report `{FEATURE_KBD_REPORT_ID, 0xd0, 0x4e, on}`
     (master `hid-asus.c:691-697`) and turns it ON at probe. The Z13's folio `0x1a30` and lightbar
     `0x18c6` have no such quirk (v7.2 lines 1498, 1533; also master). There is no sysfs control
     either. An app could send the same report through `/dev/hidrawN`, but only with write access
     to hidraw (root or a udev rule), and only if the folio firmware accepts that report `[추정;
     untested; the folio was not attached]`.
   - **hid_apple.** `fnmode` (0 off, 1 fkeyslast, 2 fkeysfirst, 3 auto, 4 disabled;
     `module_param(fnmode, uint, 0644)`, `hid-apple.c:57-60`) is root-writable and global, for Apple
     keyboards on the hid-apple driver only. Not loaded here.
   - **The compositor.** An unprivileged Wayland client cannot remap keys. KWin's
     `GlobalShortcutFilter` consumes bound keys before the client sees them, unless the focused
     surface holds a keyboard-shortcuts inhibitor (`kwin/src/input.cpp` ~1016-1090,
     `isKeyboardShortcutsInhibited`). winit 0.30.13 has no support for that protocol (no match in
     its source). With an inhibitor, every compositor shortcut (Alt+Tab, Meta) would stop while
     sopen is focused.
   - **Root daemons** with per-app remapping (e.g. keyd plus its application mapper) are a system
     setup on the user's side, not sopen's `[추정: not checked today]`.
2. **Privileges:** root (sysfs, module parameters), hidraw write access, or uinput or input-group
   access for remap daemons.
3. **Side effects:** global for every app on that keyboard, and lost on a crash; the same restore
   pattern as macOS would apply.
4. **What sopen gets without it:**
   - **Keyboards that send F-codes.** All measured external keyboards (Lofree Flow2, AU05) and the
     AT keyboard advertise KEY_F1–F12 (§1). A plain F-key → winit `KeyCode::F1…` → egui `Key::F1…`.
     It works today, with no fn needed in F-key mode. Whether each keyboard's firmware Fn layer
     defaults to media is set inside the keyboard and was not measured.
   - **A media code instead** (KEY_MUTE…): xkb keysym `XF86AudioMute` → winit
     `NamedKey::AudioVolumeMute` (`common/xkb/keymap.rs:820`, raw 113 → `KeyCode::AudioVolumeMute`
     `:155`) → egui `Key::AudioVolumeMute` (`egui-winit lib.rs:1648`). But KDE binds Volume
     Mute/Up/Down and brightness globally (§1), so KWin consumes them first (input.cpp filter above).
     Brightness has no egui key at all (`key_from_named_key` falls to `_ => None`, `lib.rs:1670`).
   - **The Z13 folio's own default** could not be read: it was not attached (§1).
5. **Fallback:** the clickable row plus Ctrl+1…Ctrl+0 (free on the measured KDE). On the Z13
   folio, the keyboard's own Fn+Esc lock, if its firmware does it; the kernel does not do it for
   0x1a30.

**Recommended for Linux:**
- Fallback only. sopen should not touch sysfs, hidraw or the compositor.
- Note in the README that external keyboards send F-keys directly, and that the folio relies on
  its own Fn lock.

### 4.3 Windows (research only; Windows is out of sopen's scope, spec §9)

1. **Can an app switch it only while focused?** No OS API for Fn behaviour was found in the
   Microsoft pages read ("Keyboard shortcuts in Windows", the `WM_APPCOMMAND` reference). Fn is
   handled in the keyboard firmware or EC, and set through BIOS or vendor tools.
   - **ASUS's documented control:** Fn+Esc (models with an LED on the Fn key) or the MyASUS
     setting "Hotkeys" (default) vs "F1-F12". It needs ASUS System Control Interface V2+ (ASUS FAQ
     1038312).
   - **Programmatic, per vendor:** for ASUS it is the **same WMI device as Linux, `0x00100023`**,
     reached through `\\.\ATKACPI` (`DeviceIoControl`). G-Helper does this (`app/AsusACPI.cs:32,
     153`) and runs `asInvoker`, i.e. without elevation (`app.manifest:7`). So a per-app flip would
     be possible on ASUS only, and each vendor would need its own path (Lenovo, Dell and HP have
     their own BIOS or utility switches `[추정: not fetched]`).
2. **Privileges:** none for G-Helper's ASUS path (asInvoker), as far as the manifest shows; it
   varies for other vendors.
3. **Side effects:** global; the same restore needs as above.
4. **What the app gets without it:**
   - **Media codes** arrive as `WM_KEYDOWN VK_VOLUME_MUTE…` (winit maps `VK_F1…VK_F24` to
     `NamedKey::F1…`; media VKs to the audio keys: `platform_impl/windows/keyboard_layout.rs:670,
     829-852`) **and** as `WM_APPCOMMAND` sent to the focused window. If the window does not handle
     it, `DefWindowProc` passes it to the parent and finally to the shell hook `HSHELL_APPCOMMAND`
     (Microsoft `WM_APPCOMMAND` remarks).
   - **Brightness and vendor keys** usually go through vendor WMI and never reach the app `[추정]`.
5. **Fallback:** the same clickable row plus Ctrl+digits.

## 5. Tensions

1. **Scope.**
   - **What the repo says today:** AGENTS.md says the file browser "adds viewing, not file
     operations: no copy, move, delete, rename or new folder". Spec §9 lists "file operations in the
     browser", "a directory tree" and "Windows" as out of scope.
   - **What the owner asked on 2026-11-20:** F2 rename, F5 copy, F6 move, and a tree on F10. The
     owner's message supersedes the old rule. AGENTS.md "Rules" plus spec §1, §3a and §9 need
     amending before code.
   - **Still open:** F7 MkDir and F8 Delete were not asked for. They are proposed because the
     convention is unanimous, but they stay **owner calls**. Windows stays out of scope.
2. **Remote copy primitive.** AGENTS.md: "Transport is the system ssh binary … file operations are
   SFTP over that session". `ssh host cp -a` would be faster and keep every attribute, but it runs a
   remote command. `copy-data` keeps it SFTP-only (§3.5).
3. **Crate default vs never-overwrite.** `openssh-sftp-client` `Fs::rename` prefers
   `posix-rename@openssh.com`, which replaces the target. OpenSSH's plain `SSH_FXP_RENAME` refuses
   to replace it. Servers other than OpenSSH may implement plain rename differently, and none was
   checked.
4. **macOS readback.** The registry `HIDFKeyMode`=0 (236 entries, IOHIDSystem's `HIDParameters` among them) disagrees with
   `hidutil property --get HIDFKeyMode` = `(null)`. Two candidate setters
   (`IOHIDSetCFTypeParameter` on IOHIDSystem vs the event-system client property); which one the
   keyboard filter honours on 26.7 is not shown.
5. **Fluor on newer macOS.** #92: a Sonoma 14.7.6 user says it works, a 15.5 user says it doesn't.
   #99: it doesn't launch on the Tahoe beta. #101 (closed 2026-04) modernises the build. The
   mechanism is sound per source, but its effect on 26.7.1 is unproven.
6. **PC Shell 6 F-keys.**
   - The printed default bar on p.67 reads `1Help 2Qview 3Exit 4Unsel 5Copy 6Disply 7Locate 8Zoom
     9Select 10Menu`, read from the page image.
   - The "Define Function Keys" dialog on the same page shows F2 Help Index, F4 Reset Selected
     Files, F5 Disk Info, F6 File Display Options, F7 Active List Switch, F8 Delete File and F9 File
     Select Filter. That is an example mid-edit (F8 being reassigned to Delete), and it matches the
     **5.5** defaults (5.5 manual: F2 Help Index, F5 Disk Info, F6 File Display Options, F7 Active
     List Switch, F8 File List Filter, F9 File Select Filter).
   - The table uses the printed bar for 6 and the 5.5 manual for 5.5.
7. **F10.** The convention is Quit or Menu everywhere surveyed, while the owner asked for the tree.
   Resolved in §3.1 for the owner, with NC/TC's Alt+F10 kept as an alias.
8. **Commander One vs Fluor.** The same feature appears with and without an Accessibility grant,
   which suggests two different mechanisms; Commander One's is undocumented.
9. **The brief's Linux path** `/sys/devices/platform/asus-nb-wmi/fn_lock` is ideapad-laptop's ABI
   name, not asus-wmi's. Measured: absent.
10. **Kernel version.** Source was read at tag v7.2 (and master 7.4-rc3); the machine runs
    7.2.6-cachyos, and CachyOS patches could differ.
11. **Gray +/−/\*** can't be told from the main-row keys in egui 0.37 (key location dropped), so
    using them takes `+ − *` out of the typed-letter jump.

## 6. Decisions taken (reversible) and why

- **F10 = Tree, Alt+F10 alias.** The owner's word. F10's conventional Quit/Menu don't exist in
  sopen, and the tree belongs on the visible row; Alt+F10 is NC/TC's tree key and free on KDE.
- **F9 left empty.** Its conventions (Menu, Terminal) have no sopen action. Don't invent one.
- **F1 = key help.** Unanimous convention, and the alternates need a place to be discovered.
- **F7/F8 proposed but flagged as owner calls.** Unanimous convention, but outside the owner's
  message and the current scope rule.
- **Shift+F6 = rename alias.** TC, Far and DC; it keeps the NC "F6 renames" habit near F2.
- **Delete aliases: Del everywhere, ⌘⌫ on macOS only.** A MacBook's "delete" key is Backspace
  (already "up"); ⌘⌫ is Finder's Trash key, and Ctrl+Backspace means something else in Krusader.
- **Shift/Alt variants are checked before the plain keys.** egui's `matches_logically` ignores
  extra Shift and Alt.
- **Space (primary) + Ins select; + − \* group; Cmd/Ctrl+A all.** The most common set that works on
  keypad-less laptops.
- **Cmd/Ctrl+R re-read, Cmd/Ctrl+U swap, Cmd/Ctrl+←/→ same folder.** TC, mc, Far, DC and Krusader
  conventions; egui's COMMAND gives ⌘ on macOS, which dodges ⌃←/→ Spaces.
- **No Ctrl+F3…F6 sort, no Alt+F1/F2/F7.** KDE grabs Ctrl+F1–F4 and Alt+F1–F4 (measured), and sopen
  has no sort, drive or find feature.
- **Cmd/Ctrl+digits as F-key aliases.** They match the row's printed digits; mc's precedent is
  Esc+digit, which sopen can't use (Esc is taken); Ctrl+digits are free on the measured KDE.
- **Clickable row.** PC Shell's was; it is the one fallback that needs no key at all.
- **macOS focus switch only behind a receipt and with a crash-safe restore.** A global side effect
  on the owner's Mac must be undoable even if sopen dies.
- **No Linux or Windows Fn switching in sopen.** Root-only or vendor-only; out of proportion and
  out of scope.

## 7. Sources

Norton family and DOS:
- Norton Commander 4.0 User's Guide (Symantec 1993): https://archive.org/details/norton-commander-ver-4 (text: `Norton Commander Ver 4_djvu.txt`)
- PC Tools Deluxe 6 Data Recovery – DOS Shell (Central Point 1990): https://archive.org/details/central-point-pc-tools-deluxe-version-6-data-recovery-dos-shell. Page images read: leaves 76 (p.66), 77 (p.67), 83 (p.73).
- PC Tools v5.5 manuals: https://archive.org/details/pc-tools-v-5.5 (`…Data Recovery and DOS Utilities (1989)_djvu.txt`)
- Total Commander 11.58 `KEYBOARD.TXT`, from https://totalcommander.ch/115801/tcmd115801x64.exe (installer read with bsdtar, not run)
- Midnight Commander: https://github.com/MidnightCommander/mc/blob/master/misc/mc.default.keymap · `src/filemanager/filemanager.c:183,257` · `lib/tty/key.c:106,1849`
- Far Manager: https://github.com/FarGroup/FarManager/blob/master/far/FarEng.hlf.m4 (@FuncCmd ~630, @PanelCmd 350, @FastFind 541, @TreePanel 1166, @DeleteFile ~770)
- Double Commander: https://doublecmd.github.io/doc/en/shortcuts.html

KDE, Windows, Mac file managers:
- Krusader handbook: https://invent.kde.org/utilities/krusader/-/raw/master/doc/handbook/keyboard-commands.docbook
- Dolphin handbook: https://invent.kde.org/system/dolphin/-/raw/master/doc/index.docbook · `src/dolphinmainwindow.cpp:1928-2553`
- KDE standard shortcuts: https://invent.kde.org/frameworks/kconfig/-/raw/master/src/gui/kstandardshortcut.cpp (Help F1 :394; RenameFile F2 :618; CreateFolder Ctrl+Shift+N :640; OpenMainMenu F10 :650; Reload F5 :269)
- Windows shortcuts: https://support.microsoft.com/en-us/windows/keyboard-shortcuts-in-windows-dcc61a57-8ff0-cffe-9796-cb9706c75eec
- WM_APPCOMMAND: https://learn.microsoft.com/en-us/windows/win32/inputdev/wm-appcommand
- Mac shortcuts: https://support.apple.com/en-us/102650 · rename: https://support.apple.com/guide/mac-help/rename-files-folders-and-disks-mchlp1144/mac · F-key setting: https://support.apple.com/en-us/102439
- Commander One: https://help.electronic.us/support/solutions/articles/44001751227-preferences-commander-one · https://help.electronic.us/support/solutions/articles/44001760744-f1-f12-keys-don-t-work-without-pressing-fn-or-stopped-working-at-all

macOS Fn mechanism:
- Fluor (MIT): https://github.com/Pyroh/Fluor, `Fluor/Misc/FKeyManager.swift`, `Controllers/BehaviorController.swift`, `Misc/AppDelegate.swift`, `Fluor.entitlements`; issues #92, #93, #97, #99, #101
- FunctionFlip: https://github.com/kevingessner/FunctionFlip, `FFHelperApp.m`
- IOHIDFamily (Apple OSS, tag IOHIDFamily-2238.100.59): https://github.com/apple-oss-distributions/IOHIDFamily, `IOHIDSystem/IOKit/hidsystem/IOHIDParameter.h:121`, `IOHIDShared.h:232`, `IOHIDSystem/IOHIDSystem.cpp:2923,3522`, `IOHIDSystem/IOHIDUserClient.cpp:357`, `IOHIDEventSystemPlugIns/IOHIDKeyboardFilter.mm:988,1680`
- macOS 27.2 SDK headers on this Mac: `IOKit/hidsystem/IOHIDLib.h:105,113`, `IOHIDEventSystemClient.h:61-75`, `IOKit/hid/IOHIDUsageTables.h`

Linux kernel and desktop:
- https://github.com/torvalds/linux/blob/v7.2/drivers/hid/hid-asus.c · `…/v7.2/drivers/platform/x86/asus-wmi.c` · `…/v7.2/drivers/hid/hid-ids.h` · master `drivers/hid/hid-apple.c:57-60` · `Documentation/ABI/testing/sysfs-platform-asus-wmi` · `…/sysfs-platform-ideapad-laptop:30`
- Kernel commits: ce357fd34f94 (asus-wmi fnlock_default), 487579bab867 (asus-wmi fn-lock), f631011e36b8 (hid-asus fn lock, ProArt P16)
- KWin: https://invent.kde.org/plasma/kwin/-/raw/master/src/input.cpp (`GlobalShortcutFilter` ~1016)
- G-Helper: https://github.com/seerge/g-helper, `app/AsusACPI.cs`, `app/app.manifest`
- ASUS FAQ: https://www.asus.com/support/faq/1038312/

sopen's own stack:
- OpenSSH: https://github.com/openssh/openssh-portable/blob/master/PROTOCOL (§4.1, 4.3, 4.5, 4.10) · `sftp-server.c:1179-1316`, `:154-164`, `:717-725`
- Local crates (`~/.cargo/registry`): egui-winit 0.37.0 `src/lib.rs`, winit 0.30.13 `src/platform_impl/{macos,linux/common/xkb,windows}`, openssh-sftp-client 0.15.12 `src/fs/mod.rs`, `src/file/mod.rs`
- sopen: `AGENTS.md`, `.prd/01-spec.md` §3a/§9, `src/gui/browser.rs:603-701`, `src/gui/chrome.rs:683-742`

