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

    def ring(self, x, r, w, tyre, rim, spokes=4):
        """A bicycle wheel in the x-z plane at y = 0: a tyre ring (see-through middle), thin spokes, a hub."""
        n = 16
        p = lambda a, rr, yy: (x + rr * math.cos(a), yy, r + rr * math.sin(a))
        for k in range(n):
            a0, a1 = 2 * math.pi * k / n, 2 * math.pi * (k + 1) / n
            self.quad(p(a0, r, -w / 2), p(a1, r, -w / 2), p(a1, r, w / 2), p(a0, r, w / 2), tyre)
            for yy in (-w / 2, w / 2):
                self.quad(p(a0, r * 0.72, yy), p(a1, r * 0.72, yy), p(a1, r, yy), p(a0, r, yy), tyre)
                self.quad(p(a0, r * 0.62, yy), p(a1, r * 0.62, yy), p(a1, r * 0.72, yy), p(a0, r * 0.72, yy), rim)
        for k in range(spokes):
            a = math.pi * k / spokes
            t = r * 0.05
            c, s_ = math.cos(a), math.sin(a)
            q = lambda u, v: (x + u * c - v * s_, 0.0, r + u * s_ + v * c)
            self.quad(q(-r * 0.62, -t), q(r * 0.62, -t), q(r * 0.62, t), q(-r * 0.62, t), rim)
        self.box(x - r * 0.12, x + r * 0.12, -w / 2, w / 2, r * 0.88, r * 1.12, rim)

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


def prism(m, prof, w, cols, side, w_top=None, z_ref=None):
    """A solid from a convex side profile [(x, z)] (counter-clockwise seen from +y), extruded across the car:
    half-width `w` at the bottom of the profile, narrowing to `w_top` at its top (tumblehome). cols[k] colours
    the face from point k to k+1 (None: skipped); `side` colours both flanks."""
    zs = [z for _, z in prof]
    z0, z1 = min(zs), max(zs)
    w_top = w if w_top is None else w_top
    hw = lambda z: w + (w_top - w) * ((z - z0) / (z1 - z0) if z1 > z0 else 0.0)
    L = [(x, -hw(z), z) for x, z in prof]
    R = [(x, hw(z), z) for x, z in prof]
    for k in range(1, len(prof) - 1):
        m.tris.append(((L[0], L[k + 1], L[k]), side))
        m.tris.append(((R[0], R[k], R[k + 1]), side))
    for k in range(len(prof)):
        j = (k + 1) % len(prof)
        if cols[k] is not None:
            m.quad(L[k], L[j], R[j], R[k], cols[k])


def shade3(c, k):
    return tuple(min(1.0, v * k) for v in c)


CHROME = (0.62, 0.64, 0.66)
PLATE = (0.9, 0.9, 0.86)


def car(body, kind="hatch"):
    """A small car seen from above at 12 px/m: lower body with a sloped bonnet, a glasshouse with raked
    windscreen and rear window, pillars, wheels in dark arches, lights and plates."""
    m = Model()
    L = {"hatch": 3.95, "sedan": 4.5, "van": 4.6, "suv": 4.5}[kind]
    W = 1.78
    h0, belt = 0.3, 0.92 if kind != "suv" else 1.02
    roof = {"hatch": 1.45, "sedan": 1.42, "van": 1.47, "suv": 1.62}[kind]
    hood = {"hatch": 0.95, "sedan": 1.15, "van": 1.0, "suv": 1.05}[kind]
    xf, xr = L / 2, -L / 2
    dark = DARK
    # lower body: bumper, bonnet sloping up to the scuttle, flat waist, boot / tailgate
    lo = [(xr, h0), (xf, h0), (xf, h0 + 0.38), (xf - 0.22, belt - 0.12), (xf - hood, belt), (xr + 0.12, belt), (xr, belt - 0.2)]
    prism(m, lo, W / 2, [None, dark, body, shade3(body, 1.04), body, body, body], body)
    # sill and bumpers in dark plastic, a touch proud of the body
    m.box(xr + 0.55, xf - 0.55, -W / 2 - 0.01, W / 2 + 0.01, h0, h0 + 0.12, dark)
    m.box(xf - 0.12, xf + 0.04, -W / 2 + 0.02, W / 2 - 0.02, h0, h0 + 0.2, dark)
    m.box(xr - 0.04, xr + 0.12, -W / 2 + 0.02, W / 2 - 0.02, h0, h0 + 0.2, dark)
    # glasshouse
    ws = xf - hood                                     # foot of the windscreen
    rake = 0.62 if kind != "suv" else 0.5
    back_foot = {"hatch": xr + 0.12, "sedan": xr + 0.85, "van": xr + 0.1, "suv": xr + 0.12}[kind]
    back_top = {"hatch": xr + 0.4, "sedan": xr + 1.4, "van": xr + 0.14, "suv": xr + 0.25}[kind]
    gh = [(back_foot, belt), (ws, belt), (ws - rake, roof), (back_top, roof)]
    roofc = shade3(body, 1.08)
    prism(m, gh, W / 2 - 0.06, [None, GLASS, roofc, GLASS], GLASS, w_top=W / 2 - 0.2)
    # pillars, in the body colour, just outside the side glass (A, B and C)
    def pillar(xa, xb, t=0.12):
        for sgn in (-1, 1):
            ya = sgn * (W / 2 - 0.05); yb = sgn * (W / 2 - 0.19)
            m.quad((xa, ya, belt), (xa + t, ya, belt), (xb + t, yb, roof), (xb, yb, roof), body)
            m.quad((xa + t, ya, belt), (xa, ya, belt), (xb, yb, roof), (xb + t, yb, roof), body)
    pillar(ws - 0.1, ws - rake - 0.02)
    mid = (ws - rake + back_top) / 2 + (0.15 if kind == "sedan" else 0.0)
    pillar(mid, mid, 0.14)
    pillar(back_foot, back_top - 0.06, 0.22 if kind != "sedan" else 0.14)
    # mirrors
    for sgn in (-1, 1):
        m.box(ws - 0.25, ws - 0.08, sgn * (W / 2 + 0.12) - 0.07, sgn * (W / 2 + 0.12) + 0.07, belt - 0.05, belt + 0.1, body)
    # lights and plates
    for sgn in (-1, 1):
        y0 = sgn * (W / 2 - 0.12)
        m.box(xf - 0.2, xf + 0.005, min(y0, y0 - sgn * 0.38), max(y0, y0 - sgn * 0.38), h0 + 0.3, h0 + 0.42, LIGHT)
        m.box(xr - 0.005, xr + 0.1, min(y0, y0 - sgn * 0.3), max(y0, y0 - sgn * 0.3), belt - 0.38, belt - 0.24, RED)
    m.box(xf + 0.04, xf + 0.05, -0.25, 0.25, h0 + 0.05, h0 + 0.17, PLATE)
    m.box(xr - 0.05, xr - 0.04, -0.25, 0.25, h0 + 0.25, h0 + 0.37, PLATE)
    # wheels, in dark arches
    for xx in (xr + 0.78, xf - 0.78):
        for sgn in (-1, 1):
            yy = sgn * (W / 2 - 0.02)
            m.quad((xx - 0.45, yy * 1.004, h0), (xx + 0.45, yy * 1.004, h0), (xx + 0.32, yy * 1.004, h0 + 0.42), (xx - 0.32, yy * 1.004, h0 + 0.42), (0.08, 0.08, 0.09))
            m.wheel(xx, sgn * (W / 2 - 0.08), r=0.33, w=0.2)
            m.box(xx - 0.12, xx + 0.12, sgn * (W / 2 - 0.02) - 0.005, sgn * (W / 2 - 0.02) + 0.005, 0.21, 0.45, CHROME)
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


