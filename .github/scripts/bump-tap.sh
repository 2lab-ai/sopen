#!/usr/bin/env bash
# Publish-time Homebrew tap bump, for both channels.
#
# Renders Formula/$FORMULA.rb from THIS repo's packaging/homebrew/$FORMULA.rb.tmpl
# (the checkout the job runs in) with the sha256s of the assets the release job
# just uploaded — the same bytes brew will download — and pushes only that
# rendered file to the tap. The tap carries no templates.
#
#   FORMULA=sopen-preview   TAG=preview-$BUILD_ID   brew version YYYY.MM.DD.HHMM
#   FORMULA=sopen           TAG=v<X.Y.Z>            brew version X.Y.Z
#
# Fail-closed: once invoked, the bump is not finished until the tap points at
# this build. Every abort except "the tap already carries this or a newer
# build" exits non-zero so the job goes red. Whether to invoke it at all (the
# credential may not be registered yet) is the workflow's decision, not this
# script's.
#
# Auth: GH_TOKEN (repo secret TAP_DISPATCH_TOKEN) through `gh auth setup-git`,
# i.e. an ephemeral git credential helper on the runner. The token never enters
# a remote URL, the clone's config, or the log.
#
# Env contract:
#   FORMULA       optional   sopen-preview (default) or sopen
#   TAG           required   preview-$BUILD_ID, or v<X.Y.Z>
#   BUILD_ID      preview    YYYY-MM-DD-HHMM-<sha12>
#   GH_TOKEN      required   token with push access to the tap
#   RELEASE_DIR   optional   dir holding the published assets (default: release)
#   TEMPLATE_DIR  optional   default packaging/homebrew
#   TAP_REPO      optional   default 2lab-ai/homebrew-tap
#   TAP_REMOTE    optional   default https://github.com/$TAP_REPO.git
#   TAP_BRANCH    optional   default master
#   TAP_DIR       optional   clone destination, must not exist (default: tap-bump)
set -euo pipefail
export LC_ALL=C

fail() {
  echo "error: $*" >&2
  exit 1
}

sha256_of() {
  if command -v sha256sum > /dev/null 2>&1; then
    sha256sum "$1" | cut -d' ' -f1
  else
    shasum -a 256 "$1" | cut -d' ' -f1
  fi
}

# Renders the formula from this repo's template. A surviving @TOKEN@ means the
# template/render contract drifted, which must not reach brew users.
render() {
  mkdir -p "$TAP_DIR/Formula"
  sed -e "s/@VERSION@/$VERSION/g" \
    -e "s/@TAG@/$TAG/g" \
    -e "s/@SHA_MACOS_AARCH64@/$SHA_MACOS_AARCH64/" \
    -e "s/@SHA_MACOS_X86_64@/$SHA_MACOS_X86_64/" \
    -e "s/@SHA_LINUX_AARCH64@/$SHA_LINUX_AARCH64/" \
    -e "s/@SHA_LINUX_X86_64@/$SHA_LINUX_X86_64/" \
    "$TEMPLATE" > "$TAP_DIR/$OUT"
  ruby -c "$TAP_DIR/$OUT" > /dev/null ||
    fail "rendered formula is not valid ruby"
  if grep -q "@[A-Z0-9_]\+@" "$TAP_DIR/$OUT"; then
    fail "unrendered @TOKEN@ left in $OUT; template contract drifted"
  fi
}

# Prints "skip" when the tap already carries a newer build than this one, and
# fails when the order cannot be known. Silent otherwise.
freshness() {
  local current
  if [ "$FORMULA" = "sopen-preview" ]; then
    # preview-YYYY-MM-DD-HHMM-<sha12> is chronological only down to the minute
    # — the sha12 tail carries no time order. Compare minute prefixes.
    current="$(sed -n 's|^.*releases/download/\(preview-[^/]*\)/.*|\1|p' \
      "$TAP_DIR/$OUT" 2> /dev/null | head -1 || true)"
    [ -n "$current" ] && [ "$current" != "$TAG" ] || return 0
    local cur_min="${current%-*}" tag_min="${TAG%-*}"
    if [[ "$cur_min" > "$tag_min" ]]; then
      echo "tap already at newer $current; skipping" >&2
      echo skip
    elif [ "$cur_min" = "$tag_min" ]; then
      # Two builds in the same minute: which one is newer is unknowable from
      # the tag alone. Overwriting could roll brew users back, so stop — and
      # stop loudly rather than leave it to the cron.
      fail "tap at $current from the same minute as $TAG; order unknowable — rerun this job once the ordering is clear"
    fi
  else
    current="$(sed -n 's|^.*releases/download/v\([^/]*\)/.*|\1|p' \
      "$TAP_DIR/$OUT" 2> /dev/null | head -1 || true)"
    [ -n "$current" ] && [ "$current" != "$VERSION" ] || return 0
    if [ "$(printf '%s\n%s\n' "$current" "$VERSION" | sort -V | tail -1)" = "$current" ]; then
      echo "tap already at newer v$current; skipping" >&2
      echo skip
    fi
  fi
}

