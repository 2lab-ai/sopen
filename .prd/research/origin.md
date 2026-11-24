# sopen on B reaching the machine the owner came from (OR-R, 2026-11-24)

Owner message 24 is quoted verbatim in [`../origin/ssot.md`](../origin/ssot.md); in short: the owner
sits at A and can `ssh B`; B must not be able to connect to A; from B's shell
`sopen --target origin origin:~/A.mp3` should still open on A's screen with A's sopen and read a file
of A, while the owner just types `ssh B`.

Machines in every experiment: **A** = a Mac (macOS 26.7.1, `/usr/bin/ssh`
OpenSSH_10.3p1 LibreSSL 3.3.6). **B** = the Linux laptop (Linux 7.2.6, OpenSSH_10.5p1,
OpenSSL 3.6.4, login shell fish). Link: Tailscale, a direct overlay-network path; ping RTT
varied run to run (avg 12–46 ms, min ~9, max 155). OpenSSH source lines are upstream
openssh-portable `V_10_3_P1` (A's client) and `V_10_5_P1` (B's server). Apple's build may differ;
every behaviour below was measured on the real binaries, and the source is the mechanism. sopen lines
are at `6ea3638` (in `main`; only `main.rs` changed since). **[추정]** marks what was not measured.

## 0. Summary

**Recommended route: sopen's own agent behind `ForwardAgent <path>`** (route a2, §2.2).
`sopen allow B` adds one `Host B` stanza on A: `ForwardAgent ~/.sopen/origin/%n.sock`. After that
the owner's plain `ssh B` opens a per-session socket on B. sshd creates it in `~/.ssh/agent/`, puts it
in `SSH_AUTH_SOCK`, makes it reachable only by the owner's uid (and root), and removes it at session
end. Every connection B makes to it reaches sopen's agent on A, never the owner's real ssh-agent.
**Nothing is installed or configured on B**, sshd is not changed, and B makes no network connection
to A.

What the prototype proved, A=Mac, B=Linux:
- **It works end to end.** "hello" returns A's hostname. A 64 KiB range of a file on A reads back
  with the right SHA-256. A path outside the allowed root is refused.
- **B makes no network connection to A.** Run under `unshare -rn` (only `lo`, down; a TCP connect to
  A gives `Network is unreachable`), the client still works.
- **Sessions coexist and clean up.** Two concurrent sessions get two distinct sockets. A shell with
  no `SSH_AUTH_SOCK` (the tmux case) finds the live one by scanning `~/.ssh/agent`. After dozens of
  sessions, including a `kill -9` of A's ssh, `~/.ssh/agent` holds 0 entries.
- **A learns which host is calling, for free.** The first message on every forwarded connection is
  written by A's own ssh client (`session-bind@openssh.com`) and carries B's host key. The measured
  fingerprint was `SHA256:<B host key>`, the same as B's `/etc/ssh/ssh_host_ed25519_key.pub`.
- **B gets no key use.** `ssh-add -l` on B says "The agent has no identities", unlike
  `ForwardAgent yes`.

It costs three things, all measured:
- **Bandwidth per channel.** The forwarded agent channel has a 64 KiB window, set on B's side
  (session.c:209-211). One channel gave **1.1–2.0 MB/s** whatever the read size or depth. Eight
  striped connections gave **5.8–7.0 MB/s**, against 5.3–7.6 MB/s for one plain unix-socket forward
  at depth 8 on the same link.
- **A stuck agent freezes the owner's ssh session.** A's ssh client waits synchronously for the
  agent's reply on every new connection from B (clientloop.c:1855). A 3 s agent delay froze the
  owner's session output for 3.01 s. With the agent `SIGSTOP`ped for 8.02 s, the session froze
  6.26 s, from B's `ssh-add -l` until `SIGCONT`. Anything on B that touches `SSH_AUTH_SOCK` triggers this, B's own `ssh-add` included.
- **A needs an ssh-agent of its own.** A's ssh drops agent forwarding silently when `SSH_AUTH_SOCK`
  is unset on A, even with a path given (ssh.c `check_agent_present`). This session had none, so the
  first run got no forwarding at all.

**Killed:**
- **(a1) a fixed-path `RemoteForward` socket:**
  - a stale file blocks the next session's bind;
  - a second concurrent session gets no forward;
  - fixing either needs `StreamLocalBindUnlink yes` in B's sshd, or route (b).
