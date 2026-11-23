"""Second-generation isometric renderer: clean volumes instead of a draped LiDAR surface.

- Ground: LiDAR terrain (MNT) with the orthophoto; small objects (cars, catenaries, lamp posts, trains) are flattened.
- Buildings: BD TOPO footprints extruded with vertical walls; the roof is the LiDAR surface inside the footprint.
  Facades are procedural (material, floor count and colour from BD TOPO).
- Trees: the LiDAR canopy, smoothed into rounded crowns.
- Orthophoto lean: BD ORTHO is not a true orthophoto, roofs and tree tops are displaced by up to ~1 m per metre
  of height. The displacement is estimated per 500 m chunk (roof colour variance), and every surface samples the
  photo at xy + lean * height. Ground the photo hides behind leaning buildings is filled from its surroundings.

Usage: uv run python -m tinynantes.render2 H I J [I J ...]   ->  out/r2/hH/I_J.png (with a 128 px margin)
"""
from __future__ import annotations

import json
import math
import sys
from functools import lru_cache
from pathlib import Path

import moderngl
import numpy as np
from PIL import Image, ImageDraw
from scipy import ndimage

from . import geo, ign
from .render import _clean, draw_rails

ROOT = Path(__file__).resolve().parent.parent
SS = 2
LEAN = ign.CACHE / "lean"

VS = """
#version 330
in vec2 in_xy;
in vec3 in_zgc;      // z, ground z, class
in vec3 in_n;        // wall normal (0 for heightfield surfaces)
in vec2 in_lean;
in vec3 in_wall;     // s along the wall, floor height, building hash
uniform vec3 u_r, u_d, u_b;
uniform vec4 u_view;
out vec3 v_pos; out float v_ground; flat out float v_cls; out vec3 v_n; out vec2 v_lean; out vec3 v_wall;
void main() {
    vec3 p = vec3(in_xy, in_zgc.x);
    float sx = dot(p, u_r), sy = dot(p, u_d), dp = dot(p, u_b);
    gl_Position = vec4((sx - u_view.x) / u_view.z, -(sy - u_view.y) / u_view.z, -(dp - u_view.w) / 1500.0, 1.0);
    v_pos = p; v_ground = in_zgc.y; v_cls = in_zgc.z; v_n = in_n; v_lean = in_lean; v_wall = in_wall;
}
"""

