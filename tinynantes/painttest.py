"""Paint a few render2 tiles with several painters and lay them out side by side.

Usage: uv run python -m tinynantes.painttest H I J [I J ...]  ->  out/painttest/<variant>/hH_I_J.png + sheet_*.jpg
Variants:
  fastcello  distilled FLUX.2 klein 4B (4 steps) + handful/fastcello LoRA     ~$0.02/tile
  mndf       FLUX.2 klein base 4B (28 steps) + handful/mndf-pixel LoRA         ~$0.03/tile
  ours9b     FLUX.2 klein 9B edit with our long prompt (the current pipeline)
"""
from __future__ import annotations

import io
import sys
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

import requests
from PIL import Image

from .stylize import PROMPT, load_env

ROOT = Path(__file__).resolve().parent.parent
OUT = ROOT / "out" / "painttest"
HF = "https://huggingface.co/handful/{}/resolve/main/pytorch_lora_weights.safetensors"
VARIANTS = {
    "fastcello": ("fal-ai/flux-2/klein/4b/edit/lora", {
        "prompt": "Repaint this 3D map render as mgwarm warm pixel-art map, keeping every road, building and tree in place",
        "loras": [{"path": HF.format("fastcello-flux2-klein-4b"), "scale": 1.0}],
        "num_inference_steps": 4, "guidance_scale": 1.0}),
    "mndf": ("fal-ai/flux-2/klein/4b/base/edit/lora", {
        "prompt": "Repaint this 3D map render as mndfstyle isometric pixel-art map, keeping every road, building and tree in place",
        "loras": [{"path": HF.format("mndf-pixel-flux2-klein-4b"), "scale": 1.0}],
        "num_inference_steps": 28, "guidance_scale": 1.5}),
    "ours9b": ("fal-ai/flux-2/klein/9b/edit", {"prompt": PROMPT}),
    "min9b": ("fal-ai/flux-2/klein/9b/edit", {"prompt": (
        "Repaint this 3D map render as a detailed isometric pixel-art city map, keeping every road, rail track, "
        "building, roof colour, tree and body of water exactly in place. Add nothing.")}),
    "g9b": ("fal-ai/flux-2/klein/9b/edit", {"prompt": (
        "Repaint this 3D city render as a detailed isometric pixel-art city map. Keep every street, building, "
        "roof colour, tree and body of water exactly where it is. Do not add or remove anything.")}),
    "fastcello06": ("fal-ai/flux-2/klein/4b/edit/lora", {
        "prompt": "Repaint this 3D map render as mgwarm warm pixel-art map, keeping every road, building and tree in place",
        "loras": [{"path": HF.format("fastcello-flux2-klein-4b"), "scale": 0.6}],
        "num_inference_steps": 4, "guidance_scale": 1.0}),
}


def paint(variant, img, seed=0):
    import fal_client
    ep, extra = VARIANTS[variant]
    buf = io.BytesIO(); img.save(buf, "PNG")
    url = fal_client.upload(buf.getvalue(), "image/png")
    args = {"image_urls": [url], "image_size": {"width": img.width, "height": img.height}, "num_images": 1,
            "output_format": "png", "seed": seed, "enable_safety_checker": False, **extra}
    res = fal_client.subscribe(ep, arguments=args)
    out = Image.open(io.BytesIO(requests.get(res["images"][0]["url"], timeout=120).content)).convert("RGB")
    return out.resize(img.size, Image.LANCZOS) if out.size != img.size else out


def main(argv):
    load_env()
    h = int(argv[0])
    variants = [v for v in VARIANTS if v in argv] or list(VARIANTS)
    nums = [int(a) for a in argv[1:] if a.lstrip("-").isdigit()]
    tiles = list(zip(nums[0::2], nums[1::2]))
    jobs = []
    for i, j in tiles:
        src = Image.open(ROOT / "out" / "r2" / f"h{h}" / f"{i}_{j}.png").convert("RGB").crop((128, 128, 1152, 1152))
        for v in variants:
            f = OUT / v / f"h{h}_{i}_{j}.png"
            if not f.exists():
                jobs.append((v, src, f))

    def run(job):
        v, src, f = job
        f.parent.mkdir(parents=True, exist_ok=True)
        paint(v, src).save(f)
        print("done", v, f.name, flush=True)

    with ThreadPoolExecutor(6) as ex:
        list(ex.map(run, jobs))
    # one sheet per tile: render | variants
    for i, j in tiles:
        ims = [Image.open(ROOT / "out" / "r2" / f"h{h}" / f"{i}_{j}.png").crop((128, 128, 1152, 1152))]
        ims += [Image.open(OUT / v / f"h{h}_{i}_{j}.png") for v in variants]
        sheet = Image.new("RGB", (1024 * len(ims), 1024))
        for k, im in enumerate(ims):
            sheet.paste(im, (1024 * k, 0))
        sheet.save(OUT / f"sheet_h{h}_{i}_{j}.jpg", quality=90)


if __name__ == "__main__":
    main(sys.argv[1:])


def seam_test(h, tiles, variant="min9b", M=128, N=1024, src="r2"):
    """Paint each tile independently from its full render (tile + M px context), then blend the overlaps."""
    from concurrent.futures import ThreadPoolExecutor
    import numpy as np
    from .qa import fix_invented_water
    load_env()
    d = OUT / ("seam" if src == "r2" else f"seam_{src}")
    d.mkdir(parents=True, exist_ok=True)

    def one(t):
        i, j = t
        f = d / f"h{h}_{i}_{j}.png"
        if not f.exists():
            paint(variant, Image.open(ROOT / "out" / src / f"h{h}" / f"{i}_{j}.png").convert("RGB")).save(f)
        return t
    list(ThreadPoolExecutor(4).map(one, tiles))
    i0, j0 = min(t[0] for t in tiles), min(t[1] for t in tiles)
    i1, j1 = max(t[0] for t in tiles), max(t[1] for t in tiles)
    W, H = (i1 - i0 + 1) * N + 2 * M, (j1 - j0 + 1) * N + 2 * M
    acc, wsum = np.zeros((H, W, 3), "f4"), np.zeros((H, W, 1), "f4")
    S = N + 2 * M
    ramp = np.clip((np.arange(S) + 0.5) / (2 * M), 0, 1)          # 0 at the outer edge -> 1 at M px inside
    ramp = np.minimum(ramp, ramp[::-1]) ** 1.0
    wt = (ramp[:, None] * ramp[None, :])[..., None] + 1e-4
    for i, j in tiles:
        art = np.asarray(Image.open(d / f"h{h}_{i}_{j}.png").convert("RGB"))
        ren = np.asarray(Image.open(ROOT / "out" / src / f"h{h}" / f"{i}_{j}.png").convert("RGB"))
        art = fix_invented_water(art, ren).astype("f4")
        x, y = (i - i0) * N, (j - j0) * N
        acc[y:y + S, x:x + S] += art * wt
        wsum[y:y + S, x:x + S] += wt
    out = Image.fromarray(np.clip(acc / np.maximum(wsum, 1e-6), 0, 255).astype("u1")).crop((M, M, W - M, H - M))
    out.save(OUT / (f"seam_h{h}.png" if src == "r2" else f"seam_{src}_h{h}.png"))
    return out
