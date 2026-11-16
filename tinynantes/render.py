"""Render isometric 'source' tiles from IGN LiDAR HD + BD ORTHO with OpenGL (moderngl, headless EGL).

Usage: uv run python -m tinynantes.render I J [I J ...]   (heading 0)
       uv run python -m tinynantes.render --area            (every tile of geo.AREA)
Output: out/h0/render/I_J.png
"""
from __future__ import annotations

import sys
from functools import lru_cache
from pathlib import Path

import moderngl
import numpy as np
from PIL import Image
from scipy import ndimage

from . import geo, ign

ROOT = Path(__file__).resolve().parent.parent
SS = 2  # supersampling

VS = """
#version 330
in vec2 in_xy;          // world xy relative to origin
in vec3 in_zgb;         // surface z, ground z, building mask
uniform vec3 u_r, u_d, u_b;
uniform vec4 u_view;    // sx centre, sy centre, half size, depth centre
out vec3 v_pos;
out float v_ground;
flat out float v_bati;
void main() {
    vec3 p = vec3(in_xy, in_zgb.x);
    float sx = dot(p, u_r), sy = dot(p, u_d), dp = dot(p, u_b);
    gl_Position = vec4((sx - u_view.x) / u_view.z, -(sy - u_view.y) / u_view.z,
                       -(dp - u_view.w) / 1500.0, 1.0);
    v_pos = p; v_ground = in_zgb.y; v_bati = in_zgb.z;
}
"""

FS = """
#version 330
in vec3 v_pos;
in float v_ground;
flat in float v_bati;   // 0 ground/other, 1 building, 2 vegetation, 3 water
uniform sampler2D u_ortho;
uniform vec4 u_obox;    // ortho x0, y1 (top-left, relative to origin), width m, height m
uniform vec3 u_sun;
out vec4 f_color;

float hash(vec2 p) { return fract(sin(dot(p, vec2(127.1, 311.7))) * 43758.5453); }

void main() {
    vec3 n = normalize(cross(dFdx(v_pos), dFdy(v_pos)));
    if (n.z < 0.0) n = -n;
    vec2 uv = vec2((v_pos.x - u_obox.x) / u_obox.z, (u_obox.y - v_pos.y) / u_obox.w);
    vec3 ortho = texture(u_ortho, uv).rgb;
    float lam = max(dot(n, u_sun), 0.0);
    bool steep = n.z < 0.6;
    vec3 col;
    if (steep && v_bati > 0.5 && v_bati < 1.5) {
        // facade: Nantes tuffeau with windows on a floor grid
        vec3 t = normalize(vec3(-n.y, n.x, 0.0));
        float s = dot(v_pos.xy, t.xy);
        float h = v_pos.z - v_ground;
        float fl = floor(h / 3.1);
        float row = fract(h / 3.1), colp = fract(s / 2.7);
        float bay = floor(s / 2.7);
        vec3 wall = vec3(0.87, 0.83, 0.74) * (0.95 + 0.08 * hash(vec2(floor(s / 9.0), 1.0)));
        bool win = h > 0.6 && row > 0.28 && row < 0.82 && colp > 0.28 && colp < 0.72;
        if (win) wall = mix(vec3(0.20, 0.25, 0.32), vec3(0.45, 0.52, 0.60), hash(vec2(bay, fl)) * 0.6);
        if (h < 0.5) wall *= 0.85;
        col = wall * (0.55 + 0.55 * lam);
    } else if (steep) {
        vec3 blur = textureLod(u_ortho, uv, 5.0).rgb;
        if (v_bati > 1.5) col = blur * vec3(0.75, 0.85, 0.75) * (0.55 + 0.4 * lam);  // canopy sides
        else       col = vec3(0.80, 0.77, 0.70) * (0.55 + 0.55 * lam);        // ramparts, quays, stone
    } else if (v_bati > 2.5) {
        // water: a clear river blue keeps the redraw from reading the brown Loire as ground or ballast
        float l = dot(ortho, vec3(0.3, 0.5, 0.2));
        col = vec3(0.22, 0.46, 0.62) * (0.85 + 0.4 * l);
    } else {
        col = ortho * (0.92 + 0.15 * lam);
    }
    f_color = vec4(col, 1.0);
}
"""


