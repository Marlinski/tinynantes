#!/usr/bin/env bash
# Render every Google 3D tile of one heading, 12 tiles per process (the EGL driver leaks GPU memory per tile).
set -e
H=${1:-0}
cd "$(dirname "$0")/.."
uv run python -c "
from tinynantes import geo
t=geo.View($H).tiles_for_area()
t.sort(key=lambda p:(p[1],p[0]), reverse=True)
for k in range(0,len(t),12): print(' '.join(f'{i} {j}' for i,j in t[k:k+12]))
" | while read -r args; do
  uv run python -m tinynantes.g3d "$H" $args || echo "batch failed: $args"   # keep going: a later run retries it
done
echo "heading $H done"