FS = """
#version 330
in vec3 v_pos; in float v_ground; flat in float v_cls; in vec3 v_n; in vec2 v_lean; in vec3 v_wall;
uniform sampler2D u_ortho;
uniform vec4 u_obox;
uniform vec3 u_sun;
out vec4 f_color;
// classes: 0 ground, 1 wall, 2 tree, 3 water, 4 roof, 5 industrial wall, 6 plain wall (church, light)

float hash(vec2 p) { return fract(sin(dot(p, vec2(127.1, 311.7))) * 43758.5453); }

vec3 ortho_at(vec3 p) {
    vec2 q = p.xy + v_lean * max(p.z - v_ground, 0.0);
    vec3 c = texture(u_ortho, vec2((q.x - u_obox.x) / u_obox.z, (u_obox.y - q.y) / u_obox.w)).rgb;
    c.b = min(c.b, max(c.r, c.g) * 1.02);   // only real water may look blue
    return c;
}

vec3 facade(float h, float s, float fh, float bh, float kind) {
    // wall colour by material: 1 stone (tuffeau), 3 concrete, 4 brick, otherwise a pale render
    vec3 wall = vec3(0.88, 0.84, 0.75);
    float m = floor(kind);
    if (m == 3.0) wall = vec3(0.82, 0.82, 0.80);
    else if (m == 4.0) wall = vec3(0.70, 0.44, 0.34);
    else if (m != 1.0) wall = mix(vec3(0.90, 0.88, 0.82), vec3(0.86, 0.80, 0.70), bh);
    wall *= 0.94 + 0.10 * bh;
    float fl = floor(h / fh), row = fract(h / fh);
    float bay = floor(s / 3.0), colp = fract(s / 3.0);
    bool win = row > 0.30 && row < 0.80 && colp > 0.30 && colp < 0.70 && h > 0.4;
    if (fl < 1.0) win = row > 0.10 && row < 0.85 && colp > 0.15 && colp < 0.85;   // shop fronts / doors
    if (win) wall = mix(vec3(0.17, 0.21, 0.27), vec3(0.40, 0.47, 0.55), hash(vec2(bay, fl + bh * 17.0)) * 0.7);
    return wall;
}

void main() {
    vec3 n = length(v_n) > 0.5 ? v_n : normalize(cross(dFdx(v_pos), dFdy(v_pos)));
    if (n.z < 0.0 && length(v_n) < 0.5) n = -n;
    float lam = max(dot(n, u_sun), 0.0);
    float h = v_pos.z - v_ground;
    vec3 col;
    if (v_cls > 0.5 && v_cls < 1.5) {
        col = facade(h, v_wall.x, v_wall.y, fract(v_wall.z), floor(v_wall.z));
        col *= (0.62 + 0.45 * lam) * (0.82 + 0.18 * smoothstep(0.0, 2.5, h));
    } else if (v_cls > 4.5 && v_cls < 5.5) {
        float rib = step(0.5, fract(v_wall.x / 0.8));
        col = vec3(0.70, 0.71, 0.72) * (0.95 + 0.05 * rib) * (0.62 + 0.45 * lam);
    } else if (v_cls > 5.5) {
        col = vec3(0.84, 0.80, 0.72) * (0.62 + 0.45 * lam) * (0.82 + 0.18 * smoothstep(0.0, 2.5, h));
    } else if (v_cls > 2.5 && v_cls < 3.5) {
        float l = dot(ortho_at(v_pos), vec3(0.3, 0.5, 0.2));
        col = vec3(0.22, 0.46, 0.62) * (0.85 + 0.4 * l);
    } else if (v_cls > 3.5) {
        vec3 o = ortho_at(v_pos);
        if (v_wall.z > 1.5 && v_wall.z < 2.5) {   // slate: neutral blue-grey, never turquoise
            float l = dot(o, vec3(0.3, 0.5, 0.2));
            o = vec3(0.43, 0.42, 0.41) * (0.75 + 0.6 * l);
        }
        if (n.z < 0.45) o = mix(o, vec3(0.62, 0.60, 0.58), 0.5);   // steps between roof levels
        col = o * (0.80 + 0.35 * lam);
    } else if (v_cls > 1.5) {
        vec3 o = mix(ortho_at(v_pos), vec3(0.24, 0.38, 0.18), 0.35);
        col = o * (0.50 + 0.75 * lam);
    } else {
        col = ortho_at(v_pos) * (0.92 + 0.15 * lam);
    }
    f_color = vec4(col, 1.0);
}
"""


@lru_cache(maxsize=1)
def _ctx():
    ctx = moderngl.create_standalone_context(backend="egl")
    return ctx, ctx.program(vertex_shader=VS, fragment_shader=FS)


# ---------------------------------------------------------------- orthophoto lean

def _label(blds, shape, x0, y1, res, fill_of=lambda k, b: k):
    im = Image.new("I", shape[::-1], 0)
    dr = ImageDraw.Draw(im)
    for k, b in enumerate(blds, 1):
        px = lambda ring: [((x - x0) / res, (y1 - y) / res) for x, y in ring]
        dr.polygon(px(b["rings"][0]), fill=fill_of(k, b))
        for hole in b["rings"][1:]:
            dr.polygon(px(hole), fill=0)
    return np.asarray(im, dtype="i4")


