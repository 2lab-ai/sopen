# origin — loop (open items only)

Status: round 0 (research), opened 2026-11-24. Target: [`ssot.md`](ssot.md). The contract will be a
numbered document written after the research; design decisions go there, not here.

## Round 0 — research

| ID | Status | What | Who |
|---|---|---|---|
| OR-SPEC | 🔴 open | The numbered document for route a2 (sopen's own agent behind `ForwardAgent <path>` in the owner's plain `ssh B`), with the orchestrator's decisions below, reviewed by two engines before any code; work units. | orchestrator |

Closed so far: OR-R (the research) — [`loop.done.md`](loop.done.md), R0 part 1.

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

## Build facts

- Measured 2026-11-24: the Mac's ssh client OpenSSH 10.3p1; the Linux host's OpenSSH 10.5p1, its
  sshd sets none of `StreamLocalBindUnlink`, `AllowStreamLocalForwarding`, `AllowTcpForwarding`
  (defaults apply); its `XDG_RUNTIME_DIR` is a 0700 directory. The Mac's sshd accepts only
  `LANG LC_*` environment.
