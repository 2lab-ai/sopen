# origin — loop (open items only)

Status: round 1 (code), opened 2026-11-25; round 0 (research and spec) is closed. Target: [`ssot.md`](ssot.md). The contract will be a
numbered document written after the research; design decisions go there, not here.

## Round 1 — code

| ID | Status | What | Who |
|---|---|---|---|
| OR-0 | 🔴 open | 11 §16: the spike on both machines before any unit merges; its measurements (a)–(j) gate the decisions they name | research unit |
| OR-units | 🔴 open | 11 §16's OR-wire → OR-filter → OR-serve → OR-open-a → OR-agent → OR-client → OR-allow → OR-docs, in that order (each shares a file with the next) | coder |

Closed so far: OR-R (the research) — [`loop.done.md`](loop.done.md), R0 part 1; OR-SPEC (`../11-origin.md`) — R0 part 2.

Decisions the orchestrator took on OR-R's open questions (2026-11-24; reversible, the owner may change
them; OR-SPEC writes them down with reasons):
1. `sopen allow B` adds one `Include` line to `~/.ssh/config` (the owner ran the command that asks for
   it), says so, and `sopen allow --remove B` / `--off` take it out; `--print` only shows the lines.
2. B never gets the owner's real agent keys.
3. B reads A's home read-only, minus every path with a component that starts with `.` (so `~/.ssh`,
   `~/.config`, `~/.gnupg` and every dotfile stay out); `--read <dir>` sets the roots instead.
4. Opening on A's screen needs no prompt; it is rate-limited, and `--app` is never reachable from B.
5. A stuck agent would freeze the owner's ssh session (measured), so the agent answers the first
   message from a thread that does nothing else, and a request that cannot be served fails at once.
6. Without `SSH_AUTH_SOCK` on A, `sopen allow` refuses and says why (ssh then forwards nothing).
7. One socket per allowed host, plus B's host key pinned from the first message, is the "key or token
   per allowed machine" of ssot item 4.
8. The agent owns its socket and fails closed (no socket activation).

Decisions taken on OR-SPEC's first draft (2026-11-25; reversible; written into `../11-origin.md` §13 as
D9–D12):
9. D3a accepted: on macOS `~/Library` is outside the default roots too (app tokens, keychains, and
   sopen's own cache of third machines' files would otherwise be readable from B); `--read <dir>`
   still sets the roots explicitly.
10. O10 accepted: `sopen allow` also installs the user service (LaunchAgent / systemd user unit) that
    starts the agent — D8 needs a starter; `allow` prints both files it writes, `--print` shows
    both, `--off`/`--remove` take both out.
11. O6 accepted: an open from B never reaches an external editor (only sopen's own windows), on top
    of `--app` never being reachable.
12. O3 accepted: B's host key is pinned only from `sopen allow`'s own probe; the agent never pins on
    first real use (a first contact the owner didn't run is refused).

Decisions taken on the two-engine review of `../11-origin.md` (2026-11-25; reversible; written into
its §13 as D13–D17):
13. Non-local objects are refused always: network/FUSE mounts (macOS `MNT_LOCAL` false; Linux deny-list
    of network and FUSE fs types) and dataless/evicted files (`SF_DATALESS`) — they make A fetch a
    third host's data for B and can block a worker without bound.
14. `--from-origin` windows show no git panel and start no git (local git runs B-influenced config:
    core.fsmonitor/hooks); §12 row 1 lists exactly the processes A's sopen may start for B
    (ffmpeg/ffprobe as viewers need them, nothing else).
15. A changed host key on a re-run of `sopen allow B` refuses unless `--repin`; changing roots or
    restarting the service never re-probes.
16. The pin covers every A→B connection made for B's requests (kind-1 opens): such connections run
    with the pinned key as their only known host key (`UserKnownHostsFile=<pin file>`,
    `StrictHostKeyChecking=yes`, no master reuse unless it was made that way).
17. A whole-process stall of the agent must be bounded from outside it: a supervisor outside the agent
    process (or the service manager's watchdog where it exists) probes the socket and kills/restarts
    a stalled agent within a stated bound.

Resolved 2026-11-25 (reversible) on the spec's two remaining tensions:
- 14 is about git: windows A's sopen opens for B start no git. The processes A's sopen may start for B
  are exactly the viewers' ffmpeg/ffprobe and, for kind-1 opens only, A's own ssh to the pinned B
  under 16's options.
- 17: a single stall (agent or supervisor) is bounded at ≈3 s on both systems. The double stall
  (supervisor and agent together: a whole-user or whole-system stall) is an accepted residual. Its
  worst case is the owner's session waiting on each new connection from B; the way out is
  `sopen allow --off` or ending the session (spec §12).

## Build facts

- Measured 2026-11-24: the Mac's ssh client OpenSSH 10.3p1; the Linux host's OpenSSH 10.5p1, its
  sshd sets none of `StreamLocalBindUnlink`, `AllowStreamLocalForwarding`, `AllowTcpForwarding`
  (defaults apply); its `XDG_RUNTIME_DIR` is a 0700 directory. The Mac's sshd accepts only
  `LANG LC_*` environment.
