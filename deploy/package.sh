#!/bin/sh
# Build the Docker build context: Dockerfile + server sources + web/ (viewer, published tiles, world data) + crawled places.
# Usage: deploy/package.sh  ->  dist/tinynantes-context-<version>.tar.gz   (needs GNU tar)
set -eu
cd "$(dirname "$0")/.."
[ -f web/data/headings.json ] || { echo "no published tiles: run 'uv run python -m tinynantes.publish' first" >&2; exit 1; }
ls web/data/world/nav/*.png >/dev/null 2>&1 || { echo "no nav grid: run 'uv run python -m tinynantes.world nav'" >&2; exit 1; }
v="$(date -u +%Y%m%d-%H%M)-$(git rev-parse --short HEAD)"
printf '{"version":"%s","headings":%s}\n' "$v" "$(cat web/data/headings.json)" > web/version.json
mkdir -p dist server/data/places
tar -czf "dist/tinynantes-context-$v.tar.gz" \
  --exclude='*.tmp.png' \
  --transform 's,^deploy/Dockerfile$,Dockerfile,' --transform 's,^server/data/places,places,' \
  deploy/Dockerfile server/Cargo.toml server/Cargo.lock server/rust-toolchain.toml server/src server/data/places web
ls -lh "dist/tinynantes-context-$v.tar.gz"
