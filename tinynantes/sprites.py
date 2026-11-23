"""Sprites for the moving things (cars, buses, trams, Navibus), drawn in the same style as the map.

1. Each vehicle is modelled from simple parts (body, glass, wheels, lights, roof gear) in its own frame
   (x forward, y left, z up, metres).
2. It is rendered with the map's own camera (the heading's axes) in 16 directions, laid out on 1024 px sheets.
3. Each sheet is repainted by the same painter as the tiles (FLUX.2 klein edit), so the style, palette and
   detail match the city; then every sprite is cut back out with the mask of its 3D render.
4. web/data/sprites/hH/sheet_N.png + atlas.json: per sprite and direction, the cell and the ground anchor;
   the viewer picks the direction closest to the vehicle's heading.

Usage: uv run python -m tinynantes.sprites H [render|paint|all]
"""
from __future__ import annotations

import io
import json
import math
import sys
from pathlib import Path

import moderngl
import numpy as np
from PIL import Image, ImageFilter

from . import geo

ROOT = Path(__file__).resolve().parent.parent
DIRS = 16
SHEET, CELL = 1024, 128
SS = 3                                     # supersampling of the 3D render
BG = (200, 200, 196)

# ---------------------------------------------------------------- modelling


class Model:
    def __init__(self):
        self.tris = []                     # (3x3 vertices, rgb)

    def quad(self, a, b, c, d, col):
        self.tris += [((a, b, c), col), ((a, c, d), col)]

    def box(self, x0, x1, y0, y1, z0, z1, top, side=None, front=None, back=None, taper=0.0):
        """Axis-aligned box; `taper` shrinks the top face inwards (cabins, noses)."""
        side = side or top
        t = taper
        b = [(x0, y0, z0), (x1, y0, z0), (x1, y1, z0), (x0, y1, z0)]
        u = [(x0 + t, y0 + t * 0.5, z1), (x1 - t, y0 + t * 0.5, z1), (x1 - t, y1 - t * 0.5, z1), (x0 + t, y1 - t * 0.5, z1)]
        self.quad(*u, top)
        self.quad(b[0], b[1], u[1], u[0], side)          # right side (y0)
        self.quad(b[2], b[3], u[3], u[2], side)          # left side (y1)
        self.quad(b[1], b[2], u[2], u[1], front or side) # front (x1)
        self.quad(b[3], b[0], u[0], u[3], back or side)  # back (x0)

    def wheel(self, x, y, r=0.33, w=0.22, col=(0.09, 0.09, 0.1)):
        n = 10
        for k in range(n):
            a0, a1 = 2 * math.pi * k / n, 2 * math.pi * (k + 1) / n
            p = lambda a, yy: (x + r * math.cos(a), yy, r + r * math.sin(a))
            self.quad(p(a0, y - w / 2), p(a1, y - w / 2), p(a1, y + w / 2), p(a0, y + w / 2), col)
            for yy in (y - w / 2, y + w / 2):
                self.tris.append((((x, yy, r), p(a0, yy), p(a1, yy)), (0.16, 0.16, 0.17)))

    def mesh(self, yaw):
        c, s = math.cos(yaw), math.sin(yaw)
        V, C = [], []
        for tri, col in self.tris:
            pts = [(x * c - y * s, x * s + y * c, z) for x, y, z in tri]
            V += pts
            C += [col] * 3
        return np.asarray(V, "f4"), np.asarray(C, "f4")


GLASS = (0.17, 0.22, 0.27)
DARK = (0.14, 0.15, 0.16)
LIGHT = (1.0, 0.95, 0.78)
RED = (0.75, 0.1, 0.1)