def person(coat, legs=(0.18, 0.2, 0.26), phase=0.0, bike=False, skin=(0.93, 0.76, 0.62), hair=(0.25, 0.17, 0.1),
           style=0):
    """A walker (legs apart by `phase`) or a cyclist, slightly larger than life (x1.6) so they read on the map.
    Proportions a touch stylised (bigger head) so a face, hair and hands survive at 20 px tall.
    style: 0 short hair + trousers, 1 long hair + skirt, 2 cap + trousers, 3 bun + trousers and a bag."""
    m = Model()
    K = 1.6
    shoe = (0.1, 0.09, 0.09)
    if bike:
        frame = (0.12, 0.13, 0.14)
        paint_ = FRAMES[COATS.index(coat) % len(FRAMES)] if coat in COATS else (0.7, 0.72, 0.75)
        m.ring(-0.55 * K, 0.33 * K, 0.06 * K, (0.08, 0.08, 0.09), (0.7, 0.72, 0.74))
        m.ring(0.55 * K, 0.33 * K, 0.06 * K, (0.08, 0.08, 0.09), (0.7, 0.72, 0.74))
        m.box(-0.55 * K, 0.42 * K, -0.035 * K, 0.035 * K, 0.36 * K, 0.44 * K, paint_)                   # down tube
        m.box(-0.3 * K, -0.22 * K, -0.035 * K, 0.035 * K, 0.33 * K, 0.5 * K, paint_)                    # seat tube
        m.box(0.4 * K, 0.48 * K, -0.035 * K, 0.035 * K, 0.33 * K, 0.85 * K, paint_)                     # fork
        m.box(0.42 * K, 0.5 * K, -0.25 * K, 0.25 * K, 0.85 * K, 0.9 * K, frame)                         # handlebar
        m.box(-0.3 * K, -0.12 * K, -0.07 * K, 0.07 * K, 0.5 * K, 0.55 * K, DARK)                         # saddle
        z0 = 0.5 * K
        for side in (-1, 1):                                                                             # legs on pedals
            m.box(-0.2 * K, 0.08 * K, side * 0.08 * K - 0.05 * K, side * 0.08 * K + 0.05 * K, z0 - 0.15 * K, z0 + 0.05 * K, legs)
            m.box(0.02 * K, 0.14 * K, side * 0.08 * K - 0.05 * K, side * 0.08 * K + 0.05 * K, z0 - 0.4 * K, z0 - 0.1 * K, legs)
            m.box(0.04 * K, 0.18 * K, side * 0.08 * K - 0.05 * K, side * 0.08 * K + 0.05 * K, z0 - 0.45 * K, z0 - 0.38 * K, shoe)
        zt = z0 + 0.08 * K
    else:
        z0 = 0.0
        LEG = 0.72 * K
        for side, sgn in ((-1, 1), (1, -1)):
            dx = sgn * phase * 0.15 * K
            y0, y1 = side * 0.09 * K - 0.06 * K, side * 0.09 * K + 0.06 * K
            col = skin if style == 1 else legs
            m.box(dx - 0.065 * K, dx + 0.065 * K, y0, y1, 0.06 * K, LEG, col)                            # leg
            m.box(dx - 0.06 * K, dx + 0.12 * K, y0 - 0.005, y1 + 0.005, 0, 0.08 * K, shoe)               # shoe
        if style == 1:                                                                                 # skirt
            m.box(-0.14 * K, 0.14 * K, -0.2 * K, 0.2 * K, LEG - 0.3 * K, LEG + 0.02 * K, legs, taper=0.04 * K)
        else:
            m.box(-0.12 * K, 0.12 * K, -0.19 * K, 0.19 * K, LEG - 0.08 * K, LEG + 0.02 * K, legs)       # hips
        zt = LEG
    lean = 0.12 * K if bike else 0.0
    T = 0.5 * K                                                                                        # torso height
    m.box(-0.12 * K + lean, 0.12 * K + lean, -0.2 * K, 0.2 * K, zt, zt + T, coat, taper=0.03 * K)       # torso, shoulders
    m.box(-0.125 * K + lean, 0.125 * K + lean, -0.205 * K, 0.205 * K, zt, zt + 0.06 * K, tuple(c * 0.75 for c in coat))  # hem
    m.box(0.11 * K + lean, 0.125 * K + lean, -0.03 * K, 0.03 * K, zt + 0.12 * K, zt + T - 0.04 * K, tuple(c * 0.8 for c in coat))  # zip
    for side in (-1, 1):                                                                               # arms + hands
        dx = (-side * phase * 0.13 * K) if not bike else 0.22 * K
        y0, y1 = side * 0.25 * K - 0.055 * K, side * 0.25 * K + 0.055 * K
        m.box(dx - 0.055 * K + lean, dx + 0.055 * K + lean, y0, y1, zt + 0.12 * K, zt + T - 0.02 * K, coat)
        m.box(dx - 0.05 * K + lean, dx + 0.05 * K + lean, y0 + 0.005, y1 - 0.005, zt + 0.04 * K, zt + 0.13 * K, skin)
    if style == 3 and not bike:                                                                        # shoulder bag
        m.box(-0.2 * K, -0.08 * K, 0.2 * K, 0.27 * K, zt + 0.05 * K, zt + 0.25 * K, (0.45, 0.3, 0.18))
    hz = zt + T
    m.box(-0.05 * K + lean, 0.05 * K + lean, -0.05 * K, 0.05 * K, hz, hz + 0.05 * K, skin)             # neck
    H0, H1 = hz + 0.05 * K, hz + 0.33 * K
    face = tuple(min(1.0, c * 1.04) for c in skin)
    m.box(-0.13 * K + lean, 0.13 * K + lean, -0.13 * K, 0.13 * K, H0, H1, skin, front=face)            # head
    for side in (-1, 1):                                                                               # eyes
        m.box(0.125 * K + lean, 0.14 * K + lean, side * 0.055 * K - 0.022 * K, side * 0.055 * K + 0.022 * K,
              H0 + 0.15 * K, H0 + 0.2 * K, (0.08, 0.07, 0.07))
    m.box(-0.14 * K + lean, 0.1 * K + lean, -0.14 * K, 0.14 * K, H1 - 0.08 * K, H1 + 0.03 * K, hair)  # hair on top
    m.box(-0.145 * K + lean, -0.06 * K + lean, -0.14 * K, 0.14 * K, H0 + 0.08 * K, H1, hair)          # back of the head
    if style == 1:
        m.box(-0.15 * K + lean, -0.07 * K + lean, -0.15 * K, 0.15 * K, hz - 0.12 * K, H1 - 0.05 * K, hair)  # long hair
    elif style == 2:
        cap = tuple(c * 0.7 for c in coat)
        m.box(-0.14 * K + lean, 0.14 * K + lean, -0.14 * K, 0.14 * K, H1 - 0.02 * K, H1 + 0.08 * K, cap)
        m.box(0.12 * K + lean, 0.25 * K + lean, -0.11 * K, 0.11 * K, H1 - 0.02 * K, H1 + 0.01 * K, cap)   # peak
    elif style == 3:
        m.box(-0.2 * K + lean, -0.11 * K + lean, -0.06 * K, 0.06 * K, H1 - 0.1 * K, H1 + 0.02 * K, hair)  # bun
    return m


def lying(coat, legs, skin, hair, style, k):
    """Someone knocked down: the walker on their back on the road, one arm out, one knee bent; centred."""
    m = person(coat, legs=legs, phase=0.35, skin=skin, hair=hair, style=style)
    H = 1.75 * 1.6
    # standing frame (x forward, z up) -> lying on the back: head towards +x, front (x) up
    c, s_ = math.cos(0.25 if k % 2 else -0.2), math.sin(0.25 if k % 2 else -0.2)
    def tf(v):
        x, y, z = v
        X, Z = z - H / 2, x + 0.2 * 1.6
        return (X * c - y * s_, X * s_ + y * c, Z * 0.85)
    m.tris = [(tuple(tf(v) for v in tri), col) for tri, col in m.tris]
    return m


COATS = [(0.72, 0.18, 0.16), (0.17, 0.38, 0.6), (0.85, 0.62, 0.2), (0.25, 0.45, 0.3), (0.42, 0.3, 0.6), (0.86, 0.84, 0.8),
         (0.16, 0.17, 0.2), (0.75, 0.42, 0.52)]
FRAMES = [(0.15, 0.45, 0.62), (0.85, 0.85, 0.82), (0.2, 0.5, 0.3), (0.72, 0.18, 0.16), (0.9, 0.75, 0.2),
          (0.2, 0.22, 0.25), (0.6, 0.62, 0.65), (0.15, 0.3, 0.55)]
