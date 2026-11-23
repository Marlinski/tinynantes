//! Naolib GTFS: today's timetable for the journey patterns drawn on the map, plus realtime delays.
//!
//! Vehicle positions are not published by Naolib, so they are inferred: each running trip is placed on its
//! (snapped) route shape by interpolating between scheduled stop times, shifted by the delays of the
//! GTFS-RT trip-updates feed (polled by the server, never by visitors).

use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{Datelike, Duration, NaiveDate, TimeZone, Weekday};
use chrono_tz::Europe::Paris;
use serde::Serialize;

const GTFS_META: &str = "https://data.nantesmetropole.fr/api/explore/v2.1/catalog/datasets/\
244400404_transports_commun_naolib_nantes_metropole_gtfs/records?limit=1";

pub struct Trip {
    pub id: String,
    pub route: String,
    pub shape: String,
    pub headsign: String,
    pub stop_ids: Vec<String>,
    pub names: Vec<std::sync::Arc<str>>,
    pub seq: Vec<u32>,
    pub arr: Vec<i64>, // epoch seconds
    pub dep: Vec<i64>,
}

pub struct Timetable {
    pub date: NaiveDate,
    pub trips: Vec<Trip>,
    pub by_id: HashMap<String, Vec<usize>>,
}

/// Download (if missing or older than `max_age_h`) and unzip the GTFS into `dir`.
pub async fn ensure_gtfs(http: &reqwest::Client, dir: &Path, max_age_h: u64) -> Result<()> {
    let marker = dir.join("stop_times.txt");
    if let Ok(m) = std::fs::metadata(&marker) {
        if m.modified()?.elapsed().map(|e| e.as_secs() < max_age_h * 3600).unwrap_or(false) {
            return Ok(());
        }
    }
    let meta: serde_json::Value = http.get(GTFS_META).send().await?.error_for_status()?.json().await?;
    let url = meta["results"][0]["fichier"]["url"].as_str().context("no GTFS file url")?.to_string();
    let bytes = http.get(&url).send().await?.error_for_status()?.bytes().await?;
    let dir = dir.to_path_buf();
    tokio::task::spawn_blocking(move || -> Result<()> {
        std::fs::create_dir_all(&dir)?;
        let mut z = zip::ZipArchive::new(std::io::Cursor::new(bytes))?;
        for i in 0..z.len() {
            let mut f = z.by_index(i)?;
            let name = PathBuf::from(f.name()).file_name().context("bad zip entry")?.to_owned();
            let mut buf = Vec::new();
            f.read_to_end(&mut buf)?;
            std::fs::write(dir.join(name), buf)?;
        }
        Ok(())
    })
    .await??;
    tracing::info!("GTFS downloaded");
    Ok(())
}

fn reader(dir: &Path, name: &str) -> Result<csv::Reader<std::fs::File>> {
    Ok(csv::ReaderBuilder::new().from_path(dir.join(name)).with_context(|| format!("open {name}"))?)
}

fn col(h: &csv::StringRecord, name: &str) -> Result<usize> {
    // tolerate a UTF-8 BOM on the first header
    h.iter().position(|c| c.trim_start_matches('\u{feff}') == name).with_context(|| format!("no column {name}"))
}

fn active_services(dir: &Path, day: NaiveDate) -> Result<HashSet<String>> {
    let mut on = HashSet::new();
    let mut r = reader(dir, "calendar.txt")?;
    let h = r.headers()?.clone();
    let wd = ["monday", "tuesday", "wednesday", "thursday", "friday", "saturday", "sunday"];
    let dcol = col(&h, wd[day.weekday().num_days_from_monday() as usize])?;
    let (sid, s0, s1) = (col(&h, "service_id")?, col(&h, "start_date")?, col(&h, "end_date")?);
    let ymd = day.format("%Y%m%d").to_string();
    for rec in r.records() {
        let rec = rec?;
        if &rec[dcol] == "1" && rec[s0] <= *ymd && *ymd <= rec[s1] {
            on.insert(rec[sid].to_string());
        }
    }
    if let Ok(mut r) = reader(dir, "calendar_dates.txt") {
        let h = r.headers()?.clone();
        let (sid, d, ex) = (col(&h, "service_id")?, col(&h, "date")?, col(&h, "exception_type")?);
        for rec in r.records() {
            let rec = rec?;
            if rec[d] == *ymd {
                if &rec[ex] == "1" {
                    on.insert(rec[sid].to_string());
                } else {
                    on.remove(&rec[sid]);
                }
            }
        }
    }
    Ok(on)
}

fn secs(t: &str) -> Option<i64> {
    let mut it = t.split(':').map(|p| p.trim().parse::<i64>());
    Some(it.next()?.ok()? * 3600 + it.next()?.ok()? * 60 + it.next()?.ok()?)
}

