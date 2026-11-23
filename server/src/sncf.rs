//! Trains at Nantes station: SNCF GTFS (TGV INOUI, OUIGO, Intercités, TER) + SNCF GTFS-RT delays, both
//! open data (transport.data.gouv.fr). Gives the departure / arrival board and, for the map, every train
//! currently between Nantes and its previous or next stop (the viewer places it on the matching rail line).

use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{Duration, NaiveDate, TimeZone};
use chrono_tz::Europe::Paris;
use serde::Serialize;

use crate::feeds::Origin;
use crate::l93;

pub const GTFS_URL: &str = "https://eu.ftp.opendatasoft.com/sncf/plandata/Export_OpenData_SNCF_GTFS_NewTripId.zip";
pub const RT_URL: &str = "https://proxy.transport.data.gouv.fr/resource/sncf-gtfs-rt-trip-updates";
pub const UIC: &str = "87481002"; // Nantes

#[derive(Clone, Serialize)]
pub struct Stop {
    pub name: String,
    pub x: f64,
    pub y: f64,
    pub arr: i64,
    pub dep: i64,
}

pub struct Train {
    pub id: String,
    pub number: String,
    pub mode: String,
    pub stops: Vec<Stop>,
    pub stop_ids: Vec<String>,
    /// index of Nantes in `stops`
    pub at: usize,
}

pub struct Day {
    pub date: NaiveDate,
    pub trains: Vec<Train>,
    pub by_id: HashMap<String, Vec<usize>>,
}

pub async fn ensure(http: &reqwest::Client, dir: &Path) -> Result<()> {
    if let Ok(m) = std::fs::metadata(dir.join("stop_times.txt")) {
        if m.modified()?.elapsed().map(|e| e.as_secs() < 24 * 3600).unwrap_or(false) {
            return Ok(());
        }
    }
    let bytes = http.get(GTFS_URL).send().await?.error_for_status()?.bytes().await?;
    let dir = dir.to_path_buf();
    tokio::task::spawn_blocking(move || -> Result<()> {
        std::fs::create_dir_all(&dir)?;
        let mut z = zip::ZipArchive::new(std::io::Cursor::new(bytes))?;
        for i in 0..z.len() {
            let mut f = z.by_index(i)?;
            let name = PathBuf::from(f.name()).file_name().context("zip")?.to_owned();
            let mut buf = Vec::new();
            f.read_to_end(&mut buf)?;
            std::fs::write(dir.join(name), buf)?;
        }
        Ok(())
    })
    .await??;
    tracing::info!("SNCF GTFS downloaded");
    Ok(())
}

fn rdr(dir: &Path, n: &str) -> Result<csv::Reader<std::fs::File>> {
    Ok(csv::ReaderBuilder::new().flexible(true).from_path(dir.join(n)).with_context(|| n.to_string())?)
}
fn col(h: &csv::StringRecord, n: &str) -> Result<usize> {
    h.iter().position(|c| c.trim_start_matches('\u{feff}') == n).with_context(|| format!("no column {n}"))
}
fn secs(t: &str) -> Option<i64> {
    let mut it = t.split(':').map(|p| p.trim().parse::<i64>());
    Some(it.next()?.ok()? * 3600 + it.next()?.ok()? * 60 + it.next()?.ok()?)
}
fn midnight(d: NaiveDate) -> i64 {
    Paris.from_local_datetime(&d.and_hms_opt(0, 0, 0).unwrap()).earliest().unwrap().timestamp()
}

