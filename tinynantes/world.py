"""The living layer's static world: streets, tram tracks, transit routes and points of interest, in Lambert-93,
with terrain heights so the viewer can project them exactly like the tiles.

Sources: OpenStreetMap (Geofabrik Pays de la Loire extract, ODbL), Naolib GTFS (Nantes Métropole, ODbL),
IGN LiDAR HD terrain.

Steps (each cached under cache/live/):
  osm       extract roads, rail/tram lines and POIs of the map area from the .osm.pbf
  transit   snap every GTFS journey pattern (stop-to-stop straight lines) onto the real network:
            trams onto OSM tram tracks, buses onto drivable streets (shortest path between consecutive stops)
  export    web/data/world/*.json + terrain.png (the viewer's copy)

Usage: uv run python -m tinynantes.world [osm|transit|export|all]
"""
from __future__ import annotations

import csv
import io
import json
import math
import sys
import zipfile
from collections import defaultdict
from pathlib import Path

import numpy as np
import requests
from PIL import Image
from pyproj import Transformer

from . import geo, ign

ROOT = Path(__file__).resolve().parent.parent
LIVE = ROOT / "cache" / "live"
WEB = ROOT / "web" / "data" / "world"
MARGIN = 1500.0
GTFS_URL = ("https://data.nantesmetropole.fr/api/explore/v2.1/catalog/datasets/"
            "244400404_transports_commun_naolib_nantes_metropole_gtfs/records?limit=1")
PBF_URL = "https://download.geofabrik.de/europe/france/pays-de-la-loire-latest.osm.pbf"

_to_l93 = Transformer.from_crs("EPSG:4326", "EPSG:2154", always_xy=True)
_to_wgs = Transformer.from_crs("EPSG:2154", "EPSG:4326", always_xy=True)


def area(margin=MARGIN):
    x0, y0, x1, y1 = geo.AREA
    return x0 - margin, y0 - margin, x1 + margin, y1 + margin


# ---------------------------------------------------------------- OSM

DRIVE = {"motorway", "trunk", "primary", "secondary", "tertiary", "unclassified", "residential", "living_street",
         "service", "motorway_link", "trunk_link", "primary_link", "secondary_link", "tertiary_link", "busway"}
WALK = DRIVE - {"motorway", "motorway_link", "trunk", "trunk_link", "busway"} | {
    "pedestrian", "footway", "path", "steps", "cycleway", "track", "corridor"}
POI_KEYS = {
    "amenity": {"restaurant", "bar", "cafe", "pub", "fast_food", "cinema", "theatre", "nightclub", "arts_centre",
                "ice_cream", "biergarten", "marketplace"},
    "tourism": {"museum", "attraction", "gallery", "viewpoint", "hotel"},
    "shop": {"bakery", "books", "deli", "wine", "cheese", "chocolate", "confectionery"},
    "leisure": {"stadium", "sports_centre"},
}
POI_TAGS = ("name", "cuisine", "website", "contact:website", "opening_hours", "phone", "contact:phone",
            "addr:housenumber", "addr:street", "wheelchair", "diet:vegetarian", "takeaway", "outdoor_seating",
            "brand", "operator", "wikidata", "screen")


