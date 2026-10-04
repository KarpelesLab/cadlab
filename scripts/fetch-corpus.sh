#!/usr/bin/env bash
# Fetches the open-source KiCad projects of tests/corpus/projects.toml, each at its pinned commit,
# into a cache directory outside the repository's tracked files (docs/TESTING.md, "Open-source corpus";
# DECISIONS D35). Nothing is vendored: the tests read the result through CADLAB_CORPUS_DIR.
#
# Usage: scripts/fetch-corpus.sh [corpus-dir] [project-name...]
#   corpus-dir defaults to $CADLAB_CORPUS_DIR, else target/corpus.
#
# Each project is a shallow (depth 1), blobless, sparse checkout of exactly the pinned commit
# (GitHub serves any reachable commit by SHA); the checked-out HEAD must equal the pinned SHA.
# A project already present at the right commit is left alone, so the script is cheap on a
# cache hit. Needs git >= 2.27 (non-cone sparse checkout).
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
manifest="$root/tests/corpus/projects.toml"
dest="${1:-${CADLAB_CORPUS_DIR:-$root/target/corpus}}"
shift || true
only=("$@")

mkdir -p "$dest"
dest="$(cd "$dest" && pwd)"

# The manifest's [[project]] tables as tab-separated rows: name, repo, sha, sparse patterns
# (separated by '|'). The manifest only uses `key = "string"` and one-line string arrays.
rows="$(awk '
  function flush() { if (name != "") print name "\t" repo "\t" sha "\t" sparse; name = repo = sha = sparse = "" }
  /^\[\[project\]\]/ { flush(); next }
  /^[a-z]+ *= */ {
    key = $1; val = $0; sub(/^[a-z]+ *= */, "", val)
    if (key == "sparse") {
      gsub(/^\[|\] *$/, "", val); n = split(val, parts, /" *, *"/); s = ""
      for (i = 1; i <= n; i++) { p = parts[i]; gsub(/^ *"|" *$/, "", p); if (p != "") s = s (s == "" ? "" : "|") p }
      sparse = s
    } else if (key == "name" || key == "repo" || key == "sha") {
      gsub(/^"|" *$/, "", val)
      if (key == "name") name = val; else if (key == "repo") repo = val; else sha = val
    }
  }
  END { flush() }
' "$manifest")"

fail=0
while IFS=$'\t' read -r name repo sha sparse; do
  [ -n "$name" ] || continue
  if [ ${#only[@]} -gt 0 ] && [[ ! " ${only[*]} " == *" $name "* ]]; then
    continue
  fi
  if ! [[ "$sha" =~ ^[0-9a-f]{40}$ ]]; then
    echo "error: $name: sha '$sha' is not a full 40-hex commit id" >&2
    fail=1
    continue
  fi
  dir="$dest/$name"
  # The marker records the commit and the checked-out patterns: changing either refetches.
  marker="$sha $sparse"
  if [ -f "$dir/.cadlab-corpus" ] && [ "$(cat "$dir/.cadlab-corpus")" = "$marker" ] \
    && [ "$(git -C "$dir" rev-parse HEAD 2>/dev/null)" = "$sha" ]; then
    echo "$name: present at $sha"
    continue
  fi
  echo "$name: fetching $repo at $sha"
  rm -rf "$dir"
  mkdir -p "$dir"
  git -C "$dir" init -q
  git -C "$dir" remote add origin "$repo"
  if [ -n "$sparse" ]; then
    IFS='|' read -r -a patterns <<<"$sparse"
    git -C "$dir" sparse-checkout set --no-cone "${patterns[@]}"
  fi
  ok=0
  for attempt in 1 2 3; do
    if git -C "$dir" fetch -q --depth 1 --filter=blob:none origin "$sha"; then
      ok=1
      break
    fi
    echo "$name: fetch attempt $attempt failed, retrying" >&2
    sleep $((attempt * 5))
  done
  if [ "$ok" != 1 ] || ! git -C "$dir" -c advice.detachedHead=false checkout -q --detach FETCH_HEAD; then
    echo "error: $name: could not fetch $sha from $repo" >&2
    rm -rf "$dir"
    fail=1
    continue
  fi
  got="$(git -C "$dir" rev-parse HEAD)"
  if [ "$got" != "$sha" ]; then
    echo "error: $name: checked out $got, expected $sha" >&2
    rm -rf "$dir"
    fail=1
    continue
  fi
  echo "$marker" >"$dir/.cadlab-corpus"
done <<<"$rows"

if [ "$fail" != 0 ]; then
  exit 1
fi
echo "corpus ready in $dest (CADLAB_CORPUS_DIR=$dest)"
