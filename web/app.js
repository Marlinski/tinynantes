// Tiny Nantes viewer: pans and zooms the isometric tile pyramid in data/h0/.
// World units are "screen metres" (see tinynantes/geo.py): tile (i, j) at level L covers
// x in [i, i+1) * tile_m * 2^L and y (downwards) likewise.

const $ = (id) => document.getElementById(id);
const cv = $("map"), ctx = cv.getContext("2d");
const lab = $("labels"), lctx = lab.getContext("2d");
const DATA = "data/h0/";

let man, places = [], dpr = 1;
const view = { x: 0, y: 0, s: 1 };     // centre (metres) and scale (CSS px per metre)
let minS = 0.05, maxS = 8;
let marker = null;                      // { x, y, name }
let showLabels = true;
const cache = new Map();                // "L/i_j" -> { img, ready }
let dirty = true;

// ---------------------------------------------------------------- loading
async function init() {
  man = await (await fetch(DATA + "manifest.json")).json();
  try { places = await (await fetch("data/places.json")).json(); } catch { places = []; }
  const [i0, j0, i1, j1] = man.bounds, T = man.tile_m;
  man.box = [i0 * T, j0 * T, (i1 + 1) * T, (j1 + 1) * T];
  man.sets = [new Set(man.tiles)];
  for (let L = 1; L < man.levels; L++) man.sets[L] = new Set(man.level_tiles[L] || []);
  maxS = 4 / (T / man.tile_px);                         // up to 4 screen px per art px
  resize();
  if (!readHash()) home();
  initMini();
  requestAnimationFrame(frame);
}

function tile(L, i, j) {
  const k = `${L}/${i}_${j}`;
  let t = cache.get(k);
  if (!t) {
    t = { img: new Image(), ready: false, used: 0 };
    t.img.decoding = "async";
    t.img.onload = () => { t.ready = true; dirty = true; };
    t.img.onerror = () => { t.failed = true; };
    t.img.src = `${DATA}${L}/${i}_${j}.webp`;
    cache.set(k, t);
    if (cache.size > 400) evict();
  }
  t.used = performance.now();
  return t;
}
function evict() {
  const arr = [...cache.entries()].sort((a, b) => a[1].used - b[1].used);
  for (const [k] of arr.slice(0, 100)) cache.delete(k);
}
function exists(L, i, j) { return man.sets[L].has(`${i}_${j}`); }

// ---------------------------------------------------------------- drawing
function resize() {
  dpr = Math.min(devicePixelRatio || 1, 2);
  for (const c of [cv, lab]) { c.width = Math.round(innerWidth * dpr); c.height = Math.round(innerHeight * dpr); }
  minS = Math.min(innerWidth / (man.box[2] - man.box[0]), innerHeight / (man.box[3] - man.box[1])) * 0.8;
  dirty = true;
}
addEventListener("resize", () => man && resize());

function drawLevel(L, need) {
  const T = man.tile_m * 2 ** L, W = innerWidth, H = innerHeight;
  const x0 = view.x - W / 2 / view.s, y0 = view.y - H / 2 / view.s;
  const x1 = view.x + W / 2 / view.s, y1 = view.y + H / 2 / view.s;
  let missing = false;
  for (let j = Math.floor(y0 / T); j <= Math.floor(y1 / T); j++) {
    for (let i = Math.floor(x0 / T); i <= Math.floor(x1 / T); i++) {
      if (!exists(L, i, j)) continue;
      const t = tile(L, i, j);
      if (!t.ready) { missing = true; continue; }
      // snap to device pixels to avoid hairline gaps between tiles
      const sx = Math.round(((i * T - view.x) * view.s + W / 2) * dpr);
      const sy = Math.round(((j * T - view.y) * view.s + H / 2) * dpr);
      const ex = Math.round((((i + 1) * T - view.x) * view.s + W / 2) * dpr);
      const ey = Math.round((((j + 1) * T - view.y) * view.s + H / 2) * dpr);
      ctx.drawImage(t.img, sx, sy, ex - sx, ey - sy);
    }
  }
  return missing;
}

