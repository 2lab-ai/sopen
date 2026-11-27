---
title: sopen — git panel
type: spec
tags: [sopen, spec, git-panel, browser]
---

# sopen — git panel (03)

Status: planned (2026-11-20)

Owner message 14 (spec §1; [`browser/ssot.md`](browser/ssot.md) items 1, 2 and 4): when the browser's
current folder is inside a git work tree — on this machine or on a remote — the opposite side shows a
git panel ([`02-architecture.md`](02-architecture.md) §3) with the graph of every commit and branch,
commit details with changed files and diffs, every action of the VS Code extension Git Graph, and
free commit editing beyond it, each history rewrite confirmed first and undoable.

Amends, when it lands: 01 §2 (row A12), §3a (the browser's opposite side), §4 (git over ssh exec).

## 1. Source, licence, conventions

- **What is reproduced.** The workflow of Git Graph v1.30.0: which git operations exist and what the
  user can choose in each. Its inventory, built from the extension's source and not by operating it,
  is [`research/git-graph-features.md`](research/git-graph-features.md); §12 maps every item of its
  indexes to a section here or to "left out" with the reason.
- **Licence rule (binding on every implementer).** Git Graph's licence withholds permission to
  publish or distribute derivative works; sopen is MIT and distributed through Homebrew. Implementers
  work from **this document and git's own documentation only**. Do not open Git Graph's source —
  the permalinks in the research file included — and do not port anything from it: no code, CSS,
  icons, regexes, string tables, setting texts, tooltips or messages. Every label below is sopen's
  own; the look is sopen's PC Shell skin (`src/gui/chrome.rs`), never Git Graph's.
- **Evidence marks.** *(verified, git 2.56.1)* = run on 2026-11-20 in a scratch repository with stdin
  from `/dev/null`, each command handed to `sh -c` exactly as a remote login shell receives it (every
  word quoted as `forward::quote` quotes it; the reword message also through `bash` and `zsh`, and
  printed back exactly by `dash`). *(estimate)* = from git's documentation, not run.
- **Defaults are git's own** unless a line says why not. Dialog options are remembered for the
  window only; nothing about the panel is saved.
- **Placeholders.** `<c>` a full commit hash, `<b>` a branch name, `<r>` a remote, `<t>` a tag,
  `<p>` a path, `<v>` a value read just before (a ref's current hash), `<stamp>` the backup stamp
  (§8.2). Command lines below start at the subcommand; every one runs with §3.3's prefix.

## 2. Detection

### 2.1 The command

One command per folder, bounded at **10 s**, run in the folder:

```
git rev-parse --is-bare-repository --is-inside-git-dir --is-shallow-repository \
              --absolute-git-dir --git-common-dir --show-toplevel --show-superproject-working-tree
```

Lines are positional *(verified, git 2.56.1)*:

| Folder | Exit | stdout | Result |
|---|---|---|---|
| a work tree or any folder below its top | 0 | `false`, `false`, shallow, git dir, common dir, top level [, superproject] | `WorkTree` |
| a linked worktree | 0 | git dir ≠ common dir (common dir resolved against the folder when relative) | `WorkTree { linked }` |
| a submodule | 0 | a 7th line: the superproject's top level | `WorkTree { submodule_of }` |
| a shallow clone | 0 | third line `true` | `WorkTree { shallow }` |
| a bare repository | 128 | `true`, `true`, …; stderr `must be run in a work tree` | `NoWorkTree(Bare)` |
| inside a `.git` directory | 128 | `false`, `true`, …; same stderr | `NoWorkTree(GitDir)` |
| not in a repository | 128 | none; stderr `not a git repository` | `NotARepo` |
| below a mount point inside a work tree (another filesystem) | 128 | none; stderr names the filesystem boundary | `NotARepo(boundary)`: git stops looking upward at a mount unless `GIT_DISCOVERY_ACROSS_FILESYSTEM` is set, and sopen does not set it — a mounted folder is not treated as part of the repo above it *(estimate, git's documentation)* |
| a repository git refuses (owned by another user: "dubious ownership") | 128 | none; stderr names `safe.directory` *(estimate)* | `Refused(stderr's first line)` |
| git missing, too old (§2.4), or the 10 s bound passed | — | — | `Unavailable(reason)` |

### 2.2 Cache

- Results are cached per `(host, folder)` for the window's lifetime; an `Unavailable` for a missing
  git is cached per host.
- Cmd/Ctrl+R in the list (04's re-read) drops the cached result for that panel's folder; the panel's
  re-read then detects again. Nothing else expires it (a `git init` in a terminal shows after a
  re-read).

### 2.3 What shows the panel

- Only `WorkTree` shows the git panel (02 §3.1, O6). The panel's frame title is the top level
  (`host:/abs`), with ` (worktree of <main top level>)`, ` (submodule of <superproject>)` or
  ` (shallow)` after it when that applies.
- `NoWorkTree`, `NotARepo`, `Refused` and `Unavailable` keep the directory list. `Refused`,
  `Unavailable` and `NotARepo(boundary)` say why once on the status row, the first time per folder (`git panel: git refuses
  this repository: …`, `git panel: no git on host`, `git panel: git 2.17.1 on host; it needs 2.20`).
  A bare repository and the inside of a `.git` directory are not work trees (ssot item 1: "inside a
  git work tree"); whether to show their history is an open question for the owner (§10.3).
- **Submodules**: the panel is the innermost repository (the submodule), as `--show-toplevel` says;
  going up a folder in the list reaches the superproject's panel. In the superproject, a submodule is
  a changed path like any other (git's own `Subproject commit` lines in its diff).
- **Linked worktrees**: everything works as in the main work tree; the in-progress paths come from
  `--git-path` (per worktree, §8.5). A branch checked out in another worktree is labelled with
  ` [wt]` and git's own refusal to check it out or delete it is shown as it comes.
- **Shallow clones**: the graph ends at the shallow boundary; the last row's lanes run off the bottom
  and the paging line says `history ends here (shallow clone)`.

### 2.4 Which git

- **Local**: `SOPEN_GIT` when set, else `git` on `PATH`. On macOS, when that is `/usr/bin/git` (the
  developer-tools shim), sopen first runs `xcode-select -p`; when it fails, git counts as missing, so
  sopen never makes macOS offer to install the tools *(estimate: the shim can raise that dialog)*.
- **Remote**: the wrapper of §3.2 looks for `git` on the session's `PATH`, then in
  `/opt/homebrew/bin`, `/usr/local/bin`, `/usr/bin` (on a Darwin remote `/usr/bin/git` only when
  `xcode-select -p` succeeds there), and says `sopen: no git` on stderr with exit 127 when there is
  none.
- **Version floor: git 2.20**: every form below that is not listed next exists there *(estimate, from
  git's release notes)*. `git version` runs once per host. Features above
  the floor are offered only when the host's git has them: `--update-refs` (2.38), `restore` (2.23;
  older uses `checkout`/`reset` forms), `cherry-pick`/`revert --skip` (2.23), `worktree list` labels
  `%(worktreepath)` (2.23), `rebase --reschedule-failed-exec` (2.22) *(estimate, from git's release
  notes)*.

## 3. Running git

### 3.1 On this machine

- `git` is spawned with an argument vector (no shell), `-C <top level>` (detection: `-C <folder>`),
  the environment of §3.3, stdin `/dev/null` unless §3.3 feeds it, stdout and stderr piped.
- **In a session of its own** (`setsid` before exec): no controlling terminal, so neither git nor an
  ssh it starts can prompt on the terminal sopen was started from; its process group is what a cancel
  stops (§3.5).
- No prompt from git or ssh: §3.3's environment blanks every askpass program and disables terminal
  prompts, here as on a remote. A credential that needs typing fails at once with git's message
  *(verified, git 2.56.1: 0.07 s, `terminal prompts disabled`)*. Programs the owner configured — a
  credential helper, a signing program and its pinentry, a hook — are the owner's own and run as git
  runs them; one that waits for its own window or input is bounded by §3.4 (the status row says what
  git waits for, and Esc stops it).

### 3.2 On a remote host

- Over sopen's ssh master for the host (01 §4): `ssh <exec args> -T -o RemoteCommand=none
  -o LogLevel=ERROR <dest> -- <words>`. `<exec args>` (`remote::exec::exec_args`) is the master's
  `-S <ControlPath>` with `-o ControlMaster=no -o BatchMode=yes` and the keepalive options of
  `connection_args`: the command **attaches** to the running master and never becomes one, and never
  prompts. (A client that turned itself into a persisting master could keep the command's stderr
  open after the command ended *(estimate)*; `ControlMaster=no` rules that out.)
- **A dispatched command that changes the repository is never re-run automatically.** Its output and
  exit status travel over the same channel as everything else, so a lost channel says nothing about
  whether git ran — not even a missing `sopen-pid` line.
  - **Before dispatching** a network or write command, the runner checks the master
    (`ssh -S <ControlPath> -O check <dest>`: exit 255 and `Control socket connect(…): No such file or
    directory` when there is none, at once and without the network, *verified*). A failed check means
    the command was never sent: the panel revives the master through the browser's `Connect` — the one
    place where a prompt may appear (ssh's own, on the terminal, exactly as the browser's reconnect),
    never git's — and then dispatches.
  - **After dispatch**, any transport failure (ssh exit 255, the channel lost, no exit status) is
    `outcome unknown` for a write or network command: the log says so, the panel refreshes (§5) and
    shows what the repository now holds, and the action is not repeated; the owner may run it again.
  - **A read** is revived and run once more.
  - What ssh does when the master dies between the check and the dispatch — connect on its own with an
    agent key, or fail with 255 — is *(estimate)*; either way the rule above holds. G-R4 is the
    receipt.
- **The words** (`SshGit::words`), each quoted as `forward::quote` quotes it for `--target` (made so
  that sh, bash, zsh, dash and fish read each word back exactly), after a bare `exec`:

  ```
  exec 'env' 'LC_ALL=C' … 'sh' '-c' '<locate>' 'sopen-git' '-C' '<top level>' '-c' '…' '<subcommand>' '<args>…'
  ```

  `<locate>` is one fixed POSIX sh script: it prints `sopen-pid <its pid>` on stderr (the runner
  consumes that line, §3.5), finds git (§2.4) and `exec`s it with `"$@"`. It is part of sopen's
  binary, sent as a word, never written to the remote. The runner has exactly three more fixed
  snippets, run the same way and also by a local `sh`: the in-progress probe (§8.5), the files
  snapshot and the files restore (§8.2). Paths reach a snippet as arguments (never in its text),
  prefixed `./` where a command could read a leading `-` as an option, in batches of at most 64 KiB
  of arguments (a remote command line is one argument to the login shell).
- **Nothing is uploaded, installed or left on the remote.** Messages, todo lists and ref updates
  travel on the command's stdin (§3.3). The one file a command creates is the backup step's temporary
  index (§8.2), which that step's snippet removes when it ends. This is the browser's only remote command besides `--target`
  (01 §4a) and runs the remote's own git; it needs a login shell that prints nothing for a
  non-interactive command, which SFTP already needs (sshd starts the sftp subsystem through the same
  shell).
- **At most 3 commands in flight per host** (02 §6), the shown panel's parked log stream (§4.5) and
  an open history search's stream each counting as one.
- **Tests run the remote path without ssh** (AGENTS.md): `ShellGit` hands the same words to a local
  `sh -c` (the login shell's part), so quoting, `<locate>`, the pid line and stdin are exercised as a
  remote reads them; `SshGit` adds only the ssh argv, which a unit test checks word by word.

### 3.3 Never interactive

Every git command gets this environment and these options:

| What | Value | Why |
|---|---|---|
| `LC_ALL` | `C` | stable error texts where sopen reads them (`not fully merged`, `stale info`) |
| `GIT_TERMINAL_PROMPT` | `0` | a credential prompt fails at once instead of waiting |
| `GIT_ASKPASS`, `SSH_ASKPASS` | empty | no askpass program is run (an empty `GIT_ASKPASS` also hides `core.askPass` and `SSH_ASKPASS` from git *(estimate, git's prompt code)*) |
| `SSH_ASKPASS_REQUIRE` | `never` | an ssh that git starts never runs an askpass program (OpenSSH ≥ 8.4) |
| `GIT_PAGER`, `PAGER` | `cat` | no pager |
| `GIT_EDITOR` | `false` | an editor sopen did not plan for makes the command fail loudly instead of taking a default (`tag -a` without a message fails so, *verified, git 2.56.1*) |
| `GIT_EDITOR` (continue only) | `true` | `rebase --continue` and `merge --continue` open the editor for the prepared message; with `true` git's prepared message is used (both fail under `false`, *verified, git 2.56.1*) |
| `GIT_SEQUENCE_EDITOR` | `false`; planner runs: `f(){ cat >"$1";};f` (§7.8.2) | set in the environment, which wins over the repository's `sequence.editor` and over one the owner exported, where a `-c sequence.editor=…` loses to an exported one *(verified, git 2.56.1)* |
| `GIT_MERGE_AUTOEDIT` | `no` | merges never ask for a message |
| `GIT_OPTIONAL_LOCKS` | `0` (reads only) | a background `status` never takes `index.lock` away from the owner's own git |
| `GIT_LITERAL_PATHSPECS` | `1`, except History search | a path the owner picked is that path, not a pattern: `a*`, `[x].txt` and `:(top)z` each stage only themselves (without it, `a*` also staged `ab`; *verified, git 2.56.1*). History search's path field is the one place a pattern is meant: it runs **without** the variable and passes `:(glob)<pattern>` — under the variable `:(glob)` matches nothing (*verified*). `stash push --include-untracked` also runs without it: under the variable git stashes the untracked files but leaves them in the work tree (*verified, git 2.56.1*); that command takes no path, so no pattern can reach it |
| fetch | every fetch carries `--no-prune --no-prune-tags`, unless the action asked to prune (`--prune` and/or `--prune-tags`) | `fetch.prune`, `fetch.pruneTags` and `remote.<r>.prune` otherwise prune on their own: after Add remote, a plain `fetch <r>` with `fetch.pruneTags` set deleted a local-only tag (*verified, git 2.56.1*). This holds for every fetch sopen runs — Fetch, Fetch a remote, Add remote's fetch now, a pull's fetch step, Fetch into, Check out's pull, and the backup fetches of §8.4 |
| options | `-c core.quotePath=false -c color.ui=false -c log.showSignature=false -c i18n.commitEncoding=UTF-8 --no-pager` | raw paths (with `-z` wherever paths are listed), no colour, no signature output in logs; commits sopen writes are UTF-8 and labelled so (without the override, a repository set to `ISO-8859-1` would stamp sopen's UTF-8 bytes `encoding ISO-8859-1`; *verified, git 2.56.1*) |

- **stdin** is `/dev/null`, except where sopen feeds it: a commit, tag or squash message
  (`-F -`), a ref transaction (`update-ref --stdin`), a path list (`update-index -z --stdin`), a
  planner's todo (§7.8). Messages never travel in argv, except a stash message, which is one line
  (`-m`, newlines turned into spaces).
- **Messages** are committed **byte for byte as typed** (`--cleanup=verbatim`): leading blank lines,
  trailing spaces, tabs, CR, `#` lines and trailing blank lines stay (*verified, git 2.56.1*, through
  `-F -` and through a planner message line; `--cleanup=whitespace` would strip them). The dialogs edit
  UTF-8 text; a message that is not valid UTF-8 as git prints it (`%B`, converted to UTF-8 by git when
  the commit names its encoding) is never prefilled into an editor: reword, squash and the commit
  dialog's amend refuse such a commit with the reason, and every edit that keeps a message (fixup,
  move, drop, author and date) leaves its bytes alone.
- **Hooks run** as git runs them (repository policy). Commit, merge and push dialogs offer
  "Skip hooks (`--no-verify`)", off by default. A hook's output is the command's output (§3.7).
- **Signing** is whatever the repository's git config says (`commit.gpgSign`, `tag.gpgSign`); sopen
  adds no `-S`. A signer that waits (a pinentry) is bounded as §3.4 says; one that fails makes the
  command fail.

### 3.4 Command classes, limits and stopping

| Class | Commands | Automatic limit | Esc |
|---|---|---|---|
| read | detection, refs, status, log pages, details, diffs, files at a revision, `ls-remote`, the probe | 30 s without a byte on stdout or stderr while sopen waits for it — a parked log stream (§4.5) is not waiting (detection: 10 s in all); then stopped: `git did not answer in 30 s` | stops it |
| network | `fetch` (also the fetch step of a pull, §7.3), `push`, a remote delete, a backup's fetch | none in all — a large transfer takes as long as it takes; **120 s without a byte**, then stopped (git with `--progress` reports about once a second while it transfers, *verified: 13 progress lines for a two-commit push, git 2.56.1*) | stops it |
| write | everything that changes the repository or the work tree | none: a hook or a signer may legitimately take minutes | after 10 s, asks "Stop git? It may stop between steps; the panel then shows the state it left." Yes stops it |

- Every running command shows its elapsed time on the status row; after **10 s without output** it
  also says what git is probably waiting for (`no output for 40 s — a hook, a signer or a credential
  helper may be waiting for you; Esc stops`).
- Only one write runs per repository at a time; a second action waits in line, and the status row says
  so. Closing the window during a write waits for it and prints its result (02 §6); a second close
  asks the same Stop question.
- **What stopping a write leaves**: git removes its own lock files on `SIGTERM` (*verified, git
  2.56.1*: a `pre-commit` hook waiting forever, a signing program waiting forever and a credential
  helper waiting forever were each stopped with `SIGTERM` to the group: no process and no `*.lock`
  left; the stopped commit was not made and the index kept its staged change). A multi-step operation
  can stop between steps: a rebase or a planner run in progress, which the banner then shows (§8.5).

### 3.5 Stopping a command

- **Priority**: a stop never waits behind the in-flight limit (§3.2); it has a session of its own.
  On a remote that is one stop session per host at a time: a handle's stop ends all its groups in
  it (`kill -TERM -- -<p1> -<p2> -<p3>`, then `kill -KILL` the same way), and a second stop waits
  behind the first (02 §6's session count).
- **Local**: `SIGTERM` to the command's process group (it has its own session, §3.1); after 5 s,
  `SIGKILL` to the group. When `SIGKILL` was needed, sopen lists any `*.lock` file it finds in the git
  directory in the result (never removing one).
- **Remote**: a second command over the same master, `exec 'kill' '-TERM' '--' '-<pid>'`, with the pid
  from the `sopen-pid` line (sshd starts the session's command as the leader of a new session, so the
  pid is the group — *measured, E5, 2026-11-23*: under fish, and under bash and zsh with `-c` and
  `-l -c` emulated from a fish account, 02 §8.2; G-R3 is the receipt of sopen's own stop and of
  `stop sent` — *measured, S3-C, 2026-11-23, under zsh* (§11)); after 5 s, `kill -KILL` the same way; then sopen
  closes the command's ssh channel. While it waits, sopen keeps reading and dropping the command's
  output: the channel cannot end while the master holds output nobody reads, so a parked stream's
  stop would otherwise take the whole 5 s (E5: 0.59 s with the output read, 5.17 s without). When
  the master is gone, or no `sopen-pid` line came back, the result is `stop sent; the command may
  still be running on <host>`, never `stopped`.
  **Never by its number alone**: a group's number proves only that some group has it. The
  `sopen-pid` line also carries the leader's identity, its start time (field 22 of
  `/proc/<pid>/stat` where there is a `/proc`, else `ps -o lstart=`), and every stop session reads
  it again right before each signal, in the same `sh -c` (`-TERM`, each `-0` asking, `-KILL`):
  a group whose leader is gone, or whose pid another process has now, is not signalled, and each
  group's answer is its own. So a command whose channel has already ended (its leader exited) is
  never signalled, and a child that outlives its leader (one that ignores `SIGTERM`) is left
  running — `stop sent; the command may still be running`. The residual: the moment between the
  reading and the `kill` inside that one shell — should the leader end and its number go to a new
  group leader within it (the pid space wrapping meanwhile), that group would get the signal;
  sopen holds no ownership across a pid wrap and does not guard it. Two more residuals: a remote
  child that ignores `SIGTERM` and outlives its leader keeps running, and the stop reports `stop
  sent` (killing by the number alone could hit an unrelated group); git's own children — remote
  helpers, `ssh` — normally exit when git's pipes close, so they do not outlive a stopped git.
- **After any stop** the panel refreshes (§5) and reports the state it finds: a stop is not a rollback.
  A stopped fetch may have updated some refs, objects and `FETCH_HEAD` (git updates refs one by one);
  a stopped write may have finished a step. The log says `stopped` with what the refresh found changed.

### 3.6 Progress

stderr is split at `\r` and `\n`. The newest line of a running network command shows on the status
row (`git fetch: Receiving objects: 45% (900/2000), 12.0 MiB`); every line, `\r` updates collapsed
into their last value, goes to the command's log entry.

### 3.7 The command log

- Every command the panel runs is one entry, in memory only, never written to disk (it can hold
  remote URLs): the time, the host, the repository, the command line as `render_command` prints it
  (the environment shown only where it differs from §3.3: `GIT_EDITOR=true`,
  `GIT_COMMITTER_DATE=…`), what went on stdin (`message, 3 lines` / `todo, 12 lines`), the exit code
  or signal or `stopped`, the duration, the stdout size, and the last 64 KiB of stderr.
- The status row always names the last entry: `git push origin main · exit 0 · 1.2 s`, or its first
  error line. **Cmd/Ctrl+L** opens the log in the git panel (newest first); Enter on an entry shows
  it whole.
- `SOPEN_LOG` (sopen's debug output) also gets each command line.
- **Credentials in URLs are masked** before a URL reaches the log, `SOPEN_LOG`, a dialog or the status
  row: `https://user:secret@host/x` is shown as `https://user:***@host/x` (a password, or a token used
  as the user name of an `https` URL with no password, is replaced). git's own stderr is masked the
  same way before it is kept.

### 3.8 Errors

- A command that fails shows a dialog titled with the action, holding git's stderr (and stdout when
  stderr is empty) whole, scrollable, and the log entry's command line. Enter or Esc closes it.
- An action of several steps stops at the first failing step; the dialog lists each step's outcome
  (`done` / `failed` / `not run`), and the panel refreshes, since earlier steps may have changed things.
- `index.lock` held by another git: git's own message is shown; sopen never removes a lock file.

## 4. The graph

### 4.1 Loading

A load has a **generation** number. It runs these on workers, the first four at once (at most 3 in
flight per host, §3.2), and every result carries the generation; a result of an older generation is
dropped unread, and its stream is stopped:

| Read | Command | Parsed into |
|---|---|---|
| refs | `for-each-ref --format=%(objectname)%00%(objecttype)%00%(*objectname)%00%(refname)%00%(upstream)%00%(upstream:track) refs/heads refs/remotes refs/tags` | `RefLabel`s (an annotated tag points at its `*objectname`), each with the object id read |
| HEAD | `symbolic-ref -q HEAD`, then `rev-parse -q --verify HEAD` | the checked-out branch, or detached at `<c>`, or an empty repository |
| stashes | `stash list -z --format=%H%x1f%P%x1f%gd%x1f%ct%x1f%s` | `StashRow`s (each with all its parents: base, index, untracked) |
| status | `status --porcelain=v2 -z --branch --untracked-files=normal` (`GIT_OPTIONAL_LOCKS=0`) | `StatusEntry`s; ahead/behind of the branch; an untracked folder is one entry `dir/` (§6.1) |
| in progress | the probe of §8.5 | `InProgress` |
| commits | the log stream of §4.5 | `CommitRow`s |

- **Roots are object ids, not names.** The log starts after the refs, HEAD and stashes of the same
  generation are read, and is given their **object ids** on stdin (`log --stdin`, one id per line;
  *verified, git 2.56.1*: the same rows as `--branches --tags` for the same refs): every shown branch, remote branch (per shown remote, §4.6) and tag, HEAD, and
  each stash's first parent (so commits only a stash reaches still appear); with a branch filter, the
  chosen refs' ids only. So the graph is the history of exactly the refs its labels show, even when a
  ref moves while the log runs; a ref that moved shows at its new place on the next refresh (§5). Ids
  go on stdin because a repository's tags can exceed what one remote command line holds. With
  "Reflog commits" (§4.6) `--reflog` adds the reflogs as git reads them then. **An empty id list is
  never sent**: git then falls back to HEAD (*verified*), so a filter that leaves no ref shows
  `no refs chosen` and runs no log.
- **Commit record** (one per commit: seven fields, each ending in NUL, `-z` ending the last; git
  prints no NUL inside a name or an email and prints a message up to its first NUL, so no field can
  shift another, *verified, git 2.56.1*): `log -z --<order> --format=%H%x00%P%x00%aN%x00%aE%x00%at%x00%ct%x00%s
  [--first-parent] [--reflog] --stdin --`. `%aN`/`%aE` follow `.mailmap` as `git log` does. Text
  that is not UTF-8 shows with U+FFFD. A name, email or subject past its cap (§4.5) is read to its
  NUL and cut, ending in `…`; the rest of the log goes on. A record whose hash, parents or dates do
  not parse, that the stream ends inside, or that runs 16 MiB past its caps ends the page with an
  error on the paging line, never a panic.
- **Order**: `--date-order` by default (children always above their parents, then by commit date);
  the filters offer `--author-date-order` and `--topo-order`. One of the three is always passed: the
  lane layout needs children before parents, which plain `git log` does not promise.
- An **empty repository** (no HEAD) shows one line, `no commits yet`, and the uncommitted row when
  there are changes.

### 4.2 Rows and columns

The git panel is a window in the PC Shell look (01 §3a): its frame, its title in the top border,
heads in the dim colour, rows a cell tall, the highlight bar, the ░ scroll bar on the right border,
and a foot line. Columns, in character cells:

| Column | Width | Content |
|---|---|---|
| Graph | 2 cells per lane, at most a third of the panel; lanes beyond it are cut with `»` | §4.3–§4.4 |
| Description | the rest | labels (§4.4), then the subject |
| Date | 16 | author date, local time, `2026-11-20 10:24` |
| Author | 14 | the author's name, cut at a glyph boundary with `…` |
| Commit | 8 | the first 8 hex digits |

- When the panel narrows, Author goes first, then Date, then Commit (the file list's rule of dropping
  dates, 01 §3a).
- **Muted rows**: merge commits draw their text in the dim colour.
- **Marked rows** (Space / Ins, §9) draw their text yellow, as PC Shell draws selected files.
- **The foot**: `N commits · M more` while more can be loaded, `N commits · all loaded`, or the
  shallow note (§2.3); the branch's ahead/behind (`main ↑2 ↓1 origin/main`) on the right when it
  fits.
- Only the rows on screen are painted.

### 4.3 Lane layout (sopen's own algorithm)

`graph::layout` is pure: rows in display order in, a `RowGraph` per row out; the same input always
gives the same output.

**The row stream** is built before layout, page by page, in this order:

- The **uncommitted row** first, when there are changes: a synthetic row whose one parent is HEAD's
  commit.
- Then the log's records, except **stash internals**: a stash's own commit and its index and untracked
  parents (all known from the stash read) are dropped from the log stream wherever they appear — with
  "Reflog commits" on, `--reflog` brings them in, and `--exclude=refs/stash` does not keep them out
  *(verified, git 2.56.1)*. So a stash is drawn once, as its stash row.
- A **stash row** is a synthetic row whose one parent is the stash's base (first parent). It enters the
  stream **immediately above its base's row**, at the moment the base's record arrives (several stashes
  on one base: newest first). A stash whose base is not loaded yet waits for it; rows above never move.

For each row in order:

1. `lanes` is a list of slots, each empty or awaiting a commit (a parent seen above, not drawn yet).
2. **Column**: the lowest slot awaiting this commit; if none awaits it (a branch tip), the lowest
   empty slot, else a new slot at the end.
3. **Joins**: every other slot awaiting this commit ends here — a line from that slot (top of the row)
   to the column — and becomes empty.
4. **First parent** takes over the column's slot (it now awaits the first parent), even when another
   slot already awaits that parent: the two lines run side by side and join at the parent's row
   (step 3). With `--first-parent` only the first parent is considered.
5. **Other parents**, in order: a parent some slot already awaits gets a line from the column to that
   slot; otherwise it takes the lowest empty slot right of the column, else a new slot at the end.
6. **No parents** (a root commit): the column's slot becomes empty after the node; no line leaves it
   downward, and the next row may take the slot.
7. Trailing empty slots are dropped. A slot still awaiting a commit that never comes (not loaded yet,
   or beyond a shallow boundary) runs to the bottom of the loaded rows.

**Stored per row** (`RowGraph`): the row's column, its node kind (§4.4), and only the segments that
touch its node — into it from above, joins, and out to each parent's slot. **Vertical pass-through
lines are not stored**: the layout keeps one interval per slot occupancy (slot, colour, first row, last
row), and painting derives the pass-throughs of the visible rows from those intervals. A slot keeps its
colour index for one occupancy; a new occupancy takes the next index in turn. Painting draws the lanes
in cells, two a lane (`git_panel/lanes.rs`, X4-lanes; 10 §6): a lane's first cell holds its node or its
line, its second the line of a lane change, which runs along its node's row — `─` from the node, then a
corner at the lane, `┐` down to a new lane, `┘` up from a lane that joins — since one row a commit
leaves no row between for a diagonal, and a cell holds none. Where pieces meet, a cell joins them:
`├ ┤ ┬ ┴` where lines join (an octopus's lines share its row; a line to a lane left of its node ends on
the line passing by there, `├`), `┼` where a lane change crosses a line passing by. A cell takes the
colour of the piece drawn last, in the window's egui order — lines passing by, then the row's lane
changes, then the node — and the lane changes among themselves longest first (on a tie the one coming
in, then the one going down; egui's diagonals share no stretch, so it has no such order): a crossing
is the crossing line's colour and a stretch two lane changes share the nearer lane's, so a line's
colour changes where it turns. **A limit**: where a longer lane change goes on past a join onto a lane
passing by (a merge whose parents two lanes on one side await), the join is `┼`, as a crossing is,
told apart only by its colour, the joined lane's; where the two lanes share a colour (five apart, the
cycle's length) the row reads as a crossing. One row a commit has no room to show more; the details
pane lists the parents (§6.1). Until X4-graph draws these cells, the window's egui graph still draws a
lane change as one diagonal across half a row, and the uncommitted row's line dashed.

- **Measured** with these rules (a script over `git log --parents`, 2026-11-20): the 100 000-commit
  fixture of §4.5 has at most 2 slots, 2.0 node segments per row; a wide fixture (60 branches committed
  in turn and merged back, 97 761 commits) has **61 slots**: storing every line would be 63 segments
  per row (6.2 million), node segments only are 2.0 per row (195 741) plus 61 intervals. Memory is
  therefore proportional to rows and edges, not rows × lanes.

Tests (`src/git/tests/graph.rs`), each a table of small graphs with the expected columns and
segments written out: a line; a branch and its merge; an octopus merge; two tips sharing a parent;
a criss-cross merge; a parent not loaded; `--first-parent`; the uncommitted row with HEAD far down;
a root commit in the middle of the stream (its column is free on the next row) and two roots; stash
rows: one stash on a loaded base, two on one base, one whose base arrives on the next page, and one
whose commits also come in through `--reflog` (drawn once). Properties over 1 000 graphs from a fixed-seed generator: every segment joins two
adjacent rows at the same slot on both sides; a commit's node is at the slot its children's lines end
in; layout of the first k rows is a prefix of the layout of all rows (paging never redraws rows
above); the result does not depend on anything but the input (run twice, compare).

### 4.4 Nodes and labels

| Thing | Drawn as |
|---|---|
| a commit | `■` in its lane's colour |
| the checked-out commit | `◙` |
| a stash | `≡`, its row's text dim, the subject after a `stash@{n}` label |
| the uncommitted row | `○` in the dim colour, a dotted dim line to HEAD's row, `:` a cell (code page 437 has no dashed line; `:` is in it, and stays one cell, apart from `\|`, in an ambiguous-wide terminal; egui's window draws it dashed until X4-graph); text `Uncommitted changes (N)`, author `*`, commit `*`, date blank |
| local branch | its name, black on light green |
| the checked-out branch | `►` before its name |
| remote branch | `origin/main`, black on light cyan |
| a local and a remote branch on the same commit with the same name | one label `main↔origin` (more remotes: `main↔origin,up`) |
| tag | its name, black on yellow |
| detached HEAD | `HEAD`, black on white, first |
| `origin/HEAD` | not drawn |
| the label cursor (§9) | the label inverted |

- Glyphs are code page 437 characters the VGA font has: lanes are `│ ─ ┐ ┘ ├ ┤ ┬ ┴ ┼` (§4.3; joins
  come only from the right, so `└` and a lone `┌` never stand), since CP437 has no rounded corner
  (`╭ ╮ ╰ ╯`) and no dashed line. In an ambiguous-wide terminal they take 10 §10's stand-ins at the
  source: `| - +`, `#` for `■`, `.` for `≡` and `○`; `◙`, `»` and `:` are narrow and stay. Lane
  colours cycle through light cyan, yellow, light green, light magenta and white — the bright CGA
  colours that read on blue and are not the highlight bar's light red. Every colour comes from
  `chrome::git` (AGENTS.md "One look, one place").
- Labels come before the subject, in this order: HEAD, the checked-out branch, other local branches,
  remote branches, tags, stashes. Labels that do not fit end in `+N`.

### 4.5 Paging

- **Page size 500.** The log is one stream (`history.rs`): `git log` runs without `--max-count`; sopen
  reads 500 records and stops reading; git then blocks on the full pipe (locally, measured below) or,
  remotely, writing its stdout pipe to sshd (*measured, E5, 2026-11-23*: `anon_pipe_write`, about
  2.2 MB beyond what sopen had read; how that backlog splits between the ssh channel's window and
  the pipes is an *(estimate)*; G-P3 is the receipt) and waits. "More" reads the next 500 from the same stream, so pages are
  one walk of the history in one order, with no rows repeated or skipped.
- More is read when the bar comes within one screen of the last loaded row, on ↓ on the last loaded
  row, or on a click on the paging line (the bar never stands on the paging line itself). The stream
  ends: `all loaded`.
- A refresh that changes refs or HEAD (§5) ends the stream and starts a new one, reading as many
  pages as were loaded; the bar stays on the same commit when it is still there.
- **Memory grows only with the pages read.** A row stores its hash, its parents (as row indices or
  hashes), two dates, the author's name and email (cut at 256 bytes each), the subject (cut at 1 KiB)
  and its node segments (about 2, §4.3) — no per-lane data. The measured records average 134 bytes of
  log text; so a fully loaded 100 000-commit history is about 25 MB typical, and about 150 MB with
  every subject and name at its cap — an estimate that assumes the measured distribution of about one
  parent and two node segments per row (§4.3); a history with many octopus merges grows with its
  edges. The graph's width adds no per-row cost (61 lanes measured).
- A parked stream holds a process (and, on a remote, an ssh session), so a panel **hidden for more than
  30 s** ends its stream (§3.5), and a dropped panel ends it at once. Shown again within 30 s — the bar
  moving from a folder to a file and back, 02 §3.1 — it continues the same stream; later, it starts a
  new one and reads as many pages as were loaded, keeping the bar on its commit.
- **Measured** (2026-11-20, Apple M5 Max, git 2.56.1, a `fast-import`ed repository of 100 000 commits
  with 1 999 merges, warm cache, median of 7): first 500 records of the stream in **303 ms** without a
  commit-graph file (`--author-date-order` 300 ms, `--topo-order` 337 ms; git walks every commit
  first) and **9 ms** with one; reading the whole history took 0.49 s (13.4 MB of records); a
  `--skip=50000 --max-count=501` page 0.36–0.52 s, which is why pages are not re-run with `--skip`.
  The wide fixture of §4.3 (97 761 commits, 61 lanes) gave its first 500 records in 622 ms without a
  commit-graph file (median of 3).

### 4.6 Filters

Filters belong to the window and reset nothing on the disk.

| Filter | Default | Effect |
|---|---|---|
| Branches | all | a list of local and remote branches with type-to-filter; the chosen refs become the log's revisions |
| Remotes | all shown | each remote on or off (`--remotes=<r>` per shown remote; a hidden remote's labels go too) |
| Tags | on | off: no tag labels, no `--tags` |
| Stashes | on | off: no stash rows, no stash parents in the revisions |
| First parent only | off | `--first-parent` |
| Order | date | date / author date / topological (§4.1) |
| Reflog commits | off | `--reflog` with every branch shown |

A label's menu has "Show only this branch" and "Show all branches" (§7.2). The frame title adds
`[filtered]` while any filter differs from its default.

### 4.7 Find and history search

- **Find** (Cmd/Ctrl+F, or a typed letter in the graph, which starts the find field with it): a
  one-line field over the graph, as the hex window's Go to field (01 §6a). It matches, case-insensitive
  by default (a toggle makes it case-sensitive), the loaded rows' subject, labels, author name and
  hash prefix; the bar jumps to the next match as you type; Enter = next, Shift+Enter = previous; the
  foot says `3 of 17`. Esc closes it with the bar where it is.
- **History search** (Cmd/Ctrl+Shift+F): a dialog with message (`--grep`, git's regular expression,
  `-i` unless case is asked for), author (`--author`), text added or removed (`-S`), changed path
  (`-- <p>`, with `--follow` when it is one file), since/until dates. It runs `git log` with those
  options and the panel's revisions, and shows the result as a flat list (no lanes; a search drops
  most parents), paged like the graph. Enter on a result opens its details; Esc returns to the graph
  with the bar on that commit when it is loaded.

## 5. Refresh

| Trigger | Refresh |
|---|---|
| a panel command finished (any class but read) | full: every read of §4.1; once, after the action's last step |
| the window gains focus | soft |
| the panel is shown again (pinned, Tab, the bar back on a folder) | soft, unless the last refresh is younger than 10 s |
| Cmd/Ctrl+R in the git panel | hard: detection, git version and every read again |
| `BrowserEvent::FilesChanged` inside the top level (02 §7) | soft |

- **Soft**: refs, HEAD, status, stashes and the probe. When refs, HEAD and stashes are unchanged, the
  graph's rows stay and only the uncommitted row, the foot and the banner change; otherwise the log
  stream restarts (§4.5). When the uncommitted row appears or disappears, the lane layout (§4.3) runs
  again over the rows already loaded — no history is read again; the row takes slot 0 and continues
  down HEAD's lane, so HEAD's lane can move.
- The bar sweeping over files and folders therefore costs nothing in git: the side follows O6 only
  after 150 ms of rest (02 §3.1), a stream survives 30 s hidden (§4.5), and a refresh younger than 10 s
  is not repeated.
- There is no file watcher and no polling (01 §9 keeps file watching out of scope; over SFTP there is
  none).
- A refresh never moves the bar off its commit when the commit is still loaded.

## 6. Details, diffs, compare, files at a revision

### 6.1 The details pane

- Enter or F3 on a commit row opens the **details pane** in the lower part of the git panel (the
  graph keeps the upper part, at least 8 rows; Cmd/Ctrl+Shift+G makes the git panel take both sides
  of the window and back, §9). The keyboard moves to the pane's file list; Esc gives it back to the
  graph and leaves the pane following the bar; Esc again closes it.
- **Summary**: `Commit` (full hash), `Parents` (hashes; Enter on one moves the bar to it when
  loaded), `Author` (name <email>), `Author date`, `Committer` and `Commit date` (shown only when they
  differ from the author's), `In` (branches and tags that contain the commit, filled in when
  `for-each-ref --contains <c>` answers; `…` until then, bounded as a read), then the whole message,
  scrollable. From `log -1 -z --format=%H%x00%P%x00%aN%x00%aE%x00%at%x00%cN%x00%cE%x00%ct%x00%B <c> --`
  (nine fields, each ending in NUL, as the commit record's; past 1 MiB an error).
  These reads and the two below end in `--`: without it a work-tree file named like the hash makes
  git stop with "ambiguous argument" (exit 128, reproduced).
- **Files**: one row per changed path — the change letter (`A` added, `M` modified, `D` deleted,
  `R` renamed with `old → new`, `T` type changed, `U` unmerged, `?` untracked), the path, and
  `+adds -dels` for text (blank for binary). From `diff -z --find-renames --name-status <old> <new> --`
  and the same with `--numstat`, the two endpoints from §6.2's table. For a merge, the diff is
  against the first parent.
- A file whose diff was opened in this window is marked `√` (the "viewed" mark, for the window only).
- **A stash**: its files against its first parent, plus its untracked files (the stash's third
  parent) as `?`.
- **The uncommitted row**: three groups, `Staged`, `Not staged`, `Untracked` (and `Conflicts` first
  during a merge, rebase, cherry-pick or revert), from the status read; the actions of §7.6 act on the
  file under the bar or the marked files.
- **An untracked folder** is one row `dir/` with `►` (status reports it so with
  `--untracked-files=normal`, *verified, git 2.56.1*); → or Enter lists its files, read then:
  `ls-files -z --others --exclude-standard -- dir/` (*verified*). On a folder row, Stage adds everything
  in it, Discard removes it with everything in it (backed up, §8.2), and the confirmation counts its
  files from that listing.
- **An annotated tag's details** (§7.4): the tag object (`cat-file -p <t>`) in the pane.

### 6.2 Diffs in the text viewer

Every file row has two **endpoints**, old and new — a blob, the index, the working file, or **none**:

| Row | Old | New |
|---|---|---|
| a commit's file (not a root) | `<c>^1:<old path>` | `<c>:<path>` |
| a root commit's file | none (the diff runs against the empty tree: `hash-object -t tree --stdin </dev/null`, which also works in a SHA-256 repository) | `<c>:<path>` |
| added (`A`) | none | as above |
| deleted (`D`) | as above | none |
| a stash's tracked file | `<s>^1:<path>` | `<s>:<path>` |
| a stash's untracked file | none | `<s>^3:<path>` |
| a comparison (§6.4) | `<from>:<old path>` | `<to>:<path>`, or the working file when `to` is the uncommitted row |
| uncommitted, staged | `HEAD:<path>` (none when HEAD lacks it) | the index |
| uncommitted, not staged | the index | the working file |
| uncommitted, untracked | none | the working file |

- Enter or F3 on a file row writes the diff of its endpoints to a file and opens it in sopen's text
  viewer, read-only, over both sides, titled `host:/top/path @ abc12345` (02 §7 `OpenViewer`). Esc
  returns to the git panel as it was. The diff is `diff --no-color --find-renames` over the two
  endpoints' trees, limited to the row's path(s) (`<old path>` and `<path>` for a rename): `<old>
  <new>` commits; the empty tree for none (*verified, git 2.56.1*: a root commit's file against the
  empty tree, and a stash's untracked file against `<s>^3`); `--cached` and the plain form for the two
  uncommitted cases. It is written to `<cache>/.revisions/<window>/<n>/<name>.diff` — a new dot-named
  cache root beside 01 §5's, never a mirrored host's folder (folders 0700, files 0600, removed when the
  window closes) — so the viewer highlights it as a diff (`chrome::syntax_theme` has the diff scopes).
- A diff over 256 MiB is cut there with a last line saying so. A binary file's diff is git's one
  `Binary files … differ` line; the file row's menu then offers both versions (§6.3).
- **The uncommitted row's** untracked file opens as the working file itself, as Enter in the list
  would (01 §3a). Nowhere else: a stash's untracked file always opens from `<s>^3:<path>`, even when a
  working file of the same name exists.

### 6.3 A file at a revision

- Shift+F3 on a file row opens its **new** endpoint; the menu's "Version before" opens its **old**
  one. A blob endpoint is read with `cat-file blob <rev>:<path>` into
  `<cache>/.revisions/<window>/<n>/<name>` (the file's own name, so the viewer is chosen by its kind,
  01 §3) and opened read-only in the viewer for its kind: text, image, media or hex. An endpoint that is
  none is not offered (the menu item is shown dim with `not in this version`); the index is read with
  `cat-file blob :<path>`; the working file opens as Enter in the list would.
- "Diff against the working file" keeps **both paths**: the path at the revision and the path **now**
  — found by following the file's renames since the revision (`diff -z --name-status --find-renames
  --diff-filter=R <rev>` against the work tree, *verified*) — and diffs them as one rename:
  `diff --no-color --find-renames <rev> -- <path then> <path now>`. A pure rename shows as
  `rename from … rename to …` with no hunk, a rename with changes as the rename and its hunks
  (*verified, git 2.56.1*; `diff <rev> -- <path now>` alone would show the file as added). F4 opens the
  path now; a file deleted since says so on the status row.
- **Read-only** means the viewer's Save and Save copy are off, with the reason on hover — a row
  where there is no hover (a screen in cells, 10 §6: the binary dialog's): `a file at a revision
  has no place to be saved` (the viewers' `read_only` flag, slice S2-E, 02 §8).
- F4 on a file row opens the working file (its path now) in the editor, as the list's F4 does (01 A2).

### 6.4 Compare two commits

- Mark one row (Space or Ins), put the bar on another and press F3: the details pane shows the
  comparison: `From` (the lower row, the older) and `To` (the upper), and the files changed between
  them (`diff --name-status` and `--numstat` of the pair); diffs open as in §6.2. The uncommitted row
  can be either end (`diff <c>` against the work tree, plus untracked files from the status).
- With marks on more than one row, F3 opens the bar's own details; the status row says compare takes
  exactly one marked row.

## 7. Actions

Every action is in the menu of the object under the bar (F10, §9) and some also have a key. An action
with an effect beyond the view opens its dialog first (§8.1); the dialog is its confirmation, Enter
runs it, Esc cancels. The **id** is the action's name in `SOPEN_STATE dialog=` (02 §9) and in
`actions.rs`. "Backup" names the kinds of §8.2 written before the first step.

### 7.1 A commit row

| id | Action | Options (default) | Commands | Backup |
|---|---|---|---|---|
| `tag` | Tag… | name; annotated (default) or lightweight; message (annotated); push to: no remote (default) or one | `tag <t> <c>` · `tag -a -F - <t> <c>` (message on stdin, may be empty); then `push --progress <r> refs/tags/<t>` | — |
| `tag` (name taken) | the dialog asks "Move tag `<t>` from `<v>` to `<c>`?" | — | adds `-f` | `tags/<t>` |
| `branch` | New branch… | name; check it out (off) | `branch <b> <c>` · `checkout --no-overwrite-ignore -b <b> <c>` | — |
| `branch` (name taken) | "Move branch `<b>` from `<v>` to `<c>`?" (refused for the checked-out branch: Reset is offered instead) | — | `branch -f <b> <c>` | `heads/<b>` |
| `move-branch` | Move a branch here… | which local branch (not the checked-out one) | `update-ref -m 'sopen: move <b>' refs/heads/<b> <c> <v>` (fails when the branch moved meanwhile, *verified, git 2.56.1*) | `heads/<b>` |
| `checkout` | Check out (detached HEAD) | "Don't ask again in this window" (off) | `checkout --no-overwrite-ignore --detach <c>` | — |
| `pick` | Cherry-pick… | parent (merge commits: 1…n, default 1); record origin `-x` (off); don't commit (off). With marked rows: all of them, oldest first | `cherry-pick [--no-commit] [-x] [-m <n>] <c>…`; refused when an ignored file is in the way (§8.6) | — |
| `revert` | Revert… | parent (merge commits, default 1). With marked rows: all, newest first | `revert --no-edit [-m <n>] <c>…`; refused when an ignored file is in the way (§8.6) | — |
| `merge` | Merge into the current branch… | fast-forward: when possible (default) / never `--no-ff` / only `--ff-only`; squash (off); don't commit (off); on conflicts prefer: nothing (default) / `-X ours` / `-X theirs`; allow unrelated histories (off); skip hooks (off) | `merge --no-edit --no-overwrite-ignore [--no-ff\|--ff-only] [--squash] [--no-commit] [-X ours\|-X theirs] [--allow-unrelated-histories] [--no-verify] <c>`; after a squash (without "don't commit"): the Commit dialog (§7.6) opens with git's prepared squash message | — |
| `rebase` | Rebase the current branch onto this… | ignore dates `--ignore-date` (off); stash my changes for it `--autostash` (on when there are uncommitted changes); move other branches along `--update-refs` (off; git ≥ 2.38); plan the commits first → the planner (§7.8) | `rebase [--ignore-date] [--autostash] [--update-refs] <c>`; the confirmation lists merge commits of the range, which a plain rebase flattens (*verified, git 2.56.1*), and offers the planner, which refuses them (§7.8.2) | `heads/<current>`; with `--update-refs` also `heads/<b>` of every branch it moves (§8.2) |
| `reset` | Reset the current branch here… | soft / mixed (default) / keep (refuses to lose changes) / hard | `reset --soft\|--mixed\|--keep\|--hard <c>` | `heads/<current>`; with hard also `worktree`, and `files` for untracked or ignored files in the way (§8.6) |
| `archive` | Save archive… | zip (default) or tar; the path field of 01 §6a ("Save to…": download folder, never over anything) | `archive --format=zip\|tar <c>`, stdout streamed into 01 §6a's staged file and published as it is (so a remote archive lands on this machine) | — |
| `copy-hash`, `copy-subject` | Copy hash / Copy subject | — | the system clipboard | — |
| `compare` | Compare with the marked commit | — | §6.4 | — |
| history edits | `reword`, `author`, `squash`, `fixup`, `move-up`, `move-down`, `drop`, `edit`, `split`, `fold`, `plan` | §7.8 | §7.8 | `heads/<current>` |

### 7.2 A local branch label

| id | Action | Options (default) | Commands | Backup |
|---|---|---|---|---|
| `checkout-branch` | Check out | — (not on the checked-out branch) | `checkout --no-overwrite-ignore <b>` | — |
| `rename` | Rename… (F2 on the label) | new name | `branch -m <b> <new>` | — |
| `delete-branch` | Delete… (F8 on the label) | force (off); also delete it on: each remote that has `<r>/<b>` (off) | per chosen remote, first: §8.4's lease delete; then `branch -d <b>` (`-D` with force) | `heads/<b>`; `pushed/<r>/heads/<b>` per remote |
| `delete-branch` (not merged) | git refuses (`not fully merged` in its C-locale message) → a second dialog "`<b>` has commits no other branch has: delete anyway?" | — | `branch -D <b>` | (already written) |
| `merge` | Merge into the current branch… | as §7.1 | `merge … <b>` | — |
| `rebase` | Rebase the current branch onto this… | as §7.1 | `rebase … <b>` | `heads/<current>` |
| `push` | Push… | remotes (more than one: a list; preselected: the branch's push remote, else its upstream's remote, else `remote.pushDefault`, else `origin`, else the first); set upstream (on); overwrite the remote branch, with lease (off); skip hooks (off) | per remote in turn, stopping at the first failure: `push --progress [--set-upstream] [--no-verify] <r> refs/heads/<b>:refs/heads/<b>`; overwrite: §8.4 | overwrite: `pushed/<r>/heads/<b>` |
| `archive` | Save archive… | as §7.1 | `archive … refs/heads/<b>` | — |
| `only-branch`, `all-branches` | Show only this branch / Show all branches | — | the filter (§4.6) | — |
| `copy-name` | Copy name | — | clipboard | — |

### 7.3 A remote branch label (`<r>/<b>`)

| id | Action | Options (default) | Commands | Backup |
|---|---|---|---|---|
| `checkout-remote` | Check out as a local branch… | local name (prefilled `<b>`) | `checkout --no-overwrite-ignore -b <name> --track <r>/<b>`; when `<name>` exists, the dialog offers "check out `<name>` and pull `<r>/<b>` into it": `checkout --no-overwrite-ignore <name>`, then `pull` below | (pull's) |
| `delete-remote-branch` | Delete on the remote… (F8 on the label) | — | §8.4's lease delete; then, if `refs/remotes/<r>/<b>` is still there, `branch -d -r <r>/<b>` | `pushed/<r>/heads/<b>` |
| `fetch-into` | Fetch into the local branch… | force, when it is not a fast-forward (off). Only when a local `<b>` exists and is not checked out | `fetch --progress --no-prune --no-prune-tags <r> refs/heads/<b>:refs/heads/<b>` (`+refs/heads/<b>:…` with force) | force: `heads/<b>` |
| `merge` | Merge into the current branch… | as §7.1 | `merge … <r>/<b>` | — |
| `pull` | Pull into the current branch… | how: merge (default) / rebase / fast-forward only; with merge: never fast-forward (off), squash (off) | step 1 (network, stoppable): `fetch --progress --no-prune --no-prune-tags <r> refs/heads/<b>`; step 2 (write): `merge --no-edit --no-overwrite-ignore [--no-ff\|--ff-only] [--squash] FETCH_HEAD` or `rebase FETCH_HEAD` — what `git pull` does, split so that the transfer is its own step *(estimate)*; with rebase the confirmation lists merges it would flatten, as `rebase` does | rebase: `heads/<current>` |
| `archive`, `only-branch`, `all-branches`, `copy-name` | as §7.2 | — | — | — |

### 7.4 A tag label

| id | Action | Options (default) | Commands | Backup |
|---|---|---|---|---|
| `tag-details` | Details (annotated tags) | — | `cat-file -p <t>` in the details pane | — |
| `delete-tag` | Delete… (F8 on the label) | also on a remote: none (default) or one | remote first (§8.4's lease delete of `refs/tags/<t>`), then `tag -d <t>` | `tags/<t>`; `pushed/<r>/tags/<t>` |
| `push-tag` | Push… | remotes (list when more than one); replace the remote's tag when it differs, with lease (off) | per remote in turn: `push --progress <r> refs/tags/<t>`; replace: §8.4 | replace: `pushed/<r>/tags/<t>` |
| `move-tag` | Move tag here… (from a commit's menu) | which tag; keep it annotated with its message (prefilled, editable) | `tag -f <t> <c>` · `tag -f -a -F - <t> <c>` | `tags/<t>` |
| `archive`, `copy-name` | as §7.2 | — | `archive … refs/tags/<t>` | — |

### 7.5 A stash row

The stash is acted on by its hash where git takes one (`stash apply <c>`), and by `stash@{n}` where
it needs the list entry (`pop`, `drop`, `branch`); then `rev-parse stash@{n}` is checked against the
hash first, and a mismatch (the list changed) refreshes instead of acting.

| id | Action | Options (default) | Commands | Backup |
|---|---|---|---|---|
| `stash-apply` | Apply… | restore the index too `--index` (off) | `stash apply [--index] <c>` | — |
| `stash-pop` | Pop… | `--index` (off) | `stash pop [--index] stash@{n}` | `stash` |
| `stash-branch` | New branch from it… | name | `stash branch <b> stash@{n}` | `stash` |
| `stash-drop` | Drop… (F8) | — | `stash drop stash@{n}` | `stash` |
| `copy-name`, `copy-hash` | Copy `stash@{n}` / the hash | — | clipboard | — |

### 7.6 The uncommitted row and its files

| id | Action | Options (default) | Commands | Backup |
|---|---|---|---|---|
| `stage`, `unstage` | Space on a file in the details pane toggles it; Stage all / Unstage all in the menu | — | `add -- <p>…` · `restore --staged -- <p>…` (git < 2.23: `reset -q -- <p>…`) | — |
| `commit` | Commit… (F7 on the uncommitted row) | message; amend the last commit instead (off); skip hooks (off) | `commit --cleanup=verbatim [--no-verify] -F -` · amend: `commit --amend --cleanup=verbatim [--no-verify] -F -` | amend: `heads/<current>` |
| `amend` | Add the staged changes to the last commit | keep its message (default) or edit it (prefilled) | `commit --amend --no-edit` · `commit --amend --cleanup=verbatim -F -` *(inventory §7, verified there)* | `heads/<current>` |
| `stash` | Stash the changes… | message (one line); include untracked files (off) | `stash push [--include-untracked] [-m <message>]` | — |
| `discard` | Discard changes… (F8 on a file) | the file under the bar or the marked files | tracked: `checkout HEAD -- <p>…`; untracked (a file or a `dir/` row): `clean -f -d -- <p>…` | `worktree`; untracked: `files` (§8.2) |
| `reset-changes` | Reset the changes… | mixed (default: unstage all) / hard (discard all tracked changes) | `reset --mixed\|--hard HEAD` | hard: `worktree` |
| `clean` | Remove untracked files… | folders too `-d` (on) | `clean -f [-d]` (ignored files stay: no `-x`) | `files` (the untracked files it removes) |
| `ours`, `theirs`, `resolved` | conflicts: Take ours / Take theirs / Mark resolved; Redo the merge of the file | — | `checkout --ours\|--theirs -- <p>` · `add -- <p>` · redo: `checkout --merge -- <p>` (puts the conflict and its markers back, *verified, git 2.56.1*, before Mark resolved) | ours / theirs / redo: `files` of the file's bytes as they are (§8.5) |
| editor | F4: the file under the bar, or the top level, in VS Code / Zed (01 A2) | — | — | — |

The uncommitted row is the place where Git Graph hands over to VS Code's own source-control view;
sopen has none to hand over to, so staging, committing and amending are here (message 14: "자유롭게
git 작업할 수 있도록").

### 7.7 The repository

| id | Action | Options (default) | Commands | Backup |
|---|---|---|---|---|
| `fetch` | Fetch all remotes (Cmd/Ctrl+Shift+R) | — (runs at once; the dialog form has prune (off), prune tags (off)) | `fetch --progress --all --prune\|--no-prune --prune-tags\|--no-prune-tags` (always both, so `fetch.prune`, `remote.<r>.prune` and `pruneTags` in the config never prune behind the dialog's back; *verified, git 2.56.1*) | with prune: `remotes/<r>/*` of every remote; with prune tags also `tags/*` — trimmed after (§8.2) |
| `fetch-remote` | Fetch a remote… | remote; prune (off); prune tags (off) | `fetch --progress <r> --prune\|--no-prune --prune-tags\|--no-prune-tags` | as `fetch`, for `<r>` |
| `prune-remote` | Prune a remote… | remote | `remote prune <r>` | `remotes/<r>/*`, trimmed after |
| `add-remote` | Add a remote… | name; fetch URL; push URL (blank: the fetch URL); fetch now (on) | `remote add <r> <url>` · `remote set-url --push <r> <url>` · `fetch --progress --no-prune --no-prune-tags <r>` (§3.3) | — |
| `edit-remote` | Edit a remote… | name, fetch URL, push URL (prefilled) | only what changed: `remote rename <old> <new>` · `remote set-url <r> <url>` · `remote set-url --push <r> <url>` | — |
| `delete-remote` | Delete a remote… | remote | `remote remove <r>` | `remotes/<r>/*` (its remote-tracking refs) |
| `remote-visibility` | Show / hide a remote's branches | — | the filter (§4.6) | — |
| `user` | Name and email… | name; email (prefilled: this repository's, else global); for this repository (default when set here) or globally | `config --local\|--global user.name <n>` · `user.email <e>`; switching to global with values set here: `config --local --unset-all user.name` / `user.email` | — |
| `user-remove` | Remove name and email… | this repository / globally | `config --local\|--global --unset-all user.name` · `user.email` | — |
| `refresh`, `find`, `search`, `filters`, `log`, `backups`, `undo` | §5, §4.7, §4.6, §3.7, §8.2, §8.3 | — | — | — |

### 7.8 History edits

These rewrite commits of the current branch (or of a detached HEAD): ancestors of HEAD, from a base
up to HEAD. Every one of them is a run of the **planner**; the quick edits are plans with one change,
shown in their confirmation.

#### 7.8.1 Quick edits (from a commit's menu)

| id | Action (key) | Options | The plan |
|---|---|---|---|
| `reword` | Reword… (F2 on a commit) | the message, prefilled | HEAD: `commit --amend --only --allow-empty --cleanup=verbatim -F -` (no plan; *inventory §7, verified there with git 2.56.1: a staged file stays staged*); older: `pick <c>` + the message line |
| `author` | Author and date… | author name, email, author date (prefilled); committer date: keep (default) / now / same as the author date | HEAD: `commit --amend --only --allow-empty --cleanup=verbatim -F - --author=<a> --date=<d>`, the commit's own message on stdin, with `GIT_COMMITTER_DATE` per the choice — not `--no-edit`, which cleans the message up again: trailing spaces and blank lines went, and with `--cleanup=verbatim` a leading blank line still went (*verified, git 2.56.1*); older: `pick <c>` + the author line. A commit whose message is not valid UTF-8 is refused, as for `reword`: its bytes could not be kept (see the checks, §7.8.2) |
| `squash` | Squash into the commit below… | the message: both messages, the older first, a blank line between, editable — each whole, byte for byte, the blank line added after the older one's last line (`rebase::squash_message`, the planner pane's and the todo's one rule) | `pick <parent>`, `fixup <c>`, the message line |
| `fixup` | Fix up into the commit below | — | `pick <parent>`, `fixup <c>` (the older message stays) |
| `move-up`, `move-down` | Move up / Move down | — | the two lines swapped |
| `drop` | Drop… (F8 on a commit) | — | `drop <c>` (any commit in the range, not only a chain ending at HEAD) |
| `edit` | Stop here to edit… | — | `edit <c>`: the run stops after `<c>`; the banner (§8.5) offers Amend, Split, Continue, Abort |
| `split` | Split… | — (refused on a root commit: it has no `HEAD^`, *verified*) | `edit <c>`; at the stop sopen records the commit's paths (`diff-tree -r -z --name-only --no-renames <c>^1 <c>`: both names of a rename — `diff --name-only` named only the new one, and Continue then let the old name's deletion stay uncommitted, *verified, git 2.56.1*) and runs `reset --mixed <c>^1` (the parent by its hash; backup `detached`, undone in its own mode, §8.3): the commit's changes are now in the uncommitted row — files it added as **untracked** — to be committed in parts (file by file: stage, Commit…). Split is refused away from an edit stop at `<c>` — a rebase stopped by a conflict or a failed `exec` line is not one: the last line done must be `edit` (*verified, git 2.56.1*: a stop at a message line a `commit-msg` hook refused) — on a commit that changes no file, and with uncommitted changes at the stop. sopen's Continue is refused while any recorded path still differs from HEAD (`status --porcelain=v2 -z --untracked-files=all --ignored -- <paths>` not empty): git itself would continue and leave an added file out of history, untracked (*verified, git 2.56.1*); without `--ignored`, an added file that `.gitignore` matches did not show and Continue went ahead without it (*verified, git 2.56.1*). Parts finer than a file are not offered (§10.2). |
| `fold` | Fold the staged changes into this commit… | — (refused with nothing staged) | `commit --fixup=<c>` (git's own `fixup! <subject>` message, no editor, *verified, git 2.56.1* under `GIT_EDITOR=false`), then the new commit's line right after `<c>`'s as `fixup HEAD fixup! <subject>`: git resolves each name of the todo when it reads it, before the rebase checks anything out, so `HEAD` is still the commit `--fixup` made (*verified, git 2.56.1*: into an older commit, into HEAD, on a detached HEAD, with unstaged changes stashed for the run). Backup also `worktree` (§8.2): Undo, even after an Abort that leaves HEAD at that commit, puts the staged changes back staged; without it they were left only in that commit (*verified, git 2.56.1*) |
| `plan` | Edit history from here… (Cmd/Ctrl+E) | — | opens the planner with base = this commit's row: the rows above it up to HEAD |

A quick edit other than the HEAD forms runs over the range from `<c>`'s parent (`squash`, `fixup`
and `move-down`: its grandparent), or `--root` where there is none, with `--autostash` when there are
uncommitted changes (its dialog has no option for it; the confirmation says so, §7.8.2's checks).

#### 7.8.2 The planner

- A pane over the git panel listing the range's commits, newest at the top as in the graph: verb,
  hash, subject, author, date. Verbs: `pick`, `reword`, `edit`, `squash`, `fixup`, `drop`; an author
  mark `A` when the author or date is changed.
- **Keys**: ↑/↓ the bar; `P` `R` `E` `S` `F` `D` set the bar's verb (git's own todo letters,
  git-rebase(1)); `A` author and date; Cmd/Ctrl+↑/↓ move the row; Enter edits the message of a
  `reword` or `squash` row (a squash row's message is the group's); Cmd/Ctrl+Enter runs; Esc closes
  (asks when something changed: a row's verb, place, author or typed message, or a run option against
  how the planner opened). Typed letters set verbs here; they do not start Find.
- **Checks before the run**: the range holds no merge commit (a range with merges is refused, naming
  them: pick a base above the newest merge, §10.2); a `squash` or `fixup` row has a non-dropped row
  below it; the uncommitted changes are stashed for the run (`--autostash`, shown; on when the
  planner opens over uncommitted changes) or the run is refused; no operation is in progress (§8.5);
  commits of the range that a remote-tracking branch already contains are named ("already on
  `origin/main`: you will need to push with overwrite") — the same warning every rewrite gives,
  quick edits, `amend` and the HEAD forms of `reword` and `author` included. git's `--autostash`
  applies its stash without `--index`: every change comes back, but what was staged comes back
  unstaged (*verified, git 2.56.1*); the confirmation says so, and the run backs up `worktree` so
  Undo puts both back as they were (§8.2, §8.3). Planner runs need git 2.22
  (`--reschedule-failed-exec`); below it they are refused. An author change on a `squash` or
  `fixup` row would be lost — its commit goes into the group's first row, whose author the result
  keeps — so it is a check that fails, in either order of setting them: "`<c>`: its author would be
  lost — it is squashed into `<head>`; set the author on `<head>`". **Only UTF-8 is rewritten**: a
  commit whose message or author sopen's own lines write (a reword, an author change, a squash
  group's first and squash rows; the HEAD forms) is read as stored (`cat-file commit <c>`) and
  refused when it has an `encoding` header other than UTF-8 or bytes that are not UTF-8 —
  "`<c>`: its message is not UTF-8 (ISO-8859-1); sopen rewrites UTF-8 only". `git log` re-encodes
  a message and its author per that header (sopen passes `i18n.commitEncoding=UTF-8`, §3.3), so the
  range read alone cannot see it (*verified, git 2.56.1*). git's own `pick` and `fixup` of such a
  commit are left as they are.
- **Other branches in the range**: with `--update-refs` (offered on git ≥ 2.38), every local branch
  whose tip is in the range — `for-each-ref --merged HEAD --no-merged <upstream> refs/heads/`, the
  current branch left out (*verified*) — gets an `update-ref refs/heads/<b>` line right after the
  line of the commit it points at (git's own todo puts it there, and a todo without the line leaves
  the branch where it was; *verified, git 2.56.1*). Each of them is backed up (§8.2). A branch at a
  commit that a squash group folds goes after the group's last line, as git's own `--autosquash
  --update-refs` todo puts it (*verified, git 2.56.1*). A branch whose commit is dropped moves to the
  nearest kept commit below it, or to the base, as git does *(estimate)*; with none (`--root`) the run
  is refused, naming the branch.
  **Branches checked out in another worktree** (`worktree list --porcelain`, its `branch` lines) are
  left out, and the plan view says so (`mid is checked out in another worktree; it stays where it
  is`): git's own todo comments them out, and an `update-ref` line for one did not move it
  (*verified, git 2.56.1*).
- **The run** *(verified, git 2.56.1, through `sh -c` as a remote shell runs it)*:

  ```
  GIT_SEQUENCE_EDITOR='f(){ cat >"$1";};f' git -c core.commentChar=# rebase -i --reschedule-failed-exec [--autostash] [--update-refs|--no-update-refs] <base>|--root
  ```

  with the todo on stdin (the variable is one of the `env` words, §3.2). git hands the todo file to the sequence editor, which replaces it with
  stdin; nothing is written by sopen anywhere. `-c core.commentChar=#`: git reads the todo with the
  repository's comment character, so `core.commentChar = p` made every `pick` line a comment — a todo
  of picks and a message line dropped the picks, amended the base with the message and reported
  success (*verified, git 2.56.1*); the `-c` also wins over `core.commentString` (*verified*). The
  `-c` covers only this command: a Continue or a Skip reads the todo again under the repository's
  own setting. So sopen **fails closed**: when the effective `core.commentChar` or
  `core.commentString` (`config --get`) is set and is not `#`, a history-edit run is refused before
  it starts, and the banner's Continue and Skip are refused for a rebase (§8.5's row 1; Abort still
  runs), each with "core.commentChar is '`<c>`': start or finish this rebase in a terminal" (`auto`
  counts as set). The HEAD forms of `reword` and `author` write no todo and are not refused.
  `--no-update-refs` (git ≥ 2.38) when other branches are not moved along, so `rebase.updateRefs` in
  the config never moves a branch the backup does not hold (the replaced todo already left it,
  *verified, git 2.56.1*; the flag says so for every git). A stop at an `edit` line exits 0; a
  conflict or a failed `exec` line exits 1 (*verified, git 2.56.1*); the banner (§8.5) tells the
  stop. The todo, oldest first, one line per row:

  | Row | Todo lines |
  |---|---|
  | pick | `pick <c> <subject>` (the subject only for git's messages; git reads the hash) |
  | drop | `drop <c> <subject>` |
  | edit | `edit <c> <subject>` |
  | fixup | `fixup <c> <subject>` |
  | reword | `pick <c> <subject>` + the **message line** |
  | squash (a group) | `pick <first> …`, `fixup <c> …` for each squashed row (a dropped row between is its `drop` line), then the **message line**: typed (the newest squash row's), else the first row's message (a reword's own) and each squash row's joined as `squash`'s (§7.8.1), a fixup's left out |
  | author / date | after the commit's line(s) — a group's last line —: the **author line**, which then carries the message in place of the message line |
  | `fold`'s commit | right after `<c>`'s line: `fixup HEAD fixup! <subject>` (§7.8.1) |
  | a branch tip (with `--update-refs`) | after the commit's line(s): `update-ref refs/heads/<b>` |

  - **Message line**: `exec printf '%b' '<m>' | git commit --amend --only --allow-empty --cleanup=verbatim -F -`,
    where `<m>` is the message with `\` doubled and each newline written `\n`, quoted as one shell
    word; every other byte (tab, CR, ESC) goes as it is. git runs `exec` lines with its own `/bin/sh`;
    the message comes back byte for byte (*verified, git 2.56.1*: quotes, `\`, `\0101`, `\c`, `%`,
    `$`, backquotes, `#` lines, Hangul, leading blank lines, trailing spaces and blank lines, tab, CR
    and ESC; `printf %b` checked in dash, bash as sh and bash). `--allow-empty` is needed: amending
    an originally empty commit fails without it (*verified*).
  - **Author line**: `exec printf '%b' '<m>' | GIT_COMMITTER_DATE='<cd>' git commit --amend --only
    --allow-empty --cleanup=verbatim -F - --author='<name> <<email>>' --date='<ad>'`, `<m>` as in the
    message line — the row's new message, else the commit's own — and dates as `@<unix> <±hhmm>`
    (*verified*); the `GIT_COMMITTER_DATE` assignment only when the committer date is not "now". The
    last line to amend a commit sets both its message bytes and its committer date, so one line does
    both. `--no-edit` alone cleaned the message up again (trailing spaces and blank lines went), and
    with `--cleanup=verbatim` still dropped a leading blank line (*verified, git 2.56.1*), so it is
    never used. On an `edit` row, whose message the stop may have changed, and for a commit's own
    message too long for one todo line (60 KiB: git runs an `exec` line as one `sh -c` argument,
    which Linux caps at 128 KiB), the line reads HEAD's message as it is when it runs, byte for byte:
    `exec git cat-file commit HEAD | { while IFS= read -r l && [ -n "$l" ]; do :; done; cat; } |
    GIT_COMMITTER_DATE='<cd>' git commit --amend --only --allow-empty --cleanup=verbatim -F -
    --author=… --date=…` — the header read line by line up to its blank line, the rest copied as it
    is (*verified, git 2.56.1*: a leading blank line kept, as the commit had it and as an Amend at the
    stop made it; the read checked under sh and dash). A typed message past 60 KiB is refused, and so
    is one with NUL or none at all.
  - The verbs `reword` and `squash` are never written: they call the editor. A message line carries
    the message instead, so **the whole plan lives in git's todo**: after a conflict stop, Continue —
    from sopen or from a terminal — finishes the same plan (*verified*: a message line ran after a
    conflict was resolved and the rebase continued).
- Each rewritten commit gets a new committer date (git's rebase does that) unless the author line
  says otherwise. git's own `pick` and `fixup` of a message that starts with a blank line drop that
  line (*verified, git 2.56.1*), as a plain Rebase does; the rows sopen writes (message and author
  lines) keep every byte.

## 8. Safety

### 8.1 Confirmations

- Every action with an effect beyond the view opens its dialog (cyan, `chrome::dialog`). The dialog
  names exactly what changes — refs as `name: old → new` with short hashes, commits by hash and
  subject, files by count and the first few paths — the backup refs it writes first (§8.2), and the
  exact command lines it will run, as the log will show them. It updates as options change. Enter
  runs, Esc cancels.
- No confirmation: views (details, diffs, find, filters, the log, the backups list), copying to the
  clipboard, Fetch all (it changes remote-tracking refs only; pruning is asked for in the Fetch dialog
  form), Stage / Unstage, and Check out of a branch (git refuses it when it would lose changes).
- A destructive or rewriting action's dialog starts with one plain line of what is lost without the
  backup ("`feature` has 3 commits on no other branch"; "12 files lose their changes").

### 8.2 Backups

Before any history rewrite or destructive action, sopen writes a **backup group**: refs under
`refs/sopen-backup/<stamp>/`, `<stamp>` being the UTC time of Run as `YYYYMMDDTHHMMSSZ` (`-2`,
`-3`… when taken), plus a **manifest**.

| Kind | Holds | Written before |
|---|---|---|
| `heads/<b>` | the branch's commit | rebase (and every branch `--update-refs` moves, §7.8.2), reset, amend, reword, every planner run, Move branch, a branch's forced move, Fetch into with force, Delete branch |
| `detached` | HEAD's commit when detached | the same, on a detached HEAD; Split at an edit stop (§7.8.1), the run's HEAD being detached there |
| `tags/<t>` | the tag (its tag object when annotated) | Delete tag, Move tag, a fetch that may prune tags |
| `stash` | the stash commit | Pop, Drop, New branch from stash |
| `worktree` | `stash create`'s commit of the index and the tracked changes (none when clean); its second parent keeps the index | Reset hard, Reset the changes (hard), Discard of tracked files; `fold` (its staged changes are committed by the run); a history edit run with `--autostash` over uncommitted changes (git's autostash gives back what was staged unstaged, *verified, git 2.56.1*, §7.8.2) |
| `files` | a commit, parent HEAD, of the **current bytes** of a list of paths, whatever git thinks of them: untracked files, ignored files in the way (§8.6), a conflicted file before Take ours / Take theirs / Redo | Remove untracked files, Discard of untracked files or folders, Reset hard over files in the way, Take ours, Take theirs, Redo the merge |
| `pushed/<r>/heads/<b>`, `pushed/<r>/tags/<t>` | the remote's value, taken from the push endpoint (§8.4) | Delete on the remote, overwriting Push |
| `remotes/<r>/<b>` | a remote-tracking ref | Delete a remote; a fetch with prune; Prune a remote |
| `manifest` | a commit of the empty tree whose message records, for every ref and path the action can change, its value before: an object id or `absent` (*the form verified, git 2.56.1*); a `reset`'s mode is recorded with it (`moved_by`), so its undo uses the same mode (§8.3) | every group |
| `after` | a commit of the empty tree, parent the manifest, recording the same refs' values just after the action — so a later restore tells `moved since` from "changed by the action itself" (§8.3). Not listed as an item; deleted with its group | every group, once the action's steps end |

- **One transaction.** All of an action's refs are written by one
  `update-ref --create-reflog -m 'sopen: <the action, one line>' --stdin` with a `create <ref> <v>`
  line each: all or nothing; `create` refuses a name that exists and then nothing is written
  (*verified*). When the backup step fails, the action does not run.
- **Remote values go through staging refs.** Each is fetched first into
  `refs/sopen-staging/<stamp>/…` (§8.4); then the one transaction creates the backup refs and deletes
  the staging refs together (*verified, git 2.56.1*: creates and deletes in one transaction; a failing
  `create` left the staging refs in place). After a failure sopen deletes the staging refs in a second
  transaction and names any it could not delete.
- **The `files` commit holds raw bytes.** It is made by the files snapshot snippet (§3.2) with the
  paths as arguments, under a temporary index (`git rev-parse --git-path sopen-backup-<pid>.index`,
  removed by a `trap … EXIT`), starting from `read-tree --empty` so that its tree holds exactly the
  backed-up paths (with HEAD's index, a folder where HEAD has a file fails with `appears as both a
  file and as a directory`, *verified*). For each path:
  - a **regular file**: `hash-object -w --no-filters -- <path>` — the file's bytes as they are, no
    `text`/`eol` conversion, no clean filter, no working-tree encoding — then one
    `<mode> <sha>\t<path>\0` record into `update-index -z --index-info`, mode `100755` when the file
    is executable, else `100644`;
  - a **symlink**: `update-index --add -- <path>` (git stores a link's target as it is; no filter
    applies to links);
  - anything else (a fifo, a socket, a device): not backed up, and named.

  Then `write-tree` and `commit-tree -p HEAD` (the message on stdin); the snippet prints the commit's
  hash. *Verified, git 2.56.1, through sh, dash, bash and zsh as login shells*: with `* text eol=lf`
  and a clean filter that deletes a character, a CRLF file and the filtered file were stored byte for
  byte (through `update-index --add` the CRLF became LF and the filter removed its character), and an
  executable bit and a symlink target survived. Permission bits other than the executable bit are not
  kept (git keeps no others); the confirmation says so when a backed-up file has them.
- **What goes into it** — every collision found, never a subset:
  - Remove untracked: `ls-files -z --others --exclude-standard`;
  - files in the way (§8.6): for each colliding entry, the entry itself when it is a regular file or a
    symlink, and every file and symlink under it when it is a folder (`ls-files -z --others
    --exclude-standard -- <p>/` and `ls-files -z --others --ignored --exclude-standard -- <p>/`);
  - a conflicted file before Take ours, Take theirs or Redo: that file.
- **Checked before the destructive command runs**: the backup tree must hold every path found
  (`ls-tree -r -z --name-only <backup>` compared with the list). A path missing — not readable, a fifo,
  vanished meanwhile — stops the action before it runs, naming the path. The real index and the work
  tree are never touched by the snapshot (*verified*: `status` unchanged; a conflicted file's
  hand-edited bytes kept while the real index stayed unmerged). Empty folders are not kept (git keeps
  no folders); the confirmation says so when there are any.
- **Pruning.** Every ref a pruning command could delete is backed up first, from the **effective**
  configuration: the destination side of each fetch refspec of the remote(s) (`remote.<r>.fetch`) and,
  with prune tags, `refs/tags/*`. A refspec whose destination is under `refs/tags/` makes `--prune`
  alone delete local tags (*verified, git 2.56.1*), so those tags are in the group too. A remote whose
  refspec writes into `refs/heads/` (a mirror) is refused for pruning, with the reason. When the
  command ends, the backups of refs it did not delete (still at the same value) are removed from the
  group in one transaction, so the group holds exactly what was pruned. A failed command keeps the
  whole group.
- **Where they show.** Not in the graph: `--branches --remotes --tags` does not reach them. The
  **Backups** list (Cmd/Ctrl+B) groups them by stamp, newest first: the time, the reflog message
  (`sopen: drop abc12345 from main`), and each ref with its kind, short hash and subject. In it:
  Restore (the group or one item, §8.3), Show (adds a ref to the graph's revisions with a dim
  `backup` label until the next refresh), Delete (confirmed; the one removal sopen does without a
  backup, and the dialog says so).
- sopen never deletes a backup by itself, except the pruning trim above, inside the action that wrote
  it. They are ordinary local refs: `git gc` keeps their commits; `git push --mirror` would publish
  them (the Backups list's header says so).

### 8.3 Restore and undo

A **restore** puts each item of a group back to its value in the manifest, and is itself an action:
it first writes its own group of the values it is about to replace (with `absent` for a ref or path
that does not exist now). So undo, redo and undo again each restore the previous group.

| Item | Restore *(verified, git 2.56.1, unless marked)* |
|---|---|
| a ref, not the checked-out branch | `update-ref --stdin`, compare-and-swap against the value read for the dialog: `update <ref> <backup> <now>`; `create <ref> <backup>` when it is absent now; `delete <ref> <now>` when the manifest says `absent`. A ref that moved since the dialog fails the transaction and nothing changes |
| the checked-out branch | `reset --keep <backup>` (refuses rather than lose local changes), after checking that HEAD still is the value read for the dialog. **The undo of a soft or mixed `reset`** uses that reset's own mode — `reset --soft <backup>` / `reset --mixed <backup>`: HEAD (and, for mixed, the index) go back and the working files are left as they are, which already hold the backup's content; `reset --keep` would refuse there (*verified, git 2.56.1*). A hard reset's undo is `reset --keep` plus its `files` and `worktree` items, the work tree reset to HEAD before the move (the `worktree` row) |
| a detached HEAD | `reset --keep <backup>` on the detached HEAD, as for the branch, and a soft or mixed reset's undo in its own mode (*verified, git 2.56.1*; `checkout --detach` failed after a mixed reset, the reset's files being untracked) — a Split's `reset --mixed` (§7.8.1) included, so its undo is `reset --mixed` back and leaves the files |
| `tags/<t>` | as a ref: an annotated tag comes back as its tag object, with its message (`update` with the old value, *verified*; `create` refuses an existing tag, *verified*) |
| `stash` | `stash store -m '<its subject>' <backup>`. **The stash list contract**: the entry comes back at the top (`stash@{0}`) with its message; its old position and its old reflog date are not restored. The inverse — restoring a group whose manifest says the entry was `absent` — is `stash drop stash@{k}`, `k` being the entry whose hash is the backup's (`stash list --format=%H`), after its own `stash` backup. `refs/stash` is never set with `update-ref` (that would not rebuild the list or its reflog). Undo, redo and undo again leave the same entries (*verified, git 2.56.1*) |
| `worktree` | First the current tracked state is backed up (`stash create`; nothing to back up = the manifest says `clean`). Then `reset --hard HEAD` (with §8.6's check) and, unless the group says `clean`, `stash apply --index <backup>`: staged and unstaged parts come back as they were. So clean → dirty → clean → dirty round-trips exactly (*verified, git 2.56.1*: Discard, Undo, Redo, Undo; the trees equal). **The order of a restore** (after its own group, whose one transaction already checks every ref and HEAD against the values the dialog read): what can refuse first — HEAD's checks (each HEAD item's `verify HEAD <now>`), then the ref transaction of the other refs (compare-and-swap) — so a refusal there leaves the work tree as it is; then `reset --hard HEAD` (the HEAD the dialog read, which §8.6's check looked at), HEAD's moves (`reset --keep <backup>`, or a soft or mixed reset's own mode), the apply, and the `files` row's writes last — so the move never meets a local change, and the files are written where the move has made room. Before anything runs, an untracked or ignored file where a `reset --keep` move writes refuses the restore, naming it (§8.6). *Measured, git 2.56.1*: a branch moved after that backup fails the ref transaction, before the work tree's reset, and the work tree, the index and HEAD stay as they were; a restore with two HEAD items (a detached HEAD's and the branch checked out since) never gets past its own group — git refuses to verify `HEAD` and the branch it points to in one transaction (`multiple updates for 'refs/heads/<b>' (including one via symref 'HEAD') are not allowed`) — so nothing runs. With the move first, the redo of a hard reset's undo was refused (`Entry 'a.txt' would be overwritten by merge`); in this order a hard reset over staged and unstaged changes round-trips — undo, redo, undo, nothing refused — with HEAD, the index's tree, every file's bytes and the status equal each time, on a branch and on a detached HEAD (*measured, git 2.56.1*, G-S1). The redo of a hard reset over a file in the way, once undo has put that file back, is refused that way before anything runs (*measured, git 2.56.1*). When a step still fails after the work tree's reset and before the apply — the move, meeting a file put there after the plan — the run stops there with the work tree reset; the changes are in the restore's own group, the result says `<the step> was refused; your uncommitted changes were set aside in backup <stamp> — restore it to bring them back` after git's own text, and restoring that group puts them back. Over a clean work tree (the manifest says `clean`) nothing was set aside and the result says only git's text (*measured, git 2.56.1*, both). A failure at the apply or later does not get the sentence. **Exact only while the changes are applied onto the backup's base** (`<backup>^1`): the commit the chosen HEAD item goes back to, else the current HEAD. When that commit is not the base (HEAD moved since the backup and is not restored), the dialog says the changes are applied onto the current HEAD, may conflict, and that redo is then not exact; a hard reset's undo, whose move puts the base back first, does not say it (*measured, git 2.56.1*). When the index part cannot be applied, the dialog offers `stash apply` without `--index` and says what it loses. Untracked files are never part of this item (they are `files`). **After a `fold` or an `--autostash` history edit** (§8.2) Undo resets the work tree, puts the branch back, then applies this item — chosen by default, and listed in the restore's confirmation as `the tracked changes as they were (staged and unstaged)` — so the staged and unstaged changes come back as they were (*verified, git 2.56.1*: a fold into HEAD, a fold stopped at a conflict and aborted, a reword with staged and unstaged changes stashed for the run). Undo therefore also resets the work tree: tracked edits made after the run go into the restore's own group first, and redo brings them back; nothing is lost |
| `files` | **Raw, never over anything, never through a link**: the files restore snippet (§3.2) gets `<mode> <sha> <path>` for each entry of `ls-tree -r -z <backup>`. For each entry: (1) **every parent component** of `./<path>`, from the top level down, must be a real folder — `[ -d c ] && [ ! -L c ]`; a missing one is made with `mkdir` (one level, never `-p`) and checked the same way; a component that is a symlink, a file or anything else refuses the entry (`not a folder: <component>`); (2) **the leaf must not exist at all** — `[ -e p ] || [ -L p ]` refuses it, so a file, a folder, a symlink and a dangling symlink all count as occupied; (3) write: `git cat-file blob <sha> > ./<path>` under `set -C` (no smudge filter, no `eol` conversion, no working-tree encoding), the executable bit for `100755`; for `120000`, `ln -s <the blob's text> ./<path>` — only after (2), because `ln -s` onto an existing folder makes the link inside it instead of refusing (BSD's `-n` does not stop that and GNU's `-T` does not exist on BSD, *verified*); (4) the parent components are checked again and a change is reported (`changed while restoring: <path>`). **The check and the write are not atomic**: a process of the same account that swaps a parent for a symlink between (1) and (3) is not prevented, only reported by (4) (the same trust as 01 §6a's private folder). *Verified, git 2.56.1*: with the backed-up folder `dir` replaced by a symlink to a folder outside the work tree, a folder at a link's path and a dangling symlink at a file's path, the unchecked recipe wrote `dir/sub/n.dat` outside the work tree and put a link inside the folder, while the checked one refused all three and wrote nothing; `>` under `set -C` through a dangling symlink was refused in sh, dash, bash and zsh. CRLF bytes, a filtered file and a nested file came back byte for byte, `100755` as 755, a symlink with its target; a second restore over the existing files was refused and left them alone (`restore --worktree` would have run the smudge filter: it did). A link target ending in a newline loses it (the shell strips it) — named when it happens. An occupied leaf or a non-folder parent is listed and left unless the dialog's "replace" is chosen, which writes its own `files` backup of what is there first, removes it (a symlink as a link, never its target) and then runs (1)–(4); a path the manifest says was `absent` is removed (`clean -f -- <path>` for an untracked file, after the same parent check). **After HEAD's move**: the files are written last — after the `worktree` row's reset, HEAD's move and the apply — so a leaf held now by HEAD's own tracked file, as HEAD has it, where the chosen HEAD item's commit has no such path, is free by then and is chosen like a free one, never "replace" (the undo of a Reset hard over an untracked file in the way: the file comes back byte for byte, *measured, git 2.56.1*); anything else there still waits for "replace", and (2) still refuses at Run whatever is there then |
| `pushed/…` | `push --progress --force-with-lease=<ref>:<the remote's value now, or empty when gone> <r> <backup>:<ref>` (an empty lease means "must not exist", *verified*) |

- **Later changes.** A restore after other changes compares against the values now, not the values
  at backup time: refs that moved since are listed in the dialog as `moved since (now …)`; restoring
  them is a second, explicit choice, compare-and-swapped against what the dialog showed.
- **Cmd/Ctrl+Z** in the git panel restores the newest group written in this window, with the same
  dialog. Older groups are restored from the Backups list.
- After every action that wrote a backup, the status row says
  `<what was done> · backup <stamp> · Cmd/Ctrl+Z undoes`.

### 8.4 Overwriting or deleting on a remote

- Never a plain `--force`. Each overwrite or delete:
  1. **Endpoints**: `remote get-url --push --all <r>` — the URLs git will push to, with `insteadOf`
     and `pushInsteadOf` already applied (*verified, git 2.56.1*: `pushurl`, two push URLs, and
     `pushInsteadOf` on a remote without `pushurl`).
  2. **Values**: for each endpoint, `fetch --no-tags --no-prune --no-prune-tags --no-write-fetch-head <url>
     +<ref>:refs/sopen-staging/<stamp>/<n>/<ref without refs/>` (a missing ref: `ls-remote` says so,
     and its value is "absent"). The fetch URL is never assumed to hold what the push URL holds.
     `--no-write-fetch-head` needs git 2.29; on an older git (the floor is 2.20, §2.4) this fetch
     fails, and the overwrite is refused there.
  3. When the endpoints disagree, the action is refused, naming each URL and its value.
  4. Backup: the staging refs become `pushed/<r>/…` (§8.2).
  5. `push --progress --force-with-lease=<ref>:<that value> <r> <src>:<ref>` — or `… <r> --delete
     <ref>`; git applies the same lease at each endpoint. When an endpoint moved between 2 and 5, git
     refuses with `stale info` and nothing changes there *(verified, git 2.56.1, for an overwrite and
     for a delete)*; with several endpoints, the result names which ones took the change.
- A tag that differs on the remote is replaced the same way (git refuses a plain push of a moved tag,
  *verified*).

### 8.5 Operations in progress and conflicts

- **Detection.** Once per repository: `rev-parse --git-path rebase-merge --git-path rebase-apply
  --git-path MERGE_HEAD --git-path CHERRY_PICK_HEAD --git-path REVERT_HEAD --git-path sequencer
  --git-path BISECT_LOG` (per worktree, *verified*). On every refresh, one probe — a fixed POSIX sh
  snippet run like §3.2's wrapper (locally by `sh`) — reports which exist, whether
  `rebase-apply/applying` or `rebase-apply/rebasing` exists, the first line of `sequencer/todo`, and
  prints `rebase-merge/msgnum`, `end`, `head-name`, `onto`, `stopped-sha` and the last line of
  `rebase-merge/done`. Conflicted files are the status read's unmerged entries.
- **Which state** (first match wins; *each marker verified, git 2.56.1*):

| # | Marker | State | Continue | Skip | Abort |
|---|---|---|---|---|---|
| 1 | `rebase-merge/` | rebase (interactive or merge backend; planner runs) | `rebase --continue`, `GIT_EDITOR=true` | `rebase --skip` | `rebase --abort` |
| 2 | `rebase-apply/` with `applying` | am (a patch series) | `am --continue` | `am --skip` | `am --abort` |
| 3 | `rebase-apply/` with `rebasing` | rebase (apply backend) | `rebase --continue`, `GIT_EDITOR=true` | `rebase --skip` | `rebase --abort` |
| 4 | `MERGE_HEAD` | merge | `merge --continue`, `GIT_EDITOR=true` | — | `merge --abort` |
| 5 | `CHERRY_PICK_HEAD` | cherry-pick | `cherry-pick --continue`, `GIT_EDITOR=true` | `cherry-pick --skip` | `cherry-pick --abort` |
| 6 | `REVERT_HEAD` | revert | `revert --continue`, `GIT_EDITOR=true` | `revert --skip` | `revert --abort` |
| 7 | `sequencer/` alone | a cherry-pick or revert sequence between steps (after a step was committed by hand): the first word of `sequencer/todo` — `pick` or `revert` — says which | as 5 or 6 | as 5 or 6 | as 5 or 6 |
| — | `BISECT_LOG` | bisect, shown on its own line as well as any of the above | — | — | `bisect reset` (the panel offers no other bisect step) |

  Every Continue, Skip and Abort form above *(verified, git 2.56.1; `rebase --skip` by the inventory's
  run)*: each conflict leaves its marker; each abort clears it and puts HEAD back; each Continue,
  resolved, finishes with git's prepared message under `GIT_EDITOR=true` (rebase and merge fail under
  `false`); a sequence left with only `sequencer/` continued with `cherry-pick --continue`; Continue
  with conflicts left is refused by git (`unmerged files`), and sopen shows that refusal as it comes.
- **The banner**: the git panel's top row, in the warning colour, while a state exists — whether
  sopen or a terminal started it: `REBASE 3/7 · stopped at abc12345 <subject> · 2 conflicts`,
  `AM · 1 conflict`, `MERGE of feature · 1 conflict`, `CHERRY-PICK abc12345`, `REVERT abc12345`,
  `BISECT · Reset`, then its keys.
- **During a conflict** the `worktree` backup cannot be written — `stash create` refuses unmerged
  entries *(verified, git 2.56.1)* — so Reset hard, Reset the changes (hard) and Discard of tracked
  files are refused then, naming Abort.
- **Conflicted files** are the uncommitted row's `Conflicts` group (§6.1). Take ours, Take theirs and
  Redo the merge replace the file's bytes, so each first writes a `files` backup of the file as it is
  (the hand-made resolution survives; Restore brings it back, *verified*). Mark resolved (`add`); F4
  opens the file in the editor to merge it by hand; Enter shows its diff.
- **An edit stop** (`edit`, `split`): `stopped to edit abc12345`, with Amend, Split, Continue, Abort.
  git writes the stopped commit in full in `rebase-merge/stopped-sha` and in `done`'s last line
  (*verified, git 2.56.1*, S4-C), so Split names it by its whole id (HEAD's, at the stop, otherwise).
- **A planner line that fails** (a commit hook refused a message line): git stops with its message,
  which the banner shows; planner runs pass `--reschedule-failed-exec`, so Continue runs that line
  again once the cause is fixed *(verified, git 2.56.1: a `commit-msg` hook that refused, removed,
  then Continue applied the message)*; Abort puts everything back.
- **Nothing is added** to git's state: no sopen file stays in the repository (only backup refs,
  §8.2), no lock is removed. Closing the window mid-operation leaves it as git keeps it, and the next
  panel shows the banner. The backup of the run's start stays, so the whole run can be undone after
  Continue as after Abort.

### 8.6 Files in the way

git writes over some files it does not track (*verified, git 2.56.1*, for every row: an ignored
file, an untracked file and an untracked file where the target has a folder; for revert and
`stash pop`, an ignored file where the reverted or stashed change brings the same path back):

| Command | What git does with a file in the way | sopen |
|---|---|---|
| `checkout` (a branch, a commit, `-b`) | refuses for an untracked file; **overwrites an ignored file** | passes `--no-overwrite-ignore`: git then refuses for both |
| `merge` (and a pull's merge step) | the same | passes `--no-overwrite-ignore` |
| `reset --keep` | refuses | a restore's HEAD move (§8.3) runs after the work tree's reset, so it refuses first, before anything runs: every untracked or ignored entry that collides with a path of the backup's commit that HEAD and the index lack, named (*measured, git 2.56.1*); elsewhere — |
| `reset --hard` (Reset hard, Reset the changes hard) | **deletes or overwrites** untracked and ignored files and folders where the target has a path — an untracked folder where the target has a file included. Where the target **removes** a tracked path that the work tree has turned into a folder, git 2.56.1 warned `unable to unlink` and left the folder and its files (*verified, macOS*; another platform or git may differ *(estimate)*) | backs up every file under each colliding path first, recursively, untracked and ignored, in a `files` backup named in the confirmation; git's warnings are shown in the result |
| `cherry-pick`, `revert`, `rebase` (plain, pull's, planner), `stash apply` / `pop` | **overwrites an ignored file** (no flag to stop it) | refuses before running when an ignored file sits at a path the operation writes, listing them: move them away, or use Reset hard, which backs them up |

- **Finding them**: the paths the operation can write or remove — for a reset, the target's paths
  (`ls-tree -r --name-only -z <target>`) **and** the current index's (`ls-files -z`), since a reset
  removes the index entries the target lacks; for the others, the union of `diff --name-only -z <from>
  <to>` of each step — against `ls-files -z --others --directory`, which lists untracked and ignored
  entries, a file as itself and a folder with nothing tracked in it as one `dir/` entry (*verified*:
  an ignored file in the way is listed as itself). An entry collides when it equals a footprint path,
  lies inside one, or contains one (a file where a folder goes, or the other way round). Each
  colliding entry is backed up as §8.2 says: **a file or symlink itself**, a folder recursively.

## 9. Keys

04 §3 owns the key rules; this section is the git panel's map under them: modifier variants are
matched before plain keys; Cmd/Ctrl+1…0 act as F1…F10; the F-key row is clickable; no plain F-key
and no Cmd/Ctrl letter acts while a text field has the typing.

### 9.1 The F-key row while the git panel has the keyboard

PC Shell changes its F-key row with the context (keys.md §3.3); the git panel has its own row. Each
slot keeps the meaning it has in the file list where git has one, so a mis-press costs a dialog.

| Slot | Label | A commit row | A label (the label cursor on it) | The uncommitted row | A stash row |
|---|---|---|---|---|---|
| 1 | Help | the git panel's keys | ← | ← | ← |
| 2 | Reword | `reword` | `rename` (a local branch) | — | — |
| 3 | View | details (§6.1); with one marked row, compare (§6.4) | `tag-details` | details | details |
| 4 | Edit | the top level in the editor | ← | the file under the bar | — |
| 5 | Pick | `pick` (the bar's or the marked commits) | `pick` of its commit | — | `stash-apply` |
| 6 | Rebase | `rebase` onto it | `rebase` onto the branch | — | — |
| 7 | Branch | `branch` here | — | `commit` (label: Commit) | `stash-branch` |
| 8 | Delete | `drop` | `delete-branch`, `delete-remote-branch` or `delete-tag` | `reset-changes`; on a file: `discard` | `stash-drop` |
| 9 | Dual / Prevw | the browser's mode (02 §2) | ← | ← | ← |
| 10 | Menu | the menu of the object under the bar | ← | ← | ← |

- Slot 10 is the file list's Tree (04, 05); the tree belongs to a directory list, so in the git panel
  slot 10 is the menu, PC Shell's own F10. Alt+F10 does nothing here.

### 9.2 Other keys in the git panel

| Key | Effect |
|---|---|
| ↑ ↓ PgUp PgDn Home End | the bar (the details pane's file list when it has the keyboard) |
| ← → | the label cursor across the row's labels (the commit itself first) |
| Enter | as F3 |
| Shift+F3 | a file at this revision (§6.3) |
| Space, Ins | mark / unmark the row and step down (on the uncommitted row's files: stage / unstage) |
| Tab, Shift+Tab | back to the list (02 §3.2) |
| Esc | the innermost first: the menu or dialog, the find field, the planner (asks when changed), the details pane's keyboard, the details pane, then the pin (02 §3.2) |
| right click, Shift+F10 | the menu of the object clicked / under the bar |
| double click | a row: details; a local branch label: check it out |
| a typed letter | starts Find with it (§4.7) |
| Cmd/Ctrl+F · Cmd/Ctrl+Shift+F | Find · History search |
| Cmd/Ctrl+R · Cmd/Ctrl+Shift+R | hard refresh (§5) · Fetch all (§7.7) |
| Cmd/Ctrl+K | the filters (§4.6) — from slice S2 on, before the F10 menu (slice S3) offers them too |
| Cmd/Ctrl+Z | undo the newest backup group (§8.3) |
| Cmd/Ctrl+B · Cmd/Ctrl+L | Backups list · command log |
| Cmd/Ctrl+Shift+C · Cmd/Ctrl+Shift+S · Cmd/Ctrl+Shift+A | while the banner shows an operation (§8.5): its Continue · Skip · Abort, each through its dialog (§8.1); a step the state does not offer (Skip in a merge) does nothing, and the status row says so |
| Cmd/Ctrl+Shift+B | while bisect is in progress: its Reset (`bisect reset`), through its dialog |
| Cmd/Ctrl+Shift+M · Cmd/Ctrl+Shift+P | at an edit stop (§8.5): its Amend (§7.6's `amend`) · Split (§7.8.1's `split`), each through its dialog (§8.1) and a button as well; while the banner shows any other state they do nothing, and the status row says so |
| Cmd/Ctrl+E | the planner from the bar's commit (§7.8.2) |
| Cmd/Ctrl+J | the bar to HEAD's row |
| Cmd/Ctrl+G · Cmd/Ctrl+Shift+G | pin / unpin (02 §3.2) · the git panel over both sides and back |

- **In the Backups list** (Cmd/Ctrl+B) the rows above act on the list, with no key of its own:
  ↑ ↓ PgUp PgDn Home End move the bar over its groups and refs; Enter and F3 (View) Show the ref,
  or the group's refs, in the graph (§8.2); Cmd/Ctrl+Z restores the group or ref under the bar
  instead of the newest group (§8.3); F8 (Delete) deletes it; Esc closes the list. Restore and
  Delete ask first.
- The banner's Continue, Skip, Abort and Reset, and the Backups list's Restore, Show and Delete, are
  buttons as well: a click does what the key does.
- Checked against keys.md: none of these Cmd/Ctrl letters is bound in the list or a viewer's own
  pass while the git panel has the keyboard (§3.2 "what exists" there); no Ctrl+F-key and no
  Alt+F1–F4 is used (KDE takes them, keys.md §1); ⌘H (macOS hides the app) and ⇧⌘3/4/5 (screenshots)
  are avoided. Cmd/Ctrl+R has the list's meaning (re-read), Cmd/Ctrl+Z the viewers' (undo). egui
  turns Cmd/Ctrl+C, X and V into clipboard events only without Shift (egui-winit 0.37), so
  Cmd/Ctrl+Shift+C reaches the panel as a key.
- The planner's keys are in §7.8.2; inside a dialog, Enter runs and Esc cancels.

## 10. Out of scope, left out, open

### 10.1 Out of scope (ssot "Not in scope", until an owner message asks)

- Creating pull requests in a web forge, and its settings.
- Avatars fetched from the network, and their cache.
- Issue-link settings.
- More than the current folder's repository: no repository list, no add / remove / rescan of
  repositories, no jump into a nested repository from a commit's files (entering its folder does it).

### 10.2 Left out by this spec, with the reason

| Left out | Why |
|---|---|
| Code reviews kept across sessions (start, end, resume) | they need stored state per workspace and sopen saves nothing for the panel; the window-only `√` viewed mark (§6.1) keeps the useful part |
| External directory diff (`difftool --dir-diff`) | a GUI diff tool runs where git runs — on a remote, nowhere useful; sopen's text viewer shows every diff |
| Open a terminal in the repository | sopen has no terminal; F4 opens the editor, whose terminal is there |
| File tree view of a commit's files (the inventory's tree / list switch) | the list with full paths fits a text pane; a tree is UI for little gain |
| Every Git Graph setting (columns, date format and type, label alignment, graph style and colours, muting, tab icon, status-bar item, editor group, extension settings, menu-item visibility, shortcut settings, branch globs, emoji, Markdown, file encoding, initial branches, repository name, exported repository settings) | they configure VS Code integration or Git Graph's look; sopen's panel has one look (AGENTS.md) and window-only state |
| Signature status of commits | needs the signer on the host where git runs, and is slow; git's config still decides signing (§3.3) |
| Parts of a file when splitting or staging (hunks) | needs a patch editor; splitting and staging go file by file |
| Plans over merge commits (`--rebase-merges`) | refused with the reason (§7.8.2); a base above the newest merge works |
| Todo verbs `exec` (the owner's own commands), `break`, `label`, `reset`, `merge` in the planner | not asked for; `exec` is used only for sopen's message and author lines, `update-ref` only as `--update-refs` needs it (§7.8.2) |
| A merge message typed in the merge dialog | git's message is used (`--no-edit`); Reword changes it after |
| Push of a branch under another name, `--tags`, `--follow-tags`, `--force-if-includes` | push sends a branch or a tag under its own name; the explicit lease (§8.4) covers what `--force-if-includes` guards |
| Signing existing commits; whole-history rewrites (`filter-branch`, `filter-repo`) | not asked for; `filter-repo` cannot be assumed on a remote |
| A list of the reflog | the Backups list holds every state sopen changed; git's reflog stays a terminal tool |
| Opening a URL found in a message | the text UI shows it as text, copyable |
| Regular expressions in Find | Find matches plain text in the loaded rows; History search (§4.7) takes git's own regular expressions over the whole history |
| Date formats (relative, date only, a choice of author or commit date in the list) | one fixed local format, author date in the list; the details pane shows both dates |
| Version information command | the host's git version is in the log (§3.7); a frame in cells has no hover (10 §6) |

### 10.3 Open for the owner

- **Bare repositories and the inside of `.git`.** The ssot says "inside a git work tree", so they
  show the directory list (§2.3). Showing their history (read-only graph, branch and tag actions)
  would need an owner message.

## 11. Acceptance

Tests need git: they skip with a note when it is missing and fail under `SOPEN_REQUIRE_GIT=1`
(CI). Remote paths are tested through `ShellGit` (02 §9); live receipts on a remote use repositories
under the remote `/tmp/sopen-receipts/`. A window receipt is a screenshot someone looked at
(AGENTS.md "Receipts").

**Detection and the runner**

- G-D1. In a local work tree's subfolder run `SOPEN_LIST=git sopen .` → the first page prints, the
  frame title line is the top level. Repeat in a bare repository, inside `.git`, in a plain folder,
  with `SOPEN_GIT=/nonexistent` → each prints the directory listing (as `SOPEN_LIST=1`) and one line
  saying why (bare / not a work tree / not a repository / no git).
- G-D2. In a submodule, a linked worktree and a shallow clone → `SOPEN_LIST=git` prints the
  submodule's own graph with `(submodule of …)`, the worktree's with `(worktree of …)`, and the shallow
  clone's ending in `history ends here (shallow clone)`.
- G-R1. Create branches and tags named `it's`, `a$b`, `ü-한` and `@`, and files named `a b'c`,
  `x\y`, `a*`, `[x].txt` and `:(top)z`, in a fixture; through `ShellGit` and `LocalGit` list refs,
  check out each branch, stage and diff each file → every name comes back byte for byte, each file
  stages only itself, and `SshGit`'s argv for the same commands equals the expected words. The New
  branch dialog refuses `x\y`, `a b`, `a..b`, `-x` and `x.lock` as git's `check-ref-format --branch`
  does (*verified, git 2.56.1*), before running anything. A branch named `@` cannot be checked out
  by its plain name: git reads `checkout @` as HEAD and `switch refs/heads/@` says a branch is
  expected (*verified, git 2.56.1*); until S3's Check out spells it another way, the runner's test
  checks `@` by name (`show-ref --verify refs/heads/@`).
- G-R2. Fetch from `http://localhost:<port>/x.git` served by the test with `401` and a Basic
  challenge → the fetch fails within 5 s with git's "terminal prompts disabled" message in the log
  entry; no window or terminal prompt appears.
- G-R3. Start a fetch whose upload-pack sleeps 30 s (`--upload-pack='sleep 30; git-upload-pack'`
  on a local fixture), press Esc → within 6 s the log says `stopped`, no process of its group is left,
  no `*.lock` exists in the repository, and the refresh reports what changed. Live receipt (S3-C's,
  on sopen's own binary against a real ssh host): the same on a remote fixture under zsh, bash and
  fish login shells → after Esc,
  `ssh host pgrep -f 'sleep 30'` prints nothing; with the master killed before Esc → the result says
  `may still be running`. *Measured, S3-C, 2026-11-23, zsh only* — sopen's own browser and runner
  (`git_connect`, `SshGit` over a real master; the window the test kit's, the shipped binary's
  `SOPEN_KEYS` cannot press Esc mid-run) against a macOS host whose login shell is zsh (git 2.54):
  Esc ended the fetch in 0.19 s, the log said `stopped`, `pgrep -f 'slee[p] 30.4711'` found
  nothing (twice: 0.19 s, 0.20 s); with the master gone (`ssh -O exit`) before Esc, one run said
  `stop sent; the command may still be running on <host>` and the other `outcome unknown: the
  connection to <host> was lost after the command was sent` — the dead master ended the command's
  own ssh before Esc reached it (a race between the two, not a wording sopen picks) — and both
  times the upload-pack was indeed still running (the receipt ended it): never `stopped`.
  *Measured, 2026-11-24, fish* — the same test against a Linux host whose login shell is fish
  (git 2.55), three runs: Esc ended the fetch in 0.36–0.38 s, the log said `stopped`, `pgrep`
  found nothing; with the master gone before Esc, two runs said `outcome unknown: the connection
  to <host> was lost after the command was sent` and one `stop sent; the command may still be
  running on <host>`, the status row `Fetch origin: failed`, never `stopped`. Not run: bash (no
  account with a bash login shell). The local
  half is `gui::browser::tests::s3::g_a3_g_r3_esc_stops_a_pull_during_its_fetch_and_leaves_head`;
  the live one `live_g_r3_esc_stops_a_remote_fetch` (AGENTS.md).
- G-R4. A test transport (`ShellGit` with a fault switch) that runs the command and then loses its
  stderr and exit status, returning 255 → for a commit: the result is `outcome unknown`, the command ran
  exactly once (a counter in the fixture's `post-commit` hook), nothing is re-run, and the refresh shows
  the commit; for a read: it is run again. With the master missing before dispatch → the pre-flight
  check fails, `Connect` revives it, the command runs once. Live receipt: a read with the master killed
  meanwhile (`ssh -O exit`) succeeds after the revival.
- G-R5. A `pre-commit` hook, a signing program (`gpg.program`) and a credential helper that each wait
  forever (stand-ins) → after 10 s the status row says what git may be waiting for; Esc (and Yes for
  the write) stops each within 6 s with no process of the group left and no `*.lock`; the commit was
  not made and its staged change is still staged.
- G-R6. A URL `https://user:secret@localhost:<port>/x.git` as a remote → the log, `SOPEN_LOG`, the
  status row and an error dialog show `user:***@`.

**The graph**

- G-G1. On a fixture with two branches and a merge, an octopus merge, an annotated and a lightweight
  tag, a remote with `main` at the local `main` and `feature` behind it, a stash and uncommitted
  changes → `SOPEN_LIST=git` prints the golden text (lanes, `►main↔origin`, `origin/feature`, both
  tags, `stash@{0}`, `Uncommitted changes (2)` first). The window receipt shows the same rows in the PC
  Shell look.
- G-G2. Lane layout: the table tests and the 1 000-graph properties of §4.3 pass.
- G-P1. Build a 100 000-commit fixture with `git fast-import` (the generator of §4.5's measurement:
  a two-commit side branch merged every 50 commits) and open the git panel on it → the first page (500
  rows) is on screen within **5 s on CI** (the guard against walking the history twice or reading
  beyond a page) and within **1 s** on the owner's machine (receipt; the measured git share is 0.3 s
  without a commit-graph, 9 ms with one); exactly 500 rows are parsed and the log process is still
  running, blocked. The wide fixture of §4.3 (60 branches, 97 761 commits) → its first page within the
  same bounds (measured git share 0.6 s), and the layout's stored segments stay about 2 per row (no
  per-lane storage).
- G-P4. A ref moved by another process while the first page loads → the rows are the history of the
  object ids the labels show (no row of the moved ref's new commits until the next refresh), and a
  result of the superseded load is dropped (test: a load generation bumped mid-stream).
- G-P2. Move the bar to the end three times → 2 000 rows, no hash twice, rows 1–500 unchanged
  (their lanes too). Load everything → 100 000 distinct hashes, `all loaded`.
- G-P3. Live receipt on a remote: open the panel on a 100 000-commit fixture under
  `/tmp/sopen-receipts/`, leave the first page parked, then enter folders in the list and open a
  file over the same master → listings and the fetch work while the stream waits; "more" then reads the
  next 500 rows of the same stream.
- G-F1. Each filter of §4.6 on G-G1's fixture changes the printed rows as its row says; the frame
  title says `[filtered]`.
- G-F2. Type `merge` in the graph → Find opens with `m…`, the bar jumps to the first match, Enter
  goes to the next, the foot says `k of n`. History search for a message only in a commit beyond
  the first page → it is found.

**Details, diffs, revisions**

- G-T1. Enter on a commit → summary and files with `A`/`M`/`D`/`R` and counts; Enter on a file → the
  diff in the text viewer, read-only, titled `… @ <hash>`; Esc → back with the bar on that file.
- G-T2. Shift+F3 on an image changed in a commit → the image window shows that version with Save and
  Save copy off and their reason on hover.
- G-T3. Mark a commit, F3 on another → the comparison's files; F3 with the uncommitted row as one end
  → the work tree's changes and untracked files.
- G-T4. A stash holding an untracked `u.txt`, while a working `u.txt` with other bytes exists → Enter
  on the stash's `u.txt` shows the stash's bytes (`<s>^3`), not the working file.
- G-T5. "Diff against the working file" for a file renamed since the revision → a pure rename shows
  `rename from/to` and no hunk; a rename with a changed line shows the rename and that hunk; neither
  shows the file as added.

**Actions** (each run locally and through `ShellGit`; each test asserts the refs and files after, the
confirmation's command lines, and the backup refs)

- G-A1. Commit row: `tag` (annotated, with push to a local bare remote), `branch` (and its "move"
  form), `move-branch`, `checkout`, `pick` of two marked commits, `revert`, `merge` with each
  fast-forward choice, `rebase`, `reset` in all four modes, `archive` into a chosen folder (a taken
  name is refused, 01 §6a).
- G-A2. Local branch: `checkout-branch`, `rename`, `delete-branch` merged / not merged (the second
  dialog) / with its remote, `push` with set-upstream, and with overwrite after a rewrite.
- G-A3. Remote branch: `checkout-remote` (new name and the existing-name pull), `delete-remote-branch`,
  `fetch-into` (with force), `pull` with merge, rebase and fast-forward only; stopping a pull during
  its fetch leaves HEAD where it was.
- G-A4. Tag: `tag-details`, `delete-tag` with the remote, `push-tag`, `move-tag` keeping the message.
- G-A5. Stash: `stash`, `stash-apply` with and without index, `stash-pop`, `stash-branch`,
  `stash-drop`; a stash list changed under the dialog → no action, a refresh.
- G-A8. Files in the way (§8.6): an ignored file, an untracked file and an untracked file where the
  target has a folder, against `checkout`, `merge`, `reset --hard`, `cherry-pick`, `revert`, `rebase`
  and `stash pop` → checkout and merge are refused by git, Reset hard backs them up, the others are
  refused by sopen listing the paths; at no point is such a file changed without a backup. For Reset
  hard the test asserts **the backup's blobs**, not only its ref: each colliding file — the ignored
  file at the exact path the target writes included — is a blob in the backup tree equal to the
  original bytes, with its mode, before the reset runs; and Restore writes the same bytes back.
  Byte-for-byte fixtures: a CRLF file under `* text eol=lf`, a file whose clean filter deletes a
  character and whose smudge filter changes one, an executable and a symlink — each restored exactly;
  a fifo in the way → the action refused before it runs, naming it. Restore's paths: the backed-up
  folder replaced by a symlink to a folder outside the work tree → nothing is written outside, the
  entry is refused naming the symlinked component; a real folder where a symlink is to be restored,
  and a dangling symlink where a file is to be restored → each refused as occupied, nothing created
  inside the folder or through the link; with "replace" → backed up, removed, restored. The deletion-only case: HEAD tracks `p`, the work tree has replaced
  it with a folder `p/` holding an untracked file, a nested one and an ignored one, and the target
  commit deletes `p` → Reset hard backs up all three first; afterwards they are present (left by git)
  or restorable byte for byte.
- G-A9. Pruning: with `fetch.prune` and `fetch.pruneTags` set in the config, a fetch with the dialog's
  prune options off → nothing pruned; the same for Add remote with fetch now, a pull and Fetch into
  (every fetch's log line shows `--no-prune --no-prune-tags`); on → the group holds exactly the pruned refs (a remote branch and
  a local-only tag), and Restore brings them back; a mirror refspec → prune refused.
- G-A6. Uncommitted: stage / unstage by Space, `commit`, `amend`, `discard` of a tracked and an
  untracked file, `reset-changes` hard, `clean` with folders, ours / theirs / resolved in a conflict.
  `clean` and the untracked `discard` of a CRLF file under `* text eol=lf`, a filtered file, an
  executable and a symlink → the backup's blobs equal the original bytes, and Restore writes them back
  exactly.
- G-A7. Repository: `fetch` all, `fetch-remote` with prune, `prune-remote`, `add-remote`,
  `edit-remote`, `delete-remote`, `user` local and global (against a temporary `HOME`), `user-remove`.

**History edits**

- G-H1. On a five-commit branch with one originally empty commit, in the planner: reword the 2nd (a
  message with quotes, `\n` as text, `%`, `$`, backquotes, `#` lines, Hangul, a leading blank line,
  trailing spaces and blank lines, a tab, CR and ESC), reword the empty one, move the 4th below the 3rd, drop the 5th, squash the 3rd
  into the 2nd with a new message, change the 1st's author and dates → one run; the log shows exactly
  that, each message byte for byte. Through `ShellGit` too, and again with `GIT_SEQUENCE_EDITOR=false`
  exported to sopen and `sequence.editor = false` in the repository's config: the plan still applies.
- G-H2. A plan whose reorder conflicts → the banner `REBASE …`; resolve, Mark resolved, Continue →
  the remaining message lines still apply; a second plan → Abort → HEAD back.
- G-H3. `split` a commit that modifies one file and adds another into two commits — Continue is
  refused while the added file is untracked, then succeeds with both in history; `split` on the root
  commit is refused; `edit` a commit and amend it; `fold` staged changes into an older commit; quick
  `reword` of HEAD keeps a staged file staged; reword of a commit whose message is not valid UTF-8 is
  refused with the reason.
- G-H4. A range holding a merge → the planner refuses, naming the merge; plain Rebase over the same
  range → the confirmation lists the merge as flattened.
- G-H5. `--update-refs`: a plan dropping a commit below another local branch's tip → that branch moves
  to the rewritten commit, its old value is in the backup group, and Restore puts both branches back;
  the same with plain Rebase. A branch in the range checked out in a linked worktree → left out of the
  todo, named in the plan view, still at its commit after the run.

**Safety**

- G-S1. Each destructive action of §8.2's table → `for-each-ref refs/sopen-backup` shows its kinds
  under one stamp with the reflog message and a manifest; Backups → Restore puts each ref, stash,
  work-tree change (staged and unstaged as they were) and file back; Cmd/Ctrl+Z restores the newest
  group, Cmd/Ctrl+Z again undoes that restore (redo), and again restores — each step exact (refs,
  index, files compared). Round trips: Drop a stash → undo (the entry back at `stash@{0}`) → redo (that
  entry dropped, the others unchanged) → undo (one entry, not two); Discard from a clean work tree →
  undo (staged and unstaged as before) → redo (clean again) → undo; the same Discard undone after HEAD
  moved → the dialog's warning, changes applied onto the new HEAD. Undo after later edits: a ref that moved since is listed as moved and left
  alone unless chosen; a deleted branch whose name is taken again, a moved tag (restored by
  compare-and-swap), and a restored untracked file that had been re-created with other bytes (kept,
  listed) each behave as §8.3 says.
- G-S6. Take theirs over a hand-edited conflicted file, then Restore (with "replace") → the hand-made
  bytes are back and the file is still unmerged; the same with the hand-made file in CRLF under
  `* text eol=lf` and a clean filter in `.gitattributes` → the bytes are back exactly.
- G-S7. A remote with a `pushurl` that differs from its URL, and one with two push URLs → the backup
  and the lease come from the push URL(s); two push URLs that disagree → refused, naming both.
- G-S8. A rebase stopped in each state of §8.5's table (am, rebase apply backend, a sequence left with
  only `sequencer/`, bisect) → the banner names it and its Continue / Skip / Abort (bisect: Reset) work.
- G-S2. Overwrite-push after another clone pushed meanwhile → refused with `stale info`, the remote
  unchanged, the error dialog shows git's text; delete on the remote the same.
- G-S3. Merge, cherry-pick and revert conflicts → the banner for each; Continue (with
  `GIT_EDITOR=true` in the log line), Skip and Abort each do what §8.5's table says; a rebase started in
  a terminal shows the same banner after a refresh.
- G-S4. Every dialog of §7 opened by `SOPEN_STATE='git=pin;select=…;dialog=<id>'` → a screenshot
  shows the exact command lines and backup names (window receipts for `drop`, `reset` hard, `push`
  with overwrite, `clean`, `plan`).
- G-S5. Hold `index.lock` from another process → a write fails with git's message; the lock is still
  there.

**Keys and look**

- G-K1. With the git panel active, each slot of §9.1 does its row's action on each kind of row
  (Cmd/Ctrl+digit and a click on the slot do the same); the find field takes letters and F-keys do
  nothing while it has the typing.
- G-K2. Esc walks §9.2's chain to the unpin; Tab returns to the list; F9 switches the mode from the
  git panel.
- G-W1. Window receipts, local and remote: the graph, the details pane, a diff, the planner, the
  banner, the Backups list, the command log. The history edits' local receipts (*measured, S4-C,
  2026-11-24*, sopen's own binary, `--here`): `SOPEN_STATE='git=pin;select=HEAD~1' SOPEN_KEYS='F10'`
  — a commit's menu, S3's items, then the history row (Reword… F2, Drop… F8, Edit history from
  here… Cmd+E), `Repository…`, the status row counting 21 items; `SOPEN_STATE='git=pin;select=HEAD~5;
  dialog=plan' SOPEN_KEYS='Down,Down,Down,text:s'` — the planner over five commits, one `squash`, the
  F-key row 1 Help and 9 Dual only; a repository stopped at an `edit` line with `git=pin` — the banner
  `stopped to edit <c>` with Amend, Split, Continue, Abort, and their keys on the status row.

## 12. Coverage of the inventory

Every row of the inventory's action tables, its coverage indexes (its §4.8) and its §7, by section and
count, mapped to sopen's own ids. Git Graph's internal names are deliberately not repeated (§1).

| Inventory | Here |
|---|---|
| §4.1 commit menu, 11 rows | `tag`, `branch`, `checkout`, `pick`, `revert`, `drop` (§7.8, any commit of the range), `merge`, `rebase`, `reset`, `copy-hash`, `copy-subject` |
| §4.2 local-branch menu, 10 rows | `checkout-branch`, `rename`, `delete-branch`, `merge`, `rebase`, `push`, `archive`, `only-branch` / `all-branches`, `copy-name`; pull-request creation out (§10.1) |
| §4.3 remote-branch menu, 9 rows | `checkout-remote`, `delete-remote-branch`, `fetch-into`, `merge`, `pull`, `archive`, `only-branch` / `all-branches`, `copy-name`; pull-request creation out |
| §4.4 tag menu, 5 rows | `tag-details`, `delete-tag`, `push-tag`, `archive`, `copy-name` |
| §4.5 stash menu, 6 rows | `stash-apply`, `stash-branch`, `stash-pop`, `stash-drop`, `copy-name`, `copy-hash` |
| §4.6 uncommitted menu, 4 rows | `stash`, `reset-changes`, `clean`; the hand-over to VS Code's source-control view → sopen's own `stage`, `unstage`, `commit`, `amend` and F4 (§7.6) |
| §4.7 repository, 21 rows | fetch all (bar and command), fetch one, prune, add, edit, delete a remote, a remote's visibility, name and email, remove them → §7.7; the remote list → the dialogs' prefills (`git config`); repository name, initial branches, issue links, pull-request settings, extension settings, exported settings, terminal, add / remove repositories, version information, avatar cache → out (§10.1) or left out (§10.2) |
| §4.8 index of 61 requests: 33 git actions | the rows above |
| — 8 data loads | detection (§2), the reads of §4.1 and §4.5, details (§6.1), comparison (§6.4), `tag-details`; avatars out |
| — 11 hand-offs | diffs (§6.2), a file at a revision and against the working file (§6.3), F4, the copy actions, §7.6; external diff, terminal, URL, extension settings left out (§10.2) |
| — 9 state entries | code reviews left out (the `√` mark, §6.1); saved view state left out (window-only); repository rescan → re-read (§2.2); error messages → §3.8 |
| §2 view (columns, lanes, labels, paging, filters, find, refresh, repositories) | §4, §5; regular-expression find and date formats left out (§10.2); settings and multi-repository features left out or out (§10) |
| §3 details, files, compare, copies | §6 (following a file's renames since the revision: §6.3), the copy actions; file tree view and code reviews left out (§10.2) |
| §6 keys and gestures | §9 (sopen's own keys) |
| §7 gaps, 22 rows | reword (HEAD and older) → `reword`; amend → `amend`; author and date (HEAD and older) → `author`; squash and fixup → `squash`, `fixup`; reorder → `move-up`, `move-down`, the planner; drop any → `drop`; split → `split` (by file); pause → `edit`; rebase planned in the panel → §7.8.2; continue / skip / abort → §8.5; stage, unstage, discard, commit → §7.6; several cherry-picks or reverts → marked rows; move a branch → `move-branch`; undo → §8.2–§8.3; merge strategy → `merge`'s options; move or edit a tag → `move-tag`; push with lease → §8.4; history search → §4.7; a reflog list, a typed merge message, other push variants, signing existing commits, whole-history rewrites → left out (§10.2) |
| §8 running over ssh, 16 rows | §3 (editor, pager, credentials, long commands, hooks, signing, C locale, quoting, paths, version, multi-step actions), §5 (no watcher), §3.8 (index lock), `archive` (streamed here), §10.2 (terminal-only features) |
