// Live layer: trams, buses and Navibus on their real timetable (realtime-corrected), cars matching the measured
// road traffic, trains around the station, aircraft over Nantes, today's events and the places on the map.
// Everything comes from our own server (/api/...), which caches the city's feeds; static geometry comes from
// data/world/ (built by tinynantes/world.py).
//
// World coordinates: Lambert-93 metres relative to the map origin, with height z; projected with the
// heading's axes exactly like the tiles (see tinynantes/geo.py). All moving things are drawn in one
// painter's-order pass, hidden behind buildings with the per-tile height maps.

const TN = window.TN;
const cv = document.getElementById("live"), ctx = cv.getContext("2d");
const COS45 = Math.SQRT1_2;

const world = { ready: false, shapes: {}, routes: {}, terrain: null, rails: null, pois: [] };
let vehicles = [], clockSkew = 0;
let sections = [];
let events = [], planes = [], planesAt = 0, trains = [];
let menus = new Set();
const hgt = new Map();
let hits = [];                                // clickable things drawn this frame: {x, y, r, open}

export const live = {
  world, project, toCanvas, zAt, along, vehiclePose, occluded, box, label, shade, sprite, get SPRITE_CARS() { return SPRITE_CARS; },
  get ctx() { return ctx; }, room: null,
  get vehicles() { return vehicles; }, get menus() { return menus; }, get sections() { return sections; },
  lights: [],                                 // light sources drawn this frame (canvas px), read by fx.js at night
  now: () => Date.now() / 1000 + clockSkew,
  layers: [],                                 // functions (t) -> items, added by game.js
  under: [],                                  // functions drawn before the items (game.js: the opened building)
  open: null,                                 // set by game.js: open(kind, data) shows the side panel
};
window.TNLive = live;

// ---------------------------------------------------------------- loading
async function getJSON(u) { const r = await fetch(u); if (!r.ok) throw new Error(`${u}: ${r.status}`); return r.json(); }

