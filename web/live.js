// Live layer: trams, buses and Navibus on their real timetable (realtime-corrected), cars matching the measured
// road traffic, and today's events. Everything comes from our own server (/api/...), which caches the city's
// feeds; static geometry comes from data/world/ (built by tinynantes/world.py).
//
// World coordinates are Lambert-93 metres relative to the map origin, with terrain height z; they are
// projected with the heading's axes exactly like the tiles (see tinynantes/geo.py).

const TN = window.TN;                       // view state shared by app.js
const cv = document.getElementById("live"), ctx = cv.getContext("2d");
const COS45 = Math.SQRT1_2;

const world = { ready: false, shapes: {}, routes: {}, terrain: null };
let vehicles = [], clockSkew = 0, rtAge = null;
let sections = [], cars = [];
let events = [];
const hgt = new Map();                       // "i_j" -> { data, ready } height tiles (0.5 m steps)
let hover = null;

// ---------------------------------------------------------------- loading
async function getJSON(u) { const r = await fetch(u); if (!r.ok) throw new Error(`${u}: ${r.status}`); return r.json(); }

async function loadWorld() {
  const [meta, tr] = await Promise.all([getJSON("data/world/world.json"), getJSON("data/world/transit.json")]);
  world.meta = meta;
  for (const r of tr.routes) world.routes[r.id] = r;
  for (const [id, s] of Object.entries(tr.shapes)) {
    const p = s.p, cum = [0];
    for (let k = 1; k < p.length; k++) cum.push(cum[k - 1] + Math.hypot(p[k][0] - p[k - 1][0], p[k][1] - p[k - 1][1]));
    world.shapes[id] = { route: s.r, p, cum, stops: s.s };
  }
  world.terrain = await loadTerrain(meta);
  world.ready = true;
}

function loadTerrain(meta) {
  return new Promise((ok) => {
    const im = new Image();
    im.onload = () => {
      const c = document.createElement("canvas"); c.width = im.width; c.height = im.height;
      const g = c.getContext("2d", { willReadFrequently: true }); g.drawImage(im, 0, 0);
      const d = g.getImageData(0, 0, im.width, im.height).data;
      const z = new Float32Array(im.width * im.height);
      for (let k = 0; k < z.length; k++) z[k] = (d[4 * k] * 256 + d[4 * k + 1]) / 10 - 100;
      ok({ z, w: im.width, h: im.height, x0: meta.x0 - meta.origin[0], y1: meta.y1 - meta.origin[1], res: meta.res });
    };
    im.onerror = () => ok(null);
    im.src = "data/world/terrain.png";
  });
}
function zAt(x, y) {
  const t = world.terrain; if (!t) return 10;
  const c = Math.min(t.w - 1, Math.max(0, Math.floor((x - t.x0) / t.res)));
  const r = Math.min(t.h - 1, Math.max(0, Math.floor((t.y1 - y) / t.res)));
  return t.z[r * t.w + c];
}

async function pollVehicles() {
  try {
    const d = await getJSON("api/transit/vehicles");
    clockSkew = d.now - Date.now() / 1000; rtAge = d.rt_age;
    vehicles = d.vehicles.filter((v) => world.shapes[v.shape]);
  } catch (e) { /* keep animating the last snapshot */ }
}
async function pollTraffic() {
  try {
    const d = await getJSON("api/traffic");
    sections = d.items;
    spawnCars();
  } catch (e) { /* keep the current cars */ }
}
async function pollEvents() {
  try { events = (await getJSON("api/events")).items.filter((e) => !e.canceled); } catch (e) { /* none */ }
}

// ---------------------------------------------------------------- geometry
function project(x, y, z) {
  const [r, d] = TN.man.axes;
  return [x * r[0] + y * r[1] + z * r[2], x * d[0] + y * d[1] + z * d[2]];
}
function toCanvas(sx, sy) {
  const v = TN.view;
  return [((sx - v.x) * v.s + innerWidth / 2) * TN.dpr, ((sy - v.y) * v.s + innerHeight / 2) * TN.dpr];
}
function along(shape, d) {
  const { p, cum } = shape;
  d = Math.max(0, Math.min(cum[cum.length - 1], d));
  let lo = 0, hi = cum.length - 1;
  while (hi - lo > 1) { const m = (lo + hi) >> 1; if (cum[m] <= d) lo = m; else hi = m; }
  const L = cum[hi] - cum[lo] || 1, f = (d - cum[lo]) / L;
  const a = p[lo], b = p[hi];
  return { x: a[0] + (b[0] - a[0]) * f, y: a[1] + (b[1] - a[1]) * f, z: a[2] + (b[2] - a[2]) * f,
           ang: Math.atan2(b[1] - a[1], b[0] - a[0]) };
}
const ease = (f) => f * f * (3 - 2 * f);     // accelerate out of a stop, brake into the next