def lean_chunk(cx, cy):
    """Photo displacement per metre of height (vx, vy) for one 500 m chunk, cached."""
    LEAN.mkdir(parents=True, exist_ok=True)
    f = LEAN / f"{cx}_{cy}.json"
    if f.exists():
        return json.loads(f.read_text())
    C = ign.CHUNK
    bbox = (cx * C, cy * C, (cx + 1) * C, (cy + 1) * C)
    mns, (gx0, gy1), res = ign.mosaic("mns", bbox)
    mnt, _, _ = ign.mosaic("mnt", bbox)
    ortho, _, ores = ign.mosaic("ortho", bbox)
    H = np.nan_to_num(mns - mnt)
    lab = ndimage.grey_erosion(_label(ign.buildings(bbox), mns.shape, gx0, gy1, res), size=3)
    ys, xs = np.nonzero((lab > 0) & (H > 3))
    out = {"v": None, "n": int(len(ys))}
    if len(ys) > 3000:
        sel = np.random.default_rng(0).choice(len(ys), min(30000, len(ys)), replace=False)
        ys, xs = ys[sel], xs[sel]
        L, h = lab[ys, xs], H[ys, xs]
        X, Y = gx0 + (xs + .5) * res, gy1 - (ys + .5) * res
        o = ortho.astype("f4")
        cnt = np.maximum(np.bincount(L), 1)

        def cost(vx, vy):
            px = ((X + vx * h - gx0) / ores).astype(int).clip(0, o.shape[1] - 1)
            py = ((gy1 - Y - vy * h) / ores).astype(int).clip(0, o.shape[0] - 1)
            c = o[py, px]
            return sum((((c[:, k] - (np.bincount(L, c[:, k]) / cnt)[L]) ** 2).mean()) for k in range(3))

        best = min((cost(vx, vy), vx, vy) for vx in np.arange(-1.6, 1.61, 0.2) for vy in np.arange(-1.6, 1.61, 0.2))
        _, bx, by = best
        best = min((cost(vx, vy), vx, vy) for vx in np.arange(bx - .2, bx + .21, .05) for vy in np.arange(by - .2, by + .21, .05))
        out["v"] = [round(float(best[1]), 3), round(float(best[2]), 3)]
    f.write_text(json.dumps(out))
    return out


def lean_field(bbox):
    """Callable (X, Y) -> (vx, vy): bilinear interpolation of chunk estimates (holes filled from neighbours)."""
    C = ign.CHUNK
    cx0, cy0 = math.floor(bbox[0] / C) - 1, math.floor(bbox[1] / C) - 1
    cx1, cy1 = math.floor(bbox[2] / C) + 1, math.floor(bbox[3] / C) + 1
    nx, ny = cx1 - cx0 + 1, cy1 - cy0 + 1
    g = np.full((ny, nx, 2), np.nan)
    for a in range(nx):
        for b in range(ny):
            v = lean_chunk(cx0 + a, cy0 + b)["v"]
            if v:
                g[b, a] = v
    ok = ~np.isnan(g[..., 0])
    if not ok.any():
        g[:] = 0
    elif not ok.all():
        _, (iy, ix) = ndimage.distance_transform_edt(~ok, return_indices=True)
        g = g[iy, ix]

    def f(X, Y):
        fx = np.clip((X + geo.ORIGIN[0]) / C - cx0 - 0.5, 0, nx - 1)
        fy = np.clip((Y + geo.ORIGIN[1]) / C - cy0 - 0.5, 0, ny - 1)
        return np.stack([ndimage.map_coordinates(g[..., k], [fy.ravel(), fx.ravel()], order=1).reshape(np.shape(X))
                         for k in range(2)], -1)
    return f


# ---------------------------------------------------------------- tile

def _ring_area(r):
    r = np.asarray(r)
    return 0.5 * np.sum(r[:-1, 0] * r[1:, 1] - r[1:, 0] * r[:-1, 1])


