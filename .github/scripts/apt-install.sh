#!/usr/bin/env bash
# Installs Ubuntu packages (`apt-install.sh PKG…`) on a CI runner or in a container, through a
# list of mirrors instead of one host.
#
# archive.ubuntu.com and security.ubuntu.com time out at times, and so did the runner's own mirror
# list and, once, azure.archive.ubuntu.com alone. So the Ubuntu archive and security sources (and
# the runner's `mirror+file:/etc/apt/apt-mirrors.txt`) are pointed at apt's mirror method with a
# list of our own: on a failed request apt tries the next mirror, in priority order (see
# apt-transport-mirror(1): one URI per line, metadata after a tab; without `priority:` the order
# would be random). Every listed mirror carries the security pockets too. ports.ubuntu.com (the
# arm64 runners) is left as it is. Retries and a per-request timeout turn a stalled request into a
# retry instead of a hung job.
#
# Root runs apt directly (a container); anyone else through sudo (a runner). For the script's test:
# SOPEN_APT_ETC is the apt directory (default /etc/apt) and SOPEN_APT_DRY=1 stops before apt-get.
set -euo pipefail

etc="${SOPEN_APT_ETC:-/etc/apt}"
list="$etc/sopen-mirrors.txt"

as_root() {
  if [ "$(id -u)" -eq 0 ] || [ -n "${SOPEN_APT_DRY:-}" ]; then
    "$@"
  else
    sudo "$@"
  fi
}

printf '%s\tpriority:%s\n' \
  http://azure.archive.ubuntu.com/ubuntu/ 1 \
  http://mirrors.edge.kernel.org/ubuntu/ 2 \
  http://archive.ubuntu.com/ubuntu/ 3 |
  as_root tee "$list" > /dev/null

for f in "$etc/sources.list" "$etc"/sources.list.d/*.list "$etc"/sources.list.d/*.sources; do
  [ -f "$f" ] || continue
  # One-line (`deb URI suite…`) and deb822 (`URIs: URI`) entries alike; written back through tee
  # (sed -i differs between GNU and BSD sed).
  rewritten="$(sed -E "s#mirror\+file:/etc/apt/apt-mirrors\.txt|https?://([a-z0-9-]+\.)*(archive|security)\.ubuntu\.com/ubuntu/?#mirror+file:$list#g" "$f")"
  printf '%s\n' "$rewritten" | as_root tee "$f" > /dev/null
done

[ -z "${SOPEN_APT_DRY:-}" ] || exit 0
apt=(-o Acquire::Retries=5 -o Acquire::http::Timeout=30)
as_root apt-get "${apt[@]}" update ${SOPEN_APT_QUIET:+-qq}
as_root apt-get "${apt[@]}" install -y ${SOPEN_APT_QUIET:+-qq} "$@"
