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


def person(coat, legs=(0.18, 0.2, 0.26), phase=0.0, bike=False, skin=(0.93, 0.76, 0.62), hair=(0.25, 0.17, 0.1)):
    """A walker (legs apart by `phase`) or a cyclist. Slightly larger than life (x1.6) so they read on the map."""
    m = Model()
    K = 1.6
    if bike:
        frame = (0.12, 0.13, 0.14)
        m.wheel(-0.55 * K, 0, r=0.33 * K, w=0.05 * K, col=frame)
        m.wheel(0.55 * K, 0, r=0.33 * K, w=0.05 * K, col=frame)
        m.box(-0.5 * K, 0.5 * K, -0.03 * K, 0.03 * K, 0.33 * K, 0.42 * K, frame)
        m.box(0.42 * K, 0.5 * K, -0.25 * K, 0.25 * K, 0.85 * K, 0.9 * K, frame)                         # handlebar
        z0 = 0.5 * K
        m.box(-0.2 * K, 0.0, -0.09 * K, 0.09 * K, z0, z0 + 0.42 * K, legs)                               # legs on pedals
    else:
        z0 = 0.0
        for side, sgn in ((-1, 1), (1, -1)):
            dx = sgn * phase * 0.16 * K
            m.box(dx - 0.07 * K, dx + 0.07 * K, side * 0.11 * K - 0.06 * K, side * 0.11 * K + 0.06 * K, 0, 0.82 * K, legs)
            m.box(dx - 0.05 * K, dx + 0.11 * K, side * 0.11 * K - 0.06 * K, side * 0.11 * K + 0.06 * K, 0, 0.07 * K, DARK)  # shoe
    zt = z0 + (0.82 * K if not bike else 0.42 * K)
    lean = 0.12 * K if bike else 0.0
    m.box(-0.11 * K + lean, 0.11 * K + lean, -0.2 * K, 0.2 * K, zt, zt + 0.6 * K, coat)                    # torso
    for side in (-1, 1):                                                                                     # arms
        dx = (-side * phase * 0.12 * K) if not bike else 0.2 * K
        m.box(dx - 0.05 * K + lean, dx + 0.05 * K + lean, side * 0.25 * K - 0.05 * K, side * 0.25 * K + 0.05 * K,
              zt + 0.1 * K, zt + 0.58 * K, coat)
    m.box(-0.09 * K + lean, 0.09 * K + lean, -0.09 * K, 0.09 * K, zt + 0.6 * K, zt + 0.82 * K, skin, front=skin)  # head
    m.box(-0.1 * K + lean, 0.07 * K + lean, -0.1 * K, 0.1 * K, zt + 0.76 * K, zt + 0.86 * K, hair)          # hair
    return m


COATS = [(0.72, 0.18, 0.16), (0.17, 0.38, 0.6), (0.85, 0.62, 0.2), (0.25, 0.45, 0.3), (0.42, 0.3, 0.6), (0.86, 0.84, 0.8),
         (0.16, 0.17, 0.2), (0.75, 0.42, 0.52)]
HAIRS = [(0.25, 0.17, 0.1), (0.1, 0.08, 0.07), (0.75, 0.6, 0.3), (0.55, 0.3, 0.15)]

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
    for k, coat in enumerate(COATS):
        for ph in (0, 1):
            out.append((f"walker_{k}_{ph}", person(coat, phase=(1.0 if ph else -1.0), hair=HAIRS[k % 4]), 22.0))
    for k, coat in enumerate(COATS):
        out.append((f"bike_{k}", person(coat, bike=True, hair=HAIRS[k % 4]), 16.0))
    for k, p in enumerate(HELI_PITCH):
        out.append((f"heli_{k}", helicopter(p), 13.5))
    # big things: one per sheet (256 px cells)
    out.append(("plane_jet", airliner(), 5.4))
    out.append(("plane_prop", regional(), 7.0))
    out.append(("train_tgv_head", rail_car(LIVERY_TGV, head=True), 7.5))
    out.append(("train_tgv", rail_car(LIVERY_TGV), 7.5))
    out.append(("train_ter", rail_car(LIVERY_TER), 7.5))
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


def render_sheet(items, view, cell=CELL):
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
    per_row = SHEET // cell
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
            cx, cy = (slot % per_row) * cell, (slot // per_row) * cell
            ax, ay = cx + cell / 2, cy + cell * 0.7           # where the ground centre lands
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
    img = Image.fromarray(raw[..., :3].copy()).resize((SHEET, SHEET), Image.LANCZOS)
    mask = Image.fromarray(raw[..., 3].copy()).resize((SHEET, SHEET), Image.BILINEAR)
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


PROMPT_HELI = ("Repaint these 3D renders of a small red helicopter body as crisp isometric pixel-art sprites with dark "
               "outlines, in the same style as a pixel-art city map. Keep every helicopter exactly where it is, with "
               "the same shape, size, direction, tilt, colours and black landing skids. Do not draw any rotor blades. "
               "Keep the plain grey background. Do not add or remove anything.")


PROMPT_PLANE = ("Repaint these 3D renders of an airliner as crisp isometric pixel-art sprites with dark outlines, in the "
                "same style as a pixel-art city map. Keep every aeroplane exactly where it is, with the same shape, size, "
                "direction and colours. There are no people anywhere: draw only the aeroplanes. Keep the plain empty grey "
                "background. Do not add or remove anything.")
PROMPT_TRAIN = ("Repaint these 3D renders of a French train car as crisp isometric pixel-art sprites with dark outlines, in "
                "the same style as a pixel-art city map. Keep every car exactly where it is, with the same shape, size, "
                "direction and colours, windows along the sides. There are no people anywhere: draw only the train cars. "
                "Keep the plain empty grey background. Do not add or remove anything.")


def paint(img, seed=1, prompt=PROMPT):
    from .stylize import load_env, stylize          # same painter as the tiles
    load_env()
    return stylize(img, model="9b", seed=seed, prompt=prompt)


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
    vehicles = [it for it in items if not it[0].startswith(("walker", "bike", "heli", "plane", "train"))]
    people = [it for it in items if it[0].startswith(("walker", "bike"))]
    aircraft = [it for it in items if it[0].startswith("heli")]
    big = [it for it in items if it[0].startswith(("plane", "train"))]
    # sheets keep their numbers (paint_N.png caches): new groups go after the existing ones
    batches = [(vehicles[s:s + 4], CELL) for s in range(0, len(vehicles), 4)]
    batches += [(people[s:s + 16], 64) for s in range(0, len(people), 16)]
    batches += [(aircraft[s:s + 4], CELL) for s in range(0, len(aircraft), 4)]
    batches += [([it], 256) for it in big]
    atlas = {"dirs": DIRS, "sheets": []}
    for k, (batch, cell) in enumerate(batches):
        img, mask, entries = render_sheet(batch, view, cell)
        img.save(work / f"render_{k}.png"); mask.save(work / f"mask_{k}.png")
        pfile = work / f"paint_{k}.png"
        if what in ("paint", "all") or pfile.exists():
            if not pfile.exists():
                pr = (PROMPT_HELI if batch[0][0].startswith("heli") else PROMPT_PLANE if batch[0][0].startswith("plane")
                      else PROMPT_TRAIN if batch[0][0].startswith("train") else PROMPT if cell == CELL else PROMPT_BIKES) if batch[0][0].startswith("bike") else PROMPT_WALKERS
                # seeds picked by eye: 3 gave the cleanest helicopters, 7 planes and trains without stray people
                seed = 3 if pr is PROMPT_HELI else 7 if pr in (PROMPT_PLANE, PROMPT_TRAIN) else 1
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
