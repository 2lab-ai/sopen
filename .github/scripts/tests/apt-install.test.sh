#!/usr/bin/env bash
# Test for .github/scripts/apt-install.sh's source rewrite (no apt, no network, no root): sample
# sources in the shapes the runners and the Ubuntu images have — one-line and deb822, the
# runner's own mirror list, archive and security hosts, a subdomain mirror, ports.ubuntu.com and
# a third-party repository — are rewritten to the mirror list, and only those that should be.
set -euo pipefail

SCRIPT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/apt-install.sh"
ROOT="$(mktemp -d)"
trap 'rm -rf "$ROOT"' EXIT
etc="$ROOT/etc/apt"
mkdir -p "$etc/sources.list.d"

cat > "$etc/sources.list" << 'EOF'
deb http://archive.ubuntu.com/ubuntu/ noble main restricted
deb http://security.ubuntu.com/ubuntu noble-security main
deb http://us.archive.ubuntu.com/ubuntu/ noble-updates main
deb mirror+file:/etc/apt/apt-mirrors.txt noble-backports main
deb http://ports.ubuntu.com/ubuntu-ports noble main
deb https://packages.example.org/apt stable main
EOF
cat > "$etc/sources.list.d/ubuntu.sources" << 'EOF'
Types: deb
URIs: http://archive.ubuntu.com/ubuntu/
Suites: noble noble-updates noble-backports
Components: main universe

Types: deb
URIs: http://security.ubuntu.com/ubuntu/
Suites: noble-security
Components: main universe
EOF

SOPEN_APT_ETC="$etc" SOPEN_APT_DRY=1 bash "$SCRIPT" ignored-package

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

mirror="mirror+file:$etc/sopen-mirrors.txt"
want_list="deb $mirror noble main restricted
deb $mirror noble-security main
deb $mirror noble-updates main
deb $mirror noble-backports main
deb http://ports.ubuntu.com/ubuntu-ports noble main
deb https://packages.example.org/apt stable main"
[ "$(cat "$etc/sources.list")" = "$want_list" ] || fail "sources.list:
$(cat "$etc/sources.list")"

[ "$(grep -c "^URIs: $mirror\$" "$etc/sources.list.d/ubuntu.sources")" = 2 ] ||
  fail "ubuntu.sources:
$(cat "$etc/sources.list.d/ubuntu.sources")"
grep -q '^Suites: noble-security$' "$etc/sources.list.d/ubuntu.sources" || fail "suites kept"

# The list: three mirrors in priority order, a tab before each priority (apt-transport-mirror(1)).
printf -v want_mirrors '%s\tpriority:%s\n' \
  http://azure.archive.ubuntu.com/ubuntu/ 1 \
  http://mirrors.edge.kernel.org/ubuntu/ 2 \
  http://archive.ubuntu.com/ubuntu/ 3
[ "$(cat "$etc/sopen-mirrors.txt")" = "${want_mirrors%$'\n'}" ] || fail "mirror list:
$(cat "$etc/sopen-mirrors.txt")"

# A second run (a rerun job) changes nothing more.
before="$(cat "$etc/sources.list" "$etc/sources.list.d/ubuntu.sources")"
SOPEN_APT_ETC="$etc" SOPEN_APT_DRY=1 bash "$SCRIPT" ignored-package
[ "$(cat "$etc/sources.list" "$etc/sources.list.d/ubuntu.sources")" = "$before" ] || fail "rerun"

echo "apt-install.sh: ok"
