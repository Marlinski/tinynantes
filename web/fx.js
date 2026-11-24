// Time of day and weather, as effects over the (unchanged) tiles:
// - light: the real sun altitude over Nantes drives a colour grade (night blue, golden hour, dawn) applied with a
//   multiply layer (#tint) and a brightness/saturation filter on the map and the live layer;
// - at night, light sources on a screen-blended layer (#fx): street lamps along the main roads, vehicle lights,
//   lit windows of the bars and restaurants;
// - weather from our server (/api/weather, Open-Meteo): cloud shadows drifting with the wind, rain streaks slanted
//   by the wind, snow, fog, lightning.
// Preview any state with ?fx=night | dusk | day, plus rain | snow | fog | storm | clear, e.g. ?fx=night,rain

import { live } from "./live.js";

const TN = window.TN;
const tint = document.getElementById("tint"), tg = tint.getContext("2d");
const fx = document.getElementById("fx"), fg = fx.getContext("2d");
const LAT = 47.218, LON = -1.553;

// Debug overrides of time and weather: ?fx=night,rain in the URL, or the panel (F9 or ?debug).
// TN.debug = { hour: null | 0..24 (local time of today), wx: null | "clear" | "cloudy" | "rain" | "storm" | "snow" | "fog" }
const force = new Set((new URLSearchParams(location.search).get("fx") || "").split(",").filter(Boolean));
TN.debug = { hour: null, wx: null };
function clock() {                                  // the clock the effects follow
  if (TN.debug.hour == null) return new Date();
  const d = new Date(); d.setHours(Math.floor(TN.debug.hour), Math.round(TN.debug.hour % 1 * 60), 0, 0);
  return d;
}
let wx = null;                                  // Open-Meteo "current" block

// ---------------------------------------------------------------- sun
function sunAltitude(date) {
  const rad = Math.PI / 180, d = date / 86400000 - 10957.5;               // days since J2000
  const g = (357.529 + 0.98560028 * d) * rad, q = 280.459 + 0.98564736 * d;
  const L = (q + 1.915 * Math.sin(g) + 0.020 * Math.sin(2 * g)) * rad, e = (23.439 - 0.00000036 * d) * rad;
  const ra = Math.atan2(Math.cos(e) * Math.sin(L), Math.cos(L)), dec = Math.asin(Math.sin(e) * Math.sin(L));
  const gmst = (18.697374558 + 24.06570982441908 * d) % 24;
  const ha = ((gmst * 15 + LON) * rad) - ra;
  return Math.asin(Math.sin(LAT * rad) * Math.sin(dec) + Math.cos(LAT * rad) * Math.cos(dec) * Math.cos(ha)) / rad;
}
const smooth = (a, b, x) => { const t = Math.min(1, Math.max(0, (x - a) / (b - a))); return t * t * (3 - 2 * t); };
const mix = (a, b, t) => a.map((v, k) => v + (b[k] - v) * t);

// ---------------------------------------------------------------- weather state
async function pollWeather() {
  try { const r = await fetch("api/weather"); if (r.ok) wx = (await r.json()).items.current; } catch { /* keep */ }
}
function sky() {
  const code = wx?.weather_code ?? 0;
  let rain = wx ? Math.max(wx.rain || 0, (wx.precipitation || 0) - (wx.snowfall || 0)) : 0;
  let snow = wx?.snowfall || 0, cloud = (wx?.cloud_cover ?? 20) / 100;
  let fog = code === 45 || code === 48 || (wx?.visibility ?? 1e5) < 1500;
  let storm = code >= 95;
  if ((code >= 51 && code <= 67) || (code >= 80 && code <= 82)) rain = Math.max(rain, code % 10 >= 5 || code >= 63 ? 2.5 : 0.8);
  if ((code >= 71 && code <= 77) || code === 85 || code === 86) snow = Math.max(snow, 0.5);
  const dw = TN.debug.wx;
  if (dw) { rain = snow = 0; fog = storm = false; cloud = { clear: 0, cloudy: 0.9 }[dw] ?? 1; }
  if (dw === "rain") rain = 3;
  if (dw === "storm") { rain = 6; storm = true; }
  if (dw === "snow") snow = 1;
  if (dw === "fog") fog = true;
  if (force.has("rain")) { rain = 3; cloud = 1; }
  if (force.has("storm")) { rain = 6; cloud = 1; storm = true; }
  if (force.has("snow")) { snow = 1; rain = 0; cloud = 1; }
  if (force.has("fog")) { fog = true; cloud = 1; }
  if (force.has("clear")) { rain = snow = 0; cloud = 0; fog = storm = false; }
  const wind = (wx?.wind_speed_10m ?? 8) / 3.6, wdir = wx?.wind_direction_10m ?? 250;       // m/s, from (deg)
  return { rain, snow, cloud, fog, storm, wind, wdir, temp: wx?.temperature_2m, code };
}

