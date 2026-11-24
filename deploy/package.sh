#!/bin/sh
# Build the Docker build context.
#   deploy/package.sh        code only: Dockerfile + server sources + web/ without the painted map (~35 MB).
#                            The map (tile pyramids, height maps, headings.json, tiles.json) ships apart:
#                            deploy/sync-tiles.sh, and the pod reads it from its own volume (TN_TILES=/tiles).
#   deploy/package.sh full   everything in the image, map included (the old way, ~480 MB).
# -> dist/tinynantes-context-<version>[-full].tar.gz   (needs GNU tar)
set -eu
cd "$(dirname "$0")/.."
mode="${1:-code}"
[ -f web/data/headings.json ] || { echo "no published tiles: run 'uv run python -m tinynantes.publish' first" >&2; exit 1; }
ls web/data/world/nav/*.png >/dev/null 2>&1 || { echo "no nav grid: run 'uv run python -m tinynantes.world nav'" >&2; exit 1; }
v="$(date -u +%Y%m%d-%H%M)-$(git rev-parse --short HEAD)"
printf '{"version":"%s","headings":%s}\n' "$v" "$(cat web/data/headings.json)" > web/version.json
mkdir -p dist server/data/places
if [ "$mode" = full ]; then
  out="dist/tinynantes-context-$v-full.tar.gz"; skip=""
else
  out="dist/tinynantes-context-$v.tar.gz"
  skip="--exclude=web/data/h[0-9] --exclude=web/data/headings.json --exclude=web/data/tiles.json"
fi
# shellcheck disable=SC2086
tar -czf "$out" \
  --exclude='*.tmp.png' $skip \
  --transform 's,^deploy/Dockerfile$,Dockerfile,' --transform 's,^server/data/places,places,' \
  deploy/Dockerfile server/Cargo.toml server/Cargo.lock server/rust-toolchain.toml server/src server/data/places web
ls -lh "$out"