async function loadWorld() {
  const [meta, tr] = await Promise.all([getJSON("data/world/world.json"), getJSON("data/world/transit.json")]);
  world.meta = meta;
  for (const r of tr.routes) world.routes[r.id] = r;
  for (const [id, s] of Object.entries(tr.shapes)) world.shapes[id] = withCum({ route: s.r, p: s.p, stops: s.s });
  world.stations = tr.stations;
  world.terrain = await loadTerrain(meta);
  try { world.rails = await getJSON("data/world/rails.json"); world.rails.routes.forEach(withCum); } catch { /* optional */ }
  try { world.pois = await getJSON("data/world/pois.json"); } catch { /* optional */ }
  try { world.airport = (await getJSON("api/airport")).p; } catch { /* optional */ }
  world.ready = true;
}
function withCum(s) {
  const p = s.p, cum = [0];
  for (let k = 1; k < p.length; k++) cum.push(cum[k - 1] + Math.hypot(p[k][0] - p[k - 1][0], p[k][1] - p[k - 1][1]));
  s.cum = cum; s.len = cum[cum.length - 1];
  return s;
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
    im.src = "data/world/terrain.png" + TN.v;
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
    clockSkew = d.now - Date.now() / 1000;
    vehicles = d.vehicles.filter((v) => world.shapes[v.shape]);
  } catch { /* keep animating the last snapshot */ }
}
async function pollTraffic() {
  try { sections = (await getJSON("api/traffic")).items; } catch { /* keep */ }
}
async function pollEvents() {
  try { events = (await getJSON("api/events")).items.filter((e) => !e.canceled); } catch { /* none */ }
}
async function pollPlanes() {
  try { planes = (await getJSON("api/planes")).items; planesAt = Date.now() / 1000; } catch { /* keep */ }
}
async function pollMenus() {
  try { menus = new Set(await getJSON("api/places/menus")); } catch { /* none */ }
}
async function pollTrains() {
  try { trains = (await getJSON("api/trains")).items; } catch { /* keep */ }
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
const ease = (f) => f * f * (3 - 2 * f);
// A rigid vehicle on its route: two bogies (axles) on the line, `front` and `front - L` along it; the body sits
// between them along the chord, so it turns smoothly through bends and trailing cars follow the head's path.
function chord(shape, front, L) {
  const a = along(shape, front), b = along(shape, front - L);
  const dx = a.x - b.x, dy = a.y - b.y;
  const ang = Math.hypot(dx, dy) > 0.5 ? Math.atan2(dy, dx) : a.ang;
  return { x: (a.x + b.x) / 2, y: (a.y + b.y) / 2, z: (a.z + b.z) / 2, ang };
}

// Where a transit vehicle is at time t; `stop` is the index (in v.names) of the stop it stands at, if any.
// Positions come from timetables + realtime delays; when a refresh changes a delay, the computed position jumps.
// Keep the drawn distance along the shape continuous: the gap is absorbed over ~1.5 s (teleports beyond 400 m).
const poseSmooth = new Map();          // vehicle id -> {shape, off, t}
function vehiclePose(v, t) {
  const p = rawPose(v, t);
  const k = v.id ?? v.trip;
  let m = poseSmooth.get(k);
  if (!m || m.shape !== v.shape) { m = { shape: v.shape, d: p.d, off: 0, t, target: p.d }; poseSmooth.set(k, m); }
  if (t !== m.t) {
    const dt = t - m.t;
    if (dt > 0) {
      const shown = m.target + m.off;                 // where it was drawn
      m.off = shown - p.d;                             // keep it there...
      if (Math.abs(m.off) > 400) m.off = 0;
      m.off *= Math.exp(-dt / 1.5);                    // ...and let it join the computed position
    }
    m.t = t; m.target = p.d;
  }
  if (Math.abs(m.off) < 0.05) return p;
  const q = along(world.shapes[v.shape], p.d + m.off);
  q.stop = p.stop; q.d = p.d + m.off;
  return q;
}
function rawPose(v, t) {
  const sh = world.shapes[v.shape], n = v.arr.length;
  let d, stop = -1;
  if (t <= v.dep[0]) { d = sh.stops[v.k0]; stop = 0; }
  else {
    let i = 0;
    while (i < n - 1 && v.arr[i + 1] <= t) i++;
    const s = v.k0 + i;
    if (i >= n - 1) { d = sh.stops[sh.stops.length - 1]; stop = n - 1; }
    else if (t <= v.dep[i]) { d = sh.stops[s]; stop = i; }
    else {
      const f = (t - v.dep[i]) / Math.max(1, v.arr[i + 1] - v.dep[i]);
      d = sh.stops[s] + (sh.stops[s + 1] - sh.stops[s]) * ease(Math.min(1, f));
    }
  }
  const p = along(sh, d);
  p.stop = stop; p.d = d;
  return p;
}

// ---------------------------------------------------------------- the inhabitants (simulated by the server)
// The server runs the city (cars on the measured traffic, people going to places, bikes) and streams what is in
// our view 4 times a second over /api/world. We only draw it, 300 ms in the past, interpolating between frames.
const CAR_COLORS = ["#d8d4cc", "#2b2f36", "#9aa3ad", "#b8322f", "#2f5b8f", "#e8e2d4", "#4b5a3c", "#c7a23a", "#7a2e4a", "#e6e6e2"];
const CLOTHES = ["#c0392b", "#2c6e9e", "#e1a23a", "#3f7f4f", "#6b4c9a", "#d9d2c5", "#2b2f36", "#c46a8a", "#4aa0a8", "#8a5a3c"];
const snaps = [];                       // [{at, m: Map(id -> [id, kind, x, y, z, a, v, c])}]
let clockOff = null, worldWs = null, viewSent = "";
function connectWorld() {
  const u = new URL("api/world", location.href);
  u.protocol = location.protocol === "https:" ? "wss:" : "ws:";
  worldWs = new WebSocket(u);
  worldWs.onopen = () => { viewSent = ""; sendView(); };
  worldWs.onmessage = (e) => {
    const d = JSON.parse(e.data), off = d.at - Date.now() / 1000;
    clockOff = clockOff == null ? off : Math.min(off, clockOff + 0.02);       // smallest delay seen, slowly forgiving
    snaps.push({ at: d.at, m: new Map(d.a.map((a) => [a[0], a])) });
    while (snaps.length > 4) snaps.shift();
  };
  worldWs.onclose = () => { worldWs = null; setTimeout(connectWorld, 3000); };
}
// the ground rectangle under the screen, in world metres
function viewBox() {
  const [r, d] = TN.man.axes, det = r[0] * d[1] - r[1] * d[0];
  const v = TN.view, hw = innerWidth / 2 / v.s + 60, hh = innerHeight / 2 / v.s + 60;
  const xs = [], ys = [];
  for (const [sx, sy] of [[v.x - hw, v.y - hh], [v.x + hw, v.y - hh], [v.x - hw, v.y + hh], [v.x + hw, v.y + hh]]) {
    const a = sx - r[2] * 10, b = sy - d[2] * 10;
    xs.push((a * d[1] - b * r[1]) / det); ys.push((b * r[0] - a * d[0]) / det);
  }
  return { x0: Math.min(...xs), y0: Math.min(...ys), x1: Math.max(...xs), y1: Math.max(...ys) };
}
function sendView() {
  if (!worldWs || worldWs.readyState !== 1 || !TN.man) return;
  const b = TN.view.s >= 0.9 || TN.playing ? viewBox() : { x0: 0, y0: 0, x1: 1e5, y1: 1e5 };   // zoomed out: nothing
  const k = [b.x0, b.y0, b.x1, b.y1].map((x) => Math.round(x / 25)).join();
  if (k !== viewSent) { viewSent = k; worldWs.send(JSON.stringify(b)); }
}
function inhabitants() {
  if (snaps.length < 2 || clockOff == null) return [];
  const rt = Date.now() / 1000 + clockOff - 0.6;      // two server frames behind: jitter never runs us dry
  let i = snaps.length - 1;
  while (i > 0 && snaps[i - 1].at > rt) i--;
  const s0 = snaps[Math.max(0, i - 1)], s1 = snaps[i];
  const f = s1.at > s0.at ? Math.max(0, Math.min(1.2, (rt - s0.at) / (s1.at - s0.at))) : 1;
  const out = [];
  for (const [id, b] of s1.m) {
    const a = s0.m.get(id);
    if (!a) { out.push(b); continue; }
    let da = b[5] - a[5]; da = Math.atan2(Math.sin(da), Math.cos(da));
    out.push([id, b[1], a[2] + (b[2] - a[2]) * f, a[3] + (b[3] - a[3]) * f, a[4] + (b[4] - a[4]) * f, a[5] + da * f, b[6], b[7]]);
  }
  return out;
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
    im.src = `data/h${TN.H}/hgt/${i}_${j}.png${TN.v}`;
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

// ---------------------------------------------------------------- drawing primitives
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
    ctx.fillStyle = shade(side, 0.62 + 0.25 * Math.abs(nx * 0.6 + ny * 0.8) / Math.hypot(nx, ny));
    ctx.beginPath(); ctx.moveTo(...lo[k]); ctx.lineTo(...lo[(k + 1) % 4]); ctx.lineTo(...hi[(k + 1) % 4]); ctx.lineTo(...hi[k]);
    ctx.closePath(); ctx.fill();
  }
  ctx.fillStyle = top;
  ctx.beginPath(); hi.forEach((q, k) => (k ? ctx.lineTo(...q) : ctx.moveTo(...q))); ctx.closePath(); ctx.fill();
  if (out) { ctx.strokeStyle = out; ctx.lineWidth = Math.max(1, TN.dpr); ctx.stroke(); }
}
function shade(hex, k) {
  if (hex[0] !== "#") return hex;
  const n = parseInt(hex.slice(1), 16);
  const r = Math.min(255, Math.round(((n >> 16) & 255) * k)), g = Math.min(255, Math.round(((n >> 8) & 255) * k)),
        b = Math.min(255, Math.round((n & 255) * k));
  return `rgb(${r},${g},${b})`;
}
function label(x, y, lines, bg = "rgba(20,24,28,.88)") {
  lines = lines.filter(Boolean);
  if (!lines.length) return;
  ctx.font = `${12 * TN.dpr}px Geist, system-ui, sans-serif`;
  const pad = 7 * TN.dpr, lh = 16 * TN.dpr;
  const w = Math.max(...lines.map((l) => ctx.measureText(l).width)) + 2 * pad;
  ctx.fillStyle = bg;
  ctx.beginPath(); ctx.roundRect(x - w / 2, y - lines.length * lh - pad, w, lines.length * lh + pad, 7 * TN.dpr); ctx.fill();
  ctx.fillStyle = "#f4efe6"; ctx.textAlign = "center"; ctx.textBaseline = "top";
  lines.forEach((l, k) => ctx.fillText(l, x, y - lines.length * lh - pad / 2 + k * lh));
  ctx.textAlign = "start";
}