fn midnight(day: NaiveDate) -> i64 {
    // GTFS times count from "noon minus 12 h", which is local midnight except on DST days (off by an hour
    // for a few night trips twice a year: acceptable here)
    Paris.from_local_datetime(&day.and_hms_opt(0, 0, 0).unwrap()).earliest().unwrap().timestamp()
}

/// Build the timetable of `day` (plus the previous day's trips that run past midnight) for the given shapes
/// (shape id -> number of stops on the snapped shape).
pub fn load(dir: &Path, day: NaiveDate, shapes: &HashMap<String, usize>) -> Result<Timetable> {
    let t0 = std::time::Instant::now();
    let prev = day - Duration::days(1);
    let (on_today, on_prev) = (active_services(dir, day)?, active_services(dir, prev)?);
    let (base_today, base_prev) = (midnight(day), midnight(prev));

    // stop names (quays carry their stop area's name)
    let mut names: HashMap<String, std::sync::Arc<str>> = HashMap::new();
    let mut r = reader(dir, "stops.txt")?;
    let h = r.headers()?.clone();
    let (sidc, snc) = (col(&h, "stop_id")?, col(&h, "stop_name")?);
    for rec in r.records() {
        let rec = rec?;
        names.insert(rec[sidc].to_string(), rec[snc].into());
    }

    // trips on the map, for either service day
    let mut wanted: HashMap<String, Vec<(i64, bool)>> = HashMap::new(); // trip -> [(base, is_prev)]
    let mut meta: HashMap<String, (String, String, String)> = HashMap::new();
    let mut r = reader(dir, "trips.txt")?;
    let h = r.headers()?.clone();
    let (rid, sid, tid, shid, hs) =
        (col(&h, "route_id")?, col(&h, "service_id")?, col(&h, "trip_id")?, col(&h, "shape_id")?, col(&h, "trip_headsign")?);
    for rec in r.records() {
        let rec = rec?;
        if !shapes.contains_key(&rec[shid]) {
            continue;
        }
        let mut v = vec![];
        if on_today.contains(&rec[sid]) {
            v.push((base_today, false));
        }
        if on_prev.contains(&rec[sid]) {
            v.push((base_prev, true));
        }
        if !v.is_empty() {
            wanted.insert(rec[tid].to_string(), v);
            meta.insert(rec[tid].to_string(), (rec[rid].to_string(), rec[shid].to_string(), rec[hs].to_string()));
        }
    }

    // stop times of those trips (the big file: ~4 M rows, streamed)
    let mut rows: HashMap<String, Vec<(u32, i64, i64, String)>> = HashMap::with_capacity(wanted.len());
    let mut r = reader(dir, "stop_times.txt")?;
    let h = r.headers()?.clone();
    let (tid, arr, dep, sid, seq) =
        (col(&h, "trip_id")?, col(&h, "arrival_time")?, col(&h, "departure_time")?, col(&h, "stop_id")?, col(&h, "stop_sequence")?);
    let mut rec = csv::StringRecord::new();
    while r.read_record(&mut rec)? {
        let id = &rec[tid];
        if !wanted.contains_key(id) {
            continue;
        }
        let (Some(a), Some(d)) = (secs(&rec[arr]), secs(&rec[dep])) else { continue };
        rows.entry(id.to_string()).or_default().push((rec[seq].parse().unwrap_or(0), a, d, rec[sid].to_string()));
    }

    let mut trips = vec![];
    let mut by_id: HashMap<String, Vec<usize>> = HashMap::new();
    let mut mismatched = 0;
    for (id, mut st) in rows {
        st.sort_by_key(|s| s.0);
        let (route, shape, headsign) = meta[&id].clone();
        if shapes.get(&shape) != Some(&st.len()) {
            mismatched += 1;
            continue;
        }
        for &(base, is_prev) in &wanted[&id] {
            if is_prev && st.last().map(|s| s.1).unwrap_or(0) < 86_400 {
                continue; // yesterday's trip that ended before midnight
            }
            by_id.entry(id.clone()).or_default().push(trips.len());
            trips.push(Trip {
                id: id.clone(),
                route: route.clone(),
                shape: shape.clone(),
                headsign: headsign.clone(),
                stop_ids: st.iter().map(|s| s.3.clone()).collect(),
                names: st.iter().map(|s| names.get(&s.3).cloned().unwrap_or_else(|| "".into())).collect(),
                seq: st.iter().map(|s| s.0).collect(),
                arr: st.iter().map(|s| base + s.1).collect(),
                dep: st.iter().map(|s| base + s.2).collect(),
            });
        }
    }
    tracing::info!(
        "timetable {day}: {} trips on the map ({mismatched} skipped: stop count differs from shape) in {:.1}s",
        trips.len(),
        t0.elapsed().as_secs_f32()
    );
    Ok(Timetable { date: day, trips, by_id })
}

