#!/usr/bin/env bash
# Paint heading H tiles as their Google renders appear; stop once the render job is gone and nothing is left.
H=${1:-0}
cd "$(dirname "$0")/.."
while true; do
  running=$(ps -eo args | grep -c "[g]3d_heading.sh $H")
  uv run python -m tinynantes.build2 "$H" paint
  [ "$running" = "0" ] && break
  sleep 60
done
uv run python -m tinynantes.build2 "$H" paint
echo "paint h$H done"