def osm():
    import osmium
    pbf = LIVE / "pdl.osm.pbf"
    if not pbf.exists():
        LIVE.mkdir(parents=True, exist_ok=True)
        with requests.get(PBF_URL, stream=True, timeout=600) as r:
            r.raise_for_status()
            with open(pbf, "wb") as f:
                for b in r.iter_content(1 << 20):
                    f.write(b)
    x0, y0, x1, y1 = area()
    (lo0, la0), (lo1, la1) = _to_wgs.transform(x0, y0), _to_wgs.transform(x1, y1)

    def inside(lon, lat):
        return lo0 <= lon <= lo1 and la0 <= lat <= la1

    roads, rails, pois = [], [], []

    def poi_kind(tags):
        for k, vals in POI_KEYS.items():
            v = tags.get(k)
            if v in vals:
                return f"{k}={v}"
        return None

    def keep_tags(tags):
        return {k: tags[k] for k in POI_TAGS if k in tags}

    class H(osmium.SimpleHandler):
        def node(self, n):
            if not n.tags or not n.location.valid() or not inside(n.location.lon, n.location.lat):
                return
            t = dict(n.tags)
            k = poi_kind(t)
            if k and "name" in t:
                pois.append({"id": f"n{n.id}", "kind": k, "lon": n.location.lon, "lat": n.location.lat,
                             "tags": keep_tags(t)})

        def way(self, w):
            t = dict(w.tags)
            hw, rw = t.get("highway"), t.get("railway")
            k = poi_kind(t)
            if not (hw or rw in ("tram", "rail", "light_rail") or (k and "name" in t)):
                return
            try:
                pts = [(nd.lon, nd.lat) for nd in w.nodes]
                ids = [nd.ref for nd in w.nodes]
            except osmium.InvalidLocationError:
                return
            if not pts or not any(inside(*p) for p in pts):
                return
            if k and "name" in t:
                lon = sum(p[0] for p in pts) / len(pts); lat = sum(p[1] for p in pts) / len(pts)
                pois.append({"id": f"w{w.id}", "kind": k, "lon": lon, "lat": lat, "tags": keep_tags(t)})
            rec = {"id": w.id, "nodes": ids, "pts": pts,
                   "layer": int(t.get("layer", "0")) if t.get("layer", "0").lstrip("-").isdigit() else 0,
                   "bridge": t.get("bridge", "no") != "no", "tunnel": t.get("tunnel", "no") != "no",
                   "name": t.get("name", "")}
            if hw:
                rec.update(hw=hw, oneway=t.get("oneway") in ("yes", "1", "true") or hw in ("motorway",) or
                           t.get("junction") == "roundabout", psv=t.get("psv") in ("yes", "designated") or
                           t.get("bus") in ("yes", "designated") or hw == "busway",
                           lanes=t.get("lanes"), sidewalk=t.get("sidewalk"), access=t.get("access"))
                roads.append(rec)
            elif rw:
                rec.update(rw=rw, service=t.get("service"))
                rails.append(rec)

    H().apply_file(str(pbf), locations=True, idx="flex_mem")
    for coll in (roads, rails):
        for r in coll:
            xs, ys = _to_l93.transform([p[0] for p in r["pts"]], [p[1] for p in r["pts"]])
            r["xy"] = [[round(x, 2), round(y, 2)] for x, y in zip(xs, ys)]
            del r["pts"]
    for p in pois:
        p["x"], p["y"] = (round(v, 2) for v in _to_l93.transform(p["lon"], p["lat"]))
    out = {"roads": roads, "rails": rails, "pois": pois}
    (LIVE / "osm_area.json").write_text(json.dumps(out))
    print(f"osm: {len(roads)} roads, {len(rails)} rail/tram ways, {len(pois)} POIs", flush=True)
    return out


def load_osm():
    return json.loads((LIVE / "osm_area.json").read_text())


# ---------------------------------------------------------------- terrain

def terrain(res=4.0):
    """Terrain height grid over the area for the viewer: 16-bit decimetres (+100 m) packed in R/G."""
    x0, y0, x1, y1 = area(300)
    mnt, (gx0, gy1), r = ign.mosaic("mnt", (x0, y0, x1, y1))
    k = int(round(res / r))
    rows0 = int((gy1 - y1) / r); cols0 = int((x0 - gx0) / r)
    nh, nw = int((y1 - y0) / res), int((x1 - x0) / res)
    sub = mnt[rows0:rows0 + nh * k, cols0:cols0 + nw * k].reshape(nh, k, nw, k).mean(axis=(1, 3))
    v = np.clip(np.round((sub + 100.0) * 10), 0, 65535).astype("u2")
    rgb = np.stack([(v >> 8).astype("u1"), (v & 255).astype("u1"), np.zeros_like(v, "u1")], -1)
    WEB.mkdir(parents=True, exist_ok=True)
    Image.fromarray(rgb, "RGB").save(WEB / "terrain.png", optimize=True)
    meta = {"x0": x0, "y1": y1, "res": res, "w": nw, "h": nh}
    return meta


class Terrain:
    def __init__(self):
        x0, y0, x1, y1 = area(300)
        self.mnt, (self.gx0, self.gy1), self.res = ign.mosaic("mnt", (x0, y0, x1, y1))

    def z(self, xy):
        xy = np.asarray(xy, "f8").reshape(-1, 2)
        c = np.clip(((xy[:, 0] - self.gx0) / self.res).astype(int), 0, self.mnt.shape[1] - 1)
        r = np.clip(((self.gy1 - xy[:, 1]) / self.res).astype(int), 0, self.mnt.shape[0] - 1)
        return self.mnt[r, c]


# ---------------------------------------------------------------- graphs

