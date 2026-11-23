// The game layer: pick a name, then walk, run, ride a bike or drive a car through Nantes; hop on a tram, bus or
// Navibus at a stop and get off at another; walk into a restaurant to read its menu; see the other players and
// talk to them. Also the side panel that shows whatever is clicked on the map (vehicles, trains, planes, the
// station and airport boards, events, places).
//
// Movement uses data/world/nav/CX_CY.png (built by world.py): 1 m cells flagged building / water / road /
// footway, plus bridge deck heights. Speeds are realistic: walk 1.4 m/s, run 3.5, bike 5.5, car up to 14
// (50 km/h) in town.

import { live } from "./live.js";

const TN = window.TN;
const $ = (id) => document.getElementById(id);
const esc = (s) => String(s ?? "").replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));
const COS45 = Math.SQRT1_2;
const SPEED = { walk: 1.4, run: 3.5, bike: 5.5, car: 14 };
const SPRITE = 3.2;                          // people and bikes are drawn larger than life, or they'd be 4 px tall
const F_BUILDING = 1, F_WATER = 2, F_ROAD = 4, F_FOOT = 8;

const me = { x: 0, y: 0, z: 0, a: 0, mode: "walk", v: 0, ride: null, color: "#e8577a" };
let session = null;                           // { token, name, reserved }
let ws = null, myId = null, wsFails = 0;
const others = new Map();                     // id -> { name, x, y, z, a, m, from, to, t0, bubble }
const keys = new Set();
let joy = null;                               // touch joystick vector [-1..1]
let lastSend = 0, sentAt = [0, 0];
window.TNGame = { me, others, navAt, walkable };       // for debugging from the console

// ---------------------------------------------------------------- side panel
const side = $("side"), sideBody = $("sideBody");
$("sideClose").onclick = () => closeSide();
function openSide(html) { sideBody.innerHTML = html; side.hidden = false; side.scrollTop = 0; }
function closeSide() { side.hidden = true; sideBody.innerHTML = ""; }
const hm = (t) => new Date(t * 1000).toLocaleTimeString("fr-FR", { hour: "2-digit", minute: "2-digit" });
const delayTxt = (d) => (d == null ? "" : Math.abs(d) < 60 ? `<span class="ok">à l'heure</span>` :
  `<span class="late">${d > 0 ? "+" : "−"}${Math.round(Math.abs(d) / 60)} min</span>`);
async function getJSON(u) { const r = await fetch(u); if (!r.ok) throw new Error(r.status); return r.json(); }

