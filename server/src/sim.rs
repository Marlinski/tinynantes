//! The living city, simulated on the server (clients only draw it): cars, pedestrians and bikes moving on the real
//! OSM network.
//!
//! - Cars fill the measured road sections (Nantes Métropole traffic feed): on each section, the number of cars
//!   present is flow / speed × length, and they drive it at the measured speed, then wander on to a nearby
//!   destination. Cars follow each other (they slow down and queue behind the car ahead on the same street).
//! - Pedestrians come out of tram and bus stops or start near places, walk to a place that is open at this hour,
//!   stay inside for a while, then walk back to a stop and leave. How many depends on the hour, the day and the
//!   weather.
//! - Bikes ride between places on streets and cycleways.
//!
//! Every inhabitant has a stable id and a personality seed (a first name for now; habits and memories later).
//! The state is published 4 times a second; each viewer receives what lies in its view (WebSocket /api/world).

use std::collections::{BinaryHeap, HashMap};
use std::cmp::Ordering;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::{Datelike, Timelike};
use rand::rngs::SmallRng;
use rand::{Rng, SeedableRng};
use serde::Deserialize;
use tokio::sync::{watch, RwLock};

use crate::people::{self, Persona, Quartiers, Ride, TransitIndex};

// ---------------------------------------------------------------- network

const DRIVE: u8 = 1;
const WALK: u8 = 2;
const BIKE: u8 = 4;

fn modes(kind: &str) -> u8 {
    match kind {
        "motorway" | "trunk" | "motorway_link" | "trunk_link" => DRIVE,
        "primary" | "secondary" | "tertiary" | "primary_link" | "secondary_link" | "tertiary_link" | "unclassified"
        | "residential" => DRIVE | WALK | BIKE,
        "living_street" | "service" => DRIVE | WALK | BIKE,
        "cycleway" => BIKE | WALK,
        "pedestrian" | "footway" | "path" | "steps" | "corridor" | "track" => WALK,
        _ => 0,
    }
}
/// typical car speed (m/s) by road class, used for routing and as the limit off measured sections
fn car_speed(kind: &str) -> f32 {
    match kind {
        "motorway" | "trunk" => 22.0,
        "motorway_link" | "trunk_link" | "primary" => 13.0,
        "secondary" | "primary_link" | "secondary_link" => 12.0,
        "tertiary" | "tertiary_link" | "unclassified" => 10.0,
        "residential" => 8.0,
        "living_street" | "service" => 5.0,
        _ => 6.0,
    }
}

struct Edge {
    to: u32,
    len: f32,
    modes: u8, // modes allowed in this direction
    vmax: f32,
}

pub struct Graph {
    pub xyz: Vec<[f32; 3]>,
    adj: Vec<Vec<Edge>>,
    grid: HashMap<(i32, i32), Vec<u32>>,
}

const CELL: f32 = 40.0;

#[derive(Deserialize)]
struct RoadsFile {
    roads: Vec<Road>,
}
#[derive(Deserialize)]
struct Road {
    k: String,
    o: u8,
    id: Vec<i64>,
    p: Vec<[f32; 3]>,
}

impl Graph {
    pub fn load(path: &Path) -> Result<Graph> {
        let f: RoadsFile = serde_json::from_slice(&std::fs::read(path).with_context(|| format!("{path:?}"))?)?;
        let mut index: HashMap<i64, u32> = HashMap::new();
        let mut xyz = vec![];
        let mut adj: Vec<Vec<Edge>> = vec![];
        for r in &f.roads {
            let m = modes(&r.k);
            if m == 0 || r.id.len() != r.p.len() {
                continue;
            }
            let ids: Vec<u32> = r
                .id
                .iter()
                .zip(&r.p)
                .map(|(id, p)| {
                    *index.entry(*id).or_insert_with(|| {
                        xyz.push(*p);
                        adj.push(vec![]);
                        (xyz.len() - 1) as u32
                    })
                })
                .collect();
            let v = car_speed(&r.k);
            for w in ids.windows(2) {
                let (a, b) = (w[0], w[1]);
                let (pa, pb) = (xyz[a as usize], xyz[b as usize]);
                let len = ((pa[0] - pb[0]).powi(2) + (pa[1] - pb[1]).powi(2)).sqrt().max(0.1);
                // one-way streets: cars only one way, people both ways
                let back = if r.o == 1 { m & !DRIVE } else { m };
                adj[a as usize].push(Edge { to: b, len, modes: m, vmax: v });
                if back != 0 {
                    adj[b as usize].push(Edge { to: a, len, modes: back, vmax: v });
                }
            }
        }
        let mut grid: HashMap<(i32, i32), Vec<u32>> = HashMap::new();
        for (i, p) in xyz.iter().enumerate() {
            grid.entry(((p[0] / CELL).floor() as i32, (p[1] / CELL).floor() as i32)).or_default().push(i as u32);
        }
        tracing::info!("sim network: {} nodes", xyz.len());
        Ok(Graph { xyz, adj, grid })
    }

    fn node_modes(&self, n: u32) -> u8 {
        self.adj[n as usize].iter().fold(0, |m, e| m | e.modes)
    }

    /// nearest node usable by `mode` within `r` metres
    pub fn nearest(&self, x: f32, y: f32, mode: u8, r: f32) -> Option<u32> {
        let (cx, cy) = ((x / CELL).floor() as i32, (y / CELL).floor() as i32);
        let k = (r / CELL).ceil() as i32;
        let mut best = None;
        let mut bd = r * r;
        for gx in cx - k..=cx + k {
            for gy in cy - k..=cy + k {
                for &n in self.grid.get(&(gx, gy)).into_iter().flatten() {
                    let p = self.xyz[n as usize];
                    let d = (p[0] - x).powi(2) + (p[1] - y).powi(2);
                    if d < bd && self.node_modes(n) & mode != 0 {
                        bd = d;
                        best = Some(n);
                    }
                }
            }
        }
        best
    }

    /// A* shortest path for `mode` (time for cars, distance otherwise); gives up after `budget` expansions
    pub fn route(&self, a: u32, b: u32, mode: u8, budget: usize) -> Option<Vec<u32>> {
        #[derive(PartialEq)]
        struct Q(f32, u32);
        impl Eq for Q {}
        impl PartialOrd for Q {
            fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
                Some(self.cmp(o))
            }
        }
        impl Ord for Q {
            fn cmp(&self, o: &Self) -> Ordering {
                o.0.partial_cmp(&self.0).unwrap_or(Ordering::Equal)
            }
        }
        let goal = self.xyz[b as usize];
        let h = |n: u32| {
            let p = self.xyz[n as usize];
            let d = ((p[0] - goal[0]).powi(2) + (p[1] - goal[1]).powi(2)).sqrt();
            if mode == DRIVE { d / 22.0 } else { d }
        };
        let mut g: HashMap<u32, f32> = HashMap::new();
        let mut prev: HashMap<u32, u32> = HashMap::new();
        let mut q = BinaryHeap::new();
        g.insert(a, 0.0);
        q.push(Q(h(a), a));
        let mut n_exp = 0;
        while let Some(Q(_, n)) = q.pop() {
            if n == b {
                let mut path = vec![b];
                while let Some(&p) = prev.get(path.last().unwrap()) {
                    path.push(p);
                }
                path.reverse();
                return Some(path);
            }
            n_exp += 1;
            if n_exp > budget {
                return None;
            }
            let gn = g[&n];
            for e in &self.adj[n as usize] {
                if e.modes & mode == 0 {
                    continue;
                }
                let c = if mode == DRIVE { e.len / e.vmax } else { e.len };
                let ng = gn + c;
                if ng < *g.get(&e.to).unwrap_or(&f32::INFINITY) {
                    g.insert(e.to, ng);
                    prev.insert(e.to, n);
                    q.push(Q(ng + h(e.to), e.to));
                }
            }
        }
        None
    }

    fn edge_vmax(&self, a: u32, b: u32) -> f32 {
        self.adj[a as usize].iter().find(|e| e.to == b).map(|e| e.vmax).unwrap_or(8.0)
    }
}

