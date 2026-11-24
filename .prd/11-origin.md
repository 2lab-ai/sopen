---
title: sopen — origin, the machine the owner came from
type: spec
tags: [sopen, spec, origin, ssh, security]
---

# 11 — origin: sopen on B reaches the machine the owner came from

Status: planned (2026-11-25). Nothing here is built. The work units start with a spike (OR-0,
§16), and the document waits for its two-engine review (OR-SPEC in [`origin/loop.md`](origin/loop.md)).

Answers [`origin/ssot.md`](origin/ssot.md) (owner message 24) by route a2 of the research,
[`research/origin.md`](research/origin.md), cited *origin* §n. That route is sopen's own agent behind
`ForwardAgent <path>` in the owner's plain `ssh B`. Decisions 1–8 are the orchestrator's (2026-11-24,
`origin/loop.md`), and so are D9–D17 (2026-11-25), which accepted four of this document's first
proposals and settled the review's open choices. O1–O35 are this document's own choices. All of
them are reversible.

**Sources.**
- sopen is cited at 77b0beb. The research's own sopen lines are at 6ea3638; `main.rs` has moved one
  line since.
- OpenSSH is cited as `tag:file:line`: `V_10_3_P1` is A's client and `V_10_5_P1` is B's server,
  both upstream openssh-portable. `authfd.h` was fetched from the `V_10_3_P1` tag. A file named
  without its tag is `V_10_3_P1` for the client's files (`ssh.c`, `readconf.c`, `authfd.c`,
  `authfd.h`, `clientloop.c`, `mux.c`, `ssh_config.5`) and `V_10_5_P1` for the server's
  (`session.c`, `misc-agent.c`, `sshd_config.5`).

**Markers.**
- **[M]**: measured in the research, at the section given.
- **[S]**: read in source.
- **[추정]**: inferred, not measured.
- **[owner]**: only the owner's machines can answer it.

Owner message 24 (2026-11-24), verbatim (the second, whole copy):

> 그리고 별도로 이거 연구해줘 지금은 서로 연결되고 ssh로 접속할수 있어야 모든 기능을 쓸수 있는데
>
> A(터미널) -> B(작업머신) 일경우 ssh B로 해서 B에서 sopen --target A 명령 하면 B에서 "ssh A"가 되야 작동하는데 이걸 좀 생각해봐
>
> A에서 B로 ssh만 될경우에도 B 쉘에서 sopen --target origin origin:~/A.mp3
>
> 이게 되려면 어떻게 해야할까? ssh를 포함한 extended-ssh를 구현해서 ssh 대신 써야할까?
>
> 이거는 별도로 리서치해줘
>
> 의도: A에서 B로만 접속이 가능할경우. B애세 sopen 타겟을 어떻게 A로 할수 있을지. 보안 문제로 B에서는 A로 접속을 못하는 상태로 하고 싶음.
> 일종의 A에서 B로 뭔가 포트를 열어두고 작업을 해야하지 하는 방법 ㅁ라고 가장 엘레강스한 방법이 뭐가 있을까 그냥
>
> A에서 ssh B해서 B에서 자연스럽게 작업하고 싶음
> 아니면 A에서 sopen allow B  같은 명령을 미리 실행하고 ssh B로 가서 할수 있든가(이러면 spen allow B명령이 B로 ssh 타고 들어가서 거시서 뭔가 설정을 미리 해두는 거지 포트 열고 포트 접속할수 있게 키 발급하고 A에서 오픈한 포트를 B ip에서만 접속할수 있게 열고  등등의 철저한 보안처리)

**What this amends when it lands:**
- 01 §4a: the reserved host word `origin`. `--target origin` and `origin:` sources. `SOURCE` rewriting
  for origin, which A does itself.
- 01 §4: "a `--target` machine runs the sopen installed there". On origin, A's sopen is started by
  A's agent, not by ssh.
- 10 §7.3 says "a HOST that cannot is the origin workstream's". The switch to `origin` is §7's `open`
  carrying the switch state (X2's, not this cut).
- AGENTS.md "Receipts" gets the hooks of §15.
- The README gets a section, "From the machine you came from" (OR-docs).

## 1. The contract

**Setup and natural use.**
- The owner runs `sopen allow B` on A once. After that, the owner's plain `ssh B` carries a channel
  to sopen's agent on A.
- On B, `sopen --target origin SOURCE` opens SOURCE in a sopen window on A's screen.
- `sopen origin:PATH` reads a file of A from B, and so does every route that reads a remote file.
- Nothing is installed or configured on B. B's sshd is not changed. B makes no network connection
  to A. *origin* §2.2 ran a client under `unshare -rn` [M].

**What B can reach.** Only sopen's requests, and only these:
- `stat`, `list`, and ranged reads of A's files. The default reach is A's home minus every dot path
  (D3; D9, §6).
- Opening sopen's own windows on A, rate-limited, for A's files under the same rule and for B's own
  files (§7).
- No shell on A, no ssh to A, no key use (D2), nothing on A's network, and no writes.

**Identity.**
- The A-side socket the connection arrives on names the host. A's own ssh config chooses that
  socket, so B cannot choose it.
- B's host key, pinned when `sopen allow B` ran, is a cross-check on it (D7).

**Undo.**
- `sopen allow --remove B` undoes one host.
- `sopen allow --off` undoes everything, the `Include` line included (D1).

**Fail closed.** A dead or removed agent costs B its origin. It never costs the owner the ssh session
(O11). A stuck agent would freeze the session [M *origin* §2.2].
- §8.3 keeps the piece that ssh waits on free of every lock, disk access and other connection, and
  always able to accept.
- A supervisor outside the agent ends a single stall within about 3 s (D17, §8.6). A stall of the
  supervisor and the agent together is §12's accepted residual.

## 2. Scope

**In:**
- The route a2 end to end: the protocol (§5), the read filter (§6), opens on A (§7), the agent's
  lifecycle (§8), discovery on B (§9), `sopen allow` and its forms (§10), and the lines (§11).
- B as Linux with the per-session socket layout of B's OpenSSH 10.5 (`misc-agent.c:186-197`; which
  release first used it is unchecked [추정]). Older sshd layouts are a discovery fallback only (§9)
  [추정].
- A as macOS or Linux, with a user service manager (§8.1).

**Not in:**
- Windows (01 §9).
- Writes from B. Every write is refused by A (§5.3).
- Addresses (`http(s)://`) through origin. B-loopback addresses are a later cut; others are never
  allowed (O14).
- Striping one read over several agent channels. The research measured 5.8–7.0 MB/s for eight
  channels against 1.1–2.0 MB/s for one [M *origin* §2.2]. This cut is one channel per session
  (O16).
- Route (c), the pty proxy `sopen ssh B` [M *origin* §2.4]. It is kept as a fallback, not built here.
- The cross-machine TUI switch to origin (10 §7.3, X2). §7's `open` reserves its `state` field.
- Key use by B (D2).
- Signature checks of the session binding (O4).
- Network and FUSE mounts and evicted cloud files under a root (D13).
- An onward hop from B (O5).

## 3. Words

- **A** is the machine the owner types `ssh B` on.
- **B** is the machine reached, named by the alias the owner types. A host allowed later is another
  B.
- **origin** is the host word on B that means "the A this session came from".
- **The agent** is sopen's process on A behind every allowed host's socket (`sopen --origin-agent`).
- **The socket** is the agent's per-host unix socket on A: `<state>/origin/<B>.sock`, where `<state>`
  is sopen's state folder, `~/.local/state/sopen` (`src/cache.rs:36-47`).
- **The included file** is `<state>/origin/ssh_config`, the only file `~/.ssh/config` gains a line
  for (D1).
- **The policy** is `<state>/origin/hosts.toml`: per host, the socket, the pin, the roots and the
  date; for all hosts, the checked ssh clients (O26), sopen's own folders (O27) and `a_id`.
- **The supervisor** is the service's process, which starts the agent and kills it when it stalls
  (§8.6).
- **The pin** is B's host-key blob and its SHA256 fingerprint.
- **The bind** is the `session-bind@openssh.com` message A's ssh writes first on every forwarded
  connection (PROTOCOL.agent §1; `V_10_3_P1:authfd.c:748-773`).
- **A root** is a folder B may read below. **Hidden** is §6.2's list: dot names, D9's `Library`,
  O27's folders and D13's non-local objects.
- **The walk** is §6.3's one path resolution, which every request runs.

## 4. The architecture

### 4.1 The happy path, hop by hop

How `origin:~/A.mp3` on B becomes bytes from A's disk. Every hop is a line of code or a measurement.

1. **A's ssh opens the agent channel.** The owner types `ssh B` on A. The included file's `Host B`
   sets `ForwardAgent <state>/origin/B.sock`.
   - The path is `~`- and %-expanded, then `stat`ed (`V_10_3_P1:ssh.c:1509-1520`).
   - ssh forwards at all only when its own `SSH_AUTH_SOCK` connects (`V_10_3_P1:ssh.c:2149-2161`,
     `:2188-2190`; `authfd.c:121-133` and `:87-113`). This is D6.
2. **B's sshd makes the session's socket.** It binds `~/.ssh/agent/s.<hash>.sshd.<random>` and sets
   `SSH_AUTH_SOCK` (`V_10_5_P1:session.c:185-215`, `misc-agent.c:186-197`, `session.c:1055`). The
   socket is removed at session end (`session.c:173-183`) [M *origin* §2.2: 0 left, also after
   `kill -9`].
3. **B's sopen parses the source.** `origin:~/A.mp3` parses as `RemoteSpec { host: "origin", path:
   "~/A.mp3" }` (`src/target.rs:75-99`). `run_open` → `Place::connect` → `SftpRemote::connect`
   (`src/main.rs:634-635`, `:366-371`; `src/remote/sftp.rs:290`).
   - **The seam:** `SftpRemote::connect` sees `origin` before it runs `ssh -G`
     (`src/remote/sftp.rs:291-293`) and calls `origin::client::sftp()` instead (§9).
   - All nine connect call sites go through that one function: `src/main.rs:370, 420, 731, 781,
     1032, 1050` and `src/gui/browser.rs:213, 238, 241`.
4. **B's sopen opens a connection.** The client connects to the session's socket (§9). Each connect
   makes B's sshd open an agent channel to A.
5. **A's ssh relays to the agent.** On the channel open, A's ssh connects to the `ForwardAgent` path,
   never to its `SSH_AUTH_SOCK` (`V_10_3_P1:clientloop.c:1845-1849`). It writes the bind and waits for
   the reply, synchronously (`:1855-1859`; `authfd.c:181-198`). Then it relays bytes unparsed
   [M *origin* §2.2: 64 KiB replies in agent framing].
6. **The agent answers the bind** from its acceptor (§8.3), then hands the connection to a worker.
   The worker checks that the peer is an ssh `sopen allow` checked (O26).
7. **B's sopen sends a hello.** The client sends `hello` with mode `sftp` (§5.2). The reply names A,
   B's alias at A and what B may read.
8. **SFTP runs on the connection.** The client's SFTP session starts on the connection's two halves
   through `start_on` (`src/remote/sftp.rs:208-234`), which takes any `AsyncWrite`/`AsyncRead`
   (openssh-sftp-client 0.15.12 `src/sftp.rs:133-139`). The client is unchanged from here.
9. **The agent's SFTP server reads A's disk.** It answers REALPATH, STAT, OPEN, FSTAT and READ through
   §6's filter (§5.3).
10. **The data reaches the user.** `into_ranged` → `open_ranged` (OPEN, FSTAT;
    `src/remote/sftp/ranged.rs:158-188`) → `Ranged` (`src/remote/ranged.rs:110`) →
    `stream::Source::open` (`src/stream/source.rs:1200`). From there, 06's machinery serves a viewer
    or a fetch as for any SFTP host.

**`sopen --target origin SOURCE`, the other path.**
- `run_source` decides the destination (`src/main.rs:526-533`). An `origin` branch goes before
  `forward::is_here`, so a B whose host name is `origin` still means A. The branch calls
  `origin::client::open()` instead of `forward::run` (`src/main.rs:544`).
- That sends `hello` with mode `open`, then `open` (§5.2, §7).
- The agent starts `sopen --here --from-origin <you> -- <source>` on A (§7). It waits for the child
  and sends its exit code back. B leaves with that code, as 01 §4a's forward does (`src/forward.rs:1008`).

### 4.2 What nothing on B may ever do

B must never run `ssh origin`. Each place sopen starts ssh gets an origin rule:

| Where | Today | For `origin` |
|---|---|---|
| `src/remote/sftp.rs:290-293` (`SftpRemote::connect`: `ssh -G`, master) | ssh to the host | §9's client; no ssh |
| `src/remote/sftp.rs:671-673` (`connection_args`), used by `forward::run` (`src/forward.rs:433`), the tunnel (`:929`) and `exec_args` (`src/remote/exec.rs:31-33`) | `ssh -G` | error: `origin has no shell or ssh: …` |
| `src/gui/browser/git_host.rs:28-30` (`SshGit::new`) | the git panel over ssh | `Unavailable::NoGit` (`src/git/model.rs:78-80`): the panel is not offered, as for a remote without git (01 §4) |
| `src/gui/browser.rs:319-333` (`ssh_user`, `ssh -G`) | the tree's `~user` | `None`: the home is `realpath(".")`, which §5.3 resolves to A's home |
| `src/remote/sftp.rs:332` (the plain-rename slot over the master) | a second sftp channel | `rename::Slot::none()` (`src/remote/sftp/rename.rs:269-271`): no rename is ever served |
| `src/forward.rs:203-241` (`source_for`), with `--target C origin:x` | passed unchanged (`:216-220`) | refused: `origin:x names the machine this session came from; C cannot reach it` |
| `src/main.rs:1085-1135` (`open_in_editor`: text and, without a display, folders) | VS Code or Zed on the remote path, `vscode-remote://ssh-remote+<host>/…` (`src/zed.rs:216-236`): **VS Code's own ssh to the host** | text: the fetched copy opened as a local file (O21); a folder without a display: refused, `origin has no ssh for an editor; the viewer reads it` |
| `src/main.rs:1142-1158` (the terminal editor on a remote copy) | edit the copy, then upload it back | edit the copy; no upload. The line says `origin is read-only: the copy keeps your changes at <path>` |
| `src/main.rs:787-812` (the browser's F4 `launch`) | `zed::launch` with the host | a file: the copy, as above; a folder: refused with the same line (O21) |

`src/git/runner.rs:1032, 1051, 1072` start ssh only through an `SshGit` built by `SshGit::new`, which
the table's third row refuses.

## 5. The protocol on the socket (O2)

### 5.1 Framing and phases

**Framing.** Every message before SFTP uses the agent protocol's framing: a `uint32` length, then
that many bytes, the first of which is the type. Fields are SSH wire types (RFC 4251 §5: `byte`,
`uint32`, `string`, `bool`).

**Codes** (`V_10_3_P1:authfd.h`): `SSH_AGENT_FAILURE` 5, `SSH_AGENT_SUCCESS` 6,
`SSH2_AGENTC_REQUEST_IDENTITIES` 11, `SSH2_AGENT_IDENTITIES_ANSWER` 12, `SSH2_AGENTC_SIGN_REQUEST` 13,
`SSH_AGENTC_EXTENSION` 27, `SSH_AGENT_EXTENSION_FAILURE` 28, `SSH_AGENT_EXTENSION_RESPONSE` 29. The
replies 5 and 28 both read as failure (`authfd.c:64-81`).

**Size caps (O23).** The length is read and checked before any buffer is allocated:
- at most 16 KiB for the first message;
- at most 64 KiB for any later control message;
- at most 64 KiB for any SFTP packet from B, since B sends no writes.

A longer length closes the connection.

**The peer (O26).** Every connection on a host socket must come from an ssh client that `sopen
allow` checked. That client must write the bind, because B's own bytes can carry a forged one: B's
host key is public.
- **How the agent finds the peer.** The worker asks the kernel for the peer's process id:
  `LOCAL_PEERPID` on macOS, `SO_PEERCRED` on Linux. It reads that process's executable
  (`proc_pidpath`, or `/proc/<pid>/exe`) and its device and inode, all before reading anything from
  B.
- **What it must match.** One of the policy's checked clients. `sopen allow`'s probe records each
  one as the executable it actually saw: path, device, inode, and an `-V` of at least 8.9
  (§10.2 step 6).
- **After an OpenSSH upgrade** the recorded path's device and inode change. The worker, never the
  acceptor, then runs `<path> -V` once. If it says at least 8.9, the new device and inode are
  accepted and cached in memory under that pair, and the log says so. A path that is gone, or an
  older version, is refused as below.
- **The relay never passes in production.** The agent's own executable is never a checked client,
  even when listed. Only with the test hook `SOPEN_ORIGIN_TEST_RELAY=1` in the agent's environment
  does it accept a peer that is its own executable, the test relay (§15).
- **Why 8.9.** That is the first release that sends `session-bind` [추정 from the release notes;
  OR-0 (f) checks it]. The binding code is `V_10_3_P1:clientloop.c:1855` and `authfd.c:748-773`.
- **Any other peer** (an old ssh, a replaced binary, a script) → an empty agent, and every `hello`
  gets code 1: `A's ssh at <path> is not one sopen allow checked — run sopen allow B again in a
  terminal on A`.
- **Where the pin cannot be claimed.** A local process with the owner's uid can still connect and
  write any bind. It owns A already (§12).

**The phases of one connection:**

| Phase | From → to | Content |
|---|---|---|
| 1 | A's ssh → agent | The bind: `byte 27, string "session-bind@openssh.com", string hostkey, string session_id, string signature, bool is_forwarding` (`authfd.c:759-764`). |
| 1 reply | agent → A's ssh | `byte 6` when `is_forwarding` = 1 and `hostkey` — as a plain key, any certificate stripped — equals the host's pin byte for byte; else `byte 5`. Either way A's ssh opens the channel: a failed bind is only a debug line (`clientloop.c:1855-1859`). The verdict is kept for phase 2. |
| 1b | worker | O26's peer check, before any B byte is read |
| 2 | B → agent | B's first message (§5.2) |
| 3 | both | Per the mode: SFTP (§5.3), `open` (§7), or close |

**A first message that is not a bind** gets no phase-1 reply. With O26 it cannot come from a checked
client. The connection is an empty agent: a `hello` gets code 1 (`A's ssh sent no session binding`),
anything else gets the empty agent's answer.

### 5.2 Messages after the bind

**B's first message is always `hello`.** B sends nothing else until it holds a `hello` reply. A real
ssh-agent behind the socket (§9) then sees one unknown extension and answers 5 or 28 [추정 for which;
OR-0 (d) reads it], and the client stops. No path, no file and no request ever reaches a real agent.

**`hello` is frozen (O24).** Its request fields and its reply's first fields (`code`, `line`,
`proto`) keep this shape in every protocol version. A later version only appends fields, and the
`hello` reader alone ignores bytes after the fields it knows. So two sopens of any versions can
always agree on a version, or say that they cannot.