live.open = async (kind, d) => {
  if (kind === "vehicle") {
    const { v, r } = d, t = live.now();
    const name = { 0: "Tram", 3: "Bus", 4: "Navibus" }[r.type] || "Ligne";
    const rows = v.names.map((n, i) => ({ n, t: v.arr[i] })).filter((s) => s.t >= t - 30).slice(0, 12);
    const pose = live.vehiclePose(v, t);
    const near = TN.playing && !me.ride && Math.hypot(pose.x - me.x, pose.y - me.y) < 25;
    openSide(`<p class="kicker" style="--c:${esc(r.color)}">${name} <b>${esc(r.short)}</b></p>
      <h2>→ ${esc(v.headsign)}</h2><p>${delayTxt(v.delay) || "horaire théorique"}</p>
      ${near ? `<button class="act" id="boardBtn">${pose.stop >= 0 ? "Monter (E)" : "Attendre l'arrêt…"}</button>` : ""}
      <h3>Prochains arrêts</h3><ol class="stops">${rows.map((s) => `<li><time>${hm(s.t)}</time>${esc(s.n)}</li>`).join("")}</ol>
      <p class="src">Horaires Naolib corrigés en temps réel (GTFS-RT)</p>`);
    if (near) $("boardBtn").onclick = () => action();
  } else if (kind === "agent") {
    let a = null;
    try { a = await getJSON(`api/agent/${d.id}`); } catch { /* gone */ }
    if (!a) return;
    const icon = { car: "🚗", walker: "🚶", bike: "🚲" }[a.kind];
    openSide(`<p class="kicker">${icon} Habitant·e</p><h2>${esc(a.name)}</h2><p>${esc(a.doing)}</p>
      ${a.place ? `<p><a href="#" id="agentPlace">Voir le lieu</a></p>` : ""}
      <p class="src">Simulé par le serveur : la circulation suit les comptages réels, les passants vont vers des lieux ouverts à cette heure.</p>`);
    const pl = a.place && live.world.pois.find((p) => p.id === a.place);
    if (pl) $("agentPlace").onclick = (e) => { e.preventDefault(); live.open("poi", pl); };
  } else if (kind === "train") {
    const m = d;
    openSide(`<p class="kicker">${esc(m.mode)} <b>${esc(m.number)}</b></p><h2>→ ${esc(m.headsign)}</h2>
      <p>${delayTxt(m.delay)}</p>
      <ol class="stops"><li><time>${hm(m.from.dep)}</time>${esc(m.from.name)}</li><li><time>${hm(m.to.arr)}</time>${esc(m.to.name)}</li></ol>
      <p class="src">SNCF open data (GTFS + temps réel)</p>`);
  } else if (kind === "plane") {
    const p = d, r = p.route;
    const ap = (a) => (a ? `${esc(a.city || a.name)} <small>${esc(a.iata)} · ${esc(a.name)}</small>` : "?");
    openSide(`<p class="kicker">✈ ${esc(r?.airline || "Vol")} <b>${esc(p.callsign || p.hex)}</b></p>
      <h2>${r ? `${esc(r.from?.city || "?")} → ${esc(r.to?.city || "?")}` : "Route inconnue"}</h2>
      ${r ? `<dl><dt>Départ</dt><dd>${ap(r.from)}</dd><dt>Arrivée</dt><dd>${ap(r.to)}</dd></dl>` : ""}
      <dl><dt>Appareil</dt><dd>${esc(p.kind || "?")} ${p.reg ? `· ${esc(p.reg)}` : ""}</dd>
      <dt>Altitude</dt><dd>${p.ground ? "au sol" : `${Math.round(p.alt)} m`}</dd>
      <dt>Vitesse</dt><dd>${Math.round(p.speed * 3.6)} km/h</dd></dl>
      <p class="src">Position ADS-B : adsb.lol (ODbL) · Route : adsbdb.com</p>`);
  } else if (kind === "station") {
    openSide(`<h2>🚉 Gare de Nantes</h2><p>Chargement…</p>`);
    try {
      const s = await getJSON("api/station"), b = s.board;
      const table = (rows, w) => `<table class="board">${rows.map((r) => `<tr><td>${hm(r.time)}</td><td>${delayTxt(r.delay)}</td>
        <td><small>${esc(r.mode.replace("Train ", ""))} ${esc(r.number)}</small><br>${esc(r.other)}</td></tr>`).join("") || `<tr><td>Aucun ${w}</td></tr>`}</table>`;
      openSide(`<h2>🚉 Gare de Nantes</h2>
        <div class="tabs"><button class="on" data-t="dep">Départs</button><button data-t="arr">Arrivées</button></div>
        <div data-p="dep">${table(b.departures, "départ")}</div><div data-p="arr" hidden>${table(b.arrivals, "arrivée")}</div>
        <p class="src">SNCF open data, retards temps réel</p>`);
      tabs();
    } catch { openSide(`<h2>🚉 Gare de Nantes</h2><p>Tableau indisponible pour le moment.</p>`); }
  } else if (kind === "airport") {
    openSide(`<h2>✈ Nantes Atlantique</h2><p>Chargement…</p>`);
    try {
      const a = await getJSON("api/airport");
      const ap = (o) => (o ? `${esc(o.city || o.name)} <small>${esc(o.iata)}</small>` : "?");
      const table = (rows, w) => `<table class="board">${rows.map((m) => `<tr><td>${hm(m.ts)}</td>
        <td><small>${esc(m.airline || "")} ${esc(m.callsign)}</small><br>${ap(m.other)}</td></tr>`).join("") || `<tr><td>Aucun ${w} observé ces dernières heures</td></tr>`}</table>`;
      openSide(`<h2>✈ Nantes Atlantique</h2>
        <div class="tabs"><button class="on" data-t="dep">Décollages</button><button data-t="arr">Atterrissages</button></div>
        <div data-p="dep">${table(a.departures, "décollage")}</div><div data-p="arr" hidden>${table(a.arrivals, "atterrissage")}</div>
        <p class="src">${esc(a.note)}. Les horaires sont ceux observés, pas les horaires prévus.</p>`);
      tabs();
    } catch { openSide(`<h2>✈ Nantes Atlantique</h2><p>Indisponible pour le moment.</p>`); }
  } else if (kind === "event") {
    const e = d;
    openSide(`${e.image ? `<img class="hero" src="${esc(e.image)}" alt="">` : ""}<p class="kicker">${esc(e.types.join(" · "))}</p>
      <h2>${esc(e.name)}</h2><p><b>${esc(e.date)} ${esc(e.start || "")}${e.end ? "–" + esc(e.end) : ""}</b>${e.free ? " · gratuit" : ""}${e.full ? " · complet" : ""}</p>
      <p>${esc(e.place)}<br><small>${esc(e.address)}</small></p><p>${esc(e.summary)}</p>
      ${e.url ? `<p><a href="${esc(e.url)}" target="_blank" rel="noopener">Plus d'infos</a></p>` : ""}
      <p class="src">Agenda de Nantes Métropole</p>`);
  } else if (kind === "poi") {
    const p = d, t = p.t;
    const head = `<p class="kicker">${esc((p.k || "").split("=")[1]?.replace(/_/g, " "))}${t.cuisine ? " · " + esc(t.cuisine.replace(/;/g, ", ")) : ""}</p>
      <h2>${esc(t.name)}</h2>
      ${t.opening_hours ? `<p><small>Horaires (OSM) :</small> ${esc(t.opening_hours)}</p>` : ""}
      ${t.phone || t["contact:phone"] ? `<p><a href="tel:${esc(t.phone || t["contact:phone"])}">${esc(t.phone || t["contact:phone"])}</a></p>` : ""}`;
    openSide(head + `<p>…</p>`);
    let info = null;
    try { info = await getJSON(`api/poi/${encodeURIComponent(p.id)}`); } catch { /* not crawled */ }
    let body = "";
    if (info?.image) body += `<img class="hero" src="${esc(info.image)}" alt="" onerror="this.remove()">`;
    if (info?.description) body += `<p>${esc(info.description)}</p>`;
    if (info?.sections?.length) {
      body += `<h3>La carte</h3>` + info.sections.map((s) => `${s.title ? `<h4>${esc(s.title)}</h4>` : ""}<ul class="menu">${s.items.map((it) =>
        `<li><span>${esc(it.name)}</span>${it.price != null ? `<b>${it.price.toFixed(2).replace(".", ",")} €</b>` : ""}</li>`).join("")}</ul>`).join("");
      body += `<p class="src">Extrait automatiquement de <a href="${esc(info.menu_url)}" target="_blank" rel="noopener">leur site</a>, peut être incomplet ou daté.</p>`;
    } else if (info?.pdfs?.length) {
      body += `<h3>La carte</h3><ul>${info.pdfs.map((u, k) => `<li><a href="${esc(u)}" target="_blank" rel="noopener">Menu (PDF ${k + 1})</a></li>`).join("")}</ul>`;
    }
    const site = t.website || t["contact:website"];
    if (site) body += `<p><a href="${esc(site)}" target="_blank" rel="noopener">Site web</a></p>`;
    if (!body) body = `<p>Pas encore d'infos sur ce lieu.</p>`;
    openSide(head + body + `<p class="src">© contributeurs OpenStreetMap</p>`);
  }
};
function tabs() {
  sideBody.querySelectorAll(".tabs button").forEach((b) => (b.onclick = () => {
    sideBody.querySelectorAll(".tabs button").forEach((x) => x.classList.toggle("on", x === b));
    sideBody.querySelectorAll("[data-p]").forEach((p) => (p.hidden = p.dataset.p !== b.dataset.t));
  }));
}
$("airportBtn").onclick = () => live.open("airport");

// ---------------------------------------------------------------- navigation grid
const navChunks = new Map();
function navAt(x, y) {
  const m = live.world.meta, nav = m?.nav;
  if (!nav) return null;
  const X = x + m.origin[0], Y = y + m.origin[1], C = nav.chunk;
  const cx = Math.floor(X / C), cy = Math.floor(Y / C), k = `${cx}_${cy}`;
  let c = navChunks.get(k);
  if (!c) {
    c = { ready: false };
    navChunks.set(k, c);
    const im = new Image();
    im.onload = () => {
      const cv = document.createElement("canvas"); cv.width = im.width; cv.height = im.height;
      const g = cv.getContext("2d", { willReadFrequently: true }); g.drawImage(im, 0, 0);
      c.d = g.getImageData(0, 0, im.width, im.height).data; c.n = im.width; c.ready = true;
    };
    c.loaded = new Promise((ok) => { im.onerror = () => { c.missing = true; ok(); }; im.addEventListener("load", () => setTimeout(ok)); });
    im.src = `data/world/nav/${k}.png`;
  }
  if (!c.ready) return null;
  c.used = performance.now();
  const col = Math.floor((X - cx * C) / nav.res), row = Math.floor((cy * C + C - Y) / nav.res);
  if (col < 0 || row < 0 || col >= c.n || row >= c.n) return null;
  const i = 4 * (row * c.n + col);
  return { f: c.d[i], deck: c.d[i + 1] / 10 };
}
async function navReady(x, y) {
  navAt(x, y);
  const m = live.world.meta, C = m.nav.chunk;
  const c = navChunks.get(`${Math.floor((x + m.origin[0]) / C)}_${Math.floor((y + m.origin[1]) / C)}`);
  await c?.loaded;
}
function walkable(mode, x, y) {
  const n = navAt(x, y);
  if (!n) return true;                        // grid not loaded (or off the grid): don't trap the player
  if (n.f & (F_ROAD | F_FOOT)) return true;        // cars may mount the pavement, GTA style (slowly)
  if (n.f & F_BUILDING) return false;
  if (n.f & F_WATER) return n.deck > 1.5;
  return true;
}
function onRoad(x, y) {
  const n = navAt(x, y);
  return !n || (n.f & F_ROAD) !== 0;
}
function groundZ(x, y) {
  const n = navAt(x, y);
  return live.zAt(x, y) + (n && n.deck > 1.5 ? n.deck : 0);
}

// screen direction -> world direction on the ground (inverse of the projection's xy part)
function screenToWorldDir(ux, uy) {
  const [r, d] = TN.man.axes;
  const det = r[0] * d[1] - r[1] * d[0];
  const wx = (ux * d[1] - uy * r[1]) / det, wy = (uy * r[0] - ux * d[0]) / det;
  const n = Math.hypot(wx, wy) || 1;
  return [wx / n, wy / n];
}
// the ground point under screen point (sx, sy)
function screenToGround(sx, sy) {
  const [r, d] = TN.man.axes;
  let z = 10, x = 0, y = 0;
  for (let k = 0; k < 3; k++) {
    const a = sx - r[2] * z, b = sy - d[2] * z, det = r[0] * d[1] - r[1] * d[0];
    x = (a * d[1] - b * r[1]) / det; y = (b * r[0] - a * d[0]) / det;
    z = live.zAt(x, y);
  }
  return [x, y];
}

// ---------------------------------------------------------------- playing
async function startPlaying() {
  if (!live.world.ready) return toast("La carte charge encore…");
  const [x0, y0] = screenToGround(TN.view.x, TN.view.y);
  for (const dx of [-200, 0, 200]) for (const dy of [-200, 0, 200]) await navReady(x0 + dx, y0 + dy);
  let spot = [x0, y0];
  outer: for (let r = 0; r < 200; r += 3) {          // nearest walkable spot, spiralling out
    for (let k = 0; k < Math.max(1, r); k++) {
      const a = k / Math.max(1, r) * Math.PI * 2, x = x0 + Math.cos(a) * r, y = y0 + Math.sin(a) * r;
      const n = navAt(x, y);
      if (n && (n.f & (F_FOOT | F_ROAD))) { spot = [x, y]; break outer; }
    }
  }
  Object.assign(me, { x: spot[0], y: spot[1], z: groundZ(...spot), mode: "walk", v: 0, ride: null });
  me.color = colorFor(session.name);
  TN.playing = true;
  document.body.classList.add("playing");
  $("playBtn").setAttribute("aria-pressed", "true");
  $("liveBtn").setAttribute("aria-pressed", "true");
  const [sx, sy] = live.project(me.x, me.y, me.z);
  TN.follow(sx, sy, Math.max(TN.view.s, 6));
  connect();
  loadChat();
  hud();
}
function stopPlaying() {
  TN.playing = false; me.ride = null;
  document.body.classList.remove("playing");
  $("playBtn").setAttribute("aria-pressed", "false");
  if (ws) { ws.onclose = null; ws.close(); ws = null; }
  others.clear();
}
$("playBtn").onclick = () => {
  if (TN.playing) return stopPlaying();
  if (!session) return showLogin();
  startPlaying();
};

// ---------------------------------------------------------------- login
function showLogin(msg) {
  const d = $("login");
  $("loginErr").textContent = msg || "";
  d.hidden = false;
  $("loginName").value = session?.name || localStorage.getItem("tn.name") || "";
  $("loginName").focus();
}
$("loginCancel").onclick = () => ($("login").hidden = true);
$("loginForm").onsubmit = async (e) => {
  e.preventDefault();
  const name = $("loginName").value.trim(), password = $("loginPass").value || undefined;
  $("loginErr").textContent = "";
  try {
    const r = await fetch("api/auth/login", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ name, password }) });
    const j = await r.json();
    if (!r.ok) { $("loginErr").textContent = j.error || "Connexion refusée"; return; }
    session = j;
    try { localStorage.setItem("tn.session", JSON.stringify(j)); localStorage.setItem("tn.name", j.name); } catch { /* private mode */ }
    $("loginPass").value = "";
    $("login").hidden = true;
    startPlaying();
  } catch { $("loginErr").textContent = "Serveur injoignable"; }
};
try { session = JSON.parse(localStorage.getItem("tn.session") || "null"); } catch { session = null; }

