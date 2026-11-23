"""Quality checks on drawn tiles.

Invented water: the redraw model sometimes paints ponds or pools where the render has none.
Water in the render is a known flat blue (render.py), so any water-blue blob in the art that is
far from render water is invented.

Usage: uv run python -m tinynantes.qa [--heading H] [--min-px N] [--save DIR]
Prints one line per suspicious tile: heading, tile, invented-water pixels.
"""
from __future__ import annotations

import argparse
import json
from pathlib import Path

import numpy as np
from PIL import Image
from scipy import ndimage

ROOT = Path(__file__).resolve().parent.parent
M = 128


def water_mask(a: np.ndarray, art: bool) -> np.ndarray:
    r, g, b = (a[..., k].astype("f4") for k in range(3))
    if art:   # drawn water: light/saturated blue
        return (b > 110) & (b > r * 1.35) & (b > g * 1.05)
    return (b > 60) & (b > r * 1.5) & (b > g * 1.1)   # render water colour


def invented_water(h: int, i: int, j: int):
    d = ROOT / "out" / f"h{h}"
    art = np.asarray(Image.open(d / "art" / f"{i}_{j}.png").convert("RGB"))
    ren = np.asarray(Image.open(d / "render_m" / f"{i}_{j}.png").convert("RGB"))[M:M + 1024, M:M + 1024]
    return invented_water_arrays(art, ren)


def invented_water_arrays(art: np.ndarray, ren: np.ndarray):
    """(pixel count, mask) of drawn water far from any render water. `ren` is the render (water is a flat blue)
    or directly a bool mask of where real water is."""
    aw = ndimage.binary_opening(water_mask(art, True), iterations=2)
    real = ren if ren.dtype == bool else water_mask(ren, False)
    rw = ndimage.binary_dilation(real, iterations=24)   # tolerate shoreline drift
    bad = aw & ~rw
    lab, n = ndimage.label(bad)
    if not n:
        return 0, bad
    sizes = ndimage.sum(bad, lab, range(1, n + 1))
    keep = np.isin(lab, 1 + np.flatnonzero(sizes >= 400))   # ignore specks (cars, windows, roofs)
    return int(keep.sum()), keep


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--heading", type=int)
    ap.add_argument("--min-px", type=int, default=1500)
    ap.add_argument("--save")
    a = ap.parse_args()
    heads = [a.heading] if a.heading is not None else range(8)
    found = []
    for h in heads:
        art = ROOT / "out" / f"h{h}" / "art"
        if not art.exists():
            continue
        for f in sorted(art.glob("*.png")):
            parts = f.stem.split("_")
            if len(parts) != 2:
                continue
            i, j = map(int, parts)
            px, mask = invented_water(h, i, j)
            if px >= a.min_px:
                found.append({"h": h, "i": i, "j": j, "px": px})
                print(f"h{h} {i}_{j} invented water: {px} px", flush=True)
                if a.save:
                    out = Path(a.save); out.mkdir(parents=True, exist_ok=True)
                    im = Image.open(f).convert("RGB")
                    ov = np.asarray(im).copy(); ov[mask] = (255, 0, 255)
                    Image.fromarray(ov).resize((512, 512)).save(out / f"h{h}_{i}_{j}.jpg")
    (ROOT / "out" / "qa_water.json").write_text(json.dumps(found))
    print(f"{len(found)} tiles with invented water")


if __name__ == "__main__":
    main()


def fix_invented_water(art: np.ndarray, ren: np.ndarray, src: np.ndarray | None = None) -> np.ndarray:
    """Deterministic repair of drawn water with no water under it (the usual culprit is a sports pitch).
    `ren`: render or real-water bool mask (see invented_water_arrays). With `src` (the photographic render the
    art was painted from), blue things that are blue in the photo too (glass roofs, real pools) are kept, and
    the invented water takes the photo's colours; otherwise it becomes mown grass."""
    _, mask = invented_water_arrays(art, ren)
    if not mask.any():
        return art
    r, g, bl = (art[..., k].astype("f4") for k in range(3))
    bluish = (bl > r * 1.12) & (bl > g * 0.95)
    mask = ndimage.binary_dilation(mask, iterations=10) & bluish | mask
    a = art.astype("f4")
    if src is not None:
        s = src.astype("f4")
        src_blue = ndimage.binary_dilation((s[..., 2] > s[..., 0] * 1.15) & (s[..., 2] > s[..., 1] * 1.0), iterations=3)
        mask &= ~src_blue
        fill = ndimage.median_filter(s, size=(5, 5, 1))
        fill = (fill.mean(-1, keepdims=True) + (fill - fill.mean(-1, keepdims=True)) * 1.3)   # a bit more saturated
    else:
        lum = a.mean(-1, keepdims=True) / 255.0
        fill = np.broadcast_to(np.array([96, 152, 66], "f4") * (0.75 + 0.5 * lum), a.shape)
    out = a.copy()
    out[mask] = fill[mask]
    return np.clip(out, 0, 255).astype("u1")


WATER_RGB = np.array([52, 112, 168], "f4")


def unify_water(art: np.ndarray, real: np.ndarray) -> np.ndarray:
    """Give every patch of real water (bool mask, from g3d.water_mask) the same colour across all tiles,
    keeping a little of the painted texture (ripples, reflections). Only blue-ish painted pixels are touched,
    so boats, bridge decks and pontoons painted over the water stay as they are."""
    a = art.astype("f4")
    r, g, b = a[..., 0], a[..., 1], a[..., 2]
    blue = (b > r * 1.08) & (b > g * 0.92) & (b > 60)
    m = ndimage.binary_erosion(real, iterations=3) & blue
    if not m.any():
        return art
    lum = a.mean(-1)
    ref = ndimage.uniform_filter(np.where(m, lum, 0), 61) / np.maximum(ndimage.uniform_filter(m.astype("f4"), 61), 1e-3)
    tex = np.clip((lum - ref) / 60.0, -0.25, 0.25)          # local detail only, not the patch's overall tone
    col = WATER_RGB[None, None, :] * (1.0 + tex[..., None])
    soft = ndimage.gaussian_filter(m.astype("f4"), 1.5)[..., None]
    out = a * (1 - soft) + col * soft
    return np.clip(out, 0, 255).astype("u1")
