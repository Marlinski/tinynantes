//! Aircraft around Nantes Atlantique (NTE) from ADS-B (adsb.fi open data; adsb.lol, ODbL, as a fallback), with routes from adsbdb.com
//! (looked up once per callsign and cached), and a reconstructed airport board: aircraft seen landing at or
//! taking off from NTE in the last hours.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use serde::Serialize;
use serde_json::Value;
use tokio::sync::RwLock;

use crate::feeds::Origin;
use crate::l93;
use crate::store::Store;

pub const NTE: (f64, f64) = (-1.6107, 47.1532); // lon, lat
/// ADS-B within 60 NM of the airport: adsb.fi's open data first (1 request/s allowed, list under "aircraft"),
/// adsb.lol as a fallback (500 requests a day per IP since 2026: not enough on its own at one every 20 s; "ac").
const FEEDS: [&str; 2] = [
    "https://opendata.adsb.fi/api/v2/lat/47.1532/lon/-1.6107/dist/60",
    "https://api.adsb.lol/v2/point/47.1532/-1.6107/60",
];

/// The feed, and whether it came from the fallback (then we poll less often, to stay within its daily quota).
async fn fetch_feed(http: &reqwest::Client) -> Result<(Value, bool)> {
    let mut last = None;
    for (k, url) in FEEDS.iter().enumerate() {
        match async { Ok::<Value, anyhow::Error>(http.get(*url).send().await?.error_for_status()?.json().await?) }.await {
            Ok(mut v) => {
                if v["ac"].is_null() {
                    v["ac"] = v["aircraft"].take();
                }
                return Ok((v, k > 0));
            }
            Err(e) => last = Some(e),
        }
    }
    Err(last.unwrap())
}
const ROUTE: &str = "https://api.adsbdb.com/v0/callsign/";

#[derive(Clone, Serialize, serde::Deserialize)]
pub struct Airport {
    pub iata: String,
    pub name: String,
    pub city: String,
    pub country: String,
}

#[derive(Clone, Serialize, serde::Deserialize)]
pub struct Route {
    pub airline: Option<String>,
    pub from: Option<Airport>,
    pub to: Option<Airport>,
}

#[derive(Clone, Serialize)]
pub struct Plane {
    pub hex: String,
    pub callsign: String,
    pub reg: Option<String>,
    pub kind: Option<String>,
    /// Lambert-93 relative to the map origin
    pub x: f64,
    pub y: f64,
    /// metres above sea level (0 on the ground)
    pub alt: f64,
    pub ground: bool,
    /// m/s and degrees from north, clockwise
    pub speed: f64,
    pub track: f64,
    pub vrate: f64,
    pub route: Option<Route>,
}

#[derive(Clone, Serialize)]
pub struct Movement {
    pub ts: i64,
    pub callsign: String,
    pub kind: Option<String>,
    pub airline: Option<String>,
    /// origin for arrivals, destination for departures
    pub other: Option<Airport>,
}

#[derive(Default)]
pub struct Sky {
    pub planes: Vec<Plane>,
    pub at: i64,
    pub error: Option<String>,
    routes: HashMap<String, (i64, Option<Route>)>,
    last_alt: HashMap<String, (f64, f64)>, // hex -> (alt, distance to NTE km)
    pub arrivals: Vec<Movement>,
    pub departures: Vec<Movement>,
}

fn airport(v: &Value) -> Option<Airport> {
    Some(Airport {
        iata: v["iata_code"].as_str()?.to_string(),
        name: v["name"].as_str().unwrap_or("").to_string(),
        city: v["municipality"].as_str().unwrap_or("").to_string(),
        country: v["country_name"].as_str().unwrap_or("").to_string(),
    })
}

async fn route(http: &reqwest::Client, cs: &str) -> Option<Route> {
    let v: Value = http.get(format!("{ROUTE}{cs}")).send().await.ok()?.json().await.ok()?;
    let r = &v["response"]["flightroute"];
    if r.is_null() {
        return None;
    }
    Some(Route { airline: r["airline"]["name"].as_str().map(String::from), from: airport(&r["origin"]), to: airport(&r["destination"]) })
}

