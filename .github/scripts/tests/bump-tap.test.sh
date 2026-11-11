#!/usr/bin/env bash
# Regression test for .github/scripts/bump-tap.sh (preview and stable channels).
#
# Hermetic: `gh` is mocked (only `auth setup-git` is expected) and the tap is a
# local bare repo, so the real clone/render/commit/push path runs with real git
# and no network or credential.
#
# What this pins (ported from llmux, where the publish step once skipped on a
# missing credential and `exit 0`ed on a failed push): once the script runs,
# every failure mode below must exit non-zero. Skipping when the repo has no
# credential is the workflow's `if:`, never the script's. And the formula is
# rendered from this repo's templates: the tap holds none, and a bump pushes
# exactly one rendered Formula/*.rb.
set -euo pipefail

SCRIPT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/bump-tap.sh"
[ -f "$SCRIPT" ] || {
  echo "missing script under test: $SCRIPT" >&2
  exit 1
}
command -v ruby > /dev/null 2>&1 || {
  echo "ruby is required (the script syntax-checks the rendered formula)" >&2
  exit 1
}

ROOT="$(mktemp -d)"
trap 'rm -rf "$ROOT"' EXIT

# Sentinel credential: must never surface in output or in the clone.
TOKEN="gho_sentinel_must_not_appear_000000000000"
TAG="preview-2026-09-11-0404-abc123def456"
VERSION="2026.09.11.0404"

FAILURES=0
ok() { printf 'ok   - %s\n' "$1"; }
notok() {
  printf 'FAIL - %s\n' "$1"
  FAILURES=$((FAILURES + 1))
}
expect_zero() {
  if [ "$RC" -eq 0 ]; then ok "$1"; else
    notok "$1 (rc=$RC)"
    printf '%s\n' "$OUT" | sed 's/^/       | /'
  fi
}
expect_nonzero() {
  if [ "$RC" -ne 0 ]; then ok "$1"; else
    notok "$1 (rc=0, expected failure)"
    printf '%s\n' "$OUT" | sed 's/^/       | /'
  fi
}
expect_out() {
  case "$OUT" in
    *"$1"*) ok "$2" ;;
    *)
      notok "$2 (output lacks '$1')"
      printf '%s\n' "$OUT" | sed 's/^/       | /'
      ;;
  esac
}
expect_true() { if "${@:2}"; then ok "$1"; else notok "$1"; fi; }
# passes when the command fails, i.e. when the pattern is absent
expect_absent() { if "${@:2}"; then notok "$1"; else ok "$1"; fi; }

# --- hermetic environment -----------------------------------------------
mkdir -p "$ROOT/bin"
cat > "$ROOT/bin/gh" << 'MOCK'
#!/usr/bin/env bash
# The local tap remote needs no credential helper, so setup-git is a no-op.
if [ "${1:-}" = "auth" ] && [ "${2:-}" = "setup-git" ]; then exit 0; fi
echo "unexpected gh invocation: $*" >&2
exit 1
MOCK
chmod +x "$ROOT/bin/gh"
export PATH="$ROOT/bin:$PATH"
# Never read or write the caller's ~/.gitconfig.
export GIT_CONFIG_GLOBAL="$ROOT/gitconfig"
export GIT_CONFIG_NOSYSTEM=1
git config --global user.name "test"
git config --global user.email "test@example.com"
git config --global init.defaultBranch master