// ---------------------------------------------------------------- inputs from the other feeds

#[derive(Deserialize, Clone)]
#[allow(dead_code)]
pub struct SectionIn {
    pub id: i64,
    pub flow: Option<f64>,
    pub speed: Option<f64>,
    pub p: Vec<[f64; 2]>,
}

#[derive(Deserialize, Clone)]
pub struct PoiIn {
    pub id: String,
    pub k: String,
    pub t: HashMap<String, String>,
    pub p: [f32; 3],
}

#[derive(Default, Clone)]
pub struct Inputs {
    pub sections: Vec<SectionIn>,
    /// mm/h of rain right now
    pub rain: f32,
}

// ---------------------------------------------------------------- agents

#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Kind {
    Car = 0,
    Walker = 1,
    Bike = 2,
    /// ambulance, police or fire engine; its variant byte = service (0 ambulance, 1 police, 2 fire) + 4 × siren
    Emergency = 3,
    /// a pedestrian in a demonstration; variant = role (0 marcher, 1 batucada drummer, 2 placard) + 4 × colour
    Marcher = 4,
}

/// Where emergency vehicles start from and return to (web/data/world/bases.json).
#[derive(Deserialize, Clone)]
pub struct BaseIn {
    pub k: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub er: bool,
    pub p: [f32; 3],
}
struct Base {
    service: u8,
    node: u32,
    name: String,
    /// relative chance of being picked (hospitals with an emergency department take most ambulances)
    weight: f32,
}
const MAX_EMERGENCY: usize = 8;

#[derive(Clone, Copy, PartialEq)]
enum Plan {
    /// on the way; on arrival: go inside `poi` (walkers, bikes) or leave the map
    Going,
    Inside { until: f64 },
    Leaving,
    /// at the stop, until the tram/bus/Navibus leaves
    Waiting { until: f64 },
    /// on board (not drawn) until it reaches the stop they get off at
    Riding { until: f64 },
}

struct Agent {
    id: u32,
    kind: Kind,
    seed: u32,
    path: Vec<u32>,
    i: usize,  // current segment path[i] -> path[i+1]
    s: f32,    // metres along it
    v: f32,
    vpref: f32,
    /// cars on a measured section: its index, and the measured speed while on its first `sec_until` segments
    section: Option<usize>,
    sec_until: usize,
    sec_speed: f32,
    poi: Option<usize>,
    plan: Plan,
    /// emergency vehicles: service (0 ambulance, 1 police, 2 fire), siren on, the base it is tied to
    service: u8,
    siren: bool,
    base: Option<usize>,
    /// pedestrians and cyclists: who they are
    persona: Option<Box<Persona>>,
    /// the ride they are on or about to take, and where they walk once off
    ride: Option<Ride>,
    /// stopped to talk with a player until then, facing this way
    hold_until: f64,
    face: f32,
    /// spawned from the debug panel (removed together), and a small offset from the path (crowds spread out)
    dbg: bool,
    jit: [f32; 2],
}

/// What clients get for one inhabitant: [id, kind, x, y, z, heading, speed, colour/variant]
pub type Packed = (u32, u8, f32, f32, f32, f32, f32, u8);

pub struct Snapshot {
    pub at: f64,
    pub items: Vec<Packed>,
}

pub struct Sim {
    g: Arc<Graph>,
    pois: Vec<PoiIn>,
    stops: Vec<u32>,
    agents: Vec<Agent>,
    next_id: u32,
    rng: SmallRng,
    target_per_section: Vec<f32>,
    sections: Vec<SectionIn>,
    rain: f32,
    bases: Vec<Base>,
    quartiers: Quartiers,
    /// per place: how many places within 300 m (the lively centre draws more people)
    liveliness: Vec<f32>,
    /// the railway station (people arriving by train)
    station: Option<[f32; 2]>,
    transit: Option<Arc<TransitIndex>>,
    march: Option<March>,
    hour: u32,
}

/// A demonstration going through the centre: its route, why, and since when.
pub struct March {
    pub motive: String,
    route: Vec<u32>,
    pub until: f64,
    /// how many marchers at once
    size: usize,
    /// started from the debug panel: the automatic schedule leaves it alone until it ends
    pinned: bool,
}

const FIRST_NAMES: &[&str] = &[
    "Camille", "Léa", "Manon", "Chloé", "Inès", "Jade", "Louise", "Alice", "Lina", "Rose", "Anna", "Mila", "Zoé",
    "Juliette", "Lucie", "Margaux", "Clara", "Sarah", "Nina", "Elsa", "Gabriel", "Louis", "Raphaël", "Jules", "Adam",
    "Lucas", "Léo", "Hugo", "Arthur", "Nathan", "Paul", "Tom", "Noah", "Ethan", "Sacha", "Malo", "Yanis", "Maël",
    "Gaspard", "Erwan", "Yann", "Gwen", "Nolwenn", "Loïc", "Soizic", "Ronan", "Morgane", "Brieuc", "Anaïs", "Mathis",
];
pub fn name_of(seed: u32) -> &'static str {
    FIRST_NAMES[(seed as usize) % FIRST_NAMES.len()]
}

/// relative number of people out at this hour (0..1), weekday vs weekend
fn crowd(hour: u32, weekend: bool) -> f32 {
    const WD: [f32; 24] = [0.06, 0.04, 0.02, 0.02, 0.03, 0.08, 0.25, 0.55, 0.7, 0.55, 0.55, 0.7, 0.95, 0.9, 0.7, 0.7,
        0.8, 1.0, 0.95, 0.8, 0.6, 0.45, 0.3, 0.15];
    const WE: [f32; 24] = [0.15, 0.1, 0.06, 0.03, 0.02, 0.03, 0.06, 0.12, 0.25, 0.45, 0.65, 0.8, 0.9, 0.85, 0.9, 1.0,
        1.0, 0.95, 0.85, 0.7, 0.6, 0.5, 0.4, 0.25];
    (if weekend { WE } else { WD })[hour as usize % 24]
}