/// Returns true when the data came from the fallback feed.
pub async fn refresh(http: &reqwest::Client, o: Origin, sky: &Arc<RwLock<Sky>>, store: &Store) -> Result<bool> {
    let (v, fallback) = fetch_feed(http).await?;
    let now = chrono::Utc::now().timestamp();
    let mut planes = vec![];
    let mut need = vec![];
    {
        let mut s = sky.write().await;
        for a in v["ac"].as_array().into_iter().flatten() {
            let cs = a["flight"].as_str().unwrap_or("").trim().to_string();
            if cs.is_empty() || s.routes.contains_key(&cs) {
                continue;
            }
            if let Some((ts, r)) = store.route(&cs) {
                s.routes.insert(cs, (ts, r.and_then(|j| serde_json::from_str(&j).ok())));
            }
        }
    }
    {
        let s = sky.read().await;
        for a in v["ac"].as_array().into_iter().flatten() {
            let (Some(lon), Some(lat)) = (a["lon"].as_f64(), a["lat"].as_f64()) else { continue };
            let hex = a["hex"].as_str().unwrap_or("").to_string();
            let cs = a["flight"].as_str().unwrap_or("").trim().to_string();
            let ground = a["alt_baro"].as_str() == Some("ground");
            let alt = if ground { 0.0 } else { a["alt_geom"].as_f64().or(a["alt_baro"].as_f64()).unwrap_or(0.0) * 0.3048 };
            let (x, y) = l93::project(lon, lat);
            if !cs.is_empty() && s.routes.get(&cs).map(|r| now - r.0 > 3 * 86400).unwrap_or(true) {
                need.push(cs.clone());
            }
            planes.push(Plane {
                hex,
                route: s.routes.get(&cs).and_then(|r| r.1.clone()),
                callsign: cs,
                reg: a["r"].as_str().map(String::from),
                kind: a["t"].as_str().map(String::from),
                x: x - o.0,
                y: y - o.1,
                alt,
                ground,
                speed: a["gs"].as_f64().unwrap_or(0.0) * 0.514_444,
                track: a["track"].as_f64().or(a["true_heading"].as_f64()).unwrap_or(0.0),
                vrate: a["baro_rate"].as_f64().or(a["geom_rate"].as_f64()).unwrap_or(0.0) * 0.00508,
            });
        }
    }
    // routes for callsigns we don't know yet (a few per refresh, to stay polite)
    let mut found = vec![];
    for cs in need.into_iter().take(8) {
        found.push((cs.clone(), route(http, &cs).await));
    }
    let mut s = sky.write().await;
    for (cs, r) in found {
        store.save_route(&cs, now, r.as_ref().and_then(|r| serde_json::to_string(r).ok()).as_deref());
        s.routes.insert(cs, (now, r));
    }
    for p in planes.iter_mut() {
        if p.route.is_none() {
            p.route = s.routes.get(&p.callsign).and_then(|r| r.1.clone());
        }
    }
    // board: an aircraft low and close to NTE that was higher before is landing; the reverse is a take-off
    for p in &planes {
        let d = ((p.x - (s_nte_x(o))).hypot(p.y - s_nte_y(o))) / 1000.0;
        let prev = s.last_alt.insert(p.hex.clone(), (p.alt, d));
        let Some((palt, _)) = prev else { continue };
        let mv = |p: &Plane, arriving: bool| Movement {
            ts: now,
            callsign: p.callsign.clone(),
            kind: p.kind.clone(),
            airline: p.route.as_ref().and_then(|r| r.airline.clone()),
            other: p.route.as_ref().and_then(|r| if arriving { r.from.clone() } else { r.to.clone() }),
        };
        if d < 4.0 && palt > 60.0 && p.alt <= 60.0 && !s.arrivals.iter().any(|m| m.callsign == p.callsign && now - m.ts < 3600) {
            let m = mv(p, true);
            s.arrivals.push(m);
        } else if d < 6.0 && palt <= 60.0 && p.alt > 60.0 && p.vrate >= 0.0 && !s.departures.iter().any(|m| m.callsign == p.callsign && now - m.ts < 3600) {
            let m = mv(p, false);
            s.departures.push(m);
        }
    }
    s.arrivals.retain(|m| now - m.ts < 6 * 3600);
    s.departures.retain(|m| now - m.ts < 6 * 3600);
    s.planes = planes;
    s.at = now;
    s.error = None;
    Ok(fallback)
}

fn s_nte_x(o: Origin) -> f64 {
    l93::project(NTE.0, NTE.1).0 - o.0
}
fn s_nte_y(o: Origin) -> f64 {
    l93::project(NTE.0, NTE.1).1 - o.1
}

pub async fn run(http: reqwest::Client, o: Origin, sky: Arc<RwLock<Sky>>, store: Arc<Store>) {
    // frugal: every 30 s (planes move slowly at this scale, the viewer interpolates), every 3 min on the fallback
    let mut wait = 30;
    loop {
        match refresh(&http, o, &sky, &store).await {
            Ok(fallback) => wait = if fallback { 180 } else { 30 },
            Err(e) => {
                tracing::warn!("planes: {e:#}");
                sky.write().await.error = Some(format!("{e:#}"));
                wait = (wait * 2).min(300);
            }
        }
        tokio::time::sleep(Duration::from_secs(wait)).await;
    }
}
