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
const SPEED = { walk: 1.6, run: 5.0, bike: 11.0, car: 34, heli: 50 };   // m/s; running is the default, Shift walks
const HELIPORT = [-421.1, -751.1];                                     // CHU Nantes heliport (OSM), on the Hôtel-Dieu roof
const SPRITE = 3.2;                          // people and bikes are drawn larger than life, or they'd be 4 px tall
const F_BUILDING = 1, F_WATER = 2, F_ROAD = 4, F_FOOT = 8;

const me = { x: 0, y: 0, z: 0, a: 0, mode: "walk", v: 0, ride: null, color: "#e8577a", alt: 0, vx: 0, vy: 0 };
let session = null;                           // { token, name, reserved }
let ws = null, myId = null, wsFails = 0;
const others = new Map();                     // id -> { name, x, y, z, a, m, from, to, t0, bubble }
const keys = new Set();
let joy = null;                               // touch joystick vector [-1..1]
let lastSend = 0, sentAt = [0, 0];
window.TNGame = { me, others, navAt, walkable, curPos };       // for debugging from the console

// ---------------------------------------------------------------- side panel
const side = $("side"), sideBody = $("sideBody");
$("sideClose").onclick = () => (talking ? closeTalk() : closeSide());
function openSide(html) { sideBody.innerHTML = html; side.hidden = false; side.scrollTop = 0; }
function closeSide() { side.hidden = true; sideBody.innerHTML = ""; sideBody.dataset.poi = ""; }
const hm = (t) => new Date(t * 1000).toLocaleTimeString("fr-FR", { hour: "2-digit", minute: "2-digit" });
const delayTxt = (d) => (d == null ? "" : Math.abs(d) < 60 ? `<span class="ok">à l'heure</span>` :
  `<span class="late">${d > 0 ? "+" : "−"}${Math.round(Math.abs(d) / 60)} min</span>`);
async function getJSON(u) { const r = await fetch(u); if (!r.ok) throw new Error(r.status); return r.json(); }