// ---------------------------------------------------------------- grade
function grade(alt, s) {
  if (TN.debug.hour == null) { if (force.has("night")) alt = -20; else if (force.has("dusk")) alt = 1; else if (force.has("day")) alt = 40; }
  const day = smooth(-7, 6, alt);                       // 0 night .. 1 day
  const golden = day > 0 ? (1 - smooth(4, 14, alt)) * smooth(-5, 2, alt) : 0;
  let col = mix([62, 82, 150], [255, 255, 255], day);   // multiply colour
  col = mix(col, [255, 206, 160], golden * 0.7);
  const grey = Math.min(1, s.cloud * 0.6 + (s.rain > 0 ? 0.4 : 0) + (s.fog ? 0.3 : 0));
  col = mix(col, mix([200, 206, 216], col, 1 - day), grey * 0.55 * day);
  const bright = (0.58 + 0.42 * day) * (1 - 0.1 * grey * day);
  const sat = (0.62 + 0.38 * day) * (1 - 0.25 * grey);
  return { day, golden, col, bright, sat, night: 1 - day };
}

// ---------------------------------------------------------------- light sources
let lamps = null;                                     // world points along the measured main roads
// Street lamps on the pavement of the measured traffic sections, every 26 m, alternating sides, 4.5 m from the
// centre line (7 m put them in front gardens and houses on narrow streets).
function buildLamps() {
  const secs = live.sections;
  if (!secs.length) return;
  lamps = [];
  let side = 1;
  for (const s of secs) {
    let acc = 0;
    for (let k = 1; k < s.p.length; k++) {
      const [ax, ay] = s.p[k - 1], [bx, by] = s.p[k], L = Math.hypot(bx - ax, by - ay);
      for (; acc < L; acc += 26) {
        const f = acc / L, x = ax + (bx - ax) * f + side * (by - ay) / L * 4.5, y = ay + (by - ay) * f - side * (bx - ax) / L * 4.5;
        lamps.push({ x, y, z: live.zAt(x, y), vis: undefined });
        side = -side;
      }
      acc -= L;
    }
  }
}
// Whether a lamp shows: decided once, when the height maps under it have loaded, then kept, so lamps never
// blink while the map moves. Hidden when its foot is on a roof or a tree (it would glow out of a house) or a
// building stands in front of its head.
function lampVisible(l) {
  if (l.vis !== undefined) return l.vis;
  const [fx_, fy] = live.project(l.x, l.y, l.z), [hx, hy] = live.project(l.x, l.y, l.z + 4);
  const foot = live.heightAt(fx_, fy), head = live.heightAt(hx, hy);
  if (foot === null || head === null) return false;
  l.vis = foot < 1.5 && !live.occluded(hx, hy, fy);
  return l.vis;
}
// Lights are drawn on a coarse grid (one cell per few map pixels) with stepped intensity, so they read as
// pixel art like the map under them rather than as smooth airbrush.
const lowCv = document.createElement("canvas"), low = lowCv.getContext("2d");
const sprites = {};
function glowSprite(rgb) {
  const k = rgb.join();
  if (sprites[k]) return sprites[k];
  const c = document.createElement("canvas"); c.width = c.height = 64;
  const g = c.getContext("2d"), gr = g.createRadialGradient(32, 32, 0, 32, 32, 32);
  gr.addColorStop(0, `rgba(${rgb},1)`); gr.addColorStop(0.25, `rgba(${rgb},.55)`); gr.addColorStop(1, `rgba(${rgb},0)`);
  g.fillStyle = gr; g.fillRect(0, 0, 64, 64);
  return (sprites[k] = c);
}
const LAMP = [255, 184, 108], HEAD = [255, 244, 225], WINDOW = [255, 170, 90];
const TAIL = [255, 36, 28], BEAM = [255, 236, 200], BLUE = [40, 110, 255];
const NIGHTLIFE = new Set(["amenity=restaurant", "amenity=bar", "amenity=pub", "amenity=cafe", "amenity=fast_food",
  "amenity=nightclub", "amenity=cinema", "amenity=theatre", "amenity=biergarten"]);
