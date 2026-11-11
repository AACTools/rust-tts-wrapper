#!/usr/bin/env bash
# Resilient apt install for GitHub-hosted Ubuntu runners.
#
# The canonical mirrors (archive.ubuntu.com, security.ubuntu.com,
# azure.archive.ubuntu.com) intermittently fail DNS resolution from
# Azure runners for hours at a time (observed 2026-11-11: every other
# host — github.com, crates.io, packages.microsoft.com — resolved
# fine). Strategy:
#   1. try the configured mirrors (fast timeouts so failures are quick)
#   2. retry once after 60 s (transient blips)
#   3. PURGE every apt source (including the mirror+file indirection,
#      which stays poisoned if you only move apt-mirrors.txt) and
#      install from mirrors.kernel.org — a different DNS zone
#   4. same again via mirror.math.princeton.edu
#
# Usage: apt-install-resilient.sh <packages...>
set -uo pipefail

pkgs="$*"

APT_OPTS="-o Acquire::Retries=1 -o Acquire::http::Timeout=15 -o Acquire::https::Timeout=15"

try_install() {
  sudo apt-get $APT_OPTS update -qq \
    && sudo apt-get $APT_OPTS install -y --no-install-recommends $pkgs
}

swap_mirror() {
  local host="$1" path="$2"
  echo "::warning::swapping all apt sources to ${host}"
  local codename
  codename="$(lsb_release -cs)"
  sudo rm -f /etc/apt/apt-mirrors.txt
  sudo find /etc/apt/sources.list.d -type f -delete 2>/dev/null || true
  sudo rm -f /etc/apt/sources.list
  for pocket in "" "-updates" "-security"; do
    echo "deb https://${host}${path} ${codename}${pocket} main restricted universe multiverse" \
      | sudo tee -a /etc/apt/sources.list >/dev/null
  done
}

if try_install; then
  exit 0
fi

echo "::warning::canonical apt mirrors unreachable; retrying after 60s"
sleep 60
if try_install; then
  exit 0
fi

swap_mirror "mirrors.kernel.org" "/ubuntu"
if try_install; then
  exit 0
fi

swap_mirror "mirror.math.princeton.edu" "/pub/ubuntu"
if try_install; then
  exit 0
fi

echo "::error::apt install failed on canonical and both fallback mirrors"
exit 1
