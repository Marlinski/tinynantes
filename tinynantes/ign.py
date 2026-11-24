"""Fetch and cache IGN Géoplateforme open data in fixed 500 m chunks (Lambert-93).

- MNS / MNT: LiDAR HD surface and terrain models, 0.5 m, float32 (WMS-R BIL)
- ortho:     BD ORTHO 20 cm (WMS-R JPEG)
- buildings: BD TOPO V3 'batiment' footprints rasterised to a 0.5 m mask (WFS)
"""
from __future__ import annotations

import json
import os
import math
import time
from pathlib import Path

import numpy as np
import requests
from PIL import Image, ImageDraw

CACHE = Path(__file__).resolve().parent.parent / "cache" / "ign"
CHUNK = 500.0
DEM_RES = 0.5
ORTHO_RES = 0.2
WMS = "https://data.geopf.fr/wms-r/wms"
WFS = "https://data.geopf.fr/wfs/ows"
LAYERS = {
    "mns": "IGNF_LIDAR-HD_MNS_ELEVATION.ELEVATIONGRIDCOVERAGE.LAMB93",
    "mnt": "IGNF_LIDAR-HD_MNT_ELEVATION.ELEVATIONGRIDCOVERAGE.LAMB93",
    "ortho": "HR.ORTHOIMAGERY.ORTHOPHOTOS",
    "cls": "IGNF_LIDAR-HD_CLASS",
}
# LiDAR HD classification map colours -> class codes
CLS_COLORS = {
    (207, 150, 70): 2,    # ground
    (90, 110, 40): 3,     # low vegetation
    (70, 150, 60): 5,     # medium/high vegetation
    (180, 20, 20): 6,     # building
    (40, 140, 140): 9,    # water
    (255, 0, 0): 17,      # bridge deck
    (255, 0, 255): 64,    # permanent above-ground
}
_session = requests.Session()
_session.headers["User-Agent"] = "tinynantes-map/0.1"


def _get(url, params, tries=5):
    for k in range(tries):
        try:
            # hard deadline for the whole body: a throttled server can trickle bytes forever
            t0 = time.monotonic()
            with requests.get(url, params=params, timeout=(15, 30), stream=True,
                              headers=_session.headers) as r:
                body = bytearray()
                for part in r.iter_content(65536):
                    body += part
                    if time.monotonic() - t0 > 90:
                        raise requests.Timeout("body took > 90 s")
                ct = r.headers.get("content-type", "")
                if r.status_code == 200 and not ct.startswith(("text/xml", "application/vnd.ogc.se")):
                    return bytes(body)
                err = f"{r.status_code} {bytes(body[:200])!r}"
        except requests.RequestException as e:
            err = str(e)
        time.sleep(2 * (k + 1))
    raise RuntimeError(f"IGN request failed: {err}")


def _wms(layer, bbox, w, h, fmt):
    return _get(WMS, {
        "SERVICE": "WMS", "VERSION": "1.3.0", "REQUEST": "GetMap", "CRS": "EPSG:2154",
        "BBOX": ",".join(f"{v:.2f}" for v in bbox), "LAYERS": layer, "STYLES": "",
        "FORMAT": fmt, "WIDTH": w, "HEIGHT": h,
    })


def _atomic(f, write):
    """Write a cache file under a temporary name, then rename it: several processes (renders, world build,
    painting) read the same chunks, and must never see one half written."""
    tmp = f.with_name(f".{f.stem}.{os.getpid()}.tmp{f.suffix}")
    write(tmp)
    os.replace(tmp, f)


def chunk(kind: str, cx: int, cy: int):
    """Return one chunk as a numpy array (row 0 = north)."""
    d = CACHE / kind
    d.mkdir(parents=True, exist_ok=True)
    x0, y0 = cx * CHUNK, cy * CHUNK
    bbox = (x0, y0, x0 + CHUNK, y0 + CHUNK)
    if kind in ("mns", "mnt"):
        f = d / f"{cx}_{cy}.npy"
        if not f.exists():
            n = int(CHUNK / DEM_RES)
            a = np.frombuffer(_wms(LAYERS[kind], bbox, n, n, "image/x-bil;bits=32"), "<f4").reshape(n, n).copy()
            _atomic(f, lambda t: np.save(t, a))
        return np.load(f)
    if kind == "ortho":
        f = d / f"{cx}_{cy}.jpg"
        if not f.exists():
            n = int(CHUNK / ORTHO_RES)
            data = _wms(LAYERS[kind], bbox, n, n, "image/jpeg")
            _atomic(f, lambda t: t.write_bytes(data))
        return np.asarray(Image.open(f).convert("RGB"))
    if kind == "cls":
        f = d / f"{cx}_{cy}.png"
        if not f.exists():
            n = int(CHUNK / DEM_RES)
            rgb = np.asarray(Image.open(__import__("io").BytesIO(_wms(LAYERS[kind], bbox, n, n, "image/png"))).convert("RGB"))
            code = np.zeros(rgb.shape[:2], "u1")
            for c, v in CLS_COLORS.items():
                code[(rgb == c).all(-1)] = v
            _atomic(f, lambda t: Image.fromarray(code).save(t))
        return np.asarray(Image.open(f))
    if kind in WFS_LAYERS:
        f = d / f"{cx}_{cy}.png"
        if not f.exists():
            m = _polygon_mask(bbox, WFS_LAYERS[kind])
            _atomic(f, lambda t: m.save(t))
        return np.asarray(Image.open(f)) > 0
    raise ValueError(kind)