// ---------------------------------------------------------------- multiplayer
function connect() {
  const u = new URL(`api/ws?token=${encodeURIComponent(session.token)}`, location.href);
  u.protocol = location.protocol === "https:" ? "wss:" : "ws:";
  myId = null;
  ws = new WebSocket(u);
  ws.onmessage = (ev) => {
    let m; try { m = JSON.parse(ev.data); } catch { return; }
    if (m.t === "hello") { myId = m.id; wsFails = 0; }
    else if (m.t === "players") {
      const now = performance.now(), seen = new Set();
      for (const p of m.list) {
        if (p.id === myId) continue;
        seen.add(p.id);
        let o = others.get(p.id);
        if (!o) { o = { ...p, from: [p.x, p.y, p.z], to: [p.x, p.y, p.z], t0: now, color: colorFor(p.name) }; others.set(p.id, o); }
        o.from = curPos(o, now); o.to = [p.x, p.y, p.z]; o.t0 = now; o.a = p.a; o.m = p.m; o.name = p.name;
      }
      for (const id of others.keys()) if (!seen.has(id)) others.delete(id);
    } else if (m.t === "leave") others.delete(m.id);
    else if (m.t === "chat") {
      addChat(m.name, m.text, m.ts);
      const o = m.id === myId ? me : others.get(m.id);
      if (o) o.bubble = { text: m.text, until: performance.now() + 7000 };
    }
  };
  ws.onclose = (ev) => {
    ws = null;
    if (!TN.playing) return;
    if (myId == null) wsFails++;
    if (wsFails >= 2) {       // refused twice before any hello: the token is no longer valid
      session = null; try { localStorage.removeItem("tn.session"); } catch { /* */ } stopPlaying(); return showLogin("Session expirée, reconnecte-toi."); }
    setTimeout(() => TN.playing && !ws && connect(), 3000);
  };
}
function curPos(o, now) {
  const f = Math.min(1, (now - o.t0) / 220);
  return o.from.map((v, k) => v + (o.to[k] - v) * f);
}
function sendPos(force) {
  if (!ws || ws.readyState !== 1) return;
  const now = performance.now();
  const moved = Math.hypot(me.x - sentAt[0], me.y - sentAt[1]) > 0.2;
  if (!force && (now - lastSend < 150 || (!moved && now - lastSend < 2000))) return;
  lastSend = now; sentAt = [me.x, me.y];
  const m = me.ride ? me.ride.kind : me.mode;
  ws.send(JSON.stringify({ t: "pos", x: +me.x.toFixed(2), y: +me.y.toFixed(2), z: +me.z.toFixed(2), a: +me.a.toFixed(3), m }));
}

