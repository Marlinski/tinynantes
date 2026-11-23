"""Turn out/hH/art/*.png into the web tile pyramids under web/data/hH/ (every heading that has art).

Level 0 is the full-resolution 1024 px tiles; level L merges 2^L x 2^L tiles into one 1024 px image.
Also writes hH/manifest.json (tile lists, projection axes), headings.json and places.json (Lambert-93).

Usage: uv run python -m tinynantes.publish
"""
from __future__ import annotations

import json
import shutil
import math
from pathlib import Path

from PIL import Image

from . import geo

ROOT = Path(__file__).resolve().parent.parent
DATA = ROOT / "web" / "data"
N = geo.TILE_PX

# name, lon, lat (checked against OpenStreetMap / Nominatim, 2026-11)
PLACES = [
    ("Château des ducs de Bretagne", -1.54931, 47.21612),
    ("Cathédrale Saint-Pierre-et-Saint-Paul", -1.55018, 47.21837),
    ("Place Graslin", -1.56224, 47.21306),
    ("Théâtre Graslin", -1.56222, 47.21365),
    ("Passage Pommeraye", -1.55944, 47.21346),
    ("Place Royale", -1.55872, 47.2145),
    ("Bouffay", -1.55345, 47.21495),
    ("Les Machines de l'île", -1.56434, 47.206),
    ("Hangar à Bananes", -1.57285, 47.19991),
    ("Les Anneaux de Buren", -1.5737, 47.2000),
    ("Tour Bretagne", -1.55823, 47.2176),
    ("Gare de Nantes", -1.5418, 47.2171),
    ("Jardin des Plantes", -1.54295, 47.21977),
    ("Lieu Unique", -1.54581, 47.21576),
    ("Cours Cambronne", -1.56315, 47.21195),
    ("Place du Commerce", -1.55785, 47.21335),
    ("Île Feydeau", -1.5555, 47.2120),
    ("Cité des Congrès", -1.54502, 47.21371),
    ("Palais de Justice", -1.5660, 47.2063),
    ("Place du Bouffay", -1.55342, 47.21477),
    ("Médiathèque Jacques Demy", -1.56218, 47.21119),
    ("Mémorial de l'abolition de l'esclavage", -1.56453, 47.20948),
    ("Pont Anne-de-Bretagne", -1.5661, 47.20818),
    ("Grue Titan jaune", -1.5715, 47.2015),
    ("CHU Hôtel-Dieu", -1.5545, 47.2105),
]


def places():
    from pyproj import Transformer
    t = Transformer.from_crs(4326, 2154, always_xy=True)
    out = []
    for name, lon, lat in PLACES:
        x, y = t.transform(lon, lat)
        out.append({"name": name, "x": round(x, 1), "y": round(y, 1)})
    return out


def main():
    import sys
    only = None
    if "--headings" in sys.argv:            # e.g. --headings 0  or  --headings 0,2
        only = {int(x) for x in sys.argv[sys.argv.index("--headings") + 1].split(",")}
    heads = []
    for h in range(8):
        if only is not None and h not in only:
            shutil.rmtree(DATA / f"h{h}", ignore_errors=True)
            continue
        art = ROOT / "out" / f"h{h}" / "art"
        tiles = geo.View(h).tiles_for_area()
        done = sum((art / f"{i}_{j}.png").exists() for i, j in tiles)
        if done < len(tiles):   # a half-drawn heading would show holes: skip it until complete
            if done:
                print(f"h{h}: {done}/{len(tiles)} tiles drawn, not published")
            shutil.rmtree(DATA / f"h{h}", ignore_errors=True)
            continue
        publish_heading(h, art, DATA / f"h{h}")
        heads.append(h)
    (DATA / "headings.json").write_text(json.dumps(heads))
    (DATA / "places.json").write_text(json.dumps(places(), ensure_ascii=False))


def publish_heading(h, ART, WEB):
    tiles = {}
    for f in ART.glob("*.png"):
        parts = f.stem.split("_")          # "I_J" only; skip experiment files like "I_J_9b"
        if len(parts) == 2:
            tiles[(int(parts[0]), int(parts[1]))] = f
    if not tiles:
        return
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
        w = d / f"{i}_{j}.webp"
        if not w.exists() or w.stat().st_mtime < f.stat().st_mtime:
            im.save(w, quality=88, method=6)
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
        "heading": h, "tile_px": N, "tile_m": geo.TILE_M, "levels": levels,
        "bounds": [i0, j0, i1, j1], "tiles": sorted(f"{i}_{j}" for i, j in tiles), "level_tiles": per_level,
        "origin_l93": geo.ORIGIN, "axes": [list(a) for a in geo.View(h).axes],
    }
    (WEB / "manifest.json").write_text(json.dumps(manifest))
    print(f"h{h}: published {len(tiles)} tiles, {levels} levels", flush=True)


if __name__ == "__main__":
    main()
