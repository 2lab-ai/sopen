# AGENTS.md — sopen

Single source of truth for anyone (human or agent) changing this repo. `CLAUDE.md` only imports this file.

## What sopen is

`sopen [user@]host:path` opens a remote file or folder as if it were local. Folders and text go to Zed's
remote mode; images, video and audio open in sopen's own small window with exactly these edits: rectangle
crop and rectangle pixelize (images), rectangle crop and time cut (video), time cut (audio). Everything else
is fetched to a local cache. Spec and scope: [`.prd/01-spec.md`](.prd/01-spec.md) — its §1 owner intent and
§9 out-of-scope list decide every "should we add X" question.

## Gate (run before every commit, CI runs the same)

```sh
just check   # cargo fmt --check · cargo clippy --all-targets --locked -- -D warnings · cargo test --locked
```

A change is done when the gate is green on macOS and Linux (CI) and, for anything user-visible, a receipt
exists: a command output or a window screenshot that someone actually looked at.

## Rules

- **The remote needs only sshd.** Never install, upload or run a helper on the remote. Transport is the system
  `ssh` binary (so `~/.ssh/config` is honoured); file operations are SFTP over that session.
- **Never overwrite silently.** Every write re-checks the remote `(size, mtime)` first and refuses on change;
  writes go to a temp name in the same directory and are renamed over the target. Image Save keeps a local
  backup of the original first. Media exports always write a new sibling file.
- **No new edit features without the owner.** The edit list above is the whole list (owner, 2026-11-11).
- **ffmpeg/ffprobe are external programs**, resolved at runtime (`SOPEN_FFMPEG` / `SOPEN_FFPROBE`, then PATH).
  Do not link libav*.
- Tests must not need ssh or a network: the remote layer sits behind a trait with a local-filesystem
  implementation used by tests.
- Public repo hygiene: no machine names, IP addresses, home-directory paths or session scratch paths in
  commits, receipts or docs. Commit identity `Z <z@2lab.ai>`.

## Distribution (llmux shape)

- `ci.yml` — every PR and main push: the gate on `macos-latest` and `ubuntu-latest`.
- `preview.yml` — every main push: builds macOS arm64/x86_64 and Linux x86_64/aarch64 binaries, publishes a
  `preview-<YYYY-MM-DD-HHMM>-<sha12>` prerelease, bumps `2lab-ai/homebrew-tap` `Formula/sopen-preview.rb`.
- `release.yml` — `v*` tag equal to the Cargo version: stable release + `Formula/sopen.rb`. Tagging is the
  owner's call.
- Install: `brew install 2lab-ai/tap/sopen-preview`. The formula depends on `ffmpeg`.
