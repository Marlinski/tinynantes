"""Isometric renders from Google Photorealistic 3D Tiles (real facades and roofs).

The tileset is walked from the root, keeping only nodes whose bounding box reaches the tile's ground bbox and
refining until the node's geometric error is below TARGET_ERR. Tile JSON and GLB files are cached under
cache/g3d/ with the session parameter stripped, so a fully cached tile costs no API call at all
(a root request, which opens a billed session, is made only when something is missing).

Meshes are ECEF (glTF y-up -> z-up), converted to Lambert-93 + NGF-ish height and drawn unlit with their
own textures (the photogrammetry already carries the light).

Needs GOOGLE_MAPS_KEY in .env (Map Tiles API).
Usage: uv run python -m tinynantes.g3d H I J [I J ...]  ->  out/g3d/hH/I_J.png (with a 128 px margin)
"""
from __future__ import annotations

import hashlib
import io
import json
import os
import sys
import threading
from concurrent.futures import ThreadPoolExecutor
from functools import lru_cache
from pathlib import Path
from urllib.parse import urlsplit, parse_qsl, urlencode

import moderngl
import numpy as np
import requests
from PIL import Image
from pyproj import Transformer

from . import geo
from .stylize import load_env

ROOT = Path(__file__).resolve().parent.parent
CACHE = ROOT / "cache" / "g3d"
API = "https://tile.googleapis.com"
TARGET_ERR = float(os.environ.get("G3D_ERR", "1.0"))   # metres
GEOID = 47.0   # ellipsoid - NGF height around Nantes, calibrated on the LiDAR terrain (ground mode)
SS = 2
RAILS = os.environ.get("G3D_RAILS", "1") == "1"

_ecef2geo = Transformer.from_crs("EPSG:4978", "EPSG:4979", always_xy=True)
_geo2l93 = Transformer.from_crs("EPSG:4326", "EPSG:2154", always_xy=True)
Y_UP_TO_Z_UP = np.array([[1, 0, 0, 0], [0, 0, -1, 0], [0, 1, 0, 0], [0, 0, 0, 1]], "f8")


def ecef_to_local(p):
    lon, lat, h = _ecef2geo.transform(p[:, 0], p[:, 1], p[:, 2])
    x, y = _geo2l93.transform(lon, lat)
    return np.stack([x - geo.ORIGIN[0], y - geo.ORIGIN[1], h - GEOID], -1)


class Client:
    def __init__(self):
        load_env()
        self.key = os.environ.get("GOOGLE_MAPS_KEY")
        self.session = None
        self.lock = threading.Lock()
        self.http = requests.Session()
        self.calls = 0

    def _session(self):
        with self.lock:
            if self.session is None:
                if not self.key:
                    raise SystemExit("GOOGLE_MAPS_KEY missing from .env")
                r = self.http.get(f"{API}/v1/3dtiles/root.json", params={"key": self.key}, timeout=60)
                if r.status_code != 200:
                    raise SystemExit(f"root.json: HTTP {r.status_code} {r.text[:300]}")
                self.calls += 1
                root = r.json()
                self._root = root
                for u in _uris(root):
                    q = dict(parse_qsl(urlsplit(u).query))
                    if "session" in q:
                        self.session = q["session"]; break
            return self.session

    def get(self, uri: str) -> bytes:
        path = urlsplit(uri).path
        f = CACHE / hashlib.sha1(path.encode()).hexdigest()[:24]
        f = f.with_suffix(Path(path).suffix or ".bin")
        if f.exists():
            return f.read_bytes()
        sess = self._session()
        for attempt in range(4):
            try:
                r = self.http.get(API + path, params={"key": self.key, "session": sess}, timeout=60)
                if r.status_code != 200:
                    raise requests.RequestException(f"HTTP {r.status_code} on {path}")
                break
            except requests.RequestException as e:
                if attempt == 3:
                    raise RuntimeError(str(e).replace(self.key, "KEY")) from None
        self.calls += 1
        data = r.content
        f.parent.mkdir(parents=True, exist_ok=True)
        tmp = f.with_suffix(f.suffix + ".tmp"); tmp.write_bytes(data); tmp.replace(f)
        return data

    def root(self):
        f = CACHE / "root.json"
        if f.exists():
            return json.loads(f.read_text())
        self._session()
        CACHE.mkdir(parents=True, exist_ok=True)
        f.write_text(json.dumps(self._root))
        return self._root