HAIRS = [(0.25, 0.17, 0.1), (0.1, 0.08, 0.07), (0.75, 0.6, 0.3), (0.55, 0.3, 0.15)]
SKINS = [(0.93, 0.76, 0.62), (0.62, 0.43, 0.3), (0.98, 0.84, 0.72), (0.42, 0.28, 0.2), (0.8, 0.6, 0.45)]
LEGS = [(0.18, 0.2, 0.26), (0.24, 0.3, 0.45), (0.15, 0.15, 0.16), (0.42, 0.36, 0.28), (0.2, 0.28, 0.42), (0.3, 0.3, 0.32),
        (0.55, 0.47, 0.36), (0.18, 0.2, 0.26)]
STYLES = [0, 1, 2, 3, 1, 0, 3, 2]


def looks(k):
    """Coat, trousers, skin, hair and style of person k (walkers, cyclists and the knocked-down share them)."""
    return COATS[k], LEGS[k], SKINS[(k * 3) % len(SKINS)], HAIRS[k % 4], STYLES[k]

CAR_COLORS = {"white": (0.9, 0.9, 0.88), "black": (0.15, 0.16, 0.18), "grey": (0.58, 0.61, 0.65), "red": (0.72, 0.15, 0.14),
              "blue": (0.17, 0.33, 0.56), "silver": (0.78, 0.79, 0.8), "green": (0.28, 0.38, 0.25), "yellow": (0.85, 0.66, 0.2)}
CAR_KINDS = ["hatch", "sedan", "suv", "van"]


def helicopter(pitch_deg=0.0):
    """A light twin helicopter (red, white belly stripe), centred on its length, without the main rotor (the
    viewer spins that). pitch_deg > 0 = nose down, the attitude of forward flight."""
    m = Model()
    R, W, G2 = (0.78, 0.13, 0.12), (0.92, 0.92, 0.9), (0.82, 0.82, 0.8)
    m.box(-1.6, 1.4, -0.8, 0.8, 0.55, 1.4, R, front=R, back=R)                     # cabin
    m.box(-1.62, 1.42, -0.82, 0.82, 0.55, 0.78, W)                                 # belly stripe
    m.box(-1.45, 1.1, -0.74, 0.74, 1.4, 2.0, R, taper=0.22)                        # cabin roof
    m.box(-0.1, 1.2, -0.83, 0.83, 0.95, 1.65, GLASS)                               # side windows
    m.box(1.3, 2.4, -0.72, 0.72, 0.62, 1.85, GLASS, front=GLASS, taper=0.42)       # nose canopy
    m.box(-1.25, 0.45, -0.46, 0.46, 2.0, 2.45, G2, taper=0.12)                     # engine cowling
    m.box(-0.12, 0.12, -0.12, 0.12, 2.45, 2.72, DARK)                              # mast
    m.box(-0.32, 0.32, -0.32, 0.32, 2.72, 2.84, DARK)                              # hub
    m.box(-5.0, -1.5, -0.24, 0.24, 1.2, 1.68, R, taper=0.06)                       # tail boom
    m.box(-4.3, -3.9, -0.95, 0.95, 1.32, 1.42, R)                                  # stabiliser
    m.box(-5.4, -4.8, -0.07, 0.07, 1.35, 2.75, R, taper=0.1)                       # fin
    m.box(-5.25, -4.85, 0.08, 0.12, 1.55, 2.55, DARK)                              # tail rotor disc
    for yy in (-0.95, 0.95):
        m.box(-1.9, 1.9, yy - 0.06, yy + 0.06, 0.0, 0.1, DARK)                     # skids
        for xx in (-1.0, 0.9):
            m.box(xx, xx + 0.1, yy - 0.04, yy + 0.04, 0.1, 0.6, DARK)              # struts
    p, cx, cz = math.radians(pitch_deg), -1.5, 1.2                                 # pitch about the middle
    c, s_ = math.cos(p), math.sin(p)
    def tf(v):
        x, y, z = v[0] - cx, v[1], v[2] - cz
        return (x * c + z * s_, y, -x * s_ + z * c + cz)
    m.tris = [(tuple(tf(v) for v in tri), col) for tri, col in m.tris]
    return m


def _slab(m, pts, z0, z1, top, side):
    """A flat prism over a ground polygon (wings, tailplanes): top face, bottom face and edges."""
    n = len(pts)
    lo = [(x, y, z0) for x, y in pts]; hi = [(x, y, z1) for x, y in pts]
    for k in range(1, n - 1):
        m.tris.append(((hi[0], hi[k], hi[k + 1]), top))
        m.tris.append(((lo[0], lo[k + 1], lo[k]), side))
    for k in range(n):
        m.quad(lo[k], lo[(k + 1) % n], hi[(k + 1) % n], hi[k], side)


def _fin(m, pts, y0, y1, col, side):
    """A vertical flat prism (fins) over an (x, z) polygon, between y0 and y1."""
    n = len(pts)
    a = [(x, y0, z) for x, z in pts]; b = [(x, y1, z) for x, z in pts]
    for k in range(1, n - 1):
        m.tris.append(((a[0], a[k], a[k + 1]), side))
        m.tris.append(((b[0], b[k + 1], b[k]), side))
    for k in range(n):
        m.quad(a[k], a[(k + 1) % n], b[(k + 1) % n], b[k], col)


def airliner():
    """A narrow-body jet (A320 class): 37 m long, 34 m span; z = 0 under the fuselage."""
    m = Model()
    W, G, B, E = (0.94, 0.94, 0.93), (0.72, 0.74, 0.77), (0.16, 0.3, 0.6), (0.6, 0.62, 0.66)
    m.box(-15.0, 14.0, -1.95, 1.95, 0.0, 4.0, W, taper=0.35)                          # fuselage
    m.box(14.0, 18.5, -1.75, 1.75, 0.5, 3.6, W, front=W, taper=1.1)                    # nose
    m.box(16.0, 17.6, -1.5, 1.5, 2.6, 3.4, GLASS, front=GLASS, taper=0.3)              # cockpit windows
    m.box(-19.5, -15.0, -1.4, 1.4, 1.2, 3.9, W, back=W, taper=0.6)                     # tail cone
    m.box(-12.0, 12.0, -1.97, 1.97, 2.5, 2.8, GLASS)                                   # window line
    m.box(-15.0, 14.0, -1.97, 1.97, 0.0, 0.9, B)                                       # belly band
    for sgn in (-1, 1):
        _slab(m, [(4.5, sgn * 1.9), (-1.5, sgn * 1.9), (-7.5, sgn * 17.0), (-5.6, sgn * 17.0)], 0.9, 1.3, G, E)
        _slab(m, [(-15.5, sgn * 1.2), (-18.5, sgn * 1.2), (-21.0, sgn * 6.3), (-19.6, sgn * 6.3)], 3.4, 3.6, G, E)
        y = sgn * 5.7
        m.box(0.8, 5.2, y - 0.95, y + 0.95, -1.0, 0.9, E, front=DARK, taper=0.1)     # engines
    _fin(m, [(-14.5, 3.8), (-19.8, 3.8), (-21.0, 11.2), (-18.8, 11.2)], -0.18, 0.18, B, B)
    return m