function glow(img, x, y, r, a) {
  if (a <= 0.01) return;
  fg.globalAlpha = Math.min(1, a);
  fg.drawImage(img, x - r, y - r, 2 * r, 2 * r);
}
function drawLights(G) {
  const k = G.night;
  if (k < 0.05) return;
  const s = TN.view.s * TN.dpr, W = fx.width, H = fx.height;
  if (!lamps) buildLamps();
  const hour = clock().getHours();
  const inView = (x, y, m) => x > -m && y > -m && x < W + m && y < H + m;
  // street lamps: a pool of light on the street and a bright head, in stepped bands on the coarse grid
  if (lamps && TN.view.s > 0.25) {
    const px = Math.max(2, Math.round(s * 0.293 * 2));             // one grid cell = 2 map pixels
    const w = Math.ceil(W / px), h = Math.ceil(H / px);
    if (lowCv.width !== w || lowCv.height !== h) { lowCv.width = w; lowCv.height = h; } else low.clearRect(0, 0, w, h);
    const R = 7 * s / px;                                           // pool radius (7 m) in grid cells
    const bands = [[1, 0.16], [0.68, 0.16], [0.38, 0.2]];           // radius share, added alpha
    let n = 0;
    for (const l of lamps) {
      const [sx, sy] = live.project(l.x, l.y, l.z);
      const [cx, cy] = live.toCanvas(sx, sy);
      if (!inView(cx, cy, 7 * s) || !lampVisible(l)) continue;
      const gx = cx / px, gy = cy / px;
      for (const [f, a] of bands) {
        low.globalAlpha = a * k; low.fillStyle = "rgb(255,176,92)";
        low.beginPath(); low.ellipse(Math.round(gx), Math.round(gy), R * f, R * f * 0.62, 0, 0, 7); low.fill();
      }
      const [hx, hy] = live.toCanvas(...live.project(l.x, l.y, l.z + 4));
      low.globalAlpha = 0.9 * k; low.fillStyle = "rgb(255,236,190)";
      low.fillRect(Math.round(hx / px) - 0.5, Math.round(hy / px) - 0.5, 1, 1);
      n++;
    }
    if (n) {
      fg.globalAlpha = 1; fg.imageSmoothingEnabled = false;
      fg.drawImage(lowCv, 0, 0, w * px, h * px);
      fg.imageSmoothingEnabled = true;
    }
  }
  // lit bars and restaurants in the evening
  if (TN.view.s > 0.5 && (hour >= 17 || hour < 2)) {
    const img = glowSprite(WINDOW), r = Math.max(3, 8 * s);
    for (const p of live.world.pois) {
      if (!NIGHTLIFE.has(p.k)) continue;
      const [x, y, z] = p.p;
      const [sx, sy] = live.project(x, y, z + 2);
      const [cx, cy] = live.toCanvas(sx, sy);
      if (inView(cx, cy, r) && !live.occluded(sx, sy, live.project(x, y, z)[1])) glow(img, cx, cy, r, 0.7 * k);
    }
  }
  // vehicles: headlamps, tail lamps and the pool of the headlights on the road (siren flashers: drawLights below)
  for (const [cx, cy, type] of live.lights) {
    if (type === 1) glow(glowSprite(HEAD), cx, cy, Math.max(2, 2.6 * s), 0.85 * k);
    else if (type === 3) glow(glowSprite(TAIL), cx, cy, Math.max(2, 2.2 * s), 0.75 * k);
    else if (type === 5) glow(glowSprite(BEAM), cx, cy, Math.max(4, 6.5 * s), 0.32 * k);
  }
  fg.globalAlpha = 1;
}
// Blue flashers of emergency vehicles: by day too (softer), two phases alternating.
function drawFlashers(G) {
  const s = TN.view.s * TN.dpr, a = 0.55 + 0.4 * G.night;
  for (const [cx, cy, type] of live.lights) {
    if (type === 4) glow(glowSprite(BLUE), cx, cy, Math.max(4, 5 * s), a);
    else if (type === 7) glow(glowSprite(BLUE), cx, cy, Math.max(2, 1.6 * s), a * 0.5);
  }
  fg.globalAlpha = 1;
}

