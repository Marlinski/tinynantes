// Debug panel, second half (F9 or ?debug; the first half, time of day and weather, is in fx.js).
//   - for you only: Le Grand Éléphant out on a ride whatever the date;
//   - for everyone (server side, needs the admin key unless the local server runs with TN_DEBUG_OPEN=1):
//     a crowd dropped where you click (strolling about, or gathered facing the middle), a demonstration along a
//     route you draw, and a button that takes all of it away.
const live = window.TNLive;
const KEY = "tn-admin";
let mode = null;            // null | "crowd" | "route"
let route = [];             // [[x, y]] local metres
let overlay = null, status = null, panel = null;

const store = { get: (k) => { try { return localStorage.getItem(k) || ""; } catch { return ""; } },
  set: (k, v) => { try { localStorage.setItem(k, v); } catch { /* private mode */ } } };

// ------------------------------------------------------------ map <-> screen
function project(x, y, z) {
  const [r, d] = TN.man.axes;
  return [x * r[0] + y * r[1] + z * r[2], x * d[0] + y * d[1] + z * d[2]];
}
function toScreen(x, y) {           // CSS pixels
  const [sx, sy] = project(x, y, live.zAt(x, y)), v = TN.view;
  return [(sx - v.x) * v.s + innerWidth / 2, (sy - v.y) * v.s + innerHeight / 2];
}
function toGround(cx, cy) {         // CSS pixels -> local metres on the ground
  const v = TN.view, sx = (cx - innerWidth / 2) / v.s + v.x, sy = (cy - innerHeight / 2) / v.s + v.y;
  const [r, d] = TN.man.axes, det = r[0] * d[1] - r[1] * d[0];
  let z = 10, x = 0, y = 0;
  for (let k = 0; k < 4; k++) {
    const a = sx - r[2] * z, b = sy - d[2] * z;
    x = (a * d[1] - b * r[1]) / det; y = (b * r[0] - a * d[0]) / det;
    z = live.zAt(x, y);
  }
  return [x, y];
}

// ------------------------------------------------------------ server actions
async function act(body) {
  const key = panel?.querySelector("#dbgKey").value.trim() || "";
  say("…");
  try {
    const r = await fetch("api/debug", { method: "POST", headers: { "content-type": "application/json" },
      body: JSON.stringify({ key: key || undefined, ...body }) });
    const j = await r.json().catch(() => ({}));
    if (!r.ok) { say(j.error || `erreur ${r.status}`, true); return null; }
    return j;
  } catch { say("serveur injoignable", true); return null; }
}
function say(t, bad) { if (status) { status.textContent = t; status.classList.toggle("bad", !!bad); } }
const val = (id) => panel.querySelector(id).value;

function arm(m) {
  mode = mode === m ? null : m;
  if (mode === "route") route = [];
  panel.querySelector("#dbgCrowd").classList.toggle("on", mode === "crowd");
  panel.querySelector("#dbgRoute").classList.toggle("on", mode === "route");
  document.body.classList.toggle("dbg-armed", !!mode);
  say(mode === "crowd" ? "clique sur la carte pour poser la foule" :
    mode === "route" ? "clique les étapes du parcours, puis « Lancer la manif »" : "");
  draw();
}

// a tap on the map while armed (app.js asks us before opening what's under the pointer)
TN.debugClick = (cx, cy) => {
  if (!mode || !TN.man) return false;
  const [x, y] = toGround(cx, cy);
  if (mode === "crowd") {
    arm("crowd");
    act({ action: "crowd", x, y, n: +val("#dbgN"), r: +val("#dbgR"), minutes: +val("#dbgMin"),
      gather: val("#dbgKind") === "gather" })
      .then((j) => j && say(`${j.spawned} personnes posées`));
  } else {
    route.push([x, y]);
    say(`${route.length} étape${route.length > 1 ? "s" : ""}${route.length >= 2 ? " · « Lancer la manif » quand c'est bon" : ""}`);
    draw();
  }
  return true;
};

async function launchMarch() {
  if (route.length < 2) { say("trace d'abord un parcours (au moins 2 clics)", true); return; }
  const j = await act({ action: "march", route: route.map(([x, y]) => [Math.round(x), Math.round(y)]),
    motive: val("#dbgMotive"), size: +val("#dbgSize"), minutes: +val("#dbgMin") });
  if (!j) return;
  arm("route"); route = []; draw();          // disarm, clear the drawn route
  say(`manif lancée sur ${(j.length / 1000).toFixed(1)} km : le cortège part du premier point`);
}