def regional():
    """A high-wing turboprop (ATR 72 class): 27 m long and wide, T tail."""
    m = Model()
    W, G, B, E = (0.95, 0.95, 0.94), (0.74, 0.76, 0.79), (0.15, 0.45, 0.7), (0.55, 0.57, 0.6)
    m.box(-10.0, 9.5, -1.4, 1.4, 0.0, 2.9, W, taper=0.25)                             # fuselage
    m.box(9.5, 13.0, -1.25, 1.25, 0.3, 2.6, W, front=W, taper=0.9)                     # nose
    m.box(11.0, 12.4, -1.05, 1.05, 1.9, 2.5, GLASS, front=GLASS, taper=0.2)
    m.box(-14.0, -10.0, -1.0, 1.0, 1.0, 2.8, W, back=W, taper=0.5)                     # tail cone
    m.box(-8.0, 8.0, -1.42, 1.42, 1.7, 1.95, GLASS)
    m.box(-10.0, 9.5, -1.42, 1.42, 0.0, 0.6, B)
    for sgn in (-1, 1):
        _slab(m, [(3.0, sgn * 1.3), (0.6, sgn * 1.3), (-0.2, sgn * 13.5), (1.6, sgn * 13.5)], 2.9, 3.2, G, E)
        y = sgn * 4.1
        m.box(0.0, 4.6, y - 0.6, y + 0.6, 2.0, 3.1, E, front=DARK, taper=0.1)        # nacelles
        m.box(4.6, 4.75, y - 1.8, y + 1.8, 2.5, 2.6, DARK)                             # propeller blur
        m.box(4.6, 4.75, y - 0.1, y + 0.1, 0.8, 4.3, DARK)
        _slab(m, [(-13.0, sgn * 0.2), (-15.2, sgn * 0.2), (-15.8, sgn * 3.9), (-14.5, sgn * 3.9)], 7.6, 7.8, G, E)
    _fin(m, [(-10.5, 2.6), (-14.2, 2.6), (-15.5, 7.8), (-13.4, 7.8)], -0.15, 0.15, B, B)
    return m


def rail_car(livery, head=False, L=25.0):
    """An SNCF coach (or power car with a nose when head), x forward; livery = (body, band, roof)."""
    m = Model()
    body, band, roof = livery
    W, H = 2.9, 4.0
    x1 = L / 2 - (5.0 if head else 0.0)
    m.box(-L / 2, x1, -W / 2, W / 2, 0.5, 0.5 + H * 0.38, band, front=band, back=band)
    m.box(-L / 2, x1, -W / 2, W / 2, 0.5 + H * 0.38, 0.5 + H * 0.68, body, GLASS if not head else body)
    m.box(-L / 2, x1, -W / 2, W / 2, 0.5 + H * 0.68, 0.5 + H, roof, body, front=body, back=body, taper=0.15)
    if head:
        m.box(x1, L / 2, -W / 2, W / 2, 0.5, 0.5 + H * 0.38, band, front=band, taper=0.9)
        m.box(x1, L / 2 - 1.4, -W / 2 + 0.1, W / 2 - 0.1, 0.5 + H * 0.38, 0.5 + H * 0.85, body, GLASS, front=GLASS, taper=1.2)
        m.box(-L / 2 + 3, -L / 2 + 7, -0.4, 0.4, 0.5 + H, 0.5 + H + 0.5, DARK)       # pantograph base
    for xx in (-L / 2 + 3, L / 2 - 3 - (2 if head else 0)):
        m.box(xx - 1.4, xx + 1.4, -W / 2 + 0.2, W / 2 - 0.2, 0.0, 0.5, DARK)          # bogies
    return m


def _lightbar(m, x, z, w=1.2):
    """Roof light bar: two blue domes on a dark base."""
    m.box(x - 0.25, x + 0.25, -w / 2, w / 2, z, z + 0.08, DARK)
    for yy in (-w / 2 + 0.05, 0.05):
        m.box(x - 0.2, x + 0.2, yy, yy + w / 2 - 0.1, z + 0.08, z + 0.24, (0.15, 0.35, 0.95))


def van_body(body, band=None, band2=None, L=5.6, H=2.55, W=1.95):
    """A tall panel van (ambulance, fire VSAV, delivery): cab with windscreen, box body."""
    m = Model()
    h0 = 0.3
    m.box(-L / 2, L / 2 - 0.9, -W / 2, W / 2, h0, H, body, front=body, back=body)
    m.box(L / 2 - 0.9, L / 2, -W / 2 + 0.03, W / 2 - 0.03, h0, h0 + 0.85, body, front=body)
    m.box(L / 2 - 0.9, L / 2 - 0.15, -W / 2 + 0.06, W / 2 - 0.06, h0 + 0.85, H - 0.35, body, GLASS, front=GLASS, taper=0.3)
    if band:
        m.box(-L / 2 - 0.01, L / 2 - 0.9, -W / 2 - 0.01, W / 2 + 0.01, h0 + 0.55, h0 + 0.85, band)
    if band2:
        m.box(-L / 2 - 0.01, L / 2 - 0.9, -W / 2 - 0.01, W / 2 + 0.01, h0 + 1.05, h0 + 1.2, band2)
    for yy in (-W / 2 + 0.2, W / 2 - 0.35):
        m.box(L / 2, L / 2 + 0.03, yy, yy + 0.15, h0 + 0.42, h0 + 0.55, LIGHT)
        m.box(-L / 2 - 0.03, -L / 2, yy, yy + 0.15, h0 + 0.42, h0 + 0.65, RED)
    for xx in (-L / 2 + 0.9, L / 2 - 1.0):
        for yy in (-W / 2 + 0.05, W / 2 - 0.05):
            m.wheel(xx, yy, r=0.36)
    return m


def ambulance():
    m = van_body((0.96, 0.96, 0.94), band=(0.98, 0.55, 0.1), band2=(0.15, 0.35, 0.8))
    _lightbar(m, 1.6, 2.55)
    _lightbar(m, -2.4, 2.55, w=0.8)
    return m


def fire_engine():
    m = van_body((0.78, 0.1, 0.08), band=(0.95, 0.95, 0.92), L=6.4, H=2.8, W=2.2)
    m.box(-3.0, 1.6, -0.9, 0.9, 2.8, 2.95, (0.7, 0.72, 0.74))          # ladder rack
    _lightbar(m, 2.0, 2.8)
    return m


def police_car():
    m = car((0.95, 0.95, 0.95), "sedan")
    m.box(-2.24, 2.22, -0.9, 0.9, 0.48, 0.64, (0.12, 0.2, 0.55))                 # blue band
    m.box(-2.24, 2.22, -0.905, 0.905, 0.64, 0.69, (0.8, 0.12, 0.12))           # red pinstripe
    _lightbar(m, -0.2, 0.28 + 0.62 + 0.55)
    return m


def samu_car():
    m = car((0.96, 0.96, 0.94), "suv")
    m.box(-2.24, 2.22, -0.9, 0.9, 0.5, 0.68, (0.98, 0.55, 0.1))
    _lightbar(m, -0.2, 0.28 + 0.62 + 0.68)
    return m


def box_truck():
    m = Model()
    W, h0 = 2.3, 0.4
    m.box(-4.0, 2.0, -W / 2, W / 2, h0 + 0.3, 3.4, (0.93, 0.93, 0.9), front=(0.85, 0.85, 0.82), back=(0.8, 0.8, 0.78))
    m.box(2.0, 3.6, -W / 2 + 0.05, W / 2 - 0.05, h0, h0 + 1.1, (0.2, 0.36, 0.62), front=(0.2, 0.36, 0.62))
    m.box(2.1, 3.4, -W / 2 + 0.1, W / 2 - 0.1, h0 + 1.1, 2.7, (0.2, 0.36, 0.62), GLASS, front=GLASS, taper=0.15)
    for yy in (-W / 2 + 0.2, W / 2 - 0.35):
        m.box(3.6, 3.63, yy, yy + 0.15, h0 + 0.5, h0 + 0.65, LIGHT)
        m.box(-4.03, -4.0, yy, yy + 0.15, h0 + 0.5, h0 + 0.75, RED)
    for xx in (-3.0, -1.9, 2.6):
        for yy in (-W / 2 + 0.05, W / 2 - 0.05):
            m.wheel(xx, yy, r=0.45, w=0.3)
    return m


def _cab(m, x0, x1, W, H, col):
    """A lorry cab facing +x: body, windscreen and side windows, bumper, lights."""
    m.box(x0, x1, -W / 2, W / 2, 0.45, 1.3, col, front=col)
    m.box(x0, x1 - 0.12, -W / 2 + 0.04, W / 2 - 0.04, 1.3, H, col, GLASS, front=GLASS, taper=0.12)
    m.box(x0 + 0.05, x1 - 0.5, -W / 2 + 0.03, W / 2 - 0.03, 1.3, 1.95, col, GLASS, back=col)   # side windows
    m.box(x1 - 0.06, x1 + 0.06, -W / 2 + 0.05, W / 2 - 0.05, 0.4, 0.62, DARK)
    for yy in (-W / 2 + 0.15, W / 2 - 0.4):
        m.box(x1, x1 + 0.03, yy, yy + 0.25, 0.75, 0.9, LIGHT)