live.open = async (kind, d) => {
  sideBody.dataset.poi = "";
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
    const icon = { car: "🚗", walker: "🚶", bike: "🚲", ambulance: "🚑", police: "🚓", pompiers: "🚒" }[a.kind];
    const who = { ambulance: "Ambulance", police: "Police", pompiers: "Pompiers" }[a.kind] || "Habitant·e (PNJ)";
    const P = a.persona;
    const near = P && TN.playing && session && nearestSim(12)?.id === a.id;
    openSide(`<p class="kicker">${icon} ${who}</p><h2>${esc(P ? P.name : a.name)}</h2>
      ${P ? `<p>${P.age} ans · ${esc(P.job)}${P.archetype ? ` · <i>${esc(P.archetype)}</i>` : ""}<br>
        Habite ${esc(P.sub ? `${P.sub} (${P.quartier})` : P.quartier)}</p>` : ""}
      <p>${esc(a.doing)}</p>
      ${P ? `<p><b>Sortie :</b> ${esc(P.purpose)}<br><b>Trajet :</b> ${esc(P.trip)}<br>
        <b>Humeur :</b> ${esc(P.mood)} · ${P.chatty < 0.3 ? "pressé·e" : P.chatty > 0.7 ? "bavard·e" : "poli·e"}</p>` : ""}
      ${a.march ? `<p>✊ ${esc(a.march)}</p>` : ""}
      ${near ? `<button class="act" id="talkBtn">Parler (E)</button>` : P && TN.playing ? `<p class="src">Approche-toi pour lui parler (E).</p>` : ""}
      ${a.place ? `<p><a href="#" id="agentPlace">Voir le lieu</a></p>` : ""}
      <p class="src">Personnage non joueur, simulé par le serveur ; ses paroles sont générées par une IA.</p>`);
    if (near) $("talkBtn").onclick = () => openTalk(a.id);
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
  } else if (kind === "elephant") {
    const e = live.elephant();
    const now = { walking: "Il est en balade dans le Parc des Chantiers.", spraying: "Il s'est arrêté et arrose les passants.",
                  boarding: "Embarquement des passagers sous les Nefs.", parked: "Il attend sous les Nefs." }[e.phase];
    openSide(`<p class="kicker">🐘 Les Machines de l'île</p><h2>Le Grand Éléphant</h2>
      <p>12 m de haut, 21 m de long, 48 tonnes de bois et d'acier ; il emmène 50 passagers pour 30 minutes, à 1 à 3 km/h.</p>
      <p><b>${e.why ? `Pas de balade : ${esc(e.why)}.` : esc(now)}</b></p>
      ${e.next.length ? `<p>Prochains départs aujourd'hui : ${e.next.map(esc).join(" · ")}${e.rough ? " <small>(horaires indicatifs)</small>" : ""}</p>` : ""}
      <p><a href="https://www.lesmachines-nantes.fr/" target="_blank" rel="noopener">lesmachines-nantes.fr</a></p>
      <p class="src">Pas de position en direct : il suit les horaires publiés par Les Machines de l'île.</p>`);
  } else if (kind === "bikes") {
    const st = d, n = Math.max(0, st.bikes - (live.rented[st.id] || 0) + (live.returned[st.id] || 0));
    const near = TN.playing && Math.hypot(st.p[0] - me.x, st.p[1] - me.y) < 25;
    openSide(`<p class="kicker">🚲 Naolib Vélo</p><h2>${esc(st.name)}</h2><p>${esc(st.address)}</p>
      <p><b>${n}</b> vélo${n > 1 ? "s" : ""} disponible${n > 1 ? "s" : ""} · <b>${st.docks}</b> place${st.docks > 1 ? "s" : ""} libre${st.docks > 1 ? "s" : ""} · ${st.capacity} au total
      ${st.renting ? "" : "<br><span class=late>station hors service</span> <small>(l'opérateur ne la fait plus fonctionner : 0 vélo, 0 place, plus de nouvelles)</small>"}</p>
      ${near ? (me.rental ? `<button class="act" id="bikeAct">Rendre le vélo (E)</button>` :
        n > 0 && st.renting ? `<button class="act" id="bikeAct">Louer un vélo (E)</button>` : "") : TN.playing ? `<p class="src">Approche-toi de la station pour louer un vélo.</p>` : ""}
      ${live.bikesStale ? `<p class="src">⚠ Le flux de l'opérateur ne bouge plus depuis le ${new Date(live.bikesStale * 1000).toLocaleString("fr-FR", { day: "numeric", month: "long", hour: "2-digit", minute: "2-digit" })} : chiffres figés, les départs de cyclistes sont simulés en attendant.</p>` : ""}
      <p class="src">Disponibilités en temps réel (open data Naolib / JCDecaux). La location dans le jeu est fictive : elle ne touche à aucun vrai compte.</p>`);
    if (near && $("bikeAct")) $("bikeAct").onclick = () => bikeAction(st);
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
      <p class="src">${e.source === "pullrouge.fr" ? `Agenda <a href="https://www.pullrouge.fr/" target="_blank" rel="noopener">pullrouge.fr</a>` : "Agenda de Nantes Métropole"}</p>`);
  } else if (kind === "poi") {
    const p = d, t = p.t;
    sideBody.dataset.poi = p.id;
    const head = `<p class="kicker">${esc((p.k || "").split("=")[1]?.replace(/_/g, " "))}${t.cuisine ? " · " + esc(t.cuisine.replace(/;/g, ", ")) : ""}</p>
      <h2>${esc(t.name)}</h2>
      ${t.opening_hours ? `<p><small>Horaires (OSM) :</small> ${esc(t.opening_hours)}</p>` : ""}
      ${t.phone || t["contact:phone"] ? `<p><a href="tel:${esc(t.phone || t["contact:phone"])}">${esc(t.phone || t["contact:phone"])}</a></p>` : ""}`;
    openSide(head + `<p>…</p>`);
    // the server answers right away with its copy and, when that copy is over a day old, reads the site again
    // in the background ("refreshing"): ask once more a few seconds later
    const render = (info) => {
      let body = "";
      if (info?.image) body += `<img class="hero" src="${esc(info.image)}" alt="" onerror="this.remove()">`;
      if (info?.description) body += `<p>${esc(info.description)}</p>`;
      const price = (v) => (v != null ? `<b>${(+v).toFixed(2).replace(".", ",")} €</b>` : "");
      if (info?.sections?.length) {
        body += `<h3>La carte</h3>` + info.sections.map((s) => `${s.title ? `<h4>${esc(s.title)}</h4>` : ""}<ul class="menu">${s.items.map((it) =>
          `<li><span>${esc(it.name)}${it.desc ? `<small>${esc(it.desc)}</small>` : ""}</span>${price(it.price)}</li>`).join("")}</ul>`).join("");
      }
      if (info?.images?.length && !info?.sections?.length) {
        body += `<h3>La carte</h3><div class="menuimgs">${info.images.map((u) =>
          `<a href="${esc(u)}" target="_blank" rel="noopener"><img src="${esc(u)}" alt="Carte" loading="lazy" onerror="this.parentNode.remove()"></a>`).join("")}</div>`;
      }
      if (info?.pdfs?.length && info.source !== "pdf") {
        body += `${info?.sections?.length || info?.images?.length ? "" : "<h3>La carte</h3>"}<ul>${info.pdfs.map((u, k) =>
          `<li><a href="${esc(u)}" target="_blank" rel="noopener">Carte en PDF${info.pdfs.length > 1 ? " " + (k + 1) : ""}</a></li>`).join("")}</ul>`;
      }
      if (info?.sections?.length || info?.images?.length) {
        const when = info.fetched ? new Date(info.fetched * 1000).toLocaleDateString("fr-FR", { day: "numeric", month: "long" }) : "";
        body += `<p class="src">Lu sur <a href="${esc(info.menu_url || info.website)}" target="_blank" rel="noopener">leur site</a>${when ? " le " + when : ""} : peut être incomplet ou daté.</p>`;
      }
      if (info?.links?.length) {
        const verb = (n) => (["Uber Eats", "Deliveroo", "Just Eat"].includes(n) ? "Commander sur" : "Réserver sur");
        body += `<p class="links">${info.links.map((l) => `<a class="pill" href="${esc(l.url)}" target="_blank" rel="noopener">${verb(l.name)} ${esc(l.name)}</a>`).join(" ")}</p>`;
      }
      const site = t.website || t["contact:website"];
      if (site) body += `<p><a href="${esc(site)}" target="_blank" rel="noopener">Site web</a></p>`;
      if (info?.refreshing) body += `<p class="src">Lecture de leur site en cours…</p>`;
      if (!body) body = `<p>Pas encore d'infos sur ce lieu.</p>`;
      return body;
    };
    let info = null;
    try { info = await getJSON(`api/poi/${encodeURIComponent(p.id)}`); } catch { /* unknown place */ }
    openSide(head + render(info) + `<p class="src">© contributeurs OpenStreetMap</p>`);
    for (let k = 0; info?.refreshing && k < 4; k++) {
      await new Promise((r) => setTimeout(r, 2500 + k * 1500));
      if (sideBody.dataset.poi !== p.id) return;   // the panel shows something else now
      try { info = await getJSON(`api/poi/${encodeURIComponent(p.id)}`); } catch { break; }
      if (!info.refreshing) openSide(head + render(info) + `<p class="src">© contributeurs OpenStreetMap</p>`);
    }
    return;
  }
};
function tabs() {
  sideBody.querySelectorAll(".tabs button").forEach((b) => (b.onclick = () => {
    sideBody.querySelectorAll(".tabs button").forEach((x) => x.classList.toggle("on", x === b));
    sideBody.querySelectorAll("[data-p]").forEach((p) => (p.hidden = p.dataset.p !== b.dataset.t));
  }));
}
// (the airport board opens from its pin on the map; no toolbar button)

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
    im.src = `data/world/nav/${k}.png${TN.v}`;
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
  loadRooms();
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
  TN.playing = false; me.ride = null; room = null; live.room = null;
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
        if (!o) { o = { ...p, buf: [], color: colorFor(p.name) }; others.set(p.id, o); }
        // a buffer of timestamped states (server time); drawn a little in the past, see curPos
        const t = p.t || m.at || now / 1000, last = o.buf[o.buf.length - 1];
        if (!last || t > last.t) o.buf.push({ t, x: p.x, y: p.y, z: p.z, a: p.a });
        while (o.buf.length > 12) o.buf.shift();
        o.m = p.m; o.name = p.name;
        if (m.at) playersOff = playersOff == null ? m.at - Date.now() / 1000 : Math.min(m.at - Date.now() / 1000, playersOff + 0.002);
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
// Other players: entity interpolation (as in Source/Quake netcode). Their states carry the server time; we draw
// them DELAY behind the newest, between the two states around that moment (cubic Hermite with the velocities
// from neighbouring states, so turns are smooth and speed doesn't pulse), and when a packet is late we keep
// going along the last velocity for up to 0.4 s (dead reckoning) instead of freezing, then ease back.
let playersOff = null;                  // server clock - local clock (smallest seen: the least delayed packet)
const DELAY = 0.25;
function curPos(o) {
  const b = o.buf;
  if (!b?.length) return [o.x, o.y, o.z];
  const rt = Date.now() / 1000 + (playersOff ?? 0) - DELAY;
  let i = b.length - 1;
  while (i > 0 && b[i - 1].t > rt) i--;
  if (i === 0 || b.length === 1) { const s = b[0]; o.a = s.a; return [s.x, s.y, s.z]; }
  const s0 = b[i - 1], s1 = b[i];
  if (rt > s1.t) {                                         // starved: extrapolate a little
    const dt = Math.min(0.4, rt - s1.t), h = Math.max(0.05, s1.t - s0.t);
    o.a = s1.a;
    return [s1.x + (s1.x - s0.x) / h * dt, s1.y + (s1.y - s0.y) / h * dt, s1.z];
  }
  const h = Math.max(0.01, s1.t - s0.t), f = (rt - s0.t) / h;
  const sp = b[i - 2] || s0, sn = b[i + 1] || s1;
  const vel = (a, c, k) => (c[k] - a[k]) / Math.max(0.01, c.t - a.t) * h;   // tangents scaled to the segment
  const f2 = f * f, f3 = f2 * f;
  const herm = (k) => (2 * f3 - 3 * f2 + 1) * s0[k] + (f3 - 2 * f2 + f) * vel(sp, s1, k) + (-2 * f3 + 3 * f2) * s1[k] + (f3 - f2) * vel(s0, sn, k);
  let da = s1.a - s0.a; da = Math.atan2(Math.sin(da), Math.cos(da));
  o.a = s0.a + da * f;
  return [herm("x"), herm("y"), s0.z + (s1.z - s0.z) * f];
}
function sendPos(force) {
  if (!ws || ws.readyState !== 1) return;
  const now = performance.now();
  const moved = Math.hypot(me.x - sentAt[0], me.y - sentAt[1]) > 0.2;
  if (!force && (now - lastSend < 150 || (!moved && now - lastSend < 2000))) return;
  lastSend = now; sentAt = [me.x, me.y];
  const m = me.ride ? me.ride.kind : me.mode === "car" && me.car ? `car${me.car.model}` : me.mode;
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
  if (e.code === "Escape") { if (talking) closeTalk(); else if (!side.hidden) closeSide(); else stopPlaying(); return; }
  if (e.code === "KeyE" || e.code === "KeyF") { action(); return; }
  if (e.code === "Digit1") setMode("walk");
  else if (e.code === "Digit2") setMode("bike");
  else if (e.code === "Digit3") setMode("car");
  else if (e.code === "Digit4") setMode("heli");
  keys.add(e.code);
  if (e.code.startsWith("Arrow") || e.code === "Space") e.preventDefault();
});
addEventListener("keyup", (e) => keys.delete(e.code));
addEventListener("blur", () => keys.clear());