def render_tile(i: int, j: int, view=geo.View(0), margin_px: int = 128) -> Image.Image:
    T = geo.TILE_M
    mm = margin_px * geo.M_PER_PX
    pad = 4.0 + mm
    bbox = view.ground_bbox(i * T - pad, j * T - pad, (i + 1) * T + pad, (j + 1) * T + pad)
    mns, (gx0, gy1), res = ign.mosaic("mns", bbox)
    mnt, _, _ = ign.mosaic("mnt", bbox)
    ortho, _, ores = ign.mosaic("ortho", bbox)
    lc, _, _ = ign.mosaic("cls", bbox)
    eau, _, _ = ign.mosaic("eau", bbox)
    mns, mnt = _clean(mns, mnt)
    H = np.maximum(mns - mnt, 0)
    ox, oy = geo.ORIGIN
    hgt, wid = mns.shape
    xs = gx0 - ox + (np.arange(wid) + 0.5) * res
    ys = gy1 - oy - (np.arange(hgt) + 0.5) * res
    X, Y = np.meshgrid(xs, ys)
    leanf = lean_field(bbox)
    LV = leanf(X, Y).astype("f4")

    # ---- buildings
    blds = [b for b in ign.buildings(bbox) if len(b["rings"][0]) >= 4]
    lab = _label(blds, mns.shape, gx0, gy1, res)
    nb = len(blds) + 1
    medH = np.zeros(nb)
    # robust per-building roof: clip the surface to [p15 - 1, p99] of its interior
    order = np.argsort(lab.ravel(), kind="stable")
    flat_lab, flat_s = lab.ravel()[order], mns.ravel()[order]
    starts = np.searchsorted(flat_lab, np.arange(nb))
    lo, hi, gnd = np.zeros(nb, "f4"), np.zeros(nb, "f4"), np.zeros(nb, "f4")
    flat_g = mnt.ravel()[order]
    for k in range(1, nb):
        a, b_ = starts[k], (starts[k + 1] if k + 1 < nb else len(flat_lab))
        if b_ - a < 4:
            continue
        s = flat_s[a:b_]
        lo[k], hi[k] = np.percentile(s, 15) - 1.0, np.percentile(s, 99)
        gnd[k] = np.percentile(flat_g[a:b_], 10)
        medH[k] = np.median(s) - gnd[k]
    real = medH > 2.0                       # demolished or tiny: no volume
    real[0] = False
    is_b = real[lab]
    roof = np.where(is_b, np.clip(mns, lo[lab], hi[lab]), 0).astype("f4")
    roof = np.where(is_b, ndimage.median_filter(roof, size=3), 0)
    # extend each roof one cell outward (eaves) so it meets its walls
    _, (iy, ix) = ndimage.distance_transform_edt(~is_b, return_indices=True)
    dom = ndimage.binary_dilation(is_b, iterations=1)
    labD = np.where(dom, lab[iy, ix], 0)
    roofD = np.where(dom, roof[iy, ix], 0).astype("f4")
    groundD = np.where(dom, gnd[labD], mnt).astype("f4")

    # ---- ground surface: terrain, rounded tree crowns, bridges
    veg = (lc == 5) & ~dom & (H > 2.0)
    Hv = ndimage.gaussian_filter(np.where(veg, H, 0).astype("f4"), 1.6)
    Hv = np.where(ndimage.binary_dilation(veg, iterations=3), Hv, 0)
    tree = Hv > 1.2
    water = (eau | (lc == 9)) & ~dom
    bridge = ndimage.binary_dilation(lc == 17, iterations=1)
    if water.sum() > 400:   # decks and boats stand well above the water surface (class 17 is patchy)
        level = np.percentile(mns[water], 20)
        high = ndimage.binary_opening(water & (mns > level + 3.0), iterations=2)
        bridge |= ndimage.binary_dilation(high, iterations=1)
    water &= ~bridge
    zg = mnt.copy()
    zg = np.where(bridge & ~dom, mns, zg)
    cls = np.where(water, 3, 0).astype("f4")

    def grid_mesh(z, g, c, keep_cell=None, drop_steep=None, lab_=None):
        verts = np.concatenate([np.stack([X, Y, z, g, c], -1), np.zeros((hgt, wid, 3)), LV,
                                np.zeros((hgt, wid, 3))], -1).astype("f4").reshape(-1, 13)
        idx = np.arange(hgt * wid, dtype="u4").reshape(hgt, wid)
        a, b, c_, d = idx[:-1, :-1], idx[:-1, 1:], idx[1:, :-1], idx[1:, 1:]
        tris = np.stack([np.stack([a, c_, b], -1), np.stack([b, c_, d], -1)], -2).reshape(-1, 3)
        if keep_cell is not None:
            k = np.repeat(keep_cell.reshape(-1), 2)
            tris = tris[k]
        if lab_ is not None:   # no slanted skirts between different buildings
            zz, ll = z.ravel()[tris], lab_.ravel()[tris]
            same = (ll[:, 0] == ll[:, 1]) & (ll[:, 1] == ll[:, 2])
            tris = tris[same | (np.ptp(zz, 1) < 0.8)]
        return verts, tris.reshape(-1).astype("u4")

    meshes = [grid_mesh(zg, mnt, cls)]
    cell_dom = dom[:-1, :-1] & dom[:-1, 1:] & dom[1:, :-1] & dom[1:, 1:]
    roofmat = np.zeros(nb, "f4")
    for k, b in enumerate(blds, 1):
        m = str(b.get("roof") or "0")[:1]
        roofmat[k] = float(m) if m.isdigit() else 0.0
    rv, rt = grid_mesh(roofD, groundD, np.full_like(roofD, 4), keep_cell=cell_dom, lab_=labD)
    rv[:, 12] = roofmat[labD].reshape(-1)
    meshes.append((rv, rt))

    # ---- trees: one rounded crown per canopy peak
    Hc = ndimage.gaussian_filter(np.where(veg, H, 0).astype("f4"), 1.0)
    peak = (Hc == ndimage.maximum_filter(Hc, size=7)) & (Hc > 2.5) & veg
    py_, px_ = np.nonzero(peak)
    if len(py_):
        ht = Hc[py_, px_]
        rad = np.clip(0.28 * ht + 1.0, 1.6, 5.0)
        cz = mnt[py_, px_] + ht - rad * 0.85
        sv, sf = _icosphere()
        nv = len(sv)
        P = np.repeat(np.stack([X[py_, px_], Y[py_, px_], cz], -1), nv, 0) \
            + np.tile(sv, (len(py_), 1)) * np.repeat(np.stack([rad, rad, rad * 0.85], -1), nv, 0)
        N = np.tile(sv, (len(py_), 1))
        g = np.repeat(mnt[py_, px_], nv)
        lv = np.repeat(LV[py_, px_], nv, 0)
        tv = np.concatenate([P[:, :2], P[:, 2:3], g[:, None], np.full((len(P), 1), 2.0), N, lv,
                             np.zeros((len(P), 3))], 1).astype("f4")
        ti = (np.tile(sf, (len(py_), 1)) + np.repeat(np.arange(len(py_)) * nv, len(sf))[:, None]).reshape(-1)
        meshes.append((tv, ti.astype("u4")))

    # ---- walls
    wv = []
    def zat(x, y, arr):
        c = np.clip(((x + ox - gx0) / res - 0.5).astype(int), 0, wid - 1)
        r = np.clip(((gy1 - (y + oy)) / res - 0.5).astype(int), 0, hgt - 1)
        return arr[r, c], r, c
    for k, b in enumerate(blds, 1):
        if not real[k]:
            continue
        fl = b.get("floors") or 0
        top_h = max(medH[k], 2.5)
        fh = float(np.clip(top_h / fl, 2.6, 4.5)) if fl else 3.1
        mat = str(b.get("walls") or "0")[:1]
        kind = 1.0
        if b.get("nature") and b["nature"] != "Indifférenciée":
            kind = 5.0 if b["nature"].startswith("Industriel") else 6.0
        if b.get("light"):
            kind = 6.0
        bh = (hash(b["id"]) % 1000) / 1000.0
        mcode = float(mat) if mat.isdigit() else 0.0
        for ri, ring in enumerate(b["rings"]):
            r = np.asarray(ring, "f8") - (ox, oy)
            if len(r) < 3:
                continue
            area = _ring_area(r)
            outward_sign = 1.0 if area > 0 else -1.0     # CCW outer: outward is to the right of travel
            if ri > 0:
                outward_sign = -outward_sign if (area > 0) == (_ring_area(np.asarray(b["rings"][0])) > 0) else outward_sign
            for p0, p1 in zip(r[:-1], r[1:]):
                d = p1 - p0
                L = float(np.hypot(*d))
                if L < 0.3:
                    continue
                t = d / L
                nrm = np.array([t[1], -t[0]]) * outward_sign
                m = max(1, int(math.ceil(L / 0.7)))
                u = np.linspace(0, 1, m + 1)
                P = p0 + np.outer(u, d)
                Pin = P - nrm * 0.45
                top, rr, cc = zat(Pin[:, 0], Pin[:, 1], roofD)
                ok = labD[rr, cc] == k
                top = np.where(ok & (top > 0), top, gnd[k] + medH[k])
                bot, _, _ = zat(P[:, 0], P[:, 1], mnt)
                bot = np.minimum(bot, gnd[k]) - 0.3
                s = u * L + (L % 3.0) / 2.0
                lv = leanf(P[:, 0], P[:, 1])
                for q in range(m):
                    for (pp, z, ss) in ((q, bot[q], s[q]), (q + 1, bot[q + 1], s[q + 1]), (q, top[q], s[q]),
                                        (q + 1, bot[q + 1], s[q + 1]), (q + 1, top[q + 1], s[q + 1]), (q, top[q], s[q])):
                        wv.append((P[pp, 0], P[pp, 1], z, gnd[k], kind,
                                   nrm[0], nrm[1], 0.0, lv[pp, 0], lv[pp, 1], ss, fh, mcode + bh * 0.999))
    if wv:
        wv = np.asarray(wv, "f4")
        meshes.append((wv, np.arange(len(wv), dtype="u4")))

    # ---- orthophoto: rails, and ground hidden behind leaning buildings
    lines = [ln for ln in ign.rails(bbox) if not ln["level"].startswith("-")]
    ortho = draw_sports(ortho, gx0, gy1, ores, ign.sports(bbox))
    ortho = draw_rails(ortho, gx0, gy1, ores, lines)
    ortho = _fill_lean_shadows(ortho, blds, real, medH, leanf, gx0, gy1, ores)

    img = _draw(view, i, j, mm, meshes, ortho, gx0, gy1, ores, margin_px)
    T2 = T / 2 + mm
    cx, cy = (i + 0.5) * T, (j + 0.5) * T
    img.info["rails"] = False
    for ln in lines:
        p = np.asarray(ln["xy"]) - np.array(geo.ORIGIN)
        sx, sy, _ = view.to_screen(p[:, 0], p[:, 1], np.full(len(p), 10.0))
        if ((np.abs(sx - cx) < T2) & (np.abs(sy - cy) < T2)).any():
            img.info["rails"] = True
            break
    return img