class Graph:
    """Undirected-or-directed street graph on OSM nodes, with edge geometry."""

    def __init__(self, ways, directed=True, weight=lambda w: 1.0):
        self.xy = {}
        self.adj = defaultdict(list)       # node -> [(node, length, way index, forward)]
        for wi, w in enumerate(ways):
            ids, pts = w["nodes"], w["xy"]
            for k, nid in enumerate(ids):
                self.xy[nid] = pts[k]
            mul = weight(w)
            for k in range(len(ids) - 1):
                a, b = ids[k], ids[k + 1]
                L = math.dist(pts[k], pts[k + 1]) * mul
                self.adj[a].append((b, L))
                if not (directed and w.get("oneway")):
                    self.adj[b].append((a, L))
        self.ids = np.array(list(self.xy), "i8")
        self.pts = np.array([self.xy[i] for i in self.ids], "f8")
        from scipy.spatial import cKDTree
        self.tree = cKDTree(self.pts)

    def nearest(self, x, y, k=6):
        d, idx = self.tree.query([x, y], k=k)
        return [int(self.ids[i]) for i in np.atleast_1d(idx)], np.atleast_1d(d)

    def path(self, a, b, limit):
        """A* from node a to b; None if longer than limit metres."""
        import heapq
        tx, ty = self.xy[b]
        h = lambda n: math.dist(self.xy[n], (tx, ty))
        best = {a: 0.0}
        prev = {}
        q = [(h(a), 0.0, a)]
        while q:
            f, g, n = heapq.heappop(q)
            if n == b:
                out = [n]
                while n in prev:
                    n = prev[n]; out.append(n)
                return out[::-1], g
            if g > best.get(n, 1e18) or f > limit:
                continue
            for m, L in self.adj[n]:
                g2 = g + L
                if g2 < best.get(m, 1e18):
                    best[m] = g2; prev[m] = n
                    heapq.heappush(q, (g2 + h(m), g2, m))
        return None


# ---------------------------------------------------------------- GTFS

def gtfs_dir():
    d = LIVE / "gtfs"
    if not (d / "stop_times.txt").exists():
        meta = requests.get(GTFS_URL, timeout=60).json()["results"][0]["fichier"]["url"]
        data = requests.get(meta, timeout=600).content
        zipfile.ZipFile(io.BytesIO(data)).extractall(d)
    return d


def _read(name):
    with open(gtfs_dir() / name, newline="", encoding="utf-8-sig") as f:
        yield from csv.DictReader(f)


def transit():
    """Snap GTFS journey patterns onto the real network. -> cache/live/transit_shapes.json:
    {shape_id: {"xy": [[x, y], ...], "stops": [distance along xy of each shape point / stop]}}"""
    o = load_osm()
    x0, y0, x1, y1 = area()
    routes = {r["route_id"]: r for r in _read("routes.txt")}
    shape_route = {}
    for t in _read("trips.txt"):
        shape_route.setdefault(t["shape_id"], t["route_id"])
    shapes = defaultdict(list)
    for r in _read("shapes.txt"):
        shapes[r["shape_id"]].append((int(r["shape_pt_sequence"]), float(r["shape_pt_lon"]), float(r["shape_pt_lat"])))
    tram_g = Graph([w for w in o["rails"] if w["rw"] == "tram"], directed=False)
    bus_g = Graph([w for w in o["roads"] if w["hw"] in DRIVE and w.get("access") not in ("no", "private")],
                  weight=lambda w: 0.7 if w.get("psv") else (1.6 if w["hw"] in ("service",) else 1.0))
    out, stats = {}, defaultdict(int)
    for sid, pts in shapes.items():
        pts.sort()
        lon = [p[1] for p in pts]; lat = [p[2] for p in pts]
        xs, ys = _to_l93.transform(lon, lat)
        P = list(zip(xs, ys))
        if not any(x0 <= x <= x1 and y0 <= y <= y1 for x, y in P):
            continue
        rt = routes[shape_route.get(sid, "")]["route_type"] if shape_route.get(sid) in routes else "3"
        g = tram_g if rt == "0" else (bus_g if rt == "3" else None)
        xy, stops = [list(P[0])], [0.0]
        for a, b in zip(P, P[1:]):
            seg = None
            if g is not None and math.dist(a, b) < 3000:
                seg = _snap_segment(g, a, b)
            if seg is None:
                seg = [list(b)]; stats["straight"] += 1
            else:
                stats["snapped"] += 1
            for q in seg:
                if math.dist(q, xy[-1]) > 0.05:
                    xy.append(list(q))
            stops.append(_cumlen(xy)[-1])
        out[sid] = {"route": shape_route.get(sid), "xy": [[round(x, 2), round(y, 2)] for x, y in xy], "stops": [round(s, 2) for s in stops]}
    (LIVE / "transit_shapes.json").write_text(json.dumps(out))
    print(f"transit: {len(out)} patterns in the area, {dict(stats)} stop-to-stop segments", flush=True)