function setMode(m) {
  if (me.ride) return toast("Descends d'abord (E à un arrêt)");
  if (me.rental && m !== "bike") return toast(`Rends d'abord ton Naolib Vélo à une station (E)`);
  if (me.mode === "heli" && m !== "heli" && me.alt > 3) return toast("Pose-toi d'abord (C pour descendre)");
  if (m === "car" && !onRoad(me.x, me.y)) return toast("Rejoins une rue pour prendre la voiture");
  if (room && m !== "walk") leave();
  if (m === "heli" && me.mode !== "heli") { me.alt = 0; me.vx = me.vy = 0; me.climb = 60; toast("Décollage ! Espace monter · C descendre"); }
  if (m !== "heli") me.alt = 0;
  if (m === "car" && me.mode !== "car") me.car = carSpec(myCarModel());
  me.mode = m; me.v = 0; hud();
  $("altBtns").hidden = m !== "heli";
}
// mobile: climb / descend buttons
document.querySelectorAll("#altBtns button").forEach((b) => {
  const k = b.dataset.d === "1" ? "Space" : "KeyC";
  b.onpointerdown = () => keys.add(k); b.onpointerup = b.onpointerleave = () => keys.delete(k);
});
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

// What's around: the closest named place within reach, and the transit vehicle at a stop next to you.
const REACH = 16;
function nearestPlace(x, y, r = REACH) {
  let bp = null, bd = r;
  for (const p of live.world.pois) {
    if (!p.t?.name) continue;
    const d = Math.hypot(p.p[0] - x, p.p[1] - y);
    if (d < bd) { bd = d; bp = p; }
  }
  return bp;
}
let hintKey = "", jackT = null;
function updateHint() {
  const el = $("hint");
  let txt = "", key = "";
  if (room) { key = "in:" + room.id; txt = `La carte · ${room.name}`; }
  else if (TN.playing && !me.ride && me.mode === "walk" && (jackT = nearestCar())) { key = "jack:" + jackT.k + (jackT.c & 7); txt = jackLabel(jackT); }
  else if (TN.playing && !me.ride && me.mode !== "car") {
    const p = nearestPlace(me.x, me.y);
    if (p) { key = p.id; txt = `Entrer · ${p.t.name}`; }
  } else if (me.ride) { key = "ride"; txt = "Descendre au prochain arrêt"; }
  if (key === hintKey) return;
  hintKey = key;
  el.hidden = !txt;
  el.innerHTML = txt ? `<kbd>E</kbd>${esc(txt)}` : "";
}
$("hint").onclick = () => action();
// walking into a building next to a place = going in (once, until you've walked away)
let insideId = null;
function bumpInto(tx, ty) {
  if (me.mode !== "walk" || room) return;
  const n = navAt(tx, ty);
  if (!n || !(n.f & F_BUILDING)) return;
  const p = nearestPlace(tx, ty, 12);
  if (!p || p.id === insideId) return;
  enter(p, tx, ty);
}

