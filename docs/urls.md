# Web addresses: `sopen URL`

```sh
sopen http://127.0.0.1:1234/                  # opens in an xtools window here; sopen ends when it is closed
sopen --browser http://127.0.0.1:1234/        # opens in this machine's default browser, and sopen ends
sopen --target <host> https://example.com/    # opens in an xtools window on <host>
sopen --target <host> http://127.0.0.1:1234/  # a page served here, in xtools on <host> through an ssh tunnel
```

An `http://` or `https://` address (the scheme in any case) is never read as `host:path`; it opens
in an xtools window on the machine that shows things: the page with its scripts on (`F2` turns them
off and on, reloading), kept to the address's origin — a link clicked elsewhere is handed to the
default browser (only an `http` or `https` one), any other navigation away is refused with a line —
and sopen returns when the window is closed (`Esc` — a second one while the page has the keyboard —
`F3`, `Cmd/Ctrl+W`). `Cmd/Ctrl+R` reloads, or tries again when the page did not load (a dev server
not up yet); an untrusted certificate is never accepted. With `--browser` the address goes to the
default browser instead — `open` on macOS, `xdg-open` on Linux — exactly as written, and sopen ends;
so it does on a machine with no display, and on one with no web engine after one line saying so
(Linux, until sopen's Linux web engine lands).

With a target, an address that would mean `<host>` itself there — `localhost`, `127.x.y.z`, `[::1]`
(and the other loopback names `--target` treats as this machine) — is served from this machine
instead: sopen opens an ssh connection of its own to `<host>` (never its shared one) with `-R
127.0.0.1:<port>:<address's host>:<port>` (`[::1]` for an IPv6 address), so the port listens on
`<host>`'s loopback only, and `<host>`'s sopen opens the address in its window and waits. If
`<host>`'s sshd has `GatewayPorts yes`, which binds a forward to every interface whatever is asked,
`<host>`'s sopen sees the port open to the network and refuses before anything opens — as it does
whenever it cannot see the tunnel's listener on loopback alone. sopen prints `<address> is open on
<host> through a tunnel — closing its window there, or Ctrl-C here, closes it` (`… — Ctrl-C closes
it` when `<host>` opened its default browser: `--browser`, no display or engine there, an older
sopen) and stays; Ctrl-C (or SIGTERM) closes the tunnel and exits 0, and so does closing the window
on `<host>` (`<address>: its window on <host> was closed; the tunnel is closed`). When sopen ends
any other way — killed outright included — its pipe to ssh closes: `<host>`'s sopen closes its
window and ends, and so does the ssh under sopen's guard, even while a browser still holds a
connection, so the page no longer opens. When `<host>`'s sopen ends first (killed there), sopen
closes the tunnel too and exits 1. When the address's port cannot be forwarded on `<host>` — taken
there, or forwarding refused (ssh says the same for both), as for a port below 1024 for a user —
sopen tries two random ports from 49152–65535 and says which one it took: the address then opens
with that port, a different origin; if those fail too, `<host>` refused the tunnel
(`AllowTcpForwarding`?). Any other address (`https://example.com/`) is handed to `<host>`'s sopen as
it is, with no tunnel, and opens there in a window (with `--browser`, its default browser). `--app`
and `--hex` open files, so with an address they are a usage error (exit 2).

What sopen prints of an address — results, errors, `SOPEN_LOG=debug` lines — never has its password,
query or fragment: `http://me:***@host:3000/path?…#…` — the window's title, bar and lines too. Only
the page view or the browser gets it whole. (It is in the ssh command line, so `ps` on either
machine can show it.) A host a browser reads as another address — `127.1`, `0x7f.0.0.1`,
`%`-escapes, full-width digits, `[::ffff:127.0.0.1]` — is refused: write it plainly.

The tunnel needs only what `ssh -R` needs: `<host>`'s sshd allowing TCP forwarding (the default). A
`LocalForward` or `RemoteForward` your `~/.ssh/config` sets for `<host>` is requested by this
connection too and, if it fails, ends it. So far the tunnel is tested against a stand-in `ssh`, not
yet a real one.