// ------------------------------------------------------------ overlay: the route being drawn, the crowd's footprint
function draw() {
  if (!overlay) return;
  const on = (mode || route.length) && TN.man && !panel?.hidden;
  overlay.style.display = on ? "" : "none";
  if (!on) return;
  let h = "";
  if (route.length) {
    const pts = route.map(([x, y]) => toScreen(x, y));
    h += `<polyline points="${pts.map((p) => p.join(",")).join(" ")}" />`;
    h += pts.map(([a, b], k) => `<circle cx="${a}" cy="${b}" r="${k ? 4 : 6}" class="${k ? "" : "start"}"/>`).join("");
  }
  if (mode === "crowd" && TN.pointer) {
    const [x, y] = toGround(...TN.pointer), r = +val("#dbgR");
    const ring = Array.from({ length: 32 }, (_, k) => toScreen(x + r * Math.cos(k / 16 * Math.PI), y + r * Math.sin(k / 16 * Math.PI)));
    h += `<polygon class="area" points="${ring.map((p) => p.join(",")).join(" ")}" />`;
  }
  overlay.innerHTML = h;
}
function loop() { draw(); requestAnimationFrame(loop); }

// ------------------------------------------------------------ Le Grand Éléphant, out now (this browser only)
function elephantOut(on) {
  // a day it rides (12:30 departure), shifted so the ride starts now and repeats every 30 min
  live.elephantAt = on ? Date.parse("2027-05-15T12:30:00+02:00") / 1000 - (live.now() % 1800) : 0;
}

// ------------------------------------------------------------ the panel
function attach(el) {
  if (el.querySelector("#dbgSim")) return;
  panel = el;
  const sec = document.createElement("div");
  sec.id = "dbgSim";
  sec.innerHTML = `
    <label><input type="checkbox" id="dbgEl"> éléphant en balade <small>(pour toi)</small></label>
    <hr><b>Simulation</b> <small>visible par tout le monde</small>
    <label class="col">clé admin <input type="password" id="dbgKey" autocomplete="off" placeholder="vide en local"></label>
    <div class="row"><label>durée <input type="number" id="dbgMin" value="20" min="1" max="240"> min</label></div>
    <fieldset><legend>Foule</legend>
      <div class="row"><label><input type="number" id="dbgN" value="80" min="5" max="400"> pers.</label>
        <label>rayon <input type="number" id="dbgR" value="40" min="5" max="300"> m</label></div>
      <div class="row"><select id="dbgKind"><option value="stroll">qui flâne</option><option value="gather">rassemblée</option></select>
        <button id="dbgCrowd">Poser…</button></div>
    </fieldset>
    <fieldset><legend>Manif</legend>
      <input id="dbgMotive" placeholder="motif (ex. Marche pour le climat)" maxlength="80">
      <div class="row"><label><input type="number" id="dbgSize" value="200" min="10" max="600"> pers.</label>
        <button id="dbgRoute">Tracer…</button></div>
      <div class="row"><button id="dbgUndo">↶ étape</button><button id="dbgGo" class="go">Lancer la manif</button></div>
    </fieldset>
    <button id="dbgStop" class="stop">Tout arrêter</button>
    <small id="dbgStatus" class="status"></small>`;
  el.insertBefore(sec, el.querySelector("#dbgInfo"));
  status = sec.querySelector("#dbgStatus");
  const key = sec.querySelector("#dbgKey");
  key.value = store.get(KEY);
  key.onchange = () => store.set(KEY, key.value.trim());
  sec.querySelector("#dbgEl").onchange = (e) => elephantOut(e.target.checked);
  sec.querySelector("#dbgCrowd").onclick = () => arm("crowd");
  sec.querySelector("#dbgRoute").onclick = () => arm("route");
  sec.querySelector("#dbgUndo").onclick = () => { route.pop(); say(`${route.length} étapes`); };
  sec.querySelector("#dbgGo").onclick = launchMarch;
  sec.querySelector("#dbgStop").onclick = async () => {
    route = []; if (mode) arm(mode);
    const j = await act({ action: "stop" });
    if (j) say(`${j.removed} personnes retirées`);
  };
  // keep the map's keyboard shortcuts out of the text fields
  sec.addEventListener("keydown", (e) => e.stopPropagation());
  addEventListener("keydown", (e) => { if (e.key === "Escape" && mode) arm(mode); });
  overlay = document.createElementNS("http://www.w3.org/2000/svg", "svg");
  overlay.id = "dbgOverlay";
  document.body.append(overlay);
  requestAnimationFrame(loop);
}
TN.onDebugPanel = attach;
const already = document.getElementById("dbg");
if (already) attach(already);