def _uris(node):
    c = node.get("root", node)
    stack = [c]
    while stack:
        n = stack.pop()
        for k in ("content", ):
            if k in n and "uri" in n[k]:
                yield n[k]["uri"]
        stack += n.get("children", [])


def _box_l93(box):
    c = np.array(box[:3]); a = np.array(box[3:]).reshape(3, 3)
    s = np.array([[i, j, k] for i in (-1, 1) for j in (-1, 1) for k in (-1, 1)], "f8")
    pts = c + s @ a
    loc = ecef_to_local(pts)
    return loc[:, 0].min(), loc[:, 1].min(), loc[:, 0].max(), loc[:, 1].max(), np.abs(a).sum()


def collect(client, bbox_local):
    """GLB uris covering the local (relative to ORIGIN) xy bbox at TARGET_ERR."""
    x0, y0, x1, y1 = bbox_local
    out = []

    def hits(n):
        bv = n.get("boundingVolume", {})
        if "box" not in bv:
            return True
        bx0, by0, bx1, by1, size = _box_l93(bv["box"])
        if size > 5e5:     # continent-scale boxes: projection is meaningless, keep walking
            return True
        return bx0 <= x1 and bx1 >= x0 and by0 <= y1 and by1 >= y0

    def walk(n):
        if not hits(n):
            return
        kids = n.get("children", [])
        uri = n.get("content", {}).get("uri")
        if uri and uri.split("?")[0].endswith(".json"):
            sub = json.loads(client.get(uri))
            walk(sub["root"]); return
        err = n.get("geometricError", 0)
        if uri and (err <= TARGET_ERR or not kids):
            out.append(uri); return
        if not kids and uri:
            out.append(uri); return
        for k in kids:
            walk(k)

    walk(client.root()["root"])
    return out


def load_glb(data: bytes):
    """-> list of (positions ECEF Nx3, uv Nx2, faces Mx3, texture PIL)"""
    from pygltflib import GLTF2
    g = GLTF2.load_from_bytes(data)
    blob = g.binary_blob()

    def bview(i):
        bv = g.bufferViews[i]
        return blob[bv.byteOffset or 0:(bv.byteOffset or 0) + bv.byteLength]

    DT = {5120: "i1", 5121: "u1", 5122: "i2", 5123: "u2", 5125: "u4", 5126: "f4"}
    NC = {"SCALAR": 1, "VEC2": 2, "VEC3": 3, "VEC4": 4}

    def acc(i):
        a = g.accessors[i]
        bv = g.bufferViews[a.bufferView]
        dt = np.dtype(DT[a.componentType]); nc = NC[a.type]
        off = (bv.byteOffset or 0) + (a.byteOffset or 0)
        stride = bv.byteStride or dt.itemsize * nc
        raw = np.frombuffer(blob, "u1", count=stride * (a.count - 1) + dt.itemsize * nc, offset=off)
        out = np.lib.stride_tricks.as_strided(raw, (a.count, dt.itemsize * nc), (stride, 1)).copy().view(dt)
        out = out.reshape(a.count, nc) if nc > 1 else out.ravel()
        if a.normalized and dt.kind in "iu":
            out = out.astype("f4") / np.iinfo(dt).max
        return out

    def node_mats():
        mats = {}
        def rec(ni, parent):
            n = g.nodes[ni]
            m = np.array(n.matrix, "f8").reshape(4, 4).T if n.matrix else np.eye(4)
            if n.translation and not n.matrix:
                m = m.copy(); m[:3, 3] = n.translation
            m = parent @ m
            if n.mesh is not None:
                mats.setdefault(n.mesh, []).append(m)
            for c in n.children or []:
                rec(c, m)
        for ni in g.scenes[g.scene or 0].nodes:
            rec(ni, Y_UP_TO_Z_UP)
        return mats

    res = []
    for mi, mats in node_mats().items():
        for prim in g.meshes[mi].primitives:
            ext = (prim.extensions or {}).get("KHR_draco_mesh_compression")
            if ext:
                import DracoPy
                d = DracoPy.decode(bview(ext["bufferView"]))
                pts = np.asarray(d.points, "f8").reshape(-1, 3)
                faces = np.asarray(d.faces, "u4").reshape(-1, 3)
                uv = np.asarray(d.tex_coord, "f4").reshape(-1, 2)
            else:
                pts = acc(prim.attributes.POSITION).astype("f8")
                uv = acc(prim.attributes.TEXCOORD_0)
                faces = acc(prim.indices).astype("u4").reshape(-1, 3)
            mat = g.materials[prim.material]
            ti = mat.pbrMetallicRoughness.baseColorTexture.index
            img = g.images[g.textures[ti].source]
            tex = Image.open(io.BytesIO(bview(img.bufferView))).convert("RGB")
            for m in mats:
                p = pts @ m[:3, :3].T + m[:3, 3]
                res.append((p, uv, faces, tex))
    return res


