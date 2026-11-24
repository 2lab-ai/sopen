# origin — done (append-only)

Loop: ./loop.md · Each closed row or round appends one block at the bottom; nothing above it ever
changes. Rows here are snapshots, frozen when they moved.

## R0 (part 1) — OR-R closed 2026-11-24

- Result: `../research/origin.md`. Recommended route (a2): sopen's own agent on A behind
  `ForwardAgent <path>` in the owner's plain `ssh B`. B's sshd makes a per-session socket in
  `~/.ssh/agent` and sets `SSH_AUTH_SOCK`; nothing is installed or configured on B. The prototype
  between the Mac (A) and the Linux laptop (B) proved these:
  - hello and a 64 KiB range of a file on A, with the right SHA-256;
  - B with no network to A (`unshare -rn`);
  - two concurrent sessions;
  - the sockets cleaned up after `kill -9`;
  - B getting no key use (`ssh-add -l`: no identities).

  Killed: a fixed-path `RemoteForward` (a stale socket blocks the next session unless B's sshd
  changes), forwards added to a ControlMaster, and a port on A (a B→A connection; root; NAT). Kept as
  a fallback: a pty proxy (`sopen ssh B`), 2.0–4.3 MB/s. Measured costs: one agent channel moves
  1.1–2.0 MB/s (8 striped: 5.8–7.0), a stuck agent freezes the owner's A→B session, and A needs an
  ssh-agent of its own.
- The orchestrator read the prototype's session log: every forwarded connection's first message
  carries a host key whose fingerprint equals the Linux laptop's own ed25519 host key, re-read there
  with `ssh-keygen -lf`. After the runs, B's `~/.ssh/agent` holds 0 entries and B's research folder
  is gone.

### Closed rows

| ID | Status | What | Who |
|---|---|---|---|
| OR-R | ✅ done 2026-11-24 | Research with a running prototype: how sopen on B reaches A when only A→B ssh exists, what `sopen allow B` sets up, the threat model, and the fit with message 23's TUI. Output: `../research/origin.md`. | research unit |

## R0 (part 2) — OR-SPEC closed 2026-11-25

- Written as `../11-origin.md`. It specifies:
  - the protocol on the forwarded socket;
  - what B may read;
  - opening on A's screen;
  - the agent and its supervisor;
  - how B finds origin;
  - `sopen allow`;
  - the threat model, acceptance, the test plan and the work units, with the OR-0 spike first.
- Reviewed by two engines.
  - **Round 1 was FIX.** 9 + 13 must-fixes; merged, 18 items.
  - **Orchestrator decisions D13–D17:**
    - D13: non-local objects are refused.
    - D14: B's opens start no git. A's sopen may start only the viewers' ffmpeg and ffprobe, and A's ssh to the pinned B.
    - D15: `--repin`.
    - D16: the pin holds on every A→B connection made for B.
    - D17: a supervisor bounds a single stall. A double stall is an accepted residual.
  - **The fix-check was FIX: 3 + 7 gaps in the fixes.** They covered DNS-vouched host keys, masters reused across a re-pin, socket name lengths, O26 in practice, the descriptor budget, test placement, the mount oracle and later cache folders.
  - **No third review round.** The orchestrator checked those fixes at their cited lines on 2026-11-25. O31's arithmetic holds: `R + 4·C + P = F`, with a floor that keeps every pool positive.

### Closed rows

| ID | Status | What | Who |
|---|---|---|---|
| OR-SPEC | ✅ done 2026-11-25 | The numbered document for route a2 (sopen's own agent behind `ForwardAgent <path>` in the owner's plain `ssh B`), with the orchestrator's decisions below, reviewed by two engines before any code; work units. | orchestrator |