pub fn load(dir: &Path, day: NaiveDate, o: Origin) -> Result<Day> {
    let (prev, next) = (day - Duration::days(1), day + Duration::days(1));
    let (ymd, ymd_prev) = (day.format("%Y%m%d").to_string(), prev.format("%Y%m%d").to_string());
    let ymd_next = next.format("%Y%m%d").to_string();
    let mut on: HashMap<String, Vec<(i64, bool)>> = HashMap::new();
    let mut r = rdr(dir, "calendar_dates.txt")?;
    let h = r.headers()?.clone();
    let (sid, dc, ex) = (col(&h, "service_id")?, col(&h, "date")?, col(&h, "exception_type")?);
    for rec in r.records() {
        let rec = rec?;
        if &rec[ex] != "1" {
            continue;
        }
        if rec[dc] == *ymd {
            on.entry(rec[sid].to_string()).or_default().push((midnight(day), false));
        } else if rec[dc] == *ymd_prev {
            on.entry(rec[sid].to_string()).or_default().push((midnight(prev), true));
        } else if rec[dc] == *ymd_next {
            // tomorrow's trains, so the board isn't empty late in the evening
            on.entry(rec[sid].to_string()).or_default().push((midnight(next), false));
        }
    }
    let mut stops: HashMap<String, (String, f64, f64)> = HashMap::new();
    let mut r = rdr(dir, "stops.txt")?;
    let h = r.headers()?.clone();
    let (id, nm, la, lo) = (col(&h, "stop_id")?, col(&h, "stop_name")?, col(&h, "stop_lat")?, col(&h, "stop_lon")?);
    for rec in r.records() {
        let rec = rec?;
        let (x, y) = l93::project(rec[lo].parse().unwrap_or(0.0), rec[la].parse().unwrap_or(0.0));
        let name = rec[nm].split(" Hall ").next().unwrap_or(&rec[nm]).trim().to_string(); // "Paris Montparnasse Hall 1 - 2"
        stops.insert(rec[id].to_string(), (name, x - o.0, y - o.1));
    }
    let mut trips: HashMap<String, Vec<(i64, bool)>> = HashMap::new();
    let mut r = rdr(dir, "trips.txt")?;
    let h = r.headers()?.clone();
    let (tid, sv) = (col(&h, "trip_id")?, col(&h, "service_id")?);
    for rec in r.records() {
        let rec = rec?;
        if let Some(b) = on.get(&rec[sv]) {
            trips.insert(rec[tid].to_string(), b.clone());
        }
    }
    let mut rows: HashMap<String, Vec<(u32, i64, i64, String)>> = HashMap::new();
    let mut via_nantes: HashSet<String> = HashSet::new();
    let mut r = rdr(dir, "stop_times.txt")?;
    let h = r.headers()?.clone();
    let (tid, ar, de, st, sq) = (col(&h, "trip_id")?, col(&h, "arrival_time")?, col(&h, "departure_time")?, col(&h, "stop_id")?, col(&h, "stop_sequence")?);
    for rec in r.records() {
        let rec = rec?;
        if !trips.contains_key(&rec[tid]) {
            continue;
        }
        if rec[st].ends_with(UIC) {
            via_nantes.insert(rec[tid].to_string());
        }
        let (Some(a), Some(d)) = (secs(&rec[ar]), secs(&rec[de])) else { continue };
        rows.entry(rec[tid].to_string()).or_default().push((rec[sq].parse().unwrap_or(0), a, d, rec[st].to_string()));
    }
    let mut out = vec![];
    let mut by_id: HashMap<String, Vec<usize>> = HashMap::new();
    for tid in via_nantes {
        let mut v = rows.remove(&tid).unwrap_or_default();
        v.sort_by_key(|s| s.0);
        let Some(at) = v.iter().position(|s| s.3.ends_with(UIC)) else { continue };
        let mode = v[at].3.trim_start_matches("StopPoint:OCE").rsplit_once('-').map(|x| x.0.to_string()).unwrap_or_default();
        let number = tid.trim_start_matches("OCESN").chars().take_while(|c| c.is_ascii_digit()).collect::<String>();
        for &(base, is_prev) in &trips[&tid] {
            if is_prev && v.last().map(|s| s.2).unwrap_or(0) < 86_400 {
                continue;
            }
            by_id.entry(tid.clone()).or_default().push(out.len());
            out.push(Train {
                id: tid.clone(),
                number: number.clone(),
                mode: mode.clone(),
                stop_ids: v.iter().map(|s| s.3.clone()).collect(),
                stops: v
                    .iter()
                    .map(|s| {
                        let (name, x, y) = stops.get(&s.3).cloned().unwrap_or_default();
                        Stop { name, x: (x * 10.0).round() / 10.0, y: (y * 10.0).round() / 10.0, arr: base + s.1, dep: base + s.2 }
                    })
                    .collect(),
                at,
            });
        }
    }
    tracing::info!("SNCF {day}: {} trains through Nantes", out.len());
    Ok(Day { date: day, trains: out, by_id })
}