- **(b) adding forwards to a ControlMaster on demand.** B cannot ask for a forward before a channel
  exists, so it only works A-initiated, and it requires the owner's `ssh B` to become a mux master.
- **(d) a port on A restricted to B.** It is a B→A connection, which the owner wants closed. A's pf
  needs root (`/dev/pf: Permission denied`), and A is behind NAT (`<addr>`), so only the
  overlay reaches it.

**Survives as an option:** (c) the terminal-stream channel through a pty proxy on A (`sopen ssh B`).
- It works at 2.0–4.3 MB/s.
- The owner types `sopen ssh B` instead of `ssh B`.
- Under tmux it needs the client to wrap frames and the owner's tmux to set `allow-passthrough on`.
  Measured: unwrapped frames never arrive, with passthrough off or on.

## 1. What sopen does today (the code path that breaks)

- `sopen --target HOST SOURCE`: `main.rs:525-543` → `forward::choose` → `forward::source_for`
  (forward.rs:203) → `forward::run` (forward.rs:430). That ssh's from here to HOST and runs HOST's
  sopen with `--here`.
- A file on this machine becomes `<short-hostname>:<abs>` (forward.rs:236-240, `this_machine`
  forward.rs:244). HOST then reads it over SFTP: `SftpRemote::connect` (remote/sftp.rs:290) runs
  ssh's `sftp` subsystem (remote/sftp.rs:184) **from HOST back to this machine**.
- From B with target A, that is B→A ssh twice, which is exactly what the owner forbids.
- `sopen URL` tunnels with its own `ssh -T -R` (forward.rs:463-500, url.rs:272). That is an A→B
  connection carrying a B-side listener, which is the right direction.

## 2. Routes, the experiment that could kill each, and what happened

Prototype: `proto/agent.py` on A (unix sockets; serves `HELLO` and byte ranges of files under
`files/` only, by realpath confinement) and `proto/client.py` on B (`/tmp/sopen-research-origin/`).
The test file is 64 MiB of random bytes. The 64 KiB block at 32 MiB has SHA-256 `8fbe0953…30c1e8`,
and every route reproduced it on B.

### 2.1 (a1) `RemoteForward <path on B> <socket on A>` inside the owner's ssh

| Experiment | Result |
|---|---|
| Bind | `srw------- <user>` (sshd applies `StreamLocalBindMask` 0177, channels.c:4091) |
| Hello / range / `../` refused | 52 ms first hello; SHA OK; `ERR outside the allowed root` |
| Session ends | **file stays**, connect → `ECONNREFUSED`. channels.c has no unlink of a forwarded socket; only the `unlink_first` flag at :4093 → misc.c:2001 |
| Next session, same path | `Warning: remote port forwarding failed for listen path …`; the shell runs without a forward. With `ExitOnForwardFailure=yes`: `Error: …` and the session ends |
| Two concurrent sessions | the second's bind fails. It reaches A through the first's socket until the first ends, then `ECONNREFUSED` |
| Relative listen path | resolves against `/`: `tmp/sopen-research-origin/rel.sock` was created at `/tmp/…`, because sshd-session does `chdir("/")` (sshd-session.c:1109). A must know B's absolute path |
| `%n %C %L %u` in the listen path | expanded **on A** (warning text: `n-B.C-c23f…b42.L-A.u-<user>.sock`); that 112-byte path exceeded Linux's 108-byte `sun_path` and failed. No token is per-session (ssh_config.5:2384-2396) |
| Discovery by env | `SetEnv SOPEN_ORIGIN=1` and `SendEnv LC_SOPEN_ORIGIN` both arrive **unset**; B's sshd accepts no env |

Verdict: **killed for natural use.** The owner's second terminal to B, or any reconnect after a
dropped link, silently loses origin until someone deletes the stale file on B. The fix,
`StreamLocalBindUnlink yes` in B's sshd, is the sshd change the scope prefers to avoid.

### 2.2 (a2) `ForwardAgent <path>`: sopen's agent in the slot of ssh-agent — survives

- The mechanism is documented: ForwardAgent "may be … an explicit path to an agent socket"
  (ssh_config.5:882-891). The path gets `~` and %-token expansion (ssh.c:1509-1515), and the
  measured config `ForwardAgent ./agt-%n.sock` became `./agt-the Linux laptop.sock`.