VS = """
#version 330
in vec3 in_p; in vec2 in_uv;
uniform vec3 u_r, u_d, u_b; uniform vec4 u_view;
out vec2 v_uv;
void main() {
    float sx = dot(in_p, u_r), sy = dot(in_p, u_d), dp = dot(in_p, u_b);
    gl_Position = vec4((sx - u_view.x) / u_view.z, -(sy - u_view.y) / u_view.z, -(dp - u_view.w) / 1500.0, 1.0);
    v_uv = in_uv;
}
"""
FS = """
#version 330
in vec2 v_uv; uniform sampler2D u_tex; out vec4 f_color;
void main() { f_color = vec4(texture(u_tex, v_uv).rgb, 1.0); }
"""


VS_C = """
#version 330
in vec3 in_p; in vec3 in_c;
uniform vec3 u_r, u_d, u_b; uniform vec4 u_view;
out vec3 v_c;
void main() {
    vec3 p = in_p + u_b * 1.5;     // pulled 1.5 m toward the camera: wins over the ground, loses to buildings
    float sx = dot(p, u_r), sy = dot(p, u_d), dp = dot(p, u_b);
    gl_Position = vec4((sx - u_view.x) / u_view.z, -(sy - u_view.y) / u_view.z, -(dp - u_view.w) / 1500.0, 1.0);
    v_c = in_c;
}
"""
FS_C = """
#version 330
in vec3 v_c; out vec4 f_color;
void main() { f_color = vec4(v_c, 1.0); }
"""


def overlay_rails(art, rails):
    """Put the rail layer (RGBA, from render_tile) back over a painted tile: the painter tends to turn
    tram platforms into roads."""
    if rails is None:
        return art
    r = np.asarray(rails, "f4")
    a = r[..., 3:] / 255.0 * 0.9
    return np.clip(np.asarray(art, "f4") * (1 - a) + r[..., :3] * a, 0, 255).astype("u1")


@lru_cache(maxsize=1)
def _ctx():
    ctx = moderngl.create_standalone_context(backend="egl")
    return ctx, ctx.program(vertex_shader=VS, fragment_shader=FS), ctx.program(vertex_shader=VS_C, fragment_shader=FS_C)


