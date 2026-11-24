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
  world, project, toCanvas, zAt, get TRAFFIC() { return TRAFFIC; }, along, vehiclePose, occluded, heightAt, vlamps, box, label, shade, sprite, get SPRITE_CARS() { return SPRITE_CARS; },
  get ctx() { return ctx; }, room: null,
  get vehicles() { return vehicles; }, get people() { return inhabitants(); }, get bikeStations() { return bikeStations; }, rented: {}, returned: {}, get menus() { return menus; }, get sections() { return sections; },
  lights: [],                                 // light sources drawn this frame (canvas px), read by fx.js at night
  now: () => Date.now() / 1000 + clockSkew,
  layers: [],                                 // functions (t) -> items, added by game.js
  under: [],                                  // functions drawn before the items (game.js: the opened building)
  open: null,                                 // set by game.js: open(kind, data) shows the side panel
};
window.TNLive = live;
live.boats = () => boatItems(live.now(), [-1e9, -1e9, 1e9, 1e9]).map((b) => [b.sx, b.sy, b.tip?.()]);   // debug: where the boats are

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
  try { world.water = boatPlan(await getJSON("data/world/water.json")); } catch { /* optional */ }
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
// ---------------------------------------------------------------- le Grand Éléphant (Les Machines de l'île)
// Departures from lesmachines-nantes.fr/fr/infos-pratiques/horaires (checked 24 Nov 2026); 30-minute rides out
// across the Parc des Chantiers and back. No live position is published.
const ELEPHANT_ROUTE = [[-1262, -1020], [-1292, -1015], [-1322, -1010], [-1349, -1008]];   // out of the Nefs, across the Parc des Chantiers
const EL_MAINT = ["2026-11-02", "2027-02-28"];
const EL_TIMES = {
  autumnWeek: ["10:15", "11:00", "11:45", "12:30", "13:15", "14:00", "15:00", "15:45"],
  autumnWeekend: ["10:15", "11:00", "11:45", "14:15", "15:00", "15:45", "16:30", "17:15", "18:00"],
  toussaint: ["10:15", "11:00", "11:45", "12:30", "13:15", "14:00", "15:00", "15:45", "16:30", "17:15", "18:00", "18:45"],
  usual: ["10:15", "11:00", "11:45", "12:30", "14:00", "15:00", "15:45", "16:30", "17:15"],
};
function parisNow(t) {
  const f = new Intl.DateTimeFormat("en-CA", { timeZone: "Europe/Paris", year: "numeric", month: "2-digit", day: "2-digit",
    hour: "2-digit", minute: "2-digit", second: "2-digit", weekday: "short", hourCycle: "h23" });
  const o = Object.fromEntries(f.formatToParts(new Date(t * 1000)).map((p) => [p.type, p.value]));
  return { date: `${o.year}-${o.month}-${o.day}`, md: `${o.month}-${o.day}`, wd: o.weekday, min: +o.hour * 60 + +o.minute + +o.second / 60 };
}
function elephantDay(p) {
  if (p.date >= EL_MAINT[0] && p.date <= EL_MAINT[1]) return { times: [], why: "en maintenance décennale jusqu'au 28 février 2027" };
  if (p.md >= "09-01" && p.md <= "10-16") {
    if (p.wd === "Mon") return { times: [], why: "pas de balade le lundi" };
    return { times: p.wd === "Sat" || p.wd === "Sun" ? EL_TIMES.autumnWeekend : EL_TIMES.autumnWeek };
  }
  if (p.md >= "10-17" && p.md <= "11-01") return { times: EL_TIMES.toussaint };
  return { times: EL_TIMES.usual, rough: true };
}
const RIDE = 30, OUT = [3, 11], BACK = [19, 27];        // boarding, out, a long stop (it sprays the crowd), back
function routeAt(f) {          // position along the route, f in [0, 1]
  const L = [0];
  for (let i = 1; i < ELEPHANT_ROUTE.length; i++) L.push(L[i - 1] + Math.hypot(ELEPHANT_ROUTE[i][0] - ELEPHANT_ROUTE[i - 1][0], ELEPHANT_ROUTE[i][1] - ELEPHANT_ROUTE[i - 1][1]));
  const d = Math.max(0, Math.min(1, f)) * L.at(-1);
  let i = 1; while (i < L.length - 1 && L[i] < d) i++;
  const a = ELEPHANT_ROUTE[i - 1], b = ELEPHANT_ROUTE[i], u = (d - L[i - 1]) / (L[i] - L[i - 1] || 1);
  return [a[0] + (b[0] - a[0]) * u, a[1] + (b[1] - a[1]) * u, Math.atan2(b[1] - a[1], b[0] - a[0])];
}
function elephantState(t) {
  if (live.elephantAt) t = live.elephantAt + (t % 1800);       // debug: a chosen moment
  const p = parisNow(t), day = elephantDay(p);
  const deps = day.times.map((h) => { const [H, M] = h.split(":"); return +H * 60 + +M; });
  const cur = deps.find((d) => p.min >= d && p.min < d + RIDE);
  const next = day.times.filter((h, i) => deps[i] > p.min);
  let x, y, ang, phase = "parked";
  if (cur == null) { [x, y, ang] = routeAt(0); }
  else {
    const m = p.min - cur;
    if (m < OUT[0]) { [x, y, ang] = routeAt(0); phase = "boarding"; }
    else if (m < OUT[1]) { [x, y, ang] = routeAt((m - OUT[0]) / (OUT[1] - OUT[0])); phase = "walking"; }
    else if (m < BACK[0]) { [x, y, ang] = routeAt(1); phase = "spraying"; }
    else if (m < BACK[1]) { [x, y, ang] = routeAt(1 - (m - BACK[0]) / (BACK[1] - BACK[0])); ang += Math.PI; phase = "walking"; }
    else { [x, y, ang] = routeAt(0); ang += Math.PI; phase = "boarding"; }
  }
  return { x, y, ang, phase, next, why: day.why, rough: day.rough };
}
live.elephant = () => elephantState(live.now());
function elephantItems(t, rect) {
  const e = elephantState(t), z = zAt(e.x, e.y);
  const [sx, sy] = project(e.x, e.y, z);
  if (!inRect(sx, sy, rect)) return [];
  const step = e.phase === "walking" ? Math.floor(t / 0.9) % 2 : 0;
  const out = [{ sx, sy, h: 12, tall: true, hit: { r: 30, dy: -25, open: () => live.open?.("elephant", e) },
    tip: () => ["🐘 Le Grand Éléphant", e.why || { walking: "en balade", spraying: "s'arrête et arrose", boarding: "embarquement", parked: e.next.length ? `prochain départ ${e.next[0]}` : "au repos" }[e.phase]],
    draw: () => {
      if (!sprite(`elephant_${step}`, e.x, e.y, z, e.ang, "elephant")) {
        box(e.x, e.y, z + 5.4, e.ang, 14, 6.4, 4.2, "#9c7048", "#7d5634", OUTLINE);
        box(e.x + Math.cos(e.ang) * 8, e.y + Math.sin(e.ang) * 8, z + 6.6, e.ang, 4, 4.2, 4.4, "#9c7048", "#7d5634", OUTLINE);
      }
      if (e.phase === "spraying") {            // the trunk sprays the crowd
        for (let k = 0; k < 10; k++) {
          const u = ((t * 1.3 + k / 10) % 1), d = 12 + u * 9;
          box(e.x + Math.cos(e.ang) * d, e.y + Math.sin(e.ang) * d, z + 4 + 6 * Math.sin(u * Math.PI), 0, 0.35, 0.35, 0.35, "#bfe6ff", "#8cc8f0");
        }
      }
    } }];
  if (e.why && TN.view.s >= 1.0) out.push({ sx, sy, overlay: true, draw: () => {
    const [cx, cy] = toCanvas(...project(e.x, e.y, z + 14));
    label(cx, cy, ["🔧 en maintenance"], "rgba(120,80,30,.92)");
  } });
  return out;
}