const KIND = { 0: { L: 32, W: 2.65, H: 3.3 }, 3: { L: 12, W: 2.55, H: 3.0 }, 4: { L: 18, W: 5, H: 2.2 } };
const TRAIN = { "TGV INOUI": ["#f3f1f4", "#7c2a6e"], "OUIGO": ["#e9f5fb", "#0096d6"], "INTERCITES": ["#eef0f6", "#2e3a87"],
                "Train TER": ["#f4f4f0", "#1f6fb2"], "Car TER": ["#f4f4f0", "#1f6fb2"] };
const POI_ICON = { "amenity=restaurant": "🍽", "amenity=fast_food": "🍔", "amenity=cafe": "☕", "amenity=bar": "🍸",
  "amenity=pub": "🍺", "amenity=cinema": "🎬", "amenity=theatre": "🎭", "amenity=nightclub": "🪩", "shop=bakery": "🥖",
  "tourism=museum": "🏛", "tourism=hotel": "🛏", "amenity=ice_cream": "🍦", "shop=books": "📚", "amenity=arts_centre": "🎨",
  "shop=chocolate": "🍫", "shop=cheese": "🧀", "shop=wine": "🍷", "amenity=marketplace": "🧺", "tourism=attraction": "⭐",
  "tourism=gallery": "🖼", "amenity=biergarten": "🍺", "shop=deli": "🥫", "leisure=stadium": "🏟" };