function vehiclePose(v, t) {
  const sh = world.shapes[v.shape], n = v.arr.length;
  let d;
  if (t <= v.dep[0]) d = sh.stops[v.k0];
  else {
    let i = 0;
    while (i < n - 1 && v.arr[i + 1] <= t) i++;
    const s = v.k0 + i;
    if (i >= n - 1) d = sh.stops[sh.stops.length - 1];
    else if (t <= v.dep[i]) d = sh.stops[s];
    else {
      const f = (t - v.dep[i]) / Math.max(1, v.arr[i + 1] - v.dep[i]);
      d = sh.stops[s] + (sh.stops[s + 1] - sh.stops[s]) * ease(Math.min(1, f));
    }
  }
  return along(sh, d);
}

// ---------------------------------------------------------------- cars from measured traffic
const CAR_COLORS = ["#d8d4cc", "#2b2f36", "#9aa3ad", "#b8322f", "#2f5b8f", "#e8e2d4", "#4b5a3c", "#c7a23a"];
function spawnCars() {
  cars = [];
  for (const s of sections) {
    const p = s.p.map(([x, y]) => [x, y]), cum = [0];
    for (let k = 1; k < p.length; k++) cum.push(cum[k - 1] + Math.hypot(p[k][0] - p[k - 1][0], p[k][1] - p[k - 1][1]));
    const len = cum[cum.length - 1];
    const v = Math.max(3, (s.speed || 30)) / 3.6;                 // m/s
    const perKm = s.flow && s.speed ? s.flow / Math.max(5, s.speed) : 6;   // flow / speed = density
    const n = Math.min(60, Math.round(len / 1000 * perKm * (0.7 + Math.random() * 0.6)));
    const shape = { p: p.map(([x, y]) => [x, y, zAt(x, y)]), cum };
    for (let k = 0; k < n; k++) {
      cars.push({ shape, len, v: v * (0.85 + Math.random() * 0.3), off: Math.random() * len,
                  color: CAR_COLORS[(Math.random() * CAR_COLORS.length) | 0], jam: s.state });
    }
  }
}

// ---------------------------------------------------------------- occlusion (height tiles)
function heightTile(i, j) {
  const k = `${TN.H}/${i}_${j}`;
  let t = hgt.get(k);
  if (!t) {
    t = { ready: false };
    hgt.set(k, t);
    const im = new Image();
    im.onload = () => {
      const c = document.createElement("canvas"); c.width = im.width; c.height = im.height;
      const g = c.getContext("2d", { willReadFrequently: true }); g.drawImage(im, 0, 0);
      t.data = g.getImageData(0, 0, im.width, im.height).data; t.w = im.width; t.ready = true;
    };
    im.onerror = () => { t.missing = true; };
    im.src = `data/h${TN.H}/hgt/${i}_${j}.png`;
    if (hgt.size > 64) hgt.delete(hgt.keys().next().value);
  }
  return t;
}
// Is screen point (sx, sy) covered by something standing in front of a sprite whose foot is at footY?
function occluded(sx, sy, footY) {
  const T = TN.man.tile_m, i = Math.floor(sx / T), j = Math.floor(sy / T);
  const t = heightTile(i, j);
  if (!t.ready) return false;
  const px = Math.floor((sx - i * T) / T * t.w), py = Math.floor((sy - j * T) / T * t.w);
  const h = t.data[4 * (py * t.w + px)] * 0.5;
  return h > 0 && sy + h * COS45 > footY + 1.0;
}