// ---------------------------------------------------------------- inside a place
// The building (BD TOPO footprint, rooms.json) opens like a doll's house: the rest of it is ghosted, its ground
// floor drawn with a floor, the back walls and furniture that depends on the kind of place; you walk inside and
// leave through any wall.
let rooms = null, room = null;
async function loadRooms() {
  if (rooms) return rooms;
  try { rooms = await getJSON("data/world/rooms.json"); } catch { rooms = {}; }
  return rooms;
}
function inRing(r, x, y) {
  let c = false;
  for (let i = 0, j = r.length - 1; i < r.length; j = i++) {
    const [x1, y1] = r[i], [x2, y2] = r[j];
    if ((y1 > y) !== (y2 > y) && x < x1 + (y - y1) * (x2 - x1) / (y2 - y1)) c = !c;
  }
  return c;
}
function edgeDist(r, x, y) {
  let best = 1e9;
  for (let i = 0, j = r.length - 1; i < r.length; j = i++) {
    const [x1, y1] = r[j], [x2, y2] = r[i], dx = x2 - x1, dy = y2 - y1;
    const t = Math.max(0, Math.min(1, ((x - x1) * dx + (y - y1) * dy) / (dx * dx + dy * dy || 1)));
    best = Math.min(best, Math.hypot(x - x1 - t * dx, y - y1 - t * dy));
  }
  return best;
}
const KIND_STYLE = (k) => {
  const v = (k || "").split("=")[1] || "";
  if (/^(bar|pub|nightclub|biergarten)$/.test(v)) return { kind: "bar", floor: "#6b4a32", wall: "#3d3a46" };
  if (/^(restaurant|fast_food|food_court)$/.test(v)) return { kind: "resto", floor: "#9a6d45", wall: "#e8dcc4" };
  if (/^(cafe|ice_cream|bakery|pastry)$/.test(v)) return { kind: "cafe", floor: "#c9b49a", wall: "#efe6d6" };
  if ((k || "").startsWith("shop=")) return { kind: "shop", floor: "#d9d4ca", wall: "#f2efe8" };
  return { kind: "hall", floor: "#b8ad9c", wall: "#e9e4da" };
};
// furniture: [x, y, ang, L, W, H, top colour, side colour]
function furnish(r, z, st) {
  const out = [];
  const xs = r.map((p) => p[0]), ys = r.map((p) => p[1]);
  // longest edge: the counter / shelves run along it
  let le = 0, la = 0, lm = null;
  for (let i = 0, j = r.length - 1; i < r.length; j = i++) {
    const L = Math.hypot(r[i][0] - r[j][0], r[i][1] - r[j][1]);
    if (L > le) { le = L; la = Math.atan2(r[i][1] - r[j][1], r[i][0] - r[j][0]); lm = [(r[i][0] + r[j][0]) / 2, (r[i][1] + r[j][1]) / 2]; }
  }
  const cx = xs.reduce((a, b) => a + b) / xs.length, cy = ys.reduce((a, b) => a + b) / ys.length;
  // the counter: 1 m inside the longest wall, towards the middle
  if (lm && st.kind !== "hall") {
    const nx = cx - lm[0], ny = cy - lm[1], n = Math.hypot(nx, ny) || 1;
    const px = lm[0] + nx / n * 1.2, py = lm[1] + ny / n * 1.2, L = Math.min(le - 2, st.kind === "bar" ? 8 : 4);
    if (L > 1.5 && inRing(r, px, py)) out.push([px, py, la, L, 0.7, 1.05, "#d8c7a8", st.kind === "bar" ? "#5a3b28" : "#8a6a4c"]);
  }
  const cos = Math.cos(la), sin = Math.sin(la);
  const step = st.kind === "shop" ? 2.4 : 2.6;
  for (let a = -40; a <= 40; a += step) for (let b = -40; b <= 40; b += step) {
    const x = cx + a * cos - b * sin, y = cy + a * sin + b * cos;
    if (!inRing(r, x, y) || edgeDist(r, x, y) < 1.3) continue;
    if (out.some((f) => Math.hypot(f[0] - x, f[1] - y) < 1.8)) continue;
    if (st.kind === "shop") out.push([x, y, la, 1.8, 0.5, 1.6, "#f4f1ea", "#b9b2a6"]);            // shelf
    else if (st.kind === "hall") out.push([x, y, la, 1.2, 0.6, 0.8, "#cfc6b6", "#8f8576"]);
    else {
      const round = st.kind === "cafe";
      out.push([x, y, la, round ? 0.7 : 0.9, round ? 0.7 : 0.9, 0.75, "#e9e2d2", "#7a5a3e"]);       // table
      for (const s of [-1, 1]) out.push([x + s * 0.75 * cos, y + s * 0.75 * sin, la, 0.42, 0.42, 0.48, "#a8402f", "#6e2a20"]);  // chairs
    }
    if (out.length > 90) break;
  }
  return out;
}
async function enter(p, tx, ty) {
  const R = (await loadRooms())[p.id];
  insideId = p.id;
  me.v = 0;
  live.open("poi", p);
  if (!R) { toast(`${p.t.name}`); return; }
  const st = KIND_STYLE(p.k);
  room = { id: p.id, name: p.t.name, r: R.r, z: R.z ?? me.z, h: R.h || 9, st, furniture: furnish(R.r, R.z ?? me.z, st) };
  live.room = room;
  // step in: from where you bumped, 1.5 m towards the middle
  const cx = R.r.reduce((a, q) => a + q[0], 0) / R.r.length, cy = R.r.reduce((a, q) => a + q[1], 0) / R.r.length;
  let x = tx ?? me.x, y = ty ?? me.y;
  for (let k = 0; k < 20 && !(inRing(R.r, x, y) && edgeDist(R.r, x, y) > 0.8); k++) { x += (cx - x) * 0.15; y += (cy - y) * 0.15; }
  me.x = x; me.y = y; me.z = room.z;
  toast(`Tu entres : ${p.t.name}`);
}
function leave() {
  if (!room) return;
  toast(`Tu sors de ${room.name}`);
  room = null; live.room = null;
}
live.under.push(() => {
  if (!room || !TN.playing) return;
  const c = live.ctx, { r, z, h, st } = room, wallH = Math.min(3.4, h);
  const P = (x, y, zz) => live.toCanvas(...live.project(x, y, zz));
  const poly = (pts) => { c.beginPath(); pts.forEach((q, k) => (k ? c.lineTo(...q) : c.moveTo(...q))); c.closePath(); };
  const lo = r.map(([x, y]) => P(x, y, z)), top = r.map(([x, y]) => P(x, y, z + h)), mid = r.map(([x, y]) => P(x, y, z + wallH));
  // 1. the building above the ground floor: ghosted (its roof and walls painted on the tile disappear)
  c.save();
  c.fillStyle = "rgba(226,234,242,0.8)";                          // frosted glass
  poly(top); c.fill(); poly(lo); c.fill();
  for (let i = 0, j = r.length - 1; i < r.length; j = i++) {
    c.beginPath(); c.moveTo(...lo[j]); c.lineTo(...lo[i]); c.lineTo(...top[i]); c.lineTo(...top[j]); c.closePath(); c.fill();
  }
  c.strokeStyle = "rgba(60,80,100,0.5)"; c.lineWidth = 1 * TN.dpr; c.setLineDash([4 * TN.dpr, 4 * TN.dpr]);
  poly(top); c.stroke();
  for (let i = 0; i < r.length; i++) { c.beginPath(); c.moveTo(...lo[i]); c.lineTo(...top[i]); c.stroke(); }
  c.setLineDash([]);
  // 2. the floor, with planks / tiles
  c.fillStyle = st.floor; poly(lo); c.fill();
  c.save(); poly(lo); c.clip();
  c.strokeStyle = "rgba(0,0,0,0.12)"; c.lineWidth = 1 * TN.dpr;
  const xs = r.map((q) => q[0]), ys = r.map((q) => q[1]);
  for (let x = Math.floor(Math.min(...xs)); x < Math.max(...xs); x += st.kind === "shop" ? 1 : 0.5) {
    c.beginPath(); c.moveTo(...P(x, Math.min(...ys), z)); c.lineTo(...P(x, Math.max(...ys), z)); c.stroke();
  }
  if (st.kind === "shop") for (let y = Math.floor(Math.min(...ys)); y < Math.max(...ys); y += 1) {
    c.beginPath(); c.moveTo(...P(Math.min(...xs), y, z)); c.lineTo(...P(Math.max(...xs), y, z)); c.stroke();
  }
  c.restore();
  // 3. walls: the far ones full height (one storey), the near ones cut low so you see in
  const back = TN.man.axes[2];
  const ccw = r.reduce((a, q, i) => { const n = r[(i + 1) % r.length]; return a + (q[0] * n[1] - n[0] * q[1]); }, 0) > 0;
  for (let i = 0, j = r.length - 1; i < r.length; j = i++) {
    const a = r[j], b = r[i];
    let nx = b[1] - a[1], ny = a[0] - b[0];                       // outward normal for a counter-clockwise ring
    if (!ccw) { nx = -nx; ny = -ny; }
    const far = nx * back[0] + ny * back[1] > 0;                    // facing away from the camera: a back wall
    const hh = far ? wallH : 0.5;
    const A = P(a[0], a[1], z), B = P(b[0], b[1], z), C = P(b[0], b[1], z + hh), D = P(a[0], a[1], z + hh);
    c.fillStyle = far ? live.shade(st.wall, 0.82 + 0.15 * Math.abs(nx) / (Math.hypot(nx, ny) || 1)) : live.shade(st.wall, 0.55);
    c.beginPath(); c.moveTo(...A); c.lineTo(...B); c.lineTo(...C); c.lineTo(...D); c.closePath(); c.fill();
    c.strokeStyle = "rgba(0,0,0,0.25)"; c.stroke();
  }
  c.restore();
});
// furniture and the warm light, as items so people sort among the tables
live.layers.push(() => {
  if (!room || !TN.playing) return [];
  return room.furniture.map(([x, y, a, L, W, H, top, side]) => {
    const [sx, sy] = live.project(x, y, room.z);
    return { sx, sy, h: H, room: true, draw: () => live.box(x, y, room.z, a, L, W, H, top, side) };
  });
});

// ---------------------------------------------------------------- talking with the inhabitants (PNJ)
function nearestSim(r = 6) {
  let best = null, bd = r;
  for (const [id, k, x, y] of live.people) {
    if (k !== 1 && k !== 2 && k !== 4) continue;
    const d = Math.hypot(x - me.x, y - me.y);
    if (d < bd) { bd = d; best = { id, k, x, y }; }
  }
  return best;
}
let talking = null;                         // { id, name, busy }
async function openTalk(id) {
  if (!session) return toast("Connecte-toi pour parler aux habitants");
  talking = { id, name: "", busy: true };
  openSide(`<p class="kicker">💬 Conversation</p><h2 id="talkWho">…</h2><div class="talk" id="talkLog"></div>
    <form id="talkForm" class="talkform"><input id="talkIn" maxlength="400" placeholder="Dis quelque chose…" autocomplete="off">
    <button class="act">Dire</button></form>
    <p class="src">Personnage non joueur ; ses répliques sont générées par une IA. <kbd>Échap</kbd> pour partir.</p>`);
  sideBody.dataset.talk = "1";
  $("talkForm").onsubmit = (e) => { e.preventDefault(); const t = $("talkIn").value.trim(); if (t) { $("talkIn").value = ""; talkSay(t); } };
  $("talkIn").addEventListener("keydown", (e) => { if (e.code === "Escape") { e.preventDefault(); closeTalk(); } });
  await talkSay("");
  $("talkIn")?.focus();
}
function talkLine(who, text, cls) {
  const log = $("talkLog"); if (!log) return;
  const p = document.createElement("p"); p.className = cls;
  p.innerHTML = `<b>${esc(who)}</b> ${esc(text)}`; log.appendChild(p); log.scrollTop = log.scrollHeight;
}
async function talkSay(text) {
  if (!talking) return;
  const t = talking;
  if (text) talkLine("Toi :", text, "me");
  t.busy = true; $("talkIn") && ($("talkIn").disabled = true);
  try {
    const r = await fetch("api/talk", { method: "POST", headers: { "content-type": "application/json" },
      body: JSON.stringify({ token: session.token, id: t.id, text }) });
    const d = await r.json();
    if (!r.ok) { talkLine("", d.error || "…", "sys"); if (r.status !== 429) talking = null; return; }
    t.name = d.name; if ($("talkWho")) $("talkWho").textContent = d.name;
    talkLine(`${d.name.split(" ")[0]} :`, d.text, "them");
    if (d.ended) { talkLine("", `${d.name.split(" ")[0]} ${d.snubbed ? "se détourne et s'en va." : "s'en va."}`, "sys"); talking = null; $("talkForm")?.remove(); }
  } catch { talkLine("", "…pas de réponse.", "sys"); }
  finally { t.busy = false; if ($("talkIn")) { $("talkIn").disabled = false; $("talkIn").focus(); } }
}
function closeTalk() {
  if (talking && session) fetch("api/talk/end", { method: "POST", headers: { "content-type": "application/json" },
    body: JSON.stringify({ token: session.token, id: talking.id }) }).catch(() => {});
  talking = null; delete sideBody.dataset.talk; closeSide();
}
// walking away ends the conversation
setInterval(() => {
  if (!TN.playing) return;
  if (talking) {
    const p = live.people.find((q) => q[0] === talking.id);
    if (!p || Math.hypot(p[2] - me.x, p[3] - me.y) > 30) closeTalk();
  }
  const h = $("talkHint"), s = !talking && !me.ride && nearestSim();
  if (h) h.hidden = !s;
}, 400);