def draw_sports(ortho, gx0, gy1, res, grounds):
    """Mown grass and white markings on sports pitches, so they read as pitches (never as pools)."""
    im = Image.fromarray(ortho)
    dr = ImageDraw.Draw(im)
    px = lambda p: [((x - gx0) / res, (gy1 - y) / res) for x, y in p]
    w = lambda m: max(1, round(m / res))
    white = (236, 236, 228)
    for g in grounds:
        if g["nature"] != "Grand terrain de sport" or len(g["rings"][0]) < 4:
            continue
        ring = np.asarray(g["rings"][0], "f8")
        c = ring.mean(0)
        u, sv, vt = np.linalg.svd(ring - c, full_matrices=False)
        ax = vt[0]; bx = vt[1]                               # long and short axis
        a = (ring - c) @ ax; b = (ring - c) @ bx
        A, B = (a.max() - a.min()) / 2, (b.max() - b.min()) / 2
        c = c + ax * (a.max() + a.min()) / 2 + bx * (b.max() + b.min()) / 2
        corner = lambda s, t: c + ax * s * A + bx * t * B
        big = g["nature"] == "Grand terrain de sport"
        if big:   # mowing stripes across the long axis
            n = 12
            for k in range(n):
                s0, s1 = -1 + 2 * k / n, -1 + 2 * (k + 1) / n
                col = (92, 148, 62) if k % 2 else (104, 160, 70)
                dr.polygon(px([corner(s0, -1), corner(s1, -1), corner(s1, 1), corner(s0, 1)]), fill=col)
        dr.line(px([corner(-1, -1), corner(1, -1), corner(1, 1), corner(-1, 1), corner(-1, -1)]), fill=white, width=w(0.5))
        dr.line(px([corner(0, -1), corner(0, 1)]), fill=white, width=w(0.5))
        if big:
            r = min(9.15, B * 0.3)
            ang = np.linspace(0, 2 * np.pi, 40)
            dr.line(px([c + ax * r * np.cos(t) + bx * r * np.sin(t) for t in ang]), fill=white, width=w(0.5))
            for s in (-1, 1):   # penalty boxes
                d = min(16.5, A * 0.3) / A
                h = min(20.0, B * 0.6) / B
                dr.line(px([corner(s, -h), corner(s - s * d, -h), corner(s - s * d, h), corner(s, h)]),
                        fill=white, width=w(0.5))
    return np.asarray(im)