// ---------------------------------------------------------------- chat
const chatLog = $("chatLog");
function addChat(name, text, ts) {
  const li = document.createElement("li");
  li.innerHTML = `<time>${hm(ts || Date.now() / 1000)}</time> <b style="color:${colorFor(name)}">${esc(name)}</b> ${esc(text)}`;
  chatLog.append(li);
  while (chatLog.children.length > 60) chatLog.firstChild.remove();
  chatLog.scrollTop = chatLog.scrollHeight;
}
async function loadChat() {
  chatLog.innerHTML = "";
  try { for (const m of await getJSON("api/chat/recent")) addChat(m.name, m.text, m.ts); } catch { /* none */ }
}
$("chatForm").onsubmit = (e) => {
  e.preventDefault();
  const t = $("chatIn").value.trim();
  if (t && ws?.readyState === 1) ws.send(JSON.stringify({ t: "chat", text: t.slice(0, 280) }));
  $("chatIn").value = ""; $("chatIn").blur();
};

// ---------------------------------------------------------------- input
addEventListener("keydown", (e) => {
  if (!TN.playing || e.target.tagName === "INPUT") return;
  if (e.code === "Enter" || e.code === "KeyT") { e.preventDefault(); $("chatIn").focus(); return; }
  if (e.code === "Escape") { if (!side.hidden) closeSide(); else stopPlaying(); return; }
  if (e.code === "KeyE" || e.code === "KeyF") { action(); return; }
  if (e.code === "Digit1") setMode("walk");
  else if (e.code === "Digit2") setMode("bike");
  else if (e.code === "Digit3") setMode("car");
  keys.add(e.code);
  if (e.code.startsWith("Arrow") || e.code === "Space") e.preventDefault();
});
addEventListener("keyup", (e) => keys.delete(e.code));
addEventListener("blur", () => keys.clear());

