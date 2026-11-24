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
        Sim {
            g,
            pois: pois.into_iter().filter(|p| p.t.contains_key("name")).collect(),
            stops,
            agents: vec![],
            next_id: 1,
            rng: SmallRng::from_entropy(),
            target_per_section: vec![],
            sections: vec![],
            rain: 0.0,
            bases,
        }
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

    fn choose_poi(&mut self, x: f32, y: f32, hour: u32, sunday: bool) -> Option<usize> {
        for _ in 0..30 {
            let k = self.rng.gen_range(0..self.pois.len().max(1));
            let p = self.pois.get(k)?;
            let d = ((p.p[0] - x).powi(2) + (p.p[1] - y).powi(2)).sqrt();
            if d < 1500.0 && d > 80.0 && open(&p.k, hour, sunday) {
                return Some(k);
            }
        }
        None
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
        for _ in 0..10 {
            if walkers >= walkers_target || self.pois.is_empty() {
                break;
            }
            // from a stop (just got off) or from somewhere near a place
            let from = if !self.stops.is_empty() && self.rng.gen_bool(0.5) {
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
            let ag = self.spawn(Kind::Walker, path, v);
            ag.poi = Some(k);
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
            let ag = self.spawn(Kind::Bike, path, v);
            ag.poi = Some(k);
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
            if let Plan::Inside { until } = a.plan {
                if now >= until {
                    arrivals.push(k); // time to leave the place
                }
                continue;
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
                (Kind::Car, _) | (Kind::Emergency, _) => done.push(k),
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
        self.spawn_people(t.hour(), wd >= 5, wd == 6);
    }

    pub fn snapshot(&self, now: f64) -> Snapshot {
        let g = &self.g;
        let items = self
            .agents
            .iter()
            .filter(|a| !matches!(a.plan, Plan::Inside { .. }) && a.i + 1 < a.path.len())
            .map(|a| {
                let (pa, pb) = (g.xyz[a.path[a.i] as usize], g.xyz[a.path[a.i + 1] as usize]);
                let len = ((pa[0] - pb[0]).powi(2) + (pa[1] - pb[1]).powi(2)).sqrt().max(0.01);
                let f = (a.s / len).min(1.0);
                let (dx, dy) = ((pb[0] - pa[0]) / len, (pb[1] - pa[1]) / len);
                // cars keep right of the centre line, people walk on the pavement
                let side = match a.kind {
                    Kind::Car | Kind::Emergency => 1.7,
                    Kind::Walker => 4.5 + (a.seed % 3) as f32 * 0.6,
                    Kind::Bike => 2.6,
                };
                let x = pa[0] + (pb[0] - pa[0]) * f + dy * side;
                let y = pa[1] + (pb[1] - pa[1]) * f - dx * side;
                let z = pa[2] + (pb[2] - pa[2]) * f;
                let variant = if a.kind == Kind::Emergency { a.service + 4 * a.siren as u8 } else { (a.seed >> 8) as u8 };
                (a.id, a.kind as u8, x, y, z, dy.atan2(dx), a.v, variant)
            })
            .collect();
        Snapshot { at: now, items }
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
        };
        let kind = match a.kind {
            Kind::Emergency => ["ambulance", "police", "pompiers"][a.service as usize % 3],
            k => ["car", "walker", "bike"][k as usize],
        };
        Some(serde_json::json!({
            "id": a.id, "kind": kind, "name": name_of(a.seed),
            "doing": what, "place": poi.map(|p| &p.id), "speed": a.v,
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
    Ok(Sim::new(g, pois, &st, &bases))
}
