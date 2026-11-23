"""Gather what the places on the map publish about themselves: restaurant menus (with prices), PDF menus,
opening hours... from their own websites (the `website` tag in OpenStreetMap).

Polite crawler: robots.txt is honoured, one request per second per site, a handful of pages per place,
an identifying User-Agent. Extraction, in order of preference:
  1. schema.org Menu / MenuItem JSON-LD embedded in the page
  2. lines that carry a price (12 €, 12,50€, €9) grouped under the nearest heading
  3. links to PDF menus (their text is extracted when possible)

Output: server data dir places/<osm id>.json, read by the server's /api/poi/<id>.
Usage: uv run python -m tinynantes.places [--limit N] [--out DIR] [--force]
"""
from __future__ import annotations

import argparse
import io
import json
import re
import threading
import time
import urllib.robotparser
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
from urllib.parse import urljoin, urlparse

import requests

ROOT = Path(__file__).resolve().parent.parent
UA = "TinyNantesBot/0.1 (+https://tinynantes.fr; map of Nantes, shows menus with a link to the source)"
KINDS = {"amenity=restaurant", "amenity=fast_food", "amenity=cafe", "amenity=bar", "amenity=pub",
         "amenity=ice_cream", "amenity=biergarten", "shop=bakery", "shop=chocolate", "shop=cheese", "shop=deli",
         "shop=wine", "amenity=cinema", "amenity=theatre", "amenity=nightclub"}
MENU_WORDS = re.compile(r"(la[-_ ]?carte|\bcarte\b|menus?\b|nos[-_ ]plats|formules?|ardoise|plats|food|drinks?|boissons|"
                        r"tarifs?|prix|dejeuner|déjeuner|diner|dîner|brunch|programme|seances|séances|horaires)", re.I)
PRICE = re.compile(r"(?:(\d{1,3}(?:\s?[.,]\s?\d{1,2})?)\s?(?:€|eur\b|euros?\b))|(?:€\s?(\d{1,3}(?:\s?[.,]\s?\d{1,2})?))", re.I)
WORDY = re.compile(r"[^\W\d_]{3}")   # an item name has at least one 3-letter word
_last_hit: dict[str, float] = {}
_lock = threading.Lock()
_robots: dict[str, urllib.robotparser.RobotFileParser | None] = {}
http = requests.Session()
http.headers.update({"User-Agent": UA, "Accept-Language": "fr-FR,fr;q=0.9,en;q=0.5"})


def allowed(url: str) -> bool:
    u = urlparse(url)
    base = f"{u.scheme}://{u.netloc}"
    if base not in _robots:
        rp = urllib.robotparser.RobotFileParser()
        try:
            r = http.get(base + "/robots.txt", timeout=10)
            rp.parse(r.text.splitlines() if r.status_code == 200 else [])
        except requests.RequestException:
            rp.parse([])
        _robots[base] = rp
    rp = _robots[base]
    return rp.can_fetch(UA, url) if rp else True


def get(url: str):
    host = urlparse(url).netloc
    with _lock:
        wait = _last_hit.get(host, 0) + 1.0 - time.time()
        _last_hit[host] = max(time.time(), _last_hit.get(host, 0) + 1.0)
    if wait > 0:
        time.sleep(wait)
    if not allowed(url):
        return None
    try:
        r = http.get(url, timeout=15, allow_redirects=True)
    except requests.RequestException:
        return None
    if r.status_code != 200 or len(r.content) > 15_000_000:
        return None
    return r


def norm_price(s: str) -> float | None:
    try:
        return float(re.sub(r"\s", "", s).replace(",", "."))
    except ValueError:
        return None


def from_jsonld(soup):
    """schema.org Menu -> sections"""
    out = []
    for tag in soup.find_all("script", type="application/ld+json"):
        try:
            data = json.loads(tag.string or "")
        except (json.JSONDecodeError, TypeError):
            continue
        stack = data if isinstance(data, list) else [data]
        while stack:
            d = stack.pop()
            if isinstance(d, list):
                stack += d; continue
            if not isinstance(d, dict):
                continue
            t = d.get("@type")
            if t in ("Menu", "MenuSection") or (isinstance(t, list) and "Menu" in t):
                for sec in d.get("hasMenuSection", []) or [d]:
                    items = []
                    for it in sec.get("hasMenuItem", []) or []:
                        off = it.get("offers") or {}
                        if isinstance(off, list):
                            off = off[0] if off else {}
                        items.append({"name": it.get("name", ""), "desc": (it.get("description") or "")[:200],
                                      "price": norm_price(str(off.get("price", "")))})
                    if items:
                        out.append({"title": sec.get("name", ""), "items": items})
            for v in d.values():
                if isinstance(v, (dict, list)):
                    stack.append(v)
    return out


def from_text(soup):
    """Lines with a price, grouped under the closest preceding heading."""
    for s in soup(["script", "style", "noscript", "nav", "footer", "header", "form"]):
        s.decompose()
    sections, cur = [], {"title": "", "items": []}
    seen = set()
    for el in soup.find_all(["h1", "h2", "h3", "h4", "p", "li", "td", "tr", "div", "span"]):
        if el.name in ("h1", "h2", "h3", "h4"):
            title = el.get_text(" ", strip=True)[:80]
            if title:
                if cur["items"]:
                    sections.append(cur)
                cur = {"title": title, "items": []}
            continue
        if el.find(["p", "li", "tr", "div"]):      # only leaf-ish blocks
            continue
        line = re.sub(r"\s+", " ", el.get_text(" ", strip=True))
        if not (4 <= len(line) <= 220) or line in seen:
            continue
        m = PRICE.search(line)
        if not m:
            continue
        seen.add(line)
        name = PRICE.sub("", line).strip(" -–—.:…|/,")
        if len(name) < 3 or not WORDY.search(name):
            continue
        cur["items"].append({"name": name[:140], "price": norm_price(m.group(1) or m.group(2))})
    if cur["items"]:
        sections.append(cur)
    # a real menu has several priced lines
    return sections if sum(len(s["items"]) for s in sections) >= 4 else []