def tow_truck(loaded=False):
    """La fourrière: a yellow cab and a flat bed (with a burnt-out car on it on the way back)."""
    m = Model()
    W, Y = 2.25, (0.95, 0.66, 0.0)
    _cab(m, 1.2, 3.25, W, 2.45, Y)
    m.box(-0.15, 0.25, -0.12, 0.12, 2.45, 2.6, DARK)
    m.box(-0.4, 0.4, -0.5, 0.5, 2.6, 2.72, (1.0, 0.62, 0.0))                       # beacon
    m.box(-3.3, 1.15, -W / 2, W / 2, 0.45, 0.95, (0.36, 0.38, 0.41), back=(0.22, 0.23, 0.25))   # bed
    m.box(-3.3, 1.15, -W / 2 - 0.02, W / 2 + 0.02, 0.6, 0.75, (0.95, 0.66, 0.0))    # chevron band
    if loaded:
        w = car((0.12, 0.11, 0.1), "hatch")
        w.tris = [(tuple((x - 1.1, y, z + 0.67) for x, y, z in tri), tuple(c * 0.55 for c in col)) for tri, col in w.tris]
        m.tris += w.tris
    for xx in (-2.3, -1.3, 2.3):
        for yy in (-W / 2 + 0.05, W / 2 - 0.05):
            m.wheel(xx, yy, r=0.45, w=0.3)
    for yy in (-W / 2 + 0.2, W / 2 - 0.35):
        m.box(-3.33, -3.3, yy, yy + 0.15, 0.6, 0.8, RED)
    return m


def sweeper():
    """Propreté de la Ville: a white cab, a green tank, the side brushes drawn by the viewer (they spin)."""
    m = Model()
    W, G = 2.05, (0.48, 0.75, 0.23)
    _cab(m, 0.6, 2.6, W, 2.35, (0.95, 0.95, 0.93))
    m.box(-2.8, 0.5, -W / 2, W / 2, 0.45, 2.2, G, front=G, back=shade3(G, 0.85), taper=0.08)
    m.box(-2.82, 0.5, -W / 2 - 0.01, W / 2 + 0.01, 0.8, 0.95, (0.95, 0.95, 0.93))
    m.box(-0.2, 0.2, -0.45, 0.45, 2.35, 2.48, (1.0, 0.62, 0.0))                       # beacon
    for xx in (-2.0, 1.8):
        for yy in (-W / 2 + 0.05, W / 2 - 0.05):
            m.wheel(xx, yy, r=0.4, w=0.28)
    return m


def taxi():
    m = car((0.12, 0.13, 0.15), "sedan")
    m.box(-0.35, 0.35, -0.3, 0.3, 0.28 + 0.62 + 0.55, 0.28 + 0.62 + 0.75, (1.0, 0.95, 0.75))   # roof sign
    return m


# ---------------------------------------------------------------- boats (z = 0 at the waterline)

def _hull(m, L, W, z0, z1, deck, side, bow=0.22, stern=0.06):
    """A hull plan: straight sides, pointed bow (bow = share of the length), slightly cut stern."""
    pts = [(L / 2, 0.0), (L / 2 - L * bow, W / 2), (-L / 2 + L * stern, W / 2), (-L / 2, W / 2 * 0.8),
           (-L / 2, -W / 2 * 0.8), (-L / 2 + L * stern, -W / 2), (L / 2 - L * bow, -W / 2)]
    _slab(m, pts, z0, z1, deck, side)


def maille_breze():
    """T 47 destroyer (132.5 m, 12.7 m beam), grey, as moored at Quai de la Fosse."""
    m = Model()
    G, G2, D, B = (0.55, 0.58, 0.6), (0.42, 0.45, 0.48), (0.22, 0.23, 0.24), (0.12, 0.12, 0.13)
    _hull(m, 132.5, 12.7, -0.6, 0.5, (0.55, 0.18, 0.15), (0.55, 0.18, 0.15), bow=0.3)   # boot-topping
    _hull(m, 132.5, 12.7, 0.5, 5.5, (0.47, 0.5, 0.52), G2, bow=0.3)
    m.box(-38.0, 30.0, -4.5, 4.5, 5.5, 9.5, G, G)                                          # superstructure
    m.box(14.0, 30.0, -3.6, 3.6, 9.5, 13.0, G, G)                                          # bridge
    m.box(24.0, 29.5, -3.0, 3.0, 13.0, 14.5, G, (0.18, 0.22, 0.27))                         # bridge windows
    for x in (2.0, -16.0):                                                                  # funnels
        m.box(x - 3.0, x + 3.0, -2.2, 2.2, 9.5, 17.0, G, G, taper=0.4)
        m.box(x - 2.2, x + 2.2, -1.6, 1.6, 17.0, 18.0, B)
    for x, z, h in ((40.0, 5.5, 3.0), (48.5, 5.5, 2.6), (-50.0, 5.5, 3.0)):               # gun turrets
        m.box(x - 3.2, x + 3.2, -2.8, 2.8, z, z + h, G, G2, taper=0.6)
        bx = 1 if x > 0 else -1
        for yy in (-0.7, 0.7):
            m.box(min(x, x + bx * 9.5), max(x, x + bx * 9.5), yy - 0.18, yy + 0.18, z + 1.4, z + 1.8, D)
    m.box(20.0, 21.0, -0.5, 0.5, 14.5, 30.0, D)                                             # fore mast
    m.box(17.0, 24.0, -0.2, 0.2, 24.0, 24.6, D)
    m.box(-6.0, -5.0, -0.5, 0.5, 9.5, 24.0, D)                                              # main mast
    m.box(-8.0, -3.0, -2.5, 2.5, 22.0, 22.6, D)                                             # radar
    return m


def belem():
    """Three-masted barque (58 m over all, 8.8 m beam, masts ~34 m), white hull with a gilt band, sails furled."""
    m = Model()
    W, Wd, Gold, Wood, Mast, Sail = (0.95, 0.95, 0.93), (0.85, 0.86, 0.85), (0.85, 0.66, 0.25), (0.66, 0.5, 0.32), (0.3, 0.22, 0.15), (0.93, 0.91, 0.85)
    _hull(m, 51.0, 8.8, -0.5, 4.2, Wood, W, bow=0.24)
    m.box(-25.4, 19.0, -4.42, 4.42, 3.2, 3.6, Gold)                                          # gilt band
    m.box(-25.0, 20.0, -4.45, 4.45, -0.5, 0.3, (0.25, 0.25, 0.27))                           # waterline
    m.box(-25.0, -16.0, -3.6, 3.6, 4.2, 6.0, Wd, Wd)                                         # poop deck house
    m.box(25.5, 33.0, -0.25, 0.25, 4.6, 5.4, Mast)                                           # bowsprit
    for x, h, yards in ((14.0, 33.0, (12, 17, 22, 27, 31)), (0.0, 35.0, (12, 17, 22, 27, 31)), (-14.0, 28.0, ())):
        m.box(x - 0.35, x + 0.35, -0.35, 0.35, 4.2, h, Mast)
        for k, z in enumerate(yards):
            w = 9.0 - k * 1.4
            m.box(x - 0.25, x + 0.25, -w, w, z, z + 0.3, Mast)                                  # yard
            m.box(x - 0.45, x + 0.15, -w * 0.92, w * 0.92, z + 0.3, z + 0.9, Sail)             # furled sail
    m.box(-22.0, -14.0, -0.2, 0.2, 12.0, 12.3, Mast)                                         # gaff boom
    return m