def rail_mesh(bbox):
    """Ballast, sleepers and rails of BD TOPO railways and tram lines (not tunnels or bridges) at terrain height,
    as (vertices [x y z r g b], triangles) in the local frame."""
    from . import ign
    from .render import _offset
    mnt, (gx0, gy1), res = ign.mosaic("mnt", bbox)
    ox, oy = geo.ORIGIN

    def z_at(p):
        c = np.clip(((p[:, 0] - gx0) / res).astype(int), 0, mnt.shape[1] - 1)
        r = np.clip(((gy1 - p[:, 1]) / res).astype(int), 0, mnt.shape[0] - 1)
        return mnt[r, c]

    V, F = [], []

    def strip(a, b, z, col, dz=0.0):
        n = len(a)
        base = sum(len(v) for v in V)
        pts = np.concatenate([a, b]); zz = np.concatenate([z, z]) + dz
        V.append(np.column_stack([pts[:, 0] - ox, pts[:, 1] - oy, zz, np.tile(col, (2 * n, 1))]))
        k = np.arange(n - 1)
        F.append(np.concatenate([np.column_stack([base + k, base + k + 1, base + n + k]),
                                 np.column_stack([base + k + 1, base + n + k + 1, base + n + k])]))

    for ln in ign.rails(bbox):
        if ln["level"] != "0" or len(ln["xy"]) < 2:
            continue
        tram = ln["nature"] == "Tramway"
        p = np.asarray(ln["xy"], "f8")
        seg = np.linalg.norm(np.diff(p, axis=0), axis=1)
        s = np.r_[0, np.cumsum(seg)]
        if s[-1] < 1:
            continue
        t = np.arange(0, s[-1], 1.0)
        p = np.column_stack([np.interp(t, s, p[:, 0]), np.interp(t, s, p[:, 1])])
        ntr = int(ln["tracks"])
        for k in range(ntr):
            c = _offset(p, (k - (ntr - 1) / 2) * (3.4 if tram else 4.5))
            z = z_at(c)
            strip(_offset(c, -1.5 if tram else -1.7), _offset(c, 1.5 if tram else 1.7), z,
                  (0.62, 0.60, 0.55) if tram else (0.47, 0.42, 0.36))                               # bed / ballast
            for side in (-0.72, 0.72):
                if tram:   # embedded grooved rails: two dark lines on the platform
                    strip(_offset(c, side - 0.14), _offset(c, side + 0.14), z, (0.16, 0.16, 0.18), 0.05)
                else:
                    strip(_offset(c, side - 0.08), _offset(c, side + 0.08), z, (0.82, 0.82, 0.85), 0.05)
            if tram:
                continue
            # sleepers every 0.65 m
            tan = np.gradient(c, axis=0); tan /= np.maximum(np.linalg.norm(tan, axis=1, keepdims=True), 1e-9)
            nrm = np.column_stack([-tan[:, 1], tan[:, 0]])
            for off in np.arange(0, 1.0, 0.65):
                m = c + tan * off
                for q in range(len(m)):
                    a = m[q] - nrm[q] * 1.25; b = m[q] + nrm[q] * 1.25
                    d = tan[q] * 0.11
                    strip(np.array([a - d, a + d]), np.array([b - d, b + d]), np.array([z[q], z[q]]),
                          (0.42, 0.42, 0.42) if tram else (0.30, 0.25, 0.21), 0.02)
    if not V:
        return None
    return np.concatenate(V).astype("f4"), np.concatenate(F).astype("u4")


