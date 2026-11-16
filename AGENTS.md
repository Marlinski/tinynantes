# AGENTS.md

Tiny Nantes: an isometric pixel-art map of Nantes, in the spirit of tinylasvegas.com / tinyrj.com
(those are closed source; everything here is written from scratch).

## Pipeline

1. **Data** (`tinynantes/ign.py`): IGN Géoplateforme open data, fetched in 500 m Lambert-93 chunks and
   cached under `cache/ign/` (gitignored):
   LiDAR HD MNS (surface) and MNT (terrain) at 0.5 m, LiDAR HD classification map, BD ORTHO 20 cm,
   BD TOPO building footprints (WFS).
2. **Render** (`tinynantes/render.py`): the MNS heightfield is meshed and drawn with moderngl (headless EGL)
   from an orthographic camera at 45° elevation. Roofs/ground get the orthophoto; steep faces get a tuffeau
   facade with procedural windows (buildings), dark foliage (vegetation) or stone (other). Output
   `out/h0/render/I_J.png`.
3. **Stylize** (`tinynantes/stylize.py`): each render is redrawn as pixel art by FLUX.2 [klein] 9B edit on
   fal.ai. Output `out/h0/art/I_J.png`.
4. **Viewer**: static web page that pans/zooms the tile pyramid.

Geometry lives in `tinynantes/geo.py`: tiles are 1024 px = 300 m of *screen* space (0.293 m/px),
tile (i, j) covers screen x in [i*300, (i+1)*300), screen y (downwards) in [j*300, (j+1)*300),
relative to `ORIGIN` (the Château). Heading 0 looks from the south-west toward the north-east.

## Environment

- Use `uv run ...`. The project pins a uv-managed CPython 3.13: the Nix-provided Python can't load
  `libstdc++` for pyproj/scipy wheels (`UV_PYTHON_PREFERENCE=only-managed` if recreating the venv).
- Secrets: `FAL_KEY` lives in `.env` (gitignored). Never print it, never commit it.
  `tools/pre-commit` refuses commits containing key-like strings; install with
  `ln -sf ../../tools/pre-commit .git/hooks/pre-commit`.

## Conventions

- No `Co-Authored-By` / AI attribution trailers in commit messages.
- Don't commit `cache/`, `out/` render intermediates, or `research/` (third-party material kept only for study).
- Data credit: © IGN (Géoplateforme, Licence Ouverte Etalab 2.0).