# Fixture templates (the repo side). Each case gets its own copy, so a drift
# case can change one without touching the others.
FIXTURES="$ROOT/fixtures"
mkdir -p "$FIXTURES"
formula_fixture() {
  local class="$1" url_tag="$2"
  cat << TMPL
class $class < Formula
  desc "test fixture mirroring packaging/homebrew"
  homepage "https://github.com/2lab-ai/sopen"
  version "@VERSION@"

  on_macos do
    on_arm do
      url "https://github.com/2lab-ai/sopen/releases/download/$url_tag/sopen-macos-aarch64"
      sha256 "@SHA_MACOS_AARCH64@"
    end
    on_intel do
      url "https://github.com/2lab-ai/sopen/releases/download/$url_tag/sopen-macos-x86_64"
      sha256 "@SHA_MACOS_X86_64@"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/2lab-ai/sopen/releases/download/$url_tag/sopen-linux-aarch64"
      sha256 "@SHA_LINUX_AARCH64@"
    end
    on_intel do
      url "https://github.com/2lab-ai/sopen/releases/download/$url_tag/sopen-linux-x86_64"
      sha256 "@SHA_LINUX_X86_64@"
    end
  end
end
TMPL
}
formula_fixture SopenPreview "@TAG@" > "$FIXTURES/sopen-preview.rb.tmpl"
formula_fixture Sopen "v@VERSION@" > "$FIXTURES/sopen.rb.tmpl"

sha256_of() {
  if command -v sha256sum > /dev/null 2>&1; then
    sha256sum "$1" | cut -d' ' -f1
  else
    shasum -a 256 "$1" | cut -d' ' -f1
  fi
}

# seed_tap <bare-repo> <formula> [current-tag] — a tap holding a README and,
# with a current tag, an already rendered Formula/<formula>.rb. Never templates.
seed_tap() {
  local origin="$1" formula="$2" current="${3:-}"
  local work="${origin%.git}-seed"
  git init --quiet --bare "$origin"
  git -C "$origin" symbolic-ref HEAD refs/heads/master
  git init --quiet "$work"
  echo "tap" > "$work/README.md"
  if [ -n "$current" ]; then
    mkdir -p "$work/Formula"
    local ver
    case "$current" in
      preview-*) ver="$(echo "${current#preview-}" | awk -F- '{print $1"."$2"."$3"."$4}')" ;;
      v*) ver="${current#v}" ;;
    esac
    sed -e "s/@VERSION@/$ver/g" -e "s/@TAG@/$current/g" -e "s/@SHA[A-Z_0-9]*@/00seed/g" \
      "$FIXTURES/$formula.rb.tmpl" > "$work/Formula/$formula.rb"
  fi
  git -C "$work" add -A
  git -C "$work" commit --quiet -m "seed"
  git -C "$work" push --quiet "$origin" HEAD:master
}

# make_release <dir> <tag> — the four binaries this build published.
make_release() {
  local dir="$1" tag="$2"
  mkdir -p "$dir"
  local name
  for name in sopen-macos-aarch64 sopen-macos-x86_64 sopen-linux-aarch64 sopen-linux-x86_64; do
    echo "$name binary for $tag" > "$dir/$name"
  done
}

