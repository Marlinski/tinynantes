#!/bin/sh
# Mirror the painted map to minix (~/Work/tinynantes-tiles/), incrementally: only tiles that changed travel.
# sysiphe then syncs that folder into the pod's tiles volume (mounted at /tiles, TN_TILES=/tiles).
# What goes: web/data/h0..h7 (pyramids, height maps, manifests, overview), headings.json, tiles.json.
# Usage: deploy/sync-tiles.sh [--dry-run]       (SSH_OPTS="-o ControlPath=..." to reuse a master connection)
set -eu
cd "$(dirname "$0")/.."
[ -f web/data/tiles.json ] || { echo "no tiles.json: run 'uv run python -m tinynantes.publish' first" >&2; exit 1; }
dirs=""
for d in web/data/h[0-9]; do [ -d "$d" ] && dirs="$dirs $d"; done
# --checksum: the publish rewrites the coarser levels each time; only files whose content changed are sent
# shellcheck disable=SC2086
rsync -a --checksum --delete --info=stats1 ${1:-} -e "ssh ${SSH_OPTS:-}" \
  $dirs web/data/headings.json web/data/tiles.json minix:Work/tinynantes-tiles/
[ -z "${1:-}" ] && echo "tiles version $(cat web/data/tiles.json) mirrored to minix:Work/tinynantes-tiles/"