// ---------------------------------------------------------------- drawing
function box(x, y, z, ang, L, W, H, top, side, out) {
  const c = Math.cos(ang), s = Math.sin(ang), hl = L / 2, hw = W / 2;
  const base = [[hl, hw], [hl, -hw], [-hl, -hw], [-hl, hw]].map(([u, v]) => [x + u * c - v * s, y + u * s + v * c]);
  const P = (q, zz) => toCanvas(...project(q[0], q[1], zz));
  const lo = base.map((q) => P(q, z)), hi = base.map((q) => P(q, z + H));
  const back = TN.man.axes[2];
  for (let k = 0; k < 4; k++) {
    const a = base[k], b = base[(k + 1) % 4];
    const nx = a[1] - b[1], ny = b[0] - a[0];                      // outward normal (the ring is clockwise)
    if (nx * back[0] + ny * back[1] <= 0) continue;
    const shade = 0.62 + 0.25 * Math.abs(nx * 0.6 + ny * 0.8) / Math.hypot(nx, ny);
    ctx.fillStyle = shadeColor(side, shade);
    ctx.beginPath(); ctx.moveTo(...lo[k]); ctx.lineTo(...lo[(k + 1) % 4]); ctx.lineTo(...hi[(k + 1) % 4]); ctx.lineTo(...hi[k]);
    ctx.closePath(); ctx.fill();
  }
  ctx.fillStyle = top;
  ctx.beginPath(); hi.forEach((q, k) => (k ? ctx.lineTo(...q) : ctx.moveTo(...q))); ctx.closePath(); ctx.fill();
  if (out) { ctx.strokeStyle = out; ctx.lineWidth = Math.max(1, TN.dpr); ctx.stroke(); }
}
function shadeColor(hex, k) {
  const n = parseInt(hex.slice(1), 16);
  const r = Math.round(((n >> 16) & 255) * k), g = Math.round(((n >> 8) & 255) * k), b = Math.round((n & 255) * k);
  return `rgb(${r},${g},${b})`;
}

const KIND = { 0: { L: 32, W: 2.65, H: 3.3 }, 3: { L: 12, W: 2.55, H: 3.0 }, 4: { L: 18, W: 5, H: 2.2 } };

function visibleRect() {
  const v = TN.view, m = 40 / v.s;
  return [v.x - innerWidth / 2 / v.s - m, v.y - innerHeight / 2 / v.s - m, v.x + innerWidth / 2 / v.s + m, v.y + innerHeight / 2 / v.s + m];
}

function frame() {
  requestAnimationFrame(frame);
  if (!world.ready || !TN.man) return;
  const w = Math.round(innerWidth * TN.dpr), h = Math.round(innerHeight * TN.dpr);
  if (cv.width !== w || cv.height !== h) { cv.width = w; cv.height = h; }
  ctx.clearRect(0, 0, w, h);
  if (!document.getElementById("liveBtn") || document.getElementById("liveBtn").getAttribute("aria-pressed") !== "true") return;
  const s = TN.view.s;
  if (s < 0.6) return;                                           // too far out: vehicles would be specks
  const t = Date.now() / 1000 + clockSkew;
  const [x0, y0, x1, y1] = visibleRect();
  const items = [];
  for (const v of vehicles) {
    const r = world.routes[v.route] || { type: 3, color: "#888" };
    const p = vehiclePose(v, t);
    const [sx, sy] = project(p.x, p.y, p.z);
    if (sx < x0 || sx > x1 || sy < y0 || sy > y1) continue;
    items.push({ sx, sy, p, kind: KIND[r.type] || KIND[3], color: r.color, v, r });
  }
  if (s > 1.0) {
    for (const c of cars) {
      const d = (c.off + c.v * t) % c.len;
      const p = along(c.shape, d);
      const nx = -Math.sin(p.ang), ny = Math.cos(p.ang);           // drive on the right of the section line
      p.x -= nx * 1.6; p.y -= ny * 1.6;
      const [sx, sy] = project(p.x, p.y, p.z);
      if (sx < x0 || sx > x1 || sy < y0 || sy > y1) continue;
      items.push({ sx, sy, p, kind: { L: 4.3, W: 1.8, H: 1.45 }, color: c.color, car: true });
    }
  }
  items.sort((a, b) => a.sy - b.sy);                              // painter's order: far (high on screen) first
  TN.liveStats = { vehicles: vehicles.length, cars: cars.length, inView: items.length };
  hover = null;
  for (const it of items) {
    if (occluded(it.sx, it.sy - it.kind.H * COS45, it.sy)) continue;
    const { p, kind } = it;
    box(p.x, p.y, p.z + 0.3, p.ang, kind.L, kind.W, kind.H, it.car ? shadeColor(it.color, 1.08) : "#f2f2ee", it.color,
        s > 2 ? "rgba(20,24,28,.55)" : null);
    if (it.v && TN.pointer) {
      const [cx, cy] = toCanvas(it.sx, it.sy);
      if (Math.hypot(cx - TN.pointer[0] * TN.dpr, cy - TN.pointer[1] * TN.dpr) < 18 * TN.dpr) hover = it;
    }
  }
  drawEvents();
  if (hover) tooltip(hover);
}