function visibleRect(margin = 40) {
  const v = TN.view, m = margin / v.s;
  return [v.x - innerWidth / 2 / v.s - m, v.y - innerHeight / 2 / v.s - m, v.x + innerWidth / 2 / v.s + m, v.y + innerHeight / 2 / v.s + m];
}

// ---------------------------------------------------------------- per-frame items
// ---------------------------------------------------------------- painted sprites (tinynantes/sprites.py)
// One sprite per vehicle and direction (16), rendered with the map's camera and repainted like the tiles.
const sprites = { atlas: null, sheets: [] };
async function loadSprites() {
  try {
    const a = await getJSON(`data/sprites/h${TN.H}/atlas.json`);
    sprites.sheets = a.sheets.map((f) => { const im = new Image(); im.src = `data/sprites/h${TN.H}/${f}${TN.v}`; return im; });
    sprites.atlas = a;
  } catch { /* no sprites: box models */ }
}
const SPRITE_CARS = ["car_white_hatch", "car_black_sedan", "car_grey_suv", "car_red_hatch", "car_blue_sedan",
  "car_silver_van", "car_green_hatch", "car_yellow_sedan"];
// draw sprite `name` with its ground centre at world (x, y, z), heading ang; false if not available
const spriteDir = new Map();            // key -> direction shown last (hysteresis: no flipping on a boundary)
function sprite(name, x, y, z, ang, key) {
  const A = sprites.atlas, e = A?.[name];
  if (!e) return false;
  const im = sprites.sheets[e.sheet];
  if (!im?.complete || !im.naturalWidth) return false;
  const f = ang / (2 * Math.PI) * A.dirs;
  let k = ((Math.round(f) % A.dirs) + A.dirs) % A.dirs;
  if (key != null) {
    const prev = spriteDir.get(key);
    if (prev != null) {
      let dd = f - prev; dd -= Math.round(dd / A.dirs) * A.dirs;      // signed distance in directions
      if (Math.abs(dd) < 0.75) k = prev;                               // stay until clearly past the boundary
    }
    spriteDir.set(key, k);
    if (spriteDir.size > 20000) spriteDir.clear();
  }
  const [cx, cy, w, h, ax, ay] = e.cells[k];
  const sc = TN.view.s * TN.dpr / e.ppm;
  const [X, Y] = toCanvas(...project(x, y, z));
  ctx.imageSmoothingEnabled = sc < 1;
  ctx.drawImage(im, cx, cy, w, h, X - ax * sc, Y - ay * sc, w * sc, h * sc);
  ctx.imageSmoothingEnabled = true;
  return true;
}