function setMode(m) {
  if (me.ride) return toast("Descends d'abord (E à un arrêt)");
  if (m === "car" && !onRoad(me.x, me.y)) return toast("Rejoins une rue pour prendre la voiture");
  me.mode = m; me.v = 0; hud();
}
document.querySelectorAll("#modes button").forEach((b) => (b.onclick = () => setMode(b.dataset.m)));
$("actBtn").onclick = () => action();

// touch joystick
const pad = $("joy");
pad.addEventListener("pointerdown", (e) => { pad.setPointerCapture(e.pointerId); moveJoy(e); });
pad.addEventListener("pointermove", (e) => { if (pad.hasPointerCapture(e.pointerId)) moveJoy(e); });
pad.addEventListener("pointerup", () => { joy = null; pad.style.setProperty("--jx", "0px"); pad.style.setProperty("--jy", "0px"); });
function moveJoy(e) {
  const r = pad.getBoundingClientRect(), R = r.width / 2;
  let dx = (e.clientX - r.left - R) / R, dy = (e.clientY - r.top - R) / R;
  const n = Math.hypot(dx, dy); if (n > 1) { dx /= n; dy /= n; }
  joy = [dx, dy];
  pad.style.setProperty("--jx", `${dx * R * 0.6}px`); pad.style.setProperty("--jy", `${dy * R * 0.6}px`);
}