function drawEvents() {
  if (TN.view.s < 0.35) return;
  const [x0, y0, x1, y1] = visibleRect();
  ctx.font = `${12 * TN.dpr}px Geist, system-ui, sans-serif`;
  for (const e of events) {
    const [sx, sy] = project(e.p[0], e.p[1], zAt(e.p[0], e.p[1]));
    if (sx < x0 || sx > x1 || sy < y0 || sy > y1) continue;
    const [cx, cy] = toCanvas(sx, sy);
    const pulse = 1 + 0.25 * Math.sin(Date.now() / 300 + e.p[0]);
    ctx.fillStyle = "rgba(242,181,68,.9)"; ctx.strokeStyle = "#fff"; ctx.lineWidth = 2 * TN.dpr;
    ctx.beginPath(); ctx.arc(cx, cy - 14 * TN.dpr, 6 * TN.dpr * pulse, 0, 7); ctx.fill(); ctx.stroke();
    ctx.beginPath(); ctx.moveTo(cx, cy); ctx.lineTo(cx, cy - 8 * TN.dpr); ctx.stroke();
    if (TN.pointer && Math.hypot(cx - TN.pointer[0] * TN.dpr, cy - 14 * TN.dpr - TN.pointer[1] * TN.dpr) < 12 * TN.dpr) {
      label(cx, cy - 30 * TN.dpr, [e.name, `${e.start || ""}${e.end ? "–" + e.end : ""} · ${e.place}`, e.free ? "Gratuit" : ""]);
    }
  }
}

function tooltip(it) {
  const [cx, cy] = toCanvas(it.sx, it.sy);
  const late = it.v.delay == null ? "horaire théorique" : Math.abs(it.v.delay) < 60 ? "à l'heure"
    : it.v.delay > 0 ? `+${Math.round(it.v.delay / 60)} min` : `${Math.round(it.v.delay / 60)} min (en avance)`;
  const kind = { 0: "Tram", 3: "Bus", 4: "Navibus" }[it.r.type] || "Ligne";
  label(cx, cy - 40 * TN.dpr, [`${kind} ${it.r.short} → ${it.v.headsign}`, late]);
}
function label(x, y, lines) {
  lines = lines.filter(Boolean);
  const pad = 8 * TN.dpr, lh = 16 * TN.dpr;
  const w = Math.max(...lines.map((l) => ctx.measureText(l).width)) + 2 * pad;
  ctx.fillStyle = "rgba(20,24,28,.88)";
  ctx.beginPath(); ctx.roundRect(x - w / 2, y - lines.length * lh - pad, w, lines.length * lh + pad, 7 * TN.dpr); ctx.fill();
  ctx.fillStyle = "#f4efe6"; ctx.textAlign = "center"; ctx.textBaseline = "top";
  lines.forEach((l, k) => ctx.fillText(l, x, y - lines.length * lh - pad / 2 + k * lh));
  ctx.textAlign = "start";
}

// ---------------------------------------------------------------- start
addEventListener("pointermove", (e) => { TN.pointer = [e.clientX, e.clientY]; });
loadWorld().then(() => {
  pollVehicles(); setInterval(pollVehicles, 20_000);
  pollTraffic(); setInterval(pollTraffic, 180_000);
  pollEvents(); setInterval(pollEvents, 600_000);
  requestAnimationFrame(frame);
}).catch((e) => console.warn("live layer off:", e));