// ---------------------------------------------------------------- weather particles
const drops = [];
let flash = 0, nextFlash = 0, cloudT = 0, clouds = null;
function makeClouds() {
  // a soft, tileable field of blobs (in screen metres), drifted with the wind
  clouds = [];
  for (let k = 0; k < 26; k++) clouds.push({ x: Math.random() * 2400, y: Math.random() * 2400, r: 120 + Math.random() * 260 });
}
function drawClouds(G, S, dt) {
  if (S.cloud < 0.15 || G.day < 0.2 || TN.view.s < 0.6) return;
  if (!clouds) makeClouds();
  const v = TN.view, s = v.s * TN.dpr, P = 2400;
  const a = (S.wdir + 180) * Math.PI / 180;                        // blowing towards
  cloudT += dt;
  const ox = Math.sin(a) * S.wind * cloudT, oy = -Math.cos(a) * S.wind * cloudT * COSY;
  tg.fillStyle = "rgba(60,70,85,1)";
  const left = v.x - innerWidth / 2 / v.s, top = v.y - innerHeight / 2 / v.s;
  tg.globalAlpha = 0.16 * Math.min(1, S.cloud) * G.day;
  for (const c of clouds) {
    // wrap each blob into one period starting just left/above the view (the period is wider than the view)
    const px = ((c.x + ox - left) % P + P) % P + left - 400, py = ((c.y + oy - top) % P + P) % P + top - 400;
    const cx = (px - v.x) * s + tint.width / 2, cy = (py - v.y) * s + tint.height / 2, r = c.r * s;
    if (cx < -r || cy < -r || cx > tint.width + r || cy > tint.height + r) continue;
    const gr = tg.createRadialGradient(cx, cy, 0, cx, cy, r);
    gr.addColorStop(0, "rgba(60,70,85,1)"); gr.addColorStop(1, "rgba(60,70,85,0)");
    tg.fillStyle = gr; tg.beginPath(); tg.arc(cx, cy, r, 0, 7); tg.fill();
  }
  tg.globalAlpha = 1;
}
const COSY = Math.SQRT1_2;                                        // ground y is foreshortened on screen
function drawPrecip(S, dt) {
  const W = fx.width, H = fx.height, d = TN.dpr;
  const want = S.snow > 0 ? Math.min(700, 250 + S.snow * 300) : S.rain > 0 ? Math.min(900, 120 + S.rain * 160) : 0;
  while (drops.length < want) drops.push({ x: Math.random() * W, y: Math.random() * H, v: 0.7 + Math.random() * 0.6 });
  drops.length = Math.min(drops.length, want);
  if (!want) return;
  const a = (S.wdir + 180) * Math.PI / 180, slant = Math.sin(a) * Math.min(1.2, S.wind / 8);
  if (S.snow > 0) {
    fg.fillStyle = "rgba(255,255,255,.9)";
    for (const p of drops) {
      p.y += 60 * p.v * dt * d; p.x += (slant * 40 + Math.sin((p.y + p.x) / 40) * 12) * dt * d;
      if (p.y > H) { p.y = -4; p.x = Math.random() * W; } if (p.x > W) p.x -= W; if (p.x < 0) p.x += W;
      fg.fillRect(p.x, p.y, 2 * d * p.v, 2 * d * p.v);
    }
  } else {
    fg.strokeStyle = "rgba(190,210,235,.5)"; fg.lineWidth = d; fg.beginPath();
    const len = 16 * d;
    for (const p of drops) {
      p.y += 900 * p.v * dt * d; p.x += slant * 900 * p.v * dt * d * 0.35;
      if (p.y > H) { p.y = -len; p.x = Math.random() * W; } if (p.x > W) p.x -= W; if (p.x < 0) p.x += W;
      fg.moveTo(p.x, p.y); fg.lineTo(p.x - slant * len * 0.35, p.y - len * p.v);
    }
    fg.stroke();
  }
}

// ---------------------------------------------------------------- frame
const mapCv = document.getElementById("map"), liveCv = document.getElementById("live");
let last = performance.now(), lastFilter = "";
function frame() {
  requestAnimationFrame(frame);
  const now = performance.now();
  if (TN.lite && now - last < 95) return;                     // light mode: light and weather at 10 fps
  const dt = Math.min(0.1, (now - last) / 1000);
  last = now;
  const w = Math.round(innerWidth * TN.dpr), h = Math.round(innerHeight * TN.dpr);
  for (const c of [tint, fx]) if (c.width !== w || c.height !== h) { c.width = w; c.height = h; }
  const S = sky(), G = grade(sunAltitude(clock().getTime()), S);
  const f = `brightness(${G.bright.toFixed(3)}) saturate(${G.sat.toFixed(3)})`;
  if (f !== lastFilter) { mapCv.style.filter = liveCv.style.filter = f; lastFilter = f; }
  tg.globalCompositeOperation = "source-over";
  tg.fillStyle = `rgb(${G.col.map(Math.round)})`; tg.fillRect(0, 0, w, h);
  drawClouds(G, S, dt);
  fg.clearRect(0, 0, w, h);
  drawLights(G);
  drawFlashers(G);
  drawPrecip(S, dt);
  if (S.fog) { fg.fillStyle = `rgba(205,210,216,${0.22 + 0.1 * G.day})`; fg.fillRect(0, 0, w, h); }
  if (S.storm) {
    if (now > nextFlash) { flash = 1; nextFlash = now + 6000 + Math.random() * 14000; }
    if (flash > 0.01) { fg.fillStyle = `rgba(235,240,255,${0.55 * flash})`; fg.fillRect(0, 0, w, h); flash *= 0.86; }
  }
  badge(S);
}