// E: get on / off transit, or go into the place in front of you
function action() {
  const t = live.now();
  if (me.ride) {
    const v = rideVehicle();
    const p = v && live.vehiclePose(v, t);
    if (p && p.stop < 0) return toast("Attends l'arrêt pour descendre");
    if (p) { me.x = p.x + Math.sin(p.ang) * 3; me.y = p.y - Math.cos(p.ang) * 3; }
    toast(p ? `Descendu à ${v.names[p.stop] || "l'arrêt"}` : "Descendu");
    me.ride = null; me.mode = "walk"; me.v = 0; hud(); return;
  }
  if (me.mode !== "car") {
    let best = null, bd = 22;
    for (const v of live.vehicles) {
      const p = live.vehiclePose(v, t), d = Math.hypot(p.x - me.x, p.y - me.y);
      if (d < bd) { bd = d; best = { v, p }; }
    }
    if (best) {
      if (best.p.stop < 0) return toast("Le véhicule roule, attends qu'il s'arrête");
      const r = live.world.routes[best.v.route] || {};
      me.ride = { id: best.v.id, kind: { 0: "tram", 3: "bus", 4: "navibus" }[r.type] || "bus", route: r, last: best.v };
      toast(`Tu montes : ${r.short || ""} → ${best.v.headsign}`); hud(); return;
    }
  }
  // places: the closest one within 18 m
  let bp = null, bd = 18;
  for (const p of live.world.pois) {
    const d = Math.hypot(p.p[0] - me.x, p.p[1] - me.y);
    if (d < bd) { bd = d; bp = p; }
  }
  if (bp) { live.open("poi", bp); return; }
  const st = live.world.rails?.station;
  if (st && Math.hypot(st[0] - me.x, st[1] - me.y) < 80) { live.open("station"); return; }
  toast("Rien ici. Approche-toi d'un arrêt (tram, bus, Navibus) ou d'un lieu.");
}
function rideVehicle() {
  if (!me.ride) return null;
  const v = live.vehicles.find((x) => x.id === me.ride.id);
  if (v) me.ride.last = v;
  return v || me.ride.last;
}

