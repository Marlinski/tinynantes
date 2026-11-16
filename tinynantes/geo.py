"""Projection and tile grid.

World coordinates are Lambert-93 (EPSG:2154): x east, y north, z up (metres),
taken relative to ORIGIN so floats stay small.

The view is a true isometric-style orthographic camera at 45 deg elevation.
Heading h (0..7) rotates the camera by h*45 deg around the vertical axis;
heading 0 looks from the south-west toward the north-east.

Screen space is in metres: sx grows to the right, sy grows *downwards*.
A tile (i, j) covers sx in [i*T, (i+1)*T) and sy in [j*T, (j+1)*T).
"""
from __future__ import annotations

import math
from dataclasses import dataclass

import numpy as np

ORIGIN = (355857.0, 6689440.0)  # Château des ducs de Bretagne, Lambert-93
TILE_PX = 1024
M_PER_PX = 0.29296875           # 300 m per 1024 px tile
TILE_M = TILE_PX * M_PER_PX     # 300.0
ELEV = math.radians(45.0)
Z_MIN, Z_MAX = -5.0, 110.0      # absolute height range we expect (m, NGF)

# Mapped area (Lambert-93 bbox): 6 x 6 km inner ring, from Chantenay to Doulon,
# up the Erdre to the university and down to north Rezé. (v1 was 354000, 6686800, 357500, 6690300.)
AREA = (352500.0, 6685500.0, 358500.0, 6691500.0)


@dataclass(frozen=True)
class View:
    heading: int = 0

    @property
    def axes(self):
        """right, down (screen) and toward-camera unit vectors in world ENU."""
        a = math.radians(45.0 + 45.0 * self.heading)   # camera azimuth (from east, ccw) pointing *to* the camera is a+180
        # forward (horizontal, from camera to scene): heading 0 -> north-east
        fx, fy = math.cos(a), math.sin(a)
        right = np.array([fy, -fx, 0.0])                # 90 deg clockwise of forward
        ce, se = math.cos(ELEV), math.sin(ELEV)
        back = np.array([-fx * ce, -fy * ce, se])       # toward the camera
        up = np.array([fx * se, fy * se, ce])           # screen up
        return right, -up, back

    def to_screen(self, x, y, z):
        """World (relative to ORIGIN) -> screen metres (sx, sy) and depth (bigger = closer)."""
        r, d, b = self.axes
        return (x * r[0] + y * r[1] + z * r[2],
                x * d[0] + y * d[1] + z * d[2],
                x * b[0] + y * b[1] + z * b[2])

    def ground_bbox(self, sx0, sy0, sx1, sy1, z0=Z_MIN, z1=Z_MAX):
        """Axis-aligned world bbox (absolute Lambert-93) of every point at height z0..z1
        that can appear in the screen rect."""
        r, d, _ = self.axes
        # solve for (x, y) given (sx, sy, z): 2x2 system on the horizontal parts
        m = np.array([[r[0], r[1]], [d[0], d[1]]])
        inv = np.linalg.inv(m)
        pts = []
        for sx in (sx0, sx1):
            for sy in (sy0, sy1):
                for z in (z0, z1):
                    xy = inv @ np.array([sx - r[2] * z, sy - d[2] * z])
                    pts.append(xy)
        pts = np.array(pts)
        return (pts[:, 0].min() + ORIGIN[0], pts[:, 1].min() + ORIGIN[1],
                pts[:, 0].max() + ORIGIN[0], pts[:, 1].max() + ORIGIN[1])

    def tiles_for_area(self, area=AREA, z=10.0):
        """Tile indices whose screen rect sees any of the area (at ground height z)."""
        x0, y0, x1, y1 = area
        xs = np.array([x0, x1, x0, x1]) - ORIGIN[0]
        ys = np.array([y0, y0, y1, y1]) - ORIGIN[1]
        sx, sy, _ = self.to_screen(xs, ys, np.full(4, z))
        i0, i1 = math.floor(sx.min() / TILE_M), math.floor(sx.max() / TILE_M)
        j0, j1 = math.floor(sy.min() / TILE_M), math.floor(sy.max() / TILE_M)
        out = []
        r, d, _ = self.axes
        inv = np.linalg.inv(np.array([[r[0], r[1]], [d[0], d[1]]]))
        m = TILE_M * 0.5   # keep tiles whose centre (on the ground) is within half a tile of the area
        for j in range(j0, j1 + 1):
            for i in range(i0, i1 + 1):
                cx, cy = (i + 0.5) * TILE_M, (j + 0.5) * TILE_M
                gx, gy = inv @ np.array([cx - r[2] * z, cy - d[2] * z])
                gx += ORIGIN[0]; gy += ORIGIN[1]
                if x0 - m <= gx <= x1 + m and y0 - m <= gy <= y1 + m:
                    out.append((i, j))
        return out