@lru_cache(maxsize=1)
def _icosphere():
    t = (1 + 5 ** 0.5) / 2
    v = [(-1, t, 0), (1, t, 0), (-1, -t, 0), (1, -t, 0), (0, -1, t), (0, 1, t), (0, -1, -t), (0, 1, -t),
         (t, 0, -1), (t, 0, 1), (-t, 0, -1), (-t, 0, 1)]
    f = [(0, 11, 5), (0, 5, 1), (0, 1, 7), (0, 7, 10), (0, 10, 11), (1, 5, 9), (5, 11, 4), (11, 10, 2), (10, 7, 6),
         (7, 1, 8), (3, 9, 4), (3, 4, 2), (3, 2, 6), (3, 6, 8), (3, 8, 9), (4, 9, 5), (2, 4, 11), (6, 2, 10),
         (8, 6, 7), (9, 8, 1)]
    v = [np.array(p, "f8") / np.linalg.norm(p) for p in v]
    cache = {}
    def mid(a, b):
        k = (min(a, b), max(a, b))
        if k not in cache:
            m = v[a] + v[b]; v.append(m / np.linalg.norm(m)); cache[k] = len(v) - 1
        return cache[k]
    for _ in range(2):
        f = [tri for a, b, c in f for tri in ((a, mid(a, b), mid(a, c)), (b, mid(b, c), mid(a, b)),
                                              (c, mid(a, c), mid(b, c)), (mid(a, b), mid(b, c), mid(a, c)))]
    return np.array(v, "f4"), np.array(f, "u4")