// ---------------------------------------------------------------- Naolib Vélo (pretend rentals)
function nearestBikeStation(r = 14) {
  let best = null, bd = r;
  for (const st of live.bikeStations) {
    const d = Math.hypot(st.p[0] - me.x, st.p[1] - me.y);
    if (d < bd) { bd = d; best = st; }
  }
  return best;
}
function bikeAction(st) {
  if (me.rental) {
    if (st.docks - (live.returned[st.id] || 0) <= 0) return toast("Station pleine, essaie la suivante");
    live.returned[st.id] = (live.returned[st.id] || 0) + 1;
    const min = Math.max(1, Math.round((Date.now() - me.rental.t0) / 60000));
    toast(`Vélo rendu à ${st.name} (${min} min). Location fictive, rien n'est facturé 😉`);
    me.rental = null; setMode("walk"); closeSide(); return;
  }
  const n = st.bikes - (live.rented[st.id] || 0) + (live.returned[st.id] || 0);
  if (!st.renting || n <= 0) return toast("Plus de vélo ici");
  if (me.mode !== "walk") return toast("Descends d'abord");
  live.rented[st.id] = (live.rented[st.id] || 0) + 1;
  me.rental = { id: st.id, name: st.name, t0: Date.now() };
  setMode("bike"); closeSide();
  toast(`Naolib Vélo loué à ${st.name} ! Rends-le à n'importe quelle station (E). C'est pour de faux, ton vrai compte n'est pas touché.`);
}

// E: get on / off transit, talk to someone, or go into the place in front of you
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
  if (me.mode === "walk" && !room) {
    const car = nearestCar();
    if (car) { carjack(car); return; }
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
  // a Naolib Vélo station: rent, or bring the bike back
  const bs = nearestBikeStation();
  if (bs && (me.mode === "walk" || me.rental)) { bikeAction(bs); return; }
  // someone right next to you: talk
  const sim = nearestSim();
  if (sim && !room) { openTalk(sim.id); return; }
  // places: the closest one within reach
  const bp = nearestPlace(me.x, me.y, REACH + 4);
  if (room) { live.open("poi", live.world.pois.find((q) => q.id === room.id)); return; }
  if (bp) { if (me.mode === "walk") enter(bp); else { insideId = bp.id; live.open("poi", bp); } return; }
  if (Math.hypot(HELIPORT[0] - me.x, HELIPORT[1] - me.y) < 40 && me.mode !== "heli") { setMode("heli"); return; }
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
    if (performance.now() < (me.dazed || 0)) ux = uy = 0;                // just thrown out of a burning car
    const slow = keys.has("ShiftLeft") || keys.has("ShiftRight") || (joy && Math.hypot(...joy) < 0.45);
    const run = !slow;
    if (me.mode === "heli") flyHeli(ux, uy, dt);
    else if (me.mode === "car") driveCar(ux, uy, dt);
    else {
      const mag = Math.min(1, Math.hypot(ux, uy));
      if (mag > 0.05) {
        const [wx, wy] = screenToWorldDir(ux, uy);
        const vmax = me.mode === "bike" ? SPEED.bike : run ? SPEED.run : SPEED.walk;
        me.v = Math.min(vmax * mag, me.v + (me.mode === "bike" ? 9 : 14) * dt);
        me.a = Math.atan2(wy, wx);
        if (solidAt(me.x + wx * me.v * dt, me.y + wy * me.v * dt, me.mode === "bike" ? 0.8 : 0.4)) me.v = 0;   // a car, a tram: in the way
        else if (!moveBy(wx * me.v * dt, wy * me.v * dt) || !walkable(me.mode, me.x + wx * 0.6, me.y + wy * 0.6))
          bumpInto(me.x + wx * 0.8, me.y + wy * 0.8);
      } else me.v = Math.max(0, me.v - 8 * dt);
    }
    me.z = room ? room.z : groundZ(me.x, me.y) + (me.mode === "heli" ? me.alt : 0);
  }
  const [sx, sy] = live.project(me.x, me.y, me.z);
  const v = TN.view, k = 1 - Math.exp(-dt * 6);
  let jx = 0, jy = 0;
  if (TN.shake > 0.1) {                                           // a crash shakes the view
    jx = (Math.random() - 0.5) * TN.shake / v.s; jy = (Math.random() - 0.5) * TN.shake / v.s;
    TN.shake *= Math.exp(-dt * 5);
  }
  TN.follow(v.x + (sx - v.x) * k + jx, v.y + (sy - v.y) * k + jy);
  sendPos();
  if (now - (step.hudAt || 0) > 500) { step.hudAt = now; hud(); }
  if (now - (step.hintAt || 0) > 250) {
    step.hintAt = now; updateHint();
    if (insideId) {                                  // walked away: the next bump enters again
      const p = live.world.pois.find((q) => q.id === insideId);
      if (!p || Math.hypot(p.p[0] - me.x, p.p[1] - me.y) > 25) insideId = null;
    }
  }
}
function moveBy(dx, dy) {
  const m = me.mode;
  if (room) {
    const nx = me.x + dx, ny = me.y + dy;
    if (inRing(room.r, nx, ny)) { me.x = nx; me.y = ny; return true; }
    if (walkable(m, nx, ny) || walkable(m, nx + dx * 4, ny + dy * 4)) { me.x = nx; me.y = ny; leave(); return true; }
    return false;
  }
  if (!walkable(m, me.x, me.y)) { me.x += dx; me.y += dy; return true; }   // stuck inside: let them walk out
  if (walkable(m, me.x + dx, me.y + dy)) { me.x += dx; me.y += dy; return true; }
  if (walkable(m, me.x + dx, me.y)) { me.x += dx; return true; }      // slide along walls
  if (walkable(m, me.x, me.y + dy)) { me.y += dy; return true; }
  return false;
}
// GTA-style: up/down = throttle/brake-reverse, left/right = steer. Each model drives its own way (top speed,
// pick-up, brakes, turning); everything hard stops the car (buildings, other cars, trams, buses, wrecks), and a
// hit faster than FATAL blows it up.
function driveCar(ux, uy, dt) {
  const c = me.car || (me.car = carSpec(myCarModel()));
  const throttle = -uy, steer = -ux;
  if (throttle > 0) me.v += (me.v < 0 ? c.brake : c.acc * (1 - 0.55 * Math.max(0, me.v) / c.vmax)) * throttle * dt;
  else if (throttle < 0) me.v += (me.v > 0 ? c.brake : 4) * throttle * dt;
  else me.v -= Math.sign(me.v) * Math.min(Math.abs(me.v), 1.5 * dt);
  // off the carriageway (pavement, square, park): walking pace-ish; probe a little ahead so 1-cell gaps don't brake
  const ahead = onRoad(me.x, me.y) || onRoad(me.x + Math.cos(me.a) * 2, me.y + Math.sin(me.a) * 2);
  const lim = ahead ? c.vmax : 6;
  if (me.v > lim) me.v = Math.max(lim, me.v - 18 * dt);
  me.v = Math.max(-5, Math.min(c.vmax, me.v));
  me.a += steer * dt * c.turn * Math.min(1, Math.abs(me.v) / 4) * Math.sign(me.v || 1) / (1 + Math.abs(me.v) / 30);
  const dx = Math.cos(me.a) * me.v * dt, dy = Math.sin(me.a) * me.v * dt;
  const nx = me.x + dx, ny = me.y + dy;
  // a wall: the bumper (front or back, by the way we go) runs into a building
  const dir = Math.sign(me.v || 1), bx = Math.cos(me.a) * dir * c.L / 2, by = Math.sin(me.a) * dir * c.L / 2;
  const sx = -Math.sin(me.a) * c.W * 0.4, sy = Math.cos(me.a) * c.W * 0.4;
  const wall = walkable("car", me.x, me.y) && [[bx + sx, by + sy], [bx - sx, by - sy], [bx, by]]
    .some(([u, v]) => !walkable("car", nx + u, ny + v));
  if (wall) {
    if (Math.abs(me.v) > FATAL) return crash(null);
    if (Math.abs(me.v) > 4) shake(Math.abs(me.v));
    me.v *= -0.25; return;
  }
  // other vehicles, people
  const hit = collide(nx, ny, me.a, c.L, c.W);
  if (hit) {
    const s = hit.s;
    if (s.soft) {
      if (s.kind !== "down" && Math.abs(me.v) > KNOCK) { knock(s); me.v *= 0.85; }
      else if (s.kind !== "down") { me.v = 0; return; }       // slowly: they step aside, we wait
    } else {
      // closing speed along the contact normal, their own motion included
      const vx = Math.cos(me.a) * me.v - (s.vx || 0), vy = Math.sin(me.a) * me.v - (s.vy || 0);
      const closing = -(vx * hit.nx + vy * hit.ny);
      if (closing > FATAL && s.kind !== "player") return crash(s);
      if (closing > 4) shake(closing);
      me.v *= -0.3;
      me.x += hit.nx * Math.min(0.5, hit.depth); me.y += hit.ny * Math.min(0.5, hit.depth);   // pushed back out
      return;
    }
  }
  me.x = nx; me.y = ny;
}