/// How much a kind of place draws people at this hour (the density map: meals, shops on Saturdays, bars at
/// night, museums at weekends...). 1 = ordinary.
fn attract(k: &str, hour: u32, weekend: bool) -> f32 {
    let h = hour;
    let sat = weekend; // Saturdays and Sundays alike, shops are closed on Sundays anyway (open())
    match k {
        "amenity=restaurant" if (12..14).contains(&h) || (19..22).contains(&h) => 3.5,
        "amenity=restaurant" => 0.6,
        "amenity=fast_food" if (12..14).contains(&h) || (19..23).contains(&h) => 3.0,
        "amenity=cafe" if (7..10).contains(&h) => 2.5,
        "amenity=cafe" if sat && (10..18).contains(&h) => 2.2,
        "shop=bakery" if (7..9).contains(&h) || (17..20).contains(&h) => 2.5,
        "shop=bakery" if sat && (8..13).contains(&h) => 3.0,
        "amenity=bar" | "amenity=pub" | "amenity=biergarten" if (18..24).contains(&h) || h < 2 => if sat { 4.5 } else { 3.0 },
        "amenity=nightclub" => if sat { 3.0 } else { 1.0 },
        "amenity=marketplace" if (8..13).contains(&h) => if sat { 5.0 } else { 3.0 },
        "amenity=cinema" | "amenity=theatre" if (19..23).contains(&h) => 3.0,
        "amenity=cinema" if sat && (14..19).contains(&h) => 2.5,
        "tourism=museum" | "tourism=gallery" | "tourism=attraction" | "tourism=viewpoint" => if sat { 3.5 } else { 1.5 },
        "leisure=sports_centre" if (18..22).contains(&h) || (sat && (9..13).contains(&h)) => 2.0,
        k if k.starts_with("shop=") && (11..19).contains(&h) => if sat { 3.0 } else { 1.4 },
        _ => 1.0,
    }
}

/// Is this kind of place open at this hour? (opening_hours parsing is for later; typical French hours)
fn open(k: &str, hour: u32, sunday: bool) -> bool {
    let h = hour;
    match k {
        "amenity=restaurant" => (11..15).contains(&h) || (18..23).contains(&h),
        "amenity=fast_food" => (11..23).contains(&h),
        "amenity=bar" | "amenity=pub" | "amenity=biergarten" => h >= 16 || h < 2,
        "amenity=nightclub" => h >= 23 || h < 5,
        "amenity=cafe" | "shop=bakery" => (7..20).contains(&h),
        "amenity=cinema" | "amenity=theatre" => (13..24).contains(&h),
        "amenity=ice_cream" => (12..21).contains(&h),
        k if k.starts_with("shop=") => !sunday && (9..19).contains(&h),
        k if k.starts_with("tourism=") || k.starts_with("amenity=") || k.starts_with("leisure=") => (9..19).contains(&h),
        _ => false,
    }
}

impl Sim {
    pub fn new(g: Arc<Graph>, pois: Vec<PoiIn>, stations: &[[f32; 3]], bases: &[BaseIn]) -> Sim {
        let stops = stations.iter().filter_map(|p| g.nearest(p[0], p[1], WALK, 80.0)).collect();
        let bases = bases
            .iter()
            .filter_map(|b| {
                let service = match b.k.as_str() { "hospital" => 0, "police" => 1, "fire" => 2, _ => return None };
                let node = g.nearest(b.p[0], b.p[1], DRIVE, 150.0)?;
                let weight = match (service, b.er) { (0, true) => 6.0, (0, false) => 0.6, (1, _) => 1.0, _ => 1.5 };
                Some(Base { service, node, name: b.name.clone(), weight })
            })
            .collect();
        let pois: Vec<PoiIn> = pois.into_iter().filter(|p| p.t.contains_key("name")).collect();
        let mut grid: HashMap<(i32, i32), u32> = HashMap::new();
        for p in &pois {
            *grid.entry(((p.p[0] / 300.0).floor() as i32, (p.p[1] / 300.0).floor() as i32)).or_default() += 1;
        }
        let liveliness = pois.iter().map(|p| {
            let (cx, cy) = ((p.p[0] / 300.0).floor() as i32, (p.p[1] / 300.0).floor() as i32);
            let mut n = 0;
            for i in -1..=1 { for j in -1..=1 { n += grid.get(&(cx + i, cy + j)).copied().unwrap_or(0); } }
            n as f32 / 3.0
        }).collect();
        Sim {
            g,
            liveliness,
            station: None,
            pois,
            stops,
            agents: vec![],
            next_id: 1,
            rng: SmallRng::from_entropy(),
            target_per_section: vec![],
            sections: vec![],
            rain: 0.0,
            bases,
            quartiers: Quartiers::default(),
            transit: None,
            march: None,
            hour: 12,
        }
    }

    pub fn set_station(&mut self, p: [f32; 2]) {
        self.station = Some(p);
    }
    pub fn set_quartiers(&mut self, q: Quartiers) {
        self.quartiers = q;
    }
    pub fn set_transit(&mut self, t: Arc<TransitIndex>) {
        self.transit = Some(t);
    }
    /// For /metrics: inhabitants on the map by kind (car, walker, bike, emergency, marcher).
    pub fn census(&self) -> [usize; 5] {
        let mut n = [0; 5];
        for a in &self.agents {
            n[a.kind as usize] += 1;
        }
        n
    }

    pub fn marching(&self) -> Option<&str> {
        self.march.as_ref().map(|m| m.motive.as_str())
    }

    /// Start (or stop, with None) a demonstration along `waypoints` (local metres) until `until`.
    pub fn set_march(&mut self, m: Option<(String, Vec<[f32; 2]>, f64)>) {
        let now = chrono::Utc::now().timestamp() as f64;
        if self.march.as_ref().is_some_and(|x| x.pinned && now < x.until) {
            return; // a march started from the debug panel
        }
        let Some((motive, wps, until)) = m else {
            self.march = None;
            return;
        };
        if self.march.as_ref().is_some_and(|x| x.motive == motive) {
            return;
        }
        let route = self.walk_route(&wps);
        if route.len() >= 2 {
            tracing::info!("march: {motive} ({} nodes)", route.len());
            self.march = Some(March { motive, route, until, size: 160, pinned: false });
        }
    }

    /// A walking route through `wps` (local metres), each snapped to the nearest footpath within 120 m.
    fn walk_route(&self, wps: &[[f32; 2]]) -> Vec<u32> {
        let nodes: Vec<u32> = wps.iter().filter_map(|w| self.g.nearest(w[0], w[1], WALK, 120.0)).collect();
        let mut route: Vec<u32> = vec![];
        for w in nodes.windows(2) {
            if let Some(p) = self.g.route(w[0], w[1], WALK, 20000) {
                if route.last() == p.first() {
                    route.extend_from_slice(&p[1..]);
                } else {
                    route.extend(p);
                }
            }
        }
        route
    }

