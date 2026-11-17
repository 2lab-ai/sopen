# AGENTS.md — sopen

Single source of truth for anyone (human or agent) changing this repo. `CLAUDE.md` only imports this file.

## What sopen is

`sopen [user@]host:path` opens a remote file or folder as if it were local. Folders and text go to Zed's
remote mode; images, video and audio open in sopen's own small window with exactly these edits: rectangle
crop and rectangle pixelize (images), rectangle crop and time cut (video), time cut (audio). Everything else
is fetched to a local cache. It opens on the machine it was typed on, or — with `--target HOST`, `SOPEN_TARGET`
or `target` in sopen's config — on HOST's screen, by running HOST's installed sopen with `--here` over ssh: HOST
is a machine the owner uses sopen on, not a file's remote, which still needs only sshd (spec §4a). Spec and
scope: [`.prd/01-spec.md`](.prd/01-spec.md) — its §1 owner intent and §9 out-of-scope list decide every "should
we add X" question.

## Gate (run before every commit, CI runs the same)

```sh
just check   # cargo fmt --check · cargo clippy --all-targets --locked -- -D warnings · cargo test --locked
```

A change is done when the gate is green on macOS and Linux (CI) and, for anything user-visible, a receipt
exists: a command output or a window screenshot that someone actually looked at. Locally, tests that need ffmpeg,
zsh or fish skip with a note when it is missing; CI installs them and sets `SOPEN_REQUIRE_FFMPEG=1` and
`SOPEN_REQUIRE_SHELLS=1`, so there they fail instead.

## Rules

- **The remote needs only sshd.** Never install, upload or run a helper on the remote. Transport is the system
  `ssh` binary (so `~/.ssh/config` is honoured); file operations are SFTP over that session.
- **Never overwrite silently.** Every write re-checks the remote file first (`lstat`: still a regular file,
  same `(size, mtime)`, same content hash up to 256 MiB) and refuses on change; writes go to a temp name in the
  same directory with the target's owner and mode and are renamed over the target. Spec §5 is the full rule.
  Image Save keeps a local backup of the original first. Media exports always write a new sibling file and
  never use a replacing rename to publish it.
- **No new edit features without the owner.** The edit list above is the whole list (owner, 2026-11-11).
- **ffmpeg/ffprobe are external programs**, resolved at runtime (`SOPEN_FFMPEG` / `SOPEN_FFPROBE`, then PATH).
  Do not link libav*.
- Tests must not need ssh or a network: the remote layer sits behind a trait with a local-filesystem
  implementation used by tests.
- Public repo hygiene: no machine names, IP addresses, home-directory paths or session scratch paths in
  commits, receipts or docs. Commit identity `Z <z@2lab.ai>`.

## Receipts

Windows are verified with test hooks — environment variables, deliberately not in the README.
A scripted run ignores keyboard and mouse input (the owner may be typing elsewhere) and never
prompts on close.

| Hook | Effect |
|---|---|
| `SOPEN_STATE='rect=x,y,w,h;t=3.5;in=2;out=6;op=crop\|pixelize;block=16'` | Initial state in source pixels and seconds; `op` (images only, comma-separated, applied in order) uses `rect`. |
| `SOPEN_SCREENSHOT=<png>` | Open the window, render the state, write a screenshot of the window, exit. Prints `screenshot <png> <w>x<h> title="<host:/abs>" focused=<bool> …`. `SOPEN_WATCHDOG=<s>` (default 30) ends a stalled run. |
| `SOPEN_AUTOSAVE=1` | Images: apply the state and Save without a window (backup, conflict-checked overwrite); exit 0 saved, 3 refused. `SOPEN_AUTOSAVE_DELAY_MS=<ms>` waits before the write so a second process can change the remote (the conflict receipt). |
| `SOPEN_AUTOEXPORT=1` | Media: apply the state and Export without a window; prints the new sibling's path. |
| `SOPEN_EXPORT_HOLD_MS=<ms>` | Media window: start an Export on open and hold it, at its first progress report at or past 30 %, for that long, so `SOPEN_SCREENSHOT` captures the window mid-export; when the window closes sopen waits for the export and prints its result. |
| `SOPEN_WINDOW_SIZE=<w>x<h>` | Open the window at that size in points (layout receipts on a narrow window). |
| `SOPEN_RENDERER=wgpu\|glow` | Force a renderer (default: wgpu, glow when wgpu does not start; `WGPU_BACKEND=dx12` on macOS forces that fallback). |
| `SOPEN_TARGET_BIN=<path>` | `--target` runs this sopen binary on HOST (a build not installed there yet) instead of the one on HOST's PATH or in its Homebrew / cargo install paths. A `--target` run carries this and the hooks above, and `SOPEN_LOG`, to HOST; settings naming local paths stay here. |
| `SOPEN_CONFIG=<file>` | The config file to use instead of `$XDG_CONFIG_HOME/sopen/config.toml` / `~/.config/sopen/config.toml` (tests point it at a temp file, so the owner's default target never forwards a test run). |
| `sopen --selftest-audio <file>` | Hidden: play the file on the real output device at gain 0 for ~2 s; prints `callbacks=N underruns=M device=<name> … decoder=ffmpeg\|wav` (a WAV is read in-process where there is no ffmpeg). |

A screenshot is a receipt only after someone has looked at it. Live receipts write only under a
remote `/tmp/sopen-receipts/` directory created for the purpose.

## Distribution (llmux shape)

- `ci.yml` — every PR and main push: the gate on `macos-latest` and `ubuntu-latest`. `preview.yml` and
  `release.yml` call it first and publish nothing unless it is green.
- `preview.yml` — every main push: gate, then macOS arm64/x86_64 and Linux x86_64/aarch64 binaries (Linux built
  on `ubuntu-22.04` / `ubuntu-22.04-arm` for an older glibc floor), a `preview-<YYYY-MM-DD-HHMM>-<sha12>`
  prerelease, and the tap's `Formula/sopen-preview.rb`.
- `release.yml` — `v*` tag equal to the Cargo version (tagging is the owner's call): gate, the same four
  binaries, a stable release, then the tap's `Formula/sopen.rb`.
- Tap bump = `.github/scripts/bump-tap.sh` (regression test `.github/scripts/tests/bump-tap.test.sh`): renders
  `Formula/<name>.rb` from this repo's `packaging/homebrew/<name>.rb.tmpl` with the sha256s of the published
  assets and pushes only that file to `2lab-ai/homebrew-tap`. The tap holds no templates.
  - With the repo secret `TAP_DISPATCH_TOKEN`: the workflow pushes the formula; a failed bump fails the job.
  - Without it: the bump job only prints a notice and the tap is not touched. The dispatcher then pushes the
    rendered formula to the tap by hand: download the release assets into `release/`, and from a checkout of
    the released commit run `FORMULA=<sopen-preview|sopen> TAG=<tag> [BUILD_ID=<build id>] GH_TOKEN=<token
    that can push the tap> .github/scripts/bump-tap.sh`.
- Install: `brew install 2lab-ai/tap/sopen-preview`. The formula depends on `ffmpeg`.