@lru_cache(maxsize=1)
def _ctx():
    ctx = moderngl.create_standalone_context(backend="egl")
    prog = ctx.program(vertex_shader=VS, fragment_shader=FS)
    return ctx, prog


def _clean(mns, mnt):
    bad = ~np.isfinite(mnt) | (mnt < -50)
    if bad.any():
        mnt = mnt.copy(); mnt[bad] = np.nanmedian(mnt[~bad]) if (~bad).any() else 0.0
    bad = ~np.isfinite(mns) | (mns < -50) | (mns < mnt - 1)
    mns = np.where(bad, mnt, mns)
    # remove single-cell spikes (cranes, wires, birds)
    med = ndimage.median_filter(mns, size=3)
    mns = np.where(np.abs(mns - med) > 4, med, mns)
    return mns.astype("f4"), mnt.astype("f4")


def _offset(xy, d):
    """Offset a polyline sideways by d metres (left of travel direction)."""
    p = np.asarray(xy, "f8")
    if len(p) < 2 or d == 0:
        return p
    t = np.gradient(p, axis=0)
    t /= np.maximum(np.linalg.norm(t, axis=1, keepdims=True), 1e-9)
    return p + np.stack([-t[:, 1], t[:, 0]], 1) * d


def draw_rails(ortho, gx0, gy1, res, lines):
    """Paint tram and train tracks onto the orthophoto (absolute Lambert-93 frame), exaggerated a bit so
    they survive the 0.29 m/px render and read as rails to the redraw model."""
    from PIL import ImageDraw
    im = Image.fromarray(ortho)
    dr = ImageDraw.Draw(im)
    px = lambda p: [((x - gx0) / res, (gy1 - y) / res) for x, y in p]
    w = lambda m: max(1, round(m / res))
    for ln in lines:
        if ln["level"].startswith("-") or len(ln["xy"]) < 2:
            continue                                    # tunnels
        tram = ln["nature"] == "Tramway"
        spacing = 3.6 if tram else 4.4
        bed = (104, 104, 100) if tram else (118, 104, 88)
        for k in range(int(ln["tracks"])):
            c = _offset(ln["xy"], (k - (int(ln["tracks"]) - 1) / 2) * spacing)
            dr.line(px(c), fill=bed, width=w(3.0), joint="curve")
            for side in (-0.75, 0.75):
                rail = px(_offset(c, side))
                dr.line(rail, fill=(38, 38, 42), width=w(0.6))
                dr.line(rail, fill=(212, 212, 218), width=w(0.3))
    return np.asarray(im)


