"""Production pipeline on Google 3D renders (out/g3d/hH/I_J.png, 1280 px = tile + 128 px context).

paint     each render is painted independently (FLUX.2 klein 9B edit, the 'g9b' prompt), then repaired:
          invented water -> photo colours, real water -> one uniform colour, rails laid back on top.
          -> out/g3d/hH/paint/I_J.png (1280 px)
assemble  each tile's 1024 px core is cut out of its painting; inside the 128 px bands next to a neighbour
          the pixels come from whichever painting lies on its side of the seam where the two paintings agree
          best. The seam of a pair depends only on that pair, so both tiles agree on it and cores join
          without a visible line.  -> out/hH/art/I_J.png (what publish.py reads)

Usage: uv run python -m tinynantes.build2 H [paint|assemble|all]
"""
from __future__ import annotations

import shutil
import sys
import time
from concurrent.futures import ThreadPoolExecutor
from functools import lru_cache
from pathlib import Path

import numpy as np
from PIL import Image
from scipy import ndimage

from . import g3d, geo
from .painttest import paint
from .qa import fix_invented_water, unify_water
from .stitch import FEATHER, GUARD, _seam
from .stylize import load_env

ROOT = Path(__file__).resolve().parent.parent
N, M = geo.TILE_PX, 128
S = N + 2 * M
VARIANT, SEED = "g9b", 1


def _dirs(h):
    src = ROOT / "out" / "g3d" / f"h{h}"
    return src, src / "paint"


def _roofs(a):
    """Pixels that look like a roof or a building wall: red/orange tiles, or bright white/grey."""
    a = a.astype("i4")
    return ((a[..., 0] > a[..., 1] + 30) & (a[..., 0] > 110)) | ((a.min(-1) > 165) & (a.max(-1) - a.min(-1) < 30))


def invented_buildings(art, ren, bm):
    """Share of the tile painted as buildings where neither BD TOPO nor the render has one: the model likes to
    build villages in woods and towers in fields."""
    return float((_roofs(art) & ~bm).mean() - (_roofs(ren) & ~bm).mean())


RURAL_BELOW = 0.4      # tiles whose core is less covered by (grown) real buildings get the countryside prompt
INVENTED_MAX = 0.015


def paint_one(h, i, j):
    src, dst = _dirs(h)
    f = dst / f"{i}_{j}.png"
    if f.exists():
        return False
    try:
        ren = Image.open(src / f"{i}_{j}.png").convert("RGB")
    except OSError:                                   # a render still being written: next pass
        return False
    rn = np.asarray(ren)
    bm = g3d.building_mask(i, j, geo.View(h))
    rural = bm[M:-M, M:-M].mean() < RURAL_BELOW
    best = None
    for attempt in range(4):
        variant = "g9b_rural" if rural or attempt >= 2 else VARIANT
        try:
            art = np.asarray(paint(variant, ren, seed=SEED + attempt))
        except Exception as e:
            print(f"{i}_{j} attempt {attempt}: {e}", flush=True)
            if "balance" in str(e).lower() or "locked" in str(e).lower():
                raise SystemExit("fal balance exhausted")
            time.sleep(5)
            continue
        bad = invented_buildings(art, rn, bm)
        if best is None or bad < best[0]:
            best = (bad, art)
        if bad <= INVENTED_MAX:
            break
        print(f"{i}_{j}: {bad:.3f} of the tile invented ({variant}), repainting", flush=True)
    if best is None:
        return False
    art = best[1]
    wm = g3d.water_mask(i, j, geo.View(h))
    art = unify_water(fix_invented_water(art, wm, rn), wm)
    rf = src / f"{i}_{j}_rails.png"
    art = g3d.overlay_rails(art, Image.open(rf) if rf.exists() else None)
    dst.mkdir(parents=True, exist_ok=True)
    tmp = f.with_suffix(".tmp.png"); Image.fromarray(art).save(tmp); tmp.replace(f)
    return True


def paint_all(h, workers=4):
    load_env()
    src, dst = _dirs(h)
    todo = []
    for i, j in geo.View(h).tiles_for_area():
        r = src / f"{i}_{j}.png"
        if r.exists() and time.time() - r.stat().st_mtime > 10 and not (dst / f"{i}_{j}.png").exists():
            todo.append((i, j))
    print(f"h{h}: {len(todo)} tiles to paint", flush=True)
    done = 0
    with ThreadPoolExecutor(workers) as ex:
        for ok in ex.map(lambda t: paint_one(h, *t), todo):
            done += ok
            if done % 10 == 0 and ok:
                print(f"painted {done}/{len(todo)}", flush=True)
    return done


# ---------------------------------------------------------------- assembly

@lru_cache(maxsize=4096)
def _pair_seam(h, i, j, axis):
    """Cut between tile (i, j) and its right (axis 0) or lower (axis 1) neighbour: for every row (column),
    the position inside the 2M overlap band, or None if the neighbour has no painting."""
    _, dst = _dirs(h)
    a, b = _img(h, i, j), _img(h, i + (axis == 0), j + (axis == 1))
    if a is None or b is None:
        return None
    if axis == 0:
        A, B = a[:, N:N + 2 * M], b[:, :2 * M]
    else:
        A, B = a[N:N + 2 * M].transpose(1, 0, 2), b[:2 * M].transpose(1, 0, 2)
    cost = ndimage.uniform_filter(np.abs(A.astype("f4") - B.astype("f4")).sum(-1), 5)
    cost[:, :GUARD] = cost[:, -GUARD:] = 1e6
    return _seam(cost)


