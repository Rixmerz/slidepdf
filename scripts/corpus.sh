#!/usr/bin/env bash
# Fetches the real-deck corpus used by tests/corpus.rs into the folder given
# as argument: two sparse, blobless, depth-1 clones pinned to a commit, so the
# baseline in tests/corpus-baseline.json stays comparable over time.
#
#   scripts/corpus.sh /path/to/corpus
#   SLIDEPDF_CORPUS=/path/to/corpus cargo test --release --test corpus -- --ignored
#
# Idempotent: a clone already at the pinned commit is left alone. CI caches
# the folder keyed on the hash of this file, so bumping a pin refreshes it.
set -euo pipefail

LO_REPO=https://github.com/LibreOffice/core.git
LO_COMMIT=72b0b19844eeac57627758f0c363f6943b9114b1
LO_PATH=sd/qa/unit/data

POI_REPO=https://github.com/apache/poi.git
POI_COMMIT=39edce935ef8ed746b4457062ecfa2670eabe49f
POI_PATH=test-data/slideshow

fetch() {
  local name=$1 repo=$2 commit=$3 path=$4 dir="$dest/$1"
  if [ "$(git -C "$dir" rev-parse HEAD 2>/dev/null || true)" = "$commit" ] \
    && [ -z "$(git -C "$dir" status --porcelain)" ]; then
    echo "corpus: $name already at ${commit:0:10}"
    return
  fi
  rm -rf "$dir"
  git init --quiet "$dir"
  git -C "$dir" remote add origin "$repo"
  git -C "$dir" sparse-checkout set "$path"
  git -C "$dir" fetch --quiet --depth 1 --filter=blob:none origin "$commit"
  git -C "$dir" checkout --quiet FETCH_HEAD
  echo "corpus: $name at ${commit:0:10}"
}

if [ $# -ne 1 ]; then
  echo "usage: $0 <corpus-folder>" >&2
  exit 2
fi
dest=$1
mkdir -p "$dest"

fetch lo "$LO_REPO" "$LO_COMMIT" "$LO_PATH"
fetch poi "$POI_REPO" "$POI_COMMIT" "$POI_PATH"
echo "corpus: $(find "$dest" -iname '*.pptx' | wc -l) decks in $dest"
