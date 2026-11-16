"""Build stylized tiles in raster order, feeding finished neighbours into each tile's margin
so FLUX.2 Klein continues them and the tiles line up without seams.

Usage: uv run python -m tinynantes.build [--heading H] I0 J0 I1 J1      (inclusive tile range)
       uv run python -m tinynantes.build [--heading H|all] --area
Writes out/hH/render_m/I_J.png (render with margin) and out/hH/art/I_J.png (1024 px core).
Headings other than 0 take a heading-0 tile as a style reference wherever a tile has no neighbour yet.
"""
from __future__ import annotations

import sys
import time
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

import numpy as np
from PIL import Image
from scipy import ndimage

from . import geo
from .render import render_tile
from .stylize import NO_RAILS_PROMPT, PROMPT, RAILS_PROMPT, load_env, stylize

ROOT = Path(__file__).resolve().parent.parent
M = 128
N = geo.TILE_PX
HEADING = 0


def hdir():
    return ROOT / "out" / f"h{HEADING}"
SEAM_PROMPT = (
    " Parts of image 1 that are already drawn as pixel art must stay exactly as they are; redraw only the "
    "realistic rendered parts and continue the existing pixel art seamlessly across the boundary."
)


def rendered(i, j):
    f = hdir() / "render_m" / f"{i}_{j}.png"
    if not f.exists():
        f.parent.mkdir(parents=True, exist_ok=True)
        img = render_tile(i, j, geo.View(HEADING), margin_px=M)
        img.save(f)
        if img.info["rails"]:
            f.with_suffix(".rails").touch()
    return Image.open(f).convert("RGB")


def _render_job(args):
    """Runs in the render child process."""
    global HEADING
    HEADING, i, j = args
    rendered(i, j)


def has_rails(i, j):
    return (hdir() / "render_m" / f"{i}_{j}.rails").exists()


def art_path(i, j):
    return hdir() / "art" / f"{i}_{j}.png"


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
        nb = np.asarray(Image.open(f).convert("RGB"), "f4").copy()
        if di == -1:   # strip: our x in [0, M), y in core -> their x in [N-M, N)
            strip, a = o[M:M + N, 0:M], ramp[None, :, None]
            nb[:, N - M:] = nb[:, N - M:] * (1 - a) + strip * a
        else:          # our y in [0, M), x in core -> their y in [N-M, N)
            strip, a = o[0:M, M:M + N], ramp[:, None, None]
            nb[N - M:, :] = nb[N - M:, :] * (1 - a) + strip * a
        _save(Image.fromarray(np.clip(nb, 0, 255).astype("u1")), f)


def _save(img, f):
    tmp = f.with_suffix(".tmp.png")
    img.save(tmp)
    tmp.replace(f)   # atomic: readers never see a half-written file


def build_wave(wave, workers=6):
    """Tiles on one anti-diagonal: compose all inputs first, draw them in parallel, then save
    and feather one by one, so no file is read while another thread writes it."""
    todo = [t for t in wave if not art_path(*t).exists()]
    if not todo:
        return
    inputs = {t: compose(*t) for t in todo}

    ref = style_ref()

    def draw(t):
        src, mask, n = inputs[t]
        for attempt in range(4):
            try:
                prompt = PROMPT + (RAILS_PROMPT if has_rails(*t) else NO_RAILS_PROMPT) + (SEAM_PROMPT if n else "")
                out = stylize(src, ref=None if n else ref, prompt=prompt, seed=7 + attempt)
                return match_seams(out, src, mask)
            except Exception as e:   # network / fal hiccup: retry, then give up on this tile only
                print("retry", t, attempt, repr(e)[:200], flush=True)
                time.sleep(5 * (attempt + 1))
        return None

    with ThreadPoolExecutor(workers) as ex:
        outs = {t: o for t, o in zip(todo, ex.map(draw, todo)) if o is not None}
    (hdir() / "art_m").mkdir(parents=True, exist_ok=True)
    art_path(0, 0).parent.mkdir(parents=True, exist_ok=True)
    for (i, j), out in outs.items():
        _save(out, hdir() / "art_m" / f"{i}_{j}.png")
        _save(out.crop((M, M, M + N, M + N)), art_path(i, j))
        feather_into_neighbours(i, j, out)
        print("art", i, j, f"({inputs[(i, j)][2]} neighbours)", flush=True)


def build(tiles):
    """Diagonal wavefront: tiles on the same anti-diagonal don't touch their left/top
    dependencies, so they can be drawn in parallel."""
    tiles = sorted(tiles, key=lambda t: (t[0] + t[1], t[1]))
    # fetch the IGN data in parallel first
    from . import ign
    boxes = [geo.View(HEADING).ground_bbox(i * geo.TILE_M - 50, j * geo.TILE_M - 50, (i + 1) * geo.TILE_M + 50, (j + 1) * geo.TILE_M + 50) for i, j in tiles]
    ign.prefetch(boxes)
    # pre-render in a child process recycled every 20 tiles: the Intel EGL driver keeps ~60 MB of
    # GPU buffers per render until the process exits (this OOM-killed the desktop session once)
    import multiprocessing as mp
    from concurrent.futures import ProcessPoolExecutor
    todo = [t for t in tiles if not (hdir() / "render_m" / f"{t[0]}_{t[1]}.png").exists()]
    with ProcessPoolExecutor(1, mp_context=mp.get_context("spawn"), max_tasks_per_child=20) as ex:
        for k, _ in enumerate(ex.map(_render_job, [(HEADING, i, j) for i, j in todo])):
            print(f"render {k + 1}/{len(todo)}", flush=True)
    waves = {}
    for i, j in tiles:
        waves.setdefault(i + j, []).append((i, j))
    for d in sorted(waves):
        build_wave(waves[d])


def style_ref():
    """A heading-0 tile of the old town, to anchor the look of the other headings."""
    if HEADING == 0:
        return None
    f = ROOT / "out" / "h0" / "art" / "0_0.png"
    return Image.open(f).convert("RGB") if f.exists() else None


def main(argv):
    global HEADING
    load_env()
    heads = [0]
    if argv[:1] == ["--heading"]:
        heads = list(range(8)) if argv[1] == "all" else [int(argv[1])]
        argv = argv[2:]
    for h in heads:
        HEADING = h
        print(f"== heading {h}", flush=True)
        if argv and argv[0] == "--area":
            tiles = geo.View(h).tiles_for_area()
        else:
            i0, j0, i1, j1 = map(int, argv)
            tiles = [(i, j) for j in range(j0, j1 + 1) for i in range(i0, i1 + 1)]
        build(tiles)


if __name__ == "__main__":
    main(sys.argv[1:])