def pdf_text(content: bytes) -> str:
    try:
        from pypdf import PdfReader
        r = PdfReader(io.BytesIO(content))
        return "\n".join((p.extract_text() or "") for p in r.pages[:6])[:20000]
    except Exception:
        return ""


def pdf_items(text: str):
    items = []
    for line in text.splitlines():
        line = re.sub(r"\s+", " ", line).strip()
        m = PRICE.search(line)
        if m and 4 <= len(line) <= 160:
            name = PRICE.sub("", line).strip(" -–—.:…|/,")
            if len(name) >= 3 and WORDY.search(name):
                items.append({"name": name[:140], "price": norm_price(m.group(1) or m.group(2))})
    return [{"title": "", "items": items}] if len(items) >= 4 else []


def crawl(poi):
    from bs4 import BeautifulSoup
    t = poi["tags"]
    site = t.get("website") or t.get("contact:website")
    if not site:
        return None
    if not site.startswith("http"):
        site = "https://" + site
    out = {"id": poi["id"], "name": t.get("name"), "kind": poi["kind"], "website": site, "fetched": int(time.time()),
           "sections": [], "pdfs": [], "menu_url": None, "pages": []}
    r = get(site)
    if r is None:
        out["error"] = "site injoignable ou refusé par robots.txt"
        return out
    host = urlparse(r.url).netloc
    pages = [(r.url, r)]
    soup = BeautifulSoup(r.content, "lxml")
    meta = lambda *names: next((m.get("content") for n in names for m in soup.find_all("meta", attrs={"property": n})
                                + soup.find_all("meta", attrs={"name": n}) if m.get("content")), None)
    out["description"] = (meta("og:description", "description") or "")[:400] or None
    img = meta("og:image", "twitter:image")
    out["image"] = urljoin(r.url, img) if img else None
    cand = []
    for a in soup.find_all("a", href=True):
        href = urljoin(r.url, a["href"])
        u = urlparse(href)
        if u.scheme not in ("http", "https"):
            continue
        label = (a.get_text(" ", strip=True) + " " + u.path).lower()
        if href.lower().split("?")[0].endswith(".pdf") and MENU_WORDS.search(label):
            out["pdfs"].append(href)
        elif u.netloc == host and MENU_WORDS.search(label) and href not in cand and href.split("#")[0] != r.url:
            cand.append(href.split("#")[0])
    for href in cand[:3]:
        p = get(href)
        if p is not None and "html" in p.headers.get("content-type", ""):
            pages.append((p.url, p))
            for a in BeautifulSoup(p.content, "lxml").find_all("a", href=True):
                h2 = urljoin(p.url, a["href"])
                if h2.lower().split("?")[0].endswith(".pdf") and h2 not in out["pdfs"]:
                    out["pdfs"].append(h2)
    best = []
    for url, p in pages:
        s = BeautifulSoup(p.content, "lxml")
        secs = from_jsonld(s) or from_text(s)
        if sum(len(x["items"]) for x in secs) > sum(len(x["items"]) for x in best):
            best, out["menu_url"] = secs, url
        out["pages"].append(url)
    if not best:
        for pdf in out["pdfs"][:2]:
            p = get(pdf)
            if p is not None:
                secs = pdf_items(pdf_text(p.content))
                if secs:
                    best, out["menu_url"] = secs, pdf
                    break
    out["sections"] = [{"title": s["title"], "items": s["items"][:60]} for s in best[:12]]
    out["pdfs"] = out["pdfs"][:5]
    return out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--limit", type=int)
    ap.add_argument("--out", default=str(ROOT / "server" / "data" / "places"))
    ap.add_argument("--force", action="store_true")
    a = ap.parse_args()
    out = Path(a.out); out.mkdir(parents=True, exist_ok=True)
    pois = json.loads((ROOT / "cache" / "live" / "osm_area.json").read_text())["pois"]
    todo = [p for p in pois if p["kind"] in KINDS and (p["tags"].get("website") or p["tags"].get("contact:website"))]
    todo = [p for p in todo if a.force or not (out / f"{p['id']}.json").exists()]
    if a.limit:
        todo = todo[:a.limit]
    print(f"{len(todo)} places to visit", flush=True)
    done = menus = 0

    def one(p):
        try:
            r = crawl(p)
        except Exception as e:      # one broken site must not stop the crawl
            r = {"id": p["id"], "name": p["tags"].get("name"), "error": f"{type(e).__name__}"}
        if r is not None:
            (out / f"{p['id']}.json").write_text(json.dumps(r, ensure_ascii=False))
        return r

    with ThreadPoolExecutor(8) as ex:
        for r in ex.map(one, todo):
            done += 1
            if r and r.get("sections"):
                menus += 1
            if done % 20 == 0:
                print(f"{done}/{len(todo)} visited, {menus} menus", flush=True)
    print(f"done: {done} visited, {menus} with a menu", flush=True)


if __name__ == "__main__":
    main()
