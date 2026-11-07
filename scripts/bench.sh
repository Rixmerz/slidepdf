#!/usr/bin/env sh
# Benchmarks the release CLI on generated decks: wall time, CPU time and
# peak memory. Needs `uv` (to run python-pptx) and a release build.
#   scripts/bench.sh [work-dir]
set -eu
cd "$(dirname "$0")/.."
WORK=${1:-target/bench}
mkdir -p "$WORK"
BIN=target/release/slidepdf
[ -x "$BIN" ] || cargo build --release
py() { uv run -q --with python-pptx --with pillow python -I "$@"; }
[ -f "$WORK/course.pptx" ] || py scripts/bench/make_course.py "$WORK"
[ -f "$WORK/huge.pptx" ] || py scripts/bench/make_huge_png.py "$WORK"
[ -f "$WORK/big.pptx" ] || py scripts/bench/make_big_images.py "$WORK"
echo "| case | wall | cpu | peak RSS |"
echo "|---|---|---|---|"
for deck in course huge big; do
  for preset in light balanced print; do
    python3 scripts/bench/measure.py "$deck $preset" "$BIN" fragments "$WORK/$deck.pptx" -o "$WORK/out-$deck-$preset" --preset "$preset" --no-viewer
  done
done
echo
for deck in course huge big; do
  for preset in light balanced print; do
    printf '%s %s: deck.pdf %s bytes\n' "$deck" "$preset" "$(wc -c < "$WORK/out-$deck-$preset/deck.pdf")"
  done
done