// ---------------------------------------------------------------- simulation
let lastT = performance.now();
function step() {
  requestAnimationFrame(step);
  const now = performance.now(), dt = Math.min(0.1, (now - lastT) / 1000);
  lastT = now;
  if (!TN.playing || !TN.man) return;
  if (me.ride) {
    const v = rideVehicle(), t = live.now();
    if (v) {
      const p = live.vehiclePose(v, t);
      me.x = p.x; me.y = p.y; me.z = p.z; me.a = p.ang;
      if (p.stop === v.arr.length - 1 && t > v.arr[v.arr.length - 1] + 20) { toast("Terminus !"); action(); }
    }
  } else {
    let ux = 0, uy = 0;
    if (keys.has("KeyW") || keys.has("ArrowUp")) uy -= 1;
    if (keys.has("KeyS") || keys.has("ArrowDown")) uy += 1;
    if (keys.has("KeyA") || keys.has("ArrowLeft")) ux -= 1;
    if (keys.has("KeyD") || keys.has("ArrowRight")) ux += 1;
    if (joy) { ux = joy[0]; uy = joy[1]; }
    const run = keys.has("ShiftLeft") || keys.has("ShiftRight") || (joy && Math.hypot(...joy) > 0.9);
    if (me.mode === "car") driveCar(ux, uy, dt);
    else {
      const mag = Math.min(1, Math.hypot(ux, uy));
      if (mag > 0.05) {
        const [wx, wy] = screenToWorldDir(ux, uy);
        const vmax = me.mode === "bike" ? SPEED.bike : run ? SPEED.run : SPEED.walk;
        me.v = Math.min(vmax * mag, me.v + (me.mode === "bike" ? 2.5 : 6) * dt);
        me.a = Math.atan2(wy, wx);
        moveBy(wx * me.v * dt, wy * me.v * dt);
      } else me.v = Math.max(0, me.v - 8 * dt);
    }
    me.z = groundZ(me.x, me.y);
  }
  const [sx, sy] = live.project(me.x, me.y, me.z);
  const v = TN.view, k = 1 - Math.exp(-dt * 6);
  TN.follow(v.x + (sx - v.x) * k, v.y + (sy - v.y) * k);
  sendPos();
  if (now - (step.hudAt || 0) > 500) { step.hudAt = now; hud(); }
}
function moveBy(dx, dy) {
  const m = me.mode;
  if (!walkable(m, me.x, me.y)) { me.x += dx; me.y += dy; return true; }   // stuck inside: let them walk out
  if (walkable(m, me.x + dx, me.y + dy)) { me.x += dx; me.y += dy; return true; }
  if (walkable(m, me.x + dx, me.y)) { me.x += dx; return true; }      // slide along walls
  if (walkable(m, me.x, me.y + dy)) { me.y += dy; return true; }
  return false;
}
// GTA-style: up/down = throttle/brake-reverse, left/right = steer
function driveCar(ux, uy, dt) {
  const throttle = -uy, steer = -ux;
  if (throttle > 0) me.v += (me.v < 0 ? 9 : 3.2) * throttle * dt;
  else if (throttle < 0) me.v += (me.v > 0 ? 9 : 2) * throttle * dt;
  else me.v -= Math.sign(me.v) * Math.min(Math.abs(me.v), 1.2 * dt);
  // off the carriageway (pavement, square, park): walking pace-ish; probe a little ahead so 1-cell gaps don't brake
  const ahead = onRoad(me.x, me.y) || onRoad(me.x + Math.cos(me.a) * 2, me.y + Math.sin(me.a) * 2);
  me.v = Math.max(-4, Math.min(ahead ? SPEED.car : 5, me.v));
  me.a += steer * dt * 1.3 * Math.min(1, Math.abs(me.v) / 4) * Math.sign(me.v || 1);
  const dx = Math.cos(me.a) * me.v * dt, dy = Math.sin(me.a) * me.v * dt;
  if (walkable("car", me.x, me.y) && !walkable("car", me.x + dx, me.y + dy)) { me.v *= -0.25; return; }  // bump
  me.x += dx; me.y += dy;
}