def car(body, kind="hatch"):
    m = Model()
    L = {"hatch": 3.9, "sedan": 4.5, "van": 4.9, "suv": 4.5}[kind]
    W, h0 = 1.78, 0.28
    hb = 0.95 if kind != "van" else 1.0
    m.box(-L / 2, L / 2, -W / 2, W / 2, h0, h0 + 0.62, body, front=body, back=body)          # lower body
    m.box(-L / 2 - 0.05, -L / 2 + 0.1, -W / 2, W / 2, h0, h0 + 0.3, DARK)                     # rear bumper
    m.box(L / 2 - 0.1, L / 2 + 0.05, -W / 2, W / 2, h0, h0 + 0.3, DARK)                       # front bumper
    for yy in (-W / 2 + 0.2, W / 2 - 0.35):
        m.box(L / 2, L / 2 + 0.03, yy, yy + 0.15, h0 + 0.42, h0 + 0.55, LIGHT)                # headlights
        m.box(-L / 2 - 0.03, -L / 2, yy, yy + 0.15, h0 + 0.42, h0 + 0.55, RED)                # tail lights
    if kind == "van":
        m.box(-L / 2, L / 2 - 0.9, -W / 2 + 0.02, W / 2 - 0.02, h0 + 0.62, h0 + 1.65, body)
        m.box(L / 2 - 0.9, L / 2 - 0.4, -W / 2 + 0.05, W / 2 - 0.05, h0 + 0.62, h0 + 1.5, body, GLASS, front=GLASS, taper=0.25)
    else:
        x0, x1 = (-L / 2 + 0.45, L / 2 - 1.15) if kind != "hatch" else (-L / 2 + 0.15, L / 2 - 1.05)
        roof = tuple(min(1, c * 1.08) for c in body)
        m.box(x0, x1, -W / 2 + 0.08, W / 2 - 0.08, h0 + 0.62, h0 + 0.62 + (0.55 if kind != "suv" else 0.68),
              roof, GLASS, front=GLASS, back=GLASS, taper=0.22)
    for xx in (-L / 2 + 0.75, L / 2 - 0.75):
        for yy in (-W / 2 + 0.05, W / 2 - 0.05):
            m.wheel(xx, yy)
    return m


def coach_body(m, L, W, H, base, stripe, roof, glass_from=0.95, glass_to=None, x0=None, nose=0.0):
    """Bus or tram car: skirt, window band with pillars, roof."""
    x0 = -L / 2 if x0 is None else x0
    x1 = x0 + L
    glass_to = glass_to or H - 0.45
    m.box(x0, x1, -W / 2, W / 2, 0.3, glass_from, base, front=base, back=base)
    m.box(x0, x1, -W / 2 - 0.01, W / 2 + 0.01, 0.3, 0.55, stripe)                                # coloured skirt
    # window band: glass, with pillars every ~1.5 m
    m.box(x0 + 0.05, x1 - 0.05, -W / 2 + 0.02, W / 2 - 0.02, glass_from, glass_to, roof, GLASS, front=GLASS, back=GLASS)
    n = max(2, int(L / 1.6))
    for k in range(n + 1):
        px = x0 + 0.1 + k * (L - 0.3) / n
        for yy in (-W / 2, W / 2 - 0.06):
            m.box(px, px + 0.12, yy, yy + 0.06, glass_from, glass_to, base)
    m.box(x0, x1, -W / 2, W / 2, glass_to, H, roof, base, front=base, back=base, taper=nose)


def bus():
    m = Model()
    L, W, H = 12.0, 2.55, 3.05
    white, green, roof = (0.93, 0.94, 0.92), (0.18, 0.55, 0.42), (0.86, 0.88, 0.87)
    coach_body(m, L, W, H, white, green, roof)
    m.box(L / 2 - 0.05, L / 2, -W / 2 + 0.1, W / 2 - 0.1, 0.9, 2.6, GLASS)                       # windscreen
    m.box(-3.0, 1.0, -0.8, 0.8, H, H + 0.3, (0.7, 0.72, 0.72))                                    # roof units
    for xx in (-L / 2 + 2.6, L / 2 - 2.6):
        for yy in (-W / 2 + 0.05, W / 2 - 0.05):
            m.wheel(xx, yy, r=0.5, w=0.3)
    for yy in (-W / 2 + 0.25, W / 2 - 0.45):
        m.box(L / 2, L / 2 + 0.03, yy, yy + 0.2, 0.55, 0.75, LIGHT)
    return m


def tram(head):
    """One 8 m car of a Nantes tram (white, dark glazing, green skirt); the head car has the sloped nose."""
    m = Model()
    L, W, H = 8.0, 2.65, 3.3
    white, green, roof = (0.94, 0.95, 0.94), (0.16, 0.5, 0.38), (0.88, 0.9, 0.9)
    coach_body(m, L - (1.2 if head else 0), W, H, white, green, roof, x0=-L / 2)
    if head:
        x0 = L / 2 - 1.2
        m.box(x0, L / 2, -W / 2 + 0.05, W / 2 - 0.05, 0.3, 1.0, white, front=white)
        m.box(x0, L / 2 - 0.15, -W / 2 + 0.1, W / 2 - 0.1, 1.0, 2.75, roof, GLASS, front=GLASS, taper=0.4)  # nose glass
        for yy in (-W / 2 + 0.3, W / 2 - 0.5):
            m.box(L / 2, L / 2 + 0.03, yy, yy + 0.2, 0.6, 0.8, LIGHT)
    else:
        # pantograph on the middle cars
        m.box(-0.9, 0.9, -0.5, 0.5, H, H + 0.25, (0.55, 0.57, 0.58))
        m.box(-0.1, 0.1, -0.7, 0.7, H + 0.25, H + 0.9, (0.25, 0.26, 0.27))
    for yy in (-W / 2 + 0.05, W / 2 - 0.05):
        m.wheel(-L / 2 + 1.5, yy, r=0.38, w=0.2)
        m.wheel(L / 2 - 1.5, yy, r=0.38, w=0.2)
    return m


