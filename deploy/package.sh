#!/bin/sh
# Build the Docker build context: Dockerfile + nginx conf + web/ (player and published tiles).
# Usage: deploy/package.sh  ->  dist/tinynantes-context-<version>.tar.gz
set -eu
cd "$(dirname "$0")/.."
[ -f web/data/headings.json ] || { echo "no published tiles: run 'uv run python -m tinynantes.publish' first" >&2; exit 1; }
v="$(date -u +%Y%m%d-%H%M)-$(git rev-parse --short HEAD)"
printf '{"version":"%s","headings":%s}\n' "$v" "$(cat web/data/headings.json)" > web/version.json
mkdir -p dist
tar -czf "dist/tinynantes-context-$v.tar.gz" --exclude='*.tmp.png' deploy/Dockerfile deploy/nginx.conf web
ls -lh "dist/tinynantes-context-$v.tar.gz"