WFS_LAYERS = {"bati": "BDTOPO_V3:batiment", "eau": "BDTOPO_V3:surface_hydrographique"}


def _polygon_mask(bbox, typename):
    x0, y0, x1, y1 = bbox
    n = int(CHUNK / DEM_RES)
    feats, start, tries = [], 0, 0
    while True:
        raw = _get(WFS, {
            "SERVICE": "WFS", "VERSION": "2.0.0", "REQUEST": "GetFeature",
            "TYPENAMES": typename, "SRSNAME": "EPSG:2154",
            "BBOX": f"{x0},{y0},{x1},{y1},EPSG:2154", "OUTPUTFORMAT": "application/json",
            "COUNT": 5000, "STARTINDEX": start,
        })
        try:
            page = json.loads(raw)["features"]
        except (ValueError, KeyError):         # the WFS sometimes answers an empty or HTML body under load
            tries += 1
            if tries > 5:
                raise
            time.sleep(10 * tries)
            continue
        feats += page
        if len(page) < 5000:
            break
        start += 5000
    img = Image.new("L", (n, n), 0)
    dr = ImageDraw.Draw(img)

    def px(c):
        return ((c[0] - x0) / DEM_RES, (y1 - c[1]) / DEM_RES)

    for f in feats:
        g = f.get("geometry") or {}
        polys = g["coordinates"] if g.get("type") == "MultiPolygon" else [g.get("coordinates") or []]
        for poly in polys:
            for k, ring in enumerate(poly):
                dr.polygon([px(c) for c in ring], fill=255 if k == 0 else 0)
    return img


def mosaic(kind: str, bbox):
    """Assemble chunks covering bbox (x0,y0,x1,y1). Returns (array, (x0,y1) of top-left, res)."""
    res = ORTHO_RES if kind == "ortho" else DEM_RES
    cx0, cy0 = math.floor(bbox[0] / CHUNK), math.floor(bbox[1] / CHUNK)
    cx1, cy1 = math.floor((bbox[2] - 1e-6) / CHUNK), math.floor((bbox[3] - 1e-6) / CHUNK)
    n = int(CHUNK / res)
    rows = []
    for cy in range(cy1, cy0 - 1, -1):          # north first
        rows.append(np.concatenate([chunk(kind, cx, cy) for cx in range(cx0, cx1 + 1)], axis=1))
    a = np.concatenate(rows, axis=0)
    return a, (cx0 * CHUNK, (cy1 + 1) * CHUNK), res


def prefetch(bboxes, kinds=("mns", "mnt", "cls", "bati", "eau", "ortho"), workers=4):
    """Download every chunk the bboxes need, in parallel."""
    from concurrent.futures import ThreadPoolExecutor
    keys = set()
    for b in bboxes:
        for cx in range(math.floor(b[0] / CHUNK), math.floor((b[2] - 1e-6) / CHUNK) + 1):
            for cy in range(math.floor(b[1] / CHUNK), math.floor((b[3] - 1e-6) / CHUNK) + 1):
                keys.update((k, cx, cy) for k in kinds)
    keys = sorted(keys)
    chunks = sorted({(cx, cy) for _, cx, cy in keys})
    done = 0
    from concurrent.futures import as_completed
    with ThreadPoolExecutor(workers) as ex:
        futs = [ex.submit(chunk, *k) for k in keys] + [ex.submit(_rail_chunk, *c) for c in chunks]
        for _ in as_completed(futs):
            done += 1
            if done % 25 == 0 or done == len(futs):
                print(f"ign {done}/{len(futs)}", flush=True)


def _rail_chunk(cx, cy):
    """BD TOPO rail segments (tramway, railway) touching one chunk, cached as JSON."""
    d = CACHE / "rail"
    d.mkdir(parents=True, exist_ok=True)
    f = d / f"{cx}_{cy}.json"
    if not f.exists():
        x0, y0 = cx * CHUNK, cy * CHUNK
        raw = _get(WFS, {
            "SERVICE": "WFS", "VERSION": "2.0.0", "REQUEST": "GetFeature",
            "TYPENAMES": "BDTOPO_V3:troncon_de_voie_ferree", "SRSNAME": "EPSG:2154",
            "BBOX": f"{x0},{y0},{x0 + CHUNK},{y0 + CHUNK},EPSG:2154", "OUTPUTFORMAT": "application/json",
            "COUNT": 5000,
        })
        out = []
        for ft in json.loads(raw)["features"]:
            p, g = ft["properties"], ft.get("geometry") or {}
            lines = g["coordinates"] if g.get("type") == "MultiLineString" else [g.get("coordinates") or []]
            for line in lines:
                out.append({"id": p.get("cleabs"), "nature": p.get("nature"), "tracks": p.get("nombre_de_voies") or 1,
                            "level": str(p.get("position_par_rapport_au_sol") or "0"),
                            "xy": [[round(c[0], 2), round(c[1], 2)] for c in line]})
        _atomic(f, lambda t: t.write_text(json.dumps(out)))
    return json.loads(f.read_text())