let bikeStations = [];
async function pollBikes() {
  try { const b = await getJSON("api/bikes"); bikeStations = b.stations; live.bikesStale = b.stale && b.reported; } catch { /* keep */ }
}
// Naolib Vélo stations: a dock bar with the live count of bikes (open data, JCDecaux GBFS)
function bikeStationItems(t, rect) {
  const out = [];
  for (const st of bikeStations) {
    const n = st.bikes - (live.rented?.[st.id] || 0) + (live.returned?.[st.id] || 0);
    const [x, y] = st.p, z = zAt(x, y);
    const [sx, sy] = project(x, y, z);
    if (!inRect(sx, sy, rect)) continue;
    out.push({ sx, sy, h: 1.2, hit: { r: 12, open: () => live.open?.("bikes", st) }, tip: () => [`🚲 ${st.name}`, st.renting ? `${Math.max(0, n)} vélos · ${st.docks} places` : "hors service"],
      draw: () => {
        box(x, y, z, 0.3, 4, 0.5, 0.25, "#6b737b", "#4a5158", OUTLINE);                            // dock rail
        // totem: Naolib yellow, or grey when the operator has the station out of service
        box(x - 2.2, y, z, 0.3, 0.3, 0.3, 2.2, st.renting ? "#ffd400" : "#8a8f94", st.renting ? "#d9a800" : "#62676c", OUTLINE);
        if (!st.renting) return;
        const k = Math.min(Math.max(0, n), 6);
        for (let i = 0; i < k; i++) box(x - 1.5 + i * 0.6, y + 0.2 * Math.sin(0.3) * i, z + 0.25, 1.87, 1.5, 0.1, 0.9, "#9be15d", "#3e8d2f");
      } });
    if (TN.view.s >= 1.2) out.push({ sx, sy, overlay: true, draw: () => {
      const [cx, cy] = toCanvas(...project(x - 2.2, y, z + 2.8));
      label(cx, cy, [`${Math.max(0, n)}`], n > 0 ? "rgba(40,120,50,.92)" : "rgba(150,40,40,.92)");
    } });
  }
  return out;
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
  worldWs.onopen = () => { viewSent = ""; sendView(); live.worldSince = performance.now(); };
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
    const x = a[2] + (b[2] - a[2]) * f, y = a[3] + (b[3] - a[3]) * f;
    out.push([id, b[1], x, y, a[4] + (b[4] - a[4]) * f, facing(id, x, y, a[5] + da * f, b[6]), b[6], b[7]]);
  }
  if (out.length < motion.size / 2) for (const k of motion.keys()) if (!s1.m.has(k)) motion.delete(k);
  return out;
}
// The way an inhabitant faces = the way it is seen moving (its drawn position is offset to the kerb and to the
// right of the road, so at corners the road's direction and its actual motion differ), smoothed; the server's
// heading when it stands still (talking, waiting, gathered).
const motion = new Map();               // id -> {x, y, ang}
function facing(id, x, y, served, v) {
  let m = motion.get(id);
  if (!m) { m = { x, y, ang: served }; motion.set(id, m); return served; }
  const dx = x - m.x, dy = y - m.y, d = Math.hypot(dx, dy);
  if (d > 30) { m.x = x; m.y = y; m.ang = served; return served; }   // jumped (a new path): start again
  if (v < 0.05) { m.x = x; m.y = y; let e = served - m.ang; m.ang += Math.atan2(Math.sin(e), Math.cos(e)) * 0.2; return m.ang; }
  if (d > 0.35) {                                                    // moved enough to read a direction
    const t = Math.atan2(dy, dx); let e = t - m.ang; e = Math.atan2(Math.sin(e), Math.cos(e));
    m.ang += e * Math.min(1, 0.35 + d * 0.15); m.x = x; m.y = y;
  }
  return m.ang;
}

