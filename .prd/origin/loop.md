# origin — loop (open items only)

Status: round 0 (research), opened 2026-11-24. Target: [`ssot.md`](ssot.md). The contract will be a
numbered document written after the research; design decisions go there, not here.

## Round 0 — research

| ID | Status | What | Who |
|---|---|---|---|
| OR-R | 🔴 open | Research with a running prototype: how sopen on B reaches A when only A→B ssh exists — a channel inside the owner's own `ssh B` (a reverse-forwarded socket to an agent on A, like ssh-agent forwarding; forwards added to an existing master; a channel through the terminal stream itself) against a listening port on A, and what `sopen allow B` would set up. Threat model, what each route needs on A and on B (no root on either, no sshd change on B preferred), stale sockets and several sessions at once, discovery from B's shell, and how it fits message 23's TUI. Output: `../research/origin.md`. | research unit |
| OR-SPEC | 🔴 open | The numbered document for the chosen route, reviewed by two engines before any code; work units. | orchestrator |

## Build facts

- Measured 2026-11-24: the Mac's ssh client OpenSSH 10.3p1; the Linux host's OpenSSH 10.5p1, its
  sshd sets none of `StreamLocalBindUnlink`, `AllowStreamLocalForwarding`, `AllowTcpForwarding`
  (defaults apply); its `XDG_RUNTIME_DIR` is a 0700 directory. The Mac's sshd accepts only
  `LANG LC_*` environment.