def rails(bbox):
    """Rail centrelines (deduplicated) for every chunk under bbox."""
    seen, out = set(), []
    for cx in range(math.floor(bbox[0] / CHUNK), math.floor((bbox[2] - 1e-6) / CHUNK) + 1):
        for cy in range(math.floor(bbox[1] / CHUNK), math.floor((bbox[3] - 1e-6) / CHUNK) + 1):
            for r in _rail_chunk(cx, cy):
                key = (r["id"], tuple(r["xy"][0]))
                if key not in seen:
                    seen.add(key); out.append(r)
    return out


def _wfs_all(typename, bbox):
    x0, y0, x1, y1 = bbox
    feats, start = [], 0
    while True:
        raw = _get(WFS, {
            "SERVICE": "WFS", "VERSION": "2.0.0", "REQUEST": "GetFeature",
            "TYPENAMES": typename, "SRSNAME": "EPSG:2154",
            "BBOX": f"{x0},{y0},{x1},{y1},EPSG:2154", "OUTPUTFORMAT": "application/json",
            "COUNT": 5000, "STARTINDEX": start,
        })
        page = json.loads(raw)["features"]
        feats += page
        if len(page) < 5000:
            return feats
        start += 5000


def _building_chunk(cx, cy):
    """BD TOPO building footprints touching one chunk (rings + useful attributes), cached as JSON."""
    d = CACHE / "bati_vec"
    d.mkdir(parents=True, exist_ok=True)
    f = d / f"{cx}_{cy}.json"
    if not f.exists():
        x0, y0 = cx * CHUNK, cy * CHUNK
        out = []
        for ft in _wfs_all("BDTOPO_V3:batiment", (x0, y0, x0 + CHUNK, y0 + CHUNK)):
            p, g = ft["properties"], ft.get("geometry") or {}
            polys = g["coordinates"] if g.get("type") == "MultiPolygon" else [g.get("coordinates") or []]
            for k, poly in enumerate(polys):
                out.append({
                    "id": f'{p.get("cleabs")}#{k}',
                    "rings": [[[round(c[0], 2), round(c[1], 2)] for c in ring] for ring in poly],
                    "floors": p.get("nombre_d_etages"), "height": p.get("hauteur"),
                    "roof": p.get("materiaux_de_la_toiture"), "walls": p.get("materiaux_des_murs"),
                    "usage": p.get("usage_1"), "nature": p.get("nature"),
                    "light": bool(p.get("construction_legere")), "year": (p.get("date_d_apparition") or "")[:4],
                })
        _atomic(f, lambda t: t.write_text(json.dumps(out)))
    return json.loads(f.read_text())


def buildings(bbox):
    """Building footprint polygons (deduplicated) for every chunk under bbox."""
    seen, out = set(), []
    for cx in range(math.floor(bbox[0] / CHUNK), math.floor((bbox[2] - 1e-6) / CHUNK) + 1):
        for cy in range(math.floor(bbox[1] / CHUNK), math.floor((bbox[3] - 1e-6) / CHUNK) + 1):
            for b in _building_chunk(cx, cy):
                if b["id"] not in seen:
                    seen.add(b["id"]); out.append(b)
    return out


def _sport_chunk(cx, cy):
    """BD TOPO sports grounds touching one chunk, cached as JSON."""
    d = CACHE / "sport"
    d.mkdir(parents=True, exist_ok=True)
    f = d / f"{cx}_{cy}.json"
    if not f.exists():
        x0, y0 = cx * CHUNK, cy * CHUNK
        out = []
        for ft in _wfs_all("BDTOPO_V3:terrain_de_sport", (x0, y0, x0 + CHUNK, y0 + CHUNK)):
            p, g = ft["properties"], ft.get("geometry") or {}
            polys = g["coordinates"] if g.get("type") == "MultiPolygon" else [g.get("coordinates") or []]
            for k, poly in enumerate(polys):
                out.append({"id": f'{p.get("cleabs")}#{k}', "nature": p.get("nature"),
                            "detail": p.get("nature_detaillee"),
                            "rings": [[[round(c[0], 2), round(c[1], 2)] for c in ring] for ring in poly]})
        _atomic(f, lambda t: t.write_text(json.dumps(out)))
    return json.loads(f.read_text())


def sports(bbox):
    seen, out = set(), []
    for cx in range(math.floor(bbox[0] / CHUNK), math.floor((bbox[2] - 1e-6) / CHUNK) + 1):
        for cy in range(math.floor(bbox[1] / CHUNK), math.floor((bbox[3] - 1e-6) / CHUNK) + 1):
            for s in _sport_chunk(cx, cy):
                if s["id"] not in seen:
                    seen.add(s["id"]); out.append(s)
    return out