// ---------------------------------------------------------------- physics: vehicles, collisions, crashes
const FATAL = 15;             // m/s (54 km/h): a hit on something hard above this, and the car blows up
const KNOCK = 4;              // m/s: hitting someone on foot faster than this knocks them down
// the player's cars: the inhabitants' models (live.js TRAFFIC, the variant byte of their cars), each with its handling
// the services' vehicles, once taken (models 16..20): ambulance, police, fire engine, tow truck, street sweeper
const SERVICE_CARS = [
  { name: "ems_ambulance", L: 5.6, W: 1.95, vmax: 40, acc: 8, brake: 11, turn: 1.0, label: "ambulance", siren: true },
  { name: "ems_police", L: 4.5, W: 1.8, vmax: 47, acc: 12, brake: 14, turn: 1.35, label: "voiture de police", siren: true },
  { name: "ems_fire", L: 6.4, W: 2.2, vmax: 31, acc: 4.5, brake: 8, turn: 0.8, label: "camion de pompiers", siren: true },
  { name: "tow", L: 6.6, W: 2.3, vmax: 30, acc: 5, brake: 9, turn: 0.85, label: "dépanneuse de la fourrière" },
  { name: "sweeper", L: 5.2, W: 2.1, vmax: 16, acc: 3, brake: 8, turn: 1.0, label: "balayeuse" },
];
function carSpec(model) {
  if (model >= 16 && model < 16 + SERVICE_CARS.length) return { model, service: model - 16, ...SERVICE_CARS[model - 16] };
  const T = live.TRAFFIC[model % live.TRAFFIC.length], n = T[0];
  const h = /truck/.test(n) ? { vmax: 24, acc: 3.8, brake: 7, turn: 0.75, label: "camion" }
    : /van/.test(n) ? { vmax: 29, acc: 5.5, brake: 9, turn: 0.95, label: "utilitaire" }
    : /suv/.test(n) ? { vmax: 36, acc: 8.5, brake: 12, turn: 1.1, label: "SUV" }
    : /taxi/.test(n) ? { vmax: 38, acc: 9.5, brake: 13, turn: 1.25, label: "taxi" }
    : /sedan/.test(n) ? { vmax: 42, acc: 10.5, brake: 13, turn: 1.2, label: "berline" }
    : { vmax: 36, acc: 11, brake: 13, turn: 1.45, label: "citadine" };
  return { model: model % live.TRAFFIC.length, name: n, L: T[1], W: T[2], ...h };
}
function myCarModel() {                                     // your own car: always the same one for your name
  const hsh = [...String(session?.name)].reduce((h, c) => (h * 31 + c.charCodeAt(0)) >>> 0, 0);
  return Math.max(0, live.TRAFFIC.findIndex((t) => t[0] === live.SPRITE_CARS[hsh % 8]));
}
// what's around, for a few frames: the inhabitants' vehicles and people, trams and buses, other players
let solidsCache = { t: 0, x: 1e9, y: 1e9, list: [] };
function solidsNear(x, y) {
  const now = performance.now();
  if (now - solidsCache.t > 60 || Math.hypot(x - solidsCache.x, y - solidsCache.y) > 8) {
    const list = live.solids(x, y, 40);
    for (const o of others.values()) {
      const [ox, oy] = curPos(o, now);
      if (Math.abs(ox - x) > 40 || Math.abs(oy - y) > 40 || o.m === "heli" || ["tram", "bus", "navibus"].includes(o.m)) continue;
      const car = /^car/.test(o.m || "");
      list.push({ x: ox, y: oy, ang: o.a, L: car ? 4.3 : 0.5, W: car ? 1.8 : 0.5, kind: "player", soft: !car });
    }
    solidsCache = { t: now, x, y, list };
  }
  return solidsCache.list;
}
// a box as discs along its length
function discs(x, y, ang, L, W) {
  const n = Math.max(1, Math.round(L / W)), r = W / 2, c = Math.cos(ang), s = Math.sin(ang), out = [];
  for (let k = 0; k < n; k++) {
    const u = n === 1 ? 0 : -(L - W) / 2 + (L - W) * k / (n - 1);
    out.push([x + c * u, y + s * u, Math.max(r, n === 1 ? L / 2 : r)]);
  }
  return out;
}
// the deepest overlap of a box at (x, y) with anything around: { s, nx, ny (pushing us out), depth }
function collide(x, y, ang, L, W) {
  const mine = discs(x, y, ang, L, W);
  let best = null;
  for (const s of solidsNear(x, y)) {
    if (Math.abs(s.x - x) > 16 || Math.abs(s.y - y) > 16) continue;
    for (const [ax, ay, ar] of mine) for (const [bx, by, br] of discs(s.x, s.y, s.ang, s.L, s.W)) {
      const dx = ax - bx, dy = ay - by, d = Math.hypot(dx, dy), depth = ar + br - d;
      if (depth > 0 && (!best || depth > best.depth)) best = { s, nx: d > 1e-3 ? dx / d : 1, ny: d > 1e-3 ? dy / d : 0, depth };
    }
  }
  return best;
}
// walking / cycling: is something hard (a car, a tram) at (x, y)?
function solidAt(x, y, r) {
  for (const s of solidsNear(x, y)) {
    if (s.soft || Math.abs(s.x - x) > 10 || Math.abs(s.y - y) > 10) continue;
    for (const [bx, by, br] of discs(s.x, s.y, s.ang, s.L, s.W)) if (Math.hypot(x - bx, y - by) < r + br) {
      // already inside it (it drove into us): let us walk out
      return !discs(s.x, s.y, s.ang, s.L, s.W).some(([cx, cy, cr]) => Math.hypot(me.x - cx, me.y - cy) < r + cr);
    }
  }
  return false;
}
function shake(v) { TN.shake = Math.min(14, v * 0.6); }
async function impact(body) {
  if (!session) return null;
  try {
    const r = await fetch("api/impact", { method: "POST", headers: { "content-type": "application/json" },
      body: JSON.stringify({ token: session.token, ...body }) });
    return r.ok ? r.json() : null;
  } catch { return null; }
}
// the car (or helicopter) blows up: you're thrown out on foot next to the wreck; the city sends police, ambulance,
// fire engine, then the tow truck, and the street cleaners the next day
function crash(s, heli) {
  const x = me.x, y = me.y, a = me.a;
  const model = heli ? 64 : (me.car?.model ?? 0);
  live.boom(x, y, me.z);
  shake(20);
  me.mode = "walk"; me.v = 0; me.car = null; me.alt = 0; me.vx = me.vy = 0;
  $("altBtns").hidden = true;
  for (const side of [1, -1, 2, -2]) {                     // out on the side that isn't a wall
    const ox = x - Math.sin(a) * 3 * side, oy = y + Math.cos(a) * 3 * side;
    if (walkable("walk", ox, oy)) { me.x = ox; me.y = oy; break; }
  }
  me.dazed = performance.now() + 2500;
  toast(heli ? "Crash ! L'hélico s'est écrasé. Les secours arrivent." : "Boum ! La voiture a explosé. Les secours arrivent.");
  hud();
  impact({ kind: "crash", x, y, a, model, id: s?.id });
}
const knocked = new Set();
function knock(s) {
  if (knocked.has(s.id)) return;
  knocked.add(s.id);
  shake(5);
  impact({ kind: "hit", id: s.id, x: me.x, y: me.y });
}
// something to take, next to you on foot: a car or a service vehicle (6 m), a bike (3 m)
function nearestCar() {
  let best = null, bd = Infinity;
  for (const [id, k, x, y, , , , c] of live.people) {
    if (k !== 0 && k !== 2 && k !== 3) continue;
    const d = Math.hypot(x - me.x, y - me.y);
    if (d > (k === 2 ? 3 : 6)) continue;
    if (d < bd) { bd = d; best = { id, k, x, y, c }; }
  }
  return best;
}
const jackLabel = (t) => t.k === 2 ? "Piquer le vélo" : t.k === 3 ? `Voler ${["l'ambulance", "la voiture de police", "le camion de pompiers", "la dépanneuse", "la balayeuse"][Math.min(4, t.c & 7)]}` : "Voler la voiture";
async function carjack(car) {
  const j = await impact({ kind: "jack", id: car.id, x: me.x, y: me.y });
  if (!j || j.model == null) return toast("Raté, il est parti");
  if (room) leave();
  me.x = j.x; me.y = j.y; me.a = j.a; me.v = 0;
  if (j.model === 255) { me.mode = "bike"; me.car = null; toast("Tu piques le vélo ! Son propriétaire s'enfuit en criant."); }
  else {
    me.mode = "car"; me.car = carSpec(j.model);
    toast(me.car.service != null ? `Tu prends ${["l'ambulance", "la voiture de police", "le camion de pompiers", "la dépanneuse", "la balayeuse"][me.car.service]} !${me.car.siren ? " Gyrophare allumé." : ""}` : `Tu prends le volant (${me.car.label}) ! Le conducteur s'enfuit.`);
  }
  hud();
}

