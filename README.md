# Tiny Nantes

Nantes redrawn as an isometric pixel-art map, from French open data.
Inspired by [tinylasvegas.com](https://tinylasvegas.com) / [tinyrj.com](https://tinyrj.com); written from scratch.

## How it works

```
IGN open data ──► 3D render (OpenGL) ──► AI redraw (FLUX.2 klein) ──► tile pyramid ──► web viewer
```

1. **Data**: LiDAR HD surface/terrain models (0.5 m), LiDAR HD classification, BD ORTHO 20 cm aerial photos,
   BD TOPO building footprints and water surfaces, all from the IGN Géoplateforme (no API key needed).
2. **Render**: the surface model becomes a mesh, drawn from an isometric camera (45° elevation, looking north-east).
   Roofs get the aerial photo, walls get tuffeau stone with windows, rivers get flat blue.
3. **Redraw**: each 1024 px tile (300 m) is redrawn as pixel art by FLUX.2 [klein] 9B (fal.ai).
   Tiles are drawn in diagonal waves; finished neighbours are pasted into each tile's margin so the
   model continues them, then colours are matched and edges dissolved, so seams disappear.
4. **View**: `web/` is a dependency-free canvas viewer (pan/zoom, tilt-shift, minimap, places, address search via the BAN).

## Run it

```sh
uv sync                                        # Python 3.13 (uv-managed)
$EDITOR .env                                   # add your fal.ai key as FAL_KEY (gitignored)
uv run python -m tinynantes.build --area       # download, render, redraw (~115 tiles, ~$3)
uv run python -m tinynantes.publish            # build web/data/
cd web && python3 -m http.server 8765          # open http://localhost:8765
```

Single tiles: `uv run python -m tinynantes.render I J`, `uv run python -m tinynantes.build I0 J0 I1 J1`.

## Credits

Data © IGN – Géoplateforme (Licence Ouverte Etalab 2.0). Addresses: Base Adresse Nationale.
Place positions checked against © OpenStreetMap contributors.