    // ------------------------------------------------------------ debug panel
    /// A demonstration along a drawn route, `size` marchers at once, for `secs`. Returns the route length (m).
    pub fn debug_march(&mut self, motive: String, wps: &[[f32; 2]], secs: f64, size: usize) -> Option<f32> {
        let route = self.walk_route(wps);
        if route.len() < 2 {
            return None;
        }
        let len = route.windows(2).map(|w| {
            let (a, b) = (self.g.xyz[w[0] as usize], self.g.xyz[w[1] as usize]);
            ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)).sqrt()
        }).sum();
        self.agents.retain(|a| a.kind != Kind::Marcher);
        let until = chrono::Utc::now().timestamp() as f64 + secs;
        tracing::info!("debug march: {motive} ({len:.0} m, {size} marchers)");
        self.march = Some(March { motive, route, until, size, pinned: true });
        Some(len)
    }

    /// A crowd of `n` around (x, y) within `r` m for `secs`: strolling about, or gathered facing the centre.
    pub fn debug_crowd(&mut self, x: f32, y: f32, n: usize, r: f32, secs: f64, gather: bool) -> usize {
        let now = chrono::Utc::now().timestamp_millis() as f64 / 1000.0;
        let mut made = 0;
        for _ in 0..n * 3 {
            if made >= n {
                break;
            }
            let Some(a) = self.random_node_near(x, y, r, WALK) else { continue };
            let path = if gather {
                vec![a, a]
            } else {
                // a chain of short legs between points of the area, long enough to last `secs`
                let v = 0.9;
                let mut path = vec![a];
                let mut walked = 0.0;
                for _ in 0..40 {
                    if walked > v * secs as f32 {
                        break;
                    }
                    let here = *path.last().unwrap();
                    let Some(b) = self.random_node_near(x, y, r, WALK) else { continue };
                    if let Some(p) = self.g.route(here, b, WALK, 3000) {
                        walked += p.windows(2).map(|w| {
                            let (a, b) = (self.g.xyz[w[0] as usize], self.g.xyz[w[1] as usize]);
                            ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)).sqrt()
                        }).sum::<f32>();
                        path.extend_from_slice(&p[1..]);
                    }
                }
                if path.len() < 2 {
                    continue;
                }
                path
            };
            let seed: u32 = self.rng.gen();
            let persona = people::persona(&self.quartiers, people::PersonaInput {
                seed, hour: self.hour, home: None, place: None, trip: people::Trip::Walk, march: None });
            // gathered: spread around their footpath node so the crowd fills the place rather than queueing on nodes
            let spread = if gather { (r * 0.3).clamp(3.0, 12.0) } else { 3.0 };
            let jit = [self.rng.gen_range(-spread..spread), self.rng.gen_range(-spread..spread)];
            let v = self.rng.gen_range(0.6..1.2);
            let here = self.g.xyz[a as usize];
            let ag = self.spawn(Kind::Walker, path, v);
            ag.seed = seed;
            ag.persona = Some(Box::new(persona));
            ag.dbg = true;
            ag.jit = jit;
            if gather {
                // everyone faces the middle, a little turned this way or that
                ag.hold_until = now + secs;
                ag.face = (y - here[1] - jit[1]).atan2(x - here[0] - jit[0]) + (seed % 100) as f32 / 100.0 - 0.5;
            }
            made += 1;
        }
        tracing::info!("debug crowd: {made} at ({x:.0}, {y:.0}) r {r:.0}");
        made
    }

    /// Remove everything the debug panel added (crowds, and a march it started). Returns how many left.
    pub fn debug_stop(&mut self) -> usize {
        let pinned = self.march.as_ref().is_some_and(|m| m.pinned);
        if pinned {
            self.march = None;
        }
        let before = self.agents.len();
        self.agents.retain(|a| !a.dbg && !(pinned && a.kind == Kind::Marcher));
        before - self.agents.len()
    }

    pub fn set_inputs(&mut self, i: &Inputs) {
        self.rain = i.rain;
        if i.sections.is_empty() {
            return;
        }
        // cars present on a section = flow (veh/h) / speed (km/h) × length (km), both directions mixed
        self.target_per_section = i
            .sections
            .iter()
            .map(|s| {
                let len: f64 = s.p.windows(2).map(|w| ((w[0][0] - w[1][0]).powi(2) + (w[0][1] - w[1][1]).powi(2)).sqrt()).sum();
                match (s.flow, s.speed) {
                    (Some(f), Some(v)) if v > 1.0 => (f / v.max(5.0) * len / 1000.0).min(80.0) as f32,
                    _ => (len / 1000.0 * 4.0) as f32,
                }
            })
            .collect();
        self.sections = i.sections.clone();
    }

    fn spawn(&mut self, kind: Kind, path: Vec<u32>, vpref: f32) -> &mut Agent {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1).max(1);
        self.agents.push(Agent {
            id,
            kind,
            seed: self.rng.gen(),
            path,
            i: 0,
            s: 0.0,
            v: if kind == Kind::Car { vpref * 0.6 } else { vpref },
            vpref,
            section: None,
            sec_until: 0,
            sec_speed: 0.0,
            poi: None,
            plan: Plan::Going,
            service: 0,
            siren: false,
            base: None,
            persona: None,
            ride: None,
            hold_until: 0.0,
            face: 0.0,
            dbg: false,
            jit: [0.0; 2],
        });
        self.agents.last_mut().unwrap()
    }

    fn random_node_near(&mut self, x: f32, y: f32, r: f32, mode: u8) -> Option<u32> {
        let a = self.rng.gen::<f32>() * std::f32::consts::TAU;
        let d = self.rng.gen::<f32>().sqrt() * r;
        self.g.nearest(x + a.cos() * d, y + a.sin() * d, mode, 60.0)
    }

    fn spawn_cars(&mut self) {
        if self.sections.is_empty() {
            return;
        }
        let mut have = vec![0f32; self.sections.len()];
        for a in &self.agents {
            if let Some(s) = a.section {
                have[s] += 1.0;
            }
        }
        // a few per second, where the deficit is largest
        for _ in 0..12 {
            let k = self.rng.gen_range(0..self.sections.len());
            if have[k] + 0.5 >= self.target_per_section[k] {
                continue;
            }
            let s = self.sections[k].clone();
            let (first, last) = (s.p[0], s.p[s.p.len() - 1]);
            let (Some(a), Some(b)) = (
                self.g.nearest(first[0] as f32, first[1] as f32, DRIVE, 40.0),
                self.g.nearest(last[0] as f32, last[1] as f32, DRIVE, 40.0),
            ) else { continue };
            let Some(mut path) = self.g.route(a, b, DRIVE, 4000) else { continue };
            let sec_until = path.len().saturating_sub(1);
            // then on to a destination a little further
            let end = self.g.xyz[b as usize];
            if let Some(c) = self.random_node_near(end[0], end[1], 900.0, DRIVE) {
                if let Some(more) = self.g.route(b, c, DRIVE, 4000) {
                    path.extend_from_slice(&more[1..]);
                }
            }
            if path.len() < 2 {
                continue;
            }
            let speed = (s.speed.unwrap_or(30.0).max(5.0) / 3.6) as f32;
            // start somewhere along the section so a fresh section isn't empty at first
            let start = self.rng.gen_range(0..sec_until.max(1));
            let ag = self.spawn(Kind::Car, path, speed);
            ag.section = Some(k);
            ag.sec_until = sec_until;
            ag.sec_speed = speed;
            ag.i = start;
            have[k] += 1.0;
        }
    }

    /// Now and then an ambulance, a police car or a fire engine: called out from its base with the siren on, or
    /// back to it quietly; ambulances also bring patients in to the emergency departments, siren on.
    fn spawn_emergency(&mut self) {
        if self.bases.is_empty() || self.agents.iter().filter(|a| a.kind == Kind::Emergency).count() >= MAX_EMERGENCY {
            return;
        }
        if self.rng.gen::<f32>() > 0.12 {
            return; // about one every 8 s while below the cap
        }
        let total: f32 = self.bases.iter().map(|b| b.weight).sum();
        let mut r = self.rng.gen::<f32>() * total;
        let k = self.bases.iter().position(|b| { r -= b.weight; r <= 0.0 }).unwrap_or(0);
        let (service, home) = (self.bases[k].service, self.bases[k].node);
        let hp = self.g.xyz[home as usize];
        let Some(far) = self.random_node_near(hp[0], hp[1], 2500.0, DRIVE) else { return };
        // ambulances: half bring someone in (siren, towards the hospital); otherwise everyone leaves on a call
        // (siren) or comes home after one (quiet)
        let roll = self.rng.gen::<f32>();
        let (from, to, siren) = if service == 0 && roll < 0.5 { (far, home, true) } else if roll < 0.75 { (home, far, true) } else { (far, home, false) };
        let Some(path) = self.g.route(from, to, DRIVE, 8000) else { return };
        if path.len() < 3 {
            return;
        }
        let v = if siren { 15.0 } else { 9.0 };
        let ag = self.spawn(Kind::Emergency, path, v);
        ag.v = v * 0.7;
        ag.service = service;
        ag.siren = siren;
        ag.base = Some(k);
    }

    /// A place to go, drawn from the density map: open places, weighted by what draws people at this hour,
    /// by how lively the area is (places around it) and by distance.
    fn choose_poi_r(&mut self, x: f32, y: f32, hour: u32, sunday: bool, r: f32) -> Option<usize> {
        let weekend = sunday || chrono::Utc::now().with_timezone(&chrono_tz::Europe::Paris).weekday().num_days_from_monday() == 5;
        let mut cand: Vec<(usize, f32)> = vec![];
        for _ in 0..60 {
            let k = self.rng.gen_range(0..self.pois.len().max(1));
            let Some(p) = self.pois.get(k) else { continue };
            let d = ((p.p[0] - x).powi(2) + (p.p[1] - y).powi(2)).sqrt();
            if d < r && d > 80.0 && open(&p.k, hour, sunday) {
                let lively = 1.0 + self.liveliness.get(k).copied().unwrap_or(0.0) / 15.0;
                cand.push((k, attract(&p.k, hour, weekend) * lively / (1.0 + d / 1500.0)));
            }
        }
        let total: f32 = cand.iter().map(|c| c.1).sum();
        let mut t = self.rng.gen::<f32>() * total;
        cand.iter().find(|c| { t -= c.1; t <= 0.0 }).map(|c| c.0)
    }

    /// Someone just took a Naolib Vélo at this station (live open data): a cyclist leaves from there.
    pub fn spawn_rental(&mut self, at: [f32; 2], station: &str) -> bool {
        let Some(from) = self.g.nearest(at[0], at[1], BIKE, 150.0) else {
            tracing::debug!("rental at {station}: no bike way near the station");
            return false;
        };
        let hour = self.hour;
        let sunday = chrono::Utc::now().with_timezone(&chrono_tz::Europe::Paris).weekday().num_days_from_monday() == 6;
        let mut found = None;
        for _ in 0..5 {
            let Some(k) = self.choose_poi_r(at[0], at[1], hour, sunday, 2500.0) else { continue };
            let pp = self.pois[k].p;
            let Some(to) = self.g.nearest(pp[0], pp[1], BIKE, 120.0) else { continue };
            if let Some(path) = self.g.route(from, to, BIKE, 30000).filter(|p| p.len() >= 2) {
                found = Some((k, path));
                break;
            }
        }
        let Some((k, path)) = found else {
            tracing::debug!("rental at {station}: no route");
            return false;
        };
        let seed: u32 = self.rng.gen();
        let p = &self.pois[k];
        let name = p.t.get("name").cloned().unwrap_or_default();
        let persona = people::persona(&self.quartiers, people::PersonaInput {
            seed, hour, home: None, place: Some((&p.k, &name)),
            trip: people::Trip::Rental { from: station.to_string() }, march: None });
        let v = self.rng.gen_range(3.8..5.5);
        let ag = self.spawn(Kind::Bike, path, v);
        ag.seed = seed & !0xff00 | (8 << 8); // variant 8: a Naolib bike
        ag.poi = Some(k);
        ag.persona = Some(Box::new(persona));
        true
    }

    /// Keep the demonstration filled: marchers start at the head of the route in small groups, a batucada in
    /// the middle, placards here and there; they walk it at a demonstration's pace and disperse at the end.
    fn spawn_marchers(&mut self, now: f64) {
        let Some(m) = &self.march else { return };
        if now > m.until {
            self.march = None;
            return;
        }
        let (route, size) = (m.route.clone(), m.size);
        let n = self.agents.iter().filter(|a| a.kind == Kind::Marcher).count();
        if n >= size {
            return;
        }
        for _ in 0..(size / 50).clamp(3, 10) {
            let seed: u32 = self.rng.gen();
            let role = match self.rng.gen_range(0..100) { 0..=11 => 1u8, 12..=40 => 2, _ => 0 };
            let label = ["manifestant", "batucada", "pancarte"][role as usize];
            let persona = people::persona(&self.quartiers, people::PersonaInput {
                seed, hour: self.hour, home: None, place: None, trip: people::Trip::Walk, march: Some(label) });
            let v = self.rng.gen_range(0.7..0.95);
            let ag = self.spawn(Kind::Marcher, route.clone(), v);
            ag.seed = seed;
            ag.service = role;
            ag.persona = Some(Box::new(persona));
        }
    }

    fn choose_poi(&mut self, x: f32, y: f32, hour: u32, sunday: bool) -> Option<usize> {
        self.choose_poi_r(x, y, hour, sunday, 1500.0)
    }

    fn spawn_people(&mut self, hour: u32, weekend: bool, sunday: bool) {
        let c = crowd(hour, weekend);
        let wet = if self.rain > 0.3 { 0.55 } else { 1.0 };
        let walkers_target = (1800.0 * c * wet) as usize;
        let bikes_target = (320.0 * c * if self.rain > 0.3 { 0.35 } else { 1.0 }) as usize;
        let (mut walkers, mut bikes) = (0, 0);
        for a in &self.agents {
            match a.kind {
                Kind::Walker => walkers += 1,
                Kind::Bike => bikes += 1,
                _ => {}
            }
        }
        let now = chrono::Utc::now().timestamp();
        for _ in 0..6 {
            if walkers >= walkers_target || self.pois.is_empty() {
                break;
            }
            // from home: a point in their sub-quartier, to a place up to 4 km away, on foot or by tram/bus/Navibus
            let seed: u32 = self.rng.gen();
            let mut r = SmallRng::seed_from_u64(seed as u64);
            let Some((q, sq)) = self.quartiers.pick_home(&mut r) else { break };
            let Some(h) = self.quartiers.random_point(&mut r, q, sq) else { continue };
            let Some(from) = self.g.nearest(h[0], h[1], WALK, 150.0) else { continue };
            let fp = self.g.xyz[from as usize];
            let Some(k) = self.choose_poi_r(fp[0], fp[1], hour, sunday, 4000.0) else { continue };
            let pp = self.pois[k].p;
            let Some(to) = self.g.nearest(pp[0], pp[1], WALK, 60.0) else { continue };
            let dist = ((pp[0] - fp[0]).powi(2) + (pp[1] - fp[1]).powi(2)).sqrt();
            let ride = if dist > 1000.0 { self.transit.as_ref().and_then(|t| t.plan([fp[0], fp[1]], [pp[0], pp[1]], now)) } else { None };
            let (path, trip) = match &ride {
                Some(rd) => {
                    let Some(stop) = self.g.nearest(rd.on[0], rd.on[1], WALK, 120.0) else { continue };
                    (self.g.route(from, stop, WALK, 8000), people::Trip::Transit {
                        line: rd.line.clone(), kind: rd.kind.clone(), from: rd.on_name.clone(), to: rd.off_name.clone() })
                }
                None if dist < 2200.0 => (self.g.route(from, to, WALK, 8000), people::Trip::Walk),
                None => continue,
            };
            let Some(path) = path.filter(|p| p.len() >= 2) else { continue };
            let p = &self.pois[k];
            let name = p.t.get("name").cloned().unwrap_or_default();
            let persona = people::persona(&self.quartiers, people::PersonaInput {
                seed, hour, home: Some((q, sq)), place: Some((&p.k, &name)), trip, march: None });
            let v = self.rng.gen_range(1.1..1.55) * if persona.chatty < 0.25 { 1.15 } else { 1.0 };
            let ag = self.spawn(Kind::Walker, path, v);
            ag.seed = seed;
            ag.poi = Some(k);
            ag.ride = ride;
            ag.persona = Some(Box::new(persona));
            walkers += 1;
        }
        for _ in 0..4 {
            if walkers >= walkers_target || self.pois.is_empty() {
                break;
            }
            // off a train, from a stop (just got off) or from somewhere near a place
            let from = if let Some(sp) = self.station.filter(|_| (7..22).contains(&hour) && self.rng.gen_bool(0.3)) {
                let Some(n) = self.random_node_near(sp[0], sp[1], 60.0, WALK) else { continue };
                n
            } else if !self.stops.is_empty() && self.rng.gen_bool(0.5) {
                self.stops[self.rng.gen_range(0..self.stops.len())]
            } else {
                let p = self.pois[self.rng.gen_range(0..self.pois.len())].p;
                let Some(n) = self.random_node_near(p[0], p[1], 300.0, WALK) else { continue };
                n
            };
            let fp = self.g.xyz[from as usize];
            let Some(k) = self.choose_poi(fp[0], fp[1], hour, sunday) else { continue };
            let pp = self.pois[k].p;
            let Some(to) = self.g.nearest(pp[0], pp[1], WALK, 60.0) else { continue };
            let Some(path) = self.g.route(from, to, WALK, 6000) else { continue };
            if path.len() < 2 {
                continue;
            }
            let v = self.rng.gen_range(1.15..1.6);
            let seed: u32 = self.rng.gen();
            let p = &self.pois[k];
            let name = p.t.get("name").cloned().unwrap_or_default();
            let persona = people::persona(&self.quartiers, people::PersonaInput {
                seed, hour, home: None, place: Some((&p.k, &name)), trip: people::Trip::Walk, march: None });
            let ag = self.spawn(Kind::Walker, path, v);
            ag.seed = seed;
            ag.poi = Some(k);
            ag.persona = Some(Box::new(persona));
            walkers += 1;
        }
        for _ in 0..4 {
            if bikes >= bikes_target || self.pois.is_empty() {
                break;
            }
            let p = self.pois[self.rng.gen_range(0..self.pois.len())].p;
            let Some(from) = self.random_node_near(p[0], p[1], 400.0, BIKE) else { continue };
            let Some(k) = self.choose_poi(p[0], p[1], hour, sunday) else { continue };
            let pp = self.pois[k].p;
            let Some(to) = self.g.nearest(pp[0], pp[1], BIKE, 80.0) else { continue };
            let Some(path) = self.g.route(from, to, BIKE, 6000) else { continue };
            if path.len() < 2 {
                continue;
            }
            let v = self.rng.gen_range(4.0..6.0);
            let seed: u32 = self.rng.gen();
            let p = &self.pois[k];
            let name = p.t.get("name").cloned().unwrap_or_default();
            let persona = people::persona(&self.quartiers, people::PersonaInput {
                seed, hour, home: None, place: Some((&p.k, &name)), trip: people::Trip::Bike, march: None });
            let ag = self.spawn(Kind::Bike, path, v);
            ag.seed = seed;
            ag.poi = Some(k);
            ag.persona = Some(Box::new(persona));
            bikes += 1;
        }
    }

    /// Advance everyone by dt seconds.
    pub fn step(&mut self, dt: f32, now: f64) {
        let g = self.g.clone();
        // cars: who is ahead of whom on each directed street segment
        let mut lanes: HashMap<(u32, u32), Vec<(f32, usize)>> = HashMap::new();
        for (k, a) in self.agents.iter().enumerate() {
            if a.kind == Kind::Car && a.i + 1 < a.path.len() {
                lanes.entry((a.path[a.i], a.path[a.i + 1])).or_default().push((a.s, k));
            }
        }
        for l in lanes.values_mut() {
            l.sort_by(|x, y| x.0.partial_cmp(&y.0).unwrap_or(Ordering::Equal));
        }
        let mut gaps = vec![f32::INFINITY; self.agents.len()];
        for l in lanes.values() {
            for w in l.windows(2) {
                gaps[w[0].1] = w[1].0 - w[0].0;
            }
        }
        let mut done = vec![];
        let mut arrivals = vec![];
        for (k, a) in self.agents.iter_mut().enumerate() {
            if let Plan::Inside { until } | Plan::Waiting { until } | Plan::Riding { until } = a.plan {
                if now >= until && now >= a.hold_until {
                    arrivals.push(k); // time to leave the place / board / get off
                }
                continue;
            }
            if now < a.hold_until {
                a.v = 0.0; // talking with someone
                continue;
            } else if a.v == 0.0 && a.kind != Kind::Car && a.kind != Kind::Emergency {
                a.v = a.vpref;
            }
            if a.i + 1 >= a.path.len() {
                arrivals.push(k);
                continue;
            }
            let (na, nb) = (a.path[a.i], a.path[a.i + 1]);
            if a.kind == Kind::Emergency {
                // they don't queue: the traffic makes way (siren) or they are simply let through
                let limit = if a.siren { g.edge_vmax(na, nb) * 1.4 } else { g.edge_vmax(na, nb) }.min(a.vpref);
                let acc = if limit > a.v { 3.0 } else { 5.0 };
                a.v += (limit - a.v).clamp(-acc * dt, acc * dt);
            }
            if a.kind == Kind::Car {
                let limit = if a.section.is_some() && a.i < a.sec_until { a.sec_speed } else { g.edge_vmax(na, nb).min(a.vpref.max(8.0)) };
                let gap = gaps[k];
                let target = if gap < 7.0 { 0.0 } else if gap < 30.0 { limit.min((gap - 7.0) * 0.6) } else { limit };
                let acc = if target > a.v { 2.2 } else { 5.0 };
                a.v += (target - a.v).clamp(-acc * dt, acc * dt);
                if a.i >= a.sec_until {
                    a.section = None; // past the measured part: no longer counts for it
                }
            }
            let mut d = a.v * dt;
            while d > 0.0 && a.i + 1 < a.path.len() {
                let (pa, pb) = (g.xyz[a.path[a.i] as usize], g.xyz[a.path[a.i + 1] as usize]);
                let len = ((pa[0] - pb[0]).powi(2) + (pa[1] - pb[1]).powi(2)).sqrt();
                if a.s + d < len {
                    a.s += d;
                    d = 0.0;
                } else {
                    d -= len - a.s;
                    a.s = 0.0;
                    a.i += 1;
                }
            }
        }
        // arrivals: go in, come out, leave
        for k in arrivals {
            let hour_rng: f64 = self.rng.gen_range(300.0..2400.0);
            let a = &mut self.agents[k];
            match (a.kind, a.plan) {
                (Kind::Car, _) | (Kind::Emergency, _) | (Kind::Marcher, _) => done.push(k),
                (_, Plan::Going) if a.ride.is_some() && a.poi.is_some() => {
                    // at the stop: wait for the tram/bus/Navibus
                    let dep = a.ride.as_ref().map(|r| r.dep).unwrap_or(0) as f64;
                    a.plan = Plan::Waiting { until: dep.max(now) };
                }
                (_, Plan::Waiting { .. }) => {
                    let arr = a.ride.as_ref().map(|r| r.arr).unwrap_or(0) as f64;
                    a.plan = Plan::Riding { until: arr.max(now + 30.0) };
                }
                (_, Plan::Riding { .. }) => {
                    // off at their stop: walk the rest of the way
                    let rd = a.ride.take().unwrap();
                    let dest = a.poi.and_then(|k| self.pois.get(k)).and_then(|p| g.nearest(p.p[0], p.p[1], WALK, 60.0));
                    let from = g.nearest(rd.off[0], rd.off[1], WALK, 150.0);
                    match (from, dest) {
                        (Some(f), Some(t)) => match g.route(f, t, WALK, 8000) {
                            Some(p) if p.len() >= 2 => {
                                a.path = p;
                                a.i = 0;
                                a.s = 0.0;
                                a.plan = Plan::Going;
                            }
                            _ => done.push(k),
                        },
                        _ => done.push(k),
                    }
                }
                (_, Plan::Going) if a.poi.is_some() => a.plan = Plan::Inside { until: now + hour_rng },
                (_, Plan::Inside { .. }) => {
                    // walk to the nearest stop (walkers) or ride away and vanish
                    let here = *a.path.last().unwrap();
                    let hp = g.xyz[here as usize];
                    let mode = if a.kind == Kind::Bike { BIKE } else { WALK };
                    let dest = if a.kind == Kind::Walker {
                        self.stops.iter().copied().min_by(|&x, &y| {
                            let (px, py) = (g.xyz[x as usize], g.xyz[y as usize]);
                            let dx = (px[0] - hp[0]).powi(2) + (px[1] - hp[1]).powi(2);
                            let dy = (py[0] - hp[0]).powi(2) + (py[1] - hp[1]).powi(2);
                            dx.partial_cmp(&dy).unwrap_or(Ordering::Equal)
                        })
                    } else {
                        g.nearest(hp[0] + 600.0, hp[1] - 300.0, BIKE, 400.0)
                    };
                    match dest.and_then(|t| g.route(here, t, mode, 6000)) {
                        Some(p) if p.len() >= 2 => {
                            a.path = p;
                            a.i = 0;
                            a.s = 0.0;
                            a.plan = Plan::Leaving;
                        }
                        _ => done.push(k),
                    }
                }
                _ => done.push(k),
            }
        }
        done.sort_unstable();
        done.dedup();
        for k in done.into_iter().rev() {
            self.agents.swap_remove(k);
        }
    }

    pub fn populate(&mut self) {
        let t = chrono::Utc::now().with_timezone(&chrono_tz::Europe::Paris);
        let wd = t.weekday().num_days_from_monday();
        self.spawn_cars();
        self.spawn_emergency();
        self.hour = t.hour();
        self.spawn_people(t.hour(), wd >= 5, wd == 6);
        self.spawn_marchers(t.timestamp() as f64);
    }

    pub fn snapshot(&self, now: f64) -> Snapshot {
        let g = &self.g;
        let items = self
            .agents
            .iter()
            .filter(|a| !matches!(a.plan, Plan::Inside { .. } | Plan::Riding { .. }) && a.path.len() >= 2
                && (a.i + 1 < a.path.len() || matches!(a.plan, Plan::Waiting { .. })))
            .map(|a| {
                let i = a.i.min(a.path.len() - 2);
                let (pa, pb) = (g.xyz[a.path[i] as usize], g.xyz[a.path[i + 1] as usize]);
                let len = ((pa[0] - pb[0]).powi(2) + (pa[1] - pb[1]).powi(2)).sqrt().max(0.01);
                let f = if a.i + 1 >= a.path.len() { 1.0 } else { (a.s / len).min(1.0) };
                let (dx, dy) = ((pb[0] - pa[0]) / len, (pb[1] - pa[1]) / len);
                // cars keep right of the centre line, people walk on the pavement
                let side = match a.kind {
                    Kind::Car | Kind::Emergency => 1.7,
                    Kind::Walker => 4.5 + (a.seed % 3) as f32 * 0.6,
                    Kind::Marcher => (a.seed % 13) as f32 - 6.0, // the whole street
                    Kind::Bike => 2.6,
                };
                let x = pa[0] + (pb[0] - pa[0]) * f + dy * side + a.jit[0];
                let y = pa[1] + (pb[1] - pa[1]) * f - dx * side + a.jit[1];
                let z = pa[2] + (pb[2] - pa[2]) * f;
                let variant = match a.kind {
                    Kind::Emergency => a.service + 4 * a.siren as u8,
                    Kind::Marcher => a.service + 4 * ((a.seed >> 8) as u8 % 60),
                    _ => (a.seed >> 8) as u8,
                };
                let heading = if now < a.hold_until { a.face } else { dy.atan2(dx) };
                (a.id, a.kind as u8, x, y, z, heading, if now < a.hold_until { 0.0 } else { a.v }, variant)
            })
            .collect();
        Snapshot { at: now, items }
    }

    /// For a conversation: the persona, where they are, what they are doing, and the places around.
    pub fn talk_context(&self, id: u32) -> Option<(Persona, [f32; 3], String, Vec<String>)> {
        let a = self.agents.iter().find(|a| a.id == id)?;
        let p = a.persona.as_deref()?.clone();
        let here = self.position(a);
        let doing = self.describe(id).and_then(|v| v["doing"].as_str().map(String::from)).unwrap_or_default();
        let mut near: Vec<(f32, String)> = self.pois.iter().filter_map(|q| {
            let d = ((q.p[0] - here[0]).powi(2) + (q.p[1] - here[1]).powi(2)).sqrt();
            let name = q.t.get("name")?;
            (d < 500.0).then(|| {
                let what = q.t.get("cuisine").map(|c| format!("{}, {c}", q.k)).unwrap_or_else(|| q.k.clone());
                (d, format!("{name} ({what}, à {} m)", (d / 10.0).round() * 10.0))
            })
        }).collect();
        near.sort_by(|x, y| x.0.partial_cmp(&y.0).unwrap_or(Ordering::Equal));
        let near = near.into_iter().take(25).map(|x| x.1).collect();
        Some((p, here, doing, near))
    }

    /// The quartier and sub-quartier names at a point.
    pub fn place_names(&self, x: f32, y: f32) -> (Option<String>, Option<String>) {
        match self.quartiers.at(x, y) {
            Some((q, s)) => {
                let qq = &self.quartiers.quartiers[q];
                (Some(qq.name.clone()), s.map(|s| qq.subs[s].name.clone()))
            }
            None => (None, None),
        }
    }

    /// Stop `id` for a talk until `until`, turned towards (x, y). False if they can't (gone, on board).
    pub fn hold(&mut self, id: u32, until: f64, x: f32, y: f32) -> bool {
        let Some(k) = self.agents.iter().position(|a| a.id == id) else { return false };
        if matches!(self.agents[k].plan, Plan::Riding { .. } | Plan::Inside { .. }) {
            return false;
        }
        let here = self.position(&self.agents[k]);
        let a = &mut self.agents[k];
        a.hold_until = until;
        a.face = (y - here[1]).atan2(x - here[0]);
        true
    }

    fn position(&self, a: &Agent) -> [f32; 3] {
        let g = &self.g;
        if a.path.len() < 2 {
            return a.path.first().map(|&n| g.xyz[n as usize]).unwrap_or([0.0; 3]);
        }
        let i = a.i.min(a.path.len() - 2);
        let (pa, pb) = (g.xyz[a.path[i] as usize], g.xyz[a.path[i + 1] as usize]);
        let len = ((pa[0] - pb[0]).powi(2) + (pa[1] - pb[1]).powi(2)).sqrt().max(0.01);
        let f = if a.i + 1 >= a.path.len() { 1.0 } else { (a.s / len).min(1.0) };
        [pa[0] + (pb[0] - pa[0]) * f, pa[1] + (pb[1] - pa[1]) * f, pa[2] + (pb[2] - pa[2]) * f]
    }

    pub fn describe(&self, id: u32) -> Option<serde_json::Value> {
        let a = self.agents.iter().find(|a| a.id == id)?;
        let poi = a.poi.and_then(|k| self.pois.get(k));
        let place = poi.and_then(|p| p.t.get("name")).cloned();
        let base = a.base.and_then(|k| self.bases.get(k));
        let what = match (a.kind, a.plan) {
            (Kind::Car, _) => "roule".to_string(),
            (Kind::Emergency, _) => {
                let at = base.map(|b| b.name.as_str()).filter(|n| !n.is_empty());
                match (a.service, a.siren, at) {
                    (0, true, Some(n)) if a.path.last() == base.map(|b| &b.node) => format!("urgence vers {n}"),
                    (_, true, _) => "en intervention".to_string(),
                    (_, false, Some(n)) => format!("rentre à {n}"),
                    _ => "rentre".to_string(),
                }
            }
            (_, Plan::Going) => place.as_ref().map(|n| format!("se rend à {n}")).unwrap_or_else(|| "se promène".into()),
            (_, Plan::Inside { .. }) => place.as_ref().map(|n| format!("est à {n}")).unwrap_or_default(),
            (_, Plan::Leaving) => "rentre".to_string(),
            (_, Plan::Waiting { .. }) => a.ride.as_ref().map(|r| format!("attend le {} {} à {}", r.kind, r.line, r.on_name)).unwrap_or_default(),
            (_, Plan::Riding { .. }) => a.ride.as_ref().map(|r| format!("dans le {} {}, descend à {}", r.kind, r.line, r.off_name)).unwrap_or_default(),
        };
        let what = match (&a.persona, a.kind) {
            (Some(p), Kind::Marcher) => format!("manifeste ({})", p.march.as_deref().unwrap_or("")),
            _ => what,
        };
        let kind = match a.kind {
            Kind::Emergency => ["ambulance", "police", "pompiers"][a.service as usize % 3],
            Kind::Marcher => "walker",
            k => ["car", "walker", "bike"][k as usize],
        };
        let p = a.persona.as_deref();
        Some(serde_json::json!({
            "id": a.id, "kind": kind, "name": p.map(|p| p.first.clone()).unwrap_or_else(|| name_of(a.seed).to_string()),
            "doing": people::contract(&what), "place": poi.map(|p| &p.id), "speed": a.v,
            "persona": p.map(|p| serde_json::json!({
                "name": p.full_name(), "age": p.age, "job": p.job, "quartier": p.quartier, "sub": p.sub,
                "purpose": p.purpose, "destination": p.destination, "trip": p.trip_text(),
                "story": p.story, "chatty": p.chatty, "mood": p.mood, "archetype": p.archetype.as_ref().map(|a| &a.0),
                "march": p.march,
            })),
            "march": if a.kind == Kind::Marcher { self.marching() } else { None },
        }))
    }
}