def navibus():
    m = Model()
    L, W = 18.0, 5.0
    hull, deck, blue = (0.95, 0.94, 0.9), (0.62, 0.6, 0.55), (0.16, 0.36, 0.56)
    m.box(-L / 2, L / 2, -W / 2, W / 2, -0.4, 0.6, deck, hull, front=hull, back=hull)
    m.box(-L / 2, L / 2, -W / 2 - 0.02, W / 2 + 0.02, -0.4, 0.0, blue)
    m.box(-4.5, 4.5, -1.8, 1.8, 0.6, 2.6, (0.97, 0.97, 0.96), GLASS, front=GLASS, back=GLASS)
    m.box(-4.7, 4.7, -2.0, 2.0, 2.6, 2.75, blue)
    for xx in (-L / 2 + 0.3, L / 2 - 0.5):
        m.box(xx, xx + 0.2, -W / 2, W / 2, 0.6, 1.5, (0.3, 0.3, 0.32))                           # gates
    return m


CAR_COLORS = {"white": (0.9, 0.9, 0.88), "black": (0.15, 0.16, 0.18), "grey": (0.58, 0.61, 0.65), "red": (0.72, 0.15, 0.14),
              "blue": (0.17, 0.33, 0.56), "silver": (0.78, 0.79, 0.8), "green": (0.28, 0.38, 0.25), "yellow": (0.85, 0.66, 0.2)}
CAR_KINDS = ["hatch", "sedan", "suv", "van"]


def catalogue():
    """(name, model, pixels per metre) -- vehicles of a sheet share a scale close to the map's, a bit larger."""
    out = []
    cars = [("white", "hatch"), ("black", "sedan"), ("grey", "suv"), ("red", "hatch"), ("blue", "sedan"), ("silver", "van"),
            ("green", "hatch"), ("yellow", "sedan")]
    for col, kind in cars:
        out.append((f"car_{col}_{kind}", car(CAR_COLORS[col], kind), 12.0))
    out.append(("bus", bus(), 7.0))
    out.append(("tram_head", tram(True), 9.0))
    out.append(("tram_mid", tram(False), 9.0))
    out.append(("navibus", navibus(), 4.8))
    return out


# ---------------------------------------------------------------- rendering

VS = """
#version 330
in vec3 in_p; in vec3 in_c; in vec3 in_n;
uniform vec3 u_r, u_d, u_b; uniform vec4 u_cell;   // cell origin x, y (px), pixels per metre, unused
uniform vec2 u_size;
out vec3 v_c; out vec3 v_n;
void main() {
    float sx = dot(in_p, u_r) * u_cell.z + u_cell.x, sy = dot(in_p, u_d) * u_cell.z + u_cell.y;
    gl_Position = vec4(sx / u_size.x * 2.0 - 1.0, 1.0 - sy / u_size.y * 2.0, -dot(in_p, u_b) / 100.0, 1.0);
    v_c = in_c; v_n = in_n;
}
"""
FS = """
#version 330
in vec3 v_c; in vec3 v_n; out vec4 f;
void main() {
    vec3 L = normalize(vec3(-0.45, -0.35, 0.82));
    float k = 0.58 + 0.42 * max(dot(normalize(v_n), L), 0.0);
    f = vec4(v_c * k, 1.0);
}
"""