// ---------------------------------------------------------------- vehicle models (fallback when no sprite)
// Small stacked boxes read as vehicles at this scale: a ground shadow, a body, a glass band, a roof.
const GLASS = "#33414d", OUTLINE = "rgba(18,22,26,.7)";
function shadow(x, y, z, ang, L, W) {
  const c = Math.cos(ang), s = Math.sin(ang), hl = L / 2 + 0.3, hw = W / 2 + 0.3;
  const q = [[hl, hw], [hl, -hw], [-hl, -hw], [-hl, hw]].map(([u, v]) => toCanvas(...project(x + u * c - v * s, y + u * s + v * c, z)));
  ctx.fillStyle = "rgba(10,14,18,.28)";
  ctx.beginPath(); q.forEach((p, k) => (k ? ctx.lineTo(...p) : ctx.moveTo(...p))); ctx.closePath(); ctx.fill();
}
function carModel(x, y, z, ang, color) {
  const ol = TN.view.s > 1.6 ? OUTLINE : null;
  shadow(x, y, z, ang, 4.3, 1.8);
  box(x, y, z + 0.25, ang, 4.3, 1.8, 0.7, shade(color, 1.05), color, ol);                     // body
  const bx = x - Math.cos(ang) * 0.25, by = y - Math.sin(ang) * 0.25;
  box(bx, by, z + 0.95, ang, 2.3, 1.6, 0.55, shade(color, 1.15), GLASS, ol);                    // cabin: glass sides, roof
}
// a coach (bus, tram car, ferry deck): lower body in the line colour, a window band, a white roof
function coach(x, y, z, ang, L, W, H, color, roof = "#eef0ec") {
  const ol = TN.view.s > 1.2 ? OUTLINE : null;
  box(x, y, z + 0.35, ang, L, W, H * 0.33, shade(color, 1.0), color, ol);
  box(x, y, z + 0.35 + H * 0.33, ang, L - 0.2, W - 0.06, H * 0.38, "#dfe3e2", GLASS, ol);
  box(x, y, z + 0.35 + H * 0.71, ang, L, W, H * 0.29 - 0.35, roof, shade(roof, 0.86), ol);
}
const TRAM_SEG = 8.0;
function transitItems(t, rect) {
  const out = [];
  for (const v of vehicles) {
    const r = world.routes[v.route] || { type: 3, color: "#888888", short: "?" };
    const p = vehiclePose(v, t);
    const [sx, sy] = project(p.x, p.y, p.z);
    if (!inRect(sx, sy, rect)) continue;
    const hit = { r: 16, open: () => live.open?.("vehicle", { v, r }) };
    const tip = () => [`${{ 0: "Tram", 3: "Bus", 4: "Navibus" }[r.type] || "Ligne"} ${r.short} → ${v.headsign}`, delayText(v.delay)];
    const color = r.color && r.color !== "#ffffff" ? r.color : "#3a8f5c";
    if (r.type === 0) {
      // articulated tram: 4 cars following the track, the head at the pose
      const sh = world.shapes[v.shape];
      const segs = [0, 1, 2, 3].map((k) => chord(sh, p.d - k * TRAM_SEG - 0.6, TRAM_SEG - 1.2));
      const back = Math.min(...segs.map((q) => project(q.x, q.y, q.z)[1]));
      out.push({ sx, sy: back - 0.05, h: 0, draw: () => segs.forEach((q) => shadow(q.x, q.y, q.z, q.ang, TRAM_SEG, 2.65)) });
      for (let k = 0; k < 4; k++) {
        const q = segs[k];
        const [qx, qy] = project(q.x, q.y, q.z);
        out.push({ sx: qx, sy: qy, h: 3.3, light: 2, draw: () => {
                     if (!sprite(k === 0 ? "tram_head" : "tram_mid", q.x, q.y, q.z, q.ang, `${v.id}:${k}`)) coach(q.x, q.y, q.z, q.ang, TRAM_SEG - 0.35, 2.65, 3.3, color); },
                   hit: k === 0 ? hit : null, tip: k === 0 ? tip : null });
      }
    } else if (r.type === 4) {
      out.push({ sx, sy, h: 2.5, draw: () => { shadow(p.x, p.y, p.z, p.ang, 18, 5);
        if (sprite("navibus", p.x, p.y, p.z, p.ang, v.id)) return;
        box(p.x, p.y, p.z, p.ang, 18, 5, 1.0, "#f4f1ea", "#2c5d8a", OUTLINE);                     // hull
        box(p.x, p.y, p.z + 1.0, p.ang, 9, 3.6, 1.6, "#ffffff", GLASS, OUTLINE); }, hit, tip }); // cabin
    } else {
      const b = chord(world.shapes[v.shape], p.d + 6, 12);          // a 12 m bus on its two axles
      out.push({ sx, sy, h: 3.0, light: 2, draw: () => { shadow(b.x, b.y, b.z, b.ang, 12, 2.55);
        if (!sprite("bus", b.x, b.y, b.z, b.ang, v.id)) coach(b.x, b.y, b.z, b.ang, 12, 2.55, 3.0, color); }, hit, tip });
    }
  }
  return out;
}
function delayText(d) {
  if (d == null) return "horaire théorique";
  if (Math.abs(d) < 60) return "à l'heure";
  return d > 0 ? `retard ${Math.round(d / 60)} min` : `avance ${Math.round(-d / 60)} min`;
}
function personModel(x, y, z, ang, col, walk, bike) {
  const K = 1.9, ol = TN.view.s > 2 ? OUTLINE : null;            // a bit larger than life, or they'd vanish
  shadow(x, y, z, ang, bike ? 1.8 : 0.6 * K, 0.5 * K);
  if (bike) box(x, y, z + 0.35, ang, 1.7, 0.12, 0.5, "#2b2f36", "#2b2f36");
  const lift = bike ? 0.55 : 0;
  // legs swing while walking: two thin boxes
  if (!bike) {
    const sw = Math.sin(walk) * 0.18 * K, c = Math.cos(ang), s = Math.sin(ang);
    for (const side of [-1, 1]) {
      const ox = c * sw * side - s * 0.1 * K * side, oy = s * sw * side + c * 0.1 * K * side;
      box(x + ox, y + oy, z, ang, 0.16 * K, 0.14 * K, 0.42 * K, "#2b2f36", "#3a3f46");
    }
  }
  box(x, y, z + lift + 0.42 * K, ang, 0.34 * K, 0.42 * K, 0.5 * K, shade(col, 1.12), col, ol);         // body
  box(x, y, z + lift + 0.92 * K, ang, 0.24 * K, 0.24 * K, 0.24 * K, "#f1c9a5", "#d9a982", ol);           // head
}
function inhabitantItems(t, rect) {
  const out = [];
  if (TN.view.s < 0.9 && !TN.playing) return out;
  const now = performance.now() / 1000;
  for (const [id, k, x, y, z, a, v, c] of inhabitants()) {
    const [sx, sy] = project(x, y, z);
    if (!inRect(sx, sy, rect)) continue;
    const hit = { r: 10, open: () => live.open?.("agent", { id }) };
    if (k === 0) out.push({ sx, sy, h: 1.5, light: 1, hit, draw: () => {
      shadow(x, y, z, a, 4.3, 1.8);
      if (!sprite(SPRITE_CARS[c % SPRITE_CARS.length], x, y, z, a, id)) carModel(x, y, z, a, CAR_COLORS[c % CAR_COLORS.length]); } });
    else if (TN.view.s >= 1.6 || TN.playing) {
      const col = CLOTHES[c % CLOTHES.length], phase = now * v * 5 + id;
      const name = k === 2 ? `bike_${c % 8}` : `walker_${c % 8}_${v > 0.2 ? Math.floor(now * v * 1.6 + id) % 2 : 0}`;
      out.push({ sx, sy, h: 2, hit, draw: () => { if (!sprite(name, x, y, z, a, id)) personModel(x, y, z, a, col, phase, k === 2); } });
    }
  }
  return out;
}
function trainItems(t, rect) {
  const out = [], R = world.rails;
  if (!R) return out;
  for (const m of trains) {
    const atNantes = m.from.name === "Nantes" ? m.from : m.to, other = m.from.name === "Nantes" ? m.to : m.from;
    const ang = Math.atan2(other.y - R.station[1], other.x - R.station[0]);
    let best = null, bd = 9;
    for (const r of R.routes) {
      const e = r.p[r.p.length - 1], a = Math.atan2(e[1] - R.station[1], e[0] - R.station[0]);
      const dd = Math.abs(Math.atan2(Math.sin(a - ang), Math.cos(a - ang)));
      if (dd < bd) { bd = dd; best = r; }
    }
    if (!best) continue;
    const straight = Math.hypot(other.x - R.station[0], other.y - R.station[1]);
    let d;
    if (t >= atNantes.arr && t <= atNantes.dep) d = 0;
    else if (m.from === atNantes) d = (t - atNantes.dep) / Math.max(60, m.to.arr - atNantes.dep) * straight;
    else d = (atNantes.arr - t) / Math.max(60, atNantes.arr - m.from.dep) * straight;
    const [top, side] = TRAIN[m.mode] || ["#f4f4f0", "#555555"];
    const ncar = /TGV|OUIGO/.test(m.mode) ? 8 : 4;
    for (let k = 0; k < ncar; k++) {
      const dk = d + k * 26 * (m.from === atNantes ? -1 : 1) + 13;
      if (dk < 0 || dk > best.len) continue;
      const p = along(best, dk);
      const [sx, sy] = project(p.x, p.y, p.z);
      if (!inRect(sx, sy, rect)) continue;
      out.push({ sx, sy, h: 4, draw: () => { shadow(p.x, p.y, p.z, p.ang, 25, 2.9); coach(p.x, p.y, p.z, p.ang, 25, 2.9, 4.0, side, top); },
                 hit: k === 0 ? { r: 18, open: () => live.open?.("train", m) } : null,
                 tip: k === 0 ? () => [`${m.mode} ${m.number} → ${m.headsign}`, m.delay ? `retard ${Math.round(m.delay / 60)} min` : "à l'heure"] : null });
    }
  }
  return out;
}
function planeItems(t, rect) {
  const out = [], dt = Date.now() / 1000 - planesAt;
  for (const pl of planes) {
    const tr = pl.track * Math.PI / 180;
    const x = pl.x + Math.sin(tr) * pl.speed * dt, y = pl.y + Math.cos(tr) * pl.speed * dt;
    const g = zAt(x, y), z = Math.max(g, pl.alt + pl.vrate * dt);
    const [sx, sy] = project(x, y, z);
    if (!inRect(sx, sy, rect)) continue;
    const ang = Math.atan2(Math.cos(tr), Math.sin(tr));
    const [gx, gy] = project(x, y, g);
    out.push({ sx, sy: gy, h: z - g + 4, draw: () => {
      // shadow on the ground, then the aircraft at its altitude
      const [cx, cy] = toCanvas(gx, gy);
      ctx.fillStyle = "rgba(0,0,0,.18)"; ctx.beginPath(); ctx.ellipse(cx, cy, 14 * TN.dpr * TN.view.s / 2, 5 * TN.dpr * TN.view.s / 2, 0, 0, 7); ctx.fill();
      box(x, y, z, ang, 36, 4, 4, "#f4f4f2", "#c9ccd2", "rgba(20,24,28,.4)");
      box(x, y, z + 1.2, ang, 6, 34, 0.6, "#e8e9ec", "#b5b9c0");
    }, hit: { r: 20, at: [sx, sy], open: () => live.open?.("plane", pl) },
      tip: () => [`✈ ${pl.callsign || pl.hex} ${pl.kind || ""}`, pl.route ? `${pl.route.from?.city || "?"} → ${pl.route.to?.city || "?"}` : "", pl.ground ? "au sol" : `${Math.round(pl.alt)} m`] });
  }
  return out;
}
function poiItems(t, rect) {
  const out = [];
  const s = TN.view.s;
  if (s < 0.9 && !TN.playing) return out;
  for (const p of world.pois) {
    if (s < 1.8 && !TN.playing && !menus.has(p.id)) continue;      // zoomed out: only places with a menu
    const [x, y, z] = p.p, [sx, sy] = project(x, y, z);
    if (!inRect(sx, sy, rect)) continue;
    out.push({ sx, sy, h: 0, overlay: true, draw: () => {
      const [cx, cy] = toCanvas(sx, sy);
      ctx.font = `${13 * TN.dpr}px system-ui, "Apple Color Emoji", "Noto Color Emoji", sans-serif`;
      ctx.textAlign = "center"; ctx.textBaseline = "bottom";
      ctx.fillStyle = "rgba(255,255,255,.85)"; ctx.beginPath(); ctx.arc(cx, cy - 12 * TN.dpr, 10 * TN.dpr, 0, 7); ctx.fill();
      ctx.fillText(POI_ICON[p.k] || "•", cx, cy - 4 * TN.dpr); ctx.textAlign = "start";
      if (menus.has(p.id)) { ctx.fillStyle = "#f2b544"; ctx.beginPath(); ctx.arc(cx + 8 * TN.dpr, cy - 20 * TN.dpr, 3.5 * TN.dpr, 0, 7); ctx.fill(); }
    }, hit: { r: 12, dy: -12, open: () => live.open?.("poi", p) }, tip: () => [p.t.name, (p.t.cuisine || "").replace(/;/g, ", ")] });
  }
  return out;
}
function placeItems(t, rect) {
  const out = [];
  const st = world.rails?.station;
  if (st) {
    const [sx, sy] = project(st[0], st[1], zAt(st[0], st[1]));
    if (inRect(sx, sy, rect)) out.push({ sx, sy, h: 0, overlay: true, draw: () => pin(sx, sy, "🚉"),
      hit: { r: 16, dy: -18, open: () => live.open?.("station", {}) }, tip: () => ["Gare de Nantes", "départs et arrivées"] });
  }
  const ap = world.airport;
  if (ap) {
    const [sx, sy] = project(ap[0], ap[1], zAt(ap[0], ap[1]));
    if (inRect(sx, sy, rect)) out.push({ sx, sy, h: 0, overlay: true, draw: () => pin(sx, sy, "✈"),
      hit: { r: 16, dy: -18, open: () => live.open?.("airport", {}) }, tip: () => ["Aéroport Nantes Atlantique", "décollages et atterrissages"] });
  }
  if (TN.view.s >= 0.35) for (const e of events) {
    const [sx, sy] = project(e.p[0], e.p[1], zAt(e.p[0], e.p[1]));
    if (!inRect(sx, sy, rect)) continue;
    out.push({ sx, sy, h: 0, overlay: true, draw: () => {
      const [cx, cy] = toCanvas(sx, sy), k = 1 + 0.25 * Math.sin(Date.now() / 300 + e.p[0]);
      ctx.fillStyle = "rgba(242,181,68,.92)"; ctx.strokeStyle = "#fff"; ctx.lineWidth = 2 * TN.dpr;
      ctx.beginPath(); ctx.arc(cx, cy - 14 * TN.dpr, 6 * TN.dpr * k, 0, 7); ctx.fill(); ctx.stroke();
      ctx.beginPath(); ctx.moveTo(cx, cy); ctx.lineTo(cx, cy - 8 * TN.dpr); ctx.stroke();
    }, hit: { r: 12, dy: -14, open: () => live.open?.("event", e) },
      tip: () => [e.name, `${e.start || ""}${e.end ? "–" + e.end : ""} · ${e.place}`] });
  }
  return out;
}
function pin(sx, sy, icon) {
  const [cx, cy] = toCanvas(sx, sy);
  ctx.fillStyle = "#1f2428"; ctx.beginPath(); ctx.arc(cx, cy - 18 * TN.dpr, 13 * TN.dpr, 0, 7); ctx.fill();
  ctx.strokeStyle = "#f2b544"; ctx.lineWidth = 2 * TN.dpr; ctx.stroke();
  ctx.font = `${15 * TN.dpr}px system-ui, "Noto Color Emoji", sans-serif`; ctx.textAlign = "center"; ctx.textBaseline = "middle";
  ctx.fillText(icon, cx, cy - 17 * TN.dpr); ctx.textAlign = "start";
}
const inRect = (sx, sy, r) => sx >= r[0] && sx <= r[2] && sy >= r[1] && sy <= r[3];

