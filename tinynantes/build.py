"""Build stylized tiles in raster order, feeding finished neighbours into each tile's margin
so FLUX.2 Klein continues them and the tiles line up without seams.

Usage: uv run python -m tinynantes.build I0 J0 I1 J1      (inclusive tile range)
       uv run python -m tinynantes.build --area
Writes out/h0/render_m/I_J.png (render with margin) and out/h0/art/I_J.png (1024 px core).
"""
from __future__ import annotations

import sys
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

import numpy as np
from PIL import Image
from scipy import ndimage

from . import geo
from .render import render_tile
from .stylize import PROMPT, load_env, stylize

ROOT = Path(__file__).resolve().parent.parent
M = 128
N = geo.TILE_PX
H0 = ROOT / "out" / "h0"
SEAM_PROMPT = (
    " Parts of image 1 that are already drawn as pixel art must stay exactly as they are; redraw only the "
    "realistic rendered parts and continue the existing pixel art seamlessly across the boundary."
)


def rendered(i, j):
    f = H0 / "render_m" / f"{i}_{j}.png"
    if not f.exists():
        f.parent.mkdir(parents=True, exist_ok=True)
        render_tile(i, j, geo.View(0), margin_px=M).save(f)
    return Image.open(f).convert("RGB")


def art_path(i, j):
    return H0 / "art" / f"{i}_{j}.png"


def compose(i, j):
    """Render with finished neighbours pasted over it. Returns (image, pasted-mask, count)."""
    img = rendered(i, j).copy()
    mask = np.zeros((img.height, img.width), bool)
    n = 0
    for dj in (-1, 0, 1):
        for di in (-1, 0, 1):
            f = art_path(i + di, j + dj)
            # only neighbours from earlier diagonal waves (same-wave tiles are drawn concurrently)
            if di + dj < 0 and f.exists():
                x, y = M + di * N, M + dj * N
                img.paste(Image.open(f).convert("RGB"), (x, y))
                mask[max(y, 0):y + N, max(x, 0):x + N] = True
                n += 1
    return img, mask, n


def match_seams(out, src, mask, reach=300, sigma=20):
    """Pull the new drawing's colours toward the pasted neighbours: the colour difference measured
    where the neighbours were pasted is smoothed and faded into the tile over `reach` pixels."""
    if not mask.any():
        return out
    o = np.asarray(out, "f4"); s = np.asarray(src, "f4")
    w = ndimage.gaussian_filter(mask.astype("f4"), sigma)
    diff = np.stack([ndimage.gaussian_filter(np.where(mask, s[..., c] - o[..., c], 0), sigma) for c in range(3)], -1)
    diff /= np.maximum(w, 1e-3)[..., None]
    # extend the field beyond the mask edge: nearest known value
    _, (iy, ix) = ndimage.distance_transform_edt(~mask, return_indices=True)
    field = diff[iy, ix]
    field = np.stack([ndimage.gaussian_filter(field[..., c], sigma * 2) for c in range(3)], -1)
    dist = ndimage.distance_transform_edt(~mask)
    fade = np.clip(1 - dist / reach, 0, 1) ** 1.5
    o = o + field * fade[..., None]
    return Image.fromarray(np.clip(o, 0, 255).astype("u1"))


_lock = __import__("threading").Lock()


def feather_into_neighbours(i, j, out):
    """Dissolve this tile's left/top margins into the finished neighbours' edges, so content changes
    gradually over M pixels instead of switching at the tile boundary."""
    o = np.asarray(out, "f4")
    ramp = (np.arange(M, dtype="f4") + 0.5) / M          # 0 far from us -> 1 at the shared edge
    ramp = ramp * ramp * (3 - 2 * ramp)                  # smoothstep
    for di, dj in ((-1, 0), (0, -1)):
        f = art_path(i + di, j + dj)
        if not f.exists():
            continue
        with _lock:
            nb = np.asarray(Image.open(f).convert("RGB"), "f4").copy()
            if di == -1:   # strip: our x in [0, M), y in core -> their x in [N-M, N)
                strip, a = o[M:M + N, 0:M], ramp[None, :, None]
                nb[:, N - M:] = nb[:, N - M:] * (1 - a) + strip * a
            else:          # our y in [0, M), x in core -> their y in [N-M, N)
                strip, a = o[0:M, M:M + N], ramp[:, None, None]
                nb[N - M:, :] = nb[N - M:, :] * (1 - a) + strip * a
            Image.fromarray(np.clip(nb, 0, 255).astype("u1")).save(f)


def build_tile(i, j):
    if art_path(i, j).exists():
        return
    src, mask, n = compose(i, j)
    out = stylize(src, prompt=PROMPT + (SEAM_PROMPT if n else ""), seed=7)
    out = match_seams(out, src, mask)
    art_path(i, j).parent.mkdir(parents=True, exist_ok=True)
    (H0 / "art_m").mkdir(exist_ok=True)
    out.save(H0 / "art_m" / f"{i}_{j}.png")
    out.crop((M, M, M + N, M + N)).save(art_path(i, j))
    feather_into_neighbours(i, j, out)
    print("art", i, j, f"({n} neighbours)", flush=True)


def build(tiles):
    """Diagonal wavefront: tiles on the same anti-diagonal don't touch their left/top
    dependencies, so they can be drawn in parallel."""
    tiles = sorted(tiles, key=lambda t: (t[0] + t[1], t[1]))
    # fetch the IGN data in parallel first
    from . import ign
    boxes = [geo.View(0).ground_bbox(i * geo.TILE_M - 50, j * geo.TILE_M - 50, (i + 1) * geo.TILE_M + 50, (j + 1) * geo.TILE_M + 50) for i, j in tiles]
    ign.prefetch(boxes)
    # pre-render sequentially (single GL context)
    for k, (i, j) in enumerate(tiles):
        rendered(i, j)
        print(f"render {k + 1}/{len(tiles)}", flush=True)
    waves = {}
    for i, j in tiles:
        waves.setdefault(i + j, []).append((i, j))
    with ThreadPoolExecutor(6) as ex:
        for d in sorted(waves):
            list(ex.map(lambda t: build_tile(*t), waves[d]))


def main(argv):
    load_env()
    if argv and argv[0] == "--area":
        tiles = geo.View(0).tiles_for_area()
    else:
        i0, j0, i1, j1 = map(int, argv)
        tiles = [(i, j) for j in range(j0, j1 + 1) for i in range(i0, i1 + 1)]
    build(tiles)


if __name__ == "__main__":
    main(sys.argv[1:])
