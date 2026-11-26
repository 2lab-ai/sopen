# sopen docs

The reference behind the [README](../README.md): every key, flag, setting and caveat.

| Page | What it covers |
| --- | --- |
| [Requirements](requirements.md) | what the machine that shows the file needs (ffmpeg, git, the voice/music model, an editor, Linux libraries and a desktop session), and what the remote needs: only sshd |
| [The file browser](file-browser.md) | folders: the two panels, preview and dual mode, the git panel, its actions and history editing, the directory tree, rename / copy / move / make folder / delete, every key, the text viewer |
| [In the terminal: `--tui`](tui.md) | the file browser in this terminal, F12 into a window and back, keys, mouse, copy, colours |
| [Images](images.md) | crop, pixelize, cut / copy / paste, Save, Save copy and Save As, large, remote and animated images |
| [Video and audio](video-audio.md) | the timeline: cut / copy / paste, Mono → Stereo, crop, Save, Save As and Export, what a render keeps |
| [Binary files](binary.md) | the binary-file dialog and "Save to…", the hex editor: keys, Save on the server, files above 256 MiB |
| [`--target`](target.md) | open on another machine's screen, the config file, `fkeys` on macOS, exit codes |
| [Web addresses](urls.md) | `sopen URL`, a loopback address through a tunnel to `<host>`, what is printed of an address |
| [Command line](cli.md) | `--app`, `sopen cut`, `sopen stems` |
| [Safety](safety.md) | how a write is checked, backups, kept edits, conflicts, exports |

Spec and scope: [`.prd/01-spec.md`](../.prd/01-spec.md). Changing the code: [`AGENTS.md`](../AGENTS.md).