// Helicopter: free flight with inertia (screen-relative like walking), Space / C to climb / descend
function flyHeli(ux, uy, dt) {
  const mag = Math.min(1, Math.hypot(ux, uy));
  const up = keys.has("Space") ? 1 : keys.has("KeyC") || keys.has("ControlLeft") ? -1 : 0;
  if (me.climb != null) { if (up || me.alt >= me.climb) me.climb = null; }
  const vz = me.climb != null ? 14 : up * 14;
  me.alt = Math.max(0, Math.min(400, me.alt + vz * dt));
  const air = me.alt > 1.5;
  let tx = 0, ty = 0;
  if (mag > 0.05 && air) { const [wx, wy] = screenToWorldDir(ux, uy); tx = wx * SPEED.heli * mag; ty = wy * SPEED.heli * mag; }
  const k = 1 - Math.exp(-dt * (mag > 0.05 ? 0.9 : 0.6));          // inertia: ~1 s to pick up speed, a bit longer to stop
  me.vx += (tx - me.vx) * k; me.vy += (ty - me.vy) * k;
  if (!air) { me.vx = me.vy = 0; }
  me.x += me.vx * dt; me.y += me.vy * dt;
  me.v = Math.hypot(me.vx, me.vy);
  // a building under us taller than our altitude: fast, we crash into it; slowly, we stop on its roof
  if (me.alt > 1.5) {
    const n = navAt(me.x, me.y);
    if (n && n.f & F_BUILDING) {
      const [sx, sy] = live.project(me.x, me.y, groundZ(me.x, me.y) + me.alt), h = live.heightAt(sx, sy);
      if (h && h >= me.alt - 1.5) {
        if (me.v > 6 || vz < -8) return crash(null, true);
        me.alt = Math.max(me.alt, h + 0.5);
        me.vx *= -0.3; me.vy *= -0.3;
      }
    }
  }
  if (me.v > 1) { let da = Math.atan2(me.vy, me.vx) - me.a; da = Math.atan2(Math.sin(da), Math.cos(da)); me.a += da * Math.min(1, dt * 3); }
}
// Attitude of a helicopter from how it moves (works the same for us and for other players): it pitches nose
// down with speed and when it accelerates, nose up when it brakes, and banks into turns (lateral acceleration).
const HELI_PITCH = [-7, 0, 8, 16];                                   // the painted attitudes (sprites heli_0..3)
function heliAttitude(o, x, y, a) {
  const t = performance.now() / 1000, st = o._att || (o._att = { x, y, a, t, v: 0, pitch: 0, roll: 0 });
  const dt = Math.min(0.25, t - st.t);
  if (dt > 0.001) {
    const vx = (x - st.x) / dt, vy = (y - st.y) / dt, v = Math.hypot(vx, vy);
    let da = a - st.a; da = Math.atan2(Math.sin(da), Math.cos(da));
    const acc = (v - st.v) / dt, turn = da / dt;
    const k = 1 - Math.exp(-dt * 2.5);
    const pitch = Math.max(-9, Math.min(18, v / SPEED.heli * 13 + acc * 0.9));
    const roll = Math.max(-28, Math.min(28, Math.atan(v * turn / 9.81) * 180 / Math.PI));
    st.pitch += (pitch - st.pitch) * k; st.roll += (roll - st.roll) * k;
    st.v += (v - st.v) * Math.min(1, dt * 6);
    Object.assign(st, { x, y, a, t });
  }
  return st;
}
function heliModel(o, x, y, z, a, col, isMe) {
  const g = live.zAt(x, y), S = 2.2;                                // drawn larger than life (like people) to read on the map
  const att = heliAttitude(o, x, y, a);
  const [gx, gy] = live.project(x, y, g), [ax, ay] = live.project(x, y, z);
  // nearest painted attitude
  let best = 0;
  HELI_PITCH.forEach((p, k) => { if (Math.abs(p - att.pitch) < Math.abs(HELI_PITCH[best] - att.pitch)) best = k; });
  // banking shows as a rotation on screen when the helicopter faces towards / away from the camera
  const [dx, dy] = live.project(x + Math.cos(a), y + Math.sin(a), z).map((v, k) => v - [ax, ay][k]);
  const facing = Math.abs(dy) / (Math.hypot(dx, dy) || 1);
  const rot = att.roll * Math.PI / 180 * (0.25 + 0.75 * facing) * (dy > 0 ? -1 : 1);
  const flying = z - g > 0.5;
  return [
    { sx: gx, sy: gy, h: 0, draw: () => {                            // shadow on the ground, smaller as it climbs
      const c = live.ctx, [cx, cy] = live.toCanvas(gx, gy), s = TN.view.s * TN.dpr * S, f = Math.max(0.5, 1 - (z - g) / 300);
      c.fillStyle = `rgba(0,0,0,${0.24 * f})`; c.beginPath(); c.ellipse(cx, cy, 5 * s * f, 2.4 * s * f, 0, 0, 7); c.fill(); } },
    { sx: ax, sy: ay, h: 3 * S, room: true, overlay: z - g > 12, draw: () => {
      const c = live.ctx;
      if (!live.sprite(`heli_${best}`, x, y, z, a, "h:" + (isMe ? "me" : o.name), { scale: S, rot })) return;
      // main rotor: pixel blades over the hub. In the sprite model the hub sits 1.5 m ahead of and 1.64 m above
      // the pitch pivot (1.2 m up), and the model is centred on the pivot (sprites.helicopter).
      const p = HELI_PITCH[best] * Math.PI / 180, cp = Math.cos(p), sp = Math.sin(p);
      const hubF = (1.5 * cp + 1.64 * sp) * S, hubZ = (-1.5 * sp + 1.64 * cp + 1.25) * S;
      const [hx, hy] = live.toCanvas(...live.project(x + Math.cos(a) * hubF, y + Math.sin(a) * hubF, z + hubZ));
      const sc = TN.view.s * TN.dpr * S, R = 5.2 * sc, px = Math.max(1, Math.round(0.32 * sc));
      const spin = performance.now() / 1000 * (flying ? 26 : 5);
      c.save(); c.translate(Math.round(hx), Math.round(hy)); c.rotate(rot);
      c.fillStyle = "rgba(30,32,36,0.16)"; c.beginPath(); c.ellipse(0, 0, R, R * 0.7, 0, 0, 7); c.fill();
      c.fillStyle = "rgba(28,30,34,0.9)";
      for (let k = 0; k < 4; k++) {                                   // blades as runs of square pixels
        const b = spin + k * Math.PI / 2, ux = Math.cos(b), uy = Math.sin(b) * 0.7;
        for (let r = px; r < R; r += px) c.fillRect(Math.round(ux * r / px) * px - px / 2, Math.round(uy * r / px) * px - px / 2, px, px);
      }
      c.fillRect(-px, -px, 2 * px, 2 * px);
      c.restore();
    } },
  ];
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
  if (mode === "heli") items.push(...heliModel(o, x, y, z, a, col, isMe));
  const hsh = [...String(name)].reduce((h, c) => (h * 31 + c.charCodeAt(0)) >>> 0, 0);
  const step = Math.floor(performance.now() / 1000 * 2.2) % 2;
  const carM = /^car(\d+)$/.exec(mode || ""), spec = isMe ? me.car : carM ? carSpec(+carM[1]) : null;
  if (carM) mode = "car";
  const spr = mode === "car" ? (spec?.name || live.SPRITE_CARS[hsh % 8]) : mode === "bike" ? `bike_${hsh % 8}` : `walker_${hsh % 8}_${(isMe ? me.v : 1) > 0.2 ? step : 0}`;
  const lamps = mode === "car" ? live.vlamps(x, y, z, a, spec?.L ?? 4.2, (spec?.W ?? 1.8) - 0.1, 0.65,
      spec?.siren ? { siren: true, bar: spec.service === 2 ? 2.0 : spec.service === 1 ? -0.2 : 1.6, barZ: spec.service === 2 ? 2.95 : spec.service === 1 ? 1.5 : 2.7 } : {}) : mode === "bike" ? live.vlamps(x, y, z, a, 1.7, 0, 0.9, { noBeam: true }) : null;
  if (mode !== "heli") items.push({ sx, sy, h: 2 * K, lamps, draw: () => {
    if (spec?.name === "tow") return live.towTruck(x, y, z, a, false, performance.now() / 1000);
    if (spec?.name === "sweeper") return live.sweeper(x, y, z, a, performance.now() / 1000);
    if (live.sprite(spr, x, y, z, a, "p:" + name)) return;
    if (mode === "car") live.box(x, y, z + 0.2, a, 4.3, 1.8, 1.45, live.shade(col, 1.1), col, "rgba(20,24,28,.6)");
    else {
      if (mode === "bike") live.box(x, y, z + 0.3 * K, a, 1.7 * K * 0.8, 0.12 * K, 0.5 * K, "#2b2f36", "#2b2f36");
      const lift = mode === "bike" ? 0.45 * K : 0;
      live.box(x, y, z + lift, a, 0.45 * K, 0.5 * K, 1.1 * K, live.shade(col, 1.1), col, "rgba(20,24,28,.5)");
      live.box(x, y, z + lift + 1.1 * K, a, 0.35 * K, 0.35 * K, 0.38 * K, "#f1c9a5", "#d9a982", "rgba(20,24,28,.5)");
    }
  } });
  items.push({ sx, sy, overlay: true, draw: () => {
    const [cx, cy] = live.toCanvas(...live.project(x, y, z + (mode === "heli" ? 7.5 : 2.2 * K)));
    const ctx = document.getElementById("live").getContext("2d");
    // humans are marked: a name pill and a diamond above the head (the inhabitants, PNJ, have neither)
    const D = TN.dpr;
    ctx.font = `600 ${11 * D}px Geist, system-ui, sans-serif`; ctx.textAlign = "center"; ctx.textBaseline = "middle";
    const w = ctx.measureText(name).width + 12 * D, hgt = 16 * D, top = cy - hgt - 6 * D;
    ctx.fillStyle = isMe ? "#f2b544" : "#3d7be0";
    ctx.beginPath(); ctx.roundRect?.(cx - w / 2, top, w, hgt, 8 * D); ctx.fill();
    ctx.beginPath(); ctx.moveTo(cx - 5 * D, top + hgt); ctx.lineTo(cx + 5 * D, top + hgt); ctx.lineTo(cx, top + hgt + 6 * D); ctx.fill();
    ctx.fillStyle = isMe ? "#1d1a12" : "#fff"; ctx.fillText(name, cx, top + hgt / 2 + 0.5 * D);
    ctx.textAlign = "start"; ctx.textBaseline = "alphabetic";
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
  const mine = avatarItems(me, me.x, me.y, me.z, me.a, me.ride ? me.ride.kind : me.mode, session?.name || "", true);
  if (room) mine.forEach((it) => (it.room = true));
  items.push(...mine);
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
    txt = `<b>${esc(session?.name)}</b> · ${{ walk: "à pied", bike: me.rental ? "en Naolib Vélo" : "à vélo", car: `en voiture${me.car ? ` (${me.car.label})` : ""}`, heli: "en hélico" }[me.mode]} · ${kmh} km/h${me.mode === "heli" ? ` · ${Math.round(me.alt)} m` : ""}`;
  }
  h.innerHTML = txt;
}
let toastT = 0;
function toast(t) {
  const el = $("toast"); el.textContent = t; el.hidden = false;
  clearTimeout(toastT); toastT = setTimeout(() => (el.hidden = true), 2600);
}

requestAnimationFrame(step);
