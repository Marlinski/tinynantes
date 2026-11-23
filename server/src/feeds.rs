//! City open-data feeds (data.nantesmetropole.fr), fetched by the server on a timer and reshaped for the
//! viewer: road traffic (flow, speed, state per road section) and today's events.
//! Coordinates are Lambert-93 relative to the map origin, like the tiles.

use anyhow::{Context, Result};
use chrono::Duration;
use serde::Serialize;
use serde_json::Value;

use crate::l93;

const ODS: &str = "https://data.nantesmetropole.fr/api/explore/v2.1/catalog/datasets";
pub const TRAFFIC: &str = "244400404_fluidite-axes-routiers-nantes-metropole";
pub const EVENTS: &str = "244400404_agenda-evenements-nantes-metropole_v3";

pub type Origin = (f64, f64);

fn rel(o: Origin, lon: f64, lat: f64) -> [f64; 2] {
    let (x, y) = l93::project(lon, lat);
    [((x - o.0) * 10.0).round() / 10.0, ((y - o.1) * 10.0).round() / 10.0]
}

#[derive(Serialize, Clone)]
pub struct Section {
    pub id: i64,
    pub name: String,
    /// vehicles per hour over the last measurement period
    pub flow: Option<f64>,
    /// km/h
    pub speed: Option<f64>,
    /// occupancy rate (%)
    pub occupancy: Option<f64>,
    /// "Fluide", "Dense", "Saturé", "Indéterminé"...
    pub state: String,
    pub measured: Option<String>,
    pub p: Vec<[f64; 2]>,
}

pub async fn traffic(http: &reqwest::Client, o: Origin) -> Result<Vec<Section>> {
    let url = format!("{ODS}/{TRAFFIC}/exports/json");
    let rows: Vec<Value> = http.get(url).send().await?.error_for_status()?.json().await?;
    let mut out = vec![];
    for r in rows {
        let geom = &r["geo_shape"]["geometry"];
        let mut lines: Vec<&Value> = vec![];
        match geom["type"].as_str() {
            Some("LineString") => lines.push(&geom["coordinates"]),
            Some("MultiLineString") => lines.extend(geom["coordinates"].as_array().into_iter().flatten()),
            _ => continue,
        }
        for line in lines {
            let p: Vec<[f64; 2]> = line
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|c| Some(rel(o, c[0].as_f64()?, c[1].as_f64()?)))
                .collect();
            if p.len() < 2 {
                continue;
            }
            out.push(Section {
                id: r["cha_id"].as_i64().unwrap_or(0),
                name: r["cha_lib"].as_str().unwrap_or("").to_string(),
                flow: r["mf1_debit"].as_f64(),
                speed: r["mf1_vit"].as_f64(),
                occupancy: r["mf1_taux"].as_f64(),
                state: r["etat_trafic"].as_str().unwrap_or("").to_string(),
                measured: r["mf1_hd"].as_str().map(String::from),
                p,
            });
        }
    }
    Ok(out)
}

#[derive(Serialize, Clone)]
pub struct Event {
    pub id: String,
    pub name: String,
    pub summary: String,
    pub place: String,
    pub address: String,
    pub date: String,
    pub start: Option<String>,
    pub end: Option<String>,
    pub types: Vec<String>,
    pub themes: Vec<String>,
    pub free: bool,
    pub canceled: bool,
    pub full: bool,
    pub url: Option<String>,
    pub image: Option<String>,
    pub p: [f64; 2],
}

/// Events of today and tomorrow (Paris time) that have a location.
pub async fn events(http: &reqwest::Client, o: Origin) -> Result<Vec<Event>> {
    let today = crate::gtfs::today();
    let tomorrow = today + Duration::days(1);
    let url = format!("{ODS}/{EVENTS}/exports/json");
    let q = format!("date>=date'{today}' and date<=date'{tomorrow}'");
    let rows: Vec<Value> = http.get(url).query(&[("where", q.as_str())]).send().await?.error_for_status()?.json().await?;
    let s = |v: &Value| v.as_str().map(|x| x.trim().to_string()).filter(|x| !x.is_empty());
    let list = |v: &Value| v.as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
    let mut out = vec![];
    for r in rows {
        let (Some(lon), Some(lat)) = (r["longitude"].as_f64(), r["latitude"].as_f64()) else { continue };
        let desc = s(&r["description_evt"]).unwrap_or_default();
        let summary: String = desc.lines().next().unwrap_or("").chars().take(280).collect();
        out.push(Event {
            id: s(&r["id_agenda_in"]).or_else(|| s(&r["id_manif"])).unwrap_or_default(),
            name: s(&r["nom"]).unwrap_or_default(),
            summary,
            place: s(&r["lieu"]).unwrap_or_default(),
            address: s(&r["adresse"]).unwrap_or_default(),
            date: s(&r["date"]).unwrap_or_default(),
            start: s(&r["heure_debut"]),
            end: s(&r["heure_fin"]),
            types: list(&r["types_libelles"]),
            themes: list(&r["themes_libelles"]),
            free: r["gratuit"].as_str() == Some("oui"),
            canceled: r["annule"].as_str() == Some("oui"),
            full: r["complet"].as_str() == Some("oui"),
            url: s(&r["lien_agenda"]).or_else(|| s(&r["url_site"])),
            image: s(&r["media_url"]),
            p: rel(o, lon, lat),
        });
    }
    Ok(out)
}

pub fn origin_from_world(path: &std::path::Path) -> Result<Origin> {
    let v: Value = serde_json::from_slice(&std::fs::read(path).with_context(|| format!("{path:?}"))?)?;
    let o = &v["origin"];
    Ok((o[0].as_f64().context("origin")?, o[1].as_f64().context("origin")?))
}