// ---------------------------------------------------------------- occlusion (height tiles)
// Height maps (one byte per pixel, 0.5 m steps): kept as single-channel arrays so a few hundred fit in memory.
// The cache is least-recently-used; a FIFO of 64 thrashed when zoomed out (more tiles on screen than that),
// and every reload made lights and sprites pop in and out of occlusion.
const HGT_MAX = 320;
function heightTile(i, j) {
  const k = `${TN.H}/${i}_${j}`;
  let t = hgt.get(k);
  if (t) { hgt.delete(k); hgt.set(k, t); return t; }        // refresh its place in the LRU order
  t = { ready: false };
  hgt.set(k, t);
  if (TN.man?.sets && !TN.man.sets[0].has(`${i}_${j}`)) { t.missing = true; return t; }   // off the map: no file
  const im = new Image();
  im.onload = () => {
    const c = document.createElement("canvas"); c.width = im.width; c.height = im.height;
    const g = c.getContext("2d", { willReadFrequently: true }); g.drawImage(im, 0, 0);
    const rgba = g.getImageData(0, 0, im.width, im.height).data, n = im.width * im.height, d = new Uint8Array(n);
    for (let q = 0; q < n; q++) d[q] = rgba[4 * q];
    t.data = d; t.w = im.width; t.ready = true;
  };
  im.onerror = () => { t.missing = true; };
  im.src = `data/h${TN.H}/hgt/${i}_${j}.png${TN.tv}`;
  while (hgt.size > HGT_MAX) hgt.delete(hgt.keys().next().value);
  return t;
}
// Height (m) of what is drawn at screen point (sx, sy), or null while its height map is loading.
function heightAt(sx, sy) {
  const T = TN.man.tile_m, i = Math.floor(sx / T), j = Math.floor(sy / T);
  const t = heightTile(i, j);
  if (t.missing) return 0;
  if (!t.ready) return null;
  const px = Math.floor((sx - i * T) / T * t.w), py = Math.floor((sy - j * T) / T * t.w);
  return t.data[py * t.w + px] * 0.5;
}
// Is screen point (sx, sy) covered by something standing in front of a sprite whose foot is at footY?
function occluded(sx, sy, footY) {
  const T = TN.man.tile_m, i = Math.floor(sx / T), j = Math.floor(sy / T);
  const t = heightTile(i, j);
  if (!t.ready) return false;
  const px = Math.floor((sx - i * T) / T * t.w), py = Math.floor((sy - j * T) / T * t.w);
  const h = t.data[py * t.w + px] * 0.5;
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
    for (const [n, e] of Object.entries(a)) if (/^(walker|down)_/.test(n)) e.person = true;
    sprites.atlas = a;
  } catch { /* no sprites: box models */ }
}
const SPRITE_CARS = ["car_white_hatch", "car_black_sedan", "car_grey_suv", "car_red_hatch", "car_blue_sedan",
  "car_silver_van", "car_green_hatch", "car_yellow_sedan"];
// Server cars by variant byte: mostly cars, some vans, a few lorries and taxis ([sprite, length, width]).
const TRAFFIC = [...["car_white_hatch", "car_black_sedan", "car_grey_suv", "car_red_hatch", "car_blue_sedan", "car_silver_van",
  "car_green_hatch", "car_yellow_sedan"].map((n) => [n, 4.3, 1.8]), ["car_teal_hatch", 3.9, 1.8], ["van_delivery", 5.6, 1.95],
  ["truck_box", 7.6, 2.3], ["car_taxi", 4.5, 1.8], ...["car_white_hatch", "car_grey_suv", "car_blue_sedan", "car_black_sedan"].map((n) => [n, 4.3, 1.8])];
