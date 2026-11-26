# Requirements

What the machine that shows the file needs, then what the remote needs. Install: [README](../README.md#install).

## This machine

- Media needs `ffmpeg` and `ffprobe` (the formula installs them).
- The git panel needs git 2.20 or newer where the repository is (`SOPEN_GIT` names this machine's;
  on macOS `/usr/bin/git` counts only when the developer tools are installed). Without it the
  folder simply shows its list.
- `sopen stems` needs its model, which the formula installs too (67 MB), and memory: about
  6.5 GB at its peak, whatever the length of the track.
- Text (and F4 in the file browser): local files open in VS Code (`code`) when it is installed;
  remote ones open in VS Code when its Remote - SSH extension is installed (sopen does not install
  it), else in Zed. Without VS Code, sopen uses Zed. On Linux under KDE Plasma, KWin would only
  flag the editor's window (often on another desktop), so sopen brings it to your current desktop
  and screen and activates it; `SOPEN_RAISE=0` turns that off.
- Linux: the ALSA runtime library (libasound2 / alsa-lib) and the C++ runtime (libstdc++, for
  `sopen stems`) — both present on desktop Linux; sopen runs
  on the machine whose screen shows the file (the one you sit at, or the `--target`), not on the
  remote. The file browser, the image and media windows, the binary-file dialog and the hex window
  need a desktop session (X11 or Wayland); without one a folder opens in the editor and a binary,
  image or media file is fetched and its path printed — or, with `--tui`, the file browser runs in
  the terminal.

## What the remote needs

Only sshd with its SFTP subsystem (the default). sopen installs and runs nothing there. When you open
a text file (or press F4 in the file browser), VS Code Remote - SSH or Zed installs its own remote server on first use — that is the editor's behaviour,
not sopen's. Folders are listed over SFTP. What xtools shows itself — text in the file browser, images,
video, audio, the hex editor — is read over SFTP a piece at a time and handled on this machine; video
and audio are decoded here by ffmpeg from that stream. The git panel runs the remote's own `git`, when
it has one, over the same ssh connection — nothing is uploaded. (A `--target` machine is
different: it shows the file, so it needs sopen installed — see [`--target`](target.md).)