def grand_elephant(phase=0):
    """Le Grand Éléphant of Les Machines de l'île (21 m long, 8 m wide, 12 m high): a tulipwood body on a riveted
    steel frame, leather ears, a passenger terrace with a glazed lounge on its back. `phase` 0/1: walking poses."""
    m = Model()
    Wood, Wood2, Steel, Dark, Leather = (0.62, 0.44, 0.27), (0.5, 0.34, 0.2), (0.38, 0.38, 0.4), (0.2, 0.2, 0.22), (0.45, 0.3, 0.2)
    Ivory, Rail = (0.92, 0.88, 0.78), (0.3, 0.24, 0.18)
    s = 1.0 if phase else -1.0
    for lx, ly, k in ((5.0, 2.4, 1), (5.0, -2.4, -1), (-5.5, 2.4, -1), (-5.5, -2.4, 1)):   # diagonal pairs swing together
        dx, up = 0.9 * s * k, (0.35 if s * k > 0 else 0.0)
        m.box(lx - 0.9 + dx, lx + 0.9 + dx, ly - 0.9, ly + 0.9, up, up + 0.5, Dark, Dark)          # foot
        m.box(lx - 0.75 + dx * 0.6, lx + 0.75 + dx * 0.6, ly - 0.75, ly + 0.75, up + 0.5, 3.2, Wood2, Wood2)   # lower leg
        m.box(lx - 0.55, lx + 0.55, ly - 0.55, ly + 0.55, 3.2, 3.8, Steel, Steel)                   # knee joint
        m.box(lx - 0.85, lx + 0.85, ly - 0.85, ly + 0.85, 3.8, 6.0, Wood, Wood)                     # upper leg
    m.box(-6.5, 4.0, -1.8, 1.8, 4.6, 5.4, Steel, Dark)                                             # under-frame
    m.box(-8.0, 6.0, -3.2, 3.2, 5.4, 9.6, Wood, Wood, taper=0.12)                                  # body
    for x in (-6.0, -3.0, 0.0, 3.0):                                                              # steel ribs
        m.box(x - 0.15, x + 0.15, -3.25, 3.25, 5.6, 9.4, Steel, Steel)
    m.box(-8.6, -8.0, -0.3, 0.3, 6.0, 8.6, Wood2, Wood2)                                           # tail
    m.box(5.6, 9.6, -2.1, 2.1, 6.6, 11.0, Wood, Wood, front=Wood2)                                 # head
    m.box(6.0, 9.2, -1.9, 1.9, 11.0, 12.2, Wood, Wood, taper=0.45)                                # domed forehead
    m.box(9.0, 9.7, -1.3, -0.8, 9.2, 9.8, Dark); m.box(9.0, 9.7, 0.8, 1.3, 9.2, 9.8, Dark)         # eyes
    for sy in (-1, 1):                                                                            # ears
        m.box(6.4, 7.6, sy * 2.1 if sy > 0 else -5.8, 5.8 if sy > 0 else -2.1, 6.2, 10.8, Leather, Leather)
        m.box(9.2, 12.2, sy * 1.0 - 0.2, sy * 1.0 + 0.2, 6.6, 7.0, Ivory, Ivory)                  # tusks
    lift = 1.2 if phase else 0.0                                                                  # trunk
    m.box(9.6, 11.0, -0.8, 0.8, 5.2, 7.6, Wood2, Wood2)
    m.box(10.4, 11.6, -0.65, 0.65, 3.0 + lift * 0.5, 5.2, Wood2, Wood2)
    m.box(11.0 + lift * 0.6, 12.0 + lift * 0.8, -0.5, 0.5, 0.6 + lift, 3.0 + lift * 0.5, Wood2, Wood2)
    m.box(11.6 + lift * 1.2, 12.6 + lift * 1.2, -0.45, 0.45, 0.4 + lift, 1.2 + lift, Wood2, Wood2)   # curled tip
    m.box(-7.6, 5.4, -3.6, 3.6, 9.6, 10.0, Wood2, Rail)                                            # terrace deck
    for x0, x1, y0, y1 in ((-7.6, 5.4, 3.4, 3.6), (-7.6, 5.4, -3.6, -3.4), (5.2, 5.4, -3.6, 3.6), (-7.6, -7.4, -3.6, 3.6)):
        m.box(x0, x1, y0, y1, 10.0, 11.0, Rail, Rail)                                              # balustrade
    m.box(-4.5, 1.5, -2.4, 2.4, 10.0, 12.0, Wood, GLASS)                                           # glazed lounge
    m.box(-4.8, 1.8, -2.7, 2.7, 12.0, 12.3, Steel, Steel)                                          # lounge roof
    return m


def peniche(cargo=(0.3, 0.38, 0.3)):
    """Freycinet barge (38.5 x 5.05 m): black hull, covered hold, wheelhouse and living quarters aft."""
    m = Model()
    H, Trim = (0.12, 0.12, 0.14), (0.7, 0.15, 0.12)
    _hull(m, 38.5, 5.05, -0.4, 1.6, (0.25, 0.25, 0.26), H, bow=0.08, stern=0.04)
    m.box(-19.0, 19.0, -2.55, 2.55, 1.3, 1.5, Trim)                                          # rubbing strake
    m.box(-9.0, 16.0, -2.0, 2.0, 1.6, 2.6, cargo, cargo, taper=0.4)                          # hold covers
    m.box(-18.0, -11.0, -2.3, 2.3, 1.6, 3.6, (0.93, 0.92, 0.88), (0.93, 0.92, 0.88))         # living quarters
    m.box(-13.5, -11.0, -1.6, 1.6, 3.6, 5.0, (0.93, 0.92, 0.88), GLASS)                      # wheelhouse
    m.box(-17.0, -16.6, -0.1, 0.1, 3.6, 6.0, H)                                              # flag pole
    return m


def sailboat():
    m = Model()
    _hull(m, 10.0, 3.3, -0.3, 1.1, (0.85, 0.8, 0.7), (0.96, 0.96, 0.95), bow=0.3)
    m.box(-3.5, 0.5, -1.1, 1.1, 1.1, 1.7, (0.96, 0.96, 0.95), GLASS)                         # cabin
    m.box(0.8, 1.0, -0.1, 0.1, 1.1, 14.0, (0.75, 0.76, 0.78))                                # mast
    _fin(m, [(0.7, 2.2), (-3.8, 2.2), (0.7, 13.5)], -0.05, 0.05, (0.97, 0.97, 0.95), (0.97, 0.97, 0.95))   # mainsail
    _fin(m, [(1.1, 1.8), (4.8, 1.4), (1.1, 12.0)], -0.05, 0.05, (0.92, 0.93, 0.95), (0.92, 0.93, 0.95))    # jib
    return m


def motorboat():
    m = Model()
    _hull(m, 8.0, 2.8, -0.3, 1.0, (0.96, 0.96, 0.95), (0.96, 0.96, 0.95), bow=0.35)
    m.box(-3.6, 4.0, -1.42, 1.42, 0.2, 0.45, (0.12, 0.3, 0.6))                               # stripe
    m.box(-1.5, 1.2, -1.0, 1.0, 1.0, 2.1, (0.96, 0.96, 0.95), GLASS, front=GLASS, taper=0.2)
    return m


LIVERY_TGV = ((0.93, 0.92, 0.93), (0.45, 0.15, 0.4), (0.82, 0.82, 0.84))
LIVERY_TER = ((0.94, 0.94, 0.92), (0.12, 0.42, 0.7), (0.78, 0.8, 0.82))