def _cumlen(xy):
    a = np.asarray(xy, "f8")
    return np.r_[0, np.cumsum(np.hypot(*np.diff(a, axis=0).T))] if len(a) > 1 else np.zeros(1)


def _snap_segment(g, a, b):
    """Network path from stop a to stop b (both off-network points), as a polyline ending at b's projection."""
    na, da = g.nearest(*a, k=4)
    nb, db = g.nearest(*b, k=4)
    if da[0] > 150 or db[0] > 150:
        return None
    straight = math.dist(a, b)
    best = None
    for s in na[:3]:
        for t in nb[:3]:
            if s == t:
                continue
            r = g.path(s, t, limit=straight * 3 + 300)
            if r and (best is None or r[1] < best[1]):
                best = r
    if best is None:
        return None
    return [g.xy[n] for n in best[0]] + [list(b)]


# ---------------------------------------------------------------- export for the viewer

def export():
    """web/data/world/: roads.json (drivable + walkable network), transit.json (routes, snapped shapes, stops),
    pois.json, terrain.png + world.json (terrain grid metadata). Coordinates are Lambert-93 relative to geo.ORIGIN,
    rounded to 0.1 m, with terrain z."""
    WEB.mkdir(parents=True, exist_ok=True)
    o = load_osm()
    T = Terrain()
    ox, oy = geo.ORIGIN

    def rel(xy, z=None):
        a = np.asarray(xy, "f8")
        zz = T.z(a) if z is None else z
        return [[round(x - ox, 1), round(y - oy, 1), round(float(h), 1)] for (x, y), h in zip(a, zz)]

    roads = []
    for w in o["roads"]:
        if w["tunnel"] or w["hw"] not in DRIVE | WALK:
            continue
        roads.append({"k": w["hw"], "o": int(bool(w.get("oneway"))), "n": w["name"], "b": int(w["bridge"]),
                      "id": w["nodes"], "p": rel(w["xy"])})
    (WEB / "roads.json").write_text(json.dumps({"roads": roads}, separators=(",", ":")))

    shapes = json.loads((LIVE / "transit_shapes.json").read_text())
    routes = {r["route_id"]: {"id": r["route_id"], "short": r["route_short_name"], "name": r["route_long_name"],
                              "type": int(r["route_type"]), "color": "#" + (r["route_color"] or "888888"),
                              "text": "#" + (r["route_text_color"] or "ffffff")} for r in _read("routes.txt")}
    stops = []
    for s in _read("stops.txt"):
        if s["location_type"] == "1":
            x, y = _to_l93.transform(float(s["stop_lon"]), float(s["stop_lat"]))
            if area()[0] <= x <= area()[2] and area()[1] <= y <= area()[3]:
                stops.append({"id": s["stop_id"], "name": s["stop_name"], "p": rel([[x, y]])[0]})
    used = {v["route"] for v in shapes.values()}
    tr = {"routes": [r for k, r in routes.items() if k in used],
          "shapes": {k: {"r": v["route"], "p": rel(v["xy"]), "s": v["stops"]} for k, v in shapes.items()},
          "stations": stops}
    (WEB / "transit.json").write_text(json.dumps(tr, separators=(",", ":")))

    pois = [{"id": p["id"], "k": p["kind"], "t": p["tags"], "p": rel([[p["x"], p["y"]]])[0]} for p in o["pois"]]
    (WEB / "pois.json").write_text(json.dumps(pois, separators=(",", ":"), ensure_ascii=False))
    meta = terrain()
    meta.update(origin=geo.ORIGIN)
    (WEB / "world.json").write_text(json.dumps(meta))
    for f in sorted(WEB.iterdir()):
        print(f"{f.name}: {f.stat().st_size / 1e6:.1f} MB")


def main(argv):
    what = argv[0] if argv else "all"
    if what in ("osm", "all"):
        osm()
    if what in ("transit", "all"):
        transit()
    if what in ("export", "all"):
        export()


if __name__ == "__main__":
    main(sys.argv[1:])