function draw() {
  ctx.clearRect(0, 0, cv.width, cv.height);
  const artPx = view.s * dpr * (man.tile_m / man.tile_px);          // device px per art px at level 0
  const L = Math.max(0, Math.min(man.levels - 1, Math.floor(Math.log2(1 / artPx))));
  ctx.imageSmoothingEnabled = artPx < 1.5;
  ctx.imageSmoothingQuality = "high";
  // coarser levels underneath while the finer ones load
  const missing = drawLevel(L, true);
  ctx.save(); ctx.globalCompositeOperation = "destination-over";   // everything below goes underneath
  if (missing) for (let k = L + 1; k < man.levels; k++) drawLevel(k, false);
  ctx.fillStyle = "#1f2428"; ctx.fillRect(0, 0, cv.width, cv.height);
  ctx.restore();
  drawLabels();
  drawMini();
}

function toScreen(x, y) { return [(x - view.x) * view.s + innerWidth / 2, (y - view.y) * view.s + innerHeight / 2]; }

function drawLabels() {
  lctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  lctx.clearRect(0, 0, innerWidth, innerHeight);
  const pins = [];
  if (showLabels && view.s > minS * 2.2) for (const p of places) pins.push({ ...p, kind: "place" });
  if (marker) pins.push({ ...marker, kind: "marker" });
  lctx.font = "600 12px Geist, system-ui, sans-serif";
  lctx.textBaseline = "middle";
  for (const p of pins) {
    const [x, y] = toScreen(p.sx, p.sy);
    if (x < -100 || y < -20 || x > innerWidth + 100 || y > innerHeight + 20) continue;
    const r = p.kind === "marker" ? 7 : 5;
    lctx.fillStyle = p.kind === "marker" ? "#f2b544" : "#e8577a";
    lctx.strokeStyle = "#fff"; lctx.lineWidth = 2;
    lctx.beginPath(); lctx.arc(x, y, r, 0, 7); lctx.fill(); lctx.stroke();
    const w = lctx.measureText(p.name).width + 14;
    lctx.fillStyle = "rgb(20 24 28 / .82)";
    roundRect(lctx, x + r + 4, y - 11, w, 22, 7); lctx.fill();
    lctx.fillStyle = "#f4efe6";
    lctx.fillText(p.name, x + r + 11, y + 0.5);
  }
}
function roundRect(c, x, y, w, h, r) {
  c.beginPath(); c.moveTo(x + r, y); c.arcTo(x + w, y, x + w, y + h, r); c.arcTo(x + w, y + h, x, y + h, r);
  c.arcTo(x, y + h, x, y, r); c.arcTo(x, y, x + w, y, r); c.closePath();
}