| Message | Fields | Reply |
|---|---|---|
| `hello` (B → A) | `byte 27, string "hello@sopen.2lab.ai", uint32 proto_min, uint32 proto_max, string sopen_version, string mode` (`info` \| `sftp` \| `open`) | `byte 29, string "hello@sopen.2lab.ai", uint32 code, string line, uint32 proto`; with code 0 also `string a_name, string a_id, string a_version, string you, string reads, bool opens` |
| `open` (B → A, only after a code-0 `hello` in mode `open`) | `byte 27, string "open@sopen.2lab.ai", uint32 kind` (0 = A's path, 1 = B's absolute path)`, string path, uint32 flags` (bit 0 = `--hex`; any other bit → code 1)`, string state` | `byte 29, string "open@sopen.2lab.ai", uint32 code, string line`; after a code 0, one more message when A's sopen ends: `byte 29, string "ended@sopen.2lab.ai", uint32 status, string last_line`, then A closes |
| `SSH2_AGENTC_REQUEST_IDENTITIES` (11) | — | `byte 12, uint32 0`: no keys (D2) [M *origin* §2.2: `ssh-add -l` → "The agent has no identities."] |
| A bind from B (`session-bind@openssh.com` as B's message) | — | `byte 6`, and the connection becomes an empty agent for good. That is an onward hop: B's own ssh binding for a C (O5). |
| anything else (sign 13, add, remove, lock, `query`, unknown extensions) | — | `byte 5` |

**The reply fields.**
- `code`: 0 = served; 1 = refused (`line` says why, one line); 2 = no common protocol version.
- `proto`: the version chosen, or 0 with code 2.
- `a_name`: A's short host name. `a_id`: 32 hex characters, made when `sopen allow` first ran on A
  and kept in the policy.
- `you`: B's alias at A. `reads`: the roots as the owner would type them (`~`, `~/Music`) with
  `(dot names left out)`, on macOS `(dot names and ~/Library left out)`. `opens`: whether B may
  open on A's screen (always true in proto 1; D4).
- `status`: A's sopen's exit code 0–255, or 256 + n when signal n ended it.
- **`state` is an allow-list (O25).** It must be empty in proto 1. A later version names the keys
  it accepts, and any other key → code 1.
- **Modes:** `info` closes after the reply. `sftp` turns the rest of the connection into SFTP, B
  speaking first. `open` expects one `open`.

**An empty agent.** A connection whose verdict is refused still answers the empty-agent rows: a pin
mismatch, `is_forwarding` = 0, no bind, a peer that is not a checked client, or a bind from B. B's
own `ssh-add` and its ssh to other hosts keep working against it and see no keys. Every `hello` on
it gets code 1 with the reason.

### 5.3 SFTP, as served by the agent (O1)

The agent serves SFTP version 3 itself (draft-ietf-secsh-filexfer-02), in the worker that holds the
connection. Every path-bearing request goes through §6's one walk, not only OPEN.

**Served:**
- **INIT** → VERSION 3, with one extension: `limits@openssh.com`.
  - Its reply is max packet 65 536, max read 262 144, max write 0 and max open handles 64.
  - The client reads it (openssh-sftp-client 0.15.12 `src/sftp.rs:228-256`). A max write of 0
    means "the default" to that client (`:249-250`), so writes are refused by the server below,
    not by the limit.
  - **The max packet is the incoming cap (O23).** READ replies may be longer: the cap is only on
    what B sends.
- **OPEN**, only with exactly `SSH_FXF_READ`. Any other flag → `PERMISSION_DENIED "origin is
  read-only"`. It opens the object §6's walk reached, and checks it.
- **READ**: at most 256 KiB, short at the end of the file, EOF at or past it. It is a `pread` on
  OPEN's descriptor.
- **FSTAT, CLOSE.**
- **STAT and LSTAT**: `fstatat` on the parent directory descriptor the walk reached, following the
  last name (STAT, through §6's link rule) or not (LSTAT).
- **OPENDIR** (`O_DIRECTORY`) and **READDIR**.
  - At most 100 names a reply, with attributes from `fstatat(…, AT_SYMLINK_NOFOLLOW)` and hidden
    names left out.
  - The `longname` is mode, links, owner, size, date and name, and never a link's target.
- **REALPATH**: the path the walk reached, built from the walk itself.
  - The client sends `.` for `~` and `rest` for `~/rest`, because no `expand-path@` extension is
    offered (`src/remote/sftp.rs:787-797`).
  - So "." and every relative path start from A's home.

**Refused.** Every other request is answered `SSH_FX_OP_UNSUPPORTED "origin is read-only"`: WRITE,
SETSTAT, FSETSTAT, REMOVE, MKDIR, RMDIR, RENAME, SYMLINK, every other EXTENDED, and READLINK (a link's
target would name a place outside the roots).

**What B sees of refusals.**
- B's file operations and saves meet today's error paths: a save keeps its copy (01 §5).
- `sopen cut` and `stems` fail with their line.
- A listing's link to a hidden target shows as a link with no target. A failed target stat becomes
  `None` (`src/remote/mod.rs:224-226`).

### 5.4 Version skew

**The rule.**
- Proto 1 is this document. B sends `[proto_min, proto_max]`.
- The agent picks the highest version both ranges hold. When there is none, it answers code 2 with
  both ranges and both sopen versions.
- Every message but `hello` is exact inside its protocol version: extra or missing bytes close the
  connection. `hello` is O24's.

**The two cases:**
- **B newer than A.** B's line: `origin: sopen on A (<ver>) speaks origin protocol 1–1, this sopen
  2–3 — update sopen on A`.
- **B older than A.** The agent still speaks proto 1 while its range holds 1.

**`a_version` and the agent's own age.** It is the agent's own sopen version. An agent left running
across an upgrade reports the old one. `sopen allow` with no host shows both versions (§10).

### 5.5 What B may ask, in all

| Request | Served when | Bound |
|---|---|---|
| stat, lstat, realpath | §6 passes | — |
| list (OPENDIR/READDIR) | §6 passes | 100 names a reply, hidden names out |
| read a range | an OPEN §6 passed, on its descriptor | 256 KiB a request; §8.4's handle caps |
| open on A's screen | §7 passes | 3 starts per 10 s, 8 running, per host (§8.4) |
| `hello` (`info`) | always | per-host connection cap |
| empty-agent requests | always | — |

## 6. What B may read (D3, D9, D13, O7, O27, O28)

### 6.1 Roots

- **The default** is A's home (D3). `sopen allow B --read DIR` (repeatable) sets the roots instead.
  Each DIR must be an existing folder, resolved when `sopen allow` runs, and stored absolute.
- **Held descriptors (O28).** At policy load (start, and each SIGHUP), every root is opened by a
  no-follow walk from `/`: `openat(O_NOFOLLOW | O_DIRECTORY | O_CLOEXEC)` per component. The agent
  holds that descriptor.
  - A root whose walk fails (a component became a link, or it is gone) is disabled and logged.
  - Roots are compared component by component as bytes: `/x/Music2` is not below `/x/Music`.

### 6.2 Hidden

1. **Dot names (D3).** Every name below a root that starts with `.`.
2. **`Library` (D9).** `Library` directly under home is hidden whenever home is at or below a root
   (home, or `/Users`, as roots), unless the root the path matched lies inside `~/Library`.
   - **Any spelling.** It is compared ASCII-case-insensitively, so `library` and `LIBRARY` count:
     macOS's default APFS is case-insensitive.
   - **The name on disk.** The name the filesystem reports for the descriptor finally opened is
     checked again (§6.3 step 4's last check).
   - **Why it is hidden.** It holds app tokens, keychains and sopen's own cache of third machines'
     files: `~/Library/Caches/ai.2lab.sopen` (`src/cache.rs:28-34`; directories 6.0.0
     `src/mac.rs:67`). That cache exists, with entries, on the Mac this document was written on
     [M, `ls`, 2026-11-25].
3. **sopen's own folders (O27).** A folder is hidden by device and inode when it is:
   - the agent's resolved cache or state folder;
   - a cache or state folder that `sopen allow` found in the owner's environment
     (`SOPEN_CACHE_DIR`, `SOPEN_STATE_DIR`, `src/cache.rs:29-37`);
   - **a folder any later sopen run used.** Every sopen run that resolves a cache or state folder
     other than the default (`cache::dirs`, `src/cache.rs:27-50`) registers it, once origin is set
     up. It finds the agent's folder through the pointer `~/.local/state/sopen/origin-at`, which
     `sopen allow` writes. It adds the folder's path, device and inode to `<that folder>/folders`
     and sends SIGHUP to the pid in `agent.pid`. From that reload on, the folder is hidden.

   Either variable can move a folder under a visible name, so the name alone cannot hide it.
   - **Residual: the window.** Between a run's first use of a new folder and the agent's reload,
     milliseconds apart, B could read what that folder holds if it lies under a root.
   - **Residual: older folders.** A folder used only before `sopen allow` ran, and never again, was
     never registered.
4. **Non-local objects (D13).** These are refused always. They make A fetch a third host's data for
   B, and they can block a worker without bound.
   - **Network and FUSE mounts.**
     - On macOS, a mount whose `MNT_LOCAL` flag is off (`getmntinfo(MNT_NOWAIT)`).
     - On Linux, a mount whose type in `/proc/self/mountinfo` is on a deny-list: nfs, nfs4, cifs,
       smb3, smbfs, 9p, afs, ceph, glusterfs, lustre, ocfs2, gfs2, fuse and every `fuse.*`.
       The list is the unit's, against `linux/magic.h` [추정].
     - The table is read without touching the mounts and read again when it changes: `poll` on
       `/proc/self/mountinfo` on Linux, at most once a second on macOS.
   - **Dataless files** (`SF_DATALESS` in `st_flags`, an evicted cloud file on macOS).
   - Whether the table read, the `fstatat` and the open stay off a hung network mount or an evicted
     file is [추정]. OR-0 (e) measures it.

### 6.3 One walk for every request (O28)

Every path-bearing request (REALPATH, STAT, LSTAT, OPEN, OPENDIR) runs these steps. So does an
`open` of kind 0 (§7).

1. **Form.** A path holding a NUL is refused. "", "." and a relative path are joined to A's home.
2. **As asked.** The path is split on `/`, and empty and `.` components are dropped.
   - A `..` component → refused (`a path with .. is not served`).
   - Any other component starting with `.` → refused (`a name starting with "."`).
   - D9's `Library`, in any case → refused.
3. **Lexically.** No filesystem call is made in this step.
   - The joined path must be a root or lie below one. Otherwise → refused (`not under what B may
     read: <reads>`).
   - **A path under a root but at or below a non-local mount** (D13's table) → `NO_SUCH_FILE`,
     step 5's one answer, so B cannot tell where mounts are.
   - Refusals in steps 1–3 name their reason, except that mount answer. They say something about
     the request, nothing about A's disk.
4. **The walk.** It starts from the matched root's held descriptor and goes component by component.
   - **Each name:** `fstatat(dir, name, AT_SYMLINK_NOFOLLOW)`. `SF_DATALESS` → stop (D13).
   - **A link:** `readlinkat`. Its target, made absolute from the walked folder's path, must pass
     steps 2–3. The walked path holds no links, so `..` in a target is exact when resolved
     lexically.
     - Then the walk starts again from that root's descriptor, with the target and the rest of the
       path.
     - At most 40 links. The agent never touches a link's target before that check.
   - **A folder:** `openat(O_NOFOLLOW | O_DIRECTORY | O_CLOEXEC)`. Then come three checks:
     - `fstat`: the same device and inode as the `fstatat` (else `changed`);
     - `fstatfs`: a local filesystem (D13);
     - O27's device and inode set: not one of sopen's own folders.
   - **The last name, per request:**
     - LSTAT: that `fstatat`.
     - STAT: the same, after a link is followed by the link rule above.
     - OPENDIR: `openat(O_NOFOLLOW | O_DIRECTORY)` and the folder checks.
     - OPEN: the `fstatat` must say a regular file. Then
       `openat(O_RDONLY | O_NOFOLLOW | O_NONBLOCK | O_CLOEXEC)`, and `fstat` must give the same
       device and inode with `st_nlink == 1` (O7). `fstatfs` must say local. Then `O_NONBLOCK` is
       cleared.
     - REALPATH: the path the walk built.
   - **The last check.** The kernel's own name for the final descriptor must pass steps 2–3 again:
     `F_GETPATH` on macOS, `/proc/self/fd/<n>` on Linux. That covers case spellings and names
     the walk could not see [추정 that `F_GETPATH` reports the stored case; OR-0 (e)].
5. **One answer.** Every refusal after step 3 is answered `SSH_FX_NO_SUCH_FILE "no such file under
   what B may read"`, exactly as a missing file is. That covers:
   - a link out of the roots, or to a hidden name;
   - a swap (`changed`), a FIFO, a device or a socket;
   - a second link, a mount, a dataless file, one of sopen's own folders.

   So the walk tells B nothing about what lies behind a link or a hidden name. The agent log keeps
   the real reason (§8.5).

**Hard links (O7).** The link count is checked on the descriptor that reads are served from.
- **Why.** A second name is invisible to a path filter.
- **The cost.** A file hard-linked between two readable places is refused too. In a home this is
  rare. git's local clones, pnpm stores and backup trees put their links under dot paths, which are
  hidden anyway [추정].
- Folders are not checked: their link count counts subfolders.

**Listings.** A READDIR reply leaves out:
- names that fail step 2 (`.ssh`, `.config`, `.`, `..`, and `Library` in a macOS home, any case);
- names at a non-local mount point, found in D13's table before any `fstatat` would touch the
  mount;
- entries whose device and inode are O27's.

Each entry's attributes are its own (`lstat`), so a link stays a link.

**Fails safe.** The lexical check compares bytes. On a case-insensitive APFS, `~/music` is refused
under a root spelled `~/Music`, with the line naming the roots. It is never served wrongly.

## 7. Opening on A's screen (D4, D11, D14, D16, O6, O29)

**B's side, `sopen --target origin SOURCE`.** B sorts the source before it connects, by 01 §4a's
forms:

| SOURCE on B | Sent as | Why |
|---|---|---|
| `origin:PATH` | kind 0, `PATH` as typed (relative = A's home) | A's own file, checked by §6 on A |
| a local path, or a loopback `host:path` (01 §4a's rule) | kind 1, its absolute path (`LocalFs::realpath`, as `src/forward.rs:236-239`) | B's own file. A names it `<you>:<abs>` itself, so the A→B direction that exists carries it (*origin* §4). `src/forward.rs:244-248` names B by B's host name, which A may not know. |
| `B:path` | refused on B | A builds the B name itself, so a user or another account on B never appears |
| another `host:path` | refused on B: `origin opens its own files and this machine's; open other:path on origin from origin itself` | A would connect to a third machine for B (ssot item 4) |
| `http(s)://…` | refused on B: `an address does not open on origin` (O14) | A's browser would fetch from A's network for B |
| with `--app` | usage error, exit 2 | D4 |
| `me@origin:…` | usage error, exit 2: `origin takes no user name` | O12 |

**A's checks.** The agent checks again, whatever B's sopen did, and answers a refused `open` at once
with code 1 and its line.
- **Kind 0** runs the whole of §6: every step, O7's link count, D13 and O27. The final object may be
  a file (OPEN) or a folder (OPENDIR). After step 3 every refusal is `no such file under what B may
  read`.
- **Kind 1** must be an absolute path with no NUL. A relative path → code 1. The agent writes
  `<you>:<path>`, taking `<you>` from the policy, never from B.
- **Flags** other than bit 0, or a non-empty `state` (O25) → code 1.
- **Then** §8.4's open limits.

**Kind 0: A's sopen never resolves the path itself.** The agent checked the path, but a link swapped
after that check would let a fresh open by path reach a forbidden file. So A's sopen reads through
the same constrained service as B does:
- **The served socket.** The agent opens `<state>/origin/s<6 random>` for this child. It serves
  B's policy over the same SFTP (§5.3) with no bind phase, since the verdict is B's.
  - It accepts only connections whose peer process id (found as in O26) is that child.
  - **It is closed before the child is reaped.** The agent waits for the child's exit with
    `waitid(…, WNOWAIT)`. It then closes and unlinks the listener and ends its connections, and
    only then reaps the child. A zombie keeps its pid, so no new process can take the child's pid
    and pass the check.
- **The child** runs `sopen --here --from-origin <you> -- origin:<q>` with
  `SOPEN_ORIGIN_SERVED=<that socket>`, where `q` is the path the walk built. Its `origin:` client
  (§9) uses the socket instead of discovery.
- **Every read goes through §6 again**, on the object it opens then: the file, its folder's
  listing, the preview, the tree. A link swapped after the agent's check is refused at the child's
  own OPEN.
- **On A's screen** the window shows `q`, A's own path, not `origin:`.

**Kind 1: B's file, held to the pin (D16).** A's sopen reaches `<you>` with its own SFTP. Every ssh
it starts for this run carries:
- `-o UserKnownHostsFile=<state>/origin/<you>.known_hosts`. It holds one line,
  `sopen-origin-<you> <the pinned key>`.
  - **The pin is a plain key.** When B's bind carries a host certificate, the pin is the public key
    inside it, with the certificate stripped. The acceptor compares that same plain key (§5.1).
- `-o GlobalKnownHostsFile=/dev/null -o KnownHostsCommand=none -o HostKeyAlias=sopen-origin-<you>`.
- `-o StrictHostKeyChecking=yes -o CheckHostIP=no -o UpdateHostKeys=no`.
- **`-o VerifyHostKeyDNS=no`.** An inherited `VerifyHostKeyDNS yes` would let SSHFP records in DNS
  vouch for a key.
- **`-o PermitLocalCommand=no`.** No `LocalCommand` of the owner's runs for B.
- **The control path `-S <state>/o<h><g>-%C`**, with `ControlMaster=auto`.
  - `<h>` is the host's one-character index in the policy, and `<g>` its pin generation, both from
    `[0-9a-z]`. So a master made under an older pin is never reused.
  - `--repin` also ends the host's old masters before the new pin takes effect (§10.2 step 6).
  - The name is short because ssh binds `<ControlPath>.<16 random>` first. sopen's own rule allows
    103 − 17 bytes (`src/remote/sftp.rs:44-45`, enforced at `:573-577`).

**What D16 does not reach.** ssh itself may still run the owner's own `ProxyCommand` or `Match exec`
from A's config. Those are the owner's commands, not B's, and D16 leaves them as they are.

The options' meanings are in `V_10_3_P1:ssh_config.5`: HostKeyAlias `:1087`, GlobalKnownHostsFile
`:969`, KnownHostsCommand `:1342`, StrictHostKeyChecking `:2017`, UserKnownHostsFile `:2183` and
CheckHostIP `:573`. That the combination accepts only the pinned key is [추정]. OR-0 (g) measures it
with B's key swapped, and with an inherited `VerifyHostKeyDNS yes` and a forged SSHFP record.

**What A runs.** `<sopen> --here --from-origin <you> [--hex] -- <source>`.
- **The process.**
  - It runs in a new session (`setsid`), with stdin `/dev/null` and its working folder A's home.
  - stdout and stderr go to one pipe the worker reads, keeping the last 4 KiB.
  - Its environment is the agent's own. On Linux the agent adds the desktop session's variables,
    read from `systemctl --user show-environment`. That is the rule of `src/forward.rs:306-321`,
    written again in Rust in the agent; `forward.rs` is not edited.
- **`--here`**, because the owner's A has a `target` set (10 §7.1: `target = <the Linux laptop>`).
  Without it, A's sopen would forward the open straight back to B.
- **`--from-origin <you>`** (hidden) changes these things in that run:
  - **Routing (D11).** Text opens in the xtools file browser on its folder, the bar on the file and
    its viewer open. It never opens in VS Code or Zed (`src/main.rs:678-685` routes text to the
    editor). This is what `SOPEN_STATE=open=NAME` does today: the hook sets `state.open`
    (`src/gui/mod.rs:310`, `:413`), and the browser enters that entry
    (`src/gui/browser.rs:1466-1470`). `--from-origin` sets it from `run_open`.
  - **No hand-off (D11).** No editor, F4 included; no `--app`; no browser for an address; no `open`
    or `xdg-open` of anything. F4 says `not in a window opened from B`.
  - **No git (D14).** The git panel is not offered and no git runs. Local git would run config that
    B could influence (`core.fsmonitor`, hooks).
  - **The F-key switch is off**, as for a run a hook drives (01 §4a). So no `--fkeys-watchdog`
    child starts.
  - **The title (O29)** begins `[from <you>] `, before anything B chose, so a long name cannot push
    it out of sight. In the part B chose (the file name, its path), bidi controls (U+202A–U+202E,
    U+2066–U+2069, U+200E, U+200F, U+061C) and C0 and C1 controls become U+FFFD. The title is built
    at `src/gui/mod.rs:1305`.
- **The processes started for B (D14).** "For B" means without the owner's own input on A.
  - The agent starts one, this `sopen`.
  - That `sopen` may start exactly these: the viewers' `ffmpeg` and `ffprobe`, and, for kind-1 opens
    only, A's own `ssh` to the pinned B under D16's options. That ssh may itself run the owner's
    own `ProxyCommand` or `Match exec` from A's config: the owner's commands, not B's.
  - It starts no git, no editor, no browser, no `open` or `xdg-open`, and no F-key watchdog.
- **Images, media, folders and the binary dialog** are sopen's own windows, as today.

**The end.**
- The worker waits for the child, then sends `ended` with its status and last line, then closes.
- B leaves with A's code, as 01 §4a's forward does: 0, 1 or 3 as A's sopen meant them; 2 as a
  usage error from A's sopen; a signal as exit 1 with `sopen on origin ended by signal n`.
- **B ending first** (Ctrl+C, a dropped session) closes the connection only. A's window stays: B
  cannot close A's windows, and unsaved edits there are never dropped (01 A11). B exits 130 on
  Ctrl+C.

## 8. The agent on A (D5, D8, D10, D17, O11)

### 8.1 Who starts it

**The service (D10).** `sopen allow` installs a user service that runs `<sopen> --origin-agent`,
keeps it alive, and starts it at login. That process is the supervisor (§8.6). It starts the agent
proper, which creates and owns its sockets (D8): there is no socket activation.
- **macOS:** `~/Library/LaunchAgents/ai.2lab.sopen.origin.plist`.
  - It sets `RunAtLoad` and `KeepAlive` to true, and `ProcessType` to `Interactive`, so launchd does
    not throttle the windows it starts [추정].
  - It is loaded with `launchctl bootstrap gui/<uid>` and unloaded with `launchctl bootout
    gui/<uid>/ai.2lab.sopen.origin`.
- **Linux:** `$XDG_CONFIG_HOME/systemd/user/sopen-origin.service` (else `~/.config/systemd/user/`).
  - It sets `Restart=always`, `RestartSec=1`, `WatchdogSec=5`, `WatchdogSignal=SIGKILL`,
    `KillMode=control-group` and `WantedBy=default.target`.
  - It is set up with `systemctl --user daemon-reload` and `enable --now`.
- **No user service manager** (no `launchctl`, or `systemctl --user` fails) → `sopen allow` refuses
  (§11).

**`<sopen>`.** It is the `sopen` found on PATH when that is the running binary, else `current_exe()`.
So a Homebrew upgrade keeps the bin link the service names [추정: brew keeps `bin/sopen` as a link
across upgrades]. The service also carries `SOPEN_STATE_DIR` when it was set at allow time.

**Upgrades.** An upgrade leaves the old agent running until it restarts. `sopen allow B` restarts the
service whenever it writes (§10.2).

**OR-0 measures:**
- whether a LaunchAgent's child can open a window on the owner's screen, and whether it takes
  focus (b) [추정];
- what macOS privacy consent (TCC) does to the agent reading `~/Documents` or `~/Desktop` (b)
  [owner].

### 8.2 Sockets and files

**The folder.** `<state>/origin/`, made private as the state folder is: 0700, owned by this account,
checked through a descriptor (`src/remote/sftp.rs:603-666`).

**Per host, the socket `<state>/origin/<B>.sock`.**
- **Binding it (O30).** It is never unlinked first.
  - The agent binds `<state>/origin/.n<6 random>` in the same folder with umask 0177, so 0600.
    Backlog 64. The name is short so it is never the longest one bound (§10.1).
  - Then `rename(2)` moves it over `<B>.sock`.
  - An `ssh B` that connects during a replacement reaches the old file (refused, if dead) or the new
    listener, and never meets a missing path [추정 for unix sockets; OP-14 tests it].
- **Before binding**, a connect to the existing `<B>.sock` that succeeds means another agent is
  live. This one then exits with `origin agent already running`.
- **Every accepted peer's uid** must be the agent's own (`peer_cred`). Otherwise the connection is
  closed with no reply.

**On stop, the socket files stay (O11).** SIGTERM or logout closes the listeners and leaves the
files.
- **Why.** A missing `ForwardAgent` path makes every `ssh B` print `Cannot forward agent socket
  path` (`V_10_3_P1:ssh.c:1516-1518`). Under the owner's `ExitOnForwardFailure yes` it ends with 255
  (`:1519-1520`): the owner would be locked out of B by a stopped agent.
- **A stale socket fails closed instead.** B's connection is reset and the owner's session goes on
  [M *origin* §2.2: "Dead or missing target → B gets Connection reset by peer"].
- **Under `ExitOnForwardFailure yes`**, a runtime failure to reach the agent does not end the
  session [추정]. The option is read at startup (`ssh.c:1519-1520`), and the channel's failure path
  (`clientloop.c:1850-1854`) only returns no channel. OL-3 measures it.
- **Unlinked only by `--remove` and `--off`**, after the line naming the socket is gone from ssh's
  config (§10.3).

**Crash and reboot.**
- **A crash** (SIGKILL, panic) leaves the same stale socket. The supervisor starts a new agent
  (§8.6), which replaces the socket by O30's rename.
- **A reboot** keeps the files (`<state>` is on disk). Until login starts the service, `ssh B`
  connects and B's origin is reset.

**Other files in `<state>/origin/`, all 0600:**
- `hosts.toml`, the policy: hosts, pins, roots, the checked ssh clients (O26), sopen's folders
  (O27), `a_id`;
- `ssh_config`, the included file;
- `<B>.known_hosts`, the pin as a known-hosts line (D16);
- `h`, the health socket (§8.6);
- `s<6>`, one served socket per running open of kind 0 (§7);
- `p<6>`, the probe's socket while `sopen allow` runs (§10.2);
- `.n<6>`, a socket being bound (O30);
- `agent.pid`, the supervisor's pid, and `folders`, the folders later runs registered (§6.2
  item 3);
- outside this folder, `~/.local/state/sopen/origin-at`: one line naming this folder, for runs
  under another `SOPEN_STATE_DIR` (§6.2 item 3);
- outside this folder, `<state>/o<h><g>-%C`: D16's ssh masters (§7);
- `agent.log`: one line per connection and per file opened, kept to 1 MiB plus one rotated file;
- `agent.err`: the service's stderr;
- `.lock`: `sopen allow`'s lock (§10.1).

### 8.3 The acceptor: the thread ssh waits on (D5)

**It does one thing.** One thread runs a current-thread runtime with every listener and the health
socket, and does nothing else. Per accepted connection it:
1. checks the peer's uid;
2. reads the first message, from many connections at once, each with a 1 s deadline;
3. parses the bind and compares the host key with the host's pin in memory;
4. writes the 5-byte reply to a bind. A first message that is no bind gets no reply here: it is B's
   (§5.1), and goes on with the connection;
5. hands the connection and its verdict to a worker. When §8.4's connection cap is full, it hands it
   to the refuser. When the refuser is full too, it closes the connection.

It also answers one-byte pings on the health socket `h` (§8.6), on the same thread, so a stalled
acceptor fails its health check.

**What it never touches.** No disk, no lock, no log write, and nothing a worker holds.
- Counters are atomics.
- The pins and caps reach it as messages on its own channel, applied between accepts.
- Log events go out with a non-blocking send, and are dropped when the channel is full.

**It always has a descriptor to spare (O31).** It keeps one descriptor open on `/dev/null`.
- On `EMFILE` or `ENFILE` at accept, it closes the spare, accepts, closes that connection at once,
  and opens the spare again.
- A's ssh then reads EOF instead of waiting for a reply that cannot come. The bind fails, which is
  only a debug line (`clientloop.c:1855-1859`), and B's `hello` sees the end.

**A panic on the acceptor aborts the whole process (O32).** The supervisor then starts a new one
(§8.6). An acceptor that died while its listeners stayed open would freeze every new connection.

**The number.** A bind is answered within 10 ms of its arrival, with 64 silent connections pending
on the same socket and every worker busy reading (OP-6), and with every handle held (OP-13). The
measured cost of a stall is the owner's session frozen for its length (3.01 s and 6.26 s,
*origin* §2.2).

**A silent local client** (it connected and sends nothing) holds no other connection up. It is
closed at its 1 s deadline. More than 64 pending connections on a socket → the newest is closed at
accept.

### 8.4 Workers and limits (O8)

**Workers.**
- One thread per connection, blocking I/O.
- A worker runs O26's peer check (and, after an upgrade, its one `-V`), then serves `hello`, SFTP (§5.3), `open` (§7) and the empty-agent
  answers.
- A panic in a worker closes its connection only.
- Workers read the policy through a read-write lock. A reload swaps it, and each request uses the
  current roots.
- **The refuser** is one thread for every connection over the cap.
  - It reads B's first message with a 1 s deadline.
  - It answers a `hello` with code 1, `too many connections from B (<cap> open)`, and an agent
    request with 5.
  - Then it closes the connection.
  - It holds at most 64 connections. Past that, the acceptor closes new ones unanswered.

**The descriptor budget (O31).** One budget covers every descriptor the agent can hold at once, so
`EMFILE` cannot happen and the spare (§8.3) is only a last resort.
- **The limit.** At start, the agent raises its soft `RLIMIT_NOFILE` to the hard limit. On macOS
  that means the lower of the hard limit and `kern.maxfilesperproc`, since an unlimited value is
  refused [추정]. launchd's default soft limit is 256 and systemd's is 1024 [추정; OR-0 (h) reads
  both]. The raised limit is `F`.
- **The fixed part**, with `H` the number of hosts: `R = 144 + 34·H`.
  - 8 for the agent itself: the health socket, the spare, the log, `agent.err`, the policy being
    read, the mount table, and two for a reload.
  - 64 for pending first messages, and 64 for the refuser's connections.
  - 8 for one `<path> -V` check at a time (its pipes; O26).
  - Per host:
    - 1 listener;
    - 16 root descriptors (at most 16 roots, §10.1);
    - for each of its at most 8 running opens, the served listener and the child's pipe: 16;
    - 1 for O30's rebinding.
- **Each connection** costs 4: its own descriptor, plus at most 3 a walk holds at once (the folder,
  the next one, the final object).
  - The served sockets' connections from A's children are connections like any other: they count
    in `C` and in B's per-host share.
- **The pools.**
  - Connections: `C = min(128, (F − R) / 16)` in all, and `min(32, C)` per host.
  - Handles: `P = F − R − 4·C`.
  - So `R + 4·C + P = F`: every descriptor has a place.
- **The floor.** The agent needs `C ≥ 8`, so `F ≥ R + 128`, which leaves `P ≥ 96`. Below that it
  does not start (`the descriptor limit <F> is too low for origin with <H> hosts (needs <R + 128>)`),
  and the sockets fail closed (O11). No pool is ever negative.
- **Over a pool, at once.** An OPEN or OPENDIR beyond `P` fails with `SSH_FX_FAILURE "origin is out
  of file handles"`, as D5 asks. It is never queued.

**Caps.**

| What | Cap | Over the cap |
|---|---|---|
| connections at once | `C` in all; `min(32, C)` per host, the served sockets' included | the refuser (above) |
| pending first messages | 64 in all, 1 s each | closed |
| the refuser's connections | 64 | closed unanswered |
| handles (files and folders open) | the pool `P`; at most 64 per connection | `SSH_FX_FAILURE "origin is out of file handles"` |
| one READ | 256 KiB | shortened |
| one incoming SFTP packet | 64 KiB (O23) | the connection is closed |
| opens on A's screen | 3 started in any 10 s, 8 running, per host | code 1: `3 opens from B in the last 10 s` / `8 windows from B are open` |
| served sockets (§7) | one per running open, so at most 8 per host | — |
| roots | 16 per host | `sopen allow` refuses more |

**Why 32 per host.** 06 §6 counts what one browser window holds on a host: listing 1, viewer 1–2,
preview 1, tree 1, and 1 per file operation. Git holds none on origin (D14, O17). That is about 6 a
window, so 32 leaves room for several windows and CLI reads at once.

**No byte cap (O8a).**
- One agent channel moves at most 1.1–2.0 MB/s, 64 KiB in flight [M *origin* §2.2], so the
  connection cap bounds a host's reads.
- A byte cap would only break a long video while protecting nothing the roots do not already grant.

**Floods.**
- **What the agent bounds.** Its threads, descriptors and memory, by the caps above. The acceptor
  never waits on a worker, the refuser or a descriptor, so every bind is answered or the connection
  closed within OP-6's bound.
- **What it cannot bound.** The cost inside A's ssh. Each connection B opens costs A's ssh one
  synchronous local round trip (`clientloop.c:1855`), even when the agent closes it at once.
  - B can therefore slow the owner's session by connecting in a loop. Its worst case is A's ssh
    doing nothing but those round trips, each well under a millisecond on a healthy agent [추정;
    OR-0 (c) times a loop of 1 000].
  - The owner ends it with `--off` or by ending the session (§12).

### 8.5 Reload, concurrent sessions, the log

**Reload.** On SIGHUP, which `sopen allow` sends after each change, the agent rereads the policy.
- A new host gets its socket (O30).
- A removed host's socket is closed, its connections ended and its file unlinked.
- The roots' descriptors are opened again by the no-follow walk (§6.1). D13's table and O27's set
  are read again.
- Changed roots apply to the next request.

**Concurrent sessions.** Every concurrent `ssh B` has its own socket on B, and all of them reach the
same A socket [M *origin* §2.2: two sessions, two sockets, both answer]. They share B's caps. Two
hosts have two sockets and two pins.

**The log.** One line per connection (time, host, mode, verdict, the real reason of every refusal,
the peer's executable) and one per file closed (path, bytes read). It answers "what did B read"
after the fact. It sits under `<state>`, a dot path and one of O27's folders, so B never reads it.

### 8.6 The supervisor (D17)

**Why.** A stall of the whole agent process freezes the owner's session [M 6.26 s]. A bug, SIGSTOP
or a kernel wait can all cause one, and nothing inside the stalled process can end it. So a process
outside it does.

**What it does.** `<sopen> --origin-agent`, the service's process, starts the agent as its child,
`<sopen> --origin-agent --serve`. Then it does only this:
1. **Probes.** Every 500 ms it connects to the health socket `h` and sends one byte. The acceptor
   thread answers it (§8.3).
2. **Kills.** Two failed probes in a row (no answer within 500 ms each) → `SIGKILL` the child.
   SIGKILL ends a stopped process too.
3. **Restarts.** A dead child is started again at once. Five deaths within 60 s → it waits 60 s
   between starts, and logs why. Meanwhile the sockets fail closed (O11).
4. **Forwards** SIGTERM and SIGHUP to the child.

It holds no policy, no socket of B and no lock.

**The bound.**
- A stalled agent is killed within **2 s** of its last good answer (four probe periods at worst).
- Its listeners close with it, so A's ssh, blocked on a bind reply, reads EOF and goes on
  [추정; OR-0 (c) measures the owner's session output across a SIGSTOP].
- The new agent serves within 1 s more [추정].
- So a freeze of the owner's session is at most **about 3 s**, not unbounded. OP-12 tests it: the
  agent SIGSTOPped while a stand-in ssh waits on a bind.

**The supervisor's own stall.**
- **On Linux**, the supervisor sends `WATCHDOG=1` (`sd_notify`) after each good probe. With
  `WatchdogSec=5`, systemd kills the whole service with SIGKILL (`KillMode=control-group`) and
  starts it again (`Restart=always`). So even both stalling at once is bounded at about 6 s
  [추정; OP-12b].
- **On macOS**, launchd has no watchdog [추정]. A supervisor stalled alone freezes nothing: the
  agent still answers every bind.
- **A single stall** of the agent or of the supervisor is therefore bounded at about 3 s on both
  systems: the agent's by the kill, the supervisor's by having no effect on binds.
- **A double stall** is the accepted residual of §12: the supervisor and the agent stalled together,
  as in a whole-user or whole-system stall.

## 9. How B finds origin (O12, O13)

**The word.**
- `origin` with no user is the host word for the session's A. A host whose name is `origin` is
  reached by another alias.
- `me@origin` is a usage error (`src/forward.rs:77-95` gains the rule, as does the client).
- `sopen config target origin` is valid on B (01 §4a's `target`). Every `sopen x` on B then opens on
  A.
- **On A**, a child the agent started for a kind-0 open (§7) finds origin through
  `SOPEN_ORIGIN_SERVED`, its served socket, and through nothing else.

**Discovery, in order.** Each candidate gets a `hello` in mode `info`, 500 ms each.
1. **`SSH_AUTH_SOCK`.** B's sshd sets it for the session (`V_10_5_P1:session.c:1055`). B's sshd
   accepts no other variable from A [M *origin* §2.1: `SetEnv`/`SendEnv` arrive unset], so there is
   no sopen-specific variable.
2. **The scan, when `SSH_AUTH_SOCK` is unset, dead, or a real agent.** A typical case is a tmux
   pane started by an earlier session.
   - The candidates are the sockets of `~/.ssh/agent/s.*.sshd.*` (B's sshd 10.5 layout,
     `misc-agent.c:186-197`) and `/tmp/ssh-*/agent.*` (older sshd [추정]).
   - Only sockets owned by this uid count, newest first, at most 16 [M *origin* §2.2: the scan found
     the live socket].
3. **The pick.**
   - One `a_id` among the answers → use it.
   - Two different `a_id`s → refuse, naming both `a_name`s: `two machines answer as origin (X, Y) —
     set SSH_AUTH_SOCK to this session's socket`.
   - None → §11's line for what was found.

**`ssh -A B` once B is allowed [가설].** In the source it forwards **sopen's** agent, not the
owner's keys.
- `-A` sets the forward flag (`V_10_3_P1:ssh.c:850-851`). The included stanza's path value then
  still sets the path, which is only checked for being unset (`readconf.c:1258-1263`,
  `:2442-2443`). The forward uses the path (`clientloop.c:1845-1849`).
- `-o ForwardAgent=yes B` would do the same.
- If so, D2 holds even against `-A`, and the owner's only way to send the real agent to an allowed B
  is a path on the command line: `ssh -o ForwardAgent='$SSH_AUTH_SOCK' B` (`ssh.c:1778-1785`).
- This is read, not run. OR-0 (i) runs `ssh -G -A B` and `ssh -G -o ForwardAgent=yes B`, and §10.2
  step 11 runs both on every allow and says which holds.
- If the real agent is forwarded instead, B's `SSH_AUTH_SOCK` is that agent, and the scan (step 2)
  finds origin only when another session to B forwards it.

**The process pin.** The first `a_id` a process reaches is kept. Every later connection of that
process — the tree's, the preview's, a reconnect after a dropped session (`src/main.rs:419-422`) —
must reach the same `a_id`. Otherwise: `origin is now another machine (Y, not X)`.
- **The cache key** stays `origin` (`RemoteSpec::label`, `src/target.rs:32-43`).
  - Two A's share it across processes.
  - Nothing is ever written back to origin, and a fetch moves a kept copy's edits aside before it
    replaces it (`src/main.rs:447-451`). So nothing is lost, and OR-client's test shows it.

## 10. `sopen allow` (D1, D6, D7, D10, D12, D15, O9, O18–O20)

### 10.1 The forms

| Command | Does |
|---|---|
| `sopen allow B` (new B) | adds B, probes and pins (§10.2); prints what it did |
| `sopen allow B` (B already allowed) | checks again: the probe runs, and the same key changes nothing. A different key → refused, unless `--repin` (D15) |
| `sopen allow B --repin` | probes and pins the key it finds |
| `sopen allow B --read DIR…` (B already allowed) | replaces B's roots only. **No probe**, no pin change (D15) |
| `sopen allow B --print` | prints what `sopen allow B` would write — the `Include` line, B's stanza, the service file — and changes nothing, running no ssh |
| `sopen allow --print` | prints the lines in force now |
| `sopen allow` | lists the hosts (roots, pin, date), the checked ssh clients, the agent's state and version against this sopen's, and the last 5 refusals from the log |
| `sopen allow --remove B` | removes B (§10.3) |
| `sopen allow --off` | removes every host and everything origin wrote (§10.3, O19) |

**Validating B.** B must be an alias of `[A-Za-z0-9._-]`, not starting with `-` or `.`, at most 63
bytes.
- **Not `origin`.** A's sopen would read `origin:/abs` as a channel of its own (§9).
- **Case.** Aliases are compared ASCII-case-insensitively, so `b` and `B` are one host. ssh matches
  `Host` patterns without case [추정], and the socket files would collide on a case-insensitive
  APFS.
- **Why so narrow.** ssh reads `*?!,` as patterns and splits on whitespace. ssh expands `%` and `${`
  in the `ForwardAgent` path (`V_10_3_P1:ssh.c:1509-1515`) and in `Include`, and globs `Include`
  (`readconf.c:2075-2105`).
- **The written paths too.** The socket path and the included file's path must hold none of
  whitespace, `"`, `#`, `%`, `$`, `*`, `?` or `[`.
- **Length (O34).** Every socket name origin will bind is checked, and the longest decides. Over
  the limit → `set SOPEN_STATE_DIR to a shorter folder`, as `src/remote/sftp.rs:573-577` says for
  sopen's own socket.
  - Bound by the agent, at most 103 bytes each (macOS's `sun_path`): `<state>/origin/<B>.sock`,
    `.n<6>`, `s<6>`, `p<6>` and `h`.
  - Bound by ssh for D16's masters: `<state>/o<h><g>-%C` with `%C` 40 hex characters, plus the 17
    bytes ssh adds when it binds `<ControlPath>.<16 random>` first (`src/remote/sftp.rs:44-45`).
    That is at most 103 − 17 bytes, the same as sopen's own `cm-%C`.
- **Roots.** At most 16 `--read` folders per host (O31's budget counts them).

**One at a time.** Every form that writes holds `<state>/origin/.lock` (`flock`). A second `sopen
allow` waits up to 5 s, then says `another sopen allow is running` and exits 1.

### 10.2 `sopen allow B`, in order

**Checks.** They change nothing; the first that fails stops the command.
1. **The alias**, by §10.1's rule.
2. **The agent ssh will use for B (D6).** `ssh -G B`'s `identityagent` decides it.
   - `SSH_AUTH_SOCK` (the default): that variable must be set **and connect**.
   - A path: that path must connect. ssh then sets `SSH_AUTH_SOCK` to it itself (`ssh.c:1772-1775`).
   - `none` → refused, since it unsets the agent (`ssh.c:1754-1758`).
   - ssh's own check connects too (`authfd.c:87-113`). Without an agent, ssh forwards nothing and
     says nothing [M *origin* §2.2].
3. **`ssh -G B`** must succeed, and then:
   - `forwardagent` must be `no`. On a re-run for an allowed B it is already B's socket path, which
     is accepted too.
   - When the owner already forwards an agent to B (`yes` or another path), the command refuses
     (O9). The `Include` goes first, so origin would replace that forward unasked and B would lose
     the owner's keys.
   - `controlmaster` `auto` or `yes` with a running master (`ssh -O check B`) → a note, not a stop:
     `your ssh master to B is running; origin starts with the next one (ssh -O exit B)`. A master's
     sessions take the master's forward (`V_10_3_P1:mux.c:1448-1449`).
4. **A user service manager** (§8.1).
5. **The ssh on PATH** must print an `ssh -V` of at least `OpenSSH_8.9`. This is only an early
   line. The checked client itself is the one the probe sees (step 6).

**The probe (D7, D12).** It runs for a new B, for a plain re-run, and for `--repin`. It never runs
for `--read` alone, and never for a service restart (D15).

6. sopen listens on a temporary `<state>/origin/p<6 random>`, and removes it on every exit.
   It runs `ssh -S none -o ControlMaster=no -o ForwardAgent=<probe> -o RemoteCommand=none -o
   LogLevel=ERROR -T B -- <words>`.
   - `<words>` come from a new `forward::check_words` (OR-allow). It is `words`
     (`src/forward.rs:370-393`) with the flag `--origin-check` and the SOURCE `origin:`.
   - The HOST script (`:284-333`) finds B's sopen as `--target` does, and runs `"$bin" --here
     --origin-check -- origin:` (`:330`).
   - On B, `--origin-check` with SOURCE `origin:` means: find origin as §9 does, send `hello` `info`,
     print the reply, exit 0.
   - **The probe server** answers the bind and records its host key, the candidate pin: the plain
     key, any certificate stripped (D16).
   - **It records the checked client (O26)** as the executable its peer actually is: the peer pid's
     executable, its device and inode. That may differ from the `ssh` on PATH, for instance behind
     a wrapper script. sopen then runs that executable's `-V`, which must be at least 8.9, and
     records it too.
   - Then it answers `hello` (mode `info`) from B's sopen.
   - **B's sopen** prints its version and protocol range and exits 0. It is not required: when the
     script exits 127 (no sopen on B), sopen runs `ssh … B -- ssh-add -l` the same way. Its
     `REQUEST_IDENTITIES` connection carries a bind, which gives the pin. The note is then `sopen is
     not installed on B; origin works once it is`.
   - **No bind by the end** → stop: `B did not open the agent channel (AllowAgentForwarding no, or
     DisableForwarding yes in B's sshd?)`. Both are options of B's sshd
     (`V_10_5_P1:sshd_config.5:100-108`, `:682`); `AllowAgentForwarding` defaults to `yes`.
   - **The owner may meet ssh's own prompts**, a passphrase or a host key, as with `ssh B`.
   - **A changed key on an allowed B (D15)** → stop, exit 1, files unchanged: `B's host key changed
     (SHA256:old → SHA256:new); if B was reinstalled: sopen allow B --repin`.
   - **`--repin` (O35).** The new key becomes the pin and the pin generation `<g>` goes up by one.
     Before the policy is written, every D16 master of B under the old generation is ended:
     `ssh -S <state>/o<h><g>-… -O exit` on each such socket, then the socket is removed. A master
     that will not end makes the command stop with the files unchanged.

**The snapshot (O33).** Before the first write, sopen keeps the exact bytes and modes of the
policy, the included file, B's known-hosts file, the service file and `~/.ssh/config`. A file that
was missing is kept as "missing".

**The writes**, each atomic: a temp file in the same folder, flushed, then renamed (O20). The
agent comes up before any line names its socket (O11).
7. **The policy** gets B's entry: socket, pin, roots, date, the checked client. Also sopen's own
   folders (O27, from this shell's environment), `a_id` on first use, the host's index `<h>` and pin
   generation `<g>` (D16), and whether `~/.ssh/config` was created by sopen. The pointer `origin-at`
   is written (§6.2 item 3). **`<state>/origin/<B>.known_hosts`** gets the pin as one known-hosts line
   (D16).
8. **The service** is written or kept, then (re)started (§8.1). The agent binds B's socket from the
   policy. Within 2 s, the health socket `h` must answer a ping, and B's socket must accept a
   connect, which sopen closes at once without writing a bind. sopen never poses as A's ssh: it is
   not a checked client. When either fails → the undo below.
9. **The included file** is written whole, 0600, with one stanza per host:
   ```
   Host B
     ForwardAgent <absolute path of <state>/origin/B.sock>
   ```
   - **Why 0600.** An included file writable by group or others is fatal for **every** ssh
     (`readconf.c:2635-2642`; included files are opened with `SSHCONF_CHECKPERM`, `:2125`).
   - **`Host B` matches every account.** `ssh root@B` gets origin as `ssh B` does, and the output
     says so.
10. **`~/.ssh/config`** gets `Include <absolute path of the included file>` as its first line, when
    no line already says exactly that.
    - **The file.** A missing file is created 0600, with `~/.ssh` 0700. An existing file keeps its
      mode. A symlinked one keeps its link: the link's target is replaced, as `src/config.rs:145-191`
      does for sopen's config.
    - **Why first.** For each option, the first value ssh reads wins. *origin* §2.2 measured that
      only `forwardagent` changes. It did so with `ssh -F` on a composed file — sopen's file, then
      the owner's config, then `/etc/ssh/ssh_config` — not on the real `~/.ssh/config`. OA-4 and
      OL-1 measure the real layout.
    - **The include's scope.** It ends with its file: the `Host` state is restored after it
      (`readconf.c:2140`).

**The verification (O18).**
11. `ssh -G B` again. It may differ from step 3's output in the `forwardagent` line only, and that
    line must now be B's socket path. On a re-run it was that already, so nothing differs.
    - `ssh -G <a host sopen made up>` must not differ at all.
    - `ssh -G -A B` and `ssh -G -o ForwardAgent=yes B` are run and recorded. The output says which
      agent `-A` would forward (§9).

**The undo.** It runs on any other difference, a failure, a fatal permission error, or an agent
that does not answer at step 8.
- Every file of the snapshot is put back exactly: its bytes and mode, or removed if it was missing.
- Then a SIGHUP, or the service as it was.
- A socket is unlinked only if the restored included file does not name it. So a failed re-run
  leaves the old B working.
- `~/.ssh/config` is then byte for byte as before, and the command says what differed.

**The output** (A's terminal):
```
origin: from an `ssh B` session, B can now use this machine as `origin`.
  B reads (read-only): ~   (names starting with "." left out; on macOS ~/Library too)
  B's host key: SHA256:…   (pinned; a different key is refused until sopen allow B --repin)
  every account on B reached as ssh [user@]B gets origin
  ssh -A B forwards: sopen's agent (your keys stay here)   [or: your agent — origin is not used then]
  ~/.ssh/config line 1: Include <state>/origin/ssh_config   (it holds `Host B / ForwardAgent …`)
  service: ~/Library/LaunchAgents/ai.2lab.sopen.origin.plist   (starts the agent at login)
  agent: running (launchd ai.2lab.sopen.origin, sopen <ver>)
  B's sopen: <ver>, origin protocol 1–1
New ssh B sessions get origin; open ones keep what they had.
Undo: sopen allow --remove B    everything: sopen allow --off
```

**When the owner's agent goes away later.** Step 2 checks the agent at allow time, and ssh checks it
at each `ssh B`. A terminal without `SSH_AUTH_SOCK` later forwards nothing, and B says so (§11).

### 10.3 `--remove` and `--off`, in order

Both hold the lock. The line that names a path always goes before the path, so no `ssh B` can meet
a named but missing socket (O11).

**`--remove B`:**
1. B's stanza leaves the included file. New `ssh B` sessions then stop forwarding to B's socket.
2. When no host is left: the `Include` line leaves `~/.ssh/config` (an exact line match, wherever it
   now is), and the included file is removed. If sopen created `~/.ssh/config` and it now holds only
   that line, the file is removed too.
3. B's policy entry and `<B>.known_hosts` are removed.
4. B's socket goes:
   - with the agent running: a SIGHUP, and the agent closes B's socket, ends its connections and
     unlinks the file (§8.5);
   - with the agent down: sopen unlinks the file itself.
5. With no host left: the service is stopped and its file removed, and so are the leftovers
   (`p*`, `s*` and `h`).

Live sessions then lose origin: their next connection is reset. Nothing was ever installed on B, so
there is nothing to undo there.

**`--off`** is `--remove` for every host at once. It also removes the policy with `a_id`,
`folders`, `agent.pid`, the `.lock`, `agent.err` and the pointer `origin-at`, and ends D16's masters
(`o*`).
- **It keeps `agent.log`**, the record of what B read, and says where it is.
- Enabling again is `sopen allow B`, which probes again and makes a new `a_id`.

## 11. The lines the owner sees

**On A, from `sopen allow`** (each line ends the run with exit 1, unless it says note):

| When | Line |
|---|---|
| no `SSH_AUTH_SOCK` | `sopen allow: this terminal has no ssh agent (SSH_AUTH_SOCK is not set), so ssh would forward nothing to B — start one (eval "$(ssh-agent)") or use a terminal that has one` |
| it does not connect | `sopen allow: SSH_AUTH_SOCK=<path> does not answer, so ssh would forward nothing to B` |
| the owner forwards an agent to B | `ssh B already forwards your agent (ForwardAgent <value>); origin would replace it and B would lose your keys — remove that setting for B first` |
| `IdentityAgent none` | `ssh's config for B says IdentityAgent none, which leaves ssh without an agent, so nothing would be forwarded — remove it for B` |
| `ssh -G B` fails | `ssh cannot read the config for B: <ssh's line>` |
| a bad alias or path | `B: an alias for origin holds letters, digits, '.', '_' and '-' only` / `the socket path <p> is N bytes, over 103 — set SOPEN_STATE_DIR to a shorter folder` / `<p> holds '<c>', which ssh would expand` |
| no service manager | `no user service manager here to keep the origin agent running (launchd, or systemd --user)` |
| B unreachable | `could not reach B: <ssh's line>` |
| no bind in the probe | `B did not open the agent channel (AllowAgentForwarding no, or DisableForwarding yes in B's sshd?)` |
| no sopen on B (note) | `note: sopen is not installed on B; origin works once it is (brew install 2lab-ai/tap/sopen-preview)` |
| verification failed | `ssh -G B changed more than ForwardAgent (<the lines>): undone, ~/.ssh/config is as it was` |
| the agent did not answer | `the origin agent did not start (<state>/origin/agent.err: <its last line>): undone` |
| B's key changed (D15) | `B's host key changed (SHA256:old → SHA256:new); if B was reinstalled: sopen allow B --repin` |
| ssh too old or not found (O26) | `the ssh on PATH (<path>) is <version>; origin needs OpenSSH 8.9 or later, which sends a session binding` |
| another allow running | `another sopen allow is running` |

**On A, otherwise.**
- The agent prints nothing to the owner. `sopen allow` lists its state and last refusals, and
  `agent.log` holds the rest, real reasons included (§6.3 step 5).
- A window B opened says `[from B]` at the start of its title (§7, O29).
- ssh's own `Cannot forward agent socket path` cannot appear in sopen's order of operations (O11).
  Seeing it means a socket file was deleted by hand.

**On B** (exit 1 unless said). Each line comes through unchanged: `refusal_said`
(`src/main.rs:438-445`) rewrites only sshd's own session refusal.

| When | Line |
|---|---|
| no socket at all | `origin: this shell has no forwarded agent (SSH_AUTH_SOCK is not set and no session socket answers) — on the machine you came from: sopen allow <its alias for this machine>, then ssh in again` |
| only real agents answer | `origin: the agent in this session is an ssh-agent, not sopen's — it was forwarded by hand (ssh -o ForwardAgent=…), or the machine you came from has not run sopen allow for this one` (whether plain `-A` gives this case is §9's [가설]) |
| reset or closed before a reply | `origin: the machine you came from closed the channel (its origin agent is off, too busy, or this machine was removed: sopen allow there)` |
| over the connection cap | `origin refused: too many connections from B (<cap> open)` |
| pin mismatch | `origin: refused — this machine's host key SHA256:x is not the one pinned for B (SHA256:y); if B was reinstalled, run sopen allow B --repin there` |
| `is_forwarding` = 0, onward hop | `origin: refused — this connection is not B's own forwarded agent (an ssh -A hop from B?)` |
| a peer that is not a checked client (O26) | §5.1's line |
| no bind | §5.1's line |
| no common version | §5.4's line |
| two A's | §9's line |
| a path refused as asked (§6.3 steps 1–3) | the SFTP error with its reason, e.g. `cannot stat ~/.ssh/id_ed25519: Permission denied (a name starting with ".")` |
| a path refused after that, or missing | `… no such file under what B may read` — one answer for all of them |
| out of handles | `… origin is out of file handles` |
| a write | `… origin is read-only` |
| `--target origin` refusals | §7's table (usage errors exit 2) |
| a refused open | `origin refused the open: <line>` |
| A's sopen failed | `sopen on origin failed: <its last line>` (exit 1); by signal: `sopen on origin ended by signal n` |

## 12. Threat model

What each attacker can do through origin, and what bounds it on A. "Through origin" means through
the socket. Anything an attacker already had without origin is out of this table.

| Attacker | Can | Cannot | Bound |
|---|---|---|---|
| **B fully compromised**, the owner's uid on B | **read** every file §6 serves: the roots, minus hidden names, local single-name regular files. **List** those folders. **Open** sopen windows on A, 3 per 10 s and 8 at once, so A's decoders parse files of B's choosing (the same exposure as `sopen B:/file` from A today). **Slow** the owner's session by connection floods (§8.4). **Processes** (D14): without the owner's own input, the agent starts `sopen --here --from-origin`, and that sopen may start exactly the viewers' `ffmpeg`/`ffprobe` and, for kind-1 opens only, A's own `ssh` to the pinned B under D16's options (which may itself run the owner's own `ProxyCommand` or `Match exec`: A's config, not B's). Nothing else: no git, no editor, no browser, no `open`/`xdg-open`, no F-key watchdog | **a shell** or any other process on A; **key use** (the agent holds no keys and signs nothing, D2); **any write**; **any connection** from A to anything but the pinned B (§7, D16); **hidden names** through links, `..`, a swap, a hard link, a moved cache (O27), a mount or a dataless file (§6); **an oracle** for what exists behind a link or a hidden name (§6.3 step 5); **another host's socket** | §6, §7, §8.4; `--remove`, `--off` |
| **root on B** | the same, plus other users' session sockets on B, plus forging any bind B's own processes send | forging A's bind: A's checked ssh writes it on A before any B byte (O26), so B's root cannot choose the host key the agent checks or the A socket ssh chose (`clientloop.c:1845-1859`) | the same; O26 |
| **A second machine answering as B** (address or DNS swap; the owner's config skips host-key checks for B, *origin* §3) | A's ssh connects and forwards to it | pass the bind: its host key is not B's pin → every `hello` refused (§5.2); be reached by a kind-1 open: that ssh accepts only the pinned key (D16) | D7, D12, D16 |
| **B's bytes forging a bind** (no bind from A's side) | nothing: a peer that is not a checked ssh ≥ 8.9 gets an empty agent | pass the pin with B's public key | O26. **Where it cannot be claimed:** a local process with the owner's uid can write any bind. It owns A anyway |
| **B2, another allowed host** | its own socket, pin and roots | reach B1's socket: A's config names each socket by the alias the owner typed | per-host socket (D7) |
| **Replay** | nothing to replay: origin carries no token or secret | a recorded bind gains nothing: the bind comes from A's ssh on A; B's bytes start after it | — |
| **An onward hop** (`ssh -A C` from B) | C's ssh traffic reaches the agent after B's ssh binds for C | origin service: a bind in B's traffic makes the connection an empty agent (O5) | O5. A malicious B can still relay for C: that is B's own capability (row 1) |
| **A local process on A** with the owner's uid | everything, origin or not | — | socket 0600 in a 0700 folder; peer-uid check (§8.2) |
| **A stall or descriptor exhaustion in the agent** | freezes the owner's A→B session at every new connection from B [M 6.26 s for a stall] | last longer than §8.6's bound: about 3 s for a single stall, of the agent or of the supervisor, on both systems; descriptors never run out for the acceptor (O31) | §8.3, §8.4, §8.6; OP-6, OP-12, OP-13; a double stall is the residual below |
| **The owner's real agent** | — | reach B through sopen's stanza: the `ForwardAgent` path replaces `SSH_AUTH_SOCK` for the forward [M *origin* §2.2: 0 forwarded connections to a stand-in real agent]. Whether `ssh -A B` does the same is §9's [가설]; a command-line path is the owner's own act | D2, O9 |

**Accepted residual: the double stall (D17).** The supervisor and the agent stall together. That
happens in a whole-user or whole-system stall: memory exhaustion, or the process group stopped.
- **The worst case.** The owner's A→B session waits on each new agent connection from B, for as long
  as both stay stalled.
- **On Linux**, systemd's watchdog usually ends it within about 6 s (§8.6) [추정; OL-6]. Nothing in
  sopen bounds it on macOS.
- **The way out:** `sopen allow --off` in another terminal on A, or ending the session.

**The hardest two cases.**
- **Escalating from reads.** A fully compromised B can do everything row 1 allows, so the bound is
  what §6 and §7 serve, not who asks.
  - §6 resolves every link itself, from held root descriptors, and checks each target before
    touching it. It opens the object it checked and checks that object's own name, links,
    filesystem and inode. That closes links, `..`, swaps, FIFOs, mounts and moved caches. After the
    lexical steps it gives one answer, so it is no oracle.
  - A's own sopen for B reads through the same service (§7), never by a path it resolves itself.
  - §7 never lets A connect anywhere but the pinned B. It starts no app but sopen's own windows, and
    those are labelled before anything B chose.
- **The session freeze.** ssh waits on the agent for every connection B makes.
  - The bind is answered by a thread that touches nothing else, keeps a spare descriptor, and is
    watched from outside: within 10 ms under load (OP-6, OP-13), and killed and replaced within
    about 3 s if it stalls (§8.6).
  - A flood still costs A's ssh one local round trip per connection. sopen cannot bound that: it is
    a residual, with OR-0 (c)'s number.

**Against agent forwarding's known risk.** `ssh_config.5:893-901` says that whoever can get past file
permissions on B can use the keys of the forwarded agent. Here the agent holds none. That is strictly
less than `ForwardAgent yes`, and less than today, where B can ssh to A.

## 13. Decisions

D1–D8 are the orchestrator's (2026-11-24), carried unchanged. D9–D17 are the orchestrator's too
(2026-11-25). D9–D12 accepted four of this document's first proposals, which the first draft
numbered D3a, O10, part of O6 and O3. D13–D17 settle design choices the two-engine review opened.
Their reasons are the orchestrator's. O1–O35 are this document's own; O3 and O10 are left unused,
since they became D12 and D10. All of them are reversible.

| # | Decision | Why |
|---|---|---|
| D1 | `sopen allow B` adds one `Include` line to `~/.ssh/config` (the owner ran the command that asks for it), says so, and `sopen allow --remove B` / `--off` take it out; `--print` only shows the lines | the owner's own command; one line, found again by exact match. *origin* §2.2 measured that only `forwardagent` changes, but with `ssh -F` on a composed file, not the real `~/.ssh/config`; OA-4 and OL-1 measure the real one |
| D2 | B never gets the owner's real agent keys | `ForwardAgent yes`'s risk (`ssh_config.5:893-901`) is what ssot item 4 rules out |
| D3 | B reads A's home read-only, minus every path with a component that starts with `.` (so `~/.ssh`, `~/.config`, `~/.gnupg` and every dotfile stay out); `--read <dir>` sets the roots instead | useful without setup; the dot rule covers where Linux keeps secrets |
| D4 | Opening on A's screen needs no prompt; it is rate-limited, and `--app` is never reachable from B | a prompt per open breaks the TUI → GUI switch's flow; `--app` would let B start any app (`src/app.rs:22`) |
| D5 | A stuck agent would freeze the owner's ssh session (measured), so the agent answers the first message from a thread that does nothing else, and a request that cannot be served fails at once | [M *origin* §2.2: 3.01 s, 6.26 s]; `clientloop.c:1855` waits synchronously |
| D6 | Without `SSH_AUTH_SOCK` on A, `sopen allow` refuses and says why (ssh then forwards nothing) | `ssh.c:2149-2161` drops the forward silently. The check connects (`authfd.c:87-113`), so `allow` checks that the agent answers. That agent is `ssh -G B`'s `identityagent` (`ssh.c:1754-1775`) |
| D7 | One socket per allowed host, plus B's host key pinned from the first message, is the "key or token per allowed machine" of ssot item 4 | A token on B adds nothing: anything with the owner's uid on B can read it (*origin* §4); each socket, pin and entry can be revoked alone |
| D8 | The agent owns its socket and fails closed (no socket activation) | an activated socket queues connections while the agent cannot start: the freeze (*origin* §3) |
| D9 | On macOS `~/Library` is outside the default roots too: `Library` directly under home is hidden whenever home is at or below a root, unless the matched root is inside `~/Library`, compared case-insensitively; `--read <dir>` still sets the roots explicitly | app tokens, keychains and sopen's own cache of third machines' files (`~/Library/Caches/ai.2lab.sopen`, directories 6.0.0 `mac.rs:67`; present on the Mac this was written on) would otherwise be readable from B |
| D10 | `sopen allow` also installs the user service (LaunchAgent / systemd user unit) that starts the agent; `allow` prints both files it writes, `--print` shows both, and `--off` (or `--remove` of the last host) takes both out | D8 needs a starter, and socket activation is excluded |
| D11 | An open from B never reaches an external editor (only sopen's own windows), on top of `--app` never being reachable | text from B would otherwise start VS Code or Zed on A (`src/main.rs:678-685`); *origin* §4: only sopen's own windows open |
| D12 | B's host key is pinned only from `sopen allow`'s own probe; the agent never pins on first real use, so a first contact the owner did not run is refused | the owner ran the probe; the acceptor never writes to disk; a swapped B is caught from the first session on |
| D13 | Non-local objects are refused always: network and FUSE mounts (macOS `MNT_LOCAL` false; Linux deny-list of network and FUSE filesystem types) and dataless or evicted files (`SF_DATALESS`) | they make A fetch a third host's data for B and can block a worker without bound |
| D14 | Windows A's sopen opens for B start no git: no git panel, no git run. The processes A's sopen may start for B are exactly the viewers' `ffmpeg`/`ffprobe` and, for kind-1 opens only, A's own `ssh` to the pinned B under D16's options (§12 row 1). That ssh may itself run the owner's own `ProxyCommand` or `Match exec` from A's config: the owner's commands, not B's | local git runs config B could influence (`core.fsmonitor`, hooks); a kind-1 open needs its own A→B connection, held to the pin |
| D15 | A changed host key on a re-run of `sopen allow B` refuses unless `--repin`; changing roots or restarting the service never re-probes | a re-run must not silently trust a new machine; a roots change is not a contact with B |
| D16 | The pin covers every A→B connection made for B's requests (kind-1 opens): such connections run with the pinned key as their only known host key (`UserKnownHostsFile=<pin file>`, `StrictHostKeyChecking=yes`), and reuse no master unless it was made that way. The spec adds `VerifyHostKeyDNS=no`, `PermitLocalCommand=no`, the pin as a plain key with any certificate stripped, and a control path keyed by pin generation (§7) | otherwise a kind-1 open would reach whatever answers at B's address, which the owner's config may not check (*origin* §3), or a key that DNS vouches for |
| D17 | A single stall, of the agent or of the supervisor, is bounded at about 3 s on both systems: a supervisor outside the agent probes the socket and kills and restarts a stalled agent (§8.6), backed by systemd's watchdog on Linux. A double stall (both together: a whole-user or whole-system stall) is an accepted residual (§12): the owner's session waits on each new connection from B, and the way out is `sopen allow --off` or ending the session | nothing inside a stalled process can end its stall, and the stall freezes the owner's session [M 6.26 s] |
| O1 | The agent serves read-only SFTP v3 itself, instead of a filtering proxy in front of `sftp-server -R` | check and open become one walk on one object (§6.3), with no gap between them |
| O2 | Agent framing; every connection starts with A's bind, then B's `hello` with a mode; SFTP after a code-0 `hello` in mode `sftp`; every sopen reply is code 29 with a code and a line | B's first message must be harmless to a real agent; ssh relays bytes unparsed [M]; one reply shape carries every refusal's reason |
| O4 | The bind's signature is not verified | it arrives from A's own ssh over a 0600 socket; only the owner's uid could forge it, and that uid owns A already |
| O5 | Onward hops are always refused: a connection whose B traffic holds a bind before `hello` is an empty agent for good, with no per-host option | ssot item 4 allows access only from B |
| O6 | `open` takes A's paths under the whole of §6 and B's own absolute paths (A writes `<you>:<abs>`); A runs `sopen --here --from-origin <you>`; a kind-0 child reads only through its served socket, never by a path it resolves itself | A connects to nothing but the pinned B (ssot item 4); a reopen by path would undo the check (a link swapped after it); 10 §7.1: A has a `target`, so `--here` |
| O7 | Regular files with more than one link are refused; FIFOs, sockets and devices are refused before they are opened | a second name is invisible to a path filter; an open must never block a worker |
| O8 | Caps derived from the descriptor budget: `min(32, C)` connections per host, `C = min(128, F / 8)` in all, the excess answered by one refuser thread; 64 pending first messages in all, 1 s each; a bind answered within 10 ms; 3 opens per 10 s and 8 running per host; a handle pool with 64 per connection; 256 KiB reads | §8.4's reasons: what one window holds (06 §6), and a bind never waiting on another connection or a descriptor |
| O8a | No per-host byte cap | it protects nothing the roots do not already grant, and one channel is bounded at 1.1–2.0 MB/s [M] |
| O9 | `ssh -G B` with `forwardagent` other than `no` → `sopen allow B` refuses | the `Include` comes first, so it would take the owner's forward away unasked (D2 forbids keeping it) |
| O11 | Socket files survive a stop or a crash; only `--remove` and `--off` unlink them, after the lines naming them are gone | `ssh.c:1516-1520`: a missing path plus `ExitOnForwardFailure yes` makes `ssh B` exit 255; a stale socket fails closed |
| O12 | `origin` (exact, no user) is a reserved host word on B; `--target C origin:x` is refused; the cache key stays `origin`; a process keeps the first `a_id` it reached | the owner's words; two A's must never mix within one process |
| O13 | Discovery: `SSH_AUTH_SOCK`, then a scan of this uid's session sockets; two A's → refuse | B's sshd accepts no variable [M]; tmux panes keep stale values |
| O14 | No address goes through origin in this cut; non-loopback addresses never | A's browser would fetch from A's network for B (ssot item 4) |
| O15 | The policy, the included file, the sockets and the log live in `<state>/origin/` | one folder to delete; under a dot path on both systems, so B never reads them |
| O16 | One SFTP session per agent channel; no striping in this cut | 1.1–2.0 MB/s is enough for an mp3 (≈ 40 KB/s), pictures and 64 KiB views (*origin* §4) |
| O17 | On B, the git panel is not offered on origin; writes are refused by A and shown by today's error paths | no shell on A (ssot item 4) |
| O18 | After the edit, `ssh -G B` must differ only in `forwardagent`, and another host's not at all; otherwise everything is undone | the owner's ssh must not change in any other way |
| O19 | `--off` removes everything, hosts included; enabling again probes again | a kill switch leaves nothing behind |
| O20 | Every file is written by an atomic replace; the included file is 0600; the paths written avoid every character ssh expands | `readconf.c:2635-2642`; `ssh.c:1509-1515`; `readconf.c:2075-2105` |
| O21 | On B, text from `origin:` opens as its fetched copy, a local file in VS Code, Zed or the terminal editor, never through Remote - SSH; nothing is uploaded; F4 on an origin folder and a folder without a display are refused | the remote route makes VS Code run `ssh origin` from B (`src/zed.rs:216-236`); origin is read-only |
| O22 | The included file names each socket by a literal absolute path, `<state>/origin/<B>.sock`, in B's own `Host` block; no `%n` | a per-host block needs no token; `%n` is the name as typed (`ssh_config.5:2360-2361`), which a literal path makes moot |
| O23 | Incoming caps checked on the length before allocation: 16 KiB first message, 64 KiB control message, 64 KiB SFTP packet; over → closed | B sends no writes; an unchecked length is a memory exhaust |
| O24 | `hello`'s fields and its reply's first fields (`code`, `line`, `proto`) are frozen for every protocol version; only `hello` ignores trailing bytes | version choice must work between any two versions |
| O25 | `open`'s `state` field is an allow-list: empty in proto 1, named keys later, anything else refused | a free-form field from B would become a channel into A's sopen |
| O26 | The peer of every host-socket connection must be an ssh client the probe actually saw (path, device, inode; `-V` ≥ 8.9), found by the peer's pid (`LOCAL_PEERPID` / `SO_PEERCRED`) and its executable. After an upgrade the worker re-runs `-V` once per new device and inode. The agent's own executable passes only under the test hook | without a bind from A's side, B's first bytes could forge one with B's public key; a wrapper script on PATH is not the peer |
| O27 | sopen's own cache and state folders are hidden by device and inode, wherever `SOPEN_CACHE_DIR` / `SOPEN_STATE_DIR` put them; every run with a non-default folder registers it and sends the agent SIGHUP | the variables can move them under a visible name (`src/cache.rs:29-37`); the residual is the moment between a folder's first use and the reload |
| O28 | One walk for every path-bearing request, from root descriptors held since policy load: links resolved in process with `readlinkat`, each target checked lexically before it is touched, the final descriptor's kernel name checked again; one `NO_SUCH_FILE` for every refusal after the lexical steps | `realpath(3)` would follow links out of the roots (an existence oracle, and stats on mounts); STAT and REALPATH must be as race-free as OPEN |
| O29 | A `--from-origin` window's title starts `[from <you>] `; bidi and control characters in the part B chose become U+FFFD | a suffix can be pushed out of sight or reordered by a name B chose |
| O30 | A socket is bound under a temporary name and `rename(2)`d over its path; never unlinked first | an `ssh B` during a restart must never meet a missing path (O11) |
| O31 | One descriptor budget: the soft limit raised to the hard one, `R = 144 + 34·H` fixed, 4 per connection, `C` and `P` derived so that `R + 4·C + P = F`; a floor of `R + 128` below which the agent does not start; a spare descriptor as the last resort | 128 connections × 64 handles exceed launchd's 256 and systemd's 1024 soft limits [추정], and an `EMFILE` at accept would leave A's ssh waiting on a bind |
| O32 | A panic on the acceptor aborts the process; a panic in a worker closes its connection only | an acceptor dead under open listeners would freeze every new connection; the supervisor replaces an aborted process |
| O33 | `sopen allow` snapshots every file it may write and the undo restores exactly those, unlinking a socket only when the restored stanza does not name it | a failed re-run must leave the old B working |
| O34 | Every socket name origin binds is short (`.n<6>`, `s<6>`, `p<6>`, `h`, `o<h><g>-%C`), and `sopen allow` checks the longest one, ssh's 17 added bytes included | ssh binds `<ControlPath>.<16 random>` first (`src/remote/sftp.rs:44-45`, `:573-577`); a long name fails on most Macs |
| O35 | `--repin` ends B's D16 masters of the old pin generation before the new pin takes effect, and the new generation renames the control path | a master authenticated under the old key must never carry a request after the pin changed |

## 14. Acceptance

Each item is "do X → observe Y", with the unit that brings it in brackets. Two test pieces recur:
- **The relay** is §15's stand-in for A's ssh plus B's sshd.
- **"Files unchanged"** means the SHA-256 of `~/.ssh/config`, `<state>/origin/` and the service
  folder, before and after.

**`sopen allow`** [OR-allow]:
- **OA-1** `SSH_AUTH_SOCK=` unset, then pointed at a dead socket: `sopen allow B` → each time
  §11's line, exit 1, files unchanged, no ssh call (the fake ssh's `calls` file is empty).
- **OA-2** `sopen allow B --print` → the `Include` line, the stanza and the service file are
  printed; files unchanged; no ssh call.
- **OA-3** `sopen allow B` against the fake ssh, its relay giving host key K:
  - the policy's pin is K's blob and fingerprint;
  - `~/.ssh/config`'s line 1 is the `Include`;
  - the included file is 0600;
  - the output is §10.2's.
- **OA-4** With `SOPEN_SSH_CONFIG=<file>` and the **real** `ssh -G -F` (no network) on a config that
  has `Host B` with a `Hostname`, a `Host *` with `ForwardAgent no`, and a `Match`:
  - before and after differ in `forwardagent` only for B, and in nothing for another host;
  - the same with that config as a symlink: the link stays;
  - `ssh -G -A B` and `ssh -G -o ForwardAgent=yes B` are recorded, and the output's `-A` line
    matches what they show (§9's [가설] on paper; OR-0 (i) on the owner's machines).
- **OA-5** The config forwards an agent to B (`ForwardAgent yes`, then a path) → O9's line; files
  unchanged. With `IdentityAgent none` → its line. With `IdentityAgent <a dead socket>` → the
  agent line; with `IdentityAgent <a live socket>` and `SSH_AUTH_SOCK` unset → accepted (D6).
- **OA-6** The verification is made to fail (the fake's second `-G` adds a line) → `~/.ssh/config`
  is byte for byte as before, and the included file and the policy entry are gone.
- **OA-6b** (O33) B is allowed and working. A re-run whose verification fails → the policy, the
  included file, B's known-hosts file and the service file are byte for byte the old ones. B's
  socket is still bound, and a relay `hello` through it still gets code 0.
- **OA-7** Two hosts are allowed:
  - `--remove B` → B's stanza and entry are gone, the `Include` stays, B's socket is unlinked;
  - `--remove` of the last host → the `Include` line, the included file and the service file are gone;
  - `--off` from two hosts → the same.
  - A test hook logs each step, and every line naming a path is removed before that path.
- **OA-8** An alias with `*`, a state folder with a space, and a socket path of 104 bytes → each
  line, files unchanged.
- **OA-9** The probe finds no sopen on B (exit 127) → the `ssh-add -l` probe pins, with the note. A
  probe whose relay sends no bind → the agent-channel line.
- **OA-10** (D15) B is allowed with key K:
  - the relay now gives K′, and `sopen allow B` → exit 1, D15's line, files unchanged;
  - `sopen allow B --repin` → K′ is the pin;
  - `sopen allow B --read ~/Music` → the roots change, and the fake ssh logged no call at all, `-G`
    included.
- **OA-11** (O26) A stand-in `ssh` on PATH that prints `OpenSSH_8.8p1` → its line, files unchanged.
  With `OpenSSH_10.3p1` → the policy records its path, device and inode.
- **OA-12** Two `sopen allow` at once, the first held by a test hook → the second waits, then says
  `another sopen allow is running`, exit 1.
- **OA-13** Leftovers:
  - `--remove` of the last host with the agent stopped → B's socket is unlinked by sopen, after the
    stanza is gone (the step log);
  - `--off` after an allow that created `~/.ssh/config` → the file is gone;
  - `--off` when `~/.ssh/config` holds other lines → only the line is gone;
  - `p*` and `s*` are gone, and `agent.log` stays, its path printed.
- **OA-14** (O34) A `SOPEN_STATE_DIR` for which `<state>/o<h><g>-%C` plus 17 bytes is 104 → the
  length line, files unchanged, although every agent socket name would fit. One byte shorter →
  accepted.
- **OA-15** (O35) B is allowed, and a D16 master of the old generation is alive (a fake ssh master
  socket at `<state>/o<h><g>-…`). `sopen allow B --repin` → the fake ssh logged `-O exit` on that
  socket before the policy changed, the socket is gone, and the new control path carries `<g>+1`. A
  master that will not end → exit 1, files unchanged.
- **OA-16** (O26) Behind a wrapper script named `ssh` on PATH, the probe → the policy records the
  real ssh the probe's peer check saw, not the wrapper. Step 8 → liveness proved by the health ping
  and a connect, and the agent log shows no bind from sopen.

**The protocol and the agent** [OR-agent]. The agent runs in a temp state folder, behind the relay:
- **OP-1** `hello` `info` → code 0 with the fields; `REQUEST_IDENTITIES` → 0 keys; a sign request →
  5; `query` → 5.
- **OP-2** The relay's bind carries another key → B's `hello` gets code 1 with the pin line, and the
  empty-agent rows are still answered. With `is_forwarding` = 0 → its line.
- **OP-3** B sends a bind, then `hello` → code 1 (onward). No bind at all, `hello` first → §5.1's
  line.
- **OP-4** (O24) `hello` with `[2, 3]` → code 2. B's line names both ranges. A proto-2-shaped
  `hello` — `[1, 2]` and two extra trailing fields → code 0 with `proto` 1, the trailing bytes
  ignored. The same trailing bytes on an `open` → closed.
- **OP-5** (O23) A length over the cap → closed, with no buffer of that size allocated (a test hook
  counts allocations): over 16 KiB first, over 64 KiB later, and an SFTP packet of 65 537 bytes.
- **OP-6** Bind latency: 64 connections that send nothing, plus 32 SFTP sessions reading a 200 MiB
  file → 100 fresh binds are answered. The bound is 10 ms on an idle machine (OR-0 (c)) and 100 ms in
  CI, where shared runners stall. The silent connections are closed after 1 s. A 65th pending one is
  closed at accept.
- **OP-7** 33 connections from one host → the 33rd connection's `hello` gets code 1, `too many
  connections from B (32 open)`, and B prints it.
- **OP-8** Two relays (two sessions) read the same file at once → both SHA-256 equal to the file's.
- **OP-9** The agent is killed with SIGKILL → its socket file stays and B gets the reset line. The
  agent starts again → it binds and serves. SIGTERM → the files stay too.
- **OP-10** A stand-in real agent (it answers 5 to every extension) behind B's socket → B's line,
  exit 1. The stand-in received exactly one message, the `hello`.
- **OP-11** SIGHUP after the policy drops B → B's socket is gone and its open connections end. After
  the policy narrows B's roots → the next STAT outside them is refused on a session already open.
- **OP-12** (D17) A stand-in ssh waits on a bind reply while the agent is SIGSTOPped → the
  supervisor kills it within 2 s, the stand-in reads EOF within 3 s, and a new agent answers a
  fresh bind within 1 s more. Five kills within 60 s → the 60 s back-off.
  - **OP-12b** (Linux) With a fake `NOTIFY_SOCKET`, `WATCHDOG=1` arrives after each good probe and
    stops while the agent is stopped.
- **OP-13** (O31) Every handle of the pool held, every connection slot full, and 8 served windows
  open per host (their listeners, pipes and connections):
  - a fresh bind is answered within OP-6's bound, and an OPEN fails at once with `out of file
    handles`;
  - the agent's open descriptors never exceed `F`, and no `EMFILE` was seen (the spare unused);
  - with `SOPEN_ORIGIN_NOFILE` set below `R + 128` → the agent does not start, with its line;
  - with a test hook leaking descriptors past the budget → a fresh connection reads EOF within the
    same bound (the spare, as a last resort).
- **OP-13b** (O26) The recorded client's file is replaced by a newer copy (another inode) → the
  worker runs its `-V` once, accepts it, and the next connection runs no `-V`. Replaced by one that
  says `OpenSSH_8.8` → refused.
- **OP-14** (O30) A client connects in a loop while the agent is restarted 20 times → it never gets
  `ENOENT`, only an answer or `ECONNREFUSED`.
- **OP-15** (§7) Raw `open` messages from the relay, each → code 1 and the agent started nothing:
  - kind 0 `~/.ssh/x`, kind 0 a link out of the roots, kind 0 a FIFO;
  - kind 1 a relative path, kind 1 a path with a NUL;
  - flag bit 1; a non-empty `state`.

  Kind 0 a folder → code 0, and the stand-in child got `--here --from-origin B -- origin:<q>`, `q`
  the walked path, with `SOPEN_ORIGIN_SERVED` set.
- **OP-16** (§7, O6) During a kind-0 open, a test hook swaps the file for a link to `~/.ssh/x` after
  the agent's check → the child's OPEN through its served socket → `NO_SUCH_FILE`. A connection to
  the served socket from another process → closed. The child exits → the served socket is gone
  before the child is reaped (the agent's step log).
- **OP-17** (O26) The relay without `SOPEN_ORIGIN_TEST_RELAY=1` → `hello` code 1 with §5.1's
  line, even with the sopen binary listed in the policy. With the hook → served.
- **OP-18** (O32) A test-hook panic on the acceptor → the process aborts, and the supervisor starts
  a new one. A panic in a worker → that connection closes and the rest serve on.
- **OP-19** (D16) A kind-1 open → every ssh the child ran (the fake ssh's `calls`) carries D16's
  options, `VerifyHostKeyDNS=no` and `PermitLocalCommand=no` among them, and the `o<h><g>-%C`
  control path.
  - With the real `ssh -G -F` on an owner config that sets `VerifyHostKeyDNS yes`,
    `PermitLocalCommand yes` and a `LocalCommand` for B, plus D16's options → `verifyhostkeydns no`
    and `permitlocalcommand no`.
  - The known-hosts file holds the plain key when the test bind carries a host certificate.

**The filter** [OR-filter, OR-serve], the roots `~` and then `~/Music`:
- **OF-1** Refused as asked, each with its reason, no data, and no filesystem call:
  - `~/.ssh/id_ed25519`; `~/Music/../.ssh/x`; `~/Music2/a` under root `~/Music`; `/etc/passwd`;
  - on macOS (D9): `~/Library/Caches/x`, `~/library/x`, `~/LIBRARY/x`, and `<home>/Library/x` under
    root `/Users`.
- **OF-1b** Refused after the lexical steps, each answered `NO_SUCH_FILE "no such file under what B
  may read"`, exactly as a missing file is. The agent log names the real reason.
  - a link `~/Music/k` → `~/.ssh/k`; a link `~/Music/l` → `~/Library/x` (macOS);
  - a FIFO (no worker blocks); a file with two names;
  - a file under a FUSE or network mount (through the mount-table seam, and a real FUSE mount where
    CI has one), with no call into the mount; a dataless file (macOS, through the seam);
  - with `SOPEN_CACHE_DIR=~/Music/cache` at allow time: `~/Music/cache/x` (O27), and READDIR of
    `~/Music` leaves `cache` out;
  - swaps, by a test hook at each step: a folder swapped for a link mid-walk; the last component
    swapped for a link between its `fstatat` and its `openat`.
- **OF-2** `~/Music/a.mp3` and a 64 KiB range at 32 MiB of a 64 MiB file → SHA-256 equal. A link
  `~/Music/b` → `~/Music/a.mp3` → served.
- **OF-3** READDIR of `~` leaves out `.ssh`, `.config`, `.`, `..` (and `Library` on macOS). A listed
  link to `~/.ssh` shows with no target.
- **OF-4** OPEN with the write flag → `PERMISSION_DENIED`. WRITE, RENAME, REMOVE, MKDIR, SETSTAT,
  READLINK → `OP_UNSUPPORTED "origin is read-only"`.
- **OF-5** Every filesystem call of the filter goes through one seam, which the test counts. A
  lexically refused path → 0 calls. `~/Music/k` → `~/.ssh/k` → no call names a path under
  `~/.ssh`.
- **OF-6** (O28) The swaps of OF-1b, run against STAT, LSTAT and REALPATH as well as OPEN → each
  answers `NO_SUCH_FILE`. A root whose component becomes a link → that root is disabled at the next
  SIGHUP, and the log says so.

**B's sopen** [OR-client, OR-open-a], with `SSH_AUTH_SOCK` = the relay's B-side socket:
- **OC-1** `SOPEN_LIST=1 sopen origin:~/Music` → A's folder without hidden names.
  `SOPEN_CHOICE=save SOPEN_SAVE_TO=<f> sopen origin:~/x.bin` → `<f>` has the same SHA-256.
- **OC-2** [OR-open-a] `sopen --target origin origin:~/A.mp3` → the agent ran the stand-in A sopen
  (`SOPEN_ORIGIN_SOPEN`) with `--here --from-origin B -- origin:<the walked path of ~/A.mp3>` and
  `SOPEN_ORIGIN_SERVED` set, as §7 says. `FAKE_SOPEN_EXIT=3` → B exits 3;
  `FAKE_SOPEN_SIGNAL=TERM` → exit 1 with the signal line.
- **OC-3** [OR-open-a] `sopen --target origin ./b.txt` on B → the A side got `B:<abs of b.txt>`.
- **OC-4** Each of these → its line and exit code. The relay saw no `open`, and the fake ssh logged
  no call, `-G` included:
  - `--target origin http://127.0.0.1:1/`;
  - `--target origin other:/x`;
  - `--target origin --app X f` (exit 2);
  - `me@origin:x` (exit 2);
  - `--target C origin:x`.
- **OC-5** [OR-open-a] Four opens within 10 s → the fourth gets code 1 and its line. Nine windows → the ninth is
  refused.
- **OC-6** Discovery:
  - no `SSH_AUTH_SOCK` and an empty `~/.ssh/agent` → the no-agent line;
  - `SSH_AUTH_SOCK` unset with one relay socket in a fake `~/.ssh/agent` → it works;
  - two relays of different `a_id`s → the two-machines line;
  - a reconnect that reaches another `a_id` → its line.
- **OC-7** `SOPEN_LIST=git sopen origin:~/repo` → the line a remote without git gives. No `ssh` was
  run: the fake ssh logs no call, `-G` included.
- **OC-8** On A, `--from-origin B` with a text file → the browser with its viewer open
  (`SOPEN_SCREENSHOT`). The title starts `[from B] `, and neither the editor stand-in nor the
  browser stand-in was called. A file named with U+202E and a newline → U+FFFD in their places.
- **OC-9** On B, with a VS Code stand-in (`SOPEN_CODE`), `sopen origin:~/notes.txt` → the stand-in
  got a local path under the cache, no `vscode-remote://` URI, and the fake ssh logged no call. With
  `SOPEN_CODE=` and the terminal-editor stand-in, an edit → the read-only line and no upload.
  `SOPEN_KEYS=F4` on an origin folder → O21's refused line.
- **OC-10** (D14) On A, `--from-origin B` on a git work tree and on a media file:
  - `SOPEN_LIST=git` → the no-git line;
  - the `SOPEN_GIT`, editor, browser and `open` stand-ins were never called;
  - F4 → `not in a window opened from B`;
  - only the `SOPEN_FFMPEG` and `SOPEN_FFPROBE` stand-ins ran, for the media file.

**Live** [OR-0, OR-docs]:
- **OL-1** [owner] On A (the Mac), `sopen allow B`, then `ssh B`. On B:
  - `sopen --target origin origin:~/A.mp3` → it plays in sopen's media window on A, its title
    starting `[from B]`;
  - `unshare -rn env SOPEN_LIST=1 sopen origin:~/Music` → the listing, B having no network.
- **OL-2** [owner] `kill -9` the agent during a read on B → B's reset line; the owner's session keeps
  working; the supervisor starts a new agent; the next read works.
- **OL-3** [owner] Reboot A, log in, `ssh B` with `ExitOnForwardFailure yes` in the owner's config
  → ssh connects, and origin works once the agent is up.
- **OL-4** [owner] `sopen allow --off` → `ssh B` prints nothing new, and B's `sopen origin:~` gives
  the no-agent line. On A, `~/.ssh/config` is as it was before the first `allow`.
- **OL-5** [owner] From B, read a file in `~/Documents` on the Mac → record what macOS privacy
  consent does: a prompt on A's screen, or a refusal.
- **OL-6** [owner] On the Linux A, SIGSTOP both the supervisor and the agent → systemd's watchdog
  kills and restarts the service within about 6 s. On the Mac, record what the double stall does
  (§12's residual).

## 15. Test plan

**Off the network**, as AGENTS.md says (`AGENTS.md:127-136`).

**Unit tests.**
- `origin::wire` round-trips every message, and checks lengths before any allocation (OP-5).
- **`origin::filter`** runs §6 over a temp tree with links, FIFOs, hard links and moved caches.
  - Every filesystem call goes through one seam, `origin::fs`. Its test implementation counts and
    logs each call (OF-5), and can pause before any step (the swap hooks of OF-1b and OF-6, the last
    component included).
  - The mount table and `SF_DATALESS` also come through the seam, so D13's cases run in CI. A real
    FUSE mount adds one where the runner has one.
- **`origin::serve`** is driven by the real client: `SftpRemote` started on one end of a
  `tokio::io::duplex` through `start_on` (`src/remote/sftp.rs:208-234`). `local_server_with`
  already joins a client to a server that way (`:1363-1392`). The same client is the oracle for
  OF-1–OF-6.

**The relay** (new, `src/origin/relay.rs`). It plays A's ssh and B's sshd at once.
- **Where it lives.** It is a hidden mode of the binary, `sopen --origin-relay <B-side socket>
  <agent socket> <host-key file> [--forwarding 0] [--no-bind]`, as `--fkeys-watchdog` is
  (`src/main.rs:103-121`).
  - It cannot be test code: the lib's test helpers are `#[cfg(test)] pub(crate)` (`src/lib.rs:43-44`),
    which `tests/cli.rs`, a crate of its own, cannot import. That is why it repeats one
    (`tests/cli.rs:1497-1501`).
  - It gives a local process nothing new: anything with the owner's uid can already connect to the
    socket and write a bind.
- **Its own hook, not O26's list.** The relay is the sopen binary, which is never a checked client.
  The agent accepts it only with `SOPEN_ORIGIN_TEST_RELAY=1` in its environment (OP-17).
- **What it does.** It listens on a B-side socket. For every connection it connects to the agent's
  socket, writes a bind with a fixed test host-key blob (`is_forwarding` as set), reads the 5-byte
  reply, then relays bytes both ways.
- **Faults it can inject:** a wrong key, no bind, a delay, a dropped connection, a forged bind as B's
  first message.
- **No crypto** is needed (O4).

**The fake ssh** (`tests/cli.rs:1436-1583`, a `/bin/sh` script).
- **It logs every call, `-G` included.** Today it answers `-G` and exits (`:1538`) before the call
  log (`:1541`). OR-client moves the log first, so "no ssh call" rows (OA-10, OC-4, OC-7, OC-9) also
  catch `ssh -G`.
- **It learns `-o ForwardAgent=<path>`.** When that is given:
  - it starts `"$FAKE_SSH_RELAY_BIN" --origin-relay` toward `<path>` in the background, where the
    test sets `FAKE_SSH_RELAY_BIN` to the built binary (`CARGO_BIN_EXE_sopen`);
  - it runs the target's command with `SSH_AUTH_SOCK` set to the relay's B-side socket in the fake
    target home;
  - it stops the relay when the command ends.
- **`FAKE_SSH_VERSION`** sets what `ssh -V` prints (OA-11).

The probe, OA-3, OA-9 and OA-10 then run as `sopen allow` runs them. The OC and OP rows start the
agent (`--origin-agent`, with a temp `SOPEN_STATE_DIR`) and the relay themselves.

**Hooks** (AGENTS.md "Receipts" rows, OR-docs):
- `SOPEN_SSH_CONFIG=<file>` — `sopen allow` edits that file instead of `~/.ssh/config` and runs
  `ssh -G -F <file>`. OA-4 uses the real ssh.
- `SOPEN_ORIGIN_SERVICE=none` — no service is written; the test runs `sopen --origin-agent` itself.
- `SOPEN_ORIGIN_SOPEN=<path>` — the agent starts this binary for `open`, as `SOPEN_TARGET_BIN` does
  for `--target`.
- `SOPEN_ORIGIN_SCAN=<dir>` — discovery scans this folder instead of `~/.ssh/agent`.
- `SOPEN_ORIGIN_NOFILE=<n>` — the agent sets its soft descriptor limit to `n` instead of raising it
  (OP-13).
- `SOPEN_ORIGIN_TEST_RELAY=1` — the agent accepts its own executable as a peer, the test relay
  (O26, OP-17). Nothing else changes.

**Gate.** The loop's: `just check`, with `SOPEN_REQUIRE_SFTP_SERVER=1` and the Linux clippy runs.
- `origin::serve` needs no `sftp-server` (O1).
- OA-4 needs the real `ssh` binary, which both CI runners have. It skips with a note where `ssh` is
  missing.
- OP-12's bounds are timed with margin in CI (×3), and exactly in OR-0 (c).
- A unit that shows something on A's screen brings a screenshot someone looked at.

## 16. Work units

**OR-0 first**, before any unit merges: a spike outside the repo, or on a branch never merged, with
the real binaries between the Mac (A) and the Linux laptop (B). Each item is a named measurement and
gates the decision given. Its results go into §§5–9 and 14.

| Item | Measure | Gates |
|---|---|---|
| (a) | `SftpRemote` through `start_on` over a real agent channel, against a minimal Rust read-only server: stat, list and a ranged read, with the throughput, against *origin* §2.2's 1.1–2.0 MB/s | O1, O16 |
| (b) | The agent as a LaunchAgent (`ProcessType Interactive`) on the Mac and as a systemd user service on Linux, starting a sopen that opens a window on A's screen: whether it shows, whether it takes focus, and TCC on `~/Documents` (OL-5) | D10, §7 |
| (c) | Bind latency under OP-6's load through the real ssh, timing the owner's session output. Then a SIGSTOP of the agent: the session's freeze until the supervisor's kill. Then a loop of 1 000 connections from B: the session's slowdown | D5, D17's ~3 s bound, §8.4's flood residual |
| (d) | Which reply (5 or 28) a real `ssh-agent` gives `hello`, on A and on B | §5.2 |
| (e) | Whether `getmntinfo(MNT_NOWAIT)` and a `/proc/self/mountinfo` read stay off a hung network mount (its server unplugged); whether `fstatat` on an evicted iCloud file downloads or blocks; whether `F_GETPATH` reports a name's stored case on APFS | D13's method, D9's last check |
| (f) | The first OpenSSH release that sends `session-bind`; that `LOCAL_PEERPID` (macOS) and `SO_PEERCRED` (Linux) name A's ssh process for a forwarded connection, and its executable's path | O26 |
| (g) | D16's option set against a B whose host key was swapped: refused. Against the real B: accepted. No master made without the pin is reused. With an inherited `VerifyHostKeyDNS yes` and a forged SSHFP record for B in a resolver the spike controls: still refused. With B presenting a host certificate: the plain-key pin accepted | D16 |
| (h) | The soft and hard `RLIMIT_NOFILE` under launchd and under systemd's user manager, and what raising the soft one gives | O31's numbers |
| (i) | `ssh -G -A B` and `ssh -G -o ForwardAgent=yes B` with the stanza in place, then a real `ssh -A B`: which agent B gets | §9's [가설]; allow's `-A` line |
| (j) | `rename(2)` of a new unix socket over a live one's path, with a client connecting in a loop on each OS | O30 |

**`open` is refused until OR-open-a merges.** The agent answers `open` with code 1 (`opening on A
is not built yet`), so no unit starts `--from-origin` before it exists. OR-open-a turns `open` on
in `src/origin/agent.rs`.

| ID | Unit | Owns | After |
|---|---|---|---|
| OR-wire | Messages and framing, codes, the frozen `hello` (O24), the `hello` / `open` / `ended` shapes, `state`'s allow-list (O25), version choice, length caps (O23) | `src/origin/mod.rs` (new: its doc and `mod wire`), `src/origin/wire.rs`, `src/origin/wire/tests.rs` (new); `src/lib.rs` (`pub mod origin;`); `Cargo.toml` (tokio's `net` feature; openssh already turns it on, so the lock does not change [추정]) | OR-0 |
| OR-filter | §6: held roots, hidden names (D9, any case), O27's folders, D13's mount table and dataless files, the one walk (O28), link counts, the final-name check, the one answer; the filesystem seam | `src/origin/fs.rs`, `src/origin/filter.rs`, `src/origin/filter/tests.rs` (new); `src/origin/mod.rs` (`mod fs`, `mod filter`) | OR-wire |
| OR-serve | §5.3's SFTP server on a stream through the walk; `SftpRemote::over_stream` (`start_on` on given halves, `rename::Slot::none()`) | `src/origin/serve.rs`, `src/origin/serve/tests.rs` (new); `src/origin/mod.rs` (`mod serve`); `src/remote/sftp.rs` (the new constructor, beside `start_on`) | OR-filter |
| OR-agent | §8: the supervisor (D17), the acceptor with its spare and health socket (O31, O32), O26's peer check, the refuser, workers, the descriptor budget, O30's binding, reload, log, the policy file, the served sockets and `open` spawning with the session environment (the rule of `src/forward.rs:306-321`, written again in Rust; `forward.rs` is not edited), `open` kept refused; the relay; the hidden `--origin-agent`, `--serve` and `--origin-relay` | `src/origin/agent.rs`, `src/origin/supervise.rs`, `src/origin/peer.rs`, `src/origin/policy.rs`, `src/origin/relay.rs`, `src/origin/agent/tests.rs` (new); `src/origin/mod.rs` (five `mod` lines); `src/main.rs` (the hidden flags) | OR-serve |
| OR-client | §9 and §4.2: discovery and `SOPEN_ORIGIN_SERVED`, the process pin, `origin::client::sftp()` and `open()`, the hidden `--origin-check`, the editor rules (O21). Its seams: `SftpRemote::connect` and `connection_args` (`src/remote/sftp.rs:290`, `:671`); `run_source`'s origin branch and `run_url`'s refusal (`src/main.rs:504-592`); `open_in_editor` and F4's `launch` (`:1085-1158`, `:787-812`); `check_host` and `source_for` (`src/forward.rs:77-95`, `:203-241`); `git_connect` (`src/gui/browser/git_host.rs:28-30`); `ssh_user` (`src/gui/browser.rs:319-333`); the fake ssh's call log moved before its `-G` answer | `src/origin/client.rs`, `src/origin/client/tests.rs` (new); `src/origin/mod.rs` (`mod client`); `src/remote/sftp.rs`; `src/main.rs`; `src/forward.rs`; `src/gui/browser/git_host.rs`; `src/gui/browser.rs` (`ssh_user` only); `tests/cli.rs` (the fake ssh's log; the OC rows but OC-2, OC-3, OC-5, OC-8 and OC-10, which need `open`) | OR-agent |
| OR-open-a | A's half of §7: `--from-origin <you>` (text → the browser's viewer by `state.open` (D11); no hand-off, no git (D14), F-key switch off), the title prefix (O29), D16's ssh options and control path for that run, `open` turned on in the agent | `src/main.rs` (`route`, `run_open`, `open_in_browser`'s hooks, the flag); `src/gui/mod.rs` (the title at `:1305`); `src/remote/sftp.rs` (D16's options in `connection_args_for` and `ssh_config`'s control path, for that run only); `src/origin/agent.rs` (`open` on); `tests/cli.rs` (OC-2, OC-3, OC-5, OC-8, OC-10, OP-15, OP-16, OP-19) | OR-client |
| OR-allow | §10 and §8.1: checks (D6's `identityagent`, O26's client), the lock, the probe, D15's re-run and `--repin`, the snapshot and undo (O33), writes, verification with the `-A` runs, the service files, `--remove`/`--off` and their leftovers, the listing, O34's length check, O35's re-pin; the run-side O27 registration (`src/origin/register.rs`, called from `main` after `cache::dirs()`, `src/main.rs:239`); `Cmd::Allow`; `forward::check_words`; the byte-level atomic replace factored out of `config::write`; the fake ssh's relay start and `FAKE_SSH_VERSION` | `src/origin/allow.rs`, `src/origin/service.rs`, `src/origin/register.rs`, `src/origin/allow/tests.rs` (new); `src/origin/mod.rs` (three `mod` lines); `src/main.rs` (`Cmd::Allow`, the registration call); `src/forward.rs` (`check_words`, beside `words`); `src/config.rs` (`write` → `replace_file`, its behaviour unchanged); `tests/cli.rs` (FakeSsh's `ForwardAgent`, the OA rows) | OR-open-a |
| OR-docs | README "From the machine you came from"; AGENTS.md rows (§15's hooks, the hidden flags); 01 §4a's and §4's amendments; the live receipts OL-1–OL-6 | `README.md`, `AGENTS.md`, `.prd/01-spec.md` | OR-allow |

**Order.** OR-0 → OR-wire → OR-filter → OR-serve → OR-agent → OR-client → OR-open-a → OR-allow →
OR-docs. The chain is linear because each unit shares a file with the next:
- `src/origin/mod.rs` gets its `mod` lines from every unit;
- `src/main.rs` passes OR-agent → OR-client → OR-open-a → OR-allow;
- `src/remote/sftp.rs` passes OR-serve → OR-client → OR-open-a;
- `src/origin/agent.rs` passes OR-agent → OR-open-a.

What ships when:
- Reads from B are usable once OR-client has merged, with the policy and service set up by hand.
  They ship behind no flag, since nothing reaches them without a policy.
- `--target origin` works from OR-open-a on.
- `sopen allow` lands with OR-allow.

**Shared files are sequenced.** Each passes from one unit to the next in the order given. A conflict
goes to the dispatcher and is never settled by dropping the other side (06 §13).

| File | Order |
|---|---|
| `src/origin/mod.rs` | OR-wire → OR-filter → OR-serve → OR-agent → OR-client → OR-allow, its `mod` lines |
| `src/origin/agent.rs` | OR-agent → OR-open-a (`open` on) |
| `src/main.rs` | OR-agent → OR-client → OR-open-a → OR-allow; 09's WV-E1 and WV-E2 and 10's XW-term and X2 also edit it, so the dispatcher sequences across workstreams |
| `src/remote/sftp.rs` | OR-serve → OR-client → OR-open-a |
| `src/forward.rs` | OR-client → OR-allow; 09's WV-E1 and 10's X2 also edit it |
| `tests/cli.rs` | OR-client → OR-open-a → OR-allow; 09's WV-E1 also edits it |
| `src/gui/mod.rs` | OR-open-a, one line; 09's WV-0 and 10's XW-shell also edit it |
| `src/gui/browser.rs` | OR-client, `ssh_user` only; shared with 09's WV-D and 10's XW-grid / XW-list |
| `Cargo.toml` | OR-wire, one feature word; shared with 09's WV-A and 10's XW-0 |
| `README.md`, `AGENTS.md` | OR-docs only; one row each from the units of 09 and 10 |

## Tensions

The places where the research, the decisions and this spec still disagree.

- ***origin* §4 (a filtering proxy before `sftp-server -R`, checking "by realpath on A") vs O1 and
  O28.**
  - With a proxy, the proxy checks a path and then the server opens it: a gap between check and
    use. The proxy would also have to rewrite READDIR replies.
  - `realpath` follows links out of the roots before any check.
  - A, a terminal machine, may have no `sftp-server` at all.
  - O1 and O28 serve SFTP in the agent through one walk from held descriptors.
- ***origin* §4 ("Default: nothing until the owner names roots") vs D3 and D9.** The orchestrator
  chose the home minus dot paths, and on macOS minus `~/Library`. §6 follows D3 and D9.
- ***origin* §4 and §5's prompts vs D4.** The research offered "an optional per-request prompt on A
  for paths outside the roots" (§4) and "per-host prompt for opens" (§5's bound column). D4 opens
  with no prompt, and this spec has no per-path prompt either: a path outside the roots is simply
  refused.
- ***origin* §4's per-host byte cap vs O8a (none).** A byte cap protects nothing the roots do not
  already grant, and one channel is bounded at 1.1–2.0 MB/s [M].
- ***origin* §5 ("refuse a connection that carries a second bind, or allow it per host") vs O5.**
  O5 always refuses, with no per-host option. ssot item 4 says access only from B.
- ***origin* §0, §3 (`ForwardAgent ~/.sopen/origin/%n.sock`) vs O22.**
  - The sockets live in sopen's existing private state folder.
  - Each socket is named by a literal path in its own `Host` block, so no token is needed.
  - (The first draft said the research's `%n` path overran `sun_path`. That 112-byte overrun was a
    `RemoteForward` listen path on B, *origin* §2.1, not a `ForwardAgent` path.)
- **ssot item 4's example ("a port A opened that only B's address can reach") vs the research's
  route.** The research killed the port: it is a B→A connection, it needs root for pf, and A sits
  behind NAT [M *origin* §2.5]. D7 maps item 4's "key or token per allowed machine" onto the
  per-host socket and the pin.
