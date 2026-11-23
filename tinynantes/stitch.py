"""Assemble independently painted tiles (each painted with an M px context margin) into one image.

Averaging the overlaps ghosts: two paintings of the same place never agree pixel for pixel. Instead each
overlap is cut along the seam where the two paintings agree best (dynamic programming over the colour
difference), and the cut is feathered over a few pixels only. Rows are stitched first, then the rows.
"""
from __future__ import annotations

import numpy as np
from scipy import ndimage

FEATHER = 4      # px of cross-fade on each side of the cut
GUARD = 24       # px kept away from the painted borders, where context is weakest


def _seam(cost):
    """Min-cost top-to-bottom path through cost (H, W); returns the column for each row."""
    h, w = cost.shape
    acc = cost.copy()
    back = np.zeros((h, w), "i4")
    for y in range(1, h):
        prev = acc[y - 1]
        cand = np.stack([np.r_[np.inf, prev[:-1]], prev, np.r_[prev[1:], np.inf]])
        k = cand.argmin(0)
        acc[y] += cand[k, np.arange(w)]
        back[y] = np.arange(w) + k - 1
    path = np.zeros(h, "i4")
    path[-1] = acc[-1].argmin()
    for y in range(h - 1, 0, -1):
        path[y - 1] = back[y, path[y]]
    return path


def _join(a, b, ov):
    """Join a and b side by side (same height), b's first `ov` columns overlapping a's last `ov`."""
    h = a.shape[0]
    A, B = a[:, -ov:].astype("f4"), b[:, :ov].astype("f4")
    cost = ndimage.uniform_filter(np.abs(A - B).sum(-1), 5)
    cost[:, :GUARD] = cost[:, -GUARD:] = 1e6
    cut = _seam(cost)
    xs = np.arange(ov)[None, :]
    w = np.clip((xs - cut[:, None] + FEATHER) / (2 * FEATHER), 0, 1)[..., None]   # 0 -> a, 1 -> b
    mid = A * (1 - w) + B * w
    return np.concatenate([a[:, :-ov].astype("f4"), mid, b[:, ov:].astype("f4")], 1)


def stitch(arts: dict, N=1024, M=128):
    """arts: {(i, j): HxWx3 array of size N+2M}. Returns the mosaic of the tile cores (margins cut off)."""
    i0, j0 = min(t[0] for t in arts), min(t[1] for t in arts)
    i1, j1 = max(t[0] for t in arts), max(t[1] for t in arts)
    S = N + 2 * M
    blank = np.full((S, S, 3), 128, "f4")
    rows = []
    for j in range(j0, j1 + 1):
        row = None
        for i in range(i0, i1 + 1):
            t = arts.get((i, j), blank)
            row = t.astype("f4") if row is None else _join(row, t, 2 * M)
        rows.append(row)
    out = rows[0]
    for r in rows[1:]:
        out = _join(out.transpose(1, 0, 2), r.transpose(1, 0, 2), 2 * M).transpose(1, 0, 2)
    return np.clip(out[M:-M, M:-M], 0, 255).astype("u1")