// a small weather badge under the logo
const WMO = { 0: "☀️", 1: "🌤", 2: "⛅", 3: "☁️", 45: "🌫", 48: "🌫", 51: "🌦", 53: "🌦", 55: "🌧", 61: "🌧", 63: "🌧", 65: "🌧",
  71: "🌨", 73: "🌨", 75: "❄️", 80: "🌦", 81: "🌧", 82: "⛈", 95: "⛈", 96: "⛈", 99: "⛈" };
let lastBadge = "";
function badge(S) {
  const el = document.getElementById("wx");
  if (!el || !wx) return;
  const night = wx.is_day === 0;
  const icon = night && S.code <= 2 ? "🌙" : WMO[S.code] || "☁️";
  const t = `${icon} ${Math.round(S.temp)}°`;
  if (t !== lastBadge) {
    el.textContent = t; lastBadge = t;
    el.title = `Nantes maintenant : ${Math.round(S.temp)} °C, vent ${Math.round(S.wind * 3.6)} km/h${S.rain ? `, pluie ${S.rain} mm/h` : ""} (Open-Meteo)`;
    el.hidden = false;
  }
}

pollWeather(); setInterval(pollWeather, 600_000);
requestAnimationFrame(frame);

// ---------------------------------------------------------------- debug panel (F9, or ?debug in the URL)
function debugPanel() {
  let el = document.getElementById("dbg");
  if (el) { el.hidden = !el.hidden; return; }
  el = document.createElement("div"); el.id = "dbg"; el.className = "dbg";
  const WX = [["", "météo réelle"], ["clear", "ciel clair"], ["cloudy", "nuageux"], ["rain", "pluie"], ["storm", "orage"],
    ["snow", "neige"], ["fog", "brouillard"]];
  el.innerHTML = `<b>Debug</b> <button class="x" title="Fermer">×</button>
    <label><input type="checkbox" id="dbgReal" checked> heure réelle</label>
    <label>heure <input type="range" id="dbgHour" min="0" max="24" step="0.25" value="12" disabled> <span id="dbgH">—</span></label>
    <label>météo <select id="dbgWx">${WX.map(([v, t]) => `<option value="${v}">${t}</option>`).join("")}</select></label>
    <small id="dbgInfo"></small>`;
  document.body.append(el);
  const real = el.querySelector("#dbgReal"), hr = el.querySelector("#dbgHour"), hl = el.querySelector("#dbgH");
  const fmt = (h) => `${String(Math.floor(h)).padStart(2, "0")}:${String(Math.round(h % 1 * 60)).padStart(2, "0")}`;
  const sync = () => { hr.disabled = real.checked; TN.debug.hour = real.checked ? null : +hr.value; hl.textContent = real.checked ? "—" : fmt(+hr.value); };
  real.onchange = sync; hr.oninput = sync;
  el.querySelector("#dbgWx").onchange = (e) => { TN.debug.wx = e.target.value || null; };
  el.querySelector(".x").onclick = () => { el.hidden = true; };
  setInterval(() => {
    if (el.hidden) return;
    const S = sky(), alt = sunAltitude(clock().getTime());
    el.querySelector("#dbgInfo").textContent = `soleil ${alt.toFixed(1)}° · pluie ${S.rain} · neige ${S.snow} · nuages ${Math.round(S.cloud * 100)} %` +
      (S.fog ? " · brouillard" : "") + (S.storm ? " · orage" : "");
  }, 500);
}
addEventListener("keydown", (e) => {
  if (e.code === "F9") { e.preventDefault(); debugPanel(); }
});
if (new URLSearchParams(location.search).has("debug")) debugPanel();
