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
    # the bbox prefilter above spans every area: drop what only touches the unmapped corners (south Rezé...)
    near = lambda xy: any(geo.inside(x, y, MARGIN) for x, y in xy)
    roads = [r for r in roads if near(r["xy"])]
    rails = [r for r in rails if near(r["xy"])]
    pois = [p for p in pois if geo.inside(p["x"], p["y"])]
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
        if not any(geo.inside(x, y) for x, y in P):
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
            if geo.inside(x, y):
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


# ---------------------------------------------------------------- navigation raster for the game

NAV_RES, NAV_CHUNK = 1.0, 250.0
ROAD_W = {"motorway": 14, "trunk": 12, "primary": 11, "secondary": 9, "tertiary": 8, "unclassified": 6,
          "residential": 6.5, "living_street": 5, "service": 4, "busway": 6, "motorway_link": 7, "trunk_link": 7,
          "primary_link": 7, "secondary_link": 7, "tertiary_link": 6}
FOOT_W = {"pedestrian": 8, "footway": 3, "path": 2.5, "steps": 3, "cycleway": 3, "track": 3.5, "corridor": 3}
B_BUILDING, B_WATER, B_ROAD, B_FOOT = 1, 2, 4, 8


def nav():
    """web/data/world/nav/CX_CY.png: 250 m chunks at 1 m. Red = flags (1 building, 2 water, 4 drivable road,
    8 footway/pedestrian), green = deck height above the terrain on bridges (decimetres)."""
    from PIL import ImageDraw
    o = load_osm()
    out = WEB / "nav"
    out.mkdir(parents=True, exist_ok=True)
    x0, y0, x1, y1 = area(300)
    cxs = range(int(x0 // NAV_CHUNK), int(x1 // NAV_CHUNK) + 1)
    cys = range(int(y0 // NAV_CHUNK), int(y1 // NAV_CHUNK) + 1)
    bins = defaultdict(list)
    for w in o["roads"]:
        if w["tunnel"]:
            continue
        a = np.asarray(w["xy"])
        for cx in range(int((a[:, 0].min() - 10) // NAV_CHUNK), int((a[:, 0].max() + 10) // NAV_CHUNK) + 1):
            for cy in range(int((a[:, 1].min() - 10) // NAV_CHUNK), int((a[:, 1].max() + 10) // NAV_CHUNK) + 1):
                bins[(cx, cy)].append(w)
    n = int(NAV_CHUNK / NAV_RES)
    count = 0
    for cx in cxs:
        for cy in cys:
            bx0, by0 = cx * NAV_CHUNK, cy * NAV_CHUNK
            bbox = (bx0, by0, bx0 + NAV_CHUNK, by0 + NAV_CHUNK)
            if not geo.inside(bx0 + NAV_CHUNK / 2, by0 + NAV_CHUNK / 2, 300 + NAV_CHUNK / 2):
                continue                                   # unmapped corner of the bounding box
            if (out / f"{cx}_{cy}.png").exists():
                count += 1
                continue
            px = lambda pts: [((x - bx0) / NAV_RES, (by0 + NAV_CHUNK - y) / NAV_RES) for x, y in pts]
            flags = np.zeros((n, n), "u1")
            # water (BD TOPO hydrography raster, 0.5 m)
            eau, (gx0, gy1), r = ign.mosaic("eau", bbox)
            r0, c0 = int(round((gy1 - (by0 + NAV_CHUNK)) / r)), int(round((bx0 - gx0) / r))
            k = int(NAV_RES / r)
            sub = eau[r0:r0 + n * k, c0:c0 + n * k]
            if sub.shape == (n * k, n * k):
                flags |= (sub.reshape(n, k, n, k).max(axis=(1, 3)) > 0).astype("u1") * B_WATER
            # buildings
            im = Image.new("L", (n, n), 0); d = ImageDraw.Draw(im)
            for attempt in range(5):
                try:
                    blds = ign.buildings(bbox)
                    break
                except ValueError:          # the WFS sometimes answers with an error page; wait and retry
                    import time
                    time.sleep(20 * (attempt + 1))
            else:
                raise RuntimeError(f"BD TOPO buildings unavailable for {bbox}")
            for b in blds:
                if b.get("light"):
                    continue
                rings = b["rings"]
                d.polygon(px(rings[0]), fill=1)
                for hole in rings[1:]:
                    d.polygon(px(hole), fill=0)
            flags |= np.asarray(im, "u1") * B_BUILDING
            # streets (drawn after buildings: arcades and passages under buildings stay usable)
            road = Image.new("L", (n, n), 0); dr = ImageDraw.Draw(road)
            foot = Image.new("L", (n, n), 0); df = ImageDraw.Draw(foot)
            deck = Image.new("L", (n, n), 0); dd = ImageDraw.Draw(deck)
            bridge_ways = []
            for w in bins.get((cx, cy), []):
                pts = px(w["xy"])
                if w["hw"] in ROAD_W and w.get("access") not in ("no", "private"):
                    wd = max(2, round(ROAD_W[w["hw"]] / NAV_RES))
                    dr.line(pts, fill=1, width=wd, joint="curve")
                    df.line(pts, fill=1, width=wd + round(6 / NAV_RES), joint="curve")   # sidewalks
                elif w["hw"] in FOOT_W:
                    df.line(pts, fill=1, width=max(2, round(FOOT_W[w["hw"]] / NAV_RES)), joint="curve")
                else:
                    continue
                if w["bridge"]:
                    bridge_ways.append((pts, ROAD_W.get(w["hw"], FOOT_W.get(w["hw"], 4)) + 4))
            R, F = np.asarray(road, "u1"), np.asarray(foot, "u1")
            flags = np.where(R > 0, (flags & ~np.uint8(B_BUILDING)) | B_ROAD, flags)
            flags = np.where(F > 0, (flags & ~np.uint8(B_BUILDING)) | B_FOOT, flags)
            hdeck = np.zeros((n, n), "u1")
            if bridge_ways:
                for pts, wd in bridge_ways:
                    dd.line(pts, fill=1, width=max(2, round(wd / NAV_RES)))
                mask = np.asarray(deck, "u1") > 0
                mns, (sx0, sy1), rs = ign.mosaic("mns", bbox)
                mnt, _, _ = ign.mosaic("mnt", bbox)
                kk = int(NAV_RES / rs)
                rr0, cc0 = int(round((sy1 - (by0 + NAV_CHUNK)) / rs)), int(round((bx0 - sx0) / rs))
                hh = (mns - mnt)[rr0:rr0 + n * kk, cc0:cc0 + n * kk]
                if hh.shape == (n * kk, n * kk):
                    hh = np.median(hh.reshape(n, kk, n, kk), axis=(1, 3))
                    from scipy import ndimage
                    hh = ndimage.median_filter(np.where(mask, hh, 0), 5)
                    hdeck = np.where(mask, np.clip(np.round(hh * 10), 0, 255), 0).astype("u1")
                    flags = np.where(mask & (hdeck > 15), flags & ~np.uint8(B_WATER), flags)
            rgb = np.stack([flags, hdeck, np.zeros_like(flags)], -1)
            Image.fromarray(rgb, "RGB").save(out / f"{cx}_{cy}.png", optimize=True)
            count += 1
    meta = json.loads((WEB / "world.json").read_text())
    meta["nav"] = {"res": NAV_RES, "chunk": NAV_CHUNK, "flags": {"building": B_BUILDING, "water": B_WATER,
                                                                  "road": B_ROAD, "foot": B_FOOT}}
    (WEB / "world.json").write_text(json.dumps(meta))
    print(f"nav: {count} chunks", flush=True)


def rail_routes():
    """Rail lines from Nantes station to the edge of the map (one per direction), for placing trains."""
    o = load_osm()
    g = Graph([w for w in o["rails"] if w["rw"] == "rail" and w.get("service") in (None, "main")], directed=False)
    st = _to_l93.transform(-1.542077, 47.217491)
    start = g.nearest(*st, k=1)[0][0]
    import heapq
    dist, prev = {start: 0.0}, {}
    q = [(0.0, start)]
    while q:
        d, n = heapq.heappop(q)
        if d > dist.get(n, 1e18):
            continue
        for m, L in g.adj[n]:
            if d + L < dist.get(m, 1e18):
                dist[m] = d + L; prev[m] = n; heapq.heappush(q, (d + L, m))
    best = {}
    for n, d in dist.items():
        x, y = g.xy[n]
        if geo.inside(x, y):
            continue
        ang = round(math.degrees(math.atan2(y - st[1], x - st[0])) / 20)
        if ang not in best or d < best[ang][1]:
            best[ang] = (n, d)                         # first exit in that direction
    T = Terrain()
    ox, oy = geo.ORIGIN
    routes = []
    for n, d in best.values():
        path = [n]
        while path[-1] in prev:
            path.append(prev[path[-1]])
        xy = np.array([g.xy[k] for k in reversed(path)])
        z = T.z(xy)
        routes.append({"p": [[round(x - ox, 1), round(y - oy, 1), round(float(h), 1)] for (x, y), h in zip(xy, z)],
                       "len": round(d, 1)})
    (WEB / "rails.json").write_text(json.dumps({"station": [round(st[0] - ox, 1), round(st[1] - oy, 1)],
                                                "routes": routes}, separators=(",", ":")))
    print(f"rail routes from the station: {len(routes)}", flush=True)


def rooms():
    """web/data/world/rooms.json: for every place, the BD TOPO building it is in (or the closest one within 6 m):
    footprint (outer ring, local metres), height, floors, so the viewer can open the building like a doll's house
    when someone goes in."""
    def inside(ring, x, y):
        c = False
        for (x1, y1), (x2, y2) in zip(ring, ring[1:] + ring[:1]):
            if (y1 > y) != (y2 > y) and x < x1 + (y - y1) * (x2 - x1) / (y2 - y1):
                c = not c
        return c

    def dist(ring, x, y):
        best = 1e9
        for (x1, y1), (x2, y2) in zip(ring, ring[1:] + ring[:1]):
            dx, dy = x2 - x1, y2 - y1
            t = max(0.0, min(1.0, ((x - x1) * dx + (y - y1) * dy) / (dx * dx + dy * dy or 1)))
            best = min(best, math.hypot(x - x1 - t * dx, y - y1 - t * dy))
        return best

    def area(ring):
        return abs(sum(x1 * y2 - x2 * y1 for (x1, y1), (x2, y2) in zip(ring, ring[1:] + ring[:1]))) / 2

    def simplify(ring, tol=0.3):
        # drop vertices closer than tol to the line through their neighbours (one pass is enough for BD TOPO)
        out = []
        n = len(ring)
        for k in range(n):
            a, b, c = ring[k - 1], ring[k], ring[(k + 1) % n]
            if dist([a, c], b[0], b[1]) > tol:
                out.append(b)
        return out if len(out) >= 3 else ring

    meta = json.loads((WEB / "world.json").read_text())
    ox, oy = meta["origin"]
    pois = json.loads((WEB / "pois.json").read_text())
    out, cache = {}, {}
    for p in pois:
        x, y = p["p"][0] + ox, p["p"][1] + oy
        key = (int(x // 250), int(y // 250))
        if key not in cache:
            bb = (key[0] * 250 - 30, key[1] * 250 - 30, key[0] * 250 + 280, key[1] * 250 + 280)
            polys = []
            for b in ign.buildings(bb):
                ring = [tuple(c) for c in b["rings"][0]]
                if ring and ring[0] == ring[-1]:
                    ring = ring[:-1]
                if len(ring) >= 3 and 8 < area(ring) < 6000:
                    polys.append((ring, b))
            cache[key] = polys
        best, bd = None, 6.0
        for ring, b in cache[key]:
            d = 0.0 if inside(ring, x, y) else dist(ring, x, y)
            if d < bd:
                best, bd = (ring, b), d
            if d == 0.0:
                break
        if not best:
            continue
        ring, b = best
        ring = simplify(ring)
        if len(ring) > 80:
            continue
        out[p["id"]] = {"r": [[round(cx - ox, 1), round(cy - oy, 1)] for cx, cy in ring],
                        "h": round(float(b.get("height") or 9.0), 1), "f": b.get("floors"), "z": p["p"][2]}
    (WEB / "rooms.json").write_text(json.dumps(out, separators=(",", ":")))
    print(f"rooms: {len(out)} of {len(pois)} places", flush=True)


def main(argv):
    what = argv[0] if argv else "all"
    if what in ("osm", "all"):
        osm()
    if what in ("transit", "all"):
        transit()
    if what in ("export", "all"):
        export()
    if what in ("nav", "all"):
        nav()
    if what in ("rails", "all"):
        rail_routes()
    if what in ("rooms", "all"):
        rooms()


if __name__ == "__main__":
    main(sys.argv[1:])
