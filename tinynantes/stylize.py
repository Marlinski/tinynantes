"""Redraw rendered tiles as isometric pixel art with FLUX.2 [klein] on fal.ai.

Usage: uv run python -m tinynantes.stylize [--model 9b|4b] [--ref path.png] [--seed N] I J [I J ...]
Reads out/h0/render/I_J.png, writes out/h0/art/I_J.png. FAL_KEY comes from .env (never committed).
"""
from __future__ import annotations

import argparse
import io
import os
from pathlib import Path

import requests
from PIL import Image

ROOT = Path(__file__).resolve().parent.parent

PROMPT = (
    "Redraw image 1 as a detailed isometric pixel art illustration of a European city, like a polished "
    "16-bit city-builder game: crisp pixel edges, clean flat shading, limited warm palette, no photo noise. "
    "Blue-grey slate roofs with dormers, cream limestone facades with neat rows of small windows, "
    "round fluffy pixel trees, clean grey asphalt streets with white lane markings, tiny pixel cars and people, "
    "green lawns. The flat blue areas of image 1 are the river Loire and its arms: draw them as calm blue water with soft ripples and keep their exact shape; draw water nowhere else. Do not invent railway tracks. Keep exactly the same layout, camera angle, geometry, buildings, streets and "
    "composition as image 1. No text, no labels, no border."
)
REF_PROMPT = " Match the pixel art style, palette and level of detail of image 2 exactly."


def load_env():
    f = ROOT / ".env"
    if f.exists():
        for line in f.read_text().splitlines():
            if "=" in line and not line.startswith("#"):
                k, v = line.split("=", 1)
                os.environ.setdefault(k.strip(), v.strip())


def stylize(img: Image.Image, ref: Image.Image | None = None, model="9b", seed=None, prompt=PROMPT) -> Image.Image:
    import fal_client
    urls = []
    for im in [img] + ([ref] if ref is not None else []):
        buf = io.BytesIO(); im.convert("RGB").save(buf, "PNG")
        urls.append(fal_client.upload(buf.getvalue(), "image/png"))
    args = {
        "prompt": prompt + (REF_PROMPT if ref is not None else ""),
        "image_urls": urls,
        "image_size": {"width": img.width, "height": img.height},
        "num_images": 1,
        "output_format": "png",
    }
    if seed is not None:
        args["seed"] = seed
    res = fal_client.subscribe(f"fal-ai/flux-2/klein/{model}/edit", arguments=args)
    url = res["images"][0]["url"]
    out = Image.open(io.BytesIO(requests.get(url, timeout=120).content)).convert("RGB")
    if out.size != img.size:
        out = out.resize(img.size, Image.LANCZOS)
    return out


def main():
    load_env()
    ap = argparse.ArgumentParser()
    ap.add_argument("--model", default="9b")
    ap.add_argument("--ref")
    ap.add_argument("--seed", type=int)
    ap.add_argument("--suffix", default="")
    ap.add_argument("nums", nargs="+", type=int)
    a = ap.parse_args()
    ref = Image.open(a.ref) if a.ref else None
    out = ROOT / "out" / "h0" / "art"
    out.mkdir(parents=True, exist_ok=True)
    for i, j in zip(a.nums[0::2], a.nums[1::2]):
        src = Image.open(ROOT / "out" / "h0" / "render" / f"{i}_{j}.png")
        stylize(src, ref, a.model, a.seed).save(out / f"{i}_{j}{a.suffix}.png")
        print("done", i, j, flush=True)


if __name__ == "__main__":
    main()