HELI_PITCH = [-7.0, 0.0, 8.0, 16.0]        # nose up (braking), hover, cruising, full speed


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
    # people: 8 coats x 2 walking phases, and 8 cyclists (one sheet of 16 directions x ... rows)
    for k in range(len(COATS)):
        coat, legs, skin, hair, style = looks(k)
        for ph in (0, 1):
            out.append((f"walker_{k}_{ph}", person(coat, legs, phase=(1.0 if ph else -1.0), skin=skin, hair=hair, style=style), 22.0))
    for k in range(len(COATS)):
        coat, legs, skin, hair, style = looks(k)
        out.append((f"bike_{k}", person(coat, legs, bike=True, skin=skin, hair=hair, style=0 if style == 1 else style), 16.0))
    for k, p in enumerate(HELI_PITCH):
        out.append((f"heli_{k}", helicopter(p), 13.5))
    # big things: one per sheet (256 px cells)
    out.append(("plane_jet", airliner(), 5.4))
    out.append(("plane_prop", regional(), 7.0))
    out.append(("train_tgv_head", rail_car(LIVERY_TGV, head=True), 7.5))
    out.append(("train_tgv", rail_car(LIVERY_TGV), 7.5))
    out.append(("train_ter", rail_car(LIVERY_TER), 7.5))
    # more traffic and the emergency services (sheets after the big things: numbering of older sheets kept)
    out.append(("ems_ambulance", ambulance(), 11.0))
    out.append(("ems_police", police_car(), 12.0))
    out.append(("ems_fire", fire_engine(), 10.0))
    out.append(("ems_samu", samu_car(), 12.0))
    out.append(("van_delivery", van_body((0.95, 0.95, 0.93)), 11.0))
    out.append(("truck_box", box_truck(), 9.0))
    out.append(("car_taxi", taxi(), 12.0))
    out.append(("car_teal_hatch", car((0.2, 0.55, 0.55), "hatch"), 12.0))
    out.append(("ems_tow", tow_truck(), 10.0))
    out.append(("ems_tow_loaded", tow_truck(True), 10.0))
    out.append(("ems_sweeper", sweeper(), 10.0))
    # boats: moving ones in 16 directions; moored landmarks in their one real heading (water.json)
    out.append(("boat_peniche", peniche(), 6.0))
    out.append(("boat_peniche2", peniche((0.45, 0.3, 0.2)), 6.0))
    out.append(("boat_sail", sailboat(), 9.0))
    out.append(("boat_motor", motorboat(), 12.0))
    for k in range(len(COATS)):                       # knocked down (sheets at the end: older numbering kept)
        out.append((f"down_{k}", lying(*looks(k), k), 22.0))
    out.append(("elephant_0", grand_elephant(0), 7.0))
    out.append(("elephant_1", grand_elephant(1), 7.0))
    try:
        moored = json.loads((ROOT / "web" / "data" / "world" / "water.json").read_text())["moored"]
    except (OSError, ValueError):
        moored = []
    for mo in moored:
        if mo["ship"] == "maille_breze" and mo["len"] > 100:
            out.append(("ship_maille_breze", maille_breze(), 5.5, [mo["a"]]))
        elif mo["ship"] == "belem":
            out.append(("ship_belem", belem(), 9.0, [mo["a"]]))
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