@lru_cache(maxsize=64)
def _img(h, i, j):
    f = _dirs(h)[1] / f"{i}_{j}.png"
    return np.asarray(Image.open(f).convert("RGB")) if f.exists() else None


def _side_weight(cut, along, x):
    """Weight (0..1) of the far painting at band coordinate x (0..2M) for each row `along`."""
    return np.clip((x[None, :] - cut[along][:, None] + FEATHER) / (2 * FEATHER), 0, 1)


def assemble_tile(h, i, j):
    me = _img(h, i, j)
    if me is None:
        return None
    ys, xs = np.arange(M, M + N), np.arange(M, M + N)
    # horizontal neighbour weights over the core: wx > 0 means "take from the left/right neighbour"
    out = me[M:M + N, M:M + N].astype("f4")

    def _shifted(n, di, dj):
        # the core of (i, j) in the coordinates of neighbour (i+di, j+dj): rows/cols M - d*N .. +N, clipped
        y0, x0 = M - dj * N, M - di * N
        canvas = np.zeros((N, N, 3), "f4")
        sy0, sx0 = max(y0, 0), max(x0, 0)
        sy1, sx1 = min(y0 + N, S), min(x0 + N, S)
        if sy1 > sy0 and sx1 > sx0:
            canvas[sy0 - y0:sy1 - y0, sx0 - x0:sx1 - x0] = n[sy0:sy1, sx0:sx1]
        return canvas

    # right / left bands (core columns N-M..N and 0..M lie inside the 2M overlap bands)
    W = {}
    cut = _pair_seam(h, i, j, 0)                       # me | right
    if cut is not None:
        band_x = np.arange(N, N + M)                   # my columns in the band, as band coordinate x - N
        w = _side_weight(cut, ys, band_x - N)
        W[(1, 0)] = np.zeros((N, N), "f4"); W[(1, 0)][:, N - M:] = w
    cut = _pair_seam(h, i - 1, j, 0)                   # left | me
    if cut is not None:
        band_x = np.arange(M, 2 * M)                   # my columns, in the left pair's band: x + N - N = x
        w = 1 - _side_weight(cut, ys, band_x)
        W[(-1, 0)] = np.zeros((N, N), "f4"); W[(-1, 0)][:, :M] = w
    cut = _pair_seam(h, i, j, 1)                       # me / below
    if cut is not None:
        w = _side_weight(cut, xs, np.arange(N, N + M) - N).T
        W[(0, 1)] = np.zeros((N, N), "f4"); W[(0, 1)][N - M:, :] = w
    cut = _pair_seam(h, i, j - 1, 1)                   # above / me
    if cut is not None:
        w = (1 - _side_weight(cut, xs, np.arange(M, 2 * M))).T
        W[(0, -1)] = np.zeros((N, N), "f4"); W[(0, -1)][:M, :] = w
    wx = W.get((1, 0), 0) + W.get((-1, 0), 0)
    wy = W.get((0, 1), 0) + W.get((0, -1), 0)
    wx = np.broadcast_to(wx, (N, N)); wy = np.broadcast_to(wy, (N, N))
    acc = out * ((1 - wx) * (1 - wy))[..., None]
    wsum = (1 - wx) * (1 - wy)
    for di, dj in ((1, 0), (-1, 0), (0, 1), (0, -1)):
        w = W.get((di, dj))
        if w is None:
            continue
        other = wy if di else wx
        ww = w * (1 - other)
        n = _shifted(_img(h, i + di, j + dj), di, dj)
        acc += n * ww[..., None]; wsum = wsum + ww
    for di in (-1, 1):                                  # corners: both a horizontal and a vertical neighbour
        for dj in (-1, 1):
            if (di, 0) in W and (0, dj) in W:
                ww = W[(di, 0)] * W[(0, dj)]
                d = _img(h, i + di, j + dj)
                if d is None:                            # no diagonal: split between the two side neighbours
                    for k, wk in (((di, 0), 0.5), ((0, dj), 0.5)):
                        acc += _shifted(_img(h, i + k[0], j + k[1]), *k) * (ww * wk)[..., None]
                else:
                    acc += _shifted(d, di, dj) * ww[..., None]
                wsum = wsum + ww
    return np.clip(acc / np.maximum(wsum, 1e-6)[..., None], 0, 255).astype("u1")


def assemble_all(h):
    art = ROOT / "out" / f"h{h}" / "art"
    old = ROOT / "out" / f"h{h}" / "art_v1"
    if art.exists() and not old.exists() and not (art / ".v2").exists():
        shutil.move(art, old)                            # keep the first-generation tiles
    art.mkdir(parents=True, exist_ok=True)
    (art / ".v2").touch()
    n = 0
    for i, j in geo.View(h).tiles_for_area():
        t = assemble_tile(h, i, j)
        if t is not None:
            Image.fromarray(t).save(art / f"{i}_{j}.png")
            n += 1
    print(f"h{h}: {n} tiles assembled", flush=True)


def main(argv):
    h = int(argv[0])
    what = argv[1] if len(argv) > 1 else "all"
    if what in ("paint", "all"):
        paint_all(h)
    if what in ("assemble", "all"):
        assemble_all(h)


if __name__ == "__main__":
    main(sys.argv[1:])