/// Delay (s) at each stop of each train, from the SNCF trip-updates feed.
pub fn delays(day: &Day, feed: &crate::rt::FeedMessage) -> HashMap<usize, Vec<i32>> {
    let mut out = HashMap::new();
    for e in &feed.entity {
        let Some(tu) = &e.trip_update else { continue };
        let Some(tid) = tu.trip.as_ref().and_then(|t| t.trip_id.as_deref()) else { continue };
        let Some(ks) = day.by_id.get(tid) else { continue };
        for &k in ks {
            let t = &day.trains[k];
            let mut d = vec![i32::MIN; t.stops.len()];
            for u in &tu.stop_time_update {
                let i = u.stop_id.as_deref().and_then(|s| t.stop_ids.iter().position(|q| q == s));
                let Some(i) = i else { continue };
                let ev = u.departure.as_ref().or(u.arrival.as_ref());
                if let Some(v) = ev.and_then(|e| e.delay.or_else(|| e.time.map(|x| (x - t.stops[i].dep) as i32))) {
                    d[i] = v;
                }
            }
            let mut last = 0;
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
    out
}

#[derive(Serialize)]
pub struct BoardRow {
    pub number: String,
    pub mode: String,
    /// origin (arrivals) or destination (departures)
    pub other: String,
    pub time: i64,
    pub delay: i32,
}

#[derive(Serialize)]
pub struct Board {
    pub arrivals: Vec<BoardRow>,
    pub departures: Vec<BoardRow>,
}

pub fn board(day: &Day, delays: &HashMap<usize, Vec<i32>>, now: i64) -> Board {
    let mut b = Board { arrivals: vec![], departures: vec![] };
    for (k, t) in day.trains.iter().enumerate() {
        let d = delays.get(&k).map(|v| v[t.at]).unwrap_or(0);
        let s = &t.stops[t.at];
        if t.at > 0 && s.arr + d as i64 > now - 300 && s.arr < now + 12 * 3600 {
            b.arrivals.push(BoardRow { number: t.number.clone(), mode: t.mode.clone(), other: t.stops[0].name.clone(), time: s.arr, delay: d });
        }
        if t.at + 1 < t.stops.len() && s.dep + d as i64 > now - 120 && s.dep < now + 12 * 3600 {
            b.departures.push(BoardRow { number: t.number.clone(), mode: t.mode.clone(), other: t.stops.last().unwrap().name.clone(), time: s.dep, delay: d });
        }
    }
    b.arrivals.sort_by_key(|r| r.time);
    b.departures.sort_by_key(|r| r.time);
    b.arrivals.truncate(30);
    b.departures.truncate(30);
    b
}

#[derive(Serialize)]
pub struct Moving {
    pub id: String,
    pub number: String,
    pub mode: String,
    pub headsign: String,
    /// the leg the train is on, each end with realtime-corrected times: (from, to)
    pub from: Stop,
    pub to: Stop,
    pub delay: i32,
}

/// Trains on a leg that touches Nantes (arriving, standing at the platform, or leaving).
pub fn moving(day: &Day, delays: &HashMap<usize, Vec<i32>>, now: i64) -> Vec<Moving> {
    let mut out = vec![];
    for (k, t) in day.trains.iter().enumerate() {
        let d = delays.get(&k);
        let fix = |i: usize, s: &Stop| {
            let dd = d.map(|v| v[i] as i64).unwrap_or(0);
            Stop { name: s.name.clone(), x: s.x, y: s.y, arr: s.arr + dd, dep: s.dep + dd }
        };
        let a = t.at;
        let legs = [(a.checked_sub(1), Some(a)), (Some(a), (a + 1 < t.stops.len()).then_some(a + 1))];
        for (i, j) in legs {
            let (Some(i), Some(j)) = (i, j) else { continue };
            let (from, to) = (fix(i, &t.stops[i]), fix(j, &t.stops[j]));
            // on this leg now (for the Nantes end, include the dwell at the platform)
            let start = if i == a { from.arr } else { from.dep };
            if now >= start && now <= to.arr {
                out.push(Moving {
                    id: t.id.clone(),
                    number: t.number.clone(),
                    mode: t.mode.clone(),
                    headsign: t.stops.last().map(|s| s.name.clone()).unwrap_or_default(),
                    from,
                    to,
                    delay: d.map(|v| v[a]).unwrap_or(0),
                });
                break;
            }
        }
    }
    out
}