// ---------------------------------------------------------------- frame
function frame() {
  requestAnimationFrame(frame);
  if (!world.ready || !TN.man) return;
  const w = Math.round(innerWidth * TN.dpr), h = Math.round(innerHeight * TN.dpr);
  if (cv.width !== w || cv.height !== h) { cv.width = w; cv.height = h; }
  ctx.clearRect(0, 0, w, h);
  hits = [];
  const on = document.getElementById("liveBtn")?.getAttribute("aria-pressed") === "true";
  const t = live.now(), rect = visibleRect();
  let items = [];
  if (on) {
    items.push(...planeItems(t, visibleRect(4000)));
    if (TN.view.s >= 0.6 || TN.playing) items.push(...transitItems(t, rect), ...trainItems(t, rect), ...inhabitantItems(t, rect));
    items.push(...placeItems(t, rect), ...poiItems(t, rect));
  }
  for (const f of live.layers) items.push(...f(t, rect));
  const world3d = items.filter((i) => !i.overlay).sort((a, b) => a.sy - b.sy);   // painter's order
  const flat = items.filter((i) => i.overlay);
  let tip = null;
  const P = TN.pointer;
  const consider = (it) => {
    if (!it.hit) return;
    const [cx, cy] = toCanvas(...(it.hit.at || [it.sx, it.sy]));
    const hy = cy + (it.hit.dy || 0) * TN.dpr;
    hits.push({ x: cx, y: hy, r: it.hit.r * TN.dpr, open: it.hit.open });
    if (P && it.tip && Math.hypot(cx - P[0] * TN.dpr, hy - P[1] * TN.dpr) < it.hit.r * TN.dpr) tip = [cx, hy, it.tip()];
  };
  const lights = [];
  for (const f of live.under) f(t);
  for (const it of world3d) {
    if (it.sy && !it.room && occluded(it.sx, it.sy - (it.h || 2) * COS45 * 0.5, it.sy)) continue;
    it.draw(); consider(it);
    if (it.light) lights.push([...toCanvas(it.sx, it.sy), it.light]);
  }
  live.lights = lights;
  for (const it of flat) { it.draw(); consider(it); }
  if (tip) label(tip[0], tip[1] - 20 * TN.dpr, tip[2]);
  TN.liveStats = { vehicles: vehicles.length, inhabitants: snaps.at(-1)?.m.size || 0, planes: planes.length, trains: trains.length, drawn: world3d.length + flat.length };
}

// a click on the map (from app.js): open whatever is under it
TN.click = (x, y) => {
  let best = null, bd = 1e9;
  for (const h of hits) {
    const d = Math.hypot(h.x - x * TN.dpr, h.y - y * TN.dpr);
    if (d < h.r * 1.4 && d < bd) { bd = d; best = h; }
  }
  if (best) { best.open(); return true; }
  return false;
};

// ---------------------------------------------------------------- start
addEventListener("pointermove", (e) => { TN.pointer = [e.clientX, e.clientY]; });
loadWorld().then(() => {
  loadSprites();
  pollVehicles(); setInterval(pollVehicles, 20_000);
  pollTraffic(); setInterval(pollTraffic, 180_000);
  connectWorld(); setInterval(sendView, 300);
  pollEvents(); setInterval(pollEvents, 600_000);
  pollPlanes(); setInterval(pollPlanes, 20_000);
  pollTrains(); setInterval(pollTrains, 30_000);
  pollMenus(); setInterval(pollMenus, 900_000);
  requestAnimationFrame(frame);
}).catch((e) => console.warn("live layer off:", e));