def render_sheet(items, view):
    """items: [(name, model, ppm)]; each gets one row of 16 directions (two rows of 8 cells). Returns the
    RGB sheet, the mask and the atlas entries."""
    r, d, b = view.axes
    n = SHEET * SS
    ctx = moderngl.create_standalone_context(backend="egl")
    prog = ctx.program(vertex_shader=VS, fragment_shader=FS)
    prog["u_r"].value = tuple(r); prog["u_d"].value = tuple(d); prog["u_b"].value = tuple(b)
    prog["u_size"].value = (n, n)
    fbo = ctx.framebuffer([ctx.renderbuffer((n, n), 4)], ctx.depth_renderbuffer((n, n)))
    fbo.use(); ctx.enable(moderngl.DEPTH_TEST)
    fbo.clear(*[c / 255 for c in BG], 0.0)
    atlas = {}
    per_row = SHEET // CELL
    slot = 0
    for name, model, ppm in items:
        cells = []
        for k in range(DIRS):
            yaw = 2 * math.pi * k / DIRS
            V, C = model.mesh(yaw)
            tri = V.reshape(-1, 3, 3)
            nrm = np.cross(tri[:, 1] - tri[:, 0], tri[:, 2] - tri[:, 0])
            nrm /= np.linalg.norm(nrm, axis=1, keepdims=True) + 1e-9
            # face normals towards the camera side for double-sided quads
            flip = (nrm @ np.asarray(b, "f4")) < 0
            nrm[flip] *= -1
            N = np.repeat(nrm, 3, axis=0).astype("f4")
            cx, cy = (slot % per_row) * CELL, (slot // per_row) * CELL
            ax, ay = cx + CELL / 2, cy + CELL * 0.62          # where the vehicle's ground centre lands
            prog["u_cell"].value = (ax * SS, ay * SS, ppm * SS, 0.0)
            vbo = ctx.buffer(np.hstack([V, C, N]).astype("f4").tobytes())
            vao = ctx.vertex_array(prog, [(vbo, "3f 3f 3f", "in_p", "in_c", "in_n")])
            ctx.scissor = (int(cx * SS), int(n - (cy + CELL) * SS), CELL * SS, CELL * SS)
            vao.render()
            vao.release(); vbo.release()
            cells.append([cx, cy, CELL, CELL, ax - cx, ay - cy])
            slot += 1
        atlas[name] = {"ppm": ppm, "cells": cells}
    ctx.scissor = None
    raw = np.frombuffer(fbo.read(components=4, alignment=1), "u1").reshape(n, n, 4)[::-1]
    img = Image.fromarray(raw[..., :3].copy()).resize((SHEET, SHEET), Image.LANCZOS)
    mask = Image.fromarray(raw[..., 3].copy()).resize((SHEET, SHEET), Image.BILINEAR)
    ctx.release()
    return img, mask, atlas


# ---------------------------------------------------------------- painting

PROMPT = ("Repaint these 3D vehicle renders as detailed isometric pixel-art vehicles, in the same style as a pixel-art "
          "city map. Keep every vehicle exactly where it is, with the same shape, size, direction and colours. "
          "Keep the plain grey background. Do not add or remove anything.")


def paint(img, seed=1):
    from .stylize import load_env, stylize          # same painter as the tiles
    load_env()
    return stylize(img, model="9b", seed=seed, prompt=PROMPT)


def cut(painted, mask, render):
    """Painted pixels where the 3D render has the vehicle; the rest transparent. Falls back to the render
    where the painter wandered off (painted pixel still background grey)."""
    p = np.asarray(painted.convert("RGB").resize(render.size, Image.LANCZOS), "f4")
    m = np.asarray(mask.filter(ImageFilter.MaxFilter(3)), "f4") / 255.0
    bgdist = np.abs(p - np.asarray(BG, "f4")).sum(-1)
    lost = (bgdist < 18) & (m > 0.5)
    rgb = np.where(lost[..., None], np.asarray(render, "f4"), p)
    a = (np.clip(m * 1.4, 0, 1) * 255).astype("u1")
    return Image.fromarray(np.dstack([rgb.astype("u1"), a]), "RGBA")


def main(argv):
    h = int(argv[0]) if argv else 0
    what = argv[1] if len(argv) > 1 else "all"
    view = geo.View(h)
    out = ROOT / "web" / "data" / "sprites" / f"h{h}"
    work = ROOT / "out" / "sprites" / f"h{h}"
    out.mkdir(parents=True, exist_ok=True); work.mkdir(parents=True, exist_ok=True)
    items = catalogue()
    per_sheet = (SHEET // CELL) ** 2 // DIRS               # 4 vehicles per sheet
    atlas = {"dirs": DIRS, "sheets": []}
    for s in range(0, len(items), per_sheet):
        k = s // per_sheet
        img, mask, entries = render_sheet(items[s:s + per_sheet], view)
        img.save(work / f"render_{k}.png"); mask.save(work / f"mask_{k}.png")
        if what in ("paint", "all"):
            pfile = work / f"paint_{k}.png"
            if not pfile.exists():
                paint(img).save(pfile)
            sprite = cut(Image.open(pfile), mask, img)
        else:
            sprite = cut(img, mask, img)
        sprite.save(out / f"sheet_{k}.png", optimize=True)
        for name, e in entries.items():
            e["sheet"] = k
            atlas[name] = e
        atlas["sheets"].append(f"sheet_{k}.png")
        print(f"sheet {k}: {', '.join(entries)}", flush=True)
    (out / "atlas.json").write_text(json.dumps(atlas))


if __name__ == "__main__":
    main(sys.argv[1:])