def _fill_lean_shadows(ortho, blds, real, medH, leanf, gx0, gy1, ores):
    """Where the photo shows a leaning roof or facade instead of the ground beside a building, the ground
    colour is unknown: fill it from the surrounding visible ground."""
    ox, oy = geo.ORIGIN
    occ = Image.new("L", (ortho.shape[1], ortho.shape[0]), 0)
    top = Image.new("L", occ.size, 0)
    dr, dt = ImageDraw.Draw(occ), ImageDraw.Draw(top)
    px = lambda q: [((x - gx0) / ores, (gy1 - y) / ores) for x, y in q]
    for k, b in enumerate(blds, 1):
        if not real[k]:
            continue
        ring = np.asarray(b["rings"][0], "f8")
        c = ring.mean(0)
        v = leanf(np.array([c[0] - ox]), np.array([c[1] - oy]))[0] * medH[k]
        for t in np.linspace(0, 1, 9):
            dr.polygon(px(ring + v * t), fill=255)
        dt.polygon(px(ring + v), fill=255)      # where the photo shows this roof: keep it
    occ = (np.asarray(occ) > 0) & ~(np.asarray(top) > 0)
    vis = ~occ & ~(np.asarray(top) > 0)
    o = ortho.astype("f4")
    out = o.copy()
    acc = np.zeros_like(o)
    w = np.zeros(o.shape[:2], "f4")
    for sigma in (6, 20, 60):
        num = np.stack([ndimage.gaussian_filter(o[..., k] * vis, sigma) for k in range(3)], -1)
        den = ndimage.gaussian_filter(vis.astype("f4"), sigma)
        good = (den > 0.15) & (w == 0)
        acc[good] = num[good] / den[good, None]
        w[good] = 1
    acc[w == 0] = (150, 148, 142)
    out[occ] = acc[occ]
    return np.clip(out, 0, 255).astype("u1")