- A connection to the forwarded socket on B makes A's ssh connect to that path, never to
  `$SSH_AUTH_SOCK` (clientloop.c:1845-1849).

| Experiment | Result |
|---|---|
| Socket on B | `SSH_AUTH_SOCK=/home/<user>/.ssh/agent/s.26Im7cAUaV.sshd.<random>`, `srw-------` in `drwx------ ~/.ssh/agent`. This 10.x layout is misc-agent.c:178-198; older sshd used `/tmp/ssh-*/agent.*` **[추정]** |
| Key use from B | `ssh-add -l` → "The agent has no identities." (the agent answers 0 identities, FAILURE to everything else) |
| First message per connection | `session-bind@openssh.com` from A's ssh client (authfd.c:748-770, sent at clientloop.c:1855). It carries B's host key `SHA256:<B host key>` (= B's ed25519 host key), session id (32 B) and `forwarding=1`. A's own authentication binds with `forwarding=0`: 9 of those vs 44 forwarded |
| No fallback to the real agent | a stand-in "real" agent as `SSH_AUTH_SOCK`, with the forward target live, killed, or missing, saw only A's own auth (3 × `forwarding=0`, 3 × request-identities) and **0 forwarded connections**. Dead or missing target → B gets `Connection reset by peer`. A missing path also prints `Cannot forward agent socket path "…": No such file or directory` on A at every ssh (ssh.c:1516-1518) |
| **Precondition on A** | with `SSH_AUTH_SOCK` unset (this session), or `IdentityAgent none` (which unsets it, ssh.c:1755-1758), **no forwarding at all, and nothing printed**: `check_agent_present()` (ssh.c:2149, called at :2188) tests `getenv("SSH_AUTH_SOCK")` (authfd.c:128), not the path. (Corrected 2026-11-25 by OR-SPEC's review: it also connects to that socket, authfd.c:132 → :87-113, so a set but dead `SSH_AUTH_SOCK` drops forwarding too.) On the Mac, launchd's `com.openssh.ssh-agent` is running and offers `SSH_AUTH_SOCK => /var/run/com.apple.launchd.…/Listeners` (`launchctl print`). That the owner's Ghostty shells inherit it is **[추정]**: no Ghostty process was found to read |
| Concurrent sessions | distinct sockets (`…dNm69E9anr`, `…8jKc8wIptn`), both answer |
| Discovery without the env (tmux, an old shell) | scanning `~/.ssh/agent/s.*.sshd.*` and sending hello found the live socket |
| Cleanup | 0 entries in `~/.ssh/agent` after all sessions (session.c:175-183 unlinks), and within ~1 s of `kill -9` of A's ssh. A link that dies without FIN leaves the socket until sshd notices **[추정]**; it is a dead socket, never in the way of the next session's random name |
| No network from B | under `unshare -rn` (`lo DOWN`; TCP to A → `Network is unreachable`): hello, range, SHA all OK |
| Stall hazard | 3 s delay before the bind reply → **3.01 s** gap in the owner's session output. Agent `SIGSTOP`ped while B runs `ssh-add -l` → **6.26 s** gap, until `SIGCONT` |
| The owner's stanza, end to end | `ssh -F proto/ssh_config.sopen` (`Host B / ForwardAgent ./agt-%n.sock`, then `Include ~/.ssh/config`). The owner's own settings still applied (`hostname <addr>`, `userknownhostsfile /dev/null`); hello, SHA OK, 8×2 striped 7.01 MB/s |
| The layout `sopen allow` would write | `layout/owner_config_with_include` is `Include <sopen file>`, then `Include ~/.ssh/config`, then `/etc/ssh/ssh_config` (which `-F` skips). Against plain `ssh -G`: the Linux laptop differs in **one line only**, `forwardagent no` → `…/.sopen/origin/the Linux laptop.sock`; `github.com` and `some-other-host` show **0** differing lines |
| Interactive login shell with a pty | `ssh -tt` into fish, with commands typed: the range read through `SSH_AUTH_SOCK` returned the right SHA (2 forwarded binds, B's host key) |

Throughput, interleaved with (a1) in the same sessions (MB/s of payload, 64 KiB reads unless noted):

| Transport | 1 in flight | more in flight | 8 connections striped |
|---|---|---|---|
| (a1) unix `RemoteForward`, 2 MiB window (channels.c:4101-4103, channels.h:233-234) | 1.82 · 2.29 · 2.18 · 2.32 | d8: 7.06 · 5.30 · 7.58 · 7.25; first, noisy run d4 0.87 · d16 1.78 · d32 1.07 | 4×4: 7.15 · 3.20 |
| (a2) agent channel, 64 KiB window (session.c:209-211 → channels.c:2104-2108; channels.h:235-236) | 1.49 · 1.44 · 1.57; first, noisy run 0.27 | d2 1.95 · d8 1.79 / 1.07 · 32 KiB d2 1.67; first run d16 0.08 | 8×1: 6.06 · 5.77; 8×2: 6.75 · 6.86 · 7.01 |
| A's agent alone, A-local | — | — | 4×2: 800 |

- **Depth does not help one agent channel.** At most 64 KiB is in flight on it, so depth adds
  nothing. Its p50 per 64 KiB read was 35–39 ms, against 22–26 ms on (a1).
- **The bandwidth-delay arithmetic over-predicts.** At an 18 ms average RTT, 64 KiB per RTT would be
  3.6 MB/s; the measured figure was 1.1–2.0. Treat any figure for another link as an **[추정]**
  upper bound.
- **What each design would get:**
  - SFTP carried over **one** agent channel gets the single-channel row.
  - The striped figures need **several channels**. `Ranged` (remote/ranged.rs:110) is "one file open
    for streaming on one connection", so striping is new work (§4).

### 2.3 (b) `ssh -O forward -R …` added to the owner's ControlMaster by an agent on A

Measured with a master of my own:
- **A file in the way blocks the forward.** `-O forward` failed with `mux_client_forward: forwarding
  request failed: remote port forwarding failed`, rc 255.
- **Cleaning first works.** The agent removed the file through the master (`ssh -S ctl B rm -f …`),
  then `-O forward` returned rc 0 and hello worked.
- **Cancel leaves the file.** `-O cancel` returned rc 0, but the file stayed and refused connections.
- **It fixes the stale-file problem only if someone acts on A.** B has no channel to ask on, so A
  must act at every connect.
- **It costs the owner's ssh behaviour.** The owner's `ssh B` must become a mux master; it is
  `controlmaster false` today (`ssh -G`). A `LocalCommand` hook can drive it, but it blocks the
  client loop: ssh.c:2315 → sshconnect.c:1696, `fork` then `waitpid`. Verdict: **killed** by (a2), which needs none of this.

### 2.4 (c) A channel through the terminal stream (the "extended ssh")

`proto/ptyproxy.py` runs ssh in a pty of its own on A. It answers APC frames
`ESC _ SOPEN; … ESC \` that B's `proto/ttyclient.py` writes to `/dev/tty` (in raw mode), typing
`#base64.` replies into ssh's input. ssh runs with `-e none`, so `~.` cannot fire.

| Experiment | Result |
|---|---|
| hello / range | 17.8 ms; SHA OK |
| Throughput (payload; wire is 4/3 for base64) | d1 2.03, d4 4.31, d8 3.68 MB/s |
| B→A tty bandwidth (the ceiling for images too) | 24 MiB as base64: 5.78 · 6.84 MB/s of payload incl. connect. ssh + pty only; **no terminal emulator in the path** |
| tmux 3.7b (local run on A) | bare APC: **never reached the proxy**, with `allow-passthrough` off or on. `\ePtmux;`-wrapped + `allow-passthrough on`: answered |

Verdict: **alive but second.**
- The owner types `sopen ssh B`, an alias, not `ssh B`.
- The proxy sits in the path of every keystroke.
- Replies are typed into tmux's *active* pane. A pane switch mid-transfer misroutes them
  **[추정]**, not measured.
- Mosh or a terminal multiplexer of the owner's own (herdr) in between breaks it **[추정]**.
- It is the only route that also works when A cannot set `ForwardAgent` (another person's ssh
  client). Kitty's `kitten ssh` is built the same way **[추정]**. Keep it as a fallback, not the
  default.

### 2.5 (d) A port on A that only B's address can reach

- **pf needs root:** `pfctl -s info` → `/dev/pf: Permission denied`.
- **The application firewall cannot do it:** it is disabled, and it filters by app, not by address
  **[추정]**.
- **A is behind NAT:** `en0 <addr>`, gateway `<addr>`. The only way B reaches A is the
  overlay (`<addr>`), which is exactly the B→A path the owner wants closed.
- **That path is in use today.** `ss` on B shows the owner's own existing B→A connections: ssh pid
  14003 to A:22, nxplayer to A:4000, msedge to A:443. They are pre-existing and not from this test;
  the `unshare -rn` run is the proof for the prototype.
- **An app-level allowlist is still a listener.** Checking the peer address at `accept` needs no
  root, but leaves a pre-auth listener on A for anything that holds or spoofs B's overlay address.

Verdict: **killed** on the owner's own requirement. A Tailscale ACL that allows B→A only on one port
would do it without root **[추정]**, but it is an admin-console change, and it is still a B→A
connection.

## 3. What `sopen allow B` does (route a2)

**On A** (all reversible):
1. **Check reachability.** `ssh B true` succeeds, and `ssh -G B` shows no `ForwardAgent` of the
   owner's own (one value only, first wins) and no `IdentityAgent none`.
2. **Check the agent precondition.** `SSH_AUTH_SOCK` is set and live in the owner's terminal. If it
   is missing, say so and stop: forwarding would silently not happen.
3. **Pin B's host key.** Run one `ssh B` with a probe socket and record the host key from its
   session-bind, the key A's ssh actually saw for B. The owner's config sets
   `UserKnownHostsFile /dev/null` and `StrictHostKeyChecking no` for B, so A does not check B's key
   today; this pin is new.
4. **Write sopen's own file** `~/.sopen/ssh_config`:
   ```
   Host B
     ForwardAgent ~/.sopen/origin/%n.sock
   ```
   Add one line, `Include ~/.sopen/ssh_config`, at the **top** of `~/.ssh/config`. It has to be
   first because the first value wins. This exact layout was checked with `ssh -G`: it changes
   B's `forwardagent` and nothing else, and changes nothing for other hosts (§2.2).
5. **Start the agent, and choose who owns its socket.** The trade-off is **[추정]**: neither side
   was run as a service.
   - **The agent owns its socket** (a launchd or systemd user service that keeps it alive). A dead
     agent fails closed: measured, B gets `Connection reset by peer` and the owner's session is
     unaffected.
   - **launchd or systemd owns it** (socket activation). The agent starts on demand. But an agent
     that crashes or cannot start leaves connections queued in the backlog, A's ssh waits for the
     bind reply, and that is the measured `SIGSTOP` freeze.
   - Recommendation: the agent owns its socket (§7).
6. **Record a policy entry for B.** It holds the alias, the host-key pin, the readable roots, whether
   B may open things on A's screen, and the rate limits.

**On B: nothing.** No file, no key, no sshd change. Optional: `sopen allow` checks B's sopen version
understands origin.

**Undo, `sopen allow --remove B`.** Delete B's stanza. Remove the `Include` line when no host is
left. Delete B's policy entry and socket. The agent closes B's open connections, and live sessions'
next connects fail closed with `Connection reset by peer`, as measured with a missing target.
Nothing to undo on B. **Kill switch for all hosts:** `sopen allow --off` stops the service and
removes every socket.

## 4. Protocol, and where it sits in sopen

**Identity.**
- The **primary** identity is the A-side socket the connection arrived on. It is set by A's own
  config (`%n`), so B cannot choose it.
- The **cross-check** is the session-bind host key against the pin. A machine can obtain another
  server's key, session id and signature by connecting to it, so the bind is a cross-check, not a
  proof. The prototype logged the key but did not verify the signature.
- **A per-host capability token adds nothing here.** Anything on B with the owner's uid can reach the
  socket and could read a token stored on B.
  - The scope line "a key or token per allowed machine" (ssot item 4) is filled by the per-host
    socket on A, the host-key pin, and the per-host policy entry. Each can be revoked alone.
  - Whether that satisfies the line is decision 7 in §7.

**Messages.**
- Requests are `SSH_AGENTC_EXTENSION` (27) messages named `…@sopen.2lab.ai`. B's ordinary ssh, git
  and `ssh-add` keep working against the same socket and see an empty agent. Afterwards the
  connection may switch to sopen framing: ssh relays the channel's bytes unparsed, as the 64 KiB
  replies in agent framing showed.
- `hello`: A's hostname, sopen version, B's alias as A knows it, and the capabilities granted.
- `open {source, view state}`: open on A's screen.
  - A **file on B** is passed as `B-alias:/abs` and A's sopen reads it with its **own** A→B SFTP,
    the existing path. That fixes forward.rs:244, which names B by B's hostname, not by A's alias.
  - A **URL on B's loopback** becomes an A→B `ssh -L`, the mirror of today's tunnel at
    forward.rs:463.
  - Only sopen's own windows open. **Never** `--app` (`app.rs:22`, `open -a`), and never hand-off to
    LaunchServices. Otherwise B could start any app, or open a `.command` file, on A.
- `origin:` reads: `stat`, `realpath`, `list`, ranged `read`. These are the only bulk data on the
  channel.

**Mapping onto sopen.**
- **`origin:` is a new remote kind beside SFTP.**
  - It implements `RemoteFs` read-only: stat_opt, realpath, read_head, list_entries, read_to
    (remote/mod.rs:406).
  - It implements `Ranged` (remote/ranged.rs:110), which `stream::Source::open` takes as-is
    (stream/source.rs:1200).
- **The cheapest form is SFTP over the channel.**
  - `openssh_sftp_client::Sftp::new(W: AsyncWrite, R: AsyncRead, …)` (openssh-sftp-client 0.15.12
    `src/sftp.rs:114`) accepts any byte stream, so sftp.rs, ranged.rs and the 06 machinery are
    reused.
  - A's agent then serves read-only SFTP with a path policy. One way is a filtering proxy in front of
    `sftp-server -R` (read-only, sftp-server.c:1926) with an op allow-list `-p` (sftp-server.c:1957).
    It checks every path request by realpath on A against the approved roots, so a symlink under a
    root cannot reach `~/.ssh`.
  - One SFTP session on one agent channel gets the single-channel row (1.1–2.0 MB/s). The 06
    in-flight cap (256 KiB–4 MiB) cannot be reached through a 64 KiB window.
  - Striping K channels needs a `Ranged` that spreads tickets over K connections and returns
    completions in ticket order.
- **What to build first is a spec decision.** 2 MB/s is fine for an mp3 (≈40 KB/s), images, and
  random 64 KiB views; it is below the link only for bulk.

**What B may read.**
- Default: nothing until the owner names roots in `sopen allow B --read ~/Music` (owner decision).
- Optional per-request prompt on A for paths outside the roots.
- Reads are rate- and byte-capped per host.

## 5. Threat model

| Attacker | Through (a2), with this design | Bound on A |
|---|---|---|
| Malware with the owner's uid on B | Connects to the socket. It can read A's files **only under the approved roots**, ask A to open sopen windows (UI spam, or a malicious file or page from B rendered by A's decoders, the same exposure as opening B's files from A today), and flood requests. It gets **no shell, no ssh, no keys, no network on A**, and no write | roots + realpath confinement, read-only; rate and byte caps; per-host prompt for opens; `--remove` or `--off` |
| root on B | the same as above, plus reaching any user's sockets on B. It also controls B's sshd, so it can forge session-bind **for B itself**, but not as another allowed host: the A-side socket names the host | the same; the host-key pin catches a swapped B |
| Anything on B, against the owner's session | each connection costs A's ssh a synchronous bind round trip; a healthy agent answers in < 1 ms locally **[추정]** | the agent answers the bind first, from the accept loop, with no locks |
| B hopping onward, `ssh -A C` from B | C gets B's origin access, the same as nested agent forwarding. Such a connection's first bind is still A's; a later bind names C **[추정]**: the prototype logs it (`first=false`), but that was not run | refuse a connection that carries a second bind, or allow it per host |
| A bug in sopen's agent (hang) | **freezes the owner's A→B terminal** whenever B touches the socket, measured at 6.26 s | supervision; keep the agent tiny; a hang is a P0 bug |

**Against ssh-agent forwarding's known risk.** `ssh_config.5:893-901`: whoever can bypass file
permissions on B "can perform operations on the keys that enable them to authenticate using the
identities loaded into the agent". Here the agent holds no keys and signs nothing. B's reach is
sopen's read-only, root-confined file service and window opens. That is strictly less than
`ForwardAgent yes`, and less than today's set-up, where B can ssh to A.

**The other routes.**
- (a1) has the same B-side exposure (a 0600 socket).
- (c) puts a proxy into every keystroke. Anything that writes to B's tty can inject requests, which
  is any process with the owner's uid.
- (d) adds a network listener on A.

## 6. Fit with message 23 (xtools as a TUI on B, GUI on A)

- **The TUI runs on B, inside A's terminal, over ssh.**
- **"Button → GUI".**
  - The TUI sends `open {B-alias:/path, view state}` over the origin channel. A's sopen opens the
    xtools GUI on A's screen and reads **B's** files through A's own A→B SFTP, the direction that
    exists.
  - The channel carries only the request and the state, a few KB. B's TUI then exits or says where
    it went (spec).
  - The view state (panel, cursor, mode 1/2/3, encoding) is a spec item.
- **Images in the TUI travel in the tty stream itself.** This is kitty graphics, iTerm2 inline
  images or sixel, with no origin channel. The ssh + pty path carried 5.8–6.8 MB/s B→A here.
  - The terminal emulator's own decode speed is unmeasured.
  - "Ghostty implements the kitty graphics protocol" is **[추정]**; `TERM_PROGRAM=ghostty` was seen
    in this session's environment.
  - tmux on B needs `allow-passthrough` and DCS wrapping (measured with APC above). Placement under
    tmux needs kitty's Unicode placeholders **[추정]**.
- **An `origin:` image is cheaper than it looks.** It is a file on A, so B's TUI could tell A's
  terminal to read it from A's own disk (kitty `t=f`). No image bytes would cross. **[추정]**, not
  built.
- **Video in the TUI** is frames as images at the tty bandwidth above. The GUI on A is the better
  surface for video.

## 7. Decisions that belong to the owner

1. **Editing `~/.ssh/config`.** `sopen allow` adds one `Include` line at the top of the owner's
   `~/.ssh/config`. Alternatively it prints the line for the owner to add.
2. **Real agent forwarding to B.** If the owner wants B to use A's keys as well, the agent must
   proxy key operations to the real agent, which re-grants what `ForwardAgent yes` grants. The
   default is no key use.
3. **What B may read.** Default: nothing until roots are named, or the whole home minus `~/.ssh`
   (and others), or a prompt per file.
4. **Opening on A's screen.** Free, or a prompt on A per open, or per session.
5. **Accept the stall risk.** A stuck agent freezes the A→B terminal. Alternatively choose (c) for
   hosts where that is unacceptable.
6. **A without `SSH_AUTH_SOCK`.** Require it (the macOS default via launchd), or let the stanza set
   `IdentityAgent` to sopen's agent and have it proxy A's own authentication too. That puts sopen in
   the owner's ssh auth path.
7. **The token line in ssot item 4.** Accept the per-host socket + host-key pin as "a key or token
   per allowed machine", or keep a token anyway as defence in depth.
8. **Who owns the agent's socket** (§3 step 5). The agent itself, which fails closed (recommended),
   or socket activation, which starts on demand but freezes the session if the agent cannot start.

## 8. Not measured

- Ghostty itself.
- A shell inside tmux on B (tmux is not installed on B; the tmux run was on A).
- Mosh and herdr in the path.
- A link that drops without FIN.
- The striped `Ranged` design under sopen's real prefetcher.
- Signature verification of session-bind.
- B as macOS: discovery there would be the `SSH_AUTH_SOCK` env and the `~/.ssh/agent` layout of
  sshd ≥ 10.x only.
- An older sshd on B, which uses `/tmp/ssh-*/agent.<pid>`.

## Appendix: the prototype

`origin-research/proto/`:
- `agent.py`: raw and agent framing, realpath-confined ranged reads, `BIND_DELAY`.
- `client.py`: hello, range, bench, pbench.
- `ptyproxy.py`, `ttyclient.py`: route (c).
- `ssh_config.sopen`: the end-to-end test config, the stanza with the owner's config Included inside
  the `Host` block.

`origin-research/layout/` holds the proposed layout: sopen's file Included first, then the owner's
config.

The agent logs are `origin-research/logs/*.jsonl`, and the OpenSSH sources read are in
`origin-research/openssh-src/V_10_{3,5}_P1/`. The B-side folder `/tmp/sopen-research-origin/` was
removed after the runs.