main() {
  local FORMULA="${FORMULA:-sopen-preview}"
  local TAG="${TAG:-}" BUILD_ID="${BUILD_ID:-}"
  local RELEASE_DIR="${RELEASE_DIR:-release}"
  local TEMPLATE_DIR="${TEMPLATE_DIR:-packaging/homebrew}"
  local TAP_REPO="${TAP_REPO:-2lab-ai/homebrew-tap}"
  local TAP_REMOTE="${TAP_REMOTE:-https://github.com/${TAP_REPO}.git}"
  local TAP_BRANCH="${TAP_BRANCH:-master}"
  local TAP_DIR="${TAP_DIR:-tap-bump}"
  local OUT="Formula/$FORMULA.rb"
  local TEMPLATE="$TEMPLATE_DIR/$FORMULA.rb.tmpl"

  [ -n "$TAG" ] || fail "TAG is required"
  # No silent no-op on a missing credential once invoked: the workflow skips
  # this step when the secret is absent, so reaching here without one is a bug.
  [ -n "${GH_TOKEN:-}" ] ||
    fail "GH_TOKEN is empty; register the tap push credential (secret TAP_DISPATCH_TOKEN)"
  command -v gh > /dev/null 2>&1 || fail "gh is not installed"

  local VERSION
  case "$FORMULA" in
    sopen-preview)
      [ -n "$BUILD_ID" ] || fail "BUILD_ID is required for sopen-preview"
      # build_id = YYYY-MM-DD-HHMM-<sha12> -> brew version YYYY.MM.DD.HHMM.
      # Anchored at both ends: a partial match would render a version the
      # freshness comparison (minute prefix) can no longer be trusted against.
      [[ "$BUILD_ID" =~ ^([0-9]{4})-([0-9]{2})-([0-9]{2})-([0-9]{4})-[0-9a-f]{12}$ ]] ||
        fail "unexpected build id: $BUILD_ID (want YYYY-MM-DD-HHMM-<sha12>)"
      VERSION="${BASH_REMATCH[1]}.${BASH_REMATCH[2]}.${BASH_REMATCH[3]}.${BASH_REMATCH[4]}"
      # The formula's version comes from BUILD_ID and its download URLs from
      # TAG. If the two describe different builds, brew gets a formula whose
      # version and URLs disagree, so require the exact relation preflight makes.
      [ "$TAG" = "preview-$BUILD_ID" ] ||
        fail "TAG $TAG does not match BUILD_ID $BUILD_ID (want preview-$BUILD_ID)"
      ;;
    sopen)
      # The stable template downloads from v@VERSION@, so TAG must be exactly that.
      [[ "$TAG" =~ ^v([0-9]+\.[0-9]+\.[0-9]+)$ ]] ||
        fail "unexpected release tag: $TAG (want v<X.Y.Z>)"
      VERSION="${BASH_REMATCH[1]}"
      ;;
    *) fail "unknown FORMULA $FORMULA (want sopen-preview or sopen)" ;;
  esac
  [ -f "$TEMPLATE" ] || fail "missing template $TEMPLATE (run from the sopen checkout)"

  # The clone destination is never deleted: TAP_DIR is caller-supplied, and a
  # stray `rm -rf` on it is a worse failure than refusing to start. Each
  # Actions run gets a fresh workspace, so an existing path means something
  # unexpected is there — including on a re-run, where the step re-clones into
  # a clean workspace.
  if [ -e "$TAP_DIR" ]; then
    fail "clone destination $TAP_DIR already exists; refusing to touch it"
  fi

  # Plain variables, not an associative array: this script must also run under
  # the bash 3.2 a macOS contributor gets from `/usr/bin/env bash`.
  local name
  for name in sopen-macos-aarch64 sopen-macos-x86_64 sopen-linux-aarch64 sopen-linux-x86_64; do
    [ -f "$RELEASE_DIR/$name" ] || fail "missing published asset $RELEASE_DIR/$name"
  done
  local SHA_MACOS_AARCH64 SHA_MACOS_X86_64 SHA_LINUX_AARCH64 SHA_LINUX_X86_64
  SHA_MACOS_AARCH64="$(sha256_of "$RELEASE_DIR/sopen-macos-aarch64")"
  SHA_MACOS_X86_64="$(sha256_of "$RELEASE_DIR/sopen-macos-x86_64")"
  SHA_LINUX_AARCH64="$(sha256_of "$RELEASE_DIR/sopen-linux-aarch64")"
  SHA_LINUX_X86_64="$(sha256_of "$RELEASE_DIR/sopen-linux-x86_64")"

  # Ephemeral credential helper on this runner; the token stays in the env.
  gh auth setup-git --hostname github.com ||
    fail "gh auth setup-git failed; the tap credential is unusable"

  git clone --depth 1 --branch "$TAP_BRANCH" "$TAP_REMOTE" "$TAP_DIR" ||
    fail "cannot clone $TAP_REPO; check the tap credential's push access"
  git -C "$TAP_DIR" config user.name "github-actions[bot]"
  git -C "$TAP_DIR" config user.email "41898282+github-actions[bot]@users.noreply.github.com"

  local attempt fresh
  for attempt in 1 2 3; do
    git -C "$TAP_DIR" fetch origin "$TAP_BRANCH"
    git -C "$TAP_DIR" reset --hard "origin/$TAP_BRANCH"
    # A failure inside the substitution (unknowable order) must stop the bump.
    fresh="$(freshness)" || exit 1
    if [ "$fresh" = "skip" ]; then
      exit 0
    fi
    render
    git -C "$TAP_DIR" add "$OUT"
    if git -C "$TAP_DIR" diff --cached --quiet; then
      echo "tap already current at $TAG"
      exit 0
    fi
    git -C "$TAP_DIR" commit -m "$FORMULA $TAG"
    if git -C "$TAP_DIR" push origin "HEAD:$TAP_BRANCH"; then
      echo "tap bumped to $TAG"
      exit 0
    fi
    echo "push lost a race (attempt $attempt); refetching" >&2
  done
  fail "tap push failed after 3 attempts; $TAG is published but brew users cannot see it"
}

main "$@"