def _draw(view, i, j, mm, meshes, ortho, gx0, gy1, ores, margin_px):
    T = geo.TILE_M
    ctx, prog = _ctx()
    r, dn, bk = view.axes
    prog["u_r"].value = tuple(r); prog["u_d"].value = tuple(dn); prog["u_b"].value = tuple(bk)
    cx, cy = (i + 0.5) * T, (j + 0.5) * T
    inv = np.linalg.inv(np.array([[r[0], r[1]], [dn[0], dn[1]]]))
    gx, gy = inv @ np.array([cx - r[2] * 10.0, cy - dn[2] * 10.0])
    prog["u_view"].value = (cx, cy, T / 2 + mm, float(gx * bk[0] + gy * bk[1] + 10.0 * bk[2]))
    ox, oy = geo.ORIGIN
    oh, ow = ortho.shape[:2]
    prog["u_obox"].value = (gx0 - ox, gy1 - oy, ow * ores, oh * ores)
    sun = np.array([-0.45, -0.75, 0.8]); sun /= np.linalg.norm(sun)
    prog["u_sun"].value = tuple(sun)
    tex = ctx.texture((ow, oh), 3, np.ascontiguousarray(ortho).tobytes())
    tex.build_mipmaps(); tex.filter = (moderngl.LINEAR_MIPMAP_LINEAR, moderngl.LINEAR)
    tex.use(0); prog["u_ortho"].value = 0
    size = geo.TILE_PX + 2 * margin_px
    n = size * SS
    fbo = ctx.simple_framebuffer((n, n), components=3)
    fbo.use(); ctx.enable(moderngl.DEPTH_TEST); fbo.clear(0.55, 0.62, 0.66, 1.0)
    objs = [tex, fbo]
    for verts, tris in meshes:
        vbo = ctx.buffer(np.ascontiguousarray(verts, "f4").tobytes()); ibo = ctx.buffer(tris.tobytes())
        vao = ctx.vertex_array(prog, [(vbo, "2f 3f 3f 2f 3f", "in_xy", "in_zgc", "in_n", "in_lean", "in_wall")], ibo)
        vao.render(moderngl.TRIANGLES)
        objs += [vao, vbo, ibo]
    img = Image.frombytes("RGB", (n, n), fbo.read(components=3)).transpose(Image.FLIP_TOP_BOTTOM)
    for o in objs:
        o.release()
    return img.resize((size, size), Image.LANCZOS)


def main(argv):
    h = int(argv[0])
    nums = list(map(int, argv[1:]))
    out = ROOT / "out" / "r2" / f"h{h}"
    out.mkdir(parents=True, exist_ok=True)
    for i, j in zip(nums[0::2], nums[1::2]):
        render_tile(i, j, geo.View(h)).save(out / f"{i}_{j}.png")
        print(f"{i}_{j}", flush=True)


if __name__ == "__main__":
    main(sys.argv[1:])