// ---------------------------------------------------------------- animation
let anim = null, vel = null;
function frame(t) {
  if (anim) {
    const k = Math.min(1, (t - anim.t0) / anim.dur), e = k < 0.5 ? 4 * k * k * k : 1 - (-2 * k + 2) ** 3 / 2;
    // zoom out-and-in flight in log-scale
    const ls = Math.log(anim.s0) + (Math.log(anim.s1) - Math.log(anim.s0)) * e - anim.hop * Math.sin(Math.PI * e);
    view.x = anim.x0 + (anim.x1 - anim.x0) * e; view.y = anim.y0 + (anim.y1 - anim.y0) * e; view.s = Math.exp(ls);
    if (k >= 1) { anim = null; writeHash(); }
    dirty = true;
  } else if (vel && !dragging) {
    view.x -= vel.x / view.s; view.y -= vel.y / view.s;
    vel.x *= 0.92; vel.y *= 0.92;
    if (Math.hypot(vel.x, vel.y) < 0.2) { vel = null; writeHash(); }
    clamp(); dirty = true;
  }
  if (dirty) { dirty = false; draw(); }
  requestAnimationFrame(frame);
}
function flyTo(x, y, s) {
  s = Math.max(minS, Math.min(maxS, s));
  const d = Math.hypot(x - view.x, y - view.y) * Math.min(view.s, s);
  const hop = Math.min(2.2, Math.max(0, Math.log(d / innerWidth + 1)));
  anim = { t0: performance.now(), dur: 900 + 300 * hop, x0: view.x, y0: view.y, s0: view.s, x1: x, y1: y, s1: s, hop };
}
function clamp() {
  view.s = Math.max(minS, Math.min(maxS, view.s));
  const [x0, y0, x1, y1] = man.box;
  view.x = Math.max(x0, Math.min(x1, view.x));
  view.y = Math.max(y0, Math.min(y1, view.y));
}
function home() {
  const [x0, y0, x1, y1] = man.box;
  view.x = (x0 + x1) / 2; view.y = (y0 + y1) / 2; view.s = minS * 1.25; clamp(); dirty = true;
}
function zoomAt(f, px = innerWidth / 2, py = innerHeight / 2) {
  const wx = view.x + (px - innerWidth / 2) / view.s, wy = view.y + (py - innerHeight / 2) / view.s;
  view.s = Math.max(minS, Math.min(maxS, view.s * f));
  view.x = wx - (px - innerWidth / 2) / view.s; view.y = wy - (py - innerHeight / 2) / view.s;
  clamp(); dirty = true;
}

// ---------------------------------------------------------------- input
let dragging = false;
const pointers = new Map();
let pinch = null, last = null;
cv.addEventListener("pointerdown", (e) => {
  cv.setPointerCapture(e.pointerId); pointers.set(e.pointerId, [e.clientX, e.clientY]);
  anim = null; vel = null; dragging = true; cv.classList.add("drag");
  last = { x: e.clientX, y: e.clientY, t: performance.now() };
  if (pointers.size === 2) {
    const [a, b] = [...pointers.values()];
    pinch = { d: Math.hypot(a[0] - b[0], a[1] - b[1]), s: view.s };
  }
});
cv.addEventListener("pointermove", (e) => {
  if (!pointers.has(e.pointerId)) return;
  const prev = pointers.get(e.pointerId);
  pointers.set(e.pointerId, [e.clientX, e.clientY]);
  if (pointers.size === 2 && pinch) {
    const [a, b] = [...pointers.values()];
    const d = Math.hypot(a[0] - b[0], a[1] - b[1]);
    zoomAt((pinch.s * d / pinch.d) / view.s, (a[0] + b[0]) / 2, (a[1] + b[1]) / 2);
    return;
  }
  const dx = e.clientX - prev[0], dy = e.clientY - prev[1];
  view.x -= dx / view.s; view.y -= dy / view.s; clamp(); dirty = true;
  const t = performance.now();
  vel = { x: dx * 16 / Math.max(8, t - last.t), y: dy * 16 / Math.max(8, t - last.t) };
  last = { x: e.clientX, y: e.clientY, t };
});
const up = (e) => {
  pointers.delete(e.pointerId);
  if (pointers.size < 2) pinch = null;
  if (pointers.size === 0) {
    dragging = false; cv.classList.remove("drag");
    if (performance.now() - last.t > 80) vel = null;
    if (!vel) writeHash();
  }
};
cv.addEventListener("pointerup", up); cv.addEventListener("pointercancel", up);
cv.addEventListener("wheel", (e) => {
  e.preventDefault(); anim = null;
  const d = e.deltaMode === 1 ? e.deltaY * 16 : e.deltaY;
  zoomAt(Math.exp(-d * 0.0015), e.clientX, e.clientY);
  clearTimeout(cv._wt); cv._wt = setTimeout(writeHash, 250);
}, { passive: false });
cv.addEventListener("dblclick", (e) => { const wx = view.x + (e.clientX - innerWidth / 2) / view.s, wy = view.y + (e.clientY - innerHeight / 2) / view.s; flyTo(wx, wy, view.s * 2); });