/// Per-trip delays (seconds) from a GTFS-RT trip-updates feed, one value per stop of the trip
/// (propagated forward from the last stop with an update, as the spec says).
pub fn apply_rt(tt: &Timetable, feed: &crate::rt::FeedMessage) -> (HashMap<usize, Vec<i32>>, HashSet<usize>, usize) {
    let mut out = HashMap::new();
    let mut canceled = HashSet::new();
    let mut matched = 0;
    for e in &feed.entity {
        let Some(tu) = &e.trip_update else { continue };
        let Some(tid) = tu.trip.as_ref().and_then(|t| t.trip_id.as_deref()) else { continue };
        let Some(idxs) = tt.by_id.get(tid) else { continue };
        matched += 1;
        if tu.trip.as_ref().and_then(|t| t.schedule_relationship) == Some(crate::rt::CANCELED) {
            canceled.extend(idxs.iter().copied());
            continue;
        }
        for &k in idxs {
            let trip = &tt.trips[k];
            let n = trip.seq.len();
            let mut d = vec![i32::MIN; n];
            for u in &tu.stop_time_update {
                let i = match (u.stop_sequence, u.stop_id.as_deref()) {
                    (Some(s), _) => trip.seq.iter().position(|&q| q == s),
                    (None, Some(sid)) => trip.stop_ids.iter().position(|q| q == sid),
                    _ => None,
                };
                let Some(i) = i else { continue };
                let ev = u.departure.as_ref().or(u.arrival.as_ref());
                let delay = ev.and_then(|e| e.delay.or_else(|| e.time.map(|t| (t - trip.dep[i]) as i32)));
                if let Some(v) = delay {
                    d[i] = v;
                }
            }
            let mut last = tu.delay.unwrap_or(0);
            for v in d.iter_mut() {
                if *v == i32::MIN {
                    *v = last;
                } else {
                    last = *v;
                }
            }
            out.insert(k, d);
        }
    }
    (out, canceled, matched)
}

#[derive(Serialize)]
pub struct Vehicle<'a> {
    /// trip id (stable while it runs)
    pub id: &'a str,
    pub route: &'a str,
    pub shape: &'a str,
    pub headsign: &'a str,
    /// stop index of the first entry in `arr` / `dep`
    pub k0: usize,
    /// expected arrival / departure (epoch s) at stops k0.. of the shape, realtime-corrected when known
    pub arr: Vec<i64>,
    pub dep: Vec<i64>,
    /// stop names, from k0
    pub names: Vec<&'a str>,
    /// current delay in seconds (None: schedule only)
    pub delay: Option<i32>,
}

/// Trips running between `now - before` and `now + after`, with their stop times from the stop before
/// `now` onwards (enough for the client to animate until the next poll).
pub fn vehicles<'a>(
    tt: &'a Timetable,
    delays: &HashMap<usize, Vec<i32>>,
    canceled: &HashSet<usize>,
    now: i64,
    after: i64,
) -> Vec<Vehicle<'a>> {
    let mut out = vec![];
    for (k, t) in tt.trips.iter().enumerate() {
        if canceled.contains(&k) {
            continue;
        }
        let d = delays.get(&k);
        let at = |i: usize, v: i64| v + d.map(|d| d[i] as i64).unwrap_or(0);
        let n = t.arr.len();
        let (start, end) = (at(0, t.dep[0]), at(n - 1, t.arr[n - 1]));
        if end < now || start > now + after {
            continue;
        }
        let k0 = (0..n).rev().find(|&i| at(i, t.dep[i]) <= now).unwrap_or(0);
        out.push(Vehicle {
            id: &t.id,
            route: &t.route,
            shape: &t.shape,
            headsign: &t.headsign,
            k0,
            arr: (k0..n).map(|i| at(i, t.arr[i])).collect(),
            dep: (k0..n).map(|i| at(i, t.dep[i])).collect(),
            names: (k0..n).map(|i| &*t.names[i]).collect(),
            delay: d.map(|d| d[k0]),
        });
    }
    out
}

pub fn shapes_from_world(path: &Path) -> Result<HashMap<String, usize>> {
    let v: serde_json::Value = serde_json::from_slice(&std::fs::read(path).with_context(|| format!("{path:?}"))?)?;
    let mut out = HashMap::new();
    for (k, s) in v["shapes"].as_object().context("transit.json: no shapes")? {
        out.insert(k.clone(), s["s"].as_array().map(|a| a.len()).unwrap_or(0));
    }
    Ok(out)
}

pub fn today() -> NaiveDate {
    chrono::Utc::now().with_timezone(&Paris).date_naive()
}

#[allow(dead_code)]
pub fn is_weekend(d: NaiveDate) -> bool {
    matches!(d.weekday(), Weekday::Sat | Weekday::Sun)
}
