# Open on another machine's screen: `--target`

```sh
sopen myserver:/tmp/a.html                    # opens here, on the machine you typed it on
sopen --target <host> myserver:/tmp/a.html    # opens on <host>'s screen (sopen runs there, over ssh)
sopen --target <host> <host>:/tmp/a.html      # a file on <host> itself opens there as a local file
sopen --target <host> ./notes.txt             # a local file: <host> fetches it from this machine
sopen config target <host>                    # from now on `sopen SOURCE` opens on <host>
sopen --here myserver:/tmp/a.html             # …but this one here
sopen --gui --target <host> <host>:/srv/app   # in a window on <host>, never its fallback without one
sopen config                                  # where the config file is, the target (and fkeys, if set)
sopen config target --unset                   # back to this machine
sopen config fkeys system                     # macOS: sopen never touches what the top row sends
sopen config fkeys function                   # …or F1–F12 without fn while xtools is in front (the default)
```

The config file is `~/.config/sopen/config.toml` (`$XDG_CONFIG_HOME/sopen/config.toml` when that is
set, `$SOPEN_CONFIG` overrides both) on macOS and Linux alike:

```toml
target = "<host>"
fkeys = "function"   # or "system"; without the key, "function"
```

`fkeys` is for macOS: with `"function"` (the default), while an xtools window is in front sopen
switches the Mac's top row to F1–F12 without fn — so F5 should copy — and brightness and volume
need fn; once no xtools window is in front (or the Mac sleeps, or you switch user), the Mac's own
setting is back, even if sopen crashed. The switch is measured to reach the keyboard driver;
that a plain F5 then reaches xtools is not yet checked on a real keyboard (`.prd/04-file-keys.md`
§6.2, K-F3). The setting is global,
so other apps see F-keys while xtools is in front. `"system"` leaves the Mac's setting alone; the
F-key row's slots and Cmd/Ctrl+1…0 (F1–F10) work either way. Linux has no such switch: a laptop's
own Fn lock (often Fn+Esc) is the way there.

`--target` or `--here` (not both) decides one run; without them `SOPEN_TARGET=<host>` in the
environment comes first, then the config. `<host>` is `[user@]host` — an ssh alias or a host
name that `ssh <host>` reaches; a port or an IPv6 address goes into `~/.ssh/config` under an
alias. It needs sopen installed; sopen there gets `--here`, so it never sends the file on again.
A local file (or a `localhost:` one) is fetched by `<host>` from this machine by its short host
name, so that name must work for `ssh` on `<host>`.

`--gui` asks for the window: with a target, sopen there gets it too (never with `--app` or for an
address), and with no desktop session there its sopen says `no display here — …` and
exits 1 instead of today's fallbacks. Without `--gui` a forwarded run is as before: `<host>` picks
by what it finds. (When the terminal becomes xtools' default, sopen will add `--gui` itself for
what opens in a window.) A `<host>` whose sopen is older than `--gui` refuses it (its own usage
error, before it does anything); sopen then runs the same thing there once more without it, and
says `sopen on <host> is older and does not know --gui; running it there again without it`.
`--gui` without a target opens here as before, and on a machine without a display (Linux without
`DISPLAY` or `WAYLAND_DISPLAY`) it exits 1 with `no display here — sopen config target <host>, or
--target, opens the window on another machine`.

The window opens on `<host>`'s desktop session (on Linux sopen finds it through
`systemctl --user`). On macOS an app started over ssh cannot take focus: the xtools window
floats above the other windows until you click, type or scroll in it (or it gets focus), but the editor
(for text) opens behind the active app; under KDE Plasma it is brought to the screen you are on. From a terminal, Ctrl+C reaches `<host>` and closes the window
there; a sopen stopped without a terminal leaves the window on `<host>` open.

The exit code is `<host>`'s; `could not reach <host>` means ssh did not get there, `sopen is not
installed on <host>` (exit 127) means it has no sopen, `sopen on <host> ended with status
N (signal n?)` means it most likely was interrupted there, and exit 2 with `<host>`'s own usage
line (other than the one for `--gui`) means its sopen is too old for the run, or the run is wrong.

From `sopen --tui` with a target, F12 moves the file browser into a window on `<host>`'s screen
— sopen there gets `--here --gui` and the terminal's state (folders, bars, selections, an open
dialog, the viewer) on its input — and closing that window, or F12 there, brings the terminal back
with the window's state. ssh is told never to ask anything (the terminal is xtools'), so `<host>`
must be reachable without a question first: run `ssh <host>` once in a shell otherwise. The
receipt hooks that script a run (`SOPEN_STATE`, `SOPEN_SCREENSHOT`, `SOPEN_AUTOSAVE`, `SOPEN_LIST`,
…) stay here for this switch. See [In the terminal](tui.md).

A web address with `--target` opens in `<host>`'s browser, a loopback one through a tunnel: see
[Web addresses](urls.md).