def render_tile(i: int, j: int, view=geo.View(0), margin_px: int = 0) -> Image.Image:
    """Render tile (i, j) plus margin_px pixels of context on every side."""
    T = geo.TILE_M
    mm = margin_px * geo.M_PER_PX
    pad = 4.0 + mm
    bbox = view.ground_bbox(i * T - pad, j * T - pad, (i + 1) * T + pad, (j + 1) * T + pad)
    mns, (gx0, gy1), res = ign.mosaic("mns", bbox)
    mnt, _, _ = ign.mosaic("mnt", bbox)
    bati, _, _ = ign.mosaic("bati", bbox)
    ortho, _, ores = ign.mosaic("ortho", bbox)
    mns, mnt = _clean(mns, mnt)
    lines = [ln for ln in ign.rails(bbox) if not ln["level"].startswith("-")]
    ortho = draw_rails(ortho, gx0, gy1, ores, lines)
    lc, _, _ = ign.mosaic("cls", bbox)
    bati = ndimage.binary_dilation(bati | (lc == 6), iterations=2)  # cover the wall ramps around footprints
    veg = ndimage.binary_dilation(lc == 5, iterations=2) & ~bati
    eau, _, _ = ign.mosaic("eau", bbox)
    water = (eau | (lc == 9)) & ~bati
    # bridges, boats and pontoons stand above the water: keep them, flatten the rest (waves, noise)
    above = lc == 17   # LiDAR bridge deck (MNT under water lies below the surface, so heights mislead)
    water &= ~ndimage.binary_dilation(above, iterations=1)
    mns = np.where(water, mnt, mns)
    cls = np.where(bati, 1.0, np.where(veg, 2.0, np.where(water, 3.0, 0.0))).astype("f4")

    ox, oy = geo.ORIGIN
    h, w = mns.shape
    xs = gx0 - ox + (np.arange(w) + 0.5) * res
    ys = gy1 - oy - (np.arange(h) + 0.5) * res
    X, Y = np.meshgrid(xs, ys)
    verts = np.stack([X, Y, mns, mnt, cls], axis=-1).astype("f4").reshape(-1, 5)
    idx = np.arange(h * w, dtype="u4").reshape(h, w)
    a, b, c, d = idx[:-1, :-1], idx[:-1, 1:], idx[1:, :-1], idx[1:, 1:]
    tris = np.stack([a, c, b, b, c, d], axis=-1).reshape(-1).astype("u4")

    ctx, prog = _ctx()
    r, dn, bk = view.axes
    prog["u_r"].value = tuple(r); prog["u_d"].value = tuple(dn); prog["u_b"].value = tuple(bk)
    cx, cy = (i + 0.5) * T, (j + 0.5) * T
    inv = np.linalg.inv(np.array([[r[0], r[1]], [dn[0], dn[1]]]))
    gx, gy = inv @ np.array([cx - r[2] * 10.0, cy - dn[2] * 10.0])   # ground point at the tile centre
    prog["u_view"].value = (cx, cy, T / 2 + mm, float(gx * bk[0] + gy * bk[1] + 10.0 * bk[2]))
    oh, ow = ortho.shape[:2]
    prog["u_obox"].value = (gx0 - ox, gy1 - oy, ow * ores, oh * ores)
    sun = np.array([-0.45, -0.75, 0.8]); sun /= np.linalg.norm(sun)
    prog["u_sun"].value = tuple(sun)

    tex = ctx.texture((ow, oh), 3, np.ascontiguousarray(ortho).tobytes())
    tex.build_mipmaps(); tex.filter = (moderngl.LINEAR_MIPMAP_LINEAR, moderngl.LINEAR)
    tex.use(0); prog["u_ortho"].value = 0
    vbo = ctx.buffer(verts.tobytes()); ibo = ctx.buffer(tris.tobytes())
    vao = ctx.vertex_array(prog, [(vbo, "2f 3f", "in_xy", "in_zgb")], ibo)
    size = geo.TILE_PX + 2 * margin_px
    n = size * SS
    fbo = ctx.simple_framebuffer((n, n), components=3)
    fbo.use(); ctx.enable(moderngl.DEPTH_TEST); fbo.clear(0.55, 0.62, 0.66, 1.0)
    vao.render(moderngl.TRIANGLES)
    img = Image.frombytes("RGB", (n, n), fbo.read(components=3)).transpose(Image.FLIP_TOP_BOTTOM)
    for o in (vao, vbo, ibo, tex, fbo):
        o.release()
    img = img.resize((size, size), Image.LANCZOS)
    # does any track cross the rendered square? (lets the redraw prompt mention rails only when true)
    img.info["rails"] = False
    for ln in lines:
        p = np.asarray(ln["xy"]) - np.array(geo.ORIGIN)
        sx, sy, _ = view.to_screen(p[:, 0], p[:, 1], np.full(len(p), 10.0))
        if ((np.abs(sx - cx) < T / 2 + mm) & (np.abs(sy - cy) < T / 2 + mm)).any():
            img.info["rails"] = True
            break
    return img


def main(argv):
    view = geo.View(0)
    if argv and argv[0] == "--area":
        tiles = view.tiles_for_area()
    else:
        nums = list(map(int, argv))
        tiles = list(zip(nums[0::2], nums[1::2]))
    out = ROOT / "out" / "h0" / "render"
    out.mkdir(parents=True, exist_ok=True)
    for k, (i, j) in enumerate(tiles):
        f = out / f"{i}_{j}.png"
        if f.exists():
            continue
        render_tile(i, j, view).save(f)
        print(f"[{k + 1}/{len(tiles)}] {f.name}", flush=True)


if __name__ == "__main__":
    main(sys.argv[1:])