def render_sheet(items, view, cell=CELL, sheet=SHEET, dirs=DIRS, raw_out=None):
    """items: [(name, model, ppm)]; each gets `dirs` directions, cells left to right then down. Returns the
    RGB sheet, the mask and the atlas entries (and the supersampled RGBA in raw_out, if given a list)."""
    r, d, b = view.axes
    n = sheet * SS
    ctx = moderngl.create_standalone_context(backend="egl")
    prog = ctx.program(vertex_shader=VS, fragment_shader=FS)
    prog["u_r"].value = tuple(r); prog["u_d"].value = tuple(d); prog["u_b"].value = tuple(b)
    prog["u_size"].value = (n, n)
    fbo = ctx.framebuffer([ctx.renderbuffer((n, n), 4)], ctx.depth_renderbuffer((n, n)))
    fbo.use(); ctx.enable(moderngl.DEPTH_TEST)
    fbo.clear(*([0.0] * 4 if raw_out is not None else [c / 255 for c in BG] + [0.0]))
    atlas = {}
    per_row = sheet // cell
    slot = 0
    for name, model, ppm, *rest in items:
        cells = []
        yaws = rest[0] if rest else [2 * math.pi * k / dirs for k in range(dirs)]
        for yaw in yaws:
            V, C = model.mesh(yaw)
            tri = V.reshape(-1, 3, 3)
            nrm = np.cross(tri[:, 1] - tri[:, 0], tri[:, 2] - tri[:, 0])
            nrm /= np.linalg.norm(nrm, axis=1, keepdims=True) + 1e-9
            # face normals towards the camera side for double-sided quads
            flip = (nrm @ np.asarray(b, "f4")) < 0
            nrm[flip] *= -1
            N = np.repeat(nrm, 3, axis=0).astype("f4")
            cx, cy = (slot % per_row) * cell, (slot // per_row) * cell
            ax, ay = cx + cell / 2, cy + cell * (0.7 if cell <= 256 else 0.62)   # where the ground centre lands
            prog["u_cell"].value = (ax * SS, ay * SS, ppm * SS, 0.0)
            vbo = ctx.buffer(np.hstack([V, C, N]).astype("f4").tobytes())
            vao = ctx.vertex_array(prog, [(vbo, "3f 3f 3f", "in_p", "in_c", "in_n")])
            ctx.scissor = (int(cx * SS), int(n - (cy + cell) * SS), cell * SS, cell * SS)
            vao.render()
            vao.release(); vbo.release()
            cells.append([cx, cy, cell, cell, ax - cx, ay - cy])
            slot += 1
        atlas[name] = {"ppm": ppm, "cells": cells}
    ctx.scissor = None
    raw = np.frombuffer(fbo.read(components=4, alignment=1), "u1").reshape(n, n, 4)[::-1]
    if raw_out is not None:
        raw_out.append(raw.copy())
    img = Image.fromarray(raw[..., :3].copy()).resize((sheet, sheet), Image.LANCZOS)
    mask = Image.fromarray(raw[..., 3].copy()).resize((sheet, sheet), Image.BILINEAR)
    ctx.release()
    return img, mask, atlas


# ---------------------------------------------------------------- painting

PROMPT = ("Repaint these 3D vehicle renders as detailed isometric pixel-art vehicles, in the same style as a pixel-art "
          "city map. Keep every vehicle exactly where it is, with the same shape, size, direction and colours. "
          "Keep the plain grey background. Do not add or remove anything.")


PROMPT_WALKERS = ("Repaint these 3D renders of small pedestrians as detailed isometric pixel-art people walking, "
                  "in the same style as a pixel-art city map. Keep every person exactly where it is, with the same pose, "
                  "size, direction and clothes colours. Nobody rides anything. Keep the plain grey background. "
                  "Do not add or remove anything.")
PROMPT_BIKES = ("Repaint these 3D renders of cyclists as detailed isometric pixel-art people riding bicycles, in the same "
                "style as a pixel-art city map. Keep every cyclist exactly where it is, with the same size, direction "
                "and clothes colours. Keep the plain grey background. Do not add or remove anything.")


PROMPT_DOWN = ("Repaint these 3D renders of small people lying on their back on the ground as detailed isometric pixel-art "
               "people, in the same style as a pixel-art city map. Keep every person exactly where it is, with the same "
               "pose, size, direction and clothes colours. No blood. Keep the plain grey background. "
               "Do not add or remove anything.")

PROMPT_HELI = ("Repaint these 3D renders of a small red helicopter body as crisp isometric pixel-art sprites with dark "
               "outlines, in the same style as a pixel-art city map. Keep every helicopter exactly where it is, with "
               "the same shape, size, direction, tilt, colours and black landing skids. Do not draw any rotor blades. "
               "Keep the plain grey background. Do not add or remove anything.")


PROMPT_PLANE = ("Repaint these 3D renders of an airliner as crisp isometric pixel-art sprites with dark outlines, in the "
                "same style as a pixel-art city map. Keep every aeroplane exactly where it is, with the same shape, size, "
                "direction and colours. There are no people anywhere: draw only the aeroplanes. Keep the plain empty grey "
                "background. Do not add or remove anything.")
PROMPT_FLEET = ("Repaint these 3D renders of cars, vans, trucks and emergency vehicles as crisp isometric pixel-art "
                "sprites with dark outlines, in the same style as a pixel-art city map. Keep every vehicle exactly where "
                "it is, with the same shape, size, direction, colours, stripes and roof lights. There are no people "
                "anywhere: draw only the vehicles. Keep the plain empty grey background. Do not add or remove anything.")
PROMPT_BOAT = ("Repaint these 3D renders of boats as crisp isometric pixel-art sprites with dark outlines, in the same "
               "style as a pixel-art city map. Keep every boat exactly where it is, with the same shape, size, direction, "
               "masts and colours. There is no water and there are no people: draw only the boats. Keep the plain empty "
               "grey background. Do not add or remove anything.")
PROMPT_BELEM = ("Repaint this 3D render of the Belem, a three-masted barque with a white hull, a thin gilt band, a wooden deck and "
                "furled white sails on her yards, as a crisp isometric pixel-art sprite with dark outlines, in the same style as a "
                "pixel-art city map. Keep the ship exactly where it is, with the same shape, size, direction and colours: the hull "
                "stays white. There is no water and there are no other boats or people. Keep the plain empty grey background.")
PROMPT_TRAIN = ("Repaint these 3D renders of a French train car as crisp isometric pixel-art sprites with dark outlines, in "
                "the same style as a pixel-art city map. Keep every car exactly where it is, with the same shape, size, "
                "direction and colours, windows along the sides. There are no people anywhere: draw only the train cars. "
                "Keep the plain empty grey background. Do not add or remove anything.")


PROMPT_ELEPHANT = ("Repaint this 3D render as the Grand Éléphant of Les Machines de l'île in Nantes: the famous 12 m tall "
                   "mechanical elephant built of weathered greyish-brown tulipwood planks on a riveted steel frame, with "
                   "visible gears and pistons in its legs, big leather ears, a long articulated trunk and a wooden passenger "
                   "terrace with a balustrade and a small glazed lounge on its back. Draw it as one crisp isometric pixel-art "
                   "sprite with dark outlines, in the style of a pixel-art city map. Keep it exactly where it is, with the same "
                   "silhouette, size, direction and pose; fill the whole silhouette. No people. Keep the plain empty grey "
                   "background.")


def paint(img, seed=1, prompt=PROMPT):
    from .stylize import load_env, stylize          # same painter as the tiles
    load_env()
    return stylize(img, model="9b", seed=seed, prompt=prompt)


def paint_cells(img, entries, prompt, seed=7):
    """Paint every cell on its own, enlarged: the painter loses small sprites scattered over an empty sheet."""
    out = img.copy()
    for e in entries.values():
        for cx, cy, w, h, *_ in e["cells"]:
            box = (cx * SS, cy * SS, (cx + w) * SS, (cy + h) * SS) if img.size[0] > SHEET else (cx, cy, cx + w, cy + h)
            cell = img.crop(box)
            big = paint(cell.resize((1024, 1024), Image.NEAREST), seed=seed, prompt=prompt)
            out.paste(big.convert("RGB").resize(cell.size, Image.LANCZOS), box[:2])
    return out


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


def pixel_art(raw, size):
    """Supersampled RGBA render (transparent black background) -> crisp pixel-art sprites, the same in every
    direction (no painter: it redraws each direction its own way, and a car changes model as it turns).
    Box-filtered down, hard alpha, a dark outline round the silhouette and darker lines where faces meet."""
    n = raw.shape[0]
    f = n // size
    a4 = raw.reshape(size, f, size, f, 4).astype("f4").mean(axis=(1, 3)) / 255.0
    alpha = a4[..., 3]
    rgb = a4[..., :3] / np.maximum(alpha, 1e-4)[..., None]
    solid = alpha > 0.45
    lum = rgb @ np.asarray([0.3, 0.59, 0.11], "f4")

    def shift(x, dy, dx, fill):
        o = np.full_like(x, fill)
        ys = slice(max(dy, 0), size + min(dy, 0)); yd = slice(max(-dy, 0), size + min(-dy, 0))
        xs = slice(max(dx, 0), size + min(dx, 0)); xd = slice(max(-dx, 0), size + min(-dx, 0))
        o[yd, xd] = x[ys, xs]
        return o

    out = rgb.copy()
    # inner lines: a pixel noticeably darker than a neighbour across a face edge gets darker still
    edge = np.zeros_like(solid)
    for dy, dx in ((0, 1), (1, 0), (0, -1), (-1, 0)):
        nl, ns = shift(lum, dy, dx, 0.0), shift(solid, dy, dx, False)
        edge |= ns & (nl - lum > 0.12)
    out[edge & solid] *= 0.78
    # outline: transparent pixels touching the sprite, the colour of the sprite there, much darker
    ring = np.zeros_like(solid)
    acc = np.zeros_like(rgb); cnt = np.zeros(solid.shape, "f4")
    for dy, dx in ((0, 1), (1, 0), (0, -1), (-1, 0)):
        ns = shift(solid, dy, dx, False)
        ring |= ns & ~solid
        acc += shift(out, dy, dx, 0.0) * ns[..., None]; cnt += ns
    col = acc / np.maximum(cnt, 1)[..., None]
    out = np.where(ring[..., None], col * 0.32 + 0.03, out)
    a = (solid | ring).astype("u1") * 255
    return Image.fromarray(np.dstack([(np.clip(out, 0, 1) * 255).astype("u1"), a]), "RGBA")


def prompt_for(name):
    """The painting prompt of the sheet whose first sprite is `name`."""
    for prefix, pr in (("elephant_", PROMPT_ELEPHANT), ("ship_belem", PROMPT_BELEM), ("boat_", PROMPT_BOAT), ("ship_", PROMPT_BOAT), ("heli", PROMPT_HELI), ("plane", PROMPT_PLANE), ("train", PROMPT_TRAIN), ("ems_", PROMPT_FLEET),
                       ("van_delivery", PROMPT_FLEET), ("walker", PROMPT_WALKERS), ("bike", PROMPT_BIKES), ("down_", PROMPT_DOWN)):
        if name.startswith(prefix):
            return pr
    return PROMPT


CLEAN_DIRS = 64
CLEAN = ("car_", "bus", "tram_", "navibus", "bike_", "heli_", "ems_", "van_delivery", "truck_")


def main(argv):
    """Two kinds of sheets:
    - clean: everything that turns while you watch it (cars, vans, buses, trams, bikes, the helicopter) in 64
      directions, drawn straight from the 3D models as pixel art, so it looks the same at every angle;
    - painted: people, planes, trains, boats, the elephant: 16 directions repainted by the tiles' painter, the
      painting cached as out/sprites/hH/paint_<first sprite>.png."""
    h = int(argv[0]) if argv else 0
    what = argv[1] if len(argv) > 1 else "all"
    view = geo.View(h)
    out = ROOT / "web" / "data" / "sprites" / f"h{h}"
    work = ROOT / "out" / "sprites" / f"h{h}"
    out.mkdir(parents=True, exist_ok=True); work.mkdir(parents=True, exist_ok=True)
    items = catalogue()
    clean = [it for it in items if it[0].startswith(CLEAN)]
    painted = [it for it in items if not it[0].startswith(CLEAN)]
    big = 2048
    bikes = [it for it in clean if it[0].startswith("bike_")]
    rest = [it for it in clean if not it[0].startswith("bike_")]
    batches = [(rest[s:s + 4], CELL, big, CLEAN_DIRS) for s in range(0, len(rest), 4)]
    batches += [(bikes, 64, big, CLEAN_DIRS)]
    walkers = [it for it in painted if it[0].startswith("walker")]
    batches += [(walkers[s:s + 16], 64, SHEET, DIRS) for s in range(0, len(walkers), 16)]
    batches += [([it], 256, SHEET, DIRS) for it in painted if it[0].startswith(("plane", "train", "boat_", "elephant_"))]
    batches += [([it], SHEET, SHEET, DIRS) for it in painted if it[0].startswith("ship_")]
    batches += [([it for it in painted if it[0].startswith("down_")], 64, SHEET, DIRS)]
    atlas = {"dirs": DIRS, "sheets": []}
    for f in out.glob("sheet_*.png"):
        f.unlink()
    for k, (batch, cell, size, dirs) in enumerate(batches):
        first = batch[0][0]
        if dirs == CLEAN_DIRS:
            raw = []
            img, mask, entries = render_sheet(batch, view, cell, size, dirs, raw)
            sprite = pixel_art(raw[0], size)
        else:
            img, mask, entries = render_sheet(batch, view, cell, size, dirs)
            img.save(work / f"render_{first}.png"); mask.save(work / f"mask_{first}.png")
            pfile = work / f"paint_{first}.png"
            if what in ("paint", "all") or pfile.exists():
                if not pfile.exists() and first.startswith("elephant_"):
                    paint_cells(img, entries, PROMPT_ELEPHANT).save(pfile)
                if not pfile.exists():
                    pr = prompt_for(first)
                    # seeds picked by eye: 7 gave planes and trains without stray people
                    seed = 7 if pr in (PROMPT_PLANE, PROMPT_TRAIN, PROMPT_FLEET, PROMPT_BOAT, PROMPT_BELEM, PROMPT_ELEPHANT) else 1
                    paint(img, prompt=pr, seed=seed).save(pfile)
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