// draw sprite `name` with its ground centre at world (x, y, z), heading ang; false if not available
const spriteDir = new Map();            // key -> direction shown last (hysteresis: no flipping on a boundary)
function sprite(name, x, y, z, ang, key, opt) {
  const A = sprites.atlas, e = A?.[name];
  if (!e) return false;
  const im = sprites.sheets[e.sheet];
  if (!im?.complete || !im.naturalWidth) return false;
  const N = e.cells.length, one = N === 1;                         // 64 directions (vehicles), 16 (people, big things)
  const f = ang / (2 * Math.PI) * N;
  let k = ((Math.round(f) % N) + N) % N;
  if (one) k = 0;                                                   // a moored ship: drawn in its one real heading
  else if (key != null) {
    const prev = spriteDir.get(key);
    if (prev != null && prev < N) {
      let dd = f - prev; dd -= Math.round(dd / N) * N;              // signed distance in directions
      if (Math.abs(dd) < 0.6) k = prev;                              // stay until past the boundary (no flicker)
    }
    spriteDir.set(key, k);
    if (spriteDir.size > 20000) spriteDir.clear();
  }
  const [cx, cy, w, h, ax, ay] = e.cells[k];
  const sc = TN.view.s * TN.dpr / e.ppm * (opt?.scale || 1);
  const [X, Y] = toCanvas(...project(x, y, z));
  ctx.imageSmoothingEnabled = sc < 1;
  // Vehicles have 64 headings (5.6° apart): drawn as they are, crisp, never distorted. The big 16-heading things
  // (planes, trains, boats) turn the rest of the way by a turn of the ground plane by d, as the camera sees it:
  // M = P·R(d)·P⁻¹ (P: ground metres -> screen), applied about the anchor; heights shear a little (≤13°).
  // People (16 headings too) are never sheared: a figure leaning 10° looks wrong, 11° off course does not.
  let d = one || N >= 64 || e.person ? 0 : ang - k * 2 * Math.PI / N;
  d = Math.atan2(Math.sin(d), Math.cos(d));
  if (opt?.rot || Math.abs(d) > 0.004) {
    ctx.save(); ctx.translate(X, Y);
    if (Math.abs(d) > 0.004) {
      const [r, q] = TN.man.axes, a = r[0], b = r[1], c = q[0], g = q[1], det = a * g - b * c;
      const co = Math.cos(d), si = Math.sin(d);
      // P·R = [[a co + b si, -a si + b co], [c co + g si, -c si + g co]]; times P⁻¹ = [[g, -b], [-c, a]] / det
      const m00 = a * co + b * si, m01 = -a * si + b * co, m10 = c * co + g * si, m11 = -c * si + g * co;
      ctx.transform((m00 * g - m01 * c) / det, (m10 * g - m11 * c) / det, (-m00 * b + m01 * a) / det, (-m10 * b + m11 * a) / det, 0, 0);
    }
    if (opt?.rot) ctx.rotate(opt.rot);                              // a bank: turn the sprite about its anchor
    ctx.drawImage(im, cx, cy, w, h, -ax * sc, -ay * sc, w * sc, h * sc);
    ctx.restore();
  } else ctx.drawImage(im, cx, cy, w, h, X - ax * sc, Y - ay * sc, w * sc, h * sc);
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
        const lp = k === 0 ? vlamps(q.x, q.y, q.z, q.ang, TRAM_SEG, 2.4, 0.9, { noTail: true }) : k === 3 ? vlamps(q.x, q.y, q.z, q.ang, TRAM_SEG, 2.4, 0.9, { noBeam: true }).filter((l) => l[3] === LAMP_TAIL) : null;
        out.push({ sx: qx, sy: qy, h: 3.3, lamps: lp, draw: () => {
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
      out.push({ sx, sy, h: 3.0, lamps: vlamps(b.x, b.y, b.z, b.ang, 12, 2.4, 0.9), draw: () => { shadow(b.x, b.y, b.z, b.ang, 12, 2.55);
        if (!sprite("bus", b.x, b.y, b.z, b.ang, v.id)) coach(b.x, b.y, b.z, b.ang, 12, 2.55, 3.0, color); }, hit, tip });
    }
  }
  return out;
}
// ---------------------------------------------------------------- boats
// Moored landmarks (Maillé-Brézé, Belem) where OSM has them; barges, sailing and motor boats going up and down
// the Loire, the Erdre and the Sèvre; boats calling at the boatyards (Chantier de l'Esclain). Positions are a
// function of the clock only, so every viewer sees the same boats in the same places.
const BOAT = {              // sprite, length, beam, speed m/s, label
  peniche: ["boat_peniche", 38.5, 5.05, 2.6, "Péniche"], peniche2: ["boat_peniche2", 38.5, 5.05, 2.4, "Péniche"],
  sail: ["boat_sail", 10, 3.3, 2.2, "Voilier"], motor: ["boat_motor", 8, 2.8, 4.5, "Bateau à moteur"],
};
const RIVER_FLEET = {       // per km of river: which boats
  "La Loire": ["peniche", "sail", "motor", "peniche2", "motor"], "La Loire - Bras de la Madeleine": ["motor", "sail"],
  "La Loire - Bras de Pirmil": ["motor", "peniche", "sail"], "L'Erdre": ["sail", "peniche", "motor", "sail"],
  "La Sèvre Nantaise": ["motor", "sail"],
};
const SHIP_INFO = { ship_maille_breze: ["Maillé-Brézé", "escorteur d'escadre (1957), musée naval"],
                    ship_belem: ["Belem", "trois-mâts barque (1896), à quai"] };
function boatPlan(w) {
  const runs = [];
  for (const r of w.rivers) {
    const fleet = RIVER_FLEET[r.name];
    if (!fleet) continue;
    withCum(r);
    const n = Math.max(1, Math.round(r.len / 1000));
    for (let k = 0; k < n; k++) runs.push({ shape: r, kind: fleet[k % fleet.length], phase: (k * 7919 + r.len * 13) % 100000, river: r.name, id: `${r.name}:${k}` });
  }
  const yards = (w.yards || []).filter((y) => y.name && y.path.length > 1).map((y) => withCum({ name: y.name, p: y.path }));
  const moored = (w.moored || []).filter((m) => (m.ship === "maille_breze" && m.len > 100) || m.ship === "belem")
    .map((m) => ({ ...m, sprite: `ship_${m.ship}` }));
  // the other boats OSM has moored (péniches on the Erdre: Bateau Lavoir, Morgenster...), scaled to their length
  const barges = (w.moored || []).filter((m) => m.ship === "maille_breze" && m.len <= 100)
    .map((m, k) => ({ ...m, kind: k % 2 ? "peniche2" : "peniche", scale: Math.max(0.45, Math.min(1, m.len / 38.5)) }));
  return { runs, yards, moored, barges };
}
function boatItems(t, rect) {
  const out = [], W = world.water;
  if (!W) return out;
  const push = (id, kind, p, ang, tip) => {
    const [name, L, B] = BOAT[kind];
    const [sx, sy] = project(p.x, p.y, p.z);
    if (!inRect(sx, sy, rect)) return;
    out.push({ sx, sy, h: kind === "sail" ? 12 : 3, lamps: vlamps(p.x, p.y, p.z, ang, L, B * 0.6, 1.4, { noBeam: true }),
               draw: () => { if (!sprite(name, p.x, p.y, p.z, ang, id)) { shadow(p.x, p.y, p.z, ang, L, B); box(p.x, p.y, p.z, ang, L, B, 1.2, "#f4f1ea", "#2c3440", OUTLINE); } },
               hit: { r: 14, open: () => {} }, tip });
  };
  for (const b of W.runs) {                       // back and forth along the river, turning at each end
    const v = BOAT[b.kind][3], L = b.shape.len, u = (b.phase + t * v) % (2 * L);
    const fwd = u < L, d = fwd ? u : 2 * L - u;
    const p = along(b.shape, d);
    push(b.id, b.kind, p, fwd ? p.ang : p.ang + Math.PI, () => [BOAT[b.kind][4], b.river]);
  }
  for (const y of W.yards) {                      // two boats an hour: in, 10 min at the slip, out
    for (const [k, kind] of [[0, "sail"], [1, "motor"]]) {
      const v = BOAT[kind][3], go = y.len / v, cycle = 1800, u = (t + k * 900) % cycle;
      let d, out_;
      if (u < go) { d = u * v; out_ = false; } else if (u < go + 600) { d = y.len; out_ = false; }
      else if (u < 2 * go + 600) { d = y.len - (u - go - 600) * v; out_ = true; } else continue;
      const p = along(y, d);
      push(`${y.name}:${k}`, kind, p, out_ ? p.ang + Math.PI : p.ang, () => [BOAT[kind][4], out_ ? `quitte le ${y.name}` : u < go ? `vers le ${y.name}` : `au ${y.name}`]);
    }
  }
  for (const m of W.barges) {
    const [sx, sy] = project(m.p[0], m.p[1], m.z);
    if (!inRect(sx, sy, rect)) continue;
    const [name, L, B] = BOAT[m.kind];
    out.push({ sx, sy, h: 3, draw: () => { if (!sprite(name, m.p[0], m.p[1], m.z, m.a, `m:${m.name}:${sx}`, { scale: m.scale })) { shadow(m.p[0], m.p[1], m.z, m.a, L * m.scale, B); box(m.p[0], m.p[1], m.z, m.a, L * m.scale, B, 1.2, "#f4f1ea", "#2c3440", OUTLINE); } },
               hit: { r: 14, open: () => {} }, tip: () => [m.name || "Péniche", "péniche à quai"] });
  }
  for (const m of W.moored) {
    const [sx, sy] = project(m.p[0], m.p[1], m.z);
    if (!inRect(sx, sy, rect)) continue;
    const [n, what] = SHIP_INFO[m.sprite];
    out.push({ sx, sy, h: m.ship === "belem" ? 34 : 25, draw: () => { sprite(m.sprite, m.p[0], m.p[1], m.z, 0, null); },
               hit: { r: 30, open: () => {} }, tip: () => [n, what] });
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
// Lights of a vehicle in world coordinates, for fx.js at night: [x, y, z, type] with type
// 1 headlamp (white), 3 tail lamp (red), 5 headlight beam on the road ahead, 4 / 7 blue flashers (two phases).
// Lamps on the side turned away from the camera are left out (they would glow through the body).
const LAMP_HEAD = 1, LAMP_TAIL = 3, LAMP_BEAM = 5, LAMP_BLUE = 4, LAMP_BLUE2 = 7;
function vlamps(x, y, z, ang, L, W, h, opt = {}) {
  const c = Math.cos(ang), s = Math.sin(ang), back = TN.man.axes[2];
  const face = c * back[0] + s * back[1];                         // > 0: the front is turned towards us
  const at = (u, v, zz, type) => [x + u * c - v * s, y + u * s + v * c, z + zz, type];
  const out = [];
  const pair = W > 0.5 ? [-W * 0.36, W * 0.36] : [0];
  if (face > -0.2) for (const v of pair) out.push(at(L / 2, v, h, LAMP_HEAD));
  if (face < 0.2 && !opt.noTail) for (const v of pair) out.push(at(-L / 2, v, h, LAMP_TAIL));
  if (!opt.noBeam) out.push(at(L / 2 + (W > 0.5 ? 6 : 3), 0, 0.1, LAMP_BEAM));
  if (opt.siren) {
    const on = Math.floor(performance.now() / 180) % 2;
    out.push(at(opt.bar ?? 0, -0.45, opt.barZ ?? 1.7, on ? LAMP_BLUE : LAMP_BLUE2), at(opt.bar ?? 0, 0.45, opt.barZ ?? 1.7, on ? LAMP_BLUE2 : LAMP_BLUE));
  }
  return out;
}
function inhabitantItems(t, rect) {
  const out = [];
  if (TN.view.s < 0.9 && !TN.playing) return out;
  const now = performance.now() / 1000;
  for (const [id, k, x, y, z, a, v, c] of inhabitants()) {
    const [sx, sy] = project(x, y, z);
    if (!inRect(sx, sy, rect)) continue;
    const hit = { r: 10, open: () => live.open?.("agent", { id }) };
    if (k === 0) {
      const [name, L, W] = TRAFFIC[c % TRAFFIC.length];
      out.push({ sx, sy, h: 1.5, lamps: vlamps(x, y, z, a, L, W - 0.1, 0.65), hit, draw: () => {
        shadow(x, y, z, a, L + 0.1, W);
        if (!sprite(name, x, y, z, a, id)) carModel(x, y, z, a, CAR_COLORS[c % CAR_COLORS.length]); } });
    } else if (k === 3 && (c & 7) >= 3) {
      // the tow truck (fourrière; a wreck on its bed on the way back) and the street cleaners
      const service = c & 7, loaded = (c & 16) !== 0;
      out.push({ sx, sy, h: 3, lamps: vlamps(x, y, z, a, 6.5, 2.2, 0.8), hit, draw: () => {
        shadow(x, y, z, a, 6.6, 2.3);
        if (service === 3) towTruck(x, y, z, a, loaded, now);
        else sweeper(x, y, z, a, now);
      }, tip: () => [service === 3 ? (loaded ? "Fourrière : remorque une épave" : "Fourrière") : "Propreté de la Ville : nettoyage", ""] });
    } else if (k === 5) {
      // a wreck: a burnt-out car (on fire the first minutes) or someone knocked down
      const person = (c & 16) !== 0, burning = (c & 32) !== 0, heli = (c & 64) !== 0;
      boomOnce(id, x, y, z, burning && !person);
      out.push({ sx, sy, h: person ? 0.3 : 1.5, hit, draw: () => {
        if (person) {
          blood(x, y, z, a, id);
          if (!sprite(`down_${c & 7}`, x, y, z, a, id)) downed(x, y, z, a, CLOTHES[(c & 7) % CLOTHES.length]);
          return;
        }
        shadow(x, y, z, a, 4.4, 1.9);
        ctx.save(); ctx.filter = "brightness(0.28) saturate(0.15) contrast(1.2)";
        const ok = heli ? sprite("heli_0", x, y, z, a, id) : sprite(TRAFFIC[(c & 15) % TRAFFIC.length][0], x, y, z, a, id);
        ctx.restore();
        if (!ok) box(x, y, z, a, 4.3, 1.8, 1.2, "#2a2a2a", "#1a1a1a", OUTLINE);
        smoke(x, y, z + 1.2, id, burning);
      }, tip: () => [person ? "Une personne à terre" : "Épave", person ? "l'ambulance arrive" : "en attente de la fourrière"] });
    } else if (k === 6) {
      decals.push([x, y, z, id]);                                   // a scorch mark, drawn on the ground first
    } else if (k === 3) {
      // emergency: variant = service (0 ambulance, 1 police, 2 fire) + 8 when the siren is on
      const service = c & 7, siren = (c & 8) !== 0;
      const [name, L, W, bar, barZ] = service === 1 ? ["ems_police", 4.5, 1.8, -0.2, 1.5]
        : service === 2 ? ["ems_fire", 6.4, 2.2, 2.0, 2.95] : id % 4 === 0 ? ["ems_samu", 4.5, 1.8, -0.2, 1.6] : ["ems_ambulance", 5.6, 1.95, 1.6, 2.7];
      out.push({ sx, sy, h: 2.5, lamps: vlamps(x, y, z, a, L, W - 0.1, 0.7, { siren, bar, barZ }), hit, draw: () => {
        shadow(x, y, z, a, L + 0.1, W);
        if (!sprite(name, x, y, z, a, id)) carModel(x, y, z, a, service === 2 ? "#c8261c" : "#f2f2ee"); } });
    }
    else if (k === 4 && (TN.view.s >= 0.9 || TN.playing)) {
      // in a demonstration: marchers, batucada drummers (they bounce), placards held high
      const role = c & 3, col = CLOTHES[(c >> 2) % CLOTHES.length];
      const walkF = Math.floor(now * 1.4 + id) % 2, bob = role === 1 ? Math.abs(Math.sin(now * 9 + id)) * 0.25 : 0;
      out.push({ sx, sy, h: 2, hit, draw: () => {
        if (!sprite(`walker_${(c >> 2) % 8}_${walkF}`, x, y, z + bob, a, id)) personModel(x, y, z, a, col, now * 4 + id, false);
        const fx = Math.cos(a), fy = Math.sin(a);
        if (role === 1) box(x + fx * 0.45, y + fy * 0.45, z + 0.9 + bob, a, 0.55, 0.55, 0.45, "#f2c14e", "#c0392b", OUTLINE);   // drum
        if (role === 2) {
          box(x, y, z + 1.4, a, 0.06, 0.06, 1.3, "#7a5a3a", "#7a5a3a");                                                     // stick
          box(x, y, z + 2.5, a + Math.PI / 2, 1.3, 0.06, 0.8, "#f6f3ea", ["#e74c3c", "#2e86de", "#27ae60", "#f39c12"][id % 4], OUTLINE); // placard
        }
      } });
    }
    else if (k === 2 && c === 8 && (TN.view.s >= 1.6 || TN.playing)) {
      // on a Naolib Vélo just taken at a station (a real rental seen in the open data)
      out.push({ sx, sy, h: 2, hit, draw: () => {
        if (!sprite("bike_3", x, y, z, a, id)) personModel(x, y, z, a, "#2f6db0", now * v * 5 + id, true);
        box(x + Math.cos(a) * 0.75, y + Math.sin(a) * 0.75, z + 0.75, a, 0.35, 0.4, 0.25, "#ffd400", "#9be15d", OUTLINE);   // basket
      } });
    }
    else if (TN.view.s >= 1.6 || TN.playing) {
      const col = CLOTHES[c % CLOTHES.length], phase = now * v * 5 + id;
      const name = k === 2 ? `bike_${c % 8}` : `walker_${c % 8}_${v > 0.2 ? Math.floor(now * v * 1.6 + id) % 2 : 0}`;
      out.push({ sx, sy, h: 2, hit, lamps: k === 2 ? vlamps(x, y, z, a, 1.7, 0, 0.9, { noBeam: true }) : null,
                 draw: () => { if (!sprite(name, x, y, z, a, id)) personModel(x, y, z, a, col, phase, k === 2); } });
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
      const spr = /TGV|OUIGO/.test(m.mode) ? (k === 0 ? "train_tgv_head" : "train_tgv") : "train_ter";
      const ang = m.from === atNantes ? p.ang : p.ang + Math.PI;       // the head leads
      out.push({ sx, sy, h: 4, draw: () => { if (!sprite(spr, p.x, p.y, p.z, ang, `t:${m.number}:${k}`)) { shadow(p.x, p.y, p.z, p.ang, 25, 2.9); coach(p.x, p.y, p.z, p.ang, 25, 2.9, 4.0, side, top); } },
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
      const spr = /^(AT[47]|DH8|D[HC]8|F50|SF3|B190|C|P|PA|DA|TB|DR)/.test(pl.kind || "") ? "plane_prop" : "plane_jet";
      if (!sprite(spr, x, y, z, ang, "pl:" + pl.hex)) {
        box(x, y, z, ang, 36, 4, 4, "#f4f4f2", "#c9ccd2", "rgba(20,24,28,.4)");
        box(x, y, z + 1.2, ang, 6, 34, 0.6, "#e8e9ec", "#b5b9c0");
      }
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
  // the CHU heliport (Hôtel-Dieu roof): E there = take the helicopter
  {
    const hp = [-421.1, -751.1];
    const [sx, sy] = project(hp[0], hp[1], zAt(hp[0], hp[1]) + 30);
    if (inRect(sx, sy, rect) && TN.view.s > 0.4) out.push({ sx, sy, h: 0, overlay: true, draw: () => pin(sx, sy, "🚁"),
      tip: () => ["Héliport du CHU", TN.playing ? "E ici pour prendre l'hélico (ou touche 4)" : "🚶 puis touche 4 pour voler"], hit: { r: 16, dy: -18, open: () => {} } });
  }
  const ap = world.airport;
  if (ap) {
    const [sx, sy] = project(ap[0], ap[1], zAt(ap[0], ap[1]));
    if (inRect(sx, sy, rect)) out.push({ sx, sy, h: 0, overlay: true, draw: () => pin(sx, sy, "✈"),
      hit: { r: 16, dy: -18, open: () => live.open?.("airport", {}) }, tip: () => ["Aéroport Nantes Atlantique", "décollages et atterrissages"] });
  }
  if (TN.view.s >= 0.35 && TN.layer?.("events") !== false) for (const e of events) {
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
// ---------------------------------------------------------------- crashes: explosions, smoke, wrecks, marks
let decals = [];                       // scorch marks this frame: [x, y, z, id]
const booms = [], boomed = new Set();
// a wreck seen burning for the first time: the explosion (once per wreck, only if it's fresh)
function boomOnce(id, x, y, z, fresh) {
  if (boomed.has(id)) return;
  boomed.add(id);
  if (fresh && snapsFresh()) booms.push({ x, y, z, t0: performance.now() });
}
live.boom = (x, y, z) => booms.push({ x, y, z, t0: performance.now() });
const snapsFresh = () => performance.now() - (live.worldSince || 0) > 3000;   // not on page load
function drawBooms() {
  const now = performance.now();
  for (let k = booms.length - 1; k >= 0; k--) {
    const b = booms[k], f = (now - b.t0) / 1400;
    if (f >= 1) { booms.splice(k, 1); continue; }
    const [cx, cy] = toCanvas(...project(b.x, b.y, b.z + 1)), s = TN.view.s * TN.dpr;
    const r = (2 + 9 * Math.sqrt(f)) * s;
    const g = ctx.createRadialGradient(cx, cy, 0, cx, cy, r);
    g.addColorStop(0, `rgba(255,250,200,${1 - f})`); g.addColorStop(0.35, `rgba(255,170,40,${0.9 * (1 - f)})`);
    g.addColorStop(0.7, `rgba(200,60,20,${0.6 * (1 - f)})`); g.addColorStop(1, "rgba(60,40,30,0)");
    ctx.fillStyle = g; ctx.beginPath(); ctx.arc(cx, cy, r, 0, 7); ctx.fill();
    ctx.fillStyle = `rgba(40,30,25,${0.8 * (1 - f)})`;                 // debris
    for (let i = 0; i < 10; i++) {
      const an = i * 2.4 + b.x, d = (1 + i % 3) * 5 * f * s;
      ctx.fillRect(cx + Math.cos(an) * d, cy + Math.sin(an) * d * 0.6 - 6 * f * s * (1 - f) * 4, 2 * TN.dpr, 2 * TN.dpr);
    }
  }
}
function drawDecals() {
  for (const [x, y, z, id] of decals) {
    const pts = Array.from({ length: 14 }, (_, k) => {
      const an = k / 14 * Math.PI * 2, r = 3.2 + 0.9 * Math.sin(k * 2.1 + id);
      return toCanvas(...project(x + Math.cos(an) * r, y + Math.sin(an) * r, z + 0.05));
    });
    const [cx, cy] = toCanvas(...project(x, y, z));
    const g = ctx.createRadialGradient(cx, cy, 0, cx, cy, 3.5 * TN.view.s * TN.dpr * 1.2);
    g.addColorStop(0, "rgba(10,8,6,.85)"); g.addColorStop(0.6, "rgba(25,20,15,.6)"); g.addColorStop(1, "rgba(40,35,30,0)");
    ctx.fillStyle = g; ctx.beginPath(); pts.forEach((p, k) => (k ? ctx.lineTo(...p) : ctx.moveTo(...p))); ctx.closePath(); ctx.fill();
  }
}
function smoke(x, y, z, id, burning) {
  const t = performance.now() / 1000, s = TN.view.s * TN.dpr;
  if (burning) {                                                    // flames
    const [cx, cy] = toCanvas(...project(x, y, z));
    for (let i = 0; i < 5; i++) {
      const fl = Math.sin(t * 13 + i * 1.7 + id) * 0.5 + 0.5;
      ctx.fillStyle = i % 2 ? `rgba(255,120,20,${0.6 + 0.3 * fl})` : `rgba(255,210,60,${0.5 + 0.4 * fl})`;
      ctx.beginPath(); ctx.ellipse(cx + (i - 2) * 0.5 * s, cy - (0.6 + fl) * s, 0.45 * s, (0.8 + fl * 0.9) * s, 0, 0, 7); ctx.fill();
    }
  }
  for (let i = 0; i < (burning ? 9 : 4); i++) {                     // a column of smoke drifting away
    const f = ((t * 0.25 + i / 9 + (id % 7) / 7) % 1);
    const [cx, cy] = toCanvas(...project(x + f * 4, y + f * 2, z + f * (burning ? 18 : 9)));
    ctx.fillStyle = `rgba(${burning ? 40 : 90},${burning ? 38 : 88},${burning ? 36 : 86},${(burning ? 0.5 : 0.3) * (1 - f)})`;
    ctx.beginPath(); ctx.arc(cx, cy, (0.8 + f * 3) * s, 0, 7); ctx.fill();
  }
}
// a pool of blood under someone knocked down: spreads for ~20 s from when we first saw it, then stays; a few
// drops around. Dark and flat, no splatter: an accident, not a horror film.
const bloodSeen = new Map();
function blood(x, y, z, a, id) {
  const t0 = bloodSeen.get(id) ?? (bloodSeen.set(id, performance.now() - (snapsFresh() ? 0 : 60000)), bloodSeen.get(id));
  if (bloodSeen.size > 300) bloodSeen.delete(bloodSeen.keys().next().value);
  const g = Math.min(1, (performance.now() - t0) / 20000), R = 0.3 + 0.7 * Math.sqrt(g);
  const c = Math.cos(a), s = Math.sin(a), cx = x + c * 0.5, cy = y + s * 0.5;     // under the chest
  const ring = (r, wob) => Array.from({ length: 16 }, (_, k) => {
    const an = k / 16 * Math.PI * 2, rr = r * (1 + wob * Math.sin(k * 2.7 + id) + 0.08 * Math.sin(k * 5.1 + id * 3));
    return toCanvas(...project(cx + Math.cos(an) * rr * 1.25 * c - Math.sin(an) * rr * s, cy + Math.cos(an) * rr * 1.25 * s + Math.sin(an) * rr * c, z + 0.03));
  });
  const fill = (pts, col) => { ctx.fillStyle = col; ctx.beginPath(); pts.forEach((p, k) => (k ? ctx.lineTo(...p) : ctx.moveTo(...p))); ctx.closePath(); ctx.fill(); };
  fill(ring(R, 0.18), "rgba(92,10,12,.88)");
  fill(ring(R * 0.7, 0.12), "rgba(128,16,18,.9)");
  const [hx, hy] = toCanvas(...project(cx - 0.15, cy - 0.1, z + 0.03)), u = TN.view.s * TN.dpr;
  ctx.fillStyle = "rgba(255,190,190,.18)"; ctx.beginPath(); ctx.ellipse(hx, hy, R * 0.35 * u, R * 0.15 * u, 0, 0, 7); ctx.fill();   // sheen
  if (g > 0.2) for (let k = 0; k < 4; k++) {                          // a few drops
    const an = id * 1.3 + k * 1.9, d = R + 0.4 + (k % 2) * 0.5;
    const [px, py] = toCanvas(...project(cx + Math.cos(an) * d, cy + Math.sin(an) * d, z + 0.03));
    ctx.fillStyle = "rgba(100,12,14,.85)"; ctx.fillRect(Math.round(px), Math.round(py), Math.max(1, Math.round(0.18 * u)), Math.max(1, Math.round(0.14 * u)));
  }
}
live.towTruck = (...a) => towTruck(...a);
live.sweeper = (...a) => sweeper(...a);
function downed(x, y, z, a, col) {                                  // someone lying on the road
  box(x, y, z, a, 1.7, 0.5, 0.25, col, shade(col, 0.8), OUTLINE);
  box(x + Math.cos(a) * 0.95, y + Math.sin(a) * 0.95, z, a, 0.3, 0.3, 0.25, "#e0b48a", "#c9986c", OUTLINE);
}
function towTruck(x, y, z, a, loaded, now) {
  if (sprite(loaded ? "ems_tow_loaded" : "ems_tow", x, y, z, a, null)) return;
  const c = Math.cos(a), s = Math.sin(a);
  box(x + c * 2.2, y + s * 2.2, z + 0.3, a, 2.0, 2.2, 2.2, "#f2a900", "#c98a00", OUTLINE);                   // cab
  box(x - c * 1.0, y - s * 1.0, z + 0.3, a, 4.4, 2.3, 0.5, "#555b61", "#3e4348", OUTLINE);                  // flat bed
  if (loaded) box(x - c * 1.0, y - s * 1.0, z + 0.8, a, 4.1, 1.8, 1.2, "#2a2a2a", "#1a1a1a", OUTLINE);      // the wreck
  const on = Math.floor(now * 3) % 2;
  box(x + c * 2.2, y + s * 2.2, z + 2.5, a, 0.3, 1.2, 0.2, on ? "#ffb000" : "#7a5500", "#7a5500");          // beacon
}
function sweeper(x, y, z, a, now) {
  const c = Math.cos(a), s = Math.sin(a);
  if (sprite("ems_sweeper", x, y, z, a, null)) {
    const sp = now * 12;                                            // the side brushes spin
    for (const side of [-1, 1]) box(x + c * 2.75 - s * side * 0.95, y + s * 2.75 + c * side * 0.95, z, sp + side, 0.6, 0.6, 0.12, "#d9c27a", "#a08a40");
    return;
  }
  box(x + c * 1.6, y + s * 1.6, z + 0.3, a, 2.0, 2.0, 2.0, "#f4f4f0", "#9fd36b", OUTLINE);                  // cab
  box(x - c * 1.2, y - s * 1.2, z + 0.3, a, 3.2, 2.1, 1.9, "#7bbf3a", "#5c9a26", OUTLINE);                  // tank
  const sp = now * 12;                                                                                         // brushes
  for (const side of [-1, 1]) box(x + c * 2.6 - s * side * 1.0, y + s * 2.6 + c * side * 1.0, z, sp + side, 0.7, 0.7, 0.15, "#d9c27a", "#a08a40");
  const on = Math.floor(now * 3) % 2;
  box(x + c * 1.6, y + s * 1.6, z + 2.4, a, 0.3, 1.0, 0.2, on ? "#ffb000" : "#7a5500", "#7a5500");
}

// Hard things near (x, y) for the player's physics: sim vehicles and wrecks, people (soft), trams and buses
// (they keep to their real track: immovable). [{x, y, ang, L, W, kind, id, soft}]
live.solids = (x, y, r = 30) => {
  const out = [], t = live.now();
  for (const [id, k, px, py, , a, sp, c] of inhabitants()) {
    if (Math.abs(px - x) > r || Math.abs(py - y) > r) continue;
    const vx = Math.cos(a) * sp, vy = Math.sin(a) * sp;
    if (k === 0) { const T = TRAFFIC[c % TRAFFIC.length]; out.push({ x: px, y: py, ang: a, L: T[1], W: T[2], kind: "car", id, vx, vy }); }
    else if (k === 3) out.push({ x: px, y: py, ang: a, L: (c & 7) === 2 ? 6.4 : 5.4, W: 2.1, kind: "service", id, vx, vy });
    else if (k === 5) { if (c & 16) out.push({ x: px, y: py, ang: a, L: 1.7, W: 0.6, kind: "down", id, soft: true }); else out.push({ x: px, y: py, ang: a, L: 4.3, W: 1.8, kind: "wreck", id }); }
    else if (k === 2) out.push({ x: px, y: py, ang: a, L: 1.7, W: 0.6, kind: "bike", id, soft: true });
    else if (k === 1 || k === 4) out.push({ x: px, y: py, ang: a, L: 0.5, W: 0.5, kind: "person", id, soft: true });
  }
  for (const v of vehicles) {
    const rt = world.routes[v.route] || { type: 3 };
    if (rt.type === 4) continue;                                    // on the river
    const p = vehiclePose(v, t);
    if (Math.abs(p.x - x) > r + 40 || Math.abs(p.y - y) > r + 40) continue;
    if (rt.type === 0) {
      const sh = world.shapes[v.shape];
      for (let k = 0; k < 4; k++) { const q = chord(sh, p.d - k * TRAM_SEG - 0.6, TRAM_SEG - 1.2); out.push({ x: q.x, y: q.y, ang: q.ang, L: TRAM_SEG, W: 2.65, kind: "tram" }); }
    } else { const b = chord(world.shapes[v.shape], p.d + 6, 12); out.push({ x: b.x, y: b.y, ang: b.ang, L: 12, W: 2.55, kind: "bus" }); }
  }
  return out;
};

function pin(sx, sy, icon) {
  const [cx, cy] = toCanvas(sx, sy);
  ctx.fillStyle = "#1f2428"; ctx.beginPath(); ctx.arc(cx, cy - 18 * TN.dpr, 13 * TN.dpr, 0, 7); ctx.fill();
  ctx.strokeStyle = "#f2b544"; ctx.lineWidth = 2 * TN.dpr; ctx.stroke();
  ctx.font = `${15 * TN.dpr}px system-ui, "Noto Color Emoji", sans-serif`; ctx.textAlign = "center"; ctx.textBaseline = "middle";
  ctx.fillText(icon, cx, cy - 17 * TN.dpr); ctx.textAlign = "start";
}
const inRect = (sx, sy, r) => sx >= r[0] && sx <= r[2] && sy >= r[1] && sy <= r[3];

// ---------------------------------------------------------------- frame
let lastDraw = 0;
function frame(now) {
  requestAnimationFrame(frame);
  if (!world.ready || !TN.man) return;
  if (TN.lite && now - lastDraw < 31) return;               // light mode: 30 fps
  lastDraw = now;
  const w = Math.round(innerWidth * TN.dpr), h = Math.round(innerHeight * TN.dpr);
  if (cv.width !== w || cv.height !== h) { cv.width = w; cv.height = h; }
  ctx.clearRect(0, 0, w, h);
  hits = [];
  decals = [];
  const on = document.getElementById("liveBtn")?.getAttribute("aria-pressed") === "true";
  const t = live.now(), rect = visibleRect();
  let items = [];
  if (on) {
    const L = (k) => TN.layer?.(k) ?? true;                        // the layers panel (app.js)
    if (L("planes")) items.push(...planeItems(t, visibleRect(4000)));
    if (TN.view.s >= 0.6 || TN.playing) {
      if (L("transit")) items.push(...transitItems(t, rect));
      if (L("trains")) items.push(...trainItems(t, rect));
      if (L("people")) items.push(...inhabitantItems(t, rect));
    }
    items.push(...placeItems(t, rect));
    if (L("pois")) items.push(...poiItems(t, rect));
    if (L("bikes") && (TN.view.s >= 0.8 || TN.playing)) items.push(...bikeStationItems(t, rect));
    if (L("elephant") && (TN.view.s >= 0.3 || TN.playing)) items.push(...elephantItems(t, visibleRect(60)));
    if (L("boats") && (TN.view.s >= 0.3 || TN.playing)) items.push(...boatItems(t, visibleRect(300)));
  }
  for (const f of live.layers) items.push(...f(t, rect));
  drawDecals();                                                    // scorch marks lie under everything
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
    if (it.sy && !it.room && !it.tall && occluded(it.sx, it.sy - (it.h || 2) * COS45 * 0.5, it.sy)) continue;
    it.draw(); consider(it);
    if (it.lamps) for (const l of it.lamps) lights.push([...toCanvas(...project(l[0], l[1], l[2])), l[3]]);
  }
  live.lights = lights;
  drawBooms();
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
  pollBikes(); setInterval(pollBikes, 30_000);
  pollMenus(); setInterval(pollMenus, 900_000);
  requestAnimationFrame(frame);
}).catch((e) => console.warn("live layer off:", e));