addEventListener("keydown", (e) => {
  if (e.target.tagName === "INPUT") { if (e.key === "Escape") e.target.blur(); return; }
  const step = 120 / view.s;
  const k = e.key;
  if (k === "+" || k === "=") zoomAt(1.5);
  else if (k === "-") zoomAt(1 / 1.5);
  else if (k === "0") flyTo((man.box[0] + man.box[2]) / 2, (man.box[1] + man.box[3]) / 2, minS * 1.25);
  else if (k === "ArrowLeft") { view.x -= step; dirty = true; }
  else if (k === "ArrowRight") { view.x += step; dirty = true; }
  else if (k === "ArrowUp") { view.y -= step; dirty = true; }
  else if (k === "ArrowDown") { view.y += step; dirty = true; }
  else if (k === "/") { e.preventDefault(); $("q").focus(); }
  else if (k === "d" || k === "D") toggleTilt();
  else if (k === "l" || k === "L") toggleLabels();
  else return;
  clamp();
});

// ---------------------------------------------------------------- hash (#x,y,zoom)
function writeHash() {
  history.replaceState(null, "", `#${view.x.toFixed(0)},${view.y.toFixed(0)},${view.s.toFixed(3)}`);
}
function readHash() {
  const m = location.hash.slice(1).split(",").map(Number);
  if (m.length < 3 || m.some(isNaN)) return false;
  [view.x, view.y, view.s] = m; clamp(); dirty = true; return true;
}

// ---------------------------------------------------------------- buttons
$("zin").onclick = () => { zoomAt(1.6); writeHash(); };
$("zout").onclick = () => { zoomAt(1 / 1.6); writeHash(); };
$("home").onclick = () => flyTo((man.box[0] + man.box[2]) / 2, (man.box[1] + man.box[3]) / 2, minS * 1.25);
function toggleTilt() {
  const b = $("tiltBtn"), on = b.getAttribute("aria-pressed") !== "true";
  b.setAttribute("aria-pressed", on); $("tilt").hidden = !on;
}
function toggleLabels() {
  const b = $("labelsBtn"); showLabels = b.getAttribute("aria-pressed") !== "true";
  b.setAttribute("aria-pressed", showLabels); dirty = true;
}
$("tiltBtn").onclick = toggleTilt;
$("labelsBtn").onclick = toggleLabels;
$("share").onclick = async () => {
  writeHash();
  try { if (navigator.share && matchMedia("(pointer: coarse)").matches) { await navigator.share({ title: document.title, url: location.href }); return; } } catch { return; }
  try { await navigator.clipboard.writeText(location.href); toast("Lien copié"); } catch { prompt("Lien", location.href); }
};
$("info").onclick = () => { $("about").hidden = !$("about").hidden; };
$("about").querySelector(".close").onclick = () => { $("about").hidden = true; };
function toast(msg) { const t = $("toast"); t.textContent = msg; t.hidden = false; clearTimeout(t._t); t._t = setTimeout(() => (t.hidden = true), 1600); }

// ---------------------------------------------------------------- minimap
const mini = { cv: $("mini").querySelector("canvas"), img: new Image() };
function initMini() {
  mini.img.onload = () => {
    const w = 180, h = Math.round(180 * mini.img.height / mini.img.width);
    mini.cv.width = w * dpr; mini.cv.height = h * dpr; mini.cv.style.width = w + "px"; mini.cv.style.height = h + "px";
    mini.k = w / (man.box[2] - man.box[0]); dirty = true;
  };
  mini.img.src = DATA + "overview.webp";
  const jump = (e) => {
    const r = mini.cv.getBoundingClientRect();
    view.x = man.box[0] + (e.clientX - r.left) / mini.k; view.y = man.box[1] + (e.clientY - r.top) / mini.k;
    anim = null; vel = null; clamp(); dirty = true;
  };
  mini.cv.addEventListener("pointerdown", (e) => { mini.cv.setPointerCapture(e.pointerId); jump(e); });
  mini.cv.addEventListener("pointermove", (e) => { if (mini.cv.hasPointerCapture(e.pointerId)) jump(e); });
  mini.cv.addEventListener("pointerup", writeHash);
}
function drawMini() {
  if (!mini.k) return;
  const c = mini.cv.getContext("2d"), k = mini.k * dpr;
  c.drawImage(mini.img, 0, 0, mini.cv.width, mini.cv.height);
  const w = innerWidth / view.s * k, h = innerHeight / view.s * k;
  const x = (view.x - man.box[0]) * k - w / 2, y = (view.y - man.box[1]) * k - h / 2;
  c.strokeStyle = "#f2b544"; c.lineWidth = 2 * dpr; c.strokeRect(x, y, w, h);
}