// ---------------------------------------------------------------- drawing (a layer of live.js)
function colorFor(name) {
  let h = 0; for (const c of String(name)) h = (h * 31 + c.charCodeAt(0)) >>> 0;
  return `hsl(${h % 360} 70% 58%)`;
}
function hslHex(c) {                            // live.box shades hex colours
  const m = /hsl\((\d+) (\d+)% (\d+)%\)/.exec(c); if (!m) return c;
  const h = +m[1] / 360, s = +m[2] / 100, l = +m[3] / 100;
  const q = l < 0.5 ? l * (1 + s) : l + s - l * s, p = 2 * l - q;
  const f = (t) => { t = (t + 1) % 1; return t < 1 / 6 ? p + (q - p) * 6 * t : t < 0.5 ? q : t < 2 / 3 ? p + (q - p) * (2 / 3 - t) * 6 : p; };
  return "#" + [h + 1 / 3, h, h - 1 / 3].map((t) => Math.round(f(t) * 255).toString(16).padStart(2, "0")).join("");
}
function avatarItems(o, x, y, z, a, mode, name, isMe) {
  const [sx, sy] = live.project(x, y, z), col = hslHex(o.color || "#e8577a"), K = SPRITE;
  const items = [];
  if (mode === "tram" || mode === "bus" || mode === "navibus") return items;    // inside the vehicle
  items.push({ sx, sy, h: 2 * K, draw: () => {
    if (mode === "car") live.box(x, y, z + 0.2, a, 4.3, 1.8, 1.45, live.shade(col, 1.1), col, "rgba(20,24,28,.6)");
    else {
      if (mode === "bike") live.box(x, y, z + 0.3 * K, a, 1.7 * K * 0.8, 0.12 * K, 0.5 * K, "#2b2f36", "#2b2f36");
      const lift = mode === "bike" ? 0.45 * K : 0;
      live.box(x, y, z + lift, a, 0.45 * K, 0.5 * K, 1.1 * K, live.shade(col, 1.1), col, "rgba(20,24,28,.5)");
      live.box(x, y, z + lift + 1.1 * K, a, 0.35 * K, 0.35 * K, 0.38 * K, "#f1c9a5", "#d9a982", "rgba(20,24,28,.5)");
    }
  } });
  items.push({ sx, sy, overlay: true, draw: () => {
    const [cx, cy] = live.toCanvas(...live.project(x, y, z + 2.2 * K));
    const ctx = document.getElementById("live").getContext("2d");
    ctx.font = `600 ${11 * TN.dpr}px Geist, system-ui, sans-serif`; ctx.textAlign = "center"; ctx.textBaseline = "bottom";
    ctx.lineWidth = 3 * TN.dpr; ctx.strokeStyle = "rgba(20,24,28,.85)"; ctx.strokeText(name, cx, cy);
    ctx.fillStyle = isMe ? "#f2b544" : "#fff"; ctx.fillText(name, cx, cy); ctx.textAlign = "start";
    if (o.bubble && performance.now() < o.bubble.until) live.label(cx, cy - 16 * TN.dpr, wrap(o.bubble.text, 34).slice(0, 4), "rgba(20,24,28,.92)");
  } });
  return items;
}
function wrap(t, n) {
  const out = []; let line = "";
  for (const w of t.split(/\s+/)) { if ((line + " " + w).trim().length > n) { out.push(line); line = w; } else line = (line + " " + w).trim(); }
  if (line) out.push(line);
  return out;
}
live.layers.push(() => {
  if (!TN.playing) return [];
  const now = performance.now(), items = [];
  for (const o of others.values()) {
    const [x, y, z] = curPos(o, now);
    items.push(...avatarItems(o, x, y, z, o.a, o.m, o.name, false));
  }
  items.push(...avatarItems(me, me.x, me.y, me.z, me.a, me.ride ? me.ride.kind : me.mode, session?.name || "", true));
  return items;
});

// ---------------------------------------------------------------- HUD
function hud() {
  const h = $("hud");
  if (!TN.playing) return;
  document.querySelectorAll("#modes button").forEach((b) => b.setAttribute("aria-pressed", String(b.dataset.m === me.mode && !me.ride)));
  let txt;
  if (me.ride) {
    const v = rideVehicle(), t = live.now();
    const i = v ? v.arr.findIndex((a) => a > t) : -1;
    const p = v && live.vehiclePose(v, t);
    txt = `${{ tram: "🚋", bus: "🚌", navibus: "⛴" }[me.ride.kind]} <b>${esc(me.ride.route.short || "")}</b> → ${esc(v?.headsign || "")}<br>` +
      (p && p.stop >= 0 ? `À l'arrêt <b>${esc(v.names[p.stop])}</b> · <kbd>E</kbd> descendre` :
       i >= 0 ? `Prochain arrêt : <b>${esc(v.names[i])}</b> (${Math.max(0, Math.round((v.arr[i] - t) / 60))} min)` : "");
  } else {
    const kmh = Math.round(Math.abs(me.v) * 3.6);
    txt = `<b>${esc(session?.name)}</b> · ${{ walk: "à pied", bike: "à vélo", car: "en voiture" }[me.mode]} · ${kmh} km/h`;
  }
  h.innerHTML = txt;
}
let toastT = 0;
function toast(t) {
  const el = $("toast"); el.textContent = t; el.hidden = false;
  clearTimeout(toastT); toastT = setTimeout(() => (el.hidden = true), 2600);
}

requestAnimationFrame(step);