// ---------------------------------------------------------------- running it

pub struct World {
    pub sim: RwLock<Sim>,
    pub snap: watch::Receiver<Arc<Snapshot>>,
    tx: watch::Sender<Arc<Snapshot>>,
}

impl World {
    pub fn new(sim: Sim) -> Arc<World> {
        let (tx, snap) = watch::channel(Arc::new(Snapshot { at: 0.0, items: vec![] }));
        Arc::new(World { sim: RwLock::new(sim), snap, tx })
    }
}

/// 10 steps a second, new inhabitants every second, a published snapshot 4 times a second.
pub async fn run(w: Arc<World>) {
    let mut iv = tokio::time::interval(Duration::from_millis(100));
    iv.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut n: u64 = 0;
    loop {
        iv.tick().await;
        let now = chrono::Utc::now().timestamp_millis() as f64 / 1000.0;
        let mut s = w.sim.write().await;
        if n % 10 == 0 {
            s.populate();
        }
        s.step(0.1, now);
        if n % 3 == 0 {
            let snap = Arc::new(s.snapshot(now));
            drop(s);
            let _ = w.tx.send(snap);
        }
        n += 1;
    }
}

/// Load the network, places and stops from the published world data.
pub fn build(web: &Path) -> Result<Sim> {
    let g = Arc::new(Graph::load(&web.join("data/world/roads.json"))?);
    let pois: Vec<PoiIn> = serde_json::from_slice(&std::fs::read(web.join("data/world/pois.json"))?)?;
    #[derive(Deserialize)]
    struct Station {
        p: [f32; 3],
    }
    #[derive(Deserialize)]
    struct Transit {
        stations: Vec<Station>,
    }
    let t: Transit = serde_json::from_slice(&std::fs::read(web.join("data/world/transit.json"))?)?;
    let st: Vec<[f32; 3]> = t.stations.iter().map(|s| s.p).collect();
    // optional: older world data has no bases
    let bases: Vec<BaseIn> = std::fs::read(web.join("data/world/bases.json"))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    let mut s = Sim::new(g, pois, &st, &bases);
    s.set_quartiers(Quartiers::load(web));
    if let Some(p) = std::fs::read(web.join("data/world/rails.json")).ok()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
        .and_then(|v| Some([v["station"][0].as_f64()? as f32, v["station"][1].as_f64()? as f32])) {
        s.set_station(p);
    }
    Ok(s)
}