// ---------------------------------------------------------------- search (places + BAN addresses)
const q = $("q"), results = $("results");
let sel = -1, items = [], seq = 0;
function l93ToScreen(x, y, z = 10) {
  const [r, d] = man.axes, ox = x - man.origin_l93[0], oy = y - man.origin_l93[1];
  return [ox * r[0] + oy * r[1] + z * r[2], ox * d[0] + oy * d[1] + z * d[2]];
}
function inMap(sx, sy) { const [x0, y0, x1, y1] = man.box; return sx >= x0 && sx <= x1 && sy >= y0 && sy <= y1; }
const norm = (s) => s.normalize("NFD").replace(/[̀-ͯ]/g, "").toLowerCase();
q.addEventListener("input", async () => {
  const text = q.value.trim(), my = ++seq;
  if (text.length < 2) { results.hidden = true; return; }
  const local = places.filter((p) => norm(p.name).includes(norm(text))).slice(0, 4)
    .map((p) => ({ name: p.name, sub: "Lieu", sx: p.sx, sy: p.sy }));
  render(local);
  try {
    const r = await fetch(`https://api-adresse.data.gouv.fr/search/?q=${encodeURIComponent(text)}&lat=47.2135&lon=-1.5555&limit=6`);
    const js = await r.json();
    if (my !== seq) return;
    const addr = js.features.map((f) => {
      const [sx, sy] = l93ToScreen(f.properties.x, f.properties.y);
      return { name: f.properties.name, sub: f.properties.city, sx, sy };
    }).filter((a) => inMap(a.sx, a.sy));
    render([...local, ...addr].slice(0, 7));
  } catch { /* offline: places only */ }
});
function render(list) {
  items = list; sel = list.length ? 0 : -1;
  results.innerHTML = "";
  if (!list.length) {
    const li = document.createElement("li"); li.textContent = "Rien sur la carte pour l'instant"; li.style.color = "var(--muted)";
    results.append(li); results.hidden = false; return;
  }
  list.forEach((it, k) => {
    const li = document.createElement("li");
    li.textContent = it.name;
    const s = document.createElement("small"); s.textContent = it.sub || ""; li.append(s);
    li.setAttribute("aria-selected", k === sel);
    li.onpointerdown = (e) => { e.preventDefault(); go(it); };
    results.append(li);
  });
  results.hidden = false;
}
function go(it) {
  marker = { sx: it.sx, sy: it.sy, name: it.name };
  flyTo(it.sx, it.sy, Math.max(view.s, 1.2));
  results.hidden = true; q.blur();
}
q.addEventListener("keydown", (e) => {
  if (e.key === "ArrowDown" || e.key === "ArrowUp") {
    e.preventDefault(); if (!items.length) return;
    sel = (sel + (e.key === "ArrowDown" ? 1 : -1) + items.length) % items.length;
    [...results.children].forEach((li, k) => li.setAttribute("aria-selected", k === sel));
  }
});
$("search").addEventListener("submit", (e) => { e.preventDefault(); if (items[sel]) go(items[sel]); });
q.addEventListener("blur", () => setTimeout(() => (results.hidden = true), 150));

init().catch((e) => { console.error(e); toast("Impossible de charger la carte"); });
addEventListener("hashchange", () => { if (man) { anim = null; vel = null; readHash(); } });