def render_tile(i, j, view=geo.View(0), margin_px=128, client=None):
    client = client or Client()
    T = geo.TILE_M
    mm = margin_px * geo.M_PER_PX
    pad = 4.0 + mm
    bx0, by0, bx1, by1 = view.ground_bbox(i * T - pad, j * T - pad, (i + 1) * T + pad, (j + 1) * T + pad)
    ox, oy = geo.ORIGIN
    uris = collect(client, (bx0 - ox, by0 - oy, bx1 - ox, by1 - oy))
    with ThreadPoolExecutor(16) as ex:
        blobs = list(ex.map(client.get, uris))
    ctx, prog, cprog = _ctx()
    r, dn, bk = view.axes
    for pr in (prog, cprog):
        pr["u_r"].value = tuple(r); pr["u_d"].value = tuple(dn); pr["u_b"].value = tuple(bk)
    cx, cy = (i + 0.5) * T, (j + 0.5) * T
    inv = np.linalg.inv(np.array([[r[0], r[1]], [dn[0], dn[1]]]))
    gx, gy = inv @ np.array([cx - r[2] * 10.0, cy - dn[2] * 10.0])
    for pr in (prog, cprog):
        pr["u_view"].value = (cx, cy, T / 2 + mm, float(gx * bk[0] + gy * bk[1] + 10.0 * bk[2]))
    size = geo.TILE_PX + 2 * margin_px
    n = size * SS
    depth = ctx.depth_renderbuffer((n, n))
    col = ctx.renderbuffer((n, n), 3)
    fbo = ctx.framebuffer([col], depth)
    fbo.use(); ctx.enable(moderngl.DEPTH_TEST); ctx.depth_func = "<"; fbo.clear(0.55, 0.62, 0.66, 1.0)
    for data in blobs:
        for p, uv, faces, tex in load_glb(data):
            loc = ecef_to_local(p).astype("f4")
            t = ctx.texture(tex.size, 3, tex.tobytes())
            t.build_mipmaps(); t.filter = (moderngl.LINEAR_MIPMAP_LINEAR, moderngl.LINEAR)
            t.use(0)
            vbo = ctx.buffer(np.hstack([loc, uv]).astype("f4").tobytes())
            ibo = ctx.buffer(faces.astype("u4").tobytes())
            vao = ctx.vertex_array(prog, [(vbo, "3f 2f", "in_p", "in_uv")], ibo)
            vao.render(moderngl.TRIANGLES)
            for o in (vao, vbo, ibo, t):
                o.release()
    rails = None
    rm = rail_mesh((bx0, by0, bx1, by1)) if RAILS else None
    if rm is not None:
        vbo = ctx.buffer(rm[0].tobytes()); ibo = ctx.buffer(rm[1].tobytes())
        vao = ctx.vertex_array(cprog, [(vbo, "3f 3f", "in_p", "in_c")], ibo)
        vao.render(moderngl.TRIANGLES)
        # the visible rail pixels again, alone on a transparent layer (same depth buffer: hidden parts stay hidden)
        layer = ctx.texture((n, n), 4); layer.write(bytes(n * n * 4))
        fbo2 = ctx.framebuffer([layer], depth)
        fbo2.use(); ctx.depth_func = "<="
        vao.render(moderngl.TRIANGLES)
        rails = Image.frombytes("RGBA", (n, n), fbo2.read(components=4)).transpose(Image.FLIP_TOP_BOTTOM)
        rails = rails.resize((size, size), Image.LANCZOS)
        for o in (vao, vbo, ibo, fbo2, layer):
            o.release()
        ctx.depth_func = "<"
    fbo.use()
    img = Image.frombytes("RGB", (n, n), fbo.read(components=3)).transpose(Image.FLIP_TOP_BOTTOM)
    for o in (fbo, col, depth):
        o.release()
    print(f"{len(uris)} glb, {client.calls} API calls", flush=True)
    return img.resize((size, size), Image.LANCZOS), rails


def main(argv):
    h = int(argv[0])
    nums = list(map(int, argv[1:]))
    out = ROOT / "out" / "g3d" / f"h{h}"
    out.mkdir(parents=True, exist_ok=True)
    c = Client()
    for i, j in zip(nums[0::2], nums[1::2]):
        img, rails = render_tile(i, j, geo.View(h), client=c)
        img.save(out / f"{i}_{j}.png")
        if rails is not None:
            rails.save(out / f"{i}_{j}_rails.png")
        print(f"{i}_{j}", flush=True)


if __name__ == "__main__":
    main(sys.argv[1:])


def water_mask(i, j, view=geo.View(0), margin_px=128):
    """Where real water (BD TOPO hydrography, at terrain height) lands on the tile render, as a bool image
    the size of the render. Occlusion is ignored: the mask only says where water is allowed."""
    from scipy import ndimage
    from . import ign
    T = geo.TILE_M
    mm = margin_px * geo.M_PER_PX
    pad = 4.0 + mm
    bbox = view.ground_bbox(i * T - pad, j * T - pad, (i + 1) * T + pad, (j + 1) * T + pad)
    eau, (gx0, gy1), res = ign.mosaic("eau", bbox)
    mnt, _, _ = ign.mosaic("mnt", bbox)
    rr, cc = np.nonzero(eau > 0)
    size = geo.TILE_PX + 2 * margin_px
    out = np.zeros((size, size), bool)
    if not len(rr):
        return out
    x = gx0 - geo.ORIGIN[0] + (cc + 0.5) * res
    y = gy1 - geo.ORIGIN[1] - (rr + 0.5) * res
    sx, sy, _ = view.to_screen(x, y, mnt[rr, cc])
    px = ((sx - i * T + mm) / geo.M_PER_PX).astype(int)
    py = ((sy - j * T + mm) / geo.M_PER_PX).astype(int)
    k = (px >= 0) & (px < size) & (py >= 0) & (py < size)
    out[py[k], px[k]] = True
    return ndimage.binary_closing(ndimage.binary_dilation(out, iterations=2), iterations=3)
