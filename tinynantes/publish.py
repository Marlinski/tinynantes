"""Turn out/h0/art/*.png into the web tile pyramid under web/data/h0/.

Level 0 is the full-resolution 1024 px tiles; level L merges 2^L x 2^L tiles into one 1024 px image.
Also writes manifest.json (tile list, grid, projection) and places.json (landmarks in screen metres).

Usage: uv run python -m tinynantes.publish
"""
from __future__ import annotations

import json
import math
from pathlib import Path

from PIL import Image

from . import geo

ROOT = Path(__file__).resolve().parent.parent
ART = ROOT / "out" / "h0" / "art"
WEB = ROOT / "web" / "data" / "h0"
N = geo.TILE_PX

# name, lon, lat
PLACES = [
    ("Château des ducs de Bretagne", -1.5497, 47.2160),
    ("Cathédrale Saint-Pierre-et-Saint-Paul", -1.5508, 47.2183),
    ("Place Graslin", -1.5626, 47.2132),
    ("Théâtre Graslin", -1.5620, 47.2135),
    ("Passage Pommeraye", -1.5600, 47.2132),
    ("Place Royale", -1.5590, 47.2150),
    ("Bouffay", -1.5530, 47.2147),
    ("Les Machines de l'île", -1.5640, 47.2065),
    ("Hangar à Bananes", -1.5730, 47.2004),
    ("Les Anneaux de Buren", -1.5737, 47.2000),
    ("Tour Bretagne", -1.5583, 47.2180),
    ("Gare de Nantes", -1.5418, 47.2171),
    ("Jardin des Plantes", -1.5425, 47.2195),
    ("Lieu Unique", -1.5458, 47.2155),
    ("Cours Cambronne", -1.5655, 47.2115),
    ("Place du Commerce", -1.5570, 47.2128),
    ("Île Feydeau", -1.5555, 47.2120),
    ("Cité des Congrès", -1.5447, 47.2130),
    ("Palais de Justice", -1.5660, 47.2063),
    ("Place du Bouffay", -1.5535, 47.2140),
    ("Médiathèque Jacques Demy", -1.5650, 47.2110),
    ("Mémorial de l'abolition de l'esclavage", -1.5620, 47.2090),
    ("Pont Anne-de-Bretagne", -1.5655, 47.2075),
    ("Grue Titan jaune", -1.5715, 47.2015),
    ("CHU Hôtel-Dieu", -1.5545, 47.2105),
]


def places():
    from pyproj import Transformer
    t = Transformer.from_crs(4326, 2154, always_xy=True)
    v = geo.View(0)
    out = []
    for name, lon, lat in PLACES:
        x, y = t.transform(lon, lat)
        sx, sy, _ = v.to_screen(x - geo.ORIGIN[0], y - geo.ORIGIN[1], 10.0)
        out.append({"name": name, "sx": round(sx, 1), "sy": round(sy, 1)})
    return out


def main():
    tiles = {}
    for f in ART.glob("*.png"):
        parts = f.stem.split("_")          # "I_J" only; skip experiment files like "I_J_9b"
        if len(parts) == 2:
            tiles[(int(parts[0]), int(parts[1]))] = f
    if not tiles:
        raise SystemExit("no art tiles")
    is_ = [k[0] for k in tiles]; js = [k[1] for k in tiles]
    i0, i1, j0, j1 = min(is_), max(is_), min(js), max(js)
    span = max(i1 - i0 + 1, j1 - j0 + 1)
    levels = max(1, math.ceil(math.log2(span)) + 1)
    WEB.mkdir(parents=True, exist_ok=True)

    cur = {}
    per_level = {}
    d = WEB / "0"; d.mkdir(exist_ok=True)
    for (i, j), f in tiles.items():
        im = Image.open(f).convert("RGB")
        cur[(i, j)] = im
        im.save(d / f"{i}_{j}.webp", quality=88, method=6)
    for L in range(1, levels):
        nxt = {}
        d = WEB / str(L); d.mkdir(exist_ok=True)
        keys = {(math.floor(i / 2), math.floor(j / 2)) for i, j in cur}
        for I, J in keys:
            canvas = Image.new("RGB", (2 * N, 2 * N), (31, 36, 40))
            for di in (0, 1):
                for dj in (0, 1):
                    im = cur.get((2 * I + di, 2 * J + dj))
                    if im is not None:
                        canvas.paste(im, (di * N, dj * N))
            im = canvas.resize((N, N), Image.LANCZOS)
            nxt[(I, J)] = im
            im.save(d / f"{I}_{J}.webp", quality=85, method=6)
        per_level[L] = sorted(f"{I}_{J}" for I, J in nxt)
        cur = nxt

    # overview for the minimap
    W = (i1 - i0 + 1); H = (j1 - j0 + 1)
    s = 256 / max(W, H)
    ov = Image.new("RGB", (round(W * s), round(H * s)), (31, 36, 40))
    for (i, j), f in tiles.items():
        ov.paste(Image.open(f).convert("RGB").resize((math.ceil(s), math.ceil(s)), Image.LANCZOS),
                 (round((i - i0) * s), round((j - j0) * s)))
    ov.save(WEB / "overview.webp", quality=85)

    manifest = {
        "heading": 0, "tile_px": N, "tile_m": geo.TILE_M, "levels": levels,
        "bounds": [i0, j0, i1, j1], "tiles": sorted(f"{i}_{j}" for i, j in tiles), "level_tiles": per_level,
        "origin_l93": geo.ORIGIN, "axes": [list(a) for a in geo.View(0).axes],
    }
    (WEB / "manifest.json").write_text(json.dumps(manifest))
    (WEB.parent / "places.json").write_text(json.dumps(places(), ensure_ascii=False))
    print(f"published {len(tiles)} tiles, {levels} levels")


if __name__ == "__main__":
    main()
