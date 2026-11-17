#!/usr/bin/env bash
# Installs KiCad 10 from its Launchpad PPA on a CI runner, with the extra packages given, and
# fails unless `kicad-cli` is version 10.
#
# Usage: scripts/ci-install-kicad.sh [package...]
#
# Launchpad answers 503 now and then. `apt-get update` then only warns, and `apt-get install
# kicad` takes Ubuntu's own package (7.0 on 24.04), whose kicad-cli has no `pcb drc`: the oracle
# and corpus tests failed at their first KiCad call with nothing on stderr (DECISIONS D112). So
# the PPA is added and its index fetched again until apt's candidate for `kicad` is a 10.x, and
# the installed version is checked.
set -euo pipefail

want=10
tries=8
candidate=""
for i in $(seq 1 "$tries"); do
  sudo add-apt-repository --yes --no-update ppa:kicad/kicad-10.0-releases || true
  sudo apt-get update || true
  candidate="$(apt-cache policy kicad | awk '/Candidate:/ { print $2 }')"
  case "$candidate" in
    "$want".*) break ;;
  esac
  echo "kicad candidate is '${candidate:-none}', not $want.x (try $i of $tries): the PPA index did not come" >&2
  if [ "$i" -eq "$tries" ]; then
    echo "the KiCad $want PPA could not be read after $tries tries" >&2
    exit 1
  fi
  sleep $((i * 20))
done

# No recommends: the symbol, footprint and 3D libraries are not needed (and are large).
for i in 1 2 3; do
  if sudo apt-get install --yes --no-install-recommends "kicad=$candidate" "$@"; then
    break
  fi
  if [ "$i" -eq 3 ]; then
    echo "apt-get install failed three times" >&2
    exit 1
  fi
  sleep $((i * 20))
done

version="$(kicad-cli version)"
echo "kicad-cli $version"
case "$version" in
  "$want".*) ;;
  *)
    echo "kicad-cli is $version, not $want.x" >&2
    exit 1
    ;;
esac
