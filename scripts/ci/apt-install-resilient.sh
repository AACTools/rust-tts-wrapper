#!/usr/bin/env bash
# Resilient apt install for GitHub-hosted Ubuntu runners.
#
# The canonical mirrors (archive.ubuntu.com, security.ubuntu.com,
# azure.archive.ubuntu.com) intermittently fail DNS resolution from
# Azure runners for hours at a time (observed 2026-11-11: every other
# host — github.com, crates.io, packages.microsoft.com — resolved
# fine). Retry once for transient blips, then fall back to
# mirrors.kernel.org, which is outside the *.ubuntu.com DNS zone.
#
# Usage: apt-install-resilient.sh <packages...>
set -uo pipefail

pkgs="$*"

try_install() {
  sudo apt-get update -qq && sudo apt-get install -y --no-install-recommends $pkgs
}

if try_install; then
  exit 0
fi

echo "::warning::canonical apt mirrors unreachable; retrying after 60s"
sleep 60
if try_install; then
  exit 0
fi

echo "::warning::apt still failing — swapping to mirrors.kernel.org"
codename="$(lsb_release -cs)"
sudo mv /etc/apt/apt-mirrors.txt /etc/apt/apt-mirrors.txt.bak 2>/dev/null || true
for pocket in "" "-updates" "-security"; do
  echo "deb https://mirrors.kernel.org/ubuntu ${codename}${pocket} main restricted universe multiverse" \
    | sudo tee -a /etc/apt/sources.list >/dev/null
done
if try_install; then
  exit 0
fi

echo "::error::apt install failed on both canonical and fallback mirrors"
exit 1
