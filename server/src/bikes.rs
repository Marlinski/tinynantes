//! Naolib Vélo, the self-service bikes (JCDecaux Cyclocity GBFS, open data): stations and live availability.
//! Polled every 30 s; when a station has fewer bikes than at the last poll, someone just took one: the city
//! simulation sends a cyclist off from there. Players can "rent" one in the game: that only exists in the game.

use std::collections::HashMap;
use std::sync::Arc;

use serde::Serialize;
use serde_json::Value;
use tokio::sync::RwLock;

const INFO: &str = "https://api.cyclocity.fr/contracts/nantes/gbfs/v2/station_information.json";
const STATUS: &str = "https://api.cyclocity.fr/contracts/nantes/gbfs/v2/station_status.json";

#[derive(Clone, Serialize)]
pub struct Station {
    pub id: String,
    pub name: String,
    pub address: String,
    pub p: [f64; 2],
    pub capacity: i64,
    pub bikes: i64,
    pub docks: i64,
    pub renting: bool,
}

#[derive(Default)]
pub struct Bikes {
    pub stations: Vec<Station>,
    pub updated: i64,
    /// the latest station report in the feed (when the operator's counts last moved)
    pub reported: i64,
    /// bikes taken since the last poll: (station position, station name), for the simulation to pick up
    pub departures: Vec<([f32; 2], String)>,
}

fn title(s: &str) -> String {
    // "PLACE DU CIRQUE" -> "Place du Cirque"
    s.split(' ').enumerate().map(|(i, w)| {
        let l = w.to_lowercase();
        if i > 0 && matches!(l.as_str(), "du" | "de" | "des" | "la" | "le" | "les" | "et" | "aux" | "au" | "sur") {
            return l;
        }
        let mut c = l.chars();
        match c.next() { Some(f) => f.to_uppercase().collect::<String>() + c.as_str(), None => String::new() }
    }).collect::<Vec<_>>().join(" ")
}

async fn get(http: &reqwest::Client, u: &str) -> anyhow::Result<Value> {
    Ok(http.get(u).send().await?.error_for_status()?.json().await?)
}

pub async fn run(http: reqwest::Client, origin: crate::feeds::Origin, b: Arc<RwLock<Bikes>>) {
    let mut info: HashMap<String, (String, String, [f64; 2], i64)> = HashMap::new();
    let mut info_at = 0i64;
    let mut prev: HashMap<String, i64> = HashMap::new();
    loop {
        let now = chrono::Utc::now().timestamp();
        if now - info_at > 3600 || info.is_empty() {
            if let Ok(v) = get(&http, INFO).await {
                info = v["data"]["stations"].as_array().into_iter().flatten().filter_map(|s| {
                    let (lon, lat) = (s["lon"].as_f64()?, s["lat"].as_f64()?);
                    Some((s["station_id"].as_str()?.to_string(), (title(s["name"].as_str().unwrap_or("")),
                          s["address"].as_str().unwrap_or("").to_string(), crate::feeds::rel(origin, lon, lat), s["capacity"].as_i64().unwrap_or(0))))
                }).collect();
                info_at = now;
            }
        }
        match get(&http, STATUS).await {
            Ok(v) => {
                let mut out = vec![];
                let mut gone = vec![];
                let mut reported = 0i64;
                for s in v["data"]["stations"].as_array().into_iter().flatten() {
                    let Some(id) = s["station_id"].as_str() else { continue };
                    let Some((name, address, p, capacity)) = info.get(id) else { continue };
                    let bikes = s["num_bikes_available"].as_i64().unwrap_or(0);
                    reported = reported.max(s["last_reported"].as_i64().unwrap_or(0));
                    if let Some(&was) = prev.get(id) {
                        for _ in 0..(was - bikes).clamp(0, 4) {
                            gone.push(([p[0] as f32, p[1] as f32], name.clone()));
                        }
                    }
                    prev.insert(id.to_string(), bikes);
                    out.push(Station {
                        id: id.to_string(), name: name.clone(), address: address.clone(), p: *p, capacity: *capacity, bikes,
                        docks: s["num_docks_available"].as_i64().unwrap_or(0),
                        renting: s["is_renting"].as_bool().unwrap_or(true) && s["is_installed"].as_bool().unwrap_or(true),
                    });
                }
                let mut w = b.write().await;
                w.stations = out;
                w.updated = now;
                w.reported = reported;
                w.departures.extend(gone);
                let n = w.departures.len();
                if n > 50 {
                    w.departures.drain(..n - 50);
                }
            }
            Err(e) => tracing::warn!("bikes: {e:#}"),
        }
        tokio::time::sleep(std::time::Duration::from_secs(30)).await;
    }
}