# new_case <name> [current-tag] [formula] — sets CASE_DIR/ORIGIN/WORK/TEMPLATES/FORMULA.
new_case() {
  CASE_DIR="$ROOT/case-$1"
  FORMULA="${3:-sopen-preview}"
  ORIGIN="$CASE_DIR/tap.git"
  WORK="$CASE_DIR/work"
  TEMPLATES="$CASE_DIR/templates"
  mkdir -p "$WORK" "$TEMPLATES"
  cp "$FIXTURES"/*.rb.tmpl "$TEMPLATES/"
  seed_tap "$ORIGIN" "$FORMULA" "${2:-}"
  make_release "$WORK/release" "$1"
}

# run_bump [tag] [token] [build_id] — runs the script in WORK against ORIGIN;
# sets OUT/RC/TAP_CLONE. Each run clones into its own destination, the way a
# fresh runner workspace hands the step an empty path (TAP_DIR_FORCE overrides).
RUN_N=0
run_bump() {
  local tag="${1:-$TAG}" token="${2-$TOKEN}"
  local build="${3-}"
  [ -n "$build" ] || build="${tag#preview-}"
  RUN_N=$((RUN_N + 1))
  TAP_CLONE="${TAP_DIR_FORCE:-tap-bump-$RUN_N}"
  set +e
  OUT="$(cd "$WORK" && FORMULA="$FORMULA" TAG="$tag" BUILD_ID="$build" GH_TOKEN="$token" \
    RELEASE_DIR=release TEMPLATE_DIR="$TEMPLATES" TAP_REMOTE="$ORIGIN" TAP_BRANCH=master \
    TAP_DIR="$TAP_CLONE" bash "$SCRIPT" 2>&1)"
  RC=$?
  set -e
}

pushed_formula() { git -C "$ORIGIN" show "master:Formula/$FORMULA.rb"; }
pushed_paths() { git -C "$ORIGIN" show --name-only --format= master; }
tap_templates() { git -C "$ORIGIN" ls-tree -r --name-only master | grep -c '\.tmpl$' || true; }
# sha_after_url <formula-text> <asset> — the sha256 on the line right after the
# url line that downloads <asset> (empty when that line is not a sha256).
sha_after_url() {
  awk -v want="/$2\"" '
    hit { if ($1 == "sha256") { gsub(/"/, "", $2); print $2 }; exit }
    $1 == "url" && index($0, want) { hit = 1 }
  ' <<< "$1"
}
commit_count() { git -C "$ORIGIN" rev-list --count master; }

# --- case: invoked without a credential ----------------------------------
new_case no-token
before="$(commit_count)"
run_bump "$TAG" ""
expect_nonzero "missing GH_TOKEN fails the step instead of skipping"
expect_out "TAP_DISPATCH_TOKEN" "missing-token error names the credential to register"
expect_true "missing GH_TOKEN pushes nothing" [ "$before" = "$(commit_count)" ]

# --- case: missing artifacts --------------------------------------------
new_case no-binary
rm -f "$WORK/release/sopen-linux-aarch64"
run_bump
expect_nonzero "missing binary asset fails the step"
expect_out "sopen-linux-aarch64" "missing-binary error names the asset"

# --- case: input validation ----------------------------------------------
new_case bad-build-id
run_bump "preview-2026-09-11-0404-nothex123456"
expect_nonzero "a build id whose sha12 is not hex fails the step"
expect_out "unexpected build id" "malformed build id is named in the error"

new_case tag-build-mismatch
run_bump "$TAG" "$TOKEN" "2026-09-11-0500-abc123def456"
expect_nonzero "a TAG that disagrees with BUILD_ID fails the step"
expect_out "does not match" "tag/build mismatch is named in the error"

new_case no-template
rm -f "$TEMPLATES/sopen-preview.rb.tmpl"
before="$(commit_count)"
run_bump
expect_nonzero "a missing repo template fails the step"
expect_out "missing template" "missing-template error says what is missing"
expect_true "the missing-template case pushes nothing" [ "$before" = "$(commit_count)" ]

# --- case: the clone destination is never clobbered -----------------------
# The script must refuse an occupied destination rather than delete a path it
# was handed (TAP_DIR is caller-supplied).
new_case occupied-dest "preview-2026-09-10-1200-000000000000"
mkdir -p "$WORK/occupied/nested"
echo "do-not-delete" > "$WORK/occupied/nested/sentinel"
before="$(commit_count)"
TAP_DIR_FORCE=occupied run_bump
unset TAP_DIR_FORCE
expect_nonzero "an existing clone destination fails the step"
expect_out "already exists" "occupied-destination error says what it refused"
expect_true "the occupied destination is left untouched" \
  [ "$(cat "$WORK/occupied/nested/sentinel" 2> /dev/null)" = "do-not-delete" ]
expect_true "the occupied-destination case pushes nothing" \
  [ "$before" = "$(commit_count)" ]

# --- case: happy path ----------------------------------------------------
new_case happy "preview-2026-09-10-1200-000000000000"
run_bump
expect_zero "happy path succeeds"
expect_out "tap bumped to $TAG" "happy path reports the push"
formula="$(pushed_formula)"
expect_true "pushed formula carries the brew version" \
  grep -q "version \"$VERSION\"" <<< "$formula"
expect_true "pushed formula points at this tag" grep -q "download/$TAG/" <<< "$formula"
# Per platform, not "somewhere in the file": a swapped pair would still carry
# all four hashes, but brew would reject (or worse, accept) the wrong binary.
for asset in sopen-macos-aarch64 sopen-macos-x86_64 sopen-linux-aarch64 sopen-linux-x86_64; do
  expect_true "pushed formula pairs the $asset url with its own sha256" \
    [ "$(sha_after_url "$formula" "$asset")" = "$(sha256_of "$WORK/release/$asset")" ]
done
expect_absent "no unrendered template token reaches the tap" \
  grep -q "@[A-Z0-9_]\+@" <<< "$formula"
expect_true "the bump commit touches only Formula/sopen-preview.rb" \
  [ "$(pushed_paths)" = "Formula/sopen-preview.rb" ]
expect_true "the tap holds no template" [ "$(tap_templates)" = "0" ]
expect_absent "the token never reaches the log" grep -qF "$TOKEN" <<< "$OUT"
expect_absent "the token never reaches the clone" \
  grep -rqF "$TOKEN" "$WORK/$TAP_CLONE/.git"

# rerunning the same build is a no-op, not a second commit
after_first="$(commit_count)"
run_bump
expect_zero "rerunning the same build succeeds"
expect_out "already current" "rerun detects the tap is already at this build"
expect_true "rerun adds no commit" [ "$after_first" = "$(commit_count)" ]

# --- case: first bump into a tap that has never had this formula ---------
new_case first-bump
run_bump
expect_zero "a tap without the formula (or any template) is bumped"
expect_true "first bump pushes a formula pointing at this tag" \
  grep -q "download/$TAG/" <<< "$(pushed_formula)"

# --- case: freshness guards ---------------------------------------------
new_case newer-tap "preview-2026-09-12-0100-ffffffffffff"
before="$(commit_count)"
run_bump
expect_zero "a newer tap is left alone"
expect_out "already at newer" "newer-tap skip is reported"
expect_true "newer tap is not rolled back" [ "$before" = "$(commit_count)" ]

new_case same-minute "preview-2026-09-11-0404-ffffffffffff"
before="$(commit_count)"
run_bump
expect_nonzero "same-minute ambiguity fails instead of guessing"
expect_out "same minute" "same-minute error explains the ambiguity"
expect_true "same-minute case pushes nothing" [ "$before" = "$(commit_count)" ]

# --- case: push failures -------------------------------------------------
new_case push-rejected "preview-2026-09-10-1200-000000000000"
printf '#!/bin/sh\necho "simulated rejection" >&2\nexit 1\n' > "$ORIGIN/hooks/pre-receive"
chmod +x "$ORIGIN/hooks/pre-receive"
before="$(commit_count)"
run_bump
expect_nonzero "a rejected push fails the step"
expect_out "after 3 attempts" "push failure reports the retry exhaustion"
expect_true "rejected push leaves the tap untouched" [ "$before" = "$(commit_count)" ]

new_case push-race "preview-2026-09-10-1200-000000000000"
printf '#!/bin/sh\nif [ -f ./reject-once ]; then rm -f ./reject-once; echo "simulated race" >&2; exit 1; fi\nexit 0\n' \
  > "$ORIGIN/hooks/pre-receive"
chmod +x "$ORIGIN/hooks/pre-receive"
: > "$ORIGIN/reject-once"
run_bump
expect_zero "a lost push race is retried to success"
expect_out "tap bumped to $TAG" "retry reaches the push"

# --- case: template drift (in the repo's template) -----------------------
new_case drift
printf '  sha256 "@SHA_UNEXPECTED@"\n' >> "$TEMPLATES/sopen-preview.rb.tmpl"
run_bump
expect_nonzero "an unrendered template token fails the step"

# Tokens carry digits (@SHA_MACOS_X86_64@), so the detector must too: a
# digit-bearing leftover is valid ruby and slips past `ruby -c`.
new_case drift-digits
printf '  sha256 "@SHA_UNEXPECTED_X86_64@"\n' >> "$TEMPLATES/sopen-preview.rb.tmpl"
before="$(commit_count)"
run_bump
expect_nonzero "an unrendered digit-bearing template token fails the step"
expect_true "the digit-bearing drift case pushes nothing" [ "$before" = "$(commit_count)" ]

# --- stable channel (release.yml) ----------------------------------------
new_case stable-happy "v0.1.0" sopen
run_bump "v0.2.0"
expect_zero "stable bump succeeds"
expect_out "tap bumped to v0.2.0" "stable bump reports the push"
formula="$(pushed_formula)"
expect_true "stable formula carries version 0.2.0" grep -q 'version "0.2.0"' <<< "$formula"
expect_true "stable formula downloads from the v0.2.0 tag" \
  grep -q "releases/download/v0.2.0/sopen-linux-x86_64" <<< "$formula"
for asset in sopen-macos-aarch64 sopen-macos-x86_64 sopen-linux-aarch64 sopen-linux-x86_64; do
  expect_true "stable formula pairs the $asset url with its own sha256" \
    [ "$(sha_after_url "$formula" "$asset")" = "$(sha256_of "$WORK/release/$asset")" ]
done
expect_true "the stable bump commit touches only Formula/sopen.rb" \
  [ "$(pushed_paths)" = "Formula/sopen.rb" ]

new_case stable-newer "v0.10.0" sopen
before="$(commit_count)"
run_bump "v0.9.1"
expect_zero "a stable tap at a newer version (0.10.0 > 0.9.1) is left alone"
expect_out "already at newer v0.10.0" "stable newer-tap skip is reported"
expect_true "the newer stable tap is not rolled back" [ "$before" = "$(commit_count)" ]

new_case stable-bad-tag "" sopen
run_bump "v0.2"
expect_nonzero "a release tag that is not v<X.Y.Z> fails the step"
expect_out "unexpected release tag" "bad release tag is named in the error"

new_case bad-formula "" sopen-nightly
run_bump
expect_nonzero "an unknown FORMULA fails the step"

# --- case: this repo's own templates render through the real script -------
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
REPO_TEMPLATES="$REPO_ROOT/packaging/homebrew"
for f in sopen-preview sopen; do
  if [ ! -f "$REPO_TEMPLATES/$f.rb.tmpl" ]; then
    notok "missing $REPO_TEMPLATES/$f.rb.tmpl"
    continue
  fi
  if [ "$f" = "sopen" ]; then tag="v0.1.0"; else tag="$TAG"; fi
  new_case "repo-$f" "" "$f"
  TEMPLATES="$REPO_TEMPLATES"
  run_bump "$tag"
  expect_zero "the repo's $f template renders and pushes"
  formula="$(pushed_formula)"
  expect_true "rendered $f formula depends on ffmpeg" grep -q 'depends_on "ffmpeg"' <<< "$formula"
  for asset in sopen-macos-aarch64 sopen-macos-x86_64 sopen-linux-aarch64 sopen-linux-x86_64; do
    expect_true "repo $f template pairs the $asset url with its own sha256" \
      [ "$(sha_after_url "$formula" "$asset")" = "$(sha256_of "$WORK/release/$asset")" ]
  done
  expect_absent "no unrendered token in the repo $f render" grep -q "@[A-Z0-9_]\+@" <<< "$formula"
done

echo
if [ "$FAILURES" -eq 0 ]; then
  echo "all tap-bump regression cases passed"
else
  echo "$FAILURES tap-bump regression case(s) failed"
  exit 1
fi
